//! The fixtures made by tests/fixtures/make_fixtures.py: one blob of arrays
//! (opencv.bin) and the index of cases that point into it.
#![allow(dead_code)]

use scw_vision::Image;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::OnceLock;

pub struct Fixtures {
    pub blob: Vec<u8>,
    pub cases: Vec<Value>,
}

pub fn fixtures() -> &'static Fixtures {
    static F: OnceLock<Fixtures> = OnceLock::new();
    F.get_or_init(|| {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures");
        let index: Value =
            serde_json::from_slice(&std::fs::read(dir.join("index.json")).unwrap()).unwrap();
        Fixtures {
            blob: std::fs::read(dir.join("opencv.bin")).unwrap(),
            cases: index["cases"].as_array().unwrap().clone(),
        }
    })
}

pub enum Arr {
    U8(Vec<u8>),
    I32(Vec<i32>),
    F32(Vec<f32>),
    F64(Vec<f64>),
}

pub struct Array {
    pub shape: Vec<usize>,
    pub data: Arr,
}

pub fn array(offset: u64) -> Array {
    let b = &fixtures().blob[offset as usize..];
    assert_eq!(&b[..4], b"SCWA", "bad fixture offset");
    let (dtype, ndim) = (b[4], b[5] as usize);
    let shape: Vec<usize> = (0..ndim)
        .map(|i| u32::from_le_bytes(b[6 + 4 * i..10 + 4 * i].try_into().unwrap()) as usize)
        .collect();
    let n: usize = shape.iter().product();
    let d = &b[6 + 4 * ndim..];
    let data = match dtype {
        0 => Arr::U8(d[..n].to_vec()),
        1 => Arr::I32(
            d[..4 * n]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| i32::from_le_bytes(*c))
                .collect(),
        ),
        2 => Arr::F32(
            d[..4 * n]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| f32::from_le_bytes(*c))
                .collect(),
        ),
        3 => Arr::F64(
            d[..8 * n]
                .as_chunks::<8>()
                .0
                .iter()
                .map(|c| f64::from_le_bytes(*c))
                .collect(),
        ),
        _ => panic!("unknown dtype {dtype}"),
    };
    Array { shape, data }
}

pub fn dims(shape: &[usize]) -> (usize, usize, usize) {
    match shape.len() {
        2 => (shape[1], shape[0], 1),
        3 => (shape[1], shape[0], shape[2]),
        _ => panic!("not an image: {shape:?}"),
    }
}

pub fn img_u8(a: Array) -> Image<u8> {
    let (w, h, c) = dims(&a.shape);
    match a.data {
        Arr::U8(v) => Image::from_vec(w, h, c, v),
        _ => panic!("expected u8"),
    }
}

pub fn img_f32(a: Array) -> Image<f32> {
    let (w, h, c) = dims(&a.shape);
    match a.data {
        Arr::F32(v) => Image::from_vec(w, h, c, v),
        _ => panic!("expected f32"),
    }
}

pub fn ints(a: Array) -> Vec<i32> {
    match a.data {
        Arr::I32(v) => v,
        _ => panic!("expected i32"),
    }
}

pub fn input(c: &Value, i: usize) -> Array {
    array(c["inputs"][i].as_u64().unwrap())
}

pub fn output(c: &Value, i: usize) -> Array {
    array(c["outputs"][i].as_u64().unwrap())
}

pub fn param(c: &Value, k: &str) -> f64 {
    c["params"][k].as_f64().unwrap()
}

/// Compares two sequences; None when equal, else a description.
pub fn diff<T: PartialEq + std::fmt::Debug>(want: &[T], got: &[T]) -> Option<String> {
    if want.len() != got.len() {
        return Some(format!("length {} vs {}", want.len(), got.len()));
    }
    let bad: Vec<usize> = (0..want.len()).filter(|&i| want[i] != got[i]).collect();
    bad.first().map(|&i| {
        format!(
            "{} of {} differ; first at {i}: opencv {:?}, rust {:?}",
            bad.len(),
            want.len(),
            want[i],
            got[i]
        )
    })
}

pub fn image_diff<T: PartialEq + std::fmt::Debug>(
    want: &Image<T>,
    got: &Image<T>,
) -> Option<String> {
    if (want.width, want.height, want.channels) != (got.width, got.height, got.channels) {
        return Some(format!(
            "shape {}x{}x{} vs {}x{}x{}",
            want.width, want.height, want.channels, got.width, got.height, got.channels
        ));
    }
    diff(&want.data, &got.data).map(|d| {
        let i = (0..want.data.len())
            .find(|&i| want.data[i] != got.data[i])
            .unwrap();
        let px = i / want.channels;
        format!("{d} (pixel x={}, y={})", px % want.width, px / want.width)
    })
}

/// Runs every case of one op; panics listing the ones that differ.
pub fn check(op: &str, run: impl Fn(&Value) -> Option<String>) {
    let cases: Vec<&Value> = fixtures().cases.iter().filter(|c| c["op"] == op).collect();
    assert!(!cases.is_empty(), "no fixtures for {op}");
    let failures: Vec<String> = cases
        .iter()
        .filter_map(|c| run(c).map(|d| format!("  {}: {d}", c["name"].as_str().unwrap())))
        .collect();
    assert!(
        failures.is_empty(),
        "{op}: {} of {} cases differ from the reference\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

pub fn floats(a: Array) -> Vec<f64> {
    match a.data {
        Arr::F64(v) => v,
        _ => panic!("expected f64"),
    }
}
