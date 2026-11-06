mod common;

use std::path::{Path, PathBuf};

use common::{TempDir, ZipFixtureEntry, patch_zip_name, write_file, write_zip};
use open_zipper_core::{ArchiveFormat, CreateOptions, ExtractOptions, OverwritePolicy, create, extract, list, test};

fn source_tree(base: &TempDir) -> PathBuf {
    let root = base.join("input");
    write_file(&root.join("docs/报告.txt"), "中文内容".as_bytes());
    write_file(&root.join("nested/data.bin"), &[0_u8, 1, 2, 3, 4]);
    root
}

fn assert_tree(destination: &Path) {
    assert_eq!(common::read_file(&destination.join("input/docs/报告.txt")), "中文内容".as_bytes());
    assert_eq!(common::read_file(&destination.join("input/nested/data.bin")), [0_u8, 1, 2, 3, 4]);
}

#[test]
fn zip_round_trip() {
    let base = TempDir::new("roundtrip-zip");
    let archive = base.join("bundle.zip");
    let report = create(&[source_tree(&base)], &archive, Some("zip"), &CreateOptions::new()).expect("create zip");
    assert!(report.entry_count >= 3);

    let listing = list(&archive, &Default::default()).expect("list zip");
    assert_eq!(listing.format, ArchiveFormat::Zip);
    assert!(listing.entries.iter().any(|entry| entry.name.ends_with("报告.txt")));
    let report_entry = listing.entries.iter().find(|entry| entry.name.ends_with("报告.txt")).expect("report entry");
    assert!(report_entry.modified_at_unix_ms.is_some(), "created zip entries carry an extended timestamp");
    test(&archive, &Default::default()).expect("test zip");

    let destination = base.join("out");
    extract(&archive, &ExtractOptions::new(&destination)).expect("extract zip");
    assert_tree(&destination);
}

#[test]
fn tar_family_round_trip() {
    for format in ["tar", "tar.gz", "tar.bz2", "tar.xz"] {
        let base = TempDir::new(&format!("roundtrip-{format}"));
        let archive = base.join(format!("bundle.{}", format));
        create(&[source_tree(&base)], &archive, Some(format), &CreateOptions::new()).unwrap_or_else(|error| panic!("create {format}: {error}"));
        let listing = list(&archive, &Default::default()).unwrap_or_else(|error| panic!("list {format}: {error}"));
        assert!(listing.entries.iter().any(|entry| entry.name.ends_with("报告.txt")));
        test(&archive, &Default::default()).unwrap_or_else(|error| panic!("test {format}: {error}"));

        let destination = base.join("out");
        extract(&archive, &ExtractOptions::new(&destination)).unwrap_or_else(|error| panic!("extract {format}: {error}"));
        assert_tree(&destination);
    }
}

#[test]
fn gbk_entry_names_are_restored() {
    let base = TempDir::new("encoding-gbk");
    let archive = base.join("gbk.zip");
    let placeholder = "xxxxxxxxxxxx";
    write_zip(&archive, &[ZipFixtureEntry::file(placeholder, b"payload")]);
    // GBK bytes for `中文测试.txt` are 12 bytes, the same length as the placeholder.
    let gbk = [0xD6, 0xD0, 0xCE, 0xC4, 0xB2, 0xE2, 0xCA, 0xD4, b'.', b't', b'x', b't'];
    patch_zip_name(&archive, placeholder.as_bytes(), &gbk);

    let listing = list(&archive, &Default::default()).expect("list gbk zip");
    assert_eq!(listing.entries[0].name, "中文测试.txt");
    let destination = base.join("out");
    extract(&archive, &ExtractOptions::new(&destination)).expect("extract gbk zip");
    assert_eq!(common::read_file(&destination.join("中文测试.txt")), b"payload");
}

#[test]
fn cp437_entry_names_are_restored() {
    let base = TempDir::new("encoding-cp437");
    let archive = base.join("cp437.zip");
    let placeholder = "x.txt";
    write_zip(&archive, &[ZipFixtureEntry::file(placeholder, b"payload")]);
    // CP437 `é` followed by `.txt`; the GBK pass rejects 0x82 followed by a dot.
    patch_zip_name(&archive, placeholder.as_bytes(), &[0x82, b'.', b't', b'x', b't']);

    let listing = list(&archive, &Default::default()).expect("list cp437 zip");
    assert_eq!(listing.entries[0].name, "\u{e9}.txt");
}

