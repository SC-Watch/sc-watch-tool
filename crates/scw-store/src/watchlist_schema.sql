
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
