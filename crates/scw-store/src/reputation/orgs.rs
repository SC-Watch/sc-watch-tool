//! Flagged orgs, their rosters and membership history, and what you added
//! by hand (reputation.py's org half), as the Python does them.
//!
//! A roster records MEMBERSHIP, not blame: being in an org is a fact; no
//! report is ever written against its members. A source can only retire the
//! people IT listed, and only on a complete pull - and never somebody you
//! put on the roster yourself.

use std::collections::HashMap;

use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use serde_json::{Map, Value};

use super::database::Database;

/// One member as a roster lists them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RosterMember {
    pub handle: String,
    pub display: String,
    pub rank: String,
    pub stars: i64,
}

/// An org on a player's profile.
#[derive(Clone, Debug, PartialEq)]
pub struct ProfileOrg {
    pub sid: String,
    pub is_main: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RosterChanges {
    pub members: usize,
    pub joined: Vec<String>,
    pub left: Vec<String>,
    pub back: Vec<String>,
    pub partial: bool,
    pub switched: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Member {
    pub handle: String,
    pub display: String,
    pub rank: String,
    pub stars: i64,
    pub first_seen: Option<f64>,
    pub last_seen: Option<f64>,
    pub gone_since: Option<f64>,
    pub manual_ts: Option<f64>,
    pub note: String,
    pub manual_by: Option<String>,
    pub on_roster: bool,
    pub met: i64,
    pub met_last: Option<f64>,
    pub reports: i64,
}

/// The answer to adding or removing a member by hand.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct MemberChange {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub known: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct OrgManual {
    pub sid: String,
    pub name: String,
    pub primary_focus: String,
    pub secondary_focus: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FlaggedOrg {
    pub sid: String,
    pub name: String,
    pub note: String,
    pub ts: f64,
    pub roster_ts: Option<f64>,
    pub roster_n: i64,
    pub reaches: i64,
    pub former: i64,
    pub manual: i64,
    pub manual_shared: i64,
}

fn refused(error: String) -> MemberChange {
    MemberChange {
        error: Some(error),
        ..Default::default()
    }
}

impl Database {
    pub fn flag_org(&self, sid: &str, note: &str, name: &str) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO org_flags (sid, name, ts, note) VALUES (?,?,?,?)",
            params![sid.to_uppercase(), name, self.now(), note],
        )?;
        Ok(())
    }

    /// Flagged orgs with no display name.
    pub fn unnamed_flagged(&self) -> rusqlite::Result<Vec<String>> {
        self.strings(
            "SELECT sid FROM org_flags WHERE name IS NULL OR name=''",
            [],
        )
    }

    /// Gives a flagged org the name it lacks - only a blank one.
    pub fn set_flag_name(&self, sid: &str, name: &str) -> rusqlite::Result<bool> {
        let name = name.trim();
        if name.is_empty() {
            return Ok(false);
        }
        let short: String = name.chars().take(200).collect();
        Ok(self.conn.execute(
            "UPDATE org_flags SET name=? WHERE sid=? AND (name IS NULL OR name='')",
            params![short, sid.to_uppercase()],
        )? > 0)
    }

    /// Drops the flag, the roster rows it put there and its history;
    /// profile rows survive.
    pub fn unflag_org(&self, sid: &str) -> rusqlite::Result<()> {
        let sid = sid.to_uppercase();
        self.conn.execute(
            "DELETE FROM player_orgs WHERE sid=? AND source='roster'",
            [&sid],
        )?;
        self.conn
            .execute("DELETE FROM org_membership WHERE sid=?", [&sid])?;
        self.conn
            .execute("DELETE FROM org_flags WHERE sid=?", [&sid])?;
        Ok(())
    }

    /// What a player's PROFILE says about their orgs; roster rows survive.
    pub fn set_orgs(&self, handle: &str, orgs: &[ProfileOrg]) -> rusqlite::Result<()> {
        let h = handle.to_uppercase();
        self.conn.execute(
            "DELETE FROM player_orgs WHERE handle=? AND COALESCE(source,'profile')='profile'",
            [&h],
        )?;
        for o in orgs {
            self.conn.execute(
                "INSERT OR REPLACE INTO player_orgs (handle, sid, is_main, source) VALUES (?,?,?, 'profile')",
                params![h, o.sid.to_uppercase(), o.is_main as i64],
            )?;
        }
        Ok(())
    }

