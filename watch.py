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
Live monitor: watch the screen, announce contacts as they appear.

    python watch.py

The loop is cheap-always, expensive-rarely:

  MONITOR   Every frame, run locate -> boxes -> pair -> km gate. That is ~36ms,
            so at 5 Hz it costs about 18% of one core and nothing on the GPU.
            The gate matters here: it drops cockpit furniture (DECOY/NOISE,
            H-FUEL, fuel gauges) before they ever reach the tracker, so only
            genuine contacts are tracked.

  TRIGGER   Contacts are tracked by screen position. Anything already tracked
            is ignored. When something appears that isn't, that's new - and
            only then do we spend OCR.

  BURST     Read the triggering frame plus a couple more and vote. Per-frame
            OCR is 92-100% accurate and every error seen so far is a single
            occurrence, so three frames is plenty to outvote them.

Nothing here touches the game. Frames come from the desktop compositor, and
no key is bound, hooked or sent - the trigger is purely visual.

Ctrl+C to stop; it prints a session summary.
"""

from __future__ import annotations

import argparse
import csv
import os
import queue
import shutil
import sys
import threading
import time
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

import cv2

import chat
import consensus
import housekeeping
import inputs
import km_gate
import sc_detector as sd
import settings as settings_mod
import paths


# settings.json key -> argparse dest. Only the ones the watcher acts on; the
# rest of the schema belongs to the UI or to housekeeping.
#
# Seeded through ap.set_defaults() BEFORE parsing, which keeps the CLI
# authoritative: an explicit --burst 5 still wins, and does not get written
# back. A flag you typed once should not silently become the new default.
_SETTING_TO_ARG = {
    "trigger": "trigger", "key": "key", "focus_key": "focus_key",
    "focus_size": "focus_size", "focus_always": "focus_always",
    "focus_mask": "focus_mask", "burst": "burst",
    "burst_spacing": "burst_spacing", "ping_delay": "ping_delay",
    "monitor": "monitor", "hz": "hz", "queue_max": "queue_max",
    "min_votes": "min_votes",
    "gate": "gate", "haze": "haze", "cooldown": "cooldown",
    "radius": "radius", "expire": "expire", "audio_mode": "audio",
    "rsi_min_votes": "rsi_min_votes", "audit_enabled": "audit",
    "debug_frames": "debug", "chat_key": "chat_key",
    "chat_region": "chat_region", "dual_key": "dual_key",
    "dual_on_read": "dual_on_read",
}


RESTART_FLAG = paths.data("restart.flag")


def _restart_requested() -> bool:
    """True once, when the UI has asked for a restart.

    The flag is deleted before returning True, so a failed restart cannot put
    the watcher into a loop of restarting forever. Stale flags left by a crash
    are consumed the same way on the next start.
    """
    try:
        if not RESTART_FLAG.exists():
            return False
        RESTART_FLAG.unlink(missing_ok=True)
        return True
    except OSError:
        return False


def _waiting_label(binds) -> str:
    """A compact 'waiting for ...' when bindings share a modifier chain.

    Three four-token bindings spelled out in full is 120 characters of mostly
    repetition. Bindings that share tokens - which is the normal case, since a
    HOTAS modifier is held for all of them - are shown once as a common prefix
    with only the differing part per mode.
    """
    if not binds:
        return "nothing"
    sets = [set(b.tokens) for b in binds.values()]
    common = set.intersection(*sets) if len(sets) > 1 else set()
    parts = []
    for mode, b in binds.items():
        rest = [t.label() for t in b.tokens if t not in common]
        parts.append(f"{mode}:{'+'.join(rest) or b.label()}")
    prefix = "+".join(sorted(t.label() for t in common))
    return (f"{prefix}+(" + " ".join(parts) + ")") if prefix else " ".join(parts)


def _defaults_from_settings() -> dict:
    """argparse defaults taken from settings.json.

    `no_rsi` is inverted rather than mapped: the flag is negative and the
    setting is positive, and storing the negative sense in the file would make
    the settings UI read backwards.
    """
    cur = settings_mod.load()
    out = {arg: cur[key] for key, arg in _SETTING_TO_ARG.items() if key in cur}
    out["no_rsi"] = not cur.get("rsi_enabled", True)
    return out


@dataclass
class Track:
    """A contact we've already spent OCR on, held by screen position."""
    cx: float
    cy: float
    last_seen: float
    name: str | None = None


class Tracker:
    """Decides what's new.

    Position matching only has to be good enough to avoid re-triggering on a
    contact that's drifting across the screen. Getting it slightly wrong is
    cheap in both directions: too tight re-reads something we already know
    (wasted OCR, caught later by the announce cooldown), too loose delays a
    genuinely new contact by one cycle.
    """

    def __init__(self, radius: float, expire: float):
        self.radius = radius
        self.expire = expire
        self.tracks: list[Track] = []

    def update(self, points: list[tuple[float, float]], now: float) -> int:
        self.tracks = [t for t in self.tracks if now - t.last_seen < self.expire]
        new = 0
        for cx, cy in points:
            hit = None
            best = self.radius
            for t in self.tracks:
                d = ((t.cx - cx) ** 2 + (t.cy - cy) ** 2) ** 0.5
                if d < best:
                    best, hit = d, t
            if hit is None:
                self.tracks.append(Track(cx, cy, now))
                new += 1
            else:
                hit.cx, hit.cy, hit.last_seen = cx, cy, now
        return new


class Announcer:
    """Duplicate protection across bursts.

    Matching on the exact string is not enough. One burst reads 'HORNE' and the
    next reads 'HORNET65' - the same ship, announced twice, and the first
    announcement shows the wrong name. So compare with the same rule consensus
    uses within a burst: confusion-weighted distance plus a prefix check.

    When a later burst produces a longer reading of a contact we already
    announced, that is a correction rather than a repeat - say it, and adopt
    the better name as canonical.
    """

    def __init__(self, cooldown: float):
        self.cooldown = cooldown
        self.last: dict[str, float] = {}
        self.counts: dict[str, int] = {}

    def _match(self, name: str, now: float) -> str | None:
        for known in self.last:
            if consensus.same_contact(name, known):
                return known
        return None

    def should(self, name: str, now: float) -> tuple[bool, str | None]:
        """Returns (announce?, corrected_from)."""
        known = self._match(name, now)
        if known is None:
            self.last[name] = now
            self.counts[name] = 1
            return True, None

        self.counts[known] = self.counts.get(known, 0) + 1
        self.last[known] = now

        # A longer reading of the same contact supersedes the shorter one.
        if len(name) > len(known) and name.upper().startswith(known.upper()):
            self.last[name] = self.last.pop(known)
            self.counts[name] = self.counts.pop(known)
            return True, known

        return False, None


