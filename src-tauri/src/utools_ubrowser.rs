//! Host-owned implementation of the public uTools `ubrowser` chain.
//!
//! Remote pages run in a dedicated WebView with no matching Tauri capability.
//! The plugin submits a bounded declarative chain from its loopback iframe;
//! Rust validates and executes each step through the host WebView handle. A
//! remote origin therefore never receives a bridge secret, invoke permission,
//! local filesystem path, or another plugin's browser identity.

use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{mpsc, Condvar, Mutex},
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{
    webview::{Color, Cookie, DownloadEvent, NewWindowResponse, PageLoadEvent},
    AppHandle, LogicalPosition, LogicalSize, Manager, Theme, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};
use url::Url;
use uuid::Uuid;

use crate::plugin_asset_server::PluginAssetServer;

pub(crate) const UTOOLS_UBROWSER_WINDOW_PREFIX: &str = "utools-ubrowser-";
const MAX_WINDOWS: usize = 8;
const MAX_WINDOWS_PER_PLUGIN: usize = 4;
const MAX_STEPS: usize = 128;
const MAX_STEP_ARGS: usize = 8;
const MAX_SCRIPT_CHARS: usize = 65_536;
const MAX_SCRIPT_BYTES: usize = 262_144;
const MAX_RESULT_BYTES: usize = 512 * 1024;
const MAX_CHAIN_REQUEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_CHAIN_DURATION: Duration = Duration::from_secs(120);
const DEFAULT_NAVIGATION_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_EVAL_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone)]
struct UBrowserRecord {
    instance_id: String,
    plugin_id: String,
    parent_lease_id: Option<String>,
    busy: bool,
    load_generation: u64,
    last_finished_url: String,
    download_generation: u64,
    pending_download: Option<PendingDownload>,
}

#[derive(Debug, Clone)]
struct PendingDownload {
    requested: bool,
    save_path: Option<PathBuf>,
    resolved_path: Option<PathBuf>,
    result: Option<Result<PathBuf, String>>,
}

#[derive(Default)]
struct UBrowserState {
    windows: HashMap<String, UBrowserRecord>,
    proxies: HashMap<String, Url>,
}

#[derive(Default)]
pub(crate) struct UtoolsUBrowserRegistry {
    state: Mutex<UBrowserState>,
    load_changed: Condvar,
    download_changed: Condvar,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UBrowserInstance {
    pub(crate) id: String,
    pub(crate) url: String,
    pub(crate) title: String,
    pub(crate) width: i64,
    pub(crate) height: i64,
    pub(crate) x: i64,
    pub(crate) y: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UBrowserRunRequest {
    #[serde(default)]
    pub(crate) instance_id: Option<String>,
    pub(crate) steps: Vec<UBrowserStep>,
    #[serde(default)]
    pub(crate) options: UBrowserWindowOptions,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UBrowserStep {
    pub(crate) op: String,
    #[serde(default)]
    pub(crate) args: Vec<Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UBrowserWindowOptions {
    show: Option<bool>,
    width: Option<f64>,
    height: Option<f64>,
    x: Option<f64>,
    y: Option<f64>,
    center: Option<bool>,
    min_width: Option<f64>,
    min_height: Option<f64>,
    max_width: Option<f64>,
    max_height: Option<f64>,
    resizable: Option<bool>,
    movable: Option<bool>,
    minimizable: Option<bool>,
    maximizable: Option<bool>,
    always_on_top: Option<bool>,
    fullscreen: Option<bool>,
    fullscreenable: Option<bool>,
    enable_larger_than_screen: Option<bool>,
    opacity: Option<f64>,
    frame: Option<bool>,
    closable: Option<bool>,
    focusable: Option<bool>,
    skip_taskbar: Option<bool>,
    background_color: Option<String>,
    has_shadow: Option<bool>,
    transparent: Option<bool>,
    title_bar_style: Option<String>,
    thick_frame: Option<bool>,
}

#[derive(Debug)]
struct RunReservation {
    instance_id: String,
    label: String,
    create: bool,
}

impl UtoolsUBrowserRegistry {
    fn reserve_run(
        &self,
        plugin_id: &str,
        parent_lease_id: &str,
        requested_id: Option<&str>,
    ) -> Result<RunReservation, String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(instance_id) = requested_id {
            validate_instance_id(instance_id)?;
            let (label, record) = state
                .windows
                .iter_mut()
                .find(|(_, record)| record.instance_id == instance_id)
                .ok_or_else(|| {
                    "This uTools ubrowser instance is no longer available.".to_owned()
                })?;
            if record.plugin_id != plugin_id {
                return Err("This uTools ubrowser instance belongs to another plugin.".to_owned());
            }
            if record.busy {
                return Err("This uTools ubrowser instance is already running a chain.".to_owned());
            }
            record.busy = true;
            record.parent_lease_id = Some(parent_lease_id.to_owned());
            return Ok(RunReservation {
                instance_id: instance_id.to_owned(),
                label: label.clone(),
                create: false,
            });
        }

        if state.windows.len() >= MAX_WINDOWS {
            return Err(format!(
                "Too many uTools ubrowser windows are active (limit: {MAX_WINDOWS})."
            ));
        }
        let plugin_count = state
            .windows
            .values()
            .filter(|record| record.plugin_id == plugin_id)
            .count();
        if plugin_count >= MAX_WINDOWS_PER_PLUGIN {
            return Err(format!(
                "Plugin '{plugin_id}' already owns {MAX_WINDOWS_PER_PLUGIN} ubrowser windows."
            ));
        }
        let instance_id = Uuid::new_v4().to_string();
        let label = format!("{UTOOLS_UBROWSER_WINDOW_PREFIX}{instance_id}");
        state.windows.insert(
            label.clone(),
            UBrowserRecord {
                instance_id: instance_id.clone(),
                plugin_id: plugin_id.to_owned(),
                parent_lease_id: Some(parent_lease_id.to_owned()),
                busy: true,
                load_generation: 0,
                last_finished_url: "about:blank".to_owned(),
                download_generation: 0,
                pending_download: None,
            },
        );
        Ok(RunReservation {
            instance_id,
            label,
            create: true,
        })
    }

    fn finish_run(&self, label: &str, plugin_id: &str, parent_lease_id: &str) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(record) = state.windows.get_mut(label) {
            if record.plugin_id == plugin_id
                && record.parent_lease_id.as_deref() == Some(parent_lease_id)
            {
                record.busy = false;
                record.parent_lease_id = None;
                record.pending_download = None;
            }
        }
        self.load_changed.notify_all();
    }

    fn cancel_reservation(&self, label: &str, instance_id: &str) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state
            .windows
            .get(label)
            .is_some_and(|record| record.instance_id == instance_id && record.busy)
        {
            state.windows.remove(label);
        }
        self.load_changed.notify_all();
    }

    pub(crate) fn remove_window(&self, label: &str) {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .windows
            .remove(label);
        self.load_changed.notify_all();
    }

    pub(crate) fn close_plugin_windows(&self, app: &AppHandle, plugin_id: &str) {
        let labels = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .windows
            .iter()
            .filter(|(_, record)| record.plugin_id == plugin_id)
            .map(|(label, _)| label.clone())
            .collect::<Vec<_>>();
        for label in labels {
            if let Some(window) = app.get_webview_window(&label) {
                let _ = window.close();
            } else {
                self.remove_window(&label);
            }
        }
    }

    pub(crate) fn idle_instances(&self, app: &AppHandle, plugin_id: &str) -> Vec<UBrowserInstance> {
        let records = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .windows
            .iter()
            .filter(|(_, record)| record.plugin_id == plugin_id && !record.busy)
            .map(|(label, record)| (label.clone(), record.instance_id.clone()))
            .collect::<Vec<_>>();
        records
            .into_iter()
            .filter_map(|(label, id)| {
                let window = app.get_webview_window(&label)?;
                let url = window.url().ok()?.to_string();
                let title = window.title().unwrap_or_default();
                let scale = window.scale_factor().ok()?;
                let size = window.inner_size().ok()?.to_logical::<f64>(scale);
                let position = window.outer_position().ok()?.to_logical::<f64>(scale);
                Some(UBrowserInstance {
                    id,
                    url,
                    title,
                    width: size.width.round() as i64,
                    height: size.height.round() as i64,
                    x: position.x.round() as i64,
                    y: position.y.round() as i64,
                })
            })
            .collect()
    }

    pub(crate) fn set_proxy(&self, plugin_id: &str, proxy: Option<Url>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(proxy) = proxy {
            state.proxies.insert(plugin_id.to_owned(), proxy);
        } else {
            state.proxies.remove(plugin_id);
        }
    }

    pub(crate) fn set_proxy_config(&self, plugin_id: &str, config: &Value) -> Result<(), String> {
        let object = config
            .as_object()
            .ok_or_else(|| "uTools ubrowser proxy config must be an object.".to_owned())?;
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "proxyRules" | "proxyBypassRules"))
        {
            return Err("uTools ubrowser proxy config contains an unsupported field.".to_owned());
        }
        let rules = object
            .get("proxyRules")
            .and_then(Value::as_str)
            .ok_or_else(|| "uTools ubrowser proxyRules must be a URL string.".to_owned())?;
        validate_bounded_string(rules, "proxy URL", 2048)?;
        if let Some(bypass) = object.get("proxyBypassRules") {
            let bypass = bypass
                .as_str()
                .ok_or_else(|| "uTools ubrowser proxyBypassRules must be a string.".to_owned())?;
            validate_bounded_string(bypass, "proxy bypass rules", 2048)?;
            if !bypass.trim().is_empty() {
                return Err(
                    "Proxy bypass rules require the WebView2 extended proxy phase.".to_owned(),
                );
            }
        }
        let proxy = Url::parse(rules)
            .map_err(|_| "uTools ubrowser proxyRules is not a valid URL.".to_owned())?;
        if !matches!(proxy.scheme(), "http" | "socks5")
            || proxy.host_str().is_none()
            || !proxy.username().is_empty()
            || proxy.password().is_some()
        {
            return Err(
                "uTools ubrowser proxyRules must be a credential-free http:// or socks5:// URL."
                    .to_owned(),
            );
        }
        self.set_proxy(plugin_id, Some(proxy));
        Ok(())
    }

    pub(crate) fn clear_cache(&self, app: &AppHandle, plugin_id: &str) -> Result<(), String> {
        let labels = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .windows
            .iter()
            .filter(|(_, record)| record.plugin_id == plugin_id)
            .map(|(label, _)| label.clone())
            .collect::<Vec<_>>();
        let has_live_windows = !labels.is_empty();
        for label in labels {
            if let Some(window) = app.get_webview_window(&label) {
                window.clear_all_browsing_data().map_err(|error| {
                    format!("Could not clear uTools ubrowser browsing data: {error}")
                })?;
            }
        }
        if !has_live_windows {
            let cache_root = app
                .path()
                .app_cache_dir()
                .map_err(|error| {
                    format!("Could not resolve the ubrowser cache directory: {error}")
                })?
                .join("utools-ubrowser");
            let target = cache_root.join(plugin_id);
            if target.exists() {
                let root = fs::canonicalize(&cache_root).map_err(|error| {
                    format!("Could not resolve the ubrowser cache root: {error}")
                })?;
                let resolved = fs::canonicalize(&target).map_err(|error| {
                    format!("Could not resolve the plugin ubrowser cache: {error}")
                })?;
                let metadata = fs::symlink_metadata(&target).map_err(|error| {
                    format!("Could not inspect the plugin ubrowser cache: {error}")
                })?;
                if !resolved.starts_with(&root)
                    || resolved == root
                    || !metadata.is_dir()
                    || metadata.file_type().is_symlink()
                {
                    return Err("The plugin ubrowser cache target is unsafe to clear.".to_owned());
                }
                fs::remove_dir_all(&resolved).map_err(|error| {
                    format!("Could not clear the plugin ubrowser cache: {error}")
                })?;
            }
        }
        Ok(())
    }

    fn proxy_for(&self, plugin_id: &str) -> Option<Url> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .proxies
            .get(plugin_id)
            .cloned()
    }

    fn note_page_load(&self, label: &str, url: &str) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(record) = state.windows.get_mut(label) {
            record.load_generation = record.load_generation.saturating_add(1);
            record.last_finished_url = url.to_owned();
        }
        self.load_changed.notify_all();
    }

    fn load_generation(&self, label: &str) -> Result<u64, String> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .windows
            .get(label)
            .map(|record| record.load_generation)
            .ok_or_else(|| "The uTools ubrowser window was closed.".to_owned())
    }

    fn wait_for_load(
        &self,
        label: &str,
        after_generation: u64,
        timeout: Duration,
        still_active: impl Fn() -> bool,
    ) -> Result<String, String> {
        let deadline = Instant::now() + timeout;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if !still_active() {
                return Err(
                    "The plugin surface closed while the uTools ubrowser was navigating."
                        .to_owned(),
                );
            }
            let record = state
                .windows
                .get(label)
                .ok_or_else(|| "The uTools ubrowser window closed during navigation.".to_owned())?;
            if record.load_generation > after_generation {
                return Ok(record.last_finished_url.clone());
            }
            let now = Instant::now();
            if now >= deadline {
                return Err("uTools ubrowser navigation timed out.".to_owned());
            }
            let wait = deadline
                .saturating_duration_since(now)
                .min(Duration::from_millis(250));
            let (next, result) = self
                .load_changed
                .wait_timeout(state, wait)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
            if result.timed_out() && Instant::now() >= deadline {
                return Err("uTools ubrowser navigation timed out.".to_owned());
            }
        }
    }

    fn prepare_download(&self, label: &str, save_path: Option<PathBuf>) -> Result<u64, String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let record = state
            .windows
            .get_mut(label)
            .ok_or_else(|| "The uTools ubrowser window was closed.".to_owned())?;
        if record.pending_download.is_some() {
            return Err("The uTools ubrowser already has a pending download.".to_owned());
        }
        let generation = record.download_generation;
        record.pending_download = Some(PendingDownload {
            requested: false,
            save_path,
            resolved_path: None,
            result: None,
        });
        Ok(generation)
    }

    fn cancel_download(&self, label: &str) {
        if let Some(record) = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .windows
            .get_mut(label)
        {
            record.pending_download = None;
        }
        self.download_changed.notify_all();
    }

    fn download_started(&self, label: &str) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .windows
            .get(label)
            .and_then(|record| record.pending_download.as_ref())
            .is_some_and(|download| download.requested)
    }

    fn request_download(&self, label: &str, destination: &mut PathBuf) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(download) = state
            .windows
            .get_mut(label)
            .and_then(|record| record.pending_download.as_mut())
        else {
            return false;
        };
        if download.requested {
            return false;
        }
        if let Some(save_path) = download.save_path.as_ref() {
            *destination = save_path.clone();
        }
        download.resolved_path = Some(destination.clone());
        download.requested = true;
        self.download_changed.notify_all();
        true
    }

    fn finish_download(&self, label: &str, path: Option<PathBuf>, success: bool) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(record) = state.windows.get_mut(label) {
            let Some(download) = record.pending_download.as_mut() else {
                return;
            };
            let resolved = path.or_else(|| download.resolved_path.clone());
            download.result = Some(if success {
                resolved.ok_or_else(|| "The ubrowser download returned no output path.".to_owned())
            } else {
                Err("The uTools ubrowser download failed.".to_owned())
            });
            record.download_generation = record.download_generation.saturating_add(1);
        }
        self.download_changed.notify_all();
    }

    fn wait_for_download(
        &self,
        label: &str,
        after_generation: u64,
        timeout: Duration,
        still_active: impl Fn() -> bool,
    ) -> Result<PathBuf, String> {
        let deadline = Instant::now() + timeout;
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if !still_active() {
                return Err(
                    "The plugin surface closed during the uTools ubrowser download.".to_owned(),
                );
            }
            let record = state
                .windows
                .get_mut(label)
                .ok_or_else(|| "The uTools ubrowser window closed during download.".to_owned())?;
            if record.download_generation > after_generation {
                let result = record
                    .pending_download
                    .take()
                    .and_then(|download| download.result)
                    .ok_or_else(|| "The uTools ubrowser lost its download result.".to_owned())?;
                return result;
            }
            if Instant::now() >= deadline {
                record.pending_download = None;
                return Err("uTools ubrowser download timed out.".to_owned());
            }
            let wait = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(250));
            let (next, _) = self
                .download_changed
                .wait_timeout(state, wait)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
        }
    }
}

