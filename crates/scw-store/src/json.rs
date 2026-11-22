//! One way to read and write a JSON config file - sc-watch's jsonstore.py,
//! so both versions can share settings.json, audio.json and the rest while
//! the port replaces the Python:
//!
//! - READS what people write: a BOM (Notepad), or the Windows codepage when
//!   the bytes are not UTF-8; NaN and Infinity as "no value" (null).
//! - NEVER DESTROYS A FILE IT COULD NOT READ: before a write replaces one
//!   that does not parse, it is copied aside as
//!   `<name>.unreadable-<time>.json`.
//! - WRITES ATOMICALLY (a temp file beside it, then a rename), and the bytes
//!   are Python's `json.dumps(indent=2)`'s, so a save from either version
//!   does not rewrite the whole file.
//! - SURVIVES BEING READ WHILE IT IS WRITTEN: Windows refuses to replace a
//!   file another reader has open, so both sides retry briefly, and writers
//!   to one file queue behind a lock.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;

const RETRY: Duration = Duration::from_millis(500);

/// What the top level of a file must be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expect {
    Object,
    Array,
    Anything,
}

impl Expect {
    fn problem(self, data: &Value) -> Option<String> {
        if self == Expect::Anything {
            return None;
        }
        if data.is_null() {
            return Some("is empty (null)".into());
        }
        let (ok, want) = match self {
            Expect::Object => (data.is_object(), "dict"),
            Expect::Array => (data.is_array(), "list"),
            Expect::Anything => (true, ""),
        };
        (!ok).then(|| format!("holds a {}, not a {want}", py_type(data)))
    }
}

/// Python's type name for a JSON value, as jsonstore's messages say it.
fn py_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

// ---- reading ---------------------------------------------------------------------

/// Windows-1252 for bytes 0x80-0x9F (None: undefined, read as U+FFFD).
const CP1252: [Option<char>; 32] = [
    Some('\u{20AC}'),
    None,
    Some('\u{201A}'),
    Some('\u{0192}'),
    Some('\u{201E}'),
    Some('\u{2026}'),
    Some('\u{2020}'),
    Some('\u{2021}'),
    Some('\u{02C6}'),
    Some('\u{2030}'),
    Some('\u{0160}'),
    Some('\u{2039}'),
    Some('\u{0152}'),
    None,
    Some('\u{017D}'),
    None,
    None,
    Some('\u{2018}'),
    Some('\u{2019}'),
    Some('\u{201C}'),
    Some('\u{201D}'),
    Some('\u{2022}'),
    Some('\u{2013}'),
    Some('\u{2014}'),
    Some('\u{02DC}'),
    Some('\u{2122}'),
    Some('\u{0161}'),
    Some('\u{203A}'),
    Some('\u{0153}'),
    None,
    Some('\u{017E}'),
    Some('\u{0178}'),
];

/// UTF-8 (a BOM dropped), else Windows-1252 with undefined bytes replaced.
pub fn decode(blob: &[u8]) -> String {
    let body = blob.strip_prefix(b"\xef\xbb\xbf").unwrap_or(blob);
    match std::str::from_utf8(body) {
        Ok(s) => s.to_string(),
        Err(_) => blob
            .iter()
            .map(|&b| match b {
                0x80..=0x9F => CP1252[(b - 0x80) as usize].unwrap_or('\u{FFFD}'),
                _ => b as char,
            })
            .collect(),
    }
}

