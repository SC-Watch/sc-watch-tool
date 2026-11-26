//! One run of the watcher - watch.Session: wait for a trigger (a key, or a
//! new contact on screen), capture a burst, read it frame by frame and
//! announce as it goes, then settle the burst's vote into the sightings log,
//! the database, the audits and the lookup queue, and finish with a summary.
//!
//! Everything it works with is handed to it ([`Parts`]): where frames come
//! from, the reader, the keys, the console, the alert player, the session
//! log, the database, the lookup queue, the clock, the restart flag and the
//! housekeeping sweep. Nothing is global, so a test can run a whole session
//! on a script and a fake clock.
//!
//! Ctrl+C is [`Stop`]: the keys report it (and the auto loop checks the
//! stop flag); it unwinds to [`Session::run`], which shuts down and prints
//! the summary as an interrupt does in the Python.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::NaiveDateTime;
use serde_json::{Map, Value, json};

use scw_rsi::Clock;
use scw_store::json::{py_repr, py_repr_str};
use scw_store::reputation::Database;
use scw_vision::names::vote::{Resolved, resolve};
use scw_vision::pipeline::{Burst, Contact};
use scw_vision::{Image, Rect};

use crate::audit::{crops_from, save_audit_images, write_png};
use crate::enricher::Pending;
use crate::sightings::SightingsLog;
use crate::sinks::{AlertSink, AudioHealth, Console, EventLog, Spoken};
use crate::tracker::{Announcer, Tracker};

/// What a trigger asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    Full,
    Focus,
    Dual,
    Chat,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Full => "full",
            Mode::Focus => "focus",
            Mode::Dual => "dual",
            Mode::Chat => "chat",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Mode::Full, Mode::Focus, Mode::Dual, Mode::Chat]
            .into_iter()
            .find(|m| m.as_str() == s)
    }
}

/// Ctrl+C.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stop;

/// Where frames come from: the screen, or a replay.
pub trait Frames {
    /// The next frame; None when there is nothing new.
    fn grab(&mut self) -> Option<Image<u8>>;
    /// True once a replay has given its last frame.
    fn exhausted(&self) -> bool {
        false
    }
    /// Hands the capture device back.
    fn release(&mut self) {}
    /// Only a part (x, y, width, height) of the next frame - the mining
    /// overlay's band; None when there is nothing new or it is not inside.
    fn grab_region(&mut self, r: Rect) -> Option<Image<u8>> {
        let f = self.grab()?;
        scw_capture::crop(&f, r.x as usize, r.y as usize, r.w as usize, r.h as usize)
    }
    /// The frames' size, when known without grabbing one.
    fn size(&self) -> Option<(usize, usize)> {
        None
    }
    /// Where the captured screen sits on the desktop.
    fn origin(&self) -> (i32, i32) {
        (0, 0)
    }
}

/// What the reader made of the last frame it read, for --debug.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReadStats {
    /// Distance lines the scan kept.
    pub lines: usize,
    /// Plates read as a name.
    pub named: usize,
    /// Plates it would not read, by its reason, in the order first met.
    pub refused: Vec<(String, usize)>,
}

/// The reader.
pub trait Detector {
    /// Every plate in the frame, read; `mode` is what the trigger asked for.
    fn detect(&mut self, frame: &Image<u8>, mode: Mode) -> Vec<Contact>;
    /// The always-on scan: where the distance lines are, without reading.
    fn scan(&mut self, frame: &Image<u8>) -> Vec<Rect>;
    /// What the last `detect` found, stage by stage, when the reader can
    /// say.
    fn last_read(&self) -> Option<ReadStats> {
        None
    }
}

/// The trigger bindings, in the order they are polled.
pub trait Keys {
    fn modes(&self) -> Vec<Mode>;
    /// Was binding `i` pressed since it was last asked? Err on Ctrl+C.
    fn pressed(&mut self, i: usize) -> Result<bool, Stop>;
    /// Forgets presses made before the session started.
    fn drain(&mut self);
    /// (held, of how many, the names of the keys not held) for binding `i`.
    fn partial(&mut self, i: usize) -> (usize, usize, Vec<String>);
    /// "waiting for ..." in the idle line.
    fn waiting_label(&self) -> String;
    /// Was the mining overlay's key pressed since it was last asked?
    fn toggle_pressed(&mut self) -> bool {
        false
    }
}

/// The lookup queue (watch.RsiEnricher, as the session uses it).
pub trait LookupQueue {
    fn submit(&self, handle: &str, pending: Option<Pending>);
    /// Lookups still queued; None when lookups are off.
    fn waiting(&self) -> Option<usize>;
    fn stop(&mut self);
}

/// Lookups off.
pub struct NoLookups;

impl LookupQueue for NoLookups {
    fn submit(&self, _: &str, _: Option<Pending>) {}
    fn waiting(&self) -> Option<usize> {
        None
    }
    fn stop(&mut self) {}
}

