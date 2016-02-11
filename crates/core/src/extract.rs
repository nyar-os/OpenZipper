use std::{
    fs,
    path::{Path, PathBuf},
};

use crate::{
    error::{Error, ErrorCode, Result},
    format::{self, ArchiveFormat},
    model::{ExtractAction, ExtractOptions, ExtractOutcome, ExtractReport, OverwritePolicy},
    safety::{SafeEntryPath, join_destination},
    sevenz_codec, tar_codec, zip_codec,
};

#[derive(Debug, Default)]
pub(crate) struct ExtractContext {
    pub written_files: Vec<(String, PathBuf)>,
}

#[derive(Debug, Clone)]
pub(crate) struct Target {
    pub path: PathBuf,
    pub renamed: bool,
    pub relative_name: String,
}

pub(crate) fn decide_target(root: &Path, safe: &SafeEntryPath, policy: OverwritePolicy) -> Result<Option<Target>> {
    let target = join_destination(root, safe);
    if fs::symlink_metadata(&target).is_err() {
        return Ok(Some(Target { relative_name: relative_name(root, &target), path: target, renamed: false }));
    }
    match policy {
        OverwritePolicy::Error => {
            Err(Error::coded(ErrorCode::PathConflict, format!("`{}` already exists and the overwrite policy is `error`", target.display())))
        }
        OverwritePolicy::Skip => Ok(None),
        OverwritePolicy::Rename => {
            let parent = target.parent().unwrap_or(root);
            let stem = target.file_stem().and_then(|stem| stem.to_str()).unwrap_or("entry");
            let extension = target.extension().and_then(|extension| extension.to_str());
            for index in 1..10_000_u32 {
                let candidate_name = match extension {
                    Some(extension) => format!("{stem} ({index}).{extension}"),
                    None => format!("{stem} ({index})"),
                };
                let candidate = parent.join(&candidate_name);
                if fs::symlink_metadata(&candidate).is_err() {
                    return Ok(Some(Target { relative_name: relative_name(root, &candidate), path: candidate, renamed: true }));
                }
            }
            Err(Error::coded(ErrorCode::PathConflict, format!("cannot find an unused name for `{}`", target.display())))
        }
    }
}

fn relative_name(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

pub(crate) fn record_directory(report: &mut ExtractReport, name: &str) -> Result<()> {
    report.directories += 1;
    report.entries.push(ExtractOutcome { name: name.to_string(), action: ExtractAction::Directory, bytes_written: 0, renamed_to: None });
    Ok(())
}

pub(crate) fn record_file(
    report: &mut ExtractReport,
    name: &str,
    bytes: u64,
    action: ExtractAction,
    renamed_to: Option<String>,
    written_path: Option<PathBuf>,
    context: &mut ExtractContext,
) {
    if let Some(path) = written_path {
        context.written_files.push((name.to_string(), path));
    }
    report.bytes_written = report.bytes_written.saturating_add(bytes);
    if matches!(action, ExtractAction::Written | ExtractAction::Renamed | ExtractAction::WouldWrite) {
        report.written += 1;
    }
    report.entries.push(ExtractOutcome { name: name.to_string(), action, bytes_written: bytes, renamed_to });
}

pub fn extract(path: &Path, options: &ExtractOptions) -> Result<ExtractReport> {
    options.limits.validate()?;
    let format = format::detect(path)?;
    extract_with_depth(path, format, options, 0)
}

fn extract_with_depth(path: &Path, format: ArchiveFormat, options: &ExtractOptions, depth: u32) -> Result<ExtractReport> {
    options.limits.check_nested_depth(depth)?;
    if !options.dry_run {
        if let Ok(metadata) = fs::metadata(&options.destination)
            && !metadata.is_dir()
        {
            return Err(Error::coded(ErrorCode::InvalidOption, format!("destination `{}` is not a directory", options.destination.display())));
        }
        fs::create_dir_all(&options.destination).map_err(|error| Error::io(options.destination.display().to_string(), error))?;
    }

    let mut report = ExtractReport {
        archive: path.display().to_string(),
        destination: options.destination.display().to_string(),
        format,
        dry_run: options.dry_run,
        written: 0,
        skipped: 0,
        directories: 0,
        renamed: 0,
        bytes_written: 0,
        nested_archives: 0,
        entries: Vec::new(),
    };
    let mut context = ExtractContext::default();

    match format {
        ArchiveFormat::Zip => {
            let entries = zip_codec::inspect(path, &options.limits, true)?;
            zip_codec::extract(path, &entries, options, &mut context, &mut report)?;
        }
        ArchiveFormat::SevenZ => {
            let entries = sevenz_codec::inspect(path, &options.limits, true)?;
            sevenz_codec::extract(path, &entries, options, &mut context, &mut report)?;
        }
        _ => {
            let entries = tar_codec::inspect(path, format, &options.limits, true)?;
            tar_codec::extract(path, format, &entries, options, &mut context, &mut report)?;
        }
    }

    if options.recurse && !options.dry_run {
        for (name, written) in context.written_files.clone() {
            let Some(nested_format) = format::detect_by_name(&written.to_string_lossy())
            else {
                continue;
            };
            if depth + 1 > options.limits.max_nested_depth {
                return Err(Error::coded(
                    ErrorCode::NestedDepth,
                    format!("nested archive `{name}` would reach depth {}, above the limit of {}", depth + 1, options.limits.max_nested_depth),
                ));
            }
            let nested_destination = unique_directory(&written);
            let mut nested_options = options.clone();
            nested_options.destination = nested_destination;
            let nested = extract_with_depth(&written, nested_format, &nested_options, depth + 1)?;
            report.nested_archives += 1 + nested.nested_archives;
            report.written += nested.written;
            report.skipped += nested.skipped;
            report.directories += nested.directories;
            report.renamed += nested.renamed;
            report.bytes_written = report.bytes_written.saturating_add(nested.bytes_written);
        }
    }

    Ok(report)
}

fn unique_directory(archive: &Path) -> PathBuf {
    let base = PathBuf::from(format!("{}.extracted", archive.display()));
    if fs::symlink_metadata(&base).is_err() {
        return base;
    }
    for index in 1..10_000_u32 {
        let candidate = PathBuf::from(format!("{}.{index}", base.display()));
        if fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
    }
    base
}
