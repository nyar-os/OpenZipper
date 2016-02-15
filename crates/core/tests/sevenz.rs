mod common;

use common::{FILE_TIME_Y2K, SevenZFixtureEntry, TempDir, write_7z};
use open_zipper_core::{ArchiveFormat, ArchiveLimits, EntryKind, ExtractOptions, OverwritePolicy, extract, list, test};

fn assert_rejected(base: &TempDir, name: &str, tag: &str, expected_code: &str) {
    let archive = base.join(format!("{tag}.7z"));
    write_7z(&archive, &[SevenZFixtureEntry::file(name, b"owned")], None);
    let destination = base.join(format!("out-{tag}"));
    let error = extract(&archive, &ExtractOptions::new(&destination)).expect_err("malicious 7z must be rejected");
    assert_eq!(error.code(), expected_code, "entry `{name}` must be rejected, got {error}");
    assert!(!destination.join("owned").exists());
    assert!(!base.join("owned").exists());
}

#[test]
fn roundtrip_lists_and_extracts() {
    let base = TempDir::new("sevenz-roundtrip");
    let archive = base.join("sample.7z");
    write_7z(
        &archive,
        &[
            SevenZFixtureEntry::directory("docs"),
            SevenZFixtureEntry::file("docs/report.txt", b"hello seven zip"),
            SevenZFixtureEntry::file("中文文件.txt", "中文内容".as_bytes()),
            SevenZFixtureEntry::file("empty.txt", b""),
        ],
        None,
    );

    let listing = list(&archive, &ArchiveLimits::default()).expect("list 7z");
    assert_eq!(listing.format, ArchiveFormat::SevenZ);
    assert_eq!(listing.entry_count, 4);
    assert_eq!(listing.unsafe_entry_count, 0);
    let report = listing.entries.iter().find(|entry| entry.name == "docs/report.txt").expect("file entry");
    assert_eq!(report.kind, EntryKind::File);
    assert_eq!(report.size_bytes, 15);
    assert_eq!(report.compression_method.as_deref(), Some("lzma2"));
    let chinese = listing.entries.iter().find(|entry| entry.name == "中文文件.txt").expect("unicode entry");
    assert_eq!(chinese.size_bytes, "中文内容".len() as u64);

    let destination = base.join("out");
    let extract_report = extract(&archive, &ExtractOptions::new(&destination)).expect("extract 7z");
    assert_eq!(extract_report.written, 3);
    assert_eq!(extract_report.directories, 1);
    assert_eq!(common::read_file(&destination.join("docs/report.txt")), b"hello seven zip");
    assert_eq!(common::read_file(&destination.join("中文文件.txt")), "中文内容".as_bytes());
    assert!(destination.join("empty.txt").is_file());

    let test_report = test(&archive, &ArchiveLimits::default()).expect("integrity check");
    assert!(test_report.ok);
    assert_eq!(test_report.format, ArchiveFormat::SevenZ);
    assert_eq!(test_report.entry_count, 4);
    assert_eq!(test_report.total_bytes, 15 + "中文内容".len() as u64);
}

#[test]
fn metadata_carries_time_and_size() {
    let base = TempDir::new("sevenz-metadata");
    let archive = base.join("metadata.7z");
    write_7z(&archive, &[SevenZFixtureEntry::file("stamped.txt", b"1234").modified_at(FILE_TIME_Y2K)], None);

    let listing = list(&archive, &ArchiveLimits::default()).expect("list 7z");
    let entry = listing.entries.first().expect("entry");
    assert_eq!(entry.compression_method.as_deref(), Some("lzma2"));
    assert_eq!(entry.modified_at_unix_ms, Some(946_684_800_000));
    assert!(entry.compressed_size_bytes.is_some());
}

#[test]
fn parent_traversal_is_rejected_before_writing() {
    let base = TempDir::new("sevenz-slip-parent");
    assert_rejected(&base, "../owned.txt", "parent", "zip_slip");
    assert_rejected(&base, "docs/../../owned.txt", "nested-parent", "zip_slip");
    assert_rejected(&base, "..\\owned.txt", "backslash-parent", "zip_slip");
}

