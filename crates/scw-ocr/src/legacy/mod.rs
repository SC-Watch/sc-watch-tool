//! The legacy OCR reader: sc-watch's own pipeline from before 0.3.0
//! (sc_detector.py, as `watch.py --reader ocr` runs it), for players who
//! keep the game's own HDR option on, where IRIS reads nothing (PORT.md,
//! "OCR (phase 5)").
//!
//! A full read finds HUD text with brightness masks, pairs each name box
//! with a range box centred under it, ranks the pairs by how much the range
//! looks like 'N.Nkm' and reads the best eight with OCR - the range first,
//! the name only when the range reads as one. A focused read hands the
//! centre of the screen to the OCR's own detector and pairs its lines
//! instead; a dual read merges the two. Every size is tuned at 1440 lines
//! and scaled by the screen's height.
//!
//! Exactly the Python's reads (tests/legacy_parity.rs), but for one
//! improvement shared with the IRIS reader: damaged ship serials are taken
//! as serials, not players ([`Options::as_python`] turns it off).

mod boxes;
pub mod km_gate;

use scw_vision::cv::{bgr_to_gray, gray_to_bgr, resize_cubic_by};
use scw_vision::names::{NameKind, classify_name, classify_name_as_python, py_digit};
use scw_vision::pipeline::Contact;
use scw_vision::{Image, Rect};

use crate::{DetSettings, Limit, Line, Ocr};
use boxes::slice;
pub use boxes::{
    LegacyBox, find_all_boxes, find_boxes, hud_mask, hud_masks, pair_boxes, pair_diagnostics,
    range_shape_score, red_sat_mask,
};

// Tuning, measured at 1440 lines (sc_detector.py says where each came from).
const REFERENCE_HEIGHT: i64 = 1440;
const GLYPH_MIN_H: i64 = 7;
const GLYPH_MAX_H: i64 = 26;
const BOX_MIN_W: i64 = 18;
const BOX_MAX_W: i64 = 520;
const MIN_CORE_PIXELS: i64 = 25;
const DILATE_PX: i64 = 7;
const PAIR_MAX_CENTRE_DRIFT: i64 = 5;
const DRIFT_FRACTION: f64 = 0.15;
const PAIR_MIN_GAP: i64 = 4;
const PAIR_MAX_GAP: i64 = 20;
const RANGE_MIN_W: i64 = 26;
const RANGE_MAX_W: i64 = 150;
const LOCATE_V: u8 = 245;
const NEUTRAL_MIN: u8 = 170;
const RED_MIN: u8 = 245;
const NEUTRAL_HI: u8 = 250;
const RED_SAT_MIN: u8 = 170;
const RED_SAT_DOM: i16 = 150;
/// The gate's floor: a range box scoring under it is not read.
pub const KM_GATE_THRESHOLD: f64 = 0.43;
const OCR_UPSCALE: f64 = 3.0;
const OCR_MARGIN_NATIVE: usize = 16;
/// Pixels of frame kept round a box when it is cut out for reading.
pub const CROP_PAD: i32 = 6;
/// Pairs a full read sends to OCR, the best-ranked first.
pub const MAX_OCR_PAIRS: usize = 8;
const IDEAL_GLYPHS: i64 = 4;
const FOCUS_W: f64 = 1200.0;
const FOCUS_H: f64 = 600.0;
const FOCUS_UP: f64 = 0.54;
/// The pair cap for a focused read through the masks.
pub const FOCUS_MAX_PAIRS: usize = 40;
const RANGE_RETRY_MAX: usize = 3;
const RANGE_RETRY_CENTRE_FRAC: f64 = 0.12;
const RANGE_REREAD_PAD: i64 = 6;
/// Dim-range retries a full read makes when the setting is on.
pub const ORPHAN_RETRY_WHEN_ON: usize = 2;

/// `_scaled`: a length at 1440 lines, for a screen `frame_h` tall.
fn scaled(value: i64, frame_h: usize) -> i64 {
    let v = (value * frame_h as i64) as f64 / REFERENCE_HEIGHT as f64;
    1.max(v.round_ties_even() as i64)
}

