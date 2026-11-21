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

fn resize_cubic_u8(src: &Image<u8>, dw: usize, dh: usize, inv_x: f64, inv_y: f64) -> Image<u8> {
    if (dw, dh) == (src.width, src.height) {
        return src.clone();
    }
    let (sw, sh, cn) = (src.width, src.height, src.channels);
    let (scale_x, scale_y) = (1.0 / inv_x, 1.0 / inv_y);
    let xtab: Vec<(i32, [i32; 4])> = (0..dw)
        .map(|dx| {
            let (sx, f) = src_pos(dx, scale_x);
            (sx, interpolate_cubic(f).map(fixed))
        })
        .collect();
    let mut out = Image::new(dw, dh, cn);
    let mut rows: Vec<Vec<i64>> = vec![vec![0; dw * cn]; 4];
    for dy in 0..dh {
        let (sy, f) = src_pos(dy, scale_y);
        let beta = interpolate_cubic(f).map(fixed);
        for (k, buf) in rows.iter_mut().enumerate() {
            let s = src.row(clamp_idx(sy - 1 + k as i32, sh));
            for (dx, &(sx, a)) in xtab.iter().enumerate() {
                for c in 0..cn {
                    let mut v = 0i64;
                    for (j, &aj) in a.iter().enumerate() {
                        v += s[clamp_idx(sx - 1 + j as i32, sw) * cn + c] as i64 * aj as i64;
                    }
                    buf[dx * cn + c] = v;
                }
            }
        }
        let d = out.row_mut(dy);
        let vend = vector_end(d.len(), CUBIC_U8_LANES);
        // Vector blocks: float, with the weights pre-scaled by 1/2048^2,
        // summed from the last row up, rounded to nearest even.
        let scale = 1.0f32 / ((1i32 << COEF_BITS) * (1i32 << COEF_BITS)) as f32;
        let bf = beta.map(|b| b as f32 * scale);
        for (i, px) in d.iter_mut().enumerate() {
            *px = if i < vend {
                let mut t = rows[3][i] as f32 * bf[3];
                // (t += x is x + t: one IEEE addition, the same bits.)
                t += rows[2][i] as f32 * bf[2];
                t += rows[1][i] as f32 * bf[1];
                t += rows[0][i] as f32 * bf[0];
                sat_u8(round_even_f32(t) as i64)
            } else {
                let v = rows[0][i] * beta[0] as i64
                    + rows[1][i] * beta[1] as i64
                    + rows[2][i] * beta[2] as i64
                    + rows[3][i] * beta[3] as i64;
                sat_u8((v + (1 << 21)) >> 22)
            };
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
    let xtab: Vec<(i32, [f32; 4])> = (0..dw)
        .map(|dx| {
            let (sx, f) = src_pos(dx, scale_x);
            (sx, interpolate_cubic(f))
        })
        .collect();
    let mut out = Image::new(dw, dh, cn);
    let mut rows: Vec<Vec<f32>> = vec![vec![0.0; dw * cn]; 4];
    for dy in 0..dh {
        let (sy, f) = src_pos(dy, scale_y);
        let beta = interpolate_cubic(f);
        for (k, buf) in rows.iter_mut().enumerate() {
            let s = src.row(clamp_idx(sy - 1 + k as i32, sh));
            for (dx, &(sx, a)) in xtab.iter().enumerate() {
                for c in 0..cn {
                    let p = |j: i32| s[clamp_idx(sx - 1 + j, sw) * cn + c];
                    buf[dx * cn + c] = p(0) * a[0] + p(1) * a[1] + p(2) * a[2] + p(3) * a[3];
                }
            }
        }
        let d = out.row_mut(dy);
        let vend = vector_end(d.len(), CUBIC_F32_LANES);
        for (i, px) in d.iter_mut().enumerate() {
            *px = if i < vend {
                // Vector blocks add from the last row up.
                let mut t = rows[3][i] * beta[3];
                t += rows[2][i] * beta[2];
                t += rows[1][i] * beta[1];
                rows[0][i] * beta[0] + t
            } else {
                rows[0][i] * beta[0]
                    + rows[1][i] * beta[1]
                    + rows[2][i] * beta[2]
                    + rows[3][i] * beta[3]
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
    let mut out = Image::new(dw, dh, cn);
    let mut rows: Vec<Vec<i64>> = vec![vec![0; dw * cn]; 2];
    for dy in 0..dh {
        let (sy, f) = if area_mode {
            src_pos_area(dy, scale_y, inv_y)
        } else {
            src_pos(dy, scale_y)
        };
        let beta = [fixed(1.0 - f) as i64, fixed(f) as i64];
        for (k, buf) in rows.iter_mut().enumerate() {
            let s = src.row(clamp_idx(sy + k as i32, sh));
            for (dx, &(sx, a, _)) in xtab.iter().enumerate() {
                for c in 0..cn {
                    buf[dx * cn + c] = if dx < xmax {
                        s[sx * cn + c] as i64 * a[0] + s[(sx + 1) * cn + c] as i64 * a[1]
                    } else {
                        s[sx * cn + c] as i64 * ONE
                    };
                }
            }
        }
        // OpenCV's 8-bit linear specialisation, vector blocks and the rest
        // alike: each row sum shifted down 4 bits into int16, multiplied
        // keeping the high half, then (sum + 2) >> 2.
        for (i, px) in out.row_mut(dy).iter_mut().enumerate() {
            let a = mul_hi_i16(sat_i16(rows[0][i] >> 4), beta[0] as i32);
            let b = mul_hi_i16(sat_i16(rows[1][i] >> 4), beta[1] as i32);
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
