//! `MiningSignalReader`: the scanner's signal row read off a frame.
//!
//! The crop is cleaned to "ink" - per pixel the darkest and the brightest
//! channel, each top-hatted with the HUD's straight lines opened away - and
//! the strongest rows are cut into cells: Otsu's threshold, components,
//! wide cells split in two, the condensed red font recut. A row counts when
//! a signal icon sits left of its digits, centred within a line height of
//! where the scanner puts it; its cells are matched to the digit shapes
//! ([`super::digits`]). When no row is found, the icon is looked for by
//! template ("the pin") and the row read beside it.
//!
//! The pin's template match is not OpenCV's bit for bit (OpenCV correlates
//! through a DFT in single precision; here it is summed directly in double),
//! so on the pin path the decisions are IRIS's, the scores within rounding.

use super::digits::{self, HEIGHT, LEN, Patch, WIDTH};
use super::{
    Bgr, MiningSettings, PixelRect, SignalRead, SignalState, base64_decode, is_number, round_even,
};
use crate::cv;
use crate::{Image, Rect};

const MAX_ROWS: usize = 4;

/// IRIS's pin template: 24 rows of 21 values, the signal icon's ink.
const PIN_TEMPLATE: &str = "DAkLCQYGBAUNEA4OCw4KEAwEBAQBCwgLCAYGBTSSkJmQji0KDAkEBAQCDAkHBwYFRpiblZqRmY5UCgsFBAMACwkGBQYwmZJpLCo2c5ORCAgFBAQCDAYJCQqHmXwACQoMCoKYXgcGBQcDCQQHCgePmE8DDA0NC3WYegQFBQYDBgYDBwePlkcGCg4OC3KYfwAGBQgEBgIEBQiFmXQGCxIODYeaYgAGBQQDAgEEBwgylpBOFxYZXI+TAQIGBgYCAwIBCAgLhJGZjJSMlZBuAAcKBwgEAQQCCAoMI4yVjpiNl4MDBQgHCAYCAAUEBgkMD2eVjJeMlUoACQsGBgcCAgYGCQ4NERSRkJuOgQULDQ0ICQkFBQMHBwoPEhBMjJiMNAQOCgwGBgYDBAQICQ0QEQ8Uf5lsAAkSCwsJCQcEBgYIDAwNExATOJILCRASDAwICAgGCQMIBwoLDg4SD0kFDgwPCwkHCAQEBQQEETBCVG0jCxELFGhJMSIOBAQDBQaGjpOTlpFyChUOfpCUjpGLeQQEBpCXlJaUmZKSNhIrlJGbkpaTlYMAB4+TkJOPlo+VjJeMlY2Vj5KQlI0FCWSNlJSQlpGVjJaMlYuUj5KNik8ACwcMUnB6hYSOiZSJjYeHfHFYCAAJCAoICQYFAgEFBAMFAAMABQIGBgwO";
const PIN_ROWS: usize = 24;
const PIN_COLS: usize = 21;

fn pin_template() -> &'static Image<f32> {
    static P: std::sync::OnceLock<Image<f32>> = std::sync::OnceLock::new();
    P.get_or_init(|| {
        let bytes = base64_decode(PIN_TEMPLATE).expect("the pin template's base64");
        assert_eq!(bytes.len(), PIN_ROWS * PIN_COLS);
        Image::from_vec(
            PIN_COLS,
            PIN_ROWS,
            1,
            bytes.iter().map(|&b| b as f32).collect(),
        )
    })
}

