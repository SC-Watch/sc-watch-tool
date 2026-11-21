//! Compares the port's sketch() with the sketch the Python stored for every
//! player and every watchlist entry - opened READ-ONLY. Prints counts only.
//!
//!     cargo run -p scw-store --example sketch_check -- <sc-watch.db> [watchlist.db]
use rusqlite::{Connection, OpenFlags};

fn check(path: &str, sql: &str) {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).expect("open");
    let mut stmt = db.prepare(sql).expect("query");
    let rows: Vec<(String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("rows")
        .map(Result::unwrap)
        .collect();
    let differ = rows
        .iter()
        .filter(|(h, s)| scw_store::reputation::sketch(h) != *s)
        .count();
    println!("{}: {} stored sketches, {differ} differ", std::path::Path::new(path).file_name().unwrap().to_string_lossy(), rows.len());
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    check(&args[0], "SELECT handle, sketch FROM players WHERE sketch IS NOT NULL AND sketch != ''");
    if let Some(w) = args.get(1) {
        check(w, "SELECT handle, sketch FROM entries WHERE sketch IS NOT NULL AND sketch != ''");
    }
}
