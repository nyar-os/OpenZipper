use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{
    error::{Error, ErrorCode, Result},
    platform::Platform,
    safety::validate_entry_path,
};

#[derive(Debug, Clone)]
pub(crate) struct SourceEntry {
    pub path: PathBuf,
    pub name: String,
    pub directory: bool,
    pub size: u64,
}

/// Collect regular files and directories without following symbolic links.
pub(crate) fn collect_sources(sources: &[PathBuf]) -> Result<Vec<SourceEntry>> {
    let mut collected = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for source in sources {
        let metadata = fs::symlink_metadata(source).map_err(|error| {
            let code = if error.kind() == std::io::ErrorKind::NotFound { ErrorCode::FileMissing } else { ErrorCode::IoError };
            Error::coded(code, format!("{}: {error}", source.display()))
        })?;
        if metadata.file_type().is_symlink() {
            return Err(Error::coded(ErrorCode::SymlinkEntry, format!("`{}` is a symbolic link and cannot be archived", source.display())));
        }

        let base_name = source
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| Error::coded(ErrorCode::InvalidOption, format!("`{}` has no usable file name", source.display())))?
            .to_string();

        if metadata.is_file() {
            push_entry(&mut collected, &mut seen, source.clone(), base_name, false, metadata.len())?;
        }
        else if metadata.is_dir() {
            push_entry(&mut collected, &mut seen, source.clone(), base_name.clone(), true, 0)?;
            walk_directory(source, &base_name, &mut collected, &mut seen)?;
        }
        else {
            return Err(Error::coded(ErrorCode::InvalidOption, format!("`{}` is not a file or directory", source.display())));
        }
    }

    collected.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(collected)
}

fn walk_directory(
    directory: &Path,
    prefix: &str,
    collected: &mut Vec<SourceEntry>,
    seen: &mut std::collections::HashSet<String>,
) -> Result<()> {
    let mut children: Vec<_> = fs::read_dir(directory)
        .map_err(|error| Error::io(directory.display().to_string(), error))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| Error::io(directory.display().to_string(), error))?;
    children.sort_by_key(|entry| entry.file_name());

    for child in children {
        let path = child.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| Error::io(path.display().to_string(), error))?;
        let name = format!("{prefix}/{}", child.file_name().to_string_lossy().replace('\\', "/"));
        if metadata.file_type().is_symlink() {
            return Err(Error::coded(ErrorCode::SymlinkEntry, format!("`{}` is a symbolic link and cannot be archived", path.display())));
        }
        if metadata.is_dir() {
            push_entry(collected, seen, path.clone(), name.clone(), true, 0)?;
            walk_directory(&path, &name, collected, seen)?;
        }
        else if metadata.is_file() {
            push_entry(collected, seen, path, name, false, metadata.len())?;
        }
        else {
            return Err(Error::coded(ErrorCode::InvalidOption, format!("`{}` is not a regular file or directory", path.display())));
        }
    }
    Ok(())
}

fn push_entry(
    collected: &mut Vec<SourceEntry>,
    seen: &mut std::collections::HashSet<String>,
    path: PathBuf,
    name: String,
    directory: bool,
    size: u64,
) -> Result<()> {
    let safe = validate_entry_path(&name, Platform::current())?;
    if !seen.insert(safe.normalized_key.clone()) {
        return Err(Error::coded(ErrorCode::PathConflict, format!("source name `{name}` collides with another source")));
    }
    collected.push(SourceEntry { path, name: safe.display(), directory, size });
    Ok(())
}
