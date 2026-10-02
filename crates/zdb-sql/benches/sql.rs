//! SQL-layer benchmarks (harness = false; numbers land in
//! docs/benchmarks.md). These double as the measurement harness for the
//! documented optimization stories:
//!
//! 1. index scan vs full scan (the planner's choice),
//! 2. batched transactions vs per-statement auto-commit (the fsync cost).

use std::path::Path;
use std::time::Instant;

use zdb_sql::{Output, SqlEngine};

/// Benches are compiled like integration tests: they carry their own
/// minimal self-cleaning temp dir (see `zdb-core/src/testing.rs`).
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

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn report(name: &str, ops: u64, elapsed: std::time::Duration) {
    let micros = elapsed.as_secs_f64() * 1_000_000.0;
    println!(
        "{name:<48} {ops:>9} ops  {elapsed:>10.1?}  {:>12.0} ops/s  {:>10.1} us/op",
        ops as f64 / elapsed.as_secs_f64(),
        micros / ops as f64
    );
}

fn bench(name: &str, ops: u64, f: impl FnOnce()) {
    let start = Instant::now();
    f();
    report(name, ops, start.elapsed());
}

fn count_of(output: &Output) -> usize {
    match output {
        Output::Query { rows, .. } => rows.len(),
        _ => panic!("expected query"),
    }
}

fn main() {
    println!("ZelligeDB SQL benchmarks (release build)\n");
    let dir = TempDir::new("sqlbench");
    let path = dir.path().join("sql.zdb");
    let mut e = SqlEngine::create(&path).unwrap();

    e.execute("CREATE TABLE events (id INTEGER, kind TEXT, score INTEGER)")
        .unwrap();

    // 1. 10k auto-commit inserts (one fsync per statement).
    const N: u64 = 10_000;
    bench("sql insert, 10k auto-commit statements", N, || {
        for i in 0..N {
            e.execute(&format!(
                "INSERT INTO events VALUES ({i}, 'kind-{}', {})",
                i % 7,
                (i * 31) % 1000
            ))
            .unwrap();
        }
    });

    // 2. 10k batched inserts in one transaction — the fsync contrast.
    let mut e2 = SqlEngine::create(dir.path().join("sql2.zdb")).unwrap();
    e2.execute("CREATE TABLE events (id INTEGER, kind TEXT, score INTEGER)")
        .unwrap();
    bench("sql insert, 10k rows in one batched txn", N, || {
        e2.execute("BEGIN").unwrap();
        for i in 0..N {
            e2.execute(&format!(
                "INSERT INTO events VALUES ({i}, 'kind-{}', {})",
                i % 7,
                (i * 31) % 1000
            ))
            .unwrap();
        }
        e2.execute("COMMIT").unwrap();
    });

    // 3a. Point query BEFORE any index exists: the planner must full-scan.
    const Q: u64 = 200;
    let query = "SELECT id FROM events WHERE score = 500";
    bench("sql point query WITHOUT index (full scan)", Q, || {
        for _ in 0..Q {
            let out = e.execute(query).unwrap();
            assert!(count_of(&out) > 0);
        }
    });

    // 3b. The same query after CREATE INDEX: the planner picks the index.
    e.execute("CREATE INDEX by_score ON events (score)").unwrap();
    bench("sql point query WITH index (planner)", Q, || {
        for _ in 0..Q {
            let out = e.execute(query).unwrap();
            assert!(count_of(&out) > 0);
        }
    });

    // 4. UPDATEs with MVCC versioning.
    bench("sql update, 2k versioned updates", 2_000, || {
        for i in 0..2_000u64 {
            e.execute(&format!(
                "UPDATE events SET score = {} WHERE id = {}",
                i % 999,
                i
            ))
            .unwrap();
        }
    });

    println!("\nall numbers: this machine, release build, warm OS cache");
}
