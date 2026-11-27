//! RapidOCR 1.2.3's TextDetector around PP-OCRv3's DB detector: the image
//! sized to a multiple of 32 (DetResizeForTest type 0), normalised
//! (NormalizeImage, ToCHWImage), run, and the map turned into text boxes
//! (DBPostProcess with dilation and the "fast" score, then
//! filter_tag_det_res).

use scw_vision::Image;
use scw_vision::cv::{dilate, resize_linear_to};

use crate::geom::{self, P32};

/// How the detector sizes its input (DetResizeForTest's limit).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Limit {
    /// Enlarge so the shorter side is at least this (RapidOCR's default,
    /// 736).
    Min(u32),
    /// Shrink so the longer side is at most this; never enlarge.
    Max(u32),
}

#[derive(Clone, Debug, PartialEq)]
pub struct DetSettings {
    pub limit: Limit,
    pub thresh: f32,
    pub box_thresh: f64,
    pub max_candidates: usize,
    pub unclip_ratio: f64,
    pub use_dilation: bool,
}

impl Default for DetSettings {
    fn default() -> Self {
        DetSettings {
            limit: Limit::Min(736),
            thresh: 0.3,
            box_thresh: 0.5,
            max_candidates: 1000,
            unclip_ratio: 1.6,
            use_dilation: true,
        }
    }
}

/// The detector's input size for an `h` x `w` image (resize_image_type0);
/// None when it would be empty.
pub fn input_size(h: usize, w: usize, limit: Limit) -> Option<(usize, usize)> {
    let (hf, wf) = (h as f64, w as f64);
    let ratio = match limit {
        Limit::Max(side) => {
            let side = side as f64;
            if h.max(w) as f64 > side {
                if h > w { side / hf } else { side / wf }
            } else {
                1.0
            }
        }
        Limit::Min(side) => {
            let side = side as f64;
            if (h.min(w) as f64) < side {
                if h < w { side / hf } else { side / wf }
            } else {
                1.0
            }
        }
    };
    let rh = (hf * ratio) as i64;
    let rw = (wf * ratio) as i64;
    // int(round(x / 32) * 32): Python rounds half to even
    let rh = ((rh as f64 / 32.0).round_ties_even() * 32.0) as i64;
    let rw = ((rw as f64 / 32.0).round_ties_even() * 32.0) as i64;
    if rw <= 0 || rh <= 0 {
        return None;
    }
    Some((rh as usize, rw as usize))
}

/// NormalizeImage + ToCHWImage on a BGR image: (x * 1/255 - mean) / std in
/// float32, channel by channel in the image's own (BGR) order.
pub fn normalise(img: &Image<u8>) -> Vec<f32> {
    let (w, h) = (img.width, img.height);
    let scale = (1.0f64 / 255.0) as f32;
    let mean = [0.485f64 as f32, 0.456f64 as f32, 0.406f64 as f32];
    let std = [0.229f64 as f32, 0.224f64 as f32, 0.225f64 as f32];
    let mut out = vec![0f32; 3 * w * h];
    for y in 0..h {
        let row = img.row(y);
        for x in 0..w {
            for c in 0..3 {
                let v = row[x * 3 + c] as f32;
                out[c * w * h + y * w + x] = (v * scale - mean[c]) / std[c];
            }
        }
    }
    out
}

/// DBPostProcess.box_score_fast: the mean of `pred` under the box.
fn box_score(pred: &[f32], w: usize, h: usize, b: &[P32; 4]) -> f64 {
    let fx = |f: fn(f32, f32) -> f32, init: f32| b.iter().fold(init, |a, p| f(a, p.x));
    let fy = |f: fn(f32, f32) -> f32, init: f32| b.iter().fold(init, |a, p| f(a, p.y));
    let clampi = |v: f32, hi: usize| (v as i32).clamp(0, hi as i32 - 1);
    let xmin = clampi(fx(f32::min, f32::INFINITY).floor(), w);
    let xmax = clampi(fx(f32::max, f32::NEG_INFINITY).ceil(), w);
    let ymin = clampi(fy(f32::min, f32::INFINITY).floor(), h);
    let ymax = clampi(fy(f32::max, f32::NEG_INFINITY).ceil(), h);
    let (mw, mh) = ((xmax - xmin + 1) as usize, (ymax - ymin + 1) as usize);
    let mut mask = vec![0u8; mw * mh];
    // box - (xmin, ymin) in float32, then astype(int32): truncation
    let pts: Vec<(i32, i32)> = b
        .iter()
        .map(|p| ((p.x - xmin as f32) as i32, (p.y - ymin as f32) as i32))
        .collect();
    crate::fill::fill_poly(&mut mask, mw, mh, &pts, 1);
    // cv2.mean with a mask: the sum in double over the masked pixels
    let (mut sum, mut n) = (0f64, 0usize);
    for yy in 0..mh {
        let row = &pred[(ymin as usize + yy) * w + xmin as usize..][..mw];
        for (xx, &p) in row.iter().enumerate() {
            if mask[yy * mw + xx] != 0 {
                sum += p as f64;
                n += 1;
            }
        }
    }
    if n == 0 { 0.0 } else { sum / n as f64 }
}

