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
Second-screen UI: a tile per contact, served locally.

    python ui_server.py            # opens a browser at http://127.0.0.1:8731

Reads the same SQLite databases watch.py writes, so the two run independently -
start the UI whenever, leave it open across sessions, restart either without
disturbing the other.

A browser rather than a desktop toolkit because the ask was for Star Citizen's
holographic blue, and that is a styling problem CSS is simply better at. It
also makes 'open RSI profile' a link rather than a feature.
"""

from __future__ import annotations

import itertools
import json
import os
import re
import sqlite3
import subprocess
import sys
import threading
import time
import webbrowser
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlparse

import dbopen
import housekeeping
import inputs as inputs_mod
import debuglog
import flags as flags_mod
import reputation
import rsi
import share
import watchlist
import settings
import paths

HOST, PORT = "127.0.0.1", 8731
# Two roots, because this module touches both kinds of file. Audit images
# and the frame directories are things we WRITE, so they follow the data
# root; an installed build cannot write beside its own executable.
HERE = paths.DATA_ROOT

# The UI writes this; the watcher deletes it and re-execs. A file rather than a
# signal because the two are separate processes started independently, and
# neither has a handle on the other.
RESTART_FLAG = HERE / "restart.flag"


# --------------------------------------------------------------------------
# The identity verifier
#
# reputation.Database refuses to fold one handle onto another when BOTH spell
# real accounts - SLIVER and 5LIVER are two different people, and giving one
# of them the other's history is the worst thing this tool can do. That refusal
# needs a way to ask whether an account exists, and it is passed in.
#
# The watcher and the CLI both passed one. The UI did not, so every merge the
# UI could reach - confirming an audit is the main one - ran with verify=None
# and took the "cannot tell" branch, which for the human-driven path means
# proceed. The check was written, tested and unreachable from the one place a
# person actually clicks.
#
# The page's one client (_client()), so its identity checks share the pacing,
# the pauses and the remembered answers of every other lookup here - it used
# to be a second client of its own. Lookups are cached in rsi_cache.db, so
# confirming the same audit twice costs nothing, and with no lookup backend
# this stays None and behaves exactly as before.
# --------------------------------------------------------------------------
_verify = None
_verify_ready = False
_verify_lock = threading.Lock()


def _verifier():
    global _verify, _verify_ready
    with _verify_lock:
        if not _verify_ready:
            _verify_ready = True
            try:
                if _client().enriches:
                    _verify = _shared_exists
            except Exception:
                _verify = None
        return _verify


def _shared_exists(handle: str, strict: bool = False):
    return _client().exists(handle, strict=strict)


def _open_db():
    """A database that can answer identity questions.

    The caller owns it and should close() it. Refcounting used to be the only
    thing releasing these, which works until a traceback holds a frame - a
    test pinned 51 connections that way.

    watchlist.shared() is None until a third-party list has been imported, so
    this is exactly the old call for anyone who never set one up.
    """
    return reputation.Database(verify=_verifier(), watchlist=watchlist.shared())


# Cached, because measuring it is expensive and the answer barely moves. The
# scan stats every file in five directories, which was 29.5 ms of an 86 ms
# collect() - a third of the cost of building the whole page, for a number that
# only appears on one settings panel. It gets worse the more frames are on disk,
# which is exactly when you least want the UI slowing down. Ten seconds is far
# fresher than anyone reads it.
_DISK_TTL = 10.0
_disk_cache: tuple[float, list[dict]] = (0.0, [])


def _disk_usage(max_age: float = _DISK_TTL) -> list[dict]:
    """What the capture directories are costing, biggest first.

    The Settings tab shows this next to the retention controls because the
    numbers are the argument: 'delete frames older than 24h' means nothing
    until you can see that debug_bursts/ is 1.1 GB.
    """
    global _disk_cache
    now = time.time()
    if _disk_cache[1] and now - _disk_cache[0] < max_age:
        return _disk_cache[1]
    out = []
    for name in list(housekeeping.MANAGED) + ["audit"]:
        d = HERE / name
        if not d.is_dir():
            continue
        n = size = 0
        for p in d.iterdir():
            try:
                if p.is_file():
                    n += 1
                    size += p.stat().st_size
            except OSError:
                pass
        out.append({"name": name, "files": n, "mb": round(size / 1e6, 1)})
    out = sorted(out, key=lambda r: -r["mb"])
    _disk_cache = (now, out)
    return out


def _folder_targets() -> list[dict]:
    """Every folder this app uses, as a FIXED table keyed by name.

    The table is the security boundary, and it is why the API takes a `key`
    and never a path. os.startfile() on a directory opens Explorer; on a file
    it RUNS the file. A path that arrived in a request body is a path an
    attacker chose, and "open this folder" would quietly become "execute this
    executable". So the client can only name one of these rows, and the
    handler still checks the resolved target is a directory before opening it.

    `program` is listed only when it differs from the data root. Running from
    source they are the same directory and two buttons pointing at one folder
    is just noise; installed, they are far apart and that is exactly the
    confusion this answers.
    """
    out = [{"key": "data", "label": "Your data",
            "path": str(paths.DATA_ROOT),
            "note": "databases, settings, keybinds"}]
    if paths.RESOURCE_ROOT != paths.DATA_ROOT:
        out.append({"key": "program", "label": "Program files",
                    "path": str(paths.RESOURCE_ROOT),
                    "note": "the app itself - nothing you edit"})
    known = {"audit": "crops behind uncertain reads",
             "live_frames": "frames kept from the watcher",
             "debug_bursts": "full bursts saved for debugging",
             "probe_frames": "probe captures",
             "detections": "annotated detection images"}
    for name in ["audit"] + list(housekeeping.MANAGED):
        d = HERE / name
        if d.is_dir():
            out.append({"key": name, "label": name,
                        "path": str(d), "note": known.get(name, "")})
    return out


def _reveal(p: Path) -> None:
    """Show a directory in the desktop file manager."""
    if sys.platform.startswith("win"):
        os.startfile(str(p))                      # noqa: S606 - a directory
    elif sys.platform == "darwin":
        subprocess.Popen(["open", str(p)])
    else:
        subprocess.Popen(["xdg-open", str(p)])


# RSI handles are 1-40 characters of letters, digits, underscore and hyphen.
# Real stored examples that must keep working: 0F-8942-TS, PANDE_MONIUM,
# GRIM-H2GOBLIN. Enforced because every write path here reaches add_report() or
# ensure_player(), both of which INSERT a players row - so an unvalidated field
# is a way to put arbitrary junk in the database. A malformed POST wrote a
# 5000-character player during this audit, and `{"handle": null}` wrote one
# called "NONE", because str(None) is a perfectly good string.
HANDLE_RE = re.compile(r"^[A-Za-z0-9_-]{1,40}$")
MAX_BODY = 16 * 1024 * 1024
# Org SIDs are the short code in the RSI URL, e.g. EXMPL. Same character set as
# a handle and shorter; validated for the same reason, which is that it arrives
# from a URL and goes into a query.
#
# The example is deliberately invented. Every org SID in this file used to be
# one this tool had actually flagged, which published a list of the author's
# own judgements about real organisations as a side effect of documenting a
# regex. Keep the placeholders fictional.
SID_RE = re.compile(r"^[A-Za-z0-9_-]{1,32}$")


def _clean_handle(raw) -> str:
    h = ("" if raw is None else str(raw)).strip()
    if not HANDLE_RE.match(h):
        raise ValueError(f"not a valid handle: {h[:24]!r}")
    return h.upper()


def _clean_sid(raw) -> str:
    v = ("" if raw is None else str(raw)).strip()
    if not SID_RE.match(v):
        raise ValueError(f"not a valid org SID: {v[:24]!r}")
    return v.upper()


def _clean_date(raw) -> str:
    """A typed enlistment date: YYYY-MM-DD, not in the future, not before
    RSI accounts existed. Blank is allowed - it clears the entry."""
    v = ("" if raw is None else str(raw)).strip()
    if not v:
        return ""
    import datetime as _dt
    try:
        d = _dt.date.fromisoformat(v)
    except ValueError:
        raise ValueError(f"not a date: {v[:24]!r} - use YYYY-MM-DD") from None
    if d > _dt.date.today() or d.year < 2011:
        raise ValueError(f"{v} is not a date an RSI account could have been made")
    return d.isoformat()


def _days_since(iso: str) -> int | None:
    try:
        import datetime as _dt
        return (_dt.date.today() - _dt.date.fromisoformat(iso)).days
    except (TypeError, ValueError):
        return None


# The "hear it" button's voice: one speech engine for the life of the UI,
# built on first use. Driven through audio.Alerts, whose worker thread owns the
# engine - SAPI must be created and driven on the same thread (see
# Alerts._make_engine), and a request thread is gone the moment it answers.
_VOICE = None
_VOICE_LOCK = threading.Lock()


def _preview_voice():
    """The voice, or (None, why). Waits for it to actually come up: the engine
    is built on the voice's own thread, and "queued" used to be reported as
    "saying" even when that build had failed - every click after that
    answered ok and played nothing. A dead voice is dropped, so the next click
    tries again instead of reusing it."""
    global _VOICE
    with _VOICE_LOCK:
        if _VOICE is not None and (not _VOICE.available["speech"]
                                   or not _VOICE._thread.is_alive()):
            _VOICE = None
        if _VOICE is None:
            try:
                import audio
                cfg = audio.AudioConfig.load()
                s = settings.load()
                cfg.mode = "speech"
                cfg.volume = float(s.get("volume", cfg.volume))
                cfg.rate = int(s.get("rate", cfg.rate))
                v = audio.Alerts(cfg)
            except Exception as exc:
                return None, f"{type(exc).__name__}: {exc}"
            deadline = time.time() + 5.0
            while (v.available["speech"] and v._engine is None
                   and time.time() < deadline):
                time.sleep(0.05)
            if not v.available["speech"] or v._engine is None:
                return None, v.last_error or "no speech engine on this machine"
            _VOICE = v
        return _VOICE, ""


API_KEY_RE = re.compile(r"^[A-Za-z0-9_-]{8,128}$")


def _key_status() -> dict:
    """Whether a starcitizen-api.com key is set, and where from. Never the
    key itself - the page gets its last four characters, enough to tell two
    keys apart and useless to anyone else."""
    env = os.environ.get("SC_API_KEY", "").strip()
    try:
        k = (rsi.KEY_FILE.read_text(encoding="utf-8-sig").strip()
             if rsi.KEY_FILE.exists() else "")
    except OSError:
        k = ""
    live = env or k
    return {"set": bool(live), "from": "env" if env else ("file" if k else None),
            "hint": live[-4:] if live else "", "file_set": bool(k),
            "path": str(rsi.KEY_FILE)}


def _save_key(payload: dict) -> dict:
    """Write sc_api_key.txt from the page, or remove it. The file is the one
    place a key is read from besides SC_API_KEY, so this is all "adding a key"
    ever needed - it just meant finding the data folder first."""
    key = str(payload.get("key", "")).strip()
    if payload.get("remove") is True:
        try:
            rsi.KEY_FILE.unlink(missing_ok=True)
        except OSError as exc:
            return {"ok": False, "error": f"could not remove it: {exc}"}
        return {"ok": True, "status": _key_status(),
                "message": "key removed - restart the watcher to stop using it"}
    if not API_KEY_RE.match(key):
        return {"ok": False, "error": "that does not look like an API key - copy "
                                      "just the key, with nothing around it"}
    try:
        rsi.KEY_FILE.write_text(key + "\n", encoding="utf-8")
    except OSError as exc:
        return {"ok": False, "error": f"could not save it: {exc}"}
    st = _key_status()
    msg = "key saved - restart the watcher to start using it"
    if st["from"] == "env":
        msg += (" (note: an SC_API_KEY environment variable is also set, and it "
                "wins over the file)")
    return {"ok": True, "status": st, "message": msg}


_CLIENT_LOCK = threading.Lock()
_CLIENT: tuple | None = None        # (what it was built from, the client)


def _client() -> "rsi.Client":
    """The page's RSI client, honouring the configured API mode.

    Built through here rather than `rsi.Client()` at each call site, because
    `rsi_mode` was a setting nothing read: three separate constructors all took
    the library default and the control in the UI did nothing.

    ONE client, shared by every request, rebuilt only when what it was built
    from changes (mode, source, key). It used to be a new one per call, and the
    pacing and the outage breaker live on the client: five "try now" clicks
    went out as five unpaced streams, a 429 telling SENTRY's caller to wait
    was forgotten by the next lookup, and "once per run" guards lasted one
    lookup. A client is safe across threads (dbopen, _Paced's locks).
    """
    global _CLIENT
    cfg = settings.load()
    key = (cfg.get("rsi_mode", "auto"), cfg.get("rsi_source", "sentry"),
           rsi.find_api_key(), str(rsi.CACHE_PATH))
    with _CLIENT_LOCK:
        if _CLIENT is None or _CLIENT[0] != key:
            _CLIENT = (key, rsi.Client(mode=key[0]))
        return _CLIENT[1]


# The background check on profiles missing an org's focus - see
# rsi.Client.complete_partials(). Here, not in the watcher: it is about tiles,
# and must never sit in the queue of lookups for players near you. First a
# minute after start, out of the way of the first page load.
FOCUS_SWEEP_FIRST, FOCUS_SWEEP_EVERY = 60.0, 600.0


_NAME_TRIES: dict[str, tuple[int, float]] = {}    # SID -> (asked, next due)

# Storage & cleanup's "look up contacts that were never checked": at most this
# many a run. See the job.
BACKFILL_MAX = 60

# Contacts whose last lookup FAILED - no profile at all - asked again by the
# background check: a lookup refused while SENTRY's live fetches were used
# up, or one that met a failing fallback. Before, only a sighting or a button
# did. A failure older than RETRY_FAILED_AFTER (the watcher's own retries are
# long over), each contact once per rsi.SENTRY_LIVE_AGAIN, newest first.
# SENTRY's database first - no live allowance, and it finds anyone SENTRY has
# picked up on its own since - then at most RETRY_FAILED_LIVE live fetches a
# round (60 an hour, half the allowance); none while SENTRY says wait.
RETRY_FAILED_AFTER = 1800.0
RETRY_FAILED_LIVE = 10
RETRY_FAILED_CHECKS = 30
_FAILED_TRIES: dict[str, float] = {}               # HANDLE -> next due


def _retry_failed_lookups(client, ask: bool) -> dict:
    """One round of retrying failed lookups - see RETRY_FAILED_AFTER.
    Returns counts: checked (database asks), live (fetches), found."""
    got = {"checked": 0, "live": 0, "found": 0}
    sentry = client.sentry
    if (not ask or sentry is None or not client.enriches
            or sentry.down_for() > 0 or sentry.live_paused_for() > 0):
        return got
    rows = client.cache.db.execute(
        """SELECT handle FROM lookup_failures
           WHERE ts < ?
             AND handle NOT IN (SELECT handle FROM citizens)
             AND handle NOT IN (SELECT handle FROM partial_citizens)
           ORDER BY ts DESC""", (time.time() - RETRY_FAILED_AFTER,)).fetchall()
    db = _open_db()
    try:
        now = time.monotonic()
        for (h,) in rows:
            if got["checked"] >= RETRY_FAILED_CHECKS:
                break
            if now < _FAILED_TRIES.get(h, 0.0):
                continue
            # Contacts only: a reading that never became one was mostly a
            # misread, and fetching those is what used the allowance up.
            if not db.db.execute("SELECT 1 FROM players WHERE handle=?",
                                 (h,)).fetchone():
                continue
            got["checked"] += 1
            try:
                held = sentry.citizen(h, live=False)    # its database only
            except Exception:
                break           # its breaker says when; the next round asks
            if held is None:
                if got["live"] >= RETRY_FAILED_LIVE or sentry.live_paused_for() > 0:
                    continue    # no live fetch left this round: the next one
                got["live"] += 1
            _FAILED_TRIES[h] = now + rsi.SENTRY_LIVE_AGAIN
            try:
                # Held: from the remembered answer, no request. Not held: the
                # one live fetch, then the other service as ever.
                c = client.citizen(h)
            except Exception:
                if sentry.live_paused_for() > 0:
                    break       # refused: stop for the pause
                continue
            if c.exists and not getattr(c, "unverified", False):
                if c.orgs:
                    db.set_orgs(h, c.orgs)
                db.update_profile(h, c.moniker, c.enlisted)
                got["found"] += 1
    finally:
        db.close()
    return got


def _fill_flag_names(client, ask: bool) -> int:
    """Names for flagged orgs that have none - our cache first, at no cost,
    then SENTRY's profile, one request per org, on rsi.FOCUS_SWEEP_GAPS.
    Returns how many were named."""
    db = _open_db()
    named = 0
    try:
        now = time.monotonic()
        for sid in db.unnamed_flagged():
            name = client.org_name(sid, ask=False)
            n, due = _NAME_TRIES.get(sid, (0, 0.0))
            if not name and ask and now >= due:
                gaps = rsi.FOCUS_SWEEP_GAPS
                _NAME_TRIES[sid] = (n + 1, now + gaps[min(n, len(gaps) - 1)])
                name = client.org_name(sid)
            if name and db.set_flag_name(sid, name):
                _NAME_TRIES.pop(sid, None)
                named += 1
    finally:
        db.close()
    return named


def _focus_sweeper(srv):
    """Run complete_partials() every FOCUS_SWEEP_EVERY until shutdown, name
    any flagged org that has no name, and retry failed lookups
    (_retry_failed_lookups). Asks SENTRY only while "Look
    handles up" is on; whatever can be done from what is already stored costs
    nothing and always runs. Tiles update through the stream, which watches
    the cache. Never spoken."""
    time.sleep(FOCUS_SWEEP_FIRST)
    while not getattr(srv, "_shutting_down", False):
        try:
            ask = bool(settings.load().get("rsi_enabled", True))
            client = _client()
            with rsi.why("bg-focus"):
                got = client.complete_partials(ask=ask)
            if got["completed"] or got["filled"]:
                print(f"  org focus check: {got['completed']} profile(s) "
                      f"completed, {got['filled']} filled in part")
            with rsi.why("bg-names"):
                named = _fill_flag_names(client, ask)
            if named:
                print(f"  org focus check: named {named} flagged org(s)")
            with rsi.why("bg-retry"):
                again = _retry_failed_lookups(client, ask)
            if again["found"]:
                print(f"  background check: {again['found']} failed lookup(s) "
                      f"answered ({again['live']} needed a live fetch)")
        except Exception:
            pass                # the next round tries again
        finally:
            dbopen.release_thread()
        time.sleep(FOCUS_SWEEP_EVERY)


def _rsi_cache(handles=None) -> dict:
    """Cached profile data for the handles asked for. Never fetches here -
    the UI must not block or hammer the API on a page refresh.

    Scoped to the contacts being drawn, because the cache grows with every
    handle ever looked up while the page only ever shows the most recent 120.
    Loading all of it was the one part of building the page that scaled with
    total history rather than with what is on screen:

        195 profiles     2.4 ms      (today)
       1000 profiles     9.7 ms
       5000 profiles    41.8 ms
      20000 profiles   186.9 ms      per stream frame

    Passing `handles` makes it O(shown) instead. `None` still loads everything,
    for callers that genuinely want the lot.
    """
    out = {}
    path = rsi.CACHE_PATH
    if not path.exists():
        return out
    if handles is not None:
        handles = [h.upper() for h in handles]
        if not handles:
            return out
    try:
        db = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
        db.row_factory = sqlite3.Row
        if handles is None:
            rows = db.execute("SELECT handle, data, fetched_at FROM citizens")
        else:
            # Chunked: SQLite caps host parameters per statement (999 on older
            # builds), and a caller could hand us more handles than that.
            rows = []
            for i in range(0, len(handles), 400):
                chunk = handles[i:i + 400]
                qs = ",".join("?" * len(chunk))
                rows += db.execute(
                    f"SELECT handle, data, fetched_at FROM citizens WHERE handle IN ({qs})",
                    chunk).fetchall()
        for row in rows:
            try:
                prof = json.loads(row["data"])
                # When it was STORED, which is what "how old is this" means.
                # The profile's own fetched_at is when the Citizen object was
                # built, and is absent from some older rows.
                prof["_stored_at"] = row["fetched_at"]
                out[row["handle"].upper()] = prof
            except Exception:
                pass
        _partials(db, handles, out)
        db.close()
    except Exception:
        pass
    return out


def _partials(db, handles, out: dict):
    """Lay SENTRY's partial profiles over `out` - see rsi.Cache.

    One is shown when there is no whole profile, or when it is newer than the
    whole one; marked `_partial` with why, and `_unknown_focus` naming the orgs
    whose focus is missing, so a missing PIRACY mark is never read as "no".
    Its own try: a cache from before the table existed must still draw every
    other tile.
    """
    try:
        if handles is None:
            rows = db.execute("SELECT handle, data, observed_at, reason, unknown "
                              "FROM partial_citizens").fetchall()
        else:
            rows = []
            for i in range(0, len(handles), 400):
                chunk = handles[i:i + 400]
                qs = ",".join("?" * len(chunk))
                rows += db.execute(
                    "SELECT handle, data, observed_at, reason, unknown FROM "
                    f"partial_citizens WHERE handle IN ({qs})", chunk).fetchall()
    except sqlite3.Error:
        return
    for handle, data, observed, reason, unknown in rows:
        h = handle.upper()
        whole = out.get(h)
        if whole is not None and (whole.get("_stored_at") or 0) >= (observed or 0):
            continue
        try:
            prof = json.loads(data)
            prof["_stored_at"] = observed
            prof["_partial"] = reason or "incomplete"
            prof["_unknown_focus"] = [s.upper() for s in json.loads(unknown or "[]")]
        except (TypeError, ValueError):
            continue
        out[h] = prof


def _rsi_failures(handles) -> dict:
    """Lookups that failed and have not since succeeded, for these handles.

    handle -> {"error", "attempts", "gave_up", "age_s"}. Empty if the cache
    predates the table, which is the same as "nothing failed".
    """
    out = {}
    path = rsi.CACHE_PATH
    handles = [h.upper() for h in handles or []]
    if not handles or not path.exists():
        return out
    try:
        db = sqlite3.connect(path.resolve().as_uri() + "?mode=ro", uri=True)
        try:
            now = time.time()
            for i in range(0, len(handles), 400):
                chunk = handles[i:i + 400]
                qs = ",".join("?" * len(chunk))
                for h, ts, err, n, gave in db.execute(
                        "SELECT handle, ts, error, attempts, gave_up FROM "
                        f"lookup_failures WHERE handle IN ({qs})", chunk):
                    out[h] = {"error": err, "attempts": n,
                              "gave_up": bool(gave), "age_s": int(now - ts)}
        finally:
            db.close()
    except sqlite3.Error:
        pass
    return out


# Outcome of the most recent manual lookup per handle, so a background fetch
# can report back. Bounded because it is only ever a UI hint - the real result
# is the profile landing in the cache, which the stream picks up on its own.
LOOKUPS: dict[str, str] = {}
_LOOKUPS_MAX = 40
# When each handle's latest lookup FINISHED, as a number that only grows. The
# page cannot tell a new result from the previous one by its text - the same
# summary twice reads the same - so a tile waiting on its lookup watches this
# number pass the one it was given when the button was pressed.
LOOKUP_DONE: dict[str, int] = {}
_DONE_SEQ = itertools.count(1)
# Handles with a lookup in flight: a second press while one runs is not a
# second request to a service that may be taking 25 s to answer.
_LOOKING: set[str] = set()
_LOOKING_LOCK = threading.Lock()


def lookup_now(handle: str, fresh: bool = False) -> bool:
    """Fetch a profile because a PERSON asked for it, not because a burst did.

    The poll path deliberately never fetches - it must not hammer the API on a
    page refresh - but confirming an audit is a one-off, user-initiated action,
    and a human who has read the crop and typed the name is better evidence
    than the two votes `--rsi-min-votes` normally demands. So the usual bar
    does not apply here; the evidence is stronger, not weaker.

    Runs on its own thread: the API is rate limited to one request a second and
    the network is the network, so blocking the POST would freeze the button.
    The result arrives the way everything else does - the profile lands in the
    cache, the database mtime changes, and the stream pushes the new tile.

    `fresh` is a tile's look up / try now button: skip the cache and ask now.
    Those used to open the lookup window and make you wait in it - up to 50 s
    measured while the older service was timing out - for an answer the tile
    then showed anyway. Returns False if a lookup of this handle is already
    running.
    """
    h = (handle or "").strip().upper()
    if not h:
        return False
    with _LOOKING_LOCK:
        if h in _LOOKING:
            return False
        _LOOKING.add(h)

    def work():
        db = None
        try:
            client = _client()
            try:
                with rsi.why("try-now" if fresh else "page-lookup"):
                    c = client.citizen(h, fresh=fresh)
            except Exception as exc:
                _note_lookup_failure(client, h, exc)
                raise
            if not c.exists:
                # Same reading as the watcher's: a handle that does not resolve
                # is usually a misread, not a ghost.
                LOOKUPS[h] = f"{h}: no profile — likely still a misread"
                return
            db = _open_db()
            if c.orgs:
                db.set_orgs(h, c.orgs)
            db.update_profile(h, c.moniker, c.enlisted)
            bits = [c.summary()]
            a = db.assess(h)
            for sid, note in a.flagged_orgs:
                bits.append(f"FLAGGED ORG {sid}" + (f" — {note}" if note else ""))
            for flag in c.flags:
                bits.append(flag)
            # Answered, but not all of it: SENTRY's copy with the other
            # service down, or an org whose focus could not be had.
            if getattr(c, "stale_reason", None):
                bits.append(f"incomplete - {c.stale_reason}")
            LOOKUPS[h] = f"{h}: " + " | ".join(bits)
        except Exception as exc:
            LOOKUPS[h] = f"{h}: lookup failed - {rsi.describe_failure(exc)}"
        finally:
            if db is not None:
                db.close()
            while len(LOOKUPS) > _LOOKUPS_MAX:
                LOOKUPS.pop(next(iter(LOOKUPS)))
            # A number from ONE sequence for every handle, not a per-handle
            # count. The cap below drops the oldest entry, and it could drop a
            # handle while its lookup was still running: a count restarted
            # from 0 then finished at 1, no higher than the page was waiting
            # to see pass, and the tile sat on "looking up" for 150 s before
            # calling an updated tile "no answer". This only ever grows.
            LOOKUP_DONE.pop(h, None)
            LOOKUP_DONE[h] = next(_DONE_SEQ)
            while len(LOOKUP_DONE) > _LOOKUPS_MAX:
                LOOKUP_DONE.pop(next(iter(LOOKUP_DONE)))
            with _LOOKING_LOCK:
                _LOOKING.discard(h)

    LOOKUPS.pop(h, None)                # newest last - see LOOKUP_DONE above
    LOOKUPS[h] = f"{h}: looking up…"
    threading.Thread(target=work, daemon=True).start()
    return True


def _note_lookup_failure(client, handle: str, exc: Exception):
    """Record a manual lookup that failed, the same way the watcher does, so
    the tile says "RSI lookup failed" instead of going back to "not looked
    up". A person asked once; nothing retries it for them."""
    try:
        client.cache.note_failure(handle, rsi.describe_failure(exc), 1,
                                  gave_up=True)
    except Exception:
        pass


# Status of the most recent roster pull per org, so a background fetch can
# report back. Same shape and same reasoning as LOOKUPS above: the real result
# is rows landing in the database, and this is only the line of text that says
# what happened while you waited.
ROSTERS: dict[str, str] = {}
_ROSTERS_MAX = 20
# Orgs whose roster is being pulled RIGHT NOW. A pull is up to twenty requests
# at one a second, and the button is easy to press twice: without this, a
# second click sent a second walk of the same pages through the same rate
# limiter, doubling the wait and the API budget for an identical answer.
_ROSTER_BUSY: set[str] = set()


def fetch_roster_now(sid: str):
    """Pull a flagged org's member list, on its own thread.

    Threaded because this is the slowest thing the UI can ask for by a wide
    margin. The API is rate limited to one request a second and a roster is
    paginated 32 at a time, so a 600-member org is twenty seconds of waiting -
    long enough that doing it inside the POST would look like the server had
    died.

    Nothing here writes a report or a judgement about anybody. It records
    MEMBERSHIP, which is a fact about a player, so that a flag you put on an
    org can reach the people actually in it.
    """
    try:
        sid = _clean_sid(sid)
    except ValueError as exc:
        return {"ok": False, "error": str(exc)}

    def work():
        db = None
        try:
            db = _open_db()
            # A big roster is thirty requests at one a second, so it says
            # where it has got to rather than sitting on one line for half a
            # minute looking stuck.
            with rsi.why("roster"):
                out = db.fetch_roster(
                    sid, client=_client(),
                    on_page=lambda page, so_far: ROSTERS.__setitem__(
                        sid, f"{sid}: {so_far} member(s) so far, page {page}…"))
            if out.get("ok"):
                bits = [f"{out['members']} member(s)"]
                for key, word in (("joined", "new"), ("back", "back on it"),
                                  ("left", "off the roster since the last pull")):
                    if out.get(key):
                        bits.append(f"{len(out[key])} {word}")
                if out.get("org_members"):
                    bits.append(f"RSI lists {out['org_members']}")
                if out.get("hidden") or out.get("redacted"):
                    bits.append(f"{out.get('hidden', 0) + out.get('redacted', 0)}"
                                f" hidden or redacted")
                if out.get("source") == "sentry":
                    bits.append("via SENTRY")
                if out.get("switched"):
                    # First pull from a different source: nobody is retired,
                    # because two services' lists are not the same list.
                    bits.append("first pull from this source, so nobody was "
                                "marked as having left")
                ROSTERS[sid] = (
                    f"{sid}: " + ", ".join(bits) + f" - reaching "
                    f"{out['reaches']} player(s) you have seen"
                    # Said plainly, because a partial pull means the one thing
                    # this screen otherwise implies is not true: nobody was
                    # checked for having left.
                    + ("  (the list stopped early, so these are the members "
                       "seen so far and nobody was marked as having left)"
                       if out.get("partial") else ""))
            else:
                ROSTERS[sid] = f"{sid}: {out.get('error')}"
        except Exception as exc:
            ROSTERS[sid] = f"{sid}: failed - {rsi.describe_failure(exc)}"
        finally:
            _ROSTER_BUSY.discard(sid)
            if db is not None:
                db.close()
            while len(ROSTERS) > _ROSTERS_MAX:
                ROSTERS.pop(next(iter(ROSTERS)))

    if sid in _ROSTER_BUSY:
        return {"ok": True, "sid": sid,
                "message": ROSTERS.get(sid) or f"{sid}: already pulling that list"}
    _ROSTER_BUSY.add(sid)
    ROSTERS[sid] = f"{sid}: pulling the member list, one request a second…"
    threading.Thread(target=work, daemon=True).start()
    return {"ok": True, "sid": sid, "message": ROSTERS[sid]}


# Member lookups in progress or just finished, per org, for the Orgs view.
ORG_JOBS: dict[str, dict] = {}


def _needs_lookup(prof: dict | None, failed: bool) -> bool:
    """A member worth asking RSI about: no stored profile, an expired one, or
    a last attempt that failed. A partial SENTRY profile is shown, but it is
    not an answer yet - an org on it is still missing its focus."""
    if not prof or failed or prof.get("_partial"):
        return True
    ttl = rsi.CITIZEN_TTL if prof.get("exists", True) else rsi.MISS_TTL
    return time.time() - (prof.get("_stored_at") or 0) > ttl


def lookup_members_now(sid: str) -> dict:
    """Look up every CURRENT member of a flagged org that we have no usable
    profile for, one at a time, in the background.

    This is the other half of getting a bad org "local". The roster already
    means its members are recognised when you meet them, with or without the
    API; a profile adds what the roster cannot - how old the account is, and
    every other org it is in - and makes identity checks answerable offline,
    because they read the same cache. Former members are skipped: the roster
    no longer lists them, so a lookup has nothing to add to that org.

    Goes through the shared client, so it keeps to one request a second and
    stops for an outage pause like everything else. Profiles land in the
    cache, which is what the tiles and the member list read.
    """
    sid = sid.upper()
    job = ORG_JOBS.get(sid)
    if job and job.get("running"):
        return {"ok": True, "sid": sid, "message": "already looking them up"}

    db = _open_db()
    try:
        members = [m for m in db.members_of(sid) if not m["gone_since"]]
    finally:
        db.close()
    handles = [m["handle"] for m in members]
    cache = _rsi_cache(handles)
    failures = _rsi_failures(handles)
    todo = [h for h in handles if _needs_lookup(cache.get(h), h in failures)]
    job = {"running": bool(todo), "total": len(todo), "done": 0, "found": 0,
           "incomplete": 0, "no_account": 0, "failed": 0, "stop": False,
           "started": time.time(),
           "message": (f"looking up {len(todo)} member(s), about "
                       f"{max(1, round(len(todo) * 1.5 / 60))} min" if todo
                       else "every current member already has a profile")}
    ORG_JOBS[sid] = job
    if not todo:
        return {"ok": True, "sid": sid, "message": job["message"]}

    def work():
        client = _client()
        wdb = _open_db()
        waits: dict[str, int] = {}
        try:
            for h in todo:
                if job["stop"]:
                    job["message"] = f"stopped after {job['done']} of {job['total']}"
                    return
                try:
                    with rsi.why("org-members"):
                        c = client.citizen(h)
                    stale = getattr(c, "stale_reason", None)
                    # Our own expired copy standing in (it carries no source):
                    # the lookup FAILED. But an answer missing an org's focus,
                    # or SENTRY's not confirmed by the other service, is an
                    # answer - stored, and used by the tile's lookups and the
                    # watcher alike. Counting it "failed" wrote its reason into
                    # lookup_failures as though nothing had come back.
                    if stale and getattr(c, "source", None) is None:
                        raise RuntimeError(stale)
                    if c.exists:
                        job["found"] += 1
                        if stale:
                            job["incomplete"] += 1
                        if c.orgs:
                            wdb.set_orgs(h, c.orgs)   # roster rows survive this
                        # Only for someone already in your contacts.
                        # update_profile() INSERTS a player that is not there,
                        # and a roster member you have never met is not a
                        # contact - thirty strangers would appear on the grid
                        # as though you had seen them. The profile itself is in
                        # the cache either way, which is what everything reads.
                        if wdb.db.execute("SELECT 1 FROM players WHERE handle=?",
                                          (h,)).fetchone():
                            wdb.update_profile(h, c.moniker, c.enlisted)
                    else:
                        job["no_account"] += 1
                except rsi.ServiceDown as exc:
                    # Wait the pause out rather than failing everyone behind
                    # it - but only so often, or an outage that never ends
                    # would keep this thread going round forever.
                    waits[h] = waits.get(h, 0) + 1
                    if waits[h] <= 3:
                        who = getattr(exc, "service", None) or "the lookup service"
                        job["message"] = (f"{who} is not responding - "
                                          f"waiting {exc.remaining:.0f}s")
                        time.sleep(exc.remaining + 0.5)
                        todo.append(h)        # try this one again at the end
                        continue
                    job["failed"] += 1
                    _note_lookup_failure(client, h, exc)
                except Exception as exc:
                    job["failed"] += 1
                    _note_lookup_failure(client, h, exc)
                job["done"] += 1
                job["message"] = (f"looked up {job['done']} of {job['total']}")
            job["message"] = (f"done: {job['found']} found"
                              + (f" ({job['incomplete']} incomplete - an org's "
                                 f"focus unknown, or not confirmed)"
                                 if job["incomplete"] else "")
                              + (f", {job['no_account']} with no account"
                                 if job["no_account"] else "")
                              + (f", {job['failed']} failed - try again later"
                                 if job["failed"] else ""))
        finally:
            job["running"] = False
            wdb.close()

    threading.Thread(target=work, daemon=True).start()
    return {"ok": True, "sid": sid, "message": job["message"]}


def org_members(sid: str) -> dict:
    """Everyone an org's roster has listed, with what we know about each."""
    sid = sid.upper()
    db = _open_db()
    try:
        members = db.members_of(sid)
    finally:
        db.close()
    handles = [m["handle"] for m in members]
    cache = _rsi_cache(handles)
    failures = _rsi_failures(handles)
    for m in members:
        prof = cache.get(m["handle"])
        m["profile"] = None
        if prof:
            days = rsi.Citizen(handle=m["handle"],
                               enlisted=prof.get("enlisted", "")).enlisted_days
            m["profile"] = {
                "exists": bool(prof.get("exists")),
                "enlisted_days": days,
                "orgs": [o.get("sid") for o in prof.get("orgs", []) if o.get("sid")],
                "age_days": int((time.time() - (prof.get("_stored_at") or time.time())) / 86400),
            }
        m["lookup_failed"] = failures.get(m["handle"])
        m["needs_lookup"] = (not m["gone_since"]) and _needs_lookup(
            prof, m["handle"] in failures)
    job = ORG_JOBS.get(sid)
    return {"ok": True, "sid": sid, "members": members,
            "job": {k: v for k, v in job.items() if k != "stop"} if job else None}


def _audits_with_files(db, handle):
    """Audits, with any missing image path blanked and the row marked.

    A row can outlive its files - the directory is on disk and things happen to
    disks. Rendering a broken <img> tells the user nothing; saying the picture
    is gone tells them to dismiss it.
    """
    out = []
    for a in db.audits_for(handle):
        missing = False
        for key in ("crop_name", "crop_range", "frame_path"):
            rel = a.get(key) or ""
            if rel and not (HERE / rel).exists():
                a[key] = ""
                missing = True
        a["files_missing"] = missing
        out.append(a)
    return out


def collect() -> dict:
    db = _open_db()
    try:
        return _collect(db)
    finally:
        db.close()


def _collect(db) -> dict:
    contacts = []

    cfg = settings.load()
    rows = db.db.execute(
        """SELECT handle, sightings, first_seen, last_seen
           FROM players ORDER BY last_seen DESC LIMIT ?""",
        (int(cfg.get("max_contacts", 120)),)).fetchall()
    # Plus anyone with an audit waiting who has dropped out of that window.
    # The header counts every audit, and clicking the count lists the players
    # they belong to - a list that could only reach the most recent contacts
    # would show "to audit 3" over two tiles, missing the oldest read - the
    # one likeliest to be forgotten. Marked, so the normal grid
    # still draws exactly the window it always did.
    window = {r["handle"] for r in rows}
    rows = list(rows) + [r for r in db.db.execute(
        """SELECT handle, sightings, first_seen, last_seen FROM players
           WHERE handle IN (SELECT handle FROM audits)
           ORDER BY last_seen DESC""") if r["handle"] not in window]
    # Only the profiles for the contacts actually being drawn.
    cache = _rsi_cache([r["handle"] for r in rows])
    failures = _rsi_failures([r["handle"] for r in rows])
    # Read once, from the settings this page has already loaded, and handed to
    # every assess() below. Left to itself, assess() asks
    # watchlist.variants_enabled(), which reads settings.json - so building one
    # page opened and parsed that file 120 times, for 46-80 ms of a 115 ms
    # build. The setting still takes effect on the next poll, because the next
    # poll loads the settings again; it just cannot change mid-page now, which
    # it never could anyway.
    variants = bool(cfg.get("watchlist_variants", True))
    # What you filled in by hand, read once per page - see reputation's
    # org_manual / player_manual. Used only where a lookup left a gap.
    org_manual = db.org_manual()
    enl_manual = db.manual_enlisted([r["handle"] for r in rows])

    for row in rows:
        handle = row["handle"]
        a = db.assess(handle, variants=variants)
        last_range = db.db.execute(
            """SELECT range_km FROM sightings WHERE handle=?
               ORDER BY ts DESC LIMIT 1""", (handle,)).fetchone()

        prof = cache.get(handle.upper(), {})
        unknown_focus = set(prof.get("_unknown_focus") or ())
        orgs = []
        for o in prof.get("orgs", []):
            sid_u = (o.get("sid") or "").upper()
            focuses = [f for f in (o.get("primary_focus"), o.get("secondary_focus")) if f]
            # Your entry fills a gap and never overrides: once a lookup brings
            # the org's own focus or name, that is what shows.
            man = org_manual.get(sid_u) or {}
            focus_manual = not focuses and bool(
                man.get("primary_focus") or man.get("secondary_focus"))
            if focus_manual:
                focuses = [f for f in (man.get("primary_focus"),
                                       man.get("secondary_focus")) if f]
            name_manual = not o.get("name") and bool(man.get("name"))
            piracy = any(f.lower() in rsi.PIRACY_FOCUS for f in focuses)
            adjacent = [f for f in focuses if f.lower() in rsi.ADJACENT_FOCUS]
            if (o.get("archetype") or "").lower() in rsi.PIRACY_ARCHETYPE:
                adjacent.append(o.get("archetype"))
            orgs.append({
                "sid": o.get("sid", ""),
                "name": o.get("name") or (man.get("name") if name_manual else ""),
                "focus_manual": focus_manual, "name_manual": name_manual,
                "members": o.get("members", 0),
                "archetype": o.get("archetype", ""),
                "focuses": focuses, "piracy": piracy, "adjacent": adjacent,
                "is_main": o.get("is_main", False),
                "url": rsi.org_url(o.get("sid", "")),
                # Nobody here could say what this org declares. Rendered as
                # such - a blank focus reads as "nothing to see".
                "focus_unknown": sid_u in unknown_focus and not focus_manual,
            })
        # Partial only while an org is STILL missing its focus. One you have
        # filled in is described; the lookup is still retried (_needs_lookup
        # reads the stored record, not this), so RSI's answer replaces yours.
        partial_left = bool(prof.get("_partial")) and any(
            o["focus_unknown"] for o in orgs)

        enlisted_days = None
        if prof:
            c = rsi.Citizen(handle=handle, enlisted=prof.get("enlisted", ""))
            enlisted_days = c.enlisted_days
        enlisted_manual = enlisted_days is None and handle.upper() in enl_manual
        if enlisted_manual:
            enlisted_days = _days_since(enl_manual[handle.upper()])

        yours = " (your entry)"
        flags = []
        for o in orgs:
            if o["piracy"]:
                flags.append(f"PIRACY · {o['sid']}" + (yours if o["focus_manual"] else ""))
            for adj in o["adjacent"]:
                flags.append(f"{adj} · {o['sid']}" + (yours if o["focus_manual"] else ""))
        if enlisted_days is not None and enlisted_days < 90:
            flags.append(f"new account · {enlisted_days}d"
                         + (yours if enlisted_manual else ""))

        state = "clean"
        if a.alert or (a.score > 0):
            state = "known"
        elif flags:
            state = "flagged"
        elif not prof or partial_left:
            # A partial profile has not been checked for piracy, so it does
            # not get the green stripe of one that was.
            state = "unchecked"

        contacts.append({
            "handle": handle,
            "in_window": handle in window,
            "range_km": last_range["range_km"] if last_range else None,
            "sightings": row["sightings"] or 0,
            "first_seen": row["first_seen"],
            "last_seen": row["last_seen"],
            "score": round(a.score, 2),
            # NOT a.lines(). That summarises three things, and the tile now
            # renders two of them better than the summary did:
            #
            #   "1x unprovoked_attack, most recent 2d ago"  -> the reports box,
            #       which lists every occurrence with its actual note
            #   "seen 9x today"                             -> the meta row
            #   "member of flagged org EXMPLORG"            -> nowhere else
            #
            # Only the third survives. a.lines() itself is untouched: the
            # console and the spoken alert both use it, and they have no boxes
            # to render into.
            "reasons": [f"member of flagged org {sid}"
                        + (f" — {note}" if note else "")
                        + reputation.manual_line(sid, a.manual_orgs, a.manual_by)
                        for sid, note in a.flagged_orgs]
                       + [reputation.former_line(sid, since)
                          for sid, since in a.former_orgs],
            # STRUCTURED, not folded into reasons above. A claim from someone
            # else's list has to be visibly someone else's on the tile - whose
            # list, what they said, how sure they were - and a flat string
            # cannot be styled into saying that.
            "watchlist": [{"source": h.source, "claim": h.claim,
                           "confidence": h.confidence, "match": h.match,
                           "posted_ts": h.posted_ts, "by": h.reported_by,
                           "url": h.msg_url, "listed_as": h.handle}
                          for h in a.watchlist],
            "reports": [{"id": r.id, "category": r.category, "note": r.note,
                         "age_days": int(r.age_days), "weight": round(r.weight, 2),
                         "source": r.source} for r in a.reports],
            "pending": sum(1 for r in a.reports if r.category == reputation.PENDING),
            "audits": _audits_with_files(db, handle),
            "enlisted_days": enlisted_days,
            "redacted": bool(prof.get("redacted")),
            # Three states, not two. A cache row means "we asked"; it does NOT
            # mean the account exists - the cache stores confirmed absences too,
            # and 46 of its rows are handles the API says do not exist. Treating
            # any row as a profile reported a misread as a looked-up player.
            "known_profile": bool(prof),
            # Only while there is no profile: an answer supersedes a failure,
            # and put_citizen() deletes the row anyway.
            # Present with or without a profile now: with one, it is the
            # reason an expired profile could not be refreshed.
            "lookup_failed": failures.get(handle.upper()),
            # Past its expiry, the stored profile is still shown - it is most
            # of the answer, and on a day the service is down it is the only
            # one - but the tile says how old it is.
            "profile_age_days": (int((time.time() - prof["_stored_at"]) / 86400)
                                 if prof.get("_stored_at") else None),
            "profile_stale": bool(prof.get("_stored_at")) and (
                time.time() - prof["_stored_at"] >
                (rsi.CITIZEN_TTL if prof.get("exists", True) else rsi.MISS_TTL)),
            "verified": bool(prof.get("exists")),
            # SENTRY's profile, shown though an org's focus could not be had -
            # the reason why, or None. See rsi.Cache.put_partial().
            "profile_partial": prof.get("_partial") if partial_left else None,
            "enlisted_manual": enlisted_manual,
            "enlisted_iso": enl_manual.get(handle.upper()) if enlisted_manual else None,
            # A lookup was tried and did not bring an enlistment date: the one
            # case the detail window offers to take yours.
            "enlisted_missing": enlisted_days is None and bool(
                prof or failures.get(handle.upper())),
            # For the detail window's roster controls.
            "flagged_sids": [sid for sid, _ in a.flagged_orgs],
            "manual_sids": sorted(a.manual_orgs),
            # sid -> the org mate whose export added them; absent = you.
            "manual_by": a.manual_by,
            "orgs": orgs,
            "flags": flags,
            "state": state,
            "profile_url": rsi.profile_url(handle),
            "lookup_url": rsi.lookup_url(handle),
        })

    stats = db.stats()
    stats["pending"] = len(db.pending_reports())
    stats["audits"] = len(db.open_audits())
    stats["focuses"] = list(rsi.FOCUSES)
    stats["api_key"] = _key_status()
    # A 0.1.x install's data this portable copy is not using - see paths.
    old = paths.unused_old_data()
    stats["old_data"] = {"old": str(old), "here": str(paths.DATA_ROOT)} if old else None
    # The part of that count no tile can show: audits whose player row is
    # gone. Same test as orphan_audits(), as one query instead of one a row.
    stats["audits_orphaned"] = db.db.execute(
        """SELECT COUNT(*) FROM audits
           WHERE handle NOT IN (SELECT handle FROM players)""").fetchone()[0]
    stats["lookups"] = dict(LOOKUPS)
    stats["lookup_done"] = dict(LOOKUP_DONE)
    # The schema travels with the state so the settings form is rendered from
    # it, not hand-written: adding a Field in settings.py puts a control in the
    # UI with no changes here.
    stats["settings"] = cfg
    stats["settings_schema"] = settings.describe()
    stats["disk"] = _disk_usage()
    stats["ignored"] = db.ignored_list()
    # Two different things, and they are not interchangeable.
    #   categories  the ACTIVE flags, for the dropdowns
    #   flagmap     EVERY flag ever defined, keyed, so a report filed under a
    #               reason that has since been retired still renders with its
    #               proper name and colour instead of a raw key
    stats["categories"] = [
        {"key": f.key, "label": f.label, "color": f.color,
         "polarity": f.polarity} for f in flags_mod.active()]
    stats["flag_slots"] = flags_mod.SLOTS
    stats["flagmap"] = {
        f.key: {"label": f.label, "color": f.color, "polarity": f.polarity,
                "say": f.say, "retired": f.retired}
        for f in flags_mod.all_flags()}
    # Carries roster_ts and reaches, not just the SID: a flag with no roster
    # reaches only players whose profile names the org, and the profile
    # endpoint gives a main org only - so an unpulled flag on an affiliate org
    # is a row that looks active and does nothing. The panel says which.
    stats["flagged_orgs_list"] = db.flagged_orgs()
    stats["rosters"] = dict(ROSTERS)
    stats["org_jobs"] = {sid: {k: v for k, v in j.items() if k != "stop"}
                         for sid, j in ORG_JOBS.items()}
    # Opened directly, NOT through watchlist.shared(). The panel must still
    # render when the feature is switched off - otherwise the toggle hides the
    # only screen that could turn it back on, along with the Forget button.
    stats["folders"] = _folder_targets()
    stats["watchlist_on"] = watchlist.enabled()
    stats["watchlists"] = []
    try:
        if watchlist.DB_PATH.exists():
            wl = watchlist.Watchlist()
            try:
                # Per-source counts ride along on sources(); no global
                # figure, because the panel shows each list against the others.
                stats["watchlists"] = wl.sources()
                stats["watchlist_orgs"] = wl.orgs_flagged()
            finally:
                wl.close()
    except Exception:
        pass
    return {"contacts": contacts, "stats": stats, "now": time.time()}


# The page, as three ordinary files in ui/ rather than one Python string.
#
# It was a 2,350-line raw string literal here - 64% of this file - and nothing
# looked inside it: py_compile is happy with JavaScript that will not parse.
# Two bugs shipped that way, a colour written `#44soc6f7f` and a backslash-n
# that became a real newline inside a JS string and blanked the whole settings
# panel. As files, an editor highlights them, a linter can read them, and
# check_page.py checks them directly.
#
# Read from disk on every request rather than cached: they are small, the UI
# asks for them once per page load, and editing one and pressing refresh is
# how anyone works on them. paths.resource() finds them beside the script
# from source and inside the bundle when frozen - sc-watch.spec ships ui/.
UI_DIR = paths.resource("ui")
STATIC = {
    "/": ("index.html", "text/html; charset=utf-8"),
    "/index.html": ("index.html", "text/html; charset=utf-8"),
    "/app.css": ("app.css", "text/css; charset=utf-8"),
    "/app.js": ("app.js", "text/javascript; charset=utf-8"),
}


class _DataVersion:
    """A number that changes whenever anyone commits to one SQLite file.

    SQLite's own answer to "has this database changed since I last looked":
    PRAGMA data_version moves whenever ANOTHER connection commits - another
    thread, or the watcher in its own process.

    The stream used to watch the file's modification time instead, and that
    stopped working the day the databases moved to WAL. Under WAL a commit is
    appended to `<name>-wal`; the main file is only rewritten at a checkpoint,
    and the watcher holding its connection open puts checkpoints off. Measured
    with a watcher process writing: the old check noticed 0 of 8 sightings,
    so nothing reached an open tab until it was refreshed. This notices 8 of
    8, in any journal mode, without relying on how Windows updates timestamps.

    Read-only, opened lazily, and never creates the file: a fresh install has
    no database until the watcher first writes one.
    """

    def __init__(self, path: Path):
        self.path = path
        self.con = None

    def get(self):
        if self.con is None:
            if not self.path.exists():
                return None
            try:
                # as_uri(): a folder called `a#b` or `50%off` must not be read
                # as URI syntax.
                self.con = sqlite3.connect(self.path.resolve().as_uri() + "?mode=ro",
                                           uri=True, check_same_thread=False)
            except sqlite3.Error:
                return None
        try:
            return self.con.execute("PRAGMA data_version").fetchone()[0]
        except sqlite3.Error:
            self.close()            # file replaced or removed; reopen next time
            return None

    def close(self):
        if self.con is not None:
            try:
                self.con.close()
            except sqlite3.Error:
                pass
            self.con = None


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass  # a request log per poll is noise

    def finish(self):
        # Each connection is served on its own thread, and a thread that
        # touched a shared handle - the watchlist - now has its own SQLite
        # connection under it (see dbopen). Closed here, as the thread ends,
        # rather than whenever the garbage collector gets to it.
        try:
            super().finish()
        finally:
            dbopen.release_thread()

    # ----------------------------------------------------------------------
    # Who is allowed to talk to this server
    #
    # Binding to 127.0.0.1 keeps other machines out. It does NOT keep other
    # WEB PAGES out: any site open in the browser can POST to
    # http://127.0.0.1:8731/api/forget while sc-watch is running, and the
    # request arrives with the user's own privileges. The reply is unreadable
    # cross-origin, but the DELETE already happened - and this server's write
    # endpoints erase reputation history that cannot be recovered.
    #
    # Two checks close it, and neither costs a legitimate request anything:
    #
    #   HOST     A cross-origin fetch carries the attacker's hostname here, so
    #            requiring a loopback Host also defeats DNS rebinding - the
    #            attack that otherwise lets a remote page READ /api/state,
    #            which is the whole reputation database.
    #
    #   CONTENT-TYPE  A form post or a no-preflight fetch can only send
    #            text/plain, form-urlencoded or multipart. Requiring JSON
    #            forces a CORS preflight, and the preflight fails because this
    #            server sends no Access-Control-Allow-Origin. The UI's own
    #            fetches already send application/json, so nothing changes for
    #            them.
    ALLOWED_HOSTS = ("127.0.0.1", "localhost", "[::1]")

    def _host_ok(self) -> bool:
        host = (self.headers.get("Host") or "").rsplit(":", 1)[0]
        return host in self.ALLOWED_HOSTS

    def _guard(self, *, json_body: bool) -> bool:
        """True if this request may proceed. Answers it itself if not."""
        if not self._host_ok():
            self.send_error(403, "not a loopback host")
            return False
        if json_body:
            ctype = (self.headers.get("Content-Type") or "").split(";")[0]
            if ctype.strip().lower() != "application/json":
                self.send_error(415, "expected application/json")
                return False
        return True

    def _send(self, body: bytes, ctype: str):
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def _stream(self):
        """Server-sent events: push when the data actually changes.

        Polling cost nothing in CPU - collect() is 19ms and the 2s poll was
        under 1% of a core - but it moved 80 MB/hour of identical JSON per open
        tab, and it caused every render race this UI has had: the grid was
        rebuilt on a timer whether or not anything had changed, which is what
        kept wiping half-typed notes, open dropdowns and armed buttons.

        Change is detected with PRAGMA data_version on the two databases (see
        _DataVersion) and the mtime of settings.json - so the server still
        polls, but for microseconds instead of 19ms, and sends nothing when
        nothing happened. It used the databases' mtimes too, until WAL made
        those stand still while the watcher was writing.

        A comment line every KEEPALIVE seconds keeps proxies and idle-connection
        timeouts from closing it. EventSource reconnects on its own if it does.
        """
        POLL, KEEPALIVE = 0.4, 15.0
        try:
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Cache-Control", "no-store")
            self.send_header("Connection", "keep-alive")
            self.send_header("X-Accel-Buffering", "no")
            self.end_headers()
        except Exception:
            return

        # This stream's own read-only connections, closed when the tab goes.
        watched = [_DataVersion(reputation.DB_PATH), _DataVersion(rsi.CACHE_PATH)]

        def stamp():
            # settings.json is in here too: a setting changed in one tab has to
            # reach any other open tab, and the settings panel is part of the
            # state payload. It is written by os.replace, so its mtime is a
            # fine signal - the WAL problem is only the databases'.
            out = [v.get() for v in watched]
            try:
                out.append(settings.CONFIG_PATH.stat().st_mtime_ns)
            except OSError:
                out.append(0)
            return tuple(out)

        try:
            self._stream_loop(stamp, POLL, KEEPALIVE)
        finally:
            for v in watched:
                v.close()

    def _stream_loop(self, stamp, POLL, KEEPALIVE):
        last, last_beat = None, 0.0
        while not self.server._shutting_down:
            now = time.time()
            cur = stamp()
            try:
                if cur != last:
                    last = cur
                    payload = json.dumps(collect())
                    self.wfile.write(f"data: {payload}\n\n".encode())
                    self.wfile.flush()
                    last_beat = now
                elif now - last_beat >= KEEPALIVE:
                    self.wfile.write(b": keepalive\n\n")
                    self.wfile.flush()
                    last_beat = now
            except (BrokenPipeError, ConnectionResetError, OSError):
                return          # tab closed or reloaded; _stream closes the connections
            except Exception as exc:
                try:
                    self.wfile.write(
                        f"data: {json.dumps({'error': str(exc)})}\n\n".encode())
                    self.wfile.flush()
                except Exception:
                    return
            time.sleep(POLL)

    def _send_file(self, rel: str):
        """Serve one audit image.

        Only from audit/, only a bare filename, and only image extensions -
        the path arrives from a URL, so it is treated as hostile even on a
        loopback-bound server.
        """
        name = Path(rel).name
        if name != rel or not name or name.startswith("."):
            self.send_error(404)
            return
        if Path(name).suffix.lower() not in (".png", ".jpg", ".jpeg"):
            self.send_error(404)
            return
        f = (HERE / "audit" / name).resolve()
        if f.parent != (HERE / "audit").resolve() or not f.is_file():
            self.send_error(404)
            return
        ctype = "image/png" if f.suffix.lower() == ".png" else "image/jpeg"
        self._send(f.read_bytes(), ctype)

    def do_GET(self):
        if not self._guard(json_body=False):
            return
        path = urlparse(self.path).path
        if path.startswith("/audit/"):
            self._send_file(path[len("/audit/"):])
            return
        if path == "/api/inputs":
            # What is held RIGHT NOW, as binding strings. The settings form
            # polls this while you hold a button, which is the only practical
            # way to identify one of four joysticks that all report the same
            # generic driver name.
            try:
                devs = inputs_mod.devices()
                mods = {0xA0: "lshift", 0xA1: "rshift", 0xA2: "lctrl",
                        0xA3: "rctrl", 0xA4: "lalt", 0xA5: "ralt"}
                # Holding RCTRL also sets the GENERIC VK_CONTROL (0x11), and
                # the same for shift and alt. Reporting both would put
                # "rctrl+ctrl" in every combination, so the generic three are
                # never listed - the specific side is strictly more precise.
                generic = {0x10, 0x11, 0x12}
                held = [n for vk, n in mods.items() if inputs_mod.key_down(vk)]
                keys = [name for name, vk in inputs_mod.VK_CODES.items()
                        if vk not in mods and vk not in generic
                        and inputs_mod.key_down(vk)]
                buttons = []
                for d in devs:
                    mask = inputs_mod.buttons_down(d.index)
                    for b in range(32):
                        if mask & (1 << b):
                            buttons.append({"index": f"joy{d.index}.b{b+1}",
                                            "stable": f"{d.ident}.b{b+1}"})
                out = {"ok": True,
                       "devices": [{"label": d.describe(), "ident": d.ident,
                                    "index": d.index} for d in devs],
                       "mods": held, "keys": keys, "buttons": buttons}
            except Exception as exc:
                out = {"ok": False, "error": f"{type(exc).__name__}: {exc}"}
            self._send(json.dumps(out).encode(), "application/json")
            return
        if path == "/api/stream":
            self._stream()
            return
        if path == "/api/contacts":
            try:
                body = json.dumps(collect()).encode()
            except Exception as exc:
                body = json.dumps({"error": f"{type(exc).__name__}: {exc}"}).encode()
            self._send(body, "application/json")
        elif path in STATIC:
            self._send_static(*STATIC[path])
        else:
            self.send_error(404)

    def _send_static(self, name: str, ctype: str):
        """One of the page's own files. Only names from STATIC reach here, so
        there is no path from the URL to anywhere else on disk."""
        try:
            body = (UI_DIR / name).read_bytes()
        except OSError:
            # Say which file and where it was looked for. A packaging mistake
            # otherwise shows up as a blank page with nothing to go on.
            self.send_error(500, f"ui/{name} is missing from this install "
                                 f"(looked in {UI_DIR})")
            return
        self._send(body, ctype)

    def _lookup(self, payload: dict) -> dict:
        """Look one handle up on RSI, and optionally keep it.

        Synchronous, unlike lookup_now(): the caller is a person waiting on a
        result they asked for, and the answer IS the response. The server is
        threaded, so the ~1s rate limit blocks this request only.

        Looking up and storing are separate steps. Most lookups are "who is
        this?" and should leave nothing behind - a handle typed to check it is
        not a sighting, and writing one would put a person in the database you
        never actually met.
        """
        try:
            handle = _clean_handle(payload.get("handle"))
        except ValueError as exc:
            return {"ok": False, "error": str(exc)}
        # `fresh` comes from the look-up button on a tile: ask RSI now, not
        # the cache. That is the whole point of pressing it on a contact whose
        # last lookup failed, or that the cache remembers as "no account".
        fresh = bool(payload.get("fresh"))
        client = _client()
        try:
            with rsi.why("lookup-window"):
                c = client.citizen(handle, fresh=fresh)
        except Exception as exc:
            _note_lookup_failure(client, handle.upper(), exc)
            return {"ok": False, "handle": handle.upper(),
                    "error": f"RSI lookup failed: {rsi.describe_failure(exc)}"}

        h = c.handle.upper()
        out = {"ok": True, "handle": h, "exists": bool(c.exists), "fresh": fresh,
               "unverified": bool(getattr(c, "unverified", False)),
               "moniker": c.moniker or "", "enlisted": c.enlisted or "",
               "enlisted_days": c.enlisted_days, "redacted": bool(c.redacted),
               "profile_url": rsi.profile_url(h), "lookup_url": rsi.lookup_url(h),
               # Who answered, how old their copy is, and anything it could not
               # settle. A SENTRY record whose org focus could not be fetched is
               # returned for the moment and not stored - and it must SAY so, or
               # a missing PIRACY mark reads as "not a piracy org".
               "source": getattr(c, "source", None),
               "observed_days": (int((time.time() - c.fetched_at) / 86400)
                                 if c.fetched_at else None),
               "stale": getattr(c, "stale_reason", None),
               "orgs": [{"sid": o.sid, "name": o.name, "members": o.members,
                         "is_main": o.is_main,
                         "focuses": [f for f in (o.primary_focus,
                                                 o.secondary_focus) if f],
                         "focus_unknown": o.sid.upper() in
                             getattr(c, "unknown_focus", ()),
                         "url": rsi.org_url(o.sid)} for o in c.orgs]}
        if not c.exists:
            out["message"] = f"{h}: no such account"
            return out

        db = _open_db()
        try:
            out["stored"] = bool(db.db.execute(
                "SELECT 1 FROM players WHERE handle=?", (h,)).fetchone())
            if payload.get("add"):
                added = db.ensure_player(h)
                if c.orgs:
                    db.set_orgs(h, c.orgs)
                db.update_profile(h, c.moniker, c.enlisted)
                out["stored"] = True
                out["added"] = added
                out["message"] = (f"added {h}" if added
                                  else f"{h} was already stored — profile refreshed")
            elif out["stored"]:
                # Somebody already in the database: keep their record current.
                # "Nothing is stored unless you say so" is about not creating a
                # person you never met - refreshing one you did is not that, and
                # skipping it would leave an org flag unable to reach them.
                if c.orgs:
                    db.set_orgs(h, c.orgs)
                db.update_profile(h, c.moniker, c.enlisted)
                out["refreshed"] = True
                out["message"] = f"{h}: " + c.summary()
            else:
                out["message"] = f"{h}: " + c.summary()
            a = db.assess(h)
            out["score"] = round(a.score, 2)
            out["reasons"] = a.lines()
            out["flagged_orgs"] = [sid for sid, _ in a.flagged_orgs]
        finally:
            db.close()
        return out

    def _restart_watcher(self) -> dict:
        """Ask the watcher to restart itself.

        The UI cannot restart a process it did not start and does not own, so
        it leaves a note instead: the watcher checks for this file while idle,
        deletes it and re-execs. That keeps the capture device's lifecycle in
        the process that owns it - killing the watcher from here would leave
        dxcam's handle to be cleaned up by the OS.

        Nothing to clean up if no watcher is running; the file is picked up
        whenever one next starts.
        """
        try:
            RESTART_FLAG.write_text(str(time.time()), encoding="utf-8")
        except OSError as exc:
            return {"ok": False, "error": f"could not write the flag: {exc}"}
        return {"ok": True,
                "message": "asked the watcher to restart — it picks this up "
                           "while idle, within a second or two. If nothing "
                           "happens, no watcher is running."}

    def _close_req_db(self):
        """Release the per-request connection, whichever way the handler left."""
        db = getattr(self, "_req_db", None)
        if db is not None:
            self._req_db = None
            try:
                db.close()
            except Exception:
                pass

    def _open_folder(self, payload: dict) -> dict:
        """Open one of the app's own folders in the file manager.

        Takes a KEY and looks the path up in _folder_targets(). It never takes
        a path, because os.startfile() on a file executes it - so accepting
        one would turn this into a remote "run that program" button for
        anything that got a request past the loopback and Host guards.

        Re-checks is_dir() after resolving even though the table only ever
        holds directories: the folder can be deleted between the page being
        drawn and the button being pressed, and startfile on a path that no
        longer exists raises rather than saying so.
        """
        key = str(payload.get("key", ""))
        row = next((r for r in _folder_targets() if r["key"] == key), None)
        if row is None:
            return {"ok": False, "error": "not a folder this app uses"}
        p = Path(row["path"])
        if not p.is_dir():
            return {"ok": False,
                    "error": f"{row['label']} does not exist any more"}
        try:
            _reveal(p)
        except Exception as exc:
            return {"ok": False,
                    "error": f"could not open it ({type(exc).__name__})"}
        return {"ok": True, "message": f"opened {row['label']}"}

    def _share(self, path: str, payload: dict) -> dict:
        """Export your records for an org mate, or import theirs.

        The export comes back as base64 in JSON rather than as a download
        response, so it travels the same guarded POST path as everything else;
        the page turns it into a file. An import is two calls - preview, then
        import - and the file is sent with both, so nothing is held between
        them and nothing is written until the second.
        """
        db = _open_db()
        try:
            if path == "/api/share/export":
                doc = share.build(db, payload.get("name", ""))
                import base64
                return {"ok": True, "filename": share.filename(doc),
                        "data": base64.b64encode(share.pack(doc)).decode("ascii"),
                        "reports": len(doc["reports"]), "orgs": len(doc["orgs"]),
                        "roster": len(doc["roster"]), "from": doc["from"]}
            if path == "/api/share/sources":
                return {"ok": True, "sources": share.sources(db)}
            if path == "/api/share/remove":
                out = share.remove_source(db, str(payload.get("install", "")))
                return {"ok": True, **out,
                        "message": f"removed everything from {out['name']}: "
                                   f"{out['reports']} report(s), {out['roster']} "
                                   f"roster member(s)"}
            doc = share.unpack_b64(payload.get("data", ""))
            if path == "/api/share/preview":
                return {"ok": True, **share.preview(db, doc)}
            inc = payload.get("include") or {}
            out = share.apply(db, doc, reports=inc.get("reports", True) is not False,
                              orgs=inc.get("orgs", True) is not False,
                              roster=inc.get("roster", True) is not False)
            bits = [f"{out['reports_added']} new report(s)"]
            if out["reports_updated"]:
                bits.append(f"{out['reports_updated']} updated")
            if out["reports_dropped"]:
                bits.append(f"{out['reports_dropped']} they had deleted removed")
            if out["orgs_flagged"]:
                bits.append(f"flagged {', '.join(out['orgs_flagged'])} - pull their "
                            f"member lists in the Orgs tab")
            if out["roster_added"]:
                bits.append(f"{out['roster_added']} roster member(s)")
            return {"ok": True, **out,
                    "message": f"imported from {out['from']}: " + "; ".join(bits)}
        except share.ShareError as exc:
            return {"ok": False, "error": str(exc)}
        finally:
            db.close()

    def _speech(self, path: str, payload: dict) -> dict:
        """The spoken-alert phrases: read them, save them, or hear one.

        Read and saved on request, not sent with every stream frame - the
        panel is drawn when opened, like the rest of Settings. Every phrase is
        checked against the placeholders it can use before anything is
        written, so a typo is refused here instead of turning an alert into
        silence in the middle of a fight.
        """
        import audio
        cfg = audio.AudioConfig.load()
        if path == "/api/audio/preview":
            key = str(payload.get("key", ""))
            text = str(payload.get("text", ""))
            err = audio.check_phrase(key, text)
            if err:
                return {"ok": False, "error": err}
            spoken = audio.sample(key, text, cfg.speech,
                                  bool(settings.load().get("speak_handles", True)))
            if not spoken:
                return {"ok": True, "said": "",
                        "message": "that is blank - only the tone plays"}
            voice, why = _preview_voice()
            if voice is None:
                return {"ok": False, "said": spoken,
                        "error": f"speech is not working: {why}"}
            voice.say(spoken)
            return {"ok": True, "said": spoken}

        values = payload.get("values")
        if isinstance(values, dict):
            errors = {}
            for key, text in values.items():
                err = audio.check_phrase(str(key), str(text))
                if err:
                    errors[key] = err
            if errors:
                return {"ok": False, "errors": errors,
                        "error": f"{len(errors)} phrase(s) not saved - see below"}
            for key, text in values.items():
                cfg.speech[str(key)] = str(text).strip()
            muted = payload.get("muted")
            if isinstance(muted, list):
                cfg.muted = [k for k in muted if isinstance(k, str) and k in audio.ALLOWED]
            cfg.save()
        return {"ok": True,
                "phrases": [{"key": k, "group": g, "title": t, "when": w,
                             "placeholders": list(ph), "default": d,
                             "value": cfg.speech.get(k, d), "on": k not in cfg.muted}
                            for k, g, t, w, ph, d in audio.PHRASES]}

    def _flags(self, payload: dict) -> dict:
        """Save the flag definitions. Never deletes a key - see flags.save().

        Validation lives in flags._coerce: a bad colour becomes the neutral
        grey, an unknown polarity becomes 'negative', and a reserved key is
        dropped outright. A malformed row costs that row, not the file.
        """
        rows = payload.get("flags")
        if not isinstance(rows, list):
            return {"ok": False, "error": "expected a list of flags"}
        if len(rows) > flags_mod.SLOTS * 4:
            return {"ok": False, "error": "too many flags"}
        try:
            saved = flags_mod.save(rows)
        except Exception as exc:
            return {"ok": False, "error": f"{type(exc).__name__}: {exc}"}
        active = [f for f in saved if not f.retired and f.label]
        return {"ok": True, "active": len(active), "total": len(saved),
                "message": f"saved {len(active)} flag(s)"}

    def _watchlist(self, path: str, payload: dict) -> dict:
        """List the imported watchlists, silence one, or delete one outright.

        Opened directly rather than through watchlist.shared(), because this
        panel has to keep working when the feature is switched OFF - otherwise
        turning it off would hide the only screen that can turn it back on,
        and hide the Forget button with it.
        """
        try:
            wl = watchlist.Watchlist()
        except Exception as exc:
            return {"ok": False, "error": f"{type(exc).__name__}: {exc}"}
        try:
            if path == "/api/watchlist/list":
                w, n = wl.reason_coverage()
                return {"ok": True, "enabled": watchlist.enabled(),
                        "sources": wl.sources(), "orgs": wl.orgs_flagged(),
                        "with_reason": w, "entries": n}

            if path in ("/api/watchlist/preview", "/api/watchlist/import"):
                text = str(payload.get("text", ""))[:200_000]
                if path == "/api/watchlist/preview":
                    # Shown before anything is stored. Importing a list of
                    # accusations sight-unseen is how a typo becomes a flag on
                    # a stranger, so the parse is always inspectable first.
                    pv = watchlist.parse_lines(text)
                    return {"ok": True, "entries": pv["entries"][:400],
                            "n": len(pv["entries"]), "orgs": pv["orgs"],
                            "skipped": pv["skipped"][:20],
                            "skipped_n": len(pv["skipped"])}
                name = str(payload.get("name", "")).strip()[:80]
                if not name:
                    return {"ok": False, "error": "give the list a name"}
                unsure = payload.get("unsure") is True
                out = watchlist.import_text(
                    name, text,
                    url=str(payload.get("url", "")).strip()[:300],
                    note=str(payload.get("note", "")).strip()[:500],
                    reported_by=str(payload.get("by", "")).strip()[:80],
                    confidence=(watchlist.UNCONFIRMED if unsure
                                else watchlist.ASSERTED))
                if out.get("ok"):
                    out["message"] = (
                        f"imported {out['added']} handle(s)"
                        + (f", {out['skipped_n']} line(s) skipped"
                           if out.get("skipped_n") else ""))
                return out

            sid = int(payload.get("id", 0))
            if path == "/api/watchlist/enable":
                on = payload.get("on") is True
                wl.set_enabled(sid, on)
                return {"ok": True, "message":
                        ("source on" if on else "source silenced - kept, "
                                                "not deleted")}

            if path == "/api/watchlist/forget":
                # Deliberately unguarded by a name match. This is somebody
                # else's accusations about real people; making it hard to
                # delete would be the wrong side to err on.
                n = wl.forget_source(sid)
                return {"ok": True, "message": f"forgotten - {n} claims deleted"}
            return {"ok": False, "error": "unknown watchlist action"}
        finally:
            wl.close()

    def _org(self, path: str, payload: dict) -> dict:
        """Flag an org, unflag it, or pull its member list.

        Flagging pulls the roster straight away unless told not to. A flag on
        its own reaches only players whose profile names the org, and the
        profile endpoint returns a main org only - so on the affiliate case the
        feature exists for, a flag with no roster silently reaches nobody. The
        pull is the point, not an extra.
        """
        try:
            sid = _clean_sid(payload.get("sid"))
        except ValueError as exc:
            return {"ok": False, "error": str(exc)}
        db = _open_db()
        try:
            return self._org_with(db, path, payload, sid)
        finally:
            db.close()

    def _org_with(self, db, path: str, payload: dict, sid: str) -> dict:
        flagged = {o["sid"] for o in db.flagged_orgs()}
        if path in ("/api/org/members", "/api/org/lookup", "/api/org/stop",
                    "/api/org/member/add", "/api/org/member/remove") \
                and sid not in flagged:
            return {"ok": False, "error": f"{sid} is not flagged"}
        if path == "/api/org/member/add":
            try:
                h = _clean_handle(payload.get("handle"))
            except ValueError as exc:
                return {"ok": False, "error": str(exc)}
            out = db.add_member_manually(sid, h, str(payload.get("note", "")))
            if out.get("ok"):
                out["message"] = {
                    "added": (f"added {h} to {sid} — their record now carries "
                              f"the flag" if out["known"] else
                              f"added {h} to {sid} — not met yet; flagged the "
                              f"moment you do"),
                    "listed": (f"{h} is already on {sid}'s public roster — now "
                               f"kept by you too, so a refresh never drops them"),
                    "back": (f"{h} had dropped off {sid}'s roster — back on it, "
                             f"kept by you"),
                    "updated": f"updated your note on {h}",
                }[out["state"]]
            return out
        if path == "/api/org/member/remove":
            try:
                h = _clean_handle(payload.get("handle"))
            except ValueError as exc:
                return {"ok": False, "error": str(exc)}
            out = db.remove_manual_member(sid, h)
            if out.get("ok"):
                out["message"] = (f"took {h} off {sid}" if out["state"] == "removed"
                                  else f"{h} is on {sid}'s public roster, so stays "
                                       f"a member — your note was removed")
            return out
        if path == "/api/org/manual":
            # Any org, flagged or not: this is filling in what a lookup could
            # not get, and that happens to every kind of org.
            prim = str(payload.get("primary", "")).strip()
            sec = str(payload.get("secondary", "")).strip()
            for f in (prim, sec):
                if f and f not in rsi.FOCUSES:
                    return {"ok": False, "error": f"{f[:24]!r} is not an RSI org focus"}
            if prim and prim == sec:
                sec = ""
            name = str(payload.get("name", "")).strip()[:80]
            kept = db.set_org_manual(sid, name, prim, sec)
            return {"ok": True, "sid": sid, "manual": kept,
                    "message": (f"saved your entry for {sid}" if kept
                                else f"cleared your entry for {sid}")}
        if path == "/api/org/members":
            return org_members(sid)
        if path == "/api/org/lookup":
            return lookup_members_now(sid)
        if path == "/api/org/stop":
            job = ORG_JOBS.get(sid)
            if job and job.get("running"):
                job["stop"] = True
                return {"ok": True, "sid": sid, "message": "stopping after this one"}
            return {"ok": True, "sid": sid, "message": "nothing running"}

        if path == "/api/org/unflag":
            job = ORG_JOBS.pop(sid, None)
            if job:
                job["stop"] = True
            db.unflag_org(sid)
            ROSTERS.pop(sid, None)
            try:
                rsi.Cache().drop_members(sid)
            except Exception:
                pass            # the unflag stands; only a stale list is left
            return {"ok": True, "sid": sid, "message": f"unflagged {sid}"}

        if path == "/api/org/flag":
            note = str(payload.get("note", "")).strip()[:500]
            name = ""
            # The org's display name is worth having on the row, but a failed
            # lookup must not stop the flag being recorded - the flag is the
            # user's judgement and the name is decoration. org_name(), not
            # org(): org() wants the FOCUS, and dropped SENTRY's answer - name
            # and all - for an org it had not read the focus of yet. One that
            # comes back blank is filled in by the background check.
            try:
                client = _client()
                if client.enriches:
                    with rsi.why("flag"):
                        name = client.org_name(sid)
            except Exception:
                pass
            db.flag_org(sid, note, name)
            if payload.get("roster") is False:
                return {"ok": True, "sid": sid,
                        "message": f"flagged {sid} with no member list - it "
                                   f"reaches only players whose profile names it"}
            return fetch_roster_now(sid)

        # /api/org/roster
        if sid not in flagged:
            return {"ok": False, "error": f"{sid} is not flagged"}
        return fetch_roster_now(sid)

    def _maintenance(self, payload: dict) -> dict:
        """Run one housekeeping job and describe what it did.

        Every job defaults to a DRY RUN and reports what it would do; the UI
        shows that list and asks before sending apply=true. These jobs delete
        things, and a button that deletes 21 database rows on the first click
        is a button someone will press by accident.
        """
        job = str(payload.get("job", ""))
        # `is True`, not bool(): these jobs delete rows and files, and a
        # truthy string like "maybe" arriving from a malformed request must
        # not read as consent. Real clients send a JSON boolean.
        apply = payload.get("apply") is True
        if job == "ghosts":
            db = _open_db()
            # The page's client: its pacing, pauses and remembered answers. It
            # used to build one of its own.
            with rsi.why("ghost-check"):
                res = db.purge_ghosts(client=_client(), apply=apply)
            n = len(res["ghosts"])
            if apply:
                for g in res["ghosts"]:
                    LOOKUPS.pop(g["handle"].upper(), None)
            return {"ok": True, "job": job, "applied": apply, "count": n,
                    "items": [f"{g['handle']} · seen {g['sightings']}x"
                              for g in res["ghosts"]],
                    "kept": [f"{g['handle']} · {g['why']}"
                             for g in res["kept"]],
                    "errors": [f"{h}: {e}" for h, e in res["errors"]],
                    "message": (f"removed {n} handle(s) with no RSI account"
                                if apply else
                                f"{n} handle(s) have no RSI account")}
        if job == "backfill":
            # Contacts that met the evidence bar but have no profile. Normally
            # the watcher submits these as they happen; one can still be missed
            # if the lookup failed, the watcher stopped mid-burst, or the
            # sighting predates a change to how the bar is counted.
            import settings as _s
            need = int(_s.load().get("rsi_min_votes", 2))
            db = _open_db()
            cache = _rsi_cache()

            def partial(h):
                return bool((cache.get(h.upper()) or {}).get("_partial"))
            # The same rule the watcher gates on: max(burst votes, sightings).
            # Keyed on `sightings` alone this missed the case that prompted it -
            # a contact seen ONCE at 3/3 is corroborated, passed the live gate,
            # and was only missing because the lookup itself never landed.
            todo = [r["handle"] for r in db.db.execute(
                """SELECT p.handle, p.sightings, COALESCE(MAX(s.votes),0) AS best
                   FROM players p LEFT JOIN sightings s ON s.handle = p.handle
                   GROUP BY p.handle
                   HAVING p.sightings >= ? OR COALESCE(MAX(s.votes),0) >= ?
                   ORDER BY p.sightings DESC""", (need, need)).fetchall()
                # A partial profile is shown on the tile but is not an answer:
                # an org on it still has no focus. Backfill is how it gets one.
                if r["handle"].upper() not in cache or partial(r["handle"])]
            # At most BACKFILL_MAX a run: a contact SENTRY does not have yet
            # costs one of its live RSI fetches, limited per hour, and one
            # click on a long list could spend the lot - after which SENTRY
            # fetches nobody new for anyone until the hour is up.
            later = max(0, len(todo) - BACKFILL_MAX)
            if not apply:
                return {"ok": True, "job": job, "applied": False,
                        "count": len(todo),
                        "items": [f"{h} · " + ("org focus missing" if partial(h)
                                               else "never looked up")
                                  for h in todo],
                        "kept": [], "errors": [],
                        "message": (f"{len(todo)} contact(s) corroborated "
                                    f"({need}+ votes in one read, or seen "
                                    f"{need}+ times) with no complete profile stored"
                                    + (f" - {BACKFILL_MAX} are looked up per run, "
                                       "to stay inside SENTRY's hourly limit"
                                       if later else "")
                                    if todo else
                                    "every corroborated contact has been "
                                    "looked up")}
            client = _client()
            done, ghosts, errors = 0, [], []
            for h in todo[:BACKFILL_MAX]:
                try:
                    with rsi.why("backfill"):
                        cit = client.citizen(h)
                except Exception as exc:
                    errors.append(f"{h}: {type(exc).__name__}")
                    continue
                if not cit.exists:
                    ghosts.append(f"{h} · no such account")
                    continue
                if cit.orgs:
                    db.set_orgs(h, cit.orgs)
                db.update_profile(h, cit.moniker, cit.enlisted)
                done += 1
            return {"ok": True, "job": job, "applied": True, "count": done,
                    "items": [], "kept": ghosts, "errors": errors,
                    "message": f"looked up {done} contact(s)" +
                               (f", {len(ghosts)} do not exist" if ghosts else "")
                               + (f"; {later} left - run it again in an hour"
                                  if later else "")}
        if job == "frames":
            pol = housekeeping.Policy.load()
            line = (housekeeping.run_now(apply=True, policy=pol) if apply
                    else housekeeping.summarise(
                        housekeeping.sweep(pol, apply=False), False))
            if apply:
                _disk_usage(max_age=0)      # it just changed; re-read it now
            return {"ok": True, "job": job, "applied": apply, "message": line,
                    "items": [], "kept": [], "errors": []}
        if job == "usage":
            return {"ok": True, "job": job, "applied": False,
                    "message": "", "usage": _disk_usage(),
                    "items": [], "kept": [], "errors": []}
        return {"ok": False, "error": f"unknown job {job!r}"}

    def do_POST(self):
        """Every write the UI can make.

        This started as one route - forgetting a handle the OCR invented - and
        the docstring still said so long after it had grown to fourteen. They
        are listed in ROUTES below; anything not on that list is a 404.

        The watcher is a separate process sharing the same SQLite file, so each
        request opens its own connection, does one small transaction and lets
        it close. Reachability is handled by _guard: loopback Host only, and
        JSON only, so another site open in the same browser cannot drive this.
        """
        ROUTES = ("/api/forget", "/api/flag", "/api/reason",
                  "/api/report/add", "/api/report/edit", "/api/report/delete",
                  "/api/audit/confirm", "/api/audit/dismiss",
                  "/api/settings", "/api/maintenance",
                  "/api/lookup", "/api/restart-watcher",
                  "/api/ignore", "/api/unignore",
                  "/api/org/flag", "/api/org/unflag", "/api/org/roster",
                  "/api/org/members", "/api/org/lookup", "/api/org/stop",
                  "/api/org/member/add", "/api/org/member/remove",
                  "/api/org/manual", "/api/player/enlisted",
                  "/api/watchlist/list", "/api/watchlist/enable",
                  "/api/watchlist/forget", "/api/watchlist/import",
                  "/api/watchlist/preview", "/api/open-folder",
                  "/api/support-bundle", "/api/flags",
                  "/api/audio/phrases", "/api/audio/preview",
                  "/api/share/export", "/api/share/preview",
                  "/api/share/import", "/api/share/sources",
                  "/api/share/remove", "/api/apikey", "/api/lookup/background")
        if not self._guard(json_body=True):
            return
        path = urlparse(self.path).path
        if path not in ROUTES:
            self.send_error(404)
            return
        try:
            n = int(self.headers.get("Content-Length") or 0)
            # A ceiling on what one request may make this process hold. The
            # largest real body is a share file being imported, a few hundred
            # KB; nothing legitimate comes near this.
            if n > MAX_BODY:
                self.send_error(413)
                return
            # And a floor: read(-1) reads until the client hangs up, which
            # held a request thread for as long as the client liked.
            if n < 0:
                self.send_error(400)
                return
            payload = json.loads(self.rfile.read(n) or b"{}")
            raw_handle = payload.get("handle", "")
            # Initialised because three later branches read it before any of
            # them assign it - confirm adopts the corrected name, delete adopts
            # the deleted report's owner, and the response echoes it back.
            handle = ""
            note = str(payload.get("note", "")).strip()[:500]
            category = str(payload.get("category", "")).strip()

            # Settings and maintenance do not touch a handle, so they are
            # answered before the reputation database is even opened.
            if path == "/api/settings":
                saved = settings.save(payload.get("values") or {})
                self._send(json.dumps({"ok": True, "values": saved}).encode(),
                           "application/json")
                return
            if path == "/api/maintenance":
                self._send(json.dumps(
                    self._maintenance(payload)).encode(), "application/json")
                return
            if path == "/api/lookup":
                self._send(json.dumps(self._lookup(payload)).encode(),
                           "application/json")
                return
            if path == "/api/restart-watcher":
                self._send(json.dumps(self._restart_watcher()).encode(),
                           "application/json")
                return
            if path.startswith("/api/org/"):
                self._send(json.dumps(self._org(path, payload)).encode(),
                           "application/json")
                return
            if path == "/api/lookup/background":
                try:
                    h = _clean_handle(payload.get("handle"))
                except ValueError as exc:
                    self._send(json.dumps({"ok": False, "error": str(exc)}).encode(),
                               "application/json")
                    return
                done = LOOKUP_DONE.get(h, 0)
                started = lookup_now(h, fresh=True)
                self._send(json.dumps({"ok": True, "handle": h, "started": started,
                                       "done": done}).encode(), "application/json")
                return
            if path == "/api/apikey":
                self._send(json.dumps(_save_key(payload)).encode(),
                           "application/json")
                return
            if path.startswith("/api/share/"):
                self._send(json.dumps(self._share(path, payload)).encode(),
                           "application/json")
                return
            if path in ("/api/audio/phrases", "/api/audio/preview"):
                self._send(json.dumps(self._speech(path, payload)).encode(),
                           "application/json")
                return
            if path == "/api/flags":
                self._send(json.dumps(self._flags(payload)).encode(),
                           "application/json")
                return
            if path == "/api/support-bundle":
                info = debuglog.build_bundle(
                    with_frames=payload.get("frames") is not False)
                self._send(json.dumps(info).encode(), "application/json")
                return
            if path == "/api/open-folder":
                self._send(json.dumps(self._open_folder(payload)).encode(),
                           "application/json")
                return
            if path.startswith("/api/watchlist/"):
                self._send(json.dumps(self._watchlist(path, payload)).encode(),
                           "application/json")
                return

            db = _open_db()
            # Released on every exit from here, including the early `return`s
            # that answer audit/confirm and the exception path below.
            self._req_db = db

            if path == "/api/audit/confirm":
                # The corrected name becomes a players row via _rename, so
                # it gets the same validation as any other handle.
                out = db.confirm_audit(int(payload.get("id", 0)),
                                       _clean_handle(payload.get("name")),
                                       payload.get("force") is True)
                if not out.get("ok"):
                    body = json.dumps({"ok": False, **out}).encode()
                    self._send(body, "application/json")
                    return
                handle = out.get("new", handle)
                # A confirmed name is the best identity evidence this tool
                # gets, so it is worth spending a lookup on immediately rather
                # than waiting for the handle to be seen twice more.
                lookup_now(handle)
                out["lookup"] = "started" 
            elif path == "/api/audit/dismiss":
                out = {"dismissed": db.dismiss_audit(int(payload.get("id", 0)))}
            elif path == "/api/report/delete":
                gone = db.delete_report(int(payload.get("id", 0)))
                out = {"deleted": gone}
                handle = (gone or {}).get("handle", handle)
            elif path == "/api/ignore":
                # Not a person: a salvage crate, a mission marker, anything the
                # HUD draws in the same shape as a contact. Takes the record
                # with it - it was never evidence about anybody.
                out = db.add_ignored(_clean_handle(raw_handle), note)
                handle = out["label"]
            elif path == "/api/unignore":
                out = {"unignored": db.remove_ignored(
                    str(payload.get("label", "")))}
            elif path == "/api/report/edit":
                out = {"edited": db.edit_report(
                    int(payload.get("id", 0)), category or None, note)}
            else:
                handle = _clean_handle(raw_handle)
                if path == "/api/forget":
                    out = {"removed": db.forget(handle)}
                elif path == "/api/flag":
                    # One click, no reason. The point is that it costs nothing
                    # to press while something is shooting at you.
                    db.add_report(handle, reputation.PENDING)
                    out = {"flagged": True}
                elif path == "/api/report/add":
                    db.add_report(handle, category, note)
                    out = {"added": True}
                elif path == "/api/player/enlisted":
                    iso = _clean_date(payload.get("enlisted"))
                    db.set_manual_enlisted(handle, iso)
                    out = {"enlisted": iso or None}
                else:
                    out = {"resolved": db.resolve_pending(handle, category, note)}
            body = json.dumps({"ok": True, "handle": (handle or "").upper(),
                               **out}).encode()
        except Exception as exc:
            body = json.dumps({"ok": False,
                               "error": f"{type(exc).__name__}: {exc}"}).encode()
        finally:
            self._close_req_db()
        self._send(body, "application/json")


def main():
    import argparse
    ap = argparse.ArgumentParser(description=__doc__)
    # Same rule as the watcher: saved settings are the defaults, an explicit
    # flag still wins. Before this, `port` and `open_browser` were settings
    # nothing consulted.
    cfg = settings.load()
    ap.add_argument("--port", type=int, default=int(cfg.get("port", PORT)))
    ap.add_argument("--no-browser", action="store_true",
                    default=not cfg.get("open_browser", True))
    args = ap.parse_args()

    # Python's HTTPServer sets allow_reuse_address = 1, and on Windows that
    # means a SECOND server can bind a port the first is already listening on.
    # Neither errors; requests go to whichever the OS picks, so a stale process
    # silently shadows the new one. That cost this project several confusing
    # "my change did not take effect" rounds. Off, so a second instance fails
    # loudly with the truth: something is already running here.
    class _Server(ThreadingHTTPServer):
        allow_reuse_address = False

    try:
        srv = _Server((HOST, args.port), Handler)
    except OSError as exc:
        print(f"cannot listen on {HOST}:{args.port} - {exc}")
        print("something is already using that port. If it is an old sc-watch "
              "UI, close it; otherwise change the port in Settings.")
        return 1
    srv.daemon_threads = True       # open streams must not block shutdown
    srv._shutting_down = False
    rsi.PROCESS = "page"            # names it in the SENTRY request log
    url = f"http://{HOST}:{args.port}/"
    print("=" * 58)
    print("  sc-watch UI")
    print(f"  {url}")
    if paths.FROZEN:
        print(f"  your data {paths.DATA_ROOT}")
    print("  Ctrl+C to stop")
    print("=" * 58)
    if not args.no_browser:
        threading.Timer(0.6, lambda: webbrowser.open(url)).start()
    threading.Thread(target=_focus_sweeper, args=(srv,), daemon=True,
                     name="focus-sweep").start()
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass
    # Tell open /api/stream loops to stop before closing the socket, or Ctrl+C
    # waits on whatever their sleep interval happens to be. daemon_threads makes
    # this belt-and-braces, but a clean exit beats a killed one when the UI gets
    # restarted as often as this one does.
    srv._shutting_down = True
    srv.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
