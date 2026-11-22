//! The reputation database's handle and its identity rules
//! (reputation.Database: canonical, the lookalike verdict, rename,
//! consolidate), plus the helpers the other parts share.
//!
//! IDENTITY NEEDS EVIDENCE, NOT RESEMBLANCE. Two spellings that look like a
//! misread of each other are routinely both real accounts, and attributing
//! one person's history to another is the worst thing this tool can do. So
//! a merge happens only when a verifier ([`Verify`], the RSI client in the
//! app) says one side is NOT a real account; with no verifier, nothing is
//! merged.

use std::path::{Path, PathBuf};

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension, Params, params};
use serde_json::{Map, Value};

use super::{migrate, sketch};
use crate::flags::Flags;
pub use crate::watchlist::Hit;

/// "Is this a real account?" asked strictly: Some(true), Some(false), or
/// None for "cannot tell"; Err when the asking itself failed.
pub trait Verify {
    fn exists(&self, handle: &str) -> Result<Option<bool>, String>;
}

/// A watchlist that can be asked about a handle.
pub trait WatchlistLookup {
    fn lookup(&self, handle: &str, variants: Option<bool>) -> Result<Vec<Hit>, String>;
}

/// sc-watch.db, open.
pub struct Database {
    pub conn: Connection,
    /// The folder holding the database: audit images are relative to it.
    pub root: PathBuf,
    pub verify: Option<Box<dyn Verify>>,
    pub watchlist: Option<Box<dyn WatchlistLookup>>,
    /// The report flags (flags.json), for scoring and wording.
    pub flags: Flags,
    /// Seconds since the epoch, as time.time() - injectable, so tests run
    /// at fixed times.
    pub clock: Box<dyn Fn() -> f64>,
    /// Dates in wording in UTC instead of local time (for tests).
    pub utc_dates: bool,
}

/// What two lookalike spellings are, by the evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Both real, or either cannot be checked: two people as far as anyone
    /// can tell.
    Separate,
    /// The first is real and the second is not: the second misreads it.
    A,
    /// The second is real and the first is not.
    B,
    /// Neither is a real account that could be shown.
    Misreads,
}

pub(crate) fn json_value(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::from(i),
        ValueRef::Real(f) => serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number),
        ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::String(b.iter().map(|x| format!("{x:02x}")).collect()),
    }
}

pub(crate) fn system_time() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