class Console:
    """Announcements scroll; a one-line status sits below them.

    Without this the console is silent whenever nothing new appears, which is
    indistinguishable from the thing having hung - and a burst blocks the loop
    for ~2.7s with no indication why. The status line is redrawn in place, so
    announcements still scroll cleanly above it.

    Disabled when output isn't a terminal, since carriage returns just make a
    mess of a redirected log.
    """

    def __init__(self):
        self.live = sys.stdout.isatty()
        self.width = 0
        self.last_status = ""
        # RSI enrichment arrives on a background thread, so writes have to be
        # serialised or its lines interleave with the main loop's.
        self._lock = threading.Lock()

    def _clear(self):
        if self.live and self.width:
            sys.stdout.write("\r" + " " * self.width + "\r")
            self.width = 0

    def status(self, text: str):
        """Redraw the one-line status in place.

        CLAMPED to the console width, and that is not cosmetic. The line is
        rewritten with a carriage return, which only returns to the start of
        the CURRENT line - so a status longer than the terminal
        wraps, the carriage return lands
        on the second row, and every redraw marches one line down the screen.
        It looks exactly like the tool spamming. It happened the moment the
        line started naming three four-part joystick bindings: 137 characters
        into an 80-column window.
        """
        if not self.live:
            return
        width = shutil.get_terminal_size((100, 24)).columns - 1
        if len(text) > width:
            text = text[:max(0, width - 1)] + "…"
        with self._lock:
            self.last_status = text
            self._clear()
            sys.stdout.write(text)
            sys.stdout.flush()
            self.width = len(text)

    def say(self, text: str):
        with self._lock:
            self._clear()
            print(text)
            # Redraw the status the announcement just wiped out.
            if self.live and self.last_status:
                sys.stdout.write(self.last_status)
                self.width = len(self.last_status)
            sys.stdout.flush()

    def done(self):
        with self._lock:
            self._clear()


class RsiEnricher:
    """Look handles up on RSI without blocking the read loop.

    A profile is 1-3 HTTP requests behind a 1s rate limit, which would add
    seconds to an announcement that already waits 4s for the ping wave. So the
    name is announced immediately and the org detail arrives when it arrives,
    on its own line - the same progressive pattern as the burst itself.

    Everything is cached, so a handle seen twice costs nothing the second time.
    """

    def __init__(self, con, enabled: bool = True, db=None, alerts=None,
                 client=None):
        self.con = con
        self.db = db
        self.alerts = alerts
        self.q: queue.Queue = queue.Queue()
        self.client = None
        self.thread = None
        self.seen: set[str] = set()
        if not enabled or client is None:
            return
        if not client.enriches:
            # Fall back to link mode rather than switching lookup off. A handle
            # with a URL beside it is still useful; silently dropping it is not.
            client.provider = "link"
            print("! no API key - profile enrichment off, handles will be "
                  "reported with a lookup link instead.\n"
                  "  For full org/piracy data: get a key at "
                  "https://starcitizen-api.com and set SC_API_KEY",
                  file=sys.stderr)
        self.client = client
        self.thread = threading.Thread(target=self._run, daemon=True)
        self.thread.start()

    def submit(self, handle: str):
        if self.client is None or handle in self.seen:
            return
        self.seen.add(handle)
        self.q.put(handle)

    def _run(self):
        while True:
            handle = self.q.get()
            if handle is None:
                return
            try:
                c = self.client.citizen(handle)
            except Exception as exc:
                self.con.say(f"          rsi: {handle} lookup failed "
                             f"({type(exc).__name__})")
                continue
            if not c.exists:
                # A handle that doesn't resolve is usually an OCR error, not a
                # ghost - VONCARSTEINO read 18/18 times and the real handle was
                # VONCARSTEIN. Worth saying so rather than silently omitting.
                self.con.say(f"          rsi: {handle} - no profile "
                             f"(likely a misread)")
                continue
            # Lookups return out of order, so the handle has to be on the
            # line - three anonymous "rsi: enlisted 12y ago" lines in a row
            # are unattributable.
            line = f"          rsi: {handle} - {c.summary()}"
            for flag in c.flags:
                line += f"\n          !!   {handle}: {flag}"
            self.con.say(line)

            # Flags arrive after the burst's own alert, so raise a second one
            # rather than staying silent about a warning that only just became
            # known.
            if self.alerts is not None and c.flags:
                self.alerts.event("flagged", handle=handle, flags=c.flags)

            # Remember org membership so an org flag reaches this player later,
            # even if we never look them up again.
            if self.db is not None and c.orgs:
                try:
                    self.db.set_orgs(handle, c.orgs)
                    self.db.update_profile(handle, c.moniker, c.enlisted)
                    a = self.db.assess(handle)
                    if a.flagged_orgs:
                        for sid, note in a.flagged_orgs:
                            self.con.say(f"          ***  {handle}: FLAGGED ORG "
                                         f"{sid}" + (f" - {note}" if note else ""))
                except Exception:
                    pass

    def stop(self):
        if self.thread is not None:
            self.q.put(None)


# Key handling moved to inputs.py, which does the same GetAsyncKeyState polling
# for keys and adds joystick buttons and combinations. The reasoning that shaped
# the original KeyWatcher is preserved there, because it is the reason this is
# safe to run next to the game: polling installs nothing, intercepts nothing and
# swallows nothing, so the trigger still reaches the game.


