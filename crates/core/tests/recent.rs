mod common;

use common::TempDir;
use open_zipper_core::{Platform, RECENT_LIMIT, RecentItem, recent};
use std::{collections::HashMap, fs, path::PathBuf};

fn now_ms(step: i64) -> i64 {
    1_700_000_000_000 + step
}

fn touch(dir: &TempDir, name: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, b"x").expect("touch file");
    path
}

#[test]
fn record_dedupes_and_keeps_the_newest_first() {
    let dir = TempDir::new("recent-record");
    let store = dir.path().to_path_buf();
    let first = touch(&dir, "first.zip");
    let second = touch(&dir, "second.zip");

    recent::record(&store, &first, now_ms(1), Platform::Linux).expect("record first");
    let items = recent::record(&store, &second, now_ms(2), Platform::Linux).expect("record second");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].path, second.display().to_string());

    let again = recent::record(&store, &first, now_ms(3), Platform::Linux).expect("re-record first");
    assert_eq!(again.len(), 2);
    assert_eq!(again[0].path, first.display().to_string());
    assert_eq!(again[0].last_opened_unix_ms, now_ms(3));
}

#[test]
fn load_prunes_missing_paths_and_caps_at_the_limit() {
    let dir = TempDir::new("recent-load");
    let store = dir.path().to_path_buf();
    let mut paths = Vec::new();
    for index in 0..(RECENT_LIMIT + 5) {
        let path = touch(&dir, &format!("archive-{index}.zip"));
        paths.push(path);
    }
    let mut last = Vec::new();
    for (index, path) in paths.iter().enumerate() {
        last = recent::record(&store, path, now_ms(index as i64), Platform::Linux).expect("record");
    }
    assert_eq!(last.len(), RECENT_LIMIT);

    let loaded = recent::load(&store, Platform::Linux);
    assert_eq!(loaded.len(), RECENT_LIMIT);
    assert_eq!(loaded[0].path, paths[paths.len() - 1].display().to_string());

    fs::remove_file(&paths[paths.len() - 1]).expect("remove newest");
    let loaded = recent::load(&store, Platform::Linux);
    assert_eq!(loaded.len(), RECENT_LIMIT - 1);
}

#[test]
fn windows_keys_fold_case_and_linux_does_not() {
    let dir = TempDir::new("recent-case");
    let store = dir.path().to_path_buf();
    let upper = touch(&dir, "Notes.ZIP");

    recent::record(&store, &upper, now_ms(1), Platform::Windows).expect("record upper");
    let items = recent::record(&store, &upper, now_ms(2), Platform::Windows).expect("re-record upper");
    assert_eq!(items.len(), 1);

    let linux_items = recent::load(&store, Platform::Linux);
    assert_eq!(linux_items.len(), 1);
}

#[test]
fn corrupted_or_stale_stores_load_empty_and_are_replaced() {
    let dir = TempDir::new("recent-corrupt");
    let store = dir.path().to_path_buf();
    fs::create_dir_all(&store).expect("create store");
    fs::write(store.join("recent.json"), b"not json at all").expect("write garbage");
    assert!(recent::load(&store, Platform::Linux).is_empty());

    fs::write(store.join("recent.json"), br#"{"version":2,"items":[]}"#).expect("write stale version");
    assert!(recent::load(&store, Platform::Linux).is_empty());

    let archive = touch(&dir, "fresh.zip");
    let items = recent::record(&store, &archive, now_ms(1), Platform::Linux).expect("record over stale");
    assert_eq!(items, vec![RecentItem { path: archive.display().to_string(), last_opened_unix_ms: now_ms(1) }]);
    assert!(!store.join("recent.json.tmp").exists(), "atomic write must not leave the temp file");
}

#[test]
fn file_path_follows_the_injected_environment() {
    let env: HashMap<String, String> =
        HashMap::from([("APPDATA".to_string(), "C:\\Users\\dev\\AppData\\Roaming".to_string()), ("HOME".to_string(), "/home/dev".to_string())]);
    let lookup = |key: &str| env.get(key).cloned();
    let windows = recent::file_path(Platform::Windows, &lookup).expect("windows path");
    assert_eq!(windows, PathBuf::from("C:\\Users\\dev\\AppData\\Roaming").join("OpenZipper").join("recent.json"));
    let linux = recent::file_path(Platform::Linux, &lookup).expect("linux path");
    assert_eq!(linux, PathBuf::from("/home/dev").join(".local").join("share").join("OpenZipper").join("recent.json"));
    assert!(recent::file_path(Platform::Other, &lookup).is_none());
}
