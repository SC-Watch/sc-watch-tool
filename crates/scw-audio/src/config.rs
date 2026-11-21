//! audio.json (audio.AudioConfig): read field by field over the defaults -
//! each value must have its default's type, as jsonstore.coerce rules - and
//! written back in the same order, so the Python and the port share it.

use std::path::Path;

use serde_json::{Map, Value, json};

use scw_store::json::{Expect, coerce, read, write};

use crate::phrases::{merge_speech, phrase};
use crate::tones::default_tones;

#[derive(Clone, Debug, PartialEq)]
pub struct AudioConfig {
    /// off | tones | speech | both (kept as written: an unknown mode plays
    /// nothing, as in the Python).
    pub mode: String,
    pub volume: f64,
    /// Words per minute.
    pub rate: i64,
    /// A substring of an installed voice's name.
    pub voice: String,
    /// Off: "contact" is said instead of the handle.
    pub speak_handles: bool,
    pub max_spoken_per_burst: i64,
    /// More contacts than this: a summary instead of names.
    pub summarise_above: i64,
    /// event -> [[frequency, ms], ...], kept as written.
    pub tones: Map<String, Value>,
    pub speech: Vec<(String, String)>,
    /// Phrases switched off.
    pub muted: Vec<String>,
}

impl Default for AudioConfig {
    fn default() -> Self {
        let mut tones = Map::new();
        for (event, steps) in default_tones() {
            tones.insert(
                event.into(),
                Value::Array(
                    steps
                        .iter()
                        .map(|&(f, ms)| json!([f as i64, ms as i64]))
                        .collect(),
                ),
            );
        }
        Self {
            mode: "both".into(),
            volume: 0.9,
            rate: 175,
            voice: String::new(),
            speak_handles: true,
            max_spoken_per_burst: 2,
            summarise_above: 3,
            tones,
            speech: merge_speech(&[]),
            muted: Vec::new(),
        }
    }
}

impl AudioConfig {
    pub fn tones_on(&self) -> bool {
        self.mode == "tones" || self.mode == "both"
    }

    pub fn speech_on(&self) -> bool {
        self.mode == "speech" || self.mode == "both"
    }

    pub fn off(&self) -> bool {
        self.mode == "off"
    }

    /// A phrase's current text.
    pub fn speech_for(&self, key: &str) -> &str {
        self.speech
            .iter()
            .find(|(k, _)| k == key)
            .map_or("", |(_, v)| v.as_str())
    }

    /// An event's tone steps, as numbers; steps that are not two numbers
    /// are skipped.
    pub fn tone_steps(&self, event: &str) -> Vec<(f64, f64)> {
        self.tones
            .get(event)
            .and_then(Value::as_array)
            .map(|steps| {
                steps
                    .iter()
                    .filter_map(|s| {
                        let a = s.as_array()?;
                        Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// As JSON, in the Python's field order.
    pub fn to_json(&self) -> Value {
        let speech: Map<String, Value> = self
            .speech
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        json!({
            "mode": self.mode,
            "volume": self.volume,
            "rate": self.rate,
            "voice": self.voice,
            "speak_handles": self.speak_handles,
            "max_spoken_per_burst": self.max_spoken_per_burst,
            "summarise_above": self.summarise_above,
            "tones": self.tones,
            "speech": speech,
            "muted": self.muted,
        })
    }

    /// audio.json over the defaults, one field at a time; a missing or
    /// unusable file is the defaults.
    pub fn load(path: &Path) -> Self {
        let defaults = Self::default();
        let base = defaults.to_json();
        let data = read(path, Expect::Object).unwrap_or_else(|| json!({}));
        let get = |k: &str| match data.get(k) {
            Some(v) => coerce(&base[k], v),
            None => base[k].clone(),
        };
        let speech_saved: Vec<(String, String)> = get("speech")
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();
        let muted = get("muted")
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .filter(|k| phrase(k).is_some())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            mode: get("mode").as_str().unwrap_or("both").to_string(),
            volume: get("volume").as_f64().unwrap_or(defaults.volume),
            rate: get("rate").as_i64().unwrap_or(defaults.rate),
            voice: get("voice").as_str().unwrap_or("").to_string(),
            speak_handles: get("speak_handles").as_bool().unwrap_or(true),
            max_spoken_per_burst: get("max_spoken_per_burst")
                .as_i64()
                .unwrap_or(defaults.max_spoken_per_burst),
            summarise_above: get("summarise_above")
                .as_i64()
                .unwrap_or(defaults.summarise_above),
            tones: get("tones").as_object().cloned().unwrap_or(defaults.tones),
            speech: merge_speech(&speech_saved),
            muted,
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        write(path, &self.to_json())
    }
}
