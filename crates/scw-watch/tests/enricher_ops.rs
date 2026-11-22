//! The lookup worker against watch.RsiEnricher itself
//! (tests/fixtures/make_enricher_ops.py): the same submissions, the same
//! scripted answers on the same fake clock give the same lookups at the same
//! moments, the same lines, alerts, log events and failure notes, the same
//! retry state, and the same rows and audit pictures.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::NaiveDateTime;
use rusqlite::Connection;
use scw_rsi::{Citizen, Clock, Failure, LookupError, Note, Org, Source};
use scw_store::reputation::Database;
use scw_vision::Image;
use scw_watch::audit::Crops;
use scw_watch::enricher::{Deps, Enricher, Pending};
use scw_watch::sinks::{AlertSink, Console, EventLog, Lookups, Spoken};
use serde_json::{Map, Value, json};

struct FakeClock(Mutex<(f64, f64)>);

impl Clock for FakeClock {
    fn time(&self) -> f64 {
        self.0.lock().unwrap().0
    }
    fn monotonic(&self) -> f64 {
        self.0.lock().unwrap().1
    }
    fn sleep(&self, s: f64) {
        let mut c = self.0.lock().unwrap();
        c.0 += s;
        c.1 += s;
    }
    fn uniform(&self, a: f64, b: f64) -> f64 {
        (a + b) / 2.0
    }
}

#[derive(Default)]
struct Rec {
    said: Vec<String>,
    alerts: Vec<Value>,
    events: Vec<Value>,
    notes: Vec<Value>,
    calls: Vec<Value>,
}

struct Shared {
    rec: Mutex<Rec>,
    script: Mutex<HashMap<String, Vec<Value>>>,
    down: Mutex<f64>,
    clock: Arc<FakeClock>,
}

impl Console for Shared {
    fn say(&self, text: &str) {
        self.rec.lock().unwrap().said.push(text.into());
    }
    fn status(&self, _: &str) {}
}

impl AlertSink for Shared {
    fn event(&self, kind: &str, handle: Option<&str>, flags: &[String], range_km: Option<f64>) {
        self.rec
            .lock()
            .unwrap()
            .alerts
            .push(json!([kind, {"handle": handle, "flags": flags, "range_km": range_km}]));
    }
    fn burst_result(&self, _: &[Spoken]) {}
}

impl EventLog for Shared {
    fn event(&self, kind: &str, fields: &[(&str, Value)]) {
        let m: Map<String, Value> = fields
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        self.rec.lock().unwrap().events.push(json!([kind, m]));
    }
}

fn org(v: &Value) -> Org {
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    Org {
        sid: s("sid"),
        name: s("name"),
        members: v.get("members").and_then(Value::as_i64).unwrap_or(0),
        archetype: s("archetype"),
        commitment: s("commitment"),
        primary_focus: s("primary_focus"),
        secondary_focus: s("secondary_focus"),
        is_main: v.get("is_main").and_then(Value::as_bool).unwrap_or(false),
        rank: s("rank"),
        redacted: false,
    }
}

impl Lookups for Shared {
    fn citizen(&self, handle: &str) -> Result<Citizen, LookupError> {
        let spec = {
            let mut sc = self.script.lock().unwrap();
            let q = sc.get_mut(handle).expect("scripted");
            if q.len() > 1 {
                q.remove(0)
            } else {
                q[0].clone()
            }
        };
        let (wall, mono) = (self.clock.time(), self.clock.monotonic());
        self.rec.lock().unwrap().calls.push(json!([handle, mono]));
        let s = |k: &str| spec.get(k).and_then(Value::as_str).map(String::from);
        if let Some(kind) = s("error") {
            let service: Option<&'static str> = match s("service").as_deref() {
                Some("SENTRY") => Some("SENTRY"),
                Some("starcitizen-api.com") => Some("starcitizen-api.com"),
                _ => None,
            };
            let failure = match kind.as_str() {
                "http" => Failure::Http {
                    code: spec["code"].as_u64().unwrap() as u16,
                    headers: Default::default(),
                },
                "down" => Failure::ServiceDown {
                    until: mono + spec["for"].as_f64().unwrap(),
                },
                "timeout" => Failure::Timeout,
                "nokey" => Failure::NoKey,
                other => panic!("{other}"),
            };
            let mut e = LookupError::new(failure);
            e.service = service;
            return Err(e);
        }
        Ok(Citizen {
            handle: s("handle").unwrap(),
            exists: spec.get("exists").and_then(Value::as_bool).unwrap_or(true),
            moniker: s("moniker").unwrap_or_default(),
            enlisted: s("enlisted").unwrap_or_default(),
            orgs: spec
                .get("orgs")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(org).collect())
                .unwrap_or_default(),
            fetched_at: wall,
            note: Note {
                stale_reason: s("stale_reason"),
                source: (s("source").as_deref() == Some("sentry")).then_some(Source::Sentry),
                awaiting_sentry: spec
                    .get("awaiting_sentry")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                checking: s("checking"),
                retry_in: spec.get("retry_in").and_then(Value::as_f64),
                stored_at: spec
                    .get("stored_days")
                    .and_then(Value::as_f64)
                    .map(|d| wall - d * 86400.0),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    fn down_for(&self) -> f64 {
        *self.down.lock().unwrap()
    }

    fn note_failure(&self, handle: &str, why: &str, attempts: usize, gave_up: bool) {
        self.rec
            .lock()
            .unwrap()
            .notes
            .push(json!([handle, why, attempts, gave_up]));
    }
}

/// The fixture's made-up crop: BGR, h x w.
fn crop(h: usize, w: usize, seed: usize) -> Image<u8> {
    let mut img = Image::new(w, h, 3);
    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                img.data[(y * w + x) * 3 + c] = ((seed * 31 + y * 7 + x * 3 + c * 50) % 256) as u8;
            }
        }
    }
    img
}