pub(crate) fn validate_run_request(request: &UBrowserRunRequest) -> Result<(), String> {
    if request.steps.is_empty() || request.steps.len() > MAX_STEPS {
        return Err(format!(
            "uTools ubrowser chains must contain 1-{MAX_STEPS} steps."
        ));
    }
    if let Some(instance_id) = request.instance_id.as_deref() {
        validate_instance_id(instance_id)?;
    }
    validate_window_options(&request.options)?;
    let mut condition_depth = 0_usize;
    for step in &request.steps {
        if !known_operation(&step.op) || step.args.len() > MAX_STEP_ARGS {
            return Err(format!(
                "Unsupported or malformed ubrowser step '{}'.",
                step.op
            ));
        }
        if step.op == "when" {
            condition_depth += 1;
            if condition_depth > 8 {
                return Err("uTools ubrowser conditions are nested too deeply.".to_owned());
            }
        } else if step.op == "end" {
            if !step.args.is_empty() || condition_depth == 0 {
                return Err("uTools ubrowser end() has no matching when().".to_owned());
            }
            condition_depth -= 1;
        }
    }
    if condition_depth != 0 {
        return Err("uTools ubrowser when() requires a matching end().".to_owned());
    }
    let bytes = serde_json::to_vec(request)
        .map_err(|error| format!("Could not encode the ubrowser chain: {error}"))?;
    if bytes.len() > MAX_CHAIN_REQUEST_BYTES {
        return Err("uTools ubrowser chain exceeds 4 MiB.".to_owned());
    }
    Ok(())
}

pub(crate) fn run_chain(
    app: &AppHandle,
    registry: &UtoolsUBrowserRegistry,
    asset_server: &PluginAssetServer,
    plugin_id: &str,
    parent_lease_id: &str,
    request: UBrowserRunRequest,
) -> Result<Value, String> {
    validate_run_request(&request)?;
    let reservation =
        registry.reserve_run(plugin_id, parent_lease_id, request.instance_id.as_deref())?;
    let result = (|| {
        let window = if reservation.create {
            create_window(
                app,
                registry,
                asset_server,
                plugin_id,
                parent_lease_id,
                &reservation,
                &request,
            )?
        } else {
            let window = app
                .get_webview_window(&reservation.label)
                .ok_or_else(|| "This uTools ubrowser window has already closed.".to_owned())?;
            apply_window_options(&window, &request.options)?;
            window
        };
        execute_steps(
            registry,
            asset_server,
            plugin_id,
            parent_lease_id,
            &window,
            &reservation,
            &request.steps,
        )
    })();
    if app.get_webview_window(&reservation.label).is_none() {
        registry.cancel_reservation(&reservation.label, &reservation.instance_id);
    } else {
        registry.finish_run(&reservation.label, plugin_id, parent_lease_id);
    }
    result
}

fn create_window(
    app: &AppHandle,
    registry: &UtoolsUBrowserRegistry,
    asset_server: &PluginAssetServer,
    plugin_id: &str,
    parent_lease_id: &str,
    reservation: &RunReservation,
    request: &UBrowserRunRequest,
) -> Result<WebviewWindow, String> {
    let user_agent = request
        .steps
        .iter()
        .find_map(|step| match step.op.as_str() {
            "useragent" => step.args.first().and_then(Value::as_str),
            "device" => step
                .args
                .first()
                .and_then(Value::as_object)
                .and_then(|device| device.get("userAgent"))
                .and_then(Value::as_str),
            _ => None,
        });
    let options = &request.options;
    let label = reservation.label.clone();
    let callback_label = label.clone();
    let download_label = label.clone();
    let mut builder = WebviewWindowBuilder::new(
        app,
        label,
        WebviewUrl::External(Url::parse("about:blank").expect("about:blank is a valid URL")),
    )
    .title("iHub ubrowser")
    .theme(Some(Theme::Light))
    .inner_size(
        options.width.unwrap_or(800.0),
        options.height.unwrap_or(600.0),
    )
    .visible(options.show.unwrap_or(true))
    .focused(options.show.unwrap_or(true) && options.focusable.unwrap_or(true))
    .resizable(options.resizable.unwrap_or(true))
    .minimizable(options.minimizable.unwrap_or(true))
    .maximizable(options.maximizable.unwrap_or(true))
    .closable(options.closable.unwrap_or(true))
    .always_on_top(options.always_on_top.unwrap_or(false))
    .fullscreen(options.fullscreen.unwrap_or(false))
    .decorations(options.frame.unwrap_or(true))
    .focusable(options.focusable.unwrap_or(true))
    .skip_taskbar(options.skip_taskbar.unwrap_or(false))
    .transparent(options.transparent.unwrap_or(false))
    .shadow(options.has_shadow.unwrap_or(false))
    .on_navigation(allowed_navigation)
    .on_new_window(|_, _| NewWindowResponse::Deny)
    .on_download(move |webview, event| {
        let registry = webview.app_handle().state::<UtoolsUBrowserRegistry>();
        match event {
            DownloadEvent::Requested { destination, .. } => {
                registry.request_download(&download_label, destination)
            }
            DownloadEvent::Finished { path, success, .. } => {
                registry.finish_download(&download_label, path, success);
                true
            }
            _ => false,
        }
    })
    .on_page_load(move |window, payload| {
        if payload.event() == PageLoadEvent::Finished {
            window
                .app_handle()
                .state::<UtoolsUBrowserRegistry>()
                .note_page_load(&callback_label, payload.url().as_str());
        }
    });
    let data_directory = app
        .path()
        .app_cache_dir()
        .map_err(|error| format!("Could not resolve the ubrowser cache directory: {error}"))?
        .join("utools-ubrowser")
        .join(plugin_id);
    fs::create_dir_all(&data_directory)
        .map_err(|error| format!("Could not create the ubrowser cache directory: {error}"))?;
    builder = builder.data_directory(data_directory);
    if let Some(user_agent) = user_agent {
        validate_bounded_string(user_agent, "user agent", 1024)?;
        builder = builder.user_agent(user_agent);
    }
    if let Some(proxy) = registry.proxy_for(plugin_id) {
        builder = builder.proxy_url(proxy);
    }
    if !options.enable_larger_than_screen.unwrap_or(false) {
        builder = builder.prevent_overflow();
    }
    if let Some(color) = options.background_color.as_deref() {
        builder = builder.background_color(parse_color(color)?);
    }
    if let (Some(width), Some(height)) = (options.min_width, options.min_height) {
        builder = builder.min_inner_size(width, height);
    }
    if let (Some(width), Some(height)) = (options.max_width, options.max_height) {
        builder = builder.max_inner_size(width, height);
    }
    if options
        .center
        .unwrap_or(options.x.is_none() && options.y.is_none())
    {
        builder = builder.center();
    } else if let (Some(x), Some(y)) = (options.x, options.y) {
        builder = builder.position(x, y);
    }
    let window = builder
        .build()
        .map_err(|error| format!("Could not create the uTools ubrowser window: {error}"))?;
    if registry.load_generation(&reservation.label)? == 0 {
        let _ = registry.wait_for_load(&reservation.label, 0, Duration::from_secs(15), || {
            asset_server.is_active_surface_for(parent_lease_id, plugin_id)
        })?;
    }
    Ok(window)
}

fn apply_window_options(
    window: &WebviewWindow,
    options: &UBrowserWindowOptions,
) -> Result<(), String> {
    validate_window_options(options)?;
    window
        .set_size(LogicalSize::new(
            options.width.unwrap_or(800.0),
            options.height.unwrap_or(600.0),
        ))
        .map_err(|error| format!("Could not resize the uTools ubrowser window: {error}"))?;
    if options.center.unwrap_or(false) {
        window
            .center()
            .map_err(|error| format!("Could not center the uTools ubrowser window: {error}"))?;
    } else if let (Some(x), Some(y)) = (options.x, options.y) {
        window
            .set_position(LogicalPosition::new(x, y))
            .map_err(|error| format!("Could not move the uTools ubrowser window: {error}"))?;
    }
    window
        .set_resizable(options.resizable.unwrap_or(true))
        .map_err(|error| format!("Could not update ubrowser resizable state: {error}"))?;
    window
        .set_minimizable(options.minimizable.unwrap_or(true))
        .map_err(|error| format!("Could not update ubrowser minimizable state: {error}"))?;
    window
        .set_maximizable(options.maximizable.unwrap_or(true))
        .map_err(|error| format!("Could not update ubrowser maximizable state: {error}"))?;
    window
        .set_closable(options.closable.unwrap_or(true))
        .map_err(|error| format!("Could not update ubrowser closable state: {error}"))?;
    window
        .set_focusable(options.focusable.unwrap_or(true))
        .map_err(|error| format!("Could not update ubrowser focusable state: {error}"))?;
    window
        .set_skip_taskbar(options.skip_taskbar.unwrap_or(false))
        .map_err(|error| format!("Could not update ubrowser taskbar state: {error}"))?;
    window
        .set_decorations(options.frame.unwrap_or(true))
        .map_err(|error| format!("Could not update ubrowser frame state: {error}"))?;
    window
        .set_shadow(options.has_shadow.unwrap_or(false))
        .map_err(|error| format!("Could not update ubrowser shadow state: {error}"))?;
    window
        .set_always_on_top(options.always_on_top.unwrap_or(false))
        .map_err(|error| format!("Could not update ubrowser always-on-top state: {error}"))?;
    window
        .set_fullscreen(options.fullscreen.unwrap_or(false))
        .map_err(|error| format!("Could not update ubrowser fullscreen state: {error}"))?;
    if let Some(color) = options.background_color.as_deref() {
        window
            .set_background_color(Some(parse_color(color)?))
            .map_err(|error| format!("Could not update ubrowser background color: {error}"))?;
    }
    if options.show.unwrap_or(true) {
        window
            .show()
            .map_err(|error| format!("Could not show the uTools ubrowser window: {error}"))?;
    } else {
        window
            .hide()
            .map_err(|error| format!("Could not hide the uTools ubrowser window: {error}"))?;
    }
    Ok(())
}

