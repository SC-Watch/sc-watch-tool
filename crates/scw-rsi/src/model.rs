//! A citizen and an org as rsi.py stores them, and what is said about them.

use std::sync::OnceLock;

use chrono::{NaiveDate, NaiveDateTime, TimeZone};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::lookup_url;

pub const PIRACY_FOCUS: &[&str] = &["piracy"];
pub const ADJACENT_FOCUS: &[&str] = &["smuggling", "infiltration"];
pub const PIRACY_ARCHETYPE: &[&str] = &["syndicate"];

/// RSI's fixed list of org focuses.
pub const FOCUSES: &[&str] = &[
    "Bounty Hunting",
    "Engineering",
    "Exploration",
    "Freelancing",
    "Infiltration",
    "Medical",
    "Piracy",
    "Resources",
    "Scouting",
    "Security",
    "Smuggling",
    "Social",
    "Trading",
    "Transport",
];

/// An org, and a person's place in it (`is_main`, `rank`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Org {
    pub sid: String,
    pub name: String,
    pub members: i64,
    pub archetype: String,
    pub commitment: String,
    pub primary_focus: String,
    pub secondary_focus: String,
    pub is_main: bool,
    pub rank: String,
    pub redacted: bool,
}

impl Org {
    pub fn new(sid: &str) -> Self {
        Org {
            sid: sid.into(),
            ..Default::default()
        }
    }

    pub fn focuses(&self) -> Vec<&str> {
        [self.primary_focus.as_str(), self.secondary_focus.as_str()]
            .into_iter()
            .filter(|f| !f.is_empty())
            .collect()
    }

    pub fn piracy(&self) -> bool {
        self.focuses()
            .iter()
            .any(|f| PIRACY_FOCUS.contains(&f.to_lowercase().as_str()))
    }

    pub fn adjacent(&self) -> Vec<String> {
        let mut hits: Vec<String> = self
            .focuses()
            .into_iter()
            .filter(|f| ADJACENT_FOCUS.contains(&f.to_lowercase().as_str()))
            .map(String::from)
            .collect();
        if PIRACY_ARCHETYPE.contains(&self.archetype.to_lowercase().as_str()) {
            hits.push(self.archetype.clone());
        }
        hits
    }
}

/// Where an answer came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Sentry,
    Sci,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Sentry => "sentry",
            Source::Sci => "sci",
        }
    }
}

/// What a lookup says about its own answer, beside the profile - the
/// attributes rsi.py sets on a Citizen. Never stored.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Note {
    /// When the profile was read from RSI; None = now.
    pub stored_at: Option<f64>,
    /// Why it is not a complete, current answer.
    pub stale_reason: Option<String>,
    pub source: Option<Source>,
    /// Worth asking again in minutes: SENTRY is reading something for it.
    pub awaiting_sentry: bool,
    /// Orgs on it whose focus nobody could give.
    pub unknown_focus: Vec<String>,
    /// SENTRY is checking RSI for a name it has never seen: its state.
    pub checking: Option<String>,
    /// ... and how long it said to wait.
    pub retry_in: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Citizen {
    pub handle: String,
    pub exists: bool,
    pub moniker: String,
    pub enlisted: String,
    pub location: String,
    pub record: String,
    pub orgs: Vec<Org>,
    pub redacted: bool,
    /// Seen, but no backend was available to check it.
    pub unverified: bool,
    pub fetched_at: f64,
    #[serde(skip)]
    pub note: Note,
}

impl Default for Citizen {
    fn default() -> Self {
        Citizen {
            handle: String::new(),
            exists: true,
            moniker: String::new(),
            enlisted: String::new(),
            location: String::new(),
            record: String::new(),
            orgs: Vec::new(),
            redacted: false,
            unverified: false,
            fetched_at: 0.0,
            note: Note::default(),
        }
    }
}

impl Citizen {
    pub fn new(handle: &str) -> Self {
        Citizen {
            handle: handle.into(),
            ..Default::default()
        }
    }

    /// Days since enlisting, from `now`; `utc` reads the date as UTC
    /// rather than local time (tests).
    pub fn enlisted_days(&self, now: f64, utc: bool) -> Option<i64> {
        let epoch = enlisted_epoch(&self.enlisted, utc)?;
        Some(((now - epoch) / 86400.0) as i64)
    }

    pub fn summary(&self, now: f64, utc: bool) -> String {
        if self.unverified {
            return format!("not checked - {}", lookup_url(&self.handle));
        }
        if !self.exists {
            return "no profile (NPC or bad read)".into();
        }
        let mut bits = Vec::new();
        if let Some(age) = self.enlisted_days(now, utc) {
            bits.push(if age < 365 {
                format!("enlisted {age}d ago")
            } else {
                format!("enlisted {}y ago", age.div_euclid(365))
            });
        }
        for o in &self.orgs {
            let mut marks = if o.piracy() {
                vec!["PIRACY".to_string()]
            } else {
                vec![]
            };
            marks.extend(o.adjacent());
            bits.push(if marks.is_empty() {
                o.sid.clone()
            } else {
                format!("{}[{}]", o.sid, marks.join("/"))
            });
        }
        if self.redacted {
            bits.push("+REDACTED".into());
        } else if self.orgs.is_empty() {
            bits.push("no org".into());
        }
        bits.join(" | ")
    }

