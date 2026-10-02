//! Error types shared by every ZelligeDB storage layer.

use crate::page::PageId;
use thiserror::Error;

/// Every failure mode ZelligeDB core can report.
///
/// The rule across the engine: corruption is *detected and reported*, never
/// silently propagated. A `ChecksumMismatch` means the bytes on disk do not
/// match what was written — the caller's only safe move is to stop trusting
/// that page.
#[derive(Debug, Error)]
pub enum DbError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    #[error(
        "checksum mismatch on page {page_id} (stored {stored:#010x}, computed {computed:#010x})"
    )]
    ChecksumMismatch {
        page_id: PageId,
        stored: u32,
        computed: u32,
    },

    #[error("invalid page type {0}")]
    InvalidPageType(u32),

    #[error("corrupt file: {0}")]
    Corrupt(&'static str),

    #[error("page {0} out of bounds")]
    PageOutOfBounds(PageId),

    #[error("database file already exists: {0}")]
    AlreadyExists(String),

    #[error("not a ZelligeDB file: bad magic in meta page")]
    BadMagic,
}
