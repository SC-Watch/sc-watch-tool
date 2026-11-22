//! Every watchlist answer for a list of handles, on a COPY of a
//! watchlist.db, as JSON - and, given a starter list, what seeding a fresh
//! database with it holds. For tools/watchlist_compare.py. The output names
//! real people and sources: keep it beside the copy, and delete both.
//!
//!     cargo run --release -p scw-store --example watchlist_dump -- <copy> <handles.txt> <starter.json> <fresh.db> <starter name>
use scw_store::watchlist::{Watchlist, seed_starter};
use serde_json::{Value, json};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mut wl = Watchlist::open(std::path::Path::new(&a[0])).unwrap();
    wl.starter_name = a[4].clone();
    let handles: Vec<String> = std::fs::read_to_string(&a[1])
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    let mut lookups = serde_json::Map::new();
    for h in &handles {
        let exact = wl.lookup(h, Some(false)).unwrap();
        let all = wl.lookup(h, Some(true)).unwrap();
        let j = |hits: &[scw_store::watchlist::Hit]| -> Value {
            hits.iter()
                .map(|x| {
                    let mut v = serde_json::to_value(x).unwrap();
                    v["describe"] = json!(x.describe());
                    v
                })
                .collect()
        };
        lookups.insert(h.clone(), json!([j(&exact), j(&all)]));
    }
    let mut stats = wl.stats().unwrap();
    stats.as_object_mut().unwrap().remove("path");
    let fresh = Watchlist::open(std::path::Path::new(&a[3])).unwrap();
    let seeded = seed_starter(std::path::Path::new(&a[2]), &fresh).unwrap();
    println!(
        "{}",
        json!({"lookups": lookups, "sources": wl.sources().unwrap(), "orgs": wl.orgs_flagged().unwrap(),
                          "stats": stats, "coverage": wl.reason_coverage().unwrap(), "seeded": seeded})
    );
}
