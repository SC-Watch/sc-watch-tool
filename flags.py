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
The reasons you can file a report under, and what each one means.

KEY vs LABEL, AND WHY THEY ARE NOT THE SAME THING
-------------------------------------------------
Every report row in the database stores a KEY - `camping`, `friendly`, and so
on. The key is fixed forever the moment a flag is created. The LABEL is what
you see, and you can rewrite it whenever you like.

That split is the whole reason renaming is safe. If reports stored the label,
renaming "Camping" to "Spawn camping" would leave every old row pointing at a
reason that no longer exists - the history would still be there, but nothing
could say what it meant. With a stable key, the rename is a display change and
three years of reports follow it automatically.

Keys are therefore never deleted either. Clearing a box RETIRES that flag: it
leaves the dropdown, and it stays in this file so the reports filed under it
still render with their proper name and colour. A flag you stop using is not
a flag you never used.

POLARITY IS NOT DECORATION
--------------------------
`Assessment.score` used to read:

    if r.category == "note":   continue
    total += -r.weight if r.category == "friendly" else r.weight

- two hard-coded strings, and everything else counted AGAINST the player. That
works until somebody makes their own flag. A custom "Good trader" would have
raised a threat score and tripped the alert, and clearing the `friendly` box
would have silently flipped every positive report ever filed into a negative
one.

So polarity is stored per flag and the score asks for it:

    negative   counts against  (+weight)   the default for a griefer log
    positive   counts for      (-weight)   cancels out negatives, e.g. friendly
    neutral    does not score  (0)         an observation, e.g. note

An unknown key - one in the database that this file has never heard of, which
should not happen - is treated as NEGATIVE, because that is what the old
hard-coded rule did and a silent change of meaning is worse than a wrong one
you can see.

