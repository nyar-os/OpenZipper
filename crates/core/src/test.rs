use std::path::Path;

use crate::{
    error::{Error, Result},
    format::{self, ArchiveFormat},
    io_util::drain_limited,
    limits::ArchiveLimits,
    model::TestReport,
    sevenz_codec, tar_codec, zip_codec,
};

/// Full read-through integrity check. Zip CRC errors surface while reading,
/// tar truncation surfaces as an IO error.
pub fn test(path: &Path, limits: &ArchiveLimits) -> Result<TestReport> {
    limits.validate()?;
    let format = format::detect(path)?;
    match format {
        ArchiveFormat::Zip => test_zip(path, limits, format),
        ArchiveFormat::SevenZ => test_sevenz(path, limits, format),
        _ => test_tar(path, format, limits),
    }
}

fn test_zip(path: &Path, limits: &ArchiveLimits, format: ArchiveFormat) -> Result<TestReport> {
    let mut archive = zip_codec::open(path)?;
    limits.check_entry_count(archive.len())?;
    let mut total_bytes = 0_u64;
    let mut checked = 0_usize;

    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(Error::from)?;
        let name = crate::encoding::decode_name(entry.name_raw());
        if entry.is_dir() || entry.is_symlink() {
            continue;
        }
        let mut entry_bytes = 0_u64;
        drain_limited(&mut entry, &name, limits, &mut entry_bytes, &mut total_bytes)?;
        checked += 1;
    }

    Ok(TestReport { archive: path.display().to_string(), format, entry_count: checked, total_bytes, ok: true })
}

fn test_tar(path: &Path, format: ArchiveFormat, limits: &ArchiveLimits) -> Result<TestReport> {
    let entries = tar_codec::inspect(path, format, limits, false)?;
    let entry_count = entries.iter().filter(|entry| !matches!(entry.kind, crate::model::EntryKind::Directory)).count();
    let total_bytes = entries.iter().filter(|entry| !matches!(entry.kind, crate::model::EntryKind::Directory)).map(|entry| entry.size).sum();
    Ok(TestReport { archive: path.display().to_string(), format, entry_count, total_bytes, ok: true })
}

fn test_sevenz(path: &Path, limits: &ArchiveLimits, format: ArchiveFormat) -> Result<TestReport> {
    let (entry_count, total_bytes) = sevenz_codec::test(path, limits)?;
    Ok(TestReport { archive: path.display().to_string(), format, entry_count, total_bytes, ok: true })
}
