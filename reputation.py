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
Local reputation database: who you've seen, and what you know about them.

Deliberately stores EVIDENCE, not verdicts. A boolean "bad" flag is both less
useful and much easier to abuse - it can't tell you why it fired, can't age
out, and can't be argued with. Every report here carries a category, a note, a
timestamp and optionally a pointer to proof, so an alert can say what happened
and when rather than just turning red.

Three things feed an assessment:

  reports     what you recorded yourself, decayed by age
  org flags   flag an org once and every member inherits it, which is how a
              small personal list generalises past the handful of people you
              have personally met
  sightings   encounter history - "seen 6 times, first 3 weeks ago" is context
              even with no reports at all

Kept in its own database, separate from rsi_cache.db. The cache is disposable
and refetchable; this is not.
"""

from __future__ import annotations

import sqlite3
import json
import time
from dataclasses import dataclass, field
from pathlib import Path

import consensus
import dbopen
import flags as flags_mod
import paths

DB_PATH = paths.data("sc-watch.db")

# Reports lose half their weight over this period. Behaviour from two years ago
# is not evidence about someone today, but it isn't nothing either.
HALF_LIFE_DAYS = 180.0

CATEGORIES = (
    "unprovoked_attack",
    "camping",
    "ransom_broken",
    "harassment",
    "cheating_suspected",
    "friendly",        # positive reports matter too
    "note",            # neutral observation
    "pending",         # flagged in the moment, reason not given yet
)

# 'pending' exists because the moment worth recording is the moment you are
# least able to type. Something happens, you press one button, and the reason
# waits until you are docked. It scores like any other negative report on
# purpose: you flagged it because something happened, so it should alert even
# before it has a name. resolve_pending() replaces it once you say what it was.
PENDING = "pending"

# Characters OCR confuses, collapsed to one representative each. Two readings
# of the same handle land on the same sketch - '5LIVER' and 'SLIVER' both
# become '51IVER' - so a confusion variant is an indexed lookup rather than a
# scan over every known player.
_SKETCH_GROUPS = ("OQD0", "IL1_", "S5", "B8", "G6", "Z2", "UV", "CG", "TY7")
_SKETCH_MAP = {c: grp[0] for grp in _SKETCH_GROUPS for c in grp}


def sketch(handle: str) -> str:
    return "".join(_SKETCH_MAP.get(c, c) for c in handle.upper())


SCHEMA = """
CREATE TABLE IF NOT EXISTS players (
    handle        TEXT PRIMARY KEY,
    moniker       TEXT,
    enlisted      TEXT,
    first_seen    REAL,
    last_seen     REAL,
    sightings     INTEGER DEFAULT 0,
    sketch        TEXT
);
CREATE TABLE IF NOT EXISTS sightings (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    handle    TEXT NOT NULL,
    ts        REAL NOT NULL,
    range_km  REAL,
    votes     INTEGER,
    total     INTEGER
);
CREATE INDEX IF NOT EXISTS idx_sightings_handle ON sightings(handle);
CREATE TABLE IF NOT EXISTS reports (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    handle    TEXT NOT NULL,
    ts        REAL NOT NULL,
    category  TEXT NOT NULL,
    note      TEXT,
    evidence  TEXT,
    source    TEXT DEFAULT 'self'
);
CREATE INDEX IF NOT EXISTS idx_reports_handle ON reports(handle);
-- Reads the tool is not sure about, kept so they can be checked by eye.
--
-- The trigger is VOTES, never `confidence`: a 1/1 read reports confidence 1.00
-- and is the weakest thing the pipeline produces - 57 rows in one session's log
-- were single-frame reads at "100%". A read is audited when it got fewer than
-- two votes, or when the burst disagreed with itself.
CREATE TABLE IF NOT EXISTS audits (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    ts         REAL NOT NULL,
    handle     TEXT NOT NULL,      -- what was recorded, right or wrong
    range_km   REAL,
    votes      INTEGER,
    total      INTEGER,
    variants   TEXT,               -- JSON [[name, count], ...] seen this burst
    frame_path TEXT,               -- full frame, JPEG; may be missing
    crop_name  TEXT,               -- lossless crop of the name box
    crop_range TEXT,               -- lossless crop of the range box
    -- WHY this audit exists, which decides what can clear it. 'weak_read' is
    -- answered by corroboration; 'no_rsi_account' is not, because the case it
    -- catches is a read that was corroborated AND wrong - VONCARSTEINO came
    -- back 18/18 and the real handle was VONCARSTEIN.
    reason     TEXT DEFAULT 'weak_read'
);
CREATE INDEX IF NOT EXISTS idx_audits_handle ON audits(handle);

-- Labels that look like contacts but are not people: a floating salvage crate,
-- a mission marker, anything the HUD draws in the same shape as a player.
-- Kept as DATA rather than a setting because it is a record of what you have
-- seen and decided about, and because it wants a reason and a date the same way
-- a report does.
CREATE TABLE IF NOT EXISTS ignored (
    label     TEXT PRIMARY KEY,       -- upper-case, as read
    sketch    TEXT,                   -- confusion-collapsed, to catch OCR variants
    note      TEXT,
    ts        REAL NOT NULL,
    hits      INTEGER DEFAULT 0       -- how often it has been suppressed since
);

