//! `cv::resize`: cubic (8-bit and float), linear (8-bit) and area (8-bit),
//! following OpenCV 5.0's arithmetic step by step - source positions in
//! double then float, coefficients in float, 8-bit work in fixed point with
//! 11 fractional bits - so the bytes come out the same.

use super::basic::{round_even_f32, round_even_f64, sat_u8};
use crate::Image;

const COEF_BITS: u32 = 11;
const COEF_SCALE: f32 = (1 << COEF_BITS) as f32;

// OpenCV combines the rows of a resize with 128-bit vector code over whole
// blocks of a row and plain code over the rest, and the two round
// differently. Matching OpenCV byte for byte means doing the same: the block
// sizes below are the lanes of one 128-bit register (the build's baseline),
// measured against OpenCV 5.0 on the fixtures. (8-bit linear has one rule
// for blocks and rest alike: see resize_linear_u8.)
const CUBIC_U8_LANES: usize = 8; // VResizeCubicVec_32s8u: 8 i16 out
const CUBIC_F32_LANES: usize = 4; // VResizeCubicVec_32f: 4 f32 out

/// Where a row's vector blocks end: OpenCV's `x <= width - lanes` loop.
#[inline]
fn vector_end(width: usize, lanes: usize) -> usize {
    width / lanes * lanes
}

/// `v_mul_hi` on int16 lanes: the high half of the 32-bit product.
#[inline]
fn mul_hi_i16(a: i32, b: i32) -> i32 {
    (a * b) >> 16
}

#[inline]
fn sat_i16(v: i64) -> i32 {
    v.clamp(i16::MIN as i64, i16::MAX as i64) as i32
}

/// Output size OpenCV derives from scale factors: `saturate_cast<int>(size * f)`.
pub fn scaled_size(w: usize, h: usize, fx: f64, fy: f64) -> (usize, usize) {
    (
        round_even_f64(w as f64 * fx).max(0) as usize,
        round_even_f64(h as f64 * fy).max(0) as usize,
    )
}

fn interpolate_cubic(x: f32) -> [f32; 4] {
    const A: f32 = -0.75;
    let c0 = ((A * (x + 1.0) - 5.0 * A) * (x + 1.0) + 8.0 * A) * (x + 1.0) - 4.0 * A;
    let c1 = ((A + 2.0) * x - (A + 3.0)) * x * x + 1.0;
    let c2 = ((A + 2.0) * (1.0 - x) - (A + 3.0)) * (1.0 - x) * (1.0 - x) + 1.0;
    let c3 = 1.0 - c0 - c1 - c2;
    [c0, c1, c2, c3]
}

/// `saturate_cast<short>(c * INTER_RESIZE_COEF_SCALE)`.
#[inline]
fn fixed(c: f32) -> i32 {
    round_even_f32(c * COEF_SCALE).clamp(i16::MIN as i32, i16::MAX as i32)
}

/// Source position for output index `d` (not area mode): (floor, fraction).
#[inline]
fn src_pos(d: usize, scale: f64) -> (i32, f32) {
    let f = ((d as f64 + 0.5) * scale - 0.5) as f32;
    let s = f.floor() as i32;
    (s, f - s as f32)
}

/// Source position in OpenCV's area mode (INTER_AREA when enlarging).
#[inline]
fn src_pos_area(d: usize, scale: f64, inv_scale: f64) -> (i32, f32) {
    let s = (d as f64 * scale).floor() as i32;
    let f = ((d as f64 + 1.0) - (s as f64 + 1.0) * inv_scale) as f32;
    let f = if f <= 0.0 { 0.0 } else { f - f.floor() };
    (s, f)
}

#[inline]
fn clamp_idx(i: i32, n: usize) -> usize {
    i.clamp(0, n as i32 - 1) as usize
}

// ---- cubic ----------------------------------------------------------------------

