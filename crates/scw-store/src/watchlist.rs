//! watchlist.db - what OTHER people say about a handle (watchlist.py,
//! exactly). Its own file on purpose: somebody else's assertion, which can
//! be wrong, a grudge, or simply go away - delete the file and your own
//! judgements do not notice.
//!
//! It never merges identities (an "alt of" claim is text, never a link), it
//! never moves your score, and it never turns a VARIANT match - "a name that
//! could misread to this is on a list" - into a hit on a person: variants
//! come back tagged so the caller has to say so. A source's hedges survive
//! the import as each entry's confidence.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::json::{py_str, truthy};
use crate::reputation::sketch;

/// watchlist.SCHEMA, extracted from the Python verbatim.
pub const SCHEMA: &str = include_str!("watchlist_schema.sql");

/// You personally verified it.
pub const CONFIRMED: &str = "confirmed";
/// The source states it plainly.
pub const ASSERTED: &str = "asserted";
/// The source hedged, or it sat under a caveat.
pub const UNCONFIRMED: &str = "unconfirmed";
/// You looked and disagreed - kept, not deleted.
pub const DISPUTED: &str = "disputed";

/// One watchlist entry that matched a handle.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Hit {
    /// As the source wrote it.
    pub handle: String,
    pub source: String,
    pub confidence: String,
    pub claim: String,
    pub alt_of: String,
    pub reported_by: String,
    pub posted_ts: String,
    pub msg_url: String,
    /// "exact" or "variant" - never conflate these.
    #[serde(rename = "match")]
    pub match_kind: String,
}

impl Hit {
    /// One line for the tile, the console and the spoken alert alike:
    /// whose list, how sure they were, why, when.
    pub fn describe(&self) -> String {
        let mut out = if self.match_kind == "variant" {
            format!("a name like this is on {}", self.source)
        } else {
            format!("listed on {}", self.source)
        };
        if self.confidence == UNCONFIRMED {
            out.push_str(" (source was unsure)");
        } else if self.confidence == DISPUTED {
            out.push_str(" (you disputed this)");
        }
        if !self.claim.is_empty() {
            out.push_str(&format!(" - {}", self.claim));
        }
        if !self.posted_ts.is_empty() {
            out.push_str(&format!(" [{}]", self.posted_ts));
        }
        out
    }
}

pub struct Watchlist {
    pub conn: Connection,
    pub path: PathBuf,
    /// Whether lookup() includes variant matches when not told (the
    /// `watchlist_variants` setting; the owner keeps it current).
    pub variants_default: bool,
    /// The bundled list's name, "" when this build ships none.
    pub starter_name: String,
    pub clock: Box<dyn Fn() -> f64>,
}

