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

/// A number that goes up whenever this process commits to a watched
/// database, and a way to wait for it - so a page can redraw the moment the
/// watcher settles a burst, instead of asking the files every 0.4 s.
#[derive(Debug, Default)]
pub struct Changes {
    seq: std::sync::Mutex<u64>,
    cv: std::sync::Condvar,
}

impl Changes {
    pub fn bump(&self) {
        *self.seq.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        self.cv.notify_all();
    }

    pub fn seen(&self) -> u64 {
        *self.seq.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Waits until the number passes `seen`, or `timeout` runs out; the
    /// number then.
    pub fn wait_past(&self, seen: u64, timeout: Duration) -> u64 {
        let g = self.seq.lock().unwrap_or_else(|e| e.into_inner());
        let (g, _) = self
            .cv
            .wait_timeout_while(g, timeout, |s| *s <= seen)
            .unwrap_or_else(|e| e.into_inner());
        *g
    }
}

/// Tells `changes` about every commit made on this connection. (A commit
/// hook, not a WAL hook: SQLite's automatic checkpoints run through the WAL
/// hook, and replacing it would let the log grow without end.)
pub fn watch(conn: &mut Connection, changes: std::sync::Arc<Changes>) {
    let _ = conn.commit_hook(Some(move || {
        changes.bump();
        false // let it commit
    }));
}

fn migrated() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static M: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    M.get_or_init(Default::default)
}

fn migration_key(path: &Path) -> String {
    let p = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    p.display().to_string().to_lowercase()
}

/// True the first time this process asks about a file, false after
/// (dbopen.migrated_once): schema work runs once per file per process, not
/// on every connection - each run is a commit, and in one process every
/// commit wakes the page.
pub fn migrated_once(path: &Path) -> bool {
    migrated()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(migration_key(path))
}

/// Forgets that a file was migrated (dbopen.forget): after a failed
/// migration, or for a test that rebuilds a database at the same path.
pub fn forget(path: Option<&Path>) {
    let mut m = migrated().lock().unwrap_or_else(|e| e.into_inner());
    match path {
        Some(p) => {
            m.remove(&migration_key(p));
        }
        None => m.clear(),
    }
}
