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
Read handles out of the in-game chat window.

    python chat.py corpus/clean_194744_120_p24.png

A second channel that needs no ping. Chat lines carry a handle in a fixed
position - `[GLOBAL] SaintEvo: o7` - so anyone talking near you is nameable
without ever putting a reticle on them, and station chat names people you will
never get a HUD label for.

WHAT THE LINE LOOKS LIKE, AND WHAT SURVIVES OCR
-----------------------------------------------
Measured on the reference frame. The channel tag OCRs badly and the handle OCRs
well, which is lucky, because only the handle matters:

    [GLOBAL] Psylencer:  thanks saint     ->  '[GLOBAL) Psylencer:thanks saint'
    [GLOBAL] SaintEvo:   o7               ->  '[GLOBAL]SaintEvo:07'
    [GLOBAL] Jackster:   try Alt N...     ->  '[GLOBAL]Jackster:'
    [GLOBAL] orion42m:   That happened... ->  '[GLOBAL] orion42m: That happened to me as well'

The closing bracket comes back as `]`, `)`, `I`, `J` or nothing, and the spaces
either side of it are optional. So the tag is matched loosely and the handle is
taken as whatever sits between the tag and the first colon.

CASE IS PRESERVED IN CHAT
-------------------------
HUD labels are upper-case; chat is not. `SaintEvo` reads as `SaintEvo`, which
is a *better* string than the HUD would ever give - but the database stores
handles upper-case, so it is folded on the way in. The original casing is kept
on the ChatLine for display, since it is what the person actually chose.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

# Region of the screen the chat window occupies, as fractions of the frame, so
# one setting works across resolutions. Defaults measured from the reference
# 5120x1440 frame, where the text sat at x 0.26-0.35, y 0.54-0.62 - widened
# generously because chat grows UPWARD as messages arrive, and a region that
# only covers the newest line reads one name instead of five.
DEFAULT_REGION = (0.15, 0.30, 0.35, 0.45)      # x, y, w, h

# Tag, then handle, then colon. The tag is matched loosely because it is the
# part OCR mangles: letters, then an optional bracket-ish character, then
# optional space. A-Z only in the tag - a lower-case run belongs to the handle.
LINE_RE = re.compile(
    r"[\[\(]?\s*[A-Z]{3,12}\s*[\]\)\}IJ|1l]?\s*"     # [GLOBAL] / [GLOBALI / (LOCAL
    r"([A-Za-z0-9_\-]{2,40})"                        # the handle
    r"\s*:",                                         # the colon that ends it
)

# Words that appear where a handle would and are not people. The system speaks
# in the same shape as a player does.
NOT_HANDLES = {
    "GLOBAL", "LOCAL", "PARTY", "ORG", "SYSTEM", "TEAM", "GUILD", "SQUAD",
    "ADMIN", "SERVER", "NOTICE", "INFO", "ERROR", "WARNING", "CHAT", "ALL",
}


@dataclass
class ChatLine:
    handle: str          # as written by its owner, e.g. 'SaintEvo'
    text: str            # the whole OCR line it came from
    conf: float = 0.0

    @property
    def key(self) -> str:
        """Upper-case, the way the database stores handles."""
        return self.handle.upper()


def crop_region(frame, region=None):
    """The chat rectangle of a frame, as (crop, (x0, y0))."""
    x, y, w, h = region or DEFAULT_REGION
    fh, fw = frame.shape[:2]
    x0, y0 = int(fw * x), int(fh * y)
    x1, y1 = min(fw, int(fw * (x + w))), min(fh, int(fh * (y + h)))
    return frame[y0:y1, x0:x1], (x0, y0)


def handles_in(text: str) -> str | None:
    """The handle in one OCR line, or None if it is not a chat line."""
    m = LINE_RE.search(text or "")
    if not m:
        return None
    h = m.group(1)
    if h.upper() in NOT_HANDLES:
        return None
    # A handle that is all digits is a timestamp or a message fragment.
    if h.isdigit():
        return None
    return h


def read(frame, ocr, region=None) -> list[ChatLine]:
    """Every distinct handle the chat window is currently showing.

    Deduplicated on the upper-case handle, keeping the first spelling seen:
    one person usually has several lines on screen, and they are one contact,
    not five sightings.
    """
    crop, _ = crop_region(frame, region)
    if crop.size == 0:
        return []
    out: list[ChatLine] = []
    seen: set[str] = set()
    for line in ocr.lines(crop):
        h = handles_in(line.text)
        if not h or h.upper() in seen:
            continue
        seen.add(h.upper())
        out.append(ChatLine(handle=h, text=line.text,
                            conf=float(getattr(line, "conf", 0.0) or 0.0)))
    return out


def main() -> int:
    import sys
    import cv2
    import sc_detector as sd

    paths = sys.argv[1:] or ["corpus/clean_194744_120_p24.png"]
    ocr = sd.make_ocr()
    for p in paths:
        frame = cv2.imread(p, cv2.IMREAD_COLOR)
        if frame is None:
            print(f"{p}: could not read")
            continue
        crop, (x0, y0) = crop_region(frame)
        print(f"\n{p}  [{frame.shape[1]}x{frame.shape[0]}] "
              f"chat region {crop.shape[1]}x{crop.shape[0]} at {x0},{y0}")
        lines = read(frame, ocr)
        if not lines:
            print("  no chat handles found")
        for c in lines:
            print(f"  {c.handle:<24} conf {c.conf:.2f}   from {c.text!r}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
