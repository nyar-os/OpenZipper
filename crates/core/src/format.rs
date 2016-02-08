use std::{fs::File, io::Read, path::Path};

use serde::{Deserialize, Serialize};

use crate::error::{CapabilityState, Error, ErrorCode, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArchiveFormat {
    Zip,
    Tar,
    TarGz,
    TarBz2,
    TarXz,
}

impl ArchiveFormat {
    pub fn from_name(name: &str) -> Result<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "zip" => Ok(Self::Zip),
            "tar" => Ok(Self::Tar),
            "tar.gz" | "tgz" | "targz" => Ok(Self::TarGz),
            "tar.bz2" | "tbz2" | "tbz" | "tarbz2" => Ok(Self::TarBz2),
            "tar.xz" | "txz" | "tarxz" => Ok(Self::TarXz),
            "7z" | "sevenz" | "rar" => {
                Err(Error::capability(CapabilityState::DependencyBlocked, format!("`{name}` has no vendored codec with an acceptable license")))
            }
            other => Err(Error::coded(ErrorCode::UnsupportedFormat, format!("unsupported archive format `{other}`"))),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::Tar => "tar",
            Self::TarGz => "tar.gz",
            Self::TarBz2 => "tar.bz2",
            Self::TarXz => "tar.xz",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::Tar => "tar",
            Self::TarGz => "tar.gz",
            Self::TarBz2 => "tar.bz2",
            Self::TarXz => "tar.xz",
        }
    }

    pub fn is_tar(self) -> bool {
        !matches!(self, Self::Zip)
    }
}

/// Detect the archive format from the file name and magic bytes.
pub fn detect(path: &Path) -> Result<ArchiveFormat> {
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default().to_ascii_lowercase();

    if name.ends_with(".7z") {
        return Err(Error::capability(
            CapabilityState::DependencyBlocked,
            "7z archives are dependency_blocked: no Apache-2.0 compatible codec is vendored",
        ));
    }
    if name.ends_with(".rar") {
        return Err(Error::capability(
            CapabilityState::DependencyBlocked,
            "rar archives are dependency_blocked: no Apache-2.0 compatible codec is vendored",
        ));
    }
    if let Some(format) = from_file_name(&name) {
        return Ok(format);
    }

    let mut file = File::open(path).map_err(|error| Error::io(path.display().to_string(), error))?;
    let mut head = [0_u8; 8];
    let read = file.read(&mut head).map_err(|error| Error::io(path.display().to_string(), error))?;
    let head = &head[..read];

    if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") || head.starts_with(b"PK\x07\x08") {
        return Ok(ArchiveFormat::Zip);
    }
    if head.starts_with(&[0x1f, 0x8b]) {
        return Ok(ArchiveFormat::TarGz);
    }
    if head.starts_with(b"BZh") {
        return Ok(ArchiveFormat::TarBz2);
    }
    if head.starts_with(&[0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00]) {
        return Ok(ArchiveFormat::TarXz);
    }
    if head.starts_with(b"7z\xbc\xaf\x27\x1c") {
        return Err(Error::capability(
            CapabilityState::DependencyBlocked,
            "7z archives are dependency_blocked: no Apache-2.0 compatible codec is vendored",
        ));
    }
    if head.starts_with(b"Rar!\x1a\x07") {
        return Err(Error::capability(
            CapabilityState::DependencyBlocked,
            "rar archives are dependency_blocked: no Apache-2.0 compatible codec is vendored",
        ));
    }
    if is_tar_magic(path)? {
        return Ok(ArchiveFormat::Tar);
    }

    Err(Error::coded(ErrorCode::UnsupportedFormat, format!("cannot determine the archive format of `{}`", path.display())))
}

fn from_file_name(name: &str) -> Option<ArchiveFormat> {
    for format in [ArchiveFormat::TarGz, ArchiveFormat::TarBz2, ArchiveFormat::TarXz] {
        if name.ends_with(&format!(".{}", format.extension())) {
            return Some(format);
        }
    }
    if name.ends_with(".tgz") {
        return Some(ArchiveFormat::TarGz);
    }
    if name.ends_with(".tbz2") || name.ends_with(".tbz") {
        return Some(ArchiveFormat::TarBz2);
    }
    if name.ends_with(".txz") {
        return Some(ArchiveFormat::TarXz);
    }
    if name.ends_with(".zip") {
        return Some(ArchiveFormat::Zip);
    }
    if name.ends_with(".tar") {
        return Some(ArchiveFormat::Tar);
    }
    None
}

fn is_tar_magic(path: &Path) -> Result<bool> {
    use std::io::{Seek, SeekFrom};
    let mut file = File::open(path).map_err(|error| Error::io(path.display().to_string(), error))?;
    let header = match file.seek(SeekFrom::Start(257)) {
        Ok(_) => {
            let mut buffer = [0_u8; 8];
            match file.read(&mut buffer) {
                Ok(read) if read >= 5 => buffer,
                _ => return Ok(false),
            }
        }
        Err(_) => return Ok(false),
    };
    Ok(header.starts_with(b"ustar"))
}

/// Format inferred from a file name only (used when recursing into nested archives).
pub fn detect_by_name(name: &str) -> Option<ArchiveFormat> {
    from_file_name(&name.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_are_accepted() {
        assert_eq!(ArchiveFormat::from_name("tgz").unwrap(), ArchiveFormat::TarGz);
        assert_eq!(ArchiveFormat::from_name("tar.bz2").unwrap(), ArchiveFormat::TarBz2);
        assert_eq!(ArchiveFormat::from_name("txz").unwrap(), ArchiveFormat::TarXz);
    }

    #[test]
    fn seven_zip_is_dependency_blocked() {
        let error = ArchiveFormat::from_name("7z").unwrap_err();
        assert_eq!(error.code(), "dependency_blocked");
    }

    #[test]
    fn file_name_detection_handles_compounds() {
        assert_eq!(detect_by_name("backup.tar.gz"), Some(ArchiveFormat::TarGz));
        assert_eq!(detect_by_name("photo.JPG"), None);
    }
}
