use std::path::PathBuf;

use serde::Serialize;

use crate::{
    error::{Error, ErrorCode, Result},
    platform::Platform,
};

/// A validated, relative archive entry path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SafeEntryPath {
    pub components: Vec<String>,
    pub relative: PathBuf,
    /// Key used for duplicate detection. Case-folded on Windows.
    pub normalized_key: String,
}

impl SafeEntryPath {
    pub fn display(&self) -> String {
        self.components.join("/")
    }
}

/// Reject every path shape that can escape the destination directory or be
/// re-interpreted by Windows after normalization.
///
/// This runs before any file or directory is created for the entry.
pub fn validate_entry_path(name: &str, platform: Platform) -> Result<SafeEntryPath> {
    if name.is_empty() {
        return Err(Error::coded(ErrorCode::ZipSlip, "archive entry has an empty name"));
    }
    if name.chars().any(|character| character == '\0' || (character.is_control() && character != '\t')) {
        return Err(Error::coded(ErrorCode::ZipSlip, format!("archive entry `{name}` contains control characters")));
    }

    // Backslashes are separators on Windows archives even when they are stored
    // verbatim. Normalizing here prevents `..\evil.txt` from slipping through.
    let unified = name.replace('\\', "/");
    if unified.starts_with('/') {
        return Err(Error::coded(ErrorCode::ZipSlip, format!("archive entry `{name}` is an absolute path")));
    }
    let bytes = unified.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return Err(Error::coded(ErrorCode::ZipSlip, format!("archive entry `{name}` uses a drive-letter prefix")));
    }

    let mut components = Vec::new();
    for component in unified.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        if component == ".." {
            return Err(Error::coded(ErrorCode::ZipSlip, format!("archive entry `{name}` contains a parent traversal segment")));
        }
        if platform == Platform::Windows {
            if component.contains(':') {
                return Err(Error::coded(
                    ErrorCode::ZipSlip,
                    format!("archive entry `{name}` contains a colon, which Windows reads as an alternate data stream"),
                ));
            }
            // Windows strips trailing dots and spaces from path components, so
            // `CON.`, `evil.` and `name ` would all resolve to a different name
            // than the archive declares.
            let trimmed = component.trim_end_matches(['.', ' ']);
            if trimmed != component {
                return Err(Error::coded(
                    ErrorCode::ZipSlip,
                    format!("archive entry `{name}` relies on Windows trailing dot or space normalization"),
                ));
            }
            if is_reserved_device_name(component) {
                return Err(Error::coded(ErrorCode::ZipSlip, format!("archive entry `{name}` uses the reserved device name `{component}`")));
            }
        }
        components.push(component.to_string());
    }

    if components.is_empty() {
        return Err(Error::coded(ErrorCode::ZipSlip, format!("archive entry `{name}` resolves to the destination root")));
    }

    let normalized_key = if platform == Platform::Windows {
        components.iter().map(|component| component.to_lowercase()).collect::<Vec<_>>().join("/")
    }
    else {
        components.join("/")
    };
    let relative: PathBuf = components.iter().collect();

    Ok(SafeEntryPath { components, relative, normalized_key })
}

fn is_reserved_device_name(component: &str) -> bool {
    let base = component.split('.').next().unwrap_or(component).trim().to_ascii_uppercase();
    matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$")
        || (base.len() == 4
            && (base.starts_with("COM") || base.starts_with("LPT"))
            && base.as_bytes()[3].is_ascii_digit()
            && base.as_bytes()[3] != b'0')
}

/// Ensure a destination path stays inside `destination` after validation.
pub fn join_destination(destination: &std::path::Path, safe: &SafeEntryPath) -> PathBuf {
    let mut base = destination.to_path_buf();
    for component in &safe.components {
        base.push(component);
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reject(name: &str) {
        assert!(validate_entry_path(name, Platform::Windows).is_err(), "`{name}` must be rejected");
    }

    fn accept(name: &str) -> SafeEntryPath {
        validate_entry_path(name, Platform::Windows).expect("path should be accepted")
    }

    #[test]
    fn plain_nested_paths_are_accepted() {
        let path = accept("docs/notes/report.txt");
        assert_eq!(path.display(), "docs/notes/report.txt");
        assert_eq!(path.relative, PathBuf::from("docs").join("notes").join("report.txt"));
    }

    #[test]
    fn parent_traversal_is_rejected() {
        reject("../evil.txt");
        reject("docs/../../evil.txt");
        reject("..\\evil.txt");
        reject("/absolute/evil.txt");
        reject("C:\\evil.txt");
        reject("\\\\server\\share\\evil.txt");
    }

    #[test]
    fn windows_normalization_bypasses_are_rejected() {
        reject("docs/evil.");
        reject("docs/evil ");
        reject("CON");
        reject("con.txt");
        reject("COM1/evil.txt");
        reject("docs/evil:stream");
    }

    #[test]
    fn dot_segments_are_ignored_but_never_escape() {
        let path = accept("docs/./notes/report.txt");
        assert_eq!(path.display(), "docs/notes/report.txt");
        reject("./");
    }

    #[test]
    fn duplicate_keys_follow_platform_case_rules() {
        let a = accept("Docs/Report.txt");
        let b = accept("docs/report.txt");
        assert_eq!(a.normalized_key, b.normalized_key);
        let c = validate_entry_path("Docs/Report.txt", Platform::Linux).expect("path");
        let d = validate_entry_path("docs/report.txt", Platform::Linux).expect("path");
        assert_ne!(c.normalized_key, d.normalized_key);
    }
}
