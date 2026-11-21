//! A text row's ink and its letters' features (IRIS's `NameplateFeatures`):
//! the 4x ink mask, the columns with ink, the capital-letter band, and the
//! 388-value vector the letter model reads.

use crate::cv;
use crate::{Image, Rect};

/// How much the mask is upscaled (`NameplateFeatures.Scale`).
pub const SCALE: i32 = 4;
pub const GLYPH_W: usize = 16;
pub const GLYPH_H: usize = 24;
/// 16 x 24 pixels, then width, height, top and bottom offsets.
pub const VECTOR_LENGTH: usize = GLYPH_W * GLYPH_H + 4;
/// The top-hat's square side, at 4x.
const TOPHAT: usize = 31;

/// One text row's ink at 4x, trimmed to the text.
#[derive(Clone, Debug, PartialEq)]
pub struct LineMask {
    /// 0 or 1 per pixel.
    pub data: Image<u8>,
    /// Where the crop started in the source image.
    pub origin_x: i32,
    pub origin_y: i32,
    /// Where the trimmed mask starts inside the 4x crop.
    pub col_offset: i32,
    pub row_offset: i32,
}

impl LineMask {
    pub fn width(&self) -> usize {
        self.data.width
    }

    pub fn height(&self) -> usize {
        self.data.height
    }

    fn at(&self, x: usize, y: usize) -> u8 {
        self.data.data[y * self.data.width + x]
    }

    /// Whether any pixel of row `y` between columns `x0..x1` is ink.
    fn row_has_ink(&self, y: usize, x0: usize, x1: usize) -> bool {
        self.data.row(y)[x0..x1].iter().any(|&v| v != 0)
    }

    fn column_has_ink(&self, x: usize, y0: usize, y1: usize) -> bool {
        (y0..y1).any(|y| self.at(x, y) != 0)
    }

    /// Ink pixels in columns `x0..x1`.
    pub fn ink(&self, x0: usize, x1: usize) -> i64 {
        (0..self.height())
            .map(|y| {
                self.data.row(y)[x0..x1]
                    .iter()
                    .map(|&v| v as i64)
                    .sum::<i64>()
            })
            .sum()
    }

    /// Ink pixels per column over `x0..x1`.
    pub fn column_sums(&self, x0: usize, x1: usize) -> Vec<i64> {
        let mut sums = vec![0i64; x1 - x0];
        for y in 0..self.height() {
            for (s, &v) in sums.iter_mut().zip(&self.data.row(y)[x0..x1]) {
                *s += v as i64;
            }
        }
        sums
    }

    /// The first and last rows with ink between columns `x0..x1`.
    pub fn ink_rows(&self, x0: usize, x1: usize) -> Option<(usize, usize)> {
        let first = (0..self.height()).find(|&y| self.row_has_ink(y, x0, x1))?;
        let last = (0..self.height())
            .rev()
            .find(|&y| self.row_has_ink(y, x0, x1))?;
        Some((first, last))
    }
}

/// The rows the capital letters span, by the median letter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapBand {
    pub top: i32,
    pub bottom: i32,
}

impl CapBand {
    pub fn height(&self) -> i32 {
        self.bottom - self.top + 1
    }
}

/// A box -> the text's ink at 4x: cubic upscale, a 31 x 31 top-hat per
/// channel, the brightest channel, Otsu's threshold clamped to 25-90 (or
/// `threshold`), a 1 x 3 opening, then trimmed to the text.
pub fn create_mask(
    bgr: &Image<u8>,
    rect: Rect,
    threshold: Option<f64>,
    pad: i32,
) -> Option<LineMask> {
    assert!(bgr.channels >= 3, "create_mask takes a colour image");
    let (w, h) = (bgr.width as i32, bgr.height as i32);
    let x0 = (rect.x - pad).max(0);
    let y0 = (rect.y - pad).max(0);
    let x1 = (rect.x + rect.w + pad).min(w);
    let y1 = (rect.y + rect.h + pad).min(h);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let crop = bgr_crop(bgr, x0 as usize, y0 as usize, x1 as usize, y1 as usize);
    let big = cv::resize_cubic_by(&crop, SCALE as f64, SCALE as f64);
    let mut top_hat = cv::tophat(&big.channel(0), TOPHAT, TOPHAT);
    for k in 1..3 {
        let hat = cv::tophat(&big.channel(k), TOPHAT, TOPHAT);
        for (t, &v) in top_hat.data.iter_mut().zip(&hat.data) {
            *t = (*t).max(v);
        }
    }
    let thr = threshold.unwrap_or_else(|| cv::otsu_threshold(&top_hat).clamp(25.0, 90.0));
    let binary = cv::threshold_binary(&top_hat, thr, 1.0);
    let opened = cv::open(&binary, 1, 3);
    trim_to_text(opened, x0, y0)
}

