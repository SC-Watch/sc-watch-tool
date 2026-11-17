"""IRIS's nameplate FINDING layer, ported to Python.

Ported from IRIS - https://github.com/Niteb1rd/IRIS - at commit aec86f08
(IRIS 2.0.0), with Read()'s bright-background rule from IRIS pull request #1
(commit 29c3250e95), licensed GPL-3.0 like this project. Modified from the
original: translated from C# to Python/numpy and cut down to finding and
reading nameplates on a still frame - NameplateDetector (colour/contrast
mask, text lines, speed unions, narrow rows, pairing, stacks, the isolation
and crowd rules), DetectorImageOps.Scan and .Read, ScanGeometry, the read
crop of RegionPlanner (with no motion), PlateColors.Tint. Left out: live
tracking, the overlay, mining, and IRIS's RSI lookups.

Reading a row is iris_reader.py. Parity with IRIS is checked by
tools/iris_parity_detect.py against IRIS's own output on the same frames.
Names and structure follow the C# so each function can be compared with its
original.

    ops = ImageOps(RowReader(), ScanGeometry.create(w, h, Settings(), boundary))
    lines = ops.scan(frame)                # distance lines, window coordinates
    result = ops.read_line(frame, line)    # ReadResult: Name / Stop / Unclear
"""

from __future__ import annotations

import math
from dataclasses import dataclass, field, replace
from typing import Callable

import cv2
import numpy as np

import iris_reader as ir

Rect = tuple  # (x, y, w, h)


# ---- settings (NameplateDetectorSettings / PipelineSettings) ----------------

@dataclass(frozen=True)
class Settings:
    region_left: float = 0.15
    region_right: float = 0.85
    region_top: float = 0.10
    region_bottom: float = 0.70
    reference_height: int = 1440
    scale_height: int = 0
    min_name_length: int = 4
    max_name_length: int = 64
    min_name_score: float = -0.20
    clear_lines: float = 3.0
    crowd_exception: bool = True
    speck_area: float = 4.0
    center_tolerance: float = 5.0
    left_tolerance: float = 2.0
    left_width_difference: float = 8.0
    max_rows: int = 3
    row_gap_max: float = 0.8
    row_pitch_min: float = 1.05
    row_pitch_max: float = 1.8
    row_min_caps: float = 11.0
    stack_tint_tolerance: float = 0.10
    narrow_min_width: float = 6.0
    narrow_max_lines: float = 2.2
    speed_join_gap: float = 1.5
    speed_join_max_lines: float = 8.0
    read_crop_min_height: float = 30.0
    read_crop_min_width: float = 60.0
    near_height_tolerance: float = 0.35
    near_height_min_px: float = 2.0
    anchor_max_offset_px: float = 8.0
    anchor_max_row_offset_lines: float = 0.8
    top_gap_max_lines: float = 1.4
    top_height_diff_min_px: float = 4.0
    top_height_diff_lines: float = 0.45
    top_centre_tolerance_px: float = 5.0
    top_left_tolerance_px: float = 2.0
    top_left_width_difference_px: float = 8.0
    top_hat_kernel_px: float = 11.0
    top_hat_min: float = 28.0
    green_top_hat_min: float = 20.0
    white_value_min: float = 150.0
    red_min: float = 140.0
    red_over_green_min: float = 80.0
    red_over_blue_min: float = 60.0
    green_min: float = 90.0
    green_over_red_min: float = 35.0
    green_over_blue_min: float = 30.0
    bg_dark_max: float = 100.0
    bg_full_contrast: float = 180.0
    contrast_hat_max: float = 45.0
    background_shrink_px: float = 4.0
    background_blur: int = 15
    line_max_height: float = 22.0
    line_min_height: float = 10.0
    line_min_width: float = 24.0
    line_aspect_min: float = 1.5
    join_width_px: float = 7.0
    fill_min: float = 0.12
    fill_max: float = 0.75
    height_tolerance_px: float = 4.0
    height_tolerance_lines: float = 0.45
    row_tolerance_px: float = 3.0
    row_tolerance_lines: float = 0.25
    row_join_top_px: float = 2.0
    row_join_height_px: float = 3.0
    row_join_gap_px: float = 4.0
    max_line_width: int = 832
    pair_gap_max_lines: float = 1.4

    def scale(self, frame_height: int, value: float) -> int:
        return max(1, int(math.floor(value * frame_height / self.reference_height + 0.5)))

    def effective_scale_height(self, image_height: int) -> int:
        return self.scale_height if self.scale_height > 0 else image_height

    def context_padding(self, sh: int) -> int:
        return 3 * self.scale(sh, self.line_max_height) + self.scale(sh, self.height_tolerance_px) + 2

    def vertical_context_padding(self, sh: int) -> int:
        return self.scale(sh, self.top_hat_kernel_px) + 2


# IRIS PR #1: a read crop is 'bright' when at least this share of its pixels
# has a channel at or above this value (snow, sunlit hulls). An off-centre
# line over it is unsure, not a refusal - the bright ground may be what
# broke the plate apart.
BRIGHT_BACKGROUND_VALUE = 170
BRIGHT_BACKGROUND_SHARE = 0.5


def has_bright_background(image: np.ndarray) -> bool:
    px = image[:, :, :3] if image.ndim == 3 else image
    bright = (px >= BRIGHT_BACKGROUND_VALUE).any(axis=2) if px.ndim == 3 else px >= BRIGHT_BACKGROUND_VALUE
    return int(np.count_nonzero(bright)) >= BRIGHT_BACKGROUND_SHARE * bright.size


