//! Host-owned windows for a plugin's visible frontend.
//!
//! A detached window is another iHub React host, never a plugin document.
//! Native code derives both its label and its local app route from a validated
//! plugin ID. The remote plugin frontend remains inside the same per-document
//! loopback iframe and parent-frame Bridge used by the launcher surface.

use std::{collections::HashMap, path::PathBuf, sync::Mutex};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager, State, Theme, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::{
    app::{is_plugin_id, AppState},
    models::PluginInfo,
};

pub(crate) const DETACHED_PLUGIN_WINDOW_PREFIX: &str = "plugin-detached-";
const DETACHED_PLUGIN_ROUTE_PARAMETER: &str = "ihubDetachedPlugin";
const DETACHED_PLUGIN_WIDTH: f64 = 800.0;
const DETACHED_PLUGIN_HEIGHT: f64 = 600.0;
const DETACHED_PLUGIN_MIN_WIDTH: f64 = 480.0;
const DETACHED_PLUGIN_MIN_HEIGHT: f64 = 320.0;

#[derive(Debug, Clone, PartialEq, Eq)]
struct DetachedPluginWindowRecord {
    plugin_id: String,
    lease_id: Option<String>,
}

/// Tracks only host-created detached windows and their current iframe lease.
///
/// The registry is deliberately process-local. Window placement is not
/// persisted, and a renderer cannot nominate its own label, route, or lease.
#[derive(Default)]
pub(crate) struct DetachedPluginWindowRegistry {
    windows: Mutex<HashMap<String, DetachedPluginWindowRecord>>,
}

impl DetachedPluginWindowRegistry {
    /// Reserves the deterministic label before native window creation.
    /// Returns `false` when an existing window for the same plugin should be
    /// focused instead of creating another copy.
    pub(crate) fn reserve(&self, plugin_id: &str) -> Result<(String, bool), String> {
        let label = detached_plugin_window_label(plugin_id)?;
        let mut windows = self
            .windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(existing) = windows.get(&label) {
            if existing.plugin_id != plugin_id {
                return Err(
                    "Detached plugin window identity collision; no window was opened.".to_owned(),
                );
            }
            return Ok((label, false));
        }
        windows.insert(
            label.clone(),
            DetachedPluginWindowRecord {
                plugin_id: plugin_id.to_owned(),
                lease_id: None,
            },
        );
        Ok((label, true))
    }

    /// Removes a reservation only when it still belongs to the failed create
    /// attempt and has not acquired a frontend lease.
    pub(crate) fn cancel_reservation(&self, label: &str, plugin_id: &str) {
        let mut windows = self
            .windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if windows
            .get(label)
            .is_some_and(|record| record.plugin_id == plugin_id && record.lease_id.is_none())
        {
            windows.remove(label);
        }
    }

    pub(crate) fn plugin_is_detached(&self, plugin_id: &str) -> bool {
        self.window_label_for_plugin(plugin_id).is_some()
    }

