//! Every setting, and the one file on disk that holds them - settings.py.
//!
//! The schema is DATA: `settings_schema.json`, generated from settings.py's
//! `describe()` (tests/fixtures/make_settings_ops.py), holds every setting's
//! type, default, range, label and help. The page renders its form from
//! [`describe`]; adding a setting is one entry there and one field in
//! [`Settings`].
//!
//! Values are CLAMPED on load, never trusted: a value that cannot be read
//! costs that one setting, and one out of range is pulled back to the
//! nearest legal value. A save merges over what is on disk, under the file's
//! lock, so a partial update from one form cannot blank the rest.

use std::path::Path;
use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::json::{Expect, py_float_text, py_str, py_strip, read, truthy, update};

const SCHEMA_JSON: &str = include_str!("settings_schema.json");

/// The settings only this port has (settings_schema_rust.json), after
/// settings.py's own: the mining overlay. Everything that holds the port to
/// the Python leaves them out ([`RUST_ONLY`]).
const SCHEMA_RUST_JSON: &str = include_str!("settings_schema_rust.json");

/// The keys of the settings only this port has.
pub const RUST_ONLY: &[&str] = &[
    "mining",
    "mining_key",
    "mining_label_hold",
    "mining_label_size",
];

/// This port's own words for some of settings.py's fields
/// (settings_wording_rust.json): a new help text, and `choice_labels`, the
/// text the page shows for each stored choice. Only the words: the key, the
/// values stored and what they do stay settings.py's. Lookup source stores
/// `sci` for "starcitizen-api.com only" - a name that reads like SC
/// Intelligence, which it has nothing to do with.
const WORDING_RUST_JSON: &str = include_str!("settings_wording_rust.json");

/// The keys of the fields this port rewords.
pub const REWORDED: &[&str] = &["rsi_source"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Bool,
    Int,
    Float,
    Str,
    Choice,
}

/// One setting, as the schema describes it.
#[derive(Clone, Debug, Deserialize)]
pub struct Field {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: Kind,
    pub default: Value,
    pub help: String,
    pub choices: Vec<String>,
    pub lo: Option<f64>,
    pub hi: Option<f64>,
    pub unit: String,
    /// Who must restart before a change takes effect: "watcher", "ui", or
    /// "" for settings read fresh each time.
    pub restart: String,
    pub advanced: bool,
}

#[derive(Deserialize)]
struct Category {
    fields: Vec<Field>,
}

struct Schema {
    described: Value,
    fields: Vec<Field>,
}

