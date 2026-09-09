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
Player and organisation lookup via starcitizen-api.com.

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
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import asdict, dataclass, field
from pathlib import Path
import paths

API_HOST = "https://api.starcitizen-api.com"
USER_AGENT = "sc-watch/0.2 (personal Star Citizen contact tool)"
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
        self.db = sqlite3.connect(str(path), check_same_thread=False)
        self.db.execute("""CREATE TABLE IF NOT EXISTS citizens (
            handle TEXT PRIMARY KEY, data TEXT, fetched_at REAL)""")
        self.db.execute("""CREATE TABLE IF NOT EXISTS orgs (
            sid TEXT PRIMARY KEY, data TEXT, fetched_at REAL)""")
        self.db.execute("""CREATE TABLE IF NOT EXISTS org_members (
            sid TEXT NOT NULL, handle TEXT NOT NULL, fetched_at REAL,
            PRIMARY KEY (sid, handle))""")
        self.db.commit()

    def get_citizen(self, handle: str) -> Citizen | None:
        row = self.db.execute(
            "SELECT data, fetched_at FROM citizens WHERE handle=?",
            (handle.upper(),)).fetchone()
        if not row:
            return None
        payload = json.loads(row[0])
        ttl = CITIZEN_TTL if payload.get("exists", True) else MISS_TTL
        if time.time() - row[1] > ttl:
            return None
        orgs = [Org(**o) for o in payload.pop("orgs", [])]
        return Citizen(orgs=orgs, **payload)

    def put_citizen(self, c: Citizen):
        self.db.execute("INSERT OR REPLACE INTO citizens VALUES (?,?,?)",
                        (c.handle.upper(), json.dumps(asdict(c)), time.time()))
        self.db.commit()

    def get_org(self, sid: str) -> Org | None:
        row = self.db.execute("SELECT data, fetched_at FROM orgs WHERE sid=?",
                              (sid.upper(),)).fetchone()
        if not row or time.time() - row[1] > ORG_TTL:
            return None
        return Org(**json.loads(row[0]))

    def put_org(self, o: Org):
        self.db.execute("INSERT OR REPLACE INTO orgs VALUES (?,?,?)",
                        (o.sid.upper(), json.dumps(asdict(o)), time.time()))
        self.db.commit()

    def put_members(self, sid: str, handles: list[str]):
        now = time.time()
        self.db.executemany(
            "INSERT OR REPLACE INTO org_members VALUES (?,?,?)",
            [(sid.upper(), h.upper(), now) for h in handles])
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


def lookup_url(handle: str) -> str:
    """Third-party lookup, useful when we have no org data of our own."""
    return LOOKUP_URL.format(handle=urllib.parse.quote(handle))


def profile_url(handle: str) -> str:
    """The player's own RSI page - for the second-screen 'open profile' button."""
    return PROFILE_URL.format(handle=urllib.parse.quote(handle))


def org_url(sid: str) -> str:
    return ORG_URL.format(sid=urllib.parse.quote(sid))


class Client:
    """Profile lookup with a swappable backend.

    Two backends have already proven fragile - direct RSI scraping is against
    their terms, and starcitizen-api.com is unmaintained (its host still
    answers, but nobody is fixing it if it stops). So the source is a
    parameter, and everything downstream - the cache, the dataclasses, the
    piracy flagging - is shared between them.

    provider:
      'api'   starcitizen-api.com. Needs a key. Full data.
      'link'  no network at all. Records the handle and hands back a URL for
              looking it up by hand. Loses the piracy flag; loses nothing else.
      'none'  disabled entirely.
    """

    def __init__(self, cache: Cache | None = None, api_key: str | None = None,
                 mode: str = DEFAULT_MODE, min_interval: float = MIN_INTERVAL,
                 provider: str = "api"):
        self.cache = cache or Cache()
        self.api_key = api_key or find_api_key()
        self.mode = mode
        self.min_interval = min_interval
        self.provider = provider
        self._last = 0.0

    @property
    def ready(self) -> bool:
        if self.provider == "none":
            return False
        if self.provider == "link":
            return True
        return bool(self.api_key)

    @property
    def enriches(self) -> bool:
        """Whether this backend returns org data, as opposed to just a link."""
        return self.provider == "api" and bool(self.api_key)

    def _get(self, path: str, mode: str | None = None):
        """Rate-limited GET returning parsed JSON, or None on 404//failure.

        `mode` overrides the client default for one call, because not every
        endpoint exists in every mode - see members().
        """
        if not self.api_key:
            raise RuntimeError(
                "no API key. Get one at https://starcitizen-api.com, then set "
                "SC_API_KEY or write it to sc_api_key.txt")
        wait = self.min_interval - (time.time() - self._last)
        if wait > 0:
            time.sleep(wait)
        self._last = time.time()
        url = f"{API_HOST}/{self.api_key}/v1/{mode or self.mode}/{path}"
        req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT,
                                                   "Accept": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=25) as r:
                payload = json.loads(r.read().decode("utf-8", "replace"))
        except urllib.error.HTTPError as e:
            if e.code == 404:
                return None
            raise
        # The API answers 200 with success:0 for a missing record, so the
        # HTTP status alone is not enough to tell "absent" from "found".
        if not payload.get("success") or payload.get("data") in (None, [], {}):
            return None
        return payload["data"]

    def org(self, sid: str) -> Org:
        cached = self.cache.get_org(sid)
        if cached:
            return cached
        data = self._get(f"organization/{urllib.parse.quote(sid)}")
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

    def citizen(self, handle: str) -> Citizen:
        handle = handle.strip()
        if SHIP_SERIAL_RE.match(handle.upper()):
            return Citizen(handle=handle.upper(), exists=False)

        if not self.enriches:
            # No backend: report the handle as seen but unverified rather than
            # claiming it doesn't exist, which would be a different and wrong
            # statement. Local reputation still applies; only org data is lost.
            return Citizen(handle=handle.upper(), exists=True,
                           unverified=True, fetched_at=time.time())

        cached = self.cache.get_citizen(handle)
        if cached:
            return cached

        data = self._get(f"user/{urllib.parse.quote(handle)}")
        if not data:
            c = Citizen(handle=handle.upper(), exists=False, fetched_at=time.time())
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
            full = self.org(sid)
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
                c.orgs.append(self.org(extra_sid))

        self.cache.put_citizen(c)
        return c

    def members(self, sid: str, max_pages: int = 20) -> list[str]:
        """Pull an org's member list and cache it.

        This is the affiliate workaround: the user endpoint only reports a main
        org, so a pirate side-org would be invisible. Pulling the roster of an
        org you have flagged once lets membership be checked locally from then
        on. Paginated 32 at a time, so keep it for small orgs.

        Forced to mode 'live'. This endpoint exists ONLY there - 'auto' and
        'cache' both answer 404 - so the client default made this return an
        EMPTY roster for every org, silently: the org resolved, the flag was
        written, and not one member was recorded. Nothing looked wrong, because
        an empty list is a valid answer for a small org.
        """
        handles: list[str] = []
        for page in range(1, max_pages + 1):
            data = self._get(
                f"organization_members/{urllib.parse.quote(sid)}?page={page}",
                mode="live")
            if not data:
                break
            handles += [m.get("handle") for m in data if m.get("handle")]
            if len(data) < 32:
                break
        if handles:
            self.cache.put_members(sid, handles)
        return handles


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