/// A restart asked for from the UI.
pub trait Restart {
    /// True once per request: the request is used up by asking.
    fn requested(&self) -> bool;
    /// Starts the watcher over. Some(why) when that failed; None when the
    /// session only has to end and the program starts the next one (in
    /// one process: no new process, nothing to fail).
    fn relaunch(&self) -> Option<String>;
}

/// A restart inside one process: the page sets the word, the session
/// ends at its next idle moment, and the program starts a new one.
#[derive(Clone, Default)]
pub struct InProcess(pub Arc<AtomicBool>);

impl Restart for InProcess {
    fn requested(&self) -> bool {
        self.0.swap(false, Ordering::Relaxed)
    }

    fn relaunch(&self) -> Option<String> {
        None
    }
}

/// The UI's request is a file in the data folder (watch._restart_requested):
/// deleted before saying yes, so a failed restart cannot loop.
pub struct FlagFile {
    pub path: PathBuf,
    pub relaunch: Box<dyn Fn() -> String>,
}

impl Restart for FlagFile {
    fn requested(&self) -> bool {
        if !self.path.exists() {
            return false;
        }
        std::fs::remove_file(&self.path).is_ok() || !self.path.exists()
    }

    fn relaunch(&self) -> Option<String> {
        Some((self.relaunch)())
    }
}

/// The run options a session uses (watch.py's arguments and settings).
#[derive(Clone, Debug, PartialEq)]
pub struct SessionOptions {
    /// Triggered by keys (else by new contacts on screen).
    pub key_mode: bool,
    /// Frames come from saved files: no waiting, and a virtual clock.
    pub replay: bool,
    pub burst: usize,
    pub burst_spacing: f64,
    pub ping_delay: f64,
    pub queue_max: usize,
    pub min_votes: usize,
    pub rsi_min_votes: usize,
    pub verbose: bool,
    pub debug: bool,
    pub audit: bool,
    pub dual_on_read: bool,
    pub focus_always: bool,
    pub hz: f64,
    pub auto_read_delay: f64,
    /// Stop after this long; 0 = until Ctrl+C.
    pub seconds: f64,
    pub radius: f64,
    pub expire: f64,
    pub cooldown: f64,
    pub audit_keep_frame: bool,
    pub audit_frame_quality: u8,
    pub housekeeping_enabled: bool,
    pub housekeeping_every_min: f64,
}

/// One housekeeping sweep, as a line; given the session's database and the
/// time.
pub type Housekeeper = Box<dyn FnMut(Option<&Database>, f64) -> String>;

/// What a session is given.
pub struct Parts {
    pub frames: Box<dyn Frames>,
    pub detector: Box<dyn Detector>,
    pub keys: Box<dyn Keys>,
    pub console: Arc<dyn Console>,
    pub alerts: Option<Arc<dyn AlertSink>>,
    pub log: Arc<dyn EventLog>,
    pub db: Option<Database>,
    pub lookups: Box<dyn LookupQueue>,
    pub sightings: SightingsLog,
    pub clock: Arc<dyn Clock>,
    /// Local time, for stamps and the audit pictures' names.
    pub local_now: Box<dyn Fn() -> NaiveDateTime>,
    pub data_root: PathBuf,
    pub restart: Box<dyn Restart>,
    /// One housekeeping sweep, as a line (housekeeping.run_now); given the
    /// session's database and the time.
    pub housekeeper: Housekeeper,
    /// Set by Ctrl+C.
    pub stop: Arc<AtomicBool>,
}

/// One burst's working state, from the first frame to the final tally.
struct BurstState {
    now: f64,
    tally: Burst,
    /// (name, frame index, name box, range box) behind each reading.
    evidence: Vec<(String, usize, Rect, Rect)>,
    /// Contacts already mentioned this burst.
    mentioned: HashSet<String>,
}

const RULE: &str = "==============================================================";

pub struct Session {
    pub opts: SessionOptions,
    pub p: Parts,
    pub tracker: Tracker,
    pub announcer: Announcer,
    pub pending: Vec<Mode>,
    pub frames: usize,
    pub triggers: usize,
    pub announced: usize,
    pub started: f64,
    hk_last: f64,
    debug_warned: bool,
    /// Ended to be started over in the same process.
    pub restarted: bool,
    /// Settings changed while running (one process): taken at idle
    /// moments, the fields that need no restart.
    pub live: Option<std::sync::mpsc::Receiver<SessionOptions>>,
    /// The mining overlay, when its setting is on.
    pub mining: Option<crate::mining::MiningRun>,
}

fn pad(s: &str, width: usize) -> String {
    format!("{s:<width$}")
}

impl Session {
    pub fn new(opts: SessionOptions, p: Parts) -> Self {
        let started = p.clock.time();
        Session {
            tracker: Tracker::new(opts.radius, opts.expire),
            announcer: Announcer::new(opts.cooldown),
            opts,
            p,
            pending: Vec::new(),
            frames: 0,
            triggers: 0,
            announced: 0,
            started,
            hk_last: 0.0,
            debug_warned: false,
            restarted: false,
            live: None,
            mining: None,
        }
    }

