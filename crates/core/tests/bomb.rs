mod common;

use common::{TempDir, ZipFixtureEntry, patch_zip_central_uncompressed_size, write_zip};
use open_zipper_core::{ArchiveLimits, ExtractOptions, OverwritePolicy, extract};

/// A member whose declared size passes the preflight check but whose payload
/// decodes past the per-entry cap must stop during the stream. Reaching the end
/// of the member instead would report `invalid_archive` from the size check, so
/// `archive_bomb` is the evidence that the limit fired mid-flight.
#[test]
fn member_that_understates_its_size_aborts_mid_stream() {
    let base = TempDir::new("bomb-lying-size");
    let archive = base.join("lying.zip");
    write_zip(&archive, &[ZipFixtureEntry::file("zeros.bin", vec![0_u8; 2 * 1024 * 1024])]);
    patch_zip_central_uncompressed_size(&archive, 1024);

    let destination = base.join("out");
    let mut options = ExtractOptions::new(&destination);
    options.limits = ArchiveLimits { max_entry_bytes: 8 * 1024, max_total_bytes: 8 * 1024, ..ArchiveLimits::default() };
    let error = extract(&archive, &options).expect_err("an understated member must be rejected");
    assert_eq!(error.code(), "archive_bomb", "got {error}");
    assert!(!destination.join("zeros.bin").exists(), "a partially written member must not survive");
}

#[test]
fn compression_ratio_aborts_before_writing() {
    let base = TempDir::new("bomb-ratio");
    let archive = base.join("ratio.zip");
    write_zip(&archive, &[ZipFixtureEntry::file("zeros.bin", vec![0_u8; 2 * 1024 * 1024])]);
    let destination = base.join("out");
    let mut options = ExtractOptions::new(&destination);
    options.limits = ArchiveLimits { max_compression_ratio: 10, ..ArchiveLimits::default() };
    let error = extract(&archive, &options).expect_err("ratio bomb must be rejected");
    assert_eq!(error.code(), "archive_bomb");
    assert!(!destination.join("zeros.bin").exists());
}

#[test]
fn entry_count_limit_aborts() {
    let base = TempDir::new("bomb-count");
    let archive = base.join("many.zip");
    let entries: Vec<_> = (0..8).map(|index| ZipFixtureEntry::file(format!("file-{index}.txt"), b"x")).collect();
    write_zip(&archive, &entries);
    let mut options = ExtractOptions::new(base.join("out"));
    options.limits = ArchiveLimits { max_entries: 4, ..ArchiveLimits::default() };
    let error = extract(&archive, &options).expect_err("entry-count bomb");
    assert_eq!(error.code(), "archive_bomb");
}

#[test]
fn total_size_limit_aborts() {
    let base = TempDir::new("bomb-total");
    let archive = base.join("total.zip");
    write_zip(&archive, &[ZipFixtureEntry::file("one.bin", vec![b'a'; 2048]), ZipFixtureEntry::file("two.bin", vec![b'b'; 2048])]);
    let mut options = ExtractOptions::new(base.join("out"));
    options.limits = ArchiveLimits { max_total_bytes: 2048, ..ArchiveLimits::default() };
    let error = extract(&archive, &options).expect_err("total-size bomb");
    assert_eq!(error.code(), "archive_bomb");
}

#[test]
fn nested_archive_depth_is_bounded() {
    let base = TempDir::new("bomb-nested");
    let deep = base.join("deep.zip");
    write_zip(&deep, &[ZipFixtureEntry::file("leaf.txt", b"leaf")]);
    let deep_bytes = common::read_file(&deep);

    let inner = base.join("inner.zip");
    write_zip(&inner, &[ZipFixtureEntry::file("deep.zip", deep_bytes)]);
    let inner_bytes = common::read_file(&inner);

    let outer = base.join("outer.zip");
    write_zip(&outer, &[ZipFixtureEntry::file("inner.zip", inner_bytes)]);

    let destination = base.join("out");
    let mut options = ExtractOptions::new(&destination);
    options.recurse = true;
    options.overwrite = OverwritePolicy::Error;
    options.limits = ArchiveLimits { max_nested_depth: 1, ..ArchiveLimits::default() };
    let error = extract(&outer, &options).expect_err("nested depth must abort");
    assert_eq!(error.code(), "nested_depth_exceeded");
}

#[test]
fn nested_archives_are_extracted_within_the_limit() {
    let base = TempDir::new("bomb-nested-ok");
    let deep = base.join("deep.zip");
    write_zip(&deep, &[ZipFixtureEntry::file("leaf.txt", b"leaf")]);
    let deep_bytes = common::read_file(&deep);

    let outer = base.join("outer.zip");
    write_zip(&outer, &[ZipFixtureEntry::file("deep.zip", deep_bytes)]);

    let destination = base.join("out");
    let mut options = ExtractOptions::new(&destination);
    options.recurse = true;
    options.limits = ArchiveLimits { max_nested_depth: 1, ..ArchiveLimits::default() };
    let report = extract(&outer, &options).expect("nested extraction within limit");
    assert!(report.nested_archives >= 1);
    assert!(destination.join("deep.zip").exists());
}