/// Cubic resize of an 8-bit image by scale factors, as
/// `cv::resize(src, None, fx, fy, INTER_CUBIC)`.
pub fn resize_cubic_by(src: &Image<u8>, fx: f64, fy: f64) -> Image<u8> {
    let (dw, dh) = scaled_size(src.width, src.height, fx, fy);
    resize_cubic_u8(src, dw, dh, fx, fy)
}

/// Cubic resize of an 8-bit image to a size, as `cv::resize(src, (dw, dh), INTER_CUBIC)`.
pub fn resize_cubic_to(src: &Image<u8>, dw: usize, dh: usize) -> Image<u8> {
    let fx = dw as f64 / src.width as f64;
    let fy = dh as f64 / src.height as f64;
    resize_cubic_u8(src, dw, dh, fx, fy)
}

/// Per output column: the four source samples' offsets in a row (edges
/// clamped, times the channel count) and the cubic weights.
fn cubic_xtab(dw: usize, sw: usize, cn: usize, scale_x: f64) -> Vec<([usize; 4], [f32; 4])> {
    (0..dw)
        .map(|dx| {
            let (sx, f) = src_pos(dx, scale_x);
            let idx = [0, 1, 2, 3].map(|j| clamp_idx(sx - 1 + j, sw) * cn);
            (idx, interpolate_cubic(f))
        })
        .collect()
}

fn resize_cubic_u8(src: &Image<u8>, dw: usize, dh: usize, inv_x: f64, inv_y: f64) -> Image<u8> {
    if (dw, dh) == (src.width, src.height) {
        return src.clone();
    }
    let (sw, sh, cn) = (src.width, src.height, src.channels);
    let (scale_x, scale_y) = (1.0 / inv_x, 1.0 / inv_y);
    let xtab: Vec<([usize; 4], [i32; 4])> = cubic_xtab(dw, sw, cn, scale_x)
        .into_iter()
        .map(|(idx, a)| (idx, a.map(fixed)))
        .collect();
    // Each source row's horizontal pass, once (OpenCV keeps them in a ring of
    // four rows; integer sums, so when they are made does not matter).
    // |sum| < 4 * 255 * 2^11 * 1.25, well inside an i32.
    let width = dw * cn;
    let mut hrows = vec![0i32; sh * width];
    for sy in 0..sh {
        let s = src.row(sy);
        let buf = &mut hrows[sy * width..(sy + 1) * width];
        for (dx, &(idx, a)) in xtab.iter().enumerate() {
            for c in 0..cn {
                buf[dx * cn + c] = s[idx[0] + c] as i32 * a[0]
                    + s[idx[1] + c] as i32 * a[1]
                    + s[idx[2] + c] as i32 * a[2]
                    + s[idx[3] + c] as i32 * a[3];
            }
        }
    }
    let mut out = Image::new(dw, dh, cn);
    // Vector blocks: float, with the weights pre-scaled by 1/2048^2, summed
    // from the last row up, rounded to nearest even.
    let scale = 1.0f32 / ((1i32 << COEF_BITS) * (1i32 << COEF_BITS)) as f32;
    let vend = vector_end(width, CUBIC_U8_LANES);
    for dy in 0..dh {
        let (sy, f) = src_pos(dy, scale_y);
        let beta = interpolate_cubic(f).map(fixed);
        let bf = beta.map(|b| b as f32 * scale);
        let r = [0, 1, 2, 3].map(|k| {
            let y = clamp_idx(sy - 1 + k, sh);
            &hrows[y * width..(y + 1) * width]
        });
        let d = out.row_mut(dy);
        for (i, px) in d.iter_mut().enumerate().take(vend) {
            let mut t = r[3][i] as f32 * bf[3];
            // (t += x is x + t: one IEEE addition, the same bits.)
            t += r[2][i] as f32 * bf[2];
            t += r[1][i] as f32 * bf[1];
            t += r[0][i] as f32 * bf[0];
            *px = sat_u8(round_even_f32(t) as i64);
        }
        for (i, px) in d.iter_mut().enumerate().skip(vend) {
            let v = r[0][i] as i64 * beta[0] as i64
                + r[1][i] as i64 * beta[1] as i64
                + r[2][i] as i64 * beta[2] as i64
                + r[3][i] as i64 * beta[3] as i64;
            *px = sat_u8((v + (1 << 21)) >> 22);
        }
    }
    out
}

