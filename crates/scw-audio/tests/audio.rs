//! scw-audio against audio.py's own answers (tests/fixtures/make_fixtures.py):
//! template parsing, check_phrase, render, the word helpers, whole lines, the
//! settings page's samples, the SAPI rate, the tone WAVs and AudioConfig.

use scw_audio::phrases::{
    self, Facts, Reason, distance_words, flag_words, live_flags, reason_words, render, render_line,
    sample, sapi_rate, speakable, speakable_name,
};
use scw_audio::{AudioConfig, format, tones};
use serde_json::Value;
use std::path::PathBuf;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .unwrap()
}

fn cases() -> Value {
    serde_json::from_slice(&fixture("cases.json")).unwrap()
}

fn s(v: &Value) -> &str {
    v.as_str().unwrap()
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| s(x).to_string())
        .collect()
}

fn words(vals: &Value) -> impl Fn(&str) -> String + '_ {
    move |k: &str| {
        vals.get(k)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }
}

fn reason(v: &Value) -> Reason {
    Reason::from_value(v)
}

fn facts(v: &Value) -> Facts {
    Facts {
        handle: v
            .get("handle")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        range_km: v.get("range_km").and_then(Value::as_f64),
        flags: v.get("flags").map(strings).unwrap_or_default(),
        said: v
            .get("said")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(reason).collect())
            .unwrap_or_default(),
        reason: v
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        count: v.get("count").and_then(Value::as_u64).map(|c| c as usize),
        flagged: v.get("flagged").and_then(Value::as_u64).map(|c| c as usize),
    }
}

/// Collects mismatches and fails once with all of them.
struct Check(Vec<String>);

impl Check {
    fn eq<T: PartialEq + std::fmt::Debug>(
        &mut self,
        what: impl std::fmt::Display,
        want: T,
        got: T,
    ) {
        if want != got {
            self.0.push(format!("{what}: python {want:?} rust {got:?}"));
        }
    }

    fn done(self, what: &str) {
        assert!(
            self.0.is_empty(),
            "{what}: {} differ\n{}",
            self.0.len(),
            self.0.join("\n")
        );
    }
}

#[test]
fn parsing_templates() {
    let mut c = Check(Vec::new());
    for case in cases()["parse"].as_array().unwrap() {
        let t = s(&case["t"]);
        let want: Result<Vec<Value>, String> = match case.get("error") {
            Some(e) => Err(s(e).to_string()),
            None => Ok(case["ok"].as_array().unwrap().clone()),
        };
        let got = format::parse(t).map(|pieces| {
            pieces
                .into_iter()
                .map(|p| match p.field {
                    None => serde_json::json!([p.literal, null, null, null]),
                    Some(n) => serde_json::json!([
                        p.literal,
                        n,
                        p.format_spec,
                        p.conversion.map(|c| c.to_string())
                    ]),
                })
                .collect()
        });
        c.eq(format!("parse {t:?}"), want, got);
    }
    c.done("parse");
}

#[test]
fn checking_phrases() {
    let mut c = Check(Vec::new());
    for case in cases()["check"].as_array().unwrap() {
        let (k, t) = (s(&case["key"]), s(&case["t"]));
        let want = case["result"].as_str().map(String::from);
        c.eq(
            format!("check {k} {t:?}"),
            want,
            phrases::check_phrase(k, t),
        );
    }
    c.done("check_phrase");
}

/// A field the settings page refuses to save: a conversion (`{handle!r}`),
/// a format spec (`{handle:>10}`) or an attribute or index
/// (`{handle.upper}`). The Python formats those - quotes spoken, a handle
/// cut to a "precision", a memory address read out - and the port says
/// nothing instead. Only a hand-edited audio.json can hold one.
fn refused_formatting(t: &str) -> bool {
    format::parse(t).is_ok_and(|pieces| {
        pieces.iter().any(|p| {
            p.field.as_ref().is_some_and(|n| n.contains(['.', '[']))
                || p.conversion.is_some()
                || !p.format_spec.is_empty()
        })
    })
}

#[test]
fn rendering() {
    let mut c = Check(Vec::new());
    let mut refused = 0;
    for case in cases()["render"].as_array().unwrap() {
        let t = s(&case["t"]);
        let want = if refused_formatting(t) {
            refused += 1;
            String::new()
        } else {
            s(&case["out"]).to_string()
        };
        c.eq(
            format!("render {t:?} with {}", case["vals"]),
            want,
            render(t, &words(&case["vals"])),
        );
    }
    assert!(
        refused > 0,
        "the fixtures include formatting the port refuses"
    );
    c.done("render");
}

