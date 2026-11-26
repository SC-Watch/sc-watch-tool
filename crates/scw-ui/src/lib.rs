//! The page (ui_server.py): a tile per contact, served on 127.0.0.1.
//!
//! Reads the databases the watcher writes, so the two run independently.
//! The page itself - ui/index.html, app.js, app.css - is the Python's,
//! byte for byte; this is the server behind it: the state the page draws
//! ([`collect`]), the writes it can make ([`routes`]), the lookups it
//! starts in the background ([`jobs`]) and the live updates ([`stream`]).
//!
//! Everything outside - the clock, the network, starting a thread, opening
//! a folder, the voice, the keys - is a hook on [`Ui`], so a test can drive
//! the whole server on a script.

pub mod collect;
pub mod desktop;
pub mod http;
pub mod jobs;
pub mod routes;
pub mod stream;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::{Datelike, NaiveDate};
use regex::Regex;
use serde_json::{Map, Value, json};

use scw_input::InputState;
use scw_rsi::{Client, ClientHub, Clock, Transport};
use scw_store::db::Changes;
use scw_store::flags::Flags;
use scw_store::json::{py_repr_str, py_str, py_strip};
use scw_store::paths::Roots;
use scw_store::reputation::Database;
use scw_store::{settings, watchlist};

pub const HOST: &str = "127.0.0.1";
pub const PORT: u16 = 8731;
/// The most one request may make this process hold.
pub const MAX_BODY: usize = 16 * 1024 * 1024;

/// Runs a job in the background (a thread; inline in tests).
pub type Spawner = Box<dyn Fn(Box<dyn FnOnce() + Send>) + Send + Sync>;
/// Shows a folder in the file manager; Err names what went wrong.
pub type Revealer = Box<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;
/// Says a line; Err is why it cannot.
pub type Speaker = Box<dyn Fn(&str) -> Result<(), String> + Send + Sync>;
/// Reads the environment.
pub type Env = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// What the one-process program plugs in: the watcher in the same process
/// restarts on a word rather than a flag file, and hears about settings.
pub struct Links {
    /// Asks the watcher to restart at its next idle moment.
    pub restart_watcher: Box<dyn Fn() + Send + Sync>,
    /// Told after settings are saved: (before, after).
    pub settings_saved: SettingsSaved,
}

/// Told after settings are saved: (before, after).
pub type SettingsSaved = Box<dyn Fn(&Map<String, Value>, &Map<String, Value>) + Send + Sync>;

/// Everything outside the server.
pub struct Hooks {
    pub spawn: Spawner,
    /// Shows a folder in the file manager.
    pub reveal: Revealer,
    /// Says a line (the phrase panel's "hear it").
    pub speak: Speaker,
    /// The keys and joysticks, for the binding form.
    pub input: Box<dyn Fn() -> Box<dyn InputState> + Send + Sync>,
    /// The environment (SC_API_KEY).
    pub env: Env,
    /// Today's date where the user is.
    pub today: Box<dyn Fn() -> NaiveDate + Send + Sync>,
    /// Dates in UTC rather than local time (tests).
    pub utc: bool,
}

/// An org's member lookups, running or just finished (ORG_JOBS).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Job {
    pub running: bool,
    pub total: usize,
    pub done: usize,
    pub found: usize,
    pub incomplete: usize,
    pub no_account: usize,
    pub failed: usize,
    pub stop: bool,
    pub started: f64,
    pub message: String,
}

impl Job {
    /// As the page sees it: everything but `stop`.
    pub fn to_json(&self) -> Value {
        json!({"running": self.running, "total": self.total, "done": self.done,
               "found": self.found, "incomplete": self.incomplete,
               "no_account": self.no_account, "failed": self.failed,
               "started": self.started, "message": self.message})
    }
}

/// A dict in insertion order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Od<V>(pub Vec<(String, V)>);

