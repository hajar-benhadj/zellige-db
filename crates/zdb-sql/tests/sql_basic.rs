//! End-to-end SQL tests: DDL, DML, expressions, transactions, persistence,
//! and the crash semantics of auto-commit statements.

use zdb_core::DbError;
use zdb_sql::output::render;
use zdb_sql::{Output, SqlEngine};

/// Integration tests cannot see the crate's `#[cfg(test)]` helpers, so they
/// carry their own minimal self-cleaning temp dir (see `zdb-core/src/testing.rs`).
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

fn engine(tag: &str) -> (TempDir, SqlEngine) {
    let dir = TempDir::new(tag);
    let engine = SqlEngine::create(dir.path().join("s.zdb")).unwrap();
    (dir, engine)
}

fn rows(output: Output) -> Vec<Vec<zdb_sql::types::Value>> {
    match output {
        Output::Query { rows, .. } => rows,
        Output::Command { tag } => panic!("expected query, got command {tag}"),
    }
}

fn text(v: &zdb_sql::types::Value) -> String {
    v.display()
}

#[test]
fn create_insert_select_roundtrip() {
    let (_dir, mut e) = engine("basic");
    e.execute("CREATE TABLE users (id INTEGER, name TEXT, active BOOLEAN)")
        .unwrap();
    e.execute(
        "INSERT INTO users VALUES (1, 'hajar', TRUE), (2, 'amine', FALSE), (3, 'sara', TRUE)",
    )
    .unwrap();

    let out = e.execute("SELECT id, name, active FROM users").unwrap();
    let r = rows(out);
    assert_eq!(r.len(), 3);
    assert_eq!(text(&r[0][1]), "hajar");
    assert_eq!(text(&r[2][0]), "3");
    assert_eq!(text(&r[1][2]), "false");
}

#[test]
fn where_clauses_and_operators() {
    let (_dir, mut e) = engine("where");
    e.execute("CREATE TABLE t (id INTEGER, name TEXT, score INTEGER)")
        .unwrap();
    e.execute(
        "INSERT INTO t VALUES (1, 'apple', 10), (2, 'banana', 25), (3, 'cherry', 7), (4, 'apricot', 25)",
    )
    .unwrap();

    let q = |e: &mut SqlEngine, sql: &str| -> Vec<String> {
        rows(e.execute(sql).unwrap())
            .into_iter()
            .map(|r| text(&r[0]))
            .collect()
    };

    assert_eq!(
        q(&mut e, "SELECT id FROM t WHERE score = 25"),
        vec!["2", "4"]
    );
    assert_eq!(
        q(&mut e, "SELECT id FROM t WHERE score > 9 AND score < 26"),
        vec!["1", "2", "4"]
    );
    assert_eq!(
        q(&mut e, "SELECT id FROM t WHERE name LIKE 'ap%'"),
        vec!["1", "4"]
    );
    assert_eq!(
        q(&mut e, "SELECT id FROM t WHERE name LIKE '_anana'"),
        vec!["2"]
    );
    assert_eq!(
        q(&mut e, "SELECT id FROM t WHERE NOT (score = 25)"),
        vec!["1", "3"]
    );
    assert_eq!(
        q(&mut e, "SELECT id FROM t WHERE score % 2 = 1"),
        vec!["2", "3", "4"]
    );
}

#[test]
fn order_by_and_limit() {
    let (_dir, mut e) = engine("order");
    e.execute("CREATE TABLE t (id INTEGER, score INTEGER)")
        .unwrap();
    e.execute("INSERT INTO t VALUES (1, 30), (2, 10), (3, 20), (4, 10)")
        .unwrap();

    let out = e
        .execute("SELECT id FROM t ORDER BY score ASC, id ASC LIMIT 3")
        .unwrap();
    assert_eq!(
        rows(out).iter().map(|r| text(&r[0])).collect::<Vec<_>>(),
        vec!["2", "4", "3"]
    );

    let out = e
        .execute("SELECT id FROM t ORDER BY score DESC LIMIT 2")
        .unwrap();
    assert_eq!(
        rows(out).iter().map(|r| text(&r[0])).collect::<Vec<_>>(),
        vec!["1", "3"]
    );
}

#[test]
fn update_and_delete() {
    let (_dir, mut e) = engine("dml");
    e.execute("CREATE TABLE t (id INTEGER, score INTEGER)")
        .unwrap();
    e.execute("INSERT INTO t VALUES (1, 10), (2, 20), (3, 30)")
        .unwrap();

    let out = e
        .execute("UPDATE t SET score = score + 5 WHERE id > 1")
        .unwrap();
    assert_eq!(
        out,
        Output::Command {
            tag: "UPDATE 2".into()
        }
    );

    let out = e.execute("SELECT score FROM t WHERE id = 3").unwrap();
    assert_eq!(text(&rows(out)[0][0]), "35");

    let out = e.execute("DELETE FROM t WHERE score < 20").unwrap();
    assert_eq!(
        out,
        Output::Command {
            tag: "DELETE 1".into()
        }
    );
    let out = e.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(text(&rows(out)[0][0]), "2");
}

