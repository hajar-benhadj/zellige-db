//! Differential testing: ZelligeDB vs SQLite on identical random workloads.
//!
//! Gated behind the `diff-sqlite` feature (it links a C library) and meant
//! to run in CI on Linux; locally:
//!
//! ```bash
//! cargo test -p zdb-sql --features diff-sqlite
//! ```
//!
//! Both engines receive byte-identical SQL. We generate a well-typed
//! workload — the engines disagree legitimately on malformed types — and
//! compare result rows exactly, using `id` as an ORDER BY tiebreaker so
//! row order is fully determined on both sides.

#![cfg(feature = "diff-sqlite")]

use rusqlite::Connection;
use zdb_sql::SqlEngine;
use zdb_sql::types::Value;

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const NAMES: [&str; 6] = ["apple", "banana", "cherry", "date", "elderberry", "fig"];

fn seed_tables(engine: &mut SqlEngine, conn: &Connection, count: usize, rng: &mut Lcg) {
    engine
        .execute("CREATE TABLE t (id INTEGER, name TEXT, score INTEGER, active BOOLEAN)")
        .unwrap();
    conn.execute(
        "CREATE TABLE t (id INTEGER, name TEXT, score INTEGER, active INTEGER)",
        [],
    )
    .unwrap();

    for i in 0..count {
        let score = if rng.below(8) == 0 {
            "NULL".to_string()
        } else {
            (rng.below(200) as i64 - 100).to_string()
        };
        let active = if rng.below(8) == 0 {
            "NULL".to_string()
        } else if rng.below(2) == 0 {
            "TRUE".to_string()
        } else {
            "FALSE".to_string()
        };
        let name = NAMES[rng.below(NAMES.len() as u64) as usize];
        let zdb = format!("INSERT INTO t VALUES ({i}, '{name}', {score}, {active})");
        // SQLite stores booleans as 0/1 integers.
        let sqlite_active = active.replace("TRUE", "1").replace("FALSE", "0");
        let sqlite = format!("INSERT INTO t VALUES ({i}, '{name}', {score}, {sqlite_active})");
        engine.execute(&zdb).unwrap();
        conn.execute(&sqlite, []).unwrap();
    }
}

/// One random SELECT with 0-2 predicates, optional ORDER BY + LIMIT.
fn random_select(rng: &mut Lcg) -> String {
    let mut predicates = Vec::new();
    for _ in 0..rng.below(3) {
        match rng.below(4) {
            0 => {
                let n = rng.below(150) as i64 - 75;
                predicates.push(format!(
                    "score {} {n}",
                    ["<", ">", "="][rng.below(3) as usize]
                ));
            }
            1 => {
                let name = NAMES[rng.below(NAMES.len() as u64) as usize];
                predicates.push(format!("name = '{name}'"));
            }
            2 => {
                let prefix = &NAMES[rng.below(NAMES.len() as u64) as usize][..2];
                predicates.push(format!("name LIKE '{prefix}%'"));
            }
            _ => {
                let active = if rng.below(2) == 0 { "TRUE" } else { "FALSE" };
                predicates.push(format!("active = {active}"));
            }
        }
    }
    let where_clause = if predicates.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", predicates.join(" AND "))
    };
    let order = if rng.below(2) == 0 {
        let col = ["id", "name", "score"][rng.below(3) as usize];
        let dir = if rng.below(2) == 0 { "ASC" } else { "DESC" };
        format!(" ORDER BY {col} {dir}, id ASC")
    } else {
        String::new()
    };
    let limit = if rng.below(2) == 0 {
        format!(" LIMIT {}", 5 + rng.below(45))
    } else {
        String::new()
    };
    format!("SELECT id, name, score, active FROM t{where_clause}{order}{limit}")
}

fn zdb_rows(engine: &mut SqlEngine, sql: &str) -> Vec<Vec<Value>> {
    match engine.execute(sql).unwrap() {
        zdb_sql::Output::Query { rows, .. } => rows
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|v| match v {
                        // Normalize: SQLite stores booleans as 0/1 integers.
                        Value::Bool(b) => Value::Int(b as i64),
                        other => other,
                    })
                    .collect()
            })
            .collect(),
        other => panic!("expected query for {sql}: {other:?}"),
    }
}

fn sqlite_rows(conn: &Connection, sql: &str) -> Vec<Vec<Value>> {
    let mut stmt = conn.prepare(sql).unwrap();
    let columns: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    assert_eq!(columns, ["id", "name", "score", "active"]);
    let rows = stmt
        .query_map([], |row| {
            let id: Option<i64> = row.get(0)?;
            let name: Option<String> = row.get(1)?;
            let score: Option<i64> = row.get(2)?;
            let active: Option<i64> = row.get(3)?;
            Ok(vec![
                id.map(Value::Int).unwrap_or(Value::Null),
                name.map(Value::Text).unwrap_or(Value::Null),
                score.map(Value::Int).unwrap_or(Value::Null),
                active.map(Value::Int).unwrap_or(Value::Null),
            ])
        })
        .unwrap();
    rows.map(|r| r.unwrap())
        .map(|mut row| {
            // Normalize: ZelligeDB's BOOLEAN comes back as Bool, SQLite's as 0/1.
            if let Value::Bool(b) = row[3] {
                row[3] = Value::Int(b as i64);
            }
            row
        })
        .collect()
}

fn run_case(seed: u64) {
    let rng = &mut Lcg(seed | 1);
    let dir = std::env::temp_dir().join(format!("zdb-diff-{seed}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let mut engine = SqlEngine::create(dir.join("diff.zdb")).unwrap();
    let conn = Connection::open(dir.join("diff.sqlite")).unwrap();
    seed_tables(&mut engine, &conn, 300, rng);

    for q in 0..150 {
        let sql = random_select(rng);
        let ours = zdb_rows(&mut engine, &sql);
        let theirs = sqlite_rows(&conn, &sql);
        assert_eq!(
            ours, theirs,
            "seed {seed} query {q} diverged on:\n{sql}\nours: {ours:?}\nsqlite: {theirs:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn differential_against_sqlite() {
    for seed in 1..=4u64 {
        run_case(seed * 7919);
    }
}
