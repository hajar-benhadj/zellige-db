//! End-to-end persistence: pages written through one `Pager` are read back
//! through a *fresh* one — the closest a unit test gets to a process restart.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use zdb_core::{DbError, PAGE_SIZE, PageId, PageType, Pager};

/// Integration tests cannot see the crate's `#[cfg(test)]` helpers, so they
/// carry their own minimal self-cleaning temp dir (see `src/testing.rs`).
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
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

#[test]
fn pages_survive_reopen() -> Result<(), DbError> {
    let dir = TempDir::new("persist");
    let path = dir.path().join("persist.zdb");

    const N: PageId = 50;
    {
        let mut pager = Pager::create(&path)?;
        for id in 1..N {
            let mut page = pager.alloc_page(PageType::Data)?;
            for (i, b) in page.payload_mut().iter_mut().enumerate() {
                *b = ((id as usize + i) % 251) as u8;
            }
            pager.write_page(&mut page)?;
        }
        pager.sync()?;
    } // Pager dropped: file closed, like a process exiting

    let mut pager = Pager::open(&path)?;
    assert_eq!(pager.page_count(), N);
    for id in 1..N {
        let page = pager.read_page(id)?;
        assert_eq!(page.page_id(), id);
        for (i, &b) in page.payload().iter().enumerate() {
            assert_eq!(b, ((id as usize + i) % 251) as u8, "page {id} byte {i}");
        }
    }
    Ok(())
}

#[test]
fn fresh_file_is_exactly_one_meta_page() -> Result<(), DbError> {
    let dir = TempDir::new("empty");
    let path = dir.path().join("empty.zdb");

    let mut pager = Pager::create(&path)?;
    pager.sync()?;
    assert_eq!(pager.page_count(), 1);
    assert_eq!(pager.free_count(), 0);
    assert_eq!(std::fs::metadata(&path)?.len(), PAGE_SIZE as u64);
    Ok(())
}
