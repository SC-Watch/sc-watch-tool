//! What the page starts and does not wait for: a lookup a person asked
//! for, an org's member list, an org's members looked up one by one, and
//! the background check that fills in what lookups left out.
//!
//! Each runs through [`crate::Hooks::spawn`] (a thread; inline in tests)
//! and reports back the way the Python's does: a line in LOOKUPS, ROSTERS
//! or an org's job, and the results landing in the databases, which the
//! stream notices.

use std::sync::{Arc, Mutex};

use serde_json::{Map, Value, json};

use scw_rsi::sentry::{sentry_absent, sentry_has};
use scw_rsi::{CITIZEN_TTL, Client, Failure, LookupError, MISS_TTL};
use scw_store::json::py_strip;
use scw_store::reputation::{Database, ProfileOrg};

use crate::collect::{enlisted_days, rsi_cache, rsi_failures};
use crate::{Job, Ui, clean_sid, py_round, truthy};

/// LOOKUPS and LOOKUP_DONE keep this many.
pub const LOOKUPS_MAX: usize = 40;
pub const ROSTERS_MAX: usize = 20;
/// "Look up contacts that were never checked": at most this many a run.
pub const BACKFILL_MAX: usize = 60;
/// Failed lookups older than this are asked again in the background...
pub const RETRY_FAILED_AFTER: f64 = 1800.0;
/// ... at most this many live fetches a round ...
pub const RETRY_FAILED_LIVE: usize = 10;
/// ... and this many database asks.
pub const RETRY_FAILED_CHECKS: usize = 30;
/// The background check: first a minute after start, then every ten.
pub const FOCUS_SWEEP_FIRST: f64 = 60.0;
pub const FOCUS_SWEEP_EVERY: f64 = 600.0;

/// A lookup error as the page says it (rsi.describe_failure).
pub fn describe(ui: &Ui, e: &LookupError) -> String {
    e.describe(ui.clock.monotonic())
}

fn orgs_of(c: &scw_rsi::Citizen) -> Vec<ProfileOrg> {
    c.orgs
        .iter()
        .map(|o| ProfileOrg {
            sid: o.sid.clone(),
            is_main: o.is_main,
        })
        .collect()
}

fn met(db: &Database, h: &str) -> bool {
    db.conn
        .query_row("SELECT 1 FROM players WHERE handle=?", [h], |_| Ok(()))
        .is_ok()
}

impl Ui {
    /// Records a lookup a person asked for that failed, so the tile says so.
    pub fn note_lookup_failure(&self, client: &Client, handle: &str, e: &LookupError) {
        let _ = client
            .cache
            .note_failure(handle, &describe(self, e), 1, true);
    }

    /// Fetches a profile because a PERSON asked for it, in the background;
    /// false if a lookup of this handle is already running.
    pub fn lookup_now(self: &Arc<Self>, handle: &str, fresh: bool) -> bool {
        let h = py_strip(handle).to_uppercase();
        if h.is_empty() {
            return false;
        }
        {
            let mut st = self.st();
            if !st.looking.insert(h.clone()) {
                return false;
            }
            st.lookups.pop(&h);
            st.lookups.set(&h, format!("{h}: looking up…"));
        }
        let me = self.clone();
        (self.hooks.spawn)(Box::new(move || me.lookup_work(&h, fresh)));
        true
    }

    fn lookup_work(&self, h: &str, fresh: bool) {
        let line = self.lookup_line(h, fresh);
        let mut st = self.st();
        if let Some(l) = line {
            st.lookups.set(h, l);
        }
        st.lookups.cap(LOOKUPS_MAX);
        st.done_seq += 1;
        let n = st.done_seq;
        st.lookup_done.pop(h);
        st.lookup_done.set(h, n);
        st.lookup_done.cap(LOOKUPS_MAX);
        st.looking.remove(h);
    }

