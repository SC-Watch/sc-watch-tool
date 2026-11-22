//! Retention and audit pruning against housekeeping.py itself
//! (tests/fixtures/make_housekeeping_ops.py): the same data folder, made
//! here with the same names, sizes and times, swept under the same policies
//! deletes the same frames and says the same; Policy::load reads the same
//! legacy files and settings the same way; the audit pruning leaves the
//! same rows and images.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use filetime::FileTime;
use scw_store::housekeeping::{self, Policy, Sweep};
use scw_store::reputation::Database;
use serde_json::{Value, json};

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

// filetime's setter is deprecated for std::fs::set_times, which is newer
// than the workspace's minimum Rust; std's File::set_modified cannot date a
// folder on Windows.
#[allow(deprecated)]
fn make(root: &Path, tree: &Value, dirs: &Value, now: f64) {
    for d in dirs.as_array().into_iter().flatten() {
        let p = root.join(d.as_str().unwrap());
        std::fs::create_dir_all(&p).unwrap();
        filetime::set_file_mtime(
            &p,
            FileTime::from_unix_time((now - 999.0 * 3600.0) as i64, 0),
        )
        .unwrap();
    }
    for f in tree.as_array().unwrap() {
        let p = root.join(f[0].as_str().unwrap());
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, vec![0u8; f[1].as_u64().unwrap() as usize]).unwrap();
        let t = now - f[2].as_f64().unwrap() * 3600.0;
        let (s, frac) = (t.floor(), t - t.floor());
        filetime::set_file_mtime(
            &p,
            FileTime::from_unix_time(s as i64, (frac * 1e9).round() as u32),
        )
        .unwrap();
    }
}

/// Every file under `root`, as `rglob` lists them, sorted by path parts.
fn files(root: &Path) -> Value {
    fn walk(d: &Path, base: &Path, out: &mut Vec<Vec<String>>) {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, base, out);
            } else {
                out.push(
                    p.strip_prefix(base)
                        .unwrap()
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect(),
                );
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    // Windows compares paths case-blind.
    out.sort_by_key(|parts| parts.iter().map(|p| p.to_lowercase()).collect::<Vec<_>>());
    json!(out.into_iter().map(|p| p.join("/")).collect::<Vec<_>>())
}

