//! sightings.csv - one row per settled contact, appended across sessions
//! (watch.SightingsLog), written as Python's csv module writes it: fields
//! quoted only when they must be, quotes doubled, CRLF line ends.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::NaiveDateTime;
use scw_vision::names::vote::Resolved;

pub const HEADER: [&str; 6] = [
    "timestamp",
    "name",
    "range_km",
    "votes",
    "total",
    "confidence",
];

pub struct SightingsLog {
    pub path: PathBuf,
    file: Option<File>,
}

fn field(s: &str) -> String {
    if s.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

impl SightingsLog {
    /// Opens for appending; a new file gets the header.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let new = !path.exists();
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        if new {
            file.write_all(format!("{}\r\n", HEADER.join(",")).as_bytes())?;
        }
        Ok(SightingsLog {
            path: path.to_path_buf(),
            file: Some(file),
        })
    }

    /// One row, stamped `at` (local time).
    pub fn write(&mut self, r: &Resolved, at: NaiveDateTime) -> std::io::Result<()> {
        let Some(f) = self.file.as_mut() else {
            return Ok(());
        };
        let row = [
            at.format("%Y-%m-%dT%H:%M:%S").to_string(),
            r.name.clone(),
            format!("{:.1}", r.range_km),
            r.votes.to_string(),
            r.total.to_string(),
            format!("{:.2}", r.confidence()),
        ];
        let line: Vec<String> = row.iter().map(|s| field(s)).collect();
        f.write_all(format!("{}\r\n", line.join(",")).as_bytes())
    }

    pub fn flush(&mut self) -> std::io::Result<()> {
        match self.file.as_mut() {
            Some(f) => f.flush(),
            None => Ok(()),
        }
    }

    pub fn close(&mut self) {
        self.file = None;
    }
}
