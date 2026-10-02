//! Write-ahead log: full-page redo records, checksummed frames.
//!
//! Every page mutation becomes a frame appended here *before* the page may
//! reach the data file (WAL-first). A transaction is durable the moment its
//! COMMIT frame is fsynced; everything after the last commit is discarded
//! during recovery, and every committed frame is replayed in LSN order.
//!
//! Frame layout (little-endian):
//!
//! ```text
//! offset  size  field
//! ──────  ────  ────────────────────────────────────────────
//! 0       8     lsn (sequential from 1 within this WAL file)
//! 8       4     page id (COMMIT_PAGE_ID marks a commit record)
//! 12      4     image length (0 for commit records)
//! 16      4     CRC32 over bytes 0..16 followed by the image
//! 20..    ...   page image (PAGE_SIZE bytes) when present
//! ```
//!
//! A torn tail — the classic crash artifact — fails the length or CRC
//! check and everything from that point on is discarded, so a partially
//! flushed frame can never be half-applied.

use std::io::Read;
use std::path::{Path, PathBuf};

use crate::error::DbError;
use crate::page::{PAGE_SIZE, PageId};
use crate::pagefile::{MemoryFile, OsFile, PageFile};

const FRAME_HEADER: usize = 20;
/// `page_id` sentinel marking a commit record.
pub const COMMIT_PAGE_ID: PageId = u32::MAX;

#[derive(Debug)]
pub(crate) struct Frame {
    pub lsn: u64,
    pub page_id: PageId,
    pub image: Option<[u8; PAGE_SIZE]>,
}

pub(crate) struct WalWriter {
    path: PathBuf,
    file: Box<dyn PageFile>,
    next_lsn: u64,
    bytes_written: u64,
}

impl WalWriter {
    pub fn create(path: impl AsRef<Path>) -> Result<Self, DbError> {
        let file: Box<dyn PageFile> = Box::new(OsFile::create(path.as_ref())?);
        Ok(WalWriter {
            path: path.as_ref().to_path_buf(),
            file,
            next_lsn: 1,
            bytes_written: 0,
        })
    }

    /// A journal over plain memory (WebAssembly backend).
    pub fn create_memory() -> Result<Self, DbError> {
        Ok(WalWriter {
            path: PathBuf::from(":memory:.wal"),
            file: Box::new(MemoryFile::new()),
            next_lsn: 1,
            bytes_written: 0,
        })
    }

    /// Open an existing WAL, returning the writer positioned after the last
    /// intact frame plus every frame found (commit records included). The
    /// caller decides which are committed; a torn or corrupt tail is simply
    /// not part of the result.
    pub fn open(path: impl AsRef<Path>) -> Result<(Self, Vec<Frame>), DbError> {
        let mut raw = Vec::new();
        std::fs::File::open(path.as_ref())?.read_to_end(&mut raw)?;
        let frames = parse_frames(&raw);

        let next_lsn = frames.last().map_or(1, |f| f.lsn + 1);
        let bytes_written = raw.len() as u64;
        let file: Box<dyn PageFile> = Box::new(OsFile::open(path.as_ref())?);
        Ok((
            WalWriter {
                path: path.as_ref().to_path_buf(),
                file,
                next_lsn,
                bytes_written,
            },
            frames,
        ))
    }

    pub fn next_lsn(&self) -> u64 {
        self.next_lsn
    }

    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    pub fn append_image(
        &mut self,
        lsn: u64,
        page_id: PageId,
        image: &[u8; PAGE_SIZE],
    ) -> Result<(), DbError> {
        debug_assert_eq!(lsn, self.next_lsn, "LSNs must be assigned sequentially");
        let header = frame_header(lsn, page_id, Some(image));
        let mut frame = header.to_vec();
        frame.extend_from_slice(image);
        let offset = self.bytes_written;
        self.file.write_all_at(&frame, offset)?;
        self.next_lsn += 1;
        self.bytes_written += FRAME_HEADER as u64 + PAGE_SIZE as u64;
        Ok(())
    }

    pub fn append_commit(&mut self) -> Result<(), DbError> {
        let lsn = self.next_lsn;
        let header = frame_header(lsn, COMMIT_PAGE_ID, None);
        let offset = self.bytes_written;
        self.file.write_all_at(&header, offset)?;
        self.next_lsn += 1;
        self.bytes_written += FRAME_HEADER as u64;
        Ok(())
    }

    /// The durability line: flush the buffer, fsync the file. When this
    /// returns, the transaction is committed as far as the world is
    /// concerned — power loss included.
    pub fn flush_and_sync(&mut self) -> Result<(), DbError> {
        self.file.sync()?;
        Ok(())
    }

    /// Rollback support: drop everything appended since `offset`.
    pub fn truncate_to(&mut self, offset: u64) -> Result<(), DbError> {
        self.file.set_len(offset)?;
        // LSNs of the discarded frames are never reused within this file
        // generation; the counter keeps climbing.
        self.bytes_written = offset;
        Ok(())
    }

