//! IRIS's two app-level operations on a still frame (`DetectorImageOps`):
//! scan the capture area for distance lines, and read the plate above one
//! line - plus the scan geometry, the read crop (`RegionPlanner.Crop` with no
//! motion) and the plate tint (`PlateColors.Tint`).

use super::detector::{Candidates, DetectorSettings, NameplateDetector};
use super::features::LineMask;
use super::{NameRead, RowReader};
use crate::{Image, Rect};
use std::collections::{HashMap, HashSet};

// IRIS PR #1: a read crop is 'bright' when at least this share of its pixels
// has a channel at or above this value (snow, sunlit hulls). An off-centre
// line over it is unsure, not a refusal: the bright ground may be what broke
// the plate apart.
const BRIGHT_BACKGROUND_VALUE: u8 = 170;
const BRIGHT_BACKGROUND_SHARE: f64 = 0.5;

// PipelineSettings: how the app cuts the crop it reads a plate from.
const READ_MARGIN_PX: f64 = 8.0;
const PATCH_MIN_WIDTH_PX: f64 = 40.0;
const CROP_HALF_WIDTH_PX: f64 = 110.0;
const CROP_HALF_WIDTH_LINES: f64 = 3.0;
const CROP_SLACK_PX: f64 = 12.0;
const CROP_ABOVE_LINES: f64 = 7.0;
const CROP_BELOW_LINES: f64 = 2.0;

const PLATE_COLOR_MIN_PIXELS: usize = 30;

pub fn has_bright_background(image: &Image<u8>) -> bool {
    let cn = image.channels.min(3);
    let bright = image
        .data
        .chunks_exact(image.channels)
        .filter(|p| p[..cn].iter().any(|&v| v >= BRIGHT_BACKGROUND_VALUE))
        .count();
    bright as f64 >= BRIGHT_BACKGROUND_SHARE * (image.width * image.height) as f64
}

// ---- plate tint ------------------------------------------------------------------------

/// The value at sorted position `index` of a histogram's samples.
fn value_at(hist: &[usize; 256], index: usize) -> usize {
    let mut seen = 0;
    for (v, &n) in hist.iter().enumerate() {
        seen += n;
        if seen > index {
            return v;
        }
    }
    255
}

fn median_hist(hist: &[usize; 256], count: usize) -> f64 {
    if count % 2 == 1 {
        value_at(hist, count / 2) as f64
    } else {
        (value_at(hist, count / 2 - 1) + value_at(hist, count / 2)) as f64 / 2.0
    }
}

/// The plate's colour as (b, g, r) over its brightest channel: the median of
/// the brighter half of the ink.
pub fn plate_tint(mask: Option<&LineMask>) -> Option<(f64, f64, f64)> {
    let c = mask?.ink_colors.as_ref()?;
    let total = c.len();
    if total < PLATE_COLOR_MIN_PIXELS {
        return None;
    }
    let mut hist = [0usize; 256];
    for p in c {
        hist[*p.iter().max().unwrap() as usize] += 1;
    }
    let threshold = value_at(&hist, total / 2) as u8;
    let mut hb = [[0usize; 256]; 3];
    let mut n = 0;
    for p in c.iter().filter(|p| *p.iter().max().unwrap() >= threshold) {
        for k in 0..3 {
            hb[k][p[k] as usize] += 1;
        }
        n += 1;
    }
    let (b, g, r) = (
        median_hist(&hb[0], n),
        median_hist(&hb[1], n),
        median_hist(&hb[2], n),
    );
    let top = r.max(g).max(b);
    if top <= 0.0 {
        None
    } else {
        Some((b / top, g / top, r / top))
    }
}

pub fn same_tint(a: (f64, f64, f64), b: (f64, f64, f64), tol: f64) -> bool {
    (a.0 - b.0).abs() <= tol && (a.1 - b.1).abs() <= tol && (a.2 - b.2).abs() <= tol
}

// ---- scan geometry and the read crop ------------------------------------------------------

