//! scw-vision's mining reader against IRIS's own, on real frames of the
//! game's scanner: for each frame the scan's search (the search area,
//! `search`) and, when it found the row, the locked read (the lock area
//! around it, `read`) - every field of both reads, every double to the bit.
//!
//! Real frames show the player's screen, so this data never enters the
//! repository: SCW_PARITY_DATA/mining holds `frames/` (the PNGs) and
//! `iris_frames.json`, the bench harness's `--mining-frames` output for
//! them. Without it the test reports that it skipped and passes.

use scw_vision::mining::reader::{MiningSignalReader, lock_area, search_area, to_window_rect};
use scw_vision::mining::table::MiningTable;
use scw_vision::mining::{SignalRead, base64_decode, snap};
use scw_vision::{Image, Rect};
use serde_json::Value;
use std::path::{Path, PathBuf};

fn data() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("SCW_PARITY_DATA")?).join("mining");
    dir.join("iris_frames.json").is_file().then_some(dir)
}

/// A PNG as OpenCV's `imread(IMREAD_COLOR)` gives it: 8-bit BGR.
fn read_bgr(path: &Path) -> Image<u8> {
    let mut dec = png::Decoder::new(std::fs::File::open(path).unwrap());
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = dec.read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).unwrap();
    let n = info.color_type.samples();
    let mut out = Vec::with_capacity(info.width as usize * info.height as usize * 3);
    for px in buf[..info.buffer_size()].chunks(n) {
        if n < 3 {
            out.extend([px[0]; 3]);
        } else {
            out.extend([px[2], px[1], px[0]]);
        }
    }
    Image::from_vec(info.width as usize, info.height as usize, 3, out)
}

fn crop(img: &Image<u8>, r: Rect) -> Image<u8> {
    let mut out = Image::new(r.w as usize, r.h as usize, 3);
    for y in 0..r.h as usize {
        let row = img.row(r.y as usize + y);
        let start = r.x as usize * 3;
        out.data[y * r.w as usize * 3..(y + 1) * r.w as usize * 3]
            .copy_from_slice(&row[start..start + r.w as usize * 3]);
    }
    out
}

fn rect(v: &Value) -> Rect {
    let i = |k: usize| v[k].as_i64().unwrap() as i32;
    Rect::new(i(0), i(1), i(2), i(3))
}

fn scores_b64(read: &SignalRead) -> Value {
    match &read.digit_scores {
        None => Value::Null,
        Some(s) => {
            let mut bytes = Vec::new();
            for digit in s {
                for x in digit {
                    bytes.extend(x.to_le_bytes());
                }
            }
            Value::String(base64_encode(&bytes))
        }
    }
}

