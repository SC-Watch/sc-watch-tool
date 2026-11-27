//! What the page draws (ui_server.collect): a tile per contact - the
//! people the watcher has seen, plus the open view's players - and the
//! state every panel needs.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::{Connection, OpenFlags, params_from_iter};
use serde_json::{Map, Value, json};

use scw_rsi::model::{ADJACENT_FOCUS, FOCUSES, PIRACY_ARCHETYPE, PIRACY_FOCUS};
use scw_rsi::{CITIZEN_TTL, Citizen, MISS_TTL, org_url, profile_url};
use scw_store::housekeeping::MANAGED;
use scw_store::reputation::{Database, PENDING, former_line, manual_line};
use scw_store::settings;

use crate::{Ui, round_to, truthy};

/// Disk use is cached this long: measuring stats every file.
pub const DISK_TTL: f64 = 10.0;
/// A view shows at most this many players.
pub const VIEW_CAP: usize = 200;

fn ro(path: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
}

fn chunks(handles: &[String]) -> impl Iterator<Item = &[String]> {
    handles.chunks(400)
}

/// The cached profiles of these handles (None: all), never fetched here.
/// Each carries `_stored_at`; SENTRY's partial profiles are laid over them.
pub fn rsi_cache(path: &Path, handles: Option<&[String]>) -> HashMap<String, Map<String, Value>> {
    let mut out = HashMap::new();
    if !path.exists() {
        return out;
    }
    let upper: Option<Vec<String>> = handles.map(|h| h.iter().map(|x| x.to_uppercase()).collect());
    if upper.as_ref().is_some_and(Vec::is_empty) {
        return out;
    }
    let Ok(db) = ro(path) else { return out };
    let mut rows: Vec<(String, String, Option<f64>)> = Vec::new();
    let mut read = |sql: &str, args: &[String]| {
        if let Ok(mut st) = db.prepare(sql)
            && let Ok(it) = st.query_map(params_from_iter(args.iter()), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<f64>>(2)?,
                ))
            })
        {
            rows.extend(it.flatten());
        }
    };
    match &upper {
        None => read("SELECT handle, data, fetched_at FROM citizens", &[]),
        Some(h) => {
            for c in chunks(h) {
                let qs = vec!["?"; c.len()].join(",");
                read(
                    &format!(
                        "SELECT handle, data, fetched_at FROM citizens WHERE handle IN ({qs})"
                    ),
                    c,
                );
            }
        }
    }
    for (h, data, fetched) in rows {
        if let Ok(Value::Object(mut prof)) = serde_json::from_str::<Value>(&data) {
            prof.insert("_stored_at".into(), json!(fetched));
            out.insert(h.to_uppercase(), prof);
        }
    }
    partials(&db, upper.as_deref(), &mut out);
    out
}

/// SENTRY's partial profiles over `out`: one shows when there is no whole
/// profile or it is newer, marked `_partial` and `_unknown_focus`.
fn partials(
    db: &Connection,
    handles: Option<&[String]>,
    out: &mut HashMap<String, Map<String, Value>>,
) {
    type Row = (String, String, Option<f64>, Option<String>, Option<String>);
    let mut rows: Vec<Row> = Vec::new();
    let base = "SELECT handle, data, observed_at, reason, unknown FROM partial_citizens";
    let mut read = |sql: &str, args: &[String]| -> bool {
        let Ok(mut st) = db.prepare(sql) else {
            return false;
        };
        let Ok(it) = st.query_map(params_from_iter(args.iter()), |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        }) else {
            return false;
        };
        rows.extend(it.flatten());
        true
    };
    match handles {
        None => {
            if !read(base, &[]) {
                return;
            }
        }
        Some(h) => {
            for c in chunks(h) {
                let qs = vec!["?"; c.len()].join(",");
                if !read(&format!("{base} WHERE handle IN ({qs})"), c) {
                    return;
                }
            }
        }
    }
    for (handle, data, observed, reason, unknown) in rows {
        let h = handle.to_uppercase();
        if let Some(whole) = out.get(&h) {
            let stored = whole
                .get("_stored_at")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            if stored >= observed.unwrap_or(0.0) {
                continue;
            }
        }
        let Ok(Value::Object(mut prof)) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        let Ok(Value::Array(unk)) = serde_json::from_str::<Value>(
            unknown.as_deref().filter(|u| !u.is_empty()).unwrap_or("[]"),
        ) else {
            continue;
        };
        prof.insert("_stored_at".into(), json!(observed));
        prof.insert(
            "_partial".into(),
            json!(
                reason
                    .filter(|r| !r.is_empty())
                    .unwrap_or_else(|| "incomplete".into())
            ),
        );
        prof.insert(
            "_unknown_focus".into(),
            Value::Array(
                unk.iter()
                    .map(|s| json!(s.as_str().unwrap_or("").to_uppercase()))
                    .collect(),
            ),
        );
        out.insert(h, prof);
    }
}

