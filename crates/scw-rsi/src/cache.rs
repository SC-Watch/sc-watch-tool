//! rsi_cache.db - `rsi.Cache`: profiles, orgs, the rosters' member lists,
//! failed lookups, profiles missing an org's focus, and the pauses every
//! process must see.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use crate::clock::Clock;
use crate::error::LookupError;
use crate::model::{Citizen, Org};
use crate::{CITIZEN_TTL, MISS_TTL, ORG_TTL};
use scw_store::json::{dumps_compact, loads, py_str, truthy};

/// rsi.py's tables, verbatim.
const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS citizens (
            handle TEXT PRIMARY KEY, data TEXT, fetched_at REAL)",
    "CREATE TABLE IF NOT EXISTS orgs (
            sid TEXT PRIMARY KEY, data TEXT, fetched_at REAL)",
    "CREATE TABLE IF NOT EXISTS org_members (
            sid TEXT NOT NULL, handle TEXT NOT NULL, fetched_at REAL,
            PRIMARY KEY (sid, handle))",
    "CREATE TABLE IF NOT EXISTS lookup_failures (
            handle TEXT PRIMARY KEY, ts REAL, error TEXT,
            attempts INTEGER, gave_up INTEGER)",
    "CREATE TABLE IF NOT EXISTS partial_citizens (
            handle TEXT PRIMARY KEY, data TEXT, observed_at REAL,
            reason TEXT, unknown TEXT)",
    "CREATE TABLE IF NOT EXISTS pauses (
            name TEXT PRIMARY KEY, until REAL)",
];

/// A profile kept apart because an org on it has no known focus.
#[derive(Clone, Debug, PartialEq)]
pub struct Partial {
    pub handle: String,
    pub data: Value,
    pub observed_at: f64,
    pub unknown: Vec<String>,
    pub reason: String,
}

pub struct Cache {
    db: Mutex<Connection>,
    path: PathBuf,
    clock: Arc<dyn Clock>,
}

fn to_json(v: &impl serde::Serialize) -> String {
    dumps_compact(&serde_json::to_value(v).unwrap_or(Value::Null)).unwrap_or_default()
}

/// `when = min(now, stored_at) if stored_at else now` - zero counts as none.
fn when(now: f64, stored_at: Option<f64>) -> f64 {
    match stored_at {
        Some(s) if s != 0.0 => now.min(s),
        _ => now,
    }
}

fn type_error(e: impl std::fmt::Display) -> LookupError {
    LookupError::other("TypeError", e.to_string())
}