fn base64_encode(bytes: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            if k <= c.len() {
                out.push(A[(n >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The read as the harness writes it.
fn as_json(r: &SignalRead) -> Value {
    let b = r.signal_box;
    serde_json::json!({
        "state": r.state.as_str(), "text": r.text, "value": r.value,
        "box": [b.x.to_bits(), b.y.to_bits(), b.w.to_bits(), b.h.to_bits()],
        "row_top": r.row_top.to_bits(), "dh": r.digit_height.to_bits(),
        "tc": [r.text_colour.b, r.text_colour.g, r.text_colour.r],
        "bc": [r.back_colour.b, r.back_colour.g, r.back_colour.r],
        "off": r.offset_px.to_bits(), "min": r.min_score.to_bits(),
        "scores": scores_b64(r),
    })
}

/// The tolerance for digit scores: IRIS's OpenCV resizes each cell with
/// Intel's IPP, whose last bit depends on the processor (within 1.1e-6 of
/// OpenCV's own code, tests/mining_units.rs); everything a score decides -
/// the digit, the state, the value - is compared exactly.
const SCORE_TOLERANCE: f64 = 1e-5;

fn double(v: &Value) -> f64 {
    f64::from_bits(v.as_u64().unwrap())
}

/// Equal to the bit, or both NaN (.NET's NaN has its sign bit set).
fn same_double(a: &Value, b: &Value) -> bool {
    let (x, y) = (double(a), double(b));
    x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan())
}

fn scores_close(a: &Value, b: &Value, worst: &mut f64) -> bool {
    match (a.as_str(), b.as_str()) {
        (None, None) => true,
        (Some(x), Some(y)) => {
            let (x, y) = (base64_decode(x).unwrap(), base64_decode(y).unwrap());
            x.len() == y.len()
                && x.as_chunks::<8>()
                    .0
                    .iter()
                    .zip(y.as_chunks::<8>().0)
                    .all(|(p, q)| {
                        let d = (f64::from_le_bytes(*p) - f64::from_le_bytes(*q)).abs();
                        *worst = worst.max(d);
                        d <= SCORE_TOLERANCE
                    })
        }
        _ => false,
    }
}

/// Which fields differ - names only, and the texts (signal values).
fn differences(got: &Value, want: &Value, worst: &mut f64) -> Vec<String> {
    let mut out = Vec::new();
    for k in ["state", "text", "value", "box", "dh", "tc", "bc"] {
        if got[k] != want[k] {
            out.push(match k {
                "state" | "text" | "value" => format!("{k} {} vs {}", want[k], got[k]),
                _ => k.to_string(),
            });
        }
    }
    for k in ["row_top", "off"] {
        if !same_double(&got[k], &want[k]) {
            out.push(k.to_string());
        }
    }
    let (gm, wm) = (double(&got["min"]), double(&want["min"]));
    if !(gm.is_nan() && wm.is_nan() || (gm - wm).abs() <= SCORE_TOLERANCE) {
        out.push("min".to_string());
    }
    if !scores_close(&got["scores"], &want["scores"], worst) {
        out.push("scores".to_string());
    }
    out
}

#[test]
fn iris_reads_the_scanner_the_same() {
    let Some(dir) = data() else {
        eprintln!("skipped: SCW_PARITY_DATA holds no mining/iris_frames.json");
        return;
    };
    let records: Vec<Value> =
        serde_json::from_slice(&std::fs::read(dir.join("iris_frames.json")).unwrap()).unwrap();
    let reader = MiningSignalReader::default();
    let s = &reader.settings;
    let (mut ok, mut bad, mut locks, mut signals) = (0, Vec::new(), 0, 0);
    let mut worst = 0f64;
    let table = MiningTable::embedded();
    let mut snapped: Vec<String> = Vec::new();
    let started = std::time::Instant::now();
    let (mut t_search, mut t_read) = (0f64, 0f64);
    for rec in &records {
        let name = rec["frame"].as_str().unwrap();
        let img = read_bgr(&dir.join("frames").join(name));
        let (w, h) = (img.width as i32, img.height as i32);
        let region = to_window_rect(search_area(w, h, s), w, h);
        let mut problems = Vec::new();
        if region != rect(&rec["search"]["region"]) {
            problems.push("search region".to_string());
        }
        let search_crop = crop(&img, region);
        let t0 = std::time::Instant::now();
        let found = reader.search(&search_crop, region, w, h);
        t_search += t0.elapsed().as_secs_f64();
        for d in differences(&as_json(&found), &rec["search"]["read"], &mut worst) {
            problems.push(format!("search {d}"));
        }
        match (found.found(), rec["lock"].is_null()) {
            (true, false) => {
                locks += 1;
                let lr = to_window_rect(lock_area(w, h, found.row_top, s), w, h);
                if lr != rect(&rec["lock"]["region"]) {
                    problems.push("lock region".to_string());
                }
                let lock_crop = crop(&img, lr);
                let t0 = std::time::Instant::now();
                let read = reader.read(&lock_crop, lr, w, h);
                t_read += t0.elapsed().as_secs_f64();
                signals += read.value.is_some() as usize;
                for d in differences(&as_json(&read), &rec["lock"]["read"], &mut worst) {
                    problems.push(format!("lock {d}"));
                }
            }
            (false, true) => {}
            _ => problems.push("locked or not".to_string()),
        }
        // what the live reader would make of it: one doubtful digit snapped
        // to the table (reported, not compared - IRIS's harness reads raw)
        if let Some(s) = snap::apply(&found, &table, s) {
            snapped.push(format!("{name}: {} -> {}", found.text, s.text));
        }
        if problems.is_empty() {
            ok += 1;
        } else {
            bad.push(format!("{name}: {}", problems.join(", ")));
        }
    }
    println!(
        "{} frames: {ok} the same as IRIS ({locks} locked, {signals} read as a signal); \
         digit scores within {worst:.1e} of IRIS's; {:.1} ms a frame with the PNG decode",
        records.len(),
        started.elapsed().as_secs_f64() * 1000.0 / records.len().max(1) as f64
    );
    println!(
        "   the reader alone: search {:.1} ms a frame, locked read {:.1} ms",
        t_search * 1000.0 / records.len().max(1) as f64,
        t_read * 1000.0 / locks.max(1) as f64
    );
    for s in &snapped {
        println!("   the table would snap {s}");
    }
    for b in &bad {
        println!("   DIFFERS {b}");
    }
    assert!(
        bad.is_empty(),
        "{} of {} frames differ from IRIS",
        bad.len(),
        records.len()
    );
}