    /// The mining overlay's turn: its key, then - when a read is due - the
    /// band it wants, read. `frame` is a whole frame the session already
    /// has (the always-on loop's); without one, only the band is captured.
    fn mining_tick(&mut self, frame: Option<&Image<u8>>) {
        let Some(mut m) = self.mining.take() else {
            return;
        };
        if self.p.keys.toggle_pressed() {
            let on = m.toggle();
            let word = if on { "mining on" } else { "mining off" };
            if let Some(a) = self.p.alerts.as_ref() {
                a.say(word);
            }
            self.con().say(&format!("  {word}"));
            self.p.log.event(
                "mining",
                &[("event", json!(if on { "resumed" } else { "paused" }))],
            );
        }
        let now = self.p.clock.monotonic();
        if m.due(now) {
            m.schedule(now);
            if let Some(msg) = m.reload_table(now) {
                self.con().say(&format!("  {msg}"));
                self.p
                    .log
                    .event("mining", &[("event", json!("table")), ("text", json!(msg))]);
            }
            let got = match frame {
                Some(f) => {
                    let (w, h) = (f.width as i32, f.height as i32);
                    let r = m.wanted(w, h);
                    scw_capture::crop(f, r.x as usize, r.y as usize, r.w as usize, r.h as usize)
                        .map(|c| (c, r, w, h))
                }
                None => {
                    let size = m
                        .size
                        .or_else(|| self.p.frames.size().map(|(w, h)| (w as i32, h as i32)));
                    match size {
                        Some((w, h)) => {
                            m.size = Some((w, h));
                            let r = m.wanted(w, h);
                            self.p.frames.grab_region(r).map(|c| (c, r, w, h))
                        }
                        // the size from one whole frame
                        None => self.grab().and_then(|f| {
                            let (w, h) = (f.width as i32, f.height as i32);
                            m.size = Some((w, h));
                            let r = m.wanted(w, h);
                            scw_capture::crop(
                                &f,
                                r.x as usize,
                                r.y as usize,
                                r.w as usize,
                                r.h as usize,
                            )
                            .map(|c| (c, r, w, h))
                        }),
                    }
                }
            };
            if let Some((crop, region, w, h)) = got {
                let (event, label) = m.feed(&crop, region, w, h, now * 1000.0);
                self.mining_report(&event, label);
            }
        }
        self.mining = Some(m);
    }

    /// What a mining read did, for the log: locks, endings, and each new
    /// label text.
    fn mining_report(
        &mut self,
        event: &scw_vision::mining::tracker::MiningEvent,
        label: Option<Option<String>>,
    ) {
        use scw_vision::mining::tracker::MiningEvent;
        match event {
            MiningEvent::Locked(read, _) => self.p.log.event(
                "mining",
                &[
                    ("event", json!("locked")),
                    ("signal", json!(read.text)),
                    ("value", json!(read.value)),
                ],
            ),
            MiningEvent::Ended { .. } => self.p.log.event("mining", &[("event", json!("ended"))]),
            _ => {}
        }
        if let Some(text) = label {
            self.p.log.event(
                "mining",
                &[("event", json!("label")), ("label", json!(text))],
            );
        }
    }

    fn con(&self) -> &dyn Console {
        &*self.p.console
    }

    fn now_text(&self) -> String {
        (self.p.local_now)().format("%H:%M:%S").to_string()
    }

    fn check_stop(&self) -> Result<(), Stop> {
        if self.p.stop.load(Ordering::Relaxed) {
            Err(Stop)
        } else {
            Ok(())
        }
    }

    fn grab(&mut self) -> Option<Image<u8>> {
        self.p.frames.grab()
    }

    /// Takes settings changed while running: the ones a session can change
    /// in place (the rest restart it).
    pub fn apply_live(&mut self) {
        let Some(rx) = self.live.as_ref() else {
            return;
        };
        let mut newest = None;
        while let Ok(o) = rx.try_recv() {
            newest = Some(o);
        }
        if let Some(o) = newest {
            self.opts.audit = o.audit;
            self.opts.audit_keep_frame = o.audit_keep_frame;
            self.opts.audit_frame_quality = o.audit_frame_quality;
            self.opts.debug = o.debug;
            self.opts.housekeeping_enabled = o.housekeeping_enabled;
        }
    }

    // -- housekeeping and the database-facing rules -------------------------

    /// Retention, rate-limited: calling it every idle tick costs a clock read.
    pub fn housekeep(&mut self, force: bool) {
        let every = self.opts.housekeeping_every_min * 60.0;
        if !self.opts.housekeeping_enabled {
            return;
        }
        if !force && (every <= 0.0 || self.p.clock.time() - self.hk_last < every) {
            return;
        }
        self.hk_last = self.p.clock.time();
        let now = self.p.clock.time();
        let line = (self.p.housekeeper)(self.p.db.as_ref(), now);
        if self.opts.verbose || line.contains("freed") {
            self.con().print(&format!("  {line}"));
        }
    }

