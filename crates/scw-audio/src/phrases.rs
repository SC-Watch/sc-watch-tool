//! What sc-watch says (audio.py, exactly): the phrase table the settings
//! page is drawn from, the checks a phrase must pass to be saved, and how a
//! line is made from a template and an event's facts.

use std::sync::OnceLock;

use regex::Regex;

use crate::format;

/// Where a phrase is used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    /// A whole utterance, for one kind of event.
    Line,
    /// What fills {reason} in "known": the first that applies, in this order.
    Reason,
    /// What fills {flags} in "flagged", one per flag, joined by commas.
    Flag,
}

/// One phrase: key, group, title, when it is said, the placeholders it may
/// use, and its default.
pub struct Phrase {
    pub key: &'static str,
    pub group: Group,
    pub title: &'static str,
    pub when: &'static str,
    pub placeholders: &'static [&'static str],
    pub default: &'static str,
}

/// EVERYTHING THAT CAN BE SAID, in the settings page's order.
pub const PHRASES: &[Phrase] = &[
    Phrase {
        key: "flagged",
        group: Group::Line,
        title: "Lookup warning",
        when: "A lookup found an org flag on someone: a piracy, smuggling or infiltration focus, a syndicate org, or a new account. It arrives a few seconds after the read, once the lookup answers.",
        placeholders: &["handle", "distance", "range", "flags"],
        default: "Warning. {handle}. {distance}. {flags}",
    },
    Phrase {
        key: "known",
        group: Group::Line,
        title: "Known contact",
        when: "Someone your own records flag - your reports, a flagged org, the watch list, or reports someone shared with you. Said as soon as the read settles.",
        placeholders: &["handle", "distance", "range", "reason"],
        default: "Known contact. {handle}. {distance}. {reason}",
    },
    Phrase {
        key: "summary",
        group: Group::Line,
        title: "Crowd summary",
        when: "Too many contacts to name one by one, or more flagged ones than you allow per read (Audio settings).",
        placeholders: &["count", "flagged"],
        default: "{count} contacts, {flagged} flagged",
    },
    Phrase {
        key: "nothing",
        group: Group::Line,
        title: "Nothing found",
        when: "A read that found nobody. Blank plays the tone alone.",
        placeholders: &[],
        default: "",
    },
    Phrase {
        key: "reason_report",
        group: Group::Reason,
        title: "Your report",
        when: "You have filed one report on them. {label} is the reason's spoken name - set it per reason under Report reasons, 'spoken as'.",
        placeholders: &["label"],
        default: "{label}",
    },
    Phrase {
        key: "reason_reports",
        group: Group::Reason,
        title: "Your reports",
        when: "You have filed several; {label} is the reason you used most.",
        placeholders: &["label", "count"],
        default: "{count} reports. {label}",
    },
    Phrase {
        key: "reason_shared",
        group: Group::Reason,
        title: "A shared report",
        when: "Reports someone else shared with you, and none of your own. {source} is the name they gave when they exported.",
        placeholders: &["label", "source"],
        default: "Reported by {source}. {label}",
    },
    Phrase {
        key: "reason_org",
        group: Group::Reason,
        title: "Flagged org member",
        when: "They are on the roster of an org you flagged, or you put them on it.",
        placeholders: &["org"],
        default: "Member of {org}",
    },
    Phrase {
        key: "reason_former",
        group: Group::Reason,
        title: "Former flagged org member",
        when: "An org you flagged used to list them and no longer does.",
        placeholders: &["org"],
        default: "Formerly {org}",
    },
    Phrase {
        key: "reason_watchlist",
        group: Group::Reason,
        title: "On the watch list",
        when: "An exact match on a watch list - including the one that came with sc-watch.",
        placeholders: &[],
        default: "On the watch list",
    },
    Phrase {
        key: "flag_piracy",
        group: Group::Flag,
        title: "Piracy org",
        when: "One of their orgs declares a Piracy focus.",
        placeholders: &["org"],
        default: "Piracy",
    },
    Phrase {
        key: "flag_syndicate",
        group: Group::Flag,
        title: "Syndicate org",
        when: "One of their orgs is a Syndicate.",
        placeholders: &["org"],
        default: "Syndicate",
    },
    Phrase {
        key: "flag_smuggling",
        group: Group::Flag,
        title: "Smuggling org",
        when: "One of their orgs declares a Smuggling focus.",
        placeholders: &["org"],
        default: "Smuggling",
    },
    Phrase {
        key: "flag_infiltration",
        group: Group::Flag,
        title: "Infiltration org",
        when: "One of their orgs declares an Infiltration focus.",
        placeholders: &["org"],
        default: "Infiltration",
    },
    Phrase {
        key: "flag_new_account",
        group: Group::Flag,
        title: "New account",
        when: "The account is under 90 days old.",
        placeholders: &["days"],
        default: "New account",
    },
];

