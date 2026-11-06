//! Random-access byte sources for archive backends.
//!
//! The source contract belongs to the shared Acorn substrate. Acorn ships
//! in-memory and range-map sources but no file-backed one, so OpenZipper
//! supplies it here. Keeping this adapter in the consumer layer means archive
//! policy (path safety, limits, reporting) never leaks into the shared crate.

use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};

use acorn_source::{ByteSource, ReadOutcome, ReadResult, SourceCapabilities, SourceIdentity};

use crate::{Error, Result};

/// A read-only archive file exposed as a random-access byte source.
///
/// Reads are positional, so several ranges may be read concurrently without a
/// shared file cursor. Bytes are never padded past EOF and a short read is
/// reported as [`ReadOutcome::Partial`] rather than silently filled.
pub struct FileSource {
    file: File,
    path: PathBuf,
    identity: SourceIdentity,
    length: u64,
}

impl FileSource {
    /// Opens `path` for positional reads and records its length once.
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).map_err(|error| Error::io(path.display().to_string(), error))?;
        let length = file.metadata().map_err(|error| Error::io(path.display().to_string(), error))?.len();
        let identity = SourceIdentity::new(path.display().to_string());
        Ok(Self { file, path: path.to_path_buf(), identity, length })
    }

    /// Archive path this source was opened from.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl ByteSource for FileSource {
    fn length(&self) -> Option<u64> {
        Some(self.length)
    }

    fn read_at(&self, offset: u64, destination: &mut [u8]) -> ReadResult {
        if destination.is_empty() {
            return Ok(ReadOutcome::Complete);
        }
        if offset >= self.length {
            return Ok(ReadOutcome::Eof);
        }
        let available = usize::try_from(self.length - offset).unwrap_or(usize::MAX);
        let wanted = destination.len().min(available);
        let mut read = 0_usize;
        while read < wanted {
            match read_at_offset(&self.file, &mut destination[read..wanted], offset + read as u64) {
                Ok(0) => break,
                Ok(count) => read += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        match read {
            0 => Ok(ReadOutcome::Eof),
            count if count == destination.len() => Ok(ReadOutcome::Complete),
            count => Ok(ReadOutcome::Partial { bytes_read: count as u64 }),
        }
    }

    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities {
            random_access: true,
            known_length: true,
            repeatable: true,
            concurrent_reads: cfg!(any(windows, unix)),
            stable_identity: true,
            decoded_views: false,
        }
    }

    fn identity(&self) -> SourceIdentity {
        self.identity.clone()
    }
}

/// Decoded archive bytes held in memory as a random-access source.
///
/// Bzip2 and xz streams are decoded before the tar index is built, so extraction
/// still reads member ranges instead of re-decompressing the whole stream.
pub struct SliceSource {
    bytes: Vec<u8>,
    identity: SourceIdentity,
}

impl SliceSource {
    pub(crate) fn new(label: impl Into<String>, bytes: Vec<u8>) -> Self {
        let identity = SourceIdentity::new(label.into()).with_content_digest(acorn_source::content_digest_of(&bytes));
        Self { bytes, identity }
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl ByteSource for SliceSource {
    fn length(&self) -> Option<u64> {
        Some(self.bytes.len() as u64)
    }

    fn read_at(&self, offset: u64, destination: &mut [u8]) -> ReadResult {
        if offset >= self.bytes.len() as u64 {
            return Ok(ReadOutcome::Eof);
        }
        let start = offset as usize;
        let count = destination.len().min(self.bytes.len() - start);
        destination[..count].copy_from_slice(&self.bytes[start..start + count]);
        match count {
            read if read == destination.len() => Ok(ReadOutcome::Complete),
            read => Ok(ReadOutcome::Partial { bytes_read: read as u64 }),
        }
    }

    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities {
            random_access: true,
            known_length: true,
            repeatable: true,
            concurrent_reads: true,
            stable_identity: true,
            decoded_views: false,
        }
    }

    fn identity(&self) -> SourceIdentity {
        self.identity.clone()
    }
}

/// Lets Acorn helpers borrow the open file instead of taking ownership.
impl ByteSource for &FileSource {
    fn length(&self) -> Option<u64> {
        (**self).length()
    }

    fn read_at(&self, offset: u64, destination: &mut [u8]) -> ReadResult {
        (**self).read_at(offset, destination)
    }

    fn capabilities(&self) -> SourceCapabilities {
        (**self).capabilities()
    }

