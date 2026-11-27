//! Each write is one commit, as in the Python (its sqlite3 opens a
//! transaction before the first write and the function commits once): a
//! page's live view, woken per commit, never sees a write half done, and
//! wakes once. confirm_audit is the one counted at two, as the Python's
//! shape is (see there). A failed write commits nothing, and leaves no
//! transaction open behind it.

use std::path::PathBuf;
use std::sync::Arc;

use scw_store::db::{Changes, watch};
use scw_store::reputation::{Database, PENDING, ProfileOrg, RosterMember};
use scw_store::watchlist::{ASSERTED, Watchlist};
use serde_json::json;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("scw-store-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// How many commits `f` made.
fn commits<T>(changes: &Changes, f: impl FnOnce() -> T) -> (u64, T) {
    let before = changes.seen();
    let out = f();
    (changes.seen() - before, out)
}

fn member(h: &str) -> RosterMember {
    RosterMember {
        handle: h.into(),
        display: format!("{h} display"),
        rank: "Rank".into(),
        stars: 2,
    }
}

fn count(db: &Database, sql: &str) -> i64 {
    db.conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn each_reputation_write_is_one_commit() {
    let dir = temp_dir("write-commits");
    let mut db = Database::open(&dir.join("sc-watch.db")).unwrap();
    let changes = Arc::new(Changes::default());
    watch(&mut db.conn, changes.clone());
    let one = |what: &str, n: u64| assert_eq!(n, 1, "{what}");

    db.flag_org("EXMPLORG", "", "Example Org").unwrap();

    // A complete roster of three, then a complete one of one: two leave.
    let roster: Vec<_> = ["EXMPLONE", "EXMPLTWO", "EXMPLTHREE"]
        .into_iter()
        .map(member)
        .collect();
    let (n, r) = commits(&changes, || {
        db.record_roster("EXMPLORG", &roster, true, None).unwrap()
    });
    one("record_roster, three joining", n);
    assert_eq!(r.joined.len(), 3);
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM player_orgs WHERE source='roster'"
        ),
        3
    );
    let (n, r) = commits(&changes, || {
        db.record_roster("EXMPLORG", &roster[..1], true, None)
            .unwrap()
    });
    one("record_roster, two leaving", n);
    let mut left = r.left.clone();
    left.sort();
    assert_eq!(left, ["EXMPLTHREE", "EXMPLTWO"]);
    assert_eq!(
        count(&db, "SELECT roster_n FROM org_flags WHERE sid='EXMPLORG'"),
        1
    );

    let handles = vec!["EXMPLTWO".to_string(), " ".into(), "EXMPLTHREE".into()];
    let (n, added) = commits(&changes, || {
        db.add_org_members("EXMPLORG", &handles, false).unwrap()
    });
    one("add_org_members, truncated", n);
    assert_eq!(added, 2);
    let (n, _) = commits(&changes, || {
        db.add_org_members("EXMPLORG", &handles, true).unwrap()
    });
    one("add_org_members, replacing", n);

    let (n, c) = commits(&changes, || {
        db.add_member_manually("EXMPLORG", "EXMPLFOUR", "by hand")
            .unwrap()
    });
    one("add_member_manually, new", n);
    assert_eq!(c.state.as_deref(), Some("added"));
    let (n, c) = commits(&changes, || {
        db.add_member_manually("EXMPLORG", "EXMPLONE", "").unwrap()
    });
    one("add_member_manually, listed", n);
    assert_eq!(c.state.as_deref(), Some("listed"));
    let (n, c) = commits(&changes, || {
        db.remove_manual_member("EXMPLORG", "EXMPLFOUR").unwrap()
    });
    one("remove_manual_member, removed", n);
    assert_eq!(c.state.as_deref(), Some("removed"));
    let (n, c) = commits(&changes, || {
        db.remove_manual_member("EXMPLORG", "EXMPLONE").unwrap()
    });
    one("remove_manual_member, still listed", n);
    assert_eq!(c.state.as_deref(), Some("still_listed"));

    let orgs = [
        ProfileOrg {
            sid: "EXMPLMAIN".into(),
            is_main: true,
        },
        ProfileOrg {
            sid: "EXMPLSIDE".into(),
            is_main: false,
        },
    ];
    let (n, _) = commits(&changes, || db.set_orgs("EXMPLONE", &orgs).unwrap());
    one("set_orgs", n);
    let (n, _) = commits(&changes, || {
        db.set_org_manual("EXMPLMAIN", "Main", "", "").unwrap()
    });
    one("set_org_manual", n);
    let (n, _) = commits(&changes, || {
        db.set_org_manual("EXMPLMAIN", "", "", "").unwrap()
    });
    one("set_org_manual, cleared", n);

    let (n, _) = commits(&changes, || {
        db.add_report("EXMPLONE", PENDING, "", "", "self", false)
            .unwrap()
    });
    one("add_report, resolved", n);
    let (n, _) = commits(&changes, || {
        db.add_report("EXMPLNEVER", PENDING, "", "", "self", true)
            .unwrap()
    });
    one("add_report, exact", n);
    let (n, _) = commits(&changes, || {
        db.set_manual_enlisted("EXMPLONE", "2020-01-01").unwrap()
    });
    one("set_manual_enlisted", n);
    let (n, _) = commits(&changes, || db.set_manual_enlisted("EXMPLONE", "").unwrap());
    one("set_manual_enlisted, cleared", n);

    // An audit confirmed as read: its own two writes, one commit.
    db.record_sighting("EXMPLONE", Some(1.0), 1, 3).unwrap();
    let audit = |h: &str| {
        db.add_audit(h, Some(1.0), 1, 3, &json!([h]), "", "", "", "weak_read")
            .unwrap()
    };
    let id = audit("EXMPLONE");
    let (n, c) = commits(&changes, || {
        db.confirm_audit(id, "EXMPLONE", false).unwrap()
    });
    one("confirm_audit, as read", n);
    assert!(c.ok);
    // Corrected to a spelling with no record: the rename commits, then the
    // audit's writes - two, as the Python's.
    db.record_sighting("EXMPLFIVE", None, 1, 3).unwrap();
    let id = audit("EXMPLFIVE");
    let (n, c) = commits(&changes, || {
        db.confirm_audit(id, "EXMPLSIX", false).unwrap()
    });
    assert_eq!(n, 2, "confirm_audit, renamed");
    assert_eq!(c.renamed, Some(true));

    db.add_audit("EXMPLONE", None, 1, 3, &json!([]), "", "", "", "weak_read")
        .unwrap();
    let (n, removed) = commits(&changes, || db.forget("EXMPLONE").unwrap());
    one("forget", n);
    assert_eq!((removed.player, removed.audits), (1, 1));
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM players WHERE handle='EXMPLONE'"),
        0
    );

    let (n, _) = commits(&changes, || db.unflag_org("EXMPLORG").unwrap());
    one("unflag_org", n);
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM org_membership WHERE sid='EXMPLORG'"
        ),
        0
    );

    // Inside a caller's transaction a write is part of that one.
    db.flag_org("EXMPLORG", "", "").unwrap();
    let before = changes.seen();
    db.conn.execute_batch("BEGIN").unwrap();
    db.record_roster("EXMPLORG", &roster, true, None).unwrap();
    db.forget("EXMPLSIX").unwrap();
    assert_eq!(changes.seen(), before, "nothing committed yet");
    db.conn.execute_batch("COMMIT").unwrap();
    assert_eq!(changes.seen() - before, 1);

    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_failed_write_commits_nothing() {
    let dir = temp_dir("write-fails");
    let mut db = Database::open(&dir.join("sc-watch.db")).unwrap();
    db.record_sighting("EXMPLONE", None, 1, 1).unwrap();
    db.add_report("EXMPLONE", PENDING, "", "", "self", false)
        .unwrap();
    // forget()'s last DELETE now fails, after the others have run.
    db.conn.execute_batch("DROP TABLE player_manual").unwrap();
    let changes = Arc::new(Changes::default());
    watch(&mut db.conn, changes.clone());

    let (n, out) = commits(&changes, || db.forget("EXMPLONE"));
    assert!(out.is_err());
    assert_eq!(n, 0, "nothing committed");
    assert!(db.conn.is_autocommit(), "no transaction left open");
    for t in ["players", "sightings", "reports"] {
        assert_eq!(count(&db, &format!("SELECT COUNT(*) FROM {t}")), 1, "{t}");
    }
    drop(db);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn forgetting_a_watchlist_source_is_one_commit() {
    let dir = temp_dir("watchlist-commits");
    let mut wl = Watchlist::open(&dir.join("watchlist.db")).unwrap();
    let id = wl.add_source("Example list", "", "").unwrap();
    for (h, msg) in [("EXMPLONE", "1"), ("EXMPLTWO", "2")] {
        wl.add_entry(id, h, ASSERTED, "", "", "", "", msg, "")
            .unwrap();
    }
    wl.add_org(id, "EXMPLORG", "", "1").unwrap();
    let changes = Arc::new(Changes::default());
    watch(&mut wl.conn, changes.clone());

    let (n, gone) = commits(&changes, || wl.forget_source(id).unwrap());
    assert_eq!(n, 1);
    assert_eq!(gone, 2);
    for t in ["entries", "orgs", "sources"] {
        let left: i64 = wl
            .conn
            .query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0, "{t}");
    }
    drop(wl);
    let _ = std::fs::remove_dir_all(&dir);
}
