//! `rsi.Client`: profile lookup - SENTRY first, starcitizen-api.com behind
//! it - with the cache, and what each source is trusted for (see rsi.py's
//! `citizen()`: SENTRY for who someone is and their orgs; "no such account"
//! only from a source that asked RSI).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::Serialize;
use serde_json::{Map, Value};

use crate::cache::Cache;
use crate::clock::Clock;
use crate::error::{Failure, LookupError, minutes};
use crate::log::RequestLog;
use crate::model::{Citizen, Org, Source, parse_utc};
use crate::paced::Pace;
use crate::sentry::{
    self, Answer, Sentry, is_answer_for, placeholder_state, sentry_absent, sentry_has,
};
use crate::transport::{FetchError, Transport};
use crate::{
    API_HOST, ARCHETYPE_TIMEOUT, CITIZEN_TTL, DEFAULT_MODE, EXISTS_TIMEOUT, FOCUS_SWEEP_GAPS,
    FRESH_WINDOW, MEMBER_PAGE_CAP, MIN_INTERVAL, SENTRY_CHECK_STATES, SENTRY_CHECK_WAIT_MIN,
    SENTRY_REFRESH_FIRST, SENTRY_REFRESH_POLL, SENTRY_REFRESH_WAIT, SENTRY_REREAD_AGAIN,
    SENTRY_TIMEOUT, USER_AGENT, is_ship_serial, quote, why,
};
use scw_store::json::{py_str, py_strip, truthy};
use scw_store::reputation::RosterMember;

pub const SERVICE: &str = "starcitizen-api.com";

/// One pull of an org's member list, and what can be concluded from it.
#[derive(Clone, Debug, PartialEq)]
pub struct Roster {
    /// Visible members only.
    pub members: Vec<RosterMember>,
    /// The walk reached the end of the list.
    pub complete: bool,
    /// "sentry" | "sci" - rosters from different sources are never compared.
    pub source: Option<&'static str>,
    /// The org's own count, hidden members included, as SENTRY gave it.
    pub total: Option<Value>,
    pub hidden: i64,
    pub redacted: i64,
    pub observed_at: Option<f64>,
}

/// What complete_partials() did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub completed: u32,
    pub filled: u32,
    pub asked: u32,
    pub found: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    /// Look people up: SENTRY first, then starcitizen-api.com with a key.
    Api,
    /// No network: the handle, and a URL to look it up by hand.
    Link,
    /// Disabled.
    None,
}

pub struct Options {
    pub api_key: Option<String>,
    /// starcitizen-api.com's mode: "auto", "cache" or "live".
    pub mode: String,
    pub min_interval: f64,
    pub provider: Provider,
    /// SENTRY first (`rsi_source` = "sentry"), or the old service only.
    pub sentry: bool,
    pub log: Option<Arc<RequestLog>>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            api_key: None,
            mode: DEFAULT_MODE.into(),
            min_interval: MIN_INTERVAL,
            provider: Provider::Api,
            sentry: true,
            log: None,
        }
    }
}

/// SENTRY is checking RSI for a name it has never seen - its placeholder:
/// an answer on its way, neither a failure nor "no such account".
#[derive(Clone, Debug, PartialEq)]
pub struct Checking {
    pub state: String,
    pub hint: Option<f64>,
}

impl Checking {
    pub fn reason(&self) -> &'static str {
        match self.state.as_str() {
            "check_failed" => "SENTRY could not check RSI for them just now",
            "queued_low" => "SENTRY is checking RSI for them, behind other new names",
            _ => "SENTRY is checking RSI for them",
        }
    }
}

/// What SENTRY's record of a player came to.
enum Sought {
    Found(Box<Citizen>),
    Nobody,
    Checking(Checking),
}

#[derive(Default)]
struct State {
    /// Orgs whose archetype the other service has been asked for this run.
    archetype_tried: HashSet<String>,
    /// Orgs SENTRY has been asked to re-read, and when (monotonic).
    sentry_reading: HashMap<String, f64>,
    /// SID -> (times asked, when next due), for complete_partials().
    focus_tries: HashMap<String, (usize, f64)>,
}

pub struct Client {
    pub cache: Arc<Cache>,
    api_key: Option<String>,
    mode: String,
    provider: Provider,
    pace: Mutex<Pace>,
    sentry: Option<Sentry>,
    clock: Arc<dyn Clock>,
    transport: Arc<dyn Transport>,
    state: Mutex<State>,
    /// Lookups being answered now, so a second ask waits for the first.
    flights: Mutex<HashMap<(String, bool, bool), Arc<Flight>>>,
}

/// One lookup in flight; whoever asks the same meanwhile waits for it.
#[derive(Default)]
struct Flight {
    answer: Mutex<Option<Result<Citizen, LookupError>>>,
    done: std::sync::Condvar,
}

/// Lands an answer even if the lookup panicked, so no one waits forever.
struct Landing<'a> {
    client: &'a Client,
    key: (String, bool, bool),
    flight: Arc<Flight>,
    answer: Option<Result<Citizen, LookupError>>,
}

impl Drop for Landing<'_> {
    fn drop(&mut self) {
        let answer = self
            .answer
            .take()
            .unwrap_or_else(|| Err(LookupError::other("RuntimeError", "the lookup stopped")));
        *self.flight.answer.lock().unwrap_or_else(|e| e.into_inner()) = Some(answer);
        self.flight.done.notify_all();
        self.client
            .flights
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
    }
}