fn execute_steps(
    registry: &UtoolsUBrowserRegistry,
    asset_server: &PluginAssetServer,
    plugin_id: &str,
    parent_lease_id: &str,
    window: &WebviewWindow,
    reservation: &RunReservation,
    steps: &[UBrowserStep],
) -> Result<Value, String> {
    let started = Instant::now();
    let mut results = Vec::new();
    let mut conditions = Vec::<bool>::new();
    for step in steps {
        if !asset_server.is_active_surface_for(parent_lease_id, plugin_id) {
            return Err("The plugin surface closed during the uTools ubrowser chain.".to_owned());
        }
        if started.elapsed() >= MAX_CHAIN_DURATION {
            return Err("uTools ubrowser chain exceeded the two-minute host limit.".to_owned());
        }
        if step.op == "when" {
            let parent_active = conditions.iter().all(|active| *active);
            let active = if parent_active {
                evaluate_condition(window, &step.args)?
            } else {
                false
            };
            conditions.push(active);
            continue;
        }
        if step.op == "end" {
            conditions.pop();
            continue;
        }
        if !conditions.iter().all(|active| *active) {
            continue;
        }
        match step.op.as_str() {
            "goto" => {
                let url = step
                    .args
                    .first()
                    .and_then(Value::as_str)
                    .ok_or_else(|| "ubrowser.goto requires a URL string.".to_owned())?;
                let url = validate_navigation_url(url)?;
                let timeout = step
                    .args
                    .get(2)
                    .and_then(Value::as_u64)
                    .map(Duration::from_millis)
                    .unwrap_or(DEFAULT_NAVIGATION_TIMEOUT)
                    .clamp(Duration::from_secs(1), Duration::from_secs(60));
                validate_headers(step.args.get(1))?;
                let headers = step
                    .args
                    .get(1)
                    .and_then(Value::as_object)
                    .filter(|headers| !headers.is_empty())
                    .cloned();
                if let Some(headers) = headers.as_ref() {
                    let _ = call_devtools_method(window, "Network.enable", json!({}))?;
                    let _ = call_devtools_method(
                        window,
                        "Network.setExtraHTTPHeaders",
                        json!({ "headers": headers }),
                    )?;
                }
                let generation = registry.load_generation(&reservation.label)?;
                let navigation = window
                    .navigate(url)
                    .map_err(|error| format!("Could not navigate the uTools ubrowser: {error}"))
                    .and_then(|()| {
                        registry
                            .wait_for_load(&reservation.label, generation, timeout, || {
                                asset_server.is_active_surface_for(parent_lease_id, plugin_id)
                            })
                            .map(|_| ())
                    });
                if headers.is_some() {
                    let _ = call_devtools_method(
                        window,
                        "Network.setExtraHTTPHeaders",
                        json!({ "headers": {} }),
                    );
                }
                navigation?;
            }
            "useragent" => {
                let value = required_string_arg(step, 0, "useragent")?;
                validate_bounded_string(value, "user agent", 1024)?;
                let _ = call_devtools_method(
                    window,
                    "Network.setUserAgentOverride",
                    json!({ "userAgent": value }),
                )?;
            }
            "viewport" => {
                let (width, height) = number_pair(step, 64.0, 16_384.0)?;
                window
                    .set_size(LogicalSize::new(width, height))
                    .map_err(|error| format!("Could not resize ubrowser viewport: {error}"))?;
            }
            "hide" => {
                require_arg_count(step, 0)?;
                window
                    .hide()
                    .map_err(|error| format!("Could not hide ubrowser: {error}"))?;
            }
            "show" => {
                require_arg_count(step, 0)?;
                window
                    .show()
                    .map_err(|error| format!("Could not show ubrowser: {error}"))?;
            }
            "css" => {
                let css = required_string_arg(step, 0, "css")?;
                validate_bounded_string(css, "CSS", MAX_SCRIPT_CHARS)?;
                let encoded = serde_json::to_string(css).map_err(|error| error.to_string())?;
                let script = format!(
                    "(() => {{ const style=document.createElement('style'); style.dataset.ihubUbrowser='1'; style.textContent={encoded}; (document.head||document.documentElement).appendChild(style); return null; }})()"
                );
                let initialization = format!(
                    "(() => {{ const apply=()=>{{if(document.querySelector('style[data-ihub-ubrowser]'))return;const style=document.createElement('style');style.dataset.ihubUbrowser='1';style.textContent={encoded};(document.head||document.documentElement).appendChild(style);}};if(document.readyState==='loading')document.addEventListener('DOMContentLoaded',apply,{{once:true}});else apply(); }})()"
                );
                let _ = call_devtools_method(
                    window,
                    "Page.addScriptToEvaluateOnNewDocument",
                    json!({ "source": initialization }),
                )?;
                let _ = eval_json(window, &script, DEFAULT_EVAL_TIMEOUT)?;
            }
            "evaluate" => {
                results.push(evaluate_function(window, &step.args)?);
            }
            "click" | "mousedown" | "mouseup" | "dblclick" | "hover" => {
                let script = pointer_script(&step.op, &step.args)?;
                let _ = eval_json(window, &script, DEFAULT_EVAL_TIMEOUT)?;
            }
            "file" => upload_files(window, &step.args, false)?,
            "drop" => upload_files(window, &step.args, true)?,
            "download" => results.push(Value::String(execute_download(
                registry,
                window,
                reservation,
                plugin_id,
                &step.args,
                || asset_server.is_active_surface_for(parent_lease_id, plugin_id),
            )?)),
            "input" => {
                let script = input_script(&step.args)?;
                let _ = eval_json(window, &script, DEFAULT_EVAL_TIMEOUT)?;
            }
            "value" => {
                require_arg_count(step, 2)?;
                let selector = required_string_arg(step, 0, "value selector")?;
                let value = required_string_arg(step, 1, "value")?;
                let script = dom_value_script(selector, value, false)?;
                let _ = eval_json(window, &script, DEFAULT_EVAL_TIMEOUT)?;
            }
            "check" => {
                require_arg_count(step, 2)?;
                let selector = required_string_arg(step, 0, "check selector")?;
                let checked = step.args[1]
                    .as_bool()
                    .ok_or_else(|| "ubrowser.check requires a boolean.".to_owned())?;
                let script = dom_check_script(selector, checked)?;
                let _ = eval_json(window, &script, DEFAULT_EVAL_TIMEOUT)?;
            }
            "focus" => {
                require_arg_count(step, 1)?;
                let selector = required_string_arg(step, 0, "focus selector")?;
                let script = element_action_script(selector, "element.focus(); return null;")?;
                let _ = eval_json(window, &script, DEFAULT_EVAL_TIMEOUT)?;
            }
            "scroll" => {
                let script = scroll_script(&step.args)?;
                let _ = eval_json(window, &script, DEFAULT_EVAL_TIMEOUT)?;
            }
            "press" => {
                let script = press_script(&step.args)?;
                let _ = eval_json(window, &script, DEFAULT_EVAL_TIMEOUT)?;
            }
            "paste" => {
                require_arg_count(step, 1)?;
                let text = required_string_arg(step, 0, "paste")?;
                if let Some(image) = image_paste_payload(text)? {
                    paste_image(window, image)?;
                } else {
                    let script = input_script(&[Value::String(text.to_owned())])?;
                    let _ = eval_json(window, &script, DEFAULT_EVAL_TIMEOUT)?;
                }
            }
            "markdown" => results.push(markdown(window, &step.args)?),
            "screenshot" => results.push(Value::String(capture_screenshot(window, &step.args)?)),
            "pdf" => results.push(Value::String(print_pdf(window, &step.args)?)),
            "cookies" => results.push(read_cookies(window, &step.args)?),
            "setCookies" => set_cookies(window, &step.args)?,
            "removeCookies" => {
                require_arg_count(step, 1)?;
                let name = required_string_arg(step, 0, "removeCookies")?;
                validate_cookie_name(name)?;
                remove_cookies(window, name)?;
            }
            "clearCookies" => clear_cookies(window, &step.args)?,
            "device" => apply_device(window, &step.args)?,
            "wait" => execute_wait(window, &step.args, started, || {
                asset_server.is_active_surface_for(parent_lease_id, plugin_id)
            })?,
            "devTools" => {
                if step.args.len() > 1 {
                    return Err("ubrowser.devTools accepts at most one mode.".to_owned());
                }
                #[cfg(debug_assertions)]
                window.open_devtools();
            }
            operation => {
                return Err(format!(
                    "uTools ubrowser operation '{operation}' is not implemented by this execution phase."
                ));
            }
        }
    }
    let instance = snapshot_instance(window, &reservation.instance_id)?;
    results.push(
        serde_json::to_value(instance)
            .map_err(|error| format!("Could not encode the ubrowser instance: {error}"))?,
    );
    Ok(Value::Array(results))
}

fn eval_json(window: &WebviewWindow, expression: &str, timeout: Duration) -> Result<Value, String> {
    if expression.chars().count() > MAX_SCRIPT_CHARS || expression.len() > MAX_SCRIPT_BYTES {
        return Err("uTools ubrowser script exceeds the execution limit.".to_owned());
    }
    let token = Uuid::new_v4().to_string();
    let encoded_token = serde_json::to_string(&token).map_err(|error| error.to_string())?;
    let script = format!(
        "(() => {{ try {{ const value=({expression}); if (value && typeof value.then === 'function') {{ const key={encoded_token}; const store=globalThis.__ihubUbrowserAsync||(globalThis.__ihubUbrowserAsync=new Map()); store.set(key,{{pending:true}}); Promise.resolve(value).then((resolved)=>store.set(key,{{ok:true,value:resolved===undefined?null:resolved}}),(error)=>store.set(key,{{ok:false,error:String(error&&error.message?error.message:error).slice(0,2000)}})); return {{ok:true,value:{{__ihubAsync:key}}}}; }} return {{ok:true,value:value===undefined?null:value}}; }} catch (error) {{ return {{ok:false,error:String(error&&error.message?error.message:error).slice(0,2000)}}; }} }})()"
    );
    let started = Instant::now();
    let value = eval_envelope_once(window, script, timeout)?;
    let Some(async_token) = value
        .as_object()
        .and_then(|value| value.get("__ihubAsync"))
        .and_then(Value::as_str)
    else {
        return Ok(value);
    };
    if async_token != token {
        return Err("uTools ubrowser returned an invalid async evaluation token.".to_owned());
    }
    loop {
        if started.elapsed() >= timeout {
            return Err("uTools ubrowser asynchronous JavaScript timed out.".to_owned());
        }
        std::thread::sleep(Duration::from_millis(25));
        let remaining = timeout.saturating_sub(started.elapsed());
        let poll = format!(
            "(() => {{ const store=globalThis.__ihubUbrowserAsync; const key={encoded_token}; const item=store&&store.get(key); if(!item)return {{ok:false,error:'The async page result was lost during navigation.'}}; if(item.pending)return {{ok:true,value:{{__ihubPending:true}}}}; store.delete(key); return item; }})()"
        );
        let value = eval_envelope_once(window, poll, remaining)?;
        if value
            .as_object()
            .and_then(|value| value.get("__ihubPending"))
            .and_then(Value::as_bool)
            != Some(true)
        {
            return Ok(value);
        }
    }
}

fn eval_envelope_once(
    window: &WebviewWindow,
    script: String,
    timeout: Duration,
) -> Result<Value, String> {
    let (sender, receiver) = mpsc::sync_channel(1);
    window
        .eval_with_callback(script, move |raw| {
            let _ = sender.try_send(raw);
        })
        .map_err(|error| format!("Could not execute uTools ubrowser JavaScript: {error}"))?;
    let raw = receiver
        .recv_timeout(timeout)
        .map_err(|_| "uTools ubrowser JavaScript timed out.".to_owned())?;
    if raw.len() > MAX_RESULT_BYTES {
        return Err("uTools ubrowser JavaScript result exceeds 512 KiB.".to_owned());
    }
    let mut value: Value = serde_json::from_str(&raw)
        .map_err(|_| "uTools ubrowser returned malformed JavaScript output.".to_owned())?;
    if let Some(encoded) = value.as_str() {
        if let Ok(decoded) = serde_json::from_str::<Value>(encoded) {
            value = decoded;
        }
    }
    let object = value
        .as_object()
        .ok_or_else(|| "uTools ubrowser returned an invalid JavaScript envelope.".to_owned())?;
    if object.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(object
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("uTools ubrowser JavaScript failed.")
            .to_owned());
    }
    Ok(object.get("value").cloned().unwrap_or(Value::Null))
}

const SELECTOR_HELPER: &str = r#"
const ihubFind=(selector)=>{
  if(typeof selector!=='string'||selector.length===0)throw new TypeError('A non-empty selector is required.');
  let root=document;
  const parts=selector.split('>>').map(part=>part.trim()).filter(Boolean);
  if(parts.length===0||parts.length>8)throw new Error('The selector iframe depth is invalid.');
  let element=null;
  for(let index=0;index<parts.length;index+=1){
    const part=parts[index];
    const doc=root.nodeType===9?root:root.ownerDocument;
    element=(part.startsWith('/')||part.startsWith('('))
      ?doc.evaluate(part,root,null,XPathResult.FIRST_ORDERED_NODE_TYPE,null).singleNodeValue
      :root.querySelector(part);
    if(!element)throw new Error('No element matched: '+part);
    if(index+1<parts.length){
      if(!(element instanceof HTMLIFrameElement)||!element.contentDocument)throw new Error('The iframe selector is unavailable or cross-origin.');
      root=element.contentDocument;
    }
  }
  return element;
};
"#;

fn element_action_script(selector: &str, action: &str) -> Result<String, String> {
    validate_selector(selector)?;
    let selector = serde_json::to_string(selector).map_err(|error| error.to_string())?;
    Ok(format!(
        "(() => {{{SELECTOR_HELPER} const element=ihubFind({selector}); {action} }})()"
    ))
}

fn pointer_script(operation: &str, args: &[Value]) -> Result<String, String> {
    if args.is_empty() || args.len() > 3 {
        return Err(format!("ubrowser.{operation} received invalid arguments."));
    }
    let (target, button_index) = if let Some(selector) = args[0].as_str() {
        validate_selector(selector)?;
        (
            format!(
                "ihubFind({})",
                serde_json::to_string(selector).map_err(|error| error.to_string())?
            ),
            1,
        )
    } else {
        if args.len() < 2 {
            return Err(format!("ubrowser.{operation} coordinates require x and y."));
        }
        let x = finite_number(&args[0], -100_000.0, 100_000.0, "pointer x")?;
        let y = finite_number(&args[1], -100_000.0, 100_000.0, "pointer y")?;
        (format!("document.elementFromPoint({x},{y})"), 2)
    };
    let button = args
        .get(button_index)
        .and_then(Value::as_str)
        .unwrap_or("left");
    let button_number = match button {
        "left" => 0,
        "middle" => 1,
        "right" => 2,
        _ => return Err("uTools ubrowser mouse button is invalid.".to_owned()),
    };
    let event_name = match operation {
        "click" => "click",
        "mousedown" => "mousedown",
        "mouseup" => "mouseup",
        "dblclick" => "dblclick",
        "hover" => "mousemove",
        _ => return Err("Unsupported ubrowser pointer operation.".to_owned()),
    };
    let action = if operation == "click" {
        "if(typeof element.click==='function')element.click();else element.dispatchEvent(new MouseEvent('click',eventInit));"
    } else {
        "element.dispatchEvent(new MouseEvent(eventName,eventInit));"
    };
    Ok(format!(
        "(() => {{{SELECTOR_HELPER} const element={target}; if(!element)throw new Error('No element exists at the requested point.'); const eventName='{event_name}'; const eventInit={{bubbles:true,cancelable:true,view:window,button:{button_number},detail:eventName==='dblclick'?2:1}}; {action} return null; }})()"
    ))
}