/// The text with NaN, Infinity and -Infinity (outside strings) as null -
/// Python's json reads them, and jsonstore takes them as "no value".
fn constants_to_null(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let (mut i, mut in_str, mut esc) = (0, false, false);
    while i < b.len() {
        let c = b[i];
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
        } else if c == b'"' {
            in_str = true;
        } else {
            let rest = &text[i..];
            let token = ["-Infinity", "Infinity", "NaN"]
                .into_iter()
                .find(|t| rest.starts_with(t));
            if let Some(t) = token {
                out.push_str("null");
                i += t.len();
                continue;
            }
            // A number too big for a double: Python reads it as infinity,
            // which jsonstore then refuses as a value - so, no value.
            if c == b'-' || c.is_ascii_digit() {
                let len = rest
                    .bytes()
                    .take_while(|b| b.is_ascii_digit() || b"+-.eE".contains(b))
                    .count();
                let num = &rest[..len];
                if num.parse::<f64>().is_ok_and(f64::is_infinite) {
                    out.push_str("null");
                } else {
                    out.push_str(num);
                }
                i += len.max(1);
                continue;
            }
        }
        // Copy one whole character.
        let ch = text[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// What reading a file found.
#[derive(Clone, Debug, PartialEq)]
pub enum Parsed {
    /// No such file.
    Missing,
    /// The file exists but could not be opened, even after retrying.
    Unreadable(String),
    /// It was read; Ok is the parsed value, Err why it is not valid JSON.
    Read(Result<Value, String>),
}

fn retrying<T>(mut f: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let deadline = Instant::now() + RETRY;
    loop {
        match f() {
            Err(e)
                if e.kind() == std::io::ErrorKind::PermissionDenied
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            r => return r,
        }
    }
}

/// Parses JSON text as Python's json.loads would, constants included.
pub fn loads(text: &str) -> Result<Value, String> {
    serde_json::from_str(&constants_to_null(text)).map_err(|e| e.to_string())
}

pub fn parse(path: &Path) -> Parsed {
    match retrying(|| std::fs::read(path)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Parsed::Missing,
        Err(e) => Parsed::Unreadable(e.to_string()),
        Ok(blob) => {
            Parsed::Read(loads(&decode(&blob)).map_err(|e| format!("is not valid JSON ({e})")))
        }
    }
}

/// The parsed file, or None when it is missing or unusable (reported once
/// on stderr). Never fails.
pub fn read(path: &Path, expect: Expect) -> Option<Value> {
    let name = file_name(path);
    match parse(path) {
        Parsed::Missing => None,
        Parsed::Unreadable(e) => {
            report(
                path,
                &format!("{name} cannot be opened ({e}) - using defaults for now."),
                false,
            );
            None
        }
        Parsed::Read(r) => {
            let problem = match &r {
                Err(p) => Some(p.clone()),
                Ok(v) => expect.problem(v),
            };
            match problem {
                Some(p) => {
                    report(
                        path,
                        &format!(
                            "{name} {p} - using defaults. Your file is kept as a copy before anything overwrites it."
                        ),
                        false,
                    );
                    None
                }
                None => r.ok().filter(|v| !v.is_null()),
            }
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// ---- writing --------------------------------------------------------------------

/// Python's `repr(float)`: the shortest digits that read back, fixed for
/// exponents -4..16, scientific (at least two exponent digits) otherwise.
pub fn py_float(x: f64) -> Option<String> {
    if !x.is_finite() {
        return None;
    }
    if x == 0.0 {
        return Some(if x.is_sign_negative() { "-0.0" } else { "0.0" }.into());
    }
    // The fewest correctly rounded digits that read back as x - Python's
    // repr, ties to even included (Rust's own shortest form can pick the
    // other neighbour of an exact tie).
    let a = x.abs();
    let sci = (1..=17)
        .map(|p| format!("{:.*e}", p - 1, a))
        .find(|t| t.parse::<f64>() == Ok(a))
        .unwrap_or_else(|| format!("{a:e}"));
    let (mant, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let sign = if x < 0.0 { "-" } else { "" };
    let body = if (-4..16).contains(&exp) {
        if exp >= 0 {
            let point = (exp + 1) as usize;
            if digits.len() <= point {
                format!("{digits}{}.0", "0".repeat(point - digits.len()))
            } else {
                format!("{}.{}", &digits[..point], &digits[point..])
            }
        } else {
            format!("0.{}{digits}", "0".repeat((-exp - 1) as usize))
        }
    } else {
        let m = if digits.len() == 1 {
            digits.clone()
        } else {
            format!("{}.{}", &digits[..1], &digits[1..])
        };
        let s = if exp < 0 { '-' } else { '+' };
        format!("{m}e{s}{:02}", exp.abs())
    };
    Some(format!("{sign}{body}"))
}

fn py_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(c),
            c => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
}

fn dump(v: &Value, level: usize, out: &mut String) -> Result<(), String> {
    let pad = |n: usize| "  ".repeat(n);
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            if let Some(f) = n.as_f64().filter(|_| n.is_f64()) {
                out.push_str(
                    &py_float(f).ok_or("Out of range float values are not JSON compliant")?,
                );
            } else {
                out.push_str(&n.to_string());
            }
        }
        Value::String(s) => py_string(s, out),
        Value::Array(a) if a.is_empty() => out.push_str("[]"),
        Value::Array(a) => {
            out.push('[');
            for (i, item) in a.iter().enumerate() {
                out.push_str(if i == 0 { "\n" } else { ",\n" });
                out.push_str(&pad(level + 1));
                dump(item, level + 1, out)?;
            }
            out.push('\n');
            out.push_str(&pad(level));
            out.push(']');
        }
        Value::Object(m) if m.is_empty() => out.push_str("{}"),
        Value::Object(m) => {
            out.push('{');
            for (i, (k, item)) in m.iter().enumerate() {
                out.push_str(if i == 0 { "\n" } else { ",\n" });
                out.push_str(&pad(level + 1));
                py_string(k, out);
                out.push_str(": ");
                dump(item, level + 1, out)?;
            }
            out.push('\n');
            out.push_str(&pad(level));
            out.push('}');
        }
    }
    Ok(())
}

/// Python's `json.dumps(v, indent=2, allow_nan=False)`, byte for byte.
pub fn dumps(v: &Value) -> Result<String, String> {
    let mut out = String::new();
    dump(v, 0, &mut out)?;
    Ok(out)
}

/// The bytes jsonstore writes for a value: `dumps`, with Python's text-mode
/// line endings - CRLF on Windows (the JSON's strings hold no raw newlines,
/// only escaped ones, so only the layout changes).
pub fn file_text(v: &Value) -> Result<String, String> {
    let text = dumps(v)?;
    Ok(if cfg!(windows) {
        text.replace('\n', "\r\n")
    } else {
        text
    })
}

/// One lock per file, for writers in this process - the one deliberate
/// global: it guards a shared resource, the file itself.
fn lock_for(path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let key = std::path::absolute(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_lowercase();
    LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .entry(key)
        .or_default()
        .clone()
}

/// Copies an unreadable file to `<stem>.unreadable-<time><suffix>`.
pub fn keep_aside(path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let suffix = path
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let dest = path.with_file_name(format!("{stem}.unreadable-{}{suffix}", local_stamp()));
    std::fs::copy(path, &dest).ok()?;
    report(
        path,
        &format!(
            "{} could not be read, so it was kept as {} before being replaced.",
            file_name(path),
            file_name(&dest)
        ),
        true,
    );
    Some(dest)
}

/// Replaces the file atomically, keeping a copy first if it could not be
/// read. Fails loudly: a save that did not happen must say so.
pub fn write(path: &Path, data: &Value) -> Result<(), String> {
    let lock = lock_for(path);
    let _held = lock.lock().unwrap();
    write_locked(path, data)
}

fn write_locked(path: &Path, data: &Value) -> Result<(), String> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let shape = if data.is_array() {
        Expect::Array
    } else if data.is_object() {
        Expect::Object
    } else {
        Expect::Anything
    };
    if let Parsed::Read(r) = parse(path) {
        let bad = match &r {
            Err(_) => true,
            Ok(v) => shape.problem(v).is_some(),
        };
        if bad {
            keep_aside(path);
        }
    }
    let text = file_text(data)?;
    // A unique temp file beside the target, created exclusively (two
    // processes can save), as mkstemp does.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let (tmp, mut file) = loop {
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = path.with_file_name(format!(
            "{}.{}-{n}.tmp",
            file_name(path),
            std::process::id()
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(f) => break (tmp, f),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("{}: {e}", file_name(path))),
        }
    };
    let result = std::io::Write::write_all(&mut file, text.as_bytes())
        .and_then(|_| file.sync_all())
        .and_then(|_| {
            drop(file);
            retrying(|| std::fs::rename(&tmp, path))
        });
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("{}: {e}", file_name(path)));
    }
    Ok(())
}

