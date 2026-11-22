//! Sharing your records with someone you trust - an org mate - and taking
//! theirs: share.py.
//!
//! An export holds your own reports, the reasons they use, the orgs you
//! flagged and the roster members you added by hand, under the name you
//! chose. Never sightings, images, settings, keys, watchlists, or anything
//! you imported from someone else. An imported report keeps who said it
//! (`source`) and where it came from (`origin` = "<install>:<their id>"), so
//! the author's next export updates it, and everything from one person can
//! be removed in one go.
//!
//! An export is untrusted input: only share.json is read, its declared size
//! is checked before it is decompressed, and every field is type-checked,
//! capped and pattern-checked before anything is written. A file that fails
//! is refused whole.
//!
//! Deliberately stricter than share.py: a value ending in a newline does not
//! pass a pattern (Python's `$` lets one through), `true` is not a version
//! (Python's bool is an int), and a share holding an infinite number is
//! refused as damaged.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{Cursor, Read, Write};
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::flags::{self, Flags, MAX_LABEL, MAX_SAY, POLARITIES, UNKNOWN_COLOR};
use crate::json::{dumps_indent, loads, py_isspace, py_repr_str, py_str, py_strip, truthy};
use crate::reputation::{Database, PENDING};

pub const FORMAT: &str = "sc-watch-share";
pub const VERSION: i64 = 1;
pub const MEMBER: &str = "share.json";
/// The file as sent.
pub const MAX_ZIP: usize = 12 * 1024 * 1024;
/// share.json once decompressed.
pub const MAX_JSON: u64 = 16 * 1024 * 1024;
pub const MAX_ROWS: usize = 50_000;
pub const MAX_NAME: usize = 40;
/// How an org flag from an import is marked.
pub const SHARED_NOTE: &str = "shared by ";

pub const README: &str = "This is an sc-watch share file: somebody's own records, for an org mate.

Import it in sc-watch: Settings -> Sharing -> Import. You will see what is in
it before anything is added, and everything from this person can be removed
again in one click.

It holds their reports (who, why, their note, when), the orgs they flagged,
and roster members they added by hand. It does NOT hold where or when they
played, screenshots, settings, API keys, or anything they imported from
someone else.
";

struct Patterns {
    handle: Regex,
    sid: Regex,
    install: Regex,
    key: Regex,
    color: Regex,
    unsafe_name: Regex,
}

fn re() -> &'static Patterns {
    static P: OnceLock<Patterns> = OnceLock::new();
    // `$` is the end of the text here - no trailing newline slips through.
    P.get_or_init(|| Patterns {
        handle: Regex::new(r"^[A-Za-z0-9_-]{2,40}$").unwrap(),
        sid: Regex::new(r"^[A-Za-z0-9_-]{1,32}$").unwrap(),
        install: Regex::new(r"^[0-9a-f]{32}$").unwrap(),
        key: Regex::new(r"^[a-z0-9_]{1,40}$").unwrap(),
        color: Regex::new(r"^#[0-9a-fA-F]{6}$").unwrap(),
        unsafe_name: Regex::new(r"[^A-Za-z0-9_-]+").unwrap(),
    })
}

fn is_control(c: char) -> bool {
    (c as u32) < 0x20 || c == '\x7f'
}

/// A share that cannot be used, with a reason a person can act on - or the
/// database failing underneath.
#[derive(Debug)]
pub enum ShareError {
    Refused(String),
    Db(rusqlite::Error),
    Flags(String),
}

impl std::fmt::Display for ShareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShareError::Refused(m) => f.write_str(m),
            ShareError::Db(e) => write!(f, "the database: {e}"),
            ShareError::Flags(e) => write!(f, "flags.json: {e}"),
        }
    }
}

impl std::error::Error for ShareError {}

impl From<rusqlite::Error> for ShareError {
    fn from(e: rusqlite::Error) -> Self {
        ShareError::Db(e)
    }
}

fn refuse<T>(m: impl Into<String>) -> Result<T, ShareError> {
    Err(ShareError::Refused(m.into()))
}

