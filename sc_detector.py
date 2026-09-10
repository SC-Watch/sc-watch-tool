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
Star Citizen contact-label detector.

Finds the "NAME / N.Nkm" pairs that appear on the flight HUD after a ping,
and hands back tight crops ready for OCR.

The whole approach rests on three things measured from real 5120x1440 frames:

  1. Contact-label text sits in a narrow cyan band, mean RGB (82, 223, 233),
     and occupies well under 2% of the screen. Masking on that colour throws
     away ~98% of the frame before any expensive work happens.

  2. Every contact renders as two stacked boxes: the NAME on top, the RANGE
     directly below it, horizontally CENTRE-ALIGNED to within a couple of
     pixels, with a gap of ~11-13px at 1440p.

  3. The range box always reads "N.Nkm". Other HUD elements are also stacked
     and centred (DECOY/NOISE, H-FUEL/Q-FUEL, 100%/AB, 0/m-s), so the range
     regex is what separates real contacts from cockpit furniture. On the
     reference frame this rejected 4/4 false positives and kept 4/4 contacts.

Everything is tuned against 1440p vertical and scaled from there, so it should
survive a resolution change without retuning.
"""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass
from pathlib import Path

import cv2
import numpy as np

import km_gate
import paths

_KM_TEMPLATE: np.ndarray | None = None
_KM_LOADED = False


def _km_template():
    """Load the 'km' template once. Absent means the gate is skipped and every
    candidate goes to OCR - slower, but correct."""
    global _KM_TEMPLATE, _KM_LOADED
    if not _KM_LOADED:
        _KM_TEMPLATE = km_gate.load_template()
        _KM_LOADED = True
        if _KM_TEMPLATE is None:
            print("! no km_template.npy - gate disabled, run build_km_template.py",
                  file=sys.stderr)
    return _KM_TEMPLATE

# --------------------------------------------------------------------------
# Tuning. Values measured from SC 4.5 HDR captures at 5120x1440.
# --------------------------------------------------------------------------

REFERENCE_HEIGHT = 1440  # everything below is expressed at this vertical res

# Contact-label cyan, MEASURED BUT NO LONGER USED.
#
# The locator was hue-based once. It is brightness-based now (LOCATE_V and the
# neutral/red thresholds below), because a colour mask cannot see a red hostile
# label and a cyan friendly one with the same rule, and every attempt to widen
# it far enough to catch both let terrain in. See the README on hue-agnostic
# masking.
#
# The numbers are kept because re-measuring them costs captures, and because
# anyone restoring a colour rule will want the starting point rather than the
# conclusion:
#
#   contact labels : mean BGR (82, 223, 233) -> ~184 deg, 65% sat, 91% val
#   party join feed: mean BGR (14, 226, 222) -> ~179 deg, 94% sat, 89% val
#
# The old rule was G>=150, B>=150, R<160, B>=R+60. It kept both cyans; the
# saturation split is only needed to tell them apart.

GLYPH_MIN_H, GLYPH_MAX_H = 7, 26  # text row height
BOX_MIN_W, BOX_MAX_W = 18, 520  # a whole word/line after dilation
MIN_CORE_PIXELS = 25  # unblurred mask pixels inside the box

DILATE_PX = 7  # horizontal only: merges glyphs into words

PAIR_MAX_CENTRE_DRIFT = 5  # name and range centres line up this tightly
DRIFT_FRACTION = 0.15      # ...plus this share of the name width
PAIR_MIN_GAP, PAIR_MAX_GAP = 4, 20  # vertical space between the two boxes
# "N.Nkm" box width. The upper bound was measured entirely on contacts under
# 5km, where the range is five glyphs ('1.8km'). Longer ranges are wider -
# '12.3km' is six, '112.3km' seven - so a window fitted to close contacts
# silently drops distant ones. Widened deliberately: the km gate rejects
# non-range boxes on shape, so the width filter does not have to be the thing
# keeping junk out, and each extra candidate only costs 0.5ms of gate.
RANGE_MIN_W, RANGE_MAX_W = 26, 150

LOCATE_V = 245  # value floor for finding HUD text, any colour

# The km gate is OFF by default. It looked excellent when measured on one ship
# in one location - 100% junk rejection, 100% real retention - but on 72 frames
# from a real session it dropped 28% of genuine readings while finding exactly
# the same 12 handles. Those lost readings are votes, which is what makes a
# contact trustworthy.
#
# The cause is not the threshold or an undertrained template (rebuilding it
# from varied ranges recovered only 2 of 27). Most pairs score exactly 0
# because km_patch cannot segment them at all, and a real range box that fails
# segmentation is rejected identically to junk.
#
# It buys speed - roughly 0.9s/frame against 2.5s - which matters much less now
# announcements are progressive: the first name still arrives after one frame
# either way, only burst completion is slower. Enable with --gate if you want
# that speed back and can accept thinner voting.
KM_GATE_THRESHOLD = 0.43
KM_GATE_ENABLED = False

OCR_UPSCALE = 3  # native glyphs are 11-14px; OCR wants 30+
OCR_MARGIN_NATIVE = 16  # white border, in native px, before upscaling
CROP_PAD = 6  # pixels of frame included around each box

# A contact's range line carries a bracketed closing-speed readout whenever
# there is relative radial velocity: 'TREKMASTER' over '6.2km [-8m/s]'. The
# original pattern was anchored at both ends, so a perfectly-read red hostile
# was thrown away over the suffix. The suffix must start with a bracket, which
# is what keeps junk like '3.34M/3.79M' out - that is the only string in 414
# range reads from a real session that a bare '.*' tail would have admitted.
RANGE_RE = re.compile(r"^\s*(\d+(?:\.\d+)?)\s*(km|m)\s*(?:[\[({|].*)?$",
                      re.IGNORECASE)

# SC handles are Latin alphanumerics plus underscore. Anything the OCR returns
# outside this is a misread, not a name.
HANDLE_RE = re.compile(r"^[A-Za-z0-9_-]{3,24}$")

# Abandoned-ship serial numbers, e.g. SP-9695-PW, JD-2266-FC, LL-8589-8W.
# Always two letters, four digits, two alphanumerics.
SHIP_SERIAL_RE = re.compile(r"^[A-Z]{2}-\d{4}-[A-Z0-9]{2}$")

# OCR truncates at the end of a line, so serials also arrive part-read:
# 'SP-969', 'SP-9695', 'JR-840'. Matching only the complete form let those
# through as handles - they got announced and sent to RSI as if they were
# people. Two letters, a hyphen and a digit is enough to call it: no real
# handle we have seen begins that way.
PARTIAL_SERIAL_RE = re.compile(r"^[A-Z]{2}-\d")


def _is_unknown_label(s: str) -> bool:
    """Is this the game's UNKNOWN label, however badly it was read?

    Imported lazily so sc_detector keeps no import-time dependency on
    consensus. They are peers - consensus votes on what this module reads - and
    a top-level import here would make the direction of that relationship
    ambiguous for no benefit.
    """
    from consensus import same_contact
    # LENGTH-BOUNDED, because same_contact deliberately treats a prefix as a
    # match - that is what lets a truncated 'UNKNO' resolve. Unbounded it also
    # swallows any handle STARTING with the word, and 'UNKNOWNSOLDIER' is a
    # perfectly ordinary name for someone to choose. Discarding a real player
    # silently is much worse than announcing one asteroid.
    #
    # The label is seven characters. Every damaged reading seen has been nine
    # or fewer ('UNKNOWN00', 'UNKNOWIN', '-UNKNOWN'), so ten leaves margin
    # without reaching handle-length words.
    return len(s) <= 10 and same_contact(s, "UNKNOWN")


def classify_name(raw: str) -> tuple[str, str | None]:
    """Sort a raw OCR reading into what it actually is.

    Returns (kind, cleaned) where kind is one of:
      player       a real handle, worth reporting and looking up
      ship_serial  an abandoned ship's serial number, not a person
      npc_crew     a crewed NPC vessel - those are named "Firstname Lastname"
      unreadable   OCR produced nothing usable

    The space test has to happen BEFORE stripping whitespace. Player handles
    cannot contain spaces, so an internal space means an NPC crew name - but
    strip spaces first and "JOHN SMITH" becomes "JOHNSMITH", which passes as a
    handle and gets reported as a player.

    A spurious OCR space inside a real handle would lose that one reading, not
    the contact: other frames in the burst still vote.
    """
    s = raw.strip()
    if not s:
        return "unreadable", None
    if any(c.isspace() for c in s):
        return "npc_crew", s
    s = s.upper()

    # Trim punctuation off the ENDS only. A label clipped by something bright
    # in front of it reads as 'WZOMBIETPAND.' - the trailing dot is the
    # recogniser's guess at the glyphs it lost, not part of anyone's handle,
    # and rejecting the whole name over it threw away a real contact. Interior
    # junk is left alone: that means the read is genuinely broken, and a
    # truncated-but-clean name still matches the full one via the prefix rule.
    s = s.strip(".,:;'\"`|/\\()[]{}*+=<>?!~^$%&")

    if SHIP_SERIAL_RE.match(s) or PARTIAL_SERIAL_RE.match(s):
        return "ship_serial", s
    if not HANDLE_RE.match(s):
        return "unreadable", None

    # The game's own UNKNOWN label, thrown out HERE rather than downstream.
    #
    # It used to classify as a player and be filtered three separate times
    # further along, after clustering and voting had already run on it. That
    # was wasted work on the most common label on a busy screen, and one of
    # those places kept a reference to the whole frame as evidence, so a scene
    # full of asteroids pinned several megapickels per burst for something that
    # was going to be discarded anyway.
    #
    # There is no useful version of reporting these. An UNKNOWN is an asteroid,
    # a cow, a crate or anything else the game has not identified, and a pilot
    # reading their own instruments already knows which. Announcing them is
    # noise that trains you to ignore the tool.
    #
    # Matched fuzzily, because the label is read by the same recogniser as
    # everything else: 'UNKNO', 'UNKNOWIN', 'JNKNOWN' and 'UNKNOWN00' are all
    # the same word with the same meaning.
    if _is_unknown_label(s):
        return "unknown_contact", s
    return "player", s


@dataclass
class Box:
    x: int
    y: int
    w: int
    h: int
    core: int

    @property
    def cx(self) -> float:
        return self.x + self.w / 2.0

    @property
    def bottom(self) -> int:
        return self.y + self.h

    def crop(self, frame: np.ndarray, pad: int = 2) -> np.ndarray:
        y0 = max(0, self.y - pad)
        x0 = max(0, self.x - pad)
        return frame[y0 : self.bottom + pad, x0 : self.x + self.w + pad]


@dataclass
class Contact:
    """A name box with a confirmed range box under it."""

    name_box: Box
    range_box: Box
    range_km: float | None = None
    name: str | None = None
    kind: str = "unreadable"  # player | ship_serial | npc_crew | unreadable
    # Set only by the focused direct-OCR path: which recogniser line this came
    # from, so the range-retry pass knows which names already paired.
    source_line: "OcrLine | None" = None


def merge_contacts(wide: list["Contact"], centre: list["Contact"],
                   frame_h: int = REFERENCE_HEIGHT) -> list["Contact"]:
    """Combine a full-screen read with a centre-only read of the same frame.

    The two passes see the same screen differently on purpose. The full read
    covers everything but spreads a fixed OCR budget across every label on
    screen, so a small or awkward one loses. The centre read crops to the
    middle and lifts that cap, so the contact you are actually pointing at gets
    the whole budget. Running both and merging gets the periphery AND a good
    read of your target. Measured at about 1.4x a wide read, not 2x: the
    centre crop is smaller and usually holds fewer labels.

    CENTRE WINS on any contact both passes found. It is the higher-quality read
    by construction - more budget per label, no competition - so when the two
    disagree about a name, the uncapped one is the better evidence.

    Matching is by POSITION, not by name. Two passes over one frame see the same
    pixels in the same place, so a name box within a few pixels of another is
    the same label. Matching by name would be circular here: the whole reason
    for the second pass is that the first may have read the name wrong, and a
    misread would then look like a different contact and be reported twice.
    """
    if not centre:
        return list(wide)
    if not wide:
        return list(centre)

    # Generous, because the two passes crop and scale differently and a box
    # edge can land a pixel or two apart. Still far tighter than the gap
    # between two real labels, which the pairing rules already keep apart.
    near = _scaled(24, frame_h)

    def same_label(a: "Contact", b: "Contact") -> bool:
        return (abs(a.name_box.x - b.name_box.x) <= near
                and abs(a.name_box.y - b.name_box.y) <= near)

    out = list(centre)
    for c in wide:
        if not any(same_label(c, k) for k in centre):
            out.append(c)
    return out


def _scaled(value: int, frame_h: int) -> int:
    return max(1, int(round(value * frame_h / REFERENCE_HEIGHT)))


def _scaled_area(value: int, frame_h: int) -> int:
    """Scale a PIXEL COUNT, which grows with area rather than with height.

    MIN_CORE_PIXELS was the one threshold here expressed in pixels-of-ink
    instead of pixels-of-height, and it was not scaled at all. A label covers
    (h/1440)^2 of the pixels it covers at the reference resolution, so on a
    1080p screen a real label keeps 56% of its ink and on a 1600x900 one only
    39% - and the fixed floor of 25 started rejecting genuine contacts that
    were simply rendered smaller.

    At frame_h == REFERENCE_HEIGHT this returns exactly `value`, so the corpus
    results are unchanged by construction.
    """
    scale = (frame_h / REFERENCE_HEIGHT) ** 2
    return max(4, int(round(value * scale)))


NEUTRAL_MIN = 170  # min(R,G,B) floor for near-white text
RED_MIN = 245      # red-channel floor for red/orange/white text
# Second, much higher neutral floor, for hazed daylight scenes. Measured on a
# sunlit Pyro 5a frame where a plainly legible label was found by nothing: the
# atmospheric haze sits at V=255 and R=255, and min(R,G,B) ~200, so all three
# locators above kept 98% of the crop, the whole region became one component,
# and _drop_oversized deleted the text along with it. The label glyphs are
# pure white (255,255,255), so a floor above the haze separates them: it kept
# 8.3% of that crop instead of 99%. Stable anywhere in 248-255; 250 is the
# middle of the plateau.
NEUTRAL_HI = 250

# OFF by default, and the reason is the same one that turned the km gate off:
# measured on a full session rather than the scene it was built for.
#
# Over 78 bursts it took bursts detecting nothing at all from 13 to 6 and
# recovered two contacts that were otherwise lost outright - RAYOFTHEROAD, and
# TREKMASTER on a burst that had found nothing. It also added 19 new name
# strings, of which ONE was a correct handle. Fourteen were garbled variants of
# the same two people (LTHLDRAGON, ILTHLEDRAGON, DDRINOMERTS, JOFOERTS...) at
# one or two votes, which land in sightings.csv as phantom handles.
#
# One useful recovery per fourteen phantoms is a bad default for a database
# that is meant to accumulate evidence about real people. Enable it with
# --haze when you are over sunlit terrain and want the read anyway.
HAZE_LOCATOR_ENABLED = False


def _drop_oversized(raw: np.ndarray, frame_h: int) -> np.ndarray:
    """Keep only components small enough to be glyphs."""
    limit = _scaled(GLYPH_MAX_H, frame_h)
    n, lab, stats, _ = cv2.connectedComponentsWithStats(raw, connectivity=8)
    keep = np.zeros(n, np.uint8)
    keep[(stats[:, cv2.CC_STAT_HEIGHT] <= limit) &
         (stats[:, cv2.CC_STAT_WIDTH] <= limit * 6)] = 255
    keep[0] = 0
    return keep[lab]


def hud_masks(bgr: np.ndarray, frame_h: int | None = None) -> list[np.ndarray]:
    """Two complementary locators, computed independently.

    Neither works alone, because HSV Value is max(R,G,B):

      BRIGHT   V >= 245 catches HUD text in any colour - red, amber, cyan,
               white - which is essential, since label colour encodes contact
               state. But a saturated cyan nebula is also V=255, so white text
               drawn on one merges into it. Measured on a real miss: text V=255
               against nebula V=247, a gap of 8. Useless.

      NEUTRAL  min(R,G,B) >= 170 is high only for near-white pixels. Against
               that same nebula the gap was 48, and it kept 2.2% of the crop
               instead of 35%. But a red label is (255,170,26), minchan 26, so
               this misses coloured text entirely.

      NEUTRAL_HI  The same channel with a far higher floor, for hazed daylight.
               Sunlit atmosphere is itself at the white point: on a Pyro 5a
               frame the haze measured V=255, R=255, min(R,G,B)~200, so all
               three locators below kept ~98% of the label crop. That makes the
               region one giant component and the size filter deletes the text
               with it - four plainly legible labels were found by nothing at
               all. Label glyphs are pure white, so a floor above the haze
               isolates them.

      RED      Hostile contacts render red - measured at (251,12,13), where
               minchan is 12, so NEUTRAL contributes literally zero pixels and
               red survives on BRIGHTNESS alone. That left red labels on bright
               backgrounds falling through both, and red is the colour that
               matters most. The red channel separates (251,12,13) from a cyan
               nebula (100,220,255) by 151.

    They are returned separately rather than unioned on purpose. Unioning would
    reconnect the text to the background it merges with in another mask, and
    the size filter would discard both together - which is exactly the bug this
    fixes. Boxes are found per mask and merged afterwards.
    """
    hsv = cv2.cvtColor(bgr, cv2.COLOR_BGR2HSV)
    v = hsv[:, :, 2]
    b, g, r = bgr[:, :, 0], bgr[:, :, 1], bgr[:, :, 2]
    minchan = np.minimum(np.minimum(r, g), b)
    # Every size threshold below is expressed at REFERENCE_HEIGHT and scaled by
    # the frame height, so a CROP must be told the height of the screen it came
    # from. Without that, a 600px-tall centre crop scales GLYPH_MAX_H from 26 to
    # 11 and discards every real 12-14px glyph box - detection silently returns
    # nothing on an image where the label is plainly present.
    h = frame_h or bgr.shape[0]
    masks = [
        _drop_oversized((v >= LOCATE_V).astype(np.uint8), h),
        _drop_oversized((minchan >= NEUTRAL_MIN).astype(np.uint8), h),
        _drop_oversized((r >= RED_MIN).astype(np.uint8), h),
    ]
    if HAZE_LOCATOR_ENABLED:
        masks.append(_drop_oversized((minchan >= NEUTRAL_HI).astype(np.uint8), h))
    return masks


def hud_mask(bgr: np.ndarray) -> np.ndarray:
    """Locate HUD text by brightness, then discard blobs too big to be glyphs.

    Colour cannot be used. Contact labels render red, white, cyan or magenta
    depending on contact state, and cockpit HUDs differ by manufacturer - a
    hue-based mask misses whole ships. Brightness works because labels are
    drawn at the white point.

    But the world does reach the white point too: sunlit ice hits 255. When a
    label sits against it, the horizontal dilation in find_boxes bridges text
    to rock, and the merged component is over a hundred pixels tall, so the
    size filter throws the text away with it. A real contact went undetected
    that way while plainly on screen.

    Dropping components taller than a glyph BEFORE that dilation fixes it: an
    asteroid is hundreds of pixels tall, a glyph is 11-16, so the rock goes and
    the text stays. It also cuts mask noise about tenfold on bright scenes,
    which makes everything downstream cheaper.
    """
    v = cv2.cvtColor(bgr, cv2.COLOR_BGR2HSV)[:, :, 2]
    raw = (v >= LOCATE_V).astype(np.uint8)

    limit = _scaled(GLYPH_MAX_H, bgr.shape[0])
    n, lab, stats, _ = cv2.connectedComponentsWithStats(raw, connectivity=8)
    keep = np.zeros(n, np.uint8)
    keep[(stats[:, cv2.CC_STAT_HEIGHT] <= limit) &
         (stats[:, cv2.CC_STAT_WIDTH] <= limit * 6)] = 255
    keep[0] = 0  # background label
    return keep[lab]


def find_all_boxes(bgr: np.ndarray, frame_h: int | None = None) -> list[Box]:
    """Boxes from every locator, merged.

    Each mask is searched separately so a label that merges into the
    background in one still resolves in the other. Overlapping results are
    deduplicated, keeping whichever version carries more ink - that is
    usually the mask that separated the glyphs cleanly.
    """
    # Compute the masks ONCE. An earlier version called hud_masks() again for
    # the trim union below, doubling three connected-component passes over a
    # 7.4M-pixel frame and costing ~2s each time.
    frame_h = frame_h or bgr.shape[0]
    masks = hud_masks(bgr, frame_h)

    found: list[Box] = []
    for mask in masks:
        found += find_boxes(mask, frame_h)

    # Boxes come from the horizontally DILATED mask, so each is looser than the
    # text inside it. Shrinking to the actual ink halves the junk pairs on busy
    # frames (52 -> 30 on sand, 48 -> 24 on a station) with no losses, because
    # tighter boxes fail the pairing geometry that loose ones accidentally pass.
    union = None
    for m in masks:
        union = m if union is None else cv2.bitwise_or(union, m)
    if union is not None:
        trimmed = []
        for b in found:
            sub = union[b.y:b.bottom, b.x:b.x + b.w]
            ys, xs = np.nonzero(sub)
            if len(xs) < 8:
                trimmed.append(b)
                continue
            nb = Box(x=b.x + int(xs.min()), y=b.y + int(ys.min()),
                     w=int(xs.max() - xs.min()) + 1,
                     h=int(ys.max() - ys.min()) + 1, core=b.core)
            trimmed.append(nb if nb.w >= 8 and nb.h >= 5 else b)
        found = trimmed

    found.sort(key=lambda b: -b.core)
    kept: list[Box] = []
    for b in found:
        dup = False
        for k in kept:
            ox = min(b.x + b.w, k.x + k.w) - max(b.x, k.x)
            oy = min(b.bottom, k.bottom) - max(b.y, k.y)
            if ox > 0 and oy > 0 and ox * oy > 0.5 * min(b.w * b.h, k.w * k.h):
                dup = True
                break
        if not dup:
            kept.append(b)
    return kept


def find_boxes(mask: np.ndarray, frame_h: int | None = None) -> list[Box]:
    """Merge glyphs into words horizontally, then take connected components."""
    h = frame_h or mask.shape[0]
    kernel_w = _scaled(DILATE_PX, h) * 2 + 1
    kernel = cv2.getStructuringElement(cv2.MORPH_RECT, (kernel_w, 1))
    merged = cv2.dilate(mask, kernel)

    count, _, stats, _ = cv2.connectedComponentsWithStats(merged, connectivity=8)

    min_h, max_h = _scaled(GLYPH_MIN_H, h), _scaled(GLYPH_MAX_H, h)
    min_w, max_w = _scaled(BOX_MIN_W, h), _scaled(BOX_MAX_W, h)

    boxes: list[Box] = []
    for i in range(1, count):  # 0 is background
        x, y, w, bh, _ = stats[i]
        if not (min_h <= bh <= max_h and min_w <= w <= max_w):
            continue
        # Count real (undilated) mask pixels so wide empty boxes get dropped.
        core = int(np.count_nonzero(mask[y : y + bh, x : x + w]))
        if core < _scaled_area(MIN_CORE_PIXELS, h):
            continue
        boxes.append(Box(int(x), int(y), int(w), int(bh), core))
    return boxes


def pair_boxes(boxes: list[Box], frame_h: int) -> list[Contact]:
    """Match each name box to a range box centred directly beneath it."""
    drift = _scaled(PAIR_MAX_CENTRE_DRIFT, frame_h)
    gap_lo, gap_hi = _scaled(PAIR_MIN_GAP, frame_h), _scaled(PAIR_MAX_GAP, frame_h)
    rng_lo, rng_hi = _scaled(RANGE_MIN_W, frame_h), _scaled(RANGE_MAX_W, frame_h)

    pairs: list[Contact] = []
    for name in boxes:
        # Centre alignment is near-perfect (0-2.5px) when a label is drawn
        # against empty space, which is where the 5px limit came from. But a
        # label partly over a bright object loses the occluded glyphs from its
        # box, which shifts the measured centre - one real contact was rejected
        # at 15.5px. A wider name has proportionally more room to lose, so the
        # allowance scales with it.
        name_drift = max(drift, name.w * DRIFT_FRACTION)
        for rng in boxes:
            if rng is name:
                continue
            if not (rng_lo <= rng.w <= rng_hi):
                continue
            if abs(rng.cx - name.cx) > name_drift:
                continue
            if not (gap_lo <= rng.y - name.bottom <= gap_hi):
                continue
            pairs.append(Contact(name_box=name, range_box=rng))
    return pairs


# OCR is 98% of detection time - 21.6 range boxes a frame at ~290ms each, of
# which only ~4 are real. Rather than REJECT on a shape test (two previous
# gates did that and each quietly cost real readings), rank candidates by how
# much they look like 'N.Nkm' and read the best ones first, capping how many
# get read at all. A real box almost always ranks high; the cap only bites
# when a frame is full of junk, which is exactly when it should.
MAX_OCR_PAIRS = 8
IDEAL_GLYPHS = 4  # '1', '8', 'k', 'm' - the period rarely survives thresholding


def range_shape_score(crop: np.ndarray) -> float:
    """0..1 plausibility that a crop is a range readout. No template, no OCR.

    Measured over 247 real candidates: genuine range boxes segment into 3-7
    glyph components (median 4), junk into 1-2 or many. That alone separates
    88% of real from 77% of junk, which is enough to rank by.
    """
    if crop.size == 0:
        return 0.0
    g = cv2.cvtColor(crop, cv2.COLOR_BGR2GRAY)
    thr = max(60, int(np.percentile(g, 99) * 0.55))
    m = (g >= thr).astype(np.uint8)
    n, _, stats, _ = cv2.connectedComponentsWithStats(m, connectivity=8)
    h = crop.shape[0]
    glyphs = [s for s in stats[1:]
              if s[cv2.CC_STAT_HEIGHT] >= h * 0.35 and s[cv2.CC_STAT_AREA] >= 4]
    if not glyphs:
        return 0.0
    # Closeness to the expected glyph count, softly - 3 and 5 are fine, 1 and
    # 15 are not.
    return 1.0 / (1.0 + abs(len(glyphs) - IDEAL_GLYPHS))


def pair_diagnostics(boxes: list[Box], frame_h: int, limit: int = 14) -> list[str]:
    """Why stacked box pairs were rejected. For --debug.

    Only considers boxes that ARE vertically stacked at a plausible gap, so the
    output is near-misses rather than every combination. Distant contacts show
    longer ranges ('12.3km' vs '1.8km'), which makes the range box wider - if
    the width window is what's dropping them, this says so outright.
    """
    drift = _scaled(PAIR_MAX_CENTRE_DRIFT, frame_h)
    gap_lo, gap_hi = _scaled(PAIR_MIN_GAP, frame_h), _scaled(PAIR_MAX_GAP, frame_h)
    rng_lo, rng_hi = _scaled(RANGE_MIN_W, frame_h), _scaled(RANGE_MAX_W, frame_h)

    out = []
    for name in boxes:
        for rng in boxes:
            if rng is name:
                continue
            gap = rng.y - name.bottom
            if not (gap_lo <= gap <= gap_hi):
                continue  # not stacked at all - not a near miss
            why = None
            if not (rng_lo <= rng.w <= rng_hi):
                why = (f"range box {rng.w}px outside {rng_lo}-{rng_hi}"
                       f"{'  <-- TOO WIDE' if rng.w > rng_hi else ''}")
            elif abs(rng.cx - name.cx) > drift:
                why = f"centres off by {abs(rng.cx - name.cx):.0f}px > {drift}"
            if why:
                out.append(f"@{name.x},{name.y} {name.w}x{name.h} "
                           f"over {rng.w}x{rng.h}: {why}")
            if len(out) >= limit:
                return out
    return out


def prep_for_ocr(crop: np.ndarray) -> np.ndarray:
    """Turn a HUD crop into something a document-OCR model will actually read.

    Three steps, in order of how much each one matters:

      margin    By far the biggest factor. Without a wide white border the
                recogniser refuses to commit to glyphs near the edge and drops
                them - 'KEPSS' reads as 'EPSS', 'JD-2266-FC' as 'JD-2266-F'.
                Measured optimum is ~16 native px, i.e. scaled by the upscale.
      upscale   Native glyphs are 11-14px; these models want 30+.
      invert    They are trained on dark text on light paper, not bright HUD
                text on a dark scene.

    Deliberately NOT masked. An earlier version segmented the glyphs first,
    which helped when the game's chromatic aberration and bloom were bridging
    characters together. With those post-effects off the source is clean, and
    masking now costs accuracy by discarding antialiasing the recogniser uses -
    'SP-9695-PW' reads as '$P-9695-PW' masked, and correctly unmasked.
    """
    g = cv2.cvtColor(crop, cv2.COLOR_BGR2GRAY)
    big = cv2.resize(g, None, fx=OCR_UPSCALE, fy=OCR_UPSCALE,
                     interpolation=cv2.INTER_CUBIC)
    big = cv2.normalize(big, None, 0, 255, cv2.NORM_MINMAX)
    img = 255 - big
    pad = OCR_MARGIN_NATIVE * OCR_UPSCALE
    img = cv2.copyMakeBorder(img, pad, pad, pad, pad, cv2.BORDER_CONSTANT, value=255)
    return cv2.cvtColor(img, cv2.COLOR_GRAY2BGR)


# ---- focused capture ------------------------------------------------------
# Contact labels render just above the ship they belong to, so aiming the
# crosshair at someone puts their label a little above screen centre. Measured
# over 27 frames captured that way, every label sat within dx -8..+38 px of
# centre and dy -104..-47 px above it.
#
# That is worth a mode of its own, because the binding constraint on reading a
# specific person is MAX_OCR_PAIRS. On the full frame 26 of those 27 frames had
# more candidate pairs than the cap, so the contact competed with junk; on a
# centre crop none of them did. Measured on the same frames:
#
#   region        pairs/frame   frames over the cap   s/frame   players read
#   full frame        16.7           26/27              1.58        17
#   1200x500           1.3            1/27              1.04        17
#   1000x450           0.9            0/27              0.90        17
#   600x300            0.0            0/27              -           17
#
# No losses at any size, ~40% faster, and the cap stops mattering at all -
# cap 8 and cap 40 give identical results because there is nothing to cap.
# 1200x500 is the default rather than the smallest that worked: the margin is
# for the contact drifting off centre, not for the detector.
FOCUS_W, FOCUS_H = 1200, 600
# Share of the crop height ABOVE centre. Labels sit above the ship, so the
# window is biased upward - but it still has to reach far enough BELOW the
# lowest name for that name's RANGE line, or the pair cannot form. At 500x0.65
# the window ended at y=895 and HOLYSPEAR's range box ran to 904: the name was
# read, the range was clipped off, and the contact was lost. Measured on the
# 48-frame corpus:
#
#   window          in-window   missed   note
#   1200x500 0.65      43          2     HOLYSPEAR range clipped
#   1200x600 0.54      43          0     no PLAYER misses
#   1200x700 0.46      46          6     bigger window admits harder edge cases
#
# 600/0.54 keeps the same 325px above centre and takes the space below from
# 175 to 275. Going further is not free: at 700 the window pulls in contacts
# near its own edges that it then fails to read, so the miss count rises even
# though nothing about the centre changed.
FOCUS_UP = 0.54
FOCUS_MAX_PAIRS = 40


def focus_crop(frame: np.ndarray, w: int = FOCUS_W, h: int = FOCUS_H,
               up: float = FOCUS_UP) -> tuple[np.ndarray, int, int]:
    """Centre crop for a focused read. Returns (crop, x_offset, y_offset).

    The offsets matter: every Box the detector returns is in crop coordinates,
    and the announcer tracks contacts by screen position, so they have to be
    translated back or a focused read looks like a different contact from the
    same one seen on a full frame.
    """
    H, W = frame.shape[:2]
    w, h = min(w, W), min(h, H)
    x0 = max(0, min(W - w, W // 2 - w // 2))
    y0 = max(0, min(H - h, H // 2 - int(h * up)))
    return frame[y0:y0 + h, x0:x0 + w], x0, y0


# Focused reads hand the crop straight to the recogniser and pair its own
# detected lines, instead of running our masks over it. On a 1200x500 crop
# RapidOCR's text detector locates the labels at least as well as three
# brightness locators do, and skipping them removes the whole ranking-and-cap
# problem: there is nothing to rank, so a contact cannot be competed away.
#
# Measured over 13 hand-verified frames: 9/10 recall, ZERO false positives,
# 681 ms/frame against ~900-1040 for the mask path over the same crop. The one
# miss was a range line clipped by the crop's bottom edge, not a read failure.
#
# This is deliberately NOT used for whole-screen reads. Full-frame direct OCR
# costs 4.3-9.6 s and returns up to 114 text lines, against ~1.5 s for the mask
# pipeline - the crop is the entire reason this works.
FOCUS_DIRECT_OCR = True


# A name whose range line the recogniser did not detect gets one targeted
# retry: crop directly beneath it and read that with prep_for_ocr, which is
# tuned for exactly this - a tiny, dim, pre-located box. TREKMASTER's
# '5.2km [-8m/s]' is read at conf 0.86 as a name but its range line is dimmer
# and RapidOCR's detector misses it entirely; the retry recovers it from all
# three crop windows tried.
#
# Bounded, and only for lines that already look like a name, because each retry
# is a full OCR call. Junk lines are cheap to exclude: DECOY has '48' beneath
# it and NOISE has '10', neither of which can match RANGE_RE.
RANGE_RETRY_MAX = 3
# ...and only near the middle of the crop. Confidence does not separate these -
# 'DECOY' reads at 0.83, higher than most real handles - but POSITION does,
# absolutely. Measured across six frames:
#
#   real contact names   |dx| from crop centre = 4, 5, 8, 14 px
#   cockpit furniture    326-534 px  (DECOY, NOISE, SCM, GUN, ESP, CPLD, NAV)
#
# That is not a coincidence to be tuned around: focused mode's whole premise is
# that the contact you are aiming at is near the centre, so a retry for anything
# else is work spent on the ship's own instruments. Without this the pass fired
# 5-8 times a frame and took a focused read from 0.7s to 2.5s.
RANGE_RETRY_CENTRE_FRAC = 0.12


def _retry_range(sub: np.ndarray, name: "OcrLine", ocr, frame_h: int):
    """Re-read the strip directly beneath an unpaired name. None if no range."""
    gap = _scaled(PAIR_MIN_GAP, frame_h)
    pad_x = max(12, name.w // 6)
    y0 = max(0, name.bottom + gap - _scaled(6, frame_h))
    y1 = min(sub.shape[0], name.bottom + gap + name.h + _scaled(10, frame_h))
    x0 = max(0, name.x - pad_x)
    x1 = min(sub.shape[1], name.x + name.w + pad_x)
    if y1 - y0 < 6 or x1 - x0 < 10:
        return None
    txt = ocr(prep_for_ocr(sub[y0:y1, x0:x1])).replace(" ", "").upper()
    m = RANGE_RE.match(txt)
    if not m:
        return None
    value = float(m.group(1))
    km = value if m.group(2).upper() == "KM" else value / 1000.0
    return km, Box(x0, y0, x1 - x0, y1 - y0, 0)


# The recogniser's own line boxes hug the text, and it will not commit to a
# glyph at the very edge of one - the same margin problem prep_for_ocr exists
# to solve, showing up one level higher. Two failure modes, both measured on
# 'HARLEY-DAVIDSON / 7.9km':
#
#   '.9km'  leading digit lost  -> fails RANGE_RE, the CONTACT is discarded
#   '99km'  decimal point lost  -> passes, as 99.0 km instead of 9.9
#
# The second is worse than the first: a silently wrong range is a contact
# reported ten times further away than it is. Re-reading the line from a
# slightly padded crop with prep_for_ocr recovers both ('7.9km' and '9.9km')
# and leaves already-correct lines alone. Pad 4-8 native px works; 14+ becomes
# unstable again, which is the same margin curve as ocr_sweep found.
RANGE_REREAD_PAD = 6


def _range_of(text: str):
    m = RANGE_RE.match(text.replace(" ", "").upper())
    if not m:
        return None
    value = float(m.group(1))
    return value if m.group(2).upper() == "KM" else value / 1000.0


def _looks_rangey(text: str) -> bool:
    """Worth spending an OCR call to re-read as a range."""
    t = text.replace(" ", "").upper()
    return "KM" in t or RANGE_RE.match(t) is not None


def _resolve_range(line: "OcrLine", sub, ocr, frame_h: int):
    """The range this line states, re-read when the reading looks truncated.

    Suspicious means: RANGE_RE rejected it, or it matched with NO decimal
    point. Every genuine km range observed carries one decimal ('1.8km',
    '40.5km', '112.3km'), so a bare '9km' is far more likely to be '7.9km' or
    '9.9km' with a character dropped than a real nine-kilometre contact.
    """
    direct = _range_of(line.text)
    m = RANGE_RE.match(line.text.replace(" ", "").upper())
    suspicious = m is None or "." not in m.group(1)
    if not suspicious or sub is None or ocr is None:
        return direct
    pad = _scaled(RANGE_REREAD_PAD, frame_h)
    y0 = max(0, line.y - pad // 2)
    y1 = min(sub.shape[0], line.bottom + pad // 2)
    x0 = max(0, line.x - pad)
    x1 = min(sub.shape[1], line.x + line.w + pad)
    if y1 - y0 < 6 or x1 - x0 < 10:
        return direct
    again = _range_of(ocr(prep_for_ocr(sub[y0:y1, x0:x1])))
    return again if again is not None else direct


def _pair_ocr_lines(lines: list["OcrLine"], frame_h: int,
                    sub=None, ocr=None) -> list["Contact"]:
    """Apply the contact geometry to the recogniser's own lines.

    Direct OCR finds every handle, but it also returns DECOY, NOISE, GUN and
    SCM, all of which pass HANDLE_RE - so the same rule that rejects them in
    the mask pipeline is still what makes this safe: a name is only a contact
    if an 'N.Nkm' range sits directly beneath it, centre-aligned.

    Pairing is decided on GEOMETRY first and the range resolved afterwards.
    Requiring RANGE_RE up front meant a truncated '.9km' never formed a pair at
    all, so its contact could not be recovered by any later re-read.
    """
    gap_hi = _scaled(PAIR_MAX_GAP, frame_h) + _scaled(6, frame_h)
    out: list[Contact] = []
    for rng in lines:
        if not _looks_rangey(rng.text):
            continue
        # Find the names first, and only spend a re-read if one of them
        # actually sits above this line. Resolving the range up front meant
        # paying an OCR call for every 'km'-looking line on screen, which took
        # a focused read from 1.0s to 2.7s for nothing.
        names = []
        for name in lines:
            if name is rng:
                continue
            gap = rng.y - name.bottom
            # OCR line boxes hug the glyphs more tightly than dilated mask
            # boxes do, so the gap can come out slightly negative when the two
            # boxes touch. Allowing that costs nothing: the centre-alignment
            # test is what does the rejecting.
            if not (-_scaled(6, frame_h) <= gap <= gap_hi):
                continue
            if abs(name.cx - rng.cx) > max(_scaled(8, frame_h), name.w * 0.35):
                continue
            names.append(name)
        if not names:
            continue
        km = _resolve_range(rng, sub, ocr, frame_h)
        if km is None:
            continue
        for name in names:
            kind, cleaned = classify_name(name.text)
            c = Contact(name_box=Box(name.x, name.y, name.w, name.h, 0),
                        range_box=Box(rng.x, rng.y, rng.w, rng.h, 0))
            c.range_km = km
            c.kind, c.name = kind, cleaned
            c.source_line = name
            out.append(c)
    return out


def detect_focused(frame: np.ndarray, ocr, w: int = FOCUS_W, h: int = FOCUS_H,
                   max_pairs: int = FOCUS_MAX_PAIRS,
                   direct: bool | None = None) -> list["Contact"]:
    """Read the centre of the screen. Returns full-frame coordinates."""
    sub, x0, y0 = focus_crop(frame, w, h)
    use_direct = FOCUS_DIRECT_OCR if direct is None else direct

    if use_direct and hasattr(ocr, "lines"):
        # Plain grey, no inversion or upscaling: measured joint-best for
        # accuracy and fastest of five preparations tried (627 ms vs 984 for
        # inverted 2x). The recogniser's own detector prefers the untouched
        # image; the heavy preparation in prep_for_ocr exists for tiny
        # pre-cropped boxes, which is a different problem.
        grey = cv2.cvtColor(cv2.cvtColor(sub, cv2.COLOR_BGR2GRAY),
                            cv2.COLOR_GRAY2BGR)
        lines = ocr.lines(grey)
        found = _pair_ocr_lines(lines, frame.shape[0], sub, ocr)

        # Second pass for names the recogniser found but whose range line it
        # did not detect. Without this a perfectly-read handle is discarded for
        # want of the line beneath it.
        paired = {id(c.source_line) for c in found if c.source_line is not None}
        retry = []
        for ln in lines:
            if id(ln) in paired:
                continue
            if abs(ln.cx - sub.shape[1] / 2) > RANGE_RETRY_CENTRE_FRAC * sub.shape[1]:
                continue
            kind, cleaned = classify_name(ln.text)
            if kind in ("player", "ship_serial") and cleaned:
                retry.append(ln)
        retry.sort(key=lambda l: abs(l.cx - sub.shape[1] / 2))
        for ln in retry[:RANGE_RETRY_MAX]:
            got = _retry_range(sub, ln, ocr, frame.shape[0])
            if got is None:
                continue
            km, rbox = got
            kind, cleaned = classify_name(ln.text)
            c = Contact(name_box=Box(ln.x, ln.y, ln.w, ln.h, 0), range_box=rbox)
            c.range_km, c.kind, c.name = km, kind, cleaned
            found.append(c)
    else:
        found = detect(sub, ocr, frame_h=frame.shape[0], max_pairs=max_pairs)

    for c in found:
        for box in (c.name_box, c.range_box):
            if box is not None:
                box.x += x0
                box.y += y0
    found.sort(key=lambda c: c.range_km or 1e9)
    return found


# A name box that found no range partner is not necessarily junk. The range
# line can be DIMMER than the name above it, and the brightness locator is one
# global threshold - so a label whose name sits at V=255 and whose range sits at
# V=237 loses only half of itself. Measured on a 3440x1440 frame from another
# player: 'FC-1559-ZO' passed with 131 lit pixels, '5.9km' twelve pixels below
# it kept 11, under the core-pixel floor, and the whole contact was discarded.
#
# Lowering LOCATE_V is NOT the fix. Measured on eight corpus frames, dropping it
# from 245 to 230 lost four player detections and removed no junk - a lower
# floor keeps more pixels, components merge, and the size filter deletes the
# merged blob. The README records the same mechanism for hazed daylight.
#
# So the relaxation is LOCAL instead: for a name that looks like a handle and
# has no partner, crop the narrow band where a range line would have to be and
# OCR it directly, with no mask involved at all. Bounded three ways - only
# unpaired boxes, only ones that classify as a name, and a hard cap per frame -
# because OCR is 98% of detection time and this adds one call per retry.
# OFF by default, and that is a measurement rather than caution. On the 5120
# display this was developed against it found nothing extra and added 2.2s to a
# 3.3s frame - 67% more OCR for zero contacts, because on that display the
# range line is as bright as the name and the locator never loses it. On a
# 3440x1440 frame from another player it recovered a contact for +0.6s.
#
# So it is a knob for the symptom: names you can plainly see that are never
# reported. `dim_range_retry` in Settings, 0 = off.
ORPHAN_RETRY_MAX = 0

# How many retries the setting buys when it is switched ON. Two, because two is
# what the evidence supports: across seven frames - four from the ultrawide
# display that has the problem and three from the corpus - a cap of 2 recovered
# everything a cap of 8 did, and each extra slot costs roughly 0.7s of OCR
# whether or not it finds anything.
#
# That is why this is a checkbox rather than a number. A number invites tuning
# a value that measurably does not matter, and hides the cost behind a knob
# nobody has data for. If contacts are still missed with it on, the useful next
# step is a frame to look at, not a bigger number.
ORPHAN_RETRY_WHEN_ON = 2

# Rank only, never reject. The km template gate was calibrated on one display
# and does not transfer: on the ultrawide frames it scored a genuine '2.0KM,'
# at 0.000 while scoring chat text at 0.55. It still ranks the good ones first
# more often than not - two of three real bands came top - so it earns its
# place as an ordering signal and nothing more. A floor would throw away the
# exact case this pass exists for.


def _orphan_band(frame, name, frame_h):
    """The rectangle where this name's range line would have to be."""
    gap = _scaled(PAIR_MIN_GAP, frame_h)
    pad_x = max(12, name.w // 6)
    y0 = max(0, name.bottom + gap - _scaled(6, frame_h))
    y1 = min(frame.shape[0], name.bottom + gap + name.h + _scaled(10, frame_h))
    x0 = max(0, name.x - pad_x)
    x1 = min(frame.shape[1], name.x + name.w + pad_x)
    if y1 - y0 < 6 or x1 - x0 < 10:
        return None
    return x0, y0, x1, y1


def _orphan_names(boxes, paired, frame, frame_h, template):
    """Unpaired boxes, ranked by how much the strip BENEATH them looks like a
    range readout.

    Ranking by the box's own pixel density was the obvious idea and the wrong
    one: dense boxes are cockpit readouts - solid, bright, permanently on
    screen - while a contact label is sparse text on black. Measured on a real
    miss, 'FC-1559-ZO' ranked 10th of 14 and never got looked at.

    The km template gate is the same discriminator detect() already uses to
    spend its OCR budget, it costs microseconds, and it answers the question
    that actually matters: is there a range line under this thing?
    """
    used = {id(c.name_box) for c in paired} | {id(c.range_box) for c in paired}
    min_w = _scaled(BOX_MIN_W, frame_h) * 2
    lo, hi = _scaled(GLYPH_MIN_H, frame_h), _scaled(GLYPH_MAX_H, frame_h)
    scored = []
    for b in boxes:
        if id(b) in used or b.w < min_w or not (lo <= b.h <= hi):
            continue
        band = _orphan_band(frame, b, frame_h)
        if band is None:
            continue
        x0, y0, x1, y1 = band
        crop = frame[y0:y1, x0:x1]
        score = (km_gate.score(crop, template)
                 if template is not None and crop.size else 0.0)
        scored.append((score, b))
    scored.sort(key=lambda t: -t[0])
    return [b for _, b in scored]


def detect(frame: np.ndarray, ocr, frame_h: int | None = None,
           max_pairs: int | None = None) -> list[Contact]:
    """Full pipeline. `ocr` takes an image and returns a string (or '')."""
    frame_h = frame_h or frame.shape[0]
    cap = MAX_OCR_PAIRS if max_pairs is None else max_pairs
    boxes = find_all_boxes(frame, frame_h)
    confirmed: list[Contact] = []

    template = _km_template()

    # Rank by shape before spending any OCR, then read only the best few.
    candidates = pair_boxes(boxes, frame_h)
    if len(candidates) > cap:
        scored = [(range_shape_score(c.range_box.crop(frame, CROP_PAD)), i, c)
                  for i, c in enumerate(candidates)]
        scored.sort(key=lambda t: (-t[0], t[1]))
        candidates = [c for _, _, c in scored[:cap]]

    for cand in candidates:
        range_crop = cand.range_box.crop(frame, CROP_PAD)

        # Cheap gate first. A broad brightness locator yields 15-20 candidate
        # pairs per frame and most are terrain speckle or cockpit furniture;
        # OCR'ing them all costs ~1.8s a frame. Every real range box ends in
        # 'km' in a fixed font, so template-matching those two glyphs rejects
        # the junk for microseconds. Measured on 156 boxes: junk peaks at
        # 0.368, real boxes bottom out at 0.499.
        if KM_GATE_ENABLED and template is not None:
            if km_gate.score(range_crop, template) < KM_GATE_THRESHOLD:
                continue

        # Now the expensive read. Confirming N.Nkm also rejects the stacked,
        # centred cockpit readouts (DECOY/NOISE, H-FUEL/Q-FUEL, 100%/AB) that
        # match the pairing geometry but aren't contacts.
        text = ocr(prep_for_ocr(range_crop))
        m = RANGE_RE.match(text.replace(" ", "").upper())
        if not m:
            continue
        value = float(m.group(1))
        cand.range_km = value if m.group(2).upper() == "KM" else value / 1000.0

        # Only now is it worth OCR'ing the name.
        raw = ocr(prep_for_ocr(cand.name_box.crop(frame, CROP_PAD)))
        cand.kind, cand.name = classify_name(raw)
        confirmed.append(cand)

    # Second pass for names the mask found but whose range line it did not.
    # Skipped entirely when off: ranking the orphans means scoring a km
    # template against every unpaired band, which is ~120 crops on a busy
    # frame. Cheap per crop, not free, and pointless if nothing will be
    # read afterwards.
    orphans = (_orphan_names(boxes, confirmed, frame, frame_h, template)
               if ORPHAN_RETRY_MAX else [])
    for name in orphans[:ORPHAN_RETRY_MAX]:
        # The BAND first, then the name - the same order detect() uses above,
        # for the same reason. Most candidates are not contacts, and finding
        # that out from the band costs one OCR call instead of two.
        got = _retry_range_box(frame, name, ocr, frame_h)
        if got is None:
            continue
        raw = ocr(prep_for_ocr(name.crop(frame, CROP_PAD)))
        kind, cleaned = classify_name(raw)
        if kind not in ("player", "ship_serial") or not cleaned:
            continue
        km, rbox = got
        confirmed.append(Contact(name_box=name, range_box=rbox,
                                 range_km=km, kind=kind, name=cleaned))

    confirmed.sort(key=lambda c: c.range_km or 1e9)
    return confirmed


def _retry_range_box(frame: np.ndarray, name: "Box", ocr, frame_h: int):
    """OCR the strip directly beneath an unpaired NAME BOX. None if no range.

    The box-based twin of _retry_range(), which does the same job for the
    focused path's OcrLine. Same geometry, same reason: the range has to be in
    a narrow band under the name, so read that band rather than trusting a
    brightness mask that has already been shown to lose it.
    """
    gap = _scaled(PAIR_MIN_GAP, frame_h)
    pad_x = max(12, name.w // 6)
    y0 = max(0, name.bottom + gap - _scaled(6, frame_h))
    y1 = min(frame.shape[0], name.bottom + gap + name.h + _scaled(10, frame_h))
    x0 = max(0, name.x - pad_x)
    x1 = min(frame.shape[1], name.x + name.w + pad_x)
    if y1 - y0 < 6 or x1 - x0 < 10:
        return None
    txt = ocr(prep_for_ocr(frame[y0:y1, x0:x1])).replace(" ", "").upper()
    m = RANGE_RE.match(txt)
    if not m:
        return None
    value = float(m.group(1))
    km = value if m.group(2).upper() == "KM" else value / 1000.0
    return km, Box(x0, y0, x1 - x0, y1 - y0, 0)


# --------------------------------------------------------------------------
# OCR backends
# --------------------------------------------------------------------------


@dataclass
class OcrLine:
    """One text line as the recogniser found it, in image coordinates."""
    x: int
    y: int
    w: int
    h: int
    text: str
    conf: float

    @property
    def cx(self) -> float:
        return self.x + self.w / 2.0

    @property
    def bottom(self) -> int:
        return self.y + self.h


# The bundled recogniser is ch_PP-OCRv3, whose output layer has 6625 classes -
# a full Chinese dictionary. This tool reads a fixed all-caps Latin HUD font, so
# every one of those classes but ~97 is dead weight, and the model does
# occasionally reach for them: a CJK glyph turned up in a real session and
# crashed an analysis script on a cp1252 console.
#
# An English PP-OCRv3 recogniser (97 classes) was tried and REJECTED - see
# models/rejected/. It is neither faster nor more accurate; the mechanism below
# is kept because swapping the recogniser is a one-file experiment, and the
# next model worth trying should not need it rebuilt.
#
# Drop a model at REC_MODEL to use it; absent, the bundled one is used and
# nothing changes.
REC_MODEL = paths.resource("models", "en_PP-OCRv3_rec_infer.onnx")
REC_KEYS = paths.resource("models", "en_dict.txt")


def _ocr_kwargs() -> dict:
    """RapidOCR overrides, only for files that actually exist."""
    kw: dict = {}
    if REC_MODEL.exists():
        kw["rec_model_path"] = str(REC_MODEL)
        if REC_KEYS.exists():
            kw["rec_keys_path"] = str(REC_KEYS)
    return kw


def ocr_backend_info() -> str:
    """What the recogniser is actually running, for the startup banner."""
    return ("en_PP-OCRv3 (Latin)" if REC_MODEL.exists()
            else "ch_PP-OCRv3 (bundled, 6625-class)")


def make_ocr():
    """RapidOCR if available, else a stub that reports nothing.

    The returned callable reads one crop and gives back a string, which is what
    the mask pipeline wants: it has already decided where the text is and only
    needs it read.

    It also carries `.lines(img)`, which returns the recogniser's OWN detected
    lines with their boxes. That is what the focused path uses - on a small
    crop RapidOCR's text detector does the locating better than our brightness
    masks, so there is no reason to do that work twice. Attached to the same
    callable rather than exposed as a second factory so both paths share one
    engine; constructing a second RapidOCR doubles start-up and memory for
    nothing.
    """
    try:
        from rapidocr_onnxruntime import RapidOCR
    except ImportError:
        print("! rapidocr-onnxruntime not installed - running detection only", file=sys.stderr)
        stub = lambda img: ""
        stub.lines = lambda img: []
        return stub

    engine = RapidOCR(**_ocr_kwargs())

    # Angle classification decides whether a line is upside down. HUD text
    # never is, so it is 72ms/frame of nothing. Measured over five focused
    # crops: 578 -> 527 ms/frame with byte-identical output, 77 lines either
    # way.
    engine.use_angle_cls = False

    def run(img: np.ndarray) -> str:
        result, _ = engine(img)
        if not result:
            return ""
        return "".join(line[1] for line in result)

    def lines(img: np.ndarray) -> list[OcrLine]:
        result, _ = engine(img)
        out: list[OcrLine] = []
        for box, text, conf in (result or []):
            xs = [p[0] for p in box]
            ys = [p[1] for p in box]
            out.append(OcrLine(x=int(min(xs)), y=int(min(ys)),
                               w=int(max(xs) - min(xs)), h=int(max(ys) - min(ys)),
                               text=text, conf=float(conf)))
        return out

    run.lines = lines
    return run


# --------------------------------------------------------------------------
# Test harness: run against saved frames and dump what it found.
# --------------------------------------------------------------------------


def main(argv: list[str]) -> int:
    if len(argv) < 2:
        print("usage: python sc_detector.py <frame.png> [more.png ...]")
        print("  (convert NVIDIA .jxr captures first - see jxr2png.ps1)")
        return 1

    ocr = make_ocr()
    outdir = paths.data_dir("detections")
    outdir.mkdir(exist_ok=True)

    for path in argv[1:]:
        frame = cv2.imread(path, cv2.IMREAD_COLOR)
        if frame is None:
            print(f"{path}: could not read")
            continue

        contacts = detect(frame, ocr)
        print(f"\n{Path(path).name}  [{frame.shape[1]}x{frame.shape[0]}]")
        if not contacts:
            print("  no contacts")
            continue

        for i, c in enumerate(contacts):
            label = c.name or "<unreadable>"
            print(f"  {label:<26} {c.range_km:>6.1f} km   @ {c.name_box.x},{c.name_box.y}")
            # Save every name crop: this is the false-positive audit trail and
            # doubles as the training corpus for a custom glyph model later.
            stem = Path(path).stem
            safe = "".join(ch if ch.isalnum() or ch in "-_" else "_" for ch in label)
            cv2.imwrite(str(outdir / f"{stem}_{i:02d}_{safe}.png"),
                        prep_for_ocr(c.name_box.crop(frame, CROP_PAD)))

    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