/// Read, change, write, as one step under the file's lock. `change` gets
/// the parsed file (None when missing or unusable). A file that exists but
/// cannot be opened is an error - writing defaults over a file that is fine
/// but momentarily locked is how concurrent saves used to wipe settings.
pub fn update(
    path: &Path,
    expect: Expect,
    change: impl FnOnce(Option<Value>) -> Value,
) -> Result<Value, String> {
    let lock = lock_for(path);
    let _held = lock.lock().unwrap();
    let current = match parse(path) {
        Parsed::Unreadable(e) => {
            return Err(format!(
                "{} is in use and could not be read ({e}); nothing was saved",
                file_name(path)
            ));
        }
        Parsed::Missing => None,
        Parsed::Read(Err(_)) => None,
        Parsed::Read(Ok(v)) => expect.problem(&v).is_none().then_some(v),
    };
    let new = change(current);
    write_locked(path, &new)?;
    Ok(new)
}

// ---- reporting ------------------------------------------------------------------

/// What makes a report new: the file, its mtime and size, and whether forced.
type ReportKey = (String, u128, u64, bool);

/// A problem with a file, said once on stderr until the file changes.
fn report(path: &Path, message: &str, force: bool) {
    static REPORTED: OnceLock<Mutex<HashSet<ReportKey>>> = OnceLock::new();
    let (mtime, size) = std::fs::metadata(path)
        .map(|m| {
            let t = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            (t, m.len())
        })
        .unwrap_or((0, 0));
    let key = (path.to_string_lossy().into_owned(), mtime, size, force);
    if REPORTED
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .insert(key)
    {
        eprintln!("! {message}");
    }
}