#[test]
fn zip64_members_are_read() {
    let base = TempDir::new("zip64");
    let archive = base.join("zip64.zip");
    let mut writer = acorn_zip::ZipWriter::with_zip64(acorn_zip::Zip64Mode::Always);
    let mut member = acorn_zip::ZipEntry::deflated(b"payload.txt", b"zipped".to_vec());
    member.version_made_by = (3 << 8) | 20;
    writer.push(member);
    std::fs::write(&archive, writer.into_bytes().expect("build zip64")).expect("write zip64");

    let index = acorn_zip::index_zip_bytes("zip64", std::fs::read(&archive).expect("read zip64")).expect("index zip64");
    assert_eq!(index.members.len(), 1);

    let listing = list(&archive, &Default::default()).expect("list zip64");
    assert_eq!(listing.entries[0].name, "payload.txt");
    let destination = base.join("out");
    extract(&archive, &ExtractOptions::new(&destination)).expect("extract zip64");
    assert_eq!(common::read_file(&destination.join("payload.txt")), b"zipped");
}

#[test]
fn zip_timestamps_round_trip() {
    let base = TempDir::new("zip-time");
    let archive = base.join("time.zip");
    let civil = acorn_zip::ZipDateTime::new(2001, 9, 9, 1, 46, 40).expect("valid civil time");
    let dos = civil.to_dos().expect("encode dos time");
    let unix = 1_000_000_000_i64;

    let mut writer = acorn_zip::ZipWriter::new();
    let mut member = acorn_zip::ZipEntry::stored(b"time.txt", b"time".to_vec());
    member.modified = dos;
    member.modified_unix = Some(unix);
    writer.push(member);
    let bytes = writer.into_bytes().expect("build timed zip");
    std::fs::write(&archive, &bytes).expect("write timed zip");

    let index = acorn_zip::index_zip_bytes("time", bytes).expect("index timed zip");
    assert_eq!(index.members[0].modified.parts(), Some(civil));

    let listing = list(&archive, &Default::default()).expect("list timed zip");
    assert_eq!(listing.entries[0].modified_at_unix_ms, Some(unix * 1_000));
}

#[test]
fn create_refuses_existing_output_without_force() {
    let base = TempDir::new("create-force");
    let source = source_tree(&base);
    let archive = base.join("bundle.zip");
    write_file(&archive, b"old");
    let error = create(std::slice::from_ref(&source), &archive, Some("zip"), &CreateOptions::new()).expect_err("existing output");
    assert_eq!(error.code(), "output_exists");

    let report = create(&[source], &archive, Some("zip"), &CreateOptions { force: true, ..CreateOptions::new() }).expect("force create");
    assert!(report.archive_bytes > 0);
}

#[test]
fn failed_force_create_keeps_the_previous_archive() {
    let base = TempDir::new("create-force-keeps");
    let archive = base.join("bundle.zip");
    write_file(&archive, b"previous");
    let missing = base.join("missing-input");
    let error = create(std::slice::from_ref(&missing), &archive, Some("zip"), &CreateOptions { force: true, ..CreateOptions::new() })
        .expect_err("missing source");
    assert_eq!(error.code(), "file_missing");
    assert_eq!(common::read_file(&archive), b"previous", "a failed force create must not destroy the previous archive");
}

#[test]
fn extract_rename_is_reported() {
    let base = TempDir::new("extract-rename-report");
    let archive = base.join("bundle.zip");
    write_zip(&archive, &[ZipFixtureEntry::file("report.txt", b"new")]);
    let destination = base.join("out");
    write_file(&destination.join("report.txt"), b"old");
    let mut options = ExtractOptions::new(&destination);
    options.overwrite = OverwritePolicy::Rename;
    let report = extract(&archive, &options).expect("rename extract");
    assert_eq!(report.renamed, 1);
    assert_eq!(report.entries[0].renamed_to.as_deref(), Some("report (1).txt"));
}
