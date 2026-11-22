//! sc-watch, the watcher (watch.py's main): reads player handles off the
//! Star Citizen HUD, announces them, records them and looks them up.
//!
//! Everything that can be wrong with the configuration is checked before
//! anything is opened; then the capture, the alert player, the lookup
//! client, the session log, the database, the lookup worker and
//! sightings.csv are opened and handed to the session.
//!
//! This version reads with IRIS alone: no OCR, so no `ocr`/`iris+ocr`
//! reader, no focused reads and no chat reads yet.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::Local;
use serde_json::{Map, Value, json};

use scw_capture::{HdrCapture, ReplaySource, Source};
use scw_rsi::{Cache, Client, Clock, Https, Options, Provider, RequestLog, SystemClock};
use scw_store::flags::Flags;
use scw_store::housekeeping::{Policy, run_now};
use scw_store::paths::Roots;
use scw_store::reputation::Database;
use scw_store::settings;
use scw_store::watchlist;
use scw_vision::pipeline::ScanArea;
use scw_watch::cli::{self, Args};
use scw_watch::console::Terminal;
use scw_watch::enricher::{EnricherThread, Owned};
use scw_watch::live::{
    AudioSink, BoundKeys, CaptureFrames, IrisDetector, NoInput, WorkerQueue, input_state,
    row_reader,
};
use scw_watch::session::{FlagFile, LookupQueue, NoLookups, Parts, Session, SessionOptions};
use scw_watch::sinks::{AlertSink, Console, EventLog, Lookups};
use scw_watch::{DebugLog, Scrub, SightingsLog, config};

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

fn main() {
    std::process::exit(run());
}

/// What the HDR check found for the captured display, for the banner.
struct HdrState {
    device: String,
    on: bool,
    sdr_white_nits: f64,
    /// The float capture was asked for and Windows HDR is on.
    capturing: bool,
}

/// The frame source, its banner line and what the HDR check found.
type Opened = (Box<dyn Source>, String, Option<HdrState>);

fn open_source(args: &Args) -> Result<Opened, String> {
    if let Some(pattern) = &args.replay {
        let paths = scw_capture::replay::expand(pattern)?;
        if paths.is_empty() {
            return Err(format!("no frames match {pattern}"));
        }
        println!("replaying {} frames from {pattern}", paths.len());
        let src = ReplaySource::new(paths);
        let what = src.describe();
        return Ok((Box::new(src), what, None));
    }
    let hdr = HdrCapture::parse(&args.hdr_capture).unwrap_or(HdrCapture::Auto);
    open_screen(args.monitor, hdr)
}

#[cfg(windows)]
fn open_screen(monitor: i64, hdr: HdrCapture) -> Result<Opened, String> {
    use scw_capture::windows::{DesktopSource, state_of};
    let src = DesktopSource::open(monitor.max(0) as usize, hdr).map_err(|e| e.to_string())?;
    let device = src.info().device.clone();
    let state = state_of(&device).map(|s| HdrState {
        device: s.device.clone(),
        on: s.on,
        sdr_white_nits: s.sdr_white_nits,
        capturing: hdr.wanted(Some(s.on)),
    });
    let what = src.describe();
    Ok((Box::new(src), what, state))
}

#[cfg(not(windows))]
fn open_screen(_monitor: i64, _hdr: HdrCapture) -> Result<Opened, String> {
    Err("live capture is Windows-only in this version - use --replay".into())
}

fn key_help(roots: &Roots) -> String {
    format!(
        "    1. register at https://starcitizen-api.com (the free tier is enough) and copy your key\n    \
         2. paste it into the UI: Settings > RSI lookups - or save it, alone on one line, as\n       \
         {}\n    3. restart the watcher",
        roots.data.join("sc_api_key.txt").display()
    )
}

