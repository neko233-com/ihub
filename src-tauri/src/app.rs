use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, Write},
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
use image::{codecs::png::PngEncoder, ColorType, ImageEncoder};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, State, WindowEvent,
};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use tauri_plugin_notification::NotificationExt;
use uuid::Uuid;

use crate::{
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
    plugin_asset_server::{PluginAssetServer, PluginFrontendLease, PluginFrontendPurpose},
    plugin_settings::PluginSettingsStore,
    plugin_shortcuts::{
        apply_plugin_shortcut_statuses, binding_is_current, binding_targets_frontend_command,
        plan_plugin_shortcuts, PluginShortcutBinding, PluginShortcutEvent, PluginShortcutRegistry,
        PluginShortcutStatus,
    },
    plugins::PluginManager,
    project_template::create_plugin_project as create_plugin_project_template,
    super_panel::{SuperPanelState, SuperPanelStatus, SuperPanelTrigger},
    system_open::{LocalOpenKind, LocalPathIdentity, PreparedLocalOpen},
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
    plugin_settings: PluginSettingsStore,
    host: Arc<PluginHostState>,
    launcher_focus: LauncherFocusGate,
    launcher_hotkey_store: LauncherHotkeyStore,
    /// Serializes native register/persist/unregister transactions so two rapid
    /// settings clicks cannot strand the resident launcher without a binding.
    launcher_hotkey_change: Mutex<()>,
    launcher_hotkey: Mutex<LauncherHotkeyStatus>,
    launcher_hotkey_toggle: Mutex<LauncherHotkeyToggleGate>,
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
        let plugin_settings = PluginSettingsStore::new(app_data_dir.clone());
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
            plugin_settings,
            host: Arc::new(PluginHostState::default()),
            launcher_focus: LauncherFocusGate::default(),
            launcher_hotkey_store: LauncherHotkeyStore::new(app_data_dir),
            launcher_hotkey_change: Mutex::new(()),
            launcher_hotkey: Mutex::new(LauncherHotkeyStatus::unavailable()),
            launcher_hotkey_toggle: Mutex::new(LauncherHotkeyToggleGate::default()),
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

    fn plugin_shortcut_binding(&self, shortcut: &str) -> Option<PluginShortcutBinding> {
        self.plugin_shortcuts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .active
            .get(shortcut)
            .cloned()
    }

    fn project_plugin_shortcut_statuses(&self, plugins: &mut [PluginInfo]) {
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
            filesystem_grants: Mutex::new(HashMap::new()),
            file_grants: Mutex::new(HashMap::new()),
            launcher_contexts: Mutex::new(HashMap::new()),
            batch_rename_previews: Mutex::new(HashMap::new()),
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

const PLUGIN_SEARCH_TIMEOUT: Duration = Duration::from_millis(280);
const MAX_PENDING_PLUGIN_SEARCHES: usize = 24;
const MAX_PLUGIN_SEARCH_RESULTS: usize = 6;
const MAX_PLUGIN_SEARCH_QUERY_BYTES: usize = 512;
const MAX_PLUGIN_SEARCH_TEXT_CHARS: usize = 320;
const MAX_PLUGIN_SEARCH_PAYLOAD_BYTES: usize = 8 * 1024;
const PLUGIN_SEARCH_SELECTION_TTL: Duration = Duration::from_secs(60);
const MAX_ISSUED_PLUGIN_SEARCHES: usize = 64;
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
const MAX_PLUGIN_CLIPBOARD_TEXT_BYTES: usize = 48 * 1024;
const MAX_UTOOLS_TYPED_TEXT_CHARS: usize = 4_096;

enum UtoolsInputAction {
    PasteText,
    TypeString(String),
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

#[tauri::command]
pub async fn get_plugin_frontend_url(
    plugin_id: String,
    purpose: Option<PluginFrontendPurpose>,
    window: tauri::WebviewWindow,
    detached: State<'_, DetachedPluginWindowRegistry>,
    state: State<'_, AppState>,
) -> Result<PluginFrontendLease, String> {
    if !is_plugin_id(&plugin_id) {
        return Err("Invalid plugin ID.".to_owned());
    }
    let caller_label = window.label().to_owned();
    let purpose = purpose.unwrap_or(PluginFrontendPurpose::Surface);
    let detached_caller = caller_label != "main";
    if detached_caller {
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

    let plugins = state.plugins.clone();
    let plugin_assets = state.plugin_assets.clone();
    let host = state.host.clone();
    let lease_plugin_id = plugin_id.clone();
    let lease = tauri::async_runtime::spawn_blocking(move || {
        let server = plugin_assets.clone();
        plugin_assets.with_plugin_operation(&lease_plugin_id, || {
            let bundle = plugins.frontend_asset_bundle(&lease_plugin_id)?;
            let resolved_plugin_id = bundle.plugin_id.clone();
            let lease = server.issue(bundle, purpose)?;
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
    state: State<'_, AppState>,
) -> Result<PluginCursorColorApproval, String> {
    if !is_plugin_id(&plugin_id) {
        return Err("Invalid plugin ID.".to_owned());
    }
    validate_plugin_renderer_lease_caller(&window, &detached, &plugin_id, &lease_id)?;
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
        let plugin_id = plugin_assets.release(lease_id)?;
        // Closing a surface is a cancellation boundary. In particular,
        // a just-dispatched launcher-context token must not remain
        // consumable while no matching iframe is alive.
        clear_plugin_runtime_state(&host, &plugin_id);
        Some(plugin_id)
    })
}

#[tauri::command]
pub fn release_plugin_frontend_url(
    lease_id: String,
    window: tauri::WebviewWindow,
    detached: State<'_, DetachedPluginWindowRegistry>,
    state: State<'_, AppState>,
) {
    if window.label() != "main" && !detached.unbind_owned_lease(window.label(), &lease_id) {
        return;
    }
    if let Some(plugin_id) = release_plugin_frontend_lease(&lease_id, &state) {
        emit_plugin_search_providers_changed(window.app_handle(), &plugin_id, None, false);
    }
}

/// Renews a renderer-owned frontend lease. The main React host sends a small
/// heartbeat while its iframe exists so a crashed/reloaded renderer cannot
/// permanently consume a loopback listener.
#[tauri::command]
pub fn touch_plugin_frontend_lease(
    lease_id: String,
    window: tauri::WebviewWindow,
    detached: State<'_, DetachedPluginWindowRegistry>,
    state: State<'_, AppState>,
) -> bool {
    if window.label() != "main" && !detached.owns_lease(window.label(), &lease_id) {
        return false;
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
    let plugin_settings = state.plugin_settings.clone();
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
    let request = request.unwrap_or_default();
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
    state: State<'_, AppState>,
) -> Result<Value, String> {
    let request = request.request;
    if !is_plugin_id(&request.plugin_id) {
        return Err("Invalid plugin ID.".to_owned());
    }
    validate_plugin_renderer_lease_caller(
        &window,
        &detached,
        &request.plugin_id,
        &request.lease_id,
    )?;
    let request_plugin_id = request.plugin_id.clone();
    let plugin_assets = state.plugin_assets.clone();
    let server = plugin_assets.clone();

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
            if !state
                .plugins
                .has_declared_search_provider(&request.plugin_id, provider_id)?
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
            hide_main_for_utools_input(app)?;
            schedule_utools_input(UtoolsInputAction::PasteText)?;
            Ok(json!({ "accepted": true }))
        }
        "compatibility.utools.input.typeString" => {
            let value = validate_utools_input_text(
                &request.params,
                MAX_PLUGIN_CLIPBOARD_TEXT_BYTES,
                Some(MAX_UTOOLS_TYPED_TEXT_CHARS),
            )?;
            hide_main_for_utools_input(app)?;
            schedule_utools_input(UtoolsInputAction::TypeString(value.to_owned()))?;
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
    if !state.host.admit_plugin_notification(&request.plugin_id) {
        return Err(format!(
            "Plugin notifications are limited to {MAX_PLUGIN_NOTIFICATIONS_PER_WINDOW} every {} seconds.",
            PLUGIN_NOTIFICATION_WINDOW.as_secs()
        ));
    }
    app.notification()
        .builder()
        .title(format!("iHub · {}", request.plugin_id))
        .body(body)
        .show()
        .map_err(|error| format!("Could not show the system notification: {error}"))?;
    Ok(json!({ "accepted": true }))
}

fn plugin_notification_body(
    params: &Value,
    compatibility_body_only: bool,
) -> Result<String, String> {
    let Some(object) = params.as_object() else {
        return Err("Plugin notification parameters must be an object.".to_owned());
    };
    let allowed_keys: &[&str] = if compatibility_body_only {
        &["body"]
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
    if (request.method.starts_with("compatibility.utools.window.")
        || request.method.starts_with("compatibility.utools.input."))
        && !request.surface
    {
        return Err(
            "uTools window and input compatibility methods require the plugin's visible active surface."
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
            if !state
                .plugins
                .has_declared_search_provider(&plugin_id, &provider_id)?
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
    let plugins = state.plugins.list();
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

fn release_detached_plugin_window_lease(window: &tauri::Window) {
    if window.label() == "main" {
        return;
    }
    let Some(detached) = window.try_state::<DetachedPluginWindowRegistry>() else {
        return;
    };
    let Some(lease_id) = detached.take_window_lease(window.label()) else {
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
                    release_detached_plugin_window_lease(window);
                }
            }
            WindowEvent::Destroyed => {
                host_log::debug("lifecycle", "A host window was destroyed.");
                // Platform shutdown paths can skip CloseRequested. This is
                // idempotent after the normal close branch.
                release_detached_plugin_window_lease(window);
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
            crate::detached_plugin_window::open_detached_plugin_window,
            crate::detached_plugin_window::get_detached_plugin_window_bootstrap,
            crate::detached_plugin_window::close_detached_plugin_window,
            get_plugin_frontend_url,
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
            query_plugin_search
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
    let plugins = state.plugins.list();
    let mut plan = plan_plugin_shortcuts(&plugins, &launcher_reserved_plugin_shortcuts(&state));
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
    let plugins = state.plugins.list();
    if state.host.auto_hide_is_suspended()
        || state
            .plugins
            .ensure_plugin_enabled(&binding.plugin_id)
            .is_err()
        || !binding_is_current(&plugins, &binding)
    {
        return;
    }
    let payload = PluginShortcutEvent::from_binding(&binding);
    if binding_targets_frontend_command(&plugins, &binding) {
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

    let _ = window.unminimize();
    if let Some(state) = window.try_state::<AppState>() {
        state.launcher_focus.begin_reveal();
    }
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
        let _ = window.unminimize();
        if fresh_reveal {
            if let Some(state) = window.try_state::<AppState>() {
                state.launcher_focus.begin_reveal();
            }
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

fn hide_main_for_utools_input(app: &AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "The iHub main window is unavailable for uTools input.".to_owned())?;
    window
        .hide()
        .map_err(|error| format!("Could not hide iHub before uTools input: {error}"))
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
        UtoolsInputAction::PasteText => send(&[
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
        for feature in utools_dynamic_features(settings, &plugin.id) {
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
    use crate::models::{LauncherHotkeyRegistration, LauncherHotkeyStatus, PluginSearchResult};

    use super::{
        attach_plugin_launcher_context_transfer, authorize_index_root_update,
        build_plugin_launcher_context_payload, canonical_selected_file, clear_plugin_runtime_state,
        clear_plugin_session_secrets, clipboard_files_from_paths, clipboard_image_from_rgba,
        complete_plugin_search, create_plugin_project_for_grant,
        create_plugin_project_with_open_grant, cursor_color_approval_id,
        decode_utools_db_storage_key, directory_for_grant, get_plugin_session_secret,
        issue_file_grant, issue_filesystem_grant, issue_plugin_launcher_context_transfer,
        launcher_visibility_action, native_plugin_command_input, normalize_plugin_search_results,
        normalized_host_target, optional_u32, optional_u8, physical_point_in_monitor,
        plugin_clipboard_history_snapshot, plugin_notification_body,
        plugin_search_providers_changed_payload, prepare_directory_for_grant,
        renderer_display_path, resolve_issued_plugin_search_selection,
        revoke_plugin_launcher_context_transfer, set_plugin_session_secret,
        startup_launcher_hotkey_candidates, take_file_grant, take_plugin_batch_rename_preview,
        take_plugin_launcher_context_transfer, truncate_utf8_bytes, utools_db_storage_key,
        utools_dynamic_feature_command_id, utools_dynamic_feature_key, validate_external_url,
        validate_local_search_selection, validate_plugin_clipboard_text,
        validate_system_icon_request, validate_utools_dynamic_feature,
        validate_utools_expend_height, validate_utools_input_text,
        validate_utools_window_request_params, CaptureFocusLease, CursorColorApproval,
        DetachedPluginFrontendEventRequest, IssuedPluginSearchResults, LauncherFocusGate,
        LauncherHotkeyToggleGate, LauncherInvocationSource, LauncherVisibilityAction,
        LauncherVisibilitySnapshot, LauncherWorkArea, NativeDialogGuard, PendingPluginSearch,
        PluginBatchRenamePreview, PluginCursorColor, PluginHostRequest, PluginHostState,
        PluginLauncherContextFileRequest, PluginLauncherContextImageRequest,
        PluginLauncherContextRequest, PluginLogAdmission, TemporaryPathOpenKind,
        TemporaryPathOpenStore, LAUNCHER_CONTEXT_TTL, LAUNCHER_FALLBACK_HOTKEY,
        LAUNCHER_HOTKEY_TOGGLE_DEBOUNCE, LAUNCHER_INITIAL_BLUR_GRACE, LAUNCHER_PRIMARY_HOTKEY,
        MAX_CAPTURE_FOCUS_LEASES, MAX_PLUGIN_CLIPBOARD_HISTORY_ITEMS,
        MAX_PLUGIN_CLIPBOARD_TEXT_BYTES, MAX_PLUGIN_LOGS_PER_WINDOW,
        MAX_PLUGIN_NOTIFICATIONS_PER_WINDOW, MAX_PLUGIN_NOTIFICATION_BODY_CHARS,
        MAX_PLUGIN_SEARCH_PAYLOAD_BYTES, PLUGIN_LOG_WINDOW, PLUGIN_NOTIFICATION_WINDOW,
        PLUGIN_SEARCH_SELECTION_TTL, TEMPORARY_PATH_OPEN_TTL,
    };

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
        assert!(plugin_notification_body(
            &json!({ "body": "ok", "clickFeatureCode": "unsafe" }),
            true,
        )
        .is_err());
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