#[test]
fn housekeeping_matches_housekeeping_py() {
    let fx: Value = serde_json::from_slice(
        &std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/housekeeping_ops.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let base = std::env::temp_dir().join(format!("scw-store-hk-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("data");
    let now = fx["now"].as_f64().unwrap();
    make(&root, &fx["tree"], &fx["dirs"], now);
    let mut bad = Vec::new();
    if fx["managed"] != json!(housekeeping::MANAGED) {
        bad.push("MANAGED differs".into());
    }

    for s in fx["sweeps"].as_array().unwrap() {
        let label = s["label"].as_str().unwrap();
        let p: Policy = serde_json::from_value(s["policy"].clone()).unwrap();
        let res = housekeeping::sweep(&p, &root, now, s["apply"].as_bool().unwrap());
        if !same(&json!(res), &s["result"]) {
            bad.push(format!(
                "sweep {label}:\n    python {}\n    rust   {}",
                s["result"],
                json!(res)
            ));
        }
        let line = housekeeping::summarise(&res, s["apply"].as_bool().unwrap());
        if line != s["summary"].as_str().unwrap() {
            bad.push(format!(
                "summary {label}:\n    python {}\n    rust   {line}",
                s["summary"]
            ));
        }
        let left = files(&root);
        if left != s["files"] {
            bad.push(format!(
                "files after {label}:\n    python {}\n    rust   {left}",
                s["files"]
            ));
        }
    }

    // Ties and the case-blind *.png: Windows only, where the fixture was made.
    if cfg!(windows) {
        let case = base.join("case");
        make(&case, &fx["case"]["tree"], &json!([]), now);
        let p = Policy {
            keep_newest: 2,
            ..Policy::default()
        };
        let res = housekeeping::sweep(&p, &case, now, true);
        if !same(&json!(res), &fx["case"]["result"]) || files(&case) != fx["case"]["files"] {
            bad.push(format!(
                "case sweep:\n    python {} {}\n    rust   {} {}",
                fx["case"]["result"],
                fx["case"]["files"],
                json!(res),
                files(&case)
            ));
        }
    }

    let legacy = base.join("housekeeping.json");
    for l in fx["loads"].as_array().unwrap() {
        match l["legacy"].as_str() {
            Some(t) => std::fs::write(&legacy, t).unwrap(),
            None => {
                let _ = std::fs::remove_file(&legacy);
            }
        }
        let p = Policy::load(&legacy, l["settings"].as_object());
        if !same(&json!(p), &l["policy"]) {
            bad.push(format!(
                "load {}:\n    python {}\n    rust   {}",
                l["label"],
                l["policy"],
                json!(p)
            ));
        }
    }
    let saved = base.join("saved.json");
    Policy {
        keep_newest: 3,
        directories: vec!["mine".into()],
        ..Policy::default()
    }
    .save(&saved)
    .unwrap();
    let got: Value = serde_json::from_slice(&std::fs::read(&saved).unwrap()).unwrap();
    if !same(&got, &fx["saved"]) {
        bad.push(format!(
            "save:\n    python {}\n    rust   {got}",
            fx["saved"]
        ));
    }

    for s in fx["summaries"].as_array().unwrap() {
        let results: Vec<Sweep> = s["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| Sweep {
                directory: r[0].as_str().unwrap().into(),
                examined: r[1].as_u64().unwrap() as usize,
                deleted: r[2].as_u64().unwrap() as usize,
                bytes_freed: r[3].as_u64().unwrap(),
                kept: r[4].as_u64().unwrap() as usize,
            })
            .collect();
        let line = housekeeping::summarise(&results, s["applied"].as_bool().unwrap());
        if line != s["line"].as_str().unwrap() {
            bad.push(format!(
                "summarise:\n    python {}\n    rust   {line:?}",
                s["line"]
            ));
        }
    }

    // Audits, in the same data folder, after the sweeps.
    let clock = Rc::new(Cell::new(now));
    let open = || {
        let mut db = Database::open(&root.join("sc-watch.db")).unwrap();
        let c = clock.clone();
        db.clock = Box::new(move || c.get());
        db.utc_dates = true;
        db
    };
    {
        let db = open();
        std::fs::create_dir_all(root.join("audit")).unwrap();
        for a in fx["audits"].as_array().unwrap() {
            let (h, age, crop) = (
                a[0].as_str().unwrap(),
                a[1].as_f64().unwrap(),
                a[2].as_str().unwrap(),
            );
            clock.set(now - age);
            std::fs::write(root.join("audit").join(crop), b"png").unwrap();
            db.record_sighting(h, Some(2.0), 1, 3).unwrap();
            db.add_audit(
                h,
                Some(2.0),
                1,
                3,
                &json!([format!("{h}X")]),
                "",
                &format!("audit/{crop}"),
                "",
                "weak_read",
            )
            .unwrap();
        }
        clock.set(now);
        db.conn
            .execute("DELETE FROM players WHERE handle='EXMPLGONE'", [])
            .unwrap();
        std::fs::write(root.join("audit").join("stray.png"), b"png").unwrap();
    }
    for step in fx["audit_steps"].as_array().unwrap() {
        let db = open();
        let got = housekeeping::prune_audit_rows(
            &db,
            &Policy::default(),
            step["apply"].as_bool().unwrap(),
        )
        .unwrap();
        let rows: Vec<Value> = db
            .conn
            .prepare("SELECT handle, crop_name FROM audits ORDER BY handle")
            .unwrap()
            .query_map([], |r| {
                Ok(json!({"handle": r.get::<_, String>(0)?, "crop_name": r.get::<_, Option<String>>(1)?}))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let mut names: Vec<String> = std::fs::read_dir(root.join("audit"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        let got =
            json!({"apply": step["apply"], "result": [got.0, got.1], "rows": rows, "files": names});
        if !same(&got, step) {
            bad.push(format!("audits:\n    python {step}\n    rust   {got}"));
        }
    }
    let db = open();
    let p = Policy {
        keep_newest: 0,
        ..Policy::default()
    };
    for (apply, want) in [false, true].iter().zip(fx["run_now"].as_array().unwrap()) {
        let line = housekeeping::run_now(&p, &root, Some(&db), now, *apply);
        if line != want.as_str().unwrap() {
            bad.push(format!(
                "run_now apply={apply}:\n    python {want}\n    rust   {line:?}"
            ));
        }
    }
    drop(db);
    let _ = std::fs::remove_dir_all(&base);
    assert!(bad.is_empty(), "{} differ\n{}", bad.len(), bad.join("\n"));
}
