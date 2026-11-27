//! Where the HUD's text is: sc_detector.py's brightness masks, the boxes
//! found in them, the name / range pairs and the range-shape ranking -
//! everything the legacy reader does before any OCR.

use scw_vision::cv::{bgr_to_gray, connected_components, dilate};
use scw_vision::{Image, Rect};

use super::{
    BOX_MAX_W, BOX_MIN_W, DILATE_PX, DRIFT_FRACTION, GLYPH_MAX_H, GLYPH_MIN_H, IDEAL_GLYPHS,
    LOCATE_V, MIN_CORE_PIXELS, NEUTRAL_HI, NEUTRAL_MIN, PAIR_MAX_CENTRE_DRIFT, PAIR_MAX_GAP,
    PAIR_MIN_GAP, RANGE_MAX_W, RANGE_MIN_W, RED_MIN, RED_SAT_DOM, RED_SAT_MIN, scaled, scaled_area,
};

/// A box of text found in a mask (sc_detector.Box): where it is, and how
/// many of the mask's own (undilated) pixels it holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LegacyBox {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub core: i64,
}

impl LegacyBox {
    pub fn cx(&self) -> f64 {
        self.x as f64 + self.w as f64 / 2.0
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn rect(&self) -> Rect {
        Rect::new(self.x, self.y, self.w, self.h)
    }

    /// `Box.crop(frame, pad)`: the box and `pad` pixels round it, cut at
    /// the frame's edges.
    pub fn crop(&self, frame: &Image<u8>, pad: i32) -> Image<u8> {
        slice(
            frame,
            (self.y - pad).max(0),
            self.bottom() + pad,
            (self.x - pad).max(0),
            self.x + self.w + pad,
        )
    }
}

/// `img[y0:y1, x0:x1]` for starts at or above zero, as numpy cuts it: ends
/// past the image stop at its edge, and an empty range gives an empty
/// image.
pub(crate) fn slice(img: &Image<u8>, y0: i32, y1: i32, x0: i32, x1: i32) -> Image<u8> {
    let (w, h) = (img.width as i32, img.height as i32);
    let (y0, y1) = (y0.min(h), y1.min(h));
    let (x0, x1) = (x0.min(w), x1.min(w));
    if y1 <= y0 || x1 <= x0 {
        return Image::new(0, 0, img.channels);
    }
    img.crop(
        x0 as usize,
        y0 as usize,
        (x1 - x0) as usize,
        (y1 - y0) as usize,
    )
}

/// `_drop_oversized`: of a 0/1 mask's 8-connected components, only those
/// small enough to be glyphs, at 255.
fn drop_oversized(raw: &Image<u8>, frame_h: usize) -> Image<u8> {
    let limit = scaled(GLYPH_MAX_H, frame_h) as i32;
    let (labels, stats) = connected_components(raw, 8);
    let keep: Vec<u8> = stats
        .iter()
        .enumerate()
        .map(|(i, s)| {
            if i > 0 && s[3] <= limit && s[2] <= limit * 6 {
                255
            } else {
                0
            }
        })
        .collect();
    let data = labels.data.iter().map(|&l| keep[l as usize]).collect();
    Image::from_vec(raw.width, raw.height, 1, data)
}

/// One 0/1 mask per pixel rule over a BGR frame.
fn rule_mask(bgr: &Image<u8>, rule: impl Fn(u8, u8, u8) -> bool) -> Image<u8> {
    assert_eq!(bgr.channels, 3, "the masks take a BGR image");
    let data = bgr
        .data
        .as_chunks::<3>()
        .0
        .iter()
        .map(|p| rule(p[0], p[1], p[2]) as u8)
        .collect();
    Image::from_vec(bgr.width, bgr.height, 1, data)
}

/// `hud_masks`: the locators, each kept separately (unioned, text would
/// reconnect to the background it merges with in another and be dropped
/// with it) - bright (HSV value >= 245, any colour), near-white
/// (min(R,G,B) >= 170), red (R >= 245), and with `haze` the near-white rule
/// at 250 for sunlit haze. Every size is scaled by `frame_h`, the height of
/// the SCREEN, which a crop must be told.
pub fn hud_masks(bgr: &Image<u8>, frame_h: usize, haze: bool) -> Vec<Image<u8>> {
    let mut masks = vec![
        // cvtColor(BGR2HSV)'s value channel is max(R,G,B) on 8 bits.
        rule_mask(bgr, |b, g, r| b.max(g).max(r) >= LOCATE_V),
        rule_mask(bgr, |b, g, r| b.min(g).min(r) >= NEUTRAL_MIN),
        rule_mask(bgr, |_, _, r| r >= RED_MIN),
    ];
    if haze {
        masks.push(rule_mask(bgr, |b, g, r| b.min(g).min(r) >= NEUTRAL_HI));
    }
    masks.iter().map(|m| drop_oversized(m, frame_h)).collect()
}

/// `hud_mask`: the bright locator alone, sized by the image's own height
/// (the monitor-rate pass).
pub fn hud_mask(bgr: &Image<u8>) -> Image<u8> {
    drop_oversized(
        &rule_mask(bgr, |b, g, r| b.max(g).max(r) >= LOCATE_V),
        bgr.height,
    )
}

/// `red_sat_mask`: saturated red - red labels on a screen without HDR,
/// which never reach the white point (R >= 170 and above G and B by 150).
pub fn red_sat_mask(bgr: &Image<u8>, frame_h: usize) -> Image<u8> {
    let raw = rule_mask(bgr, |b, g, r| {
        r >= RED_SAT_MIN && r as i16 - g.max(b) as i16 >= RED_SAT_DOM
    });
    drop_oversized(&raw, frame_h)
}

/// `find_boxes`: glyphs merged into words by a horizontal dilation, then
/// the components the size of a word with enough real ink.
pub fn find_boxes(mask: &Image<u8>, frame_h: usize) -> Vec<LegacyBox> {
    let kernel_w = scaled(DILATE_PX, frame_h) as usize * 2 + 1;
    let merged = dilate(mask, kernel_w, 1);
    let (_, stats) = connected_components(&merged, 8);
    let (min_h, max_h) = (scaled(GLYPH_MIN_H, frame_h), scaled(GLYPH_MAX_H, frame_h));
    let (min_w, max_w) = (scaled(BOX_MIN_W, frame_h), scaled(BOX_MAX_W, frame_h));
    let floor = scaled_area(MIN_CORE_PIXELS, frame_h);
    let mut boxes = Vec::new();
    for s in stats.iter().skip(1) {
        let [x, y, w, bh, _] = *s;
        let (w64, h64) = (w as i64, bh as i64);
        if !(min_h <= h64 && h64 <= max_h && min_w <= w64 && w64 <= max_w) {
            continue;
        }
        let mut core = 0i64;
        for row in y as usize..(y + bh) as usize {
            core += mask.row(row)[x as usize..(x + w) as usize]
                .iter()
                .filter(|&&v| v != 0)
                .count() as i64;
        }
        if core < floor {
            continue;
        }
        boxes.push(LegacyBox {
            x,
            y,
            w,
            h: bh,
            core,
        });
    }
    boxes
}

/// The box shrunk to the ink of `mask` inside it; None under 8 pixels.
fn ink_box(mask: &Image<u8>, b: &LegacyBox) -> Option<LegacyBox> {
    let (mut x0, mut y0, mut x1, mut y1, mut n) = (i32::MAX, i32::MAX, -1, -1, 0usize);
    for y in b.y..b.bottom() {
        let row = mask.row(y as usize);
        for x in b.x..b.x + b.w {
            if row[x as usize] != 0 {
                n += 1;
                x0 = x0.min(x);
                x1 = x1.max(x);
                y0 = y0.min(y);
                y1 = y1.max(y);
            }
        }
    }
    (n >= 8).then_some(LegacyBox {
        x: x0,
        y: y0,
        w: x1 - x0 + 1,
        h: y1 - y0 + 1,
        core: b.core,
    })
}

/// Do two boxes overlap by more than half the smaller one's area?
fn overlaps(b: &LegacyBox, k: &LegacyBox) -> bool {
    let ox = (b.x + b.w).min(k.x + k.w) - b.x.max(k.x);
    let oy = b.bottom().min(k.bottom()) - b.y.max(k.y);
    let small = (b.w as i64 * b.h as i64).min(k.w as i64 * k.h as i64);
    // ox * oy > 0.5 * small, in integers
    ox > 0 && oy > 0 && 2 * (ox as i64 * oy as i64) > small
}

/// `find_all_boxes`: boxes from every locator, each shrunk to the ink of
/// all the masks together, duplicates dropped (the box with more ink
/// kept); then the saturated-red locator's, only where no box is yet.
pub fn find_all_boxes(bgr: &Image<u8>, frame_h: usize, haze: bool) -> Vec<LegacyBox> {
    let masks = hud_masks(bgr, frame_h, haze);
    let mut found: Vec<LegacyBox> = masks.iter().flat_map(|m| find_boxes(m, frame_h)).collect();

    let mut union = masks[0].clone();
    for m in &masks[1..] {
        for (u, &v) in union.data.iter_mut().zip(&m.data) {
            *u |= v;
        }
    }
    for b in found.iter_mut() {
        if let Some(nb) = ink_box(&union, b)
            && nb.w >= 8
            && nb.h >= 5
        {
            *b = nb;
        }
    }

    // A stable sort, as Python's.
    found.sort_by_key(|b| std::cmp::Reverse(b.core));
    let mut kept: Vec<LegacyBox> = Vec::new();
    for b in found {
        if !kept.iter().any(|k| overlaps(&b, k)) {
            kept.push(b);
        }
    }

    let red = red_sat_mask(bgr, frame_h);
    for mut b in find_boxes(&red, frame_h) {
        if let Some(nb) = ink_box(&red, &b)
            && nb.w >= 8
            && nb.h >= 5
        {
            b = nb;
        }
        if !kept.iter().any(|k| overlaps(&b, k)) {
            kept.push(b);
        }
    }
    kept
}

/// `pair_boxes`: each name box with every range-wide box centred under it
/// at a plausible gap, as (name, range) indices. The centre allowance grows
/// with the name's width: a label partly over something bright loses the
/// occluded glyphs from its box, and its centre with them.
pub fn pair_boxes(boxes: &[LegacyBox], frame_h: usize) -> Vec<(usize, usize)> {
    let drift = scaled(PAIR_MAX_CENTRE_DRIFT, frame_h) as f64;
    let (gap_lo, gap_hi) = (scaled(PAIR_MIN_GAP, frame_h), scaled(PAIR_MAX_GAP, frame_h));
    let (rng_lo, rng_hi) = (scaled(RANGE_MIN_W, frame_h), scaled(RANGE_MAX_W, frame_h));
    let mut pairs = Vec::new();
    for (ni, name) in boxes.iter().enumerate() {
        let name_drift = drift.max(name.w as f64 * DRIFT_FRACTION);
        for (ri, rng) in boxes.iter().enumerate() {
            if ri == ni {
                continue;
            }
            let w = rng.w as i64;
            if !(rng_lo <= w && w <= rng_hi) {
                continue;
            }
            if (rng.cx() - name.cx()).abs() > name_drift {
                continue;
            }
            let gap = (rng.y - name.bottom()) as i64;
            if !(gap_lo <= gap && gap <= gap_hi) {
                continue;
            }
            pairs.push((ni, ri));
        }
    }
    pairs
}

/// `np.percentile(g, 99)` of an 8-bit image: numpy's linear method, its
/// virtual index and its two-sided interpolation, to the bit.
pub(crate) fn percentile99(img: &Image<u8>) -> f64 {
    let n = img.data.len();
    let mut hist = [0usize; 256];
    for &v in &img.data {
        hist[v as usize] += 1;
    }
    // The k-th smallest value.
    let kth = |k: usize| {
        let mut seen = 0;
        for (v, &c) in hist.iter().enumerate() {
            seen += c;
            if seen > k {
                return v as f64;
            }
        }
        255.0
    };
    let virtual_index = (n - 1) as f64 * 0.99;
    let previous = virtual_index.floor();
    let (lo, hi, gamma) = if virtual_index >= (n - 1) as f64 {
        // numpy takes the last value both sides (index -1)
        (n - 1, n - 1, virtual_index + 1.0)
    } else {
        let p = previous as usize;
        (p, p + 1, virtual_index - previous)
    };
    let (a, b) = (kth(lo), kth(hi));
    let d = b - a;
    if gamma >= 0.5 {
        b - d * (1.0 - gamma)
    } else {
        a + d * gamma
    }
}

/// `range_shape_score`: 0..1, how much a crop looks like an 'N.Nkm'
/// readout by its glyph count against its own brightest pixels - for
/// ranking candidates before OCR, never for rejecting them.
pub fn range_shape_score(crop: &Image<u8>) -> f64 {
    if crop.data.is_empty() {
        return 0.0;
    }
    let g = bgr_to_gray(crop);
    let thr = 60i64.max((percentile99(&g) * 0.55) as i64);
    let m = Image::from_vec(
        g.width,
        g.height,
        1,
        g.data.iter().map(|&v| (v as i64 >= thr) as u8).collect(),
    );
    let (_, stats) = connected_components(&m, 8);
    let h = crop.height as f64;
    let glyphs = stats
        .iter()
        .skip(1)
        .filter(|s| s[3] as f64 >= h * 0.35 && s[4] >= 4)
        .count() as i64;
    if glyphs == 0 {
        return 0.0;
    }
    1.0 / (1.0 + (glyphs - IDEAL_GLYPHS).abs() as f64)
}

/// `pair_diagnostics`: why boxes stacked at a plausible gap did not pair -
/// a range box outside the width window, or centres apart - as --debug's
/// near-miss lines; at most `limit`.
pub fn pair_diagnostics(boxes: &[LegacyBox], frame_h: usize, limit: usize) -> Vec<String> {
    let drift = scaled(PAIR_MAX_CENTRE_DRIFT, frame_h);
    let (gap_lo, gap_hi) = (scaled(PAIR_MIN_GAP, frame_h), scaled(PAIR_MAX_GAP, frame_h));
    let (rng_lo, rng_hi) = (scaled(RANGE_MIN_W, frame_h), scaled(RANGE_MAX_W, frame_h));
    let mut out = Vec::new();
    for (ni, name) in boxes.iter().enumerate() {
        for (ri, rng) in boxes.iter().enumerate() {
            if ri == ni {
                continue;
            }
            let gap = (rng.y - name.bottom()) as i64;
            if !(gap_lo <= gap && gap <= gap_hi) {
                continue;
            }
            let w = rng.w as i64;
            let off = (rng.cx() - name.cx()).abs();
            let why = if !(rng_lo <= w && w <= rng_hi) {
                Some(format!(
                    "range box {}px outside {rng_lo}-{rng_hi}{}",
                    rng.w,
                    if w > rng_hi { "  <-- TOO WIDE" } else { "" }
                ))
            } else if off > drift as f64 {
                Some(format!("centres off by {off:.0}px > {drift}"))
            } else {
                None
            };
            if let Some(why) = why {
                out.push(format!(
                    "@{},{} {}x{} over {}x{}: {why}",
                    name.x, name.y, name.w, name.h, rng.w, rng.h
                ));
            }
            if out.len() >= limit {
                return out;
            }
        }
    }
    out
}
