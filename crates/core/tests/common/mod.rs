#![allow(dead_code)]

use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|duration| duration.as_nanos()).unwrap_or(0);
        let path = std::env::temp_dir().join(format!("open-zipper-{tag}-{}-{stamp}-{unique}", std::process::id()));
        fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn join(&self, name: impl AsRef<Path>) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub struct ZipFixtureEntry {
    pub name: String,
    pub data: Vec<u8>,
    pub mode: u32,
}

impl ZipFixtureEntry {
    pub fn file(name: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        Self { name: name.into(), data: data.into(), mode: 0o644 }
    }

    pub fn symlink(name: impl Into<String>, target: impl Into<String>) -> Self {
        Self { name: name.into(), data: target.into().into_bytes(), mode: 0o120777 }
    }
}

pub fn write_zip(path: &Path, entries: &[ZipFixtureEntry]) {
    let file = File::create(path).expect("create zip");
    let mut writer = ZipWriter::new(file);
    for entry in entries {
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .unix_permissions(entry.mode & 0o777)
            .system(zip::System::Unix)
            .external_attributes((entry.mode & 0o177_777) << 16);
        writer.start_file(entry.name.as_str(), options).expect("start zip entry");
        writer.write_all(&entry.data).expect("write zip entry");
    }
    writer.finish().expect("finish zip");
}

/// Replace raw name bytes in an existing zip. Used to build archives whose
/// local header and central directory carry bytes a high-level writer refuses
/// to emit (GBK names, traversal names).
pub fn patch_zip_name(path: &Path, from: &[u8], to: &[u8]) {
    assert_eq!(from.len(), to.len(), "patched names must keep the same byte length");
    let bytes = fs::read(path).expect("read zip");
    let replaced = replace_all(&bytes, from, to);
    fs::write(path, replaced).expect("write patched zip");
}

fn replace_all(haystack: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(haystack.len());
    let mut cursor = 0;
    while cursor < haystack.len() {
        if cursor + from.len() <= haystack.len() && &haystack[cursor..cursor + from.len()] == from {
            output.extend_from_slice(to);
            cursor += from.len();
        }
        else {
            output.push(haystack[cursor]);
            cursor += 1;
        }
    }
    output
}

pub struct TarFixtureEntry {
    pub name: String,
    pub data: Vec<u8>,
    pub kind: tar::EntryType,
    pub link: Option<String>,
}

impl TarFixtureEntry {
    pub fn file(name: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        Self { name: name.into(), data: data.into(), kind: tar::EntryType::Regular, link: None }
    }

    pub fn symlink(name: impl Into<String>, target: impl Into<String>) -> Self {
        Self { name: name.into(), data: Vec::new(), kind: tar::EntryType::Symlink, link: Some(target.into()) }
    }
}

pub fn write_tar(path: &Path, entries: &[TarFixtureEntry]) {
    let file = File::create(path).expect("create tar");
    let mut builder = tar::Builder::new(file);
    for entry in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(entry.data.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_entry_type(entry.kind);
        if let Some(link) = &entry.link {
            let _ = header.set_link_name(link);
        }
        header.set_cksum();
        builder.append_data(&mut header, &entry.name, entry.data.as_slice()).expect("append tar entry");
    }
    builder.finish().expect("finish tar");
}

pub struct RawTarEntry {
    pub name: Vec<u8>,
    pub data: Vec<u8>,
    pub typeflag: u8,
    pub link: Vec<u8>,
}

impl RawTarEntry {
    pub fn file(name: impl Into<Vec<u8>>, data: impl Into<Vec<u8>>) -> Self {
        Self { name: name.into(), data: data.into(), typeflag: b'0', link: Vec::new() }
    }

    pub fn link(name: impl Into<Vec<u8>>, typeflag: u8, target: impl Into<Vec<u8>>) -> Self {
        Self { name: name.into(), data: Vec::new(), typeflag, link: target.into() }
    }

