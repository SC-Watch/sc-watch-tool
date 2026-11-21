//! scw-vision's image functions against OpenCV 5.0.0's own output, byte for
//! byte, on the synthetic fixtures made by tests/fixtures/make_fixtures.py.

use scw_vision::{Image, cv};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::OnceLock;

struct Fixtures {
    blob: Vec<u8>,
    cases: Vec<Value>,
}

fn fixtures() -> &'static Fixtures {
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

enum Arr {
    U8(Vec<u8>),
    I32(Vec<i32>),
    F32(Vec<f32>),
    F64(Vec<f64>),
}

struct Array {
    shape: Vec<usize>,
    data: Arr,
}

fn array(offset: u64) -> Array {
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

fn dims(shape: &[usize]) -> (usize, usize, usize) {
    match shape.len() {
        2 => (shape[1], shape[0], 1),
        3 => (shape[1], shape[0], shape[2]),
        _ => panic!("not an image: {shape:?}"),
    }
}

fn img_u8(a: Array) -> Image<u8> {
    let (w, h, c) = dims(&a.shape);
    match a.data {
        Arr::U8(v) => Image::from_vec(w, h, c, v),
        _ => panic!("expected u8"),
    }
}

fn img_f32(a: Array) -> Image<f32> {
    let (w, h, c) = dims(&a.shape);
    match a.data {
        Arr::F32(v) => Image::from_vec(w, h, c, v),
        _ => panic!("expected f32"),
    }
}

fn ints(a: Array) -> Vec<i32> {
    match a.data {
        Arr::I32(v) => v,
        _ => panic!("expected i32"),
    }
}

fn input(c: &Value, i: usize) -> Array {
    array(c["inputs"][i].as_u64().unwrap())
}

fn output(c: &Value, i: usize) -> Array {
    array(c["outputs"][i].as_u64().unwrap())
}

fn param(c: &Value, k: &str) -> f64 {
    c["params"][k].as_f64().unwrap()
}

/// Compares two sequences; None when equal, else a description.
fn diff<T: PartialEq + std::fmt::Debug>(want: &[T], got: &[T]) -> Option<String> {
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

fn image_diff<T: PartialEq + std::fmt::Debug>(want: &Image<T>, got: &Image<T>) -> Option<String> {
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
fn check(op: &str, run: impl Fn(&Value) -> Option<String>) {
    let cases: Vec<&Value> = fixtures().cases.iter().filter(|c| c["op"] == op).collect();
    assert!(!cases.is_empty(), "no fixtures for {op}");
    let failures: Vec<String> = cases
        .iter()
        .filter_map(|c| run(c).map(|d| format!("  {}: {d}", c["name"].as_str().unwrap())))
        .collect();
    assert!(
        failures.is_empty(),
        "{op}: {} of {} cases differ from OpenCV\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

#[test]
fn threshold_binary() {
    check("threshold_binary", |c| {
        let got =
            cv::threshold_binary(&img_u8(input(c, 0)), param(c, "thresh"), param(c, "maxval"));
        image_diff(&img_u8(output(c, 0)), &got)
    });
}

#[test]
fn subtract() {
    check("subtract", |c| {
        image_diff(
            &img_u8(output(c, 0)),
            &cv::subtract(&img_u8(input(c, 0)), &img_u8(input(c, 1))),
        )
    });
}

#[test]
fn lut() {
    check("lut", |c| {
        let table: [u8; 256] = match input(c, 1).data {
            Arr::U8(v) => v.try_into().unwrap(),
            _ => panic!(),
        };
        image_diff(
            &img_u8(output(c, 0)),
            &cv::lut(&img_u8(input(c, 0)), &table),
        )
    });
}

#[test]
fn count_and_bounding_rect() {
    check("count_bounding", |c| {
        let m = img_u8(input(c, 0));
        let (x, y, w, h) = cv::bounding_rect(&m);
        let got = vec![
            cv::count_nonzero(&m) as i32,
            x as i32,
            y as i32,
            w as i32,
            h as i32,
        ];
        diff(&ints(output(c, 0)), &got)
    });
}

#[test]
fn otsu() {
    check("otsu", |c| {
        let src = img_u8(input(c, 0));
        let t = cv::otsu_threshold(&src);
        let want_t = match output(c, 0).data {
            Arr::F64(v) => v[0],
            _ => panic!(),
        };
        if t != want_t {
            return Some(format!("threshold {want_t} vs {t}"));
        }
        image_diff(&img_u8(output(c, 1)), &cv::threshold_binary(&src, t, 255.0))
    });
}

fn kernel(c: &Value) -> (usize, usize) {
    (param(c, "kw") as usize, param(c, "kh") as usize)
}

#[test]
fn erode() {
    check("erode", |c| {
        let (kw, kh) = kernel(c);
        image_diff(
            &img_u8(output(c, 0)),
            &cv::erode(&img_u8(input(c, 0)), kw, kh),
        )
    });
}

#[test]
fn dilate() {
    check("dilate", |c| {
        let (kw, kh) = kernel(c);
        image_diff(
            &img_u8(output(c, 0)),
            &cv::dilate(&img_u8(input(c, 0)), kw, kh),
        )
    });
}

#[test]
fn open() {
    check("open", |c| {
        let (kw, kh) = kernel(c);
        image_diff(
            &img_u8(output(c, 0)),
            &cv::open(&img_u8(input(c, 0)), kw, kh),
        )
    });
}

#[test]
fn tophat() {
    check("tophat", |c| {
        let (kw, kh) = kernel(c);
        image_diff(
            &img_u8(output(c, 0)),
            &cv::tophat(&img_u8(input(c, 0)), kw, kh),
        )
    });
}

#[test]
fn resize_cubic() {
    check("resize_cubic", |c| {
        let f = param(c, "fx");
        image_diff(
            &img_u8(output(c, 0)),
            &cv::resize_cubic_by(&img_u8(input(c, 0)), f, f),
        )
    });
}

/// The one function NOT held byte for byte. OpenCV hands float resizes to
/// Intel's IPP (bundled in the opencv-python wheels), whose arithmetic is not
/// published: no order of operations or precision tried reproduces it, and
/// about half the values differ by 1-4 ulp (at most 6.1e-5 on values up to
/// ~300). Its only use is the O/0 rule, which thresholds this image at the
/// midpoint of two percentiles; that rule's decisions are held exactly at the
/// reader level, against IRIS's own reference readings.
#[test]
fn resize_cubic_f32() {
    check("resize_cubic_f32", |c| {
        let f = param(c, "fx");
        let want = img_f32(output(c, 0));
        let got = cv::resize_cubic_f32_by(&img_f32(input(c, 0)), f, f);
        if (want.width, want.height) != (got.width, got.height) {
            return Some(format!(
                "shape {}x{} vs {}x{}",
                want.width, want.height, got.width, got.height
            ));
        }
        let worst = want
            .data
            .iter()
            .zip(&got.data)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        (worst > 1e-4).then(|| format!("max |diff| {worst:e} above 1e-4"))
    });
}

#[test]
fn resize_area() {
    check("resize_area", |c| {
        let (dw, dh) = (param(c, "dw") as usize, param(c, "dh") as usize);
        image_diff(
            &img_u8(output(c, 0)),
            &cv::resize_area_to(&img_u8(input(c, 0)), dw, dh),
        )
    });
}

#[test]
fn resize_linear() {
    check("resize_linear", |c| {
        let (dw, dh) = (param(c, "dw") as usize, param(c, "dh") as usize);
        image_diff(
            &img_u8(output(c, 0)),
            &cv::resize_linear_to(&img_u8(input(c, 0)), dw, dh),
        )
    });
}

#[test]
fn blur() {
    check("blur", |c| {
        image_diff(
            &img_u8(output(c, 0)),
            &cv::blur(&img_u8(input(c, 0)), param(c, "k") as usize),
        )
    });
}

#[test]
fn connected_components() {
    check("components", |c| {
        let (labels, stats) =
            cv::connected_components(&img_u8(input(c, 0)), param(c, "connectivity") as u8);
        let want_labels = ints(output(c, 0));
        if let Some(d) = diff(&want_labels, &labels.data) {
            return Some(format!("labels: {d}"));
        }
        let got_stats: Vec<i32> = stats.iter().flatten().copied().collect();
        diff(&ints(output(c, 1)), &got_stats).map(|d| format!("stats: {d}"))
    });
}

#[test]
fn bgr_to_gray() {
    check("bgr2gray", |c| {
        image_diff(
            &img_u8(output(c, 0)),
            &cv::bgr_to_gray(&img_u8(input(c, 0))),
        )
    });
}
