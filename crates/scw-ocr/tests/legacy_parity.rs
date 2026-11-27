//! The legacy OCR reader against sc_detector.py itself, on every frame of
//! the five corpora (tests/fixtures/make_legacy_ref.py): the boxes the masks
//! find (with and without the haze locator), the pairs, the range-shape
//! ranking, the km gate's scores, the monitor-rate pass (gate off and on),
//! the full read (and with the gate, haze and dim-range retry), the focused
//! read (direct and through the masks) and the dual read.
//!
//! The pipeline is held apart from the OCR engine (held to RapidOCR on its
//! own in rapidocr_parity.rs): each crop the port hands the OCR must be,
//! byte for byte, one the Python handed it - and gets RapidOCR's recorded
//! answer for it - in the same order, and the contacts must be the
//! Python's. With SCW_LEGACY_E2E set, the port's own OCR (rten) reads
//! instead and the differences are listed (minutes; a measurement).
//!
//! Real frames name real players: SCW_PARITY_DATA/legacy holds
//! `frames.txt` (the frames' paths) and `legacy.json`. Without it the test
//! reports that it skipped and passes. SCW_LEGACY_LIMIT=n reads the first
//! n frames.

mod common;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use common::{key, read_bgr};

use scw_ocr::Ocr;
use scw_ocr::legacy::{self, OcrLine, Options, Recognise};
use scw_vision::pipeline::Contact;
use scw_vision::{Image, Rect};
use serde_json::{Value, json};

fn legacy_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("SCW_PARITY_DATA")?).join("legacy");
    (dir.join("frames.txt").is_file() && dir.join("legacy.json").is_file()).then_some(dir)
}

#[derive(Clone)]
enum Answer {
    Text(String),
    Lines(Vec<OcrLine>),
}

fn line_of(v: &Value) -> OcrLine {
    OcrLine {
        x: v[0].as_i64().unwrap() as i32,
        y: v[1].as_i64().unwrap() as i32,
        w: v[2].as_i64().unwrap() as i32,
        h: v[3].as_i64().unwrap() as i32,
        text: v[4].as_str().unwrap().to_string(),
        conf: v[5].as_f64().unwrap(),
    }
}

/// The OCR the pipeline sees: RapidOCR's recorded answers (or, end to
/// end, the port's own OCR, remembered per crop), with every call logged.
struct Reader<'a> {
    known: &'a mut HashMap<(String, String), Answer>,
    engine: Option<&'a Ocr>,
    calls: Vec<(String, String)>,
    missing: usize,
    /// Time spent answering, so the pipeline's own time can be told apart.
    spent: f64,
}

impl Reader<'_> {
    fn answer(&mut self, kind: &str, img: &Image<u8>) -> Answer {
        let t = Instant::now();
        let a = self.answer_now(kind, img);
        self.spent += t.elapsed().as_secs_f64();
        a
    }

    fn answer_now(&mut self, kind: &str, img: &Image<u8>) -> Answer {
        let k = (kind.to_string(), key(img));
        self.calls.push(k.clone());
        if let Some(a) = self.known.get(&k) {
            return a.clone();
        }
        let a = match (self.engine, kind) {
            (Some(e), "text") => Answer::Text(e.text(img).unwrap()),
            (Some(mut e), _) => Answer::Lines(e.lines(img).unwrap()),
            (None, "text") => {
                self.missing += 1;
                Answer::Text(String::new())
            }
            (None, _) => {
                self.missing += 1;
                Answer::Lines(Vec::new())
            }
        };
        self.known.insert(k, a.clone());
        a
    }
}

impl Recognise for Reader<'_> {
    fn text(&mut self, img: &Image<u8>) -> Result<String, String> {
        match self.answer("text", img) {
            Answer::Text(t) => Ok(t),
            Answer::Lines(_) => unreachable!(),
        }
    }

    fn lines(&mut self, img: &Image<u8>) -> Result<Vec<OcrLine>, String> {
        match self.answer("lines", img) {
            Answer::Lines(l) => Ok(l),
            Answer::Text(_) => unreachable!(),
        }
    }
}

fn rect(r: &Rect) -> Value {
    json!([r.x, r.y, r.w, r.h])
}

