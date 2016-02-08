use std::io::{Read, Write};

use crate::{
    error::{Error, ErrorCode, Result},
    limits::ArchiveLimits,
};

const CHUNK: usize = 64 * 1024;

/// Stream bytes while enforcing per-entry and archive-wide expansion limits.
pub(crate) fn copy_limited(
    reader: &mut impl Read,
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
    reader: &mut impl Read,
    name: &str,
    limits: &ArchiveLimits,
    entry_bytes: &mut u64,
    total_bytes: &mut u64,
) -> Result<u64> {
    let mut sink = std::io::sink();
    copy_limited(reader, &mut sink, name, limits, entry_bytes, total_bytes)
}
