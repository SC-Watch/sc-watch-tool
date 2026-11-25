//! What is locked and shown: the locked row (`MiningState`), the label's
//! changes over reads (`MiningLabelState`), and the locked geometry
//! following the HUD by medians (`LockGeometry`). From IRIS's
//! MiningState.cs and LockGeometry.cs.

use super::evidence::readable;
use super::table::MiningTable;
use super::{Bgr, MiningSettings, PixelRect, SignalRead, SignalState, net_max};

/// `MiningState`: the row the scanner's signal was locked on, and what its
/// label says.
#[derive(Clone, Debug, PartialEq)]
pub struct MiningState {
    pub locked_ms: f64,
    pub row_top: f64,
    pub signal_box: PixelRect,
    pub digit_height: f64,
    pub text_colour: Bgr,
    pub back_colour: Bgr,
    /// The value shown - only one the table names.
    pub value: Option<i32>,
    pub label: Option<String>,
}

impl MiningState {
    /// `MiningState.FromLock`.
    pub fn from_lock(read: &SignalRead, frame_ms: f64, table: &MiningTable) -> Self {
        let label = read.value.and_then(|v| table.lookup(v)).map(str::to_string);
        let back = readable(
            read.text_colour,
            read.back_colour,
            MiningSettings::default().min_contrast,
        );
        Self {
            locked_ms: frame_ms,
            row_top: read.row_top,
            signal_box: read.signal_box,
            digit_height: read.digit_height,
            text_colour: read.text_colour,
            back_colour: back,
            value: if label.is_some() { read.value } else { None },
            label,
        }
    }
}

/// `MiningLabelState`: the label over a lock's reads. A new value needs
/// `change_reads` agreeing reads (one when clearly different from the one
/// shown, one when none is shown and the table names it, else
/// `off_table_reads`); the label goes after `hold_ms` without its value,
/// and the lock ends after `hold_ms` without the row.
#[derive(Clone, Debug)]
pub struct MiningLabelState {
    hold_ms: f64,
    change_reads: u32,
    off_table_reads: u32,
    clear_digit_margin: f64,
    last_seen_ms: f64,
    last_value_ms: f64,
    pending: Option<i32>,
    pending_count: u32,
    pub value: Option<i32>,
    pub label: Option<String>,
    pub ended: bool,
}

impl MiningLabelState {
    pub fn new(
        settings: &MiningSettings,
        locked_ms: f64,
        value: Option<i32>,
        label: Option<String>,
    ) -> Self {
        Self {
            hold_ms: settings.hold_ms,
            change_reads: settings.change_reads,
            off_table_reads: settings.off_table_reads,
            clear_digit_margin: settings.clear_digit_margin,
            last_seen_ms: locked_ms,
            last_value_ms: locked_ms,
            pending: None,
            pending_count: 0,
            value,
            label,
            ended: false,
        }
    }

    /// One read; true when what is shown changed (or the lock ended).
    pub fn apply(&mut self, read: &SignalRead, frame_ms: f64, table: &MiningTable) -> bool {
        if self.ended {
            return false;
        }
        let mut changed = false;
        match (read.state, read.value) {
            (SignalState::Signal, Some(value)) => {
                self.last_seen_ms = net_max(self.last_seen_ms, frame_ms);
                if Some(value) == self.value {
                    self.last_value_ms = net_max(self.last_value_ms, frame_ms);
                    self.pending = None;
                } else {
                    let label = table.lookup(value);
                    let needed = match self.value {
                        Some(shown) => {
                            if is_clearly_different(read, shown, self.clear_digit_margin) {
                                1
                            } else {
                                self.change_reads
                            }
                        }
                        None if label.is_some() => 1,
                        None => self.off_table_reads,
                    };
                    if self.pending == Some(value) {
                        self.pending_count += 1;
                    } else {
                        self.pending = Some(value);
                        self.pending_count = 1;
                    }
                    if self.pending_count >= needed {
                        self.pending = None;
                        self.last_value_ms = frame_ms;
                        let label = label.or_else(|| table.above_limit_label(value));
                        changed = self.show(Some(value), label.map(str::to_string));
                    }
                }
            }
            (SignalState::Unreadable, _) => {
                self.last_seen_ms = net_max(self.last_seen_ms, frame_ms);
            }
            (SignalState::Disqualified, _) => {
                self.last_seen_ms = net_max(self.last_seen_ms, frame_ms);
                self.pending = None;
                changed = self.show(None, None);
            }
            // nothing read - and, as in IRIS, a Signal without a value
            _ => {
                self.pending = None;
                if frame_ms - self.last_seen_ms >= self.hold_ms {
                    self.ended = true;
                    self.value = None;
                    self.label = None;
                    return true;
                }
            }
        }
        if self.value.is_some() && frame_ms - self.last_value_ms >= self.hold_ms {
            changed |= self.show(None, None);
        }
        changed
    }

