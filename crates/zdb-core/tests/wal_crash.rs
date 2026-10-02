//! Crash safety: the tests that justify the WAL's existence.
//!
//! A "crash" is simulated without process signals: `std::mem::forget(db)`
//! abandons a database exactly like `kill -9` would — no `Drop`, no
//! checkpoint, OS buffers still dirty — and the journal file is then
//! truncated at adversarial offsets. After reopening, the invariant is
//! absolute:
//!
//! > every committed transaction is fully present, every uncommitted one
//! > is fully absent — whatever the crash point.

use std::io::Seek;
use std::path::Path;

use zdb_core::{BTree, Database, DbError};

/// Integration tests cannot see the crate's `#[cfg(test)]` helpers, so they
/// carry their own minimal self-cleaning temp dir (see `src/testing.rs`).
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

fn key(i: u32) -> Vec<u8> {
    format!("key-{i:04}").into_bytes()
}

fn insert_range(db: &mut Database, tree: &mut BTree, from: u32, to: u32) -> Result<(), DbError> {
    for i in from..=to {
        tree.insert(db, &key(i), format!("val-{i}").as_bytes())?;
    }
    Ok(())
}

fn present(db: &mut Database, tree: &BTree, i: u32) -> bool {
    tree.get(db, &key(i)).unwrap().is_some()
}

/// Committed and checkpointed baseline: `1..=30` live in the data file.
fn seed_committed_baseline(dir: &TempDir) -> Result<(), DbError> {
    let mut db = Database::create(dir.path().join("crash.zdb"))?;
    db.begin()?;
    let mut tree = db.open_tree("data")?;
    insert_range(&mut db, &mut tree, 1, 30)?;
    db.save_tree("data", &tree)?;
    db.commit()?;
    db.checkpoint()?;
    Ok(())
}

#[test]
fn uncommitted_transaction_vanishes_after_a_crash() -> Result<(), DbError> {
    let dir = TempDir::new("wal-uncommitted");
    seed_committed_baseline(&dir)?;

    let mut db = crash_mid_transaction(&dir, 31, 45)?;
    let tree = db.open_tree("data")?;

    for i in 1..=30 {
        assert!(present(&mut db, &tree, i), "committed key {i} must survive");
    }
    for i in 31..=45 {
        assert!(
            !present(&mut db, &tree, i),
            "uncommitted key {i} must vanish"
        );
    }
    Ok(())
}

#[test]
fn committed_transaction_survives_a_crash_before_checkpoint() -> Result<(), DbError> {
    let dir = TempDir::new("wal-committed");
    seed_committed_baseline(&dir)?;

    // Committed (fsynced) but the process died before any checkpoint:
    // recovery must replay the whole transaction from the journal alone.
    {
        let mut db = Database::open(dir.path().join("crash.zdb"))?;
        db.begin()?;
        let mut tree = db.open_tree("data")?;
        insert_range(&mut db, &mut tree, 31, 45)?;
        db.save_tree("data", &tree)?;
        db.commit()?;
        std::mem::forget(db); // crash right after the commit fsync
    }

    let mut db = Database::open(dir.path().join("crash.zdb"))?;
    let tree = db.open_tree("data")?;
    for i in 1..=45 {
        assert!(
            present(&mut db, &tree, i),
            "committed key {i} must be replayed by recovery"
        );
    }
    Ok(())
}

fn crash_mid_transaction(dir: &TempDir, from: u32, to: u32) -> Result<Database, DbError> {
    let mut db = Database::open(dir.path().join("crash.zdb"))?;
    db.begin()?;
    let mut tree = db.open_tree("data")?;
    insert_range(&mut db, &mut tree, from, to)?;
    db.save_tree("data", &tree)?;
    std::mem::forget(db); // kill -9: no Drop, no rollback, no checkpoint
    Database::open(dir.path().join("crash.zdb"))
}

#[test]
fn committed_and_uncommitted_across_a_random_torn_tail() -> Result<(), DbError> {
    // The marquee test: commit txn A fully, leave txn B committed in the
    // journal only, then tear the journal at 200 adversarial offsets.
    // Whatever survives of the log, A must always be intact and B must be
    // intact exactly when its commit frame made it through un-torn.
    let dir = TempDir::new("wal-torn");
    let wal = dir.path().join("crash.zdb.wal");

    for cut in (0..200).map(|i| i * 137) {
        let _ = std::fs::remove_file(dir.path().join("crash.zdb"));
        let _ = std::fs::remove_file(&wal);
        seed_two_transactions(&dir)?;

        let wal_len = std::fs::metadata(&wal)?.len();
        if cut > wal_len {
            break; // past the end: the two committed transactions covered it
        }
        // Crash: forget the handle, then tear the journal at `cut`.
        std::fs::OpenOptions::new()
            .write(true)
            .open(&wal)?
            .set_len(cut)?;

        let mut db = Database::open(dir.path().join("crash.zdb"))?;
        let tree = db.open_tree("data")?;
        for i in 1..=30 {
            assert!(
                present(&mut db, &tree, i),
                "cut {cut}: checkpointed key {i} lost"
            );
        }
        // B's frames (including its commit marker) are the entire journal;
        // any tear removes the commit marker, so B survives only un-torn.
        let b_expected = cut == wal_len;
        for i in 31..=60 {
            assert_eq!(
                present(&mut db, &tree, i),
                b_expected,
                "cut {cut}: key {i} wrong after recovery"
            );
        }
    }
    Ok(())
}

