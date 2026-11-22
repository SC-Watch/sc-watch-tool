//! The SENTRY request log: one line per request actually sent - from the
//! watcher and the page alike - with no handles and no org IDs. Rotated at
//! SENTRY_LOG_MAX, one old copy kept. Never fails a lookup.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use serde_json::Value;

use crate::clock::Clock;
use crate::model::parse_utc;
use crate::transport::Headers;
use crate::{SENTRY_LOG_MAX, current_why};
use scw_store::json::{py_str, truthy};

pub struct RequestLog {
    pub path: PathBuf,
    /// Which program is asking ("watch", "ui_server").
    pub process: String,
    /// Stamp lines in UTC rather than local time (tests).
    pub utc: bool,
}

static LOG_LOCK: Mutex<()> = Mutex::new(());

fn obj(v: Option<&Value>) -> Option<&serde_json::Map<String, Value>> {
    v.filter(|v| truthy(Some(v))).and_then(Value::as_object)
}

impl RequestLog {
    /// Appends one line about a request to `path` (SENTRY's, after the
    /// host).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn note(
        &self,
        clock: &dyn Clock,
        path: &str,
        live: bool,
        status: &str,
        started: f64,
        payload: Option<&Value>,
        headers: Option<&Headers>,
    ) {
        let line = self.line(clock, path, live, status, started, payload, headers);
        let _held = LOG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() > SENTRY_LOG_MAX) {
            let mut old = self.path.clone().into_os_string();
            old.push(".1");
            let _ = std::fs::rename(&self.path, old);
        }
        if let Ok(mut fh) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = fh.write_all(format!("{line}\n").as_bytes());
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn line(
        &self,
        clock: &dyn Clock,
        path: &str,
        live: bool,
        status: &str,
        started: f64,
        payload: Option<&Value>,
        headers: Option<&Headers>,
    ) -> String {
        let (route, query) = path.split_once('?').unwrap_or((path, ""));
        // parse_qs: blank values dropped.
        let q: Vec<(&str, &str)> = query
            .split('&')
            .filter_map(|p| p.split_once('='))
            .filter(|(_, v)| !v.is_empty())
            .collect();
        let first = |k: &str| q.iter().find(|(n, _)| *n == k).map(|(_, v)| *v);
        let kind = if route.starts_with("/citizens/") {
            if route.ends_with("/metadata") {
                "meta"
            } else {
                "player"
            }
            .to_string()
        } else if route.ends_with("/memberships") {
            "roster".into()
        } else if route.starts_with("/orgs/") {
            "org".into()
        } else {
            let k = route.trim_matches('/').split('/').next().unwrap_or("");
            if k.is_empty() { "?" } else { k }.to_string()
        };
        let scrape: Vec<&str> = q
            .iter()
            .filter(|(n, _)| *n == "scrape")
            .map(|(_, v)| *v)
            .collect();
        let mode = if kind == "meta" || !live || scrape == ["false"] {
            "db"
        } else {
            "live?"
        };
        let mut bits: Vec<String> = Vec::new();
        if let Some(m) = first("max_age") {
            bits.push(format!("max_age={m}"));
        }
        match payload.and_then(Value::as_object) {
            Some(p) if kind == "player" => {
                let empty = serde_json::Map::new();
                let prof = obj(p.get("profile")).unwrap_or(&empty);
                let meta = obj(p.get("metadata")).unwrap_or(&empty);
                let fetched = meta.get("last_fetched").filter(|v| truthy(Some(v)));
                let seen = parse_utc(fetched.or(prof.get("last_fetched")));
                if let Some(seen) = seen.filter(|s| *s != 0.0) {
                    let age = clock.time() - seen;
                    bits.push(if age >= 120.0 {
                        format!("copy={:.1}d", age / 86400.0)
                    } else {
                        format!("copy={}s", (age as i64).max(0))
                    });
                    if let Some(m) = first("max_age").and_then(|m| m.parse::<f64>().ok())
                        && age > m
                    {
                        bits.push("reread-queued".into());
                    }
                }
                if truthy(p.get("redirected_from")) {
                    bits.push("renamed".into());
                }
                if truthy(meta.get("is_stub")) || truthy(prof.get("is_stub")) {
                    bits.push("stub".into());
                    if truthy(meta.get("lookup_state")) {
                        let hint = meta
                            .get("retry_after_hint")
                            .filter(|h| truthy(Some(h)))
                            .map(|h| format!("/{}s", py_str(h)))
                            .unwrap_or_default();
                        bits.push(format!("state={}{hint}", py_str(&meta["lookup_state"])));
                    }
                }
                if truthy(meta.get("is_archived")) {
                    bits.push("archived".into());
                }
                if meta.get("finalized") == Some(&Value::Bool(false)) {
                    bits.push("unfinalized".into());
                }
            }
            Some(p) if kind == "org" => {
                let prof = p.get("profile").unwrap_or(payload.unwrap());
                if let Some(prof) = prof.as_object()
                    && !(truthy(prof.get("primary_activity"))
                        || truthy(prof.get("secondary_activity")))
                {
                    bits.push("no-focus".into());
                }
            }
            _ => {}
        }
        if let Some(h) = headers {
            let mut rl: Vec<(String, &str)> = h
                .items()
                .map(|(k, v)| (k.to_lowercase(), v))
                .filter(|(k, _)| {
                    k.contains("ratelimit") || k == "retry-after" || k == "x-lookup-state"
                })
                .collect();
            rl.sort();
            bits.extend(rl.into_iter().map(|(k, v)| format!("{k}={v}")));
        }
        let ms = ((clock.monotonic() - started) * 1000.0) as i64;
        let secs = clock.time().floor() as i64;
        let stamp = if self.utc {
            chrono::DateTime::from_timestamp(secs, 0)
                .map(|t| t.naive_utc().format("%Y-%m-%d %H:%M:%S").to_string())
        } else {
            use chrono::TimeZone;
            chrono::Local
                .timestamp_opt(secs, 0)
                .single()
                .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
        }
        .unwrap_or_default();
        let why = current_why()
            .filter(|w| !w.is_empty())
            .unwrap_or_else(|| "-".into());
        let mut line = format!(
            "{stamp} {:<9} {why:<12} {kind:<6} {mode:<5} {status:<9} {ms:>6}ms",
            self.process
        );
        if !bits.is_empty() {
            line.push_str("  ");
            line.push_str(&bits.join(" "));
        }
        line
    }
}
