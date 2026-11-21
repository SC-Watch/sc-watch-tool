//! sc-watch.db's schema as the port creates it, against the Python's
//! (tests/fixtures/reputation_fresh_schema.json, tools/dbdump.py
//! --schema-only of a fresh reputation.Database): every schema entry's
//! stored SQL text and every table's columns. Migrating again changes
//! nothing. (Migrations of real, older databases are compared by
//! tools/migrate_compare.py, which needs them and the Python.)

use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::PathBuf;

fn dump(db: &Connection) -> Value {
    let mut stmt = db
        .prepare(
            "SELECT type, name, tbl_name, sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
        )
        .unwrap();
    let schema: Vec<Value> = stmt
        .query_map([], |r| {
            Ok(json!([
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?
            ]))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut tables = serde_json::Map::new();
    let names: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for t in names {
        let mut stmt = db.prepare(&format!("PRAGMA table_info({t})")).unwrap();
        let cols: Vec<Value> = stmt
            .query_map([], |r| {
                Ok(json!([
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, i64>(5)?
                ]))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        tables.insert(t, json!({"columns": cols}));
    }
    json!({"schema": schema, "tables": tables})
}

#[test]
fn fresh_schema_is_the_pythons() {
    let dir = std::env::temp_dir().join(format!("scw-store-schema-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sc-watch.db");
    let db = scw_store::db::open(&path).unwrap();
    scw_store::reputation::migrate(&db).unwrap();
    let want: Value = serde_json::from_slice(
        &std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/reputation_fresh_schema.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let got = dump(&db);
    assert_eq!(want["schema"], got["schema"], "schema entries differ");
    assert_eq!(want["tables"], got["tables"], "columns differ");
    // Again: nothing changes.
    scw_store::reputation::migrate(&db).unwrap();
    assert_eq!(got, dump(&db), "a second migration changed the schema");
    let mode: String = db
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "wal");
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sketches() {
    use scw_store::reputation::sketch;
    assert_eq!(sketch("5LIVER"), sketch("SLIVER"));
    // L and _ collapse to I, 0 to O.
    assert_eq!(sketch("exmpl_0name"), "EXMPIIONAME");
    // reputation.sketch()'s own outputs:
    for (h, want) in [
        ("5LIVER", "SIIUER"),
        ("QD0O", "OOOO"),
        ("TY7", "TTT"),
        ("Stra\u{df}e", "STRASSE"),
        ("cgUV8b", "CCUUBB"),
    ] {
        assert_eq!(sketch(h), want, "sketch({h})");
    }
}
