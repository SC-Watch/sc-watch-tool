//! The session log - debuglog.DebugLog: one file per session in
//! debug_bursts/, the newest twelve kept, a line per event, flushed per
//! line, safe to write from several threads, and scrubbed of what would
//! identify you: your home folder, your user name, anything shaped like a
//! secret.
//!
//! Deliberately stricter than debuglog.py: a Windows home folder inside an
//! event's values is scrubbed too. debuglog.py scrubs the JSON text of a
//! value, where every backslash is doubled, so `C:\Users\<name>\...` in an
//! event passed through as it was.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use chrono::NaiveDateTime;
use regex::Regex;
use serde_json::{Map, Value};

use scw_store::json::{dumps_compact, py_str};

pub const DEBUG_DIR: &str = "debug_bursts";
/// How many session logs are kept.
pub const KEEP_SESSIONS: usize = 12;

fn secret_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)(api[_-]?key|token|secret|password)\s*[:=]\s*\S+").unwrap())
}

/// What is taken out of every line: the home folder and the user name.
#[derive(Clone, Debug, Default)]
pub struct Scrub {
    pub homes: Vec<String>,
    pub user: Option<String>,
}

impl Scrub {
    /// From USERPROFILE / HOME and USERNAME / USER, as debuglog.py reads them.
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Scrub {
            homes: ["USERPROFILE", "HOME"]
                .iter()
                .filter_map(|k| var(k))
                .collect(),
            user: var("USERNAME").or_else(|| var("USER")),
        }
    }

    pub fn text(&self, text: &str) -> String {
        let mut out = text.to_string();
        for home in self.homes.iter().filter(|h| h.chars().count() > 3) {
            out = out.replace(home.as_str(), "~");
            out = out.replace(&home.replace('\\', "/"), "~");
            // As it appears inside JSON, backslashes doubled.
            out = out.replace(&home.replace('\\', "\\\\"), "~");
        }
        if let Some(user) = self.user.as_ref().filter(|u| u.chars().count() > 2) {
            let re = Regex::new(&format!("(?i){}", regex::escape(user))).unwrap();
            out = re.replace_all(&out, "<user>").into_owned();
        }
        secret_re()
            .replace_all(&out, "${1}=<redacted>")
            .into_owned()
    }
}

/// The settings as the log shows them: anything secret replaced. The read
/// key is called `key`, which the rule cannot tell from an API key.
pub fn safe_settings(settings: &Map<String, Value>) -> Map<String, Value> {
    settings
        .iter()
        .map(|(k, v)| {
            let lk = k.to_lowercase();
            // re.match: a match that starts at the first character.
            let secret = secret_re()
                .find(&format!("{k}=x"))
                .is_some_and(|m| m.start() == 0)
                || (lk.contains("key") && !lk.contains("_key"));
            (
                k.clone(),
                if secret {
                    Value::String("<redacted>".into())
                } else {
                    v.clone()
                },
            )
        })
        .collect()
}

struct Inner {
    file: Option<File>,
    enabled: bool,
    counts: BTreeMap<String, usize>,
}

pub struct DebugLog {
    pub path: Option<PathBuf>,
    inner: Mutex<Inner>,
    scrub: Scrub,
    now: Box<dyn Fn() -> NaiveDateTime + Send + Sync>,
}