/// The profile client, shared by the database (to verify identities before
/// merging look-alike handles) and the lookup worker.
fn profile_client(args: &Args, cur: &Map<String, Value>, roots: &Roots) -> Option<Arc<Client>> {
    if args.no_rsi {
        return None;
    }
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let cache = match Cache::open(&roots.data(&["rsi_cache.db"]), clock.clone()) {
        Ok(c) => Arc::new(c),
        Err(e) => {
            eprintln!("! profile lookup disabled: {e}");
            return None;
        }
    };
    let text = |k: &str, d: &str| cur.get(k).and_then(Value::as_str).unwrap_or(d).to_string();
    let key = scw_rsi::find_api_key(
        env("SC_API_KEY").as_deref(),
        &roots.data.join("sc_api_key.txt"),
    );
    let sentry = text("rsi_source", "sentry") != "sci";
    let has_key = key.is_some();
    let enriches = has_key || sentry;
    if !enriches {
        // Link mode rather than lookups off: a handle with a URL beside it
        // is still useful.
        eprintln!(
            "! no starcitizen-api.com key, and Lookup source is set to starcitizen-api.com \
             only - so nobody is looked up; handles come with a lookup link instead of a \
             profile.\n  Set Settings > RSI lookups > Lookup source to 'sentry' to look \
             players up without a key, or add one:\n{}",
            key_help(roots)
        );
    } else if !has_key {
        eprintln!(
            "! no starcitizen-api.com key - players are looked up through SENTRY only. \
             Without a key:\n    - nothing else is asked while SENTRY is down or busy, so \
             lookups wait for it\n    - an org's type is known only if it is already \
             cached, so a new org gets no 'Syndicate' mark\n  Orgs, their Piracy / \
             Smuggling focus, account age, flagged-org alerts and 'no RSI account' audits \
             all still work. To add a key:\n{}",
            key_help(roots)
        );
    }
    let (a, b) = scw_rsi::SENTRY_LOG;
    let opts = Options {
        api_key: key,
        mode: text("rsi_mode", "auto"),
        provider: if enriches {
            Provider::Api
        } else {
            Provider::Link
        },
        sentry,
        log: Some(Arc::new(RequestLog {
            path: roots.data(&[a, b]),
            process: "watcher".into(),
            utc: false,
        })),
        ..Options::default()
    };
    Some(Arc::new(Client::new(
        cache,
        Arc::new(Https::new()),
        clock,
        opts,
    )))
}

