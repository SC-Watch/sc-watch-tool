//! What sc-watch knows about one handle (reputation.Assessment and
//! Database.assess), as the Python says it. A report never vanishes: it
//! loses half its weight every 180 days. Watchlists never move the score -
//! the number stays what YOU recorded.

use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};

use super::database::{Database, Hit};
use super::{HALF_LIFE_DAYS, PENDING};
use crate::flags::Flags;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Report {
    pub ts: f64,
    pub category: String,
    pub id: i64,
    pub note: String,
    pub evidence: String,
    pub source: String,
}

impl Report {
    pub fn age_days(&self, now: f64) -> f64 {
        (now - self.ts) / 86400.0
    }

    /// Half-life decay.
    pub fn weight(&self, now: f64) -> f64 {
        0.5f64.powf(self.age_days(now) / HALF_LIFE_DAYS)
    }

    fn mine(&self) -> bool {
        self.source.is_empty() || self.source == "self"
    }
}

/// An insertion-ordered map, as the Python's dicts.
pub type Ordered = Vec<(String, String)>;

fn put(m: &mut Ordered, k: &str, v: String) {
    match m.iter_mut().find(|e| e.0 == k) {
        Some(e) => e.1 = v,
        None => m.push((k.to_string(), v)),
    }
}

fn get<'a>(m: &'a Ordered, k: &str) -> Option<&'a str> {
    m.iter().find(|e| e.0 == k).map(|e| e.1.as_str())
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Assessment {
    pub handle: String,
    pub known: bool,
    pub sightings: i64,
    pub first_seen: Option<f64>,
    pub last_seen: Option<f64>,
    pub reports: Vec<Report>,
    /// (sid, your note on the flag).
    pub flagged_orgs: Vec<(String, String)>,
    /// (sid, gone since) for flagged orgs that used to list them.
    pub former_orgs: Vec<(String, f64)>,
    /// sid -> your note, for memberships recorded by hand.
    pub manual_orgs: Ordered,
    /// sid -> the org mate whose export added them (absent: you).
    pub manual_by: Ordered,
    /// sid -> a flagged org's display name.
    pub org_names: Ordered,
    pub watchlist: Vec<Hit>,
}

/// "formerly on flagged org X's roster - off it since 05 Nov (left, or hid
/// it)": never "left", since the roster omits hidden members.
pub fn former_line(sid: &str, since: Option<f64>, utc: bool) -> String {
    let when = match since.filter(|s| *s != 0.0) {
        None => "a while".to_string(),
        Some(s) => {
            let t = chrono::DateTime::from_timestamp(s.floor() as i64, 0).unwrap_or_default();
            if utc {
                t.format("%d %b").to_string()
            } else {
                t.with_timezone(&chrono::Local).format("%d %b").to_string()
            }
        }
    };
    format!("formerly on flagged org {sid}'s roster - off it since {when} (left, or hid it)")
}

/// The tail on a flagged-org line when the membership was added by hand.
pub fn manual_line(sid: &str, manual_orgs: &Ordered, manual_by: &Ordered) -> String {
    let Some(note) = get(manual_orgs, sid) else {
        return String::new();
    };
    let who = get(manual_by, sid)
        .filter(|w| !w.is_empty())
        .unwrap_or("you");
    let tail = if note.is_empty() {
        String::new()
    } else {
        format!(": {note}")
    };
    format!(" (added by {who}{tail})")
}

impl Assessment {
    /// Decayed sum of reports: negative ones count against, positive for.
    pub fn score(&self, now: f64, flags: &Flags) -> f64 {
        let mut total = 0.0;
        for r in &self.reports {
            total += r.weight(now) * flags.sign(&r.category);
        }
        total
    }

    /// Raise the alarm? Your reports, a flagged org (current or former), or
    /// an EXACT watchlist match - never a variant one.
    pub fn alert(&self, now: f64, flags: &Flags) -> bool {
        self.score(now, flags) > 0.0
            || !self.flagged_orgs.is_empty()
            || !self.former_orgs.is_empty()
            || self.watchlist.iter().any(|h| h.match_kind == "exact")
    }