pub fn phrase(key: &str) -> Option<&'static Phrase> {
    PHRASES.iter().find(|p| p.key == key)
}

/// The spoken lines, defaults first: every phrase, then the events with no
/// phrase of their own (never spoken by default, not offered for editing).
pub fn default_speech() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = PHRASES
        .iter()
        .map(|p| (p.key.to_string(), p.default.to_string()))
        .collect();
    for (k, d) in [
        ("contact", ""),
        ("trigger", ""),
        ("error", "Alert system error"),
    ] {
        v.push((k.into(), d.into()));
    }
    v
}

/// What earlier versions saved as their defaults: a copy of one is the old
/// default, not a choice, so it is upgraded on load.
const LEGACY_DEFAULTS: &[(&str, &str)] = &[
    ("flagged", "Warning. {handle}. {range} kilometres. {flags}"),
    (
        "known",
        "Known contact. {handle}. {range} kilometres. {reason}",
    ),
];

pub const MAX_PHRASE: usize = 200;

/// The saved phrases over the defaults (audio.merge_speech): a key the file
/// lacks gets its default; a saved copy of an old default becomes the new
/// one; anything else written is kept. Order: the defaults', then new keys
/// in the file's order.
pub fn merge_speech(saved: &[(String, String)]) -> Vec<(String, String)> {
    let defaults = default_speech();
    let mut out = defaults.clone();
    for (k, v) in saved {
        let legacy = LEGACY_DEFAULTS.iter().any(|(lk, lv)| lk == k && lv == v);
        let value = if legacy {
            defaults
                .iter()
                .find(|(dk, _)| dk == k)
                .map_or(v.clone(), |(_, d)| d.clone())
        } else {
            v.clone()
        };
        match out.iter_mut().find(|(ok, _)| ok == k) {
            Some(e) => e.1 = value,
            None => out.push((k.clone(), value)),
        }
    }
    out
}

/// Why a phrase cannot be saved, or None (audio.check_phrase).
pub fn check_phrase(key: &str, text: &str) -> Option<String> {
    let Some(p) = phrase(key) else {
        return Some(format!("{key} is not a phrase"));
    };
    if text.chars().count() > MAX_PHRASE {
        return Some(format!("keep it under {MAX_PHRASE} characters"));
    }
    let pieces = match format::parse(text) {
        Ok(v) => v,
        Err(e) => return Some(format!("unbalanced braces ({e})")),
    };
    for piece in pieces {
        let Some(name) = piece.field else { continue };
        if !p.placeholders.contains(&name.as_str()) {
            if p.placeholders.is_empty() {
                return Some(format!(
                    "{{{name}}} is not available here - this one takes no placeholders"
                ));
            }
            let mut names: Vec<&str> = p.placeholders.to_vec();
            names.sort_unstable();
            let ok: Vec<String> = names.iter().map(|n| format!("{{{n}}}")).collect();
            return Some(format!(
                "{{{name}}} is not available here - this one can use {}",
                ok.join(", ")
            ));
        }
        if !piece.format_spec.is_empty() || piece.conversion.is_some() {
            return Some(format!("{{{name}}} takes no formatting"));
        }
    }
    None
}

// ---- making a line ------------------------------------------------------------

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).unwrap())
}

/// Python's `\s` (str.isspace), which also takes \x1c-\x1f.
const SPACE: &str = r"[\s\x1c-\x1f]";

