use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    ffi::{OsStr, OsString},
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Arc, Mutex, TryLockError},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use url::Url;

use crate::background_process::background_command;
use crate::host_log;
use crate::launcher_hotkey::normalize_plugin_hotkey;
use crate::models::{
    OfficialWorkspacePluginProject, PluginArtifactDigest, PluginAutomaticUpdateReport,
    PluginAutomaticUpdateSkip, PluginCommandInfo, PluginCommandResult, PluginGlobalShortcutInfo,
    PluginInfo, PluginLauncherContextPermissionsInfo, PluginLifecycleUpdate,
    PluginSearchProviderInfo, PluginSnapshotIntegrity, PluginSourceLock, PluginUninstallResult,
    PluginUpdateCheck, PluginUpdateResult,
};
use crate::plugin_artwork::{load_plugin_artwork, validate_artwork_relative_path, PluginArtwork};

const MANIFEST_NAMES: [&str; 2] = ["ihub.plugin.json", "plugin.json"];
/// The immutable provenance captured for every newly imported Git snapshot.
/// Older installations used `.ihub-source.json`; they remain readable so an
/// application upgrade never makes existing plugins disappear from the list.
const SOURCE_LOCK: &str = ".ihub-source.lock.json";
const LEGACY_SOURCE_RECORD: &str = ".ihub-source.json";
const LOCAL_LINKS_RECORD: &str = ".ihub-local-links.json";
/// Small host-owned state that survives restart without changing a plugin's
/// manifest or source snapshot. It is deliberately kept beside iHub's local
/// link record rather than inside an imported repository, so a Git update can
/// never overwrite a user's enabled/disabled choice.
const LIFECYCLE_RECORD: &str = ".ihub-plugin-lifecycle.json";
const LIFECYCLE_SCHEMA_VERSION: u32 = 1;
const MAX_CAPTURED_OUTPUT_BYTES: usize = 1_000_000;
/// Plugin input may be mirrored into declared command arguments, whose OS
/// command-line limits are much lower than the pipe/output bounds. Keep the
/// serialized JSON small and make large data flow through user-selected files
/// or a worker-owned storage format instead.
const MAX_PLUGIN_COMMAND_INPUT_BYTES: usize = 16 * 1024;
const MAX_GIT_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const PLUGIN_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
const MIN_PLUGIN_COMMAND_TIMEOUT_MS: u64 = 1_000;
const MAX_PLUGIN_COMMAND_TIMEOUT_MS: u64 = 30 * 60 * 1_000;
const PLUGIN_COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(20);
/// Git network and checkout work is intentionally bounded. A refresh is a
/// user-visible control-plane action, not a background task that may hold the
/// plugin installation lock indefinitely.
const PLUGIN_GIT_TIMEOUT: Duration = Duration::from_secs(30);
const PLUGIN_GIT_POLL_INTERVAL: Duration = Duration::from_millis(20);
/// Background discovery must not leave the plugin center awaiting a long
/// sequence of slow remotes. Manual checks retain the longer 30-second
/// control-plane timeout above.
const AUTOMATIC_UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(4);
const AUTOMATIC_UPDATE_CHECK_TIME_BUDGET: Duration = Duration::from_secs(12);
/// Keep a periodic pass bounded even if a user has a very large catalog of
/// installed official plugins. Remaining packages remain available for a
/// manual per-plugin check in the UI.
const MAX_AUTOMATIC_UPDATE_CHECKS_PER_PASS: usize = 24;
/// Automatic discovery is intentionally limited to the official publisher's
/// canonical HTTPS repositories. Community imports remain fully supported,
/// but their Git refs are checked only after the user asks for it.
const OFFICIAL_GITHUB_AUTO_UPDATE_PREFIX: &str = "https://github.com/neko233-com/";
/// Launcher search is deliberately a small extension point. A manifest cannot
/// use a huge provider list to turn one installed iframe into unbounded host
/// registrations.
const MAX_SEARCH_PROVIDERS_PER_PLUGIN: usize = 32;
/// Settings live in a host-owned namespace. Keep the declaration count in
/// step with the durable-store namespace cap so a manifest cannot make a
/// session-only secret setting map unbounded.
const MAX_SETTINGS_PER_PLUGIN: usize = 128;
/// Global input hooks stay native-host owned and deliberately small. A single
/// package may combine command-local shortcuts with plugin-level
/// shortcut-to-command/keyword mappings up to this total.
const MAX_GLOBAL_SHORTCUTS_PER_PLUGIN: usize = 16;
const MAX_SHORTCUT_KEYWORD_CHARS: usize = 64;
const MAX_PERMISSION_LIST_ITEMS: usize = 64;
const MAX_PERMISSION_VALUE_CHARS: usize = 512;
const MAX_DEVELOPMENT_LAUNCHER_MARKER_BYTES: u64 = 64 * 1024;
/// Commands are projected into one Tauri IPC response. Keeping this bounded
/// prevents a manifest from multiplying even one reused artwork data URL into
/// an unbounded renderer payload.
const MAX_COMMANDS_PER_PLUGIN: usize = 64;
/// A plugin may reuse one image for every command, but distinct decoded
/// artwork remains bounded so listing plugins cannot create an oversized IPC
/// response or unbounded decode workload.
const MAX_ARTWORK_FILES_PER_PLUGIN: usize = 32;
/// Command artwork is optional identity metadata. Once the serialized image
/// budget is exhausted, later commands safely inherit the plugin icon or the
/// renderer's neutral fallback instead of expanding the IPC response.
const MAX_PROJECTED_ARTWORK_DATA_URL_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy)]
struct OfficialWorkspacePluginSpec {
    id: &'static str,
    name: &'static str,
    directory: &'static str,
}

/// Hard-coded first-party projects that a locally built development host may
/// expose through an explicit one-click source link. Every entry is also
/// distributed through the immutable registry; this allowlist gives a trusted
/// development checkout priority without weakening the ordinary Git fallback.
/// The renderer can select only an ID from this list; it never supplies the
/// corresponding filesystem path.
const OFFICIAL_WORKSPACE_PLUGIN_SPECS: [OfficialWorkspacePluginSpec; 18] = [
    OfficialWorkspacePluginSpec {
        id: "ihub-plugin-archive-tools",
        name: "Archive Tools",
        directory: "ihub-plugin-archive-tools",
    },
    OfficialWorkspacePluginSpec {
        id: "ihub-plugin-base-converter",
        name: "Base Converter",
        directory: "ihub-plugin-base-converter",
    },
    OfficialWorkspacePluginSpec {
        id: "ihub-plugin-batch-rename",
        name: "Batch Rename",
        directory: "ihub-plugin-batch-rename",
    },
    OfficialWorkspacePluginSpec {
        id: "ihub-plugin-clipboard",
        name: "Clipboard History",
        directory: "ihub-plugin-clipboard",
    },
    OfficialWorkspacePluginSpec {
        id: "ihub-plugin-colorpick",
        name: "Color Picker",
        directory: "ihub-plugin-colorpick",
    },
    OfficialWorkspacePluginSpec {
        id: "ihub-plugin-developer-tools",
        name: "Plugin Developer Tools",
        directory: "ihub-plugin-developer-tools",
    },
    OfficialWorkspacePluginSpec {
        id: "ihub-plugin-image-tools",
        name: "Image Tools",
        directory: "ihub-plugin-image-tools",
    },
    OfficialWorkspacePluginSpec {
        id: "ihub-plugin-json-tools",
        name: "JSON Tools",
        directory: "ihub-plugin-json-tools",
    },
    OfficialWorkspacePluginSpec {
        id: "ihub-plugin-ocr",
        name: "OCR",
        directory: "ihub-plugin-ocr",
    },
    OfficialWorkspacePluginSpec {
        id: "ihub-plugin-pdf-tools",
        name: "PDF Tools",
        directory: "ihub-plugin-pdf-tools",
    },
    OfficialWorkspacePluginSpec {
        id: "ihub-plugin-qrcode",
        name: "QR Code",
        directory: "ihub-plugin-qrcode",
    },
    OfficialWorkspacePluginSpec {
        id: "ihub-plugin-quick-note",
        name: "Quick Note",
        directory: "ihub-plugin-quick-note",
    },
    OfficialWorkspacePluginSpec {
        id: "ihub-plugin-screen-record",
        name: "Screen Recorder",
        directory: "ihub-plugin-screen-record",
    },
    OfficialWorkspacePluginSpec {
        id: "ihub-plugin-screenshot",
        name: "Screenshot",
        directory: "ihub-plugin-screenshot",
    },
    OfficialWorkspacePluginSpec {
        id: "ihub-plugin-text-tools",
        name: "Text Tools",
        directory: "ihub-plugin-text-tools",
    },
    OfficialWorkspacePluginSpec {
        id: "ihub-plugin-translate",
        name: "Translate",
        directory: "ihub-plugin-translate",
    },
    OfficialWorkspacePluginSpec {
        id: "ihub-plugin-web-actions",
        name: "Web Actions",
        directory: "ihub-plugin-web-actions",
    },
    OfficialWorkspacePluginSpec {
        id: "ihub-plugin-window-manager",
        name: "iHub Window Layout",
        directory: "ihub-plugin-window-manager",
    },
];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DevelopmentLauncherMarker {
    schema_version: u32,
    managed_by: String,
    launcher_revision: u32,
    source_root: String,
}

#[derive(Clone, Debug)]
pub struct PluginManager {
    root: Arc<PathBuf>,
    install_lock: Arc<Mutex<()>>,
    /// Discovery is deliberately single-flight. Opening/re-rendering the
    /// plugin center must not start several overlapping Git probe passes.
    automatic_update_lock: Arc<Mutex<()>>,
}

/// A validated, canonical frontend bundle. The HTTP asset server receives
/// this narrow capability instead of a plugin directory so an iframe can load
/// only sibling build assets next to its declared HTML entry.
#[derive(Clone, Debug)]
pub(crate) struct PluginFrontendAssetBundle {
    pub(crate) plugin_id: String,
    pub(crate) asset_root: PathBuf,
    pub(crate) entry: PathBuf,
    /// Package files explicitly declared as a uTools Electron preload are not
    /// assets in iHub. Retain their canonical paths only so the loopback server
    /// can reject a page attempting to load them as an ordinary script.
    pub(crate) blocked_asset_paths: Vec<PathBuf>,
    /// True only when the validated manifest explicitly declares
    /// `permissions.screenCapture`. Lease issuance further restricts this to
    /// visible surfaces before the renderer can delegate `display-capture`.
    pub(crate) allows_display_capture: bool,
    /// True only when the validated manifest explicitly declares
    /// `permissions.microphone`. Lease issuance further restricts this to
    /// visible surfaces before the renderer can delegate `microphone`.
    pub(crate) allows_microphone: bool,
    /// True only when the validated manifest explicitly declares at least one
    /// external network destination. The asset server uses this as a coarse
    /// CSP gate; destination strings remain review metadata, not CSP sources.
    pub(crate) allows_remote_network: bool,
    /// A host-owned compatibility bootstrap for a public uTools `plugin.json`
    /// package. This is deliberately data rather than a package-provided
    /// preload: the loopback asset server injects only iHub's fixed shim before
    /// the page's own scripts run.
    pub(crate) utools_compat: Option<UtoolsCompatRuntimeConfig>,
    /// Optional, explicitly requested BrowserWindow preload projected as a
    /// sandboxed ordinary script after iHub's fixed IPC shim.
    pub(crate) utools_browser_preload_src: Option<String>,
}

/// The small, serializable part of a uTools feature declaration that the
/// sandboxed iframe needs at runtime. It contains no filesystem path, preload
/// source, Node/Electron API, or user data.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UtoolsCompatRuntimeConfig {
    pub(crate) app_version: String,
    pub(crate) is_development: bool,
    pub(crate) plugin_id: String,
    pub(crate) commands: Vec<UtoolsCompatCommand>,
    pub(crate) native_id: String,
    pub(crate) paths: BTreeMap<String, String>,
    /// Same-plugin idle remote browser windows available for a subsequent
    /// `utools.ubrowser.run(instance.id)` call. The host derives every field
    /// from its registry and native window; package code cannot forge it.
    pub(crate) idle_ubrowsers: Vec<crate::utools_ubrowser::UBrowserInstance>,
    /// Host-owned role for this exact document. BrowserWindow children never
    /// learn or choose this value from their route or package URL.
    pub(crate) window_type: String,
    /// Only the primary surface/runtime document owns plugin registration
    /// lifecycle. Auxiliary BrowserWindow documents must not clear it.
    pub(crate) lifecycle_owner: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UtoolsCompatCommand {
    pub(crate) command_id: String,
    pub(crate) code: String,
    pub(crate) keywords: Vec<String>,
    pub(crate) main_push: bool,
}

/// Fixed provider identity owned by the compatibility host. Imported package
/// code cannot choose a different launcher registration for `onMainPush`.
pub(crate) const UTOOLS_MAIN_PUSH_PROVIDER_ID: &str = "utools-main-push";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum PluginCompatibility {
    #[default]
    Ihub,
    Utools,
}

impl PluginCompatibility {
    fn is_utools(self) -> bool {
        matches!(self, Self::Utools)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginManifest {
    id: String,
    name: String,
    #[serde(default = "default_version")]
    version: String,
    description: Option<String>,
    /// Preferred v1 package artwork declaration.
    icon: Option<String>,
    /// Backward-compatible naming alias. Validation rejects declaring both so
    /// the displayed identity is never order-dependent.
    logo: Option<String>,
    /// Legacy frontend declaration. v1 manifests use `entry.frontend` instead.
    frontend: Option<FrontendDeclaration>,
    entry: Option<EntryDeclaration>,
    backend: Option<BackendDeclaration>,
    contributes: Option<PluginContributions>,
    /// Legacy command declaration. v1 manifests use `contributes.commands`.
    #[serde(default)]
    commands: Vec<PluginCommandDeclaration>,
    /// v1 permissions are deliberately optional here so legacy manifests keep
    /// working. Missing declarations grant no sensitive frontend-host access.
    #[serde(default)]
    permissions: PluginPermissions,
    /// Update metadata is a request for bounded discovery, never a capability
    /// to replace a user's installed native code without review.
    #[serde(default)]
    update: PluginUpdateDeclaration,
    /// Parsed from the public uTools schema only after regular iHub manifest
    /// parsing fails. It is not a JSON field in an iHub package.
    #[serde(skip)]
    compatibility: PluginCompatibility,
    /// One iHub frontend command is projected per uTools feature. The runtime
    /// bootstrap translates that command back into `onPluginEnter` actions.
    #[serde(skip)]
    utools_commands: Vec<UtoolsCompatCommand>,
    #[serde(skip)]
    utools_preload: Option<String>,
}

/// Public, deliberately limited uTools manifest projection. `preload` is
/// decoded only as an ignored field so packages remain importable without ever
/// evaluating Node/Electron code in iHub.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UtoolsManifest {
    main: String,
    logo: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    preload: Option<String>,
    #[serde(default)]
    features: Vec<UtoolsFeature>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UtoolsFeature {
    code: String,
    #[serde(default)]
    explain: Option<String>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    main_push: bool,
    #[serde(default)]
    cmds: Vec<UtoolsFeatureCommand>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum UtoolsFeatureCommand {
    Keyword(String),
    Matcher(UtoolsFeatureMatcher),
}

#[derive(Debug, Deserialize)]
struct UtoolsFeatureMatcher {}

/// The subset of a manifest that can enlarge what a plugin can ask the host
/// to do or introduce executable code. It is intentionally normalized into
/// sets: reordering scopes or target-specific binary entries is not a new
/// declaration, but every semantic addition, removal, or argument/path
/// change is a security review boundary.
#[derive(Debug, Default, PartialEq, Eq)]
struct PluginSecurityDeclaration {
    permissions: BTreeSet<String>,
    native_declarations: BTreeSet<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginUpdateDeclaration {
    channel: Option<String>,
    auto_update: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PluginPermissions {
    filesystem: Option<FilesystemPermissions>,
    network: Option<NetworkPermissions>,
    clipboard: Option<ClipboardPermissions>,
    shell: Option<ShellPermissions>,
    /// Allows a visible plugin surface to receive the browser's
    /// `display-capture` Permissions Policy delegation and hold a short-lived
    /// focus-protection lease while its `getDisplayMedia` picker is open.
    /// Browser consent remains mandatory, and this does not grant a hidden
    /// runtime, native screen pixels, global shortcuts, or microphone access.
    #[serde(default)]
    screen_capture: bool,
    /// Allows only browser microphone Permissions Policy delegation for a
    /// visible plugin surface. It is independent from display capture and
    /// does not bypass browser or operating-system consent.
    #[serde(default)]
    microphone: bool,
    /// Allows a visible plugin surface to request one delayed sample of the
    /// pixel underneath the cursor. This is intentionally separate from
    /// `screenCapture`: it returns no image, cursor coordinates, recording
    /// handle, global shortcut, or background polling capability.
    #[serde(default)]
    cursor_color: bool,
    #[serde(default)]
    global_shortcut: bool,
    #[serde(default)]
    notifications: bool,
    #[serde(default)]
    native_api: bool,
    /// Allows a plugin to perform one of the host's fixed layout operations
    /// on the iHub launcher itself. It cannot enumerate or control other
    /// applications' windows.
    #[serde(default)]
    window_management: bool,
    /// Grants only a host-created, one-shot launcher handoff. It is distinct
    /// from clipboard/filesystem permissions: no declaration here lets a
    /// plugin poll clipboard data, resolve a path, or read image pixels.
    launcher_context: Option<LauncherContextPermissions>,
    process: Option<ProcessPermissions>,
}

/// Fine-grained declarations for a launcher handoff. This remains strict like
/// the outer permission contract: a typo must never look like approval for a
/// new data category.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LauncherContextPermissions {
    #[serde(default)]
    text: bool,
    #[serde(default)]
    files: bool,
    #[serde(default)]
    image: bool,
}

/// A non-empty declaration enables the iframe's coarse external-network CSP
/// gate. Destination strings remain human-review/update-lock metadata for now:
/// they are not yet parsed into an origin-level runtime allowlist. A routine
/// Git refresh still may not silently add, remove, or widen them.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct NetworkPermissions {
    #[serde(default)]
    allow: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FilesystemPermissions {
    #[serde(default)]
    read: Vec<String>,
    #[serde(default)]
    write: Vec<String>,
}

impl FilesystemPermissions {
    fn allows_user_selected_read(&self) -> bool {
        self.read.iter().any(|scope| scope == "user-selected")
    }

    fn allows_user_selected_write(&self) -> bool {
        self.write.iter().any(|scope| scope == "user-selected")
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClipboardPermissions {
    #[serde(default)]
    read: bool,
    #[serde(default)]
    write: bool,
    /// Read-only access to iHub's already opt-in, host-owned text history.
    /// This is deliberately distinct from `read`, which reads the live OS
    /// clipboard, so a manifest must make the broader historical-data access
    /// explicit for review.
    #[serde(default)]
    history: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ShellPermissions {
    #[serde(default)]
    open_path: bool,
    #[serde(default)]
    open_external: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessPermissions {
    #[serde(default)]
    spawn: bool,
    #[serde(default)]
    allow: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct EntryDeclaration {
    frontend: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum FrontendDeclaration {
    Entry(String),
    Detailed { entry: String },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BackendDeclaration {
    /// Legacy single-binary declaration.
    binary: Option<String>,
    protocol: Option<String>,
    #[serde(default)]
    binaries: Vec<PluginBinaryDeclaration>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginBinaryDeclaration {
    target: String,
    path: String,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PluginContributions {
    #[serde(default)]
    commands: Vec<PluginCommandDeclaration>,
    #[serde(default)]
    search_providers: Vec<PluginSearchProviderDeclaration>,
    #[serde(default)]
    settings: Vec<PluginSettingDeclaration>,
    #[serde(default)]
    global_shortcuts: Vec<PluginGlobalShortcutDeclaration>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PluginGlobalShortcutDeclaration {
    id: String,
    shortcut: String,
    command_id: Option<String>,
    keyword: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginSettingDeclaration {
    key: String,
    title: String,
    #[serde(rename = "type")]
    value_type: String,
    #[serde(default)]
    secret: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginSearchProviderDeclaration {
    id: String,
    title: String,
    trigger: Option<String>,
    priority: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginCommandDeclaration {
    id: String,
    #[serde(default)]
    name: Option<String>,
    title: Option<String>,
    description: Option<String>,
    subtitle: Option<String>,
    /// Package-relative artwork. Rust decodes and normalizes it before any
    /// value crosses into the WebView.
    icon: Option<String>,
    /// Bounded static launcher aliases. They are metadata only and cannot be
    /// replaced by a running iframe.
    #[serde(default)]
    keywords: Vec<String>,
    /// Command-local shorthand for one manifest-owned global binding.
    shortcut: Option<String>,
    /// Commands in a plugin that also bundles a native worker can still be
    /// entry points into its UI. Omitted values preserve the v1 behavior:
    /// commands are native when the plugin has a native worker, frontend
    /// otherwise.
    execution: Option<String>,
    binary: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    run: Option<PluginCommandRunDeclaration>,
}

/// Bounded execution policy for one native command. This stays at the command
/// level so a plugin can reserve a longer deadline for a deliberate export or
/// FFmpeg job without making every activation wait that long.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
struct PluginCommandRunDeclaration {
    timeout_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandExecution {
    Frontend,
    Native,
}

impl CommandExecution {
    fn as_str(self) -> &'static str {
        match self {
            Self::Frontend => "frontend",
            Self::Native => "native",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacySourceRecord {
    source: String,
    installed_at: String,
    #[serde(default)]
    commit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitSource {
    remote: String,
    requested_ref: String,
}

#[derive(Debug, Clone)]
struct SourceMetadata {
    source: String,
    resolved_commit: Option<String>,
    installed_at: String,
    lock: Option<PluginSourceLock>,
}

/// Local development links live in iHub's managed directory, but point to an
/// existing source tree. The source tree itself is never copied or modified by
/// the host, so rebuilding it changes what iHub reads on the next load.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocalPluginLinks {
    #[serde(default)]
    links: BTreeMap<String, LocalPluginLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LocalPluginLink {
    canonical_path: String,
    linked_at: String,
    /// Cached display metadata keeps a deleted/moved development checkout
    /// manageable in Plugin Center. It is never used for execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginLifecycleStore {
    #[serde(default = "default_lifecycle_schema_version")]
    schema_version: u32,
    #[serde(default)]
    plugins: BTreeMap<String, PluginLifecycleRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginLifecycleRecord {
    enabled: bool,
    updated_at: String,
}

impl PluginLifecycleStore {
    fn is_enabled(&self, plugin_id: &str) -> bool {
        self.plugins
            .get(plugin_id)
            .map(|record| record.enabled)
            // Existing installations predate lifecycle state and stay active
            // after an upgrade until the user explicitly changes them.
            .unwrap_or(true)
    }

    fn set_enabled(&mut self, plugin_id: &str, enabled: bool) {
        self.plugins.insert(
            plugin_id.to_owned(),
            PluginLifecycleRecord {
                enabled,
                updated_at: Utc::now().to_rfc3339(),
            },
        );
    }

    fn remove(&mut self, plugin_id: &str) {
        self.plugins.remove(plugin_id);
    }
}

impl PluginManager {
    pub fn new() -> Self {
        Self {
            root: Arc::new(default_plugin_root()),
            install_lock: Arc::new(Mutex::new(())),
            automatic_update_lock: Arc::new(Mutex::new(())),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test_root(root: PathBuf) -> Self {
        Self {
            root: Arc::new(root),
            install_lock: Arc::new(Mutex::new(())),
            automatic_update_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn list(&self) -> Vec<PluginInfo> {
        if self.ensure_root().is_err() {
            return Vec::new();
        }
        // Listing should remain useful even if an interrupted/manual edit
        // damaged the small lifecycle record. Execution still fails closed in
        // `ensure_plugin_enabled`; this fallback only lets the user see which
        // plugin needs attention instead of making it disappear from the UI.
        let lifecycle = self.read_lifecycle_store().unwrap_or_else(|error| {
            host_log::warn(
                "plugins",
                format!("Could not read plugin lifecycle state: {error}"),
            );
            PluginLifecycleStore::default()
        });
        let storage_root = self.root.as_ref().canonicalize().ok();
        let mut plugins = fs::read_dir(self.root.as_ref())
            .ok()
            .into_iter()
            .flat_map(|entries| entries.flatten())
            .filter_map(|entry| {
                let entry_path = entry.path();
                if !entry_path.is_dir() || is_internal_dir(&entry_path) {
                    return None;
                }
                let path = entry_path.canonicalize().ok()?;
                let storage_root = storage_root.as_ref()?;
                if !path.is_dir() || ensure_path_within(&path, storage_root, "Plugin root").is_err()
                {
                    return None;
                }
                // Management metadata remains visible when a managed snapshot
                // is damaged, but unverified bytes must never become its
                // launcher identity. A matching (or absent legacy) lock may
                // project artwork; a failed lock gets the same safe fallback
                // glyphs as a plugin without artwork.
                if self.verify_managed_snapshot_integrity(&path).is_ok() {
                    self.read_plugin_info_with_lifecycle(&path, &lifecycle).ok()
                } else {
                    self.read_plugin_info_without_artwork_with_lifecycle(&path, &lifecycle)
                        .ok()
                }
            })
            .collect::<Vec<_>>();

        // A local development link deliberately shadows an installed snapshot
        // with the same manifest ID. A broken link remains visible and
        // manageable; when a valid managed snapshot exists, its runtime
        // projection is marked as the safe fallback until the user unlinks.
        if let Ok(links) = self.read_local_links() {
            for (plugin_id, link) in links.links {
                let plugin = self
                    .read_linked_plugin_info_with_lifecycle(&plugin_id, &link, &lifecycle)
                    .unwrap_or_else(|local_error| {
                        self.read_stale_link_plugin_info_with_lifecycle(
                            &plugin_id,
                            &link,
                            &lifecycle,
                            &local_error,
                        )
                    });
                plugins.retain(|installed| installed.id != plugin_id);
                plugins.push(plugin);
            }
        }
        plugins.sort_unstable_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.id.cmp(&right.id))
        });
        plugins
    }

    pub fn install_from_git(&self, source: &str) -> Result<PluginInfo, String> {
        let source = parse_git_source(source)?;
        self.install_from_remote(source)
    }

    /// Installs a parsed remote source. Keeping parsing outside this method
    /// lets tests exercise the same immutable-checkout path against a local
    /// bare Git repository without broadening the public importer to accept
    /// local filesystem paths.
    fn install_from_remote(&self, source: GitSource) -> Result<PluginInfo, String> {
        let _install_guard = self
            .install_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_root()?;

        // Resolve before any files are installed. The checkout below verifies
        // that it still lands on this exact commit, so a moving branch or tag
        // cannot silently change the installed snapshot mid-import.
        let resolved_commit = resolve_remote_commit(&source.remote, &source.requested_ref)?;

        self.install_resolved_remote_snapshot(&source, &resolved_commit, None)
    }

    /// Re-resolves the exact remote/ref stored in an installed source lock.
    /// This is deliberately read-only: it does not fetch a worktree, update a
    /// lock timestamp, replace files, or start any plugin frontend/backend.
    pub fn check_git_update(&self, plugin_id: &str) -> Result<PluginUpdateCheck, String> {
        self.check_git_update_with_timeout(plugin_id, PLUGIN_GIT_TIMEOUT)
    }

    fn check_git_update_with_timeout(
        &self,
        plugin_id: &str,
        git_timeout: Duration,
    ) -> Result<PluginUpdateCheck, String> {
        // Only the small provenance read is serialized with installation.
        // `ls-remote` can take seconds, and holding the mutation lock while
        // waiting for a remote made a manual check (and, before this split,
        // the periodic pass) able to queue installs indefinitely.
        let source_lock = {
            let _install_guard = self
                .install_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            self.ensure_root()?;
            let (_, source_lock) = self.managed_source_lock(plugin_id)?;
            source_lock
        };
        self.build_git_update_check(plugin_id, source_lock, git_timeout, false)
    }

    /// Converts one already-read immutable source lock into a read-only
    /// update result. Automatic callers use the `official_only` path so the
    /// remote can only be the canonical HTTPS official namespace and Git is
    /// launched with HTTPS as its sole allowed transport.
    fn build_git_update_check(
        &self,
        plugin_id: &str,
        source_lock: PluginSourceLock,
        git_timeout: Duration,
        official_only: bool,
    ) -> Result<PluginUpdateCheck, String> {
        let source = git_source_from_lock(&source_lock)?;
        let latest_commit = if official_only {
            if !is_trusted_official_auto_update_source(&source_lock.source) {
                return Err(
                    "Automatic update discovery only accepts the canonical official HTTPS GitHub namespace."
                        .to_owned(),
                );
            }
            resolve_official_auto_update_commit_with_timeout(
                &source.remote,
                &source.requested_ref,
                git_timeout,
            )?
        } else {
            resolve_remote_commit_with_timeout(&source.remote, &source.requested_ref, git_timeout)?
        };
        let update_available = !latest_commit.eq_ignore_ascii_case(&source_lock.resolved_commit);
        let status = if update_available {
            "update-available"
        } else {
            "up-to-date"
        };
        let message = if update_available {
            format!(
                "A new commit is available for '{}'. Review it, then choose Apply update.",
                source_lock.requested_ref
            )
        } else {
            "The installed snapshot already matches the saved source ref.".to_owned()
        };

        Ok(PluginUpdateCheck {
            plugin_id: plugin_id.to_owned(),
            source: source_lock.source,
            requested_ref: source_lock.requested_ref,
            current_commit: source_lock.resolved_commit,
            latest_commit,
            update_available,
            status: status.to_owned(),
            message,
        })
    }

    /// Performs the safe part of automatic plugin updating: a bounded,
    /// read-only discovery pass for installed official plugins that opted into
    /// the stable channel. This method never checks out a candidate, changes
    /// source locks, reloads plugin code, or starts a native worker. In
    /// particular, a binary plugin can be reported as having an update but
    /// must still go through the existing explicit confirmation flow.
    pub fn check_automatic_updates(&self) -> PluginAutomaticUpdateReport {
        // A renderer effect, manual command invocation, or a stale interval
        // must never make automatic probes queue behind one another. The
        // report still explains why candidates were skipped rather than
        // making a no-op look like an up-to-date result.
        let listed_plugins = self.list();
        let _automatic_guard = match self.automatic_update_lock.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                return automatic_update_skip_report(
                    listed_plugins,
                    "Another automatic plugin update check is already running; this pass was skipped instead of queuing.",
                );
            }
        };

        // Do not wait behind an install/update/lifecycle mutation. We only
        // use this short try-lock to take a coherent `list()` snapshot; all
        // Git network work below happens after it is dropped.
        let plugins = match self.install_lock.try_lock() {
            Ok(guard) => {
                let plugins = self.list();
                drop(guard);
                plugins
            }
            Err(TryLockError::Poisoned(poisoned)) => {
                let guard = poisoned.into_inner();
                let plugins = self.list();
                drop(guard);
                plugins
            }
            Err(TryLockError::WouldBlock) => {
                return automatic_update_skip_report(
                    listed_plugins,
                    "A plugin install, update, or lifecycle change is in progress; automatic discovery was skipped instead of waiting for the install lock.",
                );
            }
        };

        let mut checks = Vec::new();
        let mut skipped = Vec::new();
        let mut attempted = 0_usize;
        let started = Instant::now();

        for plugin in plugins {
            if let Some(reason) = automatic_update_skip_reason(&plugin) {
                skipped.push(PluginAutomaticUpdateSkip {
                    plugin_id: plugin.id,
                    reason,
                });
                continue;
            }
            // Re-read the host-owned lock while holding the mutation lock and
            // verify the installed snapshot before contacting Git. `list()`
            // intentionally remains a best-effort UI projection, so using
            // its lock directly here could make a tampered or just-replaced
            // snapshot trigger a background network probe. We still never
            // wait behind a foreground install/update.
            let verified_source_lock = match self.install_lock.try_lock() {
                Ok(guard) => {
                    let result = self
                        .managed_source_lock(&plugin.id)
                        .map(|(_, source_lock)| source_lock);
                    drop(guard);
                    result
                }
                Err(TryLockError::Poisoned(poisoned)) => {
                    let guard = poisoned.into_inner();
                    let result = self
                        .managed_source_lock(&plugin.id)
                        .map(|(_, source_lock)| source_lock);
                    drop(guard);
                    result
                }
                Err(TryLockError::WouldBlock) => {
                    skipped.push(PluginAutomaticUpdateSkip {
                        plugin_id: plugin.id,
                        reason: "A plugin install, update, or lifecycle change began before this snapshot could be verified; automatic discovery was skipped instead of waiting for the install lock.".to_owned(),
                    });
                    continue;
                }
            };
            let source_lock = match verified_source_lock {
                Ok(source_lock) if source_lock.integrity.is_some() => source_lock,
                Ok(_) => {
                    skipped.push(PluginAutomaticUpdateSkip {
                        plugin_id: plugin.id,
                        reason: "Automatic discovery requires a verified snapshot integrity record. Re-import this legacy Git snapshot before enabling automatic checks; manual Check update remains available.".to_owned(),
                    });
                    continue;
                }
                Err(error) => {
                    skipped.push(PluginAutomaticUpdateSkip {
                        plugin_id: plugin.id,
                        reason: format!(
                            "Automatic discovery skipped because the installed snapshot could not be verified: {error}"
                        ),
                    });
                    continue;
                }
            };
            if attempted >= MAX_AUTOMATIC_UPDATE_CHECKS_PER_PASS {
                skipped.push(PluginAutomaticUpdateSkip {
                    plugin_id: plugin.id,
                    reason: format!(
                        "Automatic check pass is limited to {MAX_AUTOMATIC_UPDATE_CHECKS_PER_PASS} trusted plugins; use Check update or wait for the next pass."
                    ),
                });
                continue;
            }
            let Some(remaining_budget) = AUTOMATIC_UPDATE_CHECK_TIME_BUDGET
                .checked_sub(started.elapsed())
                .filter(|remaining| !remaining.is_zero())
            else {
                skipped.push(PluginAutomaticUpdateSkip {
                    plugin_id: plugin.id,
                    reason: format!(
                        "Automatic check pass reached its {}-second network budget; use Check update or wait for the next pass.",
                        AUTOMATIC_UPDATE_CHECK_TIME_BUDGET.as_secs()
                    ),
                });
                continue;
            };
            attempted += 1;

            match self.build_git_update_check(
                &plugin.id,
                source_lock,
                remaining_budget.min(AUTOMATIC_UPDATE_CHECK_TIMEOUT),
                true,
            ) {
                Ok(check) => checks.push(check),
                Err(error) => skipped.push(PluginAutomaticUpdateSkip {
                    plugin_id: plugin.id,
                    reason: format!(
                        "Automatic check failed without changing the installed snapshot: {error}"
                    ),
                }),
            }
        }

        PluginAutomaticUpdateReport {
            checked_at: Utc::now().to_rfc3339(),
            checks,
            skipped,
        }
    }

    /// Replaces an installed Git snapshot only after the caller explicitly
    /// asks for an update. The saved source/ref is resolved again under the
    /// installation lock and must still match the commit that the user saw in
    /// the preceding check; a moving branch therefore cannot swap in unseen
    /// code after confirmation. Git is used only to materialize a detached
    /// snapshot and plugin files are parsed, never run.
    pub fn update_from_git(
        &self,
        plugin_id: &str,
        expected_commit: &str,
    ) -> Result<PluginUpdateResult, String> {
        if !is_git_object_id(expected_commit) {
            return Err("Choose a checked Git commit before applying an update.".to_owned());
        }
        let _install_guard = self
            .install_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_root()?;
        let (destination, source_lock) = self.managed_source_lock(plugin_id)?;
        let source = git_source_from_lock(&source_lock)?;
        let latest_commit = resolve_remote_commit(&source.remote, &source.requested_ref)?;
        let previous_commit = source_lock.resolved_commit;

        if !latest_commit.eq_ignore_ascii_case(expected_commit) {
            return Err(format!(
                "The saved Git ref moved from the reviewed commit {expected_commit} to {latest_commit}. Check for updates again before applying it."
            ));
        }

        // Avoid needless writes and preserve the original source-lock metadata
        // when the remote ref still identifies the installed snapshot.
        if latest_commit.eq_ignore_ascii_case(&previous_commit) {
            return Ok(PluginUpdateResult {
                plugin: self.read_plugin_info(&destination)?,
                updated: false,
                previous_commit: previous_commit.clone(),
                current_commit: previous_commit,
            });
        }

        let plugin =
            self.install_resolved_remote_snapshot(&source, &latest_commit, Some(plugin_id))?;
        Ok(PluginUpdateResult {
            plugin,
            updated: true,
            previous_commit,
            current_commit: latest_commit,
        })
    }

    /// Stages an already-resolved detached Git snapshot, validates its
    /// manifest, then atomically swaps it into managed plugin storage. It does
    /// not run any source, build, package-manager, hook, frontend, or binary
    /// plugin code.
    fn install_resolved_remote_snapshot(
        &self,
        source: &GitSource,
        resolved_commit: &str,
        expected_plugin_id: Option<&str>,
    ) -> Result<PluginInfo, String> {
        let staging = self.root.join(format!(".staging-{}", unique_suffix()));
        if staging.exists() {
            return Err(
                "A plugin installation staging directory already exists; try again.".to_owned(),
            );
        }

        if let Err(error) = checkout_remote_ref(
            &source.remote,
            &source.requested_ref,
            resolved_commit,
            &staging,
        ) {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }

        let installation = (|| {
            let manifest_path = find_manifest(&staging).ok_or_else(|| {
                "The repository does not contain ihub.plugin.json or plugin.json.".to_owned()
            })?;
            let mut manifest = read_manifest(&manifest_path)?;
            if let Some(expected_plugin_id) = expected_plugin_id {
                // Public uTools packages do not own an in-manifest install ID.
                // Their host-generated ID is preserved on refresh so a feature
                // edit cannot silently fork lifecycle state into a new plugin.
                if manifest.compatibility.is_utools() {
                    manifest.id = expected_plugin_id.to_owned();
                }
            }
            validate_manifest(&manifest)?;
            let plugin_id = manifest.id.clone();
            if let Some(expected_plugin_id) = expected_plugin_id {
                if plugin_id != expected_plugin_id {
                    return Err(format!(
                        "Refusing to update plugin '{expected_plugin_id}': the fetched manifest declares a different ID '{plugin_id}'."
                    ));
                }
            }
            let destination = self.root.join(&plugin_id);
            self.guard_existing_plugin_destination(
                &destination,
                &plugin_id,
                source,
                expected_plugin_id.is_some(),
            )?;

            if expected_plugin_id.is_some() {
                // A Git ref can contain an entirely different manifest even
                // when its ID/source/ref are unchanged. Stage and validate it
                // first, then compare the host-facing security declaration
                // before *any* existing files, leases, search registrations,
                // or session-only values are touched.
                let installed_manifest_path = find_manifest(&destination).ok_or_else(|| {
                    format!(
                        "Installed plugin '{plugin_id}' has no readable manifest; refusing to replace it."
                    )
                })?;
                let installed_manifest = read_manifest(&installed_manifest_path)?;
                validate_manifest(&installed_manifest)?;
                if installed_manifest.id != plugin_id {
                    return Err(format!(
                        "Installed plugin manifest ID does not match '{plugin_id}'; refusing to replace it."
                    ));
                }
                ensure_update_security_declaration_matches(
                    &plugin_id,
                    &installed_manifest,
                    &manifest,
                )?;
            }
            let integrity = snapshot_integrity(&staging, &manifest_path, &manifest)?;
            let source_lock = PluginSourceLock {
                source: source.remote.clone(),
                requested_ref: source.requested_ref.clone(),
                resolved_commit: resolved_commit.to_owned(),
                installed_at: Utc::now().to_rfc3339(),
                integrity: Some(integrity),
            };
            write_source_lock(&staging, &source_lock)?;

            let backup = self
                .root
                .join(format!(".backup-{}-{}", plugin_id, unique_suffix()));
            let had_existing = destination.exists();
            if had_existing {
                fs::rename(&destination, &backup).map_err(|error| {
                    format!("Could not prepare the existing plugin for update: {error}")
                })?;
            }
            if let Err(error) = fs::rename(&staging, &destination) {
                if had_existing {
                    let _ = fs::rename(&backup, &destination);
                }
                return Err(format!("Could not activate the installed plugin: {error}"));
            }
            if had_existing {
                let _ = fs::remove_dir_all(&backup);
            }
            // A new import is an explicit opt-in. Clear a stale lifecycle
            // record left behind by a manually removed old snapshot so this
            // newly selected source is enabled by default. Explicit Git
            // updates deliberately preserve the user's existing state.
            if expected_plugin_id.is_none() {
                let mut lifecycle = self.read_lifecycle_store()?;
                lifecycle.remove(&plugin_id);
                self.write_lifecycle_store(&lifecycle)?;
            }
            self.read_plugin_info(&destination)
        })();

        if installation.is_err() && staging.exists() {
            let _ = fs::remove_dir_all(&staging);
        }
        installation
    }

    /// A manifest ID is an installation identity, not an invitation for an
    /// unrelated repository to replace files under the same directory. GitHub
    /// imports remain decentralized, but a collision must be resolved by the
    /// owner explicitly (unlink/remove the existing package) instead of
    /// silently swapping a trusted plugin for a lookalike.
    fn guard_existing_plugin_destination(
        &self,
        destination: &Path,
        plugin_id: &str,
        incoming: &GitSource,
        is_explicit_update: bool,
    ) -> Result<(), String> {
        if self.read_local_links()?.links.contains_key(plugin_id) {
            return Err(format!(
                "Plugin '{plugin_id}' is currently linked from a local development directory. Unlink it before importing or updating a managed snapshot."
            ));
        }
        if !destination.exists() {
            return Ok(());
        }

        let storage_root = self.root.as_ref().canonicalize().map_err(|error| {
            format!(
                "Could not resolve iHub's plugin directory {}: {error}",
                self.root.display()
            )
        })?;
        let existing_root = destination.canonicalize().map_err(|error| {
            format!("Could not resolve existing plugin '{plugin_id}' before import: {error}")
        })?;
        if !existing_root.is_dir() {
            return Err(format!(
                "Existing plugin destination for '{plugin_id}' is not a directory; refusing to replace it."
            ));
        }
        ensure_path_within(&existing_root, &storage_root, "Existing plugin directory")?;

        let existing = read_source_metadata(&existing_root).map_err(|error| {
            format!(
                "Plugin '{plugin_id}' has no trustworthy source record; refusing to replace it automatically: {error}"
            )
        })?;
        let existing_lock = existing.lock.ok_or_else(|| {
            format!(
                "Plugin '{plugin_id}' uses legacy source metadata without an immutable lock. Re-import it manually instead of replacing it from another repository."
            )
        })?;
        if existing_lock.source != incoming.remote {
            return Err(format!(
                "Refusing to replace plugin '{plugin_id}' from '{}': that ID is already managed by '{}'. Plugin IDs cannot be claimed by another repository.",
                incoming.remote, existing_lock.source
            ));
        }
        if existing_lock.requested_ref != incoming.requested_ref {
            return Err(format!(
                "Plugin '{plugin_id}' is locked to ref '{}'. Use its explicit update flow instead of importing a different ref '{}'.",
                existing_lock.requested_ref, incoming.requested_ref
            ));
        }
        if !is_explicit_update {
            return Err(format!(
                "Plugin '{plugin_id}' is already installed from this source. Check for updates and confirm the replacement instead of re-importing it."
            ));
        }
        Ok(())
    }

    /// Explicitly links an existing local plugin directory for development.
    ///
    /// Unlike a Git installation, this writes only a small record inside
    /// iHub's plugin storage. The project directory remains in place, so a
    /// later `pnpm build` is visible to iHub after its plugin frontend is
    /// reopened. This is intentionally not an implicit local-path importer.
    pub fn link_from_local(&self, directory: &str) -> Result<PluginInfo, String> {
        let requested = directory.trim();
        if requested.is_empty() {
            return Err("Choose an existing local plugin directory to link.".to_owned());
        }
        let requested_path = PathBuf::from(requested);
        if !requested_path.is_absolute() {
            return Err("Local plugin directories must use an absolute path.".to_owned());
        }

        let _install_guard = self
            .install_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_root()?;
        let storage_root = self.root.as_ref().canonicalize().map_err(|error| {
            format!(
                "Could not resolve iHub's plugin directory {}: {error}",
                self.root.display()
            )
        })?;
        let local_root = requested_path.canonicalize().map_err(|error| {
            format!(
                "Could not resolve local plugin directory '{}': {error}",
                requested_path.display()
            )
        })?;
        if !local_root.is_dir() {
            return Err("The local plugin path must point to a directory.".to_owned());
        }
        if local_root.starts_with(&storage_root) {
            return Err(
                "Choose a development project outside iHub's managed plugin directory.".to_owned(),
            );
        }

        let manifest_path = canonical_manifest_path(&local_root)?;
        let manifest = read_manifest(&manifest_path)?;
        validate_manifest(&manifest)?;
        let plugin_id = manifest.id.clone();
        let package_root = manifest_path.parent().ok_or_else(|| {
            format!(
                "Plugin manifest '{}' has no containing directory.",
                manifest_path.display()
            )
        })?;
        ensure_path_within(package_root, &local_root, "Plugin package")?;
        // Validate every declared image before persisting the development
        // link. A broken or hostile artwork declaration must not leave behind
        // a link record that only fails on the next list refresh.
        load_manifest_artwork(package_root, &manifest)?;

        let mut links = self.read_local_links()?;
        if let Some((other_id, _)) = links.links.iter().find(|(linked_id, link)| {
            linked_id.as_str() != plugin_id.as_str()
                && PathBuf::from(&link.canonical_path)
                    .canonicalize()
                    .is_ok_and(|existing| existing == local_root)
        }) {
            return Err(format!(
                "This local directory is already linked as plugin '{other_id}'."
            ));
        }
        links.links.insert(
            plugin_id.clone(),
            LocalPluginLink {
                canonical_path: local_root.to_string_lossy().into_owned(),
                linked_at: Utc::now().to_rfc3339(),
                name: Some(manifest.name.clone()),
                version: Some(manifest.version.clone()),
                description: manifest.description.clone(),
            },
        );
        self.write_local_links(&links)?;

        let link = links
            .links
            .get(&plugin_id)
            .expect("local plugin link was just inserted");
        self.read_linked_plugin_info(&plugin_id, link)
    }

    /// Reports only the hard-coded first-party projects that are actually
    /// available beside the source tree used to build this host. A normal
    /// release built elsewhere therefore reports them as unavailable instead
    /// of pretending that an unpublished package can be downloaded.
    pub fn official_workspace_projects(&self) -> Vec<OfficialWorkspacePluginProject> {
        OFFICIAL_WORKSPACE_PLUGIN_SPECS
            .iter()
            .map(|spec| match resolve_official_workspace_plugin(spec) {
                Ok((path, manifest_name)) => OfficialWorkspacePluginProject {
                    id: spec.id.to_owned(),
                    name: manifest_name,
                    available: true,
                    local_path: Some(path.to_string_lossy().into_owned()),
                    detail: "已验证当前源码工作区中的 manifest 与构建入口。".to_owned(),
                },
                Err(_) => OfficialWorkspacePluginProject {
                    id: spec.id.to_owned(),
                    name: spec.name.to_owned(),
                    available: false,
                    local_path: None,
                    detail:
                        "当前安装包没有可用的官方源码工作区；请使用从完整 checkout 构建的开发版。"
                            .to_owned(),
                },
            })
            .collect()
    }

    /// Links one allowlisted first-party workspace project without accepting a
    /// renderer-provided path. `link_from_local` repeats the full manifest and
    /// package-boundary validation before writing the development-link record.
    pub fn link_official_workspace_plugin(&self, plugin_id: &str) -> Result<PluginInfo, String> {
        let spec = OFFICIAL_WORKSPACE_PLUGIN_SPECS
            .iter()
            .find(|spec| spec.id == plugin_id)
            .ok_or_else(|| {
                format!("Plugin '{plugin_id}' is not an allowlisted official workspace project.")
            })?;
        let (path, _) = resolve_official_workspace_plugin(spec)?;
        self.link_from_local(&path.to_string_lossy())
    }

    /// Removes only iHub's metadata for a local development link. The local
    /// project directory and an optional installed snapshot are left intact.
    pub fn unlink_from_local(&self, plugin_id: &str) -> Result<(), String> {
        if !is_valid_identifier(plugin_id) {
            return Err("Plugin ID must contain only letters, digits, '.', '_' or '-'.".to_owned());
        }
        let _install_guard = self
            .install_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_root()?;
        let mut links = self.read_local_links()?;
        if links.links.remove(plugin_id).is_none() {
            return Err(format!(
                "Plugin '{plugin_id}' is not linked from a local directory."
            ));
        }
        self.write_local_links(&links)
    }

    /// Persists the user's enabled/disabled choice independently from a
    /// plugin manifest. A disabled plugin remains installed (or linked) and
    /// can be enabled again after restart, but host execution paths reject it
    /// before loading a frontend, command worker, or search provider.
    pub fn set_enabled(
        &self,
        plugin_id: &str,
        enabled: bool,
    ) -> Result<PluginLifecycleUpdate, String> {
        if !is_valid_identifier(plugin_id) {
            return Err("Plugin ID must contain only letters, digits, '.', '_' or '-'.".to_owned());
        }
        let _install_guard = self
            .install_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_root()?;
        // Resolve and validate first. A lifecycle record can never make an
        // arbitrary ID appear installed.
        self.resolve_plugin_root(plugin_id)?;

        let mut lifecycle = self.read_lifecycle_store()?;
        lifecycle.set_enabled(plugin_id, enabled);
        self.write_lifecycle_store(&lifecycle)?;
        let plugin = self.read_plugin_info_for_id_with_lifecycle(plugin_id, &lifecycle)?;
        Ok(PluginLifecycleUpdate { plugin, enabled })
    }

    /// Ensures an incoming command comes from a currently installed/linkable
    /// plugin and that its persisted lifecycle choice allows execution. This
    /// gives disabled plugins a host-side boundary even when an already-open
    /// iframe tries to send a stale bridge message.
    pub fn ensure_plugin_enabled(&self, plugin_id: &str) -> Result<(), String> {
        if !is_valid_identifier(plugin_id) {
            return Err("Plugin ID must contain only letters, digits, '.', '_' or '-'.".to_owned());
        }
        self.resolve_plugin_root(plugin_id)?;
        let lifecycle = self.read_lifecycle_store()?;
        if lifecycle.is_enabled(plugin_id) {
            Ok(())
        } else {
            Err(format!(
                "Plugin '{plugin_id}' is disabled. Enable it from the Plugin Center before using it."
            ))
        }
    }

    /// Removes only a managed Git snapshot. A local development project is
    /// never a deletion target: if a link shadows the same ID, the developer
    /// must unlink it first so the UI cannot mistake a source tree for an
    /// installed copy. The target is canonicalized beneath iHub storage and
    /// must carry host-written Git provenance before it is staged for removal.
    pub fn uninstall_managed_snapshot(
        &self,
        plugin_id: &str,
    ) -> Result<PluginUninstallResult, String> {
        if !is_valid_identifier(plugin_id) {
            return Err("Plugin ID must contain only letters, digits, '.', '_' or '-'.".to_owned());
        }
        let _install_guard = self
            .install_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_root()?;
        if self.read_local_links()?.links.contains_key(plugin_id) {
            return Err(format!(
                "Plugin '{plugin_id}' is linked from a local development directory. Unlink it first; iHub never deletes developer source folders."
            ));
        }

        let storage_root = self.root.as_ref().canonicalize().map_err(|error| {
            format!(
                "Could not resolve iHub's plugin directory {}: {error}",
                self.root.display()
            )
        })?;
        let destination = storage_root
            .join(plugin_id)
            .canonicalize()
            .map_err(|error| {
                format!("Plugin '{plugin_id}' is not installed or cannot be resolved: {error}")
            })?;
        if !destination.is_dir() {
            return Err(format!("Plugin '{plugin_id}' is not installed."));
        }
        ensure_path_within(&destination, &storage_root, "Managed plugin snapshot")?;
        self.validate_plugin_root(plugin_id, &destination)?;
        let source = read_source_metadata(&destination).map_err(|error| {
            format!(
                "Plugin '{plugin_id}' has no managed Git provenance and will not be removed automatically: {error}"
            )
        })?;
        if source.source.trim().is_empty() {
            return Err(format!(
                "Plugin '{plugin_id}' has no managed Git provenance and will not be removed automatically."
            ));
        }
        let lifecycle_before = self.read_lifecycle_store()?;
        let plugin = self.read_plugin_info_with_lifecycle(&destination, &lifecycle_before)?;
        let staged = self
            .root
            .join(format!(".uninstall-{}-{}", plugin_id, unique_suffix()));
        fs::rename(&destination, &staged).map_err(|error| {
            format!("Could not stage managed plugin '{plugin_id}' for removal: {error}")
        })?;

        let mut lifecycle_after = lifecycle_before.clone();
        lifecycle_after.remove(plugin_id);
        if let Err(error) = self.write_lifecycle_store(&lifecycle_after) {
            let _ = fs::rename(&staged, &destination);
            return Err(format!(
                "Could not update plugin lifecycle state; the managed snapshot was restored: {error}"
            ));
        }
        if let Err(error) = fs::remove_dir_all(&staged) {
            // Keep the lifecycle state aligned with the restored snapshot when
            // possible. A rare partial filesystem failure is surfaced rather
            // than silently claiming that deletion succeeded.
            let _ = self.write_lifecycle_store(&lifecycle_before);
            let restored = if staged.exists() {
                fs::rename(&staged, &destination).is_ok()
            } else {
                false
            };
            return Err(if restored {
                format!(
                    "Could not remove managed plugin '{plugin_id}'; the snapshot was restored: {error}"
                )
            } else {
                format!(
                    "Could not finish removing managed plugin '{plugin_id}': {error}. Check iHub's plugin directory before retrying."
                )
            });
        }

        Ok(PluginUninstallResult {
            plugin_id: plugin.id,
            plugin_name: plugin.name,
            source: source.source,
        })
    }

    pub fn run_command(
        &self,
        plugin_id: &str,
        command_id: &str,
        input: Option<Value>,
    ) -> Result<PluginCommandResult, String> {
        if !is_valid_identifier(plugin_id) || !is_valid_identifier(command_id) {
            return Err(
                "Plugin and command IDs must contain only letters, digits, '.', '_' or '-'."
                    .to_owned(),
            );
        }
        self.ensure_plugin_enabled(plugin_id)?;
        let plugin_root = self.resolve_plugin_root(plugin_id)?;
        let manifest_path = canonical_manifest_path(&plugin_root)?;
        let manifest = read_manifest(&manifest_path)?;
        validate_manifest(&manifest)?;
        if manifest.id != plugin_id {
            return Err(format!("Plugin manifest ID does not match '{plugin_id}'."));
        }
        let command = declared_commands(&manifest)
            .iter()
            .find(|command| command.id == command_id)
            .ok_or_else(|| {
                format!("Plugin '{plugin_id}' does not expose command '{command_id}'.")
            })?;
        if command_execution(&manifest, command) != CommandExecution::Native {
            return Err(format!(
                "Plugin command '{command_id}' is a frontend command. Open its plugin UI instead of starting a native worker."
            ));
        }
        let command_timeout = command_timeout(command);
        let package_root = manifest_path
            .parent()
            .ok_or_else(|| format!("Plugin '{plugin_id}' has an invalid manifest path."))?;
        ensure_path_within(package_root, &plugin_root, "Plugin package")?;
        let selected_backend = manifest.backend.as_ref().and_then(select_backend_binary);
        let binary_decl = command
            .binary
            .as_deref()
            .or_else(|| {
                manifest
                    .backend
                    .as_ref()
                    .and_then(|backend| backend.binary.as_deref())
            })
            .or_else(|| selected_backend.map(|binary| binary.path.as_str()))
            .ok_or_else(|| format!("Plugin command '{command_id}' has no backend binary."))?;
        let binary = resolve_plugin_path(package_root, binary_decl)?
            .canonicalize()
            .map_err(|error| format!("Could not resolve plugin binary '{binary_decl}': {error}"))?;
        ensure_path_within(&binary, &plugin_root, "Plugin binary")?;
        if !binary.is_file() {
            return Err(format!(
                "Plugin binary does not exist: {}",
                binary.display()
            ));
        }

        let input_value = input.unwrap_or(Value::Null);
        let input_text = serde_json::to_string(&input_value)
            .map_err(|error| format!("Could not serialize plugin input: {error}"))?;
        if input_text.len() > MAX_PLUGIN_COMMAND_INPUT_BYTES {
            return Err(format!(
                "Plugin command input exceeds the {} KiB limit. Pass large data through a user-selected file or plugin-owned storage.",
                MAX_PLUGIN_COMMAND_INPUT_BYTES / 1024
            ));
        }
        let is_jsonl_rpc = manifest
            .backend
            .as_ref()
            .and_then(|backend| backend.protocol.as_deref())
            == Some("jsonl-rpc-v1")
            && command.binary.is_none();
        let rpc_id = is_jsonl_rpc.then(next_rpc_id);
        let stdin_text = if is_jsonl_rpc {
            format!(
                "{}\n",
                serde_json::to_string(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": rpc_id.as_deref().expect("JSONL commands must have an RPC id"),
                    "method": command_id,
                    "params": input_value,
                }))
                .map_err(|error| format!("Could not serialize JSON-RPC input: {error}"))?
            )
        } else {
            input_text.clone()
        };
        let mut args = selected_backend
            .map(|binary| binary.args.clone())
            .unwrap_or_default();
        args.extend(
            command
                .args
                .iter()
                .map(|argument| argument.replace("{{input}}", &input_text))
                .collect::<Vec<_>>(),
        );
        let mut child = background_command(&binary)
            .args(args)
            .current_dir(package_root)
            .env("IHUB_PLUGIN_ID", plugin_id)
            .env("IHUB_COMMAND_ID", command_id)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("Could not launch plugin command: {error}"))?;

        // Drain both streams while the child runs. Polling a child with pipes
        // but waiting to read until it exits can deadlock when a noisy worker
        // fills an OS pipe buffer before it finishes its single JSONL reply.
        let stdout_reader = child
            .stdout
            .take()
            .ok_or_else(|| "Plugin command stdout was not captured.".to_owned())?;
        let stderr_reader = child
            .stderr
            .take()
            .ok_or_else(|| "Plugin command stderr was not captured.".to_owned())?;
        let stdout_task = spawn_captured_output_reader(stdout_reader);
        let stderr_task = spawn_captured_output_reader(stderr_reader);

        // Write in a detached worker so a plugin that never reads stdin cannot
        // block this host thread before the command's timeout begins. The
        // child is reaped on timeout, closing the pipe and releasing the
        // writer. We intentionally do not put input in an environment
        // variable: Windows has a small total environment block limit and
        // environment input can be exposed to unrelated process inspection.
        let stdin_task = child.stdin.take().map(|mut stdin| {
            let bytes = stdin_text.into_bytes();
            thread::spawn(move || {
                let _ = stdin.write_all(&bytes);
            })
        });

        let wait = match wait_for_child_with_timeout(&mut child, command_timeout) {
            Ok(wait) => wait,
            Err(error) => {
                drop(stdin_task);
                drop(stdout_task);
                drop(stderr_task);
                return Err(error);
            }
        };
        if matches!(&wait, ChildWaitOutcome::TimedOut) {
            // The child was killed and reaped in wait_for_child_with_timeout.
            // Do not block a launcher command on pipe joins after a timeout.
            drop(stdin_task);
            drop(stdout_task);
            drop(stderr_task);
            return Err(format!(
                "Plugin command '{command_id}' timed out after {} ms and was terminated.",
                command_timeout.as_millis()
            ));
        }
        let ChildWaitOutcome::Exited(status) = wait else {
            unreachable!("timeout branch returned above");
        };
        drop(stdin_task);
        let stdout = join_captured_output(stdout_task, "stdout")?;
        let mut stderr = join_captured_output(stderr_task, "stderr")?;
        let mut success = status.success();
        let parsed_output = if let Some(rpc_id) = rpc_id.as_deref() {
            match parse_jsonl_rpc_response(&stdout, rpc_id) {
                Ok((output, rpc_error)) => {
                    if let Some(rpc_error) = rpc_error {
                        success = false;
                        append_plugin_diagnostic(&mut stderr, &rpc_error);
                    }
                    output
                }
                Err(error) => {
                    success = false;
                    append_plugin_diagnostic(
                        &mut stderr,
                        &format!("Invalid jsonl-rpc-v1 response: {error}"),
                    );
                    None
                }
            }
        } else {
            serde_json::from_str::<Value>(&stdout).ok()
        };

        Ok(PluginCommandResult {
            plugin_id: plugin_id.to_owned(),
            command_id: command_id.to_owned(),
            success,
            exit_code: status.code(),
            stdout,
            stderr,
            output: parsed_output,
        })
    }

    /// Resolves an installed or explicitly linked development plugin's frontend
    /// bundle to canonical paths. The package manifest can live one directory
    /// below a cloned repository, but its entry and bundle root must remain
    /// inside that package. Canonicalization prevents symlinks and `..` paths
    /// from turning an iframe asset request into arbitrary local file access.
    pub(crate) fn frontend_asset_bundle(
        &self,
        plugin_id: &str,
    ) -> Result<PluginFrontendAssetBundle, String> {
        if !is_valid_identifier(plugin_id) {
            return Err("Plugin ID must contain only letters, digits, '.', '_' or '-'.".to_owned());
        }

        self.ensure_plugin_enabled(plugin_id)?;
        let active_development_root = self
            .read_local_links()?
            .links
            .get(plugin_id)
            .and_then(|link| self.resolve_local_link_root(plugin_id, link).ok());
        let plugin_root = self.resolve_plugin_root(plugin_id)?;
        let is_development = active_development_root
            .as_ref()
            .is_some_and(|root| root == &plugin_root);
        let manifest_path = canonical_manifest_path(&plugin_root)?;

        let manifest = read_manifest(&manifest_path)?;
        validate_manifest(&manifest)?;
        if manifest.id != plugin_id {
            return Err(format!("Plugin manifest ID does not match '{plugin_id}'."));
        }
        let frontend_entry = manifest_frontend_entry(&manifest)
            .ok_or_else(|| format!("Plugin '{plugin_id}' does not declare entry.frontend."))?;
        let package_root = manifest_path
            .parent()
            .ok_or_else(|| format!("Plugin '{plugin_id}' has an invalid manifest path."))?;
        ensure_path_within(package_root, &plugin_root, "Plugin package")?;

        let frontend_path = package_root
            .join(&frontend_entry)
            .canonicalize()
            .map_err(|error| {
                format!("Could not resolve plugin frontend '{frontend_entry}': {error}")
            })?;
        ensure_path_within(&frontend_path, package_root, "Plugin frontend")?;
        if !frontend_path.is_file() {
            return Err(format!(
                "Plugin frontend is not a file: {}",
                frontend_path.display()
            ));
        }

        let asset_root = frontend_path
            .parent()
            .ok_or_else(|| format!("Plugin '{plugin_id}' frontend has no parent directory."))?
            .canonicalize()
            .map_err(|error| format!("Could not resolve plugin frontend bundle: {error}"))?;
        ensure_path_within(&asset_root, package_root, "Plugin frontend bundle")?;
        if asset_root == package_root && !manifest.compatibility.is_utools() {
            return Err(format!(
                "Plugin '{plugin_id}' frontend must live in a dedicated child build directory such as dist/index.html, not beside plugin.json."
            ));
        }
        let blocked_asset_paths = manifest
            .utools_preload
            .as_deref()
            .and_then(|preload| {
                let candidate = package_root.join(preload).canonicalize().ok()?;
                (candidate.is_file() && candidate.starts_with(&asset_root)).then_some(candidate)
            })
            .into_iter()
            .collect::<Vec<_>>();

        Ok(PluginFrontendAssetBundle {
            plugin_id: plugin_id.to_owned(),
            asset_root,
            entry: frontend_path,
            blocked_asset_paths,
            allows_display_capture: manifest.permissions.screen_capture
                || manifest.compatibility.is_utools(),
            allows_microphone: manifest.permissions.microphone,
            allows_remote_network: manifest
                .permissions
                .network
                .as_ref()
                .is_some_and(|network| !network.allow.is_empty()),
            utools_compat: manifest
                .compatibility
                .is_utools()
                .then(|| UtoolsCompatRuntimeConfig {
                    app_version: env!("CARGO_PKG_VERSION").to_owned(),
                    is_development,
                    plugin_id: plugin_id.to_owned(),
                    commands: manifest.utools_commands,
                    native_id: String::new(),
                    paths: BTreeMap::new(),
                    idle_ubrowsers: Vec::new(),
                    window_type: "main".to_owned(),
                    lifecycle_owner: true,
                }),
            utools_browser_preload_src: None,
        })
    }

    /// Resolves a uTools BrowserWindow entry against the already-validated
    /// frontend bundle. The requested URL can select only a sibling HTML file;
    /// it cannot introduce a scheme, absolute path, traversal, encoded path,
    /// preload file, symlink escape, or directory.
    pub(crate) fn browser_frontend_asset_bundle(
        &self,
        plugin_id: &str,
        relative_url: &str,
        preload: Option<&str>,
    ) -> Result<(PluginFrontendAssetBundle, String), String> {
        let mut bundle = self.frontend_asset_bundle(plugin_id)?;
        let Some(config) = bundle.utools_compat.as_mut() else {
            return Err("BrowserWindow compatibility is available only to validated imported uTools packages.".to_owned());
        };
        if relative_url.is_empty()
            || relative_url.chars().count() > 2048
            || relative_url.chars().any(char::is_control)
            || relative_url.starts_with('/')
            || relative_url.starts_with('\\')
        {
            return Err("uTools BrowserWindow requires a bounded relative HTML URL.".to_owned());
        }
        let fragment_index = relative_url.find('#').unwrap_or(relative_url.len());
        let query_index = relative_url[..fragment_index]
            .find('?')
            .unwrap_or(fragment_index);
        let path_text = &relative_url[..query_index];
        let suffix = &relative_url[query_index..];
        if path_text.is_empty()
            || path_text.contains('\\')
            || path_text.contains('%')
            || path_text.contains(':')
        {
            return Err(
                "uTools BrowserWindow URL must name a plain relative HTML path.".to_owned(),
            );
        }
        let relative = Path::new(path_text);
        if relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(
                "uTools BrowserWindow URL cannot traverse outside its frontend bundle.".to_owned(),
            );
        }
        if !relative
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("html") || extension.eq_ignore_ascii_case("htm")
            })
        {
            return Err("uTools BrowserWindow entry must be an HTML file.".to_owned());
        }
        let entry = bundle
            .asset_root
            .join(relative)
            .canonicalize()
            .map_err(|error| {
                format!("Could not resolve uTools BrowserWindow entry '{path_text}': {error}")
            })?;
        ensure_path_within(&entry, &bundle.asset_root, "uTools BrowserWindow entry")?;
        if !entry.is_file() {
            return Err("uTools BrowserWindow entry is not a file.".to_owned());
        }
        if bundle
            .blocked_asset_paths
            .iter()
            .any(|blocked| blocked == &entry)
        {
            return Err(
                "A uTools Electron preload cannot be opened as a BrowserWindow page.".to_owned(),
            );
        }
        bundle.entry = entry;
        config.window_type = "browser".to_owned();
        config.lifecycle_owner = false;
        if let Some(preload) = preload {
            if preload.is_empty()
                || preload.chars().count() > 1024
                || !preload.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-')
                })
            {
                return Err(
                    "uTools BrowserWindow preload must be a plain relative JavaScript path."
                        .to_owned(),
                );
            }
            let relative_preload = Path::new(preload);
            if relative_preload
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
                || !relative_preload
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| {
                        extension.eq_ignore_ascii_case("js")
                            || extension.eq_ignore_ascii_case("cjs")
                    })
            {
                return Err(
                    "uTools BrowserWindow preload must be a relative JavaScript file.".to_owned(),
                );
            }
            let resolved = bundle
                .asset_root
                .join(relative_preload)
                .canonicalize()
                .map_err(|error| {
                    format!("Could not resolve uTools BrowserWindow preload '{preload}': {error}")
                })?;
            ensure_path_within(
                &resolved,
                &bundle.asset_root,
                "uTools BrowserWindow preload",
            )?;
            if !resolved.is_file() {
                return Err("uTools BrowserWindow preload is not a file.".to_owned());
            }
            bundle
                .blocked_asset_paths
                .retain(|blocked| blocked != &resolved);
            bundle.utools_browser_preload_src = Some(preload.replace('\\', "/"));
        }
        Ok((bundle, suffix.to_owned()))
    }

    /// Regression-test helper for callers that only need the resolved entry.
    /// Production WebView code must request a narrow bundle URL from the
    /// plugin asset server instead of exposing this path.
    #[cfg(test)]
    pub fn frontend_path(&self, plugin_id: &str) -> Result<PathBuf, String> {
        self.frontend_asset_bundle(plugin_id)
            .map(|bundle| bundle.entry)
    }

    /// Returns the manifest permission required for a sensitive frontend host
    /// method. Commands, search, settings, lifecycle, and logging stay
    /// permission-free; native plugin binaries are intentionally not covered
    /// by this bridge-level gate.
    pub fn required_permission_for_host_method(method: &str) -> Option<&'static str> {
        match method {
            "filesystem.selectDirectory"
            | "filesystem.selectFiles"
            | "filesystem.batchRename.preview" => Some("filesystem.read: [\"user-selected\"]"),
            "filesystem.batchRename.apply" => Some("filesystem.write: [\"user-selected\"]"),
            "developer.createProject" => Some("filesystem.read/write: [\"user-selected\"]"),
            "clipboard.readText" | "clipboard.read" => Some("clipboard.read"),
            "clipboard.writeText" | "clipboard.write" => Some("clipboard.write"),
            "clipboard.history.snapshot" => Some("clipboard.history"),
            "screenCapture.acquireFocusLease"
            | "screenCapture.releaseFocusLease"
            | "compatibility.utools.screen.capture" => Some("screenCapture"),
            "cursorColor.sampleOnce" => Some("cursorColor"),
            "shell.openPath" | "shell.open" => Some("shell.openPath"),
            "shell.openExternal" => Some("shell.openExternal"),
            "notifications.show" => Some("notifications"),
            "native.runCommand" => Some("nativeApi"),
            "window.manageLauncher" => Some("windowManagement"),
            "launcherContext.consume" => Some("launcherContext"),
            _ => None,
        }
    }

    /// Looks up a concrete frontend host method against an installed or local
    /// development plugin manifest. IDs are constrained before joining paths,
    /// and both the manifest and package directory are canonicalized beneath
    /// the selected plugin root.
    pub fn allows_host_method(&self, plugin_id: &str, method: &str) -> Result<bool, String> {
        if !is_valid_identifier(plugin_id) {
            return Err("Plugin ID must contain only letters, digits, '.', '_' or '-'.".to_owned());
        }

        self.ensure_plugin_enabled(plugin_id)?;
        let plugin_root = self.resolve_plugin_root(plugin_id)?;
        let manifest_path = canonical_manifest_path(&plugin_root)?;
        let manifest = read_manifest(&manifest_path)?;
        validate_manifest(&manifest)?;
        if manifest.id != plugin_id {
            return Err(format!("Plugin manifest ID does not match '{plugin_id}'."));
        }

        Ok(match method {
            "filesystem.selectDirectory"
            | "filesystem.selectFiles"
            | "filesystem.batchRename.preview" => manifest
                .permissions
                .filesystem
                .as_ref()
                .is_some_and(FilesystemPermissions::allows_user_selected_read),
            "filesystem.batchRename.apply" => manifest
                .permissions
                .filesystem
                .as_ref()
                .is_some_and(FilesystemPermissions::allows_user_selected_write),
            "developer.createProject" => {
                manifest
                    .permissions
                    .filesystem
                    .as_ref()
                    .is_some_and(|filesystem| {
                        filesystem.allows_user_selected_read()
                            && filesystem.allows_user_selected_write()
                    })
            }
            "clipboard.readText" | "clipboard.read" => manifest
                .permissions
                .clipboard
                .as_ref()
                .is_some_and(|clipboard| clipboard.read),
            "clipboard.writeText" | "clipboard.write" => manifest
                .permissions
                .clipboard
                .as_ref()
                .is_some_and(|clipboard| clipboard.write),
            "clipboard.history.snapshot" => manifest
                .permissions
                .clipboard
                .as_ref()
                .is_some_and(|clipboard| clipboard.history),
            "screenCapture.acquireFocusLease" | "screenCapture.releaseFocusLease" => {
                manifest.permissions.screen_capture || manifest.compatibility.is_utools()
            }
            "compatibility.utools.screen.capture" => {
                // The compatibility call never receives native pixels
                // directly. The trusted parent captures only after its own
                // confirmation and returns the user's cropped PNG selection.
                manifest.permissions.screen_capture || manifest.compatibility.is_utools()
            }
            "cursorColor.sampleOnce" => {
                // A uTools `screenColorPick` call is never ambient access:
                // it is intercepted by the visible iHub parent, requires a
                // fresh person click, waits the fixed native delay, and returns
                // only HEX/RGB. No other sensitive permission is implied.
                manifest.permissions.cursor_color || manifest.compatibility.is_utools()
            }
            "shell.openPath" | "shell.open" => manifest
                .permissions
                .shell
                .as_ref()
                .is_some_and(|shell| shell.open_path),
            "shell.openExternal" => manifest
                .permissions
                .shell
                .as_ref()
                .is_some_and(|shell| shell.open_external),
            "notifications.show" => manifest.permissions.notifications,
            "native.runCommand" => manifest.permissions.native_api,
            "window.manageLauncher" => manifest.permissions.window_management,
            "launcherContext.consume" => manifest
                .permissions
                .launcher_context
                .as_ref()
                .is_some_and(|context| context.text || context.files || context.image),
            _ => true,
        })
    }

    /// Checks the exact categories in a pending launcher-context handoff.
    /// This is deliberately separate from `allows_host_method`: consuming a
    /// text-only declaration must not make a queued file/image payload
    /// readable merely because both use the same one-shot bridge method.
    pub fn allows_launcher_context(
        &self,
        plugin_id: &str,
        needs_text: bool,
        needs_files: bool,
        needs_image: bool,
    ) -> Result<bool, String> {
        if !is_valid_identifier(plugin_id) {
            return Err("Plugin ID must contain only letters, digits, '.', '_' or '-'".to_owned());
        }
        if !needs_text && !needs_files && !needs_image {
            return Err("A launcher context must contain text, files, or an image.".to_owned());
        }

        self.ensure_plugin_enabled(plugin_id)?;
        let plugin_root = self.resolve_plugin_root(plugin_id)?;
        let manifest_path = canonical_manifest_path(&plugin_root)?;
        let manifest = read_manifest(&manifest_path)?;
        validate_manifest(&manifest)?;
        if manifest.id != plugin_id {
            return Err(format!("Plugin manifest ID does not match '{plugin_id}'."));
        }
        let Some(context) = manifest.permissions.launcher_context.as_ref() else {
            return Ok(false);
        };
        Ok((!needs_text || context.text)
            && (!needs_files || context.files)
            && (!needs_image || context.image))
    }

    /// Ensures that a host-issued launcher transfer can be attached only to a
    /// declared frontend command. Native commands retain their existing
    /// manifest-locked worker path and never receive this frontend handoff.
    pub fn ensure_frontend_command(&self, plugin_id: &str, command_id: &str) -> Result<(), String> {
        if !is_valid_identifier(plugin_id) || !is_valid_identifier(command_id) {
            return Err(
                "Plugin and command IDs must contain only letters, digits, '.', '_' or '-'"
                    .to_owned(),
            );
        }
        self.ensure_plugin_enabled(plugin_id)?;
        let plugin_root = self.resolve_plugin_root(plugin_id)?;
        let manifest_path = canonical_manifest_path(&plugin_root)?;
        let manifest = read_manifest(&manifest_path)?;
        validate_manifest(&manifest)?;
        if manifest.id != plugin_id {
            return Err(format!("Plugin manifest ID does not match '{plugin_id}'."));
        }
        let command = declared_commands(&manifest)
            .iter()
            .find(|command| command.id == command_id)
            .ok_or_else(|| {
                format!("Plugin '{plugin_id}' does not expose command '{command_id}'.")
            })?;
        if command_execution(&manifest, command) != CommandExecution::Frontend {
            return Err(format!(
                "Launcher context may be attached only to a frontend command; '{plugin_id}/{command_id}' is native."
            ));
        }
        Ok(())
    }

    /// Public uTools packages register their available entry features in
    /// `plugin.json`, not by calling iHub's SDK at runtime. The fixed shim
    /// still calls `lifecycle.ready`, but declared features may then be
    /// dispatched without a second dynamic registration step. No ordinary
    /// iHub plugin can use this path.
    pub fn uses_utools_compatibility(&self, plugin_id: &str) -> Result<bool, String> {
        if !is_valid_identifier(plugin_id) {
            return Err("Plugin ID must contain only letters, digits, '.', '_' or '-'.".to_owned());
        }
        self.ensure_plugin_enabled(plugin_id)?;
        let plugin_root = self.resolve_plugin_root(plugin_id)?;
        let manifest_path = canonical_manifest_path(&plugin_root)?;
        let manifest = read_manifest(&manifest_path)?;
        validate_manifest(&manifest)?;
        if manifest.id != plugin_id {
            return Err(format!("Plugin manifest ID does not match '{plugin_id}'."));
        }
        Ok(manifest.compatibility.is_utools())
    }

    /// Returns whether a manifest-declared setting is secret. Secret settings
    /// are deliberately handled by the host's process-local map rather than
    /// its JSON settings file, so a frontend cannot opt into that behavior
    /// without an auditable manifest declaration.
    pub fn is_secret_setting(&self, plugin_id: &str, key: &str) -> Result<bool, String> {
        if !is_valid_identifier(plugin_id) {
            return Err("Plugin ID must contain only letters, digits, '.', '_' or '-'.".to_owned());
        }
        if !is_valid_setting_key(key) {
            return Err(
                "Plugin setting keys must start with an ASCII letter and contain only letters, digits, '.', '_' or '-'."
                    .to_owned(),
            );
        }

        self.ensure_plugin_enabled(plugin_id)?;
        let plugin_root = self.resolve_plugin_root(plugin_id)?;
        let manifest_path = canonical_manifest_path(&plugin_root)?;
        let manifest = read_manifest(&manifest_path)?;
        validate_manifest(&manifest)?;
        if manifest.id != plugin_id {
            return Err(format!("Plugin manifest ID does not match '{plugin_id}'."));
        }

        Ok(declared_settings(&manifest)
            .iter()
            .any(|setting| setting.key == key && setting.secret))
    }

    /// Collects declared secret keys from every readable installed or linked
    /// plugin, including disabled plugins. App startup uses this to scrub a
    /// legacy plaintext value before any frontend can read it.
    pub fn declared_secret_setting_keys(&self) -> Vec<(String, String)> {
        self.list()
            .into_iter()
            .flat_map(|plugin| {
                self.secret_setting_keys_for_plugin(&plugin.id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(move |key| (plugin.id.clone(), key))
            })
            .collect()
    }

    /// Search providers are surfaced from a manifest before their iframe is
    /// activated. Require a runtime registration to match that declaration so
    /// one plugin cannot silently add unadvertised launcher providers.
    pub fn has_declared_search_provider(
        &self,
        plugin_id: &str,
        provider_id: &str,
    ) -> Result<bool, String> {
        if !is_valid_identifier(plugin_id) || !is_valid_identifier(provider_id) {
            return Ok(false);
        }

        self.ensure_plugin_enabled(plugin_id)?;
        let plugin_root = self.resolve_plugin_root(plugin_id)?;
        let manifest_path = canonical_manifest_path(&plugin_root)?;
        let manifest = read_manifest(&manifest_path)?;
        validate_manifest(&manifest)?;
        if manifest.id != plugin_id {
            return Err(format!("Plugin manifest ID does not match '{plugin_id}'."));
        }

        Ok(declared_search_providers(&manifest)
            .iter()
            .any(|provider| provider.id == provider_id)
            || (provider_id == UTOOLS_MAIN_PUSH_PROVIDER_ID
                && manifest.compatibility.is_utools()
                && manifest
                    .utools_commands
                    .iter()
                    .any(|command| command.main_push)))
    }

    fn secret_setting_keys_for_plugin(&self, plugin_id: &str) -> Result<Vec<String>, String> {
        let plugin_root = self.resolve_plugin_root(plugin_id)?;
        let manifest_path = canonical_manifest_path(&plugin_root)?;
        let manifest = read_manifest(&manifest_path)?;
        validate_manifest(&manifest)?;
        if manifest.id != plugin_id {
            return Err(format!("Plugin manifest ID does not match '{plugin_id}'."));
        }
        Ok(declared_settings(&manifest)
            .iter()
            .filter(|setting| setting.secret)
            .map(|setting| setting.key.clone())
            .collect())
    }

    fn ensure_root(&self) -> Result<(), String> {
        fs::create_dir_all(self.root.as_ref())
            .map_err(|error| format!("Could not create the plugin directory: {error}"))
    }

    fn resolve_plugin_root(&self, plugin_id: &str) -> Result<PathBuf, String> {
        if !is_valid_identifier(plugin_id) {
            return Err("Plugin ID must contain only letters, digits, '.', '_' or '-'.".to_owned());
        }
        self.ensure_root()?;

        if let Some(link) = self.read_local_links()?.links.get(plugin_id) {
            return match self.resolve_local_link_root(plugin_id, link) {
                Ok(local_root) => Ok(local_root),
                Err(local_error) => self
                    .resolve_managed_plugin_root(plugin_id)
                    .map_err(|snapshot_error| {
                        format!(
                            "Local development link for plugin '{plugin_id}' is stale: {local_error} No usable managed snapshot is available: {snapshot_error} Unlink the stale source from Plugin Center before installing another copy."
                        )
                    }),
            };
        }

        self.resolve_managed_plugin_root(plugin_id)
    }

    /// Resolves the host-owned snapshot without consulting a same-ID local
    /// development link. This is shared by the normal managed path and the
    /// stale-link fallback, and always repeats package-boundary and integrity
    /// validation before a WebView or child process receives a path.
    fn resolve_managed_plugin_root(&self, plugin_id: &str) -> Result<PathBuf, String> {
        if !is_valid_identifier(plugin_id) {
            return Err("Plugin ID must contain only letters, digits, '.', '_' or '-'.".to_owned());
        }
        self.ensure_root()?;
        let storage_root = self.root.as_ref().canonicalize().map_err(|error| {
            format!(
                "Could not resolve iHub's plugin directory {}: {error}",
                self.root.display()
            )
        })?;
        let plugin_root = storage_root
            .join(plugin_id)
            .canonicalize()
            .map_err(|error| {
                format!("Plugin '{plugin_id}' is not installed or cannot be resolved: {error}")
            })?;
        if !plugin_root.is_dir() {
            return Err(format!("Plugin '{plugin_id}' is not installed."));
        }
        ensure_path_within(&plugin_root, &storage_root, "Plugin root")?;
        self.validate_plugin_root(plugin_id, &plugin_root)?;
        self.verify_managed_snapshot_integrity(&plugin_root)?;
        Ok(plugin_root)
    }

    /// Git-installed plugins have a host-owned source lock. Newer locks also
    /// carry SHA-256 values for the manifest, served frontend bundle, and any
    /// declared native workers. Verify that immutable snapshot before handing
    /// its paths to a WebView or child process. Development links are never
    /// checked here because their purpose is to reflect the developer's next
    /// local build immediately.
    fn verify_managed_snapshot_integrity(&self, plugin_root: &Path) -> Result<(), String> {
        let lock_path = plugin_root.join(SOURCE_LOCK);
        if !lock_path.exists() {
            return Ok(());
        }
        let metadata = read_source_metadata(plugin_root)?;
        if let Some(lock) = metadata.lock {
            verify_snapshot_integrity(plugin_root, &lock)?;
        }
        Ok(())
    }

    /// Resolves only a managed installed snapshot. A local development link
    /// intentionally takes precedence at runtime, so attempting to update the
    /// hidden managed copy would be surprising and could make source-lock UI
    /// lie about what the user is actually running.
    fn managed_source_lock(&self, plugin_id: &str) -> Result<(PathBuf, PluginSourceLock), String> {
        if !is_valid_identifier(plugin_id) {
            return Err("Plugin ID must contain only letters, digits, '.', '_' or '-'.".to_owned());
        }
        if self.read_local_links()?.links.contains_key(plugin_id) {
            return Err(format!(
                "Plugin '{plugin_id}' is linked from a local development directory. Unlink it before checking or applying Git updates."
            ));
        }

        self.ensure_root()?;
        let storage_root = self.root.as_ref().canonicalize().map_err(|error| {
            format!(
                "Could not resolve iHub's plugin directory {}: {error}",
                self.root.display()
            )
        })?;
        let plugin_root = storage_root
            .join(plugin_id)
            .canonicalize()
            .map_err(|error| {
                format!("Plugin '{plugin_id}' is not installed or cannot be resolved: {error}")
            })?;
        if !plugin_root.is_dir() {
            return Err(format!("Plugin '{plugin_id}' is not installed."));
        }
        ensure_path_within(&plugin_root, &storage_root, "Plugin root")?;
        self.validate_plugin_root(plugin_id, &plugin_root)?;

        if !plugin_root.join(SOURCE_LOCK).is_file() {
            return Err(format!(
                "Plugin '{plugin_id}' has no immutable Git source lock. Re-import it before checking for updates."
            ));
        }
        let metadata = read_source_metadata(&plugin_root)?;
        let source_lock = metadata.lock.ok_or_else(|| {
            format!(
                "Plugin '{plugin_id}' has legacy source metadata without a saved ref. Re-import it before checking for updates."
            )
        })?;
        verify_snapshot_integrity(&plugin_root, &source_lock)?;
        Ok((plugin_root, source_lock))
    }

    fn resolve_local_link_root(
        &self,
        plugin_id: &str,
        link: &LocalPluginLink,
    ) -> Result<PathBuf, String> {
        if !is_valid_identifier(plugin_id) {
            return Err("Local development link has an invalid plugin ID.".to_owned());
        }
        let local_root = PathBuf::from(&link.canonical_path)
            .canonicalize()
            .map_err(|error| {
                format!(
                    "Could not resolve local development plugin '{plugin_id}' at '{}': {error}",
                    link.canonical_path
                )
            })?;
        if !local_root.is_dir() {
            return Err(format!(
                "Local development plugin '{plugin_id}' no longer points to a directory."
            ));
        }
        self.validate_plugin_root(plugin_id, &local_root)?;
        Ok(local_root)
    }

    fn validate_plugin_root(&self, plugin_id: &str, plugin_root: &Path) -> Result<(), String> {
        let manifest_path = canonical_manifest_path(plugin_root)?;
        let manifest = read_manifest(&manifest_path)?;
        validate_manifest(&manifest)?;
        if manifest.id != plugin_id {
            return Err(format!("Plugin manifest ID does not match '{plugin_id}'."));
        }
        let package_root = manifest_path
            .parent()
            .ok_or_else(|| format!("Plugin '{plugin_id}' has an invalid manifest path."))?;
        ensure_path_within(package_root, plugin_root, "Plugin package")?;
        load_manifest_artwork(package_root, &manifest)?;
        Ok(())
    }

    fn read_plugin_info_for_id_with_lifecycle(
        &self,
        plugin_id: &str,
        lifecycle: &PluginLifecycleStore,
    ) -> Result<PluginInfo, String> {
        let links = self.read_local_links()?;
        if let Some(link) = links.links.get(plugin_id) {
            return Ok(self
                .read_linked_plugin_info_with_lifecycle(plugin_id, link, lifecycle)
                .unwrap_or_else(|local_error| {
                    self.read_stale_link_plugin_info_with_lifecycle(
                        plugin_id,
                        link,
                        lifecycle,
                        &local_error,
                    )
                }));
        }
        let plugin_root = self.resolve_plugin_root(plugin_id)?;
        self.read_plugin_info_with_lifecycle(&plugin_root, lifecycle)
    }

    fn read_lifecycle_store(&self) -> Result<PluginLifecycleStore, String> {
        let path = self.root.join(LIFECYCLE_RECORD);
        if !path.exists() {
            return Ok(PluginLifecycleStore {
                schema_version: LIFECYCLE_SCHEMA_VERSION,
                ..PluginLifecycleStore::default()
            });
        }
        if !path.is_file() {
            return Err(format!(
                "Plugin lifecycle state is not a file: {}",
                path.display()
            ));
        }
        let text = fs::read_to_string(&path)
            .map_err(|error| format!("Could not read plugin lifecycle state: {error}"))?;
        let store: PluginLifecycleStore = serde_json::from_str(&text)
            .map_err(|error| format!("Invalid plugin lifecycle state: {error}"))?;
        if store.schema_version != LIFECYCLE_SCHEMA_VERSION {
            return Err(format!(
                "Unsupported plugin lifecycle state schema version {}.",
                store.schema_version
            ));
        }
        if let Some(invalid) = store
            .plugins
            .keys()
            .find(|plugin_id| !is_valid_identifier(plugin_id))
        {
            return Err(format!(
                "Plugin lifecycle state contains an invalid plugin ID '{invalid}'."
            ));
        }
        Ok(store)
    }

    fn write_lifecycle_store(&self, store: &PluginLifecycleStore) -> Result<(), String> {
        let mut normalized = store.clone();
        normalized.schema_version = LIFECYCLE_SCHEMA_VERSION;
        let target = self.root.join(LIFECYCLE_RECORD);
        let staging = self
            .root
            .join(format!(".ihub-plugin-lifecycle-{}.tmp", unique_suffix()));
        let serialized = serde_json::to_vec_pretty(&normalized)
            .map_err(|error| format!("Could not serialize plugin lifecycle state: {error}"))?;
        fs::write(&staging, serialized)
            .map_err(|error| format!("Could not stage plugin lifecycle state: {error}"))?;

        let backup = self
            .root
            .join(format!(".ihub-plugin-lifecycle-{}.backup", unique_suffix()));
        let had_existing = target.exists();
        if had_existing {
            fs::rename(&target, &backup).map_err(|error| {
                let _ = fs::remove_file(&staging);
                format!("Could not prepare plugin lifecycle state for update: {error}")
            })?;
        }
        if let Err(error) = fs::rename(&staging, &target) {
            if had_existing {
                let _ = fs::rename(&backup, &target);
            }
            let _ = fs::remove_file(&staging);
            return Err(format!("Could not save plugin lifecycle state: {error}"));
        }
        if had_existing {
            let _ = fs::remove_file(&backup);
        }
        Ok(())
    }

    fn read_local_links(&self) -> Result<LocalPluginLinks, String> {
        let path = self.root.join(LOCAL_LINKS_RECORD);
        if !path.exists() {
            return Ok(LocalPluginLinks::default());
        }
        if !path.is_file() {
            return Err(format!(
                "Local development link record is not a file: {}",
                path.display()
            ));
        }
        let text = fs::read_to_string(&path)
            .map_err(|error| format!("Could not read local development link record: {error}"))?;
        serde_json::from_str(&text)
            .map_err(|error| format!("Invalid local development link record: {error}"))
    }

    fn write_local_links(&self, links: &LocalPluginLinks) -> Result<(), String> {
        let target = self.root.join(LOCAL_LINKS_RECORD);
        let staging = self
            .root
            .join(format!(".ihub-local-links-{}.tmp", unique_suffix()));
        let serialized = serde_json::to_vec_pretty(links)
            .map_err(|error| format!("Could not serialize local development links: {error}"))?;
        fs::write(&staging, serialized)
            .map_err(|error| format!("Could not stage local development links: {error}"))?;

        let backup = self
            .root
            .join(format!(".ihub-local-links-{}.backup", unique_suffix()));
        let had_existing = target.exists();
        if had_existing {
            fs::rename(&target, &backup).map_err(|error| {
                let _ = fs::remove_file(&staging);
                format!("Could not prepare local development links for update: {error}")
            })?;
        }
        if let Err(error) = fs::rename(&staging, &target) {
            if had_existing {
                let _ = fs::rename(&backup, &target);
            }
            let _ = fs::remove_file(&staging);
            return Err(format!("Could not save local development links: {error}"));
        }
        if had_existing {
            let _ = fs::remove_file(&backup);
        }
        Ok(())
    }

    fn read_linked_plugin_info(
        &self,
        plugin_id: &str,
        link: &LocalPluginLink,
    ) -> Result<PluginInfo, String> {
        let lifecycle = self.read_lifecycle_store()?;
        self.read_linked_plugin_info_with_lifecycle(plugin_id, link, &lifecycle)
    }

    fn read_linked_plugin_info_with_lifecycle(
        &self,
        plugin_id: &str,
        link: &LocalPluginLink,
        lifecycle: &PluginLifecycleStore,
    ) -> Result<PluginInfo, String> {
        let local_root = self.resolve_local_link_root(plugin_id, link)?;
        let mut plugin = self.read_plugin_info_with_lifecycle(&local_root, lifecycle)?;
        if plugin.id != plugin_id {
            return Err(format!("Plugin manifest ID does not match '{plugin_id}'."));
        }
        plugin.source = Some(format!("local:{}", local_root.display()));
        plugin.commit = None;
        plugin.installed_at = Some(link.linked_at.clone());
        plugin.source_lock = None;
        plugin.is_development_link = true;
        plugin.local_link_status = Some("active".to_owned());
        plugin.local_link_error = None;
        plugin.uses_managed_snapshot_fallback = false;
        plugin.local_path = Some(local_root.to_string_lossy().into_owned());
        Ok(plugin)
    }

    /// Returns a management projection for a local-link record whose source
    /// can no longer be resolved. A valid managed snapshot supplies the
    /// executable metadata; otherwise cached, non-executable display metadata
    /// keeps the broken record visible until the user unlinks it.
    fn read_stale_link_plugin_info_with_lifecycle(
        &self,
        plugin_id: &str,
        link: &LocalPluginLink,
        lifecycle: &PluginLifecycleStore,
        local_error: &str,
    ) -> PluginInfo {
        let fallback = self
            .resolve_managed_plugin_root(plugin_id)
            .and_then(|plugin_root| self.read_plugin_info_with_lifecycle(&plugin_root, lifecycle));

        match fallback {
            Ok(mut plugin) => {
                plugin.is_development_link = true;
                plugin.local_link_status = Some("stale".to_owned());
                plugin.local_link_error = Some(format!(
                    "本地源码链接已失效：{local_error} 当前正安全回退到同 ID 的受管快照。"
                ));
                plugin.uses_managed_snapshot_fallback = true;
                plugin.local_path = Some(link.canonical_path.clone());
                plugin
            }
            Err(snapshot_error) => PluginInfo {
                id: plugin_id.to_owned(),
                name: link.name.clone().unwrap_or_else(|| plugin_id.to_owned()),
                version: link.version.clone().unwrap_or_else(|| "0.0.0".to_owned()),
                description: link.description.clone().or_else(|| {
                    Some("本地开发链接已失效；解除链接后可重新安装此插件。".to_owned())
                }),
                icon_src: None,
                source: Some(format!("local:{}", link.canonical_path)),
                commit: None,
                installed_at: Some(link.linked_at.clone()),
                source_lock: None,
                is_development_link: true,
                local_link_status: Some("stale".to_owned()),
                local_link_error: Some(format!(
                    "本地源码链接已失效：{local_error} 没有可用的受管快照：{snapshot_error}"
                )),
                uses_managed_snapshot_fallback: false,
                local_path: Some(link.canonical_path.clone()),
                frontend_entry: None,
                enabled: lifecycle.is_enabled(plugin_id),
                has_native_worker: false,
                update_channel: None,
                auto_update: false,
                command_count: 0,
                commands: Vec::new(),
                global_shortcuts: Vec::new(),
                search_providers: Vec::new(),
                launcher_context: None,
            },
        }
    }

    fn read_plugin_info(&self, directory: &Path) -> Result<PluginInfo, String> {
        let lifecycle = self.read_lifecycle_store()?;
        self.read_plugin_info_with_lifecycle(directory, &lifecycle)
    }

    fn read_plugin_info_with_lifecycle(
        &self,
        directory: &Path,
        lifecycle: &PluginLifecycleStore,
    ) -> Result<PluginInfo, String> {
        self.read_plugin_info_projection(directory, lifecycle, true)
    }

    fn read_plugin_info_without_artwork_with_lifecycle(
        &self,
        directory: &Path,
        lifecycle: &PluginLifecycleStore,
    ) -> Result<PluginInfo, String> {
        self.read_plugin_info_projection(directory, lifecycle, false)
    }

    fn read_plugin_info_projection(
        &self,
        directory: &Path,
        lifecycle: &PluginLifecycleStore,
        include_artwork: bool,
    ) -> Result<PluginInfo, String> {
        let manifest_path = find_manifest(directory)
            .ok_or_else(|| format!("{} has no plugin manifest", directory.display()))?;
        let manifest = read_manifest(&manifest_path)?;
        validate_manifest(&manifest)?;
        let package_root = manifest_path
            .parent()
            .ok_or_else(|| "Plugin manifest has no package directory.".to_owned())?;
        let artwork = if include_artwork {
            load_manifest_artwork(package_root, &manifest)?
        } else {
            BTreeMap::new()
        };
        let icon_src = manifest_artwork_path(&manifest)
            .and_then(|path| artwork.get(path))
            .map(|artwork| artwork.data_url.clone());
        let mut projected_artwork_bytes = icon_src.as_ref().map_or(0, String::len);
        let source = read_source_metadata(directory).ok();
        let commands = declared_commands(&manifest)
            .iter()
            .map(|command| {
                let icon_src = command
                    .icon
                    .as_deref()
                    .and_then(|path| artwork.get(path))
                    .and_then(|artwork| {
                        let next_bytes =
                            projected_artwork_bytes.saturating_add(artwork.data_url.len());
                        if next_bytes > MAX_PROJECTED_ARTWORK_DATA_URL_BYTES {
                            return None;
                        }
                        projected_artwork_bytes = next_bytes;
                        Some(artwork.data_url.clone())
                    });
                PluginCommandInfo {
                    id: command.id.clone(),
                    name: command_display_name(command),
                    description: command
                        .description
                        .clone()
                        .or_else(|| command.subtitle.clone()),
                    icon_src,
                    execution: command_execution(&manifest, command).as_str().to_owned(),
                    keywords: normalized_shortcut_keywords(&command.keywords),
                    shortcut: command
                        .shortcut
                        .as_deref()
                        .and_then(|shortcut| normalize_plugin_hotkey(shortcut).ok()),
                    shortcut_registration: command.shortcut.as_ref().map(|_| "inactive".to_owned()),
                    shortcut_error: command
                        .shortcut
                        .as_ref()
                        .map(|_| "插件快捷键尚未由驻留宿主注册。".to_owned()),
                }
            })
            .collect::<Vec<_>>();
        let has_native_worker = manifest_has_native_worker(&manifest);
        let frontend_entry = manifest_frontend_entry(&manifest);
        let mut search_providers = declared_search_providers(&manifest)
            .iter()
            .map(|provider| PluginSearchProviderInfo {
                id: provider.id.clone(),
                title: provider.title.clone(),
                trigger: provider.trigger.clone(),
                priority: provider.priority,
            })
            .collect::<Vec<_>>();
        if manifest.compatibility.is_utools()
            && manifest
                .utools_commands
                .iter()
                .any(|command| command.main_push)
        {
            search_providers.push(PluginSearchProviderInfo {
                id: UTOOLS_MAIN_PUSH_PROVIDER_ID.to_owned(),
                title: "uTools 主搜索推送".to_owned(),
                trigger: None,
                priority: Some(20),
            });
        }
        let launcher_context = manifest
            .permissions
            .launcher_context
            .as_ref()
            .map(|permissions| PluginLauncherContextPermissionsInfo {
                text: permissions.text,
                files: permissions.files,
                image: permissions.image,
            });
        let global_shortcuts = declared_global_shortcuts(&manifest)
            .iter()
            .filter_map(|shortcut| {
                Some(PluginGlobalShortcutInfo {
                    id: shortcut.id.clone(),
                    shortcut: normalize_plugin_hotkey(&shortcut.shortcut).ok()?,
                    command_id: shortcut.command_id.clone(),
                    keyword: shortcut.keyword.clone(),
                    registration: "inactive".to_owned(),
                    error: Some("插件快捷键尚未由驻留宿主注册。".to_owned()),
                })
            })
            .collect::<Vec<_>>();
        let plugin_id = manifest.id.clone();
        Ok(PluginInfo {
            id: plugin_id.clone(),
            name: manifest.name,
            version: manifest.version,
            description: manifest.description,
            icon_src,
            source: source.as_ref().map(|record| record.source.clone()),
            commit: source
                .as_ref()
                .and_then(|record| record.resolved_commit.clone()),
            installed_at: source.as_ref().map(|record| record.installed_at.clone()),
            source_lock: source.and_then(|record| record.lock),
            is_development_link: false,
            local_link_status: None,
            local_link_error: None,
            uses_managed_snapshot_fallback: false,
            local_path: None,
            frontend_entry,
            enabled: lifecycle.is_enabled(&plugin_id),
            has_native_worker,
            update_channel: manifest.update.channel.clone(),
            auto_update: manifest.update.auto_update,
            command_count: commands.len(),
            commands,
            global_shortcuts,
            search_providers,
            launcher_context,
        })
    }
}

/// The automatic pass is deliberately conservative: it only contacts the
/// official HTTPS namespace, only when the installed manifest opted into the
/// stable channel, and only for a managed immutable Git snapshot. The caller
/// still has to confirm every replacement, including frontend-only packages.
fn automatic_update_skip_reason(plugin: &PluginInfo) -> Option<String> {
    if plugin.is_development_link {
        return Some("Local development links are never checked automatically.".to_owned());
    }
    if !plugin.enabled {
        return Some(
            "Disabled plugins are never checked automatically; enable it or use a manual review first."
                .to_owned(),
        );
    }
    if !plugin.auto_update {
        return Some("The installed manifest did not opt into automatic update checks.".to_owned());
    }
    if plugin.update_channel.as_deref() != Some("stable") {
        return Some(
            "Only plugins on the stable update channel are checked automatically.".to_owned(),
        );
    }
    let Some(source_lock) = plugin.source_lock.as_ref() else {
        return Some(
            "This plugin has no immutable Git source lock; re-import it before checking updates."
                .to_owned(),
        );
    };
    if !is_trusted_official_auto_update_source(&source_lock.source) {
        return Some("Automatic checks are limited to the trusted official GitHub namespace; use Check update for this source.".to_owned());
    }
    if source_lock.integrity.is_none() {
        return Some(
            "Automatic checks require a verified snapshot integrity record. Re-import this legacy Git snapshot before enabling automatic checks; manual Check update remains available."
                .to_owned(),
        );
    }
    None
}

/// Builds a truthful no-network report when a periodic pass deliberately
/// declines to wait for another automatic pass or a foreground mutation. A
/// plugin that is already ineligible keeps its more specific reason.
fn automatic_update_skip_report(
    plugins: Vec<PluginInfo>,
    eligible_reason: &str,
) -> PluginAutomaticUpdateReport {
    PluginAutomaticUpdateReport {
        checked_at: Utc::now().to_rfc3339(),
        checks: Vec::new(),
        skipped: plugins
            .into_iter()
            .map(|plugin| PluginAutomaticUpdateSkip {
                reason: automatic_update_skip_reason(&plugin)
                    .unwrap_or_else(|| eligible_reason.to_owned()),
                plugin_id: plugin.id,
            })
            .collect(),
    }
}

/// Do not normalize arbitrary Git URLs here. A source lock may be edited or
/// corrupted, so automatic discovery needs a deliberately narrow, auditable
/// publisher identity rather than a loose substring match such as
/// `neko233-com.example`.
fn is_trusted_official_auto_update_source(source: &str) -> bool {
    let Some(repository) = source.strip_prefix(OFFICIAL_GITHUB_AUTO_UPDATE_PREFIX) else {
        return false;
    };
    let repository = repository.strip_suffix(".git").unwrap_or(repository);
    !repository.is_empty()
        && repository
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn default_version() -> String {
    "0.0.0".to_owned()
}

fn default_lifecycle_schema_version() -> u32 {
    LIFECYCLE_SCHEMA_VERSION
}

fn default_plugin_root() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join("AppData/Local"))
            })
            .unwrap_or_else(env::temp_dir)
            .join("iHub/plugins")
    }
    #[cfg(target_os = "macos")]
    {
        env::var_os("HOME")
            .map(|home| PathBuf::from(home).join("Library/Application Support/iHub/plugins"))
            .unwrap_or_else(env::temp_dir)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
            .unwrap_or_else(env::temp_dir)
            .join("ihub/plugins")
    }
}

/// Parses the public import syntax without ever accepting a local filesystem
/// path or a clear-text/interactive transport. Existing `owner/repo`,
/// `github:owner/repo`, and HTTPS imports stay valid; callers can additionally
/// pin a branch or tag with `owner/repo@ref` or `https://…/repo.git#ref`.
fn parse_git_source(value: &str) -> Result<GitSource, String> {
    const DEFAULT_REF: &str = "HEAD";

    let value = value.trim();
    if value.is_empty() || value.chars().any(char::is_whitespace) || value.starts_with('-') {
        return Err("Enter a GitHub repository (owner/repo or a git URL).".to_owned());
    }

    let (remote_input, requested_ref, has_fragment) = match value.rsplit_once('#') {
        Some((remote, fragment)) => {
            if remote.is_empty() || fragment.is_empty() || remote.contains('#') {
                return Err(
                    "A Git source may contain at most one non-empty #ref fragment.".to_owned(),
                );
            }
            (remote, fragment, true)
        }
        None => (value, DEFAULT_REF, false),
    };

    let github_shorthand = remote_input.strip_prefix("github:").unwrap_or(remote_input);
    if let Some((repository, requested_ref_from_suffix)) = github_shorthand.rsplit_once('@') {
        if is_github_shorthand(repository) {
            if has_fragment {
                return Err(
                    "Choose either owner/repo@ref or a URL#ref fragment, not both.".to_owned(),
                );
            }
            validate_requested_ref(requested_ref_from_suffix)?;
            return Ok(GitSource {
                remote: format!("https://github.com/{repository}.git"),
                requested_ref: requested_ref_from_suffix.to_owned(),
            });
        }
    }
    if is_github_shorthand(github_shorthand) {
        validate_requested_ref(requested_ref)?;
        return Ok(GitSource {
            remote: format!("https://github.com/{github_shorthand}.git"),
            requested_ref: requested_ref.to_owned(),
        });
    }

    if !remote_input.starts_with("https://") {
        return Err(
            "Only HTTPS Git URLs are accepted; HTTP, SSH, Git-shell, and local filesystem paths are not plugin sources."
                .to_owned(),
        );
    }
    let parsed_remote =
        Url::parse(remote_input).map_err(|_| "Enter a valid absolute HTTPS Git URL.".to_owned())?;
    if parsed_remote.scheme() != "https" || parsed_remote.host_str().is_none() {
        return Err("Enter a valid absolute HTTPS Git URL.".to_owned());
    }
    let authority = remote_input
        .strip_prefix("https://")
        .and_then(|value| value.split('/').next())
        .unwrap_or_default();
    if !parsed_remote.username().is_empty()
        || parsed_remote.password().is_some()
        || authority.contains('@')
    {
        return Err(
            "Git URLs must not embed a username, password, or access token. Import from a credential-free public HTTPS URL."
                .to_owned(),
        );
    }
    if parsed_remote.query().is_some() {
        return Err(
            "Git URLs with query parameters are not accepted because they may persist credentials in plugin provenance."
                .to_owned(),
        );
    }
    validate_requested_ref(requested_ref)?;
    Ok(GitSource {
        remote: remote_input.to_owned(),
        requested_ref: requested_ref.to_owned(),
    })
}

/// Re-validates provenance read from disk before it is handed back to Git.
/// Source locks are host-owned metadata, but treating them as untrusted keeps
/// an edited/corrupt lock from broadening the importer surface during refresh.
fn git_source_from_lock(lock: &PluginSourceLock) -> Result<GitSource, String> {
    match parse_git_source(&format!("{}#{}", lock.source, lock.requested_ref)) {
        Ok(source) => Ok(source),
        Err(error) => {
            // Unit tests exercise the private installer against a local bare
            // repository. Production import syntax never permits local paths,
            // and this cfg-only branch cannot be compiled into the app.
            #[cfg(test)]
            if Path::new(&lock.source).is_absolute() {
                validate_requested_ref(&lock.requested_ref)?;
                return Ok(GitSource {
                    remote: lock.source.clone(),
                    requested_ref: lock.requested_ref.clone(),
                });
            }
            Err(format!(
                "The saved source lock is not a safe Git source: {error} Re-import the plugin from a credential-free HTTPS URL."
            ))
        }
    }
}

fn validate_requested_ref(value: &str) -> Result<(), String> {
    if value == "HEAD" {
        return Ok(());
    }
    let invalid = value.is_empty()
        || value.starts_with('-')
        || value.starts_with('/')
        || value.ends_with('/')
        || value.ends_with('.')
        || value.contains("..")
        || value.contains("@{")
        || value.contains("//")
        || value.chars().any(|character| {
            character.is_whitespace()
                || character.is_control()
                || matches!(character, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
        })
        || value.split('/').any(|component| {
            component.is_empty() || component.starts_with('.') || component.ends_with(".lock")
        });
    if invalid {
        return Err(
            "Git ref must be a non-empty branch, tag, or commit-like ref without spaces or Git control characters."
                .to_owned(),
        );
    }
    Ok(())
}

/// Resolves the requested remote ref before creating the staging checkout.
/// For annotated tags the peeled commit (`^{}`) is preferred over the tag
/// object ID, which is the value callers need to reproduce the checkout.
fn resolve_remote_commit(remote: &str, requested_ref: &str) -> Result<String, String> {
    resolve_remote_commit_with_timeout(remote, requested_ref, PLUGIN_GIT_TIMEOUT)
}

/// Same resolution primitive as a manual update check, with a caller-owned
/// timeout so the periodic discovery pass can share one small global network
/// budget instead of serially consuming the full manual timeout per plugin.
fn resolve_remote_commit_with_timeout(
    remote: &str,
    requested_ref: &str,
    timeout: Duration,
) -> Result<String, String> {
    resolve_remote_commit_with_transport(
        remote,
        requested_ref,
        timeout,
        GitTransportPolicy::ImportedSource,
    )
}

/// Automatic discovery is stricter than explicit import/check: it only
/// accepts the exact official HTTPS namespace and Git itself is prevented
/// from selecting file, SSH, or custom helper transports. This is in addition
/// to clearing inherited Git configuration so a user's `insteadOf` rule
/// cannot rewrite the canonical URL to another endpoint.
fn resolve_official_auto_update_commit_with_timeout(
    remote: &str,
    requested_ref: &str,
    timeout: Duration,
) -> Result<String, String> {
    if !is_trusted_official_auto_update_source(remote) {
        return Err(
            "Automatic update discovery only accepts the canonical official HTTPS GitHub namespace."
                .to_owned(),
        );
    }
    resolve_remote_commit_with_transport(
        remote,
        requested_ref,
        timeout,
        GitTransportPolicy::OfficialHttps,
    )
}

#[derive(Clone, Copy)]
enum GitTransportPolicy {
    /// Explicit imports and manual checks retain their documented remote URL
    /// compatibility. Configuration is still scrubbed before every Git call.
    ImportedSource,
    /// Automatic availability discovery may only contact the hard-coded
    /// official HTTPS namespace.
    OfficialHttps,
}

fn resolve_remote_commit_with_transport(
    remote: &str,
    requested_ref: &str,
    timeout: Duration,
    transport: GitTransportPolicy,
) -> Result<String, String> {
    let mut command = git_command_with_transport(transport);
    command.args(["ls-remote", "--quiet", remote]);
    let output = run_git_command_with_timeout(command, "remote ref resolution", timeout)?;
    if !output.status.success() {
        return Err(format!(
            "Could not resolve remote Git ref '{requested_ref}': {}",
            readable_output(&output.stderr)
        ));
    }

    let references = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let (commit, name) = line.split_once('\t')?;
            is_git_object_id(commit).then(|| (commit.to_owned(), name.to_owned()))
        })
        .collect::<Vec<_>>();
    let candidate_names = remote_ref_candidates(requested_ref);
    for candidate in candidate_names {
        if let Some((commit, _)) = references.iter().find(|(_, name)| name == &candidate) {
            return Ok(commit.clone());
        }
    }

    // A full object ID is occasionally used as an immutable requested ref.
    // It must still be advertised by the remote; this deliberately avoids
    // accepting an arbitrary unverified SHA string.
    if is_git_object_id(requested_ref)
        && references
            .iter()
            .any(|(commit, _)| commit.eq_ignore_ascii_case(requested_ref))
    {
        return Ok(requested_ref.to_ascii_lowercase());
    }

    Err(format!(
        "The remote does not expose requested Git ref '{requested_ref}'."
    ))
}

fn remote_ref_candidates(requested_ref: &str) -> Vec<String> {
    if requested_ref == "HEAD" {
        return vec!["HEAD".to_owned()];
    }
    if let Some(tag) = requested_ref.strip_prefix("refs/tags/") {
        return vec![format!("refs/tags/{tag}^{{}}"), format!("refs/tags/{tag}")];
    }
    if requested_ref.starts_with("refs/") {
        return vec![format!("{requested_ref}^{{}}"), requested_ref.to_owned()];
    }
    vec![
        format!("refs/heads/{requested_ref}"),
        format!("refs/tags/{requested_ref}^{{}}"),
        format!("refs/tags/{requested_ref}"),
    ]
}

fn checkout_remote_ref(
    remote: &str,
    requested_ref: &str,
    resolved_commit: &str,
    staging: &Path,
) -> Result<(), String> {
    let mut init_command = git_command();
    init_command.args(["init", "--quiet"]).arg(staging);
    let init = run_git_command(init_command, "staging repository initialization")?;
    if !init.status.success() {
        return Err(format!(
            "Could not create Git staging repository: {}",
            readable_output(&init.stderr)
        ));
    }

    let add_remote = git_in(staging, &["remote", "add", "origin", remote])?;
    if !add_remote.status.success() {
        return Err(format!(
            "Could not configure Git source: {}",
            readable_output(&add_remote.stderr)
        ));
    }

    let fetch = git_in(
        staging,
        &["fetch", "--quiet", "--depth", "1", "origin", requested_ref],
    )?;
    if !fetch.status.success() {
        return Err(format!(
            "Could not fetch Git ref '{requested_ref}': {}",
            readable_output(&fetch.stderr)
        ));
    }
    let fetched_commit = git_commit_at(staging, "FETCH_HEAD")?;
    if !fetched_commit.eq_ignore_ascii_case(resolved_commit) {
        return Err(format!(
            "Git ref '{requested_ref}' changed during import (resolved {resolved_commit}, fetched {fetched_commit}). Retry after reviewing the new commit."
        ));
    }

    let checkout = git_in(staging, &["checkout", "--quiet", "--detach", "FETCH_HEAD"])?;
    if !checkout.status.success() {
        return Err(format!(
            "Could not check out resolved Git commit: {}",
            readable_output(&checkout.stderr)
        ));
    }
    let checked_out = git_revision(staging)
        .ok_or_else(|| "Could not read the checked out Git commit.".to_owned())?;
    if !checked_out.eq_ignore_ascii_case(resolved_commit) {
        return Err(format!(
            "Git checkout did not match the resolved commit (expected {resolved_commit}, got {checked_out})."
        ));
    }
    Ok(())
}

fn git_in(directory: &Path, arguments: &[&str]) -> Result<GitCommandOutput, String> {
    let mut command = git_command();
    command.arg("-C").arg(directory).args(arguments);
    run_git_command(command, "managed plugin Git operation")
}

fn git_commit_at(directory: &Path, revision: &str) -> Result<String, String> {
    let output = git_in(directory, &["rev-parse", &format!("{revision}^{{commit}}")])?;
    if !output.status.success() {
        return Err(format!(
            "Could not resolve fetched Git commit: {}",
            readable_output(&output.stderr)
        ));
    }
    let commit = readable_output(&output.stdout);
    if !is_git_object_id(&commit) {
        return Err("Git returned an invalid commit identifier.".to_owned());
    }
    Ok(commit)
}

fn is_git_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn git_revision(directory: &Path) -> Option<String> {
    let output = git_in(directory, &["rev-parse", "HEAD"]).ok()?;
    output
        .status
        .success()
        .then(|| readable_output(&output.stdout))
        .filter(|revision| !revision.is_empty())
}

/// Starts Git with no interactive credential prompt and no user/system Git
/// configuration. This prevents an update check from waiting for terminal
/// input or inheriting global filter configuration while reading a repository
/// controlled by a plugin author.
fn git_command() -> Command {
    git_command_with_transport(GitTransportPolicy::ImportedSource)
}

fn git_command_with_transport(transport: GitTransportPolicy) -> Command {
    let mut command = background_command("git");
    configure_git_command_environment(&mut command, transport, env::vars_os());
    command
}

/// Git accepts `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n`
/// and `GIT_CONFIG_PARAMETERS` as command-line configuration injection. A
/// process-wide `url.*.insteadOf` supplied through those variables can rewrite
/// an innocent looking canonical GitHub URL before transport policy applies.
/// Remove *every* inherited `GIT_CONFIG_*` key, not merely the currently
/// documented numbered names, and remove askpass/SSH/proxy helper variables
/// that could execute an inherited program even with terminal prompting off.
/// Then install the host's own non-interactive config policy.
fn configure_git_command_environment<I>(
    command: &mut Command,
    transport: GitTransportPolicy,
    inherited: I,
) where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    for (key, _) in inherited {
        if is_git_config_environment_key(&key) || is_git_external_helper_environment_key(&key) {
            command.env_remove(key);
        }
    }
    // Repository/worktree overrides could make an otherwise isolated staging
    // command read configuration or objects from an attacker-selected path.
    // The staging directory is always passed explicitly via `git -C`.
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_EXEC_PATH",
        "GIT_TEMPLATE_DIR",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
        "GIT_ASKPASS",
        "SSH_ASKPASS",
        "SSH_ASKPASS_REQUIRE",
        "GIT_SSH",
        "GIT_SSH_COMMAND",
        "GIT_PROXY_COMMAND",
    ] {
        command.env_remove(key);
    }
    let allowed_protocols = match transport {
        GitTransportPolicy::ImportedSource => "file:git:http:https:ssh",
        GitTransportPolicy::OfficialHttps => "https",
    };
    command
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", git_null_device())
        .env("GIT_ALLOW_PROTOCOL", allowed_protocols)
        .env("GIT_PROTOCOL_FROM_USER", "0")
        .env("GIT_LFS_SKIP_SMUDGE", "1");
}

fn is_git_config_environment_key(key: &OsStr) -> bool {
    // Windows environment variable names are case-insensitive, so accepting
    // only an uppercase spelling here would leave `git_config_count` able to
    // inject the same configuration into the child process.
    key.to_string_lossy()
        .to_ascii_uppercase()
        .starts_with("GIT_CONFIG_")
}

fn is_git_external_helper_environment_key(key: &OsStr) -> bool {
    matches!(
        key.to_string_lossy().to_ascii_uppercase().as_str(),
        "GIT_ASKPASS"
            | "SSH_ASKPASS"
            | "SSH_ASKPASS_REQUIRE"
            | "GIT_SSH"
            | "GIT_SSH_COMMAND"
            | "GIT_PROXY_COMMAND"
    )
}

#[cfg(windows)]
fn git_null_device() -> &'static str {
    "NUL"
}

#[cfg(not(windows))]
fn git_null_device() -> &'static str {
    "/dev/null"
}

struct GitCommandOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

struct GitStreamOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

/// Runs a single Git plumbing operation with bounded runtime and output.
/// The helper only launches Git; it never evaluates a plugin script, package
/// manager hook, plugin binary, or frontend bundle.
fn run_git_command(command: Command, operation: &str) -> Result<GitCommandOutput, String> {
    run_git_command_with_timeout(command, operation, PLUGIN_GIT_TIMEOUT)
}

fn run_git_command_with_timeout(
    mut command: Command,
    operation: &str,
    timeout: Duration,
) -> Result<GitCommandOutput, String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Unable to start git. Install Git and retry: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Git stdout was not captured.".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Git stderr was not captured.".to_owned())?;
    let stdout_task = thread::spawn(move || read_git_output(stdout));
    let stderr_task = thread::spawn(move || read_git_output(stderr));
    let deadline = Instant::now() + timeout;

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                terminate_child(&mut child);
                drop(stdout_task);
                drop(stderr_task);
                return Err(format!(
                    "Git {operation} timed out after {} seconds.",
                    timeout.as_secs()
                ));
            }
            Ok(None) => thread::sleep(PLUGIN_GIT_POLL_INTERVAL),
            Err(error) => {
                terminate_child(&mut child);
                drop(stdout_task);
                drop(stderr_task);
                return Err(format!("Could not monitor Git {operation}: {error}"));
            }
        }
    };

    let stdout = stdout_task
        .join()
        .map_err(|_| format!("Git {operation} stdout reader stopped unexpectedly."))??;
    let stderr = stderr_task
        .join()
        .map_err(|_| format!("Git {operation} stderr reader stopped unexpectedly."))??;
    if stdout.truncated || stderr.truncated {
        return Err(format!(
            "Git {operation} exceeded the {} MiB diagnostic output limit.",
            MAX_GIT_OUTPUT_BYTES / (1024 * 1024)
        ));
    }
    Ok(GitCommandOutput {
        status,
        stdout: stdout.bytes,
        stderr: stderr.bytes,
    })
}

fn read_git_output<R: Read>(mut reader: R) -> Result<GitStreamOutput, String> {
    let mut bytes = Vec::with_capacity(MAX_GIT_OUTPUT_BYTES.min(16 * 1024));
    let mut buffer = [0_u8; 8 * 1024];
    let mut truncated = false;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        let remaining = MAX_GIT_OUTPUT_BYTES.saturating_sub(bytes.len());
        let keep = read.min(remaining);
        bytes.extend_from_slice(&buffer[..keep]);
        truncated |= keep < read;
    }
    Ok(GitStreamOutput { bytes, truncated })
}

fn is_github_shorthand(source: &str) -> bool {
    let mut segments = source.split('/');
    let Some(owner) = segments.next() else {
        return false;
    };
    let Some(repository) = segments.next() else {
        return false;
    };
    segments.next().is_none() && is_valid_identifier(owner) && is_valid_identifier(repository)
}

fn resolve_official_workspace_plugin(
    spec: &OfficialWorkspacePluginSpec,
) -> Result<(PathBuf, String), String> {
    let workspace_root = trusted_development_source_root()?;
    resolve_official_workspace_plugin_at(&workspace_root, spec)
}

fn resolve_official_workspace_plugin_at(
    workspace_root: &Path,
    spec: &OfficialWorkspacePluginSpec,
) -> Result<(PathBuf, String), String> {
    let workspace_root = workspace_root
        .canonicalize()
        .map_err(|error| format!("Could not resolve the iHub workspace root: {error}"))?;
    let official_root = workspace_root
        .join("plugins")
        .join("official")
        .canonicalize()
        .map_err(|error| {
            format!("The official plugin workspace is unavailable in this build: {error}")
        })?;
    ensure_path_within(&official_root, &workspace_root, "Official plugin workspace")?;

    let project_root = official_root
        .join(spec.directory)
        .canonicalize()
        .map_err(|error| {
            format!(
                "Official workspace project '{}' is unavailable: {error}",
                spec.id
            )
        })?;
    ensure_path_within(&project_root, &official_root, "Official workspace plugin")?;
    if !project_root.is_dir() {
        return Err(format!(
            "Official workspace project '{}' is not a directory.",
            spec.id
        ));
    }

    let manifest_path = canonical_manifest_path(&project_root)?;
    let manifest = read_manifest(&manifest_path)?;
    validate_manifest(&manifest)?;
    if manifest.id != spec.id {
        return Err(format!(
            "Official workspace project '{}' declares unexpected plugin ID '{}'.",
            spec.id, manifest.id
        ));
    }
    let package_root = manifest_path
        .parent()
        .ok_or_else(|| "Official workspace plugin manifest has no package directory.".to_owned())?;
    ensure_path_within(
        package_root,
        &project_root,
        "Official workspace plugin package",
    )?;
    let frontend_entry = manifest_frontend_entry(&manifest).ok_or_else(|| {
        format!(
            "Official workspace project '{}' has no frontend entry.",
            spec.id
        )
    })?;
    canonical_package_file(package_root, &frontend_entry, "Plugin frontend")?;

    Ok((project_root, manifest.name))
}

fn trusted_development_source_root() -> Result<PathBuf, String> {
    let marker_path = development_launcher_marker_path()?;
    let metadata = fs::symlink_metadata(&marker_path).map_err(|error| {
        format!(
            "The trusted iHub development launcher is unavailable at {}: {error}",
            marker_path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("The iHub development launcher marker is not a regular file.".to_owned());
    }
    if metadata.len() > MAX_DEVELOPMENT_LAUNCHER_MARKER_BYTES {
        return Err("The iHub development launcher marker is unexpectedly large.".to_owned());
    }
    let text = fs::read_to_string(&marker_path)
        .map_err(|error| format!("Could not read the iHub development launcher marker: {error}"))?;
    let marker: DevelopmentLauncherMarker = serde_json::from_str(&text)
        .map_err(|error| format!("The iHub development launcher marker is invalid: {error}"))?;

    #[cfg(target_os = "windows")]
    let trusted_owner = "iHub Development Launcher";
    #[cfg(target_os = "windows")]
    let minimum_revision = 2;
    #[cfg(target_os = "macos")]
    let trusted_owner = "iHub macOS Development Launcher";
    #[cfg(target_os = "macos")]
    let minimum_revision = 1;
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let trusted_owner = "";
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let minimum_revision = u32::MAX;

    if marker.schema_version != 1
        || marker.managed_by != trusted_owner
        || marker.launcher_revision < minimum_revision
    {
        return Err("The iHub development launcher marker is not trusted.".to_owned());
    }
    if marker.source_root.trim().is_empty()
        || marker.source_root.contains('\r')
        || marker.source_root.contains('\n')
        || !Path::new(&marker.source_root).is_absolute()
    {
        return Err("The iHub development launcher source root is invalid.".to_owned());
    }

    let source_root = PathBuf::from(marker.source_root)
        .canonicalize()
        .map_err(|error| {
            format!("The configured iHub development checkout is unavailable: {error}")
        })?;
    if !source_root.is_dir()
        || !source_root.join("package.json").is_file()
        || !source_root.join("pnpm-lock.yaml").is_file()
        || !source_root
            .join("src-tauri")
            .join("tauri.conf.json")
            .is_file()
    {
        return Err(
            "The configured development source is not a complete iHub checkout.".to_owned(),
        );
    }
    Ok(source_root)
}

#[cfg(target_os = "windows")]
fn development_launcher_marker_path() -> Result<PathBuf, String> {
    let local_app_data = env::var_os("LOCALAPPDATA")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "LOCALAPPDATA is unavailable.".to_owned())?;
    Ok(PathBuf::from(local_app_data)
        .join("iHub Development")
        .join("launcher.json"))
}

#[cfg(target_os = "macos")]
fn development_launcher_marker_path() -> Result<PathBuf, String> {
    let home = env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "HOME is unavailable.".to_owned())?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("iHub Development")
        .join("launcher.json"))
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn development_launcher_marker_path() -> Result<PathBuf, String> {
    Err("Official workspace plugin links are available only on Windows and macOS.".to_owned())
}

fn find_manifest(root: &Path) -> Option<PathBuf> {
    for name in MANIFEST_NAMES {
        let candidate = root.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // A repository may keep a package under one top-level folder. Do not walk
    // arbitrarily deep: it would make an embedded dependency look installable.
    let entries = fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let directory = entry.path();
        if !directory.is_dir() || is_internal_dir(&directory) {
            continue;
        }
        for name in MANIFEST_NAMES {
            let candidate = directory.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Finds and canonicalizes a manifest while keeping it inside the selected
/// plugin root. This protects development links as well as managed installs
/// from a manifest file or top-level package directory that is a symlink to an
/// unrelated location.
fn canonical_manifest_path(plugin_root: &Path) -> Result<PathBuf, String> {
    let manifest_path = find_manifest(plugin_root)
        .ok_or_else(|| format!("{} has no plugin manifest", plugin_root.display()))?
        .canonicalize()
        .map_err(|error| format!("Could not resolve plugin manifest: {error}"))?;
    ensure_path_within(&manifest_path, plugin_root, "Plugin manifest")?;
    Ok(manifest_path)
}

fn read_manifest(path: &Path) -> Result<PluginManifest, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("Could not read plugin manifest {}: {error}", path.display()))?;
    match serde_json::from_str::<PluginManifest>(&text) {
        Ok(manifest) => Ok(manifest),
        Err(ihub_error) => {
            // Do not reinterpret a malformed iHub manifest as a different
            // package kind. uTools packages have no required iHub `id` field;
            // the presence of one means the author intended iHub's contract.
            let raw = serde_json::from_str::<Value>(&text)
                .map_err(|error| format!("Invalid plugin manifest {}: {error}", path.display()))?;
            if raw.get("id").is_some() {
                return Err(format!(
                    "Invalid iHub plugin manifest {}: {ihub_error}",
                    path.display()
                ));
            }
            let utools = serde_json::from_value::<UtoolsManifest>(raw).map_err(|error| {
                format!(
                    "Invalid plugin manifest {}. It is neither a valid iHub manifest nor a supported public uTools manifest: {error}",
                    path.display()
                )
            })?;
            utools.into_plugin_manifest(path)
        }
    }
}

impl UtoolsManifest {
    /// Projects the public manifest/feature subset into iHub's existing,
    /// manifest-locked command model. One feature becomes one command so a
    /// plugin sees its original `code` when the host dispatches it. Matchers,
    /// files, images and Electron preloads remain unsupported on purpose: iHub
    /// local search is native and never delegates its index/context to a
    /// third-party source-compatibility layer.
    fn into_plugin_manifest(self, manifest_path: &Path) -> Result<PluginManifest, String> {
        if self.main.trim().is_empty() {
            return Err("uTools plugin.json requires a non-empty 'main' HTML entry.".to_owned());
        }
        if let Some(preload) = self.preload.as_deref() {
            // It is never executed or served by iHub, but validate now so a
            // malformed declaration cannot later become a path-handling edge.
            validate_relative_path(preload)?;
        }
        if self.features.is_empty() {
            return Err("uTools plugin.json requires at least one feature.".to_owned());
        }
        if self.features.len() > MAX_COMMANDS_PER_PLUGIN {
            return Err(format!(
                "uTools plugin declares more than {MAX_COMMANDS_PER_PLUGIN} features."
            ));
        }

        let mut command_ids = BTreeSet::new();
        let mut commands = Vec::with_capacity(self.features.len());
        let mut runtime_commands = Vec::with_capacity(self.features.len());
        for (index, feature) in self.features.into_iter().enumerate() {
            if feature.code.trim().is_empty() || feature.code.chars().count() > 160 {
                return Err(
                    "Each uTools feature requires a code of at most 160 characters.".to_owned(),
                );
            }
            if feature.cmds.is_empty() {
                return Err(format!(
                    "uTools feature '{}' requires at least one command.",
                    feature.code
                ));
            }
            let command_id = format!("utools-feature-{}", index + 1);
            debug_assert!(command_ids.insert(command_id.clone()));
            let mut keywords = Vec::new();
            for command in &feature.cmds {
                let candidate = match command {
                    UtoolsFeatureCommand::Keyword(keyword) => Some(keyword),
                    // uTools matcher objects depend on uTools-owned text,
                    // files, images, windows, or regex dispatch. iHub keeps
                    // its local index and launcher context native, so only
                    // direct text commands enter this compatibility surface.
                    UtoolsFeatureCommand::Matcher(_) => None,
                };
                if let Some(keyword) = candidate {
                    let keyword = keyword.trim();
                    if !keyword.is_empty() && !keywords.iter().any(|existing| existing == keyword) {
                        keywords.push(keyword.to_owned());
                    }
                }
            }
            if keywords.is_empty() {
                return Err(format!(
                    "uTools feature '{}' has no direct text command; matchers are not imported into iHub local search.",
                    feature.code
                ));
            }
            let title = feature
                .explain
                .clone()
                .or_else(|| keywords.first().cloned())
                .unwrap_or_else(|| feature.code.clone());
            commands.push(PluginCommandDeclaration {
                id: command_id.clone(),
                name: Some(title.clone()),
                title: Some(title),
                description: feature.explain.clone(),
                subtitle: None,
                icon: feature.icon,
                keywords: keywords.clone(),
                shortcut: None,
                execution: Some("frontend".to_owned()),
                binary: None,
                args: Vec::new(),
                run: None,
            });
            runtime_commands.push(UtoolsCompatCommand {
                command_id,
                code: feature.code,
                keywords,
                main_push: feature.main_push,
            });
        }

        let display_name = self
            .name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| "uTools compatible plugin".to_owned());
        let identity = self.name.as_deref().filter(|name| !name.trim().is_empty());
        let generated_id = utools_plugin_id(manifest_path, identity);
        let description = self.description.or_else(|| {
            Some(
                "通过 iHub 的受限 uTools 兼容层运行：不执行 preload，且不接入本地搜索索引。"
                    .to_owned(),
            )
        });

        Ok(PluginManifest {
            id: generated_id,
            name: display_name,
            version: self
                .version
                .filter(|version| !version.trim().is_empty())
                .unwrap_or_else(|| "0.0.0-utools".to_owned()),
            description,
            icon: self.logo,
            logo: None,
            frontend: None,
            entry: Some(EntryDeclaration {
                frontend: self.main,
            }),
            backend: None,
            contributes: None,
            commands,
            permissions: PluginPermissions::default(),
            update: PluginUpdateDeclaration::default(),
            compatibility: PluginCompatibility::Utools,
            utools_commands: runtime_commands,
            utools_preload: self.preload,
        })
    }
}

/// uTools keeps the published package identity outside of `plugin.json`. iHub
/// needs a local stable identifier for locks and lifecycle state. Prefer the
/// optional project name when present; otherwise derive it from the containing
/// directory. Once managed, the generated `utools-…` directory name itself is
/// the durable identity, while an update preserves its expected ID.
fn utools_plugin_id(manifest_path: &Path, declared_name: Option<&str>) -> String {
    let directory_name = manifest_path
        .parent()
        .and_then(Path::file_name)
        .and_then(OsStr::to_str)
        .unwrap_or("plugin");
    if directory_name.starts_with("utools-") && is_valid_identifier(directory_name) {
        return directory_name.to_owned();
    }
    let identity = declared_name.unwrap_or(directory_name);
    let digest = Sha256::digest(identity.as_bytes());
    let digest = format!("{digest:x}");
    format!("utools-{}", &digest[..16])
}

const SNAPSHOT_HASH_ALGORITHM: &str = "sha256";

/// Calculates the set of runtime assets that a user actually approved when a
/// Git snapshot was imported. It deliberately covers the complete dedicated
/// frontend asset directory (not only index.html) because plugin JavaScript
/// can lazily import any sibling bundle file served by the loopback server.
/// Native workers and standalone artwork are limited to paths declared in the
/// validated manifest. New top-level artwork is decoded here as well as
/// hashed, so an unsupported or malformed identity image fails while a remote
/// snapshot is still in staging. Legacy command artwork remains optional and
/// safely falls back when it is not a supported raster.
fn snapshot_integrity(
    plugin_root: &Path,
    manifest_path: &Path,
    manifest: &PluginManifest,
) -> Result<PluginSnapshotIntegrity, String> {
    let plugin_root = plugin_root
        .canonicalize()
        .map_err(|error| format!("Could not resolve plugin snapshot root: {error}"))?;
    let manifest_path = manifest_path
        .canonicalize()
        .map_err(|error| format!("Could not resolve plugin manifest for integrity: {error}"))?;
    ensure_path_within(&manifest_path, &plugin_root, "Plugin manifest")?;
    let package_root = manifest_path
        .parent()
        .ok_or_else(|| "Plugin manifest has no package directory.".to_owned())?;
    ensure_path_within(package_root, &plugin_root, "Plugin package")?;

    let frontend_assets = snapshot_frontend_assets(package_root, manifest)?;
    let artwork_assets = snapshot_artwork_assets(package_root, manifest)?;
    let native_binaries = snapshot_native_binaries(package_root, manifest)?;
    Ok(PluginSnapshotIntegrity {
        algorithm: SNAPSHOT_HASH_ALGORITHM.to_owned(),
        manifest_sha256: sha256_file(&manifest_path)?,
        frontend_assets,
        artwork_assets: Some(artwork_assets),
        native_binaries,
    })
}

fn snapshot_frontend_assets(
    package_root: &Path,
    manifest: &PluginManifest,
) -> Result<Vec<PluginArtifactDigest>, String> {
    let Some(frontend_entry) = manifest_frontend_entry(manifest) else {
        return Ok(Vec::new());
    };
    let frontend_path = canonical_package_file(package_root, &frontend_entry, "Plugin frontend")?;
    let asset_root = frontend_path
        .parent()
        .ok_or_else(|| "Plugin frontend has no parent directory.".to_owned())?
        .canonicalize()
        .map_err(|error| format!("Could not resolve plugin frontend bundle: {error}"))?;
    ensure_path_within(&asset_root, package_root, "Plugin frontend bundle")?;
    if asset_root == package_root && !manifest.compatibility.is_utools() {
        return Err(
            "Plugin frontend must live in a dedicated child build directory such as dist/index.html, not beside plugin.json."
                .to_owned(),
        );
    }
    collect_asset_digests(package_root, &asset_root)
}

fn snapshot_native_binaries(
    package_root: &Path,
    manifest: &PluginManifest,
) -> Result<Vec<PluginArtifactDigest>, String> {
    let mut binaries = BTreeMap::<String, PathBuf>::new();
    let mut add_binary = |declared_path: &str| -> Result<(), String> {
        let path = canonical_package_file(package_root, declared_path, "Plugin native binary")?;
        let relative = normalized_package_relative_path(package_root, &path)?;
        binaries.insert(relative, path);
        Ok(())
    };

    if let Some(backend) = &manifest.backend {
        if let Some(binary) = &backend.binary {
            add_binary(binary)?;
        }
        for binary in &backend.binaries {
            add_binary(&binary.path)?;
        }
    }
    for command in declared_commands(manifest) {
        if let Some(binary) = &command.binary {
            add_binary(binary)?;
        }
    }

    binaries
        .into_iter()
        .map(|(path, binary)| {
            Ok(PluginArtifactDigest {
                path,
                sha256: sha256_file(&binary)?,
            })
        })
        .collect()
}

fn snapshot_artwork_assets(
    package_root: &Path,
    manifest: &PluginManifest,
) -> Result<Vec<PluginArtifactDigest>, String> {
    let artwork = load_manifest_artwork(package_root, manifest)?;
    let mut paths = BTreeMap::<String, PathBuf>::new();
    for image in artwork.values() {
        let relative = normalized_package_relative_path(package_root, &image.canonical_path)?;
        paths.insert(relative, image.canonical_path.clone());
    }
    paths
        .into_iter()
        .map(|(path, source)| {
            Ok(PluginArtifactDigest {
                path,
                sha256: sha256_file(&source)?,
            })
        })
        .collect()
}

fn canonical_package_file(
    package_root: &Path,
    declared_path: &str,
    label: &str,
) -> Result<PathBuf, String> {
    validate_relative_path(declared_path)?;
    let path = package_root.join(declared_path).canonicalize().map_err(|error| {
        format!(
            "Could not resolve {label} '{declared_path}' while calculating plugin integrity: {error}"
        )
    })?;
    ensure_path_within(&path, package_root, label)?;
    if !path.is_file() {
        return Err(format!("{label} is not a file: {}", path.display()));
    }
    Ok(path)
}

fn collect_asset_digests(
    package_root: &Path,
    asset_root: &Path,
) -> Result<Vec<PluginArtifactDigest>, String> {
    let mut pending = vec![asset_root.to_owned()];
    let mut assets = Vec::new();

    while let Some(directory) = pending.pop() {
        let mut entries = fs::read_dir(&directory)
            .map_err(|error| {
                format!(
                    "Could not read plugin frontend bundle {}: {error}",
                    directory.display()
                )
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("Could not inspect plugin frontend bundle: {error}"))?;
        entries.sort_by_key(|entry| entry.file_name());

        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                format!(
                    "Could not inspect plugin frontend asset {}: {error}",
                    path.display()
                )
            })?;
            if metadata.file_type().is_symlink() {
                return Err(format!(
                    "Plugin frontend bundle contains a symbolic link at {}; immutable Git frontend assets must be regular files.",
                    path.display()
                ));
            }
            if metadata.is_dir() {
                let canonical = path.canonicalize().map_err(|error| {
                    format!(
                        "Could not resolve plugin frontend directory {}: {error}",
                        path.display()
                    )
                })?;
                ensure_path_within(&canonical, asset_root, "Plugin frontend asset")?;
                pending.push(canonical);
                continue;
            }
            if !metadata.is_file() {
                return Err(format!(
                    "Plugin frontend bundle contains a non-regular asset at {}.",
                    path.display()
                ));
            }
            let canonical = path.canonicalize().map_err(|error| {
                format!(
                    "Could not resolve plugin frontend asset {}: {error}",
                    path.display()
                )
            })?;
            ensure_path_within(&canonical, asset_root, "Plugin frontend asset")?;
            assets.push(PluginArtifactDigest {
                path: normalized_package_relative_path(package_root, &canonical)?,
                sha256: sha256_file(&canonical)?,
            });
        }
    }

    assets.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(assets)
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|error| {
        format!(
            "Could not open plugin asset {} for hashing: {error}",
            path.display()
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("Could not hash plugin asset {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn normalized_package_relative_path(package_root: &Path, path: &Path) -> Result<String, String> {
    let relative = path.strip_prefix(package_root).map_err(|_| {
        format!(
            "Plugin integrity asset {} escapes package {}.",
            path.display(),
            package_root.display()
        )
    })?;
    let components = relative
        .components()
        .map(|component| match component {
            Component::Normal(name) => name.to_str().map(str::to_owned).ok_or_else(|| {
                format!(
                    "Plugin integrity asset {} has a non-UTF-8 path and cannot be locked.",
                    path.display()
                )
            }),
            _ => Err(format!(
                "Plugin integrity asset {} does not have a normal package-relative path.",
                path.display()
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if components.is_empty() {
        return Err("Plugin integrity cannot lock the package directory itself.".to_owned());
    }
    Ok(components.join("/"))
}

fn verify_snapshot_integrity(
    plugin_root: &Path,
    source_lock: &PluginSourceLock,
) -> Result<(), String> {
    let Some(expected) = source_lock.integrity.as_ref() else {
        // Backward-compatible source locks were created before iHub could
        // capture runtime hashes. They remain usable, but a re-import upgrades
        // them to a verified snapshot.
        return Ok(());
    };
    validate_snapshot_integrity(expected)?;
    let manifest_path = canonical_manifest_path(plugin_root)?;
    let manifest = read_manifest(&manifest_path)?;
    validate_manifest(&manifest)?;
    let actual = snapshot_integrity(plugin_root, &manifest_path, &manifest)?;
    // Locks written before artwork integrity existed intentionally omit the
    // field. They retain their previous verification guarantees after an app
    // upgrade; every new import writes `Some`, even when the list is empty.
    let artwork_matches = match expected.artwork_assets.as_ref() {
        Some(artwork) => actual.artwork_assets.as_ref() == Some(artwork),
        None => true,
    };
    if expected.algorithm == actual.algorithm
        && expected.manifest_sha256 == actual.manifest_sha256
        && expected.frontend_assets == actual.frontend_assets
        && artwork_matches
        && expected.native_binaries == actual.native_binaries
    {
        return Ok(());
    }

    let changed = if expected.manifest_sha256 != actual.manifest_sha256 {
        "manifest"
    } else if expected.frontend_assets != actual.frontend_assets {
        "frontend bundle"
    } else if !artwork_matches {
        "plugin artwork"
    } else {
        "native worker"
    };
    Err(format!(
        "Plugin '{}' fails its immutable integrity check: the {changed} differs from the Git snapshot approved at import. iHub will not load or run it. Re-import the plugin only after reviewing a trusted source.",
        manifest.id
    ))
}

fn validate_snapshot_integrity(integrity: &PluginSnapshotIntegrity) -> Result<(), String> {
    if integrity.algorithm != SNAPSHOT_HASH_ALGORITHM {
        return Err(format!(
            "Unsupported plugin snapshot integrity algorithm '{}'.",
            integrity.algorithm
        ));
    }
    if !is_sha256_digest(&integrity.manifest_sha256) {
        return Err("Plugin snapshot integrity has an invalid manifest SHA-256.".to_owned());
    }
    validate_artifact_digests(&integrity.frontend_assets, "frontend")?;
    if let Some(artwork) = integrity.artwork_assets.as_ref() {
        validate_artifact_digests(artwork, "artwork")?;
    }
    validate_artifact_digests(&integrity.native_binaries, "native")?;
    Ok(())
}

fn validate_artifact_digests(
    artifacts: &[PluginArtifactDigest],
    label: &str,
) -> Result<(), String> {
    let mut paths = BTreeMap::new();
    for artifact in artifacts {
        validate_relative_path(&artifact.path)?;
        if normalized_relative_path_text(&artifact.path)? != artifact.path {
            return Err(format!(
                "Plugin snapshot {label} integrity path '{}' is not normalized.",
                artifact.path
            ));
        }
        if !is_sha256_digest(&artifact.sha256) {
            return Err(format!(
                "Plugin snapshot {label} integrity has an invalid SHA-256 for '{}'.",
                artifact.path
            ));
        }
        if paths.insert(&artifact.path, ()).is_some() {
            return Err(format!(
                "Plugin snapshot {label} integrity lists '{}' more than once.",
                artifact.path
            ));
        }
    }
    Ok(())
}

fn normalized_relative_path_text(value: &str) -> Result<String, String> {
    let components = Path::new(value)
        .components()
        .map(|component| match component {
            Component::Normal(name) => name
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| "Plugin snapshot integrity paths must be valid UTF-8.".to_owned()),
            _ => Err("Plugin snapshot integrity paths must be normal relative paths.".to_owned()),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if components.is_empty() {
        return Err("Plugin snapshot integrity paths cannot be empty.".to_owned());
    }
    Ok(components.join("/"))
}

fn is_sha256_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn write_source_lock(directory: &Path, source_lock: &PluginSourceLock) -> Result<(), String> {
    fs::write(
        directory.join(SOURCE_LOCK),
        serde_json::to_vec_pretty(source_lock)
            .map_err(|error| format!("Could not serialize plugin source lock: {error}"))?,
    )
    .map_err(|error| format!("Could not save plugin source lock: {error}"))
}

fn read_source_metadata(directory: &Path) -> Result<SourceMetadata, String> {
    let lock_path = directory.join(SOURCE_LOCK);
    if lock_path.exists() {
        if !lock_path.is_file() {
            return Err(format!(
                "Plugin source lock is not a file: {}",
                lock_path.display()
            ));
        }
        let text = fs::read_to_string(&lock_path)
            .map_err(|error| format!("Could not read plugin source lock: {error}"))?;
        let mut lock = serde_json::from_str::<PluginSourceLock>(&text)
            .map_err(|error| format!("Invalid plugin source lock: {error}"))?;
        if lock.source.trim().is_empty()
            || lock.requested_ref.trim().is_empty()
            || !is_git_object_id(&lock.resolved_commit)
            || lock.installed_at.trim().is_empty()
        {
            return Err("Plugin source lock is missing required provenance fields.".to_owned());
        }
        if let Some(integrity) = lock.integrity.as_ref() {
            validate_snapshot_integrity(integrity)?;
        }
        // Source locks written by older development builds (or edited by the
        // same local user) are untrusted input. Re-validate before any
        // provenance reaches IPC or Git, and canonicalize safe shorthand
        // without ever reflecting a rejected credential-bearing URL.
        let validated_source = git_source_from_lock(&lock)?;
        lock.source = validated_source.remote;
        lock.requested_ref = validated_source.requested_ref;
        return Ok(SourceMetadata {
            source: lock.source.clone(),
            resolved_commit: Some(lock.resolved_commit.clone()),
            installed_at: lock.installed_at.clone(),
            lock: Some(lock),
        });
    }

    // Existing installations written by previous iHub versions retain their
    // old metadata and continue to show up. They have no requested ref lock,
    // so callers can distinguish them from new immutable imports.
    let legacy_path = directory.join(LEGACY_SOURCE_RECORD);
    let text = fs::read_to_string(&legacy_path)
        .map_err(|error| format!("Could not read plugin source record: {error}"))?;
    let legacy = serde_json::from_str::<LegacySourceRecord>(&text)
        .map_err(|error| format!("Invalid plugin source record: {error}"))?;
    let source = parse_git_source(&legacy.source).map_err(|error| {
        format!(
            "The legacy plugin source record is not a safe Git source: {error} Re-import the plugin from a credential-free HTTPS URL."
        )
    })?;
    Ok(SourceMetadata {
        source: source.remote,
        resolved_commit: legacy.commit,
        installed_at: legacy.installed_at,
        lock: None,
    })
}

fn manifest_artwork_path(manifest: &PluginManifest) -> Option<&str> {
    manifest.icon.as_deref().or(manifest.logo.as_deref())
}

/// Strictly decodes the top-level identity and best-effort decodes a bounded
/// set of distinct command images once for one plugin projection. Returned
/// values contain only canonical host paths (for integrity hashing) and
/// normalized PNG data URLs (for IPC); original bytes and manifest paths are
/// never serialized to the renderer.
fn load_manifest_artwork(
    package_root: &Path,
    manifest: &PluginManifest,
) -> Result<BTreeMap<String, PluginArtwork>, String> {
    let mut loaded = BTreeMap::new();
    let mut attempted_paths = BTreeSet::new();

    // Top-level artwork is a new identity declaration and therefore strict:
    // a package that explicitly opts into it must provide a valid raster.
    if let Some(path) = manifest_artwork_path(manifest) {
        attempted_paths.insert(path);
        loaded.insert(
            path.to_owned(),
            load_plugin_artwork(package_root, path, "plugin icon")?,
        );
    }

    // commands[].icon existed before the native artwork pipeline and older
    // packages commonly point it at SVG. Those packages must keep working
    // after an iHub upgrade. Treat command artwork as best-effort metadata:
    // unsafe, missing, unsupported, or over-budget candidates are never read
    // into the WebView and simply use the plugin/fallback glyph.
    for command in declared_commands(manifest) {
        let Some(path) = command.icon.as_deref() else {
            continue;
        };
        if attempted_paths.contains(path) {
            continue;
        }
        if attempted_paths.len() >= MAX_ARTWORK_FILES_PER_PLUGIN {
            break;
        }
        attempted_paths.insert(path);
        if let Ok(artwork) = load_plugin_artwork(
            package_root,
            path,
            &format!("icon for plugin command '{}'", command.id),
        ) {
            loaded.insert(path.to_owned(), artwork);
        }
    }
    Ok(loaded)
}

fn validate_manifest(manifest: &PluginManifest) -> Result<(), String> {
    if !is_valid_identifier(&manifest.id) {
        return Err(
            "Plugin manifest ID must contain only letters, digits, '.', '_' or '-'.".to_owned(),
        );
    }
    if manifest.name.trim().is_empty() {
        return Err("Plugin manifest name cannot be empty.".to_owned());
    }
    if manifest.icon.is_some() && manifest.logo.is_some() {
        return Err("Plugin manifest must declare only one of 'icon' or 'logo'.".to_owned());
    }
    if let Some(path) = manifest_artwork_path(manifest) {
        validate_artwork_relative_path(path)?;
    }
    if let Some(channel) = manifest.update.channel.as_deref() {
        if !matches!(channel, "stable" | "beta") {
            return Err(format!(
                "Plugin update channel '{channel}' is unsupported; use 'stable' or 'beta'."
            ));
        }
    }
    validate_permission_declarations(&manifest.permissions)?;
    let commands = declared_commands(manifest);
    if commands.len() > MAX_COMMANDS_PER_PLUGIN {
        return Err(format!(
            "Plugin declares more than {MAX_COMMANDS_PER_PLUGIN} commands."
        ));
    }
    let mut command_ids = std::collections::HashSet::new();
    let mut canonical_shortcuts = std::collections::HashSet::new();
    let mut shortcut_count = 0usize;
    for command in commands {
        if !is_valid_identifier(&command.id) {
            return Err(format!("Plugin command ID '{}' is invalid.", command.id));
        }
        if !command_ids.insert(command.id.clone()) {
            return Err(format!(
                "Plugin declares command '{}' more than once.",
                command.id
            ));
        }
        validate_shortcut_keywords(&command.keywords, &format!("command '{}'", command.id))?;
        if let Some(shortcut) = command.shortcut.as_deref() {
            if !manifest.permissions.global_shortcut {
                return Err(format!(
                    "Plugin command '{}' declares a shortcut without permissions.globalShortcut: true.",
                    command.id
                ));
            }
            let shortcut = normalize_plugin_hotkey(shortcut).map_err(|error| {
                format!(
                    "Plugin command '{}' has an invalid shortcut: {error}",
                    command.id
                )
            })?;
            reject_launcher_reserved_shortcut(&shortcut)?;
            if !canonical_shortcuts.insert(shortcut.clone()) {
                return Err(format!(
                    "Plugin declares global shortcut '{shortcut}' more than once."
                ));
            }
            shortcut_count += 1;
        }
        if let Some(execution) = command.execution.as_deref() {
            if !matches!(execution, "frontend" | "native") {
                return Err(format!(
                    "Plugin command '{}' has unsupported execution '{execution}'; use 'frontend' or 'native'.",
                    command.id
                ));
            }
        }
        if let Some(binary) = &command.binary {
            validate_relative_path(binary)?;
        }
        let execution = command_execution(manifest, command);
        if let Some(run) = command.run.as_ref() {
            let timeout_ms = run.timeout_ms;
            if execution != CommandExecution::Native {
                return Err(format!(
                    "Plugin command '{}' may declare run.timeoutMs only for native execution.",
                    command.id
                ));
            }
            if !(MIN_PLUGIN_COMMAND_TIMEOUT_MS..=MAX_PLUGIN_COMMAND_TIMEOUT_MS)
                .contains(&timeout_ms)
            {
                return Err(format!(
                    "Plugin command '{}' run.timeoutMs must be between {} and {} milliseconds.",
                    command.id, MIN_PLUGIN_COMMAND_TIMEOUT_MS, MAX_PLUGIN_COMMAND_TIMEOUT_MS
                ));
            }
        }
        if execution == CommandExecution::Frontend {
            if command.binary.is_some() {
                return Err(format!(
                    "Frontend command '{}' must not declare a native binary.",
                    command.id
                ));
            }
            if manifest_frontend_entry(manifest).is_none() {
                return Err(format!(
                    "Frontend command '{}' requires entry.frontend or frontend.",
                    command.id
                ));
            }
        } else if !command_has_native_worker(manifest, command) {
            return Err(format!(
                "Native command '{}' requires command.binary or a declared backend binary.",
                command.id
            ));
        }
    }
    let global_shortcuts = declared_global_shortcuts(manifest);
    let mut global_shortcut_ids = std::collections::HashSet::new();
    for binding in global_shortcuts {
        if !manifest.permissions.global_shortcut {
            return Err(format!(
                "Plugin shortcut mapping '{}' requires permissions.globalShortcut: true.",
                binding.id
            ));
        }
        if !is_valid_identifier(&binding.id) || !global_shortcut_ids.insert(&binding.id) {
            return Err(format!(
                "Plugin shortcut mapping ID '{}' is invalid or duplicated.",
                binding.id
            ));
        }
        match (binding.command_id.as_deref(), binding.keyword.as_deref()) {
            (Some(command_id), None) => {
                if !command_ids.contains(command_id) {
                    return Err(format!(
                        "Plugin shortcut mapping '{}' targets undeclared command '{command_id}'.",
                        binding.id
                    ));
                }
            }
            (None, Some(keyword)) => {
                validate_shortcut_keyword(keyword, &format!("shortcut mapping '{}'", binding.id))?;
            }
            _ => {
                return Err(format!(
                    "Plugin shortcut mapping '{}' must declare exactly one of commandId or keyword.",
                    binding.id
                ));
            }
        }
        let shortcut = normalize_plugin_hotkey(&binding.shortcut).map_err(|error| {
            format!(
                "Plugin shortcut mapping '{}' has an invalid shortcut: {error}",
                binding.id
            )
        })?;
        reject_launcher_reserved_shortcut(&shortcut)?;
        if !canonical_shortcuts.insert(shortcut.clone()) {
            return Err(format!(
                "Plugin declares global shortcut '{shortcut}' more than once."
            ));
        }
        shortcut_count += 1;
    }
    if shortcut_count > MAX_GLOBAL_SHORTCUTS_PER_PLUGIN {
        return Err(format!(
            "Plugin declares more than {MAX_GLOBAL_SHORTCUTS_PER_PLUGIN} global shortcuts."
        ));
    }
    let providers = declared_search_providers(manifest);
    if providers.len() > MAX_SEARCH_PROVIDERS_PER_PLUGIN {
        return Err(format!(
            "Plugin declares more than {MAX_SEARCH_PROVIDERS_PER_PLUGIN} search providers."
        ));
    }
    let mut provider_ids = std::collections::HashSet::new();
    for provider in providers {
        if !is_valid_identifier(&provider.id) {
            return Err(format!(
                "Plugin search provider ID '{}' is invalid.",
                provider.id
            ));
        }
        if provider.title.trim().is_empty() {
            return Err(format!(
                "Plugin search provider '{}' must declare a non-empty title.",
                provider.id
            ));
        }
        if provider.title.chars().count() > 160 {
            return Err(format!(
                "Plugin search provider '{}' has a title that is too long.",
                provider.id
            ));
        }
        if provider
            .trigger
            .as_deref()
            .is_some_and(|trigger| trigger.trim().is_empty() || trigger.chars().count() > 48)
        {
            return Err(format!(
                "Plugin search provider '{}' has an invalid trigger.",
                provider.id
            ));
        }
        if !provider_ids.insert(&provider.id) {
            return Err(format!(
                "Plugin declares search provider '{}' more than once.",
                provider.id
            ));
        }
    }
    let settings = declared_settings(manifest);
    if settings.len() > MAX_SETTINGS_PER_PLUGIN {
        return Err(format!(
            "Plugin declares more than {MAX_SETTINGS_PER_PLUGIN} settings."
        ));
    }
    let mut setting_keys = std::collections::HashSet::new();
    for setting in settings {
        if !is_valid_setting_key(&setting.key) {
            return Err(format!("Plugin setting key '{}' is invalid.", setting.key));
        }
        if setting.title.trim().is_empty() || setting.title.chars().count() > 160 {
            return Err(format!(
                "Plugin setting '{}' must declare a title of at most 160 characters.",
                setting.key
            ));
        }
        if !matches!(
            setting.value_type.as_str(),
            "string" | "number" | "boolean" | "select" | "textarea"
        ) {
            return Err(format!(
                "Plugin setting '{}' has unsupported type '{}'.",
                setting.key, setting.value_type
            ));
        }
        if !setting_keys.insert(&setting.key) {
            return Err(format!(
                "Plugin declares setting '{}' more than once.",
                setting.key
            ));
        }
    }
    if let Some(backend) = &manifest.backend {
        if let Some(binary) = &backend.binary {
            validate_relative_path(binary)?;
        }
        if !backend.binaries.is_empty() && backend.protocol.as_deref() != Some("jsonl-rpc-v1") {
            return Err("Plugin backend.binaries requires protocol 'jsonl-rpc-v1'.".to_owned());
        }
        let mut backend_targets = std::collections::HashSet::new();
        for binary in &backend.binaries {
            validate_relative_path(&binary.path)?;
            if !is_supported_target(&binary.target) {
                return Err(format!(
                    "Plugin backend target '{}' is not supported.",
                    binary.target
                ));
            }
            if !backend_targets.insert(&binary.target) {
                return Err(format!(
                    "Plugin backend target '{}' must be declared at most once.",
                    binary.target
                ));
            }
        }
    }
    if let Some(entry) = &manifest.entry {
        validate_relative_path(&entry.frontend)?;
    }
    if let Some(frontend) = &manifest.frontend {
        validate_relative_path(&frontend_entry(frontend))?;
    }
    Ok(())
}

fn validate_permission_declarations(permissions: &PluginPermissions) -> Result<(), String> {
    if let Some(filesystem) = permissions.filesystem.as_ref() {
        validate_permission_string_list(&filesystem.read, "permissions.filesystem.read")?;
        validate_permission_string_list(&filesystem.write, "permissions.filesystem.write")?;
    }
    if let Some(network) = permissions.network.as_ref() {
        validate_permission_string_list(&network.allow, "permissions.network.allow")?;
    }
    if let Some(process) = permissions.process.as_ref() {
        validate_permission_string_list(&process.allow, "permissions.process.allow")?;
    }
    Ok(())
}

fn validate_permission_string_list(values: &[String], label: &str) -> Result<(), String> {
    if values.len() > MAX_PERMISSION_LIST_ITEMS {
        return Err(format!(
            "Plugin {label} declares more than {MAX_PERMISSION_LIST_ITEMS} entries."
        ));
    }

    let mut seen = std::collections::HashSet::new();
    for value in values {
        if value
            .trim_matches(|character: char| character.is_whitespace() || character == '\u{feff}')
            != value
            || value.is_empty()
            || value.chars().count() > MAX_PERMISSION_VALUE_CHARS
            || value.chars().any(char::is_control)
        {
            return Err(format!(
                "Plugin {label} entries must be non-empty, trimmed, free of control characters, and at most {MAX_PERMISSION_VALUE_CHARS} characters."
            ));
        }
        if !seen.insert(value) {
            return Err(format!("Plugin {label} entries must be unique."));
        }
    }
    Ok(())
}

fn validate_shortcut_keywords(keywords: &[String], label: &str) -> Result<(), String> {
    if keywords.len() > 16 {
        return Err(format!("Plugin {label} declares more than 16 keywords."));
    }
    for keyword in keywords {
        validate_shortcut_keyword(keyword, label)?;
    }
    Ok(())
}

fn normalized_shortcut_keywords(keywords: &[String]) -> Vec<String> {
    let mut canonical = std::collections::HashSet::new();
    keywords
        .iter()
        .filter_map(|keyword| {
            let trimmed = keyword.trim();
            canonical
                .insert(trimmed.to_lowercase())
                .then(|| trimmed.to_owned())
        })
        .collect()
}

fn validate_shortcut_keyword(keyword: &str, label: &str) -> Result<(), String> {
    if keyword.trim().is_empty()
        || keyword.chars().count() > MAX_SHORTCUT_KEYWORD_CHARS
        || keyword.chars().any(char::is_control)
    {
        return Err(format!(
            "Plugin {label} has a keyword that is empty, contains controls, or exceeds {MAX_SHORTCUT_KEYWORD_CHARS} characters."
        ));
    }
    Ok(())
}

fn reject_launcher_reserved_shortcut(shortcut: &str) -> Result<(), String> {
    if matches!(shortcut, "Alt+Space" | "Alt+Shift+Space") {
        return Err(format!(
            "Global shortcut '{shortcut}' is reserved for the iHub launcher and its recovery binding."
        ));
    }
    Ok(())
}

fn is_valid_identifier(value: &str) -> bool {
    let length = value.len();
    (2..=96).contains(&length)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn is_valid_setting_key(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_alphabetic()
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn validate_relative_path(value: &str) -> Result<(), String> {
    let path = Path::new(value);
    if value.trim().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(
            "Plugin package paths must be relative paths inside the plugin directory.".to_owned(),
        );
    }
    Ok(())
}

fn resolve_plugin_path(plugin_dir: &Path, value: &str) -> Result<PathBuf, String> {
    validate_relative_path(value)?;
    let resolved = plugin_dir.join(value);
    if !resolved.starts_with(plugin_dir) {
        return Err("Plugin binary path escapes the plugin directory.".to_owned());
    }
    Ok(resolved)
}

fn ensure_path_within(path: &Path, root: &Path, label: &str) -> Result<(), String> {
    if path.starts_with(root) {
        Ok(())
    } else {
        Err(format!("{label} escapes the installed plugin directory."))
    }
}

fn frontend_entry(frontend: &FrontendDeclaration) -> String {
    match frontend {
        FrontendDeclaration::Entry(entry) => entry.clone(),
        FrontendDeclaration::Detailed { entry } => entry.clone(),
    }
}

fn manifest_frontend_entry(manifest: &PluginManifest) -> Option<String> {
    manifest
        .entry
        .as_ref()
        .map(|entry| entry.frontend.clone())
        .or_else(|| manifest.frontend.as_ref().map(frontend_entry))
}

fn declared_commands(manifest: &PluginManifest) -> &[PluginCommandDeclaration] {
    manifest
        .contributes
        .as_ref()
        .filter(|contributes| !contributes.commands.is_empty())
        .map(|contributes| contributes.commands.as_slice())
        .unwrap_or(manifest.commands.as_slice())
}

fn declared_global_shortcuts(manifest: &PluginManifest) -> &[PluginGlobalShortcutDeclaration] {
    manifest
        .contributes
        .as_ref()
        .map(|contributes| contributes.global_shortcuts.as_slice())
        .unwrap_or(&[])
}

fn manifest_has_native_worker(manifest: &PluginManifest) -> bool {
    manifest
        .backend
        .as_ref()
        .is_some_and(|backend| backend.binary.is_some() || !backend.binaries.is_empty())
        || declared_commands(manifest)
            .iter()
            .any(|command| command.binary.is_some())
}

fn command_has_native_worker(
    manifest: &PluginManifest,
    command: &PluginCommandDeclaration,
) -> bool {
    command.binary.is_some()
        || manifest
            .backend
            .as_ref()
            .is_some_and(|backend| backend.binary.is_some() || !backend.binaries.is_empty())
}

/// Selects a command activation target without forcing existing manifests to
/// migrate. A plugin with a backend keeps its historical native default, but
/// can opt a command into its frontend with `execution: "frontend"`.
fn command_execution(
    manifest: &PluginManifest,
    command: &PluginCommandDeclaration,
) -> CommandExecution {
    match command.execution.as_deref() {
        Some("frontend") => CommandExecution::Frontend,
        Some("native") => CommandExecution::Native,
        // Validation rejects any other explicit value. Keeping a safe default
        // here also makes this helper robust for diagnostic/read-only paths.
        Some(_) => CommandExecution::Frontend,
        None if command_has_native_worker(manifest, command) => CommandExecution::Native,
        None => CommandExecution::Frontend,
    }
}

/// `validate_manifest` ensures an explicit value is within the bounded policy
/// range before this is used to wait on a worker. Keeping the default here
/// preserves all existing native manifests unchanged.
fn command_timeout(command: &PluginCommandDeclaration) -> Duration {
    command
        .run
        .as_ref()
        .map(|run| run.timeout_ms)
        .map(Duration::from_millis)
        .unwrap_or(PLUGIN_COMMAND_TIMEOUT)
}

fn declared_search_providers(manifest: &PluginManifest) -> &[PluginSearchProviderDeclaration] {
    manifest
        .contributes
        .as_ref()
        .map(|contributes| contributes.search_providers.as_slice())
        .unwrap_or(&[])
}

fn declared_settings(manifest: &PluginManifest) -> &[PluginSettingDeclaration] {
    manifest
        .contributes
        .as_ref()
        .map(|contributes| contributes.settings.as_slice())
        .unwrap_or(&[])
}

/// Canonicalizes every host permission and executable declaration that can
/// change the trust surface of an already-installed plugin. This is not a
/// sandbox: it is the explicit review boundary that prevents a routine Git
/// refresh from silently widening bridge access or introducing/repointing a
/// native worker.
fn plugin_security_declaration(manifest: &PluginManifest) -> PluginSecurityDeclaration {
    let mut declaration = PluginSecurityDeclaration::default();
    let permissions = &manifest.permissions;

    if manifest.compatibility.is_utools() {
        // The fixed source-compatibility shim has a distinct trust contract:
        // no ambient Node bridge; an explicitly requested BrowserWindow
        // preload receives only the fixed ipcRenderer/contextBridge shim.
        // Preserve that boundary across updates.
        declaration
            .permissions
            .insert("compatibility.utools.screenColorPick.confirmed".to_owned());
        declaration
            .permissions
            .insert("compatibility.utools.screenCapture.confirmedCrop".to_owned());
        declaration
            .permissions
            .insert("compatibility.utools.desktopCaptureSources.systemPicker".to_owned());
        declaration
            .permissions
            .insert("compatibility.utools.mainPush.boundedText".to_owned());
        declaration
            .permissions
            .insert("compatibility.utools.mainPush.oneShotInput".to_owned());
        declaration
            .permissions
            .insert("compatibility.utools.getCopyedFiles.visibleBounded".to_owned());
        declaration
            .permissions
            .insert("compatibility.utools.simulation.visibleConfirmed".to_owned());
        declaration
            .permissions
            .insert("compatibility.utools.dbCryptoStorage.osKeyringAesGcm".to_owned());
        declaration
            .permissions
            .insert("compatibility.utools.startDrag.pickerGranted".to_owned());
        declaration
            .permissions
            .insert("compatibility.utools.imagePath.pickerGranted".to_owned());
        declaration
            .permissions
            .insert("compatibility.utools.browserWindow.sandboxedIpc".to_owned());
        declaration
            .permissions
            .insert("compatibility.utools.ubrowser.hostedHttpsAutomation".to_owned());
    }

    if let Some(filesystem) = permissions.filesystem.as_ref() {
        for scope in &filesystem.read {
            declaration
                .permissions
                .insert(format!("filesystem.read:{scope}"));
        }
        for scope in &filesystem.write {
            declaration
                .permissions
                .insert(format!("filesystem.write:{scope}"));
        }
    }
    if let Some(network) = permissions.network.as_ref() {
        for destination in &network.allow {
            declaration
                .permissions
                .insert(format!("network.allow:{destination}"));
        }
    }
    if let Some(clipboard) = permissions.clipboard.as_ref() {
        if clipboard.read {
            declaration.permissions.insert("clipboard.read".to_owned());
        }
        if clipboard.write {
            declaration.permissions.insert("clipboard.write".to_owned());
        }
        if clipboard.history {
            declaration
                .permissions
                .insert("clipboard.history".to_owned());
        }
    }
    if let Some(shell) = permissions.shell.as_ref() {
        if shell.open_path {
            declaration.permissions.insert("shell.openPath".to_owned());
        }
        if shell.open_external {
            declaration
                .permissions
                .insert("shell.openExternal".to_owned());
        }
    }
    if permissions.screen_capture {
        declaration.permissions.insert("screenCapture".to_owned());
    }
    if permissions.microphone {
        declaration.permissions.insert("microphone".to_owned());
    }
    if permissions.cursor_color {
        declaration.permissions.insert("cursorColor".to_owned());
    }
    if permissions.global_shortcut {
        declaration.permissions.insert("globalShortcut".to_owned());
    }
    if permissions.notifications {
        declaration.permissions.insert("notifications".to_owned());
    }
    if permissions.native_api {
        declaration.permissions.insert("nativeApi".to_owned());
    }
    if permissions.window_management {
        declaration
            .permissions
            .insert("windowManagement".to_owned());
    }
    if let Some(context) = permissions.launcher_context.as_ref() {
        if context.text {
            declaration
                .permissions
                .insert("launcherContext.text".to_owned());
        }
        if context.files {
            declaration
                .permissions
                .insert("launcherContext.files".to_owned());
        }
        if context.image {
            declaration
                .permissions
                .insert("launcherContext.image".to_owned());
        }
    }
    if let Some(process) = permissions.process.as_ref() {
        if process.spawn {
            declaration.permissions.insert("process.spawn".to_owned());
        }
        for program in &process.allow {
            declaration
                .permissions
                .insert(format!("process.allow:{program}"));
        }
    }

    if let Some(backend) = manifest.backend.as_ref() {
        if let Some(binary) = backend.binary.as_ref() {
            declaration
                .native_declarations
                .insert(format!("backend.binary:{binary}"));
            if let Some(protocol) = backend.protocol.as_ref() {
                declaration
                    .native_declarations
                    .insert(format!("backend.protocol:{protocol}"));
            }
        }
        for binary in &backend.binaries {
            declaration.native_declarations.insert(format!(
                "backend.binary:{}:{}:{}",
                binary.target,
                binary.path,
                serde_json::to_string(&binary.args).unwrap_or_default(),
            ));
        }
        // The protocol changes how any declared target binary communicates
        // with the host, so changing it also requires explicit re-import.
        if !backend.binaries.is_empty() {
            declaration.native_declarations.insert(format!(
                "backend.protocol:{}",
                backend.protocol.as_deref().unwrap_or_default()
            ));
        }
    }
    for command in declared_commands(manifest) {
        // A frontend-to-native flip changes what happens when a user selects
        // the same launcher item. Treat it as part of the native trust
        // surface even when it reuses an already-declared backend binary.
        let execution = command_execution(manifest, command);
        declaration.native_declarations.insert(format!(
            "command.execution:{}:{}",
            command.id,
            execution.as_str(),
        ));
        if let Some(shortcut) = command.shortcut.as_deref() {
            declaration.native_declarations.insert(format!(
                "command.globalShortcut:{}:{}",
                command.id,
                normalize_plugin_hotkey(shortcut).unwrap_or_else(|_| shortcut.to_owned()),
            ));
        }
        if let Some(binary) = command.binary.as_ref() {
            declaration.native_declarations.insert(format!(
                "command.binary:{}:{}:{}",
                command.id,
                binary,
                serde_json::to_string(&command.args).unwrap_or_default(),
            ));
        }
        // Commands that reuse the plugin's shared backend still append their
        // own arguments before launch. Those arguments can change the native
        // worker's behavior just as much as a per-command binary can, so they
        // must be part of the routine-update trust declaration too.
        if execution == CommandExecution::Native {
            declaration.native_declarations.insert(format!(
                "command.args:{}:{}",
                command.id,
                serde_json::to_string(&command.args).unwrap_or_default(),
            ));
            // The effective deadline is part of the native trust surface:
            // accepting a Git refresh must never silently turn a bounded
            // command into a much longer-running worker.
            declaration.native_declarations.insert(format!(
                "command.timeoutMs:{}:{}",
                command.id,
                command_timeout(command).as_millis(),
            ));
        }
    }
    for binding in declared_global_shortcuts(manifest) {
        declaration.native_declarations.insert(format!(
            "globalShortcut.mapping:{}:{}:{}:{}",
            binding.id,
            normalize_plugin_hotkey(&binding.shortcut).unwrap_or_else(|_| binding.shortcut.clone()),
            binding.command_id.as_deref().unwrap_or_default(),
            binding.keyword.as_deref().unwrap_or_default(),
        ));
    }

    declaration
}

/// Refuses a routine Git replacement when its security declaration differs.
/// The normal update button does not provide a security-confirmation UI (nor
/// a field-by-field hash diff), so a user must deliberately re-import through
/// the installation/trust flow before such a snapshot can replace a running
/// plugin.
fn ensure_update_security_declaration_matches(
    plugin_id: &str,
    installed: &PluginManifest,
    candidate: &PluginManifest,
) -> Result<(), String> {
    let installed = plugin_security_declaration(installed);
    let candidate = plugin_security_declaration(candidate);
    if installed == candidate {
        return Ok(());
    }

    let permission_changed = installed.permissions != candidate.permissions;
    let native_changed = installed.native_declarations != candidate.native_declarations;
    let mut changed = Vec::new();
    if permission_changed {
        changed.push("permissions");
    }
    if native_changed {
        changed.push("native binary declarations or global shortcut mappings");
    }
    Err(format!(
        "Refusing to update plugin '{plugin_id}': candidate {} changed. Routine Git updates cannot widen or alter the declared trust surface. After reviewing the candidate, uninstall the managed snapshot and re-import it through the explicit trust prompt before accepting this change.",
        changed.join(" and "),
    ))
}

fn command_display_name(command: &PluginCommandDeclaration) -> String {
    command
        .title
        .as_deref()
        .or(command.name.as_deref())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(&command.id)
        .to_owned()
}

fn select_backend_binary(backend: &BackendDeclaration) -> Option<&PluginBinaryDeclaration> {
    backend
        .binaries
        .iter()
        .find(|binary| binary.target == current_platform_target())
}

fn current_platform_target() -> &'static str {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "windows-x86_64"
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        "windows-aarch64"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "darwin-x86_64"
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "darwin-aarch64"
    }
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64")
    )))]
    {
        "unsupported"
    }
}

fn is_supported_target(target: &str) -> bool {
    matches!(
        target,
        "windows-x86_64" | "windows-aarch64" | "darwin-x86_64" | "darwin-aarch64"
    )
}

fn is_internal_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.'))
}

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

fn next_rpc_id() -> String {
    format!("rpc-{}", unique_suffix())
}

fn readable_output(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_owned()
}

/// Validates the one-response JSONL contract before treating a worker result
/// as successful. A worker must not be able to acknowledge another request,
/// or print arbitrary diagnostic lines that the host silently interprets as a
/// result object.
fn parse_jsonl_rpc_response(
    stdout: &str,
    request_id: &str,
) -> Result<(Option<Value>, Option<String>), String> {
    let mut lines = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let line = lines
        .next()
        .ok_or_else(|| "worker did not write a JSON-RPC response on stdout".to_owned())?;
    if lines.next().is_some() {
        return Err("worker wrote more than one JSON-RPC response line".to_owned());
    }
    let response = serde_json::from_str::<Value>(line)
        .map_err(|error| format!("worker stdout is not JSON: {error}"))?;
    if response.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err("response.jsonrpc must be exactly '2.0'".to_owned());
    }
    if response.get("id") != Some(&Value::String(request_id.to_owned())) {
        return Err("response id did not match the host request".to_owned());
    }
    if let Some(error) = response.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .filter(|message| !message.trim().is_empty())
            .unwrap_or("worker returned a JSON-RPC error")
            .to_owned();
        return Ok((
            Some(serde_json::json!({
                "message": message,
                "error": error,
            })),
            Some(message),
        ));
    }
    let result = response
        .get("result")
        .cloned()
        .ok_or_else(|| "response must contain either result or error".to_owned())?;
    Ok((Some(result), None))
}

fn append_plugin_diagnostic(stderr: &mut String, diagnostic: &str) {
    if !stderr.trim().is_empty() {
        stderr.push('\n');
    }
    stderr.push_str("[iHub] ");
    stderr.push_str(diagnostic);
}

enum ChildWaitOutcome {
    Exited(ExitStatus),
    TimedOut,
}

/// Waits for one plugin command without allowing a stuck worker to keep the
/// Tauri command future alive forever. `Child::kill` is followed by `wait` so
/// the terminated process is reaped on both Windows and macOS.
fn wait_for_child_with_timeout(
    child: &mut Child,
    timeout: Duration,
) -> Result<ChildWaitOutcome, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(ChildWaitOutcome::Exited(status)),
            Ok(None) if Instant::now() >= deadline => {
                if let Err(kill_error) = child.kill() {
                    if let Ok(Some(status)) = child.try_wait() {
                        return Ok(ChildWaitOutcome::Exited(status));
                    }
                    return Err(format!(
                        "Could not terminate timed out plugin command: {kill_error}"
                    ));
                }
                child
                    .wait()
                    .map_err(|error| format!("Could not reap timed out plugin command: {error}"))?;
                return Ok(ChildWaitOutcome::TimedOut);
            }
            Ok(None) => thread::sleep(PLUGIN_COMMAND_POLL_INTERVAL),
            Err(error) => {
                terminate_child(child);
                return Err(format!("Could not monitor plugin command: {error}"));
            }
        }
    }
}

fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn spawn_captured_output_reader<R>(reader: R) -> thread::JoinHandle<Result<String, String>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || read_captured_output(reader))
}

fn join_captured_output(
    task: thread::JoinHandle<Result<String, String>>,
    stream_name: &str,
) -> Result<String, String> {
    task.join()
        .map_err(|_| format!("Plugin {stream_name} reader stopped unexpectedly."))?
        .map_err(|error| format!("Could not read plugin {stream_name}: {error}"))
}

/// Drain the entire pipe while retaining only a bounded diagnostic. This keeps
/// a misbehaving worker from both blocking on pipe backpressure and consuming
/// unbounded host memory before the timeout can fire.
fn read_captured_output<R: Read>(mut reader: R) -> Result<String, String> {
    let mut captured = Vec::with_capacity(MAX_CAPTURED_OUTPUT_BYTES.min(16 * 1024));
    let mut buffer = [0_u8; 8 * 1024];
    let mut was_truncated = false;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        let remaining = MAX_CAPTURED_OUTPUT_BYTES.saturating_sub(captured.len());
        let keep = read.min(remaining);
        captured.extend_from_slice(&buffer[..keep]);
        was_truncated |= keep < read;
    }

    let mut text = readable_output(&captured);
    if was_truncated {
        text.push_str("\n[iHub truncated plugin output]");
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        ffi::{OsStr, OsString},
        fs,
        path::{Path, PathBuf},
        process::{Command, Stdio},
        sync::Arc,
        time::{Duration, Instant},
    };

    use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
    use image::{codecs::png::PngEncoder, ColorType, ImageEncoder};

    use crate::{background_process::background_command, models::PluginSourceLock};

    use super::{
        automatic_update_skip_reason, command_execution, command_timeout,
        configure_git_command_environment, ensure_update_security_declaration_matches,
        is_trusted_official_auto_update_source, load_manifest_artwork,
        normalized_shortcut_keywords, parse_git_source, parse_jsonl_rpc_response,
        plugin_security_declaration, read_manifest, read_source_metadata,
        resolve_official_workspace_plugin_at, snapshot_integrity, validate_manifest,
        verify_snapshot_integrity, wait_for_child_with_timeout, ChildWaitOutcome, CommandExecution,
        GitSource, GitTransportPolicy, PluginManager, PluginManifest, LEGACY_SOURCE_RECORD,
        LIFECYCLE_RECORD, LOCAL_LINKS_RECORD, MAX_COMMANDS_PER_PLUGIN, MAX_PERMISSION_LIST_ITEMS,
        MAX_PERMISSION_VALUE_CHARS, MAX_PROJECTED_ARTWORK_DATA_URL_BYTES,
        OFFICIAL_WORKSPACE_PLUGIN_SPECS, SOURCE_LOCK, UTOOLS_MAIN_PUSH_PROVIDER_ID,
    };

    fn temporary_directory(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ihub-plugin-manager-{label}-{}",
            super::unique_suffix()
        ));
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }

    fn manager_at(root: PathBuf) -> PluginManager {
        PluginManager {
            root: Arc::new(root),
            install_lock: Arc::new(std::sync::Mutex::new(())),
            automatic_update_lock: Arc::new(std::sync::Mutex::new(())),
        }
    }

    #[test]
    fn command_keyword_aliases_are_bounded_but_case_insensitive_duplicates_are_projected_once() {
        let manifest = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-keyword-aliases",
  "name": "Keyword aliases",
  "entry": { "frontend": "dist/index.html" },
  "contributes": {
    "commands": [{
      "id": "open",
      "title": "Open",
      "keywords": [" pdf ", "PDF", "合并"]
    }]
  }
}"#,
        )
        .expect("keyword alias manifest");
        validate_manifest(&manifest)
            .expect("harmless search aliases should not reject the package");
        let keywords =
            normalized_shortcut_keywords(&super::declared_commands(&manifest)[0].keywords);
        assert_eq!(keywords, vec!["pdf", "合并"]);
    }

    #[test]
    fn public_utools_manifest_projects_main_push_without_executing_preload() {
        let storage = temporary_directory("utools-manifest-projection");
        let source = storage.join("source");
        fs::create_dir_all(&source).expect("uTools source should be created");
        fs::write(source.join("index.html"), "<main>compatible</main>")
            .expect("uTools entry should be written");
        fs::write(source.join("preload.js"), "require('fs')")
            .expect("uTools preload fixture should be written");
        fs::write(source.join("child.html"), "<main>child</main>")
            .expect("uTools BrowserWindow entry should be written");
        write_test_png(&source.join("logo.png"), [10, 132, 255, 255]);
        fs::write(
            source.join("plugin.json"),
            r#"{
  "name": "utools-color-picker",
  "version": "1.0.0",
  "main": "index.html",
  "logo": "logo.png",
  "preload": "preload.js",
  "features": [{
    "code": "pick-color",
    "explain": "屏幕取色",
    "mainPush": true,
    "cmds": ["取色", { "type": "regex", "label": "从文本取色" }]
  }]
}"#,
        )
        .expect("uTools manifest should be written");

        let manifest = read_manifest(&source.join("plugin.json"))
            .expect("public uTools manifest should project safely");
        validate_manifest(&manifest).expect("projected manifest should validate");
        assert!(manifest.compatibility.is_utools());
        assert_eq!(manifest.commands.len(), 1);
        assert_eq!(manifest.commands[0].id, "utools-feature-1");
        assert_eq!(manifest.utools_commands[0].code, "pick-color");
        assert!(manifest.utools_commands[0].main_push);
        assert_eq!(manifest.utools_commands[0].keywords, vec!["取色"]);
        assert_eq!(manifest.commands[0].keywords, vec!["取色"]);
        assert!(plugin_security_declaration(&manifest)
            .permissions
            .contains("compatibility.utools.getCopyedFiles.visibleBounded"));
        assert!(plugin_security_declaration(&manifest)
            .permissions
            .contains("compatibility.utools.simulation.visibleConfirmed"));
        assert!(plugin_security_declaration(&manifest)
            .permissions
            .contains("compatibility.utools.dbCryptoStorage.osKeyringAesGcm"));
        assert!(plugin_security_declaration(&manifest)
            .permissions
            .contains("compatibility.utools.startDrag.pickerGranted"));
        assert!(plugin_security_declaration(&manifest)
            .permissions
            .contains("compatibility.utools.imagePath.pickerGranted"));
        assert!(plugin_security_declaration(&manifest)
            .permissions
            .contains("compatibility.utools.browserWindow.sandboxedIpc"));
        assert!(plugin_security_declaration(&manifest)
            .permissions
            .contains("compatibility.utools.ubrowser.hostedHttpsAutomation"));

        let plugin_id = manifest.id.clone();
        let installed = storage.join(&plugin_id);
        fs::rename(&source, &installed).expect("fixture should move under its managed identifier");
        let manager = manager_at(storage.clone());
        let bundle = manager
            .frontend_asset_bundle(&plugin_id)
            .expect("uTools root-level HTML should receive a bounded asset bundle");
        assert_eq!(
            bundle.asset_root,
            installed.canonicalize().expect("fixture root")
        );
        assert!(bundle.utools_compat.is_some());
        assert!(bundle.allows_display_capture);
        assert_eq!(bundle.blocked_asset_paths.len(), 1);
        let (browser_bundle, suffix) = manager
            .browser_frontend_asset_bundle(
                &plugin_id,
                "child.html?mode=preview#result",
                Some("preload.js"),
            )
            .expect("a sibling BrowserWindow page and explicit sandboxed preload should resolve");
        assert!(browser_bundle.entry.ends_with("child.html"));
        assert_eq!(
            browser_bundle.utools_browser_preload_src.as_deref(),
            Some("preload.js")
        );
        assert!(browser_bundle.blocked_asset_paths.is_empty());
        assert_eq!(suffix, "?mode=preview#result");
        let browser_config = browser_bundle
            .utools_compat
            .expect("BrowserWindow should keep the host compatibility shim");
        assert_eq!(browser_config.window_type, "browser");
        assert!(!browser_config.lifecycle_owner);
        for unsafe_url in [
            "../index.html",
            "%2e%2e/index.html",
            "/index.html",
            "https://example.com/index.html",
            "preload.js",
        ] {
            assert!(manager
                .browser_frontend_asset_bundle(&plugin_id, unsafe_url, None)
                .is_err());
        }
        assert!(manager
            .browser_frontend_asset_bundle(&plugin_id, "child.html", Some("../preload.js"))
            .is_err());
        assert!(manager
            .uses_utools_compatibility(&plugin_id)
            .expect("compatibility marker should be readable"));
        assert!(manager
            .has_declared_search_provider(&plugin_id, UTOOLS_MAIN_PUSH_PROVIDER_ID)
            .expect("main-push provider should be host-declared"));
        let projected = manager
            .list()
            .into_iter()
            .find(|plugin| plugin.id == plugin_id)
            .expect("compatible plugin should be listed");
        assert!(projected
            .search_providers
            .iter()
            .any(|provider| provider.id == UTOOLS_MAIN_PUSH_PROVIDER_ID));
        assert!(manager
            .allows_host_method(&plugin_id, "cursorColor.sampleOnce")
            .expect("screenColorPick should be confirmation-gated rather than ambient"));
        assert!(manager
            .allows_host_method(&plugin_id, "compatibility.utools.screen.capture")
            .expect("screenCapture should be host-confirmed for a compatible package"));
        assert!(manager
            .allows_host_method(&plugin_id, "screenCapture.acquireFocusLease")
            .expect("desktopCaptureSources should protect the system picker focus"));
        assert!(!manager
            .allows_host_method(&plugin_id, "clipboard.readText")
            .expect("uTools compatibility must not grant clipboard read"));

        let _ = fs::remove_dir_all(storage);
    }

    fn write_plugin(root: &Path, id: &str, name: &str, frontend: &str) {
        fs::create_dir_all(root.join("dist")).expect("plugin dist should be created");
        fs::write(root.join("dist/index.html"), "<main>plugin</main>")
            .expect("frontend should be written");
        fs::write(
            root.join("plugin.json"),
            format!(
                r#"{{
  "id": "{id}",
  "name": "{name}",
  "version": "0.1.0",
  "entry": {{ "frontend": "{frontend}" }}
}}"#
            ),
        )
        .expect("manifest should be written");
    }

    fn write_test_png(path: &Path, color: [u8; 4]) {
        let mut rgba = Vec::with_capacity(8 * 8 * 4);
        for _ in 0..(8 * 8) {
            rgba.extend_from_slice(&color);
        }
        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(&rgba, 8, 8, ColorType::Rgba8.into())
            .expect("test PNG encoding");
        fs::write(path, png).expect("test PNG should be written");
    }

    fn write_test_noise_png(path: &Path) {
        let mut rgba = Vec::with_capacity(128 * 128 * 4);
        let mut state = 0x91e1_0da5_u32;
        for _ in 0..(128 * 128) {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            rgba.extend_from_slice(&[state as u8, (state >> 8) as u8, (state >> 16) as u8, 255]);
        }
        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(&rgba, 128, 128, ColorType::Rgba8.into())
            .expect("noisy test PNG encoding");
        fs::write(path, png).expect("noisy test PNG should be written");
    }

    fn git_success(directory: &Path, arguments: &[&str]) -> String {
        let output = background_command("git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .output()
            .expect("git should start");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            arguments,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn tagged_bare_repository() -> (PathBuf, PathBuf, String) {
        let source = temporary_directory("git-source");
        let remote_parent = temporary_directory("git-remote-parent");
        let remote = remote_parent.join("plugin.git");
        write_plugin(
            &source,
            "ihub-plugin-pinned-demo",
            "Pinned demo",
            "dist/index.html",
        );
        git_success(&source, &["init", "--quiet"]);
        git_success(&source, &["config", "user.email", "tests@ihub.local"]);
        git_success(&source, &["config", "user.name", "iHub tests"]);
        git_success(&source, &["add", "."]);
        git_success(&source, &["commit", "--quiet", "-m", "initial plugin"]);
        git_success(&source, &["tag", "-a", "v1.2.3", "-m", "release 1.2.3"]);
        let expected_commit = git_success(&source, &["rev-parse", "v1.2.3^{commit}"]);

        let clone = background_command("git")
            .args(["clone", "--quiet", "--bare"])
            .arg(&source)
            .arg(&remote)
            .output()
            .expect("bare git clone should start");
        assert!(
            clone.status.success(),
            "bare git clone failed: {}",
            String::from_utf8_lossy(&clone.stderr)
        );
        (source, remote_parent, expected_commit)
    }

    fn bare_repository_with_plugin(plugin_id: &str, name: &str) -> (PathBuf, PathBuf) {
        let source = temporary_directory("collision-source");
        let remote_parent = temporary_directory("collision-remote-parent");
        let remote = remote_parent.join("plugin.git");
        write_plugin(&source, plugin_id, name, "dist/index.html");
        git_success(&source, &["init", "--quiet"]);
        git_success(&source, &["config", "user.email", "tests@ihub.local"]);
        git_success(&source, &["config", "user.name", "iHub tests"]);
        git_success(&source, &["add", "."]);
        git_success(&source, &["commit", "--quiet", "-m", "plugin"]);

        let clone = background_command("git")
            .args(["clone", "--quiet", "--bare"])
            .arg(&source)
            .arg(&remote)
            .output()
            .expect("bare git clone should start");
        assert!(
            clone.status.success(),
            "bare git clone failed: {}",
            String::from_utf8_lossy(&clone.stderr)
        );
        (source, remote_parent)
    }

    fn commit_and_push_plugin_update(source: &Path, version: &str) -> String {
        fs::write(
            source.join("plugin.json"),
            format!(
                r#"{{
  "id": "ihub-plugin-pinned-demo",
  "name": "Pinned demo",
  "version": "{version}",
  "entry": {{ "frontend": "dist/index.html" }}
}}"#
            ),
        )
        .expect("updated manifest should be written");
        // This tracked script is intentionally never invoked by either the
        // read-only check or explicit update path.
        fs::write(
            source.join("package.json"),
            r#"{ "scripts": { "postinstall": "this-must-not-run" } }"#,
        )
        .expect("tracked package script fixture should be written");
        git_success(source, &["add", "."]);
        git_success(source, &["commit", "--quiet", "-m", "plugin update"]);
        let branch = git_success(source, &["branch", "--show-current"]);
        let destination = format!("HEAD:refs/heads/{branch}");
        git_success(source, &["push", "--quiet", "origin", &destination]);
        git_success(source, &["rev-parse", "HEAD"])
    }

    fn commit_and_push_manifest_update(source: &Path, manifest: &str) -> String {
        fs::write(source.join("plugin.json"), manifest)
            .expect("updated manifest should be written");
        git_success(source, &["add", "."]);
        git_success(
            source,
            &["commit", "--quiet", "-m", "security declaration update"],
        );
        let branch = git_success(source, &["branch", "--show-current"]);
        let destination = format!("HEAD:refs/heads/{branch}");
        git_success(source, &["push", "--quiet", "origin", &destination]);
        git_success(source, &["rev-parse", "HEAD"])
    }

    #[cfg(target_os = "windows")]
    fn long_running_child_command() -> Command {
        let mut command = background_command("powershell");
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Start-Sleep -Seconds 5",
        ]);
        command
    }

    #[cfg(not(target_os = "windows"))]
    fn long_running_child_command() -> Command {
        let mut command = background_command("sleep");
        command.arg("5");
        command
    }

    #[test]
    fn command_timeout_kills_and_reaps_the_child() {
        let mut child = long_running_child_command()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("long-running test process should start");
        let started = Instant::now();
        let outcome = wait_for_child_with_timeout(&mut child, Duration::from_millis(120))
            .expect("timeout wait should succeed");
        assert!(matches!(outcome, ChildWaitOutcome::TimedOut));
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "timed child should not run for its full sleep duration"
        );
        assert!(
            child
                .try_wait()
                .expect("reaped child status should be readable")
                .is_some(),
            "timeout must reap the direct child process"
        );
    }

    /// Runs only when a copied test executable is launched through the native
    /// plugin command path below. Keeping the long wait inside that direct
    /// child lets the test verify that iHub terminates and reaps it.
    #[test]
    fn native_command_timeout_test_worker() {
        if std::env::var("IHUB_PLUGIN_ID").ok().as_deref() == Some("ihub-plugin-timeout-executor") {
            std::thread::sleep(Duration::from_secs(5));
        }
    }

    #[test]
    fn native_command_timeout_is_applied_and_reported_in_milliseconds() {
        let storage = temporary_directory("native-command-timeout-executor");
        let plugin_id = "ihub-plugin-timeout-executor";
        let package = storage.join(plugin_id);
        fs::create_dir_all(package.join("bin")).expect("plugin bin directory should be created");
        fs::create_dir_all(package.join("dist")).expect("plugin dist directory should be created");
        fs::write(package.join("dist/index.html"), "<main>timeout test</main>")
            .expect("plugin frontend should be written");
        fs::copy(
            std::env::current_exe().expect("current test executable should resolve"),
            package.join("bin/worker.exe"),
        )
        .expect("test executable should be copied as the plugin worker");
        fs::write(
            package.join("plugin.json"),
            format!(
                r#"{{
  "id": "{plugin_id}",
  "name": "Timeout executor",
  "entry": {{ "frontend": "dist/index.html" }},
  "backend": {{ "binary": "bin/worker.exe" }},
  "contributes": {{
    "commands": [{{
      "id": "wait",
      "title": "Wait",
      "execution": "native",
      "args": [
        "--exact",
        "plugins::tests::native_command_timeout_test_worker",
        "--nocapture"
      ],
      "run": {{ "timeoutMs": 1000 }}
    }}]
  }}
}}"#,
            ),
        )
        .expect("timeout manifest should be written");

        let started = Instant::now();
        let result = manager_at(storage.clone()).run_command(plugin_id, "wait", None);
        // Measure the command path before teardown. Windows Defender or another
        // filesystem filter may hold the copied fixture briefly after the
        // worker is reaped, and that cleanup latency is not part of the
        // command timeout contract this test covers.
        let elapsed = started.elapsed();
        let _ = fs::remove_dir_all(&storage);
        let error = result.expect_err("the sleeping worker must time out");
        assert!(
            elapsed < Duration::from_secs(3),
            "the one-second policy must not wait for the child test's full sleep"
        );
        assert!(error.contains("timed out after 1000 ms"), "{error}");
    }

    #[test]
    fn jsonl_rpc_response_requires_a_matching_single_response() {
        let (result, rpc_error) = parse_jsonl_rpc_response(
            r#"{"jsonrpc":"2.0","id":"request-1","result":{"text":"recognized"}}"#,
            "request-1",
        )
        .expect("matching JSON-RPC result should parse");
        assert_eq!(result, Some(serde_json::json!({ "text": "recognized" })));
        assert_eq!(rpc_error, None);

        assert!(parse_jsonl_rpc_response(
            r#"{"jsonrpc":"2.0","id":"other-request","result":true}"#,
            "request-1",
        )
        .expect_err("a worker must not answer a different host request")
        .contains("id"));
        assert!(parse_jsonl_rpc_response(
            "{\"jsonrpc\":\"2.0\",\"id\":\"request-1\",\"result\":true}\n{\"jsonrpc\":\"2.0\",\"id\":\"request-1\",\"result\":false}",
            "request-1",
        )
        .expect_err("exactly one stdout response line is required")
        .contains("more than one"));
    }

    #[test]
    fn jsonl_rpc_error_is_reported_as_worker_failure_data() {
        let (result, rpc_error) = parse_jsonl_rpc_response(
            r#"{"jsonrpc":"2.0","id":"request-2","error":{"code":-32001,"message":"language pack unavailable"}}"#,
            "request-2",
        )
        .expect("JSON-RPC error envelope should parse");
        assert_eq!(rpc_error.as_deref(), Some("language pack unavailable"));
        assert_eq!(
            result,
            Some(serde_json::json!({
                "message": "language pack unavailable",
                "error": { "code": -32001, "message": "language pack unavailable" }
            }))
        );
    }

    #[test]
    fn a_native_plugin_can_expose_a_frontend_activation_command() {
        let manifest = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-command-targets",
  "name": "Command targets",
  "entry": { "frontend": "dist/index.html" },
  "backend": { "binary": "bin/worker.exe" },
  "contributes": {
    "commands": [
      { "id": "open", "title": "Open OCR", "execution": "frontend" },
      { "id": "recognize", "title": "Recognize", "execution": "native" }
    ]
  }
}"#,
        )
        .expect("command target manifest should parse");
        validate_manifest(&manifest).expect("frontend command may coexist with a worker");
        let commands = super::declared_commands(&manifest);
        assert_eq!(
            command_execution(&manifest, &commands[0]),
            CommandExecution::Frontend
        );
        assert_eq!(
            command_execution(&manifest, &commands[1]),
            CommandExecution::Native
        );

        let invalid = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-invalid-command-target",
  "name": "Invalid command target",
  "contributes": {
    "commands": [{ "id": "open", "title": "Open", "execution": "foreground" }]
  }
}"#,
        )
        .expect("invalid execution fixture should parse before validation");
        assert!(validate_manifest(&invalid)
            .expect_err("unknown command execution must be rejected")
            .contains("unsupported execution"));

        let missing_worker = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-missing-worker",
  "name": "Missing worker",
  "entry": { "frontend": "dist/index.html" },
  "contributes": {
    "commands": [{ "id": "run", "title": "Run", "execution": "native" }]
  }
}"#,
        )
        .expect("missing worker fixture should parse before validation");
        assert!(validate_manifest(&missing_worker)
            .expect_err("native commands must point at a declared worker")
            .contains("requires command.binary or a declared backend binary"));
    }

    #[test]
    fn native_command_timeout_policy_defaults_and_enforces_its_bounds() {
        let default_timeout = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-timeout-default",
  "name": "Timeout default",
  "entry": { "frontend": "dist/index.html" },
  "backend": { "binary": "bin/worker.exe" },
  "contributes": {
    "commands": [{ "id": "process", "title": "Process", "execution": "native" }]
  }
}"#,
        )
        .expect("default timeout manifest should parse");
        validate_manifest(&default_timeout).expect("an omitted timeout keeps legacy behavior");
        assert_eq!(
            command_timeout(&super::declared_commands(&default_timeout)[0]),
            Duration::from_secs(60)
        );

        let bounded_timeout = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-timeout-bounded",
  "name": "Timeout bounded",
  "entry": { "frontend": "dist/index.html" },
  "backend": { "binary": "bin/worker.exe" },
  "contributes": {
    "commands": [{
      "id": "export",
      "title": "Export",
      "execution": "native",
      "run": { "timeoutMs": 1800000 }
    }]
  }
}"#,
        )
        .expect("bounded timeout manifest should parse");
        validate_manifest(&bounded_timeout).expect("the 30 minute ceiling should be accepted");
        assert_eq!(
            command_timeout(&super::declared_commands(&bounded_timeout)[0]),
            Duration::from_secs(30 * 60)
        );

        let too_short = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-timeout-short",
  "name": "Timeout short",
  "entry": { "frontend": "dist/index.html" },
  "backend": { "binary": "bin/worker.exe" },
  "contributes": {
    "commands": [{
      "id": "process",
      "title": "Process",
      "execution": "native",
      "run": { "timeoutMs": 999 }
    }]
  }
}"#,
        )
        .expect("too-short timeout manifest should deserialize before validation");
        assert!(validate_manifest(&too_short)
            .expect_err("sub-second native worker timeout must be rejected")
            .contains("between 1000 and 1800000 milliseconds"));

        let too_long = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-timeout-long",
  "name": "Timeout long",
  "entry": { "frontend": "dist/index.html" },
  "backend": { "binary": "bin/worker.exe" },
  "contributes": {
    "commands": [{
      "id": "process",
      "title": "Process",
      "execution": "native",
      "run": { "timeoutMs": 1800001 }
    }]
  }
}"#,
        )
        .expect("too-long timeout manifest should deserialize before validation");
        assert!(validate_manifest(&too_long)
            .expect_err("native worker timeout must have a hard ceiling")
            .contains("between 1000 and 1800000 milliseconds"));

        let frontend_timeout = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-timeout-frontend",
  "name": "Timeout frontend",
  "entry": { "frontend": "dist/index.html" },
  "contributes": {
    "commands": [{
      "id": "open",
      "title": "Open",
      "execution": "frontend",
      "run": { "timeoutMs": 1000 }
    }]
  }
}"#,
        )
        .expect("frontend timeout manifest should deserialize before validation");
        assert!(validate_manifest(&frontend_timeout)
            .expect_err("frontend commands must not declare a worker timeout")
            .contains("only for native execution"));

        let missing_timeout = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-timeout-missing-policy",
  "name": "Timeout missing policy",
  "entry": { "frontend": "dist/index.html" },
  "backend": { "binary": "bin/worker.exe" },
  "contributes": {
    "commands": [{
      "id": "process",
      "title": "Process",
      "execution": "native",
      "run": {}
    }]
  }
}"#,
        );
        assert!(
            missing_timeout.is_err(),
            "a native run policy must name its explicit timeout"
        );

        let unsupported_run_policy = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-timeout-unknown-policy",
  "name": "Timeout unknown policy",
  "entry": { "frontend": "dist/index.html" },
  "backend": { "binary": "bin/worker.exe" },
  "contributes": {
    "commands": [{
      "id": "process",
      "title": "Process",
      "execution": "native",
      "run": { "timeoutMs": 1000, "cancellable": true }
    }]
  }
}"#,
        );
        assert!(
            unsupported_run_policy.is_err(),
            "unknown native run policy fields must fail closed until the host implements them"
        );
    }

    #[test]
    fn frontend_commands_cannot_be_started_through_the_native_executor() {
        let storage = temporary_directory("frontend-command-executor");
        let plugin_id = "ihub-plugin-frontend-command";
        let package = storage.join(plugin_id);
        fs::create_dir_all(package.join("dist")).expect("frontend dist should be created");
        fs::write(package.join("dist/index.html"), "<main>frontend</main>")
            .expect("frontend entry should be written");
        fs::write(
            package.join("plugin.json"),
            format!(
                r#"{{
  "id": "{plugin_id}",
  "name": "Frontend command",
  "entry": {{ "frontend": "dist/index.html" }},
  "contributes": {{
    "commands": [{{ "id": "open", "title": "Open", "execution": "frontend" }}]
  }}
}}"#
            ),
        )
        .expect("frontend command manifest should be written");

        let error = manager_at(storage.clone())
            .run_command(plugin_id, "open", None)
            .expect_err("frontend commands must never fall through to a worker launch");
        assert!(error.contains("frontend command"), "{error}");
        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn parses_pinned_github_shorthand_and_url_fragments() {
        let shorthand = parse_git_source("neko233-com/ihub-plugin-demo@v1.2.3")
            .expect("pinned shorthand should parse");
        assert_eq!(
            shorthand.remote,
            "https://github.com/neko233-com/ihub-plugin-demo.git"
        );
        assert_eq!(shorthand.requested_ref, "v1.2.3");

        let url = parse_git_source(
            "https://github.com/neko233-com/ihub-plugin-demo.git#refs/tags/v1.2.3",
        )
        .expect("pinned URL should parse");
        assert_eq!(
            url.remote,
            "https://github.com/neko233-com/ihub-plugin-demo.git"
        );
        assert_eq!(url.requested_ref, "refs/tags/v1.2.3");

        let legacy = parse_git_source("github:neko233-com/ihub-plugin-demo")
            .expect("legacy shorthand should stay valid");
        assert_eq!(legacy.requested_ref, "HEAD");
        assert!(parse_git_source("owner/repo@").is_err());
        assert!(parse_git_source("https://github.com/owner/repo.git#bad ref").is_err());
        assert!(parse_git_source("http://github.com/owner/repo.git").is_err());
        assert!(parse_git_source("ssh://git@github.com/owner/repo.git").is_err());
        assert!(parse_git_source("git@github.com:owner/repo.git").is_err());
        assert!(parse_git_source("https://ghp_secret_token@github.com/owner/repo.git").is_err());
        assert!(parse_git_source("https://user:password@github.com/owner/repo.git").is_err());
        assert!(parse_git_source("https://github.com/owner/repo.git?access_token=secret").is_err());
    }

    #[test]
    fn automatic_update_discovery_requires_an_exact_official_stable_lock() {
        assert!(is_trusted_official_auto_update_source(
            "https://github.com/neko233-com/ihub-plugin-demo.git"
        ));
        assert!(is_trusted_official_auto_update_source(
            "https://github.com/neko233-com/ihub-plugin-demo"
        ));
        assert!(!is_trusted_official_auto_update_source(
            "https://github.com/neko233-com.example/ihub-plugin-demo.git"
        ));
        assert!(!is_trusted_official_auto_update_source(
            "https://github.com/neko233-com/ihub-plugin-demo.git/extra"
        ));
        assert!(!is_trusted_official_auto_update_source(
            "git@github.com:neko233-com/ihub-plugin-demo.git"
        ));

        let storage = temporary_directory("automatic-update-policy");
        let plugin_id = "ihub-plugin-auto-policy";
        let package = storage.join(plugin_id);
        write_plugin(&package, plugin_id, "Automatic policy", "dist/index.html");
        fs::write(
            package.join("plugin.json"),
            format!(
                r#"{{
  "id": "{plugin_id}",
  "name": "Automatic policy",
  "version": "0.1.0",
  "entry": {{ "frontend": "dist/index.html" }},
  "update": {{ "channel": "stable", "autoUpdate": true }}
}}"#
            ),
        )
        .expect("automatic-update manifest should be written");

        let manager = manager_at(storage.clone());
        let listed = manager.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].update_channel.as_deref(), Some("stable"));
        assert!(listed[0].auto_update);
        assert!(automatic_update_skip_reason(&listed[0])
            .expect("an unpinned local fixture must not be automatically checked")
            .contains("immutable Git source lock"));
        let report = manager.check_automatic_updates();
        assert!(
            report.checks.is_empty(),
            "an unpinned package must not make a Git call"
        );
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skipped[0].plugin_id, plugin_id);
        assert!(report.skipped[0]
            .reason
            .contains("immutable Git source lock"));

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn automatic_update_discovery_requires_verified_snapshot_integrity() {
        let storage = temporary_directory("automatic-update-integrity");
        let plugin_id = "ihub-plugin-auto-integrity";
        let package = storage.join(plugin_id);
        write_plugin(
            &package,
            plugin_id,
            "Automatic integrity",
            "dist/index.html",
        );
        fs::write(
            package.join("plugin.json"),
            format!(
                r#"{{
  "id": "{plugin_id}",
  "name": "Automatic integrity",
  "version": "0.1.0",
  "entry": {{ "frontend": "dist/index.html" }},
  "update": {{ "channel": "stable", "autoUpdate": true }}
}}"#
            ),
        )
        .expect("automatic-integrity manifest should be written");
        fs::write(
            package.join(SOURCE_LOCK),
            r#"{
  "source": "https://github.com/neko233-com/ihub-plugin-auto-integrity.git",
  "requestedRef": "HEAD",
  "resolvedCommit": "0123456789abcdef0123456789abcdef01234567",
  "installedAt": "2026-01-01T00:00:00Z"
}"#,
        )
        .expect("legacy source lock should be written");

        let manager = manager_at(storage.clone());
        let listed = manager.list();
        assert_eq!(listed.len(), 1);
        assert!(automatic_update_skip_reason(&listed[0])
            .expect("a legacy lock must not be probed automatically")
            .contains("verified snapshot integrity"));

        let report = manager.check_automatic_updates();
        assert!(
            report.checks.is_empty(),
            "legacy locks must not contact Git"
        );
        assert_eq!(report.skipped.len(), 1);
        assert!(report.skipped[0]
            .reason
            .contains("verified snapshot integrity"));

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn automatic_update_discovery_verifies_snapshot_before_any_git_probe() {
        let storage = temporary_directory("automatic-update-tampered-integrity");
        let plugin_id = "ihub-plugin-auto-tampered";
        let package = storage.join(plugin_id);
        write_plugin(&package, plugin_id, "Automatic tampered", "dist/index.html");
        fs::write(
            package.join("plugin.json"),
            format!(
                r#"{{
  "id": "{plugin_id}",
  "name": "Automatic tampered",
  "version": "0.1.0",
  "entry": {{ "frontend": "dist/index.html" }},
  "update": {{ "channel": "stable", "autoUpdate": true }}
}}"#
            ),
        )
        .expect("automatic-tampered manifest should be written");
        fs::write(
            package.join(SOURCE_LOCK),
            r#"{
  "source": "https://github.com/neko233-com/ihub-plugin-auto-tampered.git",
  "requestedRef": "HEAD",
  "resolvedCommit": "0123456789abcdef0123456789abcdef01234567",
  "installedAt": "2026-01-01T00:00:00Z",
  "integrity": {
    "algorithm": "sha256",
    "manifestSha256": "0000000000000000000000000000000000000000000000000000000000000000",
    "frontendAssets": [],
    "nativeBinaries": []
  }
}"#,
        )
        .expect("tampered source lock should be written");

        let report = manager_at(storage.clone()).check_automatic_updates();
        assert!(
            report.checks.is_empty(),
            "a tampered snapshot must not contact Git"
        );
        assert_eq!(report.skipped.len(), 1);
        assert!(report.skipped[0].reason.contains("could not be verified"));
        assert!(report.skipped[0]
            .reason
            .contains("immutable integrity check"));

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn automatic_update_discovery_skips_disabled_plugins_before_any_git_request() {
        let storage = temporary_directory("automatic-update-disabled");
        let plugin_id = "ihub-plugin-auto-disabled";
        let package = storage.join(plugin_id);
        write_plugin(&package, plugin_id, "Automatic disabled", "dist/index.html");
        fs::write(
            package.join("plugin.json"),
            format!(
                r#"{{
  "id": "{plugin_id}",
  "name": "Automatic disabled",
  "version": "0.1.0",
  "entry": {{ "frontend": "dist/index.html" }},
  "update": {{ "channel": "stable", "autoUpdate": true }}
}}"#
            ),
        )
        .expect("automatic-disabled manifest should be written");
        fs::write(
            package.join(SOURCE_LOCK),
            r#"{
  "source": "https://github.com/neko233-com/ihub-plugin-auto-disabled.git",
  "requestedRef": "HEAD",
  "resolvedCommit": "0123456789abcdef0123456789abcdef01234567",
  "installedAt": "2026-01-01T00:00:00Z"
}"#,
        )
        .expect("source lock should be written");

        let manager = manager_at(storage.clone());
        manager
            .set_enabled(plugin_id, false)
            .expect("plugin should be disabled");
        let disabled = manager.list().pop().expect("plugin should stay listed");
        assert!(automatic_update_skip_reason(&disabled)
            .expect("disabled plugins must be skipped")
            .contains("Disabled plugins"));

        let report = manager.check_automatic_updates();
        assert!(
            report.checks.is_empty(),
            "disabled plugins must not contact Git"
        );
        assert_eq!(report.skipped.len(), 1);
        assert!(report.skipped[0].reason.contains("Disabled plugins"));

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn automatic_update_discovery_skips_instead_of_waiting_for_install_lock() {
        let storage = temporary_directory("automatic-update-install-lock");
        let plugin_id = "ihub-plugin-auto-lock";
        let package = storage.join(plugin_id);
        write_plugin(&package, plugin_id, "Automatic lock", "dist/index.html");
        fs::write(
            package.join("plugin.json"),
            format!(
                r#"{{
  "id": "{plugin_id}",
  "name": "Automatic lock",
  "version": "0.1.0",
  "entry": {{ "frontend": "dist/index.html" }},
  "update": {{ "channel": "stable", "autoUpdate": true }}
}}"#
            ),
        )
        .expect("automatic-lock manifest should be written");
        fs::write(
            package.join(SOURCE_LOCK),
            r#"{
  "source": "https://github.com/neko233-com/ihub-plugin-auto-lock.git",
  "requestedRef": "HEAD",
  "resolvedCommit": "0123456789abcdef0123456789abcdef01234567",
  "installedAt": "2026-01-01T00:00:00Z",
  "integrity": {
    "algorithm": "sha256",
    "manifestSha256": "0000000000000000000000000000000000000000000000000000000000000000",
    "frontendAssets": [],
    "nativeBinaries": []
  }
}"#,
        )
        .expect("source lock should be written");

        let manager = manager_at(storage.clone());
        let install_guard = manager
            .install_lock
            .lock()
            .expect("test install lock should be acquired");
        let started = Instant::now();
        let report = manager.check_automatic_updates();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "automatic discovery must skip rather than queue behind an installation"
        );
        drop(install_guard);
        assert!(report.checks.is_empty());
        assert_eq!(report.skipped.len(), 1);
        assert!(report.skipped[0]
            .reason
            .contains("skipped instead of waiting for the install lock"));

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn official_automatic_git_environment_rejects_config_injection_and_non_https_transport() {
        let mut command = background_command("git");
        configure_git_command_environment(
            &mut command,
            GitTransportPolicy::OfficialHttps,
            vec![
                (OsString::from("GIT_CONFIG_COUNT"), OsString::from("1")),
                (
                    OsString::from("GIT_CONFIG_KEY_0"),
                    OsString::from("url.bad.insteadOf"),
                ),
                (
                    OsString::from("GIT_CONFIG_VALUE_0"),
                    OsString::from("https://github.com/neko233-com/"),
                ),
                (
                    OsString::from("GIT_CONFIG_PARAMETERS"),
                    OsString::from("url.bad.insteadOf=https://github.com/neko233-com/"),
                ),
                (OsString::from("GIT_DIR"), OsString::from("C:\\untrusted")),
                (
                    OsString::from("git_askpass"),
                    OsString::from("C:\\untrusted\\credential-window.exe"),
                ),
                (
                    OsString::from("SSH_ASKPASS"),
                    OsString::from("C:\\untrusted\\ssh-window.exe"),
                ),
                (
                    OsString::from("GIT_SSH_COMMAND"),
                    OsString::from("C:\\untrusted\\ssh.exe --capture"),
                ),
            ],
        );
        let environment = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect::<BTreeMap<_, _>>();
        for key in [
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_KEY_0",
            "GIT_CONFIG_VALUE_0",
            "GIT_CONFIG_PARAMETERS",
            "GIT_DIR",
            "git_askpass",
            "SSH_ASKPASS",
            "GIT_SSH_COMMAND",
        ] {
            assert_eq!(
                environment.get(key),
                Some(&None),
                "{key} must be removed from the child Git environment"
            );
        }
        assert_eq!(
            environment.get("GIT_ALLOW_PROTOCOL"),
            Some(&Some("https".to_owned()))
        );
        assert_eq!(
            environment.get("GIT_PROTOCOL_FROM_USER"),
            Some(&Some("0".to_owned()))
        );
        assert_eq!(
            environment.get("GIT_CONFIG_NOSYSTEM"),
            Some(&Some("1".to_owned()))
        );
    }

    #[test]
    fn security_declaration_blocks_a_native_only_change() {
        let installed = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-security-test",
  "name": "Security test",
  "entry": { "frontend": "dist/index.html" }
}"#,
        )
        .expect("installed manifest should parse");
        let candidate = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-security-test",
  "name": "Security test",
  "entry": { "frontend": "dist/index.html" },
  "backend": { "binary": "bin/worker.exe" }
}"#,
        )
        .expect("candidate manifest should parse");

        let error = ensure_update_security_declaration_matches(
            "ihub-plugin-security-test",
            &installed,
            &candidate,
        )
        .expect_err("adding a native worker must never be a routine update");
        assert!(error.contains("native binary declarations"), "{error}");
        assert!(!error.contains("permissions and"), "{error}");
    }

    #[test]
    fn security_declaration_blocks_a_command_execution_flip_to_native() {
        let installed = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-command-trust-test",
  "name": "Command trust test",
  "entry": { "frontend": "dist/index.html" },
  "backend": { "binary": "bin/worker.exe" },
  "contributes": {
    "commands": [{ "id": "open", "title": "Open", "execution": "frontend" }]
  }
}"#,
        )
        .expect("installed command manifest should parse");
        let candidate = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-command-trust-test",
  "name": "Command trust test",
  "entry": { "frontend": "dist/index.html" },
  "backend": { "binary": "bin/worker.exe" },
  "contributes": {
    "commands": [{ "id": "open", "title": "Open", "execution": "native" }]
  }
}"#,
        )
        .expect("candidate command manifest should parse");

        let error = ensure_update_security_declaration_matches(
            "ihub-plugin-command-trust-test",
            &installed,
            &candidate,
        )
        .expect_err("a same-version frontend-to-native flip needs a fresh trust action");
        assert!(error.contains("native binary declarations"), "{error}");
    }

    #[test]
    fn security_declaration_blocks_shared_backend_command_argument_changes() {
        let installed = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-command-args-test",
  "name": "Command args test",
  "entry": { "frontend": "dist/index.html" },
  "backend": { "binary": "bin/worker.exe" },
  "contributes": {
    "commands": [{ "id": "process", "title": "Process", "args": ["--safe"] }]
  }
}"#,
        )
        .expect("installed shared-worker manifest should parse");
        let candidate = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-command-args-test",
  "name": "Command args test",
  "entry": { "frontend": "dist/index.html" },
  "backend": { "binary": "bin/worker.exe" },
  "contributes": {
    "commands": [{ "id": "process", "title": "Process", "args": ["--unsafe"] }]
  }
}"#,
        )
        .expect("candidate shared-worker manifest should parse");

        let error = ensure_update_security_declaration_matches(
            "ihub-plugin-command-args-test",
            &installed,
            &candidate,
        )
        .expect_err("changing arguments for a shared native worker needs fresh trust");
        assert!(error.contains("native binary declarations"), "{error}");
    }

    #[test]
    fn security_declaration_blocks_native_command_timeout_expansion() {
        let installed = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-command-timeout-trust-test",
  "name": "Command timeout trust test",
  "entry": { "frontend": "dist/index.html" },
  "backend": { "binary": "bin/worker.exe" },
  "contributes": {
    "commands": [{
      "id": "export",
      "title": "Export",
      "execution": "native",
      "run": { "timeoutMs": 60000 }
    }]
  }
}"#,
        )
        .expect("installed timeout manifest should parse");
        let candidate = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-command-timeout-trust-test",
  "name": "Command timeout trust test",
  "entry": { "frontend": "dist/index.html" },
  "backend": { "binary": "bin/worker.exe" },
  "contributes": {
    "commands": [{
      "id": "export",
      "title": "Export",
      "execution": "native",
      "run": { "timeoutMs": 120000 }
    }]
  }
}"#,
        )
        .expect("candidate timeout manifest should parse");
        validate_manifest(&installed).expect("installed timeout should be valid");
        validate_manifest(&candidate).expect("candidate timeout should be valid");

        let error = ensure_update_security_declaration_matches(
            "ihub-plugin-command-timeout-trust-test",
            &installed,
            &candidate,
        )
        .expect_err("a routine Git update must not expand native worker runtime");
        assert!(error.contains("native binary declarations"), "{error}");
    }

    #[test]
    fn manifest_rejects_duplicate_backend_binary_targets() {
        let manifest = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-duplicate-target-test",
  "name": "Duplicate target test",
  "entry": { "frontend": "dist/index.html" },
  "backend": {
    "protocol": "jsonl-rpc-v1",
    "binaries": [
      { "target": "windows-x86_64", "path": "bin/first.exe", "args": ["--safe"] },
      { "target": "windows-x86_64", "path": "bin/second.exe", "args": ["--unsafe"] }
    ]
  }
}"#,
        )
        .expect("duplicate-target manifest should deserialize");

        let error = validate_manifest(&manifest)
            .expect_err("a platform target must not resolve to an order-dependent worker");
        assert!(error.contains("windows-x86_64"), "{error}");
        assert!(error.contains("at most once"), "{error}");
    }

    #[test]
    fn manifest_rejects_duplicate_command_ids() {
        let manifest = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-duplicate-command-test",
  "name": "Duplicate command test",
  "entry": { "frontend": "dist/index.html" },
  "contributes": {
    "commands": [
      { "id": "open", "title": "Open" },
      { "id": "open", "title": "Different open" }
    ]
  }
}"#,
        )
        .expect("duplicate-command manifest should deserialize");

        let error = validate_manifest(&manifest)
            .expect_err("a command lookup must never be order-dependent");
        assert!(error.contains("command 'open'"), "{error}");
        assert!(error.contains("more than once"), "{error}");
    }

    #[test]
    fn manifest_global_shortcuts_require_permission_and_declared_targets() {
        let missing_permission = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-shortcut-permission",
  "name": "Shortcut permission",
  "entry": { "frontend": "dist/index.html" },
  "contributes": {
    "commands": [
      { "id": "open", "title": "Open", "shortcut": "Alt+KeyO" }
    ]
  }
}"#,
        )
        .expect("shortcut manifest should deserialize");
        let error = validate_manifest(&missing_permission)
            .expect_err("a manifest shortcut is an explicit host capability");
        assert!(error.contains("permissions.globalShortcut"), "{error}");

        let unknown_target = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-shortcut-target",
  "name": "Shortcut target",
  "entry": { "frontend": "dist/index.html" },
  "permissions": { "globalShortcut": true },
  "contributes": {
    "commands": [{ "id": "open", "title": "Open" }],
    "globalShortcuts": [{
      "id": "missing",
      "shortcut": "Alt+KeyM",
      "commandId": "not-declared"
    }]
  }
}"#,
        )
        .expect("shortcut target manifest should deserialize");
        let error = validate_manifest(&unknown_target)
            .expect_err("a global mapping must target a declared command");
        assert!(error.contains("undeclared command"), "{error}");
    }

    #[test]
    fn manifest_global_shortcuts_reject_system_launcher_and_duplicate_bindings() {
        let reserved = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-shortcut-reserved",
  "name": "Shortcut reserved",
  "entry": { "frontend": "dist/index.html" },
  "permissions": { "globalShortcut": true },
  "contributes": {
    "commands": [
      { "id": "open", "title": "Open", "shortcut": "Alt+Space" }
    ]
  }
}"#,
        )
        .expect("reserved shortcut manifest should deserialize");
        let error =
            validate_manifest(&reserved).expect_err("Alt+Space belongs to the main launcher");
        assert!(error.contains("reserved"), "{error}");

        let duplicate = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-shortcut-duplicate",
  "name": "Shortcut duplicate",
  "entry": { "frontend": "dist/index.html" },
  "permissions": { "globalShortcut": true },
  "contributes": {
    "commands": [
      { "id": "open", "title": "Open", "shortcut": "Alt+KeyO" }
    ],
    "globalShortcuts": [{
      "id": "find",
      "shortcut": "alt + keyo",
      "keyword": "find"
    }]
  }
}"#,
        )
        .expect("duplicate shortcut manifest should deserialize");
        let error = validate_manifest(&duplicate)
            .expect_err("canonical duplicate accelerators must not be order-dependent");
        assert!(error.contains("more than once"), "{error}");
    }

    #[test]
    fn security_declaration_locks_shortcut_target_changes() {
        let installed = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-shortcut-lock",
  "name": "Shortcut lock",
  "entry": { "frontend": "dist/index.html" },
  "permissions": { "globalShortcut": true },
  "contributes": {
    "commands": [{ "id": "open", "title": "Open" }],
    "globalShortcuts": [{
      "id": "action",
      "shortcut": "Alt+KeyO",
      "commandId": "open"
    }]
  }
}"#,
        )
        .expect("installed shortcut manifest should deserialize");
        let candidate = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-shortcut-lock",
  "name": "Shortcut lock",
  "entry": { "frontend": "dist/index.html" },
  "permissions": { "globalShortcut": true },
  "contributes": {
    "commands": [{ "id": "open", "title": "Open" }],
    "globalShortcuts": [{
      "id": "action",
      "shortcut": "Alt+KeyO",
      "keyword": "open something else"
    }]
  }
}"#,
        )
        .expect("candidate shortcut manifest should deserialize");
        validate_manifest(&installed).unwrap();
        validate_manifest(&candidate).unwrap();
        let error = ensure_update_security_declaration_matches(
            "ihub-plugin-shortcut-lock",
            &installed,
            &candidate,
        )
        .expect_err("routine updates may not silently retarget a global shortcut");
        assert!(error.contains("native binary declarations"), "{error}");
    }

    #[test]
    fn manifest_rejects_ambiguous_identity_but_legacy_command_artwork_degrades() {
        let ambiguous = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-artwork-manifest",
  "name": "Artwork manifest",
  "icon": "assets/icon.png",
  "logo": "assets/logo.png",
  "entry": { "frontend": "dist/index.html" }
}"#,
        )
        .expect("ambiguous artwork manifest should deserialize");
        let error = validate_manifest(&ambiguous)
            .expect_err("icon and logo cannot make plugin identity order-dependent");
        assert!(error.contains("only one"), "{error}");

        let traversal = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-artwork-traversal",
  "name": "Artwork traversal",
  "entry": { "frontend": "dist/index.html" },
  "contributes": {
    "commands": [{ "id": "open", "title": "Open", "icon": "../escape.png" }]
  }
}"#,
        )
        .expect("traversal artwork manifest should deserialize");
        validate_manifest(&traversal)
            .expect("legacy command artwork declarations remain compatible");
        let package = temporary_directory("legacy-command-artwork-path");
        let artwork = load_manifest_artwork(&package, &traversal)
            .expect("unsafe legacy command artwork must degrade without reading it");
        assert!(artwork.is_empty());
        fs::remove_dir_all(package).expect("remove legacy command artwork fixture");
    }

    #[test]
    fn manifest_bounds_commands_before_artwork_projection() {
        let commands = (0..=MAX_COMMANDS_PER_PLUGIN)
            .map(|index| {
                serde_json::json!({
                    "id": format!("command-{index}"),
                    "title": format!("Command {index}"),
                    "icon": "assets/shared.png"
                })
            })
            .collect::<Vec<_>>();
        let manifest = serde_json::from_value::<PluginManifest>(serde_json::json!({
            "id": "ihub-plugin-command-limit",
            "name": "Command limit",
            "entry": { "frontend": "dist/index.html" },
            "contributes": { "commands": commands }
        }))
        .expect("command-limit manifest should deserialize");

        let error = validate_manifest(&manifest)
            .expect_err("a manifest cannot create an unbounded command projection");
        assert!(
            error.contains(&MAX_COMMANDS_PER_PLUGIN.to_string()),
            "{error}"
        );
    }

    #[test]
    fn legacy_svg_command_artwork_keeps_the_plugin_available() {
        let storage = temporary_directory("legacy-svg-command-artwork");
        let plugin_id = "ihub-plugin-legacy-svg-artwork";
        let package = storage.join(plugin_id);
        fs::create_dir_all(package.join("dist")).expect("frontend directory");
        fs::create_dir_all(package.join("public")).expect("public directory");
        fs::write(package.join("dist/index.html"), "<main>legacy SVG</main>")
            .expect("frontend fixture");
        fs::write(
            package.join("public/icon.svg"),
            r#"<svg xmlns="http://www.w3.org/2000/svg"><rect width="16" height="16"/></svg>"#,
        )
        .expect("legacy SVG fixture");
        fs::write(
            package.join("plugin.json"),
            format!(
                r#"{{
  "id": "{plugin_id}",
  "name": "Legacy SVG artwork",
  "entry": {{ "frontend": "dist/index.html" }},
  "contributes": {{
    "commands": [{{
      "id": "open",
      "title": "Open",
      "icon": "public/icon.svg"
    }}]
  }}
}}"#
            ),
        )
        .expect("legacy SVG manifest");

        let listed = manager_at(storage.clone()).list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, plugin_id);
        assert!(listed[0].commands[0].icon_src.is_none());

        fs::remove_dir_all(storage).expect("remove legacy SVG fixture");
    }

    #[test]
    fn command_artwork_projection_has_a_total_serialized_budget() {
        let storage = temporary_directory("artwork-projection-budget");
        let plugin_id = "ihub-plugin-artwork-budget";
        let package = storage.join(plugin_id);
        fs::create_dir_all(package.join("dist")).expect("frontend directory");
        fs::create_dir_all(package.join("assets")).expect("artwork directory");
        fs::write(package.join("dist/index.html"), "<main>budget</main>")
            .expect("frontend fixture");
        write_test_noise_png(&package.join("assets/noise.png"));
        let commands = (0..MAX_COMMANDS_PER_PLUGIN)
            .map(|index| {
                serde_json::json!({
                    "id": format!("command-{index}"),
                    "title": format!("Command {index}"),
                    "icon": "assets/noise.png"
                })
            })
            .collect::<Vec<_>>();
        fs::write(
            package.join("plugin.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "id": plugin_id,
                "name": "Artwork budget",
                "entry": { "frontend": "dist/index.html" },
                "contributes": { "commands": commands }
            }))
            .expect("serialize artwork-budget manifest"),
        )
        .expect("artwork-budget manifest");

        let plugin = manager_at(storage.clone())
            .read_plugin_info(&package)
            .expect("bounded artwork projection");
        let projected_bytes = plugin
            .commands
            .iter()
            .filter_map(|command| command.icon_src.as_ref())
            .map(String::len)
            .sum::<usize>();
        assert!(projected_bytes <= MAX_PROJECTED_ARTWORK_DATA_URL_BYTES);
        assert!(
            plugin
                .commands
                .iter()
                .any(|command| command.icon_src.is_none()),
            "the noisy repeated image should exhaust the bounded command-artwork budget"
        );

        fs::remove_dir_all(storage).expect("remove artwork-budget fixture");
    }

    #[test]
    fn plugin_and_command_artwork_are_normalized_and_integrity_locked() {
        let storage = temporary_directory("artwork-projection");
        let plugin_id = "ihub-plugin-artwork-projection";
        let package = storage.join(plugin_id);
        fs::create_dir_all(package.join("dist")).expect("frontend directory");
        fs::create_dir_all(package.join("assets")).expect("artwork directory");
        fs::write(package.join("dist/index.html"), "<main>artwork</main>")
            .expect("frontend fixture");
        write_test_png(&package.join("assets/icon.png"), [24, 180, 140, 255]);
        fs::write(
            package.join("plugin.json"),
            format!(
                r#"{{
  "id": "{plugin_id}",
  "name": "Artwork projection",
  "version": "1.0.0",
  "icon": "assets/icon.png",
  "entry": {{ "frontend": "dist/index.html" }},
  "contributes": {{
    "commands": [{{
      "id": "open",
      "title": "Open",
      "icon": "assets/icon.png"
    }}]
  }}
}}"#
            ),
        )
        .expect("artwork manifest");

        let manager = manager_at(storage.clone());
        let plugin = manager
            .read_plugin_info(&package)
            .expect("valid artwork plugin");
        let plugin_icon = plugin.icon_src.expect("plugin icon projection");
        let command_icon = plugin.commands[0]
            .icon_src
            .as_ref()
            .expect("command icon projection");
        assert_eq!(&plugin_icon, command_icon);
        assert!(plugin_icon.starts_with("data:image/png;base64,"));
        assert!(!plugin_icon.contains("assets/icon.png"));
        let png = BASE64_STANDARD
            .decode(
                plugin_icon
                    .strip_prefix("data:image/png;base64,")
                    .expect("PNG data URL"),
            )
            .expect("base64 artwork");
        let normalized = image::load_from_memory_with_format(&png, image::ImageFormat::Png)
            .expect("normalized artwork");
        assert_eq!((normalized.width(), normalized.height()), (8, 8));

        let manifest_path = package.join("plugin.json");
        let manifest = super::read_manifest(&manifest_path).expect("artwork manifest");
        let integrity =
            snapshot_integrity(&package, &manifest_path, &manifest).expect("artwork integrity");
        let artwork_assets = integrity
            .artwork_assets
            .as_ref()
            .expect("new integrity always covers artwork");
        assert_eq!(artwork_assets.len(), 1);
        assert_eq!(artwork_assets[0].path, "assets/icon.png");
        let source_lock = PluginSourceLock {
            source: "https://example.invalid/artwork.git".to_owned(),
            requested_ref: "v1.0.0".to_owned(),
            resolved_commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            installed_at: "2026-07-29T00:00:00Z".to_owned(),
            integrity: Some(integrity),
        };

        write_test_png(&package.join("assets/icon.png"), [180, 72, 96, 255]);
        let canonical_package = package.canonicalize().expect("canonical plugin package");
        let error = verify_snapshot_integrity(&canonical_package, &source_lock)
            .expect_err("changed artwork must fail a current integrity lock");
        assert!(error.contains("plugin artwork"), "{error}");

        fs::remove_dir_all(storage).expect("remove artwork projection fixture");
    }

    #[test]
    fn list_never_projects_artwork_from_a_tampered_managed_snapshot() {
        let storage = temporary_directory("artwork-list-integrity");
        let plugin_id = "ihub-plugin-artwork-list-integrity";
        let package = storage.join(plugin_id);
        fs::create_dir_all(package.join("dist")).expect("frontend directory");
        fs::create_dir_all(package.join("assets")).expect("artwork directory");
        fs::write(package.join("dist/index.html"), "<main>artwork list</main>")
            .expect("frontend fixture");
        write_test_png(&package.join("assets/icon.png"), [20, 160, 120, 255]);
        fs::write(
            package.join("plugin.json"),
            format!(
                r#"{{
  "id": "{plugin_id}",
  "name": "Artwork list integrity",
  "version": "1.0.0",
  "icon": "assets/icon.png",
  "entry": {{ "frontend": "dist/index.html" }},
  "contributes": {{
    "commands": [{{
      "id": "open",
      "title": "Open",
      "icon": "assets/icon.png"
    }}]
  }}
}}"#
            ),
        )
        .expect("artwork-list manifest");

        let manifest_path = package.join("plugin.json");
        let manifest = super::read_manifest(&manifest_path).expect("artwork-list manifest");
        let integrity =
            snapshot_integrity(&package, &manifest_path, &manifest).expect("artwork-list lock");
        let source_lock = PluginSourceLock {
            source: "https://github.com/neko233-com/ihub-plugin-artwork-list-integrity.git"
                .to_owned(),
            requested_ref: "v1.0.0".to_owned(),
            resolved_commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            installed_at: "2026-07-29T00:00:00Z".to_owned(),
            integrity: Some(integrity),
        };
        super::write_source_lock(&package, &source_lock).expect("write artwork-list source lock");

        let manager = manager_at(storage.clone());
        let canonical_package = package
            .canonicalize()
            .expect("canonical artwork-list package");
        manager
            .verify_managed_snapshot_integrity(&canonical_package)
            .expect("fresh artwork-list lock must verify");
        let verified = manager.list();
        assert!(verified[0].icon_src.is_some());
        assert!(verified[0].commands[0].icon_src.is_some());

        write_test_png(&package.join("assets/icon.png"), [190, 50, 80, 255]);
        let tampered = manager.list();
        assert_eq!(tampered.len(), 1, "management metadata remains visible");
        assert!(
            tampered[0].icon_src.is_none(),
            "unverified plugin identity must not cross IPC"
        );
        assert!(tampered[0].commands[0].icon_src.is_none());

        fs::remove_dir_all(storage).expect("remove artwork-list fixture");
    }

    #[test]
    fn pre_artwork_integrity_locks_remain_backward_compatible() {
        let storage = temporary_directory("legacy-artwork-integrity");
        let plugin_id = "ihub-plugin-legacy-artwork";
        let package = storage.join(plugin_id);
        fs::create_dir_all(package.join("dist")).expect("frontend directory");
        fs::create_dir_all(package.join("assets")).expect("artwork directory");
        fs::write(
            package.join("dist/index.html"),
            "<main>legacy artwork</main>",
        )
        .expect("frontend fixture");
        write_test_png(&package.join("assets/icon.png"), [20, 40, 60, 255]);
        fs::write(
            package.join("plugin.json"),
            format!(
                r#"{{
  "id": "{plugin_id}",
  "name": "Legacy artwork",
  "version": "1.0.0",
  "icon": "assets/icon.png",
  "entry": {{ "frontend": "dist/index.html" }}
}}"#
            ),
        )
        .expect("legacy artwork manifest");

        let manifest_path = package.join("plugin.json");
        let manifest = super::read_manifest(&manifest_path).expect("legacy artwork manifest");
        let mut integrity =
            snapshot_integrity(&package, &manifest_path, &manifest).expect("current integrity");
        integrity.artwork_assets = None;
        let source_lock = PluginSourceLock {
            source: "https://example.invalid/legacy.git".to_owned(),
            requested_ref: "v1.0.0".to_owned(),
            resolved_commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
            installed_at: "2026-07-29T00:00:00Z".to_owned(),
            integrity: Some(integrity),
        };

        write_test_png(&package.join("assets/icon.png"), [90, 80, 70, 255]);
        let canonical_package = package.canonicalize().expect("canonical plugin package");
        verify_snapshot_integrity(&canonical_package, &source_lock)
            .expect("a legacy lock retains its previous verification contract");

        fs::remove_dir_all(storage).expect("remove legacy artwork fixture");
    }

    #[test]
    fn pinned_git_install_writes_and_exposes_the_source_lock() {
        let storage = temporary_directory("pinned-storage");
        let (source, remote_parent, expected_commit) = tagged_bare_repository();
        let remote = remote_parent.join("plugin.git");
        let manager = manager_at(storage.clone());

        let installed = manager
            .install_from_remote(GitSource {
                remote: remote.to_string_lossy().into_owned(),
                requested_ref: "v1.2.3".to_owned(),
            })
            .expect("pinned tag should install");
        let lock = installed
            .source_lock
            .expect("installed plugin exposes source lock");
        assert_eq!(lock.source, remote.to_string_lossy());
        assert_eq!(lock.requested_ref, "v1.2.3");
        assert_eq!(lock.resolved_commit, expected_commit);
        let integrity = lock
            .integrity
            .as_ref()
            .expect("new Git imports must capture runtime integrity");
        assert_eq!(integrity.algorithm, "sha256");
        assert_eq!(integrity.frontend_assets.len(), 1);
        assert_eq!(integrity.frontend_assets[0].path, "dist/index.html");
        assert_eq!(integrity.artwork_assets.as_deref(), Some(&[][..]));
        assert!(integrity.native_binaries.is_empty());
        assert_eq!(installed.commit.as_deref(), Some(expected_commit.as_str()));
        assert!(storage
            .join("ihub-plugin-pinned-demo")
            .join(SOURCE_LOCK)
            .is_file());

        let listed = manager.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0]
                .source_lock
                .as_ref()
                .map(|source_lock| source_lock.requested_ref.as_str()),
            Some("v1.2.3")
        );
        assert_eq!(
            listed[0]
                .source_lock
                .as_ref()
                .map(|source_lock| source_lock.resolved_commit.as_str()),
            Some(expected_commit.as_str())
        );

        let _ = fs::remove_dir_all(storage);
        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(remote_parent);
    }

    #[test]
    fn legacy_integrity_lock_remains_manually_checkable() {
        let storage = temporary_directory("legacy-integrity-manual-check");
        let (source, remote_parent, expected_commit) = tagged_bare_repository();
        let remote = remote_parent.join("plugin.git");
        let manager = manager_at(storage.clone());
        let plugin_id = "ihub-plugin-pinned-demo";

        manager
            .install_from_remote(GitSource {
                remote: remote.to_string_lossy().into_owned(),
                requested_ref: "v1.2.3".to_owned(),
            })
            .expect("initial Git snapshot should install");
        let lock_path = storage.join(plugin_id).join(SOURCE_LOCK);
        let mut legacy_lock = serde_json::from_slice::<serde_json::Value>(
            &fs::read(&lock_path).expect("current source lock should be readable"),
        )
        .expect("current source lock should be JSON");
        legacy_lock
            .as_object_mut()
            .expect("source lock should be an object")
            .remove("integrity");
        fs::write(
            &lock_path,
            serde_json::to_vec_pretty(&legacy_lock).expect("legacy lock should serialize"),
        )
        .expect("legacy source lock should be written");

        let check = manager
            .check_git_update(plugin_id)
            .expect("a deliberate manual check must remain available for legacy locks");
        assert_eq!(check.current_commit, expected_commit);
        assert!(!check.update_available);

        let _ = fs::remove_dir_all(storage);
        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(remote_parent);
    }

    #[test]
    fn verified_git_snapshot_refuses_a_tampered_frontend_before_serving_or_updating() {
        let storage = temporary_directory("integrity-storage");
        let (source, remote_parent, _) = tagged_bare_repository();
        let remote = remote_parent.join("plugin.git");
        let manager = manager_at(storage.clone());
        let plugin_id = "ihub-plugin-pinned-demo";

        manager
            .install_from_remote(GitSource {
                remote: remote.to_string_lossy().into_owned(),
                requested_ref: "v1.2.3".to_owned(),
            })
            .expect("Git snapshot should install with runtime digests");
        fs::write(
            storage.join(plugin_id).join("dist/index.html"),
            "<main>tampered frontend</main>",
        )
        .expect("tamper test fixture");

        let serve_error = manager
            .frontend_path(plugin_id)
            .expect_err("a modified frontend must never receive a loopback lease");
        assert!(serve_error.contains("immutable integrity check"));
        let update_error = manager
            .check_git_update(plugin_id)
            .expect_err("a modified snapshot must not be refreshed over silently");
        assert!(update_error.contains("immutable integrity check"));

        let _ = fs::remove_dir_all(storage);
        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(remote_parent);
    }

    #[test]
    fn default_head_import_keeps_the_legacy_checkout_behavior() {
        let storage = temporary_directory("head-storage");
        let (source, remote_parent, expected_commit) = tagged_bare_repository();
        let remote = remote_parent.join("plugin.git");
        let installed = manager_at(storage.clone())
            .install_from_remote(GitSource {
                remote: remote.to_string_lossy().into_owned(),
                requested_ref: "HEAD".to_owned(),
            })
            .expect("default HEAD should remain importable");
        let lock = installed
            .source_lock
            .expect("HEAD import writes source lock");
        assert_eq!(lock.requested_ref, "HEAD");
        assert_eq!(lock.resolved_commit, expected_commit);

        let _ = fs::remove_dir_all(storage);
        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(remote_parent);
    }

    #[test]
    fn unrelated_source_cannot_replace_an_installed_plugin_with_the_same_id() {
        let storage = temporary_directory("collision-storage");
        let (trusted_source, trusted_remote_parent, _) = tagged_bare_repository();
        let trusted_remote = trusted_remote_parent.join("plugin.git");
        let (untrusted_source, untrusted_remote_parent) =
            bare_repository_with_plugin("ihub-plugin-pinned-demo", "Lookalike plugin");
        let untrusted_remote = untrusted_remote_parent.join("plugin.git");
        let manager = manager_at(storage.clone());

        manager
            .install_from_remote(GitSource {
                remote: trusted_remote.to_string_lossy().into_owned(),
                requested_ref: "HEAD".to_owned(),
            })
            .expect("trusted plugin should install");
        let collision = manager
            .install_from_remote(GitSource {
                remote: untrusted_remote.to_string_lossy().into_owned(),
                requested_ref: "HEAD".to_owned(),
            })
            .expect_err("a lookalike repository must not claim the existing plugin ID");
        assert!(collision.contains("already managed by"));
        assert_eq!(manager.list()[0].name, "Pinned demo");

        let _ = fs::remove_dir_all(storage);
        let _ = fs::remove_dir_all(trusted_source);
        let _ = fs::remove_dir_all(trusted_remote_parent);
        let _ = fs::remove_dir_all(untrusted_source);
        let _ = fs::remove_dir_all(untrusted_remote_parent);
    }

    #[test]
    fn git_refresh_checks_without_writing_then_updates_only_on_explicit_action() {
        let storage = temporary_directory("git-refresh-storage");
        let (source, remote_parent, initial_commit) = tagged_bare_repository();
        let remote = remote_parent.join("plugin.git");
        let manager = manager_at(storage.clone());
        let plugin_id = "ihub-plugin-pinned-demo";

        manager
            .install_from_remote(GitSource {
                remote: remote.to_string_lossy().into_owned(),
                requested_ref: "HEAD".to_owned(),
            })
            .expect("initial Git snapshot should install");
        let lock_path = storage.join(plugin_id).join(SOURCE_LOCK);
        let lock_before_check = fs::read(&lock_path).expect("initial source lock");
        git_success(
            &source,
            &["remote", "add", "origin", &remote.to_string_lossy()],
        );

        let reviewed_commit = commit_and_push_plugin_update(&source, "0.2.0");
        let check = manager
            .check_git_update(plugin_id)
            .expect("check should resolve the saved source/ref");
        assert_eq!(check.current_commit, initial_commit);
        assert_eq!(check.latest_commit, reviewed_commit);
        assert!(check.update_available);
        assert_eq!(check.status, "update-available");
        assert_eq!(
            fs::read(&lock_path).expect("check must not rewrite the source lock"),
            lock_before_check,
            "a read-only check must not mutate provenance"
        );
        assert_eq!(manager.list()[0].version, "0.1.0");

        let moved_commit = commit_and_push_plugin_update(&source, "0.3.0");
        let moved_error = manager
            .update_from_git(plugin_id, &reviewed_commit)
            .expect_err("a ref moved after review must not update the snapshot");
        assert!(moved_error.contains("moved from the reviewed commit"));
        assert_eq!(
            fs::read(&lock_path).expect("moved-ref check must preserve source lock"),
            lock_before_check,
            "a moved ref must require a new review rather than rewriting provenance"
        );

        let updated = manager
            .update_from_git(plugin_id, &moved_commit)
            .expect("explicit update should replace the snapshot");
        assert!(updated.updated);
        assert_eq!(updated.previous_commit, initial_commit);
        assert_eq!(updated.current_commit, moved_commit);
        assert_eq!(updated.plugin.version, "0.3.0");
        let lock = super::read_source_metadata(&storage.join(plugin_id))
            .expect("updated source metadata")
            .lock
            .expect("updated snapshot should retain an immutable lock");
        assert_eq!(lock.resolved_commit, moved_commit);
        assert!(
            !storage.join(plugin_id).join("this-must-not-run").exists(),
            "Git refresh must not execute package scripts"
        );

        let lock_before_noop = fs::read(&lock_path).expect("updated source lock");
        let unchanged = manager
            .update_from_git(plugin_id, &moved_commit)
            .expect("same commit should be a safe no-op");
        assert!(!unchanged.updated);
        assert_eq!(unchanged.current_commit, moved_commit);
        assert_eq!(
            fs::read(&lock_path).expect("no-op source lock"),
            lock_before_noop,
            "a no-op update must preserve lock metadata"
        );

        let _ = fs::remove_dir_all(storage);
        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(remote_parent);
    }

    #[test]
    fn explicit_update_refuses_permission_or_native_declaration_changes_without_replacing_snapshot()
    {
        let storage = temporary_directory("git-security-declaration-update");
        let (source, remote_parent, initial_commit) = tagged_bare_repository();
        let remote = remote_parent.join("plugin.git");
        let manager = manager_at(storage.clone());
        let plugin_id = "ihub-plugin-pinned-demo";

        manager
            .install_from_remote(GitSource {
                remote: remote.to_string_lossy().into_owned(),
                requested_ref: "HEAD".to_owned(),
            })
            .expect("initial Git snapshot should install");
        let lock_path = storage.join(plugin_id).join(SOURCE_LOCK);
        let lock_before = fs::read(&lock_path).expect("initial source lock");
        git_success(
            &source,
            &["remote", "add", "origin", &remote.to_string_lossy()],
        );
        fs::create_dir_all(source.join("bin")).expect("candidate binary directory should exist");
        fs::write(source.join("bin/worker.exe"), b"candidate binary")
            .expect("candidate binary should be written");
        let candidate_commit = commit_and_push_manifest_update(
            &source,
            r#"{
  "id": "ihub-plugin-pinned-demo",
  "name": "Pinned demo",
  "version": "0.2.0",
  "entry": { "frontend": "dist/index.html" },
  "permissions": { "clipboard": { "read": true } },
  "backend": { "binary": "bin/worker.exe" }
}"#,
        );

        let error = manager
            .update_from_git(plugin_id, &candidate_commit)
            .expect_err("routine updates must not widen permissions or add a binary");
        assert!(error.contains("permissions"), "{error}");
        assert!(error.contains("native binary declarations"), "{error}");
        assert!(error.contains("uninstall the managed snapshot"), "{error}");
        assert_eq!(
            fs::read(&lock_path).expect("blocked update must preserve source lock"),
            lock_before,
            "security-declaration rejection must not replace provenance"
        );
        let installed = manager
            .list()
            .pop()
            .expect("old snapshot must remain installed");
        assert_eq!(installed.version, "0.1.0");
        assert_eq!(installed.commit.as_deref(), Some(initial_commit.as_str()));
        assert!(
            !storage.join(plugin_id).join("bin/worker.exe").exists(),
            "candidate native files must never reach the installed snapshot"
        );

        let _ = fs::remove_dir_all(storage);
        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(remote_parent);
    }

    #[test]
    fn routine_updates_compare_every_manifest_permission_declaration() {
        fn manifest_with_permissions(permissions: &str) -> PluginManifest {
            serde_json::from_str(&format!(
                r#"{{
  "id": "ihub-plugin-security-fixture",
  "name": "Security fixture",
  "version": "1.0.0",
  "entry": {{ "frontend": "dist/index.html" }},
  "permissions": {permissions}
}}"#
            ))
            .expect("fixture manifest should deserialize")
        }

        let installed = manifest_with_permissions(
            r#"{
  "network": { "allow": ["https://translate.example.test"] },
  "process": { "spawn": true, "allow": ["tesseract"] }
}"#,
        );

        for candidate_permissions in [
            r#"{
  "network": { "allow": ["https://other.example.test"] },
  "process": { "spawn": true, "allow": ["tesseract"] }
}"#,
            r#"{
  "network": { "allow": ["https://translate.example.test"] },
  "process": { "spawn": true, "allow": ["tesseract"] },
  "globalShortcut": true
}"#,
            r#"{
  "network": { "allow": ["https://translate.example.test"] },
  "process": { "spawn": true, "allow": ["tesseract"] },
  "nativeApi": true
}"#,
            r#"{
  "network": { "allow": ["https://translate.example.test"] },
  "process": { "spawn": true, "allow": ["ffmpeg"] }
}"#,
            r#"{
  "network": { "allow": ["https://translate.example.test"] },
  "process": { "spawn": true, "allow": ["tesseract"] },
  "cursorColor": true
}"#,
            r#"{
  "network": { "allow": ["https://translate.example.test"] },
  "process": { "spawn": true, "allow": ["tesseract"] },
  "microphone": true
}"#,
            r#"{
  "network": { "allow": ["https://translate.example.test"] },
  "process": { "spawn": true, "allow": ["tesseract"] },
  "launcherContext": { "text": true, "files": true }
}"#,
        ] {
            let candidate = manifest_with_permissions(candidate_permissions);
            let error = ensure_update_security_declaration_matches(
                "ihub-plugin-security-fixture",
                &installed,
                &candidate,
            )
            .expect_err("a routine update must not alter any manifest permission declaration");
            assert!(error.contains("permissions"), "{error}");
        }
    }

    #[test]
    fn legacy_source_records_remain_readable_without_a_new_lock() {
        let storage = temporary_directory("legacy-source-storage");
        let plugin_root = storage.join("ihub-plugin-legacy-demo");
        write_plugin(
            &plugin_root,
            "ihub-plugin-legacy-demo",
            "Legacy demo",
            "dist/index.html",
        );
        fs::write(
            plugin_root.join(LEGACY_SOURCE_RECORD),
            r#"{
  "source": "https://github.com/example/ihub-plugin-legacy-demo.git",
  "installedAt": "2026-01-01T00:00:00Z",
  "commit": "0123456789abcdef0123456789abcdef01234567"
}"#,
        )
        .expect("legacy source record should be written");

        let plugin = manager_at(storage.clone())
            .list()
            .into_iter()
            .next()
            .expect("legacy plugin should remain listed");
        assert_eq!(
            plugin.source.as_deref(),
            Some("https://github.com/example/ihub-plugin-legacy-demo.git")
        );
        assert_eq!(
            plugin.commit.as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
        assert!(plugin.source_lock.is_none());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn credential_bearing_legacy_provenance_is_rejected_without_echo_or_ipc_projection() {
        let storage = temporary_directory("unsafe-legacy-provenance");
        let lock_plugin_root = storage.join("ihub-plugin-unsafe-lock");
        write_plugin(
            &lock_plugin_root,
            "ihub-plugin-unsafe-lock",
            "Unsafe lock",
            "dist/index.html",
        );
        let lock_secret = "ghp_saved_source_lock_secret";
        fs::write(
            lock_plugin_root.join(SOURCE_LOCK),
            format!(
                r#"{{
  "source": "https://{lock_secret}@github.com/example/ihub-plugin-unsafe-lock.git",
  "requestedRef": "HEAD",
  "resolvedCommit": "0123456789abcdef0123456789abcdef01234567",
  "installedAt": "2026-01-01T00:00:00Z"
}}"#
            ),
        )
        .expect("unsafe source lock should be written");
        let lock_error = read_source_metadata(&lock_plugin_root)
            .expect_err("credential-bearing source locks must be rejected");
        assert!(!lock_error.contains(lock_secret), "{lock_error}");

        let legacy_plugin_root = storage.join("ihub-plugin-unsafe-legacy");
        write_plugin(
            &legacy_plugin_root,
            "ihub-plugin-unsafe-legacy",
            "Unsafe legacy source",
            "dist/index.html",
        );
        let legacy_secret = "legacy_query_secret";
        fs::write(
            legacy_plugin_root.join(LEGACY_SOURCE_RECORD),
            format!(
                r#"{{
  "source": "https://github.com/example/ihub-plugin-unsafe-legacy.git?access_token={legacy_secret}",
  "installedAt": "2026-01-01T00:00:00Z",
  "commit": "0123456789abcdef0123456789abcdef01234567"
}}"#
            ),
        )
        .expect("unsafe legacy source should be written");
        let legacy_error = read_source_metadata(&legacy_plugin_root)
            .expect_err("credential-bearing legacy records must be rejected");
        assert!(!legacy_error.contains(legacy_secret), "{legacy_error}");

        let projected = manager_at(storage.clone()).list();
        assert_eq!(projected.len(), 2);
        assert!(projected
            .iter()
            .all(|plugin| plugin.source.is_none() && plugin.source_lock.is_none()));

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn lifecycle_state_persists_and_blocks_frontend_execution_until_reenabled() {
        let storage = temporary_directory("lifecycle-storage");
        let plugin_id = "ihub-plugin-lifecycle-demo";
        let plugin_root = storage.join(plugin_id);
        write_plugin(&plugin_root, plugin_id, "Lifecycle demo", "dist/index.html");
        let manager = manager_at(storage.clone());

        assert!(manager.list()[0].enabled, "plugins default to enabled");
        let disabled = manager
            .set_enabled(plugin_id, false)
            .expect("disabling an installed plugin should persist");
        assert!(!disabled.enabled);
        assert!(!disabled.plugin.enabled);
        assert!(storage.join(LIFECYCLE_RECORD).is_file());
        let disabled_error = manager
            .frontend_path(plugin_id)
            .expect_err("a disabled plugin frontend must not be exposed");
        assert!(disabled_error.contains("is disabled"));
        let search_error = manager
            .has_declared_search_provider(plugin_id, "missing")
            .expect_err("disabled plugin provider checks must be rejected");
        assert!(search_error.contains("is disabled"));

        let restarted = manager_at(storage.clone());
        assert!(
            !restarted.list()[0].enabled,
            "the lifecycle state must survive a new manager/process"
        );
        let enabled = restarted
            .set_enabled(plugin_id, true)
            .expect("reenabling should persist");
        assert!(enabled.enabled);
        assert!(enabled.plugin.enabled);
        assert!(restarted.frontend_path(plugin_id).is_ok());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn filesystem_bridge_requires_exact_user_selected_scopes() {
        let storage = temporary_directory("filesystem-permissions");
        let plugin_id = "ihub-plugin-filesystem-demo";
        let plugin_root = storage.join(plugin_id);
        fs::create_dir_all(plugin_root.join("dist")).expect("plugin dist should be created");
        fs::write(plugin_root.join("dist/index.html"), "<main>plugin</main>")
            .expect("frontend should be written");
        fs::write(
            plugin_root.join("plugin.json"),
            r#"{
  "id": "ihub-plugin-filesystem-demo",
  "name": "Filesystem demo",
  "version": "0.1.0",
  "entry": { "frontend": "dist/index.html" },
  "permissions": {
    "filesystem": {
      "read": ["user-selected"],
      "write": ["plugin-data"]
    }
  }
}"#,
        )
        .expect("manifest should be written");
        let manager = manager_at(storage.clone());

        assert!(manager
            .allows_host_method(plugin_id, "filesystem.selectDirectory")
            .expect("read scope should allow a native folder picker"));
        assert!(manager
            .allows_host_method(plugin_id, "filesystem.selectFiles")
            .expect("read scope should allow a native file picker"));
        assert!(manager
            .allows_host_method(plugin_id, "filesystem.batchRename.preview")
            .expect("read scope should allow rename previews"));
        assert!(!manager
            .allows_host_method(plugin_id, "filesystem.batchRename.apply")
            .expect("plugin-data write scope must not allow a user folder mutation"));
        assert!(!manager
            .allows_host_method(plugin_id, "developer.createProject")
            .expect("a project creation requires both user-selected read and write scopes"));
        assert_eq!(
            PluginManager::required_permission_for_host_method("filesystem.batchRename.apply"),
            Some("filesystem.write: [\"user-selected\"]")
        );
        assert_eq!(
            PluginManager::required_permission_for_host_method("developer.createProject"),
            Some("filesystem.read/write: [\"user-selected\"]")
        );
        assert_eq!(
            PluginManager::required_permission_for_host_method("native.runCommand"),
            Some("nativeApi")
        );
        assert!(!manager
            .allows_host_method(plugin_id, "native.runCommand")
            .expect("a filesystem-only plugin must not run its native worker from the iframe"));

        fs::write(
            plugin_root.join("plugin.json"),
            r#"{
  "id": "ihub-plugin-filesystem-demo",
  "name": "Filesystem demo",
  "version": "0.1.0",
  "entry": { "frontend": "dist/index.html" },
  "permissions": {
    "filesystem": {
      "read": ["plugin-data"],
      "write": ["user-selected"]
    }
  }
}"#,
        )
        .expect("write-only user-selected manifest should be written");
        assert!(!manager
            .allows_host_method(plugin_id, "developer.createProject")
            .expect("a project creation requires a user-selected read scope too"));

        fs::write(
            plugin_root.join("plugin.json"),
            r#"{
  "id": "ihub-plugin-filesystem-demo",
  "name": "Filesystem demo",
  "version": "0.1.0",
  "entry": { "frontend": "dist/index.html" },
  "permissions": {
    "filesystem": {
      "read": ["user-selected"],
      "write": ["user-selected"]
    },
    "nativeApi": true
  }
}"#,
        )
        .expect("fully scoped manifest should be written");
        assert!(manager
            .allows_host_method(plugin_id, "developer.createProject")
            .expect("both exact scopes should allow project creation"));
        assert!(manager
            .allows_host_method(plugin_id, "native.runCommand")
            .expect("the explicit nativeApi declaration should allow native commands"));

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn clipboard_history_snapshot_requires_its_own_manifest_permission() {
        let storage = temporary_directory("clipboard-history-permissions");
        let plugin_id = "ihub-plugin-clipboard-history-permissions";
        let plugin_root = storage.join(plugin_id);
        fs::create_dir_all(plugin_root.join("dist")).expect("plugin dist should be created");
        fs::write(plugin_root.join("dist/index.html"), "<main>plugin</main>")
            .expect("frontend should be written");
        fs::write(
            plugin_root.join("plugin.json"),
            r#"{
  "id": "ihub-plugin-clipboard-history-permissions",
  "name": "Clipboard permission demo",
  "version": "0.1.0",
  "entry": { "frontend": "dist/index.html" },
  "permissions": { "clipboard": { "read": true } }
}"#,
        )
        .expect("read-only manifest should be written");
        let manager = manager_at(storage.clone());

        assert!(manager
            .allows_host_method(plugin_id, "clipboard.readText")
            .expect("live clipboard permission should be readable"));
        assert!(!manager
            .allows_host_method(plugin_id, "clipboard.history.snapshot")
            .expect("history must not piggyback on clipboard.read"));
        assert_eq!(
            PluginManager::required_permission_for_host_method("clipboard.history.snapshot"),
            Some("clipboard.history")
        );

        fs::write(
            plugin_root.join("plugin.json"),
            r#"{
  "id": "ihub-plugin-clipboard-history-permissions",
  "name": "Clipboard permission demo",
  "version": "0.1.0",
  "entry": { "frontend": "dist/index.html" },
  "permissions": { "clipboard": { "history": true } }
}"#,
        )
        .expect("history-only manifest should be written");

        assert!(manager
            .allows_host_method(plugin_id, "clipboard.history.snapshot")
            .expect("explicit history permission should allow a snapshot"));
        assert!(!manager
            .allows_host_method(plugin_id, "clipboard.readText")
            .expect("history permission must not grant live clipboard reads"));

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn screen_capture_focus_leases_require_their_own_manifest_permission() {
        let storage = temporary_directory("screen-capture-permissions");
        let plugin_id = "ihub-plugin-screen-capture-permissions";
        let plugin_root = storage.join(plugin_id);
        fs::create_dir_all(plugin_root.join("dist")).expect("plugin dist should be created");
        fs::write(plugin_root.join("dist/index.html"), "<main>plugin</main>")
            .expect("frontend should be written");
        fs::write(
            plugin_root.join("plugin.json"),
            r#"{
  "id": "ihub-plugin-screen-capture-permissions",
  "name": "Screen capture permission demo",
  "version": "0.1.0",
  "entry": { "frontend": "dist/index.html" },
  "permissions": { "clipboard": { "read": true } }
}"#,
        )
        .expect("manifest should be written");
        let manager = manager_at(storage.clone());

        assert!(!manager
            .allows_host_method(plugin_id, "screenCapture.acquireFocusLease")
            .expect("unrelated clipboard access must not grant a capture lease"));
        assert!(!manager
            .allows_host_method(plugin_id, "screenCapture.releaseFocusLease")
            .expect("release must be gated by the same explicit permission"));
        assert!(!manager
            .allows_host_method(plugin_id, "compatibility.utools.screen.capture")
            .expect("an ordinary iHub plugin must declare screen capture"));
        assert_eq!(
            PluginManager::required_permission_for_host_method("screenCapture.acquireFocusLease"),
            Some("screenCapture")
        );
        assert_eq!(
            PluginManager::required_permission_for_host_method("screenCapture.releaseFocusLease"),
            Some("screenCapture")
        );
        assert_eq!(
            PluginManager::required_permission_for_host_method(
                "compatibility.utools.screen.capture"
            ),
            Some("screenCapture")
        );

        fs::write(
            plugin_root.join("plugin.json"),
            r#"{
  "id": "ihub-plugin-screen-capture-permissions",
  "name": "Screen capture permission demo",
  "version": "0.1.0",
  "entry": { "frontend": "dist/index.html" },
  "permissions": { "screenCapture": true }
}"#,
        )
        .expect("screen-capture manifest should be written");

        assert!(manager
            .allows_host_method(plugin_id, "screenCapture.acquireFocusLease")
            .expect("an explicit screenCapture declaration should allow acquire"));
        assert!(manager
            .allows_host_method(plugin_id, "screenCapture.releaseFocusLease")
            .expect("an explicit screenCapture declaration should allow release"));
        assert!(manager
            .allows_host_method(plugin_id, "compatibility.utools.screen.capture")
            .expect("an explicit screenCapture declaration should allow a confirmed crop"));
        assert!(!manager
            .allows_host_method(plugin_id, "clipboard.readText")
            .expect("screen capture permission must not grant clipboard reads"));

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn cursor_color_requires_its_own_manifest_permission() {
        let storage = temporary_directory("cursor-color-permissions");
        let plugin_id = "ihub-plugin-cursor-color-permissions";
        let plugin_root = storage.join(plugin_id);
        fs::create_dir_all(plugin_root.join("dist")).expect("plugin dist should be created");
        fs::write(plugin_root.join("dist/index.html"), "<main>plugin</main>")
            .expect("frontend should be written");
        fs::write(
            plugin_root.join("plugin.json"),
            r#"{
  "id": "ihub-plugin-cursor-color-permissions",
  "name": "Cursor color permission demo",
  "version": "0.1.0",
  "entry": { "frontend": "dist/index.html" },
  "permissions": { "screenCapture": true, "nativeApi": true }
}"#,
        )
        .expect("manifest should be written");
        let manager = manager_at(storage.clone());

        assert!(!manager
            .allows_host_method(plugin_id, "cursorColor.sampleOnce")
            .expect("screen capture and native API must not grant a cursor pixel"));
        assert_eq!(
            PluginManager::required_permission_for_host_method("cursorColor.sampleOnce"),
            Some("cursorColor")
        );

        fs::write(
            plugin_root.join("plugin.json"),
            r#"{
  "id": "ihub-plugin-cursor-color-permissions",
  "name": "Cursor color permission demo",
  "version": "0.1.0",
  "entry": { "frontend": "dist/index.html" },
  "permissions": { "cursorColor": true }
}"#,
        )
        .expect("cursor-color manifest should be written");

        assert!(manager
            .allows_host_method(plugin_id, "cursorColor.sampleOnce")
            .expect("an explicit cursorColor declaration should allow the narrow bridge"));
        assert!(!manager
            .allows_host_method(plugin_id, "screenCapture.acquireFocusLease")
            .expect("cursor color must not grant a screen capture lease"));
        assert!(!manager
            .allows_host_method(plugin_id, "native.runCommand")
            .expect("cursor color must not grant a general native API"));

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn launcher_layout_requires_its_own_manifest_permission() {
        let storage = temporary_directory("window-management-permissions");
        let plugin_id = "ihub-plugin-window-management-permissions";
        let plugin_root = storage.join(plugin_id);
        fs::create_dir_all(plugin_root.join("dist")).expect("plugin dist should be created");
        fs::write(plugin_root.join("dist/index.html"), "<main>plugin</main>")
            .expect("frontend should be written");
        fs::write(
            plugin_root.join("plugin.json"),
            r#"{
  "id": "ihub-plugin-window-management-permissions",
  "name": "Window permission demo",
  "version": "0.1.0",
  "entry": { "frontend": "dist/index.html" },
  "permissions": { "nativeApi": true }
}"#,
        )
        .expect("manifest should be written");
        let manager = manager_at(storage.clone());

        assert!(!manager
            .allows_host_method(plugin_id, "window.manageLauncher")
            .expect("native API must not grant launcher layout access"));
        assert_eq!(
            PluginManager::required_permission_for_host_method("window.manageLauncher"),
            Some("windowManagement")
        );

        fs::write(
            plugin_root.join("plugin.json"),
            r#"{
  "id": "ihub-plugin-window-management-permissions",
  "name": "Window permission demo",
  "version": "0.1.0",
  "entry": { "frontend": "dist/index.html" },
  "permissions": { "windowManagement": true }
}"#,
        )
        .expect("window-management manifest should be written");

        assert!(manager
            .allows_host_method(plugin_id, "window.manageLauncher")
            .expect("an explicit windowManagement declaration should allow the bounded bridge"));
        assert!(!manager
            .allows_host_method(plugin_id, "native.runCommand")
            .expect("window management must not grant native worker access"));

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn launcher_context_requires_exact_granular_permissions_and_a_frontend_command() {
        let storage = temporary_directory("launcher-context-permissions");
        let plugin_id = "ihub-plugin-launcher-context-permissions";
        let plugin_root = storage.join(plugin_id);
        fs::create_dir_all(plugin_root.join("dist")).expect("plugin dist should be created");
        fs::write(plugin_root.join("dist/index.html"), "<main>plugin</main>")
            .expect("frontend should be written");
        fs::write(
            plugin_root.join("plugin.json"),
            r#"{
  "id": "ihub-plugin-launcher-context-permissions",
  "name": "Launcher context permission demo",
  "version": "0.1.0",
  "entry": { "frontend": "dist/index.html" },
  "contributes": {
    "commands": [{ "id": "open-context", "title": "Open context", "execution": "frontend" }]
  },
  "permissions": { "clipboard": { "read": true } }
}"#,
        )
        .expect("manifest should be written");
        let manager = manager_at(storage.clone());

        assert!(!manager
            .allows_host_method(plugin_id, "launcherContext.consume")
            .expect("clipboard access must not grant launcher context"));
        assert!(!manager
            .allows_launcher_context(plugin_id, true, false, false)
            .expect("text must be explicitly declared"));
        assert_eq!(
            PluginManager::required_permission_for_host_method("launcherContext.consume"),
            Some("launcherContext")
        );

        fs::write(
            plugin_root.join("plugin.json"),
            r#"{
  "id": "ihub-plugin-launcher-context-permissions",
  "name": "Launcher context permission demo",
  "version": "0.1.0",
  "entry": { "frontend": "dist/index.html" },
  "contributes": {
    "commands": [{ "id": "open-context", "title": "Open context", "execution": "frontend" }]
  },
  "permissions": { "launcherContext": { "text": true } }
}"#,
        )
        .expect("text-only manifest should be written");

        assert!(manager
            .allows_host_method(plugin_id, "launcherContext.consume")
            .expect("one explicit category enables the narrow consume method"));
        assert!(manager
            .allows_launcher_context(plugin_id, true, false, false)
            .expect("text declaration should allow text only"));
        assert!(!manager
            .allows_launcher_context(plugin_id, false, true, false)
            .expect("text must not imply selected-file metadata"));
        assert!(!manager
            .allows_launcher_context(plugin_id, false, false, true)
            .expect("text must not imply an image handle"));
        manager
            .ensure_frontend_command(plugin_id, "open-context")
            .expect("the declared frontend command can receive a transfer");
        assert!(manager
            .ensure_frontend_command(plugin_id, "missing-command")
            .expect_err("an undeclared command must never receive a transfer")
            .contains("does not expose command"));
        let listed_context = manager
            .list()
            .into_iter()
            .find(|plugin| plugin.id == plugin_id)
            .and_then(|plugin| plugin.launcher_context)
            .expect("the trusted parent needs a read-only permission projection for candidate filtering");
        assert!(listed_context.text);
        assert!(!listed_context.files);
        assert!(!listed_context.image);

        fs::write(
            plugin_root.join("plugin.json"),
            r#"{
  "id": "ihub-plugin-launcher-context-permissions",
  "name": "Launcher context permission demo",
  "version": "0.1.0",
  "entry": { "frontend": "dist/index.html" },
  "permissions": { "launcherContext": { "text": true, "typo": true } }
}"#,
        )
        .expect("malformed manifest should be written for rejection");
        assert!(manager
            .allows_launcher_context(plugin_id, true, false, false)
            .expect_err("unknown launcherContext fields must not be silently accepted")
            .contains("unknown field"));

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn uninstall_removes_only_a_managed_git_snapshot_and_clears_its_lifecycle_state() {
        let storage = temporary_directory("uninstall-storage");
        let (source, remote_parent, _) = tagged_bare_repository();
        let remote = remote_parent.join("plugin.git");
        let manager = manager_at(storage.clone());
        let plugin_id = "ihub-plugin-pinned-demo";

        manager
            .install_from_remote(GitSource {
                remote: remote.to_string_lossy().into_owned(),
                requested_ref: "HEAD".to_owned(),
            })
            .expect("managed Git snapshot should install");
        manager
            .set_enabled(plugin_id, false)
            .expect("lifecycle state should be set before removal");

        let removed = manager
            .uninstall_managed_snapshot(plugin_id)
            .expect("managed Git snapshot should uninstall");
        assert_eq!(removed.plugin_id, plugin_id);
        assert_eq!(removed.plugin_name, "Pinned demo");
        assert_eq!(removed.source, remote.to_string_lossy());
        assert!(!storage.join(plugin_id).exists());
        assert!(manager.list().is_empty());
        let lifecycle = fs::read_to_string(storage.join(LIFECYCLE_RECORD))
            .expect("lifecycle state remains as an empty host record");
        assert!(
            !lifecycle.contains(plugin_id),
            "uninstall must not leave a disabled state that surprises a later fresh import"
        );
        assert!(
            source.join("plugin.json").is_file(),
            "uninstalling a managed snapshot must not affect the source repository"
        );

        let _ = fs::remove_dir_all(storage);
        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_dir_all(remote_parent);
    }

    #[test]
    fn uninstall_refuses_local_development_links_and_unmanaged_directories() {
        let storage = temporary_directory("uninstall-safety-storage");
        let source = temporary_directory("uninstall-safety-source");
        let plugin_id = "ihub-plugin-uninstall-safety";
        write_plugin(
            &storage.join(plugin_id),
            plugin_id,
            "Unmanaged snapshot",
            "dist/index.html",
        );
        write_plugin(&source, plugin_id, "Local development", "dist/index.html");
        let manager = manager_at(storage.clone());

        manager
            .link_from_local(&source.to_string_lossy())
            .expect("local development project should link");
        let linked_error = manager
            .uninstall_managed_snapshot(plugin_id)
            .expect_err("a local link must never become an uninstall target");
        assert!(linked_error.contains("never deletes developer source"));
        assert!(source.join("plugin.json").is_file());
        assert!(storage.join(plugin_id).is_dir());

        manager
            .unlink_from_local(plugin_id)
            .expect("unlink should only remove host metadata");
        let unmanaged_error = manager
            .uninstall_managed_snapshot(plugin_id)
            .expect_err("unmanaged directories should not be automatically deleted");
        assert!(unmanaged_error.contains("managed Git provenance"));
        assert!(storage.join(plugin_id).is_dir());

        let _ = fs::remove_dir_all(storage);
        let _ = fs::remove_dir_all(source);
    }

    #[test]
    fn manifest_search_providers_are_exposed_and_must_match_runtime_registration() {
        let storage = temporary_directory("search-provider-storage");
        let plugin_id = "ihub-plugin-search-demo";
        let plugin_root = storage.join(plugin_id);
        fs::create_dir_all(plugin_root.join("dist")).expect("plugin dist should be created");
        fs::write(plugin_root.join("dist/index.html"), "<main>plugin</main>")
            .expect("frontend should be written");
        fs::write(
            plugin_root.join("plugin.json"),
            r#"{
  "id": "ihub-plugin-search-demo",
  "name": "Search demo",
  "version": "0.1.0",
  "entry": { "frontend": "dist/index.html" },
  "contributes": {
    "searchProviders": [
      { "id": "demo-search", "title": "Demo search", "trigger": "demo ", "priority": 12 }
    ]
  }
}"#,
        )
        .expect("manifest should be written");

        let manager = manager_at(storage.clone());
        let plugin = manager
            .list()
            .into_iter()
            .next()
            .expect("plugin should be listed");
        assert_eq!(plugin.search_providers.len(), 1);
        assert_eq!(plugin.search_providers[0].id, "demo-search");
        assert_eq!(plugin.search_providers[0].trigger.as_deref(), Some("demo "));
        assert!(manager
            .has_declared_search_provider(plugin_id, "demo-search")
            .expect("declared provider lookup"));
        assert!(!manager
            .has_declared_search_provider(plugin_id, "not-declared")
            .expect("unknown provider lookup"));

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn manifest_declared_secret_settings_are_readable_only_as_secret_keys() {
        let storage = temporary_directory("secret-setting-storage");
        let plugin_id = "ihub-plugin-secret-settings";
        let plugin_root = storage.join(plugin_id);
        fs::create_dir_all(plugin_root.join("dist")).expect("plugin dist should be created");
        fs::write(plugin_root.join("dist/index.html"), "<main>plugin</main>")
            .expect("frontend should be written");
        fs::write(
            plugin_root.join("plugin.json"),
            r#"{
  "id": "ihub-plugin-secret-settings",
  "name": "Secret settings demo",
  "version": "0.1.0",
  "entry": { "frontend": "dist/index.html" },
  "contributes": {
    "settings": [
      { "key": "provider", "title": "Provider", "type": "select" },
      { "key": "apiKey", "title": "API key", "type": "string", "secret": true }
    ]
  }
}"#,
        )
        .expect("manifest should be written");

        let manager = manager_at(storage.clone());
        assert!(manager
            .is_secret_setting(plugin_id, "apiKey")
            .expect("secret setting lookup"));
        assert!(!manager
            .is_secret_setting(plugin_id, "provider")
            .expect("ordinary setting lookup"));
        assert_eq!(
            manager.declared_secret_setting_keys(),
            vec![(plugin_id.to_owned(), "apiKey".to_owned())]
        );

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn local_link_reads_the_project_in_place_after_rebuilds() {
        let storage = temporary_directory("storage");
        let source = temporary_directory("source");
        write_plugin(
            &source,
            "ihub-plugin-local-demo",
            "First build",
            "dist/index.html",
        );
        let manager = manager_at(storage.clone());

        let linked = manager
            .link_from_local(&source.to_string_lossy())
            .expect("local project should link");
        let canonical_source = source.canonicalize().expect("canonical source");
        let expected_source = format!("local:{}", canonical_source.display());
        assert!(linked.is_development_link);
        assert_eq!(linked.local_link_status.as_deref(), Some("active"));
        assert!(linked.local_link_error.is_none());
        assert!(!linked.uses_managed_snapshot_fallback);
        assert_eq!(linked.local_path.as_deref(), canonical_source.to_str());
        assert_eq!(linked.source.as_deref(), Some(expected_source.as_str()));
        assert!(!storage.join("ihub-plugin-local-demo").exists());
        assert!(storage.join(LOCAL_LINKS_RECORD).is_file());

        write_plugin(
            &source,
            "ihub-plugin-local-demo",
            "Second build",
            "dist/index.html",
        );
        let listed = manager.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Second build");
        assert!(listed[0].is_development_link);
        assert_eq!(
            manager
                .frontend_path("ihub-plugin-local-demo")
                .expect("frontend path"),
            canonical_source.join("dist/index.html")
        );

        let _ = fs::remove_dir_all(storage);
        let _ = fs::remove_dir_all(source);
    }

    #[test]
    fn linked_utools_bundle_reports_the_development_runtime() {
        let storage = temporary_directory("utools-development-storage");
        let source = temporary_directory("utools-development-source");
        fs::create_dir_all(source.join("dist")).expect("uTools dist should be created");
        fs::write(
            source.join("dist/index.html"),
            "<main>uTools development</main>",
        )
        .expect("uTools frontend should be written");
        write_test_png(&source.join("logo.png"), [10, 132, 255, 255]);
        fs::write(source.join("preload.js"), "window.localDemo = true;")
            .expect("uTools preload fixture should be written");
        fs::write(
            source.join("plugin.json"),
            r#"{
  "name": "Local uTools development demo",
  "version": "0.1.0",
  "logo": "logo.png",
  "preload": "preload.js",
  "main": "dist/index.html",
  "features": [{ "code": "local-demo", "explain": "Local demo", "cmds": ["Local demo"] }]
}"#,
        )
        .expect("uTools development manifest should be written");
        let manager = manager_at(storage.clone());
        let linked = manager
            .link_from_local(&source.to_string_lossy())
            .expect("uTools development package should link");
        let bundle = manager
            .frontend_asset_bundle(&linked.id)
            .expect("linked uTools frontend bundle");
        assert!(
            bundle
                .utools_compat
                .expect("uTools runtime configuration")
                .is_development
        );

        let _ = fs::remove_dir_all(storage);
        let _ = fs::remove_dir_all(source);
    }

    #[test]
    fn stale_local_link_uses_a_verified_managed_snapshot_and_remains_unlinkable() {
        let storage = temporary_directory("stale-link-fallback-storage");
        let local_source = temporary_directory("stale-link-fallback-source");
        let (git_source, remote_parent, _) = tagged_bare_repository();
        let remote = remote_parent.join("plugin.git");
        let plugin_id = "ihub-plugin-pinned-demo";
        write_plugin(
            &local_source,
            plugin_id,
            "Local development build",
            "dist/index.html",
        );
        let canonical_local_source = local_source
            .canonicalize()
            .expect("local development source should canonicalize");
        let manager = manager_at(storage.clone());

        manager
            .install_from_remote(GitSource {
                remote: remote.to_string_lossy().into_owned(),
                requested_ref: "HEAD".to_owned(),
            })
            .expect("managed Git snapshot should install");
        manager
            .link_from_local(&local_source.to_string_lossy())
            .expect("local development project should shadow the snapshot");
        fs::remove_dir_all(&local_source).expect("linked checkout should be removed");

        let listed = manager.list();
        assert_eq!(listed.len(), 1);
        let stale = &listed[0];
        assert_eq!(stale.id, plugin_id);
        assert_eq!(stale.name, "Pinned demo");
        assert!(stale.is_development_link);
        assert_eq!(stale.local_link_status.as_deref(), Some("stale"));
        assert!(stale.local_link_error.is_some());
        assert!(stale.uses_managed_snapshot_fallback);
        assert!(stale.source_lock.is_some());
        assert_eq!(
            stale.local_path.as_deref(),
            Some(canonical_local_source.to_string_lossy().as_ref())
        );
        let managed_root = storage
            .join(plugin_id)
            .canonicalize()
            .expect("managed snapshot should canonicalize");
        assert_eq!(
            manager
                .frontend_path(plugin_id)
                .expect("runtime should safely fall back to the managed snapshot"),
            managed_root.join("dist/index.html")
        );

        manager
            .unlink_from_local(plugin_id)
            .expect("stale development link should remain unlinkable");
        let restored = manager
            .list()
            .into_iter()
            .find(|plugin| plugin.id == plugin_id)
            .expect("managed snapshot should remain installed");
        assert!(!restored.is_development_link);
        assert!(restored.local_link_status.is_none());
        assert!(!restored.uses_managed_snapshot_fallback);
        assert!(restored.source_lock.is_some());

        let _ = fs::remove_dir_all(storage);
        let _ = fs::remove_dir_all(git_source);
        let _ = fs::remove_dir_all(remote_parent);
    }

    #[test]
    fn stale_local_link_without_snapshot_stays_visible_until_unlinked_then_allows_install() {
        let storage = temporary_directory("stale-link-empty-storage");
        let local_source = temporary_directory("stale-link-empty-source");
        let (git_source, remote_parent, _) = tagged_bare_repository();
        let remote = remote_parent.join("plugin.git");
        let plugin_id = "ihub-plugin-pinned-demo";
        write_plugin(
            &local_source,
            plugin_id,
            "Cached local development name",
            "dist/index.html",
        );
        let manager = manager_at(storage.clone());

        manager
            .link_from_local(&local_source.to_string_lossy())
            .expect("local development project should link");
        fs::remove_dir_all(&local_source).expect("linked checkout should be removed");

        let listed = manager.list();
        assert_eq!(listed.len(), 1);
        let stale = &listed[0];
        assert_eq!(stale.id, plugin_id);
        assert_eq!(stale.name, "Cached local development name");
        assert_eq!(stale.version, "0.1.0");
        assert!(stale.is_development_link);
        assert_eq!(stale.local_link_status.as_deref(), Some("stale"));
        assert!(stale.local_link_error.is_some());
        assert!(!stale.uses_managed_snapshot_fallback);
        assert!(stale.frontend_entry.is_none());
        assert!(stale.source_lock.is_none());

        let runtime_error = manager
            .frontend_path(plugin_id)
            .expect_err("a stale link without fallback must not execute");
        assert!(runtime_error.contains("is stale"), "{runtime_error}");
        assert!(runtime_error.contains("Unlink"), "{runtime_error}");
        let blocked_install = manager
            .install_from_remote(GitSource {
                remote: remote.to_string_lossy().into_owned(),
                requested_ref: "HEAD".to_owned(),
            })
            .expect_err("the stale record must retain ownership until explicitly unlinked");
        assert!(
            blocked_install.contains("currently linked"),
            "{blocked_install}"
        );

        manager
            .unlink_from_local(plugin_id)
            .expect("stale development link should remain unlinkable");
        assert!(manager.list().is_empty());
        let installed = manager
            .install_from_remote(GitSource {
                remote: remote.to_string_lossy().into_owned(),
                requested_ref: "HEAD".to_owned(),
            })
            .expect("install should become available immediately after unlink");
        assert_eq!(installed.id, plugin_id);
        assert!(!installed.is_development_link);
        assert!(installed.source_lock.is_some());

        let _ = fs::remove_dir_all(storage);
        let _ = fs::remove_dir_all(git_source);
        let _ = fs::remove_dir_all(remote_parent);
    }

    #[test]
    fn frontend_bundle_rejects_an_entry_beside_the_package_manifest() {
        let storage = temporary_directory("root-frontend-storage");
        let plugin_id = "ihub-plugin-root-frontend";
        let plugin_root = storage.join(plugin_id);
        write_plugin(&plugin_root, plugin_id, "Root frontend", "index.html");
        fs::write(plugin_root.join("index.html"), "<main>root frontend</main>")
            .expect("root frontend fixture should be written");
        fs::write(plugin_root.join(".env"), "MUST-NOT-BE-SERVED=true")
            .expect("private package fixture should be written");

        let error = manager_at(storage.clone())
            .frontend_asset_bundle(plugin_id)
            .expect_err("the package root must never become an iframe asset root");
        assert!(error.contains("dedicated child build directory"));

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn frontend_bundle_projects_only_a_nonempty_network_declaration() {
        let storage = temporary_directory("frontend-network-policy-storage");
        let locked_id = "ihub-plugin-network-locked";
        let networked_id = "ihub-plugin-network-open";
        let locked_root = storage.join(locked_id);
        let networked_root = storage.join(networked_id);
        write_plugin(&locked_root, locked_id, "Network locked", "dist/index.html");
        write_plugin(
            &networked_root,
            networked_id,
            "Network declared",
            "dist/index.html",
        );
        fs::write(
            networked_root.join("plugin.json"),
            format!(
                r#"{{
  "id": "{networked_id}",
  "name": "Network declared",
  "version": "0.1.0",
  "entry": {{ "frontend": "dist/index.html" }},
  "permissions": {{
    "network": {{
      "allow": ["user-configured HTTPS endpoint"]
    }}
  }}
}}"#
            ),
        )
        .expect("networked manifest should be written");

        let manager = manager_at(storage.clone());
        assert!(
            !manager
                .frontend_asset_bundle(locked_id)
                .expect("locked frontend bundle")
                .allows_remote_network
        );
        assert!(
            manager
                .frontend_asset_bundle(networked_id)
                .expect("networked frontend bundle")
                .allows_remote_network
        );

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn nested_permissions_reject_unknown_fields_and_unsafe_string_lists() {
        for permissions in [
            r#""network": { "allow": [], "typo": true }"#,
            r#""filesystem": { "read": [], "typo": [] }"#,
            r#""clipboard": { "read": true, "typo": true }"#,
            r#""process": { "spawn": true, "typo": true }"#,
            r#""shell": { "openPath": true, "typo": true }"#,
        ] {
            let document = format!(
                r#"{{
  "id": "ihub-plugin-permission-unknown",
  "name": "Permission unknown",
  "version": "0.1.0",
  "entry": {{ "frontend": "dist/index.html" }},
  "permissions": {{ {permissions} }}
}}"#
            );
            let error = serde_json::from_str::<PluginManifest>(&document)
                .expect_err("unknown nested permission fields must fail closed");
            assert!(error.to_string().contains("unknown field"), "{error}");
        }

        let over_capacity = (0..=MAX_PERMISSION_LIST_ITEMS)
            .map(|index| format!(r#""target-{index}""#))
            .collect::<Vec<_>>()
            .join(",");
        let too_long = "x".repeat(MAX_PERMISSION_VALUE_CHARS + 1);
        for allow in [
            r#""""#.to_owned(),
            r#"" https://api.example.test""#.to_owned(),
            r#""\uFEFFhttps://api.example.test""#.to_owned(),
            r#""https://api.example.test\u0001""#.to_owned(),
            r#""https://api.example.test","https://api.example.test""#.to_owned(),
            over_capacity,
            format!(r#""{too_long}""#),
        ] {
            let document = format!(
                r#"{{
  "id": "ihub-plugin-network-invalid",
  "name": "Network invalid",
  "version": "0.1.0",
  "entry": {{ "frontend": "dist/index.html" }},
  "permissions": {{ "network": {{ "allow": [{allow}] }} }}
}}"#
            );
            let manifest = serde_json::from_str::<PluginManifest>(&document)
                .expect("unsafe list entries remain syntactically valid JSON");
            let error = validate_manifest(&manifest)
                .expect_err("unsafe network declarations must not open the coarse CSP gate");
            assert!(error.contains("permissions.network.allow"), "{error}");
        }
    }

    #[test]
    fn frontend_bundle_projects_only_an_explicit_screen_capture_declaration() {
        let storage = temporary_directory("frontend-display-capture-policy-storage");
        let undeclared_id = "ihub-plugin-display-capture-locked";
        let declared_id = "ihub-plugin-display-capture-open";
        let undeclared_root = storage.join(undeclared_id);
        let declared_root = storage.join(declared_id);
        write_plugin(
            &undeclared_root,
            undeclared_id,
            "Display capture locked",
            "dist/index.html",
        );
        write_plugin(
            &declared_root,
            declared_id,
            "Display capture declared",
            "dist/index.html",
        );
        fs::write(
            declared_root.join("plugin.json"),
            format!(
                r#"{{
  "id": "{declared_id}",
  "name": "Display capture declared",
  "version": "0.1.0",
  "entry": {{ "frontend": "dist/index.html" }},
  "permissions": {{ "screenCapture": true }}
}}"#
            ),
        )
        .expect("screen-capture manifest should be written");

        let manager = manager_at(storage.clone());
        assert!(
            !manager
                .frontend_asset_bundle(undeclared_id)
                .expect("undeclared frontend bundle")
                .allows_display_capture
        );
        assert!(
            manager
                .frontend_asset_bundle(declared_id)
                .expect("declared frontend bundle")
                .allows_display_capture
        );

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn frontend_bundle_projects_only_an_explicit_microphone_declaration() {
        let storage = temporary_directory("frontend-microphone-policy-storage");
        let undeclared_id = "ihub-plugin-microphone-locked";
        let declared_id = "ihub-plugin-microphone-open";
        let undeclared_root = storage.join(undeclared_id);
        let declared_root = storage.join(declared_id);
        write_plugin(
            &undeclared_root,
            undeclared_id,
            "Microphone locked",
            "dist/index.html",
        );
        write_plugin(
            &declared_root,
            declared_id,
            "Microphone declared",
            "dist/index.html",
        );
        fs::write(
            declared_root.join("plugin.json"),
            format!(
                r#"{{
  "id": "{declared_id}",
  "name": "Microphone declared",
  "version": "0.1.0",
  "entry": {{ "frontend": "dist/index.html" }},
  "permissions": {{ "microphone": true }}
}}"#
            ),
        )
        .expect("microphone manifest should be written");

        let manager = manager_at(storage.clone());
        assert!(
            !manager
                .frontend_asset_bundle(undeclared_id)
                .expect("undeclared frontend bundle")
                .allows_microphone
        );
        assert!(
            manager
                .frontend_asset_bundle(declared_id)
                .expect("declared frontend bundle")
                .allows_microphone
        );

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn microphone_manifest_permission_is_boolean_and_rejects_unknown_typos() {
        let valid = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-microphone-valid",
  "name": "Microphone valid",
  "version": "1.0.0",
  "entry": { "frontend": "dist/index.html" },
  "permissions": { "microphone": true }
}"#,
        )
        .expect("an explicit boolean microphone permission should deserialize");
        assert!(valid.permissions.microphone);

        let non_boolean = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-microphone-invalid",
  "name": "Microphone invalid",
  "version": "1.0.0",
  "entry": { "frontend": "dist/index.html" },
  "permissions": { "microphone": "yes" }
}"#,
        )
        .expect_err("a non-boolean microphone declaration must be rejected");
        assert!(non_boolean.to_string().contains("boolean"));

        let typo = serde_json::from_str::<PluginManifest>(
            r#"{
  "id": "ihub-plugin-microphone-typo",
  "name": "Microphone typo",
  "version": "1.0.0",
  "entry": { "frontend": "dist/index.html" },
  "permissions": { "microhpone": true }
}"#,
        )
        .expect_err("an unknown permission typo must be rejected");
        assert!(typo.to_string().contains("unknown field"));
    }

    #[test]
    fn official_json_tools_package_is_linkable_with_its_built_frontend() {
        let storage = temporary_directory("official-json-tools-storage");
        let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../plugins/official/ihub-plugin-json-tools")
            .canonicalize()
            .expect("official JSON Tools package should exist in the workspace");
        let manager = manager_at(storage.clone());

        let linked = manager
            .link_from_local(&package.to_string_lossy())
            .expect("official JSON Tools package should link as a local project");
        assert_eq!(linked.id, "ihub-plugin-json-tools");
        assert!(linked.is_development_link);
        assert_eq!(linked.commands.len(), 1);
        assert_eq!(linked.commands[0].id, "open-json-tools");
        assert_eq!(
            manager
                .frontend_path("ihub-plugin-json-tools")
                .expect("built frontend path"),
            package.join("dist/index.html")
        );
        assert!(package.join("dist/main.js").is_file());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn official_ocr_package_is_linkable_with_its_built_frontend_and_native_worker() {
        let storage = temporary_directory("official-ocr-storage");
        let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../plugins/official/ihub-plugin-ocr")
            .canonicalize()
            .expect("official OCR package should exist in the workspace");
        let manager = manager_at(storage.clone());

        let linked = manager
            .link_from_local(&package.to_string_lossy())
            .expect("official OCR package should link as a local project");
        assert_eq!(linked.id, "ihub-plugin-ocr");
        assert!(linked.is_development_link);
        assert!(linked.has_native_worker);
        assert_eq!(linked.commands.len(), 3);
        assert_eq!(linked.commands[0].id, "open");
        assert_eq!(linked.commands[0].execution, "frontend");
        assert_eq!(linked.commands[1].id, "recognize-launcher-image");
        assert_eq!(linked.commands[1].execution, "frontend");
        assert_eq!(linked.commands[2].id, "recognize-image");
        assert_eq!(linked.commands[2].execution, "native");
        assert!(linked
            .launcher_context
            .as_ref()
            .is_some_and(|permissions| permissions.image));
        assert_eq!(
            manager
                .frontend_path("ihub-plugin-ocr")
                .expect("built frontend path"),
            package.join("dist/index.html")
        );
        assert!(package.join("dist/main.js").is_file());
        assert!(package.join("bin/windows-x86_64/ocr-worker.exe").is_file());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn official_translate_package_is_linkable_with_its_text_handoff_command() {
        let storage = temporary_directory("official-translate-storage");
        let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../plugins/official/ihub-plugin-translate")
            .canonicalize()
            .expect("official Translate package should exist in the workspace");
        let manager = manager_at(storage.clone());

        let linked = manager
            .link_from_local(&package.to_string_lossy())
            .expect("official Translate package should link as a local project");
        assert_eq!(linked.id, "ihub-plugin-translate");
        assert!(linked.is_development_link);
        assert_eq!(linked.commands.len(), 2);
        assert_eq!(linked.commands[0].id, "open-translate");
        assert_eq!(linked.commands[0].execution, "frontend");
        assert_eq!(linked.commands[1].id, "translate-launcher-text");
        assert_eq!(linked.commands[1].execution, "frontend");
        assert!(linked
            .launcher_context
            .as_ref()
            .is_some_and(|permissions| permissions.text));
        assert_eq!(
            manager
                .frontend_path("ihub-plugin-translate")
                .expect("built frontend path"),
            package.join("dist/index.html")
        );
        assert!(package.join("dist/main.js").is_file());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn official_colorpick_package_is_linkable_with_its_built_frontend() {
        let storage = temporary_directory("official-colorpick-storage");
        let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../plugins/official/ihub-plugin-colorpick")
            .canonicalize()
            .expect("official Color Picker package should exist in the workspace");
        let manager = manager_at(storage.clone());

        let linked = manager
            .link_from_local(&package.to_string_lossy())
            .expect("official Color Picker package should link as a local project");
        assert_eq!(linked.id, "ihub-plugin-colorpick");
        assert!(linked.is_development_link);
        assert_eq!(linked.commands.len(), 1);
        assert_eq!(linked.commands[0].id, "pick-color");
        assert!(!linked.has_native_worker);
        assert_eq!(
            manager
                .frontend_path("ihub-plugin-colorpick")
                .expect("built frontend path"),
            package.join("dist/index.html")
        );
        assert!(package.join("dist/main.js").is_file());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn official_qrcode_package_is_linkable_with_its_built_frontend() {
        let storage = temporary_directory("official-qrcode-storage");
        let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../plugins/official/ihub-plugin-qrcode")
            .canonicalize()
            .expect("official QR Code package should exist in the workspace");
        let manager = manager_at(storage.clone());

        let linked = manager
            .link_from_local(&package.to_string_lossy())
            .expect("official QR Code package should link as a local project");
        assert_eq!(linked.id, "ihub-plugin-qrcode");
        assert!(linked.is_development_link);
        assert_eq!(linked.commands.len(), 1);
        assert_eq!(linked.commands[0].id, "generate-qrcode");
        assert_eq!(
            manager
                .frontend_path("ihub-plugin-qrcode")
                .expect("built frontend path"),
            package.join("dist/index.html")
        );
        assert!(package.join("dist/main.js").is_file());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn official_base_converter_package_is_linkable_with_its_built_frontend() {
        let storage = temporary_directory("official-base-converter-storage");
        let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../plugins/official/ihub-plugin-base-converter")
            .canonicalize()
            .expect("official Base Converter package should exist in the workspace");
        let manager = manager_at(storage.clone());

        let linked = manager
            .link_from_local(&package.to_string_lossy())
            .expect("official Base Converter package should link as a local project");
        assert_eq!(linked.id, "ihub-plugin-base-converter");
        assert!(linked.is_development_link);
        assert_eq!(linked.commands.len(), 1);
        assert_eq!(linked.commands[0].id, "convert-base");
        assert_eq!(linked.search_providers.len(), 1);
        assert_eq!(linked.search_providers[0].id, "base-converter");
        assert_eq!(
            manager
                .frontend_path("ihub-plugin-base-converter")
                .expect("built frontend path"),
            package.join("dist/index.html")
        );
        assert!(package.join("dist/main.js").is_file());
        assert!(package.join("dist/style.css").is_file());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn official_screen_record_package_is_linkable_with_its_built_frontend() {
        let storage = temporary_directory("official-screen-record-storage");
        let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../plugins/official/ihub-plugin-screen-record")
            .canonicalize()
            .expect("official Screen Recorder package should exist in the workspace");
        let manager = manager_at(storage.clone());

        let linked = manager
            .link_from_local(&package.to_string_lossy())
            .expect("official Screen Recorder package should link as a local project");
        assert_eq!(linked.id, "ihub-plugin-screen-record");
        assert!(linked.is_development_link);
        assert_eq!(linked.commands.len(), 1);
        assert_eq!(linked.commands[0].id, "open-screen-recorder");
        assert!(!linked.has_native_worker);
        assert_eq!(
            manager
                .frontend_path("ihub-plugin-screen-record")
                .expect("built frontend path"),
            package.join("dist/index.html")
        );
        assert!(package.join("dist/main.js").is_file());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn official_window_layout_package_is_linkable_with_its_built_frontend() {
        let storage = temporary_directory("official-window-layout-storage");
        let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../plugins/official/ihub-plugin-window-manager")
            .canonicalize()
            .expect("official Window Layout package should exist in the workspace");
        let manager = manager_at(storage.clone());

        let linked = manager
            .link_from_local(&package.to_string_lossy())
            .expect("official Window Layout package should link as a local project");
        assert_eq!(linked.id, "ihub-plugin-window-manager");
        assert!(linked.is_development_link);
        assert_eq!(linked.commands.len(), 4);
        assert!(linked
            .commands
            .iter()
            .all(|command| command.execution == "frontend"));
        assert!(!linked.has_native_worker);
        assert_eq!(
            manager
                .frontend_path("ihub-plugin-window-manager")
                .expect("built frontend path"),
            package.join("dist/index.html")
        );
        assert!(package.join("dist/main.js").is_file());
        assert!(package.join("dist/style.css").is_file());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn official_image_tools_package_is_linkable_with_its_built_frontend() {
        let storage = temporary_directory("official-image-tools-storage");
        let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../plugins/official/ihub-plugin-image-tools")
            .canonicalize()
            .expect("official Image Tools package should exist in the workspace");
        let manager = manager_at(storage.clone());

        let linked = manager
            .link_from_local(&package.to_string_lossy())
            .expect("official Image Tools package should link as a local project");
        assert_eq!(linked.id, "ihub-plugin-image-tools");
        assert!(linked.is_development_link);
        assert_eq!(linked.commands.len(), 1);
        assert_eq!(linked.commands[0].id, "open-image-tools");
        assert!(!linked.has_native_worker);
        assert_eq!(
            manager
                .frontend_path("ihub-plugin-image-tools")
                .expect("built frontend path"),
            package.join("dist/index.html")
        );
        assert!(package.join("dist/main.js").is_file());
        assert!(package.join("dist/style.css").is_file());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn official_text_tools_package_is_linkable_with_its_built_frontend() {
        let storage = temporary_directory("official-text-tools-storage");
        let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../plugins/official/ihub-plugin-text-tools")
            .canonicalize()
            .expect("official Text Tools package should exist in the workspace");
        let manager = manager_at(storage.clone());

        let linked = manager
            .link_from_local(&package.to_string_lossy())
            .expect("official Text Tools package should link as a local project");
        assert_eq!(linked.id, "ihub-plugin-text-tools");
        assert!(linked.is_development_link);
        assert_eq!(linked.commands.len(), 2);
        assert_eq!(linked.commands[0].id, "open-text-tools");
        assert_eq!(linked.commands[1].id, "process-launcher-text");
        assert_eq!(linked.commands[1].execution, "frontend");
        assert!(linked
            .launcher_context
            .as_ref()
            .is_some_and(|permissions| permissions.text));
        assert_eq!(
            manager
                .frontend_path("ihub-plugin-text-tools")
                .expect("built frontend path"),
            package.join("dist/index.html")
        );
        assert!(package.join("dist/main.js").is_file());
        assert!(package.join("dist/style.css").is_file());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn official_batch_rename_package_is_linkable_with_its_built_frontend() {
        let storage = temporary_directory("official-batch-rename-storage");
        let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../plugins/official/ihub-plugin-batch-rename")
            .canonicalize()
            .expect("official Batch Rename package should exist in the workspace");
        let manager = manager_at(storage.clone());

        let linked = manager
            .link_from_local(&package.to_string_lossy())
            .expect("official Batch Rename package should link as a local project");
        assert_eq!(linked.id, "ihub-plugin-batch-rename");
        assert!(linked.is_development_link);
        assert_eq!(linked.commands.len(), 2);
        assert_eq!(linked.commands[0].id, "batch-rename");
        assert_eq!(linked.commands[1].id, "rename-launcher-files");
        assert_eq!(linked.commands[1].execution, "frontend");
        assert!(linked
            .launcher_context
            .as_ref()
            .is_some_and(|permissions| permissions.files));
        assert_eq!(linked.search_providers.len(), 1);
        assert_eq!(linked.search_providers[0].id, "batch-rename-actions");
        assert_eq!(
            manager
                .frontend_path("ihub-plugin-batch-rename")
                .expect("built frontend path"),
            package.join("dist/index.html")
        );
        assert!(package.join("dist/main.js").is_file());
        assert!(package.join("dist/style.css").is_file());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn official_quick_note_package_is_linkable_with_its_built_frontend() {
        let storage = temporary_directory("official-quick-note-storage");
        let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../plugins/official/ihub-plugin-quick-note")
            .canonicalize()
            .expect("official Quick Note package should exist in the workspace");
        let manager = manager_at(storage.clone());

        let linked = manager
            .link_from_local(&package.to_string_lossy())
            .expect("official Quick Note package should link as a local project");
        assert_eq!(linked.id, "ihub-plugin-quick-note");
        assert!(linked.is_development_link);
        assert_eq!(linked.commands.len(), 1);
        assert_eq!(linked.commands[0].id, "new-note");
        assert_eq!(linked.search_providers.len(), 1);
        assert_eq!(linked.search_providers[0].id, "quick-notes");
        assert_eq!(
            manager
                .frontend_path("ihub-plugin-quick-note")
                .expect("built frontend path"),
            package.join("dist/index.html")
        );
        assert!(package.join("dist/main.js").is_file());
        assert!(package.join("dist/style.css").is_file());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn official_clipboard_package_is_linkable_with_its_built_frontend() {
        let storage = temporary_directory("official-clipboard-storage");
        let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../plugins/official/ihub-plugin-clipboard")
            .canonicalize()
            .expect("official Clipboard History package should exist in the workspace");
        let manager = manager_at(storage.clone());

        let linked = manager
            .link_from_local(&package.to_string_lossy())
            .expect("official Clipboard History package should link as a local project");
        assert_eq!(linked.id, "ihub-plugin-clipboard");
        assert!(linked.is_development_link);
        assert_eq!(linked.commands.len(), 1);
        assert_eq!(linked.commands[0].id, "open-clipboard-history");
        assert_eq!(linked.search_providers.len(), 1);
        assert_eq!(linked.search_providers[0].id, "clipboard-history");
        assert_eq!(
            manager
                .frontend_path("ihub-plugin-clipboard")
                .expect("built frontend path"),
            package.join("dist/index.html")
        );
        assert!(package.join("dist/main.js").is_file());
        assert!(package.join("dist/style.css").is_file());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn official_developer_tools_package_is_linkable_with_its_built_frontend() {
        let storage = temporary_directory("official-developer-tools-storage");
        let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../plugins/official/ihub-plugin-developer-tools")
            .canonicalize()
            .expect("official Developer Tools package should exist in the workspace");
        let manager = manager_at(storage.clone());

        let linked = manager
            .link_from_local(&package.to_string_lossy())
            .expect("official Developer Tools package should link as a local project");
        assert_eq!(linked.id, "ihub-plugin-developer-tools");
        assert!(linked.is_development_link);
        assert_eq!(linked.commands.len(), 1);
        assert_eq!(linked.commands[0].id, "create-plugin-project");
        assert_eq!(
            manager
                .frontend_path("ihub-plugin-developer-tools")
                .expect("built frontend path"),
            package.join("dist/index.html")
        );
        assert!(package.join("dist/main.js").is_file());
        assert!(package.join("dist/style.css").is_file());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn local_pdf_tools_package_is_linkable_with_its_built_frontend_and_no_host_permissions() {
        let storage = temporary_directory("local-pdf-tools-storage");
        let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../plugins/official/ihub-plugin-pdf-tools")
            .canonicalize()
            .expect("local PDF Tools package should exist in the workspace");
        let manager = manager_at(storage.clone());

        let linked = manager
            .link_from_local(&package.to_string_lossy())
            .expect("local PDF Tools package should link as a development project");
        assert_eq!(linked.id, "ihub-plugin-pdf-tools");
        assert!(linked.is_development_link);
        assert_eq!(linked.commands.len(), 1);
        assert_eq!(linked.commands[0].id, "open-pdf-tools");
        assert_eq!(linked.commands[0].execution, "frontend");
        assert!(!linked.has_native_worker);
        assert!(linked.launcher_context.is_none());
        assert_eq!(
            manager
                .frontend_path("ihub-plugin-pdf-tools")
                .expect("built frontend path"),
            package.join("dist/index.html")
        );
        assert!(package.join("dist/main.js").is_file());
        assert!(package.join("dist/style.css").is_file());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn local_archive_tools_package_is_linkable_with_its_built_frontend_and_no_host_permissions() {
        let storage = temporary_directory("local-archive-tools-storage");
        let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../plugins/official/ihub-plugin-archive-tools")
            .canonicalize()
            .expect("local Archive Tools package should exist in the workspace");
        let manager = manager_at(storage.clone());

        let linked = manager
            .link_from_local(&package.to_string_lossy())
            .expect("local Archive Tools package should link as a development project");
        assert_eq!(linked.id, "ihub-plugin-archive-tools");
        assert!(linked.is_development_link);
        assert_eq!(linked.commands.len(), 1);
        assert_eq!(linked.commands[0].id, "open-archive-tools");
        assert_eq!(linked.commands[0].execution, "frontend");
        assert!(!linked.has_native_worker);
        assert!(linked.launcher_context.is_none());
        assert_eq!(
            manager
                .frontend_path("ihub-plugin-archive-tools")
                .expect("built frontend path"),
            package.join("dist/index.html")
        );
        assert!(package.join("dist/main.js").is_file());
        assert!(package.join("dist/style.css").is_file());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn local_web_actions_package_is_linkable_with_only_explicit_external_open_permission() {
        let storage = temporary_directory("local-web-actions-storage");
        let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../plugins/official/ihub-plugin-web-actions")
            .canonicalize()
            .expect("local Web Actions package should exist in the workspace");
        let manager = manager_at(storage.clone());

        let linked = manager
            .link_from_local(&package.to_string_lossy())
            .expect("local Web Actions package should link as a development project");
        assert_eq!(linked.id, "ihub-plugin-web-actions");
        assert!(linked.is_development_link);
        assert_eq!(linked.commands.len(), 1);
        assert_eq!(linked.commands[0].id, "open-web-actions");
        assert_eq!(linked.commands[0].execution, "frontend");
        assert!(!linked.has_native_worker);
        assert!(linked.launcher_context.is_none());
        assert_eq!(
            manager
                .frontend_path("ihub-plugin-web-actions")
                .expect("built frontend path"),
            package.join("dist/index.html")
        );
        assert!(package.join("dist/main.js").is_file());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn official_workspace_projects_are_a_fixed_validated_allowlist() {
        let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .canonicalize()
            .expect("workspace root");
        let actual_specs = OFFICIAL_WORKSPACE_PLUGIN_SPECS
            .iter()
            .map(|spec| (spec.id, spec.name, spec.directory))
            .collect::<Vec<_>>();
        assert_eq!(
            actual_specs,
            vec![
                (
                    "ihub-plugin-archive-tools",
                    "Archive Tools",
                    "ihub-plugin-archive-tools",
                ),
                (
                    "ihub-plugin-base-converter",
                    "Base Converter",
                    "ihub-plugin-base-converter",
                ),
                (
                    "ihub-plugin-batch-rename",
                    "Batch Rename",
                    "ihub-plugin-batch-rename",
                ),
                (
                    "ihub-plugin-clipboard",
                    "Clipboard History",
                    "ihub-plugin-clipboard",
                ),
                (
                    "ihub-plugin-colorpick",
                    "Color Picker",
                    "ihub-plugin-colorpick",
                ),
                (
                    "ihub-plugin-developer-tools",
                    "Plugin Developer Tools",
                    "ihub-plugin-developer-tools",
                ),
                (
                    "ihub-plugin-image-tools",
                    "Image Tools",
                    "ihub-plugin-image-tools",
                ),
                (
                    "ihub-plugin-json-tools",
                    "JSON Tools",
                    "ihub-plugin-json-tools",
                ),
                ("ihub-plugin-ocr", "OCR", "ihub-plugin-ocr"),
                (
                    "ihub-plugin-pdf-tools",
                    "PDF Tools",
                    "ihub-plugin-pdf-tools"
                ),
                ("ihub-plugin-qrcode", "QR Code", "ihub-plugin-qrcode"),
                (
                    "ihub-plugin-quick-note",
                    "Quick Note",
                    "ihub-plugin-quick-note",
                ),
                (
                    "ihub-plugin-screen-record",
                    "Screen Recorder",
                    "ihub-plugin-screen-record",
                ),
                (
                    "ihub-plugin-screenshot",
                    "Screenshot",
                    "ihub-plugin-screenshot",
                ),
                (
                    "ihub-plugin-text-tools",
                    "Text Tools",
                    "ihub-plugin-text-tools",
                ),
                (
                    "ihub-plugin-translate",
                    "Translate",
                    "ihub-plugin-translate",
                ),
                (
                    "ihub-plugin-web-actions",
                    "Web Actions",
                    "ihub-plugin-web-actions",
                ),
                (
                    "ihub-plugin-window-manager",
                    "iHub Window Layout",
                    "ihub-plugin-window-manager",
                ),
            ]
        );
        for spec in &OFFICIAL_WORKSPACE_PLUGIN_SPECS {
            let (path, name) = resolve_official_workspace_plugin_at(&workspace_root, spec)
                .expect("allowlisted project should have a valid manifest and built frontend");
            assert_eq!(
                path.file_name().and_then(OsStr::to_str),
                Some(spec.directory)
            );
            assert_eq!(name, spec.name);
        }

        let storage = temporary_directory("official-workspace-allowlist-storage");
        let manager = manager_at(storage.clone());
        let error = manager
            .link_official_workspace_plugin("ihub-plugin-not-allowlisted")
            .expect_err("an arbitrary renderer ID must never resolve a path");
        assert!(error.contains("not an allowlisted official workspace project"));
        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn local_link_rejects_relative_or_escaping_manifest_paths() {
        let storage = temporary_directory("invalid-storage");
        let source = temporary_directory("invalid-source");
        write_plugin(
            &source,
            "ihub-plugin-invalid-demo",
            "Invalid demo",
            "../outside.html",
        );
        let manager = manager_at(storage.clone());

        let relative_error = manager
            .link_from_local("relative-plugin-directory")
            .expect_err("relative paths should be rejected");
        assert!(relative_error.contains("absolute path"));
        let manifest_error = manager
            .link_from_local(&source.to_string_lossy())
            .expect_err("escaping frontend path should be rejected");
        assert!(manifest_error.contains("relative paths"));
        assert!(
            !storage.join(LOCAL_LINKS_RECORD).exists(),
            "invalid links must not be recorded"
        );

        let _ = fs::remove_dir_all(storage);
        let _ = fs::remove_dir_all(source);
    }

    #[test]
    fn unlinking_a_local_link_preserves_the_project_and_restores_snapshot() {
        let storage = temporary_directory("shadow-storage");
        let source = temporary_directory("shadow-source");
        let plugin_id = "ihub-plugin-shadow-demo";
        write_plugin(
            &storage.join(plugin_id),
            plugin_id,
            "Managed snapshot",
            "dist/index.html",
        );
        write_plugin(&source, plugin_id, "Local development", "dist/index.html");
        let manager = manager_at(storage.clone());

        manager
            .link_from_local(&source.to_string_lossy())
            .expect("local link");
        assert_eq!(manager.list()[0].name, "Local development");
        manager
            .unlink_from_local(plugin_id)
            .expect("unlink should remove only metadata");
        assert!(source.join("plugin.json").is_file());
        assert_eq!(manager.list()[0].name, "Managed snapshot");

        let _ = fs::remove_dir_all(storage);
        let _ = fs::remove_dir_all(source);
    }
}
