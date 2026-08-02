use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStatus {
    pub indexed_files: usize,
    /// Text documents held only in the process-local, bounded content index.
    /// File names and paths remain independently searchable while this work
    /// is running, and file bodies are never written into the path snapshot.
    pub content_indexed_files: usize,
    pub content_indexed_bytes: usize,
    /// `idle`, `indexing`, `ready`, or `stale` for the opt-in `content:`
    /// query path. Kept separate from the path scanner's phase.
    pub content_status: String,
    pub roots: Vec<String>,
    pub phase: String,
    pub last_indexed_at: Option<String>,
    /// Native filesystem watcher health for the explicitly authorized roots.
    /// This is separate from the scan phase: a ready snapshot can remain
    /// searchable even if continuous refresh is temporarily unavailable.
    pub watch_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watch_message: Option<String>,
    /// Windows-only NTFS USN health. `available` means the volume serial and
    /// live journal watermark were verified; it can also describe P1d's
    /// zero-change snapshot reuse and never claims journal-delta replay.
    pub usn_status: String,
    /// Number of currently authorised NTFS volumes whose live USN state was
    /// successfully queried. This is volume metadata only, never a file count.
    pub usn_eligible_volumes: usize,
    /// Number of persisted checkpoints that remained continuous with the live
    /// journal during the latest probe. A fresh baseline is intentionally 0.
    pub usn_checkpointed_volumes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usn_message: Option<String>,
    /// Windows-only P1c MFT initialization / P1d zero-change reuse health.
    /// MFT enumeration can cover only an explicitly authorised drive root
    /// (for example `C:\\`); a narrower root remains on the scoped scanner.
    pub mft_status: String,
    /// Number of raw MFT metadata records read during the latest complete
    /// direct-drive initialization. This is not a file count and is never
    /// persisted outside the normal path snapshot.
    pub mft_enumerated_records: usize,
    /// Number of USN V2 records considered only to close the bounded window
    /// during this MFT initialization. This is not a cross-restart replay
    /// checkpoint and is never persisted with file/parent IDs or raw records.
    pub mft_replayed_usn_records: usize,
    /// Number of safe path projections accepted from those MFT records for
    /// the current in-memory index. MFT file IDs, parent IDs and USN records
    /// are discarded before the index snapshot is written.
    pub mft_indexed_paths: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mft_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub id: String,
    pub path: String,
    pub name: String,
    pub kind: String,
    /// The native index has determined that this real local result may be
    /// pinned through the host-owned launcher-shortcut store. The renderer
    /// must still send only this opaque current result ID back to the host.
    pub pin_eligible: bool,
    /// Present only when this exact host-indexed source is already pinned.
    /// It is a launcher-store UUID, never a path or source lookup key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pinned_shortcut_id: Option<String>,
    pub score: f64,
    pub metadata: String,
    pub modified_at: Option<String>,
    /// Index-time file length. Directories and values outside JavaScript's
    /// exact integer range are intentionally omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

/// A filesystem object explicitly copied by the user and read from the
/// platform clipboard. It is intentionally much narrower than a search
/// result: the renderer can offer it as an action, but never receives any
/// untrusted clipboard format or arbitrary binary payload.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardFile {
    pub path: String,
    pub name: String,
    pub kind: String,
    /// Short-lived, host-owned authorization for opening this exact live
    /// filesystem object. It is not a path and cannot be forged by a WebView.
    pub open_id: String,
}

