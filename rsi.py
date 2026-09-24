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
Player and organisation lookup: SENTRY first, starcitizen-api.com behind it.

TWO SOURCES, BECAUSE ONE WAS NOT ENOUGH. starcitizen-api.com is unmaintained
and spent an evening in September timing out for 25 s a request. SENTRY
(sentry.wildknightsquadron.com) is a maintained database of RSI citizens and
orgs - measured at 564k citizens, and at 173-533 ms for anyone already in it -
and it returns EVERY org a person is in, affiliates included, which the other
source never could. Each has its own rate limit and its own outage breaker, so
one failing never pauses the other. What each is trusted for:

  who someone is, and their orgs   SENTRY, then starcitizen-api.com
  that someone does NOT exist      starcitizen-api.com ONLY - see citizen()
  an org's focus and archetype     starcitizen-api.com ONLY - SENTRY does not
                                   carry them, and they are the piracy flag
  an org's roster                  SENTRY, then starcitizen-api.com

`rsi_source` in settings chooses between SENTRY-first and the old
starcitizen-api.com-only behaviour, which is the way back without a rollback.

Scraping robertsspaceindustries.com directly is against their terms, so this
goes through the community API instead - a documented service with its own
terms, an API key, and rate limiting, which is designed to be called
programmatically.

Worth being clear-eyed: that API sources its data from RSI, so using it does
not make the underlying collection something it wasn't. What it does change is
that we are an ordinary consumer of a public API rather than scraping a site
that asks us not to.

"Piracy" is one of RSI's own fixed org focus values, exposed here as
`focus.primary.name` / `focus.secondary.name`, so the flag reads a
self-declaration rather than inferring anything. A declared focus is roleplay,
not behaviour: plenty of piracy orgs play consensually and plenty of griefers
sit in an Exploration org or none at all. Treat it as context to show, never
as a verdict.

    setx SC_API_KEY your_key_here      (or put it in sc_api_key.txt)
    python rsi.py SOMEHANDLE