const MAX_UPLOAD_FILES: usize = 8;
const MAX_UPLOAD_BYTES: u64 = 32 * 1024 * 1024;
const UPLOAD_CHUNK_BYTES: usize = 96 * 1024;

fn upload_files(window: &WebviewWindow, args: &[Value], drop_event: bool) -> Result<(), String> {
    if args.len() != 2 {
        return Err("ubrowser.file/drop requires a selector and file payload.".to_owned());
    }
    let selector = required_string_value(&args[0], "file selector")?;
    validate_selector(selector)?;
    let files = load_upload_payload(&args[1])?;
    let token = Uuid::new_v4().to_string();
    let token_json = serde_json::to_string(&token).map_err(|error| error.to_string())?;
    let _ = eval_json(
        window,
        &format!(
            "(() => {{ const store=globalThis.__ihubUbrowserUploads||(globalThis.__ihubUbrowserUploads=new Map()); store.set({token_json},[]); return null; }})()"
        ),
        DEFAULT_EVAL_TIMEOUT,
    )?;
    for (name, bytes) in files {
        let name_json = serde_json::to_string(&name).map_err(|error| error.to_string())?;
        let _ = eval_json(
            window,
            &format!(
                "(() => {{ const files=globalThis.__ihubUbrowserUploads.get({token_json}); if(!files)throw new Error('The upload staging slot was lost.'); files.push({{name:{name_json},parts:[]}}); return files.length-1; }})()"
            ),
            DEFAULT_EVAL_TIMEOUT,
        )?;
        for chunk in bytes.chunks(UPLOAD_CHUNK_BYTES) {
            let encoded = BASE64_STANDARD.encode(chunk);
            let encoded_json =
                serde_json::to_string(&encoded).map_err(|error| error.to_string())?;
            let _ = eval_json(
                window,
                &format!(
                    "(() => {{ const files=globalThis.__ihubUbrowserUploads.get({token_json}); if(!files?.length)throw new Error('The upload staging file was lost.'); files[files.length-1].parts.push({encoded_json}); return null; }})()"
                ),
                DEFAULT_EVAL_TIMEOUT,
            )?;
        }
    }
    let selector_json = serde_json::to_string(selector).map_err(|error| error.to_string())?;
    let action = if drop_event {
        "element.dispatchEvent(new DragEvent('dragenter',{bubbles:true,cancelable:true,dataTransfer:transfer})); element.dispatchEvent(new DragEvent('dragover',{bubbles:true,cancelable:true,dataTransfer:transfer})); element.dispatchEvent(new DragEvent('drop',{bubbles:true,cancelable:true,dataTransfer:transfer}));"
    } else {
        "if(!(element instanceof HTMLInputElement)||element.type!=='file')throw new Error('The selected element is not an input[type=file].'); element.files=transfer.files; element.dispatchEvent(new Event('input',{bubbles:true})); element.dispatchEvent(new Event('change',{bubbles:true}));"
    };
    let finalize = format!(
        "(() => {{{SELECTOR_HELPER} const store=globalThis.__ihubUbrowserUploads; const staged=store&&store.get({token_json}); if(!staged)throw new Error('The upload staging slot was lost.'); const transfer=new DataTransfer(); for(const item of staged){{ const parts=item.parts.map((encoded)=>{{const binary=atob(encoded);const bytes=new Uint8Array(binary.length);for(let i=0;i<binary.length;i+=1)bytes[i]=binary.charCodeAt(i);return bytes;}}); transfer.items.add(new File(parts,item.name,{{type:'application/octet-stream'}})); }} const element=ihubFind({selector_json}); {action} store.delete({token_json}); return null; }})()"
    );
    eval_json(window, &finalize, DEFAULT_EVAL_TIMEOUT).map(|_| ())
}

fn load_upload_payload(payload: &Value) -> Result<Vec<(String, Vec<u8>)>, String> {
    if let Some(path) = payload.as_str() {
        return Ok(vec![read_upload_file(Path::new(path))?]);
    }
    if let Some(paths) = payload.as_array() {
        if paths.is_empty() || paths.len() > MAX_UPLOAD_FILES {
            return Err(format!(
                "ubrowser.file accepts 1-{MAX_UPLOAD_FILES} file paths."
            ));
        }
        let mut files = Vec::with_capacity(paths.len());
        let mut total = 0_u64;
        for path in paths {
            let path = path
                .as_str()
                .ok_or_else(|| "ubrowser.file path arrays may contain only strings.".to_owned())?;
            let file = read_upload_file(Path::new(path))?;
            total = total.saturating_add(file.1.len() as u64);
            if total > MAX_UPLOAD_BYTES {
                return Err("uTools ubrowser upload payload exceeds 32 MiB.".to_owned());
            }
            files.push(file);
        }
        return Ok(files);
    }
    let encoded = payload
        .as_object()
        .filter(|object| object.len() == 1)
        .and_then(|object| object.get("__ihubBytesBase64"))
        .and_then(Value::as_str)
        .ok_or_else(|| "ubrowser.file payload must be a path, path array, or Buffer.".to_owned())?;
    let bytes = BASE64_STANDARD
        .decode(encoded)
        .map_err(|_| "uTools ubrowser Buffer payload is malformed.".to_owned())?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_UPLOAD_BYTES {
        return Err("uTools ubrowser Buffer payload is empty or exceeds 32 MiB.".to_owned());
    }
    Ok(vec![("upload.bin".to_owned(), bytes)])
}

fn read_upload_file(path: &Path) -> Result<(String, Vec<u8>), String> {
    if !path.is_absolute() {
        return Err("uTools ubrowser upload paths must be absolute.".to_owned());
    }
    let path = fs::canonicalize(path)
        .map_err(|error| format!("Could not resolve ubrowser upload file: {error}"))?;
    let metadata = fs::metadata(&path)
        .map_err(|error| format!("Could not inspect ubrowser upload file: {error}"))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_UPLOAD_BYTES {
        return Err("uTools ubrowser uploads require a regular file up to 32 MiB.".to_owned());
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && name.len() <= 255)
        .ok_or_else(|| "uTools ubrowser upload filename is invalid.".to_owned())?
        .to_owned();
    let bytes =
        fs::read(&path).map_err(|error| format!("Could not read ubrowser upload file: {error}"))?;
    Ok((name, bytes))
}

struct ImagePastePayload {
    name: String,
    mime: String,
    bytes: Vec<u8>,
}

fn image_paste_payload(value: &str) -> Result<Option<ImagePastePayload>, String> {
    if let Some(rest) = value.strip_prefix("data:image/") {
        let (metadata, encoded) = rest
            .split_once(',')
            .ok_or_else(|| "uTools ubrowser image data URL is malformed.".to_owned())?;
        let subtype = metadata
            .strip_suffix(";base64")
            .filter(|subtype| matches!(*subtype, "png" | "jpeg" | "jpg" | "webp" | "gif"))
            .ok_or_else(|| {
                "uTools ubrowser image paste supports base64 PNG/JPEG/WebP/GIF.".to_owned()
            })?;
        let bytes = BASE64_STANDARD
            .decode(encoded)
            .map_err(|_| "uTools ubrowser image data URL is malformed.".to_owned())?;
        if bytes.is_empty() || bytes.len() > 16 * 1024 * 1024 {
            return Err("uTools ubrowser pasted image is empty or exceeds 16 MiB.".to_owned());
        }
        let extension = if subtype == "jpg" { "jpeg" } else { subtype };
        return Ok(Some(ImagePastePayload {
            name: format!("pasted-image.{extension}"),
            mime: format!("image/{extension}"),
            bytes,
        }));
    }
    let path = Path::new(value);
    if !path.is_absolute() || !path.exists() {
        return Ok(None);
    }
    let (name, bytes) = read_upload_file(path)?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err("uTools ubrowser pasted image exceeds 16 MiB.".to_owned());
    }
    let extension = Path::new(&name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| "uTools ubrowser pasted image has no supported extension.".to_owned())?;
    let mime = match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => {
            return Err("uTools ubrowser image paste supports PNG/JPEG/WebP/GIF files.".to_owned())
        }
    };
    Ok(Some(ImagePastePayload {
        name,
        mime: mime.to_owned(),
        bytes,
    }))
}

fn paste_image(window: &WebviewWindow, image: ImagePastePayload) -> Result<(), String> {
    let token = Uuid::new_v4().to_string();
    let token = serde_json::to_string(&token).map_err(|error| error.to_string())?;
    let _ = eval_json(
        window,
        &format!(
            "(() => {{ const store=globalThis.__ihubUbrowserPaste||(globalThis.__ihubUbrowserPaste=new Map()); store.set({token},[]); return null; }})()"
        ),
        DEFAULT_EVAL_TIMEOUT,
    )?;
    for chunk in image.bytes.chunks(UPLOAD_CHUNK_BYTES) {
        let encoded = serde_json::to_string(&BASE64_STANDARD.encode(chunk))
            .map_err(|error| error.to_string())?;
        let _ = eval_json(
            window,
            &format!(
                "(() => {{ const parts=globalThis.__ihubUbrowserPaste.get({token}); if(!parts)throw new Error('The image paste slot was lost.'); parts.push({encoded}); return null; }})()"
            ),
            DEFAULT_EVAL_TIMEOUT,
        )?;
    }
    let name = serde_json::to_string(&image.name).map_err(|error| error.to_string())?;
    let mime = serde_json::to_string(&image.mime).map_err(|error| error.to_string())?;
    let script = format!(
        "(() => {{ const store=globalThis.__ihubUbrowserPaste; const encodedParts=store&&store.get({token}); if(!encodedParts)throw new Error('The image paste slot was lost.'); const parts=encodedParts.map((encoded)=>{{const binary=atob(encoded);const bytes=new Uint8Array(binary.length);for(let i=0;i<binary.length;i+=1)bytes[i]=binary.charCodeAt(i);return bytes;}}); const transfer=new DataTransfer(); transfer.items.add(new File(parts,{name},{{type:{mime}}})); const target=document.activeElement||document.body; target.dispatchEvent(new ClipboardEvent('paste',{{bubbles:true,cancelable:true,clipboardData:transfer}})); store.delete({token}); return null; }})()"
    );
    eval_json(window, &script, DEFAULT_EVAL_TIMEOUT).map(|_| ())
}

fn required_string_value<'a>(value: &'a Value, label: &str) -> Result<&'a str, String> {
    value
        .as_str()
        .ok_or_else(|| format!("uTools ubrowser {label} must be a string."))
}

const MAX_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;

fn execute_download(
    registry: &UtoolsUBrowserRegistry,
    window: &WebviewWindow,
    reservation: &RunReservation,
    plugin_id: &str,
    args: &[Value],
    still_active: impl Fn() -> bool,
) -> Result<String, String> {
    if args.is_empty() {
        return Err("ubrowser.download requires a URL or page function.".to_owned());
    }
    let save_path = match args.get(1) {
        None | Some(Value::Null) => None,
        Some(Value::String(path)) => {
            let path = PathBuf::from(path);
            validate_output_path(&path, "download")?;
            Some(path)
        }
        _ => return Err("ubrowser.download save path must be a string or null.".to_owned()),
    };
    if let Some(url) = args[0].as_str() {
        let url = validate_navigation_url(url)?;
        return host_download(window, registry, plugin_id, url, save_path);
    }
    let source = function_source(&args[0])?;
    let params = serde_json::to_string(&args[2..]).map_err(|error| error.to_string())?;
    let generation = registry.prepare_download(&reservation.label, save_path.clone())?;
    let result = eval_json(
        window,
        &format!("(() => {{ const fn=({source}); return fn(...{params}); }})()"),
        DEFAULT_EVAL_TIMEOUT,
    );
    let returned_url = match result {
        Ok(Value::String(url)) => Some(validate_navigation_url(&url)?),
        Ok(Value::Null) => None,
        Ok(_) => {
            registry.cancel_download(&reservation.label);
            return Err(
                "ubrowser.download page function must return a URL or trigger a download."
                    .to_owned(),
            );
        }
        Err(error) => {
            registry.cancel_download(&reservation.label);
            return Err(error);
        }
    };
    if registry.download_started(&reservation.label) {
        return registry
            .wait_for_download(
                &reservation.label,
                generation,
                Duration::from_secs(60),
                still_active,
            )
            .map(|path| path.to_string_lossy().into_owned());
    }
    if let Some(url) = returned_url {
        registry.cancel_download(&reservation.label);
        return host_download(window, registry, plugin_id, url, save_path);
    }
    registry
        .wait_for_download(
            &reservation.label,
            generation,
            Duration::from_secs(60),
            still_active,
        )
        .map(|path| path.to_string_lossy().into_owned())
}

