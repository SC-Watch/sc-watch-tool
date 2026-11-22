//! Player and organisation lookup - rsi.py: SENTRY first, starcitizen-api.com
//! behind it, and rsi_cache.db.
//!
//! What each source is trusted for, why each has its own pacing and outage
//! breaker, and every number below, are rsi.py's and are explained there;
//! this is the same behaviour, checked against it step by step
//! (tests/rsi_ops.rs). What changes is the shape: the network
//! ([`transport::Transport`]), the clock ([`clock::Clock`]) and the request
//! log are given to the client rather than reached for; the answer's notes
//! ([`model::Note`]) are fields, not attributes set on the fly; and what was
//! left on a thread for the caller (headers, the lookup state) is returned.

pub mod cache;
pub mod client;
pub mod clock;
pub mod error;
pub mod log;
pub mod model;
mod paced;
pub mod sentry;
pub mod transport;
pub mod upkeep;

use std::cell::RefCell;
use std::marker::PhantomData;
use std::path::Path;

pub use cache::{Cache, Partial};
pub use client::{Client, Counts, Options, Provider, Roster};
pub use clock::{Clock, SystemClock};
pub use error::{Failure, LookupError};
pub use log::RequestLog;
pub use model::{Citizen, Note, Org, Source};
pub use sentry::{Answer, Sentry};
pub use transport::{FetchError, Headers, Reply, Transport};
pub use upkeep::{Ghosts, Pull, Verifier, fetch_roster, find_ghosts, purge_ghosts};

pub const API_HOST: &str = "https://api.starcitizen-api.com";
pub const USER_AGENT: &str = "sc-watch/0.2 (personal Star Citizen contact tool)";
pub const MEMBER_PAGE_CAP: u32 = 60;

pub const SENTRY_HOST: &str = "https://sentry.wildknightsquadron.com/api/v1";
pub const SENTRY_USER_AGENT: &str =
    "sc-watch (Star Citizen contact tool; +https://github.com/SC-Watch/sc-watch-tool)";
pub const SENTRY_TIMEOUT: f64 = 8.0;
pub const EXISTS_TIMEOUT: f64 = 1.5;
pub const SENTRY_PAGE_TIMEOUT: f64 = 20.0;
pub const SENTRY_PAGE_SIZE: u32 = 100;
pub const SENTRY_MAX_PAGES: u32 = 40;
pub const FRESH_WINDOW: f64 = 86400.0;
pub const SENTRY_REFRESH_WAIT: f64 = 30.0;
pub const SENTRY_REFRESH_FIRST: f64 = 12.0;
pub const SENTRY_REFRESH_POLL: f64 = 6.0;
pub const SENTRY_MEMO_TTL: f64 = 60.0;
pub const SENTRY_REREAD_AGAIN: f64 = 600.0;
pub const SENTRY_LIVE_PAUSE: f64 = 900.0;
pub const SENTRY_LIVE_AGAIN: f64 = 6.0 * 3600.0;
pub const SENTRY_BUDGET_MAX: f64 = 300.0;
pub const JITTER: (f64, f64) = (1.0, 5.0);
pub const SENTRY_STUB_MEMO_TTL: f64 = 10.0;
pub const SENTRY_CHECK_STATES: &[&str] = &["queued", "queued_low", "check_failed"];
pub const SENTRY_CHECK_WAIT_MIN: f64 = 15.0;
pub const SENTRY_RL_LOW: i64 = 10;
pub const SENTRY_RL_MAX_GAP: f64 = 5.0;
pub const SENTRY_LOG: (&str, &str) = ("debug_bursts", "sentry_requests.log");
pub const SENTRY_LOG_MAX: u64 = 2_000_000;
pub const FOCUS_SWEEP_GAPS: &[f64] = &[600.0, 1800.0, 3600.0, 21600.0];
pub const ARCHETYPE_TIMEOUT: f64 = 8.0;
pub const CITIZEN_TTL: f64 = 30.0 * 86400.0;
pub const ORG_TTL: f64 = 30.0 * 86400.0;
pub const MISS_TTL: f64 = 365.0 * 86400.0;
pub const MIN_INTERVAL: f64 = 1.0;
pub const DEFAULT_MODE: &str = "auto";

pub const LOOKUP_URL: &str = "https://sc-intelligence.net/search?q=";
pub const PROFILE_URL: &str = "https://robertsspaceindustries.com/citizens/";
pub const ORG_URL: &str = "https://robertsspaceindustries.com/orgs/";

/// `urllib.parse.quote(s, safe='')`: everything but letters, digits and
/// `_.-~` percent-encoded, as UTF-8.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"_.-~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A third-party lookup page, for a person to open.
pub fn lookup_url(handle: &str) -> String {
    format!("{LOOKUP_URL}{}", quote(handle))
}

/// The player's own RSI page.
pub fn profile_url(handle: &str) -> String {
    format!("{PROFILE_URL}{}", quote(handle))
}

pub fn org_url(sid: &str) -> String {
    format!("{ORG_URL}{}", quote(sid))
}

/// An abandoned ship's serial, which never resolves to a citizen.
pub fn is_ship_serial(handle: &str) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"^[A-Z]{2}-\d").unwrap())
        .is_match(&handle.to_uppercase())
}

/// The starcitizen-api.com key: `env` (SC_API_KEY's value) first, then the
/// key file beside the data - `find_api_key()`.
pub fn find_api_key(env: Option<&str>, key_file: &Path) -> Option<String> {
    let key = scw_store::json::py_strip(env.unwrap_or(""));
    if !key.is_empty() {
        return Some(key.into());
    }
    let bytes = std::fs::read(key_file).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let key = scw_store::json::py_strip(text.strip_prefix('\u{feff}').unwrap_or(&text));
    (!key.is_empty()).then(|| key.into())
}

thread_local! {
    static WHY: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// While alive, names the reason for this thread's SENTRY requests in the
/// request log - `with rsi.why(tag):`. The innermost wins.
pub struct Why {
    prev: Option<String>,
    _here: PhantomData<*const ()>, // dropped on the thread that made it
}

pub fn why(tag: &str) -> Why {
    Why {
        prev: WHY.with(|w| w.replace(Some(tag.into()))),
        _here: PhantomData,
    }
}

impl Drop for Why {
    fn drop(&mut self) {
        let prev = self.prev.take();
        WHY.with(|w| *w.borrow_mut() = prev);
    }
}

pub(crate) fn current_why() -> Option<String> {
    WHY.with(|w| w.borrow().clone())
}