/// Lookups that failed and have not since succeeded, for these handles:
/// {"error", "attempts", "gave_up", "age_s"}.
pub fn rsi_failures(path: &Path, handles: &[String], now: f64) -> HashMap<String, Value> {
    let mut out = HashMap::new();
    let upper: Vec<String> = handles.iter().map(|h| h.to_uppercase()).collect();
    if upper.is_empty() || !path.exists() {
        return out;
    }
    let Ok(db) = ro(path) else { return out };
    for c in chunks(&upper) {
        let qs = vec!["?"; c.len()].join(",");
        let sql = format!(
            "SELECT handle, ts, error, attempts, gave_up FROM lookup_failures WHERE handle IN ({qs})"
        );
        let Ok(mut st) = db.prepare(&sql) else {
            return out;
        };
        let Ok(it) = st.query_map(params_from_iter(c.iter()), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, f64>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<i64>>(3)?,
                r.get::<_, Option<i64>>(4)?,
            ))
        }) else {
            return out;
        };
        for (h, ts, err, n, gave) in it.flatten() {
            out.insert(
                h,
                json!({"error": err, "attempts": n, "gave_up": gave.unwrap_or(0) != 0,
                       "age_s": (now - ts) as i64}),
            );
        }
    }
    out
}

fn get<'a>(m: &'a Map<String, Value>, k: &str) -> Option<&'a Value> {
    m.get(k)
}

fn s<'a>(m: &'a Map<String, Value>, k: &str) -> &'a str {
    m.get(k).and_then(Value::as_str).unwrap_or("")
}

/// The enlistment days of a stored profile (rsi.Citizen(...).enlisted_days).
pub fn enlisted_days(handle: &str, prof: &Map<String, Value>, now: f64, utc: bool) -> Option<i64> {
    let c = Citizen {
        handle: handle.into(),
        enlisted: s(prof, "enlisted").into(),
        ..Citizen::default()
    };
    c.enlisted_days(now, utc)
}

impl Ui {
    /// What the capture folders cost, biggest first; cached DISK_TTL.
    pub fn disk_usage(&self, max_age: f64) -> Vec<Value> {
        let now = self.now();
        {
            let st = self.st();
            if !st.disk.1.is_empty() && now - st.disk.0 < max_age {
                return st.disk.1.clone();
            }
        }
        let mut out: Vec<(f64, Value)> = Vec::new();
        for name in MANAGED.iter().chain(["audit"].iter()) {
            let d = self.data().join(name);
            if !d.is_dir() {
                continue;
            }
            let (mut n, mut size) = (0u64, 0u64);
            if let Ok(rd) = std::fs::read_dir(&d) {
                for e in rd.flatten() {
                    if let Ok(m) = e.metadata()
                        && m.is_file()
                    {
                        n += 1;
                        size += m.len();
                    }
                }
            }
            let mb = round_to(size as f64 / 1e6, 1);
            out.push((mb, json!({"name": name, "files": n, "mb": mb})));
        }
        // sorted(key=-mb): stable
        out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let out: Vec<Value> = out.into_iter().map(|(_, v)| v).collect();
        self.st().disk = (now, out.clone());
        out
    }

    /// Every folder the app uses, as a fixed table keyed by name - the API
    /// takes a key, never a path.
    pub fn folder_targets(&self) -> Vec<Value> {
        let mut out = vec![json!({"key": "data", "label": "Your data",
                                  "path": self.data().display().to_string(),
                                  "note": "databases, settings, keybinds"})];
        if self.roots.program != self.roots.data {
            out.push(json!({"key": "program", "label": "Program files",
                            "path": self.roots.program.display().to_string(),
                            "note": "the app itself - nothing you edit"}));
        }
        let known = |n: &str| match n {
            "audit" => "crops behind uncertain reads",
            "live_frames" => "frames kept from the watcher",
            "debug_bursts" => "full bursts saved for debugging",
            "probe_frames" => "probe captures",
            "detections" => "annotated detection images",
            _ => "",
        };
        for name in ["audit"].iter().chain(MANAGED.iter()) {
            let d = self.data().join(name);
            if d.is_dir() {
                out.push(json!({"key": name, "label": name,
                                "path": d.display().to_string(), "note": known(name)}));
            }
        }
        out
    }

