use std::path::Path;

use crate::{
    error::Result,
    format::{self, ArchiveFormat},
    limits::ArchiveLimits,
    model::{ArchiveListing, EntryInfo},
    sevenz_codec, tar_codec, zip_codec,
};

pub fn list(path: &Path, limits: &ArchiveLimits) -> Result<ArchiveListing> {
    limits.validate()?;
    let format = format::detect(path)?;
    let prepared = match format {
        ArchiveFormat::Zip => zip_codec::inspect(path, limits, false)?,
        ArchiveFormat::SevenZ => sevenz_codec::inspect(path, limits, false)?,
        _ => tar_codec::inspect(path, format, limits, false)?,
    };

    let mut total_size_bytes = 0_u64;
    let mut unsafe_entry_count = 0;
    let entries = prepared
        .iter()
        .map(|entry| {
            if !entry.unsafe_reason.is_none() {
                unsafe_entry_count += 1;
            }
            if !matches!(entry.kind, crate::model::EntryKind::Directory) {
                total_size_bytes = total_size_bytes.saturating_add(entry.size);
            }
            EntryInfo {
                index: entry.index,
                name: entry.name.clone(),
                kind: entry.kind,
                size_bytes: entry.size,
                compressed_size_bytes: entry.compressed,
                compression_method: entry.compression_method.clone(),
                modified_at_unix_ms: entry.modified_at_unix_ms,
                safe: entry.unsafe_reason.is_none(),
                unsafe_reason: entry.unsafe_reason.clone(),
            }
        })
        .collect();

    Ok(ArchiveListing { path: path.display().to_string(), format, entry_count: prepared.len(), entries, total_size_bytes, unsafe_entry_count })
}
