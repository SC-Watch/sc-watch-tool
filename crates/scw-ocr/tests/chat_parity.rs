//! The chat reader against chat.py itself (tests/fixtures/make_chat_ref.py).
//!
//! Always: `handles_in` on generated lines (every channel tag and its OCR
//! spellings, the closing bracket's misreads, Unicode spaces, digits, the
//! words that are not handles, long and short handles, several tags in a
//! line), and the chat window's place and crop on screens of many sizes,
//! helmet on and off (fixtures/chat_ops.json, placeholder handles only).
//!
//! With SCW_PARITY_DATA/chat (real frames name real players, so it stays
//! local): every frame of the five corpora read as chat.read_located reads
//! it - the place worked out from the frame and, every tenth frame, one set
//! by hand - each crop handed to the OCR byte for byte, fed RapidOCR's
//! recorded lines, and the handles, lines, scores and place the same. With
//! SCW_CHAT_E2E the port's own OCR reads instead and the differences are
//! listed. Without the data that part reports that it skipped and passes.

mod common;

use std::collections::HashMap;
use std::path::PathBuf;

use common::{key, read_bgr};
use scw_ocr::Ocr;
use scw_ocr::chat::{self, Region};
use scw_ocr::legacy::{OcrLine, Recognise};
use scw_vision::Image;
use serde_json::{Value, json};

fn region(v: &Value) -> Region {
    let a: Vec<f64> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect();
    (a[0], a[1], a[2], a[3])
}

#[test]
fn chat_lines_and_places_match_chat_py() {
    let fx: Value = serde_json::from_slice(
        &std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/chat_ops.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let mut bad = Vec::new();
    let cases = fx["handles_in"].as_array().unwrap();
    for c in cases {
        let got = json!(chat::handles_in(c[0].as_str().unwrap()));
        if got != c[1] {
            bad.push(format!("handles_in {}: python {} rust {got}", c[0], c[1]));
        }
    }
    for c in fx["default_region"].as_array().unwrap() {
        let (w, h) = (
            c[0].as_u64().unwrap() as usize,
            c[1].as_u64().unwrap() as usize,
        );
        let r = chat::default_region(w, h, c[2].as_bool().unwrap());
        if region(&c[3]) != r {
            bad.push(format!(
                "default_region {w}x{h} {}: python {} rust {r:?}",
                c[2], c[3]
            ));
        }
    }
    for c in fx["crop_region"].as_array().unwrap() {
        let (w, h) = (
            c[0].as_u64().unwrap() as usize,
            c[1].as_u64().unwrap() as usize,
        );
        let frame = Image::<u8>::new(w, h, 3);
        let (crop, (x0, y0)) = chat::crop_region(&frame, region(&c[2]));
        let got = json!([x0, y0, crop.width, crop.height]);
        if got != json!([c[3], c[4], c[5], c[6]]) {
            bad.push(format!(
                "crop_region {w}x{h} {}: python {c} rust {got}",
                c[2]
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} of {} differ:\n{}",
        bad.len(),
        cases.len(),
        bad.join("\n")
    );
}

/// The OCR the reader sees: RapidOCR's recorded lines (or, end to end, the
/// port's own OCR), every call's input key logged.
struct Reader<'a> {
    known: &'a mut HashMap<String, Vec<OcrLine>>,
    engine: Option<&'a Ocr>,
    calls: Vec<String>,
}

impl Recognise for Reader<'_> {
    fn text(&mut self, _: &Image<u8>) -> Result<String, String> {
        unreachable!("chat reads lines")
    }

    fn lines(&mut self, img: &Image<u8>) -> Result<Vec<OcrLine>, String> {
        let k = key(img);
        self.calls.push(k.clone());
        if let Some(l) = self.known.get(&k) {
            return Ok(l.clone());
        }
        let l = match self.engine {
            Some(mut e) => e.lines(img)?,
            None => Vec::new(),
        };
        self.known.insert(k, l.clone());
        Ok(l)
    }
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

#[test]
fn the_chat_reads_as_chat_py_on_real_frames() {
    let Some(dir) = std::env::var_os("SCW_PARITY_DATA")
        .map(|d| PathBuf::from(d).join("chat"))
        .filter(|d| d.join("chat.json").is_file())
    else {
        eprintln!("skipped: SCW_PARITY_DATA is not set or holds no chat/chat.json");
        return;
    };
    let fx: Value = serde_json::from_slice(&std::fs::read(dir.join("chat.json")).unwrap()).unwrap();
    let frames: Vec<PathBuf> = std::fs::read_to_string(dir.join("frames.txt"))
        .unwrap()
        .lines()
        .map(PathBuf::from)
        .collect();
    let refs = fx["frames"].as_array().unwrap();
    assert_eq!(frames.len(), refs.len());
    let set = region(&fx["set_region"]);
    let e2e = std::env::var_os("SCW_CHAT_E2E").is_some();
    let engine = e2e.then(|| Ocr::embedded().unwrap());
    let mut known: HashMap<String, Vec<OcrLine>> = HashMap::new();
    if !e2e {
        for r in refs {
            for mode in ["auto", "set"] {
                for c in r
                    .get(mode)
                    .and_then(|m| m["ocr"].as_array())
                    .into_iter()
                    .flatten()
                {
                    known.insert(
                        c[0].as_str().unwrap().into(),
                        c[1].as_array().unwrap().iter().map(line_of).collect(),
                    );
                }
            }
        }
    }
    let (mut bad, mut checked, mut handles) = (Vec::new(), 0, 0);
    for (n, (path, r)) in frames.iter().zip(refs).enumerate() {
        let frame = read_bgr(path);
        for (mode, region) in [("auto", None), ("set", Some(set))] {
            let Some(want) = r.get(mode) else {
                continue;
            };
            let mut reader = Reader {
                known: &mut known,
                engine: engine.as_ref(),
                calls: Vec::new(),
            };
            let (lines, place) = chat::read_located(&frame, &mut reader, region).unwrap();
            let got = json!({
                "lines": lines.iter().map(|c| json!([c.handle, c.text, c.conf])).collect::<Vec<_>>(),
                "place": place.as_str(),
                "calls": reader.calls,
            });
            let want = json!({
                "lines": want["lines"],
                "place": want["place"],
                "calls": want["ocr"].as_array().unwrap().iter().map(|c| c[0].clone()).collect::<Vec<_>>(),
            });
            checked += 1;
            handles += lines.len();
            for part in ["calls", "lines", "place"] {
                // End to end the scores are rten's, not onnxruntime's:
                // within 1e-4 (step 1 measured 2.4e-5); the rest exact.
                let same = if e2e && part == "lines" {
                    let (g, w) = (
                        got[part].as_array().unwrap(),
                        want[part].as_array().unwrap(),
                    );
                    g.len() == w.len()
                        && g.iter().zip(w).all(|(a, b)| {
                            a[0] == b[0]
                                && a[1] == b[1]
                                && (a[2].as_f64().unwrap() - b[2].as_f64().unwrap()).abs() <= 1e-4
                        })
                } else {
                    got[part] == want[part]
                };
                if !same {
                    // counts only: the lines hold real handles
                    bad.push(format!(
                        "frame {n} {mode} {part} differ ({} against {} items)",
                        got[part].as_array().map_or(1, Vec::len),
                        want[part].as_array().map_or(1, Vec::len)
                    ));
                }
            }
        }
    }
    eprintln!(
        "{checked} chat reads, {handles} handles, {} differences{}",
        bad.len(),
        if e2e { " (the port's own OCR)" } else { "" }
    );
    if e2e {
        eprintln!("{}", bad.join("\n"));
        return;
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
