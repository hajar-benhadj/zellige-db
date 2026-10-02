//! ZelligeDB storage core.
//!
//! This crate is built in layers, bottom to top, each landing with its own
//! ADR under `docs/adr/` and tests that prove it:
//!
//! - `pager`  — fixed-size pages on a single file, CRC32-verified (phase 1)
//! - `btree`  — a B+Tree ordered index on top of the pager (phase 2)
//! - `wal`    — write-ahead log with redo recovery (phase 3)
//!
//! Nothing in this crate uses `unsafe`.

#![deny(unsafe_code)]
