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
