//! A settings.json COPY as the port reads it, typed, and saved unchanged
//! (into a second copy) - for tools/settings_compare.py.
//!
//!     settings_dump <settings.json copy> <copy to save into> <out.json>

use std::path::Path;

use scw_store::settings::{self, Settings};
use serde_json::{Map, json};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let loaded = settings::load(Path::new(&a[1]));
    let typed = Settings::from_map(&loaded)
        .map(|_| ())
        .map_err(|e| e.to_string());
    let saved = settings::save(&Map::new(), Path::new(&a[2])).unwrap();
    let out = json!({"load": loaded, "typed": typed.is_ok(), "typed_error": typed.err(),
                     "saved": saved});
    std::fs::write(&a[3], serde_json::to_string(&out).unwrap()).unwrap();
}
