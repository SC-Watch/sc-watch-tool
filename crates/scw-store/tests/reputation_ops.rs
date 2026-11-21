//! sc-watch.db's Database against reputation.Database itself
//! (tests/fixtures/make_reputation_ops.py): the same 107 operations at the
//! same fixed times, with the same scripted "is this a real account"
//! answers - and after each, the same return value and the same rows in
//! every table, and the same audit image files left on disk.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use rusqlite::Connection;
use scw_store::reputation::{Database, ProfileOrg, RosterMember, Verify};
use serde_json::{Value, json};

struct Scripted(serde_json::Map<String, Value>);

impl Verify for Scripted {
    fn exists(&self, handle: &str) -> Result<Option<bool>, String> {
        Ok(self.0.get(&handle.to_uppercase()).and_then(Value::as_bool))
    }
}

fn dump(path: &std::path::Path, root: &std::path::Path) -> Value {
    let db = Connection::open(path).unwrap();
    let mut out = serde_json::Map::new();
    let tables: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
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
        let mut stmt = db
            .prepare(&format!("SELECT {names} FROM \"{t}\" ORDER BY {names}"))
            .unwrap();
        let n = cols.len();
        let rows: Vec<Value> = stmt
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
    let mut files: Vec<String> = std::fs::read_dir(root.join("audit"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    out.insert("audit files".into(), json!(files));
    Value::Object(out)
}

/// Equal, a computed float allowed 1e-12 relative (pow's last bit differs
/// between maths libraries).
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => match (x.as_f64(), y.as_f64()) {
            (Some(p), Some(q)) => p == q || (p - q).abs() <= 1e-12 * p.abs().max(q.abs()),
            _ => x == y,
        },
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y)
                    .all(|((k1, v1), (k2, v2))| k1 == k2 && same(v1, v2))
        }
        _ => a == b,
    }
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap_or("")
}

fn member(v: &Value) -> RosterMember {
    let stars = match &v["stars"] {
        Value::Number(n) => n
            .as_i64()
            .unwrap_or_else(|| n.as_f64().unwrap_or(0.0) as i64),
        _ => 0,
    };
    RosterMember {
        handle: s(&v["handle"]).into(),
        display: s(&v["display"]).into(),
        rank: s(&v["rank"]).into(),
        stars,
    }
}

