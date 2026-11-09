//! Windows file associations in the per-user registry (HKCU), so no elevation
//! is ever required and no machine-wide key is touched.
//!
//! The app registers one ProgID (`OpenZipper.Archive`) and lists it under each
//! supported extension's `OpenWithProgids`. Windows 10+ forbids a program from
//! silently making itself the default handler, so this never writes a
//! `UserChoice`; it only reports whether the current default already points at
//! this ProgID. Removing clears only the keys this module owns.

use serde::Serialize;

use open_zipper_core::{CapabilityState, Error as CoreError, ErrorCode, Platform, Result};

/// The single ProgID this app owns.
pub const PROG_ID: &str = "OpenZipper.Archive";

/// Every extension the app can register, normalized.
pub const ALL_EXTENSIONS: &[&str] = &[".zip", ".tar", ".tar.gz", ".tgz", ".tar.bz2", ".tbz2", ".tar.xz", ".txz", ".7z"];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssociationEntry {
    pub extension: String,
    pub registered: bool,
    pub is_default: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssociationStatus {
    pub platform: &'static str,
    pub supported: bool,
    pub prog_id: &'static str,
    pub extensions: Vec<AssociationEntry>,
}

/// Normalizes one user-supplied extension: lowercase, exactly one leading dot,
/// only `[a-z0-9.]` after it. Rejects traversal-ish or empty shapes.
pub fn normalize_extension(raw: &str) -> Result<String> {
    let trimmed = raw.trim().to_ascii_lowercase();
    let without = trimmed.strip_prefix('.').unwrap_or(trimmed.as_str());
    if without.is_empty() || !without.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.') {
        return Err(CoreError::coded(ErrorCode::InvalidOption, format!("`{raw}` is not a usable file extension")));
    }
    Ok(format!(".{without}"))
}

/// Normalizes a requested extension list, falling back to all supported ones
/// when the caller passes nothing. Unknown extensions are rejected.
pub fn normalize_extensions(requested: &[String]) -> Result<Vec<String>> {
    if requested.is_empty() {
        return Ok(ALL_EXTENSIONS.iter().map(|extension| (*extension).to_string()).collect());
    }
    let mut out = Vec::with_capacity(requested.len());
    for raw in requested {
        let normalized = normalize_extension(raw)?;
        if !ALL_EXTENSIONS.contains(&normalized.as_str()) {
            return Err(CoreError::coded(ErrorCode::UnsupportedFormat, format!("`{normalized}` is not a format OpenZipper opens")));
        }
        if !out.contains(&normalized) {
            out.push(normalized);
        }
    }
    Ok(out)
}

/// Builds the `shell\open\command` value. Each path is wrapped in quotes; the
/// `%1` placeholder is quoted too so names with spaces survive the handoff.
pub fn open_command(node_path: &str, script_path: &str) -> String {
    format!("\"{node_path}\" \"{script_path}\" gui --open \"%1\"")
}

/// Percent-encodes an archive path for the `?open=` query parameter, keeping
/// only RFC 3986 unreserved characters and `/`.
pub fn encode_open_query(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len() * 3);
    for byte in path.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/') {
            encoded.push(*byte as char);
        }
        else {
            encoded.push('%');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }
    encoded
}

fn unsupported() -> CoreError {
    CoreError::capability(CapabilityState::UnsupportedPlatform, "file associations live in the Windows registry and are unavailable here")
}

fn placeholder_status(platform: Platform) -> AssociationStatus {
    AssociationStatus {
        platform: platform.name(),
        supported: false,
        prog_id: PROG_ID,
        extensions: ALL_EXTENSIONS
            .iter()
            .map(|extension| AssociationEntry { extension: (*extension).to_string(), registered: false, is_default: false })
            .collect(),
    }
}

/// Reports association state. On non-Windows platforms this returns a
/// `supported = false` status (not an error) so the GUI can explain it.
pub fn status() -> Result<AssociationStatus> {
    let platform = Platform::current();
    if platform != Platform::Windows {
        return Ok(placeholder_status(platform));
    }
    #[cfg(target_os = "windows")]
    return Ok(win::status_all());
    #[cfg(not(target_os = "windows"))]
    Err(unsupported())
}

