//! Single-entry extraction into a caller-managed directory, for "open with
//! the default program". The destination name is a sanitized basename, so the
//! entry path can never steer the write outside the destination directory.

use serde::Serialize;

use std::path::Path;

use crate::{
    error::{Error, ErrorCode, Result},
    format::{self, ArchiveFormat},
    limits::ArchiveLimits,
    model::EntryKind,
    safety::is_reserved_device_name,
    sevenz_codec, tar_codec, zip_codec,
};

/// One entry materialized as a single file inside the destination directory.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractedEntryFile {
    /// Sanitized file name the entry was written under.
    pub name: String,
    /// Absolute path of the written file.
    pub path: String,
    pub bytes_written: u64,
}

/// Extracts one entry to `dest_dir/<sanitized basename>` under the full limits.
///
/// An existing destination file is a `path_conflict`: external open never
/// overwrites silently. Directories and links are refused with the same codes
/// as extraction.
pub fn extract_entry(path: &Path, index: usize, dest_dir: &Path, limits: &ArchiveLimits) -> Result<ExtractedEntryFile> {
    limits.validate()?;
    let archive_format = format::detect(path)?;
    let prepared = crate::preview::inspect(path, archive_format, limits)?;
    let entry = prepared.get(index).ok_or_else(|| Error::coded(ErrorCode::InvalidArchive, format!("entry index {index} is out of range")))?;

    match entry.kind {
        EntryKind::Directory => return Err(Error::coded(ErrorCode::InvalidOption, format!("entry `{}` is a directory", entry.name))),
        EntryKind::Symlink | EntryKind::Hardlink | EntryKind::Other => {
            return Err(Error::coded(ErrorCode::SymlinkEntry, format!("entry `{}` is not a regular file", entry.name)));
        }
        EntryKind::File => {}
    }

    let name = sanitize_basename(&entry.name);
    let dest = dest_dir.join(&name);
    if dest.exists() {
        return Err(Error::coded(ErrorCode::PathConflict, format!("`{}` already exists in the destination", name)));
    }

    let bytes_written = match archive_format {
        ArchiveFormat::Zip => zip_codec::write_member(path, index, &dest, limits)?,
        ArchiveFormat::SevenZ => sevenz_codec::write_member(path, &entry.name, &dest, limits)?,
        other => tar_codec::write_member(path, other, index, &dest, limits)?,
    };

    Ok(ExtractedEntryFile { name, path: dest.display().to_string(), bytes_written })
}

/// Collapses an entry name to one safe file name: last path segment, no
/// separators, colons, control characters, trailing dots or reserved devices.
pub(crate) fn sanitize_basename(name: &str) -> String {
    let unified = name.replace('\\', "/");
    let last = unified.rsplit('/').find(|part| !part.is_empty() && *part != "." && *part != "..").unwrap_or("");
    let mut cleaned: String = last.chars().filter(|character| !character.is_control() && *character != ':').collect();
    cleaned = cleaned.trim_matches(['.', ' ']).to_string();
    if cleaned.is_empty() {
        cleaned = "entry".to_string();
    }
    if is_reserved_device_name(&cleaned) {
        cleaned = format!("_{cleaned}");
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traversal_names_collapse_to_one_basename() {
        assert_eq!(sanitize_basename("a/../../evil.txt"), "evil.txt");
        assert_eq!(sanitize_basename("C:\\Windows\\system.ini"), "system.ini");
        assert_eq!(sanitize_basename("CON"), "_CON");
        assert_eq!(sanitize_basename("name."), "name");
        assert_eq!(sanitize_basename("///"), "entry");
    }
}