    /// The ignore rule a label matches, if any.
    pub fn ignored(&self, name: &str) -> Option<String> {
        self.p.db.as_ref()?.is_ignored(name).ok().flatten()
    }

    /// Is this handle on one of the user's own lists?
    pub fn listed(&self, name: &str) -> bool {
        self.p
            .db
            .as_ref()
            .is_some_and(|db| db.is_listed(name).unwrap_or(false))
    }

    /// Saves the picture behind a read the tool is not sure of: fewer than
    /// two votes, or a burst that disagreed with itself - unless earlier
    /// pings have already seen the handle twice.
    fn maybe_audit(&self, r: &Resolved, b: &BurstState, captured: &[Image<u8>], seen: i64) {
        if r.votes >= 2 && r.votes >= r.total {
            return;
        }
        if seen >= 2 {
            return;
        }
        let Some((_, fi, nb, rb)) = b.evidence.iter().find(|e| e.0 == r.name) else {
            return;
        };
        let Some(db) = self.p.db.as_ref() else {
            return;
        };
        self.p.log.event(
            "audit",
            &[
                ("handle", json!(r.name)),
                ("reason", json!("weak_read")),
                ("votes", json!(r.votes)),
                ("total", json!(r.total)),
                ("seen", json!(seen)),
            ],
        );
        let frame = &captured[*fi];
        let paths = save_audit_images(
            &self.p.data_root,
            &r.name,
            &crops_from(frame, Some(*nb), Some(*rb)),
            self.opts.audit_keep_frame.then_some(frame),
            self.opts.audit_frame_quality,
            (self.p.local_now)(),
        );
        let variants: Vec<Value> = r.variants.iter().map(|(n, c)| json!([n, c])).collect();
        let _ = db.add_audit(
            &r.name,
            Some(r.range_km),
            r.votes as i64,
            r.total as i64,
            &Value::Array(variants),
            &paths.frame_path,
            &paths.crop_name,
            &paths.crop_range,
            "weak_read",
        );
    }

    // -- chat --------------------------------------------------------------

    /// Reading chat needs OCR, which this watcher does not have yet.
    fn read_chat(&mut self) {
        self.con()
            .say("  chat: reading chat needs OCR, which this version does not have yet");
        self.p.log.event(
            "chat",
            &[("error", json!("not available: reading chat needs OCR"))],
        );
    }

    // -- bursts ------------------------------------------------------------

    /// Reads a burst progressively: announces after each frame, then
    /// settles the burst's vote. `poll` watches the keys between captures.
    pub fn read_burst(
        &mut self,
        first: Option<Image<u8>>,
        now: f64,
        label: &str,
        poll: bool,
        mode: Mode,
    ) -> Result<(), Stop> {
        // Dual is the more informative of the two promotions, so it wins.
        let mode = if self.opts.dual_on_read && mode == Mode::Full {
            Mode::Dual
        } else if self.opts.focus_always && mode == Mode::Full {
            Mode::Focus
        } else {
            mode
        };
        let mut b = BurstState {
            now,
            tally: Burst::default(),
            evidence: Vec::new(),
            mentioned: HashSet::new(),
        };
        let captured = self.capture(first, poll)?;
        let n = captured.len();
        self.con()
            .status(&format!("  {label} - captured {n}, reading..."));

        for (idx, frame) in captured.iter().enumerate() {
            self.con()
                .status(&format!("  {label} - reading frame {}/{n}...", idx + 1));
            let frame_file = if self.opts.debug {
                self.debug_frame(frame, idx)
            } else {
                String::new()
            };
            let found = self.p.detector.detect(frame, mode);
            let n_named = self.tally(&mut b, idx, &found);
            let got: Vec<Value> = found.iter().map(|c| json!([c.name, c.range_km])).collect();
            if self.opts.debug {
                let parts: Vec<String> = found
                    .iter()
                    .map(|c| {
                        format!(
                            "({}, {})",
                            c.name.as_deref().map_or("None".into(), py_repr_str),
                            c.range_km.map_or("None".into(), |k| py_repr(&json!(k)))
                        )
                    })
                    .collect();
                self.con().say(&format!(
                    "  [debug] frame {idx}: detect returned [{}]",
                    parts.join(", ")
                ));
                if let Some(st) = self.p.detector.last_read() {
                    self.debug_read(&st, &frame_file, idx);
                }
            }
            let discarded = if b.tally.discarded.is_empty() {
                Value::Null
            } else {
                Value::Object(
                    b.tally
                        .discarded
                        .iter()
                        .map(|(k, n)| (k.as_str().to_string(), json!(n)))
                        .collect::<Map<_, _>>(),
                )
            };
            self.p.log.event(
                "read",
                &[
                    ("file", json!(frame_file)),
                    ("idx", json!(idx)),
                    ("mode", json!(mode.as_str())),
                    ("named", json!(n_named)),
                    ("unreadable", json!(b.tally.unreadable)),
                    ("discarded", discarded),
                    ("got", Value::Array(got)),
                ],
            );
            self.announce_so_far(&mut b);
        }

        // Settled once at the end, on the burst's final vote counts.
        let final_ = resolve(&b.tally.accumulated, self.opts.min_votes);
        let result: Vec<Value> = final_
            .iter()
            .map(|r| json!([r.name, r.votes, r.total, r.range_km]))
            .collect();
        self.p.log.event(
            "burst",
            &[
                ("label", json!(label)),
                ("mode", json!(mode.as_str())),
                ("frames", json!(n)),
                ("per_frame", json!(b.tally.per_frame)),
                ("result", Value::Array(result)),
            ],
        );
        self.settle(&final_, &b, &captured);
        let _ = self.p.sightings.flush();
        self.burst_audio(&final_);
        self.burst_report(&final_, &b);
        Ok(())
    }

