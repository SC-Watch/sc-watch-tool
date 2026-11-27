//! RapidOCR's crop of a tilted box: `cv2.getPerspectiveTransform` then
//! `cv2.warpPerspective(img, M, (w, h), borderMode=BORDER_REPLICATE,
//! flags=INTER_CUBIC)`, ported from OpenCV 5.0: the 8x8 solve (hal::LU64f),
//! the 3x3 inverse (invert, DECOMP_LU), the generic warp's source
//! coordinates (the matrix in float32, each point in double) and the
//! bicubic kernel as its float32 vector path runs on x86 (bicubicVec: fused
//! multiply-adds, A = -0.75, rounded half to even). OpenCV 5.0's x86 builds
//! without half-precision arithmetic take that path (CV_SIMD, not
//! CV_SIMD_FP16); every pixel is computed alone, so the vector width does
//! not change the bytes.

use scw_vision::Image;

/// hal::LU64f on an n x n system with one right-hand side; false when
/// singular.
fn lu_solve(a: &mut [f64], b: &mut [f64], m: usize) -> bool {
    let eps = f64::EPSILON * 100.0;
    for i in 0..m {
        let mut k = i;
        for j in i + 1..m {
            if a[j * m + i].abs() > a[k * m + i].abs() {
                k = j;
            }
        }
        if a[k * m + i].abs() < eps {
            return false;
        }
        if k != i {
            for j in i..m {
                a.swap(i * m + j, k * m + j);
            }
            b.swap(i, k);
        }
        let d = -1.0 / a[i * m + i];
        for j in i + 1..m {
            let alpha = a[j * m + i] * d;
            for k in i + 1..m {
                a[j * m + k] += alpha * a[i * m + k];
            }
            b[j] += alpha * b[i];
        }
    }
    for i in (0..m).rev() {
        let mut s = b[i];
        for k in i + 1..m {
            s -= a[i * m + k] * b[k];
        }
        b[i] = s / a[i * m + i];
    }
    true
}

/// cv::getPerspectiveTransform(src, dst) by the c22 = 1 solve.
pub fn perspective_transform(src: &[[f32; 2]; 4], dst: &[[f32; 2]; 4]) -> Option<[f64; 9]> {
    let mut a = [0f64; 64];
    let mut b = [0f64; 8];
    for i in 0..4 {
        let (sx, sy) = (src[i][0], src[i][1]);
        let (dx, dy) = (dst[i][0], dst[i][1]);
        a[i * 8] = sx as f64;
        a[(i + 4) * 8 + 3] = sx as f64;
        a[i * 8 + 1] = sy as f64;
        a[(i + 4) * 8 + 4] = sy as f64;
        a[i * 8 + 2] = 1.0;
        a[(i + 4) * 8 + 5] = 1.0;
        // float products, stored as double
        a[i * 8 + 6] = (-sx * dx) as f64;
        a[i * 8 + 7] = (-sy * dx) as f64;
        a[(i + 4) * 8 + 6] = (-sx * dy) as f64;
        a[(i + 4) * 8 + 7] = (-sy * dy) as f64;
        b[i] = dx as f64;
        b[i + 4] = dy as f64;
    }
    if !lu_solve(&mut a, &mut b, 8) {
        return None;
    }
    let mut m = [0f64; 9];
    m[..8].copy_from_slice(&b);
    m[8] = 1.0;
    Some(m)
}

/// cv::invert(3x3 double, DECOMP_LU).
fn invert3(s: &[f64; 9]) -> Option<[f64; 9]> {
    let m = |r: usize, c: usize| s[r * 3 + c];
    let det = m(0, 0) * (m(1, 1) * m(2, 2) - m(1, 2) * m(2, 1))
        - m(0, 1) * (m(1, 0) * m(2, 2) - m(1, 2) * m(2, 0))
        + m(0, 2) * (m(1, 0) * m(2, 1) - m(1, 1) * m(2, 0));
    if det == 0.0 {
        return None;
    }
    let d = 1.0 / det;
    Some([
        (m(1, 1) * m(2, 2) - m(1, 2) * m(2, 1)) * d,
        (m(0, 2) * m(2, 1) - m(0, 1) * m(2, 2)) * d,
        (m(0, 1) * m(1, 2) - m(0, 2) * m(1, 1)) * d,
        (m(1, 2) * m(2, 0) - m(1, 0) * m(2, 2)) * d,
        (m(0, 0) * m(2, 2) - m(0, 2) * m(2, 0)) * d,
        (m(0, 2) * m(1, 0) - m(0, 0) * m(1, 2)) * d,
        (m(1, 0) * m(2, 1) - m(1, 1) * m(2, 0)) * d,
        (m(0, 1) * m(2, 0) - m(0, 0) * m(2, 1)) * d,
        (m(0, 0) * m(1, 1) - m(0, 1) * m(1, 0)) * d,
    ])
}

