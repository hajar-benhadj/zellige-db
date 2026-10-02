//! The WAL-logging engine: transactions, checkpoints, crash recovery.
//!
//! `Database` is the [`PageIo`] implementation that upper layers (B+Tree
//! and everything on it) run on. Its rules:
//!
//! 1. **WAL-first.** A page write is only journaled — the data file is not
//!    touched until a checkpoint. Uncommitted work therefore *cannot*
//!    corrupt the file, no matter when the process dies.
//! 2. **Durability at fsync.** `commit()` appends a commit frame and fsyncs
//!    the log; when it returns, the transaction survives power loss.
//! 3. **Checkpoint** copies journaled pages into the data file, fsyncs it,
//!    and empties the log. It runs automatically on clean `Drop` (and when
//!    the journal grows past [`CHECKPOINT_THRESHOLD`] pages) so a long
//!    session never accumulates an unbounded log.
//! 4. **Recovery** (in `open`) replays committed frames in LSN order over
//!    the data file, then discards the log. A torn tail — the crash
//!    artifact — is detected by frame CRC and thrown away whole.

use std::path::{Path, PathBuf};

use crate::btree::BTree;
use crate::error::DbError;
use crate::io::PageIo;
use crate::page::{NIL_PAGE, PAGE_SIZE, Page, PageId, PageType};
use crate::pager::{MetaSnapshot, Pager};
use crate::wal::{COMMIT_PAGE_ID, Frame, WalWriter};

/// Journal pages buffered before an automatic checkpoint. Purely a memory
/// bound: correctness never depends on when a checkpoint runs.
const CHECKPOINT_THRESHOLD: usize = 256;

/// A transactional database file: `foo.zdb` + its journal `foo.zdb.wal`.
pub struct Database {
    pager: Pager,
    wal: WalWriter,
    /// Journaled page images not yet copied into the data file. Entries
    /// before `txn_pending_start` belong to committed transactions.
    pending: Vec<Page>,
    in_txn: bool,
    txn_wal_start: u64,
    txn_pending_start: usize,
    txn_meta_snapshot: Option<MetaSnapshot>,
    path: PathBuf,
}

fn wal_path(db_path: &Path) -> PathBuf {
    let mut s = db_path.as_os_str().to_os_string();
    s.push(".wal");
    PathBuf::from(s)
}

impl Database {
    /// Create a fresh database plus its empty journal.
    pub fn create(path: impl AsRef<Path>) -> Result<Self, DbError> {
        let path = path.as_ref().to_path_buf();
        let pager = Pager::create(&path)?;
        let wal = WalWriter::create(wal_path(&path))?;
        Ok(Database {
            pager,
            wal,
            pending: Vec::new(),
            in_txn: false,
            txn_wal_start: 0,
            txn_pending_start: 0,
            txn_meta_snapshot: None,
            path,
        })
    }

    /// Open a database, replaying its journal if a crash left records in it.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DbError> {
        let path = path.as_ref().to_path_buf();
        // Lenient on purpose: a torn meta page is exactly the case recovery
        // exists to repair. Validation happens after the replay below.
        let mut pager = Pager::open_lenient(&path)?;
        let wal_file = wal_path(&path);

        let (mut wal, frames) = if wal_file.exists() {
            WalWriter::open(&wal_file)?
        } else {
            (WalWriter::create(&wal_file)?, Vec::new())
        };

        recover(&mut pager, &frames)?;
        // Recovery ends with an empty, consistent log: reset the writer's
        // LSN counter to match the file we just truncated.
        wal.reset()?;
        Ok(Database {
            pager,
            wal,
            pending: Vec::new(),
            in_txn: false,
            txn_wal_start: 0,
            txn_pending_start: 0,
            txn_meta_snapshot: None,
            path,
        })
    }

    /// Load a named tree from the catalog. Returns an empty tree when the
    /// name is unknown; pair with [`Database::save_tree`] to persist it.
    pub fn open_tree(&mut self, name: &str) -> Result<BTree, DbError> {
        match self.catalog() {
            Some(catalog) => {
                let root = catalog.get(self, name.as_bytes())?;
                Ok(match root {
                    Some(bytes) => BTree {
                        root: Some(PageId::from_le_bytes(bytes[..4].try_into().unwrap())),
                    },
                    None => BTree::empty(),
                })
            }
            None => Ok(BTree::empty()),
        }
    }

    /// Persist a tree's root under `name` in the catalog tree. The catalog
    /// itself is a B+Tree whose root lives in the meta page — journaled
    /// like any other page, so catalog updates are crash-safe too.
    pub fn save_tree(&mut self, name: &str, tree: &BTree) -> Result<(), DbError> {
        let catalog_root = self.pager.catalog_root();
        let mut catalog = if catalog_root == NIL_PAGE {
            BTree::empty()
        } else {
            BTree {
                root: Some(catalog_root),
            }
        };
        let root_before = catalog.root;

        match tree.root {
            Some(root) => catalog.insert(self, name.as_bytes(), &root.to_le_bytes())?,
            None => {
                catalog.delete(self, name.as_bytes())?;
            }
        }

        if catalog.root != root_before {
            self.pager
                .set_catalog_root(catalog.root.unwrap_or(NIL_PAGE));
            let mut meta = self.pager.meta_image();
            self.write_page(&mut meta)?;
        }
        Ok(())
    }

    fn catalog(&self) -> Option<BTree> {
        match self.pager.catalog_root() {
            NIL_PAGE => None,
            root => Some(BTree { root: Some(root) }),
        }
    }

    /// Begin a transaction. Nested transactions are not supported — the
    /// engine is single-writer per file in this release.
    pub fn begin(&mut self) -> Result<(), DbError> {
        if self.in_txn {
            return Err(DbError::Corrupt("nested transaction"));
        }
        self.in_txn = true;
        self.txn_wal_start = self.wal.bytes_written();
        self.txn_pending_start = self.pending.len();
        self.txn_meta_snapshot = Some(self.pager.meta_snapshot());
        Ok(())
    }

    /// Commit: journal a commit record and fsync. When this returns, the
    /// transaction is durable whether or not a checkpoint ever happens.
    pub fn commit(&mut self) -> Result<(), DbError> {
        if !self.in_txn {
            return Err(DbError::Corrupt("commit outside a transaction"));
        }
        self.wal.append_commit()?;
        self.wal.flush_and_sync()?;
        self.in_txn = false;
        self.txn_meta_snapshot = None;
        if self.pending.len() >= CHECKPOINT_THRESHOLD {
            self.checkpoint()?;
        }
        Ok(())
    }

    /// Rollback: discard this transaction's journal tail, buffered pages,
    /// and meta bookkeeping. Committed-but-uncheckpointed state is kept.
    pub fn rollback(&mut self) -> Result<(), DbError> {
        if !self.in_txn {
            return Err(DbError::Corrupt("rollback outside a transaction"));
        }
        self.wal.truncate_to(self.txn_wal_start)?;
        self.pending.truncate(self.txn_pending_start);
        if let Some(snapshot) = self.txn_meta_snapshot.take() {
            self.pager.restore_meta(snapshot);
        }
        self.in_txn = false;
        Ok(())
    }

    /// Copy every journaled page into the data file, fsync it, empty the
    /// journal. Purely an optimization + log-size control.
    pub fn checkpoint(&mut self) -> Result<(), DbError> {
        if self.in_txn {
            return Err(DbError::Corrupt("checkpoint inside a transaction"));
        }
        for page in self.pending.drain(..) {
            let mut page = page;
            self.pager.write_page(&mut page)?;
        }
        self.pager.sync()?;
        self.wal.reset()?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// True while a transaction is open (used by tests and the REPL).
    pub fn in_txn(&self) -> bool {
        self.in_txn
    }
}

