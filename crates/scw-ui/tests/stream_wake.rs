//! With in-process change notices the stream waits on them, not on a timer:
//! a commit wakes it at once, and it never sleeps its 0.4 s poll.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use scw_rsi::{Clock, FetchError, Reply, Transport};
use scw_store::db::Changes;
use scw_store::paths::{Mode, Roots};
use scw_store::reputation::Database;
use scw_ui::{Hooks, Ui};

struct NoNet;

impl Transport for NoNet {
    fn get(&self, url: &str, _: &str, _: f64) -> Result<Reply, FetchError> {
        panic!("no network: {url}")
    }
}

/// Real time, but sleeping is a failure: the stream must not poll.
struct NoSleep;

impl Clock for NoSleep {
    fn time(&self) -> f64 {
        scw_rsi::SystemClock.time()
    }
    fn monotonic(&self) -> f64 {
        scw_rsi::SystemClock.monotonic()
    }
    fn sleep(&self, s: f64) {
        panic!("the stream slept {s} s instead of waiting for a change")
    }
    fn uniform(&self, a: f64, b: f64) -> f64 {
        (a + b) / 2.0
    }
}

#[derive(Clone, Default)]
struct Frames(Arc<Mutex<Vec<(Instant, String)>>>);

impl Write for Frames {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        let t = String::from_utf8_lossy(b).into_owned();
        if t.starts_with("data: ") {
            self.0.lock().unwrap().push((Instant::now(), t));
        }
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_commit_wakes_the_stream() {
    let base = std::env::temp_dir().join(format!("scw-ui-wake-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("data")).unwrap();
    let db_path = base.join("data").join("sc-watch.db");
    drop(Database::open(&db_path).unwrap()); // built (and migrated) before
    let roots = Roots {
        mode: Mode::Chosen,
        program: base.join("program"),
        data: base.join("data"),
    };
    let mut ui = Ui::new(
        roots,
        Arc::new(NoSleep),
        Arc::new(NoNet),
        Hooks {
            spawn: Box::new(|job| job()),
            reveal: Box::new(|_| Ok(())),
            speak: Box::new(|_| Ok(())),
            input: Box::new(|| panic!("no keys")),
            env: Arc::new(|_| None),
            today: Box::new(|| chrono::NaiveDate::from_ymd_opt(2026, 11, 22).unwrap()),
            utc: true,
        },
    );
    let changes = Arc::new(Changes::default());
    ui.changes = Some(changes.clone());
    let ui = Arc::new(ui);

    let frames = Frames::default();
    let going = Arc::new(AtomicBool::new(true));
    let stream = {
        let (ui, frames, going) = (ui.clone(), frames.clone(), going.clone());
        std::thread::spawn(move || {
            let mut out = frames;
            ui.stream(&mut out, None, "", &|| going.load(Ordering::Relaxed));
        })
    };
    let wait = |what: &str, ok: &dyn Fn() -> bool| {
        let until = Instant::now() + Duration::from_secs(5);
        while !ok() {
            assert!(Instant::now() < until, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    wait("the first frame", &|| !frames.0.lock().unwrap().is_empty());
    std::thread::sleep(Duration::from_millis(200)); // the stream is waiting now

    // A commit from another connection in this process, announced.
    let mut db = Database::open(&db_path).unwrap();
    scw_store::db::watch(&mut db.conn, changes.clone());
    let at = Instant::now();
    db.record_sighting("EXMPLONE", Some(1.0), 2, 2).unwrap();
    wait("the sighting on the stream", &|| {
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
    let took = shown.duration_since(at).as_secs_f64();
    assert!(
        took < 0.5,
        "shown {took:.2} s after the commit - it should not wait for the {} s look",
        scw_ui::stream::OUTSIDE
    );
    // Reading the page commits nothing of its own: no frame without a change.
    let n = frames.0.lock().unwrap().len();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        frames.0.lock().unwrap().len(),
        n,
        "no frames without changes"
    );

    going.store(false, Ordering::Relaxed);
    changes.bump();
    stream.join().unwrap();
    let _ = std::fs::remove_dir_all(&base);
}

/// The mining overlay's state (the board the watcher in the same process
/// writes): absent from the page until there is one, then on the stream at
/// once when it changes, and gone again when it is taken down.
#[test]
fn the_board_shows_on_the_stream() {
    let base = std::env::temp_dir().join(format!("scw-ui-board-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("data")).unwrap();
    drop(Database::open(&base.join("data").join("sc-watch.db")).unwrap());
    let roots = Roots {
        mode: Mode::Chosen,
        program: base.join("program"),
        data: base.join("data"),
    };
    let mut ui = Ui::new(
        roots,
        Arc::new(NoSleep),
        Arc::new(NoNet),
        Hooks {
            spawn: Box::new(|job| job()),
            reveal: Box::new(|_| Ok(())),
            speak: Box::new(|_| Ok(())),
            input: Box::new(|| panic!("no keys")),
            env: Arc::new(|_| None),
            today: Box::new(|| chrono::NaiveDate::from_ymd_opt(2026, 11, 22).unwrap()),
            utc: true,
        },
    );
    let changes = Arc::new(Changes::default());
    let board = Arc::new(scw_store::board::Board::new(Some(changes.clone())));
    ui.changes = Some(changes.clone());
    ui.board = Some(board.clone());
    let ui = Arc::new(ui);
    let mining = |frame: &str| -> serde_json::Value {
        let v: serde_json::Value =
            serde_json::from_str(frame.trim_start_matches("data: ").trim()).unwrap();
        v["stats"]
            .get("mining")
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    };

    let frames = Frames::default();
    let going = Arc::new(AtomicBool::new(true));
    let stream = {
        let (ui, frames, going) = (ui.clone(), frames.clone(), going.clone());
        std::thread::spawn(move || {
            let mut out = frames;
            ui.stream(&mut out, None, "", &|| going.load(Ordering::Relaxed));
        })
    };
    let wait = |what: &str, ok: &dyn Fn() -> bool| {
        let until = Instant::now() + Duration::from_secs(5);
        while !ok() {
            assert!(Instant::now() < until, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    wait("the first frame", &|| !frames.0.lock().unwrap().is_empty());
    assert!(
        mining(&frames.0.lock().unwrap()[0].1).is_null(),
        "no mining state before the watcher gives one"
    );
    std::thread::sleep(Duration::from_millis(200));

    let state = serde_json::json!({"state": "showing", "label": "EXMPL Rock 2x", "held": false});
    let at = Instant::now();
    board.set(Some(state.clone()));
    wait("the label on the stream", &|| {
        frames
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|(_, f)| mining(f) == state)
    });
    let shown = frames
        .0
        .lock()
        .unwrap()
        .iter()
        .find(|(_, f)| mining(f) == state)
        .map(|(t, _)| *t)
        .unwrap();
    assert!(shown.duration_since(at).as_secs_f64() < 0.5, "not at once");
    // The same state again is no change: no frame.
    let n = frames.0.lock().unwrap().len();
    board.set(Some(state.clone()));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(frames.0.lock().unwrap().len(), n, "a frame for no change");
    // Taken down (overlay off, session over): gone from the page.
    board.set(None);
    wait("the state gone", &|| {
        frames.0.lock().unwrap().len() > n
            && mining(&frames.0.lock().unwrap().last().unwrap().1).is_null()
    });

    going.store(false, Ordering::Relaxed);
    changes.bump();
    stream.join().unwrap();
    let _ = std::fs::remove_dir_all(&base);
}
