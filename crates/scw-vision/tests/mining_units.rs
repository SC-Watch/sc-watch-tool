//! scw-vision's mining reading logic against IRIS's own C#, on the
//! synthetic cases of tests/fixtures/make_mining_units.py: IRIS's answers
//! (mining_units_out.json) come from the bench harness's `--mining-units`
//! mode, which calls IRIS's DigitTemplates, MiningTable, TableSnap,
//! DigitEvidence, SignalConfidence, MiningLabelState, LockGeometry,
//! LabelColours and MiningState directly. Every double is compared to the
//! bit.

use scw_vision::mining::digits::{self, LEN, Patch};
use scw_vision::mining::evidence::{self, DigitEvidence, LabelColours, readable};
use scw_vision::mining::snap;
use scw_vision::mining::state::{
    LockGeometry, MiningLabelState, MiningState, is_clearly_different,
};
use scw_vision::mining::table::MiningTable;
use scw_vision::mining::{
    Bgr, MiningSettings, PixelRect, SignalRead, SignalState, base64_decode, is_number,
};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::OnceLock;

fn load(name: &str) -> Value {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

fn cases() -> &'static (Value, Value) {
    static C: OnceLock<(Value, Value)> = OnceLock::new();
    C.get_or_init(|| (load("mining_units_in.json"), load("mining_units_out.json")))
}

fn d(v: &Value) -> f64 {
    f64::from_bits(v.as_u64().unwrap())
}

fn bits(x: f64) -> u64 {
    x.to_bits()
}

fn colour(v: &Value) -> Bgr {
    let c = |i: usize| v[i].as_u64().unwrap() as u8;
    Bgr::new(c(0), c(1), c(2))
}

fn colour_json(c: Bgr) -> Value {
    serde_json::json!([c.b, c.g, c.r])
}

fn opt_int(v: &Value) -> Option<i32> {
    v.as_i64().map(|x| x as i32)
}

fn state(s: &str) -> SignalState {
    match s {
        "None" => SignalState::None,
        "Disqualified" => SignalState::Disqualified,
        "Unreadable" => SignalState::Unreadable,
        "Signal" => SignalState::Signal,
        other => panic!("state {other}"),
    }
}

fn scores(v: &Value) -> Option<Vec<[f64; 10]>> {
    let bytes = base64_decode(v.as_str()?).unwrap();
    let (digits, rest) = bytes.as_chunks::<80>();
    assert!(rest.is_empty());
    Some(
        digits
            .iter()
            .map(|c| {
                std::array::from_fn(|i| f64::from_le_bytes(c[i * 8..i * 8 + 8].try_into().unwrap()))
            })
            .collect(),
    )
}

fn read(r: &Value) -> SignalRead {
    let b = &r["box"];
    SignalRead {
        state: state(r["state"].as_str().unwrap()),
        text: r["text"].as_str().unwrap().to_string(),
        value: opt_int(&r["value"]),
        signal_box: PixelRect::new(d(&b[0]), d(&b[1]), d(&b[2]), d(&b[3])),
        row_top: d(&r["row_top"]),
        digit_height: d(&r["dh"]),
        text_colour: colour(&r["tc"]),
        back_colour: colour(&r["bc"]),
        offset_px: d(&r["off"]),
        min_score: d(&r["min"]),
        digit_scores: scores(&r["scores"]),
    }
}

fn scores_eq(a: &Option<Vec<[f64; 10]>>, b: &Option<Vec<[f64; 10]>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.len() == b.len()
                && a.iter()
                    .zip(b)
                    .all(|(x, y)| x.iter().zip(y).all(|(p, q)| p.to_bits() == q.to_bits()))
        }
        _ => false,
    }
}

/// A read against IRIS's ReadOut: state, text, value, min score, scores.
fn same_read(got: &SignalRead, want: &Value) -> bool {
    got.state.as_str() == want["state"].as_str().unwrap()
        && got.text == want["text"].as_str().unwrap()
        && got.value == opt_int(&want["value"])
        && bits(got.min_score) == want["min"].as_u64().unwrap()
        && scores_eq(&got.digit_scores, &scores(&want["scores"]))
}

