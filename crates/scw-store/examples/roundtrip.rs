//! Reads JSON files and writes each back IN MEMORY, saying whether the text
//! comes out byte for byte the same - for files jsonstore.py last saved, it
//! should. Prints names, sizes and the verdict only, never contents.
//!
//!     cargo run -p scw-store --example roundtrip -- <file>...
fn main() {
    for arg in std::env::args().skip(1) {
        let p = std::path::Path::new(&arg);
        let name = p.file_name().unwrap().to_string_lossy();
        let Ok(text) = std::fs::read(p) else {
            println!("{name}: missing");
            continue;
        };
        match scw_store::json::read(p, scw_store::json::Expect::Anything) {
            None => println!("{name}: unreadable or empty ({} bytes)", text.len()),
            Some(v) => {
                let again = scw_store::json::file_text(&v).unwrap();
                println!(
                    "{name}: {} bytes, rewritten {}",
                    text.len(),
                    if again.as_bytes() == text.as_slice() {
                        "IDENTICAL"
                    } else {
                        "DIFFERENT"
                    }
                );
            }
        }
    }
}
