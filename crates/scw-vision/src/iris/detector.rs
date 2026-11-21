//! IRIS's nameplate FINDER (`NameplateDetector`): a colour and contrast mask
//! of HUD text, text lines from its components, speed-group unions, narrow
//! rows, distance/name pairs, stacked name rows, and the isolation and crowd
//! rules. Ported from sc-watch's Python port (iris_detector.py), which
//! matches IRIS's own output on the real corpus, with names kept close to
//! the C# so each function can be compared with its original.
//!
//! The Python keeps the frame's intermediate images on the detector between
//! calls; here they live in a [`MaskState`] made by each call, so a detector
//! holds only its settings and can be shared.

use std::cell::OnceCell;
use std::collections::{BTreeSet, HashMap, HashSet};

use crate::{Image, Rect, cv};

/// `NameplateDetectorSettings` / `PipelineSettings`, IRIS's defaults.
/// Pixel values are for a 1440-line frame and scale with the frame.
#[derive(Clone, Debug, PartialEq)]
pub struct DetectorSettings {
    pub region_left: f64,
    pub region_right: f64,
    pub region_top: f64,
    pub region_bottom: f64,
    pub reference_height: i32,
    /// The height pixel values scale by; 0 = the image's own.
    pub scale_height: i32,
    pub min_name_length: usize,
    pub max_name_length: usize,
    pub min_name_score: f64,
    pub clear_lines: f64,
    pub crowd_exception: bool,
    pub speck_area: f64,
    pub center_tolerance: f64,
    pub left_tolerance: f64,
    pub left_width_difference: f64,
    pub max_rows: usize,
    pub row_gap_max: f64,
    pub row_pitch_min: f64,
    pub row_pitch_max: f64,
    pub row_min_caps: f64,
    pub stack_tint_tolerance: f64,
    pub narrow_min_width: f64,
    pub narrow_max_lines: f64,
    pub speed_join_gap: f64,
    pub speed_join_max_lines: f64,
    pub read_crop_min_height: f64,
    pub read_crop_min_width: f64,
    pub near_height_tolerance: f64,
    pub near_height_min_px: f64,
    pub anchor_max_offset_px: f64,
    pub anchor_max_row_offset_lines: f64,
    pub top_gap_max_lines: f64,
    pub top_height_diff_min_px: f64,
    pub top_height_diff_lines: f64,
    pub top_centre_tolerance_px: f64,
    pub top_left_tolerance_px: f64,
    pub top_left_width_difference_px: f64,
    pub top_hat_kernel_px: f64,
    pub top_hat_min: f64,
    pub green_top_hat_min: f64,
    pub white_value_min: f64,
    pub red_min: f64,
    pub red_over_green_min: f64,
    pub red_over_blue_min: f64,
    pub green_min: f64,
    pub green_over_red_min: f64,
    pub green_over_blue_min: f64,
    pub bg_dark_max: f64,
    pub bg_full_contrast: f64,
    pub contrast_hat_max: f64,
    pub background_shrink_px: f64,
    pub background_blur: usize,
    pub line_max_height: f64,
    pub line_min_height: f64,
    pub line_min_width: f64,
    pub line_aspect_min: f64,
    pub join_width_px: f64,
    pub fill_min: f64,
    pub fill_max: f64,
    pub height_tolerance_px: f64,
    pub height_tolerance_lines: f64,
    pub row_tolerance_px: f64,
    pub row_tolerance_lines: f64,
    pub row_join_top_px: f64,
    pub row_join_height_px: f64,
    pub row_join_gap_px: f64,
    pub max_line_width: f64,
    pub pair_gap_max_lines: f64,
}

impl Default for DetectorSettings {
    fn default() -> Self {
        Self {
            region_left: 0.15,
            region_right: 0.85,
            region_top: 0.10,
            region_bottom: 0.70,
            reference_height: 1440,
            scale_height: 0,
            min_name_length: 4,
            max_name_length: 64,
            min_name_score: -0.20,
            clear_lines: 3.0,
            crowd_exception: true,
            speck_area: 4.0,
            center_tolerance: 5.0,
            left_tolerance: 2.0,
            left_width_difference: 8.0,
            max_rows: 3,
            row_gap_max: 0.8,
            row_pitch_min: 1.05,
            row_pitch_max: 1.8,
            row_min_caps: 11.0,
            stack_tint_tolerance: 0.10,
            narrow_min_width: 6.0,
            narrow_max_lines: 2.2,
            speed_join_gap: 1.5,
            speed_join_max_lines: 8.0,
            read_crop_min_height: 30.0,
            read_crop_min_width: 60.0,
            near_height_tolerance: 0.35,
            near_height_min_px: 2.0,
            anchor_max_offset_px: 8.0,
            anchor_max_row_offset_lines: 0.8,
            top_gap_max_lines: 1.4,
            top_height_diff_min_px: 4.0,
            top_height_diff_lines: 0.45,
            top_centre_tolerance_px: 5.0,
            top_left_tolerance_px: 2.0,
            top_left_width_difference_px: 8.0,
            top_hat_kernel_px: 11.0,
            top_hat_min: 28.0,
            green_top_hat_min: 20.0,
            white_value_min: 150.0,
            red_min: 140.0,
            red_over_green_min: 80.0,
            red_over_blue_min: 60.0,
            green_min: 90.0,
            green_over_red_min: 35.0,
            green_over_blue_min: 30.0,
            bg_dark_max: 100.0,
            bg_full_contrast: 180.0,
            contrast_hat_max: 45.0,
            background_shrink_px: 4.0,
            background_blur: 15,
            line_max_height: 22.0,
            line_min_height: 10.0,
            line_min_width: 24.0,
            line_aspect_min: 1.5,
            join_width_px: 7.0,
            fill_min: 0.12,
            fill_max: 0.75,
            height_tolerance_px: 4.0,
            height_tolerance_lines: 0.45,
            row_tolerance_px: 3.0,
            row_tolerance_lines: 0.25,
            row_join_top_px: 2.0,
            row_join_height_px: 3.0,
            row_join_gap_px: 4.0,
            max_line_width: 832.0,
            pair_gap_max_lines: 1.4,
        }
    }
}