// ---- the document ----------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SharedReport {
    pub id: i64,
    pub handle: String,
    pub ts: f64,
    pub category: String,
    pub note: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SharedFlag {
    pub key: String,
    pub label: String,
    pub color: String,
    pub polarity: String,
    pub say: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SharedOrg {
    pub sid: String,
    pub name: String,
    pub note: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SharedMember {
    pub sid: String,
    pub handle: String,
    pub note: String,
    pub ts: f64,
}

/// An export, as built or as checked.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Share {
    pub format: String,
    pub version: i64,
    pub from: String,
    pub install: String,
    pub exported_at: f64,
    pub reports: Vec<SharedReport>,
    pub flags: Vec<SharedFlag>,
    pub orgs: Vec<SharedOrg>,
    pub roster: Vec<SharedMember>,
}

/// A name as shown: controls removed, whitespace collapsed, at most
/// MAX_NAME characters - `clean_name(raw)`, which reads `str(raw or "")`.
pub fn clean_name(raw: Option<&Value>) -> String {
    let text = if truthy(raw) {
        py_str(raw.unwrap())
    } else {
        String::new()
    };
    let kept: String = text.chars().filter(|c| !is_control(*c)).collect();
    let mut out = String::new();
    let mut in_space = false;
    for c in py_strip(&kept).chars() {
        if py_isspace(c) {
            if !in_space {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(c);
            in_space = false;
        }
    }
    out.chars().take(MAX_NAME).collect()
}

/// "self" is what YOUR reports carry as their source.
fn reserved(name: &str) -> bool {
    py_strip(name).to_lowercase() == "self"
}

/// This install's id (32 hex), made once and kept in `meta`.
pub fn install_id(db: &Database) -> Result<String, ShareError> {
    let have: Option<Option<String>> = db
        .conn
        .query_row("SELECT value FROM meta WHERE key='install'", [], |r| {
            r.get(0)
        })
        .optional()?;
    if let Some(v) = have.flatten().filter(|v| re().install.is_match(v)) {
        return Ok(v);
    }
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|e| ShareError::Refused(format!("no randomness: {e}")))?;
    b[6] = (b[6] & 0x0f) | 0x40; // a version 4 UUID, as uuid4().hex
    b[8] = (b[8] & 0x3f) | 0x80;
    let iid: String = b.iter().map(|x| format!("{x:02x}")).collect();
    db.conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('install', ?)",
        [&iid],
    )?;
    Ok(iid)
}

// ---- export ---------------------------------------------------------------------