# PipelineSettings: how the app cuts the crop it reads a plate from.
READ_MARGIN_PX = 8
PATCH_MIN_WIDTH_PX = 40.0
CROP_HALF_WIDTH_PX = 110.0
CROP_HALF_WIDTH_LINES = 3.0
CROP_SLACK_PX = 12.0
CROP_ABOVE_LINES = 7.0
CROP_BELOW_LINES = 2.0


# ---- small rectangle helpers ------------------------------------------------

def _padded(b: Rect, size: int, cols: int, rows: int) -> Rect:
    pad = 2 * size + 2
    left, top = max(0, b[0] - pad), max(0, b[1] - pad)
    right, bottom = min(cols, b[0] + b[2] + pad), min(rows, b[1] + b[3] + pad)
    return left, top, right - left, bottom - top


def _union(a: Rect, b: Rect) -> Rect:
    left, top = min(a[0], b[0]), min(a[1], b[1])
    return (left, top, max(a[0] + a[2], b[0] + b[2]) - left,
            max(a[1] + a[3], b[1] + b[3]) - top)


def _contains(outer: Rect, inner: Rect) -> bool:
    return (outer[2] > 0 and outer[3] > 0 and inner[0] >= outer[0] and inner[1] >= outer[1]
            and inner[0] + inner[2] <= outer[0] + outer[2]
            and inner[1] + inner[3] <= outer[1] + outer[3])


def _sub(img: np.ndarray, r: Rect) -> np.ndarray:
    return img[r[1]:r[1] + r[3], r[0]:r[0] + r[2]]


def _bounding(mask: np.ndarray) -> Rect:
    return tuple(int(v) for v in cv2.boundingRect(mask))


def _tophat_roi(parent: np.ndarray, r: Rect, kernel: np.ndarray) -> np.ndarray:
    """Top-hat of parent[r] as OpenCV computes it on an ROI: the erosion may
    look at pixels outside r (the parent's), the dilation that follows works
    on a fresh image and cannot."""
    k = kernel.shape[0] // 2, kernel.shape[1] // 2          # (ry, rx)
    x, y, w, h = r
    X0, Y0 = max(0, x - k[1]), max(0, y - k[0])
    X1, Y1 = min(parent.shape[1], x + w + k[1]), min(parent.shape[0], y + h + k[0])
    eroded = cv2.erode(parent[Y0:Y1, X0:X1], kernel)
    eroded = np.ascontiguousarray(eroded[y - Y0:y - Y0 + h, x - X0:x - X0 + w])
    opened = cv2.dilate(eroded, kernel)
    return cv2.subtract(np.ascontiguousarray(_sub(parent, r)), opened)


@dataclass
class Candidates:
    boxes: list
    pairs: list
    loose: list
    crowd: list                         # (top, bottom, neighbours)
    stacks: dict = field(default_factory=dict)

    def stack_of(self, top: Rect, bottom: Rect):
        return self.stacks.get((top, bottom))


class _LinesByTop:
    def __init__(self, first: list, second: list | None, pair_gap: float):
        all_ = list(first) + list(second or [])
        self.order = sorted(range(len(all_)), key=lambda i: all_[i][1])
        self.tops = [all_[i][1] for i in self.order]
        self.pair_gap = pair_gap

    def below(self, shape: Rect) -> list:
        bottom = shape[1] + shape[3]
        farthest = bottom + int(math.floor(shape[3] * self.pair_gap))
        lo, hi = 0, len(self.tops)
        while lo < hi:
            mid = (lo + hi) // 2
            if self.tops[mid] < bottom + 1:
                lo = mid + 1
            else:
                hi = mid
        out = []
        i = lo
        while i < len(self.tops) and self.tops[i] <= farthest:
            out.append(self.order[i])
            i += 1
        out.sort()
        return out


# ---- the detector -----------------------------------------------------------

