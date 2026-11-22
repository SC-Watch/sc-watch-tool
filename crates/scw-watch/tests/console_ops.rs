//! The terminal against watch.Console itself
//! (tests/fixtures/make_console_ops.py): the same script of announcements
//! and status lines writes the same characters, carriage returns and
//! blanking included, at three widths and to a redirected output.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use scw_watch::console::Terminal;
use scw_watch::sinks::Console;
use serde_json::Value;

#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn terminal_matches_watch_py() {
    let fx: Value = serde_json::from_slice(
        &std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/console_ops.json"),
        )
        .unwrap(),
    )
    .unwrap();
    for run in fx["runs"].as_array().unwrap() {
        let buf = Buf::default();
        let cols = run["columns"].as_u64().unwrap() as usize;
        let t = Terminal::new(
            Box::new(buf.clone()),
            run["live"].as_bool().unwrap(),
            Box::new(move || cols),
        );
        for op in fx["script"].as_array().unwrap() {
            let arg = op.get(1).and_then(Value::as_str).unwrap_or("");
            match op[0].as_str().unwrap() {
                "status" => t.status(arg),
                "say" => t.say(arg),
                "done" => t.done(),
                other => panic!("{other}"),
            }
        }
        let got = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert_eq!(
            got,
            run["out"].as_str().unwrap(),
            "live {} at {cols} columns",
            run["live"]
        );
    }
}