impl DetectorSettings {
    /// A 1440-line pixel value at this frame height, at least 1.
    pub fn scale(&self, frame_height: i32, value: f64) -> i32 {
        ((value * frame_height as f64 / self.reference_height as f64 + 0.5).floor() as i32).max(1)
    }

    pub fn effective_scale_height(&self, image_height: i32) -> i32 {
        if self.scale_height > 0 {
            self.scale_height
        } else {
            image_height
        }
    }

    pub fn context_padding(&self, sh: i32) -> i32 {
        3 * self.scale(sh, self.line_max_height) + self.scale(sh, self.height_tolerance_px) + 2
    }

    pub fn vertical_context_padding(&self, sh: i32) -> i32 {
        self.scale(sh, self.top_hat_kernel_px) + 2
    }
}

// ---- rectangles and masks ------------------------------------------------------------

fn padded(b: Rect, size: i32, cols: i32, rows: i32) -> Rect {
    let pad = 2 * size + 2;
    let (left, top) = ((b.x - pad).max(0), (b.y - pad).max(0));
    let (right, bottom) = ((b.x + b.w + pad).min(cols), (b.y + b.h + pad).min(rows));
    Rect::new(left, top, right - left, bottom - top)
}

fn union(a: Rect, b: Rect) -> Rect {
    let (left, top) = (a.x.min(b.x), a.y.min(b.y));
    Rect::new(
        left,
        top,
        (a.x + a.w).max(b.x + b.w) - left,
        (a.y + a.h).max(b.y + b.h) - top,
    )
}

fn contains(outer: Rect, inner: Rect) -> bool {
    outer.w > 0
        && outer.h > 0
        && inner.x >= outer.x
        && inner.y >= outer.y
        && inner.x + inner.w <= outer.x + outer.w
        && inner.y + inner.h <= outer.y + outer.h
}

fn area(r: Rect) -> i64 {
    r.w as i64 * r.h as i64
}

fn bounding(mask: &Image<u8>) -> Rect {
    let (x, y, w, h) = cv::bounding_rect(mask);
    Rect::new(x as i32, y as i32, w as i32, h as i32)
}

/// `img[r]` as numpy slices it: clipped to the image.
fn sub<T: Copy + Default>(img: &Image<T>, r: Rect) -> Image<T> {
    let x0 = r.x.clamp(0, img.width as i32) as usize;
    let y0 = r.y.clamp(0, img.height as i32) as usize;
    let x1 = (r.x + r.w).clamp(x0 as i32, img.width as i32) as usize;
    let y1 = (r.y + r.h).clamp(y0 as i32, img.height as i32) as usize;
    img.crop(x0, y0, x1 - x0, y1 - y0)
}

/// Applies `f(dst, src)` to every pixel of `dst[r]` with the matching pixel of `src`.
fn update_region(dst: &mut Image<u8>, r: Rect, src: &Image<u8>, f: impl Fn(&mut u8, u8)) {
    for y in 0..src.height {
        let row = &mut dst.row_mut(r.y as usize + y)[r.x as usize..r.x as usize + src.width];
        for (d, &s) in row.iter_mut().zip(src.row(y)) {
            f(d, s);
        }
    }
}

fn threshold(src: &Image<u8>, t: f64) -> Image<u8> {
    cv::threshold_binary(src, t, 255.0)
}

/// `threshold(v, t, 255) != 0` as a predicate - cv::threshold's rule, edges included.
fn above(t: f64) -> impl Fn(u8) -> bool {
    let it = t.floor() as i64;
    move |v| it < 0 || (it < 255 && v as i64 > it)
}

/// The top-hat of `parent[r]` as OpenCV computes it on a region of a larger
/// image: the erosion sees the parent's pixels around `r`, the dilation that
/// follows works on a fresh image and cannot.
fn tophat_roi(parent: &Image<u8>, r: Rect, k: i32) -> Image<u8> {
    let half = k / 2;
    let (pw, ph) = (parent.width as i32, parent.height as i32);
    let (x0, y0) = ((r.x - half).max(0), (r.y - half).max(0));
    let (x1, y1) = ((r.x + r.w + half).min(pw), (r.y + r.h + half).min(ph));
    let ku = k as usize;
    let eroded = cv::erode(&sub(parent, Rect::new(x0, y0, x1 - x0, y1 - y0)), ku, ku);
    let eroded = sub(&eroded, Rect::new(r.x - x0, r.y - y0, r.w, r.h));
    let opened = cv::dilate(&eroded, ku, ku);
    cv::subtract(&sub(parent, r), &opened)
}

