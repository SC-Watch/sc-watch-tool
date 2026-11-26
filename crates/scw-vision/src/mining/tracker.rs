//! The mining reader over time: IRIS's `ScanWorker.SearchMining` (look for
//! the signal while nothing is locked; lock on it) and `MiningWorker`
//! (read the locked row again and again; steady a doubtful read with the
//! average of the last frames and the fused scores of the last reads; snap
//! one doubtful digit to the table; change the label only on agreeing
//! reads; follow the HUD's colours and geometry; end the lock when the
//! signal is gone). Without IRIS's threads and capture requests: the caller
//! asks [`MiningTracker::wanted`] which part of the screen to capture, and
//! hands that crop to [`MiningTracker::feed`].

use super::evidence::{DigitEvidence, LabelColours, is_confident, min_margin};
use super::reader::{MiningSignalReader, lock_area, search_area, to_window_rect};
use super::state::{LockGeometry, MiningLabelState, MiningState};
use super::table::MiningTable;
use super::{Bgr, MiningSettings, PixelRect, SignalRead, SignalState, snap};
use crate::{Image, Rect};

/// `FrameAverage`: the last few crops of one region, averaged.
#[derive(Clone, Debug, Default)]
pub struct FrameAverage {
    max_frames: usize,
    frames: Vec<Image<u8>>,
    region: Option<Rect>,
}

impl FrameAverage {
    pub fn new(max_frames: usize) -> Self {
        Self {
            max_frames: max_frames.max(2),
            ..Self::default()
        }
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub fn clear(&mut self) {
        self.frames.clear();
    }

    /// A crop of `region`; a crop of another region starts over.
    pub fn add(&mut self, image: &Image<u8>, region: Rect) {
        if image.data.is_empty() {
            return;
        }
        if self.region != Some(region) {
            self.clear();
            self.region = Some(region);
        }
        let bgr = if image.channels == 4 {
            let data = image
                .data
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect();
            Image::from_vec(image.width, image.height, 3, data)
        } else {
            image.clone()
        };
        self.frames.push(bgr);
        while self.frames.len() > self.max_frames {
            self.frames.remove(0);
        }
    }

    /// The mean of the crops kept (summed in floats, scaled by 1/n and
    /// rounded half to even, as OpenCV's convertTo); None under two.
    pub fn average(&self) -> Option<Image<u8>> {
        let first = self.frames.first()?;
        if self.frames.len() < 2 {
            return None;
        }
        let mut sum = vec![0f32; first.data.len()];
        for f in &self.frames {
            if f.data.len() != sum.len() {
                return None;
            }
            for (s, &v) in sum.iter_mut().zip(&f.data) {
                *s += v as f32;
            }
        }
        let scale = (1.0 / self.frames.len() as f64) as f32;
        let data = sum
            .iter()
            .map(|&s| (s * scale).round_ties_even().clamp(0.0, 255.0) as u8)
            .collect();
        Some(Image::from_vec(
            first.width,
            first.height,
            first.channels,
            data,
        ))
    }
}

/// What the overlay draws (IRIS's `MiningLabelItem`): the label, under the
/// signal it names, in the HUD's colours.
#[derive(Clone, Debug, PartialEq)]
pub struct MiningLabel {
    pub text: String,
    pub signal: PixelRect,
    pub digit_height: f64,
    pub text_colour: Bgr,
    pub back_colour: Bgr,
}

/// What one fed crop did.
#[derive(Clone, Debug, PartialEq)]
pub enum MiningEvent {
    /// Nothing locked, nothing found.
    Searching,
    /// The signal was found and locked on (the read, and the label the
    /// table gives it, if any).
    Locked(SignalRead, Option<String>),
    /// A locked read; `changed` when what the overlay shows changed.
    Read {
        read: SignalRead,
        source: &'static str,
        changed: bool,
    },
    /// The signal has been gone for the hold time: unlocked.
    Ended,
}

/// The tracker: search, lock, read, label.
pub struct MiningTracker {
    pub reader: MiningSignalReader,
    table: MiningTable,
    locked: Option<MiningState>,
    state: Option<MiningLabelState>,
    colours: Option<LabelColours>,
    state_locked_ms: f64,
    average: FrameAverage,
    evidence: DigitEvidence,
    geometry: LockGeometry,
    last_confident: Option<i32>,
    last_frame_ms: f64,
}

impl MiningTracker {
    pub fn new(settings: MiningSettings, table: MiningTable) -> Self {
        Self {
            average: FrameAverage::new(settings.average_frames),
            evidence: DigitEvidence::new(settings.fuse_reads),
            geometry: LockGeometry::new(&settings),
            reader: MiningSignalReader::new(settings),
            table,
            locked: None,
            state: None,
            colours: None,
            state_locked_ms: f64::NAN,
            last_confident: None,
            last_frame_ms: f64::NAN,
        }
    }

