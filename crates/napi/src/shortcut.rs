//! Desktop shortcut and default-app-settings launch for the Windows host.
//!
//! The shortcut is a real `.lnk` created through `IShellLinkW`: it launches
//! `node.exe <cli.js> gui` with the app icon, and its working directory is the
//! package directory so relative content resolution never depends on where the
//! user double-clicked from. `remove_shortcut` deletes only the one `.lnk`
//! this app owns by name.

use open_zipper_core::{CapabilityState, Error as CoreError, Platform, Result};
use serde::Serialize;

/// Default display name for the desktop shortcut.
pub const DEFAULT_SHORTCUT_NAME: &str = "OpenZipper";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutResult {
    pub path: String,
    pub created: bool,
}

fn unsupported() -> CoreError {
    CoreError::capability(CapabilityState::UnsupportedPlatform, "desktop shortcuts exist only on Windows")
}

fn ensure_windows() -> Result<()> {
    if Platform::current() == Platform::Windows { Ok(()) } else { Err(unsupported()) }
}

fn shortcut_name(name: &str) -> &str {
    let trimmed = name.trim();
    if trimmed.is_empty() { DEFAULT_SHORTCUT_NAME } else { trimmed }
}

/// Creates (or replaces) the desktop shortcut.
pub fn create(name: &str, node_path: &str, script_path: &str, icon_path: &str, working_dir: &str) -> Result<ShortcutResult> {
    ensure_windows()?;
    let name = shortcut_name(name);
    #[cfg(target_os = "windows")]
    return win::create(name, node_path, script_path, icon_path, working_dir);
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (name, node_path, script_path, icon_path, working_dir);
        Err(unsupported())
    }
}

/// Deletes the desktop shortcut with this app's name, if it exists.
pub fn remove(name: &str) -> Result<ShortcutResult> {
    ensure_windows()?;
    let name = shortcut_name(name);
    #[cfg(target_os = "windows")]
    return win::remove(name);
    #[cfg(not(target_os = "windows"))]
    {
        let _ = name;
        Err(unsupported())
    }
}

/// Opens the Windows default-app settings page. The URI is a fixed constant;
/// no caller-supplied string ever reaches the shell.
pub fn open_default_apps_settings() -> Result<()> {
    ensure_windows()?;
    #[cfg(target_os = "windows")]
    return win::open_uri("ms-settings:defaultapps");
    #[cfg(not(target_os = "windows"))]
    Err(unsupported())
}

#[cfg(target_os = "windows")]
mod win {
    use std::{
        ffi::{OsStr, OsString},
        os::windows::ffi::{OsStrExt, OsStringExt},
    };

    use open_zipper_core::{Error as CoreError, ErrorCode, Result};
    use windows::{
        Win32::{
            System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree, IPersistFile},
            UI::{
                Shell::{FOLDERID_Desktop, IShellLinkW, KNOWN_FOLDER_FLAG, SHGetKnownFolderPath, ShellExecuteW},
                WindowsAndMessaging::SW_SHOWNORMAL,
            },
        },
        core::{GUID, Interface, PCWSTR},
    };

    use super::ShortcutResult;

    const CLSID_SHELL_LINK: GUID = GUID::from_u128(0x0002_13F6_0000_0000_C000_0000_0000_0046);

    fn wide(value: &str) -> Vec<u16> {
        OsStr::new(value).encode_wide().chain(std::iter::once(0)).collect()
    }

    fn desktop_dir() -> Result<std::path::PathBuf> {
        unsafe {
            let text = SHGetKnownFolderPath(&FOLDERID_Desktop, KNOWN_FOLDER_FLAG(0), None)
                .map_err(|error| CoreError::coded(ErrorCode::IoError, format!("resolve desktop folder: {error}")))?;
            let raw = text.0;
            let len = (0usize..).take_while(|index| raw.add(*index).read() != 0).count();
            let slice = std::slice::from_raw_parts(raw, len);
            let owned = OsString::from_wide(slice);
            CoTaskMemFree(Some(raw as *const core::ffi::c_void));
            Ok(std::path::PathBuf::from(owned))
        }
    }

    pub fn create(name: &str, node_path: &str, script_path: &str, icon_path: &str, working_dir: &str) -> Result<ShortcutResult> {
        let target = desktop_dir()?.join(format!("{name}.lnk"));
        let existed = target.is_file();
        let arguments = format!("\"{script_path}\" gui");
        let (node_w, args_w, work_w, icon_w, target_w, desc_w) = (
            wide(node_path),
            wide(&arguments),
            wide(working_dir),
            wide(icon_path),
            wide(&target.display().to_string()),
            wide("OpenZipper archive viewer"),
        );
        unsafe {
            let link: IShellLinkW = CoCreateInstance(&CLSID_SHELL_LINK, None, CLSCTX_INPROC_SERVER)
                .map_err(|error| CoreError::coded(ErrorCode::IoError, format!("create IShellLinkW: {error}")))?;
            link.SetPath(PCWSTR(node_w.as_ptr())).map_err(|error| shell_err("set target", error))?;
            link.SetArguments(PCWSTR(args_w.as_ptr())).map_err(|error| shell_err("set arguments", error))?;
            link.SetWorkingDirectory(PCWSTR(work_w.as_ptr())).map_err(|error| shell_err("set working dir", error))?;
            link.SetIconLocation(PCWSTR(icon_w.as_ptr()), 0).map_err(|error| shell_err("set icon", error))?;
            link.SetDescription(PCWSTR(desc_w.as_ptr())).map_err(|error| shell_err("set description", error))?;
            let persist: IPersistFile = link.cast().map_err(|error| shell_err("query IPersistFile", error))?;
            persist.Save(PCWSTR(target_w.as_ptr()), true).map_err(|error| shell_err("save shortcut", error))?;
        }
        Ok(ShortcutResult { path: target.display().to_string(), created: !existed })
    }

    fn shell_err(context: &'static str, error: windows::core::Error) -> CoreError {
        CoreError::coded(ErrorCode::IoError, format!("shortcut {context}: {error}"))
    }

    pub fn remove(name: &str) -> Result<ShortcutResult> {
        let target = desktop_dir()?.join(format!("{name}.lnk"));
        if !target.is_file() {
            return Ok(ShortcutResult { path: target.display().to_string(), created: false });
        }
        std::fs::remove_file(&target).map_err(|error| CoreError::io(target.display().to_string(), error))?;
        Ok(ShortcutResult { path: target.display().to_string(), created: false })
    }

    pub fn open_uri(uri: &str) -> Result<()> {
        let verb = wide("open");
        let target = wide(uri);
        let status =
            unsafe { ShellExecuteW(None, PCWSTR(verb.as_ptr()), PCWSTR(target.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL) };
        if status.0 as isize > 32 {
            return Ok(());
        }
        Err(CoreError::coded(ErrorCode::IoError, format!("shell open `{uri}` failed with status {}", status.0 as isize)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_names_fall_back_to_the_default() {
        assert_eq!(shortcut_name("  "), DEFAULT_SHORTCUT_NAME);
        assert_eq!(shortcut_name("OpenZipper Dev"), "OpenZipper Dev");
    }

    #[test]
    fn non_windows_hosts_refuse_before_touching_the_shell() {
        if Platform::current() != Platform::Windows {
            assert_eq!(
                create("", "node", "cli.js", "app.ico", ".").err().map(|error| error.capability_code()),
                Some(Some("unsupported_platform"))
            );
            assert_eq!(remove("").err().map(|error| error.capability_code()), Some(Some("unsupported_platform")));
        }
    }
}