/// Replay committed frames over the data file, oldest first. The meta page
/// images inside the log carry the post-allocation `page_count`, so data
/// pages beyond the old file end are writable by the time their frame
/// arrives.
fn recover(pager: &mut Pager, frames: &[Frame]) -> Result<(), DbError> {
    let last_commit = frames
        .iter()
        .filter(|f| f.page_id == COMMIT_PAGE_ID)
        .map(|f| f.lsn)
        .max()
        .unwrap_or(0);

    for frame in frames.iter().filter(|f| f.lsn <= last_commit) {
        let Some(image) = frame.image else { continue };
        let mut page = Page::decode(image)?;
        pager.write_page_raw(&mut page)?;
    }
    if last_commit > 0 {
        pager.sync()?;
    }
    // Always re-derive in-memory meta from page 0: this both recovers the
    // recovered bookkeeping and refuses to open a non-database file.
    pager.reload_meta()?;
    Ok(())
}

impl PageIo for Database {
    /// Read-through: the freshest image of a page is the buffered one.
    fn read_page(&mut self, page_id: PageId) -> Result<Page, DbError> {
        if let Some(page) = self.pending.iter().rev().find(|p| p.page_id() == page_id) {
            return Ok(page.clone());
        }
        self.pager.read_page(page_id)
    }

    /// WAL-first write: journal the sealed image, buffer it, touch nothing
    /// else. The data file sees this page at the next checkpoint.
    fn write_page(&mut self, page: &mut Page) -> Result<(), DbError> {
        let lsn = self.wal.next_lsn();
        page.set_lsn(lsn as u32);
        page.seal();
        self.wal.append_image(lsn, page.page_id(), page.bytes())?;
        self.pending.push(page.clone());
        Ok(())
    }

    /// Allocate a page slot and journal the resulting meta image so the
    /// allocation itself is crash-safe. The meta image is logged *before*
    /// any data page that will use the new slot, which is what makes
    /// replay-ordering work (see `recover`).
    fn alloc_page(&mut self, page_type: PageType) -> Result<Page, DbError> {
        let page = self.pager.alloc_slot(page_type)?;
        let mut meta = self.pager.meta_image();
        self.write_page(&mut meta)?;
        Ok(page)
    }

    /// Free a page and journal both the `Free` marking and the meta image.
    fn free_page(&mut self, page_id: PageId) -> Result<(), DbError> {
        if page_id == 0 {
            return Err(DbError::PageOutOfBounds(page_id));
        }
        if let Some(page) = self.pending.iter().rev().find(|p| p.page_id() == page_id)
            && page.page_type()? == PageType::Free
        {
            return Err(DbError::Corrupt("double free of a page"));
        }
        let mut free_image = self.pager.free_slot(page_id)?;
        self.write_page(&mut free_image)?;
        let mut meta = self.pager.meta_image();
        self.write_page(&mut meta)?;
        Ok(())
    }

    fn sync(&mut self) -> Result<(), DbError> {
        self.pager.sync()
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        // A clean close must not silently commit an open transaction:
        // discard its buffered state, then checkpoint what *is* committed.
        if self.in_txn {
            self.pending.truncate(self.txn_pending_start);
            if let Some(snapshot) = self.txn_meta_snapshot.take() {
                self.pager.restore_meta(snapshot);
            }
            self.in_txn = false;
        }
        let _ = self.checkpoint();
    }
}

const _: () = {
    // Compile-time sanity: the commit sentinel must not collide with a
    // real page id stored in a frame image length position.
    assert!(COMMIT_PAGE_ID == u32::MAX && PAGE_SIZE == 4096);
};
