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
One place for every knob, and one file on disk that holds them.

Settings used to live in four places that did not know about each other: the
argparse defaults in watch.py, housekeeping.json, audio.json, and constants in
sc_detector.py. Changing the burst size meant editing a .bat file; turning off
debug frames meant remembering that the launcher passes --debug. This module is
the schema for all of it, so the UI can render a form without knowing what any
particular setting means, and watch.py can take its defaults from the same place
the UI writes to.

Design notes:

  - The schema is DATA, not code. Adding a setting is one Field entry; the UI
    picks it up with no changes, because it renders from the schema rather than
    from a hand-written form.

  - CLI still wins. watch.py seeds argparse defaults from here, so an explicit
    flag overrides the saved value for that run without writing it back. A
    one-off `--burst 5` should not silently become the new default.

  - Values are CLAMPED on load, not trusted. settings.json is a plain file a
    person can edit, and a negative burst size or a 400 Hz poll rate would
    either crash the watcher or peg a core. Out-of-range values are pulled back
    to the nearest legal one rather than rejected - a bad file should cost you
    that one setting, not stop the tool starting.

  - `restart` records who has to be restarted before a change takes effect. The
    watcher reads its settings once at startup, so telling the user "this needs
    a watcher restart" is more honest than silently doing nothing until they
    next relaunch.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path
from typing import Any
import paths

CONFIG_PATH = paths.data("settings.json")


@dataclass(frozen=True)
class Field:
    key: str
    label: str
    type: str                     # bool | int | float | str | choice
    default: Any
    help: str = ""
    choices: tuple = ()
    lo: float | None = None
    hi: float | None = None
    unit: str = ""
    # Who needs restarting before this takes effect: "watcher", "ui", or ""
    # for settings that are read fresh every time they are used.
    restart: str = ""
    advanced: bool = False

    def clamp(self, v):
        """Coerce a value to this field's type and range.

        Returns the default when a value cannot be coerced at all, so one
        corrupt entry costs that setting rather than the whole file.
        """
        try:
            if self.type == "bool":
                if isinstance(v, str):
                    t = v.strip().lower()
                    # A numeric string has to agree with the number it spells.
                    # dim_range_retry used to be an int, so an upgraded config
                    # can hold 4 or "4", and reading one as on and the other as
                    # off would be a setting that changes meaning depending on
                    # how it was written.
                    try:
                        return float(t) != 0.0
                    except ValueError:
                        return t in ("true", "yes", "on", "y")
                return bool(v)
            if self.type in ("int", "float"):
                # Infinity and NaN reach here two ways: json.load accepts the
                # literals Infinity and NaN, and float("1e999") overflows to
                # inf. Neither can be clamped - int(inf) raises OverflowError,
                # and every comparison against NaN is False, so max()/min()
                # let it through and something downstream gets a NaN instead.
                # A settings.json holding one used to stop BOTH processes from
                # starting, which is the opposite of what this function
                # promises.
                v = float(v)
                if v != v or v in (float("inf"), float("-inf")):
                    return self.default
                if self.type == "int":
                    v = int(round(v))
            elif self.type == "choice":
                v = str(v)
                return v if v in self.choices else self.default
            else:
                return str(v)
        except (TypeError, ValueError, OverflowError):
            return self.default
        if self.lo is not None:
            v = max(self.lo, v)
        if self.hi is not None:
            v = min(self.hi, v)
        return int(v) if self.type == "int" else v


@dataclass(frozen=True)
class Category:
    key: str
    label: str
    blurb: str
    fields: tuple


