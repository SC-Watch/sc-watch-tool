//! The real parts behind the session's interfaces: the screen (or saved
//! frames), the IRIS reader, the legacy OCR reader, the chat reader, the
//! keys, the alert player and the lookup worker. Each is a thin adapter: the work is in the crates they wrap.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use scw_audio::{Facts, Reason};
use scw_capture::Source;
use scw_input::{Binding, InputState};
use scw_vision::iris::{DetectorSettings, LetterModel, NameplateReader, ReaderSettings, RowReader};
use scw_vision::pipeline::{Contact, FrameReader, ScanArea};
use scw_vision::{Image, Rect};
use serde_json::{Value, json};

use scw_ocr::Ocr;
use scw_ocr::chat::{self, ChatLine, Place};
use scw_ocr::legacy::{self, km_gate};

use crate::enricher::{EnricherThread, Pending};
use crate::session::{
    ChatReader, Detector, Frames, Keys, LookupQueue, Mode, OcrStages, ReadStats, Stop,
};
use crate::sinks::{AlertSink, AudioHealth, Console, Spoken};

/// IRIS's letter model (models/iris-letters-model.bin, GPL-3.0), built in.
pub const MODEL: &[u8] = include_bytes!("../../../models/iris-letters-model.bin");

/// The text-row reader with the built-in letter model.
pub fn row_reader() -> Result<Arc<RowReader>, String> {
    let model = LetterModel::from_bytes(MODEL).map_err(|e| e.to_string())?;
    Ok(Arc::new(RowReader::new(
        NameplateReader::new(model)?,
        ReaderSettings::default(),
    )))
}

/// IRIS alone (`--reader iris`): reads scan `full`, the always-on scan
/// scans `auto`. A focused or dual read is a full read: focused reads exist
/// to get past OCR's limits, and IRIS's plates never go to OCR.
pub struct IrisDetector {
    full: FrameReader,
    auto: FrameReader,
    last: Option<ReadStats>,
}

impl IrisDetector {
    pub fn new(rows: Arc<RowReader>, full: ScanArea, auto: ScanArea) -> Self {
        IrisDetector {
            full: FrameReader::new(rows.clone(), full, DetectorSettings::default()),
            auto: FrameReader::new(rows, auto, DetectorSettings::default()),
            last: None,
        }
    }
}

impl Detector for IrisDetector {
    fn detect(&mut self, frame: &Image<u8>, _mode: Mode) -> Vec<Contact> {
        let read = self.full.read(frame);
        self.last = Some(ReadStats {
            lines: read.lines,
            named: read.contacts.len(),
            refused: read
                .refused
                .iter()
                .map(|(r, n)| (r.to_string(), *n))
                .collect(),
        });
        read.contacts
    }

    fn last_read(&self) -> Option<ReadStats> {
        self.last.clone()
    }

    fn scan(&mut self, frame: &Image<u8>) -> Vec<Rect> {
        self.auto.scan(frame)
    }
}

/// The OCR (PP-OCRv3 under rten), built when something needs it - the
/// `ocr` reader or a chat key. Its own threads, half the processor's (at
/// least two), so the game keeps the rest.
pub fn ocr_engine() -> Result<Arc<Ocr>, String> {
    let n = std::thread::available_parallelism()
        .map_or(2, |n| n.get() / 2)
        .max(2);
    Ok(Arc::new(Ocr::embedded()?.with_threads(n)))
}

/// The legacy OCR reader (`--reader ocr`, scw_ocr::legacy): full reads
/// through the brightness masks, focused reads of the centre, dual reads
/// merged; the always-on scan is the monitor-rate pass (no OCR), its
/// positions the name boxes.
pub struct OcrDetector {
    ocr: Arc<Ocr>,
    opts: legacy::Options,
    /// The focused box; None sizes it from the screen.
    focus: Option<(usize, usize)>,
    /// Focused reads through the OCR's own detector (not `focus_mask`).
    direct: bool,
    /// An OCR failure, said once.
    failed: bool,
}

impl OcrDetector {
    pub fn new(
        ocr: Arc<Ocr>,
        opts: legacy::Options,
        focus: Option<(usize, usize)>,
        direct: bool,
    ) -> Self {
        OcrDetector {
            ocr,
            opts,
            focus,
            direct,
            failed: false,
        }
    }

    fn read(&self, frame: &Image<u8>, mode: Mode) -> Result<Vec<Contact>, String> {
        // Plate crops with the detector at their own size; the focused
        // read's lines enlarged to 736, as RapidOCR (PORT.md, OCR step 5).
        let mut ocr = legacy::Tuned::new(&self.ocr, scw_ocr::Limit::Min(736));
        let whole = |ocr: &mut legacy::Tuned| {
            legacy::detect(frame, ocr, frame.height, legacy::MAX_OCR_PAIRS, &self.opts)
        };
        let centre = |ocr: &mut legacy::Tuned| {
            legacy::detect_focused(frame, ocr, self.focus, self.direct, &self.opts)
        };
        match mode {
            Mode::Focus => centre(&mut ocr),
            Mode::Dual => {
                let w = whole(&mut ocr)?;
                Ok(legacy::merge_contacts(w, centre(&mut ocr)?, frame.height))
            }
            _ => whole(&mut ocr),
        }
    }
}

