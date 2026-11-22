//! sc-watch-ui (ui_server.py's main): serves the page on 127.0.0.1 and opens
//! a browser at it. Same rule as the watcher: saved settings are the
//! defaults (`port`, `open_browser`), a flag still wins.

use std::net::TcpListener;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use chrono::Local;
use serde_json::Value;

use scw_rsi::{Https, SystemClock};
use scw_store::paths::Roots;
use scw_store::settings;
use scw_ui::jobs::{FOCUS_SWEEP_EVERY, FOCUS_SWEEP_FIRST};
use scw_ui::{HOST, Hooks, PORT, Ui, http};

fn usage(msg: &str) -> i32 {
    eprintln!("usage: sc-watch-ui [-h] [--port PORT] [--no-browser]\nsc-watch-ui: error: {msg}");
    2
}

fn reveal(p: &std::path::Path) -> Result<(), String> {
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

fn open_browser(url: &str) {
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

/// The "hear it" voice: one alert player for the life of the page.
fn speaker(roots: Roots) -> impl Fn(&str) -> Result<(), String> + Send + Sync {
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

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let roots = Roots::current();
    let cfg = settings::load(&roots.data.join("settings.json"));
    let mut port = cfg
        .get("port")
        .and_then(Value::as_i64)
        .unwrap_or(i64::from(PORT));
    let mut no_browser = !cfg
        .get("open_browser")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        let a = argv[i].as_str();
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) => (f, Some(v.to_string())),
            None => (a, None),
        };
        match flag {
            "-h" | "--help" => {
                println!(
                    "usage: sc-watch-ui [-h] [--port PORT] [--no-browser]\n\n\
                     Second-screen UI: a tile per contact, served locally.\n\n\
                     options:\n  -h, --help     show this help message and exit\n  \
                     --port PORT\n  --no-browser"
                );
                return 0;
            }
            "--no-browser" => no_browser = true,
            "--port" => {
                let v = match inline {
                    Some(v) => v,
                    None => {
                        i += 1;
                        match argv.get(i) {
                            Some(v) => v.clone(),
                            None => return usage("argument --port: expected one argument"),
                        }
                    }
                };
                match scw_store::json::py_int_text(&v) {
                    Some(n) => port = n,
                    None => return usage(&format!("argument --port: invalid int value: '{v}'")),
                }
            }
            _ => return usage(&format!("unrecognized arguments: {a}")),
        }
        i += 1;
    }
    let listener = match u16::try_from(port).ok().and_then(|p| {
        TcpListener::bind((HOST, p))
            .map_err(|e| eprintln!("{e}"))
            .ok()
    }) {
        Some(l) => l,
        None => {
            println!("cannot listen on {HOST}:{port}");
            println!(
                "something is already using that port. If it is an old sc-watch UI, close it; \
                 otherwise change the port in Settings."
            );
            return 1;
        }
    };
    let hooks = Hooks {
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
        env: Box::new(|k| std::env::var(k).ok()),
        today: Box::new(|| Local::now().date_naive()),
        utc: false,
    };
    let ui = Arc::new(Ui::new(
        roots.clone(),
        Arc::new(SystemClock),
        Arc::new(Https::new()),
        hooks,
    ));
    let url = format!("http://{HOST}:{port}/");
    let rule = "=".repeat(58);
    println!("{rule}");
    println!("  sc-watch UI");
    println!("  {url}");
    println!("  your data {}", roots.data.display());
    println!("  Ctrl+C to stop");
    println!("{rule}");
    if !no_browser {
        let u = url.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(600));
            open_browser(&u);
        });
    }
    {
        let ui = ui.clone();
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
    {
        let ui = ui.clone();
        let _ = ctrlc::set_handler(move || {
            ui.shutting_down.store(true, Ordering::Relaxed);
            std::process::exit(0);
        });
    }
    http::serve(ui, listener);
    0
}

#[cfg(not(windows))]
struct NoInput;

#[cfg(not(windows))]
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
