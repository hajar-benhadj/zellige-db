//! The page I/O surface upper layers program against.
//!
//! `BTree` (and everything built on it) takes `&mut dyn PageIo`, never a
//! concrete pager. Two implementations exist:
//!
//! - [`Pager`](crate::Pager) — raw, unlogged access (tools, tests)
//! - [`Database`](crate::Database) — the WAL-logging engine (phase 3):
//!   every page write is journaled before it can reach the data file, and
//!   recovery replays committed records after a crash.

use crate::error::DbError;
use crate::page::{Page, PageId, PageType};
use crate::pager::Pager;

/// Read/allocate/write pages of a database file.
pub trait PageIo {
    fn read_page(&mut self, page_id: PageId) -> Result<Page, DbError>;
    fn write_page(&mut self, page: &mut Page) -> Result<(), DbError>;
    fn alloc_page(&mut self, page_type: PageType) -> Result<Page, DbError>;
    fn free_page(&mut self, page_id: PageId) -> Result<(), DbError>;
    fn sync(&mut self) -> Result<(), DbError>;
}

impl PageIo for crate::pager::Pager {
    fn read_page(&mut self, page_id: PageId) -> Result<Page, DbError> {
        Pager::read_page(self, page_id)
    }

    fn write_page(&mut self, page: &mut Page) -> Result<(), DbError> {
        Pager::write_page(self, page)
    }

    fn alloc_page(&mut self, page_type: PageType) -> Result<Page, DbError> {
        Pager::alloc_page(self, page_type)
    }

    fn free_page(&mut self, page_id: PageId) -> Result<(), DbError> {
        Pager::free_page(self, page_id)
    }

    fn sync(&mut self) -> Result<(), DbError> {
        Pager::sync(self)
    }
}
