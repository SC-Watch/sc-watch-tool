//! The one process at work: real sessions (on a made-up screen, reader and
//! key) and the real page, joined as sc-watch joins them.
//!
//! - a burst settled by the watcher reaches the page's stream at once, not
//!   at the next look at the files;
//! - a setting that needs no restart reaches the running session in place
//!   (audits switched off: the next weak read raises none);
//! - the page's restart button, and saving a setting the watcher reads at
//!   start, each start a new session in the same process;
//! - Ctrl+C ends it all.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use scw_app::App;
use scw_rsi::{ClientHub, FetchError, Reply as NetReply, SystemClock, Transport};
use scw_store::db::Changes;
use scw_store::paths::{Mode, Roots};
use scw_store::reputation::Database;
use scw_store::settings;
use scw_ui::routes::{Reply, Request};
use scw_ui::{Hooks, Ui};
use scw_vision::names::NameKind;
use scw_vision::pipeline::Contact;
use scw_vision::{Image, Rect};
use scw_watch::SightingsLog;
use scw_watch::program;
use scw_watch::session::{
    Detector, Frames, InProcess, Keys, Mode as KeyMode, NoLookups, Parts, Session, Stop,
};
use scw_watch::sinks::{Console, EventLog};
use serde_json::{Value, json};

struct NoNet;

impl Transport for NoNet {
    fn get(&self, url: &str, _: &str, _: f64) -> Result<NetReply, FetchError> {
        panic!("no network here: {url}")
    }
}

/// What the test steers the sessions by.
#[derive(Default)]
struct Probe {
    press: AtomicBool,
    polls: AtomicUsize,
    handle: Mutex<String>,
    said: Mutex<Vec<String>>,
}

struct Screen;

impl Frames for Screen {
    fn grab(&mut self) -> Option<Image<u8>> {
        Some(Image::new(64, 40, 3))
    }
}

struct Reader(Arc<Probe>);

impl Detector for Reader {
    fn detect(&mut self, _: &Image<u8>, _: KeyMode) -> Vec<Contact> {
        vec![Contact {
            name: Some(self.0.handle.lock().unwrap().clone()),
            kind: NameKind::Player,
            range_km: Some(1.5),
            name_box: Rect::new(10, 10, 30, 8),
            range_box: Rect::new(10, 20, 30, 8),
        }]
    }
    fn scan(&mut self, _: &Image<u8>) -> Vec<Rect> {
        Vec::new()
    }
}

struct Key(Arc<Probe>, Arc<AtomicBool>);

impl Keys for Key {
    fn modes(&self) -> Vec<KeyMode> {
        vec![KeyMode::Full]
    }
    fn pressed(&mut self, _: usize) -> Result<bool, Stop> {
        if self.1.load(Ordering::Relaxed) {
            return Err(Stop);
        }
        self.0.polls.fetch_add(1, Ordering::Relaxed);
        Ok(self.0.press.swap(false, Ordering::Relaxed))
    }
    fn drain(&mut self) {}
    fn partial(&mut self, _: usize) -> (usize, usize, Vec<String>) {
        (0, 1, vec![])
    }
    fn waiting_label(&self) -> String {
        "full:0".into()
    }
}

struct Con(Arc<Probe>);

impl Console for Con {
    fn say(&self, text: &str) {
        self.0.said.lock().unwrap().push(text.into());
    }
    fn status(&self, _: &str) {}
    fn print(&self, text: &str) {
        self.say(text);
    }
    fn eprint(&self, text: &str) {
        self.say(text);
    }
}

struct NoLog;

impl EventLog for NoLog {
    fn event(&self, _: &str, _: &[(&str, Value)]) {}
}

#[derive(Clone, Default)]
struct Frames2(Arc<Mutex<Vec<(Instant, String)>>>);

