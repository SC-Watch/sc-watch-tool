//! Not in the Python: on Linux keyboards are read only by the `input`
//! group, and Maintenance's "keyboard" job sets it up. Its check says where
//! keys stand; doing it calls the system's setup (the desktop's password
//! dialog) once, and says what came of it. A system with nothing to set up
//! has no such job. The system is a stand-in here, so this runs anywhere.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use scw_input::{Device, InputState, KeyAccess};
use scw_rsi::{Clock, FetchError, Reply as Fetched, Transport};
use scw_store::paths::{Mode, Roots};
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

/// The system's keys: where they stand, and what its setup answers.
#[derive(Clone)]
struct Keys {
    access: Option<KeyAccess>,
    answer: Result<(), String>,
    grants: Arc<Mutex<usize>>,
}

impl InputState for Keys {
    fn key_down(&self, _: u32) -> bool {
        false
    }
    fn devices(&self) -> Vec<Device> {
        Vec::new()
    }
    fn buttons_down(&self, _: u32) -> u32 {
        0
    }
    fn key_access(&self) -> Option<KeyAccess> {
        self.access
    }
    fn grant_key_access(&self) -> Result<(), String> {
        *self.grants.lock().unwrap() += 1;
        self.answer.clone()
    }
}

fn page(name: &str, keys: Keys) -> (Arc<Ui>, Roots) {
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
            input: Box::new(move || Box::new(keys.clone())),
            env: Arc::new(|_| None),
            today: Box::new(|| chrono::NaiveDate::from_ymd_opt(2026, 11, 28).unwrap()),
            utc: true,
        },
    );
    (Arc::new(ui), roots)
}

fn job(ui: &Arc<Ui>, apply: bool) -> Value {
    let body = serde_json::to_vec(&json!({"job": "keyboard", "apply": apply})).unwrap();
    let req = Request {
        method: "POST".into(),
        target: "/api/maintenance".into(),
        headers: vec![
            ("Host".into(), "127.0.0.1:8731".into()),
            ("Content-Type".into(), "application/json".into()),
            ("Content-Length".into(), body.len().to_string()),
        ],
        body,
    };
    match ui.handle(&req) {
        Reply::Ok { body, .. } => serde_json::from_slice(&body).unwrap(),
        _ => panic!("not a JSON answer"),
    }
}

fn keys(access: Option<KeyAccess>, answer: Result<(), String>) -> Keys {
    Keys {
        access,
        answer,
        grants: Arc::new(Mutex::new(0)),
    }
}

#[test]
fn keys_are_set_up_once_asked_and_only_where_needed() {
    // Needed: the check offers it (count 1) and changes nothing; doing it
    // runs the setup once.
    let k = keys(Some(KeyAccess::Needed), Ok(()));
    let grants = k.grants.clone();
    let (ui, roots) = page("keys-needed", k);
    let shown = ui.collect(None, "").unwrap()["stats"]
        .get("key_setup")
        .cloned();
    assert_eq!(
        shown,
        cfg!(target_os = "linux").then_some(json!(true)),
        "offered on Linux only"
    );
    let check = job(&ui, false);
    assert_eq!(
        (check["ok"].clone(), check["count"].clone()),
        (json!(true), json!(1)),
        "{check}"
    );
    assert!(
        check["message"].as_str().unwrap().contains("password"),
        "{check}"
    );
    assert_eq!(*grants.lock().unwrap(), 0, "a check sets nothing up");
    let done = job(&ui, true);
    assert_eq!(done["ok"], json!(true), "{done}");
    assert!(
        done["message"].as_str().unwrap().starts_with("done"),
        "{done}"
    );
    assert_eq!(*grants.lock().unwrap(), 1);
    let _ = std::fs::remove_dir_all(roots.data.parent().unwrap());

    // The setup refused (the dialog closed): its words, not ok.
    let k = keys(
        Some(KeyAccess::Needed),
        Err("the password dialog was closed".into()),
    );
    let (ui, roots) = page("keys-refused", k);
    let refused = job(&ui, true);
    assert_eq!(refused["ok"], json!(false));
    assert_eq!(refused["error"], json!("the password dialog was closed"));
    let _ = std::fs::remove_dir_all(roots.data.parent().unwrap());

    // Already readable, or waiting on a new login: nothing to do, nothing run.
    for (state, said) in [
        (KeyAccess::Readable, "already"),
        (KeyAccess::AfterLogin, "log out"),
    ] {
        let k = keys(Some(state), Ok(()));
        let grants = k.grants.clone();
        let (ui, roots) = page("keys-done", k);
        for apply in [false, true] {
            let j = job(&ui, apply);
            assert_eq!(
                (j["ok"].clone(), j["count"].clone()),
                (json!(true), json!(0)),
                "{j}"
            );
            assert!(j["message"].as_str().unwrap().contains(said), "{j}");
        }
        assert_eq!(*grants.lock().unwrap(), 0, "{state:?}: nothing run");
        let _ = std::fs::remove_dir_all(roots.data.parent().unwrap());
    }

    // A system with nothing to set up: no such job.
    let (ui, roots) = page("keys-none", keys(None, Ok(())));
    assert_eq!(job(&ui, true)["ok"], json!(false));
    let _ = std::fs::remove_dir_all(roots.data.parent().unwrap());
}