    fn identity(&self) -> SourceIdentity {
        (**self).identity()
    }
}

/// Reads a bounded range of a [`ByteSource`] as a sequential [`Read`].
///
/// Archive backends hand out ranges, not readers, so extraction streams through
/// this adapter to keep the existing per-entry and total byte limits in force.
pub(crate) struct SourceReader<S: ByteSource> {
    source: S,
    offset: u64,
    remaining: u64,
}

impl<S: ByteSource> SourceReader<S> {
    pub(crate) fn new(source: S, start: u64, len: u64) -> Self {
        Self { source, offset: start, remaining: len }
    }
}

impl<S: ByteSource> Read for SourceReader<S> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() || self.remaining == 0 {
            return Ok(0);
        }
        let wanted = buffer.len().min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        let count = match self.source.read_at(self.offset, &mut buffer[..wanted])? {
            ReadOutcome::Complete => wanted,
            ReadOutcome::Partial { bytes_read } => usize::try_from(bytes_read).unwrap_or(0),
            ReadOutcome::Eof | ReadOutcome::Unavailable { .. } => 0,
        };
        if count == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "archive source ended before the declared range"));
        }
        self.offset += count as u64;
        self.remaining -= count as u64;
        Ok(count)
    }
}

#[cfg(windows)]
fn read_at_offset(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_read(buffer, offset)
}

#[cfg(unix)]
fn read_at_offset(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.read_at(buffer, offset)
}

#[cfg(not(any(windows, unix)))]
fn read_at_offset(_file: &File, _buffer: &mut [u8], _offset: u64) -> io::Result<usize> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "positional reads are not supported on this platform"))
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
    };

    use super::*;

    fn temp_file(bytes: &[u8]) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!("open-zipper-source-{}-{}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed)));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn bytes(count: usize) -> Vec<u8> {
        (0..count).map(|index| (index % 251) as u8).collect()
    }

    #[test]
    fn reads_the_requested_range() {
        let path = temp_file(&bytes(64));
        let source = FileSource::open(&path).unwrap();
        let mut buffer = [0_u8; 16];
        assert!(matches!(source.read_at(8, &mut buffer), Ok(ReadOutcome::Complete)));
        assert_eq!(buffer, bytes(64)[8..24]);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn tail_read_is_partial() {
        let path = temp_file(&bytes(20));
        let source = FileSource::open(&path).unwrap();
        let mut buffer = [0_u8; 8];
        assert!(matches!(source.read_at(16, &mut buffer), Ok(ReadOutcome::Partial { bytes_read: 4 })));
        assert_eq!(&buffer[..4], &bytes(20)[16..]);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn read_at_or_past_end_is_eof() {
        let path = temp_file(&bytes(10));
        let source = FileSource::open(&path).unwrap();
        let mut buffer = [0_u8; 4];
        assert!(matches!(source.read_at(10, &mut buffer), Ok(ReadOutcome::Eof)));
        assert!(matches!(source.read_at(11, &mut buffer), Ok(ReadOutcome::Eof)));
        assert_eq!(buffer, [0; 4], "EOF must not zero-fill");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn empty_destination_is_complete() {
        let path = temp_file(&bytes(4));
        let source = FileSource::open(&path).unwrap();
        assert!(matches!(source.read_at(99, &mut []), Ok(ReadOutcome::Complete)));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn reports_length_identity_and_capabilities() {
        let path = temp_file(&bytes(32));
        let source = FileSource::open(&path).unwrap();
        assert_eq!(source.length(), Some(32));
        assert_eq!(source.identity().label, path.display().to_string());
        let capabilities = source.capabilities();
        assert!(capabilities.random_access);
        assert!(capabilities.known_length);
        assert!(capabilities.repeatable);
        assert!(capabilities.stable_identity);
        assert!(!capabilities.decoded_views);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn concurrent_reads_do_not_share_a_cursor() {
        let path = temp_file(&bytes(256));
        let source = Arc::new(FileSource::open(&path).unwrap());
        let left = Arc::clone(&source);
        let right = Arc::clone(&source);
        let first = std::thread::spawn(move || {
            let mut buffer = [0_u8; 32];
            assert!(matches!(left.read_at(0, &mut buffer), Ok(ReadOutcome::Complete)));
            buffer
        });
        let second = std::thread::spawn(move || {
            let mut buffer = [0_u8; 32];
            assert!(matches!(right.read_at(128, &mut buffer), Ok(ReadOutcome::Complete)));
            buffer
        });
        assert_eq!(first.join().unwrap(), bytes(256)[0..32]);
        assert_eq!(second.join().unwrap(), bytes(256)[128..160]);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn missing_file_is_an_io_error() {
        let path = std::env::temp_dir().join("open-zipper-source-missing-file");
        let _ = std::fs::remove_file(&path);
        match FileSource::open(&path) {
            Ok(_) => panic!("opening a missing file must fail"),
            Err(error) => assert_eq!(error.code(), "io-error"),
        }
    }
}
