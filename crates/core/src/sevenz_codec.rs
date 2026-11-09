use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    fs::{self, File},
    path::Path,
};

use sevenz_rust2::{Archive, ArchiveEntry, ArchiveReader, EncoderMethod, Error as SevenZError, Password};

use crate::{
    error::{Error, ErrorCode, Result},
    extract::{ExtractContext, decide_target, record_directory},
    io_util::{copy_limited, drain_limited},
    limits::ArchiveLimits,
    model::{EntryKind, ExtractAction, ExtractOptions, ExtractOutcome, ExtractReport},
    platform::Platform,
    safety::{join_destination, validate_entry_path},
    zip_codec::PreparedEntry,
};

/// Header buffers and decoder dictionaries are bounded so a crafted archive
/// cannot make the decoder allocate without limit.
const MEMORY_LIMIT_KB: usize = 512 * 1024;

/// Milliseconds between the Windows file-time epoch (1601-01-01) and the Unix
/// epoch (1970-01-01).
const FILE_TIME_EPOCH_OFFSET_MS: i128 = 11_644_473_600_000;

fn open(path: &Path) -> Result<ArchiveReader<File>> {
    let file = File::open(path).map_err(|error| Error::io(path.display().to_string(), error))?;
    ArchiveReader::new_with_memory_limit(file, Password::empty(), MEMORY_LIMIT_KB).map_err(map_error)
}

fn map_error(error: SevenZError) -> Error {
    match error {
        SevenZError::PasswordRequired | SevenZError::MaybeBadPassword(_) => {
            Error::coded(ErrorCode::EncryptedArchive, "7z: the archive or an entry is encrypted and no password was supplied")
        }
        SevenZError::UnsupportedCompressionMethod(method) => {
            Error::coded(ErrorCode::UnsupportedCompression, format!("7z: compression method `{method}` is not supported"))
        }
        SevenZError::MaxMemLimited { max_kb, actaul_kb } => {
            Error::coded(ErrorCode::ArchiveBomb, format!("7z: the decoder needs {actaul_kb} KiB, above the {max_kb} KiB memory limit"))
        }
        SevenZError::Io(error, _) => Error::io("7z", error),
        SevenZError::FileOpen(error, _) => Error::io("7z", error),
        other => Error::coded(ErrorCode::InvalidArchive, format!("7z: {other}")),
    }
}

fn unsupported(method: &str) -> Error {
    Error::coded(ErrorCode::UnsupportedCompression, format!("7z: compression method `{method}` is not supported"))
}

/// Methods this build can decode. The optional features in `Cargo.toml` match
/// this list exactly, so anything else is rejected instead of half-decoded.
fn is_decodable(method: EncoderMethod) -> bool {
    matches!(
        method,
        EncoderMethod::COPY
            | EncoderMethod::LZMA
            | EncoderMethod::LZMA2
            | EncoderMethod::PPMD
            | EncoderMethod::BZIP2
            | EncoderMethod::DEFLATE
            | EncoderMethod::BCJ_X86_FILTER
            | EncoderMethod::BCJ_PPC_FILTER
            | EncoderMethod::BCJ_IA64_FILTER
            | EncoderMethod::BCJ_ARM_FILTER
            | EncoderMethod::BCJ_ARM64_FILTER
            | EncoderMethod::BCJ_ARM_THUMB_FILTER
            | EncoderMethod::BCJ_SPARC_FILTER
            | EncoderMethod::BCJ_RISCV_FILTER
            | EncoderMethod::DELTA_FILTER
            | EncoderMethod::BCJ2_FILTER
    )
}

/// Reject encrypted and unsupported coder chains before any entry is written.
fn check_methods(archive: &Archive) -> Result<()> {
    for block in &archive.blocks {
        for coder in &block.coders {
            let id = coder.encoder_method_id();
            if id == EncoderMethod::ID_AES256_SHA256 {
                return Err(Error::coded(ErrorCode::EncryptedArchive, "7z: the archive is encrypted and no password was supplied"));
            }
            match EncoderMethod::by_id(id) {
                Some(method) if is_decodable(method) => {}
                Some(method) => return Err(unsupported(method.name())),
                None => {
                    return Err(Error::coded(ErrorCode::UnsupportedCompression, format!("7z: unknown compression method id {id:02x?}")));
                }
            }
        }
    }
    Ok(())
}