/// Line indices sorted by top, to find the lines just below a shape.
struct LinesByTop {
    order: Vec<usize>,
    tops: Vec<i32>,
    pair_gap: f64,
}

impl LinesByTop {
    fn new(lines: &[Rect], pair_gap: f64) -> Self {
        let mut order: Vec<usize> = (0..lines.len()).collect();
        order.sort_by_key(|&i| lines[i].y);
        let tops = order.iter().map(|&i| lines[i].y).collect();
        Self {
            order,
            tops,
            pair_gap,
        }
    }

    /// Lines whose top is below the shape's bottom by at most the pair gap,
    /// in index order.
    fn below(&self, shape: Rect) -> Vec<usize> {
        let bottom = shape.y + shape.h;
        let farthest = bottom + (shape.h as f64 * self.pair_gap).floor() as i32;
        let lo = self.tops.partition_point(|&t| t < bottom + 1);
        let mut out: Vec<usize> = (lo..self.tops.len())
            .take_while(|&i| self.tops[i] <= farthest)
            .map(|i| self.order[i])
            .collect();
        out.sort_unstable();
        out
    }
}

/// Name rows stacked over a distance line: the rows (top first) and the
/// rows that broke the stack.
pub type Stack = (Vec<Rect>, Vec<Rect>);

/// What the finder found in one image, in that image's coordinates.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Candidates {
    /// Every text line: the plain ones, promoted narrow ones, speed unions.
    pub boxes: Vec<Rect>,
    /// (name line, distance line) pairs clear of other ink.
    pub pairs: Vec<(usize, usize)>,
    /// Pairs with ink beside them.
    pub loose: Vec<(usize, usize)>,
    /// Pairs whose neighbours all belong to other pairs: (top, bottom, neighbours).
    pub crowd: Vec<(usize, usize, Vec<usize>)>,
    pub stacks: HashMap<(Rect, Rect), Stack>,
}

impl Candidates {
    pub fn stack_of(&self, top: Rect, bottom: Rect) -> Option<&Stack> {
        self.stacks.get(&(top, bottom))
    }
}

/// One image's masks while it is searched.
struct MaskState {
    value: Image<u8>,
    value_hat: Option<Image<u8>>,
    value_hat_area: Rect,
    background: Option<Image<u8>>,
    bright: Option<Image<u8>>,
    mask: Image<u8>,
    labels: Image<i32>,
    solid: Vec<bool>,
    fill_mask: Image<u8>,
}

impl MaskState {
    fn solid_at(&self, x: usize, y: usize) -> bool {
        self.solid[self.labels.data[y * self.labels.width + x] as usize]
    }

    /// Whether any solid component touches columns `x0..x1`, rows `y0..y1`.
    fn solid_any(&self, x0: i32, y0: i32, x1: i32, y1: i32) -> bool {
        (y0..y1).any(|y| (x0..x1).any(|x| self.solid_at(x as usize, y as usize)))
    }
}

// ---- the detector ---------------------------------------------------------------------

pub struct NameplateDetector {
    pub settings: DetectorSettings,
    /// The top-hat an ink pixel needs over a bright background, by its brightness.
    hat_limits: [u8; 256],
}

impl NameplateDetector {
    pub fn new(settings: DetectorSettings) -> Self {
        let s = &settings;
        let mut hat_limits = [0u8; 256];
        for (bg, limit) in hat_limits.iter_mut().enumerate() {
            let t = s.top_hat_min
                + (bg as f64 - s.bg_dark_max) / (s.bg_full_contrast - s.bg_dark_max)
                    * (s.contrast_hat_max - s.top_hat_min);
            let t = t.clamp(s.top_hat_min, s.contrast_hat_max);
            *limit = t.floor().clamp(0.0, 255.0) as u8;
        }
        Self {
            settings,
            hat_limits,
        }
    }

    fn scaled(&self, fh: i32, v: f64) -> i32 {
        self.settings.scale(fh, v)
    }

    fn scaled_odd(&self, fh: i32, v: f64) -> i32 {
        let k = self.scaled(fh, v);
        if k % 2 == 1 { k } else { k + 1 }
    }

    // -- the mask (NameplateDetector.Mask) -------------------------------------