    pub fn settings(&self) -> &MiningSettings {
        &self.reader.settings
    }

    /// A new table (an edited mining.txt) - used from the next read on.
    pub fn set_table(&mut self, table: MiningTable) {
        self.table = table;
    }

    pub fn table(&self) -> &MiningTable {
        &self.table
    }

    /// The lock, while there is one.
    pub fn locked(&self) -> Option<&MiningState> {
        self.locked.as_ref()
    }

    /// Forgets everything: nothing locked, nothing shown.
    pub fn reset(&mut self) {
        self.locked = None;
        self.state = None;
        self.colours = None;
        self.state_locked_ms = f64::NAN;
        self.forget_history();
        self.geometry.clear();
    }

    /// The part of a `width` x `height` window to capture next: the search
    /// area while nothing is locked, else the lock area around the row.
    pub fn wanted(&self, width: i32, height: i32) -> Rect {
        let s = &self.reader.settings;
        let area = match &self.locked {
            Some(l) => lock_area(width, height, l.row_top, s),
            None => search_area(width, height, s),
        };
        to_window_rect(area, width, height)
    }

    /// Reads per second it wants: searching IRIS's scan rate, locked its
    /// lock rate.
    pub fn rate_hz(&self) -> f64 {
        if self.locked.is_some() {
            self.reader.settings.lock_hz
        } else {
            4.0
        }
    }

    /// What the overlay should show now (IndexModel.MiningItem).
    pub fn label(&self) -> Option<MiningLabel> {
        let l = self.locked.as_ref()?;
        Some(MiningLabel {
            text: l.label.clone()?,
            signal: l.signal_box,
            digit_height: l.digit_height,
            text_colour: l.text_colour,
            back_colour: l.back_colour,
        })
    }

    /// One crop of `region` (what [`Self::wanted`] asked for) from a
    /// `width` x `height` window, taken at `frame_ms`; `now_ms` paces the
    /// colour changes.
    ///
    /// Not in IRIS, which reads without a break: frames more than the hold
    /// time apart (the watcher pauses mining for a burst, or the key paused
    /// it) end the lock first, as the signal going would have - the last
    /// reads say nothing about what is on screen now.
    pub fn feed(
        &mut self,
        crop: &Image<u8>,
        region: Rect,
        width: i32,
        height: i32,
        frame_ms: f64,
        now_ms: f64,
    ) -> MiningEvent {
        let gap = frame_ms - self.last_frame_ms;
        self.last_frame_ms = frame_ms;
        if gap > self.reader.settings.hold_ms && self.locked.is_some() {
            self.reset();
        }
        match self.locked.clone() {
            None => self.search(crop, region, width, height, frame_ms),
            Some(locked) => self.read(crop, region, width, height, frame_ms, now_ms, locked),
        }
    }

    /// `ScanWorker.SearchMining`.
    fn search(
        &mut self,
        crop: &Image<u8>,
        region: Rect,
        width: i32,
        height: i32,
        frame_ms: f64,
    ) -> MiningEvent {
        let read = self.reader.search(crop, region, width, height);
        if !read.found() {
            return MiningEvent::Searching;
        }
        let read = snap::apply(&read, &self.table, &self.reader.settings).unwrap_or(read);
        let state = MiningState::from_lock(&read, frame_ms, &self.table);
        let label = state.label.clone();
        self.locked = Some(state);
        MiningEvent::Locked(read, label)
    }

