use std::{
    collections::HashSet,
    fs::{self, File},
    path::Path,
};

use zip::{ZipArchive, result::ZipError};

use crate::{
    encoding::decode_name,
    error::{Error, ErrorCode, Result},
    extract::{ExtractContext, decide_target, record_directory, record_file},
    io_util::copy_limited,
    limits::ArchiveLimits,
    model::{EntryKind, ExtractOptions, ExtractReport},
    platform::Platform,
    safety::{SafeEntryPath, join_destination, validate_entry_path},
};

#[derive(Debug, Clone)]
pub(crate) struct PreparedEntry {
    pub index: usize,
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    pub compressed: Option<u64>,
    pub safe: Option<SafeEntryPath>,
    pub unsafe_reason: Option<String>,
}

impl PreparedEntry {
    pub fn safe_path(&self) -> Result<&SafeEntryPath> {
        match (&self.safe, &self.unsafe_reason) {
            (Some(path), None) => Ok(path),
            (_, Some(reason)) => Err(Error::coded(ErrorCode::ZipSlip, format!("entry `{}` was rejected: {reason}", self.name))),
            (None, None) => Err(Error::coded(ErrorCode::ZipSlip, format!("entry `{}` has no validated path", self.name))),
        }
    }
}

pub(crate) fn open(path: &Path) -> Result<ZipArchive<File>> {
    let file = File::open(path).map_err(|error| Error::io(path.display().to_string(), error))?;
    ZipArchive::new(file).map_err(map_zip_error)
}

fn map_zip_error(error: ZipError) -> Error {
    if matches!(error, ZipError::Io(_)) {
        return Error::coded(ErrorCode::InvalidArchive, format!("zip: {error}"));
    }
    Error::coded(ErrorCode::InvalidArchive, format!("zip: {error}"))
}

/// Read the central directory and validate every entry before any write.
pub(crate) fn inspect(path: &Path, limits: &ArchiveLimits, strict: bool) -> Result<Vec<PreparedEntry>> {
    let mut archive = open(path)?;
    limits.check_entry_count(archive.len())?;
    let platform = Platform::current();
    let mut prepared = Vec::with_capacity(archive.len());
    let mut total = 0_u64;
    let mut seen = HashSet::new();

    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(map_zip_error)?;
        let name = decode_name(entry.name_raw());
        let kind = if entry.is_dir() {
            EntryKind::Directory
        }
        else if entry.is_symlink() {
            EntryKind::Symlink
        }
        else {
            EntryKind::File
        };
        let size = entry.size();
        let compressed = Some(entry.compressed_size());
        total = total.saturating_add(if kind == EntryKind::Directory { 0 } else { size });
        limits.check_declared_entry(&name, size, compressed, total)?;

        let mut safe = None;
        let mut unsafe_reason = None;
        match validate_entry_path(&name, platform) {
            Ok(path) => {
                if kind == EntryKind::Symlink {
                    let reason = format!("entry `{name}` is a symbolic link and could escape the destination");
                    if strict {
                        return Err(Error::coded(ErrorCode::SymlinkEntry, reason));
                    }
                    unsafe_reason = Some(reason);
                }
                else if !seen.insert(path.normalized_key.clone()) {
                    let reason = format!("entry `{name}` collides with another entry after path normalization");
                    if strict {
                        return Err(Error::coded(ErrorCode::PathConflict, reason));
                    }
                    unsafe_reason = Some(reason);
                }
                else {
                    safe = Some(path);
                }
            }
            Err(error) => {
                if strict {
                    return Err(error);
                }
                unsafe_reason = Some(error.to_string());
            }
        }

        prepared.push(PreparedEntry { index, name, kind, size, compressed, safe, unsafe_reason });
    }

    Ok(prepared)
}

pub(crate) fn extract(
    path: &Path,
    entries: &[PreparedEntry],
    options: &ExtractOptions,
    context: &mut ExtractContext,
    report: &mut ExtractReport,
) -> Result<()> {
    let mut archive = open(path)?;
    let mut entry_bytes;
    let mut total_bytes = 0_u64;

    for entry in entries {
        let safe = entry.safe_path()?;
        match entry.kind {
            EntryKind::Directory => {
                if !options.dry_run {
                    let target = join_destination(&options.destination, safe);
                    fs::create_dir_all(&target).map_err(|error| Error::io(target.display().to_string(), error))?;
                }
                record_directory(report, &entry.name)?;
            }
            EntryKind::File => {
                let target = match decide_target(&options.destination, safe, options.overwrite)? {
                    Some(target) => target,
                    None => {
                        report.skipped += 1;
                        report.entries.push(crate::model::ExtractOutcome {
                            name: entry.name.clone(),
                            action: crate::model::ExtractAction::SkippedExists,
                            bytes_written: 0,
                            renamed_to: None,
                        });
                        continue;
                    }
                };

                let mut renamed_to = None;
                if target.renamed {
                    renamed_to = Some(target.relative_name.clone());
                }

                if options.dry_run {
                    record_file(report, &entry.name, entry.size, crate::model::ExtractAction::WouldWrite, renamed_to, None, context);
                    continue;
                }

                let mut source = archive.by_index(entry.index).map_err(map_zip_error)?;
                if let Some(parent) = target.path.parent() {
                    fs::create_dir_all(parent).map_err(|error| Error::io(parent.display().to_string(), error))?;
                }
                let mut file = File::create(&target.path).map_err(|error| Error::io(target.path.display().to_string(), error))?;
                entry_bytes = 0;
                let written = copy_limited(&mut source, &mut file, &entry.name, &options.limits, &mut entry_bytes, &mut total_bytes)?;
                drop(file);

                let action = if renamed_to.is_some() {
                    report.renamed += 1;
                    crate::model::ExtractAction::Renamed
                }
                else {
                    crate::model::ExtractAction::Written
                };
                record_file(report, &entry.name, written, action, renamed_to, Some(target.path.clone()), context);
            }
            EntryKind::Symlink | EntryKind::Hardlink | EntryKind::Other => {
                return Err(Error::coded(ErrorCode::SymlinkEntry, format!("entry `{}` has a type that is not extracted", entry.name)));
            }
        }
    }

    Ok(())
}
