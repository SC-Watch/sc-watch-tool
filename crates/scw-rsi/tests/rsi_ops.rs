//! rsi_cache.db and the lookup client against rsi.py itself
//! (tests/fixtures/make_rsi_ops.py): the same script of lookups, on the same
//! scripted network and the same fake clock, gives the same answers, sends
//! the same requests at the same moments, writes the same request-log lines,
//! keeps the same pauses and leaves the same rows after every step.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use scw_rsi::model::{enlisted_epoch, parse_utc};
use scw_rsi::sentry::rate_left;
use scw_rsi::{
    API_HOST, Cache, Citizen, Client, Clock, FetchError, Headers, LookupError, MEMBER_PAGE_CAP,
    Options, Org, Provider, Reply, RequestLog, Roster, SENTRY_HOST, Transport, lookup_url, org_url,
    profile_url, why,
};
use serde_json::{Map, Value, json};

struct FakeClock(Mutex<(f64, f64)>);

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

struct FakeNet {
    clock: Arc<FakeClock>,
    key: String,
    routes: Mutex<HashMap<String, Vec<Value>>>,
    sent: Mutex<Vec<Value>>,
}

fn headers(v: Option<&Value>) -> Headers {
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
fn same(a: &Value, b: &Value) -> bool {
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

fn dump(path: &Path) -> Value {
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

fn org_json(o: &Org) -> Value {
    serde_json::to_value(o).unwrap()
}

fn citizen_json(c: &Citizen, now: f64) -> Value {
    let mut v = serde_json::to_value(c).unwrap();
    let n = &c.note;
    let mut ex = Map::new();
    if n.awaiting_sentry {
        ex.insert("awaiting_sentry".into(), json!(true));
    }
    if let Some(s) = &n.checking {
        ex.insert("checking".into(), json!(s));
    }
    if let Some(r) = n.retry_in {
        ex.insert("retry_in".into(), json!(r));
    }
    if let Some(s) = n.source {
        ex.insert("source".into(), json!(s.as_str()));
    }
    if let Some(s) = &n.stale_reason {
        ex.insert("stale_reason".into(), json!(s));
    }
    if let Some(s) = n.stored_at {
        ex.insert("stored_at".into(), json!(s));
    }
    if !n.unknown_focus.is_empty() {
        ex.insert("unknown_focus".into(), json!(n.unknown_focus));
    }
    v["extras"] = Value::Object(ex);
    v["summary"] = json!(c.summary(now, true));
    v["flags"] = json!(c.flags(now, true));
    v["enlisted_days"] = json!(c.enlisted_days(now, true));
    v
}

fn roster_json(r: &Roster) -> Value {
    json!({
        "members": r.members.iter().map(|m| json!({
            "handle": m.handle, "display": m.display, "rank": m.rank, "stars": m.stars}))
            .collect::<Vec<_>>(),
        "complete": r.complete, "source": r.source, "total": r.total,
        "hidden": r.hidden, "redacted": r.redacted, "observed_at": r.observed_at,
    })
}

fn raised(c: Option<&Client>, e: &LookupError, mono: f64) -> Value {
    json!({
        "raised": e.type_name(),
        "describe": c.map_or_else(|| e.describe(mono), |c| c.describe(e)),
        "transient": e.is_transient(),
        "service": e.service,
    })
}

fn state(c: Option<&Client>) -> Value {
    let Some(c) = c else { return Value::Null };
    let (trouble, told) = c.trouble();
    let s = c.sentry();
    json!({
        "down": c.down_for(), "trouble": trouble, "told": told,
        "sentry_down": s.map(|s| s.down_for()),
        "sentry_told": s.map(|s| s.waits().0),
        "live": s.map(|s| s.waits().1),
        "ready": c.ready(), "enriches": c.enriches(), "has_key": c.has_key(),
    })
}

fn fixture() -> Value {
    serde_json::from_slice(
        &std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rsi_ops.json"),
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn lookups_match_rsi_py() {
    let fx = fixture();
    let root = std::env::temp_dir().join(format!("scw-rsi-ops-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let clock = Arc::new(FakeClock(Mutex::new((
        fx["wall0"].as_f64().unwrap(),
        fx["mono0"].as_f64().unwrap(),
    ))));
    let net = Arc::new(FakeNet {
        clock: clock.clone(),
        key: fx["key"].as_str().unwrap().into(),
        routes: Mutex::new(HashMap::new()),
        sent: Mutex::new(Vec::new()),
    });
    let log_path = root.join("debug_bursts").join("sentry_requests.log");
    let log = Arc::new(RequestLog {
        path: log_path.clone(),
        process: "test".into(),
        utc: true,
    });
    let lines = || -> Vec<String> {
        std::fs::read_to_string(&log_path)
            .map(|t| t.lines().map(String::from).collect())
            .unwrap_or_default()
    };
    let cache_path = root.join("rsi_cache.db");
    let mut cache = Arc::new(Cache::open(&cache_path, clock.clone()).unwrap());
    let mut clients: HashMap<String, Arc<Client>> = HashMap::new();
    let mut cur: Option<Arc<Client>> = None;
    let mut bad = Vec::new();
    let mut expected_tables = Value::Null;

    for (i, step) in fx["steps"].as_array().unwrap().iter().enumerate() {
        let op = step["op"].as_str().unwrap();
        let a = step["args"].as_array().unwrap();
        let kw = &step["kwargs"];
        let flag = |n: &str, d: bool| kw.get(n).and_then(Value::as_bool).unwrap_or(d);
        let s = |i: usize| a[i].as_str().unwrap().to_string();
        if (clock.time(), clock.monotonic())
            != (
                step["wall"].as_f64().unwrap(),
                step["mono"].as_f64().unwrap(),
            )
        {
            bad.push(format!("step {i} {op}: the clocks have drifted apart"));
        }
        {
            let mut routes = net.routes.lock().unwrap();
            for (k, v) in step["routes"].as_object().unwrap() {
                routes.insert(k.clone(), v.as_array().unwrap().clone());
            }
        }
        net.sent.lock().unwrap().clear();
        let before = lines().len();
        let _why = step["why"].as_str().map(why);
        let c = cur.clone();
        let result: Result<Value, LookupError> = match op {
            "client" => {
                let provider = match kw.get("provider").and_then(Value::as_str) {
                    Some("link") => Provider::Link,
                    Some("none") => Provider::None,
                    _ => Provider::Api,
                };
                let client = Arc::new(Client::new(
                    cache.clone(),
                    net.clone(),
                    clock.clone(),
                    Options {
                        api_key: kw.get("api_key").and_then(Value::as_str).map(String::from),
                        provider,
                        sentry: flag("sentry", true),
                        min_interval: 1.0,
                        log: Some(log.clone()),
                        ..Options::default()
                    },
                ));
                clients.insert(s(0), client.clone());
                cur = Some(client);
                Ok(Value::Null)
            }
            "use" => {
                cur = Some(clients[&s(0)].clone());
                Ok(Value::Null)
            }
            "advance" => {
                clock.sleep(a[0].as_f64().unwrap());
                Ok(Value::Null)
            }
            "reopen" => {
                cache = Arc::new(Cache::open(&cache_path, clock.clone()).unwrap());
                Ok(Value::Null)
            }
            "cache" => {
                let m = s(0);
                Ok(match m.as_str() {
                    "note_failure" => {
                        cache
                            .note_failure(
                                &s(1),
                                &s(2),
                                a[3].as_i64().unwrap(),
                                a[4].as_bool().unwrap(),
                            )
                            .unwrap();
                        Value::Null
                    }
                    "book_slot" => json!(cache.book_slot(&s(1), a[2].as_f64().unwrap()).unwrap()),
                    "put_stub" => {
                        let mut stub = Citizen::new(&s(1));
                        stub.fetched_at = clock.time();
                        cache.put_citizen(&stub, None).unwrap();
                        Value::Null
                    }
                    "drop_members" => {
                        cache.drop_members(&s(1)).unwrap();
                        Value::Null
                    }
                    "member_orgs" => json!(cache.member_orgs(&s(1)).unwrap()),
                    "partials" => Value::Array(
                        cache
                            .partials()
                            .unwrap()
                            .into_iter()
                            .map(|p| json!([p.handle, p.data, p.observed_at, p.unknown, p.reason]))
                            .collect(),
                    ),
                    "stats" => {
                        let mut v = cache.stats().unwrap();
                        v.as_object_mut().unwrap().remove("bytes");
                        v
                    }
                    "get_citizen" => cache
                        .get_citizen(&s(1), a.get(2).and_then(Value::as_bool).unwrap_or(false))
                        .unwrap()
                        .map_or(Value::Null, |c| citizen_json(&c, clock.time())),
                    "get_org" => cache
                        .get_org(&s(1), a.get(2).and_then(Value::as_bool).unwrap_or(false))
                        .unwrap()
                        .map_or(Value::Null, |o| org_json(&o)),
                    other => panic!("the test does not know cache.{other}"),
                })
            }
            _ => {
                let c = c.as_ref().expect("a client");
                match op {
                    "citizen" => c
                        .citizen(&s(0), flag("fresh", false), flag("live", true))
                        .map(|x| citizen_json(&x, clock.time())),
                    "exists" => c.exists(&s(0), flag("strict", false)).map(|x| json!(x)),
                    "org" => c.org(&s(0), flag("force", false)).map(|o| org_json(&o)),
                    "org_name" => c.org_name(&s(0), flag("ask", true)).map(|n| json!(n)),
                    "roster" => c
                        .roster(&s(0), MEMBER_PAGE_CAP, &mut |_, _| {})
                        .map(|r| roster_json(&r)),
                    "complete_partials" => c
                        .complete_partials(a[0].as_bool().unwrap())
                        .map(|n| serde_json::to_value(n).unwrap()),
                    other => panic!("the test does not know {other}"),
                }
            }
        };
        drop(_why);
        let got = match &result {
            Ok(v) => json!({"ok": v}),
            Err(e) => raised(cur.as_deref(), e, clock.monotonic()),
        };
        let want = &step["result"];
        if !same(&got, want) {
            bad.push(format!(
                "step {i} {op} {a:?}: result\n    python {want}\n    rust   {got}"
            ));
        }
        let sent = Value::Array(net.sent.lock().unwrap().clone());
        if !same(&sent, &step["sent"]) {
            bad.push(format!(
                "step {i} {op} {a:?}: requests\n    python {}\n    rust   {sent}",
                step["sent"]
            ));
        }
        let new: Vec<String> = lines().split_off(before.min(lines().len()));
        if json!(new) != step["log"] {
            bad.push(format!(
                "step {i} {op} {a:?}: log\n    python {}\n    rust   {}",
                step["log"],
                json!(new)
            ));
        }
        let st = state(cur.as_deref());
        if !same(&st, &step["state"]) {
            bad.push(format!(
                "step {i} {op} {a:?}: state\n    python {}\n    rust   {st}",
                step["state"]
            ));
        }
        // The fixture holds the tables only when a step changed them.
        if !step["dump"].is_null() {
            expected_tables = step["dump"].clone();
        }
        let d = dump(&cache_path);
        if !same(&d, &expected_tables) {
            let want = expected_tables.as_object().unwrap();
            for (t, rows) in want {
                if !same(rows, &d[t.as_str()]) {
                    bad.push(format!(
                        "step {i} {op} {a:?}: table {t}\n    python {rows}\n    rust   {}",
                        d[t.as_str()]
                    ));
                }
            }
        }
    }

    for c in fx["parse_utc"].as_array().unwrap() {
        let got = json!(parse_utc(Some(&c[0])));
        if !same(&got, &c[1]) {
            bad.push(format!("parse_utc {}: python {} rust {got}", c[0], c[1]));
        }
    }
    for c in fx["enlisted"].as_array().unwrap() {
        let got = json!(enlisted_epoch(c[0].as_str().unwrap(), true));
        if !same(&got, &c[1]) {
            bad.push(format!(
                "enlisted_epoch {}: python {} rust {got}",
                c[0], c[1]
            ));
        }
    }
    for c in fx["rate_left"].as_array().unwrap() {
        let h = (!c[0].is_null()).then(|| headers(Some(&c[0])));
        let got = json!(rate_left(h.as_ref()));
        if !same(&got, &c[1]) {
            bad.push(format!("rate_left {}: python {} rust {got}", c[0], c[1]));
        }
    }
    for c in fx["urls"].as_array().unwrap() {
        let h = c[0].as_str().unwrap();
        let got = json!([h, lookup_url(h), profile_url(h), org_url(h)]);
        if got != *c {
            bad.push(format!("urls: python {c} rust {got}"));
        }
    }
    drop(clients);
    drop(cur);
    drop(cache);
    let _ = std::fs::remove_dir_all(&root);
    assert!(bad.is_empty(), "{} differ\n{}", bad.len(), bad.join("\n"));
}
