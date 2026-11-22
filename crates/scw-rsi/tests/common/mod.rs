//! The fake clock, scripted network and table dump both replay tests run
//! on - the Rust side of fixtures/fakenet.py.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use scw_rsi::{API_HOST, Clock, FetchError, Headers, Reply, SENTRY_HOST, Transport};
use serde_json::{Map, Value, json};

pub struct FakeClock(pub Mutex<(f64, f64)>);

impl Clock for FakeClock {
    fn time(&self) -> f64 {
        self.0.lock().unwrap().0
    }
    fn monotonic(&self) -> f64 {
        self.0.lock().unwrap().1
    }
    fn sleep(&self, s: f64) {
        if s > 0.0 {
            let mut c = self.0.lock().unwrap();
            c.0 += s;
            c.1 += s;
        }
    }
    fn uniform(&self, a: f64, b: f64) -> f64 {
        (a + b) / 2.0
    }
}

pub struct FakeNet {
    pub clock: Arc<FakeClock>,
    pub key: String,
    pub routes: Mutex<HashMap<String, Vec<Value>>>,
    pub sent: Mutex<Vec<Value>>,
}

pub fn headers(v: Option<&Value>) -> Headers {
    Headers(
        v.and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
                    .collect()
            })
            .unwrap_or_default(),
    )
}

impl Transport for FakeNet {
    fn get(&self, url: &str, _ua: &str, timeout: f64) -> Result<Reply, FetchError> {
        let sci = format!("{API_HOST}/{}/v1/", self.key);
        let key = if let Some(rest) = url.strip_prefix(SENTRY_HOST) {
            format!("sentry:{rest}")
        } else if let Some(rest) = url.strip_prefix(&sci) {
            format!("sci:{rest}")
        } else {
            panic!("a request to an unknown host: {url}")
        };
        let r = {
            let mut routes = self.routes.lock().unwrap();
            let q = routes
                .get_mut(&key)
                .filter(|q| !q.is_empty())
                .unwrap_or_else(|| panic!("unscripted request {key}"));
            if q.len() > 1 {
                q.remove(0)
            } else {
                q[0].clone()
            }
        };
        self.sent.lock().unwrap().push(json!({
            "url": key, "timeout": timeout, "at": self.clock.time()}));
        self.clock
            .sleep(r.get("took").and_then(Value::as_f64).unwrap_or(0.25));
        let h = headers(r.get("headers"));
        if let Some(code) = r.get("http") {
            return Err(FetchError::Http {
                code: code.as_u64().unwrap() as u16,
                headers: h,
            });
        }
        match r.get("error").and_then(Value::as_str) {
            Some("timeout") => Err(FetchError::Timeout),
            Some("connect") => Err(FetchError::Connect("<urlopen error refused>".into())),
            Some("badjson") => Err(FetchError::BadJson),
            Some(other) => panic!("unknown error {other}"),
            None => Ok(Reply {
                body: r["json"].clone(),
                headers: h,
            }),
        }
    }
}

/// Equal as JSON, numbers by value (Python's 25 and 25.0 are the same).
pub fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

pub fn dump(path: &Path) -> Value {
    let db = Connection::open(path).unwrap();
    let tables: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut out = Map::new();
    for t in tables {
        let cols: Vec<String> = db
            .prepare(&format!("PRAGMA table_info({t})"))
            .unwrap()
            .query_map([], |r| r.get(1))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let names = cols
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let rows: Vec<Value> = db
            .prepare(&format!("SELECT {names} FROM \"{t}\" ORDER BY {names}"))
            .unwrap()
            .query_map([], |r| {
                Ok(Value::Array(
                    (0..cols.len())
                        .map(|i| match r.get_ref(i).unwrap() {
                            rusqlite::types::ValueRef::Null => Value::Null,
                            rusqlite::types::ValueRef::Integer(x) => json!(x),
                            rusqlite::types::ValueRef::Real(x) => json!(x),
                            rusqlite::types::ValueRef::Text(x) => {
                                json!(String::from_utf8_lossy(x))
                            }
                            rusqlite::types::ValueRef::Blob(_) => json!("<blob>"),
                        })
                        .collect(),
                ))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        out.insert(t, Value::Array(rows));
    }
    Value::Object(out)
}