/// `cv2.findContours(mask, RETR_LIST, CHAIN_APPROX_SIMPLE)` as OpenCV 5.0
/// runs it from Python (a hierarchy is asked for, so not the RETR_LIST fast
/// path): the Suzuki scanner of contours_new.cpp over the image padded by a
/// zero pixel, outer borders and holes, each traced by icvFetchContourEx and
/// kept where its direction changes; in OpenCV's output order (the tree's
/// children, last found first). The point order matters: the hull - and so
/// minAreaRect's ties and float rounding - starts from it.
pub fn find_contours(mask: &Image<u8>) -> Vec<Vec<(i32, i32)>> {
    const RIGHT_NEW: i8 = (0x80u8 | 0x02) as i8; // nbd 2 | MASK8_RIGHT
    const NEW: i8 = 0x02;
    const DELTA: [(i32, i32); 8] = [
        (1, 0),
        (1, -1),
        (0, -1),
        (-1, -1),
        (-1, 0),
        (-1, 1),
        (0, 1),
        (1, 1),
    ];
    let (w, h) = (mask.width + 2, mask.height + 2);
    let mut img = vec![0i8; w * h];
    for y in 0..mask.height {
        for (x, &v) in mask.row(y).iter().enumerate() {
            img[(y + 1) * w + x + 1] = (v != 0) as i8;
        }
    }
    let d = |s: i32| {
        DELTA[(s & 7) as usize].0 as isize + DELTA[(s & 7) as usize].1 as isize * w as isize
    };
    let mut contours: Vec<Vec<(i32, i32)>> = Vec::new();
    let fetch = |img: &mut [i8], sx: usize, sy: usize, hole: bool| -> Vec<(i32, i32)> {
        let mut pts = Vec::new();
        let i0 = (sy * w + sx) as isize;
        let mut pt = (sx as i32 - 1, sy as i32 - 1);
        let s_end0: i32 = if hole { 0 } else { 4 };
        let mut s = s_end0;
        let mut i1;
        loop {
            s = (s - 1) & 7;
            i1 = i0 + d(s);
            if img[i1 as usize] != 0 || s == s_end0 {
                break;
            }
        }
        if s == s_end0 {
            img[i0 as usize] = RIGHT_NEW;
            pts.push(pt);
            return pts;
        }
        let mut i3 = i0;
        let mut i4;
        let mut prev_s = s ^ 4;
        loop {
            let s_end = s;
            s = s.min(15);
            i4 = i3;
            while s < 15 {
                s += 1;
                i4 = i3 + d(s);
                if img[i4 as usize] != 0 {
                    break;
                }
            }
            s &= 7;
            if ((s - 1) as u32) < (s_end as u32) {
                img[i3 as usize] = RIGHT_NEW;
            } else if img[i3 as usize] == 1 {
                img[i3 as usize] = NEW;
            }
            if s != prev_s {
                pts.push(pt);
            }
            prev_s = s;
            pt.0 += DELTA[s as usize].0;
            pt.1 += DELTA[s as usize].1;
            if i4 == i0 && i3 == i1 {
                break;
            }
            i3 = i4;
            s = (s + 4) & 7;
        }
        pts
    };
    // the raster scan (findNext / findNextX / contourScan)
    let (width, height) = (w - 1, h - 1);
    let (mut x, mut y) = (1usize, 1usize);
    let mut prev: i8 = img[y * w + x - 1];
    while y < height {
        let mut p: i8;
        while x < width {
            p = img[y * w + x];
            while x < width && p == prev {
                x += 1;
                if x < width {
                    p = img[y * w + x];
                }
            }
            if x >= width {
                break;
            }
            let outer = prev == 0 && p == 1;
            let hole = !outer && p == 0 && prev >= 1;
            if outer || hole {
                let sx = x - hole as usize;
                contours.push(fetch(&mut img, sx, y, hole));
                // resume after it, with the (now marked) pixel as prev
                x += 1;
                prev = img[y * w + x - 1];
                continue;
            }
            prev = p;
            x += 1;
        }
        y += 1;
        x = 1;
        prev = 0;
    }
    contours.reverse();
    contours
}

