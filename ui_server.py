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

import json
import re
import sqlite3
import threading
import time
import webbrowser
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import urlparse

import housekeeping
import inputs as inputs_mod
import reputation
import rsi
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
# One client for the process, built on first use. Lookups are cached in
# rsi_cache.db, so confirming the same audit twice costs nothing, and with no
# API key this stays None and behaves exactly as before.
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
                client = rsi.Client()
                if client.enriches:
                    _verify = lambda h: client.citizen(h).exists
            except Exception:
                _verify = None
        return _verify


def _open_db():
    """A database that can answer identity questions."""
    return reputation.Database(verify=_verifier())


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


# RSI handles are 1-40 characters of letters, digits, underscore and hyphen.
# Real stored examples that must keep working: 0F-8942-TS, PANDE_MONIUM,
# GRIM-H2GOBLIN. Enforced because every write path here reaches add_report() or
# ensure_player(), both of which INSERT a players row - so an unvalidated field
# is a way to put arbitrary junk in the database. A malformed POST wrote a
# 5000-character player during this audit, and `{"handle": null}` wrote one
# called "NONE", because str(None) is a perfectly good string.
HANDLE_RE = re.compile(r"^[A-Za-z0-9_-]{1,40}$")
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


def _client() -> "rsi.Client":
    """An RSI client honouring the configured API mode.

    Built through here rather than `rsi.Client()` at each call site, because
    `rsi_mode` was a setting nothing read: three separate constructors all took
    the library default and the control in the UI did nothing.
    """
    return rsi.Client(mode=settings.load().get("rsi_mode", "auto"))


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
            rows = db.execute("SELECT handle, data FROM citizens")
        else:
            # Chunked: SQLite caps host parameters per statement (999 on older
            # builds), and a caller could hand us more handles than that.
            rows = []
            for i in range(0, len(handles), 400):
                chunk = handles[i:i + 400]
                qs = ",".join("?" * len(chunk))
                rows += db.execute(
                    f"SELECT handle, data FROM citizens WHERE handle IN ({qs})",
                    chunk).fetchall()
        for row in rows:
            try:
                out[row["handle"].upper()] = json.loads(row["data"])
            except Exception:
                pass
        db.close()
    except Exception:
        pass
    return out


# Outcome of the most recent manual lookup per handle, so a background fetch
# can report back. Bounded because it is only ever a UI hint - the real result
# is the profile landing in the cache, which the stream picks up on its own.
LOOKUPS: dict[str, str] = {}
_LOOKUPS_MAX = 40


def lookup_now(handle: str):
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
    """
    h = (handle or "").strip().upper()
    if not h:
        return

    def work():
        try:
            client = _client()
            c = client.citizen(h)
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
            LOOKUPS[h] = f"{h}: " + " | ".join(bits)
        except Exception as exc:
            LOOKUPS[h] = f"{h}: lookup failed ({type(exc).__name__})"
        finally:
            while len(LOOKUPS) > _LOOKUPS_MAX:
                LOOKUPS.pop(next(iter(LOOKUPS)))

    LOOKUPS[h] = f"{h}: looking up…"
    threading.Thread(target=work, daemon=True).start()


# Status of the most recent roster pull per org, so a background fetch can
# report back. Same shape and same reasoning as LOOKUPS above: the real result
# is rows landing in the database, and this is only the line of text that says
# what happened while you waited.
ROSTERS: dict[str, str] = {}
_ROSTERS_MAX = 20


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
        try:
            out = _open_db().fetch_roster(sid, client=_client())
            if out.get("ok"):
                ROSTERS[sid] = (
                    f"{sid}: {out['members']} member(s), reaching "
                    f"{out['reaches']} player(s) you have seen"
                    + ("  (page cap hit - roster may be partial)"
                       if out.get("capped") else ""))
            else:
                ROSTERS[sid] = f"{sid}: {out.get('error')}"
        except Exception as exc:
            ROSTERS[sid] = f"{sid}: failed ({type(exc).__name__})"
        finally:
            while len(ROSTERS) > _ROSTERS_MAX:
                ROSTERS.pop(next(iter(ROSTERS)))

    ROSTERS[sid] = f"{sid}: pulling the member list, one request a second…"
    threading.Thread(target=work, daemon=True).start()
    return {"ok": True, "sid": sid, "message": ROSTERS[sid]}


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
    contacts = []

    cfg = settings.load()
    rows = db.db.execute(
        """SELECT handle, sightings, first_seen, last_seen
           FROM players ORDER BY last_seen DESC LIMIT ?""",
        (int(cfg.get("max_contacts", 120)),)).fetchall()
    # Only the profiles for the contacts actually being drawn.
    cache = _rsi_cache([r["handle"] for r in rows])

    for row in rows:
        handle = row["handle"]
        a = db.assess(handle)
        last_range = db.db.execute(
            """SELECT range_km FROM sightings WHERE handle=?
               ORDER BY ts DESC LIMIT 1""", (handle,)).fetchone()

        prof = cache.get(handle.upper(), {})
        orgs = []
        for o in prof.get("orgs", []):
            focuses = [f for f in (o.get("primary_focus"), o.get("secondary_focus")) if f]
            piracy = any(f.lower() in rsi.PIRACY_FOCUS for f in focuses)
            adjacent = [f for f in focuses if f.lower() in rsi.ADJACENT_FOCUS]
            if (o.get("archetype") or "").lower() in rsi.PIRACY_ARCHETYPE:
                adjacent.append(o.get("archetype"))
            orgs.append({
                "sid": o.get("sid", ""), "name": o.get("name", ""),
                "members": o.get("members", 0),
                "archetype": o.get("archetype", ""),
                "focuses": focuses, "piracy": piracy, "adjacent": adjacent,
                "is_main": o.get("is_main", False),
                "url": rsi.org_url(o.get("sid", "")),
            })

        enlisted_days = None
        if prof:
            c = rsi.Citizen(handle=handle, enlisted=prof.get("enlisted", ""))
            enlisted_days = c.enlisted_days

        flags = []
        for o in orgs:
            if o["piracy"]:
                flags.append(f"PIRACY · {o['sid']}")
            for adj in o["adjacent"]:
                flags.append(f"{adj} · {o['sid']}")
        if enlisted_days is not None and enlisted_days < 90:
            flags.append(f"new account · {enlisted_days}d")

        state = "clean"
        if a.alert or (a.score > 0):
            state = "known"
        elif flags:
            state = "flagged"
        elif not prof:
            state = "unchecked"

        contacts.append({
            "handle": handle,
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
                        for sid, note in a.flagged_orgs],
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
            "verified": bool(prof.get("exists")),
            "orgs": orgs,
            "flags": flags,
            "state": state,
            "profile_url": rsi.profile_url(handle),
            "lookup_url": rsi.lookup_url(handle),
        })

    stats = db.stats()
    stats["pending"] = len(db.pending_reports())
    stats["audits"] = len(db.open_audits())
    stats["lookups"] = dict(LOOKUPS)
    # The schema travels with the state so the settings form is rendered from
    # it, not hand-written: adding a Field in settings.py puts a control in the
    # UI with no changes here.
    stats["settings"] = cfg
    stats["settings_schema"] = settings.describe()
    stats["disk"] = _disk_usage()
    stats["ignored"] = db.ignored_list()
    stats["categories"] = [c for c in reputation.CATEGORIES
                           if c != reputation.PENDING]
    # Carries roster_ts and reaches, not just the SID: a flag with no roster
    # reaches only players whose profile names the org, and the profile
    # endpoint gives a main org only - so an unpulled flag on an affiliate org
    # is a row that looks active and does nothing. The panel says which.
    stats["flagged_orgs_list"] = db.flagged_orgs()
    stats["rosters"] = dict(ROSTERS)
    return {"contacts": contacts, "stats": stats, "now": time.time()}


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass  # a request log per poll is noise

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

        Change is detected from the mtime of the two SQLite files, which is two
        stat() calls - so the server still polls, but for 0.02ms instead of
        19ms, and sends nothing when nothing happened.

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

        def stamp():
            # settings.json is in here too: a setting changed in one tab has to
            # reach any other open tab, and the settings panel is part of the
            # state payload. Three stat() calls instead of two.
            out = []
            for p in (reputation.DB_PATH, rsi.CACHE_PATH, settings.CONFIG_PATH):
                try:
                    out.append(p.stat().st_mtime_ns)
                except OSError:
                    out.append(0)
            return tuple(out)

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
                return          # tab closed or reloaded; nothing to clean up
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
        elif path in ("/", "/index.html"):
            self._send(PAGE.encode(), "text/html; charset=utf-8")
        else:
            self.send_error(404)

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
        try:
            c = _client().citizen(handle)
        except Exception as exc:
            return {"ok": False,
                    "error": f"lookup failed ({type(exc).__name__}: {exc})"}

        h = c.handle.upper()
        out = {"ok": True, "handle": h, "exists": bool(c.exists),
               "unverified": bool(getattr(c, "unverified", False)),
               "moniker": c.moniker or "", "enlisted": c.enlisted or "",
               "enlisted_days": c.enlisted_days, "redacted": bool(c.redacted),
               "profile_url": rsi.profile_url(h), "lookup_url": rsi.lookup_url(h),
               "orgs": [{"sid": o.sid, "name": o.name, "members": o.members,
                         "is_main": o.is_main,
                         "focuses": [f for f in (o.primary_focus,
                                                 o.secondary_focus) if f],
                         "url": rsi.org_url(o.sid)} for o in c.orgs]}
        if not c.exists:
            out["message"] = f"{h}: no such account"
            return out

        db = _open_db()
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
        else:
            out["message"] = f"{h}: " + c.summary()
        a = db.assess(h)
        out["score"] = round(a.score, 2)
        out["reasons"] = a.lines()
        out["flagged_orgs"] = [sid for sid, _ in a.flagged_orgs]
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

        if path == "/api/org/unflag":
            db.unflag_org(sid)
            ROSTERS.pop(sid, None)
            return {"ok": True, "sid": sid, "message": f"unflagged {sid}"}

        if path == "/api/org/flag":
            note = str(payload.get("note", "")).strip()[:500]
            name = ""
            # The org's display name is worth having on the row, but a failed
            # lookup must not stop the flag being recorded - the flag is the
            # user's judgement and the name is decoration.
            try:
                client = _client()
                if client.enriches:
                    name = getattr(client.org(sid), "name", "") or ""
            except Exception:
                pass
            db.flag_org(sid, note, name)
            if payload.get("roster") is False:
                return {"ok": True, "sid": sid,
                        "message": f"flagged {sid} with no member list - it "
                                   f"reaches only players whose profile names it"}
            return fetch_roster_now(sid)

        # /api/org/roster
        if not db.flagged_orgs() or all(o["sid"] != sid
                                        for o in db.flagged_orgs()):
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
            res = db.purge_ghosts(apply=apply)
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
                if r["handle"].upper() not in cache]
            if not apply:
                return {"ok": True, "job": job, "applied": False,
                        "count": len(todo),
                        "items": [f"{h} · never looked up" for h in todo],
                        "kept": [], "errors": [],
                        "message": (f"{len(todo)} contact(s) corroborated "
                                    f"({need}+ votes in one read, or seen "
                                    f"{need}+ times) with no profile stored"
                                    if todo else
                                    "every corroborated contact has been "
                                    "looked up")}
            client = _client()
            done, ghosts, errors = 0, [], []
            for h in todo:
                try:
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
                               (f", {len(ghosts)} do not exist" if ghosts else "")}
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
                  "/api/org/flag", "/api/org/unflag", "/api/org/roster")
        if not self._guard(json_body=True):
            return
        path = urlparse(self.path).path
        if path not in ROUTES:
            self.send_error(404)
            return
        try:
            n = int(self.headers.get("Content-Length") or 0)
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

            db = _open_db()

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
                else:
                    out = {"resolved": db.resolve_pending(handle, category, note)}
            body = json.dumps({"ok": True, "handle": (handle or "").upper(),
                               **out}).encode()
        except Exception as exc:
            body = json.dumps({"ok": False,
                               "error": f"{type(exc).__name__}: {exc}"}).encode()
        self._send(body, "application/json")


PAGE = r"""<!doctype html>
<html lang="en"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>sc-watch</title>
<style>
:root{
  --bg:#03080e; --panel:#071722; --line:#0e3d55;
  --cyan:#4fd8ff; --cyan-dim:#2a7f9e; --ink:#cfeeff; --muted:#6f9cb3;
  --amber:#ffb347; --red:#ff5964; --green:#57e2a5;
}
*{box-sizing:border-box}
body{
  margin:0; background:var(--bg); color:var(--ink);
  font:13px/1.45 "Segoe UI",system-ui,sans-serif;
  background-image:
    repeating-linear-gradient(0deg,rgba(79,216,255,.028) 0 1px,transparent 1px 3px);
}
header{
  display:flex; align-items:center; gap:18px; flex-wrap:wrap;
  padding:12px 18px; border-bottom:1px solid var(--line);
  background:linear-gradient(180deg,#08202e,#04121b);
}
h1{margin:0; font-size:15px; letter-spacing:.22em; color:var(--cyan);
   text-transform:uppercase; font-weight:600}
h1 .dot{display:inline-block;width:7px;height:7px;border-radius:50%;
  background:var(--cyan);margin-right:9px;box-shadow:0 0 9px var(--cyan)}
.stat{font-size:11px;letter-spacing:.12em;color:var(--muted);text-transform:uppercase}
.stat b{color:var(--ink);font-weight:600;margin-left:5px}
.spacer{flex:1}
.controls{display:flex;gap:8px;align-items:center}
input[type=search]{
  background:#04141d;border:1px solid var(--line);color:var(--ink);
  padding:6px 10px;font:12px/1 inherit;min-width:190px;outline:none}
input[type=search]:focus{border-color:var(--cyan-dim)}
button.filter{
  background:#04141d;border:1px solid var(--line);color:var(--muted);
  padding:6px 11px;font:11px/1 inherit;letter-spacing:.1em;cursor:pointer;
  text-transform:uppercase}
button.filter[aria-pressed=true]{border-color:var(--cyan);color:var(--cyan)}

/* Tile width is a SETTING, not a column count. Asking for "six across" means
   different things on a 2560 monitor and a 5120 one - the same rule gave 410px
   tiles on the first and 836px on the second. Sizing by width instead makes it
   mean the same thing everywhere: the grid fits as many as will go, and the
   number is what the user actually cares about.
   Capped and centred so an ultrawide gets more tiles rather than wider ones. */
#grid{display:grid;gap:14px;padding:16px;max-width:2600px;margin:0 auto;
  grid-template-columns:repeat(auto-fill,minmax(var(--tile,380px),1fr))}
