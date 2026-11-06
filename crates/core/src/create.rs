use std::{
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use acorn_zip::{ZipDateTime, ZipDosDateTime, ZipEntry, ZipWriter};
use bzip2::{Compression as BzCompression, write::BzEncoder};
use flate2::{Compression as GzCompression, write::GzEncoder};
use xz2::write::XzEncoder;

use crate::{
    error::{CapabilityState, Error, ErrorCode, Result},
    format::{self, ArchiveFormat},
    fs_util::{SourceEntry, collect_sources},
    model::{CreateOptions, CreateReport},
};

pub fn create(sources: &[PathBuf], destination: &Path, format_name: Option<&str>, options: &CreateOptions) -> Result<CreateReport> {
    options.limits.validate()?;
    if sources.is_empty() {
        return Err(Error::coded(ErrorCode::InvalidOption, "at least one source path is required"));
    }

    let format = match format_name {
        Some(name) => ArchiveFormat::from_name(name)?,
        None => format::detect_by_name(&destination.to_string_lossy()).ok_or_else(|| {
            Error::coded(ErrorCode::UnsupportedFormat, format!("cannot infer the archive format from `{}`", destination.display()))
        })?,
    };

    destination_is_directory(destination)?;
    if fs::symlink_metadata(destination).is_ok() {
        if options.force {
            fs::remove_file(destination).map_err(|error| Error::io(destination.display().to_string(), error))?;
        }
        else {
            return Err(Error::coded(ErrorCode::OutputExists, format!("`{}` already exists and `force` is not set", destination.display())));
        }
    }

    let entries = collect_sources(sources)?;
    options.limits.check_entry_count(entries.len())?;
    let declared: u64 = entries.iter().map(|entry| entry.size).sum();
    if declared > options.limits.max_total_bytes {
        return Err(Error::coded(
            ErrorCode::ArchiveBomb,
            format!("sources total {declared} bytes, above the archive limit of {}", options.limits.max_total_bytes),
        ));
    }

    let temporary = PathBuf::from(format!("{}.open-zipper.tmp", destination.display()));
    if fs::symlink_metadata(&temporary).is_ok() {
        fs::remove_file(&temporary).map_err(|error| Error::io(temporary.display().to_string(), error))?;
    }

    let write_result = write_archive(&temporary, format, &entries);
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }

    if fs::symlink_metadata(destination).is_ok() {
        fs::remove_file(destination).map_err(|error| Error::io(destination.display().to_string(), error))?;
    }
    fs::rename(&temporary, destination).map_err(|error| Error::io(destination.display().to_string(), error))?;

    let archive_bytes = fs::metadata(destination).map_err(|error| Error::io(destination.display().to_string(), error))?.len();
    Ok(CreateReport {
        archive: destination.display().to_string(),
        format,
        entry_count: entries.len(),
        source_bytes: entries.iter().map(|entry| entry.size).sum(),
        archive_bytes,
    })
}

fn destination_is_directory(destination: &Path) -> Result<()> {
    if let Ok(metadata) = fs::metadata(destination)
        && metadata.is_dir()
    {
        return Err(Error::coded(
            ErrorCode::OutputExists,
            format!("`{}` is a directory and cannot be replaced by an archive", destination.display()),
        ));
    }
    Ok(())
}

fn write_archive(temporary: &Path, format: ArchiveFormat, entries: &[SourceEntry]) -> Result<()> {
    let path = temporary.display().to_string();
    match format {
        ArchiveFormat::Zip => {
            let bytes = build_zip(entries)?;
            fs::write(temporary, bytes).map_err(|error| Error::io(path, error))
        }
        ArchiveFormat::Tar => {
            let file = File::create(temporary).map_err(|error| Error::io(path.clone(), error))?;
            let mut writer = write_tar(BufWriter::new(file), entries)?;
            writer.flush().map_err(|error| Error::io(path, error))
        }
        ArchiveFormat::TarGz => {
            let file = File::create(temporary).map_err(|error| Error::io(path.clone(), error))?;
            let encoder = GzEncoder::new(file, GzCompression::default());
            let encoder = write_tar(encoder, entries)?;
            encoder.finish().map_err(|error| Error::io(path, error)).map(|_| ())
        }
        ArchiveFormat::TarBz2 => {
            let file = File::create(temporary).map_err(|error| Error::io(path.clone(), error))?;
            let encoder = BzEncoder::new(file, BzCompression::default());
            let encoder = write_tar(encoder, entries)?;
            encoder.finish().map_err(|error| Error::io(path, error)).map(|_| ())
        }
        ArchiveFormat::TarXz => {
            let file = File::create(temporary).map_err(|error| Error::io(path.clone(), error))?;
            let encoder = XzEncoder::new(file, 6);
            let encoder = write_tar(encoder, entries)?;
            encoder.finish().map_err(|error| Error::io(path, error)).map(|_| ())
        }
        ArchiveFormat::SevenZ => Err(Error::capability(CapabilityState::NotImplemented, "7z creation is not implemented")),
    }
}

