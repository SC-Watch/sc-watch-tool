"""Read a frame IRIS's way, as sc-watch contacts - with OCR for what IRIS refuses.

iris_detector.py finds the distance lines on a frame and reads the plate above
each one with IRIS's per-letter network (iris_reader.py). Both are ports of
IRIS (https://github.com/Niteb1rd/IRIS, GPL-3.0) proven identical to it on
135 frames. This module turns their answers into sc_detector.Contact objects,
so everything after detection - voting, lookups, audio, audits - is the same
whichever reader found them.

Where IRIS finds a distance line but will not commit to a name (its Read()
says Unclear or Stop: low confidence, joined letters, a hyphen at the edge,
clutter...), the plate is not dropped: the OCR reader sc-watch has always used
reads the same crop IRIS would have read, and its answer is used instead.
Capped per frame, nearest plates first, because an OCR read costs ~100-200 ms
against IRIS's ~5.

Names go through sc_detector.classify_name like any other read, so UNKNOWN in
all its misspellings, ship serials and NPC crew are sorted the same way.
"""

from __future__ import annotations

from dataclasses import dataclass

import cv2

import iris_detector as idet
import iris_reader as ir
import sc_detector as sd

# Scan-area presets: (top, bottom, left, right) margins in percent, IRIS's
# TrackingBoundary order. IRIS clamps each margin to 0-30 %.
SCAN_AREAS = {
    "whole": (0, 0, 0, 0),
    "narrow": (5, 5, 3, 3),
    "iris": (15, 15, 8, 8),          # IRIS's own default
    "centre": (25, 25, 20, 20),
}

FALLBACK_MAX = 4                     # OCR fallbacks per frame


@dataclass
class FrameStats:
    lines: int = 0                   # distance lines IRIS found
    named: int = 0                   # plates IRIS read itself
    fallback_tried: int = 0
    fallback_named: int = 0
    refused: dict | None = None      # Read() reasons for the rest


class IrisReader:
    def __init__(self, scan_area: str = "whole", fallback: bool = True,
                 fallback_max: int = FALLBACK_MAX):
        if scan_area not in SCAN_AREAS:
            raise ValueError(f"scan area must be one of {', '.join(SCAN_AREAS)}")
        self.scan_area = scan_area
        self.fallback = fallback
        self.fallback_max = fallback_max
        self.rows = ir.RowReader()
        self._ops: dict = {}
        self.last = FrameStats()

    def _ops_for(self, w: int, h: int) -> idet.ImageOps:
        ops = self._ops.get((w, h))
        if ops is None:
            geo = idet.ScanGeometry.create(w, h, idet.Settings(),
                                           idet.boundary_from_percents(*SCAN_AREAS[self.scan_area]))
            ops = self._ops[(w, h)] = idet.ImageOps(self.rows, geo)
        return ops

    def scan(self, frame) -> list:
        """Only the distance lines IRIS finds - no reading. What always-on
        mode runs on every frame to notice that a contact has appeared."""
        H, W = frame.shape[:2]
        return self._ops_for(W, H).scan(frame)

    def detect(self, frame, ocr=None) -> list[sd.Contact]:
        H, W = frame.shape[:2]
        ops = self._ops_for(W, H)
        stats = FrameStats(refused={})
        found: list[sd.Contact] = []
        refused = []
        for line in ops.scan(frame):
            stats.lines += 1
            dist = self.rows.read_distance(frame, line)
            km = dist.km if dist is not None else None
            r = ops.read_line(frame, line)
            if r.cls == "Name" and r.name:
                kind, cleaned = sd.classify_name(r.name)
                rows = r.rows or []
                x0 = min(b[0] for b in rows)
                y0 = min(b[1] for b in rows)
                x1 = max(b[0] + b[2] for b in rows)
                y1 = max(b[1] + b[3] for b in rows)
                c = sd.Contact(name_box=sd.Box(x0, y0, x1 - x0, y1 - y0, 0),
                               range_box=sd.Box(*line, 0))
                c.range_km, c.kind, c.name = km, kind, cleaned
                found.append(c)
                stats.named += 1
            else:
                stats.refused[r.reason] = stats.refused.get(r.reason, 0) + 1
                # Only IRIS's UNSURE refusals get a second opinion. 'Stop'
                # means it is sure the plate is not one name - two words
                # ('notoneword'), bad characters, too long, off centre - and
                # OCR overruling that was measured to do harm: a red station
                # label IRIS refused as two words came back from the fallback
                # as 'PORTTRESSLER', a player (RapidOCR on the grey crop
                # dropped the space). 'multiword' is the same verdict for the
                # rows of a stacked plate.
                if r.cls == "Unclear" and r.reason != "multiword":
                    refused.append((km if km is not None else 1e9, line))
        if self.fallback and ocr is not None and hasattr(ocr, "lines"):
            for _, line in sorted(refused)[:self.fallback_max]:
                stats.fallback_tried += 1
                c = self._ocr_plate(frame, line, ocr)
                if c is not None:
                    found.append(c)
                    stats.fallback_named += 1
        self.last = stats
        found.sort(key=lambda c: c.range_km if c.range_km is not None else 1e9)
        return found

    def _ocr_plate(self, frame, line, ocr) -> sd.Contact | None:
        """sc-watch's own OCR on the crop IRIS reads a plate from: the
        recogniser finds the text lines, sc_detector pairs name with range,
        and the pair whose range sits on IRIS's distance line is kept."""
        crop = idet.read_crop_rect(line, frame.shape[1], frame.shape[0])
        if crop is None:
            return None
        x, y, w, h = crop
        sub = frame[y:y + h, x:x + w]
        grey = cv2.cvtColor(cv2.cvtColor(sub, cv2.COLOR_BGR2GRAY), cv2.COLOR_GRAY2BGR)
        try:
            pairs = sd._pair_ocr_lines(ocr.lines(grey), frame.shape[0], sub, ocr)
        except Exception:
            return None
        lx, ly, lw, lh = line
        best, best_d = None, None
        for c in pairs:
            rb = c.range_box
            if rb is None or c.range_km is None:
                continue
            rx, ry = rb.x + x, rb.y + y
            # the range OCR found must be IRIS's distance line, give or take
            overlap = min(rx + rb.w, lx + lw) - max(rx, lx)
            if overlap <= 0 or abs(ry - ly) > max(4, lh):
                continue
            d = abs(rx + rb.w / 2 - (lx + lw / 2))
            if best is None or d < best_d:
                best, best_d = c, d
        if best is None:
            return None
        for b in (best.name_box, best.range_box):
            b.x += x
            b.y += y
        return best
