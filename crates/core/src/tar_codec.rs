use std::{
    collections::HashSet,
    fs::{self, File},
    io::{BufReader, Read},
    path::Path,
};

use bzip2::read::BzDecoder;
use flate2::read::GzDecoder;
use tar::Archive;
use xz2::read::XzDecoder;

use crate::{
    encoding::decode_name,
    error::{Error, ErrorCode, Result},
    extract::{ExtractContext, decide_target, record_directory, record_file},
    format::ArchiveFormat,
    io_util::{copy_limited, drain_limited},
    limits::ArchiveLimits,
    model::{EntryKind, ExtractAction, ExtractOptions, ExtractReport},
    platform::Platform,
    safety::{join_destination, validate_entry_path},
    zip_codec::PreparedEntry,
};

type BoxedReader = Box<dyn Read>;

pub(crate) fn open(path: &Path, format: ArchiveFormat) -> Result<Archive<BoxedReader>> {
    let file = File::open(path).map_err(|error| Error::io(path.display().to_string(), error))?;
    let reader: BoxedReader = match format {
        ArchiveFormat::Tar => Box::new(BufReader::new(file)),
        ArchiveFormat::TarGz => Box::new(GzDecoder::new(BufReader::new(file))),
        ArchiveFormat::TarBz2 => Box::new(BzDecoder::new(BufReader::new(file))),
        ArchiveFormat::TarXz => Box::new(XzDecoder::new(BufReader::new(file))),
        ArchiveFormat::Zip | ArchiveFormat::SevenZ => {
            return Err(Error::coded(ErrorCode::InvalidOption, format!("`{}` is not a tar stream", format.label())));
        }
    };
    Ok(Archive::new(reader))
}