SCHEMA = (
    Category(
        "capture", "Capture",
        "What starts a read, and how much of the screen it looks at.",
        (
            Field("trigger", "Trigger", "choice", "key",
                  "'key' reads when you press the key. 'auto' reads on a timer.",
                  choices=("key", "auto"), restart="watcher"),
            Field("key", "Read key", "str", "0",
                  "Full-screen read. A single character, or a name like f9.",
                  restart="watcher"),
            Field("focus_key", "Focused read key", "str", "9",
                  "Reads a box around the centre of the screen, more "
                  "thoroughly. Use it when a name sits just above your "
                  "crosshair.", restart="watcher"),
            Field("chat_key", "Chat read key", "str", "off",
                  "Reads the chat window and lists everyone talking. Chat "
                  "handles are crisp UI text with the original capitalisation, "
                  "so they are more reliable than a HUD label - measured on the "
                  "reference frame, four of four resolved to real accounts. "
                  "'off' disables it.", restart="watcher"),
            Field("chat_region", "Chat window region", "str",
                  "0.15,0.30,0.35,0.45",
                  "Where the chat window sits, as x,y,width,height fractions "
                  "of the screen. Chat grows upward as messages arrive, so a "
                  "region covering only the newest line reads one name instead "
                  "of five.", restart="watcher", advanced=True),
            Field("focus_size", "Focused box size", "str", "1200x600",
                  "Pixels, width x height, centred on the crosshair.",
                  restart="watcher"),
            Field("focus_always", "Always use focused mode", "bool", False,
                  "Treat the normal read key as a focused read too.",
                  restart="watcher"),
            Field("focus_mask", "Mask focused reads", "bool", False,
                  "Run the brightness mask on focused reads instead of "
                  "sending the crop straight to OCR. Slower, and only "
                  "occasionally better.", restart="watcher", advanced=True),
            Field("burst", "Frames per read", "int", 3,
                  "More frames means more chances to agree on a name, and a "
                  "longer wait for the last word.",
                  lo=1, hi=10, restart="watcher"),
            Field("burst_spacing", "Gap between frames", "float", 0.5,
                  "Wider gives more independent samples to vote with; too "
                  "wide and contacts drift or fade between frames.",
                  lo=0.0, hi=5.0, unit="s", restart="watcher"),
            Field("ping_delay", "Delay before first frame", "float", 0.0,
                  "Waits after the key press before grabbing, to let a scan "
                  "animation finish. Every second cut here is a second off "
                  "your answer.",
                  lo=0.0, hi=10.0, unit="s", restart="watcher"),
            Field("monitor", "Monitor", "int", 1,
                  "1-based index of the screen the game is on.",
                  lo=1, hi=8, restart="watcher"),
            Field("hz", "Key poll rate", "float", 5.0,
                  "How often the trigger key is checked.",
                  lo=1.0, hi=30.0, unit="Hz", restart="watcher",
                  advanced=True),
            Field("queue_max", "Queued reads", "int", 3,
                  "How many key presses may stack up while a read is already "
                  "running.", lo=1, hi=10, restart="watcher", advanced=True),
        )),

    Category(
        "detection", "Detection",
        "How hard the reader tries, and what it is willing to believe.",
        (
            Field("min_votes", "Votes to announce", "int", 1,
                  "How many frames of a burst must agree before a name is "
                  "said out loud. 1 announces fast and is sometimes wrong.",
                  lo=1, hi=10, restart="watcher"),
            Field("show_unknown", "Show unreadable contacts", "bool", False,
                  "An UNKNOWN could be an asteroid or a hostile.",
                  restart="watcher"),
            Field("gate", "Range gate", "bool", False,
                  "Require a range line to look like a range before trusting "
                  "the pair.", restart="watcher", advanced=True),
            Field("dim_range_retry", "Re-read dim range lines", "bool", False,
                  "Turn this on if you can plainly see contact names that are "
                  "never reported. On some displays the range line under a "
                  "name renders dimmer than the name, so the name is found and "
                  "the contact is thrown away for having no range. This "
                  "re-reads the strip underneath. It costs roughly 1.5s per "
                  "read and finds nothing at all on a display that does not "
                  "have the problem, which is why it is off by default.",
                  restart="watcher"),
            Field("haze", "Haze locator", "bool", False,
                  "Adds a fourth, brighter locator pass. Helps against a "
                  "bright planet surface; finds more junk in space.",
                  restart="watcher"),
            Field("cooldown", "Re-announce after", "float", 300.0,
                  "Too short and a wingman re-announces all session; too long "
                  "and you miss a genuine return.",
                  lo=0.0, hi=3600.0, unit="s", restart="watcher"),
            Field("radius", "Pairing radius", "float", 180.0,
                  "How far below a name its range line may sit.",
                  lo=20.0, hi=600.0, unit="px", restart="watcher",
                  advanced=True),
            Field("expire", "Contact expiry", "float", 20.0,
                  "How long a contact stays 'current' after its last "
                  "sighting.", lo=1.0, hi=600.0, unit="s", restart="watcher",
                  advanced=True),
        )),

    Category(
        "audio", "Audio",
        "What you hear when something is found.",
        (
            Field("audio_mode", "Audio", "choice", "both",
                  "Tones are fast and unambiguous; speech tells you the name.",
                  choices=("off", "tones", "speech", "both")),
            Field("volume", "Volume", "float", 0.9, "", lo=0.0, hi=1.0),
            Field("rate", "Speech rate", "int", 175, "Words per minute.",
                  lo=80, hi=400),
            Field("speak_handles", "Say handles aloud", "bool", True,
                  "Off gives you the warning without the name."),
            Field("max_spoken_per_burst", "Handles spoken per read", "int", 2,
                  "A crowded screen otherwise talks over the fight.",
                  lo=1, hi=10),
            Field("summarise_above", "Summarise above", "int", 3,
                  "More contacts than this and you get a count instead of "
                  "names.", lo=1, hi=20),
        )),

    Category(
        "rsi", "RSI lookups",
        "When to ask the API who a handle belongs to.",
        (
            Field("rsi_enabled", "Look handles up", "bool", True,
                  "Fetches org and enlistment data from starcitizen-api.com.",
                  restart="watcher"),
            Field("rsi_min_votes", "Votes before lookup", "int", 2,
                  "A misread handle wastes a request and pollutes the "
                  "database, so weak reads are not looked up automatically. "
                  "Confirming an audit by hand always looks up, whatever this "
                  "says.", lo=1, hi=10, restart="watcher"),
            Field("rsi_mode", "API mode", "choice", "auto",
                  "'auto' uses the cache where it can. 'live' is always fresh "
                  "and slower. Org rosters force 'live' regardless.",
                  choices=("auto", "live", "cache"), restart="watcher",
                  advanced=True),
        )),

    Category(
        "audit", "Audit",
        "Keeping the picture behind a read, so you can check it by eye.",
        (
            Field("audit_enabled", "Save uncertain reads", "bool", True,
                  "When a read is not corroborated, keep the crops that "
                  "produced it."),
            Field("audit_max_age_days", "Keep audits for", "float", 7.0,
                  "Unreviewed audits older than this are deleted along with "
                  "their images. Confirming or dismissing one deletes it "
                  "immediately.", lo=0.5, hi=365.0, unit="days"),
            Field("audit_keep_frame", "Keep the full frame", "bool", True,
                  "The whole screen, not just the crops, so you can see what "
                  "the contact actually was. Around 0.6 MB each."),
            Field("audit_frame_quality", "Frame quality", "int", 90,
                  "JPEG quality for that full frame. Lower is smaller.",
                  lo=40, hi=100, advanced=True),
        )),

    Category(
        "storage", "Storage & cleanup",
        "Captured frames are big. This is what stops them piling up.",
        (
            Field("debug_frames", "Save every frame (debug)", "bool", False,
                  "Writes every burst frame to debug_bursts/ so a missed "
                  "contact can be diagnosed afterwards. Around 4.7 MB per "
                  "frame - this is the setting that fills a disk."),
            Field("housekeeping_enabled", "Delete old frames", "bool", True,
                  "Master switch for the cleanup below."),
            Field("housekeeping_on_start", "Clean up at startup", "bool", True,
                  "Run a sweep when the watcher starts.", restart="watcher"),
            Field("housekeeping_every_min", "Clean up every", "float", 30.0,
                  "Time between sweeps while the watcher runs. 0 means only "
                  "at startup.", lo=0.0, hi=1440.0, unit="min",
                  restart="watcher"),
            Field("max_age_hours", "Delete frames older than", "float", 24.0,
                  "", lo=0.5, hi=8760.0, unit="h"),
            Field("keep_newest", "Always keep newest", "int", 20,
                  "Never delete below this many per directory, however old.",
                  lo=0, hi=2000),
            Field("max_files_per_dir", "Hard file cap", "int", 0,
                  "Per directory, whatever the age. 0 turns the cap off. "
                  "Useful when a long session would fill the disk before the "
                  "age rule ever applies.",
                  lo=0, hi=100000, advanced=True),
        )),

    Category(
        "interface", "Interface",
        "The web UI itself.",
        (
            Field("port", "Port", "int", 8731, "", lo=1024, hi=65535,
                  restart="ui"),
            Field("open_browser", "Open a browser on start", "bool", True,
                  "", restart="ui"),
            Field("max_contacts", "Contacts shown", "int", 120,
                  "How many of the most recent contacts the grid draws. "
                  "Measured cost is about 0.2 ms per tile to render and 0.4 ms "
                  "to assemble, and only when the data actually changes - so "
                  "this is a readability control, not a performance one. Lower "
                  "it if the board is too busy to scan.",
                  lo=10, hi=500),
            Field("tile_size", "Tile size", "int", 380,
                  "Minimum width of a contact tile. The grid fits as many as "
                  "will go, so a bigger number means fewer, larger tiles - at "
                  "2560 wide, 380 gives six across and 500 gives four. Text "
                  "scales with the tile.",
                  lo=260, hi=700, unit="px"),
            Field("tile_square", "Square tiles", "bool", True,
                  "Keeps every tile the same shape so rows line up. Tiles "
                  "still grow taller when a contact has more to show."),
            Field("show_advanced", "Show advanced settings", "bool", False,
                  "Reveals the settings that are easiest to get wrong."),
        )),
)