/// sc-watch.db, with the report flags, the verifier and the shared
/// watchlist; None with --no-db or when it cannot be opened.
fn open_database(
    roots: &Roots,
    cur: &Map<String, Value>,
    client: Option<&Arc<Client>>,
) -> Result<Database, String> {
    let mut db = Database::open(&roots.data(&["sc-watch.db"])).map_err(|e| e.to_string())?;
    db.flags = Flags::load(&roots.data.join("flags.json"));
    if let Some(c) = client.filter(|c| c.enriches()) {
        db.verify = Some(Box::new(scw_rsi::upkeep::Verifier(c.clone())));
    }
    if cur
        .get("watchlist_enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true)
    {
        // Tried once: a third-party list must never be why the tool will
        // not run.
        let path = roots.data.join("watchlist.db");
        let starter = roots.program.join("starter_list.json");
        if (path.exists() || starter.exists())
            && let Ok(mut wl) = watchlist::Watchlist::open(&roots.data(&["watchlist.db"]))
        {
            wl.starter_name = watchlist::starter_name(&starter);
            let _ = watchlist::seed_starter(&starter, &wl);
            db.watchlist = Some(Box::new(wl));
        }
    }
    Ok(db)
}

/// The alert player: audio.json, with the settings page's knobs over it.
fn make_alerts(args: &Args, cur: &Map<String, Value>, roots: &Roots) -> Option<Arc<dyn AlertSink>> {
    let path = roots.data(&["audio.json"]);
    let mut cfg = scw_audio::AudioConfig::load(&path);
    if let Some(v) = cur.get("volume").and_then(Value::as_f64) {
        cfg.volume = v;
    }
    if let Some(v) = cur.get("rate").and_then(Value::as_i64) {
        cfg.rate = v;
    }
    if let Some(v) = cur.get("speak_handles").and_then(Value::as_bool) {
        cfg.speak_handles = v;
    }
    if let Some(v) = cur.get("max_spoken_per_burst").and_then(Value::as_i64) {
        cfg.max_spoken_per_burst = v;
    }
    if let Some(v) = cur.get("summarise_above").and_then(Value::as_i64) {
        cfg.summarise_above = v;
    }
    if let Some(m) = &args.audio {
        cfg.mode = m.clone();
    }
    let tones = roots.data_dir(&["tones"]);
    #[cfg(windows)]
    let alerts = scw_audio::Alerts::new(
        cfg,
        &path,
        &tones,
        Some(Box::new(scw_audio::windows::WinTones)),
        Some(scw_audio::windows::SapiVoice::maker()),
    );
    #[cfg(not(windows))]
    let alerts = scw_audio::Alerts::new(cfg, &path, &tones, None, None);
    Some(Arc::new(AudioSink::new(alerts)))
}

fn api_key_state(roots: &Roots) -> String {
    let e = env("SC_API_KEY").unwrap_or_default();
    let e = e.trim();
    if !e.is_empty() {
        return format!("set via SC_API_KEY ({} chars)", e.chars().count());
    }
    if let Ok(t) = std::fs::read_to_string(roots.data.join("sc_api_key.txt")) {
        return format!(
            "set via sc_api_key.txt ({} chars)",
            t.trim_start_matches('\u{feff}').trim().chars().count()
        );
    }
    "not set".into()
}

/// Starts this program over with the same arguments; returns only if that
/// failed. (Windows has no exec: a new process, and this one leaves.)
fn relaunch() -> String {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => return format!("{e}"),
    };
    match std::process::Command::new(exe).args(argv).spawn() {
        Ok(_) => std::process::exit(0),
        Err(e) => format!("{e}"),
    }
}

fn scan_area(s: &str) -> ScanArea {
    ScanArea::parse(s).unwrap_or(ScanArea::Whole)
}