KNOWN GAP: the user endpoint returns only the MAIN organisation. Affiliates -
where a pirate side-org would more likely sit - are not exposed. Flagged orgs
are handled instead by pulling their member list once (see members()).
"""

from __future__ import annotations

import functools
import json
import os
import re
import sqlite3
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import asdict, dataclass, field
from pathlib import Path
import dbopen
import paths

API_HOST = "https://api.starcitizen-api.com"
USER_AGENT = "sc-watch/0.2 (personal Star Citizen contact tool)"
# How many roster pages one pull may ask for. Pages are small and not a
# fixed size - measured at 18 to 22 records each - so a 600-member org is
# thirty requests, and at one request a second the cap is really a time limit.
# 60 covers roughly 1,200 members; bigger than that, the pull says it stopped
# early rather than pretending it finished.
MEMBER_PAGE_CAP = 60

# SENTRY. No key. Paced and broken-circuited on its own, and identified
# honestly: it is run by volunteers, publishes no rate limit, and every copy of
# this tool adds load to it - the User-Agent says what is asking and where to
# find the people responsible.
SENTRY_HOST = "https://sentry.wildknightsquadron.com/api/v1"
SENTRY_USER_AGENT = ("sc-watch (Star Citizen contact tool; "
                     "+https://github.com/SC-Watch/sc-watch-tool)")
# A citizen SENTRY already holds answers in well under a second. One it does
# not hold makes it crawl RSI on the spot - measured at 26 to 40 s - and the
# other source answers in 4 to 6. So SENTRY gets 8 s: plenty for a hit, and a
# miss falls through rather than holding the queue. Its crawl carries on
# server-side, so the next time that handle is asked about, it is a hit.
SENTRY_TIMEOUT = 8.0
# The identity check, on the main loop - see exists(). Hits measured 170-533
# ms, so this is three times the slowest. A first try at 3 s cost a brand-new
# misread 9.2 s end to end, measured: 3 s of SENTRY crawling, then the other
# service. It is paid once per misread - "no such account" is then cached.
EXISTS_TIMEOUT = 1.5
SENTRY_PAGE_TIMEOUT = 20.0      # roster pages are served from its database
SENTRY_PAGE_SIZE = 100          # its maximum; 422 above it
SENTRY_MAX_PAGES = 40           # 4,000 members; larger rosters say "partial"
# How old SENTRY's copy of a profile may be and still count as an answer. It
# re-reads RSI on its own schedule and says when it last did, so this is a
# real age rather than a guess: a person pressing "look up" wants today, and
# anything else takes the same thirty days this cache does.
FRESH_WINDOW = 86400
SOURCES = ("sentry", "sci")      # sentry: SENTRY first. sci: the old way only.

CACHE_PATH = paths.data("rsi_cache.db")
# Beside your data, not beside the program: an installed build lives
# somewhere you cannot write, so a key file there could never be created.
KEY_FILE = paths.data("sc_api_key.txt")

CITIZEN_TTL = 30 * 86400
ORG_TTL = 30 * 86400
MISS_TTL = 365 * 86400  # a handle that doesn't exist rarely starts existing

MIN_INTERVAL = 1.0  # seconds between requests

# 'cache' lets the API serve a stored copy instead of hitting RSI live, which
# is both faster and kinder. 'auto' falls back to live when nothing is stored.
DEFAULT_MODE = "auto"

# RSI's fixed list of org focuses - the only values a focus can take, so a
# focus typed in by hand is picked from these rather than free text.
FOCUSES = ("Bounty Hunting", "Engineering", "Exploration", "Freelancing",
           "Infiltration", "Medical", "Piracy", "Resources", "Scouting",
           "Security", "Smuggling", "Social", "Trading", "Transport")

PIRACY_FOCUS = {"piracy"}
ADJACENT_FOCUS = {"smuggling", "infiltration"}
PIRACY_ARCHETYPE = {"syndicate"}

# Abandoned-ship serials never resolve to a citizen, so never ask.
SHIP_SERIAL_RE = re.compile(r"^[A-Z]{2}-\d")


def find_api_key() -> str | None:
    """Env var first, then a local file. Never hard-code it."""
    key = os.environ.get("SC_API_KEY", "").strip()
    if key:
        return key
    if KEY_FILE.exists():
        key = KEY_FILE.read_text(encoding="utf-8-sig").strip()
        if key:
            return key
    return None


@dataclass
class Org:
    sid: str
    name: str = ""
    members: int = 0
    archetype: str = ""
    commitment: str = ""
    primary_focus: str = ""
    secondary_focus: str = ""
    is_main: bool = False
    rank: str = ""
    redacted: bool = False

    @property
    def focuses(self) -> list[str]:
        return [f for f in (self.primary_focus, self.secondary_focus) if f]

    @property
    def piracy(self) -> bool:
        return any(f.lower() in PIRACY_FOCUS for f in self.focuses)

    @property
    def adjacent(self) -> list[str]:
        hits = [f for f in self.focuses if f.lower() in ADJACENT_FOCUS]
        if self.archetype.lower() in PIRACY_ARCHETYPE:
            hits.append(self.archetype)
        return hits


@functools.lru_cache(maxsize=4096)
def _enlisted_epoch(text: str) -> float | None:
    """Parse an enlistment date once per distinct string.

    Cached because the UI rebuilds every contact on every change, and an
    enlistment date is the most fixed thing about an account - re-running
    strptime over 120 unchanging strings cost 9.8 ms of a 50 ms page build.
    Keyed on the string, so it cannot go stale: a different date is a different
    key. Only the parse is cached; the age is still computed from the clock.
    """
    if not text:
        return None
    for fmt in ("%Y-%m-%dT%H:%M:%S.%f", "%Y-%m-%dT%H:%M:%S",
                "%Y-%m-%d", "%b %d, %Y", "%B %d, %Y"):
        try:
            return time.mktime(time.strptime(text, fmt))
        except (ValueError, OverflowError):
            continue
    return None


@dataclass
class Citizen:
    handle: str
    exists: bool = True
    moniker: str = ""
    enlisted: str = ""
    location: str = ""
    record: str = ""
    orgs: list[Org] = field(default_factory=list)
    redacted: bool = False
    unverified: bool = False  # seen, but no backend was available to check it
    fetched_at: float = 0.0

    @property
    def enlisted_days(self) -> int | None:
        epoch = _enlisted_epoch(self.enlisted)
        if epoch is None:
            return None
        return int((time.time() - epoch) / 86400)

    def summary(self) -> str:
        if self.unverified:
            return f"not checked - {lookup_url(self.handle)}"
        if not self.exists:
            return "no profile (NPC or bad read)"
        bits = []
        age = self.enlisted_days
        if age is not None:
            bits.append(f"enlisted {age}d ago" if age < 365
                        else f"enlisted {age // 365}y ago")
        for o in self.orgs:
            marks = (["PIRACY"] if o.piracy else []) + o.adjacent
            bits.append(f"{o.sid}[{'/'.join(marks)}]" if marks else o.sid)
        if self.redacted:
            bits.append("+REDACTED")
        elif not self.orgs:
            bits.append("no org")
        return " | ".join(bits)

    @property
    def flags(self) -> list[str]:
        out = []
        for o in self.orgs:
            if o.piracy:
                out.append(f"PIRACY: {o.sid} ({o.members} members)")
        for o in self.orgs:
            for a in o.adjacent:
                out.append(f"{a}: {o.sid}")
        age = self.enlisted_days
        if age is not None and age < 90:
            out.append(f"new account ({age}d)")
        return out


class Cache:
    def __init__(self, path: Path | None = None):
        # Resolved at CALL time. As a default argument this captured
        # CACHE_PATH when the class was defined, so pointing rsi.CACHE_PATH
        # at a copy for a test silently kept writing the real cache.
        path = Path(path) if path else CACHE_PATH
        self.db = dbopen.connect(path, row_factory=None)
        self.db.execute("""CREATE TABLE IF NOT EXISTS citizens (
            handle TEXT PRIMARY KEY, data TEXT, fetched_at REAL)""")
        self.db.execute("""CREATE TABLE IF NOT EXISTS orgs (
            sid TEXT PRIMARY KEY, data TEXT, fetched_at REAL)""")
        self.db.execute("""CREATE TABLE IF NOT EXISTS org_members (
            sid TEXT NOT NULL, handle TEXT NOT NULL, fetched_at REAL,
            PRIMARY KEY (sid, handle))""")
        # Lookups that FAILED - the service errored or could not be reached -
        # as opposed to lookups that answered "no such account", which are
        # cached as citizens like any other answer. Without this row the UI
        # could not tell "we never asked" from "we asked and it broke", and
        # both rendered as "not looked up yet". Cleared by put_citizen().
        self.db.execute("""CREATE TABLE IF NOT EXISTS lookup_failures (
            handle TEXT PRIMARY KEY, ts REAL, error TEXT,
            attempts INTEGER, gave_up INTEGER)""")
        # A profile SENTRY had that could not be made whole: an org on it whose
        # focus nobody here could supply. Kept APART from `citizens`, which
        # everything else trusts as a complete answer - a copy missing a focus
        # there would read as "not a piracy org" for thirty days. Only the page
        # reads this, to show what IS known and say what is not; before it
        # existed the tile showed nothing at all, as though nobody had asked.
        # Cleared by put_citizen(), like a failure. A table of its own rather
        # than a column, so a build from before it still opens this file.
        self.db.execute("""CREATE TABLE IF NOT EXISTS partial_citizens (
            handle TEXT PRIMARY KEY, data TEXT, observed_at REAL,
            reason TEXT, unknown TEXT)""")
        self.db.commit()

    def get_citizen(self, handle: str, any_age: bool = False) -> Citizen | None:
        """The stored profile, or None if there is none or it has expired.

        `any_age` returns it however old it is - the fallback for when the
        service cannot be asked. The time it was stored is attached as
        `stored_at`, so a caller can say how old it is.
        """
        row = self.db.execute(
            "SELECT data, fetched_at FROM citizens WHERE handle=?",
            (handle.upper(),)).fetchone()
        if not row:
            return None
        payload = json.loads(row[0])
        ttl = CITIZEN_TTL if payload.get("exists", True) else MISS_TTL
        if not any_age and time.time() - row[1] > ttl:
            return None
        orgs = [Org(**o) for o in payload.pop("orgs", [])]
        c = Citizen(orgs=orgs, **payload)
        c.stored_at = row[1]
        return c

    def put_citizen(self, c: Citizen, stored_at: float | None = None):
        """Store a profile. `stored_at` is when RSI was actually READ, if that
        is known and earlier than now - SENTRY says when it last looked. The
        30-day expiry and the "profile N days old" on a tile both count from
        this, so a copy SENTRY took three weeks ago is three weeks old here
        too, not a fresh answer with a new date on it."""
        when = min(time.time(), stored_at) if stored_at else time.time()
        self.db.execute("INSERT OR REPLACE INTO citizens VALUES (?,?,?)",
                        (c.handle.upper(), json.dumps(asdict(c)), when))
        # An answer supersedes any earlier failure - by the enricher's retry,
        # a later sighting, or a manual look-up from the UI - and any partial
        # copy.
        self.db.execute("DELETE FROM lookup_failures WHERE handle=?",
                        (c.handle.upper(),))
        self.db.execute("DELETE FROM partial_citizens WHERE handle=?",
                        (c.handle.upper(),))
        self.db.commit()

    def put_partial(self, c: Citizen, reason: str, unknown: list[str]):
        """Keep a profile that is missing an org's focus - see the table."""
        self.db.execute(
            """INSERT OR REPLACE INTO partial_citizens
               (handle, data, observed_at, reason, unknown) VALUES (?,?,?,?,?)""",
            (c.handle.upper(), json.dumps(asdict(c)),
             min(c.fetched_at or time.time(), time.time()), reason,
             json.dumps(unknown)))
        self.db.commit()

    def note_failure(self, handle: str, error: str, attempts: int,
                     gave_up: bool):
        self.db.execute(
            "INSERT OR REPLACE INTO lookup_failures VALUES (?,?,?,?,?)",
            (handle.upper(), time.time(), error, attempts, int(gave_up)))
        self.db.commit()

    def get_org(self, sid: str, any_age: bool = False) -> Org | None:
        row = self.db.execute("SELECT data, fetched_at FROM orgs WHERE sid=?",
                              (sid.upper(),)).fetchone()
        if not row or (not any_age and time.time() - row[1] > ORG_TTL):
            return None
        return Org(**json.loads(row[0]))

    def put_org(self, o: Org):
        self.db.execute("INSERT OR REPLACE INTO orgs VALUES (?,?,?)",
                        (o.sid.upper(), json.dumps(asdict(o)), time.time()))
        self.db.commit()

    def put_members(self, sid: str, handles: list[str], complete: bool = False):
        """Cache an org's member list, which member_orgs() hands to lookups.

        A FINISHED pull replaces the org's list. Rows used to be added and
        never removed, so everyone a roster had ever listed got the org handed
        back on every lookup of them - stored on their profile, where it reads
        as current membership, so somebody the roster says has LEFT showed as
        a member and "left, or hid it" was never said. An unfinished pull only
        adds: missing from half a list is not having left (the same rule as
        record_roster())."""
        now = time.time()
        if complete:
            self.db.execute("DELETE FROM org_members WHERE sid=?", (sid.upper(),))
        self.db.executemany(
            "INSERT OR REPLACE INTO org_members VALUES (?,?,?)",
            [(sid.upper(), h.upper(), now) for h in handles])
        self.db.commit()

    def drop_members(self, sid: str):
        """Forget an org's cached member list - for an org you unflagged, whose
        roster nothing will pull again to keep the list true."""
        self.db.execute("DELETE FROM org_members WHERE sid=?", (sid.upper(),))
        self.db.commit()

    def member_orgs(self, handle: str) -> list[str]:
        """Which cached org member lists contain this handle."""
        return [r[0] for r in self.db.execute(
            "SELECT sid FROM org_members WHERE handle=?", (handle.upper(),))]

    def stats(self) -> dict:
        q = lambda s: self.db.execute(s).fetchone()[0]
        return {"citizens": q("SELECT COUNT(*) FROM citizens"),
                "orgs": q("SELECT COUNT(*) FROM orgs"),
                "cached_members": q("SELECT COUNT(*) FROM org_members"),
                "bytes": CACHE_PATH.stat().st_size if CACHE_PATH.exists() else 0}


