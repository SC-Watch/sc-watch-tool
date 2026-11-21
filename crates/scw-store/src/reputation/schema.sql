
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