#[allow(clippy::too_many_lines)]
fn run() -> i32 {
    let roots = Roots::current();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cur = settings::load(&roots.data.join("settings.json"));
    let arg_map = match cli::parse_map(&argv, cli::defaults(&roots.data, &cur)) {
        Ok(m) => m,
        Err(e) if e.code == 0 => {
            print!("{}", cli::help());
            return 0;
        }
        Err(e) => {
            eprintln!("usage: sc-watch [options]\nsc-watch: error: {}", e.message);
            return e.code;
        }
    };
    let mut args = Args::from_map(&arg_map);

    // The one-shot commands, answered before anything is opened.
    if args.where_ {
        println!("{}", roots.describe(&env));
        return 0;
    }
    if args.list_inputs {
        let devs = input_state().devices();
        println!("{} joystick(s):", devs.len());
        for d in &devs {
            println!("  {}", d.describe());
        }
        if devs.is_empty() {
            println!("  none. Only keyboard bindings are available.");
        }
        return 0;
    }
    if args.support_bundle {
        eprintln!("--support-bundle is not in this version yet: the Python build makes one");
        return 1;
    }
    if args.check_resolution.is_some() {
        eprintln!("--check-resolution is not in this version yet: the Python build has it");
        return 1;
    }
    if args.reader != "iris" {
        eprintln!(
            "! reader '{}' needs OCR, which this version does not have - reading with IRIS alone",
            args.reader
        );
        args.reader = "iris".into();
    }

    // The configuration, checked before anything is opened.
    let key_mode = args.trigger == "key" && args.replay.is_none();
    let state: Box<dyn scw_input::InputState> = if key_mode {
        input_state()
    } else {
        Box::new(NoInput)
    };
    let checked = (|| -> Result<_, String> {
        let binds = config::parse_bindings(
            &args.key,
            &args.focus_key,
            &args.dual_key,
            &args.chat_key,
            key_mode,
            &*state,
        )
        .map_err(|e| e.0)?;
        config::parse_chat_region(&args.chat_region).map_err(|e| e.0)?;
        config::parse_focus_size(&args.focus_size).map_err(|e| e.0)?;
        let src = open_source(&args)?;
        Ok((binds, src))
    })();
    let (binds, (src, capture, hdr)) = match checked {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    let rows = match row_reader() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("! the IRIS letter model will not load: {e}");
            return 1;
        }
    };

    let alerts = make_alerts(&args, &cur, &roots);
    let con: Arc<dyn Console> = Arc::new(Terminal::stdout());
    let client = profile_client(&args, &cur, &roots);
    let dbg = Arc::new(DebugLog::open(
        &roots.data,
        args.debug_log,
        "",
        Scrub::from_env(),
        Box::new(|| Local::now().naive_local()),
    ));
    let db = if args.no_db {
        None
    } else {
        match open_database(&roots, &cur, client.as_ref()) {
            Ok(db) => Some(db),
            Err(e) => {
                eprintln!("! database disabled: {e}");
                None
            }
        }
    };
    let lookups: Box<dyn LookupQueue> = match client.clone() {
        Some(c) => {
            let (con2, log2, alerts2) = (con.clone(), dbg.clone(), alerts.clone());
            let (root2, cur2, no_db) = (roots.clone(), cur.clone(), args.no_db);
            Box::new(WorkerQueue(EnricherThread::start(
                args.audit,
                Box::new(move || Owned {
                    db: (!no_db)
                        .then(|| open_database(&root2, &cur2, Some(&c)).ok())
                        .flatten(),
                    lookups: c.clone() as Arc<dyn Lookups>,
                    console: con2,
                    log: log2 as Arc<dyn EventLog>,
                    alerts: alerts2,
                    data_root: root2.data.clone(),
                    clock: Arc::new(SystemClock),
                }),
            )))
        }
        None => Box::new(NoLookups),
    };
    let sightings = match SightingsLog::open(Path::new(&args.log)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("! cannot write {}: {e}", args.log);
            return 1;
        }
    };
    if key_mode {
        for (mode, b) in &binds {
            let missing = b.missing();
            if !missing.is_empty() {
                eprintln!(
                    "! {mode} binding {} needs a device that is not plugged in ({})",
                    b.label(),
                    missing.join(", ")
                );
            }
        }
    }
    // --hz comes straight off the command line, and 0 divides.
    if args.hz <= 0.0 || args.hz.is_nan() {
        println!(
            "! --hz {} is not a rate; using 5 Hz",
            scw_store::json::py_repr(&json!(args.hz))
        );
        args.hz = 5.0;
    }

    banner(
        &args,
        &binds,
        &roots,
        &sightings.path,
        &dbg,
        &cur,
        &arg_map,
        &capture,
        hdr.as_ref(),
        key_mode,
    );

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
    let b = |k: &str, d: bool| cur.get(k).and_then(Value::as_bool).unwrap_or(d);
    let n = |k: &str, d: f64| cur.get(k).and_then(Value::as_f64).unwrap_or(d);
    let opts = SessionOptions {
        key_mode,
        replay: args.replay.is_some(),
        burst: args.burst.max(0) as usize,
        burst_spacing: args.burst_spacing,
        ping_delay: args.ping_delay,
        queue_max: args.queue_max.max(0) as usize,
        min_votes: args.min_votes.max(0) as usize,
        rsi_min_votes: args.rsi_min_votes.max(0) as usize,
        verbose: args.verbose,
        debug: args.debug,
        audit: args.audit,
        dual_on_read: args.dual_on_read,
        focus_always: args.focus_always,
        hz: args.hz,
        auto_read_delay: args.auto_read_delay,
        seconds: args.seconds,
        radius: args.radius,
        expire: args.expire,
        cooldown: args.cooldown,
        audit_keep_frame: b("audit_keep_frame", true),
        audit_frame_quality: n("audit_frame_quality", 90.0).clamp(1.0, 100.0) as u8,
        housekeeping_enabled: b("housekeeping_enabled", true),
        housekeeping_every_min: n("housekeeping_every_min", 30.0),
    };
    let hk_roots = roots.clone();
    let hk_cur = cur.clone();
    let parts = Parts {
        frames: Box::new(CaptureFrames::new(src, con.clone())),
        detector: Box::new(IrisDetector::new(
            rows,
            scan_area(&args.scan_area),
            scan_area(&args.auto_scan_area),
        )),
        keys: Box::new(BoundKeys::new(binds, state, stop.clone())),
        console: con.clone(),
        alerts,
        log: dbg.clone(),
        db,
        lookups,
        sightings,
        clock: Arc::new(SystemClock),
        local_now: Box::new(|| Local::now().naive_local()),
        data_root: roots.data.clone(),
        restart: Box::new(FlagFile {
            path: roots.data.join("restart.flag"),
            relaunch: Box::new(relaunch),
        }),
        housekeeper: Box::new(move |db, now| {
            let policy = Policy::load(&hk_roots.data.join("housekeeping.json"), Some(&hk_cur));
            // The audit rows are pruned even with --no-db, as the Python
            // does, when there is a database to prune.
            let own = if db.is_none() && hk_roots.data.join("sc-watch.db").exists() {
                Database::open(&hk_roots.data.join("sc-watch.db")).ok()
            } else {
                None
            };
            run_now(&policy, &hk_roots.data, db.or(own.as_ref()), now, true)
        }),
        stop,
    };
    let mut session = Session::new(opts, parts);
    // Startup is the right moment for retention: last session's frames
    // have had their chance to be looked at.
    if b("housekeeping_on_start", true) {
        session.housekeep(true);
    }
    session.run()
}

