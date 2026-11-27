//! The session against watch.Session itself
//! (tests/fixtures/make_session_ops.py): the same frames, the same scripted
//! reads and key presses on the same fake clock give the same lines, status
//! changes, alerts, log events, lookups, detector calls, sightings.csv,
//! database rows, audit pictures, summary and exit code - and the clock ends
//! at the same moment to the last bit, so every wait loop turned the same
//! number of times. Chat reads play the lines scripted for each frame
//! (chat.read_located faked on both sides).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, NaiveDateTime};
use rusqlite::Connection;
use scw_input::{Device, InputState};
use scw_ocr::chat::{ChatLine, Place};
use scw_rsi::Clock;
use scw_store::housekeeping::{Policy, run_now};
use scw_store::reputation::Database;
use scw_vision::names::NameKind;
use scw_vision::pipeline::Contact;
use scw_vision::{Image, Rect};
use scw_watch::enricher::Pending;
use scw_watch::session::{ChatReader, Detector, FlagFile, Frames, Keys, LookupQueue};
use scw_watch::sinks::{AlertSink, AudioHealth, Console, EventLog, Spoken};
use scw_watch::{Mode, Parts, Session, SessionOptions, SightingsLog, Stop};
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
        if s > 0.0 {
            c.0 += s;
            c.1 += s;
        }
        assert!(c.1 <= 1000.0 + 600.0, "runaway scenario: ten fake minutes");
    }
    fn uniform(&self, a: f64, b: f64) -> f64 {
        (a + b) / 2.0
    }
}

#[derive(Default)]
struct Rec {
    said: Vec<String>,
    status: Vec<String>,
    printed: String,
    alerts: Vec<Value>,
    events: Vec<Value>,
    submits: Vec<Value>,
    detects: Vec<Value>,
    grabs: usize,
    released: usize,
    summary: Option<Value>,
    stop_polls: usize,
    chats: Vec<Value>,
}

type R = Arc<Mutex<Rec>>;

struct Con(R);

impl Console for Con {
    fn say(&self, text: &str) {
        self.0.lock().unwrap().said.push(text.into());
    }
    fn status(&self, text: &str) {
        let mut r = self.0.lock().unwrap();
        if r.status.last().map(String::as_str) != Some(text) {
            r.status.push(text.into());
        }
    }
    fn print(&self, text: &str) {
        let mut r = self.0.lock().unwrap();
        r.printed.push_str(text);
        r.printed.push('\n');
    }
    fn eprint(&self, text: &str) {
        self.print(text);
    }
}

struct Alerts(R, AudioHealth);

impl AlertSink for Alerts {
    fn event(&self, kind: &str, handle: Option<&str>, flags: &[String], range_km: Option<f64>) {
        let mut kw = Map::new();
        if let Some(h) = handle {
            kw.insert("handle".into(), json!(h));
        }
        if !flags.is_empty() {
            kw.insert("flags".into(), json!(flags));
        }
        if let Some(k) = range_km {
            kw.insert("range_km".into(), json!(k));
        }
        self.0.lock().unwrap().alerts.push(json!([kind, kw]));
    }
    fn burst_result(&self, contacts: &[Spoken]) {
        let p: Vec<Value> = contacts
            .iter()
            .map(|s| {
                json!({"handle": s.handle, "range_km": s.range_km, "flags": s.flags,
                       "reason": s.reason, "said": s.said})
            })
            .collect();
        self.0.lock().unwrap().alerts.push(json!(["burst", p]));
    }
    fn close(&self) {
        self.0.lock().unwrap().alerts.push(json!(["close"]));
    }
    fn health(&self) -> AudioHealth {
        self.1.clone()
    }
}

struct Log(R);

impl EventLog for Log {
    fn event(&self, kind: &str, fields: &[(&str, Value)]) {
        let m: Map<String, Value> = fields
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        self.0.lock().unwrap().events.push(json!([kind, m]));
    }
    fn close(&self, summary: &[(&str, Value)]) {
        let m: Map<String, Value> = summary
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        self.0.lock().unwrap().summary = Some(Value::Object(m));
    }
}

