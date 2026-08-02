use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, RecvTimeoutError, SyncSender},
        Arc, Mutex, RwLock,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono::Utc;
use image::{codecs::png::PngEncoder, ColorType, ImageEncoder, ImageFormat, ImageReader, Limits};
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, PhysicalPosition, PhysicalSize,
    State, WindowEvent,
};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use tauri_plugin_notification::NotificationExt;
use tokio::task::AbortHandle;
use uuid::Uuid;

use crate::{
    ai_providers::{
        execute_chat_round, initial_wire_messages, tool_result_wire_message, validate_ai_option,
        AiProviderProfileView, AiProviderStore, AiProviderTestResult, AiToolCall,
        SaveAiProviderProfileInput, UtoolsAiMessage, UtoolsAiModelView, UtoolsAiOption,
    },
    background_process::background_command,
    clipboard_history::{
        ClipboardHistory, ClipboardHistoryRestoreResult, ClipboardHistorySnapshot,
    },
    detached_plugin_window::DetachedPluginWindowRegistry,
    host_log::{self, HostLogSnapshot},
    indexer::{
        default_root_strings, paths_refer_to_same_location, renderer_display_path, SearchIndex,
    },
    launcher_hotkey::{normalize_launcher_hotkey, LauncherHotkeyStore, DEFAULT_LAUNCHER_HOTKEY},
    launcher_shortcuts::{
        resolve_current_search_result_open_target, LauncherShortcutStore, LauncherShortcutView,
    },
    models::{
        AppHealth, AutostartStatus, ClipboardFile, ClipboardImage, IndexStatus,
        LauncherHotkeyStatus, OfficialWorkspacePluginProject, PluginAutomaticUpdateReport,
        PluginCommandInfo, PluginCommandResult, PluginInfo, PluginLifecycleUpdate,
        PluginProjectCreated, PluginSearchResponse, PluginSearchResult, PluginUninstallResult,
        PluginUpdateCheck, PluginUpdateResult, SearchResult,
    },
    native_icons::NativeIconService,
    plugin_asset_server::{
        PluginAssetServer, PluginFrontendLease, PluginFrontendPurpose, PluginNativeCommandLease,
        UtoolsDialogRequest,
    },
    plugin_crypto_storage::PluginCryptoStorage,
    plugin_settings::PluginSettingsStore,
    plugin_shortcuts::{
        apply_plugin_shortcut_statuses, binding_is_current, binding_targets_frontend_command,
        plan_plugin_shortcuts, PluginShortcutBinding, PluginShortcutEvent, PluginShortcutRegistry,
        PluginShortcutStatus,
    },
    plugins::{
        validate_utools_tool_value, PluginManager, UtoolsCompatRuntimeConfig,
        UTOOLS_MAIN_PUSH_PROVIDER_ID,
    },
    project_template::create_plugin_project as create_plugin_project_template,
    super_panel::{SuperPanelState, SuperPanelStatus, SuperPanelTrigger},
    system_open::{LocalOpenKind, LocalPathIdentity, PreparedLocalOpen},
    utools_browser_window::{
        create_utools_browser_window, UtoolsBrowserWindowOptions, UtoolsBrowserWindowRegistry,
        UTOOLS_BROWSER_WINDOW_PREFIX,
    },
    utools_ffmpeg::{FfmpegControl, UtoolsFfmpegIntegration},
    utools_sharp::SharpRequest,
    utools_ubrowser::{
        run_chain as run_utools_ubrowser_chain, UBrowserRunRequest, UtoolsUBrowserRegistry,
        UTOOLS_UBROWSER_WINDOW_PREFIX,
    },
};

const LAUNCHER_INITIAL_BLUR_GRACE: Duration = Duration::from_millis(700);
/// Tauri's `Alt` modifier maps to `Option` on macOS.
const LAUNCHER_PRIMARY_HOTKEY: &str = DEFAULT_LAUNCHER_HOTKEY;
const LAUNCHER_FALLBACK_HOTKEY: &str = "Alt+Shift+Space";
/// Suppress shortcut auto-repeat without making two deliberate presses feel
/// sluggish. The global shortcut callback reports `Pressed` events directly,
/// so this gate belongs in the native resident process.
const LAUNCHER_HOTKEY_TOGGLE_DEBOUNCE: Duration = Duration::from_millis(160);
/// The visual design target is expressed in logical pixels so it keeps the
/// same proportions on mixed-DPI Windows and macOS displays. It is an upper
/// bound, not a minimum: a compact work area must always win.
const LAUNCHER_DESIGN_WIDTH_LOGICAL: f64 = 800.0;
const LAUNCHER_DESIGN_HEIGHT_LOGICAL: f64 = 380.0;
const MAX_SYSTEM_ICON_TARGETS: usize = 12;
const MAX_SYSTEM_ICON_SEARCH_ID_BYTES: usize = 8 * 1024;
const MAX_SYSTEM_ICON_SHORTCUT_ID_BYTES: usize = 128;
const MAX_SYSTEM_ICON_REQUEST_BYTES: usize = 32 * 1024;
const MAX_LOCAL_SEARCH_SELECTION: usize = 64;
const MAX_SUPER_PANEL_TEXT_BYTES: usize = 4 * 1024;
const TEMPORARY_PATH_OPEN_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_TEMPORARY_PATH_OPEN_GRANTS: usize = 96;
const MAX_TEMPORARY_PATH_OPEN_ID_BYTES: usize = 96;
const MAX_FIRST_PARTY_INDEX_ROOTS: usize = 32;
const IHUB_HELP_URL: &str = "https://github.com/neko233-com/ihub#readme";
const IHUB_FEEDBACK_URL: &str = "https://github.com/neko233-com/ihub/issues";
const UTOOLS_DB_STORAGE_PREFIX: &str = "utools.db.";
const UTOOLS_NATIVE_ID_SETTING_KEY: &str = "ihub.host.utools-native-id";
const MAX_UTOOLS_DB_STORAGE_KEY_BYTES: usize = 48;
const UTOOLS_DYNAMIC_FEATURE_PREFIX: &str = "utools.feature.";
const MAX_UTOOLS_DYNAMIC_FEATURES: usize = 64;
const MAX_UTOOLS_DYNAMIC_COMMANDS: usize = 16;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
enum UtoolsDynamicPlatforms {
    One(String),
    Many(Vec<String>),
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UtoolsDynamicFeature {
    code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    explain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    platform: Option<UtoolsDynamicPlatforms>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    main_hide: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    main_push: Option<bool>,
    cmds: Vec<String>,
}

/// A small, platform-neutral work-area snapshot used to calculate the next
/// launcher reveal. Keeping this calculation free of window APIs makes it
/// easy to prove that a dragged position is never retained between reveals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LauncherWorkArea {
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LauncherRevealLayout {
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
}

fn physical_point_in_monitor(
    point: PhysicalPosition<f64>,
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
) -> bool {
    if !point.x.is_finite() || !point.y.is_finite() || size.width == 0 || size.height == 0 {
        return false;
    }
    let left = f64::from(position.x);
    let top = f64::from(position.y);
    let right = left + f64::from(size.width);
    let bottom = top + f64::from(size.height);
    point.x >= left && point.x < right && point.y >= top && point.y < bottom
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LauncherInvocationSource {
    Hotkey,
    Explicit,
}

impl LauncherInvocationSource {
    fn reason(self) -> &'static str {
        match self {
            Self::Hotkey => "hotkey",
            Self::Explicit => "explicit",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LauncherVisibilitySnapshot {
    visible: bool,
    focused: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LauncherVisibilityAction {
    RevealFresh,
    FocusExisting,
    Hide,
}

fn launcher_visibility_action(
    source: LauncherInvocationSource,
    snapshot: LauncherVisibilitySnapshot,
) -> LauncherVisibilityAction {
    if !snapshot.visible {
        LauncherVisibilityAction::RevealFresh
    } else if source == LauncherInvocationSource::Hotkey && snapshot.focused {
        LauncherVisibilityAction::Hide
    } else {
        LauncherVisibilityAction::FocusExisting
    }
}

#[derive(Default)]
struct LauncherHotkeyToggleGate {
    pressed: bool,
    last_accepted_at: Option<Instant>,
}

impl LauncherHotkeyToggleGate {
    fn accept_press_at(&mut self, now: Instant) -> bool {
        if self.pressed {
            return false;
        }
        // Mark the physical key sequence held even when a too-fast second
        // press is rejected, so its auto-repeat cannot become a later toggle.
        self.pressed = true;
        if self
            .last_accepted_at
            .and_then(|previous| now.checked_duration_since(previous))
            .is_some_and(|elapsed| elapsed < LAUNCHER_HOTKEY_TOGGLE_DEBOUNCE)
        {
            return false;
        }
        self.last_accepted_at = Some(now);
        true
    }

    fn release(&mut self) {
        self.pressed = false;
    }
}

impl LauncherWorkArea {
    fn reveal_layout(self, scale_factor: f64) -> Option<LauncherRevealLayout> {
        if self.size.width == 0 || self.size.height == 0 {
            return None;
        }

        let desired = PhysicalSize::new(
            logical_dimension_to_physical(LAUNCHER_DESIGN_WIDTH_LOGICAL, scale_factor),
            logical_dimension_to_physical(LAUNCHER_DESIGN_HEIGHT_LOGICAL, scale_factor),
        );
        // Do not impose a native minimum size here. The WebView may need to
        // scroll on a very small display, but the native surface itself must
        // remain wholly inside the usable (taskbar/dock-excluding) work area.
        let size = PhysicalSize::new(
            desired.width.min(self.size.width),
            desired.height.min(self.size.height),
        );
        let x =
            i64::from(self.position.x) + i64::from(self.size.width.saturating_sub(size.width) / 2);
        let y = i64::from(self.position.y)
            + i64::from(self.size.height.saturating_sub(size.height) / 2);

        Some(LauncherRevealLayout {
            position: PhysicalPosition::new(clamp_i64_to_i32(x), clamp_i64_to_i32(y)),
            size,
        })
    }

    fn super_panel_layout(
        self,
        scale_factor: f64,
        trigger: PhysicalPosition<i32>,
    ) -> Option<LauncherRevealLayout> {
        if self.size.width == 0 || self.size.height == 0 {
            return None;
        }
        let desired = PhysicalSize::new(
            logical_dimension_to_physical(LAUNCHER_DESIGN_WIDTH_LOGICAL, scale_factor),
            logical_dimension_to_physical(LAUNCHER_DESIGN_HEIGHT_LOGICAL, scale_factor),
        );
        let size = PhysicalSize::new(
            desired.width.min(self.size.width),
            desired.height.min(self.size.height),
        );
        let left = i64::from(self.position.x);
        let top = i64::from(self.position.y);
        let right = left + i64::from(self.size.width);
        let bottom = top + i64::from(self.size.height);
        let width = i64::from(size.width);
        let height = i64::from(size.height);
        let gap = i64::from(logical_dimension_to_physical(12.0, scale_factor));
        let trigger_x = i64::from(trigger.x);
        let trigger_y = i64::from(trigger.y);
        let x = (trigger_x - width / 2).clamp(left, right.saturating_sub(width).max(left));
        let below = trigger_y.saturating_add(gap);
        let above = trigger_y.saturating_sub(gap).saturating_sub(height);
        let y = if below.saturating_add(height) <= bottom {
            below
        } else {
            above
        }
        .clamp(top, bottom.saturating_sub(height).max(top));
        Some(LauncherRevealLayout {
            position: PhysicalPosition::new(clamp_i64_to_i32(x), clamp_i64_to_i32(y)),
            size,
        })
    }
}

fn logical_dimension_to_physical(logical_dimension: f64, scale_factor: f64) -> u32 {
    let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let pixels = (logical_dimension * scale_factor).round();
    pixels.clamp(1.0, u32::MAX as f64) as u32
}

fn clamp_i64_to_i32(value: i64) -> i32 {
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TemporaryPathOpenKind {
    File,
    Folder,
}

impl TemporaryPathOpenKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Folder => "folder",
        }
    }

    fn local_open_kind(self) -> LocalOpenKind {
        match self {
            Self::File => LocalOpenKind::File,
            Self::Folder => LocalOpenKind::Folder,
        }
    }

    fn from_local_open_kind(kind: LocalOpenKind) -> Self {
        match kind {
            LocalOpenKind::File => Self::File,
            LocalOpenKind::Folder => Self::Folder,
        }
    }
}

#[derive(Debug, Clone)]
struct TemporaryPathOpenGrant {
    canonical_path: PathBuf,
    kind: TemporaryPathOpenKind,
    identity: LocalPathIdentity,
    issued_at: Instant,
}

#[derive(Debug)]
struct IssuedTemporaryPathOpen {
    open_id: String,
    canonical_path: PathBuf,
    kind: TemporaryPathOpenKind,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectedDirectoryGrant {
    path: String,
    open_id: String,
}

#[derive(Debug)]
struct AuthorizedIndexRootUpdate {
    roots: Vec<String>,
    guards: Vec<PreparedLocalOpen>,
}

#[derive(Debug, Default)]
struct TemporaryPathOpenStore {
    grants: Mutex<HashMap<String, TemporaryPathOpenGrant>>,
}

impl TemporaryPathOpenStore {
    fn issue(&self, path: &Path) -> Result<IssuedTemporaryPathOpen, String> {
        self.issue_at(path, Instant::now())
    }

    fn issue_at(&self, path: &Path, issued_at: Instant) -> Result<IssuedTemporaryPathOpen, String> {
        let prepared = crate::system_open::prepare_local_open(path, None)?;
        let canonical_path = prepared.path().to_path_buf();
        let kind = TemporaryPathOpenKind::from_local_open_kind(prepared.kind());
        let identity = prepared.identity();
        let open_id = next_capability_id("open");
        let mut grants = self
            .grants
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        remove_expired_temporary_path_open_grants(&mut grants, issued_at);
        grants.insert(
            open_id.clone(),
            TemporaryPathOpenGrant {
                canonical_path: canonical_path.clone(),
                kind,
                identity,
                issued_at,
            },
        );
        trim_oldest_records(&mut grants, MAX_TEMPORARY_PATH_OPEN_GRANTS, |grant| {
            grant.issued_at
        });
        Ok(IssuedTemporaryPathOpen {
            open_id,
            canonical_path,
            kind,
        })
    }

    #[cfg(test)]
    fn resolve(&self, open_id: &str) -> Result<PathBuf, String> {
        self.resolve_at(open_id, Instant::now())
    }

    fn prepare_open(&self, open_id: &str) -> Result<PreparedLocalOpen, String> {
        self.prepare_kind_at(open_id, None, Instant::now())
    }

    #[cfg(test)]
    fn resolve_at(&self, open_id: &str, now: Instant) -> Result<PathBuf, String> {
        self.resolve_kind_at(open_id, None, now)
    }

    fn prepare_folder(&self, open_id: &str) -> Result<PreparedLocalOpen, String> {
        self.prepare_kind_at(open_id, Some(TemporaryPathOpenKind::Folder), Instant::now())
    }

    #[cfg(test)]
    fn resolve_kind_at(
        &self,
        open_id: &str,
        expected_kind: Option<TemporaryPathOpenKind>,
        now: Instant,
    ) -> Result<PathBuf, String> {
        self.prepare_kind_at(open_id, expected_kind, now)
            .map(|prepared| prepared.path().to_path_buf())
    }

    fn prepare_kind_at(
        &self,
        open_id: &str,
        expected_kind: Option<TemporaryPathOpenKind>,
        now: Instant,
    ) -> Result<PreparedLocalOpen, String> {
        if !temporary_path_open_id_is_valid(open_id) {
            return Err(
                "This open authorization is unknown or expired. Select the item again.".to_owned(),
            );
        }
        let grant = {
            let mut grants = self
                .grants
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            remove_expired_temporary_path_open_grants(&mut grants, now);
            grants.get(open_id).cloned().ok_or_else(|| {
                "This open authorization is unknown or expired. Select the item again.".to_owned()
            })?
        };
        if let Some(expected_kind) = expected_kind {
            if expected_kind != grant.kind {
                return Err(format!(
                    "This authorization is not for a {}. Select the folder again.",
                    expected_kind.as_str()
                ));
            }
        }
        let prepared = crate::system_open::prepare_local_open(
            &grant.canonical_path,
            Some(grant.kind.local_open_kind()),
        )?;
        if prepared.path() != grant.canonical_path {
            return Err(
                "The selected filesystem target changed after authorization; it was not opened."
                    .to_owned(),
            );
        }
        if prepared.identity() != grant.identity {
            return Err(
                "The selected filesystem target was replaced after authorization; it was not opened."
                    .to_owned(),
            );
        }
        Ok(prepared)
    }
}

fn authorize_index_root_update(
    current_roots: &[PathBuf],
    requested_roots: &[String],
    directory_open_ids: &[String],
    temporary_path_opens: &TemporaryPathOpenStore,
) -> Result<AuthorizedIndexRootUpdate, String> {
    if requested_roots.len() > MAX_FIRST_PARTY_INDEX_ROOTS
        || directory_open_ids.len() > MAX_FIRST_PARTY_INDEX_ROOTS
    {
        return Err(format!(
            "Choose at most {MAX_FIRST_PARTY_INDEX_ROOTS} local index folders."
        ));
    }
    if requested_roots.is_empty() {
        return Ok(AuthorizedIndexRootUpdate {
            roots: Vec::new(),
            guards: Vec::new(),
        });
    }

    let mut selected_candidates = directory_open_ids
        .iter()
        .map(|open_id| temporary_path_opens.prepare_folder(open_id).map(Some))
        .collect::<Result<Vec<_>, _>>()?;

    let mut normalized_roots = Vec::with_capacity(requested_roots.len());
    let mut guards: Vec<PreparedLocalOpen> = Vec::with_capacity(requested_roots.len());
    for root in requested_roots {
        if root.is_empty() || root.trim() != root {
            return Err(
                "Index folders must use the exact path returned by the system folder picker."
                    .to_owned(),
            );
        }
        let requested = Path::new(root);
        if !requested.is_absolute() {
            return Err(
                "Index folders must use the exact path returned by the system folder picker."
                    .to_owned(),
            );
        }
        if normalized_roots
            .iter()
            .any(|existing| paths_refer_to_same_location(Path::new(existing), requested))
        {
            return Err("The same index folder cannot be added more than once.".to_owned());
        }
        let prepared = if let Some(current) = current_roots
            .iter()
            .find(|current| paths_refer_to_same_location(current, requested))
        {
            crate::system_open::prepare_local_open(current, Some(LocalOpenKind::Folder))?
        } else if let Some(candidate_index) = selected_candidates.iter().position(|candidate| {
            candidate
                .as_ref()
                .is_some_and(|prepared| paths_refer_to_same_location(requested, prepared.path()))
        }) {
            selected_candidates[candidate_index]
                .take()
                .expect("the matching prepared root remains available")
        } else {
            return Err(format!(
                "Index folder '{root}' is not an existing host root or a current system-folder selection. Choose it again."
            ));
        };
        normalized_roots.push(prepared.path().to_string_lossy().into_owned());
        guards.push(prepared);
    }
    Ok(AuthorizedIndexRootUpdate {
        roots: normalized_roots,
        guards,
    })
}

pub struct AppState {
    pub index: SearchIndex,
    pub plugins: PluginManager,
    pub clipboard_history: ClipboardHistory,
    pub cloud_drive: crate::cloud_drive::CloudDriveState,
    pub hosts_manager: crate::hosts_manager::HostsManagerState,
    pub lan_file_share: crate::lan_share::LanFileShareState,
    pub started_at: String,
    launcher_shortcuts: LauncherShortcutStore,
    plugin_assets: PluginAssetServer,
    plugin_crypto_storage: PluginCryptoStorage,
    plugin_settings: PluginSettingsStore,
    plugin_shortcut_preferences: crate::plugin_shortcut_preferences::PluginShortcutPreferenceStore,
    ai_providers: AiProviderStore,
    ffmpeg: UtoolsFfmpegIntegration,
    utools_documents: crate::utools_db::UtoolsDocumentStore,
    host: Arc<PluginHostState>,
    launcher_focus: LauncherFocusGate,
    launcher_hotkey_store: LauncherHotkeyStore,
    /// Serializes native register/persist/unregister transactions so two rapid
    /// settings clicks cannot strand the resident launcher without a binding.
    launcher_hotkey_change: Mutex<()>,
    launcher_hotkey: Mutex<LauncherHotkeyStatus>,
    launcher_hotkey_toggle: Mutex<LauncherHotkeyToggleGate>,
    /// The one external top-level window that owned the foreground immediately
    /// before the current launcher reveal. uTools compatibility reads never
    /// enumerate arbitrary windows and revalidate this exact HWND/PID pair.
    previous_foreground: Mutex<Option<crate::utools_foreground::ForegroundWindowTarget>>,
    /// Serializes best-effort plugin binding refreshes without ever sharing
    /// the launcher's registration transaction or unregistering its recovery
    /// accelerator.
    plugin_shortcut_change: Mutex<()>,
    plugin_shortcuts: Mutex<PluginShortcutRegistry>,
    super_panel: Arc<SuperPanelState>,
    /// First-party surfaces can open only filesystem objects selected or
    /// created by the native host. The WebView receives an opaque, bounded,
    /// short-lived ID instead of a reusable arbitrary-path command.
    temporary_path_opens: Arc<TemporaryPathOpenStore>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum SuperPanelContextPayload {
    Files { files: Vec<ClipboardFile> },
    Image { image: ClipboardImage },
    Text { text: String },
    Empty,
}

impl AppState {
    fn new(app_data_dir: PathBuf) -> Self {
        let plugins = PluginManager::new();
        let plugin_crypto_storage = PluginCryptoStorage::new(app_data_dir.clone());
        let plugin_settings = PluginSettingsStore::new(app_data_dir.clone());
        let plugin_shortcut_preferences =
            crate::plugin_shortcut_preferences::PluginShortcutPreferenceStore::new(
                app_data_dir.clone(),
            );
        let ai_providers =
            AiProviderStore::new(plugin_settings.clone(), plugin_crypto_storage.clone());
        let ffmpeg = UtoolsFfmpegIntegration::new(app_data_dir.clone());
        let utools_documents = crate::utools_db::UtoolsDocumentStore::new(app_data_dir.clone());
        let super_panel = Arc::new(SuperPanelState::with_storage(app_data_dir.clone()));
        // Older development builds persisted every setting. Before plugin
        // frontends can access the host, scrub any value now declared secret
        // from that JSON file in one atomic update.
        if let Err(error) =
            plugin_settings.remove_declared_secrets(plugins.declared_secret_setting_keys())
        {
            host_log::error(
                "plugins",
                format!("Could not scrub legacy secret plugin settings: {error}"),
            );
        }
        Self {
            index: SearchIndex::with_storage(app_data_dir.clone()),
            plugins,
            clipboard_history: ClipboardHistory::new(app_data_dir.clone()),
            cloud_drive: crate::cloud_drive::CloudDriveState::new(app_data_dir.clone()),
            hosts_manager: crate::hosts_manager::HostsManagerState::default(),
            lan_file_share: crate::lan_share::LanFileShareState::default(),
            started_at: Utc::now().to_rfc3339(),
            launcher_shortcuts: LauncherShortcutStore::new(app_data_dir.clone()),
            plugin_assets: PluginAssetServer::new(),
            plugin_crypto_storage,
            plugin_settings,
            plugin_shortcut_preferences,
            ai_providers,
            ffmpeg,
            utools_documents,
            host: Arc::new(PluginHostState::default()),
            launcher_focus: LauncherFocusGate::default(),
            launcher_hotkey_store: LauncherHotkeyStore::new(app_data_dir),
            launcher_hotkey_change: Mutex::new(()),
            launcher_hotkey: Mutex::new(LauncherHotkeyStatus::unavailable()),
            launcher_hotkey_toggle: Mutex::new(LauncherHotkeyToggleGate::default()),
            previous_foreground: Mutex::new(None),
            plugin_shortcut_change: Mutex::new(()),
            plugin_shortcuts: Mutex::new(PluginShortcutRegistry::default()),
            super_panel,
            temporary_path_opens: Arc::new(TemporaryPathOpenStore::default()),
        }
    }

    fn launcher_hotkey_status(&self) -> LauncherHotkeyStatus {
        self.launcher_hotkey
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn set_launcher_hotkey_status(&self, status: LauncherHotkeyStatus) {
        *self
            .launcher_hotkey
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = status;
    }

    fn accept_launcher_hotkey_press(&self) -> bool {
        self.launcher_hotkey_toggle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .accept_press_at(Instant::now())
    }

    fn release_launcher_hotkey(&self) {
        self.launcher_hotkey_toggle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .release();
    }

    fn reset_launcher_hotkey_toggle(&self) {
        *self
            .launcher_hotkey_toggle
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = LauncherHotkeyToggleGate::default();
    }

    fn capture_previous_foreground(&self) {
        *self
            .previous_foreground
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            crate::utools_foreground::capture_external_foreground_window();
    }

    fn previous_foreground(
        &self,
    ) -> Result<crate::utools_foreground::ForegroundWindowTarget, String> {
        self.previous_foreground
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .ok_or_else(|| {
                "iHub did not capture an external foreground window for this launcher session."
                    .to_owned()
            })
    }

    fn plugin_shortcut_binding(&self, shortcut: &str) -> Option<PluginShortcutBinding> {
        self.plugin_shortcuts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .active
            .get(shortcut)
            .cloned()
    }

    fn project_plugin_shortcut_statuses(&self, plugins: &mut [PluginInfo]) {
        if let Err(error) = self.plugin_shortcut_preferences.apply_to_plugins(plugins) {
            host_log::error(
                "hotkey",
                format!("Could not apply plugin shortcut preferences: {error}"),
            );
        }
        let registry = self
            .plugin_shortcuts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        apply_plugin_shortcut_statuses(plugins, &registry.statuses);
    }
}

/// Windows can emit an initial `Focused(false)` while a hidden, frameless
/// launcher is being shown and before `set_focus()` has completed. Auto-hide
/// only after the current reveal has actually acquired focus; otherwise a
/// manual launch can disappear before the user ever sees it.
#[derive(Default)]
struct LauncherFocusGate {
    focused_since_reveal: AtomicBool,
    revealed_at: Mutex<Option<Instant>>,
}

impl LauncherFocusGate {
    fn begin_reveal(&self) {
        self.begin_reveal_at(Instant::now());
    }

    fn begin_reveal_at(&self, revealed_at: Instant) {
        self.focused_since_reveal.store(false, Ordering::Release);
        *self
            .revealed_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(revealed_at);
    }

    fn note_focus(&self) {
        self.focused_since_reveal.store(true, Ordering::Release);
    }

    fn consume_blur_after_focus(&self) -> bool {
        if !self.focused_since_reveal.swap(false, Ordering::AcqRel) {
            return false;
        }
        let revealed_at = *self
            .revealed_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        !revealed_at.is_some_and(|revealed_at| revealed_at.elapsed() < LAUNCHER_INITIAL_BLUR_GRACE)
    }
}

struct PluginHostState {
    commands: RwLock<HashMap<String, Value>>,
    search_providers: RwLock<HashMap<String, Value>>,
    /// Manifest-declared secret settings live only for this iHub process.
    /// They are never serialized into the app-data JSON settings store.
    secret_settings: RwLock<HashMap<String, Value>>,
    /// Query receivers are owned by the host, not the iframe. A response must
    /// match both its opaque request id and the iframe's fixed plugin id.
    pending_searches: Mutex<HashMap<String, PendingPluginSearch>>,
    /// Bounded, short-lived search snapshots let the trusted launcher select
    /// an exact result without resubmitting plugin-controlled payload data
    /// when the owning frontend lives in a detached host.
    issued_search_results: Mutex<HashMap<String, IssuedPluginSearchResults>>,
    /// A main-push result is selected in the trusted launcher, but its
    /// synchronous uTools `onSelect` callback executes in the owning iframe.
    /// This one-shot rendezvous binds that callback (and any immediate paste
    /// it schedules) to the exact plugin lease that received the event.
    pending_utools_main_push_selections: Mutex<HashMap<String, PendingUtoolsMainPushSelection>>,
    /// A handler becomes callable only after the current lifecycle-owning
    /// iframe registers an exact manifest-declared uTools MCP name. Records
    /// are bound to the opaque lease and native host window that observed the
    /// registration, so a replacement document cannot inherit callbacks.
    utools_tools: RwLock<HashMap<(String, String), RegisteredUtoolsTool>>,
    /// Tool calls are native-owned rendezvous records. Renderer completions
    /// must match plugin, tool, request, and lease before they can resolve the
    /// trusted caller's wait.
    pending_utools_tool_calls: Mutex<HashMap<String, PendingUtoolsToolCall>>,
    /// Every plugin AI request is bound to its exact iframe lease and owns an
    /// abort handle. Closing or replacing a runtime therefore cancels both
    /// remote I/O and any outstanding Function Calling rendezvous.
    utools_ai_requests: Mutex<HashMap<String, ActiveUtoolsAiRequest>>,
    pending_utools_ai_function_calls: Mutex<HashMap<String, PendingUtoolsAiFunctionCall>>,
    /// A frontend can access only a directory selected through the native
    /// picker during its own session. The opaque id is scoped to the plugin
    /// and expires quickly; it is never a reusable filesystem path grant.
    filesystem_grants: Mutex<HashMap<String, FilesystemGrant>>,
    /// A frontend can request files only through a native picker. It receives
    /// names and metadata, while a one-shot native command receives the
    /// canonical paths only after the same plugin submits this opaque token.
    file_grants: Mutex<HashMap<String, PluginFileGrant>>,
    /// A trusted launcher action can stage one bounded context for one
    /// declared frontend command. These records contain no ambient clipboard
    /// state and expose no filesystem paths to the plugin iframe.
    launcher_contexts: Mutex<HashMap<String, PluginLauncherContextTransfer>>,
    /// Applying a rename requires the exact, still-valid preview that the
    /// host generated for the same plugin and directory grant.
    batch_rename_previews: Mutex<HashMap<String, PluginBatchRenamePreview>>,
    /// `startDrag` may expose only filesystem objects returned by a native
    /// picker to this exact uTools iframe lease. Bind each visible raw path to
    /// the selected object identity and revalidate it before native dragging.
    utools_drag_grants: Mutex<HashMap<(String, String), Vec<UtoolsDragGrant>>>,
    /// `showSaveDialog` exposes a path string for uTools source compatibility,
    /// but writes remain bound to the exact parent directory identity and are
    /// single-use. Only host-owned Sharp/FFmpeg publishers consume this map.
    utools_save_grants: Mutex<HashMap<(String, String), Vec<UtoolsSaveGrant>>>,
    /// Long-running FFmpeg jobs are bound to one plugin lease and controlled
    /// only through their unguessable request ID. Lifecycle disposal requests
    /// a native kill while the worker lease remains held until the process
    /// has actually exited.
    utools_ffmpeg_jobs: Mutex<HashMap<String, ActiveUtoolsFfmpegJob>>,
    /// Native dialogs briefly move focus away from the frameless launcher.
    /// Keep its resident auto-hide behavior suspended until every modal
    /// picker has returned to avoid dismissing the plugin UI underneath it.
    native_dialog_depth: AtomicUsize,
    /// The browser-owned system screen picker used by `getDisplayMedia` is not
    /// a native dialog iHub can parent. Keep a short, opaque focus lease while
    /// that picker is pending so its temporary focus loss cannot hide the
    /// resident launcher. Each lease has a fixed deadline, so a crashed
    /// renderer can never disable auto-hide permanently.
    capture_focus_leases: Mutex<HashMap<String, CaptureFocusLease>>,
    /// A plugin with the explicitly reviewed `cursorColor` permission may ask
    /// for a single pixel under the cursor from its visible surface. Keep a
    /// small per-plugin reservation map so an iframe cannot turn that narrow
    /// action into a polling API.
    cursor_color_sampled_at: Mutex<HashMap<String, Instant>>,
    /// A trusted parent-frame confirmation creates one short-lived token. The
    /// iframe never receives it, so a plugin timer cannot self-authorize a
    /// cursor sample without the visible host overlay being confirmed.
    cursor_color_approvals: Mutex<HashMap<String, CursorColorApproval>>,
    /// Plugin-authored diagnostics are useful, but a broken iframe must not
    /// turn the synchronous bounded host log into a disk or transition-lock
    /// denial of service. Keep one small fixed-window counter per active
    /// plugin and aggregate drops without retaining message text.
    plugin_log_windows: Mutex<HashMap<String, PluginLogWindow>>,
    /// Native notifications are visible outside the plugin surface. Bound
    /// each plugin to a small fixed window so a broken or hostile iframe
    /// cannot flood Windows Action Center after receiving permission.
    plugin_notification_windows: Mutex<HashMap<String, PluginNotificationWindow>>,
}

impl Default for PluginHostState {
    fn default() -> Self {
        Self {
            commands: RwLock::new(HashMap::new()),
            search_providers: RwLock::new(HashMap::new()),
            secret_settings: RwLock::new(HashMap::new()),
            pending_searches: Mutex::new(HashMap::new()),
            issued_search_results: Mutex::new(HashMap::new()),
            pending_utools_main_push_selections: Mutex::new(HashMap::new()),
            utools_tools: RwLock::new(HashMap::new()),
            pending_utools_tool_calls: Mutex::new(HashMap::new()),
            utools_ai_requests: Mutex::new(HashMap::new()),
            pending_utools_ai_function_calls: Mutex::new(HashMap::new()),
            filesystem_grants: Mutex::new(HashMap::new()),
            file_grants: Mutex::new(HashMap::new()),
            launcher_contexts: Mutex::new(HashMap::new()),
            batch_rename_previews: Mutex::new(HashMap::new()),
            utools_drag_grants: Mutex::new(HashMap::new()),
            utools_save_grants: Mutex::new(HashMap::new()),
            utools_ffmpeg_jobs: Mutex::new(HashMap::new()),
            native_dialog_depth: AtomicUsize::new(0),
            capture_focus_leases: Mutex::new(HashMap::new()),
            cursor_color_sampled_at: Mutex::new(HashMap::new()),
            cursor_color_approvals: Mutex::new(HashMap::new()),
            plugin_log_windows: Mutex::new(HashMap::new()),
            plugin_notification_windows: Mutex::new(HashMap::new()),
        }
    }
}

#[derive(Debug, Clone)]
struct FilesystemGrant {
    plugin_id: String,
    directory: String,
    identity: LocalPathIdentity,
    issued_at: Instant,
}

#[derive(Debug, Clone)]
struct PluginFileGrant {
    plugin_id: String,
    files: Vec<SelectedPluginFile>,
    issued_at: Instant,
}

#[derive(Debug, Clone)]
struct UtoolsDragGrant {
    path: PathBuf,
    kind: LocalOpenKind,
    identity: LocalPathIdentity,
}

#[derive(Debug, Clone)]
struct UtoolsSaveGrant {
    path: PathBuf,
    parent: PathBuf,
    parent_identity: LocalPathIdentity,
}

struct ActiveUtoolsFfmpegJob {
    plugin_id: String,
    lease_id: String,
    control: Arc<FfmpegControl>,
}

#[derive(Debug)]
struct PreparedUtoolsFfmpegRun {
    args: Vec<String>,
    duration_seconds: Option<f64>,
    output_grant: UtoolsSaveGrant,
    staging_output: PathBuf,
    /// Revalidated picker objects stay alive for the complete native run.
    _inputs: Vec<PreparedLocalOpen>,
}

#[derive(Debug, Clone)]
struct SelectedPluginFile {
    path: PathBuf,
    name: String,
    size: u64,
}

/// Input accepted only from iHub's trusted parent renderer after a user has
/// chosen a concrete plugin action. Plugin iframes run on a loopback remote
/// origin and cannot invoke this Tauri command directly.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginLauncherContextRequest {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    files: Vec<PluginLauncherContextFileRequest>,
    image: Option<PluginLauncherContextImageRequest>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PluginLauncherContextFileRequest {
    /// This path is trusted-parent input only. The host canonicalizes it and
    /// discards it before the context can cross into the plugin iframe.
    path: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PluginLauncherContextImageRequest {
    name: String,
    mime_type: String,
    width: u32,
    height: u32,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct PluginLauncherContextFileMetadata {
    /// An opaque identity only. This API deliberately does not make it a
    /// filesystem grant or a path resolver.
    handle_id: String,
    name: String,
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct PluginLauncherContextImageHandle {
    /// An opaque identity only. No bridge method resolves it to image bytes.
    handle_id: String,
    name: String,
    mime_type: String,
    width: u32,
    height: u32,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct PluginLauncherContextPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(default)]
    files: Vec<PluginLauncherContextFileMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<PluginLauncherContextImageHandle>,
}

#[derive(Debug, Clone)]
struct PluginLauncherContextTransfer {
    plugin_id: String,
    command_id: String,
    /// The visible iframe lease that was live when the trusted parent issued
    /// this record. A same-plugin reload must not attach the old token to its
    /// replacement document.
    frontend_lease_id: String,
    payload: PluginLauncherContextPayload,
    issued_at: Instant,
    expires_at: Instant,
    /// Empty until the trusted parent has actually dispatched the selected
    /// frontend command. A staged token is not readable by a plugin timer.
    dispatched_request_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginLauncherContextIssue {
    pub context_id: String,
    pub expires_in_ms: u64,
}

/// The only thing placed on a frontend command event. It contains no user
/// text, filesystem path, file contents, or image pixels.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct PluginLauncherContextInvocation {
    context_id: String,
    expires_in_ms: u64,
}

#[derive(Debug, Clone)]
struct PluginBatchRenamePreview {
    plugin_id: String,
    grant_id: String,
    preview: crate::builtin_tools::BatchRenamePreview,
    issued_at: Instant,
}

#[derive(Debug, Clone)]
struct CaptureFocusLease {
    /// `None` belongs to the trusted React host. Plugin-owned leases are
    /// released only through the same plugin's active bridge session.
    owner_plugin_id: Option<String>,
    expires_at: Instant,
}

#[derive(Debug, Clone)]
struct CursorColorApproval {
    plugin_id: String,
    lease_id: String,
    expires_at: Instant,
}

#[derive(Debug, Clone)]
struct PluginLogWindow {
    started_at: Instant,
    accepted: usize,
    dropped: u64,
}

#[derive(Debug, Clone)]
struct PluginNotificationWindow {
    started_at: Instant,
    accepted: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PluginLogAdmission {
    Accept { previously_dropped: u64 },
    Drop { report_limit: bool },
}

struct NativeDialogGuard<'a> {
    depth: &'a AtomicUsize,
}

impl<'a> NativeDialogGuard<'a> {
    fn begin(host: &'a PluginHostState) -> Self {
        host.native_dialog_depth.fetch_add(1, Ordering::AcqRel);
        Self {
            depth: &host.native_dialog_depth,
        }
    }
}

impl Drop for NativeDialogGuard<'_> {
    fn drop(&mut self) {
        self.depth.fetch_sub(1, Ordering::AcqRel);
    }
}

impl PluginHostState {
    fn native_dialog_is_open(&self) -> bool {
        self.native_dialog_depth.load(Ordering::Acquire) > 0
    }

    /// Issues a lease for the trusted host UI. The public Tauri command that
    /// calls this method never receives a plugin identity, so it must not be
    /// able to release a plugin-owned lease by guessing its opaque ID.
    fn acquire_capture_focus_lease(&self) -> String {
        self.issue_capture_focus_lease(None)
    }

    /// Issues the one active screen-picker lease for a specific plugin. A
    /// subsequent picker from the same plugin replaces its older lease; this
    /// keeps one plugin from consuming the shared bounded lease pool.
    fn acquire_plugin_capture_focus_lease(&self, plugin_id: &str) -> String {
        self.issue_capture_focus_lease(Some(plugin_id))
    }

    fn issue_capture_focus_lease(&self, owner_plugin_id: Option<&str>) -> String {
        let mut leases = self
            .capture_focus_leases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        remove_expired_capture_focus_leases(&mut leases, Instant::now());
        let owner_plugin_id = owner_plugin_id.map(str::to_owned);
        if let Some(plugin_id) = owner_plugin_id.as_deref() {
            // A plugin can be waiting on only one browser screen picker at a
            // time. Replacing its previous lease both bounds abuse and lets a
            // cancellation path safely acquire a fresh token.
            leases.retain(|_, lease| lease.owner_plugin_id.as_deref() != Some(plugin_id));
        }
        let lease_id = next_capability_id("capture-focus");
        leases.insert(
            lease_id.clone(),
            CaptureFocusLease {
                owner_plugin_id,
                expires_at: Instant::now() + CAPTURE_FOCUS_LEASE_TTL,
            },
        );
        trim_oldest_records(&mut leases, MAX_CAPTURE_FOCUS_LEASES, |lease| {
            lease.expires_at
        });
        lease_id
    }

    /// Releases a host-owned lease. A plugin-owned lease is intentionally not
    /// removable through this identity-free command.
    fn release_capture_focus_lease(&self, lease_id: &str) {
        let _ = self.release_capture_focus_lease_for_owner(None, lease_id);
    }

    /// Releases an opaque plugin lease only when the requesting plugin owns
    /// it. Missing/expired IDs are harmless for `finally` cleanup; an ID from
    /// another plugin is a hard error and remains active for its real owner.
    fn release_plugin_capture_focus_lease(
        &self,
        plugin_id: &str,
        lease_id: &str,
    ) -> Result<bool, String> {
        self.release_capture_focus_lease_for_owner(Some(plugin_id), lease_id)
    }

    fn release_capture_focus_lease_for_owner(
        &self,
        owner_plugin_id: Option<&str>,
        lease_id: &str,
    ) -> Result<bool, String> {
        let mut leases = self
            .capture_focus_leases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        remove_expired_capture_focus_leases(&mut leases, Instant::now());
        let Some(lease) = leases.get(lease_id) else {
            return Ok(false);
        };
        if lease.owner_plugin_id.as_deref() != owner_plugin_id {
            return Err(
                "This screen-capture focus lease belongs to another plugin session.".to_owned(),
            );
        }
        leases.remove(lease_id);
        Ok(true)
    }

    fn clear_plugin_capture_focus_leases(&self, plugin_id: &str) {
        let mut leases = self
            .capture_focus_leases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        remove_expired_capture_focus_leases(&mut leases, Instant::now());
        leases.retain(|_, lease| lease.owner_plugin_id.as_deref() != Some(plugin_id));
    }

    fn capture_focus_lease_is_active(&self) -> bool {
        let mut leases = self
            .capture_focus_leases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        remove_expired_capture_focus_leases(&mut leases, Instant::now());
        !leases.is_empty()
    }

    fn auto_hide_is_suspended(&self) -> bool {
        self.native_dialog_is_open() || self.capture_focus_lease_is_active()
    }

    fn admit_plugin_log(&self, plugin_id: &str) -> PluginLogAdmission {
        self.admit_plugin_log_at(plugin_id, Instant::now())
    }

    fn admit_plugin_log_at(&self, plugin_id: &str, now: Instant) -> PluginLogAdmission {
        let mut windows = self
            .plugin_log_windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        windows.retain(|_, window| {
            now.checked_duration_since(window.started_at)
                .unwrap_or_default()
                < PLUGIN_LOG_WINDOW_RETENTION
        });
        if let Some(window) = windows.get_mut(plugin_id) {
            if now
                .checked_duration_since(window.started_at)
                .unwrap_or_default()
                >= PLUGIN_LOG_WINDOW
            {
                let previously_dropped = window.dropped;
                *window = PluginLogWindow {
                    started_at: now,
                    accepted: 1,
                    dropped: 0,
                };
                return PluginLogAdmission::Accept { previously_dropped };
            }
            if window.accepted < MAX_PLUGIN_LOGS_PER_WINDOW {
                window.accepted += 1;
                return PluginLogAdmission::Accept {
                    previously_dropped: 0,
                };
            }
            window.dropped = window.dropped.saturating_add(1);
            return PluginLogAdmission::Drop {
                report_limit: window.dropped == 1,
            };
        }

        windows.insert(
            plugin_id.to_owned(),
            PluginLogWindow {
                started_at: now,
                accepted: 1,
                dropped: 0,
            },
        );
        trim_oldest_records(&mut windows, MAX_PLUGIN_LOG_WINDOWS, |window| {
            window.started_at
        });
        PluginLogAdmission::Accept {
            previously_dropped: 0,
        }
    }

    fn admit_plugin_notification(&self, plugin_id: &str) -> bool {
        self.admit_plugin_notification_at(plugin_id, Instant::now())
    }

    fn admit_plugin_notification_at(&self, plugin_id: &str, now: Instant) -> bool {
        let mut windows = self
            .plugin_notification_windows
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        windows.retain(|_, window| {
            now.checked_duration_since(window.started_at)
                .unwrap_or_default()
                < PLUGIN_NOTIFICATION_WINDOW_RETENTION
        });
        if let Some(window) = windows.get_mut(plugin_id) {
            if now
                .checked_duration_since(window.started_at)
                .unwrap_or_default()
                >= PLUGIN_NOTIFICATION_WINDOW
            {
                *window = PluginNotificationWindow {
                    started_at: now,
                    accepted: 1,
                };
                return true;
            }
            if window.accepted >= MAX_PLUGIN_NOTIFICATIONS_PER_WINDOW {
                return false;
            }
            window.accepted += 1;
            return true;
        }

        windows.insert(
            plugin_id.to_owned(),
            PluginNotificationWindow {
                started_at: now,
                accepted: 1,
            },
        );
        trim_oldest_records(&mut windows, MAX_PLUGIN_NOTIFICATION_WINDOWS, |window| {
            window.started_at
        });
        true
    }

    /// Reserves one fixed-delay cursor sample for a plugin. The reservation is
    /// made before the native call so concurrent iframe messages cannot race
    /// into a high-frequency cursor/screen sampling loop.
    fn reserve_plugin_cursor_color_sample(&self, plugin_id: &str) -> Result<(), String> {
        let now = Instant::now();
        let mut sampled_at = self
            .cursor_color_sampled_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        sampled_at.retain(|_, sampled| now.duration_since(*sampled) < CURSOR_COLOR_SAMPLE_COOLDOWN);
        if let Some(previous) = sampled_at.get(plugin_id) {
            let remaining = CURSOR_COLOR_SAMPLE_COOLDOWN
                .saturating_sub(now.duration_since(*previous))
                .as_millis();
            return Err(format!(
                "Cursor color sampling is rate-limited. Wait about {remaining} ms before sampling again."
            ));
        }
        sampled_at.insert(plugin_id.to_owned(), now);
        trim_oldest_records(
            &mut sampled_at,
            MAX_CURSOR_COLOR_SAMPLE_PLUGINS,
            |sampled| *sampled,
        );
        Ok(())
    }

    fn clear_plugin_cursor_color_sample(&self, plugin_id: &str) {
        self.cursor_color_sampled_at
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(plugin_id);
    }

    fn issue_plugin_cursor_color_approval(&self, plugin_id: &str, lease_id: &str) -> String {
        let now = Instant::now();
        let mut approvals = self
            .cursor_color_approvals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        remove_expired_cursor_color_approvals(&mut approvals, now);
        // At most one host-confirmed cursor sample may be outstanding for a
        // plugin. A fresh visible confirmation intentionally supersedes an
        // abandoned overlay response from the same plugin.
        approvals.retain(|_, approval| approval.plugin_id != plugin_id);
        let approval_id = next_capability_id("cursor-color");
        approvals.insert(
            approval_id.clone(),
            CursorColorApproval {
                plugin_id: plugin_id.to_owned(),
                lease_id: lease_id.to_owned(),
                expires_at: now + CURSOR_COLOR_APPROVAL_TTL,
            },
        );
        trim_oldest_records(&mut approvals, MAX_CURSOR_COLOR_APPROVALS, |approval| {
            approval.expires_at
        });
        approval_id
    }

    /// Consumes only the token issued to the same plugin and still-active
    /// frontend lease. A mismatched caller never consumes the real owner's
    /// token, which keeps a malicious iframe from cancelling another plugin's
    /// pending host confirmation.
    fn take_plugin_cursor_color_approval(
        &self,
        plugin_id: &str,
        lease_id: &str,
        approval_id: &str,
    ) -> Result<(), String> {
        let mut approvals = self
            .cursor_color_approvals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        remove_expired_cursor_color_approvals(&mut approvals, Instant::now());
        let Some(approval) = approvals.get(approval_id) else {
            return Err("This cursor-color approval has expired or was already used.".to_owned());
        };
        if approval.plugin_id != plugin_id || approval.lease_id != lease_id {
            return Err("This cursor-color approval belongs to another plugin session.".to_owned());
        }
        approvals.remove(approval_id);
        Ok(())
    }

    fn clear_plugin_cursor_color_approvals(&self, plugin_id: &str) {
        let mut approvals = self
            .cursor_color_approvals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        remove_expired_cursor_color_approvals(&mut approvals, Instant::now());
        approvals.retain(|_, approval| approval.plugin_id != plugin_id);
    }
}

struct PendingPluginSearch {
    plugin_id: String,
    provider_id: String,
    max_results: usize,
    response: SyncSender<Result<Vec<PluginSearchResult>, String>>,
}

#[derive(Debug)]
struct IssuedPluginSearchResults {
    plugin_id: String,
    provider_id: String,
    results: Vec<PluginSearchResult>,
    issued_at: Instant,
}

struct PendingUtoolsMainPushSelection {
    plugin_id: String,
    lease_id: Option<String>,
    completed: bool,
    response: SyncSender<Result<bool, String>>,
}

#[derive(Clone, Debug)]
struct RegisteredUtoolsTool {
    plugin_id: String,
    lease_id: String,
    window_label: String,
}

struct PendingUtoolsToolCall {
    plugin_id: String,
    name: String,
    lease_id: String,
    response: SyncSender<Result<Value, String>>,
}

struct ActiveUtoolsAiRequest {
    plugin_id: String,
    lease_id: String,
    cancelled: Arc<AtomicBool>,
    abort_handle: Option<AbortHandle>,
}

struct UtoolsAiStartContext {
    app: AppHandle,
    host: Arc<PluginHostState>,
    plugin_assets: PluginAssetServer,
    providers: AiProviderStore,
    plugin_id: String,
    lease_id: String,
    window_label: String,
}

struct PendingUtoolsAiFunctionCall {
    request_id: String,
    plugin_id: String,
    lease_id: String,
    name: String,
    response: SyncSender<Result<Value, String>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UtoolsToolCatalogEntry {
    plugin_id: String,
    plugin_name: String,
    name: String,
    description: String,
    input_schema: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_schema: Option<Value>,
    registered: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UtoolsToolInvocationResult {
    request_id: String,
    result: Value,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UtoolsToolProgressEvent {
    request_id: String,
    plugin_id: String,
    name: String,
    progress: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

const PLUGIN_SEARCH_TIMEOUT: Duration = Duration::from_millis(280);
const MAX_PENDING_PLUGIN_SEARCHES: usize = 24;
const MAX_PLUGIN_SEARCH_RESULTS: usize = 6;
const MAX_PLUGIN_SEARCH_QUERY_BYTES: usize = 512;
const MAX_PLUGIN_SEARCH_TEXT_CHARS: usize = 320;
const MAX_PLUGIN_SEARCH_PAYLOAD_BYTES: usize = 8 * 1024;
const PLUGIN_SEARCH_SELECTION_TTL: Duration = Duration::from_secs(60);
const MAX_ISSUED_PLUGIN_SEARCHES: usize = 64;
const UTOOLS_MAIN_PUSH_SELECTION_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_PENDING_UTOOLS_MAIN_PUSH_SELECTIONS: usize = 16;
const MAX_REGISTERED_UTOOLS_TOOLS: usize = 512;
const MAX_PENDING_UTOOLS_TOOL_CALLS: usize = 16;
const MAX_PENDING_UTOOLS_TOOL_CALLS_PER_PLUGIN: usize = 4;
const MAX_UTOOLS_TOOL_VALUE_BYTES: usize = 1024 * 1024;
const MAX_UTOOLS_TOOL_VALUE_NODES: usize = 16_384;
const MAX_UTOOLS_TOOL_VALUE_DEPTH: usize = 32;
const UTOOLS_TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MAX_ACTIVE_UTOOLS_AI_REQUESTS: usize = 8;
const MAX_ACTIVE_UTOOLS_AI_REQUESTS_PER_PLUGIN: usize = 2;
const MAX_UTOOLS_AI_ROUNDS: usize = 8;
const MAX_UTOOLS_AI_FUNCTION_CALLS: usize = 16;
const UTOOLS_AI_FUNCTION_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const FILESYSTEM_GRANT_TTL: Duration = Duration::from_secs(15 * 60);
const BATCH_RENAME_PREVIEW_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_FILESYSTEM_GRANTS: usize = 48;
const MAX_PLUGIN_FILE_GRANTS: usize = 48;
const MAX_PLUGIN_FILES_PER_GRANT: usize = 24;
const MAX_PLUGIN_BATCH_RENAME_PREVIEWS: usize = 48;
const MAX_CLIPBOARD_FILE_ITEMS: usize = 32;
/// A plugin may inspect only a small, already opt-in slice of iHub's
/// host-owned clipboard history. The bridge never enables capture, polls the
/// OS clipboard, or exposes mutation methods for that shared store.
const MAX_PLUGIN_CLIPBOARD_HISTORY_ITEMS: usize = 36;
const MAX_PASTED_IMAGE_EDGE: usize = 8_192;
const MAX_PASTED_IMAGE_PIXELS: usize = 12_000_000;
const MAX_PASTED_IMAGE_RAW_BYTES: usize = 48 * 1024 * 1024;
const MAX_PASTED_IMAGE_PNG_BYTES: usize = 12 * 1024 * 1024;
const MAX_UTOOLS_COPY_IMAGE_SOURCE_BYTES: usize = 4 * 1024 * 1024;
const UTOOLS_COPY_IMAGE_DATA_URL_PREFIX: &str = "data:image/png;base64,";
const MAX_UTOOLS_COPY_FILE_ITEMS: usize = 16;
const MAX_UTOOLS_COPY_FILE_PATH_CHARS: usize = 1_024;
const MAX_UTOOLS_COPY_FILE_PATH_BYTES: usize = 8 * 1024;
const MAX_UTOOLS_DRAG_GRANTS_PER_LEASE: usize = 64;
const MAX_UTOOLS_SAVE_GRANTS_PER_LEASE: usize = 32;
const MAX_UTOOLS_SHARP_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_UTOOLS_SHARP_OUTPUT_BYTES: usize = 24 * 1024 * 1024;
const MAX_UTOOLS_FFMPEG_ARGS: usize = 256;
const MAX_UTOOLS_FFMPEG_ARG_BYTES: usize = 8 * 1024;
const MAX_UTOOLS_FFMPEG_TOTAL_ARG_BYTES: usize = 64 * 1024;
const MAX_UTOOLS_FFMPEG_OUTPUT_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Launcher context is intentionally shorter than a filesystem picker grant:
/// it exists only to bridge one already-chosen action while a frontend loads.
const LAUNCHER_CONTEXT_TTL: Duration = Duration::from_secs(60);
const MAX_LAUNCHER_CONTEXT_TRANSFERS: usize = 32;
const MAX_LAUNCHER_CONTEXT_TEXT_BYTES: usize = 64 * 1024;
const MAX_LAUNCHER_CONTEXT_FILES: usize = 16;
const MAX_LAUNCHER_CONTEXT_PATH_BYTES: usize = 32 * 1024;
const MAX_LAUNCHER_CONTEXT_IMAGE_NAME_CHARS: usize = 255;
/// `getDisplayMedia` must have enough time for a deliberate screen choice,
/// but a renderer crash must not leave the resident surface permanently
/// immune to focus-loss dismissal.
const CAPTURE_FOCUS_LEASE_TTL: Duration = Duration::from_secs(90);
const MAX_CAPTURE_FOCUS_LEASES: usize = 4;
/// Plugins never choose this delay or submit cursor coordinates. It gives a
/// user a short, predictable move-away countdown and prevents a bridge call
/// from becoming a general sampling primitive.
const CURSOR_COLOR_SAMPLE_DELAY_MS: u64 = 2_000;
const CURSOR_COLOR_SAMPLE_COOLDOWN: Duration = Duration::from_secs(3);
const MAX_CURSOR_COLOR_SAMPLE_PLUGINS: usize = 32;
const CURSOR_COLOR_APPROVAL_TTL: Duration = Duration::from_secs(5);
const MAX_CURSOR_COLOR_APPROVALS: usize = 16;
const MAX_PLUGIN_LOG_LEVEL_BYTES: usize = 16;
const MAX_PLUGIN_LOG_MESSAGE_BYTES: usize = 8 * 1024;
const MAX_PLUGIN_LOGS_PER_WINDOW: usize = 32;
const MAX_PLUGIN_LOG_WINDOWS: usize = 128;
const PLUGIN_LOG_WINDOW: Duration = Duration::from_secs(10);
const PLUGIN_LOG_WINDOW_RETENTION: Duration = Duration::from_secs(5 * 60);
const MAX_PLUGIN_NOTIFICATION_TITLE_CHARS: usize = 120;
const MAX_PLUGIN_NOTIFICATION_BODY_CHARS: usize = 1_000;
const MAX_PLUGIN_NOTIFICATIONS_PER_WINDOW: usize = 5;
const MAX_PLUGIN_NOTIFICATION_WINDOWS: usize = 128;
const PLUGIN_NOTIFICATION_WINDOW: Duration = Duration::from_secs(10);
const PLUGIN_NOTIFICATION_WINDOW_RETENTION: Duration = Duration::from_secs(5 * 60);
const MAX_UTOOLS_DIALOG_SELECTIONS: usize = 64;
const MAX_PLUGIN_CLIPBOARD_TEXT_BYTES: usize = 48 * 1024;
const MAX_UTOOLS_TYPED_TEXT_CHARS: usize = 4_096;

enum UtoolsInputAction {
    PasteClipboard,
    TypeString(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum UtoolsSimulationAction {
    KeyboardTap {
        key: u16,
        key_label: String,
        modifiers: Vec<u16>,
        modifier_labels: Vec<String>,
    },
    MouseMove {
        x: i32,
        y: i32,
    },
    MouseClick {
        x: i32,
        y: i32,
        button: UtoolsMouseButton,
        double: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UtoolsMouseButton {
    Left,
    Right,
}

/// The plugin-facing projection deliberately strips the cursor coordinates
/// from the trusted Toolbox result. A plugin receives a color value only;
/// it never receives a screen position, screenshot, display identity, or
/// persistent sampling handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginCursorColor {
    hex: String,
    rgb: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginCursorColorApproval {
    pub approval_id: String,
}

impl From<crate::native_color_picker::CursorColorSample> for PluginCursorColor {
    fn from(sample: crate::native_color_picker::CursorColorSample) -> Self {
        Self {
            hex: sample.hex,
            rgb: sample.rgb,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginHostRequest {
    plugin_id: String,
    /// Host-owned iframe session. The parent sets this from the source lease;
    /// it is never read from the plugin's postMessage payload.
    lease_id: String,
    /// The React host, not the iframe payload, sets this according to whether
    /// it mounted a visible plugin surface or a hidden search runtime. Cursor
    /// sampling is rejected for the latter so providers cannot sample while
    /// the plugin is not open to the user.
    #[serde(default)]
    surface: bool,
    /// Optional one-shot interaction issued by the trusted launcher. The
    /// iframe can echo it only after receiving the corresponding main-push
    /// selection event; native code binds it to that exact active lease.
    #[serde(default)]
    interaction_id: Option<String>,
    method: String,
    #[serde(default)]
    params: Value,
}

/// The host receives the iframe bridge envelope as `plugin_host_call({ request })`.
#[derive(Debug, Deserialize)]
pub struct PluginHostCall {
    request: PluginHostRequest,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum DetachedPluginFrontendEventRequest {
    Command {
        plugin_id: String,
        command_id: String,
    },
    SearchSelection {
        plugin_id: String,
        provider_id: String,
        request_id: String,
        result_id: String,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RegisteredPluginSearchProvider {
    plugin_id: String,
    provider_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UtoolsMainPushSelectionResult {
    show: bool,
    opened_detached: bool,
    command_id: String,
    action: Value,
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
    let mut results = state.index.search(&query, limit);
    for result in &mut results {
        if result.pin_eligible {
            result.pinned_shortcut_id = state.launcher_shortcuts.shortcut_id_for_source(&result.id);
        }
    }
    results
}

fn validate_system_icon_request(
    search_result_ids: &[String],
    launcher_shortcut_ids: &[String],
) -> Result<(), String> {
    let target_count = search_result_ids.len() + launcher_shortcut_ids.len();
    if target_count > MAX_SYSTEM_ICON_TARGETS {
        return Err(format!(
            "一次最多读取 {MAX_SYSTEM_ICON_TARGETS} 个系统图标。"
        ));
    }
    let total_bytes = search_result_ids
        .iter()
        .chain(launcher_shortcut_ids)
        .map(String::len)
        .sum::<usize>();
    if total_bytes > MAX_SYSTEM_ICON_REQUEST_BYTES {
        return Err("系统图标请求标识过长。".to_owned());
    }

    let mut seen = HashSet::with_capacity(target_count);
    for source_id in search_result_ids {
        if source_id.is_empty() || source_id.len() > MAX_SYSTEM_ICON_SEARCH_ID_BYTES {
            return Err("系统图标搜索结果标识无效。".to_owned());
        }
        if !seen.insert(source_id.as_str()) {
            return Err("系统图标请求包含重复标识。".to_owned());
        }
    }
    for shortcut_id in launcher_shortcut_ids {
        if shortcut_id.is_empty() || shortcut_id.len() > MAX_SYSTEM_ICON_SHORTCUT_ID_BYTES {
            return Err("系统图标固定项标识无效。".to_owned());
        }
        if !seen.insert(shortcut_id.as_str()) {
            return Err("系统图标请求包含重复标识。".to_owned());
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn get_system_icons(
    search_result_ids: Vec<String>,
    launcher_shortcut_ids: Vec<String>,
    state: State<'_, AppState>,
) -> Result<HashMap<String, String>, String> {
    validate_system_icon_request(&search_result_ids, &launcher_shortcut_ids)?;
    let index = state.index.clone();
    let shortcuts = state.launcher_shortcuts.clone();

    tauri::async_runtime::spawn_blocking(move || {
        let mut sources = index.resolve_system_icon_sources(&search_result_ids);
        sources.extend(shortcuts.resolve_system_icon_sources(&launcher_shortcut_ids, &index));
        if sources.is_empty() {
            return HashMap::new();
        }
        let service = NativeIconService::shared();
        // Submit the whole bounded batch before waiting so one healthy STA
        // worker can process cold icons continuously instead of alternating
        // renderer round-trips with Shell calls.
        let pending = sources
            .into_iter()
            .filter_map(|source| {
                let kind = source.kind;
                service
                    .try_request_prepared(source.prepared, &kind)
                    .map(|request| (source.response_id, request))
            })
            .collect::<Vec<_>>();
        let deadline = Instant::now() + Duration::from_millis(2_500);
        let mut icons = HashMap::with_capacity(pending.len());
        for (response_id, request) in pending {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            if let Some(icon_src) = request.wait_timeout(remaining.min(Duration::from_millis(650)))
            {
                icons.insert(response_id, icon_src);
            }
        }
        icons
    })
    .await
    .map_err(|error| format!("系统图标后台任务未完成：{error}"))
}

#[tauri::command]
pub fn index_default_roots(state: State<'_, AppState>) -> IndexStatus {
    host_log::info("index", "Default-root index rebuild was requested.");
    state.index.rebuild_default_roots()
}

#[tauri::command]
pub fn set_index_roots(
    roots: Vec<String>,
    directory_open_ids: Vec<String>,
    state: State<'_, AppState>,
) -> Result<IndexStatus, String> {
    let current_roots = state.index.active_root_paths();
    let authorized = authorize_index_root_update(
        &current_roots,
        &roots,
        &directory_open_ids,
        &state.temporary_path_opens,
    )?;
    host_log::info(
        "index",
        format!(
            "Custom-root index rebuild was requested for {} root(s).",
            roots.len()
        ),
    );
    let AuthorizedIndexRootUpdate {
        roots: normalized_roots,
        guards,
    } = authorized;
    let result = state.index.set_roots(normalized_roots);
    drop(guards);
    result
}

#[tauri::command]
pub fn get_default_roots() -> Vec<String> {
    default_root_strings()
}

#[tauri::command]
pub async fn open_granted_path(open_id: String, state: State<'_, AppState>) -> Result<(), String> {
    let temporary_path_opens = state.temporary_path_opens.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let prepared = temporary_path_opens.prepare_open(&open_id)?;
        prepared.launch()
    })
    .await
    .map_err(|error| format!("Could not start the authorized system opener task: {error}"))?
}

/// Opens only an exact, current native index result. Renderer-provided paths
/// are deliberately ignored so stale or modified result objects cannot widen
/// the active local-search authorization boundary.
#[tauri::command]
pub async fn open_search_result(
    search_result_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let index = state.index.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let prepared = resolve_current_search_result_open_target(&search_result_id, &index)?;
        prepared.launch()
    })
    .await
    .map_err(|error| format!("Could not open the indexed search result: {error}"))?
}

fn validate_local_search_selection(search_result_ids: &[String]) -> Result<(), String> {
    if search_result_ids.is_empty() {
        return Err("请先选择至少一个本地搜索结果。".to_owned());
    }
    if search_result_ids.len() > MAX_LOCAL_SEARCH_SELECTION {
        return Err(format!(
            "一次最多复制 {MAX_LOCAL_SEARCH_SELECTION} 个本地搜索结果。"
        ));
    }

    let mut seen = HashSet::with_capacity(search_result_ids.len());
    let mut total_bytes = 0usize;
    for search_result_id in search_result_ids {
        if search_result_id.is_empty() || search_result_id.len() > MAX_SYSTEM_ICON_SEARCH_ID_BYTES {
            return Err("本地搜索结果标识无效。请重新搜索。".to_owned());
        }
        total_bytes = total_bytes
            .checked_add(search_result_id.len())
            .ok_or_else(|| "本地搜索选择过大。".to_owned())?;
        if total_bytes > MAX_SYSTEM_ICON_REQUEST_BYTES {
            return Err("本地搜索选择标识过长。".to_owned());
        }
        if !seen.insert(search_result_id.as_str()) {
            return Err("本地搜索选择包含重复项目。".to_owned());
        }
    }
    Ok(())
}

/// Copies only current, host-owned index results as a native file-list
/// clipboard payload. The renderer supplies opaque result IDs; every path is
/// resolved again and remains guarded until the clipboard operation ends.
#[tauri::command]
pub async fn copy_search_results_to_clipboard(
    search_result_ids: Vec<String>,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    validate_local_search_selection(&search_result_ids)?;
    let index = state.index.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let prepared = search_result_ids
            .iter()
            .map(|search_result_id| {
                resolve_current_search_result_open_target(search_result_id, &index)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let paths = prepared
            .iter()
            .map(|target| target.path().to_path_buf())
            .collect::<Vec<_>>();
        crate::clipboard_access::with_clipboard(|clipboard| clipboard.set().file_list(&paths))
            .map_err(|error| format!("无法把所选文件复制到系统剪贴板：{error}"))?;
        Ok(paths.len())
    })
    .await
    .map_err(|error| format!("本地搜索复制任务未完成：{error}"))?
}

/// Returns opaque launcher shortcut views only. The host-private source path
/// and source ID remain in app data and never enter renderer storage.
#[tauri::command]
pub async fn list_launcher_shortcuts(
    state: State<'_, AppState>,
) -> Result<Vec<LauncherShortcutView>, String> {
    let shortcuts = state.launcher_shortcuts.clone();
    let index = state.index.clone();
    tauri::async_runtime::spawn_blocking(move || Ok(shortcuts.list(&index)))
        .await
        .map_err(|error| format!("Could not list launcher shortcuts: {error}"))?
}

/// Pins only a current native search result. `search_id` is an opaque index
/// lookup key, never a path supplied by the renderer.
#[tauri::command]
pub async fn pin_launcher_shortcut_from_search(
    search_id: String,
    state: State<'_, AppState>,
) -> Result<LauncherShortcutView, String> {
    let shortcuts = state.launcher_shortcuts.clone();
    let index = state.index.clone();
    tauri::async_runtime::spawn_blocking(move || shortcuts.pin_from_search(&search_id, &index))
        .await
        .map_err(|error| format!("Could not pin launcher shortcut: {error}"))?
}

/// Opens a previously pinned target only after the native store resolves its
/// opaque ID through the current index and revalidates the live object.
#[tauri::command]
pub async fn open_launcher_shortcut(
    shortcut_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let shortcuts = state.launcher_shortcuts.clone();
    let index = state.index.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let prepared = shortcuts.resolve_open_target(&shortcut_id, &index)?;
        prepared.launch()
    })
    .await
    .map_err(|error| format!("Could not open launcher shortcut: {error}"))?
}

/// Removes only the host-owned launch record, never its target.
#[tauri::command]
pub async fn unpin_launcher_shortcut(
    shortcut_id: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let shortcuts = state.launcher_shortcuts.clone();
    tauri::async_runtime::spawn_blocking(move || shortcuts.unpin(&shortcut_id))
        .await
        .map_err(|error| format!("Could not remove launcher shortcut: {error}"))?
}

#[tauri::command]
pub fn list_plugins(state: State<'_, AppState>) -> Vec<PluginInfo> {
    let mut plugins = state.plugins.list();
    project_utools_dynamic_features(&state.plugins, &state.plugin_settings, &mut plugins);
    state.project_plugin_shortcut_statuses(&mut plugins);
    plugins
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UtoolsTextCommandMatch {
    plugin_id: String,
    command_id: String,
    label: String,
    matcher_type: String,
    payload: String,
}

fn utools_text_matcher_accepts(
    matcher: &crate::models::UtoolsTextMatcherInfo,
    query: &str,
    character_count: usize,
) -> Result<bool, String> {
    if matcher
        .min_length
        .is_some_and(|minimum| character_count < minimum)
        || matcher
            .max_length
            .is_some_and(|maximum| character_count > maximum)
    {
        return Ok(false);
    }
    let pattern_matched = match matcher.pattern.as_deref() {
        Some(pattern) => RegexBuilder::new(pattern)
            .case_insensitive(matcher.flags.contains('i'))
            .multi_line(matcher.flags.contains('m'))
            .dot_matches_new_line(matcher.flags.contains('s'))
            .unicode(true)
            .build()
            .map_err(|error| format!("Stored uTools matcher is invalid: {error}"))?
            .is_match(query),
        None => false,
    };
    Ok(match matcher.matcher_type.as_str() {
        "regex" => pattern_matched,
        "over" => !pattern_matched,
        _ => false,
    })
}

#[tauri::command]
pub fn match_utools_text_commands(
    query: String,
    state: State<'_, AppState>,
) -> Result<Vec<UtoolsTextCommandMatch>, String> {
    let character_count = query.chars().count();
    if query.is_empty()
        || character_count > 10_000
        || query.len() > 48 * 1024
        || query.contains('\0')
    {
        return Ok(Vec::new());
    }
    let mut plugins = state.plugins.list();
    project_utools_dynamic_features(&state.plugins, &state.plugin_settings, &mut plugins);
    let mut matches = Vec::new();
    for plugin in plugins.into_iter().filter(|plugin| plugin.enabled) {
        for command in plugin.commands {
            for matcher in command.utools_text_matchers {
                if utools_text_matcher_accepts(&matcher, &query, character_count)? {
                    matches.push(UtoolsTextCommandMatch {
                        plugin_id: plugin.id.clone(),
                        command_id: command.id.clone(),
                        label: matcher.label,
                        matcher_type: matcher.matcher_type,
                        payload: query.clone(),
                    });
                    if matches.len() >= 12 {
                        return Ok(matches);
                    }
                }
            }
        }
    }
    Ok(matches)
}

#[tauri::command]
pub fn set_plugin_command_shortcut(
    app: AppHandle,
    plugin_id: String,
    command_id: String,
    accelerator: Option<String>,
    auto_copy: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.plugins.ensure_plugin_enabled(&plugin_id)?;
    let mut plugins = state.plugins.list();
    project_utools_dynamic_features(&state.plugins, &state.plugin_settings, &mut plugins);
    let plugin = plugins
        .iter()
        .find(|plugin| plugin.id == plugin_id)
        .ok_or_else(|| "找不到要设置快捷键的插件。".to_owned())?;
    if !plugin
        .commands
        .iter()
        .any(|command| command.id == command_id)
    {
        return Err("找不到要设置快捷键的插件指令。".to_owned());
    }
    state.plugin_shortcut_preferences.set(
        &plugin_id,
        &command_id,
        accelerator.as_deref(),
        auto_copy,
    )?;
    refresh_plugin_shortcuts(&app);
    Ok(())
}

#[tauri::command]
pub fn reset_plugin_command_shortcut(
    app: AppHandle,
    plugin_id: String,
    command_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.plugins.ensure_plugin_enabled(&plugin_id)?;
    let mut plugins = state.plugins.list();
    project_utools_dynamic_features(&state.plugins, &state.plugin_settings, &mut plugins);
    if !plugins.iter().any(|plugin| {
        plugin.id == plugin_id
            && plugin
                .commands
                .iter()
                .any(|command| command.id == command_id)
    }) {
        return Err("找不到要恢复快捷键的插件指令。".to_owned());
    }
    state
        .plugin_shortcut_preferences
        .reset(&plugin_id, &command_id)?;
    refresh_plugin_shortcuts(&app);
    Ok(())
}

fn populate_utools_runtime_system_config(
    app: &AppHandle,
    settings: &PluginSettingsStore,
    config: &mut UtoolsCompatRuntimeConfig,
) -> Result<(), String> {
    let generated_native_id = format!("ihub-{}", Uuid::new_v4().simple());
    let native_id = settings.get_or_insert(
        &config.plugin_id,
        UTOOLS_NATIVE_ID_SETTING_KEY,
        Value::String(generated_native_id.clone()),
    )?;
    config.native_id = native_id
        .as_str()
        .filter(|value| {
            value.len() == 37
                && value.starts_with("ihub-")
                && value[5..].bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        .map(str::to_owned)
        .unwrap_or_else(|| generated_native_id.clone());
    if native_id.as_str() != Some(config.native_id.as_str()) {
        settings.set(
            &config.plugin_id,
            UTOOLS_NATIVE_ID_SETTING_KEY,
            Value::String(config.native_id.clone()),
        )?;
    }

    let resolver = app.path();
    let mut paths = BTreeMap::new();
    let mut insert = |name: &str, path: Result<PathBuf, tauri::Error>| {
        if let Ok(path) = path {
            paths.insert(name.to_owned(), renderer_display_path(&path));
        }
    };
    insert("home", resolver.home_dir());
    insert("appData", resolver.data_dir());
    insert("userData", resolver.app_data_dir());
    insert("temp", resolver.temp_dir());
    insert("desktop", resolver.desktop_dir());
    insert("documents", resolver.document_dir());
    insert("downloads", resolver.download_dir());
    insert("music", resolver.audio_dir());
    insert("pictures", resolver.picture_dir());
    insert("videos", resolver.video_dir());
    insert("logs", resolver.app_log_dir());
    if let Ok(path) = std::env::current_exe() {
        paths.insert("exe".to_owned(), renderer_display_path(&path));
    }
    config.paths = paths;
    config.idle_ubrowsers = app
        .try_state::<UtoolsUBrowserRegistry>()
        .map(|registry| registry.idle_instances(app, &config.plugin_id))
        .unwrap_or_default();
    Ok(())
}

#[tauri::command]
pub async fn get_plugin_frontend_url(
    plugin_id: String,
    purpose: Option<PluginFrontendPurpose>,
    window: tauri::WebviewWindow,
    detached: State<'_, DetachedPluginWindowRegistry>,
    browser_windows: State<'_, UtoolsBrowserWindowRegistry>,
    state: State<'_, AppState>,
) -> Result<PluginFrontendLease, String> {
    if !is_plugin_id(&plugin_id) {
        return Err("Invalid plugin ID.".to_owned());
    }
    let caller_label = window.label().to_owned();
    let purpose = purpose.unwrap_or(PluginFrontendPurpose::Surface);
    if purpose == PluginFrontendPurpose::Browser {
        return Err(
            "BrowserWindow leases are issued only to a host-registered auxiliary window."
                .to_owned(),
        );
    }
    let detached_caller = caller_label != "main";
    if detached_caller {
        if caller_label.starts_with(UTOOLS_BROWSER_WINDOW_PREFIX) {
            return Err(
                "A uTools BrowserWindow cannot request a primary plugin frontend lease.".to_owned(),
            );
        }
        detached.validate_window_plugin(&caller_label, &plugin_id)?;
        if purpose != PluginFrontendPurpose::Surface {
            return Err(
                "A detached plugin window can request only its visible surface lease.".to_owned(),
            );
        }
    } else if detached.plugin_is_detached(&plugin_id) {
        // One plugin runtime owns one set of command/provider registrations.
        // A launcher surface or hidden search runtime must not silently
        // replace the loopback lease of an already detached window.
        return Err(format!(
            "Plugin '{plugin_id}' is already open in its detached window."
        ));
    }

    for label in browser_windows.labels_owned_by_plugin(&plugin_id) {
        if let Some(child) = window.app_handle().get_webview_window(&label) {
            let _ = child.close();
        }
    }

    let plugins = state.plugins.clone();
    let plugin_assets = state.plugin_assets.clone();
    let host = state.host.clone();
    let app = window.app_handle().clone();
    let plugin_settings = state.plugin_settings.clone();
    let utools_documents = state.utools_documents.clone();
    let lease_plugin_id = plugin_id.clone();
    let lease = tauri::async_runtime::spawn_blocking(move || {
        let server = plugin_assets.clone();
        plugin_assets.with_plugin_operation(&lease_plugin_id, || {
            let mut bundle = plugins.frontend_asset_bundle(&lease_plugin_id)?;
            if let Some(config) = bundle.utools_compat.as_mut() {
                populate_utools_runtime_system_config(&app, &plugin_settings, config)?;
            }
            let resolved_plugin_id = bundle.plugin_id.clone();
            let sync_database = bundle
                .utools_compat
                .as_ref()
                .map(|_| utools_documents.clone());
            let lease = server.issue_with_utools_documents(bundle, purpose, sync_database)?;
            // A visible surface and hidden search runtime hand off ownership
            // for one plugin. A fresh lease therefore starts with no stale
            // command/provider/grant state from a prior document.
            clear_plugin_runtime_state(&host, &resolved_plugin_id);
            Ok::<PluginFrontendLease, String>(lease)
        })
    })
    .await
    .map_err(|error| format!("Plugin frontend bundle task failed: {error}"))??;

    // Lease issuance atomically cleared the prior runtime registrations.
    // Project that native state change before the replacement iframe can
    // register its declarations, including when ownership moves to/from a
    // detached window.
    emit_plugin_search_providers_changed(window.app_handle(), &plugin_id, None, false);
    if detached_caller {
        if let Err(error) = detached.bind_lease(&caller_label, &plugin_id, &lease.lease_id) {
            if let Some(released_plugin_id) = release_plugin_frontend_lease(&lease.lease_id, &state)
            {
                emit_plugin_search_providers_changed(
                    window.app_handle(),
                    &released_plugin_id,
                    None,
                    false,
                );
            }
            return Err(error);
        }
    }
    Ok(lease)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UtoolsBrowserWindowBootstrap {
    browser_id: String,
    plugin: PluginInfo,
    lease: PluginFrontendLease,
}

/// Issues the auxiliary loopback document only to the registry-owned native
/// child window that is asking. Its opaque query ID is never authoritative.
#[tauri::command]
pub async fn get_utools_browser_window_bootstrap(
    app: AppHandle,
    window: tauri::WebviewWindow,
    browser_windows: State<'_, UtoolsBrowserWindowRegistry>,
    state: State<'_, AppState>,
) -> Result<UtoolsBrowserWindowBootstrap, String> {
    if !window.label().starts_with(UTOOLS_BROWSER_WINDOW_PREFIX) {
        return Err(
            "Only a registered uTools BrowserWindow can request this bootstrap.".to_owned(),
        );
    }
    let record = browser_windows.bootstrap_for_window(window.label())?;
    let plugins = state.plugins.clone();
    let plugin_assets = state.plugin_assets.clone();
    let plugin_settings = state.plugin_settings.clone();
    let utools_documents = state.utools_documents.clone();
    let plugin_id = record.plugin_id.clone();
    let plugin = state
        .plugins
        .list()
        .into_iter()
        .find(|plugin| plugin.id == record.plugin_id && plugin.enabled)
        .ok_or_else(|| format!("Plugin '{}' is not available.", record.plugin_id))?;
    let relative_url = record.relative_url.clone();
    let preload = record.preload.clone();
    let parent_lease_id = record.parent_lease_id.clone();
    let asset_server = plugin_assets.clone();
    let mut lease = tauri::async_runtime::spawn_blocking(move || {
        plugin_assets.with_plugin_operation(&plugin_id, || {
            if !asset_server.is_active_surface_for(&parent_lease_id, &plugin_id) {
                return Err(
                    "The parent uTools plugin surface closed before its BrowserWindow loaded."
                        .to_owned(),
                );
            }
            let (mut bundle, suffix) = plugins.browser_frontend_asset_bundle(
                &plugin_id,
                &relative_url,
                preload.as_deref(),
            )?;
            if let Some(config) = bundle.utools_compat.as_mut() {
                populate_utools_runtime_system_config(&app, &plugin_settings, config)?;
            }
            let mut lease = plugin_assets.issue_with_utools_documents(
                bundle,
                PluginFrontendPurpose::Browser,
                Some(utools_documents),
            )?;
            lease.url.push_str(&suffix);
            Ok::<PluginFrontendLease, String>(lease)
        })
    })
    .await
    .map_err(|error| format!("uTools BrowserWindow bundle task failed: {error}"))??;
    let previous_lease =
        match browser_windows.bind_lease(window.label(), &record.browser_id, &lease.lease_id) {
            Ok(previous) => previous,
            Err(error) => {
                let _ = state.plugin_assets.release(&lease.lease_id);
                return Err(error);
            }
        };
    if let Some(previous_lease) = previous_lease {
        let _ = state.plugin_assets.release(&previous_lease);
    }
    if !browser_windows
        .parent_session_for_child(window.label())
        .is_some_and(|(plugin_id, parent_lease_id)| {
            state
                .plugin_assets
                .is_active_surface_for(&parent_lease_id, &plugin_id)
        })
    {
        browser_windows.unbind_owned_lease(window.label(), &lease.lease_id);
        let _ = state.plugin_assets.release(&lease.lease_id);
        let _ = window.close();
        return Err(
            "The parent uTools plugin surface closed during BrowserWindow startup.".to_owned(),
        );
    }
    // Keep the mutable binding local above so suffix projection cannot be
    // mistaken for an additional unverified asset-server path.
    lease.allows_display_capture = false;
    lease.allows_microphone = false;
    Ok(UtoolsBrowserWindowBootstrap {
        browser_id: record.browser_id,
        plugin,
        lease,
    })
}

#[tauri::command]
pub fn mark_utools_browser_window_ready(
    lease_id: String,
    window: tauri::WebviewWindow,
    browser_windows: State<'_, UtoolsBrowserWindowRegistry>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if !browser_windows.owns_lease(window.label(), &lease_id) {
        return Err("This uTools BrowserWindow ready signal does not own its lease.".to_owned());
    }
    let (browser_id, plugin_id, parent_label) = browser_windows.parent_for_child(window.label())?;
    if !state
        .plugin_assets
        .is_active_browser_for(&lease_id, &plugin_id)
    {
        return Err("This uTools BrowserWindow frontend lease has expired.".to_owned());
    }
    let parent_is_active = browser_windows
        .parent_session_for_child(window.label())
        .is_some_and(|(plugin_id, parent_lease_id)| {
            state
                .plugin_assets
                .is_active_surface_for(&parent_lease_id, &plugin_id)
        });
    if !parent_is_active {
        let _ = window.close();
        return Err("The parent uTools plugin surface is no longer active.".to_owned());
    }
    window
        .app_handle()
        .emit_to(
            &parent_label,
            "ihub://utools-browser/ready",
            json!({ "browserId": browser_id }),
        )
        .map_err(|error| format!("Could not announce uTools BrowserWindow readiness: {error}"))
}

/// Stages one launcher-context transfer after the trusted iHub parent has
/// received a real user choice (for example, choosing “Translate with X” for
/// the current text). The iframe cannot call this command: it only receives
/// the opaque context ID after the parent dispatches the declared frontend
/// command through `invoke_plugin_frontend_command`.
///
/// This API deliberately accepts only bounded text, canonicalized file/folder
/// metadata without paths, and an opaque image handle. It does not read the
/// OS clipboard, turn file metadata into a filesystem grant, or retain image
/// bytes. Callers must not invoke it while merely rendering suggestions.
#[tauri::command]
pub fn issue_plugin_launcher_context(
    plugin_id: String,
    command_id: String,
    context: PluginLauncherContextRequest,
    // The exact visible iframe lease that finished `lifecycle.ready` for
    // this user-confirmed command. A trusted parent must prove the surface
    // is still current before any payload is staged.
    frontend_lease_id: String,
    state: State<'_, AppState>,
) -> Result<PluginLauncherContextIssue, String> {
    if !is_plugin_id(&plugin_id) || !is_plugin_id(&command_id) {
        return Err("Invalid plugin or command ID.".to_owned());
    }
    let plugin_assets = state.plugin_assets.clone();
    plugin_assets.with_plugin_operation(&plugin_id, || {
        if !plugin_assets.is_active_surface_for(&frontend_lease_id, &plugin_id) {
            return Err("The plugin surface changed before this launcher context was issued. Choose the action again.".to_owned());
        }
        state
            .plugins
            .ensure_frontend_command(&plugin_id, &command_id)?;
        let payload = build_plugin_launcher_context_payload(context)?;
        let needs_text = payload.text.is_some();
        let needs_files = !payload.files.is_empty();
        let needs_image = payload.image.is_some();
        if !state
            .plugins
            .allows_launcher_context(&plugin_id, needs_text, needs_files, needs_image)?
        {
            return Err(format!(
                "Plugin '{plugin_id}' has not declared every requested launcherContext permission. Declare only the needed launcherContext.text, launcherContext.files, or launcherContext.image flags, then reinstall or update the plugin."
            ));
        }
        Ok(issue_plugin_launcher_context_transfer(
            &state.host,
            &plugin_id,
            &command_id,
            &frontend_lease_id,
            payload,
        ))
    })
}

/// Removes one unconsumed launcher-context transfer after the trusted parent
/// cannot dispatch its user-confirmed command. This is intentionally a
/// cleanup primitive for the main renderer, not a plugin bridge method: a
/// loopback iframe cannot invoke Tauri IPC directly and never receives a
/// context ID before dispatch.
#[tauri::command]
pub fn revoke_plugin_launcher_context(
    plugin_id: String,
    context_id: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    if !is_plugin_id(&plugin_id) {
        return Err("Invalid plugin ID.".to_owned());
    }
    revoke_plugin_launcher_context_transfer(&state.host, &plugin_id, &context_id)
}

fn validate_plugin_renderer_lease_caller(
    window: &tauri::WebviewWindow,
    detached: &DetachedPluginWindowRegistry,
    browser_windows: &UtoolsBrowserWindowRegistry,
    plugin_id: &str,
    lease_id: &str,
) -> Result<(), String> {
    if window.label() == "main" {
        if detached.plugin_is_detached(plugin_id) {
            return Err(format!(
                "Plugin '{plugin_id}' is owned by its detached window."
            ));
        }
        return Ok(());
    }
    if window.label().starts_with(UTOOLS_BROWSER_WINDOW_PREFIX) {
        if browser_windows.owns_lease(window.label(), lease_id) {
            return Ok(());
        }
        return Err(
            "This uTools BrowserWindow lease does not belong to the calling window.".to_owned(),
        );
    }
    detached.validate_window_plugin(window.label(), plugin_id)?;
    if !detached.owns_lease(window.label(), lease_id) {
        return Err("This plugin lease does not belong to the calling window.".to_owned());
    }
    Ok(())
}

/// Creates a one-time cursor-color approval only for the trusted React host
/// after it has shown its own visible confirmation overlay. Plugin iframes are
/// remote loopback origins and cannot call this Tauri command directly; they
/// receive only the final color value, never this token.
#[tauri::command]
pub fn issue_plugin_cursor_color_approval(
    plugin_id: String,
    lease_id: String,
    window: tauri::WebviewWindow,
    detached: State<'_, DetachedPluginWindowRegistry>,
    browser_windows: State<'_, UtoolsBrowserWindowRegistry>,
    state: State<'_, AppState>,
) -> Result<PluginCursorColorApproval, String> {
    if !is_plugin_id(&plugin_id) {
        return Err("Invalid plugin ID.".to_owned());
    }
    validate_plugin_renderer_lease_caller(
        &window,
        &detached,
        &browser_windows,
        &plugin_id,
        &lease_id,
    )?;
    let plugin_assets = state.plugin_assets.clone();
    plugin_assets.with_plugin_bridge_operation(&plugin_id, || {
        if !plugin_assets.is_active_surface_for(&lease_id, &plugin_id) {
            return Err(
                "Cursor color sampling must be confirmed from the plugin's visible active surface."
                    .to_owned(),
            );
        }
        state.plugins.ensure_plugin_enabled(&plugin_id)?;
        if !state
            .plugins
            .allows_host_method(&plugin_id, "cursorColor.sampleOnce")?
        {
            return Err(format!(
                "Plugin '{plugin_id}' is not allowed to request a cursor color. Declare cursorColor: true in its v1 plugin manifest, then reinstall or update the plugin."
            ));
        }
        Ok(PluginCursorColorApproval {
            approval_id: state
                .host
                .issue_plugin_cursor_color_approval(&plugin_id, &lease_id),
        })
    })
}

/// Releases the unique loopback source issued for an iframe. The URL never
/// points at Tauri's asset protocol, so WebView2 subframe initialization cannot
/// make a plugin frontend a local Tauri IPC caller.
fn release_plugin_frontend_lease(lease_id: &str, state: &AppState) -> Option<String> {
    // Use the same transition lock as bridge calls and plugin replacement.
    // Without it, a close/reload could remove a lease between `is_active_for`
    // and a sensitive host operation that had already begun.
    let plugin_assets = state.plugin_assets.clone();
    let host = state.host.clone();
    plugin_assets.with_plugin_operation("frontend-release", || {
        let released = plugin_assets.release(lease_id)?;
        // Closing a surface is a cancellation boundary. In particular,
        // a just-dispatched launcher-context token must not remain
        // consumable while no matching iframe is alive.
        if released.purpose != PluginFrontendPurpose::Browser {
            clear_plugin_runtime_state(&host, &released.plugin_id);
        }
        Some(released.plugin_id)
    })
}

#[tauri::command]
pub fn release_plugin_frontend_url(
    lease_id: String,
    window: tauri::WebviewWindow,
    detached: State<'_, DetachedPluginWindowRegistry>,
    browser_windows: State<'_, UtoolsBrowserWindowRegistry>,
    state: State<'_, AppState>,
) {
    for label in browser_windows.labels_owned_by_parent_lease(&lease_id) {
        if let Some(child) = window.app_handle().get_webview_window(&label) {
            let _ = child.close();
        }
    }
    if window.label() != "main" && window.label().starts_with(UTOOLS_BROWSER_WINDOW_PREFIX) {
        if !browser_windows.unbind_owned_lease(window.label(), &lease_id) {
            return;
        }
    } else if window.label() != "main" && !detached.unbind_owned_lease(window.label(), &lease_id) {
        return;
    }
    if let Some(plugin_id) = release_plugin_frontend_lease(&lease_id, &state) {
        emit_plugin_search_providers_changed(window.app_handle(), &plugin_id, None, false);
    }
}

fn close_utools_browser_windows_for_plugin(app: &AppHandle, plugin_id: &str) {
    let registry = app.state::<UtoolsBrowserWindowRegistry>();
    for label in registry.labels_owned_by_plugin(plugin_id) {
        if let Some(window) = app.get_webview_window(&label) {
            let _ = window.close();
        }
    }
    app.state::<UtoolsUBrowserRegistry>()
        .close_plugin_windows(app, plugin_id);
}

/// Renews a renderer-owned frontend lease. The main React host sends a small
/// heartbeat while its iframe exists so a crashed/reloaded renderer cannot
/// permanently consume a loopback listener.
#[tauri::command]
pub fn touch_plugin_frontend_lease(
    lease_id: String,
    window: tauri::WebviewWindow,
    detached: State<'_, DetachedPluginWindowRegistry>,
    browser_windows: State<'_, UtoolsBrowserWindowRegistry>,
    state: State<'_, AppState>,
) -> bool {
    if window.label() != "main" {
        let owns = if window.label().starts_with(UTOOLS_BROWSER_WINDOW_PREFIX) {
            let owns = browser_windows.owns_lease(window.label(), &lease_id);
            let parent_is_active = browser_windows
                .parent_session_for_child(window.label())
                .is_some_and(|(plugin_id, parent_lease_id)| {
                    state
                        .plugin_assets
                        .is_active_surface_for(&parent_lease_id, &plugin_id)
                });
            if owns && !parent_is_active {
                let _ = window.close();
            }
            owns && parent_is_active
        } else {
            detached.owns_lease(window.label(), &lease_id)
        };
        if !owns {
            return false;
        }
    }
    state.plugin_assets.touch(&lease_id)
}

#[tauri::command]
pub async fn install_plugin_from_git(
    source: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<PluginInfo, String> {
    let plugins = state.plugins.clone();
    let plugin_assets = state.plugin_assets.clone();
    let host = state.host.clone();
    let plugin_result = tauri::async_runtime::spawn_blocking(move || {
        let server = plugin_assets.clone();
        plugin_assets.with_plugin_source_operation(|| {
            let plugin = plugins.install_from_git(&source)?;
            server.revoke_plugin(&plugin.id);
            clear_plugin_runtime_state(&host, &plugin.id);
            clear_plugin_session_secrets(&host, &plugin.id);
            Ok(plugin)
        })
    })
    .await
    .map_err(|error| format!("Plugin installation task failed: {error}"))
    .and_then(|result| result);
    let mut plugin = plugin_result.map_err(|error| {
        host_log::warn(
            "plugins",
            format!("Managed plugin installation failed: {error}"),
        );
        error
    })?;
    close_utools_browser_windows_for_plugin(&app, &plugin.id);
    refresh_plugin_shortcuts(&app);
    state.project_plugin_shortcut_statuses(std::slice::from_mut(&mut plugin));
    host_log::info(
        "plugins",
        format!("Installed managed plugin '{}'.", plugin.id),
    );
    Ok(plugin)
}

/// Resolves an installed Git plugin's saved source/ref without changing its
/// files or source lock. The frontend uses this read-only result to let the
/// user decide whether to apply a replacement snapshot.
#[tauri::command]
pub async fn check_plugin_update(
    plugin_id: String,
    state: State<'_, AppState>,
) -> Result<PluginUpdateCheck, String> {
    let plugins = state.plugins.clone();
    tauri::async_runtime::spawn_blocking(move || plugins.check_git_update(&plugin_id))
        .await
        .map_err(|error| format!("Plugin update check task failed: {error}"))?
}

/// Bounded background discovery for trusted, stable plugins that explicitly
/// opted into automatic checks. The manager only resolves remote refs here;
/// it never downloads a candidate snapshot or applies a replacement. The UI
/// continues to require an explicit confirmation for every Git update.
#[tauri::command]
pub async fn check_automatic_plugin_updates(
    state: State<'_, AppState>,
) -> Result<PluginAutomaticUpdateReport, String> {
    let plugins = state.plugins.clone();
    tauri::async_runtime::spawn_blocking(move || plugins.check_automatic_updates())
        .await
        .map_err(|error| format!("Automatic plugin update check task failed: {error}"))
}

/// Applies a Git plugin replacement only after an explicit UI action. The
/// manager resolves the persisted source/ref again rather than trusting a
/// previously displayed commit, then stages and validates files without
/// launching any plugin code.
#[tauri::command]
pub async fn update_plugin_from_git(
    plugin_id: String,
    expected_commit: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<PluginUpdateResult, String> {
    let plugins = state.plugins.clone();
    let plugin_assets = state.plugin_assets.clone();
    let host = state.host.clone();

    // Reserve the known plugin before the long Git operation begins. Existing
    // documents stay alive, but no new native worker can start against a
    // source that may be atomically replaced moments later.
    let transition_server = plugin_assets.clone();
    plugin_assets.with_plugin_operation(&plugin_id, || {
        transition_server.ensure_no_native_commands()?;
        transition_server.begin_plugin_transition(&plugin_id);
        Ok::<(), String>(())
    })?;

    // Git resolution, staging, manifest validation, integrity capture and the
    // security-declaration comparison can take seconds. Keep the currently
    // installed iframe lease, search registrations and session-only secrets
    // alive until the manager has atomically replaced a candidate that passed
    // every one of those checks. A failed/moved/no-op update therefore leaves
    // the existing plugin session entirely intact.
    let update_plugin_id = plugin_id.clone();
    let update_expected_commit = expected_commit.clone();
    let update_result = tauri::async_runtime::spawn_blocking(move || {
        plugins.update_from_git(&update_plugin_id, &update_expected_commit)
    })
    .await;
    let update = match update_result {
        Ok(Ok(update)) => update,
        Ok(Err(error)) => {
            let finish_server = plugin_assets.clone();
            plugin_assets.with_plugin_operation(&plugin_id, || {
                finish_server.finish_plugin_transition(&plugin_id);
            });
            host_log::warn(
                "plugins",
                format!("Plugin '{plugin_id}' update failed: {error}"),
            );
            return Err(error);
        }
        Err(error) => {
            let finish_server = plugin_assets.clone();
            plugin_assets.with_plugin_operation(&plugin_id, || {
                finish_server.finish_plugin_transition(&plugin_id);
            });
            host_log::error(
                "plugins",
                format!("Plugin '{plugin_id}' update task failed: {error}"),
            );
            return Err(format!("Plugin update task failed: {error}"));
        }
    };

    // Once (and only once) the atomic replacement has succeeded, make the
    // source/session switch under the short exclusive operation lock. The
    // transition flag blocks a racing reopen between revocation and cleanup.
    if update.updated {
        let transition_server = plugin_assets.clone();
        plugin_assets.with_plugin_operation(&plugin_id, || {
            transition_server.revoke_plugin(&plugin_id);
            clear_plugin_runtime_state(&host, &plugin_id);
            clear_plugin_session_secrets(&host, &plugin_id);
            transition_server.finish_plugin_transition(&plugin_id);
        });
    } else {
        let transition_server = plugin_assets.clone();
        plugin_assets.with_plugin_operation(&plugin_id, || {
            transition_server.finish_plugin_transition(&plugin_id);
        });
    }

    // `update_from_git` may return an unchanged result after re-resolving a
    // moving ref. In either case the caller refreshes the catalog and may
    // explicitly reopen a newly issued frontend lease.
    let mut update = update;
    if update.updated {
        close_utools_browser_windows_for_plugin(&app, &update.plugin.id);
    }
    refresh_plugin_shortcuts(&app);
    state.project_plugin_shortcut_statuses(std::slice::from_mut(&mut update.plugin));
    host_log::info(
        "plugins",
        format!(
            "Plugin '{}' update finished (changed={}).",
            update.plugin.id, update.updated
        ),
    );
    Ok(update)
}

/// Links an existing local plugin project for explicit development use. The
/// plugin stays in its original directory; iHub reads freshly built files from
/// that directory whenever the plugin frontend is opened again.
#[tauri::command]
pub async fn link_plugin_from_local(
    directory_open_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<PluginInfo, String> {
    let temporary_path_opens = state.temporary_path_opens.clone();
    let plugins = state.plugins.clone();
    let plugin_assets = state.plugin_assets.clone();
    let host = state.host.clone();
    let plugin_result = tauri::async_runtime::spawn_blocking(move || {
        let prepared = temporary_path_opens.prepare_folder(&directory_open_id)?;
        let directory = prepared.path().to_string_lossy().into_owned();
        let server = plugin_assets.clone();
        plugin_assets.with_plugin_source_operation(|| {
            let _guard = &prepared;
            let plugin = plugins.link_from_local(&directory)?;
            // The local project can intentionally shadow an installed snapshot
            // under the same ID, so invalidate any prior frontend session
            // before the new source becomes usable.
            server.revoke_plugin(&plugin.id);
            clear_plugin_runtime_state(&host, &plugin.id);
            clear_plugin_session_secrets(&host, &plugin.id);
            Ok(plugin)
        })
    })
    .await
    .map_err(|error| format!("Local plugin link task failed: {error}"))
    .and_then(|result| result);
    let mut plugin = plugin_result.map_err(|error| {
        host_log::warn("plugins", format!("Local plugin link failed: {error}"));
        error
    })?;
    close_utools_browser_windows_for_plugin(&app, &plugin.id);
    refresh_plugin_shortcuts(&app);
    state.project_plugin_shortcut_statuses(std::slice::from_mut(&mut plugin));
    host_log::info(
        "plugins",
        format!("Linked local development plugin '{}'.", plugin.id),
    );
    Ok(plugin)
}

/// Lists the fixed native allowlist of first-party projects physically
/// available in the source checkout used for this development build. This
/// includes local overrides for all official registry packages; no
/// registry/install state is changed by this probe.
#[tauri::command]
pub async fn list_official_workspace_plugins(
    state: State<'_, AppState>,
) -> Result<Vec<OfficialWorkspacePluginProject>, String> {
    let plugins = state.plugins.clone();
    tauri::async_runtime::spawn_blocking(move || plugins.official_workspace_projects())
        .await
        .map_err(|error| format!("Official workspace plugin probe failed: {error}"))
}

/// Explicitly links one allowlisted first-party project from the current
/// checkout. The renderer supplies only the project ID; Rust owns and validates
/// the corresponding path before the ordinary local-link checks run again.
#[tauri::command]
pub async fn link_official_workspace_plugin(
    plugin_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<PluginInfo, String> {
    let plugins = state.plugins.clone();
    let plugin_assets = state.plugin_assets.clone();
    let host = state.host.clone();
    let plugin_result = tauri::async_runtime::spawn_blocking(move || {
        let server = plugin_assets.clone();
        plugin_assets.with_plugin_source_operation(|| {
            let plugin = plugins.link_official_workspace_plugin(&plugin_id)?;
            server.revoke_plugin(&plugin.id);
            clear_plugin_runtime_state(&host, &plugin.id);
            clear_plugin_session_secrets(&host, &plugin.id);
            Ok(plugin)
        })
    })
    .await
    .map_err(|error| format!("Official workspace plugin link task failed: {error}"))
    .and_then(|result| result);
    let mut plugin = plugin_result.map_err(|error| {
        host_log::warn(
            "plugins",
            format!("Official workspace plugin link failed: {error}"),
        );
        error
    })?;
    close_utools_browser_windows_for_plugin(&app, &plugin.id);
    refresh_plugin_shortcuts(&app);
    state.project_plugin_shortcut_statuses(std::slice::from_mut(&mut plugin));
    host_log::info(
        "plugins",
        format!("Linked official workspace plugin '{}'.", plugin.id),
    );
    Ok(plugin)
}

/// Removes iHub's local-development link metadata without deleting or editing
/// the developer's project directory.
#[tauri::command]
pub async fn unlink_plugin_from_local(
    plugin_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let log_plugin_id = plugin_id.clone();
    let plugins = state.plugins.clone();
    let plugin_assets = state.plugin_assets.clone();
    let host = state.host.clone();
    let unlink_result = tauri::async_runtime::spawn_blocking(move || {
        let server = plugin_assets.clone();
        plugin_assets.with_plugin_source_operation(|| {
            plugins.unlink_from_local(&plugin_id)?;
            server.revoke_plugin(&plugin_id);
            clear_plugin_runtime_state(&host, &plugin_id);
            clear_plugin_session_secrets(&host, &plugin_id);
            Ok(())
        })
    })
    .await
    .map_err(|error| format!("Local plugin unlink task failed: {error}"))
    .and_then(|result| result);
    unlink_result.map_err(|error| {
        host_log::warn(
            "plugins",
            format!("Local plugin '{log_plugin_id}' unlink failed: {error}"),
        );
        error
    })?;
    close_utools_browser_windows_for_plugin(&app, &log_plugin_id);
    refresh_plugin_shortcuts(&app);
    host_log::info(
        "plugins",
        format!("Unlinked local development plugin '{log_plugin_id}'."),
    );
    Ok(())
}

/// Persists a plugin's enabled state. Disabling clears registered iframe
/// commands/search providers immediately so an already-open plugin cannot
/// continue participating until the user enables it again.
#[tauri::command]
pub async fn set_plugin_enabled(
    plugin_id: String,
    enabled: bool,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<PluginLifecycleUpdate, String> {
    let plugins = state.plugins.clone();
    let plugin_assets = state.plugin_assets.clone();
    let host = state.host.clone();
    let update_result = tauri::async_runtime::spawn_blocking(move || {
        let server = plugin_assets.clone();
        plugin_assets.with_plugin_source_operation(|| {
            let update = plugins.set_enabled(&plugin_id, enabled)?;
            if !enabled {
                server.revoke_plugin(&update.plugin.id);
                clear_plugin_runtime_state(&host, &update.plugin.id);
                clear_plugin_session_secrets(&host, &update.plugin.id);
            }
            Ok::<PluginLifecycleUpdate, String>(update)
        })
    })
    .await
    .map_err(|error| format!("Plugin lifecycle task failed: {error}"))
    .and_then(|result| result);
    let mut update = update_result.map_err(|error| {
        host_log::warn(
            "plugins",
            format!("Plugin lifecycle change failed: {error}"),
        );
        error
    })?;
    if !enabled {
        close_utools_browser_windows_for_plugin(&app, &update.plugin.id);
    }
    refresh_plugin_shortcuts(&app);
    state.project_plugin_shortcut_statuses(std::slice::from_mut(&mut update.plugin));
    let _ = app.emit(
        &format!("ihub://plugin/{}/lifecycle", update.plugin.id),
        json!({ "state": if enabled { "enabled" } else { "disabled" } }),
    );
    host_log::info(
        "plugins",
        format!(
            "Plugin '{}' was {}.",
            update.plugin.id,
            if enabled { "enabled" } else { "disabled" }
        ),
    );
    Ok(update)
}

/// Removes only a host-managed Git snapshot. Local development links are
/// intentionally rejected by the manager so this command can never delete a
/// developer's working directory.
#[tauri::command]
pub async fn uninstall_managed_plugin(
    plugin_id: String,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<PluginUninstallResult, String> {
    let plugins = state.plugins.clone();
    let plugin_assets = state.plugin_assets.clone();
    let plugin_crypto_storage = state.plugin_crypto_storage.clone();
    let plugin_settings = state.plugin_settings.clone();
    let utools_documents = state.utools_documents.clone();
    let host = state.host.clone();
    let uninstall_result = tauri::async_runtime::spawn_blocking(move || {
        let server = plugin_assets.clone();
        plugin_assets.with_plugin_source_operation(|| {
            let removed = plugins.uninstall_managed_snapshot(&plugin_id)?;
            server.revoke_plugin(&removed.plugin_id);
            clear_plugin_runtime_state(&host, &removed.plugin_id);
            clear_plugin_session_secrets(&host, &removed.plugin_id);
            // The package has already been removed at this point. A failed
            // settings cleanup must not report the whole uninstall as failed
            // or tempt the caller to retry a destructive operation; retain
            // the harmless orphaned namespace and surface it in diagnostics.
            if let Err(error) = plugin_settings.remove_plugin(&removed.plugin_id) {
                host_log::warn(
                    "plugins",
                    format!(
                        "Could not remove settings for uninstalled plugin '{}': {error}",
                        removed.plugin_id
                    ),
                );
            }
            if let Err(error) = plugin_crypto_storage.remove_plugin(&removed.plugin_id) {
                host_log::warn(
                    "plugins",
                    format!(
                        "Could not remove encrypted storage for uninstalled plugin '{}': {error}",
                        removed.plugin_id
                    ),
                );
            }
            if let Err(error) = utools_documents.remove_plugin(&removed.plugin_id) {
                host_log::warn(
                    "plugins",
                    format!(
                        "Could not remove the document database for uninstalled plugin '{}': {error}",
                        removed.plugin_id
                    ),
                );
            }
            Ok::<PluginUninstallResult, String>(removed)
        })
    })
    .await
    .map_err(|error| format!("Plugin uninstall task failed: {error}"))
    .and_then(|result| result);
    let removed = uninstall_result.map_err(|error| {
        host_log::warn(
            "plugins",
            format!("Managed plugin uninstall failed: {error}"),
        );
        error
    })?;
    close_utools_browser_windows_for_plugin(&app, &removed.plugin_id);
    refresh_plugin_shortcuts(&app);
    let _ = app.emit(
        &format!("ihub://plugin/{}/lifecycle", removed.plugin_id),
        json!({ "state": "uninstalled" }),
    );
    host_log::info(
        "plugins",
        format!("Uninstalled managed plugin '{}'.", removed.plugin_id),
    );
    Ok(removed)
}

#[tauri::command]
pub async fn create_plugin_project(
    parent_directory_open_id: String,
    plugin_id: String,
    state: State<'_, AppState>,
) -> Result<PluginProjectCreated, String> {
    let temporary_path_opens = state.temporary_path_opens.clone();
    tauri::async_runtime::spawn_blocking(move || {
        create_plugin_project_with_open_grant(
            &temporary_path_opens,
            &parent_directory_open_id,
            &plugin_id,
        )
    })
    .await
    .map_err(|error| format!("Plugin project creation task failed: {error}"))?
}

fn create_plugin_project_with_open_grant(
    temporary_path_opens: &TemporaryPathOpenStore,
    parent_directory_open_id: &str,
    plugin_id: &str,
) -> Result<PluginProjectCreated, String> {
    let prepared = temporary_path_opens.prepare_folder(parent_directory_open_id)?;
    let parent_directory = prepared.path();
    let mut project =
        create_plugin_project_template(&parent_directory.to_string_lossy(), plugin_id)?;
    let issued = temporary_path_opens.issue(Path::new(&project.project_path))?;
    if issued.kind != TemporaryPathOpenKind::Folder {
        return Err("The newly created plugin project is not a directory.".to_owned());
    }
    project.open_id = Some(issued.open_id);
    drop(prepared);
    Ok(project)
}

/// Opens a host-owned directory chooser for first-party tools. Keeping this
/// picker native avoids asking people to discover or paste opaque filesystem
/// paths just to configure an index, create a plugin project, or preview a
/// batch rename. It returns a canonical display path plus an opaque, bounded
/// folder authorization; every first-party filesystem command consumes the
/// opaque ID and revalidates the exact native selection.
#[tauri::command]
pub fn select_directory(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<SelectedDirectoryGrant>, String> {
    let Some(directory) =
        select_directory_with_native_dialog(&app, &state.host, "Choose an iHub folder")?
    else {
        return Ok(None);
    };
    let issued = state.temporary_path_opens.issue(Path::new(&directory))?;
    if issued.kind != TemporaryPathOpenKind::Folder {
        return Err("The selected filesystem target is not a directory.".to_owned());
    }
    Ok(Some(SelectedDirectoryGrant {
        path: renderer_display_path(&issued.canonical_path),
        open_id: issued.open_id,
    }))
}

#[tauri::command]
pub fn preview_batch_rename(
    directory_open_id: String,
    find: String,
    replace: String,
    use_regex: Option<bool>,
    sequence_start: Option<u32>,
    sequence_padding: Option<u8>,
    state: State<'_, AppState>,
) -> Result<crate::builtin_tools::BatchRenamePreview, String> {
    let prepared = state
        .temporary_path_opens
        .prepare_folder(&directory_open_id)?;
    let directory = prepared.path().to_string_lossy().into_owned();
    let preview = crate::builtin_tools::preview_batch_rename(
        directory,
        find,
        replace,
        use_regex,
        sequence_start,
        sequence_padding,
    )?;
    drop(prepared);
    Ok(preview)
}

#[tauri::command]
pub fn apply_batch_rename(
    directory_open_id: String,
    items: Vec<crate::builtin_tools::BatchRenameItem>,
    state: State<'_, AppState>,
) -> Result<crate::builtin_tools::BatchRenameResult, String> {
    let prepared = state
        .temporary_path_opens
        .prepare_folder(&directory_open_id)?;
    let directory = prepared.path().to_string_lossy().into_owned();
    let result = crate::builtin_tools::apply_batch_rename(directory, items)?;
    drop(prepared);
    Ok(result)
}

/// Acquires a short-lived focus lease for the browser/system picker created by
/// `getDisplayMedia`. The React host obtains it immediately before opening the
/// picker and always releases it in `finally`; the native deadline still
/// restores normal focus-loss dismissal if that renderer never returns.
#[tauri::command]
pub fn acquire_capture_focus_lease(state: State<'_, AppState>) -> String {
    state.host.acquire_capture_focus_lease()
}

/// Releases one opaque screen-capture focus lease. Unknown or already-expired
/// IDs are deliberately harmless so cancellation and cleanup can be retried.
#[tauri::command]
pub fn release_capture_focus_lease(lease_id: String, state: State<'_, AppState>) {
    state.host.release_capture_focus_lease(&lease_id);
}

/// Samples one cursor pixel after a user-provided bounded delay. The frontend
/// uses the delay for its explicit two-second move-away countdown; no global
/// shortcut, background polling, or color-history storage is involved.
#[tauri::command]
pub async fn sample_cursor_color(
    delay_ms: Option<u64>,
) -> Result<crate::native_color_picker::CursorColorSample, String> {
    let delay_ms = crate::native_color_picker::validate_cursor_color_delay(delay_ms.unwrap_or(0))?;
    tauri::async_runtime::spawn_blocking(move || {
        crate::native_color_picker::sample_cursor_color(delay_ms)
    })
    .await
    .map_err(|error| format!("Cursor color sampling task failed: {error}"))?
}

/// Starts one explicit, short-lived live magnifier session for the built-in
/// color tool. The opaque capability expires after 30 seconds and is never
/// projected into a plugin frontend.
#[tauri::command]
pub fn begin_cursor_color_picker(
) -> Result<crate::native_color_picker::CursorColorPickerSession, String> {
    crate::native_color_picker::begin_cursor_color_picker()
}

/// Reads one bounded 9×9 cursor neighborhood under the session rate limit.
/// Native code only observes pixels and current button/key state; it never
/// posts, moves, clicks, or otherwise injects input.
#[tauri::command]
pub async fn sample_cursor_color_neighborhood(
    session_id: String,
) -> Result<crate::native_color_picker::CursorColorNeighborhoodSample, String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::native_color_picker::sample_cursor_color_neighborhood(&session_id)
    })
    .await
    .map_err(|error| format!("Live cursor color sampling task failed: {error}"))?
}

/// Ends a live magnifier capability. Repeating cleanup is harmless.
#[tauri::command]
pub fn end_cursor_color_picker(session_id: String) -> Result<(), String> {
    crate::native_color_picker::end_cursor_color_picker(&session_id)
}

/// Captures exactly one requested monitor as a bounded PNG payload. The host
/// UI may call this after a direct click; it is intentionally not made
/// available to plugin bridges, timers, or background services.
#[tauri::command]
pub async fn capture_native_screenshot(
    window: tauri::WebviewWindow,
    request: Option<crate::native_screenshot::NativeScreenshotRequest>,
) -> Result<crate::native_screenshot::NativeScreenshot, String> {
    capture_native_screenshot_for_window(&window, request.unwrap_or_default()).await
}

async fn capture_native_screenshot_for_window(
    window: &tauri::WebviewWindow,
    request: crate::native_screenshot::NativeScreenshotRequest,
) -> Result<crate::native_screenshot::NativeScreenshot, String> {
    window
        .hide()
        .map_err(|error| format!("iHub could not hide its own window before capture: {error}"))?;
    let capture_result = match tauri::async_runtime::spawn_blocking(move || {
        // Let the compositor finish removing only iHub's own window. This does
        // not move the pointer or inject any desktop input.
        std::thread::sleep(std::time::Duration::from_millis(120));
        crate::native_screenshot::capture_native_screenshot(request)
    })
    .await
    {
        Ok(result) => result,
        Err(error) => Err(format!("Native screenshot task failed: {error}")),
    };

    let show_result = window
        .show()
        .and_then(|_| window.set_focus())
        .map_err(|error| format!("iHub could not restore its window after capture: {error}"));
    match (capture_result, show_result) {
        (Ok(capture), Ok(())) => Ok(capture),
        (Err(capture_error), Ok(())) => Err(capture_error),
        (Ok(_), Err(show_error)) => Err(show_error),
        (Err(capture_error), Err(show_error)) => Err(format!("{capture_error}; {show_error}")),
    }
}

/// Captures one main-display frame only after the trusted parent frame's
/// explicit screenshot confirmation. The remote loopback iframe cannot call
/// Tauri IPC, and the native reservation prevents disable/update/uninstall
/// from racing the one-shot OS read. The full frame returns only to the
/// trusted parent; the plugin receives the subsequently user-cropped PNG.
#[tauri::command]
pub async fn capture_plugin_screen_screenshot(
    plugin_id: String,
    lease_id: String,
    window: tauri::WebviewWindow,
    detached: State<'_, DetachedPluginWindowRegistry>,
    browser_windows: State<'_, UtoolsBrowserWindowRegistry>,
    state: State<'_, AppState>,
) -> Result<crate::native_screenshot::NativeScreenshot, String> {
    if !is_plugin_id(&plugin_id) {
        return Err("Invalid plugin ID.".to_owned());
    }
    validate_plugin_renderer_lease_caller(
        &window,
        &detached,
        &browser_windows,
        &plugin_id,
        &lease_id,
    )?;
    let plugin_assets = state.plugin_assets.clone();
    let reservation_server = plugin_assets.clone();
    let native_command_lease = plugin_assets.with_plugin_bridge_operation(&plugin_id, || {
        if !reservation_server.is_active_surface_for(&lease_id, &plugin_id) {
            return Err(
                "Screen capture must be confirmed from the plugin's visible active surface."
                    .to_owned(),
            );
        }
        state.plugins.ensure_plugin_enabled(&plugin_id)?;
        if !state
            .plugins
            .allows_host_method(&plugin_id, "compatibility.utools.screen.capture")?
        {
            return Err(format!(
                "Plugin '{plugin_id}' is not allowed to request screen capture."
            ));
        }
        reservation_server.begin_native_command(&plugin_id)
    })?;

    let result = capture_native_screenshot_for_window(
        &window,
        crate::native_screenshot::NativeScreenshotRequest::default(),
    )
    .await;
    drop(native_command_lease);
    result
}

/// Opens a host-owned file picker and immediately starts a bounded LAN-only
/// download server. The WebView receives names, sizes and a random share URL,
/// never local filesystem paths.
#[tauri::command]
pub fn start_lan_file_share(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<crate::lan_share::LanFileShareView>, String> {
    let mut dialog = rfd::FileDialog::new().set_title("选择要在内网分享的文件");
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&window);
    }
    let _dialog_guard = NativeDialogGuard::begin(&state.host);
    let Some(paths) = dialog.pick_files() else {
        return Ok(None);
    };
    state.lan_file_share.start(paths).map(Some)
}

#[tauri::command]
pub fn get_lan_file_share_status(
    state: State<'_, AppState>,
) -> Option<crate::lan_share::LanFileShareView> {
    state.lan_file_share.status()
}

#[tauri::command]
pub fn stop_lan_file_share(state: State<'_, AppState>) -> Result<(), String> {
    state.lan_file_share.stop()
}

#[tauri::command]
pub fn get_hosts_snapshot() -> Result<crate::hosts_manager::HostsSnapshot, String> {
    crate::hosts_manager::get_hosts_snapshot()
}

#[tauri::command]
pub fn apply_hosts_entries(
    state: State<'_, AppState>,
    expected_fingerprint: String,
    entries: Vec<crate::hosts_manager::HostsManagedEntryInput>,
) -> Result<crate::hosts_manager::HostsApplyResult, String> {
    crate::hosts_manager::apply_hosts_entries(&state.hosts_manager, expected_fingerprint, entries)
}

#[tauri::command]
pub fn restore_hosts_backup(
    state: State<'_, AppState>,
    expected_fingerprint: String,
) -> Result<crate::hosts_manager::HostsApplyResult, String> {
    crate::hosts_manager::restore_hosts_backup(&state.hosts_manager, expected_fingerprint)
}

#[tauri::command]
pub fn list_ai_provider_profiles(
    state: State<'_, AppState>,
) -> Result<Vec<AiProviderProfileView>, String> {
    state.ai_providers.list_profiles()
}

#[tauri::command]
pub fn save_ai_provider_profile(
    input: SaveAiProviderProfileInput,
    state: State<'_, AppState>,
) -> Result<AiProviderProfileView, String> {
    state.ai_providers.save_profile(input)
}

#[tauri::command]
pub fn delete_ai_provider_profile(
    profile_id: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    state.ai_providers.delete_profile(&profile_id)
}

#[tauri::command]
pub fn list_ai_models(state: State<'_, AppState>) -> Result<Vec<UtoolsAiModelView>, String> {
    state.ai_providers.list_models()
}

#[tauri::command]
pub async fn test_ai_provider_profile(
    profile_id: String,
    state: State<'_, AppState>,
) -> Result<AiProviderTestResult, String> {
    state.ai_providers.test_profile(&profile_id).await
}

/// Returns display-only profile metadata. Reading this list never touches the
/// OS credential vault, so merely opening Cloud Drive cannot trigger a
/// Keychain/Credential Manager prompt.
#[tauri::command]
pub async fn list_cloud_profiles(
    state: State<'_, AppState>,
) -> Result<Vec<crate::cloud_credentials::CloudProfileView>, String> {
    state.cloud_drive.list_profiles().await
}

/// Authenticates one explicitly entered WebDAV account. The password crosses
/// IPC only in this call; every successful follow-up uses an opaque native
/// connection ID instead.
#[tauri::command]
pub async fn connect_webdav(
    request: crate::cloud_drive::WebDavConnectRequest,
    state: State<'_, AppState>,
) -> Result<crate::cloud_drive::WebDavConnectResult, String> {
    state.cloud_drive.connect_webdav(request).await
}

/// Reconnects a saved profile by ID. Its password is read on a blocking worker
/// from the OS vault and is never returned to the renderer.
#[tauri::command]
pub async fn connect_cloud_profile(
    request: crate::cloud_drive::CloudProfileConnectRequest,
    state: State<'_, AppState>,
) -> Result<crate::cloud_drive::WebDavConnectResult, String> {
    state.cloud_drive.connect_cloud_profile(request).await
}

/// Lists a directory using a validated native session. The renderer can choose
/// a child URL but cannot replace the session root or credentials.
#[tauri::command]
pub async fn list_webdav_directory(
    request: crate::cloud_drive::WebDavListRequest,
    state: State<'_, AppState>,
) -> Result<crate::cloud_drive::WebDavDirectoryResponse, String> {
    state.cloud_drive.list_directory(request).await
}

/// Disconnecting only drops the in-memory session. A separately saved profile
/// remains available until the explicit forget command below.
#[tauri::command]
pub fn disconnect_webdav(
    request: crate::cloud_drive::WebDavDisconnectRequest,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.cloud_drive.disconnect(request)
}

/// Revokes every session for one profile, then removes its OS credential and
/// non-secret metadata after a direct user action.
#[tauri::command]
pub async fn forget_cloud_profile(
    request: crate::cloud_drive::CloudProfileForgetRequest,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state.cloud_drive.forget_profile(request).await
}

/// Downloads one file only after the trusted built-in Cloud Drive surface has
/// shown a native Save As dialog. The renderer never receives the chosen local
/// path; the cloud module streams directly into its temporary sibling file.
#[tauri::command]
pub async fn download_webdav_file(
    app: AppHandle,
    state: State<'_, AppState>,
    request: crate::cloud_drive::WebDavDownloadRequest,
) -> Result<crate::cloud_drive::WebDavDownloadResult, String> {
    let suggested_filename = state
        .cloud_drive
        .validated_webdav_download_filename(&request)?;
    let Some(destination) = select_save_file_with_native_dialog(
        &app,
        &state.host,
        "Save iHub cloud file",
        &suggested_filename,
    ) else {
        return Ok(crate::cloud_drive::WebDavDownloadResult::cancelled());
    };
    state
        .cloud_drive
        .download_webdav_to_path(request, destination)
        .await
}

/// Uploads one explicitly picked local file into the currently visible WebDAV
/// directory. The picker path remains native-only; the renderer receives only
/// a completion summary after the cloud module has streamed and published it.
#[tauri::command]
pub async fn upload_webdav_file(
    app: AppHandle,
    state: State<'_, AppState>,
    request: crate::cloud_drive::WebDavUploadRequest,
) -> Result<crate::cloud_drive::WebDavUploadResult, String> {
    state.cloud_drive.validate_webdav_upload_request(&request)?;
    let Some(source) =
        select_upload_file_with_native_dialog(&app, &state.host, "Choose file to upload")
    else {
        return Ok(crate::cloud_drive::WebDavUploadResult::cancelled());
    };
    state
        .cloud_drive
        .upload_webdav_from_path(request, source)
        .await
}

#[tauri::command]
pub async fn run_plugin_command(
    plugin_id: String,
    command_id: String,
    input: Option<Value>,
    state: State<'_, AppState>,
) -> Result<PluginCommandResult, String> {
    if !is_plugin_id(&plugin_id) || !is_plugin_id(&command_id) {
        return Err("Invalid plugin or command ID.".to_owned());
    }
    let log_plugin_id = plugin_id.clone();
    let log_command_id = command_id.clone();
    let plugins = state.plugins.clone();
    let plugin_assets = state.plugin_assets.clone();
    let reservation_server = plugin_assets.clone();
    let native_command_lease = plugin_assets.with_plugin_bridge_operation(&plugin_id, || {
        plugins.ensure_plugin_enabled(&plugin_id)?;
        reservation_server.begin_native_command(&plugin_id)
    })?;
    let result = tauri::async_runtime::spawn_blocking(move || {
        plugins.run_command(&plugin_id, &command_id, input)
    })
    .await
    .map_err(|error| {
        host_log::error(
            "plugins",
            format!("Native command '{log_plugin_id}/{log_command_id}' task failed: {error}"),
        );
        format!("Plugin command task failed: {error}")
    })?;
    drop(native_command_lease);
    match &result {
        Ok(outcome) => host_log::info(
            "plugins",
            format!(
                "Native command '{log_plugin_id}/{log_command_id}' finished (success={}, exitCode={}).",
                outcome.success,
                outcome
                    .exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "none".to_owned())
            ),
        ),
        Err(error) => host_log::warn(
            "plugins",
            format!(
                "Native command '{log_plugin_id}/{log_command_id}' failed without recording stdout, stderr, input, or paths: {error}"
            ),
        ),
    }
    result
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
    let status = get_autostart_status(app)?;
    host_log::info(
        "lifecycle",
        if status.enabled {
            "Autostart was enabled."
        } else {
            "Autostart was disabled."
        },
    );
    Ok(status)
}

#[tauri::command]
pub fn quit_app(app: AppHandle) {
    host_log::info(
        "lifecycle",
        "User requested a full host exit; releasing resident listeners.",
    );
    if let Err(error) = app.state::<AppState>().super_panel.shutdown_listener() {
        host_log::error(
            "super-panel",
            format!("Could not stop the listener before exit: {error}"),
        );
    }
    app.exit(0);
}

#[tauri::command]
pub fn set_launcher_hotkey(
    app: AppHandle,
    state: State<'_, AppState>,
    accelerator: String,
) -> Result<LauncherHotkeyStatus, String> {
    let accelerator = normalize_launcher_hotkey(&accelerator)?;
    let status = replace_launcher_hotkey(&app, &state, accelerator.clone(), Some(accelerator))?;
    refresh_plugin_shortcuts(&app);
    host_log::info(
        "hotkey",
        format!(
            "Launcher hotkey changed to {}.",
            status.accelerator.as_deref().unwrap_or("unavailable")
        ),
    );
    Ok(status)
}

#[tauri::command]
pub fn reset_launcher_hotkey(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<LauncherHotkeyStatus, String> {
    let status = replace_launcher_hotkey(&app, &state, LAUNCHER_PRIMARY_HOTKEY.to_owned(), None)?;
    refresh_plugin_shortcuts(&app);
    host_log::info(
        "hotkey",
        format!(
            "Launcher hotkey reset to {}.",
            status.accelerator.as_deref().unwrap_or("unavailable")
        ),
    );
    Ok(status)
}

/// Replaces a launcher binding without first dropping the working one.
///
/// The new accelerator is registered and persisted before the old accelerator
/// is unregistered. Every failure before that last step rolls the candidate
/// back, so a conflict or disk error cannot make the resident app unreachable.
fn replace_launcher_hotkey(
    app: &AppHandle,
    state: &AppState,
    accelerator: String,
    preferred_accelerator: Option<String>,
) -> Result<LauncherHotkeyStatus, String> {
    let _change_guard = state
        .launcher_hotkey_change
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let current = state.launcher_hotkey_status();
    let previous_preference = state.launcher_hotkey_store.load_preference();
    let next_status = if preferred_accelerator.is_some() {
        LauncherHotkeyStatus::configured(accelerator.clone())
    } else {
        LauncherHotkeyStatus::primary()
    };

    if current.accelerator.as_deref() == Some(accelerator.as_str()) {
        persist_launcher_hotkey_preference(
            &state.launcher_hotkey_store,
            preferred_accelerator.as_deref(),
        )?;
        state.set_launcher_hotkey_status(next_status.clone());
        state.reset_launcher_hotkey_toggle();
        return Ok(next_status);
    }

    register_launcher_binding(app, &accelerator).map_err(|error| {
        format!("无法注册这个启动快捷键；它可能已被系统或其他应用占用。原快捷键保持不变。{error}")
    })?;

    if let Err(error) = persist_launcher_hotkey_preference(
        &state.launcher_hotkey_store,
        preferred_accelerator.as_deref(),
    ) {
        let cleanup = unregister_launcher_binding(app, &accelerator);
        return Err(match cleanup {
            Ok(()) => format!("无法保存启动快捷键；原快捷键保持不变。{error}"),
            Err(cleanup_error) => format!(
                "无法保存启动快捷键（{error}），且无法撤销候选快捷键（{cleanup_error}）。原快捷键仍保持注册。"
            ),
        });
    }

    if let Some(previous_accelerator) = current.accelerator.as_deref() {
        if let Err(unregister_error) = unregister_launcher_binding(app, previous_accelerator) {
            let restore = persist_launcher_hotkey_preference(
                &state.launcher_hotkey_store,
                previous_preference.as_deref(),
            );
            let cleanup = unregister_launcher_binding(app, &accelerator);
            return Err(format!(
                "新快捷键已注册，但无法安全移除原快捷键（{unregister_error}）；已停止切换。设置恢复：{}；候选撤销：{}。",
                rollback_result_label(restore),
                rollback_result_label(cleanup),
            ));
        }
    }

    state.set_launcher_hotkey_status(next_status.clone());
    state.reset_launcher_hotkey_toggle();
    Ok(next_status)
}

fn persist_launcher_hotkey_preference(
    store: &LauncherHotkeyStore,
    preferred_accelerator: Option<&str>,
) -> Result<(), String> {
    match preferred_accelerator {
        Some(accelerator) => store.save_preference(accelerator),
        None => store.clear_preference(),
    }
}

fn rollback_result_label(result: Result<(), String>) -> String {
    match result {
        Ok(()) => "成功".to_owned(),
        Err(error) => format!("失败（{error}）"),
    }
}

#[tauri::command]
pub fn get_app_health(app: AppHandle, state: State<'_, AppState>) -> AppHealth {
    AppHealth {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        platform: std::env::consts::OS.to_owned(),
        host_target: current_host_target(),
        started_at: state.started_at.clone(),
        autostart: app.autolaunch().is_enabled().unwrap_or(false),
        launcher_hotkey: state.launcher_hotkey_status(),
        index: state.index.status(),
        plugin_count: state.plugins.list().len(),
    }
}

/// Returns only the bounded, redacted host diagnostics retained by the native
/// logger. File-system locations and raw log-file bytes never cross IPC.
#[tauri::command]
pub fn get_host_log() -> Result<HostLogSnapshot, String> {
    host_log::snapshot()
}

/// Clears only iHub's fixed rotating diagnostics files. The command is
/// available to the trusted main window but intentionally absent from the
/// detached-plugin capability.
#[tauri::command]
pub fn clear_host_log() -> Result<HostLogSnapshot, String> {
    host_log::clear()
}

/// Produces the registry target spelling from Rust's OS and architecture names.
/// Unknown combinations remain explicit so catalog metadata can fail closed.
fn current_host_target() -> String {
    normalized_host_target(std::env::consts::OS, std::env::consts::ARCH)
}

fn normalized_host_target(os: &str, arch: &str) -> String {
    match (os, arch) {
        ("windows", "x86_64") => "windows-x86_64".to_owned(),
        ("windows", "aarch64") => "windows-aarch64".to_owned(),
        ("macos", "x86_64") => "darwin-x86_64".to_owned(),
        ("macos", "aarch64") => "darwin-aarch64".to_owned(),
        _ => format!("{os}-{arch}"),
    }
}

/// Keeps renderer-triggered panel resizes visually centered without exposing
/// arbitrary window movement. The only permitted operation is the same
/// work-area-aware center action used by the constrained plugin bridge.
#[tauri::command]
pub fn center_launcher_window(
    app: AppHandle,
) -> Result<crate::window_management::WindowManagementResult, String> {
    crate::window_management::manage_launcher_window(&app, "center")
}

#[tauri::command]
pub fn get_clipboard_history(
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> ClipboardHistorySnapshot {
    state.clipboard_history.snapshot(limit)
}

#[tauri::command]
pub fn set_clipboard_history_enabled(
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<ClipboardHistorySnapshot, String> {
    state.clipboard_history.set_enabled(enabled)
}

/// Image and file-list history are separate, default-off consents. The host
/// never treats this setting as authorization to read raw file contents.
#[tauri::command]
pub fn set_clipboard_history_capture_options(
    image_history_enabled: bool,
    file_history_enabled: bool,
    state: State<'_, AppState>,
) -> Result<ClipboardHistorySnapshot, String> {
    state
        .clipboard_history
        .set_capture_options(image_history_enabled, file_history_enabled)
}

#[tauri::command]
pub fn copy_clipboard_history_item(
    id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    state.clipboard_history.copy_to_system_clipboard(&id)
}

/// Restores a text/image/file-list history record only after an explicit UI
/// action. File-list restoration performs native canonical-path and
/// fingerprint revalidation immediately before writing to the clipboard.
#[tauri::command]
pub fn restore_clipboard_history_item(
    id: String,
    state: State<'_, AppState>,
) -> Result<ClipboardHistoryRestoreResult, String> {
    state.clipboard_history.restore_to_system_clipboard(&id)
}

/// Reads a bounded local PNG only when a person explicitly requests an image
/// preview. Clipboard polling itself never sends pixels across Tauri IPC.
#[tauri::command]
pub fn get_clipboard_history_image_preview(
    id: String,
    state: State<'_, AppState>,
) -> Result<ClipboardImage, String> {
    state.clipboard_history.image_preview(&id)
}

/// Opens one explicitly selected file/folder history entry only after the
/// native store resolves its opaque entry and revalidates the live object.
/// The stored path never reaches the WebView.
#[tauri::command]
pub async fn open_clipboard_history_file_entry(
    id: String,
    file_index: usize,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let history = state.clipboard_history.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let prepared = history.prepare_file_entry_open(&id, file_index)?;
        prepared.launch()
    })
    .await
    .map_err(|error| format!("Could not start the clipboard history opener task: {error}"))?
}

#[tauri::command]
pub fn set_clipboard_history_item_pinned(
    id: String,
    pinned: bool,
    state: State<'_, AppState>,
) -> Result<ClipboardHistorySnapshot, String> {
    state.clipboard_history.set_pinned(&id, pinned)
}

#[tauri::command]
pub fn delete_clipboard_history_item(
    id: String,
    state: State<'_, AppState>,
) -> Result<ClipboardHistorySnapshot, String> {
    state.clipboard_history.delete(&id)
}

#[tauri::command]
pub fn clear_unpinned_clipboard_history(
    state: State<'_, AppState>,
) -> Result<ClipboardHistorySnapshot, String> {
    state.clipboard_history.clear_unpinned()
}

/// Builds the deliberately narrow plugin-facing clipboard-history snapshot.
/// `ClipboardHistory::text_snapshot` only clones the host's existing opt-in
/// text state; it never calls a clipboard backend, exposes image/file entries,
/// changes the enabled flag, or writes the history file.
fn plugin_clipboard_history_snapshot(history: &ClipboardHistory) -> ClipboardHistorySnapshot {
    history.text_snapshot(Some(MAX_PLUGIN_CLIPBOARD_HISTORY_ITEMS))
}

#[tauri::command]
pub fn get_super_panel_status(state: State<'_, AppState>) -> SuperPanelStatus {
    state.super_panel.status()
}

#[tauri::command]
pub fn set_super_panel_enabled(
    enabled: bool,
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<SuperPanelStatus, String> {
    // Persist and project the opt-in before touching the OS listener. This
    // makes an unwritable preference a hard no-op instead of an orphaned hook.
    state.super_panel.set_enabled_persisted(enabled)?;
    if enabled {
        ensure_super_panel_listener(&app)?;
    }
    let status = state.super_panel.status();
    host_log::info(
        "super-panel",
        if status.enabled && status.listener_running {
            "Super Panel was enabled and its listener is running."
        } else {
            "Super Panel was disabled and its listener was stopped."
        },
    );
    Ok(status)
}

/// Consumes exactly one recent, host-issued long-right-click token and then
/// snapshots only the current clipboard payload. This deliberately does not
/// inject Ctrl/Cmd+C or inspect another application's selection. File paths
/// are canonicalized metadata, images are bounded/re-encoded, and text is
/// truncated before it can cross IPC.
#[tauri::command]
pub fn consume_super_panel_context(
    context_token: String,
    state: State<'_, AppState>,
) -> Result<SuperPanelContextPayload, String> {
    state.super_panel.consume_context(&context_token)?;
    host_log::debug(
        "super-panel",
        "A one-shot context was consumed; clipboard content and paths were not logged.",
    );

    if let Ok(paths) =
        crate::clipboard_access::with_clipboard(|clipboard| clipboard.get().file_list())
    {
        let files = clipboard_files_from_paths(&state.temporary_path_opens, paths);
        if !files.is_empty() {
            return Ok(SuperPanelContextPayload::Files { files });
        }
    }

    if let Ok(image) = crate::clipboard_access::with_clipboard(|clipboard| clipboard.get_image()) {
        if let Ok(image) = clipboard_image_from_rgba(image) {
            return Ok(SuperPanelContextPayload::Image { image });
        }
    }

    if let Ok(text) = crate::clipboard_access::with_clipboard(|clipboard| clipboard.get_text()) {
        let text = truncate_utf8_bytes(text, MAX_SUPER_PANEL_TEXT_BYTES);
        if !text.trim().is_empty() {
            return Ok(SuperPanelContextPayload::Text { text });
        }
    }

    Ok(SuperPanelContextPayload::Empty)
}

fn truncate_utf8_bytes(mut value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value;
    }
    let mut boundary = max_bytes.min(value.len());
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    value
}

/// Reads only native clipboard file-list metadata after the user explicitly
/// pastes a file payload into the launcher. Text and image clipboard contents
/// stay in the renderer's standard paste flow; no background clipboard scan
/// is introduced by this command.
#[tauri::command]
pub fn read_clipboard_files(state: State<'_, AppState>) -> Result<Vec<ClipboardFile>, String> {
    let paths = crate::clipboard_access::with_clipboard(|clipboard| clipboard.get().file_list())
        .map_err(|error| format!("The clipboard does not contain a readable file list: {error}"))?;
    Ok(clipboard_files_from_paths(
        &state.temporary_path_opens,
        paths,
    ))
}

/// Reads one bitmap only after the user explicitly pastes an image into the
/// launcher. Clipboard pixels are kept in memory, validated, re-encoded to a
/// bounded PNG, and passed directly to the renderer. They are not added to
/// clipboard history or written to the filesystem.
#[tauri::command]
pub fn read_clipboard_image() -> Result<Option<ClipboardImage>, String> {
    let image = match crate::clipboard_access::with_clipboard(|clipboard| clipboard.get_image()) {
        Ok(image) => image,
        Err(arboard::Error::ContentNotAvailable) => return Ok(None),
        Err(error) => {
            return Err(format!(
                "The clipboard image could not be read from the operating system: {error}"
            ));
        }
    };

    clipboard_image_from_rgba(image).map(Some)
}

fn clipboard_files_from_paths(
    temporary_path_opens: &TemporaryPathOpenStore,
    paths: impl IntoIterator<Item = PathBuf>,
) -> Vec<ClipboardFile> {
    paths
        .into_iter()
        .take(MAX_CLIPBOARD_FILE_ITEMS)
        .filter_map(|path| {
            let issued = temporary_path_opens.issue(&path).ok()?;
            let name = issued
                .canonical_path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .filter(|name| !name.trim().is_empty())?;
            Some(ClipboardFile {
                path: renderer_display_path(&issued.canonical_path),
                name,
                kind: issued.kind.as_str().to_owned(),
                open_id: issued.open_id,
            })
        })
        .collect()
}

fn clipboard_image_from_rgba(image: arboard::ImageData<'static>) -> Result<ClipboardImage, String> {
    if image.width == 0 || image.height == 0 {
        return Err("The pasted image has no pixels.".to_owned());
    }
    if image.width > MAX_PASTED_IMAGE_EDGE || image.height > MAX_PASTED_IMAGE_EDGE {
        return Err(format!(
            "The pasted image is larger than the {MAX_PASTED_IMAGE_EDGE}px edge limit."
        ));
    }

    let pixels = image
        .width
        .checked_mul(image.height)
        .ok_or_else(|| "The pasted image dimensions overflow the supported range.".to_owned())?;
    if pixels > MAX_PASTED_IMAGE_PIXELS {
        return Err(format!(
            "The pasted image exceeds the {} megapixel limit.",
            MAX_PASTED_IMAGE_PIXELS / 1_000_000
        ));
    }

    let expected_raw_bytes = pixels
        .checked_mul(4)
        .ok_or_else(|| "The pasted image byte size overflows the supported range.".to_owned())?;
    if expected_raw_bytes > MAX_PASTED_IMAGE_RAW_BYTES {
        return Err("The pasted image uses too much raw memory.".to_owned());
    }
    if image.bytes.len() != expected_raw_bytes {
        return Err("The pasted image has an invalid RGBA pixel buffer.".to_owned());
    }

    let width = u32::try_from(image.width)
        .map_err(|_| "The pasted image width is unsupported.".to_owned())?;
    let height = u32::try_from(image.height)
        .map_err(|_| "The pasted image height is unsupported.".to_owned())?;
    let mut png = LimitedPngBuffer::new(MAX_PASTED_IMAGE_PNG_BYTES);
    let encoding_result = PngEncoder::new(&mut png).write_image(
        image.bytes.as_ref(),
        width,
        height,
        ColorType::Rgba8.into(),
    );
    if let Err(error) = encoding_result {
        if png.limit_exceeded {
            return Err("The pasted image remains larger than the 12 MiB PNG limit.".to_owned());
        }
        return Err(format!(
            "The pasted image could not be encoded as PNG: {error}"
        ));
    }

    Ok(ClipboardImage {
        data_url: format!(
            "data:image/png;base64,{}",
            BASE64_STANDARD.encode(png.bytes)
        ),
        name: "ihub-pasted-image.png".to_owned(),
        mime_type: "image/png".to_owned(),
        width,
        height,
    })
}

fn validate_utools_copy_file_paths(params: &Value) -> Result<Vec<PathBuf>, String> {
    validate_utools_file_paths(params, "copyFile")
}

fn validate_utools_file_paths(params: &Value, api: &str) -> Result<Vec<PathBuf>, String> {
    let Some(object) = params.as_object() else {
        return Err(format!("uTools {api} parameters must be an object."));
    };
    if object.len() != 1 || !object.contains_key("paths") {
        return Err(format!("uTools {api} accepts only one paths parameter."));
    }
    let paths = object
        .get("paths")
        .and_then(Value::as_array)
        .ok_or_else(|| "uTools copyFile paths must be an array.".to_owned())?;
    if paths.is_empty() || paths.len() > MAX_UTOOLS_COPY_FILE_ITEMS {
        return Err(format!(
            "uTools {api} accepts between 1 and {MAX_UTOOLS_COPY_FILE_ITEMS} paths."
        ));
    }

    let mut total_bytes = 0_usize;
    let mut seen = HashSet::new();
    let mut validated = Vec::with_capacity(paths.len());
    for value in paths {
        let Some(path) = value.as_str() else {
            return Err(format!("Every uTools {api} path must be a string."));
        };
        total_bytes = total_bytes
            .checked_add(path.len())
            .ok_or_else(|| "uTools copyFile path bytes overflow.".to_owned())?;
        if path.is_empty()
            || path.chars().count() > MAX_UTOOLS_COPY_FILE_PATH_CHARS
            || total_bytes > MAX_UTOOLS_COPY_FILE_PATH_BYTES
            || path.chars().any(char::is_control)
        {
            return Err(format!(
                "A uTools {api} path is empty, too long, or contains controls."
            ));
        }
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err(format!("Every uTools {api} path must be absolute."));
        }
        if !seen.insert(path.clone()) {
            return Err(format!("uTools {api} does not accept duplicate paths."));
        }
        validated.push(path);
    }
    Ok(validated)
}

fn remember_utools_drag_grants(
    host: &PluginHostState,
    plugin_id: &str,
    lease_id: &str,
    paths: &[String],
    kind: LocalOpenKind,
) -> Result<(), String> {
    let mut selected = Vec::with_capacity(paths.len());
    for path in paths {
        let prepared = crate::system_open::prepare_local_open(Path::new(path), Some(kind))?;
        if prepared.path().to_string_lossy().as_ref() != path.as_str() {
            return Err(
                "A native picker result changed while its drag grant was being recorded."
                    .to_owned(),
            );
        }
        if selected
            .iter()
            .any(|grant: &UtoolsDragGrant| grant.identity == prepared.identity())
        {
            return Err(
                "The native picker returned the same local object more than once.".to_owned(),
            );
        }
        selected.push(UtoolsDragGrant {
            path: prepared.path().to_owned(),
            kind,
            identity: prepared.identity(),
        });
    }

    let mut grants = host
        .utools_drag_grants
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let lease_grants = grants
        .entry((plugin_id.to_owned(), lease_id.to_owned()))
        .or_default();
    for grant in selected {
        lease_grants.retain(|existing| existing.path != grant.path);
        lease_grants.push(grant);
    }
    if lease_grants.len() > MAX_UTOOLS_DRAG_GRANTS_PER_LEASE {
        lease_grants.drain(..lease_grants.len() - MAX_UTOOLS_DRAG_GRANTS_PER_LEASE);
    }
    Ok(())
}

fn remember_utools_save_grant(
    host: &PluginHostState,
    plugin_id: &str,
    lease_id: &str,
    path: &str,
) -> Result<(), String> {
    let target = PathBuf::from(path);
    if !target.is_absolute() || target.exists() {
        return Err(
            "uTools host-owned output requires a new file selected by showSaveDialog.".to_owned(),
        );
    }
    let file_name = target
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "The uTools save target has no file name.".to_owned())?;
    let parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| "The uTools save target has no parent directory.".to_owned())?;
    let prepared_parent =
        crate::system_open::prepare_local_open(parent, Some(LocalOpenKind::Folder))?;
    let normalized_target = prepared_parent.path().join(file_name);
    if normalized_target.to_string_lossy().as_ref() != path {
        return Err("The uTools save target changed while its grant was recorded.".to_owned());
    }
    let grant = UtoolsSaveGrant {
        path: normalized_target,
        parent: prepared_parent.path().to_owned(),
        parent_identity: prepared_parent.identity(),
    };
    let mut grants = host
        .utools_save_grants
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let lease_grants = grants
        .entry((plugin_id.to_owned(), lease_id.to_owned()))
        .or_default();
    lease_grants.retain(|existing| existing.path != grant.path);
    lease_grants.push(grant);
    if lease_grants.len() > MAX_UTOOLS_SAVE_GRANTS_PER_LEASE {
        lease_grants.drain(..lease_grants.len() - MAX_UTOOLS_SAVE_GRANTS_PER_LEASE);
    }
    Ok(())
}

fn read_authorized_utools_sharp_input(
    host: &PluginHostState,
    plugin_id: &str,
    lease_id: &str,
    path: &str,
) -> Result<Vec<u8>, String> {
    let params = json!({ "paths": [path] });
    let prepared =
        prepare_authorized_utools_picker_paths(host, plugin_id, lease_id, &params, "sharp")?;
    let item = prepared
        .first()
        .ok_or_else(|| "uTools Sharp has no authorized input file.".to_owned())?;
    if item.kind() != LocalOpenKind::File {
        return Err("uTools Sharp input must be a file.".to_owned());
    }
    let mut file = fs::File::open(item.path())
        .map_err(|error| format!("Could not open the selected Sharp input: {error}"))?;
    let length = file
        .metadata()
        .map_err(|error| format!("Could not inspect the selected Sharp input: {error}"))?
        .len();
    if length == 0 || length > MAX_UTOOLS_SHARP_SOURCE_BYTES as u64 {
        return Err("uTools Sharp input must contain 1-16 MiB.".to_owned());
    }
    let mut bytes = Vec::with_capacity(length as usize);
    Read::by_ref(&mut file)
        .take(MAX_UTOOLS_SHARP_SOURCE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Could not read the selected Sharp input: {error}"))?;
    if bytes.len() != length as usize || bytes.len() > MAX_UTOOLS_SHARP_SOURCE_BYTES {
        return Err(
            "The selected Sharp input changed or exceeded 16 MiB while reading.".to_owned(),
        );
    }
    Ok(bytes)
}

fn publish_authorized_utools_sharp_output(
    host: &PluginHostState,
    plugin_id: &str,
    lease_id: &str,
    path: &str,
    bytes: &[u8],
) -> Result<(), String> {
    if bytes.is_empty() || bytes.len() > MAX_UTOOLS_SHARP_OUTPUT_BYTES {
        return Err("uTools Sharp output is empty or exceeds 24 MiB.".to_owned());
    }
    let grant = take_utools_save_grant(host, plugin_id, lease_id, path, "Sharp")?;
    let prepared_parent =
        crate::system_open::prepare_local_open(&grant.parent, Some(LocalOpenKind::Folder))?;
    if prepared_parent.identity() != grant.parent_identity
        || prepared_parent.path() != grant.parent
        || grant.path.exists()
    {
        return Err("The uTools Sharp save target changed before publication.".to_owned());
    }
    drop(prepared_parent);
    let temporary = grant
        .parent
        .join(format!(".ihub-sharp-{}.tmp", Uuid::new_v4().simple()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("Could not create the Sharp output staging file: {error}"))?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("Could not persist the Sharp output staging file: {error}"))?;
        if grant.path.exists() {
            return Err("The Sharp output target was created by another process.".to_owned());
        }
        fs::rename(&temporary, &grant.path)
            .map_err(|error| format!("Could not publish the Sharp output file: {error}"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn take_utools_save_grant(
    host: &PluginHostState,
    plugin_id: &str,
    lease_id: &str,
    path: &str,
    api: &str,
) -> Result<UtoolsSaveGrant, String> {
    let mut grants = host
        .utools_save_grants
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let key = (plugin_id.to_owned(), lease_id.to_owned());
    let lease_grants = grants
        .get_mut(&key)
        .ok_or_else(|| format!("uTools {api} output must use showSaveDialog."))?;
    let index = lease_grants
        .iter()
        .position(|grant| grant.path.to_string_lossy().as_ref() == path)
        .ok_or_else(|| {
            format!("uTools {api} output must match the exact unused showSaveDialog path.")
        })?;
    let grant = lease_grants.remove(index);
    if lease_grants.is_empty() {
        grants.remove(&key);
    }
    Ok(grant)
}

fn parse_ffmpeg_duration(value: &str) -> Option<f64> {
    if let Ok(seconds) = value.parse::<f64>() {
        return (seconds.is_finite() && seconds > 0.0).then_some(seconds);
    }
    let parts = value
        .split(':')
        .map(str::parse::<f64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let seconds = match parts.as_slice() {
        [minutes, seconds] => minutes * 60.0 + seconds,
        [hours, minutes, seconds] => hours * 3600.0 + minutes * 60.0 + seconds,
        _ => return None,
    };
    (seconds.is_finite() && seconds > 0.0).then_some(seconds)
}

fn prepare_utools_ffmpeg_run(
    host: &PluginHostState,
    plugin_id: &str,
    lease_id: &str,
    params: &Value,
) -> Result<(String, PreparedUtoolsFfmpegRun), String> {
    validate_exact_plugin_params(params, &["requestId", "args"])?;
    let request_id = required_string(params, "requestId")?;
    let parsed = Uuid::parse_str(request_id)
        .map_err(|_| "utools.runFFmpeg requestId must be a UUID.".to_owned())?;
    if parsed.get_version() != Some(uuid::Version::Random) {
        return Err("utools.runFFmpeg requestId must be a version 4 UUID.".to_owned());
    }
    let raw_args = params
        .get("args")
        .and_then(Value::as_array)
        .ok_or_else(|| "utools.runFFmpeg args must be an array.".to_owned())?;
    if raw_args.is_empty() || raw_args.len() > MAX_UTOOLS_FFMPEG_ARGS {
        return Err(format!(
            "utools.runFFmpeg requires 1-{MAX_UTOOLS_FFMPEG_ARGS} arguments."
        ));
    }
    let mut total = 0usize;
    let mut args = Vec::with_capacity(raw_args.len());
    for value in raw_args {
        let arg = value
            .as_str()
            .ok_or_else(|| "Every utools.runFFmpeg argument must be a string.".to_owned())?;
        total = total
            .checked_add(arg.len())
            .ok_or_else(|| "utools.runFFmpeg argument bytes overflow.".to_owned())?;
        if arg.is_empty()
            || arg.len() > MAX_UTOOLS_FFMPEG_ARG_BYTES
            || total > MAX_UTOOLS_FFMPEG_TOTAL_ARG_BYTES
            || arg.chars().any(char::is_control)
        {
            return Err(
                "A utools.runFFmpeg argument is empty, too long, or contains controls.".to_owned(),
            );
        }
        let lowered = arg.to_ascii_lowercase();
        if matches!(lowered.as_str(), "-progress" | "-nostats")
            || lowered.starts_with("pipe:")
            || lowered.contains("://")
        {
            return Err(
                "utools.runFFmpeg cannot override host progress or access pipes/network URLs."
                    .to_owned(),
            );
        }
        args.push(arg.to_owned());
    }

    let output_path = args
        .last()
        .cloned()
        .ok_or_else(|| "utools.runFFmpeg has no output path.".to_owned())?;
    if !Path::new(&output_path).is_absolute() {
        return Err(
            "utools.runFFmpeg requires the final argument to be a showSaveDialog path.".to_owned(),
        );
    }

    let mut input_paths = Vec::new();
    for arg in &args[..args.len() - 1] {
        let path = Path::new(arg);
        if path.is_absolute() {
            input_paths.push(arg.clone());
        } else if arg.contains(":\\") || arg.starts_with("\\\\") {
            return Err(
                "utools.runFFmpeg does not accept embedded filesystem paths; pass exact showOpenDialog paths as separate arguments."
                    .to_owned(),
            );
        }
    }
    let inputs = if input_paths.is_empty() {
        Vec::new()
    } else {
        prepare_authorized_utools_picker_paths(
            host,
            plugin_id,
            lease_id,
            &json!({ "paths": input_paths }),
            "runFFmpeg",
        )?
    };

    let output_grant = take_utools_save_grant(host, plugin_id, lease_id, &output_path, "FFmpeg")?;
    let prepared_parent =
        crate::system_open::prepare_local_open(&output_grant.parent, Some(LocalOpenKind::Folder))?;
    if prepared_parent.path() != output_grant.parent
        || prepared_parent.identity() != output_grant.parent_identity
        || output_grant.path.exists()
    {
        return Err("The uTools FFmpeg save target changed before execution.".to_owned());
    }
    let extension = output_grant
        .path
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    let staging_output = output_grant.parent.join(format!(
        ".ihub-ffmpeg-{}{}",
        Uuid::new_v4().simple(),
        extension
    ));
    *args
        .last_mut()
        .ok_or_else(|| "utools.runFFmpeg has no output argument.".to_owned())? =
        staging_output.to_string_lossy().into_owned();
    let duration_seconds = args.windows(2).find_map(|pair| {
        matches!(pair[0].as_str(), "-t" | "-to")
            .then(|| parse_ffmpeg_duration(&pair[1]))
            .flatten()
    });
    Ok((
        request_id.to_owned(),
        PreparedUtoolsFfmpegRun {
            args,
            duration_seconds,
            output_grant,
            staging_output,
            _inputs: inputs,
        },
    ))
}

fn publish_utools_ffmpeg_output(run: &PreparedUtoolsFfmpegRun) -> Result<(), String> {
    let metadata = fs::symlink_metadata(&run.staging_output)
        .map_err(|error| format!("Could not inspect the FFmpeg output: {error}"))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > MAX_UTOOLS_FFMPEG_OUTPUT_BYTES
    {
        return Err("uTools FFmpeg produced an empty, unsafe, or oversized output.".to_owned());
    }
    let parent = crate::system_open::prepare_local_open(
        &run.output_grant.parent,
        Some(LocalOpenKind::Folder),
    )?;
    if parent.path() != run.output_grant.parent
        || parent.identity() != run.output_grant.parent_identity
        || run.output_grant.path.exists()
    {
        return Err("The uTools FFmpeg save target changed before publication.".to_owned());
    }
    drop(parent);
    fs::rename(&run.staging_output, &run.output_grant.path)
        .map_err(|error| format!("Could not publish the FFmpeg output: {error}"))
}

fn confirm_utools_ffmpeg_install(app: &AppHandle, host: &PluginHostState, plugin_id: &str) -> bool {
    let mut dialog = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Warning)
        .set_title(format!("iHub · {plugin_id} 请求安装 FFmpeg"))
        .set_description(format!(
            "此插件需要 FFmpeg {}。iHub 将从 gyan.dev 下载约 104 MiB 的 GPLv3 静态构建，并使用内置 SHA-256 校验后安装到当前用户的应用数据目录。\n\n是否继续？",
            crate::utools_ffmpeg::FFMPEG_VERSION
        ))
        .set_buttons(rfd::MessageButtons::YesNo);
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&window);
    }
    let _guard = NativeDialogGuard::begin(host);
    dialog.show() == rfd::MessageDialogResult::Yes
}

fn prepare_authorized_utools_drag_paths(
    host: &PluginHostState,
    plugin_id: &str,
    lease_id: &str,
    params: &Value,
) -> Result<Vec<PreparedLocalOpen>, String> {
    prepare_authorized_utools_picker_paths(host, plugin_id, lease_id, params, "startDrag")
}

fn prepare_authorized_utools_picker_paths(
    host: &PluginHostState,
    plugin_id: &str,
    lease_id: &str,
    params: &Value,
    api: &str,
) -> Result<Vec<PreparedLocalOpen>, String> {
    let requested = validate_utools_file_paths(params, api)?;
    let expected = {
        let grants = host
            .utools_drag_grants
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(lease_grants) = grants.get(&(plugin_id.to_owned(), lease_id.to_owned())) else {
            return Err(format!(
                "uTools {api} accepts only paths returned by showOpenDialog to this current plugin surface."
            ));
        };
        requested
            .iter()
            .map(|path| {
                lease_grants
                    .iter()
                    .find(|grant| grant.path == *path)
                    .cloned()
                    .ok_or_else(|| format!(
                        "uTools {api} accepts only exact paths returned by showOpenDialog to this current plugin surface."
                    ))
            })
            .collect::<Result<Vec<_>, _>>()?
    };

    let mut prepared = Vec::with_capacity(expected.len());
    for grant in expected {
        let item = crate::system_open::prepare_local_open(&grant.path, Some(grant.kind))?;
        if item.path() != grant.path || item.identity() != grant.identity {
            return Err(format!(
                "A file selected for uTools {api} changed before the operation began."
            ));
        }
        if prepared
            .iter()
            .any(|existing: &PreparedLocalOpen| existing.identity() == item.identity())
        {
            return Err(format!(
                "uTools {api} targets resolve to the same local object."
            ));
        }
        prepared.push(item);
    }
    Ok(prepared)
}

fn dispatch_utools_file_drag(
    app: &AppHandle,
    window_label: String,
    prepared: Vec<PreparedLocalOpen>,
) -> Result<(), String> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let callback_app = app.clone();
    app.run_on_main_thread(move || {
        let result = callback_app
            .get_webview_window(&window_label)
            .ok_or_else(|| "The plugin window closed before its file drag began.".to_owned())
            .and_then(|window| crate::utools_drag::start_file_drag(&window, &prepared));
        let _ = sender.send(result);
    })
    .map_err(|error| format!("Could not schedule the native uTools file drag: {error}"))?;
    receiver
        .recv()
        .map_err(|_| "The native uTools file drag closed without a result.".to_owned())?
}

fn confirm_utools_copy_files(
    app: &AppHandle,
    host: &PluginHostState,
    plugin_id: &str,
    paths: &[PathBuf],
) -> bool {
    let paths = paths
        .iter()
        .map(|path| format!("• {}", renderer_display_path(path)))
        .collect::<Vec<_>>()
        .join("\n");
    let mut dialog = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Warning)
        .set_title(format!("iHub · {plugin_id} 请求复制文件"))
        .set_description(format!(
            "插件想把以下本机项目放入系统剪贴板：\n\n{paths}\n\n是否允许？"
        ))
        .set_buttons(rfd::MessageButtons::YesNo);
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&window);
    }
    let _dialog_guard = NativeDialogGuard::begin(host);
    dialog.show() == rfd::MessageDialogResult::Yes
}

fn validate_utools_shell_local_path(params: &Value, method: &str) -> Result<PathBuf, String> {
    let Some(object) = params.as_object() else {
        return Err(format!("uTools {method} parameters must be an object."));
    };
    if object.len() != 1 || !object.contains_key("path") {
        return Err(format!("uTools {method} accepts exactly one path."));
    }
    let path = required_string(params, "path")?;
    if path.is_empty()
        || path.chars().count() > MAX_UTOOLS_COPY_FILE_PATH_CHARS
        || path.len() > MAX_UTOOLS_COPY_FILE_PATH_BYTES
        || path.chars().any(char::is_control)
    {
        return Err(format!(
            "uTools {method} path is empty, too long, or contains controls."
        ));
    }
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(format!("uTools {method} requires an absolute local path."));
    }
    Ok(path)
}

fn confirm_utools_local_path_action(
    app: &AppHandle,
    host: &PluginHostState,
    plugin_id: &str,
    action: &str,
    warning: &str,
    path: &Path,
) -> bool {
    let mut dialog = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Warning)
        .set_title(format!("iHub · {plugin_id} 请求{action}"))
        .set_description(format!(
            "插件请求{warning}：\n\n{}\n\n是否允许？",
            renderer_display_path(path)
        ))
        .set_buttons(rfd::MessageButtons::YesNo);
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&window);
    }
    let _dialog_guard = NativeDialogGuard::begin(host);
    dialog.show() == rfd::MessageDialogResult::Yes
}

fn confirm_utools_foreground_read(
    app: &AppHandle,
    host: &PluginHostState,
    plugin_id: &str,
    subject: &str,
) -> bool {
    let mut dialog = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Warning)
        .set_title(format!("iHub · {plugin_id} 请求读取{subject}"))
        .set_description(format!(
            "插件请求读取打开 iHub 前最后一个活动窗口的{subject}。\n\n仅检查该窗口，不会枚举其他窗口、模拟按键或读取剪贴板。是否允许？"
        ))
        .set_buttons(rfd::MessageButtons::YesNo);
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&window);
    }
    let _dialog_guard = NativeDialogGuard::begin(host);
    dialog.show() == rfd::MessageDialogResult::Yes
}

#[cfg(target_os = "windows")]
fn resolve_utools_simulation_action(
    method: &str,
    params: &Value,
) -> Result<UtoolsSimulationAction, String> {
    use windows_sys::Win32::{Foundation::POINT, UI::WindowsAndMessaging::GetCursorPos};

    let snapshot = crate::utools_screen::screen_snapshot()?;
    let physical_display_bounds = snapshot
        .metrics
        .into_iter()
        .map(|metric| metric.physical_bounds)
        .collect::<Vec<_>>();
    let mut cursor = POINT::default();
    let current_cursor =
        (unsafe { GetCursorPos(&mut cursor) } != 0).then_some((cursor.x, cursor.y));
    validate_utools_simulation_action(method, params, &physical_display_bounds, current_cursor)
}

#[cfg(not(target_os = "windows"))]
fn resolve_utools_simulation_action(
    _method: &str,
    _params: &Value,
) -> Result<UtoolsSimulationAction, String> {
    Err("uTools input simulation has not been runtime-verified on this platform.".to_owned())
}

fn utools_simulation_description(action: &UtoolsSimulationAction) -> String {
    match action {
        UtoolsSimulationAction::KeyboardTap {
            key_label,
            modifier_labels,
            ..
        } => {
            let chord = modifier_labels
                .iter()
                .map(String::as_str)
                .chain(std::iter::once(key_label.as_str()))
                .collect::<Vec<_>>()
                .join(" + ");
            format!("模拟键盘按键：{chord}")
        }
        UtoolsSimulationAction::MouseMove { x, y } => {
            format!("把鼠标指针移动到物理屏幕坐标 ({x}, {y})")
        }
        UtoolsSimulationAction::MouseClick {
            x,
            y,
            button,
            double,
        } => {
            let action = match (button, double) {
                (UtoolsMouseButton::Left, false) => "单击鼠标左键",
                (UtoolsMouseButton::Left, true) => "双击鼠标左键",
                (UtoolsMouseButton::Right, false) => "单击鼠标右键",
                (UtoolsMouseButton::Right, true) => "双击鼠标右键",
            };
            format!("在物理屏幕坐标 ({x}, {y}) {action}")
        }
    }
}

fn confirm_utools_simulation(
    app: &AppHandle,
    host: &PluginHostState,
    plugin_id: &str,
    action: &UtoolsSimulationAction,
) -> bool {
    let mut dialog = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Warning)
        .set_title(format!("iHub · {plugin_id} 请求模拟输入"))
        .set_description(format!(
            "插件请求执行以下系统输入：\n\n{}\n\n该操作会影响当前活动应用。是否允许本次操作？",
            utools_simulation_description(action)
        ))
        .set_buttons(rfd::MessageButtons::YesNo);
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&window);
    }
    let _dialog_guard = NativeDialogGuard::begin(host);
    dialog.show() == rfd::MessageDialogResult::Yes
}

#[cfg(target_os = "windows")]
fn perform_utools_windows_simulation(action: &UtoolsSimulationAction) -> Result<(), String> {
    use windows::Win32::UI::{
        Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
            KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
            MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEINPUT, MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
        },
        WindowsAndMessaging::SetCursorPos,
    };

    fn keyboard_input(key: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
        let flags = if utools_windows_key_is_extended(key) {
            flags | KEYEVENTF_EXTENDEDKEY
        } else {
            flags
        };
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(key),
                    wScan: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn mouse_input(flags: MOUSE_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn send(inputs: &[INPUT], cleanup: &[INPUT]) -> Result<(), String> {
        let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent as usize == inputs.len() {
            return Ok(());
        }
        if !cleanup.is_empty() {
            unsafe {
                let _ = SendInput(cleanup, std::mem::size_of::<INPUT>() as i32);
            }
        }
        Err(format!(
            "Windows accepted {sent} of {} confirmed uTools simulation events.",
            inputs.len()
        ))
    }

    match action {
        UtoolsSimulationAction::KeyboardTap { key, modifiers, .. } => {
            let mut inputs = Vec::with_capacity(modifiers.len() * 2 + 2);
            for modifier in modifiers {
                inputs.push(keyboard_input(*modifier, KEYBD_EVENT_FLAGS(0)));
            }
            inputs.push(keyboard_input(*key, KEYBD_EVENT_FLAGS(0)));
            inputs.push(keyboard_input(*key, KEYEVENTF_KEYUP));
            for modifier in modifiers.iter().rev() {
                inputs.push(keyboard_input(*modifier, KEYEVENTF_KEYUP));
            }
            let mut cleanup = vec![keyboard_input(*key, KEYEVENTF_KEYUP)];
            cleanup.extend(
                modifiers
                    .iter()
                    .rev()
                    .map(|modifier| keyboard_input(*modifier, KEYEVENTF_KEYUP)),
            );
            send(&inputs, &cleanup)
        }
        UtoolsSimulationAction::MouseMove { x, y } => unsafe { SetCursorPos(*x, *y) }
            .map_err(|error| format!("Windows could not move the pointer: {error}")),
        UtoolsSimulationAction::MouseClick {
            x,
            y,
            button,
            double,
        } => {
            unsafe { SetCursorPos(*x, *y) }.map_err(|error| {
                format!("Windows could not position the pointer before clicking: {error}")
            })?;
            let (down, up) = match button {
                UtoolsMouseButton::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
                UtoolsMouseButton::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
            };
            let repeat = if *double { 2 } else { 1 };
            let mut inputs = Vec::with_capacity(repeat * 2);
            for _ in 0..repeat {
                inputs.push(mouse_input(down));
                inputs.push(mouse_input(up));
            }
            send(&inputs, &[mouse_input(up)])
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn perform_utools_windows_simulation(_action: &UtoolsSimulationAction) -> Result<(), String> {
    Err("uTools input simulation has not been runtime-verified on this platform.".to_owned())
}

fn decode_utools_clipboard_png_data_url(
    data_url: &str,
) -> Result<arboard::ImageData<'static>, String> {
    let encoded = data_url
        .strip_prefix(UTOOLS_COPY_IMAGE_DATA_URL_PREFIX)
        .ok_or_else(|| "uTools copyImage accepts only a PNG data URL or Uint8Array.".to_owned())?;
    let max_encoded_chars = MAX_UTOOLS_COPY_IMAGE_SOURCE_BYTES.div_ceil(3) * 4;
    if encoded.is_empty() || encoded.len() > max_encoded_chars {
        return Err(format!(
            "uTools copyImage PNG payloads are limited to {MAX_UTOOLS_COPY_IMAGE_SOURCE_BYTES} bytes."
        ));
    }
    let png = BASE64_STANDARD
        .decode(encoded)
        .map_err(|_| "uTools copyImage received malformed PNG base64 data.".to_owned())?;
    if png.len() > MAX_UTOOLS_COPY_IMAGE_SOURCE_BYTES || !png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("uTools copyImage received an invalid or oversized PNG.".to_owned());
    }

    decode_utools_clipboard_image_bytes(&png, ImageFormat::Png)
}

fn decode_utools_clipboard_image_bytes(
    bytes: &[u8],
    format: ImageFormat,
) -> Result<arboard::ImageData<'static>, String> {
    if bytes.is_empty() || bytes.len() > MAX_UTOOLS_COPY_IMAGE_SOURCE_BYTES {
        return Err(format!(
            "uTools image files are limited to {MAX_UTOOLS_COPY_IMAGE_SOURCE_BYTES} bytes."
        ));
    }
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP
    ) {
        return Err("uTools image files must be PNG, JPEG, or WebP.".to_owned());
    }

    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_PASTED_IMAGE_EDGE as u32);
    limits.max_image_height = Some(MAX_PASTED_IMAGE_EDGE as u32);
    // The normalized RGBA output is capped separately at 48 MiB. Give the
    // decoder a small, fixed amount of workspace in addition to that output
    // instead of inheriting image-rs's 512 MiB default.
    limits.max_alloc = Some((MAX_PASTED_IMAGE_RAW_BYTES + 16 * 1024 * 1024) as u64);
    let mut dimensions_reader = ImageReader::with_format(io::Cursor::new(bytes), format);
    dimensions_reader.limits(limits.clone());
    let (width, height) = dimensions_reader
        .into_dimensions()
        .map_err(|error| format!("uTools copyImage could not read the PNG header: {error}"))?;
    let width_usize =
        usize::try_from(width).map_err(|_| "uTools copyImage width is unsupported.".to_owned())?;
    let height_usize = usize::try_from(height)
        .map_err(|_| "uTools copyImage height is unsupported.".to_owned())?;
    let pixels = width_usize
        .checked_mul(height_usize)
        .ok_or_else(|| "uTools copyImage dimensions overflow.".to_owned())?;
    if width_usize == 0
        || height_usize == 0
        || width_usize > MAX_PASTED_IMAGE_EDGE
        || height_usize > MAX_PASTED_IMAGE_EDGE
        || pixels > MAX_PASTED_IMAGE_PIXELS
    {
        return Err("uTools copyImage dimensions exceed the host limits.".to_owned());
    }
    let expected_rgba_bytes = pixels
        .checked_mul(4)
        .ok_or_else(|| "uTools copyImage byte size overflows.".to_owned())?;
    if expected_rgba_bytes > MAX_PASTED_IMAGE_RAW_BYTES {
        return Err("uTools copyImage uses too much decoded memory.".to_owned());
    }

    let mut image_reader = ImageReader::with_format(io::Cursor::new(bytes), format);
    image_reader.limits(limits);
    let rgba = image_reader
        .decode()
        .map_err(|error| format!("uTools copyImage could not decode the PNG: {error}"))?
        .into_rgba8();
    if rgba.width() != width
        || rgba.height() != height
        || rgba.as_raw().len() != expected_rgba_bytes
    {
        return Err("uTools copyImage produced an invalid RGBA pixel buffer.".to_owned());
    }

    Ok(arboard::ImageData {
        width: width_usize,
        height: height_usize,
        bytes: std::borrow::Cow::Owned(rgba.into_raw()),
    })
}

fn decode_authorized_utools_clipboard_image(
    host: &PluginHostState,
    plugin_id: &str,
    lease_id: &str,
    params: &Value,
    api: &str,
) -> Result<arboard::ImageData<'static>, String> {
    let Some(object) = params.as_object() else {
        return Err(format!("uTools {api} parameters must be an object."));
    };
    if object.len() != 1 {
        return Err(format!(
            "uTools {api} accepts exactly one dataUrl or picker-returned path."
        ));
    }
    if let Some(data_url) = object.get("dataUrl").and_then(Value::as_str) {
        return decode_utools_clipboard_png_data_url(data_url);
    }
    let path = object.get("path").and_then(Value::as_str).ok_or_else(|| {
        format!("uTools {api} accepts exactly one dataUrl or picker-returned path.")
    })?;
    let path_params = json!({ "paths": [path] });
    let prepared =
        prepare_authorized_utools_picker_paths(host, plugin_id, lease_id, &path_params, api)?;
    let item = prepared
        .first()
        .ok_or_else(|| format!("uTools {api} requires one selected image file."))?;
    if item.kind() != LocalOpenKind::File {
        return Err(format!("uTools {api} requires a selected image file."));
    }

    let file = fs::File::open(item.path())
        .map_err(|error| format!("Could not open the selected uTools image file: {error}"))?;
    let declared_len = file
        .metadata()
        .map_err(|error| format!("Could not inspect the selected uTools image file: {error}"))?
        .len();
    if declared_len == 0 || declared_len > MAX_UTOOLS_COPY_IMAGE_SOURCE_BYTES as u64 {
        return Err(format!(
            "uTools image files are limited to {MAX_UTOOLS_COPY_IMAGE_SOURCE_BYTES} bytes."
        ));
    }
    let mut bytes = Vec::with_capacity(declared_len as usize);
    file.take(MAX_UTOOLS_COPY_IMAGE_SOURCE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Could not read the selected uTools image file: {error}"))?;
    if bytes.len() > MAX_UTOOLS_COPY_IMAGE_SOURCE_BYTES {
        return Err(format!(
            "uTools image files are limited to {MAX_UTOOLS_COPY_IMAGE_SOURCE_BYTES} bytes."
        ));
    }
    let format = image::guess_format(&bytes)
        .map_err(|error| format!("Could not identify the selected uTools image: {error}"))?;
    decode_utools_clipboard_image_bytes(&bytes, format)
}

/// Lets the PNG encoder stop before an arbitrary clipboard bitmap can retain
/// tens of megabytes of compressed output in process memory.
struct LimitedPngBuffer {
    bytes: Vec<u8>,
    limit: usize,
    limit_exceeded: bool,
}

impl LimitedPngBuffer {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            limit_exceeded: false,
        }
    }
}

impl Write for LimitedPngBuffer {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.limit_exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "encoded PNG exceeds the in-memory paste limit",
            ));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The SDK calls this command from a plugin frontend. Its values intentionally
/// stay JSON-shaped so independently developed plugins can evolve without a
/// host/SDK lock-step release.
#[tauri::command]
pub async fn plugin_host_call(
    app: AppHandle,
    window: tauri::WebviewWindow,
    request: PluginHostCall,
    detached: State<'_, DetachedPluginWindowRegistry>,
    browser_windows: State<'_, UtoolsBrowserWindowRegistry>,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    let request = request.request;
    if !is_plugin_id(&request.plugin_id) {
        return Err("Invalid plugin ID.".to_owned());
    }
    validate_plugin_renderer_lease_caller(
        &window,
        &detached,
        &browser_windows,
        &request.plugin_id,
        &request.lease_id,
    )?;
    let browser_child = window.label().starts_with(UTOOLS_BROWSER_WINDOW_PREFIX)
        && browser_windows.owns_lease(window.label(), &request.lease_id);
    if browser_child
        && !browser_windows
            .parent_session_for_child(window.label())
            .is_some_and(|(plugin_id, parent_lease_id)| {
                plugin_id == request.plugin_id
                    && state
                        .plugin_assets
                        .is_active_surface_for(&parent_lease_id, &plugin_id)
            })
    {
        let _ = window.close();
        return Err("The parent uTools plugin surface is no longer active.".to_owned());
    }
    if browser_child
        && (request.method.starts_with("commands.")
            || request.method.starts_with("search.")
            || request.method.starts_with("compatibility.utools.features.")
            || request.method.starts_with("compatibility.utools.tools.")
            || request.method.starts_with("compatibility.utools.ai.")
            || request.method.starts_with("compatibility.utools.ffmpeg.")
            || request.method.starts_with("compatibility.utools.settings.")
            || matches!(
                request.method.as_str(),
                "lifecycle.ready" | "lifecycle.dispose"
            ))
    {
        return Err(
            "A uTools BrowserWindow cannot own or mutate the primary plugin runtime lifecycle."
                .to_owned(),
        );
    }
    let request_plugin_id = request.plugin_id.clone();
    let plugin_assets = state.plugin_assets.clone();
    let server = plugin_assets.clone();

    if request.method == "compatibility.utools.browser.executeJavaScript" {
        let execution = plugin_assets.with_plugin_bridge_operation(&request_plugin_id, || {
            if !request.surface
                || !server.is_active_surface_for(&request.lease_id, &request_plugin_id)
            {
                return Err("uTools BrowserWindow script execution requires its visible active parent surface.".to_owned());
            }
            ensure_plugin_host_request_is_allowed(&request, &state)?;
            validate_exact_plugin_params(&request.params, &["browserId", "script"])?;
            let browser_id = required_string(&request.params, "browserId")?;
            let script = required_string(&request.params, "script")?;
            if script.is_empty()
                || script.chars().count() > 65_536
                || script.len() > 262_144
            {
                return Err("uTools BrowserWindow script exceeds the execution limit.".to_owned());
            }
            let execution = browser_windows.begin_execution(
                browser_id,
                &request_plugin_id,
                &request.lease_id,
            )?;
            if let Err(error) = app.emit_to(
                &execution.window_label,
                "ihub://utools-browser/execute",
                json!({ "requestId": execution.request_id, "script": script }),
            ) {
                browser_windows.cancel_execution(&execution.request_id);
                return Err(format!("Could not dispatch uTools BrowserWindow script: {error}"));
            }
            Ok(execution)
        })?;
        let request_id = execution.request_id.clone();
        let response = tauri::async_runtime::spawn_blocking(move || {
            execution.response.recv_timeout(Duration::from_secs(15))
        })
        .await
        .map_err(|error| format!("uTools BrowserWindow script wait failed: {error}"))?;
        match response {
            Ok(result) => return result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                browser_windows.cancel_execution(&request_id);
                return Err("uTools BrowserWindow script timed out after 15 seconds.".to_owned());
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                browser_windows.cancel_execution(&request_id);
                return Err("uTools BrowserWindow script channel closed unexpectedly.".to_owned());
            }
        }
    }

    if request.method == "compatibility.utools.browser.executeResult" {
        return plugin_assets.with_plugin_bridge_operation(&request_plugin_id, || {
            if !server.is_active_browser_for(&request.lease_id, &request_plugin_id)
                || !browser_windows.owns_lease(window.label(), &request.lease_id)
            {
                return Err(
                    "Only the target uTools BrowserWindow can complete its script.".to_owned(),
                );
            }
            validate_exact_plugin_params(&request.params, &["requestId", "ok", "result", "error"])?;
            let request_id = required_string(&request.params, "requestId")?;
            let ok = request
                .params
                .get("ok")
                .and_then(Value::as_bool)
                .ok_or_else(|| "uTools BrowserWindow script result requires ok.".to_owned())?;
            let response = if ok {
                let value = request.params.get("result").cloned().unwrap_or(Value::Null);
                if serde_json::to_vec(&value)
                    .map_err(|error| {
                        format!("Could not encode BrowserWindow script result: {error}")
                    })?
                    .len()
                    > 256 * 1024
                {
                    return Err("uTools BrowserWindow script result exceeds 256 KiB.".to_owned());
                }
                Ok(value)
            } else {
                let error = request
                    .params
                    .get("error")
                    .and_then(Value::as_str)
                    .filter(|error| !error.is_empty() && error.chars().count() <= 2_000)
                    .unwrap_or("uTools BrowserWindow script failed.")
                    .to_owned();
                Err(error)
            };
            browser_windows.complete_execution(window.label(), request_id, response)?;
            Ok(json!({ "completed": true }))
        });
    }

    if request.method == "compatibility.utools.browser.sendToParent" {
        return plugin_assets.with_plugin_bridge_operation(&request_plugin_id, || {
            if !server.is_active_browser_for(&request.lease_id, &request_plugin_id) {
                return Err(
                    "uTools sendToParent is available only inside its active BrowserWindow."
                        .to_owned(),
                );
            }
            ensure_plugin_host_request_is_allowed(&request, &state)?;
            validate_exact_plugin_params(&request.params, &["channel", "args"])?;
            let channel = required_string(&request.params, "channel")?;
            let args = validate_utools_browser_message(&request.params, channel)?;
            let (browser_id, plugin_id, parent_label) =
                browser_windows.parent_for_child(window.label())?;
            if plugin_id != request_plugin_id {
                return Err(
                    "This uTools BrowserWindow parent channel belongs to another plugin."
                        .to_owned(),
                );
            }
            app.emit_to(
                &parent_label,
                "ihub://utools-browser/parent-message",
                json!({ "browserId": browser_id, "channel": channel, "args": args }),
            )
            .map_err(|error| format!("Could not deliver the uTools parent message: {error}"))?;
            Ok(json!({ "sent": true }))
        });
    }

    if request.method == "compatibility.utools.browser.closeSelf" {
        return plugin_assets.with_plugin_bridge_operation(&request_plugin_id, || {
            if !server.is_active_browser_for(&request.lease_id, &request_plugin_id)
                || !browser_windows.owns_lease(window.label(), &request.lease_id)
            {
                return Err("Only an active uTools BrowserWindow can close itself.".to_owned());
            }
            validate_exact_plugin_params(&request.params, &[])?;
            window
                .close()
                .map_err(|error| format!("Could not close the uTools BrowserWindow: {error}"))?;
            Ok(json!({ "closed": true }))
        });
    }

    if request.method == "compatibility.utools.ubrowser.run" {
        let (run_request, native_lease) =
            plugin_assets.with_plugin_bridge_operation(&request_plugin_id, || {
                if !request.surface
                    || !server.is_active_surface_for(&request.lease_id, &request_plugin_id)
                {
                    return Err(
                        "uTools ubrowser chains require the plugin's visible active surface."
                            .to_owned(),
                    );
                }
                ensure_plugin_host_request_is_allowed(&request, &state)?;
                if !state
                    .plugins
                    .uses_utools_compatibility(&request_plugin_id)?
                {
                    return Err(
                        "uTools ubrowser is available only to validated imported uTools packages."
                            .to_owned(),
                    );
                }
                let run_request =
                    serde_json::from_value::<UBrowserRunRequest>(request.params.clone())
                        .map_err(|error| format!("Invalid uTools ubrowser chain: {error}"))?;
                crate::utools_ubrowser::validate_run_request(&run_request)?;
                let native_lease = server.begin_native_command(&request_plugin_id)?;
                Ok((run_request, native_lease))
            })?;
        let run_app = app.clone();
        let run_server = server.clone();
        let run_plugin_id = request_plugin_id.clone();
        let parent_lease_id = request.lease_id.clone();
        let result = tauri::async_runtime::spawn_blocking(move || {
            let registry = run_app.state::<UtoolsUBrowserRegistry>();
            run_utools_ubrowser_chain(
                &run_app,
                &registry,
                &run_server,
                &run_plugin_id,
                &parent_lease_id,
                run_request,
            )
        })
        .await
        .map_err(|error| format!("uTools ubrowser host task failed: {error}"))?;
        drop(native_lease);
        return result;
    }

    // Cursor sampling is deliberately not a normal synchronous Bridge call.
    // The trusted parent only reaches this branch after rendering its own
    // confirmation overlay and injecting a host-issued, one-time approval
    // token. Validate/consume that token while the visible lease is live, then
    // release the Bridge lock before the fixed two-second native delay.
    if request.method == "cursorColor.sampleOnce" {
        let approval_id = cursor_color_approval_id(&request.params)?.to_owned();
        let _cursor_color_native_lease = plugin_assets.with_plugin_bridge_operation(&request_plugin_id, || {
            if !request.surface || !server.is_active_surface_for(&request.lease_id, &request_plugin_id) {
                return Err(
                    "Cursor color sampling is available only from the plugin's visible active surface."
                        .to_owned(),
                );
            }
            ensure_plugin_host_request_is_allowed(&request, &state)?;
            // Treat the fixed-delay system read as a host-native operation.
            // The reservation remains alive across the await below, so a
            // plugin cannot be disabled, relinked, uninstalled, or updated
            // after consent but before the OS reads its one pixel.
            let native_lease = server.begin_native_command(&request_plugin_id)?;
            state.host.take_plugin_cursor_color_approval(
                &request_plugin_id,
                &request.lease_id,
                &approval_id,
            )?;
            state.host.reserve_plugin_cursor_color_sample(&request_plugin_id)?;
            Ok(native_lease)
        })?;

        let sample = tauri::async_runtime::spawn_blocking(|| {
            crate::native_color_picker::sample_cursor_color(CURSOR_COLOR_SAMPLE_DELAY_MS)
        })
        .await
        .map_err(|error| format!("Cursor color sampling task failed: {error}"))??;
        return serde_json::to_value(PluginCursorColor::from(sample))
            .map_err(|error| format!("Could not encode cursor color sample: {error}"));
    }

    if matches!(
        request.method.as_str(),
        "compatibility.utools.system.readCurrentFolderPath"
            | "compatibility.utools.system.readCurrentBrowserUrl"
    ) {
        let method = request.method.clone();
        let (target, native_lease) = plugin_assets.with_plugin_bridge_operation(
            &request_plugin_id,
            || {
                if !request.surface
                    || !server.is_active_surface_for(&request.lease_id, &request_plugin_id)
                {
                    return Err(
                        "Reading the preceding window is available only from the plugin's visible active surface."
                            .to_owned(),
                    );
                }
                ensure_plugin_host_request_is_allowed(&request, &state)?;
                if request
                    .params
                    .as_object()
                    .map_or(true, |params| !params.is_empty())
                {
                    return Err("uTools current-window readers do not accept parameters.".to_owned());
                }
                if !state.host.admit_plugin_notification(&request_plugin_id) {
                    return Err(format!(
                        "Interactive uTools alerts are limited to {MAX_PLUGIN_NOTIFICATIONS_PER_WINDOW} every {} seconds.",
                        PLUGIN_NOTIFICATION_WINDOW.as_secs()
                    ));
                }
                Ok((
                    state.previous_foreground()?,
                    server.begin_native_command(&request_plugin_id)?,
                ))
            },
        )?;
        let subject = if method.ends_with("FolderPath") {
            "文件管理器路径"
        } else {
            "浏览器网址"
        };
        if !confirm_utools_foreground_read(&app, &state.host, &request_plugin_id, subject) {
            return Err(format!(
                "The user declined access to the preceding {subject}."
            ));
        }
        let value = tauri::async_runtime::spawn_blocking(move || {
            if method.ends_with("FolderPath") {
                crate::utools_foreground::read_folder_path(target)
            } else {
                crate::utools_foreground::read_browser_url(target)
            }
        })
        .await
        .map_err(|error| format!("The current-window read task failed: {error}"))??;
        drop(native_lease);
        return Ok(Value::String(value));
    }

    if request.method == "compatibility.utools.window.startDrag" {
        let window_label = window.label().to_owned();
        let (prepared, native_lease) = plugin_assets.with_plugin_bridge_operation(
            &request_plugin_id,
            || {
                if !request.surface
                    || !server.is_active_surface_for(&request.lease_id, &request_plugin_id)
                {
                    return Err(
                        "uTools startDrag is available only from the plugin's visible active surface."
                            .to_owned(),
                    );
                }
                ensure_plugin_host_request_is_allowed(&request, &state)?;
                let prepared = prepare_authorized_utools_drag_paths(
                    &state.host,
                    &request_plugin_id,
                    &request.lease_id,
                    &request.params,
                )?;
                Ok((prepared, server.begin_native_command(&request_plugin_id)?))
            },
        )?;
        dispatch_utools_file_drag(&app, window_label, prepared)?;
        drop(native_lease);
        return Ok(json!({ "completed": true }));
    }

    if request.method == "compatibility.utools.settings.open" {
        let section = plugin_assets.with_plugin_bridge_operation(&request_plugin_id, || {
            if !request.surface
                || !server.is_active_surface_for(&request.lease_id, &request_plugin_id)
            {
                return Err(
                    "uTools settings navigation requires the plugin's visible active surface."
                        .to_owned(),
                );
            }
            ensure_plugin_host_request_is_allowed(&request, &state)?;
            let object = request
                .params
                .as_object()
                .ok_or_else(|| "uTools settings navigation requires an object.".to_owned())?;
            let section = required_string(&request.params, "section")?;
            match section {
                "ai" => {
                    if object.len() != 1 {
                        return Err("uTools AI settings navigation accepts no options.".to_owned());
                    }
                }
                "shortcuts" => {
                    if object
                        .keys()
                        .any(|key| !matches!(key.as_str(), "section" | "commandLabel" | "autoCopy"))
                    {
                        return Err(
                            "uTools shortcut settings navigation contains an unknown option."
                                .to_owned(),
                        );
                    }
                    let label = required_string(&request.params, "commandLabel")?;
                    if label.trim().is_empty()
                        || label != label.trim()
                        || label.chars().count() > 160
                        || label.chars().any(char::is_control)
                    {
                        return Err("uTools shortcut settings command label is invalid.".to_owned());
                    }
                    if request
                        .params
                        .get("autoCopy")
                        .and_then(Value::as_bool)
                        .is_none()
                    {
                        return Err(
                            "uTools shortcut settings autoCopy must be a boolean.".to_owned()
                        );
                    }
                }
                _ => return Err("Unknown uTools settings destination.".to_owned()),
            }
            Ok(section.to_owned())
        })?;
        show_launcher(&app);
        let mut navigation = json!({ "surface": "settings", "section": section });
        if section == "shortcuts" {
            navigation["pluginId"] = Value::String(request_plugin_id.clone());
            navigation["commandLabel"] = request.params["commandLabel"].clone();
            navigation["autoCopy"] = request.params["autoCopy"].clone();
        }
        app.emit("ihub://tray-navigation", navigation)
            .map_err(|error| format!("Could not open iHub settings: {error}"))?;
        return Ok(json!({ "opened": true }));
    }

    if request.method == "compatibility.utools.sharp.execute" {
        let (sharp_request, native_lease) =
            plugin_assets.with_plugin_bridge_operation(&request_plugin_id, || {
                if !request.surface
                    || !server.is_active_surface_for(&request.lease_id, &request_plugin_id)
                {
                    return Err(
                        "uTools Sharp requires the plugin's visible active surface.".to_owned()
                    );
                }
                ensure_plugin_host_request_is_allowed(&request, &state)?;
                let mut sharp_request =
                    serde_json::from_value::<SharpRequest>(request.params.clone())
                        .map_err(|error| format!("Invalid uTools Sharp pipeline: {error}"))?;
                if let Some(path) = sharp_request.picker_path().map(str::to_owned) {
                    let bytes = read_authorized_utools_sharp_input(
                        &state.host,
                        &request_plugin_id,
                        &request.lease_id,
                        &path,
                    )?;
                    sharp_request.replace_picker_path(&bytes)?;
                }
                Ok((
                    sharp_request,
                    server.begin_native_command(&request_plugin_id)?,
                ))
            })?;
        let execution = tauri::async_runtime::spawn_blocking(move || {
            crate::utools_sharp::execute(sharp_request)
        })
        .await
        .map_err(|error| format!("uTools Sharp host task failed: {error}"))??;
        if let Some((path, bytes)) = execution.output_file {
            publish_authorized_utools_sharp_output(
                &state.host,
                &request_plugin_id,
                &request.lease_id,
                &path,
                &bytes,
            )?;
        }
        drop(native_lease);
        return Ok(execution.response);
    }

    if request.method.starts_with("compatibility.utools.ffmpeg.") {
        if request.method == "compatibility.utools.ffmpeg.start" {
            let installed = plugin_assets.with_plugin_bridge_operation(
                &request_plugin_id,
                || -> Result<bool, String> {
                    if !request.surface
                        || !server.is_active_surface_for(&request.lease_id, &request_plugin_id)
                    {
                        return Err(
                            "uTools FFmpeg requires the plugin's visible active surface."
                                .to_owned(),
                        );
                    }
                    ensure_plugin_host_request_is_allowed(&request, &state)?;
                    if !state.plugins.uses_utools_compatibility(&request_plugin_id)? {
                        return Err(
                            "utools.runFFmpeg is available only to validated imported uTools packages."
                                .to_owned(),
                        );
                    }
                    Ok(state.ffmpeg.installed_executable()?.is_some())
                },
            )?;
            if !installed && !confirm_utools_ffmpeg_install(&app, &state.host, &request_plugin_id) {
                return Err("The user declined the managed FFmpeg installation.".to_owned());
            }
            let (request_id, prepared, native_lease) =
                plugin_assets.with_plugin_bridge_operation(&request_plugin_id, || {
                    if !request.surface
                        || !server.is_active_surface_for(&request.lease_id, &request_plugin_id)
                    {
                        return Err(
                            "uTools FFmpeg requires the plugin's visible active surface."
                                .to_owned(),
                        );
                    }
                    ensure_plugin_host_request_is_allowed(&request, &state)?;
                    let (request_id, prepared) = prepare_utools_ffmpeg_run(
                        &state.host,
                        &request_plugin_id,
                        &request.lease_id,
                        &request.params,
                    )?;
                    Ok((
                        request_id,
                        prepared,
                        server.begin_native_command(&request_plugin_id)?,
                    ))
                })?;
            return start_utools_ffmpeg_request(
                app,
                state.host.clone(),
                state.ffmpeg.clone(),
                request_plugin_id,
                request.lease_id,
                window.label().to_owned(),
                request_id,
                prepared,
                native_lease,
            );
        }
        return plugin_assets.with_plugin_bridge_operation(&request_plugin_id, || {
            if !request.surface
                || !server.is_active_surface_for(&request.lease_id, &request_plugin_id)
            {
                return Err(
                    "uTools FFmpeg controls require the plugin's visible active surface."
                        .to_owned(),
                );
            }
            ensure_plugin_host_request_is_allowed(&request, &state)?;
            match request.method.as_str() {
                "compatibility.utools.ffmpeg.quit" => control_utools_ffmpeg_request(
                    &state.host,
                    &request_plugin_id,
                    &request.lease_id,
                    &request.params,
                    false,
                ),
                "compatibility.utools.ffmpeg.kill" => control_utools_ffmpeg_request(
                    &state.host,
                    &request_plugin_id,
                    &request.lease_id,
                    &request.params,
                    true,
                ),
                _ => Err("Unsupported uTools FFmpeg method.".to_owned()),
            }
        });
    }

    // A native worker can take up to the host command deadline. Reserve it
    // while the iframe lease is still protected by the Bridge read lock, then
    // release that lock before waiting for the worker. Source/lifecycle
    // mutations see the reservation and fail promptly instead of blocking all
    // plugin transitions behind a slow OCR/FFmpeg-style command.
    if request.method == "native.runCommand" {
        let (command_id, input, native_command_lease) = plugin_assets
            .with_plugin_bridge_operation(&request_plugin_id, || {
                if !server.is_active_for(&request.lease_id, &request_plugin_id) {
                    return Err(
                        "This plugin frontend session has expired. Reopen the plugin to continue."
                            .to_owned(),
                    );
                }
                ensure_plugin_host_request_is_allowed(&request, &state)?;
                let native_command_lease = server.begin_native_command(&request_plugin_id)?;
                let (command_id, input) =
                    native_plugin_command_input(&state.host, &request_plugin_id, &request.params)?;
                Ok((command_id, input, native_command_lease))
            })?;
        let plugins = state.plugins.clone();
        let plugin_id = request_plugin_id.clone();
        let log_command_id = command_id.clone();
        let native_result = tauri::async_runtime::spawn_blocking(move || {
            plugins.run_command(&plugin_id, &command_id, Some(input))
        })
        .await
        .map_err(|error| {
            let message = format!("Native plugin command task failed: {error}");
            host_log::error(
                "plugins",
                format!(
                    "Bridge native command '{request_plugin_id}/{log_command_id}' task failed: {error}"
                ),
            );
            message
        })?;
        // Do not retain the reservation during serialization or response
        // delivery. The worker process has already exited (or errored) here.
        drop(native_command_lease);
        match &native_result {
            Ok(outcome) => host_log::info(
                "plugins",
                format!(
                    "Bridge native command '{request_plugin_id}/{log_command_id}' finished (success={}, exitCode={}).",
                    outcome.success,
                    outcome
                        .exit_code
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| "none".to_owned())
                ),
            ),
            Err(error) => host_log::warn(
                "plugins",
                format!(
                    "Bridge native command '{request_plugin_id}/{log_command_id}' failed without recording input, stdout, stderr, or paths: {error}"
                ),
            ),
        }
        return native_result.and_then(|result| {
            serde_json::to_value(result)
                .map_err(|error| format!("Could not encode native plugin command result: {error}"))
        });
    }

    if request.method == "log" {
        // Validate the exact active lease and permission under the transition
        // read lock, then release it before any disk-backed diagnostic work.
        // A log already accepted from a live document is harmless if a source
        // transition begins immediately afterward, while holding the global
        // lock across flush() would delay disable/update operations.
        plugin_assets.with_plugin_bridge_operation(&request_plugin_id, || {
            if !server.is_active_for(&request.lease_id, &request_plugin_id) {
                return Err(
                    "This plugin frontend session has expired. Reopen the plugin to continue."
                        .to_owned(),
                );
            }
            ensure_plugin_host_request_is_allowed(&request, &state)
        })?;
        return handle_plugin_log_call(&request, &state);
    }

    if request.method.starts_with("compatibility.utools.tools.") {
        return plugin_assets.with_plugin_bridge_operation(&request_plugin_id, || {
            if !server.is_active_for(&request.lease_id, &request_plugin_id) {
                return Err(
                    "This uTools MCP runtime session has expired. Reopen the plugin to continue."
                        .to_owned(),
                );
            }
            ensure_plugin_host_request_is_allowed(&request, &state)?;
            match request.method.as_str() {
                "compatibility.utools.tools.register" => register_utools_tool_handler(
                    &state.host,
                    &state.plugins,
                    &request_plugin_id,
                    &request.lease_id,
                    window.label(),
                    &request.params,
                ),
                "compatibility.utools.tools.complete" => complete_utools_tool_call(
                    &state.host,
                    &state.plugins,
                    &request_plugin_id,
                    &request.lease_id,
                    &request.params,
                ),
                "compatibility.utools.tools.progress" => progress_utools_tool_call(
                    &app,
                    &state.host,
                    &request_plugin_id,
                    &request.lease_id,
                    &request.params,
                ),
                _ => Err("Unsupported uTools MCP runtime method.".to_owned()),
            }
        });
    }

    if request.method.starts_with("compatibility.utools.ai.") {
        return plugin_assets.with_plugin_bridge_operation(&request_plugin_id, || {
            if !server.is_active_for(&request.lease_id, &request_plugin_id) {
                return Err(
                    "This uTools AI runtime session has expired. Reopen the plugin to continue."
                        .to_owned(),
                );
            }
            ensure_plugin_host_request_is_allowed(&request, &state)?;
            match request.method.as_str() {
                "compatibility.utools.ai.models" => {
                    serde_json::to_value(state.ai_providers.list_models()?)
                        .map_err(|error| format!("Could not encode AI model catalog: {error}"))
                }
                "compatibility.utools.ai.start" => start_utools_ai_request(
                    UtoolsAiStartContext {
                        app: app.clone(),
                        host: state.host.clone(),
                        plugin_assets: state.plugin_assets.clone(),
                        providers: state.ai_providers.clone(),
                        plugin_id: request_plugin_id.clone(),
                        lease_id: request.lease_id.clone(),
                        window_label: window.label().to_owned(),
                    },
                    &request.params,
                ),
                "compatibility.utools.ai.abort" => abort_utools_ai_request(
                    &state.host,
                    &request_plugin_id,
                    &request.lease_id,
                    &request.params,
                ),
                "compatibility.utools.ai.toolComplete" => complete_utools_ai_function_call(
                    &state.host,
                    &request_plugin_id,
                    &request.lease_id,
                    &request.params,
                ),
                _ => Err("Unsupported uTools AI runtime method.".to_owned()),
            }
        });
    }

    plugin_assets.with_plugin_bridge_operation(&request_plugin_id, || {
        if !server.is_active_for(&request.lease_id, &request_plugin_id) {
            return Err(
                "This plugin frontend session has expired. Reopen the plugin to continue."
                    .to_owned(),
            );
        }
        plugin_host_call_for_active_lease(&app, request, &state)
    })
}

fn emit_plugin_search_providers_changed(
    app: &AppHandle,
    plugin_id: &str,
    provider_id: Option<&str>,
    registered: bool,
) {
    let payload = plugin_search_providers_changed_payload(plugin_id, provider_id, registered);
    let _ = app.emit_to("main", "ihub://plugin-search-providers-changed", payload);
}

fn validate_utools_tool_json_value(value: &Value, label: &str) -> Result<(), String> {
    let encoded = serde_json::to_vec(value)
        .map_err(|error| format!("Could not encode uTools MCP {label}: {error}"))?;
    if encoded.len() > MAX_UTOOLS_TOOL_VALUE_BYTES {
        return Err(format!(
            "uTools MCP {label} exceeds {MAX_UTOOLS_TOOL_VALUE_BYTES} bytes."
        ));
    }
    let mut pending = vec![(value, 1usize)];
    let mut nodes = 0usize;
    while let Some((next, depth)) = pending.pop() {
        nodes = nodes.saturating_add(1);
        if nodes > MAX_UTOOLS_TOOL_VALUE_NODES || depth > MAX_UTOOLS_TOOL_VALUE_DEPTH {
            return Err(format!(
                "uTools MCP {label} exceeds the JSON complexity limit."
            ));
        }
        match next {
            Value::Array(values) => {
                pending.extend(values.iter().map(|value| (value, depth + 1)));
            }
            Value::Object(object) => {
                pending.extend(object.values().map(|value| (value, depth + 1)));
            }
            _ => {}
        }
    }
    Ok(())
}

fn register_utools_tool_handler(
    host: &PluginHostState,
    plugins: &PluginManager,
    plugin_id: &str,
    lease_id: &str,
    window_label: &str,
    params: &Value,
) -> Result<Value, String> {
    validate_exact_plugin_params(params, &["name"])?;
    let name = required_string(params, "name")?;
    plugins.utools_tool_definition(plugin_id, name)?;
    let key = (plugin_id.to_owned(), name.to_owned());
    let mut tools = host
        .utools_tools
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !tools.contains_key(&key) && tools.len() >= MAX_REGISTERED_UTOOLS_TOOLS {
        return Err(format!(
            "The host has reached its {MAX_REGISTERED_UTOOLS_TOOLS}-tool runtime limit."
        ));
    }
    tools.insert(
        key,
        RegisteredUtoolsTool {
            plugin_id: plugin_id.to_owned(),
            lease_id: lease_id.to_owned(),
            window_label: window_label.to_owned(),
        },
    );
    Ok(json!({ "registered": true }))
}

fn complete_utools_tool_call(
    host: &PluginHostState,
    plugins: &PluginManager,
    plugin_id: &str,
    lease_id: &str,
    params: &Value,
) -> Result<Value, String> {
    validate_exact_plugin_params(params, &["requestId", "name", "ok", "result", "error"])?;
    let request_id = required_string(params, "requestId")?;
    let name = required_string(params, "name")?;
    let ok = params
        .get("ok")
        .and_then(Value::as_bool)
        .ok_or_else(|| "uTools MCP completion requires a boolean ok value.".to_owned())?;
    {
        let pending = host
            .pending_utools_tool_calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let call = pending
            .get(request_id)
            .ok_or_else(|| "This uTools MCP request expired or was cancelled.".to_owned())?;
        if call.plugin_id != plugin_id || call.name != name || call.lease_id != lease_id {
            return Err("This uTools MCP completion belongs to another runtime call.".to_owned());
        }
    }

    let outcome = if ok {
        (|| {
            let result = params.get("result").cloned().unwrap_or(Value::Null);
            validate_utools_tool_json_value(&result, "tool result")?;
            let tool = plugins.utools_tool_definition(plugin_id, name)?;
            validate_utools_tool_value(&tool, &result, true)?;
            Ok(result)
        })()
    } else {
        let error = params
            .get("error")
            .and_then(Value::as_str)
            .filter(|error| !error.trim().is_empty() && error.chars().count() <= 2_000)
            .unwrap_or("uTools MCP handler failed.")
            .to_owned();
        Err(error)
    };
    let call = host
        .pending_utools_tool_calls
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(request_id)
        .ok_or_else(|| "This uTools MCP request was already completed.".to_owned())?;
    let _ = call.response.send(outcome);
    Ok(json!({ "completed": true }))
}

fn progress_utools_tool_call(
    app: &AppHandle,
    host: &PluginHostState,
    plugin_id: &str,
    lease_id: &str,
    params: &Value,
) -> Result<Value, String> {
    validate_exact_plugin_params(
        params,
        &["requestId", "name", "progress", "total", "message"],
    )?;
    let request_id = required_string(params, "requestId")?;
    let name = required_string(params, "name")?;
    let progress = params
        .get("progress")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value >= 0.0)
        .ok_or_else(|| "uTools MCP progress must be a finite non-negative number.".to_owned())?;
    let total = match params.get("total") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_f64()
                .filter(|value| value.is_finite() && *value > 0.0 && *value >= progress)
                .ok_or_else(|| {
                    "uTools MCP progress total must be finite, positive, and not below progress."
                        .to_owned()
                })?,
        ),
    };
    let message = match params.get("message") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_str()
                .filter(|value| value.chars().count() <= 1_000 && !value.contains('\0'))
                .ok_or_else(|| "uTools MCP progress message is invalid or too long.".to_owned())?
                .to_owned(),
        ),
    };
    let pending = host
        .pending_utools_tool_calls
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let call = pending
        .get(request_id)
        .ok_or_else(|| "This uTools MCP request expired or was cancelled.".to_owned())?;
    if call.plugin_id != plugin_id || call.name != name || call.lease_id != lease_id {
        return Err("This uTools MCP progress belongs to another runtime call.".to_owned());
    }
    drop(pending);
    app.emit_to(
        "main",
        "ihub://utools-tool-progress",
        UtoolsToolProgressEvent {
            request_id: request_id.to_owned(),
            plugin_id: plugin_id.to_owned(),
            name: name.to_owned(),
            progress,
            total,
            message,
        },
    )
    .map_err(|error| format!("Could not emit uTools MCP progress: {error}"))?;
    Ok(json!({ "accepted": true }))
}

#[tauri::command]
pub fn list_utools_tools(
    state: State<'_, AppState>,
) -> Result<Vec<UtoolsToolCatalogEntry>, String> {
    let registered = state
        .host
        .utools_tools
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let mut catalog = Vec::new();
    for plugin in state
        .plugins
        .list()
        .into_iter()
        .filter(|plugin| plugin.enabled && plugin.tool_count > 0)
    {
        for tool in state.plugins.utools_tool_definitions(&plugin.id)? {
            let active = registered
                .get(&(plugin.id.clone(), tool.name.clone()))
                .is_some_and(|handler| {
                    state
                        .plugin_assets
                        .is_active_for(&handler.lease_id, &handler.plugin_id)
                });
            catalog.push(UtoolsToolCatalogEntry {
                plugin_id: plugin.id.clone(),
                plugin_name: plugin.name.clone(),
                name: tool.name,
                description: tool.description,
                input_schema: tool.input_schema,
                output_schema: tool.output_schema,
                registered: active,
            });
        }
    }
    catalog.sort_by(|left, right| {
        left.plugin_name
            .to_lowercase()
            .cmp(&right.plugin_name.to_lowercase())
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.plugin_id.cmp(&right.plugin_id))
    });
    Ok(catalog)
}

#[tauri::command]
pub async fn invoke_utools_tool(
    app: AppHandle,
    plugin_id: String,
    name: String,
    params: Value,
    request_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<UtoolsToolInvocationResult, String> {
    if !is_plugin_id(&plugin_id) {
        return Err("Invalid plugin ID.".to_owned());
    }
    validate_utools_tool_json_value(&params, "tool parameters")?;
    let tool = state.plugins.utools_tool_definition(&plugin_id, &name)?;
    validate_utools_tool_value(&tool, &params, false)?;
    let handler = state
        .host
        .utools_tools
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&(plugin_id.clone(), name.clone()))
        .cloned()
        .ok_or_else(|| {
            format!(
                "uTools MCP handler '{plugin_id}/{name}' is not registered by its current runtime."
            )
        })?;
    if !state
        .plugin_assets
        .is_active_for(&handler.lease_id, &handler.plugin_id)
    {
        return Err(format!(
            "uTools MCP handler '{plugin_id}/{name}' belongs to an expired runtime."
        ));
    }
    let request_id = match request_id {
        Some(request_id) if Uuid::parse_str(&request_id).is_ok() => request_id,
        Some(_) => return Err("uTools MCP requestId must be a UUID.".to_owned()),
        None => Uuid::new_v4().to_string(),
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    {
        let mut pending = state
            .host
            .pending_utools_tool_calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if pending.len() >= MAX_PENDING_UTOOLS_TOOL_CALLS {
            return Err(format!(
                "The host already has {MAX_PENDING_UTOOLS_TOOL_CALLS} uTools MCP calls in progress."
            ));
        }
        let per_plugin = pending
            .values()
            .filter(|call| call.plugin_id == plugin_id)
            .count();
        if per_plugin >= MAX_PENDING_UTOOLS_TOOL_CALLS_PER_PLUGIN {
            return Err(format!(
                "Plugin '{plugin_id}' already has {MAX_PENDING_UTOOLS_TOOL_CALLS_PER_PLUGIN} MCP calls in progress."
            ));
        }
        if pending.contains_key(&request_id) {
            return Err("This uTools MCP requestId is already in progress.".to_owned());
        }
        pending.insert(
            request_id.clone(),
            PendingUtoolsToolCall {
                plugin_id: plugin_id.clone(),
                name: name.clone(),
                lease_id: handler.lease_id.clone(),
                response: sender,
            },
        );
    }
    let event_name = format!("ihub://plugin/{plugin_id}/event/utools.tool.invoke");
    if let Err(error) = app.emit_to(
        &handler.window_label,
        &event_name,
        json!({ "requestId": request_id, "name": name, "params": params }),
    ) {
        state
            .host
            .pending_utools_tool_calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&request_id);
        return Err(format!("Could not dispatch uTools MCP call: {error}"));
    }
    let wait_id = request_id.clone();
    let response = tauri::async_runtime::spawn_blocking(move || {
        receiver.recv_timeout(UTOOLS_TOOL_CALL_TIMEOUT)
    })
    .await
    .map_err(|error| format!("uTools MCP wait failed: {error}"))?;
    let result = match response {
        Ok(result) => result?,
        Err(RecvTimeoutError::Timeout) => {
            state
                .host
                .pending_utools_tool_calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&wait_id);
            let event_name = format!("ihub://plugin/{plugin_id}/event/utools.tool.cancel");
            let _ = app.emit_to(
                &handler.window_label,
                &event_name,
                json!({ "requestId": wait_id, "name": name }),
            );
            return Err("uTools MCP handler timed out after 10 minutes.".to_owned());
        }
        Err(RecvTimeoutError::Disconnected) => {
            return Err("uTools MCP handler stopped before responding.".to_owned());
        }
    };
    Ok(UtoolsToolInvocationResult { request_id, result })
}

#[tauri::command]
pub fn cancel_utools_tool(
    app: AppHandle,
    request_id: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    if Uuid::parse_str(&request_id).is_err() {
        return Err("Invalid uTools MCP request ID.".to_owned());
    }
    let Some(call) = state
        .host
        .pending_utools_tool_calls
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&request_id)
    else {
        return Ok(false);
    };
    let handler = state
        .host
        .utools_tools
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&(call.plugin_id.clone(), call.name.clone()))
        .cloned();
    let _ = call
        .response
        .send(Err("uTools MCP call was cancelled by the host.".to_owned()));
    if let Some(handler) = handler.filter(|handler| handler.lease_id == call.lease_id) {
        let event_name = format!("ihub://plugin/{}/event/utools.tool.cancel", call.plugin_id);
        let _ = app.emit_to(
            &handler.window_label,
            &event_name,
            json!({ "requestId": request_id, "name": call.name }),
        );
    }
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn start_utools_ffmpeg_request(
    app: AppHandle,
    host: Arc<PluginHostState>,
    integration: UtoolsFfmpegIntegration,
    plugin_id: String,
    lease_id: String,
    window_label: String,
    request_id: String,
    run: PreparedUtoolsFfmpegRun,
    native_lease: PluginNativeCommandLease,
) -> Result<Value, String> {
    let control = Arc::new(FfmpegControl::default());
    {
        let mut active = host
            .utools_ffmpeg_jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if active.contains_key(&request_id) {
            return Err("This uTools FFmpeg requestId is already active.".to_owned());
        }
        active.insert(
            request_id.clone(),
            ActiveUtoolsFfmpegJob {
                plugin_id: plugin_id.clone(),
                lease_id: lease_id.clone(),
                control: control.clone(),
            },
        );
    }

    let response_request_id = request_id.clone();
    tauri::async_runtime::spawn(async move {
        let executable = integration.ensure_installed().await;
        let outcome = match executable {
            Ok(executable) => {
                let progress_app = app.clone();
                let progress_host = host.clone();
                let progress_plugin_id = plugin_id.clone();
                let progress_lease_id = lease_id.clone();
                let progress_request_id = request_id.clone();
                let progress_window_label = window_label.clone();
                let task_control = control.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    let result = crate::utools_ffmpeg::run(
                        &executable,
                        &run.args,
                        run.duration_seconds,
                        &task_control,
                        move |progress| {
                            let still_owned = progress_host
                                .utools_ffmpeg_jobs
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                .get(&progress_request_id)
                                .is_some_and(|job| {
                                    job.plugin_id == progress_plugin_id
                                        && job.lease_id == progress_lease_id
                                });
                            if still_owned {
                                let event_name = format!(
                                    "ihub://plugin/{progress_plugin_id}/event/utools.ffmpeg.progress"
                                );
                                let _ = progress_app.emit_to(
                                    &progress_window_label,
                                    &event_name,
                                    json!({
                                        "requestId": progress_request_id,
                                        "progress": progress,
                                    }),
                                );
                            }
                        },
                    );
                    let result = result.and_then(|_| publish_utools_ffmpeg_output(&run));
                    if result.is_err() {
                        let _ = fs::remove_file(&run.staging_output);
                    }
                    result
                })
                .await
                .map_err(|error| format!("uTools FFmpeg host task failed: {error}"))
                .and_then(|value| value)
            }
            Err(error) => Err(error),
        };
        // The process is now gone. Only now may lifecycle transitions observe
        // the native worker reservation as released.
        drop(native_lease);
        let still_owned = {
            let mut active = host
                .utools_ffmpeg_jobs
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let still_owned = active
                .get(&request_id)
                .is_some_and(|job| job.plugin_id == plugin_id && job.lease_id == lease_id);
            if still_owned {
                active.remove(&request_id);
            }
            still_owned
        };
        if !still_owned {
            return;
        }
        let event_name = format!("ihub://plugin/{plugin_id}/event/utools.ffmpeg.complete");
        let payload = match outcome {
            Ok(()) => json!({ "requestId": request_id, "ok": true, "error": null }),
            Err(error) => json!({
                "requestId": request_id,
                "ok": false,
                "error": error.chars().take(4_000).collect::<String>(),
            }),
        };
        let _ = app.emit_to(&window_label, &event_name, payload);
    });
    Ok(json!({ "accepted": true, "requestId": response_request_id }))
}

fn control_utools_ffmpeg_request(
    host: &PluginHostState,
    plugin_id: &str,
    lease_id: &str,
    params: &Value,
    kill: bool,
) -> Result<Value, String> {
    validate_exact_plugin_params(params, &["requestId"])?;
    let request_id = required_string(params, "requestId")?;
    let parsed = Uuid::parse_str(request_id)
        .map_err(|_| "utools.runFFmpeg requestId must be a UUID.".to_owned())?;
    if parsed.get_version() != Some(uuid::Version::Random) {
        return Err("utools.runFFmpeg requestId must be a version 4 UUID.".to_owned());
    }
    let active = host
        .utools_ffmpeg_jobs
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(job) = active.get(request_id) else {
        return Ok(json!({ "accepted": false }));
    };
    if job.plugin_id != plugin_id || job.lease_id != lease_id {
        return Err("This uTools FFmpeg job belongs to another plugin session.".to_owned());
    }
    if kill {
        job.control.kill();
    } else {
        job.control.quit();
    }
    Ok(json!({ "accepted": true }))
}

fn start_utools_ai_request(context: UtoolsAiStartContext, params: &Value) -> Result<Value, String> {
    let UtoolsAiStartContext {
        app,
        host,
        plugin_assets,
        providers,
        plugin_id,
        lease_id,
        window_label,
    } = context;
    validate_exact_plugin_params(params, &["requestId", "option", "stream"])?;
    let request_id = required_string(params, "requestId")?;
    let parsed_id = Uuid::parse_str(request_id)
        .map_err(|_| "utools.ai requestId must be a UUID.".to_owned())?;
    if parsed_id.get_version() != Some(uuid::Version::Random) {
        return Err("utools.ai requestId must be a version 4 UUID.".to_owned());
    }
    let stream = params
        .get("stream")
        .and_then(Value::as_bool)
        .ok_or_else(|| "utools.ai start requires a boolean stream value.".to_owned())?;
    let option = params
        .get("option")
        .cloned()
        .ok_or_else(|| "utools.ai start requires options.".to_owned())?;
    let option = serde_json::from_value::<UtoolsAiOption>(option)
        .map_err(|error| format!("Invalid utools.ai options: {error}"))?;
    let option = validate_ai_option(option)?;
    let cancelled = Arc::new(AtomicBool::new(false));
    {
        let mut active = host
            .utools_ai_requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if active.contains_key(request_id) {
            return Err("This uTools AI requestId is already active.".to_owned());
        }
        if active.len() >= MAX_ACTIVE_UTOOLS_AI_REQUESTS {
            return Err(format!(
                "iHub already has {MAX_ACTIVE_UTOOLS_AI_REQUESTS} uTools AI requests in progress."
            ));
        }
        let per_plugin = active
            .values()
            .filter(|request| request.plugin_id == plugin_id)
            .count();
        if per_plugin >= MAX_ACTIVE_UTOOLS_AI_REQUESTS_PER_PLUGIN {
            return Err(format!(
                "Plugin '{plugin_id}' already has {MAX_ACTIVE_UTOOLS_AI_REQUESTS_PER_PLUGIN} AI requests in progress."
            ));
        }
        active.insert(
            request_id.to_owned(),
            ActiveUtoolsAiRequest {
                plugin_id: plugin_id.clone(),
                lease_id: lease_id.clone(),
                cancelled: cancelled.clone(),
                abort_handle: None,
            },
        );
    }

    let task_host = host.clone();
    let task_plugin_id = plugin_id;
    let task_lease_id = lease_id;
    let task_window_label = window_label;
    let task_request_id = request_id.to_owned();
    let task_cancelled = cancelled.clone();
    let handle = tauri::async_runtime::spawn(async move {
        let outcome = execute_utools_ai_request(
            &app,
            &task_host,
            &plugin_assets,
            &providers,
            &task_plugin_id,
            &task_lease_id,
            &task_window_label,
            &task_request_id,
            option,
            stream,
            &task_cancelled,
        )
        .await;
        let still_owned = {
            let mut active = task_host
                .utools_ai_requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let still_owned = active.get(&task_request_id).is_some_and(|request| {
                request.plugin_id == task_plugin_id && request.lease_id == task_lease_id
            });
            if still_owned {
                active.remove(&task_request_id);
            }
            still_owned
        };
        if !still_owned || task_cancelled.load(Ordering::Acquire) {
            return;
        }
        let event_name = format!("ihub://plugin/{task_plugin_id}/event/utools.ai.complete");
        let payload = match outcome {
            Ok(message) => json!({
                "requestId": task_request_id,
                "ok": true,
                "result": message,
                "error": null,
            }),
            Err(error) => json!({
                "requestId": task_request_id,
                "ok": false,
                "result": null,
                "error": bounded_utools_ai_error(&error),
            }),
        };
        let _ = app.emit_to(&task_window_label, &event_name, payload);
    });
    let abort_handle = handle.inner().abort_handle();
    let mut active = host
        .utools_ai_requests
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(request) = active.get_mut(request_id) {
        request.abort_handle = Some(abort_handle);
    } else {
        abort_handle.abort();
    }
    Ok(json!({ "accepted": true, "requestId": request_id }))
}

#[allow(clippy::too_many_arguments)]
async fn execute_utools_ai_request(
    app: &AppHandle,
    host: &Arc<PluginHostState>,
    plugin_assets: &PluginAssetServer,
    providers: &AiProviderStore,
    plugin_id: &str,
    lease_id: &str,
    window_label: &str,
    request_id: &str,
    option: UtoolsAiOption,
    stream: bool,
    cancelled: &AtomicBool,
) -> Result<UtoolsAiMessage, String> {
    let resolved = providers.resolve_model(option.model.as_deref())?;
    let mut messages = initial_wire_messages(&option);
    let mut function_calls = 0usize;
    for _ in 0..MAX_UTOOLS_AI_ROUNDS {
        ensure_utools_ai_request_current(plugin_assets, plugin_id, lease_id, cancelled)?;
        let chunk_event = format!("ihub://plugin/{plugin_id}/event/utools.ai.chunk");
        let round = execute_chat_round(&resolved, &messages, &option.tools, stream, |message| {
            ensure_utools_ai_request_current(plugin_assets, plugin_id, lease_id, cancelled)?;
            app.emit_to(
                window_label,
                &chunk_event,
                json!({ "requestId": request_id, "message": message }),
            )
            .map_err(|error| format!("Could not emit uTools AI stream chunk: {error}"))
        })
        .await?;
        if round.tool_calls.is_empty() {
            return Ok(round.message);
        }
        function_calls = function_calls.saturating_add(round.tool_calls.len());
        if function_calls > MAX_UTOOLS_AI_FUNCTION_CALLS {
            return Err(format!(
                "utools.ai exceeded {MAX_UTOOLS_AI_FUNCTION_CALLS} function calls."
            ));
        }
        messages.push(round.assistant_wire_message);
        for call in round.tool_calls {
            if !option
                .tools
                .iter()
                .any(|tool| tool.function.name == call.name)
            {
                return Err(format!(
                    "AI provider requested undeclared function '{}'.",
                    call.name
                ));
            }
            let result = match invoke_utools_ai_function(
                app,
                host,
                plugin_assets,
                plugin_id,
                lease_id,
                window_label,
                request_id,
                &call,
                cancelled,
            )
            .await
            {
                Ok(result) => result,
                Err(error) => json!({ "error": bounded_utools_ai_error(&error) }),
            };
            messages.push(tool_result_wire_message(&call, &result)?);
        }
    }
    Err(format!(
        "utools.ai exceeded {MAX_UTOOLS_AI_ROUNDS} model rounds."
    ))
}

#[allow(clippy::too_many_arguments)]
async fn invoke_utools_ai_function(
    app: &AppHandle,
    host: &Arc<PluginHostState>,
    plugin_assets: &PluginAssetServer,
    plugin_id: &str,
    lease_id: &str,
    window_label: &str,
    request_id: &str,
    call: &AiToolCall,
    cancelled: &AtomicBool,
) -> Result<Value, String> {
    ensure_utools_ai_request_current(plugin_assets, plugin_id, lease_id, cancelled)?;
    let invocation_id = Uuid::new_v4().to_string();
    let (sender, receiver) = mpsc::sync_channel(1);
    {
        let mut pending = host
            .pending_utools_ai_function_calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if pending.len() >= MAX_ACTIVE_UTOOLS_AI_REQUESTS * MAX_UTOOLS_AI_FUNCTION_CALLS {
            return Err("The host has too many pending uTools AI functions.".to_owned());
        }
        pending.insert(
            invocation_id.clone(),
            PendingUtoolsAiFunctionCall {
                request_id: request_id.to_owned(),
                plugin_id: plugin_id.to_owned(),
                lease_id: lease_id.to_owned(),
                name: call.name.clone(),
                response: sender,
            },
        );
    }
    let event_name = format!("ihub://plugin/{plugin_id}/event/utools.ai.tool.invoke");
    if let Err(error) = app.emit_to(
        window_label,
        &event_name,
        json!({
            "requestId": request_id,
            "invocationId": invocation_id,
            "toolCallId": call.id,
            "name": call.name,
            "arguments": call.arguments,
        }),
    ) {
        host.pending_utools_ai_function_calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&invocation_id);
        return Err(format!("Could not dispatch uTools AI function: {error}"));
    }
    let wait_id = invocation_id.clone();
    let response = tauri::async_runtime::spawn_blocking(move || {
        receiver.recv_timeout(UTOOLS_AI_FUNCTION_TIMEOUT)
    })
    .await
    .map_err(|error| format!("uTools AI function wait failed: {error}"))?;
    match response {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => {
            host.pending_utools_ai_function_calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&wait_id);
            Err("uTools AI function timed out after two minutes.".to_owned())
        }
        Err(RecvTimeoutError::Disconnected) => {
            Err("uTools AI function runtime stopped before responding.".to_owned())
        }
    }
}

fn complete_utools_ai_function_call(
    host: &PluginHostState,
    plugin_id: &str,
    lease_id: &str,
    params: &Value,
) -> Result<Value, String> {
    validate_exact_plugin_params(
        params,
        &["requestId", "invocationId", "name", "ok", "result", "error"],
    )?;
    let request_id = required_string(params, "requestId")?;
    let invocation_id = required_string(params, "invocationId")?;
    let name = required_string(params, "name")?;
    let ok = params
        .get("ok")
        .and_then(Value::as_bool)
        .ok_or_else(|| "uTools AI function completion requires boolean ok.".to_owned())?;
    let pending = {
        let mut calls = host
            .pending_utools_ai_function_calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let call = calls
            .get(invocation_id)
            .ok_or_else(|| "This uTools AI function call expired or was cancelled.".to_owned())?;
        if call.request_id != request_id
            || call.plugin_id != plugin_id
            || call.lease_id != lease_id
            || call.name != name
        {
            return Err("This AI function completion belongs to another request.".to_owned());
        }
        calls
            .remove(invocation_id)
            .expect("the verified pending AI function still exists")
    };
    let outcome = if ok {
        let result = params.get("result").cloned().unwrap_or(Value::Null);
        validate_utools_tool_json_value(&result, "AI function result")?;
        Ok(result)
    } else {
        Err(params
            .get("error")
            .and_then(Value::as_str)
            .map(bounded_utools_ai_error)
            .unwrap_or_else(|| "uTools AI function failed.".to_owned()))
    };
    let _ = pending.response.send(outcome);
    Ok(json!({ "completed": true }))
}

fn abort_utools_ai_request(
    host: &PluginHostState,
    plugin_id: &str,
    lease_id: &str,
    params: &Value,
) -> Result<Value, String> {
    validate_exact_plugin_params(params, &["requestId"])?;
    let request_id = required_string(params, "requestId")?;
    if Uuid::parse_str(request_id).is_err() {
        return Err("Invalid uTools AI request ID.".to_owned());
    }
    let request = {
        let mut active = host
            .utools_ai_requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(request) = active.get(request_id) else {
            return Ok(json!({ "aborted": false }));
        };
        if request.plugin_id != plugin_id || request.lease_id != lease_id {
            return Err("This uTools AI request belongs to another runtime.".to_owned());
        }
        active
            .remove(request_id)
            .expect("the verified active AI request still exists")
    };
    request.cancelled.store(true, Ordering::Release);
    if let Some(abort_handle) = request.abort_handle {
        abort_handle.abort();
    }
    reject_pending_utools_ai_functions(
        host,
        |call| call.request_id == request_id && call.plugin_id == plugin_id,
        "uTools AI request was aborted.",
    );
    Ok(json!({ "aborted": true }))
}

fn reject_pending_utools_ai_functions<F>(host: &PluginHostState, mut predicate: F, message: &str)
where
    F: FnMut(&PendingUtoolsAiFunctionCall) -> bool,
{
    let removed = {
        let mut pending = host
            .pending_utools_ai_function_calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let ids = pending
            .iter()
            .filter_map(|(id, call)| predicate(call).then_some(id.clone()))
            .collect::<Vec<_>>();
        ids.into_iter()
            .filter_map(|id| pending.remove(&id))
            .collect::<Vec<_>>()
    };
    for call in removed {
        let _ = call.response.send(Err(message.to_owned()));
    }
}

fn ensure_utools_ai_request_current(
    plugin_assets: &PluginAssetServer,
    plugin_id: &str,
    lease_id: &str,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    if cancelled.load(Ordering::Acquire) {
        return Err("uTools AI request was aborted.".to_owned());
    }
    if !plugin_assets.is_active_for(lease_id, plugin_id) {
        return Err("uTools AI runtime session expired.".to_owned());
    }
    Ok(())
}

fn bounded_utools_ai_error(error: &str) -> String {
    let error = error.trim();
    let error = if error.is_empty() {
        "uTools AI request failed."
    } else {
        error
    };
    error.chars().take(2_000).collect()
}

fn validate_utools_browser_message<'a>(
    params: &'a Value,
    channel: &str,
) -> Result<&'a Vec<Value>, String> {
    if channel.is_empty() || channel.chars().count() > 128 || channel.chars().any(char::is_control)
    {
        return Err("uTools BrowserWindow IPC channel is invalid.".to_owned());
    }
    let args = params
        .get("args")
        .and_then(Value::as_array)
        .ok_or_else(|| "uTools BrowserWindow IPC args must be an array.".to_owned())?;
    if args.len() > 32 {
        return Err("uTools BrowserWindow IPC accepts at most 32 arguments.".to_owned());
    }
    let encoded = serde_json::to_vec(args)
        .map_err(|error| format!("Could not encode uTools BrowserWindow IPC arguments: {error}"))?;
    if encoded.len() > 256 * 1024 {
        return Err("uTools BrowserWindow IPC arguments exceed 256 KiB.".to_owned());
    }
    Ok(args)
}

fn handle_utools_browser_window_control(
    app: &AppHandle,
    request: &PluginHostRequest,
) -> Result<Value, String> {
    if !request.surface {
        return Err("uTools BrowserWindow controls require the visible parent surface.".to_owned());
    }
    validate_exact_plugin_params(&request.params, &["browserId", "action", "args"])?;
    let browser_id = required_string(&request.params, "browserId")?;
    let action = required_string(&request.params, "action")?;
    let args = request
        .params
        .get("args")
        .and_then(Value::as_array)
        .ok_or_else(|| "uTools BrowserWindow control args must be an array.".to_owned())?;
    if args.len() > 4 {
        return Err("uTools BrowserWindow control accepts at most four arguments.".to_owned());
    }
    let registry = app.state::<UtoolsBrowserWindowRegistry>();
    let (label, _) = registry.validate_parent(browser_id, &request.plugin_id, &request.lease_id)?;
    let window = app
        .get_webview_window(&label)
        .ok_or_else(|| "This uTools BrowserWindow is no longer available.".to_owned())?;
    let no_args = || {
        if args.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "uTools BrowserWindow {action} does not accept arguments."
            ))
        }
    };
    let boolean_arg = || {
        if args.len() != 1 {
            return Err(format!(
                "uTools BrowserWindow {action} requires one boolean argument."
            ));
        }
        args[0]
            .as_bool()
            .ok_or_else(|| format!("uTools BrowserWindow {action} requires one boolean argument."))
    };
    match action {
        "show" => {
            no_args()?;
            window
                .show()
                .map_err(|error| format!("Could not show BrowserWindow: {error}"))?;
            Ok(Value::Null)
        }
        "hide" => {
            no_args()?;
            window
                .hide()
                .map_err(|error| format!("Could not hide BrowserWindow: {error}"))?;
            Ok(Value::Null)
        }
        "close" | "destroy" => {
            no_args()?;
            window
                .close()
                .map_err(|error| format!("Could not close BrowserWindow: {error}"))?;
            Ok(Value::Null)
        }
        "focus" => {
            no_args()?;
            window
                .set_focus()
                .map_err(|error| format!("Could not focus BrowserWindow: {error}"))?;
            Ok(Value::Null)
        }
        "center" => {
            no_args()?;
            window
                .center()
                .map_err(|error| format!("Could not center BrowserWindow: {error}"))?;
            Ok(Value::Null)
        }
        "maximize" => {
            no_args()?;
            window
                .maximize()
                .map_err(|error| format!("Could not maximize BrowserWindow: {error}"))?;
            Ok(Value::Null)
        }
        "unmaximize" => {
            no_args()?;
            window
                .unmaximize()
                .map_err(|error| format!("Could not restore BrowserWindow: {error}"))?;
            Ok(Value::Null)
        }
        "minimize" => {
            no_args()?;
            window
                .minimize()
                .map_err(|error| format!("Could not minimize BrowserWindow: {error}"))?;
            Ok(Value::Null)
        }
        "restore" => {
            no_args()?;
            window
                .unminimize()
                .map_err(|error| format!("Could not restore BrowserWindow: {error}"))?;
            Ok(Value::Null)
        }
        "setAlwaysOnTop" => {
            window.set_always_on_top(boolean_arg()?).map_err(|error| {
                format!("Could not change BrowserWindow always-on-top state: {error}")
            })?;
            Ok(Value::Null)
        }
        "setFullScreen" => {
            window.set_fullscreen(boolean_arg()?).map_err(|error| {
                format!("Could not change BrowserWindow fullscreen state: {error}")
            })?;
            Ok(Value::Null)
        }
        "setResizable" => {
            window.set_resizable(boolean_arg()?).map_err(|error| {
                format!("Could not change BrowserWindow resizable state: {error}")
            })?;
            Ok(Value::Null)
        }
        "setMaximizable" => {
            window.set_maximizable(boolean_arg()?).map_err(|error| {
                format!("Could not change BrowserWindow maximizable state: {error}")
            })?;
            Ok(Value::Null)
        }
        "setMinimizable" => {
            window.set_minimizable(boolean_arg()?).map_err(|error| {
                format!("Could not change BrowserWindow minimizable state: {error}")
            })?;
            Ok(Value::Null)
        }
        "setClosable" => {
            window.set_closable(boolean_arg()?).map_err(|error| {
                format!("Could not change BrowserWindow closable state: {error}")
            })?;
            Ok(Value::Null)
        }
        "setDecorations" => {
            window
                .set_decorations(boolean_arg()?)
                .map_err(|error| format!("Could not change BrowserWindow frame state: {error}"))?;
            Ok(Value::Null)
        }
        "setFocusable" => {
            window.set_focusable(boolean_arg()?).map_err(|error| {
                format!("Could not change BrowserWindow focusable state: {error}")
            })?;
            Ok(Value::Null)
        }
        "setShadow" => {
            window
                .set_shadow(boolean_arg()?)
                .map_err(|error| format!("Could not change BrowserWindow shadow state: {error}"))?;
            Ok(Value::Null)
        }
        "setVisibleOnAllWorkspaces" => {
            window
                .set_visible_on_all_workspaces(boolean_arg()?)
                .map_err(|error| {
                    format!("Could not change BrowserWindow workspace visibility: {error}")
                })?;
            Ok(Value::Null)
        }
        "setContentProtection" => {
            window
                .set_content_protected(boolean_arg()?)
                .map_err(|error| {
                    format!("Could not change BrowserWindow content protection: {error}")
                })?;
            Ok(Value::Null)
        }
        "setIgnoreMouseEvents" => {
            window
                .set_ignore_cursor_events(boolean_arg()?)
                .map_err(|error| {
                    format!("Could not change BrowserWindow mouse handling: {error}")
                })?;
            Ok(Value::Null)
        }
        "setSkipTaskbar" => {
            window.set_skip_taskbar(boolean_arg()?).map_err(|error| {
                format!("Could not change BrowserWindow taskbar state: {error}")
            })?;
            Ok(Value::Null)
        }
        "setTitle" => {
            let title = args
                .first()
                .and_then(Value::as_str)
                .filter(|title| {
                    title.chars().count() <= 160 && !title.chars().any(char::is_control)
                })
                .ok_or_else(|| {
                    "uTools BrowserWindow setTitle requires one bounded string.".to_owned()
                })?;
            if args.len() != 1 {
                return Err("uTools BrowserWindow setTitle requires one bounded string.".to_owned());
            }
            window
                .set_title(title)
                .map_err(|error| format!("Could not change BrowserWindow title: {error}"))?;
            Ok(Value::Null)
        }
        "setSize" => {
            let (width, height) = browser_window_pair(args, action, 64.0, 16_384.0)?;
            window
                .set_size(LogicalSize::new(width, height))
                .map_err(|error| format!("Could not resize BrowserWindow: {error}"))?;
            Ok(Value::Null)
        }
        "setPosition" => {
            let (x, y) = browser_window_pair(args, action, -100_000.0, 100_000.0)?;
            window
                .set_position(LogicalPosition::new(x, y))
                .map_err(|error| format!("Could not move BrowserWindow: {error}"))?;
            Ok(Value::Null)
        }
        "isVisible" => {
            no_args()?;
            window
                .is_visible()
                .map(Value::Bool)
                .map_err(|error| format!("Could not read BrowserWindow visibility: {error}"))
        }
        "isFocused" => {
            no_args()?;
            window
                .is_focused()
                .map(Value::Bool)
                .map_err(|error| format!("Could not read BrowserWindow focus: {error}"))
        }
        "isMaximized" => {
            no_args()?;
            window
                .is_maximized()
                .map(Value::Bool)
                .map_err(|error| format!("Could not read BrowserWindow maximized state: {error}"))
        }
        "isMinimized" => {
            no_args()?;
            window
                .is_minimized()
                .map(Value::Bool)
                .map_err(|error| format!("Could not read BrowserWindow minimized state: {error}"))
        }
        "isFullScreen" => {
            no_args()?;
            window
                .is_fullscreen()
                .map(Value::Bool)
                .map_err(|error| format!("Could not read BrowserWindow fullscreen state: {error}"))
        }
        "isResizable" => {
            no_args()?;
            window
                .is_resizable()
                .map(Value::Bool)
                .map_err(|error| format!("Could not read BrowserWindow resizable state: {error}"))
        }
        "isAlwaysOnTop" => {
            no_args()?;
            window.is_always_on_top().map(Value::Bool).map_err(|error| {
                format!("Could not read BrowserWindow always-on-top state: {error}")
            })
        }
        "getTitle" => {
            no_args()?;
            window
                .title()
                .map(Value::String)
                .map_err(|error| format!("Could not read BrowserWindow title: {error}"))
        }
        "reload" => {
            no_args()?;
            window
                .reload()
                .map_err(|error| format!("Could not reload BrowserWindow: {error}"))?;
            Ok(Value::Null)
        }
        "getSize" => {
            no_args()?;
            let size = window
                .inner_size()
                .map_err(|error| format!("Could not read BrowserWindow size: {error}"))?;
            let scale = window
                .scale_factor()
                .map_err(|error| format!("Could not read BrowserWindow scale factor: {error}"))?;
            let logical = size.to_logical::<f64>(scale);
            Ok(json!([
                logical.width.round() as i64,
                logical.height.round() as i64
            ]))
        }
        "getPosition" => {
            no_args()?;
            let position = window
                .outer_position()
                .map_err(|error| format!("Could not read BrowserWindow position: {error}"))?;
            let scale = window
                .scale_factor()
                .map_err(|error| format!("Could not read BrowserWindow scale factor: {error}"))?;
            let logical = position.to_logical::<f64>(scale);
            Ok(json!([logical.x.round() as i64, logical.y.round() as i64]))
        }
        _ => Err(format!(
            "Unsupported uTools BrowserWindow method '{action}'."
        )),
    }
}

fn browser_window_pair(
    args: &[Value],
    action: &str,
    minimum: f64,
    maximum: f64,
) -> Result<(f64, f64), String> {
    if args.len() != 2 {
        return Err(format!(
            "uTools BrowserWindow {action} requires two numbers."
        ));
    }
    let first = args[0]
        .as_f64()
        .filter(|value| value.is_finite() && (minimum..=maximum).contains(value));
    let second = args[1]
        .as_f64()
        .filter(|value| value.is_finite() && (minimum..=maximum).contains(value));
    first.zip(second).ok_or_else(|| {
        format!("uTools BrowserWindow {action} arguments are outside the supported range.")
    })
}

fn plugin_search_providers_changed_payload(
    plugin_id: &str,
    provider_id: Option<&str>,
    registered: bool,
) -> Value {
    let mut payload = json!({
        "pluginId": plugin_id,
        "registered": registered,
    });
    if let Some(provider_id) = provider_id {
        payload["providerId"] = Value::String(provider_id.to_owned());
    }
    payload
}

/// Handles a request only after the parent-bound frontend lease has been
/// checked under the same transition lock used by plugin updates and links.
fn plugin_host_call_for_active_lease(
    app: &AppHandle,
    request: PluginHostRequest,
    state: &AppState,
) -> Result<Value, String> {
    ensure_plugin_host_request_is_allowed(&request, state)?;
    match request.method.as_str() {
        "compatibility.utools.browser.create" => {
            if !request.surface
                || !state
                    .plugin_assets
                    .is_active_surface_for(&request.lease_id, &request.plugin_id)
            {
                return Err("uTools BrowserWindow creation requires the plugin's visible active surface.".to_owned());
            }
            validate_exact_plugin_params(&request.params, &["url", "options"])?;
            let url = required_string(&request.params, "url")?;
            // Resolve before native window reservation so an invalid or escaped
            // path cannot leave behind even a blank host window.
            let options = match request.params.get("options") {
                None | Some(Value::Null) => UtoolsBrowserWindowOptions::default(),
                Some(value) => serde_json::from_value(value.clone()).map_err(|error| {
                    format!("Invalid or unsupported uTools BrowserWindow options: {error}")
                })?,
            };
            state.plugins.browser_frontend_asset_bundle(
                &request.plugin_id,
                url,
                options.preload(),
            )?;
            let registry = app.state::<UtoolsBrowserWindowRegistry>();
            let parent_window_label = detached_plugin_event_target(
                app,
                state,
                &request.plugin_id,
            )?
            .unwrap_or_else(|| "main".to_owned());
            let opened = create_utools_browser_window(
                app,
                &registry,
                &request.plugin_id,
                &request.lease_id,
                &parent_window_label,
                url,
                options,
            )?;
            serde_json::to_value(opened)
                .map_err(|error| format!("Could not encode uTools BrowserWindow identity: {error}"))
        }
        "compatibility.utools.browser.control" => {
            handle_utools_browser_window_control(app, &request)
        }
        "compatibility.utools.browser.send" => {
            validate_exact_plugin_params(&request.params, &["browserId", "channel", "args"])?;
            let browser_id = required_string(&request.params, "browserId")?;
            let channel = required_string(&request.params, "channel")?;
            let args = validate_utools_browser_message(&request.params, channel)?;
            let registry = app.state::<UtoolsBrowserWindowRegistry>();
            let (label, _) = registry.validate_parent(
                browser_id,
                &request.plugin_id,
                &request.lease_id,
            )?;
            app.emit_to(
                &label,
                "ihub://utools-browser/child-message",
                json!({ "browserId": browser_id, "channel": channel, "args": args }),
            )
            .map_err(|error| format!("Could not deliver the uTools BrowserWindow message: {error}"))?;
            Ok(json!({ "sent": true }))
        }
        "compatibility.utools.ubrowser.setProxy" => {
            if !request.surface
                || !state
                    .plugin_assets
                    .is_active_surface_for(&request.lease_id, &request.plugin_id)
            {
                return Err(
                    "uTools ubrowser proxy changes require the visible active plugin surface."
                        .to_owned(),
                );
            }
            if !state.plugins.uses_utools_compatibility(&request.plugin_id)? {
                return Err("uTools ubrowser proxy is available only to imported uTools packages.".to_owned());
            }
            validate_exact_plugin_params(&request.params, &["config"])?;
            app.state::<UtoolsUBrowserRegistry>().set_proxy_config(
                &request.plugin_id,
                required_value(&request.params, "config")?,
            )?;
            Ok(json!({ "configured": true }))
        }
        "compatibility.utools.ubrowser.clearCache" => {
            if !request.surface
                || !state
                    .plugin_assets
                    .is_active_surface_for(&request.lease_id, &request.plugin_id)
            {
                return Err(
                    "uTools ubrowser cache clearing requires the visible active plugin surface."
                        .to_owned(),
                );
            }
            if !state.plugins.uses_utools_compatibility(&request.plugin_id)? {
                return Err("uTools ubrowser cache is available only to imported uTools packages.".to_owned());
            }
            validate_exact_plugin_params(&request.params, &[])?;
            app.state::<UtoolsUBrowserRegistry>()
                .clear_cache(app, &request.plugin_id)?;
            Ok(json!({ "cleared": true }))
        }
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
                .plugins
                .ensure_frontend_command(&request.plugin_id, command_id)?;
            if definition.get("shortcut").is_some() || definition.get("icon").is_some() {
                return Err(
                    "Runtime command registration cannot add artwork or global shortcuts; declare them in plugin.json."
                        .to_owned(),
                );
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
            emit_plugin_event_to_owner(
                app,
                state,
                &request.plugin_id,
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
            if !has_declared_plugin_search_provider(state, &request.plugin_id, provider_id)?
            {
                return Err(format!(
                    "Plugin search provider '{}/{}' must be declared in contributes.searchProviders before it can register.",
                    request.plugin_id, provider_id
                ));
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
            emit_plugin_search_providers_changed(
                app,
                &request.plugin_id,
                Some(provider_id),
                true,
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
            emit_plugin_search_providers_changed(
                app,
                &request.plugin_id,
                Some(provider_id),
                false,
            );
            Ok(json!({ "unregistered": true }))
        }
        "settings.get" => {
            let key = required_string(&request.params, "key")?;
            if key.starts_with("ihub.host.") {
                return Err("Host-owned plugin settings are not readable through the Bridge."
                    .to_owned());
            }
            let value = if state
                .plugins
                .is_secret_setting(&request.plugin_id, key)?
            {
                // A pre-v1 build could have saved the value before the
                // manifest learned about `secret`; erase that legacy copy
                // before returning the process-local value or fallback.
                state.plugin_settings.remove(&request.plugin_id, key)?;
                get_plugin_session_secret(&state.host, &request.plugin_id, key)
            } else {
                state.plugin_settings.get(&request.plugin_id, key)
            }
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
            if key.starts_with("ihub.host.") {
                return Err("Host-owned plugin settings are not writable through the Bridge."
                    .to_owned());
            }
            let value = required_value(&request.params, "value")?.clone();
            if state
                .plugins
                .is_secret_setting(&request.plugin_id, key)?
            {
                // Never retain a declared secret in the durable JSON file.
                // Scrub a legacy value first, then retain the new one only
                // until iHub exits or the plugin source/lifecycle resets.
                state.plugin_settings.remove(&request.plugin_id, key)?;
                set_plugin_session_secret(&state.host, &request.plugin_id, key, value)?;
                Ok(json!({ "saved": true, "persistent": false }))
            } else {
                state
                    .plugin_settings
                    .set(&request.plugin_id, key, value)?;
                Ok(json!({ "saved": true, "persistent": true }))
            }
        }
        "compatibility.utools.db.get" => {
            validate_exact_plugin_params(&request.params, &["id"])?;
            let id = required_string(&request.params, "id")?;
            Ok(state
                .utools_documents
                .get(&request.plugin_id, id)?
                .unwrap_or(Value::Null))
        }
        "compatibility.utools.db.put" => {
            validate_exact_plugin_params(&request.params, &["doc"])?;
            serde_json::to_value(state.utools_documents.put(
                &request.plugin_id,
                required_value(&request.params, "doc")?.clone(),
            )?)
            .map_err(|error| format!("Could not encode the uTools database result: {error}"))
        }
        "compatibility.utools.db.remove" => {
            validate_exact_plugin_params(&request.params, &["target"])?;
            serde_json::to_value(state.utools_documents.remove(
                &request.plugin_id,
                required_value(&request.params, "target")?,
            )?)
            .map_err(|error| format!("Could not encode the uTools database result: {error}"))
        }
        "compatibility.utools.db.bulkDocs" => {
            validate_exact_plugin_params(&request.params, &["docs"])?;
            let documents = required_value(&request.params, "docs")?
                .as_array()
                .ok_or_else(|| "uTools bulkDocs requires a document array.".to_owned())?
                .clone();
            serde_json::to_value(
                state
                    .utools_documents
                    .bulk_docs(&request.plugin_id, documents)?,
            )
            .map_err(|error| format!("Could not encode uTools bulk database results: {error}"))
        }
        "compatibility.utools.db.allDocs" => {
            validate_optional_plugin_param(&request.params, "selector")?;
            let documents = state
                .utools_documents
                .all_docs(&request.plugin_id, request.params.get("selector"))?;
            Ok(Value::Array(documents))
        }
        "compatibility.utools.db.postAttachment" => {
            validate_exact_plugin_params(
                &request.params,
                &["id", "dataBase64", "contentType"],
            )?;
            let id = required_string(&request.params, "id")?;
            let encoded = required_string(&request.params, "dataBase64")?;
            let max_encoded = crate::utools_db::MAX_ATTACHMENT_BYTES.div_ceil(3) * 4;
            if encoded.is_empty() || encoded.len() > max_encoded {
                return Err("uTools attachment base64 is empty or exceeds 10 MiB.".to_owned());
            }
            let bytes = BASE64_STANDARD
                .decode(encoded)
                .map_err(|_| "uTools attachment base64 is malformed.".to_owned())?;
            serde_json::to_value(state.utools_documents.post_attachment(
                &request.plugin_id,
                id,
                &bytes,
                required_string(&request.params, "contentType")?,
            )?)
            .map_err(|error| format!("Could not encode the uTools attachment result: {error}"))
        }
        "compatibility.utools.db.getAttachment" => {
            validate_exact_plugin_params(&request.params, &["id"])?;
            let Some(bytes) = state.utools_documents.get_attachment(
                &request.plugin_id,
                required_string(&request.params, "id")?,
            )? else {
                return Ok(Value::Null);
            };
            Ok(json!({ "dataBase64": BASE64_STANDARD.encode(bytes) }))
        }
        "compatibility.utools.db.getAttachmentType" => {
            validate_exact_plugin_params(&request.params, &["id"])?;
            Ok(state
                .utools_documents
                .get_attachment_type(
                    &request.plugin_id,
                    required_string(&request.params, "id")?,
                )?
                .map(Value::String)
                .unwrap_or(Value::Null))
        }
        "compatibility.utools.dbStorage.snapshot" => {
            let values = state
                .plugin_settings
                .snapshot_with_prefix(&request.plugin_id, UTOOLS_DB_STORAGE_PREFIX)
                .into_iter()
                .filter_map(|(encoded_key, value)| {
                    decode_utools_db_storage_key(&encoded_key).map(|key| (key, value))
                })
                .collect::<serde_json::Map<String, Value>>();
            Ok(Value::Object(values))
        }
        "compatibility.utools.dbStorage.set" => {
            let key = required_string(&request.params, "key")?;
            let value = required_value(&request.params, "value")?.clone();
            state.plugin_settings.set(
                &request.plugin_id,
                &utools_db_storage_key(key)?,
                value,
            )?;
            Ok(json!({ "saved": true, "persistent": true }))
        }
        "compatibility.utools.dbStorage.remove" => {
            let key = required_string(&request.params, "key")?;
            let removed = state.plugin_settings.remove(
                &request.plugin_id,
                &utools_db_storage_key(key)?,
            )?;
            Ok(json!({ "removed": removed }))
        }
        "compatibility.utools.dbCryptoStorage.snapshot" => {
            validate_exact_plugin_params(&request.params, &[])?;
            serde_json::to_value(state.plugin_crypto_storage.snapshot(&request.plugin_id)?)
                .map_err(|error| {
                    format!("Could not encode the uTools encrypted storage snapshot: {error}")
                })
        }
        "compatibility.utools.dbCryptoStorage.set" => {
            validate_exact_plugin_params(&request.params, &["key", "value"])?;
            state.plugin_crypto_storage.set(
                &request.plugin_id,
                required_string(&request.params, "key")?,
                required_value(&request.params, "value")?.clone(),
            )?;
            Ok(json!({ "saved": true, "persistent": true, "encrypted": true }))
        }
        "compatibility.utools.dbCryptoStorage.remove" => {
            validate_exact_plugin_params(&request.params, &["key"])?;
            let removed = state.plugin_crypto_storage.remove(
                &request.plugin_id,
                required_string(&request.params, "key")?,
            )?;
            Ok(json!({ "removed": removed }))
        }
        "compatibility.utools.features.snapshot" => serde_json::to_value(
            utools_dynamic_features(&state.plugin_settings, &request.plugin_id),
        )
        .map_err(|error| format!("Could not encode uTools dynamic features: {error}")),
        "compatibility.utools.features.set" => {
            let feature = validate_utools_dynamic_feature(required_value(
                &request.params,
                "feature",
            )?)?;
            let key = utools_dynamic_feature_key(&feature.code);
            let existing = state.plugin_settings.get(&request.plugin_id, &key);
            if let Some(existing) = existing.as_ref() {
                let existing = validate_utools_dynamic_feature(existing)?;
                if existing.code != feature.code {
                    return Err("uTools dynamic feature identity collision; choose another code."
                        .to_owned());
                }
            } else if utools_dynamic_features(&state.plugin_settings, &request.plugin_id).len()
                >= MAX_UTOOLS_DYNAMIC_FEATURES
            {
                return Err(format!(
                    "A uTools plugin may store at most {MAX_UTOOLS_DYNAMIC_FEATURES} dynamic features."
                ));
            }
            let value = serde_json::to_value(&feature)
                .map_err(|error| format!("Could not encode uTools dynamic feature: {error}"))?;
            state
                .plugin_settings
                .set(&request.plugin_id, &key, value)?;
            let _ = app.emit("ihub://plugin-shortcuts-changed", json!({}));
            Ok(json!({
                "feature": feature,
                "commandId": utools_dynamic_feature_command_id(&feature.code),
            }))
        }
        "compatibility.utools.features.remove" => {
            let code = required_string(&request.params, "code")?.trim();
            if code.is_empty()
                || code.chars().count() > 160
                || code.chars().any(char::is_control)
            {
                return Err("uTools dynamic feature code is invalid.".to_owned());
            }
            let key = utools_dynamic_feature_key(code);
            let Some(existing) = state.plugin_settings.get(&request.plugin_id, &key) else {
                return Ok(json!({ "removed": false }));
            };
            if validate_utools_dynamic_feature(&existing)?.code != code {
                return Err("uTools dynamic feature identity collision; nothing was removed."
                    .to_owned());
            }
            let removed = state
                .plugin_settings
                .remove(&request.plugin_id, &key)?;
            if removed {
                let _ = app.emit("ihub://plugin-shortcuts-changed", json!({}));
            }
            Ok(json!({ "removed": removed }))
        }
        "compatibility.utools.input.pasteText" => {
            let value = validate_utools_input_text(
                &request.params,
                MAX_PLUGIN_CLIPBOARD_TEXT_BYTES,
                None,
            )?;
            crate::clipboard_access::with_clipboard(|clipboard| clipboard.set_text(value))
                .map_err(|error| format!("Could not prepare the system clipboard for paste: {error}"))?;
            hide_and_schedule_utools_input(app, UtoolsInputAction::PasteClipboard)?;
            Ok(json!({ "accepted": true }))
        }
        "compatibility.utools.input.pasteImage" => {
            let image = decode_authorized_utools_clipboard_image(
                &state.host,
                &request.plugin_id,
                &request.lease_id,
                &request.params,
                "hideMainWindowPasteImage",
            )?;
            let bytes = image.bytes.into_owned();
            crate::clipboard_access::with_clipboard(|clipboard| {
                clipboard.set_image(arboard::ImageData {
                    width: image.width,
                    height: image.height,
                    bytes: std::borrow::Cow::Borrowed(bytes.as_ref()),
                })
            })
            .map_err(|error| format!("Could not prepare the PNG clipboard paste: {error}"))?;
            hide_and_schedule_utools_input(app, UtoolsInputAction::PasteClipboard)?;
            Ok(json!({ "accepted": true }))
        }
        "compatibility.utools.input.pasteFiles" => {
            let requested_paths = validate_utools_copy_file_paths(&request.params)?;
            if !state.host.admit_plugin_notification(&request.plugin_id) {
                return Err(format!(
                    "Interactive uTools alerts are limited to {MAX_PLUGIN_NOTIFICATIONS_PER_WINDOW} every {} seconds.",
                    PLUGIN_NOTIFICATION_WINDOW.as_secs()
                ));
            }
            if !confirm_utools_copy_files(app, &state.host, &request.plugin_id, &requested_paths) {
                return Ok(json!({ "accepted": false, "cancelled": true }));
            }
            let mut seen = HashSet::new();
            let mut prepared = Vec::with_capacity(requested_paths.len());
            for path in requested_paths {
                let item = crate::system_open::prepare_local_open(&path, None)?;
                if !seen.insert(item.path().to_owned()) {
                    return Err("uTools paste file targets resolve to the same local object."
                        .to_owned());
                }
                prepared.push(item);
            }
            let paths = prepared
                .iter()
                .map(|item| item.path().to_owned())
                .collect::<Vec<_>>();
            crate::clipboard_access::with_clipboard(|clipboard| {
                clipboard.set().file_list(&paths)
            })
            .map_err(|error| format!("Could not prepare files for clipboard paste: {error}"))?;
            hide_and_schedule_utools_input(app, UtoolsInputAction::PasteClipboard)?;
            Ok(json!({ "accepted": true, "count": paths.len() }))
        }
        "compatibility.utools.input.typeString" => {
            let value = validate_utools_input_text(
                &request.params,
                MAX_PLUGIN_CLIPBOARD_TEXT_BYTES,
                Some(MAX_UTOOLS_TYPED_TEXT_CHARS),
            )?;
            hide_and_schedule_utools_input(
                app,
                UtoolsInputAction::TypeString(value.to_owned()),
            )?;
            Ok(json!({ "accepted": true }))
        }
        "compatibility.utools.simulate.keyboardTap"
        | "compatibility.utools.simulate.mouseMove"
        | "compatibility.utools.simulate.mouseClick"
        | "compatibility.utools.simulate.mouseDoubleClick"
        | "compatibility.utools.simulate.mouseRightClick" => {
            let action = resolve_utools_simulation_action(&request.method, &request.params)?;
            if !state.host.admit_plugin_notification(&request.plugin_id) {
                return Err(format!(
                    "Interactive uTools simulation prompts are limited to {MAX_PLUGIN_NOTIFICATIONS_PER_WINDOW} every {} seconds.",
                    PLUGIN_NOTIFICATION_WINDOW.as_secs()
                ));
            }
            if !confirm_utools_simulation(
                app,
                &state.host,
                &request.plugin_id,
                &action,
            ) {
                return Ok(json!({ "accepted": false, "cancelled": true }));
            }
            perform_utools_windows_simulation(&action)?;
            Ok(json!({ "accepted": true }))
        }
        "compatibility.utools.window.hideMain" => {
            validate_utools_window_request_params(
                &request.params,
                &["isRestorePreWindow"],
            )?;
            if let Some(value) = request.params.get("isRestorePreWindow") {
                if !value.is_boolean() {
                    return Err("uTools hideMainWindow expects a boolean argument.".to_owned());
                }
            }
            Ok(json!(true))
        }
        "compatibility.utools.window.showMain" => {
            validate_utools_window_request_params(&request.params, &[])?;
            Ok(json!(true))
        }
        "compatibility.utools.window.setHeight" => {
            let height = validate_utools_expend_height(&request.params)?;
            Ok(json!({ "accepted": true, "height": height }))
        }
        "compatibility.utools.window.outPlugin" => {
            validate_utools_window_request_params(&request.params, &["isKill"])?;
            if let Some(value) = request.params.get("isKill") {
                if !value.is_boolean() {
                    return Err("uTools outPlugin expects a boolean argument.".to_owned());
                }
            }
            Ok(json!(true))
        }
        "compatibility.utools.window.redirect" => {
            if !state.host.admit_plugin_notification(&request.plugin_id) {
                return Err(format!(
                    "Interactive uTools navigation is limited to {MAX_PLUGIN_NOTIFICATIONS_PER_WINDOW} requests every {} seconds.",
                    PLUGIN_NOTIFICATION_WINDOW.as_secs()
                ));
            }
            dispatch_utools_redirect(app, state, &request.plugin_id, &request.params)
        }
        "compatibility.utools.mainPush.selectComplete" => {
            validate_exact_plugin_params(&request.params, &["interactionId", "show"])?;
            let interaction_id = required_string(&request.params, "interactionId")?;
            if request.interaction_id.as_deref() != Some(interaction_id) {
                return Err("The uTools main-push completion interaction does not match its bridge envelope."
                    .to_owned());
            }
            claim_utools_main_push_interaction(
                &state.host,
                &request.plugin_id,
                &request.lease_id,
                interaction_id,
            )?;
            let show = request
                .params
                .get("show")
                .and_then(Value::as_bool)
                .ok_or_else(|| "uTools main-push completion requires a boolean show value."
                    .to_owned())?;
            let response = {
                let mut pending = state
                    .host
                    .pending_utools_main_push_selections
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let selection = pending.get_mut(interaction_id).ok_or_else(|| {
                    "This uTools main-push interaction has expired.".to_owned()
                })?;
                if selection.completed {
                    return Err("This uTools main-push selection was already completed.".to_owned());
                }
                selection.completed = true;
                selection.response.clone()
            };
            let _ = response.send(Ok(show));
            Ok(json!({ "accepted": true }))
        }
        "lifecycle.ready" => Ok(json!({ "ok": true })),
        "lifecycle.dispose" => {
            clear_plugin_runtime_state(&state.host, &request.plugin_id);
            emit_plugin_search_providers_changed(app, &request.plugin_id, None, false);
            Ok(json!({ "ok": true }))
        }
        "launcherContext.consume" => {
            let context_id = required_string(&request.params, "contextId")?;
            let payload = take_plugin_launcher_context_transfer(
                &state.host,
                &state.plugins,
                &request.plugin_id,
                &request.lease_id,
                context_id,
            )?;
            serde_json::to_value(payload)
                .map_err(|error| format!("Could not encode launcher context payload: {error}"))
        }
        "commands.complete" => {
            let event_name = format!("ihub://plugin/{}/response", request.plugin_id);
            app.emit(
                &event_name,
                json!({ "method": request.method, "params": request.params }),
            )
            .map_err(|error| format!("Could not forward plugin response: {error}"))?;
            Ok(json!({ "accepted": true }))
        }
        "search.complete" => {
            complete_plugin_search(&state.host, &request.plugin_id, &request.params)?;
            // Keep the existing diagnostic/event channel for integrations that
            // observe command responses, but only after the bounded native
            // request resolver has consumed the correlated payload.
            let event_name = format!("ihub://plugin/{}/response", request.plugin_id);
            app.emit(
                &event_name,
                json!({
                    "method": request.method,
                    "params": {
                        "requestId": request.params.get("requestId"),
                        "ok": request.params.get("ok").and_then(Value::as_bool).unwrap_or(false),
                    },
                }),
            )
            .map_err(|error| format!("Could not forward plugin search response: {error}"))?;
            Ok(json!({ "accepted": true }))
        }
        "filesystem.selectDirectory" => {
            let Some(directory) = select_directory_with_native_dialog(
                app,
                &state.host,
                "Choose a folder for this iHub plugin",
            )? else {
                return Ok(json!({ "cancelled": true }));
            };
            let grant_id =
                issue_filesystem_grant(&state.host, &request.plugin_id, directory.clone())?;
            Ok(json!({
                "cancelled": false,
                "grantId": grant_id,
                "directory": directory,
            }))
        }
        "filesystem.selectFiles" => {
            let Some(files) = select_files_with_native_dialog(
                app,
                &state.host,
                "Choose files for this iHub plugin",
            )?
            else {
                return Ok(json!({ "cancelled": true }));
            };
            let grant_id = issue_file_grant(&state.host, &request.plugin_id, files.clone());
            Ok(json!({
                "cancelled": false,
                "grantId": grant_id,
                "files": files.into_iter().map(|file| json!({
                    "name": file.name,
                    "size": file.size,
                })).collect::<Vec<_>>(),
            }))
        }
        "filesystem.batchRename.preview" => {
            let grant_id = required_string(&request.params, "grantId")?;
            let prepared =
                prepare_directory_for_grant(&state.host, &request.plugin_id, grant_id)?;
            let directory = prepared.path().to_string_lossy().into_owned();
            let find = required_string(&request.params, "find")?.to_owned();
            let replace = required_string(&request.params, "replace")?.to_owned();
            let use_regex = optional_bool(&request.params, "useRegex")?;
            let sequence_start = optional_u32(&request.params, "sequenceStart")?;
            let sequence_padding = optional_u8(&request.params, "sequencePadding")?;
            let preview = crate::builtin_tools::preview_batch_rename(
                directory,
                find,
                replace,
                use_regex,
                sequence_start,
                sequence_padding,
            )?;
            let preview_id = preview
                .can_apply
                .then(|| {
                    remember_plugin_batch_rename_preview(
                        &state.host,
                        &request.plugin_id,
                        grant_id,
                        preview.clone(),
                    )
                })
                .transpose()?;
            Ok(json!({
                "previewId": preview_id,
                "directory": preview.directory,
                "items": preview.items,
                "canApply": preview.can_apply,
                "errors": preview.errors,
            }))
        }
        "filesystem.batchRename.apply" => {
            let grant_id = required_string(&request.params, "grantId")?;
            let preview_id = required_string(&request.params, "previewId")?;
            let prepared =
                prepare_directory_for_grant(&state.host, &request.plugin_id, grant_id)?;
            let preview = take_plugin_batch_rename_preview(
                &state.host,
                &request.plugin_id,
                grant_id,
                preview_id,
            )?;
            let result =
                crate::builtin_tools::apply_batch_rename(preview.directory, preview.items)?;
            drop(prepared);
            serde_json::to_value(result)
                .map_err(|error| format!("Could not encode batch rename result: {error}"))
        }
        "developer.createProject" => {
            let grant_id = required_string(&request.params, "grantId")?;
            let plugin_id = required_string(&request.params, "pluginId")?;
            let project = create_plugin_project_for_grant(
                &state.host,
                &request.plugin_id,
                grant_id,
                plugin_id,
            )?;
            serde_json::to_value(project)
                .map_err(|error| format!("Could not encode the created plugin project: {error}"))
        }
        "native.runCommand" => Err(
            "Native plugin commands must be reserved before the Bridge lock is released."
                .to_owned(),
        ),
        "window.manageLauncher" => {
            let action = required_string(&request.params, "action")?;
            serde_json::to_value(crate::window_management::manage_launcher_window(app, action)?)
                .map_err(|error| format!("Could not encode launcher layout result: {error}"))
        }
        "clipboard.readText" | "clipboard.read" => {
            crate::clipboard_access::with_clipboard(|clipboard| clipboard.get_text())
                .map(Value::String)
                .map_err(|error| format!("Could not read the system clipboard: {error}"))
        }
        "clipboard.writeText"
        | "clipboard.write"
        | "compatibility.utools.clipboard.writeText" => {
            let value = required_string(&request.params, "value")?;
            validate_plugin_clipboard_text(value)?;
            crate::clipboard_access::with_clipboard(|clipboard| clipboard.set_text(value))
                .map_err(|error| format!("Could not write to the system clipboard: {error}"))?;
            Ok(json!({ "written": true }))
        }
        "compatibility.utools.clipboard.writeImage" => {
            let image = decode_authorized_utools_clipboard_image(
                &state.host,
                &request.plugin_id,
                &request.lease_id,
                &request.params,
                "copyImage",
            )?;
            let width = image.width;
            let height = image.height;
            let bytes = image.bytes.into_owned();
            crate::clipboard_access::with_clipboard(|clipboard| {
                clipboard.set_image(arboard::ImageData {
                    width,
                    height,
                    bytes: std::borrow::Cow::Borrowed(bytes.as_ref()),
                })
            })
            .map_err(|error| format!("Could not write the PNG to the system clipboard: {error}"))?;
            Ok(json!({ "written": true, "width": width, "height": height }))
        }
        "compatibility.utools.clipboard.writeFiles" => {
            let requested_paths = validate_utools_copy_file_paths(&request.params)?;
            if !state.host.admit_plugin_notification(&request.plugin_id) {
                return Err(format!(
                    "Interactive uTools alerts are limited to {MAX_PLUGIN_NOTIFICATIONS_PER_WINDOW} every {} seconds.",
                    PLUGIN_NOTIFICATION_WINDOW.as_secs()
                ));
            }
            if !confirm_utools_copy_files(app, &state.host, &request.plugin_id, &requested_paths) {
                return Ok(json!({ "written": false, "cancelled": true }));
            }

            let mut seen = HashSet::new();
            let mut prepared = Vec::with_capacity(requested_paths.len());
            for path in requested_paths {
                let item = crate::system_open::prepare_local_open(&path, None)?;
                if !seen.insert(item.path().to_owned()) {
                    return Err("uTools copyFile targets resolve to the same local object."
                        .to_owned());
                }
                prepared.push(item);
            }
            let paths = prepared
                .iter()
                .map(|item| item.path().to_owned())
                .collect::<Vec<_>>();
            crate::clipboard_access::with_clipboard(|clipboard| {
                clipboard.set().file_list(&paths)
            })
            .map_err(|error| format!("Could not write files to the system clipboard: {error}"))?;
            Ok(json!({ "written": true, "count": paths.len() }))
        }
        "clipboard.history.snapshot" => serde_json::to_value(plugin_clipboard_history_snapshot(
            &state.clipboard_history,
        ))
        .map_err(|error| format!("Could not encode the clipboard history snapshot: {error}")),
        "screenCapture.acquireFocusLease" => {
            let lease_id = state
                .host
                .acquire_plugin_capture_focus_lease(&request.plugin_id);
            Ok(json!({
                "leaseId": lease_id,
                "expiresInMs": CAPTURE_FOCUS_LEASE_TTL.as_millis(),
            }))
        }
        "screenCapture.releaseFocusLease" => {
            let lease_id = required_string(&request.params, "leaseId")?;
            let released = state
                .host
                .release_plugin_capture_focus_lease(&request.plugin_id, lease_id)?;
            Ok(json!({ "released": released }))
        }
        "cursorColor.sampleOnce" => Err(
            "Cursor color samples must be approved by the visible iHub host overlay before this Bridge call runs."
                .to_owned(),
        ),
        "shell.openPath" | "shell.open" => {
            let grant_id = required_string(&request.params, "grantId")?;
            let prepared =
                prepare_directory_for_grant(&state.host, &request.plugin_id, grant_id)?;
            prepared.launch()?;
            Ok(json!({ "opened": true }))
        }
        "shell.openExternal" => {
            open_external_in_system(required_string(&request.params, "url")?)?;
            Ok(json!({ "opened": true }))
        }
        // A plugin's executable process surface is its declared backend
        // worker. `process.spawn` is intentionally not exposed until iHub has
        // a real allow-list executor rather than an acknowledgement-only API.
        "log" => handle_plugin_log_call(&request, state),
        "notifications.show" => {
            show_plugin_notification(app, &request, state, false)
        }
        "compatibility.utools.notification.show" => {
            show_plugin_notification(app, &request, state, true)
        }
        "compatibility.utools.shell.openExternal" => {
            open_external_in_system(required_string(&request.params, "url")?)?;
            Ok(json!({ "opened": true }))
        }
        "compatibility.utools.shell.openPath"
        | "compatibility.utools.shell.trashItem"
        | "compatibility.utools.shell.showItemInFolder" => {
            let path = validate_utools_shell_local_path(
                &request.params,
                request
                    .method
                    .rsplit('.')
                    .next()
                    .unwrap_or("local shell action"),
            )?;
            if !state.host.admit_plugin_notification(&request.plugin_id) {
                return Err(format!(
                    "Interactive uTools alerts are limited to {MAX_PLUGIN_NOTIFICATIONS_PER_WINDOW} every {} seconds.",
                    PLUGIN_NOTIFICATION_WINDOW.as_secs()
                ));
            }
            let (action, warning) = match request.method.as_str() {
                "compatibility.utools.shell.openPath" => ("打开本机项目", "使用系统默认程序打开"),
                "compatibility.utools.shell.showItemInFolder" => {
                    ("在文件管理器中定位", "在文件管理器中定位")
                }
                "compatibility.utools.shell.trashItem" => {
                    ("移到回收站", "把项目移到可恢复的系统回收站")
                }
                _ => unreachable!(),
            };
            if !confirm_utools_local_path_action(
                app,
                &state.host,
                &request.plugin_id,
                action,
                warning,
                &path,
            ) {
                return Ok(json!({ "accepted": false, "cancelled": true }));
            }
            match request.method.as_str() {
                "compatibility.utools.shell.openPath" => {
                    crate::system_open::open_local_path(&path, None)?
                }
                "compatibility.utools.shell.showItemInFolder" => {
                    crate::system_open::show_local_item_in_folder(&path)?
                }
                "compatibility.utools.shell.trashItem" => {
                    crate::system_open::trash_local_item(&path)?
                }
                _ => unreachable!(),
            }
            Ok(json!({ "accepted": true }))
        }
        "compatibility.utools.shell.beep" => {
            if request
                .params
                .as_object()
                .map_or(true, |params| !params.is_empty())
            {
                return Err("uTools shellBeep does not accept parameters.".to_owned());
            }
            if !state.host.admit_plugin_notification(&request.plugin_id) {
                return Err(format!(
                    "Plugin beeps and notifications are limited to {MAX_PLUGIN_NOTIFICATIONS_PER_WINDOW} every {} seconds.",
                    PLUGIN_NOTIFICATION_WINDOW.as_secs()
                ));
            }
            play_utools_system_beep()?;
            Ok(json!({ "played": true }))
        }
        _ => Err(format!(
            "Unsupported plugin host method '{}'.",
            request.method
        )),
    }
}

fn show_plugin_notification(
    app: &AppHandle,
    request: &PluginHostRequest,
    state: &AppState,
    compatibility_body_only: bool,
) -> Result<Value, String> {
    let body = plugin_notification_body(&request.params, compatibility_body_only)?;
    let click_feature_code = compatibility_body_only
        .then(|| utools_notification_click_feature_code(&request.params))
        .transpose()?
        .flatten();
    if !state.host.admit_plugin_notification(&request.plugin_id) {
        return Err(format!(
            "Plugin notifications are limited to {MAX_PLUGIN_NOTIFICATIONS_PER_WINDOW} every {} seconds.",
            PLUGIN_NOTIFICATION_WINDOW.as_secs()
        ));
    }
    if let Some(feature_code) = click_feature_code {
        let _ = resolve_utools_notification_command(
            &state.plugins,
            &state.plugin_settings,
            &request.plugin_id,
            &feature_code,
        )?;
        show_clickable_utools_notification(app, request.plugin_id.clone(), body, feature_code)?;
    } else {
        app.notification()
            .builder()
            .title(format!("iHub · {}", request.plugin_id))
            .body(body)
            .show()
            .map_err(|error| format!("Could not show the system notification: {error}"))?;
    }
    Ok(json!({ "accepted": true }))
}

fn utools_notification_click_feature_code(params: &Value) -> Result<Option<String>, String> {
    let Some(value) = params.get("clickFeatureCode") else {
        return Ok(None);
    };
    let Some(code) = value.as_str() else {
        return Err("uTools notification clickFeatureCode must be a string.".to_owned());
    };
    let code = code.trim();
    if code.is_empty() || code.chars().count() > 160 || code.chars().any(char::is_control) {
        return Err(
            "uTools notification clickFeatureCode must contain 1-160 non-control characters."
                .to_owned(),
        );
    }
    Ok(Some(code.to_owned()))
}

fn resolve_utools_notification_command(
    plugins: &PluginManager,
    settings: &PluginSettingsStore,
    plugin_id: &str,
    feature_code: &str,
) -> Result<String, String> {
    plugins.ensure_plugin_enabled(plugin_id)?;
    let bundle = plugins.frontend_asset_bundle(plugin_id)?;
    let config = bundle.utools_compat.ok_or_else(|| {
        "Notification click routing requires a verified uTools package.".to_owned()
    })?;
    if let Some(command) = config
        .commands
        .into_iter()
        .find(|command| command.code == feature_code)
    {
        return Ok(command.command_id);
    }
    if let Some(feature) = utools_dynamic_features(settings, plugin_id)
        .into_iter()
        .find(|feature| {
            feature.code == feature_code && utools_dynamic_feature_matches_platform(feature)
        })
    {
        return Ok(utools_dynamic_feature_command_id(&feature.code));
    }
    Err(format!(
        "uTools notification feature '{feature_code}' is not currently declared by this plugin."
    ))
}

#[cfg(windows)]
fn show_clickable_utools_notification(
    app: &AppHandle,
    plugin_id: String,
    body: String,
    feature_code: String,
) -> Result<(), String> {
    let mut notification = notify_rust::Notification::new();
    notification
        .summary(&format!("iHub · {plugin_id}"))
        .body(&body)
        .auto_icon();
    let executable_directory = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf));
    let is_cargo_output = executable_directory.as_ref().is_some_and(|directory| {
        directory.ends_with(Path::new("target/debug"))
            || directory.ends_with(Path::new("target/release"))
    });
    if !is_cargo_output {
        notification.app_id(&app.config().identifier);
    }
    let handle = notification
        .show()
        .map_err(|error| format!("Could not show the clickable Windows notification: {error}"))?;
    let callback_app = app.clone();
    std::thread::Builder::new()
        .name("ihub-utools-notification".to_owned())
        .spawn(move || {
            let result =
                handle.wait_for_response(move |response: &notify_rust::NotificationResponse| {
                    if matches!(
                        response,
                        notify_rust::NotificationResponse::Default
                            | notify_rust::NotificationResponse::Action(_)
                    ) {
                        dispatch_utools_notification_click(
                            &callback_app,
                            &plugin_id,
                            &feature_code,
                        );
                    }
                });
            if let Err(error) = result {
                host_log::warn(
                    "plugins",
                    format!("Could not wait for a uTools notification response: {error}"),
                );
            }
        })
        .map(|_| ())
        .map_err(|error| format!("Could not start the notification response worker: {error}"))
}

#[cfg(not(windows))]
fn show_clickable_utools_notification(
    _app: &AppHandle,
    _plugin_id: String,
    _body: String,
    _feature_code: String,
) -> Result<(), String> {
    Err("uTools notification click routing has been runtime-verified on Windows only.".to_owned())
}

fn dispatch_utools_notification_click(app: &AppHandle, plugin_id: &str, feature_code: &str) {
    let state = app.state::<AppState>();
    let command_id = match resolve_utools_notification_command(
        &state.plugins,
        &state.plugin_settings,
        plugin_id,
        feature_code,
    ) {
        Ok(command_id) => command_id,
        Err(error) => {
            host_log::warn(
                "plugins",
                format!("Ignored stale uTools notification activation: {error}"),
            );
            return;
        }
    };
    show_launcher(app);
    let payload = PluginShortcutEvent {
        plugin_id: plugin_id.to_owned(),
        shortcut: "notification".to_owned(),
        command_id: Some(command_id),
        keyword: None,
        input: None,
    };
    if let Err(error) = app.emit_to("main", "ihub://plugin-global-shortcut", payload) {
        host_log::warn(
            "plugins",
            format!("Could not route a uTools notification activation: {error}"),
        );
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UtoolsRedirectCandidate {
    plugin_id: String,
    command_id: String,
    plugin_name: String,
    command_name: String,
}

#[derive(Clone, Debug, Serialize)]
struct UtoolsRedirectAction {
    #[serde(rename = "type")]
    kind: String,
    payload: Value,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UtoolsRedirectEvent {
    source_plugin_id: String,
    label: String,
    candidates: Vec<UtoolsRedirectCandidate>,
    action: UtoolsRedirectAction,
}

fn bounded_utools_redirect_label(value: &Value, field: &str) -> Result<String, String> {
    let label = value
        .as_str()
        .ok_or_else(|| format!("uTools redirect {field} must be a string."))?
        .trim();
    if label.is_empty()
        || label.chars().count() > 160
        || label.len() > 1024
        || label.chars().any(char::is_control)
    {
        return Err(format!(
            "uTools redirect {field} must contain 1-160 non-control characters."
        ));
    }
    Ok(label.to_owned())
}

fn validate_utools_redirect_request(
    params: &Value,
) -> Result<(Option<String>, String, UtoolsRedirectAction), String> {
    let object = params
        .as_object()
        .ok_or_else(|| "uTools redirect parameters must be an object.".to_owned())?;
    if object.len() != 2 || !object.contains_key("label") || !object.contains_key("action") {
        return Err("uTools redirect accepts exactly label and action.".to_owned());
    }
    let (plugin_name, command_label) = match object.get("label") {
        Some(Value::String(_)) => (
            None,
            bounded_utools_redirect_label(&object["label"], "label")?,
        ),
        Some(Value::Array(labels)) if labels.len() == 2 => (
            Some(bounded_utools_redirect_label(&labels[0], "plugin name")?),
            bounded_utools_redirect_label(&labels[1], "command label")?,
        ),
        _ => {
            return Err("uTools redirect label must be a string or a two-string array.".to_owned())
        }
    };
    let action = object
        .get("action")
        .and_then(Value::as_object)
        .ok_or_else(|| "uTools redirect action must be an object.".to_owned())?;
    if action.len() != 2 || !action.contains_key("type") || !action.contains_key("payload") {
        return Err("uTools redirect action accepts exactly type and payload.".to_owned());
    }
    let kind = action
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| "uTools redirect action type must be a string.".to_owned())?;
    let payload = match kind {
        "text" => {
            let text = action
                .get("payload")
                .and_then(Value::as_str)
                .ok_or_else(|| "uTools redirect text payload must be a string.".to_owned())?;
            if text.len() > MAX_PLUGIN_CLIPBOARD_TEXT_BYTES || text.contains('\0') {
                return Err(format!(
                    "uTools redirect text is limited to {MAX_PLUGIN_CLIPBOARD_TEXT_BYTES} UTF-8 bytes and cannot contain NUL."
                ));
            }
            Value::String(text.to_owned())
        }
        "img" => {
            let data_url = action
                .get("payload")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    "uTools redirect image payload must be a PNG Data URL.".to_owned()
                })?;
            let _ = decode_utools_clipboard_png_data_url(data_url)?;
            Value::String(data_url.to_owned())
        }
        "files" => {
            let values = action
                .get("payload")
                .and_then(Value::as_array)
                .ok_or_else(|| "uTools redirect files payload must be an array.".to_owned())?;
            let paths = validate_utools_copy_file_paths(&json!({ "paths": values }))?;
            Value::Array(
                paths
                    .into_iter()
                    .map(|path| Value::String(path.to_string_lossy().into_owned()))
                    .collect(),
            )
        }
        _ => return Err("uTools redirect action type must be text, img, or files.".to_owned()),
    };
    Ok((
        plugin_name,
        command_label,
        UtoolsRedirectAction {
            kind: kind.to_owned(),
            payload,
        },
    ))
}

fn resolve_utools_redirect_candidates(
    plugins: &PluginManager,
    settings: &PluginSettingsStore,
    plugin_name: Option<&str>,
    command_label: &str,
) -> Result<Vec<UtoolsRedirectCandidate>, String> {
    const MAX_REDIRECT_CANDIDATES: usize = 32;
    let mut plugin_infos = plugins.list();
    project_utools_dynamic_features(plugins, settings, &mut plugin_infos);
    let mut candidates = Vec::new();
    for plugin in plugin_infos {
        if !plugin.enabled
            || !plugins
                .uses_utools_compatibility(&plugin.id)
                .unwrap_or(false)
            || plugin_name.is_some_and(|expected| !plugin.name.eq_ignore_ascii_case(expected))
        {
            continue;
        }
        for command in plugin.commands {
            let matches_label = command.name.eq_ignore_ascii_case(command_label)
                || command
                    .keywords
                    .iter()
                    .any(|keyword| keyword.eq_ignore_ascii_case(command_label));
            if !matches_label {
                continue;
            }
            if candidates.len() >= MAX_REDIRECT_CANDIDATES {
                return Err(format!(
                    "uTools redirect matched more than {MAX_REDIRECT_CANDIDATES} installed commands."
                ));
            }
            candidates.push(UtoolsRedirectCandidate {
                plugin_id: plugin.id.clone(),
                command_id: command.id,
                plugin_name: plugin.name.clone(),
                command_name: command.name,
            });
        }
    }
    candidates.sort_by(|left, right| {
        left.plugin_name
            .to_lowercase()
            .cmp(&right.plugin_name.to_lowercase())
            .then_with(|| {
                left.command_name
                    .to_lowercase()
                    .cmp(&right.command_name.to_lowercase())
            })
            .then_with(|| left.plugin_id.cmp(&right.plugin_id))
            .then_with(|| left.command_id.cmp(&right.command_id))
    });
    if candidates.is_empty() {
        return Err(match plugin_name {
            Some(plugin_name) => format!(
                "No enabled uTools-compatible command named '{command_label}' exists in plugin '{plugin_name}'."
            ),
            None => format!(
                "No enabled uTools-compatible command named '{command_label}' is installed."
            ),
        });
    }
    Ok(candidates)
}

fn dispatch_utools_redirect(
    app: &AppHandle,
    state: &AppState,
    source_plugin_id: &str,
    params: &Value,
) -> Result<Value, String> {
    if !state.plugins.uses_utools_compatibility(source_plugin_id)? {
        return Err("uTools redirect requires a verified uTools source package.".to_owned());
    }
    let (plugin_name, command_label, action) = validate_utools_redirect_request(params)?;
    let candidates = resolve_utools_redirect_candidates(
        &state.plugins,
        &state.plugin_settings,
        plugin_name.as_deref(),
        &command_label,
    )?;
    show_launcher(app);
    app.emit_to(
        "main",
        "ihub://utools-redirect",
        UtoolsRedirectEvent {
            source_plugin_id: source_plugin_id.to_owned(),
            label: command_label,
            candidates,
            action,
        },
    )
    .map_err(|error| format!("Could not route the uTools plugin redirect: {error}"))?;
    Ok(json!(true))
}

fn plugin_notification_body(
    params: &Value,
    compatibility_body_only: bool,
) -> Result<String, String> {
    let Some(object) = params.as_object() else {
        return Err("Plugin notification parameters must be an object.".to_owned());
    };
    let allowed_keys: &[&str] = if compatibility_body_only {
        &["body", "clickFeatureCode"]
    } else {
        &["title", "body", "level"]
    };
    if object
        .keys()
        .any(|key| !allowed_keys.contains(&key.as_str()))
    {
        return Err("Plugin notification parameters contain unsupported fields.".to_owned());
    }

    if compatibility_body_only {
        let body = required_string(params, "body")?.trim();
        validate_plugin_notification_text(body, "body", MAX_PLUGIN_NOTIFICATION_BODY_CHARS)?;
        return Ok(body.to_owned());
    }

    let title = required_string(params, "title")?.trim();
    validate_plugin_notification_text(title, "title", MAX_PLUGIN_NOTIFICATION_TITLE_CHARS)?;
    let body = match params.get("body") {
        None => None,
        Some(Value::String(body)) => {
            let body = body.trim();
            if body.is_empty() {
                None
            } else {
                validate_plugin_notification_text(
                    body,
                    "body",
                    MAX_PLUGIN_NOTIFICATION_BODY_CHARS,
                )?;
                Some(body)
            }
        }
        Some(_) => return Err("Plugin notification body must be a string.".to_owned()),
    };
    if let Some(level) = params.get("level") {
        let Some(level) = level.as_str() else {
            return Err("Plugin notification level must be a string.".to_owned());
        };
        if !matches!(level, "info" | "success" | "warning" | "error") {
            return Err("Plugin notification level is unsupported.".to_owned());
        }
    }
    Ok(match body {
        Some(body) => format!("{title}\n{body}"),
        None => title.to_owned(),
    })
}

fn validate_plugin_notification_text(
    value: &str,
    field: &str,
    max_chars: usize,
) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("Plugin notification {field} must not be empty."));
    }
    if value.chars().count() > max_chars {
        return Err(format!(
            "Plugin notification {field} exceeds the {max_chars}-character limit."
        ));
    }
    Ok(())
}

fn ensure_plugin_host_request_is_allowed(
    request: &PluginHostRequest,
    state: &AppState,
) -> Result<(), String> {
    state.plugins.ensure_plugin_enabled(&request.plugin_id)?;
    if request.method.starts_with("compatibility.utools.")
        && !state
            .plugins
            .uses_utools_compatibility(&request.plugin_id)?
    {
        return Err(
            "uTools compatibility host methods are available only to validated imported uTools packages."
                .to_owned(),
        );
    }
    let is_utools_input = request.method.starts_with("compatibility.utools.input.");
    let is_utools_simulation = request.method.starts_with("compatibility.utools.simulate.");
    let has_main_push_interaction = !request.surface
        && is_utools_input
        && request
            .interaction_id
            .as_deref()
            .is_some_and(|interaction_id| {
                claim_utools_main_push_interaction(
                    &state.host,
                    &request.plugin_id,
                    &request.lease_id,
                    interaction_id,
                )
                .is_ok()
            });
    if (request.method.starts_with("compatibility.utools.window.")
        || is_utools_input
        || request
            .method
            .starts_with("compatibility.utools.shell.openPath")
        || request
            .method
            .starts_with("compatibility.utools.shell.trashItem")
        || request
            .method
            .starts_with("compatibility.utools.shell.showItemInFolder")
        || request.method == "compatibility.utools.system.readCurrentFolderPath"
        || request.method == "compatibility.utools.system.readCurrentBrowserUrl"
        || request.method == "compatibility.utools.window.redirect"
        || request.method == "compatibility.utools.clipboard.writeFiles"
        || is_utools_simulation)
        && !request.surface
        && !has_main_push_interaction
    {
        return Err(
            "uTools window, input, simulation, and confirmed file-copy methods require the plugin's visible active surface."
                .to_owned(),
        );
    }
    if request.method == "cursorColor.sampleOnce" && !request.surface {
        return Err(
            "Cursor color sampling is available only from the plugin's visible active surface."
                .to_owned(),
        );
    }
    if let Some(permission) = PluginManager::required_permission_for_host_method(&request.method) {
        let allowed = state
            .plugins
            .allows_host_method(&request.plugin_id, &request.method)?;
        if !allowed {
            return Err(format!(
                "Plugin '{}' is not allowed to call '{}'. Declare {} in its v1 plugin manifest, then reinstall or update the plugin.",
                request.plugin_id, request.method, permission
            ));
        }
    }
    Ok(())
}

fn claim_utools_main_push_interaction(
    host: &PluginHostState,
    plugin_id: &str,
    lease_id: &str,
    interaction_id: &str,
) -> Result<(), String> {
    if interaction_id.is_empty()
        || interaction_id.len() > 128
        || interaction_id.chars().any(char::is_control)
    {
        return Err("The uTools main-push interaction ID is invalid.".to_owned());
    }
    let mut pending = host
        .pending_utools_main_push_selections
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let selection = pending
        .get_mut(interaction_id)
        .ok_or_else(|| "This uTools main-push interaction has expired.".to_owned())?;
    if selection.plugin_id != plugin_id || selection.completed {
        return Err("This uTools main-push interaction is unavailable.".to_owned());
    }
    match selection.lease_id.as_deref() {
        Some(owner) if owner != lease_id => {
            return Err(
                "This uTools main-push interaction belongs to another plugin session.".to_owned(),
            );
        }
        None => selection.lease_id = Some(lease_id.to_owned()),
        Some(_) => {}
    }
    Ok(())
}

fn detached_plugin_event_target(
    app: &AppHandle,
    state: &AppState,
    plugin_id: &str,
) -> Result<Option<String>, String> {
    let detached = app.state::<DetachedPluginWindowRegistry>();
    let Some((label, lease_id)) = detached.window_label_and_lease_for_plugin(plugin_id) else {
        return if detached.plugin_is_detached(plugin_id) {
            Err(format!(
                "Plugin '{plugin_id}' detached window is still loading; try the action again."
            ))
        } else {
            Ok(None)
        };
    };
    if !state
        .plugin_assets
        .is_active_surface_for(&lease_id, plugin_id)
    {
        return Err(format!(
            "Plugin '{plugin_id}' detached surface lease is no longer active."
        ));
    }
    if app.get_webview_window(&label).is_none() {
        return Err(format!(
            "Plugin '{plugin_id}' detached window is no longer available."
        ));
    }
    Ok(Some(label))
}

fn emit_plugin_event_to_owner(
    app: &AppHandle,
    state: &AppState,
    plugin_id: &str,
    event_name: &str,
    payload: Value,
) -> Result<(), String> {
    let target =
        detached_plugin_event_target(app, state, plugin_id)?.unwrap_or_else(|| "main".to_owned());
    app.emit_to(&target, event_name, payload)
        .map_err(|error| format!("Could not deliver plugin event: {error}"))
}

fn resolve_issued_plugin_search_selection(
    host: &PluginHostState,
    plugin_id: &str,
    provider_id: &str,
    request_id: &str,
    result_id: &str,
    now: Instant,
) -> Result<Value, String> {
    let mut issued = host
        .issued_search_results
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    remove_expired_issued_plugin_searches(&mut issued, now);
    let search = issued.get(request_id).ok_or_else(|| {
        "This plugin search result has expired; search again before selecting it.".to_owned()
    })?;
    if search.plugin_id != plugin_id || search.provider_id != provider_id {
        return Err("This plugin search result belongs to another provider.".to_owned());
    }
    let payload = search
        .results
        .iter()
        .find(|result| result.id == result_id)
        .map(|result| result.payload.clone().unwrap_or(Value::Null))
        .ok_or_else(|| {
            "This plugin search result no longer exists in its issued response.".to_owned()
        })?;
    // Selection is one-shot. Removing the snapshot while the same mutex is
    // held prevents two concurrent launcher activations from delivering the
    // same plugin result twice.
    issued.remove(request_id);
    Ok(payload)
}

#[tauri::command]
pub fn dispatch_detached_plugin_frontend_event(
    app: AppHandle,
    window: tauri::WebviewWindow,
    request: DetachedPluginFrontendEventRequest,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    if window.label() != "main" {
        return Err("Only the trusted main iHub surface can dispatch launcher events.".to_owned());
    }

    let plugin_id = match &request {
        DetachedPluginFrontendEventRequest::Command { plugin_id, .. }
        | DetachedPluginFrontendEventRequest::SearchSelection { plugin_id, .. } => plugin_id,
    };
    if !is_plugin_id(plugin_id) {
        return Err("Invalid plugin ID.".to_owned());
    }
    state.plugins.ensure_plugin_enabled(plugin_id)?;
    let Some(target) = detached_plugin_event_target(&app, &state, plugin_id)? else {
        return Ok(false);
    };

    let (event_name, payload) = match request {
        DetachedPluginFrontendEventRequest::Command {
            plugin_id,
            command_id,
        } => {
            if !is_plugin_id(&command_id) {
                return Err("Invalid plugin command ID.".to_owned());
            }
            let plugin = state
                .plugins
                .list()
                .into_iter()
                .find(|plugin| plugin.enabled && plugin.id == plugin_id)
                .ok_or_else(|| format!("Plugin '{plugin_id}' is not available."))?;
            let command = plugin
                .commands
                .iter()
                .find(|command| command.id == command_id)
                .ok_or_else(|| {
                    format!("Plugin command '{plugin_id}/{command_id}' no longer exists.")
                })?;
            if command.execution != "frontend"
                && (plugin.frontend_entry.is_none() || plugin.has_native_worker)
            {
                return Err(
                    "Native plugin commands must use the launcher's reviewed worker path."
                        .to_owned(),
                );
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
            (
                format!("ihub://plugin/{plugin_id}/command"),
                json!({
                    "requestId": request_id,
                    "commandId": command_id,
                    "input": Value::Null,
                    "context": Value::Null,
                }),
            )
        }
        DetachedPluginFrontendEventRequest::SearchSelection {
            plugin_id,
            provider_id,
            request_id,
            result_id,
        } => {
            if !is_plugin_id(&provider_id)
                || request_id.is_empty()
                || request_id.len() > 128
                || request_id.chars().any(char::is_control)
                || result_id.trim().is_empty()
                || result_id.chars().count() > 160
            {
                return Err("Invalid plugin search selection.".to_owned());
            }
            if !has_declared_plugin_search_provider(&state, &plugin_id, &provider_id)?
                || !state
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
            let selected_payload = resolve_issued_plugin_search_selection(
                &state.host,
                &plugin_id,
                &provider_id,
                &request_id,
                &result_id,
                Instant::now(),
            )?;
            let selection_request_id = next_request_id();
            (
                format!("ihub://plugin/{plugin_id}/event/search.select"),
                json!({
                    "requestId": selection_request_id,
                    "providerId": provider_id,
                    "resultId": result_id,
                    "payload": selected_payload,
                }),
            )
        }
    };

    app.emit_to(&target, &event_name, payload)
        .map_err(|error| format!("Could not deliver detached plugin event: {error}"))?;
    if let Some(detached_window) = app.get_webview_window(&target) {
        let _ = detached_window.unminimize();
        let _ = detached_window.show();
        let _ = detached_window.set_focus();
    }
    Ok(true)
}

// Tauri exposes these as individually named IPC arguments. Wrapping them in a
// request object would change the existing frontend command ABI.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub fn invoke_plugin_frontend_command(
    app: AppHandle,
    plugin_id: String,
    command_id: String,
    input: Option<Value>,
    context: Option<Value>,
    // An opaque host-issued ID from `issue_plugin_launcher_context`. It is
    // optional so ordinary commands preserve their existing invocation API.
    launcher_context_id: Option<String>,
    // Required when a launcher-context ID is attached. It binds dispatch to
    // the exact live visible iframe that registered this frontend command.
    frontend_lease_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    if !is_plugin_id(&plugin_id) || !is_plugin_id(&command_id) {
        return Err("Invalid plugin or command ID.".to_owned());
    }
    let context_dispatch = launcher_context_id.is_some();
    let plugin_assets = state.plugin_assets.clone();
    let dispatch = || {
        let context_lease_id = if context_dispatch {
            let lease_id = frontend_lease_id.as_deref().ok_or_else(|| {
                "A launcher-context command requires the active frontend lease.".to_owned()
            })?;
            if !plugin_assets.is_active_surface_for(lease_id, &plugin_id) {
                return Err("The plugin surface changed before this launcher context was dispatched. Choose the action again.".to_owned());
            }
            Some(lease_id)
        } else {
            None
        };
        state.plugins.ensure_plugin_enabled(&plugin_id)?;
        let runtime_registered = state
            .host
            .commands
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(&host_key(&plugin_id, &command_id));
        if !runtime_registered {
            state
                .plugins
                .ensure_frontend_command(&plugin_id, &command_id)?;
            if !state.plugins.uses_utools_compatibility(&plugin_id)? {
                return Err(format!(
                    "Plugin command '{plugin_id}/{command_id}' is not registered."
                ));
            }
        }
        let request_id = next_request_id();
        let launcher_context = launcher_context_id
            .as_deref()
            .map(|context_id| {
                let lease_id = context_lease_id
                    .expect("launcher-context dispatches require a validated frontend lease");
                attach_plugin_launcher_context_transfer(
                    &state.host,
                    &plugin_id,
                    &command_id,
                    lease_id,
                    &request_id,
                    context_id,
                )
            })
            .transpose()?;
        let event_name = format!("ihub://plugin/{plugin_id}/command");
        if let Err(error) = emit_plugin_event_to_owner(
            &app,
            &state,
            &plugin_id,
            &event_name,
            json!({
                "requestId": request_id,
                "commandId": command_id,
                "input": input,
                "context": context,
                "launcherContext": launcher_context,
            }),
        ) {
            if let Some(context_id) = launcher_context_id.as_deref() {
                // The event was never delivered, so remove the attached
                // payload immediately instead of relying on TTL cleanup.
                let _ =
                    revoke_plugin_launcher_context_transfer(&state.host, &plugin_id, context_id);
            }
            return Err(format!("Could not invoke plugin command: {error}"));
        }
        Ok(request_id)
    };

    if context_dispatch {
        plugin_assets.with_plugin_operation(&plugin_id, dispatch)
    } else {
        dispatch()
    }
}

#[tauri::command]
pub fn list_registered_plugin_search_providers(
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
) -> Result<Vec<RegisteredPluginSearchProvider>, String> {
    if window.label() != "main" {
        return Err(
            "Only the trusted main iHub surface can inspect provider readiness.".to_owned(),
        );
    }
    let mut plugins = state.plugins.list();
    project_utools_dynamic_features(&state.plugins, &state.plugin_settings, &mut plugins);
    let declared = plugins
        .iter()
        .filter(|plugin| plugin.enabled)
        .flat_map(|plugin| {
            plugin
                .search_providers
                .iter()
                .map(|provider| host_key(&plugin.id, &provider.id))
        })
        .collect::<HashSet<_>>();
    let mut providers = state
        .host
        .search_providers
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .keys()
        .filter(|key| declared.contains(*key))
        .filter_map(|key| {
            key.split_once(':')
                .map(|(plugin_id, provider_id)| RegisteredPluginSearchProvider {
                    plugin_id: plugin_id.to_owned(),
                    provider_id: provider_id.to_owned(),
                })
        })
        .collect::<Vec<_>>();
    providers.sort_by(|left, right| {
        left.plugin_id
            .cmp(&right.plugin_id)
            .then_with(|| left.provider_id.cmp(&right.provider_id))
    });
    Ok(providers)
}

#[tauri::command]
pub async fn query_plugin_search(
    app: AppHandle,
    plugin_id: String,
    provider_id: String,
    query: String,
    limit: Option<usize>,
    context: Option<Value>,
    state: State<'_, AppState>,
) -> Result<PluginSearchResponse, String> {
    if !is_plugin_id(&plugin_id) || !is_plugin_id(&provider_id) {
        return Err("Invalid plugin or search provider ID.".to_owned());
    }
    state.plugins.ensure_plugin_enabled(&plugin_id)?;
    let query = query.trim().to_owned();
    if query.is_empty() {
        return Ok(PluginSearchResponse {
            request_id: next_request_id(),
            plugin_id,
            provider_id,
            results: Vec::new(),
        });
    }
    if query.len() > MAX_PLUGIN_SEARCH_QUERY_BYTES {
        return Err(format!(
            "Plugin search queries are limited to {MAX_PLUGIN_SEARCH_QUERY_BYTES} bytes."
        ));
    }
    if !state
        .host
        .search_providers
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains_key(&host_key(&plugin_id, &provider_id))
    {
        emit_plugin_search_providers_changed(&app, &plugin_id, Some(&provider_id), false);
        return Err(format!(
            "Plugin search provider '{plugin_id}/{provider_id}' is not registered."
        ));
    }
    let request_id = next_request_id();
    let max_results = limit.unwrap_or(3).clamp(1, MAX_PLUGIN_SEARCH_RESULTS);
    let (response_sender, response_receiver) = mpsc::sync_channel(1);
    {
        let mut pending = state
            .host
            .pending_searches
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if pending.len() >= MAX_PENDING_PLUGIN_SEARCHES {
            return Err("Plugin search is busy; try again shortly.".to_owned());
        }
        pending.insert(
            request_id.clone(),
            PendingPluginSearch {
                plugin_id: plugin_id.clone(),
                provider_id: provider_id.clone(),
                max_results,
                response: response_sender,
            },
        );
    }

    let event_name = format!("ihub://plugin/{plugin_id}/search");
    if let Err(error) = emit_plugin_event_to_owner(
        &app,
        &state,
        &plugin_id,
        &event_name,
        json!({
            "requestId": request_id.clone(),
            "providerId": provider_id.clone(),
            "query": query,
            "limit": max_results,
            "context": context,
        }),
    ) {
        state
            .host
            .pending_searches
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&request_id);
        return Err(format!("Could not query plugin search provider: {error}"));
    }

    let wait = tauri::async_runtime::spawn_blocking(move || {
        response_receiver.recv_timeout(PLUGIN_SEARCH_TIMEOUT)
    })
    .await
    .map_err(|error| format!("Plugin search wait task failed: {error}"))?;
    // Completion removes its own entry first. Removal here handles timeouts,
    // cancellation, and a frontend that was closed before it could respond.
    state
        .host
        .pending_searches
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&request_id);

    let results = match wait {
        Ok(Ok(results)) => results,
        Ok(Err(error)) => return Err(error),
        Err(RecvTimeoutError::Timeout) => {
            return Err(format!(
                "Plugin search provider '{plugin_id}/{provider_id}' did not respond within {} ms.",
                PLUGIN_SEARCH_TIMEOUT.as_millis()
            ));
        }
        Err(RecvTimeoutError::Disconnected) => {
            return Err("Plugin search provider stopped before responding.".to_owned());
        }
    };

    {
        let now = Instant::now();
        let mut issued = state
            .host
            .issued_search_results
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        remove_expired_issued_plugin_searches(&mut issued, now);
        issued.insert(
            request_id.clone(),
            IssuedPluginSearchResults {
                plugin_id: plugin_id.clone(),
                provider_id: provider_id.clone(),
                results: results.clone(),
                issued_at: now,
            },
        );
        trim_oldest_records(&mut issued, MAX_ISSUED_PLUGIN_SEARCHES, |search| {
            search.issued_at
        });
    }

    Ok(PluginSearchResponse {
        request_id,
        plugin_id,
        provider_id,
        results,
    })
}

fn normalize_utools_main_push_selection(value: &Value) -> Result<(String, Value, Value), String> {
    let object = value
        .as_object()
        .ok_or_else(|| "uTools main-push selection payload must be an object.".to_owned())?;
    if object.len() != 3
        || object.get("kind").and_then(Value::as_str) != Some("utoolsMainPush")
        || !object.contains_key("action")
        || !object.contains_key("option")
    {
        return Err("The selected search result is not a uTools main-push option.".to_owned());
    }
    let action = object
        .get("action")
        .and_then(Value::as_object)
        .ok_or_else(|| "uTools main-push action must be an object.".to_owned())?;
    if action.len() != 3 || action.get("type").and_then(Value::as_str) != Some("text") {
        return Err(
            "This iHub compatibility stage accepts only bounded text main-push actions.".to_owned(),
        );
    }
    let code = action
        .get("code")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|code| {
            !code.is_empty() && code.chars().count() <= 160 && !code.chars().any(char::is_control)
        })
        .ok_or_else(|| "uTools main-push action code is invalid.".to_owned())?
        .to_owned();
    let payload = action
        .get("payload")
        .and_then(Value::as_str)
        .filter(|payload| {
            !payload.is_empty()
                && payload.len() <= MAX_PLUGIN_SEARCH_QUERY_BYTES
                && !payload.contains('\0')
        })
        .ok_or_else(|| "uTools main-push text payload is invalid.".to_owned())?;
    let option = object
        .get("option")
        .and_then(Value::as_object)
        .ok_or_else(|| "uTools main-push option must be an object.".to_owned())?;
    let text = option
        .get("text")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty() && text.chars().count() <= 320)
        .ok_or_else(|| "uTools main-push option.text is invalid.".to_owned())?;
    if text.chars().any(char::is_control) {
        return Err("uTools main-push option.text contains control characters.".to_owned());
    }
    for (field, limit) in [("title", 320_usize), ("icon", 2_048_usize)] {
        if let Some(value) = option.get(field) {
            let value = value.as_str().ok_or_else(|| {
                format!("uTools main-push option.{field} must be a string when provided.")
            })?;
            if value.chars().count() > limit || value.chars().any(char::is_control) {
                return Err(format!("uTools main-push option.{field} is invalid."));
            }
        }
    }
    let mut enter_action = serde_json::Map::new();
    enter_action.insert("code".to_owned(), Value::String(code.clone()));
    enter_action.insert("type".to_owned(), Value::String("text".to_owned()));
    enter_action.insert("payload".to_owned(), Value::String(payload.to_owned()));
    enter_action.insert("from".to_owned(), Value::String("main".to_owned()));
    enter_action.insert("option".to_owned(), Value::Object(option.clone()));
    Ok((
        code,
        Value::Object(enter_action),
        Value::Object(option.clone()),
    ))
}

fn resolve_utools_main_push_command(
    state: &AppState,
    plugin_id: &str,
    code: &str,
) -> Result<String, String> {
    state.plugins.ensure_plugin_enabled(plugin_id)?;
    let bundle = state.plugins.frontend_asset_bundle(plugin_id)?;
    let config = bundle
        .utools_compat
        .ok_or_else(|| "Main-push selection requires a verified uTools package.".to_owned())?;
    if let Some(command) = config
        .commands
        .into_iter()
        .find(|command| command.main_push && command.code == code)
    {
        return Ok(command.command_id);
    }
    if let Some(feature) = utools_dynamic_features(&state.plugin_settings, plugin_id)
        .into_iter()
        .find(|feature| {
            feature.code == code
                && feature.main_push == Some(true)
                && utools_dynamic_feature_matches_platform(feature)
        })
    {
        return Ok(utools_dynamic_feature_command_id(&feature.code));
    }
    Err(format!(
        "uTools main-push feature '{code}' is not currently declared by this plugin."
    ))
}

#[tauri::command]
pub async fn select_utools_main_push_result(
    app: AppHandle,
    window: tauri::WebviewWindow,
    plugin_id: String,
    provider_id: String,
    request_id: String,
    result_id: String,
    state: State<'_, AppState>,
) -> Result<UtoolsMainPushSelectionResult, String> {
    if window.label() != "main" {
        return Err("Only the trusted main iHub surface can select main-push results.".to_owned());
    }
    if provider_id != UTOOLS_MAIN_PUSH_PROVIDER_ID
        || !is_plugin_id(&plugin_id)
        || request_id.is_empty()
        || request_id.len() > 128
        || request_id.chars().any(char::is_control)
        || result_id.trim().is_empty()
        || result_id.chars().count() > 160
    {
        return Err("Invalid uTools main-push selection.".to_owned());
    }
    if !has_declared_plugin_search_provider(&state, &plugin_id, &provider_id)?
        || !state
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
    let selected_payload = resolve_issued_plugin_search_selection(
        &state.host,
        &plugin_id,
        &provider_id,
        &request_id,
        &result_id,
        Instant::now(),
    )?;
    let (feature_code, enter_action, option) =
        normalize_utools_main_push_selection(&selected_payload)?;
    let command_id = resolve_utools_main_push_command(&state, &plugin_id, &feature_code)?;
    let interaction_id = next_request_id();
    let (response_sender, response_receiver) = mpsc::sync_channel(1);
    {
        let mut pending = state
            .host
            .pending_utools_main_push_selections
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if pending.len() >= MAX_PENDING_UTOOLS_MAIN_PUSH_SELECTIONS {
            return Err("uTools main-push selection is busy; try again shortly.".to_owned());
        }
        pending.insert(
            interaction_id.clone(),
            PendingUtoolsMainPushSelection {
                plugin_id: plugin_id.clone(),
                lease_id: None,
                completed: false,
                response: response_sender,
            },
        );
    }
    let selection_event = format!("ihub://plugin/{plugin_id}/event/search.select");
    if let Err(error) = emit_plugin_event_to_owner(
        &app,
        &state,
        &plugin_id,
        &selection_event,
        json!({
            "requestId": next_request_id(),
            "providerId": provider_id,
            "resultId": result_id,
            "payload": selected_payload,
            "interactionId": interaction_id,
        }),
    ) {
        state
            .host
            .pending_utools_main_push_selections
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&interaction_id);
        return Err(format!(
            "Could not deliver uTools main-push selection: {error}"
        ));
    }
    let wait = tauri::async_runtime::spawn_blocking(move || {
        response_receiver.recv_timeout(UTOOLS_MAIN_PUSH_SELECTION_TIMEOUT)
    })
    .await
    .map_err(|error| format!("uTools main-push selection wait task failed: {error}"))?;
    state
        .host
        .pending_utools_main_push_selections
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&interaction_id);
    let show = match wait {
        Ok(Ok(show)) => show,
        Ok(Err(error)) => return Err(error),
        Err(RecvTimeoutError::Timeout) => {
            return Err("The uTools onMainPush selection callback timed out.".to_owned());
        }
        Err(RecvTimeoutError::Disconnected) => {
            return Err("The uTools onMainPush runtime stopped before responding.".to_owned());
        }
    };

    let mut opened_detached = false;
    if show {
        if let Some(target) = detached_plugin_event_target(&app, &state, &plugin_id)? {
            app.emit_to(
                &target,
                &format!("ihub://plugin/{plugin_id}/command"),
                json!({
                    "requestId": next_request_id(),
                    "commandId": command_id,
                    "input": Value::Null,
                    "context": Value::Null,
                    "utoolsMainPushAction": enter_action,
                }),
            )
            .map_err(|error| format!("Could not enter detached uTools plugin: {error}"))?;
            if let Some(detached_window) = app.get_webview_window(&target) {
                let _ = detached_window.unminimize();
                let _ = detached_window.show();
                let _ = detached_window.set_focus();
            }
            opened_detached = true;
        }
    }
    // Keep the exact selected option in the action returned to the trusted
    // renderer; the renderer cannot replace it because it came from the
    // native-issued search snapshot above.
    debug_assert_eq!(enter_action.get("option"), Some(&option));
    Ok(UtoolsMainPushSelectionResult {
        show,
        opened_detached,
        command_id,
        action: enter_action,
    })
}

/// Resolves exactly one native search waiter. A stale iframe response after a
/// timeout is intentionally harmless, but a different plugin may never claim
/// another plugin's opaque request id.
fn complete_plugin_search(
    host: &PluginHostState,
    plugin_id: &str,
    params: &Value,
) -> Result<(), String> {
    let request_id = required_string(params, "requestId")?;
    let pending = {
        let mut pending_searches = host
            .pending_searches
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match pending_searches.get(request_id) {
            None => return Ok(()),
            Some(pending) if pending.plugin_id != plugin_id => {
                return Err("A plugin may only complete its own search request.".to_owned());
            }
            Some(_) => pending_searches.remove(request_id),
        }
    };
    let Some(pending) = pending else {
        return Ok(());
    };

    let response = if params.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        normalize_plugin_search_results(
            params.get("result").unwrap_or(&Value::Null),
            pending.max_results,
        )
    } else {
        let message = params
            .get("error")
            .and_then(Value::as_str)
            .map(|message| truncate_text(message, MAX_PLUGIN_SEARCH_TEXT_CHARS))
            .filter(|message| !message.trim().is_empty())
            .unwrap_or_else(|| "The plugin search provider returned an error.".to_owned());
        Err(format!(
            "Plugin search provider '{}/{}' failed: {message}",
            pending.plugin_id, pending.provider_id
        ))
    };
    let _ = pending.response.send(response);
    Ok(())
}

fn get_plugin_session_secret(host: &PluginHostState, plugin_id: &str, key: &str) -> Option<Value> {
    host.secret_settings
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&host_key(plugin_id, key))
        .cloned()
}

fn set_plugin_session_secret(
    host: &PluginHostState,
    plugin_id: &str,
    key: &str,
    value: Value,
) -> Result<(), String> {
    PluginSettingsStore::validate_entry(key, &value)?;
    host.secret_settings
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(host_key(plugin_id, key), value);
    Ok(())
}

/// Secret settings are memory-only but should not cross a source replacement,
/// disable, or uninstall boundary. Opening a new iframe intentionally does
/// not call this helper so a user can close and reopen a plugin within the
/// same application session without re-entering its credential.
fn clear_plugin_session_secrets(host: &PluginHostState, plugin_id: &str) {
    let prefix = format!("{plugin_id}:");
    host.secret_settings
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .retain(|key, _| !key.starts_with(&prefix));
}

fn clear_plugin_runtime_state(host: &PluginHostState, plugin_id: &str) {
    let prefix = format!("{plugin_id}:");
    host.commands
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .retain(|key, _| !key.starts_with(&prefix));
    host.search_providers
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .retain(|key, _| !key.starts_with(&prefix));

    let pending = {
        let mut pending_searches = host
            .pending_searches
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let request_ids = pending_searches
            .iter()
            .filter_map(|(request_id, pending)| {
                (pending.plugin_id == plugin_id).then_some(request_id.clone())
            })
            .collect::<Vec<_>>();
        request_ids
            .into_iter()
            .filter_map(|request_id| pending_searches.remove(&request_id))
            .collect::<Vec<_>>()
    };
    for request in pending {
        let _ = request.response.send(Err(format!(
            "Plugin search provider '{}/{}' stopped before responding.",
            request.plugin_id, request.provider_id
        )));
    }
    let pending_main_push = {
        let mut selections = host
            .pending_utools_main_push_selections
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let selection_ids = selections
            .iter()
            .filter_map(|(selection_id, selection)| {
                (selection.plugin_id == plugin_id).then_some(selection_id.clone())
            })
            .collect::<Vec<_>>();
        selection_ids
            .into_iter()
            .filter_map(|selection_id| selections.remove(&selection_id))
            .collect::<Vec<_>>()
    };
    for selection in pending_main_push {
        let _ = selection.response.send(Err(format!(
            "uTools main-push owner '{plugin_id}' stopped before responding."
        )));
    }
    host.utools_tools
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .retain(|(owner, _), _| owner != plugin_id);
    let pending_tools = {
        let mut pending = host
            .pending_utools_tool_calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let request_ids = pending
            .iter()
            .filter_map(|(request_id, call)| {
                (call.plugin_id == plugin_id).then_some(request_id.clone())
            })
            .collect::<Vec<_>>();
        request_ids
            .into_iter()
            .filter_map(|request_id| pending.remove(&request_id))
            .collect::<Vec<_>>()
    };
    for call in pending_tools {
        let _ = call.response.send(Err(format!(
            "uTools MCP runtime '{plugin_id}' stopped before responding."
        )));
    }
    let ai_requests = {
        let mut active = host
            .utools_ai_requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let request_ids = active
            .iter()
            .filter_map(|(request_id, request)| {
                (request.plugin_id == plugin_id).then_some(request_id.clone())
            })
            .collect::<Vec<_>>();
        request_ids
            .into_iter()
            .filter_map(|request_id| active.remove(&request_id))
            .collect::<Vec<_>>()
    };
    for request in ai_requests {
        request.cancelled.store(true, Ordering::Release);
        if let Some(abort_handle) = request.abort_handle {
            abort_handle.abort();
        }
    }
    let ffmpeg_jobs = {
        let mut active = host
            .utools_ffmpeg_jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let request_ids = active
            .iter()
            .filter_map(|(request_id, job)| {
                (job.plugin_id == plugin_id).then_some(request_id.clone())
            })
            .collect::<Vec<_>>();
        request_ids
            .into_iter()
            .filter_map(|request_id| active.remove(&request_id))
            .collect::<Vec<_>>()
    };
    for job in ffmpeg_jobs {
        job.control.kill();
    }
    reject_pending_utools_ai_functions(
        host,
        |call| call.plugin_id == plugin_id,
        "uTools AI runtime stopped before the function responded.",
    );
    host.issued_search_results
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .retain(|_, search| search.plugin_id != plugin_id);

    // Lock grants before previews everywhere so a disable/uninstall cannot
    // race a preview/apply call into retaining a capability after lifecycle
    // disposal. Durable settings and session-only secrets intentionally stay
    // separate: a fresh iframe may reuse settings during the same app run.
    let mut grants = host
        .filesystem_grants
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    remove_expired_filesystem_grants(&mut grants);
    grants.retain(|_, grant| grant.plugin_id != plugin_id);
    let mut file_grants = host
        .file_grants
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    remove_expired_file_grants(&mut file_grants);
    file_grants.retain(|_, grant| grant.plugin_id != plugin_id);
    let mut launcher_contexts = host
        .launcher_contexts
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    remove_expired_plugin_launcher_contexts(&mut launcher_contexts, Instant::now());
    launcher_contexts.retain(|_, context| context.plugin_id != plugin_id);
    let mut previews = host
        .batch_rename_previews
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    remove_expired_plugin_batch_rename_previews(&mut previews);
    previews.retain(|_, preview| preview.plugin_id != plugin_id);
    host.utools_drag_grants
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .retain(|(owner, _), _| owner != plugin_id);
    host.utools_save_grants
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .retain(|(owner, _), _| owner != plugin_id);
    host.clear_plugin_capture_focus_leases(plugin_id);
    host.clear_plugin_cursor_color_approvals(plugin_id);
    host.clear_plugin_cursor_color_sample(plugin_id);
}

fn handle_plugin_log_call(request: &PluginHostRequest, state: &AppState) -> Result<Value, String> {
    let level = request
        .params
        .get("level")
        .and_then(Value::as_str)
        .unwrap_or("info");
    if level.len() > MAX_PLUGIN_LOG_LEVEL_BYTES {
        return Err(format!(
            "Plugin log level exceeds the {MAX_PLUGIN_LOG_LEVEL_BYTES}-byte limit."
        ));
    }
    let message = request
        .params
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("Plugin emitted a diagnostic without a text message.");
    if message.len() > MAX_PLUGIN_LOG_MESSAGE_BYTES {
        return Err(format!(
            "Plugin log message exceeds the {} KiB limit.",
            MAX_PLUGIN_LOG_MESSAGE_BYTES / 1024
        ));
    }

    let component = format!("plugin:{}", request.plugin_id);
    match state.host.admit_plugin_log(&request.plugin_id) {
        PluginLogAdmission::Accept { previously_dropped } => {
            if previously_dropped > 0 {
                host_log::warn(
                    &component,
                    format!(
                        "Dropped {previously_dropped} plugin-authored diagnostic message(s) during the previous rate-limit window."
                    ),
                );
            }
            // `details` may contain arbitrary application content, paths,
            // context handles, or secrets, so it is deliberately ignored.
            // The message is bounded here and pattern-sanitized again before
            // the host log persists it.
            match level {
                "debug" => host_log::debug(&component, message),
                "warn" | "warning" => host_log::warn(&component, message),
                "error" => host_log::error(&component, message),
                _ => host_log::info(&component, message),
            }
            Ok(json!({ "accepted": true, "rateLimited": false }))
        }
        PluginLogAdmission::Drop { report_limit } => {
            if report_limit {
                host_log::warn(
                    &component,
                    format!(
                        "Plugin diagnostics exceeded {MAX_PLUGIN_LOGS_PER_WINDOW} messages per {} seconds; further messages are being dropped.",
                        PLUGIN_LOG_WINDOW.as_secs()
                    ),
                );
            }
            Ok(json!({ "accepted": false, "rateLimited": true }))
        }
    }
}

fn canonical_selected_directory(directory: PathBuf) -> Result<String, String> {
    let directory = directory
        .canonicalize()
        .map_err(|error| format!("Could not resolve the selected folder: {error}"))?;
    let metadata = fs::metadata(&directory)
        .map_err(|error| format!("Could not inspect the selected folder: {error}"))?;
    if !metadata.is_dir() {
        return Err("The selected filesystem grant must be a folder.".to_owned());
    }
    Ok(directory.to_string_lossy().into_owned())
}

fn canonical_selected_file(file: PathBuf) -> Result<SelectedPluginFile, String> {
    let path = file
        .canonicalize()
        .map_err(|error| format!("Could not resolve the selected file: {error}"))?;
    let metadata = fs::metadata(&path)
        .map_err(|error| format!("Could not inspect the selected file: {error}"))?;
    if !metadata.is_file() {
        return Err("The selected filesystem grant must be a regular file.".to_owned());
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "The selected file must have a UTF-8 file name.".to_owned())?
        .to_owned();
    Ok(SelectedPluginFile {
        path,
        name,
        size: metadata.len(),
    })
}

/// Converts trusted-parent launcher input into the only data that may later
/// cross the plugin bridge. Canonical local paths are used solely to prove the
/// selected object still exists and to derive metadata; they are discarded
/// before the return value is stored or sent to an iframe.
fn build_plugin_launcher_context_payload(
    request: PluginLauncherContextRequest,
) -> Result<PluginLauncherContextPayload, String> {
    let text = match request.text {
        Some(text) if text.is_empty() => {
            return Err("Launcher context text must not be empty when provided.".to_owned())
        }
        Some(text) if text.len() > MAX_LAUNCHER_CONTEXT_TEXT_BYTES => {
            return Err(format!(
                "Launcher context text exceeds the {MAX_LAUNCHER_CONTEXT_TEXT_BYTES}-byte limit."
            ))
        }
        other => other,
    };

    if request.files.len() > MAX_LAUNCHER_CONTEXT_FILES {
        return Err(format!(
            "A launcher context can contain at most {MAX_LAUNCHER_CONTEXT_FILES} selected files or folders."
        ));
    }
    let mut seen_paths = HashSet::new();
    let mut files = Vec::with_capacity(request.files.len());
    for file in request.files {
        if file.path.is_empty() || file.path.len() > MAX_LAUNCHER_CONTEXT_PATH_BYTES {
            return Err("A launcher context file path is missing or too long.".to_owned());
        }
        let path = PathBuf::from(file.path)
            .canonicalize()
            .map_err(|error| format!("Could not resolve the selected launcher item: {error}"))?;
        if !seen_paths.insert(path.clone()) {
            continue;
        }
        let metadata = fs::metadata(&path)
            .map_err(|error| format!("Could not inspect the selected launcher item: {error}"))?;
        let (kind, size) = if metadata.is_file() {
            ("file", Some(metadata.len()))
        } else if metadata.is_dir() {
            ("folder", None)
        } else {
            return Err("A launcher context item must be a regular file or folder.".to_owned());
        };
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .ok_or_else(|| "A selected launcher item must have a UTF-8 file name.".to_owned())?
            .to_owned();
        files.push(PluginLauncherContextFileMetadata {
            handle_id: next_capability_id("launcher-file"),
            name,
            kind: kind.to_owned(),
            size,
        });
    }

    let image = request
        .image
        .map(build_plugin_launcher_context_image_handle)
        .transpose()?;
    if text.is_none() && files.is_empty() && image.is_none() {
        return Err("A launcher context must contain text, files, or an image.".to_owned());
    }

    Ok(PluginLauncherContextPayload { text, files, image })
}

fn build_plugin_launcher_context_image_handle(
    image: PluginLauncherContextImageRequest,
) -> Result<PluginLauncherContextImageHandle, String> {
    if image.name.trim().is_empty()
        || image.name.chars().count() > MAX_LAUNCHER_CONTEXT_IMAGE_NAME_CHARS
        || image
            .name
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\'))
    {
        return Err("The launcher context image name is invalid.".to_owned());
    }
    if image.mime_type != "image/png" {
        return Err(
            "Launcher context images must be the host-normalized image/png format.".to_owned(),
        );
    }
    let width = usize::try_from(image.width).unwrap_or(usize::MAX);
    let height = usize::try_from(image.height).unwrap_or(usize::MAX);
    if width == 0
        || height == 0
        || width > MAX_PASTED_IMAGE_EDGE
        || height > MAX_PASTED_IMAGE_EDGE
        || width.saturating_mul(height) > MAX_PASTED_IMAGE_PIXELS
    {
        return Err(
            "The launcher context image dimensions are outside the host limits.".to_owned(),
        );
    }

    Ok(PluginLauncherContextImageHandle {
        handle_id: next_capability_id("launcher-image"),
        name: image.name,
        mime_type: image.mime_type,
        width: image.width,
        height: image.height,
    })
}

fn issue_plugin_launcher_context_transfer(
    host: &PluginHostState,
    plugin_id: &str,
    command_id: &str,
    frontend_lease_id: &str,
    payload: PluginLauncherContextPayload,
) -> PluginLauncherContextIssue {
    let now = Instant::now();
    let expires_at = now + LAUNCHER_CONTEXT_TTL;
    let mut contexts = host
        .launcher_contexts
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    remove_expired_plugin_launcher_contexts(&mut contexts, now);
    let context_id = next_capability_id("launcher-context");
    contexts.insert(
        context_id.clone(),
        PluginLauncherContextTransfer {
            plugin_id: plugin_id.to_owned(),
            command_id: command_id.to_owned(),
            frontend_lease_id: frontend_lease_id.to_owned(),
            payload,
            issued_at: now,
            expires_at,
            dispatched_request_id: None,
        },
    );
    trim_oldest_records(&mut contexts, MAX_LAUNCHER_CONTEXT_TRANSFERS, |context| {
        context.issued_at
    });
    PluginLauncherContextIssue {
        context_id,
        expires_in_ms: remaining_launcher_context_millis(expires_at),
    }
}

/// Best-effort cleanup for a trusted parent that staged a context but could
/// not emit its command event (for example, a plugin reloaded between the
/// visible confirmation and dispatch). This removes the payload immediately
/// instead of waiting for the normal 60-second expiry.
fn revoke_plugin_launcher_context_transfer(
    host: &PluginHostState,
    plugin_id: &str,
    context_id: &str,
) -> Result<bool, String> {
    if context_id.is_empty() || context_id.len() > 512 {
        return Err("Invalid launcher context ID.".to_owned());
    }
    let now = Instant::now();
    let mut contexts = host
        .launcher_contexts
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    remove_expired_plugin_launcher_contexts(&mut contexts, now);
    let Some(context) = contexts.get(context_id) else {
        return Ok(false);
    };
    if context.plugin_id != plugin_id {
        return Err("This launcher context belongs to another plugin.".to_owned());
    }
    contexts.remove(context_id);
    Ok(true)
}

/// Marks a staged context as attached to exactly one command event. The
/// payload remains host-owned; only the opaque ID is sent to the iframe.
fn attach_plugin_launcher_context_transfer(
    host: &PluginHostState,
    plugin_id: &str,
    command_id: &str,
    frontend_lease_id: &str,
    request_id: &str,
    context_id: &str,
) -> Result<PluginLauncherContextInvocation, String> {
    let now = Instant::now();
    let mut contexts = host
        .launcher_contexts
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    remove_expired_plugin_launcher_contexts(&mut contexts, now);
    let Some(context) = contexts.get_mut(context_id) else {
        return Err(
            "This launcher context has expired or is unavailable. Choose the action again."
                .to_owned(),
        );
    };
    if context.plugin_id != plugin_id {
        return Err("This launcher context belongs to another plugin.".to_owned());
    }
    if context.command_id != command_id {
        return Err("This launcher context belongs to another plugin command.".to_owned());
    }
    if context.frontend_lease_id != frontend_lease_id {
        return Err("This launcher context belongs to a previous plugin surface.".to_owned());
    }
    if context.dispatched_request_id.is_some() {
        return Err(
            "This launcher context was already dispatched and cannot be replayed.".to_owned(),
        );
    }
    context.dispatched_request_id = Some(request_id.to_owned());
    Ok(PluginLauncherContextInvocation {
        context_id: context_id.to_owned(),
        expires_in_ms: remaining_launcher_context_millis(context.expires_at),
    })
}

/// Returns a staged payload once only after the matching plugin *and frontend
/// lease* have received it through a command event. This rechecks granular
/// manifest permission at read time, so a local development manifest change
/// cannot leave a stale broader transfer readable.
fn take_plugin_launcher_context_transfer(
    host: &PluginHostState,
    plugins: &PluginManager,
    plugin_id: &str,
    frontend_lease_id: &str,
    context_id: &str,
) -> Result<PluginLauncherContextPayload, String> {
    let now = Instant::now();
    let context = {
        let mut contexts = host
            .launcher_contexts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        remove_expired_plugin_launcher_contexts(&mut contexts, now);
        let Some(context) = contexts.get(context_id) else {
            return Err("This launcher context has expired or was already consumed.".to_owned());
        };
        if context.plugin_id != plugin_id {
            return Err("This launcher context belongs to another plugin.".to_owned());
        }
        if context.frontend_lease_id != frontend_lease_id {
            return Err("This launcher context belongs to a previous plugin surface.".to_owned());
        }
        if context.dispatched_request_id.is_none() {
            return Err(
                "This launcher context has not been attached to a user-selected command."
                    .to_owned(),
            );
        }
        context.clone()
    };

    if !plugins.allows_launcher_context(
        plugin_id,
        context.payload.text.is_some(),
        !context.payload.files.is_empty(),
        context.payload.image.is_some(),
    )? {
        return Err(
            "This launcher context is no longer permitted by the plugin manifest.".to_owned(),
        );
    }

    let mut contexts = host
        .launcher_contexts
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    remove_expired_plugin_launcher_contexts(&mut contexts, Instant::now());
    let Some(current) = contexts.get(context_id) else {
        return Err("This launcher context has expired or was already consumed.".to_owned());
    };
    if current.plugin_id != plugin_id
        || current.frontend_lease_id != frontend_lease_id
        || current.dispatched_request_id.is_none()
    {
        return Err("This launcher context is no longer available to this plugin.".to_owned());
    }
    Ok(contexts
        .remove(context_id)
        .expect("the checked launcher context must remain present while locked")
        .payload)
}

fn remaining_launcher_context_millis(expires_at: Instant) -> u64 {
    u64::try_from(
        expires_at
            .saturating_duration_since(Instant::now())
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UtoolsDialogFilter {
    name: String,
    extensions: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UtoolsDialogOptions {
    title: Option<String>,
    default_path: Option<String>,
    button_label: Option<String>,
    #[serde(default)]
    filters: Vec<UtoolsDialogFilter>,
    #[serde(default)]
    properties: Vec<String>,
    message: Option<String>,
    name_field_label: Option<String>,
    shows_tag_field: Option<Value>,
    security_scoped_bookmarks: Option<bool>,
}

fn validate_utools_dialog_text(
    value: Option<String>,
    field: &str,
    max_chars: usize,
) -> Result<Option<String>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_empty() || value.chars().count() > max_chars || value.chars().any(char::is_control)
    {
        return Err(format!(
            "uTools dialog {field} must contain 1-{max_chars} non-control characters."
        ));
    }
    Ok(Some(value))
}

fn validate_utools_dialog_options(kind: &str, value: Value) -> Result<UtoolsDialogOptions, String> {
    if !matches!(kind, "open" | "save") {
        return Err("uTools dialog kind must be open or save.".to_owned());
    }
    let mut options = serde_json::from_value::<UtoolsDialogOptions>(value)
        .map_err(|error| format!("uTools dialog options are invalid: {error}"))?;
    options.title = validate_utools_dialog_text(options.title, "title", 240)?;
    options.button_label = validate_utools_dialog_text(options.button_label, "buttonLabel", 80)?;
    options.message = validate_utools_dialog_text(options.message, "message", 240)?;
    options.name_field_label =
        validate_utools_dialog_text(options.name_field_label, "nameFieldLabel", 80)?;
    options.default_path = validate_utools_dialog_text(options.default_path, "defaultPath", 1024)?;
    if let Some(path) = options.default_path.as_ref() {
        if !Path::new(path).is_absolute() || path.len() > MAX_UTOOLS_COPY_FILE_PATH_BYTES {
            return Err("uTools dialog defaultPath must be a bounded absolute path.".to_owned());
        }
    }
    if options.filters.len() > 16 {
        return Err("uTools dialogs accept at most 16 file filters.".to_owned());
    }
    for filter in &options.filters {
        if filter.name.is_empty()
            || filter.name.chars().count() > 80
            || filter.name.chars().any(char::is_control)
            || filter.extensions.is_empty()
            || filter.extensions.len() > 16
            || filter.extensions.iter().any(|extension| {
                extension != "*"
                    && (extension.is_empty()
                        || extension.len() > 16
                        || !extension.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'_' | b'-')
                        }))
            })
        {
            return Err("A uTools dialog file filter is invalid or too large.".to_owned());
        }
    }
    if options.properties.len() > 12 {
        return Err("uTools dialog properties are too numerous.".to_owned());
    }
    let mut seen = HashSet::new();
    if options.properties.iter().any(|property| {
        property.is_empty() || property.chars().count() > 40 || !seen.insert(property.clone())
    }) {
        return Err("uTools dialog properties must be unique bounded strings.".to_owned());
    }
    let supported: &[&str] = if kind == "open" {
        &[
            "openFile",
            "openDirectory",
            "multiSelections",
            "createDirectory",
        ]
    } else {
        &["showOverwriteConfirmation", "createDirectory"]
    };
    if options
        .properties
        .iter()
        .any(|property| !supported.contains(&property.as_str()))
    {
        return Err(
            "This uTools dialog property is not runtime-verified by the iHub native picker yet."
                .to_owned(),
        );
    }
    if kind == "open"
        && options.properties.iter().any(|value| value == "openFile")
        && options
            .properties
            .iter()
            .any(|value| value == "openDirectory")
    {
        return Err("iHub does not mix files and folders in one uTools dialog.".to_owned());
    }
    if options.security_scoped_bookmarks == Some(true)
        || options.message.is_some()
        || options.name_field_label.is_some()
        || options.shows_tag_field.is_some()
        || options.button_label.is_some()
    {
        return Err(
            "This platform-specific uTools dialog option is not runtime-verified by iHub yet."
                .to_owned(),
        );
    }
    Ok(options)
}

fn configure_utools_file_dialog(
    app: &AppHandle,
    plugin_id: &str,
    kind: &str,
    options: &UtoolsDialogOptions,
) -> rfd::FileDialog {
    let title = options.title.as_deref().unwrap_or(if kind == "open" {
        "选择文件"
    } else {
        "保存文件"
    });
    let mut dialog = rfd::FileDialog::new().set_title(format!("iHub · {plugin_id} · {title}"));
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&window);
    }
    if let Some(default_path) = options.default_path.as_ref() {
        let path = Path::new(default_path);
        if kind == "save" {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                dialog = dialog.set_directory(parent);
            }
            if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                dialog = dialog.set_file_name(name);
            }
        } else {
            dialog = dialog.set_directory(path);
        }
    }
    for filter in &options.filters {
        dialog = dialog.add_filter(&filter.name, &filter.extensions);
    }
    dialog
}

fn canonical_utools_dialog_selection(path: PathBuf, folder: bool) -> Result<String, String> {
    if folder {
        canonical_selected_directory(path)
    } else {
        canonical_selected_file(path).map(|file| file.path.to_string_lossy().into_owned())
    }
}

fn validate_utools_save_selection(path: PathBuf) -> Result<String, String> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "The selected save path has no valid file name.".to_owned())?;
    if file_name.chars().any(char::is_control) {
        return Err("The selected save file name contains control characters.".to_owned());
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| "The selected save path has no parent folder.".to_owned())?;
    let prepared = crate::system_open::prepare_local_open(parent, Some(LocalOpenKind::Folder))?;
    let result = prepared
        .path()
        .join(file_name)
        .to_string_lossy()
        .into_owned();
    if result.is_empty() || result.len() > MAX_UTOOLS_COPY_FILE_PATH_BYTES {
        return Err("The selected save path is invalid or too long.".to_owned());
    }
    Ok(result)
}

fn show_utools_dialog_on_main_thread(
    app: &AppHandle,
    request: UtoolsDialogRequest,
) -> Result<Value, String> {
    let state = app.state::<AppState>();
    state.plugins.ensure_plugin_enabled(&request.plugin_id)?;
    if !state
        .plugins
        .uses_utools_compatibility(&request.plugin_id)?
    {
        return Err("Native uTools dialogs require a verified uTools package.".to_owned());
    }
    let options = validate_utools_dialog_options(&request.kind, request.options)?;
    let dialog = configure_utools_file_dialog(app, &request.plugin_id, &request.kind, &options);
    let _dialog_guard = NativeDialogGuard::begin(&state.host);
    if request.kind == "save" {
        let selected = dialog
            .save_file()
            .map(validate_utools_save_selection)
            .transpose()?;
        if let Some(path) = selected.as_deref() {
            remember_utools_save_grant(&state.host, &request.plugin_id, &request.lease_id, path)?;
        }
        return Ok(selected.map_or(Value::Null, Value::String));
    }
    let folder = options
        .properties
        .iter()
        .any(|property| property == "openDirectory");
    let multiple = options
        .properties
        .iter()
        .any(|property| property == "multiSelections");
    let paths = match (folder, multiple) {
        (true, true) => dialog.pick_folders(),
        (true, false) => dialog.pick_folder().map(|path| vec![path]),
        (false, true) => dialog.pick_files(),
        (false, false) => dialog.pick_file().map(|path| vec![path]),
    };
    let Some(paths) = paths else {
        return Ok(Value::Null);
    };
    if paths.is_empty() || paths.len() > MAX_UTOOLS_DIALOG_SELECTIONS {
        return Err(format!(
            "uTools dialogs accept at most {MAX_UTOOLS_DIALOG_SELECTIONS} selections."
        ));
    }
    let selected = paths
        .into_iter()
        .map(|path| canonical_utools_dialog_selection(path, folder))
        .collect::<Result<Vec<_>, _>>()?;
    remember_utools_drag_grants(
        &state.host,
        &request.plugin_id,
        &request.lease_id,
        &selected,
        if folder {
            LocalOpenKind::Folder
        } else {
            LocalOpenKind::File
        },
    )?;
    Ok(Value::Array(
        selected.into_iter().map(Value::String).collect(),
    ))
}

fn dispatch_utools_dialog(app: &AppHandle, request: UtoolsDialogRequest) -> Result<Value, String> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let callback_app = app.clone();
    app.run_on_main_thread(move || {
        let _ = sender.send(show_utools_dialog_on_main_thread(&callback_app, request));
    })
    .map_err(|error| format!("Could not schedule the native uTools dialog: {error}"))?;
    receiver
        .recv()
        .map_err(|_| "The native uTools dialog closed without a result.".to_owned())?
}

fn select_directory_with_native_dialog(
    app: &AppHandle,
    host: &PluginHostState,
    title: &str,
) -> Result<Option<String>, String> {
    // Bind the native picker to the iHub window. Besides platform polish (a
    // Windows owner / macOS sheet), this avoids treating a trusted modal picker
    // as an unrelated app that should dismiss the resident launcher on focus
    // loss. The guard is RAII so cancellations and native errors always restore
    // the regular focus behavior.
    let mut dialog = rfd::FileDialog::new().set_title(title);
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&window);
    }
    let _dialog_guard = NativeDialogGuard::begin(host);
    dialog
        .pick_folder()
        .map(canonical_selected_directory)
        .transpose()
}

fn select_files_with_native_dialog(
    app: &AppHandle,
    host: &PluginHostState,
    title: &str,
) -> Result<Option<Vec<SelectedPluginFile>>, String> {
    let mut dialog = rfd::FileDialog::new().set_title(title);
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&window);
    }
    let _dialog_guard = NativeDialogGuard::begin(host);
    let Some(paths) = dialog.pick_files() else {
        return Ok(None);
    };
    if paths.is_empty() {
        return Err("Choose at least one file before continuing.".to_owned());
    }
    if paths.len() > MAX_PLUGIN_FILES_PER_GRANT {
        return Err(format!(
            "Select at most {MAX_PLUGIN_FILES_PER_GRANT} files in one plugin request."
        ));
    }
    paths
        .into_iter()
        .map(canonical_selected_file)
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn select_save_file_with_native_dialog(
    app: &AppHandle,
    host: &PluginHostState,
    title: &str,
    suggested_filename: &str,
) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new()
        .set_title(title)
        .set_file_name(suggested_filename);
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&window);
    }
    let _dialog_guard = NativeDialogGuard::begin(host);
    dialog.save_file()
}

fn select_upload_file_with_native_dialog(
    app: &AppHandle,
    host: &PluginHostState,
    title: &str,
) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title(title);
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&window);
    }
    let _dialog_guard = NativeDialogGuard::begin(host);
    dialog.pick_file()
}

fn issue_filesystem_grant(
    host: &PluginHostState,
    plugin_id: &str,
    directory: String,
) -> Result<String, String> {
    let prepared =
        crate::system_open::prepare_local_open(Path::new(&directory), Some(LocalOpenKind::Folder))?;
    let directory = prepared.path().to_string_lossy().into_owned();
    let identity = prepared.identity();
    let mut grants = host
        .filesystem_grants
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    remove_expired_filesystem_grants(&mut grants);
    let grant_id = next_capability_id("folder");
    grants.insert(
        grant_id.clone(),
        FilesystemGrant {
            plugin_id: plugin_id.to_owned(),
            directory,
            identity,
            issued_at: Instant::now(),
        },
    );
    trim_oldest_records(&mut grants, MAX_FILESYSTEM_GRANTS, |grant| grant.issued_at);
    Ok(grant_id)
}

fn issue_file_grant(
    host: &PluginHostState,
    plugin_id: &str,
    files: Vec<SelectedPluginFile>,
) -> String {
    let mut grants = host
        .file_grants
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    remove_expired_file_grants(&mut grants);
    let grant_id = next_capability_id("file");
    grants.insert(
        grant_id.clone(),
        PluginFileGrant {
            plugin_id: plugin_id.to_owned(),
            files,
            issued_at: Instant::now(),
        },
    );
    trim_oldest_records(&mut grants, MAX_PLUGIN_FILE_GRANTS, |grant| grant.issued_at);
    grant_id
}

#[cfg(test)]
fn directory_for_grant(
    host: &PluginHostState,
    plugin_id: &str,
    grant_id: &str,
) -> Result<String, String> {
    prepare_directory_for_grant(host, plugin_id, grant_id)
        .map(|prepared| prepared.path().to_string_lossy().into_owned())
}

fn prepare_directory_for_grant(
    host: &PluginHostState,
    plugin_id: &str,
    grant_id: &str,
) -> Result<PreparedLocalOpen, String> {
    let grant = {
        let mut grants = host
            .filesystem_grants
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        remove_expired_filesystem_grants(&mut grants);
        grants.get(grant_id).cloned().ok_or_else(|| {
            "This folder selection has expired. Choose the folder again.".to_owned()
        })?
    };
    if grant.plugin_id != plugin_id {
        return Err("This folder selection belongs to another plugin.".to_owned());
    }
    let prepared = crate::system_open::prepare_local_open(
        Path::new(&grant.directory),
        Some(LocalOpenKind::Folder),
    )?;
    let canonical_directory = prepared.path().to_string_lossy().into_owned();
    if canonical_directory != grant.directory {
        return Err(
            "The selected folder changed after authorization. Choose the folder again.".to_owned(),
        );
    }
    if prepared.identity() != grant.identity {
        return Err(
            "The selected folder was replaced after authorization. Choose the folder again."
                .to_owned(),
        );
    }
    Ok(prepared)
}

/// Resolves one explicit selection only for the matching plugin's native
/// command. The frontend never receives these paths, and a successful lookup
/// consumes the token even if the worker later reports an error.
fn take_file_grant(
    host: &PluginHostState,
    plugin_id: &str,
    grant_id: &str,
) -> Result<Vec<SelectedPluginFile>, String> {
    let mut grants = host
        .file_grants
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    remove_expired_file_grants(&mut grants);
    let Some(grant) = grants.get(grant_id) else {
        return Err("This file selection has expired. Choose the files again.".to_owned());
    };
    if grant.plugin_id != plugin_id {
        return Err("This file selection belongs to another plugin.".to_owned());
    }
    for file in &grant.files {
        let resolved = file
            .path
            .canonicalize()
            .map_err(|error| format!("A selected file is no longer available: {error}"))?;
        if resolved != file.path {
            return Err("A selected file changed after approval. Choose it again.".to_owned());
        }
        let metadata = fs::metadata(&resolved)
            .map_err(|error| format!("Could not inspect a selected file: {error}"))?;
        if !metadata.is_file() {
            return Err("A selected path is no longer a regular file. Choose it again.".to_owned());
        }
    }
    Ok(grants
        .remove(grant_id)
        .expect("the checked file grant must remain present while locked")
        .files)
}

/// Converts a frontend-native command request into the only input the worker
/// receives. In particular, the iframe can reference a file grant but cannot
/// smuggle an arbitrary host path through the grant mechanism.
fn native_plugin_command_input(
    host: &PluginHostState,
    plugin_id: &str,
    params: &Value,
) -> Result<(String, Value), String> {
    let command_id = required_string_any(params, &["commandId", "id"])?.to_owned();
    let input = params.get("input").cloned().unwrap_or(Value::Null);
    let input = match params.get("fileGrantId") {
        Some(Value::String(grant_id)) => {
            let files = take_file_grant(host, plugin_id, grant_id)?;
            json!({
                "input": input,
                "files": files.into_iter().map(|file| json!({
                    "path": file.path,
                    "name": file.name,
                    "size": file.size,
                })).collect::<Vec<_>>(),
            })
        }
        Some(_) => return Err("native.runCommand fileGrantId must be a string.".to_owned()),
        None => input,
    };
    Ok((command_id, input))
}

/// Creates a project only inside a live, host-issued directory grant. The
/// frontend submits an opaque grant id and a plugin id — never an arbitrary
/// path — and the template creator reserves a new child directory without
/// overwriting anything already present.
fn create_plugin_project_for_grant(
    host: &PluginHostState,
    requesting_plugin_id: &str,
    grant_id: &str,
    plugin_id: &str,
) -> Result<PluginProjectCreated, String> {
    let prepared = prepare_directory_for_grant(host, requesting_plugin_id, grant_id)?;
    let parent_directory = prepared.path().to_string_lossy().into_owned();
    let project = create_plugin_project_template(&parent_directory, plugin_id)?;
    drop(prepared);
    Ok(project)
}

fn remember_plugin_batch_rename_preview(
    host: &PluginHostState,
    plugin_id: &str,
    grant_id: &str,
    preview: crate::builtin_tools::BatchRenamePreview,
) -> Result<String, String> {
    let mut grants = host
        .filesystem_grants
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    remove_expired_filesystem_grants(&mut grants);
    let Some(grant) = grants.get(grant_id) else {
        return Err("This folder selection has expired. Choose the folder again.".to_owned());
    };
    if grant.plugin_id != plugin_id || grant.directory != preview.directory {
        return Err("The rename preview is not bound to this plugin folder selection.".to_owned());
    }

    let mut previews = host
        .batch_rename_previews
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    remove_expired_plugin_batch_rename_previews(&mut previews);
    let preview_id = next_capability_id("rename-preview");
    previews.insert(
        preview_id.clone(),
        PluginBatchRenamePreview {
            plugin_id: plugin_id.to_owned(),
            grant_id: grant_id.to_owned(),
            preview,
            issued_at: Instant::now(),
        },
    );
    trim_oldest_records(&mut previews, MAX_PLUGIN_BATCH_RENAME_PREVIEWS, |preview| {
        preview.issued_at
    });
    Ok(preview_id)
}

fn take_plugin_batch_rename_preview(
    host: &PluginHostState,
    plugin_id: &str,
    grant_id: &str,
    preview_id: &str,
) -> Result<crate::builtin_tools::BatchRenamePreview, String> {
    let mut grants = host
        .filesystem_grants
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    remove_expired_filesystem_grants(&mut grants);
    let Some(grant) = grants.get(grant_id) else {
        return Err("This folder selection has expired. Choose the folder again.".to_owned());
    };
    if grant.plugin_id != plugin_id {
        return Err("This folder selection belongs to another plugin.".to_owned());
    }

    let mut previews = host
        .batch_rename_previews
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    remove_expired_plugin_batch_rename_previews(&mut previews);
    let Some(preview) = previews.get(preview_id) else {
        return Err(
            "This rename preview has expired or was already used. Preview again before applying."
                .to_owned(),
        );
    };
    if preview.plugin_id != plugin_id || preview.grant_id != grant_id {
        return Err(
            "This rename preview belongs to another plugin or folder selection.".to_owned(),
        );
    }
    if preview.preview.directory != grant.directory {
        return Err(
            "The selected folder changed after this rename preview. Preview again.".to_owned(),
        );
    }
    // Do not consume another plugin's token on an invalid request. Only the
    // owner of the matching grant can make this single-use preview disappear.
    let preview = previews
        .remove(preview_id)
        .expect("the checked rename preview must remain present while locked");
    Ok(preview.preview)
}

fn remove_expired_filesystem_grants(grants: &mut HashMap<String, FilesystemGrant>) {
    grants.retain(|_, grant| grant.issued_at.elapsed() <= FILESYSTEM_GRANT_TTL);
}

fn remove_expired_file_grants(grants: &mut HashMap<String, PluginFileGrant>) {
    grants.retain(|_, grant| grant.issued_at.elapsed() <= FILESYSTEM_GRANT_TTL);
}

fn remove_expired_temporary_path_open_grants(
    grants: &mut HashMap<String, TemporaryPathOpenGrant>,
    now: Instant,
) {
    grants.retain(|_, grant| {
        now.saturating_duration_since(grant.issued_at) <= TEMPORARY_PATH_OPEN_TTL
    });
}

fn remove_expired_plugin_launcher_contexts(
    contexts: &mut HashMap<String, PluginLauncherContextTransfer>,
    now: Instant,
) {
    contexts.retain(|_, context| context.expires_at > now);
}

fn remove_expired_plugin_batch_rename_previews(
    previews: &mut HashMap<String, PluginBatchRenamePreview>,
) {
    previews.retain(|_, preview| preview.issued_at.elapsed() <= BATCH_RENAME_PREVIEW_TTL);
}

fn remove_expired_capture_focus_leases(
    leases: &mut HashMap<String, CaptureFocusLease>,
    now: Instant,
) {
    leases.retain(|_, lease| lease.expires_at > now);
}

fn remove_expired_cursor_color_approvals(
    approvals: &mut HashMap<String, CursorColorApproval>,
    now: Instant,
) {
    approvals.retain(|_, approval| approval.expires_at > now);
}

fn remove_expired_issued_plugin_searches(
    searches: &mut HashMap<String, IssuedPluginSearchResults>,
    now: Instant,
) {
    searches.retain(|_, search| {
        now.saturating_duration_since(search.issued_at) <= PLUGIN_SEARCH_SELECTION_TTL
    });
}

fn trim_oldest_records<T>(
    records: &mut HashMap<String, T>,
    maximum: usize,
    issued_at: impl Fn(&T) -> Instant,
) {
    while records.len() > maximum {
        let Some(oldest_id) = records
            .iter()
            .min_by_key(|(_, record)| issued_at(record))
            .map(|(id, _)| id.clone())
        else {
            break;
        };
        records.remove(&oldest_id);
    }
}

fn normalize_plugin_search_results(
    value: &Value,
    max_results: usize,
) -> Result<Vec<PluginSearchResult>, String> {
    let entries = value
        .as_array()
        .ok_or_else(|| "Plugin search result must be an array.".to_owned())?;
    let mut results = Vec::with_capacity(entries.len().min(max_results));
    for entry in entries.iter().take(max_results) {
        let entry = entry
            .as_object()
            .ok_or_else(|| "Each plugin search result must be an object.".to_owned())?;
        let id = bounded_result_text(entry.get("id"), "id", 160)?;
        let title = bounded_result_text(entry.get("title"), "title", MAX_PLUGIN_SEARCH_TEXT_CHARS)?;
        let subtitle = entry
            .get("subtitle")
            .filter(|value| !value.is_null())
            .map(|value| bounded_result_text(Some(value), "subtitle", MAX_PLUGIN_SEARCH_TEXT_CHARS))
            .transpose()?;
        let score = entry
            .get("score")
            .and_then(Value::as_f64)
            .unwrap_or_default()
            .clamp(-1_000_000.0, 1_000_000.0);
        let payload = entry
            .get("payload")
            .filter(|value| !value.is_null())
            .cloned();
        if let Some(payload) = &payload {
            let size = serde_json::to_vec(payload)
                .map_err(|error| format!("Plugin search payload cannot be serialized: {error}"))?
                .len();
            if size > MAX_PLUGIN_SEARCH_PAYLOAD_BYTES {
                return Err(format!(
                    "Plugin search payload exceeds the {MAX_PLUGIN_SEARCH_PAYLOAD_BYTES}-byte limit."
                ));
            }
        }
        results.push(PluginSearchResult {
            id,
            title,
            subtitle,
            score,
            payload,
        });
    }
    Ok(results)
}

fn bounded_result_text(
    value: Option<&Value>,
    field: &str,
    max_chars: usize,
) -> Result<String, String> {
    let value = value
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Plugin search result.{field} must be a string."))?;
    if value.trim().is_empty() {
        return Err(format!("Plugin search result.{field} cannot be empty."));
    }
    if value.chars().count() > max_chars {
        return Err(format!(
            "Plugin search result.{field} exceeds {max_chars} characters."
        ));
    }
    Ok(value.to_owned())
}

fn truncate_text(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let text = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{text}…")
    } else {
        text
    }
}

fn release_hosted_plugin_window_lease(window: &tauri::Window) {
    if window.label() == "main" {
        return;
    }
    if window.label().starts_with(UTOOLS_UBROWSER_WINDOW_PREFIX) {
        if let Some(registry) = window.try_state::<UtoolsUBrowserRegistry>() {
            registry.remove_window(window.label());
        }
        return;
    }
    let lease_id = if window.label().starts_with(UTOOLS_BROWSER_WINDOW_PREFIX) {
        window
            .try_state::<UtoolsBrowserWindowRegistry>()
            .and_then(|registry| registry.take_window_lease(window.label()))
    } else {
        window
            .try_state::<DetachedPluginWindowRegistry>()
            .and_then(|registry| registry.take_window_lease(window.label()))
    };
    let Some(lease_id) = lease_id else {
        return;
    };
    if let Some(state) = window.try_state::<AppState>() {
        if let Some(plugin_id) = release_plugin_frontend_lease(&lease_id, &state) {
            emit_plugin_search_providers_changed(window.app_handle(), &plugin_id, None, false);
        }
    }
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            host_log::info(
                "lifecycle",
                "A second launch request focused the resident host.",
            );
            show_launcher(app);
        }))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--ihub-autostart"]),
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        // uTools-style launchers are background residents: close and loss of
        // focus dismiss the surface but keep the tray/single-instance host
        // alive for the next global-hotkey invocation.
        .on_window_event(|window, event| match event {
            WindowEvent::CloseRequested { api, .. } => {
                if window.label() == "main" {
                    host_log::debug(
                        "lifecycle",
                        "Main window close request hid the resident launcher.",
                    );
                    api.prevent_close();
                    let _ = window.emit("ihub://hide-search", json!({}));
                    let _ = window.hide();
                } else {
                    host_log::debug("plugins", "A detached plugin host window closed.");
                    // A normal decorated detached window really closes. Revoke
                    // its iframe lease before the webview disappears so React
                    // cleanup is an optimization rather than a security
                    // boundary.
                    release_hosted_plugin_window_lease(window);
                }
            }
            WindowEvent::Destroyed => {
                host_log::debug("lifecycle", "A host window was destroyed.");
                // Platform shutdown paths can skip CloseRequested. This is
                // idempotent after the normal close branch.
                release_hosted_plugin_window_lease(window);
            }
            WindowEvent::Focused(true) => {
                if window.label() != "main" {
                    return;
                }
                if let Some(state) = window.try_state::<AppState>() {
                    state.launcher_focus.note_focus();
                }
            }
            WindowEvent::Focused(false) => {
                if window.label() != "main" {
                    return;
                }
                let Some(state) = window.try_state::<AppState>() else {
                    return;
                };
                if state.host.auto_hide_is_suspended()
                    || !state.launcher_focus.consume_blur_after_focus()
                {
                    return;
                }
                let _ = window.emit("ihub://hide-search", json!({}));
                let _ = window.hide();
                host_log::debug("lifecycle", "Launcher hid after focus moved away.");
            }
            _ => {}
        })
        .setup(|app| {
            // A launcher should be a background resident after login, not a
            // conventional foreground application. Manual launches still
            // explicitly show the search surface below.
            #[cfg(target_os = "macos")]
            {
                app.set_activation_policy(tauri::ActivationPolicy::Accessory);
                app.set_dock_visibility(false);
            }

            let app_data_dir = app.path().app_data_dir()?;
            if let Err(error) = host_log::initialize(&app_data_dir) {
                // Logging is diagnostic infrastructure, not an availability
                // dependency. Keep the launcher usable when app-data storage
                // is temporarily read-only; later log reads surface the same
                // bounded error through the settings UI.
                host_log::error("lifecycle", error);
            }
            host_log::info("lifecycle", "iHub host startup initialized.");
            let state = AppState::new(app_data_dir);
            state.index.start_change_watcher();
            state.index.rebuild_default_roots();
            let clipboard_history = state.clipboard_history.clone();
            app.manage(state);
            app.manage(DetachedPluginWindowRegistry::default());
            app.manage(UtoolsBrowserWindowRegistry::default());
            app.manage(UtoolsUBrowserRegistry::default());
            let dialog_app = app.handle().clone();
            app.state::<AppState>()
                .plugin_assets
                .set_utools_dialog_handler(Arc::new(move |request| {
                    dispatch_utools_dialog(&dialog_app, request)
                }));
            if app.state::<AppState>().super_panel.enabled() {
                if let Err(error) = ensure_super_panel_listener(app.handle()) {
                    host_log::error(
                        "super-panel",
                        format!("Could not restore the listener: {error}"),
                    );
                }
            }
            let _ = std::thread::Builder::new()
                .name("ihub-clipboard-history".to_owned())
                .spawn(move || loop {
                    clipboard_history.poll_system_clipboard();
                    std::thread::sleep(Duration::from_millis(750));
                });
            setup_tray(app)?;
            let preferred_launcher_hotkey = app
                .state::<AppState>()
                .launcher_hotkey_store
                .load_preference();
            let launcher_hotkey = register_launcher_hotkey(app.handle(), preferred_launcher_hotkey);
            app.state::<AppState>()
                .set_launcher_hotkey_status(launcher_hotkey);
            let active_hotkey = app.state::<AppState>().launcher_hotkey_status();
            host_log::info(
                "hotkey",
                format!(
                    "Launcher hotkey registration finished with {}.",
                    active_hotkey
                        .accelerator
                        .as_deref()
                        .unwrap_or("no active accelerator")
                ),
            );
            refresh_plugin_shortcuts(app.handle());
            host_log::info(
                "lifecycle",
                if launched_from_autostart() {
                    "Resident host ready after autostart."
                } else {
                    "Resident host ready after an explicit launch."
                },
            );
            if !launched_from_autostart() {
                show_launcher(app.handle());
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_index_status,
            search_entries,
            get_system_icons,
            index_default_roots,
            set_index_roots,
            get_default_roots,
            open_granted_path,
            open_search_result,
            copy_search_results_to_clipboard,
            list_launcher_shortcuts,
            pin_launcher_shortcut_from_search,
            open_launcher_shortcut,
            unpin_launcher_shortcut,
            list_plugins,
            match_utools_text_commands,
            set_plugin_command_shortcut,
            reset_plugin_command_shortcut,
            crate::detached_plugin_window::open_detached_plugin_window,
            crate::detached_plugin_window::get_detached_plugin_window_bootstrap,
            crate::detached_plugin_window::close_detached_plugin_window,
            get_plugin_frontend_url,
            get_utools_browser_window_bootstrap,
            mark_utools_browser_window_ready,
            issue_plugin_launcher_context,
            revoke_plugin_launcher_context,
            issue_plugin_cursor_color_approval,
            release_plugin_frontend_url,
            touch_plugin_frontend_lease,
            install_plugin_from_git,
            check_plugin_update,
            check_automatic_plugin_updates,
            update_plugin_from_git,
            link_plugin_from_local,
            list_official_workspace_plugins,
            link_official_workspace_plugin,
            unlink_plugin_from_local,
            set_plugin_enabled,
            uninstall_managed_plugin,
            create_plugin_project,
            select_directory,
            acquire_capture_focus_lease,
            release_capture_focus_lease,
            sample_cursor_color,
            begin_cursor_color_picker,
            sample_cursor_color_neighborhood,
            end_cursor_color_picker,
            capture_native_screenshot,
            capture_plugin_screen_screenshot,
            crate::network_diagnostics::get_local_network_info,
            crate::network_diagnostics::get_public_network_info,
            crate::network_diagnostics::run_network_speed_test,
            crate::ocr::get_ocr_capabilities,
            crate::ocr::recognize_ocr_image,
            start_lan_file_share,
            get_lan_file_share_status,
            stop_lan_file_share,
            get_hosts_snapshot,
            apply_hosts_entries,
            restore_hosts_backup,
            crate::wifi_profiles::list_wifi_profiles,
            crate::wifi_profiles::reveal_wifi_password,
            crate::builtin_tools::format_json,
            crate::builtin_tools::query_json,
            preview_batch_rename,
            apply_batch_rename,
            crate::builtin_tools::write_clipboard_text,
            list_ai_provider_profiles,
            save_ai_provider_profile,
            delete_ai_provider_profile,
            list_ai_models,
            test_ai_provider_profile,
            list_cloud_profiles,
            connect_webdav,
            connect_cloud_profile,
            list_webdav_directory,
            disconnect_webdav,
            forget_cloud_profile,
            download_webdav_file,
            upload_webdav_file,
            get_clipboard_history,
            set_clipboard_history_enabled,
            set_clipboard_history_capture_options,
            copy_clipboard_history_item,
            restore_clipboard_history_item,
            get_clipboard_history_image_preview,
            open_clipboard_history_file_entry,
            set_clipboard_history_item_pinned,
            delete_clipboard_history_item,
            clear_unpinned_clipboard_history,
            read_clipboard_files,
            read_clipboard_image,
            get_super_panel_status,
            set_super_panel_enabled,
            consume_super_panel_context,
            run_plugin_command,
            get_autostart_status,
            set_autostart,
            quit_app,
            set_launcher_hotkey,
            reset_launcher_hotkey,
            get_app_health,
            get_host_log,
            clear_host_log,
            center_launcher_window,
            plugin_host_call,
            dispatch_detached_plugin_frontend_event,
            invoke_plugin_frontend_command,
            list_registered_plugin_search_providers,
            list_utools_tools,
            invoke_utools_tool,
            cancel_utools_tool,
            query_plugin_search,
            select_utools_main_push_result
        ])
        .run(tauri::generate_context!())
        .expect("error while running iHub");
}

fn launched_from_autostart() -> bool {
    std::env::args_os().any(|argument| argument == "--ihub-autostart")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StartupLauncherHotkeyCandidate {
    accelerator: String,
    preferred: bool,
}

fn startup_launcher_hotkey_candidates(
    preferred_accelerator: Option<&str>,
) -> Vec<StartupLauncherHotkeyCandidate> {
    let mut candidates = Vec::with_capacity(3);
    if let Some(accelerator) = preferred_accelerator {
        candidates.push(StartupLauncherHotkeyCandidate {
            accelerator: accelerator.to_owned(),
            preferred: true,
        });
    }
    for accelerator in [LAUNCHER_PRIMARY_HOTKEY, LAUNCHER_FALLBACK_HOTKEY] {
        if candidates
            .iter()
            .any(|candidate| candidate.accelerator == accelerator)
        {
            continue;
        }
        candidates.push(StartupLauncherHotkeyCandidate {
            accelerator: accelerator.to_owned(),
            preferred: false,
        });
    }
    candidates
}

fn register_launcher_binding(app: &AppHandle, accelerator: &str) -> Result<(), String> {
    app.global_shortcut()
        .on_shortcut(accelerator, |app, _shortcut, event| {
            if event.state == ShortcutState::Pressed {
                toggle_launcher_from_hotkey(app);
            } else if event.state == ShortcutState::Released {
                app.state::<AppState>().release_launcher_hotkey();
            }
        })
        .map_err(|error| format!("（{error}）"))
}

fn unregister_launcher_binding(app: &AppHandle, accelerator: &str) -> Result<(), String> {
    app.global_shortcut()
        .unregister(accelerator)
        .map_err(|error| error.to_string())
}

/// Tries the saved preference first, then the documented default and a known
/// recovery binding. A bad conflict never leaves the resident application
/// without the tray's explicit Show iHub action.
fn register_launcher_hotkey(
    app: &AppHandle,
    preferred_accelerator: Option<String>,
) -> LauncherHotkeyStatus {
    for candidate in startup_launcher_hotkey_candidates(preferred_accelerator.as_deref()) {
        match register_launcher_binding(app, &candidate.accelerator) {
            Ok(()) if candidate.preferred => {
                return LauncherHotkeyStatus::configured(candidate.accelerator);
            }
            Ok(())
                if preferred_accelerator.is_none()
                    && candidate.accelerator == LAUNCHER_PRIMARY_HOTKEY =>
            {
                return LauncherHotkeyStatus::primary();
            }
            Ok(()) => {
                if let Some(preferred) = preferred_accelerator.as_deref() {
                    host_log::warn(
                        "hotkey",
                        format!(
                            "Could not activate preferred launcher hotkey {preferred}; using {} as a recovery binding. The tray action remains available.",
                            candidate.accelerator
                        ),
                    );
                } else {
                    host_log::warn(
                        "hotkey",
                        format!(
                            "Could not activate {LAUNCHER_PRIMARY_HOTKEY}; using {} as a recovery binding. The tray action remains available.",
                            candidate.accelerator
                        ),
                    );
                }
                return LauncherHotkeyStatus::fallback_for(
                    candidate.accelerator,
                    preferred_accelerator,
                );
            }
            Err(error) => {
                host_log::warn(
                    "hotkey",
                    format!(
                        "Could not register launcher hotkey {} {error}",
                        candidate.accelerator
                    ),
                );
            }
        }
    }

    host_log::error(
        "hotkey",
        "Could not register a launcher hotkey. The tray action remains available.",
    );
    LauncherHotkeyStatus::unavailable_for(preferred_accelerator)
}

fn launcher_reserved_plugin_shortcuts(state: &AppState) -> HashSet<String> {
    let status = state.launcher_hotkey_status();
    let mut reserved = HashSet::from([
        LAUNCHER_PRIMARY_HOTKEY.to_owned(),
        LAUNCHER_FALLBACK_HOTKEY.to_owned(),
    ]);
    reserved.extend(status.accelerator);
    reserved.extend(status.preferred_accelerator);
    reserved
}

fn register_plugin_shortcut_binding(app: &AppHandle, accelerator: &str) -> Result<(), String> {
    let accelerator_for_callback = accelerator.to_owned();
    let pressed = Arc::new(AtomicBool::new(false));
    app.global_shortcut()
        .on_shortcut(accelerator, move |app, _shortcut, event| {
            if event.state == ShortcutState::Pressed {
                if !pressed.swap(true, Ordering::AcqRel) {
                    dispatch_plugin_shortcut(app, &accelerator_for_callback);
                }
            } else {
                pressed.store(false, Ordering::Release);
            }
        })
        .map_err(|error| error.to_string())
}

/// Reconciles only plugin-owned registrations. The launcher's active and
/// recovery accelerators are reserved before this function touches the native
/// registry, and no failure here mutates the launcher's working binding.
fn refresh_plugin_shortcuts(app: &AppHandle) {
    let state = app.state::<AppState>();
    let _change_guard = state
        .plugin_shortcut_change
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut plugins = state.plugins.list();
    project_utools_dynamic_features(&state.plugins, &state.plugin_settings, &mut plugins);
    if let Err(error) = state
        .plugin_shortcut_preferences
        .apply_to_plugins(&mut plugins)
    {
        host_log::error(
            "hotkey",
            format!("Could not apply plugin shortcut preferences: {error}"),
        );
    }
    let mut plan = plan_plugin_shortcuts(&plugins, &launcher_reserved_plugin_shortcuts(&state));
    match state.plugin_shortcut_preferences.auto_copy_targets() {
        Ok(targets) => {
            for binding in &mut plan.ready {
                if let crate::plugin_shortcuts::PluginShortcutTarget::Command(command_id) =
                    &binding.target
                {
                    binding.auto_copy =
                        targets.contains(&(binding.plugin_id.clone(), command_id.clone()));
                }
            }
        }
        Err(error) => host_log::error(
            "hotkey",
            format!("Could not restore plugin shortcut auto-copy preferences: {error}"),
        ),
    }
    let mut eligibility = HashMap::<String, Result<(), String>>::new();
    plan.ready.retain(|binding| {
        let result = eligibility
            .entry(binding.plugin_id.clone())
            .or_insert_with(|| state.plugins.ensure_plugin_enabled(&binding.plugin_id));
        if let Err(error) = result {
            plan.statuses.insert(
                binding.key.clone(),
                PluginShortcutStatus::unavailable(format!(
                    "插件来源或生命周期校验失败，快捷键未注册：{error}"
                )),
            );
            false
        } else {
            true
        }
    });
    let previous = {
        state
            .plugin_shortcuts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .active
            .clone()
    };

    let desired = plan
        .ready
        .iter()
        .map(|binding| (binding.shortcut.as_str(), binding))
        .collect::<HashMap<_, _>>();
    let mut retained = HashMap::new();
    let mut failed_unregistrations = HashSet::new();
    for (accelerator, binding) in previous {
        if desired
            .get(accelerator.as_str())
            .is_some_and(|desired| **desired == binding)
        {
            plan.statuses
                .insert(binding.key.clone(), PluginShortcutStatus::registered());
            retained.insert(accelerator, binding);
            continue;
        }
        if let Err(error) = unregister_launcher_binding(app, &accelerator) {
            host_log::warn(
                "hotkey",
                format!("Could not unregister stale plugin shortcut {accelerator}: {error}"),
            );
            failed_unregistrations.insert(accelerator);
        }
    }
    drop(desired);

    for binding in plan.ready {
        if retained.contains_key(&binding.shortcut) {
            continue;
        }
        if failed_unregistrations.contains(&binding.shortcut) {
            plan.statuses.insert(
                binding.key,
                PluginShortcutStatus::unavailable(format!(
                    "无法安全移除这个快捷键的旧注册；本次未激活 {}。",
                    binding.shortcut
                )),
            );
            continue;
        }
        match register_plugin_shortcut_binding(app, &binding.shortcut) {
            Ok(()) => {
                plan.statuses
                    .insert(binding.key.clone(), PluginShortcutStatus::registered());
                retained.insert(binding.shortcut.clone(), binding);
            }
            Err(error) => {
                plan.statuses.insert(
                    binding.key,
                    PluginShortcutStatus::unavailable(format!(
                        "系统未能注册 {}；它可能已被其他应用占用。（{error}）",
                        binding.shortcut
                    )),
                );
            }
        }
    }

    let active_count = retained.len();
    let unavailable_count = plan
        .statuses
        .values()
        .filter(|status| status.registration != "registered")
        .count();
    *state
        .plugin_shortcuts
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = PluginShortcutRegistry {
        active: retained,
        statuses: plan.statuses,
    };
    host_log::info(
        "hotkey",
        format!(
            "Plugin shortcut reconciliation finished (active={active_count}, inactiveOrBlocked={unavailable_count})."
        ),
    );
    let _ = app.emit("ihub://plugin-shortcuts-changed", json!({}));
}

fn dispatch_plugin_shortcut(app: &AppHandle, accelerator: &str) {
    let binding = {
        let state = app.state::<AppState>();
        state.plugin_shortcut_binding(accelerator)
    };
    let Some(binding) = binding else {
        return;
    };
    // A lifecycle/source mutation may land just before its reconciliation
    // reaches the native registry. Re-read the validated manifest projection
    // so a stale callback can never invoke disabled or replaced code.
    let state = app.state::<AppState>();
    let mut plugins = state.plugins.list();
    project_utools_dynamic_features(&state.plugins, &state.plugin_settings, &mut plugins);
    if state
        .plugin_shortcut_preferences
        .apply_to_plugins(&mut plugins)
        .is_err()
    {
        return;
    }
    if state.host.auto_hide_is_suspended()
        || state
            .plugins
            .ensure_plugin_enabled(&binding.plugin_id)
            .is_err()
        || !binding_is_current(&plugins, &binding)
    {
        return;
    }
    let mut payload = PluginShortcutEvent::from_binding(&binding);
    if binding.auto_copy {
        let input = crate::clipboard_access::with_clipboard(|clipboard| clipboard.get_text())
            .ok()
            .map(|text| truncate_utf8_bytes(text, MAX_PLUGIN_CLIPBOARD_TEXT_BYTES))
            .filter(|text| !text.contains('\0'))
            .unwrap_or_default();
        payload.input = Some(input);
    }
    if !binding.auto_copy && binding_targets_frontend_command(&plugins, &binding) {
        match detached_plugin_event_target(app, &state, &binding.plugin_id) {
            Ok(Some(label)) => {
                // The label can only come from the host registry's exact
                // plugin-ID record. Never accept an event target from a
                // renderer or broadcast a detached command to every WebView.
                if let Err(error) = app.emit_to(&label, "ihub://plugin-global-shortcut", payload) {
                    host_log::warn(
                        "hotkey",
                        format!(
                            "Could not deliver detached plugin shortcut {}: {error}",
                            binding.shortcut
                        ),
                    );
                    return;
                }
                if let Some(window) = app.get_webview_window(&label) {
                    let _ = window.unminimize();
                    let _ = window.show();
                    let _ = window.set_focus();
                }
                return;
            }
            Ok(None) => {}
            Err(error) => {
                host_log::warn(
                    "hotkey",
                    format!(
                        "Could not route detached plugin shortcut {}: {error}",
                        binding.shortcut
                    ),
                );
                return;
            }
        }
    }

    // Keyword shortcuts and native-worker commands retain the launcher's
    // search/approval path. Target only the trusted main host so a detached
    // WebView cannot also observe and execute the same binding.
    show_launcher(app);
    if let Err(error) = app.emit_to("main", "ihub://plugin-global-shortcut", payload) {
        host_log::warn(
            "hotkey",
            format!(
                "Could not deliver plugin shortcut {}: {error}",
                binding.shortcut
            ),
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrayAction {
    Show,
    Settings,
    Reindex,
    About,
    Help,
    Feedback,
    Restart,
    Quit,
}

fn tray_action(menu_id: &str) -> Option<TrayAction> {
    match menu_id {
        "show" => Some(TrayAction::Show),
        "settings" => Some(TrayAction::Settings),
        "reindex" => Some(TrayAction::Reindex),
        "about" => Some(TrayAction::About),
        "help" => Some(TrayAction::Help),
        "feedback" => Some(TrayAction::Feedback),
        "restart" => Some(TrayAction::Restart),
        "quit" => Some(TrayAction::Quit),
        _ => None,
    }
}

fn is_fixed_tray_https_url(url: &str) -> bool {
    matches!(url, IHUB_HELP_URL | IHUB_FEEDBACK_URL) && url.starts_with("https://")
}

fn open_fixed_tray_url(url: &'static str) -> Result<(), String> {
    if !is_fixed_tray_https_url(url) {
        return Err("Tray links must use an iHub-owned fixed HTTPS destination.".to_owned());
    }
    open_external_in_system(url)
}

fn open_tray_surface(app: &AppHandle, section: &str) {
    show_launcher(app);
    let _ = app.emit(
        "ihub://tray-navigation",
        json!({ "surface": "settings", "section": section }),
    );
}

fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    // TrayIconBuilder does not inherit Tauri's default window icon. Windows
    // consequently keeps the resident process alive but has no visible tray
    // entry unless the packaged application icon is provided explicitly.
    let tray_icon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| tauri::Error::AssetNotFound("iHub tray icon".to_owned()))?;
    let show = MenuItem::with_id(app, "show", "显示 iHub", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "偏好设置", true, None::<&str>)?;
    let reindex = MenuItem::with_id(app, "reindex", "刷新文件索引", true, None::<&str>)?;
    let about = MenuItem::with_id(app, "about", "关于 iHub", true, None::<&str>)?;
    let help = MenuItem::with_id(app, "help", "帮助", true, None::<&str>)?;
    let feedback = MenuItem::with_id(app, "feedback", "反馈", true, None::<&str>)?;
    let restart = MenuItem::with_id(app, "restart", "重启 iHub", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出 iHub", true, None::<&str>)?;
    let separator_one = PredefinedMenuItem::separator(app)?;
    let separator_two = PredefinedMenuItem::separator(app)?;
    let separator_three = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(
        app,
        &[
            &show,
            &settings,
            &separator_one,
            &reindex,
            &separator_two,
            &about,
            &help,
            &feedback,
            &separator_three,
            &restart,
            &quit,
        ],
    )?;
    let _tray = TrayIconBuilder::with_id("ihub-tray")
        .icon(tray_icon)
        .tooltip("iHub")
        .menu(&menu)
        .on_menu_event(|app, event| match tray_action(event.id().as_ref()) {
            Some(TrayAction::Show) => show_launcher(app),
            Some(TrayAction::Settings) => open_tray_surface(app, "preferences"),
            Some(TrayAction::Reindex) => {
                app.state::<AppState>().index.rebuild_default_roots();
            }
            Some(TrayAction::About) => open_tray_surface(app, "about"),
            Some(TrayAction::Help) => {
                if let Err(error) = open_fixed_tray_url(IHUB_HELP_URL) {
                    host_log::warn(
                        "lifecycle",
                        format!("Could not open the fixed help URL: {error}"),
                    );
                }
            }
            Some(TrayAction::Feedback) => {
                if let Err(error) = open_fixed_tray_url(IHUB_FEEDBACK_URL) {
                    host_log::warn(
                        "lifecycle",
                        format!("Could not open the fixed feedback URL: {error}"),
                    );
                }
            }
            Some(TrayAction::Restart) => {
                host_log::info("lifecycle", "User requested a host restart.");
                if let Err(error) = app.state::<AppState>().super_panel.shutdown_listener() {
                    host_log::error(
                        "super-panel",
                        format!("Could not stop the listener before restart: {error}"),
                    );
                }
                app.restart();
            }
            Some(TrayAction::Quit) => quit_app(app.clone()),
            _ => {}
        })
        .build(app)?;
    Ok(())
}

fn ensure_super_panel_listener(app: &AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let callback_app = app.clone();
    match state
        .super_panel
        .ensure_listener(move |trigger| reveal_super_panel(&callback_app, trigger))
    {
        Ok(()) => Ok(()),
        Err(error) => {
            state.super_panel.listener_failed(error.clone());
            Err(error)
        }
    }
}

fn reveal_super_panel(app: &AppHandle, trigger: SuperPanelTrigger) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let event = {
        let state = app.state::<AppState>();
        if state.host.auto_hide_is_suspended() {
            return;
        }
        state.super_panel.issue_context(trigger)
    };
    let Some(event) = event else {
        return;
    };
    host_log::debug(
        "super-panel",
        "A deliberate long-right-click opened the compact launcher.",
    );

    if let Some(state) = window.try_state::<AppState>() {
        state.capture_previous_foreground();
        state.launcher_focus.begin_reveal();
    }
    let _ = window.unminimize();
    apply_super_panel_reveal_geometry(
        &window,
        PhysicalPosition::new(event.physical_x, event.physical_y),
    );
    let _ = window.show();
    let _ = window.set_focus();
    let _ = window.emit(
        "ihub://focus-search",
        json!({
            "freshReveal": true,
            "reason": "explicit",
        }),
    );
    let _ = window.emit("ihub://super-panel", &event);
}

fn apply_super_panel_reveal_geometry<R: tauri::Runtime>(
    window: &tauri::WebviewWindow<R>,
    trigger: PhysicalPosition<i32>,
) {
    let monitor = window
        .available_monitors()
        .ok()
        .and_then(|monitors| {
            monitors.into_iter().find(|monitor| {
                physical_point_in_monitor(
                    PhysicalPosition::new(f64::from(trigger.x), f64::from(trigger.y)),
                    *monitor.position(),
                    *monitor.size(),
                )
            })
        })
        .or_else(|| window.current_monitor().ok().flatten())
        .or_else(|| window.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        apply_launcher_reveal_geometry(window);
        return;
    };
    let work_area = monitor.work_area();
    let Some(layout) = (LauncherWorkArea {
        position: work_area.position,
        size: work_area.size,
    })
    .super_panel_layout(monitor.scale_factor(), trigger) else {
        return;
    };
    if let Err(error) = window.set_size(layout.size) {
        host_log::warn(
            "super-panel",
            format!("Could not size the launcher surface: {error}"),
        );
    }
    if let Err(error) = window.set_position(layout.position) {
        host_log::warn(
            "super-panel",
            format!("Could not anchor the launcher surface: {error}"),
        );
    }
}

fn show_launcher(app: &AppHandle) {
    apply_launcher_visibility(app, LauncherInvocationSource::Explicit);
}

fn toggle_launcher_from_hotkey(app: &AppHandle) {
    {
        let state = app.state::<AppState>();
        // A native file dialog or explicit capture focus lease owns the foreground
        // until it finishes. Toggling the parent beneath it would either strand
        // the dialog or steal focus from a system picker.
        if !state.accept_launcher_hotkey_press() || state.host.auto_hide_is_suspended() {
            return;
        }
    }
    apply_launcher_visibility(app, LauncherInvocationSource::Hotkey);
}

fn apply_launcher_visibility(app: &AppHandle, source: LauncherInvocationSource) {
    if let Some(window) = app.get_webview_window("main") {
        let snapshot = LauncherVisibilitySnapshot {
            visible: window.is_visible().unwrap_or(false),
            // If the platform cannot report focus, fail toward preserving the
            // visible surface instead of unexpectedly hiding it.
            focused: window.is_focused().unwrap_or(false),
        };
        let action = launcher_visibility_action(source, snapshot);
        if action == LauncherVisibilityAction::Hide {
            let _ = window.emit("ihub://hide-search", json!({ "reason": source.reason() }));
            let _ = window.hide();
            return;
        }

        // Preserve a long-press drag for the current visible session. Hiding
        // the resident launcher clears that temporary placement on the next
        // reveal because no position is ever written to durable state.
        let fresh_reveal = action == LauncherVisibilityAction::RevealFresh;
        if fresh_reveal {
            if let Some(state) = window.try_state::<AppState>() {
                state.capture_previous_foreground();
                state.launcher_focus.begin_reveal();
            }
        }
        let _ = window.unminimize();
        if fresh_reveal {
            // The launcher is intentionally ephemeral: a user may drag it
            // while it is visible, but every hidden-to-visible reveal starts
            // from its Spotlight-like centered position. We center against
            // the active monitor's *work area*, not its full bounds, so the
            // taskbar/dock stays reachable and a small monitor can never
            // receive an oversized off-screen native window.
            apply_launcher_reveal_geometry(&window);
        }
        let _ = window.show();
        let _ = window.set_focus();
        let _ = window.emit(
            "ihub://focus-search",
            json!({
                "freshReveal": fresh_reveal,
                "reason": source.reason(),
            }),
        );
    }
}

fn apply_launcher_reveal_geometry<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) {
    // Spotlight/uTools-style invocation follows the person's current pointer
    // across displays. The hidden window's previous monitor and the primary
    // display remain deterministic fallbacks when the runtime cannot report a
    // cursor or display topology. No drag position is persisted.
    let monitor = window
        .cursor_position()
        .ok()
        .and_then(|cursor| {
            window
                .available_monitors()
                .ok()?
                .into_iter()
                .find(|monitor| {
                    physical_point_in_monitor(cursor, *monitor.position(), *monitor.size())
                })
        })
        .or_else(|| window.current_monitor().ok().flatten())
        .or_else(|| window.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        // Without any display, moving/resizing is neither useful nor safe.
        // Leave the existing native geometry untouched until a display is
        // available rather than guessing an off-screen coordinate.
        host_log::warn("window", "Could not find a display for launcher reveal.");
        return;
    };

    let work_area = monitor.work_area();
    let Some(layout) = (LauncherWorkArea {
        position: work_area.position,
        size: work_area.size,
    })
    .reveal_layout(monitor.scale_factor()) else {
        host_log::warn("window", "The display has no usable launcher work area.");
        return;
    };

    if let Err(error) = window.set_size(layout.size) {
        host_log::warn(
            "window",
            format!("Could not fit the launcher into the display work area: {error}"),
        );
    }
    if let Err(error) = window.set_position(layout.position) {
        host_log::warn(
            "window",
            format!("Could not center the launcher in the display work area: {error}"),
        );
    }
}

fn temporary_path_open_id_is_valid(open_id: &str) -> bool {
    open_id.len() <= MAX_TEMPORARY_PATH_OPEN_ID_BYTES
        && open_id
            .strip_prefix("open-")
            .and_then(|value| Uuid::parse_str(value).ok())
            .is_some()
}

fn open_external_in_system(url: &str) -> Result<(), String> {
    validate_external_url(url)?;
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = background_command("explorer.exe");
        command.arg(url);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = background_command("open");
        command.arg(url);
        command
    };
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut command = {
        let mut command = background_command("xdg-open");
        command.arg(url);
        command
    };
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Could not open external URL: {error}"))
}

#[cfg(target_os = "windows")]
fn play_utools_system_beep() -> Result<(), String> {
    use windows::Win32::{System::Diagnostics::Debug::MessageBeep, UI::WindowsAndMessaging::MB_OK};
    unsafe { MessageBeep(MB_OK) }
        .map_err(|error| format!("Windows could not play the system notification sound: {error}"))
}

#[cfg(not(target_os = "windows"))]
fn play_utools_system_beep() -> Result<(), String> {
    Err("uTools shellBeep has not been runtime-verified on this platform.".to_owned())
}

fn validate_external_url(value: &str) -> Result<(), String> {
    const MAX_EXTERNAL_URL_CHARS: usize = 2_048;
    if value.is_empty()
        || value.chars().count() > MAX_EXTERNAL_URL_CHARS
        || value.chars().any(char::is_control)
    {
        return Err("External URL is empty, too long, or contains control characters.".to_owned());
    }
    let parsed = url::Url::parse(value).map_err(|_| "External URL is invalid.".to_owned())?;
    match parsed.scheme() {
        "http" | "https" if parsed.host_str().is_some() => Ok(()),
        "mailto" if !parsed.path().is_empty() => Ok(()),
        _ => Err(
            "Only absolute http(s) URLs and mailto recipients can be opened externally.".to_owned(),
        ),
    }
}

fn validate_plugin_clipboard_text(value: &str) -> Result<(), String> {
    if value.len() > MAX_PLUGIN_CLIPBOARD_TEXT_BYTES {
        return Err(format!(
            "Plugin clipboard text exceeds the {} KiB limit.",
            MAX_PLUGIN_CLIPBOARD_TEXT_BYTES / 1024
        ));
    }
    Ok(())
}

fn validate_utools_input_text(
    params: &Value,
    max_bytes: usize,
    max_chars: Option<usize>,
) -> Result<&str, String> {
    let Some(object) = params.as_object() else {
        return Err("uTools input parameters must be an object.".to_owned());
    };
    if object.keys().any(|key| key != "value") {
        return Err("uTools input requests accept only a text value.".to_owned());
    }
    let value = required_string(params, "value")?;
    if value.len() > max_bytes || value.contains('\0') {
        return Err("uTools input text is too large or contains a null character.".to_owned());
    }
    if max_chars.is_some_and(|limit| value.chars().count() > limit) {
        return Err(format!(
            "uTools typed text is limited to {} characters.",
            max_chars.unwrap_or_default()
        ));
    }
    Ok(value)
}

fn validate_exact_utools_simulation_params(params: &Value, allowed: &[&str]) -> Result<(), String> {
    let Some(object) = params.as_object() else {
        return Err("uTools simulation parameters must be an object.".to_owned());
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("uTools simulation parameters contain an unsupported field.".to_owned());
    }
    Ok(())
}

fn utools_virtual_key(value: &str) -> Option<(u16, bool)> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.len() == 1 {
        let byte = normalized.as_bytes()[0];
        if byte.is_ascii_lowercase() {
            return Some((u16::from(byte.to_ascii_uppercase()), false));
        }
        if byte.is_ascii_digit() {
            return Some((u16::from(byte), false));
        }
        return match byte {
            b' ' => Some((0x20, false)),
            b'`' => Some((0xc0, false)),
            b'~' => Some((0xc0, true)),
            b'-' => Some((0xbd, false)),
            b'_' => Some((0xbd, true)),
            b'=' => Some((0xbb, false)),
            b'+' => Some((0xbb, true)),
            b'[' => Some((0xdb, false)),
            b'{' => Some((0xdb, true)),
            b']' => Some((0xdd, false)),
            b'}' => Some((0xdd, true)),
            b'\\' => Some((0xdc, false)),
            b'|' => Some((0xdc, true)),
            b';' => Some((0xba, false)),
            b':' => Some((0xba, true)),
            b'\'' => Some((0xde, false)),
            b'"' => Some((0xde, true)),
            b',' => Some((0xbc, false)),
            b'<' => Some((0xbc, true)),
            b'.' => Some((0xbe, false)),
            b'>' => Some((0xbe, true)),
            b'/' => Some((0xbf, false)),
            b'?' => Some((0xbf, true)),
            b'!' => Some((b'1'.into(), true)),
            b'@' => Some((b'2'.into(), true)),
            b'#' => Some((b'3'.into(), true)),
            b'$' => Some((b'4'.into(), true)),
            b'%' => Some((b'5'.into(), true)),
            b'^' => Some((b'6'.into(), true)),
            b'&' => Some((b'7'.into(), true)),
            b'*' => Some((b'8'.into(), true)),
            b'(' => Some((b'9'.into(), true)),
            b')' => Some((b'0'.into(), true)),
            _ => None,
        };
    }
    if let Some(number) = normalized
        .strip_prefix('f')
        .and_then(|number| number.parse::<u16>().ok())
        .filter(|number| (1..=24).contains(number))
    {
        return Some((0x70 + number - 1, false));
    }
    if let Some(number) = normalized
        .strip_prefix("numpad")
        .map(|number| number.trim_start_matches('_'))
        .and_then(|number| number.parse::<u16>().ok())
        .filter(|number| *number <= 9)
    {
        return Some((0x60 + number, false));
    }
    let key = match normalized.as_str() {
        "backspace" => 0x08,
        "tab" => 0x09,
        "clear" => 0x0c,
        "enter" | "return" => 0x0d,
        "shift" => 0x10,
        "left_shift" => 0xa0,
        "right_shift" => 0xa1,
        "control" | "ctrl" => 0x11,
        "left_control" | "left_ctrl" => 0xa2,
        "right_control" | "right_ctrl" => 0xa3,
        "option" | "alt" => 0x12,
        "left_alt" | "left_option" => 0xa4,
        "right_alt" | "right_option" => 0xa5,
        "pause" => 0x13,
        "capslock" | "caps_lock" => 0x14,
        "escape" | "esc" => 0x1b,
        "space" => 0x20,
        "pageup" | "page_up" => 0x21,
        "pagedown" | "page_down" => 0x22,
        "end" => 0x23,
        "home" => 0x24,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        "printscreen" | "print_screen" => 0x2c,
        "insert" => 0x2d,
        "delete" => 0x2e,
        "command" | "super" | "meta" | "left_command" | "left_super" | "left_meta" => 0x5b,
        "right_command" | "right_super" | "right_meta" => 0x5c,
        "menu" | "contextmenu" | "context_menu" => 0x5d,
        "multiply" => 0x6a,
        "add" => 0x6b,
        "subtract" => 0x6d,
        "decimal" => 0x6e,
        "divide" => 0x6f,
        "numlock" | "num_lock" => 0x90,
        "scrolllock" | "scroll_lock" => 0x91,
        "browser_back" => 0xa6,
        "browser_forward" => 0xa7,
        "browser_refresh" => 0xa8,
        "browser_stop" => 0xa9,
        "browser_search" => 0xaa,
        "browser_favorites" => 0xab,
        "browser_home" => 0xac,
        "audio_mute" | "volume_mute" => 0xad,
        "audio_vol_down" | "volume_down" => 0xae,
        "audio_vol_up" | "volume_up" => 0xaf,
        "audio_next" => 0xb0,
        "audio_prev" | "audio_previous" => 0xb1,
        "audio_stop" => 0xb2,
        "audio_play" | "audio_pause" | "audio_play_pause" => 0xb3,
        _ => return None,
    };
    Some((key, false))
}

fn utools_modifier_key(value: &str) -> Option<(u16, &'static str)> {
    match value.trim().to_ascii_lowercase().as_str() {
        "control" | "ctrl" => Some((0x11, "Ctrl")),
        "shift" => Some((0x10, "Shift")),
        "option" | "alt" => Some((0x12, "Alt")),
        "command" | "super" | "meta" => Some((0x5b, "Meta")),
        _ => None,
    }
}

#[cfg(target_os = "windows")]
fn utools_windows_key_is_extended(key: u16) -> bool {
    matches!(
        key,
        0x21..=0x2e
            | 0x5b
            | 0x5c
            | 0x6f
            | 0x90
            | 0xa3
            | 0xa5
            | 0xa6..=0xb3
    )
}

fn point_is_on_physical_display(
    x: i32,
    y: i32,
    bounds: &[crate::utools_screen::ScreenRect],
) -> bool {
    bounds.iter().any(|bounds| {
        let right = i64::from(bounds.x) + i64::from(bounds.width);
        let bottom = i64::from(bounds.y) + i64::from(bounds.height);
        i64::from(x) >= i64::from(bounds.x)
            && i64::from(x) < right
            && i64::from(y) >= i64::from(bounds.y)
            && i64::from(y) < bottom
    })
}

fn validate_utools_simulation_action(
    method: &str,
    params: &Value,
    physical_display_bounds: &[crate::utools_screen::ScreenRect],
    current_cursor: Option<(i32, i32)>,
) -> Result<UtoolsSimulationAction, String> {
    if method == "compatibility.utools.simulate.keyboardTap" {
        validate_exact_utools_simulation_params(params, &["key", "modifiers"])?;
        let key_label = required_string(params, "key")?.trim();
        if key_label.is_empty()
            || key_label.chars().count() > 32
            || key_label.chars().any(char::is_control)
        {
            return Err(
                "uTools simulated key is empty, too long, or contains controls.".to_owned(),
            );
        }
        let (key, implied_shift) = utools_virtual_key(key_label)
            .ok_or_else(|| "uTools simulated key is not supported on Windows.".to_owned())?;
        let raw_modifiers = params
            .get("modifiers")
            .and_then(Value::as_array)
            .ok_or_else(|| "uTools simulated modifiers must be an array.".to_owned())?;
        if raw_modifiers.len() > 4 {
            return Err("uTools keyboard simulation accepts at most four modifiers.".to_owned());
        }
        let mut modifiers = Vec::new();
        let mut modifier_labels = Vec::new();
        if implied_shift {
            modifiers.push(0x10);
            modifier_labels.push("Shift".to_owned());
        }
        for modifier in raw_modifiers {
            let modifier = modifier
                .as_str()
                .and_then(utools_modifier_key)
                .ok_or_else(|| "uTools simulated keyboard modifier is invalid.".to_owned())?;
            if !modifiers.contains(&modifier.0) {
                modifiers.push(modifier.0);
                modifier_labels.push(modifier.1.to_owned());
            }
        }
        if modifiers.contains(&key) {
            return Err("uTools simulated key must differ from its modifiers.".to_owned());
        }
        return Ok(UtoolsSimulationAction::KeyboardTap {
            key,
            key_label: key_label.to_owned(),
            modifiers,
            modifier_labels,
        });
    }

    let is_move = method == "compatibility.utools.simulate.mouseMove";
    let (button, double) = match method {
        "compatibility.utools.simulate.mouseClick" => (UtoolsMouseButton::Left, false),
        "compatibility.utools.simulate.mouseDoubleClick" => (UtoolsMouseButton::Left, true),
        "compatibility.utools.simulate.mouseRightClick" => (UtoolsMouseButton::Right, false),
        "compatibility.utools.simulate.mouseMove" => (UtoolsMouseButton::Left, false),
        _ => return Err("Unsupported uTools simulation method.".to_owned()),
    };
    validate_exact_utools_simulation_params(params, &["x", "y"])?;
    let object = params
        .as_object()
        .ok_or_else(|| "uTools simulation parameters must be an object.".to_owned())?;
    let has_x = object.contains_key("x");
    let has_y = object.contains_key("y");
    if has_x != has_y || (is_move && !has_x) {
        return Err(
            "uTools mouse simulation requires both integer x and y coordinates.".to_owned(),
        );
    }
    let (x, y) = if has_x {
        let x = object
            .get("x")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok());
        let y = object
            .get("y")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok());
        x.zip(y)
            .ok_or_else(|| "uTools mouse coordinates must be 32-bit integers.".to_owned())?
    } else {
        current_cursor
            .ok_or_else(|| "Windows could not read the current pointer position.".to_owned())?
    };
    if !point_is_on_physical_display(x, y, physical_display_bounds) {
        return Err("uTools mouse coordinates must fall on an active display.".to_owned());
    }
    if is_move {
        Ok(UtoolsSimulationAction::MouseMove { x, y })
    } else {
        Ok(UtoolsSimulationAction::MouseClick {
            x,
            y,
            button,
            double,
        })
    }
}

fn hide_main_for_utools_input(app: &AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "The iHub main window is unavailable for uTools input.".to_owned())?;
    window
        .hide()
        .map_err(|error| format!("Could not hide iHub before uTools input: {error}"))
}

fn hide_and_schedule_utools_input(
    app: &AppHandle,
    action: UtoolsInputAction,
) -> Result<(), String> {
    hide_main_for_utools_input(app)?;
    if let Err(error) = schedule_utools_input(action) {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.show();
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn schedule_utools_input(action: UtoolsInputAction) -> Result<(), String> {
    std::thread::Builder::new()
        .name("ihub-utools-input".to_owned())
        .spawn(move || {
            std::thread::sleep(Duration::from_millis(180));
            if let Err(error) = send_utools_windows_input(action) {
                host_log::warn("plugins", error);
            }
        })
        .map(|_| ())
        .map_err(|error| format!("Could not start the deferred uTools input task: {error}"))
}

#[cfg(not(target_os = "windows"))]
fn schedule_utools_input(_action: UtoolsInputAction) -> Result<(), String> {
    Err("uTools input compatibility has not been runtime-verified on this platform.".to_owned())
}

#[cfg(target_os = "windows")]
fn send_utools_windows_input(action: UtoolsInputAction) -> Result<(), String> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
        KEYEVENTF_UNICODE, VIRTUAL_KEY, VK_CONTROL, VK_V,
    };

    fn keyboard_input(key: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: key,
                    wScan: scan,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    fn send(inputs: &[INPUT]) -> Result<(), String> {
        let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent as usize == inputs.len() {
            Ok(())
        } else {
            Err(format!(
                "Windows accepted {sent} of {} deferred uTools input events.",
                inputs.len()
            ))
        }
    }

    match action {
        UtoolsInputAction::PasteClipboard => send(&[
            keyboard_input(VK_CONTROL, 0, KEYBD_EVENT_FLAGS(0)),
            keyboard_input(VK_V, 0, KEYBD_EVENT_FLAGS(0)),
            keyboard_input(VK_V, 0, KEYEVENTF_KEYUP),
            keyboard_input(VK_CONTROL, 0, KEYEVENTF_KEYUP),
        ]),
        UtoolsInputAction::TypeString(value) => {
            let mut inputs = Vec::with_capacity(256);
            for code_unit in value.encode_utf16() {
                inputs.push(keyboard_input(VIRTUAL_KEY(0), code_unit, KEYEVENTF_UNICODE));
                inputs.push(keyboard_input(
                    VIRTUAL_KEY(0),
                    code_unit,
                    KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
                ));
                if inputs.len() >= 256 {
                    send(&inputs)?;
                    inputs.clear();
                }
            }
            if !inputs.is_empty() {
                send(&inputs)?;
            }
            Ok(())
        }
    }
}

fn utools_db_storage_key(key: &str) -> Result<String, String> {
    if key.len() > MAX_UTOOLS_DB_STORAGE_KEY_BYTES {
        return Err(format!(
            "uTools dbStorage keys must not exceed {MAX_UTOOLS_DB_STORAGE_KEY_BYTES} UTF-8 bytes."
        ));
    }
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(UTOOLS_DB_STORAGE_PREFIX.len() + key.len() * 2);
    encoded.push_str(UTOOLS_DB_STORAGE_PREFIX);
    for byte in key.as_bytes() {
        encoded.push(HEX[usize::from(byte >> 4)] as char);
        encoded.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    Ok(encoded)
}

fn decode_utools_db_storage_key(encoded: &str) -> Option<String> {
    if encoded.len() > MAX_UTOOLS_DB_STORAGE_KEY_BYTES * 2 || encoded.len() % 2 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(encoded.len() / 2);
    for pair in encoded.as_bytes().chunks_exact(2) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        bytes.push(((high << 4) | low) as u8);
    }
    String::from_utf8(bytes).ok()
}

fn utools_dynamic_feature_command_id(code: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in code.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("utools-dynamic-{hash:016x}")
}

fn utools_dynamic_feature_key(code: &str) -> String {
    format!(
        "{UTOOLS_DYNAMIC_FEATURE_PREFIX}{}",
        utools_dynamic_feature_command_id(code)
            .strip_prefix("utools-dynamic-")
            .unwrap_or_default()
    )
}

fn validate_utools_dynamic_feature(value: &Value) -> Result<UtoolsDynamicFeature, String> {
    let mut feature = serde_json::from_value::<UtoolsDynamicFeature>(value.clone())
        .map_err(|_| "uTools dynamic feature fields are malformed or unsupported.".to_owned())?;
    feature.code = feature.code.trim().to_owned();
    if feature.code.is_empty()
        || feature.code.chars().count() > 160
        || feature.code.chars().any(char::is_control)
    {
        return Err(
            "uTools dynamic feature code must contain 1-160 non-control characters.".to_owned(),
        );
    }
    feature.explain = feature
        .explain
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if feature
        .explain
        .as_ref()
        .is_some_and(|value| value.chars().count() > 240 || value.chars().any(char::is_control))
    {
        return Err(
            "uTools dynamic feature explanations are limited to 240 characters.".to_owned(),
        );
    }
    if feature
        .icon
        .as_ref()
        .is_some_and(|value| value.chars().count() > 2_048 || value.chars().any(char::is_control))
    {
        return Err("uTools dynamic feature icon metadata is too large or invalid.".to_owned());
    }
    let platforms = match feature.platform.as_ref() {
        None => Vec::new(),
        Some(UtoolsDynamicPlatforms::One(platform)) => vec![platform.as_str()],
        Some(UtoolsDynamicPlatforms::Many(platforms)) => {
            if platforms.is_empty() || platforms.len() > 3 {
                return Err(
                    "uTools dynamic feature platform lists must contain 1-3 items.".to_owned(),
                );
            }
            platforms.iter().map(String::as_str).collect()
        }
    };
    if platforms
        .iter()
        .any(|platform| !matches!(*platform, "win32" | "darwin" | "linux"))
    {
        return Err("uTools dynamic feature platforms must be win32, darwin, or linux.".to_owned());
    }
    if feature.cmds.is_empty() || feature.cmds.len() > MAX_UTOOLS_DYNAMIC_COMMANDS {
        return Err(format!(
            "uTools dynamic features require 1-{MAX_UTOOLS_DYNAMIC_COMMANDS} direct text commands."
        ));
    }
    let mut commands = Vec::with_capacity(feature.cmds.len());
    for command in feature.cmds {
        let command = command.trim().to_owned();
        if command.is_empty()
            || command.chars().count() > 80
            || command.chars().any(char::is_control)
        {
            return Err(
                "uTools dynamic feature commands must contain 1-80 non-control characters."
                    .to_owned(),
            );
        }
        if !commands.contains(&command) {
            commands.push(command);
        }
    }
    feature.cmds = commands;
    Ok(feature)
}

fn utools_dynamic_features(
    settings: &PluginSettingsStore,
    plugin_id: &str,
) -> Vec<UtoolsDynamicFeature> {
    settings
        .snapshot_with_prefix(plugin_id, UTOOLS_DYNAMIC_FEATURE_PREFIX)
        .into_values()
        .filter_map(|value| validate_utools_dynamic_feature(&value).ok())
        .collect()
}

fn utools_dynamic_feature_matches_platform(feature: &UtoolsDynamicFeature) -> bool {
    let current = if cfg!(target_os = "windows") {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    match feature.platform.as_ref() {
        None => true,
        Some(UtoolsDynamicPlatforms::One(platform)) => platform == current,
        Some(UtoolsDynamicPlatforms::Many(platforms)) => {
            platforms.iter().any(|platform| platform == current)
        }
    }
}

fn has_declared_plugin_search_provider(
    state: &AppState,
    plugin_id: &str,
    provider_id: &str,
) -> Result<bool, String> {
    if state
        .plugins
        .has_declared_search_provider(plugin_id, provider_id)?
    {
        return Ok(true);
    }
    if provider_id != UTOOLS_MAIN_PUSH_PROVIDER_ID
        || !state.plugins.uses_utools_compatibility(plugin_id)?
    {
        return Ok(false);
    }
    Ok(utools_dynamic_features(&state.plugin_settings, plugin_id)
        .iter()
        .any(|feature| {
            feature.main_push == Some(true) && utools_dynamic_feature_matches_platform(feature)
        }))
}

fn project_utools_dynamic_features(
    plugins: &PluginManager,
    settings: &PluginSettingsStore,
    plugin_infos: &mut [PluginInfo],
) {
    for plugin in plugin_infos {
        if !plugins
            .uses_utools_compatibility(&plugin.id)
            .unwrap_or(false)
        {
            continue;
        }
        let features = utools_dynamic_features(settings, &plugin.id);
        if features.iter().any(|feature| {
            feature.main_push == Some(true) && utools_dynamic_feature_matches_platform(feature)
        }) && !plugin
            .search_providers
            .iter()
            .any(|provider| provider.id == UTOOLS_MAIN_PUSH_PROVIDER_ID)
        {
            plugin
                .search_providers
                .push(crate::models::PluginSearchProviderInfo {
                    id: UTOOLS_MAIN_PUSH_PROVIDER_ID.to_owned(),
                    title: "uTools 主搜索推送".to_owned(),
                    trigger: None,
                    priority: Some(20),
                });
        }
        for feature in features {
            if !utools_dynamic_feature_matches_platform(&feature) {
                continue;
            }
            let command_id = utools_dynamic_feature_command_id(&feature.code);
            if plugin
                .commands
                .iter()
                .any(|command| command.id == command_id)
            {
                continue;
            }
            plugin.commands.push(PluginCommandInfo {
                id: command_id,
                name: feature
                    .explain
                    .clone()
                    .unwrap_or_else(|| feature.cmds[0].clone()),
                description: feature.explain.clone(),
                icon_src: None,
                execution: "frontend".to_owned(),
                keywords: feature.cmds,
                utools_text_matchers: Vec::new(),
                shortcut: None,
                shortcut_registration: None,
                shortcut_error: None,
            });
        }
        plugin.command_count = plugin.commands.len();
    }
}

fn validate_utools_window_request_params(
    params: &Value,
    allowed_keys: &[&str],
) -> Result<(), String> {
    let Some(object) = params.as_object() else {
        return Err("uTools window compatibility parameters must be an object.".to_owned());
    };
    if object
        .keys()
        .any(|key| !allowed_keys.contains(&key.as_str()))
    {
        return Err("uTools window compatibility request contains unsupported options.".to_owned());
    }
    Ok(())
}

fn validate_utools_expend_height(params: &Value) -> Result<u32, String> {
    validate_utools_window_request_params(params, &["height"])?;
    let height = params
        .get("height")
        .and_then(Value::as_u64)
        .and_then(|height| u32::try_from(height).ok())
        .ok_or_else(|| "uTools setExpendHeight expects an integer height.".to_owned())?;
    if !(100..=900).contains(&height) {
        return Err("uTools setExpendHeight accepts heights from 100 to 900 pixels.".to_owned());
    }
    Ok(height)
}

fn required_value<'a>(params: &'a Value, key: &str) -> Result<&'a Value, String> {
    params
        .get(key)
        .ok_or_else(|| format!("Plugin host method requires params.{key}."))
}

fn validate_exact_plugin_params(params: &Value, keys: &[&str]) -> Result<(), String> {
    let Some(object) = params.as_object() else {
        return Err("Plugin host method parameters must be an object.".to_owned());
    };
    if object.len() != keys.len() || keys.iter().any(|key| !object.contains_key(*key)) {
        return Err(format!(
            "Plugin host method accepts exactly params.{}.",
            keys.join(" and params.")
        ));
    }
    Ok(())
}

fn validate_optional_plugin_param(params: &Value, key: &str) -> Result<(), String> {
    let Some(object) = params.as_object() else {
        return Err("Plugin host method parameters must be an object.".to_owned());
    };
    if object.len() > usize::from(object.contains_key(key)) {
        return Err(format!(
            "Plugin host method accepts only the optional params.{key}."
        ));
    }
    Ok(())
}

fn required_string<'a>(params: &'a Value, key: &str) -> Result<&'a str, String> {
    required_value(params, key)?
        .as_str()
        .ok_or_else(|| format!("Plugin host method requires params.{key} to be a string."))
}

/// `cursorColor.sampleOnce` accepts no plugin-controlled capture options. The
/// parent injects the sole opaque approval after its own user confirmation;
/// rejecting every other key prevents future callers from smuggling a delay,
/// coordinate, rectangle, monitor, or a boolean "approved" bypass through the
/// generic JSON bridge.
fn cursor_color_approval_id(params: &Value) -> Result<&str, String> {
    let Some(object) = params.as_object() else {
        return Err("cursorColor.sampleOnce requires a host-issued approvalId.".to_owned());
    };
    if object.len() != 1 || !object.contains_key("approvalId") {
        return Err(
            "cursorColor.sampleOnce accepts only the host-issued approvalId; it has no capture options."
                .to_owned(),
        );
    }
    required_string(params, "approvalId")
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

pub(crate) fn is_plugin_id(value: &str) -> bool {
    let length = value.len();
    (2..=96).contains(&length)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn host_key(plugin_id: &str, child_id: &str) -> String {
    format!("{plugin_id}:{child_id}")
}

fn optional_bool(params: &Value, key: &str) -> Result<Option<bool>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(format!("{key} must be a boolean when provided.")),
    }
}

fn optional_u32(params: &Value, key: &str) -> Result<Option<u32>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => value
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| format!("{key} must be an unsigned 32-bit integer when provided.")),
        Some(_) => Err(format!(
            "{key} must be an unsigned 32-bit integer when provided."
        )),
    }
}

fn optional_u8(params: &Value, key: &str) -> Result<Option<u8>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => value
            .as_u64()
            .and_then(|value| u8::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| format!("{key} must be an unsigned 8-bit integer when provided.")),
        Some(_) => Err(format!(
            "{key} must be an unsigned 8-bit integer when provided."
        )),
    }
}

fn next_capability_id(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::new_v4())
}

fn next_request_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("req-{}-{nanos}", std::process::id())
}

#[cfg(test)]
mod tests {
    use std::{
        borrow::Cow,
        fs,
        path::Path,
        sync::mpsc,
        time::{Duration, Instant},
    };

    use serde_json::json;
    use tauri::{PhysicalPosition, PhysicalSize};

    use crate::clipboard_history::ClipboardHistory;
    use crate::models::{
        LauncherHotkeyRegistration, LauncherHotkeyStatus, PluginSearchResult, UtoolsTextMatcherInfo,
    };

    use super::{
        attach_plugin_launcher_context_transfer, authorize_index_root_update,
        build_plugin_launcher_context_payload, canonical_selected_file,
        claim_utools_main_push_interaction, clear_plugin_runtime_state,
        clear_plugin_session_secrets, clipboard_files_from_paths, clipboard_image_from_rgba,
        complete_plugin_search, create_plugin_project_for_grant,
        create_plugin_project_with_open_grant, cursor_color_approval_id,
        decode_authorized_utools_clipboard_image, decode_utools_clipboard_png_data_url,
        decode_utools_db_storage_key, directory_for_grant, get_plugin_session_secret,
        issue_file_grant, issue_filesystem_grant, issue_plugin_launcher_context_transfer,
        launcher_visibility_action, native_plugin_command_input, normalize_plugin_search_results,
        normalize_utools_main_push_selection, normalized_host_target, optional_u32, optional_u8,
        physical_point_in_monitor, plugin_clipboard_history_snapshot, plugin_notification_body,
        plugin_search_providers_changed_payload, prepare_authorized_utools_drag_paths,
        prepare_directory_for_grant, prepare_utools_ffmpeg_run, publish_utools_ffmpeg_output,
        remember_utools_drag_grants, remember_utools_save_grant, renderer_display_path,
        resolve_issued_plugin_search_selection, revoke_plugin_launcher_context_transfer,
        set_plugin_session_secret, startup_launcher_hotkey_candidates, take_file_grant,
        take_plugin_batch_rename_preview, take_plugin_launcher_context_transfer,
        truncate_utf8_bytes, utools_db_storage_key, utools_dynamic_feature_command_id,
        utools_dynamic_feature_key, utools_notification_click_feature_code, validate_external_url,
        validate_local_search_selection, validate_plugin_clipboard_text,
        validate_system_icon_request, validate_utools_copy_file_paths,
        validate_utools_dialog_options, validate_utools_dynamic_feature,
        validate_utools_expend_height, validate_utools_input_text,
        validate_utools_redirect_request, validate_utools_shell_local_path,
        validate_utools_simulation_action, validate_utools_window_request_params,
        CaptureFocusLease, CursorColorApproval, DetachedPluginFrontendEventRequest,
        IssuedPluginSearchResults, LauncherFocusGate, LauncherHotkeyToggleGate,
        LauncherInvocationSource, LauncherVisibilityAction, LauncherVisibilitySnapshot,
        LauncherWorkArea, NativeDialogGuard, PendingPluginSearch, PendingUtoolsAiFunctionCall,
        PendingUtoolsMainPushSelection, PendingUtoolsToolCall, PluginBatchRenamePreview,
        PluginCursorColor, PluginHostRequest, PluginHostState, PluginLauncherContextFileRequest,
        PluginLauncherContextImageRequest, PluginLauncherContextRequest, PluginLogAdmission,
        RegisteredUtoolsTool, TemporaryPathOpenKind, TemporaryPathOpenStore, UtoolsMouseButton,
        UtoolsSimulationAction, LAUNCHER_CONTEXT_TTL, LAUNCHER_FALLBACK_HOTKEY,
        LAUNCHER_HOTKEY_TOGGLE_DEBOUNCE, LAUNCHER_INITIAL_BLUR_GRACE, LAUNCHER_PRIMARY_HOTKEY,
        MAX_CAPTURE_FOCUS_LEASES, MAX_PLUGIN_CLIPBOARD_HISTORY_ITEMS,
        MAX_PLUGIN_CLIPBOARD_TEXT_BYTES, MAX_PLUGIN_LOGS_PER_WINDOW,
        MAX_PLUGIN_NOTIFICATIONS_PER_WINDOW, MAX_PLUGIN_NOTIFICATION_BODY_CHARS,
        MAX_PLUGIN_SEARCH_PAYLOAD_BYTES, MAX_UTOOLS_COPY_IMAGE_SOURCE_BYTES, PLUGIN_LOG_WINDOW,
        PLUGIN_NOTIFICATION_WINDOW, PLUGIN_SEARCH_SELECTION_TTL, TEMPORARY_PATH_OPEN_TTL,
    };

    #[test]
    fn utools_text_matchers_apply_bounds_regex_flags_and_over_exclusions() {
        let regex = UtoolsTextMatcherInfo {
            matcher_type: "regex".to_owned(),
            label: "Color".to_owned(),
            pattern: Some("^#[0-9a-f]{6}$".to_owned()),
            flags: "i".to_owned(),
            min_length: Some(7),
            max_length: Some(7),
        };
        assert!(super::utools_text_matcher_accepts(&regex, "#0A84FF", 7).unwrap());
        assert!(!super::utools_text_matcher_accepts(&regex, "#fff", 4).unwrap());

        let over = UtoolsTextMatcherInfo {
            matcher_type: "over".to_owned(),
            label: "Search".to_owned(),
            pattern: Some("\\n".to_owned()),
            flags: String::new(),
            min_length: Some(1),
            max_length: Some(500),
        };
        assert!(super::utools_text_matcher_accepts(&over, "hello", 5).unwrap());
        assert!(!super::utools_text_matcher_accepts(&over, "hello\nworld", 11).unwrap());
    }

    #[test]
    fn utools_db_storage_keys_are_bounded_and_reversible() {
        for key in ["", "theme", "窗口.位置", "punctuation /?=#"] {
            let stored = utools_db_storage_key(key).expect("valid compatibility storage key");
            let encoded = stored
                .strip_prefix(super::UTOOLS_DB_STORAGE_PREFIX)
                .expect("compatibility namespace prefix");
            assert_eq!(decode_utools_db_storage_key(encoded).as_deref(), Some(key));
        }
        assert!(utools_db_storage_key(&"界".repeat(17)).is_err());
        assert_eq!(decode_utools_db_storage_key("0"), None);
        assert_eq!(decode_utools_db_storage_key("zz"), None);
        assert_eq!(decode_utools_db_storage_key("ff"), None);
    }

    #[test]
    fn utools_dynamic_features_are_bounded_normalized_and_stably_identified() {
        let feature = validate_utools_dynamic_feature(&json!({
            "code": " docs ",
            "explain": " Documentation ",
            "platform": ["win32", "darwin"],
            "mainHide": true,
            "cmds": [" Docs ", "Docs", "文档"]
        }))
        .expect("bounded direct commands should be accepted");
        assert_eq!(feature.code, "docs");
        assert_eq!(feature.explain.as_deref(), Some("Documentation"));
        assert_eq!(feature.cmds, vec!["Docs", "文档"]);
        assert_eq!(
            utools_dynamic_feature_command_id("docs"),
            "utools-dynamic-dc47fd6761f51d72"
        );
        assert_eq!(
            utools_dynamic_feature_key("docs"),
            "utools.feature.dc47fd6761f51d72"
        );

        for invalid in [
            json!({ "code": "", "cmds": ["Docs"] }),
            json!({ "code": "docs", "cmds": [] }),
            json!({ "code": "docs", "cmds": [{ "type": "files" }] }),
            json!({ "code": "docs", "cmds": ["Docs"], "platform": "android" }),
            json!({ "code": "docs", "cmds": ["Docs"], "shortcut": "Alt+D" }),
        ] {
            assert!(validate_utools_dynamic_feature(&invalid).is_err());
        }
    }

    #[test]
    fn utools_window_requests_accept_only_declared_boolean_options() {
        assert!(validate_utools_window_request_params(&json!({}), &[]).is_ok());
        assert!(validate_utools_window_request_params(
            &json!({ "isRestorePreWindow": true }),
            &["isRestorePreWindow"],
        )
        .is_ok());
        assert!(validate_utools_window_request_params(
            &json!({ "nativeHandle": "forbidden" }),
            &["isRestorePreWindow"],
        )
        .is_err());
        assert!(validate_utools_window_request_params(&json!([]), &[]).is_err());
    }

    #[test]
    fn utools_dialog_options_are_bounded_and_platform_explicit() {
        let open = validate_utools_dialog_options(
            "open",
            json!({
                "title": "选择 JSON",
                "defaultPath": r"C:\Users\Tester\Downloads",
                "filters": [{ "name": "JSON", "extensions": ["json"] }],
                "properties": ["openFile", "multiSelections"]
            }),
        )
        .expect("supported open dialog options");
        assert_eq!(open.filters.len(), 1);
        assert_eq!(open.properties, ["openFile", "multiSelections"]);

        let save = validate_utools_dialog_options(
            "save",
            json!({
                "defaultPath": r"C:\Users\Tester\Downloads\result.json",
                "properties": ["showOverwriteConfirmation"]
            }),
        )
        .expect("supported save dialog options");
        assert!(save.default_path.is_some());

        for value in [
            json!({ "defaultPath": "relative.json" }),
            json!({ "filters": [{ "name": "Bad", "extensions": ["../exe"] }] }),
            json!({ "properties": ["openFile", "openDirectory"] }),
            json!({ "properties": ["showHiddenFiles"] }),
            json!({ "securityScopedBookmarks": true }),
            json!({ "unknown": true }),
        ] {
            assert!(validate_utools_dialog_options("open", value).is_err());
        }
    }

    #[test]
    fn utools_redirect_accepts_only_bounded_typed_handoffs() {
        let (_, label, action) = validate_utools_redirect_request(&json!({
            "label": ["Translate", "翻译"],
            "action": { "type": "text", "payload": "hello" }
        }))
        .expect("bounded text redirect");
        assert_eq!(label, "翻译");
        assert_eq!(action.kind, "text");
        assert_eq!(action.payload, "hello");

        let file = std::env::temp_dir().join("ihub-utools-redirect.txt");
        let (_, _, action) = validate_utools_redirect_request(&json!({
            "label": "Open file",
            "action": {
                "type": "files",
                "payload": [file.to_string_lossy()]
            }
        }))
        .expect("lexically bounded file redirect");
        assert_eq!(action.kind, "files");

        for value in [
            json!({ "label": [], "action": { "type": "text", "payload": "x" } }),
            json!({ "label": "Open", "action": { "type": "unknown", "payload": "x" } }),
            json!({ "label": "Open", "action": { "type": "text", "payload": "x", "extra": true } }),
            json!({ "label": "Open", "action": { "type": "files", "payload": [] } }),
            json!({ "label": "Open", "action": { "type": "files", "payload": ["relative.txt"] } }),
        ] {
            assert!(validate_utools_redirect_request(&value).is_err());
        }
    }

    #[test]
    fn utools_text_input_accepts_only_bounded_explicit_values() {
        assert_eq!(
            validate_utools_input_text(&json!({ "value": "你好\nworld" }), 64, Some(16))
                .expect("bounded Unicode input should be accepted"),
            "你好\nworld"
        );
        for params in [
            json!([]),
            json!({}),
            json!({ "value": 42 }),
            json!({ "value": "ok", "delay": 0 }),
            json!({ "value": "bad\u{0}value" }),
        ] {
            assert!(validate_utools_input_text(&params, 64, Some(16)).is_err());
        }
        assert!(
            validate_utools_input_text(&json!({ "value": "界".repeat(22) }), 64, None).is_err()
        );
        assert!(
            validate_utools_input_text(&json!({ "value": "x".repeat(17) }), 64, Some(16)).is_err()
        );
    }

    #[test]
    fn utools_simulation_accepts_only_bounded_keys_and_active_display_points() {
        let bounds = [crate::utools_screen::ScreenRect {
            x: -1920,
            y: 0,
            width: 3840,
            height: 1080,
        }];
        let keyboard = validate_utools_simulation_action(
            "compatibility.utools.simulate.keyboardTap",
            &json!({ "key": "a", "modifiers": ["ctrl", "shift", "control"] }),
            &bounds,
            Some((50, 60)),
        )
        .expect("bounded keyboard chord");
        assert_eq!(
            keyboard,
            UtoolsSimulationAction::KeyboardTap {
                key: u16::from(b'A'),
                key_label: "a".to_owned(),
                modifiers: vec![0x11, 0x10],
                modifier_labels: vec!["Ctrl".to_owned(), "Shift".to_owned()],
            }
        );
        let question = validate_utools_simulation_action(
            "compatibility.utools.simulate.keyboardTap",
            &json!({ "key": "?", "modifiers": [] }),
            &bounds,
            None,
        )
        .expect("shifted punctuation");
        assert!(matches!(
            question,
            UtoolsSimulationAction::KeyboardTap {
                key: 0xbf,
                ref modifiers,
                ..
            } if modifiers == &[0x10]
        ));
        assert!(matches!(
            validate_utools_simulation_action(
                "compatibility.utools.simulate.keyboardTap",
                &json!({ "key": "numpad_0", "modifiers": [] }),
                &bounds,
                None,
            )
            .expect("robot-style numpad alias"),
            UtoolsSimulationAction::KeyboardTap { key: 0x60, .. }
        ));
        assert!(matches!(
            validate_utools_simulation_action(
                "compatibility.utools.simulate.keyboardTap",
                &json!({ "key": "audio_play", "modifiers": [] }),
                &bounds,
                None,
            )
            .expect("Windows media key alias"),
            UtoolsSimulationAction::KeyboardTap { key: 0xb3, .. }
        ));
        assert_eq!(
            validate_utools_simulation_action(
                "compatibility.utools.simulate.mouseMove",
                &json!({ "x": -120, "y": 400 }),
                &bounds,
                None,
            )
            .expect("negative multi-monitor coordinate"),
            UtoolsSimulationAction::MouseMove { x: -120, y: 400 }
        );
        assert_eq!(
            validate_utools_simulation_action(
                "compatibility.utools.simulate.mouseRightClick",
                &json!({}),
                &bounds,
                Some((80, 90)),
            )
            .expect("captured current pointer"),
            UtoolsSimulationAction::MouseClick {
                x: 80,
                y: 90,
                button: UtoolsMouseButton::Right,
                double: false,
            }
        );

        for (method, params) in [
            (
                "compatibility.utools.simulate.keyboardTap",
                json!({ "key": "unknown-key", "modifiers": [] }),
            ),
            (
                "compatibility.utools.simulate.keyboardTap",
                json!({ "key": "a", "modifiers": ["hyper"] }),
            ),
            (
                "compatibility.utools.simulate.keyboardTap",
                json!({ "key": "shift", "modifiers": ["shift"] }),
            ),
            (
                "compatibility.utools.simulate.mouseMove",
                json!({ "x": 10 }),
            ),
            (
                "compatibility.utools.simulate.mouseClick",
                json!({ "x": 5000, "y": 10 }),
            ),
            (
                "compatibility.utools.simulate.mouseDoubleClick",
                json!({ "x": 10.5, "y": 20 }),
            ),
        ] {
            assert!(
                validate_utools_simulation_action(method, &params, &bounds, Some((10, 20)))
                    .is_err(),
                "{method} should reject {params}"
            );
        }
    }

    #[test]
    fn utools_expend_height_accepts_only_bounded_integer_pixels() {
        for height in [100_u32, 300, 900] {
            assert_eq!(
                validate_utools_expend_height(&json!({ "height": height }))
                    .expect("bounded integer height should be accepted"),
                height
            );
        }

        for params in [
            json!({ "height": 99 }),
            json!({ "height": 901 }),
            json!({ "height": 300.5 }),
            json!({ "height": "300" }),
            json!({ "height": null }),
            json!({}),
            json!({ "height": 300, "nativeHandle": "forbidden" }),
        ] {
            assert!(validate_utools_expend_height(&params).is_err());
        }
    }

    #[test]
    fn external_urls_require_bounded_explicit_web_or_mail_schemes() {
        for value in [
            "https://example.com/path?q=1",
            "http://127.0.0.1:8080/",
            "HTTPS://example.com",
            "mailto:user@example.com?subject=iHub",
        ] {
            assert!(validate_external_url(value).is_ok(), "{value}");
        }
        for value in [
            "",
            "https://",
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "mailto:",
            "https://example.com/\nunsafe",
        ] {
            assert!(validate_external_url(value).is_err(), "{value}");
        }
        assert!(
            validate_external_url(&format!("https://example.com/{}", "x".repeat(2_048))).is_err()
        );
    }

    #[test]
    fn plugin_clipboard_text_is_bounded_by_utf8_bytes() {
        assert!(validate_plugin_clipboard_text("").is_ok());
        assert!(
            validate_plugin_clipboard_text(&"x".repeat(MAX_PLUGIN_CLIPBOARD_TEXT_BYTES)).is_ok()
        );
        assert!(
            validate_plugin_clipboard_text(&"界".repeat(MAX_PLUGIN_CLIPBOARD_TEXT_BYTES / 3))
                .is_ok()
        );
        assert!(validate_plugin_clipboard_text(
            &"界".repeat(MAX_PLUGIN_CLIPBOARD_TEXT_BYTES / 3 + 1)
        )
        .is_err());
    }

    #[test]
    fn system_icon_requests_are_bounded_unique_ids_not_renderer_paths() {
        let search_ids = (0..10)
            .map(|ordinal| format!("native-result-{ordinal}"))
            .collect::<Vec<_>>();
        let shortcut_ids = vec!["shortcut-a".to_owned(), "shortcut-b".to_owned()];
        assert!(validate_system_icon_request(&search_ids, &shortcut_ids).is_ok());

        let too_many = (0..13)
            .map(|ordinal| format!("native-result-{ordinal}"))
            .collect::<Vec<_>>();
        assert!(validate_system_icon_request(&too_many, &[]).is_err());
        assert!(
            validate_system_icon_request(&["same-id".to_owned()], &["same-id".to_owned()],)
                .is_err()
        );
        assert!(validate_system_icon_request(&["".to_owned()], &[]).is_err());
        assert!(validate_system_icon_request(
            &["x".repeat(super::MAX_SYSTEM_ICON_SEARCH_ID_BYTES + 1)],
            &[],
        )
        .is_err());
    }

    #[test]
    fn local_search_clipboard_selection_is_nonempty_bounded_and_unique() {
        assert!(validate_local_search_selection(&["one".to_owned(), "two".to_owned()]).is_ok());
        assert!(validate_local_search_selection(&[]).is_err());
        assert!(validate_local_search_selection(&["same".to_owned(), "same".to_owned()]).is_err());
        assert!(validate_local_search_selection(
            &(0..=super::MAX_LOCAL_SEARCH_SELECTION)
                .map(|ordinal| format!("result-{ordinal}"))
                .collect::<Vec<_>>(),
        )
        .is_err());
        assert!(validate_local_search_selection(&[
            "x".repeat(super::MAX_SYSTEM_ICON_SEARCH_ID_BYTES + 1),
        ])
        .is_err());
    }

    #[test]
    fn local_open_commands_launch_the_same_prepared_object_their_resolver_proved() {
        let complete_app_source = include_str!("app.rs");
        let test_module = complete_app_source
            .rfind("mod tests {")
            .expect("app source keeps a test module");
        let app_source = complete_app_source[..test_module]
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        for required in [
            "let prepared = temporary_path_opens.prepare_open(&open_id)?; prepared.launch()",
            "let prepared = resolve_current_search_result_open_target(&search_result_id, &index)?; prepared.launch()",
            "let prepared = shortcuts.resolve_open_target(&shortcut_id, &index)?; prepared.launch()",
            "let prepared = history.prepare_file_entry_open(&id, file_index)?; prepared.launch()",
            "let prepared = prepare_directory_for_grant(&state.host, &request.plugin_id, grant_id)?; prepared.launch()",
        ] {
            assert!(
                app_source.contains(required),
                "missing prepared-open source contract: {required}"
            );
        }
        for forbidden in [
            "open_local_path(&target.path",
            "open_local_path(Path::new(&directory)",
            "open_path_in_system(",
        ] {
            assert!(
                !app_source.contains(forbidden),
                "open command reintroduced a bare-path handoff: {forbidden}"
            );
        }

        let launcher_source = include_str!("launcher_shortcuts.rs")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(launcher_source.contains(
            "resolve_current_search_result_open_target( search_result_id: &str, index: &SearchIndex, ) -> Result<PreparedLocalOpen, String>"
        ));
        let clipboard_source = include_str!("clipboard_history.rs")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(clipboard_source.contains(
            "pub fn prepare_file_entry_open( &self, id: &str, file_index: usize, ) -> Result<PreparedLocalOpen, String>"
        ));
    }

    #[test]
    fn tray_actions_are_fixed_and_external_links_are_https_allowlisted() {
        assert_eq!(super::tray_action("show"), Some(super::TrayAction::Show));
        assert_eq!(
            super::tray_action("settings"),
            Some(super::TrayAction::Settings)
        );
        assert_eq!(
            super::tray_action("restart"),
            Some(super::TrayAction::Restart)
        );
        assert_eq!(super::tray_action("unknown"), None);

        assert!(super::is_fixed_tray_https_url(super::IHUB_HELP_URL));
        assert!(super::is_fixed_tray_https_url(super::IHUB_FEEDBACK_URL));
        assert!(!super::is_fixed_tray_https_url(
            "http://github.com/neko233-com/ihub"
        ));
        assert!(!super::is_fixed_tray_https_url(
            "https://example.com/phishing"
        ));
    }

    #[test]
    fn tray_setup_explicitly_uses_the_packaged_window_icon() {
        // The runtime cannot create a Windows App instance in this unit-test
        // harness, so preserve the source-level icon contract together with
        // cargo's type check.
        let source = include_str!("app.rs");
        let setup = source
            .split("fn setup_tray(app: &tauri::App)")
            .nth(1)
            .expect("tray setup function");
        assert!(setup.contains(".default_window_icon()"));
        assert!(setup.contains(".icon(tray_icon)"));
    }

    #[test]
    fn normalizes_known_host_targets_for_the_official_catalog() {
        assert_eq!(
            normalized_host_target("windows", "x86_64"),
            "windows-x86_64"
        );
        assert_eq!(
            normalized_host_target("windows", "aarch64"),
            "windows-aarch64"
        );
        assert_eq!(normalized_host_target("macos", "x86_64"), "darwin-x86_64");
        assert_eq!(normalized_host_target("macos", "aarch64"), "darwin-aarch64");
        assert_eq!(normalized_host_target("linux", "x86_64"), "linux-x86_64");
    }

    #[test]
    fn launcher_hotkey_status_reports_active_preferred_and_recovery_bindings() {
        let primary = LauncherHotkeyStatus::primary();
        assert_eq!(primary.registration, LauncherHotkeyRegistration::Primary);
        assert_eq!(
            primary.accelerator.as_deref(),
            Some(LAUNCHER_PRIMARY_HOTKEY)
        );
        assert_eq!(primary.preferred_accelerator, None);
        assert!(primary.tray_show_available);

        let configured = LauncherHotkeyStatus::configured("CmdOrCtrl+Shift+KeyK");
        assert_eq!(
            configured.registration,
            LauncherHotkeyRegistration::Configured
        );
        assert_eq!(
            configured.accelerator.as_deref(),
            Some("CmdOrCtrl+Shift+KeyK")
        );
        assert_eq!(
            configured.preferred_accelerator.as_deref(),
            Some("CmdOrCtrl+Shift+KeyK")
        );

        let fallback = LauncherHotkeyStatus::fallback_for(
            LAUNCHER_FALLBACK_HOTKEY,
            Some("CmdOrCtrl+Shift+KeyK".to_owned()),
        );
        assert_eq!(fallback.registration, LauncherHotkeyRegistration::Fallback);
        assert_eq!(
            fallback.accelerator.as_deref(),
            Some(LAUNCHER_FALLBACK_HOTKEY)
        );
        assert_eq!(
            fallback.preferred_accelerator.as_deref(),
            Some("CmdOrCtrl+Shift+KeyK")
        );
        assert!(fallback.tray_show_available);

        let unavailable = LauncherHotkeyStatus::unavailable();
        assert_eq!(
            unavailable.registration,
            LauncherHotkeyRegistration::Unavailable
        );
        assert_eq!(unavailable.accelerator, None);
        assert_eq!(
            serde_json::to_value(unavailable).expect("launcher hotkey status serializes"),
            json!({
                "registration": "unavailable",
                "trayShowAvailable": true,
            })
        );
    }

    #[test]
    fn launcher_hotkey_toggles_only_a_visible_focused_surface() {
        let hidden = LauncherVisibilitySnapshot {
            visible: false,
            focused: false,
        };
        let visible_unfocused = LauncherVisibilitySnapshot {
            visible: true,
            focused: false,
        };
        let visible_focused = LauncherVisibilitySnapshot {
            visible: true,
            focused: true,
        };

        assert_eq!(
            launcher_visibility_action(LauncherInvocationSource::Hotkey, hidden),
            LauncherVisibilityAction::RevealFresh
        );
        assert_eq!(
            launcher_visibility_action(LauncherInvocationSource::Hotkey, visible_unfocused),
            LauncherVisibilityAction::FocusExisting
        );
        assert_eq!(
            launcher_visibility_action(LauncherInvocationSource::Hotkey, visible_focused),
            LauncherVisibilityAction::Hide
        );
    }

    #[test]
    fn explicit_launcher_requests_never_toggle_a_visible_surface_closed() {
        for focused in [false, true] {
            assert_eq!(
                launcher_visibility_action(
                    LauncherInvocationSource::Explicit,
                    LauncherVisibilitySnapshot {
                        visible: true,
                        focused,
                    },
                ),
                LauncherVisibilityAction::FocusExisting
            );
        }
        assert_eq!(
            launcher_visibility_action(
                LauncherInvocationSource::Explicit,
                LauncherVisibilitySnapshot {
                    visible: false,
                    focused: false,
                },
            ),
            LauncherVisibilityAction::RevealFresh
        );
    }

    #[test]
    fn launcher_hotkey_auto_repeat_is_debounced_but_a_later_press_is_accepted() {
        let started_at = Instant::now();
        let mut gate = LauncherHotkeyToggleGate::default();
        assert!(gate.accept_press_at(started_at));
        assert!(!gate.accept_press_at(
            started_at + LAUNCHER_HOTKEY_TOGGLE_DEBOUNCE + Duration::from_secs(1)
        ));
        gate.release();
        assert!(!gate.accept_press_at(
            started_at + LAUNCHER_HOTKEY_TOGGLE_DEBOUNCE.saturating_sub(Duration::from_millis(1))
        ));
        gate.release();
        assert!(gate.accept_press_at(started_at + LAUNCHER_HOTKEY_TOGGLE_DEBOUNCE));
    }

    #[test]
    fn launcher_hotkey_startup_candidates_prefer_saved_then_deduplicate_recovery() {
        let custom = startup_launcher_hotkey_candidates(Some("CmdOrCtrl+Shift+KeyK"));
        assert_eq!(
            custom
                .iter()
                .map(|candidate| (candidate.accelerator.as_str(), candidate.preferred))
                .collect::<Vec<_>>(),
            vec![
                ("CmdOrCtrl+Shift+KeyK", true),
                (LAUNCHER_PRIMARY_HOTKEY, false),
                (LAUNCHER_FALLBACK_HOTKEY, false),
            ]
        );

        let saved_primary = startup_launcher_hotkey_candidates(Some(LAUNCHER_PRIMARY_HOTKEY));
        assert_eq!(
            saved_primary
                .iter()
                .map(|candidate| (candidate.accelerator.as_str(), candidate.preferred))
                .collect::<Vec<_>>(),
            vec![
                (LAUNCHER_PRIMARY_HOTKEY, true),
                (LAUNCHER_FALLBACK_HOTKEY, false),
            ]
        );

        let defaults = startup_launcher_hotkey_candidates(None);
        assert_eq!(
            defaults
                .iter()
                .map(|candidate| candidate.accelerator.as_str())
                .collect::<Vec<_>>(),
            vec![LAUNCHER_PRIMARY_HOTKEY, LAUNCHER_FALLBACK_HOTKEY]
        );
    }

    #[test]
    fn numbered_rename_options_require_bounded_unsigned_integers() {
        let params = json!({ "sequenceStart": 42, "sequencePadding": 3 });
        assert_eq!(
            optional_u32(&params, "sequenceStart").expect("u32 option"),
            Some(42)
        );
        assert_eq!(
            optional_u8(&params, "sequencePadding").expect("u8 option"),
            Some(3)
        );

        for params in [
            json!({ "sequenceStart": -1 }),
            json!({ "sequenceStart": 1.5 }),
            json!({ "sequenceStart": 4_294_967_296u64 }),
        ] {
            assert!(optional_u32(&params, "sequenceStart").is_err());
        }
        assert!(optional_u8(&json!({ "sequencePadding": 256 }), "sequencePadding").is_err());
    }

    #[test]
    fn launcher_monitor_hit_test_handles_negative_displays_and_edges() {
        let position = PhysicalPosition::new(-1_920, -120);
        let size = PhysicalSize::new(1_920, 1_080);

        assert!(physical_point_in_monitor(
            PhysicalPosition::new(-1_919.5, -119.5),
            position,
            size
        ));
        assert!(physical_point_in_monitor(
            PhysicalPosition::new(-1.0, 959.0),
            position,
            size
        ));
        assert!(!physical_point_in_monitor(
            PhysicalPosition::new(0.0, 200.0),
            position,
            size
        ));
        assert!(!physical_point_in_monitor(
            PhysicalPosition::new(-1_000.0, 960.0),
            position,
            size
        ));
        assert!(!physical_point_in_monitor(
            PhysicalPosition::new(f64::NAN, 0.0),
            position,
            size
        ));
        assert!(!physical_point_in_monitor(
            PhysicalPosition::new(-1_000.0, 0.0),
            position,
            PhysicalSize::new(0, 1_080)
        ));
    }

    #[test]
    fn launcher_reveal_layout_keeps_the_spotlight_design_size_when_it_fits() {
        let layout = LauncherWorkArea {
            position: PhysicalPosition::new(0, 0),
            size: PhysicalSize::new(1_920, 1_040),
        }
        .reveal_layout(1.0)
        .expect("a non-empty work area should have a launcher layout");

        assert_eq!(layout.size, PhysicalSize::new(800, 380));
        assert_eq!(layout.position, PhysicalPosition::new(560, 330));
    }

    #[test]
    fn launcher_reveal_layout_clamps_to_a_tiny_work_area_without_overflow() {
        let layout = LauncherWorkArea {
            position: PhysicalPosition::new(-1_366, 40),
            size: PhysicalSize::new(800, 480),
        }
        .reveal_layout(1.5)
        .expect("a non-empty work area should have a launcher layout");

        assert_eq!(layout.size, PhysicalSize::new(800, 480));
        assert_eq!(layout.position, PhysicalPosition::new(-1_366, 40));
        assert!(layout.position.x >= -1_366);
        assert!(layout.position.y >= 40);
        assert!(layout.position.x + layout.size.width as i32 <= -566);
        assert!(layout.position.y + layout.size.height as i32 <= 520);
    }

    #[test]
    fn launcher_reveal_layout_is_centered_fresh_after_a_dragged_visible_session() {
        let work_area = LauncherWorkArea {
            position: PhysicalPosition::new(-1_600, 24),
            size: PhysicalSize::new(1_920, 1_056),
        };
        // A long-press drag can put this visible session anywhere. It is
        // deliberately absent from the layout input, so reopening starts at
        // the monitor's center instead of retaining the temporary position.
        let dragged_visible_position = PhysicalPosition::new(-1_542, 700);
        let first_reveal = work_area
            .reveal_layout(1.0)
            .expect("a non-empty work area should have a launcher layout");
        let reopened = work_area
            .reveal_layout(1.0)
            .expect("a non-empty work area should have a launcher layout");

        assert_ne!(first_reveal.position, dragged_visible_position);
        assert_eq!(reopened, first_reveal);
        assert_eq!(reopened.position, PhysicalPosition::new(-1_040, 362));
    }

    #[test]
    fn launcher_reveal_layout_matches_reference_pixels_at_windows_150_percent_dpi() {
        let layout = LauncherWorkArea {
            position: PhysicalPosition::new(0, 0),
            size: PhysicalSize::new(3_840, 2_080),
        }
        .reveal_layout(1.5)
        .expect("a non-empty work area should have a launcher layout");

        assert_eq!(layout.size, PhysicalSize::new(1_200, 570));
        assert_eq!(layout.position, PhysicalPosition::new(1_320, 755));
    }

    #[test]
    fn super_panel_layout_anchors_below_or_above_and_never_leaves_the_work_area() {
        let work_area = LauncherWorkArea {
            position: PhysicalPosition::new(-1_920, 40),
            size: PhysicalSize::new(1_920, 1_040),
        };
        let below = work_area
            .super_panel_layout(1.0, PhysicalPosition::new(-960, 120))
            .expect("layout");
        assert_eq!(below.size, PhysicalSize::new(800, 380));
        assert_eq!(below.position, PhysicalPosition::new(-1_360, 132));

        let above = work_area
            .super_panel_layout(1.0, PhysicalPosition::new(-20, 1_040))
            .expect("layout");
        assert_eq!(above.position, PhysicalPosition::new(-800, 648));
        assert!(above.position.x >= -1_920);
        assert!(above.position.y >= 40);
        assert!(above.position.x + above.size.width as i32 <= 0);
        assert!(above.position.y + above.size.height as i32 <= 1_080);
    }

    #[test]
    fn super_panel_text_truncation_preserves_utf8_boundaries() {
        assert_eq!(truncate_utf8_bytes("hello".to_owned(), 5), "hello");
        assert_eq!(truncate_utf8_bytes("a猫b".to_owned(), 4), "a猫");
        assert_eq!(truncate_utf8_bytes("猫".to_owned(), 2), "");
    }

    #[test]
    fn launcher_initial_blur_cannot_hide_a_newly_revealed_surface() {
        let focus = LauncherFocusGate::default();
        focus.begin_reveal();
        assert!(
            !focus.consume_blur_after_focus(),
            "a Windows startup blur before set_focus must be ignored"
        );

        focus.note_focus();
        assert!(
            !focus.consume_blur_after_focus(),
            "the brief post-reveal Windows focus churn must remain visible"
        );

        focus.begin_reveal_at(
            Instant::now() - LAUNCHER_INITIAL_BLUR_GRACE - Duration::from_millis(1),
        );
        focus.note_focus();
        assert!(
            focus.consume_blur_after_focus(),
            "a later real blur should preserve normal Spotlight auto-hide"
        );
    }

    #[test]
    fn plugin_search_results_are_bounded_and_keep_json_payloads() {
        let results = normalize_plugin_search_results(
            &json!([
                {
                    "id": "first",
                    "title": "First result",
                    "subtitle": "Small, safe result",
                    "score": 42,
                    "payload": { "open": "details" }
                },
                { "id": "second", "title": "Second result" }
            ]),
            1,
        )
        .expect("well-formed results should be accepted");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "first");
        assert_eq!(results[0].payload, Some(json!({ "open": "details" })));

        let oversized_payload = "x".repeat(MAX_PLUGIN_SEARCH_PAYLOAD_BYTES + 1);
        let error = normalize_plugin_search_results(
            &json!([{ "id": "large", "title": "Large", "payload": oversized_payload }]),
            3,
        )
        .expect_err("oversized payloads must not reach the launcher");
        assert!(error.contains("payload exceeds"));
    }

    #[test]
    fn utools_main_push_selection_accepts_only_bounded_text_options() {
        let (code, action, option) = normalize_utools_main_push_selection(&json!({
            "kind": "utoolsMainPush",
            "action": { "code": "translate", "type": "text", "payload": "hello" },
            "option": { "text": "翻译 hello", "title": "离线翻译", "language": "zh" }
        }))
        .expect("a host-issued text option should remain selectable");
        assert_eq!(code, "translate");
        assert_eq!(action["from"], "main");
        assert_eq!(action["option"]["language"], "zh");
        assert_eq!(option["text"], "翻译 hello");

        for invalid in [
            json!({
                "kind": "utoolsMainPush",
                "action": { "code": "translate", "type": "img", "payload": "data:image/png;base64,AA==" },
                "option": { "text": "OCR" }
            }),
            json!({
                "kind": "utoolsMainPush",
                "action": { "code": "translate", "type": "text", "payload": "hello" },
                "option": { "title": "missing text" }
            }),
            json!({
                "kind": "forged",
                "action": { "code": "translate", "type": "text", "payload": "hello" },
                "option": { "text": "forged" }
            }),
        ] {
            assert!(normalize_utools_main_push_selection(&invalid).is_err());
        }
    }

    #[test]
    fn utools_main_push_interactions_bind_one_plugin_lease_and_clear_on_dispose() {
        let host = PluginHostState::default();
        let (sender, receiver) = mpsc::sync_channel(1);
        host.pending_utools_main_push_selections
            .lock()
            .expect("main-push lock")
            .insert(
                "main-push-selection-1".to_owned(),
                PendingUtoolsMainPushSelection {
                    plugin_id: "utools-owner".to_owned(),
                    lease_id: None,
                    completed: false,
                    response: sender,
                },
            );
        claim_utools_main_push_interaction(
            &host,
            "utools-owner",
            "runtime-lease-owner",
            "main-push-selection-1",
        )
        .expect("the receiving runtime should claim its interaction");
        assert!(claim_utools_main_push_interaction(
            &host,
            "utools-owner",
            "runtime-lease-other",
            "main-push-selection-1",
        )
        .is_err());
        assert!(claim_utools_main_push_interaction(
            &host,
            "utools-other",
            "runtime-lease-owner",
            "main-push-selection-1",
        )
        .is_err());

        clear_plugin_runtime_state(&host, "utools-owner");
        assert!(receiver.recv().expect("dispose response").is_err());
        assert!(claim_utools_main_push_interaction(
            &host,
            "utools-owner",
            "runtime-lease-owner",
            "main-push-selection-1",
        )
        .is_err());
    }

    #[test]
    fn utools_mcp_handlers_and_calls_are_lease_owned_and_clear_on_dispose() {
        let host = PluginHostState::default();
        host.utools_tools
            .write()
            .expect("tool registry lock")
            .insert(
                ("utools-owner".to_owned(), "say_hi".to_owned()),
                RegisteredUtoolsTool {
                    plugin_id: "utools-owner".to_owned(),
                    lease_id: "runtime-lease-owner".to_owned(),
                    window_label: "main".to_owned(),
                },
            );
        let (sender, receiver) = mpsc::sync_channel(1);
        host.pending_utools_tool_calls
            .lock()
            .expect("pending tool lock")
            .insert(
                "550e8400-e29b-41d4-a716-446655440000".to_owned(),
                PendingUtoolsToolCall {
                    plugin_id: "utools-owner".to_owned(),
                    name: "say_hi".to_owned(),
                    lease_id: "runtime-lease-owner".to_owned(),
                    response: sender,
                },
            );
        let (ai_sender, ai_receiver) = mpsc::sync_channel(1);
        host.pending_utools_ai_function_calls
            .lock()
            .expect("pending AI function lock")
            .insert(
                "650e8400-e29b-41d4-a716-446655440000".to_owned(),
                PendingUtoolsAiFunctionCall {
                    request_id: "750e8400-e29b-41d4-a716-446655440000".to_owned(),
                    plugin_id: "utools-owner".to_owned(),
                    lease_id: "runtime-lease-owner".to_owned(),
                    name: "getSystemInfo".to_owned(),
                    response: ai_sender,
                },
            );

        clear_plugin_runtime_state(&host, "utools-owner");
        assert!(host.utools_tools.read().expect("registry lock").is_empty());
        assert!(host
            .pending_utools_tool_calls
            .lock()
            .expect("pending lock")
            .is_empty());
        assert!(receiver.recv().expect("dispose response").is_err());
        assert!(host
            .pending_utools_ai_function_calls
            .lock()
            .expect("pending AI function lock")
            .is_empty());
        assert!(ai_receiver.recv().expect("AI dispose response").is_err());
    }

    #[test]
    fn plugin_level_provider_dispose_omits_provider_id_instead_of_sending_null() {
        assert_eq!(
            plugin_search_providers_changed_payload(
                "ihub-plugin-owner",
                Some("provider-owner"),
                true,
            ),
            json!({
                "pluginId": "ihub-plugin-owner",
                "providerId": "provider-owner",
                "registered": true,
            }),
        );
        assert_eq!(
            plugin_search_providers_changed_payload("ihub-plugin-owner", None, false),
            json!({
                "pluginId": "ihub-plugin-owner",
                "registered": false,
            }),
        );
    }

    #[test]
    fn detached_search_selection_uses_only_the_native_issued_snapshot() {
        let host = PluginHostState::default();
        let issued_at = Instant::now();
        host.issued_search_results
            .lock()
            .expect("issued search lock")
            .insert(
                "search-request-owner".to_owned(),
                IssuedPluginSearchResults {
                    plugin_id: "ihub-plugin-owner".to_owned(),
                    provider_id: "provider-owner".to_owned(),
                    results: vec![PluginSearchResult {
                        id: "result-owner".to_owned(),
                        title: "Owned result".to_owned(),
                        subtitle: None,
                        score: 42.0,
                        payload: Some(json!({ "reviewed": true, "path": "opaque-result" })),
                    }],
                    issued_at,
                },
            );

        assert!(resolve_issued_plugin_search_selection(
            &host,
            "ihub-plugin-other",
            "provider-owner",
            "search-request-owner",
            "result-owner",
            issued_at,
        )
        .is_err());
        assert!(resolve_issued_plugin_search_selection(
            &host,
            "ihub-plugin-owner",
            "provider-other",
            "search-request-owner",
            "result-owner",
            issued_at,
        )
        .is_err());
        assert!(resolve_issued_plugin_search_selection(
            &host,
            "ihub-plugin-owner",
            "provider-owner",
            "search-request-owner",
            "result-forged-by-renderer",
            issued_at,
        )
        .is_err());
        assert_eq!(
            resolve_issued_plugin_search_selection(
                &host,
                "ihub-plugin-owner",
                "provider-owner",
                "search-request-owner",
                "result-owner",
                issued_at,
            )
            .expect("the exact owner can resolve its issued result"),
            json!({ "reviewed": true, "path": "opaque-result" }),
        );
        // A second activation cannot replay the same native-issued result.
        assert!(resolve_issued_plugin_search_selection(
            &host,
            "ihub-plugin-owner",
            "provider-owner",
            "search-request-owner",
            "result-owner",
            issued_at,
        )
        .is_err());
    }

    #[test]
    fn detached_search_selection_rejects_and_removes_expired_snapshots() {
        let host = PluginHostState::default();
        let now = Instant::now();
        host.issued_search_results
            .lock()
            .expect("issued search lock")
            .insert(
                "expired-search-request".to_owned(),
                IssuedPluginSearchResults {
                    plugin_id: "ihub-plugin-owner".to_owned(),
                    provider_id: "provider-owner".to_owned(),
                    results: vec![PluginSearchResult {
                        id: "expired-result".to_owned(),
                        title: "Expired result".to_owned(),
                        subtitle: None,
                        score: 1.0,
                        payload: None,
                    }],
                    issued_at: now
                        .checked_sub(PLUGIN_SEARCH_SELECTION_TTL + Duration::from_millis(1))
                        .expect("test instant supports a short subtraction"),
                },
            );

        assert!(resolve_issued_plugin_search_selection(
            &host,
            "ihub-plugin-owner",
            "provider-owner",
            "expired-search-request",
            "expired-result",
            now,
        )
        .is_err());
        assert!(!host
            .issued_search_results
            .lock()
            .expect("issued search lock")
            .contains_key("expired-search-request"));
    }

    #[test]
    fn detached_frontend_event_request_rejects_renderer_routing_fields() {
        let command = serde_json::from_value::<DetachedPluginFrontendEventRequest>(json!({
            "kind": "command",
            "pluginId": "ihub-plugin-owner",
            "commandId": "open",
        }))
        .expect("the bounded command request should deserialize");
        assert!(matches!(
            command,
            DetachedPluginFrontendEventRequest::Command {
                plugin_id,
                command_id,
            } if plugin_id == "ihub-plugin-owner" && command_id == "open"
        ));

        for forged in [
            json!({
                "kind": "command",
                "pluginId": "ihub-plugin-owner",
                "commandId": "open",
                "windowLabel": "main",
            }),
            json!({
                "kind": "searchSelection",
                "pluginId": "ihub-plugin-owner",
                "providerId": "provider-owner",
                "requestId": "search-request-owner",
                "resultId": "result-owner",
                "payload": { "forged": true },
            }),
            json!({
                "kind": "keyword",
                "pluginId": "ihub-plugin-owner",
                "commandId": "open",
            }),
        ] {
            assert!(
                serde_json::from_value::<DetachedPluginFrontendEventRequest>(forged).is_err(),
                "renderer-controlled routing or payload fields must be rejected",
            );
        }
    }

    #[test]
    fn only_the_owning_plugin_can_complete_a_pending_search() {
        let host = PluginHostState::default();
        let (sender, receiver) = mpsc::sync_channel(1);
        host.pending_searches
            .lock()
            .expect("pending search lock")
            .insert(
                "request-1".to_owned(),
                PendingPluginSearch {
                    plugin_id: "ihub-plugin-owner".to_owned(),
                    provider_id: "provider".to_owned(),
                    max_results: 3,
                    response: sender,
                },
            );

        let stolen = complete_plugin_search(
            &host,
            "ihub-plugin-other",
            &json!({
                "requestId": "request-1",
                "ok": true,
                "result": [{ "id": "wrong", "title": "Wrong" }]
            }),
        );
        assert!(stolen.is_err());
        assert!(host
            .pending_searches
            .lock()
            .expect("pending search lock")
            .contains_key("request-1"));

        complete_plugin_search(
            &host,
            "ihub-plugin-owner",
            &json!({
                "requestId": "request-1",
                "ok": true,
                "result": [{ "id": "right", "title": "Right" }]
            }),
        )
        .expect("owning plugin should complete its request");
        let response = receiver
            .recv_timeout(Duration::from_millis(100))
            .expect("response should be forwarded")
            .expect("response should be valid");
        assert_eq!(response[0].id, "right");
    }

    #[test]
    fn filesystem_grants_are_plugin_scoped_and_revoked_with_lifecycle_state() {
        let directory = std::env::temp_dir().join(format!(
            "ihub-plugin-folder-grant-test-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&directory).expect("create plugin folder-grant fixture");
        let canonical_directory = directory
            .canonicalize()
            .expect("canonical plugin folder-grant fixture")
            .to_string_lossy()
            .into_owned();
        let host = PluginHostState::default();
        let owner = "ihub-plugin-owner";
        let grant_id = issue_filesystem_grant(&host, owner, canonical_directory.clone())
            .expect("issue owner folder grant");

        assert_eq!(
            directory_for_grant(&host, owner, &grant_id).expect("owner grant"),
            canonical_directory
        );
        let stolen = directory_for_grant(&host, "ihub-plugin-other", &grant_id)
            .expect_err("a different plugin cannot reuse a directory grant");
        assert!(stolen.contains("another plugin"));

        host.batch_rename_previews
            .lock()
            .expect("preview lock")
            .insert(
                "rename-preview-owner".to_owned(),
                PluginBatchRenamePreview {
                    plugin_id: owner.to_owned(),
                    grant_id: grant_id.clone(),
                    preview: crate::builtin_tools::BatchRenamePreview {
                        directory: canonical_directory,
                        items: Vec::new(),
                        can_apply: false,
                        errors: Vec::new(),
                    },
                    issued_at: Instant::now(),
                },
            );

        clear_plugin_runtime_state(&host, owner);
        assert!(directory_for_grant(&host, owner, &grant_id).is_err());
        assert!(host
            .batch_rename_previews
            .lock()
            .expect("preview lock")
            .is_empty());
        fs::remove_dir_all(directory).expect("cleanup plugin folder-grant fixture");
    }

    #[test]
    fn filesystem_grants_revalidate_the_exact_live_folder() {
        let directory = std::env::temp_dir().join(format!(
            "ihub-plugin-folder-revalidation-test-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&directory).expect("create folder revalidation fixture");
        let canonical_directory = directory
            .canonicalize()
            .expect("canonical folder revalidation fixture")
            .to_string_lossy()
            .into_owned();
        let host = PluginHostState::default();
        let grant_id = issue_filesystem_grant(&host, "ihub-plugin-owner", canonical_directory)
            .expect("issue revalidation folder grant");

        fs::remove_dir(&directory).expect("remove the originally selected folder");
        fs::write(&directory, "changed type").expect("replace folder with a regular file");
        assert!(directory_for_grant(&host, "ihub-plugin-owner", &grant_id)
            .expect_err("a replaced folder grant must fail closed")
            .contains("expected a folder"));

        fs::remove_file(directory).expect("cleanup folder revalidation fixture");
    }

    #[test]
    fn filesystem_grants_reject_same_kind_folder_replacement() {
        let root = std::env::temp_dir().join(format!(
            "ihub-plugin-folder-identity-test-{}",
            uuid::Uuid::new_v4()
        ));
        let directory = root.join("selected");
        let replacement = root.join("replacement");
        fs::create_dir_all(&directory).expect("create selected folder");
        fs::create_dir(&replacement).expect("create distinct replacement folder");
        let canonical_directory = directory
            .canonicalize()
            .expect("canonical selected folder")
            .to_string_lossy()
            .into_owned();
        let host = PluginHostState::default();
        let grant_id = issue_filesystem_grant(&host, "ihub-plugin-owner", canonical_directory)
            .expect("issue identity-bound folder grant");

        fs::remove_dir(&directory).expect("remove original folder");
        fs::rename(&replacement, &directory).expect("replace with a distinct folder object");
        let error = prepare_directory_for_grant(&host, "ihub-plugin-owner", &grant_id)
            .expect_err("same-kind folder replacement must fail closed");
        assert!(error.contains("replaced after authorization"), "{error}");

        fs::remove_dir_all(root).expect("cleanup folder identity fixture");
    }

    #[test]
    fn file_grants_are_owner_scoped_one_shot_and_revoked_with_lifecycle_state() {
        let directory =
            std::env::temp_dir().join(format!("ihub-file-grant-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create file-grant fixture directory");
        let file = directory.join("scan.png");
        fs::write(&file, b"fixture").expect("write file-grant fixture");
        let selected = canonical_selected_file(file.clone()).expect("regular fixture file");
        let expected_path = file.canonicalize().expect("canonical fixture path");

        let host = PluginHostState::default();
        let owner = "ihub-plugin-owner";
        let command_grant_id = issue_file_grant(&host, owner, vec![selected.clone()]);
        let (command_id, worker_input) = native_plugin_command_input(
            &host,
            owner,
            &json!({
                "commandId": "recognize-image",
                "fileGrantId": command_grant_id,
                "input": { "language": "eng" },
            }),
        )
        .expect("the owner can convert a file grant into native worker input");
        assert_eq!(command_id, "recognize-image");
        assert_eq!(worker_input["input"], json!({ "language": "eng" }));
        assert_eq!(
            worker_input["files"][0]["path"],
            json!(expected_path.to_string_lossy().into_owned())
        );
        assert!(take_file_grant(&host, owner, &command_grant_id).is_err());

        let grant_id = issue_file_grant(&host, owner, vec![selected.clone()]);
        let stolen = take_file_grant(&host, "ihub-plugin-other", &grant_id)
            .expect_err("another plugin cannot consume a file grant");
        assert!(stolen.contains("another plugin"));
        assert!(host
            .file_grants
            .lock()
            .expect("file grant lock")
            .contains_key(&grant_id));

        let files = take_file_grant(&host, owner, &grant_id)
            .expect("the owner can consume its selected file once");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, expected_path);
        assert_eq!(files[0].name, "scan.png");
        assert_eq!(files[0].size, 7);
        assert!(take_file_grant(&host, owner, &grant_id).is_err());

        let revoked_id = issue_file_grant(&host, owner, vec![selected]);
        clear_plugin_runtime_state(&host, owner);
        assert!(take_file_grant(&host, owner, &revoked_id).is_err());

        fs::remove_dir_all(directory).expect("cleanup file-grant fixture");
    }

    #[test]
    fn launcher_context_handoff_is_canonicalized_owner_scoped_and_one_shot() {
        let directory = std::env::temp_dir().join(format!(
            "ihub-launcher-context-test-{}",
            uuid::Uuid::new_v4()
        ));
        let plugin_root = directory.join("plugins");
        let plugin_id = "ihub-plugin-context-owner";
        let command_id = "open-context";
        let package = plugin_root.join(plugin_id);
        let selected_folder = directory.join("selected");
        let selected_file = selected_folder.join("draft.txt");
        fs::create_dir_all(package.join("dist")).expect("create plugin dist");
        fs::create_dir_all(&selected_folder).expect("create selected folder");
        fs::write(&selected_file, "iHub context").expect("create selected file");
        fs::write(package.join("dist/index.html"), "<main>context</main>")
            .expect("create plugin frontend");
        fs::write(
            package.join("plugin.json"),
            format!(
                r#"{{
  "id": "{plugin_id}",
  "name": "Context owner",
  "version": "0.1.0",
  "entry": {{ "frontend": "dist/index.html" }},
  "contributes": {{
    "commands": [{{ "id": "{command_id}", "title": "Open context", "execution": "frontend" }}]
  }},
  "permissions": {{
    "launcherContext": {{ "text": true, "files": true, "image": true }}
  }}
}}"#
            ),
        )
        .expect("write plugin manifest");
        let manager = crate::plugins::PluginManager::for_test_root(plugin_root);

        let payload = build_plugin_launcher_context_payload(PluginLauncherContextRequest {
            text: Some("explicit text only".to_owned()),
            files: vec![
                PluginLauncherContextFileRequest {
                    path: selected_file.to_string_lossy().into_owned(),
                },
                // The same canonical item must not create a second metadata
                // record or a second opaque handle.
                PluginLauncherContextFileRequest {
                    path: selected_file.to_string_lossy().into_owned(),
                },
            ],
            image: Some(PluginLauncherContextImageRequest {
                name: "pasted.png".to_owned(),
                mime_type: "image/png".to_owned(),
                width: 2,
                height: 2,
            }),
        })
        .expect("trusted launcher input should be normalized");
        assert_eq!(payload.files.len(), 1);
        assert_eq!(payload.files[0].name, "draft.txt");
        assert_eq!(payload.files[0].kind, "file");
        assert_eq!(payload.files[0].size, Some(12));
        let encoded = serde_json::to_string(&payload).expect("context serializes");
        assert!(
            !encoded.contains(
                &*selected_file
                    .canonicalize()
                    .expect("canonical selected file")
                    .to_string_lossy()
            ),
            "a launcher context must never serialize its canonical path into the iframe payload"
        );

        let host = PluginHostState::default();
        let frontend_lease_id = "lease-context";
        let issue = issue_plugin_launcher_context_transfer(
            &host,
            plugin_id,
            command_id,
            frontend_lease_id,
            payload.clone(),
        );
        assert!(issue.expires_in_ms > 0);
        assert!(issue.expires_in_ms <= LAUNCHER_CONTEXT_TTL.as_millis() as u64);
        assert!(take_plugin_launcher_context_transfer(
            &host,
            &manager,
            plugin_id,
            frontend_lease_id,
            &issue.context_id,
        )
        .expect_err("an undispatched context must not be readable")
        .contains("not been attached"));
        assert!(host
            .launcher_contexts
            .lock()
            .expect("context lock")
            .contains_key(&issue.context_id));

        assert!(attach_plugin_launcher_context_transfer(
            &host,
            "ihub-plugin-context-other",
            command_id,
            frontend_lease_id,
            "req-other",
            &issue.context_id,
        )
        .is_err());
        assert!(attach_plugin_launcher_context_transfer(
            &host,
            plugin_id,
            "other-command",
            frontend_lease_id,
            "req-other-command",
            &issue.context_id,
        )
        .is_err());
        assert!(attach_plugin_launcher_context_transfer(
            &host,
            plugin_id,
            command_id,
            "lease-replaced",
            "req-lease-replaced",
            &issue.context_id,
        )
        .expect_err("a context cannot move to a newer same-plugin iframe")
        .contains("previous plugin surface"));
        let invocation = attach_plugin_launcher_context_transfer(
            &host,
            plugin_id,
            command_id,
            frontend_lease_id,
            "req-context",
            &issue.context_id,
        )
        .expect("the matching command can receive the opaque context ID");
        assert_eq!(invocation.context_id, issue.context_id);
        assert!(invocation.expires_in_ms > 0);
        assert!(attach_plugin_launcher_context_transfer(
            &host,
            plugin_id,
            command_id,
            frontend_lease_id,
            "req-replay",
            &issue.context_id,
        )
        .expect_err("a context ID may be dispatched only once")
        .contains("already dispatched"));
        assert!(take_plugin_launcher_context_transfer(
            &host,
            &manager,
            "ihub-plugin-context-other",
            frontend_lease_id,
            &issue.context_id,
        )
        .expect_err("another plugin must not consume the context")
        .contains("another plugin"));

        assert!(take_plugin_launcher_context_transfer(
            &host,
            &manager,
            plugin_id,
            "lease-replaced",
            &issue.context_id,
        )
        .expect_err("a replacement same-plugin iframe must not consume the old surface context")
        .contains("previous plugin surface"));

        let consumed = take_plugin_launcher_context_transfer(
            &host,
            &manager,
            plugin_id,
            frontend_lease_id,
            &issue.context_id,
        )
        .expect("the declared, owning plugin can consume once after dispatch");
        assert_eq!(consumed, payload);
        assert!(take_plugin_launcher_context_transfer(
            &host,
            &manager,
            plugin_id,
            frontend_lease_id,
            &issue.context_id,
        )
        .is_err());

        let expiring = issue_plugin_launcher_context_transfer(
            &host,
            plugin_id,
            command_id,
            frontend_lease_id,
            payload,
        );
        host.launcher_contexts
            .lock()
            .expect("context lock")
            .get_mut(&expiring.context_id)
            .expect("new context")
            .expires_at = Instant::now() - Duration::from_millis(1);
        assert!(attach_plugin_launcher_context_transfer(
            &host,
            plugin_id,
            command_id,
            frontend_lease_id,
            "req-expired",
            &expiring.context_id,
        )
        .expect_err("expired context IDs must be discarded before dispatch")
        .contains("expired"));
        assert!(!host
            .launcher_contexts
            .lock()
            .expect("context lock")
            .contains_key(&expiring.context_id));

        let cleanup = issue_plugin_launcher_context_transfer(
            &host,
            plugin_id,
            command_id,
            frontend_lease_id,
            build_plugin_launcher_context_payload(PluginLauncherContextRequest {
                text: Some("cleanup after failed parent dispatch".to_owned()),
                files: Vec::new(),
                image: None,
            })
            .expect("small cleanup context"),
        );
        assert!(
            revoke_plugin_launcher_context_transfer(&host, plugin_id, &cleanup.context_id)
                .expect("the owning parent can clear its undispatched context")
        );
        assert!(
            !revoke_plugin_launcher_context_transfer(&host, plugin_id, &cleanup.context_id)
                .expect("clearing the same context twice is an idempotent no-op")
        );
        let wrong_owner = issue_plugin_launcher_context_transfer(
            &host,
            plugin_id,
            command_id,
            frontend_lease_id,
            build_plugin_launcher_context_payload(PluginLauncherContextRequest {
                text: Some("owner isolation".to_owned()),
                files: Vec::new(),
                image: None,
            })
            .expect("small owner-isolation context"),
        );
        assert!(revoke_plugin_launcher_context_transfer(
            &host,
            "ihub-plugin-context-other",
            &wrong_owner.context_id
        )
        .expect_err("another plugin must not clear an owner context")
        .contains("another plugin"));
        assert!(
            revoke_plugin_launcher_context_transfer(&host, plugin_id, &wrong_owner.context_id)
                .expect("the owner can clean the isolated context")
        );

        let attached_then_closed = issue_plugin_launcher_context_transfer(
            &host,
            plugin_id,
            command_id,
            frontend_lease_id,
            build_plugin_launcher_context_payload(PluginLauncherContextRequest {
                text: Some("close after command emit".to_owned()),
                files: Vec::new(),
                image: None,
            })
            .expect("small attached cleanup context"),
        );
        attach_plugin_launcher_context_transfer(
            &host,
            plugin_id,
            command_id,
            frontend_lease_id,
            "req-close-after-emit",
            &attached_then_closed.context_id,
        )
        .expect("the declared command can receive the close-cleanup context");
        clear_plugin_runtime_state(&host, plugin_id);
        assert!(take_plugin_launcher_context_transfer(
            &host,
            &manager,
            plugin_id,
            frontend_lease_id,
            &attached_then_closed.context_id,
        )
        .expect_err("closing an iframe must revoke an attached context before consume")
        .contains("expired or was already consumed"));

        let revoked = issue_plugin_launcher_context_transfer(
            &host,
            plugin_id,
            command_id,
            frontend_lease_id,
            build_plugin_launcher_context_payload(PluginLauncherContextRequest {
                text: Some("revoke me".to_owned()),
                files: Vec::new(),
                image: None,
            })
            .expect("small text context"),
        );
        clear_plugin_runtime_state(&host, plugin_id);
        assert!(attach_plugin_launcher_context_transfer(
            &host,
            plugin_id,
            command_id,
            frontend_lease_id,
            "req-revoked",
            &revoked.context_id,
        )
        .is_err());

        fs::remove_dir_all(directory).expect("cleanup launcher-context fixture");
    }

    #[test]
    fn developer_project_creation_is_bound_to_its_owner_grant_and_safe_plugin_id() {
        let parent = std::env::temp_dir().join(format!(
            "ihub-developer-project-grant-test-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&parent).expect("temporary parent should be created");
        let parent_directory = parent
            .canonicalize()
            .expect("canonical project parent")
            .to_string_lossy()
            .into_owned();
        let host = PluginHostState::default();
        let owner = "ihub-plugin-developer-tools";
        let grant_id = issue_filesystem_grant(&host, owner, parent_directory)
            .expect("issue developer folder grant");

        let created =
            create_plugin_project_for_grant(&host, owner, &grant_id, "ihub-plugin-grant-demo")
                .expect("the grant owner can create a fresh project below the selected folder");
        assert_eq!(created.plugin_id, "ihub-plugin-grant-demo");
        assert!(parent.join("ihub-plugin-grant-demo/plugin.json").is_file());

        let stolen = create_plugin_project_for_grant(
            &host,
            "ihub-plugin-other",
            &grant_id,
            "ihub-plugin-stolen",
        )
        .expect_err("another plugin must not reuse the owner grant");
        assert!(stolen.contains("another plugin"));
        assert!(!parent.join("ihub-plugin-stolen").exists());

        let invalid = create_plugin_project_for_grant(&host, owner, &grant_id, "Invalid_ID")
            .expect_err("invalid plugin IDs must be rejected before any project is created");
        assert!(invalid.contains("lowercase kebab-case"));
        let escaping = create_plugin_project_for_grant(&host, owner, &grant_id, "../outside")
            .expect_err("a path-like plugin ID must never escape the selected folder");
        assert!(escaping.contains("lowercase kebab-case"));
        assert!(!parent.join("outside").exists());

        fs::remove_dir_all(parent).expect("cleanup developer project fixture");
    }

    #[test]
    fn rename_preview_owner_mismatch_does_not_consume_the_owner_token() {
        let directory = std::env::temp_dir().join(format!(
            "ihub-rename-preview-owner-test-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&directory).expect("create rename preview directory");
        let canonical_directory = directory
            .canonicalize()
            .expect("canonical rename preview directory")
            .to_string_lossy()
            .into_owned();
        let host = PluginHostState::default();
        let owner = "ihub-plugin-owner";
        let grant_id = issue_filesystem_grant(&host, owner, canonical_directory.clone())
            .expect("issue rename preview folder grant");
        let preview_id = "rename-preview-owner";
        host.batch_rename_previews
            .lock()
            .expect("preview lock")
            .insert(
                preview_id.to_owned(),
                PluginBatchRenamePreview {
                    plugin_id: owner.to_owned(),
                    grant_id: grant_id.clone(),
                    preview: crate::builtin_tools::BatchRenamePreview {
                        directory: canonical_directory,
                        items: Vec::new(),
                        can_apply: true,
                        errors: Vec::new(),
                    },
                    issued_at: Instant::now(),
                },
            );

        assert!(take_plugin_batch_rename_preview(
            &host,
            "ihub-plugin-other",
            &grant_id,
            preview_id,
        )
        .is_err());
        assert!(host
            .batch_rename_previews
            .lock()
            .expect("preview lock")
            .contains_key(preview_id));

        let preview = take_plugin_batch_rename_preview(&host, owner, &grant_id, preview_id)
            .expect("owner can consume its own preview");
        assert!(preview.can_apply);
        assert!(host
            .batch_rename_previews
            .lock()
            .expect("preview lock")
            .is_empty());
        fs::remove_dir_all(directory).expect("cleanup rename preview directory");
    }

    #[test]
    fn session_secret_settings_are_plugin_scoped_and_reset_on_source_transition() {
        let host = PluginHostState::default();
        let owner = "ihub-plugin-owner";
        let other = "ihub-plugin-other";
        set_plugin_session_secret(&host, owner, "apiKey", json!("owner-secret"))
            .expect("owner secret should fit the shared storage bounds");
        set_plugin_session_secret(&host, other, "apiKey", json!("other-secret"))
            .expect("other secret should fit the shared storage bounds");

        assert_eq!(
            get_plugin_session_secret(&host, owner, "apiKey"),
            Some(json!("owner-secret"))
        );
        assert_eq!(
            get_plugin_session_secret(&host, other, "apiKey"),
            Some(json!("other-secret"))
        );

        // Closing one iframe only resets registrations and grants; a reopened
        // iframe may keep its process-local credential during the app run.
        clear_plugin_runtime_state(&host, owner);
        assert_eq!(
            get_plugin_session_secret(&host, owner, "apiKey"),
            Some(json!("owner-secret"))
        );

        clear_plugin_session_secrets(&host, owner);
        assert_eq!(get_plugin_session_secret(&host, owner, "apiKey"), None);
        assert_eq!(
            get_plugin_session_secret(&host, other, "apiKey"),
            Some(json!("other-secret"))
        );
    }

    #[test]
    fn pasted_clipboard_file_metadata_is_canonicalized_and_bounded_to_filesystem_objects() {
        let directory =
            std::env::temp_dir().join(format!("ihub-clipboard-file-test-{}", uuid::Uuid::new_v4()));
        let file = directory.join("example.txt");
        fs::create_dir_all(&directory).expect("create clipboard fixture directory");
        fs::write(&file, "iHub").expect("create clipboard fixture file");
        let open_store = TemporaryPathOpenStore::default();

        let entries = clipboard_files_from_paths(
            &open_store,
            vec![
                file.clone(),
                directory.clone(),
                directory.join("already-gone.txt"),
            ],
        );
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "example.txt");
        assert_eq!(entries[0].kind, "file");
        assert_eq!(entries[1].kind, "folder");
        assert!(entries[0].path.ends_with("example.txt"));
        #[cfg(windows)]
        assert!(!entries[0].path.starts_with(r"\\?\"));
        assert!(!entries[0].open_id.contains("example.txt"));
        assert_eq!(
            open_store
                .resolve(&entries[0].open_id)
                .expect("a fresh file open ID should resolve"),
            crate::system_open::prepare_local_open(
                &file,
                Some(crate::system_open::LocalOpenKind::File),
            )
            .expect("prepare expected file")
            .path()
        );
        assert_eq!(
            open_store
                .resolve(&entries[1].open_id)
                .expect("a fresh folder open ID should resolve"),
            crate::system_open::prepare_local_open(
                &directory,
                Some(crate::system_open::LocalOpenKind::Folder),
            )
            .expect("prepare expected folder")
            .path()
        );

        fs::remove_dir_all(directory).expect("cleanup clipboard fixture directory");
    }

    #[test]
    fn temporary_path_open_ids_reject_unknown_expired_and_type_changed_targets() {
        let directory =
            std::env::temp_dir().join(format!("ihub-open-id-test-{}", uuid::Uuid::new_v4()));
        let target = directory.join("selected.txt");
        fs::create_dir_all(&directory).expect("create open-ID fixture directory");
        fs::write(&target, "iHub").expect("create open-ID fixture file");
        let open_store = TemporaryPathOpenStore::default();

        let issued = open_store
            .issue(&target)
            .expect("a regular local file should receive an open ID");
        assert_eq!(issued.kind, TemporaryPathOpenKind::File);
        assert!(!issued.open_id.contains("selected.txt"));
        assert!(open_store
            .resolve("open-00000000-0000-0000-0000-000000000000")
            .is_err());

        let expired_at = Instant::now()
            .checked_sub(TEMPORARY_PATH_OPEN_TTL + Duration::from_secs(1))
            .expect("the test clock supports a short lookback");
        let expired = open_store
            .issue_at(&target, expired_at)
            .expect("the fixture can issue an already-aged record");
        assert!(open_store
            .resolve_at(&expired.open_id, Instant::now())
            .expect_err("an expired ID must not resolve")
            .contains("unknown or expired"));

        fs::remove_file(&target).expect("replace the file with a directory");
        fs::create_dir(&target).expect("create replacement directory at the same path");
        assert!(open_store
            .resolve(&issued.open_id)
            .expect_err("a changed filesystem kind must invalidate the grant")
            .contains("changed type"));

        fs::remove_dir_all(directory).expect("cleanup open-ID fixture directory");
    }

    #[test]
    fn temporary_path_open_ids_reject_same_kind_object_replacement() {
        let directory = std::env::temp_dir().join(format!(
            "ihub-open-id-identity-test-{}",
            uuid::Uuid::new_v4()
        ));
        let target = directory.join("selected.txt");
        let replacement = directory.join("replacement.txt");
        fs::create_dir_all(&directory).expect("create identity fixture directory");
        fs::write(&target, "original").expect("create original fixture file");
        fs::write(&replacement, "replacement").expect("create distinct replacement object");
        let open_store = TemporaryPathOpenStore::default();
        let issued = open_store
            .issue(&target)
            .expect("the original object should receive an open ID");

        fs::remove_file(&target).expect("remove the original object");
        fs::rename(&replacement, &target).expect("move a distinct file into the authorized name");
        let error = open_store
            .resolve(&issued.open_id)
            .expect_err("a same-kind replacement must invalidate the grant");
        assert!(error.contains("replaced after authorization"), "{error}");

        fs::remove_dir_all(directory).expect("cleanup identity fixture directory");
    }

    #[cfg(windows)]
    #[test]
    fn temporary_path_open_grant_keeps_its_verified_object_guarded() {
        let directory =
            std::env::temp_dir().join(format!("ihub-open-id-guard-test-{}", uuid::Uuid::new_v4()));
        let target = directory.join("selected.txt");
        fs::create_dir_all(&directory).expect("create guard fixture directory");
        fs::write(&target, "original").expect("create guarded fixture file");
        let open_store = TemporaryPathOpenStore::default();
        let issued = open_store.issue(&target).expect("issue guarded open ID");

        let prepared = open_store
            .prepare_open(&issued.open_id)
            .expect("resolve and retain the original object");
        assert!(
            fs::OpenOptions::new().write(true).open(&target).is_err(),
            "the grant resolver must retain the final read/share guard"
        );
        drop(prepared);
        fs::OpenOptions::new()
            .write(true)
            .open(&target)
            .expect("dropping the prepared open releases the guard");

        fs::remove_dir_all(directory).expect("cleanup guard fixture directory");
    }

    #[test]
    fn index_root_updates_require_exact_current_or_native_folder_grants() {
        let fixture = std::env::temp_dir().join(format!(
            "ihub-index-root-grant-test-{}",
            uuid::Uuid::new_v4()
        ));
        let current = fixture.join("current");
        let selected = fixture.join("selected");
        let unselected = fixture.join("unselected");
        fs::create_dir_all(&current).expect("create current root fixture");
        fs::create_dir(&selected).expect("create selected root fixture");
        fs::create_dir(&unselected).expect("create unselected root fixture");
        let selected_file = fixture.join("selected.txt");
        fs::write(&selected_file, "iHub").expect("create selected file fixture");

        let current_root_path = current.canonicalize().expect("canonical current root");
        let current_root = renderer_display_path(&current_root_path);
        let unselected_root = renderer_display_path(
            &unselected
                .canonicalize()
                .expect("canonical unselected root"),
        );
        let open_store = TemporaryPathOpenStore::default();
        let selected_grant = open_store
            .issue(&selected)
            .expect("issue selected folder grant");
        let selected_root = renderer_display_path(&selected_grant.canonical_path);
        let file_grant = open_store
            .issue(&selected_file)
            .expect("issue selected file grant");

        assert!(authorize_index_root_update(
            std::slice::from_ref(&current_root_path),
            &[],
            &[],
            &open_store,
        )
        .is_ok());
        assert!(authorize_index_root_update(
            std::slice::from_ref(&current_root_path),
            std::slice::from_ref(&current_root),
            &[],
            &open_store,
        )
        .is_ok());
        assert!(
            authorize_index_root_update(
                &[
                    current_root_path.clone(),
                    fixture.join("missing-current-root")
                ],
                std::slice::from_ref(&current_root),
                &[],
                &open_store,
            )
            .is_ok(),
            "removing an unavailable old root must not require preparing it",
        );
        assert!(authorize_index_root_update(
            std::slice::from_ref(&current_root_path),
            &[current_root.clone(), selected_root.clone()],
            std::slice::from_ref(&selected_grant.open_id),
            &open_store,
        )
        .is_ok());

        let raw_error = authorize_index_root_update(
            std::slice::from_ref(&current_root_path),
            std::slice::from_ref(&unselected_root),
            &[],
            &open_store,
        )
        .expect_err("an arbitrary renderer path must not become an index root");
        assert!(raw_error.contains("current system-folder selection"));

        assert!(authorize_index_root_update(
            std::slice::from_ref(&current_root_path),
            std::slice::from_ref(&unselected_root),
            std::slice::from_ref(&selected_grant.open_id),
            &open_store,
        )
        .is_err());
        assert!(authorize_index_root_update(
            std::slice::from_ref(&current_root_path),
            std::slice::from_ref(&selected_root),
            std::slice::from_ref(&file_grant.open_id),
            &open_store,
        )
        .expect_err("a file open ID must not authorize an index folder")
        .contains("not for a folder"));

        let expired_at = Instant::now()
            .checked_sub(TEMPORARY_PATH_OPEN_TTL + Duration::from_secs(1))
            .expect("the test clock supports a short lookback");
        let expired = open_store
            .issue_at(&selected, expired_at)
            .expect("issue an aged folder grant");
        assert!(authorize_index_root_update(
            std::slice::from_ref(&current_root_path),
            std::slice::from_ref(&selected_root),
            std::slice::from_ref(&expired.open_id),
            &open_store,
        )
        .expect_err("an expired native selection must not authorize a new root")
        .contains("unknown or expired"));

        fs::remove_dir_all(fixture).expect("cleanup index-root grant fixture");
    }

    #[test]
    fn temporary_path_open_store_is_bounded_and_evicts_the_oldest_grant() {
        let directory =
            std::env::temp_dir().join(format!("ihub-open-bound-test-{}", uuid::Uuid::new_v4()));
        let target = directory.join("selected.txt");
        fs::create_dir_all(&directory).expect("create bounded open-ID fixture directory");
        fs::write(&target, "iHub").expect("create bounded open-ID fixture file");
        let open_store = TemporaryPathOpenStore::default();
        let base = Instant::now();
        let mut issued_ids = Vec::new();
        for ordinal in 0..=super::MAX_TEMPORARY_PATH_OPEN_GRANTS {
            issued_ids.push(
                open_store
                    .issue_at(&target, base + Duration::from_nanos(ordinal as u64))
                    .expect("issue a bounded open authorization")
                    .open_id,
            );
        }

        assert_eq!(
            open_store
                .grants
                .lock()
                .expect("inspect bounded open grant store")
                .len(),
            super::MAX_TEMPORARY_PATH_OPEN_GRANTS
        );
        assert!(open_store.resolve(&issued_ids[0]).is_err());
        assert_eq!(
            open_store
                .resolve(issued_ids.last().expect("latest issued ID"))
                .expect("the latest bounded grant remains valid"),
            crate::system_open::prepare_local_open(
                &target,
                Some(crate::system_open::LocalOpenKind::File),
            )
            .expect("prepare bounded fixture")
            .path()
        );

        fs::remove_dir_all(directory).expect("cleanup bounded open-ID fixture");
    }

    #[test]
    fn a_created_plugin_project_receives_a_folder_open_id() {
        let parent = std::env::temp_dir().join(format!(
            "ihub-project-open-id-test-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&parent).expect("create project parent fixture");
        let open_store = TemporaryPathOpenStore::default();
        let parent_grant = open_store
            .issue(&parent)
            .expect("the selected project parent receives a folder grant");
        let created = create_plugin_project_with_open_grant(
            &open_store,
            &parent_grant.open_id,
            "ihub-plugin-open-id-test",
        )
        .expect("create a plugin project with a native open authorization");
        let open_id = created
            .open_id
            .as_deref()
            .expect("the first-party project result must carry an open ID");
        assert_eq!(
            open_store
                .resolve(open_id)
                .expect("resolve project open ID"),
            crate::system_open::prepare_local_open(
                Path::new(&created.project_path),
                Some(crate::system_open::LocalOpenKind::Folder),
            )
            .expect("prepare created project")
            .path()
        );

        fs::remove_dir_all(parent).expect("cleanup project open-ID fixture");
    }

    #[cfg(unix)]
    #[test]
    fn temporary_path_open_ids_reject_symbolic_links() {
        use std::os::unix::fs::symlink;

        let directory =
            std::env::temp_dir().join(format!("ihub-open-link-test-{}", uuid::Uuid::new_v4()));
        let target = directory.join("target.txt");
        let link = directory.join("link.txt");
        fs::create_dir_all(&directory).expect("create symlink fixture directory");
        fs::write(&target, "iHub").expect("create symlink target");
        symlink(&target, &link).expect("create symlink fixture");

        assert!(TemporaryPathOpenStore::default()
            .issue(&link)
            .expect_err("a symlink must not receive an open ID")
            .contains("Symbolic links"));

        fs::remove_dir_all(directory).expect("cleanup symlink fixture directory");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn temporary_path_open_ids_reject_network_namespaces() {
        assert!(TemporaryPathOpenStore::default()
            .issue(Path::new(r"\\server\share\file.txt"))
            .expect_err("a network namespace must not receive an open ID")
            .contains("UNC"));
    }

    #[test]
    fn pasted_clipboard_images_are_encoded_and_rejected_when_malformed() {
        use base64::{engine::general_purpose::STANDARD, Engine as _};

        let image = arboard::ImageData {
            width: 1,
            height: 1,
            bytes: Cow::Owned(vec![0x12, 0x34, 0x56, 0xff]),
        };
        let payload = clipboard_image_from_rgba(image).expect("a small RGBA bitmap should encode");
        assert_eq!(payload.width, 1);
        assert_eq!(payload.height, 1);
        assert_eq!(payload.mime_type, "image/png");
        let encoded = payload
            .data_url
            .strip_prefix("data:image/png;base64,")
            .expect("a PNG data URL");
        let png = STANDARD.decode(encoded).expect("valid base64 PNG");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");

        let malformed = arboard::ImageData {
            width: 1,
            height: 1,
            bytes: Cow::Owned(vec![0, 0, 0]),
        };
        assert!(clipboard_image_from_rgba(malformed)
            .expect_err("wrong RGBA byte lengths must be refused")
            .contains("invalid RGBA"));

        let oversized = arboard::ImageData {
            width: 8_193,
            height: 1,
            bytes: Cow::Owned(Vec::new()),
        };
        assert!(clipboard_image_from_rgba(oversized)
            .expect_err("edge limits must apply before allocating")
            .contains("edge limit"));
    }

    #[test]
    fn utools_copy_image_decodes_only_bounded_png_data_urls() {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        use image::{codecs::png::PngEncoder, ColorType, ImageEncoder};

        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(&[0x12, 0x34, 0x56, 0xff], 1, 1, ColorType::Rgba8.into())
            .expect("a one-pixel PNG should encode");
        let data_url = format!("data:image/png;base64,{}", STANDARD.encode(&png));
        let decoded = decode_utools_clipboard_png_data_url(&data_url)
            .expect("a bounded PNG data URL should decode");
        assert_eq!((decoded.width, decoded.height), (1, 1));
        assert_eq!(decoded.bytes.as_ref(), &[0x12, 0x34, 0x56, 0xff]);

        assert!(
            decode_utools_clipboard_png_data_url("C:\\untrusted\\image.png")
                .expect_err("filesystem paths must not bypass a picker grant")
                .contains("PNG data URL")
        );
        assert!(
            decode_utools_clipboard_png_data_url("data:image/jpeg;base64,/9j/")
                .expect_err("non-PNG formats must be rejected")
                .contains("PNG data URL")
        );
        let oversized = format!(
            "data:image/png;base64,{}",
            "A".repeat(MAX_UTOOLS_COPY_IMAGE_SOURCE_BYTES.div_ceil(3) * 4 + 1)
        );
        assert!(decode_utools_clipboard_png_data_url(&oversized)
            .expect_err("compressed PNG bytes must be bounded before decoding")
            .contains("limited"));
    }

    #[test]
    fn utools_copy_image_reads_only_picker_granted_image_files() {
        use image::{codecs::jpeg::JpegEncoder, ColorType, ImageEncoder};

        let directory = std::env::temp_dir().join(format!(
            "ihub-utools-copy-image-grant-test-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&directory).expect("create image grant fixture directory");
        let path = directory.join("selected.jpg");
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, 90)
            .write_image(&[0x12, 0x34, 0x56], 1, 1, ColorType::Rgb8.into())
            .expect("encode picker-selected JPEG fixture");
        fs::write(&path, jpeg).expect("write picker-selected JPEG fixture");
        let canonical = canonical_selected_file(path)
            .expect("canonicalize picker-selected JPEG fixture")
            .path
            .to_string_lossy()
            .into_owned();
        let host = PluginHostState::default();
        remember_utools_drag_grants(
            &host,
            "image-plugin",
            "image-lease",
            std::slice::from_ref(&canonical),
            crate::system_open::LocalOpenKind::File,
        )
        .expect("picker selection should create the shared path grant");

        let decoded = decode_authorized_utools_clipboard_image(
            &host,
            "image-plugin",
            "image-lease",
            &json!({ "path": canonical }),
            "copyImage",
        )
        .expect("the same plugin lease should read the selected JPEG");
        assert_eq!((decoded.width, decoded.height), (1, 1));
        assert_eq!(decoded.bytes.len(), 4);
        assert!(decode_authorized_utools_clipboard_image(
            &host,
            "other-plugin",
            "image-lease",
            &json!({ "path": canonical }),
            "copyImage",
        )
        .expect_err("another plugin must not reuse the selected image path")
        .contains("showOpenDialog"));
        let missing = directory.join("never-selected.png");
        assert!(decode_authorized_utools_clipboard_image(
            &host,
            "image-plugin",
            "image-lease",
            &json!({ "path": missing.to_string_lossy() }),
            "copyImage",
        )
        .expect_err("an unselected path must be rejected before filesystem probing")
        .contains("showOpenDialog"));

        fs::remove_dir_all(directory).expect("cleanup image grant fixture directory");
    }

    #[test]
    fn utools_copy_file_validates_strings_without_probing_the_filesystem() {
        let missing = std::env::temp_dir().join(format!(
            "ihub-utools-copy-file-missing-{}",
            uuid::Uuid::new_v4()
        ));
        let validated = validate_utools_copy_file_paths(&json!({
            "paths": [missing.to_string_lossy()]
        }))
        .expect("pre-confirmation validation must not inspect path existence");
        assert_eq!(validated, vec![missing.clone()]);

        assert!(validate_utools_copy_file_paths(&json!({
            "paths": [missing.to_string_lossy(), missing.to_string_lossy()]
        }))
        .expect_err("duplicates must be rejected")
        .contains("duplicate"));
        assert!(
            validate_utools_copy_file_paths(&json!({ "paths": ["relative.txt"] }))
                .expect_err("relative paths must not reach the clipboard")
                .contains("absolute")
        );
        assert!(
            validate_utools_copy_file_paths(&json!({ "paths": ["bad\u{0}path"] }))
                .expect_err("control characters must be rejected")
                .contains("controls")
        );
    }

    #[test]
    fn utools_file_drag_is_bound_to_the_picker_lease_and_object_identity() {
        let directory = std::env::temp_dir().join(format!(
            "ihub-utools-drag-grant-test-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&directory).expect("create file drag fixture directory");
        let path = directory.join("selected.txt");
        fs::write(&path, "original").expect("create selected drag fixture");
        let canonical = canonical_selected_file(path.clone())
            .expect("canonicalize selected drag fixture")
            .path
            .to_string_lossy()
            .into_owned();
        let host = PluginHostState::default();
        remember_utools_drag_grants(
            &host,
            "plugin-one",
            "lease-one",
            std::slice::from_ref(&canonical),
            crate::system_open::LocalOpenKind::File,
        )
        .expect("native picker result should create a drag grant");

        let params = json!({ "paths": [canonical] });
        let prepared =
            prepare_authorized_utools_drag_paths(&host, "plugin-one", "lease-one", &params)
                .expect("the exact lease should prepare the selected object");
        assert_eq!(prepared.len(), 1);
        drop(prepared);
        assert!(
            prepare_authorized_utools_drag_paths(&host, "plugin-two", "lease-one", &params,)
                .is_err()
        );
        assert!(
            prepare_authorized_utools_drag_paths(&host, "plugin-one", "lease-two", &params,)
                .is_err()
        );

        let replacement = directory.join("replacement.txt");
        fs::write(&replacement, "replacement").expect("create replacement drag fixture");
        fs::remove_file(&path).expect("remove selected drag fixture");
        fs::rename(&replacement, &path).expect("replace selected path with another object");
        assert!(
            prepare_authorized_utools_drag_paths(&host, "plugin-one", "lease-one", &params,)
                .expect_err("a same-kind replacement must invalidate the drag grant")
                .contains("changed")
        );

        clear_plugin_runtime_state(&host, "plugin-one");
        assert!(host.utools_drag_grants.lock().unwrap().is_empty());
        fs::remove_dir_all(directory).expect("cleanup file drag fixture directory");
    }

    #[test]
    fn utools_ffmpeg_paths_are_picker_bound_and_publish_once() {
        let directory = std::env::temp_dir().join(format!(
            "ihub-utools-ffmpeg-grant-test-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&directory).expect("create FFmpeg fixture directory");
        let directory = std::path::PathBuf::from(
            super::canonical_selected_directory(directory)
                .expect("canonicalize FFmpeg fixture directory"),
        );
        let input = directory.join("input.mp4");
        fs::write(&input, b"fixture").expect("create FFmpeg input fixture");
        let input = canonical_selected_file(input)
            .expect("canonicalize FFmpeg input fixture")
            .path
            .to_string_lossy()
            .into_owned();
        let output = directory.join("output.webm").to_string_lossy().into_owned();
        let host = PluginHostState::default();
        remember_utools_drag_grants(
            &host,
            "plugin-one",
            "lease-one",
            std::slice::from_ref(&input),
            crate::system_open::LocalOpenKind::File,
        )
        .expect("remember FFmpeg input picker grant");
        remember_utools_save_grant(&host, "plugin-one", "lease-one", &output)
            .expect("remember FFmpeg output picker grant");
        let request_id = uuid::Uuid::new_v4().to_string();
        let (_, run) = prepare_utools_ffmpeg_run(
            &host,
            "plugin-one",
            "lease-one",
            &json!({
                "requestId": request_id,
                "args": ["-y", "-i", input, "-t", "00:00:02", output]
            }),
        )
        .expect("exact picker paths should prepare FFmpeg");
        assert_eq!(run.duration_seconds, Some(2.0));
        assert!(run.staging_output.starts_with(&directory));
        assert!(run
            .args
            .last()
            .is_some_and(|value| value == &run.staging_output.to_string_lossy()));
        assert!(host.utools_save_grants.lock().unwrap().is_empty());

        fs::write(&run.staging_output, b"encoded").expect("create staged FFmpeg output");
        publish_utools_ffmpeg_output(&run).expect("publish exact FFmpeg output once");
        assert_eq!(fs::read(&run.output_grant.path).unwrap(), b"encoded");
        assert!(publish_utools_ffmpeg_output(&run).is_err());
        drop(run);
        fs::remove_dir_all(directory).expect("cleanup FFmpeg fixture directory");
    }

    #[test]
    fn utools_ffmpeg_rejects_network_and_ungranted_output_paths() {
        let host = PluginHostState::default();
        let request_id = uuid::Uuid::new_v4().to_string();
        let output = std::env::temp_dir()
            .join(format!("ihub-ungranted-{}.mp4", uuid::Uuid::new_v4()))
            .to_string_lossy()
            .into_owned();
        assert!(prepare_utools_ffmpeg_run(
            &host,
            "plugin-one",
            "lease-one",
            &json!({ "requestId": request_id, "args": ["-i", "https://example.com/video", output] })
        )
        .expect_err("network inputs must remain unavailable")
        .contains("network"));
    }

    #[test]
    fn utools_local_shell_paths_are_lexical_before_confirmation() {
        let missing = std::env::temp_dir().join(format!(
            "ihub-utools-local-shell-missing-{}",
            uuid::Uuid::new_v4()
        ));
        assert_eq!(
            validate_utools_shell_local_path(
                &json!({ "path": missing.to_string_lossy() }),
                "openPath"
            )
            .expect("missing paths are not probed before user confirmation"),
            missing
        );
        assert!(
            validate_utools_shell_local_path(&json!({ "path": "relative.txt" }), "openPath")
                .expect_err("relative paths must be rejected")
                .contains("absolute")
        );
        assert!(validate_utools_shell_local_path(
            &json!({ "path": missing.to_string_lossy(), "extra": true }),
            "trashItem"
        )
        .expect_err("extra fields must be rejected")
        .contains("exactly one"));
    }

    #[test]
    fn plugin_clipboard_history_snapshot_is_opt_in_bounded_and_read_only() {
        let directory = std::env::temp_dir().join(format!(
            "ihub-plugin-history-snapshot-test-{}",
            uuid::Uuid::new_v4()
        ));
        let history = ClipboardHistory::new(directory.clone());

        let initial = plugin_clipboard_history_snapshot(&history);
        assert!(!initial.enabled);
        assert!(initial.items.is_empty());
        assert!(
            !directory.exists(),
            "a read-only plugin snapshot must not create or persist history state"
        );

        history
            .set_enabled(true)
            .expect("enable the built-in history");
        for index in 0..(MAX_PLUGIN_CLIPBOARD_HISTORY_ITEMS + 3) {
            assert!(history
                .capture_text(format!("entry-{index}"))
                .expect("capture enabled history text"));
        }
        let before = history.snapshot(Some(100));
        let snapshot = plugin_clipboard_history_snapshot(&history);

        assert!(snapshot.enabled);
        assert_eq!(snapshot.items.len(), MAX_PLUGIN_CLIPBOARD_HISTORY_ITEMS);
        assert_eq!(
            history.snapshot(Some(100)).items,
            before.items,
            "a plugin snapshot must not mutate the host history or trigger capture"
        );

        fs::remove_dir_all(directory).expect("cleanup clipboard history fixture");
    }

    #[test]
    fn native_dialog_guard_only_suspends_auto_hide_while_the_picker_is_open() {
        let host = PluginHostState::default();
        assert!(!host.native_dialog_is_open());
        assert!(!host.auto_hide_is_suspended());
        {
            let _dialog = NativeDialogGuard::begin(&host);
            assert!(host.native_dialog_is_open());
            assert!(host.auto_hide_is_suspended());
        }
        assert!(!host.native_dialog_is_open());
        assert!(!host.auto_hide_is_suspended());
    }

    #[test]
    fn capture_focus_lease_suspends_auto_hide_only_until_release_or_expiry() {
        let host = PluginHostState::default();
        assert!(!host.auto_hide_is_suspended());

        let lease_id = host.acquire_capture_focus_lease();
        assert!(host.capture_focus_lease_is_active());
        assert!(host.auto_hide_is_suspended());

        host.release_capture_focus_lease(&lease_id);
        assert!(!host.capture_focus_lease_is_active());
        assert!(!host.auto_hide_is_suspended());

        host.capture_focus_leases
            .lock()
            .expect("capture lease lock")
            .insert(
                "expired-capture-focus".to_owned(),
                CaptureFocusLease {
                    owner_plugin_id: None,
                    expires_at: Instant::now()
                        .checked_sub(Duration::from_secs(1))
                        .expect("current instant should have a prior second"),
                },
            );
        assert!(
            !host.capture_focus_lease_is_active(),
            "an expired renderer lease must not disable focus-loss hiding"
        );
        assert!(host
            .capture_focus_leases
            .lock()
            .expect("capture lease lock")
            .is_empty());
    }

    #[test]
    fn plugin_capture_focus_leases_are_owner_scoped_and_revoked_with_runtime_state() {
        let host = PluginHostState::default();
        let owner = "ihub-plugin-capture-owner";
        let other = "ihub-plugin-capture-other";
        let owner_lease = host.acquire_plugin_capture_focus_lease(owner);
        let other_lease = host.acquire_plugin_capture_focus_lease(other);

        let mismatch = host
            .release_plugin_capture_focus_lease(other, &owner_lease)
            .expect_err("another plugin must not release the owner's lease");
        assert!(mismatch.contains("another plugin"));
        assert!(host.capture_focus_lease_is_active());

        // The trusted host's identity-free command must not become an escape
        // hatch for a plugin lease ID forwarded or guessed by another caller.
        host.release_capture_focus_lease(&owner_lease);
        assert!(host
            .release_plugin_capture_focus_lease(owner, &owner_lease)
            .expect("the owner may still release its own lease"));
        assert!(host.capture_focus_lease_is_active());

        clear_plugin_runtime_state(&host, other);
        assert!(
            !host.capture_focus_lease_is_active(),
            "closing a plugin runtime must revoke its pending screen picker lease"
        );
        assert!(!host
            .release_plugin_capture_focus_lease(other, &other_lease)
            .expect("a revoked lease should be harmless to retry from finally"));
    }

    #[test]
    fn plugin_capture_focus_leases_replace_per_plugin_and_keep_the_global_bound() {
        let host = PluginHostState::default();
        let owner = "ihub-plugin-capture-replacement";
        let first = host.acquire_plugin_capture_focus_lease(owner);
        let replacement = host.acquire_plugin_capture_focus_lease(owner);
        assert_ne!(first, replacement);
        assert!(!host
            .release_plugin_capture_focus_lease(owner, &first)
            .expect("a superseded lease should be harmless to retry"));
        assert!(host
            .release_plugin_capture_focus_lease(owner, &replacement)
            .expect("the replacement lease should remain owned by the plugin"));

        for index in 0..(MAX_CAPTURE_FOCUS_LEASES + 2) {
            let plugin_id = format!("ihub-plugin-capture-bound-{index}");
            host.acquire_plugin_capture_focus_lease(&plugin_id);
        }
        assert!(
            host.capture_focus_leases
                .lock()
                .expect("capture lease lock")
                .len()
                <= MAX_CAPTURE_FOCUS_LEASES,
            "screen-picker focus protection must stay globally bounded"
        );
    }

    #[test]
    fn cursor_color_approval_is_plugin_and_lease_scoped_single_use() {
        let host = PluginHostState::default();
        let owner = "ihub-plugin-cursor-owner";
        let other = "ihub-plugin-cursor-other";
        let lease_id = "visible-surface-lease";
        let approval_id = host.issue_plugin_cursor_color_approval(owner, lease_id);

        let mismatch = host
            .take_plugin_cursor_color_approval(other, lease_id, &approval_id)
            .expect_err("another plugin must not consume the owner's approval");
        assert!(mismatch.contains("another plugin"));

        // A rejected cross-plugin attempt must not cancel the owner's action.
        host.take_plugin_cursor_color_approval(owner, lease_id, &approval_id)
            .expect("the real owner may consume its own approval once");
        assert!(host
            .take_plugin_cursor_color_approval(owner, lease_id, &approval_id)
            .expect_err("an approval must be single-use")
            .contains("expired or was already used"));

        host.cursor_color_approvals
            .lock()
            .expect("cursor approval lock")
            .insert(
                "expired-cursor-color".to_owned(),
                CursorColorApproval {
                    plugin_id: owner.to_owned(),
                    lease_id: lease_id.to_owned(),
                    expires_at: Instant::now()
                        .checked_sub(Duration::from_secs(1))
                        .expect("current instant should have a prior second"),
                },
            );
        assert!(host
            .take_plugin_cursor_color_approval(owner, lease_id, "expired-cursor-color")
            .expect_err("expired approvals must not be usable")
            .contains("expired or was already used"));
    }

    #[test]
    fn cursor_color_sampling_is_rate_limited_and_revoked_with_the_runtime() {
        let host = PluginHostState::default();
        let owner = "ihub-plugin-cursor-rate-owner";
        let other = "ihub-plugin-cursor-rate-other";

        host.reserve_plugin_cursor_color_sample(owner)
            .expect("first owner sample should reserve");
        assert!(host
            .reserve_plugin_cursor_color_sample(owner)
            .expect_err("the same plugin must not turn samples into polling")
            .contains("rate-limited"));
        host.reserve_plugin_cursor_color_sample(other)
            .expect("one plugin's cooldown must not affect another");

        clear_plugin_runtime_state(&host, owner);
        host.reserve_plugin_cursor_color_sample(owner)
            .expect("closing a runtime must discard its pending cooldown state");
    }

    #[test]
    fn plugin_diagnostics_are_rate_limited_and_report_aggregated_drops() {
        let host = PluginHostState::default();
        let plugin_id = "bounded-logger";
        let started_at = Instant::now();
        for _ in 0..MAX_PLUGIN_LOGS_PER_WINDOW {
            assert_eq!(
                host.admit_plugin_log_at(plugin_id, started_at),
                PluginLogAdmission::Accept {
                    previously_dropped: 0
                }
            );
        }
        assert_eq!(
            host.admit_plugin_log_at(plugin_id, started_at),
            PluginLogAdmission::Drop { report_limit: true }
        );
        assert_eq!(
            host.admit_plugin_log_at(plugin_id, started_at),
            PluginLogAdmission::Drop {
                report_limit: false
            }
        );
        clear_plugin_runtime_state(&host, plugin_id);
        assert_eq!(
            host.admit_plugin_log_at(plugin_id, started_at),
            PluginLogAdmission::Drop {
                report_limit: false
            },
            "disposing and reopening a plugin runtime must not reset the diagnostics limiter"
        );
        assert_eq!(
            host.admit_plugin_log_at(plugin_id, started_at + PLUGIN_LOG_WINDOW),
            PluginLogAdmission::Accept {
                previously_dropped: 3
            }
        );
    }

    #[test]
    fn plugin_notifications_are_bounded_and_source_safe() {
        assert_eq!(
            plugin_notification_body(
                &json!({ "title": "Build complete", "body": "12 files", "level": "success" }),
                false,
            )
            .expect("valid SDK notification"),
            "Build complete\n12 files"
        );
        assert_eq!(
            plugin_notification_body(&json!({ "title": "Build complete", "body": "  " }), false)
                .expect("blank optional body is omitted"),
            "Build complete"
        );
        assert_eq!(
            plugin_notification_body(&json!({ "body": "兼容通知" }), true)
                .expect("valid compatibility notification"),
            "兼容通知"
        );
        assert_eq!(
            plugin_notification_body(
                &json!({ "body": "兼容通知", "clickFeatureCode": "open" }),
                true,
            )
            .expect("click routing stays outside the displayed body"),
            "兼容通知"
        );
        assert_eq!(
            utools_notification_click_feature_code(
                &json!({ "body": "兼容通知", "clickFeatureCode": " open " })
            )
            .expect("valid click feature code"),
            Some("open".to_owned())
        );

        for params in [
            json!({}),
            json!({ "title": "" }),
            json!({ "title": 7 }),
            json!({ "title": "ok", "body": 7 }),
            json!({ "title": "ok", "level": "critical" }),
            json!({ "title": "ok", "action": "spoof" }),
        ] {
            assert!(plugin_notification_body(&params, false).is_err());
        }
        assert!(plugin_notification_body(
            &json!({ "body": "x".repeat(MAX_PLUGIN_NOTIFICATION_BODY_CHARS + 1) }),
            true,
        )
        .is_err());
        for value in [json!(7), json!(""), json!("bad\ncode")] {
            assert!(utools_notification_click_feature_code(
                &json!({ "body": "ok", "clickFeatureCode": value })
            )
            .is_err());
        }
    }

    #[test]
    fn plugin_notifications_remain_rate_limited_across_runtime_disposal() {
        let host = PluginHostState::default();
        let plugin_id = "bounded-notifier";
        let started_at = Instant::now();
        for _ in 0..MAX_PLUGIN_NOTIFICATIONS_PER_WINDOW {
            assert!(host.admit_plugin_notification_at(plugin_id, started_at));
        }
        assert!(!host.admit_plugin_notification_at(plugin_id, started_at));
        clear_plugin_runtime_state(&host, plugin_id);
        assert!(!host.admit_plugin_notification_at(plugin_id, started_at));
        assert!(
            host.admit_plugin_notification_at(plugin_id, started_at + PLUGIN_NOTIFICATION_WINDOW,)
        );
    }

    #[test]
    fn plugin_cursor_color_projection_never_serializes_screen_coordinates() {
        let projected = PluginCursorColor::from(crate::native_color_picker::CursorColorSample {
            hex: "#12AB34".to_owned(),
            rgb: "rgb(18, 171, 52)".to_owned(),
            x: -1800,
            y: 926,
        });
        let value = serde_json::to_value(projected).expect("color result should serialize");
        assert_eq!(
            value,
            json!({
                "hex": "#12AB34",
                "rgb": "rgb(18, 171, 52)"
            })
        );
        assert!(value.get("x").is_none());
        assert!(value.get("y").is_none());
    }

    #[test]
    fn cursor_color_bridge_params_cannot_supply_capture_options_or_self_approval() {
        assert_eq!(
            cursor_color_approval_id(&json!({ "approvalId": "host-token" })),
            Ok("host-token")
        );
        for params in [
            json!({}),
            json!({ "approvalId": "host-token", "delayMs": 0 }),
            json!({ "approvalId": "host-token", "x": 4, "y": 7 }),
            json!({ "approved": true }),
        ] {
            assert!(cursor_color_approval_id(&params).is_err());
        }

        let untrusted_default: PluginHostRequest = serde_json::from_value(json!({
            "pluginId": "ihub-plugin-cursor-owner",
            "leaseId": "lease",
            "method": "cursorColor.sampleOnce",
            "params": { "approvalId": "host-token" }
        }))
        .expect("bridge fixture should deserialize");
        assert!(
            !untrusted_default.surface,
            "a caller that omits the host-owned surface role must not become visible by default"
        );
    }
}