/// The export: your own records only, with the reasons as `db.flags` defines
/// them.
pub fn build(db: &Database, name: &str) -> Result<Share, ShareError> {
    let name = clean_name(Some(&Value::String(name.into())));
    if name.is_empty() {
        return refuse("give a name to be shown as - your in-game name is best");
    }
    if reserved(&name) {
        return refuse("\"self\" cannot be used as a name - your in-game name is best");
    }
    // origin IS NULL as well: an imported report is never yours to re-share.
    let reports: Vec<SharedReport> = db
        .conn
        .prepare(
            "SELECT id, handle, ts, category, note FROM reports
              WHERE COALESCE(source,'self')='self' AND origin IS NULL
                AND category!=?
              ORDER BY id",
        )?
        .query_map([PENDING], |r| {
            Ok(SharedReport {
                id: r.get(0)?,
                handle: r.get(1)?,
                ts: r.get(2)?,
                category: r.get(3)?,
                note: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            })
        })?
        .collect::<Result<_, _>>()?;
    let used: BTreeSet<&str> = reports.iter().map(|r| r.category.as_str()).collect();
    let flags = used
        .iter()
        .map(|key| {
            let f = db.flags.by_key(key);
            SharedFlag {
                key: key.to_string(),
                label: f.label,
                color: f.color,
                polarity: f.polarity,
                say: f.say,
            }
        })
        .collect();
    // Not the orgs an import flagged ("shared by <name>").
    let orgs = db
        .flagged_orgs()?
        .into_iter()
        .filter(|o| !o.note.starts_with(SHARED_NOTE))
        .map(|o| SharedOrg {
            sid: o.sid,
            name: o.name,
            note: o.note,
        })
        .collect();
    let roster = db
        .conn
        .prepare(
            "SELECT m.sid, m.handle, m.note, m.manual_ts
               FROM org_membership m JOIN org_flags f ON f.sid = m.sid
              WHERE m.manual_ts IS NOT NULL AND m.manual_src IS NULL
                AND m.gone_since IS NULL
              ORDER BY m.sid, m.handle",
        )?
        .query_map([], |r| {
            Ok(SharedMember {
                sid: r.get(0)?,
                handle: r.get(1)?,
                note: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                ts: r.get(3)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(Share {
        format: FORMAT.into(),
        version: VERSION,
        from: name,
        install: install_id(db)?,
        exported_at: db.now(),
        reports,
        flags,
        orgs,
        roster,
    })
}

/// share.json, as share.py writes it: `json.dumps(doc, indent=1)`.
pub fn share_json(doc: &Share) -> String {
    dumps_indent(&serde_json::to_value(doc).unwrap(), 1).unwrap_or_default()
}

/// A moment as the date and time on a calendar - local, or UTC (tests).
fn calendar(t: f64, utc: bool) -> chrono::NaiveDateTime {
    use chrono::TimeZone;
    let secs = t.floor() as i64;
    if utc {
        return chrono::DateTime::from_timestamp(secs, 0)
            .unwrap_or_default()
            .naive_utc();
    }
    chrono::Local
        .timestamp_opt(secs, 0)
        .earliest()
        .map(|d| d.naive_local())
        .unwrap_or_default()
}

/// The zip that is sent: share.json and a README, deflated, dated when the
/// export was made (local time, as zipfile dates it).
pub fn pack(doc: &Share) -> Vec<u8> {
    use chrono::{Datelike, Timelike};
    use zip::write::SimpleFileOptions;
    let at = calendar(doc.exported_at, false);
    let when = zip::DateTime::from_date_and_time(
        at.year() as u16,
        at.month() as u8,
        at.day() as u8,
        at.hour() as u8,
        at.minute() as u8,
        at.second() as u8,
    )
    .unwrap_or_default();
    let opts = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(when);
    let mut z = zip::ZipWriter::new(Cursor::new(Vec::new()));
    // Writing into memory cannot fail but for a bug.
    z.start_file(MEMBER, opts).unwrap();
    z.write_all(share_json(doc).as_bytes()).unwrap();
    z.start_file("README.txt", opts).unwrap();
    z.write_all(README.as_bytes()).unwrap();
    z.finish().unwrap().into_inner()
}

/// The file's name: sc-watch-share-<name>-<today>.zip, today as of `now`
/// (local time; `utc` for tests).
pub fn filename(doc: &Share, now: f64, utc: bool) -> String {
    let safe = re().unsafe_name.replace_all(&doc.from, "_");
    let safe = safe.trim_matches('_');
    let safe = if safe.is_empty() { "share" } else { safe };
    format!(
        "sc-watch-share-{safe}-{}.zip",
        calendar(now, utc).format("%Y%m%d")
    )
}

// ---- reading one ---------------------------------------------------------------------

const DAMAGED: &str = "that share is damaged (share.json is not readable)";

/// Bytes of a zip -> a checked document, or why not. `now` caps dates.
pub fn unpack(data: &[u8], now: f64) -> Result<Share, ShareError> {
    if data.len() > MAX_ZIP {
        return refuse("that file is far too big to be an sc-watch share");
    }
    let Ok(mut z) = zip::ZipArchive::new(Cursor::new(data)) else {
        return refuse("that is not a zip file");
    };
    let Some(i) = z.index_for_name(MEMBER) else {
        return refuse("that zip is not an sc-watch share (no share.json)");
    };
    let Ok(entry) = z.by_index_data(i) else {
        return refuse(DAMAGED);
    };
    if entry.encrypted() {
        return refuse("that share is encrypted");
    }
    if entry.size() > MAX_JSON {
        return refuse("that share is too big");
    }
    let mut raw = Vec::new();
    let Ok(f) = z.by_index(i) else {
        return refuse(DAMAGED);
    };
    if f.take(MAX_JSON + 1).read_to_end(&mut raw).is_err() {
        return refuse(DAMAGED);
    }
    if raw.len() as u64 > MAX_JSON {
        return refuse("that share is too big");
    }
    let Ok(text) = String::from_utf8(raw) else {
        return refuse(DAMAGED);
    };
    let Ok(doc) = loads(&text) else {
        return refuse(DAMAGED);
    };
    check(&doc, now)
}

/// Strict base64 (`b64decode(validate=True)`): the alphabet only, padding
/// only to complete the last group.
fn b64_strict(text: &str) -> Option<Vec<u8>> {
    let b = text.as_bytes();
    if !b.len().is_multiple_of(4) {
        return None;
    }
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let mut out = Vec::with_capacity(b.len() / 4 * 3);
    for (n, q) in b.chunks(4).enumerate() {
        let last = n == b.len() / 4 - 1;
        let pads = q.iter().rev().take_while(|c| **c == b'=').count();
        if pads > 2 || (pads > 0 && !last) {
            return None;
        }
        let mut acc = 0u32;
        for &c in &q[..4 - pads] {
            acc = (acc << 6) | val(c)?;
        }
        acc <<= 6 * pads as u32;
        let bytes = [(acc >> 16) as u8, (acc >> 8) as u8, acc as u8];
        out.extend_from_slice(&bytes[..3 - pads]);
    }
    Some(out)
}

/// A share sent through the page, as base64 text.
pub fn unpack_b64(text: &str, now: f64) -> Result<Share, ShareError> {
    match b64_strict(text) {
        Some(data) if text.is_ascii() => unpack(&data, now),
        _ => refuse("the file did not arrive intact"),
    }
}

fn num(v: Option<&Value>, what: &str) -> Result<f64, ShareError> {
    match v {
        Some(Value::Number(n)) => n.as_f64().filter(|f| f.is_finite()).map_or_else(
            || refuse(format!("damaged share: {what} is not a number")),
            Ok,
        ),
        _ => refuse(format!("damaged share: {what} is not a number")),
    }
}

fn text(v: Option<&Value>, cap: usize, what: &str) -> Result<String, ShareError> {
    match v {
        None | Some(Value::Null) => Ok(String::new()),
        Some(Value::String(s)) => {
            let spaced: String = s
                .chars()
                .map(|c| if is_control(c) { ' ' } else { c })
                .collect();
            Ok(py_strip(&spaced).chars().take(cap).collect())
        }
        Some(_) => refuse(format!("damaged share: {what} is not text")),
    }
}

/// `str(row.get(key, ""))`.
fn field(m: &Map<String, Value>, k: &str, default: &str) -> String {
    m.get(k).map_or_else(|| default.to_string(), py_str)
}

fn rows<'a>(
    doc: &'a Map<String, Value>,
    key: &str,
) -> Result<Vec<&'a Map<String, Value>>, ShareError> {
    let v = match doc.get(key) {
        None => return Ok(vec![]),
        Some(Value::Array(a)) => a,
        Some(_) => return refuse(format!("damaged share: {key} is not a list")),
    };
    if v.len() > MAX_ROWS {
        return refuse(format!("that share has too many {key}"));
    }
    v.iter()
        .map(|r| {
            r.as_object().ok_or_else(|| {
                ShareError::Refused(format!("damaged share: an entry in {key} is not a record"))
            })
        })
        .collect()
}

/// The first 24 characters, as Python's `{text[:24]!r}`.
fn shown(s: &str) -> String {
    py_repr_str(&s.chars().take(24).collect::<String>())
}

/// Validates everything, or refuses everything; returns a clean copy.
pub fn check(doc: &Value, now: f64) -> Result<Share, ShareError> {
    let Some(d) = doc
        .as_object()
        .filter(|d| d.get("format") == Some(&json!(FORMAT)))
    else {
        return refuse("that is not an sc-watch share");
    };
    let version = match d.get("version") {
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => n.as_i64().unwrap_or(i64::MAX),
        _ => return refuse("damaged share: no version"),
    };
    if version < 1 {
        return refuse("damaged share: no version");
    }
    if version > VERSION {
        return refuse("that share was made by a newer sc-watch - update to import it");
    }
    let name = clean_name(d.get("from"));
    if name.is_empty() {
        return refuse("damaged share: it does not say who it is from");
    }
    if reserved(&name) {
        return refuse("damaged share: \"self\" is not a name it can be imported under");
    }
    let install = field(d, "install", "");
    if !re().install.is_match(&install) {
        return refuse("damaged share: no install id");
    }
    let exported = num(d.get("exported_at"), "the export date")?;

    let mut reports = Vec::new();
    for r in rows(d, "reports")? {
        let (h, cat) = (field(r, "handle", ""), field(r, "category", ""));
        if !re().handle.is_match(&h) {
            return refuse(format!("damaged share: {} is not a handle", shown(&h)));
        }
        if !re().key.is_match(&cat) || cat == PENDING {
            return refuse(format!(
                "damaged share: {} is not a report reason",
                shown(&cat)
            ));
        }
        let id = match r.get("id") {
            Some(Value::Number(n)) if n.is_i64() || n.is_u64() => n.as_i64(),
            _ => None,
        };
        let Some(id) = id.filter(|i| *i >= 0) else {
            return refuse("damaged share: a report has no id");
        };
        let ts = num(r.get("ts"), "a report date")?;
        reports.push(SharedReport {
            id,
            handle: h.to_uppercase(),
            category: cat,
            ts: ts.min(now),
            note: text(r.get("note"), 500, "a note")?,
        });
    }
    if reports.iter().map(|r| r.id).collect::<HashSet<_>>().len() != reports.len() {
        return refuse("damaged share: two reports share an id");
    }

    let mut flags = Vec::new();
    for f in rows(d, "flags")? {
        let key = field(f, "key", "");
        if !re().key.is_match(&key) {
            return refuse("damaged share: a reason has a bad key");
        }
        if key == PENDING {
            continue;
        }
        let color = field(f, "color", "");
        let pol = field(f, "polarity", "negative");
        let label = text(f.get("label"), MAX_LABEL, "a reason name")?;
        flags.push(SharedFlag {
            label: if label.is_empty() { key.clone() } else { label },
            key,
            color: if re().color.is_match(&color) {
                color
            } else {
                UNKNOWN_COLOR.into()
            },
            polarity: if POLARITIES.contains(&pol.as_str()) {
                pol
            } else {
                "negative".into()
            },
            say: text(f.get("say"), MAX_SAY, "a spoken name")?,
        });
    }

    let mut orgs = Vec::new();
    for o in rows(d, "orgs")? {
        let sid = field(o, "sid", "");
        if !re().sid.is_match(&sid) {
            return refuse(format!("damaged share: {} is not an org SID", shown(&sid)));
        }
        orgs.push(SharedOrg {
            sid: sid.to_uppercase(),
            name: text(o.get("name"), 80, "an org name")?,
            note: text(o.get("note"), 200, "an org note")?,
        });
    }

    let mut roster = Vec::new();
    for m in rows(d, "roster")? {
        let (sid, h) = (field(m, "sid", ""), field(m, "handle", ""));
        if !re().sid.is_match(&sid) || !re().handle.is_match(&h) {
            return refuse("damaged share: a roster entry is not an org and a handle");
        }
        let note = text(m.get("note"), 200, "a roster note")?;
        let ts = match m.get("ts") {
            None | Some(Value::Null) => now,
            ts => num(ts, "a roster date")?.min(now),
        };
        roster.push(SharedMember {
            sid: sid.to_uppercase(),
            handle: h.to_uppercase(),
            note,
            ts,
        });
    }

    Ok(Share {
        format: FORMAT.into(),
        version,
        from: name,
        install,
        exported_at: exported.min(now),
        reports,
        flags,
        orgs,
        roster,
    })
}

// ---- what an import would do, and doing it -----------------------------------------

fn mine(db: &Database, install: &str) -> Result<bool, ShareError> {
    let v: Option<Option<String>> = db
        .conn
        .query_row("SELECT value FROM meta WHERE key='install'", [], |r| {
            r.get(0)
        })
        .optional()?;
    Ok(v.flatten().as_deref() == Some(install))
}

fn strings(db: &Database, sql: &str, p: impl rusqlite::Params) -> Result<Vec<String>, ShareError> {
    Ok(db
        .conn
        .prepare(sql)?
        .query_map(p, |r| r.get(0))?
        .collect::<Result<_, _>>()?)
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Before {
    pub name: Option<String>,
    pub imported_at: Option<f64>,
}

/// What importing a share would add, change and remove.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Preview {
    pub from: String,
    pub exported_at: f64,
    /// Their previous import, if any.
    pub before: Option<Before>,
    pub reports: usize,
    pub players: usize,
    /// The reported players you have met.
    pub met: Vec<String>,
    pub new_reports: usize,
    pub updated_reports: usize,
    pub dropped_reports: usize,
    pub orgs: usize,
    pub new_orgs: Vec<String>,
    pub roster: usize,
    /// Members of orgs flagged neither here nor in the file: nothing to put
    /// them on.
    pub roster_orphans: usize,
    pub roster_orgs: Vec<String>,
}

/// What importing `doc` would do. Writes nothing.
pub fn preview(db: &Database, doc: &Share) -> Result<Preview, ShareError> {
    if mine(db, &doc.install)? {
        return refuse("that is your own export");
    }
    let prefix = format!("{}:", doc.install);
    let have: HashSet<String> = strings(
        db,
        "SELECT origin FROM reports WHERE origin LIKE ?",
        [format!("{prefix}%")],
    )?
    .into_iter()
    .collect();
    let incoming: HashSet<String> = doc
        .reports
        .iter()
        .map(|r| format!("{prefix}{}", r.id))
        .collect();
    let handles: HashSet<&str> = doc.reports.iter().map(|r| r.handle.as_str()).collect();
    let mut met: Vec<String> = strings(db, "SELECT handle FROM players WHERE sightings > 0", [])?
        .into_iter()
        .filter(|h| handles.contains(h.as_str()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    met.dedup();
    let flagged: HashSet<String> = db.flagged_orgs()?.into_iter().map(|o| o.sid).collect();
    let before = db
        .conn
        .query_row(
            "SELECT name, imported_at FROM shares WHERE install=?",
            [&doc.install],
            |r| {
                Ok(Before {
                    name: r.get(0)?,
                    imported_at: r.get(1)?,
                })
            },
        )
        .optional()?;
    let listed: HashSet<&str> = doc.orgs.iter().map(|o| o.sid.as_str()).collect();
    Ok(Preview {
        from: doc.from.clone(),
        exported_at: doc.exported_at,
        before,
        reports: doc.reports.len(),
        players: handles.len(),
        met,
        new_reports: incoming.difference(&have).count(),
        updated_reports: incoming.intersection(&have).count(),
        dropped_reports: have.difference(&incoming).count(),
        orgs: doc.orgs.len(),
        new_orgs: doc
            .orgs
            .iter()
            .filter(|o| !flagged.contains(&o.sid))
            .map(|o| o.sid.clone())
            .collect(),
        roster: doc.roster.len(),
        roster_orphans: doc
            .roster
            .iter()
            .filter(|m| !flagged.contains(&m.sid) && !listed.contains(m.sid.as_str()))
            .count(),
        roster_orgs: doc
            .roster
            .iter()
            .map(|m| m.sid.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
    })
}

/// Reasons you do not have are added RETIRED - they render with the
/// author's name and colour but stay out of your dropdown; a reason you have
/// keeps YOUR definition. Returns the keys added. (A report reason the file
/// does not define is added in key order; share.py's order there follows a
/// set's.)
fn ensure_flags(
    db: &mut Database,
    doc: &Share,
    flags_path: &Path,
) -> Result<Vec<String>, ShareError> {
    let current = Flags::load(flags_path);
    let known: HashSet<&str> = current.all.iter().map(|f| f.key.as_str()).collect();
    let mut new: Vec<Value> = doc
        .flags
        .iter()
        .filter(|f| !known.contains(f.key.as_str()))
        .map(|f| {
            json!({"key": f.key, "label": f.label, "color": f.color,
                   "polarity": f.polarity, "say": f.say, "retired": true})
        })
        .collect();
    let new_keys: HashSet<String> = new.iter().map(|f| py_str(&f["key"])).collect();
    let used: BTreeSet<&str> = doc
        .reports
        .iter()
        .map(|r| r.category.as_str())
        .filter(|k| !known.contains(k) && !new_keys.contains(*k))
        .collect();
    new.extend(used.into_iter().map(|k| {
        json!({"key": k, "label": k.replace('_', " "), "color": UNKNOWN_COLOR,
               "polarity": "negative", "say": "", "retired": true})
    }));
    if !new.is_empty() {
        let items: Vec<Value> = current
            .all
            .iter()
            .map(|f| f.to_json())
            .chain(new.iter().cloned())
            .collect();
        let written = flags::save(&items, flags_path).map_err(ShareError::Flags)?;
        db.flags = Flags { all: written };
    }
    Ok(new.iter().map(|f| py_str(&f["key"])).collect())
}

/// What an import did.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Applied {
    pub from: String,
    pub reports_added: usize,
    pub reports_updated: usize,
    pub reports_dropped: usize,
    pub orgs_flagged: Vec<String>,
    pub roster_added: usize,
    pub roster_dropped: usize,
    pub roster_skipped: usize,
    pub reasons_added: Vec<String>,
}

fn drop_manual(
    c: &rusqlite::Connection,
    sid: &str,
    handle: &str,
    src: Option<&str>,
) -> rusqlite::Result<()> {
    if src.unwrap_or("sci") == "manual" {
        c.execute(
            "DELETE FROM org_membership WHERE sid=? AND handle=?",
            [sid, handle],
        )?;
        c.execute(
            "DELETE FROM player_orgs WHERE sid=? AND handle=? AND source='roster'",
            [sid, handle],
        )?;
    } else {
        c.execute(
            "UPDATE org_membership SET manual_ts=NULL, note=NULL, manual_src=NULL
              WHERE sid=? AND handle=?",
            [sid, handle],
        )?;
    }
    Ok(())
}

/// Imports. The author's previous import is REPLACED: their reports updated
/// by origin, the ones they deleted dropped, the same for their roster
/// members. Your own records are never touched. All or nothing.
pub fn apply(
    db: &mut Database,
    doc: &Share,
    flags_path: &Path,
    reports: bool,
    orgs: bool,
    roster: bool,
) -> Result<Applied, ShareError> {
    let (install, name) = (doc.install.as_str(), doc.from.as_str());
    if mine(db, install)? {
        return refuse("that is your own export");
    }
    let now = db.now();
    let mut out = Applied {
        from: name.into(),
        ..Default::default()
    };
    if reports {
        out.reasons_added = ensure_flags(db, doc, flags_path)?;
    }
    let c = &db.conn;
    let tx = c.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO shares (install, name, exported_at, imported_at)
         VALUES (?,?,?,?) ON CONFLICT(install) DO UPDATE SET
           name=excluded.name, exported_at=excluded.exported_at,
           imported_at=excluded.imported_at",
        params![install, name, doc.exported_at, now],
    )?;
    if reports {
        let prefix = format!("{install}:");
        let have: Vec<(i64, String)> = tx
            .prepare("SELECT id, origin FROM reports WHERE origin LIKE ?")?
            .query_map([format!("{prefix}%")], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let by_origin: HashMap<&str, i64> = have.iter().map(|(id, o)| (o.as_str(), *id)).collect();
        let mut seen = HashSet::new();
        for r in &doc.reports {
            let origin = format!("{prefix}{}", r.id);
            if let Some(id) = by_origin.get(origin.as_str()) {
                tx.execute(
                    "UPDATE reports SET handle=?, ts=?, category=?, note=?, source=? WHERE id=?",
                    params![r.handle, r.ts, r.category, r.note, name, id],
                )?;
                out.reports_updated += 1;
            } else {
                // Straight into reports: a stranger someone else met is not
                // made your contact.
                tx.execute(
                    "INSERT INTO reports (handle, ts, category, note, evidence, source, origin)
                     VALUES (?,?,?,?,'',?,?)",
                    params![r.handle, r.ts, r.category, r.note, name, origin],
                )?;
                out.reports_added += 1;
            }
            seen.insert(origin);
        }
        for (id, origin) in &have {
            if !seen.contains(origin) {
                tx.execute("DELETE FROM reports WHERE id=?", [id])?;
                out.reports_dropped += 1;
            }
        }
    }
    let flagged_now = |tx: &rusqlite::Transaction| -> rusqlite::Result<HashSet<String>> {
        tx.prepare("SELECT sid FROM org_flags")?
            .query_map([], |r| r.get(0))?
            .collect()
    };
    if orgs {
        let flagged = flagged_now(&tx)?;
        for o in &doc.orgs {
            if flagged.contains(&o.sid) {
                continue;
            }
            let mut note = format!("{SHARED_NOTE}{name}");
            if !o.note.is_empty() {
                note = format!("{note}: {}", o.note);
            }
            let note: String = note.chars().take(500).collect();
            tx.execute(
                "INSERT INTO org_flags (sid, name, ts, note) VALUES (?,?,?,?)",
                params![o.sid, o.name, now, note],
            )?;
            out.orgs_flagged.push(o.sid.clone());
        }
    }
    if roster {
        let flagged = flagged_now(&tx)?;
        let theirs: Vec<(String, String, Option<String>)> = tx
            .prepare("SELECT sid, handle, src FROM org_membership WHERE manual_src=?")?
            .query_map([install], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        let mut seen = HashSet::new();
        for m in &doc.roster {
            if !flagged.contains(&m.sid) {
                out.roster_skipped += 1;
                continue;
            }
            seen.insert((m.sid.clone(), m.handle.clone()));
            let row: Option<(Option<f64>, Option<String>)> = tx
                .query_row(
                    "SELECT manual_ts, manual_src FROM org_membership WHERE sid=? AND handle=?",
                    [&m.sid, &m.handle],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            match row {
                None => {
                    tx.execute(
                        "INSERT INTO org_membership
                           (sid, handle, display, rank, stars, first_seen,
                            last_seen, gone_since, src, manual_ts, note, manual_src)
                         VALUES (?,?,'','',0,?,?,NULL,'manual',?,?,?)",
                        params![m.sid, m.handle, now, now, m.ts, m.note, install],
                    )?;
                    out.roster_added += 1;
                }
                Some((manual_ts, src)) => {
                    let manual = manual_ts.is_some_and(|t| t != 0.0);
                    if manual && src.is_none() {
                        continue; // yours: your note, your call
                    }
                    if manual && src.as_deref() != Some(install) {
                        continue; // another mate's: first in keeps it
                    }
                    tx.execute(
                        "UPDATE org_membership SET gone_since=NULL,
                           manual_ts=COALESCE(manual_ts, ?), note=?, manual_src=?
                         WHERE sid=? AND handle=?",
                        params![m.ts, m.note, install, m.sid, m.handle],
                    )?;
                    if !manual {
                        out.roster_added += 1;
                    }
                }
            }
            tx.execute(
                "INSERT OR IGNORE INTO player_orgs (handle, sid, is_main, source)
                 VALUES (?,?,0,'roster')",
                [&m.handle, &m.sid],
            )?;
        }
        for (sid, handle, src) in &theirs {
            if !seen.contains(&(sid.clone(), handle.clone())) {
                drop_manual(&tx, sid, handle, src.as_deref())?;
                out.roster_dropped += 1;
            }
        }
    }
    tx.commit()?;
    Ok(out)
}

/// Someone you have imported from, with what of theirs you still hold.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ShareSource {
    pub install: String,
    pub name: Option<String>,
    pub exported_at: Option<f64>,
    pub imported_at: Option<f64>,
    pub reports: i64,
    pub roster: i64,
}

pub fn sources(db: &Database) -> Result<Vec<ShareSource>, ShareError> {
    let mut out: Vec<ShareSource> = db
        .conn
        .prepare(
            "SELECT install, name, exported_at, imported_at FROM shares ORDER BY imported_at DESC",
        )?
        .query_map([], |r| {
            Ok(ShareSource {
                install: r.get(0)?,
                name: r.get(1)?,
                exported_at: r.get(2)?,
                imported_at: r.get(3)?,
                reports: 0,
                roster: 0,
            })
        })?
        .collect::<Result<_, _>>()?;
    let count =
        |sql: &str, p: &str| -> rusqlite::Result<i64> { db.conn.query_row(sql, [p], |r| r.get(0)) };
    for s in &mut out {
        s.reports = count(
            "SELECT COUNT(*) FROM reports WHERE origin LIKE ?",
            &format!("{}:%", s.install),
        )?;
        s.roster = count(
            "SELECT COUNT(*) FROM org_membership WHERE manual_src=?",
            &s.install,
        )?;
    }
    Ok(out)
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RemovedSource {
    pub name: Option<String>,
    pub reports: usize,
    pub roster: usize,
}

/// Everything one person's exports brought in, gone. The orgs their import
/// flagged stay flagged - unflagging is a judgement for the Orgs tab.
pub fn remove_source(db: &Database, install: &str) -> Result<RemovedSource, ShareError> {
    if !re().install.is_match(install) {
        return refuse("no such source");
    }
    let name: Option<Option<String>> = db
        .conn
        .query_row("SELECT name FROM shares WHERE install=?", [install], |r| {
            r.get(0)
        })
        .optional()?;
    let Some(name) = name else {
        return refuse("no such source");
    };
    let tx = db.conn.unchecked_transaction()?;
    let reports = tx.execute(
        "DELETE FROM reports WHERE origin LIKE ?",
        [format!("{install}:%")],
    )?;
    let rows: Vec<(String, String, Option<String>)> = tx
        .prepare("SELECT sid, handle, src FROM org_membership WHERE manual_src=?")?
        .query_map([install], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    for (sid, handle, src) in &rows {
        drop_manual(&tx, sid, handle, src.as_deref())?;
    }
    tx.execute("DELETE FROM shares WHERE install=?", [install])?;
    tx.commit()?;
    Ok(RemovedSource {
        name,
        reports,
        roster: rows.len(),
    })
}