/// The lookup queue: records what is submitted; at shutdown, `queued`
/// lookups are waiting, one fewer every `every` seconds after the first look.
struct Queue {
    rec: R,
    queued: Option<usize>,
    every: f64,
    t0: Mutex<Option<f64>>,
    clock: Arc<FakeClock>,
}

impl LookupQueue for Queue {
    fn submit(&self, handle: &str, p: Option<Pending>) {
        let (crops, km, votes, total, variants) = match &p {
            Some(p) => {
                // each crop as [key, height, width, sum of its bytes]
                let held: Vec<Value> =
                    [("crop_name", &p.crops.name), ("crop_range", &p.crops.range)]
                        .into_iter()
                        .filter_map(|(k, img)| {
                            img.as_ref().map(|i| {
                                let sum: u64 = i.data.iter().map(|&b| b as u64).sum();
                                json!([k, i.height, i.width, sum])
                            })
                        })
                        .collect();
                let crops = if held.is_empty() {
                    Value::Null
                } else {
                    Value::Array(held)
                };
                let v: Vec<Value> = p.variants.iter().map(|(n, c)| json!([n, c])).collect();
                (crops, json!(p.range_km), p.votes, p.total, v)
            }
            None => (Value::Null, Value::Null, 0, 0, vec![]),
        };
        self.rec
            .lock()
            .unwrap()
            .submits
            .push(json!([handle, crops, km, votes, total, variants]));
    }
    fn waiting(&self) -> Option<usize> {
        let n0 = self.queued?;
        let now = self.clock.monotonic();
        let t0 = *self.t0.lock().unwrap().get_or_insert(now);
        Some(n0.saturating_sub(((now - t0) / self.every) as usize))
    }
    fn stop(&mut self) {}
}

fn frame(i: usize, w: usize, h: usize) -> Image<u8> {
    let mut f = Image::new(w, h, 3);
    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                f.data[(y * w + x) * 3 + c] = ((i * 40 + y * 3 + x + c * 7) % 256) as u8;
            }
        }
    }
    f.data[0] = i as u8;
    f
}

struct Src {
    ids: Vec<usize>,
    looping: bool,
    i: usize,
    done: bool,
    rec: R,
    restart_at: Option<usize>,
    flag: PathBuf,
    size: (usize, usize),
}

impl Frames for Src {
    fn grab(&mut self) -> Option<Image<u8>> {
        if self.i >= self.ids.len() && !self.looping {
            self.done = true;
            return None;
        }
        let n = self.ids[self.i % self.ids.len()];
        self.i += 1;
        let grabs = {
            let mut r = self.rec.lock().unwrap();
            r.grabs += 1;
            r.grabs
        };
        if self.restart_at == Some(grabs) {
            std::fs::write(&self.flag, b"").unwrap();
        }
        Some(frame(n, self.size.0, self.size.1))
    }
    fn exhausted(&self) -> bool {
        self.done
    }
    fn release(&mut self) {
        self.rec.lock().unwrap().released += 1;
    }
}

fn kind(s: &str) -> NameKind {
    match s {
        "player" => NameKind::Player,
        "ship_serial" => NameKind::ShipSerial,
        "npc_crew" => NameKind::NpcCrew,
        "unknown_contact" => NameKind::UnknownContact,
        "world_label" => NameKind::WorldLabel,
        "unreadable" => NameKind::Unreadable,
        other => panic!("{other}"),
    }
}

fn rect(v: &Value) -> Rect {
    let n = |i: usize| v[i].as_i64().unwrap() as i32;
    Rect::new(n(0), n(1), n(2), n(3))
}

struct Script {
    contacts: Value,
    scan: Value,
    rec: R,
}

/// chat.read_located, scripted: the frame's lines and place, or a failure.
struct ChatScript {
    chat: Value,
    region: Option<[f64; 4]>,
    rec: R,
}