fn schema() -> &'static Schema {
    static S: OnceLock<Schema> = OnceLock::new();
    S.get_or_init(|| {
        let mut described: Value = serde_json::from_str(SCHEMA_JSON).expect("settings_schema.json");
        let rust: Value =
            serde_json::from_str(SCHEMA_RUST_JSON).expect("settings_schema_rust.json");
        described
            .as_array_mut()
            .expect("a list of categories")
            .extend(
                rust.as_array()
                    .expect("a list of categories")
                    .iter()
                    .cloned(),
            );
        let wording: Map<String, Value> =
            serde_json::from_str(WORDING_RUST_JSON).expect("settings_wording_rust.json");
        for cat in described.as_array_mut().expect("a list of categories") {
            for f in cat["fields"].as_array_mut().expect("a list of fields") {
                if let Some(Value::Object(words)) = f["key"].as_str().and_then(|k| wording.get(k)) {
                    let f = f.as_object_mut().expect("a field");
                    for (k, v) in words {
                        f.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        let cats: Vec<Category> =
            serde_json::from_value(described.clone()).expect("settings_schema.json");
        Schema {
            described,
            fields: cats.into_iter().flat_map(|c| c.fields).collect(),
        }
    })
}

/// The schema as plain JSON, for the page to render a form from.
pub fn describe() -> Value {
    schema().described.clone()
}

/// settings.py's own description of one of its fields, before this port's
/// words ([`REWORDED`]) - for the tests that hold the port to the Python.
pub fn python_field(key: &str) -> Option<Value> {
    let described: Value = serde_json::from_str(SCHEMA_JSON).expect("settings_schema.json");
    described
        .as_array()?
        .iter()
        .flat_map(|c| c["fields"].as_array().into_iter().flatten())
        .find(|f| f["key"] == key)
        .cloned()
}

/// Every setting, in schema order.
pub fn fields() -> &'static [Field] {
    &schema().fields
}

pub fn field(key: &str) -> Option<&'static Field> {
    fields().iter().find(|f| f.key == key)
}

/// Every default, in schema order.
pub fn defaults() -> Map<String, Value> {
    fields()
        .iter()
        .map(|f| (f.key.clone(), f.default.clone()))
        .collect()
}

impl Field {
    /// Coerces a value to this field's type and range; the default when it
    /// cannot be read at all.
    pub fn clamp(&self, v: &Value) -> Value {
        let x = match self.kind {
            Kind::Bool => {
                return Value::Bool(match v {
                    // A numeric string agrees with the number it spells.
                    Value::String(s) => {
                        let t = py_strip(s).to_lowercase();
                        match py_float_text(&t) {
                            Some(x) => x != 0.0,
                            None => matches!(t.as_str(), "true" | "yes" | "on" | "y"),
                        }
                    }
                    other => truthy(Some(other)),
                });
            }
            Kind::Choice => {
                let s = py_str(v);
                return if self.choices.contains(&s) {
                    Value::String(s)
                } else {
                    self.default.clone()
                };
            }
            Kind::Str => return Value::String(py_str(v)),
            Kind::Int | Kind::Float => match v {
                Value::Bool(b) => *b as u8 as f64,
                Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
                Value::String(s) => py_float_text(s).unwrap_or(f64::NAN),
                _ => f64::NAN,
            },
        };
        // Infinity and NaN cannot be clamped: the default, as for junk.
        if !x.is_finite() {
            return self.default.clone();
        }
        let mut x = if self.kind == Kind::Int {
            x.round_ties_even() // Python's round()
        } else {
            x
        };
        // max(lo, x) and min(hi, x), as Python picks between equals.
        if let Some(lo) = self.lo {
            x = if x > lo { x } else { lo };
        }
        if let Some(hi) = self.hi {
            x = if x < hi { x } else { hi };
        }
        if self.kind == Kind::Int {
            json!(x as i64)
        } else {
            json!(x)
        }
    }
}

/// Old default values that were never anybody's choice, and what they are
/// now: a saved file keeps the default that was current when it was written.
const SUPERSEDED: &[(&str, &str, &str)] = &[
    // A fraction of screen WIDTH, which missed the chat window on a 2560.
    ("chat_region", "0.15,0.30,0.35,0.45", "auto"),
    // Pixels measured on a 1440-tall screen; 'auto' scales by height.
    ("focus_size", "1200x600", "auto"),
];

fn migrate(out: &mut Map<String, Value>) {
    for (key, old, new) in SUPERSEDED {
        if let Some(Value::String(cur)) = out.get(*key)
            && py_strip(cur) == *old
        {
            out.insert(key.to_string(), json!(new));
        }
    }
}

/// Defaults, overlaid with whatever of `raw` is usable, clamped.
pub fn from_raw(raw: Option<&Value>) -> Map<String, Value> {
    let mut out = defaults();
    if let Some(Value::Object(m)) = raw {
        for (k, v) in m {
            if let Some(f) = field(k) {
                out.insert(k.clone(), f.clamp(v));
            }
        }
    }
    migrate(&mut out);
    out
}

/// Every setting, defaults filled in and values clamped. Never fails: a
/// missing or unreadable file is the defaults (an unreadable one is
/// reported, and kept aside before any save replaces it).
pub fn load(path: &Path) -> Map<String, Value> {
    from_raw(read(path, Expect::Object).as_ref())
}

/// Clamps `values`, merges them over what is saved, and writes - as one
/// locked step. Returns what was written. Unknown keys are ignored.
pub fn save(values: &Map<String, Value>, path: &Path) -> Result<Map<String, Value>, String> {
    let written = update(path, Expect::Object, |raw| {
        let mut cur = from_raw(raw.as_ref());
        for (k, v) in values {
            if let Some(f) = field(k) {
                cur.insert(k.clone(), f.clamp(v));
            }
        }
        Value::Object(cur)
    })?;
    match written {
        Value::Object(m) => Ok(m),
        _ => Err("settings were not written as an object".into()),
    }
}

// ---- the typed view ---------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Trigger {
    Key,
    Auto,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HdrMode {
    Auto,
    Off,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub enum Reader {
    #[serde(rename = "ocr")]
    Ocr,
    #[serde(rename = "iris+ocr")]
    IrisOcr,
    #[serde(rename = "iris")]
    Iris,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScanArea {
    Whole,
    Narrow,
    Iris,
    Centre,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioMode {
    Off,
    Tones,
    Speech,
    Both,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mining {
    Off,
    On,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RsiSource {
    Sentry,
    Sci,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RsiMode {
    Auto,
    Live,
    Cache,
}

/// Every setting, typed - what the watcher reads instead of string keys.
/// Built from a clamped map ([`load`]), which always has every key with the
/// right type, so [`Settings::from_map`] cannot fail on one.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    // capture
    pub trigger: Trigger,
    pub key: String,
    pub focus_key: String,
    pub chat_key: String,
    pub chat_region: String,
    pub dual_key: String,
    pub dual_on_read: bool,
    pub focus_size: String,
    pub focus_mask: bool,
    pub burst: i64,
    pub burst_spacing: f64,
    pub ping_delay: f64,
    pub auto_read_delay: f64,
    pub monitor: i64,
    pub hdr_capture: HdrMode,
    pub hz: f64,
    pub queue_max: i64,
    // detection
    pub reader: Reader,
    pub scan_area: ScanArea,
    pub auto_scan_area: ScanArea,
    pub opencv_threads: i64,
    pub min_votes: i64,
    pub gate: bool,
    pub dim_range_retry: bool,
    pub haze: bool,
    pub cooldown: f64,
    pub radius: f64,
    pub expire: f64,
    // audio
    pub audio_mode: AudioMode,
    pub volume: f64,
    pub rate: i64,
    pub speak_handles: bool,
    pub max_spoken_per_burst: i64,
    pub summarise_above: i64,
    // RSI lookups
    pub rsi_enabled: bool,
    pub rsi_source: RsiSource,
    pub rsi_min_votes: i64,
    pub rsi_mode: RsiMode,
    // shared watchlists
    pub watchlist_enabled: bool,
    pub watchlist_variants: bool,
    // audit
    pub debug_log: bool,
    pub audit_enabled: bool,
    pub audit_max_age_days: f64,
    pub audit_keep_frame: bool,
    pub audit_frame_quality: i64,
    // storage
    pub debug_frames: bool,
    pub housekeeping_enabled: bool,
    pub housekeeping_on_start: bool,
    pub housekeeping_every_min: f64,
    pub max_age_hours: f64,
    pub keep_newest: i64,
    pub max_files_per_dir: i64,
    // interface
    pub port: i64,
    pub open_browser: bool,
    pub max_contacts: i64,
    pub tile_size: i64,
    pub tile_square: bool,
    pub show_advanced: bool,
    // the mining overlay (this port only)
    pub mining: Mining,
    pub mining_key: String,
    pub mining_label_hold: f64,
    pub mining_label_size: i64,
}

impl Settings {
    pub fn from_map(m: &Map<String, Value>) -> Result<Settings, String> {
        serde_json::from_value(Value::Object(m.clone())).map_err(|e| e.to_string())
    }

    /// Loaded, clamped and typed.
    pub fn load(path: &Path) -> Settings {
        Settings::from_map(&load(path)).expect("a clamped map is always a valid Settings")
    }
}