    /// The text mask of `bgr`'s region (x0, y0)..(x1, y1).
    fn build_color_mask(
        &self,
        bgr: &Image<u8>,
        region: (usize, usize, usize, usize),
        sh: i32,
    ) -> MaskState {
        let s = &self.settings;
        let tophat = self.scaled_odd(sh, s.top_hat_kernel_px);
        // The three channels and their maximum, in one pass over the frame.
        let (x0, y0, x1, y1) = region;
        let (w, h, cn) = (x1 - x0, y1 - y0, bgr.channels);
        let (mut blue, mut green, mut red, mut value) = (
            Image::<u8>::new(w, h, 1),
            Image::<u8>::new(w, h, 1),
            Image::<u8>::new(w, h, 1),
            Image::<u8>::new(w, h, 1),
        );
        for y in 0..h {
            let src = &bgr.row(y0 + y)[x0 * cn..x1 * cn];
            let o = y * w;
            for (x, p) in src.chunks_exact(cn).enumerate() {
                let (b, g, r) = (p[0], p[1], p[2]);
                blue.data[o + x] = b;
                green.data[o + x] = g;
                red.data[o + x] = r;
                value.data[o + x] = b.max(g).max(r);
            }
        }
        let (cols, rows) = (value.width as i32, value.height as i32);
        let mut st = MaskState {
            value,
            value_hat: None,
            value_hat_area: Rect::default(),
            background: None,
            bright: None,
            mask: Image::new(0, 0, 1),
            labels: Image::new(0, 0, 1),
            solid: Vec::new(),
            fill_mask: Image::new(0, 0, 1),
        };
        let bright_area = self.bright_area(&mut st, tophat, sh);

        let mut cyan_white = threshold(&st.value, s.white_value_min);
        let b = bounding(&cyan_white);
        if b.w > 0 && b.h > 0 {
            let area = padded(b, tophat, cols, rows);
            let mut hat_area = area;
            if let Some(br) = bright_area {
                let u = union(area, br);
                if self::area(u) <= self::area(area) + self::area(br) {
                    hat_area = u;
                }
            }
            let value_hat = tophat_roi(&st.value, hat_area, tophat);
            let hat = sub(
                &value_hat,
                Rect::new(area.x - hat_area.x, area.y - hat_area.y, area.w, area.h),
            );
            let bits = threshold(&hat, s.top_hat_min);
            update_region(&mut cyan_white, area, &bits, |d, s| *d &= s);
            st.value_hat = Some(value_hat);
            st.value_hat_area = hat_area;
        }

        // threshold(c) & threshold(c - o1) & threshold(c - o2), per pixel.
        let hue_mask = |c: &Image<u8>, o1: &Image<u8>, o2: &Image<u8>, t: [f64; 3]| {
            let [a, b, d] = t.map(above);
            let data = (0..c.data.len())
                .map(|i| {
                    let v = c.data[i];
                    let on =
                        a(v) && b(v.saturating_sub(o1.data[i])) && d(v.saturating_sub(o2.data[i]));
                    if on { 255 } else { 0 }
                })
                .collect();
            Image::from_vec(c.width, c.height, 1, data)
        };
        let mut red_mask = hue_mask(
            &red,
            &green,
            &blue,
            [s.red_min, s.red_over_green_min, s.red_over_blue_min],
        );
        self.apply_tophat(&red, &mut red_mask, tophat, s.top_hat_min);
        let mut green_mask = hue_mask(
            &green,
            &red,
            &blue,
            [s.green_min, s.green_over_red_min, s.green_over_blue_min],
        );
        self.apply_tophat(&green, &mut green_mask, tophat, s.green_top_hat_min);

        let data = (0..cyan_white.data.len())
            .map(|i| cyan_white.data[i] | red_mask.data[i] | green_mask.data[i])
            .collect();
        st.mask = Image::from_vec(cyan_white.width, cyan_white.height, 1, data);
        if let Some(br) = bright_area {
            self.apply_contrast_rule(&mut st, br, tophat);
        }
        st
    }

    /// The bright background, blurred at low resolution; its padded bounds.
    fn bright_area(&self, st: &mut MaskState, tophat: i32, sh: i32) -> Option<Rect> {
        let s = &self.settings;
        let v = &st.value;
        let shrink = self.scaled(sh, s.background_shrink_px) as usize;
        let sw = v.width.div_ceil(shrink).max(1);
        let shh = v.height.div_ceil(shrink).max(1);
        let small = cv::resize_area_to(v, sw, shh);
        let blur = cv::blur(&small, s.background_blur);
        if *blur.data.iter().max().unwrap_or(&0) as f64 <= s.bg_dark_max {
            return None;
        }
        let background = cv::resize_linear_to(&blur, v.width, v.height);
        let bright = threshold(&background, s.bg_dark_max);
        let b = bounding(&bright);
        st.background = Some(background);
        st.bright = Some(bright);
        if b.w <= 0 || b.h <= 0 {
            return None;
        }
        Some(padded(b, tophat, v.width as i32, v.height as i32))
    }

    fn apply_tophat(&self, channel: &Image<u8>, plain: &mut Image<u8>, size: i32, minimum: f64) {
        let b = bounding(plain);
        if b.w <= 0 || b.h <= 0 {
            return;
        }
        let area = padded(b, size, plain.width as i32, plain.height as i32);
        let hat = tophat_roi(channel, area, size);
        let bits = threshold(&hat, minimum);
        update_region(plain, area, &bits, |d, s| *d &= s);
    }

    /// Over a bright background, ink is what stands out by more than the
    /// background's own limit; the plain mask there is replaced.
    fn apply_contrast_rule(&self, st: &mut MaskState, area: Rect, tophat: i32) {
        let hat = match &st.value_hat {
            Some(vh) if contains(st.value_hat_area, area) => {
                let h = st.value_hat_area;
                sub(vh, Rect::new(area.x - h.x, area.y - h.y, area.w, area.h))
            }
            _ => tophat_roi(&st.value, area, tophat),
        };
        let background = st
            .background
            .as_ref()
            .expect("a bright area has a background");
        let limit = cv::lut(&sub(background, area), &self.hat_limits);
        let bright = sub(st.bright.as_ref().unwrap(), area);
        for y in 0..bright.height {
            let row = &mut st.mask.row_mut(area.y as usize + y)
                [area.x as usize..area.x as usize + bright.width];
            for (x, m) in row.iter_mut().enumerate() {
                let i = y * bright.width + x;
                if bright.data[i] != 0 {
                    *m = if hat.data[i] > limit.data[i] { 255 } else { 0 };
                }
            }
        }
    }