/// Local time as `YYYYmmdd-HHMMSS`, as jsonstore names kept-aside copies.
fn local_stamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

/// `value` when it has the type of `default`, else `default` (jsonstore's
/// coerce, the dataclass defaults being the schema): int and float
/// interchange where lossless, bool is strict, non-finite numbers are refused.
pub fn coerce(default: &Value, value: &Value) -> Value {
    match default {
        Value::Bool(_) => {
            if value.is_boolean() {
                value.clone()
            } else {
                default.clone()
            }
        }
        Value::Number(d) if !d.is_f64() => match value {
            Value::Number(n) if !n.is_f64() => value.clone(),
            Value::Number(n) => {
                let f = n.as_f64().unwrap();
                if f.is_finite() && f.fract() == 0.0 && f.abs() < 9.2e18 {
                    Value::from(f as i64)
                } else {
                    default.clone()
                }
            }
            _ => default.clone(),
        },
        Value::Number(_) => match value {
            Value::Number(n) => n
                .as_f64()
                .filter(|f| f.is_finite())
                .and_then(serde_json::Number::from_f64)
                .map_or(default.clone(), Value::Number),
            _ => default.clone(),
        },
        Value::String(_) if value.is_string() => value.clone(),
        Value::Array(_) if value.is_array() => value.clone(),
        Value::Object(_) if value.is_object() => value.clone(),
        Value::String(_) | Value::Array(_) | Value::Object(_) => default.clone(),
        Value::Null => {
            if value.is_null() {
                default.clone()
            } else {
                value.clone()
            }
        }
    }
}

/// Python's `str(value)` for what a JSON file can hold.
pub fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) if n.is_f64() => py_float(n.as_f64().unwrap()).unwrap_or_default(),
        Value::Number(n) => n.to_string(),
        // Python would print its repr; a list or object here is nonsense.
        other => other.to_string(),
    }
}

/// Python's truthiness.
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::Bool(true)) => true,
    }
}
