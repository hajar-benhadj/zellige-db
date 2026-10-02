//! Storage-layer benchmarks (harness = false: a plain program, run by
//! `cargo bench -p zdb-core`). The numbers land in docs/benchmarks.md.

use std::hint::black_box;
use std::path::Path;
use std::time::Instant;

use zdb_core::{BTree, Pager};

/// Benches are compiled like integration tests: they carry their own
/// minimal self-cleaning temp dir (see `src/testing.rs`).
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
        "{name:<46} {ops:>9} ops  {elapsed:>10.1?}  {:>12.0} ops/s  {:>10.1} us/op",
        ops as f64 / elapsed.as_secs_f64(),
        micros / ops as f64
    );
}

fn bench(name: &str, ops: u64, f: impl FnOnce()) {
    let start = Instant::now();
    f();
    report(name, ops, start.elapsed());
}

fn key(i: u64) -> Vec<u8> {
    format!("key-{i:010}").into_bytes()
}

fn main() {
    println!("ZelligeDB storage benchmarks (release build)\n");

    let dir = TempDir::new("bench");
    let path = dir.path().join("bench.zdb");

    // 1-3. B+Tree inserts, lookups, range scans.
    {
        let mut pager = Pager::create(&path).unwrap();
        let mut tree = BTree::empty();
        const N: u64 = 100_000;
        pager.sync().unwrap();
        bench("btree insert, sequential 100k", N, || {
            for i in 0..N {
                tree.insert(&mut pager, &key(i), b"payload-0123456789")
                    .unwrap();
            }
        });
        pager.sync().unwrap();

        bench("btree get, 100k existing keys", N, || {
            for i in 0..N {
                black_box(tree.get(&mut pager, &key(i)).unwrap());
            }
        });

        bench("btree range scan, 100 x 1000 keys", 100_000, || {
            let mut total = 0u64;
            for start in (0..100u64).map(|s| s * 1000) {
                let scan = tree
                    .range(&mut pager, Some(&key(start)), Some(&key(start + 1000)))
                    .unwrap();
                total += scan.count() as u64;
            }
            black_box(total);
        });

        tree.check_integrity(&mut pager).unwrap();
    }

    // 4. WAL throughput: batched commits.
    {
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}.wal", path.display()));
        let mut db = zdb_core::Database::create(&path).unwrap();
        let mut tree = BTree::empty();
        const N: u64 = 50_000;
        bench("wal: 50k inserts in 10 batched txns", N, || {
            for batch in 0..10u64 {
                db.begin().unwrap();
                for i in batch * (N / 10)..(batch + 1) * (N / 10) {
                    tree.insert(&mut db, &key(i), b"payload-0123456789")
                        .unwrap();
                }
                db.commit().unwrap();
            }
        });
        db.checkpoint().unwrap();
    }

    println!("\ninteger key space: key-0000000000 .. key-0000099999");
}