    /// Checkpoint support: empty the log and restart LSN numbering.
    pub fn reset(&mut self) -> Result<(), DbError> {
        self.file.set_len(0)?;
        self.next_lsn = 1;
        self.bytes_written = 0;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn frame_header(lsn: u64, page_id: PageId, image: Option<&[u8; PAGE_SIZE]>) -> [u8; FRAME_HEADER] {
    let mut header = [0u8; FRAME_HEADER];
    header[0..8].copy_from_slice(&lsn.to_le_bytes());
    header[8..12].copy_from_slice(&page_id.to_le_bytes());
    let img_len = image.map_or(0, |_| PAGE_SIZE);
    header[12..16].copy_from_slice(&(img_len as u32).to_le_bytes());
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&header[0..16]);
    if let Some(img) = image {
        hasher.update(img);
    }
    let crc = hasher.finalize();
    header[16..20].copy_from_slice(&crc.to_le_bytes());
    header
}

/// Parse every intact, sequentially-numbered frame from the front of a WAL
/// image; stop at the first torn or corrupt frame.
fn parse_frames(raw: &[u8]) -> Vec<Frame> {
    let mut frames = Vec::new();
    let mut off = 0usize;
    let mut expected_lsn = 1u64;
    while off + FRAME_HEADER <= raw.len() {
        let lsn = u64::from_le_bytes(raw[off..off + 8].try_into().unwrap());
        let page_id = u32::from_le_bytes(raw[off + 8..off + 12].try_into().unwrap());
        let img_len = u32::from_le_bytes(raw[off + 12..off + 16].try_into().unwrap()) as usize;
        let stored_crc = u32::from_le_bytes(raw[off + 16..off + 20].try_into().unwrap());
        let frame_len = FRAME_HEADER + img_len;
        if off + frame_len > raw.len() || lsn != expected_lsn {
            break; // torn tail or a hole in the sequence
        }
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&raw[off..off + 16]);
        hasher.update(&raw[off + FRAME_HEADER..off + frame_len]);
        if hasher.finalize() != stored_crc {
            break;
        }
        let image = if img_len == 0 {
            if page_id != COMMIT_PAGE_ID {
                break; // zero-length data frame is meaningless
            }
            None
        } else if img_len == PAGE_SIZE {
            let mut img = [0u8; PAGE_SIZE];
            img.copy_from_slice(&raw[off + FRAME_HEADER..off + frame_len]);
            Some(img)
        } else {
            break;
        };
        frames.push(Frame {
            lsn,
            page_id,
            image,
        });
        expected_lsn += 1;
        off += frame_len;
    }
    frames
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_bytes(lsn: u64, page_id: PageId, img: Option<u8>) -> Vec<u8> {
        let mut out = Vec::new();
        match img {
            Some(fill) => {
                let image = [fill; PAGE_SIZE];
                let h = frame_header(lsn, page_id, Some(&image));
                out.extend_from_slice(&h);
                out.extend_from_slice(&image);
            }
            None => out.extend_from_slice(&frame_header(lsn, COMMIT_PAGE_ID, None)),
        }
        out
    }

    #[test]
    fn parse_accepts_intact_frames_and_stops_at_torn_tail() {
        let mut raw = Vec::new();
        raw.extend(frame_bytes(1, 7, Some(0xAA)));
        raw.extend(frame_bytes(2, 8, Some(0xBB)));
        raw.extend(frame_bytes(3, u32::MAX, None));
        assert_eq!(parse_frames(&raw).len(), 3);

        // Cut into the middle of the commit frame: it vanishes, earlier
        // frames survive.
        for cut in [raw.len() - 1, raw.len() - 10, raw.len() - 20 + 1] {
            let parsed = parse_frames(&raw[..cut]);
            assert_eq!(parsed.len(), 2, "cut at {cut}");
            assert_eq!(parsed[0].page_id, 7);
            assert_eq!(parsed[1].page_id, 8);
        }

        // Cut into an image frame: that frame and everything after vanish.
        let parsed = parse_frames(&raw[..FRAME_HEADER + PAGE_SIZE - 1]);
        assert_eq!(parsed.len(), 0);
    }

    #[test]
    fn parse_rejects_a_corrupted_middle_frame() {
        let mut raw = Vec::new();
        raw.extend(frame_bytes(1, 7, Some(0xAA)));
        raw.extend(frame_bytes(2, 8, Some(0xBB)));
        raw[FRAME_HEADER + PAGE_SIZE + FRAME_HEADER + 100] ^= 0xFF; // inside frame 2's image
        let parsed = parse_frames(&raw);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].page_id, 7);
    }

    #[test]
    fn parse_rejects_sequence_holes() {
        let mut raw = Vec::new();
        raw.extend(frame_bytes(1, 7, Some(0xAA)));
        raw.extend(frame_bytes(3, 8, Some(0xBB))); // LSN gap
        let parsed = parse_frames(&raw);
        assert_eq!(parsed.len(), 1);
    }
}