impl<V: Clone> Od<V> {
    pub fn get(&self, k: &str) -> Option<&V> {
        self.0.iter().find(|e| e.0 == k).map(|e| &e.1)
    }
    /// `d[k] = v`: in place if there, else at the end.
    pub fn set(&mut self, k: &str, v: V) {
        match self.0.iter_mut().find(|e| e.0 == k) {
            Some(e) => e.1 = v,
            None => self.0.push((k.to_string(), v)),
        }
    }
    pub fn pop(&mut self, k: &str) -> Option<V> {
        let i = self.0.iter().position(|e| e.0 == k)?;
        Some(self.0.remove(i).1)
    }
    /// Drops the oldest until at most `n` are left.
    pub fn cap(&mut self, n: usize) {
        while self.0.len() > n {
            self.0.remove(0);
        }
    }
}

/// What the server remembers between requests.
#[derive(Default)]
pub struct State {
    pub lookups: Od<String>,
    pub lookup_done: Od<u64>,
    pub done_seq: u64,
    pub looking: HashSet<String>,
    pub rosters: Od<String>,
    pub roster_busy: HashSet<String>,
    pub org_jobs: Od<Arc<Mutex<Job>>>,
    pub disk: (f64, Vec<Value>),
    pub name_tries: HashMap<String, (usize, f64)>,
    pub failed_tries: HashMap<String, f64>,
    /// _verifier(): decided once, by whether the first client enriched.
    pub verify: Option<bool>,
    /// watchlist.shared(): tried, and whether a list was there.
    pub wl_tried: Option<bool>,
}

pub struct Ui {
    pub roots: Roots,
    pub clock: Arc<dyn Clock>,
    /// The lookup client, shared with the watcher in one process.
    pub hub: Arc<ClientHub>,
    pub hooks: Hooks,
    pub shutting_down: AtomicBool,
    /// Commits in this process, so the stream redraws at once (None: it
    /// looks every 0.4 s, as the Python's).
    pub changes: Option<Arc<Changes>>,
    /// What the watcher in the same process is doing that no database
    /// holds (the mining overlay): `stats.mining` while it has something,
    /// absent otherwise - and always without one, as the Python's page.
    pub board: Option<Arc<scw_store::board::Board>>,
    pub links: Option<Links>,
    state: Mutex<State>,
}

/// Python's truthiness of a JSON value.
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `str(payload.get(k, default))`.
pub fn text_of(p: &Map<String, Value>, k: &str, default: &str) -> String {
    p.get(k).map_or_else(|| default.to_string(), py_str)
}

/// `int(v)` for a JSON value, with Python's messages.
pub fn py_int(v: Option<&Value>, default: i64) -> Result<i64, String> {
    match v {
        None => Ok(default),
        Some(Value::Bool(b)) => Ok(i64::from(*b)),
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| {
                n.as_f64()
                    .filter(|x| x.is_finite())
                    .map(|x| x.trunc() as i64)
            })
            .ok_or_else(|| "OverflowError: cannot convert float infinity to integer".into()),
        Some(Value::String(s)) => scw_store::json::py_int_text(s).ok_or_else(|| {
            format!(
                "ValueError: invalid literal for int() with base 10: {}",
                py_repr_str(s)
            )
        }),
        Some(Value::Null) => Err(
            "TypeError: int() argument must be a string, a bytes-like object or a real number, \
             not 'NoneType'"
                .into(),
        ),
        Some(Value::Array(_)) => Err(
            "TypeError: int() argument must be a string, a bytes-like object or a real number, \
             not 'list'"
                .into(),
        ),
        Some(Value::Object(_)) => Err(
            "TypeError: int() argument must be a string, a bytes-like object or a real number, \
             not 'dict'"
                .into(),
        ),
    }
}