#[test]
fn absolute_and_drive_paths_are_rejected() {
    let base = TempDir::new("sevenz-slip-absolute");
    assert_rejected(&base, "/tmp/owned.txt", "absolute", "zip_slip");
    assert_rejected(&base, "C:\\owned.txt", "drive", "zip_slip");
    assert_rejected(&base, "\\\\server\\share\\owned.txt", "unc", "zip_slip");
}

#[cfg(windows)]
#[test]
fn windows_normalization_bypasses_are_rejected() {
    let base = TempDir::new("sevenz-slip-windows");
    assert_rejected(&base, "docs/owned.txt.", "trailing-dot", "zip_slip");
    assert_rejected(&base, "docs/owned.txt ", "trailing-space", "zip_slip");
    assert_rejected(&base, "CON", "device", "zip_slip");
    assert_rejected(&base, "COM1/owned.txt", "device-com1", "zip_slip");
    assert_rejected(&base, "docs/owned.txt:stream", "ads", "zip_slip");
}

#[cfg(not(windows))]
#[test]
fn posix_hosts_accept_names_that_only_windows_normalizes() {
    let base = TempDir::new("sevenz-slip-posix");
    for (name, tag) in [("docs/owned.txt.", "trailing-dot"), ("docs/owned.txt ", "trailing-space"), ("CON", "device")] {
        let archive = base.join(format!("{tag}.7z"));
        write_7z(&archive, &[SevenZFixtureEntry::file(name, b"ok")], None);
        let destination = base.join(format!("out-{tag}"));
        extract(&archive, &ExtractOptions::new(&destination)).expect("POSIX host keeps the name as written");
    }
}

#[test]
fn unsafe_names_are_flagged_when_listing() {
    let base = TempDir::new("sevenz-slip-list");
    let archive = base.join("hostile.7z");
    write_7z(&archive, &[SevenZFixtureEntry::file("../owned.txt", b"owned")], None);
    let listing = list(&archive, &ArchiveLimits::default()).expect("listing stays available");
    assert_eq!(listing.unsafe_entry_count, 1);
    assert!(!listing.entries[0].safe);
    assert!(listing.entries[0].unsafe_reason.is_some());
}

#[test]
fn symlink_entries_are_rejected() {
    let base = TempDir::new("sevenz-symlink");
    let archive = base.join("symlink.7z");
    write_7z(&archive, &[SevenZFixtureEntry::file("ok.txt", b"ok"), SevenZFixtureEntry::symlink("escape", "/etc/passwd")], None);
    let destination = base.join("out");
    let error = extract(&archive, &ExtractOptions::new(&destination)).expect_err("symlink must be rejected");
    assert_eq!(error.code(), "symlink_entry");
    assert!(!destination.join("ok.txt").exists(), "preflight must run before any write");
}

#[test]
fn encrypted_archives_fail_with_stable_code() {
    let base = TempDir::new("sevenz-encrypted");
    let source = base.join("source");
    common::write_file(&source.join("secret.txt"), b"secret");
    let archive = base.join("encrypted.7z");
    sevenz_rust2::compress_to_path_encrypted(&source, &archive, "password".into()).expect("write encrypted 7z");

    let listing = list(&archive, &ArchiveLimits::default()).expect_err("encrypted archive cannot be read");
    assert_eq!(listing.code(), "encrypted_archive");
    let extraction = extract(&archive, &ExtractOptions::new(base.join("out"))).expect_err("encrypted archive cannot be extracted");
    assert_eq!(extraction.code(), "encrypted_archive");
}

#[test]
fn unsupported_compression_methods_fail_with_stable_code() {
    let base = TempDir::new("sevenz-lz4");
    let archive = base.join("lz4.7z");
    write_7z(&archive, &[SevenZFixtureEntry::file("data.txt", b"payload")], Some(sevenz_rust2::EncoderMethod::LZ4));

    let error = list(&archive, &ArchiveLimits::default()).expect_err("unsupported method must be rejected");
    assert_eq!(error.code(), "unsupported_compression");
    assert!(error.to_string().contains("LZ4"), "message should name the method: {error}");
}

