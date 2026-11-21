//! `cv::blur`: the normalised box filter on 8-bit images, with OpenCV's
//! default border (reflect-101: ... 2 1 | 0 1 2 ... n-2 | n-1 n-2 ...).

use crate::Image;

/// Reflect-101 index into 0..n.
fn reflect101(mut i: i64, n: usize) -> usize {
    let n = n as i64;
    if n == 1 {
        return 0;
    }
    loop {
        if i < 0 {
            i = -i;
        } else if i >= n {
            i = 2 * n - 2 - i;
        } else {
            return i as usize;
        }
    }
}

/// `cv::blur(src, (k, k))` for odd `k`: the window sum divided by k*k,
/// rounded to nearest - measured against OpenCV 5.0 to be exact (its old
/// 16-bit reciprocal divide was off by one on about 1 pixel in 9 at k = 15).
/// With odd k a sum can never sit exactly half-way, so no tie rule is needed.
pub fn blur(src: &Image<u8>, k: usize) -> Image<u8> {
    assert_eq!(src.channels, 1, "blur takes a single-channel image");
    assert!(k % 2 == 1, "blur supports odd kernel sizes");
    let (w, h) = (src.width, src.height);
    let anchor = (k / 2) as i64;
    let d = (k * k) as i64;
    // Horizontal sums, reflected at the row ends.
    let mut rowsum = vec![0i64; w * h];
    for y in 0..h {
        let r = src.row(y);
        for x in 0..w {
            let mut s = 0i64;
            for j in 0..k as i64 {
                s += r[reflect101(x as i64 - anchor + j, w)] as i64;
            }
            rowsum[y * w + x] = s;
        }
    }
    let mut out = Image::new(w, h, 1);
    for y in 0..h {
        for x in 0..w {
            let mut s = 0i64;
            for j in 0..k as i64 {
                s += rowsum[reflect101(y as i64 - anchor + j, h) * w + x];
            }
            out.data[y * w + x] = ((2 * s + d) / (2 * d)) as u8;
        }
    }
    out
}