    /// Returns only the deterministic label of the registry record that owns
    /// this exact plugin ID. Callers cannot supply or derive an arbitrary
    /// event target, and a hash collision cannot route across plugin records.
    pub(crate) fn window_label_for_plugin(&self, plugin_id: &str) -> Option<String> {
        let label = detached_plugin_window_label(plugin_id).ok()?;
        self.windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&label)
            .filter(|record| record.plugin_id == plugin_id)
            .map(|_| label)
    }

    /// Resolves a dispatch target only after the exact detached host has
    /// acquired its visible frontend lease. The opaque lease is returned for
    /// an independent asset-server ownership check by the native caller.
    pub(crate) fn window_label_and_lease_for_plugin(
        &self,
        plugin_id: &str,
    ) -> Option<(String, String)> {
        let label = detached_plugin_window_label(plugin_id).ok()?;
        self.windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&label)
            .filter(|record| record.plugin_id == plugin_id)
            .and_then(|record| {
                record
                    .lease_id
                    .as_ref()
                    .map(|lease_id| (label, lease_id.clone()))
            })
    }

    pub(crate) fn validate_window_plugin(
        &self,
        label: &str,
        plugin_id: &str,
    ) -> Result<(), String> {
        let expected_label = detached_plugin_window_label(plugin_id)?;
        if label != expected_label {
            return Err("Detached plugin window identity does not match its plugin.".to_owned());
        }
        let windows = self
            .windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if windows
            .get(label)
            .is_some_and(|record| record.plugin_id == plugin_id)
        {
            Ok(())
        } else {
            Err("This detached plugin window is no longer registered.".to_owned())
        }
    }

    /// Binds the lease returned by the existing loopback asset server. A
    /// retry replaces only this window's previous opaque lease identifier.
    pub(crate) fn bind_lease(
        &self,
        label: &str,
        plugin_id: &str,
        lease_id: &str,
    ) -> Result<(), String> {
        if lease_id.is_empty() || lease_id.len() > 128 || lease_id.chars().any(char::is_control) {
            return Err("Detached plugin frontend returned an invalid lease ID.".to_owned());
        }
        let expected_label = detached_plugin_window_label(plugin_id)?;
        if label != expected_label {
            return Err("Detached plugin window identity does not match its plugin.".to_owned());
        }
        let mut windows = self
            .windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let record = windows
            .get_mut(label)
            .ok_or_else(|| "This detached plugin window has already closed.".to_owned())?;
        if record.plugin_id != plugin_id {
            return Err("Detached plugin window identity does not match its plugin.".to_owned());
        }
        record.lease_id = Some(lease_id.to_owned());
        Ok(())
    }

    pub(crate) fn owns_lease(&self, label: &str, lease_id: &str) -> bool {
        self.windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(label)
            .and_then(|record| record.lease_id.as_deref())
            == Some(lease_id)
    }

    /// Removes the lease association without closing the host window. This is
    /// used by normal React cleanup before a source retry.
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

    /// Removes the entire host-window record. Returning the exact lease lets
    /// the app release the loopback listener on both CloseRequested and
    /// Destroyed without trusting renderer cleanup.
    pub(crate) fn take_window_lease(&self, label: &str) -> Option<String> {
        self.windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(label)
            .and_then(|record| record.lease_id)
    }

    pub(crate) fn plugin_for_window(&self, label: &str) -> Option<String> {
        self.windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(label)
            .map(|record| record.plugin_id.clone())
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DetachedPluginWindowOpened {
    plugin_id: String,
    window_label: String,
    created: bool,
}

/// The only native detached-window creation entrypoint. The renderer supplies
/// a plugin ID, never a window label or URL.
#[tauri::command]
pub(crate) async fn open_detached_plugin_window(
    plugin_id: String,
    app: AppHandle,
    window: WebviewWindow,
    state: State<'_, AppState>,
    detached: State<'_, DetachedPluginWindowRegistry>,
) -> Result<DetachedPluginWindowOpened, String> {
    if window.label() != "main" {
        return Err("Only the trusted main iHub surface can detach a plugin.".to_owned());
    }
    if !is_plugin_id(&plugin_id) {
        return Err("Invalid plugin ID.".to_owned());
    }

    // Resolve the canonical, enabled frontend before reserving any window.
    // This repeats the same manifest/path/integrity boundary used when a
    // PluginFrontendFrame later asks for its loopback lease.
    state.plugins.frontend_asset_bundle(&plugin_id)?;
    let plugin = state
        .plugins
        .list()
        .into_iter()
        .find(|plugin| plugin.id == plugin_id && plugin.enabled)
        .ok_or_else(|| format!("Plugin '{plugin_id}' is not available."))?;

    let (window_label, created) = detached.reserve(&plugin_id)?;
    if !created {
        if let Some(existing) = app.get_webview_window(&window_label) {
            let _ = existing.unminimize();
            let _ = existing.show();
            let _ = existing.set_focus();
        }
        return Ok(DetachedPluginWindowOpened {
            plugin_id,
            window_label,
            created: false,
        });
    }

    let route = detached_plugin_app_route(&plugin_id)?;
    let title = format!("{} · iHub", plugin.name);
    let build_result =
        WebviewWindowBuilder::new(&app, window_label.clone(), WebviewUrl::App(route))
            .title(title)
            .inner_size(DETACHED_PLUGIN_WIDTH, DETACHED_PLUGIN_HEIGHT)
            .min_inner_size(DETACHED_PLUGIN_MIN_WIDTH, DETACHED_PLUGIN_MIN_HEIGHT)
            .resizable(true)
            .maximizable(true)
            .minimizable(true)
            .closable(true)
            .decorations(true)
            .always_on_top(false)
            .skip_taskbar(false)
            .theme(Some(Theme::Light))
            .center()
            .prevent_overflow()
            .build();

    if let Err(error) = build_result {
        detached.cancel_reservation(&window_label, &plugin_id);
        return Err(format!(
            "Could not create the detached plugin window: {error}"
        ));
    }

    Ok(DetachedPluginWindowOpened {
        plugin_id,
        window_label,
        created: true,
    })
}

/// Returns only the plugin bound to the calling host window. The route's query
/// string is display/bootstrap metadata and cannot select a different plugin.
#[tauri::command]
pub(crate) fn get_detached_plugin_window_bootstrap(
    window: WebviewWindow,
    state: State<'_, AppState>,
    detached: State<'_, DetachedPluginWindowRegistry>,
) -> Result<PluginInfo, String> {
    let plugin_id = detached
        .plugin_for_window(window.label())
        .ok_or_else(|| "This detached plugin window is no longer registered.".to_owned())?;
    detached.validate_window_plugin(window.label(), &plugin_id)?;
    state.plugins.frontend_asset_bundle(&plugin_id)?;
    state
        .plugins
        .list()
        .into_iter()
        .find(|plugin| plugin.id == plugin_id && plugin.enabled)
        .ok_or_else(|| format!("Plugin '{plugin_id}' is not available."))
}

/// Closes only the calling registered detached window. No label or target can
/// be supplied over IPC.
#[tauri::command]
pub(crate) fn close_detached_plugin_window(
    window: WebviewWindow,
    detached: State<'_, DetachedPluginWindowRegistry>,
) -> Result<(), String> {
    let plugin_id = detached
        .plugin_for_window(window.label())
        .ok_or_else(|| "This detached plugin window is no longer registered.".to_owned())?;
    detached.validate_window_plugin(window.label(), &plugin_id)?;
    window
        .close()
        .map_err(|error| format!("Could not close the detached plugin window: {error}"))
}

fn detached_plugin_window_label(plugin_id: &str) -> Result<String, String> {
    if !is_plugin_id(plugin_id) {
        return Err("Invalid plugin ID.".to_owned());
    }
    let digest = Sha256::digest(plugin_id.as_bytes());
    let mut label = String::with_capacity(DETACHED_PLUGIN_WINDOW_PREFIX.len() + digest.len() * 2);
    label.push_str(DETACHED_PLUGIN_WINDOW_PREFIX);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut label, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(label)
}

fn detached_plugin_app_route(plugin_id: &str) -> Result<PathBuf, String> {
    if !is_plugin_id(plugin_id) {
        return Err("Invalid plugin ID.".to_owned());
    }
    // Plugin IDs use an intentionally query-safe ASCII alphabet. No path,
    // scheme, host, fragment, or additional query field can be injected.
    Ok(PathBuf::from(format!(
        "index.html?{DETACHED_PLUGIN_ROUTE_PARAMETER}={plugin_id}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_identity_and_route_are_derived_from_a_strict_plugin_id() {
        let label = detached_plugin_window_label("com.example.notes").expect("valid label");
        assert!(label.starts_with(DETACHED_PLUGIN_WINDOW_PREFIX));
        assert_eq!(
            label.len(),
            DETACHED_PLUGIN_WINDOW_PREFIX.len() + Sha256::output_size() * 2
        );
        assert!(label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'));
        assert_eq!(
            detached_plugin_app_route("com.example.notes")
                .expect("valid route")
                .to_string_lossy(),
            "index.html?ihubDetachedPlugin=com.example.notes"
        );

        for invalid in [
            "x",
            "../escape",
            "https://example.com",
            "plugin?next=evil",
            "plugin#fragment",
            "插件",
        ] {
            assert!(detached_plugin_window_label(invalid).is_err());
            assert!(detached_plugin_app_route(invalid).is_err());
        }
    }

    #[test]
    fn registry_rejects_plugin_or_lease_confusion() {
        let registry = DetachedPluginWindowRegistry::default();
        let (label, created) = registry
            .reserve("com.example.notes")
            .expect("reserve valid plugin");
        assert!(created);
        assert_eq!(
            registry.reserve("com.example.notes"),
            Ok((label.clone(), false))
        );
        assert_eq!(
            registry
                .window_label_for_plugin("com.example.notes")
                .as_deref(),
            Some(label.as_str())
        );
        assert_eq!(registry.window_label_for_plugin("com.example.other"), None);
        assert_eq!(registry.window_label_for_plugin("../escape"), None);
        assert_eq!(
            registry.window_label_and_lease_for_plugin("com.example.notes"),
            None
        );
        assert!(registry
            .validate_window_plugin(&label, "com.example.other")
            .is_err());
        assert!(registry
            .bind_lease(&label, "com.example.other", "other-lease")
            .is_err());
        registry
            .bind_lease(&label, "com.example.notes", "owned-lease")
            .expect("bind owned lease");
        assert_eq!(
            registry.window_label_and_lease_for_plugin("com.example.notes"),
            Some((label.clone(), "owned-lease".to_owned()))
        );
        assert!(registry.owns_lease(&label, "owned-lease"));
        assert!(!registry.owns_lease(&label, "other-lease"));
        assert!(!registry.unbind_owned_lease(&label, "other-lease"));
        assert!(registry.unbind_owned_lease(&label, "owned-lease"));
    }

    #[test]
    fn closing_a_registered_window_returns_its_exact_lease_once() {
        let registry = DetachedPluginWindowRegistry::default();
        let (label, _) = registry
            .reserve("com.example.notes")
            .expect("reserve valid plugin");
        registry
            .bind_lease(&label, "com.example.notes", "surface-lease")
            .expect("bind surface lease");
        assert_eq!(
            registry.take_window_lease(&label).as_deref(),
            Some("surface-lease")
        );
        assert_eq!(registry.take_window_lease(&label), None);
        assert!(!registry.plugin_is_detached("com.example.notes"));
    }

    #[test]
    fn detached_capability_is_local_and_event_only() {
        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/plugin-detached.json"))
                .expect("capability JSON");
        assert_eq!(capability["local"], true);
        assert_eq!(
            capability["windows"],
            serde_json::json!(["plugin-detached-*"])
        );
        assert_eq!(
            capability["permissions"],
            serde_json::json!([
                "detached-plugin-host-commands",
                "core:event:allow-listen",
                "core:event:allow-unlisten"
            ])
        );
        assert!(!capability["permissions"]
            .as_array()
            .expect("permission list")
            .iter()
            .any(|permission| permission == "main-app-commands"));
        assert!(capability.get("remote").is_none());
    }
}
