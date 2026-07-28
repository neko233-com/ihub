use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, Write},
    path::PathBuf,
    process::Command,
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
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, State, WindowEvent,
};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use uuid::Uuid;

use crate::{
    clipboard_history::{
        ClipboardHistory, ClipboardHistoryRestoreResult, ClipboardHistorySnapshot,
    },
    indexer::{default_root_strings, SearchIndex},
    launcher_hotkey::{normalize_launcher_hotkey, LauncherHotkeyStore, DEFAULT_LAUNCHER_HOTKEY},
    launcher_shortcuts::{LauncherShortcutStore, LauncherShortcutView},
    models::{
        AppHealth, AutostartStatus, ClipboardFile, ClipboardImage, IndexStatus,
        LauncherHotkeyStatus, OfficialWorkspacePluginProject, PluginAutomaticUpdateReport,
        PluginCommandResult, PluginInfo, PluginLifecycleUpdate, PluginProjectCreated,
        PluginSearchResponse, PluginSearchResult, PluginUninstallResult, PluginUpdateCheck,
        PluginUpdateResult, SearchResult,
    },
    plugin_asset_server::{PluginAssetServer, PluginFrontendLease, PluginFrontendPurpose},
    plugin_settings::PluginSettingsStore,
    plugins::PluginManager,
    project_template::create_plugin_project as create_plugin_project_template,
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
const LAUNCHER_DESIGN_WIDTH_LOGICAL: f64 = 1200.0;
const LAUNCHER_DESIGN_HEIGHT_LOGICAL: f64 = 756.0;

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

pub struct AppState {
    pub index: SearchIndex,
    pub plugins: PluginManager,
    pub clipboard_history: ClipboardHistory,
    pub cloud_drive: crate::cloud_drive::CloudDriveState,
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
}

impl AppState {
    fn new(app_data_dir: PathBuf) -> Self {
        let plugins = PluginManager::new();
        let plugin_settings = PluginSettingsStore::new(app_data_dir.clone());
        // Older development builds persisted every setting. Before plugin
        // frontends can access the host, scrub any value now declared secret
        // from that JSON file in one atomic update.
        if let Err(error) =
            plugin_settings.remove_declared_secrets(plugins.declared_secret_setting_keys())
        {
            eprintln!("iHub could not scrub legacy secret plugin settings: {error}");
        }
        Self {
            index: SearchIndex::with_storage(app_data_dir.clone()),
            plugins,
            clipboard_history: ClipboardHistory::new(app_data_dir.clone()),
            cloud_drive: crate::cloud_drive::CloudDriveState::new(app_data_dir.clone()),
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
}

impl Default for PluginHostState {
    fn default() -> Self {
        Self {
            commands: RwLock::new(HashMap::new()),
            search_providers: RwLock::new(HashMap::new()),
            secret_settings: RwLock::new(HashMap::new()),
            pending_searches: Mutex::new(HashMap::new()),
            filesystem_grants: Mutex::new(HashMap::new()),
            file_grants: Mutex::new(HashMap::new()),
            launcher_contexts: Mutex::new(HashMap::new()),
            batch_rename_previews: Mutex::new(HashMap::new()),
            native_dialog_depth: AtomicUsize::new(0),
            capture_focus_leases: Mutex::new(HashMap::new()),
            cursor_color_sampled_at: Mutex::new(HashMap::new()),
            cursor_color_approvals: Mutex::new(HashMap::new()),
        }
    }
}

#[derive(Debug, Clone)]
struct FilesystemGrant {
    plugin_id: String,
    directory: String,
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

const PLUGIN_SEARCH_TIMEOUT: Duration = Duration::from_millis(280);
const MAX_PENDING_PLUGIN_SEARCHES: usize = 24;
const MAX_PLUGIN_SEARCH_RESULTS: usize = 6;
const MAX_PLUGIN_SEARCH_QUERY_BYTES: usize = 512;
const MAX_PLUGIN_SEARCH_TEXT_CHARS: usize = 320;
const MAX_PLUGIN_SEARCH_PAYLOAD_BYTES: usize = 8 * 1024;
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

#[tauri::command]
pub fn index_default_roots(state: State<'_, AppState>) -> IndexStatus {
    state.index.rebuild_default_roots()
}

#[tauri::command]
pub fn set_index_roots(
    roots: Vec<String>,
    state: State<'_, AppState>,
) -> Result<IndexStatus, String> {
    state.index.set_roots(roots)
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
        let path = shortcuts.resolve_open_path(&shortcut_id, &index)?;
        open_path_in_system(&path)
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
    state.plugins.list()
}

#[tauri::command]
pub async fn get_plugin_frontend_url(
    plugin_id: String,
    purpose: Option<PluginFrontendPurpose>,
    state: State<'_, AppState>,
) -> Result<PluginFrontendLease, String> {
    let plugins = state.plugins.clone();
    let plugin_assets = state.plugin_assets.clone();
    let host = state.host.clone();
    let purpose = purpose.unwrap_or(PluginFrontendPurpose::Surface);
    tauri::async_runtime::spawn_blocking(move || {
        let server = plugin_assets.clone();
        plugin_assets.with_plugin_operation(&plugin_id, || {
            let bundle = plugins.frontend_asset_bundle(&plugin_id)?;
            let resolved_plugin_id = bundle.plugin_id.clone();
            let lease = server.issue(bundle, purpose)?;
            // A visible surface and hidden search runtime hand off ownership
            // for one plugin. A fresh lease therefore starts with no stale
            // command/provider/grant state from a prior document.
            clear_plugin_runtime_state(&host, &resolved_plugin_id);
            Ok(lease)
        })
    })
    .await
    .map_err(|error| format!("Plugin frontend bundle task failed: {error}"))?
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

/// Creates a one-time cursor-color approval only for the trusted React host
/// after it has shown its own visible confirmation overlay. Plugin iframes are
/// remote loopback origins and cannot call this Tauri command directly; they
/// receive only the final color value, never this token.
#[tauri::command]
pub fn issue_plugin_cursor_color_approval(
    plugin_id: String,
    lease_id: String,
    state: State<'_, AppState>,
) -> Result<PluginCursorColorApproval, String> {
    if !is_plugin_id(&plugin_id) {
        return Err("Invalid plugin ID.".to_owned());
    }
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
#[tauri::command]
pub fn release_plugin_frontend_url(lease_id: String, state: State<'_, AppState>) {
    // Use the same transition lock as bridge calls and plugin replacement.
    // Without it, a close/reload could remove a lease between `is_active_for`
    // and a sensitive host operation that had already begun.
    let plugin_assets = state.plugin_assets.clone();
    let host = state.host.clone();
    plugin_assets.with_plugin_operation("frontend-release", || {
        if let Some(plugin_id) = plugin_assets.release(&lease_id) {
            // Closing a surface is a cancellation boundary. In particular,
            // a just-dispatched launcher-context token must not remain
            // consumable while no matching iframe is alive.
            clear_plugin_runtime_state(&host, &plugin_id);
        }
    });
}

/// Renews a renderer-owned frontend lease. The main React host sends a small
/// heartbeat while its iframe exists so a crashed/reloaded renderer cannot
/// permanently consume a loopback listener.
#[tauri::command]
pub fn touch_plugin_frontend_lease(lease_id: String, state: State<'_, AppState>) -> bool {
    state.plugin_assets.touch(&lease_id)
}

#[tauri::command]
pub async fn install_plugin_from_git(
    source: String,
    state: State<'_, AppState>,
) -> Result<PluginInfo, String> {
    let plugins = state.plugins.clone();
    let plugin_assets = state.plugin_assets.clone();
    let host = state.host.clone();
    tauri::async_runtime::spawn_blocking(move || {
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
    .map_err(|error| format!("Plugin installation task failed: {error}"))?
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
            return Err(error);
        }
        Err(error) => {
            let finish_server = plugin_assets.clone();
            plugin_assets.with_plugin_operation(&plugin_id, || {
                finish_server.finish_plugin_transition(&plugin_id);
            });
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
    Ok(update)
}

/// Links an existing local plugin project for explicit development use. The
/// plugin stays in its original directory; iHub reads freshly built files from
/// that directory whenever the plugin frontend is opened again.
#[tauri::command]
pub async fn link_plugin_from_local(
    directory: String,
    state: State<'_, AppState>,
) -> Result<PluginInfo, String> {
    let plugins = state.plugins.clone();
    let plugin_assets = state.plugin_assets.clone();
    let host = state.host.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let server = plugin_assets.clone();
        plugin_assets.with_plugin_source_operation(|| {
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
    .map_err(|error| format!("Local plugin link task failed: {error}"))?
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
    state: State<'_, AppState>,
) -> Result<PluginInfo, String> {
    let plugins = state.plugins.clone();
    let plugin_assets = state.plugin_assets.clone();
    let host = state.host.clone();
    tauri::async_runtime::spawn_blocking(move || {
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
    .map_err(|error| format!("Official workspace plugin link task failed: {error}"))?
}

/// Removes iHub's local-development link metadata without deleting or editing
/// the developer's project directory.
#[tauri::command]
pub async fn unlink_plugin_from_local(
    plugin_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let plugins = state.plugins.clone();
    let plugin_assets = state.plugin_assets.clone();
    let host = state.host.clone();
    tauri::async_runtime::spawn_blocking(move || {
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
    .map_err(|error| format!("Local plugin unlink task failed: {error}"))?
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
    let update = tauri::async_runtime::spawn_blocking(move || {
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
    .map_err(|error| format!("Plugin lifecycle task failed: {error}"))??;
    let _ = app.emit(
        &format!("ihub://plugin/{}/lifecycle", update.plugin.id),
        json!({ "state": if enabled { "enabled" } else { "disabled" } }),
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
    let removed = tauri::async_runtime::spawn_blocking(move || {
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
                eprintln!(
                    "iHub could not remove settings for uninstalled plugin '{}': {error}",
                    removed.plugin_id
                );
            }
            Ok::<PluginUninstallResult, String>(removed)
        })
    })
    .await
    .map_err(|error| format!("Plugin uninstall task failed: {error}"))??;
    let _ = app.emit(
        &format!("ihub://plugin/{}/lifecycle", removed.plugin_id),
        json!({ "state": "uninstalled" }),
    );
    Ok(removed)
}

#[tauri::command]
pub async fn create_plugin_project(
    parent_directory: String,
    plugin_id: String,
) -> Result<PluginProjectCreated, String> {
    tauri::async_runtime::spawn_blocking(move || {
        create_plugin_project_template(&parent_directory, &plugin_id)
    })
    .await
    .map_err(|error| format!("Plugin project creation task failed: {error}"))?
}

/// Opens a host-owned directory chooser for first-party tools. Keeping this
/// picker native avoids asking people to discover or paste opaque filesystem
/// paths just to configure an index, create a plugin project, or preview a
/// batch rename. It deliberately returns only a canonical folder path after a
/// direct user choice; the plugin bridge uses its stricter opaque grants.
#[tauri::command]
pub fn select_directory(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Option<String>, String> {
    select_directory_with_native_dialog(&app, &state.host, "Choose an iHub folder")
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

/// Captures exactly one requested monitor as a bounded PNG payload. The host
/// UI may call this after a direct click; it is intentionally not made
/// available to plugin bridges, timers, or background services.
#[tauri::command]
pub async fn capture_native_screenshot(
    request: Option<crate::native_screenshot::NativeScreenshotRequest>,
) -> Result<crate::native_screenshot::NativeScreenshot, String> {
    let request = request.unwrap_or_default();
    tauri::async_runtime::spawn_blocking(move || {
        crate::native_screenshot::capture_native_screenshot(request)
    })
    .await
    .map_err(|error| format!("Native screenshot task failed: {error}"))?
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
    .map_err(|error| format!("Plugin command task failed: {error}"))?;
    drop(native_command_lease);
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
    get_autostart_status(app)
}

#[tauri::command]
pub fn set_launcher_hotkey(
    app: AppHandle,
    state: State<'_, AppState>,
    accelerator: String,
) -> Result<LauncherHotkeyStatus, String> {
    let accelerator = normalize_launcher_hotkey(&accelerator)?;
    replace_launcher_hotkey(&app, &state, accelerator.clone(), Some(accelerator))
}

#[tauri::command]
pub fn reset_launcher_hotkey(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<LauncherHotkeyStatus, String> {
    replace_launcher_hotkey(&app, &state, LAUNCHER_PRIMARY_HOTKEY.to_owned(), None)
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
    let path = state
        .clipboard_history
        .revalidated_file_entry_path(&id, file_index)?;
    tauri::async_runtime::spawn_blocking(move || open_path_in_system(&path))
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

/// Reads only native clipboard file-list metadata after the user explicitly
/// pastes a file payload into the launcher. Text and image clipboard contents
/// stay in the renderer's standard paste flow; no background clipboard scan
/// is introduced by this command.
#[tauri::command]
pub fn read_clipboard_files() -> Result<Vec<ClipboardFile>, String> {
    let paths = crate::clipboard_access::with_clipboard(|clipboard| clipboard.get().file_list())
        .map_err(|error| format!("The clipboard does not contain a readable file list: {error}"))?;
    Ok(clipboard_files_from_paths(paths))
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

fn clipboard_files_from_paths(paths: impl IntoIterator<Item = PathBuf>) -> Vec<ClipboardFile> {
    paths
        .into_iter()
        .take(MAX_CLIPBOARD_FILE_ITEMS)
        .filter_map(|path| {
            let path = path.canonicalize().ok()?;
            let metadata = fs::metadata(&path).ok()?;
            let kind = if metadata.is_dir() {
                "folder"
            } else if metadata.is_file() {
                "file"
            } else {
                return None;
            };
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .filter(|name| !name.trim().is_empty())?;
            Some(ClipboardFile {
                path: path.to_string_lossy().into_owned(),
                name,
                kind: kind.to_owned(),
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
    request: PluginHostCall,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    let request = request.request;
    if !is_plugin_id(&request.plugin_id) {
        return Err("Invalid plugin ID.".to_owned());
    }
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
        let native_result = tauri::async_runtime::spawn_blocking(move || {
            plugins.run_command(&plugin_id, &command_id, Some(input))
        })
        .await
        .map_err(|error| format!("Native plugin command task failed: {error}"))?;
        // Do not retain the reservation during serialization or response
        // delivery. The worker process has already exited (or errored) here.
        drop(native_command_lease);
        return native_result.and_then(|result| {
            serde_json::to_value(result)
                .map_err(|error| format!("Could not encode native plugin command result: {error}"))
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
        "lifecycle.ready" => Ok(json!({ "ok": true })),
        "lifecycle.dispose" => {
            clear_plugin_runtime_state(&state.host, &request.plugin_id);
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
                issue_filesystem_grant(&state.host, &request.plugin_id, directory.clone());
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
            let directory = directory_for_grant(&state.host, &request.plugin_id, grant_id)?;
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
            let preview = take_plugin_batch_rename_preview(
                &state.host,
                &request.plugin_id,
                grant_id,
                preview_id,
            )?;
            let result =
                crate::builtin_tools::apply_batch_rename(preview.directory, preview.items)?;
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
        "clipboard.writeText" | "clipboard.write" => {
            let value = required_string(&request.params, "value")?;
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
        // A plugin's executable process surface is its declared backend
        // worker. `process.spawn` is intentionally not exposed until iHub has
        // a real allow-list executor rather than an acknowledgement-only API.
        "notifications.show" | "log" => {
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

fn ensure_plugin_host_request_is_allowed(
    request: &PluginHostRequest,
    state: &AppState,
) -> Result<(), String> {
    state.plugins.ensure_plugin_enabled(&request.plugin_id)?;
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
        if let Err(error) = app.emit(
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
    if let Err(error) = app.emit(
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

fn issue_filesystem_grant(host: &PluginHostState, plugin_id: &str, directory: String) -> String {
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
            issued_at: Instant::now(),
        },
    );
    trim_oldest_records(&mut grants, MAX_FILESYSTEM_GRANTS, |grant| grant.issued_at);
    grant_id
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

fn directory_for_grant(
    host: &PluginHostState,
    plugin_id: &str,
    grant_id: &str,
) -> Result<String, String> {
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
    Ok(grant.directory.clone())
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
    let parent_directory = directory_for_grant(host, requesting_plugin_id, grant_id)?;
    create_plugin_project_template(&parent_directory, plugin_id)
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

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_launcher(app);
        }))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
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
                api.prevent_close();
                let _ = window.emit("ihub://hide-search", json!({}));
                let _ = window.hide();
            }
            WindowEvent::Focused(true) => {
                if let Some(state) = window.try_state::<AppState>() {
                    state.launcher_focus.note_focus();
                }
            }
            WindowEvent::Focused(false) => {
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

            let state = AppState::new(app.path().app_data_dir()?);
            state.index.start_change_watcher();
            state.index.rebuild_default_roots();
            let clipboard_history = state.clipboard_history.clone();
            app.manage(state);
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
            if !launched_from_autostart() {
                show_launcher(app.handle());
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_index_status,
            search_entries,
            index_default_roots,
            set_index_roots,
            get_default_roots,
            open_path,
            list_launcher_shortcuts,
            pin_launcher_shortcut_from_search,
            open_launcher_shortcut,
            unpin_launcher_shortcut,
            list_plugins,
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
            capture_native_screenshot,
            crate::builtin_tools::format_json,
            crate::builtin_tools::query_json,
            crate::builtin_tools::preview_batch_rename,
            crate::builtin_tools::apply_batch_rename,
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
            run_plugin_command,
            get_autostart_status,
            set_autostart,
            set_launcher_hotkey,
            reset_launcher_hotkey,
            get_app_health,
            center_launcher_window,
            plugin_host_call,
            invoke_plugin_frontend_command,
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
                    eprintln!(
                        "iHub could not activate preferred launcher hotkey {preferred}; using {} as a recovery binding. Tray menu \"Show iHub\" remains available.",
                        candidate.accelerator
                    );
                } else {
                    eprintln!(
                        "iHub could not activate {LAUNCHER_PRIMARY_HOTKEY}; using {} as a recovery binding. Tray menu \"Show iHub\" remains available.",
                        candidate.accelerator
                    );
                }
                return LauncherHotkeyStatus::fallback_for(
                    candidate.accelerator,
                    preferred_accelerator,
                );
            }
            Err(error) => {
                eprintln!(
                    "iHub could not register launcher hotkey {} {error}",
                    candidate.accelerator
                );
            }
        }
    }

    eprintln!(
        "iHub could not register a launcher hotkey. Tray menu \"Show iHub\" remains available."
    );
    LauncherHotkeyStatus::unavailable_for(preferred_accelerator)
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
            "show" => show_launcher(app),
            "reindex" => {
                app.state::<AppState>().index.rebuild_default_roots();
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    Ok(())
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
    // A hidden window retains the monitor it was last dragged onto. That is
    // the natural target for the next launch; a disconnected monitor falls
    // back to the primary display. No drag position or window geometry is
    // persisted, so every visible session starts centered again.
    let monitor = window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        // Without any display, moving/resizing is neither useful nor safe.
        // Leave the existing native geometry untouched until a display is
        // available rather than guessing an off-screen coordinate.
        eprintln!("iHub could not find a display for the launcher reveal.");
        return;
    };

    let work_area = monitor.work_area();
    let Some(layout) = (LauncherWorkArea {
        position: work_area.position,
        size: work_area.size,
    })
    .reveal_layout(monitor.scale_factor()) else {
        eprintln!("iHub found a display with no usable launcher work area.");
        return;
    };

    if let Err(error) = window.set_size(layout.size) {
        eprintln!("iHub could not fit the launcher into the display work area: {error}");
    }
    if let Err(error) = window.set_position(layout.position) {
        eprintln!("iHub could not center the launcher in the display work area: {error}");
    }
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
        sync::mpsc,
        time::{Duration, Instant},
    };

    use serde_json::json;
    use tauri::{PhysicalPosition, PhysicalSize};

    use crate::clipboard_history::ClipboardHistory;
    use crate::models::{LauncherHotkeyRegistration, LauncherHotkeyStatus};

    use super::{
        attach_plugin_launcher_context_transfer, build_plugin_launcher_context_payload,
        canonical_selected_file, clear_plugin_runtime_state, clear_plugin_session_secrets,
        clipboard_files_from_paths, clipboard_image_from_rgba, complete_plugin_search,
        create_plugin_project_for_grant, cursor_color_approval_id, directory_for_grant,
        get_plugin_session_secret, issue_file_grant, issue_filesystem_grant,
        issue_plugin_launcher_context_transfer, launcher_visibility_action,
        native_plugin_command_input, normalize_plugin_search_results, normalized_host_target,
        optional_u32, optional_u8, plugin_clipboard_history_snapshot,
        revoke_plugin_launcher_context_transfer, set_plugin_session_secret,
        startup_launcher_hotkey_candidates, take_file_grant, take_plugin_batch_rename_preview,
        take_plugin_launcher_context_transfer, CaptureFocusLease, CursorColorApproval,
        LauncherFocusGate, LauncherHotkeyToggleGate, LauncherInvocationSource,
        LauncherVisibilityAction, LauncherVisibilitySnapshot, LauncherWorkArea, NativeDialogGuard,
        PendingPluginSearch, PluginBatchRenamePreview, PluginCursorColor, PluginHostRequest,
        PluginHostState, PluginLauncherContextFileRequest, PluginLauncherContextImageRequest,
        PluginLauncherContextRequest, LAUNCHER_CONTEXT_TTL, LAUNCHER_FALLBACK_HOTKEY,
        LAUNCHER_HOTKEY_TOGGLE_DEBOUNCE, LAUNCHER_INITIAL_BLUR_GRACE, LAUNCHER_PRIMARY_HOTKEY,
        MAX_CAPTURE_FOCUS_LEASES, MAX_PLUGIN_CLIPBOARD_HISTORY_ITEMS,
        MAX_PLUGIN_SEARCH_PAYLOAD_BYTES,
    };

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
    fn launcher_reveal_layout_keeps_the_spotlight_design_size_when_it_fits() {
        let layout = LauncherWorkArea {
            position: PhysicalPosition::new(0, 0),
            size: PhysicalSize::new(1_920, 1_040),
        }
        .reveal_layout(1.0)
        .expect("a non-empty work area should have a launcher layout");

        assert_eq!(layout.size, PhysicalSize::new(1_200, 756));
        assert_eq!(layout.position, PhysicalPosition::new(360, 142));
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
        assert_eq!(reopened.position, PhysicalPosition::new(-1_240, 174));
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
        let host = PluginHostState::default();
        let owner = "ihub-plugin-owner";
        let grant_id = issue_filesystem_grant(&host, owner, "C:/safe-folder".to_owned());

        assert_eq!(
            directory_for_grant(&host, owner, &grant_id).expect("owner grant"),
            "C:/safe-folder"
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
                        directory: "C:/safe-folder".to_owned(),
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
        let parent_directory = parent.to_string_lossy().into_owned();
        let host = PluginHostState::default();
        let owner = "ihub-plugin-developer-tools";
        let grant_id = issue_filesystem_grant(&host, owner, parent_directory);

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
        let host = PluginHostState::default();
        let owner = "ihub-plugin-owner";
        let grant_id = issue_filesystem_grant(&host, owner, "C:/safe-folder".to_owned());
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
                        directory: "C:/safe-folder".to_owned(),
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

        let entries = clipboard_files_from_paths(vec![
            file.clone(),
            directory.clone(),
            directory.join("already-gone.txt"),
        ]);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "example.txt");
        assert_eq!(entries[0].kind, "file");
        assert_eq!(entries[1].kind, "folder");
        assert!(entries[0].path.ends_with("example.txt"));

        fs::remove_dir_all(directory).expect("cleanup clipboard fixture directory");
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