    pub fn directory(name: impl Into<Vec<u8>>) -> Self {
        Self { name: name.into(), data: Vec::new(), typeflag: b'5', link: Vec::new() }
    }
}

/// Write tar records directly so hostile names are preserved byte-for-byte.
pub fn write_raw_tar(path: &Path, entries: &[RawTarEntry]) {
    let mut output = Vec::new();
    for entry in entries {
        let name = &entry.name;
        let data = &entry.data;
        let typeflag = &entry.typeflag;
        let link = &entry.link;
        let mut header = [0_u8; 512];
        header[..name.len().min(100)].copy_from_slice(&name[..name.len().min(100)]);
        header[100..108].copy_from_slice(b"0000644\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        let size = format!("{:011o}\0", data.len());
        header[124..136].copy_from_slice(size.as_bytes());
        header[136..148].copy_from_slice(b"00000000000\0");
        header[148..156].copy_from_slice(b"        ");
        header[156] = *typeflag;
        header[157..157 + link.len().min(100)].copy_from_slice(&link[..link.len().min(100)]);
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let checksum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
        header[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
        output.extend_from_slice(&header);
        output.extend_from_slice(data);
        let padding = (512 - data.len() % 512) % 512;
        output.extend(std::iter::repeat_n(0_u8, padding));
    }
    output.extend(std::iter::repeat_n(0_u8, 1024));
    fs::write(path, output).expect("write raw tar");
}

pub struct SevenZFixtureEntry {
    pub name: String,
    pub data: Vec<u8>,
    pub directory: bool,
    pub attributes: Option<u32>,
    pub modified: Option<u64>,
}

impl SevenZFixtureEntry {
    pub fn file(name: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        Self { name: name.into(), data: data.into(), directory: false, attributes: None, modified: None }
    }

    pub fn directory(name: impl Into<String>) -> Self {
        Self { name: name.into(), data: Vec::new(), directory: true, attributes: None, modified: None }
    }

    pub fn symlink(name: impl Into<String>, target: impl Into<String>) -> Self {
        Self { name: name.into(), data: target.into().into_bytes(), directory: false, attributes: Some(0o120777 << 16), modified: None }
    }

    pub fn modified_at(mut self, file_time: u64) -> Self {
        self.modified = Some(file_time);
        self
    }
}

/// Write a real 7z archive with hostile names or attributes intact.
pub fn write_7z(path: &Path, entries: &[SevenZFixtureEntry], method: Option<sevenz_rust2::EncoderMethod>) {
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter, EncoderConfiguration};

    let mut writer = ArchiveWriter::create(path).expect("create 7z");
    writer.set_encrypt_header(false);
    if let Some(method) = method {
        writer.set_content_methods(vec![EncoderConfiguration::new(method)]);
    }
    for entry in entries {
        let mut archive_entry = if entry.directory { ArchiveEntry::new_directory(&entry.name) } else { ArchiveEntry::new_file(&entry.name) };
        if let Some(attributes) = entry.attributes {
            archive_entry.has_windows_attributes = true;
            archive_entry.windows_attributes = attributes;
        }
        if let Some(file_time) = entry.modified {
            archive_entry.has_last_modified_date = true;
            archive_entry.last_modified_date = sevenz_rust2::NtTime::new(file_time);
        }
        if entry.directory {
            writer.push_archive_entry::<&[u8]>(archive_entry, None).expect("push 7z directory");
        }
        else {
            writer.push_archive_entry(archive_entry, Some(entry.data.as_slice())).expect("push 7z file");
        }
    }
    writer.finish().expect("finish 7z");
}

/// Unix file time for 2000-01-01T00:00:00Z, used to assert time metadata.
pub const FILE_TIME_Y2K: u64 = 125_911_584_000_000_000;

pub fn write_file(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, contents).expect("write file");
}

pub fn read_file(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}