    // -- text lines (NameplateDetector.Lines) -----------------------------------

    fn find_text_lines(&self, st: &mut MaskState, fh: i32, narrow: &mut Vec<Rect>) -> Vec<Rect> {
        let s = &self.settings;
        let max_h = self.scaled(fh, s.line_max_height);
        let min_h = self.scaled(fh, s.line_min_height);
        let min_w = self.scaled(fh, s.line_min_width);
        let max_w = self.scaled(fh, s.max_line_width);
        let min_narrow_w = self.scaled(fh, s.narrow_min_width);
        let ratio = fh as f64 / s.reference_height as f64;
        let speck = ((s.speck_area * (ratio * ratio) + 0.5).floor() as i64).max(1);
        let (labels, stats) = cv::connected_components(&st.mask, 8);
        st.labels = labels;
        if stats.len() <= 1 {
            st.solid = vec![false; stats.len().max(1)];
            st.fill_mask = st.mask.clone();
            return Vec::new();
        }
        let mut solid: Vec<bool> = stats.iter().map(|c| c[4] as i64 > speck).collect();
        let mut tall: Vec<bool> = stats.iter().map(|c| c[3] > max_h).collect();
        solid[0] = false;
        tall[0] = false;
        st.solid = solid;
        let kept = if tall.iter().any(|&t| t) {
            let mut k = st.mask.clone();
            for (p, &l) in k.data.iter_mut().zip(&st.labels.data) {
                if tall[l as usize] {
                    *p = 0;
                }
            }
            k
        } else {
            st.mask.clone()
        };
        let joined = cv::dilate(&kept, self.scaled(fh, s.join_width_px) as usize, 1);
        let (_, jstats) = cv::connected_components(&joined, 8);
        let mut boxes = Vec::new();
        for c in jstats.iter().skip(1) {
            let r = Rect::new(c[0], c[1], c[2], c[3]);
            if r.h < min_h || r.h > max_h {
                continue;
            }
            let is_line =
                min_w <= r.w && r.w <= max_w && r.w as f64 > r.h as f64 * s.line_aspect_min;
            if !is_line {
                if r.w >= min_narrow_w && r.w as f64 <= s.narrow_max_lines * r.h as f64 {
                    narrow.push(r);
                }
                continue;
            }
            let fill = cv::count_nonzero(&sub(&kept, r)) as f64 / (r.w as f64 * r.h as f64);
            if s.fill_min < fill && fill < s.fill_max {
                boxes.push(r);
            }
        }
        st.fill_mask = kept;
        boxes
    }

    fn narrow_fill_ok(&self, st: &MaskState, b: Rect) -> bool {
        if b.w <= 0 || b.h <= 0 {
            return false;
        }
        let fill = cv::count_nonzero(&sub(&st.fill_mask, b)) as f64 / (b.w as f64 * b.h as f64);
        self.settings.fill_min < fill && fill < self.settings.fill_max
    }

    /// No solid ink within `clear_lines` line heights to either side.
    fn is_clear(&self, st: &MaskState, b: Rect) -> bool {
        let reach = (self.settings.clear_lines * b.h as f64).ceil() as i32;
        let (w, h) = (st.labels.width as i32, st.labels.height as i32);
        let (r0, r1) = (b.y.max(0), (b.y + b.h).min(h));
        if r1 <= r0 {
            return true;
        }
        let (ls, le) = ((b.x - reach).max(0), b.x);
        if le > ls && st.solid_any(ls, r0, le, r1) {
            return false;
        }
        let (rs, re) = (b.x + b.w, (b.x + b.w + reach).min(w));
        !(re > rs && st.solid_any(rs, r0, re, r1))
    }

    /// A distance line and the speed group just right of it, as one line.
    fn speed_unions(&self, boxes: &[Rect], fh: i32) -> Vec<Rect> {
        let s = &self.settings;
        let ht = self.scaled(fh, s.height_tolerance_px) as f64;
        let rt = self.scaled(fh, s.row_tolerance_px) as f64;
        let mut unions = Vec::new();
        for (li, &left) in boxes.iter().enumerate() {
            let mut best: Option<(Rect, i32)> = None;
            for (ri, &right) in boxes.iter().enumerate() {
                if ri == li {
                    continue;
                }
                let gap = right.x - (left.x + left.w);
                if !(1 <= gap && gap as f64 <= s.speed_join_gap * left.h as f64)
                    || right.w as f64 > s.speed_join_max_lines * left.h as f64
                {
                    continue;
                }
                if ((right.h - left.h).abs() as f64)
                    > ht.max(left.h as f64 * s.height_tolerance_lines)
                {
                    continue;
                }
                let dc = (right.y as f64 + right.h as f64 / 2.0
                    - (left.y as f64 + left.h as f64 / 2.0))
                    .abs();
                if dc > rt.max(s.row_tolerance_lines * left.h as f64) {
                    continue;
                }
                if best.is_none_or(|(_, g)| gap < g) {
                    best = Some((right, gap));
                }
            }
            if let Some((b, _)) = best {
                let top = left.y.min(b.y);
                let bottom = (left.y + left.h).max(b.y + b.h);
                unions.push(Rect::new(left.x, top, b.x + b.w - left.x, bottom - top));
            }
        }
        unions
    }

    // -- pairs (NameplateDetector.Pairs) -----------------------------------------