impl ChatReader for ChatScript {
    fn read(&mut self, frame: &Image<u8>) -> Result<(Vec<ChatLine>, Place), String> {
        let i = frame.data[0] as usize;
        self.rec
            .lock()
            .unwrap()
            .chats
            .push(json!([i, self.region.map(|r| r.to_vec())]));
        match &self.chat[i.to_string()] {
            Value::String(s) if s == "error" => Err("RuntimeError: the OCR fell over".into()),
            Value::Array(a) => {
                let lines = a[0]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|l| ChatLine {
                        handle: l[0].as_str().unwrap().into(),
                        text: l[1].as_str().unwrap().into(),
                        conf: l[2].as_f64().unwrap(),
                    })
                    .collect();
                let place = match a[1].as_str().unwrap() {
                    "helmet on" => Place::HelmetOn,
                    "helmet off" => Place::HelmetOff,
                    "your region" => Place::Set,
                    _ => Place::Both,
                };
                Ok((lines, place))
            }
            _ => Ok((
                Vec::new(),
                if self.region.is_some() {
                    Place::Set
                } else {
                    Place::Both
                },
            )),
        }
    }
}

impl Detector for Script {
    fn detect(&mut self, frame: &Image<u8>, mode: Mode) -> Vec<Contact> {
        let i = frame.data[0] as usize;
        self.rec
            .lock()
            .unwrap()
            .detects
            .push(json!([i, mode.as_str()]));
        let list = self.contacts[i.to_string()]
            .as_array()
            .cloned()
            .unwrap_or_default();
        list.iter()
            .map(|c| Contact {
                name: c["name"].as_str().map(String::from),
                kind: kind(c["kind"].as_str().unwrap()),
                range_km: c["km"].as_f64(),
                name_box: rect(&c["nb"]),
                range_box: rect(&c["rb"]),
            })
            .collect()
    }
    fn scan(&mut self, frame: &Image<u8>) -> Vec<Rect> {
        let i = frame.data[0] as usize;
        self.scan[i.to_string()]
            .as_array()
            .map(|l| l.iter().map(rect).collect())
            .unwrap_or_default()
    }
}

struct ScriptKeys {
    modes: Vec<Mode>,
    specs: Vec<String>,
    tokens: Vec<usize>,
    press: Value,
    fired: HashSet<(String, u64)>,
    stop_at: Option<usize>,
    linger: usize,
    label: String,
    rec: R,
}

impl Keys for ScriptKeys {
    fn modes(&self) -> Vec<Mode> {
        self.modes.clone()
    }
    fn pressed(&mut self, i: usize) -> Result<bool, Stop> {
        let n = {
            let mut r = self.rec.lock().unwrap();
            if let Some(at) = self.stop_at
                && r.grabs >= at
            {
                r.stop_polls += 1;
                if r.stop_polls > self.linger {
                    return Err(Stop);
                }
            }
            r.grabs
        };
        let spec = &self.specs[i];
        for at in self.press[spec].as_array().cloned().unwrap_or_default() {
            let at = at.as_u64().unwrap();
            if at == n as u64 && self.fired.insert((spec.clone(), at)) {
                return Ok(true);
            }
        }
        Ok(false)
    }
    fn drain(&mut self) {}
    fn partial(&mut self, i: usize) -> (usize, usize, Vec<String>) {
        (0, self.tokens[i], vec![])
    }
    fn waiting_label(&self) -> String {
        self.label.clone()
    }
}

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

fn rows(c: &Connection, sql: &str) -> Value {
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
                        rusqlite::types::ValueRef::Text(x) => {
                            json!(String::from_utf8_lossy(x))
                        }
                        rusqlite::types::ValueRef::Blob(_) => json!("<blob>"),
                    })
                    .collect(),
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect(),
    )
}

