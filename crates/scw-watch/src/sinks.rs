//! What the watcher talks to, as interfaces: the console, the alert player,
//! the session log, and the lookup client - so a session can run against
//! the real ones, or against recorders and a script.

use serde_json::Value;

use scw_rsi::{Citizen, LookupError};

/// Announcements scroll; a one-line status sits below them.
pub trait Console: Send + Sync {
    fn say(&self, text: &str);
    fn status(&self, text: &str);
    fn done(&self) {}
}

/// One contact in a burst's alert (watch.Session._burst_audio's payload).
#[derive(Clone, Debug, PartialEq)]
pub struct Spoken {
    pub handle: String,
    pub range_km: Option<f64>,
    pub flags: Vec<String>,
    /// The console's first reason line, "" when nothing is known.
    pub reason: String,
    /// What speech says, as data (Assessment.spoken_all()); None: nothing.
    pub said: Option<Value>,
}

/// The alert player, as the watcher calls it (audio.Alerts).
pub trait AlertSink: Send + Sync {
    /// "trigger", "contact", "nothing", "flagged", ...
    fn event(&self, kind: &str, handle: Option<&str>, flags: &[String], range_km: Option<f64>);
    fn burst_result(&self, contacts: &[Spoken]);
    fn close(&self) {}
}

/// Where structured events go (debuglog.DebugLog.event).
pub trait EventLog: Send + Sync {
    fn event(&self, kind: &str, fields: &[(&str, Value)]);
}

impl EventLog for crate::DebugLog {
    fn event(&self, kind: &str, fields: &[(&str, Value)]) {
        crate::DebugLog::event(self, kind, fields)
    }
}

/// The lookup client, as the enricher uses it (rsi.Client).
pub trait Lookups: Send + Sync {
    fn citizen(&self, handle: &str) -> Result<Citizen, LookupError>;
    /// Seconds until the fallback service is asked again.
    fn down_for(&self) -> f64;
    /// Records that a lookup failed, for the page (Cache.note_failure).
    fn note_failure(&self, handle: &str, why: &str, attempts: usize, gave_up: bool);
}

impl Lookups for scw_rsi::Client {
    fn citizen(&self, handle: &str) -> Result<Citizen, LookupError> {
        let _why = scw_rsi::why("watcher");
        scw_rsi::Client::citizen(self, handle, false, true)
    }

    fn down_for(&self) -> f64 {
        scw_rsi::Client::down_for(self)
    }

    fn note_failure(&self, handle: &str, why: &str, attempts: usize, gave_up: bool) {
        let _ = self
            .cache
            .note_failure(handle, why, attempts as i64, gave_up);
    }
}
