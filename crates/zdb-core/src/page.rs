//! On-disk page format — the "tiles" of ZelligeDB.
//!
//! Every page is exactly [`PAGE_SIZE`] bytes, laid out little-endian:
//!
//! ```text
//! offset    size  field
//! ────────  ────  ─────────────────────────────────────────
//! 0         4     page type (0 = free, 1 = meta, 2 = data)
//! 4         4     page id (self-describing redundancy)
//! 8         4     next free page id (meaningful when type = free)
//! 12        4     reserved — future WAL LSN slot
//! 16        4076  payload (owned by the layer above)
//! 4092      4     CRC32 of bytes 0..4092
//! ```
//!
//! The trailing checksum covers header + payload, so a torn write or a
//! flipped bit anywhere in the page is caught by [`Page::decode`] instead of
//! surfacing as silently wrong data in the layers above. See ADR-0002.

use crate::error::DbError;

/// Fixed on-disk page size. See ADR-0002 for why 4 KiB.
pub const PAGE_SIZE: usize = 4096;

/// Identifier of a page: its index in the file. Page 0 is always the meta page.
pub type PageId = u32;

/// Sentinel meaning "no page": free-list end, absent pointer.
pub const NIL_PAGE: PageId = u32::MAX;

const HEADER_LEN: usize = 16;
const CHECKSUM_LEN: usize = 4;
const CHECKSUM_OFF: usize = PAGE_SIZE - CHECKSUM_LEN;

/// Payload bytes owned by whichever layer allocates the page.
pub const PAYLOAD_LEN: usize = PAGE_SIZE - HEADER_LEN - CHECKSUM_LEN;

/// What a page is used for. Upper layers (B+Tree, heap) will grow this enum;
/// unknown values on disk are a corruption signal, never a guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum PageType {
    Free = 0,
    Meta = 1,
    Data = 2,
}

impl PageType {
    fn from_u32(v: u32) -> Result<Self, DbError> {
        match v {
            0 => Ok(PageType::Free),
            1 => Ok(PageType::Meta),
            2 => Ok(PageType::Data),
            other => Err(DbError::InvalidPageType(other)),
        }
    }
}

/// A single 4 KiB page, mirrored in memory.
///
/// Mutation is deliberately manual: change what you need, then let the pager
/// [`seal`](Page::seal) the checksum when the page goes to disk. There is no
/// way to write an unsealed page through the public API.
#[derive(Debug, Clone)]
pub struct Page {
    bytes: [u8; PAGE_SIZE],
}

impl Page {
    /// A blank page with sane header defaults.
    pub fn zeroed(page_id: PageId, page_type: PageType) -> Self {
        let mut page = Page {
            bytes: [0; PAGE_SIZE],
        };
        page.set_page_type(page_type);
        page.set_page_id(page_id);
        page.set_next_free(NIL_PAGE);
        page
    }

    /// Decode raw bytes read from disk, verifying the checksum.
    pub fn decode(bytes: [u8; PAGE_SIZE]) -> Result<Self, DbError> {
        let mut stored_bytes = [0u8; CHECKSUM_LEN];
        stored_bytes.copy_from_slice(&bytes[CHECKSUM_OFF..]);
        let stored = u32::from_le_bytes(stored_bytes);
        let computed = crc32fast::hash(&bytes[..CHECKSUM_OFF]);
        if stored != computed {
            // The page id itself lives in the possibly-corrupt header, so it
            // is a best-effort hint, not gospel.
            let mut id_bytes = [0u8; 4];
            id_bytes.copy_from_slice(&bytes[4..8]);
            return Err(DbError::ChecksumMismatch {
                page_id: u32::from_le_bytes(id_bytes),
                stored,
                computed,
            });
        }
        let page = Page { bytes };
        page.page_type()?;
        Ok(page)
    }

    fn u32_at(&self, off: usize) -> u32 {
        let mut b = [0u8; 4];
        b.copy_from_slice(&self.bytes[off..off + 4]);
        u32::from_le_bytes(b)
    }

    fn set_u32_at(&mut self, off: usize, v: u32) {
        self.bytes[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }

    pub fn page_type(&self) -> Result<PageType, DbError> {
        PageType::from_u32(self.u32_at(0))
    }

    pub fn set_page_type(&mut self, page_type: PageType) {
        self.set_u32_at(0, page_type as u32);
    }

    pub fn page_id(&self) -> PageId {
        self.u32_at(4)
    }

    pub fn set_page_id(&mut self, page_id: PageId) {
        self.set_u32_at(4, page_id);
    }

    pub fn next_free(&self) -> PageId {
        self.u32_at(8)
    }

    pub fn set_next_free(&mut self, next: PageId) {
        self.set_u32_at(8, next);
    }

    pub fn payload(&self) -> &[u8] {
        &self.bytes[HEADER_LEN..CHECKSUM_OFF]
    }

    pub fn payload_mut(&mut self) -> &mut [u8] {
        &mut self.bytes[HEADER_LEN..CHECKSUM_OFF]
    }

    /// Raw page bytes, ready for the disk. Only meaningful after `seal`.
    pub fn bytes(&self) -> &[u8; PAGE_SIZE] {
        &self.bytes
    }

    /// Compute and store the checksum. The pager does this on every write,
    /// so callers never deal with checksums directly.
    pub fn seal(&mut self) {
        let sum = crc32fast::hash(&self.bytes[..CHECKSUM_OFF]);
        self.set_u32_at(CHECKSUM_OFF, sum);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_and_decode_roundtrip() {
        let mut page = Page::zeroed(7, PageType::Data);
        page.payload_mut()[0] = 0xAB;
        page.payload_mut()[PAYLOAD_LEN - 1] = 0xCD;
        page.seal();

        let decoded = Page::decode(page.bytes().to_owned()).expect("sealed page must decode");
        assert_eq!(decoded.page_id(), 7);
        assert_eq!(decoded.page_type().unwrap(), PageType::Data);
        assert_eq!(decoded.payload()[0], 0xAB);
        assert_eq!(decoded.payload()[PAYLOAD_LEN - 1], 0xCD);
        assert_eq!(decoded.next_free(), NIL_PAGE);
    }

    #[test]
    fn decode_detects_a_single_flipped_bit() {
        let mut page = Page::zeroed(3, PageType::Data);
        page.payload_mut()[100] = 0x42;
        page.seal();

        let mut bytes = *page.bytes();
        bytes[100] ^= 0x01; // flip one bit in the payload

        let err = Page::decode(bytes).expect_err("corrupted page must be rejected");
        assert!(matches!(err, DbError::ChecksumMismatch { page_id: 3, .. }));
    }

    #[test]
    fn decode_rejects_unknown_page_type() {
        // Forge a page with an unknown type byte, checksummed correctly,
        // so we know the type validation fires *after* the checksum passes.
        let mut bytes = [0u8; PAGE_SIZE];
        bytes[0..4].copy_from_slice(&99u32.to_le_bytes());
        bytes[4..8].copy_from_slice(&1u32.to_le_bytes());
        let sum = crc32fast::hash(&bytes[..PAGE_SIZE - CHECKSUM_LEN]);
        bytes[PAGE_SIZE - CHECKSUM_LEN..].copy_from_slice(&sum.to_le_bytes());

        let err = Page::decode(bytes).expect_err("unknown page type must be rejected");
        assert!(matches!(err, DbError::InvalidPageType(99)));
    }
}