BUILT FOR SPEECH LATER
----------------------
Each flag carries `say`, an optional pronunciation used in place of the label
when the alert is spoken. Nothing reads it yet. It is here now because adding
it later would mean migrating everybody's flags file, and because a label is
written to be READ - "KOS!!!", "1v1 griefer" - while TTS needs something that
can be SPOKEN. flags.speakable(key) is the one call the audio path will need.
"""

from __future__ import annotations

import re
from dataclasses import asdict, dataclass
from pathlib import Path

import jsonstore
import paths

CONFIG_PATH = paths.data("flags.json")

# How many editable rows the settings panel shows. Twelve rather than the
# eight first sketched: seven slots arrive already filled with the built-in
# reasons, so eight would leave exactly one free and the feature is about
# having room for your own.
SLOTS = 12

POLARITIES = ("negative", "positive", "neutral")
_SIGN = {"negative": 1.0, "positive": -1.0, "neutral": 0.0}

# Not editable, not in the dropdown, never retired. 'pending' is the one-button
# "something happened, I will explain when I land" flow; resolve_pending()
# replaces it, and the header counts it. Renaming it would break all three.
RESERVED = ("pending",)

MAX_LABEL = 32
MAX_SAY = 48
_KEY_RE = re.compile(r"^[a-z0-9_]{1,40}$")
_COLOR_RE = re.compile(r"^#[0-9a-fA-F]{6}$")


@dataclass
class Flag:
    key: str
    label: str
    color: str = "#8fa6bb"
    polarity: str = "negative"
    say: str = ""            # spoken instead of the label; "" means use label
    retired: bool = False

    @property
    def sign(self) -> float:
        return _SIGN.get(self.polarity, 1.0)

    @property
    def spoken(self) -> str:
        return (self.say or self.label).strip()


# The reasons this tool shipped with. Their keys are the strings already
# written into every existing database, so nothing migrates - the defaults ARE
# the old values, given labels and colours.
DEFAULTS = [
    Flag("unprovoked_attack", "Unprovoked attack", "#ff5f56", "negative"),
    Flag("camping", "Camping a location", "#ff9f43", "negative"),
    Flag("ransom_broken", "Broke a ransom", "#ffd166", "negative"),
    Flag("harassment", "Harassment", "#ff6b9d", "negative"),
    Flag("cheating_suspected", "Suspected cheating", "#c77dff", "negative"),
    Flag("friendly", "Friendly", "#4ade80", "positive"),
    Flag("note", "Note", "#8fa6bb", "neutral"),
]

# Reported to the user when an unknown key turns up, rather than a blank chip.
UNKNOWN_COLOR = "#6b7a8c"


def _coerce(d: dict) -> Flag | None:
    key = str(d.get("key", "")).strip().lower()
    if not _KEY_RE.match(key) or key in RESERVED:
        return None
    label = str(d.get("label", "")).strip()[:MAX_LABEL]
    color = str(d.get("color", "")).strip()
    if not _COLOR_RE.match(color):
        color = UNKNOWN_COLOR
    pol = str(d.get("polarity", "negative")).strip().lower()
    if pol not in POLARITIES:
        pol = "negative"
    return Flag(key=key, label=label, color=color, polarity=pol,
                say=str(d.get("say", "")).strip()[:MAX_SAY],
                retired=bool(d.get("retired")) or not label)


# Cached on (path, mtime, size). score() asks for a flag's polarity once per
# report, and the UI recomputes every tile on every poll - re-reading and
# re-parsing a JSON file thousands of times a minute for a file that changes
# when somebody edits a text box would be absurd. Keyed on mtime AND size
# because a same-second edit can leave mtime unchanged on some filesystems.
_cache: dict = {}

# Don't even stat() more than this often. Measured: a bare mtime check costs
# ~0.038 ms, which is nothing once and 38 ms across the thousand lookups one
# UI poll makes over 200 tiles. The file only changes when a person edits a
# text box, so a second of staleness is invisible - and save() clears the
# cache outright, so an edit made HERE is never stale at all.
_STAT_TTL = 1.0
_checked: dict = {}


def _cache_key(path: Path):
    try:
        st = path.stat()
        return (str(path), st.st_mtime_ns, st.st_size)
    except OSError:
        return (str(path), 0, 0)


def invalidate():
    """Drop the cache. Called after save(); exposed for tests."""
    _cache.clear()
    _checked.clear()


def load(path: Path | None = None) -> list[Flag]:
    """Every flag ever defined, retired ones included.

    A missing or unreadable file is the defaults - the same rule settings.py
    uses, and for the same reason: refusing to start over a bad config file is
    worse than starting with known-good values.
    """
    import time
    path = Path(path) if path else CONFIG_PATH
    sp = str(path)
    seen_at, cached = _checked.get(sp, (0.0, None))
    now = time.monotonic()
    if cached is not None and now - seen_at < _STAT_TTL:
        return cached
    ck = _cache_key(path)
    hit = _cache.get(ck)
    if hit is None:
        hit = _load_uncached(path)
        _cache.clear()      # one entry is enough; a stale key is just churn
        _cache[ck] = hit
    _checked[sp] = (now, hit)
    return hit


def _load_uncached(path: Path) -> list[Flag]:
    return _from_raw(jsonstore.read(path, expect=list))


def _from_raw(raw) -> list[Flag]:
    if raw is None:
        return [Flag(**asdict(f)) for f in DEFAULTS]
    out, seen = [], set()
    for d in raw:
        if not isinstance(d, dict):
            continue
        f = _coerce(d)
        if f and f.key not in seen:
            seen.add(f.key)
            out.append(f)
    # A default the file has never heard of is appended rather than dropped,
    # so an upgrade that adds a built-in reason reaches existing installs.
    for d in DEFAULTS:
        if d.key not in seen:
            out.append(Flag(**asdict(d)))
    return out or [Flag(**asdict(f)) for f in DEFAULTS]


def save(items: list[Flag] | list[dict], path: Path | None = None) -> list[Flag]:
    """Write the list, KEEPING every key that already existed.

    Keys are never dropped, only retired. Somebody clearing a box means "stop
    offering me this", not "erase the reports I filed under it" - and those
    reports still need a name and a colour to render with.
    """
    path = Path(path) if path else CONFIG_PATH
    written: list[Flag] = []

    def apply(raw):
        written[:] = _merge(items, {f.key: f for f in _from_raw(raw)})
        return [asdict(f) for f in written]

    # One locked read-merge-write, straight from the file rather than the
    # cache: the merge must see what is on disk NOW, or a save could retire
    # a key another save had just added.
    jsonstore.update(path, apply, expect=list)
    invalidate()
    return written


def _merge(items, previous: dict) -> list[Flag]:
    incoming: list[Flag] = []
    for it in items or []:
        f = _coerce(it if isinstance(it, dict) else asdict(it))
        if not f:
            continue
        # A cleared box retires the flag, and a retired flag KEEPS THE NAME IT
        # HAD. Without this the reports filed under it render with a blank
        # chip, which is the exact failure retiring was supposed to avoid -
        # the history survives but nothing can say what it meant.
        if not f.label:
            was = previous.get(f.key)
            if was and was.label:
                f.label = was.label
                if f.color == UNKNOWN_COLOR:
                    f.color = was.color
                f.polarity = was.polarity
                f.say = f.say or was.say
            f.retired = True
        incoming.append(f)
    have = {f.key for f in incoming}
    for old in previous.values():
        if old.key not in have:
            old.retired = True
            incoming.append(old)
    # Written by save() through jsonstore: atomically, and with a copy of any
    # file that could not be read. Before that, an unreadable flags.json
    # loaded as the defaults and the next save wrote them over it - every
    # custom reason gone for good, and the reports filed under them left
    # rendering with no name.
    return incoming


# ---- lookup -------------------------------------------------------------

def all_flags(path: Path | None = None) -> list[Flag]:
    return load(path)


def active(path: Path | None = None) -> list[Flag]:
    """The flags to offer in the dropdown: defined, labelled, not retired."""
    return [f for f in load(path) if not f.retired and f.label]


def by_key(key: str, path: Path | None = None) -> Flag:
    """Always returns something renderable, even for a key we do not know."""
    k = (key or "").strip().lower()
    for f in load(path):
        if f.key == k:
            return f
    if k in RESERVED:
        return Flag(k, "Awaiting reason", "#7aa2f7", "negative")
    return Flag(k or "unknown", k or "unknown", UNKNOWN_COLOR, "negative",
                retired=True)


def sign(key: str, path: Path | None = None) -> float:
    """+1 counts against, -1 counts for, 0 does not score."""
    if (key or "").strip().lower() in RESERVED:
        # A pending flag scores like a negative on purpose: you pressed the
        # button because something happened, so it should alert before it has
        # a name.
        return 1.0
    return by_key(key, path).sign


def speakable(key: str, path: Path | None = None) -> str:
    """What TTS should say for this flag - its "spoken as", else its label.
    Used by reputation.Assessment.spoken() for the {label} of a spoken alert."""
    return by_key(key, path).spoken


def keys(path: Path | None = None) -> list[str]:
    """Every key that may appear in the database, reserved ones included."""
    return [f.key for f in load(path)] + list(RESERVED)
