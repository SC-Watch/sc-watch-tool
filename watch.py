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
import heapq
import os
import queue
import shutil
import sys
import threading
import time
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path

import cv2

import chat
import consensus
import debuglog
import housekeeping
import imgio
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
    "focus_size": "focus_size",
    "focus_mask": "focus_mask", "burst": "burst",
    "burst_spacing": "burst_spacing", "ping_delay": "ping_delay",
    "monitor": "monitor", "hz": "hz", "queue_max": "queue_max",
    "min_votes": "min_votes",
    "gate": "gate", "haze": "haze", "cooldown": "cooldown",
    "radius": "radius", "expire": "expire", "audio_mode": "audio",
    "rsi_min_votes": "rsi_min_votes", "audit_enabled": "audit",
    "debug_frames": "debug", "debug_log": "debug_log",
    "chat_key": "chat_key",
    "chat_region": "chat_region", "dual_key": "dual_key",
    "dual_on_read": "dual_on_read",
}


# Per-frame diagnostic lines kept in the log. The console still prints
# them all; the log is the copy somebody has to read.
DEBUG_DETAIL_MAX = 12

RESTART_FLAG = paths.data("restart.flag")


def _relaunch() -> bool:
    """Restart this process with the same arguments. False if it failed.

    argv[0] is NOT the same thing in a source run and a frozen one, and
    getting that wrong is why "restart watcher" closed the .exe instead of
    restarting it:

        source   sys.executable = python.exe
                 sys.argv       = ['watch.py', '--verbose']
                 -> [python.exe, 'watch.py', '--verbose']        correct

        frozen   sys.executable = sc-watch.exe
                 sys.argv       = ['sc-watch.exe', '--verbose']
                 -> [sc-watch.exe, 'sc-watch.exe', '--verbose']  WRONG

    In the frozen case the exe path arrives as a POSITIONAL argument.
    argparse answers "unrecognized arguments: sc-watch.exe", exits 2,
    and the window vanishes. Reproduced against the real build.

    Frozen already carries its own path in argv[0], so only the rest is
    forwarded; from source argv[0] is the script and has to be kept.

    execv does not return on success. If it raises - a locked exe, a path with
    something odd in it - that is reported rather than left as a process that
    quietly stopped. The caller then ends the session: see Session._restart.
    """
    argv = sys.argv[1:] if paths.FROZEN else sys.argv
    try:
        os.execv(sys.executable, [sys.executable] + argv)
    except Exception as exc:
        print(f"! restart failed: {type(exc).__name__}: {exc}", file=sys.stderr)
        print("  the watcher has to stop - its camera and log were already "
              "handed back. Reopen it to pick up the new settings.",
              file=sys.stderr)
        return False
    return True


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


def _crops_from(frame, name_box, range_box) -> dict:
    """Cut the two label crops out of a frame. Cheap, and small to keep.

    Separated from writing them because the RSI path needs the pixels NOW -
    while the frame is still in hand - but does not know whether it will need
    to write them until a lookup comes back seconds later. A name crop is a
    few tens of KB; holding the 8 MB frame that long, for every handle in
    flight, is what this avoids.
    """
    out = {}
    for key, box in (("crop_name", name_box), ("crop_range", range_box)):
        if box is None:
            continue
        try:
            crop = box.crop(frame, 10)
        except Exception:
            continue
        if getattr(crop, "size", 0):
            out[key] = crop.copy()
    return out


def _save_audit_images(name: str, crops: dict, frame=None,
                       quality: int = 90) -> dict:
    """Write an audit's pictures and return the paths, keyed for add_audit().

    Shared by the two things that raise audits, because they have to produce
    the same files in the same place or the viewer can only open half of them.

    Crops are lossless: they may be re-read later, and a JPEG artefact on a
    12-pixel-tall glyph is the difference between an I and a 1. The full frame
    is JPEG because it is only ever looked at by eye - measured at 4.11 MB PNG
    against 0.65 MB at q90.

    Written under the DATA ROOT, and the stored paths are relative to it,
    because that is where everything that reads them looks: the UI's audit
    viewer, orphan pruning, and the support bundle. This used to write to a
    bare relative `audit/`, which is the working directory - the same folder
    in a source run, so nobody noticed, and a different one in every built
    copy. A standalone launched from Explorer wrote beside the exe while the
    UI looked in data/audit, so every audit picture was a broken link, was
    never pruned, and never reached a bundle.

    A path is only recorded if the file was actually written. Recording it
    regardless left rows pointing at pictures that did not exist.
    """
    out = paths.data_dir("audit")
    stem = f"{datetime.now():%Y%m%d_%H%M%S}_{name[:16]}"
    saved = {}
    try:
        for key in ("crop_name", "crop_range"):
            img = crops.get(key)
            if img is None or not getattr(img, "size", 0):
                continue
            fname = f"{stem}_{key}.png"
            if imgio.imwrite(out / fname, img):
                saved[key] = f"audit/{fname}"
        if frame is not None:
            fname = f"{stem}_frame.jpg"
            if imgio.imwrite(out / fname, frame,
                             [cv2.IMWRITE_JPEG_QUALITY, int(quality)]):
                saved["frame_path"] = f"audit/{fname}"
    except Exception:
        # An audit with no pictures is still worth having - it names a read
        # that needs checking. Losing the whole row because a disk write
        # failed would be the worse outcome.
        pass
    return saved