    fn tolerances(&self, fh: i32) -> (f64, f64, f64, f64) {
        let s = &self.settings;
        (
            self.scaled(fh, s.height_tolerance_px) as f64,
            self.scaled(fh, s.center_tolerance) as f64,
            self.scaled(fh, s.left_tolerance) as f64,
            self.scaled(fh, s.left_width_difference) as f64,
        )
    }

    fn pair_ok(&self, top: Rect, bottom: Rect, tol: (f64, f64, f64, f64)) -> bool {
        let (ht, ct, lt, lwd) = tol;
        let s = &self.settings;
        let gap = bottom.y - (top.y + top.h);
        if !(1 <= gap && gap as f64 <= top.h as f64 * s.pair_gap_max_lines) {
            return false;
        }
        if ((bottom.h - top.h).abs() as f64) > ht.max(top.h as f64 * s.height_tolerance_lines) {
            return false;
        }
        let centre = |r: Rect| r.x as f64 + r.w as f64 / 2.0;
        if (centre(bottom) - centre(top)).abs() > ct {
            return false;
        }
        if ((top.x - bottom.x).abs() as f64) <= lt && ((top.w - bottom.w).abs() as f64) >= lwd {
            return false;
        }
        true
    }

    fn pair_lines(&self, boxes: &[Rect], fh: i32, real_count: usize) -> Vec<(usize, usize)> {
        let tol = self.tolerances(fh);
        let by_top = LinesByTop::new(boxes, self.settings.pair_gap_max_lines);
        let mut pairs = Vec::new();
        for t in 0..real_count {
            for b in by_top.below(boxes[t]) {
                if b != t && self.pair_ok(boxes[t], boxes[b], tol) {
                    pairs.push((t, b));
                }
            }
        }
        pairs
    }

    /// Narrow shapes (a short name) that pair with a line below and stack
    /// with a row above become lines: (shape, the bottoms it pairs with).
    fn promote_narrow(
        &self,
        st: &MaskState,
        narrow: &[Rect],
        plain: &[Rect],
        unions: &[Rect],
        fh: i32,
        pool: &dyn Fn() -> Vec<Rect>,
    ) -> Vec<(Rect, Vec<Rect>)> {
        let tol = self.tolerances(fh);
        let plain_set: HashSet<Rect> = plain.iter().copied().collect();
        let all: Vec<Rect> = plain.iter().chain(unions).copied().collect();
        let below = LinesByTop::new(&all, self.settings.pair_gap_max_lines);
        let mut promoted = Vec::new();
        for &shape in narrow {
            if plain_set.contains(&shape) {
                continue;
            }
            let mut bottoms: Option<Vec<Rect>> = None;
            for i in below.below(shape) {
                let bottom = all[i];
                if !self.pair_ok(shape, bottom, tol) {
                    continue;
                }
                if self.stack_rows(&pool(), shape, bottom, fh).0.len() > 1 {
                    bottoms.get_or_insert_with(Vec::new).push(bottom);
                }
            }
            if let Some(b) = bottoms
                && self.narrow_fill_ok(st, shape)
            {
                promoted.push((shape, b));
            }
        }
        promoted
    }

    fn find_stacks(
        &self,
        boxes: &[Rect],
        pairs: &[(usize, usize)],
        pool: &dyn Fn() -> Vec<Rect>,
        fh: i32,
    ) -> HashMap<(Rect, Rect), Stack> {
        let mut stacks = HashMap::new();
        if pairs.is_empty() {
            return stacks;
        }
        let pool = pool();
        for &(t, b) in pairs {
            let (rows, blockers) = self.stack_rows(&pool, boxes[t], boxes[b], fh);
            if rows.len() > 1 || !blockers.is_empty() {
                stacks.insert((boxes[t], boxes[b]), (rows, blockers));
            }
        }
        stacks
    }

    /// Lines merged into rows: lines on one row that touch or nearly do.
    fn row_candidates(&self, boxes: &[Rect], fh: i32) -> Vec<Rect> {
        let s = &self.settings;
        let s2 = self.scaled(fh, s.row_join_top_px);
        let s3 = self.scaled(fh, s.row_join_height_px);
        let s4 = self.scaled(fh, s.row_join_gap_px);
        let n = boxes.len();
        let mut parent: Vec<usize> = (0..n).collect();
        fn find(parent: &mut [usize], mut i: usize) -> usize {
            while parent[i] != i {
                parent[i] = parent[parent[i]];
                i = parent[i];
            }
            i
        }
        for i in 0..n {
            let a = boxes[i];
            for (j, &c) in boxes.iter().enumerate().skip(i + 1) {
                if (a.y - c.y).abs() <= s2
                    && (a.h - c.h).abs() <= s3
                    && c.x <= a.x + a.w + s4
                    && a.x <= c.x + c.w + s4
                {
                    let ri = find(&mut parent, i);
                    let rj = find(&mut parent, j);
                    parent[rj] = ri;
                }
            }
        }
        let mut order: Vec<usize> = Vec::new();
        let mut members: HashMap<usize, Vec<usize>> = HashMap::new();
        for i in 0..n {
            let root = find(&mut parent, i);
            members
                .entry(root)
                .or_insert_with(|| {
                    order.push(root);
                    Vec::new()
                })
                .push(i);
        }
        order
            .iter()
            .map(|root| {
                let g = &members[root];
                if g.len() == 1 {
                    return boxes[g[0]];
                }
                let x0 = g.iter().map(|&i| boxes[i].x).min().unwrap();
                let y0 = g.iter().map(|&i| boxes[i].y).min().unwrap();
                let x1 = g.iter().map(|&i| boxes[i].x + boxes[i].w).max().unwrap();
                let y1 = g.iter().map(|&i| boxes[i].y + boxes[i].h).max().unwrap();
                Rect::new(x0, y0, x1 - x0, y1 - y0)
            })
            .collect()
    }

