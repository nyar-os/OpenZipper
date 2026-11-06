use serde::Serialize;

use crate::error::CapabilityState;

pub const CAP_ARCHIVE_READ_ZIP: &str = "archive-read-zip";
pub const CAP_ARCHIVE_READ_TAR: &str = "archive-read-tar";
pub const CAP_ARCHIVE_READ_TAR_GZ: &str = "archive-read-tar-gz";
pub const CAP_ARCHIVE_READ_TAR_BZ2: &str = "archive-read-tar-bz2";
pub const CAP_ARCHIVE_READ_TAR_XZ: &str = "archive-read-tar-xz";
pub const CAP_ARCHIVE_WRITE_ZIP: &str = "archive-write-zip";
pub const CAP_ARCHIVE_WRITE_TAR: &str = "archive-write-tar";
pub const CAP_ARCHIVE_WRITE_TAR_GZ: &str = "archive-write-tar-gz";
pub const CAP_ARCHIVE_WRITE_TAR_BZ2: &str = "archive-write-tar-bz2";
pub const CAP_ARCHIVE_WRITE_TAR_XZ: &str = "archive-write-tar-xz";
pub const CAP_ARCHIVE_READ_7Z: &str = "archive-read-7z";
pub const CAP_ARCHIVE_READ_RAR: &str = "archive-read-rar";
pub const CAP_PATH_SAFETY: &str = "path-safety";
pub const CAP_BOMB_GUARD: &str = "archive-bomb-guard";
pub const CAP_ENCODING_GBK: &str = "archive-name-encoding";
pub const CAP_NTFS_METADATA: &str = "ntfs-metadata";
pub const CAP_GUI: &str = "gui";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Platform {
    Windows,
    Macos,
    Linux,
    Other,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        }
        else if cfg!(target_os = "macos") {
            Self::Macos
        }
        else if cfg!(target_os = "linux") {
            Self::Linux
        }
        else {
            Self::Other
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Macos => "macos",
            Self::Linux => "linux",
            Self::Other => "other",
        }
    }

    /// Whether the host's default volumes fold case. Windows does, and macOS
    /// defaults to a case-insensitive APFS/HFS+ volume. Linux is treated as
    /// case-sensitive. Duplicate entry keys follow this rule so a name pair
    /// cannot collapse into one file after extraction.
    pub fn case_insensitive_paths(self) -> bool {
        matches!(self, Self::Windows | Self::Macos)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityEntry {
    pub capability: &'static str,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<CapabilityState>,
    pub detail: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub platform: Platform,
    pub architecture: &'static str,
    pub entries: Vec<CapabilityEntry>,
    pub default_limits: crate::limits::ArchiveLimits,
}

fn available(capability: &'static str, detail: &'static str) -> CapabilityEntry {
    CapabilityEntry { capability, available: true, code: None, detail }
}

fn blocked(capability: &'static str, state: CapabilityState, detail: &'static str) -> CapabilityEntry {
    CapabilityEntry { capability, available: false, code: Some(state), detail }
}

/// Reports which archive capabilities are usable on the current platform.
///
/// Archive codecs are portable. The GUI host is provided by the WAE facade and
/// is only implemented for Windows; the NTFS metadata sink is not implemented
/// yet and therefore reports `unsupported_platform` off Windows.
pub fn capabilities() -> Capabilities {
    let windows = cfg!(windows);
    let ntfs = if windows {
        blocked(CAP_NTFS_METADATA, CapabilityState::NotImplemented, "preserving NTFS alternate data streams and ACLs is not implemented")
    }
    else {
        blocked(CAP_NTFS_METADATA, CapabilityState::UnsupportedPlatform, "NTFS alternate data streams and ACLs exist on Windows only")
    };

    let mut entries = vec![
        available(CAP_ARCHIVE_READ_ZIP, "zip entries are decoded in process, including UTF-8, GBK and CP437 names"),
        available(CAP_ARCHIVE_READ_TAR, "uncompressed tar streams are decoded in process"),
        available(CAP_ARCHIVE_READ_TAR_GZ, "gzip-wrapped tar streams are decoded in process with the Acorn tar.gz codec"),
        available(CAP_ARCHIVE_READ_TAR_BZ2, "bzip2-wrapped tar streams are decoded in process"),
        available(CAP_ARCHIVE_READ_TAR_XZ, "xz-wrapped tar streams are decoded in process"),
        available(CAP_ARCHIVE_WRITE_ZIP, "zip archives are written in process with deflate"),
        available(CAP_ARCHIVE_WRITE_TAR, "uncompressed tar archives are written in process"),
        available(CAP_ARCHIVE_WRITE_TAR_GZ, "gzip-wrapped tar archives are written in process"),
        available(CAP_ARCHIVE_WRITE_TAR_BZ2, "bzip2-wrapped tar archives are written in process"),
        available(CAP_ARCHIVE_WRITE_TAR_XZ, "xz-wrapped tar archives are written in process"),
        available(
            CAP_ARCHIVE_READ_7Z,
            "7z entries are decoded in process with sevenz-rust2 (Apache-2.0), covering LZMA, LZMA2, PPMD, BZip2 and Deflate",
        ),
        blocked(
            CAP_ARCHIVE_READ_RAR,
            CapabilityState::DependencyBlocked,
            "UnRAR source is custom freeware, not Apache-2.0 or MIT: it may not be used to build a RAR-compatible archiver or recreate the RAR algorithm, and the wrapper crates that claim MIT still vendor those sources",
        ),
        available(CAP_PATH_SAFETY, "zip slip, symlink, drive, UNC, device-name and Windows normalization checks run before any write"),
        available(CAP_BOMB_GUARD, "entry count, total size, entry size, compression ratio and nested depth limits abort extraction"),
        available(CAP_ENCODING_GBK, "zip entry names fall back to GBK and CP437 when raw bytes are not valid UTF-8"),
        ntfs,
    ];

    entries.push(if windows {
        available(CAP_GUI, "WebView2 GUI host is available on Windows")
    }
    else {
        blocked(CAP_GUI, CapabilityState::UnsupportedPlatform, "the GUI host is shipped by the WAE facade for Windows only")
    });

    Capabilities {
        platform: Platform::current(),
        architecture: architecture_name(),
        entries,
        default_limits: crate::limits::ArchiveLimits::default(),
    }
}

fn architecture_name() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "x86",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_vocabulary_uses_the_shared_codes() {
        assert_eq!(CapabilityState::UnsupportedPlatform.code(), "unsupported_platform");
        assert_eq!(CapabilityState::NotImplemented.code(), "not_implemented");
        assert_eq!(CapabilityState::DependencyBlocked.code(), "dependency_blocked");
    }

    #[test]
    fn archive_codecs_are_portable() {
        let report = capabilities();
        for capability in [
            CAP_ARCHIVE_READ_ZIP,
            CAP_ARCHIVE_READ_TAR,
            CAP_ARCHIVE_READ_TAR_GZ,
            CAP_ARCHIVE_READ_TAR_BZ2,
            CAP_ARCHIVE_READ_TAR_XZ,
            CAP_ARCHIVE_READ_7Z,
            CAP_ARCHIVE_WRITE_ZIP,
            CAP_ARCHIVE_WRITE_TAR,
            CAP_ARCHIVE_WRITE_TAR_GZ,
            CAP_ARCHIVE_WRITE_TAR_BZ2,
            CAP_ARCHIVE_WRITE_TAR_XZ,
            CAP_PATH_SAFETY,
            CAP_BOMB_GUARD,
            CAP_ENCODING_GBK,
        ] {
            let entry = report.entries.iter().find(|entry| entry.capability == capability).expect("capability entry");
            assert!(entry.available, "{capability} should be portable");
            assert!(entry.code.is_none());
        }
    }

    #[test]
    fn proprietary_formats_are_dependency_blocked() {
        let report = capabilities();
        let entry = report.entries.iter().find(|entry| entry.capability == CAP_ARCHIVE_READ_RAR).expect("entry");
        assert!(!entry.available);
        assert_eq!(entry.code, Some(CapabilityState::DependencyBlocked));
    }
}
