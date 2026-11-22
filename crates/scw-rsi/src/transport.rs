//! One GET, parsed - the seam rsi.py's tests fake (`Sentry._fetch`,
//! `Client._fetch`), and the only place the network is touched.

use serde_json::Value;

/// Response headers, looked up without regard to case, as
/// `http.client.HTTPMessage` does; the first of a repeated name wins.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Headers(pub Vec<(String, String)>);

impl Headers {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn items(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

/// A 2xx answer: its body as JSON, and its headers.
#[derive(Clone, Debug)]
pub struct Reply {
    pub body: Value,
    pub headers: Headers,
}

/// What can go wrong with one GET, as urllib tells them apart.
#[derive(Clone, Debug, PartialEq)]
pub enum FetchError {
    /// Any non-2xx answer (`urllib.error.HTTPError`).
    Http { code: u16, headers: Headers },
    /// No answer in time (`TimeoutError`).
    Timeout,
    /// Could not connect, or the connection broke (`URLError`, `OSError`).
    Connect(String),
    /// An answer that is not JSON (`json.JSONDecodeError`).
    BadJson,
}

pub trait Transport: Send + Sync {
    fn get(&self, url: &str, user_agent: &str, timeout: f64) -> Result<Reply, FetchError>;
}
