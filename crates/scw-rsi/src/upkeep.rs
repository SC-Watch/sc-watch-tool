//! What sc-watch.db needs the lookup client for - reputation.py's
//! find_ghosts, purge_ghosts and fetch_roster, and the client as the
//! database's identity verifier. Here rather than in scw-store, which knows
//! nothing of the network.

use std::sync::Arc;

use serde::Serialize;
use serde_json::{Value, json};

use crate::MEMBER_PAGE_CAP;
use crate::client::Client;
use scw_store::json::{py_strip, truthy};
use scw_store::reputation::{Database, RosterChanges, Verify};

/// The client as sc-watch.db's verifier: "is this a real account?", asked
/// strictly - None when nobody here can tell.
pub struct Verifier(pub Arc<Client>);

impl Verify for Verifier {
    fn exists(&self, handle: &str) -> Result<Option<bool>, String> {
        self.0.exists(handle, true).map_err(|e| self.0.describe(&e))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Ghost {
    pub handle: String,
    pub sightings: i64,
    pub reports: i64,
    pub audits: i64,
    /// Why it is kept, when it is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// find_ghosts()'s buckets.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Ghosts {
    /// Positively not accounts, with nothing that could correct the record.
    pub ghosts: Vec<Ghost>,
    /// Not accounts, but a report or an audit is waiting on them.
    pub kept: Vec<Ghost>,
    /// Nobody here could check them - absence of evidence.
    pub unchecked: Vec<String>,
    /// (handle, why the lookup failed): the port words it with describe,
    /// where rsi.py printed the exception.
    pub errors: Vec<(String, String)>,
    /// purge_ghosts(): whether they were deleted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied: Option<bool>,
}

/// Players a source that asked RSI says do not exist - OCR inventions that
/// dilute the counts. SENTRY is asked database-only. `limit` stops after
/// that many ghosts (0: no limit). Acts on nothing.
pub fn find_ghosts(db: &Database, client: &Client, limit: usize) -> rusqlite::Result<Ghosts> {
    let rows: Vec<(String, Option<i64>)> = db
        .conn
        .prepare("SELECT handle, sightings FROM players ORDER BY handle")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let count = |table: &str, h: &str| -> rusqlite::Result<i64> {
        db.conn.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE handle=?"),
            [h],
            |r| r.get(0),
        )
    };
    let mut out = Ghosts::default();
    for (h, sightings) in rows {
        if limit > 0 && out.ghosts.len() >= limit {
            break;
        }
        let cit = match client.citizen(&h, false, false) {
            Ok(c) => c,
            Err(e) => {
                out.errors
                    .push((h, format!("{}: {}", e.type_name(), client.describe(&e))));
                continue;
            }
        };
        if cit.unverified {
            out.unchecked.push(h);
            continue;
        }
        if cit.exists {
            continue;
        }
        let (reports, audits) = (count("reports", &h)?, count("audits", &h)?);
        let why = match (reports > 0, audits > 0) {
            (false, false) => None,
            (true, false) => Some("has a report".into()),
            (false, true) => Some("waiting on an audit".into()),
            (true, true) => Some("has a report and waiting on an audit".into()),
        };
        let item = Ghost {
            handle: h,
            sightings: sightings.unwrap_or(0),
            reports,
            audits,
            why,
        };
        if item.why.is_some() {
            out.kept.push(item);
        } else {
            out.ghosts.push(item);
        }
    }
    Ok(out)
}

/// Deletes every handle find_ghosts() is sure about - a dry run unless
/// `apply`.
pub fn purge_ghosts(db: &Database, client: &Client, apply: bool) -> rusqlite::Result<Ghosts> {
    let mut found = find_ghosts(db, client, 0)?;
    if apply {
        for g in &found.ghosts {
            db.forget(&g.handle)?;
        }
    }
    found.applied = Some(apply);
    Ok(found)
}

/// A roster pull, recorded.
#[derive(Clone, Debug, PartialEq)]
pub struct Pulled {
    pub sid: String,
    pub changes: RosterChanges,
    /// Players met that the flag now reaches.
    pub reaches: i64,
    /// What the org itself says it has, for a sanity check on the pull.
    pub org_members: Option<Value>,
    pub source: Option<&'static str>,
    pub hidden: i64,
    pub redacted: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Pull {
    /// Nothing recorded, and why.
    Refused {
        sid: Option<String>,
        error: String,
    },
    Recorded(Pulled),
}

impl Pull {
    /// The dict reputation.py's fetch_roster() returns.
    pub fn to_json(&self) -> Value {
        match self {
            Pull::Refused { sid: None, error } => json!({"ok": false, "error": error}),
            Pull::Refused {
                sid: Some(s),
                error,
            } => json!({"ok": false, "sid": s, "error": error}),
            Pull::Recorded(p) => json!({
                "ok": true, "sid": p.sid, "members": p.changes.members,
                "joined": p.changes.joined, "left": p.changes.left, "back": p.changes.back,
                "reaches": p.reaches, "partial": p.changes.partial,
                "org_members": p.org_members, "source": p.source,
                "switched": p.changes.switched, "hidden": p.hidden, "redacted": p.redacted,
            }),
        }
    }
}

/// Pulls a FLAGGED org's member list and records it - the step that makes
/// flagging an org reach its affiliates. Refuses rather than guesses: no
/// source, a failed pull, or an empty list records nothing.
pub fn fetch_roster(
    db: &Database,
    client: &Client,
    sid: &str,
    max_pages: Option<u32>,
    on_page: &mut dyn FnMut(u32, usize),
) -> rusqlite::Result<Pull> {
    let sid = py_strip(sid).to_uppercase();
    let refuse = |sid: Option<&str>, e: &str| Pull::Refused {
        sid: sid.map(String::from),
        error: e.into(),
    };
    if sid.is_empty() {
        return Ok(refuse(None, "no org given"));
    }
    let flagged: Option<String> = rusqlite::OptionalExtension::optional(db.conn.query_row(
        "SELECT sid FROM org_flags WHERE sid=?",
        [&sid],
        |r| r.get(0),
    ))?;
    if flagged.is_none() {
        return Ok(refuse(None, &format!("{sid} is not flagged")));
    }
    if !client.enriches() {
        return Ok(refuse(
            Some(&sid),
            "no lookup source is available, so no roster can be fetched",
        ));
    }
    let r = match client.roster(&sid, max_pages.unwrap_or(MEMBER_PAGE_CAP), on_page) {
        Ok(r) => r,
        Err(e) => {
            return Ok(refuse(
                Some(&sid),
                &format!("roster fetch failed - {}", client.describe(&e)),
            ));
        }
    };
    if r.members.is_empty() {
        return Ok(refuse(
            Some(&sid),
            "the member list came back empty - keeping the one already stored",
        ));
    }
    let changes = db.record_roster(&sid, &r.members, r.complete, r.source)?;
    let reaches = db
        .flagged_orgs()?
        .into_iter()
        .find(|o| o.sid == sid)
        .map_or(0, |o| o.reaches);
    let mut org_members = r.total.clone();
    // What the org record says it has - a sanity check on the pull; a
    // failure to get it is not a failure to pull.
    if !truthy(org_members.as_ref())
        && let Ok(o) = client.org(&sid, false)
    {
        org_members = (o.members != 0).then(|| json!(o.members));
    }
    Ok(Pull::Recorded(Pulled {
        sid,
        changes,
        reaches,
        org_members,
        source: r.source,
        hidden: r.hidden,
        redacted: r.redacted,
    }))
}