/// Cubic resize of a float image by scale factors, as
/// `cv::resize(src, None, fx, fy, INTER_CUBIC)` on 32-bit floats.
pub fn resize_cubic_f32_by(src: &Image<f32>, fx: f64, fy: f64) -> Image<f32> {
    let (dw, dh) = scaled_size(src.width, src.height, fx, fy);
    if (dw, dh) == (src.width, src.height) {
        return src.clone();
    }
    let (sw, sh, cn) = (src.width, src.height, src.channels);
    let (scale_x, scale_y) = (1.0 / fx, 1.0 / fy);
    let xtab = cubic_xtab(dw, sw, cn, scale_x);
    let width = dw * cn;
    let mut hrows = vec![0f32; sh * width];
    for sy in 0..sh {
        let s = src.row(sy);
        let buf = &mut hrows[sy * width..(sy + 1) * width];
        for (dx, &(idx, a)) in xtab.iter().enumerate() {
            for c in 0..cn {
                buf[dx * cn + c] = s[idx[0] + c] * a[0]
                    + s[idx[1] + c] * a[1]
                    + s[idx[2] + c] * a[2]
                    + s[idx[3] + c] * a[3];
            }
        }
    }
    let mut out = Image::new(dw, dh, cn);
    let vend = vector_end(width, CUBIC_F32_LANES);
    for dy in 0..dh {
        let (sy, f) = src_pos(dy, scale_y);
        let beta = interpolate_cubic(f);
        let r = [0, 1, 2, 3].map(|k| {
            let y = clamp_idx(sy - 1 + k, sh);
            &hrows[y * width..(y + 1) * width]
        });
        let d = out.row_mut(dy);
        for (i, px) in d.iter_mut().enumerate() {
            *px = if i < vend {
                // Vector blocks add from the last row up.
                let mut t = r[3][i] * beta[3];
                t += r[2][i] * beta[2];
                t += r[1][i] * beta[1];
                r[0][i] * beta[0] + t
            } else {
                r[0][i] * beta[0] + r[1][i] * beta[1] + r[2][i] * beta[2] + r[3][i] * beta[3]
            };
        }
    }
    out
}

// ---- linear ---------------------------------------------------------------------

/// Bilinear resize of an 8-bit image, as `cv::resize(src, (dw, dh), INTER_LINEAR)`.
pub fn resize_linear_to(src: &Image<u8>, dw: usize, dh: usize) -> Image<u8> {
    let fx = dw as f64 / src.width as f64;
    let fy = dh as f64 / src.height as f64;
    resize_linear_u8(src, dw, dh, fx, fy, false)
}