/// `_scaled_area`: a pixel COUNT at 1440 lines, which grows with the area.
fn scaled_area(value: i64, frame_h: usize) -> i64 {
    let s = frame_h as f64 / REFERENCE_HEIGHT as f64;
    4.max((value as f64 * (s * s)).round_ties_even() as i64)
}

/// The switches `watch.py` sets on sc_detector.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// `gate`: skip OCR on pairs whose range box does not end in 'km'.
    pub gate: bool,
    /// `haze`: a fourth locator for sunlit haze.
    pub haze: bool,
    /// `dim_range_retry`: read the strip under up to two unpaired names.
    pub dim_range_retry: bool,
    /// Names classified exactly as sc_detector.classify_name, without the
    /// damaged-serial rule.
    pub as_python: bool,
}

impl Options {
    fn classify(&self, raw: &str) -> (NameKind, Option<String>) {
        if self.as_python {
            classify_name_as_python(raw)
        } else {
            classify_name(raw)
        }
    }
}

/// One text line as the OCR found it (sc_detector.OcrLine).
#[derive(Clone, Debug, PartialEq)]
pub struct OcrLine {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub text: String,
    pub conf: f64,
}

impl OcrLine {
    /// make_ocr's `lines`: the box around the quad's corners.
    pub fn from_line(l: &Line) -> OcrLine {
        let xs = l.quad.map(|p| p[0] as f64);
        let ys = l.quad.map(|p| p[1] as f64);
        let (x0, x1) = (
            xs.iter().copied().fold(f64::INFINITY, f64::min),
            xs.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        );
        let (y0, y1) = (
            ys.iter().copied().fold(f64::INFINITY, f64::min),
            ys.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        );
        OcrLine {
            x: x0 as i32,
            y: y0 as i32,
            w: (x1 - x0) as i32,
            h: (y1 - y0) as i32,
            text: l.text.clone(),
            conf: l.score,
        }
    }

    pub fn cx(&self) -> f64 {
        self.x as f64 + self.w as f64 / 2.0
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    fn rect(&self) -> Rect {
        Rect::new(self.x, self.y, self.w, self.h)
    }
}

/// What the reader asks of the OCR (sc_detector.make_ocr's callable).
pub trait Recognise {
    /// `ocr(img)`: the lines' texts, joined.
    fn text(&mut self, img: &Image<u8>) -> Result<String, String>;
    /// `ocr.lines(img)`: the lines with their boxes.
    fn lines(&mut self, img: &Image<u8>) -> Result<Vec<OcrLine>, String>;
}

/// The detector's sizing for a plate crop in this port: the crop's own size
/// (at most 960 px a side), not enlarged until its short side is 736 as
/// RapidOCR does. Measured on the 298 corpus frames (PORT.md, OCR step 5):
/// the same reads of handles seen in three or more frames, fewer handles
/// read only once (39 against 44), about five times faster (0.20 s a frame
/// against 1.04); on test2's answer key one more correct read.
pub const PLATE_LIMIT: Limit = Limit::Max(960);

/// The OCR as this port's legacy reader uses it: plate crops (`text`) with
/// the detector at [`PLATE_LIMIT`], the OCR's own lines (`lines`, the
/// focused read) at `lines`. `&Ocr` alone is RapidOCR's sizing throughout,
/// which the tests hold to the Python.
pub struct Tuned<'a> {
    pub ocr: &'a Ocr,
    plates: DetSettings,
    lines: DetSettings,
}

impl<'a> Tuned<'a> {
    pub fn new(ocr: &'a Ocr, lines: Limit) -> Self {
        Tuned {
            ocr,
            plates: DetSettings {
                limit: PLATE_LIMIT,
                ..ocr.det_settings.clone()
            },
            lines: DetSettings {
                limit: lines,
                ..ocr.det_settings.clone()
            },
        }
    }
}

impl Recognise for Tuned<'_> {
    fn text(&mut self, img: &Image<u8>) -> Result<String, String> {
        Ok(self
            .ocr
            .read_with(img, &self.plates)?
            .into_iter()
            .map(|l| l.text)
            .collect())
    }

    fn lines(&mut self, img: &Image<u8>) -> Result<Vec<OcrLine>, String> {
        Ok(self
            .ocr
            .read_with(img, &self.lines)?
            .iter()
            .map(OcrLine::from_line)
            .collect())
    }
}

