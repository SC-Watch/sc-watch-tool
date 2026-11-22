//! The watcher's small pieces against watch.py and debuglog.py
//! (tests/fixtures/make_watch_parts.py): the tracker, the announcer, the
//! binding checks and 'waiting for' label, the chat region and focus size
//! checks, the sightings CSV, and the session log's lines.

use std::path::PathBuf;

use chrono::NaiveDateTime;
use scw_input::{Device, InputState};
use scw_vision::names::vote::Resolved;
use scw_watch::{
    Announcer, DebugLog, Scrub, SightingsLog, Tracker, parse_bindings, parse_chat_region,
    parse_focus_size, waiting_label,
};
use serde_json::{Value, json};

/// The fixture's line where the port is deliberately stricter: a Windows
/// home folder inside an event's values is scrubbed.
const DELIBERATE_LOG: (&str, &str) = (
    r#"text="saved to C:\\Users\\Exmpl Person\\data and ~/x""#,
    r#"text="saved to ~\\data and ~/x""#,
);

struct NoDevices;

impl InputState for NoDevices {
    fn key_down(&self, _: u32) -> bool {
        false
    }
    fn devices(&self) -> Vec<Device> {
        vec![]
    }
    fn buttons_down(&self, _: u32) -> u32 {
        0
    }
}

fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