#[allow(clippy::too_many_arguments)]
fn banner(
    args: &Args,
    binds: &[(String, scw_input::Binding)],
    roots: &Roots,
    log_path: &Path,
    dbg: &DebugLog,
    cur: &Map<String, Value>,
    arg_map: &Map<String, Value>,
    capture: &str,
    hdr: Option<&HdrState>,
    key_mode: bool,
) {
    let rule = "=".repeat(62);
    let label = |m: &str| binds.iter().find(|(k, _)| k == m).map(|(_, b)| b.label());
    println!("{rule}");
    println!("  sc-watch");
    if key_mode {
        println!(
            "  trigger: {} pressed, then read after {:.1}s",
            label("full").unwrap_or_default(),
            args.ping_delay
        );
        if let Some(l) = label("focus") {
            println!(
                "  focused: {l} does a full read (focused reads are off with the 'iris' reader)"
            );
        }
        if let Some(l) = label("dual") {
            println!("  dual:    {l} does a full read (no OCR in this version)");
        }
        if let Some(l) = label("chat") {
            println!(
                "  chat:    {l} - reading chat needs OCR, which this version does not have yet"
            );
        }
    } else {
        println!("  trigger: continuous, {:.0} Hz", args.hz);
    }
    println!(
        "  {}-frame bursts, {:.0}s repeat cooldown",
        args.burst, args.cooldown
    );
    let shown = std::fs::canonicalize(log_path).unwrap_or_else(|_| log_path.to_path_buf());
    println!("  logging to {}", plain(&shown).display());
    println!("  your data {}", roots.data.display());
    if let Some(old) = roots.unused_old_data(&env) {
        println!("  ! an older sc-watch's data is at {}", old.display());
        println!("    and this copy is NOT using it. To bring it over: close both");
        println!(
            "    windows, copy what is in that folder into {},",
            roots.data.display()
        );
        println!("    and start again (README: Upgrading).");
    }
    println!("  reading with IRIS's letter model (this version has no OCR)");
    println!(
        "  full reads: IRIS's finder and reader (scan area {}), OCR never (reader iris)",
        args.scan_area
    );
    if !key_mode {
        println!(
            "  always on: IRIS's scan on every frame (scan area {}) notices new contacts",
            args.auto_scan_area
        );
    }
    if args.debug {
        println!("  DEBUG: saving every frame to debug_bursts/ (~4.7 MB each)");
    }
    if let Some(h) = hdr {
        if h.capturing {
            println!(
                "  HDR capture: Windows HDR is on for {} - reading the HDR picture and",
                h.device
            );
            println!(
                "    converting it (SDR white {:.0} nits), so nothing is blown out. Keep Star",
                h.sdr_white_nits
            );
            println!(
                "    Citizen's own HDR option off: it makes HUD text bloom (NVIDIA RTX HDR does not)."
            );
        } else if h.on {
            for line in [
                format!(
                    "  ! Windows HDR is ON for {} - the screen sc-watch captures.",
                    h.device
                ),
                "    With HDR on, the copy of the screen Windows hands to capture tools".into(),
                "    treats 80 nits as white, so most of the picture arrives blown out".into(),
                "    and labels on anything bright are missed. Turn HDR off for this".into(),
                "    display while sc-watch runs: Windows Settings > System > Display >".into(),
                "    Use HDR. Star Citizen's own HDR option should be off as well - it".into(),
                "    makes HUD text bloom.".into(),
            ] {
                println!("{line}");
            }
            if args.hdr_capture == "off" {
                println!("    Or set Settings > Capture > HDR capture to 'auto'.");
            }
        }
    }

    let mut environment: Vec<(String, String)> = vec![
        (
            "sc-watch".into(),
            format!("{} (Rust)", env!("CARGO_PKG_VERSION")),
        ),
        (
            "platform".into(),
            format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        ),
        ("api key".into(), api_key_state(roots)),
        ("mode".into(), roots.mode.as_str().into()),
        ("program".into(), roots.program.display().to_string()),
        ("your data".into(), roots.data.display().to_string()),
        (
            "reader".into(),
            format!("iris, scan area {}, no OCR", args.scan_area),
        ),
        ("monitor".into(), args.monitor.to_string()),
        ("capture hz".into(), format!("{}", args.hz)),
        (
            "trigger".into(),
            if key_mode { "key" } else { "auto" }.into(),
        ),
        (
            "bindings".into(),
            if key_mode {
                config::waiting_label(binds)
            } else {
                "n/a".into()
            },
        ),
        (
            "windows hdr".into(),
            match (args.replay.is_some(), hdr) {
                (true, _) => "n/a (replay)".into(),
                (false, None) => "unknown".into(),
                (false, Some(h)) if h.on => format!("ON, SDR white {:.0} nits", h.sdr_white_nits),
                (false, Some(_)) => "off".into(),
            },
        ),
        ("capture".into(), capture.to_string()),
    ];
    if let Some(old) = roots.unused_old_data(&env) {
        environment.push(("NOT USED".into(), old.display().to_string()));
    }
    let overrides: Vec<(String, Value)> = arg_map
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    dbg.header(&environment, cur, &overrides);
    if let Some(h) = hdr.filter(|h| h.on) {
        dbg.event(
            "windows_hdr_on",
            &[
                ("display", json!(h.device)),
                ("sdr_white_nits", json!(h.sdr_white_nits.round())),
            ],
        );
    }
    if let Some(p) = &dbg.path
        && let Some(name) = p.file_name()
    {
        println!("  log: {} in debug_bursts/", name.to_string_lossy());
    }
    println!("  Ctrl+C to stop");
    println!("{rule}");
}

/// A canonical Windows path without its \\?\ prefix.
fn plain(p: &Path) -> PathBuf {
    let s = p.display().to_string();
    PathBuf::from(s.strip_prefix(r"\\?\").unwrap_or(&s))
}