fn host_download(
    window: &WebviewWindow,
    registry: &UtoolsUBrowserRegistry,
    plugin_id: &str,
    url: Url,
    save_path: Option<PathBuf>,
) -> Result<String, String> {
    let path = match save_path {
        Some(path) => path,
        None => {
            let directory = window
                .app_handle()
                .path()
                .download_dir()
                .or_else(|_| window.app_handle().path().temp_dir())
                .map_err(|error| {
                    format!("Could not resolve the ubrowser download directory: {error}")
                })?;
            let filename = safe_download_filename(&url);
            let candidate = directory.join(&filename);
            if candidate.exists() {
                directory.join(format!("{}-{filename}", Uuid::new_v4().simple()))
            } else {
                candidate
            }
        }
    };
    validate_output_path(&path, "download")?;
    let cookies = window
        .cookies_for_url(url.clone())
        .map_err(|error| format!("Could not read ubrowser download cookies: {error}"))?;
    let cookie_header = cookies
        .iter()
        .map(|cookie| format!("{}={}", cookie.name(), cookie.value()))
        .collect::<Vec<_>>()
        .join("; ");
    let referer = window
        .url()
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"));
    let proxy = registry.proxy_for(plugin_id);
    let temp_path = path.with_file_name(format!(
        ".ihub-ubrowser-download-{}.part",
        Uuid::new_v4().simple()
    ));
    let async_temp_path = temp_path.clone();
    let result =
        tauri::async_runtime::block_on(async move {
            let mut builder = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::limited(10))
                .user_agent("iHub uTools-compatible ubrowser");
            if let Some(proxy) = proxy {
                builder = builder.proxy(reqwest::Proxy::all(proxy.as_str()).map_err(|error| {
                    format!("Could not configure the ubrowser download proxy: {error}")
                })?);
            }
            let client = builder.build().map_err(|error| {
                format!("Could not create the ubrowser download client: {error}")
            })?;
            let mut request = client.get(url);
            if !cookie_header.is_empty() {
                request = request.header(reqwest::header::COOKIE, cookie_header);
            }
            if let Some(referer) = referer {
                request = request.header(reqwest::header::REFERER, referer.as_str());
            }
            let mut response = request
                .send()
                .await
                .map_err(|error| format!("uTools ubrowser download request failed: {error}"))?
                .error_for_status()
                .map_err(|error| {
                    format!("uTools ubrowser download server rejected the request: {error}")
                })?;
            if response
                .content_length()
                .is_some_and(|length| length > MAX_DOWNLOAD_BYTES)
            {
                return Err("uTools ubrowser download exceeds 256 MiB.".to_owned());
            }
            let mut file = fs::File::create(&async_temp_path).map_err(|error| {
                format!("Could not create the ubrowser download staging file: {error}")
            })?;
            let mut written = 0_u64;
            while let Some(chunk) = response.chunk().await.map_err(|error| {
                format!("Could not read the ubrowser download response: {error}")
            })? {
                written = written.saturating_add(chunk.len() as u64);
                if written > MAX_DOWNLOAD_BYTES {
                    return Err("uTools ubrowser download exceeds 256 MiB.".to_owned());
                }
                file.write_all(&chunk)
                    .map_err(|error| format!("Could not write the ubrowser download: {error}"))?;
            }
            file.sync_all()
                .map_err(|error| format!("Could not flush the ubrowser download: {error}"))?;
            if written == 0 {
                return Err("The uTools ubrowser download was empty.".to_owned());
            }
            Ok::<(), String>(())
        });
    if let Err(error) = result {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    fs::copy(&temp_path, &path)
        .map_err(|error| format!("Could not publish the uTools ubrowser download: {error}"))?;
    let _ = fs::remove_file(&temp_path);
    Ok(path.to_string_lossy().into_owned())
}

fn safe_download_filename(url: &Url) -> String {
    let candidate = url
        .path_segments()
        .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
        .unwrap_or("download.bin");
    let mut filename = candidate
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .take(128)
        .collect::<String>();
    if filename.is_empty() || filename == "." || filename == ".." {
        filename = "download.bin".to_owned();
    }
    filename
}

fn input_script(args: &[Value]) -> Result<String, String> {
    let (selector, text) = match args {
        [text] => (None, text.as_str()),
        [selector, text] => (selector.as_str(), text.as_str()),
        _ => return Err("ubrowser.input requires text or selector and text.".to_owned()),
    };
    let text = text.ok_or_else(|| "ubrowser.input text must be a string.".to_owned())?;
    validate_bounded_string(text, "input text", 65_536)?;
    let text = serde_json::to_string(text).map_err(|error| error.to_string())?;
    if let Some(selector) = selector {
        validate_selector(selector)?;
        let selector = serde_json::to_string(selector).map_err(|error| error.to_string())?;
        Ok(format!(
            "(() => {{{SELECTOR_HELPER} const element=ihubFind({selector}); element.focus(); const next=String(element.value??'')+{text}; element.value=next; element.dispatchEvent(new InputEvent('input',{{bubbles:true,inputType:'insertText',data:{text}}})); return null; }})()"
        ))
    } else {
        Ok(format!(
            "(() => {{ const element=document.activeElement; if(!element||!('value' in element))throw new Error('No editable element is focused.'); element.value=String(element.value??'')+{text}; element.dispatchEvent(new InputEvent('input',{{bubbles:true,inputType:'insertText',data:{text}}})); return null; }})()"
        ))
    }
}

fn dom_value_script(selector: &str, value: &str, append: bool) -> Result<String, String> {
    validate_selector(selector)?;
    validate_bounded_string(value, "value", 65_536)?;
    let selector = serde_json::to_string(selector).map_err(|error| error.to_string())?;
    let value = serde_json::to_string(value).map_err(|error| error.to_string())?;
    let assignment = if append {
        format!("String(element.value??'')+{value}")
    } else {
        value
    };
    Ok(format!(
        "(() => {{{SELECTOR_HELPER} const element=ihubFind({selector}); if(!('value' in element))throw new Error('The selected element has no value.'); element.value={assignment}; element.dispatchEvent(new Event('input',{{bubbles:true}})); element.dispatchEvent(new Event('change',{{bubbles:true}})); return null; }})()"
    ))
}

fn dom_check_script(selector: &str, checked: bool) -> Result<String, String> {
    validate_selector(selector)?;
    let selector = serde_json::to_string(selector).map_err(|error| error.to_string())?;
    Ok(format!(
        "(() => {{{SELECTOR_HELPER} const element=ihubFind({selector}); if(!('checked' in element))throw new Error('The selected element is not checkable.'); element.checked={checked}; element.dispatchEvent(new Event('input',{{bubbles:true}})); element.dispatchEvent(new Event('change',{{bubbles:true}})); return null; }})()"
    ))
}

fn scroll_script(args: &[Value]) -> Result<String, String> {
    match args {
        [selector] if selector.is_string() => {
            let selector = selector.as_str().expect("guarded string");
            validate_selector(selector)?;
            element_action_script(
                selector,
                "element.scrollIntoView({block:'center'}); return null;",
            )
        }
        [y] => {
            let y = finite_number(y, -1_000_000.0, 1_000_000.0, "scroll y")?;
            Ok(format!(
                "(() => {{ window.scrollTo(0,{y}); return null; }})()"
            ))
        }
        [x, y] => {
            let x = finite_number(x, -1_000_000.0, 1_000_000.0, "scroll x")?;
            let y = finite_number(y, -1_000_000.0, 1_000_000.0, "scroll y")?;
            Ok(format!(
                "(() => {{ window.scrollTo({x},{y}); return null; }})()"
            ))
        }
        _ => Err("ubrowser.scroll received invalid arguments.".to_owned()),
    }
}

fn press_script(args: &[Value]) -> Result<String, String> {
    if args.is_empty() || args.len() > 5 {
        return Err("ubrowser.press requires a key and up to four modifiers.".to_owned());
    }
    let key = args[0]
        .as_str()
        .ok_or_else(|| "ubrowser.press key must be a string.".to_owned())?;
    validate_bounded_string(key, "key", 40)?;
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut meta = false;
    for modifier in &args[1..] {
        match modifier.as_str() {
            Some("ctrl" | "control") => ctrl = true,
            Some("alt") => alt = true,
            Some("shift") => shift = true,
            Some("meta" | "command") => meta = true,
            _ => return Err("ubrowser.press modifier is invalid.".to_owned()),
        }
    }
    let key = serde_json::to_string(key).map_err(|error| error.to_string())?;
    Ok(format!(
        "(() => {{ const target=document.activeElement||document.body; const init={{key:{key},bubbles:true,cancelable:true,ctrlKey:{ctrl},altKey:{alt},shiftKey:{shift},metaKey:{meta}}}; target.dispatchEvent(new KeyboardEvent('keydown',init)); target.dispatchEvent(new KeyboardEvent('keyup',init)); if(({key}).toLowerCase()==='enter'&&target.form&&typeof target.form.requestSubmit==='function')target.form.requestSubmit(); return null; }})()"
    ))
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ScreenshotRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    #[serde(default)]
    viewport_width: f64,
    #[serde(default)]
    viewport_height: f64,
}

fn capture_screenshot(window: &WebviewWindow, args: &[Value]) -> Result<String, String> {
    if args.len() > 2 {
        return Err("ubrowser.screenshot accepts a target and optional save path.".to_owned());
    }
    let crop = match args.first() {
        None | Some(Value::Null) => None,
        Some(Value::String(selector)) => {
            validate_selector(selector)?;
            let selector = serde_json::to_string(selector).map_err(|error| error.to_string())?;
            let value = eval_json(
                window,
                &format!(
                    "(() => {{{SELECTOR_HELPER} const rect=ihubFind({selector}).getBoundingClientRect(); return {{x:rect.x,y:rect.y,width:rect.width,height:rect.height,viewportWidth:innerWidth,viewportHeight:innerHeight}}; }})()"
                ),
                DEFAULT_EVAL_TIMEOUT,
            )?;
            Some(
                serde_json::from_value::<ScreenshotRect>(value).map_err(|error| {
                    format!("Could not read screenshot element bounds: {error}")
                })?,
            )
        }
        Some(Value::Object(_)) => {
            let mut rect = serde_json::from_value::<ScreenshotRect>(args[0].clone())
                .map_err(|error| format!("ubrowser.screenshot rectangle is invalid: {error}"))?;
            let viewport = eval_json(
                window,
                "(() => ({width:innerWidth,height:innerHeight}))()",
                DEFAULT_EVAL_TIMEOUT,
            )?;
            rect.viewport_width = viewport
                .get("width")
                .and_then(Value::as_f64)
                .unwrap_or_default();
            rect.viewport_height = viewport
                .get("height")
                .and_then(Value::as_f64)
                .unwrap_or_default();
            Some(rect)
        }
        _ => return Err("ubrowser.screenshot target must be a selector or rectangle.".to_owned()),
    };
    let save_path = screenshot_save_path(window, args.get(1))?;
    let mut png = capture_webview_png(window)?;
    if let Some(rect) = crop {
        validate_screenshot_rect(rect)?;
        let image = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .map_err(|error| format!("Could not decode the ubrowser screenshot: {error}"))?;
        let scale_x = f64::from(image.width()) / rect.viewport_width;
        let scale_y = f64::from(image.height()) / rect.viewport_height;
        let x = (rect.x.max(0.0) * scale_x).floor() as u32;
        let y = (rect.y.max(0.0) * scale_y).floor() as u32;
        let width = (rect.width * scale_x).ceil() as u32;
        let height = (rect.height * scale_y).ceil() as u32;
        if width == 0
            || height == 0
            || x >= image.width()
            || y >= image.height()
            || x.saturating_add(width) > image.width()
            || y.saturating_add(height) > image.height()
        {
            return Err("The ubrowser screenshot rectangle is outside the viewport.".to_owned());
        }
        let cropped = image.crop_imm(x, y, width, height);
        let mut cursor = std::io::Cursor::new(Vec::new());
        cropped
            .write_to(&mut cursor, image::ImageFormat::Png)
            .map_err(|error| {
                format!("Could not encode the cropped ubrowser screenshot: {error}")
            })?;
        png = cursor.into_inner();
    }
    if png.is_empty() || png.len() > 32 * 1024 * 1024 {
        return Err("The uTools ubrowser screenshot is empty or exceeds 32 MiB.".to_owned());
    }
    fs::write(&save_path, png)
        .map_err(|error| format!("Could not save the uTools ubrowser screenshot: {error}"))?;
    Ok(save_path.to_string_lossy().into_owned())
}

fn validate_screenshot_rect(rect: ScreenshotRect) -> Result<(), String> {
    if [
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        rect.viewport_width,
        rect.viewport_height,
    ]
    .into_iter()
    .any(|value| !value.is_finite())
        || rect.x < -100_000.0
        || rect.y < -100_000.0
        || rect.width <= 0.0
        || rect.height <= 0.0
        || rect.width > 16_384.0
        || rect.height > 16_384.0
        || rect.viewport_width <= 0.0
        || rect.viewport_height <= 0.0
    {
        return Err("ubrowser.screenshot rectangle is outside the supported bounds.".to_owned());
    }
    Ok(())
}

fn screenshot_save_path(window: &WebviewWindow, value: Option<&Value>) -> Result<PathBuf, String> {
    let path = match value {
        None | Some(Value::Null) => window
            .app_handle()
            .path()
            .temp_dir()
            .map_err(|error| format!("Could not resolve the screenshot temp directory: {error}"))?
            .join(format!("ihub-ubrowser-{}.png", Uuid::new_v4())),
        Some(Value::String(path)) => PathBuf::from(path),
        _ => return Err("ubrowser.screenshot save path must be a string.".to_owned()),
    };
    validate_output_path(&path, "screenshot")?;
    Ok(path)
}

