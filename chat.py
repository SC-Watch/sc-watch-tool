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
DEFAULT_REGION = (0.15, 0.30, 0.35, 0.45)      # x, y, w, h - legacy, see below

# THE CHAT WINDOW IS ANCHORED TO SCREEN CENTRE, NOT TO THE LEFT EDGE
# ------------------------------------------------------------------
# DEFAULT_REGION above is a fraction of the frame WIDTH, and that is the bug
# it caused: it works on a wide screen and silently misses on a narrow one.
# Measured on eight frames at three resolutions, the chat window's left edge
# sits a constant number of pixels left of screen centre:
#
#   2559x1439   x0 =   96    centre - x0 = 1183.5
#   3440x1440   x0 =  537    centre - x0 = 1183.0
#   5120x1440   x0 = 1377    centre - x0 = 1183.0
#
# 1181-1185 across every sample. As a fraction of WIDTH that same edge is
# 0.038, 0.156 and 0.269 - which is why 0.15 framed the chat on the 3440 and
# 5120 screens and framed a poster on the 2559 one, reading zero handles.
#
# Offsets are scaled by HEIGHT against REFERENCE_HEIGHT, the same convention
# _scaled() uses in sc_detector, because the HUD scales with vertical
# resolution. Every sample here is ~1440 tall, so the corpus CANNOT distinguish
# height-scaled from fixed-pixel: this follows the existing convention rather
# than a measurement, and a 1080p or 4K frame would settle it.
#
# WITH THE HELMET OFF THE WINDOW SITS FURTHER LEFT. Every sample above had a
# helmet on, which draws a small pitch indicator left of the chat and pushes
# the chat right (the user found the cause, 2026-10-17). Helmet off, on foot
# in a station at 5120x1440, the [GLOBAL] tags began 1228 px left of centre:
# the 1190 edge cut "[GLO" off every line, OCR read "BALI EXMPLHANDLE:", no
# channel tag matched, and every chat read came back empty with the chat
# plainly on screen.
#
# One wide region for both was measured and refused: on 54 saved frames
# (helmet on) it LOST 32 names - the pitch indicator came into the crop - and
# reading both regions and combining them found 251 against 220, but 17 of
# the 31 extra names were look-alike spellings of names the other read had
# found: two contacts for one person. So read() tries the helmet-on region
# and only if that finds nobody - helmet off cuts every tag, so it always
# finds nobody - the helmet-off one. One region's answer, never both.
REFERENCE_HEIGHT = 1440
_ANCHOR_LEFT = 1190.0     # px left of centre at 1440p (measured 1183 + margin)
_WIDTH = 1120.0           # longest message measured ran 1032 px from the anchor
_ANCHOR_LEFT_BARE = 1290.0   # helmet off: measured 1228 + margin
_WIDTH_BARE = 1220.0         # the same right edge as the helmet-on region
_HEIGHT = 520.0           # rows measured 179-211 px; room for a fuller window
_BOTTOM = 0.590           # of frame height; measured 0.555-0.581


def default_region(frame_w: int, frame_h: int, helmet: bool = True) -> tuple:
    """The chat rectangle for THIS frame, as x, y, w, h fractions - where it
    sits with the helmet on, or with `helmet` False, off (see _ANCHOR_LEFT_BARE).

    Returned as fractions because that is what the rest of the pipeline and
    the settings file already speak, even though the underlying measurement
    is in pixels from the centre.
    """
    anchor, width = ((_ANCHOR_LEFT, _WIDTH) if helmet
                     else (_ANCHOR_LEFT_BARE, _WIDTH_BARE))
    s = frame_h / REFERENCE_HEIGHT
    x0 = frame_w / 2.0 - anchor * s
    y1 = _BOTTOM * frame_h
    y0 = y1 - _HEIGHT * s
    return (max(0.0, x0) / frame_w, max(0.0, y0) / frame_h,
            min(width * s, frame_w - x0) / frame_w,
            min(_HEIGHT * s, y1) / frame_h)

