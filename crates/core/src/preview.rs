//! Bounded single-entry reads for the GUI preview, without touching disk.
//!
//! The preview reuses the extraction codecs' member readers, so the same
//! encrypted, compression and path checks apply. Over-length entries stop at
//! the cap and report `truncated` instead of failing, and the byte budget of
//! the decoder keeps a lying member from expanding past the cap plus one chunk.

use serde::Serialize;

use std::path::Path;

use crate::{
    error::{Error, ErrorCode, Result},
    format::{self, ArchiveFormat},
    limits::ArchiveLimits,
    model::EntryKind,
    sevenz_codec, tar_codec,
    zip_codec::{self, PreparedEntry},
};

/// Default preview budget: 256 KiB of decoded bytes.
pub const DEFAULT_PREVIEW_CAP: u64 = 256 * 1024;
/// Hard ceiling for one preview reply, sized for the `PostWebMessageAsString` limit.
pub const MAX_PREVIEW_CAP: u64 = 1024 * 1024;

/// How the GUI should render one entry payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PreviewKind {
    Text,
    Image,
    Binary,
    Empty,
}

/// One bounded entry read with the classification the page renders from.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewResult {
    pub name: String,
    pub index: usize,
    pub kind: PreviewKind,
    /// `image/*` for images; absent for text so the page never trusts extensions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    /// Set for text previews: `utf-8` or `gbk`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
    /// Decoded text for `text` previews.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Standard base64 of the read bytes for `image` previews.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_base64: Option<String>,
    pub truncated: bool,
    pub previewed_bytes: u64,
    pub total_bytes: u64,
}

/// Reads up to `cap_bytes` of one entry and classifies the payload.
///
/// The cap is clamped to `1..=MAX_PREVIEW_CAP`. Directories, links, encrypted
/// members and out-of-range indexes fail with the same stable codes as
/// extraction; over-length payloads only set `truncated`.
pub fn preview(path: &Path, index: usize, cap_bytes: u64, limits: &ArchiveLimits) -> Result<PreviewResult> {
    limits.validate()?;
    let cap = cap_bytes.clamp(1, MAX_PREVIEW_CAP);
    let archive_format = format::detect(path)?;
    let prepared = inspect(path, archive_format, limits)?;
    let entry = prepared.get(index).ok_or_else(|| Error::coded(ErrorCode::InvalidArchive, format!("entry index {index} is out of range")))?;

    match entry.kind {
        EntryKind::Directory => return Err(Error::coded(ErrorCode::InvalidOption, format!("entry `{}` is a directory", entry.name))),
        EntryKind::Symlink | EntryKind::Hardlink | EntryKind::Other => {
            return Err(Error::coded(ErrorCode::SymlinkEntry, format!("entry `{}` is not a regular file", entry.name)));
        }
        EntryKind::File => {}
    }

    let (bytes, truncated) = match archive_format {
        ArchiveFormat::Zip => zip_codec::read_member(path, index, cap)?,
        ArchiveFormat::SevenZ => sevenz_codec::read_member(path, &entry.name, cap)?,
        other => tar_codec::read_member(path, other, index, cap, limits)?,
    };
    Ok(classify(entry, bytes, truncated))
}

pub(crate) fn inspect(path: &Path, archive_format: ArchiveFormat, limits: &ArchiveLimits) -> Result<Vec<PreparedEntry>> {
    match archive_format {
        ArchiveFormat::Zip => zip_codec::inspect(path, limits, false),
        ArchiveFormat::SevenZ => sevenz_codec::inspect(path, limits, false),
        other => tar_codec::inspect(path, other, limits, false),
    }
}

fn classify(entry: &PreparedEntry, bytes: Vec<u8>, truncated: bool) -> PreviewResult {
    let previewed_bytes = bytes.len() as u64;
    let mut result = PreviewResult {
        name: entry.name.clone(),
        index: entry.index,
        kind: PreviewKind::Binary,
        mime: None,
        encoding: None,
        text: None,
        data_base64: None,
        truncated,
        previewed_bytes,
        total_bytes: entry.size,
    };

    if bytes.is_empty() && entry.size == 0 {
        result.kind = PreviewKind::Empty;
        return result;
    }

    if let Some(mime) = image_mime(&entry.name, &bytes) {
        result.kind = PreviewKind::Image;
        result.mime = Some(mime);
        result.data_base64 = Some(encode_base64(&bytes));
        return result;
    }

    if bytes.iter().take(8192).any(|byte| *byte == 0) {
        return result;
    }

    if let Some(text) = decode_text(&bytes, truncated) {
        result.kind = PreviewKind::Text;
        result.encoding = Some(text.1.to_string());
        result.text = Some(text.0);
        return result;
    }
    result
}