impl Cache {
    /// Opens (creating) the cache and drops stored placeholder profiles -
    /// rsi.py does that once per process per file; the port opens a cache
    /// once per process.
    pub fn open(path: &Path, clock: Arc<dyn Clock>) -> rusqlite::Result<Self> {
        let db = scw_store::db::open(path)?;
        for s in SCHEMA {
            db.execute(s, [])?;
        }
        let cache = Cache {
            db: Mutex::new(db),
            path: path.to_path_buf(),
            clock,
        };
        let _ = cache.drop_placeholder_profiles();
        Ok(cache)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Books the next turn to send, `interval` after the last one booked by
    /// any process; returns when (epoch seconds).
    pub fn book_slot(&self, name: &str, interval: f64) -> rusqlite::Result<f64> {
        let now = self.clock.time();
        let got: Option<f64> = self
            .db()
            .query_row(
                "INSERT INTO pauses (name, until) VALUES (?, ?)
               ON CONFLICT(name) DO UPDATE SET
                   until = MAX(excluded.until, MIN(until, excluded.until + 60) + ?)
               RETURNING until",
                params![name, now, interval],
                |r| r.get(0),
            )
            .optional()?;
        Ok(got.unwrap_or(now))
    }

    /// Deletes stored profiles no RSI read could have produced: said to
    /// exist, with no enlistment date, no record and no org. Returns how many.
    pub fn drop_placeholder_profiles(&self) -> rusqlite::Result<usize> {
        let db = self.db();
        let rows: Vec<(String, Option<String>)> = db
            .prepare("SELECT handle, data FROM citizens")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let mut gone = Vec::new();
        for (h, data) in rows {
            let Some(Value::Object(d)) = data.as_deref().and_then(|t| loads(t).ok()) else {
                continue;
            };
            let exists = d.get("exists").is_none_or(|v| truthy(Some(v)));
            if exists
                && !truthy(d.get("enlisted"))
                && !truthy(d.get("record"))
                && !truthy(d.get("orgs"))
            {
                gone.push(h);
            }
        }
        for h in &gone {
            db.execute("DELETE FROM citizens WHERE handle=?", [h])?;
        }
        Ok(gone.len())
    }

    /// Epoch seconds a stored pause runs to; 0 if there is none.
    pub fn pause_until(&self, name: &str) -> rusqlite::Result<f64> {
        let v: Option<Option<f64>> = self
            .db()
            .query_row("SELECT until FROM pauses WHERE name=?", [name], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(v.flatten().filter(|u| *u != 0.0).unwrap_or(0.0))
    }

    pub fn set_pause(&self, name: &str, until: f64) -> rusqlite::Result<()> {
        self.db().execute(
            "INSERT OR REPLACE INTO pauses VALUES (?,?)",
            params![name, until],
        )?;
        Ok(())
    }

    /// The stored profile, or None if there is none or it has expired;
    /// `any_age` returns it however old. Its `note.stored_at` says when it
    /// was read.
    pub fn get_citizen(&self, handle: &str, any_age: bool) -> Result<Option<Citizen>, LookupError> {
        let row: Option<(Option<String>, Option<f64>)> = self
            .db()
            .query_row(
                "SELECT data, fetched_at FROM citizens WHERE handle=?",
                [handle.to_uppercase()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((data, at)) = row else {
            return Ok(None);
        };
        let payload = loads(data.as_deref().unwrap_or("null"))
            .map_err(|e| LookupError::other("JSONDecodeError", e))?;
        let exists = payload.get("exists").is_none_or(|v| truthy(Some(v)));
        let ttl = if exists { CITIZEN_TTL } else { MISS_TTL };
        let at = at.ok_or_else(|| type_error("fetched_at is NULL"))?;
        if !any_age && self.clock.time() - at > ttl {
            return Ok(None);
        }
        let mut c: Citizen = serde_json::from_value(payload).map_err(type_error)?;
        c.note.stored_at = Some(at);
        Ok(Some(c))
    }

    /// Stores a profile. `stored_at` is when RSI was actually read, if that
    /// is known and earlier than now. Clears any failure and partial copy.
    pub fn put_citizen(&self, c: &Citizen, stored_at: Option<f64>) -> rusqlite::Result<()> {
        let at = when(self.clock.time(), stored_at);
        let h = c.handle.to_uppercase();
        let db = self.db();
        db.execute(
            "INSERT OR REPLACE INTO citizens VALUES (?,?,?)",
            params![h, to_json(c), at],
        )?;
        db.execute("DELETE FROM lookup_failures WHERE handle=?", [&h])?;
        db.execute("DELETE FROM partial_citizens WHERE handle=?", [&h])?;
        Ok(())
    }

    /// Keeps a profile missing an org's focus apart; clears any failure.
    pub fn put_partial(
        &self,
        c: &Citizen,
        reason: &str,
        unknown: &[String],
    ) -> rusqlite::Result<()> {
        let now = self.clock.time();
        let observed = if c.fetched_at != 0.0 {
            c.fetched_at
        } else {
            now
        }
        .min(now);
        let h = c.handle.to_uppercase();
        let db = self.db();
        db.execute(
            "INSERT OR REPLACE INTO partial_citizens
               (handle, data, observed_at, reason, unknown) VALUES (?,?,?,?,?)",
            params![h, to_json(c), observed, reason, to_json(&unknown)],
        )?;
        db.execute("DELETE FROM lookup_failures WHERE handle=?", [&h])?;
        Ok(())
    }

    /// Every partial profile; a damaged row is skipped.
    pub fn partials(&self) -> rusqlite::Result<Vec<Partial>> {
        let db = self.db();
        let mut stmt =
            db.prepare("SELECT handle, data, observed_at, unknown, reason FROM partial_citizens")?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(r) = rows.next()? {
            let handle: String = r.get(0)?;
            let Ok(data) = loads(&r.get::<_, Option<String>>(1)?.unwrap_or_default()) else {
                continue;
            };
            let observed = match r.get_ref(2)? {
                ValueRef::Null => 0.0,
                ValueRef::Integer(i) => i as f64,
                ValueRef::Real(f) => f,
                ValueRef::Text(t) => match std::str::from_utf8(t)
                    .ok()
                    .and_then(|t| scw_store::json::py_strip(t).parse::<f64>().ok())
                {
                    Some(f) => f,
                    None => continue,
                },
                ValueRef::Blob(_) => continue,
            };
            let unknown = r.get::<_, Option<String>>(3)?.filter(|u| !u.is_empty());
            let Ok(Value::Array(list)) = loads(unknown.as_deref().unwrap_or("[]")) else {
                continue;
            };
            let reason: Option<String> = r.get(4)?;
            out.push(Partial {
                handle,
                data,
                observed_at: observed,
                unknown: list.iter().map(|x| py_str(x).to_uppercase()).collect(),
                reason: reason.unwrap_or_default(),
            });
        }
        Ok(out)
    }

    /// When the whole profile on file was read, whatever its age.
    pub fn citizen_stored_at(&self, handle: &str) -> rusqlite::Result<Option<f64>> {
        Ok(self
            .db()
            .query_row(
                "SELECT fetched_at FROM citizens WHERE handle=?",
                [handle.to_uppercase()],
                |r| r.get::<_, Option<f64>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Rewrites a partial profile in place, keeping when it was read.
    pub fn update_partial(
        &self,
        c: &Citizen,
        reason: &str,
        unknown: &[String],
    ) -> rusqlite::Result<()> {
        self.db().execute(
            "UPDATE partial_citizens SET data=?, reason=?, unknown=? WHERE handle=?",
            params![
                to_json(c),
                reason,
                to_json(&unknown),
                c.handle.to_uppercase()
            ],
        )?;
        Ok(())
    }

    pub fn note_failure(
        &self,
        handle: &str,
        error: &str,
        attempts: i64,
        gave_up: bool,
    ) -> rusqlite::Result<()> {
        self.db().execute(
            "INSERT OR REPLACE INTO lookup_failures VALUES (?,?,?,?,?)",
            params![
                handle.to_uppercase(),
                self.clock.time(),
                error,
                attempts,
                gave_up as i64
            ],
        )?;
        Ok(())
    }

    pub fn get_org(&self, sid: &str, any_age: bool) -> Result<Option<Org>, LookupError> {
        let row: Option<(Option<String>, Option<f64>)> = self
            .db()
            .query_row(
                "SELECT data, fetched_at FROM orgs WHERE sid=?",
                [sid.to_uppercase()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((data, at)) = row else {
            return Ok(None);
        };
        if !any_age {
            let at = at.ok_or_else(|| type_error("fetched_at is NULL"))?;
            if self.clock.time() - at > ORG_TTL {
                return Ok(None);
            }
        }
        let v = loads(data.as_deref().unwrap_or("null"))
            .map_err(|e| LookupError::other("JSONDecodeError", e))?;
        Ok(Some(serde_json::from_value(v).map_err(type_error)?))
    }

    /// `stored_at` keeps an existing copy's age.
    pub fn put_org(&self, o: &Org, stored_at: Option<f64>) -> rusqlite::Result<()> {
        let at = when(self.clock.time(), stored_at);
        self.db().execute(
            "INSERT OR REPLACE INTO orgs VALUES (?,?,?)",
            params![o.sid.to_uppercase(), to_json(o), at],
        )?;
        Ok(())
    }

    pub fn org_stored_at(&self, sid: &str) -> rusqlite::Result<Option<f64>> {
        Ok(self
            .db()
            .query_row(
                "SELECT fetched_at FROM orgs WHERE sid=?",
                [sid.to_uppercase()],
                |r| r.get::<_, Option<f64>>(0),
            )
            .optional()?
            .flatten())
    }

    /// An org's member list. A finished pull replaces it; an unfinished one
    /// only adds.
    pub fn put_members<S: AsRef<str>>(
        &self,
        sid: &str,
        handles: &[S],
        complete: bool,
    ) -> rusqlite::Result<()> {
        let now = self.clock.time();
        let sid = sid.to_uppercase();
        let mut db = self.db();
        let tx = db.transaction()?;
        if complete {
            tx.execute("DELETE FROM org_members WHERE sid=?", [&sid])?;
        }
        for h in handles {
            tx.execute(
                "INSERT OR REPLACE INTO org_members VALUES (?,?,?)",
                params![sid, h.as_ref().to_uppercase(), now],
            )?;
        }
        tx.commit()
    }

    /// Handles whose lookup failed before `before` and that have no
    /// profile since, whole or partial - newest failure first (the page's
    /// background retry).
    pub fn failed_unanswered(&self, before: f64) -> rusqlite::Result<Vec<String>> {
        let db = self.db();
        let mut st = db.prepare(
            "SELECT handle FROM lookup_failures
               WHERE ts < ?
                 AND handle NOT IN (SELECT handle FROM citizens)
                 AND handle NOT IN (SELECT handle FROM partial_citizens)
               ORDER BY ts DESC",
        )?;
        st.query_map([before], |r| r.get(0))?.collect()
    }

    /// Forgets an org's member list.
    pub fn drop_members(&self, sid: &str) -> rusqlite::Result<()> {
        self.db()
            .execute("DELETE FROM org_members WHERE sid=?", [sid.to_uppercase()])?;
        Ok(())
    }

    /// Which cached member lists contain this handle.
    pub fn member_orgs(&self, handle: &str) -> rusqlite::Result<Vec<String>> {
        let db = self.db();
        let mut stmt = db.prepare("SELECT sid FROM org_members WHERE handle=?")?;

        stmt.query_map([handle.to_uppercase()], |r| r.get(0))?
            .collect()
    }

    /// Counts, and the file's size - this cache's own file (rsi.py measures
    /// the default path whichever file it opened).
    pub fn stats(&self) -> rusqlite::Result<Value> {
        let db = self.db();
        let q = |s: &str| db.query_row(s, [], |r| r.get::<_, i64>(0));
        Ok(serde_json::json!({
            "citizens": q("SELECT COUNT(*) FROM citizens")?,
            "orgs": q("SELECT COUNT(*) FROM orgs")?,
            "cached_members": q("SELECT COUNT(*) FROM org_members")?,
            "bytes": std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0),
        }))
    }
}
