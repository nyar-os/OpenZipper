//! Per-user application directories with an injectable environment, so path
//! resolution is testable without touching the real machine.

use std::path::PathBuf;

use crate::platform::Platform;

/// The per-user directory holding OpenZipper's own state (never elevation).
pub(crate) fn app_dir(platform: Platform, env: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let non_empty = |key| env(key).map(|value| value.trim().to_string()).filter(|value| !value.is_empty());
    match platform {
        Platform::Windows => non_empty("APPDATA")
            .map(PathBuf::from)
            .or_else(|| non_empty("USERPROFILE").map(|home| PathBuf::from(home).join("AppData").join("Roaming")))
            .map(|base| base.join("OpenZipper")),
        Platform::Macos => non_empty("HOME").map(|home| PathBuf::from(home).join("Library").join("Application Support").join("OpenZipper")),
        Platform::Linux => non_empty("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| non_empty("HOME").map(|home| PathBuf::from(home).join(".local").join("share")))
            .map(|base| base.join("OpenZipper")),
        Platform::Other => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(map: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: HashMap<String, String> = map.iter().map(|(key, value)| ((*key).to_string(), (*value).to_string())).collect();
        move |key| owned.get(key).cloned()
    }

    #[test]
    fn windows_prefers_appdata_and_falls_back_to_userprofile() {
        let direct = app_dir(Platform::Windows, &env(&[("APPDATA", "C:\\Roaming\\App")]));
        assert_eq!(direct, Some(PathBuf::from("C:\\Roaming\\App").join("OpenZipper")));
        let fallback = app_dir(Platform::Windows, &env(&[("USERPROFILE", "C:\\Users\\dev")]));
        assert_eq!(fallback, Some(PathBuf::from("C:\\Users\\dev").join("AppData").join("Roaming").join("OpenZipper")));
    }

    #[test]
    fn empty_environment_values_are_not_accepted() {
        assert_eq!(app_dir(Platform::Windows, &env(&[("APPDATA", "   ")])), None);
    }

    #[test]
    fn linux_honours_xdg_data_home() {
        let resolved = app_dir(Platform::Linux, &env(&[("XDG_DATA_HOME", "/srv/data"), ("HOME", "/home/dev")]));
        assert_eq!(resolved, Some(PathBuf::from("/srv/data").join("OpenZipper")));
    }
}
