//! Live updates (ui_server._stream): server-sent events, pushed when the
//! data actually changes - PRAGMA data_version on the two databases (it
//! moves whenever another connection commits, in any journal mode) and
//! settings.json's modification time - with a keepalive comment between.

use std::io::Write;
use std::path::PathBuf;

use rusqlite::{Connection, OpenFlags};
use serde_json::json;

use crate::Ui;

pub const POLL: f64 = 0.4;
pub const KEEPALIVE: f64 = 15.0;
/// With in-process changes: how often the files are looked at for writers
/// outside this process.
pub const OUTSIDE: f64 = 2.0;
/// After a wake: lets the commit that woke it land before it is read.
pub const SETTLE_MS: u64 = 50;

/// A number that changes whenever anyone commits to one SQLite file.
/// Read-only, opened lazily, never creating the file.
pub struct DataVersion {
    pub path: PathBuf,
    con: Option<Connection>,
}

impl DataVersion {
    pub fn new(path: PathBuf) -> Self {
        DataVersion { path, con: None }
    }

    pub fn get(&mut self) -> Option<i64> {
        if self.con.is_none() {
            if !self.path.exists() {
                return None;
            }
            self.con =
                Connection::open_with_flags(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok();
        }
        let v = self
            .con
            .as_ref()?
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .ok();
        if v.is_none() {
            self.con = None; // replaced or removed: reopen next time
        }
        v
    }
}

impl Ui {
    /// Writes events to `out` while `keep_going` says so: the state when it
    /// changes, a keepalive every KEEPALIVE seconds otherwise.
    pub fn stream(
        &self,
        out: &mut dyn Write,
        view: Option<&str>,
        arg: &str,
        keep_going: &dyn Fn() -> bool,
    ) {
        let mut watched = [
            DataVersion::new(self.db_path()),
            DataVersion::new(self.cache_path()),
        ];
        let settings = self.data().join("settings.json");
        let board = self.board.clone();
        let mut stamp = || {
            let mtime = std::fs::metadata(&settings)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            // The board's change number: the mining overlay's state moved.
            let shown = board.as_ref().map_or(0, |b| b.get().0);
            (watched[0].get(), watched[1].get(), mtime, shown)
        };
        let (mut last, mut last_beat) = (None, 0.0);
        while keep_going() {
            // Read before the stamp, so a commit in between is not missed.
            let seen = self.changes.as_ref().map(|c| c.seen());
            let now = self.now();
            let cur = stamp();
            let wrote = if last != Some(cur) {
                last = Some(cur);
                let frame = match self.collect(view, arg) {
                    Ok(v) => format!("data: {v}\n\n"),
                    Err(e) => format!("data: {}\n\n", json!({"error": e})),
                };
                last_beat = now;
                out.write_all(frame.as_bytes()).and_then(|_| out.flush())
            } else if now - last_beat >= KEEPALIVE {
                last_beat = now;
                out.write_all(b": keepalive\n\n").and_then(|_| out.flush())
            } else {
                Ok(())
            };
            if wrote.is_err() {
                return; // tab closed or reloaded
            }
            match (&self.changes, seen) {
                // In one process the watcher's commits wake the stream at
                // once; the files are still looked at now and then, for
                // anything writing from outside.
                (Some(ch), Some(s)) => {
                    ch.wait_past(s, std::time::Duration::from_secs_f64(OUTSIDE));
                    std::thread::sleep(std::time::Duration::from_millis(SETTLE_MS));
                }
                _ => self.clock.sleep(POLL),
            }
        }
    }
}
