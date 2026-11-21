//! sc-watch.db - who you've seen, and what you know about them
//! (reputation.py). It stores EVIDENCE, not verdicts: every report carries a
//! category, a note, a timestamp and optionally a pointer to proof, so an
//! alert can say what happened and when.
//!
//! This part: the schema - reputation.py's own text, so a file either
//! version creates is the same file - and the migrations that bring an
//! older database up to it, in the Python's order (ALTER TABLE appends to
//! the stored CREATE text, so the order is part of the result).

use rusqlite::{Connection, params};

use crate::db::columns;

/// reputation.SCHEMA, extracted from the Python verbatim.
pub const SCHEMA: &str = include_str!("reputation_schema.sql");

/// Reports lose half their weight over this period.
pub const HALF_LIFE_DAYS: f64 = 180.0;

/// A report flagged in the moment, its reason not given yet.
pub const PENDING: &str = "pending";

/// Characters reading confuses, collapsed to one representative each, so two
/// readings of one handle share a sketch ('5LIVER' and 'SLIVER' both become
/// 'SIIUER' - reputation.py's comment says '51IVER', its code does not) and a
/// confusion variant is an indexed lookup.
const SKETCH_GROUPS: [&str; 9] = ["OQD0", "IL1_", "S5", "B8", "G6", "Z2", "UV", "CG", "TY7"];

/// The LAST group holding a character decides, as in the Python's dict
/// comprehension: 'G' is in both "G6" and "CG", and sketches to 'C'.
pub fn sketch(handle: &str) -> String {
    handle
        .to_uppercase()
        .chars()
        .map(|c| {
            SKETCH_GROUPS
                .iter()
                .rev()
                .find(|g| g.contains(c))
                .and_then(|g| g.chars().next())
                .unwrap_or(c)
        })
        .collect()
}

/// Brings a database up to the current schema (reputation.Database._migrate),
/// creating it when empty. Running it again changes nothing.
pub fn migrate(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(SCHEMA)?;
    let has = |table: &str, col: &str| -> rusqlite::Result<bool> {
        Ok(columns(db, table)?.iter().any(|c| c == col))
    };
    let tx = db.unchecked_transaction()?;
    if !has("player_orgs", "source")? {
        tx.execute_batch("ALTER TABLE player_orgs ADD COLUMN source TEXT DEFAULT 'profile'")?;
    }
    if !has("players", "sketch")? {
        tx.execute_batch("ALTER TABLE players ADD COLUMN sketch TEXT")?;
    }
    // An audit's `handle` is the canonical spelling; `read_as` what was read.
    if !has("audits", "read_as")? {
        tx.execute_batch("ALTER TABLE audits ADD COLUMN read_as TEXT")?;
        tx.execute_batch("UPDATE audits SET read_as=handle WHERE read_as IS NULL")?;
    }
    // Why an audit exists; every older one was raised by the vote rule.
    if !has("audits", "reason")? {
        tx.execute_batch("ALTER TABLE audits ADD COLUMN reason TEXT DEFAULT 'weak_read'")?;
        tx.execute_batch("UPDATE audits SET reason='weak_read' WHERE reason IS NULL")?;
    }
    if !has("org_flags", "roster_ts")? {
        tx.execute_batch("ALTER TABLE org_flags ADD COLUMN roster_ts REAL")?;
    }
    if !has("org_flags", "roster_n")? {
        tx.execute_batch("ALTER TABLE org_flags ADD COLUMN roster_n INTEGER DEFAULT 0")?;
    }
    // Which service produced a roster (NULL: starcitizen-api.com).
    if !has("org_flags", "roster_src")? {
        tx.execute_batch("ALTER TABLE org_flags ADD COLUMN roster_src TEXT")?;
    }
    if !has("org_membership", "src")? {
        tx.execute_batch("ALTER TABLE org_membership ADD COLUMN src TEXT")?;
    }
    // Members YOU put on a roster, and what you know about them.
    if !has("org_membership", "manual_ts")? {
        tx.execute_batch("ALTER TABLE org_membership ADD COLUMN manual_ts REAL")?;
    }
    if !has("org_membership", "note")? {
        tx.execute_batch("ALTER TABLE org_membership ADD COLUMN note TEXT")?;
    }
    // Whose export brought a hand-added member (NULL: you).
    if !has("org_membership", "manual_src")? {
        tx.execute_batch("ALTER TABLE org_membership ADD COLUMN manual_src TEXT")?;
    }
    // An imported report remembers which of THEIR reports it was.
    if !has("reports", "origin")? {
        tx.execute_batch("ALTER TABLE reports ADD COLUMN origin TEXT")?;
    }
    tx.execute_batch("CREATE INDEX IF NOT EXISTS idx_reports_origin ON reports(origin)")?;
    // Rosters pulled before the history existed become its first entries,
    // dated to when they were pulled.
    let empty: bool = tx
        .query_row("SELECT 1 FROM org_membership LIMIT 1", [], |_| Ok(()))
        .is_err();
    if empty {
        tx.execute_batch(
            "INSERT OR IGNORE INTO org_membership
                   (sid, handle, first_seen, last_seen)
                   SELECT p.sid, p.handle, COALESCE(f.roster_ts, f.ts),
                          COALESCE(f.roster_ts, f.ts)
                   FROM player_orgs p JOIN org_flags f ON f.sid = p.sid
                   WHERE p.source = 'roster'",
        )?;
    }
    tx.execute_batch("CREATE INDEX IF NOT EXISTS idx_players_sketch ON players(sketch)")?;
    let missing: Vec<String> = {
        let mut stmt =
            tx.prepare("SELECT handle FROM players WHERE sketch IS NULL OR sketch=''")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for h in missing {
        tx.execute(
            "UPDATE players SET sketch=? WHERE handle=?",
            params![sketch(&h), h],
        )?;
    }
    tx.commit()
}