    /// The name rows above `top`, joined upwards while they line up with the
    /// anchor (the distance line): (rows top first, rows that broke it).
    fn stack_rows(&self, pool: &[Rect], top: Rect, anchor: Rect, fh: i32) -> Stack {
        let s = &self.settings;
        let (ht, ct, lt, lwd) = self.tolerances(fh);
        let anchor_center = anchor.x as f64 + anchor.w as f64 / 2.0;
        let mut chain = vec![top];
        let mut current = top;
        loop {
            let mut joinable: Vec<(i32, Rect)> = Vec::new();
            let mut broken = Vec::new();
            for &row in pool {
                if row == anchor || chain.contains(&row) {
                    continue;
                }
                let gap = current.y - (row.y + row.h);
                if !(1 <= gap && gap as f64 <= s.row_gap_max * current.h as f64) {
                    continue;
                }
                let pitch = (current.y - row.y) as f64;
                let ch = current.h as f64;
                if !(s.row_pitch_min * ch <= pitch && pitch <= s.row_pitch_max * ch) {
                    continue;
                }
                if ((row.h - current.h).abs() as f64) > ht.max(ch * s.height_tolerance_lines) {
                    continue;
                }
                if (row.w as f64) < s.row_min_caps * row.h as f64 {
                    continue;
                }
                if row.x >= current.x + current.w || row.x + row.w <= current.x {
                    continue;
                }
                if ((row.x - current.x).abs() as f64) <= lt
                    && ((row.w - current.w).abs() as f64) >= lwd
                {
                    continue;
                }
                if (row.x as f64 + row.w as f64 / 2.0 - anchor_center).abs() > ct {
                    broken.push(row);
                    continue;
                }
                joinable.push((gap, row));
            }
            if joinable.is_empty() {
                chain.reverse();
                return (chain, broken);
            }
            let key = |c: &(i32, Rect)| (c.0, c.1.x, c.1.y, c.1.w, c.1.h);
            let mut best = joinable[0];
            for c in &joinable {
                if key(c) < key(&best) {
                    best = *c;
                }
            }
            if chain.len() >= s.max_rows {
                chain.reverse();
                broken.push(best.1);
                return (chain, broken);
            }
            chain.push(best.1);
            current = best.1;
        }
    }

    #[allow(clippy::type_complexity)]
    fn sort_pairs(
        &self,
        st: &MaskState,
        boxes: &[Rect],
        clear: &dyn Fn(usize) -> bool,
        all_pairs: &[(usize, usize)],
        real_count: usize,
    ) -> (
        Vec<(usize, usize)>,
        Vec<(usize, usize)>,
        Vec<(usize, usize, Vec<usize>)>,
    ) {
        let (mut strict, mut loose, mut crowd) = (Vec::new(), Vec::new(), Vec::new());
        let mut pairs_of_line: HashMap<usize, Vec<(usize, usize)>> = HashMap::new();
        for &pair in all_pairs {
            pairs_of_line.entry(pair.0).or_default().push(pair);
            pairs_of_line.entry(pair.1).or_default().push(pair);
        }
        let mut cache: HashMap<usize, Option<Vec<usize>>> = HashMap::new();
        for &(t, b) in all_pairs {
            if clear(t) && clear(b) {
                strict.push((t, b));
                continue;
            }
            if self.settings.crowd_exception {
                let mut need: BTreeSet<usize> = BTreeSet::new();
                let mut blocked = false;
                for line in [t, b] {
                    let near = cache.entry(line).or_insert_with(|| {
                        if clear(line) {
                            Some(Vec::new())
                        } else {
                            self.neighbours(st, boxes, line, real_count)
                        }
                    });
                    match near {
                        None => {
                            blocked = true;
                            break;
                        }
                        Some(n) => need.extend(n.iter().copied()),
                    }
                }
                if !blocked {
                    need.remove(&t);
                    need.remove(&b);
                    let other = |o: &(usize, usize)| o.0 != t && o.0 != b && o.1 != t && o.1 != b;
                    if !need.is_empty()
                        && need
                            .iter()
                            .all(|k| pairs_of_line.get(k).is_some_and(|ps| ps.iter().any(other)))
                    {
                        crowd.push((t, b, need.into_iter().collect()));
                        continue;
                    }
                }
            }
            loose.push((t, b));
        }
        (strict, loose, crowd)
    }

