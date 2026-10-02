//! Secondary-index tests: correctness of maintenance under mutations,
//! planner equivalence (index scan vs full scan), and durability.

use zdb_sql::types::Value;
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

fn ids(engine: &mut SqlEngine, sql: &str) -> Vec<i64> {
    match engine.execute(sql).unwrap() {
        Output::Query { rows, .. } => rows
            .iter()
            .map(|r| match &r[0] {
                Value::Int(n) => *n,
                other => panic!("expected int, got {other:?}"),
            })
            .collect(),
        other => panic!("expected query, got {other:?}"),
    }
}

#[test]
fn index_scan_agrees_with_full_scan() {
    let dir = TempDir::new("idx-equiv");
    let mut e = SqlEngine::create(dir.path().join("i.zdb")).unwrap();
    e.execute("CREATE TABLE t (id INTEGER, score INTEGER)")
        .unwrap();
    e.execute("CREATE INDEX by_score ON t (score)").unwrap();
    for i in 1..=200i64 {
        let score = (i * 37) % 101;
        e.execute(&format!("INSERT INTO t VALUES ({i}, {score})"))
            .unwrap();
    }

    // The planner routes these through the index; a full scan must agree.
    assert_eq!(
        ids(&mut e, "SELECT id FROM t WHERE score = 42"),
        ids(&mut e, "SELECT id FROM t WHERE score = 42 + 0")
    );
    let via_index = ids(&mut e, "SELECT id FROM t WHERE score > 90 ORDER BY id");
    assert!(!via_index.is_empty());
    for (i, score) in (1..=200i64).map(|i| (i, (i * 37) % 101)) {
        if score > 90 {
            assert!(via_index.contains(&i), "row {i} (score {score}) missing");
        } else {
            assert!(!via_index.contains(&i));
        }
    }

    // Without ORDER BY, row order is unspecified: compare sorted sets.
    let mut got = ids(&mut e, "SELECT id FROM t WHERE score >= 100");
    got.sort_unstable();
    let want: Vec<i64> = (1..=200i64).filter(|i| (*i * 37) % 101 >= 100).collect();
    assert_eq!(got, want);

    let mut got = ids(&mut e, "SELECT id FROM t WHERE score < 2");
    got.sort_unstable();
    let want: Vec<i64> = (1..=200i64).filter(|i| (*i * 37) % 101 < 2).collect();
    assert_eq!(got, want);
}

#[test]
fn mutations_maintain_the_index() {
    let dir = TempDir::new("idx-maint");
    let mut e = SqlEngine::create(dir.path().join("i.zdb")).unwrap();
    e.execute("CREATE TABLE t (id INTEGER, name TEXT)").unwrap();
    e.execute("CREATE INDEX by_name ON t (name)").unwrap();
    e.execute("INSERT INTO t VALUES (1, 'alpha'), (2, 'beta'), (3, 'gamma')")
        .unwrap();

    assert_eq!(ids(&mut e, "SELECT id FROM t WHERE name = 'beta'"), vec![2]);

    // UPDATE moves the entry (old value must be removed, new added).
    e.execute("UPDATE t SET name = 'beta2' WHERE id = 2")
        .unwrap();
    assert_eq!(
        ids(&mut e, "SELECT id FROM t WHERE name = 'beta'"),
        Vec::<i64>::new()
    );
    assert_eq!(
        ids(&mut e, "SELECT id FROM t WHERE name = 'beta2'"),
        vec![2]
    );

    // DELETE removes the entry.
    e.execute("DELETE FROM t WHERE id = 2").unwrap();
    assert_eq!(
        ids(&mut e, "SELECT id FROM t WHERE name = 'beta2'"),
        Vec::<i64>::new()
    );

    // The other entries survive the maintenance churn.
    assert_eq!(
        ids(&mut e, "SELECT id FROM t WHERE name = 'alpha'"),
        vec![1]
    );
    assert_eq!(
        ids(&mut e, "SELECT id FROM t WHERE name = 'gamma'"),
        vec![3]
    );
}

#[test]
fn create_index_backfills_existing_rows() {
    let dir = TempDir::new("idx-backfill");
    let mut e = SqlEngine::create(dir.path().join("i.zdb")).unwrap();
    e.execute("CREATE TABLE t (id INTEGER, name TEXT)").unwrap();
    e.execute("INSERT INTO t VALUES (1, 'x'), (2, 'y'), (3, 'x')")
        .unwrap();

    // Index created AFTER the rows exist.
    e.execute("CREATE INDEX by_name ON t (name)").unwrap();
    assert_eq!(ids(&mut e, "SELECT id FROM t WHERE name = 'x'"), vec![1, 3]);

    // Drop the index: the planner falls back to a full scan, same answers.
    e.execute("DROP INDEX by_name").unwrap();
    assert_eq!(ids(&mut e, "SELECT id FROM t WHERE name = 'x'"), vec![1, 3]);
}

#[test]
fn index_survives_reopen() {
    let dir = TempDir::new("idx-restart");
    let path = dir.path().join("i.zdb");
    {
        let mut e = SqlEngine::create(&path).unwrap();
        e.execute("CREATE TABLE t (id INTEGER, name TEXT)").unwrap();
        e.execute("CREATE INDEX by_name ON t (name)").unwrap();
        e.execute("INSERT INTO t VALUES (1, 'keeper')").unwrap();
    }
    let mut e = SqlEngine::open(&path).unwrap();
    assert_eq!(
        ids(&mut e, "SELECT id FROM t WHERE name = 'keeper'"),
        vec![1]
    );
}

#[test]
fn null_values_are_not_indexed_but_still_queryable() {
    let dir = TempDir::new("idx-null");
    let mut e = SqlEngine::create(dir.path().join("i.zdb")).unwrap();
    e.execute("CREATE TABLE t (id INTEGER, name TEXT)").unwrap();
    e.execute("CREATE INDEX by_name ON t (name)").unwrap();
    e.execute("INSERT INTO t VALUES (1, NULL), (2, 'set')")
        .unwrap();

    // NULL lookup goes through IS NULL (no index), full-scan fallback.
    assert_eq!(ids(&mut e, "SELECT id FROM t WHERE name IS NULL"), vec![1]);
    assert_eq!(
        ids(&mut e, "SELECT id FROM t WHERE name IS NOT NULL"),
        vec![2]
    );
}