    pub fn flags(&self, now: f64, utc: bool) -> Vec<String> {
        let mut out = Vec::new();
        for o in &self.orgs {
            if o.piracy() {
                out.push(format!("PIRACY: {} ({} members)", o.sid, o.members));
            }
        }
        for o in &self.orgs {
            for a in o.adjacent() {
                out.push(format!("{a}: {}", o.sid));
            }
        }
        if let Some(age) = self.enlisted_days(now, utc).filter(|a| *a < 90) {
            out.push(format!("new account ({age}d)"));
        }
        out
    }
}

// ---- dates ----------------------------------------------------------------------

/// An enlistment date as epoch seconds - `_enlisted_epoch()`: the first of
/// strptime's formats that reads the whole text, as local time (`utc` for
/// tests); the fraction of a second is dropped, as mktime drops it.
pub fn enlisted_epoch(text: &str, utc: bool) -> Option<f64> {
    if text.is_empty() {
        return None;
    }
    static FORMATS: OnceLock<Vec<Regex>> = OnceLock::new();
    // strptime's own patterns for %Y %m %d %H %M %S %f %b %B; its regex is
    // case-insensitive, and a space in a format matches any whitespace.
    // ASCII digits only: strptime also takes other scripts' digits, which
    // no enlistment date is written in.
    let formats = FORMATS.get_or_init(|| {
        const Y: &str = r"(?P<Y>[0-9]{4})";
        const M: &str = r"(?P<m>1[0-2]|0[1-9]|[1-9])";
        const D: &str = r"(?P<d>3[01]|[12][0-9]|0[1-9]|[1-9]| [1-9])";
        const T: &str = r"T(?P<H>2[0-3]|[0-1][0-9]|[0-9]):(?P<M>[0-5][0-9]|[0-9]):(?P<S>6[0-1]|[0-5][0-9]|[0-9])";
        const B: &str = "(?P<b>jan|feb|mar|apr|may|jun|jul|aug|sep|oct|nov|dec)";
        const BB: &str = "(?P<b>january|february|march|april|may|june|july|august|september|october|november|december)";
        [
            format!(r"{Y}-{M}-{D}{T}\.(?P<f>[0-9]{{1,6}})"),
            format!(r"{Y}-{M}-{D}{T}"),
            format!(r"{Y}-{M}-{D}"),
            format!(r"{B}\s+{D},\s+{Y}"),
            format!(r"{BB}\s+{D},\s+{Y}"),
        ]
        .iter()
        .map(|p| Regex::new(&format!("(?i)^(?:{p})")).unwrap())
        .collect()
    });
    for re in formats {
        let Some(m) = re.captures(text) else { continue };
        if m.get(0).unwrap().end() != text.len() {
            continue; // "unconverted data remains"
        }
        let num = |n: &str| {
            m.name(n)
                .map(|v| v.as_str().trim().parse::<u32>().unwrap())
                .unwrap_or(0)
        };
        let month = match m.name("b") {
            Some(b) => {
                let b = b.as_str().to_lowercase();
                [
                    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov",
                    "dec",
                ]
                .iter()
                .position(|p| b.starts_with(p))
                .unwrap() as u32
                    + 1
            }
            None => num("m"),
        };
        let Some(date) = NaiveDate::from_ymd_opt(num("Y") as i32, month, num("d")) else {
            continue;
        };
        // A leap second (:60, :61) is the next minute, as mktime takes it.
        let secs = num("H") as i64 * 3600 + num("M") as i64 * 60 + num("S") as i64;
        let naive = date.and_hms_opt(0, 0, 0).unwrap() + chrono::Duration::seconds(secs);
        return Some(epoch_of(naive, utc) as f64);
    }
    None
}

/// A wall-clock time as epoch seconds: UTC, or local as mktime reads it.
fn epoch_of(naive: NaiveDateTime, utc: bool) -> i64 {
    if utc {
        return naive.and_utc().timestamp();
    }
    match chrono::Local.from_local_datetime(&naive) {
        chrono::LocalResult::Single(t) => t.timestamp(),
        chrono::LocalResult::Ambiguous(a, _) => a.timestamp(),
        // In a clock change's gap: an hour on, where it lands.
        chrono::LocalResult::None => chrono::Local
            .from_local_datetime(&(naive + chrono::Duration::hours(1)))
            .earliest()
            .map(|t| t.timestamp())
            .unwrap_or_else(|| naive.and_utc().timestamp()),
    }
}

/// A SENTRY timestamp - ISO, in UTC when it does not say - as epoch seconds,
/// or None - `_parse_utc()`. Python's `datetime.fromisoformat` (3.13) for
/// the extended and basic calendar forms; ISO week dates are not read.
pub fn parse_utc(v: Option<&Value>) -> Option<f64> {
    if !scw_store::json::truthy(v) {
        return None;
    }
    let text = match v? {
        Value::String(s) => s.clone(),
        Value::Array(_) | Value::Object(_) => return None,
        other => scw_store::json::py_str(other),
    };
    let us = fromisoformat(&text.replace('Z', "+00:00"))?;
    Some(us as f64 / 1e6)
}

