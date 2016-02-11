use std::{
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use bzip2::{Compression as BzCompression, write::BzEncoder};
use flate2::{Compression as GzCompression, write::GzEncoder};
use xz2::write::XzEncoder;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

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
    let file = File::create(temporary).map_err(|error| Error::io(path.clone(), error))?;
    match format {
        ArchiveFormat::Zip => write_zip(BufWriter::new(file), entries),
        ArchiveFormat::Tar => {
            let mut writer = write_tar(BufWriter::new(file), entries)?;
            writer.flush().map_err(|error| Error::io(path, error))
        }
        ArchiveFormat::TarGz => {
            let encoder = GzEncoder::new(file, GzCompression::default());
            let encoder = write_tar(encoder, entries)?;
            encoder.finish().map_err(|error| Error::io(path, error)).map(|_| ())
        }
        ArchiveFormat::TarBz2 => {
            let encoder = BzEncoder::new(file, BzCompression::default());
            let encoder = write_tar(encoder, entries)?;
            encoder.finish().map_err(|error| Error::io(path, error)).map(|_| ())
        }
        ArchiveFormat::TarXz => {
            let encoder = XzEncoder::new(file, 6);
            let encoder = write_tar(encoder, entries)?;
            encoder.finish().map_err(|error| Error::io(path, error)).map(|_| ())
        }
        ArchiveFormat::SevenZ => Err(Error::capability(CapabilityState::NotImplemented, "7z creation is not implemented")),
    }
}

fn write_zip(writer: BufWriter<File>, entries: &[SourceEntry]) -> Result<()> {
    let path = "zip output".to_string();
    let mut zip = ZipWriter::new(writer);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated).unix_permissions(0o644);

    for entry in entries {
        if entry.directory {
            zip.add_directory(format!("{}/", entry.name), options).map_err(Error::from)?;
        }
        else {
            zip.start_file(entry.name.clone(), options).map_err(Error::from)?;
            let mut source = File::open(&entry.path).map_err(|error| Error::io(entry.path.display().to_string(), error))?;
            std::io::copy(&mut source, &mut zip).map_err(|error| Error::io(entry.path.display().to_string(), error))?;
        }
    }

    let mut writer = zip.finish().map_err(Error::from)?;
    writer.flush().map_err(|error| Error::io(path, error))
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