/// Python's `int(value)` for what an API can send.
pub(crate) fn py_int(v: &Value) -> Result<i64, LookupError> {
    match v {
        Value::Bool(b) => Ok(*b as i64),
        Value::Number(n) => n
            .as_i64()
            .or_else(|| {
                n.as_f64()
                    .filter(|f| f.is_finite())
                    .map(|f| f.trunc() as i64)
            })
            .ok_or_else(|| LookupError::other("OverflowError", "cannot convert")),
        Value::String(s) => py_strip(s).replace('_', "").parse().map_err(|_| {
            LookupError::other("ValueError", format!("invalid literal for int(): {s:?}"))
        }),
        _ => Err(LookupError::other("TypeError", "int() argument")),
    }
}

/// `x or default` for a text field: a value that is not text is read as
/// its text.
fn text(m: &Map<String, Value>, k: &str) -> String {
    m.get(k)
        .filter(|v| truthy(Some(v)))
        .map(py_str)
        .unwrap_or_default()
}

fn sub<'a>(m: &'a Map<String, Value>, k: &str) -> Option<&'a Map<String, Value>> {
    m.get(k)
        .filter(|v| truthy(Some(v)))
        .and_then(Value::as_object)
}

fn attribute_error() -> LookupError {
    LookupError::other("AttributeError", "an answer of the wrong shape")
}

/// Every RSI org declares a primary focus, so a blank one is a field not
/// filled in - never "no focus".
fn no_focus(p: &Map<String, Value>) -> bool {
    py_strip(&text(p, "primary_activity")).is_empty()
        && py_strip(&text(p, "secondary_activity")).is_empty()
}

impl Client {
    pub fn new(
        cache: Arc<Cache>,
        transport: Arc<dyn Transport>,
        clock: Arc<dyn Clock>,
        opts: Options,
    ) -> Self {
        let sentry = opts.sentry.then(|| {
            Sentry::new(
                opts.min_interval,
                Some(cache.clone()),
                clock.clone(),
                transport.clone(),
                opts.log.clone(),
            )
        });
        Client {
            cache,
            api_key: opts.api_key.filter(|k| !k.is_empty()),
            mode: opts.mode,
            provider: opts.provider,
            pace: Mutex::new(Pace::new(opts.min_interval)),
            sentry,
            clock,
            transport,
            state: Mutex::new(State::default()),
            flights: Mutex::new(HashMap::new()),
        }
    }

