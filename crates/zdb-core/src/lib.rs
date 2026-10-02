//! ZelligeDB storage core.
//!
//! This crate is built in layers, bottom to top, each landing with its own
//! ADR under `docs/adr/` and tests that prove it:
//!
//! - [`error`] — the single error type used across the engine
//! - [`page`]  — the 4 KiB on-disk page format with CRC32 integrity
//! - [`pager`] — the only layer that touches the database file
//!
//! Coming next: `btree` (phase 2), `wal` (phase 3).
//!
//! Nothing in this crate uses `unsafe`.

#![deny(unsafe_code)]

pub mod error;
pub mod page;
pub mod pager;

#[cfg(test)]
pub(crate) mod testing;

pub use error::DbError;
pub use page::{NIL_PAGE, PAGE_SIZE, PAYLOAD_LEN, Page, PageId, PageType};
pub use pager::Pager;
