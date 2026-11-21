//! The IRIS reader against IRIS's OWN readings of real game frames - the
//! reference the Python port was held to (tools/iris_parity.py), stage by
//! stage: mask, pieces in both alphabets, name, distance.
//!
//! Real frames name real players, so this data never enters the repository.
//! Point SCW_PARITY_DATA at a folder holding `iris-golden/` (the bench
//! harness's --golden output: crops as PNG plus golden.json); without it the
//! test reports that it skipped and passes.

mod reading;

use reading::MaskCheck;
use scw_vision::{Image, Rect};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn golden_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("SCW_PARITY_DATA")?).join("iris-golden");
    dir.join("golden.json").is_file().then_some(dir)
}

/// A PNG as OpenCV's `imread(IMREAD_COLOR)` gives it: 8-bit BGR.
fn read_bgr(path: &Path) -> Image<u8> {
    let mut dec = png::Decoder::new(std::fs::File::open(path).unwrap());
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut r = dec.read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).unwrap();
    let (w, h) = (info.width as usize, info.height as usize);
    let n = info.color_type.samples();
    let mut data = Vec::with_capacity(w * h * 3);
    for px in buf[..info.buffer_size()].chunks_exact(n) {
        match n {
            1 | 2 => data.extend([px[0]; 3]),
            _ => data.extend([px[2], px[1], px[0]]),
        }
    }
    Image::from_vec(w, h, 3, data)
}

#[test]
fn iris_reference_readings() {
    let Some(dir) = golden_dir() else {
        eprintln!("skipped: SCW_PARITY_DATA is not set or holds no iris-golden/golden.json");
        return;
    };
    let records: Vec<Value> =
        serde_json::from_slice(&std::fs::read(dir.join("golden.json")).unwrap()).unwrap();
    let rr = reading::row_reader();
    let mut ok: BTreeMap<&str, usize> = BTreeMap::new();
    let mut bad: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut rows = 0;
    let started = std::time::Instant::now();
    for rec in &records {
        let crop = rec["crop"].as_str().unwrap();
        let img = read_bgr(&dir.join(crop));
        for row in rec["rows"].as_array().unwrap() {
            rows += 1;
            let b: Vec<i32> = row["box"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_i64().unwrap() as i32)
                .collect();
            let rect = Rect::new(b[0], b[1], b[2], b[3]);
            for (stage, r) in reading::compare_row_stages(&rr, &img, rect, row, MaskCheck::Sha1) {
                match r {
                    Ok(()) => *ok.entry(stage).or_default() += 1,
                    // Where and how, never what: a text can be a real name.
                    Err(e) => bad
                        .entry(stage)
                        .or_default()
                        .push(format!("{crop} {b:?}: {e}")),
                }
            }
        }
    }
    let secs = started.elapsed().as_secs_f64();
    // The same timing as iris_parity.py's: one name read and one distance
    // read a row, each from the box (its own mask).
    let (mut t_name, mut t_dist) = (0.0, 0.0);
    for rec in &records {
        let img = read_bgr(&dir.join(rec["crop"].as_str().unwrap()));
        for row in rec["rows"].as_array().unwrap() {
            let b: Vec<i32> = row["box"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_i64().unwrap() as i32)
                .collect();
            let rect = Rect::new(b[0], b[1], b[2], b[3]);
            let t = std::time::Instant::now();
            std::hint::black_box(rr.read_name(&img, rect));
            t_name += t.elapsed().as_secs_f64();
            let t = std::time::Instant::now();
            std::hint::black_box(rr.read_distance(&img, rect));
            t_dist += t.elapsed().as_secs_f64();
        }
    }
    let n = rows.max(1) as f64;
    println!(
        "{} crops, {rows} text rows; checked in {secs:.1} s; a read takes name {:.2} ms, distance {:.2} ms",
        records.len(),
        1000.0 * t_name / n,
        1000.0 * t_dist / n
    );
    for (stage, n) in &ok {
        println!("   {stage:<12} ok {n}");
    }
    for (stage, list) in &bad {
        println!("   {stage:<12} DIFFERS {}", list.len());
        for e in list.iter().take(4) {
            println!("      {e}");
        }
    }
    assert!(bad.is_empty(), "the reader differs from IRIS's readings");
}
