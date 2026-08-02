//! Host-owned auxiliary windows created through uTools compatibility.
//!
//! Every native label and route is derived from a host-generated UUID. The
//! plugin can choose bounded presentation options and a verified relative
//! bundle entry, but never a Tauri URL, window label, or another plugin's
//! parent/child identity.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{mpsc, Mutex},
};

use serde::{Deserialize, Serialize};
use tauri::{webview::Color, AppHandle, Theme, WebviewUrl, WebviewWindowBuilder};
use uuid::Uuid;

pub(crate) const UTOOLS_BROWSER_WINDOW_PREFIX: &str = "plugin-browser-";
pub(crate) const UTOOLS_BROWSER_ROUTE_PARAMETER: &str = "ihubUtoolsBrowserWindow";
const MAX_UTOOLS_BROWSER_WINDOWS: usize = 32;
const MAX_UTOOLS_BROWSER_WINDOWS_PER_PLUGIN: usize = 8;

#[derive(Debug, Clone)]
struct UtoolsBrowserWindowRecord {
    browser_id: String,
    plugin_id: String,
    parent_lease_id: String,
    parent_window_label: String,
    relative_url: String,
    preload: Option<String>,
    lease_id: Option<String>,
}

#[derive(Default)]
pub(crate) struct UtoolsBrowserWindowRegistry {
    windows: Mutex<HashMap<String, UtoolsBrowserWindowRecord>>,
    executions: Mutex<HashMap<String, PendingBrowserExecution>>,
}

struct PendingBrowserExecution {
    browser_id: String,
    response: mpsc::SyncSender<Result<serde_json::Value, String>>,
}

