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
Third-party watchlists: what OTHER people say about a handle.

A deliberately separate database from sc-watch.db, and the split is the whole
design. reputation.py's docstring already draws this line for rsi_cache.db:

    The cache is disposable and refetchable; this is not.

A watchlist is disposable and refetchable. It is somebody else's assertion,
from somewhere that can be wrong, can be a grudge, or can simply go away. Keeping it in its own file means you can delete the file and
your own judgements do not notice. Pulling third-party rows back out of a
shared table months later is the kind of cleanup that quietly misses some.

THREE THINGS THIS MODULE REFUSES TO DO
--------------------------------------
1. It never merges identities. reputation.canonical() is never called here.
   An entry saying "X is an alt of Y" is stored as TEXT in alt_of, never as a
   link between rows. The claim is displayed; it is not acted on. A list of
   alts is exactly the operation this tool exists to refuse on weak evidence,
   and somebody else's say-so is still say-so.

2. It never feeds Assessment.score(). Your score stays a function of what YOU
   recorded. A watchlist hit is a separate line in the tile, the same shape
   flagged_orgs already has - context you can read and judge, not a number
   that moved for reasons you cannot see.

3. It never turns a variant match into a hit on a person. A sketch match
   means "a name that could OCR to this is on a list", and it comes back with
   match='variant' so the caller has to say so. SLIVER and 5LIVER are two
   different real people; that rule does not relax because a third party is
   the one making the claim.

CONFIDENCE
----------
Sources hedge, and the hedge has to survive the import. One trusted
griefer-log message lists 24 handles under one heading and then says:

    -some of the ones below are still vague, or not directly affiliated-