/// The three colour channels of a rectangle (a 4-channel capture loses alpha).
pub(crate) fn bgr_crop(src: &Image<u8>, x0: usize, y0: usize, x1: usize, y1: usize) -> Image<u8> {
    let crop = src.crop(x0, y0, x1 - x0, y1 - y0);
    if crop.channels == 3 {
        return crop;
    }
    let data = crop
        .data
        .chunks_exact(crop.channels)
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    Image::from_vec(crop.width, crop.height, 3, data)
}

/// Trims a 0/1 mask to its text: the first and last rows holding more than
/// 2% of the width in ink, then the columns with any ink between them.
pub fn trim_to_text(data: Image<u8>, origin_x: i32, origin_y: i32) -> Option<LineMask> {
    let (w, h) = (data.width, data.height);
    let busy =
        |y: usize| data.row(y).iter().map(|&v| v as i64).sum::<i64>() as f64 > 0.02 * w as f64;
    let r0 = (0..h).find(|&y| busy(y))?;
    let r1 = (0..h).rev().find(|&y| busy(y))?;
    let col_ink = |x: usize| (r0..=r1).any(|y| data.data[y * w + x] != 0);
    let c0 = (0..w).find(|&x| col_ink(x))?;
    let c1 = (0..w).rev().find(|&x| col_ink(x))?;
    Some(LineMask {
        data: data.crop(c0, r0, c1 - c0 + 1, r1 - r0 + 1),
        origin_x,
        origin_y,
        col_offset: c0 as i32,
        row_offset: r0 as i32,
    })
}

/// Runs of columns with any ink, as (start, end) with end exclusive.
pub fn gap_cuts(m: &LineMask) -> Vec<(usize, usize)> {
    let mut cuts = Vec::new();
    let mut start: Option<usize> = None;
    for x in 0..m.width() {
        let has = m.column_has_ink(x, 0, m.height());
        match (has, start) {
            (true, None) => start = Some(x),
            (false, Some(s)) => {
                cuts.push((s, x));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        cuts.push((s, m.width()));
    }
    cuts
}

fn median_int(values: &mut [i32]) -> i32 {
    values.sort_unstable();
    let n = values.len();
    if n & 1 == 1 {
        values[n / 2]
    } else {
        ((values[n / 2 - 1] + values[n / 2]) as f64 / 2.0) as i32
    }
}

/// The capital band: median top and bottom of the ink runs taller than half
/// the mask; the whole mask when there are none.
pub fn find_cap_band(m: &LineMask) -> CapBand {
    let (mut tops, mut bottoms) = (Vec::new(), Vec::new());
    for (a, b) in gap_cuts(m) {
        if let Some((first, last)) = m.ink_rows(a, b)
            && (last - first) as f64 > 0.5 * m.height() as f64
        {
            tops.push(first as i32);
            bottoms.push(last as i32);
        }
    }
    if tops.is_empty() {
        return CapBand {
            top: 0,
            bottom: m.height() as i32 - 1,
        };
    }
    CapBand {
        top: median_int(&mut tops),
        bottom: median_int(&mut bottoms),
    }
}

/// The letter between columns `x0..x1`, area-resized to 16 x 24, then its
/// width, height and top/bottom offsets in cap heights (x4).
pub fn glyph_vector(m: &LineMask, x0: usize, x1: usize, band: CapBand) -> Option<Vec<f64>> {
    let x1 = x1.min(m.width());
    if x0 >= x1 {
        return None;
    }
    let cap = band.height().max(1) as f64;
    let (row_min, row_max) = m.ink_rows(x0, x1)?;
    let col_min = (x0..x1).find(|&x| m.column_has_ink(x, 0, m.height()))?;
    let col_max = (x0..x1)
        .rev()
        .find(|&x| m.column_has_ink(x, 0, m.height()))?;
    let (gh, gw) = (row_max - row_min + 1, col_max - col_min + 1);
    let mut src = Image::<f32>::new(gw, gh, 1);
    for y in 0..gh {
        for x in 0..gw {
            src.data[y * gw + x] = m.at(col_min + x, row_min + y) as f32;
        }
    }
    let small = cv::resize_area_f32_to(&src, GLYPH_W, GLYPH_H);
    let mut v: Vec<f64> = small.data.iter().map(|&p| p as f64).collect();
    v.push(gw as f64 / cap * 4.0);
    v.push(gh as f64 / cap * 4.0);
    v.push((row_min as f64 - band.top as f64) / cap * 4.0);
    v.push((row_max as f64 - band.bottom as f64) / cap * 4.0);
    Some(v)
}