pub(crate) struct UtoolsBrowserExecution {
    pub(crate) request_id: String,
    pub(crate) window_label: String,
    pub(crate) response: mpsc::Receiver<Result<serde_json::Value, String>>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UtoolsBrowserWindowOpened {
    pub(crate) browser_id: String,
}

#[derive(Debug, Clone)]
pub(crate) struct UtoolsBrowserWindowBootstrapRecord {
    pub(crate) browser_id: String,
    pub(crate) plugin_id: String,
    pub(crate) parent_lease_id: String,
    pub(crate) relative_url: String,
    pub(crate) preload: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UtoolsBrowserWindowOptions {
    #[serde(default)]
    pub(crate) show: Option<bool>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    width: Option<f64>,
    #[serde(default)]
    height: Option<f64>,
    #[serde(default)]
    min_width: Option<f64>,
    #[serde(default)]
    min_height: Option<f64>,
    #[serde(default)]
    max_width: Option<f64>,
    #[serde(default)]
    max_height: Option<f64>,
    #[serde(default)]
    x: Option<f64>,
    #[serde(default)]
    y: Option<f64>,
    #[serde(default)]
    center: Option<bool>,
    #[serde(default)]
    resizable: Option<bool>,
    #[serde(default)]
    maximizable: Option<bool>,
    #[serde(default)]
    minimizable: Option<bool>,
    #[serde(default, alias = "closeable")]
    closable: Option<bool>,
    #[serde(default)]
    always_on_top: Option<bool>,
    #[serde(default)]
    skip_taskbar: Option<bool>,
    #[serde(default)]
    fullscreen: Option<bool>,
    #[serde(default)]
    fullscreenable: Option<bool>,
    #[serde(default)]
    maximized: Option<bool>,
    #[serde(default)]
    focused: Option<bool>,
    #[serde(default)]
    frame: Option<bool>,
    #[serde(default)]
    transparent: Option<bool>,
    #[serde(default)]
    focusable: Option<bool>,
    #[serde(default)]
    visible_on_all_workspaces: Option<bool>,
    #[serde(default)]
    has_shadow: Option<bool>,
    #[serde(default)]
    background_color: Option<String>,
    /// Accepted Electron presentation toggles whose closest Tauri/WebView2
    /// behavior is already represented by frame/overflow/menu-free hosting.
    #[serde(default)]
    thick_frame: Option<bool>,
    #[serde(default)]
    movable: Option<bool>,
    #[serde(default)]
    auto_hide_menu_bar: Option<bool>,
    #[serde(default)]
    enable_larger_than_screen: Option<bool>,
    #[serde(default)]
    rounded_corners: Option<bool>,
    #[serde(default)]
    web_preferences: Option<UtoolsBrowserWebPreferences>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UtoolsBrowserWebPreferences {
    #[serde(default)]
    pub(crate) preload: Option<String>,
    #[serde(default)]
    node_integration: Option<bool>,
    #[serde(default)]
    context_isolation: Option<bool>,
    #[serde(default)]
    sandbox: Option<bool>,
}

impl UtoolsBrowserWindowOptions {
    pub(crate) fn preload(&self) -> Option<&str> {
        self.web_preferences
            .as_ref()
            .and_then(|preferences| preferences.preload.as_deref())
    }
}

impl UtoolsBrowserWindowRegistry {
    fn reserve(
        &self,
        plugin_id: &str,
        parent_lease_id: &str,
        parent_window_label: &str,
        relative_url: &str,
        preload: Option<String>,
    ) -> Result<(String, String), String> {
        let mut windows = self
            .windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if windows.len() >= MAX_UTOOLS_BROWSER_WINDOWS {
            return Err(format!(
                "Too many uTools BrowserWindows are active (limit: {MAX_UTOOLS_BROWSER_WINDOWS})."
            ));
        }
        let plugin_count = windows
            .values()
            .filter(|record| record.plugin_id == plugin_id)
            .count();
        if plugin_count >= MAX_UTOOLS_BROWSER_WINDOWS_PER_PLUGIN {
            return Err(format!(
                "Plugin '{plugin_id}' already owns the maximum of {MAX_UTOOLS_BROWSER_WINDOWS_PER_PLUGIN} BrowserWindows."
            ));
        }
        let browser_id = Uuid::new_v4().to_string();
        let label = format!("{UTOOLS_BROWSER_WINDOW_PREFIX}{browser_id}");
        windows.insert(
            label.clone(),
            UtoolsBrowserWindowRecord {
                browser_id: browser_id.clone(),
                plugin_id: plugin_id.to_owned(),
                parent_lease_id: parent_lease_id.to_owned(),
                parent_window_label: parent_window_label.to_owned(),
                relative_url: relative_url.to_owned(),
                preload,
                lease_id: None,
            },
        );
        Ok((browser_id, label))
    }

    pub(crate) fn cancel_reservation(&self, label: &str, browser_id: &str) {
        let mut windows = self
            .windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if windows
            .get(label)
            .is_some_and(|record| record.browser_id == browser_id && record.lease_id.is_none())
        {
            windows.remove(label);
        }
    }

    pub(crate) fn bootstrap_for_window(
        &self,
        label: &str,
    ) -> Result<UtoolsBrowserWindowBootstrapRecord, String> {
        let windows = self
            .windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let record = windows
            .get(label)
            .ok_or_else(|| "This uTools BrowserWindow is no longer registered.".to_owned())?;
        Ok(UtoolsBrowserWindowBootstrapRecord {
            browser_id: record.browser_id.clone(),
            plugin_id: record.plugin_id.clone(),
            parent_lease_id: record.parent_lease_id.clone(),
            relative_url: record.relative_url.clone(),
            preload: record.preload.clone(),
        })
    }

    pub(crate) fn bind_lease(
        &self,
        label: &str,
        browser_id: &str,
        lease_id: &str,
    ) -> Result<Option<String>, String> {
        if lease_id.is_empty() || lease_id.len() > 128 || lease_id.chars().any(char::is_control) {
            return Err("uTools BrowserWindow returned an invalid frontend lease ID.".to_owned());
        }
        let mut windows = self
            .windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let record = windows
            .get_mut(label)
            .ok_or_else(|| "This uTools BrowserWindow has already closed.".to_owned())?;
        if record.browser_id != browser_id {
            return Err("uTools BrowserWindow identity mismatch.".to_owned());
        }
        Ok(record.lease_id.replace(lease_id.to_owned()))
    }

    pub(crate) fn owns_lease(&self, label: &str, lease_id: &str) -> bool {
        self.windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(label)
            .and_then(|record| record.lease_id.as_deref())
            == Some(lease_id)
    }

    pub(crate) fn unbind_owned_lease(&self, label: &str, lease_id: &str) -> bool {
        let mut windows = self
            .windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(record) = windows.get_mut(label) else {
            return false;
        };
        if record.lease_id.as_deref() != Some(lease_id) {
            return false;
        }
        record.lease_id = None;
        true
    }

    pub(crate) fn take_window_lease(&self, label: &str) -> Option<String> {
        let record = self
            .windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(label);
        if let Some(record) = record.as_ref() {
            self.cancel_executions_for_browser(
                &record.browser_id,
                "The uTools BrowserWindow closed before script execution completed.",
            );
        }
        record.and_then(|record| record.lease_id)
    }

    pub(crate) fn validate_parent(
        &self,
        browser_id: &str,
        plugin_id: &str,
        parent_lease_id: &str,
    ) -> Result<(String, String), String> {
        self.windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .find(|(_, record)| record.browser_id == browser_id)
            .and_then(|(label, record)| {
                (record.plugin_id == plugin_id && record.parent_lease_id == parent_lease_id)
                    .then(|| (label.clone(), record.parent_window_label.clone()))
            })
            .ok_or_else(|| {
                "This uTools BrowserWindow does not belong to the active parent session.".to_owned()
            })
    }

    pub(crate) fn parent_for_child(
        &self,
        child_label: &str,
    ) -> Result<(String, String, String), String> {
        let windows = self
            .windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let record = windows
            .get(child_label)
            .ok_or_else(|| "This uTools BrowserWindow is no longer registered.".to_owned())?;
        Ok((
            record.browser_id.clone(),
            record.plugin_id.clone(),
            record.parent_window_label.clone(),
        ))
    }

    pub(crate) fn parent_session_for_child(&self, child_label: &str) -> Option<(String, String)> {
        self.windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(child_label)
            .map(|record| (record.plugin_id.clone(), record.parent_lease_id.clone()))
    }

    pub(crate) fn labels_owned_by_parent_lease(&self, lease_id: &str) -> Vec<String> {
        self.windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|(_, record)| record.parent_lease_id == lease_id)
            .map(|(label, _)| label.clone())
            .collect()
    }

    pub(crate) fn labels_owned_by_plugin(&self, plugin_id: &str) -> Vec<String> {
        self.windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|(_, record)| record.plugin_id == plugin_id)
            .map(|(label, _)| label.clone())
            .collect()
    }

