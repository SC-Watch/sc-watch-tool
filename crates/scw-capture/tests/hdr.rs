//! The HDR conversion against hdr_capture.py's own tables and output
//! (tests/fixtures/make_fixtures.py): every one of the 65,536 float16 bit
//! patterns at ten SDR white levels, and a frame with a padded pitch.

use scw_capture::hdr::{lut_for, to_bgr};
use serde_json::Value;
use std::path::PathBuf;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .unwrap()
}

#[test]
fn tables_match_hdr_capture() {
    let index: Value = serde_json::from_slice(&fixture("index.json")).unwrap();
    let luts = fixture("luts.bin");
    let mut bad = Vec::new();
    for (k, w) in index["whites"].as_array().unwrap().iter().enumerate() {
        let white = w.as_f64().unwrap();
        let want = &luts[k * 65536..(k + 1) * 65536];
        let got = lut_for(white);
        let differ: Vec<usize> = (0..65536).filter(|&i| want[i] != got[i]).collect();
        if let Some(&i) = differ.first() {
            bad.push(format!(
                "white {white}: {} of 65536 differ; first bit pattern {i:#06x}: python {}, rust {}",
                differ.len(),
                want[i],
                got[i]
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn frame_matches_hdr_capture() {
    let index: Value = serde_json::from_slice(&fixture("index.json")).unwrap();
    let f = &index["frame"];
    let (h, w, pitch) = (
        f["h"].as_u64().unwrap() as usize,
        f["w"].as_u64().unwrap() as usize,
        f["pitch"].as_u64().unwrap() as usize,
    );
    let raw: Vec<u16> = fixture("frame_f16.bin")
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    let got = to_bgr(
        &raw,
        w,
        h,
        pitch * 4,
        &lut_for(f["white"].as_f64().unwrap()),
    );
    assert_eq!(got.data, fixture("frame_bgr.bin"));
}