class NameplateDetector:
    def __init__(self, rows: ir.RowReader, settings: Settings | None = None):
        self.rows = rows
        self.s = settings or Settings()
        s = self.s
        lut = np.empty(256, np.uint8)
        for bg in range(256):
            t = s.top_hat_min + (bg - s.bg_dark_max) / (s.bg_full_contrast - s.bg_dark_max) \
                * (s.contrast_hat_max - s.top_hat_min)
            t = min(max(t, s.top_hat_min), s.contrast_hat_max)
            lut[bg] = min(max(math.floor(t), 0), 255)
        self.hat_limits = lut
        self._kernels: dict = {}

    def _kernel(self, w: int, h: int) -> np.ndarray:
        k = self._kernels.get((w, h))
        if k is None:
            k = self._kernels[(w, h)] = cv2.getStructuringElement(cv2.MORPH_RECT, (w, h))
        return k

    def _scaled(self, fh: int, v: float) -> int:
        return self.s.scale(fh, v)

    def _scaled_odd(self, fh: int, v: float) -> int:
        k = self._scaled(fh, v)
        return k if k % 2 == 1 else k + 1

    # -- the mask (NameplateDetector.Mask) ----------------------------------

    def _build_color_mask(self, region: np.ndarray, sh: int) -> None:
        s = self.s
        tophat = self._scaled_odd(sh, s.top_hat_kernel_px)
        kernel = self._kernel(tophat, tophat)
        blue = np.ascontiguousarray(region[:, :, 0])
        green = np.ascontiguousarray(region[:, :, 1])
        red = np.ascontiguousarray(region[:, :, 2])
        value = np.maximum(np.maximum(blue, green), red)
        rows, cols = value.shape
        self.value = value
        self.value_hat = None
        self.value_hat_area = (0, 0, 0, 0)
        bright = self._bright_area(tophat, sh)

        _, cyan_white = cv2.threshold(value, s.white_value_min, 255, cv2.THRESH_BINARY)
        b = _bounding(cyan_white)
        if b[2] > 0 and b[3] > 0:
            area = _padded(b, tophat, cols, rows)
            hat_area = area
            if bright is not None:
                u = _union(area, bright)
                if u[2] * u[3] <= area[2] * area[3] + bright[2] * bright[3]:
                    hat_area = u
            self.value_hat = _tophat_roi(value, hat_area, kernel)
            self.value_hat_area = hat_area
            hat = _sub(self.value_hat, (area[0] - hat_area[0], area[1] - hat_area[1], area[2], area[3]))
            _, bits = cv2.threshold(hat, s.top_hat_min, 255, cv2.THRESH_BINARY)
            t = _sub(cyan_white, area)
            t &= bits

        _, red_mask = cv2.threshold(red, s.red_min, 255, cv2.THRESH_BINARY)
        _, x = cv2.threshold(cv2.subtract(red, green), s.red_over_green_min, 255, cv2.THRESH_BINARY)
        red_mask &= x
        _, x = cv2.threshold(cv2.subtract(red, blue), s.red_over_blue_min, 255, cv2.THRESH_BINARY)
        red_mask &= x
        self._apply_tophat(red, red_mask, kernel, tophat, s.top_hat_min)

        _, green_mask = cv2.threshold(green, s.green_min, 255, cv2.THRESH_BINARY)
        _, x = cv2.threshold(cv2.subtract(green, red), s.green_over_red_min, 255, cv2.THRESH_BINARY)
        green_mask &= x
        _, x = cv2.threshold(cv2.subtract(green, blue), s.green_over_blue_min, 255, cv2.THRESH_BINARY)
        green_mask &= x
        self._apply_tophat(green, green_mask, kernel, tophat, s.green_top_hat_min)

        self.mask = cyan_white | red_mask | green_mask
        if bright is not None:
            self._apply_contrast_rule(bright, kernel)

    def _bright_area(self, tophat: int, sh: int):
        s = self.s
        v = self.value
        shrink = self._scaled(sh, s.background_shrink_px)
        sw = max(1, (v.shape[1] + shrink - 1) // shrink)
        shh = max(1, (v.shape[0] + shrink - 1) // shrink)
        small = cv2.resize(v, (sw, shh), interpolation=cv2.INTER_AREA)
        blur = cv2.blur(small, (s.background_blur, s.background_blur))
        if float(blur.max()) <= s.bg_dark_max:
            self.background = self.bright = None
            return None
        self.background = cv2.resize(blur, (v.shape[1], v.shape[0]), interpolation=cv2.INTER_LINEAR)
        _, self.bright = cv2.threshold(self.background, s.bg_dark_max, 255, cv2.THRESH_BINARY)
        b = _bounding(self.bright)
        if b[2] <= 0 or b[3] <= 0:
            return None
        return _padded(b, tophat, v.shape[1], v.shape[0])

    def _apply_tophat(self, channel, plain, kernel, size: int, minimum: float) -> None:
        b = _bounding(plain)
        if b[2] <= 0 or b[3] <= 0:
            return
        area = _padded(b, size, plain.shape[1], plain.shape[0])
        hat = _tophat_roi(channel, area, kernel)
        _, bits = cv2.threshold(hat, minimum, 255, cv2.THRESH_BINARY)
        t = _sub(plain, area)
        t &= bits

    def _apply_contrast_rule(self, area: Rect, kernel) -> None:
        if _contains(self.value_hat_area, area):
            h = self.value_hat_area
            hat = _sub(self.value_hat, (area[0] - h[0], area[1] - h[1], area[2], area[3]))
        else:
            hat = _tophat_roi(self.value, area, kernel)
        limit = cv2.LUT(np.ascontiguousarray(_sub(self.background, area)), self.hat_limits)
        bright = _sub(self.bright, area)
        ink = np.where((hat > limit) & (bright != 0), 255, 0).astype(np.uint8)
        m = _sub(self.mask, area)
        m[bright != 0] = 0
        m |= ink

    # -- text lines (NameplateDetector.Lines) --------------------------------

    def _find_text_lines(self, fh: int, narrow: list | None) -> list:
        s = self.s
        max_h = self._scaled(fh, s.line_max_height)
        min_h = self._scaled(fh, s.line_min_height)
        min_w = self._scaled(fh, s.line_min_width)
        max_w = self._scaled(fh, s.max_line_width)
        min_narrow_w = self._scaled(fh, s.narrow_min_width)
        speck = max(1, int(math.floor(s.speck_area * (fh / s.reference_height) ** 2 + 0.5)))
        count, labels, stats, _ = cv2.connectedComponentsWithStats(self.mask, connectivity=8)
        self.mask_labels = labels
        if count <= 1:
            self.solid = np.zeros(max(count, 1), bool)
            self.fill_mask = self.mask
            return []
        solid = stats[:, cv2.CC_STAT_AREA] > speck
        tall = stats[:, cv2.CC_STAT_HEIGHT] > max_h
        solid[0] = tall[0] = False
        self.solid = solid
        if tall.any():
            kept = self.mask.copy()
            kept[tall[labels]] = 0
        else:
            kept = self.mask
        self.fill_mask = kept
        joined = cv2.dilate(kept, self._kernel(self._scaled(fh, s.join_width_px), 1))
        count, _, stats, _ = cv2.connectedComponentsWithStats(joined, connectivity=8)
        boxes = []
        for i in range(1, count):
            x, y, w, h = (int(v) for v in stats[i, :4])
            if h < min_h or h > max_h:
                continue
            is_line = min_w <= w <= max_w and w > h * s.line_aspect_min
            if not is_line:
                if narrow is not None and w >= min_narrow_w and w <= s.narrow_max_lines * h:
                    narrow.append((x, y, w, h))
                continue
            fill = cv2.countNonZero(kept[y:y + h, x:x + w]) / float(w * h)
            if s.fill_min < fill < s.fill_max:
                boxes.append((x, y, w, h))
        return boxes

    def _narrow_fill_ok(self, b: Rect) -> bool:
        if b[2] <= 0 or b[3] <= 0:
            return False
        fill = cv2.countNonZero(_sub(self.fill_mask, b)) / float(b[2] * b[3])
        return self.s.fill_min < fill < self.s.fill_max

    def _solid_in(self, area: Rect) -> np.ndarray:
        return self.solid[_sub(self.mask_labels, area)]

    def _is_clear(self, b: Rect) -> bool:
        reach = int(math.ceil(self.s.clear_lines * b[3]))
        H, W = self.mask_labels.shape
        r0, r1 = max(0, b[1]), min(H, b[1] + b[3])
        if r1 <= r0:
            return True
        ls, le = max(0, b[0] - reach), b[0]
        if le > ls and self._solid_in((ls, r0, le - ls, r1 - r0)).any():
            return False
        rs, re_ = b[0] + b[2], min(W, b[0] + b[2] + reach)
        if re_ > rs and self._solid_in((rs, r0, re_ - rs, r1 - r0)).any():
            return False
        return True

    def _speed_unions(self, boxes: list, fh: int) -> list:
        s = self.s
        ht = self._scaled(fh, s.height_tolerance_px)
        rt = self._scaled(fh, s.row_tolerance_px)
        unions = []
        for li, left in enumerate(boxes):
            best, best_gap = None, 0
            for right in (boxes[r] for r in range(len(boxes)) if r != li):
                gap = right[0] - (left[0] + left[2])
                if not (1 <= gap <= s.speed_join_gap * left[3]) or right[2] > s.speed_join_max_lines * left[3]:
                    continue
                if abs(right[3] - left[3]) > max(ht, left[3] * s.height_tolerance_lines):
                    continue
                if abs(right[1] + right[3] / 2.0 - (left[1] + left[3] / 2.0)) > max(rt, s.row_tolerance_lines * left[3]):
                    continue
                if best is None or gap < best_gap:
                    best, best_gap = right, gap
            if best is not None:
                top = min(left[1], best[1])
                bottom = max(left[1] + left[3], best[1] + best[3])
                unions.append((left[0], top, best[0] + best[2] - left[0], bottom - top))
        return unions

    # -- pairs (NameplateDetector.Pairs) -------------------------------------

    def _tolerances(self, fh: int):
        s = self.s
        return (self._scaled(fh, s.height_tolerance_px), self._scaled(fh, s.center_tolerance),
                self._scaled(fh, s.left_tolerance), self._scaled(fh, s.left_width_difference))

    def _pair_ok(self, top: Rect, bottom: Rect, ht, ct, lt, lwd) -> bool:
        gap = bottom[1] - (top[1] + top[3])
        if not (1 <= gap <= top[3] * self.s.pair_gap_max_lines):
            return False
        if abs(bottom[3] - top[3]) > max(ht, top[3] * self.s.height_tolerance_lines):
            return False
        if abs((bottom[0] + bottom[2] / 2.0) - (top[0] + top[2] / 2.0)) > ct:
            return False
        if abs(top[0] - bottom[0]) <= lt and abs(top[2] - bottom[2]) >= lwd:
            return False
        return True

    def _pair_lines(self, boxes: list, fh: int, real_count: int) -> list:
        tol = self._tolerances(fh)
        by_top = _LinesByTop(boxes, None, self.s.pair_gap_max_lines)
        pairs = []
        for t in range(real_count):
            for b in by_top.below(boxes[t]):
                if b != t and self._pair_ok(boxes[t], boxes[b], *tol):
                    pairs.append((t, b))
        return pairs

    def _promote_narrow(self, narrow, plain, unions, fh, row_pool) -> list:
        tol = self._tolerances(fh)
        plain_set = set(plain)
        below = _LinesByTop(plain, unions, self.s.pair_gap_max_lines)
        promoted = []
        for shape in narrow:
            if shape in plain_set:
                continue
            bottoms = None
            for i in below.below(shape):
                bottom = plain[i] if i < len(plain) else unions[i - len(plain)]
                if not self._pair_ok(shape, bottom, *tol):
                    continue
                if len(self._stack_rows(row_pool(), shape, bottom, fh)[0]) > 1:
                    bottoms = (bottoms or []) + [bottom]
            if bottoms is not None and self._narrow_fill_ok(shape):
                promoted.append((shape, bottoms))
        return promoted

    def _find_stacks(self, boxes, pairs, row_pool, fh) -> dict:
        stacks: dict = {}
        if not pairs:
            return stacks
        pool = row_pool()
        for t, b in pairs:
            rows, blockers = self._stack_rows(pool, boxes[t], boxes[b], fh)
            if len(rows) > 1 or blockers:
                stacks[(boxes[t], boxes[b])] = (rows, blockers)
        return stacks

    def _row_candidates(self, boxes: list, real_count: int, fh: int) -> list:
        s = self.s
        s2 = self._scaled(fh, s.row_join_top_px)
        s3 = self._scaled(fh, s.row_join_height_px)
        s4 = self._scaled(fh, s.row_join_gap_px)
        parent = list(range(real_count))

        def find(i):
            while parent[i] != i:
                parent[i] = parent[parent[i]]
                i = parent[i]
            return i

        for i in range(real_count):
            a = boxes[i]
            for j in range(i + 1, real_count):
                c = boxes[j]
                if (abs(a[1] - c[1]) <= s2 and abs(a[3] - c[3]) <= s3
                        and c[0] <= a[0] + a[2] + s4 and a[0] <= c[0] + c[2] + s4):
                    parent[find(j)] = find(i)
        order, members = [], {}
        for i in range(real_count):
            root = find(i)
            if root not in members:
                members[root] = []
                order.append(root)
            members[root].append(i)
        pool = []
        for root in order:
            g = members[root]
            if len(g) == 1:
                pool.append(boxes[g[0]])
                continue
            x0 = min(boxes[i][0] for i in g)
            y0 = min(boxes[i][1] for i in g)
            x1 = max(boxes[i][0] + boxes[i][2] for i in g)
            y1 = max(boxes[i][1] + boxes[i][3] for i in g)
            pool.append((x0, y0, x1 - x0, y1 - y0))
        return pool

    def _stack_rows(self, pool: list, top: Rect, anchor: Rect, fh: int):
        s = self.s
        ht, ct, lt, lwd = self._tolerances(fh)
        anchor_center = anchor[0] + anchor[2] / 2.0
        chain = [top]
        current = top
        while True:
            joinable, broken = [], []
            for row in pool:
                if row == anchor or row in chain:
                    continue
                gap = current[1] - (row[1] + row[3])
                if not (1 <= gap <= s.row_gap_max * current[3]):
                    continue
                pitch = current[1] - row[1]
                if not (s.row_pitch_min * current[3] <= pitch <= s.row_pitch_max * current[3]):
                    continue
                if abs(row[3] - current[3]) > max(ht, current[3] * s.height_tolerance_lines):
                    continue
                if row[2] < s.row_min_caps * row[3]:
                    continue
                if row[0] >= current[0] + current[2] or row[0] + row[2] <= current[0]:
                    continue
                if abs(row[0] - current[0]) <= lt and abs(row[2] - current[2]) >= lwd:
                    continue
                if abs(row[0] + row[2] / 2.0 - anchor_center) > ct:
                    broken.append(row)
                    continue
                joinable.append((gap, row))
            if not joinable:
                chain.reverse()
                return chain, broken
            best = joinable[0]
            for cand in joinable:
                if (cand[0], cand[1][0], cand[1][1], cand[1][2], cand[1][3]) < \
                        (best[0], best[1][0], best[1][1], best[1][2], best[1][3]):
                    best = cand
            if len(chain) >= s.max_rows:
                chain.reverse()
                broken.append(best[1])
                return chain, broken
            chain.append(best[1])
            current = best[1]

    def _sort_pairs(self, boxes, clear: Callable[[int], bool], all_pairs, real_count):
        strict, loose, crowd = [], [], []
        pairs_of_line: dict = {}
        for pair in all_pairs:
            for line in pair:
                pairs_of_line.setdefault(line, []).append(pair)
        cache: dict = {}
        for t, b in all_pairs:
            if clear(t) and clear(b):
                strict.append((t, b))
                continue
            if self.s.crowd_exception:
                need, blocked = set(), False
                for line in (t, b):
                    if line not in cache:
                        cache[line] = [] if clear(line) else self._neighbours(boxes, line, real_count)
                    near = cache[line]
                    if near is None:
                        blocked = True
                        break
                    need.update(near)
                if not blocked:
                    need.discard(t)
                    need.discard(b)
                    if need and all(any(o[0] not in (t, b) and o[1] not in (t, b)
                                        for o in pairs_of_line.get(k, ())) for k in need):
                        crowd.append((t, b, sorted(need)))
                        continue
            loose.append((t, b))
        return strict, loose, crowd

    def _neighbours(self, boxes, index: int, real_count: int):
        box = boxes[index]
        reach = int(math.ceil(self.s.clear_lines * box[3]))
        H, W = self.mask_labels.shape
        r0, r1 = max(0, box[1]), min(H, box[1] + box[3])
        found = set()
        if r1 <= r0:
            return []
        for start, end in ((max(0, box[0] - reach), box[0]),
                           (box[0] + box[2], min(W, box[0] + box[2] + reach))):
            if end <= start:
                continue
            window = self._solid_in((start, r0, end - start, r1 - r0))
            if not window.any():
                continue
            rest = window.copy()
            for k in range(real_count):
                if k == index:
                    continue
                o = boxes[k]
                if o[0] >= end or o[0] + o[2] <= start or o[1] >= r1 or o[1] + o[3] <= r0:
                    continue
                x0, x1 = max(start, o[0]) - start, min(end, o[0] + o[2]) - start
                y0, y1 = max(r0, o[1]) - r0, min(r1, o[1] + o[3]) - r0
                if window[y0:y1, x0:x1].any():
                    found.add(k)
                rest[y0:y1, x0:x1] = False
            if rest.any():
                return None
        return sorted(found)

    # -- candidates ------------------------------------------------------------

    def find_candidates(self, bgr: np.ndarray) -> Candidates:
        s = self.s
        height, width = bgr.shape[:2]
        sh = s.effective_scale_height(height)
        x0 = int(s.region_left * width)
        x1 = min(width, int(s.region_right * width))
        y0 = int(s.region_top * height)
        y1 = min(height, int(s.region_bottom * height))
        empty = Candidates([], [], [], [])
        if x1 <= x0 or y1 <= y0:
            return empty
        pad = s.context_padding(sh)
        px0, px1 = max(0, x0 - pad), min(width, x1 + pad)
        self._build_color_mask(bgr[y0:y1, px0:px1, :3], sh)
        narrow_shapes: list = []
        lines = self._find_text_lines(sh, narrow_shapes)
        if not lines:
            return empty
        plain = [l for l in lines if x0 <= l[0] + px0 and l[0] + l[2] + px0 <= x1]
        if not plain:
            return empty
        plain_count = len(plain)
        unions = self._speed_unions(plain, sh)
        narrow_inside = [n for n in narrow_shapes if x0 <= n[0] + px0 and n[0] + n[2] + px0 <= x1]
        pool_cache: list = []

        def pool():
            if not pool_cache:
                pool_cache.append(self._row_candidates(plain, len(plain), sh))
            return pool_cache[0]

        promoted = self._promote_narrow(narrow_inside, plain, unions, sh, pool) if narrow_inside else []
        local = list(plain) + [p[0] for p in promoted]
        real_count = plain_count + len(promoted)
        local += unions
        clear_known: dict = {}

        def clear(i):
            if i not in clear_known:
                clear_known[i] = self._is_clear(local[i])
            return clear_known[i]

        all_pairs = self._pair_lines(local, sh, real_count)
        if promoted:
            sandwich = {(n, b) for n, bottoms in promoted for b in bottoms}
            all_pairs = [p for p in all_pairs
                         if not (plain_count <= p[1] < real_count)
                         and not (plain_count <= p[0] < real_count
                                  and (local[p[0]], local[p[1]]) not in sandwich)]
        stacks = self._find_stacks(local, all_pairs, pool, sh)
        pairs, loose, crowd = self._sort_pairs(local, clear, all_pairs, real_count)
        shift = lambda r: (r[0] + px0, r[1] + y0, r[2], r[3])
        boxes = [shift(r) for r in local]
        shifted = {(shift(t), shift(b)): ([shift(r) for r in rows], [shift(r) for r in bl])
                   for (t, b), (rows, bl) in stacks.items()}
        return Candidates(boxes, pairs, loose, crowd, shifted)

    # -- reading rows ------------------------------------------------------------

    def read_distance_line(self, bgr, box) -> ir.DistanceReading | None:
        return self.rows.read_distance(bgr, box)

    def read_name_row(self, bgr, box) -> ir.NameRowRead:
        return self.rows.read_name(bgr, box)

    def reads_as_name_not_distance(self, bgr, box) -> bool:
        mask = ir.create_mask(bgr, box, keep_colors=True)
        return self.rows.read_name_mask(bgr, mask).usable and self.rows.read_distance_mask(mask) is None


# ---- plate tint (PlateColors.Tint) ------------------------------------------

PLATE_COLOR_MIN_PIXELS = 30


def _value_at(hist: np.ndarray, index: int) -> int:
    seen = np.cumsum(hist)
    k = int(np.searchsorted(seen, index, side="right"))
    return min(k, len(hist) - 1)


def _median_hist(hist: np.ndarray, count: int) -> float:
    if count % 2 == 1:
        return float(_value_at(hist, count // 2))
    return (_value_at(hist, count // 2 - 1) + _value_at(hist, count // 2)) / 2.0


def plate_tint(mask: ir.LineMask | None):
    if mask is None or mask.ink_colors is None:
        return None
    c = mask.ink_colors
    total = len(c)
    if total < PLATE_COLOR_MIN_PIXELS:
        return None
    mx = c.max(axis=1)
    threshold = _value_at(np.bincount(mx, minlength=256), total // 2)
    core = c[mx >= threshold]
    n = len(core)
    b = _median_hist(np.bincount(core[:, 0], minlength=256), n)
    g = _median_hist(np.bincount(core[:, 1], minlength=256), n)
    r = _median_hist(np.bincount(core[:, 2], minlength=256), n)
    top = max(r, g, b)
    return None if top <= 0 else (b / top, g / top, r / top)


def same_tint(a, b, tol: float) -> bool:
    return abs(a[0] - b[0]) <= tol and abs(a[1] - b[1]) <= tol and abs(a[2] - b[2]) <= tol


# ---- scan geometry, the read crop, and the two app-level operations ----------

def boundary_from_percents(top: int, bottom: int, left: int, right: int):
    """(left, top, right, bottom) as fractions, like IRIS's TrackingBoundary.
    IRIS clamps each margin to 0-30 %; so does this."""
    lim = lambda v: min(max(v, 0), 30)
    return lim(left) / 100.0, lim(top) / 100.0, (100 - lim(right)) / 100.0, (100 - lim(bottom)) / 100.0


@dataclass(frozen=True)
class ScanGeometry:
    width: int
    height: int
    capture: Rect
    settings: Settings

    @classmethod
    def create(cls, width: int, height: int, base: Settings, boundary=None):
        if boundary is not None:
            base = replace(base, region_left=boundary[0], region_top=boundary[1],
                           region_right=boundary[2], region_bottom=boundary[3])
        x0 = int(base.region_left * width)
        x1 = min(width, int(base.region_right * width))
        y0 = int(base.region_top * height)
        y1 = min(height, int(base.region_bottom * height))
        if x1 <= x0 or y1 <= y0:
            return None
        pad = base.context_padding(height)
        vpad = base.vertical_context_padding(height)
        cx0, cx1 = max(0, x0 - pad), min(width, x1 + pad)
        cy0, cy1 = max(0, y0 - vpad), min(height, y1 + vpad)
        w, h = float(cx1 - cx0), float(cy1 - cy0)
        settings = replace(base,
                           region_left=(x0 - cx0 + 0.25) / w,
                           region_right=min(1.0, (x1 - cx0 + 0.25) / w),
                           region_top=(y0 - cy0 + 0.25) / h,
                           region_bottom=min(1.0, (y1 - cy0 + 0.25) / h),
                           scale_height=height)
        return cls(width, height, (cx0, cy0, cx1 - cx0, cy1 - cy0), settings)


def read_crop_rect(line: Rect, width: int, height: int) -> Rect | None:
    """The crop the app reads a plate from (RegionPlanner.Crop, no motion)."""
    scale = height / 1440.0
    lh = line[3]
    margin = READ_MARGIN_PX * scale
    cx = line[0] + max(line[2], PATCH_MIN_WIDTH_PX * scale) / 2.0
    half = int(CROP_HALF_WIDTH_PX * scale + CROP_HALF_WIDTH_LINES * lh + CROP_SLACK_PX * scale + margin)
    x0, x1 = max(0, int(cx - half)), min(width, int(cx + half))
    y0 = max(0, int(line[1] - CROP_ABOVE_LINES * lh - margin))
    y1 = min(height, int(line[1] + lh + CROP_BELOW_LINES * lh + margin))
    if x1 <= x0 or y1 <= y0:
        return None
    return x0, y0, x1 - x0, y1 - y0


@dataclass
class ReadResult:
    cls: str                            # Name | Stop | Unclear
    reason: str = ""
    name: str | None = None
    rows: list | None = None            # name rows, window coordinates
    anchor: Rect | None = None          # the distance line, window coordinates


class ImageOps:
    """DetectorImageOps: scan a frame for distance lines, read one plate."""

    def __init__(self, rows: ir.RowReader, geometry: ScanGeometry):
        self.geometry = geometry
        self.window_height = geometry.height
        crop_settings = replace(geometry.settings, region_left=0.0, region_right=1.0,
                                region_top=0.0, region_bottom=1.0, scale_height=geometry.height)
        self.scan_detector = NameplateDetector(rows, geometry.settings)
        self.crop_detector = NameplateDetector(rows, crop_settings)
        self.cs = crop_settings

    def _px(self, at1440: float) -> float:
        return at1440 * self.window_height / 1440.0

    def scan(self, frame: np.ndarray) -> list:
        """Distance lines found in the frame's capture area, in window
        coordinates - DetectorImageOps.Scan with nothing tracked."""
        cx, cy, cw, ch = self.geometry.capture
        img = frame[cy:cy + ch, cx:cx + cw]
        cands = self.scan_detector.find_candidates(img)
        boxes = cands.boxes
        memo: dict = {}

        def distance_ok(line: int) -> bool:
            if line not in memo:
                memo[line] = self.scan_detector.read_distance_line(img, boxes[line]) is not None
            return memo[line]

        line_pairs: dict = {}
        crowd = {(t, b): nb for t, b, nb in cands.crowd}
        ordered = list(cands.pairs) + [(t, b) for t, b, _ in cands.crowd]
        for pair in ordered + list(cands.loose):
            line_pairs.setdefault(pair[0], []).append(pair)
            line_pairs.setdefault(pair[1], []).append(pair)
        taken, found = set(), []
        for top, bottom in ordered:
            if bottom in taken:
                continue
            nb = crowd.get((top, bottom))
            if nb is not None:
                if not distance_ok(bottom):
                    continue
                supported = True
                for line in nb:
                    if not any(ot not in (top, bottom) and ob not in (top, bottom) and distance_ok(ob)
                               for ot, ob in line_pairs.get(line, ())):
                        supported = False
                        break
                if not supported:
                    continue
            if distance_ok(bottom):
                taken.add(bottom)
                b = boxes[bottom]
                found.append((b[0] + cx, b[1] + cy, b[2], b[3]))
        return found

    def read_line(self, frame: np.ndarray, line: Rect) -> ReadResult:
        crop = read_crop_rect(line, frame.shape[1], frame.shape[0])
        if crop is None:
            return ReadResult("Unclear", "nocrop")
        x, y, w, h = crop
        return self.read(frame[y:y + h, x:x + w], crop, line, line[3])

    def read(self, image: np.ndarray, origin: Rect, expected: Rect, lh: float) -> ReadResult:
        cs = self.cs
        if image.shape[0] < self._px(cs.read_crop_min_height) or image.shape[1] < self._px(cs.read_crop_min_width):
            return ReadResult("Unclear", "offscreen")
        det = self.crop_detector
        cands = det.find_candidates(image)
        boxes = cands.boxes
        ox, oy = origin[0], origin[1]
        tol = max(cs.near_height_min_px, cs.near_height_tolerance * lh)
        anchors = []
        for i, b in enumerate(boxes):
            off_left = abs(b[0] + ox - expected[0])
            off_centre = abs(b[0] + b[2] / 2.0 + ox - (expected[0] + expected[2] / 2.0))
            if (min(off_left, off_centre) <= self._px(cs.anchor_max_offset_px)
                    and abs(b[1] + oy - expected[1]) <= cs.anchor_max_row_offset_lines * lh
                    and abs(b[3] - lh) <= tol):
                anchors.append(i)
        anchors.sort(key=lambda i: -boxes[i][2])
        bi = -1
        for i in anchors[:2]:
            if det.read_distance_line(image, boxes[i]) is not None:
                bi = i
                break
        if bi < 0:
            return ReadResult("Unclear", "nodist")
        bottom = boxes[bi]
        bottom_centre = bottom[0] + bottom[2] / 2.0
        tops = []
        for ti, t in enumerate(boxes):
            if ti == bi:
                continue
            gap = bottom[1] - (t[1] + t[3])
            if not (1 <= gap <= t[3] * cs.top_gap_max_lines) or \
                    abs(bottom[3] - t[3]) > max(self._px(cs.top_height_diff_min_px), t[3] * cs.top_height_diff_lines):
                continue
            if t[0] >= bottom[0] + bottom[2] or t[0] + t[2] <= bottom[0]:
                continue
            tops.append((gap, ti))
        tops.sort(key=lambda g: g[0])
        tol_c = self._px(cs.top_centre_tolerance_px)
        centred = [i for _, i in tops if abs(boxes[i][0] + boxes[i][2] / 2.0 - bottom_centre) <= tol_c]
        off = [i for _, i in tops if abs(boxes[i][0] + boxes[i][2] / 2.0 - bottom_centre) > tol_c]

        def left_aligned(t):
            return (abs(t[0] - bottom[0]) <= self._px(cs.top_left_tolerance_px)
                    and abs(t[2] - bottom[2]) >= self._px(cs.top_left_width_difference_px))

        valid = [i for i in centred if not left_aligned(boxes[i])]
        if valid:
            top = valid[0]
            result = self._read_pair(image, origin, cands, top, bi)
            strict = (top, bi) in cands.pairs or any(c[0] == top and c[1] == bi for c in cands.crowd)
            if result.cls != "Name" and not strict:
                return ReadResult("Unclear", "clutter")
            return result
        if centred:
            return ReadResult("Unclear", "leftcol")
        if off:
            return ReadResult("Unclear" if has_bright_background(image) else "Stop", "offcenter")
        return ReadResult("Unclear", "nothing")

    def _read_pair(self, image, origin, cands: Candidates, top: int, bottom: int) -> ReadResult:
        det = self.crop_detector
        cs = self.cs
        boxes = cands.boxes
        stack = cands.stack_of(boxes[top], boxes[bottom])
        rows = stack[0] if stack and stack[0] else [boxes[top]]
        blockers = stack[1] if stack else []
        name_box = boxes[top]
        name_read = det.read_name_row(image, name_box)
        if not name_read.text or not name_read.one_word:
            return ReadResult("Unclear", "unreadable") if not name_read.text else ReadResult("Stop", "notoneword")
        tint = plate_tint(name_read.mask)
        above = sorted((r for r in rows if r != name_box), key=lambda r: -r[1])
        kept_rows, reads = [name_box], [name_read]
        whole = len(above) == len(rows) - 1
        for row in above:
            read = det.read_name_row(image, row)
            row_tint = plate_tint(read.mask)
            if tint is not None and row_tint is not None and not same_tint(tint, row_tint, cs.stack_tint_tolerance):
                whole = False
                break
            if not read.text:
                kept_rows, reads = [name_box], [name_read]
                whole = False
                break
            if not read.one_word:
                return ReadResult("Unclear", "multiword")
            kept_rows.insert(0, row)
            reads.insert(0, read)
        rows = kept_rows
        if not whole:
            blockers = []
        text = "".join(r.text for r in reads)
        if any(not r.valid_chars for r in reads):
            return ReadResult("Stop", "badchars")
        total = sum(r.score for r in reads)
        if len(text) > cs.max_name_length:
            return ReadResult("Stop", "toolong")
        if len(text) < cs.min_name_length:
            return ReadResult("Unclear", "short")
        for blocker in blockers:
            if det.reads_as_name_not_distance(image, blocker):
                return ReadResult("Unclear", "partialstack")
        if not text or text[0] == "-" or text[-1] == "-":
            return ReadResult("Unclear", "edgehyphen")
        if total / len(text) < cs.min_name_score:
            return ReadResult("Unclear", "lowconf")
        win = lambda r: (r[0] + origin[0], r[1] + origin[1], r[2], r[3])
        line = boxes[bottom]
        return ReadResult("Name", "", text, [win(r) for r in rows], win(line))
