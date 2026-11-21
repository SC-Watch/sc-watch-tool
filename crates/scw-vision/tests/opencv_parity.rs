//! scw-vision's image functions against OpenCV 5.0.0's own output, byte for
//! byte, on the synthetic fixtures made by tests/fixtures/make_fixtures.py.

mod common;

use common::*;
use scw_vision::cv;
use serde_json::Value;

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

/// The glyph features' resize, exact: float sums in OpenCV's order.
#[test]
fn resize_area_f32() {
    check("resize_area_f32", |c| {
        let (dw, dh) = (param(c, "dw") as usize, param(c, "dh") as usize);
        let want = img_f32(output(c, 0));
        let got = cv::resize_area_f32_to(&img_f32(input(c, 0)), dw, dh);
        let bits =
            |i: &scw_vision::Image<f32>| i.data.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
        if (want.width, want.height) != (got.width, got.height) {
            return Some(format!(
                "shape {}x{} vs {}x{}",
                want.width, want.height, got.width, got.height
            ));
        }
        diff(&bits(&want), &bits(&got))
    });
}