/// Fills a phrase and tidies what an empty placeholder leaves behind
/// (audio.render): "Warning. Jimmy. . Piracy" -> "Warning. Jimmy. Piracy".
pub fn render(template: &str, words: &dyn Fn(&str) -> String) -> String {
    if template.is_empty() {
        return String::new();
    }
    static UNIT: OnceLock<Regex> = OnceLock::new();
    static RUNS: OnceLock<Regex> = OnceLock::new();
    static BEFORE: OnceLock<Regex> = OnceLock::new();
    static REPEATS: OnceLock<Regex> = OnceLock::new();
    let mut template = template.to_string();
    // "{range} kilometres" written before {distance} existed: with no
    // range, the unit has nothing to measure and goes too.
    if words("range").is_empty() {
        let unit = re(
            &UNIT,
            &format!(r"(?i)\{{range\}}{SPACE}*(kilomet(?:re|er)s?|km|klicks?)\b"),
        );
        template = unit.replace_all(&template, "{range}").into_owned();
    }
    let Some(text) = format::fill(&template, words) else {
        return String::new();
    };
    let text = re(&RUNS, &format!("{SPACE}+")).replace_all(&text, " ");
    let text = re(&BEFORE, &format!("{SPACE}+([.,;:])")).replace_all(&text, "$1");
    let text = re(&REPEATS, &format!("([.,;:])(?:{SPACE}*[.,;:])+")).replace_all(&text, "$1");
    text.trim_matches([' ', '.', ',', ';', ':']).to_string()
}

/// ({range}, {distance}) for a range in km: "2.4", "2.4 kilometres"; one
/// kilometre is singular; nothing without a range.
pub fn distance_words(range_km: Option<f64>) -> (String, String) {
    let Some(r) = range_km else {
        return (String::new(), String::new());
    };
    let s = format!("{r:.1}");
    let num = s.trim_end_matches('0').trim_end_matches('.').to_string();
    let unit = if num == "1" {
        "kilometre"
    } else {
        "kilometres"
    };
    let dist = format!("{num} {unit}");
    (num, dist)
}

/// Python's `str.title()`: a letter after a cased letter is lowered, any
/// other letter raised.
pub fn title(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_cased = false;
    for c in s.chars() {
        if prev_cased {
            out.extend(c.to_lowercase());
        } else {
            out.extend(c.to_uppercase());
        }
        prev_cased = c.is_lowercase() || c.is_uppercase();
    }
    out
}

/// Python's `str.isspace()` for one character.
fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// A handle as text-to-speech can say it: separators become spaces, a run
/// with at most one vowel (longer than three letters) is spelled out, the
/// rest title-cased.
pub fn speakable(handle: &str) -> String {
    if handle.is_empty() {
        return String::new();
    }
    let s = handle.replace(['_', '-'], " ");
    let words: Vec<String> = s
        .split(is_space)
        .filter(|w| !w.is_empty())
        .map(|w| {
            let vowels = w
                .to_uppercase()
                .chars()
                .filter(|c| "AEIOUY".contains(*c))
                .count();
            if w.chars().count() > 3 && vowels <= 1 {
                w.chars().map(String::from).collect::<Vec<_>>().join(" ")
            } else {
                title(w)
            }
        })
        .collect();
    words.join(" ")
}

/// An org's or person's name as given: spaced words are left alone, a
/// run-together SID is treated as a handle is.
pub fn speakable_name(name: &str) -> String {
    let name = name.trim_matches(is_space);
    if name.contains(' ') {
        name.to_string()
    } else {
        speakable(name)
    }
}

/// One lookup flag string ("PIRACY: SID (40 members)", "Syndicate: SID",
/// "new account (12d)") -> its phrase key (None for a kind with no
/// phrase), the values its phrase can use, and words to say without one.
fn flag_parts(f: &str) -> (Option<&'static str>, Vec<(&'static str, String)>, String) {
    static FLAG: OnceLock<Regex> = OnceLock::new();
    static DAYS: OnceLock<Regex> = OnceLock::new();
    if f.to_lowercase().starts_with("new account") {
        let days = re(&DAYS, r"(\d+)\s*d\b")
            .captures(f)
            .map_or(String::new(), |m| m[1].to_string());
        return (
            Some("flag_new_account"),
            vec![("days", days)],
            "New account".into(),
        );
    }
    let m = re(
        &FLAG,
        &format!(r"^{SPACE}*([A-Za-z][A-Za-z ]*?){SPACE}*:{SPACE}*([A-Za-z0-9_-]+)"),
    )
    .captures(f);
    let kind_raw = match &m {
        Some(m) => m[1].to_string(),
        None => f.split(':').next().unwrap_or("").to_string(),
    };
    let kind = kind_raw.trim_matches(is_space).to_string();
    let key = format!("flag_{}", kind.to_lowercase().replace(' ', "_"));
    let key = phrase(&key).map(|p| p.key);
    let org = m.as_ref().map_or(String::new(), |m| speakable(&m[2]));
    (key, vec![("org", org)], title(&kind))
}