/// (left, top, right, bottom) fractions from margins in percent, each
/// clamped to 0-30 % as IRIS's TrackingBoundary does.
pub fn boundary_from_percents(
    top: i32,
    bottom: i32,
    left: i32,
    right: i32,
) -> (f64, f64, f64, f64) {
    let lim = |v: i32| v.clamp(0, 30) as f64;
    (
        lim(left) / 100.0,
        lim(top) / 100.0,
        (100.0 - lim(right)) / 100.0,
        (100.0 - lim(bottom)) / 100.0,
    )
}

/// The part of the window the scan looks at, and the settings that put the
/// region where it was inside that capture.
#[derive(Clone, Debug, PartialEq)]
pub struct ScanGeometry {
    pub width: i32,
    pub height: i32,
    pub capture: Rect,
    pub settings: DetectorSettings,
}

impl ScanGeometry {
    pub fn create(
        width: i32,
        height: i32,
        base: &DetectorSettings,
        boundary: Option<(f64, f64, f64, f64)>,
    ) -> Option<Self> {
        let mut base = base.clone();
        if let Some((l, t, r, b)) = boundary {
            (
                base.region_left,
                base.region_top,
                base.region_right,
                base.region_bottom,
            ) = (l, t, r, b);
        }
        let x0 = (base.region_left * width as f64) as i32;
        let x1 = width.min((base.region_right * width as f64) as i32);
        let y0 = (base.region_top * height as f64) as i32;
        let y1 = height.min((base.region_bottom * height as f64) as i32);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        let pad = base.context_padding(height);
        let vpad = base.vertical_context_padding(height);
        let (cx0, cx1) = ((x0 - pad).max(0), width.min(x1 + pad));
        let (cy0, cy1) = ((y0 - vpad).max(0), height.min(y1 + vpad));
        let (w, h) = ((cx1 - cx0) as f64, (cy1 - cy0) as f64);
        let mut settings = base;
        settings.region_left = ((x0 - cx0) as f64 + 0.25) / w;
        settings.region_right = (((x1 - cx0) as f64 + 0.25) / w).min(1.0);
        settings.region_top = ((y0 - cy0) as f64 + 0.25) / h;
        settings.region_bottom = (((y1 - cy0) as f64 + 0.25) / h).min(1.0);
        settings.scale_height = height;
        Some(Self {
            width,
            height,
            capture: Rect::new(cx0, cy0, cx1 - cx0, cy1 - cy0),
            settings,
        })
    }
}

/// The crop the app reads a plate from (RegionPlanner.Crop, no motion).
pub fn read_crop_rect(line: Rect, width: i32, height: i32) -> Option<Rect> {
    let scale = height as f64 / 1440.0;
    let lh = line.h as f64;
    let margin = READ_MARGIN_PX * scale;
    let cx = line.x as f64 + (line.w as f64).max(PATCH_MIN_WIDTH_PX * scale) / 2.0;
    let half =
        (CROP_HALF_WIDTH_PX * scale + CROP_HALF_WIDTH_LINES * lh + CROP_SLACK_PX * scale + margin)
            as i32;
    // Python's int() truncates toward zero; the max(0, ...) makes that and
    // floor agree here.
    let x0 = ((cx - half as f64) as i32).max(0);
    let x1 = width.min((cx + half as f64) as i32);
    let y0 = ((line.y as f64 - CROP_ABOVE_LINES * lh - margin) as i32).max(0);
    let y1 = height.min((line.y as f64 + lh + CROP_BELOW_LINES * lh + margin) as i32);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some(Rect::new(x0, y0, x1 - x0, y1 - y0))
}

// ---- scan and read ----------------------------------------------------------------------

/// What reading a plate concluded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadClass {
    /// A name was read.
    Name,
    /// The plate is not a player's (or not readable as one): stop following it.
    Stop,
    /// Not sure: look again.
    Unclear,
}