Eleven of those 24 names sit below that line. A parser that flattened the
message into 24 entries all labelled with the heading would be making eleven
accusations the source itself declined to make. So every entry carries a
confidence, and anything hedged is stored UNCONFIRMED and renders differently.
"""

from __future__ import annotations

import re
import sqlite3
import time
from dataclasses import dataclass
from pathlib import Path

import dbopen
import paths
from reputation import sketch

DB_PATH = paths.data("watchlist.db")
# A list that ships WITH the tool, seeded into the database on first run so a
# new install is useful on day one instead of after somebody hands you a list.
# A resource, not data: read-only, inside the bundle when frozen.
STARTER_PATH = paths.resource("starter_list.json")

# Confidence levels, most to least. Stored as text because this is data a
# human reads, and an integer would need a legend nobody would write down.
CONFIRMED = "confirmed"      # you personally verified it
ASSERTED = "asserted"        # the source states it plainly
UNCONFIRMED = "unconfirmed"  # the source hedged, or it sat under a caveat
DISPUTED = "disputed"        # you looked and disagreed - kept, not deleted

SCHEMA = """
CREATE TABLE IF NOT EXISTS sources (
    id        INTEGER PRIMARY KEY,
    name      TEXT UNIQUE NOT NULL,
    url       TEXT DEFAULT '',
    note      TEXT DEFAULT '',
    added_ts  REAL,
    synced_ts REAL,
    enabled   INTEGER DEFAULT 1
);
CREATE TABLE IF NOT EXISTS entries (
    id          INTEGER PRIMARY KEY,
    source_id   INTEGER NOT NULL REFERENCES sources(id),
    handle      TEXT NOT NULL,
    sketch      TEXT NOT NULL,
    confidence  TEXT DEFAULT 'asserted',
    claim       TEXT DEFAULT '',
    alt_of      TEXT DEFAULT '',
    reported_by TEXT DEFAULT '',
    posted_ts   TEXT DEFAULT '',
    msg_id      TEXT DEFAULT '',
    msg_url     TEXT DEFAULT '',
    fetched_ts  REAL,
    UNIQUE(source_id, handle, msg_id)
);
CREATE INDEX IF NOT EXISTS idx_entries_handle ON entries(handle COLLATE NOCASE);
CREATE INDEX IF NOT EXISTS idx_entries_sketch ON entries(sketch);
CREATE TABLE IF NOT EXISTS orgs (
    id         INTEGER PRIMARY KEY,
    source_id  INTEGER NOT NULL REFERENCES sources(id),
    sid        TEXT NOT NULL,
    claim      TEXT DEFAULT '',
    msg_id     TEXT DEFAULT '',
    fetched_ts REAL,
    UNIQUE(source_id, sid, msg_id)
);
-- What the bundled list has already been seeded from, so that deleting it is
-- PERMANENT. Without this row, "forget this list" would last until the next
-- launch and then quietly undo itself, which is the worst way for a delete to
-- behave when what it deletes is accusations about named people.
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT
);
"""


@dataclass
class Hit:
    """One watchlist entry that matched a handle you just saw."""
    handle: str          # the handle AS THE SOURCE WROTE IT
    source: str
    confidence: str
    claim: str = ""
    alt_of: str = ""
    reported_by: str = ""
    posted_ts: str = ""
    msg_url: str = ""
    match: str = "exact"   # 'exact' or 'variant' - never conflate these

    def describe(self) -> str:
        """One line, for the tile, the console and the spoken alert alike.

        Written once and shared, because these three surfaces disagreeing
        about how sure a claim is would be worse than any of them being
        terse. Order is fixed: whose list, how sure they were, why, when.
        """
        if self.match == "variant":
            out = f"a name like this is on {self.source}"
        else:
            out = f"listed on {self.source}"
        if self.confidence == UNCONFIRMED:
            out += " (source was unsure)"
        elif self.confidence == DISPUTED:
            out += " (you disputed this)"
        if self.claim:
            # ASCII hyphen, not an em dash. This string reaches the Windows
            # console through watch.py, and lines() has always been ASCII for
            # that reason. The tile renders it just as well.
            out += f" - {self.claim}"
        if self.posted_ts:
            out += f" [{self.posted_ts}]"
        return out


class Watchlist:
    def __init__(self, path: Path | str | None = None):
        self.path = Path(path) if path else DB_PATH
        self.db = dbopen.connect(self.path)
        if dbopen.migrated_once(self.path):
            self.db.executescript(SCHEMA)
            self.db.commit()

    def close(self):
        self.db.close()

    # ---- sources -------------------------------------------------------

    def add_source(self, name: str, url: str = "", note: str = "") -> int:
        self.db.execute(
            "INSERT INTO sources (name, url, note, added_ts) VALUES (?,?,?,?)"
            " ON CONFLICT(name) DO UPDATE SET url=excluded.url, note=excluded.note",
            (name, url, note, time.time()))
        self.db.commit()
        return self.db.execute(
            "SELECT id FROM sources WHERE name=?", (name,)).fetchone()["id"]

    def sources(self) -> list[dict]:
        """Every source, with its OWN claim count and reason coverage.

        Per source, not overall. The panel showed the global figure against
        every row, so a two-entry list both of whose entries gave a reason
        was labelled "74 of 120" - which is exactly backwards from what the
        number is for, which is judging one list against another.

        `bundled` marks the list that CAME WITH the tool. Where a claim came
        from is the whole point of keeping these separate from your own
        records, and "it was already here when I installed it" is a different
        provenance from "somebody handed it to me" - so the panel can say so
        rather than leaving it to be assumed.
        """
        starter = starter_name()
        out = []
        for r in self.db.execute(
                "SELECT s.*,"
                " (SELECT COUNT(*) FROM entries e WHERE e.source_id=s.id) AS n,"
                " (SELECT COUNT(*) FROM entries e WHERE e.source_id=s.id"
                "    AND e.claim != '') AS n_reason"
                " FROM sources s ORDER BY name"):
            d = dict(r)
            d["bundled"] = bool(starter and d["name"] == starter)
            out.append(d)
        return out

    def forget_source(self, source_id: int) -> int:
        """Delete a source and everything it ever claimed.

        This method is the reason the separate database exists. One call, and
        a source you stopped trusting leaves no residue in anything you
        decided yourself.
        """
        n = self.db.execute(
            "SELECT COUNT(*) c FROM entries WHERE source_id=?",
            (source_id,)).fetchone()["c"]
        self.db.execute("DELETE FROM entries WHERE source_id=?", (source_id,))
        self.db.execute("DELETE FROM orgs WHERE source_id=?", (source_id,))
        self.db.execute("DELETE FROM sources WHERE id=?", (source_id,))
        self.db.commit()
        return n

    def set_enabled(self, source_id: int, on: bool):
        self.db.execute("UPDATE sources SET enabled=? WHERE id=?",
                        (1 if on else 0, source_id))
        self.db.commit()

    # ---- import --------------------------------------------------------

    def add_entry(self, source_id: int, handle: str, *, confidence: str = ASSERTED,
                  claim: str = "", alt_of: str = "", reported_by: str = "",
                  posted_ts: str = "", msg_id: str = "", msg_url: str = "") -> bool:
        """Record one claim. Returns False if this source already said it.

        Uniqueness is (source, handle, message) rather than (source, handle):
        two orgs naming the same person in two separate incidents is
        corroboration, and both rows are worth keeping.
        """
        h = handle.strip()
        if not h:
            return False
        cur = self.db.execute(
            "INSERT OR IGNORE INTO entries (source_id, handle, sketch, confidence,"
            " claim, alt_of, reported_by, posted_ts, msg_id, msg_url, fetched_ts)"
            " VALUES (?,?,?,?,?,?,?,?,?,?,?)",
            (source_id, h, sketch(h), confidence, claim, alt_of, reported_by,
             posted_ts, msg_id, msg_url, time.time()))
        self.db.commit()
        return cur.rowcount > 0

    def add_org(self, source_id: int, sid: str, claim: str = "", msg_id: str = ""):
        self.db.execute(
            "INSERT OR IGNORE INTO orgs (source_id, sid, claim, msg_id, fetched_ts)"
            " VALUES (?,?,?,?,?)",
            (source_id, sid.strip().upper(), claim, msg_id, time.time()))
        self.db.commit()

    def mark_synced(self, source_id: int):
        self.db.execute("UPDATE sources SET synced_ts=? WHERE id=?",
                        (time.time(), source_id))
        self.db.commit()

    # ---- lookup --------------------------------------------------------

    def lookup(self, handle: str, variants: bool | None = None) -> list[Hit]:
        """Every claim any enabled source makes about this handle.

        Exact matches first. Variant matches are included only because OCR
        misreads names and a list is useless if a one-character slip hides it,
        but they come back tagged match='variant' and the caller MUST render
        that distinction. A variant match is a prompt to look, not a finding.
        """
        h = handle.strip()
        if not h:
            return []
        if variants is None:
            variants = variants_enabled()
        out, seen = [], set()
        for r in self.db.execute(
                "SELECT e.*, s.name AS source FROM entries e JOIN sources s"
                " ON s.id = e.source_id WHERE s.enabled=1"
                " AND e.handle=? COLLATE NOCASE", (h,)):
            seen.add(r["id"])
            out.append(self._hit(r, "exact"))
        if variants:
            for r in self.db.execute(
                    "SELECT e.*, s.name AS source FROM entries e JOIN sources s"
                    " ON s.id = e.source_id WHERE s.enabled=1 AND e.sketch=?",
                    (sketch(h),)):
                if r["id"] not in seen:
                    out.append(self._hit(r, "variant"))
        return out

    @staticmethod
    def _hit(r, match: str) -> Hit:
        return Hit(handle=r["handle"], source=r["source"],
                   confidence=r["confidence"] or ASSERTED,
                   claim=r["claim"] or "", alt_of=r["alt_of"] or "",
                   reported_by=r["reported_by"] or "",
                   posted_ts=r["posted_ts"] or "", msg_url=r["msg_url"] or "",
                   match=match)

    def orgs_flagged(self) -> list[dict]:
        return [dict(r) for r in self.db.execute(
            "SELECT o.*, s.name AS source FROM orgs o JOIN sources s"
            " ON s.id = o.source_id WHERE s.enabled=1 ORDER BY o.sid")]

    def reason_coverage(self) -> tuple[int, int]:
        """How many entries actually say why. Honest reporting, not a boast.

        A list where most rows are a bare name is a weaker instrument than one
        where most rows carry a reason, and the UI should be able to say so.
        """
        r = self.db.execute(
            "SELECT COUNT(*) n, SUM(CASE WHEN claim != '' THEN 1 ELSE 0 END) w"
            " FROM entries").fetchone()
        return (r["w"] or 0), (r["n"] or 0)

    def stats(self) -> dict:
        c = self.db.execute(
            "SELECT COUNT(*) n, COUNT(DISTINCT handle) d FROM entries").fetchone()
        return {"entries": c["n"], "handles": c["d"],
                "sources": self.db.execute(
                    "SELECT COUNT(*) n FROM sources").fetchone()["n"],
                "orgs": self.db.execute(
                    "SELECT COUNT(*) n FROM orgs").fetchone()["n"],
                "path": str(self.path)}


# ---- shared instance ---------------------------------------------------

_shared: Watchlist | None = None
_shared_tried = False


def enabled() -> bool:
    """Is the whole feature switched on?

    Read fresh rather than cached, so the toggle in the UI takes effect on the
    next tile poll instead of at the next restart. settings.load() is a small
    JSON file and the UI already re-reads it per request for rsi_mode.

    A missing or broken settings file means ON, matching every other default
    in the schema - the failure mode of a corrupt file should not be to
    silently stop flagging people.
    """
    try:
        import settings
        return bool(settings.load().get("watchlist_enabled", True))
    except Exception:
        return True


def variants_enabled() -> bool:
    try:
        import settings
        return bool(settings.load().get("watchlist_variants", True))
    except Exception:
        return True


_starter_name: str | None = None


def starter_name() -> str:
    """What the bundled list is called, or "" if this build has none.

    Read once. The file is on disk beside the program and does not change
    while it runs, and this is asked on every render of the panel.
    """
    global _starter_name
    if _starter_name is None:
        doc = read_starter()
        _starter_name = str((doc or {}).get("name") or "")
    return _starter_name


def read_starter(path: Path | str | None = None) -> dict | None:
    """The bundled list as plain data, or None if this build has none.

    JSON rather than a prepared database: it is reviewable in a diff, it
    carries no schema to go stale against this one, and anybody can read
    exactly which claims they are being handed before running anything. Each
    entry keeps its own confidence, which a pasted list cannot express - 14 of
    the 118 shipped claims are ones their source hedged, and flattening those
    into plain assertions would be making accusations nobody made.
    """
    import json
    p = Path(path) if path else STARTER_PATH
    try:
        doc = json.loads(p.read_bytes().decode("utf-8-sig"))
    except FileNotFoundError:
        return None
    except (OSError, ValueError) as exc:
        print(f"watchlist: {p.name} could not be read ({exc}); "
              f"no starter list.")
        return None
    if not isinstance(doc, dict) or not isinstance(doc.get("entries"), list):
        print(f"watchlist: {p.name} is not a starter list; ignoring it.")
        return None
    return doc


def seed_starter(path: Path | str | None = None,
                 db_path: Path | str | None = None) -> dict:
    """Import the bundled list once. Safe to call on every launch.

    Returns {"seeded": n, "why": ...}. It does nothing at all when:

      - there is no bundled list (a build that ships without one);
      - this version has already been seeded, whether or not the list is still
        there - see the meta table. Deleting the list is meant to stick;
      - the feature is switched off.

    A NEW version of the bundled list is imported into the same source, so a
    release can add claims to it, and `INSERT OR IGNORE` on (source, handle,
    message) means re-running costs nothing. It never re-enables a source the
    user silenced, and it never touches any other source.
    """
    doc = read_starter(path)
    if doc is None:
        return {"seeded": 0, "why": "no bundled list"}
    version = str(doc.get("version", "1"))
    wl = Watchlist(db_path)
    try:
        done = wl.db.execute(
            "SELECT value FROM meta WHERE key='starter_version'").fetchone()
        if done and str(done["value"]) == version:
            return {"seeded": 0, "why": "already seeded"}
        name = str(doc.get("name") or "bundled list")[:80]
        sid = wl.add_source(name, "", str(doc.get("note") or "")[:500])
        # Matched on CONTENT, not on the message id that normally keys a row.
        # The same list may already be in this database, imported by hand,
        # and those rows carry different message ids. Keyed on the id,
        # seeding would have written every claim a second time and shown
        # every handle on the list twice.
        # Keyed on the DATE as well. One handle on the shipped list is named
        # twice with no reason given, on two different days: two separate
        # mentions, which this schema keeps on purpose as corroboration.
        # Without the date they collapse into one.
        already = {(r["handle"].strip().upper(), r["claim"] or "",
                    r["posted_ts"] or "")
                   for r in wl.db.execute(
                       "SELECT handle, claim, posted_ts FROM entries"
                       " WHERE source_id=?", (sid,))}
        n = 0
        for i, e in enumerate(doc["entries"]):
            if not isinstance(e, dict):
                continue
            h = str(e.get("handle") or "").strip()
            claim = str(e.get("claim") or "")[:300]
            posted = str(e.get("posted") or "")[:32]
            if not h or (h.upper(), claim, posted) in already:
                continue
            conf = str(e.get("confidence") or ASSERTED)
            n += wl.add_entry(sid, h,
                              confidence=conf if conf in
                              (CONFIRMED, ASSERTED, UNCONFIRMED, DISPUTED)
                              else ASSERTED,
                              claim=claim,
                              posted_ts=posted,
                              # Stable per row, so a re-import matches the
                              # entry it already wrote instead of doubling it.
                              msg_id=f"s{i}")
            already.add((h.upper(), claim, posted))
        for o in doc.get("orgs") or []:
            if isinstance(o, dict) and o.get("sid"):
                wl.add_org(sid, str(o["sid"]), str(o.get("claim") or "")[:300])
        wl.mark_synced(sid)
        wl.db.execute("INSERT OR REPLACE INTO meta (key, value) VALUES"
                      " ('starter_version', ?)", (version,))
        wl.db.commit()
        return {"seeded": n, "why": "imported", "source": name,
                "version": version}
    finally:
        wl.close()


def shared() -> Watchlist | None:
    """The process-wide watchlist, or None if there isn't one.

    Returns None rather than an empty database when the file does not exist,
    so somebody who never imports a watchlist never gets one created, never
    sees the panel, and pays nothing for a feature they did not ask for.
    Returns None too when the feature is switched off, which is why the check
    is here: every caller goes through this, so one test covers all of them.

    The CONNECTION is cached; the setting is not. Caching the setting would
    mean the toggle needed a restart to take effect in the UI.

    Opening is tried once. If it fails - corrupt file, locked by another
    process, a half-written import - it stays None for the life of the
    process and everything else carries on. A third-party list is the least
    important thing here and must never be the reason the tool will not run.
    """
    global _shared, _shared_tried
    if not enabled():
        return None
    if _shared_tried:
        return _shared
    _shared_tried = True
    try:
        # First run with a bundled list: this is what creates the database.
        # Seeding is what makes a fresh install useful before anyone has been
        # handed a list, and it is the only thing that ever creates one
        # unasked - so it happens here, once, where every caller goes.
        if not DB_PATH.exists() and STARTER_PATH.exists():
            seed_starter()
        elif DB_PATH.exists():
            seed_starter()          # a newer bundled version, if there is one
        if DB_PATH.exists():
            _shared = Watchlist()
    except Exception:
        _shared = None
    return _shared


# ---- importing --------------------------------------------------------

# A handle as RSI allows it. Same shape the UI enforces on a typed handle.
_HANDLE = re.compile(r"^[A-Za-z0-9_\-]{2,40}$")
_CITIZEN_URL = re.compile(
    r"robertsspaceindustries\.com/(?:[a-z-]+/)?citizens/([A-Za-z0-9_\-]{2,40})",
    re.I)
_ORG_URL = re.compile(
    r"robertsspaceindustries\.com/(?:[a-z-]+/)?orgs/([A-Za-z0-9_\-]{2,32})", re.I)
# What separates a handle from the reason written beside it.
#
# A hyphen only counts when it has SPACE ON BOTH SIDES. A bare hyphen is a
# legal handle character, and treating it as a separator turned
# "EXMPL-SomeName - camped a station pad" into the handle "EXMPL" - a name
# that belongs to somebody else entirely. Inventing a handle is the one
# outcome this parser must never produce, so the greedy reading loses.
# | : and , cannot occur in a handle, so those split without needing spaces.
_SPLIT = re.compile(r"(?:\s+[-–—]\s+|\s*[|:,]\s*|\s{2,})")
# Whole URLs are removed before the reason is read; stripping only the matched
# citizens/<handle> part left "https //" sitting at the front of every claim.
_ANY_URL = re.compile(r"https?://\S+", re.I)


def parse_lines(text: str) -> dict:
    """Turn a pasted block into entries, and say what it could not read.

    Written because the import path did not exist: a shipped copy could
    silence and forget watchlists it had no way to create.

    Deliberately forgiving about SHAPE and strict about the HANDLE. People
    paste what they were given - a bare list, RSI links, "name - reason",
    numbered lines - and any of those should work. But a token that is not a
    legal handle is never guessed at: it is returned in `skipped` for the
    person to look at, because a mangled handle in a griefer list is an
    accusation against whoever really owns that name.
    """
    entries, skipped, orgs = [], [], []
    for raw in (text or "").splitlines():
        line = raw.strip()
        if not line or line.startswith(("#", "//")):
            continue
        o = _ORG_URL.search(line)
        if o and not _CITIZEN_URL.search(line):
            orgs.append(o.group(1).upper())
            continue
        m = _CITIZEN_URL.search(line)
        if m:
            handle = m.group(1)
            claim = _SPLIT.sub(" ", _ANY_URL.sub("", line)).strip(" -|:,\t")
        else:
            # Drop a leading list marker: "1.", "-", "*", "1)".
            body = re.sub(r"^\s*(?:\d+[.)]|[-*•])\s+", "", line)
            parts = _SPLIT.split(body, maxsplit=1)
            handle = parts[0].strip()
            claim = (parts[1] if len(parts) > 1 else "").strip()
            if not _HANDLE.match(handle):
                skipped.append(line[:120])
                continue
        entries.append({"handle": handle, "claim": claim[:300]})
    # One row per handle; a repeated handle keeps the first reason given.
    seen, out = set(), []
    for e in entries:
        k = e["handle"].lower()
        if k in seen:
            continue
        seen.add(k)
        out.append(e)
    return {"entries": out, "skipped": skipped,
            "orgs": sorted(set(orgs))}


def safe_url(raw: str) -> str:
    """A URL fit to put in an href, or "".

    The source link is rendered as a clickable link in the UI, and the UI is
    the origin that can drive every write endpoint this tool has. A
    `javascript:` link there would run in that origin - so only http(s)
    survives, and anything else is dropped rather than shown.
    """
    u = (raw or "").strip()
    return u[:300] if u[:8].lower().startswith(("http://", "https://")) else ""


def import_text(name: str, text: str, url: str = "", note: str = "",
                confidence: str = ASSERTED, reported_by: str = "",
                posted_ts: str = "", path: Path | str | None = None) -> dict:
    """Parse a pasted block and store it as one source. Returns a summary."""
    parsed = parse_lines(text)
    if not parsed["entries"] and not parsed["orgs"]:
        return {"ok": False, "error": "nothing in there looked like a handle",
                "skipped": parsed["skipped"][:12]}
    wl = Watchlist(path)
    try:
        url = safe_url(url)
        sid = wl.add_source(name.strip()[:80] or "pasted list", url,
                            note.strip()[:500])
        added = 0
        for e in parsed["entries"]:
            added += wl.add_entry(sid, e["handle"], confidence=confidence,
                                  claim=e["claim"], reported_by=reported_by[:80],
                                  posted_ts=posted_ts[:32], msg_id="paste",
                                  msg_url=url)
        for o in parsed["orgs"]:
            wl.add_org(sid, o, "named in the imported list")
        wl.mark_synced(sid)
        return {"ok": True, "source_id": sid, "added": added,
                "seen": len(parsed["entries"]), "orgs": parsed["orgs"],
                "skipped": parsed["skipped"][:12],
                "skipped_n": len(parsed["skipped"])}
    finally:
        wl.close()
