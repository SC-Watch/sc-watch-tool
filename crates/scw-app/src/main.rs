//! sc-watch: the watcher and its page in one process.
//!
//! Takes the watcher's options (sc-watch-watcher's, which are watch.py's)
//! and three of its own: `--no-page` (the watcher alone), `--port N` and
//! `--no-browser`. The page is served while the watcher runs; restarting
//! the watcher from the page, or saving its settings, starts a new session
//! in the same process.

use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde_json::Value;

use scw_app::{App, KeepOpen, split_flags};
use scw_rsi::{ClientHub, Https, SystemClock};
use scw_store::db::Changes;
use scw_store::paths::Roots;
use scw_store::settings;
use scw_ui::{HOST, PORT, Ui, desktop, http};
use scw_watch::console::Terminal;
use scw_watch::live::AudioSink;
use scw_watch::program::{self, Shared};
use scw_watch::session::InProcess;
use scw_watch::sinks::{AlertSink, Console};

fn main() {
    std::process::exit(run());
}

#[allow(clippy::too_many_lines)]
fn run() -> i32 {
    let roots = Roots::current();
    let all: Vec<String> = std::env::args().skip(1).collect();
    let (flags, argv) = match split_flags(&all) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("usage: sc-watch [options]\nsc-watch: error: {e}");
            return 2;
        }
    };
    let cur = settings::load(&roots.data.join("settings.json"));
    let (mut args, _) = match program::parse(&argv, &roots, &cur) {
        Ok(v) => v,
        Err(rc) => return rc,
    };
    if let Some(rc) = program::utilities(&args, &roots) {
        return rc;
    }
    program::check_reader(&mut args);

    // What the sessions and the page share.
    let changes = Arc::new(Changes::default());
    let board = Arc::new(scw_store::board::Board::new(Some(changes.clone())));
    let env: scw_rsi::hub::Env = Arc::new(|k| std::env::var(k).ok());
    let mut hub = ClientHub::new(
        roots.data.clone(),
        Arc::new(SystemClock),
        Arc::new(Https::new()),
        "sc-watch",
        false,
        env,
    );
    {
        let ch = changes.clone();
        hub.on_cache = Some(Box::new(move |cache| cache.watch(ch.clone())));
    }
    let hub = Arc::new(hub);
    let console: Arc<dyn Console> = Arc::new(Terminal::stdout());
    let audio = Arc::new(AudioSink::new(program::make_player(
        args.audio.as_deref(),
        &cur,
        &roots,
    )));
    if !args.no_rsi
        && let Some(notice) = program::lookup_notice(&cur, hub.api_key().is_some(), &roots)
    {
        eprintln!("{notice}");
    }

    // The page.
    let port = flags.port.unwrap_or_else(|| {
        cur.get("port")
            .and_then(Value::as_i64)
            .unwrap_or(i64::from(PORT))
    });
    let listener = if flags.no_page {
        None
    } else {
        match u16::try_from(port)
            .ok()
            .and_then(|p| TcpListener::bind((HOST, p)).ok())
        {
            Some(l) => Some(l),
            None => {
                println!(
                    "! cannot serve the page on {HOST}:{port} - something else is using that \
                     port (an old sc-watch?). The watcher runs without it; change the port in \
                     Settings, or close the other one."
                );
                None
            }
        }
    };
    let app = Arc::new(App::new(
        roots.clone(),
        argv.clone(),
        Some(audio.clone()),
        listener.is_some(),
    ));
    {
        let stop = app.stop.clone();
        let _ = ctrlc::set_handler(move || {
            // The second Ctrl+C leaves at once, whatever is still running.
            if stop.swap(true, Ordering::Relaxed) {
                std::process::exit(130);
            }
        });
    }
    if let Some(listener) = listener {
        let mut ui = Ui::with_hub(
            roots.clone(),
            Arc::new(SystemClock),
            hub.clone(),
            desktop::hooks(&roots),
        );
        ui.changes = Some(changes.clone());
        ui.board = Some(board.clone());
        ui.links = Some(app.links());
        let ui = Arc::new(ui);
        let url = format!("http://{HOST}:{port}/");
        println!("  page: {url}");
        let no_browser = flags.no_browser
            || !cur
                .get("open_browser")
                .and_then(Value::as_bool)
                .unwrap_or(true);
        if !no_browser {
            let u = url.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(600));
                desktop::open_browser(&u);
            });
        }
        desktop::start_sweeper(ui.clone());
        std::thread::spawn(move || http::serve(ui, listener));
    }

    // The watcher, session after session.
    app.supervise(|app| {
        let cur = settings::load(&app.roots.data.join("settings.json"));
        let (mut args, arg_map) = program::parse(&app.argv, &app.roots, &cur)?;
        program::check_reader(&mut args);
        program::build_session(&mut args, &arg_map, &cur, &app.roots, |args| Shared {
            console: console.clone(),
            alerts: app
                .audio
                .clone()
                .map(|a| Arc::new(KeepOpen(a)) as Arc<dyn AlertSink>),
            client: if args.no_rsi { None } else { hub.get().ok() },
            restart: Box::new(InProcess(app.restart.clone())),
            stop: app.stop.clone(),
            changes: Some(changes.clone()),
            board: Some(board.clone()),
        })
    })
}