    /// Captures the whole burst first, then it is read: frames land where
    /// they were configured to be, instead of each waiting for the last
    /// one's reading.
    fn capture(&mut self, first: Option<Image<u8>>, poll: bool) -> Result<Vec<Image<u8>>, Stop> {
        let mut captured: Vec<Image<u8>> = first.into_iter().collect();
        let mut tries = 0;
        while captured.len() < self.opts.burst && tries < self.opts.burst * 8 {
            tries += 1;
            if !self.opts.replay {
                // Presses while waiting queue: rapid pings show different
                // contacts.
                let end = self.p.clock.monotonic() + self.opts.burst_spacing;
                while self.p.clock.monotonic() < end {
                    if poll {
                        self.poll_keys()?;
                    }
                    self.check_stop()?;
                    self.p.clock.sleep(0.02);
                }
            }
            match self.grab() {
                Some(f) => captured.push(f),
                None if self.p.frames.exhausted() => break,
                None => {}
            }
        }
        Ok(captured)
    }

    /// --debug: what IRIS made of the frame - the distance lines its scan
    /// kept, the plates it read as names, and the ones it refused, by its
    /// reason. (The Python's --debug reports the OCR pipeline's stages
    /// instead: boxes, pairs, the km gate.)
    fn debug_read(&mut self, st: &ReadStats, frame_file: &str, idx: usize) {
        let refused: usize = st.refused.iter().map(|(_, n)| n).sum();
        let why = if st.refused.is_empty() {
            String::new()
        } else {
            let parts: Vec<String> = st
                .refused
                .iter()
                .map(|(r, n)| format!("{r} x{n}"))
                .collect();
            format!(" ({})", parts.join(", "))
        };
        self.con().say(&format!(
            "  [debug] frame {idx}: iris lines={} read={} refused={refused}{why}",
            st.lines, st.named
        ));
        let by_reason: Map<String, Value> = st
            .refused
            .iter()
            .map(|(r, n)| (r.clone(), json!(n)))
            .collect();
        self.p.log.event(
            "iris",
            &[
                ("file", json!(frame_file)),
                ("idx", json!(idx)),
                ("lines", json!(st.lines)),
                ("read", json!(st.named)),
                ("refused", Value::Object(by_reason)),
            ],
        );
    }

    /// --debug: the frame saved, and its size; what the reader made of it
    /// follows (debug_read).
    fn debug_frame(&mut self, frame: &Image<u8>, idx: usize) -> String {
        let dir = self.p.data_root.join("debug_bursts");
        let _ = std::fs::create_dir_all(&dir);
        let name = format!("burst_{}_{idx}.png", (self.p.local_now)().format("%H%M%S"));
        if !write_png(&dir.join(&name), frame) && !self.debug_warned {
            self.debug_warned = true;
            self.con().say(&format!(
                "  [debug] could not save {name} in {}",
                dir.display()
            ));
        }
        self.con().say(&format!("  [debug] frame {idx}: -> {name}"));
        self.p.log.event(
            "frame",
            &[
                ("file", json!(name)),
                ("idx", json!(idx)),
                ("size", json!(format!("{}x{}", frame.width, frame.height))),
            ],
        );
        name
    }

    /// Sorts one frame's contacts into the burst; returns the players read.
    fn tally(&self, b: &mut BurstState, idx: usize, found: &[Contact]) -> usize {
        let listed = |n: &str| self.listed(n);
        let evidence = &mut b.evidence;
        b.tally.tally_with(found, &listed, |c| {
            evidence.push((
                c.name.clone().unwrap_or_default(),
                idx,
                c.name_box,
                c.range_box,
            ))
        })
    }