CREATE TABLE IF NOT EXISTS org_flags (
    sid       TEXT PRIMARY KEY,
    name      TEXT,
    ts        REAL NOT NULL,
    note      TEXT,
    -- When the member list was last pulled, and how many names it held. A
    -- flag with no roster only reaches players whose PROFILE names this org,
    -- and the profile endpoint reports a main org only - so for the affiliate
    -- case, which is the likely one, a flag without a roster reaches nobody.
    -- Storing the fetch time is what lets the UI say that out loud instead of
    -- showing a flag that silently does nothing.
    roster_ts REAL,
    roster_n  INTEGER DEFAULT 0
);
CREATE TABLE IF NOT EXISTS player_orgs (
    handle    TEXT NOT NULL,
    sid       TEXT NOT NULL,
    is_main   INTEGER DEFAULT 0,
    -- 'profile' = the API said this is their org. 'roster' = they appeared in
    -- a flagged org's member list. The distinction is load-bearing: the user
    -- endpoint reports only a MAIN org, so an affiliate membership can only
    -- ever come from a roster, and set_orgs() must not delete it when it
    -- refreshes what the profile says.
    source    TEXT DEFAULT 'profile',
    PRIMARY KEY (handle, sid)
);
-- Everyone a flagged org's public roster has EVER listed, and when.
--
-- player_orgs holds CURRENT membership only, and that is what carries an org
-- flag to a person - someone who left is not handed the flag as though they
-- were still in. This table is the history beside it: when each person was
-- first and last listed, and since when they have NOT been.
--
-- `gone_since` is worded carefully in everything that shows it. The API
-- leaves hidden members off the roster entirely, so vanishing from it means
-- "left, OR hid their membership" - never simply "left".
CREATE TABLE IF NOT EXISTS org_membership (
    sid        TEXT NOT NULL,
    handle     TEXT NOT NULL,
    display    TEXT,
    rank       TEXT,
    stars      INTEGER,
    first_seen REAL,             -- first roster pull that listed them
    last_seen  REAL,             -- most recent one that did
    gone_since REAL,             -- first pull that did NOT; NULL while listed
    PRIMARY KEY (sid, handle)
);
CREATE INDEX IF NOT EXISTS idx_org_membership_handle ON org_membership(handle);
-- What YOU filled in where a lookup could not: an org's name and declared
-- focus, and a player's enlistment date, typed from their RSI page on a day
-- the lookup services could not supply them. Never passed off as RSI's -
-- everything that shows one says it is yours - and never used where RSI has
-- an answer: the moment a lookup brings the real value, that is what shows.
-- Kept here rather than in rsi_cache.db, which is a cache and may be thrown
-- away; this is the user's own record.
CREATE TABLE IF NOT EXISTS org_manual (
    sid             TEXT PRIMARY KEY,
    name            TEXT,
    primary_focus   TEXT,
    secondary_focus TEXT,
    ts              REAL
);
-- Small facts about this install. `install` is a random id made on the first
-- export, so an org mate's copy of your reports can be matched up again when
-- you send a newer export - and so your own export is recognised as yours.
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT
);
-- Everyone whose export you imported. Their reports carry `source` (the name
-- they gave) and `origin` ("<install>:<their report id>"); their hand-added
-- roster members carry `manual_src` (the install). See share.py.
CREATE TABLE IF NOT EXISTS shares (
    install     TEXT PRIMARY KEY,
    name        TEXT,
    exported_at REAL,
    imported_at REAL
);
CREATE TABLE IF NOT EXISTS player_manual (
    handle   TEXT PRIMARY KEY,
    enlisted TEXT,               -- ISO date, YYYY-MM-DD
    ts       REAL
);
"""


def former_line(sid: str, since: float | None) -> str:
    """How former membership is said, everywhere it is said.

    "Off the public roster", never "left": the roster omits hidden members,
    so dropping off it is equally what hiding your membership looks like.
    """
    when = time.strftime("%d %b", time.localtime(since)) if since else "a while"
    return (f"formerly on flagged org {sid}'s roster - off it since {when} "
            f"(left, or hid it)")


def manual_line(sid: str, manual_orgs: dict, manual_by: dict | None = None) -> str:
    """The tail on a flagged-org line when the membership was added by hand.

    Said every time, on the tile and the console alike: a membership no roster
    shows is somebody's intel, and has to read as whose - yours, or the org
    mate whose export brought it.
    """
    if sid not in manual_orgs:
        return ""
    note = manual_orgs[sid]
    who = (manual_by or {}).get(sid) or "you"
    return f" (added by {who}{': ' + note if note else ''})"


@dataclass
class Report:
    ts: float
    category: str
    id: int = 0          # rowid, so a single report can be edited or removed
    note: str = ""
    evidence: str = ""
    source: str = "self"

    @property
    def age_days(self) -> float:
        return (time.time() - self.ts) / 86400.0

    @property
    def weight(self) -> float:
        """Half-life decay. A report never vanishes, it just stops shouting."""
        return 0.5 ** (self.age_days / HALF_LIFE_DAYS)


@dataclass
class Assessment:
    handle: str
    known: bool = False
    sightings: int = 0
    first_seen: float | None = None
    last_seen: float | None = None
    reports: list[Report] = field(default_factory=list)
    flagged_orgs: list[tuple[str, str]] = field(default_factory=list)
    # (sid, gone_since) for flagged orgs whose roster USED to list this person
    # and no longer does. Alerts just as current membership does - see alert.
    former_orgs: list[tuple[str, float]] = field(default_factory=list)
    # sid -> your note, for flagged orgs whose membership of this person YOU
    # recorded rather than a roster. Said wherever the membership is said.
    manual_orgs: dict[str, str] = field(default_factory=dict)
    # sid -> the name of the org mate whose export added them; absent = you.
    manual_by: dict[str, str] = field(default_factory=dict)
    # sid -> the flagged org's display name, where one is stored. Speech says
    # "Member of Example Org"; an SID read aloud is noise.
    org_names: dict[str, str] = field(default_factory=dict)
    # Third-party watchlist hits - see watchlist.py. Populated only when a
    # watchlist was handed to Database(); duck-typed so reputation.py never
    # imports watchlist.py, which imports sketch() from here.
    watchlist: list = field(default_factory=list)

    @property
    def score(self) -> float:
        """Decayed sum of negative reports. Positive reports subtract.

        Watchlists are deliberately absent. This number is what YOU recorded,
        and it stays answerable to you alone - a stranger's post can put a
        line on the tile, but it cannot move your score.
        """
        total = 0.0
        for r in self.reports:
            # Polarity comes from the flag definition, not from two hard-coded
            # strings. See flags.py: a custom "Good trader" used to count
            # AGAINST the player, and clearing the friendly box would have
            # flipped every positive report ever filed.
            total += r.weight * flags_mod.sign(r.category)
        return total

    @property
    def alert(self) -> bool:
        # An EXACT watchlist match raises the flag even when you have never
        # met this person - that is the entire value of a shared list. A
        # VARIANT match does not: it only means a name that could OCR to this
        # one is listed, and firing a griefer alert on that would eventually
        # accuse the wrong player. It still shows up in lines() to look at.
        #
        # FORMER membership of a flagged org alerts too. The user's call, and
        # the reasoning is theirs: people leave these orgs precisely to act
        # with fewer eyes on them, so dropping off the roster is not evidence
        # of reform. The wording still says what the roster can actually
        # show - "off it since ... (left, or hid it)" - so the alert is loud
        # without claiming more than is known.
        return (self.score > 0 or bool(self.flagged_orgs)
                or bool(self.former_orgs)
                or any(h.match == "exact" for h in self.watchlist))

    def spoken(self) -> dict | None:
        """The ONE reason a spoken alert gives - the strongest that applies."""
        every = self.spoken_all()
        return every[0] if every else None

    def spoken_all(self) -> list[dict]:
        """Every reason a spoken alert could give, as data, strongest first -
        audio.py says the first one whose phrase is switched on, through
        phrases you can edit. Empty if nothing applies.

        Order: your own negative reports, reports someone shared with you, a
        flagged org, a former one, an exact watch-list match. The console and
        the tile still get lines(); speech is rationed to a single reason.
        """
        out = []
        for mine in (True, False):
            by_cat: dict[str, list[Report]] = {}
            for r in self.reports:
                if (r.source in ("self", "", None)) != mine:
                    continue
                # Pending counts: a flag pressed mid-fight is the freshest
                # warning there is, and it is spoken as "Flagged".
                if r.category != PENDING and flags_mod.sign(r.category) <= 0:
                    continue
                by_cat.setdefault(r.category, []).append(r)
            if by_cat:
                cat, rs = max(by_cat.items(),
                              key=lambda kv: (len(kv[1]), max(r.ts for r in kv[1])))
                r = {"kind": "report" if mine else "shared",
                     "label": ("Flagged" if cat == PENDING
                               else flags_mod.speakable(cat)),
                     "count": sum(len(v) for v in by_cat.values())}
                if not mine:
                    r["source"] = rs[0].source
                out.append(r)
        for sid, _ in self.flagged_orgs[:1]:
            out.append({"kind": "org", "org": self.org_names.get(sid) or sid})
        for sid, _ in self.former_orgs[:1]:
            out.append({"kind": "former", "org": self.org_names.get(sid) or sid})
        if any(h.match == "exact" for h in self.watchlist):
            out.append({"kind": "watchlist"})
        return out

    def lines(self) -> list[str]:
        """Why this fired, in plain language. Never just a verdict."""
        out = []
        by_cat: dict[str, list[Report]] = {}
        for r in self.reports:
            by_cat.setdefault(r.category, []).append(r)
        for cat, rs in sorted(by_cat.items(), key=lambda kv: -len(kv[1])):
            newest = min(r.age_days for r in rs)
            when = ("today" if newest < 1 else
                    f"{int(newest)}d ago" if newest < 365 else
                    f"{newest / 365:.1f}y ago")
            # The LABEL, not the key. 'ransom_broken' is a database value;
            # "Broke a ransom" is what a person wrote and what a person should
            # read. Retired flags still resolve, so an old report keeps its
            # proper name forever.
            #
            # When speech is wired to flags this should become
            # flags_mod.speakable(cat), which falls back to the label but lets
            # a flag carry a pronunciation - a label like "KOS!!!" reads fine
            # and speaks badly.
            out.append(f"{len(rs)}x {flags_mod.by_key(cat).label}, "
                       f"most recent {when}")
        for sid, note in self.flagged_orgs:
            out.append(f"member of flagged org {sid}" + (f" - {note}" if note else "")
                       + manual_line(sid, self.manual_orgs, self.manual_by))
        for sid, since in self.former_orgs:
            out.append(former_line(sid, since))
        # describe() lives on the Hit so the tile, the console and the spoken
        # alert all say the same thing about how sure a source was. Called
        # duck-typed: this module must not import watchlist, which imports
        # sketch() from here.
        out.extend(h.describe() for h in self.watchlist)
        if self.sightings > 1:
            span = (self.last_seen - self.first_seen) / 86400.0
            out.append(f"seen {self.sightings}x over {span:.0f}d"
                       if span >= 1 else f"seen {self.sightings}x today")
        return out


class Database:
    def __init__(self, path: Path | None = None, verify=None, watchlist=None):
        # Resolved at CALL time, not bound as a default argument. As a default
        # it was captured when the class was defined, so `reputation.DB_PATH =
        # copy` before `Database()` silently kept using the real database - a
        # test written that way ran against live data and deleted a live
        # audit's images. None means "whatever DB_PATH says now".
        path = Path(path) if path else DB_PATH
        # `verify(handle) -> bool` decides whether a handle is a real account.
        # Merging is only permitted when it says a candidate is NOT real.
        self.verify = verify
        # Optional watchlist.Watchlist. INJECTED rather than imported, for two
        # reasons: watchlist.py imports sketch() from here, so importing it
        # back would be a cycle; and a Database with no watchlist has to keep
        # working exactly as before, because most callers (tests, migrations,
        # the corpus tools) have no business loading third-party accusations.
        self.watchlist = watchlist
        # Audit images are stored as paths relative to the DATABASE, not to
        # this module, so a database opened from somewhere else cannot reach
        # back and delete the real installation's files.
        self.root = path.parent
        self.db = dbopen.connect(path)
        # Schema and column migrations run ONCE per file per process, not on
        # every construction. The UI builds a Database per HTTP request, so
        # this was ~1 ms of repeated DDL per request - and two requests
        # arriving together raced the same ALTER TABLE.
        if not dbopen.migrated_once(path):
            return
        try:
            self._migrate()
        except Exception:
            # Un-mark the path so the NEXT connection retries. Marking it done
            # and then failing would leave every later Database in this
            # process talking to a half-built schema.
            dbopen.forget(path)
            raise

    def _migrate(self):
        self.db.executescript(SCHEMA)
        # Older databases predate the sketch column. The column has to exist
        # before the index that uses it, so neither can live in SCHEMA.
        po = {r[1] for r in self.db.execute("PRAGMA table_info(player_orgs)")}
        if "source" not in po:
            self.db.execute(
                "ALTER TABLE player_orgs ADD COLUMN source TEXT DEFAULT 'profile'")
        cols = {r[1] for r in self.db.execute("PRAGMA table_info(players)")}
        if "sketch" not in cols:
            self.db.execute("ALTER TABLE players ADD COLUMN sketch TEXT")
        # `handle` on an audit is the CANONICAL spelling, so the row attaches to
        # the same player the sighting did; `read_as` keeps what OCR actually
        # produced, which is the thing you are checking the crop against.
        # Before this they were the same column, stored raw, while audits_for()
        # looked up by canonical name - so an audit whose read got canonicalised
        # was orphaned the moment it was written: counted in the header, absent
        # from every tile.
        au = {r[1] for r in self.db.execute("PRAGMA table_info(audits)")}
        if "read_as" not in au:
            self.db.execute("ALTER TABLE audits ADD COLUMN read_as TEXT")
            self.db.execute(
                "UPDATE audits SET read_as=handle WHERE read_as IS NULL")
        if "reason" not in au:
            # Existing rows were all raised by the vote rule, so that is what
            # they are backfilled as. Getting this wrong in the other direction
            # would make corroboration unable to clear them and leave everyone
            # with a permanent pile of audits they cannot dismiss.
            self.db.execute(
                "ALTER TABLE audits ADD COLUMN reason TEXT DEFAULT 'weak_read'")
            self.db.execute(
                "UPDATE audits SET reason='weak_read' WHERE reason IS NULL")
        of = {r[1] for r in self.db.execute("PRAGMA table_info(org_flags)")}
        if "roster_ts" not in of:
            self.db.execute("ALTER TABLE org_flags ADD COLUMN roster_ts REAL")
        if "roster_n" not in of:
            self.db.execute(
                "ALTER TABLE org_flags ADD COLUMN roster_n INTEGER DEFAULT 0")
        # Which service produced the stored roster. NULL on everything pulled
        # before there were two, and every one of those came from
        # starcitizen-api.com - record_roster() reads NULL that way. Added by
        # name, and every write to this table names its columns, so the code
        # from before this column existed still runs against it.
        if "roster_src" not in of:
            self.db.execute("ALTER TABLE org_flags ADD COLUMN roster_src TEXT")
        # ...and per member, which is the one that decides anything - see
        # record_roster(). NULL means starcitizen-api.com, like above.
        om = {r[1] for r in self.db.execute("PRAGMA table_info(org_membership)")}
        if "src" not in om:
            self.db.execute("ALTER TABLE org_membership ADD COLUMN src TEXT")
        # Members YOU put on a roster, and what you know about them. Some orgs
        # keep most of their people OFF the public roster, so for them this is
        # the roster; see add_member_manually(). By name, like src above.
        if "manual_ts" not in om:
            self.db.execute("ALTER TABLE org_membership ADD COLUMN manual_ts REAL")
        if "note" not in om:
            self.db.execute("ALTER TABLE org_membership ADD COLUMN note TEXT")
        # Who put a hand-added member there: NULL is you, otherwise the install
        # of the org mate whose export brought them. See share.py.
        if "manual_src" not in om:
            self.db.execute("ALTER TABLE org_membership ADD COLUMN manual_src TEXT")
        # A report imported from somebody else remembers which of THEIR reports
        # it was, so their next export updates it instead of duplicating it.
        rp = {r[1] for r in self.db.execute("PRAGMA table_info(reports)")}
        if "origin" not in rp:
            self.db.execute("ALTER TABLE reports ADD COLUMN origin TEXT")
        self.db.execute(
            "CREATE INDEX IF NOT EXISTS idx_reports_origin ON reports(origin)")
        # Rosters pulled before the history existed become its first entries,
        # dated to when they were pulled - otherwise the first refresh would
        # report every existing member as having just joined.
        if not self.db.execute("SELECT 1 FROM org_membership LIMIT 1").fetchone():
            self.db.execute(
                """INSERT OR IGNORE INTO org_membership
                   (sid, handle, first_seen, last_seen)
                   SELECT p.sid, p.handle, COALESCE(f.roster_ts, f.ts),
                          COALESCE(f.roster_ts, f.ts)
                   FROM player_orgs p JOIN org_flags f ON f.sid = p.sid
                   WHERE p.source = 'roster'""")
        self.db.execute(
            "CREATE INDEX IF NOT EXISTS idx_players_sketch ON players(sketch)")
        for row in self.db.execute(
                "SELECT handle FROM players WHERE sketch IS NULL OR sketch=''"
        ).fetchall():
            h = row["handle"]
            self.db.execute("UPDATE players SET sketch=? WHERE handle=?",
                            (sketch(h), h))
        self.db.commit()

    # -- identity --------------------------------------------------------

    def canonical(self, handle: str, mutate: bool = True) -> tuple[str, str | None]:
        """Resolve a reading to the handle it should be stored under.

        Without this, one player accumulates history under several spellings -
        DAMADZK and DAMADZKIPPA - and "seen 6 times" becomes three records of
        two.

        BUT string similarity cannot decide identity, and getting this wrong
        attributes one person's history to another - the worst thing this tool
        can do. Two spellings that look like a misread of each other are
        routinely both real:

            SLIVER   enlisted 13y ago, org NTSVB
            5LIVER   enlisted 12y ago, no org
            HORNE    a real account
            HORNET65 a different real account

        So a merge requires EVIDENCE, not resemblance. `verify` is a callable
        returning True if a handle is a real account; when it says a candidate
        exists, the two are left as separate players however alike they look.
        With no verifier available nothing is merged, because being wrong here
        costs more than a split history does.

        Returns (canonical, renamed_from).
        """
        h = handle.upper()
        if self.db.execute("SELECT 1 FROM players WHERE handle=?", (h,)).fetchone():
            return h, None

        if self.verify is None:
            return h, None  # no evidence available - never guess at identity

        sk = sketch(h)
        row = self.db.execute(
            "SELECT handle FROM players WHERE sketch=? LIMIT 1", (sk,)).fetchone()
        if row:
            known = row["handle"]
            if self._both_real(h, known):
                return h, None
            if len(h) > len(known) and h.startswith(known):
                self._rename(known, h)
                return h, known
            return known, None

        # Truncations differ in length, so the sketch won't match. Try a stem
        # first (cheap, indexed), then fall back to a length-bounded scan -
        # OCR also truncates at the START ('P-9695-PW' for 'SP-9695-PW'), and
        # a start-anchored stem can never find those.
        candidates = []
        stem = sk[:4]
        if len(stem) >= 3:
            candidates = [r["handle"] for r in self.db.execute(
                "SELECT handle FROM players WHERE sketch LIKE ?", (stem + "%",))]
        if not candidates:
            candidates = [r["handle"] for r in self.db.execute(
                """SELECT handle FROM players
                   WHERE LENGTH(handle) BETWEEN ? AND ? LIMIT 400""",
                (len(h) - 3, len(h) + 3))]

        for known in candidates:
            if consensus.same_contact(h, known):
                if self._both_real(h, known):
                    return h, None
                if len(h) > len(known) and h.startswith(known):
                    if not mutate:
                        return h, None
                    self._rename(known, h)
                    return h, known
                return known, None
        return h, None

    def _both_real(self, a: str, b: str) -> bool:
        """Do both spellings correspond to actual accounts?

        If so they are two people who happen to read alike, and merging them
        would hand one person the other's history.
        """
        if self.verify is None:
            return True  # cannot tell - assume distinct, the safe direction
        try:
            return bool(self.verify(a)) and bool(self.verify(b))
        except Exception:
            return True

    def _both_accounts_exist(self, a: str, b: str) -> tuple[bool, bool]:
        """(both are real accounts, we were actually able to check).

        Distinct from _both_real, which answers "should the automatic path
        leave these alone" and says yes when it cannot tell. Here the caller
        needs to know the difference between "checked, both exist" and "could
        not check", because a human is supplying the evidence either way.
        """
        if self.verify is None:
            return False, False
        try:
            return (bool(self.verify(a)) and bool(self.verify(b))), True
        except Exception:
            return False, False

    def close(self):
        """Release the connection.

        Existed nowhere before, so every caller relied on refcounting. That
        works in CPython right up until something holds a reference - a
        retained traceback pinned 51 connections in a test - so the UI now
        closes explicitly and the guarantee stops being incidental.
        """
        try:
            self.db.close()
        except Exception:
            pass

    def _rename(self, old: str, new: str) -> bool:
        """Fold `old`'s history into `new`. True if anything actually moved.

        Two cases, and an early version only handled the first: resolving a
        new reading (where `new` does not exist yet, so a rename suffices), and
        consolidating two records that both exist, where renaming collides with
        the primary key. The second needs a merge.

        Returns a bool because the "nothing to move" case used to be silent.
        confirm_audit() called this, ignored it, and reported renamed=True to a
        user whose correction had gone nowhere.
        """
        target = self.db.execute(
            "SELECT sightings, first_seen FROM players WHERE handle=?",
            (new,)).fetchone()
        src = self.db.execute(
            "SELECT sightings, first_seen FROM players WHERE handle=?",
            (old,)).fetchone()
        if src is None:
            return False

        if target is None:
            self.db.execute(
                "UPDATE players SET handle=?, sketch=? WHERE handle=?",
                (new, sketch(new), old))
        else:
            self.db.execute(
                """UPDATE players SET
                     sightings = sightings + ?,
                     first_seen = MIN(first_seen, ?)
                   WHERE handle=?""",
                (src["sightings"] or 0,
                 src["first_seen"] or target["first_seen"], new))
            self.db.execute("DELETE FROM players WHERE handle=?", (old,))

        for table in ("sightings", "reports", "player_orgs", "player_manual"):
            self.db.execute(
                f"UPDATE OR IGNORE {table} SET handle=? WHERE handle=?",
                (new, old))
            self.db.execute(f"DELETE FROM {table} WHERE handle=?", (old,))
        self.db.commit()
        return True

    def consolidate(self) -> list[tuple[str, str]]:
        """Merge variant spellings already in the database.

        Deduplication happens on write now, but records written before that
        are still split - SLIVER and 5LIVER as two players, each with half the
        history. Walks longest-first so the fuller spelling wins.
        """
        handles = [r["handle"] for r in self.db.execute(
            "SELECT handle FROM players ORDER BY LENGTH(handle) DESC")]
        merged, skipped, gone = [], [], set()
        for keep in handles:
            if keep in gone:
                continue
            for other in handles:
                if other == keep or other in gone:
                    continue
                if not consensus.same_contact(keep, other):
                    continue
                if self._both_real(keep, other):
                    skipped.append((other, keep))
                    continue
                self._rename(other, keep)
                merged.append((other, keep))
                gone.add(other)
        return merged, skipped

    # -- writes ----------------------------------------------------------

    def record_sighting(self, handle: str, range_km: float | None = None,
                        votes: int = 0, total: int = 0) -> int:
        """Record one sighting. Returns how many times this handle is now seen.

        The count is returned because corroboration is not only an intra-burst
        idea. Frames inside one burst are half a second apart and near
        identical, so OCR tends to repeat the same mistake and consensus
        confirms it - the README records that measurement. Two SEPARATE pings
        agreeing on a spelling is independent evidence, and at least as strong.
        Callers that gate on vote counts need this to avoid treating a contact
        seen five times as unconfirmed.
        """
        h, _ = self.canonical(handle)
        now = time.time()
        self.db.execute(
            """INSERT INTO players (handle, first_seen, last_seen, sightings, sketch)
               VALUES (?,?,?,1,?)
               ON CONFLICT(handle) DO UPDATE SET
                 last_seen=excluded.last_seen, sightings=sightings+1""",
            (h, now, now, sketch(h)))
        self.db.execute(
            "INSERT INTO sightings (handle, ts, range_km, votes, total) VALUES (?,?,?,?,?)",
            (h, now, range_km, votes, total))
        self.db.commit()
        row = self.db.execute("SELECT sightings FROM players WHERE handle=?",
                              (h,)).fetchone()
        seen = int(row["sightings"]) if row else 0
        # A corroborated read answers the question an older audit was asking -
        # and being seen again on a LATER ping corroborates just as well as a
        # second frame in the same burst did. Without the max() an audit raised
        # on a 1/1 read survived every subsequent 1/1 sighting of the same
        # handle, however many times it was confirmed.
        self.clear_corroborated_audits(h, max(votes, seen))
        return seen

    # -- things that are not people ---------------------------------------

    def add_ignored(self, label: str, note: str = "") -> dict:
        """Never report this label again, and forget what it already left.

        For a label the HUD draws like a contact but which is not a person -
        the floating construction-salvage crate that reads as a handle and
        gets a range line under it like anything else.

        Deliberately narrow. It suppresses THIS string and OCR variants of it,
        not anything that merely resembles it: the filter runs before a name
        reaches the database, so a rule that is too loose silently stops
        reporting a real player and there is nothing left to notice it by.
        """
        lab = (label or "").strip().upper()
        if not lab:
            raise ValueError("no label given")
        removed = self.forget(lab)          # it is junk; take its history too
        self.db.execute(
            """INSERT INTO ignored (label, sketch, note, ts, hits)
               VALUES (?,?,?,?,0)
               ON CONFLICT(label) DO UPDATE SET note=excluded.note""",
            (lab, sketch(lab), note, time.time()))
        self.db.commit()
        return {"label": lab, "removed": removed}

    def remove_ignored(self, label: str) -> bool:
        cur = self.db.execute("DELETE FROM ignored WHERE label=?",
                              ((label or "").strip().upper(),))
        self.db.commit()
        return cur.rowcount > 0

    def ignored_list(self) -> list[dict]:
        return [dict(r) for r in self.db.execute(
            "SELECT * FROM ignored ORDER BY hits DESC, label").fetchall()]

    def is_ignored(self, name: str) -> str | None:
        """The rule that suppresses this name, or None.

        Matches the exact string first, then the confusion-collapsed sketch, so
        one entry covers CONSTRUCTIONSALVAGE and CONSTRUCTI0NSALVAGE without
        needing both typed in. Sketch collapses only the pairs OCR actually
        confuses (O/0/Q/D, I/L/1, S/5 ...), so it cannot reach a genuinely
        different word.
        """
        n = (name or "").strip().upper()
        if not n:
            return None
        row = self.db.execute("SELECT label FROM ignored WHERE label=?",
                              (n,)).fetchone()
        if row is None:
            row = self.db.execute("SELECT label FROM ignored WHERE sketch=?",
                                  (sketch(n),)).fetchone()
        if row is None:
            return None
        self.db.execute("UPDATE ignored SET hits=hits+1 WHERE label=?",
                        (row["label"],))
        self.db.commit()
        return row["label"]

    def ensure_player(self, handle: str) -> bool:
        """Add a handle with no sighting attached. True if it was new.

        For a player added by hand - somebody named in chat, or looked up
        because you are about to meet them. `sightings` stays 0 because you have
        not seen them: the count is evidence about encounters, and inflating it
        by one for every manual entry would quietly corrupt "seen 6 times".
        """
        h, _ = self.canonical(handle, mutate=False)
        now = time.time()
        cur = self.db.execute(
            """INSERT INTO players (handle, first_seen, last_seen, sightings, sketch)
               VALUES (?,?,?,0,?) ON CONFLICT(handle) DO NOTHING""",
            (h, now, now, sketch(h)))
        self.db.commit()
        return cur.rowcount > 0

    def add_report(self, handle: str, category: str, note: str = "",
                   evidence: str = "", source: str = "self"):
        h, _ = self.canonical(handle)
        now = time.time()
        self.db.execute(
            """INSERT INTO players (handle, first_seen, last_seen, sightings, sketch)
               VALUES (?,?,?,0,?) ON CONFLICT(handle) DO NOTHING""",
            (h, now, now, sketch(h)))
        self.db.execute(
            "INSERT INTO reports (handle, ts, category, note, evidence, source)"
            " VALUES (?,?,?,?,?,?)", (h, now, category, note, evidence, source))
        self.db.commit()

    def pending_reports(self) -> list[tuple[str, float]]:
        """Handles flagged but not yet explained, newest first."""
        return [(r[0], r[1]) for r in self.db.execute(
            "SELECT handle, MAX(ts) FROM reports WHERE category=?"
            " GROUP BY handle ORDER BY MAX(ts) DESC", (PENDING,))]

    def resolve_pending(self, handle: str, category: str, note: str = "",
                        evidence: str = "") -> int:
        """Give a reason to the flags raised against a handle in the moment.

        Rewrites the pending rows rather than adding new ones, so a flag and
        its explanation stay one event. The original timestamp is kept - the
        report is evidence about when the thing happened, not about when you
        got round to describing it, and the 180-day decay reads that column.
        """
        if category not in flags_mod.keys() or category == PENDING:
            raise ValueError(f"category must be one of "
                             f"{[c for c in flags_mod.keys() if c != PENDING]}")
        h, _ = self.canonical(handle)
        cur = self.db.execute(
            "UPDATE reports SET category=?, note=?, evidence=?"
            " WHERE handle=? AND category=?",
            (category, note, evidence, h, PENDING))
        self.db.commit()
        return cur.rowcount

    def edit_report(self, report_id: int, category: str | None = None,
                    note: str | None = None, evidence: str | None = None) -> bool:
        """Change one report in place.

        Reports get written before the story is complete - a flag pressed
        mid-fight, a category guessed at, a note added later when it turns out
        the same person did it twice. Editing one is not falsifying evidence,
        it is finishing writing it down.

        The TIMESTAMP is never touched. A report dates the incident, not the
        paperwork, and the 180-day decay reads that column.
        """
        sets, vals = [], []
        if category is not None:
            # Retired keys are accepted on purpose: editing an old report
            # must not fail just because its reason has since left the
            # dropdown. flags.keys() is every key ever defined.
            if category not in flags_mod.keys():
                raise ValueError(
                    f"category must be one of {flags_mod.keys()}")
            sets.append("category=?"); vals.append(category)
        if note is not None:
            sets.append("note=?"); vals.append(note)
        if evidence is not None:
            sets.append("evidence=?"); vals.append(evidence)
        if not sets:
            return False
        vals.append(int(report_id))
        cur = self.db.execute(
            f"UPDATE reports SET {', '.join(sets)} WHERE id=?", vals)
        self.db.commit()
        return cur.rowcount > 0

    def delete_report(self, report_id: int) -> dict | None:
        """Remove one report. Returns what it was, or None if already gone.

        Needed because a flag is one button press and some of those are tests,
        misfires, or the wrong tile. Without this the only way to undo one was
        forget(), which throws away the sighting history too - so a mis-click
        cost more than the mistake did.
        """
        row = self.db.execute(
            "SELECT handle, category, note, ts FROM reports WHERE id=?",
            (int(report_id),)).fetchone()
        if row is None:
            return None
        self.db.execute("DELETE FROM reports WHERE id=?", (int(report_id),))
        self.db.commit()
        return {"handle": row["handle"], "category": row["category"],
                "note": row["note"] or "", "ts": row["ts"]}

    # ---- audits -------------------------------------------------------
    #
    # Files live in audit/ and are owned by the row that points at them, so
    # every path that removes a row removes its files too. A row without its
    # images is useless - the whole point is looking at the crop.

    def add_audit(self, handle: str, range_km, votes: int, total: int,
                  variants, frame_path: str = "", crop_name: str = "",
                  crop_range: str = "", reason: str = "weak_read") -> int:
        """Record a read worth checking by eye. Returns the row id.

        Canonicalised the same way record_sighting() is, so the audit lands on
        the same player row the sighting did. Storing the raw read here instead
        produced audits attached to nothing: the header counted them and no tile
        could show them, because audits_for() resolves through canonical().

        The raw string is kept in `read_as` - it is what you are checking the
        crop against, so losing it would defeat the point.
        """
        raw = (handle or "").upper()
        h, _ = self.canonical(raw, mutate=False)
        # One open audit per handle for the NON-vote reasons. "RSI has never
        # heard of this name" is the same fact however many times it is
        # restated, so restating it across sessions would leave you forty rows
        # asking one question.
        #
        # Deliberately NOT applied to weak_read. Two poor reads in two separate
        # bursts are two independent looks at the label, each with its own
        # crop, and the second may be the legible one. Collapsing them would
        # throw away evidence to tidy a count.
        if reason != "weak_read":
            dup = self.db.execute(
                "SELECT id FROM audits WHERE handle=? AND reason=?",
                (h, reason)).fetchone()
            if dup is not None:
                return int(dup["id"])
        cur = self.db.execute(
            """INSERT INTO audits (ts, handle, read_as, range_km, votes, total,
                                   variants, frame_path, crop_name, crop_range,
                                   reason)
               VALUES (?,?,?,?,?,?,?,?,?,?,?)""",
            (time.time(), h, raw, range_km, votes, total,
             json.dumps(list(variants or [])), frame_path, crop_name, crop_range,
             reason))
        self.db.commit()
        return int(cur.lastrowid)

    def clear_corroborated_audits(self, handle: str, votes: int) -> int:
        """Drop open audits for a handle a later read has now confirmed.

        An audit means "nobody corroborated this read". Once the same handle
        comes back with two or more agreeing votes, that is no longer true, and
        leaving the row up asks you to check something the tool has already
        answered. Same threshold _maybe_audit() uses to raise one, so the rule
        reads the same in both directions.

        Only touches audits for THIS handle. A different spelling is a different
        claim and still needs looking at.

        And only audits raised BY the vote rule. An audit raised because the
        handle has no RSI account is asking a question corroboration cannot
        answer: VONCARSTEINO was read 18/18 and was still wrong. Clearing it on
        vote count would delete the row every time the misread repeated, which
        is exactly when it repeats.
        """
        if votes < 2:
            return 0
        h, _ = self.canonical(handle, mutate=False)
        rows = self.db.execute(
            "SELECT * FROM audits WHERE handle=? AND"
            " COALESCE(reason,'weak_read')='weak_read'", (h,)).fetchall()
        for row in rows:
            self._drop_audit_files(row)
        if rows:
            self.db.execute(
                "DELETE FROM audits WHERE handle=? AND"
                " COALESCE(reason,'weak_read')='weak_read'", (h,))
            self.db.commit()
        return len(rows)

    def audits_for(self, handle: str) -> list[dict]:
        h, _ = self.canonical(handle, mutate=False)
        out = []
        for r in self.db.execute(
                "SELECT * FROM audits WHERE handle=? OR handle=? ORDER BY ts DESC",
                (h, handle.upper())):
            d = dict(r)
            try:
                d["variants"] = json.loads(d["variants"] or "[]")
            except Exception:
                d["variants"] = []
            out.append(d)
        return out

    def open_audits(self) -> list[dict]:
        rows = []
        for r in self.db.execute("SELECT * FROM audits ORDER BY ts DESC"):
            d = dict(r)
            try:
                d["variants"] = json.loads(d["variants"] or "[]")
            except Exception:
                d["variants"] = []
            rows.append(d)
        return rows

    def _drop_audit_files(self, row):
        """Delete the pictures behind one audit row.

        The paths come out of the database, and this deletes files, so it
        checks rather than trusts: only inside audit/, only a file. Nothing
        can currently put a path here except _save_audit_images(), which
        builds them itself - the check is so that stays true if anything else
        ever writes the row, because the failure mode is deleting somebody's
        file outside the data directory.
        """
        allowed = (self.root / "audit").resolve()
        for key in ("frame_path", "crop_name", "crop_range"):
            rel = (row[key] if isinstance(row, dict) else row[key]) or ""
            if not rel:
                continue
            try:
                f = (self.root / rel).resolve()
                if f.parent == allowed and f.is_file():
                    f.unlink(missing_ok=True)
            except OSError:
                pass

    def dismiss_audit(self, audit_id: int) -> bool:
        """Drop an audit without changing the record - the read was fine."""
        row = self.db.execute("SELECT * FROM audits WHERE id=?",
                              (int(audit_id),)).fetchone()
        if row is None:
            return False
        self._drop_audit_files(row)
        self.db.execute("DELETE FROM audits WHERE id=?", (int(audit_id),))
        self.db.commit()
        return True

    def confirm_audit(self, audit_id: int, correct: str,
                      force: bool = False) -> dict:
        """Say what the label actually said, and fold the record onto it.

        Updates the EXISTING record rather than creating a new one, so the
        sighting history the bad spelling accumulated follows the real handle
        instead of being stranded.

        Refuses, unless forced, when both spellings resolve to real accounts.
        That is the case canonical() exists to protect - SLIVER and 5LIVER are
        two different people - and a human reading a blurry crop is exactly the
        situation where it could be got wrong. The UI asks first and passes
        force=True once you have said yes.
        """
        row = self.db.execute("SELECT * FROM audits WHERE id=?",
                              (int(audit_id),)).fetchone()
        if row is None:
            return {"ok": False, "error": "audit not found"}
        old = row["handle"].upper()
        new = (correct or "").strip().upper()
        if not new:
            return {"ok": False, "error": "no name given"}

        renamed = False
        adopted = False          # no record to rename, so one was created
        checked = False          # was the identity question actually asked?
        if new != old:
            # NOT _both_real(): that returns True when there is no verifier,
            # which is right for the automatic path (guess nothing without
            # evidence) and wrong here. A person has looked at the crop and
            # said what it says - that IS the evidence, and blocking on a
            # missing API key would make the feature unusable without one.
            # Only a verifier positively finding BOTH accounts is a reason to
            # stop.
            both, checked = self._both_accounts_exist(old, new)
            if not force and both:
                return {"ok": False, "needs_confirm": True, "old": old,
                        "new": new, "checked": checked,
                        "error": (f"{old} and {new} both resolve to real "
                                  f"accounts - merging them would give one "
                                  f"person the other's history")}
            renamed = self._rename(old, new)
            if not renamed:
                # Nothing to move: the record this audit pointed at is gone.
                # It can be ignored as "not a person", forgotten by a retention
                # sweep, or already folded away by an earlier confirm. This
                # used to fall through reporting renamed=True, deleting the
                # audit, and discarding the correction without a word.
                #
                # The read still happened, and a human has just said what it
                # actually said, so the correction is kept as a record of its
                # own - UNLESS you had explicitly marked that label "not a
                # person", in which case recreating it would undo a decision
                # you made on purpose.
                if self.is_ignored(new):
                    return {"ok": False, "old": old, "new": new,
                            "error": (f"{new} is on your ignore list as "
                                      f"'not a person' - remove it there first "
                                      f"if that was wrong")}
                self.record_sighting(new, row["range_km"],
                                     row["votes"] or 0, row["total"] or 0)
                adopted = True

        self._drop_audit_files(row)
        self.db.execute("DELETE FROM audits WHERE id=?", (int(audit_id),))
        # Any other open audit for the old spelling now points at a handle that
        # no longer exists; move them so they stay reachable.
        self.db.execute("UPDATE audits SET handle=? WHERE handle=?", (new, old))
        self.db.commit()
        # `checked` travels with the result so a merge made without a verifier
        # is visible rather than silent. It is not a refusal - see above - but
        # "folded 5LIVER into SLIVER, nobody confirmed they are the same
        # person" is worth saying out loud.
        return {"ok": True, "old": old, "new": new, "renamed": renamed,
                "adopted": adopted, "checked": checked}

    def orphan_audits(self, apply: bool = False) -> list[dict]:
        """Audits pointing at a player row that no longer exists.

        The header counts every audit row; a tile can only show an audit for a
        contact that is in the list. So an audit whose player has been forgotten
        or purged shows up as a number you cannot act on - "to audit 1" with no
        audit button anywhere.

        Two ways one is created: the player was deleted after the audit was
        written, or - before add_audit() canonicalised - the audit was filed
        under a raw read while the sighting went to the canonical spelling.
        """
        out = []
        for row in self.db.execute("SELECT * FROM audits").fetchall():
            hit = self.db.execute("SELECT 1 FROM players WHERE handle=?",
                                  (row["handle"],)).fetchone()
            if hit:
                continue
            out.append({"id": row["id"], "handle": row["handle"],
                        "read_as": (row["read_as"] if "read_as" in row.keys()
                                    else row["handle"]) or row["handle"]})
            if apply:
                self._drop_audit_files(row)
                self.db.execute("DELETE FROM audits WHERE id=?", (row["id"],))
        if apply and out:
            self.db.commit()
        return out

    def audit_files_in_use(self) -> set:
        """Every file an audit row points at, as paths relative to the project.

        Exists so nothing ever blanket-deletes audit/ again. A row without its
        image is dead weight - the entire point is looking at the picture - and
        the directory is written to by a RUNNING watcher, so "it only has my
        test files in it" stops being true the moment a burst comes in. It did,
        and three real audits lost their crops.
        """
        used = set()
        for r in self.db.execute(
                "SELECT crop_name, crop_range, frame_path FROM audits"):
            for v in r:
                if v:
                    used.add(str(v).replace("\\", "/"))
        return used

    def prune_orphan_audit_files(self, apply: bool = False) -> list:
        """Files in audit/ that no row points at. The inverse of prune_audits."""
        base = self.root / "audit"
        if not base.is_dir():
            return []
        used = self.audit_files_in_use()
        orphans = []
        for f in base.iterdir():
            if not f.is_file():
                continue
            rel = f"audit/{f.name}"
            if rel not in used:
                orphans.append(rel)
                if apply:
                    try:
                        f.unlink()
                    except OSError:
                        pass
        return orphans

    def prune_audits(self, max_age_days: float = 7.0) -> dict:
        """Age out audits nobody got round to checking, and their files."""
        cutoff = time.time() - max_age_days * 86400.0
        rows = list(self.db.execute("SELECT * FROM audits WHERE ts < ?", (cutoff,)))
        for r in rows:
            self._drop_audit_files(r)
        self.db.execute("DELETE FROM audits WHERE ts < ?", (cutoff,))
        self.db.commit()
        return {"removed": len(rows)}

    def flag_org(self, sid: str, note: str = "", name: str = ""):
        self.db.execute(
            "INSERT OR REPLACE INTO org_flags (sid, name, ts, note) VALUES (?,?,?,?)",
            (sid.upper(), name, time.time(), note))
        self.db.commit()

    def unflag_org(self, sid: str):
        """Drop the flag AND the roster rows it put there.

        A roster row exists only to carry a flag to a member. Left behind after
        the flag is gone it carries nothing, but it is still a stored claim
        that a named person belongs to a piracy org, accumulating for every org
        ever flagged and then unflagged. 'profile' rows survive: those are what
        the API says about that player directly, and they were not ours to
        write or to delete.
        """
        sid = sid.upper()
        self.db.execute(
            "DELETE FROM player_orgs WHERE sid=? AND source='roster'", (sid,))
        # The membership history goes with it, for the same reason: it exists
        # to say something about people in an org YOU flagged. Unflagged, it
        # is a list of strangers' past memberships nobody asked to keep.
        self.db.execute("DELETE FROM org_membership WHERE sid=?", (sid,))
        self.db.execute("DELETE FROM org_flags WHERE sid=?", (sid,))
        self.db.commit()

    def find_ghosts(self, client=None, limit: int = 0) -> dict:
        """Players the RSI API positively says do not exist.

        These are OCR inventions - 'ONNCARSTE' for VONCARSTEIN, 'OOFINSM' for
        DOFINSMERTS - and they dilute the sighting counts that everything else
        is judged on.

        The bar is a POSITIVE negative: the API was asked and answered that the
        account does not exist. Three states are deliberately NOT ghosts:

          unchecked   never looked up, because it never reached the vote
                      threshold. Absence of evidence.
          unverified  no API key configured, so nothing can be claimed.
          error       the request failed. rsi._get raises on anything but a
                      404, so a network blip cannot be mistaken for absence -
                      but if that ever changed, an errored handle must not be
                      silently deleted.

        Two exclusions on top, both because deleting would destroy the thing
        that could correct the record:

          reports     a human wrote that down. It is evidence about a person,
                      and it outranks a spelling.
          audits      the crop is sitting there waiting to be read by eye. That
                      is exactly how a ghost becomes a real handle.

        Returns the buckets rather than acting, so a caller can show the list
        before anything is deleted.
        """
        import rsi as rsi_mod
        client = client or rsi_mod.Client()
        out = {"ghosts": [], "kept": [], "unchecked": [], "errors": []}
        rows = self.db.execute(
            "SELECT handle, sightings FROM players ORDER BY handle").fetchall()
        for row in rows:
            h = row["handle"]
            if limit and len(out["ghosts"]) >= limit:
                break
            try:
                cit = client.citizen(h)
            except Exception as exc:
                out["errors"].append((h, f"{type(exc).__name__}: {exc}"))
                continue
            if getattr(cit, "unverified", False):
                out["unchecked"].append(h)
                continue
            if cit.exists:
                continue
            n_rep = self.db.execute(
                "SELECT COUNT(*) FROM reports WHERE handle=?", (h,)).fetchone()[0]
            n_aud = self.db.execute(
                "SELECT COUNT(*) FROM audits WHERE handle=?", (h,)).fetchone()[0]
            item = {"handle": h, "sightings": row["sightings"] or 0,
                    "reports": n_rep, "audits": n_aud}
            if n_rep or n_aud:
                item["why"] = ("has a report" if n_rep else "") + \
                              (" and " if n_rep and n_aud else "") + \
                              ("waiting on an audit" if n_aud else "")
                out["kept"].append(item)
            else:
                out["ghosts"].append(item)
        return out

    def purge_ghosts(self, client=None, apply: bool = False) -> dict:
        """Delete every handle find_ghosts() is sure about. Dry run by default."""
        found = self.find_ghosts(client=client)
        if apply:
            for g in found["ghosts"]:
                self.forget(g["handle"])
        found["applied"] = bool(apply)
        return found

    def forget(self, handle: str) -> dict:
        """Erase a handle entirely. Returns what was removed.

        OCR occasionally invents a person: a truncated 'UNKNOWN' that lost both
        ends reads as 'NKNE', passes the handle pattern, and lands here as
        somebody. There was no way to take one back out, so the database
        accumulated phantoms that then diluted the real 'seen N times' counts.

        This deletes rows, so it is deliberately narrow - one exact handle, no
        pattern matching, no cascade to similar spellings. Merging two spellings
        that ARE the same person is a different operation with a different bar:
        consolidate() does that, and only with verification. Deleting the wrong
        row here loses evidence about a real person, so the UI confirms first
        and reports exactly what went.
        """
        h = handle.upper()
        removed = {
            "audits": self.db.execute(
                "SELECT COUNT(*) FROM audits WHERE handle=?", (h,)).fetchone()[0],
            "sightings": self.db.execute(
                "SELECT COUNT(*) FROM sightings WHERE handle=?", (h,)).fetchone()[0],
            "reports": self.db.execute(
                "SELECT COUNT(*) FROM reports WHERE handle=?", (h,)).fetchone()[0],
            "orgs": self.db.execute(
                "SELECT COUNT(*) FROM player_orgs WHERE handle=?", (h,)).fetchone()[0],
            "player": self.db.execute(
                "SELECT COUNT(*) FROM players WHERE handle=?", (h,)).fetchone()[0],
        }
        if not any(removed.values()):
            return removed
        # Audits own image files, so they cannot just be DELETEd with the rest -
        # and leaving them behind would orphan rows whose contact no longer has
        # a tile, making them unreachable until the weekly prune.
        for r in self.db.execute("SELECT * FROM audits WHERE handle=?", (h,)):
            self._drop_audit_files(r)
        # player_orgs is not in this list. A 'roster' row there says an org
        # YOU flagged lists this handle, which is the org's published fact and
        # not part of your record of a person - and deleting it silently
        # disarmed the flag: forget someone on a flagged roster and the next
        # time you met them the tool called them clean, until the next roster
        # pull put the row back. The 'profile' rows, which came from their RSI
        # profile, go with the rest.
        self.db.execute(
            "DELETE FROM player_orgs WHERE handle=? AND source!='roster'", (h,))
        for table in ("sightings", "reports", "audits", "players",
                      "player_manual"):
            self.db.execute(f"DELETE FROM {table} WHERE handle=?", (h,))
        self.db.commit()
        return removed

    def set_orgs(self, handle: str, orgs):
        """Remember which orgs a handle belongs to, so org flags can reach them.

        Only replaces what the PROFILE said. Roster-derived rows survive,
        because they carry the one thing a profile lookup cannot: affiliate
        membership. Wiping them here would silently undo every roster import
        the first time that player was looked up again.
        """
        h = handle.upper()
        self.db.execute(
            "DELETE FROM player_orgs WHERE handle=? AND COALESCE(source,'profile')='profile'",
            (h,))
        for o in orgs:
            self.db.execute(
                "INSERT OR REPLACE INTO player_orgs (handle, sid, is_main, source)"
                " VALUES (?,?,?, 'profile')",
                (h, o.sid.upper(), 1 if getattr(o, "is_main", False) else 0))
        self.db.commit()

    def add_org_members(self, sid: str, handles: list[str],
                        replace: bool = True) -> int:
        """Record a flagged org's roster so its members match locally.

        This is the affiliate workaround the README describes, and it is what
        makes flagging an org actually reach people: the API's user endpoint
        returns only a main org, so a pirate side-org - the likely case - is
        invisible from the profile alone.

        It records MEMBERSHIP, not blame. No report is written against anyone
        here. Being in an org is a fact about them; 'they attacked me' is a
        claim about something they did, and inventing 150 of those would poison
        the one signal this database exists to carry.
        """
        sid = sid.upper()
        # REPLACE the roster rather than adding to it. People leave orgs, and a
        # membership recorded once and never removed would keep handing them an
        # org flag years after they walked away - a claim about someone that
        # nothing supports any more. Only 'roster' rows are cleared; a
        # 'profile' row is what the API says about that player directly and is
        # not ours to delete here, which is the same split set_orgs() honours
        # from the other side.
        #
        # replace=False for a roster that came back TRUNCATED. Replacing from
        # half a list drops the flag from everyone on the half we did not get,
        # which is the same silent unflagging in the other direction.
        if replace:
            self.db.execute(
                "DELETE FROM player_orgs WHERE sid=? AND source='roster'", (sid,))
        n = 0
        for handle in handles:
            h = (handle or "").strip().upper()
            if not h:
                continue
            # OR IGNORE, so a member whose profile already names this org keeps
            # the stronger 'profile' row with its is_main flag intact.
            self.db.execute(
                "INSERT OR IGNORE INTO player_orgs (handle, sid, is_main, source)"
                " VALUES (?,?,0,'roster')", (h, sid))
            n += 1
        # Record the fetch even for an org that came back empty. "Asked, got
        # nothing" and "never asked" look identical otherwise, and they call
        # for opposite actions. A truncated pull updates the timestamp but not
        # the count: a partial number presented as the roster size would read
        # as an org that had shrunk.
        if replace:
            self.db.execute(
                "UPDATE org_flags SET roster_ts=?, roster_n=? WHERE sid=?",
                (time.time(), n, sid))
        else:
            self.db.execute(
                "UPDATE org_flags SET roster_ts=? WHERE sid=?",
                (time.time(), sid))
        self.db.commit()
        return n

    def record_roster(self, sid: str, members: list[dict],
                      complete: bool = True, source: str | None = None) -> dict:
        """Store a freshly pulled roster: current membership AND its history.

        `members` are the API's records - handle, display, rank, stars.
        Current membership still goes through add_org_members(), so an org
        flag reaches exactly the people the roster lists NOW. The history
        records everyone it has ever listed: who is new, who has dropped off
        since the last pull, and who has come back.

        `complete` says whether the list is the WHOLE roster - see
        rsi.members_detail(), which is the only thing that can know. On a
        truncated pull the people we did see are recorded and NOBODY is
        retired, because "missing from this list" only means "left the org"
        when the list was finished. It is not a rare distinction: a 40-member
        org whose page 2 came back empty retired 24 people in a test, and
        former membership raises an alert - so the bug would have fabricated
        two dozen alerts about real people leaving an org they never left.

        `source` is which service pulled it, and A SOURCE CAN ONLY RETIRE THE
        PEOPLE IT LISTED. The two services see different subsets of a roster -
        measured on one org the same afternoon, SENTRY listed 511 and
        starcitizen-api.com 463 - so one service's silence about somebody the
        OTHER service listed is not evidence of anything. Comparing across
        them would "retire" everyone the second happens not to see: dozens of
        alerts about people leaving an org they are still in, caused by
        nothing but which service answered.

        An org-level "did the source change" flag was tried first and was not
        enough: it spared the pull that switched sources, then the NEXT pull
        from the new source retired everybody the old one had listed and the
        new one never had. test_sentry.py case O is that. So each member now
        carries the source that last listed them (`src`), and is retired only
        when that same source, on a complete pull, no longer does. Somebody
        only one service can see stays current until that service says
        otherwise - leavers going unnoticed is the cost, and it is the right
        way round.

        Current membership - the player_orgs rows that carry an org flag to a
        person - is rewritten from this history every time, so the two cannot
        disagree: someone is flagged as a current member exactly when their
        history says they have not left. None as `source` means
        starcitizen-api.com, which is what every roster was before there were
        two.
        """
        sid = sid.upper()
        now = time.time()
        row = self.db.execute(
            "SELECT roster_src FROM org_flags WHERE sid=?", (sid,)).fetchone()
        # Rosters pulled before sources were recorded all came from
        # starcitizen-api.com, so that is what NULL means.
        this_src = source or "sci"
        switched = this_src != ((row["roster_src"] if row else None) or "sci")
        listed = {}
        for m in members or []:
            h = (m.get("handle") or "").strip().upper()
            # Hidden members arrive without a handle, or not at all. Either
            # way there is nobody to record.
            if not h or h == "REDACTED":
                continue
            listed[h] = m
        before = {r["handle"]: r for r in self.db.execute(
            "SELECT handle, gone_since, src, manual_ts FROM org_membership "
            "WHERE sid=?", (sid,))}
        joined = [h for h in listed if h not in before]
        back = [h for h in listed if h in before and before[h]["gone_since"]]
        # Only people THIS source listed last time, and only on a list that
        # was finished. See above for why both conditions.
        # And never somebody YOU put on it: a roster's silence about a person
        # is exactly what an org that keeps its people off the public list
        # looks like. Only you take them off - remove_manual_member().
        left = [h for h, r in before.items()
                if complete and h not in listed and not r["gone_since"]
                and not r["manual_ts"] and (r["src"] or "sci") == this_src]
        for h, m in listed.items():
            self.db.execute(
                """INSERT INTO org_membership
                     (sid, handle, display, rank, stars, first_seen, last_seen,
                      gone_since, src)
                   VALUES (?,?,?,?,?,?,?,NULL,?)
                   ON CONFLICT(sid, handle) DO UPDATE SET
                     display=excluded.display, rank=excluded.rank,
                     stars=excluded.stars, last_seen=excluded.last_seen,
                     gone_since=NULL, src=excluded.src""",
                (sid, h, m.get("display") or "", m.get("rank") or "",
                 int(m.get("stars") or 0), now, now, this_src))
        for h in left:
            self.db.execute(
                "UPDATE org_membership SET gone_since=? WHERE sid=? AND handle=?",
                (now, sid, h))
        self.db.execute("UPDATE org_flags SET roster_src=? WHERE sid=?",
                        (this_src, sid))
        self.db.commit()
        # Current membership is whoever the history says has not left - this
        # pull's list, plus anybody it could not speak for.
        current = [r[0] for r in self.db.execute(
            "SELECT handle FROM org_membership WHERE sid=? AND gone_since IS NULL",
            (sid,))]
        self.add_org_members(sid, current, replace=True)
        # roster_n is what the PUBLIC roster holds; members only you have
        # listed are counted apart (flagged_orgs()["manual"]).
        self.db.execute(
            """UPDATE org_flags SET roster_n=(
                 SELECT COUNT(*) FROM org_membership WHERE sid=?
                 AND gone_since IS NULL AND COALESCE(src,'sci')!='manual')
               WHERE sid=?""", (sid, sid))
        self.db.commit()
        return {"members": len(listed), "joined": joined, "left": left,
                "back": back, "partial": not complete, "switched": switched}

    def members_of(self, sid: str) -> list[dict]:
        """Everyone this org's roster has listed, current members first."""
        sid = sid.upper()
        out = []
        for r in self.db.execute(
                """SELECT m.*, p.sightings, p.last_seen AS met_last,
                          s.name AS manual_by
                   FROM org_membership m LEFT JOIN players p ON p.handle = m.handle
                   LEFT JOIN shares s ON s.install = m.manual_src
                   WHERE m.sid=?
                   ORDER BY (m.gone_since IS NOT NULL), m.handle""", (sid,)):
            out.append({
                "handle": r["handle"], "display": r["display"] or "",
                "rank": r["rank"] or "", "stars": r["stars"] or 0,
                "first_seen": r["first_seen"], "last_seen": r["last_seen"],
                "gone_since": r["gone_since"],
                # Yours, and whether the public roster lists them as well.
                "manual_ts": r["manual_ts"], "note": r["note"] or "",
                "manual_by": r["manual_by"] if r["manual_src"] else None,
                "on_roster": (r["src"] or "sci") != "manual",
                "met": r["sightings"] or 0, "met_last": r["met_last"],
                "reports": self.db.execute(
                    "SELECT COUNT(*) FROM reports WHERE handle=?",
                    (r["handle"],)).fetchone()[0],
            })
        return out

    def fetch_roster(self, sid: str, client=None, max_pages: int | None = None,
                     on_page=None) -> dict:
        """Pull a flagged org's member list and record it.

        This is the step that makes flagging an org mean anything. The API's
        user endpoint reports a MAIN org only, so a player whose piracy org is
        an affiliate - the likely arrangement, and the whole reason the feature
        exists - looks unaffiliated from their profile alone. The roster is the
        only route to them.

        Only flagged orgs. Pulling a roster is one request per PAGE at one
        request a second - a 600-member org is around thirty of them - and
        doing that for an org nobody has flagged spends it on a question
        nobody asked. `on_page(page, so_far)` is passed straight through, for
        a caller that wants to say how it is going.

        Refuses rather than guesses when it cannot check: without an API key
        there is no roster, and recording an EMPTY one would look exactly like
        a genuinely empty org and overwrite whatever was there.
        """
        sid = (sid or "").strip().upper()
        if not sid:
            return {"ok": False, "error": "no org given"}
        flag = self.db.execute(
            "SELECT sid FROM org_flags WHERE sid=?", (sid,)).fetchone()
        if flag is None:
            return {"ok": False, "error": f"{sid} is not flagged"}

        import rsi as rsi_mod
        client = client or rsi_mod.Client()
        if not client.enriches:
            return {"ok": False, "sid": sid,
                    "error": "no lookup source is available, so no roster "
                             "can be fetched"}
        try:
            r = client.roster(sid, max_pages=max_pages or rsi_mod.MEMBER_PAGE_CAP,
                              on_page=on_page)
            members, complete = r.members, r.complete
        except Exception as exc:
            return {"ok": False, "sid": sid,
                    "error": f"roster fetch failed - {rsi_mod.describe_failure(exc)}"}

        # The docstring above promised this and the code did not do it: an
        # empty answer was recorded as an empty roster, which retired every
        # member and unflagged every one of them.
        if not members:
            return {"ok": False, "sid": sid,
                    "error": "the member list came back empty - keeping the "
                             "one already stored"}

        changes = self.record_roster(sid, members, complete=complete,
                                     source=r.source)
        reaches = next((o["reaches"] for o in self.flagged_orgs()
                        if o["sid"] == sid), 0)
        # What the ORG RECORD says it has, for a sanity check the pager itself
        # cannot do. The two numbers differ legitimately - RSI counts hidden
        # members and the roster omits them - but an order-of-magnitude gap is
        # how a truncated pull looks, and a truncated pull went unnoticed for
        # weeks. Cached, so this costs nothing most of the time, and a failure
        # to get it is not a failure to pull the roster.
        listed = r.total
        if not listed:
            try:
                listed = client.org(sid).members or None
            except Exception:
                pass
        return {"ok": True, "sid": sid, "members": changes["members"],
                "joined": changes["joined"], "left": changes["left"],
                "back": changes["back"], "reaches": reaches,
                "partial": not complete, "org_members": listed,
                "source": r.source, "switched": changes["switched"],
                "hidden": r.hidden, "redacted": r.redacted}

    # -- your own intel --------------------------------------------------

    def add_member_manually(self, sid: str, handle: str, note: str = "") -> dict:
        """Put somebody on a flagged org's roster by hand.

        For what the public roster does not show - and for some orgs that is
        most of the membership: people stay off it on purpose, so a pull finds
        a fraction of them. The org flag then reaches them exactly as it
        reaches a listed member, alerts included.

        EXACT handle, as typed. Nothing here matches it against similar
        spellings: resemblance is not evidence, and attaching an org to the
        wrong person is the worst thing this tool can do. Someone you have
        never met is recorded all the same, and recognised when you do.

        Sticky: `manual_ts` keeps them on the roster whatever a pull later says
        (record_roster() never retires them). Only remove_manual_member()
        takes them off.
        """
        sid, h = sid.strip().upper(), handle.strip().upper()
        if not self.db.execute("SELECT 1 FROM org_flags WHERE sid=?",
                               (sid,)).fetchone():
            return {"ok": False, "error": f"{sid} is not flagged"}
        note = (note or "").strip()[:200]
        now = time.time()
        row = self.db.execute(
            "SELECT gone_since, manual_ts, src FROM org_membership "
            "WHERE sid=? AND handle=?", (sid, h)).fetchone()
        if row is None:
            state = "added"
            self.db.execute(
                """INSERT INTO org_membership
                     (sid, handle, display, rank, stars, first_seen, last_seen,
                      gone_since, src, manual_ts, note)
                   VALUES (?,?,'','',0,?,?,NULL,'manual',?,?)""",
                (sid, h, now, now, now, note))
        else:
            state = ("updated" if row["manual_ts"] else
                     "back" if row["gone_since"] else "listed")
            # Adding someone yourself makes the entry yours, even if an org
            # mate's export put them there first - your note, your call.
            self.db.execute(
                """UPDATE org_membership SET gone_since=NULL,
                     manual_ts=COALESCE(manual_ts, ?), note=?, manual_src=NULL
                   WHERE sid=? AND handle=?""", (now, note, sid, h))
        # OR IGNORE: a profile row naming this org already carries the flag.
        self.db.execute(
            "INSERT OR IGNORE INTO player_orgs (handle, sid, is_main, source)"
            " VALUES (?,?,0,'roster')", (h, sid))
        self.db.commit()
        known = bool(self.db.execute(
            "SELECT 1 FROM players WHERE handle=?", (h,)).fetchone())
        return {"ok": True, "sid": sid, "handle": h, "state": state,
                "known": known}

    def remove_manual_member(self, sid: str, handle: str) -> dict:
        """Take back a membership you recorded. Only yours: a member the public
        roster lists stays a member, and just loses your note."""
        sid, h = sid.strip().upper(), handle.strip().upper()
        row = self.db.execute(
            "SELECT src, manual_ts FROM org_membership WHERE sid=? AND handle=?",
            (sid, h)).fetchone()
        if row is None or not row["manual_ts"]:
            return {"ok": False, "error": f"{h} was not added to {sid} by you"}
        if (row["src"] or "sci") == "manual":
            self.db.execute(
                "DELETE FROM org_membership WHERE sid=? AND handle=?", (sid, h))
            self.db.execute(
                "DELETE FROM player_orgs WHERE sid=? AND handle=? AND source='roster'",
                (sid, h))
            state = "removed"
        else:
            self.db.execute(
                "UPDATE org_membership SET manual_ts=NULL, note=NULL, "
                "manual_src=NULL WHERE sid=? AND handle=?", (sid, h))
            state = "still_listed"
        self.db.commit()
        return {"ok": True, "sid": sid, "handle": h, "state": state}

    def set_org_manual(self, sid: str, name: str = "", primary: str = "",
                       secondary: str = "") -> dict | None:
        """Your entry for an org a lookup could not describe. All blank
        clears it. Returns what is now stored, or None."""
        sid = sid.strip().upper()
        name, primary, secondary = ((v or "").strip() for v in (name, primary, secondary))
        if not (name or primary or secondary):
            self.db.execute("DELETE FROM org_manual WHERE sid=?", (sid,))
            self.db.commit()
            return None
        self.db.execute(
            """INSERT OR REPLACE INTO org_manual
                 (sid, name, primary_focus, secondary_focus, ts)
               VALUES (?,?,?,?,?)""", (sid, name, primary, secondary, time.time()))
        self.db.commit()
        return {"sid": sid, "name": name, "primary_focus": primary,
                "secondary_focus": secondary}

    def org_manual(self) -> dict[str, dict]:
        """Every org entry you have made, by SID. Small: one row per org you
        ever had to fill in, so the page reads the lot once per build."""
        return {r["sid"]: dict(r) for r in self.db.execute(
            "SELECT sid, name, primary_focus, secondary_focus, ts FROM org_manual")}

    def set_manual_enlisted(self, handle: str, iso: str = "") -> None:
        """Your entry for when an account was made; blank clears it."""
        h = handle.strip().upper()
        if iso:
            self.db.execute(
                "INSERT OR REPLACE INTO player_manual (handle, enlisted, ts)"
                " VALUES (?,?,?)", (h, iso, time.time()))
        else:
            self.db.execute("DELETE FROM player_manual WHERE handle=?", (h,))
        self.db.commit()

    def manual_enlisted(self, handles: list[str]) -> dict[str, str]:
        """handle -> your ISO enlistment date, for the handles asked about."""
        out = {}
        hs = [h.upper() for h in handles]
        for i in range(0, len(hs), 400):
            chunk = hs[i:i + 400]
            qs = ",".join("?" * len(chunk))
            for r in self.db.execute(
                    f"SELECT handle, enlisted FROM player_manual WHERE handle IN ({qs})",
                    chunk):
                if r["enlisted"]:
                    out[r["handle"]] = r["enlisted"]
        return out

    def flagged_orgs(self) -> list[dict]:
        """Every flagged org, with how far its flag actually reaches.

        `members` counts the roster rows this flag put in place. `reaches`
        counts the players IN YOUR DATABASE it touches, by either route, which
        is the number that says whether flagging it was worth anything.
        """
        out = []
        for r in self.db.execute(
                "SELECT sid, name, note, ts, roster_ts, roster_n"
                " FROM org_flags ORDER BY sid"):
            sid = r["sid"]
            # Current members by either route, plus FORMER members - they are
            # flagged too, so they count towards how far the flag reaches.
            reaches = self.db.execute(
                """SELECT COUNT(DISTINCT h) FROM (
                     SELECT p.handle AS h FROM player_orgs p WHERE p.sid=?
                     UNION
                     SELECT m.handle FROM org_membership m
                     WHERE m.sid=? AND m.gone_since IS NOT NULL)
                   WHERE h IN (SELECT handle FROM players)""",
                (sid, sid)).fetchone()[0]
            former = self.db.execute(
                "SELECT COUNT(*) FROM org_membership WHERE sid=? AND gone_since IS NOT NULL",
                (sid,)).fetchone()[0]
            # Hand-added members: yours, and those an org mate's export added
            # (manual_src). Counted apart - the page said "added by you" for
            # both.
            manual, shared = self.db.execute(
                "SELECT COALESCE(SUM(manual_src IS NULL), 0),"
                " COALESCE(SUM(manual_src IS NOT NULL), 0) FROM org_membership"
                " WHERE sid=? AND gone_since IS NULL AND manual_ts IS NOT NULL",
                (sid,)).fetchone()
            out.append({
                "sid": sid,
                "name": r["name"] or "",
                "note": r["note"] or "",
                "ts": r["ts"],
                "roster_ts": r["roster_ts"],
                "roster_n": r["roster_n"] or 0,
                "reaches": reaches,
                "former": former,
                "manual": manual,
                "manual_shared": shared,
            })
        return out

    def update_profile(self, handle: str, moniker: str = "", enlisted: str = ""):
        self.db.execute(
            """INSERT INTO players (handle, moniker, enlisted, first_seen, last_seen)
               VALUES (?,?,?,?,?)
               ON CONFLICT(handle) DO UPDATE SET
                 moniker=excluded.moniker, enlisted=excluded.enlisted""",
            (handle.upper(), moniker, enlisted, time.time(), time.time()))
        self.db.commit()

    # -- reads -----------------------------------------------------------

    def assess(self, handle: str, variants: bool | None = None) -> Assessment:
        # `variants` is only the watchlist's variant-matching switch, passed
        # through. None means "ask the settings file", which is right for one
        # assessment at a time - the watcher's, a handful per burst - and wrong
        # for a page of tiles: watchlist.variants_enabled() reads settings.json
        # every call, and a 120-tile page build was spending 46-80 ms of its
        # 115 ms re-reading the same file 120 times for an answer that cannot
        # change halfway down the page. A caller that has already loaded the
        # settings passes the answer it read.
        #
        # Read through the same resolution as writes, or a variant spelling
        # would report "not in database" for someone already recorded. But
        # mutate=False: this is a read, and the UI calls it for every tile on
        # every poll - renaming records as a side effect of displaying them
        # would be a nasty surprise.
        h, _ = self.canonical(handle, mutate=False)
        a = Assessment(handle=h)
        row = self.db.execute("SELECT * FROM players WHERE handle=?", (h,)).fetchone()
        if row:
            a.known = True
            a.sightings = row["sightings"] or 0
            a.first_seen = row["first_seen"]
            a.last_seen = row["last_seen"]
        for r in self.db.execute(
                "SELECT * FROM reports WHERE handle=? ORDER BY ts DESC", (h,)):
            a.reports.append(Report(ts=r["ts"], category=r["category"],
                                    id=r["id"],
                                    note=r["note"] or "", evidence=r["evidence"] or "",
                                    source=r["source"] or "self"))
        # The membership row rides along on the same query - one more per tile
        # on every page build would be the kind of growth test_perf counts.
        for r in self.db.execute(
                """SELECT f.sid, f.note, f.name, m.manual_ts, m.note AS mnote,
                          s.name AS mby
                   FROM org_flags f
                   JOIN player_orgs p ON p.sid = f.sid
                   LEFT JOIN org_membership m
                     ON m.sid = f.sid AND m.handle = p.handle
                   LEFT JOIN shares s ON s.install = m.manual_src
                   WHERE p.handle=?""", (h,)):
            a.flagged_orgs.append((r["sid"], r["note"] or ""))
            if r["name"]:
                a.org_names[r["sid"]] = r["name"]
            if r["manual_ts"]:
                a.manual_orgs[r["sid"]] = r["mnote"] or ""
                if r["mby"]:
                    a.manual_by[r["sid"]] = r["mby"]
        current = {sid for sid, _ in a.flagged_orgs}
        for r in self.db.execute(
                """SELECT m.sid, m.gone_since, f.name FROM org_membership m
                   JOIN org_flags f ON f.sid = m.sid
                   WHERE m.handle=? AND m.gone_since IS NOT NULL""", (h,)):
            if r["sid"] not in current:
                a.former_orgs.append((r["sid"], r["gone_since"]))
                if r["name"]:
                    a.org_names[r["sid"]] = r["name"]
        if self.watchlist is not None:
            # Looked up on the handle AS READ, not the canonical spelling. A
            # watchlist is an outside source that never saw your merges, so
            # asking it about your internal name for someone would miss the
            # spelling it actually recorded.
            try:
                a.watchlist = self.watchlist.lookup(handle, variants)
            except Exception:
                # A corrupt or missing watchlist must never take down an
                # assessment. Your own records are the thing that matters.
                a.watchlist = []
        return a

    def stats(self) -> dict:
        q = lambda s: self.db.execute(s).fetchone()[0]
        return {
            "players": q("SELECT COUNT(*) FROM players"),
            "sightings": q("SELECT COUNT(*) FROM sightings"),
            "reports": q("SELECT COUNT(*) FROM reports"),
            "flagged_orgs": q("SELECT COUNT(*) FROM org_flags"),
            # self.root, not DB_PATH: stats() belongs to the database
            # this instance opened. Reading the module global reported
            # the default file's size for any other database.
            "bytes": (path.stat().st_size if (path := self.root /
                      "sc-watch.db").exists() else 0),
        }

    def recent(self, limit=20):
        return self.db.execute(
            """SELECT handle, sightings, last_seen FROM players
               ORDER BY last_seen DESC LIMIT ?""", (limit,)).fetchall()


