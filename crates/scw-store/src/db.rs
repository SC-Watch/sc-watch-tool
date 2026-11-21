//! Opening a SQLite file the way every part of sc-watch opens one
//! (dbopen.py), so the Python and the port can use the same files at once.
//!
//! WAL: readers and writers stay out of each other's way (measured in the
//! Python, the UI froze for up to 0.7 s behind a writer without it). The
//! mode belongs to the file; it is read on every open and set only when it
//! differs, because setting it takes a lock. `synchronous=NORMAL` is WAL's
//! documented safe pairing. A 5 s busy timeout absorbs the rest.
//!
//! The Python wraps one connection per thread behind a shared object,
//! because sharing a connection across threads gave wrong answers. Here a
//! connection cannot be shared across threads at all (it is not Sync); each
//! owner opens its own, which WAL makes cheap.

use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

pub const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

/// Opens (creating if need be) a database, configured as dbopen._open.
pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    let db = Connection::open(path)?;
    configure(&db);
    Ok(db)
}

/// Busy timeout, synchronous=NORMAL, WAL - failures ignored, as the Python
/// ignores them: a read-only folder or a share that refuses WAL still works
/// in the default mode, only slower under contention.
pub fn configure(db: &Connection) {
    let _ = db.busy_timeout(BUSY_TIMEOUT);
    let _ = db.execute_batch("PRAGMA synchronous=NORMAL");
    let mode: Option<String> = db.query_row("PRAGMA journal_mode", [], |r| r.get(0)).ok();
    if !mode.is_some_and(|m| m.eq_ignore_ascii_case("wal")) {
        let _ = db.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()));
    }
}

/// The column names of a table (PRAGMA table_info), in order.
pub fn columns(db: &Connection, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = db.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt.query_map([], |r| r.get::<_, String>(1))?;
    names.collect()
}
