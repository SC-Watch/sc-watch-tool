//! sc-watch-ui (ui_server.py's main): serves the page on 127.0.0.1 and opens
//! a browser at it. Same rule as the watcher: saved settings are the
//! defaults (`port`, `open_browser`), a flag still wins. (`sc-watch` serves
//! the same page from the watcher's own process.)

use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde_json::Value;

use scw_rsi::{Https, SystemClock};
use scw_store::paths::Roots;
use scw_store::settings;
use scw_ui::{HOST, PORT, Ui, desktop, http};

fn usage(msg: &str) -> i32 {
    eprintln!("usage: sc-watch-ui [-h] [--port PORT] [--no-browser]\nsc-watch-ui: error: {msg}");
    2
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
    let ui = Arc::new(Ui::new(
        roots.clone(),
        Arc::new(SystemClock),
        Arc::new(Https::new()),
        desktop::hooks(&roots),
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
            desktop::open_browser(&u);
        });
    }
    desktop::start_sweeper(ui.clone());
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
