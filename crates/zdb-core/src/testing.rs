//! Test-only helpers kept behind `#[cfg(test)]`.
//!
//! Integration tests (under `tests/`) cannot see this module — they carry
//! their own copy of `TempDir`. The ~15 duplicated lines buy a public API
//! with zero test scaffolding leaking into it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// A unique directory under the OS temp dir, removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("zdb-{tag}-{}-{id}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        TempDir(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