    /// The outcome line; None leaves "looking up…" (a client that could
    /// not be built).
    fn lookup_line(&self, h: &str, fresh: bool) -> Option<String> {
        let client = match self.client() {
            Ok(c) => c,
            Err(e) => return Some(format!("{h}: lookup failed - {e}")),
        };
        let c = {
            let _w = scw_rsi::why(if fresh { "try-now" } else { "page-lookup" });
            client.citizen(h, fresh, true)
        };
        let c = match c {
            Ok(c) => c,
            Err(e) => {
                self.note_lookup_failure(&client, h, &e);
                return Some(format!("{h}: lookup failed - {}", describe(self, &e)));
            }
        };
        if c.note.checking.is_some() {
            return Some(format!(
                "{h}: {} - try again in about {} s",
                c.note.stale_reason.clone().unwrap_or_else(|| "None".into()),
                20.max(py_round(
                    c.note.retry_in.filter(|r| *r != 0.0).unwrap_or(30.0)
                ))
            ));
        }
        if !c.exists {
            return Some(format!("{h}: no profile — likely still a misread"));
        }
        let now = self.now();
        let utc = self.hooks.utc;
        let db = match self.open_db() {
            Ok(db) => db,
            Err(e) => return Some(format!("{h}: lookup failed - {e}")),
        };
        let r = (|| -> rusqlite::Result<String> {
            if !c.orgs.is_empty() {
                db.set_orgs(h, &orgs_of(&c))?;
            }
            let was_met = met(&db, h);
            if was_met {
                db.update_profile(h, &c.moniker, &c.enlisted)?;
            }
            let mut bits = vec![c.summary(now, utc)];
            let a = db.assess(h, None, !was_met)?;
            for (sid, note) in &a.flagged_orgs {
                bits.push(format!(
                    "FLAGGED ORG {sid}{}",
                    if note.is_empty() {
                        String::new()
                    } else {
                        format!(" — {note}")
                    }
                ));
            }
            bits.extend(c.flags(now, utc));
            if let Some(why) = &c.note.stale_reason {
                bits.push(format!("incomplete - {why}"));
            }
            Ok(format!("{h}: {}", bits.join(" | ")))
        })();
        Some(r.unwrap_or_else(|e| format!("{h}: lookup failed - {}", sqlite_kind(&e))))
    }

    /// Pulls a flagged org's member list in the background.
    pub fn fetch_roster_now(self: &Arc<Self>, sid: Option<&Value>) -> Value {
        let sid = match clean_sid(sid) {
            Ok(s) => s,
            Err(e) => return json!({"ok": false, "error": e}),
        };
        {
            let mut st = self.st();
            if st.roster_busy.contains(&sid) {
                let msg = st
                    .rosters
                    .get(&sid)
                    .cloned()
                    .filter(|m| !m.is_empty())
                    .unwrap_or_else(|| format!("{sid}: already pulling that list"));
                return json!({"ok": true, "sid": sid, "message": msg});
            }
            st.roster_busy.insert(sid.clone());
            st.rosters.set(
                &sid,
                format!("{sid}: pulling the member list, one request a second…"),
            );
        }
        let me = self.clone();
        let s2 = sid.clone();
        (self.hooks.spawn)(Box::new(move || me.roster_work(&s2)));
        let msg = self.st().rosters.get(&sid).cloned().unwrap_or_default();
        json!({"ok": true, "sid": sid, "message": msg})
    }

    fn roster_work(&self, sid: &str) {
        let line = (|| -> Result<String, String> {
            let db = self.open_db()?;
            let client = self.client()?;
            let out = {
                let _w = scw_rsi::why("roster");
                let mut on_page = |page: u32, so_far: usize| {
                    self.st().rosters.set(
                        sid,
                        format!("{sid}: {so_far} member(s) so far, page {page}…"),
                    );
                };
                scw_rsi::fetch_roster(&db, &client, sid, None, &mut on_page)
                    .map_err(|e| sqlite_kind(&e).to_string())?
                    .to_json()
            };
            if out["ok"] != json!(true) {
                return Ok(format!("{sid}: {}", py_value(out.get("error"))));
            }
            let mut bits = vec![format!("{} member(s)", out["members"])];
            for (key, word) in [
                ("joined", "new"),
                ("back", "back on it"),
                ("left", "off the roster since the last pull"),
            ] {
                if let Some(v) = out[key].as_array().filter(|v| !v.is_empty()) {
                    bits.push(format!("{} {word}", v.len()));
                }
            }
            if truthy(out.get("org_members")) {
                bits.push(format!("RSI lists {}", py_value(out.get("org_members"))));
            }
            let hidden = out["hidden"].as_i64().unwrap_or(0);
            let redacted = out["redacted"].as_i64().unwrap_or(0);
            if hidden != 0 || redacted != 0 {
                bits.push(format!("{} hidden or redacted", hidden + redacted));
            }
            if out["source"] == json!("sentry") {
                bits.push("via SENTRY".into());
            }
            if truthy(out.get("switched")) {
                bits.push(
                    "first pull from this source, so nobody was marked as having left".into(),
                );
            }
            Ok(format!(
                "{sid}: {} - reaching {} player(s) you have seen{}",
                bits.join(", "),
                out["reaches"],
                if truthy(out.get("partial")) {
                    "  (the list stopped early, so these are the members seen so far and \
                     nobody was marked as having left)"
                } else {
                    ""
                }
            ))
        })()
        .unwrap_or_else(|e| format!("{sid}: failed - {e}"));
        let mut st = self.st();
        st.rosters.set(sid, line);
        st.roster_busy.remove(sid);
        st.rosters.cap(ROSTERS_MAX);
    }

