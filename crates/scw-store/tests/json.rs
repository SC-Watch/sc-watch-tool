//! scw-store's JSON files against jsonstore.py's own answers
//! (tests/fixtures/make_fixtures.py): dumps() text byte for byte, float
//! repr, what reading raw files gives, and coerce().

use scw_store::json::{Expect, Parsed, coerce, dumps, loads, parse, py_float, read};
use serde_json::Value;
use std::path::PathBuf;

fn cases() -> Value {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/json_cases.json");
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap()
}

fn hex(h: &str) -> Vec<u8> {
    (0..h.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn floats_as_python_repr() {
    let mut bad = Vec::new();
    for c in cases()["floats"].as_array().unwrap() {
        let bits: [u8; 8] = hex(s(&c["bits"])).try_into().unwrap();
        let f = f64::from_le_bytes(bits);
        let got = py_float(f).unwrap();
        if got != s(&c["repr"]) {
            bad.push(format!("{f:e}: python {} rust {got}", s(&c["repr"])));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn dumps_as_python() {
    for c in cases()["dumps"].as_array().unwrap() {
        let v = loads(s(&c["value"])).unwrap();
        assert_eq!(dumps(&v).unwrap(), s(&c["text"]), "for {}", s(&c["value"]));
    }
}

/// A value from the fixture's compact JSON, Infinity and NaN included.
fn value(text: Option<&str>) -> Option<Value> {
    text.map(|t| loads(t).unwrap())
}

#[test]
fn reading_files_as_jsonstore() {
    let dir = std::env::temp_dir().join(format!("scw-store-json-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut bad = Vec::new();
    for (i, c) in cases()["read"].as_array().unwrap().iter().enumerate() {
        let p = dir.join(format!("case{i}.json"));
        std::fs::write(&p, hex(s(&c["hex"]))).unwrap();
        let (problem, data) = match parse(&p) {
            Parsed::Read(Ok(v)) => (false, Some(v)),
            Parsed::Read(Err(_)) => (true, None),
            other => panic!("{other:?}"),
        };
        if problem != c["problem"].as_bool().unwrap() || data != value(c["data"].as_str()) {
            bad.push(format!(
                "case {i} {:?}: python problem {} data {}; rust problem {problem} data {data:?}",
                String::from_utf8_lossy(&hex(s(&c["hex"]))),
                c["problem"],
                c["data"]
            ));
            continue;
        }
        for (name, expect) in [
            ("read_dict", Expect::Object),
            ("read_list", Expect::Array),
            ("read_any", Expect::Anything),
        ] {
            if read(&p, expect) != value(c[name].as_str()) {
                bad.push(format!("case {i} {name}: python {}", c[name]));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn coerce_as_jsonstore() {
    let mut bad = Vec::new();
    for c in cases()["coerce"].as_array().unwrap() {
        let (d, v) = (
            loads(s(&c["default"])).unwrap(),
            loads(s(&c["value"])).unwrap(),
        );
        let want = loads(s(&c["result"])).unwrap();
        let got = coerce(&d, &v);
        // Same value AND the same int/float kind (Python's 1 vs 1.0).
        let kind = |x: &Value| x.as_number().map(|n| n.is_f64());
        if got != want || kind(&got) != kind(&want) {
            bad.push(format!("coerce({d}, {v}): python {want} rust {got}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn writing_keeps_an_unreadable_file_and_writes_python_text() {
    let dir = std::env::temp_dir().join(format!("scw-store-write-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("settings.json");
    std::fs::write(&p, b"{ not json").unwrap();
    let v: Value = serde_json::json!({"volume": 0.9, "voice": "Caf\u{e9}"});
    scw_store::json::write(&p, &v).unwrap();
    let want = "{\n  \"volume\": 0.9,\n  \"voice\": \"Caf\\u00e9\"\n}";
    let want = if cfg!(windows) {
        want.replace('\n', "\r\n")
    } else {
        want.to_string()
    };
    assert_eq!(std::fs::read_to_string(&p).unwrap(), want);
    let kept: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("settings.unreadable-")
        })
        .collect();
    assert_eq!(kept.len(), 1, "the unreadable file was kept aside");
    assert_eq!(std::fs::read(kept[0].path()).unwrap(), b"{ not json");
    let after = scw_store::json::update(&p, Expect::Object, |cur| {
        let mut m = cur.unwrap();
        m["volume"] = serde_json::json!(0.5);
        m
    })
    .unwrap();
    assert_eq!(after["volume"], serde_json::json!(0.5));
    let _ = std::fs::remove_dir_all(&dir);
}