# The channel tags, spelled to absorb the substitutions OCR makes inside them
# (O/0, I/1/L, S/5). Chat is a closed set of channels, so this can be a list.
_TAG = (r"(?:GL[O0]B[A4]L|L[O0]C[A4]L|P[A4]RTY|[O0]RG|SYSTEM|TE[A4]M|"
        r"GUILD|SQU[A4]D|[A4]DMIN|SERVER)")

# Tag, then handle, then colon.
#
# The tag is an ALTERNATION of known channel names, not the [A-Z]{3,12} it
# used to be, and that is a correctness fix rather than a tidy-up. [A-Z]{3,12}
# is greedy, so when OCR read the closing bracket as an I - which it does
# constantly, the glyphs are nearly identical - the tag match ran straight on
# into the handle and ate its capital:
#
#   [GLOBALIChrisbykills:     ->  tag 'GLOBALIC',  handle 'hrisbykills'
#   [GLOBALIBustyWolf:        ->  tag 'GLOBALIB',  handle 'ustyWolf'
#   [GLOBALINightDragon850:   ->  tag 'GLOBALIN',  handle 'ightDragon850'
#
# Three of five handles in one real screenshot, each silently one character
# short. A truncated handle is not a failed read, it is a DIFFERENT PERSON -
# 'HORNE' is somebody's actual account - so this was manufacturing exactly the
# misattribution the rest of the tool refuses to make.
#
# A tag this does not know now fails to match at all, and that is the right
# way round: no handle is recoverable from a line we cannot parse, while a
# wrong handle looks exactly like a right one.
LINE_RE = re.compile(
    r"[\[\(]?\s*" + _TAG + r"\s*[\]\)\}IJ|1l]?\s*"   # [GLOBAL] / [GLOBALI / (LOCAL
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
    """The chat rectangle of a frame, as (crop, (x0, y0)).

    `region` None or "auto" means work it out from the frame - see
    default_region(). An explicit 4-tuple always wins, because chat can be
    dragged anywhere and no measurement beats someone who has looked.
    """
    fh, fw = frame.shape[:2]
    if region is None or region == "auto":
        region = default_region(fw, fh)
    x, y, w, h = region
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

    With the region worked out from the frame ("auto"), the helmet-on place is
    read first and, only if it finds nobody, the helmet-off place - see
    _ANCHOR_LEFT_BARE. A region you set yourself is read as it is.
    """
    return read_located(frame, ocr, region)[0]


# Where read_located()'s answer came from.
PLACE_ON, PLACE_OFF, PLACE_BOTH, PLACE_SET = (
    "helmet on", "helmet off", "helmet on+off", "your region")


def read_located(frame, ocr, region=None) -> tuple[list[ChatLine], str]:
    """read(), and where its answer came from: PLACE_ON or PLACE_OFF for
    "auto", PLACE_SET for a region you set - or, when "auto" found nobody,
    PLACE_BOTH: both places were looked at. For the session log and the
    watcher's "nothing readable" line, which used to point at the chat
    region setting when the helmet was the reason."""
    if region is None or region == "auto":
        fh, fw = frame.shape[:2]
        lines = _read(frame, ocr, default_region(fw, fh))
        if lines:
            return lines, PLACE_ON
        lines = _read(frame, ocr, default_region(fw, fh, helmet=False))
        return lines, (PLACE_OFF if lines else PLACE_BOTH)
    return _read(frame, ocr, region), PLACE_SET


def _read(frame, ocr, region) -> list[ChatLine]:
    """read() for one region."""
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
    import imgio
    import sc_detector as sd

    paths = sys.argv[1:] or ["corpus/clean_194744_120_p24.png"]
    ocr = sd.make_ocr()
    for p in paths:
        frame = imgio.imread(p)
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