impl Detector for OcrDetector {
    fn detect(&mut self, frame: &Image<u8>, mode: Mode) -> Vec<Contact> {
        match self.read(frame, mode) {
            Ok(found) => found,
            Err(e) => {
                if !self.failed {
                    self.failed = true;
                    eprintln!("! OCR failed: {e}");
                }
                Vec::new()
            }
        }
    }

    fn scan(&mut self, frame: &Image<u8>) -> Vec<Rect> {
        legacy::cheap_pass(frame, self.opts.gate)
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }

    fn debug_stages(&mut self, frame: &Image<u8>) -> Option<OcrStages> {
        let h = frame.height;
        let boxes = legacy::find_boxes(&legacy::hud_mask(frame), h);
        let pairs = legacy::pair_boxes(&boxes, h);
        let gate_rejects = pairs
            .iter()
            .filter_map(|&(n, r)| {
                let s =
                    km_gate::score(&boxes[r].crop(frame, legacy::CROP_PAD), km_gate::template());
                let nb = boxes[n];
                (s < legacy::KM_GATE_THRESHOLD).then(|| {
                    (
                        format!("{}x{}@{},{}", nb.w, nb.h, nb.x, nb.y),
                        boxes[r].w,
                        s,
                    )
                })
            })
            .collect();
        Some(OcrStages {
            boxes: boxes.len(),
            pairs: pairs.len(),
            passed_gate: legacy::cheap_pass(frame, self.opts.gate).len(),
            near_misses: legacy::pair_diagnostics(&boxes, h, 14),
            gate_rejects,
        })
    }
}

/// The chat key's reader: chat.read_located through the OCR, the place
/// worked out from the frame or the region set.
pub struct OcrChat {
    pub ocr: Arc<Ocr>,
    pub region: Option<[f64; 4]>,
}

impl ChatReader for OcrChat {
    fn read(&mut self, frame: &Image<u8>) -> Result<(Vec<ChatLine>, Place), String> {
        let mut ocr = legacy::Tuned::new(&self.ocr, chat::CHAT_LIMIT);
        chat::read_located(
            frame,
            &mut ocr,
            self.region.map(|r| (r[0], r[1], r[2], r[3])),
        )
    }
}

/// A capture source as the session's frames. A capture that fails says so
/// once (and again only when the reason changes), then counts as no new
/// frame; release drops the source, handing the device back.
pub struct CaptureFrames {
    src: Option<Box<dyn Source>>,
    console: Arc<dyn Console>,
    said: Option<String>,
}

impl CaptureFrames {
    pub fn new(src: Box<dyn Source>, console: Arc<dyn Console>) -> Self {
        CaptureFrames {
            src: Some(src),
            console,
            said: None,
        }
    }
}

impl CaptureFrames {
    fn checked(
        &mut self,
        got: Result<Option<Image<u8>>, scw_capture::CaptureError>,
    ) -> Option<Image<u8>> {
        match got {
            Ok(f) => f,
            Err(e) => {
                let why = e.to_string();
                if self.said.as_deref() != Some(why.as_str()) {
                    self.console.say(&format!("  ! capture: {why}"));
                    self.said = Some(why);
                }
                None
            }
        }
    }
}

impl Frames for CaptureFrames {
    fn grab(&mut self) -> Option<Image<u8>> {
        let got = self.src.as_mut()?.grab();
        self.checked(got)
    }

    fn grab_region(&mut self, r: Rect) -> Option<Image<u8>> {
        if r.x < 0 || r.y < 0 || r.w <= 0 || r.h <= 0 {
            return None;
        }
        let got =
            self.src
                .as_mut()?
                .grab_region(r.x as usize, r.y as usize, r.w as usize, r.h as usize);
        self.checked(got)
    }

    fn size(&self) -> Option<(usize, usize)> {
        self.src.as_ref()?.size()
    }

    fn origin(&self) -> (i32, i32) {
        self.src.as_ref().map_or((0, 0), |s| s.origin())
    }

    fn exhausted(&self) -> bool {
        self.src.as_ref().is_none_or(|s| s.exhausted())
    }

    fn release(&mut self) {
        self.src = None;
    }
}

/// The bindings, polled through `state`; Ctrl+C (`stop`) is a Stop.
pub struct BoundKeys {
    binds: Vec<(String, Binding)>,
    modes: Vec<Mode>,
    state: Box<dyn InputState>,
    stop: Arc<AtomicBool>,
    /// The mining overlay's pause key.
    toggle: Option<Binding>,
}

impl BoundKeys {
    pub fn new(
        binds: Vec<(String, Binding)>,
        state: Box<dyn InputState>,
        stop: Arc<AtomicBool>,
    ) -> Self {
        let modes = binds
            .iter()
            .map(|(m, _)| Mode::parse(m).unwrap_or(Mode::Full))
            .collect();
        BoundKeys {
            binds,
            modes,
            state,
            stop,
            toggle: None,
        }
    }

