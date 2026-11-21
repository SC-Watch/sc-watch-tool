//! A flags.json as the port loads it, as JSON (read-only).
//!
//!     cargo run -p scw-store --example flags_dump -- <flags.json>
fn main() {
    let path = std::env::args().nth(1).expect("a flags.json");
    let f = scw_store::flags::Flags::load(std::path::Path::new(&path));
    let v: Vec<serde_json::Value> = f.all.iter().map(|x| x.to_json()).collect();
    println!("{}", serde_json::Value::Array(v));
}