#[test]
fn word_helpers() {
    let all = cases();
    let mut c = Check(Vec::new());
    for case in all["distance"].as_array().unwrap() {
        let got = distance_words(case["km"].as_f64());
        c.eq(
            format!("distance {}", case["km"]),
            strings(&case["out"]),
            vec![got.0, got.1],
        );
    }
    for case in all["speakable"].as_array().unwrap() {
        let h = s(&case["h"]);
        c.eq(
            format!("speakable {h:?}"),
            s(&case["out"]).to_string(),
            speakable(h),
        );
        c.eq(
            format!("speakable_name {h:?}"),
            s(&case["name"]).to_string(),
            speakable_name(h),
        );
    }
    let defaults = phrases::merge_speech(&[]);
    let mut custom = defaults.clone();
    for (k, v) in [
        ("flag_piracy", "pirates ({org})"),
        ("flag_new_account", "{days} days old"),
    ] {
        custom.iter_mut().find(|e| e.0 == k).unwrap().1 = v.into();
    }
    for case in all["flags"].as_array().unwrap() {
        let flags = strings(&case["flags"]);
        let muted = strings(&case["muted"]);
        let speech = if case["custom"].as_bool().unwrap() {
            &custom
        } else {
            &defaults
        };
        c.eq(
            format!("flag_words {flags:?} muted {muted:?}"),
            s(&case["out"]).to_string(),
            flag_words(&flags, speech, &muted),
        );
        let live: Vec<String> = live_flags(&flags, &muted).into_iter().cloned().collect();
        c.eq(
            format!("live_flags {flags:?}"),
            strings(&case["live"]),
            live,
        );
    }
    for case in all["reasons"].as_array().unwrap() {
        let said: Vec<Reason> = case["said"]
            .as_array()
            .unwrap()
            .iter()
            .map(reason)
            .collect();
        let muted = strings(&case["muted"]);
        c.eq(
            format!("reason_words {} muted {muted:?}", case["said"]),
            s(&case["out"]).to_string(),
            reason_words(&said, &defaults, &muted),
        );
    }
    for case in all["rates"].as_array().unwrap() {
        let wpm = case["wpm"].as_f64().unwrap();
        c.eq(
            format!("sapi_rate {wpm}"),
            case["out"].as_i64().unwrap() as i32,
            sapi_rate(wpm),
        );
    }
    c.done("word helpers");
}

#[test]
fn whole_lines_and_samples() {
    let all = cases();
    let defaults = phrases::merge_speech(&[]);
    let mut c = Check(Vec::new());
    for case in all["lines"].as_array().unwrap() {
        let t = s(&case["t"]);
        let sh = case["speak_handles"].as_bool().unwrap();
        c.eq(
            format!("line {t:?} {} handles {sh}", case["facts"]),
            s(&case["out"]).to_string(),
            render_line(t, &facts(&case["facts"]), &defaults, sh, &[]),
        );
    }
    for case in all["samples"].as_array().unwrap() {
        let (k, t, sh) = (
            s(&case["key"]),
            s(&case["text"]),
            case["sh"].as_bool().unwrap(),
        );
        c.eq(
            format!("sample {k} {t:?} {sh}"),
            s(&case["out"]).to_string(),
            sample(k, t, &defaults, sh),
        );
    }
    c.done("lines and samples");
}

#[test]
fn tone_wavs() {
    let blob = fixture("tones.bin");
    let mut c = Check(Vec::new());
    for case in cases()["tones"].as_array().unwrap() {
        let steps: Vec<(f64, f64)> = case["steps"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| (p[0].as_f64().unwrap(), p[1].as_f64().unwrap()))
            .collect();
        let (at, len) = (
            case["at"].as_u64().unwrap() as usize,
            case["len"].as_u64().unwrap() as usize,
        );
        let want = &blob[at..at + len];
        let got = tones::tone_wav(&steps, case["volume"].as_f64().unwrap(), tones::RATE);
        if want != got.as_slice() {
            let first = want.iter().zip(&got).position(|(a, b)| a != b);
            c.0.push(format!(
                "{} at volume {}: {} bytes vs {}, first difference at {first:?}",
                case["event"],
                case["volume"],
                want.len(),
                got.len()
            ));
        }
    }
    c.done("tones");
}

#[test]
fn loading_audio_json() {
    let dir = std::env::temp_dir().join(format!("scw-audio-config-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut c = Check(Vec::new());
    for (i, case) in cases()["configs"].as_array().unwrap().iter().enumerate() {
        let p = dir.join(format!("audio{i}.json"));
        if let Some(text) = case["file"].as_str() {
            std::fs::write(&p, text).unwrap();
        }
        let want: Value = scw_store::json::loads(s(&case["loaded"])).unwrap();
        let got = AudioConfig::load(&p).to_json();
        c.eq(
            format!("config {:?}", case["file"]),
            serde_json::to_string(&want).unwrap(),
            serde_json::to_string(&got).unwrap(),
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
    c.done("AudioConfig.load");
}
