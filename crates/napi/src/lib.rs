//! Node-API surface for `@wae/open-zipper` through the WAE facade.
//!
//! This crate is the only implementation path for archive work. The CLI and the
//! GUI both consume it, and every call is audited. Archive parsing, path
//! validation, extraction and creation live in `open-zipper-core`; no child
//! process or shell pipeline is ever used here.

mod audit;

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
};

use open_zipper_core::{
    ArchiveLimits, CapabilityState, CreateOptions, Error as CoreError, ExtractOptions, OverwritePolicy, capabilities, create, extract, list,
    test,
};
use serde_json::{Value, json};
use wae::{
    CallContext, Env, Error, JsValue, Result,
    audit::{Actor, ActorKind, Mode, Query},
    function, optional_string_prop, register_module, sys,
    webview::{IpcHandler, IpcOutcome, OpenOptions, WebViewHost},
};

const APP_TITLE: &str = "OpenZipper";

thread_local! {
    static GUI: RefCell<Option<GuiState>> = const { RefCell::new(None) };
}

struct GuiState {
    host: WebViewHost,
}

struct ZipperIpcHandler;

impl IpcHandler for ZipperIpcHandler {
    fn on_ipc(&mut self, message: &str) -> IpcOutcome {
        let Ok(value) = serde_json::from_str::<Value>(message)
        else {
            return IpcOutcome::Ignored;
        };
        let ty = value.get("type").and_then(Value::as_str).unwrap_or("");
        let id = value.get("id").cloned().unwrap_or(Value::Null);
        match ty {
            "close" => IpcOutcome::CloseWindow,
            "capabilities" => match serde_json::to_value(capabilities()) {
                Ok(report) => IpcOutcome::ReplyToPage(json!({ "type": "capabilitiesResult", "id": id, "capabilities": report }).to_string()),
                Err(error) => {
                    IpcOutcome::ReplyToPage(json!({ "type": "capabilitiesError", "id": id, "message": error.to_string() }).to_string())
                }
            },
            "list" => {
                let path = value.get("path").and_then(Value::as_str).unwrap_or("").to_string();
                let limits = limits_from_json(value.get("limits"));
                match audited_list(path.clone(), limits) {
                    Ok(report) => IpcOutcome::ReplyToPage(
                        json!({ "type": "listResult", "id": id, "listing": serde_json::to_value(&report).unwrap_or(Value::Null) }).to_string(),
                    ),
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
            _ => IpcOutcome::Ignored,
        }
    }
}

fn core_error(error: CoreError) -> Error {
    Error::new(error.to_string())
}

fn webview_error(error: wae::webview::WebViewError) -> CoreError {
    if matches!(error, wae::webview::WebViewError::Unsupported { .. }) {
        return CoreError::capability(CapabilityState::UnsupportedPlatform, error);
    }
    CoreError::message(error.to_string())
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
    let mut title = APP_TITLE.to_string();
    let mut content_dir: Option<String> = None;
    let mut url: Option<String> = None;
    let mut width = 1120;
    let mut height = 780;

    if let Ok(arg) = ctx.arg(0)
        && !arg.is_undefined()?
    {
        if let Some(value) = optional_string_prop(arg, "title")? {
            title = value;
        }
        content_dir = optional_string_prop(arg, "contentDir")?;
        url = optional_string_prop(arg, "url")?;
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
    let host = audit::run("gui-open", Mode::Execute, params, || {
        let already_open = GUI.with(|slot| slot.borrow().as_ref().map(|state| state.host.is_alive()).unwrap_or(false));
        if already_open {
            return Err(CoreError::message("a zipper window is already open"));
        }
        let options = if let Some(dir) = content_dir.clone() {
            OpenOptions::from_content_dir(dir, title.clone())
        }
        else if let Some(url) = url.clone() {
            OpenOptions::from_url(url, title.clone())
        }
        else {
            let dir = resolve_gui_dir().map_err(|error| CoreError::message(error.to_string()))?;
            OpenOptions::from_content_dir(dir.display().to_string(), title.clone())
        };
        let mut options = options;
        options.width = width;
        options.height = height;
        let host = WebViewHost::open(options, Box::new(ZipperIpcHandler)).map_err(webview_error)?;
        let summary = format!("window opened with {} backend", host.backend_name());
        Ok((host, audit::Summary::new(1, summary)))
    })
    .map_err(core_error)?;

    GUI.with(|slot| {
        *slot.borrow_mut() = Some(GuiState { host });
    });
    ctx.env().undefined()
}

#[function(js_name = "pumpEvents")]
fn pump_events(ctx: CallContext) -> Result<JsValue> {
    let alive = audit::run("gui-pump", Mode::Execute, Value::Null, || {
        let alive = GUI.with(|slot| {
            let mut guard = slot.borrow_mut();
            match guard.as_mut() {
                Some(state) => {
                    let ok = state.host.pump();
                    if !ok {
                        *guard = None;
                    }
                    ok
                }
                None => false,
            }
        });
        Ok((alive, audit::Summary::new(1, if alive { "window alive" } else { "window closed" })))
    })
    .map_err(core_error)?;
    ctx.env().boolean(alive)
}

#[function(js_name = "closeGui")]
fn close_gui(ctx: CallContext) -> Result<JsValue> {
    audit::run("gui-close", Mode::Execute, Value::Null, || {
        let closed = GUI.with(|slot| {
            let mut guard = slot.borrow_mut();
            if let Some(mut state) = guard.take() {
                let _ = state.host.close();
                true
            }
            else {
                false
            }
        });
        Ok(((), audit::Summary::new(1, if closed { "window closed" } else { "no window" })))
    })
    .map_err(core_error)?;
    ctx.env().undefined()
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
    version,
    capabilities_js,
    open_gui,
    pump_events,
    close_gui,
    is_gui_open,
    configure_audit_js,
    audit_query_js
];