/// Python's round() to an integer: halves to even.
pub fn py_round(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// Python's round(x, n).
pub fn round_to(x: f64, n: usize) -> f64 {
    format!("{x:.n$}").parse().unwrap_or(x)
}

fn re(p: &str) -> Regex {
    Regex::new(p).unwrap()
}

/// A handle as RSI allows it, upper-cased; refused with the Python's
/// message otherwise (the stored rows depend on it).
pub fn clean_handle(raw: Option<&Value>) -> Result<String, String> {
    clean(raw, r"^[A-Za-z0-9_-]{1,40}$", "not a valid handle")
}

/// An org's SID, upper-cased.
pub fn clean_sid(raw: Option<&Value>) -> Result<String, String> {
    clean(raw, r"^[A-Za-z0-9_-]{1,32}$", "not a valid org SID")
}

fn clean(raw: Option<&Value>, pat: &str, what: &str) -> Result<String, String> {
    let h = match raw {
        None | Some(Value::Null) => String::new(),
        Some(v) => py_strip(&py_str(v)).to_string(),
    };
    if !re(pat).is_match(&h) {
        let short: String = h.chars().take(24).collect();
        return Err(format!("{what}: {}", py_repr_str(&short)));
    }
    Ok(h.to_uppercase())
}

/// Python's date.fromisoformat (3.13): YYYY-MM-DD, YYYYMMDD and the ISO
/// week forms YYYY-Www-D and YYYYWwwD.
pub fn from_iso_date(s: &str) -> Option<NaiveDate> {
    let b = s.as_bytes();
    let digits = |r: std::ops::Range<usize>| -> Option<u32> {
        let t = s.get(r)?;
        t.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| t.parse().ok())
            .flatten()
    };
    match b.len() {
        10 if b[4] == b'-' && b[7] == b'-' => {
            NaiveDate::from_ymd_opt(digits(0..4)? as i32, digits(5..7)?, digits(8..10)?)
        }
        8 if b[4] != b'W' => {
            NaiveDate::from_ymd_opt(digits(0..4)? as i32, digits(4..6)?, digits(6..8)?)
        }
        10 if b[4] == b'-' && b[5] == b'W' && b[8] == b'-' => {
            week(digits(0..4)? as i32, digits(6..8)?, digits(9..10)?)
        }
        8 if b[4] == b'W' => week(digits(0..4)? as i32, digits(5..7)?, digits(7..8)?),
        _ => None,
    }
}

fn week(y: i32, w: u32, d: u32) -> Option<NaiveDate> {
    let wd = match d {
        1 => chrono::Weekday::Mon,
        2 => chrono::Weekday::Tue,
        3 => chrono::Weekday::Wed,
        4 => chrono::Weekday::Thu,
        5 => chrono::Weekday::Fri,
        6 => chrono::Weekday::Sat,
        7 => chrono::Weekday::Sun,
        _ => return None,
    };
    NaiveDate::from_isoywd_opt(y, w, wd)
}

impl Ui {
    pub fn new(
        roots: Roots,
        clock: Arc<dyn Clock>,
        transport: Arc<dyn Transport>,
        hooks: Hooks,
    ) -> Self {
        let hub = Arc::new(ClientHub::new(
            roots.data.clone(),
            clock.clone(),
            transport,
            "page",
            hooks.utc,
            hooks.env.clone(),
        ));
        Self::with_hub(roots, clock, hub, hooks)
    }

    /// A page sharing a lookup client with the rest of the program.
    pub fn with_hub(
        roots: Roots,
        clock: Arc<dyn Clock>,
        hub: Arc<ClientHub>,
        hooks: Hooks,
    ) -> Self {
        Ui {
            roots,
            clock,
            hub,
            hooks,
            shutting_down: AtomicBool::new(false),
            changes: None,
            board: None,
            links: None,
            state: Mutex::new(State::default()),
        }
    }

