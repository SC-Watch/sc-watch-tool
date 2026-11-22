//! SENTRY (sentry.wildknightsquadron.com) - `rsi.Sentry`: no key, fast on a
//! hit, every org a person is in. Only a transport and a reader; what its
//! answers are trusted for is decided in [`crate::Client`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use regex::Regex;
use serde_json::{Map, Value};

use crate::cache::Cache;
use crate::clock::Clock;
use crate::error::{Failure, LookupError};
use crate::log::RequestLog;
use crate::model::parse_utc;
use crate::paced::{Pace, jitter};
use crate::transport::{FetchError, Headers, Transport};
use crate::{
    SENTRY_BUDGET_MAX, SENTRY_HOST, SENTRY_LIVE_AGAIN, SENTRY_MAX_PAGES, SENTRY_MEMO_TTL,
    SENTRY_PAGE_SIZE, SENTRY_PAGE_TIMEOUT, SENTRY_RL_LOW, SENTRY_RL_MAX_GAP, SENTRY_STUB_MEMO_TTL,
    SENTRY_TIMEOUT, SENTRY_USER_AGENT, quote,
};
use scw_store::json::{py_str, py_strip, truthy};
use scw_store::reputation::RosterMember;

pub const SERVICE: &str = "SENTRY";
/// Its rows in the cache's `pauses`: the live-lookup pause, the next send.
pub const LIVE_PAUSE: &str = "sentry_live";
pub const SLOT: &str = "sentry_slot";

/// What SENTRY said about a player.
#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    /// It has nothing (a 404, or `not_checked`): silence, not absence.
    Nobody,
    /// RSI has no public profile for the handle - SENTRY asked.
    NotOnRsi,
    /// Its record (a profile, a placeholder, or whatever it sent).
    Record(Value),
}

impl Answer {
    fn truthy(&self) -> bool {
        match self {
            Answer::Record(v) => truthy(Some(v)),
            _ => false,
        }
    }

    pub fn record(&self) -> Option<&Map<String, Value>> {
        match self {
            Answer::Record(Value::Object(m)) => Some(m),
            _ => None,
        }
    }
}

fn sub<'a>(m: &'a Map<String, Value>, k: &str) -> Option<&'a Map<String, Value>> {
    m.get(k)
        .filter(|v| truthy(Some(v)))
        .and_then(Value::as_object)
}

/// A real, current profile to take as a player: not a placeholder
/// (`is_stub`), not an archived copy - `sentry_has()`.
pub fn sentry_has(a: &Answer) -> bool {
    let Some(d) = a.record() else { return false };
    if !truthy(d.get("profile")) {
        return false;
    }
    let meta = sub(d, "metadata");
    let prof = sub(d, "profile");
    !(meta.is_some_and(|m| truthy(m.get("is_stub")))
        || prof.is_some_and(|p| truthy(p.get("is_stub")))
        || meta.is_some_and(|m| truthy(m.get("is_archived"))))
}

/// Is the record about THIS handle - not a rename it followed?
pub fn is_answer_for(a: &Answer, handle: &str) -> bool {
    let d = match a {
        Answer::Record(Value::Object(m)) => m,
        Answer::Record(v) if truthy(Some(v)) => return false,
        _ => return true, // nothing to say otherwise: `(d or {})`
    };
    let shown = sub(d, "profile")
        .and_then(|p| p.get("handle"))
        .filter(|h| truthy(Some(h)))
        .map(py_str)
        .unwrap_or_else(|| handle.to_string());
    py_strip(&shown).to_lowercase() == py_strip(handle).to_lowercase()
        && !truthy(d.get("redirected_from"))
}

pub fn is_placeholder(a: &Answer) -> bool {
    let Some(d) = a.record() else { return false };
    sub(d, "metadata").is_some_and(|m| truthy(m.get("is_stub")))
        || sub(d, "profile").is_some_and(|p| truthy(p.get("is_stub")))
}