class RsiEnricher:
    """Look handles up on RSI without blocking the read loop.

    A profile is 1-3 HTTP requests behind a 1s rate limit, which would add
    seconds to an announcement that already waits 4s for the ping wave. So the
    name is announced immediately and the org detail arrives when it arrives,
    on its own line - the same progressive pattern as the burst itself.

    Everything is cached, so a handle seen twice costs nothing the second time.
    """

    def __init__(self, con, enabled: bool = True, db=None, alerts=None,
                 client=None, audit: bool = True, dbg=None):
        self.con = con
        self.db = db
        # --no-audit turns off BOTH ways an audit can be raised. Honouring it
        # for the vote rule and ignoring it here would mean the flag quietly
        # did half of what it says.
        self.audit = audit
        self.dbg = dbg
        self.alerts = alerts
        self.q: queue.Queue = queue.Queue()
        self.client = None
        self.thread = None
        self.seen: set[str] = set()
        # Lookups waiting to be tried again: (due, sequence, handle). Only the
        # worker thread touches these.
        self._retry: list = []
        self._retry_seq = 0
        self.attempts: dict[str, int] = {}
        self._announced_pause = 0.0
        # Handles already reported from an expired copy while a refresh is
        # pending, and every flag already alerted per handle - so a refresh
        # that lands later neither repeats the line nor sounds the alarm twice.
        self._stale_shown: set[str] = set()
        self._alerted: dict[str, set] = {}
        # handle -> the crops and vote counts behind the read, kept only until
        # its lookup returns. See submit().
        self.pending: dict[str, dict] = {}
        if not enabled or client is None:
            return
        if not client.enriches:
            # Fall back to link mode rather than switching lookup off. A handle
            # with a URL beside it is still useful; silently dropping it is not.
            client.provider = "link"
            print("! no starcitizen-api.com key, and Lookup source is set to "
                  "starcitizen-api.com only - so nobody is looked up; handles "
                  "come with a lookup link instead of a profile.\n"
                  "  Set Settings > RSI lookups > Lookup source to 'sentry' to "
                  "look players up without a key, or add one:\n" + _key_help(),
                  file=sys.stderr)
        elif getattr(client, "sentry", None) is not None and not client.confirms_absence:
            # SENTRY needs no key, so lookups work - but only the other
            # service can say a handle does NOT exist, and that is what raises
            # a misread audit. Worth saying once rather than leaving it to be
            # noticed as audits that never arrive.
            print("! no starcitizen-api.com key - players are looked up "
                  "through SENTRY only. Without a key:\n"
                  "    - a misread handle cannot be confirmed as a misread, so "
                  "no 'no RSI account' audits are raised\n"
                  "    - an org's Piracy / Smuggling focus is known only if it "
                  "is already cached; new orgs show 'focus unknown'\n"
                  "  Orgs, account age and flagged-org alerts still work. "
                  "To add a key:\n" + _key_help(),
                  file=sys.stderr)
        self.client = client
        self.thread = threading.Thread(target=self._run, daemon=True)
        self.thread.start()

    # How many handles' crops to hold while their lookups are in flight. The
    # queue drains at one handle a second, so this is only ever deep during a
    # crowded ping. Crops are tens of KB, not the 8 MB frame they came from.
    PENDING_MAX = 64

    # Seconds before the second and third attempts at a lookup that failed
    # for a reason that might pass (rsi.is_transient). The failures seen in
    # practice took ~10 s to come back as HTTP 500 and answered normally when
    # asked again, so the first retry waits a little longer than that.
    RETRY_DELAYS = (15.0, 60.0)

    def submit(self, handle: str, crops: dict | None = None,
               range_km=None, votes: int = 0, total: int = 0, variants=None):
        if self.client is None or handle in self.seen:
            return
        self.seen.add(handle)
        if crops:
            if len(self.pending) >= self.PENDING_MAX:
                # Drop the oldest rather than grow without bound. Losing a
                # crop costs an audit its picture; it does not cost the audit.
                self.pending.pop(next(iter(self.pending)), None)
            self.pending[handle] = {
                "crops": crops, "range_km": range_km,
                "votes": votes, "total": total,
                "variants": [(n, c) for n, c in (variants or [])]}
        self.q.put(handle)

    def _next(self):
        """The next handle to look up: a retry that has come due, else the
        next submission. Waits on the queue only until the next retry is due,
        so a scheduled retry never holds up a new handle."""
        while True:
            now = time.monotonic()
            if self._retry and self._retry[0][0] <= now:
                return heapq.heappop(self._retry)[2]
            wait = (self._retry[0][0] - now) if self._retry else None
            try:
                return self.q.get(timeout=wait)
            except queue.Empty:
                continue

    def _failed(self, handle: str, exc: Exception):
        """A lookup raised. Retry it later if that could help, else give up
        FOR NOW - it leaves `seen`, so the next sighting asks again.

        It used to be one attempt, ever: `seen` kept the handle for the rest
        of the session, so a lookup that met a passing server error never ran
        again however often the contact was read, and the tile said "not
        looked up yet" - indistinguishable from a read too weak to look up.
        """
        from rsi import ServiceDown, describe_failure, is_transient
        if isinstance(exc, ServiceDown):
            # Nothing was sent, so this is not an attempt at THIS handle - it
            # waits out the pause instead of using up its retries on it.
            self._retry_seq += 1
            heapq.heappush(self._retry, (exc.until + 0.5, self._retry_seq, handle))
            if exc.until != self._announced_pause:
                self._announced_pause = exc.until
                waiting = len(self._retry) + self.q.qsize()
                self.con.say(f"          rsi: the lookup service is not "
                             f"responding - pausing lookups for "
                             f"{exc.remaining:.0f}s ({waiting} waiting)")
                if self.dbg is not None:
                    self.dbg.event("rsi_paused", seconds=round(exc.remaining),
                                   waiting=waiting)
            cache = getattr(self.client, "cache", None)
            if cache is not None:
                try:
                    cache.note_failure(handle, describe_failure(exc),
                                       self.attempts.get(handle, 0),
                                       gave_up=False)
                except Exception:
                    pass
            return
        why = describe_failure(exc)
        attempt = self.attempts.get(handle, 0) + 1
        self.attempts[handle] = attempt
        retry = is_transient(exc) and attempt <= len(self.RETRY_DELAYS)
        if retry:
            delay = self.RETRY_DELAYS[attempt - 1]
            self._retry_seq += 1
            heapq.heappush(self._retry,
                           (time.monotonic() + delay, self._retry_seq, handle))
            self.con.say(f"          rsi: {handle} lookup failed - {why}; "
                         f"trying again in {delay:.0f}s")
        else:
            # The crops go: an audit can only come from an answer, and there
            # is none. The next sighting brings fresh ones.
            self.pending.pop(handle, None)
            self.seen.discard(handle)
            self.attempts.pop(handle, None)
            self.con.say(f"          rsi: {handle} lookup failed - {why}; "
                         f"will try again next time it is read")
        cache = getattr(self.client, "cache", None)
        if cache is not None:
            try:
                cache.note_failure(handle, why, attempt, gave_up=not retry)
            except Exception:
                pass
        if self.dbg is not None:
            self.dbg.event("rsi_failed", handle=handle, why=why,
                           attempt=attempt, retry=retry)

    def _keep_refreshing(self, handle: str, reason: str) -> bool:
        """An expired stored profile stood in for a lookup that failed. Keep
        trying for a current one on the same schedule as a failed lookup.
        Returns True the first time, so the copy is reported once."""
        first = handle not in self._stale_shown
        self._stale_shown.add(handle)
        attempt = self.attempts.get(handle, 0) + 1
        self.attempts[handle] = attempt
        again = attempt <= len(self.RETRY_DELAYS)
        if again:
            delay = self.RETRY_DELAYS[attempt - 1]
            down_for = getattr(self.client, "down_for", lambda: 0.0)()
            self._retry_seq += 1
            heapq.heappush(self._retry, (time.monotonic() + max(delay, down_for + 0.5),
                                         self._retry_seq, handle))
        else:
            # Stop for this session's sake; the next sighting asks again.
            self.attempts.pop(handle, None)
            self.pending.pop(handle, None)
            self.seen.discard(handle)
            self._stale_shown.discard(handle)
        cache = getattr(self.client, "cache", None)
        if cache is not None:
            try:
                cache.note_failure(handle, reason, attempt, gave_up=not again)
            except Exception:
                pass
        if self.dbg is not None:
            self.dbg.event("rsi_stale", handle=handle, why=reason,
                           attempt=attempt, retry=again)
        return first

    def _run(self):
        while True:
            handle = self._next()
            if handle is None:
                return
            try:
                c = self.client.citizen(handle)
            except Exception as exc:
                self._failed(handle, exc)
                continue
            stale = getattr(c, "stale_reason", None)
            if stale:
                if not self._keep_refreshing(handle, stale):
                    continue        # already reported from this copy
                ev = self.pending.get(handle)   # the refresh may still need them
            else:
                self.attempts.pop(handle, None)
                self._stale_shown.discard(handle)
                ev = self.pending.pop(handle, None)
            if not c.exists:
                # A handle that doesn't resolve is usually an OCR error, not a
                # ghost - VONCARSTEINO read 18/18 times and the real handle was
                # VONCARSTEIN. Worth saying so rather than silently omitting.
                #
                # And worth AUDITING, which the vote rule cannot do for us:
                # that read was unanimous, so _maybe_audit() correctly left it
                # alone. "RSI has never heard of this name" is independent
                # evidence, and it is the only evidence that catches a
                # confident misread. Raised here rather than at read time
                # because the answer takes a network round trip to arrive.
                self.con.say(f"          rsi: {handle} - no profile "
                             f"(likely a misread)")
                if self.dbg is not None:
                    self.dbg.event("rsi", handle=handle, exists=False,
                                   votes=(ev or {}).get("votes"),
                                   total=(ev or {}).get("total"))
                if self.db is not None and self.audit:
                    if self.dbg is not None:
                        self.dbg.event("audit", handle=handle,
                                       reason="no_rsi_account")
                    try:
                        paths = (_save_audit_images(handle, ev["crops"])
                                 if ev and ev.get("crops") else {})
                        self.db.add_audit(
                            handle,
                            ev["range_km"] if ev else None,
                            ev["votes"] if ev else 0,
                            ev["total"] if ev else 0,
                            ev["variants"] if ev else [],
                            reason="no_rsi_account", **paths)
                    except Exception:
                        pass
                continue
            # Lookups return out of order, so the handle has to be on the
            # line - three anonymous "rsi: enlisted 12y ago" lines in a row
            # are unattributable.
            line = f"          rsi: {handle} - {c.summary()}"
            if getattr(c, "source", None) == "sentry":
                line += "  [via SENTRY]"
            if stale:
                age = (time.time() - getattr(c, "stored_at", time.time())) / 86400
                line += (f"  (stored profile from {age:.0f} days ago - {stale}; "
                         f"will refresh)")
            for flag in c.flags:
                line += f"\n          !!   {handle}: {flag}"
            self.con.say(line)

            # Flags arrive after the burst's own alert, so raise a second one
            # rather than staying silent about a warning that only just became
            # known. Only flags not already alerted: a refresh that confirms an
            # expired copy should not sound the same alarm twice.
            known = self._alerted.setdefault(handle, set())
            new_flags = [f for f in c.flags if f not in known]
            known.update(new_flags)
            if self.alerts is not None and new_flags:
                # With the range the read had, when it had one. This event
                # used to carry none, so every lookup warning said
                # "kilometres" with no number in front of it.
                self.alerts.event("flagged", handle=handle, flags=new_flags,
                                  range_km=(ev or {}).get("range_km"))

            # Remember org membership so an org flag reaches this player later,
            # even if we never look them up again.
            if self.db is not None and c.orgs:
                try:
                    self.db.set_orgs(handle, c.orgs)
                    self.db.update_profile(handle, c.moniker, c.enlisted)
                    a = self.db.assess(handle)
                    if a.flagged_orgs:
                        from reputation import manual_line
                        for sid, note in a.flagged_orgs:
                            self.con.say(f"          ***  {handle}: FLAGGED ORG "
                                         f"{sid}" + (f" - {note}" if note else "")
                                         + manual_line(sid, a.manual_orgs,
                                                       a.manual_by))
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