# --------------------------------------------------------------------------
# CLI
# --------------------------------------------------------------------------


def main() -> int:
    import argparse
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("report", help="record something about a player")
    p.add_argument("handle")
    p.add_argument("category", choices=CATEGORIES)
    p.add_argument("note", nargs="?", default="")
    p.add_argument("--evidence", default="", help="screenshot path, clip timestamp...")

    p = sub.add_parser("flag-org", help="flag an org; members inherit it")
    p.add_argument("sid")
    p.add_argument("note", nargs="?", default="")
    p.add_argument("--no-roster", action="store_true",
                   help="do not pull the member list. The flag then only "
                        "reaches players whose PROFILE names this org, which "
                        "for an affiliate org is nobody.")

    p = sub.add_parser("unflag-org")
    p.add_argument("sid")

    p = sub.add_parser("rosters",
                       help="show flagged orgs, or refresh their member lists")
    p.add_argument("sid", nargs="?", default="",
                   help="one org, or omit for all of them")
    p.add_argument("--refresh", action="store_true",
                   help="re-pull member lists. Up to 20 requests per org at "
                        "one per second.")

    p = sub.add_parser("show", help="everything known about a handle")
    p.add_argument("handle")

    sub.add_parser("consolidate", help="merge variant spellings already stored")
    sub.add_parser("stats")
    p = sub.add_parser("recent")
    p.add_argument("-n", type=int, default=20)

    p = sub.add_parser("import", help="load an existing sightings.csv")
    p.add_argument("csv", nargs="?", default="sightings.csv")

    p = sub.add_parser("ghosts",
                       help="list handles the RSI API says do not exist")
    p.add_argument("--apply", action="store_true",
                   help="actually delete them (dry run without this)")

    args = ap.parse_args()

    # Identity questions need evidence, so give the database a way to check
    # whether a handle is a real account before it merges anything.
    verify = None
    try:
        import rsi
        client = rsi.Client()
        if client.enriches:
            verify = client.exists
    except Exception:
        pass
    db = Database(verify=verify)
    if verify is None and args.cmd == "consolidate":
        print("! no profile backend - cannot verify identities, so nothing "
              "will be merged.\n  Set SC_API_KEY to enable consolidation.\n")

    if args.cmd == "report":
        db.add_report(args.handle, args.category, args.note, args.evidence)
        print(f"recorded {args.category} against {args.handle.upper()}")
    elif args.cmd == "flag-org":
        db.flag_org(args.sid, args.note)
        print(f"flagged org {args.sid.upper()}")
        # The flag on its own reaches only players whose profile names this
        # org, and the profile endpoint gives a main org only. Pulling the
        # roster is what reaches the affiliates, so it happens by default
        # rather than waiting to be remembered.
        if args.no_roster:
            print("  no roster pulled - run 'rosters --refresh' to fix that")
        else:
            print("  pulling the member list, one request a second...")
            r = db.fetch_roster(args.sid)
            if r.get("ok"):
                print(f"  {r['members']} member(s) recorded; the flag now "
                      f"reaches {r['reaches']} player(s) you have seen"
                      + (f" (RSI lists {r['org_members']})"
                         if r.get("org_members") else ""))
                if r.get("partial"):
                    print("  ! the list stopped early, so nobody was marked "
                          "as having left")
            else:
                print(f"  {r.get('error')}")
    elif args.cmd == "rosters":
        targets = [o for o in db.flagged_orgs()
                   if not args.sid or o["sid"] == args.sid.upper()]
        if not targets:
            print("no flagged orgs" if not args.sid
                  else f"{args.sid.upper()} is not flagged")
        for o in targets:
            if args.refresh:
                r = db.fetch_roster(o["sid"])
                if not r.get("ok"):
                    print(f"{o['sid']:<14} {r.get('error')}")
                    continue
                o = next(x for x in db.flagged_orgs() if x["sid"] == o["sid"])
            age = 0.0 if not o["roster_ts"] else time.time() - o["roster_ts"]
            when = ("never" if not o["roster_ts"] else
                    f"{age/60:.0f}m ago" if age < 3600 else
                    f"{age/3600:.0f}h ago" if age < 86400 else
                    f"{age/86400:.0f}d ago")
            print(f"{o['sid']:<14} {o['roster_n']:>5} members  "
                  f"pulled {when:<10} reaches {o['reaches']} player(s)"
                  + (f"   {o['note']}" if o["note"] else ""))
            if not o["roster_ts"]:
                print("               this flag currently reaches only players "
                      "whose profile names it")
    elif args.cmd == "unflag-org":
        db.unflag_org(args.sid)
        print(f"unflagged {args.sid.upper()}")
    elif args.cmd == "show":
        a = db.assess(args.handle)
        print(f"\n{a.handle}" + ("" if a.known else "  (not in database)"))
        if a.known:
            print(f"  score {a.score:+.2f}   alert={a.alert}")
            for line in a.lines():
                print(f"  - {line}")
            for r in a.reports:
                print(f"    [{r.category}] {int(r.age_days)}d ago  {r.note}"
                      + (f"  ({r.evidence})" if r.evidence else ""))
        print()
    elif args.cmd == "consolidate":
        merged, skipped = db.consolidate()
        for old, new in merged:
            print(f"  merged   {old}  ->  {new}")
        for old, new in skipped:
            print(f"  kept     {old}  /  {new}   (both are real accounts)")
        if not merged and not skipped:
            print("nothing to merge")
        print(f"\n{len(merged)} merged, {len(skipped)} left separate. "
              f"{db.stats()['players']} players.")
    elif args.cmd == "stats":
        for k, v in db.stats().items():
            print(f"  {k:<14} {v}")
    elif args.cmd == "recent":
        for r in db.recent(args.n):
            ago = (time.time() - r["last_seen"]) / 3600
            print(f"  {r['handle']:<24} {r['sightings']:>3}x   {ago:.1f}h ago")
    elif args.cmd == "ghosts":
        res = db.purge_ghosts(apply=args.apply)
        verb = "deleted" if args.apply else "would delete"
        print(f"{verb} {len(res['ghosts'])} handle(s) with no RSI account:")
        for g in res["ghosts"]:
            print(f"   {g['handle']:<24} seen {g['sightings']}x")
        if res["kept"]:
            print(f"\nheld back {len(res['kept'])} (evidence would be lost):")
            for g in res["kept"]:
                print(f"   {g['handle']:<24} {g['why']}")
        if res["unchecked"]:
            print(f"\n{len(res['unchecked'])} unverifiable (no API key) - "
                  f"left alone")
        if res["errors"]:
            print(f"\n{len(res['errors'])} lookup error(s) - left alone:")
            for h, e in res["errors"]:
                print(f"   {h:<24} {e}")
        if not args.apply and res["ghosts"]:
            print("\n(dry run - pass --apply to delete)")
    elif args.cmd == "import":
        import csv as _csv
        path = Path(args.csv)
        if not path.exists():
            print(f"no such file: {path}")
            return 1
        n = 0
        with path.open(encoding="utf-8-sig") as f:
            for row in _csv.DictReader(f):
                db.record_sighting(row["name"], float(row.get("range_km") or 0),
                                   int(row.get("votes") or 0),
                                   int(row.get("total") or 0))
                n += 1
        print(f"imported {n} sighting(s) from {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