/// A placeholder's (lookup_state, retry_after_hint).
pub fn placeholder_state(a: &Answer) -> (Option<String>, Option<f64>) {
    if !is_placeholder(a) {
        return (None, None);
    }
    let meta = a.record().and_then(|d| sub(d, "metadata"));
    let hint = meta
        .and_then(|m| m.get("retry_after_hint"))
        .filter(|h| !h.is_null())
        .and_then(|h| match h {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => py_strip(s).parse().ok(),
            Value::Bool(b) => Some(*b as u8 as f64),
            _ => None,
        });
    let state = meta
        .and_then(|m| m.get("lookup_state"))
        .filter(|s| truthy(Some(s)))
        .map(|s| py_str(s).to_lowercase())
        .filter(|s| !s.is_empty());
    (state, hint)
}

/// SENTRY saying RSI has no public profile: NotOnRsi, or a placeholder
/// whose state is not_found.
pub fn sentry_absent(a: &Answer) -> bool {
    *a == Answer::NotOnRsi || placeholder_state(a).0.as_deref() == Some("not_found")
}

/// (requests left, seconds until there is room) from SENTRY's headers.
pub fn rate_left(headers: Option<&Headers>) -> Option<(i64, f64)> {
    static R: std::sync::OnceLock<(Regex, Regex)> = std::sync::OnceLock::new();
    let (r_re, t_re) = R.get_or_init(|| {
        (
            Regex::new(r"\br=(\d+)").unwrap(),
            Regex::new(r"\bt=(\d+)").unwrap(),
        )
    });
    let h = headers?;
    let text = h.get("RateLimit").unwrap_or("");
    if let (Some(r), Some(t)) = (r_re.captures(text), t_re.captures(text)) {
        return Some((r[1].parse().ok()?, t[1].parse().ok()?));
    }
    let left = py_strip(h.get("RateLimit-Remaining")?).parse().ok()?;
    let reset = py_strip(h.get("RateLimit-Reset")?).parse().ok()?;
    Some((left, reset))
}

/// Seconds a 429 or 503 says to wait (at most two hours), or None.
pub(crate) fn retry_after(headers: &Headers) -> Option<f64> {
    let v: f64 = match headers.get("Retry-After") {
        None | Some("") => 0.0,
        Some(s) => py_strip(s).parse().ok()?,
    };
    (v > 0.0).then(|| v.min(7200.0))
}

/// What one request brought: the payload (None for a 404) and, for a 404,
/// which one (`X-Lookup-State`).
pub(crate) struct Called {
    pub payload: Option<Value>,
    pub lookup_state: Option<String>,
}

struct State {
    pace: Pace,
    /// Monotonic; see LiveLimited.
    live_until: f64,
    /// HANDLE -> when it last had a live lookup.
    live_asked: HashMap<String, f64>,
    /// SID -> until when it is not asked again.
    org_refused: HashMap<String, f64>,
    /// (kind, key) -> (when, answer, asked live); kind 'c' a player, 'o' an
    /// org's profile.
    memo: HashMap<(char, String), (f64, Answer, bool)>,
    /// From its RateLimit header: the gap to keep, until when.
    rl_gap: f64,
    rl_until: f64,
}

pub struct Sentry {
    state: Mutex<State>,
    cache: Option<Arc<Cache>>,
    clock: Arc<dyn Clock>,
    transport: Arc<dyn Transport>,
    log: Option<Arc<RequestLog>>,
}

impl Sentry {
    pub fn new(
        min_interval: f64,
        cache: Option<Arc<Cache>>,
        clock: Arc<dyn Clock>,
        transport: Arc<dyn Transport>,
        log: Option<Arc<RequestLog>>,
    ) -> Self {
        Sentry {
            state: Mutex::new(State {
                pace: Pace::new(min_interval),
                live_until: 0.0,
                live_asked: HashMap::new(),
                org_refused: HashMap::new(),
                memo: HashMap::new(),
                rl_gap: 0.0,
                rl_until: 0.0,
            }),
            cache,
            clock,
            transport,
            log,
        }
    }

