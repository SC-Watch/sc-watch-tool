//! The mining overlay in the watcher (PORT.md, "The mining overlay"):
//! IRIS's mining tracker (scw_vision::mining::tracker) given a turn when the
//! session is idle, reading only the band of the screen it asks for, and
//! its label drawn in a click-through window (scw_overlay) - or, where
//! there is no window yet, said in the console.
//!
//! Off unless the setting `mining` is on; `mining_key` pauses and resumes
//! it. The table is IRIS's, written to the data folder as mining.txt the
//! first time so it can be edited, and read again when it changes.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use scw_vision::mining::MiningSettings;
use scw_vision::mining::table::{EMBEDDED_TEXT, MiningTable};
use scw_vision::mining::tracker::{MiningEvent, MiningLabel, MiningTracker};
use scw_vision::{Image, Rect};

use crate::sinks::Console;

/// How long the last rock's label stays after the game drops its number.
const LABEL_HOLD_MS: f64 = 10_000.0;

/// A table file larger than this is not read (IRIS 2.1's limit).
const TABLE_MAX_BYTES: u64 = 1024 * 1024;

/// Where the label is shown.
pub trait MiningView: Send {
    /// Shows the label, or nothing.
    fn show(&mut self, label: Option<&MiningLabel>);
    /// The captured screen's size, once known (which monitor to draw on).
    fn screen(&mut self, _w: i32, _h: i32) {}
    /// For the banner.
    fn describe(&self) -> String;
}

/// The overlay window.
pub struct WindowView {
    painter: scw_overlay::Painter,
    window: scw_overlay::OverlayWindow,
    scale: f64,
}

/// The overlay window over a monitor at `origin` on the desktop, its
/// letters `scale` times the signal's digit height.
pub fn window_view(origin: (i32, i32), scale: f64) -> Result<WindowView, String> {
    let painter = scw_overlay::Painter::system()?;
    let window = scw_overlay::OverlayWindow::open(origin.0, origin.1)?;
    Ok(WindowView {
        painter,
        window,
        scale,
    })
}

fn to_overlay(l: &MiningLabel) -> scw_overlay::Label {
    scw_overlay::Label {
        text: l.text.clone(),
        signal: [l.signal.x, l.signal.y, l.signal.w, l.signal.h],
        digit_height: l.digit_height,
        text_bgr: [l.text_colour.b, l.text_colour.g, l.text_colour.r],
        back_bgr: [l.back_colour.b, l.back_colour.g, l.back_colour.r],
    }
}

impl MiningView for WindowView {
    fn show(&mut self, label: Option<&MiningLabel>) {
        let drawn = label.and_then(|l| self.painter.draw(&to_overlay(l), self.scale));
        self.window.show(drawn);
    }

    fn screen(&mut self, w: i32, h: i32) {
        self.window.set_screen(w.max(0) as u32, h.max(0) as u32);
    }

    fn describe(&self) -> String {
        format!(
            "a window over the game ({}), labels at {:.0}% (font {})",
            self.window.kind(),
            self.scale * 100.0,
            self.painter.font_path.display()
        )
    }
}

/// No window: each new label said in the console.
pub struct ConsoleView {
    console: Arc<dyn Console>,
    last: Option<String>,
}

impl ConsoleView {
    pub fn new(console: Arc<dyn Console>) -> Self {
        Self {
            console,
            last: None,
        }
    }
}

impl MiningView for ConsoleView {
    fn show(&mut self, label: Option<&MiningLabel>) {
        let text = label.map(|l| l.text.clone());
        if text != self.last
            && let Some(t) = &text
        {
            self.console.say(&format!("  mining: {t}"));
        }
        self.last = text;
    }

    fn describe(&self) -> String {
        "this console (no overlay window on this system yet)".into()
    }
}

/// The overlay while a session runs.
pub struct MiningRun {
    pub tracker: MiningTracker,
    view: Box<dyn MiningView>,
    /// Not paused by its key.
    pub active: bool,
    next_at: f64,
    /// The captured screen's size, once known.
    pub size: Option<(i32, i32)>,
    table_path: PathBuf,
    /// The table file's time when last read; Some(None) when it was missing.
    table_stamp: Option<Option<SystemTime>>,
    table_checked: f64,
    shown: Option<MiningLabel>,
    /// The screen size the view was told.
    told_screen: Option<(i32, i32)>,
    /// When the tracker last had a label (ms).
    label_seen_ms: f64,
    /// The pause key, for the banner; None without one.
    pub key: Option<String>,
}

