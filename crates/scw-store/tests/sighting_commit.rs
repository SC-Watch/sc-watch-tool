//! A sighting is one commit, as in the Python (reputation.record_sighting
//! commits once): the player and the sighting land together, so a page
//! never shows one without the other, and its live view wakes once.

use std::sync::Arc;

use scw_store::db::{Changes, watch};
use scw_store::reputation::Database;

#[test]
fn a_sighting_is_one_commit() {
    let dir = std::env::temp_dir().join(format!("scw-store-sighting-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut db = Database::open(&dir.join("sc-watch.db")).unwrap();
    let changes = Arc::new(Changes::default());
    watch(&mut db.conn, changes.clone());

    let before = changes.seen();
    assert_eq!(db.record_sighting("EXMPLONE", Some(1.5), 2, 3).unwrap(), 1);
    assert_eq!(changes.seen() - before, 1, "a new player and its sighting");

    let before = changes.seen();
    assert_eq!(db.record_sighting("EXMPLONE", None, 1, 1).unwrap(), 2);
    assert_eq!(changes.seen() - before, 1, "a player seen again");

    // Inside a caller's transaction it is part of that one.
    let before = changes.seen();
    db.conn.execute_batch("BEGIN").unwrap();
    db.record_sighting("EXMPLTWO", Some(3.0), 1, 1).unwrap();
    assert_eq!(changes.seen(), before, "nothing committed yet");
    db.conn.execute_batch("COMMIT").unwrap();
    assert_eq!(changes.seen() - before, 1);
    let n: i64 = db
        .conn
        .query_row("SELECT COUNT(*) FROM sightings", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 3);
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}