fn text(m: &Map<String, Value>, k: &str) -> String {
    m.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

fn take(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// A JSON value as Python's `str(x or default)`.
fn str_or(v: Option<&Value>, default: &str) -> String {
    if truthy(v) {
        py_str(v.unwrap())
    } else {
        default.to_string()
    }
}

impl Watchlist {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = crate::db::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn,
            path: path.to_path_buf(),
            variants_default: true,
            starter_name: String::new(),
            clock: Box::new(crate::reputation::system_time),
        })
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    fn maps(
        &self,
        sql: &str,
        p: impl rusqlite::Params,
    ) -> rusqlite::Result<Vec<Map<String, Value>>> {
        let mut stmt = self.conn.prepare(sql)?;
        let names: Vec<String> = stmt.column_names().map(|s| s.to_string()).collect();
        let rows = stmt.query_map(p, |r| {
            let mut m = Map::new();
            for (i, n) in names.iter().enumerate() {
                m.insert(n.clone(), crate::reputation::json_value(r.get_ref(i)?));
            }
            Ok(m)
        })?;
        rows.collect()
    }

    // ---- sources --------------------------------------------------------------

    pub fn add_source(&self, name: &str, url: &str, note: &str) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT INTO sources (name, url, note, added_ts) VALUES (?,?,?,?) ON CONFLICT(name) DO UPDATE SET url=excluded.url, note=excluded.note",
            params![name, url, note, self.now()],
        )?;
        self.conn
            .query_row("SELECT id FROM sources WHERE name=?", [name], |r| r.get(0))
    }

    /// Every source with its own claim count and reason coverage, and
    /// whether it is the list that came with the tool.
    pub fn sources(&self) -> rusqlite::Result<Vec<Map<String, Value>>> {
        let mut out = self.maps(
            "SELECT s.*, (SELECT COUNT(*) FROM entries e WHERE e.source_id=s.id) AS n, (SELECT COUNT(*) FROM entries e WHERE e.source_id=s.id    AND e.claim != '') AS n_reason FROM sources s ORDER BY name",
            [],
        )?;
        for d in &mut out {
            let bundled = !self.starter_name.is_empty() && text(d, "name") == self.starter_name;
            d.insert("bundled".into(), Value::Bool(bundled));
        }
        Ok(out)
    }

    /// Deletes a source and everything it ever claimed; returns how many
    /// entries went.
    pub fn forget_source(&self, source_id: i64) -> rusqlite::Result<i64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) c FROM entries WHERE source_id=?",
            [source_id],
            |r| r.get(0),
        )?;
        self.conn
            .execute("DELETE FROM entries WHERE source_id=?", [source_id])?;
        self.conn
            .execute("DELETE FROM orgs WHERE source_id=?", [source_id])?;
        self.conn
            .execute("DELETE FROM sources WHERE id=?", [source_id])?;
        Ok(n)
    }

    pub fn set_enabled(&self, source_id: i64, on: bool) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE sources SET enabled=? WHERE id=?",
            params![on as i64, source_id],
        )?;
        Ok(())
    }

    // ---- import ---------------------------------------------------------------

    /// Records one claim; false when this source already said it (unique on
    /// source, handle and message - two incidents are corroboration).
    #[allow(clippy::too_many_arguments)]
    pub fn add_entry(
        &self,
        source_id: i64,
        handle: &str,
        confidence: &str,
        claim: &str,
        alt_of: &str,
        reported_by: &str,
        posted_ts: &str,
        msg_id: &str,
        msg_url: &str,
    ) -> rusqlite::Result<bool> {
        let h = handle.trim();
        if h.is_empty() {
            return Ok(false);
        }
        Ok(self.conn.execute(
            "INSERT OR IGNORE INTO entries (source_id, handle, sketch, confidence, claim, alt_of, reported_by, posted_ts, msg_id, msg_url, fetched_ts) VALUES (?,?,?,?,?,?,?,?,?,?,?)",
            params![source_id, h, sketch(h), confidence, claim, alt_of, reported_by, posted_ts, msg_id, msg_url, self.now()],
        )? > 0)
    }

    pub fn add_org(
        &self,
        source_id: i64,
        sid: &str,
        claim: &str,
        msg_id: &str,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO orgs (source_id, sid, claim, msg_id, fetched_ts) VALUES (?,?,?,?,?)",
            params![source_id, sid.trim().to_uppercase(), claim, msg_id, self.now()],
        )?;
        Ok(())
    }

    pub fn mark_synced(&self, source_id: i64) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE sources SET synced_ts=? WHERE id=?",
            params![self.now(), source_id],
        )?;
        Ok(())
    }

    // ---- lookup ---------------------------------------------------------------

    /// Every claim an enabled source makes about this handle: exact matches
    /// first, then (when asked, or by default) variant matches, tagged.
    pub fn lookup(&self, handle: &str, variants: Option<bool>) -> rusqlite::Result<Vec<Hit>> {
        let h = handle.trim();
        if h.is_empty() {
            return Ok(Vec::new());
        }
        let variants = variants.unwrap_or(self.variants_default);
        let mut out = Vec::new();
        let mut seen = Vec::new();
        for r in self.maps(
            "SELECT e.*, s.name AS source FROM entries e JOIN sources s ON s.id = e.source_id WHERE s.enabled=1 AND e.handle=? COLLATE NOCASE",
            [h],
        )? {
            seen.push(r.get("id").and_then(Value::as_i64).unwrap_or(0));
            out.push(hit(&r, "exact"));
        }
        if variants {
            for r in self.maps(
                "SELECT e.*, s.name AS source FROM entries e JOIN sources s ON s.id = e.source_id WHERE s.enabled=1 AND e.sketch=?",
                [sketch(h)],
            )? {
                if !seen.contains(&r.get("id").and_then(Value::as_i64).unwrap_or(0)) {
                    out.push(hit(&r, "variant"));
                }
            }
        }
        Ok(out)
    }

    pub fn orgs_flagged(&self) -> rusqlite::Result<Vec<Map<String, Value>>> {
        self.maps(
            "SELECT o.*, s.name AS source FROM orgs o JOIN sources s ON s.id = o.source_id WHERE s.enabled=1 ORDER BY o.sid",
            [],
        )
    }

    /// (entries that say why, entries).
    pub fn reason_coverage(&self) -> rusqlite::Result<(i64, i64)> {
        self.conn.query_row(
            "SELECT COUNT(*) n, SUM(CASE WHEN claim != '' THEN 1 ELSE 0 END) w FROM entries",
            [],
            |r| {
                Ok((
                    r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    r.get::<_, Option<i64>>(0)?.unwrap_or(0),
                ))
            },
        )
    }

    pub fn stats(&self) -> rusqlite::Result<Value> {
        let (n, d): (i64, i64) = self.conn.query_row(
            "SELECT COUNT(*) n, COUNT(DISTINCT handle) d FROM entries",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let count =
            |sql: &str| -> rusqlite::Result<i64> { self.conn.query_row(sql, [], |r| r.get(0)) };
        Ok(json!({"entries": n, "handles": d,
                  "sources": count("SELECT COUNT(*) n FROM sources")?,
                  "orgs": count("SELECT COUNT(*) n FROM orgs")?,
                  "path": self.path.to_string_lossy()}))
    }
}