    pub fn st(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn now(&self) -> f64 {
        self.clock.time()
    }

    pub fn data(&self) -> &Path {
        &self.roots.data
    }

    pub fn settings(&self) -> Map<String, Value> {
        settings::load(&self.roots.data.join("settings.json"))
    }

    /// The clock's time as a wall-clock date and time (local, or UTC in
    /// tests that ask for it).
    pub fn local_now(&self) -> chrono::NaiveDateTime {
        let t = self.now();
        let at = chrono::DateTime::from_timestamp(t.floor() as i64, ((t - t.floor()) * 1e9) as u32)
            .unwrap_or_default();
        if self.hooks.utc {
            at.naive_utc()
        } else {
            at.with_timezone(&chrono::Local).naive_local()
        }
    }

    pub fn key_file(&self) -> PathBuf {
        self.roots.data.join("sc_api_key.txt")
    }

    pub fn cache_path(&self) -> PathBuf {
        self.roots.data.join("rsi_cache.db")
    }

    pub fn db_path(&self) -> PathBuf {
        self.roots.data.join("sc-watch.db")
    }

    pub fn flags(&self) -> Flags {
        Flags::load(&self.roots.data.join("flags.json"))
    }

    /// The program's one lookup client (from the hub, which rebuilds it
    /// only when what it was built from changes).
    pub fn client(&self) -> Result<Arc<Client>, String> {
        self.hub.get()
    }

    /// Is the watchlist feature on? (watchlist.enabled: a broken settings
    /// file means on.)
    pub fn watchlist_on(&self) -> bool {
        self.settings()
            .get("watchlist_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    /// The shared watchlist (watchlist.shared): None while the feature is
    /// off. Decided once per process, as the Python's: the bundled list is
    /// seeded at the first look, and a list that did not exist then is not
    /// used until a restart.
    pub fn shared_watchlist(&self) -> Option<watchlist::Watchlist> {
        if !self.watchlist_on() {
            return None;
        }
        let path = self.roots.data.join("watchlist.db");
        let starter = self.roots.program.join("starter_list.json");
        let tried = self.st().wl_tried;
        let present = match tried {
            Some(p) => p,
            None => {
                if (path.exists() || starter.exists())
                    && let Some(wl) = self.open_watchlist(&self.roots.data(&["watchlist.db"]))
                {
                    let _ = watchlist::seed_starter(&starter, &wl);
                }
                let p = path.exists();
                self.st().wl_tried = Some(p);
                p
            }
        };
        if !present {
            return None;
        }
        self.open_watchlist(&path)
    }

    /// A watchlist.db on the page's clock.
    pub fn open_watchlist(&self, path: &Path) -> Option<watchlist::Watchlist> {
        let mut wl = watchlist::Watchlist::open(path).ok()?;
        let clock = self.clock.clone();
        wl.clock = Box::new(move || clock.time());
        wl.starter_name = watchlist::starter_name(&self.roots.program.join("starter_list.json"));
        Some(wl)
    }

    /// sc-watch.db, able to answer identity questions (_open_db).
    pub fn open_db(&self) -> Result<Database, String> {
        let mut db =
            Database::open(&self.roots.data(&["sc-watch.db"])).map_err(|e| e.to_string())?;
        if let Some(ch) = &self.changes {
            scw_store::db::watch(&mut db.conn, ch.clone());
        }
        let clock = self.clock.clone();
        db.clock = Box::new(move || clock.time());
        db.utc_dates = self.hooks.utc;
        db.flags = self.flags();
        let verify = {
            let decided = self.st().verify;
            match decided {
                Some(v) => v,
                None => {
                    let v = self.client().map(|c| c.enriches()).unwrap_or(false);
                    self.st().verify = Some(v);
                    v
                }
            }
        };
        if verify && let Ok(c) = self.client() {
            db.verify = Some(Box::new(scw_rsi::Verifier(c)));
        }
        if let Some(wl) = self.shared_watchlist() {
            db.watchlist = Some(Box::new(wl));
        }
        Ok(db)
    }

    /// Whether a key is set and where from - never the key; its last four
    /// characters tell two keys apart.
    pub fn key_status(&self) -> Value {
        let env = (self.hooks.env)("SC_API_KEY").unwrap_or_default();
        let env = py_strip(&env).to_string();
        let k = std::fs::read(self.key_file())
            .map(|b| {
                let t = String::from_utf8_lossy(&b).into_owned();
                py_strip(t.strip_prefix('\u{feff}').unwrap_or(&t)).to_string()
            })
            .unwrap_or_default();
        let live = if env.is_empty() {
            k.clone()
        } else {
            env.clone()
        };
        let hint: String = {
            let c: Vec<char> = live.chars().collect();
            c[c.len().saturating_sub(4)..].iter().collect()
        };
        json!({
            "set": !live.is_empty(),
            "from": if !env.is_empty() { json!("env") } else if !k.is_empty() { json!("file") } else { Value::Null },
            "hint": hint,
            "file_set": !k.is_empty(),
            "path": self.key_file().display().to_string(),
        })
    }

    /// Writes sc_api_key.txt from the page, or removes it.
    pub fn save_key(&self, p: &Map<String, Value>) -> Value {
        let key = py_strip(&text_of(p, "key", "")).to_string();
        if p.get("remove") == Some(&Value::Bool(true)) {
            if let Err(e) = remove_if_there(&self.key_file()) {
                return json!({"ok": false, "error": format!("could not remove it: {e}")});
            }
            return json!({"ok": true, "status": self.key_status(),
                          "message": "key removed - restart the watcher to stop using it"});
        }
        if !re(r"^[A-Za-z0-9_-]{8,128}$").is_match(&key) {
            return json!({"ok": false, "error": "that does not look like an API key - copy just \
                                                 the key, with nothing around it"});
        }
        if let Err(e) = std::fs::write(self.key_file(), format!("{key}\n")) {
            return json!({"ok": false, "error": format!("could not save it: {e}")});
        }
        let st = self.key_status();
        let mut msg = "key saved - restart the watcher to start using it".to_string();
        if st["from"] == "env" {
            msg.push_str(
                " (note: an SC_API_KEY environment variable is also set, and it wins over the file)",
            );
        }
        json!({"ok": true, "status": st, "message": msg})
    }

    /// A typed enlistment date: YYYY-MM-DD, not in the future, not before
    /// RSI accounts existed; blank clears it.
    pub fn clean_date(&self, raw: Option<&Value>) -> Result<String, String> {
        let v = match raw {
            None | Some(Value::Null) => String::new(),
            Some(x) => py_strip(&py_str(x)).to_string(),
        };
        if v.is_empty() {
            return Ok(String::new());
        }
        let Some(d) = from_iso_date(&v) else {
            let short: String = v.chars().take(24).collect();
            return Err(format!(
                "not a date: {} - use YYYY-MM-DD",
                py_repr_str(&short)
            ));
        };
        if d > (self.hooks.today)() || d.year() < 2011 {
            return Err(format!(
                "{v} is not a date an RSI account could have been made"
            ));
        }
        Ok(d.format("%Y-%m-%d").to_string())
    }

    /// Days from an ISO date to today; None when it is not one.
    pub fn days_since(&self, iso: &str) -> Option<i64> {
        from_iso_date(iso).map(|d| ((self.hooks.today)() - d).num_days())
    }
}

fn remove_if_there(p: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}

/// Standard base64, padded.
pub fn b64(data: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        out.push(A[(n >> 18) as usize & 63] as char);
        out.push(A[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            A[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            A[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_handles() {
        assert_eq!(
            from_iso_date("2015-03-01"),
            NaiveDate::from_ymd_opt(2015, 3, 1)
        );
        assert_eq!(
            from_iso_date("20150301"),
            NaiveDate::from_ymd_opt(2015, 3, 1)
        );
        assert_eq!(
            from_iso_date("2015-W01-1"),
            NaiveDate::from_ymd_opt(2014, 12, 29)
        );
        assert_eq!(from_iso_date("2015-3-1"), None);
        assert_eq!(from_iso_date("2015-02-30"), None);
        assert_eq!(
            clean_handle(Some(&json!(" exmpl-one "))),
            Ok("EXMPL-ONE".into())
        );
        assert_eq!(
            clean_handle(Some(&json!(null))),
            Err("not a valid handle: ''".into())
        );
        assert_eq!(clean_handle(None), Err("not a valid handle: ''".into()));
        assert_eq!(b64(b"hello"), "aGVsbG8=");
        assert_eq!(b64(b"hi!"), "aGkh");
    }
}
