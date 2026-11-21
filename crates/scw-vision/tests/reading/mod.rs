//! One text row read by scw-vision and compared with an expectation in the
//! reference's JSON shape (iris_parity.py's stages): the mask, the pieces in
//! both alphabets, the name read and the distance read.
#![allow(dead_code)]

use scw_vision::iris::{LetterModel, NameplateReader, PieceRead, ReaderSettings, RowReader};
use scw_vision::{Image, Rect};
use serde_json::Value;
use std::path::PathBuf;

/// Score tolerances, as iris_parity.py's: the reference's log-probabilities
/// come from another order of operations (numpy's matrix product, or IRIS's
/// own build), so scores agree to about 1e-12, not to the bit.
const PIECE_TOL: f64 = 1e-6;
const NAME_TOL: f64 = 1e-5;
const O0_TOL: f64 = 1e-6;

pub fn model_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../models/iris-letters-model.bin")
}

pub fn model() -> LetterModel {
    LetterModel::from_bytes(&std::fs::read(model_path()).expect("models/iris-letters-model.bin"))
        .expect("the letter model loads")
}

pub fn row_reader() -> RowReader {
    RowReader::new(
        NameplateReader::new(model()).unwrap(),
        ReaderSettings::default(),
    )
}

/// How the expectation identifies the mask's pixels.
#[derive(Clone, Copy)]
pub enum MaskCheck {
    /// `crc`: zlib's CRC-32 (the synthetic fixtures).
    Crc32,
    /// `sha`: SHA-1 in upper-case hex (IRIS's reference readings).
    Sha1,
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, t) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *t = c;
    }
    let mut c = !0u32;
    for &b in data {
        c = table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

fn int(v: &Value) -> i64 {
    v.as_i64()
        .or_else(|| v.as_f64().map(|f| f as i64))
        .expect("an integer")
}

fn pieces_match(want: &Value, got: &PieceRead) -> Result<(), String> {
    if int(&want["cap"]) != got.cap as i64 {
        return Err(format!("cap {} vs {}", want["cap"], got.cap));
    }
    let wp = want["pieces"].as_array().unwrap();
    if wp.len() != got.pieces.len() {
        return Err(format!("{} pieces vs {}", wp.len(), got.pieces.len()));
    }
    for (k, (w, g)) in wp.iter().zip(&got.pieces).enumerate() {
        let (s, e) = (int(&w[0]) as usize, int(&w[1]) as usize);
        if (s, e) != (g.start, g.end) {
            return Err(format!("piece {k} span {s}..{e} vs {}..{}", g.start, g.end));
        }
        let text = w[2].as_str().unwrap();
        if text != g.text {
            // Lengths only: a reference text can be a real name.
            return Err(format!(
                "piece {k} text differs (len {} vs {})",
                text.len(),
                g.text.len()
            ));
        }
        let (sc, fr) = (w[3].as_f64().unwrap(), w[4].as_f64().unwrap());
        if (sc - g.score).abs() > PIECE_TOL || (fr - g.free_score).abs() > PIECE_TOL {
            return Err(format!(
                "piece {k} score {sc:.6}/{fr:.6} vs {:.6}/{:.6}",
                g.score, g.free_score
            ));
        }
    }
    let ws: Vec<(usize, usize)> = want["spans"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|p| (int(&p[0]) as usize, int(&p[1]) as usize))
                .collect()
        })
        .unwrap_or_default();
    if ws != got.spans {
        return Err(format!(
            "spans differ ({} vs {})",
            ws.len(),
            got.spans.len()
        ));
    }
    Ok(())
}

/// Where two texts differ, by position only.
fn text_diff(a: &str, b: &str) -> String {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len() != b.len() {
        return format!("length {} vs {}", a.len(), b.len());
    }
    let at: Vec<String> = (0..a.len())
        .filter(|&i| a[i] != b[i])
        .take(3)
        .map(|i| format!("at {i}"))
        .collect();
    at.join(", ")
}