impl Recognise for &Ocr {
    fn text(&mut self, img: &Image<u8>) -> Result<String, String> {
        Ocr::text(self, img)
    }

    fn lines(&mut self, img: &Image<u8>) -> Result<Vec<OcrLine>, String> {
        Ok(self.read(img)?.iter().map(OcrLine::from_line).collect())
    }
}

/// Python's `str.isspace()` for one character.
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `RANGE_RE.match`, case-insensitive:
/// `^\s*(\d+(?:\.\d+)?)\s*(km|m)\s*(?:[\[({|].*)?$` - the number's text and
/// whether it is in km. (The OCR's characters hold no newline, which `.`
/// and `$` would treat apart.)
fn range_match(t: &str) -> Option<(String, bool)> {
    let c: Vec<char> = t.chars().collect();
    let n = c.len();
    let mut i = 0;
    let spaces = |i: &mut usize| {
        while *i < n && py_space(c[*i]) {
            *i += 1;
        }
    };
    spaces(&mut i);
    let start = i;
    while i < n && py_digit(c[i]) {
        i += 1;
    }
    if i == start {
        return None;
    }
    if i + 1 < n && c[i] == '.' && py_digit(c[i + 1]) {
        i += 1;
        while i < n && py_digit(c[i]) {
            i += 1;
        }
    }
    let number: String = c[start..i].iter().collect();
    spaces(&mut i);
    let k = |ch: char| matches!(ch, 'k' | 'K' | '\u{212a}');
    let m = |ch: char| matches!(ch, 'm' | 'M');
    let km = if i + 1 < n && k(c[i]) && m(c[i + 1]) {
        i += 2;
        true
    } else if i < n && m(c[i]) {
        i += 1;
        false
    } else {
        return None;
    };
    spaces(&mut i);
    if i < n && !matches!(c[i], '[' | '(' | '{' | '|') {
        return None;
    }
    Some((number, km))
}

/// What sc_detector reads text as: spaces out, upper case.
fn squeeze(text: &str) -> String {
    text.replace(' ', "").to_uppercase()
}

/// The km a matched range states.
fn km_of((number, km): &(String, bool)) -> f64 {
    let ascii: String = number
        .chars()
        .map(|c| match c {
            '\u{ff10}'..='\u{ff19}' => char::from(b'0' + (c as u32 - 0xff10) as u8),
            c => c,
        })
        .collect();
    let value: f64 = ascii.parse().expect("digits with at most one point");
    if *km { value } else { value / 1000.0 }
}

/// `_range_of`: the km a text states, if it reads as a range.
fn range_of(text: &str) -> Option<f64> {
    range_match(&squeeze(text)).map(|m| km_of(&m))
}

/// `_looks_rangey`: worth an OCR call to re-read as a range.
fn looks_rangey(text: &str) -> bool {
    let t = squeeze(text);
    t.contains("KM") || range_match(&t).is_some()
}

/// `prep_for_ocr`: a HUD crop as a document recogniser reads best - grey,
/// three times the size (cubic), stretched to the full range, inverted to
/// dark on light, in a wide white margin.
pub fn prep_for_ocr(crop: &Image<u8>) -> Image<u8> {
    let g = bgr_to_gray(crop);
    let big = resize_cubic_by(&g, OCR_UPSCALE, OCR_UPSCALE);
    let norm = normalize_minmax(&big);
    let pad = OCR_MARGIN_NATIVE * OCR_UPSCALE as usize;
    let (w, h) = (norm.width + 2 * pad, norm.height + 2 * pad);
    let mut img = Image::filled(w, h, 1, 255u8);
    for y in 0..norm.height {
        for (o, &v) in img.row_mut(y + pad)[pad..pad + norm.width]
            .iter_mut()
            .zip(norm.row(y))
        {
            *o = 255 - v;
        }
    }
    gray_to_bgr(&img)
}