/* An explicit display beats the browser's own [hidden]{display:none}, so
   grid.hidden=true did nothing until this existed. */
#grid[hidden]{display:none}

.tile{
  position:relative; background:linear-gradient(160deg,#08202d,#050f17);
  border:1px solid var(--line); padding:12px 13px 11px;
  /* Column layout so the action row can be pushed to the bottom. Tiles in a
     grid row already stretch to the tallest, so this puts flag and forget in
     the same place on every tile instead of wherever the content above them
     happened to end. Muscle memory beats hunting for a button. */
  display:flex; flex-direction:column;
  clip-path:polygon(0 0,calc(100% - 13px) 0,100% 13px,100% 100%,13px 100%,0 calc(100% - 13px));
  transition:border-color .15s, transform .15s;
}
/* Square by default, growing taller when the content needs it - a grid item's
   min-height is auto, so aspect-ratio sets the floor rather than clipping. A
   card of roughly fixed shape is much easier to scan across a row. Turned off
   by the 'square tiles' setting, which lets every tile shrink to its content. */
.tile{aspect-ratio:1/1}
body.no-square .tile{aspect-ratio:auto}
/* Type scales with the box. Clamped at both ends so a 700px tile does not turn
   into a poster and a 260px one stays legible. */
.tile{font-size:clamp(11px,calc(var(--tile,380px) * .032),15px)}
.handle{font-size:clamp(14px,calc(var(--tile,380px) * .046),21px) !important}
.range{font-size:clamp(12px,calc(var(--tile,380px) * .037),18px) !important}
.meta,.reasons,.rpt-note{font-size:clamp(10.5px,calc(var(--tile,380px) * .031),14px)}
.org-name{font-size:clamp(12px,calc(var(--tile,380px) * .036),17px) !important}
.tile:hover{border-color:var(--cyan-dim); transform:translateY(-1px)}
.tile::before{content:"";position:absolute;left:0;top:0;bottom:0;width:2px;
  background:var(--cyan-dim)}
.tile.known::before{background:var(--red);box-shadow:0 0 10px var(--red)}
.tile.flagged::before{background:var(--amber);box-shadow:0 0 10px var(--amber)}
.tile.clean::before{background:var(--green)}
.tile.unchecked::before{background:#2c4b5c}

.row1{display:flex;align-items:baseline;gap:9px}
/* Handles are monospace, and digits are tinted, because SLIVER and 5LIVER are
   two different real people and in a proportional sans they look identical.
   Acting on the wrong player is the failure this whole tool exists to avoid,
   so the distinction has to survive a glance from a second monitor. */
.handle{font-size:15px;font-weight:600;letter-spacing:.04em;color:#eaf9ff;
  word-break:break-all;
  font-family:ui-monospace,"Cascadia Mono",Consolas,"DejaVu Sans Mono",monospace}
/* Violet, not amber. The tint is a DISAMBIGUATION - it says "this glyph is a
   digit", so 5LIVER cannot be read as SLIVER - but amber means "warning"
   everywhere else in this UI (org flags, the flag button, an unanswered
   prompt), so a confirmed contact like FROST1545 looked permanently suspect
   because its handle happens to contain numbers. Violet is used for nothing
   else here, so it carries no verdict: it distinguishes without accusing. */
.handle .d{color:#b9a6ff}
.range{margin-left:auto;font-size:13px;color:var(--cyan);white-space:nowrap;
  font-variant-numeric:tabular-nums}
.meta{margin-top:5px;font-size:11px;color:var(--muted);letter-spacing:.05em;
  display:flex;gap:12px;flex-wrap:wrap}

.badges{margin-top:9px;display:flex;flex-wrap:wrap;gap:5px}
.badge{font-size:10px;letter-spacing:.11em;text-transform:uppercase;
  padding:3px 7px;border:1px solid currentColor;color:var(--cyan-dim)}
.badge.warn{color:var(--amber)}
.badge.bad{color:var(--red)}
.badge.ok{color:var(--green)}

.orgs{margin-top:9px;border-top:1px solid #0b2f42;padding-top:8px;
  display:flex;flex-direction:column;gap:5px}
.org{display:flex;gap:8px;align-items:flex-start;font-size:11.5px;
     margin-bottom:5px}
.org a{color:var(--cyan);text-decoration:none;border-bottom:1px dotted #245e78}
.org a:hover{border-bottom-style:solid}
/* The org NAME is what you recognise mid-flight - 'Ghost Squadron' reads at a
   glance where 'GHSTSQ' has to be decoded. The tag still matters (it is what
   flag-org takes, and what the RSI URL uses) so it stays, just demoted. */
.org-name{font-size:13px;font-weight:600;line-height:1.25;display:block}
.org-tag{display:block;color:var(--muted);font-size:10.5px;margin-top:1px;
     font-family:ui-monospace,Consolas,monospace;letter-spacing:.03em}
.org .focus{color:var(--muted);margin-left:auto;text-align:right;
     padding-top:2px;flex:0 0 auto}
/* "No org" is a FACT about someone, and a different one from "we have not
   looked" or "they hid it" - three states that used to render as the same
   blank space. Deliberately not the cyan an org name uses: this is an absence,
   and it should not read as a link you failed to click. */
.noorg{margin-top:9px;border-top:1px solid #0b2f42;padding-top:8px;
     font-size:11.5px;color:#7d6f57;letter-spacing:.06em;font-style:italic}
/* Slate, not amber. Hiding your orgs is a privacy setting a lot of people
   turn on; amber reads as an accusation, and the README already records that
   treating org-less players as "hiding something" was the wrong claim. */
.noorg.hidden-org{color:#9a8fb8;font-style:normal}
.noorg.unknown-org{color:#4a6a7a}
/* A handle the API positively denies is the strongest hint the read was wrong,
   so this one does get the alarm colour - unlike "no org", which is a fact
   about a real person. */
.noorg.no-account{color:var(--red);font-style:normal}

/* A saved report is permanent record, so it gets a box rather than the amber
   "tell me later" prompt, and it lists every occurrence instead of collapsing
   to a count - two reports for camping months apart is a pattern, and a
   pattern is the thing worth seeing on a tile. */
.rpts{margin-top:9px;border:1px solid #234a5e;background:#061a25;
     padding:7px 9px}
.rpts h4{margin:0 0 6px;font-size:9.5px;letter-spacing:.16em;font-weight:600;
     text-transform:uppercase;color:var(--cyan-dim)}
.rpt{border-top:1px solid #0e3242;padding:5px 0 4px}
.rpt:first-of-type{border-top:none;padding-top:0}
.rpt-head{display:flex;align-items:baseline;gap:7px}
/* The category is the headline of a report - what they did - so it reads a step
   larger than the note under it. Scales with the tile like the rest of the card;
   at 11px fixed it shrank into the surrounding text as tiles grew. */
.rpt-cat{font-size:clamp(12px,calc(var(--tile,380px) * .037),17px);
     font-weight:600;letter-spacing:.05em;color:#ff8b8b}
.rpt-cat.friendly{color:var(--green)}
.rpt-cat.note{color:var(--muted)}
.rpt-when{font-size:10px;color:var(--muted)}
.rpt-edit{margin-left:auto;background:none;border:1px solid var(--line);
     color:var(--muted);font:inherit;font-size:10px;padding:2px 8px;
     border-radius:3px;cursor:pointer}
.rpt-edit:hover{border-color:var(--cyan);color:var(--cyan)}
.rpt-note{font-size:11.5px;color:var(--ink);line-height:1.45;margin-top:2px;
     word-break:break-word}
.rpt-note.empty{color:var(--muted);font-style:italic}
.rpt-edit-row{display:flex;gap:5px;align-items:center;margin-top:5px;
     flex-wrap:wrap}
.rpt-edit-row select,.rpt-edit-row input{background:#0a1b26;color:var(--ink);
     border:1px solid var(--line);border-radius:3px;font:inherit;
     font-size:11px;padding:3px 6px}
.rpt-edit-row input{flex:1;min-width:90px}
.rpt-edit-row button{background:none;border:1px solid var(--cyan-dim);
     color:var(--cyan);font:inherit;font-size:10.5px;padding:3px 9px;
     border-radius:3px;cursor:pointer}
.rpt-edit-row button.cancel{border-color:var(--line);color:var(--muted)}

.tile-actions{display:flex;justify-content:space-between;gap:8px;
     align-items:center;margin-top:auto;padding-top:12px}
/* Flagging has to be one click and hard to miss - it gets pressed while
   something is shooting at you. Deleting is the opposite: two clicks, small,
   and off to the side. */
.flagbtn{background:#2a1a06;border:1px solid var(--amber);color:var(--amber);
     font:inherit;font-size:11px;letter-spacing:.08em;text-transform:uppercase;
     padding:5px 14px;border-radius:3px;cursor:pointer;font-weight:600}
.flagbtn:hover{background:var(--amber);color:#160d00}
.flagbtn.done{background:#3a2408;border-color:#7a5410;color:#c9922f}
.pending-note{margin-top:9px;border-top:1px solid #4a3410;padding-top:8px}
.pending-note .why{color:var(--amber);font-size:11px;letter-spacing:.05em;
     margin-bottom:6px}
.pending-note select,.pending-note input{background:#0a1b26;color:var(--ink);
     border:1px solid var(--line);border-radius:3px;font:inherit;font-size:11px;
     padding:4px 6px}
.pending-note input{flex:1;min-width:0}
.pending-row{display:flex;gap:6px;align-items:center}
.pending-note button{background:none;border:1px solid var(--cyan-dim);
     color:var(--cyan);font:inherit;font-size:11px;padding:4px 10px;
     border-radius:3px;cursor:pointer}
.pending-note button:hover{border-color:var(--cyan)}
.forget{background:none;border:1px solid #3a1f27;color:#8d5761;
     font:inherit;font-size:10.5px;letter-spacing:.04em;padding:3px 9px;
     border-radius:3px;cursor:pointer}
.forget:hover{border-color:var(--red);color:var(--red);background:#1a0c10}
.forget[disabled]{opacity:.5;cursor:default}
.forget.armed{border-color:var(--red);color:#fff;background:#5c1f28}
.reasons{margin-top:8px;font-size:11.5px;color:#ffd9a0}

/* ---- detail window ---------------------------------------------------- */
.tile{cursor:pointer}
.tile button,.tile a,.tile select,.tile input{cursor:auto}
#veil{position:fixed;inset:0;background:rgba(2,7,12,.82);z-index:50;
  display:flex;align-items:flex-start;justify-content:center;padding:24px 16px;
  overflow:auto}
#veil[hidden]{display:none}
/* Wide, because of what these windows hold: an audit shows OCR crops at native
   size and a full-frame link, and the edit view stacks a report list with
   editable notes. At 640px the crops wrapped and the notes were a two-inch
   slot. min() keeps it from overflowing a small window. */
#modal{background:linear-gradient(160deg,#08202d,#050f17);
  border:1px solid var(--cyan-dim);width:min(1100px,96vw);
  padding:20px 24px 18px;position:relative;
  clip-path:polygon(0 0,calc(100% - 16px) 0,100% 16px,100% 100%,16px 100%,0 calc(100% - 16px))}
#modal.wide{width:min(1320px,97vw)}
#modal h2{margin:0 0 2px;font-size:19px;letter-spacing:.06em;
  font-family:ui-monospace,Consolas,monospace}
#modal .sub{color:var(--muted);font-size:11.5px;margin-bottom:12px}
#modal .close{position:absolute;top:10px;right:14px;background:none;border:none;
  color:var(--muted);font-size:20px;cursor:pointer;line-height:1}
#modal .close:hover{color:var(--ink)}
#modal section{border-top:1px solid #0b2f42;padding-top:10px;margin-top:12px}
#modal h3{margin:0 0 8px;font-size:10.5px;letter-spacing:.16em;
  text-transform:uppercase;color:var(--cyan-dim);font-weight:600}
.rep{display:flex;gap:6px;align-items:center;margin-bottom:6px;flex-wrap:wrap}
.rep select,.rep input{background:#0a1b26;color:var(--ink);
  border:1px solid var(--line);border-radius:3px;font:inherit;font-size:11.5px;
  padding:4px 6px}
.rep input{flex:1;min-width:120px}
.rep .when{color:var(--muted);font-size:10.5px;min-width:62px}
.rep button{background:none;border:1px solid var(--line);color:var(--muted);
  font:inherit;font-size:10.5px;padding:4px 9px;border-radius:3px;cursor:pointer}
.rep .save:hover{border-color:var(--cyan);color:var(--cyan)}
.rep .del:hover{border-color:var(--red);color:var(--red)}
.rep .del.armed{border-color:var(--red);background:#5c1f28;color:#fff}
.rep.ispending select{border-color:var(--amber);color:var(--amber)}
#modal .empty{color:var(--muted);font-size:11.5px}
.notperson p{margin:0 0 9px;color:var(--muted);font-size:11.5px;line-height:1.5}
.np-row{display:flex;gap:8px;align-items:center;flex-wrap:wrap}
.np-note{flex:1;min-width:160px;background:#0a1b26;color:var(--ink);
  border:1px solid var(--line);border-radius:3px;font:inherit;font-size:11.5px;
  padding:5px 7px}
.np-note:focus{outline:none;border-color:var(--cyan)}
.np-go{white-space:nowrap}

/* ---- audit ------------------------------------------------------------ */
.auditbtn{background:#06222e;border:1px solid var(--cyan-dim);color:var(--cyan);
  font:inherit;font-size:10.5px;letter-spacing:.08em;text-transform:uppercase;
  padding:4px 10px;border-radius:3px;cursor:pointer}
.auditbtn:hover{border-color:var(--cyan);background:#0a3547}
.aud{border:1px solid #0b2f42;padding:9px;margin-bottom:9px;background:#04121b}
.aud .meta{color:var(--muted);font-size:10.5px;margin-bottom:7px}
/* Crops are 11-14px tall natively, so they are useless at 1:1 - scaled up with
   crisp-edges rather than smoothed, because the question being asked is
   "which glyph is that", and interpolation invents strokes. */
.aud img.crop{image-rendering:pixelated;display:block;background:#000;
  border:1px solid #123; max-width:100%; margin-bottom:5px}
.aud .cands{display:flex;gap:6px;flex-wrap:wrap;margin:7px 0}
.aud .cands button{background:#2a1a06;border:1px solid var(--amber);
  color:var(--amber);font:inherit;font-size:11.5px;padding:4px 10px;
  border-radius:3px;cursor:pointer;font-family:ui-monospace,Consolas,monospace}
.aud .cands button:hover{background:var(--amber);color:#160d00}
.aud .manual{display:flex;gap:6px;align-items:center;margin-top:6px}
.aud .manual input{flex:1;min-width:0;background:#0a1b26;color:var(--ink);
  border:1px solid var(--line);border-radius:3px;font:inherit;font-size:11.5px;
  padding:4px 6px;font-family:ui-monospace,Consolas,monospace}
.aud .warn{color:var(--amber);font-size:11px;margin-top:7px;line-height:1.4}
.reasons div{margin-top:2px}
.links{margin-top:10px;display:flex;gap:7px;flex-wrap:wrap}
/* Inside the detail window the same links sit in a row that already has its
   own spacing, so they need the box styling without the top margin. */
#modal .links{margin-top:0;display:inline-flex}
.links a{font-size:10px;letter-spacing:.1em;text-transform:uppercase;
  padding:5px 9px;border:1px solid var(--line);color:var(--muted);
  text-decoration:none}
.links a:hover{border-color:var(--cyan);color:var(--cyan)}
#empty{padding:60px 20px;text-align:center;color:var(--muted);
  letter-spacing:.14em;text-transform:uppercase;font-size:12px}
/* ---- manual lookup ---------------------------------------------------- */
.lk-row{display:flex;gap:8px;margin-bottom:14px}
#lk-in{flex:1;background:#0a1b26;color:var(--ink);border:1px solid var(--line);
  border-radius:3px;font:inherit;font-size:14px;padding:8px 10px;
  font-family:ui-monospace,Consolas,monospace;letter-spacing:.04em}
#lk-in:focus{outline:none;border-color:var(--cyan)}
.lk-row button{background:none;border:1px solid var(--cyan-dim);
  color:var(--cyan);font:inherit;font-size:12px;letter-spacing:.08em;
  text-transform:uppercase;padding:8px 18px;border-radius:3px;cursor:pointer}
.lk-row button:hover:not(:disabled){background:rgba(79,216,255,.08)}
.lk-row button:disabled{opacity:.5;cursor:default}
.lk-card{border:1px solid var(--line);padding:13px 15px;background:#061a25}
.lk-top{display:flex;align-items:baseline;gap:12px;flex-wrap:wrap}
.lk-mon{color:var(--muted);font-size:12.5px}
.lk-en{margin-left:auto;color:var(--cyan);font-size:12px}
.lk-note{font-size:12px;color:var(--muted);line-height:1.5;margin-top:8px}
.lk-note.bad{color:var(--red)}
.lk-actions{display:flex;align-items:center;gap:10px;margin-top:12px;
  padding-top:11px;border-top:1px solid #0b2f42}
.lk-actions button{background:none;border:1px solid var(--cyan-dim);
  color:var(--cyan);font:inherit;font-size:11px;letter-spacing:.08em;
  text-transform:uppercase;padding:5px 13px;border-radius:3px;cursor:pointer}
.lk-actions button:hover{background:rgba(79,216,255,.08)}
.lk-actions .lk-note{margin-top:0}

/* ---- settings -------------------------------------------------------
   Two columns because the ask was to find a setting fast: a category list
   you can scan down the side beats one long scrolling page where Audio and
   Storage look identical. The panel is rendered from the schema in
   settings.py, so a new Field shows up here with no CSS or JS changes. */
#settings{display:grid;grid-template-columns:210px minmax(0,1fr);
  gap:0;padding:0;align-items:start}
#settings[hidden]{display:none}
#setnav{border-right:1px solid var(--line);padding:14px 0 30px;
  position:sticky;top:0}
#setnav button{display:block;width:100%;text-align:left;background:none;
  border:none;border-left:2px solid transparent;color:var(--muted);
  font:inherit;font-size:12px;letter-spacing:.09em;text-transform:uppercase;
  padding:9px 16px;cursor:pointer}
#setnav button:hover{color:var(--ink)}
#setnav button[aria-pressed=true]{color:var(--cyan);
  border-left-color:var(--cyan);background:rgba(79,216,255,.05)}
#setpanel{padding:16px 20px 40px;max-width:860px}
#setpanel h2{margin:0 0 3px;font-size:16px;letter-spacing:.08em;
  text-transform:uppercase;color:var(--cyan)}
#setpanel .blurb{color:var(--muted);font-size:12px;margin-bottom:16px}
.setrow{display:grid;grid-template-columns:minmax(0,1fr) 190px;gap:14px;
  align-items:start;padding:11px 0;border-top:1px solid #0b2f42}
.setrow:first-of-type{border-top:none}
.setrow .lab{font-size:13px;color:var(--ink);letter-spacing:.02em}
.setrow .hint{color:var(--muted);font-size:11.5px;line-height:1.45;
  margin-top:3px}
.setrow .ctl{display:flex;align-items:center;gap:7px;justify-content:flex-end}
.setrow input[type=number],.setrow input[type=text],.setrow select{
  background:#0a1b26;color:var(--ink);border:1px solid var(--line);
  border-radius:3px;font:inherit;font-size:12.5px;padding:5px 7px;
  width:120px}
.setrow select{width:140px}
.setrow input:focus,.setrow select:focus{outline:none;border-color:var(--cyan)}
.setrow .unit{color:var(--muted);font-size:11px;min-width:30px}
.setrow .detect{background:none;border:1px solid var(--cyan-dim);
  color:var(--cyan);font:inherit;font-size:10px;letter-spacing:.09em;
  text-transform:uppercase;padding:4px 9px;border-radius:3px;cursor:pointer;
  white-space:nowrap}
.setrow .detect:hover{background:rgba(79,216,255,.08)}
.setrow .detect.listening{border-color:var(--amber);color:var(--amber);
  background:#2a1a06}
.setrow input[type=checkbox]{width:17px;height:17px;accent-color:var(--cyan);
  cursor:pointer}
/* A changed setting that needs a restart is the one thing this form can get
   silently wrong, so it says so on the row rather than in a banner. */
.setrow .needs{color:var(--amber);font-size:10px;letter-spacing:.08em;
  text-transform:uppercase;margin-top:4px}
.setrow.changed .lab{color:var(--cyan)}

/* Advanced rows are the ones that are easy to get wrong, so they are marked
   rather than merely mixed in: a tinted panel, a warning-coloured edge and a
   label on the row itself. Revealing them is opt-in; once revealed they should
   still not look like the settings it is safe to fiddle with. */
.setrow.adv{background:linear-gradient(90deg,rgba(255,179,71,.055),transparent 70%);
  border-left:2px solid #7a5410;padding-left:11px;margin-left:-13px}
.setrow.adv .lab::after{content:"ADVANCED";margin-left:8px;font-size:8.5px;
  letter-spacing:.14em;color:#c9922f;border:1px solid #7a5410;padding:1px 5px;
  border-radius:2px;vertical-align:middle;font-weight:600}
.setrow.adv .hint{color:#9c8464}

/* Reset appears only on a row that has been changed - a button that does
   nothing on most rows is noise, and its presence is itself the signal that
   this value is not the default. */
.setrow .reset{background:none;border:1px solid var(--line);color:var(--muted);
  font:inherit;font-size:9.5px;letter-spacing:.1em;text-transform:uppercase;
  padding:2px 7px;border-radius:3px;cursor:pointer;margin-top:5px}
.setrow .reset:hover{border-color:var(--cyan);color:var(--cyan)}
.setrow .default-was{color:var(--muted);font-size:10px;margin-top:5px}
.adv-toggle{margin:18px 0 0;color:var(--muted);font-size:11.5px}
.adv-toggle button{background:none;border:1px solid var(--line);
  color:var(--muted);font:inherit;font-size:11px;padding:4px 10px;
  border-radius:3px;cursor:pointer}
.adv-toggle button:hover{border-color:var(--cyan);color:var(--cyan)}

/* ---- maintenance ---------------------------------------------------- */
.job{border-top:1px solid #0b2f42;padding:13px 0}
.job:first-of-type{border-top:none}
.job h3{margin:0 0 4px;font-size:12.5px;letter-spacing:.05em;color:var(--ink);
  text-transform:none;font-weight:600}
.job p{margin:0 0 9px;color:var(--muted);font-size:11.5px;line-height:1.5}
.job button{background:none;border:1px solid var(--cyan-dim);color:var(--cyan);
  font:inherit;font-size:11px;letter-spacing:.08em;text-transform:uppercase;
  padding:5px 13px;border-radius:3px;cursor:pointer}
.job button:hover{border-color:var(--cyan);background:rgba(79,216,255,.08)}
.job button.go{border-color:var(--red);color:var(--red)}
.job button.go:hover{background:#5c1f28;color:#fff}
.job .out{margin-top:9px;font-size:11.5px;color:var(--muted);
  line-height:1.55;white-space:pre-wrap}
.job .out b{color:var(--ink);font-weight:600}
.job .list{margin-top:6px;max-height:230px;overflow:auto;border:1px solid
  var(--line);padding:7px 9px;font-family:ui-monospace,Consolas,monospace;
  font-size:11px;color:var(--muted);line-height:1.6}
.job .list div{white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.iglist{margin-top:8px;border:1px solid var(--line);max-height:260px;
  overflow:auto}
.igrow{display:flex;gap:10px;align-items:center;padding:6px 9px;
  border-top:1px solid #0b2f42;font-size:11.5px}
.igrow:first-child{border-top:none}
.iglabel{font-family:ui-monospace,Consolas,monospace;font-weight:600;
  color:var(--ink);min-width:170px}
.ignote{color:var(--muted);flex:1;min-width:0;overflow:hidden;
  text-overflow:ellipsis;white-space:nowrap}
.ighits{color:var(--cyan-dim);font-size:10.5px;white-space:nowrap}
.igdel{background:none;border:1px solid var(--line);color:var(--muted);
  font:inherit;font-size:10px;letter-spacing:.08em;text-transform:uppercase;
  padding:3px 9px;border-radius:3px;cursor:pointer}
.igdel:hover{border-color:var(--cyan);color:var(--cyan)}
/* ---- flagged orgs ----------------------------------------------------- */
.orglist{margin-top:8px;border:1px solid var(--line)}
.orgrow{padding:9px 10px;border-top:1px solid #0b2f42}
.orgrow:first-child{border-top:none}
.orghead{display:flex;gap:10px;align-items:center;flex-wrap:wrap}
.orghead .org-name{font-weight:600;color:var(--ink);text-decoration:none}
.orghead .org-name:hover{color:var(--cyan);text-decoration:underline}
.orghead .org-tag{font-family:ui-monospace,Consolas,monospace;font-size:10.5px;
  color:var(--cyan-dim);letter-spacing:.06em}
.orghead button{margin-left:auto}
.orghead .del{margin-left:0}
.orgmeta{margin-top:4px;font-size:11.5px;color:var(--muted)}
/* An unpulled flag is inert, so it is the one state that gets a colour. */
.noroster{color:var(--amber);font-weight:600}
.orgnote{margin-top:3px;font-size:11.5px;color:var(--muted);font-style:italic}
.orgmsg{margin-top:5px;font-size:11px;color:var(--cyan)}
.orgadd{display:flex;gap:8px;align-items:center;flex-wrap:wrap;margin-top:11px}
.orgadd input{background:#04161f;border:1px solid var(--line);color:var(--ink);
  font:inherit;font-size:11.5px;padding:5px 8px;border-radius:3px}
#org-sid{width:150px;font-family:ui-monospace,Consolas,monospace;
  text-transform:uppercase}
#org-note{flex:1;min-width:160px}
.orgadd .blurb{flex-basis:100%;margin:0}
.disk{display:flex;gap:16px;flex-wrap:wrap;margin:2px 0 14px;
  font-size:11.5px;color:var(--muted)}
.disk b{color:var(--ink);font-family:ui-monospace,Consolas,monospace}

footer{padding:8px 18px;border-top:1px solid var(--line);font-size:10.5px;
  letter-spacing:.1em;color:#456f83}
</style></head><body>

<header>
  <h1><span class="dot"></span>sc-watch</h1>
  <span class="stat">contacts<b id="s-players">–</b></span>
  <span class="stat">sightings<b id="s-sightings">–</b></span>
  <span class="stat">reports<b id="s-reports">–</b></span>
  <span class="stat">awaiting reason<b id="s-pending">–</b></span>
  <span class="stat">to audit<b id="s-audits">–</b></span>
  <span class="stat">flagged orgs<b id="s-orgs">–</b></span>
  <span class="spacer"></span>
  <div class="controls">
    <input type="search" id="q" placeholder="filter handle / org">
    <button class="filter" id="f-alerts" aria-pressed="false">alerts only</button>
    <button class="filter" id="f-lookup">look up</button>
    <button class="filter" id="f-settings" aria-pressed="false">settings</button>
  </div>
</header>

<div id="grid"></div>
<div id="settings" hidden>
  <div id="setnav"></div>
  <div id="setpanel"></div>
</div>
<div id="veil" hidden><div id="modal"></div></div>
<!-- Names no file: an installed build has no watch.py to run, and the first
     thing a new user reads should not point at something that is not there. -->
<div id="empty" hidden>no contacts yet — start the watcher and ping something</div>
<footer><span id="updated">connecting…</span></footer>

<script>
const grid=document.getElementById('grid'), empty=document.getElementById('empty');
const q=document.getElementById('q'), fAlerts=document.getElementById('f-alerts');
const fSettings=document.getElementById('f-settings');
const setPane=document.getElementById('settings'), setNav=document.getElementById('setnav'),
      setPanel=document.getElementById('setpanel');
let data={contacts:[],stats:{}};
let settingsOpen=false, settingsCat='capture';
let lookupOpen=false;

fAlerts.onclick=()=>{fAlerts.setAttribute('aria-pressed',
  fAlerts.getAttribute('aria-pressed')==='true'?'false':'true'); render();};
q.oninput=render;

document.getElementById('f-lookup').onclick=openLookup;

fSettings.onclick=()=>{
  settingsOpen=!settingsOpen;
  fSettings.setAttribute('aria-pressed', settingsOpen?'true':'false');
  render();
  if(settingsOpen) renderSettings();
};

function ago(ts){
  if(!ts) return '–';
  const s=Math.max(0,Date.now()/1000-ts);
  if(s<60) return Math.round(s)+'s ago';
  if(s<3600) return Math.round(s/60)+'m ago';
  if(s<86400) return Math.round(s/3600)+'h ago';
  return Math.round(s/86400)+'d ago';
}
function esc(s){return String(s??'').replace(/[&<>"]/g,
  c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]));}
// Tint digits so 5LIVER cannot be mistaken for SLIVER at a glance.
// Tinting digits answers a live question: "could this 5 actually be an S?"
// Once the exact string has resolved to a real RSI account, that question is
// settled - FROST1545 is verifiably FROST1545 - and marking the digits after
// that is stale information dressed as a warning. So the tint is for
// UNVERIFIED handles only, which is precisely where a 5/S mix-up would do
// damage. A handle the API says does not exist keeps the tint emphatically:
// that is the case most likely to be a misread.
function handleHTML(s, verified){
  const t=esc(s);
  return verified ? t : t.replace(/[0-9]/g,d=>`<span class="d">${d}</span>`);
}

// Three different claims that all used to render as nothing at all.
function orgStateHTML(c){
  if(c.orgs.length) return '';
  if(!c.known_profile)
    return `<div class="noorg unknown-org">not looked up yet</div>`;
  if(!c.verified)
    return `<div class="noorg no-account">no RSI account by this name —
      likely a misread</div>`;
  if(c.redacted)
    return `<div class="noorg hidden-org">orgs hidden by this player</div>`;
  return `<div class="noorg">no org</div>`;
}

// Saved reports, newest first. 'pending' is excluded - it has its own amber
// prompt above, because it is a question rather than a record.
function reportsHTML(c){
  const rs=(c.reports||[]).filter(r=>r.category!=='pending');
  if(!rs.length) return '';
  const age=d=>d<1?'today':(d<365?d+'d ago':(d/365).toFixed(1)+'y ago');
  return `<div class="rpts">
    <h4>${rs.length} report${rs.length===1?'':'s'}</h4>
    ${rs.map(r=>`
      <div class="rpt" data-id="${r.id}" data-handle="${esc(c.handle)}">
        <div class="rpt-head">
          <span class="rpt-cat ${esc(r.category)}">${esc(r.category.replace(/_/g,' '))}</span>
          <span class="rpt-when">${age(r.age_days)}</span>
          <button class="rpt-edit">edit</button>
        </div>
        <div class="rpt-note ${r.note?'':'empty'}">${esc(r.note||'no reason recorded')}</div>
      </div>`).join('')}
  </div>`;
}

function tile(c){
  const el=document.createElement('div');
  el.className='tile '+c.state;
  el.dataset.handle=c.handle;
  const badges=[];
  if(c.state==='known') badges.push(['bad','known '+(c.score>0?'+'+c.score:'')]);
  c.flags.forEach(f=>badges.push([/PIRACY/.test(f)?'bad':'warn',f]));
  if(c.redacted) badges.push(['','orgs redacted']);
  if(!c.known_profile) badges.push(['','not looked up']);
  else if(!c.verified) badges.push(['bad','no RSI account']);
  if(c.state==='clean'&&!c.flags.length) badges.push(['ok','no flags']);

  el.innerHTML=`
    <div class="row1">
      <span class="handle">${handleHTML(c.handle, c.verified)}</span>
      <span class="range">${c.range_km!=null?c.range_km.toFixed(1)+' km':''}</span>
    </div>
    <div class="meta">
      <span>seen ${c.sightings}×</span>
      <span>${ago(c.last_seen)}</span>
      ${c.enlisted_days!=null?`<span>enlisted ${
        c.enlisted_days<365?c.enlisted_days+'d':Math.floor(c.enlisted_days/365)+'y'}</span>`:''}
    </div>
    ${badges.length?`<div class="badges">${badges.map(([k,t])=>
      `<span class="badge ${k}">${esc(t)}</span>`).join('')}</div>`:''}
    ${c.orgs.length?`<div class="orgs">${c.orgs.map(o=>`
      <div class="org">
        <span>
          <a class="org-name" href="${esc(o.url)}" target="_blank"
             rel="noopener">${esc(o.name||o.sid||'—')}</a>
          <span class="org-tag">${esc(o.sid)} · ${o.is_main?'main':'affil'} · ${o.members}</span>
        </span>
        <span class="focus">${esc(o.focuses.join(' / '))}</span>
      </div>`).join('')}</div>`:''}
    ${orgStateHTML(c)}
    ${reportsHTML(c)}
    ${c.reasons.length?`<div class="reasons">${c.reasons.map(r=>
      `<div>▸ ${esc(r)}</div>`).join('')}</div>`:''}
    <div class="links">
      <a href="${esc(c.profile_url)}" target="_blank" rel="noopener">RSI profile</a>
      <a href="${esc(c.lookup_url)}" target="_blank" rel="noopener">SCI lookup</a>
    </div>
    ${c.pending?`<div class="pending-note">
      <div class="why">flagged ${c.pending}x — no reason recorded yet</div>
      <div class="pending-row">
        <select class="reason-cat" data-handle="${esc(c.handle)}">
          ${(data.stats.categories||[]).map(k=>
            `<option value="${esc(k)}">${esc(k.replace(/_/g,' '))}</option>`).join('')}
        </select>
        <input class="reason-note" data-handle="${esc(c.handle)}"
               placeholder="what happened (optional)" maxlength="500">
        <button class="reason-save" data-handle="${esc(c.handle)}">save</button>
      </div>
    </div>`:''}
    <div class="tile-actions">
      <button class="flagbtn ${c.pending?'done':''}"
              data-handle="${esc(c.handle)}">${c.pending?'flag again':'flag'}</button>
      ${(c.audits||[]).length?`<button class="auditbtn"
         data-handle="${esc(c.handle)}">audit ${c.audits.length}</button>`:''}
      <button class="forget ${c.handle===armedHandle?'armed':''}"
              data-handle="${esc(c.handle)}">${c.handle===armedHandle
                ?'delete '+esc(c.handle)+'? click again':'forget'}</button>
    </div>`;
  return el;
}

// Deleting is two clicks, not one: 'forget' arms, a second click inside six
// seconds commits. The poll rebuilds the whole grid every 2s, which would rip
// an armed button out from under the second click, so rendering pauses while
// something is armed - and disarms itself so a stray arm cannot freeze the UI.
// How long an armed confirm stays live. The first version used SIX seconds,
// which was far too short: a human reads "delete WASTE? click again", thinks
// about it, and clicks after eight - by then it had silently disarmed, so the
// second click just re-armed it and nothing ever happened. Reported from a
// live session as "the second click didn't work", and it loops forever.
//
// It only had a fuse because arming used to SUPPRESS rendering, and a stray
// arm would have frozen the grid. Armed state is now rendered FROM state, so
// it survives a rebuild and the timeout is just tidiness.
const ARM_MS=20000;
let armedHandle=null, armedTimer=null;

function disarm(){
  armedHandle=null;
  if(armedTimer){clearTimeout(armedTimer); armedTimer=null;}
  render();
}

// Editing a saved report, in place on the tile. A report gets written before
// the story is complete - a flag pressed mid-fight, a category guessed at, a
// note added later when the same person does it twice - so amending one is
// finishing the record rather than falsifying it. The timestamp is never
// touched: a report dates the incident, not the paperwork.
grid.addEventListener('click', async ev=>{
  const editBtn=ev.target.closest('.rpt-edit');
  if(editBtn){
    const box=editBtn.closest('.rpt');
    if(box.querySelector('.rpt-edit-row')) return;   // already open
    const cat=box.querySelector('.rpt-cat').textContent.trim().replace(/ /g,'_');
    const noteEl=box.querySelector('.rpt-note');
    const note=noteEl.classList.contains('empty')?'':noteEl.textContent.trim();
    const cats=data.stats.categories||[];
    const row=document.createElement('div');
    row.className='rpt-edit-row';
    row.innerHTML=`
      <select class="rpt-cat-in">${cats.map(k=>
        `<option value="${esc(k)}" ${k===cat?'selected':''}>${esc(k.replace(/_/g,' '))}</option>`
      ).join('')}</select>
      <input class="rpt-note-in" maxlength="500" placeholder="what happened"
             value="${esc(note)}">
      <button class="rpt-save">save</button>
      <button class="cancel rpt-cancel">cancel</button>`;
    box.appendChild(row);
    row.querySelector('.rpt-note-in').focus();
    return;
  }

  if(ev.target.closest('.rpt-cancel')){
    ev.target.closest('.rpt-edit-row').remove();
    // The render that was skipped while this field had focus is replayed on
    // blur; removing the row without that leaves the tile a frame behind.
    render();
    return;
  }

  const save=ev.target.closest('.rpt-save');
  if(!save) return;
  const box=save.closest('.rpt'), row=save.closest('.rpt-edit-row');
  const id=Number(box.dataset.id);
  const category=row.querySelector('.rpt-cat-in').value;
  const note=row.querySelector('.rpt-note-in').value;
  save.disabled=true; save.textContent='saving…';
  try{
    // Blur first: the save is supposed to trigger a refresh, and the guard
    // would suppress the very render it causes while this input holds focus.
    row.querySelector('.rpt-note-in').blur();
    const j=await post('/api/report/edit',{id, category, note});
    if(!j.ok) throw new Error(j.error||'failed');
    say(`updated ${box.dataset.handle} — ${category.replace(/_/g,' ')}`);
    await poll(); render();
  }catch(e){
    say('could not save: '+e.message);
    save.disabled=false; save.textContent='save';
  }
});

grid.addEventListener('click', async ev=>{
  const btn=ev.target.closest('.forget');
  if(!btn) return;
  const handle=btn.dataset.handle;
  if(armedHandle!==handle){
    armedHandle=handle;
    if(armedTimer) clearTimeout(armedTimer);
    armedTimer=setTimeout(disarm,ARM_MS);
    render();
    return;
  }
  btn.disabled=true; btn.textContent='deleting...';
  try{
    const r=await fetch('/api/forget',{method:'POST',
      headers:{'Content-Type':'application/json'},
      body:JSON.stringify({handle})});
    const j=await r.json();
    if(!j.ok) throw new Error(j.error||'failed');
    const n=j.removed||{};
    say('forgot '+j.handle+' — '+
      (n.sightings||0)+' sighting(s), '+(n.reports||0)+' report(s)');
    armedHandle=null;
    if(armedTimer){clearTimeout(armedTimer); armedTimer=null;}
    await poll();
  }catch(e){
    btn.disabled=false; btn.textContent='forget';
    say('delete failed: '+e.message);
    disarm();
  }
});

// ---- detail window -------------------------------------------------------
// Opened by clicking anywhere on a tile that is not itself a control. This is
// where anything gets UNDONE or corrected: a flag is one button press, so some
// of them are tests, misfires or the wrong tile, and until there was a way to
// take one back the only undo was forget() - which throws the sighting history
// away too, making a mis-click cost more than the mistake did.
const veil=document.getElementById('veil'), modal=document.getElementById('modal');
let openHandle=null;

function contactByHandle(h){
  return (data.contacts||[]).find(c=>c.handle===h);
}

function closeModal(){ openHandle=null; lookupOpen=false; veil.hidden=true; render(); }

function renderModal(){
  const c=contactByHandle(openHandle);
  if(!c){ closeModal(); return; }
  const cats=data.stats.categories||[];
  const age=d=>d<1?'today':(d<365?d+'d ago':(d/365).toFixed(1)+'y ago');
  modal.innerHTML=`
    <button class="close" title="close">&times;</button>
    <h2>${esc(c.handle)}</h2>
    <div class="sub">
      ${c.sightings} sighting${c.sightings===1?'':'s'}
      ${c.enlisted_days!=null?` · enlisted ${c.enlisted_days<365?c.enlisted_days+'d':
        Math.floor(c.enlisted_days/365)+'y'}`:''}
      ${c.range_km!=null?` · last seen ${Number(c.range_km).toFixed(1)} km`:''}
      · score ${c.score}
    </div>
    ${(c.flags.length||c.reasons.length)?`<section><h3>flags</h3>${
      c.flags.concat(c.reasons).map(f=>
      `<div>▸ ${esc(f)}</div>`).join('')}</section>`:''}
    ${c.orgs.length?`<section><h3>orgs</h3>${c.orgs.map(o=>
      `<div class="org"><span>
         <a class="org-name" href="${esc(o.url)}" target="_blank" rel="noopener">${esc(o.name||o.sid)}</a>
         <span class="org-tag">${esc(o.sid)} · ${o.is_main?'main':'affil'} · ${o.members}</span>
       </span><span class="focus">${esc(o.focuses.join(' / '))}</span></div>`).join('')}</section>`:''}
    ${(c.audits||[]).length?`<section>
      <h3>audit — ${c.audits.length} uncertain read${c.audits.length===1?'':'s'}</h3>
      ${c.audits.map(a=>`
        <div class="aud" data-audit="${a.id}">
          <div class="meta">read as <b>${esc(a.read_as||a.handle)}</b>${
            (a.read_as&&a.read_as!==a.handle)
              ? ` <span style="color:var(--muted)">(stored as ${esc(a.handle)})</span>`:''} at
            ${a.range_km==null?'?':Number(a.range_km).toFixed(1)} km ·
            ${a.votes}/${a.total} votes ·
            ${new Date(a.ts*1000).toLocaleString()}</div>
          ${a.files_missing?`<div class="warn">the images for this audit are
             gone — nothing left to check, dismiss it</div>`:''}
          ${a.crop_name?`<img class="crop" src="/${esc(a.crop_name)}" alt="name crop">`:''}
          ${a.crop_range?`<img class="crop" src="/${esc(a.crop_range)}" alt="range crop">`:''}
          <div class="cands">
            <button data-pick="${esc(a.read_as||a.handle)}">${esc(a.read_as||a.handle)}</button>
            ${(a.read_as&&a.read_as!==a.handle)
              ? `<button data-pick="${esc(a.handle)}">${esc(a.handle)}</button>`:''}
            ${(a.variants||[]).map(v=>
              `<button data-pick="${esc(v[0])}">${esc(v[0])} ×${v[1]}</button>`).join('')}
          </div>
          <div class="manual">
            <input class="aud-name" placeholder="or type what it actually says"
                   maxlength="24" value="">
            <button class="save aud-confirm">confirm</button>
            <button class="del aud-dismiss">dismiss</button>
            ${a.frame_path?`<a class="links" href="/${esc(a.frame_path)}"
               target="_blank" rel="noopener"
               style="border:1px solid var(--line);padding:4px 9px;color:var(--muted);
                      text-decoration:none;font-size:10px;letter-spacing:.1em;
                      text-transform:uppercase">full frame</a>`:''}
          </div>
        </div>`).join('')}
    </section>`:''}
    <section>
      <h3>reports${c.reports.length?` — ${c.reports.length}`:''}</h3>
      ${c.reports.length?c.reports.map(r=>`
        <div class="rep ${r.category==='pending'?'ispending':''}" data-id="${r.id}">
          <span class="when">${age(r.age_days)}</span>
          <select class="r-cat">
            ${r.category==='pending'?'<option value="pending">pending</option>':''}
            ${cats.map(k=>`<option value="${esc(k)}"${k===r.category?' selected':''}>
              ${esc(k.replace(/_/g,' '))}</option>`).join('')}
          </select>
          <input class="r-note" value="${esc(r.note||'')}"
                 placeholder="what happened" maxlength="500">
          <button class="save" data-id="${r.id}">save</button>
          <button class="del ${String(r.id)===armedDel?'armed':''}"
                  data-id="${r.id}">${String(r.id)===armedDel
                    ?'remove? click again':'remove'}</button>
        </div>`).join(''):'<div class="empty">no reports yet</div>'}
    </section>
    <section>
      <h3>add a report</h3>
      <div class="rep">
        <select class="add-cat">${cats.map(k=>
          `<option value="${esc(k)}">${esc(k.replace(/_/g,' '))}</option>`).join('')}</select>
        <input class="add-note" placeholder="what happened" maxlength="500">
        <button class="save add-go">add</button>
      </div>
    </section>
    <section>
      <div class="rep">
        <span class="links">
          <a href="${esc(c.profile_url)}" target="_blank" rel="noopener">RSI profile</a>
          <a href="${esc(c.lookup_url)}" target="_blank" rel="noopener">SCI lookup</a>
        </span>
        <span style="flex:1"></span>
        <button class="del forget-all ${armedDel==='forget'?'armed':''}"
                data-handle="${esc(c.handle)}">${armedDel==='forget'
                  ?'delete everything? click again':'forget this contact'}</button>
      </div>
    </section>
    <section>
      <h3>not a person?</h3>
      <div class="notperson">
        <p>Some things the HUD draws look exactly like a contact — a floating
           salvage crate gets a label and a range line like anybody else.
           Ignoring one deletes its record and stops it being reported again,
           including OCR variants of the same string.</p>
        <div class="np-row">
          <input class="np-note" maxlength="200"
                 placeholder="what it actually is (optional)">
          <button class="del np-go ${armedDel==='ignore'?'armed':''}"
                  data-handle="${esc(c.handle)}">${armedDel==='ignore'
                    ?'ignore '+esc(c.handle)+' forever? click again'
                    :'not a player — always ignore'}</button>
        </div>
      </div>
    </section>`;
  veil.hidden=false;
}

// ---- manual lookup ----------------------------------------------------
// Check a handle you were told rather than one you saw. Looking up and
// storing are deliberately two clicks: most lookups are "who is this?" and
// should leave nothing behind, because a handle typed to check it is not a
// sighting and storing one would put somebody in the database you never met.
let lookupState={handle:'', result:null, busy:false, error:''};

function renderLookup(){
  const r=lookupState.result;
  const age=d=>d==null?'':(d<365?d+'d':(d/365).toFixed(1)+'y');
  let body='';
  if(lookupState.busy){
    body=`<div class="lk-note">looking up…</div>`;
  }else if(lookupState.error){
    body=`<div class="lk-note bad">${esc(lookupState.error)}</div>`;
  }else if(r && !r.exists){
    body=`<div class="lk-note bad">no RSI account called ${esc(r.handle)}.
      If you read this off the HUD it is almost certainly a misread.</div>`;
  }else if(r){
    body=`
      <div class="lk-card">
        <div class="lk-top">
          <span class="handle">${handleHTML(r.handle, r.exists)}</span>
          ${r.moniker&&r.moniker.toUpperCase()!==r.handle
            ? `<span class="lk-mon">${esc(r.moniker)}</span>`:''}
          ${r.enlisted_days!=null
            ? `<span class="lk-en">enlisted ${age(r.enlisted_days)}</span>`:''}
        </div>
        ${r.flagged_orgs.length?`<div class="lk-note bad">member of flagged org
          ${r.flagged_orgs.map(esc).join(', ')}</div>`:''}
        ${r.reasons.length?`<div class="reasons">${r.reasons.map(x=>
          `<div>▸ ${esc(x)}</div>`).join('')}</div>`:''}
        ${r.orgs.length?`<div class="orgs">${r.orgs.map(o=>`
          <div class="org"><span>
            <a class="org-name" href="${esc(o.url)}" target="_blank"
               rel="noopener">${esc(o.name||o.sid)}</a>
            <span class="org-tag">${esc(o.sid)} · ${o.is_main?'main':'affil'} · ${o.members}</span>
          </span><span class="focus">${esc(o.focuses.join(' / '))}</span></div>`
          ).join('')}</div>`
          : (r.redacted
              ? `<div class="noorg hidden-org">orgs hidden by this player</div>`
              : `<div class="noorg">no org</div>`)}
        <div class="links">
          <a href="${esc(r.profile_url)}" target="_blank" rel="noopener">RSI profile</a>
          <a href="${esc(r.lookup_url)}" target="_blank" rel="noopener">SCI lookup</a>
        </div>
        <div class="lk-actions">
          ${r.stored
            ? `<span class="lk-note">already in your database</span>
               <button class="lk-open" data-handle="${esc(r.handle)}">open record</button>`
            : `<button class="save lk-add" data-handle="${esc(r.handle)}">add to database</button>`}
        </div>
      </div>`;
  }
  modal.innerHTML=`
    <button class="close" title="close">&times;</button>
    <h2>look up a player</h2>
    <div class="sub">Asks RSI directly. Nothing is stored unless you say so.</div>
    <div class="lk-row">
      <input id="lk-in" placeholder="handle, exactly as spelled" maxlength="40"
             value="${esc(lookupState.handle)}" autocomplete="off">
      <button class="save" id="lk-go" ${lookupState.busy?'disabled':''}>look up</button>
    </div>
    ${body}`;
  veil.hidden=false;
  const inp=document.getElementById('lk-in');
  if(inp && !lookupState.busy){ inp.focus(); inp.select(); }
}

function openLookup(){ openHandle=null; lookupOpen=true; renderLookup(); }

async function doLookup(add){
  const inp=document.getElementById('lk-in');
  const handle=(inp?inp.value:lookupState.handle).trim();
  if(!handle) return;
  lookupState={handle, result:lookupState.result, busy:true, error:''};
  renderLookup();
  try{
    const j=await post('/api/lookup',{handle, add:!!add});
    if(!j.ok){ lookupState={handle, result:null, busy:false, error:j.error}; }
    else{ lookupState={handle, result:j, busy:false, error:''};
          if(j.message) say(j.message); }
    if(add) await poll();
  }catch(e){
    lookupState={handle, result:null, busy:false, error:e.message};
  }
  renderLookup();
}

modal.addEventListener('click', async ev=>{
  if(!lookupOpen) return;
  if(ev.target.id==='lk-go'){ await doLookup(false); return; }
  if(ev.target.closest('.lk-add')){ await doLookup(true); return; }
  const open=ev.target.closest('.lk-open');
  if(open){ lookupOpen=false; openModal(open.dataset.handle); }
});
modal.addEventListener('keydown', ev=>{
  if(lookupOpen && ev.key==='Enter' && ev.target.id==='lk-in') doLookup(false);
});

function openModal(handle){ openHandle=handle; renderModal(); }

grid.addEventListener('click', ev=>{
  const aud=ev.target.closest('.auditbtn');
  if(aud){ openModal(aud.dataset.handle); return; }
  if(ev.target.closest('button,a,select,input')) return;
  const tile=ev.target.closest('.tile');
  if(tile && tile.dataset.handle) openModal(tile.dataset.handle);
});
veil.addEventListener('click', ev=>{ if(ev.target===veil) closeModal(); });
document.addEventListener('keydown', ev=>{ if(ev.key==='Escape'&&openHandle) closeModal(); });

let stickyUntil=0;
function say(msg){ updated.textContent=msg; stickyUntil=Date.now()+6000; }

// A lookup runs on the server's own thread, so its result arrives with a later
// stream frame rather than in the POST response. Watch for it and report -
// otherwise a handle that does not resolve looks exactly like one that does,
// which is the case most worth knowing about.
function watchLookup(handle){
  const h=(handle||'').toUpperCase();
  let tries=0;
  const t=setInterval(()=>{
    const msg=(data.stats&&data.stats.lookups||{})[h];
    if(msg && !/looking up/.test(msg)){ clearInterval(t); say(msg); }
    else if(++tries>20){ clearInterval(t); }
  },700);
}

async function post(url, payload){
  const r=await fetch(url,{method:'POST',
    headers:{'Content-Type':'application/json'},
    body:JSON.stringify(payload)});
  const j=await r.json();
  if(!j.ok) throw new Error(j.error||'failed');
  return j;
}

grid.addEventListener('click', async ev=>{
  const flag=ev.target.closest('.flagbtn');
  if(flag){
    const handle=flag.dataset.handle;
    flag.disabled=true;
    try{
      await post('/api/flag',{handle});
      say('flagged '+handle+' — add the reason when you land');
      await poll();
    }catch(e){ say('flag failed: '+e.message); flag.disabled=false; }
    return;
  }
  const save=ev.target.closest('.reason-save');
  if(save){
    const handle=save.dataset.handle;
    const cat=grid.querySelector(`.reason-cat[data-handle="${CSS.escape(handle)}"]`);
    const note=grid.querySelector(`.reason-note[data-handle="${CSS.escape(handle)}"]`);
    save.disabled=true;
    try{
      const j=await post('/api/reason',
        {handle, category:cat.value, note:note.value});
      say('recorded '+j.resolved+' report(s) for '+handle+
        ' as '+cat.value);
      // Drop focus first, or editingInGrid() is still true and the refresh
      // this poll is supposed to do gets skipped.
      note.blur(); cat.blur();
      await poll();
    }catch(e){ say('save failed: '+e.message); save.disabled=false; }
  }
});

// True whenever the cursor is in a control inside a tile.
//
// The poll rebuilds the whole grid every 2s, and replaceChildren destroys
// whatever had focus. The first version of this guard only paused for a
// .reason-note that ALREADY had text, which got it exactly backwards: an empty
// field is the one you are about to type into, so it lost focus after at most
// two seconds and you could never get a first character in. An open <select>
// was not covered at all, so the dropdown shut itself.
//
// Any focused input or select inside the grid pauses rendering. Click away and
// it resumes on the next poll.
function editingInGrid(){
  const a = document.activeElement;
  if(!a || !grid.contains(a)) return false;
  return a.tagName==='INPUT' || a.tagName==='SELECT' || a.tagName==='TEXTAREA';
}

// The modal has the same problem the grid does - a poll every 2s must not wipe
// a half-edited note or a chosen category out from under you.
function editingInModal(){
  const x=document.activeElement;
  if(!x || veil.hidden || !modal.contains(x)) return false;
  return x.tagName==='INPUT'||x.tagName==='SELECT'||x.tagName==='TEXTAREA';
}

let armedDel=null;

modal.addEventListener('click', async ev=>{
  const t=ev.target;
  if(t.closest('.close')){ closeModal(); return; }

  const np=t.closest('.np-go');
  if(np){
    if(armedDel!=='ignore'){
      armedDel='ignore';
      if(armedTimer) clearTimeout(armedTimer);
      armedTimer=setTimeout(()=>{armedDel=null;renderModal();},ARM_MS);
      renderModal();
      return;
    }
    const note=(modal.querySelector('.np-note')||{}).value||'';
    np.disabled=true; np.textContent='ignoring…';
    try{
      const j=await post('/api/ignore',{handle:np.dataset.handle, note});
      if(!j.ok) throw new Error(j.error||'failed');
      armedDel=null;
      const gone=j.removed||{};
      say(`${j.label} will not be reported again`+
          (gone.sightings?` — removed ${gone.sightings} sighting(s)`:''));
      closeModal();
      await poll(); render();
    }catch(e){ say('could not ignore: '+e.message);
               np.disabled=false; renderModal(); }
    return;
  }

  const del=t.closest('.del');
  if(del){
    const isForget=del.classList.contains('forget-all');
    const key=isForget?'forget':del.dataset.id;
    if(armedDel!==key){
      armedDel=key;
      setTimeout(()=>{ if(armedDel===key){ armedDel=null; renderModal(); } },ARM_MS);
      renderModal();
      return;
    }
    armedDel=null; del.disabled=true;
    try{
      if(isForget){
        await post('/api/forget',{handle:openHandle});
        say('forgot '+openHandle);
        closeModal(); await poll();
      }else{
        const j=await post('/api/report/delete',{id:Number(del.dataset.id)});
        const g=j.deleted;
        say(g?('removed '+g.category+' report on '+j.handle)
              :'report already gone');
        await poll(); renderModal();
      }
    }catch(e){ say('failed: '+e.message); del.disabled=false; }
    return;
  }

  // ---- audit ----
  const pick=t.closest('[data-pick]');
  if(pick){
    pick.closest('.aud').querySelector('.aud-name').value=pick.dataset.pick;
    return;
  }
  const dismiss=t.closest('.aud-dismiss');
  if(dismiss){
    const box=dismiss.closest('.aud');
    dismiss.disabled=true;
    try{
      await post('/api/audit/dismiss',{id:Number(box.dataset.audit)});
      say('audit dismissed — reading left as it was');
      await poll(); renderModal();
    }catch(e){ say('failed: '+e.message); dismiss.disabled=false; }
    return;
  }
  const conf=t.closest('.aud-confirm');
  if(conf){
    const box=conf.closest('.aud');
    const name=box.querySelector('.aud-name').value.trim();
    if(!name){ say('pick a candidate or type the name first'); return; }
    conf.disabled=true;
    try{
      let j=await post('/api/audit/confirm',
        {id:Number(box.dataset.audit), name});
      if(j.needs_confirm===true){
        // Both spellings are real accounts. This is the failure the whole
        // identity rule exists to prevent, so it is asked out loud rather
        // than resolved quietly.
        const w=document.createElement('div');
        w.className='warn';
        w.textContent=j.error+'. Merge anyway?';
        const yes=document.createElement('button');
        yes.className='del'; yes.textContent='merge anyway';
        yes.onclick=async()=>{
          try{
            await post('/api/audit/confirm',
              {id:Number(box.dataset.audit), name, force:true});
            say('merged '+j.old+' into '+j.new+' — looking up on RSI…');
            watchLookup(j.new);
            openHandle=j.new;
            await poll(); renderModal();
          }catch(e){ say('failed: '+e.message); }
        };
        w.appendChild(document.createTextNode(' '));
        w.appendChild(yes);
        box.appendChild(w);
        conf.disabled=false;
        return;
      }
      // A rename with checked===false means no profile backend was available
      // to ask whether the two spellings are two different people. The merge
      // is still what was asked for, but it went through unverified and that
      // should not be silent.
      const blind = j.renamed && j.checked === false;
      say((j.renamed?('corrected '+j.old+' to '+j.new):('confirmed '+j.new))
          + (blind?' — no identity check, add an API key to enable one':'')
          + (j.lookup?' — looking up on RSI…':''));
      if(j.lookup) watchLookup(j.new);
      // Follow the rename. openHandle still points at the old spelling, which
      // no longer exists, so the window would sit on a dead record.
      if(j.renamed) openHandle=j.new;
      await poll(); renderModal();
    }catch(e){ say('failed: '+e.message); conf.disabled=false; }
    return;
  }

  const save=t.closest('.save');
  if(!save) return;
  try{
    if(save.classList.contains('add-go')){
      const cat=modal.querySelector('.add-cat').value;
      const note=modal.querySelector('.add-note').value;
      await post('/api/report/add',{handle:openHandle,category:cat,note});
      say('added '+cat+' report on '+openHandle);
    }else{
      const row=save.closest('.rep');
      await post('/api/report/edit',{id:Number(save.dataset.id),
        category:row.querySelector('.r-cat').value,
        note:row.querySelector('.r-note').value});
      say('updated report on '+openHandle);
    }
    document.activeElement.blur();
    await poll(); renderModal();
  }catch(e){ say('failed: '+e.message); }
});

// ---- settings ---------------------------------------------------------
// Rendered from data.stats.settings_schema, so this function never mentions a
// specific setting. Adding one is a Field in settings.py.
//
// Deliberately NOT re-rendered on every stream frame: the panel is full of
// inputs, and rebuilding it under the cursor is exactly the race that kept
// wiping half-typed notes in the grid. It is drawn when opened, when you
// switch category, and after a save replies - never on a timer.

const JOBS=[
  {id:'ghosts', title:'Remove handles with no RSI account',
   body:'Looks every stored handle up and deletes the ones the API says do '+
        'not exist - OCR inventions like ONNCARSTE for VONCARSTEIN. A handle '+
        'with a report against it, or one waiting on an audit, is never '+
        'touched: that is evidence, and an audit is how a misread gets '+
        'corrected rather than deleted. Rate limited to one lookup a second, '+
        'so a large database takes a minute.',
   verb:'delete them'},
  {id:'backfill', title:'Look up contacts that were never checked',
   body:'Finds contacts seen often enough to meet the evidence bar but with '+
        'no RSI profile stored, and fetches them. The watcher normally does '+
        'this as it goes; one can still be missed if a lookup failed or the '+
        'watcher stopped mid-burst. One request a second.',
   verb:'look them up'},
  {id:'restart', title:'Restart the watcher',
   body:'Most capture and detection settings are read once when the watcher '+
        'starts, so changing one does nothing until it restarts. This asks it '+
        'to reload itself in place - it picks the request up while idle, '+
        'within a second or two. Nothing happens if no watcher is running.',
   verb:'restart it', direct:true},
  {id:'frames', title:'Delete old capture frames',
   body:'Applies the retention rules above to debug_bursts/, live_frames/ '+
        'and probe_frames/ right now, and prunes audits past their age limit '+
        'along with any image no audit row points at.',
   verb:'delete them'},
];

function setVal(k){ return (data.stats.settings||{})[k]; }

// Settings that take an input binding rather than a plain string.
const BINDING_FIELDS=new Set(['key','focus_key','chat_key']);

function fieldHTML(f,v){
  const id='set_'+f.key;
  let ctl;
  if(f.type==='bool'){
    ctl=`<input type="checkbox" id="${id}" data-key="${f.key}" ${v?'checked':''}>`;
  }else if(f.type==='choice'){
    ctl=`<select id="${id}" data-key="${f.key}">`+
      f.choices.map(c=>`<option value="${esc(c)}" ${c===v?'selected':''}>${esc(c)}</option>`)
      .join('')+`</select>`;
  }else if(f.type==='int'||f.type==='float'){
    const step=f.type==='int'?'1':'any';
    ctl=`<input type="number" id="${id}" data-key="${f.key}" value="${esc(v)}"`+
        ` step="${step}"${f.lo!=null?` min="${f.lo}"`:''}${f.hi!=null?` max="${f.hi}"`:''}>`;
  }else if(BINDING_FIELDS.has(f.key)){
    // Four joysticks here all report "HID-compliant game controller", so a
    // dropdown of device names would be four identical rows. Pressing the
    // thing you want is the only way to tell them apart.
    ctl=`<input type="text" id="${id}" data-key="${f.key}" value="${esc(v)}">`+
        `<button class="detect" data-detect="${f.key}">detect</button>`;
  }else{
    ctl=`<input type="text" id="${id}" data-key="${f.key}" value="${esc(v)}">`;
  }
  const changed = JSON.stringify(v)!==JSON.stringify(f.default);
  const shown = d=>f.type==='bool' ? (d?'on':'off') : String(d);
  return `<div class="setrow${changed?' changed':''}${f.advanced?' adv':''}">
    <div>
      <label class="lab" for="${id}">${esc(f.label)}</label>
      ${f.help?`<div class="hint">${esc(f.help)}</div>`:''}
      ${f.restart?`<div class="needs">needs a ${esc(f.restart)} restart</div>`:''}
      ${changed?`<button class="reset" data-reset="${f.key}"
         title="back to ${esc(shown(f.default))}">reset · default ${
         esc(shown(f.default))}</button>`:''}
    </div>
    <div class="ctl">${ctl}<span class="unit">${esc(f.unit||'')}</span></div>
  </div>`;
}

function diskHTML(){
  const d=data.stats.disk||[];
  if(!d.length) return '';
  return `<div class="disk">`+d.map(r=>
    `<span>${esc(r.name)} <b>${r.mb} MB</b> · ${r.files} file${r.files===1?'':'s'}</span>`
  ).join('')+`</div>`;
}

// Hold the input you want; the field fills in with its binding string.
//
// Polls /api/inputs rather than listening for browser key events, because the
// browser cannot see joystick buttons at all, and because the captured string
// has to be the one the watcher will parse.
//
// TWO THINGS THIS HAS TO GET RIGHT
//
// 1. A combination is pressed over TIME, not at once. Holding a modifier and
//    then hitting a button means the held set grows: {rctrl} then
//    {rctrl, joy3.b5}. Committing the first thing seen captures the modifier
//    alone. So the set is accumulated and only committed once it stops
//    growing, or once you start letting go.
//
// 2. The browser must not act on the keys. The settings tab is opened by a
//    <button>, which keeps focus - so a keypress arriving while detect is
//    listening can activate it and close the panel out from under you. Every
//    key event is swallowed at the capture phase until detect finishes.
let detectTimer=null, detectSwallow=null;

function swallow(ev){ ev.preventDefault(); ev.stopPropagation(); }

function stopDetect(msg){
  if(detectTimer){ clearInterval(detectTimer); detectTimer=null; }
  if(detectSwallow){
    for(const t of ['keydown','keyup','keypress'])
      window.removeEventListener(t, swallow, true);
    detectSwallow=null;
  }
  document.querySelectorAll('.detect.listening').forEach(b=>{
    b.classList.remove('listening'); b.textContent='detect';
  });
  if(msg) say(msg);
}

// Order the parts so the same gesture always produces the same string:
// modifiers first, then joystick buttons, then plain keys.
function sortParts(parts){
  const rank=x=>/^(l|r)(ctrl|shift|alt)$/.test(x)?0:(x.startsWith('joy')?1:2);
  return [...parts].sort((a,b)=>rank(a)-rank(b)||a.localeCompare(b));
}

async function startDetect(key, btn){
  stopDetect();
  btn.classList.add('listening');
  btn.textContent='press it…';
  // Nothing in the page should have focus while keys are flying.
  btn.blur();
  if(document.activeElement && document.activeElement.blur)
    document.activeElement.blur();
  for(const t of ['keydown','keyup','keypress'])
    window.addEventListener(t, swallow, true);
  detectSwallow=true;

  const input=document.getElementById('set_'+key);
  // Measured in MILLISECONDS, not polls: the browser throttles this interval
  // to roughly 300ms in practice rather than the 120ms asked for, so a
  // poll-count settle window silently means something different from what it
  // reads as.
  const SETTLE_MS=700;
  let tries=0, baseline=null, best=new Set(), steadySince=0, stableMap={};

  const commit=()=>{
    if(!best.size){ stopDetect('nothing detected'); return; }
    const parts=sortParts(best);
    const spec=parts.join('+');
    input.value=spec;
    input.dispatchEvent(new Event('change',{bubbles:true}));
    const alt=parts.map(p=>stableMap[p]||p).join('+');
    // A joy-to-key modifier shows up twice: the physical button AND the key it
    // emits. Requiring both is usually harmless, but if the emitted key is a
    // tap rather than a hold they will never be true together and the binding
    // will never fire. Worth saying so at capture time rather than in-flight.
    const hasJoy=parts.some(p=>p.startsWith('joy'));
    const plainKeys=parts.filter(p=>!p.startsWith('joy')&&
                                    !/^(l|r)(ctrl|shift|alt)$/.test(p));
    const warn = (parts.length>2 && hasJoy && plainKeys.length)
      ? `  — check it with:  python inputs.py --test "${spec}"` : '';
    stopDetect(`bound to ${spec}`+(alt!==spec?` — stable form: ${alt}`:'')+warn);
  };

  detectTimer=setInterval(async ()=>{
    if(++tries>250){ stopDetect('detect timed out'); return; }
    let j;
    try{ j=await fetch('/api/inputs',{cache:'no-store'}).then(r=>r.json()); }
    catch(e){ stopDetect('detect failed: '+e.message); return; }
    if(!j.ok){ stopDetect('detect failed: '+(j.error||'')); return; }

    const now=new Set([...(j.mods||[]),
                       ...(j.buttons||[]).map(b=>b.index),
                       ...(j.keys||[]).map(k=>k.toLowerCase())]);
    for(const b of (j.buttons||[])) stableMap[b.index]=b.stable;

    // Whatever was ALREADY held when you clicked detect is not what you are
    // binding - two of these sticks have reported button 1 as permanently down.
    if(baseline===null){
      baseline=now;
      if(now.size)
        say('ignoring '+[...now].join(', ')+' — already held when detect started');
      return;
    }
    const fresh=new Set([...now].filter(x=>!baseline.has(x)));

    // Compared by CONTENTS, not size: {rctrl, r} and {rctrl, joy0.b5} are both
    // size 2, so a size test reads a completely different gesture as "steady".
    const same = fresh.size===best.size && [...fresh].every(x=>best.has(x));
    if(!same && [...best].every(x=>fresh.has(x))){
      best=fresh; steadySince=Date.now();            // grew: still building
      btn.textContent='… '+sortParts(best).join('+');
      return;
    }
    if(!best.size){ best=fresh; steadySince=Date.now(); return; }
    if(!same){ commit(); return; }        // changed or shrank: that was it
    if(Date.now()-steadySince >= SETTLE_MS) commit();   // held steady
  },120);
}

// The ignore list, with a way back out. A filter you cannot see is a filter
// that will one day be silently suppressing a real player, and the hit count is
// how you notice: a rule with thousands of hits is doing more than you meant.
function ignoredHTML(){
  const rows=data.stats.ignored||[];
  if(!rows.length) return `<div class="job"><h3>Ignored labels</h3>
    <p>Nothing ignored. Open a contact that is not a person — a salvage crate,
       a mission marker — and use <b>not a player</b> in its window.</p></div>`;
  return `<div class="job"><h3>Ignored labels — ${rows.length}</h3>
    <p>These are never reported. Matching covers OCR variants of the same
       string, so one entry catches both spellings of a misread.</p>
    <div class="iglist">${rows.map(r=>`
      <div class="igrow">
        <span class="iglabel">${handleHTML(r.label,false)}</span>
        <span class="ignote">${esc(r.note||'')}</span>
        <span class="ighits">${r.hits} hit${r.hits===1?'':'s'}</span>
        <button class="igdel" data-unignore="${esc(r.label)}">un-ignore</button>
      </div>`).join('')}</div></div>`;
}

// Flagged orgs, and how far each flag actually reaches.
//
// A flag on its own only reaches players whose PROFILE names the org, and the
// API's user endpoint returns a main org only. So for an affiliate org - the
// likely arrangement for a piracy side-org, which is the case this feature was
// built for - a flag with no member list reaches nobody at all. That state
// used to be invisible: the header counted the flag and nothing said it was
// inert. It is now the first thing each row says.
function orgsHTML(){
  const rows=data.stats.flagged_orgs_list||[];
  const status=data.stats.rosters||{};
  const list = !rows.length
    ? `<p class="blurb">No orgs flagged. Flag one and every member of it that
         you meet is marked, without needing a report against each person.</p>`
    : rows.map(o=>{
        const pulled = o.roster_ts
          ? `${o.roster_n} member${o.roster_n===1?'':'s'}, pulled ${ago(o.roster_ts)}`
          : `<b class="noroster">no member list pulled</b>`;
        const reach = o.roster_ts
          ? `reaches ${o.reaches} contact${o.reaches===1?'':'s'} you have seen`
          : `reaches only players whose profile names it`;
        const msg = status[o.sid] ? `<div class="orgmsg">${esc(status[o.sid])}</div>` : '';
        return `<div class="orgrow" data-sid="${esc(o.sid)}">
          <div class="orghead">
            <a class="org-name" target="_blank" rel="noopener"
               href="https://robertsspaceindustries.com/orgs/${encodeURIComponent(o.sid)}"
               >${esc(o.name||o.sid)}</a>
            <span class="org-tag">${esc(o.sid)}</span>
            <button class="orgpull" data-roster="${esc(o.sid)}">${
              o.roster_ts?'refresh members':'pull members'}</button>
            <button class="del" data-unflag="${esc(o.sid)}">unflag</button>
          </div>
          <div class="orgmeta">${pulled} &middot; ${reach}</div>
          ${o.note?`<div class="orgnote">${esc(o.note)}</div>`:''}
          ${msg}
        </div>`;
      }).join('');

  return `<div class="job"><h3>Flagged orgs<span id="orgcount">${
      rows.length?' — '+rows.length:''}</span></h3>
    <p>Flagging an org marks everyone in it. It records MEMBERSHIP, not blame —
       no report is written against anybody, because being in an org is a fact
       about someone and "they attacked me" is a claim about what they did.</p>
    <div class="orglist">${list}</div>
    <div class="orgadd">
      <input id="org-sid" placeholder="org SID, e.g. EXMPL" maxlength="32">
      <input id="org-note" placeholder="why (optional)" maxlength="200">
      <button id="org-flag">flag org</button>
      <div class="blurb">Pulling a member list is one request a second and up
        to 20 pages, so a large org takes a while. It runs in the background.</div>
    </div></div>`;
}

// The settings panel is deliberately NOT rebuilt when new data arrives - that
// is what used to wipe half-typed values mid-keystroke. But a roster pull runs
// on a server thread and finishes seconds after the click, so something has to
// notice. The org LIST is pure output; the only inputs on this page live
// outside it, in .orgadd. So refresh that one element and leave the rest of
// the panel, and whatever is typed into it, alone.
function refreshOrgList(){
  if(!settingsOpen || settingsCat!=='orgs') return;
  const cur=setPanel.querySelector('.orglist');
  if(!cur) return;
  const holder=document.createElement('div');
  holder.innerHTML=orgsHTML();
  const fresh=holder.querySelector('.orglist');
  if(fresh) cur.innerHTML=fresh.innerHTML;
  const n=setPanel.querySelector('#orgcount'),
        m=holder.querySelector('#orgcount');
  if(n && m) n.textContent=m.textContent;
}

function renderSettings(){
  const schema=data.stats.settings_schema||[];
  if(!schema.length){ setPanel.innerHTML='<div class="blurb">settings unavailable</div>'; return; }
  const cats=schema.concat([
    {key:'orgs',label:'Flagged orgs',
     blurb:'Orgs whose members are all marked, and whether each flag is '
          +'actually reaching anyone.', fields:[]},
    {key:'maintenance',label:'Maintenance',
     blurb:'One-off jobs. Each one shows you what it would do before it does it.',
     fields:[]}]);
  if(!cats.some(c=>c.key===settingsCat)) settingsCat=cats[0].key;

  setNav.innerHTML=cats.map(c=>
    `<button data-cat="${esc(c.key)}" aria-pressed="${c.key===settingsCat}">${esc(c.label)}</button>`
  ).join('');

  const cat=cats.find(c=>c.key===settingsCat);
  const showAdv=!!setVal('show_advanced');
  const fields=cat.fields.filter(f=>showAdv||!f.advanced);
  const hidden=cat.fields.length-fields.length;

  let body;
  if(cat.key==='orgs'){
    body=orgsHTML();
  }else if(cat.key==='maintenance'){
    body=diskHTML()+ignoredHTML()+JOBS.map(j=>`
      <div class="job" data-job="${j.id}">
        <h3>${esc(j.title)}</h3>
        <p>${esc(j.body)}</p>
        <button class="check" data-job="${j.id}">${j.direct?esc(j.verb):'check first'}</button>
        <div class="out"></div>
      </div>`).join('');
  }else{
    body=(cat.key==='storage'?diskHTML():'')+
      fields.map(f=>fieldHTML(f,setVal(f.key))).join('')+
      (hidden?`<div class="adv-toggle">${hidden} advanced setting${hidden===1?'':'s'} hidden ·
        <button id="adv-on">show advanced</button></div>`:'');
  }
  setPanel.innerHTML=`<h2>${esc(cat.label)}</h2>
    <div class="blurb">${esc(cat.blurb||'')}</div>${body}`;
}

setNav.addEventListener('click', ev=>{
  const b=ev.target.closest('button[data-cat]');
  if(!b) return;
  settingsCat=b.dataset.cat;
  stopDetect();
  renderSettings();
});

// 'change' rather than 'input': saving on every keystroke would write a
// half-typed number, and the server clamps - so typing 120 into a field
// capped at 100 would fight the cursor back to 100 mid-word.
setPanel.addEventListener('change', async ev=>{
  const el=ev.target.closest('[data-key]');
  if(!el) return;
  const key=el.dataset.key;
  const v = el.type==='checkbox' ? el.checked
          : (el.type==='number' ? Number(el.value) : el.value);
  try{
    const j=await post('/api/settings',{values:{[key]:v}});
    if(!j.ok) throw new Error(j.error||'save failed');
    data.stats.settings=j.values;
    const f=(data.stats.settings_schema||[]).flatMap(c=>c.fields).find(x=>x.key===key);
    const shown=j.values[key];
    say(`saved ${f?f.label.toLowerCase():key} = ${shown}`+
        (f&&f.restart?` — restart the ${f.restart} to apply`:''));
    // Re-render only when the change alters what the form shows.
    applyTileSize();
    if(key==='show_advanced'){ renderSettings(); }
    else{
      const row=el.closest('.setrow');
      const isDefault=JSON.stringify(shown)===JSON.stringify(f.default);
      if(el.type!=='checkbox'&&String(shown)!==String(el.value)) el.value=shown;
      // The reset button's existence tracks "differs from default", so a save
      // that crosses that line has to add or remove it. Redrawing the whole
      // panel would steal focus from the control just used, so patch the row.
      if(row&&f&&(row.classList.contains('changed')!==!isDefault)){
        row.classList.toggle('changed',!isDefault);
        const old=row.querySelector('[data-reset]');
        if(isDefault){ if(old) old.remove(); }
        else if(!old){
          const b=document.createElement('button');
          b.className='reset'; b.dataset.reset=key;
          const d=f.type==='bool'?(f.default?'on':'off'):String(f.default);
          b.textContent=`reset · default ${d}`;
          row.firstElementChild.appendChild(b);
        }
      }
    }
  }catch(e){ say('could not save: '+e.message); }
});

setPanel.addEventListener('click', async ev=>{
  const pull=ev.target.closest('[data-roster]');
  if(pull){
    pull.disabled=true; pull.textContent='pulling…';
    try{
      const j=await post('/api/org/roster',{sid:pull.dataset.roster});
      say(j.message||j.error||'started');
      // The fetch runs on a server thread. A SUCCESS writes rows, which changes
      // the database mtime, which the stream notices - so the list refreshes on
      // its own. A FAILURE writes nothing and would therefore never appear, so
      // poll a few times as well and stop as soon as the status line changes.
      await poll(); renderSettings();
      const sid=pull.dataset.roster;
      const before=(data.stats.rosters||{})[sid];
      for(let i=0;i<12;i++){
        await new Promise(r=>setTimeout(r,2000));
        if(!settingsOpen||settingsCat!=='orgs') break;
        await poll();
        const now=(data.stats.rosters||{})[sid];
        if(now && now!==before){ say(now); break; }
      }
    }catch(e){ say('failed: '+e.message); pull.disabled=false; }
    return;
  }
  const unf=ev.target.closest('[data-unflag]');
  if(unf){
    if(unf.dataset.armed!=='1'){
      unf.dataset.armed='1'; unf.textContent='unflag? click again';
      setTimeout(()=>{ if(unf.isConnected){ unf.dataset.armed='';
        unf.textContent='unflag'; } }, 4000);
      return;
    }
    unf.disabled=true;
    try{
      const j=await post('/api/org/unflag',{sid:unf.dataset.unflag});
      if(!j.ok) throw new Error(j.error||'failed');
      say(j.message);
      await poll(); renderSettings();
    }catch(e){ say('failed: '+e.message); unf.disabled=false; }
    return;
  }
  if(ev.target.id==='org-flag'){
    const sid=(document.getElementById('org-sid').value||'').trim();
    const note=(document.getElementById('org-note').value||'').trim();
    if(!sid){ say('type an org SID first'); return; }
    ev.target.disabled=true;
    try{
      const j=await post('/api/org/flag',{sid, note});
      say(j.ok?(j.message||'flagged '+sid):('failed: '+(j.error||'')));
      await poll(); renderSettings();
    }catch(e){ say('failed: '+e.message); ev.target.disabled=false; }
    return;
  }
  const un=ev.target.closest('[data-unignore]');
  if(un){
    un.disabled=true;
    try{
      const j=await post('/api/unignore',{label:un.dataset.unignore});
      if(!j.ok) throw new Error(j.error||'failed');
      say(`${un.dataset.unignore} will be reported again`);
      await poll(); renderSettings();
    }catch(e){ say('failed: '+e.message); un.disabled=false; }
    return;
  }
  const det=ev.target.closest('[data-detect]');
  if(det){
    if(det.classList.contains('listening')) stopDetect('detect cancelled');
    else startDetect(det.dataset.detect, det);
    return;
  }
  const rst=ev.target.closest('[data-reset]');
  if(rst){
    const key=rst.dataset.reset;
    const f=(data.stats.settings_schema||[]).flatMap(c=>c.fields)
             .find(x=>x.key===key);
    if(!f) return;
    try{
      const j=await post('/api/settings',{values:{[key]:f.default}});
      if(!j.ok) throw new Error(j.error||'failed');
      data.stats.settings=j.values;
      say(`reset ${f.label.toLowerCase()} to ${f.default}`+
          (f.restart?` — restart the ${f.restart} to apply`:''));
      renderSettings();
      applyTileSize();
    }catch(e){ say('could not reset: '+e.message); }
    return;
  }
  if(ev.target.id==='adv-on'){
    await post('/api/settings',{values:{show_advanced:true}});
    data.stats.settings.show_advanced=true;
    renderSettings();
    return;
  }
  const btn=ev.target.closest('button[data-job]');
  if(!btn) return;
  const wrap=btn.closest('.job'), out=wrap.querySelector('.out');
  const job=btn.dataset.job;
  const spec=JOBS.find(x=>x.id===job)||{};
  // A 'direct' job destroys nothing, so the confirm step would be ceremony.
  const apply=btn.classList.contains('go')||!!spec.direct;
  btn.disabled=true;
  out.textContent = apply?'working…':'checking…';
  try{
    const j=job==='restart'
      ? await post('/api/restart-watcher',{})
      : await post('/api/maintenance',{job, apply});
    if(!j.ok) throw new Error(j.error||'failed');
    let html=`<b>${esc(j.message||'done')}</b>`;
    if((j.items||[]).length)
      html+=`<div class="list">`+j.items.map(t=>`<div>${esc(t)}</div>`).join('')+`</div>`;
    if((j.kept||[]).length)
      html+=`<div>held back, evidence would be lost:</div>`+
            `<div class="list">`+j.kept.map(t=>`<div>${esc(t)}</div>`).join('')+`</div>`;
    if((j.errors||[]).length)
      html+=`<div class="list">`+j.errors.map(t=>`<div>${esc(t)}</div>`).join('')+`</div>`;
    out.innerHTML=html;
    // A second, differently-worded button to actually do it. Nothing here
    // deletes on first click.
    if(!apply&&!spec.direct&&(j.count>0||job==='frames')){
      // (frames always offers the button: its dry run reports 0 when nothing
      //  is old enough yet, but the user may still want a sweep now.)
      const go=document.createElement('button');
      go.className='go'; go.dataset.job=job;
      go.textContent=(JOBS.find(x=>x.id===job)||{}).verb||'apply';
      out.appendChild(document.createTextNode(' '));
      out.appendChild(go);
    }
    if(apply){ await poll(); renderSettings(); }
  }catch(e){ out.textContent='failed: '+e.message; }
  btn.disabled=false;
});

// A render that was skipped used to be a render that never happened. The grid
// is only ever drawn from a stream frame, so bailing out because a text box had
// focus left the tile stale until some UNRELATED database write pushed the next
// frame - which is why a confirmed audit's amber button sat there afterwards.
// Now a skip is remembered and replayed once the reason for it is gone.
let renderPending=false;

function render(){
  // armedDel too, not just focus: arming a remove button changes nothing about
  // focus, so without this the 2s poll rebuilt the modal and silently disarmed
  // it - the second click then armed it again and it could never commit.
  if(openHandle && !lookupOpen && !editingInModal()) renderModal();
  grid.hidden=settingsOpen;
  setPane.hidden=!settingsOpen;
  if(settingsOpen){ empty.hidden=true; applyTileSize(); renderStats();
    refreshOrgList(); return; }
  if(editingInGrid() || !veil.hidden){ renderPending=true; return; }
  renderPending=false;
  const term=q.value.trim().toLowerCase();
  const onlyAlerts=fAlerts.getAttribute('aria-pressed')==='true';
  let list=data.contacts;
  if(onlyAlerts) list=list.filter(c=>c.state==='known'||c.flags.length);
  if(term) list=list.filter(c=>c.handle.toLowerCase().includes(term)||
    c.orgs.some(o=>(o.sid+' '+o.name).toLowerCase().includes(term)));
  applyTileSize();
  grid.replaceChildren(...list.map(tile));
  empty.hidden=list.length>0;
  renderStats();
}

// Applied from the state so a change in another tab lands here too.
function applyTileSize(){
  const s=data.stats.settings||{};
  const px=Number(s.tile_size)||380;
  document.documentElement.style.setProperty('--tile', px+'px');
  document.body.classList.toggle('no-square', s.tile_square===false);
}

function renderStats(){
  const s=data.stats||{};
  s_players.textContent=s.players??'–'; s_sightings.textContent=s.sightings??'–';
  s_reports.textContent=s.reports??'–'; s_orgs.textContent=s.flagged_orgs??'–';
  s_pending.textContent=s.pending??'–';
  s_audits.textContent=s.audits??'–';
  s_audits.parentElement.style.color=s.audits?'var(--cyan)':'';
  s_pending.parentElement.style.color=s.pending?'var(--amber)':'';
}

// Leaving a field is the moment a skipped render becomes safe. focusout fires
// before focus lands anywhere new, so the check is deferred a tick - otherwise
// tabbing between two inputs would redraw the grid out from under the cursor.
document.addEventListener('focusout', ()=>setTimeout(()=>{
  if(renderPending && !editingInGrid() && veil.hidden) render();
}, 0));
const s_players=document.getElementById('s-players'),
      s_sightings=document.getElementById('s-sightings'),
      s_reports=document.getElementById('s-reports'),
      s_orgs=document.getElementById('s-orgs'),
      s_pending=document.getElementById('s-pending'),
      s_audits=document.getElementById('s-audits'),
      updated=document.getElementById('updated');

async function poll(){
  try{
    const r=await fetch('/api/contacts',{cache:'no-store'});
    data=await r.json();
    if(data.error){say('error: '+data.error);}
    // Do not overwrite what an action just reported. Every handler used to set
    // a message and then call poll(), which replaced it with the clock before
    // it could be read - so a delete that worked looked like nothing happened.
    else if(!stickyUntil || Date.now()>stickyUntil){
      updated.textContent='updated '+new Date().toLocaleTimeString();}
    render();
  }catch(e){ say('disconnected — is ui_server.py running?'); }
}
// Push, not poll. Two reasons beyond the 80 MB/hour of identical JSON:
//
//  - Every render race this UI has had came from rebuilding the DOM on a timer
//    whether or not anything changed - wiped half-typed notes, closed the
//    category dropdown, disarmed confirm buttons. Rendering only on real change
//    removes the cause rather than guarding against it.
//  - setInterval is THROTTLED in a background tab, and this UI lives on a second
//    monitor behind a game. Measured during testing: the poll simply stopped and
//    the page went stale until it was fronted again. EventSource keeps delivering.
//
// poll() is kept for an immediate refresh after your own action, and as the
// fallback if the stream cannot connect.
let es=null;
function connect(){
  try{
    es=new EventSource('/api/stream');
  }catch(e){ setInterval(poll,2000); return; }
  es.onmessage=ev=>{
    try{ data=JSON.parse(ev.data); }catch(e){ return; }
    if(data.error){ say('error: '+data.error); return; }
    if(!stickyUntil || Date.now()>stickyUntil)
      updated.textContent='updated '+new Date().toLocaleTimeString();
    render();
  };
  es.onerror=()=>{
    // EventSource retries on its own; say so rather than looking frozen.
    if(es.readyState===EventSource.CLOSED)
      say('disconnected — is ui_server.py running?');
  };
}
poll().then(connect);
</script></body></html>
"""


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
