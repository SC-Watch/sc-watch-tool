//! Sharing against share.py itself (tests/fixtures/make_share_ops.py): the
//! same exports, previews, imports and removals between three installs give
//! the same results and leave the same rows and flags.json after every
//! step; the zips each side writes are read by the other; and check(),
//! unpack() and unpack_b64() answer the same on every crafted input - except
//! the few where the port is deliberately stricter (DELIBERATE).

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use rusqlite::Connection;
use scw_store::flags::{self, Flags, UNKNOWN_COLOR};
use scw_store::reputation::{Database, RosterMember};
use scw_store::share::{self, Share};
use serde_json::{Map, Value, json};

/// Where the port refuses what share.py lets through, and what it says.
const DELIBERATE: &[(&str, &str)] = &[
    ("version true", "damaged share: no version"),
    ("install newline", "damaged share: no install id"),
    (
        "handle newline",
        "damaged share: 'EXMPLNL\\n' is not a handle",
    ),
    (
        "category newline",
        "damaged share: 'griefing\\n' is not a report reason",
    ),
];

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

fn sql_value(v: &Value) -> rusqlite::types::Value {
    match v {
        Value::Null => rusqlite::types::Value::Null,
        Value::Number(n) if n.is_f64() => rusqlite::types::Value::Real(n.as_f64().unwrap()),
        Value::Number(n) => rusqlite::types::Value::Integer(n.as_i64().unwrap()),
        Value::String(s) => rusqlite::types::Value::Text(s.clone()),
        other => panic!("not a parameter: {other}"),
    }
}

fn hex(h: &str) -> Vec<u8> {
    (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap())
        .collect()
}

fn outcome(r: Result<Value, share::ShareError>) -> Value {
    match r {
        Ok(v) => json!({"ok": v}),
        Err(share::ShareError::Refused(m)) => json!({"error": m}),
        Err(e) => panic!("not a refusal: {e}"),
    }
}

fn doc_json(d: &Share) -> Value {
    serde_json::to_value(d).unwrap()
}

/// The text of one member of a zip.
fn member(zip_bytes: &[u8], name: &str) -> String {
    use std::io::Read;
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).unwrap();
    let mut s = String::new();
    z.by_name(name).unwrap().read_to_string(&mut s).unwrap();
    s
}

