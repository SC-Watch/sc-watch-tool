//! The reasons a report can be filed under (flags.py, exactly). Each report
//! stores a KEY, fixed forever; the LABEL is what you see and can be
//! rewritten. Keys are never deleted: clearing one RETIRES it, so reports
//! filed under it still render with their name and colour. Polarity is
//! stored per flag - negative counts against (+1), positive for (-1),
//! neutral not at all - and an unknown key counts as negative, as the old
//! hard-coded rule did.

use std::path::Path;

use serde_json::{Map, Value, json};

use crate::json::{Expect, read, update};

pub const SLOTS: usize = 12;
pub const POLARITIES: [&str; 3] = ["negative", "positive", "neutral"];
/// Keys the program owns: a report flagged in the moment, reason to follow.
pub const RESERVED: [&str; 1] = ["pending"];
pub const MAX_LABEL: usize = 32;
pub const MAX_SAY: usize = 48;
pub const UNKNOWN_COLOR: &str = "#6b7a8c";

#[derive(Clone, Debug, PartialEq)]
pub struct Flag {
    pub key: String,
    pub label: String,
    pub color: String,
    pub polarity: String,
    /// Spoken instead of the label; "" means the label.
    pub say: String,
    pub retired: bool,
}

impl Flag {
    fn new(key: &str, label: &str, color: &str, polarity: &str) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
            color: color.into(),
            polarity: polarity.into(),
            say: String::new(),
            retired: false,
        }
    }

    /// +1 counts against, -1 for, 0 not at all.
    pub fn sign(&self) -> f64 {
        match self.polarity.as_str() {
            "positive" => -1.0,
            "neutral" => 0.0,
            _ => 1.0,
        }
    }

    /// What speech says for it: "spoken as", else the label.
    pub fn spoken(&self) -> String {
        let s = if self.say.is_empty() {
            &self.label
        } else {
            &self.say
        };
        s.trim_matches(is_space).to_string()
    }

    /// As flags.json holds it (dataclasses.asdict order).
    pub fn to_json(&self) -> Value {
        json!({"key": self.key, "label": self.label, "color": self.color,
               "polarity": self.polarity, "say": self.say, "retired": self.retired})
    }
}

pub fn defaults() -> Vec<Flag> {
    vec![
        Flag::new(
            "unprovoked_attack",
            "Unprovoked attack",
            "#ff5f56",
            "negative",
        ),
        Flag::new("camping", "Camping a location", "#ff9f43", "negative"),
        Flag::new("ransom_broken", "Broke a ransom", "#ffd166", "negative"),
        Flag::new("harassment", "Harassment", "#ff6b9d", "negative"),
        Flag::new(
            "cheating_suspected",
            "Suspected cheating",
            "#c77dff",
            "negative",
        ),
        Flag::new("friendly", "Friendly", "#4ade80", "positive"),
        Flag::new("note", "Note", "#8fa6bb", "neutral"),
    ]
}

/// Python's `str.isspace()` for one character.
fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python's `str(value)` for what a JSON file can hold.
fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(s) => s.clone(),
        Value::Number(n) if n.is_f64() => {
            crate::json::py_float(n.as_f64().unwrap()).unwrap_or_default()
        }
        Value::Number(n) => n.to_string(),
        // Python would print its repr; a list or object here is nonsense.
        other => other.to_string(),
    }
}

/// Python's truthiness.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::Bool(true)) => true,
    }
}