/// bicubicWeights, the vector version.
fn weights(alpha: f32) -> [f32; 4] {
    const A: f32 = -0.75;
    let a2 = alpha * alpha;
    let b = 1.0 - alpha;
    let b2 = b * b;
    let w0 = A * (alpha * b2);
    let w3 = A * (a2 * b);
    let w1 = a2.mul_add((A + 2.0).mul_add(alpha, -(A + 3.0)), 1.0);
    let w2 = ((1.0 - w0) - w1) - w3;
    [w0, w1, w2, w3]
}

/// One output pixel at source (xs, ys), BORDER_REPLICATE.
fn bicubic_px(img: &Image<u8>, xs: f32, ys: f32, out: &mut [u8]) {
    let (w, h, cn) = (img.width as i32, img.height as i32, img.channels);
    let bw = w.max(16) as f32;
    let bh = h.max(16) as f32;
    let vx = xs.max(-bw).min(bw * 2.0);
    let vy = ys.max(-bh).min(bh * 2.0);
    let ix = vx.floor() as i32;
    let iy = vy.floor() as i32;
    let wx = weights(vx - ix as f32);
    let wy = weights(vy - iy as f32);
    let (ix, iy) = (ix - 1, iy - 1);
    let cols = [0, 1, 2, 3].map(|k| (ix + k).clamp(0, w - 1) as usize);
    for c in 0..cn {
        let mut acc = 0f32;
        for (r, &wyr) in wy.iter().enumerate() {
            let row = img.row((iy + r as i32).clamp(0, h - 1) as usize);
            let v = cols.map(|x| row[x * cn + c] as f32);
            let mut s = v[1].mul_add(wx[1], v[0] * wx[0]);
            s = v[2].mul_add(wx[2], s);
            s = v[3].mul_add(wx[3], s);
            acc = s.mul_add(wyr, acc);
        }
        // v_round (half to even), packed to int16 then saturated to u8
        let r = (acc.round_ties_even() as i32).clamp(i16::MIN as i32, i16::MAX as i32);
        out[c] = r.clamp(0, 255) as u8;
    }
}

/// The `cw` x `ch` crop of box `q` (tl, tr, br, bl), as RapidOCR's
/// get_rotate_crop_image warps it.
pub fn perspective_crop(img: &Image<u8>, q: &[[f32; 2]; 4], cw: usize, ch: usize) -> Image<u8> {
    let dst = [
        [0.0, 0.0],
        [cw as f32, 0.0],
        [cw as f32, ch as f32],
        [0.0, ch as f32],
    ];
    let cn = img.channels;
    let mut out = Image::<u8>::new(cw, ch, cn);
    let Some(m) = perspective_transform(q, &dst).and_then(|m| invert3(&m)) else {
        return out;
    };
    let f: [f32; 9] = m.map(|v| v as f32);
    let (m_xx, m_yx, m_zx) = (f[0], f[3], f[6]);
    for y in 0..ch {
        let yf = y as f32;
        let m_x = yf * f[1] + f[2];
        let m_y = yf * f[4] + f[5];
        let m_z = yf * f[7] + f[8];
        let row = out.row_mut(y);
        for x in 0..cw {
            let xf = x as f64;
            let invz = 1.0 / (m_z as f64 + m_zx as f64 * xf);
            let xs = ((m_x as f64 + m_xx as f64 * xf) * invz) as f32;
            let ys = ((m_y as f64 + m_yx as f64 * xf) * invz) as f32;
            bicubic_px(img, xs, ys, &mut row[x * cn..(x + 1) * cn]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_whole_pixel_shift_copies() {
        let mut img = Image::<u8>::new(20, 10, 3);
        for (i, v) in img.data.iter_mut().enumerate() {
            *v = (i * 7 % 251) as u8;
        }
        let q = [[3.0, 2.0], [13.0, 2.0], [13.0, 7.0], [3.0, 7.0]];
        let out = perspective_crop(&img, &q, 10, 5);
        for y in 0..5 {
            assert_eq!(out.row(y), &img.row(y + 2)[9..39]);
        }
    }
}
