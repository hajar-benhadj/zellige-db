//! ZelligeDB storage core.
//!
//! This crate is built in layers, bottom to top, each landing with its own
//! ADR under `docs/adr/` and tests that prove it:
//!
//! - [`error`]    — the single error type used across the engine
//! - [`page`]     — the 4 KiB on-disk page format with CRC32 integrity
//! - [`pager`]    — raw page I/O on one file (tools and tests)
//! - [`io`]       — the `PageIo` trait upper layers program against
//! - [`btree`]    — the B+Tree ordered index
//! - [`wal`]      — checksummed write-ahead log frames
//! - [`database`] — transactions, checkpoints, and crash recovery
//!
//! Coming next: SQL front-end (phase 4), indexes + MVCC (phase 5).
//!
//! Nothing in this crate uses `unsafe`.

#![deny(unsafe_code)]

pub mod btree;
pub mod database;
pub mod error;
pub mod io;
pub mod page;
pub mod pager;
pub mod wal;

#[cfg(test)]
pub(crate) mod testing;

pub use btree::{BTree, BTreeScan};
pub use database::{Database, TxnStatus};
pub use error::DbError;
pub use io::PageIo;
pub use page::{NIL_PAGE, PAGE_SIZE, PAYLOAD_LEN, Page, PageId, PageType};
pub use pager::Pager;