FIELDS = {f.key: f for cat in SCHEMA for f in cat.fields}
DEFAULTS = {k: f.default for k, f in FIELDS.items()}


def load(path: Path | None = None) -> dict:
    """Every setting, defaults filled in and values clamped.

    Never raises. A missing file is the default set; a corrupt one is reported
    on stderr and treated as missing, because failing to start over a bad
    config file is worse than starting with known-good values.
    """
    path = path or CONFIG_PATH
    raw = {}
    if path.exists():
        try:
            raw = json.loads(path.read_text(encoding="utf-8")) or {}
        except Exception as exc:
            print(f"settings: ignoring {path.name} ({exc}); using defaults")
            raw = {}
    out = dict(DEFAULTS)
    for k, v in (raw.items() if isinstance(raw, dict) else ()):
        if k in FIELDS:
            try:
                out[k] = FIELDS[k].clamp(v)
            except Exception as exc:      # clamp is meant to swallow these
                print(f"settings: {k}={v!r} unusable ({exc}); "
                      f"using {FIELDS[k].default!r}")
    return out


def save(values: dict, path: Path | None = None) -> dict:
    """Clamp, merge over what is already saved, and write. Returns the result.

    Merges rather than replaces so a partial update from the UI - one category,
    one field - cannot blank the settings it did not send.
    """
    path = path or CONFIG_PATH
    cur = load(path)
    for k, v in (values or {}).items():
        if k in FIELDS:
            cur[k] = FIELDS[k].clamp(v)
    tmp = path.with_suffix(".json.tmp")
    # allow_nan=False: json.dumps happily writes Infinity and NaN, which are
    # not JSON and which nothing else should have to cope with. clamp() already
    # rejects them, so this only fires if that ever stops being true.
    tmp.write_text(json.dumps(cur, indent=2, allow_nan=False), encoding="utf-8")
    tmp.replace(path)           # atomic: never leave a half-written config
    return cur