fn resize_linear_u8(
    src: &Image<u8>,
    dw: usize,
    dh: usize,
    inv_x: f64,
    inv_y: f64,
    area_mode: bool,
) -> Image<u8> {
    if (dw, dh) == (src.width, src.height) {
        return src.clone();
    }
    let (sw, sh, cn) = (src.width, src.height, src.channels);
    let (scale_x, scale_y) = (1.0 / inv_x, 1.0 / inv_y);
    const ONE: i64 = 1 << COEF_BITS;
    // Per output column: source column, two weights, and whether the second
    // tap exists (OpenCV's xmax: past it only the first sample, times ONE).
    let xtab: Vec<(usize, [i64; 2], bool)> = (0..dw)
        .map(|dx| {
            let (mut sx, mut f) = if area_mode {
                src_pos_area(dx, scale_x, inv_x)
            } else {
                src_pos(dx, scale_x)
            };
            if sx < 0 {
                sx = 0;
                f = 0.0;
            }
            let both = sx + 1 < sw as i32;
            if sx >= sw as i32 - 1 {
                sx = sw as i32 - 1;
                f = 0.0;
            }
            (sx as usize, [fixed(1.0 - f) as i64, fixed(f) as i64], both)
        })
        .collect();
    let xmax = xtab.iter().position(|t| !t.2).unwrap_or(dw);
    // Each source row's horizontal pass, once (|value| <= 255 * 2^11).
    let width = dw * cn;
    let mut hrows = vec![0i32; sh * width];
    for sy in 0..sh {
        let s = src.row(sy);
        let buf = &mut hrows[sy * width..(sy + 1) * width];
        for (dx, &(sx, a, _)) in xtab.iter().enumerate() {
            for c in 0..cn {
                buf[dx * cn + c] = if dx < xmax {
                    (s[sx * cn + c] as i64 * a[0] + s[(sx + 1) * cn + c] as i64 * a[1]) as i32
                } else {
                    (s[sx * cn + c] as i64 * ONE) as i32
                };
            }
        }
    }
    let mut out = Image::new(dw, dh, cn);
    for dy in 0..dh {
        let (sy, f) = if area_mode {
            src_pos_area(dy, scale_y, inv_y)
        } else {
            src_pos(dy, scale_y)
        };
        let beta = [fixed(1.0 - f), fixed(f)];
        let r = [0, 1].map(|k| {
            let y = clamp_idx(sy + k, sh);
            &hrows[y * width..(y + 1) * width]
        });
        // OpenCV's 8-bit linear specialisation, vector blocks and the rest
        // alike: each row sum shifted down 4 bits into int16, multiplied
        // keeping the high half, then (sum + 2) >> 2.
        for ((px, &h0), &h1) in out.row_mut(dy).iter_mut().zip(r[0]).zip(r[1]) {
            let a = mul_hi_i16(sat_i16((h0 >> 4) as i64), beta[0]);
            let b = mul_hi_i16(sat_i16((h1 >> 4) as i64), beta[1]);
            *px = sat_u8(((a + b + 2) >> 2) as i64);
        }
    }
    out
}

// ---- area -----------------------------------------------------------------------

/// Area resize of an 8-bit image, as `cv::resize(src, (dw, dh), INTER_AREA)`:
/// a true average when shrinking both ways; OpenCV's area-mode bilinear
/// otherwise.
pub fn resize_area_to(src: &Image<u8>, dw: usize, dh: usize) -> Image<u8> {
    if (dw, dh) == (src.width, src.height) {
        return src.clone();
    }
    let inv_x = dw as f64 / src.width as f64;
    let inv_y = dh as f64 / src.height as f64;
    let (scale_x, scale_y) = (1.0 / inv_x, 1.0 / inv_y);
    let ix = round_even_f64(scale_x);
    let iy = round_even_f64(scale_y);
    let fast =
        (scale_x - ix as f64).abs() < f64::EPSILON && (scale_y - iy as f64).abs() < f64::EPSILON;
    if scale_x >= 1.0 && scale_y >= 1.0 {
        if fast {
            return area_fast(src, dw, dh, ix as usize, iy as usize);
        }
        return area_general(src, dw, dh, scale_x, scale_y);
    }
    resize_linear_u8(src, dw, dh, inv_x, inv_y, true)
}