fn hit(r: &Map<String, Value>, match_kind: &str) -> Hit {
    let confidence = text(r, "confidence");
    Hit {
        handle: text(r, "handle"),
        source: text(r, "source"),
        confidence: if confidence.is_empty() {
            ASSERTED.into()
        } else {
            confidence
        },
        claim: text(r, "claim"),
        alt_of: text(r, "alt_of"),
        reported_by: text(r, "reported_by"),
        posted_ts: text(r, "posted_ts"),
        msg_url: text(r, "msg_url"),
        match_kind: match_kind.into(),
    }
}

impl crate::reputation::WatchlistLookup for Watchlist {
    fn lookup(&self, handle: &str, variants: Option<bool>) -> Result<Vec<Hit>, String> {
        Watchlist::lookup(self, handle, variants).map_err(|e| e.to_string())
    }
}

// ---- the bundled list ---------------------------------------------------------

/// The bundled list as plain data, or None when there is none or it is not
/// one (read_starter).
pub fn read_starter(path: &Path) -> Option<Map<String, Value>> {
    let bytes = std::fs::read(path).ok()?;
    let body = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
    let doc = crate::json::loads(std::str::from_utf8(body).ok()?).ok()?;
    let m = doc.as_object()?;
    m.get("entries")?.as_array()?;
    Some(m.clone())
}

/// The bundled list's name, "" without one.
pub fn starter_name(path: &Path) -> String {
    read_starter(path).map_or(String::new(), |d| str_or(d.get("name"), ""))
}