fn tables(path: &Path) -> Value {
    if !path.exists() {
        return Value::Null;
    }
    let c = Connection::open(path).unwrap();
    json!({
        "players": rows(&c, "SELECT handle, sightings FROM players ORDER BY handle"),
        "sightings": rows(&c, "SELECT handle, range_km, votes, total FROM sightings ORDER BY id"),
        "audits": rows(&c, "SELECT handle, range_km, votes, total, variants, reason, crop_name, \
                            crop_range, frame_path FROM audits ORDER BY id"),
    })
}

/// A PNG as [[[b, g, r], ...], ...], as imgio.imread(...).tolist().
fn png_pixels(path: &Path) -> Value {
    let dec = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).unwrap()));
    let mut r = dec.read_info().unwrap();
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgb);
    let (w, h) = (info.width as usize, info.height as usize);
    Value::Array(
        (0..h)
            .map(|y| {
                Value::Array(
                    (0..w)
                        .map(|x| {
                            let p = &buf[(y * w + x) * 3..(y * w + x) * 3 + 3];
                            json!([p[2], p[1], p[0]])
                        })
                        .collect(),
                )
            })
            .collect(),
    )
}

/// A JPEG's [height, width], from its start-of-frame marker.
fn jpeg_size(path: &Path) -> Value {
    let b = std::fs::read(path).unwrap();
    let mut i = 2;
    while i + 9 < b.len() {
        assert_eq!(b[i], 0xFF);
        let m = b[i + 1];
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        if (0xC0..=0xC2).contains(&m) {
            let h = u16::from_be_bytes([b[i + 5], b[i + 6]]);
            let w = u16::from_be_bytes([b[i + 7], b[i + 8]]);
            return json!([h, w]);
        }
        i += 2 + len;
    }
    panic!("no frame header in {}", path.display());
}

fn options(a: &Value, cfg: &Value) -> SessionOptions {
    let f = |k: &str| a[k].as_f64().unwrap();
    let u = |k: &str| a[k].as_u64().unwrap() as usize;
    let b = |k: &str| a[k].as_bool().unwrap();
    SessionOptions {
        key_mode: a["trigger"] == "key" && a["replay"].is_null(),
        replay: !a["replay"].is_null(),
        burst: u("burst"),
        burst_spacing: f("burst_spacing"),
        ping_delay: f("ping_delay"),
        queue_max: u("queue_max"),
        min_votes: u("min_votes"),
        rsi_min_votes: u("rsi_min_votes"),
        verbose: b("verbose"),
        debug: b("debug"),
        audit: b("audit"),
        dual_on_read: b("dual_on_read"),
        focus_always: b("focus_always"),
        hz: f("hz"),
        auto_read_delay: f("auto_read_delay"),
        seconds: f("seconds"),
        radius: f("radius"),
        expire: f("expire"),
        cooldown: f("cooldown"),
        audit_keep_frame: cfg["audit_keep_frame"].as_bool().unwrap(),
        audit_frame_quality: cfg["audit_frame_quality"].as_u64().unwrap() as u8,
        housekeeping_enabled: cfg["housekeeping_enabled"].as_bool().unwrap(),
        housekeeping_every_min: cfg["housekeeping_every_min"].as_f64().unwrap(),
        chat_region: None,
    }
}

fn utc(wall: f64) -> NaiveDateTime {
    let secs = wall.floor();
    DateTime::from_timestamp(secs as i64, ((wall - secs) * 1e9) as u32)
        .unwrap()
        .naive_utc()
}

