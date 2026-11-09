use std::{
    collections::HashSet,
    fs::{self, File},
    io::{BufReader, Read, Write},
    path::Path,
};

use acorn_codec::CodecLimits;
use acorn_tar::{StreamChunk, TarArchive, TarEntry, TarEntryKind, TarGzEntry, TarGzError, TarGzStream, TarIndexError, index_tar};
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

/// Chunk size for streaming a `tar.gz` member to disk.
const TAR_STREAM_CHUNK: usize = 64 * 1024;

/// Where a seekable tar byte space lives.
enum Backing {
    /// Plain tar read directly from disk, entry payloads stream through ranges.
    File { source: FileSource, archive: TarArchive },
    /// A decoded bzip2 or xz tar held in memory.
    Memory { source: SliceSource, archive: TarArchive },
}

impl Backing {
    fn entries(&self) -> &[TarEntry] {
        match self {
            Self::File { archive, .. } | Self::Memory { archive, .. } => &archive.entries,
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
        }
    }
}

/// Loads the seekable variants. `tar.gz` is streamed and never lands here.
fn load(path: &Path, format: ArchiveFormat, limits: &ArchiveLimits) -> Result<Backing> {
    let label = path.display().to_string();
    match format {
        ArchiveFormat::Tar => {
            let source = FileSource::open(path)?;
            let archive = index_tar(&source).map_err(map_index_error)?;
            Ok(Backing::File { source, archive })
        }
        ArchiveFormat::TarBz2 | ArchiveFormat::TarXz => {
            let file = File::open(path).map_err(|error| Error::io(label.clone(), error))?;
            let reader: Box<dyn Read> = match format {
                ArchiveFormat::TarBz2 => Box::new(BzDecoder::new(BufReader::new(file))),
                _ => Box::new(XzDecoder::new(BufReader::new(file))),
            };
            let bytes = read_all(reader, limits, label.clone())?;
            let source = SliceSource::new(label, bytes);
            let archive = index_tar(&source).map_err(map_index_error)?;
            Ok(Backing::Memory { source, archive })
        }
        ArchiveFormat::TarGz | ArchiveFormat::Zip | ArchiveFormat::SevenZ => {
            Err(Error::coded(ErrorCode::InvalidOption, format!("`{}` is not a seekable tar stream", format.label())))
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

/// Decoded tar bytes stay bounded by the payload budget plus one header block
/// per entry, which matches the allowance the in-memory path applied.
fn gzip_limits(limits: &ArchiveLimits) -> CodecLimits {
    let decoded_limit = limits.max_total_bytes.saturating_add((limits.max_entries as u64).saturating_mul(1_024));
    CodecLimits::new().with_max_output_bytes(decoded_limit)
}

/// Decompresses a bzip2 or xz stream while the archive-wide budget is applied.
///
/// Acorn has no streaming bzip2 or xz decoder, so these two formats still
/// materialize the decoded tar and stop as soon as the total cap is crossed.
fn read_all(mut reader: impl Read, limits: &ArchiveLimits, name: String) -> Result<Vec<u8>> {
    let mut buffer = vec![0_u8; TAR_STREAM_CHUNK];
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

/// Applies the shared entry policy to one tar member.
#[allow(clippy::too_many_arguments)]
fn prepare_entry(
    index: usize,
    name: String,
    kind: EntryKind,
    size: u64,
    modified_unix: i64,
    limits: &ArchiveLimits,
    platform: Platform,
    strict: bool,
    seen: &mut HashSet<String>,
    total_declared: &mut u64,
) -> Result<PreparedEntry> {
    *total_declared = total_declared.saturating_add(if kind == EntryKind::Directory { 0 } else { size });
    limits.check_declared_entry(&name, size, None, *total_declared)?;

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

    Ok(PreparedEntry {
        index,
        name,
        kind,
        size,
        compressed: None,
        compression_method: Some("store".to_string()),
        modified_at_unix_ms: modified_unix.checked_mul(1_000),
        safe,
        unsafe_reason,
    })
}

pub(crate) fn inspect(path: &Path, format: ArchiveFormat, limits: &ArchiveLimits, strict: bool) -> Result<Vec<PreparedEntry>> {
    let archive_bytes = fs::metadata(path).map_err(|error| Error::io(path.display().to_string(), error))?.len();
    if format == ArchiveFormat::TarGz {
        return inspect_targz(path, limits, strict, archive_bytes);
    }

    let backing = load(path, format, limits)?;
    let platform = Platform::current();
    let mut prepared = Vec::new();
    let mut total_declared = 0_u64;
    let mut seen = HashSet::new();

    for (index, entry) in backing.entries().iter().enumerate() {
        limits.check_entry_count(index + 1)?;
        let name = decode_name(&entry.path);
        let prepared_entry = prepare_entry(
            index,
            name,
            entry_kind(entry),
            entry.size,
            entry.modified_unix,
            limits,
            platform,
            strict,
            &mut seen,
            &mut total_declared,
        )?;
        prepared.push(prepared_entry);
    }

    if !matches!(format, ArchiveFormat::Tar) {
        limits.check_global_ratio(total_declared, archive_bytes)?;
    }

    Ok(prepared)
}

/// Reads `tar.gz` headers through the streaming decoder without materializing
/// the decoded tar image. Dropping each entry makes the stream skip its payload.
fn inspect_targz(path: &Path, limits: &ArchiveLimits, strict: bool, archive_bytes: u64) -> Result<Vec<PreparedEntry>> {
    let source = FileSource::open(path)?;
    let mut stream = TarGzStream::open(&source, gzip_limits(limits)).map_err(map_targz_error)?;
    let platform = Platform::current();
    let mut prepared = Vec::new();
    let mut total_declared = 0_u64;
    let mut seen = HashSet::new();
    let mut index = 0_usize;

    while let Some(entry) = stream.next_entry().map_err(map_targz_error)? {
        limits.check_entry_count(index + 1)?;
        let entry = entry.entry();
        let name = decode_name(&entry.path);
        let prepared_entry = prepare_entry(
            index,
            name,
            entry_kind(entry),
            entry.size,
            entry.modified_unix,
            limits,
            platform,
            strict,
            &mut seen,
            &mut total_declared,
        )?;
        prepared.push(prepared_entry);
        index += 1;
    }

    limits.check_global_ratio(total_declared, archive_bytes)?;
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
    if format == ArchiveFormat::TarGz {
        return extract_targz(path, entries, archive_bytes, options, context, report);
    }

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

/// Extracts a `tar.gz` archive member by member over the streaming decoder.
///
/// The stream is sequential, so members are consumed in archive order and any
/// gap before the requested index is skipped, which keeps peak memory at the
/// decoder's read-ahead instead of the decoded tar size.
fn extract_targz(
    path: &Path,
    entries: &[PreparedEntry],
    archive_bytes: u64,
    options: &ExtractOptions,
    context: &mut ExtractContext,
    report: &mut ExtractReport,
) -> Result<()> {
    let source = FileSource::open(path)?;
    let mut stream = TarGzStream::open(&source, gzip_limits(&options.limits)).map_err(map_targz_error)?;
    let mut total_bytes = 0_u64;
    let mut next_index = 0_usize;

    for prepared in entries {
        let safe = prepared.safe_path()?;
        let mut entry = loop {
            let Some(candidate) = stream.next_entry().map_err(map_targz_error)?
            else {
                return Err(Error::coded(ErrorCode::InvalidArchive, format!("entry `{}` disappeared between passes", prepared.name)));
            };
            if next_index == prepared.index {
                break candidate;
            }
            next_index += 1;
        };
        next_index += 1;

        match prepared.kind {
            EntryKind::Directory => {
                if !options.dry_run {
                    let target = join_destination(&options.destination, safe);
                    fs::create_dir_all(&target).map_err(|error| Error::io(target.display().to_string(), error))?;
                }
                record_directory(report, &prepared.name)?;
            }
            EntryKind::File => {
                let declared = entry.entry().size;
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
                        record_file(report, &prepared.name, declared, ExtractAction::WouldWrite, None, None, context);
                    }
                    Some(target) => {
                        if let Some(parent) = target.path.parent() {
                            fs::create_dir_all(parent).map_err(|error| Error::io(parent.display().to_string(), error))?;
                        }
                        let mut file = File::create(&target.path).map_err(|error| Error::io(target.path.display().to_string(), error))?;
                        let written = match stream_targz_entry(&mut entry, &prepared.name, &options.limits, &mut total_bytes, &mut file) {
                            Ok(written) => written,
                            Err(error) => {
                                // The member stopped mid-payload, so nothing may
                                // remain of the partial file.
                                drop(file);
                                let _ = fs::remove_file(&target.path);
                                return Err(error);
                            }
                        };
                        drop(file);
                        if target.renamed {
                            report.renamed += 1;
                            record_file(
                                report,
                                &prepared.name,
                                written,
                                ExtractAction::Renamed,
                                Some(target.relative_name.clone()),
                                Some(target.path.clone()),
                                context,
                            );
                        }
                        else {
                            record_file(report, &prepared.name, written, ExtractAction::Written, None, Some(target.path.clone()), context);
                        }
                    }
                }
                options.limits.check_global_ratio(total_bytes, archive_bytes)?;
            }
            EntryKind::Symlink | EntryKind::Hardlink | EntryKind::Other => {
                return Err(Error::coded(ErrorCode::SymlinkEntry, format!("entry `{}` has a type that is not extracted", prepared.name)));
            }
        }
    }

    Ok(())
}

/// Copies one `tar.gz` member into `writer` while the byte caps are enforced.
fn stream_targz_entry(
    entry: &mut TarGzEntry<'_, &FileSource>,
    name: &str,
    limits: &ArchiveLimits,
    total_bytes: &mut u64,
    writer: &mut impl Write,
) -> Result<u64> {
    let mut buffer = vec![0_u8; TAR_STREAM_CHUNK];
    loop {
        match entry.read(&mut buffer).map_err(map_targz_error)? {
            StreamChunk::Finished => break,
            StreamChunk::Produced(0) | StreamChunk::NeedInput => continue,
            StreamChunk::Produced(count) => {
                *total_bytes = total_bytes.saturating_add(count as u64);
                limits.check_written_bytes(name, entry.read_bytes(), *total_bytes)?;
                writer.write_all(&buffer[..count]).map_err(|error| Error::io(name, error))?;
            }
        }
    }
    Ok(entry.read_bytes())
}

/// Reads up to `cap` bytes of one entry without touching disk.
///
/// Seekable tars read through the entry data range; `tar.gz` walks the stream
/// to the requested index because gzip gives no random access.
pub(crate) fn read_member(path: &Path, format: ArchiveFormat, index: usize, cap: u64, limits: &ArchiveLimits) -> Result<(Vec<u8>, bool)> {
    if format == ArchiveFormat::TarGz {
        return read_member_targz(path, index, cap, limits);
    }
    let backing = load(path, format, limits)?;
    let entry = backing
        .entries()
        .get(index)
        .ok_or_else(|| Error::coded(ErrorCode::InvalidArchive, format!("tar: entry index {index} is out of range")))?;
    let mut reader = backing.reader(entry)?;
    crate::io_util::read_bounded(&mut reader, cap)
}

fn read_member_targz(path: &Path, index: usize, cap: u64, limits: &ArchiveLimits) -> Result<(Vec<u8>, bool)> {
    let source = FileSource::open(path)?;
    let mut stream = TarGzStream::open(&source, gzip_limits(limits)).map_err(map_targz_error)?;
    let mut next_index = 0_usize;
    while let Some(mut candidate) = stream.next_entry().map_err(map_targz_error)? {
        if next_index == index {
            let mut buffer = vec![0_u8; TAR_STREAM_CHUNK];
            let mut output = Vec::new();
            let mut truncated = false;
            loop {
                match candidate.read(&mut buffer).map_err(map_targz_error)? {
                    StreamChunk::Finished => break,
                    StreamChunk::Produced(0) | StreamChunk::NeedInput => continue,
                    StreamChunk::Produced(count) => {
                        let room = cap.saturating_sub(output.len() as u64) as usize;
                        if count > room {
                            output.extend_from_slice(&buffer[..room]);
                            truncated = true;
                            break;
                        }
                        output.extend_from_slice(&buffer[..count]);
                    }
                }
            }
            return Ok((output, truncated));
        }
        next_index += 1;
    }
    Err(Error::coded(ErrorCode::InvalidArchive, format!("tar.gz: entry index {index} is out of range")))
}

/// Writes one entry to `dest` under the full archive limits, for external open.
pub(crate) fn write_member(path: &Path, format: ArchiveFormat, index: usize, dest: &Path, limits: &ArchiveLimits) -> Result<u64> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|error| Error::io(parent.display().to_string(), error))?;
    }
    let mut file = File::create(dest).map_err(|error| Error::io(dest.display().to_string(), error))?;
    if format == ArchiveFormat::TarGz {
        let source = FileSource::open(path)?;
        let mut stream = TarGzStream::open(&source, gzip_limits(limits)).map_err(map_targz_error)?;
        let mut next_index = 0_usize;
        let mut total_bytes = 0_u64;
        while let Some(mut candidate) = stream.next_entry().map_err(map_targz_error)? {
            if next_index == index {
                return stream_targz_entry(&mut candidate, &dest.display().to_string(), limits, &mut total_bytes, &mut file);
            }
            next_index += 1;
        }
        return Err(Error::coded(ErrorCode::InvalidArchive, format!("tar.gz: entry index {index} is out of range")));
    }

    let backing = load(path, format, limits)?;
    let entry = backing
        .entries()
        .get(index)
        .ok_or_else(|| Error::coded(ErrorCode::InvalidArchive, format!("tar: entry index {index} is out of range")))?;
    let mut reader = backing.reader(entry)?;
    let mut entry_bytes = 0_u64;
    let mut total_bytes = 0_u64;
    copy_limited(&mut reader, &mut file, &dest.display().to_string(), limits, &mut entry_bytes, &mut total_bytes)
}
