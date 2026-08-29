//! Host-side block backends for the virtio-blk device.
//!
//! The device reads and writes the host file through this trait, so the
//! emulator never holds the disk image in memory and the existing artifact
//! limits (regular file, non-empty, at most 16 GiB) keep applying unchanged.

use std::fs::File;
use std::path::Path;

use crate::virtio::VirtioError;

/// Random-access byte storage behind the emulated disk.
pub trait BlockBackend {
    /// Total backend size in bytes.
    fn length(&self) -> u64;
    /// Reads exactly output.len() bytes at offset, failing closed on short
    /// reads.
    fn read_at(&mut self, offset: u64, output: &mut [u8]) -> Result<(), VirtioError>;
    /// Writes all of input at offset, failing closed on short writes.
    fn write_at(&mut self, offset: u64, input: &[u8]) -> Result<(), VirtioError>;
}

/// Block backend backed by a host file (the guest root disk image).
#[derive(Debug)]
pub struct FileBlockBackend {
    file: File,
    length: u64,
}

impl FileBlockBackend {
    /// Opens a disk image file and records its validated length. The caller
    /// (the provider artifact validation) already checked that the path
    /// names a regular, non-empty file within the engine size limit.
    pub fn open(path: &Path) -> Result<Self, std::io::Error> {
        // The guest mounts the root disk read-write (overlay staging, apk
        // cache), so the backend needs write access too. A read-only path
        // fails here instead of surfacing as a broken guest disk later.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        let length = file.metadata()?.len();
        Ok(Self { file, length })
    }
}

impl BlockBackend for FileBlockBackend {
    fn length(&self) -> u64 {
        self.length
    }

    #[cfg(unix)]
    fn read_at(&mut self, offset: u64, output: &mut [u8]) -> Result<(), VirtioError> {
        use std::os::unix::fs::FileExt;
        self.file
            .read_exact_at(output, offset)
            .map_err(|error| VirtioError::Backend(format!("host disk read failed: {error}")))
    }

    #[cfg(not(unix))]
    fn read_at(&mut self, offset: u64, output: &mut [u8]) -> Result<(), VirtioError> {
        use std::io::{Read, Seek, SeekFrom};
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|error| VirtioError::Backend(format!("host disk seek failed: {error}")))?;
        self.file
            .read_exact(output)
            .map_err(|error| VirtioError::Backend(format!("host disk read failed: {error}")))
    }

    #[cfg(unix)]
    fn write_at(&mut self, offset: u64, input: &[u8]) -> Result<(), VirtioError> {
        use std::os::unix::fs::FileExt;
        self.file
            .write_all_at(input, offset)
            .map_err(|error| VirtioError::Backend(format!("host disk write failed: {error}")))
    }

    #[cfg(not(unix))]
    fn write_at(&mut self, offset: u64, input: &[u8]) -> Result<(), VirtioError> {
        use std::io::{Seek, SeekFrom, Write};
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|error| VirtioError::Backend(format!("host disk seek failed: {error}")))?;
        self.file
            .write_all(input)
            .map_err(|error| VirtioError::Backend(format!("host disk write failed: {error}")))
    }
}

/// In-memory backend for unit tests.
#[derive(Debug, Default)]
pub struct VecBlockBackend {
    pub bytes: Vec<u8>,
}

impl BlockBackend for VecBlockBackend {
    fn length(&self) -> u64 {
        self.bytes.len() as u64
    }

    fn read_at(&mut self, offset: u64, output: &mut [u8]) -> Result<(), VirtioError> {
        let end = offset
            .checked_add(output.len() as u64)
            .ok_or_else(|| VirtioError::Backend("read offset overflowed".to_owned()))?;
        if end > self.bytes.len() as u64 {
            return Err(VirtioError::Backend(
                "read past the end of the backend".to_owned(),
            ));
        }
        output.copy_from_slice(&self.bytes[offset as usize..end as usize]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, input: &[u8]) -> Result<(), VirtioError> {
        let end = offset
            .checked_add(input.len() as u64)
            .ok_or_else(|| VirtioError::Backend("write offset overflowed".to_owned()))?;
        if end > self.bytes.len() as u64 {
            return Err(VirtioError::Backend(
                "write past the end of the backend".to_owned(),
            ));
        }
        self.bytes[offset as usize..end as usize].copy_from_slice(input);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vec_backend_round_trips() {
        let mut backend = VecBlockBackend {
            bytes: vec![0xAA; 4096],
        };
        assert_eq!(backend.length(), 4096);
        backend.write_at(100, b"hello").unwrap();
        let mut buffer = [0_u8; 5];
        backend.read_at(100, &mut buffer).unwrap();
        assert_eq!(&buffer, b"hello");
    }

    #[test]
    fn vec_backend_rejects_out_of_bounds() {
        let mut backend = VecBlockBackend {
            bytes: vec![0; 512],
        };
        let mut buffer = [0_u8; 4];
        assert!(backend.read_at(510, &mut buffer).is_err());
        assert!(backend.write_at(511, b"ab").is_err());
    }

    #[test]
    fn file_backend_reads_back_what_it_wrote() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("disk.img");
        std::fs::write(&path, vec![0x11; 8192]).unwrap();
        let mut backend = FileBlockBackend::open(&path).unwrap();
        assert_eq!(backend.length(), 8192);
        backend.write_at(4096, &[1, 2, 3, 4]).unwrap();
        let mut buffer = [0_u8; 4];
        backend.read_at(4096, &mut buffer).unwrap();
        assert_eq!(buffer, [1, 2, 3, 4]);
    }
}
