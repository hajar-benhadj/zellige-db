//! Deterministic property testing: random operation sequences against
//! `std::collections::BTreeMap` as the reference model, with a full
//! structural integrity check after every batch and an exact scan
//! comparison at the end.
//!
//! Randomness comes from a 5-line LCG instead of a RNG crate: the whole
//! suite is deterministic on every platform, and every seed printed in a
//! failure message reproduces the bug exactly.
//!
//! The "small universe" case is the bug hunter: 60 keys over thousands of
//! inserts and deletes forces constant splits, borrows, and merges.

use std::collections::BTreeMap;

use zdb_core::{BTree, Pager};

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

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

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

fn run_case(seed: u64, ops: usize, key_space: usize) {
    let dir = TempDir::new("prop");
    let mut pager = Pager::create(dir.path().join("prop.zdb")).expect("create pager");
    let mut tree = BTree::empty();
    let mut model: BTreeMap<Vec<u8>, Vec<u8>> = BTreeMap::new();
    let mut rng = Lcg(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1);

    for op in 0..ops {
        let key = format!("k{:06}", rng.below(key_space as u64));
        match rng.below(100) {
            0..=59 => {
                // Variable-length values exercise byte-weighted leaf splits.
                let val_len = 1 + rng.below(60) as usize;
                let val: Vec<u8> = (0..val_len).map(|i| b'a' + (i as u8 % 26)).collect();
                tree.insert(&mut pager, key.as_bytes(), &val)
                    .expect("insert");
                model.insert(key.into_bytes(), val);
            }
            60..=79 => {
                let deleted = tree.delete(&mut pager, key.as_bytes()).expect("delete");
                let expected = model.remove(key.as_bytes()).is_some();
                assert_eq!(
                    deleted, expected,
                    "seed {seed} op {op}: delete of {key} disagreed with model"
                );
            }
            _ => {
                let got = tree.get(&mut pager, key.as_bytes()).expect("get");
                let expected = model.get(key.as_bytes()).cloned();
                assert_eq!(
                    got, expected,
                    "seed {seed} op {op}: get of {key} disagreed with model"
                );
            }
        }
        if op % 500 == 0 {
            tree.check_integrity(&mut pager)
                .unwrap_or_else(|e| panic!("seed {seed} op {op}: integrity: {e}"));
        }
    }

    tree.check_integrity(&mut pager)
        .unwrap_or_else(|e| panic!("seed {seed} final integrity: {e}"));
    let scanned: Vec<(Vec<u8>, Vec<u8>)> = tree.scan(&mut pager).expect("scan").collect();
    let modeled: Vec<(Vec<u8>, Vec<u8>)> = model.into_iter().collect();
    assert_eq!(scanned, modeled, "seed {seed}: scan disagrees with model");
}

#[test]
fn property_small_universe_forces_splits_and_merges() {
    for seed in 1..=8u64 {
        run_case(seed * 7919, 4_000, 60);
    }
}

#[test]
fn property_medium_universe_builds_multi_level_trees() {
    for seed in 1..=6u64 {
        run_case(seed * 104_729, 4_000, 800);
    }
}

#[test]
fn property_large_universe_grows_deep_trees() {
    for seed in 1..=4u64 {
        run_case(seed * 15_485_863, 6_000, 20_000);
    }
}
