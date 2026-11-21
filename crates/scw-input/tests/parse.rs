//! scw-input's binding parser against inputs.parse() on the specs in
//! tests/fixtures/make_fixtures.py: the same tokens and labels, or the same
//! error message, for every one.

use scw_input::{Token, parse};
use serde_json::Value;
use std::path::PathBuf;

fn token_json(t: &Token) -> Value {
    match t {
        Token::Key(vk) => {
            serde_json::json!({"kind": "key", "vk": vk, "index": null, "ident": "", "button": 0})
        }
        Token::Joy {
            index,
            ident,
            button,
        } => {
            serde_json::json!({"kind": "joy", "vk": 0, "index": index, "ident": ident, "button": button})
        }
    }
}

#[test]
fn parse_matches_inputs_py() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/parse.json");
    let cases: Vec<Value> = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let mut bad = Vec::new();
    for c in &cases {
        let spec = c["spec"].as_str().unwrap();
        let got = match parse(spec) {
            Ok(tokens) => serde_json::json!({
                "spec": spec, "ok": true,
                "tokens": tokens.iter().map(token_json).collect::<Vec<_>>(),
                "labels": tokens.iter().map(Token::label).collect::<Vec<_>>(),
            }),
            Err(e) => serde_json::json!({"spec": spec, "ok": false, "error": e.0}),
        };
        if &got != c {
            bad.push(format!("  {spec:?}:\n    python {c}\n    rust   {got}"));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {} specs differ\n{}",
        bad.len(),
        cases.len(),
        bad.join("\n")
    );
}