fn area_fast(src: &Image<u8>, dw: usize, dh: usize, ix: usize, iy: usize) -> Image<u8> {
    let cn = src.channels;
    let mut out = Image::new(dw, dh, cn);
    let area = (ix * iy) as i64;
    let scale = 1.0f32 / area as f32;
    for dy in 0..dh {
        for dx in 0..dw {
            for c in 0..cn {
                let mut sum = 0i64;
                for yy in 0..iy {
                    let r = src.row(dy * iy + yy);
                    for xx in 0..ix {
                        sum += r[(dx * ix + xx) * cn + c] as i64;
                    }
                }
                let v = if ix == 2 && iy == 2 && matches!(cn, 1 | 3 | 4) {
                    (sum + 2) >> 2
                } else {
                    round_even_f32(sum as f32 * scale) as i64
                };
                out.set(dx, dy, c, sat_u8(v));
            }
        }
    }
    out
}

/// One source span's share of an output cell: (output index, source index, weight).
fn area_tab(ssize: usize, dsize: usize, scale: f64) -> Vec<(usize, usize, f32)> {
    let mut tab = Vec::with_capacity(ssize * 2);
    for dx in 0..dsize {
        let fsx1 = dx as f64 * scale;
        let fsx2 = fsx1 + scale;
        let cell = scale.min(ssize as f64 - fsx1);
        let mut sx1 = fsx1.ceil() as i64;
        let mut sx2 = fsx2.floor() as i64;
        sx2 = sx2.min(ssize as i64 - 1);
        sx1 = sx1.min(sx2);
        if sx1 as f64 - fsx1 > 1e-3 {
            tab.push((dx, (sx1 - 1) as usize, ((sx1 as f64 - fsx1) / cell) as f32));
        }
        for sx in sx1..sx2 {
            tab.push((dx, sx as usize, (1.0 / cell) as f32));
        }
        if fsx2 - sx2 as f64 > 1e-3 {
            tab.push((
                dx,
                sx2 as usize,
                ((fsx2 - sx2 as f64).min(1.0).min(cell) / cell) as f32,
            ));
        }
    }
    tab
}

fn area_general(src: &Image<u8>, dw: usize, dh: usize, scale_x: f64, scale_y: f64) -> Image<u8> {
    let cn = src.channels;
    let xtab = area_tab(src.width, dw, scale_x);
    let ytab = area_tab(src.height, dh, scale_y);
    let width = dw * cn;
    let mut out = Image::new(dw, dh, cn);
    let mut buf = vec![0f32; width];
    let mut sum = vec![0f32; width];
    let mut prev_dy: Option<usize> = None;
    let store = |out: &mut Image<u8>, dy: usize, sum: &[f32]| {
        for (o, &s) in out.row_mut(dy).iter_mut().zip(sum) {
            *o = sat_u8(round_even_f32(s) as i64);
        }
    };
    for &(dy, sy, beta) in &ytab {
        buf.iter_mut().for_each(|b| *b = 0.0);
        let s = src.row(sy);
        for &(dx, sx, alpha) in &xtab {
            for c in 0..cn {
                buf[dx * cn + c] += s[sx * cn + c] as f32 * alpha;
            }
        }
        match prev_dy {
            Some(p) if p == dy => {
                for (acc, &b) in sum.iter_mut().zip(&buf) {
                    *acc += beta * b;
                }
            }
            _ => {
                if let Some(p) = prev_dy {
                    store(&mut out, p, &sum);
                }
                for (acc, &b) in sum.iter_mut().zip(&buf) {
                    *acc = beta * b;
                }
                prev_dy = Some(dy);
            }
        }
    }
    if let Some(p) = prev_dy {
        store(&mut out, p, &sum);
    }
    out
}

// ---- float area (glyph features) ------------------------------------------------

