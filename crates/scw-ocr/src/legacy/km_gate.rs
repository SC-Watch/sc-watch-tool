//! km_gate.py: does a crop end in the glyphs 'km'? The last two glyphs,
//! segmented against the crop's own brightest pixels at five thresholds,
//! scaled to 30x18 and compared with a template made from real range boxes
//! (km_template.npy, in the repository). Microseconds a crop; used to rank
//! the dim-range retries, and - with the gate setting on - to skip OCR on
//! pairs whose range box does not end in 'km'.

use std::sync::OnceLock;

use scw_vision::Image;
use scw_vision::cv::{connected_components, resize_area_to};

use super::boxes::slice;

/// The template's size: the 'km' glyph pair, normalised.
pub const TPL_W: usize = 30;
pub const TPL_H: usize = 18;

const MIN_GLYPH_H_FRAC: f64 = 0.45;
const MIN_GLYPH_AREA: i32 = 4;
const MIN_GLYPHS: usize = 3;
const MAX_GLYPHS: usize = 8;
const SEGMENT_FRACS: [f64; 5] = [0.45, 0.55, 0.65, 0.75, 0.85];

static TEMPLATE_NPY: &[u8] = include_bytes!("../../../../km_template.npy");

/// The 'km' template (18 rows of 30), from km_template.npy.
pub fn template() -> &'static [u8] {
    static T: OnceLock<Vec<u8>> = OnceLock::new();
    T.get_or_init(|| npy_u8(TEMPLATE_NPY, TPL_H, TPL_W))
}

/// A version-1 .npy file of unsigned bytes in C order, of this shape.
fn npy_u8(npy: &[u8], rows: usize, cols: usize) -> Vec<u8> {
    assert!(
        npy.starts_with(b"\x93NUMPY\x01"),
        "km_template.npy: not a version-1 .npy file"
    );
    let len = u16::from_le_bytes([npy[8], npy[9]]) as usize;
    let header = std::str::from_utf8(&npy[10..10 + len]).expect("km_template.npy header");
    assert!(
        header.contains("'|u1'")
            && header.contains("'fortran_order': False")
            && header.contains(&format!("({rows}, {cols})")),
        "km_template.npy: {header}"
    );
    let data = &npy[10 + len..];
    assert_eq!(data.len(), rows * cols, "km_template.npy: size");
    data.to_vec()
}

/// `segment`: 255 where the crop's value (max of B, G, R) reaches `frac`
/// of its brightest, and at least 110.
fn segment(crop: &Image<u8>, frac: f64) -> Image<u8> {
    let v: Vec<u8> = if crop.channels == 3 {
        crop.data
            .as_chunks::<3>()
            .0
            .iter()
            .map(|p| p[0].max(p[1]).max(p[2]))
            .collect()
    } else {
        crop.data.clone()
    };
    let max = v.iter().copied().max().unwrap_or(0);
    let thr = 110i64.max((max as f64 * frac) as i64);
    let data = v
        .iter()
        .map(|&x| if x as i64 >= thr { 255 } else { 0 })
        .collect();
    Image::from_vec(crop.width, crop.height, 1, data)
}

/// `glyph_boxes`: components tall enough to be glyphs, left to right
/// (equal lefts in label order).
fn glyph_boxes(mask: &Image<u8>) -> Vec<[i32; 4]> {
    let (_, stats) = connected_components(mask, 8);
    let min_h = 3i32.max((mask.height as f64 * MIN_GLYPH_H_FRAC) as i32);
    let mut out: Vec<[i32; 4]> = stats
        .iter()
        .skip(1)
        .filter(|s| s[3] >= min_h && s[4] >= MIN_GLYPH_AREA)
        .map(|s| [s[0], s[1], s[2], s[3]])
        .collect();
    out.sort_by_key(|g| g[0]);
    out
}

/// `km_patch`: the last two glyphs' bitmap at 30x18, or None when the crop
/// does not segment like a range readout.
fn km_patch(crop: &Image<u8>, frac: f64) -> Option<Image<u8>> {
    let mask = segment(crop, frac);
    let gs = glyph_boxes(&mask);
    if !(MIN_GLYPHS..=MAX_GLYPHS).contains(&gs.len()) {
        return None;
    }
    let last = &gs[gs.len() - 2..];
    let x0 = last[0][0];
    let x1 = last.iter().map(|g| g[0] + g[2]).max().unwrap();
    let y0 = last.iter().map(|g| g[1]).min().unwrap();
    let y1 = last.iter().map(|g| g[1] + g[3]).max().unwrap();
    if x1 - x0 < 4 || y1 - y0 < 4 {
        return None;
    }
    Some(resize_area_to(&slice(&mask, y0, y1, x0, x1), TPL_W, TPL_H))
}

/// numpy's float32 sum of a contiguous array: pairwise, eight running
/// sums per block of up to 128.
fn np_sum_f32(a: &[f32]) -> f32 {
    let n = a.len();
    if n < 8 {
        let mut r = 0f32;
        for &v in a {
            r += v;
        }
        r
    } else if n <= 128 {
        let mut r = [0f32; 8];
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
        np_sum_f32(&a[..n2]) + np_sum_f32(&a[n2..])
    }
}

/// `_similarity`: soft IoU of the ink, in float32 as numpy has it.
fn similarity(patch: &[u8], template: &[u8]) -> f64 {
    let (mut lo, mut hi) = (
        Vec::with_capacity(patch.len()),
        Vec::with_capacity(patch.len()),
    );
    for (&p, &t) in patch.iter().zip(template) {
        let (a, b) = (p as f32 / 255.0, t as f32 / 255.0);
        lo.push(a.min(b));
        hi.push(a.max(b));
    }
    let (inter, union) = (np_sum_f32(&lo), np_sum_f32(&hi));
    // max(union, 1e-6): a sum of these is 0 or at least 1/255
    if union > 0.0 {
        (inter / union) as f64
    } else {
        0.0
    }
}

/// `score`: the best 'km' match over the five segmentations.
pub fn score(crop: &Image<u8>, template: &[u8]) -> f64 {
    let mut best = 0.0f64;
    for frac in SEGMENT_FRACS {
        if let Some(p) = km_patch(crop, frac) {
            best = best.max(similarity(&p.data, template));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_loads() {
        let t = template();
        assert_eq!(t.len(), TPL_W * TPL_H);
        assert!(t.iter().any(|&v| v > 0));
    }

    #[test]
    fn numpy_sums_in_its_own_order() {
        // np.full(540, 0.1, np.float32).sum(), and .reshape(18, 30).sum():
        // 54.00000762939453 (a running sum gives 53.99974822998047)
        assert_eq!(np_sum_f32(&[0.1f32; 540]) as f64, 54.000_007_629_394_53);
    }
}