    fn st(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn pace(&self) -> MutexGuard<'_, Pace> {
        self.pace.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn now(&self) -> f64 {
        self.clock.time()
    }

    fn mono(&self) -> f64 {
        self.clock.monotonic()
    }

    pub fn sentry(&self) -> Option<&Sentry> {
        self.sentry.as_ref()
    }

    pub fn ready(&self) -> bool {
        match self.provider {
            Provider::None => false,
            Provider::Link => true,
            Provider::Api => self.api_key.is_some() || self.sentry.is_some(),
        }
    }

    /// Whether lookups return org data, as opposed to just a link.
    pub fn enriches(&self) -> bool {
        self.provider == Provider::Api && (self.api_key.is_some() || self.sentry.is_some())
    }

    /// Can starcitizen-api.com be asked - the fallback, and the only source
    /// of an org's archetype?
    pub fn has_key(&self) -> bool {
        self.provider == Provider::Api && self.api_key.is_some()
    }

    /// Seconds until starcitizen-api.com is asked again; 0 if not paused.
    pub fn down_for(&self) -> f64 {
        self.pace().down_for(self.mono())
    }

    /// For tests and diagnostics: (failures in a row, told-to-wait left).
    pub fn trouble(&self) -> (u32, f64) {
        let m = self.mono();
        let p = self.pace();
        (p.trouble, (p.told_until - m).max(0.0))
    }

    /// The failure's wording, now.
    pub fn describe(&self, e: &LookupError) -> String {
        e.describe(self.mono())
    }

    // ---- starcitizen-api.com ---------------------------------------------------

    /// One rate-limited request with the breaker applied: the whole payload,
    /// or None for a 404.
    fn call(
        &self,
        path: &str,
        mode: Option<&str>,
        force: bool,
        timeout: Option<f64>,
    ) -> Result<Option<Value>, LookupError> {
        let Some(key) = &self.api_key else {
            return Err(LookupError::new(Failure::NoKey));
        };
        self.pace().check_paused(force, self.mono(), SERVICE)?;
        let slot = {
            let mut p = self.pace();
            let gap = p.min_interval;
            p.book(self.now(), gap)
        };
        let wait = slot - self.now();
        if wait > 0.0 {
            self.clock.sleep(wait);
        }
        let url = format!("{API_HOST}/{key}/v1/{}/{path}", mode.unwrap_or(&self.mode));
        match self
            .transport
            .get(&url, USER_AGENT, timeout.unwrap_or(25.0))
        {
            Ok(reply) => {
                self.pace().went_ok();
                Ok(Some(reply.body))
            }
            Err(FetchError::Http { code: 404, .. }) => {
                self.pace().went_ok();
                Ok(None)
            }
            Err(e) => {
                let retry = match &e {
                    FetchError::Http {
                        code: 429 | 503,
                        headers,
                    } => sentry::retry_after(headers),
                    _ => None,
                };
                let err = LookupError::from(e);
                if err.is_transient() {
                    self.pace().went_wrong(retry, &*self.clock);
                }
                Err(err.tagged(SERVICE))
            }
        }
    }

    /// A record's `data`, or None for a 404 or "success:0" or nothing.
    fn get(&self, path: &str, force: bool) -> Result<Option<Value>, LookupError> {
        let Some(payload) = self.call(path, None, force, None)? else {
            return Ok(None);
        };
        let p = payload.as_object().ok_or_else(attribute_error)?;
        let data = p.get("data");
        let empty = match data {
            None | Some(Value::Null) => true,
            Some(Value::Array(a)) => a.is_empty(),
            Some(Value::Object(o)) => o.is_empty(),
            _ => false,
        };
        if !truthy(p.get("success")) || empty {
            return Ok(None);
        }
        Ok(data.cloned())
    }

    // ---- orgs -----------------------------------------------------------------

    /// An org with its focus: our cache, then SENTRY, then the other
    /// service, then an expired copy.
    pub fn org(&self, sid: &str, force: bool) -> Result<Org, LookupError> {
        if let Some(mut cached) = self.cache.get_org(sid, false)? {
            self.fill_archetype(&mut cached, true);
            return Ok(cached);
        }
        if self.sentry.is_some() {
            // Counted by SENTRY's own breaker; the other is asked.
            if let Ok(Some(got)) = self.org_sentry(sid, true, force) {
                self.cache.put_org(&got, None)?;
                return Ok(got);
            }
        }
        let data = match self.get(&format!("organization/{}", quote(sid)), force) {
            Ok(d) => d,
            Err(e) => {
                if !force && let Some(old) = self.cache.get_org(sid, true)? {
                    return Ok(old);
                }
                return Err(e);
            }
        };
        let mut org = Org::new(&sid.to_uppercase());
        if let Some(data) = data.filter(|d| truthy(Some(d))) {
            let d = data.as_object().ok_or_else(attribute_error)?;
            let empty = Map::new();
            let focus = sub(d, "focus").unwrap_or(&empty);
            org.name = d
                .get("name")
                .filter(|v| truthy(Some(v)))
                .map(py_str)
                .unwrap_or_else(|| sid.to_string());
            org.members = match d.get("members").filter(|v| truthy(Some(v))) {
                Some(v) => py_int(v)?,
                None => 0,
            };
            org.archetype = text(d, "archetype");
            org.commitment = text(d, "commitment");
            org.primary_focus = sub(focus, "primary")
                .map(|p| text(p, "name"))
                .unwrap_or_default();
            org.secondary_focus = sub(focus, "secondary")
                .map(|p| text(p, "name"))
                .unwrap_or_default();
        }
        self.cache.put_org(&org, None)?;
        Ok(org)
    }

    /// SENTRY's org WITH its focus, or None if it cannot give the focus. A
    /// profile without one is asked to be re-read (max_age=0), once per
    /// SENTRY_REREAD_AGAIN, and remembered as on its way.
    fn org_sentry(
        &self,
        sid: &str,
        archetype: bool,
        force: bool,
    ) -> Result<Option<Org>, LookupError> {
        let Some(s) = &self.sentry else {
            return Ok(None);
        };
        let key = sid.to_uppercase();
        let Some(mut p) = s.org(sid, None, force)? else {
            self.st().sentry_reading.remove(&key);
            return Ok(None);
        };
        if no_focus(&p) {
            let asked = self.st().sentry_reading.get(&key).copied();
            if asked.is_some_and(|a| self.mono() - a < SENTRY_REREAD_AGAIN) {
                return Ok(None);
            }
            match s.org(sid, Some(0.0), false)? {
                Some(again) if !no_focus(&again) => p = again,
                again => {
                    if again.is_some() {
                        let m = self.mono();
                        self.st().sentry_reading.insert(key, m);
                    }
                    return Ok(None);
                }
            }
        }
        self.st().sentry_reading.remove(&key);
        let mut org = self.org_from_profile(sid, &p)?;
        if archetype {
            self.fill_archetype(&mut org, false);
        }
        Ok(Some(org))
    }

    /// SENTRY's org profile as an Org, keeping the archetype we hold.
    fn org_from_profile(&self, sid: &str, p: &Map<String, Value>) -> Result<Org, LookupError> {
        let prim = py_strip(&text(p, "primary_activity")).to_string();
        let mut sec = py_strip(&text(p, "secondary_activity")).to_string();
        if sec == prim {
            sec.clear();
        }
        let mut org = Org {
            sid: sid.to_uppercase(),
            name: p
                .get("name")
                .filter(|v| truthy(Some(v)))
                .map(py_str)
                .unwrap_or_else(|| sid.to_string()),
            members: match p.get("member_count").filter(|v| truthy(Some(v))) {
                Some(v) => py_int(v)?,
                None => 0,
            },
            primary_focus: prim,
            secondary_focus: sec,
            ..Default::default()
        };
        if let Some(old) = self.cache.get_org(sid, true)? {
            org.archetype = old.archetype;
            org.commitment = old.commitment;
        }
        Ok(org)
    }

    /// An org's display name, for flagging it - "" if none can be had: our
    /// cache, any age; else SENTRY's profile, one request (`ask`).
    pub fn org_name(&self, sid: &str, ask: bool) -> Result<String, LookupError> {
        if let Some(c) = self.cache.get_org(sid, true)?
            && !c.name.is_empty()
        {
            return Ok(c.name);
        }
        let Some(s) = self.sentry.as_ref().filter(|_| ask && self.enriches()) else {
            return Ok(String::new());
        };
        let Ok(Some(p)) = s.org(sid, None, false) else {
            return Ok(String::new());
        };
        if p.is_empty() {
            return Ok(String::new());
        }
        if !no_focus(&p) {
            self.cache.put_org(&self.org_from_profile(sid, &p)?, None)?;
        }
        Ok(py_strip(&text(&p, "name")).to_string())
    }

    /// Best effort: an archetype from the other service for an org that has
    /// none - with a key, once per org per run, never while it is failing.
    fn fill_archetype(&self, org: &mut Org, keep_age: bool) {
        if !org.archetype.is_empty() || !self.has_key() {
            return;
        }
        let sid = org.sid.to_uppercase();
        if self.st().archetype_tried.contains(&sid)
            || self.down_for() > 0.0
            || self.pace().trouble > 0
        {
            return;
        }
        self.st().archetype_tried.insert(sid.clone());
        let Ok(payload) = self.call(
            &format!("organization/{}", quote(&sid)),
            None,
            false,
            Some(ARCHETYPE_TIMEOUT),
        ) else {
            return;
        };
        let Some(data) = payload
            .as_ref()
            .and_then(Value::as_object)
            .and_then(|p| p.get("data"))
            .and_then(Value::as_object)
        else {
            return;
        };
        if !truthy(data.get("archetype")) {
            return;
        }
        org.archetype = text(data, "archetype");
        if org.commitment.is_empty() {
            org.commitment = text(data, "commitment");
        }
        if keep_age && let Ok(at) = self.cache.org_stored_at(&sid) {
            let _ = self.cache.put_org(org, at);
        }
    }

    // ---- players ----------------------------------------------------------------

    /// A profile, from the cache unless `fresh` (a person pressing "look
    /// up"). `live` false asks SENTRY's database only.
    pub fn citizen(&self, handle: &str, fresh: bool, live: bool) -> Result<Citizen, LookupError> {
        let handle = py_strip(handle);
        if is_ship_serial(handle) {
            return Ok(Citizen {
                exists: false,
                ..Citizen::new(&handle.to_uppercase())
            });
        }
        if !self.enriches() {
            return Ok(self.unverified(handle));
        }
        if !fresh && let Some(c) = self.cache.get_citizen(handle, false)? {
            return Ok(c);
        }
        // The same question asked twice at once is answered once: the
        // second waits for the first rather than asking again.
        let key = (handle.to_uppercase(), fresh, live);
        let (flight, leader) = {
            let mut m = self.flights.lock().unwrap_or_else(|e| e.into_inner());
            match m.get(&key) {
                Some(f) => (f.clone(), false),
                None => {
                    let f = Arc::new(Flight::default());
                    m.insert(key.clone(), f.clone());
                    (f, true)
                }
            }
        };
        if !leader {
            let mut g = flight.answer.lock().unwrap_or_else(|e| e.into_inner());
            while g.is_none() {
                g = flight.done.wait(g).unwrap_or_else(|e| e.into_inner());
            }
            return g.clone().unwrap();
        }
        let mut landing = Landing {
            client: self,
            key,
            flight,
            answer: None,
        };
        let r = self.resolve(handle, fresh, true, live);
        landing.answer = Some(r.clone());
        r
    }

    /// Seen, but nobody here can check it.
    fn unverified(&self, handle: &str) -> Citizen {
        Citizen {
            unverified: true,
            fetched_at: self.now(),
            ..Citizen::new(&handle.to_uppercase())
        }
    }

    /// citizen() past the cache. `use_sentry` false when the caller has
    /// already asked SENTRY and had nothing (exists()).
    fn resolve(
        &self,
        handle: &str,
        fresh: bool,
        use_sentry: bool,
        live: bool,
    ) -> Result<Citizen, LookupError> {
        let (mut found, mut sentry_err, mut queued, mut checking) = (None, None, false, None);
        if let Some(s) = self.sentry.as_ref().filter(|_| use_sentry) {
            match self.citizen_sentry(handle, fresh, live) {
                Ok(Sought::Found(c)) => found = Some(*c),
                Ok(Sought::Nobody) => {}
                Ok(Sought::Checking(chk)) => {
                    if fresh && !self.has_key() {
                        (found, checking) = self.await_check(s, handle, chk, live);
                    } else {
                        checking = Some(chk);
                    }
                }
                Err(e) => sentry_err = Some(e),
            }
            if let Some(mut f) = found.take() {
                if !f.exists {
                    // SENTRY: RSI has no profile for them.
                    f.note.source = Some(Source::Sentry);
                    self.cache.put_citizen(&f, None)?;
                    return Ok(f);
                }
                let window = if fresh { FRESH_WINDOW } else { CITIZEN_TTL };
                if self.now() - f.fetched_at > window {
                    let newer;
                    (queued, newer) = self.sentry_refresh(s, handle, &f, fresh);
                    if let Some(n) = newer {
                        f = n;
                    }
                }
                let whole = f.note.stale_reason.is_none();
                if whole && self.now() - f.fetched_at <= window {
                    f.note.source = Some(Source::Sentry);
                    self.cache.put_citizen(&f, Some(f.fetched_at))?;
                    return Ok(f);
                }
                if whole && queued && !fresh {
                    // Asked to re-read, not waited on: the old copy stands
                    // in, marked, stored at its real age.
                    f.note.source = Some(Source::Sentry);
                    f.note.stale_reason = Some("SENTRY is re-reading RSI for a newer copy".into());
                    f.note.awaiting_sentry = true;
                    f.note.stored_at = Some(f.fetched_at);
                    self.cache.put_citizen(&f, Some(f.fetched_at))?;
                    return Ok(f);
                }
                found = Some(f);
            }
        }

        // The other service's failure - or, without a key, SENTRY's. None
        // only when SENTRY knew them and nothing else could be asked.
        let err = if self.has_key() {
            let tried = self
                .citizen_sci(handle, fresh, found.is_none())
                .and_then(|mut c| {
                    c.note.source = Some(Source::Sci);
                    match &found {
                        Some(f) => self.keep_affiliates(c, f),
                        None => Ok(c),
                    }
                });
            match tried {
                Ok(c) => return Ok(c),
                Err(e) => Some(e),
            }
        } else {
            if found.is_none() && sentry_err.is_none() {
                return Ok(match checking {
                    Some(chk) => self.pending(handle, &chk),
                    // SENTRY has never heard of them, and nothing here can
                    // ask RSI: seen, unverified - not "no such account".
                    None => self.unverified(handle),
                });
            }
            sentry_err.clone()
        };
        let described = |e: &LookupError| self.describe(e);

        if let Some(mut f) = found {
            // SENTRY knew them; the confirmation could not be had.
            f.note.source = Some(Source::Sentry);
            let partial = f.note.stale_reason.clone();
            let mut reason = err.as_ref().map_or("not confirmed".into(), described);
            if queued && partial.is_none() {
                reason = format!(
                    "SENTRY is re-reading RSI for a newer copy{}",
                    if err.is_some() {
                        format!("; {reason}")
                    } else {
                        String::new()
                    }
                );
            }
            f.note.stale_reason = Some(match &partial {
                None => reason,
                Some(p) if p.contains(&reason) => p.clone(),
                Some(p) => format!("{p}; {reason}"),
            });
            f.note.stored_at = Some(f.fetched_at);
            match &partial {
                None => self.cache.put_citizen(&f, Some(f.fetched_at))?,
                Some(p) => self.cache.put_partial(&f, p, &f.note.unknown_focus)?,
            }
            return Ok(f);
        }

        let mut err = err.expect("set whenever nothing was found");
        if sentry_err.is_none()
            && let Some(s) = &self.sentry
        {
            let left = s.live_paused_for();
            if left > 0.0 {
                // SENTRY answered "nobody" from its database only.
                err.sentry_note = Some(format!(
                    "SENTRY does not have them yet and is rate limiting new lookups for {} more min",
                    minutes(left)
                ));
            }
        }
        if !fresh && let Some(mut old) = self.cache.get_citizen(handle, true)? {
            old.note.stale_reason = Some(described(&err));
            return Ok(old);
        }
        if let Some(chk) = checking {
            return Ok(self.pending(handle, &chk));
        }
        Err(err)
    }

    /// A "try now" on a name SENTRY is checking: wait once and ask again.
    fn await_check(
        &self,
        s: &Sentry,
        handle: &str,
        chk: Checking,
        live: bool,
    ) -> (Option<Citizen>, Option<Checking>) {
        let wait = SENTRY_CHECK_WAIT_MIN.max(chk.hint.unwrap_or(0.0));
        if chk.state == "check_failed" || wait > SENTRY_REFRESH_WAIT {
            return (None, Some(chk));
        }
        self.clock.sleep(wait);
        s.forget(handle);
        match self.citizen_sentry(handle, false, live) {
            Ok(Sought::Found(c)) => (Some(*c), None),
            Ok(Sought::Nobody) => (None, None),
            Ok(Sought::Checking(again)) => (None, Some(again)),
            Err(_) => (None, Some(chk)),
        }
    }

    /// Seen, not yet known: SENTRY is checking RSI for them. Never cached.
    fn pending(&self, handle: &str, chk: &Checking) -> Citizen {
        let mut c = self.unverified(handle);
        c.note.stale_reason = Some(chk.reason().into());
        c.note.checking = Some(chk.state.clone());
        c.note.retry_in = chk.hint;
        c
    }

    /// Waits for SENTRY's re-read of a player whose copy is too old - only
    /// with `wait` (a person pressed "try now"). Returns (asked, newer).
    fn sentry_refresh(
        &self,
        s: &Sentry,
        handle: &str,
        found: &Citizen,
        wait: bool,
    ) -> (bool, Option<Citizen>) {
        if !wait {
            return (true, None);
        }
        let before = found.fetched_at;
        let deadline = self.now() + SENTRY_REFRESH_WAIT;
        let mut pause = SENTRY_REFRESH_FIRST;
        while self.now() + pause <= deadline + 0.001 {
            self.clock.sleep(pause);
            pause = SENTRY_REFRESH_POLL;
            let Ok(landed) = s.citizen_fetched(handle) else {
                return (true, None);
            };
            if landed.is_some_and(|l| l != 0.0 && l > before + 1.0) {
                s.forget(handle); // the remembered copy is the old one
                return match self.citizen_sentry(handle, false, true) {
                    Ok(Sought::Found(c)) => (true, Some(*c)),
                    _ => (true, None),
                };
            }
        }
        (true, None)
    }

    /// Is this a real account? The identity check's question, and only that.
    /// `strict` returns None for "nobody here can tell".
    pub fn exists(&self, handle: &str, strict: bool) -> Result<Option<bool>, LookupError> {
        let handle = py_strip(handle);
        if is_ship_serial(handle) {
            return Ok(Some(false));
        }
        if !self.enriches() {
            return Ok(Some(true));
        }
        if let Some(c) = self.cache.get_citizen(handle, false)? {
            return Ok(Some(c.exists));
        }
        if let Some(s) = &self.sentry {
            let asked = {
                let _w = why("identity");
                s.citizen(handle, false, EXISTS_TIMEOUT, None, false)
            };
            if let Ok(d) = asked {
                if d != Answer::Nobody && is_answer_for(&d, handle) && sentry_has(&d) {
                    return Ok(Some(true));
                }
                if sentry_absent(&d) {
                    let gone = Citizen {
                        exists: false,
                        fetched_at: self.now(),
                        ..Citizen::new(&handle.to_uppercase())
                    };
                    if self.cache.put_citizen(&gone, None).is_ok() {
                        return Ok(Some(false));
                    }
                }
            }
        }
        let c = {
            let _w = why("identity");
            self.resolve(handle, false, false, true)?
        };
        if strict && c.unverified {
            return Ok(None);
        }
        Ok(Some(c.exists))
    }

    /// An org WITH its focus, or None if none can be had; `why` collects
    /// the reason.
    fn org_details(
        &self,
        sid: &str,
        fresh: bool,
        why: &mut Vec<String>,
    ) -> Result<Option<Org>, LookupError> {
        let exc = match self.org(sid, fresh) {
            Ok(o) => return Ok(Some(o)),
            Err(e) => e,
        };
        let old = self.cache.get_org(sid, true)?;
        if old.is_none() {
            let more = if exc.failure == Failure::NoKey {
                String::new()
            } else {
                format!(" and {}", self.describe(&exc))
            };
            let reading = self.st().sentry_reading.contains_key(&sid.to_uppercase());
            let limited = self
                .sentry
                .as_ref()
                .map(|s| s.live_paused_for())
                .unwrap_or(0.0);
            why.push(if reading {
                format!("SENTRY is still reading it (usually a few minutes){more}")
            } else if limited > 0.0 {
                format!(
                    "SENTRY is rate limiting new lookups for {} more min{more}",
                    minutes(limited)
                )
            } else {
                self.describe(&exc)
            });
        }
        Ok(old)
    }

    /// SENTRY's record as a Citizen. A record with an org whose focus could
    /// not be found carries `stale_reason`, and must not be cached.
    fn citizen_sentry(&self, handle: &str, fresh: bool, live: bool) -> Result<Sought, LookupError> {
        let s = self.sentry.as_ref().expect("asked only with SENTRY on");
        let max_age = if fresh { FRESH_WINDOW } else { CITIZEN_TTL };
        let d = s.citizen(handle, fresh, SENTRY_TIMEOUT, Some(max_age), live)?;
        if sentry_absent(&d) {
            return Ok(Sought::Found(Box::new(Citizen {
                exists: false,
                fetched_at: self.now(),
                ..Citizen::new(&handle.to_uppercase())
            })));
        }
        let Some(rec) = d.record().filter(|r| !r.is_empty()) else {
            return Ok(Sought::Nobody);
        };
        let (state, hint) = placeholder_state(&d);
        if let Some(state) = state.filter(|s| SENTRY_CHECK_STATES.contains(&s.as_str()))
            && is_answer_for(&d, handle)
        {
            return Ok(Sought::Checking(Checking { state, hint }));
        }
        let Some(prof) = sub(rec, "profile") else {
            return Ok(Sought::Nobody);
        };
        if !is_answer_for(&d, handle) || !sentry_has(&d) {
            return Ok(Sought::Nobody); // a rename, a placeholder or an archived copy
        }
        let empty = Map::new();
        let meta = sub(rec, "metadata").unwrap_or(&empty);
        let observed = parse_utc(meta.get("last_fetched"))
            .filter(|t| *t != 0.0)
            .or(parse_utc(prof.get("last_fetched")).filter(|t| *t != 0.0))
            .unwrap_or_else(|| self.now());
        let mut record = text(prof, "citizen_record");
        if !record.is_empty() && !record.starts_with('#') {
            record = format!("#{record}");
        }
        let mut c = Citizen {
            handle: handle.to_uppercase(),
            fetched_at: observed.min(self.now()),
            moniker: prof
                .get("display_name")
                .filter(|v| truthy(Some(v)))
                .map(py_str)
                .unwrap_or_else(|| handle.to_string()),
            enlisted: text(prof, "enlisted"),
            location: py_strip(&text(prof, "location")).to_string(),
            record,
            ..Default::default()
        };
        let mems: Vec<Value> = match rec.get("memberships").filter(|v| truthy(Some(v))) {
            Some(Value::Object(m)) => m
                .get("members")
                .filter(|v| truthy(Some(v)))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            Some(Value::Array(a)) => a.clone(),
            _ => vec![],
        };
        let (mut unknown, mut why): (Vec<String>, Vec<String>) = (vec![], vec![]);
        for m in &mems {
            let m = m.as_object().ok_or_else(attribute_error)?;
            let vis = text(m, "visibility");
            let vis = if vis.is_empty() {
                "visible".into()
            } else {
                vis.to_lowercase()
            };
            let sid = py_strip(&text(m, "organization_sid")).to_string();
            if vis != "visible" || sid.is_empty() || sid.starts_with('[') {
                // A membership that exists and does not say where.
                c.redacted = true;
                continue;
            }
            let sid = sid.to_uppercase();
            let mut org = match self.org_details(&sid, fresh, &mut why)? {
                Some(o) => o,
                None => {
                    unknown.push(sid.clone());
                    Org::new(&sid)
                }
            };
            org.is_main = truthy(m.get("is_main"));
            org.rank = text(m, "member_rank");
            c.orgs.push(org);
        }
        c.orgs.sort_by_key(|o| !o.is_main); // main first
        for extra in self.cache.member_orgs(handle)? {
            if c.orgs.iter().any(|o| o.sid.to_uppercase() == extra) {
                continue;
            }
            let org = match self.org_details(&extra, fresh, &mut why)? {
                Some(o) => o,
                None => {
                    unknown.push(extra.clone());
                    Org::new(&extra)
                }
            };
            c.orgs.push(org);
        }
        if !unknown.is_empty() {
            c.note.stale_reason = Some(format!(
                "no org focus for {}: {}, so whether it is a piracy org is unknown",
                unknown.join(", "),
                why.first().map(String::as_str).unwrap_or("not available")
            ));
            c.note.stored_at = Some(c.fetched_at);
            let st = self.st();
            c.note.awaiting_sentry = unknown
                .iter()
                .any(|s| st.sentry_reading.contains_key(&s.to_uppercase()));
            drop(st);
            c.note.unknown_focus = unknown;
        }
        Ok(Sought::Found(Box::new(c)))
    }

    /// The other service's answer, plus the affiliate orgs SENTRY listed,
    /// stored - its silence about affiliates is not evidence they are gone.
    fn keep_affiliates(&self, mut c: Citizen, found: &Citizen) -> Result<Citizen, LookupError> {
        if !c.exists {
            self.cache.put_citizen(&c, None)?;
            return Ok(c);
        }
        let mut have: HashSet<String> = c.orgs.iter().map(|o| o.sid.to_uppercase()).collect();
        let unknown: HashSet<String> = found
            .note
            .unknown_focus
            .iter()
            .map(|s| s.to_uppercase())
            .collect();
        let mut still = Vec::new();
        for o in &found.orgs {
            let sid = o.sid.to_uppercase();
            if o.is_main || have.contains(&sid) {
                continue;
            }
            c.orgs.push(o.clone());
            have.insert(sid.clone());
            if unknown.contains(&sid) {
                still.push(sid);
            }
        }
        c.redacted = c.redacted || found.redacted;
        if still.is_empty() {
            self.cache.put_citizen(&c, None)?;
        } else {
            let reason = format!(
                "no org focus for {}, so whether it is a piracy org is unknown",
                still.join(", ")
            );
            c.note.stale_reason = Some(reason.clone());
            c.note.stored_at = Some(c.fetched_at);
            c.note.unknown_focus = still;
            self.cache.put_partial(&c, &reason, &c.note.unknown_focus)?;
        }
        Ok(c)
    }

    /// Finishes profiles missing an org's focus - the page's background
    /// check: free from the cache first, then SENTRY about each org still
    /// missing (with `ask`), on FOCUS_SWEEP_GAPS.
    pub fn complete_partials(&self, ask: bool) -> Result<Counts, LookupError> {
        let mut got = Counts::default();
        let missing = self.fill_partials(&mut got)?;
        self.st().focus_tries.retain(|sid, _| missing.contains(sid));
        let Some(s) = &self.sentry else {
            return Ok(got);
        };
        if !ask
            || missing.is_empty()
            || !self.enriches()
            || s.down_for() > 0.0
            || s.live_paused_for() > 0.0
        {
            return Ok(got);
        }
        let now = self.mono();
        let mut sids: Vec<&String> = missing.iter().collect();
        sids.sort();
        for sid in sids {
            let (n, due) = self.st().focus_tries.get(sid).copied().unwrap_or((0, 0.0));
            if now < due {
                continue;
            }
            let gap = FOCUS_SWEEP_GAPS[n.min(FOCUS_SWEEP_GAPS.len() - 1)];
            self.st()
                .focus_tries
                .insert(sid.clone(), (n + 1, now + gap));
            got.asked += 1;
            match self.org_sentry(sid, false, false) {
                Err(_) => break, // its breaker says when; the next round asks
                Ok(Some(org)) => {
                    self.cache.put_org(&org, None)?;
                    got.found += 1;
                }
                Ok(None) => {}
            }
        }
        if got.found > 0 {
            self.fill_partials(&mut got)?;
        }
        Ok(got)
    }

    /// Step 1 of complete_partials(); returns the SIDs still missing.
    fn fill_partials(&self, got: &mut Counts) -> Result<HashSet<String>, LookupError> {
        let mut missing = HashSet::new();
        for p in self.cache.partials()? {
            if self
                .cache
                .citizen_stored_at(&p.handle)?
                .is_some_and(|w| w >= p.observed_at)
            {
                continue; // a newer whole copy is on file
            }
            let data = p.data.as_object().ok_or_else(attribute_error)?;
            let want: HashSet<&String> = p.unknown.iter().collect();
            let (mut orgs, mut left, mut found) = (Some(Vec::new()), Vec::new(), 0);
            let listed = data
                .get("orgs")
                .filter(|v| truthy(Some(v)))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for od in listed {
                let Ok(mut o) = serde_json::from_value::<Org>(od) else {
                    orgs = None;
                    break;
                };
                let sid = o.sid.to_uppercase();
                if want.contains(&sid) {
                    if let Some(mut known) = self.cache.get_org(&sid, true)? {
                        known.is_main = o.is_main;
                        known.rank = o.rank;
                        known.redacted = o.redacted;
                        o = known;
                        found += 1;
                    } else {
                        left.push(sid);
                    }
                }
                orgs.as_mut().unwrap().push(o);
            }
            let Some(orgs) = orgs else { continue };
            missing.extend(left.iter().cloned());
            if found == 0 {
                continue;
            }
            let mut rest = data.clone();
            rest.remove("orgs");
            let Ok(mut c) = serde_json::from_value::<Citizen>(Value::Object(rest)) else {
                continue;
            };
            c.orgs = orgs;
            if left.is_empty() {
                self.cache.put_citizen(&c, Some(p.observed_at))?;
                got.completed += 1;
            } else {
                let why = match p.reason.split_once(':') {
                    Some((_, w)) => w.to_string(),
                    None => " not available, so whether it is a piracy org is unknown".into(),
                };
                self.cache.update_partial(
                    &c,
                    &format!("no org focus for {}:{why}", left.join(", ")),
                    &left,
                )?;
                got.filled += 1;
            }
        }
        Ok(missing)
    }

    /// starcitizen-api.com's answer, and the only source of "no account"
    /// besides SENTRY saying so outright. `store` false leaves storing to
    /// the caller.
    fn citizen_sci(&self, handle: &str, fresh: bool, store: bool) -> Result<Citizen, LookupError> {
        let data = self.get(&format!("user/{}", quote(handle)), fresh)?;
        let Some(data) = data.filter(|d| truthy(Some(d))) else {
            let c = Citizen {
                exists: false,
                fetched_at: self.now(),
                ..Citizen::new(&handle.to_uppercase())
            };
            if store {
                self.cache.put_citizen(&c, None)?;
            }
            return Ok(c);
        };
        let d = data.as_object().ok_or_else(attribute_error)?;
        let empty = Map::new();
        let profile = sub(d, "profile").unwrap_or(&empty);
        let mut c = Citizen {
            handle: handle.to_uppercase(),
            fetched_at: self.now(),
            moniker: profile
                .get("display")
                .filter(|v| truthy(Some(v)))
                .map(py_str)
                .unwrap_or_else(|| handle.to_string()),
            enlisted: text(profile, "enlisted"),
            record: text(profile, "id"),
            ..Default::default()
        };
        // Three states, not the same claim: a visible org, an org that is
        // hidden ({"name": ""} - redacted), and genuinely none.
        let main = d.get("organization").filter(|v| truthy(Some(v)));
        let main = match main {
            Some(Value::Object(m)) => m.clone(),
            Some(_) => return Err(attribute_error()),
            None => Map::new(),
        };
        match main.get("sid").filter(|v| truthy(Some(v))) {
            Some(sid) => {
                let mut full = self.org(&py_str(sid), fresh)?;
                full.is_main = true;
                full.rank = text(&main, "rank");
                c.orgs = vec![full];
            }
            None => c.redacted = main.contains_key("name"),
        }
        for extra in self.cache.member_orgs(handle)? {
            if !c.orgs.iter().any(|o| o.sid.to_uppercase() == extra) {
                c.orgs.push(self.org(&extra, fresh)?);
            }
        }
        if store {
            self.cache.put_citizen(&c, None)?;
        }
        Ok(c)
    }

    // ---- rosters ----------------------------------------------------------------

    /// An org's member list: SENTRY first, starcitizen-api.com behind it.
    /// `on_page(page, so_far)` after each page.
    pub fn roster(
        &self,
        sid: &str,
        max_pages: u32,
        on_page: &mut dyn FnMut(u32, usize),
    ) -> Result<Roster, LookupError> {
        let mut sentry_err = None;
        if let Some(s) = &self.sentry {
            match s.roster(sid, on_page) {
                Ok(Some(r))
                    if !r.members.is_empty()
                        || r.total.as_ref().is_some_and(|t| truthy(Some(t))) =>
                {
                    if !r.members.is_empty() {
                        let handles: Vec<&str> =
                            r.members.iter().map(|m| m.handle.as_str()).collect();
                        self.cache.put_members(sid, &handles, r.complete)?;
                    }
                    return Ok(r);
                }
                Ok(_) => {}
                Err(e) => sentry_err = Some(e),
            }
        }
        if self.has_key() {
            let (members, complete) = self.members_detail(sid, max_pages, on_page)?;
            return Ok(Roster {
                members,
                complete,
                source: Some("sci"),
                total: None,
                hidden: 0,
                redacted: 0,
                observed_at: None,
            });
        }
        if let Some(e) = sentry_err {
            return Err(e);
        }
        Ok(Roster {
            members: vec![],
            complete: false,
            source: Some("sentry"),
            total: None,
            hidden: 0,
            redacted: 0,
            observed_at: None,
        })
    }

    /// A list endpoint's page: a list (possibly empty - the end), or None
    /// when the service did not answer properly.
    fn rows(&self, path: &str, mode: Option<&str>) -> Result<Option<Vec<Value>>, LookupError> {
        let Some(payload) = self.call(path, mode, false, None)? else {
            return Ok(None);
        };
        let p = payload.as_object().ok_or_else(attribute_error)?;
        if !truthy(p.get("success")) {
            return Ok(None);
        }
        Ok(p.get("data").and_then(Value::as_array).cloned())
    }

    /// The roster as starcitizen-api.com gives it, and whether the walk
    /// FINISHED - an empty page is the end; a failure or the page cap is
    /// not. Forced to mode 'live', the only one that has it.
    pub fn members_detail(
        &self,
        sid: &str,
        max_pages: u32,
        on_page: &mut dyn FnMut(u32, usize),
    ) -> Result<(Vec<RosterMember>, bool), LookupError> {
        let mut out = Vec::new();
        let mut complete = false;
        for page in 1..=max_pages {
            let Some(rows) = self.rows(
                &format!("organization_members/{}?page={page}", quote(sid)),
                Some("live"),
            )?
            else {
                break;
            };
            if rows.is_empty() {
                complete = true;
                break;
            }
            for m in &rows {
                let m = m.as_object().ok_or_else(attribute_error)?;
                if !truthy(m.get("handle")) {
                    continue;
                }
                out.push(RosterMember {
                    handle: py_str(&m["handle"]),
                    display: text(m, "display"),
                    rank: text(m, "rank"),
                    stars: m
                        .get("stars")
                        .filter(|v| truthy(Some(v)))
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                });
            }
            on_page(page, out.len());
        }
        if !out.is_empty() {
            let handles: Vec<&str> = out
                .iter()
                .map(|m: &RosterMember| m.handle.as_str())
                .collect();
            self.cache.put_members(sid, &handles, complete)?;
        }
        Ok((out, complete))
    }
}

/// The default page cap for a roster pull.
pub const DEFAULT_MAX_PAGES: u32 = MEMBER_PAGE_CAP;
