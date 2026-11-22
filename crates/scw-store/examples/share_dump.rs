//! An export from a COPY of sc-watch.db, as the port makes it, imported into
//! a fresh empty database - for tools/share_compare.py.
//!
//!     share_dump <db copy> <flags copy> <target db> <target flags> <out.json> <now>

use std::path::Path;

use scw_store::flags::Flags;
use scw_store::reputation::Database;
use scw_store::share;
use serde_json::{Map, Value, json};

fn rows(db: &Database) -> Value {
    let tables: Vec<String> = db
        .conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut out = Map::new();
    for t in tables {
        let mut s = db.conn.prepare(&format!("SELECT * FROM \"{t}\"")).unwrap();
        let n = s.column_count();
        let mut got: Vec<Value> = s
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
        got.sort_by_key(|r| r.to_string());
        out.insert(t, Value::Array(got));
    }
    Value::Object(out)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let now: f64 = a[6].parse().unwrap();
    let mut db = Database::open(Path::new(&a[1])).unwrap();
    db.clock = Box::new(move || now);
    db.flags = Flags::load(Path::new(&a[2]));
    let doc = share::build(&db, "Compare").unwrap();
    let text = share::share_json(&doc);
    let checked = share::unpack(&share::pack(&doc), now).unwrap();
    let mut target = Database::open(Path::new(&a[3])).unwrap();
    target.clock = Box::new(move || now);
    let tflags = Path::new(&a[4]);
    target.flags = Flags::load(tflags);
    let preview = share::preview(&target, &checked).unwrap();
    let applied = share::apply(&mut target, &checked, tflags, true, true, true).unwrap();
    let sources = share::sources(&target).unwrap();
    let flags: Value = std::fs::read(tflags)
        .map(|b| serde_json::from_slice(&b).unwrap())
        .unwrap_or(Value::Null);
    let out = json!({
        "doc": doc, "text": text, "checked": checked, "preview": preview,
        "applied": applied, "sources": sources, "rows": rows(&target), "flags": flags,
    });
    std::fs::write(&a[5], serde_json::to_string(&out).unwrap()).unwrap();
}
