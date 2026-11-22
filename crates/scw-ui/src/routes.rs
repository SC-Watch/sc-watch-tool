//! The page's requests (ui_server.Handler): the page itself, the audit
//! pictures, the state, and every write the page can make.
//!
//! Who may talk to the server: a loopback Host only (that also defeats DNS
//! rebinding), and writes only as application/json (which forces a CORS
//! preflight no other site can pass). Settings and housekeeping are
//! answered before the reputation database is opened; the rest open a
//! connection per request.

use std::path::Path;
use std::sync::Arc;

use serde_json::{Map, Value, json};

use scw_audio::phrases::{Group, PHRASES, check_phrase, sample};
use scw_input::binding::vk_codes;
use scw_store::json::{py_repr_str, py_str, py_strip};
use scw_store::reputation::PENDING;
use scw_store::{housekeeping, settings, share, watchlist};

use crate::collect::{DISK_TTL, rsi_cache};
use crate::jobs::{BACKFILL_MAX, describe};
use crate::{MAX_BODY, Ui, b64, clean_handle, clean_sid, py_int, text_of, truthy};

/// One request, as read off the connection.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Request {
    pub method: String,
    /// Path and query, as sent.
    pub target: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The path, without the query (urlparse(...).path).
    pub fn path(&self) -> &str {
        let t = self.target.split('#').next().unwrap_or("");
        t.split('?').next().unwrap_or("")
    }

    pub fn query(&self) -> &str {
        let t = self.target.split('#').next().unwrap_or("");
        t.split_once('?').map_or("", |(_, q)| q)
    }
}

/// What a request is answered with.
#[derive(Clone, Debug, PartialEq)]
pub enum Reply {
    Ok {
        ctype: String,
        body: Vec<u8>,
    },
    /// send_error: a status with its message, as an HTML page.
    Error {
        code: u16,
        message: Option<String>,
    },
    /// /api/stream: server-sent events for this view.
    Stream {
        view: Option<String>,
        arg: String,
    },
}

fn json_reply(v: &Value) -> Reply {
    Reply::Ok {
        ctype: "application/json".into(),
        body: serde_json::to_vec(v).unwrap_or_default(),
    }
}

/// The page's own files, built in.
pub const STATIC: &[(&str, &str, &str)] = &[
    ("/", "index.html", "text/html; charset=utf-8"),
    ("/index.html", "index.html", "text/html; charset=utf-8"),
    ("/app.css", "app.css", "text/css; charset=utf-8"),
    ("/app.js", "app.js", "text/javascript; charset=utf-8"),
];

pub fn page_file(name: &str) -> &'static [u8] {
    match name {
        "index.html" => include_bytes!("../../../ui/index.html"),
        "app.css" => include_bytes!("../../../ui/app.css"),
        _ => include_bytes!("../../../ui/app.js"),
    }
}

const POSTS: &[&str] = &[
    "/api/forget",
    "/api/flag",
    "/api/reason",
    "/api/report/add",
    "/api/report/edit",
    "/api/report/delete",
    "/api/audit/confirm",
    "/api/audit/dismiss",
    "/api/settings",
    "/api/maintenance",
    "/api/lookup",
    "/api/restart-watcher",
    "/api/ignore",
    "/api/unignore",
    "/api/org/flag",
    "/api/org/unflag",
    "/api/org/roster",
    "/api/org/members",
    "/api/org/lookup",
    "/api/org/stop",
    "/api/org/member/add",
    "/api/org/member/remove",
    "/api/org/manual",
    "/api/player/enlisted",
    "/api/watchlist/list",
    "/api/watchlist/enable",
    "/api/watchlist/forget",
    "/api/watchlist/import",
    "/api/watchlist/preview",
    "/api/open-folder",
    "/api/support-bundle",
    "/api/flags",
    "/api/audio/phrases",
    "/api/audio/preview",
    "/api/share/export",
    "/api/share/preview",
    "/api/share/import",
    "/api/share/sources",
    "/api/share/remove",
    "/api/apikey",
    "/api/lookup/background",
];

/// parse_qs: decoded, blank values dropped, first one wins here.
pub fn query_first(q: &str, key: &str) -> String {
    for part in q.split('&') {
        let (k, v) = part.split_once('=').unwrap_or((part, ""));
        if url_decode(k) == key && !v.is_empty() {
            return url_decode(v);
        }
    }
    String::new()
}

fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16)
                {
                    Ok(x) => {
                        out.push(x);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

const ERR_PREFIX_VALUE: &str = "ValueError: ";

impl Ui {
    fn host_ok(&self, r: &Request) -> bool {
        let host = r.header("Host").unwrap_or("");
        let host = host.rsplit_once(':').map_or(host, |(h, _)| h);
        matches!(host, "127.0.0.1" | "localhost" | "[::1]")
    }

    /// The view a request names, checked: an unknown one, or an argument
    /// it does not take, is no view.
    pub fn view_query(&self, r: &Request) -> (Option<String>, String) {
        let q = r.query();
        let view = query_first(q, "view");
        if view != "audits" && view != "reports" {
            return (None, String::new());
        }
        let arg: String = query_first(q, "arg").chars().take(64).collect();
        if view == "reports" {
            let install = regex::Regex::new(r"^[0-9a-f]{32}$").unwrap();
            if arg.is_empty() || arg == "self" || install.is_match(&arg) {
                return (Some(view), arg);
            }
            return (None, String::new());
        }
        (Some(view), String::new())
    }

    /// Answers one request.
    pub fn handle(self: &Arc<Self>, r: &Request) -> Reply {
        if !self.host_ok(r) {
            return Reply::Error {
                code: 403,
                message: Some("not a loopback host".into()),
            };
        }
        match r.method.as_str() {
            "GET" => self.get(r),
            "POST" => {
                let ctype = r.header("Content-Type").unwrap_or("");
                let ctype = ctype.split(';').next().unwrap_or("").trim().to_lowercase();
                if ctype != "application/json" {
                    return Reply::Error {
                        code: 415,
                        message: Some("expected application/json".into()),
                    };
                }
                self.post(r)
            }
            _ => Reply::Error {
                code: 501,
                message: Some(format!("Unsupported method ({})", py_repr_str(&r.method))),
            },
        }
    }

    fn get(self: &Arc<Self>, r: &Request) -> Reply {
        let path = r.path();
        if let Some(rel) = path.strip_prefix("/audit/") {
            return self.audit_file(rel);
        }
        match path {
            "/api/inputs" => json_reply(&self.inputs()),
            "/api/stream" => {
                let (view, arg) = self.view_query(r);
                Reply::Stream { view, arg }
            }
            "/api/contacts" => {
                let (view, arg) = self.view_query(r);
                json_reply(
                    &self
                        .collect(view.as_deref(), &arg)
                        .unwrap_or_else(|e| json!({"error": e})),
                )
            }
            _ => match STATIC.iter().find(|(p, _, _)| *p == path) {
                Some((_, name, ctype)) => Reply::Ok {
                    ctype: (*ctype).into(),
                    body: page_file(name).to_vec(),
                },
                None => Reply::Error {
                    code: 404,
                    message: None,
                },
            },
        }
    }

    /// One audit picture: only from audit/, only a bare file name, only an
    /// image - the path arrives from a URL.
    fn audit_file(&self, rel: &str) -> Reply {
        let nf = Reply::Error {
            code: 404,
            message: None,
        };
        let name = Path::new(rel)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name != rel || name.is_empty() || name.starts_with('.') {
            return nf;
        }
        let ext = Path::new(&name)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        if !matches!(ext.as_str(), "png" | "jpg" | "jpeg") {
            return nf;
        }
        let dir = self.data().join("audit");
        let f = dir.join(&name);
        let (Ok(fr), Ok(dr)) = (f.canonicalize(), dir.canonicalize()) else {
            return nf;
        };
        if fr.parent() != Some(dr.as_path()) || !fr.is_file() {
            return nf;
        }
        match std::fs::read(&fr) {
            Ok(body) => Reply::Ok {
                ctype: if ext == "png" {
                    "image/png"
                } else {
                    "image/jpeg"
                }
                .into(),
                body,
            },
            Err(_) => nf,
        }
    }

    /// What is held right now, as binding strings (the binding form polls
    /// this to tell four look-alike joysticks apart).
    pub fn inputs(&self) -> Value {
        let state = (self.hooks.input)();
        let devs = state.devices();
        let mods = [
            (0xA0, "lshift"),
            (0xA1, "rshift"),
            (0xA2, "lctrl"),
            (0xA3, "rctrl"),
            (0xA4, "lalt"),
            (0xA5, "ralt"),
        ];
        let generic = [0x10, 0x11, 0x12];
        let held: Vec<&str> = mods
            .iter()
            .filter(|(vk, _)| state.key_down(*vk))
            .map(|(_, n)| *n)
            .collect();
        let keys: Vec<String> = vk_codes()
            .into_iter()
            .filter(|(_, vk)| {
                !mods.iter().any(|(m, _)| m == vk) && !generic.contains(vk) && state.key_down(*vk)
            })
            .map(|(n, _)| n)
            .collect();
        let mut buttons = Vec::new();
        for d in &devs {
            let mask = state.buttons_down(d.index);
            for b in 0..32 {
                if mask & (1 << b) != 0 {
                    buttons.push(json!({"index": format!("joy{}.b{}", d.index, b + 1),
                                        "stable": format!("{}.b{}", d.ident(), b + 1)}));
                }
            }
        }
        json!({"ok": true,
               "devices": devs.iter().map(|d| json!({"label": d.describe(), "ident": d.ident(),
                                                    "index": d.index})).collect::<Vec<_>>(),
               "mods": held, "keys": keys, "buttons": buttons})
    }

    fn post(self: &Arc<Self>, r: &Request) -> Reply {
        let path = r.path();
        if !POSTS.contains(&path) {
            return Reply::Error {
                code: 404,
                message: None,
            };
        }
        let n = match r
            .header("Content-Length")
            .map(py_strip)
            .filter(|s| !s.is_empty())
        {
            None => 0,
            Some(t) => match scw_store::json::py_int_text(t) {
                Some(n) => n,
                None => {
                    return json_reply(&json!({"ok": false, "error": format!(
                        "ValueError: invalid literal for int() with base 10: {}", py_repr_str(t))}));
                }
            },
        };
        if n > MAX_BODY as i64 {
            return Reply::Error {
                code: 413,
                message: None,
            };
        }
        if n < 0 {
            return Reply::Error {
                code: 400,
                message: None,
            };
        }
        let raw = &r.body[..(n as usize).min(r.body.len())];
        let payload: Value = if raw.is_empty() {
            json!({})
        } else {
            match serde_json::from_slice(raw) {
                Ok(v) => v,
                Err(e) => {
                    return json_reply(&json!({"ok": false,
                                              "error": format!("JSONDecodeError: {e}")}));
                }
            }
        };
        let Value::Object(p) = payload else {
            let t = match payload {
                Value::Array(_) => "list",
                Value::String(_) => "str",
                Value::Number(ref n) if n.is_f64() => "float",
                Value::Number(_) => "int",
                Value::Bool(_) => "bool",
                _ => "NoneType",
            };
            return json_reply(&json!({"ok": false,
                "error": format!("AttributeError: '{t}' object has no attribute 'get'")}));
        };
        json_reply(
            &self
                .post_json(path, &p)
                .unwrap_or_else(|e| json!({"ok": false, "error": e})),
        )
    }

    /// Every write; Err is the Python's "Type: message".
    fn post_json(self: &Arc<Self>, path: &str, p: &Map<String, Value>) -> Result<Value, String> {
        let value_err = |e: String| format!("{ERR_PREFIX_VALUE}{e}");
        match path {
            "/api/settings" => {
                let values = p
                    .get("values")
                    .filter(|v| truthy(Some(v)))
                    .cloned()
                    .unwrap_or(json!({}));
                let values = values.as_object().cloned().unwrap_or_default();
                let saved = settings::save(&values, &self.roots.data(&["settings.json"]))?;
                return Ok(json!({"ok": true, "values": saved}));
            }
            "/api/maintenance" => return self.maintenance(p),
            "/api/lookup" => return Ok(self.lookup(p)),
            "/api/restart-watcher" => return Ok(self.restart_watcher()),
            "/api/lookup/background" => {
                let h = match clean_handle(p.get("handle")) {
                    Ok(h) => h,
                    Err(e) => return Ok(json!({"ok": false, "error": e})),
                };
                let done = self.st().lookup_done.get(&h).copied().unwrap_or(0);
                let started = self.lookup_now(&h, true);
                return Ok(json!({"ok": true, "handle": h, "started": started, "done": done}));
            }
            "/api/apikey" => return Ok(self.save_key(p)),
            "/api/audio/phrases" | "/api/audio/preview" => return Ok(self.speech(path, p)),
            "/api/flags" => return Ok(self.save_flags(p)),
            "/api/support-bundle" => {
                return Ok(json!({"ok": false,
                    "error": "the support bundle is not in this version yet - the Python build makes one"}));
            }
            "/api/open-folder" => return Ok(self.open_folder(p)),
            _ => {}
        }
        if path.starts_with("/api/org/") {
            return Ok(self.org(path, p));
        }
        if path.starts_with("/api/share/") {
            return self.share(path, p);
        }
        if path.starts_with("/api/watchlist/") {
            return self.watchlist(path, p);
        }

        let db = self.open_db()?;
        let raw_handle = p.get("handle").cloned().unwrap_or(json!(""));
        let mut handle = String::new();
        let note: String = py_strip(&text_of(p, "note", ""))
            .chars()
            .take(500)
            .collect();
        let category = py_strip(&text_of(p, "category", "")).to_string();
        let db_err = |e: rusqlite::Error| format!("{}: {e}", crate::jobs::sqlite_kind(&e));
        let mut out: Map<String, Value> = match path {
            "/api/audit/confirm" => {
                let id = py_int(p.get("id"), 0)?;
                let name = clean_handle(p.get("name")).map_err(value_err)?;
                let res = db
                    .confirm_audit(id, &name, p.get("force") == Some(&Value::Bool(true)))
                    .map_err(db_err)?;
                let mut m = to_map(&res);
                if m.get("ok") != Some(&json!(true)) {
                    let mut o = Map::new();
                    o.insert("ok".into(), json!(false));
                    o.extend(m);
                    return Ok(Value::Object(o));
                }
                if let Some(new) = m.get("new").and_then(Value::as_str) {
                    handle = new.to_string();
                }
                self.lookup_now(&handle, false);
                m.insert("lookup".into(), json!("started"));
                m
            }
            "/api/audit/dismiss" => {
                let id = py_int(p.get("id"), 0)?;
                one("dismissed", json!(db.dismiss_audit(id).map_err(db_err)?))
            }
            "/api/report/delete" => {
                let id = py_int(p.get("id"), 0)?;
                let gone = db.delete_report(id).map_err(db_err)?;
                if let Some(g) = &gone {
                    handle = g.handle.clone();
                }
                one("deleted", json!(gone))
            }
            "/api/ignore" => {
                let h = clean_handle(Some(&raw_handle)).map_err(value_err)?;
                let got = db.add_ignored(&h, &note).map_err(value_err)?;
                handle = got.label.clone();
                to_map(&got)
            }
            "/api/unignore" => one(
                "unignored",
                json!(
                    db.remove_ignored(&text_of(p, "label", ""))
                        .map_err(db_err)?
                ),
            ),
            "/api/report/edit" => {
                let id = py_int(p.get("id"), 0)?;
                let cat = (!category.is_empty()).then_some(category.as_str());
                one(
                    "edited",
                    json!(
                        db.edit_report(id, cat, Some(&note), None)
                            .map_err(value_err)?
                    ),
                )
            }
            _ => {
                handle = clean_handle(Some(&raw_handle)).map_err(value_err)?;
                match path {
                    "/api/forget" => one("removed", json!(db.forget(&handle).map_err(db_err)?)),
                    "/api/flag" => {
                        db.add_report(&handle, PENDING, "", "", "self", false)
                            .map_err(db_err)?;
                        one("flagged", json!(true))
                    }
                    "/api/report/add" => {
                        db.add_report(&handle, &category, &note, "", "self", true)
                            .map_err(db_err)?;
                        one("added", json!(true))
                    }
                    "/api/player/enlisted" => {
                        let iso = self.clean_date(p.get("enlisted")).map_err(value_err)?;
                        db.set_manual_enlisted(&handle, &iso).map_err(db_err)?;
                        one(
                            "enlisted",
                            if iso.is_empty() {
                                Value::Null
                            } else {
                                json!(iso)
                            },
                        )
                    }
                    _ => one(
                        "resolved",
                        json!(
                            db.resolve_pending(&handle, &category, &note, "")
                                .map_err(value_err)?
                        ),
                    ),
                }
            }
        };
        let mut body = Map::new();
        body.insert("ok".into(), json!(true));
        body.insert("handle".into(), json!(handle.to_uppercase()));
        body.append(&mut out);
        Ok(Value::Object(body))
    }

    /// Looks one handle up, synchronously - the answer is the response -
    /// and keeps it only when asked to (or when already stored).
    fn lookup(self: &Arc<Self>, p: &Map<String, Value>) -> Value {
        let handle = match clean_handle(p.get("handle")) {
            Ok(h) => h,
            Err(e) => return json!({"ok": false, "error": e}),
        };
        let fresh = truthy(p.get("fresh"));
        let client = match self.client() {
            Ok(c) => c,
            Err(e) => return json!({"ok": false, "handle": handle, "error": e}),
        };
        let c = {
            let _w = scw_rsi::why("lookup-window");
            client.citizen(&handle, fresh, true)
        };
        let c = match c {
            Ok(c) => c,
            Err(e) => {
                self.note_lookup_failure(&client, &handle, &e);
                return json!({"ok": false, "handle": handle,
                              "error": format!("RSI lookup failed: {}", describe(self, &e))});
            }
        };
        let now = self.now();
        let utc = self.hooks.utc;
        let h = c.handle.to_uppercase();
        let unknown: Vec<String> = c
            .note
            .unknown_focus
            .iter()
            .map(|s| s.to_uppercase())
            .collect();
        let mut out = json!({
            "ok": true, "handle": h, "exists": c.exists, "fresh": fresh,
            "unverified": c.unverified, "moniker": c.moniker, "enlisted": c.enlisted,
            "enlisted_days": c.enlisted_days(now, utc), "redacted": c.redacted,
            "profile_url": scw_rsi::profile_url(&h), "lookup_url": scw_rsi::lookup_url(&h),
            "source": c.note.source.map(|s| s.as_str()),
            "observed_days": (c.fetched_at != 0.0).then(|| ((now - c.fetched_at) / 86400.0) as i64),
            "stale": c.note.stale_reason,
            "checking": c.note.checking.is_some(),
            "retry_in": c.note.retry_in,
            "orgs": c.orgs.iter().map(|o| json!({
                "sid": o.sid, "name": o.name, "members": o.members, "is_main": o.is_main,
                "focuses": ([&o.primary_focus, &o.secondary_focus].into_iter()
                    .filter(|f| !f.is_empty()).collect::<Vec<_>>()),
                "focus_unknown": unknown.contains(&o.sid.to_uppercase()),
                "url": scw_rsi::org_url(&o.sid)})).collect::<Vec<_>>(),
        });
        if !c.exists {
            out["message"] = json!(format!("{h}: no such account"));
            return out;
        }
        let db = match self.open_db() {
            Ok(db) => db,
            Err(e) => return json!({"ok": false, "error": e}),
        };
        let orgs: Vec<scw_store::reputation::ProfileOrg> = c
            .orgs
            .iter()
            .map(|o| scw_store::reputation::ProfileOrg {
                sid: o.sid.clone(),
                is_main: o.is_main,
            })
            .collect();
        let r = (|| -> rusqlite::Result<()> {
            let stored = db
                .conn
                .query_row("SELECT 1 FROM players WHERE handle=?", [&h], |_| Ok(()))
                .is_ok();
            out["stored"] = json!(stored);
            if truthy(p.get("add")) {
                let added = db.ensure_player(&h)?;
                if !orgs.is_empty() {
                    db.set_orgs(&h, &orgs)?;
                }
                db.update_profile(&h, &c.moniker, &c.enlisted)?;
                out["stored"] = json!(true);
                out["added"] = json!(added);
                out["message"] = json!(if added {
                    format!("added {h}")
                } else {
                    format!("{h} was already stored — profile refreshed")
                });
            } else if stored {
                if !orgs.is_empty() {
                    db.set_orgs(&h, &orgs)?;
                }
                db.update_profile(&h, &c.moniker, &c.enlisted)?;
                out["refreshed"] = json!(true);
                out["message"] = json!(format!("{h}: {}", c.summary(now, utc)));
            } else {
                out["message"] = json!(format!("{h}: {}", c.summary(now, utc)));
            }
            let a = db.assess(&h, None, false)?;
            out["score"] = json!(crate::round_to(a.score(now, &db.flags), 2));
            out["reasons"] = json!(a.lines(now, &db.flags, utc));
            out["flagged_orgs"] = json!(a.flagged_orgs.iter().map(|(s, _)| s).collect::<Vec<_>>());
            Ok(())
        })();
        if let Err(e) = r {
            return json!({"ok": false, "error": format!("{}: {e}", crate::jobs::sqlite_kind(&e))});
        }
        out
    }

    /// Leaves the watcher a note to restart itself.
    fn restart_watcher(&self) -> Value {
        let text = scw_store::json::py_repr(&json!(self.now()));
        if let Err(e) = std::fs::write(self.data().join("restart.flag"), text) {
            return json!({"ok": false, "error": format!("could not write the flag: {e}")});
        }
        json!({"ok": true,
               "message": "asked the watcher to restart — it picks this up while idle, within a \
                           second or two. If nothing happens, no watcher is running."})
    }

    /// Opens one of the app's own folders, named by key - never a path.
    fn open_folder(&self, p: &Map<String, Value>) -> Value {
        let key = text_of(p, "key", "");
        let Some(row) = self
            .folder_targets()
            .into_iter()
            .find(|r| r["key"] == json!(key))
        else {
            return json!({"ok": false, "error": "not a folder this app uses"});
        };
        let label = row["label"].as_str().unwrap_or("").to_string();
        let path = std::path::PathBuf::from(row["path"].as_str().unwrap_or(""));
        if !path.is_dir() {
            return json!({"ok": false, "error": format!("{label} does not exist any more")});
        }
        match (self.hooks.reveal)(&path) {
            Ok(()) => json!({"ok": true, "message": format!("opened {label}")}),
            Err(kind) => json!({"ok": false, "error": format!("could not open it ({kind})")}),
        }
    }

    /// Export records for an org mate, or import theirs.
    fn share(&self, path: &str, p: &Map<String, Value>) -> Result<Value, String> {
        let mut db = self.open_db()?;
        let now = self.now();
        let utc = self.hooks.utc;
        let refused =
            |e: share::ShareError| -> Value { json!({"ok": false, "error": e.to_string()}) };
        let merge = |mut head: Map<String, Value>, v: Value| -> Value {
            if let Value::Object(m) = v {
                head.extend(m);
            }
            Value::Object(head)
        };
        let ok = || {
            let mut m = Map::new();
            m.insert("ok".into(), json!(true));
            m
        };
        match path {
            "/api/share/export" => {
                let doc = match share::build(&db, &text_of(p, "name", "")) {
                    Ok(d) => d,
                    Err(e) => return Ok(refused(e)),
                };
                Ok(
                    json!({"ok": true, "filename": share::filename(&doc, now, utc),
                          "data": b64(&share::pack(&doc)),
                          "reports": doc.reports.len(), "orgs": doc.orgs.len(),
                          "roster": doc.roster.len(), "from": doc.from}),
                )
            }
            "/api/share/sources" => match share::sources(&db) {
                Ok(s) => Ok(json!({"ok": true, "sources": s})),
                Err(e) => Ok(refused(e)),
            },
            "/api/share/remove" => match share::remove_source(&db, &text_of(p, "install", "")) {
                Ok(out) => {
                    let v = serde_json::to_value(&out).unwrap_or(Value::Null);
                    let msg = format!(
                        "removed everything from {}: {} report(s), {} roster member(s)",
                        crate::jobs::py_value(v.get("name")),
                        v["reports"],
                        v["roster"]
                    );
                    let mut m = merge(ok(), v);
                    m["message"] = json!(msg);
                    Ok(m)
                }
                Err(e) => Ok(refused(e)),
            },
            _ => {
                let data = p.get("data").map(py_str).unwrap_or_default();
                let doc = match share::unpack_b64(&data, now) {
                    Ok(d) => d,
                    Err(e) => return Ok(refused(e)),
                };
                if path == "/api/share/preview" {
                    return Ok(match share::preview(&db, &doc) {
                        Ok(pv) => merge(ok(), serde_json::to_value(&pv).unwrap_or(Value::Null)),
                        Err(e) => refused(e),
                    });
                }
                let inc = p
                    .get("include")
                    .filter(|v| truthy(Some(v)))
                    .cloned()
                    .unwrap_or(json!({}));
                let want = |k: &str| inc.get(k) != Some(&Value::Bool(false));
                let flags_path = self.roots.data(&["flags.json"]);
                let out = match share::apply(
                    &mut db,
                    &doc,
                    &flags_path,
                    want("reports"),
                    want("orgs"),
                    want("roster"),
                ) {
                    Ok(o) => serde_json::to_value(&o).unwrap_or(Value::Null),
                    Err(e) => return Ok(refused(e)),
                };
                let n = |k: &str| out[k].as_i64().unwrap_or(0);
                let mut bits = vec![format!("{} new report(s)", out["reports_added"])];
                if n("reports_updated") != 0 {
                    bits.push(format!("{} updated", out["reports_updated"]));
                }
                if n("reports_dropped") != 0 {
                    bits.push(format!(
                        "{} they had deleted removed",
                        out["reports_dropped"]
                    ));
                }
                if let Some(f) = out["orgs_flagged"].as_array().filter(|f| !f.is_empty()) {
                    let sids: Vec<String> = f
                        .iter()
                        .map(|s| s.as_str().unwrap_or("").to_string())
                        .collect();
                    bits.push(format!(
                        "flagged {} - pull their member lists in the Orgs tab",
                        sids.join(", ")
                    ));
                }
                if n("roster_added") != 0 {
                    bits.push(format!("{} roster member(s)", out["roster_added"]));
                }
                let msg = format!(
                    "imported from {}: {}",
                    crate::jobs::py_value(out.get("from")),
                    bits.join("; ")
                );
                let mut m = merge(ok(), out);
                m["message"] = json!(msg);
                Ok(m)
            }
        }
    }

    /// The spoken-alert phrases: read, save, or hear one.
    fn speech(&self, path: &str, p: &Map<String, Value>) -> Value {
        let cfg_path = self.roots.data(&["audio.json"]);
        let mut cfg = scw_audio::AudioConfig::load(&cfg_path);
        if path == "/api/audio/preview" {
            let key = text_of(p, "key", "");
            let text = text_of(p, "text", "");
            if let Some(err) = check_phrase(&key, &text) {
                return json!({"ok": false, "error": err});
            }
            let speak_handles = self
                .settings()
                .get("speak_handles")
                .map(|v| truthy(Some(v)))
                .unwrap_or(true);
            let spoken = sample(&key, &text, &cfg.speech, speak_handles);
            if spoken.is_empty() {
                return json!({"ok": true, "said": "", "message": "that is blank - only the tone plays"});
            }
            return match (self.hooks.speak)(&spoken) {
                Ok(()) => json!({"ok": true, "said": spoken}),
                Err(why) => json!({"ok": false, "said": spoken,
                                   "error": format!("speech is not working: {why}")}),
            };
        }
        if let Some(Value::Object(values)) = p.get("values") {
            let mut errors = Map::new();
            for (k, t) in values {
                if let Some(e) = check_phrase(k, &py_str(t)) {
                    errors.insert(k.clone(), json!(e));
                }
            }
            if !errors.is_empty() {
                let n = errors.len();
                return json!({"ok": false, "errors": errors,
                              "error": format!("{n} phrase(s) not saved - see below")});
            }
            for (k, t) in values {
                let t = py_strip(&py_str(t)).to_string();
                match cfg.speech.iter_mut().find(|(key, _)| key == k) {
                    Some(e) => e.1 = t,
                    None => cfg.speech.push((k.clone(), t)),
                }
            }
            if let Some(Value::Array(m)) = p.get("muted") {
                cfg.muted = m
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|k| PHRASES.iter().any(|ph| ph.key == *k))
                    .map(String::from)
                    .collect();
            }
            let _ = cfg.save(&cfg_path);
        }
        let group = |g: &Group| match g {
            Group::Line => "line",
            Group::Reason => "reason",
            Group::Flag => "flag",
        };
        json!({"ok": true, "phrases": PHRASES.iter().map(|ph| {
            let value = cfg.speech.iter().find(|(k, _)| k == ph.key).map_or(ph.default, |(_, v)| v.as_str());
            json!({"key": ph.key, "group": group(&ph.group), "title": ph.title, "when": ph.when,
                   "placeholders": ph.placeholders, "default": ph.default, "value": value,
                   "on": !cfg.muted.iter().any(|m| m == ph.key)})
        }).collect::<Vec<_>>()})
    }

    /// Saves the flag definitions (never deletes a key).
    fn save_flags(&self, p: &Map<String, Value>) -> Value {
        let Some(Value::Array(rows)) = p.get("flags") else {
            return json!({"ok": false, "error": "expected a list of flags"});
        };
        if rows.len() > scw_store::flags::SLOTS * 4 {
            return json!({"ok": false, "error": "too many flags"});
        }
        match scw_store::flags::save(rows, &self.roots.data(&["flags.json"])) {
            Ok(saved) => {
                let active = saved
                    .iter()
                    .filter(|f| !f.retired && !f.label.is_empty())
                    .count();
                json!({"ok": true, "active": active, "total": saved.len(),
                       "message": format!("saved {active} flag(s)")})
            }
            Err(e) => json!({"ok": false, "error": e}),
        }
    }

    /// The imported watchlists: list, preview, import, silence, forget.
    fn watchlist(&self, path: &str, p: &Map<String, Value>) -> Result<Value, String> {
        let wl = self
            .open_watchlist(&self.roots.data(&["watchlist.db"]))
            .ok_or_else(|| "DatabaseError: watchlist.db will not open".to_string())?;
        let db_err = |e: rusqlite::Error| format!("{}: {e}", crate::jobs::sqlite_kind(&e));
        match path {
            "/api/watchlist/list" => {
                let (w, n) = wl.reason_coverage().map_err(db_err)?;
                Ok(json!({"ok": true, "enabled": self.watchlist_on(),
                          "sources": wl.sources().map_err(db_err)?,
                          "orgs": wl.orgs_flagged().map_err(db_err)?,
                          "with_reason": w, "entries": n}))
            }
            "/api/watchlist/preview" | "/api/watchlist/import" => {
                let text: String = text_of(p, "text", "").chars().take(200_000).collect();
                if path == "/api/watchlist/preview" {
                    let pv = watchlist::parse_lines(&text);
                    let first = |k: &str, n: usize| -> Value {
                        Value::Array(
                            pv[k]
                                .as_array()
                                .map(|a| a.iter().take(n).cloned().collect())
                                .unwrap_or_default(),
                        )
                    };
                    return Ok(json!({"ok": true, "entries": first("entries", 400),
                                     "n": pv["entries"].as_array().map_or(0, Vec::len),
                                     "orgs": pv["orgs"], "skipped": first("skipped", 20),
                                     "skipped_n": pv["skipped"].as_array().map_or(0, Vec::len)}));
                }
                let name: String = py_strip(&text_of(p, "name", "")).chars().take(80).collect();
                if name.is_empty() {
                    return Ok(json!({"ok": false, "error": "give the list a name"}));
                }
                let unsure = p.get("unsure") == Some(&Value::Bool(true));
                let cut = |k: &str, n: usize| -> String {
                    py_strip(&text_of(p, k, "")).chars().take(n).collect()
                };
                let mut out = watchlist::import_text(
                    &wl,
                    &name,
                    &text,
                    &cut("url", 300),
                    &cut("note", 500),
                    if unsure {
                        watchlist::UNCONFIRMED
                    } else {
                        watchlist::ASSERTED
                    },
                    &cut("by", 80),
                    "",
                )
                .map_err(db_err)?;
                if out["ok"] == json!(true) {
                    let skipped = out.get("skipped_n").filter(|v| truthy(Some(v))).cloned();
                    out["message"] = json!(format!(
                        "imported {} handle(s){}",
                        out["added"],
                        skipped.map_or(String::new(), |s| format!(", {s} line(s) skipped"))
                    ));
                }
                Ok(out)
            }
            _ => {
                let id = py_int(p.get("id"), 0)?;
                match path {
                    "/api/watchlist/enable" => {
                        let on = p.get("on") == Some(&Value::Bool(true));
                        wl.set_enabled(id, on).map_err(db_err)?;
                        Ok(json!({"ok": true, "message":
                            if on { "source on" } else { "source silenced - kept, not deleted" }}))
                    }
                    "/api/watchlist/forget" => {
                        let n = wl.forget_source(id).map_err(db_err)?;
                        Ok(
                            json!({"ok": true, "message": format!("forgotten - {n} claims deleted")}),
                        )
                    }
                    _ => Ok(json!({"ok": false, "error": "unknown watchlist action"})),
                }
            }
        }
    }

    /// Flag an org, unflag it, pull its member list, or look its members up.
    fn org(self: &Arc<Self>, path: &str, p: &Map<String, Value>) -> Value {
        let sid = match clean_sid(p.get("sid")) {
            Ok(s) => s,
            Err(e) => return json!({"ok": false, "error": e}),
        };
        let db = match self.open_db() {
            Ok(db) => db,
            Err(e) => return json!({"ok": false, "error": e}),
        };
        match self.org_with(&db, path, p, &sid) {
            Ok(v) => v,
            Err(e) => json!({"ok": false, "error": e}),
        }
    }

    fn org_with(
        self: &Arc<Self>,
        db: &scw_store::reputation::Database,
        path: &str,
        p: &Map<String, Value>,
        sid: &str,
    ) -> Result<Value, String> {
        let db_err = |e: rusqlite::Error| format!("{}: {e}", crate::jobs::sqlite_kind(&e));
        let flagged: Vec<String> = db
            .flagged_orgs()
            .map_err(db_err)?
            .into_iter()
            .map(|o| o.sid)
            .collect();
        let is_flagged = flagged.iter().any(|s| s == sid);
        let need_flag = [
            "/api/org/members",
            "/api/org/lookup",
            "/api/org/stop",
            "/api/org/member/add",
            "/api/org/member/remove",
        ];
        if need_flag.contains(&path) && !is_flagged {
            return Ok(json!({"ok": false, "error": format!("{sid} is not flagged")}));
        }
        match path {
            "/api/org/member/add" => {
                let h = match clean_handle(p.get("handle")) {
                    Ok(h) => h,
                    Err(e) => return Ok(json!({"ok": false, "error": e})),
                };
                let mut out = to_map(
                    &db.add_member_manually(sid, &h, &text_of(p, "note", ""))
                        .map_err(db_err)?,
                );
                if out.get("ok") == Some(&json!(true)) {
                    let known = truthy(out.get("known"));
                    let msg = match out.get("state").and_then(Value::as_str).unwrap_or("") {
                        "added" if known => {
                            format!("added {h} to {sid} — their record now carries the flag")
                        }
                        "added" => {
                            format!("added {h} to {sid} — not met yet; flagged the moment you do")
                        }
                        "listed" => format!(
                            "{h} is already on {sid}'s public roster — now kept by you too, so a \
                             refresh never drops them"
                        ),
                        "back" => {
                            format!("{h} had dropped off {sid}'s roster — back on it, kept by you")
                        }
                        _ => format!("updated your note on {h}"),
                    };
                    out.insert("message".into(), json!(msg));
                }
                Ok(Value::Object(out))
            }
            "/api/org/member/remove" => {
                let h = match clean_handle(p.get("handle")) {
                    Ok(h) => h,
                    Err(e) => return Ok(json!({"ok": false, "error": e})),
                };
                let mut out = to_map(&db.remove_manual_member(sid, &h).map_err(db_err)?);
                if out.get("ok") == Some(&json!(true)) {
                    let msg = if out.get("state") == Some(&json!("removed")) {
                        format!("took {h} off {sid}")
                    } else {
                        format!(
                            "{h} is on {sid}'s public roster, so stays a member — your note was removed"
                        )
                    };
                    out.insert("message".into(), json!(msg));
                }
                Ok(Value::Object(out))
            }
            "/api/org/manual" => {
                let prim = py_strip(&text_of(p, "primary", "")).to_string();
                let mut sec = py_strip(&text_of(p, "secondary", "")).to_string();
                for f in [&prim, &sec] {
                    if !f.is_empty() && !scw_rsi::model::FOCUSES.contains(&f.as_str()) {
                        let short: String = f.chars().take(24).collect();
                        return Ok(json!({"ok": false,
                            "error": format!("{} is not an RSI org focus", py_repr_str(&short))}));
                    }
                }
                if !prim.is_empty() && prim == sec {
                    sec = String::new();
                }
                let name: String = py_strip(&text_of(p, "name", "")).chars().take(80).collect();
                let kept = db.set_org_manual(sid, &name, &prim, &sec).map_err(db_err)?;
                let msg = if kept.is_some() {
                    format!("saved your entry for {sid}")
                } else {
                    format!("cleared your entry for {sid}")
                };
                Ok(json!({"ok": true, "sid": sid, "manual": kept, "message": msg}))
            }
            "/api/org/members" => self.org_members(sid),
            "/api/org/lookup" => self.lookup_members_now(sid),
            "/api/org/stop" => {
                let job = self.st().org_jobs.get(sid).cloned();
                if let Some(j) = job {
                    let mut g = j.lock().unwrap_or_else(|e| e.into_inner());
                    if g.running {
                        g.stop = true;
                        return Ok(
                            json!({"ok": true, "sid": sid, "message": "stopping after this one"}),
                        );
                    }
                }
                Ok(json!({"ok": true, "sid": sid, "message": "nothing running"}))
            }
            "/api/org/unflag" => {
                if let Some(j) = self.st().org_jobs.pop(sid) {
                    j.lock().unwrap_or_else(|e| e.into_inner()).stop = true;
                }
                db.unflag_org(sid).map_err(db_err)?;
                self.st().rosters.pop(sid);
                if let Ok(c) = self.client() {
                    let _ = c.cache.drop_members(sid);
                }
                Ok(json!({"ok": true, "sid": sid, "message": format!("unflagged {sid}")}))
            }
            "/api/org/flag" => {
                let note: String = py_strip(&text_of(p, "note", ""))
                    .chars()
                    .take(500)
                    .collect();
                let mut name = String::new();
                if let Ok(c) = self.client()
                    && c.enriches()
                {
                    let _w = scw_rsi::why("flag");
                    name = c.org_name(sid, true).unwrap_or_default();
                }
                db.flag_org(sid, &note, &name).map_err(db_err)?;
                if p.get("roster") == Some(&Value::Bool(false)) {
                    return Ok(json!({"ok": true, "sid": sid,
                        "message": format!("flagged {sid} with no member list - it reaches only \
                                            players whose profile names it")}));
                }
                Ok(self.fetch_roster_now(Some(&json!(sid))))
            }
            _ => {
                if !is_flagged {
                    return Ok(json!({"ok": false, "error": format!("{sid} is not flagged")}));
                }
                Ok(self.fetch_roster_now(Some(&json!(sid))))
            }
        }
    }

    /// One housekeeping job; a dry run unless `apply` is exactly true.
    fn maintenance(&self, p: &Map<String, Value>) -> Result<Value, String> {
        let job = text_of(p, "job", "");
        let apply = p.get("apply") == Some(&Value::Bool(true));
        let db_err = |e: rusqlite::Error| format!("{}: {e}", crate::jobs::sqlite_kind(&e));
        match job.as_str() {
            "ghosts" => {
                let db = self.open_db()?;
                let client = self.client()?;
                let res = {
                    let _w = scw_rsi::why("ghost-check");
                    scw_rsi::purge_ghosts(&db, &client, apply).map_err(db_err)?
                };
                let n = res.ghosts.len();
                if apply {
                    let mut st = self.st();
                    for g in &res.ghosts {
                        st.lookups.pop(&g.handle.to_uppercase());
                    }
                }
                Ok(json!({"ok": true, "job": job, "applied": apply, "count": n,
                    "items": res.ghosts.iter().map(|g| format!("{} · seen {}x", g.handle, g.sightings)).collect::<Vec<_>>(),
                    "kept": res.kept.iter().map(|g| format!("{} · {}", g.handle,
                        g.why.clone().unwrap_or_else(|| "None".into()))).collect::<Vec<_>>(),
                    "errors": res.errors.iter().map(|(h, e)| format!("{h}: {e}")).collect::<Vec<_>>(),
                    "message": if apply { format!("removed {n} handle(s) with no RSI account") }
                               else { format!("{n} handle(s) have no RSI account") }}))
            }
            "backfill" => {
                let need = self
                    .settings()
                    .get("rsi_min_votes")
                    .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|x| x as i64)))
                    .unwrap_or(2);
                let db = self.open_db()?;
                let cache = rsi_cache(&self.cache_path(), None);
                let partial = |h: &str| {
                    cache
                        .get(&h.to_uppercase())
                        .is_some_and(|p| truthy(p.get("_partial")))
                };
                let todo: Vec<String> = db
                    .conn
                    .prepare(
                        "SELECT p.handle, p.sightings, COALESCE(MAX(s.votes),0) AS best
                           FROM players p LEFT JOIN sightings s ON s.handle = p.handle
                           GROUP BY p.handle
                           HAVING p.sightings >= ? OR COALESCE(MAX(s.votes),0) >= ?
                           ORDER BY p.sightings DESC",
                    )
                    .and_then(|mut st| {
                        st.query_map([need, need], |r| r.get::<_, String>(0))?
                            .collect()
                    })
                    .map_err(db_err)?;
                let todo: Vec<String> = todo
                    .into_iter()
                    .filter(|h| !cache.contains_key(&h.to_uppercase()) || partial(h))
                    .collect();
                let later = todo.len().saturating_sub(BACKFILL_MAX);
                if !apply {
                    let msg = if todo.is_empty() {
                        "every corroborated contact has been looked up".to_string()
                    } else {
                        format!(
                            "{} contact(s) corroborated ({need}+ votes in one read, or seen {need}+ \
                             times) with no complete profile stored{}",
                            todo.len(),
                            if later > 0 {
                                format!(
                                    " - {BACKFILL_MAX} are looked up per run, to stay inside SENTRY's hourly limit"
                                )
                            } else {
                                String::new()
                            }
                        )
                    };
                    return Ok(
                        json!({"ok": true, "job": job, "applied": false, "count": todo.len(),
                        "items": todo.iter().map(|h| format!("{h} · {}",
                            if partial(h) { "org focus missing" } else { "never looked up" })).collect::<Vec<_>>(),
                        "kept": [], "errors": [], "message": msg}),
                    );
                }
                let client = self.client()?;
                let (mut done, mut ghosts, mut errors) = (0, Vec::new(), Vec::new());
                for h in todo.iter().take(BACKFILL_MAX) {
                    let got = {
                        let _w = scw_rsi::why("backfill");
                        client.citizen(h, false, true)
                    };
                    let cit = match got {
                        Ok(c) => c,
                        Err(e) => {
                            errors.push(format!("{h}: {}", e.type_name()));
                            continue;
                        }
                    };
                    if !cit.exists {
                        ghosts.push(format!("{h} · no such account"));
                        continue;
                    }
                    if !cit.orgs.is_empty() {
                        let orgs: Vec<scw_store::reputation::ProfileOrg> = cit
                            .orgs
                            .iter()
                            .map(|o| scw_store::reputation::ProfileOrg {
                                sid: o.sid.clone(),
                                is_main: o.is_main,
                            })
                            .collect();
                        db.set_orgs(h, &orgs).map_err(db_err)?;
                    }
                    db.update_profile(h, &cit.moniker, &cit.enlisted)
                        .map_err(db_err)?;
                    done += 1;
                }
                let mut msg = format!("looked up {done} contact(s)");
                if !ghosts.is_empty() {
                    msg.push_str(&format!(", {} do not exist", ghosts.len()));
                }
                if later > 0 {
                    msg.push_str(&format!("; {later} left - run it again in an hour"));
                }
                Ok(
                    json!({"ok": true, "job": job, "applied": true, "count": done,
                          "items": [], "kept": ghosts, "errors": errors, "message": msg}),
                )
            }
            "frames" => {
                let cur = self.settings();
                let pol =
                    housekeeping::Policy::load(&self.data().join("housekeeping.json"), Some(&cur));
                let line = if apply {
                    let db = self.open_db().ok();
                    housekeeping::run_now(&pol, self.data(), db.as_ref(), self.now(), true)
                } else {
                    housekeeping::summarise(
                        &housekeeping::sweep(&pol, self.data(), self.now(), false),
                        false,
                    )
                };
                if apply {
                    self.disk_usage(0.0);
                }
                Ok(
                    json!({"ok": true, "job": job, "applied": apply, "message": line,
                          "items": [], "kept": [], "errors": []}),
                )
            }
            "usage" => Ok(
                json!({"ok": true, "job": job, "applied": false, "message": "",
                                 "usage": self.disk_usage(DISK_TTL), "items": [], "kept": [], "errors": []}),
            ),
            _ => Ok(json!({"ok": false, "error": format!("unknown job {}", py_repr_str(&job))})),
        }
    }
}

fn to_map(v: &impl serde::Serialize) -> Map<String, Value> {
    match serde_json::to_value(v) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

fn one(k: &str, v: Value) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(k.into(), v);
    m
}