/// `RegionPlanner.ToWindowRect`: an area in whole window pixels, as the
/// capture cuts it.
pub fn to_window_rect(r: PixelRect, width: i32, height: i32) -> Rect {
    let x0 = (round_even(r.x) as i32).clamp(0, width);
    let y0 = (round_even(r.y) as i32).clamp(0, height);
    let x1 = (round_even(r.x + r.w) as i32).clamp(0, width);
    let y1 = (round_even(r.y + r.h) as i32).clamp(0, height);
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

fn line_height(window_height: i32, s: &MiningSettings) -> f64 {
    s.line_height_at_1440 * window_height as f64 / 1440.0
}

fn area(
    window_width: i32,
    window_height: i32,
    s: &MiningSettings,
    top: f64,
    bottom: f64,
) -> PixelRect {
    let lh = line_height(window_height, s);
    let cx = window_width as f64 / 2.0;
    let (w, h) = (window_width as f64, window_height as f64);
    let x0 = round_even(cx - s.left_lines * lh).clamp(0.0, w);
    let x1 = round_even(cx + s.right_lines * lh).clamp(0.0, w);
    let y0 = round_even(top).clamp(0.0, h);
    let y1 = round_even(bottom).clamp(0.0, h);
    PixelRect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
}

/// `SearchArea`: where the scan looks for the signal while nothing is
/// locked.
pub fn search_area(window_width: i32, window_height: i32, s: &MiningSettings) -> PixelRect {
    let lh = line_height(window_height, s);
    let cy = s.row_centre_y * window_height as f64 / 1440.0;
    area(
        window_width,
        window_height,
        s,
        cy - s.search_above_lines * lh,
        cy + s.search_below_lines * lh,
    )
}

/// `RowSearchArea`: the band the rows are looked for in first.
pub fn row_search_area(window_width: i32, window_height: i32, s: &MiningSettings) -> PixelRect {
    let lh = line_height(window_height, s);
    let cy = s.row_centre_y * window_height as f64 / 1440.0;
    area(
        window_width,
        window_height,
        s,
        cy - s.row_search_half_lines * lh,
        cy + s.row_search_half_lines * lh,
    )
}

/// `LockArea`: where a locked row is read again.
pub fn lock_area(
    window_width: i32,
    window_height: i32,
    row_top: f64,
    s: &MiningSettings,
) -> PixelRect {
    let lh = line_height(window_height, s);
    area(
        window_width,
        window_height,
        s,
        row_top - s.lock_above_lines * lh,
        row_top + s.lock_below_lines * lh,
    )
}

/// The part of `img` inside `r` (a copy; `r` lies inside it).
fn crop<T: Copy + Default>(img: &Image<T>, r: Rect) -> Image<T> {
    let cn = img.channels;
    let mut out = Image::new(r.w as usize, r.h as usize, cn);
    for y in 0..r.h as usize {
        let src = img.row(r.y as usize + y);
        let start = r.x as usize * cn;
        out.data[y * r.w as usize * cn..(y + 1) * r.w as usize * cn]
            .copy_from_slice(&src[start..start + r.w as usize * cn]);
    }
    out
}

/// One cell of a row: a digit or a comma, a component (`shape`) of the
/// strip's threshold.
#[derive(Clone, Copy, Debug)]
struct Cell {
    digit: bool,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    shape: i32,
}

#[derive(Clone, Copy, Debug)]
struct SymbolBox {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

struct Group {
    cells: Vec<Cell>,
    strip: std::rc::Rc<Vec<u8>>,
    strip_height: i32,
    threshold: f64,
    row_height: i32,
    small: bool,
    condensed: bool,
    left: i32,
    right: i32,
    top: i32,
    height: i32,
    symbol: Option<SymbolBox>,
}

impl Group {
    fn centre(&self) -> f64 {
        match self.symbol {
            Some(s) => (s.x + self.right) as f64 / 2.0,
            None => (self.left + self.right) as f64 / 2.0,
        }
    }
}

/// LINQ's `Min()` on doubles: the first of equals, a NaN at once.
fn linq_min(values: impl IntoIterator<Item = f64>) -> Option<f64> {
    let mut it = values.into_iter();
    let mut v = it.next()?;
    if v.is_nan() {
        return Some(v);
    }
    for x in it {
        if x < v {
            v = x;
        } else if x.is_nan() {
            return Some(x);
        }
    }
    Some(v)
}

/// The frame prepared for reading: its BGR pixels and its ink.
struct Prepared {
    bgr: Image<u8>,
    ink: Image<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct MiningSignalReader {
    pub settings: MiningSettings,
}

impl MiningSignalReader {
    pub fn new(settings: MiningSettings) -> Self {
        Self { settings }
    }

    /// `Search`: the row band first, then the pin over the whole crop.
    /// `image` is the BGR (or BGRA) crop of `region` of a window of
    /// `window_width` x `window_height`.
    pub fn search(
        &self,
        image: &Image<u8>,
        region: Rect,
        window_width: i32,
        window_height: i32,
    ) -> SignalRead {
        let rows = row_search_area(window_width, window_height, &self.settings);
        let x0 = (round_even(rows.x) as i32).max(region.x);
        let y0 = (round_even(rows.y) as i32).max(region.y);
        let x1 =
            (round_even(rows.x + rows.w) as i32).min(region.w.min(image.width as i32) + region.x);
        let y1 =
            (round_even(rows.y + rows.h) as i32).min(region.h.min(image.height as i32) + region.y);
        if x1 - x0 >= 4 && y1 - y0 >= 4 && !image.data.is_empty() {
            let band = Rect::new(x0, y0, x1 - x0, y1 - y0);
            let part = crop(
                image,
                Rect::new(band.x - region.x, band.y - region.y, band.w, band.h),
            );
            let read = self.read_rows(&part, band, window_width, window_height);
            if read.state != SignalState::None {
                return read;
            }
        }
        self.read_from_pin(image, region, window_width, window_height)
    }

    /// `Read`: a locked row read again - the rows, then the pin.
    pub fn read(
        &self,
        image: &Image<u8>,
        region: Rect,
        window_width: i32,
        window_height: i32,
    ) -> SignalRead {
        let read = self.read_rows(image, region, window_width, window_height);
        if read.state != SignalState::None {
            read
        } else {
            self.read_from_pin(image, region, window_width, window_height)
        }
    }

    fn prepare(&self, image: &Image<u8>, window_height: i32) -> Option<Prepared> {
        if image.data.is_empty() || image.height < 4 || image.width < 4 || window_height <= 0 {
            return None;
        }
        let bgr = match image.channels {
            3 => image.clone(),
            4 => {
                let data = image
                    .data
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .flat_map(|p| [p[0], p[1], p[2]])
                    .collect();
                Image::from_vec(image.width, image.height, 3, data)
            }
            n => panic!("a mining crop with {n} channels"),
        };
        let ink = self.ink(&bgr, window_height as f64 / 1440.0);
        Some(Prepared { bgr, ink })
    }

    /// `Ink`: the darkest and the brightest channel, each top-hatted with
    /// straight lines removed, the larger of the two.
    fn ink(&self, bgr: &Image<u8>, scale: f64) -> Image<u8> {
        let (w, h) = (bgr.width, bgr.height);
        let mut low = Image::new(w, h, 1);
        let mut high = Image::new(w, h, 1);
        for (i, p) in bgr.data.as_chunks::<3>().0.iter().enumerate() {
            low.data[i] = p[0].min(p[1]).min(p[2]);
            high.data[i] = p[0].max(p[1]).max(p[2]);
        }
        let k = (round_even(15.0 * scale) as i32 | 1) as usize;
        let length =
            (round_even(2.5 * self.settings.line_height_at_1440 * scale) as i32).max(3) as usize;
        let clean = |ch: &Image<u8>| -> Image<u8> {
            let hat = cv::tophat(ch, k, k);
            let open_h = cv::open(&hat, length, 1);
            let open_v = cv::open(&hat, 1, length);
            let lines = max_of(&open_h, &open_v);
            cv::subtract(&hat, &lines)
        };
        max_of(&clean(&low), &clean(&high))
    }

    fn read_rows(
        &self,
        image: &Image<u8>,
        region: Rect,
        window_width: i32,
        window_height: i32,
    ) -> SignalRead {
        let lh = self.settings.line_height_at_1440 * window_height as f64 / 1440.0;
        let h = round_even(lh) as i32;
        if h < 4 {
            return SignalRead::nothing();
        }
        let Some(p) = self.prepare(image, window_height) else {
            return SignalRead::nothing();
        };
        let (width, height) = (p.ink.width as i32, p.ink.height as i32);
        let mut groups = Vec::new();
        for row_top in row_tops(&p.ink.data, width, height, h) {
            groups.extend(self.read_row(&p, width, height, row_top, h, lh, false));
        }
        self.decide(groups, lh, &p, width, region, window_width)
    }

    fn read_from_pin(
        &self,
        image: &Image<u8>,
        region: Rect,
        window_width: i32,
        window_height: i32,
    ) -> SignalRead {
        let Some(p) = self.prepare(image, window_height) else {
            return SignalRead::nothing();
        };
        let s = &self.settings;
        let scale = window_height as f64 / 1440.0;
        let (width, height) = (p.ink.width as i32, p.ink.height as i32);
        let (mut best_score, mut best_height, mut best_loc) =
            (f64::NEG_INFINITY, 0.0, (0i32, 0i32));
        let ink_f = Image::from_vec(
            p.ink.width,
            p.ink.height,
            1,
            p.ink.data.iter().map(|&v| v as f32).collect(),
        );
        for pin_height in s.pin_height_min..=s.pin_height_max {
            let th = round_even(pin_height as f64 * scale) as i32;
            let tw = (round_even(th as f64 * PIN_COLS as f64 / PIN_ROWS as f64) as i32).max(3);
            if th < 4 || th > height || tw > width {
                continue;
            }
            let template = cv::resize_area_f32_to(pin_template(), tw as usize, th as usize);
            if let Some((max, loc)) = match_ccoeff_normed_max(&ink_f, &template)
                && !max.is_nan()
                && max > best_score
            {
                best_score = max;
                best_height = th as f64;
                best_loc = loc;
            }
        }
        if best_score < s.pin_min_score {
            return SignalRead::nothing();
        }
        let lh = best_height * s.pin_digit_ratio;
        let h = round_even(lh) as i32;
        if h < 4 {
            return SignalRead::nothing();
        }
        let row_top = round_even(best_loc.1 as f64 + best_height / 2.0 - lh / 2.0) as i32;
        let small = (h as f64) < s.small_row_lines * s.line_height_at_1440 * scale;
        let groups = self.read_row(&p, width, height, row_top, h, lh, small);
        self.decide(groups, lh, &p, width, region, window_width)
    }

    fn decide(
        &self,
        groups: Vec<Group>,
        lh: f64,
        p: &Prepared,
        width: i32,
        region: Rect,
        window_width: i32,
    ) -> SignalRead {
        let s = &self.settings;
        let mut unique: Vec<Group> = Vec::new();
        for g in groups {
            if !unique.iter().any(|u| {
                ((g.left - u.left).abs() as f64) <= 0.5 * lh
                    && ((g.top - u.top).abs() as f64) <= 0.5 * lh
            }) {
                unique.push(g);
            }
        }
        let expected = window_width as f64 / 2.0 + s.centre_offset_lines * lh - region.x as f64;
        let mut main: Option<usize> = None;
        for (i, g) in unique.iter().enumerate() {
            if g.symbol.is_some()
                && main.is_none_or(|m| {
                    (g.centre() - expected).abs() < (unique[m].centre() - expected).abs()
                })
            {
                main = Some(i);
            }
        }
        let Some(mi) = main else {
            return SignalRead::nothing();
        };
        let m = &unique[mi];
        let offset = m.centre() - expected;
        let others = unique
            .iter()
            .enumerate()
            .filter(|&(i, g)| i != mi && ((g.top - m.top).abs() as f64) <= 0.5 * lh)
            .count();
        let (text, scores, all) = self.classify(m, width);
        if offset.abs() > s.centre_tolerance_lines * lh || others > 0 {
            return SignalRead {
                state: SignalState::Disqualified,
                text,
                offset_px: offset,
                ..SignalRead::nothing()
            };
        }
        let sym = m.symbol.expect("the main group has its symbol");
        let top = sym.y.min(m.top);
        let bottom = (sym.y + sym.h).max(m.top + m.height);
        let signal_box = PixelRect::new(
            (region.x + sym.x) as f64,
            (region.y + top) as f64,
            (m.right - sym.x) as f64,
            (bottom - top) as f64,
        );
        let (text_colour, back_colour) = colours(m, p, width);
        let min_score = linq_min(scores.iter().copied()).unwrap_or(0.0);
        let readable = is_number(&text) && scores.iter().all(|&x| x >= s.min_digit_score);
        let value = if readable {
            text.replace(',', "").parse::<i32>().ok()
        } else {
            None
        };
        SignalRead {
            state: if value.is_some() {
                SignalState::Signal
            } else {
                SignalState::Unreadable
            },
            text,
            value,
            signal_box,
            row_top: (region.y + m.top) as f64,
            digit_height: m.height as f64,
            text_colour,
            back_colour,
            offset_px: offset,
            min_score,
            digit_scores: Some(all),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn read_row(
        &self,
        p: &Prepared,
        width: i32,
        height: i32,
        row_top: i32,
        h: i32,
        lh: f64,
        small: bool,
    ) -> Vec<Group> {
        let s = &self.settings;
        let hf = h as f64;
        let mut groups = Vec::new();
        let ya = 0.max((row_top as f64 - 0.3 * hf) as i32);
        let yb = height.min((row_top as f64 + hf + 0.3 * hf) as i32 + 1);
        if yb - ya < 2 {
            return groups;
        }
        let strip_height = yb - ya;
        let strip_data =
            p.ink.data[(ya * width) as usize..((ya + strip_height) * width) as usize].to_vec();
        let strip_img = Image::from_vec(width as usize, strip_height as usize, 1, strip_data);
        let threshold = cv::otsu_threshold(&strip_img).max(s.min_ink_threshold);
        let binary = cv::threshold_binary(&strip_img, threshold, 1.0);
        let (labels, stats) = cv::connected_components(&binary, 8);
        let labels = labels.data;
        let strip = std::rc::Rc::new(strip_img.data);
        let mut cells = Vec::new();
        for (i, st) in stats.iter().enumerate().skip(1) {
            let [x, y, w, hh, area] = *st;
            if area < 2 {
                continue;
            }
            let (wf, hhf) = (w as f64, hh as f64);
            let wide = wf > s.split_width_lines * hf && hhf <= 1.15 * hf;
            let cell = Cell {
                digit: true,
                x,
                y,
                w,
                h: hh,
                shape: i as i32,
            };
            if hhf >= 0.7 * hf && wf <= 1.05 * hf && hhf <= 1.4 * hf && !wide {
                cells.push(cell);
            } else if hhf >= 0.7 * hf && wide && wf <= 1.6 * hf {
                cells.extend(self.split_or_whole(
                    &strip,
                    strip_height,
                    width,
                    h,
                    small,
                    &labels,
                    cell,
                ));
            } else if hhf >= 0.25 * hf
                && hhf <= 0.6 * hf
                && wf <= 0.45 * hf
                && y as f64 >= 0.55 * hf
            {
                cells.push(Cell {
                    digit: false,
                    ..cell
                });
            }
        }
        cells.sort_by_key(|c| c.x);
        let mut runs: Vec<Vec<Cell>> = Vec::new();
        let mut current: Vec<Cell> = Vec::new();
        for c in cells {
            if let Some(last) = current.last()
                && (c.x - (last.x + last.w)) as f64 > 0.75 * hf
            {
                runs.push(std::mem::take(&mut current));
            }
            current.push(c);
        }
        if !current.is_empty() {
            runs.push(current);
        }
        for run in runs {
            let mut g: Vec<Cell> = run.into_iter().skip_while(|c| !c.digit).collect();
            while g.last().is_some_and(|c| !c.digit) {
                g.pop();
            }
            let mut shapes: Vec<i32> = g.iter().filter(|c| c.digit).map(|c| c.shape).collect();
            shapes.sort_unstable();
            shapes.dedup();
            if shapes.len() < 2 {
                continue;
            }
            let left = g[0].x;
            let right = g[g.len() - 1].x + g[g.len() - 1].w;
            let digits: Vec<&Cell> = g.iter().filter(|c| c.digit).collect();
            let g_top = digits.iter().map(|c| c.y).min().expect("digits");
            let g_bottom = digits.iter().map(|c| c.y + c.h).max().expect("digits");
            let gh = g_bottom - g_top;
            let symbol = find_symbol(
                &p.ink.data,
                width,
                height,
                left,
                ya + g_top,
                ya + g_bottom,
                gh,
                lh,
                threshold,
            );
            let condensed = symbol.is_some_and(|sb| self.is_condensed(p, width, sb, threshold, &g));
            if condensed {
                g = self.recut(
                    &strip,
                    strip_height,
                    width,
                    h,
                    small,
                    &labels,
                    &stats,
                    left,
                    right,
                );
            }
            groups.push(Group {
                cells: g,
                strip: strip.clone(),
                strip_height,
                threshold,
                row_height: h,
                small,
                condensed,
                left,
                right,
                top: ya + g_top,
                height: gh,
                symbol,
            });
        }
        groups
    }

    #[allow(clippy::too_many_arguments)]
    fn split_or_whole(
        &self,
        strip: &[u8],
        strip_height: i32,
        width: i32,
        h: i32,
        small: bool,
        labels: &[i32],
        whole: Cell,
    ) -> Vec<Cell> {
        let parts = split(labels, width, whole);
        if whole.w as f64 > self.settings.split_whole_max_lines * h as f64 {
            return parts;
        }
        if parts.len() != 2 {
            return vec![whole];
        }
        let one = digits::best_score(
            &self.patch(strip, strip_height, h, whole, width, false),
            small,
        );
        let two = linq_min(parts.iter().map(|&c| {
            digits::best_score(&self.patch(strip, strip_height, h, c, width, false), small)
        }))
        .expect("two parts");
        if two > one { parts } else { vec![whole] }
    }

    fn is_condensed(
        &self,
        p: &Prepared,
        width: i32,
        symbol: SymbolBox,
        threshold: f64,
        cells: &[Cell],
    ) -> bool {
        let s = &self.settings;
        let (mut b, mut g, mut r, mut n) = (0i64, 0i64, 0i64, 0i64);
        for y in symbol.y..symbol.y + symbol.h {
            for x in symbol.x..symbol.x + symbol.w {
                if p.ink.data[(y * width + x) as usize] as f64 <= threshold {
                    continue;
                }
                let i = ((y * width + x) * 3) as usize;
                b += p.bgr.data[i] as i64;
                g += p.bgr.data[i + 1] as i64;
                r += p.bgr.data[i + 2] as i64;
                n += 1;
            }
        }
        if n < s.condensed_min_ink_pixels {
            return false;
        }
        let (mb, mg, mr) = (
            b as f64 / n as f64,
            g as f64 / n as f64,
            r as f64 / n as f64,
        );
        if mr < s.condensed_min_red
            || mg >= s.condensed_green_share * mr
            || mb >= s.condensed_blue_share * mr
        {
            return false;
        }
        let mut shares: Vec<f64> = cells
            .iter()
            .filter(|c| c.digit)
            .map(|c| c.w as f64 / c.h as f64)
            .filter(|&v| v > s.condensed_shape_min_share && v < s.condensed_shape_max_share)
            .collect();
        shares.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        !shares.is_empty() && shares[(shares.len() - 1) / 4] < s.condensed_narrow_share
    }

    #[allow(clippy::too_many_arguments)]
    fn recut(
        &self,
        strip: &[u8],
        strip_height: i32,
        width: i32,
        h: i32,
        small: bool,
        labels: &[i32],
        stats: &[cv::Stats],
        left: i32,
        right: i32,
    ) -> Vec<Cell> {
        let s = &self.settings;
        let hf = h as f64;
        let mut cells = Vec::new();
        for (i, st) in stats.iter().enumerate().skip(1) {
            let [x, y, w, hh, area] = *st;
            if area < 2 || x < left - 1 || x + w > right + 1 {
                continue;
            }
            let (wf, hhf) = (w as f64, hh as f64);
            let cell = Cell {
                digit: true,
                x,
                y,
                w,
                h: hh,
                shape: i as i32,
            };
            if hhf >= 0.7 * hf && hhf <= 1.4 * hf && wf <= s.condensed_max_shape_lines * hf {
                cells.extend(self.cut_condensed(
                    strip,
                    strip_height,
                    width,
                    h,
                    small,
                    labels,
                    cell,
                ));
            } else if hhf >= 0.25 * hf
                && hhf <= 0.6 * hf
                && wf <= 0.45 * hf
                && y as f64 >= 0.55 * hf
            {
                cells.push(Cell {
                    digit: false,
                    ..cell
                });
            }
        }
        cells.sort_by_key(|c| c.x);
        cells
    }

    #[allow(clippy::too_many_arguments)]
    fn cut_condensed(
        &self,
        strip: &[u8],
        strip_height: i32,
        width: i32,
        h: i32,
        small: bool,
        labels: &[i32],
        whole: Cell,
    ) -> Vec<Cell> {
        let s = &self.settings;
        let share = whole.w as f64 / whole.h as f64;
        if share <= s.condensed_one_share {
            return vec![whole];
        }
        if share <= s.condensed_pair_share {
            let parts = cut_into(labels, width, whole, 2, s.condensed_cut_reach);
            if parts.len() != 2 {
                return vec![whole];
            }
            let one = digits::best_score(
                &self.patch(strip, strip_height, h, whole, width, true),
                small,
            );
            let two = linq_min(parts.iter().map(|&c| {
                digits::best_score(&self.patch(strip, strip_height, h, c, width, true), small)
            }))
            .expect("two parts");
            return if two > one { parts } else { vec![whole] };
        }
        let n = (round_even((whole.w + 1) as f64 / (s.condensed_pitch_share * whole.h as f64))
            as i32)
            .clamp(2, 5);
        cut_into(labels, width, whole, n, s.condensed_cut_reach)
    }

    /// `Classify`: each cell matched; a comma for a comma cell.
    fn classify(&self, g: &Group, width: i32) -> (String, Vec<f64>, Vec<[f64; 10]>) {
        let mut text = String::new();
        let mut scores = Vec::new();
        let mut all = Vec::new();
        for &cell in &g.cells {
            if !cell.digit {
                text.push(',');
                continue;
            }
            let patch = self.patch(
                &g.strip,
                g.strip_height,
                g.row_height,
                cell,
                width,
                g.condensed,
            );
            let (digit, score, every) =
                digits::match_digit(&patch, self.settings.tie_margin, g.small);
            all.push(every);
            text.push(digit as char);
            scores.push(score);
        }
        (text, scores, all)
    }

    /// `Patch`: a cell's ink around its centre, scaled by its 95th
    /// percentile, resampled to the shapes' 14 x 20.
    fn patch(
        &self,
        strip: &[u8],
        strip_height: i32,
        h: i32,
        cell: Cell,
        width: i32,
        condensed: bool,
    ) -> Patch {
        let s = &self.settings;
        let hf = h as f64;
        let cw = if condensed {
            round_even(s.condensed_cell_lines * hf) as i32 + 2
        } else if cell.w as f64 > s.split_width_lines * hf && cell.h as f64 <= 1.15 * hf {
            cell.w
        } else {
            round_even(0.62 * hf) as i32 + 2
        };
        let x0 = round_even(cell.x as f64 + cell.w as f64 / 2.0 - cw as f64 / 2.0) as i32;
        let y0 = cell.y - 1;
        let rows = cell.h + 2;
        let mut sub = vec![0f32; (rows * cw) as usize];
        for r in 0..rows {
            let y = y0 + r;
            if y < 0 || y >= strip_height {
                continue;
            }
            for c in 0..cw {
                let x = x0 + c;
                if x >= 0 && x < width {
                    sub[(r * cw + c) as usize] = strip[(y * width + x) as usize] as f32;
                }
            }
        }
        let v = percentile(&sub, 0.95) + 1e-3;
        for x in sub.iter_mut() {
            *x = (*x as f64 / v).clamp(0.0, 1.0) as f32;
        }
        let source = Image::from_vec(cw as usize, rows as usize, 1, sub);
        let resized = cv::resize_linear_f32_to(&source, WIDTH, HEIGHT);
        let mut out = [0f32; LEN];
        out.copy_from_slice(&resized.data);
        out
    }
}

fn max_of(a: &Image<u8>, b: &Image<u8>) -> Image<u8> {
    let data = a
        .data
        .iter()
        .zip(&b.data)
        .map(|(&x, &y)| x.max(y))
        .collect();
    Image::from_vec(a.width, a.height, 1, data)
}

/// `RowTops`: up to four rows of height `h` holding the most ink, at least
/// 0.8 h apart.
fn row_tops(ink: &[u8], width: i32, height: i32, h: i32) -> Vec<i32> {
    let mut tops: Vec<i32> = Vec::new();
    if height < h {
        return tops;
    }
    let profile: Vec<f64> = (0..height)
        .map(|y| {
            let row = &ink[(y * width) as usize..((y + 1) * width) as usize];
            row.iter().map(|&v| v as i64).sum::<i64>() as f64 / width as f64
        })
        .collect();
    let sums: Vec<f64> = (0..=(height - h) as usize)
        .map(|y| {
            let mut s = 0.0;
            for i in 0..h as usize {
                s += profile[y + i];
            }
            s
        })
        .collect();
    let mut order: Vec<usize> = (0..sums.len()).collect();
    // OrderByDescending: stable, highest first
    order.sort_by(|&a, &b| {
        sums[b]
            .partial_cmp(&sums[a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for y in order {
        let y = y as i32;
        if tops
            .iter()
            .all(|&t| ((y - t).abs() as f64) >= 0.8 * h as f64)
        {
            tops.push(y);
        }
        if tops.len() == MAX_ROWS {
            break;
        }
    }
    tops
}

/// The columns of `shape` inside a cell's box.
fn columns(labels: &[i32], width: i32, c: Cell) -> Vec<i32> {
    let mut cols = vec![0i32; c.w as usize];
    for r in 0..c.h {
        for x in 0..c.w {
            if labels[((c.y + r) * width + c.x + x) as usize] == c.shape {
                cols[x as usize] += 1;
            }
        }
    }
    cols
}

/// The box of `c.shape`'s pixels in columns [from, to) of a cell.
fn part_box(labels: &[i32], width: i32, c: Cell, from: i32, to: i32) -> Option<Cell> {
    let (mut min_x, mut max_x, mut min_y, mut max_y) = (i32::MAX, -1, i32::MAX, -1);
    for r in 0..c.h {
        for x in from..to {
            if labels[((c.y + r) * width + c.x + x) as usize] != c.shape {
                continue;
            }
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(r);
            max_y = max_y.max(r);
        }
    }
    (max_x >= 0).then_some(Cell {
        digit: true,
        x: c.x + min_x,
        y: c.y + min_y,
        w: max_x - min_x + 1,
        h: max_y - min_y + 1,
        shape: c.shape,
    })
}

/// `Split`: a wide cell cut in two at its thinnest column in the middle
/// 40 %.
fn split(labels: &[i32], width: i32, c: Cell) -> Vec<Cell> {
    let cols = columns(labels, width, c);
    let w = c.w;
    let lo = (0.3 * w as f64) as i32;
    let hi = (lo + 1).max(round_even(0.7 * w as f64) as i32);
    let mut cut = lo;
    for x in lo..hi {
        if cols[x as usize] < cols[cut as usize] {
            cut = x;
        }
    }
    [(0, cut), (cut + 1, w)]
        .into_iter()
        .filter_map(|(from, to)| part_box(labels, width, c, from, to))
        .collect()
}

/// `CutInto`: a condensed cell cut into `n` at the thinnest columns near
/// its even divisions.
fn cut_into(labels: &[i32], width: i32, c: Cell, n: i32, reach: f64) -> Vec<Cell> {
    let cols = columns(labels, width, c);
    let w = c.w;
    let mut cuts: Vec<i32> = Vec::new();
    for i in 1..n {
        let at = i as f64 * w as f64 / n as f64;
        let mut lo = 1.max((at - reach * w as f64 / n as f64) as i32);
        let mut hi = (w - 1).min(round_even(at + reach * w as f64 / n as f64) as i32 + 1);
        lo = lo.max(cuts.last().map_or(1, |&l| l + 2));
        if lo >= w {
            break;
        }
        if hi <= lo {
            hi = lo + 1;
        }
        let mut cut = lo;
        for x in lo..hi.min(w) {
            if cols[x as usize] < cols[cut as usize] {
                cut = x;
            }
        }
        cuts.push(cut);
    }
    let mut edges = vec![-1];
    edges.extend(&cuts);
    edges.push(w);
    edges
        .windows(2)
        .filter_map(|e| part_box(labels, width, c, e[0] + 1, e[1]))
        .collect()
}

/// `FindSymbol`: the signal icon - enough ink left of the digits, at least
/// as tall as them and of a plausible width.
#[allow(clippy::too_many_arguments)]
fn find_symbol(
    ink: &[u8],
    width: i32,
    height: i32,
    left: i32,
    top: i32,
    bottom: i32,
    gh: i32,
    lh: f64,
    threshold: f64,
) -> Option<SymbolBox> {
    let (ghf, lf) = (gh as f64, left as f64);
    let sx0 = (lf - 2.6 * lh).max(0.0) as i32;
    let sx1 = (lf - 0.3 * lh) as i32;
    let sy0 = (top as f64 - 0.7 * ghf).max(0.0) as i32;
    let sy1 = (bottom as f64 + 0.7 * ghf).min(height as f64) as i32;
    if sx1 <= sx0 || sy1 <= sy0 {
        return None;
    }
    let (mut min_x, mut min_y, mut max_x, mut max_y, mut count) =
        (i32::MAX, i32::MAX, i32::MIN, i32::MIN, 0);
    for y in sy0..sy1 {
        for x in sx0..sx1.min(width) {
            if ink[(y * width + x) as usize] as f64 <= threshold {
                continue;
            }
            count += 1;
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
        }
    }
    if (count as f64) < 0.4 * ghf * ghf {
        return None;
    }
    let (sw, sh) = (max_x - min_x + 1, max_y - min_y + 1);
    (sh >= gh && sw as f64 >= 0.5 * ghf && sw as f64 <= 2.4 * ghf).then_some(SymbolBox {
        x: min_x,
        y: min_y,
        w: sw,
        h: sh,
    })
}

/// `Percentile`: linear between the two nearest of the sorted values.
fn percentile(values: &[f32], q: f64) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pos = q * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = (lo + 1).min(sorted.len() - 1);
    sorted[lo] as f64 + (sorted[hi] - sorted[lo]) as f64 * (pos - lo as f64)
}

/// `Colours`: the mean colour of the row's ink and the median of its
/// background.
fn colours(g: &Group, p: &Prepared, width: i32) -> (Bgr, Bgr) {
    let (mut tb, mut tg, mut tr, mut tn) = (0i64, 0i64, 0i64, 0i64);
    let mut back: Vec<[u8; 3]> = Vec::new();
    let top = 0.max(g.top - g.height / 3);
    let bottom = (p.bgr.height as i32).min(g.top + g.height + g.height / 3);
    for y in top..bottom {
        for x in 0.max(g.left)..width.min(g.right) {
            let i = ((y * width + x) * 3) as usize;
            let v = p.ink.data[(y * width + x) as usize] as f64;
            if v > g.threshold {
                tb += p.bgr.data[i] as i64;
                tg += p.bgr.data[i + 1] as i64;
                tr += p.bgr.data[i + 2] as i64;
                tn += 1;
            } else if v < g.threshold / 2.0 {
                back.push([p.bgr.data[i], p.bgr.data[i + 1], p.bgr.data[i + 2]]);
            }
        }
    }
    let text = if tn == 0 {
        Bgr::new(255, 255, 255)
    } else {
        Bgr::new((tb / tn) as u8, (tg / tn) as u8, (tr / tn) as u8)
    };
    if back.is_empty() {
        return (text, Bgr::default());
    }
    let median = |c: usize| -> u8 {
        let mut v: Vec<u8> = back.iter().map(|p| p[c]).collect();
        v.sort_unstable();
        v[v.len() / 2]
    };
    (text, Bgr::new(median(0), median(1), median(2)))
}

/// TM_CCOEFF_NORMED over every position, summed directly in double, with
/// OpenCV's normalisation rules; its maximum and where (the first in raster
/// order, as `minMaxLoc`). None when the template does not fit.
fn match_ccoeff_normed_max(img: &Image<f32>, templ: &Image<f32>) -> Option<(f64, (i32, i32))> {
    let (iw, ih, tw, th) = (img.width, img.height, templ.width, templ.height);
    if tw > iw || th > ih {
        return None;
    }
    let area = (tw * th) as f64;
    let t_mean = templ.data.iter().map(|&v| v as f64).sum::<f64>() / area;
    let t_norm2: f64 = templ
        .data
        .iter()
        .map(|&v| (v as f64 - t_mean).powi(2))
        .sum();
    let (rw, rh) = (iw - tw + 1, ih - th + 1);
    if t_norm2 / area < f64::EPSILON {
        return Some((1.0, (0, 0)));
    }
    let t_norm = t_norm2.sqrt();
    // integral images of the image and its squares
    let mut sum = vec![0f64; (iw + 1) * (ih + 1)];
    let mut sq = vec![0f64; (iw + 1) * (ih + 1)];
    for y in 0..ih {
        let (mut rs, mut rq) = (0f64, 0f64);
        for x in 0..iw {
            let v = img.data[y * iw + x] as f64;
            rs += v;
            rq += v * v;
            sum[(y + 1) * (iw + 1) + x + 1] = sum[y * (iw + 1) + x + 1] + rs;
            sq[(y + 1) * (iw + 1) + x + 1] = sq[y * (iw + 1) + x + 1] + rq;
        }
    }
    let window = |t: &[f64], x: usize, y: usize| {
        let s = iw + 1;
        t[(y + th) * s + x + tw] - t[y * s + x + tw] - t[(y + th) * s + x] + t[y * s + x]
    };
    let tc: Vec<f64> = templ.data.iter().map(|&v| v as f64 - t_mean).collect();
    // The correlation of every position at once, template pixel by template
    // pixel: each position still adds its terms in (row, column) order, and
    // the inner loop runs along a row of positions, which vectorises.
    let mut nums = vec![0f64; rw * rh];
    for ty in 0..th {
        for tx in 0..tw {
            let t = tc[ty * tw + tx];
            for y in 0..rh {
                let src = &img.data[(y + ty) * iw + tx..(y + ty) * iw + tx + rw];
                for (n, &v) in nums[y * rw..(y + 1) * rw].iter_mut().zip(src) {
                    *n += v as f64 * t;
                }
            }
        }
    }
    let mut best: Option<(f64, (i32, i32))> = None;
    for y in 0..rh {
        for x in 0..rw {
            let num = nums[y * rw + x];
            let s = window(&sum, x, y);
            let wnd_mean2 = s * s / area;
            let wnd_sum2 = window(&sq, x, y);
            let diff2 = (wnd_sum2 - wnd_mean2).max(0.0);
            let t = if diff2 <= 0.5f64.min(10.0 * f32::EPSILON as f64 * wnd_sum2) {
                0.0
            } else {
                diff2.sqrt() * t_norm
            };
            let v = if num.abs() < t {
                num / t
            } else if num.abs() < t * 1.125 {
                if num > 0.0 { 1.0 } else { -1.0 }
            } else {
                0.0
            };
            if best.is_none_or(|b| v > b.0) {
                best = Some((v, (x as i32, y as i32)));
            }
        }
    }
    best
}