    fn st(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn mono(&self) -> f64 {
        self.clock.monotonic()
    }

    /// Seconds until lookups resume; 0 if not paused.
    pub fn down_for(&self) -> f64 {
        self.st().pace.down_for(self.mono())
    }

    /// For tests and diagnostics: (told-to-wait left, live-limit left)
    /// without reading the stored pause.
    pub fn waits(&self) -> (f64, f64) {
        let m = self.mono();
        let s = self.st();
        (
            (s.pace.told_until - m).max(0.0),
            (s.live_until - m).max(0.0),
        )
    }

    fn recall(&self, kind: char, key: &str) -> Option<(f64, Answer, bool)> {
        let s = self.st();
        let m = s.memo.get(&(kind, key.to_string()))?;
        let ttl = if is_placeholder(&m.1) {
            SENTRY_STUB_MEMO_TTL
        } else {
            SENTRY_MEMO_TTL
        };
        (self.mono() - m.0 < ttl).then(|| m.clone())
    }

    fn remember(&self, kind: char, key: &str, answer: Answer, live: bool) {
        let now = self.mono();
        let mut s = self.st();
        s.memo.insert((kind, key.to_string()), (now, answer, live));
        if s.memo.len() > 2000 {
            s.memo.retain(|_, m| now - m.0 < SENTRY_MEMO_TTL);
        }
    }

    /// Drops a remembered answer for a player - its re-read has landed.
    pub fn forget(&self, handle: &str) {
        self.st().memo.remove(&('c', handle.to_uppercase()));
    }

    /// One pace for every process: the next turn is booked in the cache,
    /// where the watcher and the page both see it.
    fn wait_turn(&self) {
        let now_m = self.mono();
        let (gap, min_interval) = {
            let s = self.st();
            (
                if now_m < s.rl_until { s.rl_gap } else { 0.0 },
                s.pace.min_interval,
            )
        };
        let own = |gap: f64| {
            let slot = self.st().pace.book(self.clock.time(), gap);
            let wait = slot - self.clock.time();
            if wait > 0.0 {
                self.clock.sleep(wait);
            }
        };
        let Some(cache) = self.cache.as_ref().filter(|_| min_interval > 0.0) else {
            own(if gap > min_interval {
                gap
            } else {
                min_interval
            });
            return;
        };
        match cache.book_slot(SLOT, min_interval.max(gap)) {
            Ok(slot) => {
                let wait = (slot - self.clock.time()).min(10.0);
                if wait > 0.0 {
                    self.clock.sleep(wait);
                }
            }
            Err(_) => own(min_interval),
        }
    }

    fn note_rate(&self, headers: &Headers) {
        let Some((left, reset)) = rate_left(Some(headers)) else {
            return;
        };
        let now = self.mono();
        let mut s = self.st();
        if left <= 0 && reset > 0.0 {
            let j = jitter(&*self.clock, reset);
            s.pace.down_until = s.pace.down_until.max(now + j);
            s.pace.told_until = s.pace.told_until.max(s.pace.down_until);
            (s.rl_gap, s.rl_until) = (0.0, 0.0);
        } else if left <= SENTRY_RL_LOW && reset > 0.0 {
            s.rl_gap = (reset / left as f64).min(SENTRY_RL_MAX_GAP);
            s.rl_until = now + reset;
        } else {
            (s.rl_gap, s.rl_until) = (0.0, 0.0);
        }
    }

    /// Seconds until live lookups may be asked for again; 0 if now. Another
    /// process may have been told, so the stored pause is read too.
    pub fn live_paused_for(&self) -> f64 {
        if let Some(cache) = &self.cache {
            let left = cache
                .pause_until(LIVE_PAUSE)
                .map(|u| u - self.clock.time())
                .unwrap_or(0.0);
            if left > 0.0 {
                let m = self.mono();
                let mut s = self.st();
                s.live_until = s.live_until.max(m + left);
            }
        }
        (self.st().live_until - self.mono()).max(0.0)
    }

    fn live_told(&self, seconds: f64) {
        let seconds = jitter(&*self.clock, seconds);
        {
            let m = self.mono();
            let mut s = self.st();
            s.live_until = s.live_until.max(m + seconds);
        }
        if let Some(cache) = &self.cache {
            let _ = cache.set_pause(LIVE_PAUSE, self.clock.time() + seconds);
        }
    }

    fn live_limited(&self) -> LookupError {
        LookupError::new(Failure::LiveLimited {
            until: self.st().live_until,
        })
        .tagged(SERVICE)
    }

    fn log(
        &self,
        path: &str,
        live: bool,
        status: &str,
        started: f64,
        payload: Option<&Value>,
        headers: Option<&Headers>,
    ) {
        if let Some(log) = &self.log {
            log.note(&*self.clock, path, live, status, started, payload, headers);
        }
    }

    /// One request: a parsed answer, or None for a 404. `live` is a request
    /// that can make SENTRY read RSI on the spot; `gate` false sends it even
    /// while live lookups are paused.
    pub(crate) fn call(
        &self,
        path: &str,
        timeout: f64,
        force: bool,
        live: bool,
        gate: bool,
    ) -> Result<Called, LookupError> {
        self.st().pace.check_paused(force, self.mono(), SERVICE)?;
        if live && gate && self.live_paused_for() > 0.0 {
            self.log(path, live, "not-sent", self.mono(), None, None);
            return Err(self.live_limited());
        }
        self.wait_turn();
        let started = self.mono();
        match self
            .transport
            .get(&format!("{SENTRY_HOST}{path}"), SENTRY_USER_AGENT, timeout)
        {
            Ok(reply) => {
                self.log(
                    path,
                    live,
                    "200",
                    started,
                    Some(&reply.body),
                    Some(&reply.headers),
                );
                self.st().pace.went_ok();
                self.note_rate(&reply.headers);
                Ok(Called {
                    payload: Some(reply.body),
                    lookup_state: None,
                })
            }
            Err(FetchError::Http { code, headers }) => {
                self.log(path, live, &code.to_string(), started, None, Some(&headers));
                if code == 404 {
                    let state = headers
                        .get("X-Lookup-State")
                        .map(|s| py_strip(s).to_lowercase())
                        .filter(|s| !s.is_empty());
                    self.st().pace.went_ok();
                    self.note_rate(&headers);
                    return Ok(Called {
                        payload: None,
                        lookup_state: state,
                    });
                }
                let err = LookupError::from(FetchError::Http {
                    code,
                    headers: headers.clone(),
                });
                if err.is_transient() {
                    let retry = if matches!(code, 429 | 503) {
                        retry_after(&headers)
                    } else {
                        None
                    };
                    if code == 429 && live && retry.unwrap_or(0.0) > SENTRY_BUDGET_MAX {
                        self.live_told(retry.unwrap());
                        return Err(self.live_limited());
                    }
                    self.st().pace.went_wrong(retry, &*self.clock);
                }
                Err(err.tagged(SERVICE))
            }
            Err(e) => {
                let status = match &e {
                    FetchError::Timeout => "timeout".to_string(),
                    FetchError::Connect(m) if m.to_lowercase().contains("timed out") => {
                        "timeout".into()
                    }
                    FetchError::Connect(_) => "URLError".into(),
                    FetchError::BadJson => "JSONDecodeError".into(),
                    FetchError::Http { .. } => unreachable!(),
                };
                self.log(path, live, &status, started, None, None);
                let err = LookupError::from(e);
                if err.is_transient() {
                    self.st().pace.went_wrong(None, &*self.clock);
                }
                Err(err.tagged(SERVICE))
            }
        }
    }

    /// SENTRY's record of a citizen. `max_age` asks it to re-read RSI if
    /// its copy is older; `live` false asks its database only - as does any
    /// ask while live lookups are limited, or for a handle that had one
    /// lately (unless `force`, a person pressing try now).
    pub fn citizen(
        &self,
        handle: &str,
        force: bool,
        timeout: f64,
        max_age: Option<f64>,
        mut live: bool,
    ) -> Result<Answer, LookupError> {
        let key = handle.to_uppercase();
        if live {
            let now = self.mono();
            let last = self.st().live_asked.get(&key).copied();
            if self.live_paused_for() > 0.0
                || (!force && last.is_some_and(|t| now - t < SENTRY_LIVE_AGAIN))
            {
                live = false;
            }
        }
        if !force && let Some((_, d, was_live)) = self.recall('c', &key) {
            match &d {
                Answer::NotOnRsi => return Ok(d),
                _ if is_placeholder(&d) => return Ok(d),
                Answer::Record(v) => {
                    let Some(m) = v.as_object() else {
                        // rsi.py reads `.get` off whatever it kept.
                        return Err(LookupError::other("AttributeError", "not a record"));
                    };
                    let fetched = sub(m, "metadata")
                        .and_then(|x| x.get("last_fetched"))
                        .filter(|v| truthy(Some(v)));
                    let seen = parse_utc(
                        fetched.or(sub(m, "profile").and_then(|p| p.get("last_fetched"))),
                    )
                    .filter(|s| *s != 0.0);
                    let fresh_enough = match max_age {
                        None => true,
                        Some(a) => seen.is_some_and(|s| self.clock.time() - s <= a),
                    };
                    if fresh_enough {
                        return Ok(d);
                    }
                }
                Answer::Nobody if was_live || !live => return Ok(Answer::Nobody),
                Answer::Nobody => {}
            }
        }
        let mut params = Vec::new();
        if let Some(a) = max_age {
            params.push(format!("max_age={}", (a as i64).max(0)));
        }
        if !live {
            params.push("scrape=false".into());
        }
        let mut path = format!("/citizens/{}", quote(handle));
        if !params.is_empty() {
            path = format!("{path}?{}", params.join("&"));
        }
        let called = match self.call(&path, timeout, force, live, true) {
            Ok(c) => c,
            Err(e) if matches!(e.failure, Failure::LiveLimited { .. }) => {
                return self.citizen(handle, force, timeout, max_age, false);
            }
            Err(e) => {
                if live {
                    self.note_live(&key); // timed out: it may still be reading
                }
                return Err(e);
            }
        };
        let d = match called.payload {
            Some(v) => Answer::Record(v),
            None if called.lookup_state.as_deref() == Some("not_found") => Answer::NotOnRsi,
            None => Answer::Nobody,
        };
        if live && !d.truthy() {
            self.note_live(&key);
        }
        self.remember('c', &key, d.clone(), live);
        Ok(d)
    }

    fn note_live(&self, key: &str) {
        let now = self.mono();
        let mut s = self.st();
        s.live_asked.insert(key.to_string(), now);
        if s.live_asked.len() > 5000 {
            s.live_asked.retain(|_, t| now - *t < SENTRY_LIVE_AGAIN);
        }
    }

    /// When SENTRY last read this player from RSI - the light way to see
    /// whether a refresh has landed.
    pub fn citizen_fetched(&self, handle: &str) -> Result<Option<f64>, LookupError> {
        let got = self.call(
            &format!("/citizens/{}/metadata", quote(handle)),
            SENTRY_TIMEOUT,
            false,
            false,
            true,
        )?;
        Ok(match got.payload {
            Some(Value::Object(m)) => parse_utc(m.get("last_fetched")),
            _ => None,
        })
    }

    /// An org's profile - name, member count, focus - or None if SENTRY has
    /// no such org. Asked even while live lookups are paused; an org it
    /// refuses is not asked again until the pause is over.
    pub fn org(
        &self,
        sid: &str,
        max_age: Option<f64>,
        force: bool,
    ) -> Result<Option<Map<String, Value>>, LookupError> {
        let key = sid.to_uppercase();
        let refused = self.st().org_refused.get(&key).copied().unwrap_or(0.0);
        if self.mono() < refused {
            return Err(LookupError::new(Failure::LiveLimited { until: refused }).tagged(SERVICE));
        }
        if max_age.is_none()
            && !force
            && let Some((_, a, _)) = self.recall('o', &key)
        {
            return Ok(a.record().cloned());
        }
        let q = max_age
            .map(|a| format!("?max_age={}", (a as i64).max(0)))
            .unwrap_or_default();
        let called = match self.call(
            &format!("/orgs/{}/profile{q}", quote(sid)),
            SENTRY_TIMEOUT,
            false,
            true,
            false,
        ) {
            Ok(c) => c,
            Err(e) => {
                if matches!(e.failure, Failure::LiveLimited { .. }) {
                    let until = self.st().live_until;
                    self.st().org_refused.insert(key, until);
                }
                return Err(e);
            }
        };
        let prof = match called.payload {
            Some(Value::Object(d)) => match d.get("profile") {
                Some(Value::Object(p)) => Some(p.clone()),
                Some(_) => None,
                None => Some(d),
            },
            _ => None,
        };
        let kept = prof
            .clone()
            .map_or(Answer::Nobody, |p| Answer::Record(Value::Object(p)));
        self.remember('o', &key, kept, true);
        Ok(prof)
    }

    /// An org's member list, walked to the end; None if SENTRY has no such
    /// org. Hidden and redacted members are counted, never listed.
    pub fn roster(
        &self,
        sid: &str,
        on_page: &mut dyn FnMut(u32, usize),
    ) -> Result<Option<crate::Roster>, LookupError> {
        let mut out: Vec<RosterMember> = Vec::new();
        let (mut hidden, mut redacted) = (0i64, 0i64);
        let mut total: Option<Value> = None;
        let mut observed: Option<f64> = None;
        let (mut page, mut pages) = (1u32, 1i64);
        let partial = |out: Vec<RosterMember>, total: Option<Value>, hidden, redacted, observed| {
            Some(crate::Roster {
                members: out,
                complete: false,
                source: Some("sentry"),
                total,
                hidden,
                redacted,
                observed_at: observed,
            })
        };
        while page as i64 <= pages {
            if page > SENTRY_MAX_PAGES {
                return Ok(partial(out, total, hidden, redacted, observed));
            }
            let d = self
                .call(
                    &format!(
                        "/orgs/{}/memberships?per_page={SENTRY_PAGE_SIZE}&page={page}",
                        quote(sid)
                    ),
                    SENTRY_PAGE_TIMEOUT,
                    false,
                    true,
                    false,
                )?
                .payload;
            let Some(d) = d else {
                return Ok(if page == 1 {
                    None
                } else {
                    partial(out, total, hidden, redacted, observed)
                });
            };
            let empty = Map::new();
            let m = match &d {
                Value::Object(o) => match o.get("memberships") {
                    Some(Value::Object(m)) => m,
                    Some(_) => return Ok(partial(out, total, hidden, redacted, observed)),
                    None => o,
                },
                _ => &empty,
            };
            pages = match m.get("pages").filter(|v| truthy(Some(v))) {
                None => 1,
                Some(v) => crate::client::py_int(v)?,
            };
            if let Some(t) = m.get("total") {
                total = Some(t.clone());
            }
            if observed.is_none() {
                let meta = d.as_object().and_then(|o| sub(o, "metadata"));
                observed = parse_utc(meta.and_then(|x| x.get("last_fetched")));
            }
            let rows = m
                .get("members")
                .filter(|v| truthy(Some(v)))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for r in &rows {
                let r = r
                    .as_object()
                    .ok_or_else(|| LookupError::other("AttributeError", "not a member"))?;
                let text = |k: &str| {
                    r.get(k)
                        .filter(|v| truthy(Some(v)))
                        .map(py_str)
                        .unwrap_or_default()
                };
                let vis = text("visibility");
                let vis = if vis.is_empty() {
                    "visible".to_string()
                } else {
                    vis.to_lowercase()
                };
                let h = py_strip(&text("citizen_handle")).to_string();
                if vis == "hidden" {
                    hidden += 1;
                } else if vis != "visible" || h.is_empty() || h.starts_with('[') {
                    redacted += 1;
                } else {
                    out.push(RosterMember {
                        handle: h,
                        display: text("display_name"),
                        rank: text("member_rank"),
                        stars: r
                            .get("rank_number")
                            .filter(|v| truthy(Some(v)))
                            .and_then(Value::as_i64)
                            .unwrap_or(0),
                    });
                }
            }
            on_page(page, out.len());
            page += 1;
        }
        Ok(Some(crate::Roster {
            members: out,
            complete: true,
            source: Some("sentry"),
            total,
            hidden,
            redacted,
            observed_at: observed,
        }))
    }
}