/// Text decoders tried in order; the returned label is the encoding name.
fn decode_text(bytes: &[u8], truncated: bool) -> Option<(String, &'static str)> {
    match std::str::from_utf8(bytes) {
        Ok(text) => return Some((text.to_string(), "utf-8")),
        Err(error) => {
            // A truncated read can end inside a multi-byte sequence; accept the
            // valid prefix when the remainder is only that incomplete tail.
            if truncated
                && error.error_len().is_none()
                && error.valid_up_to() + 3 >= bytes.len()
                && let Ok(text) = std::str::from_utf8(&bytes[..error.valid_up_to()])
            {
                return Some((text.to_string(), "utf-8"));
            }
        }
    }
    let (decoded, encoding, had_errors) = encoding_rs::GBK.decode(bytes);
    if !had_errors && encoding == encoding_rs::GBK {
        return Some((decoded.to_string(), "gbk"));
    }
    None
}

/// Image classification requires both the extension and the magic bytes, so a
/// renamed binary never enters the page as a `data:` image URI.
fn image_mime(name: &str, bytes: &[u8]) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    let matches = |magic: &[&[u8]]| magic.iter().any(|prefix| bytes.starts_with(prefix));
    if lower.ends_with(".png") && matches(&[b"\x89PNG\r\n\x1a\n"]) {
        return Some("image/png".to_string());
    }
    if (lower.ends_with(".jpg") || lower.ends_with(".jpeg")) && matches(&[b"\xff\xd8\xff"]) {
        return Some("image/jpeg".to_string());
    }
    if lower.ends_with(".gif") && matches(&[b"GIF87a", b"GIF89a"]) {
        return Some("image/gif".to_string());
    }
    if lower.ends_with(".webp") && bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return Some("image/webp".to_string());
    }
    if lower.ends_with(".bmp") && matches(&[b"BM"]) {
        return Some("image/bmp".to_string());
    }
    if lower.ends_with(".ico") && matches(&[b"\x00\x00\x01\x00"]) {
        return Some("image/x-icon".to_string());
    }
    None
}

fn encode_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = *chunk.get(1).unwrap_or(&0);
        let third = *chunk.get(2).unwrap_or(&0);
        let bits = (u32::from(first) << 16) | (u32::from(second) << 8) | u32::from(third);
        output.push(ALPHABET[(bits >> 18) as usize] as char);
        output.push(ALPHABET[((bits >> 12) & 63) as usize] as char);
        output.push(if chunk.len() > 1 { ALPHABET[((bits >> 6) & 63) as usize] as char } else { '=' });
        output.push(if chunk.len() > 2 { ALPHABET[(bits & 63) as usize] as char } else { '=' });
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_alphabet() {
        assert_eq!(encode_base64(b""), "");
        assert_eq!(encode_base64(b"f"), "Zg==");
        assert_eq!(encode_base64(b"fo"), "Zm8=");
        assert_eq!(encode_base64(b"foo"), "Zm9v");
        assert_eq!(encode_base64(b"foob"), "Zm9vYg==");
    }

    #[test]
    fn forged_png_extension_without_magic_is_not_an_image() {
        let entry = PreparedEntry {
            index: 0,
            name: "fake.png".to_string(),
            kind: EntryKind::File,
            size: 4,
            compressed: None,
            compression_method: None,
            modified_at_unix_ms: None,
            safe: None,
            unsafe_reason: None,
        };
        let result = classify(&entry, vec![0x00, 0x01, 0x02, 0x03], false);
        assert_eq!(result.kind, PreviewKind::Binary);
    }

    #[test]
    fn truncated_utf8_tail_is_trimmed_not_rejected() {
        let entry = PreparedEntry {
            index: 0,
            name: "note.txt".to_string(),
            kind: EntryKind::File,
            size: 8,
            compressed: None,
            compression_method: None,
            modified_at_unix_ms: None,
            safe: None,
            unsafe_reason: None,
        };
        let mut bytes = "héllo".as_bytes().to_vec();
        bytes.push(0xC3);
        let result = classify(&entry, bytes, true);
        assert_eq!(result.kind, PreviewKind::Text);
        assert_eq!(result.text.as_deref(), Some("héllo"));
    }
}
