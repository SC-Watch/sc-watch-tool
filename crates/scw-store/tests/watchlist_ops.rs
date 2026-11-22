//! watchlist.db against watchlist.py itself (tests/fixtures/
//! make_watchlist_ops.py): the same operations give the same results and
//! the same rows after each; parse_lines and safe_url as the Python; and an
//! assessment with the watchlist attached says and decides the same.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use rusqlite::Connection;
use scw_store::reputation::Database;
use scw_store::watchlist::{Watchlist, import_text, parse_lines, safe_url, seed_starter};
use serde_json::{Value, json};

fn fixture() -> Value {
    serde_json::from_slice(
        &std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/watchlist_ops.json"),
        )
        .unwrap(),
    )
    .unwrap()
}

fn dump(path: &std::path::Path) -> Value {
    let db = Connection::open(path).unwrap();
    let tables: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut out = serde_json::Map::new();
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
        let n = cols.len();
        let rows: Vec<Value> = db
            .prepare(&format!("SELECT {names} FROM \"{t}\" ORDER BY {names}"))
            .unwrap()
            .query_map([], |r| {
                Ok(Value::Array(
                    (0..n)
                        .map(|i| match r.get_ref(i).unwrap() {
                            rusqlite::types::ValueRef::Null => Value::Null,
                            rusqlite::types::ValueRef::Integer(x) => json!(x),
                            rusqlite::types::ValueRef::Real(x) => json!(x),
                            rusqlite::types::ValueRef::Text(x) => json!(String::from_utf8_lossy(x)),
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

fn hex(h: &str) -> Vec<u8> {
    (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap())
        .collect()
}

fn hits_json(hits: &[scw_store::watchlist::Hit]) -> Value {
    Value::Array(
        hits.iter()
            .map(|h| {
                let mut v = serde_json::to_value(h).unwrap();
                v["describe"] = Value::String(h.describe());
                v
            })
            .collect(),
    )
}

#[test]
fn watchlist_matches_watchlist_py() {
    let fx = fixture();
    let root = std::env::temp_dir().join(format!("scw-store-wl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    for (name, h) in fx["starters"].as_object().unwrap() {
        std::fs::write(root.join(name), hex(h.as_str().unwrap())).unwrap();
    }
    let path = root.join("watchlist.db");
    let mut wl = Watchlist::open(&path).unwrap();
    let now = Rc::new(Cell::new(0.0));
    let c = now.clone();
    wl.clock = Box::new(move || c.get());
    wl.starter_name = "EXMPL bundled list".into();
    let mut bad = Vec::new();
    for (i, step) in fx["steps"].as_array().unwrap().iter().enumerate() {
        now.set(step["at"].as_f64().unwrap());
        let a = step["args"].as_array().unwrap();
        let kw = &step["kwargs"];
        let k = |n: &str| kw.get(n).and_then(Value::as_str).unwrap_or("").to_string();
        let s = |i: usize| a[i].as_str().unwrap_or("").to_string();
        let id = |i: usize| a[i].as_i64().unwrap();
        let op = step["op"].as_str().unwrap();
        let result = match op {
            "add_source" => json!(wl.add_source(&s(0), &s(1), &s(2)).unwrap()),
            "add_entry" => {
                let conf = kw
                    .get("confidence")
                    .and_then(Value::as_str)
                    .unwrap_or("asserted");
                json!(
                    wl.add_entry(
                        id(0),
                        &s(1),
                        conf,
                        &k("claim"),
                        &k("alt_of"),
                        &k("reported_by"),
                        &k("posted_ts"),
                        &k("msg_id"),
                        &k("msg_url")
                    )
                    .unwrap()
                )
            }
            "add_org" => {
                wl.add_org(id(0), &s(1), &k("claim"), &k("msg_id")).unwrap();
                Value::Null
            }
            "mark_synced" => {
                wl.mark_synced(id(0)).unwrap();
                Value::Null
            }
            "lookup" => hits_json(
                &wl.lookup(&s(0), kw.get("variants").and_then(Value::as_bool))
                    .unwrap(),
            ),
            "reason_coverage" => json!(wl.reason_coverage().unwrap()),
            "stats" => {
                let mut v = wl.stats().unwrap();
                v.as_object_mut().unwrap().remove("path");
                v
            }
            "orgs_flagged" => json!(wl.orgs_flagged().unwrap()),
            "sources" => json!(wl.sources().unwrap()),
            "seed" => seed_starter(&root.join(s(0)), &wl).unwrap(),
            "import_text" => {
                let conf = kw
                    .get("confidence")
                    .and_then(Value::as_str)
                    .unwrap_or("asserted");
                import_text(
                    &wl,
                    &s(0),
                    &s(1),
                    &k("url"),
                    &k("note"),
                    conf,
                    &k("reported_by"),
                    &k("posted_ts"),
                )
                .unwrap()
            }
            "set_enabled" => {
                wl.set_enabled(id(0), a[1].as_bool().unwrap()).unwrap();
                Value::Null
            }
            "forget_source" => json!(wl.forget_source(id(0)).unwrap()),
            other => panic!("the test does not know {other}"),
        };
        if result != step["result"] {
            bad.push(format!(
                "step {i} {op}: result\n    python {}\n    rust   {result}",
                step["result"]
            ));
        }
        let d = dump(&path);
        if d != step["dump"] {
            let want = step["dump"].as_object().unwrap();
            let differ: Vec<&String> = want.keys().filter(|t| want[*t] != d[t.as_str()]).collect();
            bad.push(format!("step {i} {op}: tables differ {differ:?}"));
        }
    }
    for c in fx["parse"].as_array().unwrap() {
        let got = parse_lines(c["text"].as_str().unwrap());
        if got != c["result"] {
            bad.push(format!(
                "parse_lines {:?}:\n    python {}\n    rust   {got}",
                c["text"], c["result"]
            ));
        }
    }
    for c in fx["safe_url"].as_array().unwrap() {
        let (u, want) = (c[0].as_str().unwrap(), c[1].as_str().unwrap());
        if safe_url(u) != want {
            bad.push(format!(
                "safe_url {u:?}: python {want:?} rust {:?}",
                safe_url(u)
            ));
        }
    }
    // The reputation database with this watchlist attached.
    let at = fx["assess_at"].as_f64().unwrap();
    now.set(at);
    let mut rep = Database::open(&root.join("sc-watch.db")).unwrap();
    rep.clock = Box::new(move || at);
    rep.utc_dates = true;
    rep.watchlist = Some(Box::new(wl));
    rep.record_sighting("EXMPLBAD", Some(2.0), 2, 2).unwrap();
    rep.record_sighting("EXMPLONAME", Some(2.0), 2, 2).unwrap();
    for c in fx["assessed"].as_array().unwrap() {
        let h = c["handle"].as_str().unwrap();
        let a = rep
            .assess(h, c["variants"].as_bool(), false)
            .unwrap()
            .to_json(at, &rep.flags, true);
        if a != c["assess"] {
            bad.push(format!(
                "assess {h} {}:\n    python {}\n    rust   {a}",
                c["variants"], c["assess"]
            ));
        }
        if json!(rep.is_listed(h).unwrap()) != c["listed"] {
            bad.push(format!("is_listed {h}"));
        }
    }
    drop(rep);
    let _ = std::fs::remove_dir_all(&root);
    assert!(bad.is_empty(), "{} differ\n{}", bad.len(), bad.join("\n"));
}