/// Up to four differences are named; the test fails on any.
fn report(what: &str, bad: Vec<String>, total: usize) {
    for b in bad.iter().take(4) {
        eprintln!("  {what}: {b}");
    }
    assert!(
        bad.is_empty(),
        "{what}: {} of {total} differ from IRIS",
        bad.len()
    );
    println!("{what}: {total} identical to IRIS");
}

#[test]
fn digit_shapes_load() {
    let (normal, small) = digits::shape_counts();
    // 52 full shapes, 10 mid and 20 small, as in DigitTemplates.cs
    assert_eq!(normal.iter().sum::<usize>(), 62);
    assert_eq!(small.iter().sum::<usize>(), 72);
    assert!(normal.iter().chain(&small).all(|&n| n > 0));
}

#[test]
fn digit_matching() {
    let (input, want) = cases();
    let mut bad = Vec::new();
    let list = input["patches"].as_array().unwrap();
    for (i, (c, w)) in list
        .iter()
        .zip(want["patches"].as_array().unwrap())
        .enumerate()
    {
        let bytes = base64_decode(c["patch"].as_str().unwrap()).unwrap();
        assert_eq!(bytes.len(), LEN * 4);
        let mut patch: Patch = [0.0; LEN];
        for (p, b) in patch.iter_mut().zip(bytes.as_chunks::<4>().0) {
            *p = f32::from_le_bytes(*b);
        }
        let small = c["small"].as_bool().unwrap();
        let (digit, score, all) = digits::match_digit(&patch, d(&c["tie"]), small);
        let best = digits::best_score(&patch, small);
        let all_ok = all
            .iter()
            .zip(w["all"].as_array().unwrap())
            .all(|(x, y)| bits(*x) == y.as_u64().unwrap());
        if (digit as char).to_string() != w["digit"].as_str().unwrap()
            || bits(score) != w["score"].as_u64().unwrap()
            || !all_ok
            || bits(best) != w["best"].as_u64().unwrap()
        {
            bad.push(format!("patch {i}: {} vs {}", w["digit"], digit as char));
        }
    }
    report("digit matching", bad, list.len());
}

#[test]
fn tables() {
    let (input, want) = cases();
    let mut bad = Vec::new();
    let texts = input["tables"].as_array().unwrap();
    for (i, (t, w)) in texts
        .iter()
        .zip(want["tables"].as_array().unwrap())
        .enumerate()
    {
        let table = MiningTable::parse(t.as_str().unwrap());
        let resources: Vec<Value> = table
            .resources
            .iter()
            .map(|r| serde_json::json!([r.base, r.name, r.max_multiple]))
            .collect();
        if Value::from(resources) != w["resources"] {
            bad.push(format!("table {i}: resources"));
        }
        if serde_json::json!(table.errors) != w["errors"] {
            bad.push(format!(
                "table {i}: errors {:?} vs {}",
                table.errors, w["errors"]
            ));
        }
        if serde_json::json!(table.version) != w["version"]
            || table.max_signal as i64 != w["max_signal"].as_i64().unwrap()
        {
            bad.push(format!("table {i}: version or max"));
        }
        let values = input["lookups"][i].as_array().unwrap();
        for (v, wl) in values.iter().zip(w["lookups"].as_array().unwrap()) {
            let v = v.as_i64().unwrap() as i32;
            let got = serde_json::json!([
                table.lookup(v),
                table.contains(v),
                table.above_limit_label(v)
            ]);
            if &got != wl {
                bad.push(format!("table {i} value {v}: {got} vs {wl}"));
            }
        }
    }
    report("tables", bad, texts.len());
    assert_eq!(
        MiningTable::embedded().version.as_deref(),
        Some("2026-11-01")
    );
    assert!(!MiningTable::is_outdated(Some(
        scw_vision::mining::table::EMBEDDED_TEXT
    )));
    assert!(MiningTable::is_outdated(Some(
        "# IRIS mining data 2020-01-01\n"
    )));
    assert!(!MiningTable::is_outdated(None));
}