# ---------------------------------------------------------------------------
# Command line
# ---------------------------------------------------------------------------

def build_parser() -> argparse.ArgumentParser:
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
    ap.add_argument("--chat-region", default="auto",
                    help="chat window as x,y,w,h fractions of the screen, or "
                         "'auto' (default) to place it from the frame - the "
                         "window is anchored to screen centre, so fixed "
                         "fractions miss it on any other aspect ratio")
    ap.add_argument("--no-debug-log", dest="debug_log", action="store_false",
                    help="stop writing the session log to debug_bursts/. The "
                         "log is a few KB, rotates after 12 sessions, and is "
                         "what --support-bundle packages up")
    ap.add_argument("--support-bundle", action="store_true",
                    help="zip the session logs, your settings and your debug "
                         "screenshots into one file to send for support, then "
                         "exit. Never includes your databases or API key")
    ap.add_argument("--no-frames", dest="bundle_frames", action="store_false",
                    help="with --support-bundle: leave the screenshots out "
                         "and send a text-only bundle")
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
    # A testing switch, not a setting any more - see settings.py. Replays
    # cannot press the focus key, so this is how the focused path is tested.
    ap.add_argument("--focus-always", action="store_true",
                    help="(testing) read focused on EVERY trigger, not just "
                         "the focus key. Works under --replay, which the "
                         "focus key cannot")
    ap.add_argument("--focus-size", default="auto",
                    help="focused capture region, WxH, centred horizontally "
                         "and biased upward, or 'auto' (default) to scale "
                         f"the measured {sd.FOCUS_W}x{sd.FOCUS_H} by screen "
                         "height. Bigger tolerates the contact drifting off "
                         "centre; smaller is faster")
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
                       ("--no-dual-on-read", "dual_on_read"),
                       ("--no-focus-mask", "focus_mask")):
        ap.add_argument(flag, dest=dest, action="store_false",
                        help=argparse.SUPPRESS)
    ap.add_argument("--rsi", dest="no_rsi", action="store_false",
                    help=argparse.SUPPRESS)
    return ap


def parse_args(argv=None) -> argparse.Namespace:
    ap = build_parser()
    # Saved settings become the defaults; anything typed on the command line
    # still overrides them.
    ap.set_defaults(**_defaults_from_settings())
    return ap.parse_args(argv)


def _run_utility(args) -> int | None:
    """The one-shot commands. An exit code, or None to carry on and watch.

    Every one of these is answered before the camera, the OCR engine or the
    database is touched, because the reason somebody runs them is usually that
    one of those will not start.
    """
    if args.where:
        print(paths.describe())
        return 0

    if args.support_bundle:
        info = debuglog.build_bundle(with_frames=args.bundle_frames)
        print(info["manifest"])
        print()
        if not info.get("ok"):
            print(f"  FAILED: {info.get('error')}")
            return 1
        print(f"  written: {info['file']}")
        print(f"  size:    {info['mb']} MB")
        if info["images"] < info["images_available"]:
            print(f"  note:    {info['images']} of {info['images_available']} "
                  f"screenshots included (newest first, "
                  f"{debuglog.BUNDLE_MB:.0f} MB budget)")
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

    return None


class ConfigError(Exception):
    """A flag or setting the watcher cannot start with. Shown as-is, exit 1."""


def _parse_bindings(args, key_mode: bool) -> dict[str, inputs.Binding]:
    """Four bindings, each of which may be a key, a joystick button, or a
    combination - `rctrl+joy0.b24` is one binding, not a chord to type.
    `python inputs.py --listen` prints the string for whatever you press.
    """
    binds: dict[str, inputs.Binding] = {}
    for mode, spec, flag in (("full", args.key, "--key"),
                             ("focus", args.focus_key, "--focus-key"),
                             ("dual", args.dual_key, "--dual-key"),
                             ("chat", args.chat_key, "--chat-key")):
        try:
            b = inputs.Binding(spec)
        except inputs.BindError as exc:
            raise ConfigError(
                f"{flag} {spec!r}: {exc}\n"
                "  keys:      " + ", ".join(sorted(inputs.VK_CODES)[:18]) + ", ...\n"
                "  joysticks: joy0.b24, vid231d:0126.b7, rctrl+joy0.b24\n"
                "  run `python inputs.py --listen` to find yours") from None
        if b:
            binds[mode] = b
    if key_mode and "full" not in binds:
        raise ConfigError("--key cannot be 'off' - it is the main trigger")
    # Two bindings on the same input would make one of them unreachable.
    seen_specs = {}
    for mode, b in binds.items():
        sig = frozenset(b.tokens)
        if sig in seen_specs:
            raise ConfigError(
                f"{mode} and {seen_specs[sig]} are both bound to "
                f"{b.label()}; they have to differ")
        seen_specs[sig] = mode
    return binds


