//! sc-watch in one process: the watcher and its page run side by side,
//! joined by words rather than files.
//!
//! - The page's restart button tells the watcher directly; the session ends
//!   at its next idle moment and the next one starts with the saved
//!   settings - no flag file, no new process.
//! - Saving a setting that the watcher reads at start restarts it the same
//!   way; the alert settings rebuild the alert player; the rest reach the
//!   running session in place.
//! - One lookup client serves both, so one rate limiter and one outage
//!   breaker, and a question asked twice at once goes out once.
//! - Every commit made here wakes the page's stream, so a burst shows on
//!   the page the moment it is settled.
//!
//! [`App`] holds what is shared and runs the sessions; how a session is
//! built is handed to [`App::supervise`], so a test can run it on fakes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value};

use scw_store::paths::Roots;
use scw_store::settings;
use scw_ui::Links;
use scw_watch::live::AudioSink;
use scw_watch::program;
use scw_watch::session::{Session, SessionOptions};
use scw_watch::sinks::{AlertSink, AudioHealth, Spoken};

/// The settings that change what the alert player says and how.
pub const AUDIO_KEYS: [&str; 6] = [
    "audio_mode",
    "volume",
    "rate",
    "speak_handles",
    "max_spoken_per_burst",
    "summarise_above",
];

/// What a settings change asks of the watcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reaction {
    /// Nothing the watcher reads changed.
    None,
    /// A setting the watcher reads only at start: a new session.
    Restart,
    /// In place; `audio` when the alert player is rebuilt.
    Live { audio: bool },
}

/// The keys whose values differ (numbers by value).
pub fn changed_keys(before: &Map<String, Value>, after: &Map<String, Value>) -> Vec<String> {
    let same = |a: Option<&Value>, b: Option<&Value>| match (a, b) {
        (Some(Value::Number(x)), Some(Value::Number(y))) => x.as_f64() == y.as_f64(),
        _ => a == b,
    };
    let mut keys: Vec<String> = before.keys().chain(after.keys()).cloned().collect();
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .filter(|k| !same(before.get(k), after.get(k)))
        .collect()
}

/// What changing these settings asks of the watcher, by the schema's own
/// restart marks.
pub fn reaction(changed: &[String]) -> Reaction {
    if changed.is_empty() {
        return Reaction::None;
    }
    let restart = changed
        .iter()
        .any(|k| settings::field(k).is_some_and(|f| f.restart == "watcher"));
    if restart {
        return Reaction::Restart;
    }
    Reaction::Live {
        audio: changed.iter().any(|k| AUDIO_KEYS.contains(&k.as_str())),
    }
}

/// The program's options: the watcher's arguments with three of its own,
/// taken out first. Exact names only.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AppFlags {
    pub no_page: bool,
    pub no_browser: bool,
    pub port: Option<i64>,
}

/// Splits the program's own flags from the watcher's arguments.
pub fn split_flags(argv: &[String]) -> Result<(AppFlags, Vec<String>), String> {
    let mut flags = AppFlags::default();
    let mut rest = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        match a.as_str() {
            "--no-page" => flags.no_page = true,
            "--no-browser" => flags.no_browser = true,
            "--port" => {
                i += 1;
                let v = argv
                    .get(i)
                    .ok_or("argument --port: expected one argument")?;
                flags.port = Some(parse_port(v)?);
            }
            _ if a.starts_with("--port=") => flags.port = Some(parse_port(&a["--port=".len()..])?),
            _ => rest.push(a.clone()),
        }
        i += 1;
    }
    Ok((flags, rest))
}

fn parse_port(v: &str) -> Result<i64, String> {
    scw_store::json::py_int_text(v)
        .ok_or_else(|| format!("argument --port: invalid int value: '{v}'"))
}

/// The alert player, shared by every session: a session's end does not
/// close it.
pub struct KeepOpen(pub Arc<AudioSink>);

impl AlertSink for KeepOpen {
    fn event(&self, kind: &str, handle: Option<&str>, flags: &[String], range_km: Option<f64>) {
        self.0.event(kind, handle, flags, range_km);
    }
    fn burst_result(&self, contacts: &[Spoken]) {
        self.0.burst_result(contacts);
    }
    fn say(&self, text: &str) {
        self.0.say(text);
    }
    fn close(&self) {}
    fn health(&self) -> AudioHealth {
        self.0.health()
    }
}

/// What the sessions share, and the running of them.
pub struct App {
    pub roots: Roots,
    /// The watcher's arguments (the program's own taken out).
    pub argv: Vec<String>,
    /// The page's word to restart.
    pub restart: Arc<AtomicBool>,
    /// Set by Ctrl+C.
    pub stop: Arc<AtomicBool>,
    /// The running session's settings channel.
    pub live: Mutex<Option<Sender<SessionOptions>>>,
    pub audio: Option<Arc<AudioSink>>,
    /// The page is up: a watcher that will not start waits for a fix.
    pub page_up: bool,
    /// How many sessions have started.
    pub sessions: Mutex<usize>,
}