    /// Current membership from a roster; `replace` false for a truncated one.
    pub fn add_org_members(
        &self,
        sid: &str,
        handles: &[String],
        replace: bool,
    ) -> rusqlite::Result<usize> {
        let sid = sid.to_uppercase();
        if replace {
            self.conn.execute(
                "DELETE FROM player_orgs WHERE sid=? AND source='roster'",
                [&sid],
            )?;
        }
        let mut n = 0;
        for handle in handles {
            let h = handle.trim().to_uppercase();
            if h.is_empty() {
                continue;
            }
            self.conn.execute(
                "INSERT OR IGNORE INTO player_orgs (handle, sid, is_main, source) VALUES (?,?,0,'roster')",
                params![h, sid],
            )?;
            n += 1;
        }
        let now = self.now();
        if replace {
            self.conn.execute(
                "UPDATE org_flags SET roster_ts=?, roster_n=? WHERE sid=?",
                params![now, n as i64, sid],
            )?;
        } else {
            self.conn.execute(
                "UPDATE org_flags SET roster_ts=? WHERE sid=?",
                params![now, sid],
            )?;
        }
        Ok(n)
    }

    /// Stores a freshly pulled roster: current membership and its history.
    pub fn record_roster(
        &self,
        sid: &str,
        members: &[RosterMember],
        complete: bool,
        source: Option<&str>,
    ) -> rusqlite::Result<RosterChanges> {
        let sid = sid.to_uppercase();
        let now = self.now();
        let prev_src: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT roster_src FROM org_flags WHERE sid=?",
                [&sid],
                |r| r.get(0),
            )
            .optional()?;
        let this_src = source
            .filter(|s| !s.is_empty())
            .unwrap_or("sci")
            .to_string();
        let switched = this_src
            != prev_src
                .flatten()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "sci".into());
        // listed: first position kept, the last record for a handle wins.
        let mut order: Vec<String> = Vec::new();
        let mut listed: HashMap<String, &RosterMember> = HashMap::new();
        for m in members {
            let h = m.handle.trim().to_uppercase();
            if h.is_empty() || h == "REDACTED" {
                continue;
            }
            if !listed.contains_key(&h) {
                order.push(h.clone());
            }
            listed.insert(h, m);
        }
        struct Before {
            handle: String,
            gone: Option<f64>,
            src: Option<String>,
            manual: Option<f64>,
        }
        let before: Vec<Before> = {
            let mut stmt = self.conn.prepare(
                "SELECT handle, gone_since, src, manual_ts FROM org_membership WHERE sid=?",
            )?;
            let rows = stmt.query_map([&sid], |r| {
                Ok(Before {
                    handle: r.get(0)?,
                    gone: r.get(1)?,
                    src: r.get(2)?,
                    manual: r.get(3)?,
                })
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let find = |h: &str| before.iter().find(|b| b.handle == h);
        let joined: Vec<String> = order
            .iter()
            .filter(|h| find(h).is_none())
            .cloned()
            .collect();
        let back: Vec<String> = order
            .iter()
            .filter(|h| find(h).is_some_and(|b| b.gone.is_some_and(|g| g != 0.0)))
            .cloned()
            .collect();
        let left: Vec<String> = before
            .iter()
            .filter(|b| {
                complete
                    && !listed.contains_key(&b.handle)
                    && b.gone.is_none_or(|g| g == 0.0)
                    && b.manual.is_none_or(|m| m == 0.0)
                    && b.src.as_deref().filter(|s| !s.is_empty()).unwrap_or("sci") == this_src
            })
            .map(|b| b.handle.clone())
            .collect();
        for h in &order {
            let m = listed[h];
            self.conn.execute(
                "INSERT INTO org_membership
                     (sid, handle, display, rank, stars, first_seen, last_seen,
                      gone_since, src)
                   VALUES (?,?,?,?,?,?,?,NULL,?)
                   ON CONFLICT(sid, handle) DO UPDATE SET
                     display=excluded.display, rank=excluded.rank,
                     stars=excluded.stars, last_seen=excluded.last_seen,
                     gone_since=NULL, src=excluded.src",
                params![sid, h, m.display, m.rank, m.stars, now, now, this_src],
            )?;
        }
        for h in &left {
            self.conn.execute(
                "UPDATE org_membership SET gone_since=? WHERE sid=? AND handle=?",
                params![now, sid, h],
            )?;
        }
        self.conn.execute(
            "UPDATE org_flags SET roster_src=? WHERE sid=?",
            params![this_src, sid],
        )?;
        let current = self.strings(
            "SELECT handle FROM org_membership WHERE sid=? AND gone_since IS NULL",
            [&sid],
        )?;
        self.add_org_members(&sid, &current, true)?;
        self.conn.execute(
            "UPDATE org_flags SET roster_n=(
                 SELECT COUNT(*) FROM org_membership WHERE sid=?
                 AND gone_since IS NULL AND COALESCE(src,'sci')!='manual')
               WHERE sid=?",
            params![sid, sid],
        )?;
        Ok(RosterChanges {
            members: listed.len(),
            joined,
            left,
            back,
            partial: !complete,
            switched,
        })
    }

    /// Everyone this org's roster has listed, current members first.
    pub fn members_of(&self, sid: &str) -> rusqlite::Result<Vec<Member>> {
        let sid = sid.to_uppercase();
        let rows = self.maps(
            "SELECT m.*, p.sightings, p.last_seen AS met_last,
                          s.name AS manual_by
                   FROM org_membership m LEFT JOIN players p ON p.handle = m.handle
                   LEFT JOIN shares s ON s.install = m.manual_src
                   WHERE m.sid=?
                   ORDER BY (m.gone_since IS NOT NULL), m.handle",
            [&sid],
        )?;
        let text = |r: &Map<String, Value>, k: &str| {
            r.get(k).and_then(Value::as_str).unwrap_or("").to_string()
        };
        let num = |r: &Map<String, Value>, k: &str| r.get(k).and_then(Value::as_f64);
        let mut out = Vec::new();
        for r in rows {
            let handle = text(&r, "handle");
            let reports = self.count("SELECT COUNT(*) FROM reports WHERE handle=?", [&handle])?;
            out.push(Member {
                display: text(&r, "display"),
                rank: text(&r, "rank"),
                stars: r.get("stars").and_then(Value::as_i64).unwrap_or(0),
                first_seen: num(&r, "first_seen"),
                last_seen: num(&r, "last_seen"),
                gone_since: num(&r, "gone_since"),
                manual_ts: num(&r, "manual_ts"),
                note: text(&r, "note"),
                manual_by: if r
                    .get("manual_src")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty())
                {
                    r.get("manual_by").and_then(Value::as_str).map(String::from)
                } else {
                    None
                },
                on_roster: r
                    .get("src")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("sci")
                    != "manual",
                met: r.get("sightings").and_then(Value::as_i64).unwrap_or(0),
                met_last: num(&r, "met_last"),
                reports,
                handle,
            });
        }
        Ok(out)
    }

    /// Puts somebody on a flagged org's roster by hand - EXACT handle, and
    /// sticky: no pull retires them.
    pub fn add_member_manually(
        &self,
        sid: &str,
        handle: &str,
        note: &str,
    ) -> rusqlite::Result<MemberChange> {
        let (sid, h) = (sid.trim().to_uppercase(), handle.trim().to_uppercase());
        if !self.exists("SELECT 1 FROM org_flags WHERE sid=?", [&sid])? {
            return Ok(refused(format!("{sid} is not flagged")));
        }
        let note: String = note.trim().chars().take(200).collect();
        let now = self.now();
        type Row = (Option<f64>, Option<f64>, Option<String>);
        let row: Option<Row> = self
            .conn
            .query_row(
                "SELECT gone_since, manual_ts, src FROM org_membership WHERE sid=? AND handle=?",
                params![sid, h],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let state = match row {
            None => {
                self.conn.execute(
                    "INSERT INTO org_membership
                     (sid, handle, display, rank, stars, first_seen, last_seen,
                      gone_since, src, manual_ts, note)
                   VALUES (?,?,'','',0,?,?,NULL,'manual',?,?)",
                    params![sid, h, now, now, now, note],
                )?;
                "added"
            }
            Some((gone, manual, _)) => {
                self.conn.execute(
                    "UPDATE org_membership SET gone_since=NULL,
                     manual_ts=COALESCE(manual_ts, ?), note=?, manual_src=NULL
                   WHERE sid=? AND handle=?",
                    params![now, note, sid, h],
                )?;
                if manual.is_some_and(|m| m != 0.0) {
                    "updated"
                } else if gone.is_some_and(|g| g != 0.0) {
                    "back"
                } else {
                    "listed"
                }
            }
        };
        self.conn.execute(
            "INSERT OR IGNORE INTO player_orgs (handle, sid, is_main, source) VALUES (?,?,0,'roster')",
            params![h, sid],
        )?;
        let known = self.exists("SELECT 1 FROM players WHERE handle=?", [&h])?;
        Ok(MemberChange {
            ok: true,
            sid: Some(sid),
            handle: Some(h),
            state: Some(state.into()),
            known: Some(known),
            error: None,
        })
    }

    /// Takes back a membership you recorded; a publicly listed member stays.
    pub fn remove_manual_member(&self, sid: &str, handle: &str) -> rusqlite::Result<MemberChange> {
        let (sid, h) = (sid.trim().to_uppercase(), handle.trim().to_uppercase());
        let row: Option<(Option<String>, Option<f64>)> = self
            .conn
            .query_row(
                "SELECT src, manual_ts FROM org_membership WHERE sid=? AND handle=?",
                params![sid, h],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((src, _)) = row.filter(|(_, m)| m.is_some_and(|m| m != 0.0)) else {
            return Ok(refused(format!("{h} was not added to {sid} by you")));
        };
        let state = if src.as_deref().filter(|s| !s.is_empty()).unwrap_or("sci") == "manual" {
            self.conn.execute(
                "DELETE FROM org_membership WHERE sid=? AND handle=?",
                params![sid, h],
            )?;
            self.conn.execute(
                "DELETE FROM player_orgs WHERE sid=? AND handle=? AND source='roster'",
                params![sid, h],
            )?;
            "removed"
        } else {
            self.conn.execute(
                "UPDATE org_membership SET manual_ts=NULL, note=NULL, manual_src=NULL WHERE sid=? AND handle=?",
                params![sid, h],
            )?;
            "still_listed"
        };
        Ok(MemberChange {
            ok: true,
            sid: Some(sid),
            handle: Some(h),
            state: Some(state.into()),
            known: None,
            error: None,
        })
    }

    /// Your entry for an org a lookup could not describe; all blank clears it.
    pub fn set_org_manual(
        &self,
        sid: &str,
        name: &str,
        primary: &str,
        secondary: &str,
    ) -> rusqlite::Result<Option<OrgManual>> {
        let sid = sid.trim().to_uppercase();
        let (name, primary, secondary) = (name.trim(), primary.trim(), secondary.trim());
        if name.is_empty() && primary.is_empty() && secondary.is_empty() {
            self.conn
                .execute("DELETE FROM org_manual WHERE sid=?", [&sid])?;
            return Ok(None);
        }
        self.conn.execute(
            "INSERT OR REPLACE INTO org_manual
                 (sid, name, primary_focus, secondary_focus, ts)
               VALUES (?,?,?,?,?)",
            params![sid, name, primary, secondary, self.now()],
        )?;
        Ok(Some(OrgManual {
            sid,
            name: name.into(),
            primary_focus: primary.into(),
            secondary_focus: secondary.into(),
        }))
    }

    /// Every org entry you have made, by SID.
    pub fn org_manual(&self) -> rusqlite::Result<Map<String, Value>> {
        let mut out = Map::new();
        for r in self.maps(
            "SELECT sid, name, primary_focus, secondary_focus, ts FROM org_manual",
            [],
        )? {
            let sid = r["sid"].as_str().unwrap_or("").to_string();
            out.insert(sid, Value::Object(r));
        }
        Ok(out)
    }

    /// Every flagged org, with how far its flag actually reaches.
    pub fn flagged_orgs(&self) -> rusqlite::Result<Vec<FlaggedOrg>> {
        let rows = self.maps(
            "SELECT sid, name, note, ts, roster_ts, roster_n FROM org_flags ORDER BY sid",
            [],
        )?;
        let mut out = Vec::new();
        for r in rows {
            let sid = r["sid"].as_str().unwrap_or("").to_string();
            let reaches = self.count(
                "SELECT COUNT(DISTINCT h) FROM (
                     SELECT p.handle AS h FROM player_orgs p WHERE p.sid=?
                     UNION
                     SELECT m.handle FROM org_membership m
                     WHERE m.sid=? AND m.gone_since IS NOT NULL)
                   WHERE h IN (SELECT handle FROM players)",
                params![sid, sid],
            )?;
            let former = self.count(
                "SELECT COUNT(*) FROM org_membership WHERE sid=? AND gone_since IS NOT NULL",
                [&sid],
            )?;
            let (manual, shared): (i64, i64) = self.conn.query_row(
                "SELECT COALESCE(SUM(manual_src IS NULL), 0), COALESCE(SUM(manual_src IS NOT NULL), 0) FROM org_membership WHERE sid=? AND gone_since IS NULL AND manual_ts IS NOT NULL",
                [&sid],
                |x| Ok((x.get(0)?, x.get(1)?)),
            )?;
            let text = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            out.push(FlaggedOrg {
                name: text("name"),
                note: text("note"),
                ts: r.get("ts").and_then(Value::as_f64).unwrap_or(0.0),
                roster_ts: r.get("roster_ts").and_then(Value::as_f64),
                roster_n: r.get("roster_n").and_then(Value::as_i64).unwrap_or(0),
                reaches,
                former,
                manual,
                manual_shared: shared,
                sid,
            });
        }
        Ok(out)
    }
}
