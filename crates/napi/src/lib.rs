//! Node-API surface for `@wae/open-zipper` through the WAE facade.
//!
//! This crate is the only implementation path for archive work. The CLI and the
//! GUI both consume it, and every call is audited. Archive parsing, path
//! validation, extraction and creation live in `open-zipper-core`; no child
//! process or shell pipeline is ever used here.

mod audit;

use std::{
    cell::RefCell,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use open_zipper_core::{
    ArchiveLimits, CapabilityState, CreateOptions, DEFAULT_PREVIEW_CAP, Error as CoreError, ErrorCode, ExtractOptions, ExtractedEntryFile,
    OverwritePolicy, Platform, PreviewResult, capabilities, create, extract, extract_entry, list, preview, recent, test,
};
use serde::Serialize;
use serde_json::{Value, json};
use wae::{
    CallContext, Env, Error, JsValue, Result,
    audit::{Actor, ActorKind, Mode, Query},
    function, optional_string_prop, register_module, sys,
    webview::{IpcHandler, IpcOutcome, OpenOptions, WebViewError, WebViewHost},
};

const APP_TITLE: &str = "OpenZipper";

/// Stable GUI event codes returned by `pumpEvents`.
const GUI_CLOSED: &str = "gui_closed";
const GUI_HOST_EXITED: &str = "gui_host_exited";
const GUI_CLOSE_FAILED: &str = "gui_close_failed";

/// Code for a `window.secondLaunch` value the host does not understand.
const WINDOW_CONFIG_INVALID: &str = "window_config_invalid";

thread_local! {
    static GUI: RefCell<Option<GuiState>> = const { RefCell::new(None) };
}

struct GuiState {
    host: WebViewHost,
    backend: &'static str,
    opened_at: Instant,
    /// Set when the page asks the host to close the window, so a dead host can
    /// be told apart from a host that never processed the request.
    close_requested: Arc<AtomicBool>,
}

/// What `openGui` did: open a fresh host or focus the live one.
enum GuiOpenAction {
    Opened(GuiState),
    Focused { backend: &'static str, uptime_ms: u64 },
}

/// Lifecycle of one GUI host. Failure codes are stable for scripts and audit.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GuiStatus {
    state: &'static str,
    code: Option<&'static str>,
    backend: Option<&'static str>,
    platform: &'static str,
    uptime_ms: u64,
    message: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GuiOpenResult {
    ok: bool,
    #[serde(flatten)]
    status: GuiStatus,
    stage: Option<&'static str>,
    hresult: Option<i32>,
    attempted: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GuiCloseResult {
    closed: bool,
    #[serde(flatten)]
    status: GuiStatus,
}

struct ZipperIpcHandler {
    close_requested: Arc<AtomicBool>,
    /// Directory this host session materializes single entries into for
    /// "open with the default program"; the page never names a destination.
    temp_dir: PathBuf,
}

impl IpcHandler for ZipperIpcHandler {
    fn on_ipc(&mut self, message: &str) -> IpcOutcome {
        let Ok(value) = serde_json::from_str::<Value>(message)
        else {
            return IpcOutcome::Ignored;
        };
        let ty = value.get("type").and_then(Value::as_str).unwrap_or("");
        let id = value.get("id").cloned().unwrap_or(Value::Null);
        match ty {
            "close" => {
                self.close_requested.store(true, Ordering::SeqCst);
                IpcOutcome::CloseWindow
            }
            "capabilities" => match audited_capabilities() {
                Ok(report) => IpcOutcome::ReplyToPage(json!({ "type": "capabilitiesResult", "id": id, "capabilities": report }).to_string()),
                Err(error) => {
                    IpcOutcome::ReplyToPage(json!({ "type": "capabilitiesError", "id": id, "message": error.to_string() }).to_string())
                }
            },
            "recent" => match audited_recent() {
                Ok(items) => IpcOutcome::ReplyToPage(json!({ "type": "recentResult", "id": id, "items": items }).to_string()),
                Err(error) => IpcOutcome::ReplyToPage(json!({ "type": "recentError", "id": id, "message": error.to_string() }).to_string()),
            },
            "preview" => {
                let path = value.get("path").and_then(Value::as_str).unwrap_or("").to_string();
                let index = value.get("index").and_then(Value::as_u64).map(|index| index as usize).unwrap_or(usize::MAX);
                let cap_bytes = value.get("capBytes").and_then(Value::as_u64).unwrap_or(DEFAULT_PREVIEW_CAP);
                let limits = limits_from_json(value.get("limits"));
                if path.trim().is_empty() {
                    return IpcOutcome::ReplyToPage(
                        json!({ "type": "previewError", "id": id, "message": "preview requires a path" }).to_string(),
                    );
                }
                match audited_preview(path, index, cap_bytes, limits) {
                    Ok(result) => IpcOutcome::ReplyToPage(
                        json!({ "type": "previewResult", "id": id, "preview": serde_json::to_value(&result).unwrap_or(Value::Null) })
                            .to_string(),
                    ),
                    Err(error) => {
                        IpcOutcome::ReplyToPage(json!({ "type": "previewError", "id": id, "message": error.to_string() }).to_string())
                    }
                }
            }
            "openEntry" => {
                let path = value.get("path").and_then(Value::as_str).unwrap_or("").to_string();
                let index = value.get("index").and_then(Value::as_u64).map(|index| index as usize).unwrap_or(usize::MAX);
                let limits = limits_from_json(value.get("limits"));
                if path.trim().is_empty() {
                    return IpcOutcome::ReplyToPage(
                        json!({ "type": "openEntryError", "id": id, "message": "openEntry requires a path" }).to_string(),
                    );
                }
                match audited_open_entry(path, index, &self.temp_dir, limits) {
                    Ok(file) => IpcOutcome::ReplyToPage(
                        json!({ "type": "openEntryResult", "id": id, "file": serde_json::to_value(&file).unwrap_or(Value::Null) }).to_string(),
                    ),
                    Err(error) => {
                        IpcOutcome::ReplyToPage(json!({ "type": "openEntryError", "id": id, "message": error.to_string() }).to_string())
                    }
                }
            }
            "list" => {
                let path = value.get("path").and_then(Value::as_str).unwrap_or("").to_string();
                let limits = limits_from_json(value.get("limits"));
                match audited_list(path.clone(), limits) {
                    Ok(report) => {
                        record_recent_open(&path);
                        IpcOutcome::ReplyToPage(
                            json!({ "type": "listResult", "id": id, "listing": serde_json::to_value(&report).unwrap_or(Value::Null) })
                                .to_string(),
                        )
                    }
                    Err(error) => IpcOutcome::ReplyToPage(json!({ "type": "listError", "id": id, "message": error.to_string() }).to_string()),
                }
            }
            "extract" => {
                let path = value.get("path").and_then(Value::as_str).unwrap_or("").to_string();
                let destination = value.get("destination").and_then(Value::as_str).unwrap_or("").to_string();
                let overwrite = value.get("overwrite").and_then(Value::as_str).unwrap_or("error").to_string();
                let dry_run = value.get("dryRun").and_then(Value::as_bool).unwrap_or(false);
                let recurse = value.get("recurse").and_then(Value::as_bool).unwrap_or(false);
                let limits = limits_from_json(value.get("limits"));
                let options = match extract_options(destination, &overwrite, dry_run, recurse, limits) {
                    Ok(options) => options,
                    Err(error) => {
                        return IpcOutcome::ReplyToPage(json!({ "type": "extractError", "id": id, "message": error.to_string() }).to_string());
                    }
                };
                match audited_extract(path, options) {
                    Ok(report) => IpcOutcome::ReplyToPage(
                        json!({ "type": "extractResult", "id": id, "report": serde_json::to_value(&report).unwrap_or(Value::Null) })
                            .to_string(),
                    ),
                    Err(error) => {
                        IpcOutcome::ReplyToPage(json!({ "type": "extractError", "id": id, "message": error.to_string() }).to_string())
                    }
                }
            }
            "test" => {
                let path = value.get("path").and_then(Value::as_str).unwrap_or("").to_string();
                let limits = limits_from_json(value.get("limits"));
                match audited_test(path, limits) {
                    Ok(report) => IpcOutcome::ReplyToPage(
                        json!({ "type": "testResult", "id": id, "report": serde_json::to_value(&report).unwrap_or(Value::Null) }).to_string(),
                    ),
                    Err(error) => IpcOutcome::ReplyToPage(json!({ "type": "testError", "id": id, "message": error.to_string() }).to_string()),
                }
            }
            "create" => {
                let destination = value.get("destination").and_then(Value::as_str).unwrap_or("").to_string();
                let sources: Vec<PathBuf> = value
                    .get("sources")
                    .and_then(Value::as_array)
                    .map(|items| items.iter().filter_map(Value::as_str).map(PathBuf::from).collect())
                    .unwrap_or_default();
                let format = value.get("format").and_then(Value::as_str).map(str::to_string);
                let force = value.get("force").and_then(Value::as_bool).unwrap_or(false);
                let limits = limits_from_json(value.get("limits"));
                if destination.trim().is_empty() || sources.is_empty() {
                    return IpcOutcome::ReplyToPage(
                        json!({ "type": "createError", "id": id, "message": "create requires a destination and at least one source path" })
                            .to_string(),
                    );
                }
                match audited_create(destination, sources, format, force, limits) {
                    Ok(report) => IpcOutcome::ReplyToPage(
                        json!({ "type": "createResult", "id": id, "report": serde_json::to_value(&report).unwrap_or(Value::Null) }).to_string(),
                    ),
                    Err(error) => IpcOutcome::ReplyToPage(json!({ "type": "createError", "id": id, "message": error.to_string() }).to_string()),
                }
            }
            _ => IpcOutcome::Ignored,
        }
    }
}

fn core_error(error: CoreError) -> Error {
    Error::new(error.to_string())
}

fn webview_error(error: &WebViewError) -> CoreError {
    if error.capability().is_some() {
        return CoreError::capability(CapabilityState::UnsupportedPlatform, error);
    }
    CoreError::coded(ErrorCode::GuiOpenFailed, error.to_string())
}

fn extract_options(destination: String, overwrite: &str, dry_run: bool, recurse: bool, limits: ArchiveLimits) -> Result<ExtractOptions> {
    if destination.trim().is_empty() {
        return Err(Error::new("extract requires a destination directory"));
    }
    let overwrite = OverwritePolicy::from_name(overwrite).map_err(core_error)?;
    Ok(ExtractOptions { destination: PathBuf::from(destination), overwrite, dry_run, recurse, limits })
}

fn audited_list(path: String, limits: ArchiveLimits) -> Result<open_zipper_core::ArchiveListing> {
    let params = json!({ "archive": path.clone(), "limits": limits });
    audit::run("list", Mode::DryRun, params, || {
        let listing = list(Path::new(&path), &limits)?;
        let count = listing.entry_count;
        let summary = format!("{count} entries, {} bytes", listing.total_size_bytes);
        Ok((listing, audit::Summary::new(count as u64, summary)))
    })
    .map_err(core_error)
}

fn audited_extract(path: String, options: ExtractOptions) -> Result<open_zipper_core::ExtractReport> {
    let mode = if options.dry_run { Mode::DryRun } else { Mode::Execute };
    let params = json!({
        "archive": path.clone(),
        "destination": options.destination.display().to_string(),
        "overwrite": options.overwrite,
        "recurse": options.recurse,
        "limits": options.limits,
    });
    audit::run("extract", mode, params, || {
        let report = extract(Path::new(&path), &options)?;
        let count = report.written;
        let summary =
            format!("{count} written, {} skipped, {} directories, {} bytes", report.skipped, report.directories, report.bytes_written);
        Ok((report, audit::Summary::new(count as u64, summary)))
    })
    .map_err(core_error)
}

fn audited_create(
    destination: String,
    sources: Vec<PathBuf>,
    format: Option<String>,
    force: bool,
    limits: ArchiveLimits,
) -> Result<open_zipper_core::CreateReport> {
    let params = json!({
        "archive": destination.clone(),
        "sources": sources.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
        "force": force,
        "limits": limits,
    });
    audit::run("create", Mode::Execute, params, || {
        let options = CreateOptions { force, limits };
        let report = create(&sources, Path::new(&destination), format.as_deref(), &options)?;
        let count = report.entry_count;
        let summary = format!("{count} entries, {} bytes", report.archive_bytes);
        Ok((report, audit::Summary::new(count as u64, summary)))
    })
    .map_err(core_error)
}

fn audited_test(path: String, limits: ArchiveLimits) -> Result<open_zipper_core::TestReport> {
    let params = json!({ "archive": path.clone(), "limits": limits });
    audit::run("test", Mode::DryRun, params, || {
        let report = test(Path::new(&path), &limits)?;
        let count = report.entry_count;
        let summary = format!("{count} entries, {} bytes verified", report.total_bytes);
        Ok((report, audit::Summary::new(count as u64, summary)))
    })
    .map_err(core_error)
}

/// Reports the core capability table plus the host-provided entries the page
/// shows, so `shell-open` and `file-drop` follow the same shape as archive rows.
fn audited_capabilities() -> Result<Value> {
    audit::run("capabilities", Mode::DryRun, Value::Null, || {
        let report = capabilities();
        let mut value = serde_json::to_value(&report).map_err(|error| CoreError::message(error.to_string()))?;
        let windows = report.platform == Platform::Windows;
        if let Some(entries) = value.get_mut("entries").and_then(Value::as_array_mut) {
            entries.push(json!({
                "capability": "shell-open",
                "available": true,
                "detail": "entries are opened with the system default program through the WAE shell facade",
            }));
            entries.push(if windows {
                json!({
                    "capability": "file-drop",
                    "available": true,
                    "detail": "files dropped on the window are delivered to the page with full paths",
                })
            }
            else {
                json!({
                    "capability": "file-drop",
                    "available": false,
                    "code": "unsupported_platform",
                    "detail": "the file drop bridge is wired for Windows only in this build",
                })
            });
        }
        let count = report.entries.len() as u64;
        Ok((value, audit::Summary::new(count, format!("platform {:?}, {count} capabilities", report.platform))))
    })
    .map_err(core_error)
}

/// Reads the recent-archive store for the GUI welcome surface.
fn audited_recent() -> Result<Vec<recent::RecentItem>> {
    audit::run("recent", Mode::DryRun, Value::Null, || {
        let (dir, platform) =
            recent_store().ok_or_else(|| CoreError::coded(ErrorCode::InvalidOption, "no per-user directory for the recent store"))?;
        let items = recent::load(&dir, platform);
        let count = items.len();
        Ok((items, audit::Summary::new(count as u64, format!("{count} recent archives"))))
    })
    .map_err(core_error)
}

/// Reads one bounded entry payload for the preview pane.
fn audited_preview(path: String, index: usize, cap_bytes: u64, limits: ArchiveLimits) -> Result<PreviewResult> {
    let params = json!({ "archive": path.clone(), "index": index, "capBytes": cap_bytes });
    audit::run("preview", Mode::DryRun, params, || {
        let result = preview(Path::new(&path), index, cap_bytes, &limits)?;
        let summary = format!("{:?}: {} of {} bytes of entry `{}`", result.kind, result.previewed_bytes, result.total_bytes, result.name);
        Ok((result, audit::Summary::new(1, summary)))
    })
    .map_err(core_error)
}

/// Materializes one entry under the host-managed temp directory and launches
/// the system default program. The page only names the archive and the index.
fn audited_open_entry(path: String, index: usize, temp_dir: &Path, limits: ArchiveLimits) -> Result<ExtractedEntryFile> {
    let params = json!({ "archive": path.clone(), "index": index, "tempDir": temp_dir.display().to_string() });
    audit::run("open-entry", Mode::Execute, params, || {
        fs::create_dir_all(temp_dir).map_err(|error| CoreError::io(temp_dir.display().to_string(), error))?;
        let file = extract_entry(Path::new(&path), index, temp_dir, &limits)?;
        wae::shell::open_path(Path::new(&file.path))
            .map_err(|error| CoreError::coded(ErrorCode::Message, format!("shell open failed: {error}")))?;
        Ok((file.clone(), audit::Summary::new(1, format!("opened `{}` externally", file.name))))
    })
    .map_err(core_error)
}

fn recent_store() -> Option<(PathBuf, Platform)> {
    let platform = Platform::current();
    let file = recent::file_path(platform, &|key| std::env::var(key).ok())?;
    Some((file.parent()?.to_path_buf(), platform))
}

/// Records a successful GUI listing as a recent open. UI state must never
/// change the listing result, so every failure here is silently ignored.
fn record_recent_open(path: &str) {
    let Some((dir, platform)) = recent_store()
    else {
        return;
    };
    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|value| value.as_millis() as i64).unwrap_or_default();
    let _ = recent::record(&dir, Path::new(path), now_ms, platform);
}

/// The per-process directory external open writes into.
fn preview_temp_dir() -> PathBuf {
    std::env::temp_dir().join("OpenZipper").join(format!("preview-{}", std::process::id()))
}

/// Best-effort sweep of preview temp directories older than a day. Only the
/// host's own `preview-*` children under `<temp>/OpenZipper` are removed.
fn sweep_preview_temp(current: &Path) {
    let Some(parent) = current.parent()
    else {
        return;
    };
    let Ok(read_dir) = fs::read_dir(parent)
    else {
        return;
    };
    let cutoff = SystemTime::now() - Duration::from_secs(24 * 60 * 60);
    for entry in read_dir.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().strip_prefix("preview-").is_none() {
            continue;
        }
        if current.file_name().is_some_and(|keep| name == keep) {
            continue;
        }
        let Ok(metadata) = entry.metadata()
        else {
            continue;
        };
        if metadata.modified().ok().is_some_and(|modified| modified < cutoff) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

fn json_to_js(env: Env, value: &Value) -> Result<JsValue> {
    match value {
        Value::Null => env.null(),
        Value::Bool(flag) => env.boolean(*flag),
        Value::Number(number) => match number.as_i64() {
            Some(int) => env.int64(int),
            None => env.double(number.as_f64().unwrap_or(0.0)),
        },
        Value::String(text) => env.string(text),
        Value::Array(items) => {
            let array = env.array_with_length(items.len())?;
            for (index, item) in items.iter().enumerate() {
                array.set_element(index as u32, json_to_js(env, item)?)?;
            }
            Ok(array)
        }
        Value::Object(map) => {
            let object = env.object()?;
            for (key, item) in map {
                object.set_named(key, json_to_js(env, item)?)?;
            }
            Ok(object)
        }
    }
}

fn value_to_js(env: Env, value: impl serde::Serialize) -> Result<JsValue> {
    let value = serde_json::to_value(value).map_err(|error| Error::new(error.to_string()))?;
    json_to_js(env, &value)
}

fn optional_string_prop_of(arg: Option<JsValue>, name: &str) -> Result<Option<String>> {
    match arg {
        Some(value) if !value.is_undefined()? => optional_string_prop(value, name),
        _ => Ok(None),
    }
}

fn required_string_prop(arg: Option<JsValue>, name: &str, caller: &str) -> Result<String> {
    let Some(value) = arg
    else {
        return Err(Error::new(format!("{caller} expects an options object with `{name}`")));
    };
    if value.is_undefined()? {
        return Err(Error::new(format!("{caller} expects an options object with `{name}`")));
    }
    let text = optional_string_prop(value, name)?.ok_or_else(|| Error::new(format!("{caller} requires `{name}`")))?;
    if text.trim().is_empty() {
        return Err(Error::new(format!("{caller} requires a non-empty `{name}`")));
    }
    Ok(text)
}

fn bool_prop(arg: Option<JsValue>, name: &str) -> Result<bool> {
    let Some(value) = arg
    else {
        return Ok(false);
    };
    if value.is_undefined()? {
        return Ok(false);
    }
    match value.get_named(name) {
        Ok(inner) if inner.type_of()? == sys::napi_boolean => inner.as_bool(),
        _ => Ok(false),
    }
}

fn number_prop(arg: Option<JsValue>, name: &str, default: u64) -> Result<u64> {
    let Some(value) = arg
    else {
        return Ok(default);
    };
    if value.is_undefined()? {
        return Ok(default);
    }
    match value.get_named(name) {
        Ok(inner) if inner.type_of()? == sys::napi_number => {
            let raw = inner.as_i64()?;
            if raw < 0 {
                return Err(Error::new(format!("`{name}` must not be negative")));
            }
            Ok(raw as u64)
        }
        _ => Ok(default),
    }
}

fn limits_from_js(arg: Option<JsValue>) -> Result<ArchiveLimits> {
    let mut limits = ArchiveLimits::default();
    let Some(value) = arg
    else {
        return Ok(limits);
    };
    if value.is_undefined()? {
        return Ok(limits);
    }
    let object = match value.get_named("limits") {
        Ok(inner) if inner.type_of()? == sys::napi_object => inner,
        _ => return Ok(limits),
    };
    limits.max_entries = usize::try_from(number_prop(Some(object), "maxEntries", limits.max_entries as u64)?).unwrap_or(limits.max_entries);
    limits.max_total_bytes = number_prop(Some(object), "maxTotalBytes", limits.max_total_bytes)?;
    limits.max_entry_bytes = number_prop(Some(object), "maxEntryBytes", limits.max_entry_bytes)?;
    limits.max_compression_ratio = u32::try_from(number_prop(Some(object), "maxCompressionRatio", u64::from(limits.max_compression_ratio))?)
        .unwrap_or(limits.max_compression_ratio);
    limits.max_nested_depth =
        u32::try_from(number_prop(Some(object), "maxNestedDepth", u64::from(limits.max_nested_depth))?).unwrap_or(limits.max_nested_depth);
    Ok(limits)
}

fn limits_from_json(value: Option<&Value>) -> ArchiveLimits {
    let mut limits = ArchiveLimits::default();
    let Some(object) = value.and_then(Value::as_object)
    else {
        return limits;
    };
    if let Some(number) = object.get("maxEntries").and_then(Value::as_u64) {
        limits.max_entries = number as usize;
    }
    if let Some(number) = object.get("maxTotalBytes").and_then(Value::as_u64) {
        limits.max_total_bytes = number;
    }
    if let Some(number) = object.get("maxEntryBytes").and_then(Value::as_u64) {
        limits.max_entry_bytes = number;
    }
    if let Some(number) = object.get("maxCompressionRatio").and_then(Value::as_u64) {
        limits.max_compression_ratio = number as u32;
    }
    if let Some(number) = object.get("maxNestedDepth").and_then(Value::as_u64) {
        limits.max_nested_depth = number as u32;
    }
    limits
}

fn sources_from_js(arg: Option<JsValue>) -> Result<Vec<String>> {
    let Some(value) = arg
    else {
        return Err(Error::new("create expects an options object with `sources`"));
    };
    if value.is_undefined()? {
        return Err(Error::new("create expects an options object with `sources`"));
    }
    let array = value.get_named("sources").map_err(|_| Error::new("create requires `sources`"))?;
    let length_value = array.get_named("length").map_err(|_| Error::new("`sources` must be an array of paths"))?;
    let length = length_value.as_i64().map_err(|_| Error::new("`sources` must be an array of paths"))?;
    if length <= 0 {
        return Err(Error::new("`sources` must contain at least one path"));
    }
    let mut sources = Vec::with_capacity(length as usize);
    for index in 0..length {
        let item = array.get_named(&index.to_string())?;
        sources.push(item.as_string()?);
    }
    Ok(sources)
}

fn resolve_gui_dir() -> Result<PathBuf> {
    if let Ok(value) = std::env::var("OPEN_ZIPPER_GUI") {
        let path = PathBuf::from(value);
        if path.is_dir() {
            return Ok(path);
        }
        if path.is_file()
            && let Some(parent) = path.parent()
        {
            return Ok(parent.to_path_buf());
        }
    }

    let manifest_relative =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("packages").join("open-zipper").join("dist").join("gui");
    if manifest_relative.join("index.html").is_file() {
        return Ok(manifest_relative);
    }

    if let Ok(cwd) = std::env::current_dir() {
        let candidates = [cwd.join("packages").join("open-zipper").join("dist").join("gui"), cwd.join("dist").join("gui"), cwd.join("gui")];
        for candidate in candidates {
            if candidate.join("index.html").is_file() {
                return Ok(candidate);
            }
        }
    }

    Err(Error::new("dist/gui folder not found, pass contentDir to openGui() or set OPEN_ZIPPER_GUI"))
}

fn actor_from_js(arg: Option<JsValue>) -> Result<Actor> {
    let mut actor = Actor::from_env();
    let Some(value) = arg
    else {
        return Ok(actor);
    };
    if value.is_undefined()? {
        return Ok(actor);
    }
    if let Some(kind) = optional_string_prop(value, "actorKind")? {
        actor.kind = if kind.eq_ignore_ascii_case("agent") { ActorKind::Agent } else { ActorKind::User };
    }
    if let Some(id) = optional_string_prop(value, "actorId")?
        && !id.trim().is_empty()
    {
        actor.id = id;
    }
    if let Some(source) = optional_string_prop(value, "actorSource")?
        && !source.trim().is_empty()
    {
        actor.source = source;
    }
    Ok(actor)
}

#[function(js_name = "listEntries")]
fn list_entries_js(ctx: CallContext) -> Result<JsValue> {
    let path = required_string_prop(ctx.arg(0).ok(), "path", "listEntries")?;
    let limits = limits_from_js(ctx.arg(0).ok())?;
    let listing = audited_list(path, limits)?;
    value_to_js(ctx.env(), &listing)
}

#[function(js_name = "extract")]
fn extract_js(ctx: CallContext) -> Result<JsValue> {
    let arg = ctx.arg(0).ok();
    let path = required_string_prop(arg, "path", "extract")?;
    let destination = required_string_prop(arg, "destination", "extract")?;
    let overwrite = optional_string_prop_of(arg, "overwrite")?.unwrap_or_else(|| "error".to_string());
    let dry_run = bool_prop(arg, "dryRun")?;
    let recurse = bool_prop(arg, "recurse")?;
    let limits = limits_from_js(arg)?;
    let options = extract_options(destination, &overwrite, dry_run, recurse, limits)?;
    let report = audited_extract(path, options)?;
    value_to_js(ctx.env(), &report)
}

#[function(js_name = "create")]
fn create_js(ctx: CallContext) -> Result<JsValue> {
    let arg = ctx.arg(0).ok();
    let destination = required_string_prop(arg, "destination", "create")?;
    let sources = sources_from_js(arg)?;
    let format = optional_string_prop_of(arg, "format")?;
    let force = bool_prop(arg, "force")?;
    let limits = limits_from_js(arg)?;
    let report = audited_create(destination, sources.into_iter().map(PathBuf::from).collect(), format, force, limits)?;
    value_to_js(ctx.env(), &report)
}

#[function(js_name = "testArchive")]
fn test_archive_js(ctx: CallContext) -> Result<JsValue> {
    let path = required_string_prop(ctx.arg(0).ok(), "path", "testArchive")?;
    let limits = limits_from_js(ctx.arg(0).ok())?;
    let report = audited_test(path, limits)?;
    value_to_js(ctx.env(), &report)
}

#[function(js_name = "previewEntry")]
fn preview_entry_js(ctx: CallContext) -> Result<JsValue> {
    let arg = ctx.arg(0).ok();
    let path = required_string_prop(arg, "path", "previewEntry")?;
    let Some(value) = arg
    else {
        return Err(Error::new("previewEntry expects an options object with `path` and `index`"));
    };
    let index_value = value.get_named("index").map_err(|_| Error::new("previewEntry requires `index`"))?;
    if index_value.is_undefined()? {
        return Err(Error::new("previewEntry requires `index`"));
    }
    let raw_index = index_value.as_i64()?;
    if raw_index < 0 {
        return Err(Error::new("`index` must not be negative"));
    }
    let cap_bytes = number_prop(arg, "capBytes", DEFAULT_PREVIEW_CAP)?;
    let limits = limits_from_js(arg)?;
    let result = audited_preview(path, raw_index as usize, cap_bytes, limits)?;
    value_to_js(ctx.env(), &result)
}

#[function]
fn version(ctx: CallContext) -> Result<JsValue> {
    let version = env!("CARGO_PKG_VERSION");
    audit::run("version", Mode::DryRun, Value::Null, || Ok(((), audit::Summary::new(0, format!("version {version}"))))).map_err(core_error)?;
    ctx.env().string(version)
}

#[function(js_name = "capabilities")]
fn capabilities_js(ctx: CallContext) -> Result<JsValue> {
    let report = audit::run("capabilities", Mode::DryRun, Value::Null, || {
        let report = capabilities();
        let count = report.entries.len() as u64;
        let summary = format!("platform {:?}, {count} capabilities", report.platform);
        Ok((report, audit::Summary::new(count, summary)))
    })
    .map_err(core_error)?;
    value_to_js(ctx.env(), &report)
}

#[function(js_name = "openGui")]
fn open_gui(ctx: CallContext) -> Result<JsValue> {
    let platform = Platform::current().name();
    let mut title = APP_TITLE.to_string();
    let mut content_dir: Option<String> = None;
    let mut url: Option<String> = None;
    let mut width = 1120;
    let mut height = 780;
    let mut second_launch = "focus".to_string();

    if let Ok(arg) = ctx.arg(0)
        && !arg.is_undefined()?
    {
        if let Some(value) = optional_string_prop(arg, "title")? {
            title = value;
        }
        content_dir = optional_string_prop(arg, "contentDir")?;
        url = optional_string_prop(arg, "url")?;
        if let Some(value) = optional_string_prop(arg, "secondLaunch")? {
            if value != "focus" && value != "reject" {
                let message = format!("`secondLaunch` must be `focus` or `reject`, got `{value}`");
                return value_to_js(ctx.env(), gui_open_failure(platform, WINDOW_CONFIG_INVALID, message));
            }
            second_launch = value;
        }
        if let Ok(value) = arg.get_named("width")
            && value.type_of()? == sys::napi_number
        {
            width = value.as_i64()? as i32;
        }
        if let Ok(value) = arg.get_named("height")
            && value.type_of()? == sys::napi_number
        {
            height = value.as_i64()? as i32;
        }
    }

    let params = json!({ "title": title.clone(), "contentDir": content_dir.clone(), "url": url.clone() });
    let mut failure: Option<WebViewError> = None;
    let outcome = audit::run("gui-open", Mode::Execute, params, || {
        GUI.with(|slot| {
            let mut guard = slot.borrow_mut();
            if let Some(existing) = guard.as_mut()
                && existing.host.is_alive()
            {
                if second_launch == "reject" {
                    return Err(CoreError::coded(
                        ErrorCode::WindowSecondLaunchRejected,
                        "a window is already open and `window.secondLaunch` is `reject`",
                    ));
                }
                existing.host.focus().map_err(|error| {
                    let core = webview_error(&error);
                    failure = Some(error);
                    core
                })?;
                let backend = existing.backend;
                let uptime_ms = existing.opened_at.elapsed().as_millis() as u64;
                let summary = format!("window focused with {backend} backend");
                return Ok((GuiOpenAction::Focused { backend, uptime_ms }, audit::Summary::new(1, summary)));
            }
            // A host left in the slot without a live window is stale; dropping it
            // keeps `openGui` usable after an unexpected host exit.
            *guard = None;
            let options = if let Some(dir) = content_dir.clone() {
                OpenOptions::from_content_dir(dir, title.clone())
            }
            else if let Some(url) = url.clone() {
                OpenOptions::from_url(url, title.clone())
            }
            else {
                let dir =
                    resolve_gui_dir().map_err(|error| CoreError::coded(ErrorCode::GuiOpenFailed, format!("gui content directory: {error}")))?;
                OpenOptions::from_content_dir(dir.display().to_string(), title.clone())
            };
            let mut options = options;
            options.width = width;
            options.height = height;
            options.accept_file_drop = true;
            let close_requested = Arc::new(AtomicBool::new(false));
            let host = WebViewHost::open(
                options,
                Box::new(ZipperIpcHandler { close_requested: Arc::clone(&close_requested), temp_dir: preview_temp_dir() }),
            )
            .map_err(|error| {
                let core = webview_error(&error);
                failure = Some(error);
                core
            })?;
            let backend = host.backend_name();
            sweep_preview_temp(&preview_temp_dir());
            let state = GuiState { host, backend, opened_at: Instant::now(), close_requested };
            let summary = format!("window opened with {backend} backend");
            Ok((GuiOpenAction::Opened(state), audit::Summary::new(1, summary)))
        })
    });

    let result = match outcome {
        Ok(GuiOpenAction::Opened(state)) => {
            let backend = state.backend;
            GUI.with(|slot| *slot.borrow_mut() = Some(state));
            GuiOpenResult {
                ok: true,
                status: GuiStatus { state: "open", code: None, backend: Some(backend), platform, uptime_ms: 0, message: None },
                stage: None,
                hresult: None,
                attempted: Vec::new(),
            }
        }
        Ok(GuiOpenAction::Focused { backend, uptime_ms }) => GuiOpenResult {
            ok: true,
            status: GuiStatus { state: "open", code: None, backend: Some(backend), platform, uptime_ms, message: None },
            stage: None,
            hresult: None,
            attempted: Vec::new(),
        },
        Err(error) => {
            let (code, stage, hresult, attempted) = match &failure {
                Some(failure) => (failure.code().unwrap_or(error.code()), failure.stage(), failure.hresult(), failure_attempted(failure)),
                None => (error.code(), None, None, Vec::new()),
            };
            GuiOpenResult {
                ok: false,
                status: GuiStatus {
                    state: "failed",
                    code: Some(code),
                    backend: None,
                    platform,
                    uptime_ms: 0,
                    message: Some(error.to_string()),
                },
                stage,
                hresult,
                attempted,
            }
        }
    };
    value_to_js(ctx.env(), &result)
}

fn failure_attempted(failure: &WebViewError) -> Vec<String> {
    match failure {
        WebViewError::Host(host) => host.attempted.clone(),
        _ => Vec::new(),
    }
}

/// Structured failure returned before a host is touched, for policy violations.
fn gui_open_failure(platform: &'static str, code: &'static str, message: String) -> GuiOpenResult {
    GuiOpenResult {
        ok: false,
        status: GuiStatus { state: "failed", code: Some(code), backend: None, platform, uptime_ms: 0, message: Some(message) },
        stage: None,
        hresult: None,
        attempted: Vec::new(),
    }
}

#[function(js_name = "pumpEvents")]
fn pump_events(ctx: CallContext) -> Result<JsValue> {
    let platform = Platform::current().name();
    let status = audit::run("gui-pump", Mode::Execute, Value::Null, || {
        let status = GUI.with(|slot| {
            let mut guard = slot.borrow_mut();
            let Some(state) = guard.as_mut()
            else {
                return GuiStatus {
                    state: "closed",
                    code: Some(ErrorCode::GuiNotOpen.code()),
                    backend: None,
                    platform,
                    uptime_ms: 0,
                    message: Some("no zipper window is open".to_string()),
                };
            };
            let backend = state.backend;
            let uptime_ms = state.opened_at.elapsed().as_millis() as u64;
            if state.host.pump() {
                return GuiStatus { state: "open", code: None, backend: Some(backend), platform, uptime_ms, message: None };
            }
            let requested = state.close_requested.swap(false, Ordering::SeqCst);
            *guard = None;
            if requested {
                GuiStatus { state: "closed", code: Some(GUI_CLOSED), backend: Some(backend), platform, uptime_ms, message: None }
            }
            else {
                GuiStatus {
                    state: "closed",
                    code: Some(GUI_HOST_EXITED),
                    backend: Some(backend),
                    platform,
                    uptime_ms,
                    message: Some(format!("the {backend} webview host stopped without a page close request")),
                }
            }
        });
        let summary = status.code.unwrap_or("window alive");
        Ok((status, audit::Summary::new(1, summary)))
    })
    .map_err(core_error)?;
    value_to_js(ctx.env(), &status)
}

#[function(js_name = "closeGui")]
fn close_gui(ctx: CallContext) -> Result<JsValue> {
    let platform = Platform::current().name();
    let result = audit::run("gui-close", Mode::Execute, Value::Null, || {
        let result = GUI.with(|slot| {
            let mut guard = slot.borrow_mut();
            let Some(state) = guard.as_mut()
            else {
                return GuiCloseResult {
                    closed: false,
                    status: GuiStatus {
                        state: "not_open",
                        code: Some(ErrorCode::GuiNotOpen.code()),
                        backend: None,
                        platform,
                        uptime_ms: 0,
                        message: Some("no zipper window is open".to_string()),
                    },
                };
            };
            let backend = state.backend;
            let uptime_ms = state.opened_at.elapsed().as_millis() as u64;
            match state.host.close() {
                Ok(()) => {
                    *guard = None;
                    GuiCloseResult {
                        closed: true,
                        status: GuiStatus { state: "closed", code: None, backend: Some(backend), platform, uptime_ms, message: None },
                    }
                }
                Err(error) => GuiCloseResult {
                    closed: false,
                    status: GuiStatus {
                        state: "open",
                        code: Some(GUI_CLOSE_FAILED),
                        backend: Some(backend),
                        platform,
                        uptime_ms,
                        message: Some(error.to_string()),
                    },
                },
            }
        });
        let summary = if result.closed { "window closed" } else { "close did not complete" };
        Ok((result, audit::Summary::new(1, summary)))
    })
    .map_err(core_error)?;
    value_to_js(ctx.env(), &result)
}

#[function(js_name = "isGuiOpen")]
fn is_gui_open(ctx: CallContext) -> Result<JsValue> {
    let open = audit::run("gui-status", Mode::DryRun, Value::Null, || {
        let open = GUI.with(|slot| slot.borrow().as_ref().map(|state| state.host.is_alive()).unwrap_or(false));
        Ok((open, audit::Summary::new(1, if open { "open" } else { "closed" })))
    })
    .map_err(core_error)?;
    ctx.env().boolean(open)
}

#[function(js_name = "configureAudit")]
fn configure_audit_js(ctx: CallContext) -> Result<JsValue> {
    let arg = ctx.arg(0).ok();
    let actor = actor_from_js(arg)?;
    let session = optional_string_prop_of(arg, "session")?;
    let host = optional_string_prop_of(arg, "host")?;
    audit::configure(actor, session, host);
    ctx.env().undefined()
}

#[function(js_name = "auditQuery")]
fn audit_query_js(ctx: CallContext) -> Result<JsValue> {
    let filter = query_from_js(ctx.arg(0).ok())?;
    let entries = audit::run("audit-query", Mode::DryRun, Value::Null, || {
        let entries = audit::query(&filter).map_err(|error| CoreError::message(error.to_string()))?;
        Ok((entries, audit::Summary::new(0, "audit history queried")))
    })
    .map_err(core_error)?;
    value_to_js(ctx.env(), &entries)
}

fn query_from_js(arg: Option<JsValue>) -> Result<Query> {
    let Some(value) = arg
    else {
        return Ok(Query::default());
    };
    if value.is_undefined()? {
        return Ok(Query::default());
    }
    Ok(Query {
        actor_kind: optional_string_prop(value, "actorKind")?
            .map(|kind| if kind.eq_ignore_ascii_case("agent") { ActorKind::Agent } else { ActorKind::User }),
        actor_id: optional_string_prop(value, "actorId")?,
        from_ms: optional_number_prop(value, "fromMs")?,
        to_ms: optional_number_prop(value, "toMs")?,
        capability: optional_string_prop(value, "capability")?,
        limit: optional_number_prop(value, "limit")?.map(|limit| limit as usize),
    })
}

fn optional_number_prop(value: JsValue, name: &str) -> Result<Option<u64>> {
    match value.get_named(name) {
        Ok(inner) if !inner.is_undefined()? && inner.type_of()? == sys::napi_number => Ok(Some(inner.as_i64()?.max(0) as u64)),
        _ => Ok(None),
    }
}

register_module![
    list_entries_js,
    extract_js,
    create_js,
    test_archive_js,
    preview_entry_js,
    version,
    capabilities_js,
    open_gui,
    pump_events,
    close_gui,
    is_gui_open,
    configure_audit_js,
    audit_query_js
];

#[cfg(test)]
mod ipc_routing_tests {
    use super::*;

    fn handler() -> ZipperIpcHandler {
        ZipperIpcHandler { close_requested: Arc::new(AtomicBool::new(false)), temp_dir: PathBuf::from("unused-in-these-cases") }
    }

    #[test]
    fn malformed_and_unknown_messages_are_ignored() {
        let mut handler = handler();
        assert!(matches!(handler.on_ipc("not json"), IpcOutcome::Ignored));
        assert!(matches!(handler.on_ipc(r#"{"type":"nope","id":1}"#), IpcOutcome::Ignored));
    }

    #[test]
    fn preview_and_open_entry_reject_an_empty_path_before_any_audit() {
        let mut handler = handler();
        let IpcOutcome::ReplyToPage(text) = handler.on_ipc(r#"{"type":"preview","id":1,"index":0,"path":" "}"#)
        else {
            panic!("expected a preview reply");
        };
        let value: Value = serde_json::from_str(&text).expect("preview reply json");
        assert_eq!(value["type"], "previewError");
        assert_eq!(value["id"], 1);

        let IpcOutcome::ReplyToPage(text) = handler.on_ipc(r#"{"type":"openEntry","id":2,"index":0}"#)
        else {
            panic!("expected an open entry reply");
        };
        let value: Value = serde_json::from_str(&text).expect("open entry reply json");
        assert_eq!(value["type"], "openEntryError");
        assert_eq!(value["id"], 2);
    }

    #[test]
    fn close_marks_the_requested_flag() {
        let mut handler = handler();
        assert!(matches!(handler.on_ipc(r#"{"type":"close"}"#), IpcOutcome::CloseWindow));
        assert!(handler.close_requested.load(Ordering::SeqCst));
    }

    /// Builds one small zip through the core so the preview and recent paths
    /// run against a real archive instead of fixtures.
    fn demo_archive(dir: &Path) -> std::path::PathBuf {
        let source = dir.join("readme.txt");
        fs::write(&source, b"hello zipper").expect("write source");
        let archive = dir.join("demo.zip");
        open_zipper_core::create(&[source], &archive, Some("zip"), &open_zipper_core::CreateOptions::default()).expect("create demo zip");
        archive
    }

    #[test]
    fn preview_recent_and_list_record_round_trip_through_the_handler() {
        let dir = std::env::temp_dir().join(format!("open-zipper-ipc-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");
        let archive = demo_archive(dir.as_path());
        let mut handler = ZipperIpcHandler { close_requested: Arc::new(AtomicBool::new(false)), temp_dir: dir.clone() };

        let IpcOutcome::ReplyToPage(text) =
            handler.on_ipc(&json!({ "type": "preview", "id": 7, "path": archive.display().to_string(), "index": 0 }).to_string())
        else {
            panic!("expected a preview reply");
        };
        let value: Value = serde_json::from_str(&text).expect("preview reply json");
        assert_eq!(value["type"], "previewResult");
        assert_eq!(value["id"], 7);
        assert_eq!(value["preview"]["kind"], "text");
        assert_eq!(value["preview"]["text"], "hello zipper");

        let IpcOutcome::ReplyToPage(text) =
            handler.on_ipc(&json!({ "type": "list", "id": 8, "path": archive.display().to_string() }).to_string())
        else {
            panic!("expected a list reply");
        };
        let value: Value = serde_json::from_str(&text).expect("list reply json");
        assert_eq!(value["type"], "listResult");

        // The list success path records the recent store in the real per-user
        // directory; assert the archive appears there after the call.
        let (store, platform) = recent_store().expect("recent store resolves");
        let items = recent::load(&store, platform);
        assert!(items.iter().any(|item| item.path == archive.display().to_string()), "recent store must contain the listed archive");

        let IpcOutcome::ReplyToPage(text) = handler.on_ipc(r#"{"type":"recent","id":9}"#)
        else {
            panic!("expected a recent reply");
        };
        let value: Value = serde_json::from_str(&text).expect("recent reply json");
        assert_eq!(value["type"], "recentResult");
        assert!(value["items"].as_array().is_some_and(|items| !items.is_empty()));

        let _ = fs::remove_dir_all(&dir);
    }
}