fn validate_output_path(path: &Path, label: &str) -> Result<(), String> {
    if !path.is_absolute() || path.as_os_str().len() > 32_768 {
        return Err(format!(
            "uTools ubrowser {label} path must be bounded and absolute."
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("uTools ubrowser {label} path has no parent directory."))?;
    let filename = path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 255
                && !value.ends_with(['.', ' '])
                && !value
                    .chars()
                    .any(|character| character.is_control() || r#"<>:"/\|?*"#.contains(character))
        })
        .ok_or_else(|| format!("uTools ubrowser {label} filename is invalid."))?;
    let stem = filename
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    ) {
        return Err(format!("uTools ubrowser {label} filename is reserved."));
    }
    let parent = fs::canonicalize(parent)
        .map_err(|error| format!("Could not resolve the ubrowser {label} directory: {error}"))?;
    if !parent.is_dir() {
        return Err(format!(
            "The uTools ubrowser {label} parent is not a directory."
        ));
    }
    if path.exists() {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| format!("Could not inspect the ubrowser {label} target: {error}"))?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err(format!(
                "The uTools ubrowser {label} target must be a regular file."
            ));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn capture_webview_png(window: &WebviewWindow) -> Result<Vec<u8>, String> {
    use webview2_com::{
        CapturePreviewCompletedHandler,
        Microsoft::Web::WebView2::Win32::COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,
    };
    use windows::{
        core::HSTRING,
        Win32::{
            Storage::FileSystem::FILE_ATTRIBUTE_NORMAL,
            System::Com::{IStream, STGM_CREATE, STGM_SHARE_EXCLUSIVE, STGM_WRITE},
            UI::Shell::SHCreateStreamOnFileEx,
        },
    };

    let capture_path = window
        .app_handle()
        .path()
        .temp_dir()
        .map_err(|error| format!("Could not resolve the screenshot temp directory: {error}"))?
        .join(format!("ihub-ubrowser-capture-{}.png", Uuid::new_v4()));
    let closure_capture_path = capture_path.clone();
    let (sender, receiver) = mpsc::sync_channel::<Result<(), String>>(1);
    let callback_sender = sender.clone();
    window
        .with_webview(move |platform| {
            let result = unsafe {
                let path = HSTRING::from(closure_capture_path.to_string_lossy().as_ref());
                SHCreateStreamOnFileEx(
                    &path,
                    (STGM_CREATE | STGM_WRITE | STGM_SHARE_EXCLUSIVE).0,
                    FILE_ATTRIBUTE_NORMAL.0,
                    true,
                    None::<&IStream>,
                )
                .and_then(|stream| {
                    platform.controller().CoreWebView2().and_then(|webview| {
                        webview.CapturePreview(
                            COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,
                            &stream,
                            &CapturePreviewCompletedHandler::create(Box::new(move |result| {
                                let _ =
                                    callback_sender.send(result.map_err(|error| error.to_string()));
                                Ok(())
                            })),
                        )
                    })
                })
            };
            if let Err(error) = result {
                let _ = sender.send(Err(error.to_string()));
            }
        })
        .map_err(|error| format!("Could not access the Windows ubrowser WebView: {error}"))?;
    receiver
        .recv_timeout(Duration::from_secs(15))
        .map_err(|_| "uTools ubrowser screenshot capture timed out.".to_owned())??;
    let bytes = fs::read(&capture_path)
        .map_err(|error| format!("Could not read the ubrowser screenshot stream: {error}"))?;
    let _ = fs::remove_file(&capture_path);
    if bytes.len() > 32 * 1024 * 1024 {
        return Err("The uTools ubrowser screenshot exceeds 32 MiB.".to_owned());
    }
    Ok(bytes)
}

#[cfg(not(windows))]
fn capture_webview_png(_window: &WebviewWindow) -> Result<Vec<u8>, String> {
    Err("uTools ubrowser screenshot capture is currently available on Windows only.".to_owned())
}

fn print_pdf(window: &WebviewWindow, args: &[Value]) -> Result<String, String> {
    if args.is_empty() || args.len() > 2 {
        return Err("ubrowser.pdf requires options and an optional save path.".to_owned());
    }
    let options = args[0]
        .as_object()
        .ok_or_else(|| "ubrowser.pdf options must be an object.".to_owned())?;
    let allowed = [
        "landscape",
        "displayHeaderFooter",
        "printBackground",
        "scale",
        "pageSize",
        "margins",
        "pageRanges",
        "headerTemplate",
        "footerTemplate",
        "preferCSSPageSize",
        "generateTaggedPDF",
        "generateDocumentOutline",
    ];
    if options.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("ubrowser.pdf options contain an unsupported field.".to_owned());
    }
    let mut params = serde_json::Map::new();
    for key in [
        "landscape",
        "displayHeaderFooter",
        "printBackground",
        "preferCSSPageSize",
        "generateTaggedPDF",
        "generateDocumentOutline",
    ] {
        if let Some(value) = options.get(key) {
            let value = value
                .as_bool()
                .ok_or_else(|| format!("ubrowser.pdf {key} must be a boolean."))?;
            params.insert(key.to_owned(), Value::Bool(value));
        }
    }
    if let Some(value) = options.get("scale") {
        params.insert(
            "scale".to_owned(),
            Value::from(finite_number(value, 0.1, 2.0, "PDF scale")?),
        );
    }
    if let Some(page_size) = options.get("pageSize") {
        let (width, height) = pdf_page_size(page_size)?;
        params.insert("paperWidth".to_owned(), Value::from(width));
        params.insert("paperHeight".to_owned(), Value::from(height));
    }
    if let Some(margins) = options.get("margins") {
        let margins = margins
            .as_object()
            .filter(|margins| {
                margins
                    .keys()
                    .all(|key| matches!(key.as_str(), "top" | "bottom" | "left" | "right"))
            })
            .ok_or_else(|| "ubrowser.pdf margins are invalid.".to_owned())?;
        for (input, output) in [
            ("top", "marginTop"),
            ("bottom", "marginBottom"),
            ("left", "marginLeft"),
            ("right", "marginRight"),
        ] {
            if let Some(value) = margins.get(input) {
                params.insert(
                    output.to_owned(),
                    Value::from(finite_number(value, 0.0, 100.0, "PDF margin")?),
                );
            }
        }
    }
    if let Some(value) = options.get("pageRanges") {
        let value = value
            .as_str()
            .filter(|value| {
                value.len() <= 256
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || matches!(byte, b' ' | b',' | b'-'))
            })
            .ok_or_else(|| "ubrowser.pdf pageRanges is invalid.".to_owned())?;
        params.insert("pageRanges".to_owned(), Value::String(value.to_owned()));
    }
    for key in ["headerTemplate", "footerTemplate"] {
        if let Some(value) = options.get(key) {
            let value = value
                .as_str()
                .ok_or_else(|| format!("ubrowser.pdf {key} must be a string."))?;
            validate_bounded_string(value, key, 65_536)?;
            params.insert(key.to_owned(), Value::String(value.to_owned()));
        }
    }
    let response = call_devtools_method(window, "Page.printToPDF", Value::Object(params))?;
    let encoded = response
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| "The Windows WebView returned no PDF data.".to_owned())?;
    let pdf = BASE64_STANDARD
        .decode(encoded)
        .map_err(|_| "The Windows WebView returned malformed PDF data.".to_owned())?;
    if pdf.len() < 5 || !pdf.starts_with(b"%PDF-") || pdf.len() > 64 * 1024 * 1024 {
        return Err("The generated uTools ubrowser PDF is invalid or exceeds 64 MiB.".to_owned());
    }
    let path = match args.get(1) {
        None | Some(Value::Null) => window
            .app_handle()
            .path()
            .temp_dir()
            .map_err(|error| format!("Could not resolve the PDF temp directory: {error}"))?
            .join(format!("ihub-ubrowser-{}.pdf", Uuid::new_v4())),
        Some(Value::String(path)) => PathBuf::from(path),
        _ => return Err("ubrowser.pdf save path must be a string.".to_owned()),
    };
    validate_output_path(&path, "PDF")?;
    fs::write(&path, pdf)
        .map_err(|error| format!("Could not save the uTools ubrowser PDF: {error}"))?;
    Ok(path.to_string_lossy().into_owned())
}

fn pdf_page_size(value: &Value) -> Result<(f64, f64), String> {
    if let Some(name) = value.as_str() {
        return match name.to_ascii_uppercase().as_str() {
            "A0" => Ok((33.1, 46.8)),
            "A1" => Ok((23.4, 33.1)),
            "A2" => Ok((16.54, 23.4)),
            "A3" => Ok((11.7, 16.54)),
            "A4" => Ok((8.27, 11.7)),
            "A5" => Ok((5.83, 8.27)),
            "A6" => Ok((4.13, 5.83)),
            "LEGAL" => Ok((8.5, 14.0)),
            "LETTER" => Ok((8.5, 11.0)),
            "TABLOID" => Ok((11.0, 17.0)),
            "LEDGER" => Ok((17.0, 11.0)),
            _ => Err("ubrowser.pdf pageSize name is unsupported.".to_owned()),
        };
    }
    let object = value
        .as_object()
        .filter(|object| {
            object.len() == 2 && object.contains_key("width") && object.contains_key("height")
        })
        .ok_or_else(|| "ubrowser.pdf pageSize is invalid.".to_owned())?;
    Ok((
        finite_number(&object["width"], 0.1, 200.0, "PDF page width")?,
        finite_number(&object["height"], 0.1, 200.0, "PDF page height")?,
    ))
}

#[cfg(windows)]
fn call_devtools_method(
    window: &WebviewWindow,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    use webview2_com::CallDevToolsProtocolMethodCompletedHandler;
    use windows::core::HSTRING;

    let method = HSTRING::from(method);
    let params = HSTRING::from(
        serde_json::to_string(&params)
            .map_err(|error| format!("Could not encode WebView print settings: {error}"))?,
    );
    let (sender, receiver) = mpsc::sync_channel::<Result<String, String>>(1);
    let callback_sender = sender.clone();
    window
        .with_webview(move |platform| {
            let result = unsafe {
                platform.controller().CoreWebView2().and_then(|webview| {
                    webview.CallDevToolsProtocolMethod(
                        &method,
                        &params,
                        &CallDevToolsProtocolMethodCompletedHandler::create(Box::new(
                            move |result, response| {
                                let response =
                                    result.map(|()| response).map_err(|error| error.to_string());
                                let _ = callback_sender.send(response);
                                Ok(())
                            },
                        )),
                    )
                })
            };
            if let Err(error) = result {
                let _ = sender.send(Err(error.to_string()));
            }
        })
        .map_err(|error| format!("Could not access the Windows ubrowser WebView: {error}"))?;
    let response = receiver
        .recv_timeout(Duration::from_secs(30))
        .map_err(|_| "uTools ubrowser PDF generation timed out.".to_owned())??;
    serde_json::from_str(&response)
        .map_err(|error| format!("The Windows WebView returned malformed PDF output: {error}"))
}

#[cfg(not(windows))]
fn call_devtools_method(
    _window: &WebviewWindow,
    _method: &str,
    _params: Value,
) -> Result<Value, String> {
    Err("uTools ubrowser PDF generation is currently available on Windows only.".to_owned())
}

