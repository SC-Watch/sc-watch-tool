//! The page's server against ui_server.py itself
//! (tests/fixtures/make_ui_ops.py): the same setup, the same requests on
//! the same made-up network and fake clock give the same status, content
//! type and body for every request, the same requests out to the network,
//! the same folders opened and lines spoken - and at each checkpoint the
//! same rows in all three databases and the same files.

#[path = "../../scw-rsi/tests/common/mod.rs"]
mod common;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::DateTime;
use common::{FakeClock, FakeNet, dump, same};
use scw_input::{Device, InputState};
use scw_rsi::{Clock, FetchError, Reply as NetReply, SENTRY_HOST, Transport};
use scw_store::paths::{Mode, Roots};
use scw_store::reputation::Database;
use scw_ui::routes::{Reply, Request};
use scw_ui::{Hooks, Ui};
use serde_json::{Map, Value, json};

/// The made-up network: each request answered from the fixture's world.
struct World {
    net: FakeNet,
    answers: Map<String, Value>,
    defaults: Map<String, Value>,
}

fn answer_key(key: &str) -> String {
    let rest = key
        .strip_prefix("sentry:")
        .unwrap_or_else(|| panic!("unscripted {key}"));
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let bits: Vec<&str> = path.trim_matches('/').split('/').collect();
    match bits.as_slice() {
        ["citizens", h] => format!("citizen:{}", h.to_uppercase()),
        ["citizens", h, "metadata"] => format!("meta:{}", h.to_uppercase()),
        ["orgs", s, "profile"] => format!("org:{}", s.to_uppercase()),
        ["orgs", s, "memberships"] => {
            let page = query
                .split('&')
                .find_map(|p| p.strip_prefix("page="))
                .unwrap_or("1");
            format!("roster:{}:{page}", s.to_uppercase())
        }
        _ => panic!("unscripted {key}"),
    }
}

impl Transport for World {
    fn get(&self, url: &str, ua: &str, timeout: f64) -> Result<NetReply, FetchError> {
        let key = format!(
            "sentry:{}",
            url.strip_prefix(SENTRY_HOST)
                .unwrap_or_else(|| panic!("a request off the world: {url}"))
        );
        let k = answer_key(&key);
        let kind = k.split(':').next().unwrap();
        let r = self
            .answers
            .get(&k)
            .cloned()
            .unwrap_or_else(|| self.defaults[kind].clone());
        self.net.routes.lock().unwrap().insert(key, vec![r]);
        self.net.get(url, ua, timeout)
    }
}

struct Stick {
    dev: Device,
    keys: HashSet<u32>,
    mask: u32,
}

impl InputState for Stick {
    fn key_down(&self, vk: u32) -> bool {
        self.keys.contains(&vk)
    }
    fn devices(&self) -> Vec<Device> {
        vec![self.dev.clone()]
    }
    fn buttons_down(&self, index: u32) -> u32 {
        if index == 0 { self.mask } else { 0 }
    }
}

/// A page file as the Python serves it: without this port's own lines -
/// those between a line saying `rust-only {` and one saying `} rust-only`,
/// and single lines ending in `// rust-only` (the mining overlay's state in
/// the header) - and with the Python's lines this port leaves out: those
/// between `python-only {` and `} python-only` (an HTML comment in this
/// port: the sc-intelligence.net buttons) are kept, the two marker lines
/// dropped. Everything else must still be the Python's to the byte.
fn python_view(body: &[u8]) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(body) else {
        return body.to_vec();
    };
    let mut out = String::with_capacity(text.len());
    let (mut inside, mut python) = (false, false);
    for line in text.split_inclusive('\n') {
        let bare = line.trim_end();
        if inside {
            inside = !bare.contains("} rust-only");
        } else if python && bare.contains("} python-only") {
            python = false;
        } else if !python && bare.contains("python-only {") {
            python = true;
        } else if python {
            out.push_str(line);
        } else if bare.contains("rust-only {") {
            inside = true;
        } else if !bare.ends_with("// rust-only") {
            out.push_str(line);
        }
    }
    assert!(!inside, "a rust-only block never closed");
    assert!(!python, "a python-only block never closed");
    out.into_bytes()
}