/// `cv2.normalize(img, None, 0, 255, NORM_MINMAX)` on 8 bits: the scale and
/// shift in double, applied in float with a fused multiply-add and rounded
/// to even (OpenCV 5.0's vector convertTo).
fn normalize_minmax(img: &Image<u8>) -> Image<u8> {
    let smin = img.data.iter().copied().min().unwrap_or(0) as f64;
    let smax = img.data.iter().copied().max().unwrap_or(0) as f64;
    let scale = 255.0
        * if smax - smin > f64::EPSILON {
            1.0 / (smax - smin)
        } else {
            0.0
        };
    let shift = 0.0 - smin * scale;
    let (a, b) = (scale as f32, shift as f32);
    let data = img
        .data
        .iter()
        .map(|&v| (v as f32).mul_add(a, b).round_ties_even().clamp(0.0, 255.0) as u8)
        .collect();
    Image::from_vec(img.width, img.height, 1, data)
}

/// Sorts contacts nearest first (`range_km or 1e9`: none, or zero, last).
fn by_range(found: &mut [Contact]) {
    let key = |c: &Contact| match c.range_km {
        Some(k) if k != 0.0 => k,
        _ => 1e9,
    };
    found.sort_by(|a, b| {
        key(a)
            .partial_cmp(&key(b))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// The strip under a name where its range line would have to be (x0, y0,
/// x1, y1), inside an image `w` x `h`; None when too thin to read.
fn band_under(
    nx: i32,
    ny_bottom: i32,
    nw: i32,
    nh: i32,
    w: i32,
    h: i32,
    frame_h: usize,
) -> Option<(i32, i32, i32, i32)> {
    let gap = scaled(PAIR_MIN_GAP, frame_h) as i32;
    let pad_x = 12.max(nw.div_euclid(6));
    let y0 = 0.max(ny_bottom + gap - scaled(6, frame_h) as i32);
    let y1 = h.min(ny_bottom + gap + nh + scaled(10, frame_h) as i32);
    let x0 = 0.max(nx - pad_x);
    let x1 = w.min(nx + nw + pad_x);
    (y1 - y0 >= 6 && x1 - x0 >= 10).then_some((x0, y0, x1, y1))
}

/// `_retry_range` / `_retry_range_box`: OCR the strip under a name; its km
/// and the strip, when it reads as a range.
fn retry_range(
    img: &Image<u8>,
    name: Rect,
    ocr: &mut dyn Recognise,
    frame_h: usize,
) -> Result<Option<(f64, Rect)>, String> {
    let Some((x0, y0, x1, y1)) = band_under(
        name.x,
        name.y + name.h,
        name.w,
        name.h,
        img.width as i32,
        img.height as i32,
        frame_h,
    ) else {
        return Ok(None);
    };
    let txt = ocr.text(&prep_for_ocr(&slice(img, y0, y1, x0, x1)))?;
    Ok(range_match(&squeeze(&txt)).map(|m| (km_of(&m), Rect::new(x0, y0, x1 - x0, y1 - y0))))
}

/// `_orphan_names`: boxes no pair used, wide and tall enough for a name,
/// best first by how much the strip under them ends in 'km'.
fn orphan_names(
    boxes: &[LegacyBox],
    used: &[usize],
    frame: &Image<u8>,
    frame_h: usize,
) -> Vec<usize> {
    let min_w = scaled(BOX_MIN_W, frame_h) * 2;
    let (lo, hi) = (scaled(GLYPH_MIN_H, frame_h), scaled(GLYPH_MAX_H, frame_h));
    let mut scored: Vec<(f64, usize)> = Vec::new();
    for (i, b) in boxes.iter().enumerate() {
        if used.contains(&i) || (b.w as i64) < min_w || !(lo <= b.h as i64 && b.h as i64 <= hi) {
            continue;
        }
        let Some((x0, y0, x1, y1)) = band_under(
            b.x,
            b.bottom(),
            b.w,
            b.h,
            frame.width as i32,
            frame.height as i32,
            frame_h,
        ) else {
            continue;
        };
        let crop = slice(frame, y0, y1, x0, x1);
        let score = if crop.data.is_empty() {
            0.0
        } else {
            km_gate::score(&crop, km_gate::template())
        };
        scored.push((score, i));
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().map(|(_, i)| i).collect()
}

/// `detect`: the full read of `frame` (a whole screen, or a crop of one
/// `frame_h` tall), at most `max_pairs` pairs read.
pub fn detect(
    frame: &Image<u8>,
    ocr: &mut dyn Recognise,
    frame_h: usize,
    max_pairs: usize,
    opt: &Options,
) -> Result<Vec<Contact>, String> {
    let boxes = find_all_boxes(frame, frame_h, opt.haze);
    let mut candidates = pair_boxes(&boxes, frame_h);
    if candidates.len() > max_pairs {
        let mut scored: Vec<(f64, usize, (usize, usize))> = candidates
            .iter()
            .enumerate()
            .map(|(i, &(n, r))| {
                (
                    range_shape_score(&boxes[r].crop(frame, CROP_PAD)),
                    i,
                    (n, r),
                )
            })
            .collect();
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.1.cmp(&b.1))
        });
        candidates = scored[..max_pairs].iter().map(|s| s.2).collect();
    }

    let mut confirmed = Vec::new();
    let mut used = Vec::new();
    for (n, r) in candidates {
        let range_crop = boxes[r].crop(frame, CROP_PAD);
        if opt.gate && km_gate::score(&range_crop, km_gate::template()) < KM_GATE_THRESHOLD {
            continue;
        }
        let text = ocr.text(&prep_for_ocr(&range_crop))?;
        let Some(km) = range_of(&text) else {
            continue;
        };
        let raw = ocr.text(&prep_for_ocr(&boxes[n].crop(frame, CROP_PAD)))?;
        let (kind, name) = opt.classify(&raw);
        confirmed.push(Contact {
            name,
            kind,
            range_km: Some(km),
            name_box: boxes[n].rect(),
            range_box: boxes[r].rect(),
        });
        used.extend([n, r]);
    }

    if opt.dim_range_retry {
        let orphans = orphan_names(&boxes, &used, frame, frame_h);
        for &i in orphans.iter().take(ORPHAN_RETRY_WHEN_ON) {
            let name = boxes[i];
            let Some((km, rbox)) = retry_range(frame, name.rect(), ocr, frame_h)? else {
                continue;
            };
            let raw = ocr.text(&prep_for_ocr(&name.crop(frame, CROP_PAD)))?;
            let (kind, cleaned) = opt.classify(&raw);
            if !matches!(kind, NameKind::Player | NameKind::ShipSerial) || cleaned.is_none() {
                continue;
            }
            confirmed.push(Contact {
                name: cleaned,
                kind,
                range_km: Some(km),
                name_box: name.rect(),
                range_box: rbox,
            });
        }
    }
    by_range(&mut confirmed);
    Ok(confirmed)
}

/// `_resolve_range`: the range a line states, re-read from a padded crop
/// when the reading looks truncated (no match, or no decimal point).
fn resolve_range(
    line: &OcrLine,
    sub: &Image<u8>,
    ocr: &mut dyn Recognise,
    frame_h: usize,
) -> Result<Option<f64>, String> {
    let m = range_match(&squeeze(&line.text));
    let direct = m.as_ref().map(km_of);
    let suspicious = m.as_ref().is_none_or(|(number, _)| !number.contains('.'));
    if !suspicious {
        return Ok(direct);
    }
    let pad = scaled(RANGE_REREAD_PAD, frame_h) as i32;
    let y0 = 0.max(line.y - pad.div_euclid(2));
    let y1 = (sub.height as i32).min(line.bottom() + pad.div_euclid(2));
    let x0 = 0.max(line.x - pad);
    let x1 = (sub.width as i32).min(line.x + line.w + pad);
    if y1 - y0 < 6 || x1 - x0 < 10 {
        return Ok(direct);
    }
    let again = range_of(&ocr.text(&prep_for_ocr(&slice(sub, y0, y1, x0, x1)))?);
    Ok(again.or(direct))
}

/// `_pair_ocr_lines`: the contact geometry on the OCR's own lines - a name
/// is a contact only with a range line under it, centred. Each contact
/// comes with the index of its name line.
fn pair_ocr_lines(
    lines: &[OcrLine],
    frame_h: usize,
    sub: &Image<u8>,
    ocr: &mut dyn Recognise,
    opt: &Options,
) -> Result<Vec<(Contact, usize)>, String> {
    let gap_hi = scaled(PAIR_MAX_GAP, frame_h) + scaled(6, frame_h);
    let gap_lo = -scaled(6, frame_h);
    let near = scaled(8, frame_h) as f64;
    let mut out = Vec::new();
    for (ri, rng) in lines.iter().enumerate() {
        if !looks_rangey(&rng.text) {
            continue;
        }
        let names: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|&(ni, name)| {
                let gap = (rng.y - name.bottom()) as i64;
                ni != ri
                    && gap_lo <= gap
                    && gap <= gap_hi
                    && (name.cx() - rng.cx()).abs() <= near.max(name.w as f64 * 0.35)
            })
            .map(|(ni, _)| ni)
            .collect();
        if names.is_empty() {
            continue;
        }
        let Some(km) = resolve_range(rng, sub, ocr, frame_h)? else {
            continue;
        };
        for ni in names {
            let (kind, name) = opt.classify(&lines[ni].text);
            out.push((
                Contact {
                    name,
                    kind,
                    range_km: Some(km),
                    name_box: lines[ni].rect(),
                    range_box: rng.rect(),
                },
                ni,
            ));
        }
    }
    Ok(out)
}

