//! IRIS's mining signal reader (Core/Mining of github.com/Niteb1rd/IRIS,
//! GPL-3.0, at commit aec86f08 - unchanged in 2.1 but for a size limit on
//! the table file): the scanner's signal number read off the HUD, steadied
//! over frames, and named from a table of rock signals.
//!
//! Step 1 of the port (PORT.md, "The mining overlay"): the reading logic -
//! the table ([`table`]), the digit shapes and how a cell is matched
//! ([`digits`]), snapping a doubtful digit to the table ([`snap`]), fusing
//! reads ([`evidence`]) and the label's state ([`state`]). Held to IRIS's
//! own answers to the bit (tests/mining_units.rs). The reader on images
//! comes next.
//!
//! Doubles are compared and combined as .NET does ([`net_max`],
//! [`net_min`], [`round_even`]) so a tie or a signed zero goes the same way.

pub mod digits;
pub mod evidence;
pub mod snap;
pub mod state;
pub mod table;

/// `MiningSettings`, IRIS's defaults. Pixel values are for a 1440-line
/// window and scale with it.
#[derive(Clone, Debug, PartialEq)]
pub struct MiningSettings {
    pub lock_hz: f64,
    pub hold_ms: f64,
    pub off_table_reads: u32,
    pub change_reads: u32,
    pub clear_digit_margin: f64,
    pub confident_margin: f64,
    pub snap_choice_margin: f64,
    pub average_frames: usize,
    pub fuse_reads: usize,
    pub geometry_reads: usize,
    pub geometry_min_reads: usize,
    pub geometry_change_px: f64,
    pub label_size_change_px: f64,
    pub label_size_change_share: f64,
    pub line_height_at_1440: f64,
    pub row_centre_y: f64,
    pub search_above_lines: f64,
    pub search_below_lines: f64,
    pub row_search_half_lines: f64,
    pub pin_height_min: i32,
    pub pin_height_max: i32,
    pub pin_min_score: f64,
    pub pin_digit_ratio: f64,
    pub small_row_lines: f64,
    pub lock_above_lines: f64,
    pub lock_below_lines: f64,
    pub left_lines: f64,
    pub right_lines: f64,
    pub centre_offset_lines: f64,
    pub centre_tolerance_lines: f64,
    pub min_digit_score: f64,
    pub tie_margin: f64,
    pub split_width_lines: f64,
    pub split_whole_max_lines: f64,
    pub condensed_min_red: f64,
    pub condensed_green_share: f64,
    pub condensed_blue_share: f64,
    pub condensed_narrow_share: f64,
    pub condensed_one_share: f64,
    pub condensed_pair_share: f64,
    pub condensed_pitch_share: f64,
    pub condensed_cell_lines: f64,
    pub condensed_min_ink_pixels: i64,
    pub condensed_shape_min_share: f64,
    pub condensed_shape_max_share: f64,
    pub condensed_max_shape_lines: f64,
    pub condensed_cut_reach: f64,
    pub min_ink_threshold: f64,
    pub label_gap_lines: f64,
    pub label_pad_x_lines: f64,
    pub label_pad_y_lines: f64,
    pub label_back_alpha: u8,
    pub colour_blend: f64,
    pub colour_step: i32,
    pub colour_update_ms: f64,
    pub min_contrast: f64,
}

impl Default for MiningSettings {
    fn default() -> Self {
        Self {
            lock_hz: 12.0,
            hold_ms: 1000.0,
            off_table_reads: 2,
            change_reads: 2,
            clear_digit_margin: 0.08,
            confident_margin: 0.08,
            snap_choice_margin: 0.08,
            average_frames: 4,
            fuse_reads: 5,
            geometry_reads: 5,
            geometry_min_reads: 3,
            geometry_change_px: 1.0,
            label_size_change_px: 2.0,
            label_size_change_share: 0.15,
            line_height_at_1440: 12.0,
            row_centre_y: 472.0,
            search_above_lines: 4.0,
            search_below_lines: 11.0,
            row_search_half_lines: 4.0,
            pin_height_min: 13,
            pin_height_max: 26,
            pin_min_score: 0.7,
            pin_digit_ratio: 0.5,
            small_row_lines: 0.75,
            lock_above_lines: 1.5,
            lock_below_lines: 2.5,
            left_lines: 6.0,
            right_lines: 11.0,
            centre_offset_lines: 0.4,
            centre_tolerance_lines: 1.0,
            min_digit_score: 0.55,
            tie_margin: 0.08,
            split_width_lines: 0.8,
            split_whole_max_lines: 1.05,
            condensed_min_red: 150.0,
            condensed_green_share: 0.7,
            condensed_blue_share: 0.4,
            condensed_narrow_share: 0.52,
            condensed_one_share: 0.5,
            condensed_pair_share: 0.6,
            condensed_pitch_share: 0.55,
            condensed_cell_lines: 0.45,
            condensed_min_ink_pixels: 4,
            condensed_shape_min_share: 0.35,
            condensed_shape_max_share: 0.8,
            condensed_max_shape_lines: 2.6,
            condensed_cut_reach: 0.2,
            min_ink_threshold: 12.0,
            label_gap_lines: 0.6,
            label_pad_x_lines: 0.5,
            label_pad_y_lines: 0.25,
            label_back_alpha: 0x80,
            colour_blend: 0.3,
            colour_step: 12,
            colour_update_ms: 250.0,
            min_contrast: 3.0,
        }
    }
}

