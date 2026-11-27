//! rsi_cache.db and the lookup client against rsi.py itself
//! (tests/fixtures/make_rsi_ops.py): the same script of lookups, on the same
//! scripted network and the same fake clock, gives the same answers, sends
//! the same requests at the same moments, writes the same request-log lines,
//! keeps the same pauses and leaves the same rows after every step.

mod common;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use common::*;
use scw_rsi::model::{enlisted_epoch, parse_utc};
use scw_rsi::sentry::rate_left;
use scw_rsi::{
    Cache, Citizen, Client, Clock, LookupError, MEMBER_PAGE_CAP, Options, Org, Provider,
    RequestLog, Roster, org_url, profile_url, why,
};
use serde_json::{Map, Value, json};

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
    let mut fx = serde_json::from_slice(
        &std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rsi_ops.json"),
        )
        .unwrap(),
    )
    .unwrap();
    without_sci(&mut fx);
    fx
}

/// rsi.py's answers with this port's words: a handle not checked is "not
/// checked", without the sc-intelligence.net link the Python adds (sc-watch
/// points at no third-party lookup site).
fn without_sci(v: &mut Value) {
    match v {
        Value::String(s) if s.starts_with("not checked - https://sc-intelligence.net/") => {
            *s = "not checked".into();
        }
        Value::Array(a) => a.iter_mut().for_each(without_sci),
        Value::Object(m) => m.values_mut().for_each(without_sci),
        _ => {}
    }
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
        // [handle, the Python's sc-intelligence.net link, profile, org]
        let got = json!([h, profile_url(h), org_url(h)]);
        if got != json!([c[0], c[2], c[3]]) {
            bad.push(format!("urls: python {c} rust {got}"));
        }
    }
    drop(clients);
    drop(cur);
    drop(cache);
    let _ = std::fs::remove_dir_all(&root);
    assert!(bad.is_empty(), "{} differ\n{}", bad.len(), bad.join("\n"));
}