impl App {
    pub fn new(
        roots: Roots,
        argv: Vec<String>,
        audio: Option<Arc<AudioSink>>,
        page_up: bool,
    ) -> Self {
        App {
            roots,
            argv,
            restart: Arc::new(AtomicBool::new(false)),
            stop: Arc::new(AtomicBool::new(false)),
            live: Mutex::new(None),
            audio,
            page_up,
            sessions: Mutex::new(0),
        }
    }

    /// The page's links to the watcher.
    pub fn links(self: &Arc<Self>) -> Links {
        let (a, b) = (self.clone(), self.clone());
        Links {
            restart_watcher: Box::new(move || a.restart.store(true, Ordering::Relaxed)),
            settings_saved: Box::new(move |before, after| b.settings_saved(before, after)),
        }
    }

    /// A settings save on the page: a new session, a new alert player, or
    /// the new values to the running session.
    pub fn settings_saved(&self, before: &Map<String, Value>, after: &Map<String, Value>) {
        match reaction(&changed_keys(before, after)) {
            Reaction::None => {}
            Reaction::Restart => self.restart.store(true, Ordering::Relaxed),
            Reaction::Live { audio } => {
                let Ok((args, _)) = program::parse(&self.argv, &self.roots, after) else {
                    return;
                };
                if audio && let Some(a) = &self.audio {
                    a.replace(program::make_player(
                        args.audio.as_deref(),
                        after,
                        &self.roots,
                    ));
                }
                let key_mode = args.trigger == "key" && args.replay.is_none();
                let opts = program::session_options(&args, after, key_mode);
                if let Some(tx) = self.live.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                    let _ = tx.send(opts);
                }
            }
        }
    }

    fn wait_for_restart(&self) -> bool {
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return false;
            }
            if self.restart.swap(false, Ordering::Relaxed) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Runs watcher sessions, one after another, until Ctrl+C - or the
    /// watcher's end when there is no page. `build` makes each from the
    /// settings as they are then. The exit code.
    pub fn supervise(&self, mut build: impl FnMut(&App) -> Result<Session, i32>) -> i32 {
        loop {
            let built = build(self);
            *self.sessions.lock().unwrap_or_else(|e| e.into_inner()) += 1;
            let mut session = match built {
                Ok(s) => s,
                Err(rc) => {
                    if !self.page_up {
                        return rc;
                    }
                    println!(
                        "  the watcher cannot start - the page is still up: fix it in Settings, \
                         and it starts again when you save"
                    );
                    if !self.wait_for_restart() {
                        return rc;
                    }
                    continue;
                }
            };
            // A word left from before this session is already answered.
            self.restart.store(false, Ordering::Relaxed);
            let (tx, rx) = std::sync::mpsc::channel();
            session.live = Some(rx);
            *self.live.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
            let rc = session.run();
            *self.live.lock().unwrap_or_else(|e| e.into_inner()) = None;
            if self.stop.load(Ordering::Relaxed) {
                return rc;
            }
            if session.restarted {
                continue;
            }
            if !self.page_up {
                return rc;
            }
            println!("  the watcher has stopped; the page is still up - Ctrl+C to leave");
            if !self.wait_for_restart() {
                return rc;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn m(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn what_a_change_asks() {
        let base = m(json!({"burst": 3, "volume": 0.9, "audit_enabled": true, "port": 8731}));
        let with = |k: &str, v: Value| {
            let mut x = base.clone();
            x.insert(k.into(), v);
            x
        };
        assert_eq!(reaction(&changed_keys(&base, &base)), Reaction::None);
        assert_eq!(
            reaction(&changed_keys(&base, &with("burst", json!(4)))),
            Reaction::Restart
        );
        assert_eq!(
            reaction(&changed_keys(&base, &with("burst", json!(3.0)))),
            Reaction::None,
            "the same number"
        );
        assert_eq!(
            reaction(&changed_keys(&base, &with("volume", json!(0.5)))),
            Reaction::Live { audio: true }
        );
        assert_eq!(
            reaction(&changed_keys(&base, &with("audit_enabled", json!(false)))),
            Reaction::Live { audio: false }
        );
        assert_eq!(
            reaction(&changed_keys(&base, &with("port", json!(9000)))),
            Reaction::Live { audio: false },
            "the page's own: nothing for the watcher"
        );
    }

    #[test]
    fn the_programs_own_flags() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let (f, rest) = split_flags(&v(&[
            "--verbose",
            "--port",
            "9000",
            "--no-browser",
            "--burst",
            "2",
        ]))
        .unwrap();
        assert_eq!(
            f,
            AppFlags {
                no_page: false,
                no_browser: true,
                port: Some(9000)
            }
        );
        assert_eq!(rest, v(&["--verbose", "--burst", "2"]));
        let (f, _) = split_flags(&v(&["--port=8800", "--no-page"])).unwrap();
        assert_eq!((f.port, f.no_page), (Some(8800), true));
        assert!(split_flags(&v(&["--port"])).is_err());
        assert!(split_flags(&v(&["--port", "x"])).is_err());
    }
}