def describe() -> list[dict]:
    """The schema as plain JSON, for the UI to render a form from."""
    return [{
        "key": cat.key, "label": cat.label, "blurb": cat.blurb,
        "fields": [{
            "key": f.key, "label": f.label, "type": f.type,
            "default": f.default, "help": f.help, "choices": list(f.choices),
            "lo": f.lo, "hi": f.hi, "unit": f.unit, "restart": f.restart,
            "advanced": f.advanced,
        } for f in cat.fields],
    } for cat in SCHEMA]


def main() -> int:
    import argparse
    ap = argparse.ArgumentParser(description="show or change saved settings")
    ap.add_argument("--set", nargs=2, metavar=("KEY", "VALUE"), action="append",
                    help="set a value (repeatable)")
    ap.add_argument("--reset", action="store_true", help="restore defaults")
    args = ap.parse_args()

    if args.reset:
        save({k: v for k, v in DEFAULTS.items()})
        CONFIG_PATH.write_text(json.dumps(DEFAULTS, indent=2), encoding="utf-8")
        print(f"reset {CONFIG_PATH.name}")
    if args.set:
        bad = [k for k, _ in args.set if k not in FIELDS]
        if bad:
            print(f"unknown setting(s): {', '.join(bad)}")
            return 2
        save(dict(args.set))
        print(f"saved {CONFIG_PATH.name}")

    cur = load()
    for cat in SCHEMA:
        print(f"\n[{cat.label}]")
        for f in cat.fields:
            v = cur[f.key]
            mark = "" if v == f.default else "  *"
            print(f"  {f.key:24} {str(v):>12}{mark}")
    print("\n* = changed from default")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