def cheap_pass(frame, template):
    """Monitor-rate detection: everything except OCR."""
    pairs = sd.pair_boxes(sd.find_boxes(sd.hud_mask(frame)), frame.shape[0])
    if not (sd.KM_GATE_ENABLED and template is not None):
        return pairs
    return [p for p in pairs
            if km_gate.score(p.range_box.crop(frame, sd.CROP_PAD), template)
            >= sd.KM_GATE_THRESHOLD]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--trigger", choices=("key", "auto"), default="key",
                    help="'key' reads a burst after you press the ping key "
                         "(default); 'auto' watches continuously")
    ap.add_argument("--key", default="0",
                    help="key to listen for (default 0). Set to TAB to fire on "
                         "the ping itself; a separate key lets you trigger "
                         "manually once you judge the wave has passed, which "
                         "isolates whether the wave is interfering at all")
    ap.add_argument("--ping-delay", type=float, default=0.0,
                    help="seconds to wait after the keypress before reading "
                         "(default 0). The ping's expanding blue wave hides "
                         "labels as it passes and clears distant targets last, "
                         "so bind --key TAB and set this to ~4 to fire off the "
                         "ping itself. At 0 you trigger manually when the "
                         "screen already looks right")
    ap.add_argument("--burst-spacing", type=float, default=0.5,
                    help="seconds between burst frames. Voting only helps if "
                         "errors are independent, and frames a quarter-second "
                         "apart are near-identical - OCR repeats the same "
                         "mistake and consensus confirms it. Widening costs "
                         "nothing now announcements are progressive "
                         "(default 0.5)")
    ap.add_argument("--debug", action="store_true",
                    help="save every burst frame to debug_bursts/ and report "
                         "what each pipeline stage found")
    ap.add_argument("--focus-key", default="9",
                    help="second ping key that reads only the centre of the "
                         "screen. Contact labels sit just above the ship they "
                         "belong to, so putting the crosshair on someone puts "
                         "their label near centre - and a centre crop has ~1 "
                         "candidate pair instead of ~17, so the OCR cap stops "
                         "competing them away. Use it when you want a read on "
                         "one specific person. 'off' disables it")
    ap.add_argument("--chat-key", default="off",
                    help="read the chat window and list who is talking. Takes "
                         "the same forms as --key, including joystick buttons "
                         "and combinations: 'joy0.b24', 'rctrl+joy0.b24', "
                         "'vid231d:0126.b7'. 'off' disables it")
    ap.add_argument("--chat-region", default="",
                    help="chat window as x,y,w,h fractions of the screen "
                         "(default 0.15,0.30,0.35,0.45). Chat grows upward, so "
                         "a region covering only the newest line reads one "
                         "name instead of five")
    ap.add_argument("--no-audit", dest="audit", action="store_false",
                    help="stop saving crops for low-confidence reads. Audits "
                         "are on by default and cost about 0.7 MB each; they "
                         "age out after a week and vanish the moment you "
                         "confirm one")
    ap.add_argument("--focus-mask", action="store_true",
                    help="make focused reads use the mask pipeline instead of "
                         "handing the crop straight to OCR. The direct path is "
                         "the default: measured on the corpus it matched 42 of "
                         "43 in-window contacts with no player misses, and it "
                         "has no OCR cap to compete a contact away")
    ap.add_argument("--dual-key", default="off",
                    help="key or button that reads the WHOLE screen and the "
                         "centre on one press, merging both. Costs about "
                         "double a normal read. 'off' disables it.")
    ap.add_argument("--dual-on-read", action="store_true",
                    help="make the normal read key do a wide + centre read, "
                         "instead of needing a separate binding")
    ap.add_argument("--focus-always", action="store_true",
                    help="read focused on EVERY trigger, not just the focus "
                         "key. Works under --replay, which the focus key "
                         "cannot, so this is how the focused path gets tested "
                         "and how it would be used for offline mining")
    ap.add_argument("--focus-size", default=f"{sd.FOCUS_W}x{sd.FOCUS_H}",
                    help="focused capture region, WxH, centred horizontally "
                         "and biased upward. Bigger tolerates the contact "
                         "drifting off centre; smaller is faster")
    ap.add_argument("--queue-max", type=int, default=3,
                    help="how many pings can queue up while a burst is being "
                         "read. Rapid pings reveal different contacts, so they "
                         "queue rather than being dropped - but bounded, or "
                         "the tool ends up reading minutes-old screens "
                         "(default 3)")
    ap.add_argument("--no-rsi", action="store_true",
                    help="skip RSI profile lookups")
    ap.add_argument("--where", action="store_true",
                    help="print where the program and your data live, and "
                         "exit. An installed build keeps them apart.")
    # The two diagnostics people actually need, reachable from the packaged
    # build. They used to live only in check_resolution.py and inputs.py, which
    # a source tree has and an installed copy does not - so every troubleshooting
    # instruction written for them was useless to exactly the users most likely
    # to need it.
    ap.add_argument("--list-inputs", action="store_true",
                    help="list the joysticks this machine exposes, with their "
                         "button and axis counts, and exit")
    ap.add_argument("--check-resolution", metavar="IMAGE", nargs="+",
                    help="run a screenshot of your HUD through the reader and "
                         "report what it found and why, then exit. The useful "
                         "thing to attach to a bug report.")
    ap.add_argument("--no-db", action="store_true",
                    help="don't record sightings or check the reputation database")
    ap.add_argument("--audio", choices=("off", "tones", "speech", "both"),
                    help="audible alerts. 'tones' is a distinct sound per "
                         "event; 'speech' reads them out; 'both' uses tones "
                         "for routine contacts and speech only for flagged "
                         "ones, which is the default in audio.json")
    ap.add_argument("--gate", action="store_true",
                    help="enable the km pre-gate. Faster (~0.9s/frame vs 2.5s) "
                         "but measured to drop 28%% of genuine readings on real "
                         "session frames, which thins vote counts")
    ap.add_argument("--haze", action="store_true",
                    help="enable the hazed-daylight locator. Recovers contacts "
                         "over sunlit terrain that all three normal locators "
                         "miss entirely, but adds roughly fourteen garbled "
                         "one-vote name strings per real recovery, which land "
                         "in the database as phantom handles")
    ap.add_argument("--rsi-min-votes", type=int, default=2,
                    help="votes needed before looking a handle up. Higher than "
                         "--min-votes on purpose: reporting a contact on one "
                         "frame's word is fine, but attributing a real "
                         "person's org history to it is not. A truncated read "
                         "can resolve to a genuine different player - 'HORNE' "
                         "is somebody's actual handle (default 2)")
    ap.add_argument("--hz", type=float, default=5.0, help="monitor rate (default 5)")
    ap.add_argument("--burst", type=int, default=3,
                    help="frames to OCR per trigger (default 3)")
    ap.add_argument("--cooldown", type=float, default=300.0,
                    help="seconds before re-announcing the same handle")
    ap.add_argument("--radius", type=float, default=180.0,
                    help="px within which a contact counts as already tracked")
    ap.add_argument("--expire", type=float, default=20.0,
                    help="seconds before a vanished contact counts as new again")
    ap.add_argument("--monitor", type=int, default=1, help="1-based monitor index")
    ap.add_argument("--seconds", type=float, default=0.0,
                    help="stop after N seconds (0 = run until Ctrl+C)")
    ap.add_argument("--replay", metavar="GLOB",
                    help="feed saved frames instead of live capture, e.g. "
                         "'corpus/test2_*.png'. Same code path otherwise.")
    # Default resolved against the data root, not the working directory: a
    # shortcut launches with a cwd nobody chose, and a log written there is a
    # log nobody finds. An explicit --log path is still honoured as given.
    ap.add_argument("--log", default=str(paths.data("sightings.csv")))
    ap.add_argument("--min-votes", type=int, default=1,
                    help="votes needed to announce. 1 reports everything with "
                         "its vote count visible; 2+ suppresses single-frame "
                         "reads, which also loses real contacts when a burst "
                         "is inconsistent (default 1)")
    ap.add_argument("--verbose", action="store_true",
                    help="also report unreadable contacts and suppressed repeats")

    # Negative counterparts for every flag whose saved value may be true.
    # Without these a setting turned on in the UI could not be overridden for
    # one run - and --debug in particular is the setting that fills a disk, so
    # being able to say "not this time" without opening the UI matters.
    for flag, dest in (("--no-debug", "debug"), ("--no-haze", "haze"),
                       ("--no-gate", "gate"),
                       ("--no-focus-always", "focus_always"),
                       ("--no-dual-on-read", "dual_on_read"),
                       ("--no-focus-mask", "focus_mask")):
        ap.add_argument(flag, dest=dest, action="store_false",
                        help=argparse.SUPPRESS)
    ap.add_argument("--rsi", dest="no_rsi", action="store_false",
                    help=argparse.SUPPRESS)

    # Saved settings become the defaults; anything typed on the command line
    # still overrides them.
    ap.set_defaults(**_defaults_from_settings())
    args = ap.parse_args()

    # Answered before anything is opened or captured, so it stays useful when
    # the reason you are asking is that something else will not start.
    if args.where:
        print(paths.describe())
        return 0

    if args.list_inputs:
        devs = inputs.devices()
        print(f"{len(devs)} joystick(s):")
        for d in devs:
            print(f"  {d.describe()}")
        if not devs:
            print("  none. Only keyboard bindings are available.")
        return 0

    if args.check_resolution:
        import check_resolution
        ocr = sd.make_ocr()
        for p in args.check_resolution:
            check_resolution.report(p, ocr)
        return 0

    # Frame source: live capture, or saved frames for testing without the game
    # running. Everything downstream is identical either way.
    exhausted = False
    if args.replay:
        # glob.glob, not Path().glob: the latter raises NotImplementedError on
        # an ABSOLUTE pattern, so --replay "D:/frames/*.png" died with
        # "Non-relative patterns are unsupported" rather than doing the obvious
        # thing. Relative patterns behave identically either way.
        import glob as _glob
        replay_paths = sorted(Path(p) for p in _glob.glob(args.replay))
        if not replay_paths:
            return int(bool(sys.stderr.write(f"no frames match {args.replay}\n")))
        replay_iter = iter(replay_paths)

        def grab():
            nonlocal exhausted
            p = next(replay_iter, None)
            if p is None:
                exhausted = True
                return None
            return cv2.imread(str(p), cv2.IMREAD_COLOR)

        def release():
            pass

        print(f"replaying {len(replay_paths)} frames from {args.replay}")
    else:
        try:
            import dxcam
        except ImportError:
            return int(bool(sys.stderr.write("need dxcam: pip install dxcam\n")))

        cam = dxcam.create(output_idx=args.monitor - 1, output_color="BGR")
        if cam is None:
            return int(bool(sys.stderr.write("dxcam.create failed\n")))

        def grab():
            return cam.grab()

        def release():
            cam.release()

    ocr = sd.make_ocr()
    template = km_gate.load_template()
    if template is None:
        print("! no km_template.npy - monitoring will be slow "
              "(run build_km_template.py)", file=sys.stderr)

    sd.KM_GATE_ENABLED = args.gate
    sd.HAZE_LOCATOR_ENABLED = args.haze
    sd.ORPHAN_RETRY_MAX = (sd.ORPHAN_RETRY_WHEN_ON
                           if settings_mod.load().get("dim_range_retry")
                           else 0)

    # Loaded once, here, because three separate things below read it: the audio
    # overlay, the audit capture and the housekeeping sweep. Re-reading the file
    # per use would let a mid-session edit apply to one and not another.
    _cfg = settings_mod.load()

    try:
        import audio
        # audio.json still owns the tone tables and speech templates - nested
        # structures the settings form has no way to edit. The flat knobs the
        # UI DOES expose are overlaid on top of it here, because otherwise they
        # were written to settings.json and read by nobody: an audit found
        # volume, rate, speak_handles, max_spoken_per_burst and summarise_above
        # all inert, five controls that moved and changed nothing.
        audio_cfg = audio.AudioConfig.load()
        for setting, attr in (("volume", "volume"), ("rate", "rate"),
                              ("speak_handles", "speak_handles"),
                              ("max_spoken_per_burst", "max_spoken_per_burst"),
                              ("summarise_above", "summarise_above")):
            if setting in _cfg:
                setattr(audio_cfg, attr, _cfg[setting])
        if args.audio:
            audio_cfg.mode = args.audio
        alerts = audio.Alerts(audio_cfg)
    except Exception as exc:
        print(f"! audio disabled: {type(exc).__name__}: {exc}", file=sys.stderr)
        alerts = None

    con = Console()

    # One profile client, shared. The database uses it to VERIFY identity
    # before merging two similar-looking handles - string resemblance is not
    # evidence, and SLIVER/5LIVER and HORNE/HORNET65 are each two real people.
    profile_client = None
    if not args.no_rsi:
        try:
            import rsi
            profile_client = rsi.Client(mode=_cfg.get("rsi_mode", "auto"))
        except Exception as exc:
            print(f"! profile lookup disabled: {type(exc).__name__}: {exc}",
                  file=sys.stderr)

    db = None
    if not args.no_db:
        try:
            import reputation
            verify = None
            if profile_client is not None and profile_client.enriches:
                verify = lambda h: profile_client.citizen(h).exists
            db = reputation.Database(verify=verify)
        except Exception as exc:
            print(f"! database disabled: {type(exc).__name__}: {exc}", file=sys.stderr)

    enricher = RsiEnricher(con, enabled=not args.no_rsi, db=db, alerts=alerts,
                           client=profile_client)
    tracker = Tracker(args.radius, args.expire)
    announcer = Announcer(args.cooldown)
    log = Path(args.log)
    new_log = not log.exists()
    logf = log.open("a", newline="", encoding="utf-8")
    writer = csv.writer(logf)
    if new_log:
        writer.writerow(["timestamp", "name", "range_km", "votes", "total", "confidence"])

    key_mode = args.trigger == "key" and not args.replay

    # Three bindings, each of which may be a key, a joystick button, or a
    # combination - `rctrl+joy0.b24` is one binding, not a chord to type.
    # `python inputs.py --listen` prints the string for whatever you press.
    binds: dict[str, inputs.Binding] = {}
    for mode, spec, flag in (("full", args.key, "--key"),
                             ("focus", args.focus_key, "--focus-key"),
                             ("dual", args.dual_key, "--dual-key"),
                             ("chat", args.chat_key, "--chat-key")):
        try:
            b = inputs.Binding(spec)
        except inputs.BindError as exc:
            sys.stderr.write(
                f"{flag} {spec!r}: {exc}\n"
                "  keys:      " + ", ".join(sorted(inputs.VK_CODES)[:18]) + ", ...\n"
                "  joysticks: joy0.b24, vid231d:0126.b7, rctrl+joy0.b24\n"
                "  run `python inputs.py --listen` to find yours\n")
            return 1
        if b:
            binds[mode] = b
    if key_mode and "full" not in binds:
        sys.stderr.write("--key cannot be 'off' - it is the main trigger\n")
        return 1
    # Two bindings on the same input would make one of them unreachable.
    seen_specs = {}
    for mode, b in binds.items():
        sig = frozenset(b.tokens)
        if sig in seen_specs:
            sys.stderr.write(
                f"{mode} and {seen_specs[sig]} are both bound to "
                f"{b.label()}; they have to differ\n")
            return 1
        seen_specs[sig] = mode
    if key_mode:
        for mode, b in binds.items():
            if b.missing:
                sys.stderr.write(
                    f"! {mode} binding {b.label()} needs a device that is not "
                    f"plugged in ({', '.join(b.missing)})\n")
    chat_region = chat.DEFAULT_REGION
    if args.chat_region.strip():
        try:
            parts = [float(v) for v in args.chat_region.split(",")]
            if len(parts) != 4 or not all(0.0 <= v <= 1.0 for v in parts):
                raise ValueError
            chat_region = tuple(parts)
        except ValueError:
            sys.stderr.write(
                f"--chat-region {args.chat_region!r} should be four fractions "
                "between 0 and 1, like 0.15,0.30,0.35,0.45" + chr(10))
            return 1

    try:
        focus_w, focus_h = (int(v) for v in args.focus_size.lower().split("x"))
    except ValueError:
        return int(bool(sys.stderr.write(
            f"--focus-size {args.focus_size!r} should look like 1200x500"
            + chr(10))))

    # Settings clamp hz to 1-30, but --hz comes straight off the command line
    # and 0 divides. A watcher that will not start is worse than a slow one.
    if args.hz <= 0:
        print(f"! --hz {args.hz} is not a rate; using 5 Hz")
        args.hz = 5.0
    interval = 1.0 / args.hz
    print("=" * 62)
    print("  sc-watch")
    if key_mode:
        print(f"  trigger: {binds['full'].label()} pressed, then read after "
              f"{args.ping_delay:.1f}s")
        if "focus" in binds:
            print(f"  focused: {binds['focus'].label()} reads the centre "
                  f"{focus_w}x{focus_h} only")
        if "chat" in binds:
            print(f"  chat:    {binds['chat'].label()} lists who is talking")
    else:
        print(f"  trigger: continuous, {args.hz:.0f} Hz")
    print(f"  {args.burst}-frame bursts, {args.cooldown:.0f}s repeat cooldown")
    print(f"  logging to {log.resolve()}")
    if paths.FROZEN:
        # Only when installed. From source the answer is "here", and a
        # line saying so in every dev run is noise.
        print(f"  your data {paths.DATA_ROOT}")
    # Which recogniser produced this session's readings. Two are possible and
    # they do not agree, so a log without this is hard to compare against the
    # measurements in the README.
    print(f"  reading with {sd.ocr_backend_info()}")
    if args.debug:
        print("  DEBUG: saving every frame to debug_bursts/ (~4.7 MB each)")
    print("  Ctrl+C to stop")
    print("=" * 62)

    # Retention. housekeeping.py existed for three sessions before anything
    # called it, and debug_bursts/ reached 1.1 GB across 234 frames in the
    # meantime - a cleanup nothing runs is not a cleanup. Startup is the right
    # moment: the disk is about to be written to hard, and last session's
    # frames have had their chance to be looked at.
    _hk_last = 0.0

    def _housekeep(force=False):
        nonlocal _hk_last
        every = float(_cfg.get("housekeeping_every_min", 30.0)) * 60.0
        if not _cfg.get("housekeeping_enabled", True):
            return
        if not force and (every <= 0 or time.time() - _hk_last < every):
            return
        _hk_last = time.time()
        line = housekeeping.run_now(apply=True)
        if args.verbose or "freed" in line:
            print(f"  {line}")

    if _cfg.get("housekeeping_on_start", True):
        _housekeep(force=True)

    frames = triggers = announced = 0
    started = time.time()
    stats = {"frames": 0}

    def _maybe_audit(db, r, evidence, seen=0):
        """Save the picture behind a read the tool is not sure of.

        Triggered on VOTES, never on `confidence`: a 1/1 read reports 1.00 and
        is the least trustworthy thing here - a whole session's log had 57 of
        them, several of which were misspellings announced as certain. Audited
        when a read got fewer than two votes, or when the burst disagreed with
        itself.

        Crops are lossless because they may be re-read later; the full frame is
        JPEG because it is only ever looked at by eye, and at 4.9 MB a PNG per
        audit a busy hour would cost gigabytes. Measured: 4.11 MB PNG against
        0.65 MB at q90.
        """
        if r.votes >= 2 and r.votes >= r.total:
            return                      # corroborated and unanimous
        # ...or corroborated by earlier PINGS. Raising an audit for a handle
        # already seen twice asks you to verify something the session has
        # answered, and the answer clear_corroborated_audits() would give is to
        # delete it again immediately. Same threshold, applied to the same
        # quantity, in both directions.
        if seen >= 2:
            return
        mine = [e for e in evidence if e[0] == r.name]
        if not mine:
            return
        _, frame, nb, rb = mine[0]
        out = Path("audit")
        out.mkdir(exist_ok=True)
        stem = f"{datetime.now():%Y%m%d_%H%M%S}_{r.name[:16]}"
        crop_paths = {}
        try:
            for key, box in (("crop_name", nb), ("crop_range", rb)):
                if box is None:
                    continue
                crop = box.crop(frame, 10)
                if crop.size:
                    rel = f"audit/{stem}_{key}.png"
                    cv2.imwrite(rel, crop)
                    crop_paths[key] = rel
            if _cfg.get("audit_keep_frame", True):
                rel = f"audit/{stem}_frame.jpg"
                cv2.imwrite(rel, frame, [cv2.IMWRITE_JPEG_QUALITY,
                                         int(_cfg.get("audit_frame_quality", 90))])
                crop_paths["frame_path"] = rel
        except Exception:
            pass
        db.add_audit(r.name, r.range_km, r.votes, r.total,
                     [(n, c) for n, c in (r.variants or [])], **crop_paths)

    def _ignored(name):
        """Is this a label you have marked as not-a-person?

        Checked in the WATCHER rather than inside classify_name(), because the
        rule lives in the database and the detector is a pure module with no
        database handle. It also means the list can be edited from the UI and
        take effect on the next read, without restarting anything - which is
        the point: you see a crate reported as a contact, you press ignore, it
        stops. Waiting for a restart would make it feel broken.
        """
        if db is None:
            return None
        try:
            return db.is_ignored(name)
        except Exception:
            return None

    def read_chat_now(frame):
        """List everyone the chat window is currently showing.

        A different kind of evidence from a HUD read, and a better one. Chat is
        crisp UI text on a stable background, and it preserves case - measured
        on the reference frame, four of four handles read correctly and all
        four resolved to real RSI accounts. A HUD label is upper-case, often
        half-occluded, and read off a moving ship.

        So these are recorded as full-confidence sightings and looked up
        immediately, rather than waiting for the vote threshold a HUD read has
        to clear. Range is None: chat says who is talking, not where they are,
        and inventing a distance would put a number in the database that no
        measurement stands behind.
        """
        nonlocal announced
        if frame is None:
            con.say("  chat: no frame captured")
            return
        try:
            lines = chat.read(frame, ocr, region=chat_region)
        except Exception as exc:
            con.say(f"  chat: read failed ({type(exc).__name__}: {exc})")
            return
        if not lines:
            con.say("  chat: nothing readable in the chat region "
                     "(check 'chat region' in Settings)")
            if alerts is not None:
                alerts.event("nothing")
            return

        lines = [c for c in lines if not _ignored(c.key)]
        if not lines:
            con.say("  chat: everything readable is on the ignore list")
            return
        con.say(f"  chat: {len(lines)} handle(s)")
        for c in lines:
            note = ""
            if db is not None:
                try:
                    db.record_sighting(c.key, None, 1, 1)
                    a = db.assess(c.key)
                    if a.alert:
                        note = "   " + "; ".join(a.lines()[:2])
                except Exception:
                    pass
            con.say(f"    {c.handle:<24}{note}")
            announced += 1
            # Always looked up: a chat handle is stronger evidence than the
            # vote threshold exists to protect against.
            if not args.no_rsi:
                enricher.submit(c.key)
        if alerts is not None:
            alerts.event("contact")

    def read_burst(first, now, label, on_wait=None, mode="full"):
        """Read a burst progressively: announce after each frame, don't wait
        for the whole thing.

        Serial OCR means a 3-frame burst is ~4.5s of work on top of the 4s
        wave delay. Waiting for all of it puts the first name 9-10s after the
        ping, which is a long time when something is closing on you. Reading
        frame by frame gets a name out at ~5.5s and lets the rest confirm,
        correct or add to it.

        The announcer already deduplicates across bursts and treats a longer
        later reading as a correction, so re-resolving after every frame costs
        nothing extra: contacts already announced stay quiet, and only genuinely
        new or corrected ones speak up.
        """
        nonlocal announced
        # Two promotions of a plain read, in priority order. Asking for both
        # is contradictory - one says "centre only", the other says "wide AND
        # centre" - and dual is the strictly more informative of the two, so
        # it wins rather than silently discarding the periphery.
        if args.dual_on_read and mode == "full":
            mode = "dual"
        elif args.focus_always and mode == "full":
            mode = "focus"
        accumulated, unreadable, per_frame = [], 0, []
        evidence: list = []
        discarded: dict[str, int] = {}
        # Re-resolving after every frame would re-report the same repeats once
        # per frame in verbose mode. Mention each contact at most once a burst.
        mentioned: set[str] = set()

        # CAPTURE THE WHOLE BURST FIRST, then read it.
        #
        # Interleaving grab and OCR - which is what progressive announcement
        # originally did - means each capture waits for the previous frame's
        # ~1.5s of OCR. Frames configured 0.5s apart actually landed ~2s apart,
        # spanning four seconds of game time: contacts drift, ping labels fade,
        # and later frames read something different or nothing at all. That
        # showed up as contacts stuck at 1 vote.
        #
        # Capturing up front costs nothing (a grab is ~2ms) and puts the frames
        # where they were configured to be.
        frames_buf = [f for f in (first,) if f is not None]
        tries = 0
        while len(frames_buf) < args.burst and tries < args.burst * 8:
            tries += 1
            if not args.replay:
                # Watch for further presses while waiting between captures.
                # Rapid pings show different contacts, so they queue rather
                # than being discarded.
                end = time.perf_counter() + args.burst_spacing
                while time.perf_counter() < end:
                    if on_wait:
                        on_wait()
                    time.sleep(0.02)
            extra = grab()
            if extra is not None:
                frames_buf.append(extra)
            elif exhausted:
                break
        con.status(f"  {label} - captured {len(frames_buf)}, reading...")

        for idx, frame in enumerate(frames_buf):
            con.status(f"  {label} - reading frame {idx + 1}/{len(frames_buf)}...")

            if args.debug:
                dbg = paths.data_dir("debug_bursts")
                path = dbg / f"burst_{datetime.now():%H%M%S}_{idx}.png"
                cv2.imwrite(str(path), frame)
                boxes = sd.find_boxes(sd.hud_mask(frame))
                pairs = sd.pair_boxes(boxes, frame.shape[0])
                gated = cheap_pass(frame, template)
                con.say(f"  [debug] frame {idx}: boxes={len(boxes)} "
                        f"pairs={len(pairs)} passed_gate={len(gated)} "
                        f"-> {path.name}")
                for why in sd.pair_diagnostics(boxes, frame.shape[0]):
                    con.say(f"  [debug]   near-miss {why}")
                for p in pairs:
                    s = km_gate.score(p.range_box.crop(frame, sd.CROP_PAD), template) \
                        if template is not None else 1.0
                    if s < sd.KM_GATE_THRESHOLD:
                        con.say(f"  [debug]   gate rejected {p.name_box.w}x"
                                f"{p.name_box.h}@{p.name_box.x},{p.name_box.y} "
                                f"range {p.range_box.w}px score={s:.3f} "
                                f"(need {sd.KM_GATE_THRESHOLD})")

            # A focused read crops to the centre of the screen and lifts the
            # OCR cap. Boxes come back in full-frame coordinates either way, so
            # dedup and position tracking are unaffected.
            def _centre():
                return sd.detect_focused(frame, ocr, focus_w, focus_h,
                                         direct=not args.focus_mask)

            if mode == "dual":
                # Both passes over the SAME frame, then merged. Costs roughly
                # twice the OCR of either alone, which is why it is a separate
                # binding rather than the default: you spend it when you want
                # the periphery and a good read of your target at once.
                found = sd.merge_contacts(sd.detect(frame, ocr), _centre(),
                                          frame.shape[0])
            elif mode == "focus":
                found = _centre()
            else:
                found = sd.detect(frame, ocr)
            n_named = 0
            for c in found:
                if c.range_km is None:
                    continue
                # Only real handles go forward. Abandoned-ship serials
                # (XX-0000-XX) and crewed NPC vessels ("Firstname Lastname")
                # are contacts, but they aren't people - no point voting on
                # them, logging them, or looking them up.
                if c.kind == "player":
                    accumulated.append(consensus.Reading(
                        c.name, c.range_km, c.name_box.cx, c.name_box.y))
                    # Keep the frame and boxes behind each reading. A low
                    # confidence read is only checkable if the picture it came
                    # from still exists, and --debug is not always on, so the
                    # audit cannot rely on debug_bursts/ being there.
                    evidence.append((c.name, frame, c.name_box, c.range_box))
                    n_named += 1
                elif c.kind == "unreadable":
                    unreadable += 1
                else:
                    discarded[c.kind] = discarded.get(c.kind, 0) + 1
            per_frame.append(n_named)

            if args.debug:
                con.say(f"  [debug] frame {idx}: detect returned "
                        f"{[(c.name, c.range_km) for c in found]}")

            # Announce what we know so far. Anything already said stays quiet.
            for r in consensus.resolve(accumulated, min_votes=args.min_votes):
                if _ignored(r.name):
                    continue
                ok, corrected_from = announcer.should(r.name, now)
                if ok:
                    announced += 1
                    note = f"  (was {corrected_from})" if corrected_from else ""
                    con.say(f"[{datetime.now():%H:%M:%S}]  {r.name:<22} "
                            f"{r.range_km:>5.1f} km   {r.votes}/{r.total} "
                            f"votes{note}")
                    # Anything already known fires immediately - this is the
                    # part that has to be fast, not the RSI enrichment.
                    if db is not None:
                        try:
                            a = db.assess(r.name)
                            if a.alert:
                                con.say(f"          ***  KNOWN: {r.name} "
                                        f"(score {a.score:+.1f})")
                            for detail in a.lines() if a.alert else []:
                                con.say(f"          ***    {detail}")
                        except Exception:
                            pass
                elif args.verbose and r.name not in mentioned:
                    con.say(f"[{datetime.now():%H:%M:%S}]  (repeat) {r.name} "
                            f"{r.range_km:.1f} km")
                mentioned.add(r.name)

        # Log once at the end, with the burst's final vote counts rather than
        # the partial ones each announcement was made on.
        final = consensus.resolve(accumulated, min_votes=args.min_votes)
        for r in final:
            rule = _ignored(r.name)
            if rule:
                if args.verbose:
                    con.say(f"  ignored {r.name} (matches '{rule}')")
                continue
            writer.writerow([datetime.now().isoformat(timespec="seconds"),
                             r.name, f"{r.range_km:.1f}", r.votes, r.total,
                             f"{r.confidence:.2f}"])
            # Look up only settled readings with corroboration. A truncation
            # can resolve to a real DIFFERENT person - 'HORNE' is somebody's
            # actual handle, and a single-frame read of a contact that was
            # really HORNET65 pulled up a stranger's org history. For a tool
            # that flags people, that is the failure that matters most, so
            # identity attribution needs more votes than mere reporting.
            # Two different bars, because the two actions carry different risk.
            # A sighting records "this string appeared on screen" and accuses
            # nobody; the vote count is stored alongside, so weak reads stay
            # identifiable. An RSI lookup attaches a real person's identity and
            # org history to that string, and a truncation can resolve to a
            # genuine different player - so that needs corroboration.
            seen = 0
            if db is not None:
                try:
                    seen = db.record_sighting(r.name, r.range_km,
                                              r.votes, r.total)
                    if args.audit:
                        _maybe_audit(db, r, evidence, seen)
                except Exception:
                    pass
            # Corroboration across PINGS counts as well as corroboration across
            # frames. Gating on burst votes alone meant a contact read 1/1 on
            # five separate pings was never looked up at all, however many
            # times it was confirmed - and separate pings are the better
            # evidence of the two: frames half a second apart are near
            # identical, so OCR repeats the same mistake and the vote confirms
            # it, while a second ping is an independent look.
            if max(r.votes, seen) >= args.rsi_min_votes:
                enricher.submit(r.name)
        logf.flush()

        # Audio goes out once the burst has settled, with whatever the local
        # database already knows. RSI flags arrive later on their own thread
        # and raise their own alert then - the same progressive pattern as the
        # console, so a warning is never held back waiting on the network.
        if alerts is not None:
            payload = []
            for r in final:
                reason = ""
                if db is not None:
                    try:
                        a = db.assess(r.name)
                        if a.alert:
                            reason = (a.lines() or [""])[0]
                    except Exception:
                        pass
                payload.append({"handle": r.name, "range_km": r.range_km,
                                "flags": [], "reason": reason})
            alerts.burst_result(payload)

        # Progressive announcement necessarily reports the vote count at first
        # sighting, which is always 1/1 - later frames haven't been read yet.
        # Without a closing tally it looks like voting never happens at all.
        if final and mentioned:
            tally = ", ".join(f"{r.name} {r.votes}/{r.total}"
                              for r in final if r.name in mentioned)
            if tally:
                con.say(f"          final: {tally}")

        if args.verbose:
            # Per-frame yield answers whether later frames earn their latency,
            # and whether wave timing is right - a first frame contributing
            # nothing means the delay is too short.
            junk = ", ".join(f"{n} {k}" for k, n in sorted(discarded.items()))
            con.say(f"  [burst] named reads per frame: {per_frame}"
                    f"{f', {unreadable} unreadable' if unreadable else ''}"
                    f"{f', discarded {junk}' if junk else ''}"
                    f" -> {len(final)} contact(s)")

    deadline = started + args.seconds if args.seconds else None

    try:
        if key_mode:
            # Idle until the ping key is pressed. No continuous capture at all,
            # so no backlog can build: we grab frames only when asked to.
            # Queue entries carry WHICH binding queued them, so a focused
            # press still reads focused after waiting behind a full-frame one.
            for _b in binds.values():
                _b.drain()
            pending: list[str] = []

            def poll_keys():
                """Count presses instead of discarding them.

                Rapid pings can reveal different contacts each time - the
                scan sweeps, ships move - so a press during a burst is a
                request for another read, not noise. Bounded, because an
                unbounded queue would have the tool reading minutes-old
                screens while you fly on.
                """
                for _mode, _b in binds.items():
                    if _b.pressed() and len(pending) < args.queue_max:
                        pending.append(_mode)

            while deadline is None or time.time() < deadline:
                poll_keys()
                if not pending:
                    # Show a PARTIALLY held binding. A combination that never
                    # completes does nothing and says nothing, which is
                    # indistinguishable from the tool being broken - so if some
                    # of a binding is down, name the parts that are not.
                    part = ""
                    for _m, _b in binds.items():
                        _h, _t, _miss = _b.partial()
                        if _h and _h < _t:
                            part = (f"  |  {_m}: {_h}/{_t} held, waiting on "
                                    f"{', '.join(_miss)}")
                            break
                    # Counts first, bindings last: the line is clamped to the
                    # console width, so whatever sits at the end is what gets
                    # trimmed on a narrow window. The binding list is reference
                    # material; the counters are what you glance at.
                    con.status(f"  idle | {triggers} pings | "
                               f"{announced} announced{part} | "
                               f"waiting for {_waiting_label(binds)}")
                    # Idle is the only safe moment to delete files: a sweep
                    # during a burst would compete with the capture for disk.
                    # Rate-limited inside _housekeep, so calling it every tick
                    # costs one clock read.
                    _housekeep()
                    if _restart_requested():
                        con.status("")
                        print("\n  restart requested from the UI - reloading "
                              "settings and starting over")
                        release()       # hand the capture device back first
                        if alerts is not None:
                            alerts.close()
                        logf.close()
                        os.execv(sys.executable, [sys.executable] + sys.argv)
                    time.sleep(0.03)
                    continue

                mode = pending.pop(0)
                triggers += 1
                if alerts is not None:
                    alerts.event("trigger")

                if args.ping_delay > 0:
                    end = time.time() + args.ping_delay
                    while time.time() < end:
                        poll_keys()
                        con.status(f"  {mode} ping - "
                                   f"waiting {end - time.time():.1f}s for "
                                   f"labels..."
                                   + (f"  [{len(pending)} queued]"
                                      if pending else ""))
                        time.sleep(0.03)

                queued = f"  [{len(pending)} queued]" if pending else ""
                if mode == "chat":
                    # One frame, not a burst. Chat is static UI text that does
                    # not flicker or move between frames, so voting over three
                    # of them would cost three OCR passes to confirm what the
                    # first one already said.
                    con.status(f"  reading chat...{queued}")
                    read_chat_now(grab())
                else:
                    label = {"focus": "reading centre",
                             "dual": "reading wide + centre"}.get(
                                 mode, "reading") + queued
                    read_burst(grab(), time.time(), label,
                               on_wait=poll_keys, mode=mode)
        else:
            while deadline is None or time.time() < deadline:
                cycle = time.perf_counter()
                frame = grab()
                if frame is None:
                    if exhausted:
                        break
                    time.sleep(0.005)
                    continue
                frames += 1

                pairs = cheap_pass(frame, template)

                # Replay runs frames as fast as they process, so wall-clock
                # advances far faster relative to the sequence than it would
                # live - which would expire tracks and lapse cooldowns that
                # should still hold. A virtual clock keeps replay faithful.
                now = (started + frames / args.hz) if args.replay else time.time()
                n_new = tracker.update(
                    [(p.name_box.cx, p.name_box.y) for p in pairs], now)

                con.status(f"  watching | {frames} frames | {len(pairs)} "
                           f"contact(s) on screen | {len(tracker.tracks)} tracked "
                           f"| {announced} announced")

                # Continuous mode has no idle branch, so the housekeeping and
                # restart checks live here instead. Both are rate-limited or a
                # single stat(), so running them per frame is free.
                _housekeep()
                if not args.replay and _restart_requested():
                    con.status("")
                    print("\n  restart requested from the UI - "
                          "reloading settings and starting over")
                    release()
                    if alerts is not None:
                        alerts.close()
                    logf.close()
                    os.execv(sys.executable, [sys.executable] + sys.argv)

                if n_new:
                    triggers += 1
                    read_burst(frame, now, f"reading {n_new} new contact(s)")

                if not args.replay:
                    slack = interval - (time.perf_counter() - cycle)
                    if slack > 0:
                        time.sleep(slack)

    except KeyboardInterrupt:
        pass
    finally:
        # Give in-flight lookups a moment to land before the summary.
        if enricher.client is not None and not enricher.q.empty():
            pending = enricher.q.qsize()
            con.status(f"  finishing {pending} RSI lookup(s)...")
            deadline_rsi = time.time() + min(15.0, pending * 4.0)
            while not enricher.q.empty() and time.time() < deadline_rsi:
                time.sleep(0.2)
        enricher.stop()
        con.done()
        release()
        logf.close()

    mins = (time.time() - started) / 60.0
    print("\n" + "=" * 62)
    print(f"  {mins:.1f} min | {frames} frames | {triggers} triggers | "
          f"{announced} announced")
    if announcer.counts:
        print("\n  contacts seen:")
        for name, n in sorted(announcer.counts.items(), key=lambda kv: -kv[1]):
            print(f"     {name:<24} {n} burst(s)")
    print("=" * 62)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