impl ReadClass {
    pub fn as_str(self) -> &'static str {
        match self {
            ReadClass::Name => "Name",
            ReadClass::Stop => "Stop",
            ReadClass::Unclear => "Unclear",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReadResult {
    pub class: ReadClass,
    /// Why, for Stop and Unclear (IRIS's reason words).
    pub reason: &'static str,
    pub name: Option<String>,
    /// The name rows, window coordinates.
    pub rows: Option<Vec<Rect>>,
    /// The distance line, window coordinates.
    pub anchor: Option<Rect>,
}

impl ReadResult {
    fn no(class: ReadClass, reason: &'static str) -> Self {
        Self {
            class,
            reason,
            name: None,
            rows: None,
            anchor: None,
        }
    }
}

/// DetectorImageOps: scan a frame for distance lines, read one plate.
pub struct ImageOps {
    pub rows: RowReader,
    pub geometry: ScanGeometry,
    pub scan_detector: NameplateDetector,
    pub crop_detector: NameplateDetector,
}

impl ImageOps {
    pub fn new(rows: RowReader, geometry: ScanGeometry) -> Self {
        let mut crop = geometry.settings.clone();
        (
            crop.region_left,
            crop.region_right,
            crop.region_top,
            crop.region_bottom,
        ) = (0.0, 1.0, 0.0, 1.0);
        crop.scale_height = geometry.height;
        Self {
            scan_detector: NameplateDetector::new(geometry.settings.clone()),
            crop_detector: NameplateDetector::new(crop),
            rows,
            geometry,
        }
    }

    fn px(&self, at1440: f64) -> f64 {
        at1440 * self.geometry.height as f64 / 1440.0
    }

    /// The capture area of a window-sized frame.
    pub fn capture_of(&self, frame: &Image<u8>) -> Image<u8> {
        let c = self.geometry.capture;
        frame.crop(c.x as usize, c.y as usize, c.w as usize, c.h as usize)
    }

    /// Distance lines in the frame's capture area, window coordinates
    /// (DetectorImageOps.Scan with nothing tracked).
    pub fn scan(&self, frame: &Image<u8>) -> Vec<Rect> {
        let c = self.geometry.capture;
        let img = self.capture_of(frame);
        let cands = self.scan_detector.find_candidates(&img);
        self.scan_candidates(&img, &cands)
            .into_iter()
            .map(|b| Rect::new(b.x + c.x, b.y + c.y, b.w, b.h))
            .collect()
    }

    /// The distance lines among one image's candidates, that image's coordinates.
    pub fn scan_candidates(&self, img: &Image<u8>, cands: &Candidates) -> Vec<Rect> {
        let boxes = &cands.boxes;
        let memo: std::cell::RefCell<HashMap<usize, bool>> = Default::default();
        let distance_ok = |line: usize| -> bool {
            if let Some(&v) = memo.borrow().get(&line) {
                return v;
            }
            let v = self.rows.read_distance(img, boxes[line]).is_some();
            memo.borrow_mut().insert(line, v);
            v
        };
        let crowd: HashMap<(usize, usize), &Vec<usize>> = cands
            .crowd
            .iter()
            .map(|(t, b, nb)| ((*t, *b), nb))
            .collect();
        let ordered: Vec<(usize, usize)> = cands
            .pairs
            .iter()
            .copied()
            .chain(cands.crowd.iter().map(|(t, b, _)| (*t, *b)))
            .collect();
        let mut line_pairs: HashMap<usize, Vec<(usize, usize)>> = HashMap::new();
        for &pair in ordered.iter().chain(&cands.loose) {
            line_pairs.entry(pair.0).or_default().push(pair);
            line_pairs.entry(pair.1).or_default().push(pair);
        }
        let mut taken = HashSet::new();
        let mut found = Vec::new();
        for &(top, bottom) in &ordered {
            if taken.contains(&bottom) {
                continue;
            }
            if let Some(nb) = crowd.get(&(top, bottom)) {
                if !distance_ok(bottom) {
                    continue;
                }
                let supported = nb.iter().all(|line| {
                    line_pairs.get(line).is_some_and(|ps| {
                        ps.iter().any(|&(ot, ob)| {
                            ot != top
                                && ot != bottom
                                && ob != top
                                && ob != bottom
                                && distance_ok(ob)
                        })
                    })
                });
                if !supported {
                    continue;
                }
            }
            if distance_ok(bottom) {
                taken.insert(bottom);
                found.push(boxes[bottom]);
            }
        }
        found
    }

    /// Reads the plate above a distance line (window coordinates).
    pub fn read_line(&self, frame: &Image<u8>, line: Rect) -> ReadResult {
        let Some(crop) = read_crop_rect(line, frame.width as i32, frame.height as i32) else {
            return ReadResult::no(ReadClass::Unclear, "nocrop");
        };
        let image = frame.crop(
            crop.x as usize,
            crop.y as usize,
            crop.w as usize,
            crop.h as usize,
        );
        self.read(&image, crop, line, line.h as f64)
    }

    /// DetectorImageOps.Read: the distance line near where it was expected,
    /// the name line above it, then the plate's rows.
    pub fn read(&self, image: &Image<u8>, origin: Rect, expected: Rect, lh: f64) -> ReadResult {
        let cs = &self.crop_detector.settings;
        if (image.height as f64) < self.px(cs.read_crop_min_height)
            || (image.width as f64) < self.px(cs.read_crop_min_width)
        {
            return ReadResult::no(ReadClass::Unclear, "offscreen");
        }
        let cands = self.crop_detector.find_candidates(image);
        let boxes = &cands.boxes;
        let (ox, oy) = (origin.x as f64, origin.y as f64);
        let tol = cs.near_height_min_px.max(cs.near_height_tolerance * lh);
        let centre = |r: Rect| r.x as f64 + r.w as f64 / 2.0;
        let mut anchors: Vec<usize> = (0..boxes.len())
            .filter(|&i| {
                let b = boxes[i];
                let off_left = (b.x as f64 + ox - expected.x as f64).abs();
                let off_centre = (centre(b) + ox - centre(expected)).abs();
                off_left.min(off_centre) <= self.px(cs.anchor_max_offset_px)
                    && (b.y as f64 + oy - expected.y as f64).abs()
                        <= cs.anchor_max_row_offset_lines * lh
                    && (b.h as f64 - lh).abs() <= tol
            })
            .collect();
        anchors.sort_by_key(|&i| std::cmp::Reverse(boxes[i].w));
        let Some(bi) = anchors
            .iter()
            .take(2)
            .copied()
            .find(|&i| self.rows.read_distance(image, boxes[i]).is_some())
        else {
            return ReadResult::no(ReadClass::Unclear, "nodist");
        };
        let bottom = boxes[bi];
        let bottom_centre = centre(bottom);
        let mut tops: Vec<(i32, usize)> = Vec::new();
        for (ti, &t) in boxes.iter().enumerate() {
            if ti == bi {
                continue;
            }
            let gap = bottom.y - (t.y + t.h);
            if !(1 <= gap && gap as f64 <= t.h as f64 * cs.top_gap_max_lines)
                || ((bottom.h - t.h).abs() as f64)
                    > self
                        .px(cs.top_height_diff_min_px)
                        .max(t.h as f64 * cs.top_height_diff_lines)
            {
                continue;
            }
            if t.x >= bottom.x + bottom.w || t.x + t.w <= bottom.x {
                continue;
            }
            tops.push((gap, ti));
        }
        tops.sort_by_key(|g| g.0);
        let tol_c = self.px(cs.top_centre_tolerance_px);
        let is_centred = |i: usize| (centre(boxes[i]) - bottom_centre).abs() <= tol_c;
        let centred: Vec<usize> = tops
            .iter()
            .map(|g| g.1)
            .filter(|&i| is_centred(i))
            .collect();
        let off = tops.iter().any(|g| !is_centred(g.1));
        let left_aligned = |t: Rect| {
            ((t.x - bottom.x).abs() as f64) <= self.px(cs.top_left_tolerance_px)
                && ((t.w - bottom.w).abs() as f64) >= self.px(cs.top_left_width_difference_px)
        };
        if let Some(&top) = centred.iter().find(|&&i| !left_aligned(boxes[i])) {
            let result = self.read_pair(image, origin, &cands, top, bi);
            let strict = cands.pairs.contains(&(top, bi))
                || cands.crowd.iter().any(|c| c.0 == top && c.1 == bi);
            if result.class != ReadClass::Name && !strict {
                return ReadResult::no(ReadClass::Unclear, "clutter");
            }
            return result;
        }
        if !centred.is_empty() {
            return ReadResult::no(ReadClass::Unclear, "leftcol");
        }
        if off {
            let class = if has_bright_background(image) {
                ReadClass::Unclear
            } else {
                ReadClass::Stop
            };
            return ReadResult::no(class, "offcenter");
        }
        ReadResult::no(ReadClass::Unclear, "nothing")
    }

    fn reads_as_name_not_distance(&self, image: &Image<u8>, row: Rect) -> bool {
        let mask = self.rows.mask(image, row, true);
        self.rows.read_name_mask(image, mask.as_ref()).usable()
            && self.rows.read_distance_mask(mask.as_ref()).is_none()
    }

    fn read_pair(
        &self,
        image: &Image<u8>,
        origin: Rect,
        cands: &Candidates,
        top: usize,
        bottom: usize,
    ) -> ReadResult {
        let cs = &self.crop_detector.settings;
        let boxes = &cands.boxes;
        let stack = cands.stack_of(boxes[top], boxes[bottom]);
        let rows: Vec<Rect> = match stack {
            Some((r, _)) if !r.is_empty() => r.clone(),
            _ => vec![boxes[top]],
        };
        let mut blockers: Vec<Rect> = stack.map(|s| s.1.clone()).unwrap_or_default();
        let name_box = boxes[top];
        let name_read = self.rows.read_name(image, name_box);
        if name_read.text.is_empty() {
            return ReadResult::no(ReadClass::Unclear, "unreadable");
        }
        if !name_read.one_word {
            return ReadResult::no(ReadClass::Stop, "notoneword");
        }
        let tint = plate_tint(name_read.mask.as_ref());
        let mut above: Vec<Rect> = rows.iter().copied().filter(|&r| r != name_box).collect();
        above.sort_by_key(|r| std::cmp::Reverse(r.y));
        let mut kept_rows = vec![name_box];
        let mut reads: Vec<NameRead> = vec![name_read.clone()];
        let mut whole = above.len() == rows.len() - 1;
        for &row in &above {
            let read = self.rows.read_name(image, row);
            let row_tint = plate_tint(read.mask.as_ref());
            if let (Some(a), Some(b)) = (tint, row_tint)
                && !same_tint(a, b, cs.stack_tint_tolerance)
            {
                whole = false;
                break;
            }
            if read.text.is_empty() {
                kept_rows = vec![name_box];
                reads = vec![name_read.clone()];
                whole = false;
                break;
            }
            if !read.one_word {
                return ReadResult::no(ReadClass::Unclear, "multiword");
            }
            kept_rows.insert(0, row);
            reads.insert(0, read);
        }
        if !whole {
            blockers.clear();
        }
        let text: String = reads.iter().map(|r| r.text.as_str()).collect();
        if reads.iter().any(|r| !r.valid_chars) {
            return ReadResult::no(ReadClass::Stop, "badchars");
        }
        let total: f64 = reads.iter().map(|r| r.score).sum();
        let len = text.chars().count();
        if len > cs.max_name_length {
            return ReadResult::no(ReadClass::Stop, "toolong");
        }
        if len < cs.min_name_length {
            return ReadResult::no(ReadClass::Unclear, "short");
        }
        if blockers
            .iter()
            .any(|&b| self.reads_as_name_not_distance(image, b))
        {
            return ReadResult::no(ReadClass::Unclear, "partialstack");
        }
        if text.is_empty() || text.starts_with('-') || text.ends_with('-') {
            return ReadResult::no(ReadClass::Unclear, "edgehyphen");
        }
        if total / (len as f64) < cs.min_name_score {
            return ReadResult::no(ReadClass::Unclear, "lowconf");
        }
        let win = |r: Rect| Rect::new(r.x + origin.x, r.y + origin.y, r.w, r.h);
        ReadResult {
            class: ReadClass::Name,
            reason: "",
            name: Some(text),
            rows: Some(kept_rows.into_iter().map(win).collect()),
            anchor: Some(win(boxes[bottom])),
        }
    }
}
