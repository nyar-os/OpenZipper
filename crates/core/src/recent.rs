//! The GUI host's "recent archives" store.
//!
//! This is host state, not an archive capability: only the N-API GUI host
//! records after a successful listing, and the SDK and CLI never write here.
//! Concurrent GUI and CLI runs are last-write-wins without a file lock, which
//! is acceptable for a bounded UI list.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    error::{Error, ErrorCode, Result},
    platform::Platform,
};

/// Maximum number of remembered archives.
pub const RECENT_LIMIT: usize = 20;
const RECENT_VERSION: u32 = 1;

/// One remembered archive with the last successful open time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentItem {
    pub path: String,
    pub last_opened_unix_ms: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecentFile {
    version: u32,
    items: Vec<RecentItem>,
}

/// Absolute path of `recent.json` for the current platform and environment.
pub fn file_path(platform: Platform, env: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    crate::locations::app_dir(platform, env).map(|dir| dir.join("recent.json"))
}

fn key(path: &str, platform: Platform) -> String {
    if platform.case_insensitive_paths() { path.to_lowercase() } else { path.to_string() }
}

/// Reads the store, dropping missing files, duplicates and stale versions.
pub fn load(dir: &Path, platform: Platform) -> Vec<RecentItem> {
    load_from(&dir.join("recent.json"), platform)
}

fn load_from(file: &Path, platform: Platform) -> Vec<RecentItem> {
    let Ok(text) = fs::read_to_string(file)
    else {
        return Vec::new();
    };
    let Ok(stored) = serde_json::from_str::<RecentFile>(&text)
    else {
        return Vec::new();
    };
    if stored.version != RECENT_VERSION {
        return Vec::new();
    }
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    for item in stored.items {
        if !Path::new(&item.path).is_file() {
            continue;
        }
        if !seen.insert(key(&item.path, platform)) {
            continue;
        }
        items.push(item);
        if items.len() >= RECENT_LIMIT {
            break;
        }
    }
    items
}

/// Records one successful open: newest first, deduplicated, capped, atomic.
pub fn record(dir: &Path, archive: &Path, now_ms: i64, platform: Platform) -> Result<Vec<RecentItem>> {
    let file = dir.join("recent.json");
    let mut items = load_from(&file, platform);
    let path = archive.display().to_string();
    let insert_key = key(&path, platform);
    items.retain(|item| key(&item.path, platform) != insert_key);
    items.insert(0, RecentItem { path, last_opened_unix_ms: now_ms });
    items.truncate(RECENT_LIMIT);

    let stored = RecentFile { version: RECENT_VERSION, items: items.clone() };
    let text = serde_json::to_string_pretty(&stored).map_err(|error| Error::coded(ErrorCode::Message, format!("recent store: {error}")))?;
    fs::create_dir_all(dir).map_err(|error| Error::io(dir.display().to_string(), error))?;
    let temp = dir.join("recent.json.tmp");
    fs::write(&temp, text.as_bytes()).map_err(|error| Error::io(temp.display().to_string(), error))?;
    fs::rename(&temp, &file).map_err(|error| {
        let _ = fs::remove_file(&temp);
        Error::io(file.display().to_string(), error)
    })?;
    Ok(stored.items)
}