/// `focus_size_for`: the focused read's crop for a screen this tall
/// (1200 x 600 at 1440 lines).
pub fn focus_size_for(frame_h: usize) -> (usize, usize) {
    let s = frame_h as f64 / REFERENCE_HEIGHT as f64;
    (
        320.max((FOCUS_W * s).round_ties_even() as usize),
        200.max((FOCUS_H * s).round_ties_even() as usize),
    )
}

/// `focus_crop`: the centre of the screen, biased upward (labels sit above
/// their ship), and where it starts.
pub fn focus_crop(frame: &Image<u8>, w: usize, h: usize) -> (Image<u8>, usize, usize) {
    let (fw, fh) = (frame.width as i64, frame.height as i64);
    let (w, h) = ((w as i64).min(fw), (h as i64).min(fh));
    let x0 = 0.max((fw - w).min(fw.div_euclid(2) - w.div_euclid(2)));
    let y0 = 0.max((fh - h).min(fh.div_euclid(2) - (h as f64 * FOCUS_UP) as i64));
    (
        slice(
            frame,
            y0 as i32,
            (y0 + h) as i32,
            x0 as i32,
            (x0 + w) as i32,
        ),
        x0 as usize,
        y0 as usize,
    )
}

/// `detect_focused`: the centre of the screen, in screen coordinates.
/// `size` None sizes the crop from the screen ([`focus_size_for`]);
/// `direct` (the default; off is the `focus_mask` setting) reads it with
/// the OCR's own detector rather than the masks.
pub fn detect_focused(
    frame: &Image<u8>,
    ocr: &mut dyn Recognise,
    size: Option<(usize, usize)>,
    direct: bool,
    opt: &Options,
) -> Result<Vec<Contact>, String> {
    let (w, h) = size.unwrap_or_else(|| focus_size_for(frame.height));
    let (sub, x0, y0) = focus_crop(frame, w, h);
    let mut found = if direct {
        // Plain grey, no inversion or enlarging: the OCR's own detector
        // prefers the untouched image.
        let grey = gray_to_bgr(&bgr_to_gray(&sub));
        let lines = ocr.lines(&grey)?;
        let paired = pair_ocr_lines(&lines, frame.height, &sub, ocr, opt)?;
        let used: Vec<usize> = paired.iter().map(|(_, i)| *i).collect();
        let mut found: Vec<Contact> = paired.into_iter().map(|(c, _)| c).collect();

        // Names whose range line the detector missed: the strip under the
        // few nearest the centre, read on its own.
        let mid = sub.width as f64 / 2.0;
        let mut retry: Vec<&OcrLine> = lines
            .iter()
            .enumerate()
            .filter(|&(i, ln)| {
                !used.contains(&i)
                    && (ln.cx() - mid).abs() <= RANGE_RETRY_CENTRE_FRAC * sub.width as f64
                    && matches!(
                        opt.classify(&ln.text),
                        (NameKind::Player | NameKind::ShipSerial, Some(_))
                    )
            })
            .map(|(_, ln)| ln)
            .collect();
        retry.sort_by(|a, b| {
            (a.cx() - mid)
                .abs()
                .partial_cmp(&(b.cx() - mid).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for ln in retry.into_iter().take(RANGE_RETRY_MAX) {
            let Some((km, rbox)) = retry_range(&sub, ln.rect(), ocr, frame.height)? else {
                continue;
            };
            let (kind, name) = opt.classify(&ln.text);
            found.push(Contact {
                name,
                kind,
                range_km: Some(km),
                name_box: ln.rect(),
                range_box: rbox,
            });
        }
        found
    } else {
        detect(&sub, ocr, frame.height, FOCUS_MAX_PAIRS, opt)?
    };
    for c in &mut found {
        for b in [&mut c.name_box, &mut c.range_box] {
            b.x += x0 as i32;
            b.y += y0 as i32;
        }
    }
    by_range(&mut found);
    Ok(found)
}

/// `merge_contacts`: a full read and a focused read of the same frame. The
/// focused read wins any label both found - by position, within 24 px at
/// 1440 lines, never by name (the second read exists because the first may
/// have misread it).
pub fn merge_contacts(wide: Vec<Contact>, centre: Vec<Contact>, frame_h: usize) -> Vec<Contact> {
    if centre.is_empty() {
        return wide;
    }
    if wide.is_empty() {
        return centre;
    }
    let near = scaled(24, frame_h) as i32;
    let same = |a: &Contact, b: &Contact| {
        (a.name_box.x - b.name_box.x).abs() <= near && (a.name_box.y - b.name_box.y).abs() <= near
    };
    let mut out = centre.clone();
    for c in wide {
        if !centre.iter().any(|k| same(&c, k)) {
            out.push(c);
        }
    }
    out
}

/// watch.cheap_pass: the monitor-rate pass of the auto trigger - the
/// bright locator's pairs, no OCR (with `gate`, only those whose range box
/// ends in 'km'), as (name, range) boxes.
pub fn cheap_pass(frame: &Image<u8>, gate: bool) -> Vec<(Rect, Rect)> {
    let mask = hud_mask(frame);
    let boxes = find_boxes(&mask, mask.height);
    pair_boxes(&boxes, frame.height)
        .into_iter()
        .filter(|&(_, r)| {
            !gate
                || km_gate::score(&boxes[r].crop(frame, CROP_PAD), km_gate::template())
                    >= KM_GATE_THRESHOLD
        })
        .map(|(n, r)| (boxes[n].rect(), boxes[r].rect()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_read_as_range_re_reads_them() {
        let km = |t: &str| range_of(t);
        assert_eq!(km("1.8km"), Some(1.8));
        assert_eq!(km(" 6.2 KM [-8m/s]"), Some(6.2));
        assert_eq!(km("450m"), Some(0.45));
        assert_eq!(km("12km(x"), Some(12.0));
        assert_eq!(km("\u{ff11}.\u{ff15}km"), Some(1.5));
        assert_eq!(km("3.34M/3.79M"), None);
        assert_eq!(km(".9km"), None);
        assert_eq!(km("9.km"), None);
        assert_eq!(km("5kmx"), None);
        assert_eq!(km("km"), None);
        assert!(looks_rangey("x KM y"));
        assert!(!looks_rangey("DECOY"));
    }

    #[test]
    fn sizes_scale_with_the_screen() {
        assert_eq!(scaled(25, 1440), 25);
        assert_eq!(scaled(7, 1080), 5);
        assert_eq!(scaled_area(25, 1080), 14);
        assert_eq!(focus_size_for(1440), (1200, 600));
        assert_eq!(focus_size_for(720), (600, 300));
    }

    #[test]
    fn the_centre_read_wins_where_both_found_a_label() {
        let c = |x: i32, n: &str| Contact {
            name: Some(n.into()),
            kind: NameKind::Player,
            range_km: Some(1.0),
            name_box: Rect::new(x, 100, 50, 12),
            range_box: Rect::new(x, 120, 40, 12),
        };
        let out = merge_contacts(
            vec![c(100, "WIDE"), c(900, "FAR")],
            vec![c(110, "CENTRE")],
            1440,
        );
        let names: Vec<_> = out.iter().map(|c| c.name.clone().unwrap()).collect();
        assert_eq!(names, ["CENTRE", "FAR"]);
    }
}
