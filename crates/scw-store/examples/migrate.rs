//! Brings a sc-watch.db up to the current schema IN PLACE, as the Python's
//! Database() does on open. Use it on a COPY.
//!
//!     cargo run -p scw-store --example migrate -- <copy of sc-watch.db>
fn main() {
    let path = std::env::args().nth(1).expect("a database path");
    let db = scw_store::db::open(std::path::Path::new(&path)).expect("open");
    scw_store::reputation::migrate(&db).expect("migrate");
    println!("migrated {path}");
}
