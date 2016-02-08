use std::{
    fmt::{self, Display, Formatter},
    io,
};

use serde::Serialize;

pub type Result<T> = std::result::Result<T, Error>;

/// Capability-level failure codes. The vocabulary is fixed across the Open app family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityState {
    UnsupportedPlatform,
    NotImplemented,
    DependencyBlocked,
}

impl CapabilityState {
    pub fn code(self) -> &'static str {
        match self {
            Self::UnsupportedPlatform => "unsupported_platform",
            Self::NotImplemented => "not_implemented",
            Self::DependencyBlocked => "dependency_blocked",
        }
    }

    pub fn detail(self) -> &'static str {
        match self {
            Self::UnsupportedPlatform => "the capability is not available on this platform",
            Self::NotImplemented => "the capability is not implemented yet",
            Self::DependencyBlocked => "the capability is blocked by a missing dependency",
        }
    }
}

/// Domain-level error codes. They are stable for scripts and audit records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    ZipSlip,
    SymlinkEntry,
    ArchiveBomb,
    NestedDepth,
    PathConflict,
    InvalidArchive,
    UnsupportedFormat,
    OutputExists,
    InvalidOption,
    FileMissing,
    IoError,
    Message,
}

impl ErrorCode {
    pub fn code(self) -> &'static str {
        match self {
            Self::ZipSlip => "zip_slip",
            Self::SymlinkEntry => "symlink_entry",
            Self::ArchiveBomb => "archive_bomb",
            Self::NestedDepth => "nested_depth_exceeded",
            Self::PathConflict => "path_conflict",
            Self::InvalidArchive => "invalid_archive",
            Self::UnsupportedFormat => "unsupported_format",
            Self::OutputExists => "output_exists",
            Self::InvalidOption => "invalid_option",
            Self::FileMissing => "file_missing",
            Self::IoError => "io-error",
            Self::Message => "message",
        }
    }
}

#[derive(Debug)]
pub enum Error {
    Message(String),
    Coded { code: ErrorCode, message: String },
    Io { path: String, source: io::Error },
}

impl Error {
    pub fn message(text: impl Into<String>) -> Self {
        Self::Message(text.into())
    }

    pub fn coded(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Coded { code, message: message.into() }
    }

    pub fn io(path: impl Into<String>, source: io::Error) -> Self {
        Self::Io { path: path.into(), source }
    }

    pub fn capability(state: CapabilityState, context: impl Display) -> Self {
        Self::Message(format!("{}: {context}", state.code()))
    }

    /// Returns the capability code carried by this error, if any.
    pub fn capability_code(&self) -> Option<&'static str> {
        let text = match self {
            Self::Message(text) => text.as_str(),
            Self::Coded { code, message } => {
                if message.starts_with(code.code()) {
                    return Some(code.code());
                }
                message.as_str()
            }
            Self::Io { .. } => return None,
        };
        [CapabilityState::UnsupportedPlatform, CapabilityState::NotImplemented, CapabilityState::DependencyBlocked]
            .into_iter()
            .find(|state| text.starts_with(state.code()))
            .map(CapabilityState::code)
    }

    /// Stable code used in audit records and CLI output.
    pub fn code(&self) -> &'static str {
        if let Some(code) = self.capability_code() {
            return code;
        }
        match self {
            Self::Coded { code, .. } => code.code(),
            Self::Io { .. } => ErrorCode::IoError.code(),
            Self::Message(_) => ErrorCode::Message.code(),
        }
    }
}

impl Display for Error {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message(text) => f.write_str(text),
            Self::Coded { code, message } => write!(f, "{}: {message}", code.code()),
            Self::Io { path, source } => write!(f, "{path}: {source}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Message(_) | Self::Coded { .. } => None,
            Self::Io { source, .. } => Some(source),
        }
    }
}

impl From<zip::result::ZipError> for Error {
    fn from(error: zip::result::ZipError) -> Self {
        Self::coded(ErrorCode::InvalidArchive, format!("zip: {error}"))
    }
}

impl From<io::Error> for Error {
    fn from(source: io::Error) -> Self {
        Self::Io { path: "<io>".to_string(), source }
    }
}