#[test]
fn null_semantics() {
    let (_dir, mut e) = engine("nulls");
    e.execute("CREATE TABLE t (id INTEGER, note TEXT)").unwrap();
    e.execute("INSERT INTO t VALUES (1, 'x'), (2, NULL)")
        .unwrap();

    // NULL comparisons are false; IS NULL is the tool.
    let q = |e: &mut SqlEngine, sql: &str| -> usize { rows(e.execute(sql).unwrap()).len() };
    assert_eq!(q(&mut e, "SELECT id FROM t WHERE note = NULL"), 0);
    assert_eq!(q(&mut e, "SELECT id FROM t WHERE note IS NULL"), 1);
    assert_eq!(q(&mut e, "SELECT id FROM t WHERE note IS NOT NULL"), 1);

    let out = e.execute("SELECT id, note FROM t WHERE id = 2").unwrap();
    assert_eq!(text(&rows(out)[0][1]), "NULL");

    // NULLs order first (SQLite-compatible).
    let out = e.execute("SELECT id FROM t ORDER BY note ASC").unwrap();
    assert_eq!(text(&rows(out)[0][0]), "2");
}

#[test]
fn explicit_transactions_rollback_and_commit() {
    let (_dir, mut e) = engine("txn");
    e.execute("CREATE TABLE t (id INTEGER)").unwrap();

    e.execute("BEGIN").unwrap();
    e.execute("INSERT INTO t VALUES (1)").unwrap();
    e.execute("INSERT INTO t VALUES (2)").unwrap();
    e.execute("ROLLBACK").unwrap();
    let out = e.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(text(&rows(out)[0][0]), "0");

    e.execute("BEGIN").unwrap();
    e.execute("INSERT INTO t VALUES (1)").unwrap();
    e.execute("COMMIT").unwrap();
    let out = e.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(text(&rows(out)[0][0]), "1");
}

#[test]
fn failed_statement_rolls_back_its_own_transaction() {
    let (_dir, mut e) = engine("autoback");
    e.execute("CREATE TABLE t (id INTEGER)").unwrap();
    e.execute("INSERT INTO t VALUES (1)").unwrap();

    // Type error mid-statement: the whole statement is rolled back.
    let err = e
        .execute("INSERT INTO t VALUES ('not a number')")
        .unwrap_err();
    assert!(err.to_string().contains("type mismatch"), "got: {err}");
    let out = e.execute("SELECT COUNT(*) FROM t").unwrap();
    assert_eq!(text(&rows(out)[0][0]), "1");
}

#[test]
fn data_survives_reopen_like_a_restart() {
    let dir = TempDir::new("persist");
    let path = dir.path().join("p.zdb");
    {
        let mut e = SqlEngine::create(&path).unwrap();
        e.execute("CREATE TABLE users (id INTEGER, name TEXT)")
            .unwrap();
        e.execute("INSERT INTO users VALUES (1, 'hajar'), (2, 'amine')")
            .unwrap();
        // engine dropped: clean close (checkpoint)
    }
    let mut e = SqlEngine::open(&path).unwrap();
    let out = e.execute("SELECT name FROM users WHERE id = 2").unwrap();
    assert_eq!(text(&rows(out)[0][0]), "amine");
}

#[test]
fn show_tables_and_duplicates() {
    let (_dir, mut e) = engine("catalog");
    e.execute("CREATE TABLE alpha (x INTEGER)").unwrap();
    e.execute("CREATE TABLE beta (y TEXT)").unwrap();

    let out = e.execute("SHOW TABLES").unwrap();
    let names: Vec<String> = rows(out).iter().map(|r| text(&r[0])).collect();
    assert_eq!(names, vec!["alpha", "beta"]);

    assert!(matches!(
        e.execute("CREATE TABLE alpha (x INTEGER)").unwrap_err(),
        zdb_sql::types::SqlError::DuplicateTable(_)
    ));
}

#[test]
fn unknown_tables_and_columns_are_reported() {
    let (_dir, mut e) = engine("unknown");
    assert!(matches!(
        e.execute("SELECT * FROM nope").unwrap_err(),
        zdb_sql::types::SqlError::UnknownTable(_)
    ));
    e.execute("CREATE TABLE t (a INTEGER)").unwrap();
    assert!(matches!(
        e.execute("SELECT b FROM t").unwrap_err(),
        zdb_sql::types::SqlError::UnknownColumn(_)
    ));
}

#[test]
fn ascii_table_rendering_is_stable() {
    let (_dir, mut e) = engine("render");
    e.execute("CREATE TABLE t (id INTEGER, name TEXT)").unwrap();
    e.execute("INSERT INTO t VALUES (1, 'hajar')").unwrap();
    let rendered = render(&e.execute("SELECT id, name FROM t").unwrap());
    assert!(
        rendered.contains("│ id │ name  │"),
        "header line: {rendered}"
    );
    assert!(rendered.contains("│ 1  │ hajar │"), "data line: {rendered}");
    assert!(rendered.contains("(1 row)"), "footer: {rendered}");
}

#[test]
fn arithmetic_in_insert_and_where() {
    let (_dir, mut e) = engine("arith");
    e.execute("CREATE TABLE t (id INTEGER)").unwrap();
    e.execute("INSERT INTO t VALUES (2 * 21), (10 / 3), (-7)")
        .unwrap();
    let out = e
        .execute("SELECT id FROM t WHERE id = 42 OR id = 3 OR id = -7")
        .unwrap();
    assert_eq!(rows(out).len(), 3);
}

#[test]
fn storage_error_surfaces_as_sql_error() -> Result<(), DbError> {
    // Opening a non-database file must fail cleanly, not panic.
    let dir = TempDir::new("garbage");
    let path = dir.path().join("g.zdb");
    std::fs::write(&path, b"not a database at all..........")?;
    assert!(matches!(
        SqlEngine::open(&path),
        Err(DbError::Io(_) | DbError::BadMagic | DbError::ChecksumMismatch { .. })
    ));
    Ok(())
}
