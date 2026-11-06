use std::{
    collections::HashSet,
    fs::{self, File},
    io::{BufReader, Read},
    path::Path,
};

use acorn_codec::CodecLimits;
use acorn_tar::{TarArchive, TarEntry, TarEntryKind, TarGz, TarGzArchive, TarGzError, TarIndexError, index_tar};
use bzip2::read::BzDecoder;
use xz2::read::XzDecoder;

use crate::{
    encoding::decode_name,
    error::{Error, ErrorCode, Result},
    extract::{ExtractContext, decide_target, record_directory, record_file},
    format::ArchiveFormat,
    io_util::copy_limited,
    limits::ArchiveLimits,
    model::{EntryKind, ExtractAction, ExtractOptions, ExtractOutcome, ExtractReport},
    platform::Platform,
    safety::{join_destination, validate_entry_path},
    source::{FileSource, SliceSource, SourceReader},
    zip_codec::PreparedEntry,
};

/// Where the tar byte space lives for the current archive.
enum Backing {
    /// Plain tar read directly from disk, entry payloads stream through ranges.
    File { source: FileSource, archive: TarArchive },
    /// A decoded compressed tar held in memory.
    Memory { source: SliceSource, archive: TarArchive },
    /// `tar.gz` decoded by the Acorn gzip codec.
    Gz(TarGzArchive),
}

impl Backing {
    fn entries(&self) -> &[TarEntry] {
        match self {
            Self::File { archive, .. } | Self::Memory { archive, .. } => &archive.entries,
            Self::Gz(archive) => &archive.archive().entries,
        }
    }

    fn reader(&self, entry: &TarEntry) -> Result<Box<dyn Read + '_>> {
        match self {
            Self::File { source, .. } => Ok(Box::new(SourceReader::new(source, entry.data_range.start, entry.data_range.len))),
            Self::Memory { source, .. } => {
                let start = usize::try_from(entry.data_range.start).unwrap_or(usize::MAX);
                let end = start.saturating_add(usize::try_from(entry.data_range.len).unwrap_or(usize::MAX));
                source
                    .as_bytes()
                    .get(start..end)
                    .map(|payload| Box::new(payload) as Box<dyn Read>)
                    .ok_or_else(|| Error::coded(ErrorCode::InvalidArchive, "tar: member payload lies outside the decoded tar bytes"))
            }
            Self::Gz(archive) => archive
                .payload(entry)
                .map(|payload| Box::new(payload) as Box<dyn Read>)
                .ok_or_else(|| Error::coded(ErrorCode::InvalidArchive, "tar.gz: member payload lies outside the decoded tar bytes")),
        }
    }
}

fn load(path: &Path, format: ArchiveFormat, limits: &ArchiveLimits) -> Result<Backing> {
    let label = path.display().to_string();
    match format {
        ArchiveFormat::Tar => {
            let source = FileSource::open(path)?;
            let archive = index_tar(&source).map_err(map_index_error)?;
            Ok(Backing::File { source, archive })
        }
        ArchiveFormat::TarGz => {
            let bytes = fs::read(path).map_err(|error| Error::io(label.clone(), error))?;
            // Tar headers add a bounded amount on top of the payload bytes.
            let decoded_limit = limits.max_total_bytes.saturating_add((limits.max_entries as u64).saturating_mul(1_024));
            let codec = CodecLimits::new().with_max_input_bytes(bytes.len() as u64).with_max_output_bytes(decoded_limit);
            let archive = TarGz::decode(label, &bytes, codec).map_err(map_targz_error)?;
            Ok(Backing::Gz(archive))
        }
        ArchiveFormat::TarBz2 | ArchiveFormat::TarXz => {
            let file = File::open(path).map_err(|error| Error::io(label.clone(), error))?;
            let reader: Box<dyn Read> = match format {
                ArchiveFormat::TarBz2 => Box::new(BzDecoder::new(BufReader::new(file))),
                _ => Box::new(XzDecoder::new(BufReader::new(file))),
            };
            let bytes = read_all_limited(reader, limits, &label)?;
            let source = SliceSource::new(label, bytes);
            let archive = index_tar(&source).map_err(map_index_error)?;
            Ok(Backing::Memory { source, archive })
        }
        ArchiveFormat::Zip | ArchiveFormat::SevenZ => {
            Err(Error::coded(ErrorCode::InvalidOption, format!("`{}` is not a tar stream", format.label())))
        }
    }
}

fn map_index_error(error: TarIndexError) -> Error {
    Error::coded(ErrorCode::InvalidArchive, format!("tar: {error}"))
}

fn map_targz_error(error: TarGzError) -> Error {
    match &error {
        TarGzError::Codec(codec) if codec.is_resource_limit() => Error::coded(ErrorCode::ArchiveBomb, format!("tar.gz: {error}")),
        _ => Error::coded(ErrorCode::InvalidArchive, format!("tar.gz: {error}")),
    }
}

