//! Not in the Python: where the desktop picks the screen (Linux, on a
//! Wayland desktop), the page says so (stats.screen_share) and its
//! Maintenance job "screen" forgets the desktop's choice - the restore
//! token - and asks the watcher to restart, so the desktop asks again.
//! Anywhere else there is no such job.

use std::sync::Arc;

use serde_json::{Value, json};

use scw_rsi::{Clock, FetchError, Reply as Fetched, Transport};
use scw_store::paths::{Mode, Roots, SCREENCAST_TOKEN};
use scw_store::reputation::Database;
use scw_ui::routes::{Reply, Request};
use scw_ui::{Hooks, Ui};

struct NoNet;

impl Transport for NoNet {
    fn get(&self, url: &str, _: &str, _: f64) -> Result<Fetched, FetchError> {
        panic!("no network: {url}")
    }
}

struct Now;

impl Clock for Now {
    fn time(&self) -> f64 {
        1_790_000_000.0
    }
    fn monotonic(&self) -> f64 {
        1000.0
    }
    fn sleep(&self, _: f64) {}
    fn uniform(&self, a: f64, b: f64) -> f64 {
        (a + b) / 2.0
    }
}

fn page(name: &str, env: &'static [(&'static str, &'static str)]) -> (Arc<Ui>, Roots) {
    let base = std::env::temp_dir().join(format!("scw-ui-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("data")).unwrap();
    drop(Database::open(&base.join("data").join("sc-watch.db")).unwrap());
    let roots = Roots {
        mode: Mode::Chosen,
        program: base.join("program"),
        data: base.join("data"),
    };
    let ui = Ui::new(
        roots.clone(),
        Arc::new(Now),
        Arc::new(NoNet),
        Hooks {
            spawn: Box::new(|job| job()),
            reveal: Box::new(|_| Ok(())),
            speak: Box::new(|_| Ok(())),
            input: Box::new(|| panic!("no keys")),
            env: Arc::new(move |k| {
                env.iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }),
            today: Box::new(|| chrono::NaiveDate::from_ymd_opt(2026, 11, 27).unwrap()),
            utc: true,
        },
    );
    (Arc::new(ui), roots)
}

fn post(ui: &Arc<Ui>, path: &str, body: Value) -> Value {
    let body = serde_json::to_vec(&body).unwrap();
    let req = Request {
        method: "POST".into(),
        target: path.into(),
        headers: vec![
            ("Host".into(), "127.0.0.1:8731".into()),
            ("Content-Type".into(), "application/json".into()),
            ("Content-Length".into(), body.len().to_string()),
        ],
        body,
    };
    match ui.handle(&req) {
        Reply::Ok { body, .. } => serde_json::from_slice(&body).unwrap(),
        _ => panic!("{path}: not a JSON answer"),
    }
}

#[test]
fn the_desktop_is_asked_again_only_where_it_picks_the_screen() {
    let wayland: &'static [(&'static str, &'static str)] = &[("WAYLAND_DISPLAY", "wayland-0")];
    let (ui, roots) = page("screen", wayland);
    let token = roots.data.join(SCREENCAST_TOKEN);
    let flag = roots.data.join("restart.flag");
    std::fs::write(&token, "a-token\n").unwrap();
    let shown = ui.collect(None, "").unwrap()["stats"]
        .get("screen_share")
        .cloned();
    let answer = post(
        &ui,
        "/api/maintenance",
        json!({"job": "screen", "apply": true}),
    );
    if cfg!(target_os = "linux") {
        assert_eq!(shown, Some(json!(true)), "the page offers the job");
        assert_eq!(answer["ok"], json!(true), "{answer}");
        assert!(!token.exists(), "the desktop's choice is forgotten");
        assert!(flag.exists(), "the watcher is asked to restart");
        // nothing to forget: still a restart, still fine
        std::fs::remove_file(&flag).unwrap();
        let again = post(
            &ui,
            "/api/maintenance",
            json!({"job": "screen", "apply": true}),
        );
        assert_eq!(again["ok"], json!(true), "{again}");
        assert!(flag.exists());
    } else {
        assert_eq!(shown, None, "no such job off Linux");
        assert_eq!(answer["ok"], json!(false), "{answer}");
        assert!(token.exists(), "nothing touched");
        assert!(!flag.exists());
    }
    let _ = std::fs::remove_dir_all(roots.data.parent().unwrap());

    // The X11 capture (SCW_CAPTURE=x11) picks the monitor itself: no job.
    let x11: &'static [(&'static str, &'static str)] =
        &[("WAYLAND_DISPLAY", "wayland-0"), ("SCW_CAPTURE", "x11")];
    let (ui, roots) = page("screen-x11", x11);
    assert_eq!(
        ui.collect(None, "").unwrap()["stats"].get("screen_share"),
        None
    );
    let answer = post(
        &ui,
        "/api/maintenance",
        json!({"job": "screen", "apply": true}),
    );
    assert_eq!(answer["ok"], json!(false), "{answer}");
    let _ = std::fs::remove_dir_all(roots.data.parent().unwrap());
}
