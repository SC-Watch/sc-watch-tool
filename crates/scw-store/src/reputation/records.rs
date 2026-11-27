//! Sightings, reports, the ignore list and audits (reputation.py's writes
//! and their reads), as the Python does them.

use std::collections::BTreeSet;

use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde_json::{Map, Value};

use super::database::Database;
use super::{PENDING, sketch};

/// What forget() removed, per table.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Removed {
    pub audits: i64,
    pub sightings: i64,
    pub reports: i64,
    pub orgs: i64,
    pub player: i64,
}

impl Removed {
    fn any(&self) -> bool {
        self.audits + self.sightings + self.reports + self.orgs + self.player > 0
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Ignored {
    pub label: String,
    pub removed: Removed,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DeletedReport {
    pub handle: String,
    pub category: String,
    pub note: String,
    pub ts: f64,
}

/// What confirm_audit() did, or why it did not (the Python's dict, its keys
/// in its order).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Confirmed {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs_confirm: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub renamed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adopted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Confirmed {
    fn refused(error: String) -> Self {
        Self {
            error: Some(error),
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Orphan {
    pub id: i64,
    pub handle: String,
    pub read_as: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Pruned {
    pub removed: usize,
}

/// One audit row with its `variants` JSON parsed (an unreadable one as []).
fn parse_variants(mut row: Map<String, Value>) -> Map<String, Value> {
    let v = row
        .get("variants")
        .and_then(Value::as_str)
        .map_or(Ok(Value::Array(vec![])), |s| {
            if s.is_empty() {
                Ok(Value::Array(vec![]))
            } else {
                crate::json::loads(s)
            }
        })
        .unwrap_or(Value::Array(vec![]));
    row.insert("variants".into(), v);
    row
}

/// Python's json.dumps(list(variants)): compact, ", " and ": ", ASCII.
fn py_compact(v: &Value) -> String {
    crate::json::dumps_compact(v).unwrap_or_default()
}

impl Database {
    /// Records one sighting; returns how many times this handle is now seen,
    /// and clears audits the sighting count corroborates.
    pub fn record_sighting(
        &self,
        handle: &str,
        range_km: Option<f64>,
        votes: i64,
        total: i64,
    ) -> rusqlite::Result<i64> {
        let (h, _) = self.canonical(handle, true)?;
        let now = self.now();
        // One commit for the player and the sighting, as the Python's: two
        // would show the page the player without the sighting for a moment
        // (and wake its live view twice). A savepoint, so it nests inside a
        // caller's transaction.
        self.conn.execute_batch("SAVEPOINT record_sighting")?;
        let wrote = (|| {
            self.conn.execute(
                "INSERT INTO players (handle, first_seen, last_seen, sightings, sketch)
                   VALUES (?,?,?,1,?)
                   ON CONFLICT(handle) DO UPDATE SET
                     last_seen=excluded.last_seen, sightings=sightings+1",
                params![h, now, now, sketch(&h)],
            )?;
            self.conn.execute(
                "INSERT INTO sightings (handle, ts, range_km, votes, total) VALUES (?,?,?,?,?)",
                params![h, now, range_km, votes, total],
            )
        })();
        if let Err(e) = wrote {
            let _ = self
                .conn
                .execute_batch("ROLLBACK TO record_sighting; RELEASE record_sighting");
            return Err(e);
        }
        self.conn.execute_batch("RELEASE record_sighting")?;
        let seen: i64 = self
            .conn
            .query_row("SELECT sightings FROM players WHERE handle=?", [&h], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or(0);
        // Seen again on a LATER ping corroborates as well as a second frame.
        self.clear_corroborated_audits(&h, votes.max(seen))?;
        Ok(seen)
    }

    /// Never report this label again, and forget what it already left.
    pub fn add_ignored(&self, label: &str, note: &str) -> Result<Ignored, String> {
        let lab = label.trim().to_uppercase();
        if lab.is_empty() {
            return Err("no label given".into());
        }
        let removed = self.forget(&lab).map_err(|e| e.to_string())?;
        self.conn
            .execute(
                "INSERT INTO ignored (label, sketch, note, ts, hits)
               VALUES (?,?,?,?,0)
               ON CONFLICT(label) DO UPDATE SET note=excluded.note",
                params![lab, sketch(&lab), note, self.now()],
            )
            .map_err(|e| e.to_string())?;
        Ok(Ignored {
            label: lab,
            removed,
        })
    }

    pub fn remove_ignored(&self, label: &str) -> rusqlite::Result<bool> {
        Ok(self.conn.execute(
            "DELETE FROM ignored WHERE label=?",
            [label.trim().to_uppercase()],
        )? > 0)
    }

    /// Is this exact handle on one of YOUR lists - a report, a flagged
    /// org's roster, an exact watchlist entry? (Your lists outrank the
    /// built-in word list of game labels.)
    pub fn is_listed(&self, handle: &str) -> rusqlite::Result<bool> {
        let h = handle.trim();
        if h.is_empty() {
            return Ok(false);
        }
        if self.exists(
            "SELECT 1 FROM reports WHERE handle=? COLLATE NOCASE LIMIT 1",
            [h],
        )? {
            return Ok(true);
        }
        if self.exists(
            "SELECT 1 FROM org_membership m JOIN org_flags f ON f.sid = m.sid WHERE m.handle=? COLLATE NOCASE LIMIT 1",
            [h],
        )? {
            return Ok(true);
        }
        Ok(self.watchlist.as_ref().is_some_and(|wl| {
            wl.lookup(h, Some(false))
                .is_ok_and(|hits| hits.iter().any(|x| x.match_kind == "exact"))
        }))
    }

    pub fn ignored_list(&self) -> rusqlite::Result<Vec<Map<String, Value>>> {
        self.maps("SELECT * FROM ignored ORDER BY hits DESC, label", [])
    }

    /// The rule that suppresses this name - exact, then by sketch - or None.
    /// Counts the hit.
    pub fn is_ignored(&self, name: &str) -> rusqlite::Result<Option<String>> {
        let n = name.trim().to_uppercase();
        if n.is_empty() {
            return Ok(None);
        }
        let get = |sql: &str, v: &str| -> rusqlite::Result<Option<String>> {
            self.conn.query_row(sql, [v], |r| r.get(0)).optional()
        };
        let mut label = get("SELECT label FROM ignored WHERE label=?", &n)?;
        if label.is_none() {
            label = get("SELECT label FROM ignored WHERE sketch=?", &sketch(&n))?;
        }
        if let Some(l) = &label {
            self.conn
                .execute("UPDATE ignored SET hits=hits+1 WHERE label=?", [l])?;
        }
        Ok(label)
    }

    /// Adds a handle with no sighting attached; true if it was new.
    pub fn ensure_player(&self, handle: &str) -> rusqlite::Result<bool> {
        let (h, _) = self.canonical(handle, false)?;
        let now = self.now();
        Ok(self.conn.execute(
            "INSERT INTO players (handle, first_seen, last_seen, sightings, sketch)
               VALUES (?,?,?,0,?) ON CONFLICT(handle) DO NOTHING",
            params![h, now, now, sketch(&h)],
        )? > 0)
    }

    /// Files a report. `exact`: under the handle as given, making nobody a
    /// contact; otherwise the spelling is resolved and a players row made.
    pub fn add_report(
        &self,
        handle: &str,
        category: &str,
        note: &str,
        evidence: &str,
        source: &str,
        exact: bool,
    ) -> rusqlite::Result<()> {
        let now = self.now();
        let h = if exact {
            handle.trim().to_uppercase()
        } else {
            let (h, _) = self.canonical(handle, true)?;
            self.conn.execute(
                "INSERT INTO players (handle, first_seen, last_seen, sightings, sketch)
                   VALUES (?,?,?,0,?) ON CONFLICT(handle) DO NOTHING",
                params![h, now, now, sketch(&h)],
            )?;
            h
        };
        self.conn.execute(
            "INSERT INTO reports (handle, ts, category, note, evidence, source) VALUES (?,?,?,?,?,?)",
            params![h, now, category, note, evidence, source],
        )?;
        Ok(())
    }

    /// Handles flagged but not yet explained, newest first.
    pub fn pending_reports(&self) -> rusqlite::Result<Vec<(String, f64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT handle, MAX(ts) FROM reports WHERE category=? GROUP BY handle ORDER BY MAX(ts) DESC",
        )?;
        let rows = stmt.query_map([PENDING], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    }

    fn category_error(&self, without_pending: bool) -> String {
        let keys: Vec<String> = self
            .flags
            .keys()
            .into_iter()
            .filter(|k| !(without_pending && k == PENDING))
            .collect();
        let list = keys
            .iter()
            .map(|k| format!("'{k}'"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("category must be one of [{list}]")
    }

    /// Gives a reason to the flags raised in the moment, keeping their time.
    pub fn resolve_pending(
        &self,
        handle: &str,
        category: &str,
        note: &str,
        evidence: &str,
    ) -> Result<usize, String> {
        if !self.flags.keys().iter().any(|k| k == category) || category == PENDING {
            return Err(self.category_error(true));
        }
        let (h, _) = self.canonical(handle, true).map_err(|e| e.to_string())?;
        self.conn
            .execute(
                "UPDATE reports SET category=?, note=?, evidence=? WHERE handle=? AND category=?",
                params![category, note, evidence, h, PENDING],
            )
            .map_err(|e| e.to_string())
    }

    /// Changes one report in place; its time is never touched.
    pub fn edit_report(
        &self,
        id: i64,
        category: Option<&str>,
        note: Option<&str>,
        evidence: Option<&str>,
    ) -> Result<bool, String> {
        let mut sets = Vec::new();
        let mut vals: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(c) = category {
            if !self.flags.keys().iter().any(|k| k == c) {
                return Err(self.category_error(false));
            }
            sets.push("category=?");
            vals.push(c.to_string().into());
        }
        if let Some(n) = note {
            sets.push("note=?");
            vals.push(n.to_string().into());
        }
        if let Some(e) = evidence {
            sets.push("evidence=?");
            vals.push(e.to_string().into());
        }
        if sets.is_empty() {
            return Ok(false);
        }
        vals.push(id.into());
        let sql = format!("UPDATE reports SET {} WHERE id=?", sets.join(", "));
        Ok(self
            .conn
            .execute(&sql, rusqlite::params_from_iter(vals))
            .map_err(|e| e.to_string())?
            > 0)
    }

    pub fn delete_report(&self, id: i64) -> rusqlite::Result<Option<DeletedReport>> {
        let row = self
            .conn
            .query_row(
                "SELECT handle, category, note, ts FROM reports WHERE id=?",
                [id],
                |r| {
                    Ok(DeletedReport {
                        handle: r.get(0)?,
                        category: r.get(1)?,
                        note: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        ts: r.get(3)?,
                    })
                },
            )
            .optional()?;
        if row.is_some() {
            self.conn.execute("DELETE FROM reports WHERE id=?", [id])?;
        }
        Ok(row)
    }

    // ---- audits -------------------------------------------------------------

    /// Records a read worth checking by eye; returns its id. One open audit
    /// per handle for the non-vote reasons.
    #[allow(clippy::too_many_arguments)]
    pub fn add_audit(
        &self,
        handle: &str,
        range_km: Option<f64>,
        votes: i64,
        total: i64,
        variants: &Value,
        frame_path: &str,
        crop_name: &str,
        crop_range: &str,
        reason: &str,
    ) -> rusqlite::Result<i64> {
        let raw = handle.to_uppercase();
        let (h, _) = self.canonical(&raw, false)?;
        if reason != "weak_read"
            && let Some(id) = self
                .conn
                .query_row(
                    "SELECT id FROM audits WHERE handle=? AND reason=?",
                    params![h, reason],
                    |r| r.get::<_, i64>(0),
                )
                .optional()?
        {
            return Ok(id);
        }
        let list = match variants {
            Value::Array(_) => variants.clone(),
            Value::Null => Value::Array(vec![]),
            other => Value::Array(vec![other.clone()]),
        };
        self.conn.execute(
            "INSERT INTO audits (ts, handle, read_as, range_km, votes, total,
                                   variants, frame_path, crop_name, crop_range,
                                   reason)
               VALUES (?,?,?,?,?,?,?,?,?,?,?)",
            params![
                self.now(),
                h,
                raw,
                range_km,
                votes,
                total,
                py_compact(&list),
                frame_path,
                crop_name,
                crop_range,
                reason
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Drops the vote-raised audits of a handle a read now corroborates.
    pub fn clear_corroborated_audits(&self, handle: &str, votes: i64) -> rusqlite::Result<usize> {
        if votes < 2 {
            return Ok(0);
        }
        let (h, _) = self.canonical(handle, false)?;
        let rows = self.maps(
            "SELECT * FROM audits WHERE handle=? AND COALESCE(reason,'weak_read')='weak_read'",
            [&h],
        )?;
        for row in &rows {
            self.drop_audit_files(row);
        }
        if !rows.is_empty() {
            self.conn.execute(
                "DELETE FROM audits WHERE handle=? AND COALESCE(reason,'weak_read')='weak_read'",
                [&h],
            )?;
        }
        Ok(rows.len())
    }

    pub fn audits_for(&self, handle: &str) -> rusqlite::Result<Vec<Map<String, Value>>> {
        let (h, _) = self.canonical(handle, false)?;
        Ok(self
            .maps(
                "SELECT * FROM audits WHERE handle=? OR handle=? ORDER BY ts DESC",
                params![h, handle.to_uppercase()],
            )?
            .into_iter()
            .map(parse_variants)
            .collect())
    }

    pub fn open_audits(&self) -> rusqlite::Result<Vec<Map<String, Value>>> {
        Ok(self
            .maps("SELECT * FROM audits ORDER BY ts DESC", [])?
            .into_iter()
            .map(parse_variants)
            .collect())
    }

    fn audit(&self, id: i64) -> rusqlite::Result<Option<Map<String, Value>>> {
        Ok(self
            .maps("SELECT * FROM audits WHERE id=?", [id])?
            .into_iter()
            .next())
    }

    /// Drops an audit without changing the record - the read was fine.
    pub fn dismiss_audit(&self, id: i64) -> rusqlite::Result<bool> {
        let Some(row) = self.audit(id)? else {
            return Ok(false);
        };
        self.drop_audit_files(&row);
        self.conn.execute("DELETE FROM audits WHERE id=?", [id])?;
        Ok(true)
    }

    /// Says what the label actually said, and folds the record onto it -
    /// refusing, unless forced, when both spellings are real accounts.
    pub fn confirm_audit(
        &self,
        id: i64,
        correct: &str,
        force: bool,
    ) -> rusqlite::Result<Confirmed> {
        let Some(row) = self.audit(id)? else {
            return Ok(Confirmed::refused("audit not found".into()));
        };
        let old = row["handle"].as_str().unwrap_or("").to_uppercase();
        let new = correct.trim().to_uppercase();
        if new.is_empty() {
            return Ok(Confirmed::refused("no name given".into()));
        }
        let (mut renamed, mut adopted, mut checked) = (false, false, false);
        if new != old {
            let (both, was_checked) = self.both_accounts_exist(&old, &new);
            checked = was_checked;
            if !force && both {
                return Ok(Confirmed {
                    needs_confirm: Some(true),
                    old: Some(old.clone()),
                    new: Some(new.clone()),
                    checked: Some(checked),
                    error: Some(format!(
                        "{old} and {new} both resolve to real accounts - merging them would give one person the other's history"
                    )),
                    ..Default::default()
                });
            }
            renamed = self.rename(&old, &new)?;
            if !renamed {
                if self.is_ignored(&new)?.is_some() {
                    return Ok(Confirmed {
                        old: Some(old),
                        new: Some(new.clone()),
                        error: Some(format!(
                            "{new} is on your ignore list as 'not a person' - remove it there first if that was wrong"
                        )),
                        ..Default::default()
                    });
                }
                let num = |k: &str| row.get(k).and_then(Value::as_i64).unwrap_or(0);
                self.record_sighting(
                    &new,
                    row.get("range_km").and_then(Value::as_f64),
                    num("votes"),
                    num("total"),
                )?;
                adopted = true;
            }
        }
        self.drop_audit_files(&row);
        self.conn.execute("DELETE FROM audits WHERE id=?", [id])?;
        self.conn.execute(
            "UPDATE audits SET handle=? WHERE handle=?",
            params![new, old],
        )?;
        Ok(Confirmed {
            ok: true,
            old: Some(old),
            new: Some(new),
            renamed: Some(renamed),
            adopted: Some(adopted),
            checked: Some(checked),
            ..Default::default()
        })
    }

    /// Audits pointing at a player row that no longer exists.
    pub fn orphan_audits(&self, apply: bool) -> rusqlite::Result<Vec<Orphan>> {
        let mut out = Vec::new();
        for row in self.maps("SELECT * FROM audits", [])? {
            let h = row["handle"].as_str().unwrap_or("").to_string();
            if self.exists("SELECT 1 FROM players WHERE handle=?", [&h])? {
                continue;
            }
            let id = row["id"].as_i64().unwrap_or(0);
            let read_as = row
                .get("read_as")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or(&h)
                .to_string();
            out.push(Orphan {
                id,
                handle: h.clone(),
                read_as,
            });
            if apply {
                self.drop_audit_files(&row);
                self.conn.execute("DELETE FROM audits WHERE id=?", [id])?;
            }
        }
        Ok(out)
    }

    /// Every file an audit row points at, with forward slashes.
    pub fn audit_files_in_use(&self) -> rusqlite::Result<BTreeSet<String>> {
        let mut used = BTreeSet::new();
        for row in self.maps("SELECT crop_name, crop_range, frame_path FROM audits", [])? {
            for v in row.values() {
                if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
                    used.insert(s.replace('\\', "/"));
                }
            }
        }
        Ok(used)
    }

    /// Files in audit/ that no row points at (sorted).
    pub fn prune_orphan_audit_files(&self, apply: bool) -> rusqlite::Result<Vec<String>> {
        let base = self.root.join("audit");
        let Ok(dir) = std::fs::read_dir(&base) else {
            return Ok(Vec::new());
        };
        let used = self.audit_files_in_use()?;
        let mut files: Vec<_> = dir
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        files.sort();
        let mut orphans = Vec::new();
        for f in files {
            let rel = format!("audit/{}", f.file_name().unwrap().to_string_lossy());
            if !used.contains(&rel) {
                orphans.push(rel);
                if apply {
                    let _ = std::fs::remove_file(&f);
                }
            }
        }
        Ok(orphans)
    }

    /// Ages out audits nobody checked, and their files.
    pub fn prune_audits(&self, max_age_days: f64) -> rusqlite::Result<Pruned> {
        let cutoff = self.now() - max_age_days * 86400.0;
        let rows = self.maps("SELECT * FROM audits WHERE ts < ?", [cutoff])?;
        for r in &rows {
            self.drop_audit_files(r);
        }
        self.conn
            .execute("DELETE FROM audits WHERE ts < ?", [cutoff])?;
        Ok(Pruned {
            removed: rows.len(),
        })
    }

    /// Erases one exact handle; returns what was removed. Roster rows stay:
    /// they are a flagged org's published fact, not your record of a person.
    pub fn forget(&self, handle: &str) -> rusqlite::Result<Removed> {
        let h = handle.to_uppercase();
        let n = |t: &str| self.count(&format!("SELECT COUNT(*) FROM {t} WHERE handle=?"), [&h]);
        let removed = Removed {
            audits: n("audits")?,
            sightings: n("sightings")?,
            reports: n("reports")?,
            orgs: n("player_orgs")?,
            player: n("players")?,
        };
        if !removed.any() {
            return Ok(removed);
        }
        for r in self.maps("SELECT * FROM audits WHERE handle=?", [&h])? {
            self.drop_audit_files(&r);
        }
        self.conn.execute(
            "DELETE FROM player_orgs WHERE handle=? AND source!='roster'",
            [&h],
        )?;
        for table in ["sightings", "reports", "audits", "players", "player_manual"] {
            self.conn
                .execute(&format!("DELETE FROM {table} WHERE handle=?"), [&h])?;
        }
        Ok(removed)
    }

    pub fn update_profile(
        &self,
        handle: &str,
        moniker: &str,
        enlisted: &str,
    ) -> rusqlite::Result<()> {
        let now = self.now();
        self.conn.execute(
            "INSERT INTO players (handle, moniker, enlisted, first_seen, last_seen)
               VALUES (?,?,?,?,?)
               ON CONFLICT(handle) DO UPDATE SET
                 moniker=excluded.moniker, enlisted=excluded.enlisted",
            params![handle.to_uppercase(), moniker, enlisted, now, now],
        )?;
        Ok(())
    }

    /// Your entry for when an account was made; blank clears it.
    pub fn set_manual_enlisted(&self, handle: &str, iso: &str) -> rusqlite::Result<()> {
        let h = handle.trim().to_uppercase();
        if iso.is_empty() {
            self.conn
                .execute("DELETE FROM player_manual WHERE handle=?", [&h])?;
        } else {
            self.conn.execute(
                "INSERT OR REPLACE INTO player_manual (handle, enlisted, ts) VALUES (?,?,?)",
                params![h, iso, self.now()],
            )?;
        }
        Ok(())
    }

    /// handle -> your ISO enlistment date, for the handles asked about.
    pub fn manual_enlisted(&self, handles: &[String]) -> rusqlite::Result<Map<String, Value>> {
        let hs: Vec<String> = handles.iter().map(|h| h.to_uppercase()).collect();
        let mut out = Map::new();
        for chunk in hs.chunks(400) {
            let qs = vec!["?"; chunk.len()].join(",");
            let mut stmt = self.conn.prepare(&format!(
                "SELECT handle, enlisted FROM player_manual WHERE handle IN ({qs})"
            ))?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })?;
            for row in rows {
                let (h, e) = row?;
                if let Some(e) = e.filter(|e| !e.is_empty()) {
                    out.insert(h, Value::String(e));
                }
            }
        }
        Ok(out)
    }
}