fn digits(s: &str) -> Option<u32> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// Microseconds since the epoch.
fn fromisoformat(s: &str) -> Option<i64> {
    if !s.is_ascii() || s.len() < 7 {
        return None;
    }
    let b = s.as_bytes();
    // The separator's place (`_find_isoformat_datetime_separator`), for
    // the calendar forms: YYYY-MM-DD is 10 long, YYYYMMDD 8.
    if b.get(4) == Some(&b'W') || b.get(5) == Some(&b'W') || s.len() == 7 {
        return None;
    }
    let sep = if b[4] == b'-' { 10 } else { 8 };
    let date = s.get(..sep)?;
    let (y, m, d) = if sep == 10 {
        if date.as_bytes()[7] != b'-' {
            return None;
        }
        (
            digits(&date[..4])?,
            digits(&date[5..7])?,
            digits(&date[8..10])?,
        )
    } else {
        if date.contains('-') {
            return None;
        }
        (
            digits(&date[..4])?,
            digits(&date[4..6])?,
            digits(&date[6..8])?,
        )
    };
    if y == 0 {
        return None;
    }
    let day = NaiveDate::from_ymd_opt(y as i32, m, d)?;
    let (mut secs, mut micros) = (0i64, 0i64);
    // Any character after the date is the separator; something after it,
    // even nothing, is a time.
    if s.len() > sep {
        let ([h, mi, sec, us], off) = parse_time(&b[sep + 1..])?;
        if h > 23 || mi > 59 || sec > 59 || off.abs() >= 86_400 * 1_000_000 {
            return None;
        }
        secs = h * 3600 + mi * 60 + sec;
        micros = us - off;
    }
    let days = day
        .signed_duration_since(NaiveDate::from_ymd_opt(1970, 1, 1)?)
        .num_days();
    Some((days * 86400 + secs) * 1_000_000 + micros)
}

/// The time and its zone offset in microseconds - CPython's C
/// `parse_isoformat_time`, which is what runs (the pure-Python fallback
/// differs in corners: it refuses "12.5", accepts a bare separator).
fn parse_time(t: &[u8]) -> Option<([i64; 4], i64)> {
    let tz = t
        .iter()
        .position(|c| matches!(c, b'Z' | b'+' | b'-'))
        .unwrap_or(t.len());
    let (v, trailing) = hh_mm_ss_ff(t, 0, tz)?;
    if tz == t.len() {
        return (!trailing).then_some((v, 0));
    }
    if t[tz] == b'Z' {
        return (tz + 1 == t.len()).then_some((v, 0));
    }
    let sign = if t[tz] == b'-' { -1 } else { 1 };
    let (z, trailing) = hh_mm_ss_ff(t, tz + 1, t.len())?;
    if trailing {
        return None;
    }
    Some((
        v,
        sign * ((z[0] * 3600 + z[1] * 60 + z[2]) * 1_000_000 + z[3]),
    ))
}

/// HH[:?MM[:?SS[{.,}f+]]] between `start` and `end` - the C
/// `parse_hh_mm_ss_ff`, reading past `end` the way it does (into the
/// zone's sign, or the end of the text). Returns the parts, and whether
/// something other than the end of the text follows.
fn hh_mm_ss_ff(t: &[u8], start: usize, end: usize) -> Option<([i64; 4], bool)> {
    let at = |i: usize| t.get(i).copied().unwrap_or(0);
    let two = |i: usize| {
        (at(i).is_ascii_digit() && at(i + 1).is_ascii_digit())
            .then(|| ((at(i) - b'0') * 10 + (at(i + 1) - b'0')) as i64)
    };
    let mut v = [0i64; 4];
    let mut p = start;
    let mut has_sep = true;
    let mut i = 0;
    while i < 3 {
        v[i] = two(p)?;
        p += 2;
        let c = at(p);
        p += 1;
        if i == 0 {
            has_sep = c == b':';
        }
        if p >= end {
            // Measured on 3.13: a decimal mark with no digit after it is
            // refused, anything else there is "something follows".
            if c == b'.' || c == b',' {
                return None;
            }
            return Some((v, c != 0));
        }
        if has_sep && c == b':' {
            i += 1;
            continue;
        }
        if c == b'.' || c == b',' {
            break;
        }
        if has_sep {
            return None; // a malformed separator
        }
        p -= 1;
        i += 1;
    }
    let n = (end - p).min(6);
    let mut us = 0i64;
    for k in 0..n {
        let c = at(p + k);
        if !c.is_ascii_digit() {
            return None;
        }
        us = us * 10 + (c - b'0') as i64;
    }
    v[3] = us * 10i64.pow(6 - n as u32);
    p += n;
    while at(p).is_ascii_digit() {
        p += 1;
    }
    Some((v, at(p) != 0))
}