/// What a read found (`SignalState`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignalState {
    None,
    /// A signal row, but off centre or beside another: not the scanner's.
    Disqualified,
    /// The scanner's row, but not a number.
    Unreadable,
    Signal,
}

impl SignalState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Disqualified => "Disqualified",
            Self::Unreadable => "Unreadable",
            Self::Signal => "Signal",
        }
    }
}

/// A colour as the frame stores it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bgr {
    pub b: u8,
    pub g: u8,
    pub r: u8,
}

impl Bgr {
    pub const fn new(b: u8, g: u8, r: u8) -> Self {
        Self { b, g, r }
    }
}

/// A rectangle in window pixels, fractional (IRIS's `PixelRect`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PixelRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl PixelRect {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
}

/// One read of the signal row (`SignalRead`).
#[derive(Clone, Debug, PartialEq)]
pub struct SignalRead {
    pub state: SignalState,
    /// The digits and commas as read.
    pub text: String,
    pub value: Option<i32>,
    /// The icon and the digits, in window pixels.
    pub signal_box: PixelRect,
    pub row_top: f64,
    pub digit_height: f64,
    pub text_colour: Bgr,
    pub back_colour: Bgr,
    /// How far the row's centre sits from where the scanner puts it.
    pub offset_px: f64,
    pub min_score: f64,
    /// Each digit's score against the ten digits.
    pub digit_scores: Option<Vec<[f64; 10]>>,
}

impl SignalRead {
    /// `SignalRead.Nothing`.
    pub fn nothing() -> Self {
        Self {
            state: SignalState::None,
            text: String::new(),
            value: None,
            signal_box: PixelRect::default(),
            row_top: f64::NAN,
            digit_height: 0.0,
            text_colour: Bgr::default(),
            back_colour: Bgr::default(),
            offset_px: f64::NAN,
            min_score: f64::NAN,
            digit_scores: None,
        }
    }

    /// A signal row was there, read or not.
    pub fn found(&self) -> bool {
        matches!(self.state, SignalState::Signal | SignalState::Unreadable)
    }
}

/// `MiningSignalReader.IsNumber`: 4 to 6 digits, or digits grouped by
/// commas in threes (a first group of 1 to 3).
pub fn is_number(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let digits = |s: &str| s.bytes().all(|c| c.is_ascii_digit());
    if (4..=6).contains(&text.len()) && digits(text) {
        return true;
    }
    let mut parts = text.split(',');
    let first = parts.next().unwrap_or_default();
    (1..=3).contains(&first.len()) && digits(first) && parts.all(|p| p.len() == 3 && digits(p))
}

/// `int.TryParse(s, NumberStyles.None, InvariantCulture)`: ASCII digits
/// only, at least one, within an i32.
pub fn parse_digits(s: &str) -> Option<i32> {
    if s.is_empty() || !s.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    s.bytes().try_fold(0i32, |v, c| {
        v.checked_mul(10)?.checked_add(i32::from(c - b'0'))
    })
}

/// A signal's text as a number: the commas dropped, then [`parse_digits`].
pub fn parse_signal(text: &str) -> Option<i32> {
    parse_digits(&text.replace(',', ""))
}

/// .NET's `Math.Max(double, double)`: NaN wins, and +0 beats -0.
pub fn net_max(a: f64, b: f64) -> f64 {
    if a > b || a.is_nan() {
        a
    } else if a == b {
        if a.is_sign_negative() { b } else { a }
    } else {
        b
    }
}

/// .NET's `Math.Min(double, double)`: NaN wins, and -0 beats +0.
pub fn net_min(a: f64, b: f64) -> f64 {
    if a < b || a.is_nan() {
        a
    } else if a == b {
        if a.is_sign_negative() { a } else { b }
    } else {
        b
    }
}

/// .NET's `Math.Round(double)`: halves to even.
pub fn round_even(v: f64) -> f64 {
    v.round_ties_even()
}

/// Standard base64 (with padding), for the digit shapes' text.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let bytes = text.trim().as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let pad = chunk.iter().rev().take_while(|&&c| c == b'=').count();
        let mut n = 0u32;
        for &c in &chunk[..4 - pad] {
            n = (n << 6) | value(c)?;
        }
        n <<= 6 * pad as u32;
        let three = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        out.extend_from_slice(&three[..3 - pad]);
    }
    Some(out)
}
