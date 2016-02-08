mod common;

use common::{RawTarEntry, TempDir, write_raw_tar};
use open_zipper_core::{ExtractOptions, extract, list};

#[test]
fn tar_parent_traversal_is_rejected() {
    let base = TempDir::new("tar-slip");
    let archive = base.join("evil.tar");
    write_raw_tar(&archive, &[RawTarEntry::file("../owned.txt", "owned"), RawTarEntry::file("safe/ok.txt", "ok")]);
    let destination = base.join("out");
    let error = extract(&archive, &ExtractOptions::new(&destination)).expect_err("tar traversal must be rejected");
    assert_eq!(error.code(), "zip_slip");
    assert!(!base.join("owned.txt").exists());
    assert!(!destination.join("safe/ok.txt").exists(), "preflight rejects the whole archive before writing");
}

#[test]
fn tar_absolute_path_is_rejected() {
    let base = TempDir::new("tar-absolute");
    let archive = base.join("evil.tar");
    write_raw_tar(&archive, &[RawTarEntry::file("/tmp/owned.txt", "owned")]);
    let error = extract(&archive, &ExtractOptions::new(base.join("out"))).expect_err("absolute tar path");
    assert_eq!(error.code(), "zip_slip");
}

#[test]
fn tar_symlink_and_hardlink_are_rejected() {
    let base = TempDir::new("tar-links");
    for (tag, typeflag) in [("symlink", b'2'), ("hardlink", b'1')] {
        let archive = base.join(format!("{tag}.tar"));
        write_raw_tar(&archive, &[RawTarEntry::link("escape", typeflag, "../../outside.txt")]);
        let error = extract(&archive, &ExtractOptions::new(base.join(format!("out-{tag}")))).expect_err("link entry");
        assert_eq!(error.code(), "symlink_entry", "{tag}");
    }
}

#[test]
fn list_reports_unsafe_entries_without_failing() {
    let base = TempDir::new("tar-list");
    let archive = base.join("mixed.tar");
    write_raw_tar(&archive, &[RawTarEntry::file("../owned.txt", "owned")]);
    let listing = list(&archive, &Default::default()).expect("listing stays inspectable");
    assert_eq!(listing.entry_count, 1);
    assert_eq!(listing.unsafe_entry_count, 1);
    assert!(!listing.entries[0].safe);
}

#[test]
fn tar_directories_and_files_extract_in_order() {
    let base = TempDir::new("tar-safe");
    let archive = base.join("safe.tar");
    write_raw_tar(&archive, &[RawTarEntry::directory("docs"), RawTarEntry::file("docs/report.txt", "hello tar")]);
    let destination = base.join("out");
    let report = extract(&archive, &ExtractOptions::new(&destination)).expect("safe tar extracts");
    assert_eq!(report.directories, 1);
    assert_eq!(report.written, 1);
    assert_eq!(common::read_file(&destination.join("docs/report.txt")), b"hello tar");
}
