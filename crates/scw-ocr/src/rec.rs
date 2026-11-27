//! RapidOCR 1.2.3's TextRecognizer around PP-OCRv3's recogniser: crops in
//! batches of six by aspect ratio, each resized to 48 px high and padded to
//! the batch's widest (resize_norm_img), run, and read back by CTC
//! (CTCLabelDecode: best class per step, repeats and blanks dropped, the
//! score numpy's mean of the steps kept plus a 1e-50).

use scw_vision::Image;
use scw_vision::cv::resize_linear_to;

pub const HEIGHT: usize = 48;
pub const BATCH: usize = 6;

/// resize_norm_img for a batch whose widest is `max_wh_ratio`: the
/// normalised 3 x 48 x width tensor data and the width.
pub fn resize_norm(img: &Image<u8>, max_wh_ratio: f64) -> (Vec<f32>, usize) {
    let width = (HEIGHT as f64 * max_wh_ratio) as usize;
    let ratio = img.width as f64 / img.height as f64;
    let want = (HEIGHT as f64 * ratio).ceil();
    let rw = if want > width as f64 {
        width
    } else {
        want as usize
    };
    let mut out = vec![0f32; 3 * HEIGHT * width];
    if rw == 0 || width == 0 {
        return (out, width);
    }
    let r = resize_linear_to(img, rw, HEIGHT);
    for y in 0..HEIGHT {
        let row = r.row(y);
        for x in 0..rw {
            for c in 0..3 {
                let v = row[x * 3 + c] as f32 / 255.0f32;
                out[c * HEIGHT * width + y * width + x] = (v - 0.5f32) / 0.5f32;
            }
        }
    }
    (out, width)
}

/// numpy's sum of a float64 array (pairwise, eight accumulators per block).
fn np_sum(a: &[f64]) -> f64 {
    let n = a.len();
    if n < 8 {
        let mut r = 0f64;
        for &v in a {
            r += v;
        }
        r
    } else if n <= 128 {
        let mut r = [0f64; 8];
        r.copy_from_slice(&a[..8]);
        let mut i = 8;
        while i < n - (n % 8) {
            for j in 0..8 {
                r[j] += a[i + j];
            }
            i += 8;
        }
        let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        while i < n {
            res += a[i];
            i += 1;
        }
        res
    } else {
        let mut n2 = n / 2;
        n2 -= n2 % 8;
        np_sum(&a[..n2]) + np_sum(&a[n2..])
    }
}

/// CTC decoding of one crop's `steps` x `classes` probabilities: the text
/// and RapidOCR's score. `chars[i]` is class i's text (class 0 the blank).
pub fn decode(probs: &[f32], classes: usize, chars: &[String]) -> (String, f64) {
    decode_only(probs, classes, chars, None)
}

/// [`decode`], answering only with the classes `keep` allows: the others'
/// probabilities count as 0.
pub fn decode_only(
    probs: &[f32],
    classes: usize,
    chars: &[String],
    keep: Option<&[bool]>,
) -> (String, f64) {
    let allowed = |i: usize, v: f32| match keep {
        Some(k) if !k[i] => 0.0,
        _ => v,
    };
    let steps = probs.len() / classes;
    let mut text = String::new();
    let mut conf: Vec<f64> = Vec::new();
    let mut prev = usize::MAX;
    for t in 0..steps {
        let p = &probs[t * classes..(t + 1) * classes];
        // argmax: the first of equal maxima
        let (mut best, mut bv) = (0usize, allowed(0, p[0]));
        for (i, &v) in p.iter().enumerate().skip(1) {
            let v = allowed(i, v);
            if v > bv {
                best = i;
                bv = v;
            }
        }
        let repeat = t > 0 && prev == best;
        prev = best;
        if best == 0 || repeat {
            continue;
        }
        text.push_str(&chars[best]);
        conf.push(bv as f64);
    }
    conf.push(1e-50);
    let score = np_sum(&conf) / conf.len() as f64;
    (text, score)
}

/// The order crops are read in: by width / height, ties in input order.
pub fn order(crops: &[Image<u8>]) -> Vec<usize> {
    let ratio: Vec<f64> = crops
        .iter()
        .map(|c| c.width as f64 / c.height as f64)
        .collect();
    let mut idx: Vec<usize> = (0..crops.len()).collect();
    idx.sort_by(|&a, &b| {
        ratio[a]
            .partial_cmp(&ratio[b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_score_is_numpys() {
        // numpy: np.mean([np.float32(0.9), np.float32(0.8)] + [1e-50])
        let chars: Vec<String> = ["blank", "a", "b"].iter().map(|s| s.to_string()).collect();
        let probs = [0.1f32, 0.9, 0.0, 0.1, 0.9, 0.0, 0.2, 0.0, 0.8];
        let (t, s) = decode(&probs, 3, &chars);
        assert_eq!(t, "ab");
        assert_eq!(s, 0.5666666626930237);
    }
}