def _parse_chat_region(spec: str):
    """None means "work it out from the frame" - chat.default_region() anchors
    the window to screen CENTRE, which is where it actually is. An explicit
    four-fraction value still wins, because chat can be dragged anywhere.
    """
    if not spec.strip() or spec.strip().lower() == "auto":
        return None
    try:
        parts = [float(v) for v in spec.split(",")]
        if len(parts) != 4 or not all(0.0 <= v <= 1.0 for v in parts):
            raise ValueError
    except ValueError:
        raise ConfigError(
            f"--chat-region {spec!r} should be 'auto' or four "
            "fractions between 0 and 1, like 0.15,0.30,0.35,0.45") from None
    return tuple(parts)


def _parse_focus_size(spec: str) -> tuple:
    """(None, None) means "size it from the frame" - sd.focus_size_for() scales
    the measured 1200x600 by height, because the HUD scales with vertical
    resolution and the default was tuned on a 1440-tall screen.
    """
    if spec.strip().lower() in ("", "auto"):
        return None, None
    try:
        w, h = (int(v) for v in spec.lower().split("x"))
    except ValueError:
        raise ConfigError(f"--focus-size {spec!r} should be 'auto' or look "
                          "like 1200x600") from None
    return w, h


# ---------------------------------------------------------------------------
# Frame sources
# ---------------------------------------------------------------------------

class ScreenSource:
    """Live capture from the desktop compositor. Nothing touches the game."""

    replay = False
    exhausted = False               # the desktop never runs out of frames

    def __init__(self, monitor: int):
        try:
            import dxcam
        except ImportError:
            raise ConfigError("need dxcam: pip install dxcam") from None
        self._cam = dxcam.create(output_idx=monitor - 1, output_color="BGR")
        if self._cam is None:
            raise ConfigError("dxcam.create failed")

    def grab(self):
        return self._cam.grab()

    def release(self):
        # Idempotent: the restart path hands the device back, and shutdown
        # would otherwise release it a second time.
        if self._cam is not None:
            self._cam.release()
            self._cam = None


class ReplaySource:
    """Saved frames instead of live capture, for testing without the game.

    Same code path as ScreenSource from here down; only grab() differs.
    `exhausted` lets a burst that asked for more frames than remain stop
    asking instead of spinning.
    """

    replay = True

    def __init__(self, pattern: str):
        # glob.glob, not Path().glob: the latter raises NotImplementedError on
        # an ABSOLUTE pattern, so --replay "D:/frames/*.png" died with
        # "Non-relative patterns are unsupported" rather than doing the obvious
        # thing. Relative patterns behave identically either way.
        import glob as _glob
        self.paths = sorted(Path(p) for p in _glob.glob(pattern))
        if not self.paths:
            raise ConfigError(f"no frames match {pattern}")
        self._it = iter(self.paths)
        self.exhausted = False

    def grab(self):
        p = next(self._it, None)
        if p is None:
            self.exhausted = True
            return None
        return imgio.imread(p)

    def release(self):
        pass


def open_source(args):
    if args.replay:
        src = ReplaySource(args.replay)
        print(f"replaying {len(src.paths)} frames from {args.replay}")
        return src
    return ScreenSource(args.monitor)


# ---------------------------------------------------------------------------
# The services a session is built from
# ---------------------------------------------------------------------------

def _key_help() -> str:
    """How to add a starcitizen-api.com key, with THIS install's file path -
    the step people get wrong is finding the data folder."""
    import rsi
    return ("    1. register at https://starcitizen-api.com (the free tier is "
            "enough) and copy your key\n"
            "    2. paste it into the UI: Settings > RSI lookups - or save it, "
            "alone on one line, as\n"
            f"       {rsi.KEY_FILE}\n"
            "    3. restart the watcher")


def _make_alerts(args, cfg):
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
            if setting in cfg:
                setattr(audio_cfg, attr, cfg[setting])
        if args.audio:
            audio_cfg.mode = args.audio
        return audio.Alerts(audio_cfg)
    except Exception as exc:
        print(f"! audio disabled: {type(exc).__name__}: {exc}", file=sys.stderr)
        return None


def _make_profile_client(args, cfg):
    """One profile client, shared. The database uses it to VERIFY identity
    before merging two similar-looking handles - string resemblance is not
    evidence, and SLIVER/5LIVER and HORNE/HORNET65 are each two real people.
    """
    if args.no_rsi:
        return None
    try:
        import rsi
        return rsi.Client(mode=cfg.get("rsi_mode", "auto"))
    except Exception as exc:
        print(f"! profile lookup disabled: {type(exc).__name__}: {exc}",
              file=sys.stderr)
        return None


def _open_database(args, profile_client):
    if args.no_db:
        return None
    try:
        import reputation
        import watchlist
        verify = None
        if profile_client is not None and profile_client.enriches:
            verify = profile_client.exists
        return reputation.Database(verify=verify, watchlist=watchlist.shared())
    except Exception as exc:
        print(f"! database disabled: {type(exc).__name__}: {exc}", file=sys.stderr)
        return None


class SightingsLog:
    """sightings.csv: one row per settled contact, appended across sessions."""

    HEADER = ["timestamp", "name", "range_km", "votes", "total", "confidence"]

    def __init__(self, path):
        self.path = Path(path)
        new = not self.path.exists()
        self._f = self.path.open("a", newline="", encoding="utf-8")
        self._w = csv.writer(self._f)
        if new:
            self._w.writerow(self.HEADER)

    def write(self, r):
        self._w.writerow([datetime.now().isoformat(timespec="seconds"),
                          r.name, f"{r.range_km:.1f}", r.votes, r.total,
                          f"{r.confidence:.2f}"])

    def flush(self):
        self._f.flush()

    def close(self):
        self._f.close()


# ---------------------------------------------------------------------------
# The session
# ---------------------------------------------------------------------------

@dataclass
class Burst:
    """One burst's working state, from the first frame to the final tally."""
    label: str
    mode: str
    now: float
    accumulated: list = field(default_factory=list)
    # (name, frame, name_box, range_box) behind each reading. A low
    # confidence read is only checkable if the picture it came from still
    # exists, and --debug is not always on, so the audit cannot rely on
    # debug_bursts/ being there.
    evidence: list = field(default_factory=list)
    per_frame: list = field(default_factory=list)
    discarded: dict = field(default_factory=dict)
    # Re-resolving after every frame would re-report the same repeats once
    # per frame in verbose mode. Mention each contact at most once a burst.
    mentioned: set = field(default_factory=set)
    unreadable: int = 0

    def crops_for(self, name: str) -> dict | None:
        mine = [e for e in self.evidence if e[0] == name]
        return _crops_from(mine[0][1], mine[0][2], mine[0][3]) if mine else None


