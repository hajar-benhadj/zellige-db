//! Multi-session tests: snapshot isolation, write conflicts, and the
//! single-writer rule — the behaviors ADR-0006 promises.

use zdb_sql::types::{SqlError, Value};
use zdb_sql::{Output, SqlEngine};

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("zdb-{tag}-{}-{id}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

fn count(engine: &mut SqlEngine, sql: &str) -> i64 {
    match engine.execute(sql).unwrap() {
        Output::Query { rows, .. } => match &rows[0][0] {
            Value::Int(n) => *n,
            other => panic!("expected count, got {other:?}"),
        },
        other => panic!("expected query, got {other:?}"),
    }
}

fn seeded(tag: &str) -> (TempDir, SqlEngine, SqlEngine) {
    let dir = TempDir::new(tag);
    let mut e1 = SqlEngine::create(dir.path().join("mvcc.zdb")).unwrap();
    let e2 = e1.clone();
    e1.execute("CREATE TABLE t (id INTEGER, score INTEGER)")
        .unwrap();
    e1.execute("INSERT INTO t VALUES (1, 10), (2, 20), (3, 30)")
        .unwrap();
    (dir, e1, e2)
}

#[test]
fn own_writes_are_visible_then_atomically_published() {
    let (_dir, mut s1, mut s2) = seeded("iso");

    s1.execute("BEGIN").unwrap();
    let before = count(&mut s1, "SELECT COUNT(*) FROM t");
    assert_eq!(before, 3);

    // The writer sees its own uncommitted rows...
    s1.execute("INSERT INTO t VALUES (4, 40), (5, 50)").unwrap();
    assert_eq!(count(&mut s1, "SELECT COUNT(*) FROM t"), 5);
    // ...while every other session still sees exactly three: no dirty
    // reads, and nothing is published until the commit lands.
    assert_eq!(count(&mut s2, "SELECT COUNT(*) FROM t"), 3);

    s1.execute("COMMIT").unwrap();
    assert_eq!(count(&mut s1, "SELECT COUNT(*) FROM t"), 5);
    assert_eq!(count(&mut s2, "SELECT COUNT(*) FROM t"), 5);
}

#[test]
fn uncommitted_rows_from_other_sessions_are_invisible() {
    let (_dir, mut s1, mut s2) = seeded("dirty");

    s2.execute("BEGIN").unwrap();
    s2.execute("INSERT INTO t VALUES (9, 90)").unwrap();
    // s2 sees its own write...
    assert_eq!(count(&mut s2, "SELECT COUNT(*) FROM t"), 4);
    // ...but s1 does not: no dirty reads.
    assert_eq!(count(&mut s1, "SELECT COUNT(*) FROM t"), 3);
    s2.execute("ROLLBACK").unwrap();
    assert_eq!(count(&mut s1, "SELECT COUNT(*) FROM t"), 3);
}

#[test]
fn rollback_discards_versions_without_traces() {
    let (_dir, mut s1, mut s2) = seeded("versions");

    s1.execute("BEGIN").unwrap();
    s1.execute("UPDATE t SET score = 99 WHERE id = 1").unwrap();

    // The writer sees its own update...
    let out = match s1.execute("SELECT score FROM t WHERE id = 1").unwrap() {
        Output::Query { rows, .. } => rows,
        _ => panic!("query"),
    };
    assert_eq!(out[0][0], Value::Int(99));
    // ...others see the committed version.
    let out = match s2.execute("SELECT score FROM t WHERE id = 1").unwrap() {
        Output::Query { rows, .. } => rows,
        _ => panic!("query"),
    };
    assert_eq!(out[0][0], Value::Int(10));

    // Rollback tombstones nothing permanently: the old version stands.
    s1.execute("ROLLBACK").unwrap();
    let out = match s1.execute("SELECT score FROM t WHERE id = 1").unwrap() {
        Output::Query { rows, .. } => rows,
        _ => panic!("query"),
    };
    assert_eq!(out[0][0], Value::Int(10));
    assert_eq!(count(&mut s1, "SELECT COUNT(*) FROM t"), 3);
}

#[test]
fn single_writer_rule_reports_a_lock() {
    let (_dir, mut s1, mut s2) = seeded("lock");

    s1.execute("BEGIN").unwrap();
    s1.execute("INSERT INTO t VALUES (7, 70)").unwrap();

    // Another session cannot start its own explicit transaction...
    let err = s2.execute("BEGIN").unwrap_err();
    assert!(matches!(err, SqlError::Locked), "got: {err}");
    // ...nor sneak an auto-commit write past the lock.
    let err = s2.execute("INSERT INTO t VALUES (8, 80)").unwrap_err();
    assert!(matches!(err, SqlError::Locked), "got: {err}");

    // Reads still work (MVCC hides s1's uncommitted row).
    assert_eq!(count(&mut s2, "SELECT COUNT(*) FROM t"), 3);

    s1.execute("COMMIT").unwrap();
    // Lock released: s2 can now write.
    s2.execute("INSERT INTO t VALUES (8, 80)").unwrap();
    assert_eq!(count(&mut s2, "SELECT COUNT(*) FROM t"), 5);
}

#[test]
fn mvcc_state_survives_a_restart() {
    let dir = TempDir::new("mvcc-restart");
    let path = dir.path().join("r.zdb");

    {
        let mut e1 = SqlEngine::create(&path).unwrap();
        let mut e2 = e1.clone();
        e1.execute("CREATE TABLE t (id INTEGER)").unwrap();
        e1.execute("INSERT INTO t VALUES (1)").unwrap();
        // Uncommitted work from another session dies with the process —
        // and a clean Drop of e1 must not accidentally commit it.
        e2.execute("BEGIN").unwrap();
        e2.execute("INSERT INTO t VALUES (2)").unwrap();
        drop(e2);
        drop(e1);
    }

    let mut fresh = SqlEngine::open(&path).unwrap();
    assert_eq!(count(&mut fresh, "SELECT COUNT(*) FROM t"), 1);
}
