//! scw-store's flags against flags.py's own answers
//! (tests/fixtures/make_flags.py): loading odd flags.json contents, every
//! lookup, and what save() writes over a file - the bytes included.

use scw_store::flags::{Flags, from_raw, save};
use serde_json::Value;
use std::path::PathBuf;

fn cases() -> Vec<Value> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/flags_cases.json");
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

fn flags_json(f: &[scw_store::flags::Flag]) -> Value {
    Value::Array(f.iter().map(|x| x.to_json()).collect())
}

#[test]
fn flags_match_flags_py() {
    let dir = std::env::temp_dir().join(format!("scw-store-flags-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut bad = Vec::new();
    for (i, c) in cases().iter().enumerate() {
        let raw = if c["raw"].is_null() {
            None
        } else {
            Some(&c["raw"])
        };
        let loaded = from_raw(raw);
        if flags_json(&loaded) != c["loaded"] {
            bad.push(format!(
                "case {i} loaded: python {} rust {}",
                c["loaded"],
                flags_json(&loaded)
            ));
        }
        let p = dir.join(format!("flags{i}.json"));
        let _ = std::fs::remove_file(&p);
        if let Some(r) = raw {
            std::fs::write(&p, serde_json::to_vec(r).unwrap()).unwrap();
        }
        let fl = Flags::load(&p);
        for l in c["lookups"].as_array().unwrap() {
            let k = l["key"].as_str().unwrap();
            let got = (fl.by_key(k).to_json(), fl.sign(k), fl.speakable(k));
            let want = (
                l["by_key"].clone(),
                l["sign"].as_f64().unwrap(),
                l["speakable"].as_str().unwrap().to_string(),
            );
            if got != want {
                bad.push(format!(
                    "case {i} lookup {k:?}: python {want:?} rust {got:?}"
                ));
            }
        }
        let active: Vec<String> = fl.active().iter().map(|f| f.key.clone()).collect();
        let want_active: Vec<String> = serde_json::from_value(c["active"].clone()).unwrap();
        if active != want_active {
            bad.push(format!(
                "case {i} active: python {want_active:?} rust {active:?}"
            ));
        }
        let want_keys: Vec<String> = serde_json::from_value(c["keys"].clone()).unwrap();
        if fl.keys() != want_keys {
            bad.push(format!(
                "case {i} keys: python {want_keys:?} rust {:?}",
                fl.keys()
            ));
        }
        let items = c["save_items"].as_array().unwrap();
        let written = save(items, &p).unwrap();
        if flags_json(&written) != c["written"] {
            bad.push(format!(
                "case {i} save: python {} rust {}",
                c["written"],
                flags_json(&written)
            ));
        }
        // The fixture read the Python's file with read_text(), which turns
        // CRLF into LF; the line endings themselves are tests/json.rs's.
        let text = std::fs::read_to_string(&p).unwrap().replace("\r\n", "\n");
        if text != c["file_after"].as_str().unwrap() {
            bad.push(format!("case {i}: the saved file differs"));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(bad.is_empty(), "{} differ\n{}", bad.len(), bad.join("\n"));
}
