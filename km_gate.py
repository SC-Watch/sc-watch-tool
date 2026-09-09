# sc-watch - reads player handles off the Star Citizen HUD.
# Copyright (C) 2026 SC-Watch
#
# This program is free software: you can redistribute it and/or modify it
# under the terms of the GNU General Public License as published by the Free
# Software Foundation, either version 3 of the License, or (at your option)
# any later version.
#
# This program is distributed in the hope that it will be useful, but WITHOUT
# ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or
# FITNESS FOR A PARTICULAR PURPOSE. See the GNU General Public License for
# more details. You should have received a copy of the GNU General Public
# License along with this program. If not, see <https://www.gnu.org/licenses/>.

"""
Cheap structural gate for candidate range boxes.

A broad brightness locator hands us 15-20 candidate pairs per frame, and a
RapidOCR call on each costs 50-100ms - about 4.6 seconds a frame at Levski,
which is unusable. But we don't need OCR to decide whether a box is a range
readout: every real one ends in "km", in a fixed font at a fixed size.

So we template-match the last two glyphs. Fixed font means no scale or
rotation invariance is needed - just normalise the glyph pair to a canonical
box and compare bitmaps. That's microseconds, and it runs before any OCR.

The template is bootstrapped from boxes that OCR already confirmed, so it
learns the real font rather than one we guessed at. See build_km_template.py.
"""

from __future__ import annotations

from pathlib import Path

import cv2
import numpy as np
import paths

TEMPLATE_PATH = paths.resource("km_template.npy")

# Canonical size for the normalised "km" glyph pair.
TPL_W, TPL_H = 30, 18

MIN_GLYPH_H_FRAC = 0.45  # glyph must be this tall relative to the box
MIN_GLYPH_AREA = 4
MIN_GLYPHS, MAX_GLYPHS = 3, 8  # "N.Nkm" is 4-5 components; "NN.Nkm" up to 6


def segment(crop: np.ndarray, frac: float = 0.55, floor: int = 110) -> np.ndarray:
    """Threshold a crop against its own brightest pixels.

    Colour-agnostic by construction: whatever hue this particular label is
    drawn in, its glyphs are the brightest thing in its own bounding box.
    """
    v = cv2.cvtColor(crop, cv2.COLOR_BGR2HSV)[:, :, 2] if crop.ndim == 3 else crop
    thr = max(floor, int(v.max() * frac))
    return (v >= thr).astype(np.uint8) * 255


def glyph_boxes(mask: np.ndarray) -> list[tuple[int, int, int, int]]:
    """Connected components that look like glyphs, left to right."""
    n, _, stats, _ = cv2.connectedComponentsWithStats(mask, connectivity=8)
    min_h = max(3, int(mask.shape[0] * MIN_GLYPH_H_FRAC))
    out = []
    for i in range(1, n):
        x, y, w, h, area = stats[i]
        if h >= min_h and area >= MIN_GLYPH_AREA:
            out.append((int(x), int(y), int(w), int(h)))
    out.sort(key=lambda g: g[0])
    return out


SEGMENT_FRACS = (0.45, 0.55, 0.65, 0.75, 0.85)


def km_patch(crop: np.ndarray, frac: float = 0.55) -> np.ndarray | None:
    """Normalised bitmap of the final two glyphs, or None if the box is
    not shaped like a range readout at all.

    Note the period in 'N.Nkm' is deliberately dropped by the height filter -
    it is far shorter than 45% of the line. That is fine and in fact wanted:
    we only ever use the last two glyphs, which are always full-height 'k'
    and 'm', so excluding it consistently is better than including it
    sometimes.
    """
    mask = segment(crop, frac)
    gs = glyph_boxes(mask)
    if not (MIN_GLYPHS <= len(gs) <= MAX_GLYPHS):
        return None

    last_two = gs[-2:]
    x0 = last_two[0][0]
    x1 = max(g[0] + g[2] for g in last_two)
    y0 = min(g[1] for g in last_two)
    y1 = max(g[1] + g[3] for g in last_two)
    if x1 - x0 < 4 or y1 - y0 < 4:
        return None

    patch = mask[y0:y1, x0:x1]
    return cv2.resize(patch, (TPL_W, TPL_H), interpolation=cv2.INTER_AREA)


def load_template(path: Path = TEMPLATE_PATH) -> np.ndarray | None:
    if not path.exists():
        return None
    return np.load(path)


def _similarity(patch: np.ndarray, template: np.ndarray) -> float:
    """Soft IoU on the ink. Mean absolute difference is useless on bitmaps
    this sparse - two unrelated glyphs agree on the empty background and score
    ~0.8 regardless of shape."""
    a = patch.astype(np.float32) / 255.0
    b = template.astype(np.float32) / 255.0
    inter = np.minimum(a, b).sum()
    union = np.maximum(a, b).sum()
    return float(inter / max(union, 1e-6))


def score(crop: np.ndarray, template: np.ndarray) -> float:
    """Best 'km' match over a small sweep of segmentation thresholds.

    A single fixed threshold segments only some boxes cleanly - it dropped 38%
    of genuine range boxes in testing, which the gate then discarded. Sweeping
    costs nothing measurable next to an OCR call, and the max over thresholds
    is the right statistic: we are asking whether ANY sane segmentation of this
    box ends in 'km'.
    """
    best = 0.0
    for frac in SEGMENT_FRACS:
        patch = km_patch(crop, frac)
        if patch is None:
            continue
        best = max(best, _similarity(patch, template))
    return best


def build_template(patches: list[np.ndarray]) -> np.ndarray:
    """Average several confirmed examples into one template."""
    stack = np.stack([p.astype(np.float32) for p in patches])
    return stack.mean(axis=0).astype(np.uint8)