/// Decompresses a bzip2 or xz stream while the archive-wide budget is applied.
fn read_all_limited(mut reader: impl Read, limits: &ArchiveLimits, name: &str) -> Result<Vec<u8>> {
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut output = Vec::new();
    loop {
        let read = reader.read(&mut buffer).map_err(|error| Error::coded(ErrorCode::InvalidArchive, format!("`{name}`: {error}")))?;
        if read == 0 {
            return Ok(output);
        }
        output.extend_from_slice(&buffer[..read]);
        if output.len() as u64 > limits.max_total_bytes {
            return Err(Error::coded(
                ErrorCode::ArchiveBomb,
                format!("`{name}` expanded past the total limit of {} bytes", limits.max_total_bytes),
            ));
        }
    }
}

fn entry_kind(entry: &TarEntry) -> EntryKind {
    match entry.kind {
        TarEntryKind::Directory => EntryKind::Directory,
        TarEntryKind::Regular => EntryKind::File,
        TarEntryKind::Symlink => EntryKind::Symlink,
        TarEntryKind::HardLink => EntryKind::Hardlink,
        _ => EntryKind::Other,
    }
}

pub(crate) fn inspect(path: &Path, format: ArchiveFormat, limits: &ArchiveLimits, strict: bool) -> Result<Vec<PreparedEntry>> {
    let archive_bytes = fs::metadata(path).map_err(|error| Error::io(path.display().to_string(), error))?.len();
    let backing = load(path, format, limits)?;
    let platform = Platform::current();
    let mut prepared = Vec::new();
    let mut total_declared = 0_u64;
    let mut seen = HashSet::new();

    for (index, entry) in backing.entries().iter().enumerate() {
        limits.check_entry_count(index + 1)?;
        let name = decode_name(&entry.path);
        let kind = entry_kind(entry);
        let size = entry.size;
        total_declared = total_declared.saturating_add(if kind == EntryKind::Directory { 0 } else { size });
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

        prepared.push(PreparedEntry {
            index,
            name,
            kind,
            size,
            compressed: None,
            compression_method: Some("store".to_string()),
            modified_at_unix_ms: entry.modified_unix.checked_mul(1_000),
            safe,
            unsafe_reason,
        });
    }

    if !matches!(format, ArchiveFormat::Tar) {
        limits.check_global_ratio(total_declared, archive_bytes)?;
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
    let backing = load(path, format, &options.limits)?;
    let compressed = !matches!(format, ArchiveFormat::Tar);
    let mut total_bytes = 0_u64;

    for prepared in entries {
        let safe = prepared.safe_path()?;
        let entry = backing
            .entries()
            .get(prepared.index)
            .ok_or_else(|| Error::coded(ErrorCode::InvalidArchive, format!("entry `{}` disappeared between passes", prepared.name)))?;

        match prepared.kind {
            EntryKind::Directory => {
                if !options.dry_run {
                    let target = join_destination(&options.destination, safe);
                    fs::create_dir_all(&target).map_err(|error| Error::io(target.display().to_string(), error))?;
                }
                record_directory(report, &prepared.name)?;
            }
            EntryKind::File => {
                let target = decide_target(&options.destination, safe, options.overwrite)?;
                match &target {
                    None => {
                        report.skipped += 1;
                        report.entries.push(ExtractOutcome {
                            name: prepared.name.clone(),
                            action: ExtractAction::SkippedExists,
                            bytes_written: 0,
                            renamed_to: None,
                        });
                    }
                    Some(target) if options.dry_run => {
                        record_file(report, &prepared.name, entry.size, ExtractAction::WouldWrite, None, None, context);
                    }
                    Some(target) => {
                        let mut reader = backing.reader(entry)?;
                        if let Some(parent) = target.path.parent() {
                            fs::create_dir_all(parent).map_err(|error| Error::io(parent.display().to_string(), error))?;
                        }
                        let mut file = File::create(&target.path).map_err(|error| Error::io(target.path.display().to_string(), error))?;
                        let mut entry_bytes = 0_u64;
                        let bytes = copy_limited(&mut reader, &mut file, &prepared.name, &options.limits, &mut entry_bytes, &mut total_bytes)?;
                        drop(file);
                        if target.renamed {
                            report.renamed += 1;
                            record_file(
                                report,
                                &prepared.name,
                                bytes,
                                ExtractAction::Renamed,
                                Some(target.relative_name.clone()),
                                Some(target.path.clone()),
                                context,
                            );
                        }
                        else {
                            record_file(report, &prepared.name, bytes, ExtractAction::Written, None, Some(target.path.clone()), context);
                        }
                    }
                }
                if compressed {
                    options.limits.check_global_ratio(total_bytes, archive_bytes)?;
                }
            }
            EntryKind::Symlink | EntryKind::Hardlink | EntryKind::Other => {
                return Err(Error::coded(ErrorCode::SymlinkEntry, format!("entry `{}` has a type that is not extracted", prepared.name)));
            }
        }
    }

    Ok(())
}
