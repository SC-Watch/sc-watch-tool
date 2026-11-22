//! The network: one GET, parsed - what rsi.py's `_fetch` does with urllib.
//! TLS is rustls, so Linux needs no system OpenSSL.
//!
//! As urllib: redirects are followed; any answer outside 2xx is an HTTP
//! error carrying its headers (SENTRY says which 404 it is, and how much of
//! its budget is left, in them); the body is read as UTF-8 with bad bytes
//! replaced, then as JSON. One difference: `timeout` bounds the whole
//! request, where urllib's bounds each socket operation - a server
//! trickling bytes can no longer hold a lookup past it.

use std::io::ErrorKind;
use std::time::Duration;

use ureq::Agent;

use crate::transport::{FetchError, Headers, Reply, Transport};

pub struct Https {
    agent: Agent,
}

impl Default for Https {
    fn default() -> Self {
        Self::new()
    }
}

impl Https {
    pub fn new() -> Self {
        let agent: Agent = Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        Https { agent }
    }
}

fn failure(e: ureq::Error) -> FetchError {
    match e {
        ureq::Error::Timeout(_) => FetchError::Timeout,
        ureq::Error::Io(io) if matches!(io.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => {
            FetchError::Timeout
        }
        ureq::Error::StatusCode(code) => FetchError::Http {
            code,
            headers: Headers::default(),
        },
        other => FetchError::Connect(other.to_string()),
    }
}

impl Transport for Https {
    fn get(&self, url: &str, user_agent: &str, timeout: f64) -> Result<Reply, FetchError> {
        let limit = Duration::from_secs_f64(timeout.max(0.001));
        let mut resp = self
            .agent
            .get(url)
            .header("User-Agent", user_agent)
            .header("Accept", "application/json")
            .config()
            .timeout_global(Some(limit))
            .build()
            .call()
            .map_err(failure)?;
        let headers = Headers(
            resp.headers()
                .iter()
                .map(|(k, v)| {
                    (
                        k.as_str().to_string(),
                        String::from_utf8_lossy(v.as_bytes()).into_owned(),
                    )
                })
                .collect(),
        );
        let code = resp.status().as_u16();
        if !(200..300).contains(&code) {
            return Err(FetchError::Http { code, headers });
        }
        let bytes = resp.body_mut().read_to_vec().map_err(failure)?;
        let body = scw_store::json::loads(&String::from_utf8_lossy(&bytes))
            .map_err(|_| FetchError::BadJson)?;
        Ok(Reply { body, headers })
    }
}
