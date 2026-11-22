//! The page's real surroundings: threads, the file manager, the browser,
//! the "hear it" voice, the keys - the [`Hooks`] a running program gives
//! its page (tests give recorders).

use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use chrono::Local;
use serde_json::Value;

use scw_store::paths::Roots;
use scw_store::settings;

use crate::jobs::{FOCUS_SWEEP_EVERY, FOCUS_SWEEP_FIRST};
use crate::{Hooks, Ui};

/// Shows a folder in the file manager.
pub fn reveal(p: &Path) -> Result<(), String> {
    let prog = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(prog)
        .arg(p)
        .spawn()
        .map(|_| ())
        .map_err(|_| "OSError".to_string())
}

/// Opens the page in the default browser.
pub fn open_browser(url: &str) {
    let _ = if cfg!(windows) {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
}

/// The "hear it" voice: one speaking alert player for the life of the page,
/// whatever the alerts' own mode (a tones-only player cannot say a line).
pub fn speaker(roots: Roots) -> impl Fn(&str) -> Result<(), String> + Send + Sync {
    let voice: Mutex<Option<scw_audio::Alerts>> = Mutex::new(None);
    move |text: &str| {
        let mut g = voice.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_none() {
            let path = roots.data(&["audio.json"]);
            let mut cfg = scw_audio::AudioConfig::load(&path);
            let s = settings::load(&roots.data.join("settings.json"));
            cfg.mode = "speech".into();
            if let Some(v) = s.get("volume").and_then(Value::as_f64) {
                cfg.volume = v;
            }
            if let Some(v) = s.get("rate").and_then(Value::as_i64) {
                cfg.rate = v;
            }
            #[cfg(windows)]
            let a = scw_audio::Alerts::new(
                cfg,
                &path,
                &roots.data_dir(&["tones"]),
                None,
                Some(scw_audio::windows::SapiVoice::maker()),
            );
            #[cfg(not(windows))]
            let a = scw_audio::Alerts::new(cfg, &path, &roots.data_dir(&["tones"]), None, None);
            if !a.health().speech {
                return Err("no speech engine on this machine".into());
            }
            *g = Some(a);
        }
        if let Some(a) = g.as_ref() {
            a.say(text);
        }
        Ok(())
    }
}

/// No keys or joysticks to read.
pub struct NoInput;

impl scw_input::InputState for NoInput {
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

/// The hooks of a running program.
pub fn hooks(roots: &Roots) -> Hooks {
    Hooks {
        spawn: Box::new(|job| {
            std::thread::spawn(job);
        }),
        reveal: Box::new(reveal),
        speak: Box::new(speaker(roots.clone())),
        input: Box::new(|| -> Box<dyn scw_input::InputState> {
            #[cfg(windows)]
            {
                Box::new(scw_input::windows::WindowsInput)
            }
            #[cfg(not(windows))]
            {
                Box::new(NoInput)
            }
        }),
        env: Arc::new(|k| std::env::var(k).ok()),
        today: Box::new(|| Local::now().date_naive()),
        utc: false,
    }
}

/// The background check, on its own thread: first a minute after start,
/// then every ten, until the page shuts down.
pub fn start_sweeper(ui: Arc<Ui>) {
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs_f64(FOCUS_SWEEP_FIRST));
        while !ui.shutting_down.load(Ordering::Relaxed) {
            for line in ui.sweep_once() {
                println!("{line}");
            }
            std::thread::sleep(std::time::Duration::from_secs_f64(FOCUS_SWEEP_EVERY));
        }
    });
}