    /// Announces what the burst knows so far; anything already said stays
    /// quiet.
    fn announce_so_far(&mut self, b: &mut BurstState) {
        for r in resolve(&b.tally.accumulated, self.opts.min_votes) {
            if self.ignored(&r.name).is_some() {
                continue;
            }
            let (ok, corrected_from) = self.announcer.should(&r.name, b.now);
            if ok {
                self.announced += 1;
                let note = corrected_from
                    .map(|c| format!("  (was {c})"))
                    .unwrap_or_default();
                self.con().say(&format!(
                    "[{}]  {} {:>5.1} km   {}/{} votes{note}",
                    self.now_text(),
                    pad(&r.name, 22),
                    r.range_km,
                    r.votes,
                    r.total
                ));
                // Anything already known fires at once.
                if let Some(db) = self.p.db.as_ref()
                    && let Ok(a) = db.assess(&r.name, None, false)
                {
                    let now = db.now();
                    if a.alert(now, &db.flags) {
                        self.con().say(&format!(
                            "          ***  KNOWN: {} (score {:+.1})",
                            r.name,
                            a.score(now, &db.flags)
                        ));
                        for detail in a.lines(now, &db.flags, db.utc_dates) {
                            self.con().say(&format!("          ***    {detail}"));
                        }
                    }
                }
            } else if self.opts.verbose && !b.mentioned.contains(&r.name) {
                self.con().say(&format!(
                    "[{}]  (repeat) {} {:.1} km",
                    self.now_text(),
                    r.name,
                    r.range_km
                ));
            }
            b.mentioned.insert(r.name.clone());
        }
    }

    /// Records the burst's final readings: log, database, audit, lookup.
    fn settle(&mut self, final_: &[Resolved], b: &BurstState, captured: &[Image<u8>]) {
        for r in final_ {
            if let Some(rule) = self.ignored(&r.name) {
                if self.opts.verbose {
                    self.con()
                        .say(&format!("  ignored {} (matches '{rule}')", r.name));
                }
                continue;
            }
            let at = (self.p.local_now)();
            let _ = self.p.sightings.write(r, at);
            // A sighting accuses nobody; a lookup attaches a real person to
            // the string, so it needs corroboration - across frames or
            // across pings.
            let mut seen = 0;
            if let Some(db) = self.p.db.as_ref()
                && let Ok(s) =
                    db.record_sighting(&r.name, Some(r.range_km), r.votes as i64, r.total as i64)
            {
                seen = s;
                if self.opts.audit {
                    self.maybe_audit(r, b, captured, seen);
                }
            }
            if (r.votes as i64).max(seen) >= self.opts.rsi_min_votes as i64 {
                // The crops travel with the handle: a "no such account"
                // answer raises an audit long after this frame is gone.
                let pending = b
                    .evidence
                    .iter()
                    .find(|e| e.0 == r.name)
                    .map(|(_, fi, nb, rb)| Pending {
                        crops: crops_from(&captured[*fi], Some(*nb), Some(*rb)),
                        range_km: Some(r.range_km),
                        votes: r.votes,
                        total: r.total,
                        variants: r.variants.clone(),
                    });
                self.p.lookups.submit(&r.name, pending);
            }
        }
    }

    /// The burst's alert, with whatever the database already knows; flags
    /// from a lookup raise their own alert later.
    fn burst_audio(&self, final_: &[Resolved]) {
        let Some(alerts) = self.p.alerts.as_ref() else {
            return;
        };
        let payload: Vec<Spoken> = final_
            .iter()
            .map(|r| {
                let (mut reason, mut said) = (String::new(), None);
                if let Some(db) = self.p.db.as_ref()
                    && let Ok(a) = db.assess(&r.name, None, false)
                {
                    let now = db.now();
                    if a.alert(now, &db.flags) {
                        reason = a
                            .lines(now, &db.flags, db.utc_dates)
                            .into_iter()
                            .next()
                            .unwrap_or_default();
                        said = Some(Value::Array(a.spoken_all(&db.flags)));
                    }
                }
                Spoken {
                    handle: r.name.clone(),
                    range_km: Some(r.range_km),
                    flags: Vec::new(),
                    reason,
                    said,
                }
            })
            .collect();
        alerts.burst_result(&payload);
    }

    fn burst_report(&self, final_: &[Resolved], b: &BurstState) {
        // The closing tally: announcements are made on partial counts.
        if !final_.is_empty() && !b.mentioned.is_empty() {
            let tally: Vec<String> = final_
                .iter()
                .filter(|r| b.mentioned.contains(&r.name))
                .map(|r| format!("{} {}/{}", r.name, r.votes, r.total))
                .collect();
            if !tally.is_empty() {
                self.con()
                    .say(&format!("          final: {}", tally.join(", ")));
            }
        }
        if self.opts.verbose {
            let mut d: Vec<(&str, usize)> = b
                .tally
                .discarded
                .iter()
                .map(|(k, n)| (k.as_str(), *n))
                .collect();
            d.sort();
            let junk: Vec<String> = d.iter().map(|(k, n)| format!("{n} {k}")).collect();
            let unread = if b.tally.unreadable > 0 {
                format!(", {} unreadable", b.tally.unreadable)
            } else {
                String::new()
            };
            let disc = if junk.is_empty() {
                String::new()
            } else {
                format!(", discarded {}", junk.join(", "))
            };
            self.con().say(&format!(
                "  [burst] named reads per frame: {:?}{unread}{disc} -> {} contact(s)",
                b.tally.per_frame,
                final_.len()
            ));
        }
    }

    // -- the loops ---------------------------------------------------------

