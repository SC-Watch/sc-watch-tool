//! Everything an rsi_cache.db holds, as the port reads it, for
//! tools/rsi_compare.py: every profile and org (and their JSON written
//! back), partial profiles, member lists, stats; then every cached handle
//! looked up with the network refused - without a key and with one - the
//! identity check, and the cache-only step of complete_partials(); then
//! every row. At a fixed time; dates read in local time, as rsi.py reads
//! them.
//!
//!     rsi_dump <rsi_cache.db COPY> <out.json> <now (epoch seconds)>

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use scw_rsi::{
    Cache, Citizen, Client, Clock, FetchError, LookupError, Options, Reply, RequestLog, Transport,
};
use serde_json::{Map, Value, json};

struct Fixed(Mutex<(f64, f64)>);

impl Clock for Fixed {
    fn time(&self) -> f64 {
        self.0.lock().unwrap().0
    }
    fn monotonic(&self) -> f64 {
        self.0.lock().unwrap().1
    }
    fn sleep(&self, s: f64) {
        if s > 0.0 {
            let mut c = self.0.lock().unwrap();
            c.0 += s;
            c.1 += s;
        }
    }
    fn uniform(&self, a: f64, b: f64) -> f64 {
        (a + b) / 2.0
    }
}

/// Every request refused, as a dead network refuses it.
struct Refuse;

impl Transport for Refuse {
    fn get(&self, _: &str, _: &str, _: f64) -> Result<Reply, FetchError> {
        Err(FetchError::Connect("<urlopen error refused>".into()))
    }
}

fn citizen(c: &Citizen, now: f64) -> Value {
    let mut v = serde_json::to_value(c).unwrap();
    let n = &c.note;
    let mut ex = Map::new();
    if n.awaiting_sentry {
        ex.insert("awaiting_sentry".into(), json!(true));
    }
    if let Some(s) = &n.checking {
        ex.insert("checking".into(), json!(s));
    }
    if let Some(r) = n.retry_in {
        ex.insert("retry_in".into(), json!(r));
    }
    if let Some(s) = n.source {
        ex.insert("source".into(), json!(s.as_str()));
    }
    if let Some(s) = &n.stale_reason {
        ex.insert("stale_reason".into(), json!(s));
    }
    if let Some(s) = n.stored_at {
        ex.insert("stored_at".into(), json!(s));
    }
    if !n.unknown_focus.is_empty() {
        ex.insert("unknown_focus".into(), json!(n.unknown_focus));
    }
    v["extras"] = Value::Object(ex);
    v["summary"] = json!(c.summary(now, false));
    v["flags"] = json!(c.flags(now, false));
    v["enlisted_days"] = json!(c.enlisted_days(now, false));
    v
}

fn raised(client: Option<&Client>, e: &LookupError) -> Value {
    json!({"raised": e.type_name(), "describe": client.map(|c| c.describe(e)),
           "transient": e.is_transient()})
}

