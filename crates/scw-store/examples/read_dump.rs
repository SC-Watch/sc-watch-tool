//! Every read the reputation database answers, for every player in a COPY
//! of a sc-watch.db, as JSON - for comparing with the Python
//! (tools/read_compare.py). The output names real players: keep it beside
//! the copy, and delete both.
//!
//!     cargo run --release -p scw-store --example read_dump -- <copy of sc-watch.db> <flags.json> <now> > out.json
//!
//! "Is this a real account" is a formula both sides compute: the sum of the
//! handle's code points mod 3 - 0 real, 1 not, 2 cannot tell.
use scw_store::reputation::{Database, Verify};
use serde_json::{Value, json};

struct Formula;
impl Verify for Formula {
    fn exists(&self, handle: &str) -> Result<Option<bool>, String> {
        let s: u32 = handle.to_uppercase().chars().map(|c| c as u32).sum();
        Ok(match s % 3 {
            0 => Some(true),
            1 => Some(false),
            _ => None,
        })
    }
}

fn variants(h: &str) -> Vec<String> {
    let c: Vec<char> = h.chars().collect();
    let mut v = Vec::new();
    if c.len() > 4 {
        v.push(c[..c.len() - 1].iter().collect());
        v.push(c[1..].iter().collect());
    }
    v.push(h.replace('O', "0").replace('I', "1"));
    v.push(h.replace('0', "O").replace('S', "5"));
    v
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let now: f64 = a[2].parse().unwrap();
    let mut db = Database::open(std::path::Path::new(&a[0])).unwrap();
    db.flags = scw_store::flags::Flags::load(std::path::Path::new(&a[1]));
    db.verify = Some(Box::new(Formula));
    db.clock = Box::new(move || now);
    db.utc_dates = true;
    let handles = db.strings_pub("SELECT handle FROM players ORDER BY handle");
    let mut players = serde_json::Map::new();
    for h in &handles {
        let canon: Vec<Value> = variants(h)
            .iter()
            .map(|v| {
                let (c, f) = db.canonical(v, false).unwrap();
                json!([v, c, f])
            })
            .collect();
        players.insert(
            h.clone(),
            json!({
                "assess": db.assess(h, None, false).unwrap().to_json(now, &db.flags, true),
                "listed": db.is_listed(h).unwrap(),
                "audits": serde_json::to_value(db.audits_for(h).unwrap()).unwrap(),
                "canonical": canon,
            }),
        );
    }
    let sids = db.strings_pub("SELECT sid FROM org_flags ORDER BY sid");
    let members: serde_json::Map<String, Value> = sids
        .iter()
        .map(|s| {
            (
                s.clone(),
                serde_json::to_value(db.members_of(s).unwrap()).unwrap(),
            )
        })
        .collect();
    let mut stats = serde_json::to_value(db.stats().unwrap()).unwrap();
    stats.as_object_mut().unwrap().remove("bytes");
    println!(
        "{}",
        json!({
            "players": players,
            "flagged_orgs": serde_json::to_value(db.flagged_orgs().unwrap()).unwrap(),
            "members": members,
            "pending": db.pending_reports().unwrap(),
            "stats": stats,
            "recent": serde_json::to_value(db.recent(25).unwrap()).unwrap(),
            "open_audits": serde_json::to_value(db.open_audits().unwrap()).unwrap(),
            "org_manual": db.org_manual().unwrap(),
            "unnamed": db.unnamed_flagged().unwrap(),
        })
    );
}