    pub(crate) fn begin_execution(
        &self,
        browser_id: &str,
        plugin_id: &str,
        parent_lease_id: &str,
    ) -> Result<UtoolsBrowserExecution, String> {
        let (window_label, _) = self.validate_parent(browser_id, plugin_id, parent_lease_id)?;
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut executions = self
            .executions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if executions.len() >= 32
            || executions
                .values()
                .filter(|pending| pending.browser_id == browser_id)
                .count()
                >= 4
        {
            return Err("Too many uTools BrowserWindow scripts are already pending.".to_owned());
        }
        let request_id = Uuid::new_v4().to_string();
        executions.insert(
            request_id.clone(),
            PendingBrowserExecution {
                browser_id: browser_id.to_owned(),
                response: sender,
            },
        );
        Ok(UtoolsBrowserExecution {
            request_id,
            window_label,
            response: receiver,
        })
    }

    pub(crate) fn complete_execution(
        &self,
        child_label: &str,
        request_id: &str,
        response: Result<serde_json::Value, String>,
    ) -> Result<(), String> {
        let (browser_id, _, _) = self.parent_for_child(child_label)?;
        let mut executions = self
            .executions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let pending = executions
            .get(request_id)
            .ok_or_else(|| "This uTools BrowserWindow script request has expired.".to_owned())?;
        if pending.browser_id != browser_id {
            return Err("This uTools BrowserWindow script belongs to another child.".to_owned());
        }
        let pending = executions
            .remove(request_id)
            .expect("the validated BrowserWindow script request must still exist");
        drop(executions);
        let _ = pending.response.try_send(response);
        Ok(())
    }

    pub(crate) fn cancel_execution(&self, request_id: &str) {
        self.executions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(request_id);
    }