/// Imports the bundled list once per version (seed_starter): nothing when
/// there is none or this version was seeded already - deleting it sticks.
pub fn seed_starter(starter: &Path, wl: &Watchlist) -> rusqlite::Result<Value> {
    let Some(doc) = read_starter(starter) else {
        return Ok(json!({"seeded": 0, "why": "no bundled list"}));
    };
    let version = doc.get("version").map_or("1".to_string(), py_str);
    let done: Option<String> = wl
        .conn
        .query_row(
            "SELECT value FROM meta WHERE key='starter_version'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if done.as_deref() == Some(version.as_str()) {
        return Ok(json!({"seeded": 0, "why": "already seeded"}));
    }
    let name = take(&str_or(doc.get("name"), "bundled list"), 80);
    let sid = wl.add_source(&name, "", &take(&str_or(doc.get("note"), ""), 500))?;
    // Matched on CONTENT and date, not message id: the same list may already
    // be here, imported by hand.
    let mut already: Vec<(String, String, String)> = wl
        .maps(
            "SELECT handle, claim, posted_ts FROM entries WHERE source_id=?",
            [sid],
        )?
        .iter()
        .map(|r| {
            (
                text(r, "handle").trim().to_uppercase(),
                text(r, "claim"),
                text(r, "posted_ts"),
            )
        })
        .collect();
    let mut n = 0;
    for (i, e) in doc["entries"].as_array().unwrap().iter().enumerate() {
        let Some(e) = e.as_object() else { continue };
        let h = str_or(e.get("handle"), "").trim().to_string();
        let claim = take(&str_or(e.get("claim"), ""), 300);
        let posted = take(&str_or(e.get("posted"), ""), 32);
        let key = (h.to_uppercase(), claim.clone(), posted.clone());
        if h.is_empty() || already.contains(&key) {
            continue;
        }
        let conf = str_or(e.get("confidence"), ASSERTED);
        let conf = if [CONFIRMED, ASSERTED, UNCONFIRMED, DISPUTED].contains(&conf.as_str()) {
            conf
        } else {
            ASSERTED.into()
        };
        n += wl.add_entry(
            sid,
            &h,
            &conf,
            &claim,
            "",
            "",
            &posted,
            &format!("s{i}"),
            "",
        )? as i64;
        already.push(key);
    }
    if let Some(orgs) = doc
        .get("orgs")
        .filter(|o| truthy(Some(o)))
        .and_then(Value::as_array)
    {
        for o in orgs {
            if let Some(o) = o.as_object()
                && truthy(o.get("sid"))
            {
                wl.add_org(
                    sid,
                    &py_str(&o["sid"]),
                    &take(&str_or(o.get("claim"), ""), 300),
                    "",
                )?;
            }
        }
    }
    wl.mark_synced(sid)?;
    wl.conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('starter_version', ?)",
        [&version],
    )?;
    Ok(json!({"seeded": n, "why": "imported", "source": name, "version": version}))
}

// ---- pasted lists -----------------------------------------------------------------

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).unwrap())
}

const SPACE: &str = r"[\s\x1c-\x1f]";

/// A pasted block as entries, orgs, and the lines it could not read -
/// forgiving about SHAPE, strict about the HANDLE: a token that is not a
/// legal handle is never guessed at (parse_lines).
pub fn parse_lines(text: &str) -> Value {
    static HANDLE: OnceLock<Regex> = OnceLock::new();
    static CITIZEN: OnceLock<Regex> = OnceLock::new();
    static ORG: OnceLock<Regex> = OnceLock::new();
    static SPLIT: OnceLock<Regex> = OnceLock::new();
    static ANY_URL: OnceLock<Regex> = OnceLock::new();
    static MARKER: OnceLock<Regex> = OnceLock::new();
    let handle_re = re(&HANDLE, r"^[A-Za-z0-9_\-]{2,40}$");
    let citizen = re(
        &CITIZEN,
        r"(?i)robertsspaceindustries\.com/(?:[a-z-]+/)?citizens/([A-Za-z0-9_\-]{2,40})",
    );
    let org = re(
        &ORG,
        r"(?i)robertsspaceindustries\.com/(?:[a-z-]+/)?orgs/([A-Za-z0-9_\-]{2,32})",
    );
    let split = re(
        &SPLIT,
        &format!(r"(?:{SPACE}+[-–—]{SPACE}+|{SPACE}*[|:,]{SPACE}*|{SPACE}{{2,}})"),
    );
    let any_url = re(&ANY_URL, r"(?i)https?://\S+");
    let marker = re(&MARKER, &format!(r"^{SPACE}*(?:\d+[.)]|[-*•]){SPACE}+"));
    let (mut entries, mut skipped, mut orgs): (Vec<(String, String)>, Vec<String>, Vec<String>) =
        (Vec::new(), Vec::new(), Vec::new());
    for raw in py_splitlines(text) {
        let line = raw.trim_matches(is_space);
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        if let Some(o) = org.captures(line)
            && !citizen.is_match(line)
        {
            orgs.push(o[1].to_uppercase());
            continue;
        }
        let (handle, claim) = if let Some(m) = citizen.captures(line) {
            let no_url = any_url.replace_all(line, "");
            let spaced = split.replace_all(&no_url, " ");
            (
                m[1].to_string(),
                spaced
                    .trim_matches([' ', '-', '|', ':', ',', '\t'])
                    .to_string(),
            )
        } else {
            let body = marker.replace(line, "");
            let mut parts = split.splitn(&body, 2);
            let handle = parts
                .next()
                .unwrap_or("")
                .trim_matches(is_space)
                .to_string();
            let claim = parts
                .next()
                .unwrap_or("")
                .trim_matches(is_space)
                .to_string();
            if !handle_re.is_match(&handle) {
                skipped.push(take(line, 120));
                continue;
            }
            (handle, claim)
        };
        entries.push((handle, take(&claim, 300)));
    }
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for (h, c) in entries {
        let k = h.to_lowercase();
        if seen.contains(&k) {
            continue;
        }
        seen.push(k);
        out.push(json!({"handle": h, "claim": c}));
    }
    orgs.sort();
    orgs.dedup();
    json!({"entries": out, "skipped": skipped, "orgs": orgs})
}

