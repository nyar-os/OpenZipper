use std::{
    collections::HashSet,
    fs::{self, File},
    path::Path,
};

use acorn_core::{Budget, BudgetTracker};
use acorn_source::ByteSource;
use acorn_zip::{ZipIndexError, ZipMember, ZipViewError, decode_member, index_zip};

use crate::{
    encoding::decode_name,
    error::{Error, ErrorCode, Result},
    extract::{ExtractContext, decide_target, record_directory, record_file},
    io_util::copy_limited,
    limits::ArchiveLimits,
    model::{EntryKind, ExtractAction, ExtractOptions, ExtractOutcome, ExtractReport},
    platform::Platform,
    safety::{SafeEntryPath, join_destination, validate_entry_path},
    source::{FileSource, SourceReader},
};

/// Unix file-type mask in the upper half of ZIP external attributes.
const UNIX_FILE_TYPE_MASK: u32 = 0xf000;
/// Unix symbolic-link file type.
const UNIX_SYMLINK: u32 = 0xa000;
/// ZIP compression method for stored members.
const METHOD_STORED: u16 = 0;
/// ZIP compression method for raw Deflate members.
const METHOD_DEFLATE: u16 = 8;

#[derive(Debug, Clone)]
pub(crate) struct PreparedEntry {
    pub index: usize,
    pub name: String,
    pub kind: EntryKind,
    pub size: u64,
    pub compressed: Option<u64>,
    pub compression_method: Option<String>,
    pub modified_at_unix_ms: Option<i64>,
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

/// Opens the archive file and indexes its central directory without reading payloads.
fn open_index(path: &Path) -> Result<(FileSource, acorn_zip::ZipArchiveIndex)> {
    let source = FileSource::open(path)?;
    let index = index_zip(&source).map_err(map_index_error)?;
    Ok((source, index))
}

fn map_index_error(error: ZipIndexError) -> Error {
    Error::coded(ErrorCode::InvalidArchive, format!("zip: {error}"))
}

fn map_view_error(error: ZipViewError) -> Error {
    match &error {
        ZipViewError::UnsupportedCompression(method) => {
            Error::coded(ErrorCode::UnsupportedCompression, format!("zip: compression method `{method}` is not supported"))
        }
        ZipViewError::BudgetExceeded => Error::coded(ErrorCode::ArchiveBomb, format!("zip: {error}")),
        ZipViewError::Codec(codec) if codec.is_resource_limit() => Error::coded(ErrorCode::ArchiveBomb, format!("zip: {error}")),
        _ => Error::coded(ErrorCode::InvalidArchive, format!("zip: {error}")),
    }
}

/// One member gets its own ledger, because the decoded bytes are released as soon
/// as the entry is written and the archive-wide allowance is enforced separately.
fn member_budget(limits: &ArchiveLimits) -> Budget {
    Budget {
        max_read_bytes: limits.max_entry_bytes,
        max_decoded_bytes: limits.max_entry_bytes,
        max_cache_bytes: limits.max_entry_bytes,
        max_container_members: limits.max_entries as u64,
        ..Budget::default()
    }
}

fn member_kind(member: &ZipMember) -> EntryKind {
    if member.is_directory() {
        EntryKind::Directory
    }
    else if member.unix_mode().is_some_and(|mode| mode & UNIX_FILE_TYPE_MASK == UNIX_SYMLINK) {
        EntryKind::Symlink
    }
    else {
        EntryKind::File
    }
}

/// Rejects encrypted members before any entry is written.
fn check_encrypted(name: &str, member: &ZipMember) -> Result<()> {
    if !member.is_encrypted() {
        return Ok(());
    }
    let masked = if member.has_masked_data() { " with masked local sizes" } else { "" };
    Err(Error::coded(ErrorCode::EncryptedArchive, format!("zip: entry `{name}` is encrypted{masked} and no password was supplied")))
}

/// Rejects methods this build cannot decode before any entry is written.
fn check_compression(name: &str, member: &ZipMember) -> Result<()> {
    match member.compression_method {
        METHOD_STORED | METHOD_DEFLATE => Ok(()),
        other => Err(Error::coded(
            ErrorCode::UnsupportedCompression,
            format!("zip: entry `{name}` uses compression method `{other}`, which is not supported"),
        )),
    }
}

fn compression_label(method: u16) -> Option<String> {
    match method {
        METHOD_STORED => Some("store".to_string()),
        METHOD_DEFLATE => Some("deflate".to_string()),
        _ => None,
    }
}

/// Read the central directory and validate every entry before any write.
pub(crate) fn inspect(path: &Path, limits: &ArchiveLimits, strict: bool) -> Result<Vec<PreparedEntry>> {
    let source = FileSource::open(path)?;
    let index = index_zip(&source).map_err(map_index_error)?;
    drop(source);
    limits.check_entry_count(index.members.len())?;
    let platform = Platform::current();
    let mut prepared = Vec::with_capacity(index.members.len());
    let mut total = 0_u64;
    let mut seen = HashSet::new();

    for (position, member) in index.members.iter().enumerate() {
        // The UTF-8 hint in `name_encoding()` is advisory because writers apply it
        // inconsistently, so the fallback order stays UTF-8, then GBK, then CP437.
        let name = decode_name(&member.raw_name);
        check_encrypted(&name, member)?;
        check_compression(&name, member)?;
        let kind = member_kind(member);
        let size = member.uncompressed_size;
        let compressed = Some(member.compressed_size);
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

        prepared.push(PreparedEntry {
            index: position,
            name,
            kind,
            size,
            compressed,
            compression_method: compression_label(member.compression_method),
            // The DOS field is local wall-clock time with no zone, so only the
            // extended Unix timestamp is reported as an absolute instant.
            modified_at_unix_ms: member.modified_unix.and_then(|seconds| seconds.checked_mul(1_000)),
            safe,
            unsafe_reason,
        });
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
    let (source, index) = open_index(path)?;
    let mut total_bytes = 0_u64;

    for entry in entries {
        let safe = entry.safe_path()?;
        let member = index
            .members
            .get(entry.index)
            .ok_or_else(|| Error::coded(ErrorCode::InvalidArchive, format!("entry `{}` disappeared between passes", entry.name)))?;

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
                        report.entries.push(ExtractOutcome {
                            name: entry.name.clone(),
                            action: ExtractAction::SkippedExists,
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
                    record_file(report, &entry.name, entry.size, ExtractAction::WouldWrite, renamed_to, None, context);
                    continue;
                }

                let mut ledger = BudgetTracker::new(member_budget(&options.limits));
                let decoded = decode_member(entry.name.clone(), &source, member, &mut ledger).map_err(map_view_error)?;
                let length = decoded.length().unwrap_or(0);
                let mut reader = SourceReader::new(decoded, 0, length);

                if let Some(parent) = target.path.parent() {
                    fs::create_dir_all(parent).map_err(|error| Error::io(parent.display().to_string(), error))?;
                }
                let mut file = File::create(&target.path).map_err(|error| Error::io(target.path.display().to_string(), error))?;
                let mut entry_bytes = 0_u64;
                let written = copy_limited(&mut reader, &mut file, &entry.name, &options.limits, &mut entry_bytes, &mut total_bytes)?;
                drop(file);

                let action = if renamed_to.is_some() {
                    report.renamed += 1;
                    ExtractAction::Renamed
                }
                else {
                    ExtractAction::Written
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

/// Reads every file member to the end so CRC and declared sizes are verified.
pub(crate) fn verify(path: &Path, limits: &ArchiveLimits) -> Result<(usize, u64)> {
    let (source, index) = open_index(path)?;
    limits.check_entry_count(index.members.len())?;
    let mut checked = 0_usize;
    let mut total_bytes = 0_u64;

    for member in &index.members {
        let name = decode_name(&member.raw_name);
        check_encrypted(&name, member)?;
        check_compression(&name, member)?;
        if member_kind(member) != EntryKind::File {
            continue;
        }
        let mut ledger = BudgetTracker::new(member_budget(limits));
        let decoded = decode_member(name.clone(), &source, member, &mut ledger).map_err(map_view_error)?;
        let length = decoded.length().unwrap_or(0);
        total_bytes = total_bytes.saturating_add(length);
        limits.check_written_bytes(&name, length, total_bytes)?;
        checked += 1;
    }

    Ok((checked, total_bytes))
}