class Session:
    """Everything one run of the watcher shares, and the things it does.

    These were locals of a 1,072-line main(), and the functions that needed
    them - read_burst, read_chat, the audit rule, the ignore check,
    housekeeping - were closures nested inside it, reaching them through
    `nonlocal`. That capture is what made main() impossible to split and why
    it only ever grew. Held here, each step is an ordinary method that can be
    read, and called, on its own.
    """

    def __init__(self, args, cfg, src, ocr, template, alerts, con, db, dbg,
                 enricher, log, binds, chat_region, focus):
        self.args, self.cfg = args, cfg
        self.src, self.ocr, self.template = src, ocr, template
        self.alerts, self.con, self.db, self.dbg = alerts, con, db, dbg
        self.enricher, self.log = enricher, log
        self.binds = binds
        self.chat_region = chat_region
        self.focus_w, self.focus_h = focus
        self.key_mode = args.trigger == "key" and not args.replay
        self.tracker = Tracker(args.radius, args.expire)
        self.announcer = Announcer(args.cooldown)
        self.pending: list[str] = []
        self.frames = self.triggers = self.announced = 0
        self.started = time.time()
        self._hk_last = 0.0

    # -- startup -----------------------------------------------------------

    def banner(self):
        args, binds = self.args, self.binds
        print("=" * 62)
        print("  sc-watch")
        if self.key_mode:
            print(f"  trigger: {binds['full'].label()} pressed, then read after "
                  f"{args.ping_delay:.1f}s")
            if "focus" in binds:
                print(f"  focused: {binds['focus'].label()} reads the centre "
                      f"{args.focus_size} only")
            if "chat" in binds:
                print(f"  chat:    {binds['chat'].label()} lists who is talking")
        else:
            print(f"  trigger: continuous, {args.hz:.0f} Hz")
        print(f"  {args.burst}-frame bursts, {args.cooldown:.0f}s repeat cooldown")
        print(f"  logging to {self.log.path.resolve()}")
        if paths.FROZEN:
            # Only when installed. From source the answer is "here", and a
            # line saying so in every dev run is noise.
            print(f"  your data {paths.DATA_ROOT}")
            old = paths.unused_old_data()
            if old:
                print(f"  ! an older sc-watch's data is at {old}")
                print("    and this copy is NOT using it. To bring it over: close both")
                print(f"    windows, copy what is in that folder into {paths.DATA_ROOT},")
                print("    and start again (README: Upgrading).")
        # Which recogniser produced this session's readings. Two are possible
        # and they do not agree, so a log without this is hard to compare
        # against the measurements in the README.
        print(f"  reading with {sd.ocr_backend_info()}")
        if args.debug:
            print("  DEBUG: saving every frame to debug_bursts/ (~4.7 MB each)")

        # The LOG is on by default and the FRAMES are not, because they cost
        # wildly different things: a session log is a few KB and rotates, a
        # frame is 4.7 MB and debug_bursts/ was measured at 810 MB on a real
        # machine. On by default matters - the log has to already exist when
        # somebody hits a problem, or the first thing we ask is "turn it on
        # and do it again".
        self.dbg.header(args, extra={
            "ocr backend": sd.ocr_backend_info(),
            "monitor": args.monitor,
            "capture hz": args.hz,
            "trigger": "key" if self.key_mode else "auto",
            "bindings": _waiting_label(binds) if self.key_mode else "n/a",
        })
        if self.dbg.path:
            print(f"  log: {self.dbg.path.name} in debug_bursts/")
        print("  Ctrl+C to stop")
        print("=" * 62)

    def housekeep(self, force=False):
        """Retention. housekeeping.py existed for three sessions before
        anything called it, and debug_bursts/ reached 1.1 GB across 234 frames
        in the meantime - a cleanup nothing runs is not a cleanup.

        Rate-limited here, so calling it every idle tick costs one clock read.
        """
        every = float(self.cfg.get("housekeeping_every_min", 30.0)) * 60.0
        if not self.cfg.get("housekeeping_enabled", True):
            return
        if not force and (every <= 0 or time.time() - self._hk_last < every):
            return
        self._hk_last = time.time()
        line = housekeeping.run_now(apply=True)
        if self.args.verbose or "freed" in line:
            print(f"  {line}")

    # -- the database-facing rules ------------------------------------------

    def ignored(self, name):
        """Is this a label you have marked as not-a-person?

        Checked in the WATCHER rather than inside classify_name(), because the
        rule lives in the database and the detector is a pure module with no
        database handle. It also means the list can be edited from the UI and
        take effect on the next read, without restarting anything - which is
        the point: you see a crate reported as a contact, you press ignore, it
        stops. Waiting for a restart would make it feel broken.
        """
        if self.db is None:
            return None
        try:
            return self.db.is_ignored(name)
        except Exception:
            return None

    def maybe_audit(self, r, burst: Burst, seen=0):
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
        mine = [e for e in burst.evidence if e[0] == r.name]
        if not mine:
            return
        _, frame, nb, rb = mine[0]
        # Logged, because an audit that is raised and then cleared by the next
        # ping's corroboration otherwise leaves no trace at all - which makes
        # "why was I not asked to audit that?" unanswerable from a session log.
        self.dbg.event("audit", handle=r.name, reason="weak_read",
                       votes=r.votes, total=r.total, seen=seen)
        crop_paths = _save_audit_images(
            r.name, _crops_from(frame, nb, rb),
            frame if self.cfg.get("audit_keep_frame", True) else None,
            int(self.cfg.get("audit_frame_quality", 90)))
        self.db.add_audit(r.name, r.range_km, r.votes, r.total,
                          [(n, c) for n, c in (r.variants or [])],
                          reason="weak_read", **crop_paths)

    # -- chat --------------------------------------------------------------

    def read_chat(self, frame):
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
        con, db = self.con, self.db
        if frame is None:
            con.say("  chat: no frame captured")
            return
        try:
            lines = chat.read(frame, self.ocr, region=self.chat_region)
        except Exception as exc:
            con.say(f"  chat: read failed ({type(exc).__name__}: {exc})")
            return
        if not lines:
            con.say("  chat: nothing readable in the chat region "
                    "(check 'chat region' in Settings)")
            if self.alerts is not None:
                self.alerts.event("nothing")
            return

        lines = [c for c in lines if not self.ignored(c.key)]
        if not lines:
            con.say("  chat: everything readable is on the ignore list")
            return
        con.say(f"  chat: {len(lines)} handle(s)")
        self.dbg.event("chat", handles=[c.handle for c in lines],
                       region=str(self.chat_region or "auto"))
        # Known contacts in chat are SPOKEN, as they are from a read. They
        # used to get the routine tone and nothing else - a handle on your
        # own report list typing in chat made the same sound as a stranger.
        # No range: chat has none, and the phrase leaves the distance out.
        spoken = []
        for c in lines:
            note = ""
            if db is not None:
                try:
                    db.record_sighting(c.key, None, 1, 1)
                    a = db.assess(c.key)
                    if a.alert:
                        note = "   " + "; ".join(a.lines()[:2])
                        spoken.append({"handle": c.key, "range_km": None,
                                       "flags": [], "reason": a.lines()[0],
                                       "said": a.spoken_all()})
                except Exception:
                    pass
            con.say(f"    {c.handle:<24}{note}")
            self.announced += 1
            # Always looked up: a chat handle is stronger evidence than the
            # vote threshold exists to protect against. (A no-op when lookups
            # are off - the enricher has no client then.)
            self.enricher.submit(c.key)
        if self.alerts is not None:
            self.alerts.event("contact")
            if spoken:
                self.alerts.burst_result(spoken)

    # -- bursts ------------------------------------------------------------

    def read_burst(self, first, now, label, on_wait=None, mode="full"):
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
        args = self.args
        # Two promotions of a plain read, in priority order. Asking for both
        # is contradictory - one says "centre only", the other says "wide AND
        # centre" - and dual is the strictly more informative of the two, so
        # it wins rather than silently discarding the periphery.
        if args.dual_on_read and mode == "full":
            mode = "dual"
        elif args.focus_always and mode == "full":
            mode = "focus"
        b = Burst(label=label, mode=mode, now=now)

        captured = self._capture(first, on_wait)
        self.con.status(f"  {label} - captured {len(captured)}, reading...")

        for idx, frame in enumerate(captured):
            self.con.status(f"  {label} - reading frame {idx + 1}/{len(captured)}...")
            frame_file = self._debug_frame(frame, idx) if args.debug else ""
            found = self._detect(frame, mode)
            n_named = self._tally(b, frame, found)

            if args.debug:
                self.con.say(f"  [debug] frame {idx}: detect returned "
                             f"{[(c.name, c.range_km) for c in found]}")
            # Logged unconditionally, unlike the block above. This is the one
            # thing every support question needs - WHAT THE WATCHER READ - and
            # it is already computed, so it costs a line rather than a second
            # pass over the frame. `file` ties the reading to the screenshot
            # it came from, which is what makes a bundle's images worth
            # having: a frame on its own is a puzzle, a frame plus the names
            # it produced is a diagnosis.
            self.dbg.event("read", file=frame_file, idx=idx, mode=mode,
                           named=n_named, unreadable=b.unreadable,
                           discarded=b.discarded or None,
                           got=[(c.name, c.range_km) for c in found])
            self._announce_so_far(b)

        # Settled once at the end, with the burst's final vote counts rather
        # than the partial ones each announcement was made on.
        final = consensus.resolve(b.accumulated, min_votes=args.min_votes)
        self.dbg.event("burst", label=label, mode=mode, frames=len(captured),
                       per_frame=b.per_frame,
                       result=[(r.name, r.votes, r.total, r.range_km)
                               for r in final])
        self._settle(final, b)
        self.log.flush()
        self._burst_audio(final)
        self._burst_report(final, b)

    def _capture(self, first, on_wait) -> list:
        """CAPTURE THE WHOLE BURST FIRST, then read it.

        Interleaving grab and OCR - which is what progressive announcement
        originally did - means each capture waits for the previous frame's
        ~1.5s of OCR. Frames configured 0.5s apart actually landed ~2s apart,
        spanning four seconds of game time: contacts drift, ping labels fade,
        and later frames read something different or nothing at all. That
        showed up as contacts stuck at 1 vote.

        Capturing up front costs nothing (a grab is ~2ms) and puts the frames
        where they were configured to be.
        """
        args = self.args
        captured = [f for f in (first,) if f is not None]
        tries = 0
        while len(captured) < args.burst and tries < args.burst * 8:
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
            extra = self.src.grab()
            if extra is not None:
                captured.append(extra)
            elif self.src.exhausted:
                break
        return captured

    def _debug_frame(self, frame, idx) -> str:
        """--debug: save the frame and report what each stage found in it.
        Returns the saved file's name, which ties log lines to the picture."""
        con, dbg, template = self.con, self.dbg, self.template
        # `dbg_dir`, not `dbg`: `dbg` is the session log.
        dbg_dir = paths.data_dir("debug_bursts")
        path = dbg_dir / f"burst_{datetime.now():%H%M%S}_{idx}.png"
        imgio.imwrite(path, frame)
        frame_file = path.name
        boxes = sd.find_boxes(sd.hud_mask(frame))
        pairs = sd.pair_boxes(boxes, frame.shape[0])
        gated = cheap_pass(frame, template)
        con.say(f"  [debug] frame {idx}: boxes={len(boxes)} "
                f"pairs={len(pairs)} passed_gate={len(gated)} "
                f"-> {path.name}")
        # Same numbers to the log. The console scrolls away; this is the copy
        # somebody can send.
        dbg.event("frame", file=frame_file, idx=idx, boxes=len(boxes),
                  pairs=len(pairs), passed_gate=len(gated),
                  size=f"{frame.shape[1]}x{frame.shape[0]}")
        # Capped. An 11-frame replay produced 154 near-miss and 230
        # gate-reject lines; a real session would bury the events somebody is
        # actually looking for. The first few show the pattern, and the counts
        # above say how many there were.
        diags = list(sd.pair_diagnostics(boxes, frame.shape[0]))
        for why in diags:
            con.say(f"  [debug]   near-miss {why}")
        for why in diags[:DEBUG_DETAIL_MAX]:
            dbg.event("near_miss", file=frame_file, why=why)
        if len(diags) > DEBUG_DETAIL_MAX:
            dbg.event("near_miss", file=frame_file,
                      more=len(diags) - DEBUG_DETAIL_MAX)
        rejected = 0
        for p in pairs:
            s = km_gate.score(p.range_box.crop(frame, sd.CROP_PAD), template) \
                if template is not None else 1.0
            if s < sd.KM_GATE_THRESHOLD:
                con.say(f"  [debug]   gate rejected {p.name_box.w}x"
                        f"{p.name_box.h}@{p.name_box.x},{p.name_box.y} "
                        f"range {p.range_box.w}px score={s:.3f} "
                        f"(need {sd.KM_GATE_THRESHOLD})")
                rejected += 1
                if rejected <= DEBUG_DETAIL_MAX:
                    dbg.event("gate_reject", file=frame_file,
                              box=f"{p.name_box.w}x{p.name_box.h}"
                                  f"@{p.name_box.x},{p.name_box.y}",
                              score=round(s, 3),
                              need=sd.KM_GATE_THRESHOLD)
        if rejected > DEBUG_DETAIL_MAX:
            dbg.event("gate_reject", file=frame_file,
                      more=rejected - DEBUG_DETAIL_MAX)
        return frame_file

    def _detect(self, frame, mode):
        """Run the detector the mode asks for.

        A focused read crops to the centre of the screen and lifts the OCR
        cap. Boxes come back in full-frame coordinates either way, so dedup
        and position tracking are unaffected.
        """
        def centre():
            return sd.detect_focused(frame, self.ocr, self.focus_w, self.focus_h,
                                     direct=not self.args.focus_mask)

        if mode == "dual":
            # Both passes over the SAME frame, then merged. Costs roughly
            # twice the OCR of either alone, which is why it is a separate
            # binding rather than the default: you spend it when you want the
            # periphery and a good read of your target at once.
            return sd.merge_contacts(sd.detect(frame, self.ocr), centre(),
                                     frame.shape[0])
        if mode == "focus":
            return centre()
        return sd.detect(frame, self.ocr)

    @staticmethod
    def _tally(b: Burst, frame, found) -> int:
        """Sort one frame's contacts into the burst. Returns named reads."""
        n_named = 0
        for c in found:
            if c.range_km is None:
                continue
            # Only real handles go forward. Abandoned-ship serials
            # (XX-0000-XX) and crewed NPC vessels ("Firstname Lastname") are
            # contacts, but they aren't people - no point voting on them,
            # logging them, or looking them up.
            if c.kind == "player":
                b.accumulated.append(consensus.Reading(
                    c.name, c.range_km, c.name_box.cx, c.name_box.y))
                b.evidence.append((c.name, frame, c.name_box, c.range_box))
                n_named += 1
            elif c.kind == "unreadable":
                b.unreadable += 1
            else:
                b.discarded[c.kind] = b.discarded.get(c.kind, 0) + 1
        b.per_frame.append(n_named)
        return n_named

    def _announce_so_far(self, b: Burst):
        """Announce what the burst knows so far. Anything already said stays
        quiet."""
        con, db = self.con, self.db
        for r in consensus.resolve(b.accumulated, min_votes=self.args.min_votes):
            if self.ignored(r.name):
                continue
            ok, corrected_from = self.announcer.should(r.name, b.now)
            if ok:
                self.announced += 1
                note = f"  (was {corrected_from})" if corrected_from else ""
                con.say(f"[{datetime.now():%H:%M:%S}]  {r.name:<22} "
                        f"{r.range_km:>5.1f} km   {r.votes}/{r.total} "
                        f"votes{note}")
                # Anything already known fires immediately - this is the part
                # that has to be fast, not the RSI enrichment.
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
            elif self.args.verbose and r.name not in b.mentioned:
                con.say(f"[{datetime.now():%H:%M:%S}]  (repeat) {r.name} "
                        f"{r.range_km:.1f} km")
            b.mentioned.add(r.name)

    def _settle(self, final, b: Burst):
        """Record the burst's final readings: log, database, audit, lookup."""
        args, db = self.args, self.db
        for r in final:
            rule = self.ignored(r.name)
            if rule:
                if args.verbose:
                    self.con.say(f"  ignored {r.name} (matches '{rule}')")
                continue
            self.log.write(r)
            # Two different bars, because the two actions carry different
            # risk. A sighting records "this string appeared on screen" and
            # accuses nobody; the vote count is stored alongside, so weak reads
            # stay identifiable. An RSI lookup attaches a real person's
            # identity and org history to that string, and a truncation can
            # resolve to a genuine different player - 'HORNE' is somebody's
            # actual handle, and a single-frame read of a contact that was
            # really HORNET65 pulled up a stranger's org history. For a tool
            # that flags people, that is the failure that matters most.
            seen = 0
            if db is not None:
                try:
                    seen = db.record_sighting(r.name, r.range_km,
                                              r.votes, r.total)
                    if args.audit:
                        self.maybe_audit(r, b, seen)
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
                # Hand the crops over WITH the handle. If the lookup comes
                # back "no such account" the enricher raises an audit, and by
                # then this frame is long gone - so the pixels travel with the
                # request rather than being fetched when it is too late.
                self.enricher.submit(r.name, b.crops_for(r.name), r.range_km,
                                     r.votes, r.total, r.variants)

    def _burst_audio(self, final):
        """Audio goes out once the burst has settled, with whatever the local
        database already knows. RSI flags arrive later on their own thread and
        raise their own alert then - the same progressive pattern as the
        console, so a warning is never held back waiting on the network.
        """
        if self.alerts is None:
            return
        payload = []
        for r in final:
            reason, said = "", None
            if self.db is not None:
                try:
                    a = self.db.assess(r.name)
                    if a.alert:
                        reason = (a.lines() or [""])[0]
                        # What speech says: one reason, as data, worded by
                        # the phrases in audio.json. `reason` stays the
                        # console line for anything that reads it.
                        said = a.spoken_all()
                except Exception:
                    pass
            payload.append({"handle": r.name, "range_km": r.range_km,
                            "flags": [], "reason": reason, "said": said})
        self.alerts.burst_result(payload)

    def _burst_report(self, final, b: Burst):
        # Progressive announcement necessarily reports the vote count at first
        # sighting, which is always 1/1 - later frames haven't been read yet.
        # Without a closing tally it looks like voting never happens at all.
        if final and b.mentioned:
            tally = ", ".join(f"{r.name} {r.votes}/{r.total}"
                              for r in final if r.name in b.mentioned)
            if tally:
                self.con.say(f"          final: {tally}")

        if self.args.verbose:
            # Per-frame yield answers whether later frames earn their latency,
            # and whether wave timing is right - a first frame contributing
            # nothing means the delay is too short.
            junk = ", ".join(f"{n} {k}" for k, n in sorted(b.discarded.items()))
            self.con.say(f"  [burst] named reads per frame: {b.per_frame}"
                         f"{f', {b.unreadable} unreadable' if b.unreadable else ''}"
                         f"{f', discarded {junk}' if junk else ''}"
                         f" -> {len(final)} contact(s)")

    # -- the loops ---------------------------------------------------------

    def run(self) -> int:
        self.started = time.time()
        deadline = self.started + self.args.seconds if self.args.seconds else None
        rc = 0
        try:
            rc = (self._run_keys if self.key_mode else self._run_auto)(deadline)
        except KeyboardInterrupt:
            pass
        finally:
            self._shutdown()
        self._summary()
        return rc

    def poll_keys(self):
        """Count presses instead of discarding them.

        Rapid pings can reveal different contacts each time - the scan sweeps,
        ships move - so a press during a burst is a request for another read,
        not noise. Bounded, because an unbounded queue would have the tool
        reading minutes-old screens while you fly on.
        """
        for mode, b in self.binds.items():
            if b.pressed() and len(self.pending) < self.args.queue_max:
                self.pending.append(mode)

    def _run_keys(self, deadline) -> int:
        """Idle until a binding is pressed. No continuous capture at all, so
        no backlog can build: frames are grabbed only when asked for. Queue
        entries carry WHICH binding queued them, so a focused press still
        reads focused after waiting behind a full-frame one."""
        args, con = self.args, self.con
        for b in self.binds.values():
            b.drain()
        self.pending = []

        while deadline is None or time.time() < deadline:
            self.poll_keys()
            if not self.pending:
                con.status(self._idle_status())
                # Idle is the only safe moment to delete files: a sweep during
                # a burst would compete with the capture for disk.
                self.housekeep()
                if _restart_requested():
                    return self._restart()
                time.sleep(0.03)
                continue

            mode = self.pending.pop(0)
            self.triggers += 1
            if self.alerts is not None:
                self.alerts.event("trigger")

            if args.ping_delay > 0:
                end = time.time() + args.ping_delay
                while time.time() < end:
                    self.poll_keys()
                    con.status(f"  {mode} ping - "
                               f"waiting {end - time.time():.1f}s for labels..."
                               + (f"  [{len(self.pending)} queued]"
                                  if self.pending else ""))
                    time.sleep(0.03)

            queued = f"  [{len(self.pending)} queued]" if self.pending else ""
            if mode == "chat":
                # One frame, not a burst. Chat is static UI text that does not
                # flicker or move between frames, so voting over three of them
                # would cost three OCR passes to confirm what the first one
                # already said.
                con.status(f"  reading chat...{queued}")
                self.read_chat(self.src.grab())
            else:
                label = {"focus": "reading centre",
                         "dual": "reading wide + centre"}.get(
                             mode, "reading") + queued
                self.read_burst(self.src.grab(), time.time(), label,
                                on_wait=self.poll_keys, mode=mode)
        return 0

    def _idle_status(self) -> str:
        # Show a PARTIALLY held binding. A combination that never completes
        # does nothing and says nothing, which is indistinguishable from the
        # tool being broken - so if some of a binding is down, name the parts
        # that are not.
        part = ""
        for mode, b in self.binds.items():
            held, total, missing = b.partial()
            if held and held < total:
                part = (f"  |  {mode}: {held}/{total} held, waiting on "
                        f"{', '.join(missing)}")
                break
        # Counts first, bindings last: the line is clamped to the console
        # width, so whatever sits at the end is what gets trimmed on a narrow
        # window. The binding list is reference material; the counters are
        # what you glance at.
        return (f"  idle | {self.triggers} pings | {self.announced} announced"
                f"{part} | waiting for {_waiting_label(self.binds)}")

    def _run_auto(self, deadline) -> int:
        args, con = self.args, self.con
        interval = 1.0 / args.hz
        while deadline is None or time.time() < deadline:
            cycle = time.perf_counter()
            frame = self.src.grab()
            if frame is None:
                if self.src.exhausted:
                    break
                time.sleep(0.005)
                continue
            self.frames += 1

            pairs = cheap_pass(frame, self.template)

            # Replay runs frames as fast as they process, so wall-clock
            # advances far faster relative to the sequence than it would live
            # - which would expire tracks and lapse cooldowns that should still
            # hold. A virtual clock keeps replay faithful.
            now = (self.started + self.frames / args.hz) if args.replay \
                else time.time()
            n_new = self.tracker.update(
                [(p.name_box.cx, p.name_box.y) for p in pairs], now)

            con.status(f"  watching | {self.frames} frames | {len(pairs)} "
                       f"contact(s) on screen | {len(self.tracker.tracks)} "
                       f"tracked | {self.announced} announced")

            # Continuous mode has no idle branch, so the housekeeping and
            # restart checks live here instead. Both are rate-limited or a
            # single stat(), so running them per frame is free.
            self.housekeep()
            if not args.replay and _restart_requested():
                return self._restart()

            if n_new:
                self.triggers += 1
                self.read_burst(frame, now, f"reading {n_new} new contact(s)")

            if not args.replay:
                slack = interval - (time.perf_counter() - cycle)
                if slack > 0:
                    time.sleep(slack)
        return 0

    def _restart(self) -> int:
        """Hand everything back and re-exec. Returns only if that failed.

        A failed relaunch used to print "the watcher is still running" and
        carry on - with the camera already released and the sightings log
        already closed, so the next read died on a closed file. Everything is
        torn down before execv because it has to be: the new process needs
        the capture device. So a failure here ends the session, honestly,
        through the same shutdown and summary as Ctrl+C.
        """
        self.con.status("")
        print("\n  restart requested from the UI - reloading "
              "settings and starting over")
        self.src.release()          # hand the capture device back first
        if self.alerts is not None:
            self.alerts.close()
        self.log.close()
        _relaunch()                 # does not return if it worked
        self.dbg.event("restart_failed")
        return 1

    # -- shutdown ----------------------------------------------------------

    def _shutdown(self):
        enricher = self.enricher
        # Give in-flight lookups a moment to land before the summary.
        if enricher.client is not None and not enricher.q.empty():
            waiting = enricher.q.qsize()
            self.con.status(f"  finishing {waiting} RSI lookup(s)...")
            deadline_rsi = time.time() + min(15.0, waiting * 4.0)
            while not enricher.q.empty() and time.time() < deadline_rsi:
                time.sleep(0.2)
        enricher.stop()
        self.con.done()
        self.src.release()
        self.log.close()

    def _summary(self):
        alerts, counts = self.alerts, self.announcer.counts
        mins = (time.time() - self.started) / 60.0
        # Audio health in the summary and the log. Speech dying silently was
        # the entire shape of the bug this reports on; a counter nobody prints
        # is the same as no counter.
        if alerts is not None and (alerts.failures or alerts.repairs):
            print(f"\n  audio: {alerts.failures} failure(s), "
                  f"{alerts.repairs} engine repair(s)"
                  + (f" - last: {alerts.last_error}" if alerts.last_error else ""))
        # Alerts that waited past audio.MAX_ALERT_AGE and were dropped unsaid.
        # Only when there were some, like the line above.
        expired = getattr(alerts, "expired", 0)
        if expired:
            import audio
            print(f"\n  audio: {expired} alert(s) dropped - still queued "
                  f"{audio.MAX_ALERT_AGE:.0f}s after the read, too late to be news")
        # Closed in the same breath as the console summary so the log ends
        # with the same numbers the user just read, and a support bundle made
        # after a crash still has a complete tail.
        self.dbg.close({"minutes": round(mins, 1), "frames": self.frames,
                        "triggers": self.triggers, "announced": self.announced,
                        "audio_failures": getattr(alerts, "failures", 0),
                        "audio_repairs": getattr(alerts, "repairs", 0),
                        "audio_last_error": getattr(alerts, "last_error", "") or "-",
                        "audio_available": getattr(alerts, "available", {}),
                        **({"audio_expired": expired} if expired else {}),
                        "contacts": dict(sorted(counts.items(),
                                                key=lambda kv: -kv[1]))})
        print("\n" + "=" * 62)
        print(f"  {mins:.1f} min | {self.frames} frames | {self.triggers} "
              f"triggers | {self.announced} announced")
        if counts:
            print("\n  contacts seen:")
            for name, n in sorted(counts.items(), key=lambda kv: -kv[1]):
                print(f"     {name:<24} {n} burst(s)")
        print("=" * 62)