impl MiningRun {
    /// With IRIS's table, written to `table_path` first when there is no
    /// file there to edit.
    pub fn new(view: Box<dyn MiningView>, table_path: PathBuf, key: Option<String>) -> Self {
        if !table_path.exists() {
            if let Some(dir) = table_path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&table_path, EMBEDDED_TEXT);
        }
        Self {
            tracker: MiningTracker::new(MiningSettings::default(), MiningTable::embedded()),
            view,
            active: true,
            next_at: f64::NEG_INFINITY,
            size: None,
            table_path,
            table_stamp: None,
            table_checked: f64::NEG_INFINITY,
            shown: None,
            told_screen: None,
            label_seen_ms: f64::NEG_INFINITY,
            key,
        }
    }

    pub fn describe(&self) -> String {
        self.view.describe()
    }

    /// The idle line's part.
    pub fn status(&self) -> String {
        if !self.active {
            "mining paused".into()
        } else if let Some(l) = &self.shown {
            format!("mining: {}", l.text)
        } else if self.tracker.locked().is_some() {
            "mining: reading".into()
        } else {
            "mining: looking".into()
        }
    }

    /// Time for a read (monotonic seconds)?
    pub fn due(&self, now: f64) -> bool {
        self.active && now >= self.next_at
    }

    /// The next read at the tracker's rate.
    pub fn schedule(&mut self, now: f64) {
        self.next_at = now + 1.0 / self.tracker.rate_hz();
    }

    /// The key was pressed: paused or carrying on; true when now on. A
    /// pause forgets the lock and takes the label down.
    pub fn toggle(&mut self) -> bool {
        self.active = !self.active;
        if !self.active {
            self.tracker.reset();
            self.set_shown(None);
        }
        self.active
    }

    /// The table file read again when it changed (checked once a second,
    /// as IRIS's MiningTableSource); a line to say when it was.
    pub fn reload_table(&mut self, now: f64) -> Option<String> {
        if now - self.table_checked < 1.0 {
            return None;
        }
        self.table_checked = now;
        let meta = std::fs::metadata(&self.table_path).ok();
        let stamp = meta.as_ref().and_then(|m| m.modified().ok());
        if self.table_stamp == Some(stamp) {
            return None;
        }
        self.table_stamp = Some(stamp);
        let Some(meta) = meta else {
            self.tracker.set_table(MiningTable::embedded());
            return Some("mining: no mining.txt - using the table built in".into());
        };
        if meta.len() > TABLE_MAX_BYTES {
            return Some(format!(
                "mining: mining.txt is larger than {} MB and was not read; the table before it stays",
                TABLE_MAX_BYTES / (1024 * 1024)
            ));
        }
        match std::fs::read_to_string(&self.table_path) {
            Ok(text) => {
                let table = MiningTable::parse(&text);
                let n = table.resources.len();
                let msg = if table.errors.is_empty() {
                    format!("mining: using mining.txt ({n} resources)")
                } else {
                    format!(
                        "mining: using mining.txt ({n} resources); lines left out: {}",
                        table
                            .errors
                            .iter()
                            .take(5)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join("; ")
                    )
                };
                let outdated = MiningTable::is_outdated(Some(&text));
                self.tracker.set_table(table);
                Some(if outdated {
                    format!("{msg} - older than the table built in: delete it to get the new one")
                } else {
                    msg
                })
            }
            Err(e) => {
                self.table_stamp = None;
                Some(format!(
                    "mining: mining.txt could not be read ({e}); the table before it stays"
                ))
            }
        }
    }

    /// The part of a `width` x `height` screen to capture next.
    pub fn wanted(&self, width: i32, height: i32) -> Rect {
        self.tracker.wanted(width, height)
    }

    /// One crop read. The event, and the label's new text when the text
    /// shown changed.
    pub fn feed(
        &mut self,
        crop: &Image<u8>,
        region: Rect,
        width: i32,
        height: i32,
        now_ms: f64,
    ) -> (MiningEvent, Option<Option<String>>) {
        if self.told_screen != Some((width, height)) {
            self.view.screen(width, height);
            self.told_screen = Some((width, height));
        }
        let event = self
            .tracker
            .feed(crop, region, width, height, now_ms, now_ms);
        // The game drops and flashes its own number: the last rock's label
        // stays up for LABEL_HOLD_MS after it went, unless another replaces
        // it (not IRIS, whose label goes with the lock).
        let mut label = self.tracker.label();
        if label.is_some() {
            self.label_seen_ms = now_ms;
        } else if self.shown.is_some() && now_ms - self.label_seen_ms < LABEL_HOLD_MS {
            label = self.shown.clone();
        }
        let text_changed = label.as_ref().map(|l| &l.text) != self.shown.as_ref().map(|l| &l.text);
        let changed = label != self.shown;
        if changed {
            self.set_shown(label.clone());
        }
        (event, text_changed.then(|| label.map(|l| l.text)))
    }

    fn set_shown(&mut self, label: Option<MiningLabel>) {
        self.view.show(label.as_ref());
        self.shown = label;
    }
}

impl Drop for MiningRun {
    fn drop(&mut self) {
        self.view.show(None);
    }
}
