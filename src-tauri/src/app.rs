use std::{
    collections::HashMap,
    path::PathBuf,
    process::Command,
    sync::RwLock,
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, State,
};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use crate::{
    indexer::{default_root_strings, SearchIndex},
    models::{
        AppHealth, AutostartStatus, IndexStatus, PluginCommandResult, PluginInfo, SearchResult,
    },
    plugins::PluginManager,
};

pub struct AppState {
    pub index: SearchIndex,
    pub plugins: PluginManager,
    pub started_at: String,
    host: PluginHostState,
}

impl AppState {
    fn new() -> Self {
        Self {
            index: SearchIndex::new(),
            plugins: PluginManager::new(),
            started_at: Utc::now().to_rfc3339(),
            host: PluginHostState::default(),
        }
    }
}

#[derive(Default)]
struct PluginHostState {
    commands: RwLock<HashMap<String, Value>>,
    search_providers: RwLock<HashMap<String, Value>>,
    settings: RwLock<HashMap<String, Value>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginHostRequest {
    plugin_id: String,
    method: String,
    #[serde(default)]
    params: Value,
}

/// The host receives the iframe bridge envelope as `plugin_host_call({ request })`.
#[derive(Debug, Deserialize)]
pub struct PluginHostCall {
    request: PluginHostRequest,
}

#[tauri::command]
pub fn get_index_status(state: State<'_, AppState>) -> IndexStatus {
    state.index.status()
}

#[tauri::command]
pub fn search_entries(
    query: String,
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Vec<SearchResult> {
    state.index.search(&query, limit)
}

#[tauri::command]
pub fn index_default_roots(state: State<'_, AppState>) -> IndexStatus {
    state.index.rebuild_default_roots()
}

#[tauri::command]
pub fn get_default_roots() -> Vec<String> {
    default_root_strings()
}

#[tauri::command]
pub async fn open_path(path: String) -> Result<(), String> {
    let path = PathBuf::from(path);
    let path = path
        .canonicalize()
        .map_err(|error| format!("Path cannot be opened: {error}"))?;
    tauri::async_runtime::spawn_blocking(move || open_path_in_system(&path))
        .await
        .map_err(|error| format!("Could not start the system opener task: {error}"))?
}

#[tauri::command]
pub fn list_plugins(state: State<'_, AppState>) -> Vec<PluginInfo> {
    state.plugins.list()
}

#[tauri::command]
pub async fn get_plugin_frontend_path(
    plugin_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let plugins = state.plugins.clone();
    tauri::async_runtime::spawn_blocking(move || {
        plugins
            .frontend_path(&plugin_id)
            .map(|path| path.to_string_lossy().into_owned())
    })
    .await
    .map_err(|error| format!("Plugin frontend path task failed: {error}"))?
}

#[tauri::command]
pub async fn install_plugin_from_git(
    source: String,
    state: State<'_, AppState>,
) -> Result<PluginInfo, String> {
    let plugins = state.plugins.clone();
    tauri::async_runtime::spawn_blocking(move || plugins.install_from_git(&source))
        .await
        .map_err(|error| format!("Plugin installation task failed: {error}"))?
}

#[tauri::command]
pub async fn run_plugin_command(
    plugin_id: String,
    command_id: String,
    input: Option<Value>,
    state: State<'_, AppState>,
) -> Result<PluginCommandResult, String> {
    let plugins = state.plugins.clone();
    tauri::async_runtime::spawn_blocking(move || {
        plugins.run_command(&plugin_id, &command_id, input)
    })
    .await
    .map_err(|error| format!("Plugin command task failed: {error}"))?
}

#[tauri::command]
pub fn get_autostart_status(app: AppHandle) -> Result<AutostartStatus, String> {
    let enabled = app
        .autolaunch()
        .is_enabled()
        .map_err(|error| format!("Could not read autostart status: {error}"))?;
    Ok(AutostartStatus {
        enabled,
        supported: cfg!(any(target_os = "windows", target_os = "macos")),
    })
}

#[tauri::command]
pub fn set_autostart(app: AppHandle, enabled: bool) -> Result<AutostartStatus, String> {
    if enabled {
        app.autolaunch()
            .enable()
            .map_err(|error| format!("Could not enable autostart: {error}"))?;
    } else {
        app.autolaunch()
            .disable()
            .map_err(|error| format!("Could not disable autostart: {error}"))?;
    }
    get_autostart_status(app)
}

#[tauri::command]
pub fn get_app_health(app: AppHandle, state: State<'_, AppState>) -> AppHealth {
    AppHealth {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        platform: std::env::consts::OS.to_owned(),
        started_at: state.started_at.clone(),
        autostart: app.autolaunch().is_enabled().unwrap_or(false),
        index: state.index.status(),
        plugin_count: state.plugins.list().len(),
    }
}

/// The SDK calls this command from a plugin frontend. Its values intentionally
/// stay JSON-shaped so independently developed plugins can evolve without a
/// host/SDK lock-step release.
#[tauri::command]
pub fn plugin_host_call(
    app: AppHandle,
    request: PluginHostCall,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    let request = request.request;
    if !is_plugin_id(&request.plugin_id) {
        return Err("Invalid plugin ID.".to_owned());
    }
    if let Some(permission) = PluginManager::required_permission_for_host_method(&request.method) {
        let allowed = state
            .plugins
            .allows_host_method(&request.plugin_id, &request.method)?;
        if !allowed {
            return Err(format!(
                "Plugin '{}' is not allowed to call '{}'. Declare permissions.{}: true in its v1 plugin manifest, then reinstall or update the plugin.",
                request.plugin_id, request.method, permission
            ));
        }
    }
    match request.method.as_str() {
        "commands.register" => {
            let definition = required_value(&request.params, "definition")?;
            let command_id = definition
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| "commands.register requires definition.id.".to_owned())?;
            if !is_plugin_id(command_id) {
                return Err("Invalid plugin command ID.".to_owned());
            }
            state
                .host
                .commands
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(host_key(&request.plugin_id, command_id), definition.clone());
            Ok(json!({ "registered": true }))
        }
        "commands.execute" => {
            let command_id = required_string_any(&request.params, &["commandId", "id"])?;
            if !state
                .host
                .commands
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .contains_key(&host_key(&request.plugin_id, command_id))
            {
                return Err(format!(
                    "Plugin command '{}/{}' is not registered.",
                    request.plugin_id, command_id
                ));
            }
            let request_id = next_request_id();
            let event_name = format!("ihub://plugin/{}/command", request.plugin_id);
            app.emit(
                &event_name,
                json!({
                    "requestId": request_id,
                    "commandId": command_id,
                    "input": request.params.get("input").cloned(),
                    "context": request.params.get("context").cloned(),
                }),
            )
            .map_err(|error| format!("Could not invoke plugin command: {error}"))?;
            Ok(json!({ "requestId": request_id }))
        }
        "commands.unregister" => {
            let command_id = required_string_any(&request.params, &["commandId", "id"])?;
            state
                .host
                .commands
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&host_key(&request.plugin_id, command_id));
            Ok(json!({ "unregistered": true }))
        }
        "search.register" => {
            let definition = required_value(&request.params, "definition")?;
            let provider_id = definition
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| "search.register requires definition.id.".to_owned())?;
            if !is_plugin_id(provider_id) {
                return Err("Invalid search provider ID.".to_owned());
            }
            state
                .host
                .search_providers
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(
                    host_key(&request.plugin_id, provider_id),
                    definition.clone(),
                );
            Ok(json!({ "registered": true }))
        }
        "search.unregister" => {
            let provider_id = required_string_any(&request.params, &["providerId", "id"])?;
            state
                .host
                .search_providers
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&host_key(&request.plugin_id, provider_id));
            Ok(json!({ "unregistered": true }))
        }
        "settings.get" => {
            let key = required_string(&request.params, "key")?;
            let value = state
                .host
                .settings
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&host_key(&request.plugin_id, key))
                .cloned()
                .unwrap_or_else(|| {
                    request
                        .params
                        .get("fallback")
                        .cloned()
                        .unwrap_or(Value::Null)
                });
            Ok(value)
        }
        "settings.set" => {
            let key = required_string(&request.params, "key")?;
            let value = required_value(&request.params, "value")?.clone();
            state
                .host
                .settings
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(host_key(&request.plugin_id, key), value);
            Ok(json!({ "saved": true }))
        }
        "lifecycle.ready" | "lifecycle.dispose" => Ok(json!({ "ok": true })),
        "commands.complete" | "search.complete" => {
            let event_name = format!("ihub://plugin/{}/response", request.plugin_id);
            app.emit(
                &event_name,
                json!({ "method": request.method, "params": request.params }),
            )
            .map_err(|error| format!("Could not forward plugin response: {error}"))?;
            Ok(json!({ "accepted": true }))
        }
        "clipboard.readText" | "clipboard.read" => {
            let mut clipboard = arboard::Clipboard::new()
                .map_err(|error| format!("Could not access the system clipboard: {error}"))?;
            clipboard
                .get_text()
                .map(Value::String)
                .map_err(|error| format!("Could not read the system clipboard: {error}"))
        }
        "clipboard.writeText" | "clipboard.write" => {
            let value = required_string(&request.params, "value")?;
            let mut clipboard = arboard::Clipboard::new()
                .map_err(|error| format!("Could not access the system clipboard: {error}"))?;
            clipboard
                .set_text(value)
                .map_err(|error| format!("Could not write to the system clipboard: {error}"))?;
            Ok(json!({ "written": true }))
        }
        "shell.openPath" | "shell.open" => {
            let path = PathBuf::from(required_string(&request.params, "path")?);
            let path = path
                .canonicalize()
                .map_err(|error| format!("Path cannot be opened: {error}"))?;
            open_path_in_system(&path)?;
            Ok(json!({ "opened": true }))
        }
        "shell.openExternal" => {
            open_external_in_system(required_string(&request.params, "url")?)?;
            Ok(json!({ "opened": true }))
        }
        // Process execution remains tied to run_plugin_command and its installed
        // manifest path. Other capability calls are delivered to an injected
        // production bridge, which may implement richer platform UX.
        "notifications.show" | "process.spawn" | "log" => {
            let event_name = format!("ihub://plugin/{}/host-call", request.plugin_id);
            app.emit(
                &event_name,
                json!({ "method": request.method, "params": request.params }),
            )
            .map_err(|error| format!("Could not forward plugin host call: {error}"))?;
            Ok(json!({ "accepted": true }))
        }
        _ => Err(format!(
            "Unsupported plugin host method '{}'.",
            request.method
        )),
    }
}