    /// Every reason a spoken alert could give, strongest first: your own
    /// negative reports, shared ones, a flagged org, a former one, an exact
    /// watchlist match.
    pub fn spoken_all(&self, flags: &Flags) -> Vec<Value> {
        let mut out = Vec::new();
        for mine in [true, false] {
            let mut by_cat: Vec<(String, Vec<&Report>)> = Vec::new();
            for r in &self.reports {
                if r.mine() != mine {
                    continue;
                }
                if r.category != PENDING && flags.sign(&r.category) <= 0.0 {
                    continue;
                }
                match by_cat.iter_mut().find(|e| e.0 == r.category) {
                    Some(e) => e.1.push(r),
                    None => by_cat.push((r.category.clone(), vec![r])),
                }
            }
            if by_cat.is_empty() {
                continue;
            }
            // max by (count, newest), the first of equals.
            let key = |rs: &Vec<&Report>| {
                (
                    rs.len(),
                    rs.iter().map(|r| r.ts).fold(f64::NEG_INFINITY, f64::max),
                )
            };
            let mut best = &by_cat[0];
            for e in &by_cat[1..] {
                let (a, b) = (key(&e.1), key(&best.1));
                if a.0 > b.0 || (a.0 == b.0 && a.1 > b.1) {
                    best = e;
                }
            }
            let (cat, rs) = (&best.0, &best.1);
            let label = if cat == PENDING {
                "Flagged".to_string()
            } else {
                flags.speakable(cat)
            };
            let count: usize = by_cat.iter().map(|e| e.1.len()).sum();
            let mut r = json!({"kind": if mine { "report" } else { "shared" }, "label": label, "count": count});
            if !mine {
                r["source"] = Value::String(rs[0].source.clone());
            }
            out.push(r);
        }
        let name = |sid: &str| {
            get(&self.org_names, sid)
                .filter(|n| !n.is_empty())
                .unwrap_or(sid)
                .to_string()
        };
        if let Some((sid, _)) = self.flagged_orgs.first() {
            out.push(json!({"kind": "org", "org": name(sid)}));
        }
        if let Some((sid, _)) = self.former_orgs.first() {
            out.push(json!({"kind": "former", "org": name(sid)}));
        }
        if self.watchlist.iter().any(|h| h.match_kind == "exact") {
            out.push(json!({"kind": "watchlist"}));
        }
        out
    }

    /// Why it fired, in plain language - never just a verdict.
    pub fn lines(&self, now: f64, flags: &Flags, utc: bool) -> Vec<String> {
        let mut out = Vec::new();
        let mut by_cat: Vec<(String, Vec<&Report>)> = Vec::new();
        for r in &self.reports {
            match by_cat.iter_mut().find(|e| e.0 == r.category) {
                Some(e) => e.1.push(r),
                None => by_cat.push((r.category.clone(), vec![r])),
            }
        }
        by_cat.sort_by_key(|e| std::cmp::Reverse(e.1.len()));
        for (cat, rs) in &by_cat {
            let newest = rs
                .iter()
                .map(|r| r.age_days(now))
                .fold(f64::INFINITY, f64::min);
            let when = if newest < 1.0 {
                "today".to_string()
            } else if newest < 365.0 {
                format!("{}d ago", newest.trunc() as i64)
            } else {
                format!("{:.1}y ago", newest / 365.0)
            };
            out.push(format!(
                "{}x {}, most recent {when}",
                rs.len(),
                flags.by_key(cat).label
            ));
        }
        for (sid, note) in &self.flagged_orgs {
            let tail = if note.is_empty() {
                String::new()
            } else {
                format!(" - {note}")
            };
            out.push(format!(
                "member of flagged org {sid}{tail}{}",
                manual_line(sid, &self.manual_orgs, &self.manual_by)
            ));
        }
        for (sid, since) in &self.former_orgs {
            out.push(former_line(sid, Some(*since), utc));
        }
        out.extend(self.watchlist.iter().map(|h| h.describe()));
        if self.sightings > 1 {
            let span = (self.last_seen.unwrap_or(0.0) - self.first_seen.unwrap_or(0.0)) / 86400.0;
            out.push(if span >= 1.0 {
                format!("seen {}x over {span:.0}d", self.sightings)
            } else {
                format!("seen {}x today", self.sightings)
            });
        }
        out
    }