fn seed_two_transactions(dir: &TempDir) -> Result<(), DbError> {
    let mut db = Database::create(dir.path().join("crash.zdb"))?;

    // Transaction A: durable and checkpointed into the data file.
    db.begin()?;
    let mut tree = db.open_tree("data")?;
    insert_range(&mut db, &mut tree, 1, 30)?;
    db.save_tree("data", &tree)?;
    db.commit()?;
    db.checkpoint()?;

    // Transaction B: durable in the journal only (no checkpoint).
    db.begin()?;
    insert_range(&mut db, &mut tree, 31, 60)?;
    db.save_tree("data", &tree)?;
    db.commit()?;
    std::mem::forget(db); // crash before any checkpoint
    Ok(())
}

#[test]
fn recovery_repairs_a_torn_data_file_page() -> Result<(), DbError> {
    let dir = TempDir::new("wal-repair");
    seed_committed_baseline(&dir)?;

    // A committed transaction whose writes touch the leftmost leaves
    // (keys sorting before every existing key), left un-checkpointed.
    {
        let mut db = Database::open(dir.path().join("crash.zdb"))?;
        db.begin()?;
        let mut tree = db.open_tree("data")?;
        for i in 1..=200u32 {
            let k = format!("aaa-{i:04}");
            tree.insert(&mut db, k.as_bytes(), format!("val-{i}").as_bytes())?;
        }
        db.save_tree("data", &tree)?;
        db.commit()?;
        std::mem::forget(db); // crash before checkpoint: journal has it all
    }

    // Tear the DATA file inside page 1 — the leftmost leaf, which the
    // journal's replayed images must rebuild byte for byte.
    let data_file = dir.path().join("crash.zdb");
    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new().write(true).open(&data_file)?;
        f.seek(std::io::SeekFrom::Start(4100))?; // page 1 payload
        f.write_all(&[0xDE; 64])?;
        f.sync_all()?;
    }

    let mut db = Database::open(&data_file)?;
    let tree = db.open_tree("data")?;
    for i in 1..=200u32 {
        assert!(
            tree.get(&mut db, format!("aaa-{i:04}").as_bytes())?
                .is_some(),
            "aaa key {i} missing after repair"
        );
    }
    for i in 1..=30 {
        assert!(
            present(&mut db, &tree, i),
            "key {i} must be repaired from the journal"
        );
    }
    Ok(())
}

#[test]
fn rollback_discards_exactly_the_open_transaction() -> Result<(), DbError> {
    let dir = TempDir::new("wal-rollback");
    seed_committed_baseline(&dir)?;

    let mut db = Database::open(dir.path().join("crash.zdb"))?;
    db.begin()?;
    let mut tree = db.open_tree("data")?;
    insert_range(&mut db, &mut tree, 31, 40)?;
    db.rollback()?;

    // A committed transaction after a rollback still works, and the
    // rolled-back keys never appear.
    db.begin()?;
    insert_range(&mut db, &mut tree, 41, 45)?;
    db.save_tree("data", &tree)?;
    db.commit()?;
    db.checkpoint()?;

    for i in 1..=30 {
        assert!(present(&mut db, &tree, i));
    }
    for i in 31..=40 {
        assert!(!present(&mut db, &tree, i), "rolled-back key {i} leaked");
    }
    for i in 41..=45 {
        assert!(present(&mut db, &tree, i));
    }
    Ok(())
}

#[test]
fn catalog_roots_survive_a_crash() -> Result<(), DbError> {
    let dir = TempDir::new("wal-catalog");
    {
        let mut db = Database::create(dir.path().join("cat.zdb"))?;
        db.begin()?;
        let mut tree = db.open_tree("items")?;
        insert_range(&mut db, &mut tree, 1, 10)?;
        db.save_tree("items", &tree)?;
        db.commit()?;
        db.checkpoint()?;
    }
    let mut db = Database::open(dir.path().join("cat.zdb"))?;
    let tree = db.open_tree("items")?;
    for i in 1..=10 {
        assert!(present(&mut db, &tree, i), "catalog lost the tree root");
    }
    assert!(!present(&mut db, &tree, 11));
    Ok(())
}