/// Every stage's outcome, in order: "mask", "namePieces", "distPieces",
/// "name", "distance". The pieces are judged only when the mask matched;
/// the name and distance reads always.
pub fn compare_row_stages(
    rr: &RowReader,
    img: &Image<u8>,
    rect: Rect,
    expect: &Value,
    how: MaskCheck,
) -> Vec<(&'static str, Result<(), String>)> {
    let mut out = Vec::new();
    let mask = rr.mask(img, rect, false);
    let want = &expect["mask"];
    let mask_ok = match (&mask, want.is_null()) {
        (None, true) => Ok(()),
        (None, false) => Err("IRIS has a mask, ours none".to_string()),
        (Some(_), true) => Err("ours has a mask, IRIS none".to_string()),
        (Some(m), false) => {
            let mut bad = Vec::new();
            let fields = [
                ("w", m.width() as i64),
                ("h", m.height() as i64),
                ("ox", m.origin_x as i64),
                ("oy", m.origin_y as i64),
                ("col", m.col_offset as i64),
                ("row", m.row_offset as i64),
            ];
            for (k, v) in fields {
                if int(&want[k]) != v {
                    bad.push(k);
                }
            }
            let pixels_ok = match how {
                MaskCheck::Crc32 => int(&want["crc"]) as u32 == crc32(&m.data.data),
                MaskCheck::Sha1 => {
                    want["sha"].as_str().unwrap()
                        == sha1_smol::Sha1::from(&m.data.data)
                            .digest()
                            .to_string()
                            .to_uppercase()
                }
            };
            if !pixels_ok {
                bad.push("pixels");
            }
            if bad.is_empty() {
                Ok(())
            } else {
                Err(format!("{bad:?}"))
            }
        }
    };
    let judged = mask_ok.is_ok();
    out.push(("mask", mask_ok));
    if judged && let Some(m) = &mask {
        let s = &rr.settings;
        let name = rr
            .reader
            .read_pieces(m, Some(&rr.reader.name_alphabet), s.name_min_ink, s);
        out.push(("namePieces", pieces_match(&expect["namePieces"], &name)));
        let dist =
            rr.reader
                .read_pieces(m, Some(&rr.reader.distance_alphabet), s.distance_min_ink, s);
        out.push(("distPieces", pieces_match(&expect["distPieces"], &dist)));
    }

    let nr = rr.read_name(img, rect);
    let gn = &expect["name"];
    let name_ok = (|| {
        let text = gn["text"].as_str().unwrap();
        if text != nr.text {
            return Err(format!("text {}", text_diff(text, &nr.text)));
        }
        if (gn["oneWord"].as_bool(), gn["validChars"].as_bool())
            != (Some(nr.one_word), Some(nr.valid_chars))
        {
            return Err("flags".into());
        }
        let score = gn["score"].as_f64().unwrap();
        if (score - nr.score).abs() > NAME_TOL {
            return Err(format!("score {score:.6} vs {:.6}", nr.score));
        }
        let o0 = gn["o0"].as_array().unwrap();
        let same = o0.len() == nr.o0.len()
            && o0.iter().zip(&nr.o0).all(|(w, &(k, p))| {
                int(&w[0]) as usize == k && (w[1].as_f64().unwrap() - p).abs() <= O0_TOL
            });
        if !same {
            return Err(format!("O/0 evidence {o0:?} vs {:?}", nr.o0));
        }
        Ok(())
    })();
    out.push(("name", name_ok));

    let dr = rr.read_distance(img, rect);
    let gd = &expect["dist"];
    let dist_ok = match (&dr, gd.is_null()) {
        (None, true) => Ok(()),
        (None, false) => Err("IRIS reads, ours refuses".to_string()),
        (Some(_), true) => Err("IRIS refuses, ours reads".to_string()),
        (Some(d), false) => {
            let want = (
                gd["text"].as_str().unwrap(),
                gd["km"].as_bool().unwrap(),
                gd["speed"].as_bool().unwrap(),
            );
            let value = gd["value"].as_f64().unwrap();
            if want != (d.text.as_str(), d.kilometers, d.speed_group)
                || (value - d.value).abs() > 1e-9
            {
                Err(format!(
                    "{}/{}/{} vs {}/{}/{}",
                    want.0, want.1, want.2, d.text, d.kilometers, d.speed_group
                ))
            } else {
                Ok(())
            }
        }
    };
    out.push(("distance", dist_ok));
    out
}

/// The first stage that differs, if any.
pub fn compare_row(
    rr: &RowReader,
    img: &Image<u8>,
    rect: Rect,
    expect: &Value,
    how: MaskCheck,
) -> Result<(), String> {
    match compare_row_stages(rr, img, rect, expect, how)
        .into_iter()
        .find_map(|(stage, r)| r.err().map(|e| format!("{stage}: {e}")))
    {
        Some(e) => Err(e),
        None => Ok(()),
    }
}