#[test]
fn watch_parts_match_watch_py() {
    let fx: Value = serde_json::from_slice(
        &std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/watch_parts.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let mut bad = Vec::new();

    let mut tr = Tracker::new(180.0, 20.0);
    for s in fx["tracker"].as_array().unwrap() {
        let pts: Vec<(f64, f64)> = s["points"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| (p[0].as_f64().unwrap(), p[1].as_f64().unwrap()))
            .collect();
        let n = tr.update(&pts, s["now"].as_f64().unwrap());
        let tracks = json!(
            tr.tracks
                .iter()
                .map(|t| [t.cx, t.cy, t.last_seen])
                .collect::<Vec<_>>()
        );
        if json!(n) != s["new"] || !same(&tracks, &s["tracks"]) {
            bad.push(format!(
                "tracker at {}: python {} {} rust {n} {tracks}",
                s["now"], s["new"], s["tracks"]
            ));
        }
    }

    let mut an = Announcer::new(300.0);
    for (i, s) in fx["announcer"].as_array().unwrap().iter().enumerate() {
        let (ok, was) = an.should(s["name"].as_str().unwrap(), i as f64);
        let counts: serde_json::Map<String, Value> = an
            .order
            .iter()
            .map(|n| (n.clone(), json!(an.counts[n])))
            .collect();
        let known: Vec<&str> = an.last.iter().map(|(n, _)| n.as_str()).collect();
        let got =
            json!({"name": s["name"], "ok": ok, "was": was, "counts": counts, "known": known});
        if got != *s {
            bad.push(format!("announcer {}: python {s} rust {got}", s["name"]));
        }
    }

    for b in fx["bindings"].as_array().unwrap() {
        let sp: Vec<&str> = b["specs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap())
            .collect();
        let got = match parse_bindings(
            sp[0],
            sp[1],
            sp[2],
            sp[3],
            b["key_mode"].as_bool().unwrap(),
            &NoDevices,
        ) {
            Ok(binds) => {
                let labels: serde_json::Map<String, Value> = binds
                    .iter()
                    .map(|(m, x)| (m.clone(), json!(x.label())))
                    .collect();
                json!({"specs": b["specs"], "key_mode": b["key_mode"], "labels": labels,
                       "waiting": waiting_label(&binds)})
            }
            Err(e) => json!({"specs": b["specs"], "key_mode": b["key_mode"], "error": e.0}),
        };
        if got != *b {
            bad.push(format!(
                "bindings {sp:?}:\n    python {b}\n    rust   {got}"
            ));
        }
    }

    for r in fx["regions"].as_array().unwrap() {
        let spec = r["spec"].as_str().unwrap();
        let got = match parse_chat_region(spec) {
            Ok(v) => json!({"spec": spec, "ok": v}),
            Err(e) => json!({"spec": spec, "error": e.0}),
        };
        if !same(&got, r) {
            bad.push(format!("chat region {spec:?}: python {r} rust {got}"));
        }
    }
    for s in fx["sizes"].as_array().unwrap() {
        let spec = s["spec"].as_str().unwrap();
        let got = match parse_focus_size(spec) {
            Ok((w, h)) => json!({"spec": spec, "ok": [w, h]}),
            Err(e) => json!({"spec": spec, "error": e.0}),
        };
        if got != *s {
            bad.push(format!("focus size {spec:?}: python {s} rust {got}"));
        }
    }

    let dir = std::env::temp_dir().join(format!("scw-watch-parts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let at =
        NaiveDateTime::parse_from_str(fx["at"].as_str().unwrap(), "%Y-%m-%dT%H:%M:%S%.f").unwrap();
    let csv = dir.join("sightings.csv");
    let resolved = |name: &str, km: f64, v: usize, t: usize| Resolved {
        name: name.into(),
        range_km: km,
        votes: v,
        total: t,
        variants: vec![],
    };
    {
        let mut log = SightingsLog::open(&csv).unwrap();
        for r in fx["rows"].as_array().unwrap() {
            let row = resolved(
                r[0].as_str().unwrap(),
                r[1].as_f64().unwrap(),
                r[2].as_u64().unwrap() as usize,
                r[3].as_u64().unwrap() as usize,
            );
            log.write(&row, at).unwrap();
        }
        log.close();
        let mut again = SightingsLog::open(&csv).unwrap();
        again.write(&resolved("EXMPLAGAIN", 3.0, 1, 1), at).unwrap();
    }
    let text = String::from_utf8(std::fs::read(&csv).unwrap()).unwrap();
    if text != fx["sightings"].as_str().unwrap() {
        bad.push(format!(
            "sightings.csv:\n    python {:?}\n    rust   {text:?}",
            fx["sightings"]
        ));
    }

    let scrub = Scrub {
        homes: vec![fx["home"].as_str().unwrap().into(); 2],
        user: Some(fx["user"].as_str().unwrap().into()),
    };
    let log = DebugLog::open(&dir, true, "", scrub, Box::new(move || at));
    for e in fx["events"].as_array().unwrap() {
        let fields: Vec<(&str, Value)> = e[1]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .collect();
        log.event(e[0].as_str().unwrap(), &fields);
    }
    log.note("a plain note");
    log.exception(
        "burst",
        "ValueError",
        &format!("bad frame {}", "x".repeat(400)),
    );
    log.close(&[
        ("minutes", json!(1.5)),
        ("frames", json!(10)),
        ("triggers", json!(2)),
        ("announced", json!(3)),
        ("audio_last_error", json!("-")),
        ("audio_available", json!({"tones": true, "speech": false})),
        ("contacts", json!({"EXMPLONE": 2, "EXMPLTWO": 1})),
    ]);
    let path = log.path.clone().unwrap();
    if path.file_name().unwrap().to_str() != fx["log_name"].as_str() {
        bad.push(format!("log name {path:?}"));
    }
    let lines: Vec<String> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    let want: Vec<String> = fx["log"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            l.as_str()
                .unwrap()
                .replace(DELIBERATE_LOG.0, DELIBERATE_LOG.1)
        })
        .collect();
    if lines != want {
        for (i, (g, w)) in lines.iter().zip(&want).enumerate() {
            if g != w {
                bad.push(format!("log line {i}:\n    python {w}\n    rust   {g}"));
            }
        }
        if lines.len() != want.len() {
            bad.push(format!("log: {} lines, python {}", lines.len(), want.len()));
        }
    }
    drop(log);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(bad.is_empty(), "{} differ\n{}", bad.len(), bad.join("\n"));
}