    fn cancel_executions_for_browser(&self, browser_id: &str, reason: &str) {
        let removed = {
            let mut executions = self
                .executions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let ids = executions
                .iter()
                .filter(|(_, pending)| pending.browser_id == browser_id)
                .map(|(id, _)| id.clone())
                .collect::<Vec<_>>();
            ids.into_iter()
                .filter_map(|id| executions.remove(&id))
                .collect::<Vec<_>>()
        };
        for pending in removed {
            let _ = pending.response.try_send(Err(reason.to_owned()));
        }
    }
}

pub(crate) fn create_utools_browser_window(
    app: &AppHandle,
    registry: &UtoolsBrowserWindowRegistry,
    plugin_id: &str,
    parent_lease_id: &str,
    parent_window_label: &str,
    relative_url: &str,
    options: UtoolsBrowserWindowOptions,
) -> Result<UtoolsBrowserWindowOpened, String> {
    validate_options(&options)?;
    let preload = options
        .web_preferences
        .as_ref()
        .and_then(|preferences| preferences.preload.clone());
    let (browser_id, label) = registry.reserve(
        plugin_id,
        parent_lease_id,
        parent_window_label,
        relative_url,
        preload,
    )?;
    let route = PathBuf::from(format!(
        "index.html?{UTOOLS_BROWSER_ROUTE_PARAMETER}={browser_id}"
    ));
    let mut builder = WebviewWindowBuilder::new(app, label.clone(), WebviewUrl::App(route))
        .title(options.title.as_deref().unwrap_or("iHub BrowserWindow"))
        .inner_size(
            options.width.unwrap_or(800.0),
            options.height.unwrap_or(600.0),
        )
        .resizable(options.resizable.unwrap_or(true))
        .maximizable(options.maximizable.unwrap_or(true))
        .minimizable(options.minimizable.unwrap_or(true))
        .closable(options.closable.unwrap_or(true))
        .always_on_top(options.always_on_top.unwrap_or(false))
        .skip_taskbar(options.skip_taskbar.unwrap_or(false))
        .decorations(options.frame.unwrap_or(true))
        .transparent(options.transparent.unwrap_or(false))
        .focusable(options.focusable.unwrap_or(true))
        .visible(options.show.unwrap_or(true))
        .focused(options.focused.unwrap_or(options.show.unwrap_or(true)))
        .maximized(options.maximized.unwrap_or(false))
        .visible_on_all_workspaces(options.visible_on_all_workspaces.unwrap_or(false))
        .shadow(options.has_shadow.unwrap_or(true))
        .theme(Some(Theme::Light))
        .fullscreen(options.fullscreen.unwrap_or(false));
    if !options.enable_larger_than_screen.unwrap_or(false) {
        builder = builder.prevent_overflow();
    }
    if let Some(color) = options.background_color.as_deref() {
        builder = builder.background_color(parse_background_color(color)?);
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
    let _window = match builder.build() {
        Ok(window) => window,
        Err(error) => {
            registry.cancel_reservation(&label, &browser_id);
            return Err(format!(
                "Could not create the uTools BrowserWindow: {error}"
            ));
        }
    };
    Ok(UtoolsBrowserWindowOpened { browser_id })
}

fn validate_options(options: &UtoolsBrowserWindowOptions) -> Result<(), String> {
    let _electron_presentation_hints = (
        options.thick_frame,
        options.movable,
        options.auto_hide_menu_bar,
        options.rounded_corners,
    );
    for (name, value) in [
        ("width", options.width),
        ("height", options.height),
        ("minWidth", options.min_width),
        ("minHeight", options.min_height),
        ("maxWidth", options.max_width),
        ("maxHeight", options.max_height),
    ] {
        if value.is_some_and(|value| !value.is_finite() || !(64.0..=16_384.0).contains(&value)) {
            return Err(format!(
                "uTools BrowserWindow {name} must be between 64 and 16384."
            ));
        }
    }
    for (name, value) in [("x", options.x), ("y", options.y)] {
        if value
            .is_some_and(|value| !value.is_finite() || !(-100_000.0..=100_000.0).contains(&value))
        {
            return Err(format!(
                "uTools BrowserWindow {name} is outside the supported desktop range."
            ));
        }
    }
    if options.x.is_some() != options.y.is_some() {
        return Err("uTools BrowserWindow x and y must be supplied together.".to_owned());
    }
    if let Some(title) = options.title.as_deref() {
        if title.chars().count() > 160 || title.chars().any(char::is_control) {
            return Err("uTools BrowserWindow title is invalid.".to_owned());
        }
    }
    if options.fullscreen == Some(true) && options.fullscreenable == Some(false) {
        return Err(
            "uTools BrowserWindow cannot start fullscreen when fullscreenable is false.".to_owned(),
        );
    }
    if let Some(color) = options.background_color.as_deref() {
        parse_background_color(color)?;
    }
    if let Some(preferences) = options.web_preferences.as_ref() {
        if preferences.node_integration == Some(true)
            || preferences.context_isolation == Some(false)
            || preferences.sandbox == Some(false)
        {
            return Err("iHub BrowserWindows keep Node disabled, context isolation enabled, and the page sandboxed.".to_owned());
        }
        if let Some(preload) = preferences.preload.as_deref() {
            if preload.is_empty()
                || preload.chars().count() > 1024
                || preload.chars().any(char::is_control)
                || preload.starts_with('/')
                || preload.starts_with('\\')
                || preload.contains('\\')
                || preload.contains('%')
                || preload.contains(':')
                || std::path::Path::new(preload)
                    .components()
                    .any(|component| !matches!(component, std::path::Component::Normal(_)))
            {
                return Err(
                    "uTools BrowserWindow preload must be a plain relative package path."
                        .to_owned(),
                );
            }
        }
    }
    Ok(())
}

fn parse_background_color(value: &str) -> Result<Color, String> {
    let hex = value.strip_prefix('#').ok_or_else(|| {
        "uTools BrowserWindow backgroundColor must use #RRGGBB or #RRGGBBAA.".to_owned()
    })?;
    if !matches!(hex.len(), 6 | 8) || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(
            "uTools BrowserWindow backgroundColor must use #RRGGBB or #RRGGBBAA.".to_owned(),
        );
    }
    let component = |start: usize| {
        u8::from_str_radix(&hex[start..start + 2], 16)
            .map_err(|_| "uTools BrowserWindow backgroundColor is invalid.".to_owned())
    };
    Ok(Color(
        component(0)?,
        component(2)?,
        component(4)?,
        if hex.len() == 8 { component(6)? } else { 255 },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_binds_children_to_exact_parent_sessions() {
        let registry = UtoolsBrowserWindowRegistry::default();
        let (browser_id, label) = registry
            .reserve(
                "com.example.test",
                "parent-lease",
                "main",
                "child.html",
                None,
            )
            .expect("reservation should succeed");
        assert_eq!(
            registry
                .validate_parent(&browser_id, "com.example.test", "parent-lease")
                .expect("exact owner should validate"),
            (label.clone(), "main".to_owned())
        );
        assert!(registry
            .validate_parent(&browser_id, "com.example.test", "other")
            .is_err());
        assert_eq!(
            registry
                .bootstrap_for_window(&label)
                .expect("reserved child should expose its host-owned bootstrap")
                .parent_lease_id,
            "parent-lease"
        );
        let previous = registry
            .bind_lease(&label, &browser_id, "child-lease")
            .expect("lease should bind");
        assert!(previous.is_none());
        assert!(registry.owns_lease(&label, "child-lease"));
        assert_eq!(
            registry.take_window_lease(&label).as_deref(),
            Some("child-lease")
        );
    }

    #[test]
    fn unsafe_browser_preferences_fail_closed() {
        let options = serde_json::from_value::<UtoolsBrowserWindowOptions>(serde_json::json!({
            "webPreferences": { "nodeIntegration": true }
        }))
        .expect("shape should deserialize");
        assert!(validate_options(&options).is_err());
        assert!(
            serde_json::from_value::<UtoolsBrowserWindowOptions>(serde_json::json!({
                "unknownElectronOption": true
            }))
            .is_err()
        );
        let documented_overlay =
            serde_json::from_value::<UtoolsBrowserWindowOptions>(serde_json::json!({
                "show": true,
                "x": -1920,
                "y": 0,
                "width": 1920,
                "height": 1080,
                "backgroundColor": "#00000000",
                "thickFrame": false,
                "resizable": false,
                "fullscreenable": true,
                "fullscreen": true,
                "minimizable": false,
                "maximizable": false,
                "movable": false,
                "autoHideMenuBar": true,
                "frame": false,
                "transparent": true,
                "skipTaskbar": true,
                "enableLargerThanScreen": true,
                "alwaysOnTop": true,
                "roundedCorners": false,
                "hasShadow": false,
                "webPreferences": { "preload": "foo_preload.js" }
            }))
            .expect("documented screen-overlay options should deserialize");
        validate_options(&documented_overlay)
            .expect("documented screen-overlay options should validate");
        assert_eq!(
            parse_background_color("#0A84FFFF").expect("Apple blue RGBA"),
            Color(10, 132, 255, 255)
        );
    }

    #[test]
    fn browserwindow_capability_is_local_event_only_and_narrow() {
        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/plugin-browser.json"))
                .expect("BrowserWindow capability JSON");
        assert_eq!(capability["local"], true);
        assert_eq!(
            capability["windows"],
            serde_json::json!(["plugin-browser-*"])
        );
        assert_eq!(
            capability["permissions"],
            serde_json::json!([
                "utools-browser-window-host-commands",
                "core:event:allow-listen",
                "core:event:allow-unlisten"
            ])
        );
        assert!(capability.get("remote").is_none());
    }
}
