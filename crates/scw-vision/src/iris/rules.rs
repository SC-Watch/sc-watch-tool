//! The rules IRIS applies after the letter model: O against 0 by the shape
//! of the hole (`O0Rule`), '_' against '-' by the bar's height
//! (`UnderscoreHyphenRule`), and the distance line's checks
//! (`DistanceLineRule`).

use super::ReaderSettings;
use super::features::{CapBand, LineMask, SCALE, bgr_crop, find_cap_band};
use super::reader::{EFFICIENCY_GROUPS, Piece, PieceRead};
use crate::{Image, cv};

// ---- O / 0 ----------------------------------------------------------------------

const O0_WEIGHT_A: f64 = 0.5667556378738614;
const O0_WEIGHT_B: f64 = 0.8238859429181884;
const O0_MEAN_O: f64 = 0.7533676849758579;
const O0_SD_O: f64 = 0.025796386202400184;
const O0_MEAN_0: f64 = 0.6808192767983506;
const O0_SD_0: f64 = 0.01720362317305907;
const O0_UPSCALE: usize = 8;
const O0_PAD: i32 = 3;
/// How sure the rule must be to change a letter.
pub const O0_DECIDE: f64 = 0.95;

/// Outer width and height, hole width and height, in source pixels.
pub type O0Measure = (f64, f64, f64, f64);

fn percentile_sorted(s: &[f32], q: usize) -> f64 {
    let pos = ((s.len() - 1) * q) as f64 / 100.0;
    let lower = pos.floor() as usize;
    let upper = (lower + 1).min(s.len() - 1);
    s[lower] as f64 + (s[upper] as f64 - s[lower] as f64) * (pos - lower as f64)
}

/// Measures the letter in a small colour crop: the brightest channel,
/// upscaled 8x, thresholded halfway between its 10th and 99th percentiles.
pub fn o0_measure(crop: &Image<u8>) -> Option<O0Measure> {
    let mut gray = Image::<f32>::new(crop.width, crop.height, 1);
    for (g, p) in gray
        .data
        .iter_mut()
        .zip(crop.data.chunks_exact(crop.channels))
    {
        *g = *p.iter().max().unwrap() as f32;
    }
    let f = O0_UPSCALE as f64;
    let big = cv::resize_cubic_f32_by(&gray, f, f);
    let mut s = big.data.clone();
    s.sort_unstable_by(f32::total_cmp);
    let thr = ((percentile_sorted(&s, 10) + percentile_sorted(&s, 99)) / 2.0) as f32;
    let data = big.data.iter().map(|&v| (v > thr) as u8).collect();
    o0_measure_mask(&Image::from_vec(big.width, big.height, 1, data))
}

/// The widest-inked run of columns, its height, and its largest interior hole.
pub fn o0_measure_mask(mask: &Image<u8>) -> Option<O0Measure> {
    let (w, h) = (mask.width, mask.height);
    let mut columns = vec![0i64; w];
    for y in 0..h {
        for (c, &v) in columns.iter_mut().zip(mask.row(y)) {
            *c += v as i64;
        }
    }
    let (mut best_start, mut best_end, mut best_ink) = (None, 0, -1i64);
    let mut at = 0;
    while at < w {
        if columns[at] > 0 {
            let start = at;
            let mut ink = 0;
            while at < w && columns[at] > 0 {
                ink += columns[at];
                at += 1;
            }
            if ink > best_ink {
                (best_ink, best_start, best_end) = (ink, Some(start), at);
            }
        }
        at += 1;
    }
    let start = best_start?;
    let gw = best_end - start;
    let has = |y: usize| mask.row(y)[start..best_end].iter().any(|&v| v != 0);
    let first = (0..h).find(|&y| has(y))?;
    let last = (0..h).rev().find(|&y| has(y))?;
    let gh = last - first + 1;
    if gw as f64 > 1.6 * gh as f64 {
        return None;
    }
    let region = mask.crop(start, first, gw, gh);
    let holes = Image::from_vec(
        gw,
        gh,
        1,
        region.data.iter().map(|&v| (v == 0) as u8).collect(),
    );
    let (_, stats) = cv::connected_components(&holes, 4);
    // Labels come in raster order of their first pixel, so among equal
    // largest holes the first one wins, as in IRIS.
    let (mut best_area, mut hole_w, mut hole_h) = (0, 0, 0);
    for s in stats.iter().skip(1) {
        let [x, y, bw, bh, area] = *s;
        let interior = x > 0 && y > 0 && x + bw < gw as i32 && y + bh < gh as i32;
        if interior && area > best_area {
            (best_area, hole_w, hole_h) = (area, bw, bh);
        }
    }
    if best_area == 0 {
        return None;
    }
    let u = O0_UPSCALE as f64;
    Some((
        gw as f64 / u,
        gh as f64 / u,
        hole_w as f64 / u,
        hole_h as f64 / u,
    ))
}

