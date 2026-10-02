//! The pager: turns one file into an array of verified 4 KiB pages.
//!
//! The pager is the **only** component that touches the database file. Its
//! API is deliberately single-owner (`&mut self` everywhere): real
//! concurrency arrives in the server phase, behind one well-tested
//! serialization point.
//!
//! Durability contract (pre-WAL, see ARCHITECTURE.md): a write reaches
//! stable storage only at [`Pager::sync`]. Phase 3's WAL replaces this with
//! commit-time fsync semantics.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::error::DbError;
use crate::page::{NIL_PAGE, PAGE_SIZE, Page, PageId, PageType};

const MAGIC: [u8; 4] = *b"ZDB1";
const META_PAGE_ID: PageId = 0;

/// Bookkeeping persisted in page 0 so the file is fully self-describing:
///
/// ```text
/// payload offset  size  field
/// ──────────────  ────  ─────────────────────────
/// 0               4     magic b"ZDB1"
/// 4               4     page size (sanity check)
/// 8               4     page count (file length in pages)
/// 12              4     free list head (NIL_PAGE if empty)
/// 16              4     free page count
/// ```
#[derive(Debug, Clone, Copy)]
struct MetaPage {
    page_count: u32,
    free_head: PageId,
    free_count: u32,
}

impl MetaPage {
    fn encode(self, page: &mut Page) {
        debug_assert_eq!(page.page_id(), META_PAGE_ID);
        let p = page.payload_mut();
        p[0..4].copy_from_slice(&MAGIC);
        p[4..8].copy_from_slice(&(PAGE_SIZE as u32).to_le_bytes());
        p[8..12].copy_from_slice(&self.page_count.to_le_bytes());
        p[12..16].copy_from_slice(&self.free_head.to_le_bytes());
        p[16..20].copy_from_slice(&self.free_count.to_le_bytes());
    }

    fn decode(page: &Page) -> Result<Self, DbError> {
        let p = page.payload();
        if p[0..4] != MAGIC {
            return Err(DbError::BadMagic);
        }
        let u32_at = |off: usize| -> u32 {
            let mut b = [0u8; 4];
            b.copy_from_slice(&p[off..off + 4]);
            u32::from_le_bytes(b)
        };
        if u32_at(4) as usize != PAGE_SIZE {
            return Err(DbError::Corrupt("meta page records a different page size"));
        }
        let page_count = u32_at(8);
        if page_count == 0 {
            return Err(DbError::Corrupt("meta page records zero pages"));
        }
        Ok(MetaPage {
            page_count,
            free_head: u32_at(12),
            free_count: u32_at(16),
        })
    }
}

/// Owns the database file and hands out checksum-verified pages.
#[derive(Debug)]
pub struct Pager {
    file: File,
    path: PathBuf,
    meta: MetaPage,
}

impl Pager {
    /// Create a fresh database file. Fails if the path already exists —
    /// overwriting a database by accident is not a recoverable mistake.
    pub fn create(path: impl AsRef<Path>) -> Result<Self, DbError> {
        let path = path.as_ref();
        if path.exists() {
            return Err(DbError::AlreadyExists(path.display().to_string()));
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;
        let mut pager = Pager {
            file,
            path: path.to_path_buf(),
            meta: MetaPage {
                page_count: 1,
                free_head: NIL_PAGE,
                free_count: 0,
            },
        };
        pager.persist_meta()?;
        pager.sync()?;
        Ok(pager)
    }

    /// Open an existing database, verifying its meta page.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DbError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path.as_ref())?;
        let mut pager = Pager {
            file,
            path: path.as_ref().to_path_buf(),
            meta: MetaPage {
                page_count: 1,
                free_head: NIL_PAGE,
                free_count: 0,
            },
        };
        let meta_page = pager.read_page(META_PAGE_ID)?;
        if meta_page.page_type()? != PageType::Meta {
            return Err(DbError::Corrupt("page 0 is not a meta page"));
        }
        pager.meta = MetaPage::decode(&meta_page)?;
        Ok(pager)
    }

    fn offset(page_id: PageId) -> u64 {
        page_id as u64 * PAGE_SIZE as u64
    }

    /// Read a page and verify its checksum against what was written.
    pub fn read_page(&mut self, page_id: PageId) -> Result<Page, DbError> {
        if page_id >= self.meta.page_count {
            return Err(DbError::PageOutOfBounds(page_id));
        }
        let mut bytes = [0u8; PAGE_SIZE];
        self.file.seek(SeekFrom::Start(Self::offset(page_id)))?;
        self.file.read_exact(&mut bytes)?;
        Page::decode(bytes)
    }