    /// Audits, with a missing picture's path blanked and the row marked.
    pub fn audits_with_files(&self, db: &Database, handle: &str) -> Vec<Value> {
        let mut out = Vec::new();
        for mut a in db.audits_for(handle).unwrap_or_default() {
            let mut missing = false;
            for key in ["crop_name", "crop_range", "frame_path"] {
                let rel = a.get(key).and_then(Value::as_str).unwrap_or("").to_string();
                if !rel.is_empty() && !self.data().join(&rel).exists() {
                    a.insert(key.into(), json!(""));
                    missing = true;
                }
            }
            a.insert("files_missing".into(), json!(missing));
            out.push(Value::Object(a));
        }
        out
    }

    /// The state the page draws.
    pub fn collect(&self, view: Option<&str>, arg: &str) -> Result<Value, String> {
        let db = self.open_db()?;
        self.collect_with(&db, view, arg).map_err(|e| e.to_string())
    }

    #[allow(clippy::too_many_lines)]
    fn collect_with(
        &self,
        db: &Database,
        view: Option<&str>,
        arg: &str,
    ) -> rusqlite::Result<Value> {
        let cfg = self.settings();
        let now = self.now();
        let utc = self.hooks.utc;
        let max = cfg
            .get("max_contacts")
            .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|x| x as i64)))
            .unwrap_or(120);
        type Row = (String, Option<i64>, Option<f64>, Option<f64>);
        let player = |r: &rusqlite::Row| -> rusqlite::Result<Row> {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        };
        let mut rows: Vec<Row> = db
            .conn
            .prepare(
                "SELECT handle, sightings, first_seen, last_seen
                   FROM players WHERE COALESCE(sightings, 0) > 0
                   ORDER BY last_seen DESC LIMIT ?",
            )?
            .query_map([max], player)?
            .collect::<rusqlite::Result<_>>()?;
        let window: HashSet<String> = rows.iter().map(|r| r.0.clone()).collect();
        let mut view_rank: HashMap<String, usize> = HashMap::new();
        let mut view_total = 0;
        let mut met: HashSet<String> = window.clone();
        let view = view.filter(|v| matches!(*v, "audits" | "reports"));
        if let Some(v) = view {
            let mut handles = self.view_handles(db, v, arg)?;
            view_total = handles.len();
            handles.truncate(VIEW_CAP);
            for (i, h) in handles.iter().enumerate() {
                view_rank.insert(h.to_uppercase(), i);
            }
            let extra: Vec<String> = handles
                .into_iter()
                .filter(|h| !window.contains(h))
                .collect();
            let mut found: HashMap<String, Row> = HashMap::new();
            for c in chunks(&extra) {
                let qs = vec!["?"; c.len()].join(",");
                let sql = format!(
                    "SELECT handle, sightings, first_seen, last_seen FROM players WHERE handle IN ({qs})"
                );
                let got: Vec<Row> = db
                    .conn
                    .prepare(&sql)?
                    .query_map(params_from_iter(c.iter()), player)?
                    .collect::<rusqlite::Result<_>>()?;
                for r in got {
                    found.insert(r.0.clone(), r);
                }
            }
            for (h, r) in &found {
                if r.1.unwrap_or(0) > 0 {
                    met.insert(h.clone());
                }
            }
            for h in extra {
                rows.push(found.remove(&h).unwrap_or((h, Some(0), None, None)));
            }
        }
        let handles: Vec<String> = rows.iter().map(|r| r.0.clone()).collect();
        let cache = rsi_cache(&self.cache_path(), Some(&handles));
        let failures = rsi_failures(&self.cache_path(), &handles, now);
        let variants = truthy(Some(cfg.get("watchlist_variants").unwrap_or(&json!(true))));
        let org_manual = db.org_manual()?;
        let enl_manual = db.manual_enlisted(&handles)?;
        let empty = Map::new();
        let mut contacts = Vec::new();

        for (handle, sightings, first_seen, last_seen) in &rows {
            let a = db.assess(handle, Some(variants), !window.contains(handle))?;
            let last_range: Option<Option<f64>> =
                rusqlite::OptionalExtension::optional(db.conn.query_row(
                    "SELECT range_km FROM sightings WHERE handle=? ORDER BY ts DESC LIMIT 1",
                    [handle],
                    |r| r.get(0),
                ))?;
            let hu = handle.to_uppercase();
            let prof = cache.get(&hu).unwrap_or(&empty);
            let unknown_focus: HashSet<String> = prof
                .get("_unknown_focus")
                .and_then(Value::as_array)
                .map(|v| {
                    v.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let mut orgs: Vec<Map<String, Value>> = Vec::new();
            for o in prof
                .get("orgs")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                let o = o.as_object().cloned().unwrap_or_default();
                let sid_u = s(&o, "sid").to_uppercase();
                let mut focuses: Vec<String> = ["primary_focus", "secondary_focus"]
                    .iter()
                    .filter(|k| truthy(get(&o, k)))
                    .map(|k| s(&o, k).to_string())
                    .collect();
                let man = org_manual
                    .get(&sid_u)
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                let focus_manual = focuses.is_empty()
                    && (truthy(man.get("primary_focus")) || truthy(man.get("secondary_focus")));
                if focus_manual {
                    focuses = ["primary_focus", "secondary_focus"]
                        .iter()
                        .filter(|k| truthy(man.get(**k)))
                        .map(|k| s(&man, k).to_string())
                        .collect();
                }
                let name_manual = !truthy(o.get("name")) && truthy(man.get("name"));
                let piracy = focuses
                    .iter()
                    .any(|f| PIRACY_FOCUS.contains(&f.to_lowercase().as_str()));
                let mut adjacent: Vec<String> = focuses
                    .iter()
                    .filter(|f| ADJACENT_FOCUS.contains(&f.to_lowercase().as_str()))
                    .cloned()
                    .collect();
                let arch = s(&o, "archetype");
                if PIRACY_ARCHETYPE.contains(&arch.to_lowercase().as_str()) {
                    adjacent.push(arch.to_string());
                }
                let name = if truthy(o.get("name")) {
                    o["name"].clone()
                } else if name_manual {
                    man["name"].clone()
                } else {
                    json!("")
                };
                let mut m = Map::new();
                m.insert("sid".into(), o.get("sid").cloned().unwrap_or(json!("")));
                m.insert("name".into(), name);
                m.insert("focus_manual".into(), json!(focus_manual));
                m.insert("name_manual".into(), json!(name_manual));
                m.insert(
                    "members".into(),
                    o.get("members").cloned().unwrap_or(json!(0)),
                );
                m.insert(
                    "archetype".into(),
                    o.get("archetype").cloned().unwrap_or(json!("")),
                );
                m.insert("focuses".into(), json!(focuses));
                m.insert("piracy".into(), json!(piracy));
                m.insert("adjacent".into(), json!(adjacent));
                m.insert(
                    "is_main".into(),
                    o.get("is_main").cloned().unwrap_or(json!(false)),
                );
                m.insert("url".into(), json!(org_url(s(&o, "sid"))));
                m.insert(
                    "focus_unknown".into(),
                    json!(unknown_focus.contains(&sid_u) && !focus_manual),
                );
                orgs.push(m);
            }
            let partial_left = truthy(prof.get("_partial"))
                && orgs.iter().any(|o| o["focus_unknown"] == json!(true));

            let mut enl_days = if prof.is_empty() {
                None
            } else {
                enlisted_days(handle, prof, now, utc)
            };
            let enlisted_manual = enl_days.is_none() && enl_manual.contains_key(&hu);
            if enlisted_manual {
                enl_days = enl_manual
                    .get(&hu)
                    .and_then(Value::as_str)
                    .and_then(|iso| self.days_since(iso));
            }
            let yours = " (your entry)";
            let mut flags: Vec<String> = Vec::new();
            for o in &orgs {
                let tail = if o["focus_manual"] == json!(true) {
                    yours
                } else {
                    ""
                };
                if o["piracy"] == json!(true) {
                    flags.push(format!(
                        "PIRACY · {}{tail}",
                        o["sid"].as_str().unwrap_or("")
                    ));
                }
                for adj in o["adjacent"].as_array().unwrap() {
                    flags.push(format!(
                        "{} · {}{tail}",
                        adj.as_str().unwrap_or(""),
                        o["sid"].as_str().unwrap_or("")
                    ));
                }
            }
            if let Some(d) = enl_days
                && d < 90
            {
                flags.push(format!(
                    "new account · {d}d{}",
                    if enlisted_manual { yours } else { "" }
                ));
            }
            let score = a.score(now, &db.flags);
            let state = if a.alert(now, &db.flags) || score > 0.0 {
                "known"
            } else if !flags.is_empty() {
                "flagged"
            } else if prof.is_empty() || partial_left {
                "unchecked"
            } else {
                "clean"
            };
            let stored = prof
                .get("_stored_at")
                .and_then(Value::as_f64)
                .filter(|x| *x != 0.0);
            let ttl = if truthy(Some(prof.get("exists").unwrap_or(&json!(true)))) {
                CITIZEN_TTL
            } else {
                MISS_TTL
            };
            let in_met = met.contains(handle);
            let reasons: Vec<String> =
                a.flagged_orgs
                    .iter()
                    .map(|(sid, note)| {
                        format!(
                            "member of flagged org {sid}{}{}",
                            if note.is_empty() {
                                String::new()
                            } else {
                                format!(" — {note}")
                            },
                            manual_line(sid, &a.manual_orgs, &a.manual_by)
                        )
                    })
                    .chain(a.former_orgs.iter().map(|(sid, since)| {
                        former_line(sid, Some(*since).filter(|x| *x != 0.0), utc)
                    }))
                    .collect();
            let manual_by: Map<String, Value> = a
                .manual_by
                .iter()
                .map(|(k, v)| (k.clone(), json!(v)))
                .collect();
            let mut manual_sids: Vec<String> =
                a.manual_orgs.iter().map(|(k, _)| k.clone()).collect();
            manual_sids.sort();
            let fail = failures.get(&hu).cloned();
            let mut c = Map::new();
            c.insert("handle".into(), json!(handle));
            c.insert("in_window".into(), json!(window.contains(handle)));
            c.insert("in_view".into(), json!(view_rank.contains_key(&hu)));
            c.insert("view_rank".into(), json!(view_rank.get(&hu)));
            c.insert("met".into(), json!(in_met));
            c.insert("range_km".into(), json!(last_range.flatten()));
            c.insert("sightings".into(), json!(sightings.unwrap_or(0)));
            c.insert("first_seen".into(), json!(first_seen));
            c.insert("last_seen".into(), json!(last_seen));
            c.insert("score".into(), json!(round_to(score, 2)));
            c.insert("reasons".into(), json!(reasons));
            c.insert(
                "watchlist".into(),
                Value::Array(
                    a.watchlist
                        .iter()
                        .map(|h| {
                            json!({"source": h.source, "claim": h.claim, "confidence": h.confidence,
                                   "match": h.match_kind, "posted_ts": h.posted_ts,
                                   "by": h.reported_by, "url": h.msg_url, "listed_as": h.handle})
                        })
                        .collect(),
                ),
            );
            c.insert(
                "reports".into(),
                Value::Array(
                    a.reports
                        .iter()
                        .map(|r| {
                            json!({"id": r.id, "category": r.category, "note": r.note,
                                   "age_days": r.age_days(now) as i64,
                                   "weight": round_to(r.weight(now), 2), "source": r.source})
                        })
                        .collect(),
                ),
            );
            c.insert(
                "pending".into(),
                json!(a.reports.iter().filter(|r| r.category == PENDING).count()),
            );
            c.insert(
                "audits".into(),
                Value::Array(if in_met {
                    self.audits_with_files(db, handle)
                } else {
                    Vec::new()
                }),
            );
            c.insert("enlisted_days".into(), json!(enl_days));
            c.insert("redacted".into(), json!(truthy(prof.get("redacted"))));
            c.insert("known_profile".into(), json!(!prof.is_empty()));
            c.insert("lookup_failed".into(), fail.clone().unwrap_or(Value::Null));
            c.insert(
                "profile_age_days".into(),
                json!(stored.map(|t| ((now - t) / 86400.0) as i64)),
            );
            c.insert(
                "profile_stale".into(),
                json!(stored.is_some_and(|t| now - t > ttl)),
            );
            c.insert("verified".into(), json!(truthy(prof.get("exists"))));
            c.insert(
                "profile_partial".into(),
                if partial_left {
                    prof["_partial"].clone()
                } else {
                    Value::Null
                },
            );
            c.insert("enlisted_manual".into(), json!(enlisted_manual));
            c.insert(
                "enlisted_iso".into(),
                if enlisted_manual {
                    enl_manual.get(&hu).cloned().unwrap_or(Value::Null)
                } else {
                    Value::Null
                },
            );
            c.insert(
                "enlisted_missing".into(),
                json!(
                    enl_days.is_none()
                        && (!prof.is_empty() || fail.as_ref().is_some_and(|f| truthy(Some(f))))
                ),
            );
            c.insert(
                "flagged_sids".into(),
                json!(
                    a.flagged_orgs
                        .iter()
                        .map(|(s, _)| s.clone())
                        .collect::<Vec<_>>()
                ),
            );
            c.insert("manual_sids".into(), json!(manual_sids));
            c.insert("manual_by".into(), Value::Object(manual_by));
            c.insert(
                "orgs".into(),
                Value::Array(orgs.into_iter().map(Value::Object).collect()),
            );
            c.insert("flags".into(), json!(flags));
            c.insert("state".into(), json!(state));
            c.insert("profile_url".into(), json!(profile_url(handle)));
            contacts.push(Value::Object(c));
        }

        let mut stats = match serde_json::to_value(db.stats()?) {
            Ok(Value::Object(m)) => m,
            _ => Map::new(),
        };
        let one = |sql: &str| -> rusqlite::Result<i64> { db.conn.query_row(sql, [], |r| r.get(0)) };
        stats.insert(
            "contacts".into(),
            json!(one(
                "SELECT COUNT(*) FROM players WHERE COALESCE(sightings, 0) > 0"
            )?),
        );
        stats.insert("pending".into(), json!(db.pending_reports()?.len()));
        stats.insert("audits".into(), json!(db.open_audits()?.len()));
        stats.insert("focuses".into(), json!(FOCUSES));
        stats.insert("api_key".into(), self.key_status());
        let old = self.roots.unused_old_data(&|k| (self.hooks.env)(k));
        stats.insert(
            "old_data".into(),
            match old {
                Some(o) => json!({"old": o.display().to_string(), "here": self.data().display().to_string()}),
                None => Value::Null,
            },
        );
        stats.insert(
            "audits_orphaned".into(),
            json!(one(
                "SELECT COUNT(*) FROM audits WHERE handle NOT IN (SELECT handle FROM players)"
            )?),
        );
        {
            let st = self.st();
            stats.insert(
                "lookups".into(),
                Value::Object(
                    st.lookups
                        .0
                        .iter()
                        .map(|(k, v)| (k.clone(), json!(v)))
                        .collect(),
                ),
            );
            stats.insert(
                "lookup_done".into(),
                Value::Object(
                    st.lookup_done
                        .0
                        .iter()
                        .map(|(k, v)| (k.clone(), json!(v)))
                        .collect(),
                ),
            );
        }
        stats.insert(
            "view".into(),
            match view {
                Some(v) => {
                    json!({"name": v, "arg": arg, "total": view_total, "shown": view_rank.len()})
                }
                None => Value::Null,
            },
        );
        if view == Some("reports") {
            stats.insert("report_sources".into(), json!(report_sources(db)?));
        }
        stats.insert("settings".into(), Value::Object(cfg.clone()));
        stats.insert("settings_schema".into(), settings::describe());
        stats.insert("disk".into(), Value::Array(self.disk_usage(DISK_TTL)));
        stats.insert("ignored".into(), json!(db.ignored_list()?));
        let flags = self.flags();
        stats.insert(
            "categories".into(),
            Value::Array(
                flags
                    .active()
                    .iter()
                    .map(|f| json!({"key": f.key, "label": f.label, "color": f.color, "polarity": f.polarity}))
                    .collect(),
            ),
        );
        stats.insert("flag_slots".into(), json!(scw_store::flags::SLOTS));
        stats.insert(
            "flagmap".into(),
            Value::Object(
                flags
                    .all
                    .iter()
                    .map(|f| {
                        (
                            f.key.clone(),
                            json!({"label": f.label, "color": f.color, "polarity": f.polarity,
                                               "say": f.say, "retired": f.retired}),
                        )
                    })
                    .collect(),
            ),
        );
        stats.insert("flagged_orgs_list".into(), json!(db.flagged_orgs()?));
        {
            let st = self.st();
            stats.insert(
                "rosters".into(),
                Value::Object(
                    st.rosters
                        .0
                        .iter()
                        .map(|(k, v)| (k.clone(), json!(v)))
                        .collect(),
                ),
            );
            stats.insert(
                "org_jobs".into(),
                Value::Object(
                    st.org_jobs
                        .0
                        .iter()
                        .map(|(k, j)| {
                            (
                                k.clone(),
                                j.lock().unwrap_or_else(|e| e.into_inner()).to_json(),
                            )
                        })
                        .collect(),
                ),
            );
        }
        stats.insert("folders".into(), Value::Array(self.folder_targets()));
        stats.insert("watchlist_on".into(), json!(self.watchlist_on()));
        stats.insert("watchlists".into(), json!([]));
        let wl_path = self.data().join("watchlist.db");
        if wl_path.exists()
            && let Some(wl) = self.open_watchlist(&wl_path)
            && let (Ok(src), Ok(orgs)) = (wl.sources(), wl.orgs_flagged())
        {
            stats.insert("watchlists".into(), json!(src));
            stats.insert("watchlist_orgs".into(), json!(orgs));
        }
        // Not in the Python: the mining overlay of the watcher in this
        // process, while it runs.
        if let Some((_, Some(v))) = self.board.as_ref().map(|b| b.get()) {
            stats.insert("mining".into(), v);
        }
        // Not in the Python: the watcher and the page are one program, so
        // saving a watcher setting restarts it by itself.
        if self.links.is_some() {
            stats.insert("one_program".into(), json!(true));
        }
        Ok(json!({"contacts": contacts, "stats": stats, "now": now}))
    }

    /// The handles a view names, in its order.
    pub fn view_handles(
        &self,
        db: &Database,
        view: &str,
        arg: &str,
    ) -> rusqlite::Result<Vec<String>> {
        let col = |sql: &str, args: &[&str]| -> rusqlite::Result<Vec<String>> {
            db.conn
                .prepare(sql)?
                .query_map(params_from_iter(args.iter()), |r| r.get(0))?
                .collect()
        };
        match view {
            "audits" => col(
                "SELECT handle FROM audits WHERE handle IN (SELECT handle FROM players)
                   GROUP BY handle ORDER BY MAX(ts) DESC",
                &[],
            ),
            _ => {
                let like = format!("{arg}:%");
                let (whr, args): (&str, Vec<&str>) = match arg {
                    "self" => ("WHERE origin IS NULL", vec![]),
                    "" => ("", vec![]),
                    _ => ("WHERE origin LIKE ?", vec![like.as_str()]),
                };
                col(
                    &format!(
                        "SELECT UPPER(handle) FROM reports {whr}
                            GROUP BY UPPER(handle) ORDER BY COUNT(*) DESC, MAX(ts) DESC"
                    ),
                    &args,
                )
            }
        }
    }
}

/// Whose reports there are, for the reports view's chips: yours first,
/// then each org mate by name.
pub fn report_sources(db: &Database) -> rusqlite::Result<Vec<Value>> {
    let names: HashMap<String, Option<String>> = db
        .conn
        .prepare("SELECT install, name FROM shares")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut out: Vec<(bool, String, Value)> = Vec::new();
    let rows: Vec<(String, i64, i64)> = db
        .conn
        .prepare(
            "SELECT CASE WHEN origin IS NULL THEN 'self'
                    ELSE substr(origin, 1, instr(origin, ':') - 1) END AS src,
                    COUNT(DISTINCT UPPER(handle)), COUNT(*)
               FROM reports GROUP BY src",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (src, players, n) in rows {
        let name = if src == "self" {
            "yours".to_string()
        } else {
            names
                .get(&src)
                .cloned()
                .flatten()
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "an org mate".into())
        };
        out.push((
            src != "self",
            name.to_lowercase(),
            json!({"key": src, "players": players, "reports": n, "name": name}),
        ));
    }
    out.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    Ok(out.into_iter().map(|(_, _, v)| v).collect())
}