impl Database {
    /// Opens (creating if need be) and migrates a database. Default flags,
    /// the system clock, no verifier, no watchlist.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = crate::db::open(path)?;
        migrate(&conn)?;
        Ok(Self {
            conn,
            root: path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from(".")),
            verify: None,
            watchlist: None,
            flags: Flags::defaults(),
            clock: Box::new(system_time),
            utc_dates: false,
        })
    }

    pub fn now(&self) -> f64 {
        (self.clock)()
    }

    // ---- small query helpers ------------------------------------------------

    /// Every row of a query as {column: value}, in the query's column order.
    pub(crate) fn maps<P: Params>(
        &self,
        sql: &str,
        p: P,
    ) -> rusqlite::Result<Vec<Map<String, Value>>> {
        let mut stmt = self.conn.prepare(sql)?;
        let names: Vec<String> = stmt.column_names().map(|s| s.to_string()).collect();
        let rows = stmt.query_map(p, |r| {
            let mut m = Map::new();
            for (i, n) in names.iter().enumerate() {
                m.insert(n.clone(), json_value(r.get_ref(i)?));
            }
            Ok(m)
        })?;
        rows.collect()
    }

    pub(crate) fn strings<P: Params>(&self, sql: &str, p: P) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(p, |r| r.get::<_, String>(0))?;
        rows.collect()
    }

    /// The first column of every row of a query (for tools and tests).
    pub fn strings_pub(&self, sql: &str) -> Vec<String> {
        self.strings(sql, []).unwrap_or_default()
    }

    pub(crate) fn exists<P: Params>(&self, sql: &str, p: P) -> rusqlite::Result<bool> {
        Ok(self
            .conn
            .query_row(sql, p, |_| Ok(()))
            .optional()?
            .is_some())
    }

    pub(crate) fn count<P: Params>(&self, sql: &str, p: P) -> rusqlite::Result<i64> {
        self.conn.query_row(sql, p, |r| r.get(0))
    }

    // ---- identity -------------------------------------------------------------

    /// The handle a reading should be stored under, and the spelling it was
    /// renamed from when resolving it moved a record (reputation.canonical).
    pub fn canonical(
        &self,
        handle: &str,
        mutate: bool,
    ) -> rusqlite::Result<(String, Option<String>)> {
        let h = handle.to_uppercase();
        if self.exists("SELECT 1 FROM players WHERE handle=?", [&h])? {
            return Ok((h, None));
        }
        if self.verify.is_none() {
            return Ok((h, None)); // no evidence available - never guess at identity
        }
        let sk = sketch(&h);
        let same_sketch: Option<String> = self
            .conn
            .query_row(
                "SELECT handle FROM players WHERE sketch=? LIMIT 1",
                [&sk],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(known) = same_sketch {
            return self.settle_lookalike(&h, &known, mutate);
        }
        // Truncations differ in length, so the sketch won't match: a stem
        // first (indexed), then a length-bounded scan (truncated at the
        // START, a stem cannot find it).
        let stem: String = sk.chars().take(4).collect();
        let mut candidates = Vec::new();
        if stem.chars().count() >= 3 {
            candidates = self.strings(
                "SELECT handle FROM players WHERE sketch LIKE ?",
                [format!("{stem}%")],
            )?;
        }
        if candidates.is_empty() {
            let n = h.chars().count() as i64;
            candidates = self.strings(
                "SELECT handle FROM players
                   WHERE LENGTH(handle) BETWEEN ? AND ? LIMIT 400",
                params![n - 3, n + 3],
            )?;
        }
        for known in candidates {
            if scw_vision::names::vote::same_contact(&h, &known) {
                return self.settle_lookalike(&h, &known, mutate);
            }
        }
        Ok((h, None))
    }

    /// True / False / None ("cannot tell") - strictly, never guessing.
    fn realness(&self, handle: &str) -> Option<bool> {
        self.verify.as_ref()?.exists(handle).ok().flatten()
    }

    pub fn lookalike_verdict(&self, a: &str, b: &str) -> Verdict {
        let (ra, rb) = (self.realness(a), self.realness(b));
        if ra != Some(false) && rb != Some(false) {
            return Verdict::Separate;
        }
        if ra == Some(true) && rb == Some(false) {
            return Verdict::A;
        }
        if rb == Some(true) && ra == Some(false) {
            return Verdict::B;
        }
        Verdict::Misreads
    }

    /// canonical() for a new reading `h` that reads like `known`: the real
    /// account wins whichever came first, and a misread's audits go with it.
    fn settle_lookalike(
        &self,
        h: &str,
        known: &str,
        mutate: bool,
    ) -> rusqlite::Result<(String, Option<String>)> {
        match self.lookalike_verdict(h, known) {
            Verdict::Separate => Ok((h.to_string(), None)),
            Verdict::A => {
                if !mutate {
                    return Ok((h.to_string(), None));
                }
                self.rename(known, h)?;
                self.close_answered_audits(known)?;
                Ok((h.to_string(), Some(known.to_string())))
            }
            Verdict::Misreads
                if h.chars().count() > known.chars().count() && h.starts_with(known) =>
            {
                if !mutate {
                    return Ok((h.to_string(), None));
                }
                self.rename(known, h)?;
                Ok((h.to_string(), Some(known.to_string())))
            }
            _ => Ok((known.to_string(), None)),
        }
    }

    /// Drops the audits of a spelling just folded into the real account it
    /// misread: the question they asked has its answer.
    pub(crate) fn close_answered_audits(&self, misread: &str) -> rusqlite::Result<usize> {
        let rows = self.maps("SELECT * FROM audits WHERE handle=?", [misread])?;
        for row in &rows {
            self.drop_audit_files(row);
        }
        if !rows.is_empty() {
            self.conn
                .execute("DELETE FROM audits WHERE handle=?", [misread])?;
        }
        Ok(rows.len())
    }

    /// (both are real accounts, the question could actually be asked).
    pub(crate) fn both_accounts_exist(&self, a: &str, b: &str) -> (bool, bool) {
        let Some(v) = self.verify.as_ref() else {
            return (false, false);
        };
        let (Ok(ra), Ok(rb)) = (v.exists(a), v.exists(b)) else {
            return (false, false);
        };
        if ra == Some(false) || rb == Some(false) {
            return (false, true);
        }
        if ra.is_none() || rb.is_none() {
            return (false, false);
        }
        (true, true)
    }

    /// Folds `old`'s history into `new` - a rename when `new` has no record,
    /// a merge when it has. True when anything moved.
    pub fn rename(&self, old: &str, new: &str) -> rusqlite::Result<bool> {
        type Row = (Option<i64>, Option<f64>);
        let get = |h: &str| -> rusqlite::Result<Option<Row>> {
            self.conn
                .query_row(
                    "SELECT sightings, first_seen FROM players WHERE handle=?",
                    [h],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
        };
        let target = get(new)?;
        let Some(src) = get(old)? else {
            return Ok(false);
        };
        let tx = self.conn.unchecked_transaction()?;
        match target {
            None => {
                tx.execute(
                    "UPDATE players SET handle=?, sketch=? WHERE handle=?",
                    params![new, sketch(new), old],
                )?;
            }
            Some(t) => {
                tx.execute(
                    "UPDATE players SET
                     sightings = sightings + ?,
                     first_seen = MIN(first_seen, ?)
                   WHERE handle=?",
                    params![src.0.unwrap_or(0), src.1.or(t.1), new],
                )?;
                tx.execute("DELETE FROM players WHERE handle=?", [old])?;
            }
        }
        for table in ["sightings", "reports", "player_orgs", "player_manual"] {
            tx.execute(
                &format!("UPDATE OR IGNORE {table} SET handle=? WHERE handle=?"),
                params![new, old],
            )?;
            tx.execute(&format!("DELETE FROM {table} WHERE handle=?"), [old])?;
        }
        tx.commit()?;
        Ok(true)
    }

    /// Merges variant spellings already in the database, longest first so
    /// the fuller spelling wins - unless the shorter is the real account and
    /// the longer is not. Returns (merged, kept apart) as (from, into) pairs.
    #[allow(clippy::type_complexity)]
    pub fn consolidate(&self) -> rusqlite::Result<(Vec<(String, String)>, Vec<(String, String)>)> {
        let handles = self.strings(
            "SELECT handle FROM players ORDER BY LENGTH(handle) DESC",
            [],
        )?;
        let (mut merged, mut skipped) = (Vec::new(), Vec::new());
        let mut gone: std::collections::HashSet<String> = Default::default();
        for keep in &handles {
            if gone.contains(keep) {
                continue;
            }
            for other in &handles {
                if other == keep || gone.contains(other) {
                    continue;
                }
                if !scw_vision::names::vote::same_contact(keep, other) {
                    continue;
                }
                let verdict = self.lookalike_verdict(keep, other);
                if verdict == Verdict::Separate {
                    skipped.push((other.clone(), keep.clone()));
                    continue;
                }
                if verdict == Verdict::B {
                    self.rename(keep, other)?;
                    self.close_answered_audits(keep)?;
                    merged.push((keep.clone(), other.clone()));
                    gone.insert(keep.clone());
                    break;
                }
                self.rename(other, keep)?;
                if verdict == Verdict::A {
                    self.close_answered_audits(other)?;
                }
                merged.push((other.clone(), keep.clone()));
                gone.insert(other.clone());
            }
        }
        Ok((merged, skipped))
    }

    /// Deletes the pictures behind one audit row - only inside audit/, only
    /// files: the paths come out of the database, and this deletes.
    pub(crate) fn drop_audit_files(&self, row: &Map<String, Value>) {
        let Ok(allowed) = std::fs::canonicalize(self.root.join("audit")) else {
            return;
        };
        for key in ["frame_path", "crop_name", "crop_range"] {
            let Some(rel) = row
                .get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            if let Ok(f) = std::fs::canonicalize(self.root.join(rel))
                && f.parent() == Some(allowed.as_path())
                && f.is_file()
            {
                let _ = std::fs::remove_file(&f);
            }
        }
    }
}
