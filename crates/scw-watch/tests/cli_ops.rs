//! The command line against watch.parse_args itself
//! (tests/fixtures/make_cli_ops.py): over the same settings.json, the same
//! argv gives every argument the same value - or the same argparse error
//! and exit code.

use std::path::PathBuf;

use scw_store::settings;
use scw_watch::cli::{Args, defaults, parse_map};
use serde_json::{Map, Value, json};

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

#[test]
fn command_line_matches_watch_py() {
    let fx: Value = serde_json::from_slice(
        &std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cli_ops.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let root = PathBuf::from("DATAROOT");
    let mut bad = Vec::new();
    for c in fx["cases"].as_array().unwrap() {
        let raw = &fx["settings"][c["settings"].as_str().unwrap()];
        let cur = settings::from_raw((!raw.is_null()).then_some(raw));
        let argv: Vec<String> = c["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        let got = match parse_map(&argv, defaults(&root, &cur)) {
            Ok(mut m) => {
                // the log's default is under the data folder
                if let Some(Value::String(l)) = m.get("log") {
                    let l = l.replace('\\', "/");
                    let l = l
                        .strip_prefix("DATAROOT")
                        .map_or(l.clone(), |r| format!("<DATA>{r}"));
                    m.insert("log".into(), json!(l));
                }
                // and typed: every value survives into Args
                let a = Args::from_map(&m);
                assert_eq!(Args::from_map(&m), a);
                json!({"args": Value::Object(m)})
            }
            Err(e) => json!({"exit": e.code, "error": e.message}),
        };
        let mut want: Map<String, Value> = c.as_object().unwrap().clone();
        want.remove("settings");
        want.remove("argv");
        if let Some(Value::Object(a)) = want.get_mut("args")
            && let Some(Value::String(l)) = a.get("log")
        {
            let l = l.replace('\\', "/");
            a.insert("log".into(), json!(l));
        }
        let want = Value::Object(want);
        if !same(&want, &got) {
            bad.push(format!(
                "{} {:?}\n   want {}\n   got  {}",
                c["settings"], argv, want, got
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} differences:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

#[test]
fn typed_arguments() {
    let cur = settings::from_raw(None);
    let a = Args::parse(
        &[
            "--hz".into(),
            "inf".into(),
            "--replay".into(),
            "x/*.png".into(),
        ],
        &PathBuf::from("D"),
        &cur,
    )
    .unwrap();
    assert_eq!(a.hz, f64::INFINITY);
    assert_eq!(a.replay.as_deref(), Some("x/*.png"));
    assert!(!a.where_ && a.check_resolution.is_none());
    let e = Args::parse(&["--help".into()], &PathBuf::from("D"), &cur).unwrap_err();
    assert_eq!(e.code, 0);
}