    /// With the mining overlay's key; presses before now are forgotten.
    pub fn with_toggle(mut self, mut b: Binding) -> Self {
        b.drain(&*self.state);
        self.toggle = Some(b);
        self
    }
}

impl Keys for BoundKeys {
    fn modes(&self) -> Vec<Mode> {
        self.modes.clone()
    }

    fn pressed(&mut self, i: usize) -> Result<bool, Stop> {
        if self.stop.load(Ordering::Relaxed) {
            return Err(Stop);
        }
        Ok(self.binds[i].1.pressed(&*self.state))
    }

    fn drain(&mut self) {
        for (_, b) in &mut self.binds {
            b.drain(&*self.state);
        }
    }

    fn partial(&mut self, i: usize) -> (usize, usize, Vec<String>) {
        self.binds[i].1.partial(&*self.state)
    }

    fn waiting_label(&self) -> String {
        crate::config::waiting_label(&self.binds)
    }

    fn toggle_pressed(&mut self) -> bool {
        match self.toggle.as_mut() {
            Some(b) => b.pressed(&*self.state),
            None => false,
        }
    }
}

/// What the alert player is told about one contact of a burst.
pub fn spoken_facts(s: &Spoken) -> Facts {
    Facts {
        handle: s.handle.clone(),
        range_km: s.range_km,
        flags: s.flags.clone(),
        said: s
            .said
            .as_ref()
            .and_then(Value::as_array)
            .map(|v| v.iter().map(Reason::from_value).collect())
            .unwrap_or_default(),
        reason: s.reason.clone(),
        ..Default::default()
    }
}

/// The alert player (scw_audio::Alerts) as the watcher's alerts.
pub struct AudioSink {
    alerts: Mutex<Option<scw_audio::Alerts>>,
}

impl AudioSink {
    pub fn new(alerts: scw_audio::Alerts) -> Self {
        AudioSink {
            alerts: Mutex::new(Some(alerts)),
        }
    }

    /// Swaps in a player built from changed settings; the old one finishes
    /// what it has queued on its own thread.
    pub fn replace(&self, new: scw_audio::Alerts) {
        let old = self
            .alerts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(new);
        if let Some(o) = old {
            std::thread::spawn(move || o.close());
        }
    }

    fn with(&self, f: impl FnOnce(&scw_audio::Alerts)) {
        if let Some(a) = self
            .alerts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            f(a);
        }
    }
}

impl AlertSink for AudioSink {
    fn event(&self, kind: &str, handle: Option<&str>, flags: &[String], range_km: Option<f64>) {
        let facts = Facts {
            handle: handle.unwrap_or("").to_string(),
            range_km,
            flags: flags.to_vec(),
            ..Default::default()
        };
        self.with(|a| a.event(kind, facts, false));
    }

    fn burst_result(&self, contacts: &[Spoken]) {
        let facts: Vec<Facts> = contacts.iter().map(spoken_facts).collect();
        self.with(|a| a.burst_result(&facts));
    }

    fn say(&self, text: &str) {
        self.with(|a| a.say(text));
    }

    /// Plays what is queued, then stops the player.
    fn close(&self) {
        let a = self.alerts.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(a) = a {
            a.close();
        }
    }

    fn health(&self) -> AudioHealth {
        let mut out = AudioHealth {
            max_alert_age: scw_audio::alerts::MAX_ALERT_AGE.as_secs_f64(),
            ..AudioHealth::default()
        };
        self.with(|a| {
            let h = a.health();
            out.failures = h.failures;
            out.repairs = h.repairs;
            out.expired = h.expired;
            out.last_error = h.last_error;
            out.available = json!({"tones": h.tones, "speech": h.speech});
        });
        out
    }
}

/// The lookup worker as the session's queue.
pub struct WorkerQueue(pub EnricherThread);

impl LookupQueue for WorkerQueue {
    fn submit(&self, handle: &str, pending: Option<Pending>) {
        self.0.submit(handle, pending);
    }

    fn waiting(&self) -> Option<usize> {
        Some(self.0.waiting())
    }

    fn stop(&mut self) {
        self.0.stop();
    }
}

/// No keys or joysticks: where there is no input to read (replays, and
/// systems this version cannot poll yet).
pub struct NoInput;

impl InputState for NoInput {
    fn key_down(&self, _: u32) -> bool {
        false
    }
    fn devices(&self) -> Vec<scw_input::Device> {
        Vec::new()
    }
    fn buttons_down(&self, _: u32) -> u32 {
        0
    }
}

/// The input of this system.
pub fn input_state() -> Box<dyn InputState> {
    #[cfg(windows)]
    {
        Box::new(scw_input::windows::WindowsInput)
    }
    #[cfg(target_os = "linux")]
    {
        Box::new(scw_input::linux::LinuxInput::new())
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Box::new(NoInput)
    }
}
