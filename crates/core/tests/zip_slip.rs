mod common;

use std::path::PathBuf;

use common::{TempDir, ZipFixtureEntry, mark_zip_encrypted, patch_zip_name, write_zip};
use open_zipper_core::{ArchiveLimits, ExtractOptions, OverwritePolicy, extract, list};

fn malicious_zip(directory: &TempDir, name: &str, tag: &str) -> PathBuf {
    let placeholder = "x".repeat(name.len());
    let path = directory.join(format!("{tag}.zip"));
    write_zip(&path, &[ZipFixtureEntry::file(&placeholder, b"owned")]);
    patch_zip_name(&path, placeholder.as_bytes(), name.as_bytes());
    path
}

fn assert_rejected(base: &TempDir, name: &str, tag: &str) {
    let archive = malicious_zip(base, name, tag);
    let destination = base.join(format!("out-{tag}"));
    let error = extract(&archive, &ExtractOptions::new(&destination)).expect_err("malicious archive must be rejected");
    assert_eq!(error.code(), "zip_slip", "entry `{name}` must be a zip slip rejection, got {error}");
    assert!(!destination.join("owned").exists());
    assert!(!base.join("owned").exists());
}

#[test]
fn parent_traversal_is_rejected_before_writing() {
    let base = TempDir::new("zip-slip-parent");
    assert_rejected(&base, "../owned.txt", "parent");
    assert_rejected(&base, "docs/../../owned.txt", "nested-parent");
    assert_rejected(&base, "..\\owned.txt", "backslash-parent");
}

#[test]
fn absolute_and_drive_paths_are_rejected() {
    let base = TempDir::new("zip-slip-absolute");
    assert_rejected(&base, "/tmp/owned.txt", "absolute");
    assert_rejected(&base, "C:\\owned.txt", "drive");
    assert_rejected(&base, "\\\\server\\share\\owned.txt", "unc");
}

#[cfg(windows)]
#[test]
fn windows_normalization_bypasses_are_rejected() {
    let base = TempDir::new("zip-slip-windows");
    assert_rejected(&base, "docs/owned.txt.", "trailing-dot");
    assert_rejected(&base, "docs/owned.txt ", "trailing-space");
    assert_rejected(&base, "CON", "device");
    assert_rejected(&base, "COM1/owned.txt", "device-com1");
    assert_rejected(&base, "docs/owned.txt:stream", "ads");
}

/// Trailing dots, spaces, device names and colons are ordinary file names on
/// POSIX hosts, so the guard must not over-block them there.
#[cfg(not(windows))]
#[test]
fn posix_hosts_accept_names_that_only_windows_normalizes() {
    let base = TempDir::new("zip-slip-posix");
    for (name, tag) in [("docs/owned.txt.", "trailing-dot"), ("docs/owned.txt ", "trailing-space"), ("CON", "device")] {
        let archive = malicious_zip(&base, name, tag);
        let destination = base.join(format!("out-{tag}"));
        extract(&archive, &ExtractOptions::new(&destination)).expect("POSIX host keeps the name as written");
    }
}

#[test]
fn symlink_entries_are_rejected() {
    let base = TempDir::new("zip-slip-symlink");
    let archive = base.join("symlink.zip");
    write_zip(&archive, &[ZipFixtureEntry::file("ok.txt", b"ok"), ZipFixtureEntry::symlink("escape", "../../outside.txt")]);
    let destination = base.join("out");
    let error = extract(&archive, &ExtractOptions::new(&destination)).expect_err("symlink must be rejected");
    assert_eq!(error.code(), "symlink_entry");
    assert!(!destination.join("ok.txt").exists(), "preflight must run before any write");
}

/// Windows and default macOS volumes fold case, so these names would land in
/// the same file.
#[cfg(any(windows, target_os = "macos"))]
#[test]
fn case_insensitive_duplicates_are_rejected() {
    let base = TempDir::new("zip-slip-duplicate");
    let archive = base.join("duplicate.zip");
    write_zip(&archive, &[ZipFixtureEntry::file("Docs/report.txt", b"one"), ZipFixtureEntry::file("docs/report.txt", b"two")]);
    let destination = base.join("out");
    let error = extract(&archive, &ExtractOptions::new(&destination)).expect_err("case collision must be rejected");
    assert_eq!(error.code(), "path_conflict");
    assert!(!destination.join("Docs/report.txt").exists());
}

#[cfg(not(any(windows, target_os = "macos")))]
#[test]
fn case_distinct_names_are_not_treated_as_collisions() {
    let base = TempDir::new("zip-slip-duplicate");
    let archive = base.join("duplicate.zip");
    write_zip(&archive, &[ZipFixtureEntry::file("Docs/report.txt", b"one"), ZipFixtureEntry::file("docs/report.txt", b"two")]);
    let destination = base.join("out");
    let report = extract(&archive, &ExtractOptions::new(&destination)).expect("case-sensitive host keeps both files");
    assert_eq!(report.written, 2);
    assert_eq!(common::read_file(&destination.join("Docs/report.txt")), b"one");
    assert_eq!(common::read_file(&destination.join("docs/report.txt")), b"two");
}

#[test]
fn safe_archive_still_extracts() {
    let base = TempDir::new("zip-slip-safe");
    let archive = base.join("safe.zip");
    write_zip(&archive, &[ZipFixtureEntry::file("docs/report.txt", b"hello")]);
    let destination = base.join("out");
    let report = extract(&archive, &ExtractOptions::new(&destination)).expect("safe archive extracts");
    assert_eq!(report.written, 1);
    assert_eq!(common::read_file(&destination.join("docs/report.txt")), b"hello");
}

#[test]
fn dry_run_rejects_and_writes_nothing() {
    let base = TempDir::new("zip-slip-dry-run");
    let archive = malicious_zip(&base, "../owned.txt", "dry");
    let destination = base.join("out");
    let mut options = ExtractOptions::new(&destination);
    options.dry_run = true;
    let error = extract(&archive, &options).expect_err("dry run must still reject");
    assert_eq!(error.code(), "zip_slip");
    assert!(!destination.exists());
}

#[test]
fn overwrite_policies_are_explicit() {
    let base = TempDir::new("zip-overwrite");
    let archive = base.join("safe.zip");
    write_zip(&archive, &[ZipFixtureEntry::file("report.txt", b"new")]);
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
fn encrypted_members_list_but_do_not_extract() {
    let base = TempDir::new("zip-encrypted");
    let archive = base.join("encrypted.zip");
    write_zip(&archive, &[ZipFixtureEntry::file("secret.txt", b"secret")]);
    mark_zip_encrypted(&archive);

    let listing = list(&archive, &Default::default()).expect("indexing ciphertext names stays available");
    assert_eq!(listing.entries[0].name, "secret.txt");

    let destination = base.join("out");
    let error = extract(&archive, &ExtractOptions::new(&destination)).expect_err("encrypted member must be rejected");
    assert_eq!(error.code(), "encrypted_archive");
    assert!(!destination.join("secret.txt").exists());
}

#[test]
fn limits_are_part_of_the_extract_contract() {
    let base = TempDir::new("zip-limits");
    let archive = base.join("safe.zip");
    write_zip(&archive, &[ZipFixtureEntry::file("big.txt", vec![b'a'; 4096])]);
    let destination = base.join("out");
    let mut options = ExtractOptions::new(&destination);
    options.limits = ArchiveLimits { max_entry_bytes: 1024, ..ArchiveLimits::default() };
    let error = extract(&archive, &options).expect_err("per-entry limit must abort");
    assert_eq!(error.code(), "archive_bomb");
}