/// Python's `str.isspace()` for one character.
fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Python's `str.splitlines()`: \n, \r\n, \r and the other line boundaries.
fn py_splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < s.len() {
        let c = s[i..].chars().next().unwrap();
        let w = c.len_utf8();
        let boundary = matches!(
            c,
            '\n' | '\r'
                | '\u{0b}'
                | '\u{0c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if boundary {
            out.push(&s[start..i]);
            i += w;
            if c == '\r' && i < s.len() && b[i] == b'\n' {
                i += 1;
            }
            start = i;
        } else {
            i += w;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// A URL fit for an href: http(s) only, else "".
pub fn safe_url(raw: &str) -> String {
    let u = raw.trim();
    let head: String = u.chars().take(8).collect::<String>().to_lowercase();
    if head.starts_with("http://") || head.starts_with("https://") {
        take(u, 300)
    } else {
        String::new()
    }
}

/// Parses a pasted block and stores it as one source.
#[allow(clippy::too_many_arguments)]
pub fn import_text(
    wl: &Watchlist,
    name: &str,
    text_in: &str,
    url: &str,
    note: &str,
    confidence: &str,
    reported_by: &str,
    posted_ts: &str,
) -> rusqlite::Result<Value> {
    let parsed = parse_lines(text_in);
    let entries = parsed["entries"].as_array().unwrap();
    let orgs = parsed["orgs"].as_array().unwrap();
    let skipped: Vec<Value> = parsed["skipped"].as_array().unwrap().clone();
    let first12 = |v: &[Value]| Value::Array(v.iter().take(12).cloned().collect());
    if entries.is_empty() && orgs.is_empty() {
        return Ok(
            json!({"ok": false, "error": "nothing in there looked like a handle", "skipped": first12(&skipped)}),
        );
    }
    let url = safe_url(url);
    let trimmed = take(name.trim(), 80);
    let sid = wl.add_source(
        if trimmed.is_empty() {
            "pasted list"
        } else {
            &trimmed
        },
        &url,
        &take(note.trim(), 500),
    )?;
    let mut added = 0;
    for e in entries {
        added += wl.add_entry(
            sid,
            e["handle"].as_str().unwrap(),
            confidence,
            e["claim"].as_str().unwrap(),
            "",
            &take(reported_by, 80),
            &take(posted_ts, 32),
            "paste",
            &url,
        )? as i64;
    }
    for o in orgs {
        wl.add_org(sid, o.as_str().unwrap(), "named in the imported list", "")?;
    }
    wl.mark_synced(sid)?;
    Ok(
        json!({"ok": true, "source_id": sid, "added": added, "seen": entries.len(),
              "orgs": orgs, "skipped": first12(&skipped), "skipped_n": skipped.len()}),
    )
}
