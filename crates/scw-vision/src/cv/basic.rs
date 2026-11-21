//! Per-pixel functions: threshold, subtract, lookup table, counting, colour.

use crate::Image;

/// OpenCV's `cvRound` for doubles: nearest, ties to even (the FPU's default).
#[inline]
pub(crate) fn round_even_f64(v: f64) -> i64 {
    v.round_ties_even() as i64
}

/// OpenCV's `cvRound` for floats.
#[inline]
pub(crate) fn round_even_f32(v: f32) -> i32 {
    v.round_ties_even() as i32
}

/// `saturate_cast<uchar>` of an int.
#[inline]
pub(crate) fn sat_u8(v: i64) -> u8 {
    v.clamp(0, 255) as u8
}

/// `cv::threshold(src, thresh, maxval, THRESH_BINARY)` on 8-bit samples:
/// a sample above `floor(thresh)` becomes `round(maxval)`, any other 0.
pub fn threshold_binary(src: &Image<u8>, thresh: f64, maxval: f64) -> Image<u8> {
    let ithresh = thresh.floor() as i64;
    let imax = sat_u8(round_even_f64(maxval));
    let data = if ithresh < 0 {
        vec![imax; src.data.len()]
    } else if ithresh >= 255 {
        vec![0; src.data.len()]
    } else {
        let t = ithresh as u8;
        src.data
            .iter()
            .map(|&v| if v > t { imax } else { 0 })
            .collect()
    };
    Image::from_vec(src.width, src.height, src.channels, data)
}

/// `cv::subtract` on 8-bit samples: saturating, so never below 0.
pub fn subtract(a: &Image<u8>, b: &Image<u8>) -> Image<u8> {
    assert_eq!(
        (a.width, a.height, a.channels),
        (b.width, b.height, b.channels),
        "subtract needs images of one shape"
    );
    let data = a
        .data
        .iter()
        .zip(&b.data)
        .map(|(&x, &y)| x.saturating_sub(y))
        .collect();
    Image::from_vec(a.width, a.height, a.channels, data)
}

/// `cv::LUT` with a 256-entry table.
pub fn lut(src: &Image<u8>, table: &[u8; 256]) -> Image<u8> {
    let data = src.data.iter().map(|&v| table[v as usize]).collect();
    Image::from_vec(src.width, src.height, src.channels, data)
}

/// `cv::countNonZero` (single channel).
pub fn count_nonzero(src: &Image<u8>) -> usize {
    src.data.iter().filter(|&&v| v != 0).count()
}

/// `cv::boundingRect` of a mask's non-zero pixels: (x, y, width, height),
/// all zero when there are none.
pub fn bounding_rect(mask: &Image<u8>) -> (usize, usize, usize, usize) {
    assert_eq!(
        mask.channels, 1,
        "bounding_rect takes a single-channel mask"
    );
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    let mut any = false;
    for y in 0..mask.height {
        let row = mask.row(y);
        if let (Some(first), Some(last)) = (
            row.iter().position(|&v| v != 0),
            row.iter().rposition(|&v| v != 0),
        ) {
            any = true;
            x0 = x0.min(first);
            x1 = x1.max(last);
            y0 = y0.min(y);
            y1 = y;
        }
    }
    if any {
        (x0, y0, x1 - x0 + 1, y1 - y0 + 1)
    } else {
        (0, 0, 0, 0)
    }
}

/// `cv::cvtColor(BGR2GRAY)` on 8-bit: OpenCV 5's fixed-point weights
/// (0.114, 0.587, 0.299 in 15 bits - the older 14-bit ones differ on about
/// one pixel in 400), rounded.
pub fn bgr_to_gray(src: &Image<u8>) -> Image<u8> {
    assert_eq!(src.channels, 3, "bgr_to_gray takes a 3-channel image");
    let data = src
        .data
        .as_chunks::<3>()
        .0
        .iter()
        .map(|p| {
            ((p[0] as u32 * 3735 + p[1] as u32 * 19235 + p[2] as u32 * 9798 + (1 << 14)) >> 15)
                as u8
        })
        .collect();
    Image::from_vec(src.width, src.height, 1, data)
}

/// `cv::cvtColor(GRAY2BGR)`: the grey value in all three channels.
pub fn gray_to_bgr(src: &Image<u8>) -> Image<u8> {
    assert_eq!(src.channels, 1, "gray_to_bgr takes a single-channel image");
    let data = src.data.iter().flat_map(|&v| [v, v, v]).collect();
    Image::from_vec(src.width, src.height, 3, data)
}

/// The threshold `cv::threshold(..., THRESH_OTSU)` picks for an 8-bit image:
/// OpenCV's double-precision loop, in its exact order of operations, so ties
/// and near-ties resolve the same way.
pub fn otsu_threshold(src: &Image<u8>) -> f64 {
    assert_eq!(
        src.channels, 1,
        "otsu_threshold takes a single-channel image"
    );
    let mut hist = [0i64; 256];
    for &v in &src.data {
        hist[v as usize] += 1;
    }
    let scale = 1.0 / (src.width * src.height) as f64;
    let mut mu = 0.0f64;
    for (i, &h) in hist.iter().enumerate() {
        mu += i as f64 * h as f64;
    }
    mu *= scale;
    let eps = f32::EPSILON as f64;
    let (mut mu1, mut q1) = (0.0f64, 0.0f64);
    let (mut max_sigma, mut max_val) = (0.0f64, 0.0f64);
    for (i, &h) in hist.iter().enumerate() {
        let p_i = h as f64 * scale;
        mu1 *= q1;
        q1 += p_i;
        let q2 = 1.0 - q1;
        if q1.min(q2) < eps || q1.max(q2) > 1.0 - eps {
            continue;
        }
        mu1 = (mu1 + i as f64 * p_i) / q1;
        let mu2 = (mu - q1 * mu1) / q2;
        let sigma = q1 * q2 * (mu1 - mu2) * (mu1 - mu2);
        if sigma > max_sigma {
            max_sigma = sigma;
            max_val = i as f64;
        }
    }
    max_val
}