/// The flags whose phrase is switched on.
pub fn live_flags<'a>(flags: &'a [String], muted: &[String]) -> Vec<&'a String> {
    flags
        .iter()
        .filter(|f| {
            flag_parts(f)
                .0
                .is_none_or(|k| !muted.iter().any(|m| m == k))
        })
        .collect()
}

fn lookup<'a>(speech: &'a [(String, String)], key: &str) -> &'a str {
    speech
        .iter()
        .find(|(k, _)| k == key)
        .map_or("", |(_, v)| v.as_str())
}

fn table(vals: Vec<(&'static str, String)>) -> impl Fn(&str) -> String {
    move |name: &str| {
        vals.iter()
            .find(|(k, _)| *k == name)
            .map_or(String::new(), |(_, v)| v.clone())
    }
}

/// The {flags} of a lookup warning, each flag through its phrase; one
/// phrase said once; a flag whose phrase is off not said at all.
pub fn flag_words(flags: &[String], speech: &[(String, String)], muted: &[String]) -> String {
    let mut out: Vec<String> = Vec::new();
    for f in live_flags(flags, muted) {
        let (key, vals, plain) = flag_parts(f);
        let said = match key {
            Some(k) => render(lookup(speech, k), &table(vals)),
            None => plain,
        };
        if !said.is_empty() && !out.contains(&said) {
            out.push(said);
        }
    }
    out.join(", ")
}

/// What kind of record makes a contact known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReasonKind {
    Report,
    Shared,
    Org,
    Former,
    Watchlist,
    /// A kind with no phrase (said by nothing).
    Other,
}

/// One reason a contact is known (Assessment.spoken_all()'s entries).
#[derive(Clone, Debug, PartialEq)]
pub struct Reason {
    pub kind: ReasonKind,
    pub label: String,
    pub count: Option<i64>,
    pub source: String,
    pub org: String,
}

impl Reason {
    /// One of Assessment.spoken_all()'s entries:
    /// {"kind", "label", "count", "source", "org"}.
    pub fn from_value(v: &serde_json::Value) -> Reason {
        let text = |k: &str| {
            v.get(k)
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        Reason {
            kind: match v.get("kind").and_then(serde_json::Value::as_str) {
                Some("report") => ReasonKind::Report,
                Some("shared") => ReasonKind::Shared,
                Some("org") => ReasonKind::Org,
                Some("former") => ReasonKind::Former,
                Some("watchlist") => ReasonKind::Watchlist,
                _ => ReasonKind::Other,
            },
            label: text("label"),
            count: v.get("count").and_then(serde_json::Value::as_i64),
            source: text("source"),
            org: text("org"),
        }
    }
}

/// Which phrase speaks one reason.
pub fn reason_key(r: &Reason) -> Option<&'static str> {
    match r.kind {
        ReasonKind::Report => Some(if r.count.filter(|&c| c != 0).unwrap_or(1) > 1 {
            "reason_reports"
        } else {
            "reason_report"
        }),
        ReasonKind::Shared => Some("reason_shared"),
        ReasonKind::Org => Some("reason_org"),
        ReasonKind::Former => Some("reason_former"),
        ReasonKind::Watchlist => Some("reason_watchlist"),
        ReasonKind::Other => None,
    }
}

/// The first reason whose phrase is on (they come strongest first).
pub fn pick_reason<'a>(said: &'a [Reason], muted: &[String]) -> Option<&'a Reason> {
    said.iter()
        .find(|r| reason_key(r).is_some_and(|k| !muted.iter().any(|m| m == k)))
}

/// The {reason} of a known-contact line, from structured reasons.
pub fn reason_words(reasons: &[Reason], speech: &[(String, String)], muted: &[String]) -> String {
    let Some(r) = pick_reason(reasons, muted) else {
        return String::new();
    };
    let vals = vec![
        ("label", r.label.clone()),
        ("count", r.count.map_or(String::new(), |c| c.to_string())),
        ("source", speakable_name(&r.source)),
        ("org", speakable_name(&r.org)),
    ];
    render(lookup(speech, reason_key(r).unwrap()), &table(vals))
}

/// The facts an alert line can use.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facts {
    pub handle: String,
    pub range_km: Option<f64>,
    pub flags: Vec<String>,
    /// Why the contact is known, strongest first (preferred over `reason`).
    pub said: Vec<Reason>,
    /// An older caller's console line for the same, said as it is.
    pub reason: String,
    pub count: Option<usize>,
    pub flagged: Option<usize>,
}