/// The probability the letter is an O rather than a 0.
pub fn o0_probability(m: O0Measure) -> f64 {
    let (ow, oh, hw, _) = m;
    let s = (ow / oh) * O0_WEIGHT_A + (hw / oh) * O0_WEIGHT_B;
    let s = s.clamp(O0_MEAN_0.min(O0_MEAN_O), O0_MEAN_0.max(O0_MEAN_O));
    let l_o = -0.5 * ((s - O0_MEAN_O) / O0_SD_O).powi(2) - O0_SD_O.ln();
    let l_0 = -0.5 * ((s - O0_MEAN_0) / O0_SD_0).powi(2) - O0_SD_0.ln();
    let d = l_0 - l_o;
    if d > 700.0 {
        return 0.0;
    }
    1.0 / (1.0 + d.exp())
}

/// The letter's box in the source image, padded: (x0, y0, x1, y1).
pub fn o0_glyph_box(mask: &LineMask, start: usize, end: usize) -> Option<(i32, i32, i32, i32)> {
    let clamped = end.min(mask.width());
    if start >= clamped {
        return None;
    }
    let (first, last) = mask.ink_rows(start, clamped)?;
    let floor = |v: i32| v.div_euclid(SCALE);
    let ceil = |v: i32| -(-v).div_euclid(SCALE);
    Some((
        mask.origin_x + floor(mask.col_offset + start as i32) - O0_PAD,
        mask.origin_y + floor(mask.row_offset + first as i32) - O0_PAD,
        mask.origin_x + ceil(mask.col_offset + end as i32) + O0_PAD,
        mask.origin_y + ceil(mask.row_offset + last as i32 + 1) + O0_PAD,
    ))
}

/// Re-decides each O and 0 by its hole; `evidence` gets (letter, P(O)).
pub fn o0_correct(
    bgr: &Image<u8>,
    read: &PieceRead,
    mask: &LineMask,
    text: &str,
    evidence: &mut Vec<(usize, f64)>,
) -> String {
    let mut chars: Vec<char> = text.chars().collect();
    if read.spans.len() != chars.len() || !chars.iter().any(|&c| c == 'O' || c == '0') {
        return text.to_string();
    }
    let (w, h) = (bgr.width as i32, bgr.height as i32);
    for k in 0..chars.len() {
        if chars[k] != 'O' && chars[k] != '0' {
            continue;
        }
        let (s, e) = read.spans[k];
        let Some((bx0, by0, bx1, by1)) = o0_glyph_box(mask, s, e) else {
            continue;
        };
        let (x0, y0, x1, y1) = (bx0.max(0), by0.max(0), bx1.min(w), by1.min(h));
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let crop = bgr_crop(bgr, x0 as usize, y0 as usize, x1 as usize, y1 as usize);
        let Some(m) = o0_measure(&crop) else {
            continue;
        };
        let p = o0_probability(m);
        evidence.push((k, p));
        if p >= O0_DECIDE {
            chars[k] = 'O';
        } else if p <= 1.0 - O0_DECIDE {
            chars[k] = '0';
        }
    }
    chars.into_iter().collect()
}

// ---- '_' / '-' --------------------------------------------------------------------

const BAR_UNDERSCORE_MIN: f64 = 0.87;
const BAR_HYPHEN_MAX: f64 = 0.71;
const BAR_MAX_THICK: f64 = 0.45;
const BAR_ROW_FRACTION: f64 = 0.25;

/// The bar's middle (from the cap top, in cap heights), thickness and length.
pub fn bar_measure(
    m: &LineMask,
    start: usize,
    end: usize,
    band: CapBand,
) -> Option<(f64, f64, f64)> {
    let end = end.min(m.width());
    if end <= start {
        return None;
    }
    let sums: Vec<i64> = (0..m.height())
        .map(|y| m.data.row(y)[start..end].iter().map(|&v| v as i64).sum())
        .collect();
    let top = *sums.iter().max()?;
    if top <= 0 {
        return None;
    }
    let busy = |y: &usize| sums[*y] as f64 >= BAR_ROW_FRACTION * top as f64;
    let r0 = (0..sums.len()).find(busy)?;
    let r1 = (0..sums.len()).rev().find(busy)?;
    let ink_col = |x: usize| (r0..=r1).any(|y| m.data.row(y)[x] != 0);
    let c0 = (start..end).find(|&x| ink_col(x));
    let c1 = (start..end).rev().find(|&x| ink_col(x));
    let cap = band.height().max(1) as f64;
    let frac = ((r0 + r1 + 1) as f64 / 2.0 - band.top as f64) / cap;
    let thick = (r1 - r0 + 1) as f64 / cap;
    let length = match (c0, c1) {
        (Some(a), Some(b)) => (b - a + 1) as f64 / cap,
        _ => 0.0,
    };
    Some((frac, thick, length))
}