fn markdown(window: &WebviewWindow, args: &[Value]) -> Result<Value, String> {
    if args.len() > 1 {
        return Err("ubrowser.markdown accepts at most one selector.".to_owned());
    }
    let root = if let Some(selector) = args.first().and_then(Value::as_str) {
        validate_selector(selector)?;
        format!(
            "ihubFind({})",
            serde_json::to_string(selector).map_err(|error| error.to_string())?
        )
    } else {
        "document.body".to_owned()
    };
    let script = format!(
        r#"(() => {{
          {SELECTOR_HELPER}
          const root={root};
          const clean=(text)=>text.replace(/\s+/g,' ').trim();
          const walk=(node,depth=0)=>{{
            if(depth>64)return '';
            if(node.nodeType===Node.TEXT_NODE)return node.nodeValue||'';
            if(node.nodeType!==Node.ELEMENT_NODE)return '';
            const tag=node.tagName.toLowerCase();
            if(['script','style','noscript','svg'].includes(tag))return '';
            const body=()=>Array.from(node.childNodes).map((child)=>walk(child,depth+1)).join('');
            if(tag==='br')return '\n';
            if(/^h[1-6]$/.test(tag))return '\n'+('#'.repeat(Number(tag[1])))+' '+clean(body())+'\n\n';
            if(tag==='p'||tag==='section'||tag==='article')return '\n'+clean(body())+'\n\n';
            if(tag==='strong'||tag==='b')return '**'+clean(body())+'**';
            if(tag==='em'||tag==='i')return '*'+clean(body())+'*';
            if(tag==='code'&&node.parentElement?.tagName.toLowerCase()!=='pre')return '`'+body().replace(/`/g,'\\`')+'`';
            if(tag==='pre')return '\n```\n'+(node.textContent||'').trimEnd()+'\n```\n\n';
            if(tag==='blockquote')return '\n'+(node.textContent||'').trim().split('\n').map((line)=>'> '+line).join('\n')+'\n\n';
            if(tag==='a'){{const href=node.href||'';const label=clean(body())||href;return href?'['+label+']('+href+')':label;}}
            if(tag==='img'){{const src=node.src||'';const alt=node.alt||'';return src?'!['+alt+']('+src+')':'';}}
            if(tag==='li')return '\n- '+clean(body());
            if(tag==='ul'||tag==='ol')return '\n'+body().trim()+'\n\n';
            if(tag==='hr')return '\n---\n\n';
            return body();
          }};
          return walk(root).replace(/\n{{3,}}/g,'\n\n').trim();
        }})()"#
    );
    eval_json(window, &script, DEFAULT_EVAL_TIMEOUT)
}

fn read_cookies(window: &WebviewWindow, args: &[Value]) -> Result<Value, String> {
    if args.len() > 1 {
        return Err("ubrowser.cookies accepts at most one name or filter.".to_owned());
    }
    let filter = args.first().cloned().unwrap_or(Value::Null);
    if let Some(object) = filter.as_object() {
        if object.keys().any(|key| {
            !matches!(
                key.as_str(),
                "url" | "name" | "domain" | "path" | "secure" | "session" | "httpOnly"
            )
        }) {
            return Err("ubrowser.cookies filter contains an unsupported field.".to_owned());
        }
    } else if !filter.is_null() && !filter.is_string() {
        return Err("ubrowser.cookies filter must be a name or object.".to_owned());
    }
    let object = filter.as_object();
    let requested_url = object
        .and_then(|filter| filter.get("url"))
        .and_then(Value::as_str)
        .map(validate_navigation_url)
        .transpose()?;
    let cookies = if let Some(url) = requested_url {
        window.cookies_for_url(url)
    } else if object.is_some() {
        window.cookies()
    } else {
        let url = current_http_url(window)?;
        window.cookies_for_url(url)
    }
    .map_err(|error| format!("Could not read uTools ubrowser cookies: {error}"))?;
    let requested_name = filter.as_str().or_else(|| {
        object
            .and_then(|filter| filter.get("name"))
            .and_then(Value::as_str)
    });
    let requested_domain = object
        .and_then(|filter| filter.get("domain"))
        .and_then(Value::as_str)
        .map(|domain| domain.trim_start_matches('.').to_ascii_lowercase());
    let requested_path = object
        .and_then(|filter| filter.get("path"))
        .and_then(Value::as_str);
    let requested_secure = object
        .and_then(|filter| filter.get("secure"))
        .and_then(Value::as_bool);
    let requested_session = object
        .and_then(|filter| filter.get("session"))
        .and_then(Value::as_bool);
    let requested_http_only = object
        .and_then(|filter| filter.get("httpOnly"))
        .and_then(Value::as_bool);
    let projected = cookies
        .into_iter()
        .filter(|cookie| requested_name.map_or(true, |name| cookie.name() == name))
        .filter(|cookie| {
            requested_domain.as_ref().map_or(true, |domain| {
                let cookie_domain = cookie
                    .domain()
                    .unwrap_or_default()
                    .trim_start_matches('.')
                    .to_ascii_lowercase();
                cookie_domain == *domain || cookie_domain.ends_with(&format!(".{domain}"))
            })
        })
        .filter(|cookie| requested_path.map_or(true, |path| cookie.path() == Some(path)))
        .filter(|cookie| requested_secure.map_or(true, |secure| cookie.secure() == Some(secure)))
        .filter(|cookie| {
            requested_session.map_or(true, |session| cookie.expires_datetime().is_none() == session)
        })
        .filter(|cookie| {
            requested_http_only.map_or(true, |http_only| cookie.http_only() == Some(http_only))
        })
        .map(|cookie| {
            let expiration = cookie
                .expires_datetime()
                .map(|date| Value::from(date.unix_timestamp() as f64))
                .unwrap_or(Value::Null);
            json!({
                "name": cookie.name(),
                "value": cookie.value(),
                "domain": cookie.domain().unwrap_or_default(),
                "path": cookie.path().unwrap_or("/"),
                "secure": cookie.secure().unwrap_or(false),
                "httpOnly": cookie.http_only().unwrap_or(false),
                "session": cookie.expires_datetime().is_none(),
                "expirationDate": expiration,
                "sameSite": cookie.same_site().map(|value| format!("{value:?}").to_ascii_lowercase()),
            })
        })
        .collect::<Vec<_>>();
    Ok(Value::Array(projected))
}

fn set_cookies(window: &WebviewWindow, args: &[Value]) -> Result<(), String> {
    let cookies = normalized_cookie_pairs(args)?;
    let current_url = current_http_url(window)?;
    let domain = current_url
        .host_str()
        .ok_or_else(|| "The current ubrowser URL has no cookie domain.".to_owned())?;
    for (name, value) in cookies {
        let cookie = Cookie::build((name, value))
            .domain(domain.to_owned())
            .path("/")
            .build();
        window
            .set_cookie(cookie)
            .map_err(|error| format!("Could not set uTools ubrowser cookie: {error}"))?;
    }
    Ok(())
}

fn normalized_cookie_pairs(args: &[Value]) -> Result<Vec<(String, String)>, String> {
    let cookies = match args {
        [name, value] => vec![json!({ "name": name, "value": value })],
        [Value::Array(cookies)] if !cookies.is_empty() && cookies.len() <= 64 => cookies.clone(),
        _ => {
            return Err(
                "ubrowser.setCookies requires name/value or a bounded cookie array.".to_owned(),
            );
        }
    };
    let mut result = Vec::with_capacity(cookies.len());
    for cookie in cookies {
        let object = cookie
            .as_object()
            .filter(|object| {
                object
                    .keys()
                    .all(|key| matches!(key.as_str(), "name" | "value"))
            })
            .ok_or_else(|| "ubrowser.setCookies cookie is invalid.".to_owned())?;
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| "ubrowser.setCookies name must be a string.".to_owned())?;
        let value = object
            .get("value")
            .and_then(Value::as_str)
            .ok_or_else(|| "ubrowser.setCookies value must be a string.".to_owned())?;
        validate_cookie_name(name)?;
        validate_bounded_string(value, "cookie value", 4096)?;
        result.push((name.to_owned(), value.to_owned()));
    }
    Ok(result)
}

fn remove_cookies(window: &WebviewWindow, name: &str) -> Result<(), String> {
    let url = current_http_url(window)?;
    let cookies = window
        .cookies_for_url(url)
        .map_err(|error| format!("Could not read uTools ubrowser cookies: {error}"))?;
    for cookie in cookies.into_iter().filter(|cookie| cookie.name() == name) {
        window
            .delete_cookie(cookie)
            .map_err(|error| format!("Could not remove uTools ubrowser cookie: {error}"))?;
    }
    Ok(())
}

fn clear_cookies(window: &WebviewWindow, args: &[Value]) -> Result<(), String> {
    if args.len() > 1 {
        return Err("ubrowser.clearCookies accepts at most one URL.".to_owned());
    }
    let url = if let Some(url) = args.first() {
        validate_navigation_url(
            url.as_str()
                .ok_or_else(|| "ubrowser.clearCookies URL must be a string.".to_owned())?,
        )?
    } else {
        current_http_url(window)?
    };
    let cookies = window
        .cookies_for_url(url)
        .map_err(|error| format!("Could not read uTools ubrowser cookies: {error}"))?;
    for cookie in cookies {
        window
            .delete_cookie(cookie)
            .map_err(|error| format!("Could not clear uTools ubrowser cookie: {error}"))?;
    }
    Ok(())
}

fn current_http_url(window: &WebviewWindow) -> Result<Url, String> {
    let url = window
        .url()
        .map_err(|error| format!("Could not read ubrowser URL: {error}"))?;
    if matches!(url.scheme(), "http" | "https") {
        Ok(url)
    } else {
        Err("uTools ubrowser cookies require a current HTTP or HTTPS URL.".to_owned())
    }
}

fn validate_cookie_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 256
        || name.bytes().any(|byte| {
            byte <= 0x20
                || matches!(
                    byte,
                    b'(' | b')'
                        | b'<'
                        | b'>'
                        | b'@'
                        | b','
                        | b';'
                        | b':'
                        | b'\\'
                        | b'"'
                        | b'/'
                        | b'['
                        | b']'
                        | b'?'
                        | b'='
                        | b'{'
                        | b'}'
                        | 0x7f
                )
        })
    {
        return Err("uTools ubrowser cookie name is invalid.".to_owned());
    }
    Ok(())
}

fn apply_device(window: &WebviewWindow, args: &[Value]) -> Result<(), String> {
    if args.len() != 1 {
        return Err("ubrowser.device requires one device object.".to_owned());
    }
    let device = args[0]
        .as_object()
        .filter(|object| {
            object.len() == 2 && object.contains_key("userAgent") && object.contains_key("size")
        })
        .ok_or_else(|| "ubrowser.device options are invalid.".to_owned())?;
    let user_agent = device
        .get("userAgent")
        .and_then(Value::as_str)
        .ok_or_else(|| "ubrowser.device userAgent must be a string.".to_owned())?;
    validate_bounded_string(user_agent, "device user agent", 1024)?;
    let size = device
        .get("size")
        .and_then(Value::as_object)
        .filter(|size| size.len() == 2 && size.contains_key("width") && size.contains_key("height"))
        .ok_or_else(|| "ubrowser.device size is invalid.".to_owned())?;
    let width = finite_number(&size["width"], 64.0, 16_384.0, "device width")?;
    let height = finite_number(&size["height"], 64.0, 16_384.0, "device height")?;
    window
        .set_size(LogicalSize::new(width, height))
        .map_err(|error| format!("Could not resize ubrowser device viewport: {error}"))?;
    let _ = call_devtools_method(
        window,
        "Network.setUserAgentOverride",
        json!({ "userAgent": user_agent }),
    )?;
    let user_agent = serde_json::to_string(user_agent).map_err(|error| error.to_string())?;
    let script = format!(
        "(() => {{ try {{ Object.defineProperty(navigator,'userAgent',{{get:()=>{user_agent},configurable:true}}); }} catch {{}} return null; }})()"
    );
    let _ = eval_json(window, &script, DEFAULT_EVAL_TIMEOUT)?;
    Ok(())
}

fn evaluate_function(window: &WebviewWindow, args: &[Value]) -> Result<Value, String> {
    if args.is_empty() || args.len() > 2 {
        return Err("ubrowser.evaluate requires a function and optional params.".to_owned());
    }
    let source = function_source(&args[0])?;
    let params = match args.get(1) {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(params)) if params.len() <= 32 => params.clone(),
        _ => return Err("ubrowser.evaluate params must be a bounded array.".to_owned()),
    };
    let params = serde_json::to_string(&params).map_err(|error| error.to_string())?;
    let script = format!("(() => {{ const fn=({source}); return fn(...{params}); }})() ");
    eval_json(window, &script, DEFAULT_EVAL_TIMEOUT)
}

fn evaluate_condition(window: &WebviewWindow, args: &[Value]) -> Result<bool, String> {
    if args.is_empty() {
        return Err("ubrowser.when requires a selector or function.".to_owned());
    }
    let script = if let Some(selector) = args[0].as_str() {
        validate_selector(selector)?;
        let selector = serde_json::to_string(selector).map_err(|error| error.to_string())?;
        format!(
            "(() => {{ try {{{SELECTOR_HELPER} return Boolean(ihubFind({selector})); }} catch {{ return false; }} }})()"
        )
    } else {
        let source = function_source(&args[0])?;
        let params = serde_json::to_string(&args[1..]).map_err(|error| error.to_string())?;
        format!("(() => Boolean((({source}))(...{params})))()")
    };
    eval_json(window, &script, DEFAULT_EVAL_TIMEOUT)?
        .as_bool()
        .ok_or_else(|| "ubrowser.when did not return a boolean.".to_owned())
}

fn execute_wait(
    window: &WebviewWindow,
    args: &[Value],
    chain_started: Instant,
    still_active: impl Fn() -> bool,
) -> Result<(), String> {
    if args.is_empty() {
        return Err("ubrowser.wait requires a duration, selector, or function.".to_owned());
    }
    if let Some(milliseconds) = args[0].as_u64() {
        let duration = Duration::from_millis(milliseconds).min(Duration::from_secs(60));
        if chain_started.elapsed().saturating_add(duration) > MAX_CHAIN_DURATION {
            return Err("uTools ubrowser wait exceeds the chain deadline.".to_owned());
        }
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            if !still_active() {
                return Err("The plugin surface closed during the uTools ubrowser wait.".to_owned());
            }
            std::thread::sleep(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_millis(100)),
            );
        }
        return Ok(());
    }
    let timeout = args
        .get(1)
        .and_then(Value::as_u64)
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_NAVIGATION_TIMEOUT)
        .clamp(Duration::from_millis(100), Duration::from_secs(60));
    let deadline = Instant::now() + timeout;
    loop {
        if !still_active() {
            return Err("The plugin surface closed during the uTools ubrowser wait.".to_owned());
        }
        let ready = if let Some(selector) = args[0].as_str() {
            evaluate_condition(window, &[Value::String(selector.to_owned())])?
        } else {
            let mut condition_args = vec![args[0].clone()];
            if args.len() > 2 {
                condition_args.extend_from_slice(&args[2..]);
            }
            evaluate_condition(window, &condition_args)?
        };
        if ready {
            return Ok(());
        }
        if Instant::now() >= deadline || chain_started.elapsed() >= MAX_CHAIN_DURATION {
            return Err("uTools ubrowser wait timed out.".to_owned());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn snapshot_instance(window: &WebviewWindow, id: &str) -> Result<UBrowserInstance, String> {
    let url = window
        .url()
        .map_err(|error| format!("Could not read ubrowser URL: {error}"))?
        .to_string();
    let title = window.title().unwrap_or_default();
    let scale = window
        .scale_factor()
        .map_err(|error| format!("Could not read ubrowser scale factor: {error}"))?;
    let size = window
        .inner_size()
        .map_err(|error| format!("Could not read ubrowser size: {error}"))?
        .to_logical::<f64>(scale);
    let position = window
        .outer_position()
        .map_err(|error| format!("Could not read ubrowser position: {error}"))?
        .to_logical::<f64>(scale);
    Ok(UBrowserInstance {
        id: id.to_owned(),
        url,
        title,
        width: size.width.round() as i64,
        height: size.height.round() as i64,
        x: position.x.round() as i64,
        y: position.y.round() as i64,
    })
}

fn known_operation(operation: &str) -> bool {
    matches!(
        operation,
        "goto"
            | "useragent"
            | "viewport"
            | "hide"
            | "show"
            | "css"
            | "evaluate"
            | "press"
            | "click"
            | "mousedown"
            | "mouseup"
            | "dblclick"
            | "hover"
            | "file"
            | "drop"
            | "input"
            | "value"
            | "check"
            | "focus"
            | "scroll"
            | "download"
            | "paste"
            | "screenshot"
            | "markdown"
            | "pdf"
            | "device"
            | "wait"
            | "when"
            | "end"
            | "devTools"
            | "cookies"
            | "setCookies"
            | "removeCookies"
            | "clearCookies"
    )
}

fn allowed_navigation(url: &Url) -> bool {
    url.as_str() == "about:blank" || matches!(url.scheme(), "http" | "https")
}

fn validate_navigation_url(value: &str) -> Result<Url, String> {
    validate_bounded_string(value, "URL", 4096)?;
    let url = Url::parse(value).map_err(|_| "uTools ubrowser URL is invalid.".to_owned())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("uTools ubrowser accepts only credential-free HTTP(S) URLs.".to_owned());
    }
    Ok(url)
}

fn validate_headers(value: Option<&Value>) -> Result<(), String> {
    let Some(value) = value else { return Ok(()) };
    if value.is_null() {
        return Ok(());
    }
    let headers = value
        .as_object()
        .ok_or_else(|| "ubrowser.goto headers must be an object.".to_owned())?;
    if headers.len() > 32 {
        return Err("ubrowser.goto accepts at most 32 headers.".to_owned());
    }
    for (name, value) in headers {
        if name.is_empty()
            || name.len() > 128
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
            || !value.as_str().is_some_and(|value| {
                value.len() <= 4096 && !value.chars().any(|character| character.is_control())
            })
        {
            return Err("ubrowser.goto contains an invalid request header.".to_owned());
        }
    }
    Ok(())
}

fn validate_window_options(options: &UBrowserWindowOptions) -> Result<(), String> {
    let _accepted_electron_hints = (
        options.movable,
        options.fullscreenable,
        options.opacity,
        options.title_bar_style.as_deref(),
        options.thick_frame,
    );
    for (name, value, minimum) in [
        ("width", options.width, 64.0),
        ("height", options.height, 64.0),
        ("minWidth", options.min_width, 0.0),
        ("minHeight", options.min_height, 0.0),
        ("maxWidth", options.max_width, 64.0),
        ("maxHeight", options.max_height, 64.0),
    ] {
        if value.is_some_and(|value| !value.is_finite() || !(minimum..=16_384.0).contains(&value)) {
            return Err(format!(
                "ubrowser option {name} is outside the supported range."
            ));
        }
    }
    if options.x.is_some() != options.y.is_some() {
        return Err("ubrowser x and y must be supplied together.".to_owned());
    }
    for value in [options.x, options.y] {
        if value
            .is_some_and(|value| !value.is_finite() || !(-100_000.0..=100_000.0).contains(&value))
        {
            return Err("ubrowser position is outside the supported desktop range.".to_owned());
        }
    }
    if options.fullscreen == Some(true) && options.fullscreenable == Some(false) {
        return Err("ubrowser cannot start fullscreen when fullscreenable is false.".to_owned());
    }
    if let Some(opacity) = options.opacity {
        if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
            return Err("ubrowser opacity must be between 0 and 1.".to_owned());
        }
    }
    if let Some(style) = options.title_bar_style.as_deref() {
        if !matches!(
            style,
            "default" | "hidden" | "hiddenInset" | "customButtonsOnHover"
        ) {
            return Err("ubrowser titleBarStyle is invalid.".to_owned());
        }
    }
    if let Some(color) = options.background_color.as_deref() {
        parse_color(color)?;
    }
    Ok(())
}

fn parse_color(value: &str) -> Result<Color, String> {
    let hex = value
        .strip_prefix('#')
        .ok_or_else(|| "ubrowser backgroundColor must use #RRGGBB or #RRGGBBAA.".to_owned())?;
    if !matches!(hex.len(), 6 | 8) || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("ubrowser backgroundColor must use #RRGGBB or #RRGGBBAA.".to_owned());
    }
    let component = |start: usize| {
        u8::from_str_radix(&hex[start..start + 2], 16)
            .map_err(|_| "ubrowser backgroundColor is invalid.".to_owned())
    };
    Ok(Color(
        component(0)?,
        component(2)?,
        component(4)?,
        if hex.len() == 8 { component(6)? } else { 255 },
    ))
}

fn validate_instance_id(value: &str) -> Result<(), String> {
    let parsed =
        Uuid::parse_str(value).map_err(|_| "uTools ubrowser instance ID is invalid.".to_owned())?;
    if parsed.get_version_num() != 4 || parsed.to_string() != value {
        return Err("uTools ubrowser instance ID is invalid.".to_owned());
    }
    Ok(())
}

fn validate_selector(value: &str) -> Result<(), String> {
    validate_bounded_string(value, "selector", 4096)
}

fn validate_bounded_string(value: &str, label: &str, max_chars: usize) -> Result<(), String> {
    if value.is_empty()
        || value.chars().count() > max_chars
        || value.chars().any(|character| character == '\0')
    {
        return Err(format!("uTools ubrowser {label} is invalid or too large."));
    }
    Ok(())
}

fn function_source(value: &Value) -> Result<&str, String> {
    let object = value
        .as_object()
        .filter(|object| object.len() == 1)
        .ok_or_else(|| "uTools ubrowser requires a serialized page function.".to_owned())?;
    let source = object
        .get("__ihubFunction")
        .and_then(Value::as_str)
        .ok_or_else(|| "uTools ubrowser requires a serialized page function.".to_owned())?;
    validate_bounded_string(source, "page function", MAX_SCRIPT_CHARS)?;
    if source.len() > MAX_SCRIPT_BYTES {
        return Err("uTools ubrowser page function exceeds 256 KiB.".to_owned());
    }
    Ok(source)
}

fn required_string_arg<'a>(
    step: &'a UBrowserStep,
    index: usize,
    label: &str,
) -> Result<&'a str, String> {
    step.args
        .get(index)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("ubrowser.{label} requires a string."))
}

fn require_arg_count(step: &UBrowserStep, count: usize) -> Result<(), String> {
    if step.args.len() == count {
        Ok(())
    } else {
        Err(format!(
            "ubrowser.{} requires exactly {count} arguments.",
            step.op
        ))
    }
}

fn number_pair(step: &UBrowserStep, minimum: f64, maximum: f64) -> Result<(f64, f64), String> {
    require_arg_count(step, 2)?;
    Ok((
        finite_number(&step.args[0], minimum, maximum, "width/x")?,
        finite_number(&step.args[1], minimum, maximum, "height/y")?,
    ))
}

fn finite_number(value: &Value, minimum: f64, maximum: f64, label: &str) -> Result<f64, String> {
    value
        .as_f64()
        .filter(|value| value.is_finite() && (minimum..=maximum).contains(value))
        .ok_or_else(|| format!("uTools ubrowser {label} is outside the supported range."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(steps: Value) -> UBrowserRunRequest {
        serde_json::from_value(json!({ "steps": steps, "options": {} }))
            .expect("test request should deserialize")
    }

    #[test]
    fn chain_validation_bounds_steps_conditions_and_unknown_operations() {
        validate_run_request(&request(json!([
            { "op": "goto", "args": ["https://example.com", {}, 1000] },
            { "op": "wait", "args": ["#ready", 1000] },
            { "op": "when", "args": ["#ready"] },
            { "op": "click", "args": ["button"] },
            { "op": "end", "args": [] }
        ])))
        .expect("official core chain should validate");
        assert!(validate_run_request(&request(json!([{ "op": "unknown", "args": [] }]))).is_err());
        assert!(validate_run_request(&request(json!([{ "op": "end", "args": [] }]))).is_err());
        assert!(validate_run_request(&request(json!([{ "op": "when", "args": ["x"] }]))).is_err());
    }

    #[test]
    fn navigation_allows_only_bounded_credential_free_http_urls() {
        assert!(validate_navigation_url("https://example.com/path?q=1").is_ok());
        assert!(validate_navigation_url("http://127.0.0.1:8080/").is_ok());
        assert!(validate_navigation_url("file:///C:/secret.txt").is_err());
        assert!(validate_navigation_url("https://user:secret@example.com/").is_err());
        assert!(!allowed_navigation(
            &Url::parse("data:text/html,test").unwrap()
        ));
        assert!(allowed_navigation(&Url::parse("about:blank").unwrap()));
    }

    #[test]
    fn scripts_embed_selectors_and_values_as_json_not_source() {
        let script = dom_value_script("input[data-x='\"']", "</script>\n世界", false)
            .expect("bounded values should build a script");
        assert!(script.contains("ihubFind"));
        assert!(script.contains("\\\""));
        assert!(script.contains("世界"));
        assert!(!script.contains("value=</script>"));
    }

    #[test]
    fn window_options_match_documented_bounds_and_apple_color_tokens() {
        let options: UBrowserWindowOptions = serde_json::from_value(json!({
            "show": true,
            "width": 1200,
            "height": 800,
            "x": -100,
            "y": 50,
            "opacity": 0.9,
            "backgroundColor": "#0A84FFFF",
            "titleBarStyle": "hidden"
        }))
        .expect("official options should deserialize");
        validate_window_options(&options).expect("official options should validate");
        assert_eq!(parse_color("#0A84FFFF").unwrap(), Color(10, 132, 255, 255));
    }

    #[test]
    fn registry_scopes_idle_reuse_to_one_plugin_and_one_run() {
        let registry = UtoolsUBrowserRegistry::default();
        let first = registry
            .reserve_run("com.example.one", "lease-one", None)
            .expect("first run should reserve a window");
        assert!(first.create);
        registry.finish_run(&first.label, "com.example.one", "lease-one");
        let reused = registry
            .reserve_run("com.example.one", "lease-two", Some(&first.instance_id))
            .expect("same plugin should reuse an idle window");
        assert!(!reused.create);
        assert!(registry
            .reserve_run("com.example.one", "lease-three", Some(&first.instance_id))
            .is_err());
        registry.finish_run(&reused.label, "com.example.one", "lease-two");
        assert!(registry
            .reserve_run("com.example.two", "other-lease", Some(&first.instance_id))
            .is_err());
    }

    #[test]
    fn cookie_and_proxy_inputs_are_bounded_and_source_safe() {
        let cookies = normalized_cookie_pairs(&[
            Value::String("session".to_owned()),
            Value::String("value';document.body.remove()//".to_owned()),
        ])
        .expect("cookie values should stay typed host data");
        assert_eq!(cookies[0].0, "session");
        assert_eq!(cookies[0].1, "value';document.body.remove()//");

        let registry = UtoolsUBrowserRegistry::default();
        registry
            .set_proxy_config(
                "com.example.one",
                &json!({ "proxyRules": "http://127.0.0.1:1080" }),
            )
            .expect("documented HTTP proxy should validate");
        assert_eq!(
            registry.proxy_for("com.example.one").unwrap().as_str(),
            "http://127.0.0.1:1080/"
        );
        assert!(registry
            .set_proxy_config(
                "com.example.one",
                &json!({ "proxyRules": "file:///C:/secret" })
            )
            .is_err());
    }

    #[test]
    fn upload_and_image_payloads_are_bounded_typed_data() {
        let payload = json!({
            "__ihubBytesBase64": BASE64_STANDARD.encode(b"iHub upload")
        });
        let files = load_upload_payload(&payload).expect("bounded Buffer should decode");
        assert_eq!(
            files,
            vec![("upload.bin".to_owned(), b"iHub upload".to_vec())]
        );
        assert!(load_upload_payload(&json!({ "__ihubBytesBase64": "%%%" })).is_err());

        let image = image_paste_payload(&format!(
            "data:image/png;base64,{}",
            BASE64_STANDARD.encode(b"png fixture")
        ))
        .expect("image data URL should validate")
        .expect("image payload should be recognized");
        assert_eq!(image.mime, "image/png");
        assert_eq!(image.name, "pasted-image.png");
        assert!(image_paste_payload("data:image/svg+xml;base64,AAAA").is_err());
    }

    #[test]
    fn screenshot_pdf_and_download_parameters_match_documented_bounds() {
        validate_screenshot_rect(ScreenshotRect {
            x: 0.0,
            y: 0.0,
            width: 640.0,
            height: 480.0,
            viewport_width: 1280.0,
            viewport_height: 720.0,
        })
        .expect("ordinary screenshot bounds should validate");
        assert!(validate_screenshot_rect(ScreenshotRect {
            x: 0.0,
            y: 0.0,
            width: f64::NAN,
            height: 480.0,
            viewport_width: 1280.0,
            viewport_height: 720.0,
        })
        .is_err());
        assert_eq!(pdf_page_size(&json!("A4")).unwrap(), (8.27, 11.7));
        assert_eq!(
            pdf_page_size(&json!({ "width": 5.5, "height": 8.5 })).unwrap(),
            (5.5, 8.5)
        );
        assert!(pdf_page_size(&json!("unknown")).is_err());
        assert_eq!(
            safe_download_filename(&Url::parse("https://example.com/a%20b?.zip").unwrap()),
            "a_20b"
        );
    }

    #[test]
    fn navigation_headers_reject_control_characters_and_oversized_sets() {
        validate_headers(Some(&json!({
            "Authorization": "Bearer bounded-token",
            "X-iHub": "compatibility"
        })))
        .expect("bounded request headers should validate");
        assert!(validate_headers(Some(&json!({ "X-iHub": "ok\r\nInjected: true" }))).is_err());
        let too_many = (0..33)
            .map(|index| (format!("X-Header-{index}"), Value::String("x".to_owned())))
            .collect::<serde_json::Map<_, _>>();
        assert!(validate_headers(Some(&Value::Object(too_many))).is_err());
    }

    #[test]
    fn remote_ubrowser_labels_match_no_tauri_capability() {
        for capability in [
            include_str!("../capabilities/default.json"),
            include_str!("../capabilities/plugin-detached.json"),
            include_str!("../capabilities/plugin-browser.json"),
        ] {
            let value: Value = serde_json::from_str(capability).expect("capability JSON");
            let windows = value["windows"].as_array().expect("window patterns");
            assert!(windows.iter().all(|pattern| {
                let pattern = pattern.as_str().expect("string pattern");
                pattern != "*"
                    && !UTOOLS_UBROWSER_WINDOW_PREFIX.starts_with(pattern.trim_end_matches('*'))
            }));
        }
    }
}