#[test]
fn sharing_matches_share_py() {
    let fx: Value = serde_json::from_slice(
        &std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/share_ops.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let root = std::env::temp_dir().join(format!("scw-store-share-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let now = Rc::new(Cell::new(fx["t0"].as_f64().unwrap()));
    let who = ["bob", "alice", "carol"];
    let flags_path = |w: &str| root.join(w).join("flags.json");
    let mut dbs: HashMap<&str, Database> = HashMap::new();
    for w in who {
        std::fs::create_dir_all(root.join(w)).unwrap();
        let mut db = Database::open(&root.join(w).join("sc-watch.db")).unwrap();
        let n = now.clone();
        db.clock = Box::new(move || n.get());
        db.utc_dates = true;
        db.flags = Flags::load(&flags_path(w));
        dbs.insert(w, db);
    }
    let mut built: HashMap<String, Share> = HashMap::new();
    let mut received: HashMap<String, Share> = HashMap::new();
    let mut want_state = Value::Null;
    let mut bad = Vec::new();

    for (i, step) in fx["steps"].as_array().unwrap().iter().enumerate() {
        let at = step["at"].as_f64().unwrap();
        now.set(at);
        let op = step["op"].as_str().unwrap();
        let a = step["args"].as_array().unwrap();
        let s = |i: usize| a[i].as_str().unwrap().to_string();
        let w = || s(0);
        let result: Value = match op {
            "sql" => {
                let params: Vec<rusqlite::types::Value> =
                    a[2].as_array().unwrap().iter().map(sql_value).collect();
                dbs[w().as_str()]
                    .conn
                    .execute(&s(1), rusqlite::params_from_iter(params))
                    .unwrap();
                Value::Null
            }
            "flags.save" => {
                let written = flags::save(a[1].as_array().unwrap(), &flags_path(&w())).unwrap();
                dbs.get_mut(w().as_str()).unwrap().flags = Flags { all: written };
                Value::Null
            }
            "rep" => {
                let db = &dbs[w().as_str()];
                let args = a[2].as_array().unwrap();
                let t = |i: usize| args[i].as_str().unwrap();
                match s(1).as_str() {
                    "record_sighting" => {
                        db.record_sighting(
                            t(0),
                            args[1].as_f64(),
                            args[2].as_i64().unwrap(),
                            args[3].as_i64().unwrap(),
                        )
                        .unwrap();
                    }
                    "add_report" => db.add_report(t(0), t(1), t(2), "", "self", false).unwrap(),
                    "flag_org" => db.flag_org(t(0), t(1), t(2)).unwrap(),
                    "add_member_manually" => {
                        db.add_member_manually(t(0), t(1), t(2)).unwrap();
                    }
                    "remove_manual_member" => {
                        db.remove_manual_member(t(0), t(1)).unwrap();
                    }
                    "record_roster" => {
                        let members: Vec<RosterMember> = args[1]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|m| RosterMember {
                                handle: m["handle"].as_str().unwrap().into(),
                                display: m["display"].as_str().unwrap().into(),
                                rank: m["rank"].as_str().unwrap().into(),
                                stars: m["stars"].as_i64().unwrap(),
                            })
                            .collect();
                        db.record_roster(
                            t(0),
                            &members,
                            args[2].as_bool().unwrap(),
                            args[3].as_str(),
                        )
                        .unwrap();
                    }
                    other => panic!("the test does not know {other}"),
                }
                Value::Null
            }
            "build" => {
                let r = share::build(&dbs[w().as_str()], &s(1));
                if let (Ok(d), Some(label)) = (&r, a[2].as_str()) {
                    built.insert(label.into(), d.clone());
                }
                outcome(r.map(|d| doc_json(&d)))
            }
            "receive" => {
                let label = s(0);
                let mine = &built[&label];
                // The port's own zip, read back; and Python's zip, read by the port.
                let ours = share::pack(mine);
                let back = share::unpack(&ours, at).unwrap();
                let theirs =
                    share::unpack(&hex(step["result"]["zip"].as_str().unwrap()), at).unwrap();
                if back != theirs {
                    bad.push(format!(
                        "step {i} receive {label}: its own zip and Python's read differently"
                    ));
                }
                if share::share_json(mine) != step["result"]["json"].as_str().unwrap() {
                    bad.push(format!(
                        "step {i} receive {label}: share.json differs from Python's"
                    ));
                }
                if member(&ours, "share.json") != share::share_json(mine)
                    || member(&ours, "README.txt") != share::README
                {
                    bad.push(format!(
                        "step {i} receive {label}: its zip does not hold what it wrote"
                    ));
                }
                received.insert(label, back.clone());
                json!({"ok": doc_json(&back), "zip": step["result"]["zip"], "json": step["result"]["json"]})
            }
            "filename" => json!({"ok": share::filename(&built[&s(0)], at, true)}),
            "preview" => outcome(
                share::preview(&dbs[w().as_str()], &received[&s(1)])
                    .map(|p| serde_json::to_value(p).unwrap()),
            ),
            "apply" => {
                let fp = flags_path(&w());
                let db = dbs.get_mut(w().as_str()).unwrap();
                let flag = |i: usize| a[i].as_bool().unwrap();
                outcome(
                    share::apply(db, &received[&s(1)], &fp, flag(2), flag(3), flag(4))
                        .map(|x| serde_json::to_value(x).unwrap()),
                )
            }
            "sources" => outcome(
                share::sources(&dbs[w().as_str()]).map(|x| serde_json::to_value(x).unwrap()),
            ),
            "remove_source" => outcome(
                share::remove_source(&dbs[w().as_str()], &s(1))
                    .map(|x| serde_json::to_value(x).unwrap()),
            ),
            other => panic!("the test does not know {other}"),
        };
        if !same(&result, &step["result"]) {
            bad.push(format!(
                "step {i} {op} {:?}: result\n    python {}\n    rust   {result}",
                a.iter().filter(|x| !x.is_array()).collect::<Vec<_>>(),
                step["result"]
            ));
        }
        if !step["state"].is_null() {
            want_state = step["state"].clone();
        }
        for w in who {
            let db = dump(&root.join(w).join("sc-watch.db"));
            for (t, rows) in want_state[w]["db"].as_object().unwrap() {
                if !same(rows, &db[t.as_str()]) {
                    bad.push(format!(
                        "step {i} {op}: {w}'s {t}\n    python {rows}\n    rust   {}",
                        db[t.as_str()]
                    ));
                }
            }
            let fl = std::fs::read(flags_path(w))
                .map(|b| serde_json::from_slice::<Value>(&b).unwrap())
                .unwrap_or(Value::Null);
            if !same(&fl, &want_state[w]["flags"]) {
                bad.push(format!(
                    "step {i} {op}: {w}'s flags.json\n    python {}\n    rust   {fl}",
                    want_state[w]["flags"]
                ));
            }
        }
    }

    // check() on crafted documents
    let at = fx["check_at"].as_f64().unwrap();
    for c in fx["checks"].as_array().unwrap() {
        let label = c["label"].as_str().unwrap();
        let got = outcome(share::check(&c["doc"], at).map(|d| doc_json(&d)));
        let mut want = match c.get("error") {
            Some(e) => json!({"error": e}),
            None => json!({"ok": c["ok"]}),
        };
        if let Some((_, msg)) = DELIBERATE.iter().find(|(l, _)| *l == label) {
            want = json!({"error": msg});
        }
        if label == "color newline" {
            // Python kept "#abcdef\n" as the colour; the port does not pass it.
            for f in want["ok"]["flags"].as_array_mut().unwrap() {
                if f["key"] == "k5" {
                    f["color"] = json!(UNKNOWN_COLOR);
                }
            }
        }
        if !same(&got, &want) {
            bad.push(format!(
                "check {label}:\n    python {want}\n    rust   {got}"
            ));
        }
    }
    let mut big = fx["checks"][0]["doc"].clone();
    big = if big.is_object() {
        big
    } else {
        fx["checks"][1]["doc"].clone()
    };
    big["format"] = json!(share::FORMAT);
    big["reports"] = Value::Array(vec![json!({}); share::MAX_ROWS + 1]);
    if outcome(share::check(&big, at).map(|d| doc_json(&d)))
        != json!({"error": fx["too_many_rows"]})
    {
        bad.push("check: too many rows".into());
    }

    // unpack() on crafted zips, unpack_b64() on text
    for c in fx["unpacks"].as_array().unwrap() {
        let got =
            outcome(share::unpack(&hex(c["zip"].as_str().unwrap()), at).map(|d| doc_json(&d)));
        let want = match c.get("error") {
            Some(e) => json!({"error": e}),
            None => json!({"ok": c["ok"]}),
        };
        if !same(&got, &want) {
            bad.push(format!(
                "unpack {}:\n    python {want}\n    rust   {got}",
                c["label"]
            ));
        }
    }
    let too_big = outcome(share::unpack(&vec![0u8; share::MAX_ZIP + 1], at).map(|d| doc_json(&d)));
    if too_big != json!({"error": fx["too_big_zip"]}) {
        bad.push(format!("unpack too big: {too_big}"));
    }
    for c in fx["b64"].as_array().unwrap() {
        let text = c["text"].as_str().unwrap();
        let got = outcome(share::unpack_b64(text, at).map(|d| doc_json(&d)));
        let want = match c.get("error") {
            Some(e) => json!({"error": e}),
            None => json!({"ok": c["ok"]}),
        };
        if !same(&got, &want) {
            bad.push(format!(
                "unpack_b64 {text:?}:\n    python {want}\n    rust   {got}"
            ));
        }
    }
    if fx["readme"] != share::README {
        bad.push("README differs".into());
    }

    // A fresh install gets an id once: a version 4 UUID's hex, kept.
    let fresh = Database::open(&root.join("fresh.db")).unwrap();
    let id = share::install_id(&fresh).unwrap();
    let ok = id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        && &id[12..13] == "4"
        && "89ab".contains(&id[16..17]);
    if !ok || share::install_id(&fresh).unwrap() != id {
        bad.push(format!("install id {id}"));
    }

    drop(dbs);
    drop(fresh);
    let _ = std::fs::remove_dir_all(&root);
    assert!(bad.is_empty(), "{} differ\n{}", bad.len(), bad.join("\n"));
}