impl DebugLog {
    /// A new session log in `data_root`/debug_bursts, named for `now()`.
    /// Never fails: a log that cannot be written is simply off.
    pub fn open(
        data_root: &Path,
        enabled: bool,
        tag: &str,
        scrub: Scrub,
        now: Box<dyn Fn() -> NaiveDateTime + Send + Sync>,
    ) -> Self {
        let mut log = DebugLog {
            path: None,
            inner: Mutex::new(Inner {
                file: None,
                enabled,
                counts: BTreeMap::new(),
            }),
            scrub,
            now,
        };
        if !enabled {
            return log;
        }
        let dir = data_root.join(DEBUG_DIR);
        let stamp = (log.now)().format("%Y%m%d_%H%M%S").to_string();
        let name = if tag.is_empty() {
            format!("session_{stamp}.log")
        } else {
            format!("session_{stamp}_{tag}.log")
        };
        let path = dir.join(name);
        let opened = std::fs::create_dir_all(&dir).and_then(|_| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
        });
        match opened {
            Ok(f) => {
                log.inner.get_mut().unwrap().file = Some(f);
                log.path = Some(path);
                rotate(&dir);
            }
            Err(_) => log.inner.get_mut().unwrap().enabled = false,
        }
        log
    }

    /// A log that writes nothing.
    pub fn off() -> Self {
        DebugLog {
            path: None,
            inner: Mutex::new(Inner {
                file: None,
                enabled: false,
                counts: BTreeMap::new(),
            }),
            scrub: Scrub::default(),
            now: Box::new(|| chrono::Local::now().naive_local()),
        }
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn enabled(&self) -> bool {
        self.inner().enabled
    }

    fn write(&self, line: &str) {
        let mut g = self.inner();
        if !g.enabled {
            return;
        }
        let ok = match g.file.as_mut() {
            Some(f) => {
                let text = format!("{}\n", line.trim_end());
                f.write_all(text.as_bytes()).and_then(|_| f.flush()).is_ok()
            }
            None => false,
        };
        if !ok {
            g.enabled = false;
        }
    }

    /// The opening section: environment, settings (secrets redacted), the
    /// command line in effect.
    pub fn header(
        &self,
        environment: &[(String, String)],
        settings: &Map<String, Value>,
        args: &[(String, Value)],
    ) {
        if !self.enabled() {
            return;
        }
        let rule = "=".repeat(72);
        self.write(&rule);
        self.write(&format!(
            "sc-watch session log   {}",
            (self.now)().format("%Y-%m-%d %H:%M:%S")
        ));
        self.write(&rule);
        self.write("");
        self.write("-- environment");
        for (k, v) in environment {
            self.write(&format!("   {k:24} {}", self.scrub.text(v)));
        }
        self.write("");
        self.write("-- settings (secrets redacted)");
        let mut safe: Vec<(String, Value)> = safe_settings(settings).into_iter().collect();
        safe.sort_by(|a, b| a.0.cmp(&b.0));
        for (k, v) in safe {
            let text = dumps_compact(&v).unwrap_or_default();
            self.write(&format!("   {k:24} {}", self.scrub.text(&text)));
        }
        if !args.is_empty() {
            self.write("");
            self.write("-- command line overrides in effect");
            let mut sorted = args.to_vec();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            for (k, v) in sorted {
                let text = dumps_compact(&v).unwrap_or_default();
                self.write(&format!("   {k:24} {}", self.scrub.text(&text)));
            }
        }
        self.write("");
        self.write("-- events");
    }

    /// One structured line: time, kind, then field=json for each field.
    pub fn event(&self, kind: &str, fields: &[(&str, Value)]) {
        if !self.enabled() {
            return;
        }
        *self.inner().counts.entry(kind.to_string()).or_insert(0) += 1;
        let bits: Vec<String> = fields
            .iter()
            .map(|(k, v)| {
                format!(
                    "{k}={}",
                    self.scrub.text(&dumps_compact(v).unwrap_or_default())
                )
            })
            .collect();
        let t = (self.now)().format("%H:%M:%S%.3f");
        self.write(&format!("{t} {kind:16} {}", bits.join(" ")));
    }

    pub fn note(&self, text: &str) {
        self.event("note", &[("text", Value::String(text.into()))]);
    }

    /// An error: where, its type, and the first 300 characters of it.
    pub fn exception(&self, place: &str, kind: &str, detail: &str) {
        self.event(
            "error",
            &[
                ("where", Value::String(place.into())),
                ("type", Value::String(kind.into())),
                ("detail", Value::String(detail.chars().take(300).collect())),
            ],
        );
    }

    /// The closing section: how many of each event, then `summary`.
    pub fn close(&self, summary: &[(&str, Value)]) {
        if !self.enabled() {
            return;
        }
        self.write("");
        self.write("-- session summary");
        let counts: Vec<(String, usize)> = self
            .inner()
            .counts
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        for (k, v) in counts {
            self.write(&format!("   {k:24} {v}"));
        }
        for (k, v) in summary {
            self.write(&format!("   {k:24} {}", self.scrub.text(&py_str(v))));
        }
        self.inner().file = None;
    }
}

/// Keeps the newest KEEP_SESSIONS session logs.
fn rotate(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut logs: Vec<(PathBuf, std::time::SystemTime)> = entries
        .flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with("session_") && n.ends_with(".log")
        })
        .filter_map(|e| Some((e.path(), e.metadata().ok()?.modified().ok()?)))
        .collect();
    logs.sort_by_key(|e| std::cmp::Reverse(e.1));
    for (old, _) in logs.into_iter().skip(KEEP_SESSIONS) {
        let _ = std::fs::remove_file(old);
    }
}
