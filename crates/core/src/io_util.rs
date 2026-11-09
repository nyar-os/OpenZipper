use std::io::{Read, Write};

use crate::{
    error::{Error, ErrorCode, Result},
    limits::ArchiveLimits,
};

const CHUNK: usize = 64 * 1024;

/// Stream bytes while enforcing per-entry and archive-wide expansion limits.
pub(crate) fn copy_limited(
    reader: &mut (impl Read + ?Sized),
    writer: &mut impl Write,
    name: &str,
    limits: &ArchiveLimits,
    entry_bytes: &mut u64,
    total_bytes: &mut u64,
) -> Result<u64> {
    let mut buffer = vec![0_u8; CHUNK];
    let mut written = 0_u64;
    loop {
        let read = reader.read(&mut buffer).map_err(|error| Error::coded(ErrorCode::InvalidArchive, format!("`{name}`: {error}")))?;
        if read == 0 {
            break;
        }
        *entry_bytes = entry_bytes.saturating_add(read as u64);
        *total_bytes = total_bytes.saturating_add(read as u64);
        limits.check_written_bytes(name, *entry_bytes, *total_bytes)?;
        writer.write_all(&buffer[..read]).map_err(|error| Error::io(name, error))?;
        written = written.saturating_add(read as u64);
    }
    Ok(written)
}

/// Consume an entry without materializing it so streaming readers advance and
/// declared limits are verified against actual bytes.
pub(crate) fn drain_limited(
    reader: &mut (impl Read + ?Sized),
    name: &str,
    limits: &ArchiveLimits,
    entry_bytes: &mut u64,
    total_bytes: &mut u64,
) -> Result<u64> {
    let mut sink = std::io::sink();
    copy_limited(reader, &mut sink, name, limits, entry_bytes, total_bytes)
}

/// Reads at most `cap` bytes and reports whether the source still held more.
///
/// This is the preview primitive: the reader stops at the cap, so a large
/// entry never materializes and over-length is a `truncated` flag, not an error.
pub(crate) fn read_bounded(reader: &mut (impl Read + ?Sized), cap: u64) -> Result<(Vec<u8>, bool)> {
    let mut buffer = vec![0_u8; CHUNK];
    let mut output = Vec::new();
    let mut truncated = false;
    loop {
        let read = reader.read(&mut buffer).map_err(|error| Error::coded(ErrorCode::InvalidArchive, format!("bounded read: {error}")))?;
        if read == 0 {
            break;
        }
        let room = cap.saturating_sub(output.len() as u64);
        if (read as u64) > room {
            output.extend_from_slice(&buffer[..room as usize]);
            truncated = true;
            break;
        }
        output.extend_from_slice(&buffer[..read]);
    }
    Ok((output, truncated))
}