# Places to send a HUMAN. These are for opening in a browser, which is not
# scraping - it is a person viewing a public page the way it is published to
# be viewed. The terms question was about automated bulk fetching, and none of
# it applies to a link.
LOOKUP_URL = "https://sc-intelligence.net/search?q={handle}"
PROFILE_URL = "https://robertsspaceindustries.com/citizens/{handle}"
ORG_URL = "https://robertsspaceindustries.com/orgs/{sid}"


class NoKey(RuntimeError):
    """starcitizen-api.com asked with no key configured. Still a RuntimeError,
    as it always was; its own class so the reason can be said plainly - it
    reached the tile as "RuntimeError"."""


class ServiceDown(Exception):
    """Refused without asking: recent lookups kept failing, so the service
    is treated as down until `until` (time.monotonic()). Transient by
    definition - it is a statement about the service, not the handle."""

    def __init__(self, until: float):
        self.until = until
        super().__init__(f"lookups paused for {self.remaining:.0f}s")

    @property
    def remaining(self) -> float:
        return max(0.0, self.until - time.monotonic())


def is_transient(exc: BaseException) -> bool:
    """Would asking again later plausibly work?

    Yes for the service erroring (5xx), rate limiting (429), timeouts and
    failing to connect at all. Measured on 2026-09-21: four lookups in one
    session came back HTTP 500 after ~10 s, and all four answered normally
    when asked again - real accounts, not misreads. No for the other 4xx: a
    rejected API key is not going to start working in a minute.
    """
    if isinstance(exc, ServiceDown):
        return True
    if isinstance(exc, urllib.error.HTTPError):
        return exc.code >= 500 or exc.code == 429
    return isinstance(exc, (urllib.error.URLError, TimeoutError,
                            ConnectionError, OSError))


def describe_failure(exc: BaseException) -> str:
    """A short, plain reason a lookup failed, for the console and the tile."""
    if isinstance(exc, ServiceDown):
        return "the lookup service is not responding, so lookups are paused"
    if isinstance(exc, NoKey):
        return "needs a starcitizen-api.com API key"
    if isinstance(exc, urllib.error.HTTPError):
        if exc.code == 429:
            return "the lookup service is rate limiting (HTTP 429)"
        if exc.code >= 500:
            return f"the lookup service had an error (HTTP {exc.code})"
        if exc.code in (401, 403):
            return f"the API key was refused (HTTP {exc.code})"
        return f"HTTP {exc.code}"
    if isinstance(exc, TimeoutError) or "timed out" in str(exc).lower():
        return "the lookup service timed out"
    if isinstance(exc, (urllib.error.URLError, ConnectionError)):
        return "could not reach the lookup service"
    return type(exc).__name__


def lookup_url(handle: str) -> str:
    """Third-party lookup, useful when we have no org data of our own."""
    return LOOKUP_URL.format(handle=urllib.parse.quote(handle))


