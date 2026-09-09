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
Does the detector work on a frame from someone else's screen?

    python check_resolution.py corpus_16x9/*.png

Everything the tool knows about geometry was calibrated on one 5120x1440
display. This reports, per frame, what a different resolution actually does to
each assumption - rather than asking someone to eyeball a debug overlay.

Checks, in the order they can break:

  1. LETTERBOXING. A screenshot shared through a phone or a chat client is
     often padded with black. Every fraction-based region in this tool is a
     fraction of the FRAME, so padding silently shifts all of them. Detected by
     looking for uniformly dark columns and rows at the edges.

  2. LOCATORS. The brightness masks are absolute thresholds (V>=245 and so on).
     Those are properties of the game's UI colours, not of resolution, so they
     should survive - but a different display gamma or HDR setting would move
     them, and that is worth knowing before blaming anything else.

  3. GEOMETRY. Label height in pixels scales with resolution. The pairing rules
     use `radius` (how far below a name its range line may sit) in PIXELS, so a
     1080p frame has labels roughly half the height of a 1440p one and the
     tolerances may not fit.

  4. WHAT IT ACTUALLY READ, against what a person can see in the frame.
"""

from __future__ import annotations

import sys
from pathlib import Path

import cv2
import numpy as np

import chat
import sc_detector as sd


def letterbox(frame, thresh: int = 12) -> tuple[int, int, int, int]:
    """(left, right, top, bottom) padding of near-black border, in pixels."""
    g = cv2.cvtColor(frame, cv2.COLOR_BGR2GRAY)
    h, w = g.shape
    col = g.max(axis=0)
    row = g.max(axis=1)
    left = int(np.argmax(col > thresh))
    right = int(w - 1 - np.argmax(col[::-1] > thresh))
    top = int(np.argmax(row > thresh))
    bottom = int(h - 1 - np.argmax(row[::-1] > thresh))
    return left, w - 1 - right, top, h - 1 - bottom


def report(path: str, ocr):
    frame = cv2.imread(path, cv2.IMREAD_COLOR)
    if frame is None:
        print(f"\n{path}: could not read")
        return
    h, w = frame.shape[:2]
    print(f"\n{'=' * 72}\n{Path(path).name}   {w}x{h}   aspect {w/h:.2f}")

    l, r, t, b = letterbox(frame)
    if l + r + t + b > 8:
        iw, ih = w - l - r, h - t - b
        print(f"  LETTERBOXED: {l}px left, {r}px right, {t}px top, {b}px bottom")
        print(f"    game content is {iw}x{ih}, aspect {iw/ih:.2f}")
        print(f"    every fraction-of-frame region is offset by this. The tool "
              f"should be fed the")
        print(f"    unpadded frame, or told the content rectangle.")
        inner = frame[t:h - b, l:w - r]
    else:
        print("  no letterboxing detected")
        inner = frame

    # 2. locators - do the brightness masks find anything at all?
    ih, iw = inner.shape[:2]
    masks = sd.hud_masks(inner, ih)
    for name, m in (masks.items() if isinstance(masks, dict)
                    else enumerate(masks)):
        on = int((m > 0).sum())
        print(f"  locator {name}: {on:>7} px lit ({100*on/(ih*iw):.3f}% of frame)")

    # 3+4. what the detector makes of it
    for label, img in (("as shared", frame), ("content only", inner)):
        if label == "content only" and inner is frame:
            continue
        contacts = sd.detect(img, ocr)
        players = [c for c in contacts if c.kind == "player"]
        other = [c for c in contacts if c.kind != "player"]
        print(f"  detect({label}): {len(players)} player(s), "
              f"{len(other)} other")
        for c in players:
            print(f"      {c.name:<22} {c.range_km:>6.1f} km   "
                  f"@ {c.name_box.x},{c.name_box.y}  "
                  f"label h={c.name_box.h}px")
        for c in other:
            print(f"      [{c.kind}] {c.name or '?'}")

    # 5. chat region, as configured
    crop, (x0, y0) = chat.crop_region(inner)
    lines = chat.read(inner, ocr)
    print(f"  chat region {crop.shape[1]}x{crop.shape[0]} at {x0},{y0} "
          f"-> {len(lines)} handle(s)")
    for c in lines:
        print(f"      {c.handle}")


def main() -> int:
    paths = sys.argv[1:]
    if not paths:
        paths = sorted(str(p) for p in Path("corpus_16x9").glob("*")
                       if p.suffix.lower() in (".png", ".jpg", ".jpeg"))
    if not paths:
        print(__doc__)
        print("no frames found. Put them in corpus_16x9/ first.")
        return 1
    ocr = sd.make_ocr()
    for p in paths:
        report(p, ocr)
    print(f"\n{'=' * 72}")
    print("Compare the handles listed above against what you can see in each "
          "frame.\nA name the tool missed is the interesting case; a name it "
          "invented is worse.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