fn take(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// One entry of flags.json as a flag, or None when its key is unusable
/// (flags._coerce).
pub fn coerce(d: &Map<String, Value>) -> Option<Flag> {
    let get = |k: &str, default: &str| d.get(k).map_or(default.to_string(), py_str);
    let key = get("key", "").trim_matches(is_space).to_lowercase();
    let key_ok = (1..=40).contains(&key.chars().count())
        && key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !key_ok || RESERVED.contains(&key.as_str()) {
        return None;
    }
    let label = take(get("label", "").trim_matches(is_space), MAX_LABEL);
    let color = get("color", "").trim_matches(is_space).to_string();
    let color_ok = color.len() == 7
        && color.starts_with('#')
        && color[1..].chars().all(|c| c.is_ascii_hexdigit());
    let color = if color_ok {
        color
    } else {
        UNKNOWN_COLOR.into()
    };
    let pol = get("polarity", "negative")
        .trim_matches(is_space)
        .to_lowercase();
    let polarity = if POLARITIES.contains(&pol.as_str()) {
        pol
    } else {
        "negative".into()
    };
    let say = take(get("say", "").trim_matches(is_space), MAX_SAY);
    let retired = truthy(d.get("retired")) || label.is_empty();
    Some(Flag {
        key,
        label,
        color,
        polarity,
        say,
        retired,
    })
}

/// The flags in a parsed flags.json (None: the file is missing or
/// unusable), defaults appended for any the file has never heard of.
pub fn from_raw(raw: Option<&Value>) -> Vec<Flag> {
    let Some(list) = raw.and_then(Value::as_array) else {
        return defaults();
    };
    let mut out: Vec<Flag> = Vec::new();
    for d in list {
        if let Some(f) = d.as_object().and_then(coerce)
            && !out.iter().any(|o| o.key == f.key)
        {
            out.push(f);
        }
    }
    for d in defaults() {
        if !out.iter().any(|o| o.key == d.key) {
            out.push(d);
        }
    }
    if out.is_empty() { defaults() } else { out }
}

/// Every flag ever defined, retired ones included, as one loaded set.
#[derive(Clone, Debug, PartialEq)]
pub struct Flags {
    pub all: Vec<Flag>,
}

impl Flags {
    pub fn load(path: &Path) -> Self {
        Self {
            all: from_raw(read(path, Expect::Array).as_ref()),
        }
    }

    pub fn defaults() -> Self {
        Self { all: defaults() }
    }

    /// The flags to offer: labelled, not retired.
    pub fn active(&self) -> Vec<&Flag> {
        self.all
            .iter()
            .filter(|f| !f.retired && !f.label.is_empty())
            .collect()
    }

    /// Always something renderable, even for a key nobody defined.
    pub fn by_key(&self, key: &str) -> Flag {
        let k = key.trim_matches(is_space).to_lowercase();
        if let Some(f) = self.all.iter().find(|f| f.key == k) {
            return f.clone();
        }
        if RESERVED.contains(&k.as_str()) {
            return Flag::new(&k, "Awaiting reason", "#7aa2f7", "negative");
        }
        let name = if k.is_empty() {
            "unknown".to_string()
        } else {
            k
        };
        Flag {
            retired: true,
            ..Flag::new(&name, &name, UNKNOWN_COLOR, "negative")
        }
    }

    /// A pending flag scores as a negative: it was pressed because
    /// something happened.
    pub fn sign(&self, key: &str) -> f64 {
        let k = key.trim_matches(is_space).to_lowercase();
        if RESERVED.contains(&k.as_str()) {
            return 1.0;
        }
        self.by_key(key).sign()
    }

    pub fn speakable(&self, key: &str) -> String {
        self.by_key(key).spoken()
    }

    /// Every key that may appear in the database, reserved ones included.
    pub fn keys(&self) -> Vec<String> {
        let mut k: Vec<String> = self.all.iter().map(|f| f.key.clone()).collect();
        k.extend(RESERVED.iter().map(|r| r.to_string()));
        k
    }
}

/// The flags being saved over the ones in the file: a cleared label retires
/// the flag under the name it had, and no key is ever dropped (flags._merge).
fn merge(items: &[Value], previous: &[Flag]) -> Vec<Flag> {
    let mut incoming: Vec<Flag> = Vec::new();
    for it in items {
        let Some(mut f) = it.as_object().and_then(coerce) else {
            continue;
        };
        if f.label.is_empty() {
            if let Some(was) = previous.iter().find(|p| p.key == f.key)
                && !was.label.is_empty()
            {
                f.label = was.label.clone();
                if f.color == UNKNOWN_COLOR {
                    f.color = was.color.clone();
                }
                f.polarity = was.polarity.clone();
                if f.say.is_empty() {
                    f.say = was.say.clone();
                }
            }
            f.retired = true;
        }
        incoming.push(f);
    }
    for old in previous {
        if !incoming.iter().any(|f| f.key == old.key) {
            let mut o = old.clone();
            o.retired = true;
            incoming.push(o);
        }
    }
    incoming
}

/// Writes the flags, keeping every key that already existed - one locked
/// read-merge-write against the file as it is now. Returns what was written.
pub fn save(items: &[Value], path: &Path) -> Result<Vec<Flag>, String> {
    let mut written = Vec::new();
    update(path, Expect::Array, |raw| {
        written = merge(items, &from_raw(raw.as_ref()));
        Value::Array(written.iter().map(Flag::to_json).collect())
    })?;
    Ok(written)
}