    /// A member worth asking RSI about: no stored profile, an expired one,
    /// a failed last attempt, or a partial one.
    pub fn needs_lookup(&self, prof: Option<&Map<String, Value>>, failed: bool) -> bool {
        let Some(prof) = prof.filter(|p| !p.is_empty()) else {
            return true;
        };
        if failed || truthy(prof.get("_partial")) {
            return true;
        }
        let ttl = if truthy(Some(prof.get("exists").unwrap_or(&json!(true)))) {
            CITIZEN_TTL
        } else {
            MISS_TTL
        };
        let stored = prof
            .get("_stored_at")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        self.now() - stored > ttl
    }

    /// Looks up, one at a time in the background, every current member of a
    /// flagged org with no usable profile.
    pub fn lookup_members_now(self: &Arc<Self>, sid: &str) -> Result<Value, String> {
        let sid = sid.to_uppercase();
        if let Some(j) = self.st().org_jobs.get(&sid)
            && j.lock().unwrap_or_else(|e| e.into_inner()).running
        {
            return Ok(json!({"ok": true, "sid": sid, "message": "already looking them up"}));
        }
        let db = self.open_db()?;
        let members: Vec<String> = db
            .members_of(&sid)
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|m| m.gone_since.is_none_or(|g| g == 0.0))
            .map(|m| m.handle)
            .collect();
        drop(db);
        let cache = rsi_cache(&self.cache_path(), Some(&members));
        let failures = rsi_failures(&self.cache_path(), &members, self.now());
        let todo: Vec<String> = members
            .into_iter()
            .filter(|h| self.needs_lookup(cache.get(h), failures.contains_key(h)))
            .collect();
        let message = if todo.is_empty() {
            "every current member already has a profile".to_string()
        } else {
            format!(
                "looking up {} member(s), about {} min",
                todo.len(),
                1.max(py_round(todo.len() as f64 * 1.5 / 60.0))
            )
        };
        let job = Arc::new(Mutex::new(Job {
            running: !todo.is_empty(),
            total: todo.len(),
            started: self.now(),
            message: message.clone(),
            ..Job::default()
        }));
        self.st().org_jobs.set(&sid, job.clone());
        if todo.is_empty() {
            return Ok(json!({"ok": true, "sid": sid, "message": message}));
        }
        let me = self.clone();
        let j2 = job.clone();
        (self.hooks.spawn)(Box::new(move || me.members_work(&j2, todo)));
        let now_says = job
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .message
            .clone();
        Ok(json!({"ok": true, "sid": sid, "message": now_says}))
    }

    fn members_work(&self, job: &Arc<Mutex<Job>>, mut todo: Vec<String>) {
        let j = || job.lock().unwrap_or_else(|e| e.into_inner());
        let (Ok(client), Ok(wdb)) = (self.client(), self.open_db()) else {
            j().running = false;
            return;
        };
        let mut waits: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        let mut i = 0;
        while i < todo.len() {
            let h = todo[i].clone();
            i += 1;
            if j().stop {
                let mut g = j();
                g.message = format!("stopped after {} of {}", g.done, g.total);
                g.running = false;
                return;
            }
            let got = {
                let _w = scw_rsi::why("org-members");
                client.citizen(&h, false, true)
            };
            let res = got.and_then(|c| {
                // Our own expired copy standing in: the lookup failed.
                if let Some(stale) = &c.note.stale_reason
                    && c.note.source.is_none()
                {
                    return Err(LookupError::new(Failure::Other {
                        kind: "RuntimeError".into(),
                        message: stale.clone(),
                    }));
                }
                Ok(c)
            });
            match res {
                Ok(c) if c.exists => {
                    {
                        let mut g = j();
                        g.found += 1;
                        if c.note.stale_reason.is_some() {
                            g.incomplete += 1;
                        }
                    }
                    if !c.orgs.is_empty() {
                        let _ = wdb.set_orgs(&h, &orgs_of(&c));
                    }
                    if met(&wdb, &h) {
                        let _ = wdb.update_profile(&h, &c.moniker, &c.enlisted);
                    }
                }
                Ok(_) => j().no_account += 1,
                Err(e) => {
                    if let Failure::ServiceDown { until } | Failure::LiveLimited { until } =
                        e.failure
                    {
                        let n = waits.entry(h.clone()).or_insert(0);
                        *n += 1;
                        if *n <= 3 {
                            let remaining = until - self.clock.monotonic();
                            let who = e.service.unwrap_or("the lookup service");
                            j().message =
                                format!("{who} is not responding - waiting {remaining:.0}s");
                            self.clock.sleep(remaining + 0.5);
                            todo.push(h);
                            continue;
                        }
                    }
                    j().failed += 1;
                    self.note_lookup_failure(&client, &h, &e);
                }
            }
            let mut g = j();
            g.done += 1;
            g.message = format!("looked up {} of {}", g.done, g.total);
        }
        let mut g = j();
        let mut msg = format!("done: {} found", g.found);
        if g.incomplete > 0 {
            msg.push_str(&format!(
                " ({} incomplete - an org's focus unknown, or not confirmed)",
                g.incomplete
            ));
        }
        if g.no_account > 0 {
            msg.push_str(&format!(", {} with no account", g.no_account));
        }
        if g.failed > 0 {
            msg.push_str(&format!(", {} failed - try again later", g.failed));
        }
        g.message = msg;
        g.running = false;
    }

    /// Everyone an org's roster has listed, with what we know about each.
    pub fn org_members(&self, sid: &str) -> Result<Value, String> {
        let sid = sid.to_uppercase();
        let db = self.open_db()?;
        let members = db.members_of(&sid).map_err(|e| e.to_string())?;
        drop(db);
        let handles: Vec<String> = members.iter().map(|m| m.handle.clone()).collect();
        let cache = rsi_cache(&self.cache_path(), Some(&handles));
        let failures = rsi_failures(&self.cache_path(), &handles, self.now());
        let now = self.now();
        let mut out = Vec::new();
        for m in members {
            let gone = m.gone_since.is_some_and(|g| g != 0.0);
            let mut v = match serde_json::to_value(&m) {
                Ok(Value::Object(o)) => o,
                _ => Map::new(),
            };
            let prof = cache.get(&m.handle);
            v.insert(
                "profile".into(),
                match prof.filter(|p| !p.is_empty()) {
                    None => Value::Null,
                    Some(p) => {
                        let stored = p
                            .get("_stored_at")
                            .and_then(Value::as_f64)
                            .filter(|x| *x != 0.0);
                        json!({
                            "exists": truthy(p.get("exists")),
                            "enlisted_days": enlisted_days(&m.handle, p, now, self.hooks.utc),
                            "orgs": p.get("orgs").and_then(Value::as_array).map(|os| os.iter()
                                .filter_map(|o| o.get("sid").filter(|s| truthy(Some(s))).cloned())
                                .collect::<Vec<_>>()).unwrap_or_default(),
                            "age_days": ((now - stored.unwrap_or(now)) / 86400.0) as i64,
                        })
                    }
                },
            );
            v.insert(
                "lookup_failed".into(),
                failures.get(&m.handle).cloned().unwrap_or(Value::Null),
            );
            v.insert(
                "needs_lookup".into(),
                json!(!gone && self.needs_lookup(prof, failures.contains_key(&m.handle))),
            );
            out.push(Value::Object(v));
        }
        let job = self
            .st()
            .org_jobs
            .get(&sid)
            .map(|j| j.lock().unwrap_or_else(|e| e.into_inner()).to_json());
        Ok(json!({"ok": true, "sid": sid, "members": out, "job": job}))
    }

    /// One round of retrying lookups that failed: SENTRY's database first,
    /// then a few live fetches. Counts: checked, live, found.
    pub fn retry_failed_lookups(&self, client: &Client, ask: bool) -> Value {
        let mut got = (0usize, 0usize, 0usize);
        let result = |g: (usize, usize, usize)| json!({"checked": g.0, "live": g.1, "found": g.2});
        let Some(sentry) = client.sentry() else {
            return result(got);
        };
        if !ask || !client.enriches() || sentry.down_for() > 0.0 || sentry.live_paused_for() > 0.0 {
            return result(got);
        }
        let rows = client
            .cache
            .failed_unanswered(self.now() - RETRY_FAILED_AFTER)
            .unwrap_or_default();
        let Ok(db) = self.open_db() else {
            return result(got);
        };
        let now = self.clock.monotonic();
        for h in rows {
            if got.0 >= RETRY_FAILED_CHECKS {
                break;
            }
            let due = self.st().failed_tries.get(&h).copied().unwrap_or(0.0);
            if now < due {
                continue;
            }
            if !met(&db, &h) {
                continue;
            }
            got.0 += 1;
            let Ok(held) = sentry.citizen(&h, false, scw_rsi::SENTRY_TIMEOUT, None, false) else {
                break;
            };
            let absent = sentry_absent(&held);
            let held = if sentry_has(&held) { Some(held) } else { None };
            if held.is_none() && !absent {
                if got.1 >= RETRY_FAILED_LIVE || sentry.live_paused_for() > 0.0 {
                    continue;
                }
                got.1 += 1;
            }
            self.st()
                .failed_tries
                .insert(h.clone(), now + scw_rsi::SENTRY_LIVE_AGAIN);
            match client.citizen(&h, false, true) {
                Err(_) => {
                    if sentry.live_paused_for() > 0.0 {
                        break;
                    }
                    continue;
                }
                Ok(c) => {
                    if c.exists && !c.unverified {
                        if !c.orgs.is_empty() {
                            let _ = db.set_orgs(&h, &orgs_of(&c));
                        }
                        let _ = db.update_profile(&h, &c.moniker, &c.enlisted);
                        got.2 += 1;
                    }
                }
            }
        }
        result(got)
    }

    /// Names for flagged orgs that have none: the cache first, then SENTRY
    /// on rsi.FOCUS_SWEEP_GAPS. How many were named.
    pub fn fill_flag_names(&self, client: &Client, ask: bool) -> usize {
        let Ok(db) = self.open_db() else { return 0 };
        let now = self.clock.monotonic();
        let mut named = 0;
        for sid in db.unnamed_flagged().unwrap_or_default() {
            let mut name = client.org_name(&sid, false).unwrap_or_default();
            let (n, due) = self.st().name_tries.get(&sid).copied().unwrap_or((0, 0.0));
            if name.is_empty() && ask && now >= due {
                let gaps = scw_rsi::FOCUS_SWEEP_GAPS;
                self.st()
                    .name_tries
                    .insert(sid.clone(), (n + 1, now + gaps[n.min(gaps.len() - 1)]));
                name = client.org_name(&sid, true).unwrap_or_default();
            }
            if !name.is_empty() && db.set_flag_name(&sid, &name).unwrap_or(false) {
                self.st().name_tries.remove(&sid);
                named += 1;
            }
        }
        named
    }

    /// One round of the background check (_focus_sweeper's body); the lines
    /// it would print.
    pub fn sweep_once(&self) -> Vec<String> {
        let mut said = Vec::new();
        let ask = self
            .settings()
            .get("rsi_enabled")
            .map(|v| truthy(Some(v)))
            .unwrap_or(true);
        let Ok(client) = self.client() else {
            return said;
        };
        let got = {
            let _w = scw_rsi::why("bg-focus");
            client.complete_partials(ask)
        };
        let Ok(got) = got else { return said };
        if got.completed > 0 || got.filled > 0 {
            said.push(format!(
                "  org focus check: {} profile(s) completed, {} filled in part",
                got.completed, got.filled
            ));
        }
        let named = {
            let _w = scw_rsi::why("bg-names");
            self.fill_flag_names(&client, ask)
        };
        if named > 0 {
            said.push(format!("  org focus check: named {named} flagged org(s)"));
        }
        let again = {
            let _w = scw_rsi::why("bg-retry");
            self.retry_failed_lookups(&client, ask)
        };
        if again["found"].as_u64().unwrap_or(0) > 0 {
            said.push(format!(
                "  background check: {} failed lookup(s) answered ({} needed a live fetch)",
                again["found"], again["live"]
            ));
        }
        said
    }
}

/// A value as Python's str() shows it.
pub fn py_value(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "None".into(),
        Some(v) => scw_store::json::py_str(v),
    }
}

/// An SQLite error as Python names its exception type.
pub fn sqlite_kind(e: &rusqlite::Error) -> &'static str {
    match e {
        rusqlite::Error::SqliteFailure(..) => "OperationalError",
        _ => "DatabaseError",
    }
}
