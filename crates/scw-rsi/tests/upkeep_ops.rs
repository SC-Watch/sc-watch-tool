//! find_ghosts, purge_ghosts, fetch_roster and the client as the
//! database's verifier, against reputation.py with rsi.py
//! (tests/fixtures/make_upkeep_ops.py): the same steps on the same scripted
//! network give the same results, send the same requests, and leave the
//! same rows in sc-watch.db and rsi_cache.db.

mod common;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use common::*;
use scw_rsi::{
    Cache, Citizen, Client, Clock, Options, Provider, Verifier, fetch_roster, find_ghosts,
    purge_ghosts,
};
use scw_store::reputation::{Database, Verify};
use serde_json::{Value, json};

/// find_ghosts's errors by type only, as the fixture keeps them.
fn plain(mut v: Value) -> Value {
    if let Some(errs) = v.get_mut("errors").and_then(Value::as_array_mut) {
        for e in errs {
            let kind = e[1]
                .as_str()
                .unwrap()
                .split(':')
                .next()
                .unwrap()
                .to_string();
            e[1] = json!(kind);
        }
    }
    v
}

#[test]
fn upkeep_matches_reputation_py() {
    let fx: Value = serde_json::from_slice(
        &std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/upkeep_ops.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let root = std::env::temp_dir().join(format!("scw-rsi-upkeep-{}", std::process::id()));
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
    let cache = Arc::new(Cache::open(&root.join("rsi_cache.db"), clock.clone()).unwrap());
    let mut rep = Database::open(&root.join("sc-watch.db")).unwrap();
    let c = clock.clone();
    rep.clock = Box::new(move || c.time());
    rep.utc_dates = true;
    let mut clients: HashMap<String, Arc<Client>> = HashMap::new();
    let mut cur: Option<Arc<Client>> = None;
    let mut want_tables = Value::Null;
    let mut bad = Vec::new();

    for (i, step) in fx["steps"].as_array().unwrap().iter().enumerate() {
        let op = step["op"].as_str().unwrap();
        let a = step["args"].as_array().unwrap();
        let kw = &step["kwargs"];
        let s = |i: usize| a[i].as_str().unwrap().to_string();
        let f = |i: usize| a[i].as_f64().unwrap();
        let n = |i: usize| a[i].as_i64().unwrap();
        let opt = |i: usize| a.get(i).and_then(Value::as_str).unwrap_or("").to_string();
        {
            let mut routes = net.routes.lock().unwrap();
            for (k, v) in step["routes"].as_object().unwrap() {
                routes.insert(k.clone(), v.as_array().unwrap().clone());
            }
        }
        net.sent.lock().unwrap().clear();
        let result: Value = match op {
            "client" => {
                let client = Arc::new(Client::new(
                    cache.clone(),
                    net.clone(),
                    clock.clone(),
                    Options {
                        api_key: kw.get("api_key").and_then(Value::as_str).map(String::from),
                        provider: match kw.get("provider").and_then(Value::as_str) {
                            Some("link") => Provider::Link,
                            _ => Provider::Api,
                        },
                        sentry: kw["sentry"].as_bool().unwrap(),
                        min_interval: 1.0,
                        ..Options::default()
                    },
                ));
                clients.insert(s(0), client.clone());
                cur = Some(client);
                Value::Null
            }
            "use" => {
                cur = Some(clients[&s(0)].clone());
                Value::Null
            }
            "advance" => {
                clock.sleep(f(0));
                Value::Null
            }
            "cache.put_gone" => {
                let gone = Citizen {
                    exists: false,
                    fetched_at: clock.time(),
                    ..Citizen::new(&s(0))
                };
                cache.put_citizen(&gone, None).unwrap();
                Value::Null
            }
            "rep.record_sighting" => {
                json!(rep.record_sighting(&s(0), Some(f(1)), n(2), n(3)).unwrap())
            }
            "rep.add_report" => {
                rep.add_report(&s(0), &s(1), &opt(2), "", "self", false)
                    .unwrap();
                Value::Null
            }
            "rep.add_audit" => json!(
                rep.add_audit(
                    &s(0),
                    Some(f(1)),
                    n(2),
                    n(3),
                    &a[4],
                    "",
                    "",
                    "",
                    "weak_read"
                )
                .unwrap()
            ),
            "rep.flag_org" => {
                rep.flag_org(&s(0), &opt(1), &opt(2)).unwrap();
                Value::Null
            }
            "verify" => json!(Verifier(cur.clone().unwrap()).exists(&s(0)).unwrap()),
            _ => {
                let c = cur.as_deref().unwrap();
                match op {
                    "find_ghosts" => {
                        let limit = kw.get("limit").and_then(Value::as_u64).unwrap_or(0);
                        plain(json!(find_ghosts(&rep, c, limit as usize).unwrap()))
                    }
                    "purge_ghosts" => {
                        let apply = kw.get("apply").and_then(Value::as_bool).unwrap_or(false);
                        plain(json!(purge_ghosts(&rep, c, apply).unwrap()))
                    }
                    "fetch_roster" => fetch_roster(&rep, c, &s(0), None, &mut |_, _| {})
                        .unwrap()
                        .to_json(),
                    other => panic!("the test does not know {other}"),
                }
            }
        };
        if !same(&result, &step["result"]) {
            bad.push(format!(
                "step {i} {op} {a:?}: result\n    python {}\n    rust   {result}",
                step["result"]
            ));
        }
        let sent = Value::Array(net.sent.lock().unwrap().clone());
        if !same(&sent, &step["sent"]) {
            bad.push(format!(
                "step {i} {op} {a:?}: requests\n    python {}\n    rust   {sent}",
                step["sent"]
            ));
        }
        if !step["dump"].is_null() {
            want_tables = step["dump"].clone();
        }
        let got = json!({"rep": dump(&root.join("sc-watch.db")), "rsi": dump(&root.join("rsi_cache.db"))});
        for db in ["rep", "rsi"] {
            for (t, rows) in want_tables[db].as_object().unwrap() {
                if !same(rows, &got[db][t.as_str()]) {
                    bad.push(format!(
                        "step {i} {op} {a:?}: {db} table {t}\n    python {rows}\n    rust   {}",
                        got[db][t.as_str()]
                    ));
                }
            }
        }
    }
    drop(rep);
    drop(clients);
    drop(cur);
    drop(cache);
    let _ = std::fs::remove_dir_all(&root);
    assert!(bad.is_empty(), "{} differ\n{}", bad.len(), bad.join("\n"));
}
