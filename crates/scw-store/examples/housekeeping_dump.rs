//! A DRY RUN of housekeeping over a data folder, as the port does it - for
//! tools/housekeeping_compare.py. Deletes nothing: every call here is made
//! with apply = false, and the audits are counted against a COPY of
//! sc-watch.db (pointed at the folder's audit/ only to list its files).
//!
//!     housekeeping_dump <data folder> <legacy housekeeping.json> <settings as loaded> <db copy> <out.json> <now>

use std::path::Path;

use scw_store::housekeeping::{self, Policy};
use scw_store::reputation::Database;
use serde_json::{Value, json};

const APPLY: bool = false;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let root = Path::new(&a[1]);
    let settings: Value = serde_json::from_slice(&std::fs::read(&a[3]).unwrap()).unwrap();
    let now: f64 = a[6].parse().unwrap();
    let policy = Policy::load(Path::new(&a[2]), settings.as_object());
    let swept = housekeeping::sweep(&policy, root, now, APPLY);
    let summary = housekeeping::summarise(&swept, APPLY);
    let mut db = Database::open(Path::new(&a[4])).unwrap();
    db.clock = Box::new(move || now);
    db.root = root.to_path_buf();
    let audits = housekeeping::prune_audit_rows(&db, &policy, APPLY).unwrap();
    let line = housekeeping::run_now(&policy, root, Some(&db), now, APPLY);
    let out = json!({"policy": policy, "sweep": swept, "summary": summary,
                     "audits": [audits.0, audits.1], "run_now": line});
    std::fs::write(&a[5], serde_json::to_string(&out).unwrap()).unwrap();
}