def profile_url(handle: str) -> str:
    """The player's own RSI page - for the second-screen 'open profile' button."""
    return PROFILE_URL.format(handle=urllib.parse.quote(handle))


def org_url(sid: str) -> str:
    return ORG_URL.format(sid=urllib.parse.quote(sid))


def _parse_utc(text) -> float | None:
    """An ISO timestamp from SENTRY, which it writes in UTC without saying so,
    as epoch seconds. None if there is none or it will not parse."""
    if not text:
        return None
    from datetime import datetime, timezone
    try:
        dt = datetime.fromisoformat(str(text).replace("Z", "+00:00"))
    except ValueError:
        return None
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    return dt.timestamp()


class _Paced:
    """One service's request pacing and outage breaker.

    Split out of Client when a second service arrived, so the two cannot share
    state they must not share: SENTRY timing out must never pause
    starcitizen-api.com, or the fallback would be switched off by the thing it
    is the fallback for. The attribute names are the ones Client always had.
    """

    # After this many transient failures in a row the service is treated as
    # down, and nothing is sent for DOWN_FIRST seconds, doubling each time it
    # is still down afterwards, up to DOWN_MAX.
    DOWN_AFTER = 3
    DOWN_FIRST, DOWN_MAX = 60.0, 300.0

    def _init_pacing(self, min_interval: float):
        self.min_interval = min_interval
        self._last = 0.0
        # Guards _last. See _wait_turn().
        self._slot_lock = threading.Lock()
        # Outage handling - see Client._get(). Guarded by _slot_lock.
        self._trouble = 0            # consecutive transient failures
        self._down_until = 0.0       # time.monotonic(); 0 = not paused
        self._pause = self.DOWN_FIRST
        self._probation = False      # paused before and not yet answered since

    def down_for(self) -> float:
        """Seconds until lookups resume; 0 if the service is not paused."""
        with self._slot_lock:
            return max(0.0, self._down_until - time.monotonic())

    def _check_paused(self):
        with self._slot_lock:
            if time.monotonic() < self._down_until:
                raise ServiceDown(self._down_until)

    def _went_ok(self):
        with self._slot_lock:
            self._trouble = 0
            self._down_until = 0.0
            self._pause = self.DOWN_FIRST
            self._probation = False

    def _went_wrong(self, retry_after: float | None = None):
        """Count a transient failure; pause when there have been enough.

        After a pause, the first request out is a probe: if it fails too, the
        service is still down and the next, longer pause starts at once - not
        after another DOWN_AFTER requests have each spent 25 s finding out.

        `retry_after` is a service TELLING us to wait (a 429 with Retry-After),
        which is not a guess to be accumulated towards: it pauses at once, for
        at least that long.
        """
        with self._slot_lock:
            self._trouble += 1
            if retry_after:
                self._down_until = max(self._down_until,
                                       time.monotonic() + float(retry_after))
                self._trouble = 0
                self._probation = True
                return
            if self._trouble >= self.DOWN_AFTER or self._probation:
                self._down_until = time.monotonic() + self._pause
                self._pause = min(self._pause * 2, self.DOWN_MAX)
                self._trouble = 0
                self._probation = True

    def _wait_turn(self):
        """Hold this request until min_interval after the previous one.

        The client is called from two threads - RSI lookups on the enricher,
        identity checks from the main loop - and the old check-sleep-stamp was
        not atomic: both threads read the same `_last`, both decided it was
        their turn, and both went out together. Measured with two threads, 140
        of 285 requests left closer together than the limit allows. Each caller
        now RESERVES a slot under the lock and sleeps outside it, so requests
        queue one interval apart without the lock being held across a sleep
        or a network call.
        """
        with self._slot_lock:
            slot = max(time.time(), self._last + self.min_interval)
            self._last = slot
        wait = slot - time.time()
        if wait > 0:
            time.sleep(wait)


@dataclass
class Roster:
    """One pull of an org's member list, and what can be concluded from it."""
    members: list                   # [{handle, display, rank, stars}], visible only
    complete: bool                  # the walk reached the end of the list
    source: str | None              # "sentry" | "sci"
    total: int | None = None        # the org's own count, hidden members included
    hidden: int = 0                 # members who hide their membership
    redacted: int = 0               # ... or whose org entry is redacted
    observed_at: float | None = None


class Sentry(_Paced):
    """sentry.wildknightsquadron.com: no key, fast on a hit, full org lists.

    Only a transport and a reader. What its answers are trusted FOR is decided
    in Client, which is the one place both sources meet.
    """

    def __init__(self, min_interval: float = MIN_INTERVAL):
        self._init_pacing(min_interval)

    def _fetch(self, url: str, timeout: float):
        """One GET, parsed. Separate so tests can fake the network."""
        req = urllib.request.Request(url, headers={
            "User-Agent": SENTRY_USER_AGENT, "Accept": "application/json"})
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return json.loads(r.read().decode("utf-8", "replace"))

    def _call(self, path: str, timeout: float = SENTRY_TIMEOUT,
              force: bool = False):
        """A parsed answer, or None for 404. Raises on anything else."""
        if not force:
            self._check_paused()
        self._wait_turn()
        try:
            payload = self._fetch(SENTRY_HOST + path, timeout)
        except urllib.error.HTTPError as e:
            if e.code == 404:
                self._went_ok()
                return None
            if is_transient(e):
                retry = None
                if e.code == 429:
                    try:
                        retry = float(e.headers.get("Retry-After") or 0) or None
                    except (TypeError, ValueError):
                        retry = None
                self._went_wrong(retry)
            raise
        except Exception as e:
            if is_transient(e):
                self._went_wrong()
            raise
        self._went_ok()
        return payload

    def citizen(self, handle: str, force: bool = False,
                timeout: float = SENTRY_TIMEOUT) -> dict | None:
        """SENTRY's record of a citizen, or None if it has none."""
        return self._call(f"/citizens/{urllib.parse.quote(handle)}",
                          timeout=timeout, force=force)

    def roster(self, sid: str, on_page=None) -> Roster | None:
        """An org's member list, walked to the end. None if SENTRY has no
        such org. Hidden and redacted members are COUNTED, never listed: they
        arrive as placeholders called [HIDDEN] and [REDACTED], which are not
        org SIDs or handles, and SENTRY's own documentation warns that real
        orgs with those names exist."""
        out, hidden, redacted = [], 0, 0
        total = observed = None
        page, pages = 1, 1
        while page <= pages:
            if page > SENTRY_MAX_PAGES:
                return Roster(out, False, "sentry", total, hidden, redacted,
                              observed)
            d = self._call(
                f"/orgs/{urllib.parse.quote(sid)}/memberships"
                f"?per_page={SENTRY_PAGE_SIZE}&page={page}",
                timeout=SENTRY_PAGE_TIMEOUT)
            if d is None:
                return None if page == 1 else Roster(
                    out, False, "sentry", total, hidden, redacted, observed)
            m = d.get("memberships", d) if isinstance(d, dict) else {}
            if not isinstance(m, dict):
                return Roster(out, False, "sentry", total, hidden, redacted,
                              observed)
            pages = int(m.get("pages") or 1)
            total = m.get("total", total)
            if observed is None:
                observed = _parse_utc((d.get("metadata") or {}).get("last_fetched"))
            for r in m.get("members") or []:
                vis = (r.get("visibility") or "visible").lower()
                h = (r.get("citizen_handle") or "").strip()
                if vis == "hidden":
                    hidden += 1
                elif vis != "visible" or not h or h.startswith("["):
                    redacted += 1
                else:
                    out.append({"handle": h,
                                "display": r.get("display_name") or "",
                                "rank": r.get("member_rank") or "",
                                "stars": r.get("rank_number") or 0})
            if on_page is not None:
                try:
                    on_page(page, len(out))
                except Exception:
                    pass                # a progress line must never stop a pull
            page += 1
        return Roster(out, True, "sentry", total, hidden, redacted, observed)


