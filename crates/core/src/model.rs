use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{format::ArchiveFormat, limits::ArchiveLimits};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntryKind {
    File,
    Directory,
    Symlink,
    Hardlink,
    Other,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryInfo {
    pub index: usize,
    pub name: String,
    pub kind: EntryKind,
    pub size_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compressed_size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compression_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at_unix_ms: Option<i64>,
    pub safe: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unsafe_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveListing {
    pub path: String,
    pub format: ArchiveFormat,
    pub entries: Vec<EntryInfo>,
    pub entry_count: usize,
    pub total_size_bytes: u64,
    pub unsafe_entry_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OverwritePolicy {
    /// Fail when the destination exists. This is the default so nothing is replaced silently.
    Error,
    Skip,
    Rename,
}

impl OverwritePolicy {
    pub fn from_name(name: &str) -> crate::error::Result<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "error" => Ok(Self::Error),
            "skip" => Ok(Self::Skip),
            "rename" => Ok(Self::Rename),
            other => Err(crate::error::Error::coded(
                crate::error::ErrorCode::InvalidOption,
                format!("unknown overwrite policy `{other}` (expected error, skip or rename)"),
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExtractOptions {
    pub destination: PathBuf,
    pub overwrite: OverwritePolicy,
    pub dry_run: bool,
    pub recurse: bool,
    pub limits: ArchiveLimits,
}

impl ExtractOptions {
    pub fn new(destination: impl Into<PathBuf>) -> Self {
        Self {
            destination: destination.into(),
            overwrite: OverwritePolicy::Error,
            dry_run: false,
            recurse: false,
            limits: ArchiveLimits::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExtractAction {
    Written,
    WouldWrite,
    Directory,
    SkippedExists,
    Renamed,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractOutcome {
    pub name: String,
    pub action: ExtractAction,
    pub bytes_written: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub renamed_to: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractReport {
    pub archive: String,
    pub destination: String,
    pub format: ArchiveFormat,
    pub dry_run: bool,
    pub written: usize,
    pub skipped: usize,
    pub directories: usize,
    pub renamed: usize,
    pub bytes_written: u64,
    pub nested_archives: usize,
    pub entries: Vec<ExtractOutcome>,
}

#[derive(Debug, Clone, Default)]
pub struct CreateOptions {
    pub force: bool,
    pub limits: ArchiveLimits,
}

impl CreateOptions {
    pub fn new() -> Self {
        Self { force: false, limits: ArchiveLimits::default() }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateReport {
    pub archive: String,
    pub format: ArchiveFormat,
    pub entry_count: usize,
    pub source_bytes: u64,
    pub archive_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestReport {
    pub archive: String,
    pub format: ArchiveFormat,
    pub entry_count: usize,
    pub total_bytes: u64,
    pub ok: bool,
}