/// A bounded bitmap explicitly pasted by the user. The data URL exists only
/// long enough to cross the local Tauri IPC boundary; the host never writes
/// this clipboard payload to disk or to clipboard history.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardImage {
    pub data_url: String,
    pub name: String,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginCommandInfo {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    /// Host-decoded, bounded PNG artwork. A manifest path is never exposed to
    /// the renderer, and unsupported or unsafe artwork rejects the plugin.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_src: Option<String>,
    /// The host chooses the activation surface from this manifest-declared
    /// target. Frontend commands open the constrained iframe; native commands
    /// may start the plugin's declared worker after user confirmation.
    pub execution: String,
    /// Bounded launcher aliases declared in the signed/linked manifest.
    /// Runtime command registration cannot add or replace them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// Host-validated public uTools text matcher declarations. Ordinary iHub
    /// commands project an empty list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub utools_text_matchers: Vec<UtoolsTextMatcherInfo>,
    /// Canonical manifest-declared global accelerator, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shortcut: Option<String>,
    /// `registered`, `blocked`, `unavailable`, or `inactive`. This is
    /// projected by the resident shortcut registry, never supplied by the
    /// plugin package.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shortcut_registration: Option<String>,
    /// A bounded user-facing reason when the OS binding was not activated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shortcut_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UtoolsTextMatcherInfo {
    pub matcher_type: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub flags: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_length: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_length: Option<usize>,
}

/// A plugin-level declarative mapping from a global accelerator to either a
/// declared command or a bounded launcher keyword. The native host owns
/// validation and registration; plugin JavaScript never receives a shortcut
/// handle.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginGlobalShortcutInfo {
    pub id: String,
    pub shortcut: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keyword: Option<String>,
    /// `registered`, `blocked`, `unavailable`, or `inactive`.
    pub registration: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A declarative search provider advertised by an installed plugin manifest.
/// The host still waits for the plugin iframe to register this provider before
/// it sends a query, so a manifest alone never executes plugin code.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginSearchProviderInfo {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
}

/// A bounded result returned from an iframe-backed plugin search provider.
/// Values remain JSON-shaped because they cross the existing plugin bridge;
/// no plugin JavaScript object or Tauri API is ever exposed to the launcher.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginSearchResult {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    pub score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginSearchResponse {
    pub request_id: String,
    pub plugin_id: String,
    pub provider_id: String,
    pub results: Vec<PluginSearchResult>,
}

/// A SHA-256 digest for one executable plugin asset. Paths are normalized
/// relative to the package containing `plugin.json`, never to the host's
/// plugin directory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginArtifactDigest {
    pub path: String,
    pub sha256: String,
}

/// Runtime-code integrity captured alongside a Git source lock. A frontend
/// bundle is represented by every file that its dedicated asset directory can
/// serve; artwork and native binaries are represented by manifest-declared
/// files.
/// Keeping this host-owned data out of `plugin.json` prevents a fetched
/// snapshot from declaring its own expected hashes after the user approved a
/// commit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginSnapshotIntegrity {
    pub algorithm: String,
    pub manifest_sha256: String,
    #[serde(default)]
    pub frontend_assets: Vec<PluginArtifactDigest>,
    /// `None` identifies a lock created before standalone manifest artwork was
    /// integrity-covered. New locks always store `Some`, including an empty
    /// list, so artwork bytes cannot change without failing verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artwork_assets: Option<Vec<PluginArtifactDigest>>,
    #[serde(default)]
    pub native_binaries: Vec<PluginArtifactDigest>,
}