/// One event's utterance (audio.render_line).
pub fn render_line(
    template: &str,
    facts: &Facts,
    speech: &[(String, String)],
    speak_handles: bool,
    muted: &[String],
) -> String {
    let handle = if speak_handles {
        facts.handle.as_str()
    } else {
        "contact"
    };
    let (range, distance) = distance_words(facts.range_km);
    let reason = if !facts.said.is_empty() {
        reason_words(&facts.said, speech, muted)
    } else {
        facts.reason.clone()
    };
    let vals = vec![
        ("handle", speakable(handle)),
        ("range", range),
        ("distance", distance),
        ("flags", flag_words(&facts.flags, speech, muted)),
        ("reason", reason),
        (
            "count",
            facts.count.map_or(String::new(), |c| c.to_string()),
        ),
        (
            "flagged",
            facts.flagged.map_or(String::new(), |c| c.to_string()),
        ),
    ];
    render(template, &table(vals))
}

/// Words per minute -> SAPI's -10..10, on the curve pyttsx3 used (175 -> 1).
pub fn sapi_rate(wpm: f64) -> i32 {
    if wpm <= 0.0 || !wpm.is_finite() {
        return 0;
    }
    let v = (wpm / 156.63).ln() / 1.11f64.ln();
    (v.trunc() as i64).clamp(-10, 10) as i32
}

// ---- the settings page's "hear it" -------------------------------------------------

fn report(count: i64) -> Reason {
    Reason {
        kind: ReasonKind::Report,
        label: "Unprovoked attack".into(),
        count: Some(count),
        source: String::new(),
        org: String::new(),
    }
}

fn reason(kind: ReasonKind, label: &str, source: &str, org: &str) -> Reason {
    Reason {
        kind,
        label: label.into(),
        count: None,
        source: source.into(),
        org: org.into(),
    }
}

/// What "hear it" plays for each phrase: the whole line it would be part
/// of, with MADE-UP names - every one of these lines is an accusation.
pub fn sample_facts(key: &str) -> Facts {
    let pilot = |flags: &[&str], said: Vec<Reason>| Facts {
        handle: "SAMPLE_PILOT".into(),
        range_km: Some(2.4),
        flags: flags.iter().map(|f| f.to_string()).collect(),
        said,
        ..Default::default()
    };
    match key {
        "flagged" => pilot(
            &["PIRACY: EXMPLORG (40 members)", "new account (12d)"],
            vec![],
        ),
        "known" | "reason_report" => pilot(&[], vec![report(1)]),
        "reason_reports" => pilot(&[], vec![report(3)]),
        "reason_shared" => pilot(
            &[],
            vec![reason(
                ReasonKind::Shared,
                "Camping a location",
                "Orgmate",
                "",
            )],
        ),
        "reason_org" => pilot(&[], vec![reason(ReasonKind::Org, "", "", "Example Org")]),
        "reason_former" => pilot(&[], vec![reason(ReasonKind::Former, "", "", "Example Org")]),
        "reason_watchlist" => pilot(&[], vec![reason(ReasonKind::Watchlist, "", "", "")]),
        "flag_piracy" => pilot(&["PIRACY: EXMPLORG (40 members)"], vec![]),
        "flag_syndicate" => pilot(&["Syndicate: EXMPLORG"], vec![]),
        "flag_smuggling" => pilot(&["Smuggling: EXMPLORG"], vec![]),
        "flag_infiltration" => pilot(&["Infiltration: EXMPLORG"], vec![]),
        "flag_new_account" => pilot(&["new account (12d)"], vec![]),
        "summary" => Facts {
            count: Some(6),
            flagged: Some(2),
            ..Default::default()
        },
        _ => Facts::default(),
    }
}

/// How `text` would sound as phrase `key`, in the line it belongs to.
pub fn sample(key: &str, text: &str, speech: &[(String, String)], speak_handles: bool) -> String {
    let mut trial = speech.to_vec();
    match trial.iter_mut().find(|(k, _)| k == key) {
        Some(e) => e.1 = text.to_string(),
        None => trial.push((key.to_string(), text.to_string())),
    }
    let line = match phrase(key).map(|p| p.group) {
        Some(Group::Reason) => "known",
        Some(Group::Flag) => "flagged",
        _ => key,
    };
    let template = lookup(&trial, line).to_string();
    render_line(&template, &sample_facts(key), &trial, speak_handles, &[])
}