/// Area resize of a float image, as `cv::resize(src, (dw, dh), INTER_AREA)`
/// on 32-bit floats - the glyph features' 16 x 24 letter image. OpenCV keeps
/// this one to its own code (IPP's "super sampling" is not exact, so it is
/// off by default), and the order of every sum below is OpenCV's.
pub fn resize_area_f32_to(src: &Image<f32>, dw: usize, dh: usize) -> Image<f32> {
    if (dw, dh) == (src.width, src.height) {
        return src.clone();
    }
    let inv_x = dw as f64 / src.width as f64;
    let inv_y = dh as f64 / src.height as f64;
    let (scale_x, scale_y) = (1.0 / inv_x, 1.0 / inv_y);
    let ix = round_even_f64(scale_x);
    let iy = round_even_f64(scale_y);
    let fast =
        (scale_x - ix as f64).abs() < f64::EPSILON && (scale_y - iy as f64).abs() < f64::EPSILON;
    if scale_x >= 1.0 && scale_y >= 1.0 {
        if fast {
            return area_fast_f32(src, dw, dh, ix as usize, iy as usize);
        }
        return area_general_f32(src, dw, dh, scale_x, scale_y);
    }
    resize_linear_f32(src, dw, dh, inv_x, inv_y, true)
}

/// `resizeAreaFast_` for floats: the 2 x 2 single-channel case in 4-lane
/// vector blocks, `((r00 + r01) + (r10 + r11)) * 0.25`; everything else the
/// plain loop, its sum unrolled by four as OpenCV builds it.
fn area_fast_f32(src: &Image<f32>, dw: usize, dh: usize, ix: usize, iy: usize) -> Image<f32> {
    let cn = src.channels;
    let mut out = Image::new(dw, dh, cn);
    let area = ix * iy;
    let scale = 1.0f32 / area as f32;
    // OpenCV's ofs table: row by row, then column by column within the cell.
    let ofs: Vec<(usize, usize)> = (0..iy)
        .flat_map(|sy| (0..ix).map(move |sx| (sy, sx * cn)))
        .collect();
    let vector = ix == 2 && iy == 2 && (cn == 1 || cn == 4);
    let width = dw * cn;
    for dy in 0..dh {
        let rows: Vec<&[f32]> = (0..iy).map(|k| src.row(dy * iy + k)).collect();
        let d = out.row_mut(dy);
        let mut x = 0;
        if vector && cn == 1 {
            let end = vector_end(dw, CUBIC_F32_LANES);
            while x < end {
                let (r0, r1) = (rows[0], rows[1]);
                d[x] = ((r0[2 * x] + r0[2 * x + 1]) + (r1[2 * x] + r1[2 * x + 1])) * 0.25;
                x += 1;
            }
        }
        while x < width {
            let (dx, c) = (x / cn, x % cn);
            let base = dx * ix * cn + c;
            let at = |k: usize| rows[ofs[k].0][base + ofs[k].1];
            let mut sum = 0.0f32;
            let mut k = 0;
            while k + 4 <= area {
                sum += at(k) + at(k + 1) + at(k + 2) + at(k + 3);
                k += 4;
            }
            while k < area {
                sum += at(k);
                k += 1;
            }
            d[x] = sum * scale;
            x += 1;
        }
    }
    out
}

fn area_general_f32(
    src: &Image<f32>,
    dw: usize,
    dh: usize,
    scale_x: f64,
    scale_y: f64,
) -> Image<f32> {
    let cn = src.channels;
    let xtab = area_tab(src.width, dw, scale_x);
    let ytab = area_tab(src.height, dh, scale_y);
    let width = dw * cn;
    let mut out = Image::new(dw, dh, cn);
    let mut buf = vec![0f32; width];
    let mut sum = vec![0f32; width];
    let mut prev_dy: Option<usize> = None;
    for &(dy, sy, beta) in &ytab {
        buf.iter_mut().for_each(|b| *b = 0.0);
        let s = src.row(sy);
        for &(dx, sx, alpha) in &xtab {
            for c in 0..cn {
                buf[dx * cn + c] += s[sx * cn + c] * alpha;
            }
        }
        match prev_dy {
            Some(p) if p == dy => {
                for (acc, &b) in sum.iter_mut().zip(&buf) {
                    *acc += beta * b;
                }
            }
            _ => {
                if let Some(p) = prev_dy {
                    out.row_mut(p).copy_from_slice(&sum);
                }
                for (acc, &b) in sum.iter_mut().zip(&buf) {
                    *acc = beta * b;
                }
                prev_dy = Some(dy);
            }
        }
    }
    if let Some(p) = prev_dy {
        out.row_mut(p).copy_from_slice(&sum);
    }
    out
}