class Client(_Paced):
    """Profile lookup with a swappable backend.

    Two backends have already proven fragile - direct RSI scraping is against
    their terms, and starcitizen-api.com is unmaintained (its host still
    answers, but nobody is fixing it if it stops). So the source is a
    parameter, and everything downstream - the cache, the dataclasses, the
    piracy flagging - is shared between them.

    provider:
      'api'   look people up: SENTRY first unless `sentry` is off, then
              starcitizen-api.com if there is a key. SENTRY needs none.
      'link'  no network at all. Records the handle and hands back a URL for
              looking it up by hand. Loses the piracy flag; loses nothing else.
      'none'  disabled entirely.

    `sentry`: None reads `rsi_source` from settings, which is how everything
    in the tool gets it. Tests pass False to pin the old behaviour exactly.
    """

    def __init__(self, cache: Cache | None = None, api_key: str | None = None,
                 mode: str = DEFAULT_MODE, min_interval: float = MIN_INTERVAL,
                 provider: str = "api", sentry: bool | None = None):
        self.cache = cache or Cache()
        self.api_key = api_key or find_api_key()
        self.mode = mode
        self.provider = provider
        self._init_pacing(min_interval)
        if sentry is None:
            try:
                import settings as _settings
                sentry = _settings.load().get("rsi_source", "sentry") != "sci"
            except Exception:
                sentry = True
        self.sentry = Sentry(min_interval) if sentry else None

    @property
    def ready(self) -> bool:
        if self.provider == "none":
            return False
        if self.provider == "link":
            return True
        return bool(self.api_key) or self.sentry is not None

    @property
    def enriches(self) -> bool:
        """Whether this backend returns org data, as opposed to just a link.

        True without an API key when SENTRY is on, because SENTRY needs none.
        What a key still buys is `confirms_absence` and org focus - see there.
        """
        return self.provider == "api" and (bool(self.api_key)
                                           or self.sentry is not None)

    @property
    def confirms_absence(self) -> bool:
        """Can this client say a handle does NOT exist?

        Only starcitizen-api.com is asked that, so only with a key. "No such
        account" is what raises a misread audit, what purge_ghosts deletes on,
        and what lets two spellings be treated as one person - so it is taken
        from the service that asks RSI live, never from a database's silence.
        """
        return self.provider == "api" and bool(self.api_key)

    def _call(self, path: str, mode: str | None = None,
              force: bool = False) -> dict | None:
        """One rate-limited request, parsed, with the outage breaker applied.

        Returns the WHOLE payload so the caller can decide what an empty
        answer means - which differs between an endpoint that returns one
        record and one that returns a page of them. None is a 404 only.

        Split out of _get() rather than duplicated, because everything here -
        the key check, the pause, the slot reservation, counting a failure
        towards the breaker - has to behave identically for every endpoint.
        """
        if not self.api_key:
            raise NoKey(
                "no API key. Get one at https://starcitizen-api.com, then set "
                "SC_API_KEY or write it to sc_api_key.txt")
        if not force:
            with self._slot_lock:
                if time.monotonic() < self._down_until:
                    raise ServiceDown(self._down_until)
        self._wait_turn()
        url = f"{API_HOST}/{self.api_key}/v1/{mode or self.mode}/{path}"
        try:
            payload = self._fetch(url)
        except urllib.error.HTTPError as e:
            if e.code == 404:
                self._went_ok()
                return None
            if is_transient(e):
                self._went_wrong()
            raise
        except Exception as e:
            if is_transient(e):
                self._went_wrong()
            raise
        self._went_ok()
        return payload

    def _get(self, path: str, mode: str | None = None, force: bool = False):
        """Rate-limited GET returning parsed JSON, or None on 404//failure.

        `mode` overrides the client default for one call, because not every
        endpoint exists in every mode - see members().

        OUTAGES. Measured on 2026-09-21: the service answered HTTP 500 for a
        while, then stopped answering entirely - six lookups in a row each
        took the full 25 s timeout, and its own front page did not respond in
        15 s. Every request sent into that is 25 s of the lookup queue gone,
        and worse on the main loop, which calls this for identity checks. So
        after DOWN_AFTER transient failures in a row, requests are refused
        at once with ServiceDown for a while instead of being sent. The
        enricher HOLDS handles through the pause rather than failing them;
        identity checks fail fast into their safe path ("treat as distinct").

        `force` skips the pause - for a person pressing "look up", who is
        asking precisely whether it is back. A success ends the pause.
        """
        payload = self._call(path, mode, force)
        if payload is None:
            return None
        # The API answers 200 with success:0 for a missing record, so the
        # HTTP status alone is not enough to tell "absent" from "found".
        if not payload.get("success") or payload.get("data") in (None, [], {}):
            return None
        return payload["data"]

    def _fetch(self, url: str):
        """One GET, parsed. Separate from _get() so tests can fake the network
        without faking the rate limit."""
        req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT,
                                                   "Accept": "application/json"})
        with urllib.request.urlopen(req, timeout=25) as r:
            return json.loads(r.read().decode("utf-8", "replace"))

    def org(self, sid: str, force: bool = False) -> Org:
        cached = self.cache.get_org(sid)
        if cached:
            return cached
        try:
            data = self._get(f"organization/{urllib.parse.quote(sid)}", force=force)
        except Exception:
            # Same fallback as citizen(): an expired copy of an org beats a
            # lookup that fails outright. With no copy at all this still
            # raises, rather than storing an org with no focus - that would
            # hide a PIRACY focus for thirty days.
            if not force:
                old = self.cache.get_org(sid, any_age=True)
                if old is not None:
                    return old
            raise
        org = Org(sid=sid.upper())
        if data:
            focus = data.get("focus") or {}
            org.name = data.get("name") or sid
            org.members = int(data.get("members") or 0)
            org.archetype = data.get("archetype") or ""
            org.commitment = data.get("commitment") or ""
            org.primary_focus = ((focus.get("primary") or {}).get("name")) or ""
            org.secondary_focus = ((focus.get("secondary") or {}).get("name")) or ""
        self.cache.put_org(org)
        return org

    def citizen(self, handle: str, fresh: bool = False) -> Citizen:
        """A profile, from the cache when it has one unless `fresh`.

        `fresh` is for a person pressing "look up" on a contact. The cache
        keeps "no such account" for a YEAR, so without it a forced lookup of
        a handle you know is right would only hand back the old answer.

        WHICH SOURCE, AND WHAT EACH ONE IS TRUSTED FOR

          1. SENTRY. Its answer is taken when it has the person, its copy is
             recent enough (FRESH_WINDOW when `fresh`, CITIZEN_TTL otherwise),
             and every org on it could be given its focus. That is the fast
             path, and the one that sees affiliate orgs.
          2. starcitizen-api.com, for everything else - which includes every
             case where SENTRY does not have the person. SENTRY not having
             someone is a database's silence, not RSI saying they do not
             exist; "no such account" drives misread audits, ghost purges and
             identity merges, so it is only ever taken from the source that
             asks RSI live.
          3. If that fails too: SENTRY's copy if there was one, marked with
             why it could not be confirmed; else the expired copy in our own
             cache, marked the same way; else the error.

        Without SENTRY (`rsi_source = sci`) the order is the one it was before
        SENTRY existed: step 2, then our own expired copy. (One difference, on
        purpose: a failure fetching the MAIN org's details now falls back to
        the expired copy too, where it used to escape as an error.)
        """
        handle = handle.strip()
        if SHIP_SERIAL_RE.match(handle.upper()):
            return Citizen(handle=handle.upper(), exists=False)

        if not self.enriches:
            # No backend: report the handle as seen but unverified rather than
            # claiming it doesn't exist, which would be a different and wrong
            # statement. Local reputation still applies; only org data is lost.
            return Citizen(handle=handle.upper(), exists=True,
                           unverified=True, fetched_at=time.time())

        cached = None if fresh else self.cache.get_citizen(handle)
        if cached:
            return cached
        return self._resolve(handle, fresh, self.sentry)

    def _resolve(self, handle: str, fresh: bool, sentry) -> Citizen:
        """citizen() past the cache. `sentry` is None when the caller has
        already asked SENTRY and had nothing - see exists()."""
        found, sentry_err = None, None
        if sentry is not None:
            try:
                found = self._citizen_sentry(handle, fresh)
            except Exception as exc:
                sentry_err = exc
            window = FRESH_WINDOW if fresh else CITIZEN_TTL
            if (found is not None and not getattr(found, "stale_reason", None)
                    and time.time() - found.fetched_at <= window):
                found.source = "sentry"
                self.cache.put_citizen(found, stored_at=found.fetched_at)
                return found

        if self.confirms_absence:
            try:
                c = self._citizen_sci(handle, fresh, store=found is None)
                c.source = "sci"
                if found is not None:
                    c = self._keep_affiliates(c, found)
                return c
            except Exception as exc:
                err = exc
        else:
            err = sentry_err
            if found is None and err is None:
                # SENTRY has never heard of them and nothing here can ask RSI.
                # That is not "no such account" - see above. Seen, unverified.
                return Citizen(handle=handle.upper(), exists=True,
                               unverified=True, fetched_at=time.time())

        if found is not None:
            # SENTRY knew them; the confirmation could not be had. Its copy is
            # most of the answer - marked, and stored only if it is whole:
            # a copy missing an org's focus must never sit in the cache for
            # thirty days looking like "not a piracy org".
            found.source = "sentry"
            partial = getattr(found, "stale_reason", None)
            reason = describe_failure(err) if err else "not confirmed"
            # Once: the org focus usually failed for the same reason, and
            # "...timed out, so ...; the lookup service timed out" says it twice.
            found.stale_reason = (reason if not partial else partial
                                  if reason in partial else f"{partial}; {reason}")
            found.stored_at = found.fetched_at
            if not partial:
                self.cache.put_citizen(found, stored_at=found.fetched_at)
            else:
                # Not dropped: kept apart, where only the page reads it.
                self.cache.put_partial(found, partial,
                                       getattr(found, "unknown_focus", []))
            return found

        # EXPIRED IS BETTER THAN NOTHING. A stored profile past its 30 days
        # used to be dropped the moment the service failed to replace it, so
        # on a bad evening a player known to be in a piracy org came back as
        # "not looked up". Orgs and account age change slowly; a profile from
        # six weeks ago is still most of the answer.
        #
        # Marked, never passed off as current: `stale_reason` says why it
        # could not be refreshed, `stored_at` says when it is from. Not for
        # `fresh`, which is a person asking what RSI says NOW - they get the
        # error, and the tile still shows the old copy anyway.
        if not fresh:
            old_copy = self.cache.get_citizen(handle, any_age=True)
            if old_copy is not None:
                old_copy.stale_reason = describe_failure(err)
                return old_copy
        raise err

    def exists(self, handle: str) -> bool:
        """Is this a real account? The identity check's question, and ONLY that.

        Asked on the main loop, mid-burst, whenever a new reading looks like a
        contact already on file. It used to build a whole profile to answer a
        yes/no - and with SENTRY returning every org a person is in, a whole
        profile can mean fetching details for six orgs. SENTRY having the
        person is a yes on its own, answered in a quarter of a second. Anything
        else goes the full way, because only that can say no.
        """
        handle = handle.strip()
        if SHIP_SERIAL_RE.match(handle.upper()):
            return False
        if not self.enriches:
            return True
        c = self.cache.get_citizen(handle)
        if c:
            return c.exists
        if self.sentry is not None:
            try:
                # A shorter budget than a lookup gets: this blocks the watcher.
                # A hit is a quarter of a second; anything slower is SENTRY
                # crawling a name it has never seen, which is usually a misread.
                if self.sentry.citizen(handle, timeout=EXISTS_TIMEOUT) is not None:
                    return True
            except Exception:
                pass
        # SENTRY has been asked; do not ask it again on the way through.
        return self._resolve(handle, False, None).exists

    def _org_details(self, sid: str, fresh: bool,
                     why: list[str] | None = None) -> Org | None:
        """An org WITH its focus and archetype, or None if none can be had.

        SENTRY does not carry focus. starcitizen-api.com does, and so does our
        own cache - any age, because an org's declared focus is about the most
        stable thing in this data. None means nobody here can say whether it
        is a piracy org, which callers must not quietly render as "it is not".

        Any age INCLUDING on a fresh lookup. org() skips its expired copy when
        forced, so a person pressing "look up" while the other service was
        down got "focus unknown" for an org whose focus was sitting right here.
        `why` collects the reason when there is nothing to give.
        """
        try:
            return self.org(sid, force=fresh)
        except Exception as exc:
            old = self.cache.get_org(sid, any_age=True)
            if old is None and why is not None:
                why.append(describe_failure(exc))
            return old

    def _citizen_sentry(self, handle: str, fresh: bool) -> Citizen | None:
        """SENTRY's record as a Citizen, or None if SENTRY does not have them.

        `stale_reason` is set if any org's focus could not be found - the
        record is then usable for the moment and must not be cached.
        """
        d = self.sentry.citizen(handle, force=fresh)
        if not d or not isinstance(d, dict):
            return None
        prof = d.get("profile") or {}
        if not prof:
            return None
        meta = d.get("metadata") or {}
        observed = (_parse_utc(meta.get("last_fetched"))
                    or _parse_utc(prof.get("last_fetched")) or time.time())
        record = str(prof.get("citizen_record") or "")
        if record and not record.startswith("#"):
            record = "#" + record
        c = Citizen(handle=handle.upper(), fetched_at=min(observed, time.time()),
                    moniker=prof.get("display_name") or handle,
                    enlisted=prof.get("enlisted") or "",
                    location=(prof.get("location") or "").strip(),
                    record=record)
        mems = d.get("memberships") or []
        if isinstance(mems, dict):
            mems = mems.get("members") or []
        unknown, why = [], []
        for m in mems:
            vis = (m.get("visibility") or "visible").lower()
            sid = (m.get("organization_sid") or "").strip()
            if vis != "visible" or not sid or sid.startswith("["):
                # A membership that exists and does not say where. The
                # placeholder is NOT an org SID - real orgs called HIDDEN and
                # REDACTED exist - so it becomes a fact about the person.
                c.redacted = True
                continue
            org = self._org_details(sid.upper(), fresh, why)
            if org is None:
                unknown.append(sid.upper())
                org = Org(sid=sid.upper())
            org.is_main = bool(m.get("is_main"))
            org.rank = m.get("member_rank") or ""
            c.orgs.append(org)
        c.orgs.sort(key=lambda o: not o.is_main)      # main first, as before
        for extra_sid in self.cache.member_orgs(handle):
            if extra_sid not in {o.sid.upper() for o in c.orgs}:
                org = self._org_details(extra_sid, fresh, why)
                if org is None:
                    unknown.append(extra_sid)
                    org = Org(sid=extra_sid)
                c.orgs.append(org)
        if unknown:
            # With the cause: "the lookup service had an error" and "needs an
            # API key" ask different things of the person reading it.
            c.stale_reason = (f"no org focus for {', '.join(unknown)}: "
                              f"{why[0] if why else 'not available'}, so "
                              "whether it is a piracy org is unknown")
            c.unknown_focus = unknown
            c.stored_at = c.fetched_at
        return c

    def _keep_affiliates(self, c: Citizen, found: Citizen) -> Citizen:
        """The other service's answer, plus the affiliate orgs SENTRY listed,
        stored.

        It reports a MAIN org only, so its silence about affiliates is not
        evidence they are gone - the rule record_roster() already keeps: a
        source can only retire what it can list. Reported from use: a lookup
        that failed and then worked wiped the affiliates off the tile, because
        a forced lookup takes SENTRY only if its copy is under a day old and
        otherwise stored this answer over SENTRY's. Its main org wins - it
        asked RSI just now - and SENTRY's old main is not carried along as
        though it were an affiliate.

        An affiliate carried without a known focus makes the record partial,
        stored apart like any other (put_partial): whole, it would sit in the
        cache reading as "not a piracy org".
        """
        if not c.exists:
            self.cache.put_citizen(c)
            return c
        have = {o.sid.upper() for o in c.orgs}
        unknown = {s.upper() for s in (getattr(found, "unknown_focus", None) or [])}
        still_unknown = []
        for o in found.orgs:
            sid = o.sid.upper()
            if o.is_main or sid in have:
                continue
            c.orgs.append(o)
            have.add(sid)
            if sid in unknown:
                still_unknown.append(sid)
        c.redacted = c.redacted or found.redacted
        if still_unknown:
            c.stale_reason = (f"no org focus for {', '.join(still_unknown)}, so "
                              "whether it is a piracy org is unknown")
            c.unknown_focus = still_unknown
            c.stored_at = c.fetched_at
            self.cache.put_partial(c, c.stale_reason, still_unknown)
        else:
            self.cache.put_citizen(c)
        return c

    def _citizen_sci(self, handle: str, fresh: bool, store: bool = True) -> Citizen:
        """starcitizen-api.com's answer, and the only source of "no account".

        Raises on failure; the fallbacks are citizen()'s to decide. `store`
        False leaves storing to the caller - _resolve() does, once it has
        folded in what SENTRY knew.
        """
        # `fresh` is a person asking; it also gets past an outage pause.
        data = self._get(f"user/{urllib.parse.quote(handle)}", force=fresh)
        if not data:
            c = Citizen(handle=handle.upper(), exists=False, fetched_at=time.time())
            if store:
                self.cache.put_citizen(c)
            return c

        profile = data.get("profile") or {}
        c = Citizen(handle=handle.upper(), fetched_at=time.time(),
                    moniker=profile.get("display") or handle,
                    enlisted=profile.get("enlisted") or "",
                    record=str(profile.get("id") or ""))

        main = data.get("organization") or {}
        sid = main.get("sid")
        if sid:
            full = self.org(sid, force=fresh)
            full.is_main = True
            full.rank = main.get("rank") or ""
            c.orgs = [full]
        else:
            # Three states, and they are not the same claim about a person:
            #   {"sid": "MERRILLS", ...}     visible org
            #   {"name": "", "stars": 0}     org exists but hidden -> redacted
            #   {"stars": 0}                 genuinely no org
            # Treating any non-empty block as redacted reported org-less
            # players as hiding something, which is a different accusation.
            c.redacted = "name" in main

        # Affiliates aren't exposed by this API. Flagged orgs whose member
        # lists we've pulled can still be matched locally.
        for extra_sid in self.cache.member_orgs(handle):
            if extra_sid not in {o.sid.upper() for o in c.orgs}:
                c.orgs.append(self.org(extra_sid, force=fresh))

        if store:
            self.cache.put_citizen(c)
        return c

    def roster(self, sid: str, max_pages: int = MEMBER_PAGE_CAP,
               on_page=None) -> Roster:
        """An org's member list: SENTRY first, starcitizen-api.com behind it.

        SENTRY measured as a strict superset of the other source on the same
        org the same afternoon - 511 handles against 463, none the other way -
        in six requests instead of nineteen, and it counts the hidden members
        the other leaves out entirely. `source` travels with the result,
        because a roster from one source must never be compared with a roster
        from the other to decide who has LEFT - see record_roster().
        """
        sentry_err = None
        if self.sentry is not None:
            try:
                r = self.sentry.roster(sid, on_page=on_page)
                if r is not None and (r.members or r.total):
                    if r.members:
                        self.cache.put_members(sid, [m["handle"] for m in r.members],
                                               complete=r.complete)
                    return r
            except Exception as exc:
                sentry_err = exc
        if self.confirms_absence:
            members, complete = self.members_detail(sid, max_pages, on_page)
            return Roster(members, complete, "sci")
        if sentry_err is not None:
            raise sentry_err
        return Roster([], False, "sentry")

    def _rows(self, path: str, mode: str | None = None) -> list | None:
        """A LIST endpoint's answer, with an empty list kept as an empty list.

        `_get()` folds "success:1 and no records" into the same None it uses
        for "no such record", which is right for the endpoints that return one
        thing and wrong for a paginated one: the pager has to tell the end of
        the list from a failure, and those are the two cases. Measured against
        the live service:

            page 1 of an org      success:1, data: [22 records]
            page 60 of that org   success:1, data: []
            success:0             not seen here, but means the request failed

        So: a list back means a page (possibly the last, empty one); None
        means the service did not answer properly and nothing may be
        concluded from it.
        """
        payload = self._call(path, mode)
        if payload is None or not payload.get("success"):
            return None
        data = payload.get("data")
        return data if isinstance(data, list) else None

    def members_detail(self, sid: str, max_pages: int = MEMBER_PAGE_CAP,
                       on_page=None) -> tuple[list[dict], bool]:
        """The roster as the API gives it, and whether the walk FINISHED.

        Returns (members, complete). `complete` is the important half: the
        caller writes "no longer on the roster" for everybody missing from
        this list, and that sentence must never be written from a list that
        stopped early.

        PAGES ARE NOT A FIXED SIZE, and that is what this used to get wrong.
        The old rule was "a page with fewer than 32 records is the last one",
        which is how pagination usually works and is not how this service
        behaves: measured on one org, pages came back with 22, 18, 21 and 20
        records. So every walk stopped after page one. Three flagged orgs had
        been storing a quarter of their rosters for weeks - 23 of 97, 30 of
        54, and 22 of 598 - with nothing anywhere saying so, because a short
        page looked exactly like the end of a small org.

        The end of the list is an EMPTY page, which the service answers with
        success:1 and no records, and which is distinguishable from a failure.
        So the walk runs until a page comes back empty, and `complete` is true
        only then. Hitting `max_pages` is not the end; neither is a page the
        service failed to answer.

        Hidden members are not in it - RSI omits them from the roster and
        counts them separately - so the total here is usually smaller than the
        member count on the org record, and that gap is not a truncation.

        `on_page(page, so_far)` is called after each page, for a caller that
        wants to say how a thirty-second pull is going.

        Forced to mode 'live'. This endpoint exists ONLY there - 'auto' and
        'cache' both answer 404 - so the client default returned an EMPTY
        roster for every org, silently.
        """
        out: list[dict] = []
        complete = False
        for page in range(1, max_pages + 1):
            rows = self._rows(
                f"organization_members/{urllib.parse.quote(sid)}?page={page}",
                mode="live")
            if rows is None:            # the service did not answer properly
                break
            if not rows:                # success, no records: that was the end
                complete = True
                break
            out += [{"handle": m.get("handle"), "display": m.get("display") or "",
                     "rank": m.get("rank") or "", "stars": m.get("stars") or 0}
                    for m in rows if m.get("handle")]
            if on_page is not None:
                try:
                    on_page(page, len(out))
                except Exception:
                    pass                # a progress line must never stop a pull
        if out:
            self.cache.put_members(sid, [m["handle"] for m in out], complete=complete)
        return out, complete


def main():
    import sys
    client = Client()
    if not client.ready:
        print("No API key found.\n"
              "  1. Get a free key at https://starcitizen-api.com\n"
              "  2. setx SC_API_KEY your_key    (or write it to sc_api_key.txt)")
        return 1
    # No default handle. This used to look up a real person whenever the
    # module was run bare, which is a live API call against a named player
    # nobody asked about - fine in a private repo, not fine in a public one.
    if len(sys.argv) < 2:
        print("usage: python rsi.py HANDLE [HANDLE ...]")
        return 2
    for h in sys.argv[1:]:
        t0 = time.time()
        c = client.citizen(h)
        print(f"\n{c.handle}  ({time.time() - t0:.1f}s)")
        print(f"  {c.summary()}")
        for o in c.orgs:
            print(f"    {'main ' if o.is_main else 'affil'} {o.sid:<12} "
                  f"{o.members:>6} members  {o.archetype:<14} "
                  f"{' / '.join(o.focuses) or '-'}")
        for f in c.flags:
            print(f"    !! {f}")
    print(f"\ncache: {client.cache.stats()}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