    /// Runs until Ctrl+C, the deadline, the end of a replay or a restart;
    /// the exit code.
    pub fn run(&mut self) -> i32 {
        self.started = self.p.clock.time();
        let deadline = (self.opts.seconds != 0.0).then_some(self.started + self.opts.seconds);
        let rc = if self.opts.key_mode {
            self.run_keys(deadline)
        } else {
            self.run_auto(deadline)
        }
        .unwrap_or(0);
        self.shutdown();
        self.summary();
        rc
    }

    /// Counts presses instead of discarding them, up to the queue's limit.
    pub fn poll_keys(&mut self) -> Result<(), Stop> {
        for (i, mode) in self.p.keys.modes().into_iter().enumerate() {
            if self.p.keys.pressed(i)? && self.pending.len() < self.opts.queue_max {
                self.pending.push(mode);
            }
        }
        Ok(())
    }

    fn queued(&self) -> String {
        if self.pending.is_empty() {
            String::new()
        } else {
            format!("  [{} queued]", self.pending.len())
        }
    }

    /// Idle until a binding is pressed: frames are grabbed only when asked
    /// for, so no backlog can build.
    fn run_keys(&mut self, deadline: Option<f64>) -> Result<i32, Stop> {
        self.p.keys.drain();
        self.pending.clear();
        while deadline.is_none_or(|d| self.p.clock.time() < d) {
            self.poll_keys()?;
            if self.pending.is_empty() {
                self.apply_live();
                let idle = self.idle_status();
                self.con().status(&idle);
                // Idle is the only safe moment to delete files.
                self.housekeep(false);
                if self.p.restart.requested() {
                    return Ok(self.restart());
                }
                self.mining_tick(None);
                self.p.clock.sleep(0.03);
                continue;
            }
            let mode = self.pending.remove(0);
            self.triggers += 1;
            if let Some(a) = self.p.alerts.as_ref() {
                a.event("trigger", None, &[], None);
            }
            if self.opts.ping_delay > 0.0 {
                let end = self.p.clock.time() + self.opts.ping_delay;
                while self.p.clock.time() < end {
                    self.poll_keys()?;
                    let text = format!(
                        "  {} ping - waiting {:.1}s for labels...{}",
                        mode.as_str(),
                        end - self.p.clock.time(),
                        self.queued()
                    );
                    self.con().status(&text);
                    self.p.clock.sleep(0.03);
                }
            }
            let queued = self.queued();
            if mode == Mode::Chat {
                self.con().status(&format!("  reading chat...{queued}"));
                let _ = self.grab();
                self.read_chat();
            } else {
                let label = match mode {
                    Mode::Focus => "reading centre",
                    Mode::Dual => "reading wide + centre",
                    _ => "reading",
                };
                let first = self.grab();
                let now = self.p.clock.time();
                self.read_burst(first, now, &format!("{label}{queued}"), true, mode)?;
            }
        }
        Ok(0)
    }

    /// The idle line; names the keys not held when a binding is partly
    /// down, since a combination that never completes otherwise looks like
    /// a tool that is broken.
    fn idle_status(&mut self) -> String {
        let mut part = String::new();
        for (i, mode) in self.p.keys.modes().into_iter().enumerate() {
            let (held, total, missing) = self.p.keys.partial(i);
            if held > 0 && held < total {
                part = format!(
                    "  |  {}: {held}/{total} held, waiting on {}",
                    mode.as_str(),
                    missing.join(", ")
                );
                break;
            }
        }
        let mining = self
            .mining
            .as_ref()
            .map_or_else(String::new, |m| format!(" | {}", m.status()));
        format!(
            "  idle | {} pings | {} announced{part} | waiting for {}{mining}",
            self.triggers,
            self.announced,
            self.p.keys.waiting_label()
        )
    }

    /// Continuous: the always-on scan notices new contacts, which are read.
    fn run_auto(&mut self, deadline: Option<f64>) -> Result<i32, Stop> {
        let interval = 1.0 / self.opts.hz;
        while deadline.is_none_or(|d| self.p.clock.time() < d) {
            self.check_stop()?;
            self.apply_live();
            let cycle = self.p.clock.monotonic();
            let Some(frame) = self.grab() else {
                if self.p.frames.exhausted() {
                    break;
                }
                self.p.clock.sleep(0.005);
                continue;
            };
            self.frames += 1;
            let positions: Vec<(f64, f64)> = self
                .p
                .detector
                .scan(&frame)
                .iter()
                .map(|r| (r.x as f64 + r.w as f64 / 2.0, r.y as f64))
                .collect();
            // A replay runs faster than real time; a virtual clock keeps
            // tracks and cooldowns as they would be live.
            let now = if self.opts.replay {
                self.started + self.frames as f64 / self.opts.hz
            } else {
                self.p.clock.time()
            };
            let n_new = self.tracker.update(&positions, now);
            self.mining_tick(Some(&frame));
            let mining = self
                .mining
                .as_ref()
                .map_or_else(String::new, |m| format!(" | {}", m.status()));
            let text = format!(
                "  watching | {} frames | {} contact(s) on screen | {} tracked | {} announced{mining}",
                self.frames,
                positions.len(),
                self.tracker.tracks.len(),
                self.announced
            );
            self.con().status(&text);
            self.housekeep(false);
            if !self.opts.replay && self.p.restart.requested() {
                return Ok(self.restart());
            }
            if n_new > 0 {
                self.triggers += 1;
                let label = format!("reading {n_new} new contact(s)");
                let delay = self.opts.auto_read_delay;
                if self.opts.replay || delay <= 0.0 {
                    self.read_burst(Some(frame), now, &label, false, Mode::Full)?;
                } else {
                    // The HUD types a new name in letter by letter: wait,
                    // then read fresh frames instead of this one.
                    self.p.clock.sleep(delay);
                    let t = self.p.clock.time();
                    self.read_burst(None, t, &label, false, Mode::Full)?;
                }
            }
            if !self.opts.replay {
                let slack = interval - (self.p.clock.monotonic() - cycle);
                if slack > 0.0 {
                    self.p.clock.sleep(slack);
                }
            }
        }
        Ok(0)
    }