    /// As the Python's dataclass, with score, alert, spoken_all and lines.
    pub fn to_json(&self, now: f64, flags: &Flags, utc: bool) -> Value {
        let obj = |m: &Ordered| -> Value {
            Value::Object(
                m.iter()
                    .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                    .collect(),
            )
        };
        json!({
            "handle": self.handle,
            "known": self.known,
            "sightings": self.sightings,
            "first_seen": self.first_seen,
            "last_seen": self.last_seen,
            "reports": self.reports,
            "flagged_orgs": self.flagged_orgs.iter().map(|(s, n)| json!([s, n])).collect::<Vec<_>>(),
            "former_orgs": self.former_orgs.iter().map(|(s, t)| json!([s, t])).collect::<Vec<_>>(),
            "manual_orgs": obj(&self.manual_orgs),
            "manual_by": obj(&self.manual_by),
            "org_names": obj(&self.org_names),
            "watchlist": self.watchlist,
            "score": self.score(now, flags),
            "alert": self.alert(now, flags),
            "spoken_all": self.spoken_all(flags),
            "lines": self.lines(now, flags, utc),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Stats {
    pub players: i64,
    pub sightings: i64,
    pub reports: i64,
    pub flagged_orgs: i64,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Recent {
    pub handle: String,
    pub sightings: Option<i64>,
    pub last_seen: Option<f64>,
}

impl Database {
    /// What is known about a handle. `exact` skips the misread resolution
    /// (records filed under THIS handle, as stored); `variants` is passed to
    /// the watchlist.
    pub fn assess(
        &self,
        handle: &str,
        variants: Option<bool>,
        exact: bool,
    ) -> rusqlite::Result<Assessment> {
        let h = if exact {
            handle.trim().to_uppercase()
        } else {
            self.canonical(handle, false)?.0
        };
        let mut a = Assessment {
            handle: h.clone(),
            ..Default::default()
        };
        type Player = (Option<i64>, Option<f64>, Option<f64>);
        let row: Option<Player> = self
            .conn
            .query_row(
                "SELECT sightings, first_seen, last_seen FROM players WHERE handle=?",
                [&h],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((s, f, l)) = row {
            a.known = true;
            a.sightings = s.unwrap_or(0);
            a.first_seen = f;
            a.last_seen = l;
        }
        for r in self.maps(
            "SELECT * FROM reports WHERE handle=? ORDER BY ts DESC",
            [&h],
        )? {
            let text = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let source = text("source");
            a.reports.push(Report {
                ts: r.get("ts").and_then(Value::as_f64).unwrap_or(0.0),
                category: text("category"),
                id: r.get("id").and_then(Value::as_i64).unwrap_or(0),
                note: text("note"),
                evidence: text("evidence"),
                source: if source.is_empty() {
                    "self".into()
                } else {
                    source
                },
            });
        }
        for r in self.maps(
            "SELECT f.sid, f.note, f.name, m.manual_ts, m.note AS mnote,
                          s.name AS mby
                   FROM org_flags f
                   JOIN player_orgs p ON p.sid = f.sid
                   LEFT JOIN org_membership m
                     ON m.sid = f.sid AND m.handle = p.handle
                   LEFT JOIN shares s ON s.install = m.manual_src
                   WHERE p.handle=?",
            [&h],
        )? {
            let text = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let sid = text("sid");
            a.flagged_orgs.push((sid.clone(), text("note")));
            if !text("name").is_empty() {
                put(&mut a.org_names, &sid, text("name"));
            }
            if r.get("manual_ts")
                .and_then(Value::as_f64)
                .is_some_and(|t| t != 0.0)
            {
                put(&mut a.manual_orgs, &sid, text("mnote"));
                if !text("mby").is_empty() {
                    put(&mut a.manual_by, &sid, text("mby"));
                }
            }
        }
        let current: Vec<String> = a.flagged_orgs.iter().map(|(s, _)| s.clone()).collect();
        for r in self.maps(
            "SELECT m.sid, m.gone_since, f.name FROM org_membership m
                   JOIN org_flags f ON f.sid = m.sid
                   WHERE m.handle=? AND m.gone_since IS NOT NULL",
            [&h],
        )? {
            let sid = r
                .get("sid")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if !current.contains(&sid) {
                a.former_orgs.push((
                    sid.clone(),
                    r.get("gone_since").and_then(Value::as_f64).unwrap_or(0.0),
                ));
                if let Some(n) = r
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|n| !n.is_empty())
                {
                    put(&mut a.org_names, &sid, n.to_string());
                }
            }
        }
        if let Some(wl) = &self.watchlist {
            // On the handle AS READ: the watchlist never saw your merges.
            a.watchlist = wl.lookup(handle, variants).unwrap_or_default();
        }
        Ok(a)
    }

    pub fn stats(&self) -> rusqlite::Result<Stats> {
        let q = |s: &str| self.count(s, []);
        let path = self.root.join("sc-watch.db");
        Ok(Stats {
            players: q("SELECT COUNT(*) FROM players")?,
            sightings: q("SELECT COUNT(*) FROM sightings")?,
            reports: q("SELECT COUNT(*) FROM reports")?,
            flagged_orgs: q("SELECT COUNT(*) FROM org_flags")?,
            bytes: std::fs::metadata(path).map_or(0, |m| m.len()),
        })
    }

    pub fn recent(&self, limit: i64) -> rusqlite::Result<Vec<Recent>> {
        let mut stmt = self.conn.prepare(
            "SELECT handle, sightings, last_seen FROM players ORDER BY last_seen DESC LIMIT ?",
        )?;
        let rows = stmt.query_map(params![limit], |r| {
            Ok(Recent {
                handle: r.get(0)?,
                sightings: r.get(1)?,
                last_seen: r.get(2)?,
            })
        })?;
        rows.collect()
    }
}