#[test]
fn limits_apply_to_seven_zip() {
    let base = TempDir::new("sevenz-limits");
    let archive = base.join("big.7z");
    write_7z(&archive, &[SevenZFixtureEntry::file("big.txt", vec![b'a'; 4096])], None);
    let destination = base.join("out");
    let mut options = ExtractOptions::new(&destination);
    options.limits = ArchiveLimits { max_entry_bytes: 1024, ..ArchiveLimits::default() };
    let error = extract(&archive, &options).expect_err("per-entry limit must abort");
    assert_eq!(error.code(), "archive_bomb");
    assert!(!destination.join("big.txt").exists());
}

#[test]
fn overwrite_policies_are_explicit() {
    let base = TempDir::new("sevenz-overwrite");
    let archive = base.join("safe.7z");
    write_7z(&archive, &[SevenZFixtureEntry::file("report.txt", b"new")], None);
    let destination = base.join("out");
    common::write_file(&destination.join("report.txt"), b"old");

    let error = extract(&archive, &ExtractOptions::new(&destination)).expect_err("default policy must fail");
    assert_eq!(error.code(), "path_conflict");
    assert_eq!(common::read_file(&destination.join("report.txt")), b"old");

    let mut skip = ExtractOptions::new(&destination);
    skip.overwrite = OverwritePolicy::Skip;
    let report = extract(&archive, &skip).expect("skip policy");
    assert_eq!(report.skipped, 1);
    assert_eq!(common::read_file(&destination.join("report.txt")), b"old");

    let mut rename = ExtractOptions::new(&destination);
    rename.overwrite = OverwritePolicy::Rename;
    let report = extract(&archive, &rename).expect("rename policy");
    assert_eq!(report.renamed, 1);
    assert_eq!(common::read_file(&destination.join("report (1).txt")), b"new");
}

#[test]
fn dry_run_writes_nothing() {
    let base = TempDir::new("sevenz-dry-run");
    let archive = base.join("safe.7z");
    write_7z(&archive, &[SevenZFixtureEntry::file("report.txt", b"payload")], None);
    let destination = base.join("out");
    let mut options = ExtractOptions::new(&destination);
    options.dry_run = true;
    let report = extract(&archive, &options).expect("dry run");
    assert_eq!(report.written, 1);
    assert!(!destination.exists());
}

/// Manual timing run for a larger archive:
/// `cargo test -p open-zipper-core --release --test sevenz -- --ignored large_archive_extract_time --nocapture`
#[test]
#[ignore]
fn large_archive_extract_time() {
    let base = TempDir::new("sevenz-large");
    let archive = base.join("large.7z");
    let payload = make_payload(128 * 1024 * 1024);
    write_7z(&archive, &[SevenZFixtureEntry::file("large.bin", payload.clone())], None);

    let destination = base.join("out");
    let started = std::time::Instant::now();
    let report = extract(&archive, &ExtractOptions::new(&destination)).expect("extract large archive");
    let elapsed = started.elapsed();
    let written = std::fs::metadata(destination.join("large.bin")).expect("extracted file").len();
    assert_eq!(written, payload.len() as u64);
    assert_eq!(report.bytes_written, payload.len() as u64);
    println!(
        "7z large extract: archive={} bytes, output={} bytes, elapsed={:?} ({:.1} MiB/s)",
        std::fs::metadata(&archive).expect("archive metadata").len(),
        written,
        elapsed,
        written as f64 / 1_048_576.0 / elapsed.as_secs_f64()
    );
}

/// Deterministic incompressible bytes. A repeated block compresses thousands of
/// times and trips the bomb-ratio guard, which would measure the guard rather
/// than the decoder.
fn make_payload(size: usize) -> Vec<u8> {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut payload = Vec::with_capacity(size);
    while payload.len() < size {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        payload.push((state >> 24) as u8);
    }
    payload
}