fn column(db: &Connection, sql: &str) -> Vec<String> {
    let mut s = db.prepare(sql).unwrap();
    s.query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn rows(path: &Path) -> Value {
    let db = Connection::open(path).unwrap();
    let mut out = Map::new();
    for t in column(
        &db,
        "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name",
    ) {
        let n = db
            .prepare(&format!("SELECT * FROM \"{t}\""))
            .unwrap()
            .column_count();
        let mut got: Vec<Value> = db
            .prepare(&format!("SELECT * FROM \"{t}\""))
            .unwrap()
            .query_map([], |r| {
                Ok(Value::Array(
                    (0..n)
                        .map(|i| match r.get_ref(i).unwrap() {
                            rusqlite::types::ValueRef::Null => Value::Null,
                            rusqlite::types::ValueRef::Integer(x) => json!(x),
                            rusqlite::types::ValueRef::Real(x) => json!(x),
                            rusqlite::types::ValueRef::Text(x) => json!(String::from_utf8_lossy(x)),
                            rusqlite::types::ValueRef::Blob(_) => json!("<blob>"),
                        })
                        .collect(),
                ))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        got.sort_by_key(|r| r.to_string());
        out.insert(t, Value::Array(got));
    }
    Value::Object(out)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (path, out, now) = (
        Path::new(&a[1]),
        Path::new(&a[2]),
        a[3].parse::<f64>().unwrap(),
    );
    let clock = Arc::new(Fixed(Mutex::new((now, 1000.0))));
    let cache = Arc::new(Cache::open(path, clock.clone()).unwrap());
    let db = Connection::open(path).unwrap();
    let handles = column(&db, "SELECT handle FROM citizens ORDER BY handle");
    let sids = column(&db, "SELECT sid FROM orgs ORDER BY sid");
    let members = column(
        &db,
        "SELECT DISTINCT handle FROM org_members ORDER BY handle",
    );
    drop(db);

    let mut citizens = Vec::new();
    for h in &handles {
        let any = cache.get_citizen(h, true);
        let text =
            any.as_ref().ok().and_then(Option::as_ref).map(|c| {
                scw_store::json::dumps_compact(&serde_json::to_value(c).unwrap()).unwrap()
            });
        citizens.push(json!({
            "handle": h,
            "any": match &any { Ok(c) => c.as_ref().map_or(Value::Null, |c| citizen(c, now)),
                                Err(e) => raised(None, e) },
            "text": text,
            "fresh": cache.get_citizen(h, false).map(|c| c.is_some()).unwrap_or(false),
        }));
    }
    let orgs: Vec<Value> = sids
        .iter()
        .map(|s| {
            let any = cache.get_org(s, true);
            json!({
                "sid": s,
                "any": match &any { Ok(o) => json!(o), Err(e) => raised(None, e) },
                "text": any.ok().flatten().map(|o| scw_store::json::dumps_compact(
                    &serde_json::to_value(&o).unwrap()).unwrap()),
                "fresh": cache.get_org(s, false).map(|o| o.is_some()).unwrap_or(false),
            })
        })
        .collect();
    let partials: Vec<Value> = cache
        .partials()
        .unwrap()
        .into_iter()
        .map(|p| json!([p.handle, p.data, p.observed_at, p.unknown, p.reason]))
        .collect();
    let member_orgs: Map<String, Value> = members
        .iter()
        .map(|h| (h.clone(), json!(cache.member_orgs(h).unwrap())))
        .collect();
    let mut stats = cache.stats().unwrap();
    stats.as_object_mut().unwrap().remove("bytes");

    let log_path = out.with_extension("log");
    let log = Arc::new(RequestLog {
        path: log_path.clone(),
        process: "compare".into(),
        utc: true,
    });
    let mut offline = Map::new();
    for (name, key) in [("nokey", None), ("key", Some("EXMPLKEY".to_string()))] {
        let client = Client::new(
            cache.clone(),
            Arc::new(Refuse),
            clock.clone(),
            Options {
                api_key: key,
                min_interval: 0.0,
                log: Some(log.clone()),
                ..Options::default()
            },
        );
        let mut got = Vec::new();
        for h in &handles {
            let c = client.citizen(h, false, true);
            let e = client.exists(h, true);
            got.push(json!({
                "handle": h,
                "citizen": match &c { Ok(c) => citizen(c, clock.time()), Err(e) => raised(Some(&client), e) },
                "exists": match &e { Ok(v) => json!(v), Err(e) => raised(Some(&client), e) },
            }));
        }
        let complete = client
            .complete_partials(false)
            .map(|n| json!(n))
            .unwrap_or_else(|e| raised(Some(&client), &e));
        offline.insert(name.into(), json!({"lookups": got, "complete": complete}));
    }
    let log_lines: Vec<String> = std::fs::read_to_string(&log_path)
        .map(|t| t.lines().map(String::from).collect())
        .unwrap_or_default();
    let _ = std::fs::remove_file(&log_path);
    drop(cache);
    let result = json!({
        "citizens": citizens, "orgs": orgs, "partials": partials,
        "member_orgs": member_orgs, "stats": stats, "offline": offline,
        "log": log_lines, "rows": rows(path),
    });
    std::fs::write(out, serde_json::to_string(&result).unwrap()).unwrap();
}