fn run(db: &Database, op: &str, a: &[Value], kw: &Value, now: f64) -> Value {
    let arg = |i: usize| a.get(i).cloned().unwrap_or(Value::Null);
    let text = |i: usize, d: &str| a.get(i).and_then(Value::as_str).unwrap_or(d).to_string();
    let kws = |k: &str, d: &str| kw.get(k).and_then(Value::as_str).unwrap_or(d).to_string();
    let kwb = |k: &str| kw.get(k).and_then(Value::as_bool).unwrap_or(false);
    let int = |i: usize| a.get(i).and_then(Value::as_i64).unwrap_or(0);
    let to = |x: &dyn erased::Ser| x.json();
    match op {
        "stats" => {
            let mut v = serde_json::to_value(db.stats().unwrap()).unwrap();
            v.as_object_mut().unwrap().remove("bytes");
            v
        }
        "record_sighting" => json!(
            db.record_sighting(&text(0, ""), arg(1).as_f64(), int(2), int(3))
                .unwrap()
        ),
        "add_audit" => json!(
            db.add_audit(
                &text(0, ""),
                arg(1).as_f64(),
                int(2),
                int(3),
                &arg(4),
                &text(5, ""),
                &text(6, ""),
                &text(7, ""),
                &kws("reason", "weak_read")
            )
            .unwrap()
        ),
        "audits_for" => to(&db.audits_for(&text(0, "")).unwrap()),
        "canonical" => {
            let (h, from) = db
                .canonical(
                    &text(0, ""),
                    kw.get("mutate").and_then(Value::as_bool).unwrap_or(true),
                )
                .unwrap();
            json!([h, from])
        }
        "ensure_player" => json!(db.ensure_player(&text(0, "")).unwrap()),
        "update_profile" => {
            db.update_profile(&text(0, ""), &text(1, ""), &text(2, ""))
                .unwrap();
            Value::Null
        }
        "add_report" => {
            db.add_report(
                &text(0, ""),
                &text(1, ""),
                &text(2, ""),
                &text(3, ""),
                &kws("source", "self"),
                kwb("exact"),
            )
            .unwrap();
            Value::Null
        }
        "pending_reports" => json!(db.pending_reports().unwrap()),
        "resolve_pending" => json!(
            db.resolve_pending(&text(0, ""), &text(1, ""), &text(2, ""), &text(3, ""))
                .unwrap()
        ),
        "edit_report" => json!(
            db.edit_report(
                int(0),
                kw.get("category").and_then(Value::as_str),
                kw.get("note").and_then(Value::as_str),
                kw.get("evidence").and_then(Value::as_str)
            )
            .unwrap()
        ),
        "delete_report" => to(&db.delete_report(int(0)).unwrap()),
        "add_ignored" => to(&db.add_ignored(&text(0, ""), &text(1, "")).unwrap()),
        "is_ignored" => json!(db.is_ignored(&text(0, "")).unwrap()),
        "ignored_list" => to(&db.ignored_list().unwrap()),
        "remove_ignored" => json!(db.remove_ignored(&text(0, "")).unwrap()),
        "flag_org" => {
            db.flag_org(&text(0, ""), &text(1, ""), &kws("name", ""))
                .unwrap();
            Value::Null
        }
        "unnamed_flagged" => json!(db.unnamed_flagged().unwrap()),
        "set_flag_name" => json!(db.set_flag_name(&text(0, ""), &text(1, "")).unwrap()),
        "record_roster" => {
            let members: Vec<RosterMember> =
                arg(1).as_array().unwrap().iter().map(member).collect();
            let complete = kw.get("complete").and_then(Value::as_bool).unwrap_or(true);
            to(&db
                .record_roster(
                    &text(0, ""),
                    &members,
                    complete,
                    kw.get("source").and_then(Value::as_str),
                )
                .unwrap())
        }
        "members_of" => to(&db.members_of(&text(0, "")).unwrap()),
        "add_member_manually" => to(&db
            .add_member_manually(&text(0, ""), &text(1, ""), &text(2, ""))
            .unwrap()),
        "remove_manual_member" => to(&db.remove_manual_member(&text(0, ""), &text(1, "")).unwrap()),
        "set_orgs" => {
            let orgs: Vec<ProfileOrg> = arg(1)
                .as_array()
                .unwrap()
                .iter()
                .map(|o| ProfileOrg {
                    sid: s(&o["sid"]).into(),
                    is_main: o["is_main"].as_bool().unwrap_or(false),
                })
                .collect();
            db.set_orgs(&text(0, ""), &orgs).unwrap();
            Value::Null
        }
        "flagged_orgs" => to(&db.flagged_orgs().unwrap()),
        "set_org_manual" => to(&db
            .set_org_manual(&text(0, ""), &text(1, ""), &text(2, ""), &text(3, ""))
            .unwrap()),
        "org_manual" => json!(db.org_manual().unwrap()),
        "set_manual_enlisted" => {
            db.set_manual_enlisted(&text(0, ""), &text(1, "")).unwrap();
            Value::Null
        }
        "manual_enlisted" => {
            let hs: Vec<String> = arg(0)
                .as_array()
                .unwrap()
                .iter()
                .map(|h| s(h).to_string())
                .collect();
            json!(db.manual_enlisted(&hs).unwrap())
        }
        "is_listed" => json!(db.is_listed(&text(0, "")).unwrap()),
        "assess" => db
            .assess(&text(0, ""), None, kwb("exact"))
            .unwrap()
            .to_json(now, &db.flags, true),
        "confirm_audit" => to(&db
            .confirm_audit(int(0), &text(1, ""), kwb("force"))
            .unwrap()),
        "dismiss_audit" => json!(db.dismiss_audit(int(0)).unwrap()),
        "orphan_audits" => to(&db.orphan_audits(kwb("apply")).unwrap()),
        "audit_files_in_use" => json!(db.audit_files_in_use().unwrap()),
        "prune_orphan_audit_files" => json!(db.prune_orphan_audit_files(kwb("apply")).unwrap()),
        "prune_audits" => to(&db.prune_audits(7.0).unwrap()),
        "open_audits" => to(&db.open_audits().unwrap()),
        "forget" => to(&db.forget(&text(0, "")).unwrap()),
        "consolidate" => json!(db.consolidate().unwrap()),
        "unflag_org" => {
            db.unflag_org(&text(0, "")).unwrap();
            Value::Null
        }
        "recent" => to(&db.recent(int(0)).unwrap()),
        other => panic!("the test does not know {other}"),
    }
}

/// Serialising any result the same way.
mod erased {
    pub trait Ser {
        fn json(&self) -> serde_json::Value;
    }
    impl<T: serde::Serialize> Ser for T {
        fn json(&self) -> serde_json::Value {
            serde_json::to_value(self).unwrap()
        }
    }
}

#[test]
fn operations_match_reputation_py() {
    let fixture: Value = serde_json::from_slice(
        &std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/reputation_ops.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let root = std::env::temp_dir().join(format!("scw-store-ops-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("audit")).unwrap();
    for f in fixture["audit_files"].as_array().unwrap() {
        std::fs::write(root.join("audit").join(s(f)), b"x").unwrap();
    }
    let path = root.join("sc-watch.db");
    let mut db = Database::open(&path).unwrap();
    let now = Rc::new(Cell::new(0.0));
    let clock = now.clone();
    db.clock = Box::new(move || clock.get());
    db.verify = Some(Box::new(Scripted(
        fixture["real"].as_object().unwrap().clone(),
    )));
    db.utc_dates = true;
    let mut bad = Vec::new();
    for (i, step) in fixture["steps"].as_array().unwrap().iter().enumerate() {
        let at = step["at"].as_f64().unwrap();
        now.set(at);
        let op = s(&step["op"]);
        let result = run(
            &db,
            op,
            step["args"].as_array().unwrap(),
            &step["kwargs"],
            at,
        );
        if !same(&step["result"], &result) {
            bad.push(format!(
                "step {i} {op} {}: result\n    python {}\n    rust   {result}",
                step["args"], step["result"]
            ));
        }
        let d = dump(&path, &root);
        if !same(&step["dump"], &d) {
            let want = step["dump"].as_object().unwrap();
            let differ: Vec<&String> = want
                .keys()
                .filter(|k| !same(&want[*k], &d[k.as_str()]))
                .collect();
            bad.push(format!("step {i} {op}: tables differ {differ:?}"));
        }
        if bad.len() > 6 {
            break;
        }
    }
    drop(db);
    let _ = std::fs::remove_dir_all(&root);
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