#[test]
fn table_snapping() {
    let (input, want) = cases();
    let table = MiningTable::embedded();
    let settings = MiningSettings::default();
    let mut bad = Vec::new();
    let list = input["snaps"].as_array().unwrap();
    for (i, (c, w)) in list
        .iter()
        .zip(want["snaps"].as_array().unwrap())
        .enumerate()
    {
        let r = read(c);
        let ok = match (snap::apply(&r, &table, &settings), w.is_null()) {
            (None, true) => true,
            (Some(got), false) => {
                // and everything else carried over
                same_read(&got, w)
                    && got.signal_box == r.signal_box
                    && got.text_colour == r.text_colour
            }
            _ => false,
        };
        if !ok {
            bad.push(format!("snap {i}"));
        }
    }
    report("snapping", bad, list.len());
}

#[test]
fn fusing_reads() {
    let (input, want) = cases();
    let mut bad = Vec::new();
    let list = input["evidence"].as_array().unwrap();
    for (i, (c, w)) in list
        .iter()
        .zip(want["evidence"].as_array().unwrap())
        .enumerate()
    {
        let mut ev = DigitEvidence::new(c["max"].as_u64().unwrap() as usize);
        let min_digit = d(&c["min_digit"]);
        for (k, (r, ws)) in c["reads"]
            .as_array()
            .unwrap()
            .iter()
            .zip(w.as_array().unwrap())
            .enumerate()
        {
            let r = read(r);
            ev.add(&r);
            let fused = ev.fused(&r, min_digit);
            let ok = ev.len() as u64 == ws["count"].as_u64().unwrap()
                && match (&fused, ws["fused"].is_null()) {
                    (None, true) => true,
                    (Some(f), false) => same_read(f, &ws["fused"]),
                    _ => false,
                };
            if !ok {
                bad.push(format!("sequence {i} read {k}"));
            }
        }
    }
    report("fusing", bad, list.len());
}

#[test]
fn the_label_over_reads() {
    let (input, want) = cases();
    let table = MiningTable::embedded();
    let settings = MiningSettings::default();
    let mut bad = Vec::new();
    let list = input["label_states"].as_array().unwrap();
    for (i, (c, w)) in list
        .iter()
        .zip(want["label_states"].as_array().unwrap())
        .enumerate()
    {
        let v0 = opt_int(&c["value"]);
        let label0 = v0.and_then(|v| table.lookup(v)).map(str::to_string);
        let value0 = if label0.is_some() { v0 } else { None };
        let mut state = MiningLabelState::new(&settings, d(&c["locked_ms"]), value0, label0);
        for (k, (s, ws)) in c["steps"]
            .as_array()
            .unwrap()
            .iter()
            .zip(w.as_array().unwrap())
            .enumerate()
        {
            let changed = state.apply(&read(&s["read"]), d(&s["frame_ms"]), &table);
            let got = serde_json::json!({"changed": changed, "value": state.value,
                                         "label": state.label, "ended": state.ended});
            if &got != ws {
                bad.push(format!("sequence {i} step {k}: {got} vs {ws}"));
            }
        }
    }
    report("label state", bad, list.len());
}

#[test]
fn geometry_follows_by_medians() {
    let (input, want) = cases();
    let settings = MiningSettings::default();
    let mut bad = Vec::new();
    let list = input["geometry"].as_array().unwrap();
    for (i, (c, w)) in list
        .iter()
        .zip(want["geometry"].as_array().unwrap())
        .enumerate()
    {
        let l = read(&c["locked"]);
        let locked = MiningState {
            locked_ms: 0.0,
            row_top: l.row_top,
            signal_box: l.signal_box,
            digit_height: l.digit_height,
            text_colour: l.text_colour,
            back_colour: l.back_colour,
            value: l.value,
            label: None,
        };
        let mut g = LockGeometry::new(&settings);
        for (k, (s, ws)) in c["steps"]
            .as_array()
            .unwrap()
            .iter()
            .zip(w.as_array().unwrap())
            .enumerate()
        {
            let got = g.add(
                PixelRect::new(d(&s[0]), d(&s[1]), d(&s[2]), d(&s[3])),
                d(&s[4]),
                d(&s[5]),
                &locked,
            );
            let got = got.map(|(b, row, dh)| {
                serde_json::json!([
                    bits(b.x),
                    bits(b.y),
                    bits(b.w),
                    bits(b.h),
                    bits(row),
                    bits(dh)
                ])
            });
            if got.as_ref().unwrap_or(&Value::Null) != ws {
                bad.push(format!("sequence {i} step {k}"));
            }
        }
    }
    report("geometry", bad, list.len());
}