/// SHA-1, for comparing the files served.
fn sha1(data: &[u8]) -> String {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

fn b64d(s: &str) -> Vec<u8> {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let (mut buf, mut n) = (0u32, 0);
    for c in s.bytes().filter(|c| *c != b'=') {
        buf = (buf << 6) | A.iter().position(|x| *x == c).unwrap() as u32;
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((buf >> n) as u8);
        }
    }
    out
}

fn norm(v: &Value, data: &str, program: &str) -> Value {
    match v {
        Value::String(s) => json!(
            s.replace(data, "<DATA>")
                .replace(program, "<PROGRAM>")
                .replace('\\', "/")
        ),
        Value::Array(a) => Value::Array(a.iter().map(|x| norm(x, data, program)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (k.clone(), norm(x, data, program)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// What may differ by design: the database file's size in bytes.
fn loosen(v: &mut Value) {
    if let Some(s) = v.pointer_mut("/stats/bytes") {
        *s = Value::Null;
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn server_matches_ui_server_py() {
    let mut fx: Value = serde_json::from_slice(
        &std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ui_ops.json"),
        )
        .unwrap(),
    )
    .unwrap();
    without_lookup_url(&mut fx);
    let base = std::env::temp_dir().join(format!("scw-ui-ops-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (data, program) = (base.join("data"), base.join("program"));
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&program).unwrap();
    let clock = Arc::new(FakeClock(Mutex::new((
        fx["wall0"].as_f64().unwrap(),
        fx["mono0"].as_f64().unwrap(),
    ))));
    let world = Arc::new(World {
        net: FakeNet {
            clock: clock.clone(),
            key: String::new(),
            routes: Mutex::new(HashMap::new()),
            sent: Mutex::new(Vec::new()),
        },
        answers: fx["answers"].as_object().unwrap().clone(),
        defaults: fx["defaults"].as_object().unwrap().clone(),
    });
    let revealed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let said: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let d = &fx["device"];
    let stick = || Stick {
        dev: Device {
            index: 0,
            vid: d["vid"].as_u64().unwrap() as u16,
            pid: d["pid"].as_u64().unwrap() as u16,
            name: d["name"].as_str().unwrap().into(),
            buttons: 32,
            axes: 5,
            truncated: false,
        },
        keys: fx["held"]["keys"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| k.as_u64().unwrap() as u32)
            .collect(),
        mask: fx["held"]["mask"].as_u64().unwrap() as u32,
    };
    let (r2, s2, c2) = (revealed.clone(), said.clone(), clock.clone());
    let first = stick();
    let hooks = Hooks {
        spawn: Box::new(|job| job()),
        reveal: Box::new(move |p: &Path| {
            r2.lock().unwrap().push(p.display().to_string());
            Ok(())
        }),
        speak: Box::new(move |t: &str| {
            s2.lock().unwrap().push(t.to_string());
            Ok(())
        }),
        input: Box::new(move || {
            Box::new(Stick {
                dev: first.dev.clone(),
                keys: first.keys.clone(),
                mask: first.mask,
            })
        }),
        env: Arc::new(|_| None),
        today: Box::new(move || {
            DateTime::from_timestamp(c2.time() as i64, 0)
                .unwrap()
                .date_naive()
        }),
        utc: true,
    };
    let roots = Roots {
        mode: Mode::Chosen,
        program: program.clone(),
        data: data.clone(),
    };
    let ui = Arc::new(Ui::new(roots, clock.clone(), world.clone(), hooks));
    let (ds, ps) = (data.display().to_string(), program.display().to_string());

    // the setup, as the fixture made it
    {
        let mut db = Database::open(&data.join("sc-watch.db")).unwrap();
        let c = clock.clone();
        db.clock = Box::new(move || c.time());
        db.utc_dates = true;
        for op in fx["setup"].as_array().unwrap() {
            let a = op.as_array().unwrap();
            let s = |i: usize| a[i].as_str().unwrap().to_string();
            let n = |i: usize| a[i].as_f64().unwrap();
            let i = |i: usize| a[i].as_i64().unwrap();
            match a[0].as_str().unwrap() {
                "sql" => {
                    let args: Vec<String> = a[2]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_str().unwrap().into())
                        .collect();
                    db.conn
                        .execute(&s(1), rusqlite::params_from_iter(args.iter()))
                        .unwrap();
                }
                "sight" => {
                    db.record_sighting(&s(1), Some(n(2)), i(3), i(4)).unwrap();
                }
                "advance" => clock.sleep(n(1)),
                "audit" => {
                    db.add_audit(
                        &s(1),
                        Some(n(2)),
                        i(3),
                        i(4),
                        &a[5],
                        &s(6),
                        &s(7),
                        &s(8),
                        "weak_read",
                    )
                    .unwrap();
                }
                "files" => {
                    for (rel, b) in fx["files"].as_object().unwrap() {
                        let p = data.join(rel);
                        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                        std::fs::write(p, b64d(b.as_str().unwrap())).unwrap();
                    }
                }
                other => panic!("{other}"),
            }
        }
        let _ = &mut db;
    }

    let mate = fx["mate"].as_str().unwrap();
    let files = || -> Value {
        let mut out = Map::new();
        for rel in [
            "restart.flag",
            "sc_api_key.txt",
            "settings.json",
            "flags.json",
            "audio.json",
        ] {
            out.insert(
                rel.into(),
                std::fs::read_to_string(data.join(rel))
                    .map_or(Value::Null, |t| json!(t.replace("\r\n", "\n"))),
            );
        }
        let mut names: Vec<String> = std::fs::read_dir(data.join("audit"))
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        out.insert("audit".into(), json!(names));
        Value::Object(out)
    };
    let dump_or_null = |p: PathBuf| if p.exists() { dump(&p) } else { Value::Null };

    let mut bad: Vec<String> = Vec::new();
    for (n, want) in fx["results"].as_array().unwrap().iter().enumerate() {
        world.net.sent.lock().unwrap().clear();
        revealed.lock().unwrap().clear();
        said.lock().unwrap().clear();
        let sent = || -> Vec<Value> {
            world
                .net
                .sent
                .lock()
                .unwrap()
                .iter()
                .map(|s| s["url"].clone())
                .collect()
        };
        let got: Value = match want.get("op").and_then(Value::as_str) {
            Some("tables") => norm(
                &json!({"op": "tables", "db": dump_or_null(data.join("sc-watch.db")),
                        "cache": dump_or_null(data.join("rsi_cache.db")),
                        "watchlist": dump_or_null(data.join("watchlist.db")), "files": files()}),
                &ds,
                &ps,
            ),
            Some("advance") => {
                clock.sleep(want["s"].as_f64().unwrap());
                want.clone()
            }
            Some("sweep") => {
                let lines = ui.sweep_once();
                json!({"op": "sweep", "said": lines,
                       "sent": world.net.sent.lock().unwrap().clone()})
            }
            Some("stream") => {
                let req = Request {
                    method: "GET".into(),
                    target: "/api/stream".into(),
                    headers: vec![("Host".into(), "127.0.0.1:8731".into())],
                    body: Vec::new(),
                };
                let Reply::Stream { view, arg } = ui.handle(&req) else {
                    panic!("not a stream")
                };
                let mut buf: Vec<u8> = Vec::new();
                let left = Mutex::new(1);
                let keep = || {
                    let mut l = left.lock().unwrap();
                    let go = *l > 0;
                    *l -= 1;
                    go
                };
                ui.stream(&mut buf, view.as_deref(), &arg, &keep);
                let text = String::from_utf8(buf).unwrap();
                let events: Vec<Value> = text
                    .split("\n\n")
                    .filter(|f| !f.is_empty())
                    .map(|f| match f.strip_prefix("data: ") {
                        Some(j) => {
                            let mut v = norm(&serde_json::from_str(j).unwrap(), &ds, &ps);
                            loosen(&mut v);
                            v
                        }
                        None => json!(f),
                    })
                    .collect();
                json!({"op": "stream", "status": 200, "ctype": "text/event-stream", "cache": "no-store",
                       "events": events, "sent": world.net.sent.lock().unwrap().clone()})
            }
            Some(other) => panic!("{other}"),
            None => {
                let step = &want["step"];
                let method = step["method"].as_str().unwrap();
                let mut headers = Vec::new();
                match step.get("host") {
                    Some(Value::Null) => {}
                    Some(h) => headers.push(("Host".into(), h.as_str().unwrap().to_string())),
                    None => headers.push(("Host".into(), "127.0.0.1:8731".into())),
                }
                let mut body = Vec::new();
                if method == "POST" {
                    body = match step.get("raw") {
                        Some(r) => r.as_str().unwrap().as_bytes().to_vec(),
                        None => serde_json::to_vec(
                            &serde_json::from_str::<Value>(
                                &step["body"]
                                    .to_string()
                                    .replace("\"MATE\"", &json!(mate).to_string()),
                            )
                            .unwrap(),
                        )
                        .unwrap(),
                    };
                    headers.push((
                        "Content-Type".into(),
                        step.get("ctype")
                            .and_then(Value::as_str)
                            .unwrap_or("application/json")
                            .into(),
                    ));
                    headers.push((
                        "Content-Length".into(),
                        step.get("length")
                            .and_then(Value::as_str)
                            .map_or_else(|| body.len().to_string(), String::from),
                    ));
                }
                let req = Request {
                    method: method.into(),
                    target: step["path"].as_str().unwrap().into(),
                    headers,
                    body,
                };
                let mut out = Map::new();
                match ui.handle(&req) {
                    Reply::Ok { ctype, body } => {
                        out.insert("status".into(), json!(200));
                        out.insert("ctype".into(), json!(ctype));
                        out.insert("cache".into(), json!("no-store"));
                        if ctype == "application/json" {
                            let mut v = norm(&serde_json::from_slice(&body).unwrap(), &ds, &ps);
                            loosen(&mut v);
                            out.insert("json".into(), v);
                        } else {
                            let body = python_view(&body);
                            out.insert("sha1".into(), json!(sha1(&body)));
                            out.insert("bytes".into(), json!(body.len()));
                        }
                    }
                    Reply::Error { code, .. } => {
                        out.insert("status".into(), json!(code));
                        out.insert("ctype".into(), json!("text/html;charset=utf-8"));
                        out.insert("cache".into(), Value::Null);
                    }
                    Reply::Stream { .. } => panic!("a stream"),
                }
                out.insert("sent".into(), json!(sent()));
                let rev = revealed.lock().unwrap().clone();
                if !rev.is_empty() {
                    out.insert("revealed".into(), norm(&json!(rev), &ds, &ps));
                }
                let sp = said.lock().unwrap().clone();
                if !sp.is_empty() {
                    out.insert("said".into(), json!(sp));
                }
                out.insert("step".into(), step.clone());
                Value::Object(out)
            }
        };
        let mut want = want.clone();
        let mut got = got;
        if want.get("op").is_some_and(|o| o == "stream") {
            for e in want["events"].as_array_mut().unwrap() {
                loosen(e);
            }
        } else if let Some(j) = want.get_mut("json") {
            loosen(j);
        }
        let path = want
            .pointer("/step/path")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        // An export's zip bytes are the compressor's: compare what is in it.
        if path == "/api/share/export" {
            let now = clock.time();
            for v in [&mut want, &mut got] {
                if let Some(Value::String(b)) = v.pointer("/json/data").cloned() {
                    let doc = scw_store::share::unpack_b64(&b, now)
                        .map(|d| serde_json::to_value(d).unwrap());
                    v["json"]["data"] = doc.unwrap_or(Value::Null);
                }
            }
        }
        // Deliberate: a ghost check's HTTP failure is worded as every other
        // lookup error, not as urllib's "HTTP Error 500: <reason>".
        if path == "/api/maintenance" {
            for v in [&mut want, &mut got] {
                if let Some(Value::Array(es)) = v.pointer_mut("/json/errors") {
                    for e in es.iter_mut() {
                        let s = e.as_str().unwrap_or("").to_string();
                        let cut: Vec<&str> = s.splitn(3, ": ").collect();
                        *e = json!(cut.iter().take(2).copied().collect::<Vec<_>>().join(": "));
                    }
                }
            }
        }
        // The page lists these in their dict order: compare it too.
        let order = |v: &Value, ptr: &str| -> Vec<String> {
            v.pointer(ptr)
                .and_then(Value::as_object)
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default()
        };
        for ptr in [
            "/json/stats/lookups",
            "/json/stats/lookup_done",
            "/json/stats/rosters",
        ] {
            if order(&want, ptr) != order(&got, ptr) {
                bad.push(format!(
                    "step {n}: {ptr} in the order {:?}, not {:?}",
                    order(&want, ptr),
                    order(&got, ptr)
                ));
            }
        }
        as_python(&mut got);
        // settings.json on disk as data on both sides, so the keys only
        // the port writes can be left out of ours
        for (v, strip) in [(&mut want, false), (&mut got, true)] {
            if let Some(f) = v.pointer_mut("/files/settings.json")
                && let Some(mut parsed) = f.as_str().and_then(|t| serde_json::from_str(t).ok())
            {
                if strip {
                    as_python(&mut parsed);
                }
                *f = parsed;
            }
        }
        if !same(&want, &got) {
            let label = want.get("step").map_or_else(
                || want["op"].to_string(),
                |s| format!("{} {}", s["method"], s["path"]),
            );
            bad.push(format!(
                "step {n} {label}: {}",
                first_difference(&want, &got, "")
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&base);
    assert!(
        bad.is_empty(),
        "{} differences:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// The port's answer as ui_server.py would give it: without the settings
/// only the port has (the mining overlay) - out of every settings map, and
/// their category out of every schema - and with settings.py's own words
/// for the fields the port rewords.
fn as_python(v: &mut Value) {
    let rust_only = scw_store::settings::RUST_ONLY;
    match v {
        Value::Object(m) => {
            if rust_only.iter().all(|k| m.contains_key(*k)) {
                for k in rust_only {
                    m.shift_remove(*k);
                }
            }
            if m.contains_key("choices")
                && let Some(k) = m.get("key").and_then(Value::as_str)
                && scw_store::settings::reworded(k)
            {
                *v = scw_store::settings::python_field(k).expect("a field of settings.py's");
                return;
            }
            for (_, x) in m.iter_mut() {
                as_python(x);
            }
        }
        Value::Array(a) => {
            a.retain(|c| !(c.get("fields").is_some() && c.get("key") == Some(&json!("mining"))));
            for x in a.iter_mut() {
                as_python(x);
            }
        }
        _ => {}
    }
}

/// ui_server.py's answers without the field this port leaves out: each
/// contact's `lookup_url`, an sc-intelligence.net link (sc-watch points at
/// no third-party lookup site).
fn without_lookup_url(v: &mut Value) {
    match v {
        Value::Object(m) => {
            if m.get("lookup_url")
                .and_then(Value::as_str)
                .is_some_and(|u| u.starts_with("https://sc-intelligence.net/"))
            {
                m.shift_remove("lookup_url");
            }
            m.values_mut().for_each(without_lookup_url);
        }
        Value::Array(a) => a.iter_mut().for_each(without_lookup_url),
        _ => {}
    }
}

/// Where two values first differ, as a path and the two sides.
fn first_difference(a: &Value, b: &Value, at: &str) -> String {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for (k, v) in x {
                match y.get(k) {
                    None => return format!("{at}/{k}: missing (want {})", short(v)),
                    Some(w) if !same(v, w) => return first_difference(v, w, &format!("{at}/{k}")),
                    _ => {}
                }
            }
            for k in y.keys() {
                if !x.contains_key(k) {
                    return format!("{at}/{k}: unexpected {}", short(&y[k]));
                }
            }
            format!("{at}: ?")
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            for (i, (v, w)) in x.iter().zip(y).enumerate() {
                if !same(v, w) {
                    return first_difference(v, w, &format!("{at}/{i}"));
                }
            }
            format!("{at}: ?")
        }
        _ => format!("{at}: want {} got {}", short(a), short(b)),
    }
}

fn short(v: &Value) -> String {
    let t = v.to_string();
    t.chars().take(400).collect()
}