    fn show(&mut self, value: Option<i32>, label: Option<String>) -> bool {
        let changed = value != self.value || label != self.label;
        self.value = value;
        self.label = label;
        changed
    }
}

/// `IsClearlyDifferent`: a read whose digits differ from the shown value's
/// in length, or where every differing digit beats the shown one by
/// `margin`.
pub fn is_clearly_different(read: &SignalRead, shown: i32, margin: f64) -> bool {
    let (Some(value), Some(scores)) = (read.value, read.digit_scores.as_ref()) else {
        return false;
    };
    let read_digits = value.to_string().into_bytes();
    let shown_digits = shown.to_string().into_bytes();
    if read_digits.len() != shown_digits.len() {
        return true;
    }
    if scores.len() != read_digits.len() {
        return false;
    }
    for (i, (&r, &s)) in read_digits.iter().zip(&shown_digits).enumerate() {
        if r == s {
            continue;
        }
        let ds = &scores[i];
        if ds[(r - b'0') as usize] - ds[(s - b'0') as usize] < margin {
            return false;
        }
    }
    true
}

/// `LockGeometry`: the medians of the last `geometry_reads` confident
/// reads' boxes, row tops and digit heights, once there are
/// `geometry_min_reads`, when they moved by `geometry_change_px` or more.
#[derive(Clone, Debug)]
pub struct LockGeometry {
    reads_max: usize,
    min_reads: usize,
    change_px: f64,
    size_change_px: f64,
    size_change_share: f64,
    reads: Vec<(PixelRect, f64, f64)>,
}

impl LockGeometry {
    pub fn new(settings: &MiningSettings) -> Self {
        Self {
            reads_max: settings.geometry_reads,
            min_reads: settings.geometry_min_reads,
            change_px: settings.geometry_change_px,
            size_change_px: settings.label_size_change_px,
            size_change_share: settings.label_size_change_share,
            reads: Vec::new(),
        }
    }

    pub fn clear(&mut self) {
        self.reads.clear();
    }

    /// The new (box, row top, digit height), or None when nothing moved.
    pub fn add(
        &mut self,
        signal_box: PixelRect,
        row_top: f64,
        digit_height: f64,
        locked: &MiningState,
    ) -> Option<(PixelRect, f64, f64)> {
        self.reads.push((signal_box, row_top, digit_height));
        if self.reads.len() > self.reads_max {
            self.reads.remove(0);
        }
        if self.reads.len() < self.min_reads {
            return None;
        }
        let median = |f: fn(&(PixelRect, f64, f64)) -> f64| -> f64 {
            let mut v: Vec<f64> = self.reads.iter().map(f).collect();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let mid = v.len() / 2;
            if v.len() % 2 == 1 {
                v[mid]
            } else {
                (v[mid - 1] + v[mid]) / 2.0
            }
        };
        let m = PixelRect::new(
            median(|r| r.0.x),
            median(|r| r.0.y),
            median(|r| r.0.w),
            median(|r| r.0.h),
        );
        let row = median(|r| r.1);
        let mut height = median(|r| r.2);
        if locked.digit_height > 0.0
            && (height - locked.digit_height).abs()
                < net_max(
                    self.size_change_px,
                    self.size_change_share * locked.digit_height,
                )
        {
            height = locked.digit_height;
        }
        let mv = self.change_px;
        let l = &locked.signal_box;
        let differs = (m.x - l.x).abs() >= mv
            || (m.y - l.y).abs() >= mv
            || (m.right() - l.right()).abs() >= mv
            || (m.y + m.h - (l.y + l.h)).abs() >= mv
            || (row - locked.row_top).abs() >= mv
            || (height - locked.digit_height).abs() >= mv;
        differs.then_some((m, row, height))
    }
}