def main(argv=None) -> int:
    args = parse_args(argv)
    rc = _run_utility(args)
    if rc is not None:
        return rc

    # Everything that can be wrong with the configuration is checked before
    # anything is opened. A mistyped binding used to be discovered after the
    # camera, the OCR models and the database were already up - slow to
    # report, and it exited holding all three.
    key_mode = args.trigger == "key" and not args.replay
    try:
        binds = _parse_bindings(args, key_mode)
        chat_region = _parse_chat_region(args.chat_region)
        focus = _parse_focus_size(args.focus_size)
        src = open_source(args)
    except ConfigError as exc:
        sys.stderr.write(f"{exc}\n")
        return 1

    ocr = sd.make_ocr()
    template = km_gate.load_template()
    if template is None:
        print("! no km_template.npy - monitoring will be slow "
              "(from source: python tools/build_km_template.py)", file=sys.stderr)

    # Loaded once, here, because several things below read it: the detector
    # switches, the audio overlay, the audit capture and the housekeeping
    # sweep. Re-reading the file per use would let a mid-session edit apply to
    # one and not another.
    cfg = settings_mod.load()
    sd.KM_GATE_ENABLED = args.gate
    sd.HAZE_LOCATOR_ENABLED = args.haze
    sd.ORPHAN_RETRY_MAX = (sd.ORPHAN_RETRY_WHEN_ON
                           if cfg.get("dim_range_retry") else 0)

    alerts = _make_alerts(args, cfg)
    con = Console()
    profile_client = _make_profile_client(args, cfg)
    # Opened before the database and the enricher, because both of them take
    # it - and because a failure in either is exactly the kind of thing the log
    # exists to record. header() is written by banner(), once the bindings and
    # trigger mode are known.
    dbg = debuglog.DebugLog(enabled=bool(getattr(args, "debug_log", True)))
    db = _open_database(args, profile_client)
    enricher = RsiEnricher(con, enabled=not args.no_rsi, db=db, alerts=alerts,
                           audit=args.audit, dbg=dbg, client=profile_client)
    log = SightingsLog(args.log)

    if key_mode:
        for mode, b in binds.items():
            if b.missing:
                sys.stderr.write(
                    f"! {mode} binding {b.label()} needs a device that is not "
                    f"plugged in ({', '.join(b.missing)})\n")
    # Settings clamp hz to 1-30, but --hz comes straight off the command line
    # and 0 divides. A watcher that will not start is worse than a slow one.
    if args.hz <= 0:
        print(f"! --hz {args.hz} is not a rate; using 5 Hz")
        args.hz = 5.0

    session = Session(args, cfg, src, ocr, template, alerts, con, db, dbg,
                      enricher, log, binds, chat_region, focus)
    session.banner()
    # Startup is the right moment for retention: the disk is about to be
    # written to hard, and last session's frames have had their chance to be
    # looked at.
    if cfg.get("housekeeping_on_start", True):
        session.housekeep(force=True)
    return session.run()


if __name__ == "__main__":
    raise SystemExit(main())