/// Immutable provenance captured when a plugin snapshot is installed from a
/// remote Git repository. This is intentionally separate from the manifest:
/// it records what the user asked to install and the commit that was actually
/// checked out, plus the runtime files whose contents were approved during
/// import.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PluginSourceLock {
    pub source: String,
    pub requested_ref: String,
    pub resolved_commit: String,
    pub installed_at: String,
    /// Locks written before integrity verification was introduced omit this
    /// field and remain readable. New Git imports always write it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integrity: Option<PluginSnapshotIntegrity>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginLauncherContextPermissionsInfo {
    /// Read-only manifest metadata for the trusted parent renderer. It is
    /// used only to avoid offering impossible handoff actions; issuance and
    /// consumption still repeat the exact capability checks in Rust.
    pub text: bool,
    pub files: bool,
    pub image: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    /// Host-decoded, normalized plugin logo. No package path or source bytes
    /// cross the IPC boundary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_src: Option<String>,
    pub source: Option<String>,
    pub commit: Option<String>,
    pub installed_at: Option<String>,
    /// Provenance for a Git-installed snapshot. Local development links do
    /// not have a remote source lock.
    pub source_lock: Option<PluginSourceLock>,
    /// True while an explicit local development-link record owns this plugin
    /// ID. A stale link can safely fall back to a verified managed snapshot,
    /// but remains true so the user can still find and unlink the broken
    /// source reference.
    pub is_development_link: bool,
    /// `active` when the development source resolves and validates, or
    /// `stale` when it was deleted, moved, or no longer contains the expected
    /// plugin. This is absent for plugins without a local-link record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_link_status: Option<String>,
    /// User-facing diagnostic for a stale development link. It never contains
    /// plugin file bodies or other renderer capabilities.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_link_error: Option<String>,
    /// True only when a stale local link is currently executing the same ID's
    /// validated host-managed snapshot.
    pub uses_managed_snapshot_fallback: bool,
    /// Last canonical source directory recorded for an explicit local
    /// development link. A stale path remains visible so it can be diagnosed
    /// and unlinked; this is absent for plugins without a link record.
    pub local_path: Option<String>,
    pub frontend_entry: Option<String>,
    pub enabled: bool,
    pub has_native_worker: bool,
    /// The plugin's declared release channel, when it opts into the update
    /// metadata in its manifest. This is metadata only: iHub never treats a
    /// manifest declaration as permission to silently replace native code.
    pub update_channel: Option<String>,
    /// Whether the plugin opted into iHub's bounded automatic *availability*
    /// checks. Applying a Git snapshot remains an explicit user action.
    pub auto_update: bool,
    pub command_count: usize,
    /// Number of manifest-declared uTools MCP tools. Handlers remain
    /// unavailable until the current sandbox runtime registers the exact
    /// declaration name.
    #[serde(default)]
    pub tool_count: usize,
    pub commands: Vec<PluginCommandInfo>,
    /// Plugin-level shortcut-to-command/keyword mappings declared in the
    /// manifest. Command-local shorthand remains on `commands[].shortcut`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub global_shortcuts: Vec<PluginGlobalShortcutInfo>,
    /// Manifest-declared providers. These are metadata only until the
    /// plugin's constrained iframe bridge registers the same provider.
    pub search_providers: Vec<PluginSearchProviderInfo>,
    /// A deliberately narrow projection of `permissions.launcherContext`.
    /// This does not provide clipboard, filesystem, image, or native-worker
    /// capability to a plugin or to the renderer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launcher_context: Option<PluginLauncherContextPermissionsInfo>,
}

/// A first-party plugin project present in the trusted source checkout. It may
/// override its immutable registry package during development. The renderer
/// receives only this validated allowlist projection; it cannot choose a path
/// for the one-click workspace-link command.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OfficialWorkspacePluginProject {
    pub id: String,
    pub name: String,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
    pub detail: String,
}

/// A read-only comparison between an installed Git snapshot and the commit
/// currently resolved from its saved source/ref. Calling the check command
/// never changes the installed files or source lock.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginUpdateCheck {
    pub plugin_id: String,
    pub source: String,
    pub requested_ref: String,
    pub current_commit: String,
    pub latest_commit: String,
    pub update_available: bool,
    /// `up-to-date` or `update-available`; kept as a string for a forward
    /// compatible desktop API without serializing Rust enum names.
    pub status: String,
    pub message: String,
}

/// A plugin that was deliberately left out of the periodic automatic
/// availability check. Skips are returned to the renderer instead of being
/// hidden so it can distinguish a trustworthy, up-to-date package from one
/// that needs a manual check or user review.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginAutomaticUpdateSkip {
    pub plugin_id: String,
    pub reason: String,
}

/// Best-effort, read-only periodic update discovery for installed plugins.
/// The report cannot change an installed snapshot, its source lock, or the
/// lifecycle state. It exists solely to surface reviewed Git revisions in the
/// plugin center.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginAutomaticUpdateReport {
    pub checked_at: String,
    pub checks: Vec<PluginUpdateCheck>,
    pub skipped: Vec<PluginAutomaticUpdateSkip>,
}