fn pixels(path: &Path) -> Value {
    let dec = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).unwrap()));
    let mut r = dec.read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).unwrap();
    let (w, h) = (info.width as usize, info.height as usize);
    json!(
        (0..h)
            .map(|y| (0..w)
                .map(|x| {
                    let p = &buf[(y * w + x) * 3..(y * w + x) * 3 + 3];
                    json!([p[2], p[1], p[0]])
                })
                .collect::<Vec<_>>())
            .collect::<Vec<_>>()
    )
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

fn tables(db: &Path) -> Value {
    let c = Connection::open(db).unwrap();
    let q = |sql: &str| -> Value {
        let mut s = c.prepare(sql).unwrap();
        let n = s.column_count();
        Value::Array(
            s.query_map([], |r| {
                Ok(Value::Array(
                    (0..n)
                        .map(|i| match r.get_ref(i).unwrap() {
                            rusqlite::types::ValueRef::Null => Value::Null,
                            rusqlite::types::ValueRef::Integer(x) => json!(x),
                            rusqlite::types::ValueRef::Real(x) => json!(x),
                            rusqlite::types::ValueRef::Text(x) => json!(String::from_utf8_lossy(x)),
                            rusqlite::types::ValueRef::Blob(_) => json!("<blob>"),
                        })
                        .collect(),
                ))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect(),
        )
    };
    json!({
        "audits": q("SELECT handle, range_km, votes, total, variants, reason, crop_name, crop_range, frame_path FROM audits ORDER BY id"),
        "player_orgs": q("SELECT handle, sid, is_main, source FROM player_orgs ORDER BY handle, sid"),
        "players": q("SELECT handle, moniker, enlisted FROM players ORDER BY handle"),
    })
}

fn state(en: &Enricher) -> Value {
    let sorted = |s: &std::collections::HashSet<String>| {
        let mut v: Vec<&String> = s.iter().collect();
        v.sort();
        json!(v)
    };
    let mut retry: Vec<(f64, String)> = en.retry.iter().map(|r| (r.0, r.2.clone())).collect();
    retry.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut attempts: Vec<(&String, &usize)> = en.attempts.iter().collect();
    attempts.sort();
    let mut alerted: Vec<(&String, Vec<String>)> = en
        .alerted
        .iter()
        .map(|(k, v)| {
            let mut v = v.clone();
            v.sort();
            (k, v)
        })
        .collect();
    alerted.sort();
    json!({
        "seen": sorted(&en.seen),
        "retry": retry.iter().map(|(d, h)| json!([d, h])).collect::<Vec<_>>(),
        "attempts": attempts.into_iter().map(|(k, v)| (k.clone(), json!(v))).collect::<Map<_, _>>(),
        "pending": en.pending.iter().map(|(h, _)| h.clone()).collect::<Vec<_>>(),
        "stale_shown": sorted(&en.stale_shown),
        "check_shown": sorted(&en.check_shown),
        "announced_pause": en.announced_pause,
        "alerted": alerted.into_iter().map(|(k, v)| (k.clone(), json!(v))).collect::<Map<_, _>>(),
    })
}

#[test]
fn enricher_matches_watch_py() {
    let fx: Value = serde_json::from_slice(
        &std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/enricher_ops.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let root = std::env::temp_dir().join(format!("scw-watch-enricher-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let clock = Arc::new(FakeClock(Mutex::new((
        fx["wall0"].as_f64().unwrap(),
        fx["mono0"].as_f64().unwrap(),
    ))));
    let sh = Shared {
        rec: Mutex::new(Rec::default()),
        script: Mutex::new(HashMap::new()),
        down: Mutex::new(0.0),
        clock: clock.clone(),
    };
    let db_path = root.join("sc-watch.db");
    let mut db = Database::open(&db_path).unwrap();
    let c = clock.clone();
    db.clock = Box::new(move || c.time());
    db.utc_dates = true;
    db.flag_org("EXMPLORG", "they camp", "Example Org").unwrap();
    db.add_member_manually("EXMPLORG", "EXMPLGOOD", "seen with them")
        .unwrap();
    let at =
        NaiveDateTime::parse_from_str(fx["at"].as_str().unwrap(), "%Y-%m-%dT%H:%M:%S").unwrap();
    let local_now = move || at;
    let deps = Deps {
        lookups: &sh,
        console: &sh,
        log: &sh,
        alerts: Some(&sh),
        db: Some(&db),
        data_root: &root,
        clock: &*clock,
        local_now: &local_now,
        utc_dates: true,
    };
    let en = RefCell::new(Enricher::new(true));
    let mut bad = Vec::new();

    let run = |limit: f64| {
        let until = clock.monotonic() + limit;
        loop {
            let now = clock.monotonic();
            let next = en.borrow_mut().next_due(now);
            match next {
                Some(h) => en.borrow_mut().process(&h, &deps),
                None => match en.borrow().next_retry_at() {
                    Some(t) if t <= until => clock.sleep(t - now),
                    _ => break,
                },
            }
        }
        let now = clock.monotonic();
        if now < until {
            clock.sleep(until - now);
        }
    };

    for (i, step) in fx["steps"].as_array().unwrap().iter().enumerate() {
        *sh.rec.lock().unwrap() = Rec::default();
        let op = step["op"].as_str().unwrap();
        let a = step["args"].as_array().unwrap();
        match op {
            "script" => {
                sh.script.lock().unwrap().insert(
                    a[0].as_str().unwrap().into(),
                    a[1].as_array().unwrap().clone(),
                );
            }
            "submit" => {
                let kw = &a[1];
                let crops = kw.get("crops").map(|c| {
                    let make = |k: &str| {
                        c.get(k).map(|v| {
                            crop(
                                v[0].as_u64().unwrap() as usize,
                                v[1].as_u64().unwrap() as usize,
                                v[2].as_u64().unwrap() as usize,
                            )
                        })
                    };
                    Crops {
                        name: make("crop_name"),
                        range: make("crop_range"),
                    }
                });
                let ev = crops.map(|crops| Pending {
                    crops,
                    range_km: kw.get("range_km").and_then(Value::as_f64),
                    votes: kw.get("votes").and_then(Value::as_u64).unwrap_or(0) as usize,
                    total: kw.get("total").and_then(Value::as_u64).unwrap_or(0) as usize,
                    variants: kw
                        .get("variants")
                        .and_then(Value::as_array)
                        .map(|v| {
                            v.iter()
                                .map(|p| {
                                    (
                                        p[0].as_str().unwrap().into(),
                                        p[1].as_u64().unwrap() as usize,
                                    )
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                });
                en.borrow_mut().submit(a[0].as_str().unwrap(), ev);
            }
            "run" => run(a[0].as_f64().unwrap()),
            "down" => *sh.down.lock().unwrap() = a[0].as_f64().unwrap(),
            "crowd" => {
                for n in 0..a[0].as_u64().unwrap() as usize {
                    let h = format!("EXMPLCROWD{n:02}");
                    sh.script
                        .lock()
                        .unwrap()
                        .insert(h.clone(), vec![json!({"handle": h, "exists": n != 0})]);
                    en.borrow_mut().submit(
                        &h,
                        Some(Pending {
                            crops: Crops {
                                name: Some(crop(2, 3, 100 + n)),
                                range: None,
                            },
                            range_km: Some(1.0),
                            votes: 1,
                            total: 1,
                            variants: vec![],
                        }),
                    );
                }
            }
            other => panic!("the test does not know {other}"),
        }
        let rec = std::mem::take(&mut *sh.rec.lock().unwrap());
        let t = tables(&db_path);
        let mut pictures = Map::new();
        for row in t["audits"].as_array().unwrap() {
            for rel in row.as_array().unwrap()[6..9]
                .iter()
                .filter_map(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                pictures.insert(rel.into(), pixels(&root.join(rel)));
            }
        }
        let got = json!({
            "mono": clock.monotonic(), "wall": clock.time(), "said": rec.said,
            "alerts": rec.alerts, "events": rec.events, "notes": rec.notes, "calls": rec.calls,
            "state": state(&en.borrow()), "tables": t, "pictures": pictures,
        });
        for k in got.as_object().unwrap().keys() {
            if !same(&got[k], &step[k]) {
                bad.push(format!(
                    "step {i} {op} {}: {k}\n    python {}\n    rust   {}",
                    a.first().map(|v| v.to_string()).unwrap_or_default(),
                    step[k],
                    got[k]
                ));
            }
        }
    }
    drop(db);
    let _ = std::fs::remove_dir_all(&root);
    assert!(bad.is_empty(), "{} differ\n{}", bad.len(), bad.join("\n"));
}