#[tauri::command]
pub fn invoke_plugin_frontend_command(
    app: AppHandle,
    plugin_id: String,
    command_id: String,
    input: Option<Value>,
    context: Option<Value>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    if !is_plugin_id(&plugin_id) || !is_plugin_id(&command_id) {
        return Err("Invalid plugin or command ID.".to_owned());
    }
    if !state
        .host
        .commands
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains_key(&host_key(&plugin_id, &command_id))
    {
        return Err(format!(
            "Plugin command '{plugin_id}/{command_id}' is not registered."
        ));
    }
    let request_id = next_request_id();
    let event_name = format!("ihub://plugin/{plugin_id}/command");
    app.emit(
        &event_name,
        json!({
            "requestId": request_id,
            "commandId": command_id,
            "input": input,
            "context": context,
        }),
    )
    .map_err(|error| format!("Could not invoke plugin command: {error}"))?;
    Ok(request_id)
}

#[tauri::command]
pub fn query_plugin_search(
    app: AppHandle,
    plugin_id: String,
    provider_id: String,
    query: String,
    limit: Option<usize>,
    context: Option<Value>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    if !is_plugin_id(&plugin_id) || !is_plugin_id(&provider_id) {
        return Err("Invalid plugin or search provider ID.".to_owned());
    }
    if !state
        .host
        .search_providers
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains_key(&host_key(&plugin_id, &provider_id))
    {
        return Err(format!(
            "Plugin search provider '{plugin_id}/{provider_id}' is not registered."
        ));
    }
    let request_id = next_request_id();
    let event_name = format!("ihub://plugin/{plugin_id}/search");
    app.emit(
        &event_name,
        json!({
            "requestId": request_id,
            "providerId": provider_id,
            "query": query,
            "limit": limit,
            "context": context,
        }),
    )
    .map_err(|error| format!("Could not query plugin search provider: {error}"))?;
    Ok(request_id)
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            focus_search(app);
        }))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state == ShortcutState::Pressed {
                        focus_search(app);
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let state = AppState::new();
            state.index.rebuild_default_roots();
            app.manage(state);
            setup_tray(app)?;
            // On macOS Command+Space belongs to Spotlight, so CmdOrCtrl+Shift+Space
            // avoids stealing a system shortcut while remaining one-handed.
            let _ = app.global_shortcut().on_shortcut(
                "CmdOrCtrl+Shift+Space",
                |app, _shortcut, event| {
                    if event.state == ShortcutState::Pressed {
                        focus_search(app);
                    }
                },
            );
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_index_status,
            search_entries,
            index_default_roots,
            get_default_roots,
            open_path,
            list_plugins,
            get_plugin_frontend_path,
            install_plugin_from_git,
            run_plugin_command,
            get_autostart_status,
            set_autostart,
            get_app_health,
            plugin_host_call,
            invoke_plugin_frontend_command,
            query_plugin_search
        ])
        .run(tauri::generate_context!())
        .expect("error while running iHub");
}

fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show iHub", true, None::<&str>)?;
    let reindex = MenuItem::with_id(app, "reindex", "Refresh file index", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &reindex, &quit])?;
    let _tray = TrayIconBuilder::with_id("ihub-tray")
        .tooltip("iHub")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => focus_search(app),
            "reindex" => {
                app.state::<AppState>().index.rebuild_default_roots();
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    Ok(())
}

fn focus_search(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    let _ = app.emit("ihub://focus-search", json!({}));
}

fn open_path_in_system(path: &PathBuf) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("explorer.exe");
        command.arg(path);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(path);
        command
    };
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(path);
        command
    };
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Could not open {}: {error}", path.display()))
}

fn open_external_in_system(url: &str) -> Result<(), String> {
    let allowed = ["https://", "http://", "mailto:"];
    if !allowed.iter().any(|prefix| url.starts_with(prefix)) {
        return Err("Only http(s) and mailto URLs can be opened externally.".to_owned());
    }
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("explorer.exe");
        command.arg(url);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(url);
        command
    };
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    };
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Could not open external URL: {error}"))
}

fn required_value<'a>(params: &'a Value, key: &str) -> Result<&'a Value, String> {
    params
        .get(key)
        .ok_or_else(|| format!("Plugin host method requires params.{key}."))
}

fn required_string<'a>(params: &'a Value, key: &str) -> Result<&'a str, String> {
    required_value(params, key)?
        .as_str()
        .ok_or_else(|| format!("Plugin host method requires params.{key} to be a string."))
}

fn required_string_any<'a>(params: &'a Value, keys: &[&str]) -> Result<&'a str, String> {
    for key in keys {
        if let Some(value) = params.get(*key).and_then(Value::as_str) {
            return Ok(value);
        }
    }
    Err(format!(
        "Plugin host method requires one of params.{}.",
        keys.join(" or params.")
    ))
}

fn is_plugin_id(value: &str) -> bool {
    let length = value.len();
    (2..=96).contains(&length)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn host_key(plugin_id: &str, child_id: &str) -> String {
    format!("{plugin_id}:{child_id}")
}

fn next_request_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("req-{}-{nanos}", std::process::id())
}