fn build_zip(entries: &[SourceEntry]) -> Result<Vec<u8>> {
    let modified_unix = now_unix_seconds();
    let modified = dos_from_unix(modified_unix);
    let mut writer = ZipWriter::new();

    for entry in entries {
        if entry.directory {
            let mut member = ZipEntry::directory(format!("{}/", entry.name), modified);
            member.modified_unix = Some(modified_unix);
            writer.push(member);
        }
        else {
            let data = fs::read(&entry.path).map_err(|error| Error::io(entry.path.display().to_string(), error))?;
            let mut member = ZipEntry::deflated(entry.name.as_bytes(), data);
            member.modified = modified;
            member.modified_unix = Some(modified_unix);
            member.version_made_by = UNIX_HOST_VERSION;
            member.external_attributes = UNIX_FILE_MODE;
            writer.push(member);
        }
    }

    writer.into_bytes().map_err(|error| Error::coded(ErrorCode::InvalidArchive, format!("zip: {error}")))
}

/// Unix host identifier with the ZIP 2.0 version, so `unix_mode()` is meaningful.
const UNIX_HOST_VERSION: u16 = (3 << 8) | 20;
/// Regular file with `rw-r--r--` permissions in the Unix attributes high word.
const UNIX_FILE_MODE: u32 = 0o100644 << 16;

fn now_unix_seconds() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|duration| duration.as_secs() as i64).unwrap_or(0)
}

/// Converts Unix seconds to both time fields the writer can emit.
fn dos_from_unix(seconds: i64) -> ZipDosDateTime {
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let Ok(year) = u16::try_from(year)
    else {
        return ZipDosDateTime::ZERO;
    };
    let hour = (day_seconds / 3_600) as u8;
    let minute = ((day_seconds % 3_600) / 60) as u8;
    let second = ((day_seconds % 60) / 2 * 2) as u8;
    ZipDateTime::new(year, month, day, hour, minute, second).and_then(|value| value.to_dos()).unwrap_or(ZipDosDateTime::ZERO)
}

/// Proleptic Gregorian date from a day count relative to 1970-01-01 (UTC).
fn civil_from_days(days: i64) -> (i64, u8, u8) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 { shifted } else { shifted - 146_096 } / 146_097;
    let day_of_era = (shifted - era * 146_097) as u64;
    let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_position = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_position + 2) / 5 + 1;
    let month = if month_position < 10 { month_position + 3 } else { month_position - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month as u8, day as u8)
}

fn write_tar<W: Write>(writer: W, entries: &[SourceEntry]) -> Result<W> {
    let mut builder = tar::Builder::new(writer);
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|duration| duration.as_secs()).unwrap_or(0);

    for entry in entries {
        if entry.directory {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Directory);
            header.set_mode(0o755);
            header.set_size(0);
            header.set_mtime(timestamp);
            header.set_cksum();
            builder
                .append_data(&mut header, format!("{}/", entry.name), std::io::empty())
                .map_err(|error| Error::io(entry.path.display().to_string(), error))?;
        }
        else {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Regular);
            header.set_mode(0o644);
            header.set_size(entry.size);
            header.set_mtime(timestamp);
            header.set_cksum();
            let mut source = File::open(&entry.path).map_err(|error| Error::io(entry.path.display().to_string(), error))?;
            builder.append_data(&mut header, &entry.name, &mut source).map_err(|error| Error::io(entry.path.display().to_string(), error))?;
        }
    }

    builder.into_inner().map_err(Error::from)
}