fn is_symlink(entry: &ArchiveEntry) -> bool {
    // p7zip stores the POSIX mode in the high 16 bits of the Windows attribute
    // word. We never create links, so a pointed-out symlink entry is rejected.
    entry.windows_attributes >> 16 & 0o170000 == 0o120000
}

fn file_time_to_unix_ms(file_time: u64) -> i64 {
    let milliseconds = (file_time as i128) / 10_000 - FILE_TIME_EPOCH_OFFSET_MS;
    milliseconds.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

fn compression_label(reader: &ArchiveReader<File>, entry: &ArchiveEntry) -> Option<String> {
    if !entry.has_stream {
        return None;
    }
    let mut methods = Vec::new();
    let _ = reader.file_compression_methods(&entry.name, &mut methods);
    let label =
        methods.iter().filter(|method| !is_filter(**method)).map(|method| method.name().to_ascii_lowercase()).collect::<Vec<_>>().join("+");
    (!label.is_empty()).then_some(label)
}

fn is_filter(method: EncoderMethod) -> bool {
    matches!(
        method,
        EncoderMethod::BCJ_X86_FILTER
            | EncoderMethod::BCJ_PPC_FILTER
            | EncoderMethod::BCJ_IA64_FILTER
            | EncoderMethod::BCJ_ARM_FILTER
            | EncoderMethod::BCJ_ARM64_FILTER
            | EncoderMethod::BCJ_ARM_THUMB_FILTER
            | EncoderMethod::BCJ_SPARC_FILTER
            | EncoderMethod::BCJ_RISCV_FILTER
            | EncoderMethod::DELTA_FILTER
            | EncoderMethod::BCJ2_FILTER
    )
}

/// Read the 7z header and validate every entry before any write.
///
/// Encrypted archives and unsupported compression methods fail in both strict
/// and listing mode: the tool never claims to have read what it cannot decode.
pub(crate) fn inspect(path: &Path, limits: &ArchiveLimits, strict: bool) -> Result<Vec<PreparedEntry>> {
    let reader = open(path)?;
    let archive = reader.archive();
    limits.check_entry_count(archive.files.len())?;
    check_methods(archive)?;

    let platform = Platform::current();
    let mut seen = HashSet::new();
    let mut total = 0_u64;
    let mut prepared = Vec::with_capacity(archive.files.len());

    for (index, entry) in archive.files.iter().enumerate() {
        let name = entry.name.clone();
        let kind = if entry.is_directory {
            EntryKind::Directory
        }
        else if is_symlink(entry) {
            EntryKind::Symlink
        }
        else if entry.is_anti_item {
            EntryKind::Other
        }
        else {
            EntryKind::File
        };
        let size = entry.size;
        let compressed = entry.has_stream.then_some(entry.compressed_size);
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
                else if kind == EntryKind::Other {
                    let reason = format!("entry `{name}` is an anti-item, not a regular file or directory");
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

        let modified_at_unix_ms = entry.has_last_modified_date.then(|| file_time_to_unix_ms(u64::from(entry.last_modified_date)));
        prepared.push(PreparedEntry {
            index,
            name,
            kind,
            size,
            compressed,
            compression_method: compression_label(&reader, entry),
            modified_at_unix_ms,
            safe,
            unsafe_reason,
        });
    }

    Ok(prepared)
}

fn outcome(name: &str, action: ExtractAction, bytes_written: u64, renamed_to: Option<String>) -> ExtractOutcome {
    ExtractOutcome { name: name.to_string(), action, bytes_written, renamed_to }
}

pub(crate) fn extract(
    path: &Path,
    entries: &[PreparedEntry],
    options: &ExtractOptions,
    context: &mut ExtractContext,
    report: &mut ExtractReport,
) -> Result<()> {
    if options.dry_run {
        return extract_dry_run(entries, options, context, report);
    }

    let prepared: HashMap<&str, &PreparedEntry> = entries.iter().map(|entry| (entry.name.as_str(), entry)).collect();
    let mut outcomes: Vec<Option<ExtractOutcome>> = entries.iter().map(|_| None).collect();
    let mut total_bytes = 0_u64;
    let mut failure: Option<Error> = None;
    let mut reader = open(path)?;
    let result = reader.for_each_entries(|archive_entry, stream| {
        let Some(entry) = prepared.get(archive_entry.name())
        else {
            failure =
                Some(Error::coded(ErrorCode::InvalidArchive, format!("7z: decoder produced an unknown entry `{}`", archive_entry.name())));
            return Err(SevenZError::Other(Cow::Borrowed("open-zipper extraction aborted")));
        };
        match extract_one(entry, stream, options, context, &mut total_bytes) {
            Ok(extract_outcome) => {
                outcomes[entry.index] = Some(extract_outcome);
                Ok(true)
            }
            Err(error) => {
                failure = Some(error);
                Err(SevenZError::Other(Cow::Borrowed("open-zipper extraction aborted")))
            }
        }
    });

    match result {
        Ok(()) => {}
        Err(error) => return Err(failure.unwrap_or_else(|| map_error(error))),
    }
    let Some(filled) = outcomes.into_iter().collect::<Option<Vec<_>>>()
    else {
        return Err(Error::coded(ErrorCode::InvalidArchive, "7z: the decoder skipped one or more entries"));
    };

    for extract_outcome in &filled {
        match extract_outcome.action {
            ExtractAction::Written | ExtractAction::Renamed | ExtractAction::WouldWrite => report.written += 1,
            ExtractAction::Directory => report.directories += 1,
            ExtractAction::SkippedExists => report.skipped += 1,
        }
        if matches!(extract_outcome.action, ExtractAction::Renamed) {
            report.renamed += 1;
        }
        report.bytes_written = report.bytes_written.saturating_add(extract_outcome.bytes_written);
    }
    report.entries = filled;
    Ok(())
}

fn extract_dry_run(
    entries: &[PreparedEntry],
    options: &ExtractOptions,
    _context: &mut ExtractContext,
    report: &mut ExtractReport,
) -> Result<()> {
    for entry in entries {
        let safe = entry.safe_path()?;
        match entry.kind {
            EntryKind::Directory => {
                record_directory(report, &entry.name)?;
            }
            EntryKind::File => {
                let Some(target) = decide_target(&options.destination, safe, options.overwrite)?
                else {
                    report.skipped += 1;
                    report.entries.push(outcome(&entry.name, ExtractAction::SkippedExists, 0, None));
                    continue;
                };
                let renamed_to = target.renamed.then(|| target.relative_name.clone());
                report.written += 1;
                report.bytes_written = report.bytes_written.saturating_add(entry.size);
                report.entries.push(outcome(&entry.name, ExtractAction::WouldWrite, entry.size, renamed_to));
            }
            EntryKind::Symlink | EntryKind::Hardlink | EntryKind::Other => {
                return Err(Error::coded(ErrorCode::InvalidArchive, format!("entry `{}` cannot be extracted", entry.name)));
            }
        }
    }
    Ok(())
}

fn extract_one(
    entry: &PreparedEntry,
    stream: &mut dyn std::io::Read,
    options: &ExtractOptions,
    context: &mut ExtractContext,
    total_bytes: &mut u64,
) -> Result<ExtractOutcome> {
    let safe = entry.safe_path()?;
    match entry.kind {
        EntryKind::Directory => {
            let target = join_destination(&options.destination, safe);
            fs::create_dir_all(&target).map_err(|error| Error::io(target.display().to_string(), error))?;
            Ok(outcome(&entry.name, ExtractAction::Directory, 0, None))
        }
        EntryKind::File => {
            let Some(target) = decide_target(&options.destination, safe, options.overwrite)?
            else {
                return Ok(outcome(&entry.name, ExtractAction::SkippedExists, 0, None));
            };
            let renamed_to = target.renamed.then(|| target.relative_name.clone());
            if let Some(parent) = target.path.parent() {
                fs::create_dir_all(parent).map_err(|error| Error::io(parent.display().to_string(), error))?;
            }
            let mut file = File::create(&target.path).map_err(|error| Error::io(target.path.display().to_string(), error))?;
            let mut entry_bytes = 0_u64;
            let written = copy_limited(stream, &mut file, &entry.name, &options.limits, &mut entry_bytes, total_bytes)?;
            drop(file);
            context.written_files.push((entry.name.clone(), target.path));
            let action = if renamed_to.is_some() { ExtractAction::Renamed } else { ExtractAction::Written };
            Ok(outcome(&entry.name, action, written, renamed_to))
        }
        EntryKind::Symlink | EntryKind::Hardlink | EntryKind::Other => {
            Err(Error::coded(ErrorCode::InvalidArchive, format!("entry `{}` cannot be extracted", entry.name)))
        }
    }
}

/// Full read-through integrity check. The decoder verifies per-file CRCs while
/// draining, and our limits bound the decompressed output.
pub(crate) fn test(path: &Path, limits: &ArchiveLimits) -> Result<(usize, u64)> {
    inspect(path, limits, true)?;
    let archive_bytes = fs::metadata(path).map_err(|error| Error::io(path.display().to_string(), error))?.len();
    let mut reader = open(path)?;
    let entry_count = reader.archive().files.len();
    let mut total_bytes = 0_u64;
    let mut failure: Option<Error> = None;
    let result = reader.for_each_entries(|entry, stream| {
        if entry.is_directory || !entry.has_stream {
            return Ok(true);
        }
        let mut entry_bytes = 0_u64;
        match drain_limited(stream, &entry.name, limits, &mut entry_bytes, &mut total_bytes)
            .and_then(|_| limits.check_global_ratio(total_bytes, archive_bytes))
        {
            Ok(()) => Ok(true),
            Err(error) => {
                failure = Some(error);
                Err(SevenZError::Other(Cow::Borrowed("open-zipper integrity check aborted")))
            }
        }
    });

    match result {
        Ok(()) => {}
        Err(error) => return Err(failure.unwrap_or_else(|| map_error(error))),
    }
    Ok((entry_count, total_bytes))
}

/// Reads up to `cap` bytes of one entry (matched by name) without touching disk.
pub(crate) fn read_member(path: &Path, name: &str, cap: u64) -> Result<(Vec<u8>, bool)> {
    let mut reader = open(path)?;
    let mut found: Option<(Vec<u8>, bool)> = None;
    let mut failure: Option<Error> = None;
    let result = reader.for_each_entries(|entry, stream| {
        if entry.name() != name {
            return Ok(true);
        }
        match crate::io_util::read_bounded(stream, cap) {
            Ok(bytes) => {
                found = Some(bytes);
                Ok(false)
            }
            Err(error) => {
                failure = Some(error);
                Err(SevenZError::Other(Cow::Borrowed("open-zipper preview aborted")))
            }
        }
    });
    match result {
        Ok(()) => {}
        Err(error) => return Err(failure.unwrap_or_else(|| map_error(error))),
    }
    found.ok_or_else(|| Error::coded(ErrorCode::InvalidArchive, format!("7z: entry `{name}` disappeared between passes")))
}

/// Writes one entry to `dest` under the full archive limits, for external open.
pub(crate) fn write_member(path: &Path, name: &str, dest: &Path, limits: &ArchiveLimits) -> Result<u64> {
    let mut reader = open(path)?;
    let mut written: Option<u64> = None;
    let mut failure: Option<Error> = None;
    let result = reader.for_each_entries(|entry, stream| {
        if entry.name() != name {
            return Ok(true);
        }
        match File::create(dest) {
            Ok(mut file) => {
                let mut entry_bytes = 0_u64;
                let mut total_bytes = 0_u64;
                match copy_limited(stream, &mut file, name, limits, &mut entry_bytes, &mut total_bytes) {
                    Ok(count) => {
                        written = Some(count);
                        Ok(false)
                    }
                    Err(error) => {
                        failure = Some(error);
                        Err(SevenZError::Other(Cow::Borrowed("open-zipper extraction aborted")))
                    }
                }
            }
            Err(error) => {
                failure = Some(Error::io(dest.display().to_string(), error));
                Err(SevenZError::Other(Cow::Borrowed("open-zipper extraction aborted")))
            }
        }
    });
    match result {
        Ok(()) => {}
        Err(error) => return Err(failure.unwrap_or_else(|| map_error(error))),
    }
    written.ok_or_else(|| Error::coded(ErrorCode::InvalidArchive, format!("7z: entry `{name}` disappeared between passes")))
}