/// Bilinear resize of a float image, as `cv::resize(src, (dw, dh),
/// INTER_LINEAR)` on 32-bit floats (a same-size resize is a copy, as in
/// OpenCV). IRIS's mining reader fits each digit cell to 14 x 20 with it.
pub fn resize_linear_f32_to(src: &Image<f32>, dw: usize, dh: usize) -> Image<f32> {
    if (dw, dh) == (src.width, src.height) {
        return src.clone();
    }
    let inv_x = dw as f64 / src.width as f64;
    let inv_y = dh as f64 / src.height as f64;
    // cv::resize: "INTER_AREA (fast) is equal to INTER_LINEAR" when halving
    // both ways exactly, and OpenCV takes the area path then.
    let (scale_x, scale_y) = (1.0 / inv_x, 1.0 / inv_y);
    let (ix, iy) = (round_even_f64(scale_x), round_even_f64(scale_y));
    let fast =
        (scale_x - ix as f64).abs() < f64::EPSILON && (scale_y - iy as f64).abs() < f64::EPSILON;
    if fast && ix == 2 && iy == 2 {
        return resize_area_f32_to(src, dw, dh);
    }
    resize_linear_f32(src, dw, dh, inv_x, inv_y, false)
}

/// OpenCV's float bilinear (`HResizeLinear` / `VResizeLinear` on floats):
/// float weights, each tap multiplied and added separately.
fn resize_linear_f32(
    src: &Image<f32>,
    dw: usize,
    dh: usize,
    inv_x: f64,
    inv_y: f64,
    area_mode: bool,
) -> Image<f32> {
    let (sw, sh, cn) = (src.width, src.height, src.channels);
    let (scale_x, scale_y) = (1.0 / inv_x, 1.0 / inv_y);
    let xtab: Vec<(usize, [f32; 2], bool)> = (0..dw)
        .map(|dx| {
            let (mut sx, mut f) = if area_mode {
                src_pos_area(dx, scale_x, inv_x)
            } else {
                src_pos(dx, scale_x)
            };
            if sx < 0 {
                sx = 0;
                f = 0.0;
            }
            let both = sx + 1 < sw as i32;
            if sx >= sw as i32 - 1 {
                sx = sw as i32 - 1;
                f = 0.0;
            }
            (sx as usize, [1.0 - f, f], both)
        })
        .collect();
    let xmax = xtab.iter().position(|t| !t.2).unwrap_or(dw);
    let mut out = Image::new(dw, dh, cn);
    let mut rows: Vec<Vec<f32>> = vec![vec![0.0; dw * cn]; 2];
    for dy in 0..dh {
        let (sy, f) = if area_mode {
            src_pos_area(dy, scale_y, inv_y)
        } else {
            src_pos(dy, scale_y)
        };
        let (b0, b1) = (1.0 - f, f);
        for (k, buf) in rows.iter_mut().enumerate() {
            let s = src.row(clamp_idx(sy + k as i32, sh));
            for (dx, &(sx, a, _)) in xtab.iter().enumerate() {
                for c in 0..cn {
                    buf[dx * cn + c] = if dx < xmax {
                        s[sx * cn + c] * a[0] + s[(sx + 1) * cn + c] * a[1]
                    } else {
                        s[sx * cn + c]
                    };
                }
            }
        }
        for (i, px) in out.row_mut(dy).iter_mut().enumerate() {
            *px = rows[0][i] * b0 + rows[1][i] * b1;
        }
    }
    out
}
