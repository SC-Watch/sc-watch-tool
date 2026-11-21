//! The IRIS letter model and reader against sc-watch's Python port, on the
//! synthetic rows made by tests/fixtures/make_fixtures.py (OpenCV's Hershey
//! fonts on noise - no game frame). The real-frame check against IRIS's own
//! readings is tests/iris_golden.rs, which runs only where that data is.

mod common;
mod reading;

use common::*;
use scw_vision::iris::LetterModel;
use serde_json::Value;

/// The logits follow IRIS's order of operations, so they are compared to
/// the bit; the log-probabilities go through exp and ln, whose last bit
/// belongs to the platform's maths library.
#[test]
fn letter_model() {
    let model = reading::model();
    check("letter_model", |c| {
        let x = floats(input(c, 0));
        let want_logits = floats(output(c, 0));
        let got = model.logits(&x);
        let bits = |v: &[f64]| v.iter().map(|f| f.to_bits()).collect::<Vec<_>>();
        if let Some(d) = diff(&bits(&want_logits), &bits(&got)) {
            return Some(format!("logits: {d}"));
        }
        let want_lp = floats(output(c, 1));
        let worst = want_lp
            .iter()
            .zip(model.log_probs(&x))
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f64, f64::max);
        (worst > 1e-12).then(|| format!("log-probabilities differ by {worst:e}"))
    });
}

#[test]
fn model_file_is_checked() {
    assert!(LetterModel::from_bytes(b"not a model").is_err());
    let good = std::fs::read(reading::model_path()).unwrap();
    assert!(LetterModel::from_bytes(&good[..good.len() - 1]).is_err());
    let mut longer = good.clone();
    longer.push(0);
    assert!(LetterModel::from_bytes(&longer).is_err());
}

#[test]
fn synthetic_rows() {
    let rr = reading::row_reader();
    check("iris_row", |c: &Value| {
        let img = img_u8(input(c, 0));
        let b: Vec<i32> = c["params"]["box"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_i64().unwrap() as i32)
            .collect();
        reading::compare_row(
            &rr,
            &img,
            scw_vision::Rect::new(b[0], b[1], b[2], b[3]),
            &c["params"]["expect"],
            reading::MaskCheck::Crc32,
        )
        .err()
    });
}
