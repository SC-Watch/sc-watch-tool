//! sc-watch-watcher, the watcher on its own (watch.py's main): reads player
//! handles off the Star Citizen HUD, announces them, records them and looks
//! them up. Its page is sc-watch-ui; `sc-watch` runs both in one process.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use scw_store::paths::Roots;
use scw_store::settings;
use scw_watch::console::Terminal;
use scw_watch::program::{self, Shared};
use scw_watch::session::FlagFile;

fn main() {
    std::process::exit(run());
}

fn run() -> i32 {
    let roots = Roots::current();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cur = settings::load(&roots.data.join("settings.json"));
    let (mut args, arg_map) = match program::parse(&argv, &roots, &cur) {
        Ok(v) => v,
        Err(rc) => return rc,
    };
    if let Some(rc) = program::utilities(&args, &roots) {
        return rc;
    }
    program::check_reader(&mut args);
    let built = program::build_session(&mut args, &arg_map, &cur, &roots, |args| {
        let stop = Arc::new(AtomicBool::new(false));
        {
            let stop = stop.clone();
            let _ = ctrlc::set_handler(move || {
                // The second Ctrl+C leaves at once, whatever is still running.
                if stop.swap(true, Ordering::Relaxed) {
                    std::process::exit(130);
                }
            });
        }
        Shared {
            console: Arc::new(Terminal::stdout()),
            alerts: program::make_alerts(args, &cur, &roots),
            client: program::profile_client(args, &cur, &roots),
            restart: Box::new(FlagFile {
                path: roots.data.join("restart.flag"),
                relaunch: Box::new(program::relaunch),
            }),
            stop,
            changes: None,
        }
    });
    match built {
        Ok(mut session) => session.run(),
        Err(rc) => rc,
    }
}