/// Registers (`enabled = true`) or unregisters the given extensions and
/// returns the fresh status. Unregistering also prunes the shared ProgID when
/// no requested extension still references it.
pub fn set(extensions: &[String], enabled: bool, node_path: &str, script_path: &str, icon_path: &str) -> Result<AssociationStatus> {
    if Platform::current() != Platform::Windows {
        return Err(unsupported());
    }
    let targets = normalize_extensions(extensions)?;
    #[cfg(target_os = "windows")]
    {
        for extension in &targets {
            if enabled {
                win::register(extension, node_path, script_path, icon_path)?;
            }
            else {
                win::unregister(extension)?;
            }
        }
        if !enabled {
            win::prune_progid_if_unused(&targets)?;
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (targets, node_path, script_path, icon_path);
        return Err(unsupported());
    }
    status()
}

#[cfg(target_os = "windows")]
mod win {
    use std::{ffi::OsStr, os::windows::ffi::OsStrExt};

    use open_zipper_core::{Error as CoreError, ErrorCode, Result};
    use windows::{
        Win32::{
            Foundation::WIN32_ERROR,
            System::Registry::{
                HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, KEY_WOW64_64KEY, REG_OPTION_NON_VOLATILE, REG_SAM_FLAGS, REG_SZ,
                REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
            },
        },
        core::PCWSTR,
    };

    use super::{ALL_EXTENSIONS, AssociationEntry, PROG_ID};

    const CLASSES: &str = r"Software\Classes";
    const FILE_EXTS: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts";
    const ERROR_FILE_NOT_FOUND: WIN32_ERROR = WIN32_ERROR(2);

    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value).encode_wide().chain(std::iter::once(0)).collect()
    }

    fn sam(flags: u32) -> REG_SAM_FLAGS {
        REG_SAM_FLAGS(flags | KEY_WOW64_64KEY.0)
    }

    fn check(code: WIN32_ERROR, context: &str) -> Result<()> {
        if code.is_ok() || code == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        Err(CoreError::coded(ErrorCode::IoError, format!("registry {context}: {code:?}")))
    }

    fn set_string(sub_key: &str, value_name: Option<&str>, data: &str, context: &str) -> Result<()> {
        let sub = wide(sub_key);
        let mut key = HKEY::default();
        let code = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(sub.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                sam(KEY_READ.0 | KEY_SET_VALUE.0),
                None,
                &mut key,
                None,
            )
        };
        check(code, &format!("{context} (open)"))?;
        let name = value_name.map(wide).unwrap_or_default();
        let payload = wide(data);
        let bytes = unsafe { std::slice::from_raw_parts(payload.as_ptr().cast::<u8>(), payload.len() * 2) };
        let name_ptr = if value_name.is_some() { PCWSTR(name.as_ptr()) } else { PCWSTR::null() };
        let code = unsafe { RegSetValueExW(key, name_ptr, None, REG_SZ, Some(bytes)) };
        unsafe {
            let _ = RegCloseKey(key);
        }
        check(code, &format!("{context} (set)"))
    }

    fn open_read(sub_key: &str) -> Option<HKEY> {
        let sub = wide(sub_key);
        let mut key = HKEY::default();
        let code = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(sub.as_ptr()), None, sam(KEY_READ.0), &mut key) };
        code.is_ok().then_some(key)
    }

    fn read_string(key: HKEY, value_name: &str) -> Option<String> {
        let name = wide(value_name);
        let mut kind = REG_VALUE_TYPE::default();
        let mut buffer = [0_u16; 1024];
        let mut len = (buffer.len() * 2) as u32;
        let code =
            unsafe { RegQueryValueExW(key, PCWSTR(name.as_ptr()), None, Some(&mut kind), Some(buffer.as_mut_ptr().cast()), Some(&mut len)) };
        if !code.is_ok() || kind != REG_SZ {
            return None;
        }
        let chars = (len as usize / 2).saturating_sub(1).min(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..chars]))
    }

    fn value_exists(sub_key: &str, value_name: &str) -> bool {
        let Some(key) = open_read(sub_key)
        else {
            return false;
        };
        let found = read_string(key, value_name).is_some();
        unsafe {
            let _ = RegCloseKey(key);
        }
        found
    }

    pub fn entry(extension: &str) -> AssociationEntry {
        let registered = value_exists(&format!("{CLASSES}\\{extension}\\OpenWithProgids"), PROG_ID);
        let is_default = open_read(&format!("{FILE_EXTS}\\{extension}\\UserChoice")).is_some_and(|key| {
            let current = read_string(key, "ProgId").as_deref() == Some(PROG_ID);
            unsafe {
                let _ = RegCloseKey(key);
            }
            current
        });
        AssociationEntry { extension: extension.to_string(), registered, is_default }
    }

    pub fn status_all() -> super::AssociationStatus {
        super::AssociationStatus {
            platform: open_zipper_core::Platform::current().name(),
            supported: true,
            prog_id: PROG_ID,
            extensions: ALL_EXTENSIONS.iter().map(|extension| entry(extension)).collect(),
        }
    }

    pub fn register(extension: &str, node_path: &str, script_path: &str, icon_path: &str) -> Result<()> {
        let progid = format!("{CLASSES}\\{PROG_ID}");
        set_string(&progid, Some("FriendlyTypeName"), "OpenZipper archive", "ProgID friendly name")?;
        set_string(&format!("{progid}\\DefaultIcon"), None, &format!("{icon_path},0"), "ProgID icon")?;
        set_string(&format!("{progid}\\shell\\open\\command"), None, &super::open_command(node_path, script_path), "open command")?;
        set_string(&format!("{CLASSES}\\{extension}\\OpenWithProgids"), Some(PROG_ID), "", "OpenWithProgids")
    }

    pub fn unregister(extension: &str) -> Result<()> {
        let progids = format!("{CLASSES}\\{extension}\\OpenWithProgids");
        let Some(key) = open_read(&progids)
        else {
            return Ok(());
        };
        let name = wide(PROG_ID);
        let code = unsafe { RegDeleteValueW(key, PCWSTR(name.as_ptr())) };
        unsafe {
            let _ = RegCloseKey(key);
        }
        check(code, &format!("remove {PROG_ID} from {extension}"))
    }

    pub fn prune_progid_if_unused(extensions: &[String]) -> Result<()> {
        let still_used = extensions.iter().any(|extension| value_exists(&format!("{CLASSES}\\{extension}\\OpenWithProgids"), PROG_ID));
        if still_used {
            return Ok(());
        }
        let progid = wide(&format!("{CLASSES}\\{PROG_ID}"));
        let code = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(progid.as_ptr())) };
        check(code, "remove ProgID tree")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_normalize_with_one_leading_dot() {
        assert_eq!(normalize_extension(" ZIP ").unwrap(), ".zip");
        assert_eq!(normalize_extension(".tar.gz").unwrap(), ".tar.gz");
        assert_eq!(normalize_extension("7z").unwrap(), ".7z");
        assert!(normalize_extension("").is_err());
        assert!(normalize_extension(".").is_err());
        assert!(normalize_extension("zip/../exe").is_err());
    }

    #[test]
    fn unknown_extensions_are_rejected() {
        let error = normalize_extensions(&[".exe".to_string()]).unwrap_err();
        assert_eq!(error.code(), "unsupported_format");
    }

    #[test]
    fn empty_request_falls_back_to_all_supported() {
        assert_eq!(normalize_extensions(&[]).unwrap().len(), ALL_EXTENSIONS.len());
    }

    #[test]
    fn open_command_quotes_every_path() {
        let command = open_command("C:\\Program Files\\node.exe", "E:\\my app\\cli.js");
        assert_eq!(command, "\"C:\\Program Files\\node.exe\" \"E:\\my app\\cli.js\" gui --open \"%1\"");
    }

    #[test]
    fn open_query_keeps_only_safe_characters() {
        let encoded = encode_open_query("C:\\Downloads\\a b.zip");
        assert_eq!(encoded, "C%3A%5CDownloads%5Ca%20b.zip");
        assert_eq!(encode_open_query("E:/data/x.7z"), "E%3A/data/x.7z");
    }
}
