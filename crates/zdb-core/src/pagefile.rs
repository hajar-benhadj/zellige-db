//! The file abstraction under the pager: one implementation talks to the
//! operating system, the other is plain memory — which is what lets the
//! whole engine compile to WebAssembly and run inside a browser tab
//! (phase 8's playground), where there is no filesystem at all.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Sequential + positioned byte storage with durability control.
pub(crate) trait PageFile: Send {
    fn read_exact_at(&mut self, buf: &mut [u8], offset: u64) -> std::io::Result<()>;
    fn write_all_at(&mut self, buf: &[u8], offset: u64) -> std::io::Result<()>;
    /// Grow or shrink storage to `len` bytes.
    fn set_len(&mut self, len: u64) -> std::io::Result<()>;
    /// Flush to durable storage (fsync for files; a no-op for memory).
    fn sync(&mut self) -> std::io::Result<()>;
}

pub(crate) struct OsFile {
    file: File,
}

impl OsFile {
    pub fn create(path: &Path) -> std::io::Result<Self> {
        Ok(OsFile {
            file: File::options()
                .read(true)
                .write(true)
                .create_new(true)
                .open(path)?,
        })
    }

    pub fn open(path: &Path) -> std::io::Result<Self> {
        Ok(OsFile {
            file: File::options().read(true).write(true).open(path)?,
        })
    }
}

impl PageFile for OsFile {
    fn read_exact_at(&mut self, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(buf)
    }

    fn write_all_at(&mut self, buf: &[u8], offset: u64) -> std::io::Result<()> {
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(buf)
    }

    fn set_len(&mut self, len: u64) -> std::io::Result<()> {
        self.file.set_len(len)
    }

    fn sync(&mut self) -> std::io::Result<()> {
        self.file.sync_all()
    }
}

/// Pure memory: holes read as zeros, exactly like a sparse file.
pub(crate) struct MemoryFile {
    bytes: Vec<u8>,
}

impl MemoryFile {
    pub fn new() -> Self {
        MemoryFile { bytes: Vec::new() }
    }
}

impl PageFile for MemoryFile {
    fn read_exact_at(&mut self, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
        let start = offset as usize;
        let end = start + buf.len();
        if end > self.bytes.len() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "read past end of memory file",
            ));
        }
        buf.copy_from_slice(&self.bytes[start..end]);
        Ok(())
    }

    fn write_all_at(&mut self, buf: &[u8], offset: u64) -> std::io::Result<()> {
        let start = offset as usize;
        let end = start + buf.len();
        if end > self.bytes.len() {
            self.bytes.resize(end, 0);
        }
        self.bytes[start..end].copy_from_slice(buf);
        Ok(())
    }

    fn set_len(&mut self, len: u64) -> std::io::Result<()> {
        self.bytes.resize(len as usize, 0);
        Ok(())
    }

    fn sync(&mut self) -> std::io::Result<()> {
        Ok(()) // memory is the durable medium here
    }
}
