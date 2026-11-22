//! Why a lookup failed - rsi.py's exceptions, one type: which kind, which
//! service, and SENTRY's note when the failure shown is the other service's.

use crate::transport::{FetchError, Headers};

#[derive(Clone, Debug, PartialEq)]
pub enum Failure {
    /// A non-2xx answer (`urllib.error.HTTPError`).
    Http {
        code: u16,
        headers: Headers,
    },
    Timeout,
    Connect(String),
    BadJson,
    /// starcitizen-api.com asked with no key configured (`NoKey`).
    NoKey,
    /// Refused without asking: the service is treated as down until
    /// `until` (monotonic seconds) - `ServiceDown`.
    ServiceDown {
        until: f64,
    },
    /// SENTRY's limit on live lookups is used up until `until` -
    /// `LiveLimited`, a kind of ServiceDown.
    LiveLimited {
        until: f64,
    },
    /// Anything else, by its Python type name: a damaged cache row
    /// (`TypeError`, `JSONDecodeError`), the database (`DatabaseError`).
    Other {
        kind: String,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct LookupError {
    pub failure: Failure,
    /// "SENTRY" or "starcitizen-api.com", once known - `_tag()`.
    pub service: Option<&'static str>,
    /// What SENTRY could not do, said in front of the other service's
    /// failure - `sentry_note`.
    pub sentry_note: Option<String>,
}

impl LookupError {
    pub fn new(failure: Failure) -> Self {
        LookupError {
            failure,
            service: None,
            sentry_note: None,
        }
    }

    pub fn other(kind: &str, message: impl Into<String>) -> Self {
        Self::new(Failure::Other {
            kind: kind.into(),
            message: message.into(),
        })
    }

    /// Notes the service, unless one is already noted - `_tag()`.
    pub fn tagged(mut self, service: &'static str) -> Self {
        self.service.get_or_insert(service);
        self
    }

    /// Would asking again later plausibly work? Yes for the service
    /// erroring (5xx), rate limiting (429), timeouts and failing to connect;
    /// no for the other 4xx, a missing key, or an answer that is not JSON.
    pub fn is_transient(&self) -> bool {
        match &self.failure {
            Failure::ServiceDown { .. } | Failure::LiveLimited { .. } => true,
            Failure::Http { code, .. } => *code >= 500 || *code == 429,
            Failure::Timeout | Failure::Connect(_) => true,
            Failure::BadJson | Failure::NoKey | Failure::Other { .. } => false,
        }
    }

    pub fn is_service_down(&self) -> bool {
        matches!(
            self.failure,
            Failure::ServiceDown { .. } | Failure::LiveLimited { .. }
        )
    }

    /// The Python exception's type name, for reports and tests.
    pub fn type_name(&self) -> &str {
        match &self.failure {
            Failure::Http { .. } => "HTTPError",
            Failure::Timeout => "TimeoutError",
            Failure::Connect(_) => "URLError",
            Failure::BadJson => "JSONDecodeError",
            Failure::NoKey => "NoKey",
            Failure::ServiceDown { .. } => "ServiceDown",
            Failure::LiveLimited { .. } => "LiveLimited",
            Failure::Other { kind, .. } => kind,
        }
    }

    /// A short, plain reason, naming the service when it is known, with
    /// SENTRY's note in front - `describe_failure()`. `monotonic` is now,
    /// for how long a pause has left.
    pub fn describe(&self, monotonic: f64) -> String {
        let text = self.describe_alone(monotonic);
        match &self.sentry_note {
            Some(note) => format!("{note}; {text}"),
            None => text,
        }
    }

    fn describe_alone(&self, monotonic: f64) -> String {
        let who = self.service.unwrap_or("the lookup service");
        match &self.failure {
            Failure::LiveLimited { until } => format!(
                "{who} is rate limiting new lookups for {} more min",
                minutes((until - monotonic).max(0.0))
            ),
            Failure::ServiceDown { .. } => {
                format!("{who} is not responding, so lookups are paused")
            }
            Failure::NoKey => "needs a starcitizen-api.com API key".into(),
            Failure::Http { code: 429, .. } => format!("{who} is rate limiting (HTTP 429)"),
            Failure::Http { code, .. } if *code >= 500 => {
                format!("{who} had an error (HTTP {code})")
            }
            Failure::Http {
                code: code @ (401 | 403),
                ..
            } => format!("the API key was refused (HTTP {code})"),
            Failure::Http { code, .. } => format!("{who}: HTTP {code}"),
            Failure::Timeout => format!("{who} timed out"),
            Failure::Connect(m) if m.to_lowercase().contains("timed out") => {
                format!("{who} timed out")
            }
            Failure::Connect(_) => format!("could not reach {who}"),
            Failure::Other { message, .. } if message.to_lowercase().contains("timed out") => {
                format!("{who} timed out")
            }
            Failure::BadJson => "JSONDecodeError".into(),
            Failure::Other { kind, .. } => kind.clone(),
        }
    }
}

/// `max(1, round(seconds / 60))`, Python's round: halves to even.
pub fn minutes(seconds: f64) -> i64 {
    ((seconds / 60.0).round_ties_even() as i64).max(1)
}

impl From<FetchError> for LookupError {
    fn from(e: FetchError) -> Self {
        LookupError::new(match e {
            FetchError::Http { code, headers } => Failure::Http { code, headers },
            FetchError::Timeout => Failure::Timeout,
            FetchError::Connect(m) => Failure::Connect(m),
            FetchError::BadJson => Failure::BadJson,
        })
    }
}

impl From<rusqlite::Error> for LookupError {
    fn from(e: rusqlite::Error) -> Self {
        LookupError::other("DatabaseError", e.to_string())
    }
}

impl std::fmt::Display for LookupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.describe_alone(0.0))
    }
}

impl std::error::Error for LookupError {}