    /// `MiningWorker.Process`.
    #[allow(clippy::too_many_arguments)]
    fn read(
        &mut self,
        crop: &Image<u8>,
        region: Rect,
        width: i32,
        height: i32,
        frame_ms: f64,
        now_ms: f64,
        locked: MiningState,
    ) -> MiningEvent {
        let settings = self.reader.settings.clone();
        if self.state.is_none() || self.state_locked_ms != locked.locked_ms {
            self.state = Some(MiningLabelState::new(
                &settings,
                locked.locked_ms,
                locked.value,
                locked.label.clone(),
            ));
            self.colours = Some(LabelColours::new(
                &settings,
                locked.text_colour,
                locked.back_colour,
            ));
            self.state_locked_ms = locked.locked_ms;
            self.forget_history();
            self.geometry.clear();
        }
        let mut read = self.reader.read(crop, region, width, height);
        let mut source = self.improve(&mut read, crop, region, width, height);
        if let Some(snapped) = snap::apply(&read, &self.table, &settings) {
            read = snapped;
            source = match source {
                "average" => "average+table",
                "fused" => "fused+table",
                _ => "frame+table",
            };
        }
        let state = self.state.as_mut().expect("the label's state");
        let changed = state.apply(&read, frame_ms, &self.table);
        let ended = state.ended;
        let confident = is_confident(&read, settings.confident_margin);
        let colours = self.colours.as_mut().expect("the label's colours");
        let recoloured =
            !ended && confident && colours.add(read.text_colour, read.back_colour, now_ms);
        let geometry = if !ended && confident && read.state == SignalState::Signal {
            self.geometry
                .add(read.signal_box, read.row_top, read.digit_height, &locked)
        } else {
            None
        };
        if !changed && !recoloured && geometry.is_none() {
            return MiningEvent::Read {
                read,
                source,
                changed: false,
            };
        }
        if ended {
            self.locked = None;
            self.state = None;
            self.state_locked_ms = f64::NAN;
            return MiningEvent::Ended;
        }
        let state = self.state.as_ref().expect("the label's state");
        let colours = self.colours.as_ref().expect("the label's colours");
        let mut next = MiningState {
            value: state.value,
            label: state.label.clone(),
            text_colour: colours.text_colour,
            back_colour: colours.back_colour,
            ..locked
        };
        if let Some((b, row, dh)) = geometry {
            next.signal_box = b;
            next.row_top = row;
            next.digit_height = dh;
        }
        let label_changed = next.label != self.locked.as_ref().and_then(|l| l.label.clone());
        self.locked = Some(next);
        MiningEvent::Read {
            read,
            source,
            changed: changed || recoloured || geometry.is_some() || label_changed,
        }
    }

    /// `MiningWorker.Improve`: a doubtful read again from the average of
    /// the last frames, then from the fused scores of the last reads.
    fn improve(
        &mut self,
        read: &mut SignalRead,
        crop: &Image<u8>,
        region: Rect,
        width: i32,
        height: i32,
    ) -> &'static str {
        if matches!(read.state, SignalState::None | SignalState::Disqualified) {
            self.forget_history();
            return "frame";
        }
        let settings = &self.reader.settings;
        let confident = is_confident(read, settings.confident_margin);
        if confident && read.value != self.last_confident {
            self.forget_history();
            self.last_confident = read.value;
        }
        self.average.add(crop, region);
        let mut source = "frame";
        if !confident && let Some(mean) = self.average.average() {
            let again = self.reader.read(&mean, region, width, height);
            if again.state == SignalState::Signal
                && (read.state != SignalState::Signal || min_margin(&again) > min_margin(read))
            {
                *read = again;
                source = "average";
            }
        }
        self.evidence.add(read);
        if !is_confident(read, self.reader.settings.confident_margin)
            && let Some(fused) = self
                .evidence
                .fused(read, self.reader.settings.min_digit_score)
        {
            *read = fused;
            source = "fused";
        }
        source
    }

    fn forget_history(&mut self) {
        self.average.clear();
        self.evidence.reset();
        self.last_confident = None;
    }
}