    /// Hands everything back and starts over; returns only if that failed,
    /// and then the session ends.
    fn restart(&mut self) -> i32 {
        self.con().status("");
        self.con()
            .print("\n  restart requested from the UI - reloading settings and starting over");
        self.p.frames.release();
        if let Some(a) = self.p.alerts.as_ref() {
            a.close();
        }
        self.p.sightings.close();
        let Some(why) = self.p.restart.relaunch() else {
            // In one process: this session ends, the next one starts.
            self.restarted = true;
            self.p.log.event("restart", &[]);
            return 0;
        };
        self.con().eprint(&format!("! restart failed: {why}"));
        self.con().eprint(
            "  the watcher has to stop - its camera and log were already handed back. \
             Reopen it to pick up the new settings.",
        );
        self.p.log.event("restart_failed", &[]);
        1
    }

    // -- shutdown ----------------------------------------------------------

    fn shutdown(&mut self) {
        // In-flight lookups get a moment to land before the summary.
        if let Some(waiting) = self.p.lookups.waiting().filter(|n| *n > 0) {
            self.con()
                .status(&format!("  finishing {waiting} RSI lookup(s)..."));
            let deadline = self.p.clock.time() + 15.0f64.min(waiting as f64 * 4.0);
            while self.p.lookups.waiting().unwrap_or(0) > 0 && self.p.clock.time() < deadline {
                self.p.clock.sleep(0.2);
            }
        }
        self.p.lookups.stop();
        self.con().done();
        self.p.frames.release();
        self.p.sightings.close();
    }

    fn summary(&self) {
        let mins = (self.p.clock.time() - self.started) / 60.0;
        let h = self
            .p
            .alerts
            .as_ref()
            .map(|a| a.health())
            .unwrap_or_default();
        if self.p.alerts.is_some() && (h.failures > 0 || h.repairs > 0) {
            let last = if h.last_error.is_empty() {
                String::new()
            } else {
                format!(" - last: {}", h.last_error)
            };
            self.con().print(&format!(
                "\n  audio: {} failure(s), {} engine repair(s){last}",
                h.failures, h.repairs
            ));
        }
        if h.expired > 0 {
            self.con().print(&format!(
                "\n  audio: {} alert(s) dropped - still queued {:.0}s after the read, too late to be news",
                h.expired, h.max_alert_age
            ));
        }
        let counts = self.announcer.summary();
        let contacts: Map<String, Value> =
            counts.iter().map(|(n, c)| (n.clone(), json!(c))).collect();
        let minutes: f64 = format!("{mins:.1}").parse().unwrap_or(0.0);
        let last_error = if h.last_error.is_empty() {
            "-".to_string()
        } else {
            h.last_error.clone()
        };
        let AudioHealth {
            failures,
            repairs,
            available,
            expired,
            ..
        } = h;
        let mut summary: Vec<(&str, Value)> = vec![
            ("minutes", json!(minutes)),
            ("frames", json!(self.frames)),
            ("triggers", json!(self.triggers)),
            ("announced", json!(self.announced)),
            ("audio_failures", json!(failures)),
            ("audio_repairs", json!(repairs)),
            ("audio_last_error", json!(last_error)),
            ("audio_available", available),
        ];
        if expired > 0 {
            summary.push(("audio_expired", json!(expired)));
        }
        summary.push(("contacts", Value::Object(contacts)));
        self.p.log.close(&summary);
        self.con().print(&format!("\n{RULE}"));
        self.con().print(&format!(
            "  {mins:.1} min | {} frames | {} triggers | {} announced",
            self.frames, self.triggers, self.announced
        ));
        if !counts.is_empty() {
            self.con().print("\n  contacts seen:");
            for (name, n) in &counts {
                self.con()
                    .print(&format!("     {} {n} burst(s)", pad(name, 24)));
            }
        }
        self.con().print(RULE);
    }
}