/// Result of an explicit Git plugin update. `updated` is false when the saved
/// ref still resolves to the installed commit, in which case the source lock
/// is intentionally left untouched.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginUpdateResult {
    pub plugin: PluginInfo,
    pub updated: bool,
    pub previous_commit: String,
    pub current_commit: String,
}

/// Returned after the user changes the persisted lifecycle state of one
/// installed or explicitly linked plugin. Keeping the full plugin projection
/// here lets the renderer update its launcher and marketplace state from the
/// host's canonical record instead of guessing whether a toggle succeeded.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginLifecycleUpdate {
    pub plugin: PluginInfo,
    pub enabled: bool,
}

/// Evidence that a managed Git snapshot was removed. Local development links
/// are intentionally excluded from this operation: their source trees belong
/// to the developer and are never removed by iHub.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginUninstallResult {
    pub plugin_id: String,
    pub plugin_name: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginCommandResult {
    pub plugin_id: String,
    pub command_id: String,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub output: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginProjectCreated {
    pub project_path: String,
    pub plugin_id: String,
    pub next_steps: Vec<String>,
    /// Present only when the first-party host has issued a short-lived open
    /// authorization for the newly created project directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutostartStatus {
    pub enabled: bool,
    pub supported: bool,
}

/// The active launcher hotkey state owned by the native host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LauncherHotkeyRegistration {
    /// A validated preference registered successfully.
    Configured,
    /// `Alt+Space` (`Option+Space` on macOS) registered successfully.
    Primary,
    /// A preferred or primary binding was unavailable, so a safe recovery
    /// binding is active instead.
    Fallback,
    /// No binding could be registered. The resident tray's Show action remains
    /// the recovery path.
    Unavailable,
}

/// A renderer-safe description of the global launcher hotkey registration.
/// It intentionally contains no OS handle or native registration error. The
/// renderer may request a validated replacement through one narrow command,
/// while registration and persistence remain native-host owned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LauncherHotkeyStatus {
    pub registration: LauncherHotkeyRegistration,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accelerator: Option<String>,
    /// Present when a saved preference could not become the active binding and
    /// the host retained a known recovery accelerator instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred_accelerator: Option<String>,
    /// The tray menu always provides an explicit Show iHub recovery action,
    /// including when both global registrations are unavailable.
    pub tray_show_available: bool,
}

impl LauncherHotkeyStatus {
    pub fn configured(accelerator: impl Into<String>) -> Self {
        let accelerator = accelerator.into();
        Self {
            registration: LauncherHotkeyRegistration::Configured,
            accelerator: Some(accelerator.clone()),
            preferred_accelerator: Some(accelerator),
            tray_show_available: true,
        }
    }

    pub fn primary() -> Self {
        Self {
            registration: LauncherHotkeyRegistration::Primary,
            accelerator: Some("Alt+Space".to_owned()),
            preferred_accelerator: None,
            tray_show_available: true,
        }
    }

    pub fn fallback_for(
        accelerator: impl Into<String>,
        preferred_accelerator: Option<String>,
    ) -> Self {
        Self {
            registration: LauncherHotkeyRegistration::Fallback,
            accelerator: Some(accelerator.into()),
            preferred_accelerator,
            tray_show_available: true,
        }
    }

    pub fn unavailable() -> Self {
        Self::unavailable_for(None)
    }

    pub fn unavailable_for(preferred_accelerator: Option<String>) -> Self {
        Self {
            registration: LauncherHotkeyRegistration::Unavailable,
            accelerator: None,
            preferred_accelerator,
            tray_show_available: true,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppHealth {
    pub version: String,
    pub platform: String,
    /// Canonical release target, for example `windows-x86_64`.
    pub host_target: String,
    pub started_at: String,
    pub autostart: bool,
    /// Active native-host-owned launcher hotkey state.
    pub launcher_hotkey: LauncherHotkeyStatus,
    pub index: IndexStatus,
    pub plugin_count: usize,
}