/// Re-decides each '_' and '-' by how low its bar sits.
pub fn underscore_hyphen_correct(read: &PieceRead, mask: &LineMask, text: &str) -> String {
    let mut chars: Vec<char> = text.chars().collect();
    if read.spans.len() != chars.len() || !chars.iter().any(|&c| c == '_' || c == '-') {
        return text.to_string();
    }
    let band = find_cap_band(mask);
    for k in 0..chars.len() {
        if chars[k] != '_' && chars[k] != '-' {
            continue;
        }
        let (s, e) = read.spans[k];
        let Some((frac, thick, _)) = bar_measure(mask, s, e, band) else {
            continue;
        };
        if thick > BAR_MAX_THICK {
            continue;
        }
        if frac >= BAR_UNDERSCORE_MIN {
            chars[k] = '_';
        } else if frac <= BAR_HYPHEN_MAX {
            chars[k] = '-';
        }
    }
    chars.into_iter().collect()
}

// ---- distance lines -----------------------------------------------------------------

/// A distance read off a nameplate's second line.
#[derive(Clone, Debug, PartialEq)]
pub struct DistanceReading {
    pub text: String,
    pub value: f64,
    pub kilometers: bool,
    /// A speed group ('[' ...) followed the distance.
    pub speed_group: bool,
}

impl DistanceReading {
    pub fn km(&self) -> f64 {
        if self.kilometers {
            self.value
        } else {
            self.value / 1000.0
        }
    }
}

/// A letter the distance font's digit is mistaken for, as that digit.
pub fn efficiency_digit(c: char) -> Option<char> {
    if c.is_ascii_digit() {
        return Some(c);
    }
    EFFICIENCY_GROUPS
        .iter()
        .find(|g| g.find(c).is_some_and(|i| i > 0))
        .and_then(|g| g.chars().next())
}

/// "123.4km" / "56.7m": up to four digits, a point, one digit, the unit.
pub fn parse_distance(text: &str, max_value: u32) -> Option<DistanceReading> {
    let b = text.as_bytes();
    let mut at = 0;
    while at < b.len() && at < 4 && b[at].is_ascii_digit() {
        at += 1;
    }
    if at == 0 || at >= b.len() || b[at] != b'.' {
        return None;
    }
    let whole: u32 = text[..at].parse().ok()?;
    if whole > max_value {
        return None;
    }
    if at + 1 >= b.len() || !b[at + 1].is_ascii_digit() {
        return None;
    }
    let tenth = (b[at + 1] - b'0') as f64;
    let unit = &text[at + 2..];
    if unit != "km" && unit != "m" {
        return None;
    }
    Some(DistanceReading {
        text: text.to_string(),
        value: whole as f64 + tenth / 10.0,
        kilometers: unit == "km",
        speed_group: false,
    })
}

/// IRIS's distance-line checks on a read: junk trimmed off either end by
/// score per letter (IRIS PR #1), a word gap allowed only before a '[',
/// letters read as their digits, then the score and the format.
pub fn evaluate_distance(
    read: &PieceRead,
    settings: &ReaderSettings,
    is_open_bracket: impl Fn(&Piece) -> bool,
) -> Option<DistanceReading> {
    if read.pieces.is_empty() {
        return None;
    }
    let all = &read.pieces;
    let (mut first, mut last) = (0, all.len() - 1);
    while last > first && all[last].score_per_letter() < settings.distance_end_score {
        last -= 1;
    }
    while first < last && all[first].score_per_letter() < settings.distance_end_score {
        first += 1;
    }
    let mut pieces = &all[first..=last];
    let cap = read.cap as f64;
    let mut speed = false;
    for i in 0..pieces.len().saturating_sub(1) {
        let gap = pieces[i + 1].start as f64 - pieces[i].end as f64;
        if gap <= settings.word_gap * cap {
            continue;
        }
        if !is_open_bracket(&pieces[i + 1]) {
            return None;
        }
        pieces = &pieces[..=i];
        speed = true;
        break;
    }
    let mut text = String::new();
    let (mut chars, mut total) = (0usize, 0.0f64);
    for p in pieces {
        text.extend(p.text.chars().map(|c| efficiency_digit(c).unwrap_or(c)));
        chars += p.len();
        total += p.score;
    }
    let mean = total / chars.max(1) as f64;
    let count = pieces.len().min(6);
    let gap = pieces[..count]
        .iter()
        .map(|p| (p.free_score - p.score) / p.len().max(1) as f64)
        .sum::<f64>()
        / count as f64;
    if mean < settings.min_distance_score || gap > settings.max_distance_gap {
        return None;
    }
    let mut parsed = parse_distance(&text, settings.max_distance_value)?;
    parsed.speed_group = speed;
    Some(parsed)
}
