//! The alert player against audio.Alerts' own decisions
//! (tests/fixtures/make_alerts.py): for 8 configurations x 12 bursts, the
//! same tones and the same spoken lines, in the same order.

use scw_audio::phrases::{Facts, Reason, ReasonKind};
use scw_audio::{Alerts, AudioConfig, TonePlayer, Voice};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

type Log = Arc<Mutex<Vec<(String, String)>>>;

struct RecVoice(Log);
impl Voice for RecVoice {
    fn speak(&mut self, text: &str) -> Result<(), String> {
        self.0.lock().unwrap().push(("speak".into(), text.into()));
        Ok(())
    }
}

struct RecTones(Log);
impl TonePlayer for RecTones {
    fn play(&mut self, wav: &Path) -> Result<(), String> {
        let stem = wav.file_stem().unwrap().to_string_lossy().into_owned();
        self.0.lock().unwrap().push(("tone".into(), stem));
        Ok(())
    }
}

fn reason(v: &Value) -> Reason {
    let kind = match v["kind"].as_str().unwrap_or("") {
        "report" => ReasonKind::Report,
        "shared" => ReasonKind::Shared,
        "org" => ReasonKind::Org,
        "former" => ReasonKind::Former,
        "watchlist" => ReasonKind::Watchlist,
        _ => ReasonKind::Other,
    };
    let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    Reason {
        kind,
        label: text("label"),
        count: v.get("count").and_then(Value::as_i64),
        source: text("source"),
        org: text("org"),
    }
}

fn facts(v: &Value) -> Facts {
    let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    Facts {
        handle: text("handle"),
        range_km: v.get("range_km").and_then(Value::as_f64),
        flags: v
            .get("flags")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|f| f.as_str().unwrap().to_string()).collect())
            .unwrap_or_default(),
        said: v
            .get("said")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(reason).collect())
            .unwrap_or_default(),
        reason: text("reason"),
        ..Default::default()
    }
}

#[test]
fn decisions_match_audio_py() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/alerts.json");
    let cases: Vec<Value> = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let root = std::env::temp_dir().join(format!("scw-audio-alerts-{}", std::process::id()));
    let mut bad = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        let dir = root.join(i.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        // The configuration, through audio.json as the Python's was.
        let cfg_file = dir.join("given.json");
        std::fs::write(&cfg_file, serde_json::to_vec(&case["config"]).unwrap()).unwrap();
        let cfg = AudioConfig::load(&cfg_file);
        let log: Log = Arc::default();
        let (l1, l2) = (log.clone(), log.clone());
        let alerts = Alerts::new(
            cfg,
            &dir.join("audio.json"), // absent, as the Python's was: no re-read
            &dir.join("tones"),
            Some(Box::new(RecTones(l1))),
            Some(Box::new(move |_| {
                Ok(Box::new(RecVoice(l2.clone())) as Box<dyn Voice>)
            })),
        );
        let contacts: Vec<Facts> = case["contacts"]
            .as_array()
            .unwrap()
            .iter()
            .map(facts)
            .collect();
        alerts.burst_result(&contacts);
        alerts.event("trigger", Facts::default(), false);
        alerts.say("a line as it is");
        alerts.close();
        let want: Vec<(String, String)> = case["log"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| (e[0].as_str().unwrap().into(), e[1].as_str().unwrap().into()))
            .collect();
        let got = log.lock().unwrap().clone();
        if want != got {
            bad.push(format!(
                "{}:\n    python {want:?}\n    rust   {got:?}",
                case["name"]
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        bad.is_empty(),
        "{} of {} differ\n{}",
        bad.len(),
        cases.len(),
        bad.join("\n")
    );
}