/// A contact as make_legacy_ref.py writes one.
fn contact(c: &Contact) -> Value {
    json!([
        rect(&c.name_box),
        rect(&c.range_box),
        c.range_km,
        c.name,
        c.kind.as_str()
    ])
}

fn boxes_json(b: &[legacy::LegacyBox]) -> Value {
    Value::Array(
        b.iter()
            .map(|b| json!([b.x, b.y, b.w, b.h, b.core]))
            .collect(),
    )
}

/// Where two values first differ.
fn first_difference(want: &Value, got: &Value) -> String {
    match (want, got) {
        (Value::Array(a), Value::Array(b)) => {
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                if x != y {
                    return format!("[{i}] {}", first_difference(x, y));
                }
            }
            format!("{} items, want {}", b.len(), a.len())
        }
        _ => format!("want {want}, got {got}"),
    }
}

#[test]
fn the_legacy_reader_reads_as_sc_detector() {
    let Some(dir) = legacy_dir() else {
        eprintln!("skipped: SCW_PARITY_DATA is not set or holds no legacy/legacy.json");
        return;
    };
    let fx: Value =
        serde_json::from_slice(&std::fs::read(dir.join("legacy.json")).unwrap()).unwrap();
    let frames: Vec<PathBuf> = std::fs::read_to_string(dir.join("frames.txt"))
        .unwrap()
        .lines()
        .map(PathBuf::from)
        .collect();
    let refs = fx["frames"].as_array().unwrap();
    assert_eq!(frames.len(), refs.len());
    let limit: usize = std::env::var("SCW_LEGACY_LIMIT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(usize::MAX);
    let e2e = std::env::var_os("SCW_LEGACY_E2E").is_some();
    let engine = e2e.then(|| Ocr::embedded().unwrap());

    // RapidOCR's answers, by OCR input.
    let mut known: HashMap<(String, String), Answer> = HashMap::new();
    if !e2e {
        for r in refs {
            for mode in [
                "detect",
                "detect_gate",
                "detect_haze",
                "detect_orphan",
                "focused",
                "focused_mask",
            ] {
                for c in r[mode]["ocr"].as_array().unwrap() {
                    let k = (c[0].as_str().unwrap().into(), c[1].as_str().unwrap().into());
                    let a = if c[0] == "text" {
                        Answer::Text(c[2].as_str().unwrap().into())
                    } else {
                        Answer::Lines(c[2].as_array().unwrap().iter().map(line_of).collect())
                    };
                    known.insert(k, a);
                }
            }
        }
    }

    let py = Options {
        as_python: true,
        ..Options::default()
    };
    let mut bad: Vec<String> = Vec::new();
    let mut counts: HashMap<&str, (usize, usize)> = HashMap::new();
    let mut timing: HashMap<&str, (f64, f64)> = HashMap::new();
    let mut missing = 0usize;
    let mut frames_read = 0usize;
    let t0 = Instant::now();
    for (n, (path, r)) in frames.iter().zip(refs).enumerate().take(limit) {
        let frame = read_bgr(path);
        frames_read += 1;
        let h = frame.height;
        let mut check = |what: &str, want: &Value, got: Value| {
            let e = counts
                .entry(match what {
                    "boxes" | "boxes_haze" | "pairs" | "shape_scores" | "gate_scores" | "cheap"
                    | "cheap_gate" => "before OCR",
                    _ => "contacts",
                })
                .or_default();
            e.0 += 1;
            if *want != got {
                e.1 += 1;
                bad.push(format!(
                    "frame {n} {what}: {}",
                    first_difference(want, &got)
                ));
            }
        };

        let boxes = legacy::find_all_boxes(&frame, h, false);
        check("boxes", &r["boxes"], boxes_json(&boxes));
        check(
            "boxes_haze",
            &r["boxes_haze"],
            boxes_json(&legacy::find_all_boxes(&frame, h, true)),
        );
        let pairs = legacy::pair_boxes(&boxes, h);
        check(
            "pairs",
            &r["pairs"],
            json!(pairs.iter().map(|&(a, b)| [a, b]).collect::<Vec<_>>()),
        );
        let crop = |i: usize| boxes[i].crop(&frame, 6);
        if let Some(want) = r.get("shape_scores") {
            let got: Vec<f64> = pairs
                .iter()
                .map(|&(_, r)| legacy::range_shape_score(&crop(r)))
                .collect();
            check("shape_scores", want, json!(got));
        }
        let got: Vec<f64> = pairs
            .iter()
            .map(|&(_, r)| legacy::km_gate::score(&crop(r), legacy::km_gate::template()))
            .collect();
        check("gate_scores", &r["gate_scores"], json!(got));
        for (what, gate) in [("cheap", false), ("cheap_gate", true)] {
            let got: Vec<Value> = legacy::cheap_pass(&frame, gate)
                .iter()
                .map(|(a, b)| json!([rect(a), rect(b)]))
                .collect();
            check(what, &r[what], Value::Array(got));
        }

        let mut whole = Vec::new();
        let mut centre = Vec::new();
        for mode in [
            "detect",
            "detect_gate",
            "detect_haze",
            "detect_orphan",
            "focused",
            "focused_mask",
        ] {
            let opt = Options {
                gate: mode == "detect_gate",
                haze: mode == "detect_haze",
                dim_range_retry: mode == "detect_orphan",
                ..py
            };
            let mut reader = Reader {
                known: &mut known,
                engine: engine.as_ref(),
                calls: Vec::new(),
                missing: 0,
                spent: 0.0,
            };
            let t = Instant::now();
            let found = match mode {
                "focused" => legacy::detect_focused(&frame, &mut reader, None, true, &opt),
                "focused_mask" => legacy::detect_focused(&frame, &mut reader, None, false, &opt),
                _ => legacy::detect(&frame, &mut reader, h, legacy::MAX_OCR_PAIRS, &opt),
            }
            .unwrap();
            let all = t.elapsed().as_secs_f64();
            let e = timing.entry(mode).or_default();
            e.0 += all - reader.spent;
            e.1 += reader.spent;
            missing += reader.missing;
            let want_calls: Vec<Value> = r[mode]["ocr"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| json!([c[0], c[1]]))
                .collect();
            let got_calls: Vec<Value> = reader.calls.iter().map(|(k, h)| json!([k, h])).collect();
            check(
                &format!("{mode} OCR calls"),
                &Value::Array(want_calls),
                Value::Array(got_calls),
            );
            check(
                mode,
                &r[mode]["contacts"],
                Value::Array(found.iter().map(contact).collect()),
            );
            match mode {
                "detect" => whole = found,
                "focused" => centre = found,
                _ => {}
            }
        }
        let dual = legacy::merge_contacts(whole, centre, h);
        check(
            "dual",
            &r["dual"],
            Value::Array(dual.iter().map(contact).collect()),
        );
        if (n + 1) % 25 == 0 {
            eprintln!(
                "{} frames, {} differences, {:.0} s",
                n + 1,
                bad.len(),
                t0.elapsed().as_secs_f64()
            );
        }
    }
    let mut c: Vec<_> = counts.into_iter().collect();
    c.sort();
    eprintln!(
        "checked: {}; {:.0} s",
        c.iter()
            .map(|(k, (n, b))| format!("{k} {n} ({b} differ)"))
            .collect::<Vec<_>>()
            .join(", "),
        t0.elapsed().as_secs_f64()
    );
    let mut tm: Vec<_> = timing.into_iter().collect();
    tm.sort_by(|a, b| a.0.cmp(b.0));
    eprintln!(
        "ms a frame, the reader's own work (+ {}): {}; {missing} crops never seen in the reference",
        if e2e {
            "the port's OCR"
        } else {
            "looking answers up"
        },
        tm.iter()
            .map(|(m, (own, ocr))| format!(
                "{m} {:.0} (+{:.0})",
                own * 1e3 / frames_read as f64,
                ocr * 1e3 / frames_read as f64
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if e2e {
        eprintln!("{} differences end to end:\n{}", bad.len(), bad.join("\n"));
        return;
    }
    assert!(
        bad.is_empty(),
        "{} differences:\n{}",
        bad.len(),
        bad.iter().take(40).cloned().collect::<Vec<_>>().join("\n")
    );
}
