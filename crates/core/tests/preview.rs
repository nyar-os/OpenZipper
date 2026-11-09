mod common;

use common::{SevenZFixtureEntry, TarFixtureEntry, TempDir, ZipFixtureEntry, mark_zip_encrypted, write_7z, write_file, write_tar, write_zip};
use open_zipper_core::{ArchiveLimits, EntryKind, PreviewKind, extract_entry, list, preview};

fn limits() -> ArchiveLimits {
    ArchiveLimits::default()
}

#[test]
fn zip_text_and_image_and_binary_previews_classify_in_core() {
    let dir = TempDir::new("preview-zip");
    let archive = dir.join("bundle.zip");
    let png = [0x89_u8, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'].to_vec();
    write_zip(
        &archive,
        &[
            ZipFixtureEntry::file("notes/readme.md", b"# hello zipper".to_vec()),
            ZipFixtureEntry::file("img/logo.png", png.clone()),
            ZipFixtureEntry::file("bin/blob.dat", vec![0x00, 0x01, 0x02, 0x00]),
            ZipFixtureEntry::file("empty.txt", Vec::new()),
            ZipFixtureEntry::file("docs/", Vec::new()),
        ],
    );

    let listing = list(&archive, &limits()).expect("listing");
    let markdown = preview(&archive, 0, 256 * 1024, &limits()).expect("markdown preview");
    assert_eq!(markdown.kind, PreviewKind::Text);
    assert_eq!(markdown.encoding.as_deref(), Some("utf-8"));
    assert_eq!(markdown.text.as_deref(), Some("# hello zipper"));
    assert_eq!(markdown.total_bytes, listing.entries[0].size_bytes);

    let image = preview(&archive, 1, 256 * 1024, &limits()).expect("image preview");
    assert_eq!(image.kind, PreviewKind::Image);
    assert_eq!(image.mime.as_deref(), Some("image/png"));
    assert!(image.data_base64.as_deref().is_some_and(|text| !text.is_empty()));

    let binary = preview(&archive, 2, 256 * 1024, &limits()).expect("binary preview");
    assert_eq!(binary.kind, PreviewKind::Binary);

    let empty = preview(&archive, 3, 256 * 1024, &limits()).expect("empty preview");
    assert_eq!(empty.kind, PreviewKind::Empty);

    let directory = preview(&archive, 4, 256 * 1024, &limits());
    assert_eq!(directory.err().map(|error| error.code()), Some("invalid_option"));
}

#[test]
fn gbk_text_decodes_with_the_shared_name_fallback() {
    let dir = TempDir::new("preview-gbk");
    let archive = dir.join("gbk.zip");
    // "中文" in GBK: D6 D0 CE C4, invalid UTF-8 but valid GBK with no replacements.
    write_zip(&archive, &[ZipFixtureEntry::file("cn.txt", vec![0xd6, 0xd0, 0xce, 0xc4])]);
    let result = preview(&archive, 0, 256 * 1024, &limits()).expect("gbk preview");
    assert_eq!(result.kind, PreviewKind::Text);
    assert_eq!(result.encoding.as_deref(), Some("gbk"));
    assert_eq!(result.text.as_deref(), Some("中文"));
}

#[test]
fn oversized_entries_truncate_instead_of_failing() {
    let dir = TempDir::new("preview-cap");
    let archive = dir.join("big.zip");
    write_zip(&archive, &[ZipFixtureEntry::file("big.txt", vec![b'a'; 100_000])]);
    let result = preview(&archive, 0, 1024, &limits()).expect("truncated preview");
    assert_eq!(result.kind, PreviewKind::Text);
    assert!(result.truncated);
    assert_eq!(result.previewed_bytes, 1024);
    assert_eq!(result.total_bytes, 100_000);
    assert_eq!(result.text.as_deref().map(str::len), Some(1024));
}

#[test]
fn encrypted_and_out_of_range_entries_are_refused() {
    let dir = TempDir::new("preview-refuse");
    let archive = dir.join("locked.zip");
    write_zip(&archive, &[ZipFixtureEntry::file("secret.txt", b"top secret".to_vec())]);
    mark_zip_encrypted(&archive);
    assert_eq!(preview(&archive, 0, 1024, &limits()).err().map(|error| error.code()), Some("encrypted_archive"));

    let plain = dir.join("plain.zip");
    write_zip(&plain, &[ZipFixtureEntry::file("a.txt", b"a".to_vec())]);
    assert_eq!(preview(&plain, 99, 1024, &limits()).err().map(|error| error.code()), Some("invalid_archive"));
}

#[test]
fn tar_and_7z_previews_match_the_zip_result() {
    let dir = TempDir::new("preview-formats");
    let tar_path = dir.join("bundle.tar");
    write_tar(&tar_path, &[TarFixtureEntry::file("notes.txt", b"tar preview text".to_vec())]);
    let tar_result = preview(&tar_path, 0, 256 * 1024, &limits()).expect("tar preview");
    assert_eq!(tar_result.kind, PreviewKind::Text);
    assert_eq!(tar_result.text.as_deref(), Some("tar preview text"));

    let targz = dir.join("bundle.tar.gz");
    // Build the tar.gz through the create path so the fixture is a real stream.
    let source_file = dir.join("payload.txt");
    write_file(&source_file, b"gz preview text");
    open_zipper_core::create(std::slice::from_ref(&source_file), &targz, Some("tar.gz"), &Default::default()).expect("create tar.gz");
    let listing = list(&targz, &limits()).expect("targz listing");
    let gz_result = preview(&targz, listing.entries.len() - 1, 256 * 1024, &limits()).expect("targz preview");
    assert_eq!(gz_result.kind, PreviewKind::Text);
    assert_eq!(gz_result.text.as_deref(), Some("gz preview text"));

    let seven = dir.join("bundle.7z");
    write_7z(&seven, &[SevenZFixtureEntry::file("deep/note.txt", b"7z preview text".to_vec())], None);
    let seven_result = preview(&seven, 0, 256 * 1024, &limits()).expect("7z preview");
    assert_eq!(seven_result.kind, PreviewKind::Text);
    assert_eq!(seven_result.text.as_deref(), Some("7z preview text"));
}

#[test]
fn extract_entry_writes_one_sanitized_file_and_refuses_conflicts() {
    let dir = TempDir::new("extract-entry");
    let archive = dir.join("bundle.zip");
    write_zip(&archive, &[ZipFixtureEntry::file("docs/readme.md", b"single file".to_vec())]);
    let out = dir.join("temp-preview");
    std::fs::create_dir_all(&out).expect("create out dir");

    let extracted = extract_entry(&archive, 0, &out, &limits()).expect("extract one entry");
    assert_eq!(extracted.name, "readme.md");
    assert_eq!(std::fs::read(&extracted.path).expect("read extracted"), b"single file");
    assert_eq!(extracted.bytes_written, 11);

    let conflict = extract_entry(&archive, 0, &out, &limits());
    assert_eq!(conflict.err().map(|error| error.code()), Some("path_conflict"));

    let listing = list(&archive, &limits()).expect("listing");
    assert_eq!(listing.entries[0].kind, EntryKind::File);
}

#[test]
fn extract_entry_refuses_directories() {
    let dir = TempDir::new("extract-entry-dir");
    let archive = dir.join("bundle.zip");
    write_zip(&archive, &[ZipFixtureEntry::file("docs/", Vec::new())]);
    let out = dir.join("temp-preview");
    std::fs::create_dir_all(&out).expect("create out dir");
    assert_eq!(extract_entry(&archive, 0, &out, &limits()).err().map(|error| error.code()), Some("invalid_option"));
}