    /// Seal (checksum) a page and write it to disk.
    ///
    /// The write bypasses the OS dirty-page decision point only at the next
    /// [`Pager::sync`] — until the WAL exists, that is the durability line.
    pub fn write_page(&mut self, page: &mut Page) -> Result<(), DbError> {
        let page_id = page.page_id();
        if page_id >= self.meta.page_count {
            return Err(DbError::PageOutOfBounds(page_id));
        }
        page.seal();
        self.file.seek(SeekFrom::Start(Self::offset(page_id)))?;
        self.file.write_all(page.bytes())?;
        Ok(())
    }

    /// Allocate a fresh zeroed page: recycled from the free list when
    /// possible, appended to the file otherwise.
    ///
    /// The returned page exists only in memory until [`Pager::write_page`]
    /// is called with it — allocating without writing leaves a hole that
    /// reads back as an I/O error, so upper layers always write immediately.
    pub fn alloc_page(&mut self, page_type: PageType) -> Result<Page, DbError> {
        debug_assert_ne!(page_type, PageType::Meta, "page 0 is reserved for meta");
        let page_id = if self.meta.free_head != NIL_PAGE {
            self.pop_free_head()?
        } else {
            let page_id = self.meta.page_count;
            self.meta.page_count += 1;
            page_id
        };
        self.persist_meta()?;
        Ok(Page::zeroed(page_id, page_type))
    }

    /// Return a page to the free list. Its contents become garbage; freeing
    /// a page twice is a corruption signal and is rejected.
    pub fn free_page(&mut self, page_id: PageId) -> Result<(), DbError> {
        if page_id == META_PAGE_ID || page_id >= self.meta.page_count {
            return Err(DbError::PageOutOfBounds(page_id));
        }
        if self.read_page(page_id)?.page_type()? == PageType::Free {
            return Err(DbError::Corrupt("double free of a page"));
        }
        let mut page = Page::zeroed(page_id, PageType::Free);
        page.set_next_free(self.meta.free_head);
        self.meta.free_head = page_id;
        self.meta.free_count += 1;
        self.write_page(&mut page)?;
        self.persist_meta()?;
        Ok(())
    }

    /// fsync everything. Before this returns, nothing is guaranteed to have
    /// reached the platter (or the SSD's FTL).
    pub fn sync(&mut self) -> Result<(), DbError> {
        self.file.sync_all()?;
        Ok(())
    }

    pub fn page_count(&self) -> u32 {
        self.meta.page_count
    }