#[test]
fn session_matches_watch_py() {
    let fx: Value = serde_json::from_slice(
        &std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/session_ops.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let size = (
        fx["w"].as_u64().unwrap() as usize,
        fx["h"].as_u64().unwrap() as usize,
    );
    let mut bad: Vec<String> = Vec::new();
    for (n, s) in fx["scenarios"].as_array().unwrap().iter().enumerate() {
        let name = s["name"].as_str().unwrap();
        let sc = &s["scenario"];
        let root =
            std::env::temp_dir().join(format!("scw-watch-session-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let clock = Arc::new(FakeClock(Mutex::new((1_790_000_000.0, 1000.0))));
        let rec: R = Arc::new(Mutex::new(Rec::default()));
        let mut opts = options(&s["args"], &s["cfg"]);
        opts.chat_region =
            scw_watch::parse_chat_region(s["chat_region"].as_str().unwrap_or("auto")).unwrap();

        let db_path = root.join("sc-watch.db");
        let db = (!sc["no_db"].as_bool().unwrap_or(false)).then(|| {
            let mut db = Database::open(&db_path).unwrap();
            let c = clock.clone();
            db.clock = Box::new(move || c.time());
            db.utc_dates = true;
            db.add_report(
                "EXMPLKNOWN",
                "unprovoked_attack",
                "rammed us",
                "",
                "self",
                false,
            )
            .unwrap();
            db.add_report("EXMPLKNOWN", "camping", "", "", "self", false)
                .unwrap();
            db.add_ignored("EXMPLIGNORE", "test").unwrap();
            if let Some(m) = sc["seen_before"].as_object() {
                for (h, k) in m {
                    for _ in 0..k.as_u64().unwrap() {
                        db.record_sighting(h, Some(1.0), 1, 1).unwrap();
                    }
                }
            }
            db
        });

        let (modes, specs, tokens, label) = if opts.key_mode {
            // the scenario's chat key: its last --chat-key
            let argv: Vec<&str> = s["argv"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            let chat_key = argv
                .windows(2)
                .filter(|w| w[0] == "--chat-key")
                .map(|w| w[1])
                .next_back()
                .unwrap_or("off");
            let binds =
                scw_watch::parse_bindings("0", "9", "7", chat_key, true, &NoDevices).unwrap();
            (
                binds.iter().map(|(m, _)| Mode::parse(m).unwrap()).collect(),
                binds.iter().map(|(_, b)| b.spec.clone()).collect(),
                binds.iter().map(|(_, b)| b.tokens.len()).collect(),
                scw_watch::waiting_label(&binds),
            )
        } else {
            (vec![], vec![], vec![], String::new())
        };
        let health = sc.get("health").map(|h| AudioHealth {
            failures: h["failures"].as_u64().unwrap_or(0),
            repairs: h["repairs"].as_u64().unwrap_or(0),
            last_error: h["last_error"].as_str().unwrap_or("").into(),
            available: json!({"tones": true, "speech": h["speech"].as_bool().unwrap_or(true)}),
            expired: h["expired"].as_u64().unwrap_or(0),
            max_alert_age: 20.0,
        });
        let alerts: Option<Arc<dyn AlertSink>> = (!sc["no_alerts"].as_bool().unwrap_or(false))
            .then(|| {
                Arc::new(Alerts(
                    rec.clone(),
                    health.unwrap_or(AudioHealth {
                        available: json!({"tones": true, "speech": true}),
                        ..AudioHealth::default()
                    }),
                )) as Arc<dyn AlertSink>
            });
        let flag = root.join("restart.flag");
        let hk_root = root.clone();
        let c2 = clock.clone();
        let parts = Parts {
            frames: Box::new(Src {
                ids: sc["ids"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| v.as_u64().unwrap() as usize)
                    .collect(),
                looping: sc["loop"].as_bool().unwrap(),
                i: 0,
                done: false,
                rec: rec.clone(),
                restart_at: sc["restart_at"].as_u64().map(|v| v as usize),
                flag: flag.clone(),
                size,
            }),
            detector: Box::new(Script {
                contacts: fx["contacts"].clone(),
                scan: sc.get("scan").cloned().unwrap_or(json!({})),
                rec: rec.clone(),
            }),
            chat: Some(Box::new(ChatScript {
                chat: sc.get("chat").cloned().unwrap_or(json!({})),
                region: opts.chat_region,
                rec: rec.clone(),
            })),
            keys: Box::new(ScriptKeys {
                modes,
                specs,
                tokens,
                press: sc.get("press").cloned().unwrap_or(json!({})),
                fired: HashSet::new(),
                stop_at: sc["stop_at"].as_u64().map(|v| v as usize),
                linger: sc["linger"].as_u64().unwrap_or(0) as usize,
                label,
                rec: rec.clone(),
            }),
            console: Arc::new(Con(rec.clone())),
            alerts,
            log: Arc::new(Log(rec.clone())),
            db,
            lookups: Box::new(Queue {
                rec: rec.clone(),
                queued: sc["queued"].as_u64().map(|v| v as usize),
                every: sc["drain_every"].as_f64().unwrap_or(0.5),
                t0: Mutex::new(None),
                clock: clock.clone(),
            }),
            sightings: SightingsLog::open(&root.join("sightings.csv")).unwrap(),
            clock: clock.clone(),
            local_now: Box::new(move || utc(c2.time())),
            data_root: root.clone(),
            restart: Box::new(FlagFile {
                path: flag.clone(),
                relaunch: Box::new(|| "OSError: simulated execv failure".into()),
            }),
            housekeeper: Box::new(move |db, now| {
                let policy = Policy::load(&hk_root.join("housekeeping.json"), None);
                run_now(&policy, &hk_root, db, now, true)
            }),
            stop: Arc::new(AtomicBool::new(false)),
        };
        let mut session = Session::new(opts, parts);
        let rc = session.run();
        let counters = json!([session.frames, session.triggers, session.announced]);
        drop(session);

        let t = tables(&db_path);
        let mut pictures = Map::new();
        if let Some(audits) = t["audits"].as_array() {
            for a in audits {
                for rel in a.as_array().unwrap()[6..9].iter().filter_map(Value::as_str) {
                    if rel.is_empty() {
                        continue;
                    }
                    let p = root.join(rel);
                    let v = if rel.ends_with(".png") {
                        png_pixels(&p)
                    } else {
                        jpeg_size(&p)
                    };
                    pictures.insert(rel.into(), v);
                }
            }
        }
        let r = rec.lock().unwrap();
        let (wall, mono) = (clock.time(), clock.monotonic());
        let csv = std::fs::read_to_string(root.join("sightings.csv"))
            .unwrap()
            .replace("\r\n", "\n");
        let printed: Vec<Value> = r.printed.lines().map(|l| json!(l)).collect();
        let got: HashMap<&str, Value> = HashMap::from([
            ("rc", json!(rc)),
            ("said", json!(r.said)),
            ("status", json!(r.status)),
            ("printed", Value::Array(printed)),
            ("alerts", Value::Array(r.alerts.clone())),
            ("events", Value::Array(r.events.clone())),
            ("submits", Value::Array(r.submits.clone())),
            ("detects", Value::Array(r.detects.clone())),
            ("grabs", json!(r.grabs)),
            ("chats", Value::Array(r.chats.clone())),
            ("released", json!(r.released)),
            ("summary", r.summary.clone().unwrap_or(Value::Null)),
            ("counters", counters),
            ("tables", t),
            ("pictures", Value::Object(pictures)),
            ("sightings", json!(csv)),
            ("restart_flag_left", json!(flag.exists())),
            ("end", json!([wall, mono])),
        ]);
        let mut keys: Vec<&str> = got.keys().copied().collect();
        keys.sort();
        for k in keys {
            let (want, have) = (&s[k], &got[k]);
            if !same(want, have) {
                let show = |v: &Value| {
                    let t = serde_json::to_string(v).unwrap();
                    t.chars().take(1500).collect::<String>()
                };
                bad.push(format!(
                    "{name}: {k}\n   want {}\n   got  {}",
                    show(want),
                    show(have)
                ));
            }
        }
        drop(r);
        let _ = std::fs::remove_dir_all(&root);
    }
    assert!(
        bad.is_empty(),
        "{} differences:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// This port only (the Python's auto loop listens to no key): with the auto
/// trigger the chat key still reads the chat, on the frame just grabbed,
/// counted as a trigger - and the always-on scan carries on.
#[test]
fn the_chat_key_reads_with_the_auto_trigger() {
    let root = std::env::temp_dir().join(format!("scw-watch-autochat-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let clock = Arc::new(FakeClock(Mutex::new((1_790_000_000.0, 1000.0))));
    let rec: R = Arc::new(Mutex::new(Rec::default()));
    let opts = SessionOptions {
        key_mode: false,
        replay: true,
        burst: 1,
        burst_spacing: 0.05,
        ping_delay: 0.0,
        queue_max: 2,
        min_votes: 1,
        rsi_min_votes: 2,
        verbose: false,
        debug: false,
        audit: false,
        dual_on_read: false,
        focus_always: false,
        hz: 5.0,
        auto_read_delay: 0.0,
        seconds: 0.0,
        radius: 40.0,
        expire: 5.0,
        cooldown: 300.0,
        audit_keep_frame: true,
        audit_frame_quality: 90,
        housekeeping_enabled: false,
        housekeeping_every_min: 30.0,
        chat_region: None,
    };
    let c2 = clock.clone();
    let parts = Parts {
        frames: Box::new(Src {
            ids: vec![11, 12, 13],
            looping: false,
            i: 0,
            done: false,
            rec: rec.clone(),
            restart_at: None,
            flag: root.join("restart.flag"),
            size: (64, 40),
        }),
        detector: Box::new(Script {
            contacts: json!({}),
            scan: json!({}),
            rec: rec.clone(),
        }),
        chat: Some(Box::new(ChatScript {
            chat: json!({"12": [[["ExmplOne", "[GLOBAL] ExmplOne: o7", 0.9]], "helmet on"]}),
            region: None,
            rec: rec.clone(),
        })),
        // pressed when the second frame has been grabbed
        keys: Box::new(ScriptKeys {
            modes: vec![Mode::Chat],
            specs: vec!["8".into()],
            tokens: vec![1],
            press: json!({"8": [2]}),
            fired: HashSet::new(),
            stop_at: None,
            linger: 0,
            label: "8".into(),
            rec: rec.clone(),
        }),
        console: Arc::new(Con(rec.clone())),
        alerts: Some(Arc::new(Alerts(
            rec.clone(),
            AudioHealth {
                available: json!({"tones": true, "speech": true}),
                ..AudioHealth::default()
            },
        ))),
        log: Arc::new(Log(rec.clone())),
        db: None,
        lookups: Box::new(Queue {
            rec: rec.clone(),
            queued: None,
            every: 0.5,
            t0: Mutex::new(None),
            clock: clock.clone(),
        }),
        sightings: SightingsLog::open(&root.join("sightings.csv")).unwrap(),
        clock: clock.clone(),
        local_now: Box::new(move || utc(c2.time())),
        data_root: root.clone(),
        restart: Box::new(FlagFile {
            path: root.join("restart.flag"),
            relaunch: Box::new(|| "no relaunch here".into()),
        }),
        housekeeper: Box::new(|_, _| String::new()),
        stop: Arc::new(AtomicBool::new(false)),
    };
    let mut session = Session::new(opts, parts);
    assert_eq!(session.run(), 0);
    let triggers = session.triggers;
    drop(session);
    let r = rec.lock().unwrap();
    assert_eq!(
        r.chats,
        vec![json!([12, null])],
        "one chat read, of the frame grabbed"
    );
    assert!(
        r.said.contains(&"  chat: 1 handle(s)".to_string()),
        "{:?}",
        r.said
    );
    assert_eq!(r.submits, vec![json!(["EXMPLONE", null, null, 0, 0, []])]);
    assert_eq!(triggers, 1);
    let kinds: Vec<&str> = r.alerts.iter().filter_map(|a| a[0].as_str()).collect();
    assert!(kinds.starts_with(&["trigger", "contact"]), "{kinds:?}");
    assert_eq!(r.grabs, 3, "the scan went on over every frame");
    drop(r);
    let _ = std::fs::remove_dir_all(&root);
}
