//! OpenZipper archive engine.
//!
//! All archive parsing, validation, extraction and creation happens in this
//! crate. Hosts (N-API, CLI and GUI) only pass paths and options in and out.
//! Archive tools are never invoked as child processes and no shell pipeline is
//! used for archive work.

pub mod create;
pub mod encoding;
pub mod error;
pub mod extract;
pub mod extract_entry;
pub mod format;
pub mod limits;
pub mod list;
pub mod model;
pub mod platform;
pub mod preview;
pub mod recent;
pub mod safety;
pub mod source;
pub mod test;

mod fs_util;
mod io_util;
mod locations;
mod sevenz_codec;
mod tar_codec;
mod zip_codec;

pub use create::create;
pub use error::{CapabilityState, Error, ErrorCode, Result};
pub use extract::extract;
pub use extract_entry::{ExtractedEntryFile, extract_entry};
pub use format::ArchiveFormat;
pub use limits::ArchiveLimits;
pub use list::list;
pub use model::{
    ArchiveListing, CreateOptions, CreateReport, EntryInfo, EntryKind, ExtractAction, ExtractOptions, ExtractOutcome, ExtractReport,
    OverwritePolicy, TestReport,
};
pub use platform::{Capabilities, CapabilityEntry, Platform, capabilities};
pub use preview::{DEFAULT_PREVIEW_CAP, MAX_PREVIEW_CAP, PreviewKind, PreviewResult, preview};
pub use recent::{RECENT_LIMIT, RecentItem};
pub use source::FileSource;
pub use test::test;