pub(crate) fn inspect(path: &Path, format: ArchiveFormat, limits: &ArchiveLimits, strict: bool) -> Result<Vec<PreparedEntry>> {
    let archive_bytes = fs::metadata(path).map_err(|error| Error::io(path.display().to_string(), error))?.len();
    let platform = Platform::current();
    let mut archive = open(path, format)?;
    let mut prepared = Vec::new();
    let mut total_declared = 0_u64;
    let mut total_actual = 0_u64;
    let mut seen = HashSet::new();

    for (index, entry) in archive.entries().map_err(Error::from)?.enumerate() {
        let mut entry = entry.map_err(Error::from)?;
        let name = decode_name(&entry.path_bytes());
        let kind = entry_kind(&entry);
        let size = entry.size();
        let modified_at_unix_ms = entry.header().mtime().ok().map(|seconds| seconds as i64 * 1_000);
        total_declared = total_declared.saturating_add(if kind == EntryKind::Directory { 0 } else { size });
        limits.check_entry_count(index + 1)?;
        limits.check_declared_entry(&name, size, None, total_declared)?;

        let mut safe = None;
        let mut unsafe_reason = None;
        match validate_entry_path(&name, platform) {
            Ok(path) => {
                if matches!(kind, EntryKind::Symlink | EntryKind::Hardlink) {
                    let reason = format!("entry `{name}` is a link and could escape the destination");
                    if strict {
                        return Err(Error::coded(ErrorCode::SymlinkEntry, reason));
                    }
                    unsafe_reason = Some(reason);
                }
                else if !matches!(kind, EntryKind::File | EntryKind::Directory) {
                    let reason = format!("entry `{name}` has an unsupported tar entry type");
                    if strict {
                        return Err(Error::coded(ErrorCode::InvalidArchive, reason));
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

        let mut entry_bytes = 0_u64;
        if kind != EntryKind::Directory && !matches!(kind, EntryKind::Symlink | EntryKind::Hardlink) {
            drain_limited(&mut entry, &name, limits, &mut entry_bytes, &mut total_actual)?;
            limits.check_global_ratio(total_actual, archive_bytes)?;
        }

        prepared.push(PreparedEntry {
            index,
            name,
            kind,
            size,
            compressed: None,
            compression_method: Some("store".to_string()),
            modified_at_unix_ms,
            safe,
            unsafe_reason,
        });
    }

    Ok(prepared)
}

pub(crate) fn extract(
    path: &Path,
    format: ArchiveFormat,
    entries: &[PreparedEntry],
    options: &ExtractOptions,
    context: &mut ExtractContext,
    report: &mut ExtractReport,
) -> Result<()> {
    let archive_bytes = fs::metadata(path).map_err(|error| Error::io(path.display().to_string(), error))?.len();
    let mut archive = open(path, format)?;
    let mut total_bytes = 0_u64;

    for (index, result) in archive.entries().map_err(Error::from)?.enumerate() {
        let mut entry = result.map_err(Error::from)?;
        let name = decode_name(&entry.path_bytes());
        let kind = entry_kind(&entry);
        let prepared =
            entries.get(index).ok_or_else(|| Error::coded(ErrorCode::InvalidArchive, format!("entry `{name}` disappeared between passes")))?;
        let safe = prepared.safe_path()?.clone();

        match kind {
            EntryKind::Directory => {
                if !options.dry_run {
                    let target = join_destination(&options.destination, &safe);
                    fs::create_dir_all(&target).map_err(|error| Error::io(target.display().to_string(), error))?;
                }
                record_directory(report, &name)?;
            }
            EntryKind::File => {
                let target = decide_target(&options.destination, &safe, options.overwrite)?;
                match &target {
                    None => {
                        report.skipped += 1;
                        report.entries.push(crate::model::ExtractOutcome {
                            name: name.clone(),
                            action: ExtractAction::SkippedExists,
                            bytes_written: 0,
                            renamed_to: None,
                        });
                        let mut entry_bytes = 0_u64;
                        drain_limited(&mut entry, &name, &options.limits, &mut entry_bytes, &mut total_bytes)?;
                    }
                    Some(target) if options.dry_run => {
                        record_file(report, &name, entry.size(), ExtractAction::WouldWrite, None, None, context);
                        let mut entry_bytes = 0_u64;
                        drain_limited(&mut entry, &name, &options.limits, &mut entry_bytes, &mut total_bytes)?;
                    }
                    Some(target) => {
                        if let Some(parent) = target.path.parent() {
                            fs::create_dir_all(parent).map_err(|error| Error::io(parent.display().to_string(), error))?;
                        }
                        let mut file = File::create(&target.path).map_err(|error| Error::io(target.path.display().to_string(), error))?;
                        let mut entry_bytes = 0_u64;
                        let bytes = copy_limited(&mut entry, &mut file, &name, &options.limits, &mut entry_bytes, &mut total_bytes)?;
                        drop(file);
                        if target.renamed {
                            report.renamed += 1;
                            record_file(
                                report,
                                &name,
                                bytes,
                                ExtractAction::Renamed,
                                Some(target.relative_name.clone()),
                                Some(target.path.clone()),
                                context,
                            );
                        }
                        else {
                            record_file(report, &name, bytes, ExtractAction::Written, None, Some(target.path.clone()), context);
                        }
                    }
                }
                limits_ratio(&options.limits, total_bytes, archive_bytes)?;
            }
            EntryKind::Symlink | EntryKind::Hardlink | EntryKind::Other => {
                return Err(Error::coded(ErrorCode::SymlinkEntry, format!("entry `{name}` has a type that is not extracted")));
            }
        }
    }

    Ok(())
}

fn limits_ratio(limits: &ArchiveLimits, total: u64, archive_bytes: u64) -> Result<()> {
    limits.check_global_ratio(total, archive_bytes)
}

fn entry_kind(entry: &tar::Entry<'_, BoxedReader>) -> EntryKind {
    let entry_type = entry.header().entry_type();
    if entry_type.is_dir() {
        EntryKind::Directory
    }
    else if entry_type.is_file() {
        EntryKind::File
    }
    else if entry_type.is_symlink() {
        EntryKind::Symlink
    }
    else if entry_type.is_hard_link() {
        EntryKind::Hardlink
    }
    else {
        EntryKind::Other
    }
}
