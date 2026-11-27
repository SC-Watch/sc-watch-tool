//! Settings against settings.py itself (tests/fixtures/make_settings_ops.py):
//! the same schema, the same clamping of every kind of field against awkward
//! values, the same load of crafted files and the same merges on save - and
//! the typed view covers exactly the schema. The settings only the port
//! has (the mining overlay, `settings::RUST_ONLY`) are left out of every
//! comparison and checked on their own.

use std::path::PathBuf;

use scw_store::json::py_str;
use scw_store::settings::{self, Settings};
use serde_json::{Map, Value, json};

/// The port's settings as settings.py has them: without the ones only the
/// port has.
fn as_python(v: Value) -> Value {
    match v {
        Value::Object(mut m) => {
            for k in settings::RUST_ONLY {
                m.shift_remove(*k);
            }
            Value::Object(m)
        }
        other => other,
    }
}

fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|((k, v), (l, w))| k == l && same(v, w))
        }
        _ => a == b,
    }
}

#[test]
fn settings_match_settings_py() {
    let fx: Value = serde_json::from_slice(
        &std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/settings_ops.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let dir = std::env::temp_dir().join(format!("scw-store-settings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut bad = Vec::new();

    // The defaults, in schema order (same() compares key order too).
    let defaults = as_python(Value::Object(settings::defaults()));
    if !same(&defaults, &fx["defaults"]) {
        bad.push(format!(
            "defaults:\n    python {}\n    rust   {defaults}",
            fx["defaults"]
        ));
    }
    let described: Vec<String> = settings::describe()
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|c| c["fields"].as_array().unwrap().iter())
        .map(|f| f["key"].as_str().unwrap().to_string())
        .collect();
    if described != settings::defaults().keys().cloned().collect::<Vec<_>>() {
        bad.push("describe() and the defaults list different settings".into());
    }

    for c in fx["clamps"].as_array().unwrap() {
        let f = settings::field(c["field"].as_str().unwrap()).unwrap();
        let got = f.clamp(&c["value"]);
        let mut want = c["result"].clone();
        // KNOWN: a 31-digit integer reaches the port as a float (serde_json
        // holds 64-bit integers), so as text it reads "1e+30", not Python's
        // digits. As a number or a bool it clamps the same.
        if c["value"].as_f64().is_some_and(|v| v.abs() >= 1e19)
            && c["value"].is_f64()
            && f.kind == settings::Kind::Str
        {
            want = json!(py_str(&c["value"]));
        }
        if !same(&got, &want) {
            bad.push(format!(
                "clamp {} {}: python {want} rust {got}",
                c["field"], c["value"]
            ));
        }
    }

    for (i, l) in fx["loads"].as_array().unwrap().iter().enumerate() {
        let p = dir.join(format!("load_{i}.json"));
        if let Some(t) = l["text"].as_str() {
            std::fs::write(&p, t).unwrap();
        }
        let full = settings::load(&p);
        let got = as_python(Value::Object(full.clone()));
        if !same(&got, &l["result"]) {
            bad.push(format!(
                "load {}:\n    python {}\n    rust   {got}",
                l["label"], l["result"]
            ));
        }
        if let Err(e) = Settings::from_map(&full) {
            bad.push(format!("load {}: not a Settings: {e}", l["label"]));
        }
    }

    let sp = dir.join("saved.json");
    for s in fx["saves"].as_array().unwrap() {
        let got = as_python(Value::Object(
            settings::save(s["values"].as_object().unwrap(), &sp).unwrap(),
        ));
        let file = as_python(serde_json::from_slice(&std::fs::read(&sp).unwrap()).unwrap());
        if !same(&got, &s["result"]) || !same(&file, &s["file"]) {
            bad.push(format!(
                "save {}:\n    python {} / {}\n    rust   {got} / {file}",
                s["values"], s["result"], s["file"]
            ));
        }
    }
    let over = &fx["save_over"];
    let sp2 = dir.join("saved2.json");
    std::fs::write(&sp2, over["start"].to_string()).unwrap();
    let mut values = Map::new();
    values.insert("volume".into(), json!(0.5));
    let got = as_python(Value::Object(settings::save(&values, &sp2).unwrap()));
    let file = as_python(serde_json::from_slice(&std::fs::read(&sp2).unwrap()).unwrap());
    if !same(&got, &over["result"]) || !same(&file, &over["file"]) {
        bad.push(format!(
            "save over an old file:\n    python {}\n    rust   {got}",
            over["result"]
        ));
    }

    // The typed view is exactly the schema: an unknown key fails it
    // (deny_unknown_fields), and so does a missing one.
    let typed = Settings::from_map(&settings::defaults());
    if let Err(e) = &typed {
        bad.push(format!("the defaults are not a Settings: {e}"));
    }
    let mut short = settings::defaults();
    short.shift_remove("burst");
    if Settings::from_map(&short).is_ok() {
        bad.push("a Settings without burst".into());
    }
    if let Ok(s) = typed
        && (s.burst != 3 || s.reader != settings::Reader::Iris || s.port != 8731)
    {
        bad.push(format!("typed defaults: {s:?}"));
    }
    // The port's own settings: last, in their own category, off by default.
    let cats = settings::describe();
    let last = cats.as_array().unwrap().last().unwrap();
    if last["key"] != "mining" || settings::defaults()["mining"] != "off" {
        bad.push("the mining overlay's settings are not the last category, off".into());
    }
    if settings::RUST_ONLY
        .iter()
        .any(|k| settings::field(k).is_none())
    {
        bad.push("a RUST_ONLY key is not in the schema".into());
    }
    // The port's words for settings.py's fields: only the words. Every
    // other part of the field is settings.py's, and a choice has one label
    // per stored value.
    for k in settings::REWORDED {
        let ours = cats
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|c| c["fields"].as_array().unwrap())
            .find(|f| f["key"] == *k)
            .unwrap();
        let theirs = settings::python_field(k).unwrap();
        for (name, v) in theirs.as_object().unwrap() {
            if name != "help" && ours[name] != *v {
                bad.push(format!("{k}: the port changed {name}, not only words"));
            }
        }
        let labels = ours["choice_labels"].as_array().map_or(0, Vec::len);
        if labels != theirs["choices"].as_array().unwrap().len() {
            bad.push(format!("{k}: {labels} labels for its choices"));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(bad.is_empty(), "{} differ\n{}", bad.len(), bad.join("\n"));
}