/// A text box: four corners [tl, tr, br, bl] in the image's pixels.
pub type Quad = [[f32; 2]; 4];

/// DBPostProcess + filter_tag_det_res: the boxes in `pred` (the detector's
/// `pw` x `ph` map) for an `src_w` x `src_h` image.
pub fn boxes(
    pred: &[f32],
    pw: usize,
    ph: usize,
    src_w: usize,
    src_h: usize,
    s: &DetSettings,
) -> Vec<Quad> {
    let mut bitmap = Image::<u8>::new(pw, ph, 1);
    for (b, &p) in bitmap.data.iter_mut().zip(pred) {
        *b = (p > s.thresh) as u8;
    }
    let mask = if s.use_dilation {
        dilate(&bitmap, 2, 2)
    } else {
        bitmap
    };
    let mut out = Vec::new();
    for pts in find_contours(&mask).into_iter().take(s.max_candidates) {
        let (b, sside) = geom::mini_box(&pts);
        if sside < 3.0 {
            continue;
        }
        let score = box_score(pred, pw, ph, &b);
        if s.box_thresh > score {
            continue;
        }
        let (area, length) = geom::area_length(&b);
        let expanded = geom::offset_round(&b, area * s.unclip_ratio / length);
        if expanded.is_empty() {
            continue;
        }
        let e: Vec<(i32, i32)> = expanded
            .iter()
            .map(|&(x, y)| (x as i32, y as i32))
            .collect();
        let (b, sside) = geom::mini_box(&e);
        if sside < 5.0 {
            continue;
        }
        // x / width in float32, * dest_width in float64, rounded half to
        // even, clipped, int16
        let sx = |v: f32| {
            ((v / pw as f32) as f64 * src_w as f64)
                .round_ties_even()
                .clamp(0.0, src_w as f64) as i16
        };
        let sy = |v: f32| {
            ((v / ph as f32) as f64 * src_h as f64)
                .round_ties_even()
                .clamp(0.0, src_h as f64) as i16
        };
        let q: Quad = [
            [sx(b[0].x) as f32, sy(b[0].y) as f32],
            [sx(b[1].x) as f32, sy(b[1].y) as f32],
            [sx(b[2].x) as f32, sy(b[2].y) as f32],
            [sx(b[3].x) as f32, sy(b[3].y) as f32],
        ];
        // filter_tag_det_res
        let mut q = geom::order_clockwise(q);
        for p in q.iter_mut() {
            p[0] = (p[0].max(0.0).min(src_w as f32 - 1.0)) as i32 as f32;
            p[1] = (p[1].max(0.0).min(src_h as f32 - 1.0)) as i32 as f32;
        }
        let norm = |a: [f32; 2], b: [f32; 2]| {
            let (dx, dy) = (a[0] - b[0], a[1] - b[1]);
            (dx * dx + dy * dy).sqrt()
        };
        if (norm(q[0], q[1]) as i32) <= 3 || (norm(q[0], q[3]) as i32) <= 3 {
            continue;
        }
        out.push(q);
    }
    out
}

/// RapidOCR's sorted_boxes: by (top-left y, x), then one pass swapping
/// neighbours on about the same line (within 10 px) into x order.
pub fn sort_boxes(mut b: Vec<Quad>) -> Vec<Quad> {
    b.sort_by(|p, q| {
        (p[0][1], p[0][0])
            .partial_cmp(&(q[0][1], q[0][0]))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for i in 0..b.len().saturating_sub(1) {
        if (b[i + 1][0][1] - b[i][0][1]).abs() < 10.0 && b[i + 1][0][0] < b[i][0][0] {
            b.swap(i, i + 1);
        }
    }
    b
}

/// The image resized for the detector.
pub fn prepare(img: &Image<u8>, limit: Limit) -> Option<Image<u8>> {
    let (rh, rw) = input_size(img.height, img.width, limit)?;
    Some(resize_linear_to(img, rw, rh))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_as_rapidocr() {
        // a typical prepared plate crop: short side to 736, multiples of 32
        assert_eq!(input_size(168, 252, Limit::Min(736)), Some((736, 1088)));
        assert_eq!(input_size(168, 252, Limit::Max(960)), Some((160, 256)));
        assert_eq!(input_size(800, 1200, Limit::Min(736)), Some((800, 1216)));
    }
}