    pub fn free_count(&self) -> u32 {
        self.meta.free_count
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Pop the head of the free list (LIFO reuse keeps freed pages warm).
    fn pop_free_head(&mut self) -> Result<PageId, DbError> {
        if self.meta.free_count == 0 {
            return Err(DbError::Corrupt("free list head set but count is zero"));
        }
        let page_id = self.meta.free_head;
        let recycled = self.read_page(page_id)?;
        if recycled.page_type()? != PageType::Free {
            return Err(DbError::Corrupt("free-list page is not marked Free"));
        }
        self.meta.free_head = recycled.next_free();
        self.meta.free_count -= 1;
        Ok(page_id)
    }

    fn persist_meta(&mut self) -> Result<(), DbError> {
        let mut page = Page::zeroed(META_PAGE_ID, PageType::Meta);
        self.meta.encode(&mut page);
        self.write_page(&mut page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;

    #[test]
    fn create_then_reopen_roundtrip() -> Result<(), DbError> {
        let dir = TempDir::new("pager");
        let path = dir.path().join("t.zdb");

        {
            let mut pager = Pager::create(&path)?;
            assert_eq!(pager.page_count(), 1);
            let mut page = pager.alloc_page(PageType::Data)?;
            page.payload_mut()[0] = 0x5A;
            pager.write_page(&mut page)?;
            pager.sync()?;
        }

        let mut pager = Pager::open(&path)?;
        assert_eq!(pager.page_count(), 2);
        let page = pager.read_page(1)?;
        assert_eq!(page.payload()[0], 0x5A);
        Ok(())
    }

    #[test]
    fn create_refuses_to_overwrite() -> Result<(), DbError> {
        let dir = TempDir::new("pager");
        let path = dir.path().join("t.zdb");
        Pager::create(&path)?;
        let err = Pager::create(&path).expect_err("must refuse existing file");
        assert!(matches!(err, DbError::AlreadyExists(_)));
        Ok(())
    }

    #[test]
    fn open_rejects_a_file_that_is_not_zelligedb() -> Result<(), DbError> {
        let dir = TempDir::new("pager");
        let path = dir.path().join("garbage.zdb");
        std::fs::write(&path, [0u8; PAGE_SIZE])?;
        assert!(Pager::open(&path).is_err());
        Ok(())
    }

    #[test]
    fn read_rejects_out_of_bounds_pages() -> Result<(), DbError> {
        let dir = TempDir::new("pager");
        let path = dir.path().join("t.zdb");
        let mut pager = Pager::create(&path)?;
        let err = pager.read_page(7).expect_err("page 7 does not exist");
        assert!(matches!(err, DbError::PageOutOfBounds(7)));
        Ok(())
    }

    #[test]
    fn write_rejects_pages_beyond_the_end() -> Result<(), DbError> {
        let dir = TempDir::new("pager");
        let path = dir.path().join("t.zdb");
        let mut pager = Pager::create(&path)?;
        let mut forged = Page::zeroed(9, PageType::Data);
        let err = pager
            .write_page(&mut forged)
            .expect_err("page 9 does not exist");
        assert!(matches!(err, DbError::PageOutOfBounds(9)));
        Ok(())
    }

    #[test]
    fn free_list_recycles_pages_lifo() -> Result<(), DbError> {
        let dir = TempDir::new("pager");
        let path = dir.path().join("t.zdb");
        let mut pager = Pager::create(&path)?;

        for _ in 0..3 {
            let mut page = pager.alloc_page(PageType::Data)?;
            pager.write_page(&mut page)?;
        }
        assert_eq!(pager.page_count(), 4);

        pager.free_page(2)?;
        pager.free_page(1)?;
        assert_eq!(pager.free_count(), 2);

        let mut a = pager.alloc_page(PageType::Data)?;
        pager.write_page(&mut a)?;
        assert_eq!(a.page_id(), 1, "most recently freed page is reused first");

        let mut b = pager.alloc_page(PageType::Data)?;
        pager.write_page(&mut b)?;
        assert_eq!(b.page_id(), 2);

        let mut c = pager.alloc_page(PageType::Data)?;
        pager.write_page(&mut c)?;
        assert_eq!(c.page_id(), 4, "list empty again: file grows");
        assert_eq!(pager.page_count(), 5);
        assert_eq!(pager.free_count(), 0);
        Ok(())
    }

    #[test]
    fn double_free_is_rejected() -> Result<(), DbError> {
        let dir = TempDir::new("pager");
        let path = dir.path().join("t.zdb");
        let mut pager = Pager::create(&path)?;
        let mut page = pager.alloc_page(PageType::Data)?;
        pager.write_page(&mut page)?;

        pager.free_page(1)?;
        let err = pager.free_page(1).expect_err("second free must fail");
        assert!(matches!(err, DbError::Corrupt("double free of a page")));
        assert_eq!(pager.free_count(), 1, "list unchanged after rejection");
        Ok(())
    }

    #[test]
    fn freeing_the_meta_page_is_rejected() -> Result<(), DbError> {
        let dir = TempDir::new("pager");
        let path = dir.path().join("t.zdb");
        let mut pager = Pager::create(&path)?;
        let err = pager.free_page(0).expect_err("meta page is not freeable");
        assert!(matches!(err, DbError::PageOutOfBounds(0)));
        Ok(())
    }

    #[test]
    fn detects_a_byte_flipped_between_sessions() -> Result<(), DbError> {
        let dir = TempDir::new("pager");
        let path = dir.path().join("t.zdb");

        {
            let mut pager = Pager::create(&path)?;
            let mut page = pager.alloc_page(PageType::Data)?;
            page.payload_mut()[100] = 0x42;
            pager.write_page(&mut page)?;
            pager.sync()?;
        }

        // Corrupt page 1 directly on disk, bypassing the pager — this is
        // what a torn write or bad sector looks like.
        let mut file = OpenOptions::new().write(true).open(&path)?;
        file.seek(SeekFrom::Start(PAGE_SIZE as u64 + 100))?;
        file.write_all(&[0xFF])?;
        file.sync_all()?;

        let mut pager = Pager::open(&path)?;
        let err = pager.read_page(1).expect_err("corruption must surface");
        assert!(matches!(err, DbError::ChecksumMismatch { page_id: 1, .. }));
        Ok(())
    }

    #[test]
    fn meta_bookkeeping_survives_reopen() -> Result<(), DbError> {
        let dir = TempDir::new("pager");
        let path = dir.path().join("t.zdb");

        {
            let mut pager = Pager::create(&path)?;
            for _ in 0..3 {
                let mut page = pager.alloc_page(PageType::Data)?;
                pager.write_page(&mut page)?;
            }
            pager.free_page(1)?;
            pager.sync()?;
        }

        let mut pager = Pager::open(&path)?;
        assert_eq!(pager.page_count(), 4);
        assert_eq!(pager.free_count(), 1);

        // The freed page is still recyclable after a restart.
        let mut page = pager.alloc_page(PageType::Data)?;
        pager.write_page(&mut page)?;
        assert_eq!(page.page_id(), 1);
        Ok(())
    }
}