impl Write for Frames2 {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        let text = String::from_utf8_lossy(b).into_owned();
        if text.starts_with("data: ") {
            self.0.lock().unwrap().push((Instant::now(), text));
        }
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn wait_until(what: &str, secs: f64, ok: impl Fn() -> bool) {
    let until = Instant::now() + Duration::from_secs_f64(secs);
    while !ok() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn post(ui: &Arc<Ui>, path: &str, body: Value) -> Value {
    let body = body.to_string().into_bytes();
    let req = Request {
        method: "POST".into(),
        target: path.into(),
        headers: vec![
            ("Host".into(), "127.0.0.1".into()),
            ("Content-Type".into(), "application/json".into()),
            ("Content-Length".into(), body.len().to_string()),
        ],
        body,
    };
    match ui.handle(&req) {
        Reply::Ok { body, .. } => serde_json::from_slice(&body).unwrap(),
        other => panic!("{other:?}"),
    }
}

fn count(db: &PathBuf, sql: &str) -> i64 {
    let c = rusqlite::Connection::open(db).unwrap();
    c.query_row(sql, [], |r| r.get(0)).unwrap_or(0)
}

#[test]
fn one_process() {
    let base = std::env::temp_dir().join(format!("scw-app-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let data = base.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let roots = Roots {
        mode: Mode::Chosen,
        program: base.join("program"),
        data: data.clone(),
    };
    let changes = Arc::new(Changes::default());
    let hub = Arc::new(ClientHub::new(
        data.clone(),
        Arc::new(SystemClock),
        Arc::new(NoNet),
        "test",
        false,
        Arc::new(|_| None),
    ));
    let app = Arc::new(App::new(
        roots.clone(),
        vec!["--burst".into(), "1".into(), "--no-rsi".into()],
        None,
        true,
    ));
    let mut ui = Ui::with_hub(
        roots.clone(),
        Arc::new(SystemClock),
        hub,
        Hooks {
            spawn: Box::new(|job| job()),
            reveal: Box::new(|_| Ok(())),
            speak: Box::new(|_| Ok(())),
            input: Box::new(|| panic!("no keys here")),
            env: Arc::new(|_| None),
            today: Box::new(|| chrono::Local::now().date_naive()),
            utc: false,
        },
    );
    ui.changes = Some(changes.clone());
    ui.links = Some(app.links());
    let ui = Arc::new(ui);
    let probe = Arc::new(Probe::default());

    // The watcher, as sc-watch runs it - on made-up parts.
    let supervisor = {
        let (app, probe, changes) = (app.clone(), probe.clone(), changes.clone());
        std::thread::spawn(move || {
            app.supervise(|app| {
                let cur = settings::load(&app.roots.data.join("settings.json"));
                let (args, _) = program::parse(&app.argv, &app.roots, &cur)?;
                let mut db = Database::open(&app.roots.data.join("sc-watch.db")).unwrap();
                scw_store::db::watch(&mut db.conn, changes.clone());
                let parts = Parts {
                    frames: Box::new(Screen),
                    detector: Box::new(Reader(probe.clone())),
                    keys: Box::new(Key(probe.clone(), app.stop.clone())),
                    console: Arc::new(Con(probe.clone())),
                    alerts: None,
                    log: Arc::new(NoLog),
                    db: Some(db),
                    lookups: Box::new(NoLookups),
                    sightings: SightingsLog::open(&app.roots.data.join("sightings.csv")).unwrap(),
                    clock: Arc::new(SystemClock),
                    local_now: Box::new(|| chrono::Local::now().naive_local()),
                    data_root: app.roots.data.clone(),
                    restart: Box::new(InProcess(app.restart.clone())),
                    housekeeper: Box::new(|_, _| "housekeeping: nothing to do".into()),
                    stop: app.stop.clone(),
                };
                Ok(Session::new(
                    program::session_options(&args, &cur, true),
                    parts,
                ))
            })
        })
    };
    let sessions = || *app.sessions.lock().unwrap();
    wait_until("the first session to idle", 5.0, || {
        probe.polls.load(Ordering::Relaxed) > 5
    });
    assert_eq!(sessions(), 1);

    // The page's stream, open throughout.
    let frames = Frames2::default();
    let streaming = Arc::new(AtomicBool::new(true));
    let stream = {
        let (ui, frames, streaming) = (ui.clone(), frames.clone(), streaming.clone());
        std::thread::spawn(move || {
            let mut out = frames;
            ui.stream(&mut out, None, "", &|| streaming.load(Ordering::Relaxed));
        })
    };
    wait_until("the stream's first frame", 5.0, || {
        !frames.0.lock().unwrap().is_empty()
    });

    // A burst: on the page at once.
    let db = data.join("sc-watch.db");
    *probe.handle.lock().unwrap() = "EXMPLONE".into();
    let pressed = Instant::now();
    probe.press.store(true, Ordering::Relaxed);
    wait_until("the burst on the stream", 5.0, || {
        frames
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|(_, f)| f.contains("EXMPLONE"))
    });
    let shown = frames
        .0
        .lock()
        .unwrap()
        .iter()
        .find(|(_, f)| f.contains("EXMPLONE"))
        .map(|(t, _)| *t)
        .unwrap();
    let took = shown.duration_since(pressed).as_secs_f64();
    assert!(
        took < 1.5,
        "the page heard about the burst in {took:.2} s - it should not wait for the 2 s look"
    );
    assert!(changes.seen() > 0, "commits were announced");
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM audits"),
        1,
        "a 1/1 read is audited"
    );

    // A setting that needs no restart reaches the running session.
    let out = post(
        &ui,
        "/api/settings",
        json!({"values": {"audit_enabled": false}}),
    );
    assert_eq!(out["ok"], true);
    let polls = probe.polls.load(Ordering::Relaxed);
    wait_until("an idle moment", 5.0, || {
        probe.polls.load(Ordering::Relaxed) > polls + 3
    });
    *probe.handle.lock().unwrap() = "EXMPLTWO".into();
    probe.press.store(true, Ordering::Relaxed);
    wait_until("the second burst", 5.0, || {
        count(
            &db,
            "SELECT COUNT(*) FROM sightings WHERE handle='EXMPLTWO'",
        ) > 0
    });
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM audits"),
        1,
        "audits are off now"
    );
    assert_eq!(sessions(), 1, "and no restart was needed for it");

    // The page's restart button: a new session, same process.
    let out = post(&ui, "/api/restart-watcher", json!({}));
    assert_eq!(out["ok"], true);
    wait_until("the second session", 5.0, || sessions() == 2);
    assert!(
        probe
            .said
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("restart requested from the UI"))
    );
    assert!(
        !data.join("restart.flag").exists(),
        "no flag file in one process"
    );

    // A setting read only at start: a new session too.
    let polls = probe.polls.load(Ordering::Relaxed);
    wait_until("the second session to idle", 5.0, || {
        probe.polls.load(Ordering::Relaxed) > polls + 3
    });
    let out = post(&ui, "/api/settings", json!({"values": {"burst": 2}}));
    // ...which the page says happens by itself (not "restart the watcher").
    assert_eq!(out["one_program"], true);
    wait_until("the third session", 5.0, || sessions() == 3);

    // Ctrl+C.
    app.stop.store(true, Ordering::Relaxed);
    let rc = supervisor.join().unwrap();
    assert_eq!(rc, 0);
    streaming.store(false, Ordering::Relaxed);
    changes.bump();
    stream.join().unwrap();
    let _ = std::fs::remove_dir_all(&base);
}