#[test]
fn label_colours() {
    let (input, want) = cases();
    let settings = MiningSettings::default();
    let mut bad = Vec::new();
    let list = input["colours"].as_array().unwrap();
    for (i, (c, w)) in list
        .iter()
        .zip(want["colours"].as_array().unwrap())
        .enumerate()
    {
        let mut lc = LabelColours::new(&settings, colour(&c["text"]), colour(&c["back"]));
        for (k, (s, ws)) in c["steps"]
            .as_array()
            .unwrap()
            .iter()
            .zip(w.as_array().unwrap())
            .enumerate()
        {
            let changed = lc.add(colour(&s["text"]), colour(&s["back"]), d(&s["now"]));
            let got = serde_json::json!({"changed": changed, "text": colour_json(lc.text_colour),
                                         "back": colour_json(lc.back_colour)});
            if &got != ws {
                bad.push(format!("sequence {i} step {k}: {got} vs {ws}"));
            }
        }
    }
    report("colours", bad, list.len());
    let list = input["readable"].as_array().unwrap();
    let mut bad = Vec::new();
    for (i, (c, w)) in list
        .iter()
        .zip(want["readable"].as_array().unwrap())
        .enumerate()
    {
        let got = readable(colour(&c["text"]), colour(&c["back"]), d(&c["ratio"]));
        if colour_json(got) != *w {
            bad.push(format!("case {i}: {} vs {w}", colour_json(got)));
        }
    }
    report("readable", bad, list.len());
}

#[test]
fn locking_and_confidence() {
    let (input, want) = cases();
    let table = MiningTable::embedded();
    let settings = MiningSettings::default();
    let mut bad = Vec::new();
    let list = input["from_lock"].as_array().unwrap();
    for (i, (c, w)) in list
        .iter()
        .zip(want["from_lock"].as_array().unwrap())
        .enumerate()
    {
        let s = MiningState::from_lock(&read(&c["read"]), d(&c["frame_ms"]), &table);
        let b = s.signal_box;
        let got = serde_json::json!({
            "locked_ms": bits(s.locked_ms), "row_top": bits(s.row_top),
            "box": [bits(b.x), bits(b.y), bits(b.w), bits(b.h)], "dh": bits(s.digit_height),
            "tc": colour_json(s.text_colour), "bc": colour_json(s.back_colour),
            "value": s.value, "label": s.label,
        });
        if &got != w {
            bad.push(format!("lock {i}: {got} vs {w}"));
        }
    }
    report("locks", bad, list.len());

    let mut bad = Vec::new();
    let list = input["margins"].as_array().unwrap();
    for (i, (c, w)) in list
        .iter()
        .zip(want["margins"].as_array().unwrap())
        .enumerate()
    {
        let r = read(c);
        if bits(evidence::min_margin(&r)) != w["margin"].as_u64().unwrap()
            || evidence::is_confident(&r, settings.confident_margin)
                != w["confident"].as_bool().unwrap()
        {
            bad.push(format!("margin {i}"));
        }
    }
    report("margins", bad, list.len());

    let mut bad = Vec::new();
    let list = input["numbers"].as_array().unwrap();
    for (c, w) in list.iter().zip(want["numbers"].as_array().unwrap()) {
        let s = c.as_str().unwrap();
        if is_number(s) != w.as_bool().unwrap() {
            bad.push(format!("{s:?}"));
        }
    }
    report("is a number", bad, list.len());

    let mut bad = Vec::new();
    let list = input["clearly"].as_array().unwrap();
    for (i, (c, w)) in list
        .iter()
        .zip(want["clearly"].as_array().unwrap())
        .enumerate()
    {
        let got = is_clearly_different(
            &read(&c["read"]),
            c["shown"].as_i64().unwrap() as i32,
            d(&c["margin"]),
        );
        if got != w.as_bool().unwrap() {
            bad.push(format!("case {i}"));
        }
    }
    report("clearly different", bad, list.len());
}