    /// The other lines whose solid ink sits beside line `index`, or None
    /// when some of that ink belongs to no line.
    fn neighbours(
        &self,
        st: &MaskState,
        boxes: &[Rect],
        index: usize,
        real_count: usize,
    ) -> Option<Vec<usize>> {
        let b = boxes[index];
        let reach = (self.settings.clear_lines * b.h as f64).ceil() as i32;
        let (w, h) = (st.labels.width as i32, st.labels.height as i32);
        let (r0, r1) = (b.y.max(0), (b.y + b.h).min(h));
        let mut found = BTreeSet::new();
        if r1 <= r0 {
            return Some(Vec::new());
        }
        for (start, end) in [
            ((b.x - reach).max(0), b.x),
            (b.x + b.w, (b.x + b.w + reach).min(w)),
        ] {
            if end <= start {
                continue;
            }
            let (ww, wh) = ((end - start) as usize, (r1 - r0) as usize);
            let window: Vec<bool> = (0..wh)
                .flat_map(|y| (0..ww).map(move |x| (x + start as usize, y + r0 as usize)))
                .map(|(x, y)| st.solid_at(x, y))
                .collect();
            if !window.iter().any(|&v| v) {
                continue;
            }
            let mut rest = window.clone();
            for (k, &o) in boxes.iter().enumerate().take(real_count) {
                if k == index {
                    continue;
                }
                if o.x >= end || o.x + o.w <= start || o.y >= r1 || o.y + o.h <= r0 {
                    continue;
                }
                let (x0, x1) = (
                    (start.max(o.x) - start) as usize,
                    ((end.min(o.x + o.w)) - start) as usize,
                );
                let (y0, y1) = (
                    (r0.max(o.y) - r0) as usize,
                    ((r1.min(o.y + o.h)) - r0) as usize,
                );
                let mut any = false;
                for y in y0..y1 {
                    for x in x0..x1 {
                        any |= window[y * ww + x];
                        rest[y * ww + x] = false;
                    }
                }
                if any {
                    found.insert(k);
                }
            }
            if rest.iter().any(|&v| v) {
                return None;
            }
        }
        Some(found.into_iter().collect())
    }

    // -- candidates -------------------------------------------------------------------

    /// Every text line and line pair in the settings' region of `bgr`.
    pub fn find_candidates(&self, bgr: &Image<u8>) -> Candidates {
        let s = &self.settings;
        let (width, height) = (bgr.width as i32, bgr.height as i32);
        let sh = s.effective_scale_height(height);
        let x0 = (s.region_left * width as f64) as i32;
        let x1 = width.min((s.region_right * width as f64) as i32);
        let y0 = (s.region_top * height as f64) as i32;
        let y1 = height.min((s.region_bottom * height as f64) as i32);
        if x1 <= x0 || y1 <= y0 {
            return Candidates::default();
        }
        let pad = s.context_padding(sh);
        let (px0, px1) = ((x0 - pad).max(0), width.min(x1 + pad));
        assert!(bgr.channels >= 3, "the finder takes a colour image");
        let mut st = self.build_color_mask(
            bgr,
            (px0 as usize, y0 as usize, px1 as usize, y1 as usize),
            sh,
        );
        let mut narrow_shapes = Vec::new();
        let lines = self.find_text_lines(&mut st, sh, &mut narrow_shapes);
        if lines.is_empty() {
            return Candidates::default();
        }
        let inside = |l: &Rect| x0 <= l.x + px0 && l.x + l.w + px0 <= x1;
        let plain: Vec<Rect> = lines.iter().copied().filter(inside).collect();
        if plain.is_empty() {
            return Candidates::default();
        }
        let plain_count = plain.len();
        let unions = self.speed_unions(&plain, sh);
        let narrow_inside: Vec<Rect> = narrow_shapes.iter().copied().filter(inside).collect();
        let pool_cell: OnceCell<Vec<Rect>> = OnceCell::new();
        let pool = || {
            pool_cell
                .get_or_init(|| self.row_candidates(&plain, sh))
                .clone()
        };

        let promoted = if narrow_inside.is_empty() {
            Vec::new()
        } else {
            self.promote_narrow(&st, &narrow_inside, &plain, &unions, sh, &pool)
        };
        let mut local: Vec<Rect> = plain.clone();
        local.extend(promoted.iter().map(|p| p.0));
        let real_count = plain_count + promoted.len();
        local.extend(unions.iter().copied());

        let clear_known: std::cell::RefCell<HashMap<usize, bool>> = Default::default();
        let clear = |i: usize| -> bool {
            if let Some(&v) = clear_known.borrow().get(&i) {
                return v;
            }
            let v = self.is_clear(&st, local[i]);
            clear_known.borrow_mut().insert(i, v);
            v
        };

        let mut all_pairs = self.pair_lines(&local, sh, real_count);
        if !promoted.is_empty() {
            let sandwich: HashSet<(Rect, Rect)> = promoted
                .iter()
                .flat_map(|(n, bottoms)| bottoms.iter().map(move |&b| (*n, b)))
                .collect();
            let is_promoted = |i: usize| plain_count <= i && i < real_count;
            all_pairs.retain(|&(t, b)| {
                !is_promoted(b) && !(is_promoted(t) && !sandwich.contains(&(local[t], local[b])))
            });
        }
        let stacks = self.find_stacks(&local, &all_pairs, &pool, sh);
        let (pairs, loose, crowd) = self.sort_pairs(&st, &local, &clear, &all_pairs, real_count);
        let shift = |r: Rect| Rect::new(r.x + px0, r.y + y0, r.w, r.h);
        let boxes = local.iter().map(|&r| shift(r)).collect();
        let stacks = stacks
            .into_iter()
            .map(|((t, b), (rows, blockers))| {
                (
                    (shift(t), shift(b)),
                    (
                        rows.into_iter().map(shift).collect(),
                        blockers.into_iter().map(shift).collect(),
                    ),
                )
            })
            .collect();
        Candidates {
            boxes,
            pairs,
            loose,
            crowd,
            stacks,
        }
    }
}
