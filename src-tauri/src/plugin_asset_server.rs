//! Per-plugin loopback asset servers for TypeScript plugin frontends.
//!
//! Plugin HTML must not be loaded through Tauri's local asset protocol: on
//! Windows/WebView2, injected initialization scripts can reach subframes.
//! Instead, every iframe receives a fresh 127.0.0.1 origin backed by only the
//! canonical directory containing its declared built entry. Tauri treats this
//! as a remote origin, while the parent uses an explicit postMessage bridge.

use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read, Write},
    net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream},
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, RwLock,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::plugins::{PluginFrontendAssetBundle, UtoolsCompatRuntimeConfig};
use crate::utools_db::UtoolsDocumentStore;

const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(16);
const CONNECTION_READ_TIMEOUT: Duration = Duration::from_millis(250);
const CONNECTION_WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const FRONTEND_LEASE_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const ASSET_STREAM_CHUNK_BYTES: usize = 64 * 1024;
const MAX_HTTP_HEADER_BYTES: usize = 16 * 1024;
const MAX_COMPAT_ENTRY_BYTES: usize = 2 * 1024 * 1024;
const UTOOLS_COMPAT_SCRIPT_NAME: &str = "__ihub_utools_compat.js";
const UTOOLS_PRELOAD_SCRIPT_NAME: &str = "__ihub_utools_preload.js";
const UTOOLS_SYNC_DB_ROUTE: &str = "__ihub_utools_db_sync";
const UTOOLS_SYNC_DB_HEADER: &str = "x-ihub-utools-db";
const UTOOLS_SYNC_SCREEN_ROUTE: &str = "__ihub_utools_screen_sync";
const UTOOLS_SYNC_ICON_ROUTE: &str = "__ihub_utools_icon_sync";
const UTOOLS_SYNC_ICON_HEADER: &str = "x-ihub-utools-icon";
const UTOOLS_SYNC_DIALOG_ROUTE: &str = "__ihub_utools_dialog_sync";
const UTOOLS_SYNC_DIALOG_HEADER: &str = "x-ihub-utools-dialog";
const UTOOLS_SYNC_CLIPBOARD_ROUTE: &str = "__ihub_utools_clipboard_sync";
const UTOOLS_SYNC_CLIPBOARD_HEADER: &str = "x-ihub-utools-clipboard";
const MAX_UTOOLS_SYNC_DB_REQUEST_BYTES: usize = 15 * 1024 * 1024;
const MAX_UTOOLS_SYNC_ICON_REQUEST_BYTES: usize = 12 * 1024;
const MAX_UTOOLS_SYNC_DIALOG_REQUEST_BYTES: usize = 32 * 1024;
const MAX_UTOOLS_COPIED_FILE_ITEMS: usize = 32;
const MAX_UTOOLS_CLIPBOARD_FILE_LIST_SOURCE_BYTES: usize = 256 * 1024;
#[cfg(not(test))]
const UTOOLS_SYNC_ICON_TIMEOUT: Duration = Duration::from_millis(650);
// Parallel Windows Shell tests can starve the process-wide STA briefly on a
// loaded runner; production keeps the much tighter synchronous UI bound.
#[cfg(test)]
const UTOOLS_SYNC_ICON_TIMEOUT: Duration = Duration::from_secs(5);
/// Must stay distinct from `tauri.conf.json`'s `build.devUrl`. Tauri treats a
/// URL relative to that development origin as local, which would defeat the
/// remote-origin boundary if the OS happened to assign this port while dev is
/// not running.
const RESERVED_TAURI_DEV_PORT: u16 = 1420;
const LOOPBACK_BIND_ATTEMPTS: usize = 16;
/// A malformed host UI must not turn every catalog item into a resident
/// listener thread. The normal launcher has one active iframe lease per
/// plugin, so this remains comfortably above expected use while keeping
/// resource failure explicit.
const MAX_ACTIVE_FRONTEND_LEASES: usize = 96;
const MAX_ACTIVE_BROWSER_LEASES_PER_PLUGIN: usize = 8;
/// Native workers and narrow host-native operations (such as one confirmed
/// cursor-pixel read) can outlive their bridge check. Keep their concurrent
/// count deliberately small so a malicious or buggy iframe cannot exhaust the
/// process table or permanently starve lifecycle operations.
const MAX_ACTIVE_NATIVE_COMMANDS: usize = 4;
const MAX_ACTIVE_NATIVE_COMMANDS_PER_PLUGIN: usize = 1;
const LOCKED_PLUGIN_CSP: &str = concat!(
    "default-src 'self'; base-uri 'none'; object-src 'none'; form-action 'none'; ",
    "frame-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; ",
    "img-src 'self' data: blob:; font-src 'self' data:; ",
    "media-src 'self' data: blob:; worker-src 'self' blob:; ",
    "connect-src 'self'"
);
const NETWORKED_PLUGIN_CSP: &str = concat!(
    "default-src 'self'; base-uri 'none'; object-src 'none'; form-action 'none'; ",
    "frame-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; ",
    "img-src 'self' data: blob: https:; font-src 'self' data:; ",
    "media-src 'self' blob: https:; worker-src 'self' blob:; ",
    "connect-src 'self' https: wss:"
);
const LOCKED_BROWSER_PLUGIN_CSP: &str = concat!(
    "default-src 'self'; base-uri 'none'; object-src 'none'; form-action 'none'; ",
    "frame-src 'none'; script-src 'self' 'unsafe-eval'; style-src 'self' 'unsafe-inline'; ",
    "img-src 'self' data: blob:; font-src 'self' data:; ",
    "media-src 'self' data: blob:; worker-src 'self' blob:; ",
    "connect-src 'self'"
);
const NETWORKED_BROWSER_PLUGIN_CSP: &str = concat!(
    "default-src 'self'; base-uri 'none'; object-src 'none'; form-action 'none'; ",
    "frame-src 'none'; script-src 'self' 'unsafe-eval'; style-src 'self' 'unsafe-inline'; ",
    "img-src 'self' data: blob: https:; font-src 'self' data:; ",
    "media-src 'self' blob: https:; worker-src 'self' blob:; ",
    "connect-src 'self' https: wss:"
);

/// A short-lived source URL for one plugin iframe. `origin` is returned
/// separately so the frontend can use it as the exact postMessage target.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginFrontendLease {
    pub(crate) lease_id: String,
    pub(crate) url: String,
    pub(crate) origin: String,
    /// Native-projected Permissions Policy capability. It can be true only
    /// for a validated `screenCapture` manifest and a visible surface lease.
    pub(crate) allows_display_capture: bool,
    /// Native-projected Permissions Policy capability. It can be true only
    /// for a validated `microphone` manifest and a visible surface lease.
    pub(crate) allows_microphone: bool,
}

/// The trusted React host tells the asset server why it is creating a lease.
/// A visible surface can present one-off consent UI; a hidden search runtime
/// cannot request cursor pixels or other user-presence-only capabilities.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PluginFrontendPurpose {
    Surface,
    Runtime,
    Browser,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReleasedPluginFrontendLease {
    pub(crate) plugin_id: String,
    pub(crate) purpose: PluginFrontendPurpose,
}

impl std::ops::Deref for ReleasedPluginFrontendLease {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.plugin_id
    }
}

#[derive(Clone, Debug)]
pub(crate) struct UtoolsDialogRequest {
    pub(crate) plugin_id: String,
    pub(crate) lease_id: String,
    pub(crate) kind: String,
    pub(crate) options: Value,
}

type UtoolsDialogHandler =
    Arc<dyn Fn(UtoolsDialogRequest) -> Result<Value, String> + Send + Sync + 'static>;

#[derive(Clone)]
pub(crate) struct PluginAssetServer {
    inner: Arc<PluginAssetServerInner>,
}

struct PluginAssetServerInner {
    leases: Mutex<HashMap<String, ActiveLease>>,
    /// Source transitions exclude Bridge execution so a lease cannot be
    /// checked, then revoked, before a host operation begins. Bridge calls
    /// share the read side and therefore do not serialize normal plugins.
    /// A local-link manifest reveals its plugin ID only after validation, so a
    /// host-wide transition lock avoids a brief unknown-ID race there.
    operation: RwLock<()>,
    /// A long-running known-plugin update releases `operation` while Git is
    /// resolving/checking out. This counter keeps that plugin's old document
    /// revoked and rejects a new source lease until its transition ends.
    transitions: Mutex<HashMap<String, usize>>,
    /// Native command reservations outlive the short Bridge read lock. Source
    /// transitions reject while this is non-empty instead of waiting behind a
    /// worker for its full command timeout.
    native_commands: Mutex<HashMap<String, usize>>,
    /// Synchronous uTools dialogs originate on a per-plugin loopback worker,
    /// but native pickers must be created on Tauri's UI thread. The app
    /// installs one trusted dispatcher during setup; package code can only
    /// reach it through a current visible lease and a bounded JSON request.
    utools_dialog_handler: Mutex<Option<UtoolsDialogHandler>>,
}

/// Keeps one host-native reservation active until its operation returns. The
/// guard deliberately owns the shared server state rather than borrowing a
/// Bridge lock, allowing a slow worker or fixed-delay host read to run without
/// blocking unrelated plugin calls or every source/lifecycle transition.
pub(crate) struct PluginNativeCommandLease {
    inner: Arc<PluginAssetServerInner>,
    plugin_id: String,
}

impl Drop for PluginNativeCommandLease {
    fn drop(&mut self) {
        let mut commands = self
            .inner
            .native_commands
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(count) = commands.get_mut(&self.plugin_id) else {
            return;
        };
        if *count <= 1 {
            commands.remove(&self.plugin_id);
        } else {
            *count -= 1;
        }
    }
}

struct ActiveLease {
    plugin_id: String,
    purpose: PluginFrontendPurpose,
    shutdown: Arc<AtomicBool>,
    last_heartbeat: Arc<Mutex<Instant>>,
    worker: JoinHandle<()>,
}

struct ServedBundle {
    plugin_id: String,
    lease_id: String,
    purpose: PluginFrontendPurpose,
    asset_root: PathBuf,
    entry: PathBuf,
    synthetic_entry: bool,
    blocked_asset_paths: Vec<PathBuf>,
    route_token: String,
    allows_remote_network: bool,
    utools_compat_script: Option<Vec<u8>>,
    utools_preload_script: Option<Vec<u8>>,
    utools_documents: Option<UtoolsDocumentStore>,
    utools_browser_preload_src: Option<String>,
}

#[derive(Clone, Copy)]
enum HttpMethod {
    Get,
    Head,
    Post,
}

struct HttpRequest {
    method: HttpMethod,
    target: String,
    headers: HashMap<String, String>,
    buffered_body: Vec<u8>,
}

impl PluginAssetServer {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(PluginAssetServerInner {
                leases: Mutex::new(HashMap::new()),
                operation: RwLock::new(()),
                transitions: Mutex::new(HashMap::new()),
                native_commands: Mutex::new(HashMap::new()),
                utools_dialog_handler: Mutex::new(None),
            }),
        }
    }

    pub(crate) fn set_utools_dialog_handler(&self, handler: UtoolsDialogHandler) {
        *self
            .inner
            .utools_dialog_handler
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(handler);
    }

    /// Runs a source/lifecycle transition exclusively. Issuing, releasing or
    /// revoking a lease must not interleave with a Bridge call that already
    /// passed its lease check.
    pub(crate) fn with_plugin_operation<T>(
        &self,
        _plugin_id: &str,
        operation: impl FnOnce() -> T,
    ) -> T {
        let _guard = self
            .inner
            .operation
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        operation()
    }

    /// Runs a source or lifecycle mutation only when no native worker is
    /// active. The write lock keeps a Bridge call from reserving a worker
    /// between that check and the mutation. Failing promptly is intentional:
    /// users can retry after the worker completes instead of seeing a 60s UI
    /// stall while a command sits behind a shared lock.
    pub(crate) fn with_plugin_source_operation<T>(
        &self,
        operation: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let _guard = self
            .inner
            .operation
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_no_native_commands()?;
        operation()
    }

    /// Runs a Bridge call while preventing an exclusive source transition.
    /// Independent plugin bridge calls deliberately run concurrently; their
    /// own host state uses narrow locks around individual maps.
    pub(crate) fn with_plugin_bridge_operation<T>(
        &self,
        _plugin_id: &str,
        operation: impl FnOnce() -> T,
    ) -> T {
        let _guard = self
            .inner
            .operation
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        operation()
    }

    /// Reserves a bounded native worker slot after a live iframe lease and
    /// manifest permissions have been checked under `with_plugin_bridge_operation`.
    /// A source transition already in progress wins: a frontend cannot start a
    /// worker from a snapshot that is about to be replaced.
    pub(crate) fn begin_native_command(
        &self,
        plugin_id: &str,
    ) -> Result<PluginNativeCommandLease, String> {
        if self.is_transitioning(plugin_id) {
            return Err(format!(
                "Plugin '{plugin_id}' is being updated. Wait for the update to finish before running its native worker."
            ));
        }

        let mut commands = self
            .inner
            .native_commands
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let total = commands.values().sum::<usize>();
        if total >= MAX_ACTIVE_NATIVE_COMMANDS {
            return Err(format!(
                "Too many native plugin commands are already running (limit: {MAX_ACTIVE_NATIVE_COMMANDS}). Try again after one finishes."
            ));
        }
        let count = commands.entry(plugin_id.to_owned()).or_default();
        if *count >= MAX_ACTIVE_NATIVE_COMMANDS_PER_PLUGIN {
            return Err(format!(
                "Plugin '{plugin_id}' already has a native command running. Wait for it to finish before starting another one."
            ));
        }
        *count += 1;
        Ok(PluginNativeCommandLease {
            inner: self.inner.clone(),
            plugin_id: plugin_id.to_owned(),
        })
    }

    /// Must be called under the exclusive operation lock immediately before a
    /// source replacement, disable, unlink, or uninstall. It intentionally
    /// covers all plugins: Git/local imports can discover the target plugin ID
    /// only after inspecting the candidate manifest.
    pub(crate) fn ensure_no_native_commands(&self) -> Result<(), String> {
        let commands = self
            .inner
            .native_commands
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if commands.is_empty() {
            return Ok(());
        }
        let active = commands
            .iter()
            .map(|(plugin_id, count)| format!("{plugin_id} ({count})"))
            .collect::<Vec<_>>()
            .join(", ");
        Err(format!(
            "Cannot change plugin sources or lifecycle while native plugin command(s) are running: {active}. Wait for them to finish and try again."
        ))
    }

    /// Starts a one-origin server for an already-validated bundle. A plugin has
    /// one primary Surface/Runtime lease, while bounded BrowserWindow children
    /// receive independent origins and never replace that primary document.
    ///
    /// The random port is intentional: separate plugins must not share an HTTP
    /// origin, otherwise one iframe could impersonate another over postMessage.
    #[cfg(test)]
    pub(crate) fn issue(
        &self,
        bundle: PluginFrontendAssetBundle,
        purpose: PluginFrontendPurpose,
    ) -> Result<PluginFrontendLease, String> {
        self.issue_with_utools_documents(bundle, purpose, None)
    }

    pub(crate) fn issue_with_utools_documents(
        &self,
        bundle: PluginFrontendAssetBundle,
        purpose: PluginFrontendPurpose,
        utools_documents: Option<UtoolsDocumentStore>,
    ) -> Result<PluginFrontendLease, String> {
        let listener = bind_plugin_listener()?;
        let port = listener
            .local_addr()
            .map_err(|error| format!("Could not read the plugin asset listener address: {error}"))?
            .port();

        let PluginFrontendAssetBundle {
            plugin_id,
            asset_root,
            entry,
            synthetic_entry,
            blocked_asset_paths,
            allows_display_capture,
            allows_microphone,
            allows_remote_network,
            utools_compat,
            utools_preload_script,
            utools_browser_preload_src,
        } = bundle;
        if utools_compat.is_some() != utools_documents.is_some() {
            return Err(
                "A uTools frontend lease requires its plugin-scoped synchronous database."
                    .to_owned(),
            );
        }
        if utools_preload_script.is_some() && utools_compat.is_none() {
            return Err(
                "Only a verified uTools package may receive a sandboxed preload.".to_owned(),
            );
        }
        if self.is_transitioning(&plugin_id) {
            return Err(format!(
                "Plugin '{plugin_id}' is being updated. Wait for the update to finish before reopening it."
            ));
        }
        let lease_id = Uuid::new_v4().to_string();
        let route_token = Uuid::new_v4().to_string();
        let origin = format!("http://127.0.0.1:{port}");
        let lease = PluginFrontendLease {
            lease_id: lease_id.clone(),
            url: format!("{origin}/v1/{route_token}/"),
            origin,
            allows_display_capture: allows_display_capture
                && purpose == PluginFrontendPurpose::Surface,
            allows_microphone: allows_microphone && purpose == PluginFrontendPurpose::Surface,
        };
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = shutdown.clone();
        let last_heartbeat = Arc::new(Mutex::new(Instant::now()));
        let worker_heartbeat = last_heartbeat.clone();
        let utools_compat_script = utools_compat
            .as_ref()
            .map(render_utools_compat_script)
            .transpose()?;
        let worker_bundle = ServedBundle {
            plugin_id: plugin_id.clone(),
            lease_id: lease_id.clone(),
            purpose,
            asset_root,
            entry,
            synthetic_entry,
            blocked_asset_paths,
            route_token,
            allows_remote_network,
            utools_compat_script,
            utools_preload_script,
            utools_documents,
            utools_browser_preload_src,
        };
        let worker_inner = self.inner.clone();
        let worker = thread::Builder::new()
            .name("ihub-plugin-assets".to_owned())
            .spawn(move || {
                serve_loop(
                    listener,
                    worker_bundle,
                    worker_shutdown,
                    worker_heartbeat,
                    worker_inner,
                )
            })
            .map_err(|error| format!("Could not start the plugin asset server: {error}"))?;

        let mut next_lease = Some(ActiveLease {
            plugin_id: plugin_id.clone(),
            purpose,
            shutdown,
            last_heartbeat,
            worker,
        });
        let result = {
            let mut leases = self
                .inner
                .leases
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut removed = take_expired_leases(&mut leases);
            let browser_count = leases
                .values()
                .filter(|existing| {
                    existing.plugin_id == plugin_id
                        && existing.purpose == PluginFrontendPurpose::Browser
                })
                .count();
            let replacement_ids = leases
                .iter()
                .filter(|(_, existing)| {
                    purpose != PluginFrontendPurpose::Browser
                        && existing.plugin_id == plugin_id
                        && existing.purpose != PluginFrontendPurpose::Browser
                })
                .map(|(existing_lease_id, _)| existing_lease_id.clone())
                .collect::<Vec<_>>();
            removed.extend(
                replacement_ids
                    .into_iter()
                    .filter_map(|existing_lease_id| leases.remove(&existing_lease_id)),
            );

            if leases.len() >= MAX_ACTIVE_FRONTEND_LEASES
                || (purpose == PluginFrontendPurpose::Browser
                    && browser_count >= MAX_ACTIVE_BROWSER_LEASES_PER_PLUGIN)
            {
                Err(removed)
            } else {
                leases.insert(
                    lease_id,
                    next_lease
                        .take()
                        .expect("the fresh frontend lease must be available"),
                );
                Ok(removed)
            }
        };

        match result {
            Ok(removed) => {
                for previous in removed {
                    stop_lease(previous);
                }
                Ok(lease)
            }
            Err(removed) => {
                for expired in removed {
                    stop_lease(expired);
                }
                if let Some(unused) = next_lease {
                    stop_lease(unused);
                }
                Err(
                    "Too many plugin frontends are active. Close an existing plugin and try again."
                        .to_owned(),
                )
            }
        }
    }

    /// Stops one iframe source. Dropping a worker handle does not block the
    /// frameless UI; the worker observes the flag within the bounded
    /// accept/read timeout and then releases its loopback socket.
    /// Stops one iframe source and returns its owning plugin ID when this was
    /// the active lease. The caller can then revoke host-owned runtime state
    /// (registered commands, grants, one-shot launcher contexts) for a
    /// surface that a person closed before it consumed its work.
    pub(crate) fn release(&self, lease_id: &str) -> Option<ReleasedPluginFrontendLease> {
        let lease = self
            .inner
            .leases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(lease_id);
        if let Some(lease) = lease {
            let released = ReleasedPluginFrontendLease {
                plugin_id: lease.plugin_id.clone(),
                purpose: lease.purpose,
            };
            stop_lease(lease);
            Some(released)
        } else {
            None
        }
    }

    /// Refreshes a renderer-owned lease. Only the trusted host renderer calls
    /// this command; plugin SDK payloads never receive a lease ID. It lets the
    /// server reclaim a listener after a renderer reload/crash whose React
    /// cleanup did not reach native code.
    pub(crate) fn touch(&self, lease_id: &str) -> bool {
        let expired = {
            let mut leases = self
                .inner
                .leases
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(lease) = leases.get(lease_id) else {
                return false;
            };
            let should_remove = lease.shutdown.load(Ordering::Acquire) || !lease_is_fresh(lease);
            if !should_remove {
                *lease
                    .last_heartbeat
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Instant::now();
                return true;
            }
            leases.remove(lease_id)
        };
        if let Some(lease) = expired {
            stop_lease(lease);
        }
        false
    }

    /// Checks the host-only session binding used by `plugin_host_call`.
    /// A plugin never supplies this value: the parent frame associates the
    /// active lease with the iframe that actually sent the postMessage.
    pub(crate) fn is_active_for(&self, lease_id: &str, plugin_id: &str) -> bool {
        self.inner
            .leases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(lease_id)
            .is_some_and(|lease| {
                lease.plugin_id == plugin_id
                    && !lease.shutdown.load(Ordering::Acquire)
                    && lease_is_fresh(lease)
            })
    }

    /// A hidden search runtime has the same origin/lease protections as a
    /// visible frontend but is intentionally not a user-presence surface.
    pub(crate) fn is_active_surface_for(&self, lease_id: &str, plugin_id: &str) -> bool {
        self.inner
            .leases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(lease_id)
            .is_some_and(|lease| {
                lease.plugin_id == plugin_id
                    && lease.purpose == PluginFrontendPurpose::Surface
                    && !lease.shutdown.load(Ordering::Acquire)
                    && lease_is_fresh(lease)
            })
    }

    pub(crate) fn is_active_browser_for(&self, lease_id: &str, plugin_id: &str) -> bool {
        self.inner
            .leases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(lease_id)
            .is_some_and(|lease| {
                lease.plugin_id == plugin_id
                    && lease.purpose == PluginFrontendPurpose::Browser
                    && !lease.shutdown.load(Ordering::Acquire)
                    && lease_is_fresh(lease)
            })
    }

    /// Revokes all sources for a plugin after its files or lifecycle state
    /// change. It is safe to call when no iframe is currently open.
    pub(crate) fn revoke_plugin(&self, plugin_id: &str) {
        let removed = {
            let mut leases = self
                .inner
                .leases
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let lease_ids = leases
                .iter()
                .filter(|(_, lease)| lease.plugin_id == plugin_id)
                .map(|(lease_id, _)| lease_id.clone())
                .collect::<Vec<_>>();
            lease_ids
                .into_iter()
                .filter_map(|lease_id| leases.remove(&lease_id))
                .collect::<Vec<_>>()
        };
        for lease in removed {
            stop_lease(lease);
        }
    }

    /// Marks a known plugin unavailable while an expensive replacement is
    /// prepared outside the host-wide transition lock. Callers must invoke
    /// this and `finish_plugin_transition` under `with_plugin_operation`.
    pub(crate) fn begin_plugin_transition(&self, plugin_id: &str) {
        let mut transitions = self
            .inner
            .transitions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *transitions.entry(plugin_id.to_owned()).or_default() += 1;
    }

    /// Completes one transition reservation. A counter avoids accidentally
    /// reopening a plugin while a second explicit update is still serialized
    /// by the plugin manager's install lock.
    pub(crate) fn finish_plugin_transition(&self, plugin_id: &str) {
        let mut transitions = self
            .inner
            .transitions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(count) = transitions.get_mut(plugin_id) else {
            return;
        };
        if *count <= 1 {
            transitions.remove(plugin_id);
        } else {
            *count -= 1;
        }
    }

    fn is_transitioning(&self, plugin_id: &str) -> bool {
        self.inner
            .transitions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(plugin_id)
    }
}

fn lease_is_fresh(lease: &ActiveLease) -> bool {
    heartbeat_is_fresh(&lease.last_heartbeat)
}

fn take_expired_leases(leases: &mut HashMap<String, ActiveLease>) -> Vec<ActiveLease> {
    let expired_ids = leases
        .iter()
        .filter(|(_, lease)| !lease_is_fresh(lease))
        .map(|(lease_id, _)| lease_id.clone())
        .collect::<Vec<_>>();
    expired_ids
        .into_iter()
        .filter_map(|lease_id| leases.remove(&lease_id))
        .collect()
}

fn bind_plugin_listener() -> Result<TcpListener, String> {
    for _ in 0..LOOPBACK_BIND_ATTEMPTS {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            .map_err(|error| format!("Could not reserve a local plugin asset port: {error}"))?;
        if listener
            .local_addr()
            .map_err(|error| format!("Could not read the plugin asset listener address: {error}"))?
            .port()
            == RESERVED_TAURI_DEV_PORT
        {
            drop(listener);
            continue;
        }
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("Could not configure the plugin asset listener: {error}"))?;
        return Ok(listener);
    }
    Err(
        "Could not reserve a plugin asset port distinct from the Tauri development origin."
            .to_owned(),
    )
}

impl Drop for PluginAssetServerInner {
    fn drop(&mut self) {
        let leases = match self.leases.get_mut() {
            Ok(leases) => std::mem::take(leases),
            Err(poisoned) => std::mem::take(poisoned.into_inner()),
        };
        for (_, lease) in leases {
            lease.shutdown.store(true, Ordering::Release);
            // Do not block application shutdown on a local peer that is still
            // inside a bounded read/write. The worker owns no app state and
            // sees the shutdown flag as soon as that socket operation returns.
            drop(lease.worker);
        }
    }
}

fn stop_lease(lease: ActiveLease) {
    lease.shutdown.store(true, Ordering::Release);
    // `JoinHandle` deliberately drops here. Waiting for a hostile local client
    // to finish a read would make closing a plugin surface visibly sluggish.
    drop(lease.worker);
}

fn serve_loop(
    listener: TcpListener,
    bundle: ServedBundle,
    shutdown: Arc<AtomicBool>,
    last_heartbeat: Arc<Mutex<Instant>>,
    server: Arc<PluginAssetServerInner>,
) {
    while !shutdown.load(Ordering::Acquire) && heartbeat_is_fresh(&last_heartbeat) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_read_timeout(Some(CONNECTION_READ_TIMEOUT));
                let _ = stream.set_write_timeout(Some(CONNECTION_WRITE_TIMEOUT));
                if shutdown.load(Ordering::Acquire) || !heartbeat_is_fresh(&last_heartbeat) {
                    break;
                }
                handle_connection(&mut stream, &bundle, &shutdown, &last_heartbeat, &server);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
            Err(_) => {
                // A transient local socket error must not expose any path or
                // crash the desktop process. Retry unless the lease was revoked.
                thread::sleep(ACCEPT_POLL_INTERVAL);
            }
        }
    }
}

fn handle_connection(
    stream: &mut TcpStream,
    bundle: &ServedBundle,
    shutdown: &AtomicBool,
    last_heartbeat: &Mutex<Instant>,
    server: &Arc<PluginAssetServerInner>,
) {
    let Some(request) = read_request(stream).ok().flatten() else {
        let _ = write_status(stream, "400 Bad Request");
        return;
    };
    if shutdown.load(Ordering::Acquire) || !heartbeat_is_fresh(last_heartbeat) {
        return;
    }
    if is_utools_sync_dialog_request(bundle, &request.target) {
        let reservation = {
            let _guard = server
                .operation
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if shutdown.load(Ordering::Acquire) || !heartbeat_is_fresh(last_heartbeat) {
                return;
            }
            let active = server
                .leases
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let is_current_surface = active.get(&bundle.lease_id).is_some_and(|lease| {
                lease.plugin_id == bundle.plugin_id
                    && lease.purpose == PluginFrontendPurpose::Surface
                    && bundle.purpose == PluginFrontendPurpose::Surface
            });
            drop(active);
            if !is_current_surface {
                let _ = write_status(stream, "403 Forbidden");
                return;
            }
            let server_handle = PluginAssetServer {
                inner: Arc::clone(server),
            };
            match server_handle.begin_native_command(&bundle.plugin_id) {
                Ok(reservation) => reservation,
                Err(_) => {
                    let _ = write_status(stream, "429 Too Many Requests");
                    return;
                }
            }
        };
        let handler = server
            .utools_dialog_handler
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let Some(handler) = handler else {
            let _ = write_status(stream, "503 Service Unavailable");
            return;
        };
        handle_utools_sync_dialog_request(stream, bundle, request, handler);
        drop(reservation);
        return;
    }
    if is_utools_sync_clipboard_request(bundle, &request.target) {
        let _guard = server
            .operation
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if shutdown.load(Ordering::Acquire) || !heartbeat_is_fresh(last_heartbeat) {
            return;
        }
        let active = server
            .leases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let is_current_surface = active.get(&bundle.lease_id).is_some_and(|lease| {
            lease.plugin_id == bundle.plugin_id
                && lease.purpose == PluginFrontendPurpose::Surface
                && bundle.purpose == PluginFrontendPurpose::Surface
        });
        drop(active);
        if !is_current_surface {
            let _ = write_status(stream, "403 Forbidden");
            return;
        }
        handle_utools_sync_clipboard_request(stream, bundle, request);
        return;
    }
    if is_utools_sync_screen_request(bundle, &request.target) {
        let _guard = server
            .operation
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if shutdown.load(Ordering::Acquire) || !heartbeat_is_fresh(last_heartbeat) {
            return;
        }
        handle_utools_sync_screen_request(stream, bundle, request);
        return;
    }
    if is_utools_sync_icon_request(bundle, &request.target) {
        let _guard = server
            .operation
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if shutdown.load(Ordering::Acquire) || !heartbeat_is_fresh(last_heartbeat) {
            return;
        }
        handle_utools_sync_icon_request(stream, bundle, request);
        return;
    }
    if is_utools_sync_db_request(bundle, &request.target) {
        let _guard = server
            .operation
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if shutdown.load(Ordering::Acquire) || !heartbeat_is_fresh(last_heartbeat) {
            return;
        }
        handle_utools_sync_db_request(stream, bundle, request);
        return;
    }
    if matches!(request.method, HttpMethod::Post) {
        let _ = write_status(stream, "405 Method Not Allowed");
        return;
    }
    if is_utools_compat_script_request(bundle, &request.target) {
        let Some(script) = bundle.utools_compat_script.as_deref() else {
            let _ = write_status(stream, "404 Not Found");
            return;
        };
        let _ = serve_memory_asset(
            stream,
            request.method,
            script,
            "text/javascript; charset=utf-8",
            bundle.allows_remote_network,
            false,
        );
        return;
    }
    if is_utools_preload_script_request(bundle, &request.target) {
        let Some(script) = bundle.utools_preload_script.as_deref() else {
            let _ = write_status(stream, "404 Not Found");
            return;
        };
        let _ = serve_memory_asset(
            stream,
            request.method,
            script,
            "text/javascript; charset=utf-8",
            bundle.allows_remote_network,
            false,
        );
        return;
    }
    if bundle.synthetic_entry && route_relative_path(bundle, &request.target).as_deref() == Some("")
    {
        let preload = bundle
            .utools_preload_script
            .as_ref()
            .map(|_| UTOOLS_PRELOAD_SCRIPT_NAME);
        let document = inject_utools_compat_document(
            b"<!doctype html><html><head></head><body></body></html>".to_vec(),
            preload,
        );
        let _ = serve_memory_asset(
            stream,
            request.method,
            &document,
            "text/html; charset=utf-8",
            bundle.allows_remote_network,
            false,
        );
        return;
    }
    let Some(path) = resolve_asset_path(bundle, &request.target) else {
        let _ = write_status(stream, "404 Not Found");
        return;
    };
    // A file can disappear between canonicalization and opening during a
    // local development rebuild. Close the connection without writing a
    // second HTTP status after a partial 200 response.
    let policy = AssetServePolicy {
        allows_remote_network: bundle.allows_remote_network,
        utools_compat_script: bundle
            .utools_compat_script
            .as_deref()
            .filter(|_| path == bundle.entry),
        utools_browser_preload_src: bundle
            .utools_preload_script
            .as_ref()
            .map(|_| UTOOLS_PRELOAD_SCRIPT_NAME)
            .or(bundle.utools_browser_preload_src.as_deref())
            .filter(|_| path == bundle.entry),
        allows_script_execution: bundle.purpose == PluginFrontendPurpose::Browser,
    };
    let _ = serve_asset(
        stream,
        request.method,
        &path,
        policy,
        shutdown,
        last_heartbeat,
    );
}

fn is_utools_compat_script_request(bundle: &ServedBundle, target: &str) -> bool {
    let Some(relative) = route_relative_path(bundle, target) else {
        return false;
    };
    relative == UTOOLS_COMPAT_SCRIPT_NAME
}

fn is_utools_preload_script_request(bundle: &ServedBundle, target: &str) -> bool {
    bundle.utools_preload_script.is_some()
        && route_relative_path(bundle, target).as_deref() == Some(UTOOLS_PRELOAD_SCRIPT_NAME)
}

fn is_utools_sync_db_request(bundle: &ServedBundle, target: &str) -> bool {
    bundle.utools_documents.is_some()
        && bundle.utools_compat_script.is_some()
        && route_relative_path(bundle, target).as_deref() == Some(UTOOLS_SYNC_DB_ROUTE)
}

fn is_utools_sync_screen_request(bundle: &ServedBundle, target: &str) -> bool {
    bundle.utools_compat_script.is_some()
        && route_relative_path(bundle, target).as_deref() == Some(UTOOLS_SYNC_SCREEN_ROUTE)
}

fn is_utools_sync_icon_request(bundle: &ServedBundle, target: &str) -> bool {
    bundle.utools_compat_script.is_some()
        && route_relative_path(bundle, target).as_deref() == Some(UTOOLS_SYNC_ICON_ROUTE)
}

fn is_utools_sync_dialog_request(bundle: &ServedBundle, target: &str) -> bool {
    bundle.utools_compat_script.is_some()
        && route_relative_path(bundle, target).as_deref() == Some(UTOOLS_SYNC_DIALOG_ROUTE)
}

fn is_utools_sync_clipboard_request(bundle: &ServedBundle, target: &str) -> bool {
    bundle.utools_compat_script.is_some()
        && route_relative_path(bundle, target).as_deref() == Some(UTOOLS_SYNC_CLIPBOARD_ROUTE)
}

fn handle_utools_sync_clipboard_request(
    stream: &mut TcpStream,
    bundle: &ServedBundle,
    request: HttpRequest,
) {
    let result = execute_utools_sync_clipboard_request(request, || {
        match crate::clipboard_access::try_with_bounded_file_clipboard(
            MAX_UTOOLS_CLIPBOARD_FILE_LIST_SOURCE_BYTES,
            |clipboard| clipboard.get().file_list(),
        ) {
            Some(Ok(paths)) => Ok(paths),
            Some(Err(arboard::Error::ContentNotAvailable)) | None => Ok(Vec::new()),
            Some(Err(error)) => Err(format!(
                "Could not read the bounded clipboard file list: {error}"
            )),
        }
    });
    let (status, payload) = match result {
        Ok(value) => ("200 OK", value),
        Err(error) => ("400 Bad Request", json!({ "error": error })),
    };
    let encoded = serde_json::to_vec(&payload)
        .unwrap_or_else(|_| br#"{"error":"Could not encode clipboard response."}"#.to_vec());
    let _ = write_json_response(stream, status, &encoded, bundle.allows_remote_network);
}

fn execute_utools_sync_clipboard_request(
    request: HttpRequest,
    read_files: impl FnOnce() -> Result<Vec<PathBuf>, String>,
) -> Result<Value, String> {
    if !matches!(request.method, HttpMethod::Get) {
        return Err("The synchronous uTools clipboard endpoint accepts only GET.".to_owned());
    }
    if request
        .headers
        .get(UTOOLS_SYNC_CLIPBOARD_HEADER)
        .map(String::as_str)
        != Some("1")
    {
        return Err("The synchronous uTools clipboard request header is missing.".to_owned());
    }
    if !request.buffered_body.is_empty()
        || request.headers.contains_key("content-length")
        || request.headers.contains_key("transfer-encoding")
    {
        return Err("The synchronous uTools clipboard request accepts no body.".to_owned());
    }
    if request
        .headers
        .get("sec-fetch-site")
        .is_some_and(|value| value != "same-origin")
    {
        return Err("The synchronous uTools clipboard request is not same-origin.".to_owned());
    }
    let host = request.headers.get("host").ok_or_else(|| {
        "The synchronous uTools clipboard request has no loopback host.".to_owned()
    })?;
    let valid_host = host
        .strip_prefix("127.0.0.1:")
        .and_then(|port| port.parse::<u16>().ok())
        .is_some_and(|port| port != 0);
    if !valid_host {
        return Err("The synchronous uTools clipboard request host is invalid.".to_owned());
    }
    if request
        .headers
        .get("origin")
        .is_some_and(|origin| origin != &format!("http://{host}"))
    {
        return Err("The synchronous uTools clipboard request origin is invalid.".to_owned());
    }

    Ok(project_utools_copied_files(read_files()?))
}

fn project_utools_copied_files(paths: impl IntoIterator<Item = PathBuf>) -> Value {
    let mut seen = std::collections::HashSet::new();
    let files = paths
        .into_iter()
        .take(MAX_UTOOLS_COPIED_FILE_ITEMS)
        .filter_map(|path| {
            let prepared = crate::system_open::prepare_local_open(&path, None).ok()?;
            let path = prepared.path().to_owned();
            if !seen.insert(path.clone()) {
                return None;
            }
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .filter(|name| !name.trim().is_empty())?;
            let is_file = prepared.kind() == crate::system_open::LocalOpenKind::File;
            Some(json!({
                "path": crate::indexer::renderer_display_path(&path),
                "isDiractory": !is_file,
                "isFile": is_file,
                "name": name,
            }))
        })
        .collect::<Vec<_>>();
    Value::Array(files)
}

fn handle_utools_sync_dialog_request(
    stream: &mut TcpStream,
    bundle: &ServedBundle,
    request: HttpRequest,
    handler: UtoolsDialogHandler,
) {
    let result = execute_utools_sync_dialog_request(stream, bundle, request, &handler);
    let (status, payload) = match result {
        Ok(value) => ("200 OK", value),
        Err(error) => ("400 Bad Request", json!({ "error": error })),
    };
    let encoded = serde_json::to_vec(&payload)
        .unwrap_or_else(|_| br#"{"error":"Could not encode dialog response."}"#.to_vec());
    let _ = write_json_response(stream, status, &encoded, bundle.allows_remote_network);
}

fn execute_utools_sync_dialog_request(
    stream: &mut TcpStream,
    bundle: &ServedBundle,
    mut request: HttpRequest,
    handler: &UtoolsDialogHandler,
) -> Result<Value, String> {
    if !matches!(request.method, HttpMethod::Post) {
        return Err("The synchronous uTools dialog endpoint accepts only POST.".to_owned());
    }
    if request
        .headers
        .get(UTOOLS_SYNC_DIALOG_HEADER)
        .map(String::as_str)
        != Some("1")
    {
        return Err("The synchronous uTools dialog request header is missing.".to_owned());
    }
    if request
        .headers
        .get("content-type")
        .and_then(|value| value.split(';').next())
        .map(|value| value.trim().eq_ignore_ascii_case("application/json"))
        != Some(true)
    {
        return Err("The synchronous uTools dialog request must be JSON.".to_owned());
    }
    if request
        .headers
        .get("sec-fetch-site")
        .is_some_and(|value| value != "same-origin")
    {
        return Err("The synchronous uTools dialog request is not same-origin.".to_owned());
    }
    let host = request
        .headers
        .get("host")
        .ok_or_else(|| "The synchronous uTools dialog request has no loopback host.".to_owned())?;
    let valid_host = host
        .strip_prefix("127.0.0.1:")
        .and_then(|port| port.parse::<u16>().ok())
        .is_some_and(|port| port != 0);
    if !valid_host {
        return Err("The synchronous uTools dialog request host is invalid.".to_owned());
    }
    if request
        .headers
        .get("origin")
        .is_some_and(|origin| origin != &format!("http://{host}"))
    {
        return Err("The synchronous uTools dialog request origin is invalid.".to_owned());
    }
    if request.headers.contains_key("transfer-encoding") {
        return Err("Chunked synchronous uTools dialog requests are not accepted.".to_owned());
    }
    let content_length = request
        .headers
        .get("content-length")
        .ok_or_else(|| "The synchronous uTools dialog request has no content length.".to_owned())?
        .parse::<usize>()
        .map_err(|_| "The synchronous uTools dialog content length is invalid.".to_owned())?;
    if content_length == 0 || content_length > MAX_UTOOLS_SYNC_DIALOG_REQUEST_BYTES {
        return Err(format!(
            "Synchronous uTools dialog requests are limited to {MAX_UTOOLS_SYNC_DIALOG_REQUEST_BYTES} bytes."
        ));
    }
    if request.buffered_body.len() > content_length {
        return Err("The synchronous uTools dialog request contains trailing bytes.".to_owned());
    }
    let already_read = request.buffered_body.len();
    request.buffered_body.resize(content_length, 0);
    if already_read < content_length {
        stream
            .read_exact(&mut request.buffered_body[already_read..])
            .map_err(|error| format!("Could not read the synchronous dialog body: {error}"))?;
    }
    let payload = serde_json::from_slice::<Value>(&request.buffered_body)
        .map_err(|error| format!("The synchronous dialog request is invalid JSON: {error}"))?;
    let object = payload
        .as_object()
        .ok_or_else(|| "The synchronous dialog request must be an object.".to_owned())?;
    if object.len() != 2 || !object.contains_key("kind") || !object.contains_key("options") {
        return Err("The synchronous dialog request accepts only kind and options.".to_owned());
    }
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .filter(|kind| matches!(*kind, "open" | "save"))
        .ok_or_else(|| "The synchronous dialog kind must be open or save.".to_owned())?;
    let options = object
        .get("options")
        .filter(|value| value.is_object())
        .cloned()
        .ok_or_else(|| "The synchronous dialog options must be an object.".to_owned())?;
    let result = handler(UtoolsDialogRequest {
        plugin_id: bundle.plugin_id.clone(),
        lease_id: bundle.lease_id.clone(),
        kind: kind.to_owned(),
        options,
    })?;
    match kind {
        "open" if result.is_null() => Ok(result),
        "open"
            if result.as_array().is_some_and(|paths| {
                !paths.is_empty()
                    && paths.len() <= 64
                    && paths.iter().all(|path| {
                        path.as_str().is_some_and(|path| {
                            !path.is_empty()
                                && path.len() <= 8192
                                && !path.chars().any(char::is_control)
                        })
                    })
            }) =>
        {
            Ok(result)
        }
        "save"
            if result.is_null()
                || result.as_str().is_some_and(|path| {
                    !path.is_empty() && path.len() <= 8192 && !path.chars().any(char::is_control)
                }) =>
        {
            Ok(result)
        }
        _ => Err("The native uTools dialog returned an invalid result.".to_owned()),
    }
}

fn handle_utools_sync_icon_request(
    stream: &mut TcpStream,
    bundle: &ServedBundle,
    request: HttpRequest,
) {
    let result = execute_utools_sync_icon_request(stream, request);
    let (status, payload) = match result {
        Ok(value) => ("200 OK", Value::String(value)),
        Err(error) => ("400 Bad Request", json!({ "error": error })),
    };
    let encoded = serde_json::to_vec(&payload)
        .unwrap_or_else(|_| br#"{"error":"Could not encode icon response."}"#.to_vec());
    let _ = write_json_response(stream, status, &encoded, bundle.allows_remote_network);
}

fn execute_utools_sync_icon_request(
    stream: &mut TcpStream,
    mut request: HttpRequest,
) -> Result<String, String> {
    if !matches!(request.method, HttpMethod::Post) {
        return Err("The synchronous uTools icon endpoint accepts only POST.".to_owned());
    }
    if request
        .headers
        .get(UTOOLS_SYNC_ICON_HEADER)
        .map(String::as_str)
        != Some("1")
    {
        return Err("The synchronous uTools icon request header is missing.".to_owned());
    }
    if request
        .headers
        .get("content-type")
        .and_then(|value| value.split(';').next())
        .map(|value| value.trim().eq_ignore_ascii_case("application/json"))
        != Some(true)
    {
        return Err("The synchronous uTools icon request must be JSON.".to_owned());
    }
    if request
        .headers
        .get("sec-fetch-site")
        .is_some_and(|value| value != "same-origin")
    {
        return Err("The synchronous uTools icon request is not same-origin.".to_owned());
    }
    let host = request
        .headers
        .get("host")
        .ok_or_else(|| "The synchronous uTools icon request has no loopback host.".to_owned())?;
    let valid_host = host
        .strip_prefix("127.0.0.1:")
        .and_then(|port| port.parse::<u16>().ok())
        .is_some_and(|port| port != 0);
    if !valid_host {
        return Err("The synchronous uTools icon request host is invalid.".to_owned());
    }
    if request
        .headers
        .get("origin")
        .is_some_and(|origin| origin != &format!("http://{host}"))
    {
        return Err("The synchronous uTools icon request origin is invalid.".to_owned());
    }
    if request.headers.contains_key("transfer-encoding") {
        return Err("Chunked synchronous uTools icon requests are not accepted.".to_owned());
    }
    let content_length = request
        .headers
        .get("content-length")
        .ok_or_else(|| "The synchronous uTools icon request has no content length.".to_owned())?
        .parse::<usize>()
        .map_err(|_| "The synchronous uTools icon content length is invalid.".to_owned())?;
    if content_length == 0 || content_length > MAX_UTOOLS_SYNC_ICON_REQUEST_BYTES {
        return Err(format!(
            "Synchronous uTools icon requests are limited to {MAX_UTOOLS_SYNC_ICON_REQUEST_BYTES} bytes."
        ));
    }
    if request.buffered_body.len() > content_length {
        return Err("The synchronous uTools icon request contains trailing bytes.".to_owned());
    }
    let already_read = request.buffered_body.len();
    request.buffered_body.resize(content_length, 0);
    if already_read < content_length {
        stream
            .read_exact(&mut request.buffered_body[already_read..])
            .map_err(|error| format!("Could not read the synchronous icon body: {error}"))?;
    }
    let payload = serde_json::from_slice::<Value>(&request.buffered_body)
        .map_err(|error| format!("The synchronous icon request is invalid JSON: {error}"))?;
    let object = payload
        .as_object()
        .ok_or_else(|| "The synchronous icon request must be an object.".to_owned())?;
    if object.len() != 1 || !object.contains_key("path") {
        return Err("The synchronous icon request accepts only path.".to_owned());
    }
    let requested = object
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "The synchronous icon path must be a string.".to_owned())?;
    if requested.is_empty()
        || requested.chars().count() > 1024
        || requested.len() > 8192
        || requested.chars().any(|character| character.is_control())
    {
        return Err("The synchronous icon path is invalid or too long.".to_owned());
    }

    let service = crate::native_icons::NativeIconService::shared();
    let pending = if requested == "folder" {
        service.try_request_type_hint(None, true)
    } else if requested.len() >= 2
        && requested.len() <= 17
        && requested.starts_with('.')
        && requested[1..]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric())
    {
        service.try_request_type_hint(Some(requested), false)
    } else {
        let prepared = crate::system_open::prepare_local_open(Path::new(requested), None)?;
        let kind = match prepared.kind() {
            crate::system_open::LocalOpenKind::File => "file",
            crate::system_open::LocalOpenKind::Folder => "folder",
        };
        service.try_request_prepared(prepared, kind)
    };
    Ok(pending
        .and_then(|pending| pending.wait_timeout(UTOOLS_SYNC_ICON_TIMEOUT))
        .unwrap_or_default())
}

fn handle_utools_sync_screen_request(
    stream: &mut TcpStream,
    bundle: &ServedBundle,
    request: HttpRequest,
) {
    let result = execute_utools_sync_screen_request(request);
    let (status, payload) = match result {
        Ok(value) => ("200 OK", value),
        Err(error) => ("400 Bad Request", json!({ "error": error })),
    };
    let encoded = serde_json::to_vec(&payload)
        .unwrap_or_else(|_| br#"{"error":"Could not encode screen response."}"#.to_vec());
    let _ = write_json_response(stream, status, &encoded, bundle.allows_remote_network);
}

fn execute_utools_sync_screen_request(request: HttpRequest) -> Result<Value, String> {
    if !matches!(request.method, HttpMethod::Get) {
        return Err("The synchronous uTools screen endpoint accepts only GET.".to_owned());
    }
    if !request.buffered_body.is_empty()
        || request.headers.contains_key("content-length")
        || request.headers.contains_key("transfer-encoding")
    {
        return Err("The synchronous uTools screen request accepts no body.".to_owned());
    }
    if request
        .headers
        .get("sec-fetch-site")
        .is_some_and(|value| value != "same-origin")
    {
        return Err("The synchronous uTools screen request is not same-origin.".to_owned());
    }
    let host = request
        .headers
        .get("host")
        .ok_or_else(|| "The synchronous uTools screen request has no loopback host.".to_owned())?;
    let valid_host = host
        .strip_prefix("127.0.0.1:")
        .and_then(|port| port.parse::<u16>().ok())
        .is_some_and(|port| port != 0);
    if !valid_host {
        return Err("The synchronous uTools screen request host is invalid.".to_owned());
    }
    if request
        .headers
        .get("origin")
        .is_some_and(|origin| origin != &format!("http://{host}"))
    {
        return Err("The synchronous uTools screen request origin is invalid.".to_owned());
    }
    serde_json::to_value(crate::utools_screen::screen_snapshot()?)
        .map_err(|error| format!("Could not encode the uTools screen snapshot: {error}"))
}

fn handle_utools_sync_db_request(
    stream: &mut TcpStream,
    bundle: &ServedBundle,
    request: HttpRequest,
) {
    let result = execute_utools_sync_db_request(stream, bundle, request);
    let (status, payload) = match result {
        Ok(value) => ("200 OK", value),
        Err(error) => ("400 Bad Request", json!({ "error": error })),
    };
    let encoded = serde_json::to_vec(&payload)
        .unwrap_or_else(|_| br#"{"error":"Could not encode database response."}"#.to_vec());
    let _ = write_json_response(stream, status, &encoded, bundle.allows_remote_network);
}

fn execute_utools_sync_db_request(
    stream: &mut TcpStream,
    bundle: &ServedBundle,
    mut request: HttpRequest,
) -> Result<Value, String> {
    if !matches!(request.method, HttpMethod::Post) {
        return Err("The synchronous uTools database endpoint accepts only POST.".to_owned());
    }
    if request
        .headers
        .get(UTOOLS_SYNC_DB_HEADER)
        .map(String::as_str)
        != Some("1")
    {
        return Err("The synchronous uTools database request header is missing.".to_owned());
    }
    if request
        .headers
        .get("content-type")
        .and_then(|value| value.split(';').next())
        .map(|value| value.trim().eq_ignore_ascii_case("application/json"))
        != Some(true)
    {
        return Err("The synchronous uTools database request must be JSON.".to_owned());
    }
    if request
        .headers
        .get("sec-fetch-site")
        .is_some_and(|value| value != "same-origin")
    {
        return Err("The synchronous uTools database request is not same-origin.".to_owned());
    }
    let host = request.headers.get("host").ok_or_else(|| {
        "The synchronous uTools database request has no loopback host.".to_owned()
    })?;
    let valid_host = host
        .strip_prefix("127.0.0.1:")
        .and_then(|port| port.parse::<u16>().ok())
        .is_some_and(|port| port != 0);
    if !valid_host {
        return Err("The synchronous uTools database request host is invalid.".to_owned());
    }
    if request
        .headers
        .get("origin")
        .is_some_and(|origin| origin != &format!("http://{host}"))
    {
        return Err("The synchronous uTools database request origin is invalid.".to_owned());
    }
    if request.headers.contains_key("transfer-encoding") {
        return Err("Chunked synchronous uTools database requests are not accepted.".to_owned());
    }
    let content_length = request
        .headers
        .get("content-length")
        .ok_or_else(|| "The synchronous uTools database request has no content length.".to_owned())?
        .parse::<usize>()
        .map_err(|_| "The synchronous uTools database content length is invalid.".to_owned())?;
    if content_length == 0 || content_length > MAX_UTOOLS_SYNC_DB_REQUEST_BYTES {
        return Err(format!(
            "Synchronous uTools database requests are limited to {MAX_UTOOLS_SYNC_DB_REQUEST_BYTES} bytes."
        ));
    }
    if request.buffered_body.len() > content_length {
        return Err("The synchronous uTools database request contains trailing bytes.".to_owned());
    }
    let already_read = request.buffered_body.len();
    request.buffered_body.resize(content_length, 0);
    if already_read < content_length {
        stream
            .read_exact(&mut request.buffered_body[already_read..])
            .map_err(|error| format!("Could not read the synchronous database body: {error}"))?;
    }

    let payload = serde_json::from_slice::<Value>(&request.buffered_body)
        .map_err(|error| format!("The synchronous database request is invalid JSON: {error}"))?;
    let object = payload
        .as_object()
        .ok_or_else(|| "The synchronous database request must be an object.".to_owned())?;
    let operation = object
        .get("op")
        .and_then(Value::as_str)
        .ok_or_else(|| "The synchronous database request requires an operation.".to_owned())?;
    let store = bundle
        .utools_documents
        .as_ref()
        .ok_or_else(|| "The synchronous database is unavailable for this plugin.".to_owned())?;
    let plugin_id = &bundle.plugin_id;

    match operation {
        "get" => {
            validate_sync_db_keys(object, &["op", "id"])?;
            Ok(store
                .get(plugin_id, sync_db_string(object, "id")?)?
                .unwrap_or(Value::Null))
        }
        "put" => {
            validate_sync_db_keys(object, &["op", "doc"])?;
            serde_json::to_value(
                store.put(
                    plugin_id,
                    object
                        .get("doc")
                        .ok_or_else(|| "Synchronous db.put requires a document.".to_owned())?
                        .clone(),
                )?,
            )
            .map_err(|error| format!("Could not encode the database result: {error}"))
        }
        "remove" => {
            validate_sync_db_keys(object, &["op", "target"])?;
            serde_json::to_value(
                store.remove(
                    plugin_id,
                    object
                        .get("target")
                        .ok_or_else(|| "Synchronous db.remove requires a target.".to_owned())?,
                )?,
            )
            .map_err(|error| format!("Could not encode the database result: {error}"))
        }
        "bulkDocs" => {
            validate_sync_db_keys(object, &["op", "docs"])?;
            let documents = object
                .get("docs")
                .and_then(Value::as_array)
                .ok_or_else(|| "Synchronous db.bulkDocs requires a document array.".to_owned())?
                .clone();
            serde_json::to_value(store.bulk_docs(plugin_id, documents)?)
                .map_err(|error| format!("Could not encode the database results: {error}"))
        }
        "allDocs" => {
            if object.len() == 1 {
                Ok(Value::Array(store.all_docs(plugin_id, None)?))
            } else {
                validate_sync_db_keys(object, &["op", "selector"])?;
                Ok(Value::Array(
                    store.all_docs(plugin_id, object.get("selector"))?,
                ))
            }
        }
        "postAttachment" => {
            validate_sync_db_keys(object, &["op", "id", "dataBase64", "contentType"])?;
            let encoded = sync_db_string(object, "dataBase64")?;
            let max_encoded = crate::utools_db::MAX_ATTACHMENT_BYTES.div_ceil(3) * 4;
            if encoded.is_empty() || encoded.len() > max_encoded {
                return Err("The synchronous uTools attachment exceeds 10 MiB.".to_owned());
            }
            let bytes = BASE64_STANDARD
                .decode(encoded)
                .map_err(|_| "The synchronous uTools attachment is malformed.".to_owned())?;
            serde_json::to_value(store.post_attachment(
                plugin_id,
                sync_db_string(object, "id")?,
                &bytes,
                sync_db_string(object, "contentType")?,
            )?)
            .map_err(|error| format!("Could not encode the attachment result: {error}"))
        }
        "getAttachment" => {
            validate_sync_db_keys(object, &["op", "id"])?;
            Ok(store
                .get_attachment(plugin_id, sync_db_string(object, "id")?)?
                .map(|bytes| json!({ "dataBase64": BASE64_STANDARD.encode(bytes) }))
                .unwrap_or(Value::Null))
        }
        "getAttachmentType" => {
            validate_sync_db_keys(object, &["op", "id"])?;
            Ok(store
                .get_attachment_type(plugin_id, sync_db_string(object, "id")?)?
                .map(Value::String)
                .unwrap_or(Value::Null))
        }
        _ => Err("The synchronous uTools database operation is unsupported.".to_owned()),
    }
}

fn validate_sync_db_keys(
    object: &serde_json::Map<String, Value>,
    keys: &[&str],
) -> Result<(), String> {
    if object.len() != keys.len() || keys.iter().any(|key| !object.contains_key(*key)) {
        return Err("The synchronous uTools database request shape is invalid.".to_owned());
    }
    Ok(())
}

fn sync_db_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Result<&'a str, String> {
    object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("The synchronous uTools database field '{key}' must be a string."))
}

fn write_json_response(
    stream: &mut TcpStream,
    status: &str,
    body: &[u8],
    allows_remote_network: bool,
) -> io::Result<()> {
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: {}\r\nConnection: close\r\n\r\n",
        body.len(),
        plugin_csp(allows_remote_network, false),
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)
}

fn read_request(stream: &mut TcpStream) -> io::Result<Option<HttpRequest>> {
    let mut header = Vec::with_capacity(1024);
    let mut buffer = [0_u8; 1024];
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Ok(None);
        }
        header.extend_from_slice(&buffer[..read]);
        if header.len() > MAX_HTTP_HEADER_BYTES
            && !header.windows(4).any(|window| window == b"\r\n\r\n")
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP header is too large",
            ));
        }
        if header.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }

    let header_end = header
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing HTTP header end"))?;
    if header_end > MAX_HTTP_HEADER_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "HTTP header is too large",
        ));
    }
    let buffered_body = header.split_off(header_end);
    let header = std::str::from_utf8(&header)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "HTTP header is not UTF-8"))?;
    let request_line = header
        .split("\r\n")
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing request line"))?;
    let mut parts = request_line.split_ascii_whitespace();
    let method = match parts.next() {
        Some("GET") => HttpMethod::Get,
        Some("HEAD") => HttpMethod::Head,
        Some("POST") => HttpMethod::Post,
        _ => return Ok(None),
    };
    let target = parts
        .next()
        .filter(|target| target.starts_with('/'))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid request target"))?;
    if !matches!(parts.next(), Some("HTTP/1.0") | Some("HTTP/1.1")) || parts.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid HTTP request line",
        ));
    }
    let mut headers = HashMap::new();
    for line in header.split("\r\n").skip(1).filter(|line| !line.is_empty()) {
        let Some((name, value)) = line.split_once(':') else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid HTTP header",
            ));
        };
        let name = name.trim().to_ascii_lowercase();
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || headers.insert(name, value.trim().to_owned()).is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid or duplicate HTTP header",
            ));
        }
    }
    Ok(Some(HttpRequest {
        method,
        target: target.to_owned(),
        headers,
        buffered_body,
    }))
}

fn resolve_asset_path(bundle: &ServedBundle, target: &str) -> Option<PathBuf> {
    let relative = route_relative_path(bundle, target)?;
    if relative.is_empty() {
        return Some(bundle.entry.clone());
    }
    if bundle.synthetic_entry {
        return None;
    }

    let relative_path = Path::new(&relative);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    let candidate = bundle.asset_root.join(relative_path).canonicalize().ok()?;
    (candidate.starts_with(&bundle.asset_root)
        && candidate.is_file()
        && !bundle
            .blocked_asset_paths
            .iter()
            .any(|blocked| blocked == &candidate))
    .then_some(candidate)
}

fn route_relative_path(bundle: &ServedBundle, target: &str) -> Option<String> {
    let path_without_query = target.split_once('?').map_or(target, |(path, _)| path);
    let decoded_path = decode_path(path_without_query)?;
    let route = format!("/v1/{}", bundle.route_token);
    if decoded_path == route || decoded_path == format!("{route}/") {
        Some(String::new())
    } else {
        decoded_path.strip_prefix(&(route + "/")).map(str::to_owned)
    }
}

fn decode_path(path: &str) -> Option<String> {
    if !path.starts_with('/') {
        return None;
    }
    let source = path.as_bytes();
    let mut decoded = Vec::with_capacity(source.len());
    let mut index = 0;
    while index < source.len() {
        let byte = source[index];
        if byte == b'%' {
            let high = hex_value(*source.get(index + 1)?)?;
            let low = hex_value(*source.get(index + 2)?)?;
            let decoded_byte = (high << 4) | low;
            if matches!(decoded_byte, b'/' | b'\\' | 0) {
                return None;
            }
            decoded.push(decoded_byte);
            index += 3;
            continue;
        }
        if byte == 0 || byte == b'\\' || byte < 0x20 {
            return None;
        }
        decoded.push(byte);
        index += 1;
    }
    let decoded = String::from_utf8(decoded).ok()?;
    (!decoded.contains(':') && !decoded.contains('\\') && !decoded.contains('\0'))
        .then_some(decoded)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

struct AssetServePolicy<'a> {
    allows_remote_network: bool,
    utools_compat_script: Option<&'a [u8]>,
    utools_browser_preload_src: Option<&'a str>,
    allows_script_execution: bool,
}

fn serve_asset(
    stream: &mut TcpStream,
    method: HttpMethod,
    path: &Path,
    policy: AssetServePolicy<'_>,
    shutdown: &AtomicBool,
    last_heartbeat: &Mutex<Instant>,
) -> io::Result<()> {
    if policy.utools_compat_script.is_some() {
        let document = inject_utools_compat_script(path, policy.utools_browser_preload_src)?;
        return serve_memory_asset(
            stream,
            method,
            &document,
            "text/html; charset=utf-8",
            policy.allows_remote_network,
            policy.allows_script_execution,
        );
    }
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "not a file"));
    }
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: {}\r\nConnection: close\r\n\r\n",
        content_type(path),
        metadata.len(),
        plugin_csp(
            policy.allows_remote_network,
            policy.allows_script_execution
        ),
    );
    stream.write_all(header.as_bytes())?;
    if matches!(method, HttpMethod::Get) {
        let mut buffer = [0_u8; ASSET_STREAM_CHUNK_BYTES];
        loop {
            if shutdown.load(Ordering::Acquire) || !heartbeat_is_fresh(last_heartbeat) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "plugin frontend lease was revoked",
                ));
            }
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            stream.write_all(&buffer[..read])?;
        }
    }
    Ok(())
}

fn serve_memory_asset(
    stream: &mut TcpStream,
    method: HttpMethod,
    body: &[u8],
    content_type: &str,
    allows_remote_network: bool,
    allows_script_execution: bool,
) -> io::Result<()> {
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: {}\r\nConnection: close\r\n\r\n",
        body.len(),
        plugin_csp(allows_remote_network, allows_script_execution),
    );
    stream.write_all(header.as_bytes())?;
    if matches!(method, HttpMethod::Get) {
        stream.write_all(body)?;
    }
    Ok(())
}

fn inject_utools_compat_script(
    entry: &Path,
    utools_browser_preload_src: Option<&str>,
) -> io::Result<Vec<u8>> {
    let metadata = entry.metadata()?;
    if metadata.len() > MAX_COMPAT_ENTRY_BYTES as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "uTools-compatible plugin HTML entry exceeds the 2 MiB injection limit",
        ));
    }
    let mut document = Vec::with_capacity(metadata.len() as usize + 96);
    File::open(entry)?.read_to_end(&mut document)?;
    Ok(inject_utools_compat_document(
        document,
        utools_browser_preload_src,
    ))
}

fn inject_utools_compat_document(
    mut document: Vec<u8>,
    utools_browser_preload_src: Option<&str>,
) -> Vec<u8> {
    let preload = utools_browser_preload_src
        .map(|src| format!("<script src=\"{src}\"></script>"))
        .unwrap_or_default();
    let bootstrap =
        format!("<script src=\"{UTOOLS_COMPAT_SCRIPT_NAME}\"></script>{preload}").into_bytes();
    let insertion = document
        .windows(b"<head".len())
        .position(|window| window.eq_ignore_ascii_case(b"<head"))
        .and_then(|head_start| {
            document[head_start..]
                .iter()
                .position(|byte| *byte == b'>')
                .map(|offset| head_start + offset + 1)
        })
        // A malformed-but-renderable page still receives the bootstrap before
        // any package script instead of silently losing the compatibility API.
        .unwrap_or(0);
    document.splice(insertion..insertion, bootstrap);
    document
}

/// Builds the only script injected into a compatible page. Configuration is
/// JSON-encoded by serde, so package strings cannot escape into executable
/// source. The shim intentionally implements only the public entry lifecycle
/// and one user-confirmed pixel sampler; every call still crosses the existing
/// origin/lease/permission-checked iHub bridge.
fn render_utools_compat_script(config: &UtoolsCompatRuntimeConfig) -> Result<Vec<u8>, String> {
    let config = serde_json::to_string(config)
        .map_err(|error| format!("Could not encode uTools compatibility configuration: {error}"))?;
    Ok(format!(
        r#"(() => {{
"use strict";
const config = {config};
const responseChannel = "ihub-host-bridge/v1";
const requestChannel = "ihub-plugin-bridge/v1";
const copyImageMaxPngBytes = 4194304;
const copyImageMaxDataUrlChars = 5592430;
const attachmentMaxBytes = 10485760;
const syncDbRoute = "__ihub_utools_db_sync";
const syncScreenRoute = "__ihub_utools_screen_sync";
const syncIconRoute = "__ihub_utools_icon_sync";
const syncDialogRoute = "__ihub_utools_dialog_sync";
const syncClipboardRoute = "__ihub_utools_clipboard_sync";
const mainPushProviderId = "utools-main-push";
let sequence = 0;
const pending = new Map();
const readyCallbacks = [];
const enterCallbacks = [];
const outCallbacks = [];
const detachCallbacks = [];
const dbPullCallbacks = [];
let mainPushCallback = null;
let mainPushSelectCallback = null;
let mainPushProviderState = "idle";
let activeMainPushInteractionId = null;
let activeMainPushInteractionCalls = null;
let pluginOutDispatched = false;
let pluginDetachDispatched = false;
let subInputChangeCallback = null;
let currentWindowType = config.windowType;
const ipcListeners = new Map();
const browserWindows = new Map();
const browserReady = new Set();
const declaredTools = new Map(Array.isArray(config.tools)
  ? config.tools.filter((tool) => tool && typeof tool.name === "string").map((tool) => [tool.name, Object.freeze({{ ...tool }})])
  : []);
const toolHandlers = new Map();
const activeToolCalls = new Map();
const activeAiRequests = new Map();
const activeFfmpegRequests = new Map();
let desktopCaptureSlot = null;
let desktopCaptureSequence = 0;
function call(method, params, interactionId, timeoutMs) {{
  const id = "utools-compat-" + (++sequence).toString(36);
  return new Promise((resolve, reject) => {{
    const boundedTimeout = Number.isInteger(timeoutMs) && timeoutMs >= 1000 && timeoutMs <= 125000 ? timeoutMs : 15000;
    const timeout = window.setTimeout(() => {{ pending.delete(id); reject(new Error("iHub host bridge timed out.")); }}, boundedTimeout);
    pending.set(id, {{ resolve, reject, timeout }});
    const request = {{ pluginId: config.pluginId, method, params: params || {{}} }};
    if (interactionId) request.interactionId = interactionId;
    window.parent.postMessage({{ channel: requestChannel, type: "call", id, request }}, "*");
  }});
}}
function interactionCall(method, params) {{
  const interactionId = activeMainPushInteractionId;
  const promise = call(method, params, interactionId);
  if (interactionId && activeMainPushInteractionCalls) activeMainPushInteractionCalls.push(promise.catch(() => undefined));
  return promise;
}}
function boundedBrowserArgs(args) {{
  if (!Array.isArray(args) || args.length > 32) throw new TypeError("uTools BrowserWindow IPC accepts at most 32 arguments.");
  let encoded;
  try {{ encoded = JSON.stringify(args); }} catch {{ throw new TypeError("uTools BrowserWindow IPC arguments must be JSON-serializable."); }}
  if (typeof encoded !== "string" || new TextEncoder().encode(encoded).byteLength > 262144) throw new RangeError("uTools BrowserWindow IPC arguments exceed 256 KiB.");
  return JSON.parse(encoded);
}}
function validBrowserChannel(channel) {{
  return typeof channel === "string" && channel.length > 0 && Array.from(channel).length <= 128 && !/[\u0000-\u001f\u007f]/.test(channel);
}}
function dispatchBrowserIpc(channel, args) {{
  if (!validBrowserChannel(channel) || !Array.isArray(args)) return;
  const listeners = ipcListeners.get(channel);
  if (!listeners) return;
  const event = Object.freeze({{ sender: null }});
  for (const listener of Array.from(listeners)) {{
    try {{ listener(event, ...args); }} catch (error) {{ console.error("uTools BrowserWindow IPC listener failed", error); }}
  }}
}}
const ipcRenderer = Object.freeze({{
  on(channel, listener) {{
    if (!validBrowserChannel(channel) || typeof listener !== "function") return this;
    const listeners = ipcListeners.get(channel) || new Set();
    listeners.add(listener); ipcListeners.set(channel, listeners); return this;
  }},
  once(channel, listener) {{
    if (!validBrowserChannel(channel) || typeof listener !== "function") return this;
    const wrapped = (event, ...args) => {{ this.removeListener(channel, wrapped); listener(event, ...args); }};
    return this.on(channel, wrapped);
  }},
  removeListener(channel, listener) {{
    const listeners = ipcListeners.get(channel); if (listeners) {{ listeners.delete(listener); if (listeners.size === 0) ipcListeners.delete(channel); }} return this;
  }},
  off(channel, listener) {{ return this.removeListener(channel, listener); }},
  removeAllListeners(channel) {{
    if (channel === undefined) ipcListeners.clear();
    else if (validBrowserChannel(channel)) ipcListeners.delete(channel);
    return this;
  }}
}});
const contextBridge = Object.freeze({{
  exposeInMainWorld(key, value) {{
    if (typeof key !== "string" || !/^[A-Za-z_$][A-Za-z0-9_$]{{0,63}}$/.test(key) || ["utools", "rubick", "require"].includes(key)) throw new TypeError("iHub rejected an unsafe contextBridge key.");
    if (Object.prototype.hasOwnProperty.call(window, key)) throw new Error("The requested contextBridge key already exists.");
    Object.defineProperty(window, key, {{ configurable: false, enumerable: true, writable: false, value }});
  }}
}});
if (!("require" in window)) {{
  Object.defineProperty(window, "require", {{
    configurable: false,
    writable: false,
    value(name) {{
      if (name === "electron") return Object.freeze({{ contextBridge, ipcRenderer }});
      throw new Error("iHub's sandboxed BrowserWindow preload exposes only electron.ipcRenderer.");
    }}
  }});
}}
function browserWindowProxy(identityPromise, callback) {{
  let browserId = null;
  const invoke = (action, args, expectsResult) => {{
    const operation = identityPromise.then((id) => call("compatibility.utools.browser.control", {{ browserId: id, action, args }}));
    if (expectsResult) return operation;
    void operation.catch((error) => console.error("iHub BrowserWindow " + action + " failed", error));
    return undefined;
  }};
  const proxy = {{
    show() {{ return invoke("show", [], false); }},
    hide() {{ return invoke("hide", [], false); }},
    close() {{ return invoke("close", [], false); }},
    destroy() {{ return invoke("destroy", [], false); }},
    focus() {{ return invoke("focus", [], false); }},
    center() {{ return invoke("center", [], false); }},
    maximize() {{ return invoke("maximize", [], false); }},
    unmaximize() {{ return invoke("unmaximize", [], false); }},
    minimize() {{ return invoke("minimize", [], false); }},
    restore() {{ return invoke("restore", [], false); }},
    setAlwaysOnTop(value) {{ return invoke("setAlwaysOnTop", [value], false); }},
    setFullScreen(value) {{ return invoke("setFullScreen", [value], false); }},
    setResizable(value) {{ return invoke("setResizable", [value], false); }},
    setMaximizable(value) {{ return invoke("setMaximizable", [value], false); }},
    setMinimizable(value) {{ return invoke("setMinimizable", [value], false); }},
    setClosable(value) {{ return invoke("setClosable", [value], false); }},
    setDecorations(value) {{ return invoke("setDecorations", [value], false); }},
    setFocusable(value) {{ return invoke("setFocusable", [value], false); }},
    setHasShadow(value) {{ return invoke("setShadow", [value], false); }},
    setVisibleOnAllWorkspaces(value) {{ return invoke("setVisibleOnAllWorkspaces", [value], false); }},
    setContentProtection(value) {{ return invoke("setContentProtection", [value], false); }},
    setIgnoreMouseEvents(value) {{ return invoke("setIgnoreMouseEvents", [value], false); }},
    setSkipTaskbar(value) {{ return invoke("setSkipTaskbar", [value], false); }},
    setTitle(value) {{ return invoke("setTitle", [value], false); }},
    setSize(width, height) {{ return invoke("setSize", [width, height], false); }},
    setPosition(x, y) {{ return invoke("setPosition", [x, y], false); }},
    isVisible() {{ return invoke("isVisible", [], true); }},
    isFocused() {{ return invoke("isFocused", [], true); }},
    isMaximized() {{ return invoke("isMaximized", [], true); }},
    isMinimized() {{ return invoke("isMinimized", [], true); }},
    isFullScreen() {{ return invoke("isFullScreen", [], true); }},
    isResizable() {{ return invoke("isResizable", [], true); }},
    isAlwaysOnTop() {{ return invoke("isAlwaysOnTop", [], true); }},
    getTitle() {{ return invoke("getTitle", [], true); }},
    getSize() {{ return invoke("getSize", [], true); }},
    getPosition() {{ return invoke("getPosition", [], true); }},
    webContents: Object.freeze({{
      send(channel, ...args) {{
        if (!validBrowserChannel(channel)) throw new TypeError("uTools BrowserWindow IPC channel is invalid.");
        const cloned = boundedBrowserArgs(args);
        void identityPromise.then((id) => call("compatibility.utools.browser.send", {{ browserId: id, channel, args: cloned }}))
          .catch((error) => console.error("iHub BrowserWindow send failed", error));
      }},
      executeJavaScript(script) {{
        if (typeof script !== "string" || script.length === 0 || Array.from(script).length > 65536) return Promise.reject(new TypeError("uTools BrowserWindow script is invalid."));
        return identityPromise.then((id) => call("compatibility.utools.browser.executeJavaScript", {{ browserId: id, script }}));
      }},
      reload() {{ return invoke("reload", [], false); }}
    }})
  }};
  const frozen = Object.freeze(proxy);
  void identityPromise.then((id) => {{
    browserId = id;
    const record = {{ proxy: frozen, callback: typeof callback === "function" ? callback : null, called: false }};
    browserWindows.set(id, record);
    if (browserReady.has(id) && record.callback) {{ record.called = true; record.callback(frozen); }}
  }}).catch((error) => console.error("iHub BrowserWindow creation failed", error));
  return frozen;
}}
function pngDataUrlForCopyImage(value) {{
  if (typeof value === "string") {{
    return value.startsWith("data:image/png;base64,iVBORw0KGgo") && value.length <= copyImageMaxDataUrlChars
      ? value
      : null;
  }}
  if (!(value instanceof Uint8Array) || value.byteLength === 0 || value.byteLength > copyImageMaxPngBytes) return null;
  const signature = [137, 80, 78, 71, 13, 10, 26, 10];
  if (signature.some((byte, index) => value[index] !== byte)) return null;
  let binary = "";
  for (let offset = 0; offset < value.byteLength; offset += 32768) {{
    binary += String.fromCharCode(...value.subarray(offset, Math.min(value.byteLength, offset + 32768)));
  }}
  return "data:image/png;base64," + btoa(binary);
}}
function normalizedCopyImagePayload(value) {{
  const dataUrl = pngDataUrlForCopyImage(value);
  if (dataUrl) return {{ dataUrl }};
  if (typeof value === "string" && value.startsWith("data:")) return null;
  if (typeof value !== "string" || value.length === 0 || Array.from(value).length > 1024 || new TextEncoder().encode(value).byteLength > 8192 || /[\u0000-\u001f\u007f]/.test(value)) return null;
  return {{ path: value }};
}}
function attachmentBase64(value) {{
  if (!(value instanceof Uint8Array) || value.byteLength === 0 || value.byteLength > attachmentMaxBytes) return null;
  let binary = "";
  for (let offset = 0; offset < value.byteLength; offset += 32768) {{
    binary += String.fromCharCode(...value.subarray(offset, Math.min(value.byteLength, offset + 32768)));
  }}
  return btoa(binary);
}}
function attachmentBytes(value) {{
  if (typeof value !== "string") return null;
  const binary = atob(value);
  if (binary.length === 0 || binary.length > attachmentMaxBytes) throw new RangeError("uTools attachment response exceeds 10 MiB.");
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index);
  return bytes;
}}
function syncDbCall(op, payload) {{
  const request = new XMLHttpRequest();
  request.open("POST", syncDbRoute, false);
  request.setRequestHeader("Content-Type", "application/json");
  request.setRequestHeader("X-IHub-Utools-DB", "1");
  request.send(JSON.stringify({{ op, ...payload }}));
  let result;
  try {{ result = JSON.parse(request.responseText); }}
  catch {{ throw new Error("iHub returned an invalid synchronous database response."); }}
  if (request.status !== 200) throw new Error(result && typeof result.error === "string" ? result.error : "iHub synchronous database request failed.");
  return result;
}}
function syncScreenSnapshot() {{
  const request = new XMLHttpRequest();
  request.open("GET", syncScreenRoute, false);
  request.send();
  let result;
  try {{ result = JSON.parse(request.responseText); }}
  catch {{ throw new Error("iHub returned an invalid synchronous screen response."); }}
  if (request.status !== 200) throw new Error(result && typeof result.error === "string" ? result.error : "iHub synchronous screen request failed.");
  if (!result || !Array.isArray(result.displays) || !Array.isArray(result.metrics)) throw new Error("iHub returned an invalid screen snapshot.");
  return result;
}}
function syncFileIcon(path) {{
  const request = new XMLHttpRequest();
  request.open("POST", syncIconRoute, false);
  request.setRequestHeader("Content-Type", "application/json");
  request.setRequestHeader("X-IHub-Utools-Icon", "1");
  request.send(JSON.stringify({{ path }}));
  let result;
  try {{ result = JSON.parse(request.responseText); }}
  catch {{ throw new Error("iHub returned an invalid synchronous icon response."); }}
  if (request.status !== 200) throw new Error(result && typeof result.error === "string" ? result.error : "iHub synchronous icon request failed.");
  return typeof result === "string" ? result : "";
}}
function syncCopiedFiles() {{
  const request = new XMLHttpRequest();
  request.open("GET", syncClipboardRoute, false);
  request.setRequestHeader("X-IHub-Utools-Clipboard", "1");
  request.send();
  let result;
  try {{ result = JSON.parse(request.responseText); }}
  catch {{ throw new Error("iHub returned an invalid synchronous clipboard response."); }}
  if (request.status !== 200) throw new Error(result && typeof result.error === "string" ? result.error : "iHub synchronous clipboard request failed.");
  if (!Array.isArray(result) || result.length > 32 || result.some((item) => !item || typeof item !== "object" || Array.isArray(item) || Object.keys(item).some((key) => !["path", "isDiractory", "isFile", "name"].includes(key)) || typeof item.path !== "string" || typeof item.name !== "string" || typeof item.isDiractory !== "boolean" || typeof item.isFile !== "boolean" || item.isDiractory === item.isFile)) throw new Error("iHub returned an invalid copied-file list.");
  return result.map((item) => Object.freeze({{ ...item }}));
}}
function normalizedDialogOptions(kind, value) {{
  if (value === undefined) return {{}};
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new TypeError("uTools dialog options must be an object.");
  const openKeys = new Set(["title", "defaultPath", "buttonLabel", "filters", "properties", "message", "securityScopedBookmarks"]);
  const saveKeys = new Set(["title", "defaultPath", "buttonLabel", "filters", "message", "nameFieldLabel", "showsTagField", "properties", "securityScopedBookmarks"]);
  const allowed = kind === "open" ? openKeys : saveKeys;
  if (Object.keys(value).some((key) => !allowed.has(key))) throw new TypeError("uTools dialog options contain an unsupported field.");
  const result = {{}};
  for (const key of ["title", "defaultPath", "buttonLabel", "message", "nameFieldLabel", "showsTagField"]) {{
    if (value[key] === undefined) continue;
    if (typeof value[key] !== "string" || value[key].length === 0 || Array.from(value[key]).length > (key === "defaultPath" ? 1024 : 240) || /[\u0000-\u001f\u007f]/.test(value[key])) throw new TypeError("uTools dialog " + key + " is invalid.");
    result[key] = value[key];
  }}
  if (value.securityScopedBookmarks !== undefined) {{
    if (typeof value.securityScopedBookmarks !== "boolean") throw new TypeError("uTools dialog securityScopedBookmarks must be boolean.");
    result.securityScopedBookmarks = value.securityScopedBookmarks;
  }}
  if (value.filters !== undefined) {{
    if (!Array.isArray(value.filters) || value.filters.length > 16) throw new TypeError("uTools dialog filters must be a bounded array.");
    result.filters = value.filters.map((filter) => {{
      if (!filter || typeof filter !== "object" || Array.isArray(filter) || Object.keys(filter).some((key) => key !== "name" && key !== "extensions") || typeof filter.name !== "string" || filter.name.length === 0 || Array.from(filter.name).length > 80 || /[\u0000-\u001f\u007f]/.test(filter.name) || !Array.isArray(filter.extensions) || filter.extensions.length === 0 || filter.extensions.length > 16 || filter.extensions.some((extension) => typeof extension !== "string" || !/^(?:\*|[A-Za-z0-9][A-Za-z0-9+_-]{{0,15}})$/.test(extension))) throw new TypeError("uTools dialog filter is invalid.");
      return {{ name: filter.name, extensions: Array.from(new Set(filter.extensions)) }};
    }});
  }}
  if (value.properties !== undefined) {{
    if (!Array.isArray(value.properties) || value.properties.length > 12 || value.properties.some((property) => typeof property !== "string" || Array.from(property).length > 40)) throw new TypeError("uTools dialog properties must be a bounded string array.");
    result.properties = Array.from(new Set(value.properties));
  }}
  return result;
}}
function syncDialog(kind, options) {{
  const request = new XMLHttpRequest();
  request.open("POST", syncDialogRoute, false);
  request.setRequestHeader("Content-Type", "application/json");
  request.setRequestHeader("X-IHub-Utools-Dialog", "1");
  request.send(JSON.stringify({{ kind, options: normalizedDialogOptions(kind, options) }}));
  let result;
  try {{ result = JSON.parse(request.responseText); }}
  catch {{ throw new Error("iHub returned an invalid synchronous dialog response."); }}
  if (request.status !== 200) throw new Error(result && typeof result.error === "string" ? result.error : "iHub synchronous dialog request failed.");
  return result === null ? undefined : result;
}}
function normalizedRedirect(label, value) {{
  const normalizeLabel = (candidate) => {{
    if (typeof candidate !== "string") throw new TypeError("uTools redirect labels must be strings.");
    const result = candidate.trim();
    if (!result || Array.from(result).length > 160 || new TextEncoder().encode(result).byteLength > 1024 || /[\u0000-\u001f\u007f]/.test(result)) throw new TypeError("uTools redirect label is invalid.");
    return result;
  }};
  const normalizedLabel = typeof label === "string"
    ? normalizeLabel(label)
    : Array.isArray(label) && label.length === 2
      ? [normalizeLabel(label[0]), normalizeLabel(label[1])]
      : null;
  if (!normalizedLabel) throw new TypeError("uTools redirect label must be a string or two-string array.");
  let action;
  if (value === undefined) action = {{ type: "text", payload: "" }};
  else if (typeof value === "string") action = {{ type: "text", payload: value }};
  else {{
    if (!value || typeof value !== "object" || Array.isArray(value) || Object.keys(value).some((key) => key !== "type" && key !== "data") || typeof value.type !== "string" || !("data" in value)) throw new TypeError("uTools redirect payload is invalid.");
    if (value.type === "text") {{
      if (typeof value.data !== "string") throw new TypeError("uTools redirect text data must be a string.");
      action = {{ type: "text", payload: value.data }};
    }} else if (value.type === "img") {{
      const dataUrl = pngDataUrlForCopyImage(value.data);
      if (!dataUrl) throw new TypeError("uTools redirect image data must be a bounded PNG.");
      action = {{ type: "img", payload: dataUrl }};
    }} else if (value.type === "files") {{
      const paths = typeof value.data === "string" ? [value.data] : value.data;
      if (!Array.isArray(paths) || paths.length === 0 || paths.length > 16) throw new TypeError("uTools redirect files data must contain 1-16 paths.");
      const encoder = new TextEncoder();
      let totalBytes = 0;
      const normalized = [];
      for (const path of paths) {{
        if (typeof path !== "string" || path.length === 0 || Array.from(path).length > 1024 || /[\u0000-\u001f\u007f]/.test(path)) throw new TypeError("uTools redirect file path is invalid.");
        totalBytes += encoder.encode(path).byteLength;
        if (totalBytes > 8192 || normalized.includes(path)) throw new TypeError("uTools redirect file paths are too large or duplicated.");
        normalized.push(path);
      }}
      action = {{ type: "files", payload: normalized }};
    }} else throw new TypeError("uTools redirect payload type must be text, img, or files.");
  }}
  if (action.type === "text" && (new TextEncoder().encode(action.payload).byteLength > 49152 || action.payload.includes("\u0000"))) throw new TypeError("uTools redirect text is too large or contains NUL.");
  return {{ label: normalizedLabel, action }};
}}
function normalizedSimulationPoint(x, y, optional) {{
  if (x === undefined && y === undefined && optional) return {{}};
  if (!Number.isSafeInteger(x) || !Number.isSafeInteger(y) || x < -2147483648 || x > 2147483647 || y < -2147483648 || y > 2147483647) return null;
  return {{ x, y }};
}}
function screenPoint(value, label) {{
  if (!value || typeof value !== "object" || Array.isArray(value) || !Number.isFinite(value.x) || !Number.isFinite(value.y)) throw new TypeError(label + " requires finite x and y coordinates.");
  return {{ x: Math.round(value.x), y: Math.round(value.y) }};
}}
function screenRect(value, label) {{
  const point = screenPoint(value, label);
  if (!Number.isFinite(value.width) || !Number.isFinite(value.height) || value.width < 0 || value.height < 0) throw new TypeError(label + " requires non-negative finite width and height.");
  return {{ ...point, width: Math.round(value.width), height: Math.round(value.height) }};
}}
function publicDisplay(value) {{ return JSON.parse(JSON.stringify(value)); }}
function rectDistanceSquared(point, rect) {{
  const dx = point.x < rect.x ? rect.x - point.x : point.x >= rect.x + rect.width ? point.x - (rect.x + rect.width) : 0;
  const dy = point.y < rect.y ? rect.y - point.y : point.y >= rect.y + rect.height ? point.y - (rect.y + rect.height) : 0;
  return dx * dx + dy * dy;
}}
function nearestMetric(snapshot, point, field) {{
  let selected = snapshot.metrics[0];
  let selectedDistance = Number.POSITIVE_INFINITY;
  for (const metric of snapshot.metrics) {{
    const rect = metric && metric[field];
    if (!rect) continue;
    const distance = rectDistanceSquared(point, rect);
    if (distance < selectedDistance) {{ selected = metric; selectedDistance = distance; }}
  }}
  if (!selected) throw new Error("No active display is available.");
  return selected;
}}
function displayForMetric(snapshot, metric) {{
  const display = snapshot.displays.find((candidate) => candidate && candidate.id === metric.id);
  if (!display) throw new Error("The display snapshot is inconsistent.");
  return publicDisplay(display);
}}
function displayMatchingRect(snapshot, rect) {{
  let selected = null;
  let selectedArea = -1;
  for (const display of snapshot.displays) {{
    const bounds = display && display.bounds;
    if (!bounds) continue;
    const width = Math.max(0, Math.min(rect.x + rect.width, bounds.x + bounds.width) - Math.max(rect.x, bounds.x));
    const height = Math.max(0, Math.min(rect.y + rect.height, bounds.y + bounds.height) - Math.max(rect.y, bounds.y));
    const area = width * height;
    if (area > selectedArea) {{ selected = display; selectedArea = area; }}
  }}
  if (selectedArea <= 0) {{
    const center = {{ x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 }};
    return displayForMetric(snapshot, nearestMetric(snapshot, center, "dipBounds"));
  }}
  return publicDisplay(selected);
}}
function desktopCaptureOptions(value) {{
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new TypeError("desktopCaptureSources requires an options object.");
  const allowed = new Set(["types", "thumbnailSize", "fetchWindowIcons"]);
  if (Object.keys(value).some((key) => !allowed.has(key))) throw new TypeError("desktopCaptureSources options contain an unsupported field.");
  if (!Array.isArray(value.types) || value.types.length === 0 || value.types.length > 2 || value.types.some((type) => type !== "screen" && type !== "window")) throw new TypeError("desktopCaptureSources types must contain screen or window.");
  const types = Array.from(new Set(value.types));
  if (value.fetchWindowIcons !== undefined && typeof value.fetchWindowIcons !== "boolean") throw new TypeError("desktopCaptureSources fetchWindowIcons must be boolean.");
  const rawSize = value.thumbnailSize === undefined ? {{ width: 150, height: 150 }} : value.thumbnailSize;
  if (!rawSize || typeof rawSize !== "object" || Array.isArray(rawSize) || Object.keys(rawSize).some((key) => key !== "width" && key !== "height") || !Number.isInteger(rawSize.width) || !Number.isInteger(rawSize.height) || rawSize.width < 0 || rawSize.height < 0 || rawSize.width > 512 || rawSize.height > 512) throw new TypeError("desktopCaptureSources thumbnailSize must be 0-512 integer pixels.");
  return {{ types, thumbnailSize: {{ width: rawSize.width, height: rawSize.height }} }};
}}
function nativeImageBytes(dataUrl) {{
  if (!dataUrl) return new Uint8Array();
  const binary = atob(dataUrl.slice(dataUrl.indexOf(",") + 1));
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index);
  return bytes;
}}
function nativeImageCompat(dataUrl, width, height) {{
  return Object.freeze({{
    isEmpty() {{ return !dataUrl; }},
    getSize() {{ return {{ width, height }}; }},
    getAspectRatio() {{ return height > 0 ? width / height : 1; }},
    toDataURL() {{ return dataUrl; }},
    toPNG() {{ return nativeImageBytes(dataUrl); }}
  }});
}}
async function desktopCaptureThumbnail(stream, size) {{
  if (size.width === 0 || size.height === 0) return nativeImageCompat("", 0, 0);
  const video = document.createElement("video");
  video.muted = true;
  video.playsInline = true;
  video.srcObject = stream;
  try {{
    await new Promise((resolve, reject) => {{
      const timeout = window.setTimeout(() => reject(new Error("Screen source thumbnail timed out.")), 3000);
      video.onloadedmetadata = () => {{ window.clearTimeout(timeout); resolve(); }};
      video.onerror = () => {{ window.clearTimeout(timeout); reject(new Error("Screen source thumbnail failed.")); }};
    }});
    await video.play();
    await new Promise((resolve) => {{
      if (typeof video.requestVideoFrameCallback === "function") video.requestVideoFrameCallback(() => resolve());
      else window.setTimeout(resolve, 80);
    }});
    const canvas = document.createElement("canvas");
    canvas.width = size.width;
    canvas.height = size.height;
    const context = canvas.getContext("2d", {{ alpha: false }});
    if (!context) throw new Error("Screen source thumbnail canvas is unavailable.");
    context.drawImage(video, 0, 0, size.width, size.height);
    const dataUrl = canvas.toDataURL("image/png");
    if (!dataUrl.startsWith("data:image/png;base64,")) throw new Error("Screen source thumbnail is invalid.");
    return nativeImageCompat(dataUrl, size.width, size.height);
  }} finally {{
    video.pause();
    video.srcObject = null;
  }}
}}
function stopDesktopCaptureSlot() {{
  const slot = desktopCaptureSlot;
  desktopCaptureSlot = null;
  if (!slot) return;
  window.clearTimeout(slot.timeout);
  for (const track of slot.stream.getTracks()) track.stop();
}}
const mediaDevices = navigator.mediaDevices;
const originalGetUserMedia = mediaDevices && typeof mediaDevices.getUserMedia === "function" ? mediaDevices.getUserMedia.bind(mediaDevices) : null;
let legacyDesktopCaptureBridgeAvailable = false;
if (mediaDevices && originalGetUserMedia) {{
  try {{
    Object.defineProperty(mediaDevices, "getUserMedia", {{
      configurable: true,
      value(constraints) {{
        const mandatory = constraints && constraints.video && typeof constraints.video === "object" && constraints.video.mandatory;
        const sourceId = mandatory && mandatory.chromeMediaSource === "desktop" && mandatory.chromeMediaSourceId;
        if (typeof sourceId === "string" && desktopCaptureSlot && desktopCaptureSlot.id === sourceId) {{
          const slot = desktopCaptureSlot;
          desktopCaptureSlot = null;
          window.clearTimeout(slot.timeout);
          if (!constraints.audio) {{
            for (const track of slot.stream.getAudioTracks()) {{ slot.stream.removeTrack(track); track.stop(); }}
          }}
          return Promise.resolve(slot.stream);
        }}
        return originalGetUserMedia(constraints);
      }}
    }});
    legacyDesktopCaptureBridgeAvailable = true;
  }} catch {{ legacyDesktopCaptureBridgeAvailable = false; }}
}}
function projectedDbRemoveTarget(target) {{
  if (typeof target === "string") return target;
  if (!target || typeof target !== "object" || Array.isArray(target)) throw new TypeError("uTools db.remove accepts a document ID or document object.");
  const projected = {{ _id: target._id }};
  if (target._rev !== undefined) projected._rev = target._rev;
  return projected;
}}
function invoke(callbacks, value) {{
  for (const callback of callbacks.slice()) {{ try {{ callback(value); }} catch (error) {{ console.error("uTools compatibility callback failed", error); }} }}
}}
function invokePluginOut(isKill) {{
  if (pluginOutDispatched) return;
  pluginOutDispatched = true;
  invoke(outCallbacks, Boolean(isKill));
}}
function invokePluginDetach() {{
  if (pluginDetachDispatched) return;
  pluginDetachDispatched = true;
  invoke(detachCallbacks);
}}
const dbStorageState = Object.create(null);
const dbStorageVersions = new Map();
const dbCryptoStorageState = Object.create(null);
const dbCryptoStorageVersions = new Map();
const dbPromises = Object.freeze({{
  get(id) {{ return call("compatibility.utools.db.get", {{ id }}); }},
  put(doc) {{ return call("compatibility.utools.db.put", {{ doc }}); }},
  remove(target) {{
    try {{ return call("compatibility.utools.db.remove", {{ target: projectedDbRemoveTarget(target) }}); }}
    catch (error) {{ return Promise.reject(error); }}
  }},
  bulkDocs(docs) {{ return call("compatibility.utools.db.bulkDocs", {{ docs }}); }},
  allDocs(selector) {{
    return selector === undefined
      ? call("compatibility.utools.db.allDocs", {{}})
      : call("compatibility.utools.db.allDocs", {{ selector }});
  }},
  postAttachment(id, attachment, contentType) {{
    const dataBase64 = attachmentBase64(attachment);
    if (typeof id !== "string" || !dataBase64 || typeof contentType !== "string" || !/^[A-Za-z0-9!#$&^_.+-]+\/[A-Za-z0-9!#$&^_.+-]+$/.test(contentType) || contentType.length > 255) {{
      return Promise.reject(new TypeError("uTools postAttachment accepts one bounded ID, Uint8Array, and MIME type."));
    }}
    return call("compatibility.utools.db.postAttachment", {{ id, dataBase64, contentType }});
  }},
  getAttachment(id) {{
    return call("compatibility.utools.db.getAttachment", {{ id }})
      .then((result) => result === null ? null : attachmentBytes(result && result.dataBase64));
  }},
  getAttachmentType(id) {{ return call("compatibility.utools.db.getAttachmentType", {{ id }}); }},
  replicateStateFromCloud() {{ return Promise.resolve(null); }}
}});
const db = Object.freeze({{
  promises: dbPromises,
  get(id) {{ return syncDbCall("get", {{ id }}); }},
  put(doc) {{ return syncDbCall("put", {{ doc }}); }},
  remove(target) {{ return syncDbCall("remove", {{ target: projectedDbRemoveTarget(target) }}); }},
  bulkDocs(docs) {{ return syncDbCall("bulkDocs", {{ docs }}); }},
  allDocs(selector) {{
    return selector === undefined
      ? syncDbCall("allDocs", {{}})
      : syncDbCall("allDocs", {{ selector }});
  }},
  postAttachment(id, attachment, contentType) {{
    const dataBase64 = attachmentBase64(attachment);
    if (typeof id !== "string" || !dataBase64 || typeof contentType !== "string" || !/^[A-Za-z0-9!#$&^_.+-]+\/[A-Za-z0-9!#$&^_.+-]+$/.test(contentType) || contentType.length > 255) {{
      throw new TypeError("uTools postAttachment accepts one bounded ID, Uint8Array, and MIME type.");
    }}
    return syncDbCall("postAttachment", {{ id, dataBase64, contentType }});
  }},
  getAttachment(id) {{
    const result = syncDbCall("getAttachment", {{ id }});
    return result === null ? null : attachmentBytes(result && result.dataBase64);
  }},
  getAttachmentType(id) {{ return syncDbCall("getAttachmentType", {{ id }}); }},
  replicateStateFromCloud() {{ return null; }}
}});
function dbStorageKey(key) {{
  if (typeof key !== "string") throw new TypeError("uTools dbStorage keys must be strings.");
  if (new TextEncoder().encode(key).byteLength > 48) throw new RangeError("uTools dbStorage keys must not exceed 48 UTF-8 bytes.");
  return key;
}}
function cloneDbStorageValue(value) {{
  const serialized = JSON.stringify(value);
  if (typeof serialized !== "string") throw new TypeError("uTools dbStorage values must be JSON-serializable.");
  if (new TextEncoder().encode(serialized).byteLength > 65536) throw new RangeError("uTools dbStorage values must not exceed 64 KiB.");
  return JSON.parse(serialized);
}}
function nextDbStorageVersion(key) {{
  const version = (dbStorageVersions.get(key) || 0) + 1;
  dbStorageVersions.set(key, version);
  return version;
}}
function normalizedUtoolsFilePaths(value) {{
  const paths = typeof value === "string" ? [value] : value;
  if (!Array.isArray(paths) || paths.length === 0 || paths.length > 16) return null;
  const encoder = new TextEncoder();
  let totalBytes = 0;
  const normalized = [];
  for (const path of paths) {{
    if (typeof path !== "string" || path.length === 0 || Array.from(path).length > 1024 || /[\u0000-\u001f\u007f]/.test(path)) return null;
    totalBytes += encoder.encode(path).byteLength;
    if (totalBytes > 8192 || normalized.includes(path)) return null;
    normalized.push(path);
  }}
  return normalized;
}}
function validUtoolsPaymentOptions(value) {{
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  if (Object.keys(value).some((key) => !["goodsId", "outOrderId", "attach"].includes(key))) return false;
  if (typeof value.goodsId !== "string" || value.goodsId.length === 0 || Array.from(value.goodsId).length > 160 || /[\u0000-\u001f\u007f]/.test(value.goodsId)) return false;
  if (value.outOrderId !== undefined && (typeof value.outOrderId !== "string" || Array.from(value.outOrderId).length < 6 || Array.from(value.outOrderId).length > 64 || /[\u0000-\u001f\u007f]/.test(value.outOrderId))) return false;
  if (value.attach !== undefined && (typeof value.attach !== "string" || Array.from(value.attach).length > 256 || value.attach.includes("\u0000"))) return false;
  return true;
}}
const dbStorage = Object.freeze({{
  setItem(rawKey, value) {{
    const key = dbStorageKey(rawKey);
    const storedValue = cloneDbStorageValue(value);
    const hadPrevious = Object.prototype.hasOwnProperty.call(dbStorageState, key);
    const previous = dbStorageState[key];
    const version = nextDbStorageVersion(key);
    dbStorageState[key] = storedValue;
    void call("compatibility.utools.dbStorage.set", {{ key, value: storedValue }}).catch((error) => {{
      if (dbStorageVersions.get(key) === version) {{
        if (hadPrevious) dbStorageState[key] = previous;
        else delete dbStorageState[key];
      }}
      console.error("iHub compatibility dbStorage write failed", error);
    }});
  }},
  getItem(rawKey) {{
    const key = dbStorageKey(rawKey);
    return Object.prototype.hasOwnProperty.call(dbStorageState, key) ? cloneDbStorageValue(dbStorageState[key]) : null;
  }},
  removeItem(rawKey) {{
    const key = dbStorageKey(rawKey);
    const hadPrevious = Object.prototype.hasOwnProperty.call(dbStorageState, key);
    const previous = dbStorageState[key];
    const version = nextDbStorageVersion(key);
    delete dbStorageState[key];
    void call("compatibility.utools.dbStorage.remove", {{ key }}).catch((error) => {{
      if (hadPrevious && dbStorageVersions.get(key) === version) dbStorageState[key] = previous;
      console.error("iHub compatibility dbStorage remove failed", error);
    }});
  }}
}});
function dbCryptoStorageKey(key) {{
  if (typeof key !== "string") throw new TypeError("uTools dbCryptoStorage keys must be strings.");
  if (new TextEncoder().encode(key).byteLength > 48) throw new RangeError("uTools dbCryptoStorage keys must not exceed 48 UTF-8 bytes.");
  return key;
}}
function cloneDbCryptoStorageValue(value) {{
  const serialized = JSON.stringify(value);
  if (typeof serialized !== "string") throw new TypeError("uTools dbCryptoStorage values must be JSON-serializable.");
  if (new TextEncoder().encode(serialized).byteLength > 65536) throw new RangeError("uTools dbCryptoStorage values must not exceed 64 KiB.");
  return JSON.parse(serialized);
}}
function nextDbCryptoStorageVersion(key) {{
  const version = (dbCryptoStorageVersions.get(key) || 0) + 1;
  dbCryptoStorageVersions.set(key, version);
  return version;
}}
const dbCryptoStorage = Object.freeze({{
  setItem(rawKey, value) {{
    const key = dbCryptoStorageKey(rawKey);
    const storedValue = cloneDbCryptoStorageValue(value);
    const hadPrevious = Object.prototype.hasOwnProperty.call(dbCryptoStorageState, key);
    const previous = dbCryptoStorageState[key];
    const version = nextDbCryptoStorageVersion(key);
    dbCryptoStorageState[key] = storedValue;
    void call("compatibility.utools.dbCryptoStorage.set", {{ key, value: storedValue }}).catch((error) => {{
      if (dbCryptoStorageVersions.get(key) === version) {{
        if (hadPrevious) dbCryptoStorageState[key] = previous;
        else delete dbCryptoStorageState[key];
      }}
      console.error("iHub compatibility dbCryptoStorage encrypted write failed", error);
    }});
  }},
  getItem(rawKey) {{
    const key = dbCryptoStorageKey(rawKey);
    return Object.prototype.hasOwnProperty.call(dbCryptoStorageState, key)
      ? cloneDbCryptoStorageValue(dbCryptoStorageState[key])
      : null;
  }},
  removeItem(rawKey) {{
    const key = dbCryptoStorageKey(rawKey);
    const hadPrevious = Object.prototype.hasOwnProperty.call(dbCryptoStorageState, key);
    const previous = dbCryptoStorageState[key];
    const version = nextDbCryptoStorageVersion(key);
    delete dbCryptoStorageState[key];
    void call("compatibility.utools.dbCryptoStorage.remove", {{ key }}).catch((error) => {{
      if (hadPrevious && dbCryptoStorageVersions.get(key) === version) dbCryptoStorageState[key] = previous;
      console.error("iHub compatibility dbCryptoStorage encrypted remove failed", error);
    }});
  }}
}});
const dynamicFeatures = new Map();
const dynamicFeatureVersions = new Map();
function dynamicFeatureCommandId(code) {{
  let hash = 0xcbf29ce484222325n;
  for (const byte of new TextEncoder().encode(code)) {{
    hash = BigInt.asUintN(64, (hash ^ BigInt(byte)) * 0x100000001b3n);
  }}
  return "utools-dynamic-" + hash.toString(16).padStart(16, "0");
}}
function normalizeDynamicFeature(value) {{
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const allowed = new Set(["code", "explain", "icon", "platform", "mainHide", "mainPush", "cmds"]);
  if (Object.keys(value).some((key) => !allowed.has(key))) return null;
  const code = typeof value.code === "string" ? value.code.trim() : "";
  if (!code || Array.from(code).length > 160 || /[\u0000-\u001f\u007f]/.test(code)) return null;
  if (!Array.isArray(value.cmds) || value.cmds.length < 1 || value.cmds.length > 16) return null;
  const cmds = [];
  for (const rawCommand of value.cmds) {{
    if (typeof rawCommand !== "string") return null;
    const command = rawCommand.trim();
    if (!command || Array.from(command).length > 80 || /[\u0000-\u001f\u007f]/.test(command)) return null;
    if (!cmds.includes(command)) cmds.push(command);
  }}
  const feature = {{ code, cmds }};
  if (value.explain !== undefined) {{
    if (typeof value.explain !== "string" || Array.from(value.explain).length > 240) return null;
    const explain = value.explain.trim();
    if (explain) feature.explain = explain;
  }}
  if (value.icon !== undefined) {{
    if (typeof value.icon !== "string" || Array.from(value.icon).length > 2048 || /[\u0000-\u001f\u007f]/.test(value.icon)) return null;
    feature.icon = value.icon;
  }}
  if (value.platform !== undefined) {{
    const platforms = typeof value.platform === "string" ? [value.platform] : value.platform;
    if (!Array.isArray(platforms) || platforms.length < 1 || platforms.length > 3 || platforms.some((platform) => !["win32", "darwin", "linux"].includes(platform))) return null;
    feature.platform = typeof value.platform === "string" ? value.platform : Array.from(new Set(platforms));
  }}
  for (const key of ["mainHide", "mainPush"]) {{
    if (value[key] !== undefined) {{
      if (typeof value[key] !== "boolean") return null;
      feature[key] = value[key];
    }}
  }}
  return Object.freeze({{ ...feature, commandId: dynamicFeatureCommandId(code) }});
}}
function publicDynamicFeature(feature) {{
  const {{ commandId, ...value }} = feature;
  return JSON.parse(JSON.stringify(value));
}}
function nextDynamicFeatureVersion(code) {{
  const version = (dynamicFeatureVersions.get(code) || 0) + 1;
  dynamicFeatureVersions.set(code, version);
  return version;
}}
function projectedRedirectAction(value) {{
  if (!value || typeof value !== "object" || Array.isArray(value) || Object.keys(value).some((key) => key !== "type" && key !== "payload")) return null;
  if (value.type === "text" && typeof value.payload === "string" && new TextEncoder().encode(value.payload).byteLength <= 49152 && !value.payload.includes("\u0000")) return {{ type: "text", payload: value.payload }};
  if (value.type === "img" && typeof value.payload === "string" && value.payload.startsWith("data:image/png;base64,iVBORw0KGgo") && value.payload.length <= copyImageMaxDataUrlChars) return {{ type: "img", payload: value.payload }};
  if (value.type === "files" && Array.isArray(value.payload) && value.payload.length > 0 && value.payload.length <= 16 && value.payload.every((path) => typeof path === "string" && path.length > 0 && Array.from(path).length <= 1024 && !/[\u0000-\u001f\u007f]/.test(path))) return {{ type: "files", payload: value.payload.slice() }};
  return null;
}}
function mainPushFeaturesForQuery(query) {{
  const normalized = typeof query === "string" ? query.trim().toLocaleLowerCase() : "";
  if (!normalized) return [];
  const matches = (keywords) => Array.isArray(keywords) && keywords.some((keyword) => {{
    const candidate = typeof keyword === "string" ? keyword.trim().toLocaleLowerCase() : "";
    return candidate && (candidate.includes(normalized) || normalized.includes(candidate));
  }});
  const actions = [];
  for (const command of config.commands) {{
    if (command && command.mainPush === true && matches(command.keywords)) actions.push({{ code: command.code, type: "text", payload: query }});
  }}
  for (const feature of dynamicFeatures.values()) {{
    if (feature && feature.mainPush === true && matches(feature.cmds)) actions.push({{ code: feature.code, type: "text", payload: query }});
  }}
  return actions.slice(0, 16);
}}
function clonedMainPushOption(value) {{
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  let serialized;
  try {{ serialized = JSON.stringify(value); }} catch {{ return null; }}
  if (typeof serialized !== "string" || new TextEncoder().encode(serialized).byteLength > 6144) return null;
  let option;
  try {{ option = JSON.parse(serialized); }} catch {{ return null; }}
  if (typeof option.text !== "string" || !option.text.trim() || Array.from(option.text).length > 320 || /[\u0000-\u001f\u007f]/.test(option.text)) return null;
  if (option.title !== undefined && (typeof option.title !== "string" || Array.from(option.title).length > 320 || /[\u0000-\u001f\u007f]/.test(option.title))) return null;
  if (option.icon !== undefined && (typeof option.icon !== "string" || Array.from(option.icon).length > 2048 || /[\u0000-\u001f\u007f]/.test(option.icon))) return null;
  return option;
}}
function ensureMainPushProviderRegistration() {{
  if (typeof mainPushCallback !== "function" || typeof mainPushSelectCallback !== "function" || mainPushProviderState !== "idle") return;
  const available = config.commands.some((command) => command && command.mainPush === true)
    || Array.from(dynamicFeatures.values()).some((feature) => feature && feature.mainPush === true);
  if (!available) return;
  mainPushProviderState = "pending";
  void call("search.register", {{ definition: {{ id: mainPushProviderId, title: "uTools 主搜索推送", priority: 20 }} }})
    .then(() => {{ mainPushProviderState = "registered"; }})
    .catch((error) => {{ mainPushProviderState = "idle"; console.error("iHub compatibility main-push registration failed", error); }});
}}
function completeMainPushSearch(message) {{
  const requestId = message.payload && message.payload.requestId;
  const query = message.payload && message.payload.query;
  const limit = message.payload && Number.isInteger(message.payload.limit) ? Math.max(1, Math.min(6, message.payload.limit)) : 3;
  if (typeof requestId !== "string" || typeof query !== "string" || typeof mainPushCallback !== "function") return;
  try {{
    const results = [];
    for (const [actionIndex, action] of mainPushFeaturesForQuery(query).entries()) {{
      const options = mainPushCallback(Object.freeze({{ ...action }}));
      if (!Array.isArray(options)) continue;
      for (const [optionIndex, candidate] of options.entries()) {{
        const option = clonedMainPushOption(candidate);
        if (!option) continue;
        results.push({{
          id: "main-push-" + actionIndex.toString(36) + "-" + optionIndex.toString(36),
          title: option.text,
          ...(option.title ? {{ subtitle: option.title }} : {{}}),
          score: 100 - actionIndex - optionIndex / 100,
          payload: {{ kind: "utoolsMainPush", action, option }}
        }});
        if (results.length >= limit) break;
      }}
      if (results.length >= limit) break;
    }}
    void call("search.complete", {{ requestId, ok: true, result: results, error: null }})
      .catch((error) => console.error("iHub compatibility main-push response failed", error));
  }} catch (error) {{
    void call("search.complete", {{ requestId, ok: false, result: [], error: error instanceof Error ? error.message : "uTools onMainPush callback failed." }})
      .catch(() => undefined);
  }}
}}
function selectMainPushOption(message) {{
  const payload = message.payload && message.payload.payload;
  const interactionId = message.payload && message.payload.interactionId;
  if (!payload || payload.kind !== "utoolsMainPush" || typeof interactionId !== "string" || typeof mainPushSelectCallback !== "function") return;
  const action = payload.action;
  const option = clonedMainPushOption(payload.option);
  if (!action || typeof action !== "object" || action.type !== "text" || typeof action.code !== "string" || typeof action.payload !== "string" || !option) return;
  const trackedCalls = [];
  activeMainPushInteractionId = interactionId;
  activeMainPushInteractionCalls = trackedCalls;
  let show = false;
  try {{ show = mainPushSelectCallback({{ code: action.code, type: "text", payload: action.payload, from: "main", option }}) === true; }}
  catch (error) {{ console.error("uTools compatibility main-push selection callback failed", error); }}
  finally {{ activeMainPushInteractionId = null; activeMainPushInteractionCalls = null; }}
  void Promise.allSettled(trackedCalls).then(() => call(
    "compatibility.utools.mainPush.selectComplete",
    {{ interactionId, show }},
    interactionId,
  )).catch((error) => console.error("iHub compatibility main-push completion failed", error));
}}
function boundedToolResult(value) {{
  if (value === undefined) return null;
  let encoded;
  try {{ encoded = JSON.stringify(value); }} catch {{ throw new TypeError("uTools MCP result must be JSON-serializable."); }}
  if (typeof encoded !== "string" || new TextEncoder().encode(encoded).byteLength > 1048576) throw new RangeError("uTools MCP result exceeds 1 MiB.");
  return JSON.parse(encoded);
}}
function invokeRegisteredTool(payload) {{
  const requestId = payload && payload.requestId;
  const name = payload && payload.name;
  const params = payload && payload.params;
  if (typeof requestId !== "string" || typeof name !== "string" || !declaredTools.has(name)) return;
  const handler = toolHandlers.get(name);
  if (typeof handler !== "function") return;
  const state = {{ cancelled: false }};
  activeToolCalls.set(requestId, state);
  const context = Object.freeze({{
    requestId,
    sendProgress(options) {{
      if (state.cancelled) return Promise.reject(new Error("uTools MCP call was cancelled."));
      if (!options || typeof options !== "object" || Array.isArray(options)) return Promise.reject(new TypeError("uTools MCP progress must be an object."));
      const progress = options.progress;
      const total = options.total;
      const message = options.message;
      if (typeof progress !== "number" || !Number.isFinite(progress) || progress < 0) return Promise.reject(new TypeError("uTools MCP progress must be finite and non-negative."));
      if (total !== undefined && (typeof total !== "number" || !Number.isFinite(total) || total <= 0 || total < progress)) return Promise.reject(new TypeError("uTools MCP progress total is invalid."));
      if (message !== undefined && (typeof message !== "string" || Array.from(message).length > 1000 || message.includes("\0"))) return Promise.reject(new TypeError("uTools MCP progress message is invalid."));
      return call("compatibility.utools.tools.progress", {{ requestId, name, progress, total: total ?? null, message: message ?? null }});
    }}
  }});
  void Promise.resolve()
    .then(() => handler(params, context))
    .then((value) => boundedToolResult(value))
    .then((result) => state.cancelled ? undefined : call("compatibility.utools.tools.complete", {{ requestId, name, ok: true, result, error: null }}, undefined, 125000))
    .catch((error) => state.cancelled ? undefined : call("compatibility.utools.tools.complete", {{
      requestId,
      name,
      ok: false,
      result: null,
      error: String(error instanceof Error ? error.message : error).slice(0, 2000)
    }}, undefined, 125000).catch(() => undefined))
    .finally(() => activeToolCalls.delete(requestId));
}}
function boundedAiFunctionResult(value) {{
  if (value === undefined) return null;
  let encoded;
  try {{ encoded = JSON.stringify(value); }} catch {{ throw new TypeError("uTools AI function result must be JSON-serializable."); }}
  if (typeof encoded !== "string" || new TextEncoder().encode(encoded).byteLength > 1048576) throw new RangeError("uTools AI function result exceeds 1 MiB.");
  return JSON.parse(encoded);
}}
function abortAiRequest(requestId, reason) {{
  const state = activeAiRequests.get(requestId);
  if (!state || state.settled) return;
  state.settled = true;
  activeAiRequests.delete(requestId);
  const error = new Error(reason || "uTools AI request was aborted.");
  error.name = "AbortError";
  state.reject(error);
  void state.started.then(() => call("compatibility.utools.ai.abort", {{ requestId }})).catch(() => undefined);
}}
function invokeAiFunction(payload) {{
  const requestId = payload && payload.requestId;
  const invocationId = payload && payload.invocationId;
  const name = payload && payload.name;
  const args = payload && payload.arguments;
  if (typeof requestId !== "string" || typeof invocationId !== "string" || typeof name !== "string" || !activeAiRequests.has(requestId)) return;
  const handler = globalThis[name];
  void Promise.resolve()
    .then(() => {{
      if (typeof handler !== "function") throw new Error("uTools AI function '" + name + "' is not attached to window.");
      return handler(args);
    }})
    .then((value) => boundedAiFunctionResult(value))
    .then((result) => call("compatibility.utools.ai.toolComplete", {{ requestId, invocationId, name, ok: true, result, error: null }}, undefined, 125000))
    .catch((error) => call("compatibility.utools.ai.toolComplete", {{
      requestId,
      invocationId,
      name,
      ok: false,
      result: null,
      error: String(error instanceof Error ? error.message : error).slice(0, 2000)
    }}, undefined, 125000).catch(() => undefined));
}}
function ai(option, streamCallback) {{
  if (!config.lifecycleOwner) throw new Error("A uTools BrowserWindow cannot start AI requests.");
  if (!option || typeof option !== "object" || Array.isArray(option)) throw new TypeError("utools.ai requires an options object.");
  if (streamCallback !== undefined && typeof streamCallback !== "function") throw new TypeError("utools.ai stream callback must be a function.");
  let encoded;
  try {{ encoded = JSON.stringify(option); }} catch {{ throw new TypeError("utools.ai options must be JSON-serializable."); }}
  if (typeof encoded !== "string" || new TextEncoder().encode(encoded).byteLength > 1048576) throw new RangeError("utools.ai options exceed 1 MiB.");
  const snapshot = JSON.parse(encoded);
  const requestId = crypto.randomUUID();
  let resolvePromise;
  let rejectPromise;
  const promise = new Promise((resolve, reject) => {{ resolvePromise = resolve; rejectPromise = reject; }});
  const state = {{ resolve: resolvePromise, reject: rejectPromise, streamCallback, settled: false, started: null }};
  activeAiRequests.set(requestId, state);
  state.started = call("compatibility.utools.ai.start", {{ requestId, option: snapshot, stream: typeof streamCallback === "function" }}, undefined, 125000);
  void state.started.catch((error) => {{
    if (state.settled) return;
    state.settled = true;
    activeAiRequests.delete(requestId);
    rejectPromise(error);
  }});
  Object.defineProperty(promise, "abort", {{
    configurable: false,
    enumerable: false,
    writable: false,
    value: () => abortAiRequest(requestId)
  }});
  return promise;
}}
function runFFmpeg(args, onProgress) {{
  if (!config.lifecycleOwner) throw new Error("A uTools BrowserWindow cannot start FFmpeg jobs.");
  if (!Array.isArray(args) || args.length === 0 || args.length > 256 || args.some((value) => typeof value !== "string" || value.length === 0 || new TextEncoder().encode(value).byteLength > 8192 || /[\u0000-\u001f\u007f]/.test(value))) {{
    throw new TypeError("utools.runFFmpeg requires 1-256 bounded string arguments.");
  }}
  if (onProgress !== undefined && typeof onProgress !== "function") throw new TypeError("utools.runFFmpeg progress callback must be a function.");
  const requestId = crypto.randomUUID();
  const snapshot = args.slice();
  let resolvePromise;
  let rejectPromise;
  const promise = new Promise((resolve, reject) => {{ resolvePromise = resolve; rejectPromise = reject; }});
  const state = {{ resolve: resolvePromise, reject: rejectPromise, onProgress, settled: false, started: null }};
  activeFfmpegRequests.set(requestId, state);
  state.started = call("compatibility.utools.ffmpeg.start", {{ requestId, args: snapshot }}, undefined, 125000);
  void state.started.catch((error) => {{
    if (state.settled) return;
    state.settled = true;
    activeFfmpegRequests.delete(requestId);
    rejectPromise(error);
  }});
  const control = (action) => {{
    if (state.settled) return false;
    void state.started.then(() => call("compatibility.utools.ffmpeg." + action, {{ requestId }})).catch(() => undefined);
    return true;
  }};
  Object.defineProperties(promise, {{
    kill: {{ configurable: false, enumerable: false, writable: false, value: () => control("kill") }},
    quit: {{ configurable: false, enumerable: false, writable: false, value: () => control("quit") }}
  }});
  return promise;
}}
window.addEventListener("message", (event) => {{
  if (event.source !== window.parent || !event.data || event.data.channel !== responseChannel) return;
  const message = event.data;
  if (message.type === "response") {{
    const request = pending.get(message.id);
    if (!request) return;
    pending.delete(message.id); window.clearTimeout(request.timeout);
    message.ok ? request.resolve(message.result) : request.reject(new Error(typeof message.error === "string" ? message.error : "iHub host request failed."));
    return;
  }}
  if (message.type !== "event") return;
  if (message.name === "ihub://plugin/" + config.pluginId + "/event/utools.tool.invoke") {{
    invokeRegisteredTool(message.payload);
    return;
  }}
  if (message.name === "ihub://plugin/" + config.pluginId + "/event/utools.tool.cancel") {{
    const requestId = message.payload && message.payload.requestId;
    const state = typeof requestId === "string" ? activeToolCalls.get(requestId) : null;
    if (state) state.cancelled = true;
    return;
  }}
  if (message.name === "ihub://plugin/" + config.pluginId + "/event/utools.ai.chunk") {{
    const payload = message.payload;
    const state = payload && typeof payload.requestId === "string" ? activeAiRequests.get(payload.requestId) : null;
    if (!state || state.settled || typeof state.streamCallback !== "function" || !payload.message || typeof payload.message !== "object") return;
    try {{ state.streamCallback(Object.freeze({{ ...payload.message }})); }}
    catch (error) {{ abortAiRequest(payload.requestId, error instanceof Error ? error.message : "uTools AI stream callback failed."); }}
    return;
  }}
  if (message.name === "ihub://plugin/" + config.pluginId + "/event/utools.ai.tool.invoke") {{
    invokeAiFunction(message.payload);
    return;
  }}
  if (message.name === "ihub://plugin/" + config.pluginId + "/event/utools.ai.complete") {{
    const payload = message.payload;
    const state = payload && typeof payload.requestId === "string" ? activeAiRequests.get(payload.requestId) : null;
    if (!state || state.settled) return;
    state.settled = true;
    activeAiRequests.delete(payload.requestId);
    if (payload.ok === true) state.resolve(typeof state.streamCallback === "function" ? undefined : Object.freeze({{ ...(payload.result || {{}}) }}));
    else state.reject(new Error(typeof payload.error === "string" ? payload.error : "uTools AI request failed."));
    return;
  }}
  if (message.name === "ihub://plugin/" + config.pluginId + "/event/utools.ffmpeg.progress") {{
    const payload = message.payload;
    const state = payload && typeof payload.requestId === "string" ? activeFfmpegRequests.get(payload.requestId) : null;
    if (!state || state.settled || typeof state.onProgress !== "function" || !payload.progress || typeof payload.progress !== "object") return;
    try {{ state.onProgress(Object.freeze({{ ...payload.progress }})); }}
    catch (error) {{ console.error("uTools FFmpeg progress callback failed", error); }}
    return;
  }}
  if (message.name === "ihub://plugin/" + config.pluginId + "/event/utools.ffmpeg.complete") {{
    const payload = message.payload;
    const state = payload && typeof payload.requestId === "string" ? activeFfmpegRequests.get(payload.requestId) : null;
    if (!state || state.settled) return;
    state.settled = true;
    activeFfmpegRequests.delete(payload.requestId);
    if (payload.ok === true) state.resolve();
    else state.reject(new Error(typeof payload.error === "string" ? payload.error : "uTools FFmpeg job failed."));
    return;
  }}
  if (message.name === "ihub://plugin/" + config.pluginId + "/event/utools.browser.ipc") {{
    const payload = message.payload;
    if (payload && validBrowserChannel(payload.channel) && Array.isArray(payload.args)) dispatchBrowserIpc(payload.channel, payload.args);
    return;
  }}
  if (message.name === "ihub://plugin/" + config.pluginId + "/event/utools.browser.ready") {{
    const browserId = message.payload && message.payload.browserId;
    if (typeof browserId !== "string") return;
    browserReady.add(browserId);
    const record = browserWindows.get(browserId);
    if (record && !record.called && record.callback) {{ record.called = true; record.callback(record.proxy); }}
    return;
  }}
  if (message.name === "ihub://plugin/" + config.pluginId + "/event/utools.browser.execute") {{
    const payload = message.payload;
    if (config.windowType !== "browser" || !payload || typeof payload.requestId !== "string" || typeof payload.script !== "string" || payload.script.length === 0 || Array.from(payload.script).length > 65536) return;
    void Promise.resolve()
      .then(() => (0, eval)(payload.script))
      .then((value) => Promise.resolve(value))
      .then((value) => {{
        let result = null;
        if (value !== undefined) {{
          const encoded = JSON.stringify(value);
          if (typeof encoded !== "string" || new TextEncoder().encode(encoded).byteLength > 262144) throw new RangeError("BrowserWindow script result exceeds 256 KiB.");
          result = JSON.parse(encoded);
        }}
        return call("compatibility.utools.browser.executeResult", {{ requestId: payload.requestId, ok: true, result, error: null }});
      }})
      .catch((error) => call("compatibility.utools.browser.executeResult", {{
        requestId: payload.requestId,
        ok: false,
        result: null,
        error: String(error instanceof Error ? error.message : error).slice(0, 2000)
      }}).catch(() => undefined));
    return;
  }}
  if (message.name === "ihub://plugin/" + config.pluginId + "/event/subInput.change") {{
    if (typeof subInputChangeCallback === "function") {{
      const text = message.payload && typeof message.payload.text === "string" ? message.payload.text : "";
      try {{ subInputChangeCallback({{ text }}); }} catch (error) {{ console.error("uTools compatibility sub-input callback failed", error); }}
    }}
    return;
  }}
  if (message.name === "ihub://plugin/" + config.pluginId + "/event/utools.windowType") {{
    const value = message.payload && message.payload.windowType;
    if (value === "main" || value === "detach" || value === "browser") {{
      currentWindowType = value;
      if (value === "detach") invokePluginDetach();
    }}
    return;
  }}
  if (message.name === "ihub://plugin/" + config.pluginId + "/search") {{
    if (message.payload && message.payload.providerId === mainPushProviderId) completeMainPushSearch(message);
    return;
  }}
  if (message.name === "ihub://plugin/" + config.pluginId + "/event/search.select") {{
    if (message.payload && message.payload.providerId === mainPushProviderId) selectMainPushOption(message);
    return;
  }}
  if (message.name === "ihub://plugin/" + config.pluginId + "/event/utools.dbPull") {{
    const docs = message.payload && Array.isArray(message.payload.docs) ? message.payload.docs : [];
    invoke(dbPullCallbacks, docs);
    return;
  }}
  if (message.name !== "ihub://plugin/" + config.pluginId + "/command") return;
  const commandId = message.payload && message.payload.commandId;
  const command = config.commands.find((candidate) => candidate.commandId === commandId)
    || Array.from(dynamicFeatures.values()).find((candidate) => candidate.commandId === commandId);
  if (!command) return;
  const mainPushAction = message.payload && message.payload.utoolsMainPushAction;
  if (mainPushAction && typeof mainPushAction === "object" && mainPushAction.code === command.code && mainPushAction.type === "text" && typeof mainPushAction.payload === "string") {{
    invoke(enterCallbacks, mainPushAction);
    return;
  }}
  const redirectAction = projectedRedirectAction(message.payload && message.payload.utoolsAction);
  if (redirectAction) {{
    invoke(enterCallbacks, {{ code: command.code, type: redirectAction.type, payload: redirectAction.payload, from: "redirect" }});
    return;
  }}
  const input = message.payload && message.payload.input;
  invoke(enterCallbacks, {{ code: command.code, type: "text", payload: typeof input === "string" ? input : "", from: "main" }});
}});
let idleUBrowsers = Array.isArray(config.idleUbrowsers)
  ? config.idleUbrowsers.filter((value) => value && typeof value === "object" && typeof value.id === "string").map((value) => Object.freeze({{ ...value }}))
  : [];
function ubrowserJsonValue(value, depth) {{
  if (depth > 12) throw new RangeError("uTools ubrowser arguments are nested too deeply.");
  if (value === null || typeof value === "string" || typeof value === "boolean") return value;
  if (typeof value === "number") {{ if (!Number.isFinite(value)) throw new TypeError("uTools ubrowser numbers must be finite."); return value; }}
  if (value instanceof Uint8Array) {{
    if (value.byteLength === 0 || value.byteLength > 2 * 1024 * 1024) throw new RangeError("uTools ubrowser binary payload is empty or too large.");
    let binary = "";
    for (let offset = 0; offset < value.byteLength; offset += 32768) binary += String.fromCharCode(...value.subarray(offset, Math.min(value.byteLength, offset + 32768)));
    return {{ __ihubBytesBase64: btoa(binary) }};
  }}
  if (Array.isArray(value)) {{ if (value.length > 64) throw new RangeError("uTools ubrowser arrays are too large."); return value.map((item) => ubrowserJsonValue(item, depth + 1)); }}
  if (!value || typeof value !== "object" || Object.getPrototypeOf(value) !== Object.prototype) throw new TypeError("uTools ubrowser arguments must be JSON values.");
  const entries = Object.entries(value);
  if (entries.length > 64) throw new RangeError("uTools ubrowser objects are too large.");
  const result = {{}};
  for (const [key, item] of entries) {{
    if (!key || Array.from(key).length > 128 || /[\u0000-\u001f\u007f]/.test(key)) throw new TypeError("uTools ubrowser object key is invalid.");
    result[key] = ubrowserJsonValue(item, depth + 1);
  }}
  return result;
}}
function ubrowserFunction(value) {{
  if (typeof value !== "function") throw new TypeError("uTools ubrowser requires a page function.");
  const source = Function.prototype.toString.call(value);
  if (!source || Array.from(source).length > 65536 || new TextEncoder().encode(source).byteLength > 262144) throw new RangeError("uTools ubrowser page function is too large.");
  return {{ __ihubFunction: source }};
}}
const ubrowserSimpleMethods = [
  "useragent", "viewport", "hide", "show", "css", "press", "click", "mousedown", "mouseup", "dblclick", "hover",
  "file", "drop", "input", "value", "check", "focus", "scroll", "paste", "screenshot", "markdown", "pdf", "device",
  "end", "devTools", "cookies", "setCookies", "removeCookies", "clearCookies"
];
function createUBrowserChain(initialOperation, initialArgs) {{
  const steps = [];
  const chain = {{}};
  const push = (op, args) => {{
    if (steps.length >= 128) throw new RangeError("uTools ubrowser chains are limited to 128 steps.");
    if (!Array.isArray(args) || args.length > 8) throw new RangeError("uTools ubrowser step has too many arguments.");
    steps.push({{ op, args: args.map((value) => ubrowserJsonValue(value, 0)) }});
    return chain;
  }};
  chain.goto = (url, headers, timeout) => push("goto", headers === undefined && timeout === undefined ? [url] : [url, headers ?? null, timeout ?? null]);
  chain.evaluate = (func, params) => push("evaluate", [ubrowserFunction(func), params === undefined ? [] : ubrowserJsonValue(params, 0)]);
  chain.wait = (target, timeout, ...params) => typeof target === "function"
    ? push("wait", [ubrowserFunction(target), timeout ?? null, ...params])
    : push("wait", timeout === undefined ? [target] : [target, timeout]);
  chain.when = (target, ...params) => typeof target === "function"
    ? push("when", [ubrowserFunction(target), ...params])
    : push("when", [target]);
  chain.download = (target, savePath, ...params) => typeof target === "function"
    ? push("download", [ubrowserFunction(target), savePath ?? null, ...params])
    : push("download", savePath === undefined ? [target] : [target, savePath]);
  for (const method of ubrowserSimpleMethods) chain[method] = (...args) => push(method, args);
  chain.run = (instanceOrOptions, maybeOptions) => {{
    let instanceId = null;
    let options = {{}};
    if (typeof instanceOrOptions === "string") {{ instanceId = instanceOrOptions; options = maybeOptions === undefined ? {{}} : ubrowserJsonValue(maybeOptions, 0); }}
    else if (instanceOrOptions !== undefined && instanceOrOptions !== null) options = ubrowserJsonValue(instanceOrOptions, 0);
    if (!options || typeof options !== "object" || Array.isArray(options)) return Promise.reject(new TypeError("uTools ubrowser run options must be an object."));
    return call("compatibility.utools.ubrowser.run", {{ instanceId, steps, options }}, undefined, 125000).then((result) => {{
      if (!Array.isArray(result) || result.length === 0) throw new Error("iHub returned an invalid ubrowser result.");
      const instance = result[result.length - 1];
      if (!instance || typeof instance !== "object" || typeof instance.id !== "string") throw new Error("iHub returned an invalid ubrowser instance.");
      idleUBrowsers = idleUBrowsers.filter((candidate) => candidate.id !== instance.id);
      idleUBrowsers.push(Object.freeze({{ ...instance }}));
      return result;
    }});
  }};
  if (initialOperation) chain[initialOperation](...(initialArgs || []));
  return Object.freeze(chain);
}}
const ubrowser = {{}};
for (const method of ["goto", "evaluate", "wait", "when", "download", ...ubrowserSimpleMethods]) {{
  ubrowser[method] = (...args) => createUBrowserChain(method, args);
}}
Object.freeze(ubrowser);
function sharpBytesBase64(value) {{
  let bytes;
  if (value instanceof ArrayBuffer) bytes = new Uint8Array(value);
  else if (ArrayBuffer.isView(value)) bytes = new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  else throw new TypeError("utools.sharp byte input must be Uint8Array or ArrayBuffer.");
  if (bytes.byteLength === 0 || bytes.byteLength > 16 * 1024 * 1024) throw new RangeError("utools.sharp input must contain 1-16 MiB.");
  let binary = "";
  for (let offset = 0; offset < bytes.byteLength; offset += 32768) binary += String.fromCharCode(...bytes.subarray(offset, Math.min(bytes.byteLength, offset + 32768)));
  return btoa(binary);
}}
function sharpPlainValue(value, depth) {{
  if (depth > 12) throw new RangeError("utools.sharp options are nested too deeply.");
  if (value === null || typeof value === "string" || typeof value === "boolean") return value;
  if (typeof value === "number") {{ if (!Number.isFinite(value)) throw new TypeError("utools.sharp numbers must be finite."); return value; }}
  if (value instanceof ArrayBuffer || ArrayBuffer.isView(value)) return {{ dataBase64: sharpBytesBase64(value) }};
  if (Array.isArray(value)) {{ if (value.length > 64) throw new RangeError("utools.sharp arrays are too large."); return value.map((item) => sharpPlainValue(item, depth + 1)); }}
  if (!value || typeof value !== "object" || Object.getPrototypeOf(value) !== Object.prototype) throw new TypeError("utools.sharp options must contain plain JSON values.");
  const entries = Object.entries(value);
  if (entries.length > 64) throw new RangeError("utools.sharp objects are too large.");
  const result = {{}};
  for (const [key, item] of entries) {{
    if (!key || Array.from(key).length > 128 || /[\u0000-\u001f\u007f]/.test(key)) throw new TypeError("utools.sharp option key is invalid.");
    result[key] = sharpPlainValue(item, depth + 1);
  }}
  return result;
}}
function normalizedSharpInput(input, options) {{
  const normalizedOptions = options === undefined ? {{}} : sharpPlainValue(options, 0);
  if (!normalizedOptions || typeof normalizedOptions !== "object" || Array.isArray(normalizedOptions)) throw new TypeError("utools.sharp options must be an object.");
  if (typeof input === "string") {{
    if (!input || Array.from(input).length > 1024 || /[\u0000-\u001f\u007f]/.test(input)) throw new TypeError("utools.sharp path is invalid.");
    return {{ kind: "path", path: input }};
  }}
  if (input instanceof ArrayBuffer || ArrayBuffer.isView(input)) {{
    const dataBase64 = sharpBytesBase64(input);
    if (normalizedOptions.raw) {{
      const raw = normalizedOptions.raw;
      return {{ kind: "raw", dataBase64, width: raw.width, height: raw.height, channels: raw.channels }};
    }}
    return {{ kind: "bytes", dataBase64 }};
  }}
  const source = input === undefined || input === null ? normalizedOptions : sharpPlainValue(input, 0);
  if (source && typeof source === "object" && !Array.isArray(source) && source.create && typeof source.create === "object") {{
    const create = source.create;
    return {{ kind: "create", width: create.width, height: create.height, channels: create.channels, background: create.background ?? null }};
  }}
  throw new TypeError("utools.sharp supports picker paths, bounded bytes, raw pixels, or create input.");
}}
const sharpChainMethods = new Set([
  "resize", "rotate", "flip", "flop", "grayscale", "greyscale", "negate", "blur", "sharpen", "threshold",
  "normalize", "normalise", "gamma", "median", "tint", "flatten", "extend", "trim", "extract", "composite",
  "jpeg", "jpg", "png", "webp", "gif", "tiff"
]);
function sharpDataBytes(dataBase64) {{
  if (typeof dataBase64 !== "string" || dataBase64.length === 0) throw new Error("iHub returned invalid uTools Sharp bytes.");
  const binary = atob(dataBase64);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index);
  return bytes;
}}
function createSharp(input, options, inherited) {{
  if (!config.lifecycleOwner) throw new Error("A uTools BrowserWindow cannot start Sharp pipelines.");
  const state = inherited || {{ input: normalizedSharpInput(input, options), operations: [] }};
  const terminal = (output) => call("compatibility.utools.sharp.execute", {{
    input: sharpPlainValue(state.input, 0),
    operations: state.operations.map((operation) => ({{ method: operation.method, args: sharpPlainValue(operation.args, 0) }})),
    output
  }}, undefined, 125000);
  let proxy;
  const target = {{}};
  proxy = new Proxy(target, {{
    get(_target, property) {{
      if (property === "then") return undefined;
      if (property === "clone") return () => createSharp(undefined, undefined, {{ input: sharpPlainValue(state.input, 0), operations: state.operations.map((operation) => ({{ method: operation.method, args: sharpPlainValue(operation.args, 0) }})) }});
      if (property === "metadata") return () => terminal({{ kind: "metadata" }});
      if (property === "toFile") return (path) => {{
        if (typeof path !== "string" || !path || Array.from(path).length > 1024 || /[\u0000-\u001f\u007f]/.test(path)) return Promise.reject(new TypeError("utools.sharp toFile requires a showSaveDialog path."));
        return terminal({{ kind: "file", path }});
      }};
      if (property === "toBuffer") return (bufferOptions) => terminal({{ kind: "buffer" }}).then((result) => {{
        const data = sharpDataBytes(result && result.dataBase64);
        return bufferOptions && bufferOptions.resolveWithObject === true ? {{ data, info: Object.freeze({{ ...(result.info || {{}}) }}) }} : data;
      }});
      if (property === "toFormat") return (format, formatOptions) => {{
        const method = typeof format === "string" ? format.toLowerCase() : "";
        if (!sharpChainMethods.has(method) || !["jpeg", "jpg", "png", "webp", "gif", "tiff"].includes(method)) throw new TypeError("utools.sharp toFormat format is unsupported.");
        state.operations.push({{ method, args: formatOptions === undefined ? [] : [sharpPlainValue(formatOptions, 0)] }});
        return proxy;
      }};
      if (typeof property === "string" && sharpChainMethods.has(property)) return (...args) => {{
        if (state.operations.length >= 48) throw new RangeError("utools.sharp pipelines are limited to 48 operations.");
        state.operations.push({{ method: property, args: sharpPlainValue(args, 0) }});
        return proxy;
      }};
      return undefined;
    }}
  }});
  return proxy;
}}
const utools = Object.freeze({{
  db,
  dbStorage,
  dbCryptoStorage,
  ubrowser,
  getIdleUBrowsers() {{ return idleUBrowsers.map((value) => ({{ ...value }})); }},
  setUBrowserProxy(config) {{
    const normalized = ubrowserJsonValue(config, 0);
    if (!normalized || typeof normalized !== "object" || Array.isArray(normalized)) return false;
    void call("compatibility.utools.ubrowser.setProxy", {{ config: normalized }})
      .catch((error) => console.error("iHub ubrowser proxy configuration failed", error));
    return true;
  }},
  clearUBrowserCache() {{
    void call("compatibility.utools.ubrowser.clearCache", {{}})
      .catch((error) => console.error("iHub ubrowser cache clearing failed", error));
    return true;
  }},
  ai,
  allAiModels() {{ return call("compatibility.utools.ai.models", {{}}); }},
  runFFmpeg,
  sharp: createSharp,
  registerTool(name, handler) {{
    if (!config.lifecycleOwner) throw new Error("A uTools BrowserWindow cannot register MCP tools.");
    if (typeof name !== "string" || !declaredTools.has(name)) throw new TypeError("uTools registerTool name must exactly match plugin.json tools.");
    if (typeof handler !== "function") throw new TypeError("uTools registerTool handler must be a function.");
    const previous = toolHandlers.get(name);
    toolHandlers.set(name, handler);
    void call("compatibility.utools.tools.register", {{ name }})
      .catch((error) => {{
        if (toolHandlers.get(name) === handler) {{
          if (previous) toolHandlers.set(name, previous); else toolHandlers.delete(name);
        }}
        console.error("iHub uTools MCP registration failed", error);
      }});
  }},
  onPluginReady(callback) {{ if (typeof callback === "function") readyCallbacks.push(callback); }},
  onPluginEnter(callback) {{ if (typeof callback === "function") enterCallbacks.push(callback); }},
  onPluginOut(callback) {{ if (typeof callback === "function") outCallbacks.push(callback); }},
  onMainPush(callback, onSelect) {{
    if (typeof callback !== "function" || typeof onSelect !== "function") return;
    mainPushCallback = callback;
    mainPushSelectCallback = onSelect;
    ensureMainPushProviderRegistration();
  }},
  onDbPull(callback) {{ if (typeof callback === "function") dbPullCallbacks.push(callback); }},
  onPluginDetach(callback) {{
    if (typeof callback !== "function") return;
    if (pluginDetachDispatched) {{
      try {{ callback(); }} catch (error) {{ console.error("uTools compatibility detach callback failed", error); }}
      return;
    }}
    detachCallbacks.push(callback);
  }},
  createBrowserWindow(url, options, callback) {{
    if (currentWindowType === "browser") throw new Error("Nested uTools BrowserWindows are not supported by this host.");
    if (typeof url !== "string" || url.length === 0 || Array.from(url).length > 2048 || /[\u0000-\u001f\u007f]/.test(url)) throw new TypeError("uTools createBrowserWindow requires a bounded relative URL.");
    if (typeof options === "function" && callback === undefined) {{ callback = options; options = {{}}; }}
    if (options === undefined) options = {{}};
    if (!options || typeof options !== "object" || Array.isArray(options)) throw new TypeError("uTools BrowserWindow options must be an object.");
    if (callback !== undefined && typeof callback !== "function") throw new TypeError("uTools BrowserWindow callback must be a function.");
    const identity = call("compatibility.utools.browser.create", {{ url, options }}).then((result) => {{
      if (!result || typeof result.browserId !== "string") throw new Error("iHub returned an invalid BrowserWindow identity.");
      return result.browserId;
    }});
    return browserWindowProxy(identity, callback);
  }},
  sendToParent(channel, ...args) {{
    if (currentWindowType !== "browser") return;
    if (!validBrowserChannel(channel)) throw new TypeError("uTools BrowserWindow IPC channel is invalid.");
    void call("compatibility.utools.browser.sendToParent", {{ channel, args: boundedBrowserArgs(args) }})
      .catch((error) => console.error("iHub sendToParent failed", error));
  }},
  getFeatures(codes) {{
    if (codes !== undefined && (!Array.isArray(codes) || codes.some((code) => typeof code !== "string"))) return [];
    const selected = codes === undefined ? Array.from(dynamicFeatures.values()) : codes.flatMap((code) => dynamicFeatures.has(code) ? [dynamicFeatures.get(code)] : []);
    return selected.map(publicDynamicFeature);
  }},
  setFeature(value) {{
    const feature = normalizeDynamicFeature(value);
    if (!feature) {{ console.error("iHub rejected an invalid or unsupported uTools dynamic feature."); return; }}
    if (!dynamicFeatures.has(feature.code) && dynamicFeatures.size >= 64) {{ console.error("iHub limits each uTools plugin to 64 dynamic features."); return; }}
    const previous = dynamicFeatures.get(feature.code);
    const version = nextDynamicFeatureVersion(feature.code);
    dynamicFeatures.set(feature.code, feature);
    void call("compatibility.utools.features.set", {{ feature: publicDynamicFeature(feature) }})
      .then(() => ensureMainPushProviderRegistration())
      .catch((error) => {{
        if (dynamicFeatureVersions.get(feature.code) === version) {{
          if (previous) dynamicFeatures.set(feature.code, previous);
          else dynamicFeatures.delete(feature.code);
        }}
        console.error("iHub compatibility dynamic feature setup failed", error);
      }});
  }},
  removeFeature(code) {{
    if (typeof code !== "string" || !dynamicFeatures.has(code)) return false;
    const previous = dynamicFeatures.get(code);
    const version = nextDynamicFeatureVersion(code);
    dynamicFeatures.delete(code);
    void call("compatibility.utools.features.remove", {{ code }}).catch((error) => {{
      if (dynamicFeatureVersions.get(code) === version && previous) dynamicFeatures.set(code, previous);
      console.error("iHub compatibility dynamic feature removal failed", error);
    }});
    return true;
  }},
  hideMainWindowPasteText(value) {{
    if (typeof value !== "string" || new TextEncoder().encode(value).byteLength > 49152) return false;
    void interactionCall("compatibility.utools.input.pasteText", {{ value }})
      .catch((error) => console.error("iHub compatibility text paste failed", error));
    return true;
  }},
  hideMainWindowPasteImage(value) {{
    const payload = normalizedCopyImagePayload(value);
    if (!payload) return false;
    void interactionCall("compatibility.utools.input.pasteImage", payload)
      .catch((error) => console.error("iHub compatibility image paste failed", error));
    return true;
  }},
  hideMainWindowPasteFile(value) {{
    const paths = typeof value === "string" ? [value] : value;
    if (!Array.isArray(paths) || paths.length === 0 || paths.length > 16) return false;
    const encoder = new TextEncoder();
    let totalBytes = 0;
    const normalized = [];
    for (const path of paths) {{
      if (typeof path !== "string" || path.length === 0 || Array.from(path).length > 1024 || /[\u0000-\u001f\u007f]/.test(path)) return false;
      totalBytes += encoder.encode(path).byteLength;
      if (totalBytes > 8192 || normalized.includes(path)) return false;
      normalized.push(path);
    }}
    void interactionCall("compatibility.utools.input.pasteFiles", {{ paths: normalized }})
      .catch((error) => console.error("iHub compatibility file paste failed", error));
    return true;
  }},
  hideMainWindowTypeString(value) {{
    if (typeof value !== "string" || Array.from(value).length > 4096 || value.includes("\u0000")) return false;
    void interactionCall("compatibility.utools.input.typeString", {{ value }})
      .catch((error) => console.error("iHub compatibility text input failed", error));
    return true;
  }},
  simulateKeyboardTap(key, ...modifiers) {{
    if (typeof key !== "string" || key.length === 0 || Array.from(key).length > 32 || /[\u0000-\u001f\u007f]/.test(key) || modifiers.length > 4 || modifiers.some((modifier) => typeof modifier !== "string" || !["control", "ctrl", "shift", "option", "alt", "command", "super", "meta"].includes(modifier.trim().toLowerCase()))) return;
    void call("compatibility.utools.simulate.keyboardTap", {{ key, modifiers }})
      .catch((error) => console.error("iHub compatibility keyboard simulation failed", error));
  }},
  simulateMouseMove(x, y) {{
    const point = normalizedSimulationPoint(x, y, false);
    if (!point) return;
    void call("compatibility.utools.simulate.mouseMove", point)
      .catch((error) => console.error("iHub compatibility mouse move failed", error));
  }},
  simulateMouseClick(x, y) {{
    const point = normalizedSimulationPoint(x, y, true);
    if (!point) return;
    void call("compatibility.utools.simulate.mouseClick", point)
      .catch((error) => console.error("iHub compatibility mouse click failed", error));
  }},
  simulateMouseDoubleClick(x, y) {{
    const point = normalizedSimulationPoint(x, y, true);
    if (!point) return;
    void call("compatibility.utools.simulate.mouseDoubleClick", point)
      .catch((error) => console.error("iHub compatibility mouse double-click failed", error));
  }},
  simulateMouseRightClick(x, y) {{
    const point = normalizedSimulationPoint(x, y, true);
    if (!point) return;
    void call("compatibility.utools.simulate.mouseRightClick", point)
      .catch((error) => console.error("iHub compatibility mouse right-click failed", error));
  }},
  findInPage(text, options) {{
    if (typeof text !== "string" || text.length === 0 || Array.from(text).length > 512) return;
    if (options !== undefined && (!options || typeof options !== "object" || Array.isArray(options))) return;
    const allowed = new Set(["forward", "findNext", "matchCase", "wordStart", "medialCapitalAsWordStart"]);
    if (options && (Object.keys(options).some((key) => !allowed.has(key)) || Object.values(options).some((value) => typeof value !== "boolean"))) return;
    if (typeof window.find === "function") {{
      window.find(text, options?.matchCase === true, options?.forward === false, true, false, false, false);
    }}
  }},
  stopFindInPage(action) {{
    if (!["clearSelection", "keepSelection", "activateSelection"].includes(action)) return;
    const selection = window.getSelection?.();
    if (!selection || selection.rangeCount === 0) return;
    if (action === "activateSelection") {{
      const node = selection.anchorNode;
      const element = node?.nodeType === Node.ELEMENT_NODE ? node : node?.parentElement;
      if (element instanceof HTMLElement) element.focus({{ preventScroll: true }});
    }}
    if (action === "clearSelection") selection.removeAllRanges();
  }},
  showOpenDialog(options) {{ return syncDialog("open", options); }},
  showSaveDialog(options) {{ return syncDialog("save", options); }},
  setSubInput(callback, placeholder, isFocus) {{
    if (typeof callback !== "function") return false;
    if (placeholder !== undefined && typeof placeholder !== "string") return false;
    if (isFocus !== undefined && typeof isFocus !== "boolean") return false;
    const previous = subInputChangeCallback;
    subInputChangeCallback = callback;
    void call("ui.subInput.set", {{ placeholder: placeholder || "", focus: isFocus !== false }}).catch((error) => {{
      if (subInputChangeCallback === callback) subInputChangeCallback = previous;
      console.error("iHub compatibility sub-input setup failed", error);
    }});
    return true;
  }},
  removeSubInput() {{
    subInputChangeCallback = null;
    void call("ui.subInput.remove", {{}}).catch((error) => console.error("iHub compatibility sub-input removal failed", error));
    return true;
  }},
  setSubInputValue(value) {{
    if (typeof value !== "string") return false;
    void call("ui.subInput.setValue", {{ value }}).catch((error) => console.error("iHub compatibility sub-input update failed", error));
    return true;
  }},
  subInputFocus() {{ void call("ui.subInput.focus", {{}}).catch((error) => console.error("iHub compatibility sub-input focus failed", error)); return true; }},
  subInputBlur() {{ void call("ui.subInput.blur", {{}}).catch((error) => console.error("iHub compatibility sub-input blur failed", error)); return true; }},
  subInputSelect() {{ void call("ui.subInput.select", {{}}).catch((error) => console.error("iHub compatibility sub-input selection failed", error)); return true; }},
  hideMainWindow(isRestorePreWindow) {{
    if (isRestorePreWindow !== undefined && typeof isRestorePreWindow !== "boolean") return false;
    void call("compatibility.utools.window.hideMain", {{ isRestorePreWindow: isRestorePreWindow !== false }}).catch((error) => console.error("iHub compatibility window hide failed", error));
    return true;
  }},
  showMainWindow() {{ void call("compatibility.utools.window.showMain", {{}}).catch((error) => console.error("iHub compatibility window show failed", error)); return true; }},
  setExpendHeight(height) {{
    if (!Number.isInteger(height) || height < 100 || height > 900) return false;
    void call("compatibility.utools.window.setHeight", {{ height }})
      .catch((error) => console.error("iHub compatibility window resize failed", error));
    return true;
  }},
  outPlugin(isKill) {{
    if (isKill !== undefined && typeof isKill !== "boolean") return false;
    invokePluginOut(Boolean(isKill));
    void call("compatibility.utools.window.outPlugin", {{ isKill: Boolean(isKill) }})
      .catch((error) => console.error("iHub compatibility plugin exit failed", error));
    return true;
  }},
  redirect(label, payload) {{
    let params;
    try {{ params = normalizedRedirect(label, payload); }}
    catch (error) {{ console.error("iHub rejected an invalid uTools redirect", error); return false; }}
    void call("compatibility.utools.window.redirect", params)
      .catch((error) => console.error("iHub compatibility redirect failed", error));
    return true;
  }},
  copyText(value) {{
    if (typeof value !== "string" || new TextEncoder().encode(value).byteLength > 49152) return false;
    void call("compatibility.utools.clipboard.writeText", {{ value }})
      .catch((error) => console.error("iHub compatibility clipboard write failed", error));
    return true;
  }},
  copyImage(value) {{
    const payload = normalizedCopyImagePayload(value);
    if (!payload) return false;
    void call("compatibility.utools.clipboard.writeImage", payload)
      .catch((error) => console.error("iHub compatibility image copy failed", error));
    return true;
  }},
  copyFile(value) {{
    const normalized = normalizedUtoolsFilePaths(value);
    if (!normalized) return false;
    void call("compatibility.utools.clipboard.writeFiles", {{ paths: normalized }})
      .catch((error) => console.error("iHub compatibility file copy failed", error));
    return true;
  }},
  startDrag(value) {{
    const paths = normalizedUtoolsFilePaths(value);
    if (!paths) {{ console.error("iHub rejected invalid uTools startDrag paths."); return; }}
    void call("compatibility.utools.window.startDrag", {{ paths }})
      .catch((error) => console.error("iHub compatibility native file drag failed", error));
  }},
  getCopyedFiles() {{
    try {{ return syncCopiedFiles(); }}
    catch (error) {{ console.error("iHub compatibility clipboard file read failed", error); return []; }}
  }},
  showNotification(body, clickFeatureCode) {{
    if (typeof body !== "string") return;
    const trimmedBody = body.trim();
    if (trimmedBody.length === 0 || Array.from(trimmedBody).length > 1000) return;
    if (clickFeatureCode !== undefined && (typeof clickFeatureCode !== "string" || clickFeatureCode.trim().length === 0 || Array.from(clickFeatureCode.trim()).length > 160 || /[\u0000-\u001f\u007f]/.test(clickFeatureCode))) return;
    const params = clickFeatureCode === undefined ? {{ body: trimmedBody }} : {{ body: trimmedBody, clickFeatureCode: clickFeatureCode.trim() }};
    void call("compatibility.utools.notification.show", params)
      .catch((error) => console.error("iHub compatibility notification failed", error));
  }},
  shellOpenExternal(url) {{
    if (typeof url !== "string" || url.length === 0 || Array.from(url).length > 2048 || /[\u0000-\u001f\u007f]/.test(url)) return;
    void call("compatibility.utools.shell.openExternal", {{ url }})
      .catch((error) => console.error("iHub compatibility external URL failed", error));
  }},
  shellOpenPath(path) {{
    if (typeof path !== "string" || path.length === 0 || Array.from(path).length > 1024 || /[\u0000-\u001f\u007f]/.test(path)) return;
    void call("compatibility.utools.shell.openPath", {{ path }})
      .catch((error) => console.error("iHub compatibility local open failed", error));
  }},
  shellShowItemInFolder(path) {{
    if (typeof path !== "string" || path.length === 0 || Array.from(path).length > 1024 || /[\u0000-\u001f\u007f]/.test(path)) return;
    void call("compatibility.utools.shell.showItemInFolder", {{ path }})
      .catch((error) => console.error("iHub compatibility file reveal failed", error));
  }},
  shellTrashItem(path) {{
    if (typeof path !== "string" || path.length === 0 || Array.from(path).length > 1024 || /[\u0000-\u001f\u007f]/.test(path)) return;
    void call("compatibility.utools.shell.trashItem", {{ path }})
      .catch((error) => console.error("iHub compatibility recycle-bin action failed", error));
  }},
  getFileIcon(path) {{
    if (typeof path !== "string" || path.length === 0 || Array.from(path).length > 1024 || new TextEncoder().encode(path).byteLength > 8192 || /[\u0000-\u001f\u007f]/.test(path)) return "";
    try {{ return syncFileIcon(path); }}
    catch (error) {{ console.error("iHub compatibility file icon failed", error); return ""; }}
  }},
  readCurrentFolderPath() {{
    return call("compatibility.utools.system.readCurrentFolderPath", {{}});
  }},
  readCurrentBrowserUrl() {{
    return call("compatibility.utools.system.readCurrentBrowserUrl", {{}});
  }},
  shellBeep() {{
    void call("compatibility.utools.shell.beep", {{}})
      .catch((error) => console.error("iHub compatibility system beep failed", error));
  }},
  screenColorPick(callback) {{
    if (typeof callback !== "function") return;
    void call("cursorColor.sampleOnce", {{}}).then((color) => callback(color)).catch((error) => console.error("iHub compatibility color pick failed", error));
  }},
  screenCapture(callback) {{
    if (typeof callback !== "function") return;
    void call("compatibility.utools.screen.capture", {{}}).then((image) => callback(image)).catch((error) => console.error("iHub compatibility screen capture failed", error));
  }},
  getPrimaryDisplay() {{
    const snapshot = syncScreenSnapshot();
    const display = snapshot.displays.find((candidate) => candidate && candidate.id === snapshot.primaryDisplayId) || snapshot.displays[0];
    if (!display) throw new Error("No active display is available.");
    return publicDisplay(display);
  }},
  getAllDisplays() {{ return syncScreenSnapshot().displays.map(publicDisplay); }},
  getCursorScreenPoint() {{
    const point = syncScreenSnapshot().cursorScreenPoint;
    return screenPoint(point, "getCursorScreenPoint");
  }},
  getDisplayNearestPoint(value) {{
    const point = screenPoint(value, "getDisplayNearestPoint");
    const snapshot = syncScreenSnapshot();
    return displayForMetric(snapshot, nearestMetric(snapshot, point, "dipBounds"));
  }},
  getDisplayMatching(value) {{
    const rect = screenRect(value, "getDisplayMatching");
    return displayMatchingRect(syncScreenSnapshot(), rect);
  }},
  screenToDipPoint(value) {{
    const point = screenPoint(value, "screenToDipPoint");
    const snapshot = syncScreenSnapshot();
    const metric = nearestMetric(snapshot, point, "physicalBounds");
    return {{
      x: metric.dipBounds.x + Math.round((point.x - metric.physicalBounds.x) / metric.scaleFactor),
      y: metric.dipBounds.y + Math.round((point.y - metric.physicalBounds.y) / metric.scaleFactor)
    }};
  }},
  dipToScreenPoint(value) {{
    const point = screenPoint(value, "dipToScreenPoint");
    const snapshot = syncScreenSnapshot();
    const metric = nearestMetric(snapshot, point, "dipBounds");
    return {{
      x: metric.physicalBounds.x + Math.round((point.x - metric.dipBounds.x) * metric.scaleFactor),
      y: metric.physicalBounds.y + Math.round((point.y - metric.dipBounds.y) * metric.scaleFactor)
    }};
  }},
  screenToDipRect(value) {{
    const rect = screenRect(value, "screenToDipRect");
    const snapshot = syncScreenSnapshot();
    const metric = nearestMetric(snapshot, rect, "physicalBounds");
    return {{
      x: metric.dipBounds.x + Math.round((rect.x - metric.physicalBounds.x) / metric.scaleFactor),
      y: metric.dipBounds.y + Math.round((rect.y - metric.physicalBounds.y) / metric.scaleFactor),
      width: Math.round(rect.width / metric.scaleFactor),
      height: Math.round(rect.height / metric.scaleFactor)
    }};
  }},
  dipToScreenRect(value) {{
    const rect = screenRect(value, "dipToScreenRect");
    const snapshot = syncScreenSnapshot();
    const metric = nearestMetric(snapshot, rect, "dipBounds");
    return {{
      x: metric.physicalBounds.x + Math.round((rect.x - metric.dipBounds.x) * metric.scaleFactor),
      y: metric.physicalBounds.y + Math.round((rect.y - metric.dipBounds.y) * metric.scaleFactor),
      width: Math.round(rect.width * metric.scaleFactor),
      height: Math.round(rect.height * metric.scaleFactor)
    }};
  }},
  async desktopCaptureSources(value) {{
    const options = desktopCaptureOptions(value);
    if (!legacyDesktopCaptureBridgeAvailable || !mediaDevices || typeof mediaDevices.getDisplayMedia !== "function") throw new Error("This WebView does not support the secure display picker compatibility bridge.");
    stopDesktopCaptureSlot();
    const focusLeasePromise = call("screenCapture.acquireFocusLease", {{}}).catch(() => null);
    let stream;
    try {{
      const streamPromise = mediaDevices.getDisplayMedia({{ video: true, audio: true }});
      stream = await streamPromise;
    }} finally {{
      const focusLease = await focusLeasePromise;
      if (focusLease && typeof focusLease.leaseId === "string") await call("screenCapture.releaseFocusLease", {{ leaseId: focusLease.leaseId }}).catch(() => undefined);
    }}
    const videoTrack = stream.getVideoTracks()[0];
    if (!videoTrack) {{ for (const track of stream.getTracks()) track.stop(); throw new Error("The selected desktop source has no video track."); }}
    const settings = typeof videoTrack.getSettings === "function" ? videoTrack.getSettings() : {{}};
    const sourceKind = settings.displaySurface === "monitor"
      ? "screen"
      : settings.displaySurface === "window" || settings.displaySurface === "browser"
        ? "window"
        : options.types.length === 1 ? options.types[0] : "screen";
    if (!options.types.includes(sourceKind)) {{
      for (const track of stream.getTracks()) track.stop();
      throw new Error("The selected source type was not requested by desktopCaptureSources.");
    }}
    let thumbnail;
    try {{ thumbnail = await desktopCaptureThumbnail(stream, options.thumbnailSize); }}
    catch (error) {{ for (const track of stream.getTracks()) track.stop(); throw error; }}
    const id = sourceKind + ":" + (9000000 + (++desktopCaptureSequence)).toString(10) + ":0";
    const slot = {{ id, stream, timeout: 0 }};
    slot.timeout = window.setTimeout(() => {{ if (desktopCaptureSlot === slot) stopDesktopCaptureSlot(); }}, 60000);
    desktopCaptureSlot = slot;
    videoTrack.addEventListener("ended", () => {{
      if (desktopCaptureSlot === slot) {{ window.clearTimeout(slot.timeout); desktopCaptureSlot = null; }}
    }}, {{ once: true }});
    return [Object.freeze({{
      id,
      name: videoTrack.label || (sourceKind === "screen" ? "Selected Screen" : "Selected Window"),
      thumbnail,
      display_id: "",
      appIcon: null
    }})];
  }},
  getWindowType() {{ return currentWindowType; }},
  getNativeId() {{ return config.nativeId; }},
  getPath(name) {{
    if (typeof name !== "string" || !Object.prototype.hasOwnProperty.call(config.paths, name)) return "";
    const value = config.paths[name];
    return typeof value === "string" ? value : "";
  }},
  getAppName() {{ return "iHub"; }},
  getAppVersion() {{ return config.appVersion; }},
  getUser() {{ return null; }},
  fetchUserServerTemporaryToken() {{
    return Promise.reject(new Error("iHub has no uTools account session and cannot issue a uTools user token."));
  }},
  isPurchasedUser() {{ return false; }},
  openPurchase(options, callback) {{
    if (!validUtoolsPaymentOptions(options) || (callback !== undefined && typeof callback !== "function")) return;
    console.error("iHub cannot open uTools purchases because no uTools billing session is available.");
  }},
  openPayment(options, callback) {{
    if (!validUtoolsPaymentOptions(options) || (callback !== undefined && typeof callback !== "function")) return;
    console.error("iHub cannot open uTools payments because no uTools billing session is available.");
  }},
  fetchUserPayments() {{
    return Promise.reject(new Error("iHub has no uTools account session and cannot read uTools payment records."));
  }},
  isDev() {{ return config.isDevelopment === true; }},
  isDarkColors() {{ return typeof window.matchMedia === "function" && window.matchMedia("(prefers-color-scheme: dark)").matches; }},
  isWindows() {{ return /\\bwindows?\\b|\\bwin(?:32|64)\\b/.test((navigator.platform + " " + navigator.userAgent).toLowerCase()); }},
  isMacOS() {{ const platform = (navigator.platform + " " + navigator.userAgent).toLowerCase(); return platform.includes("mac") || platform.includes("darwin"); }},
  isLinux() {{ return (navigator.platform + " " + navigator.userAgent).toLowerCase().includes("linux"); }}
}});
Object.defineProperties(window, {{
  utools: {{ value: utools, configurable: false, writable: false }},
  rubick: {{ value: utools, configurable: false, writable: false }}
}});
// Public uTools preloads are CommonJS scripts. iHub deliberately exposes only
// a tiny sandbox module surface here: no Node filesystem/process/network
// authority is implied by loading the declared script in the WebView.
const sandboxModule = {{ exports: {{}} }};
try {{
  if (!("module" in window)) Object.defineProperty(window, "module", {{ value: sandboxModule, configurable: false, writable: false }});
  if (!("exports" in window)) Object.defineProperty(window, "exports", {{ value: sandboxModule.exports, configurable: true, writable: true }});
}} catch {{ /* A package page may already own one of these ordinary CommonJS names. */ }}
if (config.windowType === "browser") {{
  try {{
    Object.defineProperty(window, "close", {{
      configurable: false,
      writable: false,
      value() {{ void call("compatibility.utools.browser.closeSelf", {{}}).catch((error) => console.error("iHub BrowserWindow self-close failed", error)); }}
    }});
  }} catch {{ /* WebView may keep its native close property non-configurable. */ }}
}}
const bootstrap = Promise.all([
  call("compatibility.utools.dbStorage.snapshot", {{}}),
  call("compatibility.utools.dbCryptoStorage.snapshot", {{}}).catch((error) => {{
    console.error("iHub compatibility dbCryptoStorage restore failed", error);
    return null;
  }}),
  config.lifecycleOwner ? call("compatibility.utools.features.snapshot", {{}}) : Promise.resolve([])
])
  .then(([snapshot, cryptoSnapshot, features]) => {{
    if (snapshot && typeof snapshot === "object" && !Array.isArray(snapshot)) {{
      for (const [key, value] of Object.entries(snapshot)) {{
        if (!dbStorageVersions.has(key)) dbStorageState[key] = value;
      }}
    }}
    if (cryptoSnapshot && typeof cryptoSnapshot === "object" && !Array.isArray(cryptoSnapshot)) {{
      for (const [key, value] of Object.entries(cryptoSnapshot)) {{
        if (!dbCryptoStorageVersions.has(key)) dbCryptoStorageState[key] = value;
      }}
    }}
    if (Array.isArray(features)) {{
      for (const value of features) {{
        const feature = normalizeDynamicFeature(value);
        if (feature && !dynamicFeatureVersions.has(feature.code)) dynamicFeatures.set(feature.code, feature);
      }}
    }}
    if (config.lifecycleOwner) ensureMainPushProviderRegistration();
  }})
  .catch((error) => console.error("iHub compatibility storage restore failed", error))
  .then(() => config.lifecycleOwner ? call("lifecycle.ready", {{}}) : undefined)
  .then(() => invoke(readyCallbacks, undefined))
  .catch((error) => console.error("iHub uTools compatibility bootstrap failed", error));
void bootstrap;
window.addEventListener("pagehide", () => {{
  stopDesktopCaptureSlot();
  for (const requestId of activeFfmpegRequests.keys()) void call("compatibility.utools.ffmpeg.kill", {{ requestId }}).catch(() => undefined);
  activeFfmpegRequests.clear();
  if (config.lifecycleOwner) {{ invokePluginOut(false); void call("lifecycle.dispose", {{}}).catch(() => undefined); }}
}}, {{ once: true }});
}})();
"#
    )
    .into_bytes())
}

fn heartbeat_is_fresh(last_heartbeat: &Mutex<Instant>) -> bool {
    last_heartbeat
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .elapsed()
        < FRONTEND_LEASE_HEARTBEAT_TIMEOUT
}

fn write_status(stream: &mut TcpStream, status: &str) -> io::Result<()> {
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Length: 0\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(header.as_bytes())
}

fn plugin_csp(allows_remote_network: bool, allows_script_execution: bool) -> &'static str {
    match (allows_remote_network, allows_script_execution) {
        (true, true) => NETWORKED_BROWSER_PLUGIN_CSP,
        (false, true) => LOCKED_BROWSER_PLUGIN_CSP,
        (true, false) => NETWORKED_PLUGIN_CSP,
        (false, false) => LOCKED_PLUGIN_CSP,
    }
}

fn content_type(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" | "cjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" | "webmanifest" => "application/json; charset=utf-8",
        "txt" => "text/plain; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "m4a" => "audio/mp4",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{Read, Write},
        net::TcpStream,
        sync::{mpsc, Arc, Mutex},
        thread,
        time::Duration,
    };

    use super::{
        execute_utools_sync_clipboard_request, inject_utools_compat_document,
        inject_utools_compat_script, render_utools_compat_script, resolve_asset_path, HttpMethod,
        HttpRequest, PluginAssetServer, PluginFrontendAssetBundle, PluginFrontendPurpose,
        ServedBundle, LOCKED_PLUGIN_CSP, NETWORKED_PLUGIN_CSP, UTOOLS_PRELOAD_SCRIPT_NAME,
    };
    use crate::plugins::{UtoolsCompatCommand, UtoolsCompatRuntimeConfig};
    use crate::utools_db::UtoolsDocumentStore;

    fn temporary_bundle(
        plugin_id: &str,
        allows_remote_network: bool,
    ) -> (std::path::PathBuf, PluginFrontendAssetBundle) {
        let root =
            std::env::temp_dir().join(format!("ihub-plugin-assets-test-{}", uuid::Uuid::new_v4()));
        let asset_root = root.join("dist");
        fs::create_dir_all(&asset_root).expect("test asset root should be created");
        let entry = asset_root.join("index.html");
        fs::write(&entry, "<main>plugin</main>").expect("test entry should be written");
        let asset_root = asset_root
            .canonicalize()
            .expect("asset root should canonicalize");
        let entry = entry.canonicalize().expect("entry should canonicalize");
        (
            root,
            PluginFrontendAssetBundle {
                plugin_id: plugin_id.to_owned(),
                asset_root,
                entry,
                synthetic_entry: false,
                blocked_asset_paths: Vec::new(),
                allows_display_capture: false,
                allows_microphone: false,
                allows_remote_network,
                utools_compat: None,
                utools_preload_script: None,
                utools_browser_preload_src: None,
            },
        )
    }

    fn utools_runtime_config(plugin_id: &str) -> UtoolsCompatRuntimeConfig {
        UtoolsCompatRuntimeConfig {
            app_version: "0.1.0".to_owned(),
            is_development: false,
            plugin_id: plugin_id.to_owned(),
            commands: Vec::new(),
            tools: Vec::new(),
            native_id: "ihub-0123456789abcdef0123456789abcdef".to_owned(),
            paths: Default::default(),
            idle_ubrowsers: Vec::new(),
            window_type: "main".to_owned(),
            lifecycle_owner: true,
        }
    }

    fn send_sync_database_request(
        lease: &super::PluginFrontendLease,
        payload: &serde_json::Value,
        include_capability_header: bool,
    ) -> (String, serde_json::Value) {
        let url = url::Url::parse(&lease.url).expect("lease URL should parse");
        let host = url.host_str().expect("lease host");
        let port = url.port().expect("lease port");
        let target = format!("{}{}", url.path(), super::UTOOLS_SYNC_DB_ROUTE);
        let body = serde_json::to_vec(payload).expect("request JSON");
        let capability_header = if include_capability_header {
            "X-IHub-Utools-DB: 1\r\n"
        } else {
            ""
        };
        let request = format!(
            "POST {target} HTTP/1.1\r\nHost: {host}:{port}\r\nOrigin: http://{host}:{port}\r\nSec-Fetch-Site: same-origin\r\nContent-Type: application/json\r\n{capability_header}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let mut stream = TcpStream::connect((host, port)).expect("sync endpoint should accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("sync response timeout");
        stream
            .write_all(request.as_bytes())
            .expect("request header");
        stream.write_all(&body).expect("request body");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .expect("complete sync response");
        let header_end = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|index| index + 4)
            .expect("complete response header");
        let status = std::str::from_utf8(&response[..header_end])
            .expect("UTF-8 response header")
            .lines()
            .next()
            .expect("response status")
            .to_owned();
        let payload = serde_json::from_slice(&response[header_end..]).expect("response JSON");
        (status, payload)
    }

    fn send_sync_screen_request(
        lease: &super::PluginFrontendLease,
        method: &str,
    ) -> (String, serde_json::Value) {
        let url = url::Url::parse(&lease.url).expect("lease URL should parse");
        let host = url.host_str().expect("lease host");
        let port = url.port().expect("lease port");
        let target = format!("{}{}", url.path(), super::UTOOLS_SYNC_SCREEN_ROUTE);
        let request = format!(
            "{method} {target} HTTP/1.1\r\nHost: {host}:{port}\r\nOrigin: http://{host}:{port}\r\nSec-Fetch-Site: same-origin\r\nConnection: close\r\n\r\n"
        );
        let mut stream = TcpStream::connect((host, port)).expect("screen endpoint should accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("screen response timeout");
        stream
            .write_all(request.as_bytes())
            .expect("screen request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .expect("complete screen response");
        let header_end = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|index| index + 4)
            .expect("complete response header");
        let status = std::str::from_utf8(&response[..header_end])
            .expect("UTF-8 response header")
            .lines()
            .next()
            .expect("response status")
            .to_owned();
        let payload = serde_json::from_slice(&response[header_end..]).expect("response JSON");
        (status, payload)
    }

    fn send_sync_clipboard_request(
        lease: &super::PluginFrontendLease,
        method: &str,
        include_capability_header: bool,
    ) -> (String, serde_json::Value) {
        let url = url::Url::parse(&lease.url).expect("lease URL should parse");
        let host = url.host_str().expect("lease host");
        let port = url.port().expect("lease port");
        let target = format!("{}{}", url.path(), super::UTOOLS_SYNC_CLIPBOARD_ROUTE);
        let capability_header = if include_capability_header {
            "X-IHub-Utools-Clipboard: 1\r\n"
        } else {
            ""
        };
        let request = format!(
            "{method} {target} HTTP/1.1\r\nHost: {host}:{port}\r\nOrigin: http://{host}:{port}\r\nSec-Fetch-Site: same-origin\r\n{capability_header}Connection: close\r\n\r\n"
        );
        let mut stream =
            TcpStream::connect((host, port)).expect("clipboard endpoint should accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("clipboard response timeout");
        stream
            .write_all(request.as_bytes())
            .expect("clipboard request");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .expect("complete clipboard response");
        let header_end = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|index| index + 4)
            .expect("complete response header");
        let status = std::str::from_utf8(&response[..header_end])
            .expect("UTF-8 response header")
            .lines()
            .next()
            .expect("response status")
            .to_owned();
        let payload = if response.len() == header_end {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&response[header_end..]).expect("clipboard response JSON")
        };
        (status, payload)
    }

    fn send_sync_icon_request(
        lease: &super::PluginFrontendLease,
        path: &str,
        include_capability_header: bool,
    ) -> (String, serde_json::Value) {
        let url = url::Url::parse(&lease.url).expect("lease URL should parse");
        let host = url.host_str().expect("lease host");
        let port = url.port().expect("lease port");
        let target = format!("{}{}", url.path(), super::UTOOLS_SYNC_ICON_ROUTE);
        let body =
            serde_json::to_vec(&serde_json::json!({ "path": path })).expect("icon request JSON");
        let capability_header = if include_capability_header {
            "X-IHub-Utools-Icon: 1\r\n"
        } else {
            ""
        };
        let request = format!(
            "POST {target} HTTP/1.1\r\nHost: {host}:{port}\r\nOrigin: http://{host}:{port}\r\nSec-Fetch-Site: same-origin\r\nContent-Type: application/json\r\n{capability_header}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let mut stream = TcpStream::connect((host, port)).expect("icon endpoint should accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("icon response timeout");
        stream
            .write_all(request.as_bytes())
            .expect("icon request header");
        stream.write_all(&body).expect("icon request body");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .expect("complete icon response");
        let header_end = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|index| index + 4)
            .expect("complete response header");
        let status = std::str::from_utf8(&response[..header_end])
            .expect("UTF-8 response header")
            .lines()
            .next()
            .expect("response status")
            .to_owned();
        let payload = serde_json::from_slice(&response[header_end..]).expect("response JSON");
        (status, payload)
    }

    fn send_sync_dialog_request(
        lease: &super::PluginFrontendLease,
        body: serde_json::Value,
        include_capability_header: bool,
    ) -> (String, serde_json::Value) {
        let url = url::Url::parse(&lease.url).expect("lease URL should parse");
        let host = url.host_str().expect("lease host");
        let port = url.port().expect("lease port");
        let target = format!("{}{}", url.path(), super::UTOOLS_SYNC_DIALOG_ROUTE);
        let body = serde_json::to_vec(&body).expect("dialog request JSON");
        let capability_header = if include_capability_header {
            "X-IHub-Utools-Dialog: 1\r\n"
        } else {
            ""
        };
        let request = format!(
            "POST {target} HTTP/1.1\r\nHost: {host}:{port}\r\nOrigin: http://{host}:{port}\r\nSec-Fetch-Site: same-origin\r\nContent-Type: application/json\r\n{capability_header}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let mut stream = TcpStream::connect((host, port)).expect("dialog endpoint should accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .expect("dialog response timeout");
        stream
            .write_all(request.as_bytes())
            .expect("dialog request header");
        stream.write_all(&body).expect("dialog request body");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .expect("complete dialog response");
        let header_end = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|index| index + 4)
            .expect("complete response header");
        let status = std::str::from_utf8(&response[..header_end])
            .expect("UTF-8 response header")
            .lines()
            .next()
            .expect("response status")
            .to_owned();
        let payload = if response.len() == header_end {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&response[header_end..]).expect("dialog response JSON")
        };
        (status, payload)
    }

    #[test]
    fn utools_bootstrap_is_host_owned_and_precedes_page_scripts() {
        let config = UtoolsCompatRuntimeConfig {
            app_version: "0.1.0".to_owned(),
            is_development: true,
            plugin_id: "utools-color-picker".to_owned(),
            commands: vec![UtoolsCompatCommand {
                command_id: "utools-feature-1".to_owned(),
                code: "pick-color".to_owned(),
                keywords: vec!["取色".to_owned()],
                main_push: true,
            }],
            tools: Vec::new(),
            native_id: "ihub-0123456789abcdef0123456789abcdef".to_owned(),
            paths: [("home".to_owned(), "C:\\Users\\Tester".to_owned())]
                .into_iter()
                .collect(),
            idle_ubrowsers: Vec::new(),
            window_type: "main".to_owned(),
            lifecycle_owner: true,
        };
        let script = String::from_utf8(
            render_utools_compat_script(&config).expect("compatibility bootstrap should render"),
        )
        .expect("bootstrap should be UTF-8");
        assert!(script.contains("Object.defineProperties(window"));
        assert!(script.contains("rubick"));
        assert!(script.contains("copyText"));
        assert!(script.contains("compatibility.utools.clipboard.writeText"));
        assert!(script.contains("copyImage"));
        assert!(script.contains("pngDataUrlForCopyImage"));
        assert!(script.contains("normalizedCopyImagePayload"));
        assert!(script.contains("return { path: value }"));
        assert!(script.contains("value instanceof Uint8Array"));
        assert!(script.contains("compatibility.utools.clipboard.writeImage"));
        assert!(script.contains("copyFile"));
        assert!(script.contains("compatibility.utools.clipboard.writeFiles"));
        assert!(script.contains("getCopyedFiles"));
        assert!(script.contains("function syncCopiedFiles"));
        assert!(script.contains("X-IHub-Utools-Clipboard"));
        assert!(script.contains("isDiractory"));
        assert!(script.contains("showNotification"));
        assert!(script.contains("Array.from(trimmedBody).length > 1000"));
        assert!(script.contains("clickFeatureCode: clickFeatureCode.trim()"));
        assert!(script.contains("compatibility.utools.notification.show"));
        assert!(script.contains("shellOpenExternal"));
        assert!(script.contains("compatibility.utools.shell.openExternal"));
        assert!(script.contains("shellOpenPath"));
        assert!(script.contains("compatibility.utools.shell.openPath"));
        assert!(script.contains("shellShowItemInFolder"));
        assert!(script.contains("compatibility.utools.shell.showItemInFolder"));
        assert!(script.contains("shellTrashItem"));
        assert!(script.contains("compatibility.utools.shell.trashItem"));
        assert!(script.contains("getFileIcon(path)"));
        assert!(script.contains("request.open(\"POST\", syncIconRoute, false)"));
        assert!(script.contains("X-IHub-Utools-Icon"));
        assert!(script.contains("readCurrentFolderPath()"));
        assert!(script.contains("compatibility.utools.system.readCurrentFolderPath"));
        assert!(script.contains("readCurrentBrowserUrl()"));
        assert!(script.contains("compatibility.utools.system.readCurrentBrowserUrl"));
        assert!(script.contains("showOpenDialog(options)"));
        assert!(script.contains("showSaveDialog(options)"));
        assert!(script.contains("X-IHub-Utools-Dialog"));
        assert!(script.contains("syncDialog(\"open\", options)"));
        assert!(script.contains("syncDialog(\"save\", options)"));
        assert!(script.contains("redirect(label, payload)"));
        assert!(script.contains("compatibility.utools.window.redirect"));
        assert!(script.contains("from: \"redirect\""));
        assert!(script.contains("projectedRedirectAction"));
        assert!(script.contains("shellBeep"));
        assert!(script.contains("compatibility.utools.shell.beep"));
        assert!(script.contains("screenColorPick"));
        assert!(script.contains("screenCapture(callback)"));
        assert!(script.contains("compatibility.utools.screen.capture"));
        assert!(script.contains("getPrimaryDisplay()"));
        assert!(script.contains("getAllDisplays()"));
        assert!(script.contains("getCursorScreenPoint()"));
        assert!(script.contains("getDisplayNearestPoint(value)"));
        assert!(script.contains("getDisplayMatching(value)"));
        assert!(script.contains("screenToDipPoint(value)"));
        assert!(script.contains("dipToScreenPoint(value)"));
        assert!(script.contains("screenToDipRect(value)"));
        assert!(script.contains("dipToScreenRect(value)"));
        assert!(script.contains("request.open(\"GET\", syncScreenRoute, false)"));
        assert!(script.contains("desktopCaptureSources(value)"));
        assert!(script.contains("mediaDevices.getDisplayMedia"));
        assert!(script.contains("chromeMediaSourceId"));
        assert!(script.contains("stopDesktopCaptureSlot()"));
        assert!(script.contains("onPluginDetach"));
        assert!(script.contains("onMainPush"));
        assert!(script.contains("onDbPull"));
        assert!(script.contains("utools-main-push"));
        assert!(script.contains("compatibility.utools.mainPush.selectComplete"));
        assert!(script.contains("\"keywords\":[\"取色\"]"));
        assert!(script.contains("\"mainPush\":true"));
        assert!(script.contains("invokePluginDetach"));
        assert!(script.contains("cursorColor.sampleOnce"));
        assert!(script.contains("dbStorage"));
        assert!(script.contains("compatibility.utools.dbStorage.set"));
        assert!(script.contains("compatibility.utools.dbStorage.remove"));
        assert!(script.contains("startDrag(value)"));
        assert!(script.contains("compatibility.utools.window.startDrag"));
        assert!(script.contains("fetchUserServerTemporaryToken"));
        assert!(script.contains("isPurchasedUser"));
        assert!(script.contains("openPurchase(options, callback)"));
        assert!(script.contains("openPayment(options, callback)"));
        assert!(script.contains("fetchUserPayments"));
        assert!(script.contains("config.isDevelopment === true"));
        assert!(script.contains("\"isDevelopment\":true"));
        assert!(script.contains("dbCryptoStorage"));
        assert!(script.contains("compatibility.utools.dbCryptoStorage.snapshot"));
        assert!(script.contains("compatibility.utools.dbCryptoStorage.set"));
        assert!(script.contains("compatibility.utools.dbCryptoStorage.remove"));
        assert!(script.contains("const dbPromises = Object.freeze"));
        assert!(script.contains("compatibility.utools.db.get"));
        assert!(script.contains("compatibility.utools.db.put"));
        assert!(script.contains("compatibility.utools.db.remove"));
        assert!(script.contains("compatibility.utools.db.bulkDocs"));
        assert!(script.contains("compatibility.utools.db.allDocs"));
        assert!(script.contains("postAttachment"));
        assert!(script.contains("compatibility.utools.db.postAttachment"));
        assert!(script.contains("getAttachment"));
        assert!(script.contains("compatibility.utools.db.getAttachment"));
        assert!(script.contains("getAttachmentType"));
        assert!(script.contains("replicateStateFromCloud"));
        assert!(script.contains("function syncDbCall"));
        assert!(script.contains("request.open(\"POST\", syncDbRoute, false)"));
        assert!(script.contains("X-IHub-Utools-DB"));
        assert!(script.contains("getFeatures"));
        assert!(script.contains("setFeature"));
        assert!(script.contains("removeFeature"));
        assert!(script.contains("compatibility.utools.features.snapshot"));
        assert!(script.contains("compatibility.utools.features.set"));
        assert!(script.contains("compatibility.utools.features.remove"));
        assert!(script.contains("utools-dynamic-"));
        assert!(script.contains("findInPage"));
        assert!(script.contains("stopFindInPage"));
        assert!(script.contains("selection.removeAllRanges()"));
        assert!(script.contains("hideMainWindowPasteText"));
        assert!(script.contains("compatibility.utools.input.pasteText"));
        assert!(script.contains("hideMainWindowPasteImage"));
        assert!(script.contains("compatibility.utools.input.pasteImage"));
        assert!(script.contains("hideMainWindowPasteFile"));
        assert!(script.contains("compatibility.utools.input.pasteFiles"));
        assert!(script.contains("hideMainWindowTypeString"));
        assert!(script.contains("compatibility.utools.input.typeString"));
        assert!(script.contains("simulateKeyboardTap(key, ...modifiers)"));
        assert!(script.contains("compatibility.utools.simulate.keyboardTap"));
        assert!(script.contains("simulateMouseMove(x, y)"));
        assert!(script.contains("compatibility.utools.simulate.mouseMove"));
        assert!(script.contains("simulateMouseClick(x, y)"));
        assert!(script.contains("compatibility.utools.simulate.mouseClick"));
        assert!(script.contains("simulateMouseDoubleClick(x, y)"));
        assert!(script.contains("compatibility.utools.simulate.mouseDoubleClick"));
        assert!(script.contains("simulateMouseRightClick(x, y)"));
        assert!(script.contains("compatibility.utools.simulate.mouseRightClick"));
        assert!(script.contains("setSubInput"));
        assert!(script.contains("subInputSelect"));
        assert!(script.contains("compatibility.utools.window.hideMain"));
        assert!(script.contains("setExpendHeight"));
        assert!(script.contains("compatibility.utools.window.setHeight"));
        assert!(script.contains("compatibility.utools.window.outPlugin"));
        assert!(script.contains("registerTool(name, handler)"));
        assert!(script.contains("compatibility.utools.tools.register"));
        assert!(script.contains("compatibility.utools.tools.complete"));
        assert!(script.contains("compatibility.utools.tools.progress"));
        assert!(script.contains("event/utools.tool.invoke"));
        assert!(script.contains("function ai(option, streamCallback)"));
        assert!(script.contains("compatibility.utools.ai.start"));
        assert!(script.contains("compatibility.utools.ai.abort"));
        assert!(script.contains("compatibility.utools.ai.toolComplete"));
        assert!(script.contains("event/utools.ai.chunk"));
        assert!(script.contains("allAiModels()"));
        assert!(script.contains("function createSharp(input, options, inherited)"));
        assert!(script.contains("compatibility.utools.sharp.execute"));
        assert!(script.contains("compatibility.utools.ffmpeg.start"));
        assert!(script.contains("compatibility.utools.ffmpeg."));
        assert!(script.contains("control(\"kill\")"));
        assert!(script.contains("control(\"quit\")"));
        assert!(script.contains("event/utools.ffmpeg.progress"));
        assert!(script.contains("event/utools.ffmpeg.complete"));
        assert!(script.contains("sharp: createSharp"));
        assert!(script.contains("const sandboxModule"));
        assert!(script.contains("getAppVersion"));
        assert!(script.contains("getNativeId"));
        assert!(script.contains("ihub-0123456789abcdef0123456789abcdef"));
        assert!(script.contains("getPath(name)"));
        assert!(script.contains("C:\\\\Users\\\\Tester"));
        assert!(script.contains("getUser() { return null; }"));
        let storage_snapshot = script
            .find("compatibility.utools.dbStorage.snapshot")
            .expect("bootstrap should hydrate the compatibility storage cache");
        let lifecycle_ready = script
            .find("call(\"lifecycle.ready\"")
            .expect("bootstrap should announce readiness");
        assert!(
            storage_snapshot < lifecycle_ready,
            "dbStorage must be hydrated before onPluginReady callbacks run"
        );
        let crypto_storage_snapshot = script
            .find("compatibility.utools.dbCryptoStorage.snapshot")
            .expect("bootstrap should hydrate encrypted compatibility storage");
        assert!(
            crypto_storage_snapshot < lifecycle_ready,
            "dbCryptoStorage must be hydrated before onPluginReady callbacks run"
        );
        let feature_snapshot = script
            .find("compatibility.utools.features.snapshot")
            .expect("bootstrap should hydrate dynamic features");
        assert!(feature_snapshot < lifecycle_ready);
        assert!(script.contains("utools-color-picker"));
        assert!(!script.contains("require("));
        assert!(script.contains("if (name === \"electron\")"));
        assert!(script.contains("Object.freeze({ contextBridge, ipcRenderer })"));
        assert!(!script.contains("require('fs')"));

        let root =
            std::env::temp_dir().join(format!("ihub-utools-bootstrap-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).expect("fixture root should be created");
        let entry = root.join("index.html");
        fs::write(
            &entry,
            "<!doctype html><html><head><script>window.pluginLoaded=true</script></head><body></body></html>",
        )
        .expect("entry should be written");
        let document = String::from_utf8(
            inject_utools_compat_script(&entry, None).expect("entry should receive bootstrap tag"),
        )
        .expect("injected entry should stay UTF-8");
        let bootstrap = document
            .find("__ihub_utools_compat.js")
            .expect("entry should include the host bootstrap");
        let page_script = document
            .find("window.pluginLoaded")
            .expect("fixture page script should remain");
        assert!(
            bootstrap < page_script,
            "the compatibility global must exist before package JavaScript evaluates"
        );
        let browser_document = String::from_utf8(
            inject_utools_compat_script(&entry, Some("preload.js"))
                .expect("BrowserWindow entry should receive its sandboxed preload"),
        )
        .expect("BrowserWindow entry should stay UTF-8");
        let browser_bootstrap = browser_document
            .find("__ihub_utools_compat.js")
            .expect("BrowserWindow host bootstrap");
        let preload = browser_document
            .find("preload.js")
            .expect("BrowserWindow preload tag");
        let browser_page_script = browser_document
            .find("window.pluginLoaded")
            .expect("BrowserWindow page script");
        assert!(browser_bootstrap < preload && preload < browser_page_script);
        let runtime_document = String::from_utf8(inject_utools_compat_document(
            b"<!doctype html><html><head></head><body></body></html>".to_vec(),
            Some(UTOOLS_PRELOAD_SCRIPT_NAME),
        ))
        .expect("synthetic tools runtime should stay UTF-8");
        assert!(runtime_document.contains("__ihub_utools_compat.js"));
        assert!(runtime_document.contains("__ihub_utools_preload.js"));
        assert!(
            runtime_document.find("__ihub_utools_compat.js")
                < runtime_document.find("__ihub_utools_preload.js")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn synchronous_utools_screen_endpoint_is_random_origin_scoped_and_current() {
        let server = PluginAssetServer::new();
        let plugin_id = "utools-sync-screen-test";
        let (root, mut bundle) = temporary_bundle(plugin_id, false);
        bundle.utools_compat = Some(utools_runtime_config(plugin_id));
        let documents = UtoolsDocumentStore::new(root.join("app-data"));
        let lease = server
            .issue_with_utools_documents(bundle, PluginFrontendPurpose::Surface, Some(documents))
            .expect("uTools screen lease should issue");

        let (status, snapshot) = send_sync_screen_request(&lease, "GET");
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert!(snapshot["displays"]
            .as_array()
            .is_some_and(|items| !items.is_empty()));
        assert_eq!(
            snapshot["displays"].as_array().map(Vec::len),
            snapshot["metrics"].as_array().map(Vec::len)
        );
        assert!(snapshot["cursorScreenPoint"]["x"].is_number());
        assert!(snapshot["primaryDisplayId"].is_number());

        let (status, rejection) = send_sync_screen_request(&lease, "POST");
        assert_eq!(status, "HTTP/1.1 400 Bad Request");
        assert!(rejection["error"]
            .as_str()
            .is_some_and(|error| error.contains("only GET")));

        assert_eq!(server.release(&lease.lease_id).as_deref(), Some(plugin_id));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn synchronous_utools_icon_endpoint_is_lease_scoped_and_uses_native_icons() {
        let server = PluginAssetServer::new();
        let plugin_id = "utools-sync-icon-test";
        let (root, mut bundle) = temporary_bundle(plugin_id, false);
        bundle.utools_compat = Some(utools_runtime_config(plugin_id));
        let fixture = root.join("native-icon-fixture.txt");
        fs::write(&fixture, b"iHub native icon fixture").expect("icon fixture should be written");
        let documents = UtoolsDocumentStore::new(root.join("app-data"));
        let lease = server
            .issue_with_utools_documents(bundle, PluginFrontendPurpose::Surface, Some(documents))
            .expect("uTools icon lease should issue");

        let fixture_path = fixture.to_string_lossy().into_owned();
        for request in [".txt", "folder", fixture_path.as_str()] {
            let mut response = send_sync_icon_request(&lease, request, true);
            for _ in 0..3 {
                if response
                    .1
                    .as_str()
                    .is_some_and(|value| value.starts_with("data:image/png;base64,"))
                {
                    break;
                }
                thread::sleep(Duration::from_millis(25));
                response = send_sync_icon_request(&lease, request, true);
            }
            let (status, icon) = response;
            assert_eq!(status, "HTTP/1.1 200 OK");
            assert!(
                icon.as_str()
                    .is_some_and(|value| value.starts_with("data:image/png;base64,")),
                "the synchronous native icon was empty for {request:?}"
            );
        }

        let (status, rejection) = send_sync_icon_request(&lease, ".txt", false);
        assert_eq!(status, "HTTP/1.1 400 Bad Request");
        assert!(rejection["error"]
            .as_str()
            .is_some_and(|error| error.contains("header is missing")));

        assert_eq!(server.release(&lease.lease_id).as_deref(), Some(plugin_id));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn synchronous_utools_dialog_endpoint_requires_a_visible_current_lease() {
        let server = PluginAssetServer::new();
        let seen = Arc::new(Mutex::new(Vec::<super::UtoolsDialogRequest>::new()));
        let callback_seen = Arc::clone(&seen);
        server.set_utools_dialog_handler(Arc::new(move |request| {
            callback_seen
                .lock()
                .expect("dialog requests lock")
                .push(request.clone());
            Ok(if request.kind == "open" {
                serde_json::json!([r"C:\Users\Tester\selected.txt"])
            } else {
                serde_json::Value::Null
            })
        }));

        let plugin_id = "utools-sync-dialog-test";
        let (root, mut bundle) = temporary_bundle(plugin_id, false);
        bundle.utools_compat = Some(utools_runtime_config(plugin_id));
        let documents = UtoolsDocumentStore::new(root.join("app-data"));
        let lease = server
            .issue_with_utools_documents(bundle, PluginFrontendPurpose::Surface, Some(documents))
            .expect("uTools dialog surface lease should issue");
        let request = serde_json::json!({
            "kind": "open",
            "options": { "title": "Choose one" }
        });
        let (status, result) = send_sync_dialog_request(&lease, request.clone(), true);
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert_eq!(result, serde_json::json!([r"C:\Users\Tester\selected.txt"]));
        let observed = seen.lock().expect("dialog requests lock");
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].plugin_id, plugin_id);
        assert_eq!(observed[0].lease_id, lease.lease_id);
        assert_eq!(observed[0].kind, "open");
        assert_eq!(observed[0].options["title"], "Choose one");
        drop(observed);

        let (status, rejection) = send_sync_dialog_request(&lease, request, false);
        assert_eq!(status, "HTTP/1.1 400 Bad Request");
        assert!(rejection["error"]
            .as_str()
            .is_some_and(|error| error.contains("header is missing")));
        assert_eq!(server.release(&lease.lease_id).as_deref(), Some(plugin_id));
        let _ = fs::remove_dir_all(root);

        let runtime_id = "utools-sync-dialog-runtime";
        let (runtime_root, mut runtime_bundle) = temporary_bundle(runtime_id, false);
        runtime_bundle.utools_compat = Some(utools_runtime_config(runtime_id));
        let runtime_documents = UtoolsDocumentStore::new(runtime_root.join("app-data"));
        let runtime_lease = server
            .issue_with_utools_documents(
                runtime_bundle,
                PluginFrontendPurpose::Runtime,
                Some(runtime_documents),
            )
            .expect("uTools dialog runtime lease should issue");
        let (status, _) = send_sync_dialog_request(
            &runtime_lease,
            serde_json::json!({ "kind": "save", "options": {} }),
            true,
        );
        assert_eq!(status, "HTTP/1.1 403 Forbidden");
        assert_eq!(
            server.release(&runtime_lease.lease_id).as_deref(),
            Some(runtime_id)
        );
        let _ = fs::remove_dir_all(runtime_root);
    }

    #[test]
    fn utools_preload_is_not_a_servable_loopback_asset() {
        let root =
            std::env::temp_dir().join(format!("ihub-utools-preload-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).expect("fixture root should be created");
        let entry = root.join("index.html");
        let preload = root.join("preload.js");
        fs::write(&entry, "<main>plugin</main>").expect("entry should be written");
        fs::write(&preload, "require('fs')").expect("preload should be written");
        let asset_root = root.canonicalize().expect("asset root should canonicalize");
        let bundle = ServedBundle {
            plugin_id: "utools-preload-test".to_owned(),
            lease_id: "utools-preload-test-lease".to_owned(),
            purpose: PluginFrontendPurpose::Surface,
            asset_root: asset_root.clone(),
            entry: entry.canonicalize().expect("entry should canonicalize"),
            synthetic_entry: false,
            blocked_asset_paths: vec![preload.canonicalize().expect("preload should canonicalize")],
            route_token: "route-token".to_owned(),
            allows_remote_network: false,
            utools_compat_script: None,
            utools_preload_script: None,
            utools_documents: None,
            utools_browser_preload_src: None,
        };
        assert!(resolve_asset_path(&bundle, "/v1/route-token/").is_some());
        assert!(
            resolve_asset_path(&bundle, "/v1/route-token/preload.js").is_none(),
            "an Electron preload must not become executable browser source"
        );
        let _ = fs::remove_dir_all(root);
    }

    fn fetch_lease_response(lease: &super::PluginFrontendLease) -> String {
        const REQUEST_ATTEMPTS: usize = 3;

        fn peer_closed(error: &std::io::Error) -> bool {
            matches!(
                error.kind(),
                std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
            )
        }

        let url = url::Url::parse(&lease.url).expect("lease URL should parse");
        let host = url.host_str().expect("lease URL should have a host");
        let port = url.port().expect("lease URL should have a port");
        let request = format!(
            "GET {} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n",
            url.path()
        );

        // The production server deliberately drops local clients that stall
        // while sending headers. Build the whole request before connecting and
        // write one buffer so a loaded CI runner cannot turn `write_fmt`
        // fragments into an artificial slow client. Only a peer-close race is
        // retried; every response must still be a complete 200 before the CSP
        // assertion can pass.
        'request: for attempt in 1..=REQUEST_ATTEMPTS {
            let mut stream =
                TcpStream::connect((host, port)).expect("asset listener should accept a request");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("test request should have a read timeout");
            if let Err(error) = stream.write_all(request.as_bytes()) {
                if peer_closed(&error) && attempt < REQUEST_ATTEMPTS {
                    thread::sleep(super::ACCEPT_POLL_INTERVAL);
                    continue;
                }
                panic!("test request attempt {attempt} should be written: {error}");
            }
            let mut response = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let remaining = super::MAX_HTTP_HEADER_BYTES - response.len();
                assert!(
                    remaining > 0,
                    "asset response headers should fit within the server header limit"
                );
                let read_limit = remaining.min(buffer.len());
                match stream.read(&mut buffer[..read_limit]) {
                    Ok(0) if response.is_empty() && attempt < REQUEST_ATTEMPTS => {
                        thread::sleep(super::ACCEPT_POLL_INTERVAL);
                        continue 'request;
                    }
                    Ok(0) => {
                        panic!(
                            "asset response attempt {attempt} ended after {} bytes before a complete header",
                            response.len()
                        );
                    }
                    Ok(read) => {
                        response.extend_from_slice(&buffer[..read]);
                        if let Some(header_end) = response
                            .windows(4)
                            .position(|window| window == b"\r\n\r\n")
                            .map(|index| index + 4)
                        {
                            response.truncate(header_end);
                            break;
                        }
                    }
                    Err(error)
                        if peer_closed(&error)
                            && response.is_empty()
                            && attempt < REQUEST_ATTEMPTS =>
                    {
                        thread::sleep(super::ACCEPT_POLL_INTERVAL);
                        continue 'request;
                    }
                    Err(error) => {
                        panic!(
                            "asset response attempt {attempt} should be readable after {} header bytes: {error}",
                            response.len()
                        );
                    }
                }
            }
            let response = String::from_utf8(response).expect("asset response should be UTF-8");
            assert!(
                response.starts_with("HTTP/1.1 200 OK\r\n"),
                "asset response should be successful, got {}",
                response.lines().next().unwrap_or("an empty response")
            );
            return response;
        }

        unreachable!("the final request attempt either returns or fails explicitly");
    }

    #[test]
    fn http_response_csp_opens_external_network_only_for_declared_bundles() {
        for (allows_remote_network, expected_csp) in
            [(false, LOCKED_PLUGIN_CSP), (true, NETWORKED_PLUGIN_CSP)]
        {
            let server = PluginAssetServer::new();
            let plugin_id = if allows_remote_network {
                "ihub-plugin-networked-csp-test"
            } else {
                "ihub-plugin-locked-csp-test"
            };
            let (root, bundle) = temporary_bundle(plugin_id, allows_remote_network);
            let lease = server
                .issue(bundle, PluginFrontendPurpose::Surface)
                .expect("frontend lease should issue");
            let response = fetch_lease_response(&lease);
            let csp = response
                .lines()
                .find_map(|line| line.strip_prefix("Content-Security-Policy: "))
                .expect("asset response should contain a CSP header");

            assert_eq!(csp, expected_csp);
            if allows_remote_network {
                assert!(csp.contains("connect-src 'self' https: wss:"));
                assert!(csp.contains("img-src 'self' data: blob: https:"));
                assert!(csp.contains("media-src 'self' blob: https:"));
            } else {
                assert!(csp.contains("connect-src 'self'"));
                assert!(csp.contains("img-src 'self' data: blob:;"));
                assert!(csp.contains("media-src 'self' data: blob:;"));
                assert!(!csp.contains("https:"));
                assert!(!csp.contains("wss:"));
            }

            assert_eq!(server.release(&lease.lease_id).as_deref(), Some(plugin_id));
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn unsafe_eval_is_scoped_only_to_browserwindow_documents() {
        let server = PluginAssetServer::new();
        let plugin_id = "ihub-plugin-browser-script-csp";
        let (root, bundle) = temporary_bundle(plugin_id, false);
        let surface = server
            .issue(bundle.clone(), PluginFrontendPurpose::Surface)
            .expect("surface should issue");
        let browser = server
            .issue(bundle, PluginFrontendPurpose::Browser)
            .expect("BrowserWindow should issue");
        let surface_response = fetch_lease_response(&surface);
        let browser_response = fetch_lease_response(&browser);
        assert!(!surface_response.contains("script-src 'self' 'unsafe-eval'"));
        assert!(browser_response.contains("script-src 'self' 'unsafe-eval'"));
        assert_eq!(
            server.release(&surface.lease_id).as_deref(),
            Some(plugin_id)
        );
        assert_eq!(
            server.release(&browser.lease_id).as_deref(),
            Some(plugin_id)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn synchronous_utools_database_endpoint_is_lease_scoped_and_persistent() {
        let server = PluginAssetServer::new();
        let plugin_id = "utools-sync-database-test";
        let (root, mut bundle) = temporary_bundle(plugin_id, false);
        bundle.utools_compat = Some(utools_runtime_config(plugin_id));
        let documents = UtoolsDocumentStore::new(root.join("app-data"));
        let lease = server
            .issue_with_utools_documents(
                bundle,
                PluginFrontendPurpose::Surface,
                Some(documents.clone()),
            )
            .expect("uTools frontend lease should issue");

        let (status, created) = send_sync_database_request(
            &lease,
            &serde_json::json!({
                "op": "put",
                "doc": { "_id": "sync/one", "value": 1 }
            }),
            true,
        );
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert_eq!(created.get("ok"), Some(&serde_json::json!(true)));

        let (status, document) = send_sync_database_request(
            &lease,
            &serde_json::json!({ "op": "get", "id": "sync/one" }),
            true,
        );
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert_eq!(document.get("value"), Some(&serde_json::json!(1)));
        assert!(documents
            .get(plugin_id, "sync/one")
            .expect("read persistent sync document")
            .is_some());

        let (status, attached) = send_sync_database_request(
            &lease,
            &serde_json::json!({
                "op": "postAttachment",
                "id": "sync/asset",
                "dataBase64": "c3luYyBieXRlcw==",
                "contentType": "text/plain"
            }),
            true,
        );
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert_eq!(attached.get("ok"), Some(&serde_json::json!(true)));
        let (status, attachment) = send_sync_database_request(
            &lease,
            &serde_json::json!({ "op": "getAttachment", "id": "sync/asset" }),
            true,
        );
        assert_eq!(status, "HTTP/1.1 200 OK");
        assert_eq!(
            attachment.get("dataBase64"),
            Some(&serde_json::json!("c3luYyBieXRlcw=="))
        );

        let (status, rejection) = send_sync_database_request(
            &lease,
            &serde_json::json!({ "op": "get", "id": "sync/one" }),
            false,
        );
        assert_eq!(status, "HTTP/1.1 400 Bad Request");
        assert!(rejection
            .get("error")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|error| error.contains("header")));

        assert_eq!(server.release(&lease.lease_id).as_deref(), Some(plugin_id));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn synchronous_utools_clipboard_projection_is_bounded_and_shape_exact() {
        let root = std::env::temp_dir().join(format!(
            "ihub-utools-copied-files-test-{}",
            uuid::Uuid::new_v4()
        ));
        let folder = root.join("folder");
        let file = root.join("note.txt");
        fs::create_dir_all(&folder).expect("clipboard fixture folder");
        fs::write(&file, "iHub").expect("clipboard fixture file");

        let mut headers = std::collections::HashMap::new();
        headers.insert(
            super::UTOOLS_SYNC_CLIPBOARD_HEADER.to_owned(),
            "1".to_owned(),
        );
        headers.insert("host".to_owned(), "127.0.0.1:43123".to_owned());
        headers.insert("origin".to_owned(), "http://127.0.0.1:43123".to_owned());
        headers.insert("sec-fetch-site".to_owned(), "same-origin".to_owned());
        let request = HttpRequest {
            method: HttpMethod::Get,
            target: String::new(),
            headers,
            buffered_body: Vec::new(),
        };
        let projection = execute_utools_sync_clipboard_request(request, || {
            Ok(vec![
                file.clone(),
                folder.clone(),
                file.clone(),
                root.join("missing.txt"),
            ])
        })
        .expect("bounded local clipboard paths should project");
        let entries = projection.as_array().expect("copied files array");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["name"], "note.txt");
        assert_eq!(entries[0]["isFile"], true);
        assert_eq!(entries[0]["isDiractory"], false);
        assert_eq!(entries[1]["name"], "folder");
        assert_eq!(entries[1]["isFile"], false);
        assert_eq!(entries[1]["isDiractory"], true);
        assert_eq!(entries[0].as_object().map(|entry| entry.len()), Some(4));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn synchronous_utools_clipboard_endpoint_requires_visible_surface_and_header() {
        let server = PluginAssetServer::new();

        let surface_id = "utools-sync-clipboard-surface";
        let (surface_root, mut surface_bundle) = temporary_bundle(surface_id, false);
        surface_bundle.utools_compat = Some(utools_runtime_config(surface_id));
        let surface_documents = UtoolsDocumentStore::new(surface_root.join("app-data"));
        let surface = server
            .issue_with_utools_documents(
                surface_bundle,
                PluginFrontendPurpose::Surface,
                Some(surface_documents),
            )
            .expect("uTools clipboard surface lease should issue");
        let (status, rejection) = send_sync_clipboard_request(&surface, "GET", false);
        assert_eq!(status, "HTTP/1.1 400 Bad Request");
        assert!(rejection["error"]
            .as_str()
            .is_some_and(|error| error.contains("header is missing")));

        let runtime_id = "utools-sync-clipboard-runtime";
        let (runtime_root, mut runtime_bundle) = temporary_bundle(runtime_id, false);
        runtime_bundle.utools_compat = Some(utools_runtime_config(runtime_id));
        let runtime_documents = UtoolsDocumentStore::new(runtime_root.join("app-data"));
        let runtime = server
            .issue_with_utools_documents(
                runtime_bundle,
                PluginFrontendPurpose::Runtime,
                Some(runtime_documents),
            )
            .expect("uTools clipboard runtime lease should issue");
        let (status, payload) = send_sync_clipboard_request(&runtime, "GET", true);
        assert_eq!(status, "HTTP/1.1 403 Forbidden");
        assert!(payload.is_null());

        assert_eq!(
            server.release(&surface.lease_id).as_deref(),
            Some(surface_id)
        );
        assert_eq!(
            server.release(&runtime.lease_id).as_deref(),
            Some(runtime_id)
        );
        let _ = fs::remove_dir_all(surface_root);
        let _ = fs::remove_dir_all(runtime_root);
    }

    #[test]
    fn a_fresh_lease_replaces_the_same_plugins_old_bridge_session() {
        let server = PluginAssetServer::new();
        let plugin_id = "ihub-plugin-lease-test";
        let (root, bundle) = temporary_bundle(plugin_id, false);
        let first = server
            .issue(bundle.clone(), PluginFrontendPurpose::Surface)
            .expect("first lease should issue");
        assert!(server.is_active_for(&first.lease_id, plugin_id));

        let second = server
            .issue(bundle, PluginFrontendPurpose::Surface)
            .expect("replacement lease should issue");
        assert!(
            !server.is_active_for(&first.lease_id, plugin_id),
            "an obsolete iframe must lose Bridge access before its replacement runs"
        );
        assert!(server.is_active_for(&second.lease_id, plugin_id));

        assert_eq!(server.release(&second.lease_id).as_deref(), Some(plugin_id));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn browser_leases_survive_primary_handoff_and_report_their_purpose() {
        let server = PluginAssetServer::new();
        let plugin_id = "ihub-plugin-browser-lease-test";
        let (root, bundle) = temporary_bundle(plugin_id, false);
        let surface = server
            .issue(bundle.clone(), PluginFrontendPurpose::Surface)
            .expect("surface lease should issue");
        let browser = server
            .issue(bundle.clone(), PluginFrontendPurpose::Browser)
            .expect("browser lease should issue");
        let runtime = server
            .issue(bundle, PluginFrontendPurpose::Runtime)
            .expect("runtime handoff should issue");

        assert!(!server.is_active_for(&surface.lease_id, plugin_id));
        assert!(server.is_active_for(&runtime.lease_id, plugin_id));
        assert!(server.is_active_browser_for(&browser.lease_id, plugin_id));
        let released = server
            .release(&browser.lease_id)
            .expect("browser lease should release");
        assert_eq!(released.plugin_id, plugin_id);
        assert_eq!(released.purpose, PluginFrontendPurpose::Browser);
        assert_eq!(
            server.release(&runtime.lease_id).as_deref(),
            Some(plugin_id)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_hidden_runtime_lease_is_not_a_visible_user_presence_surface() {
        let server = PluginAssetServer::new();
        let plugin_id = "ihub-plugin-runtime-purpose-test";
        let (root, mut bundle) = temporary_bundle(plugin_id, false);
        bundle.allows_display_capture = true;
        let runtime = server
            .issue(bundle, PluginFrontendPurpose::Runtime)
            .expect("runtime lease should issue");

        assert!(server.is_active_for(&runtime.lease_id, plugin_id));
        assert!(
            !runtime.allows_display_capture,
            "a declared screen-capture permission must not survive projection into a hidden runtime lease"
        );
        assert!(
            !server.is_active_surface_for(&runtime.lease_id, plugin_id),
            "a hidden search runtime must never qualify for a visible-only host capability"
        );

        assert_eq!(
            server.release(&runtime.lease_id).as_deref(),
            Some(plugin_id)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn display_capture_delegation_requires_both_manifest_permission_and_surface_purpose() {
        let server = PluginAssetServer::new();

        let undeclared_id = "ihub-plugin-display-capture-undeclared";
        let (undeclared_root, undeclared_bundle) = temporary_bundle(undeclared_id, false);
        let undeclared_surface = server
            .issue(undeclared_bundle, PluginFrontendPurpose::Surface)
            .expect("undeclared surface lease should issue");
        assert!(
            !undeclared_surface.allows_display_capture,
            "a visible surface without the manifest permission must receive no display-capture delegation"
        );

        let declared_id = "ihub-plugin-display-capture-declared";
        let (declared_root, mut declared_bundle) = temporary_bundle(declared_id, false);
        declared_bundle.allows_display_capture = true;
        let declared_surface = server
            .issue(declared_bundle, PluginFrontendPurpose::Surface)
            .expect("declared surface lease should issue");
        assert!(
            declared_surface.allows_display_capture,
            "a visible surface with the validated manifest permission should receive display-capture delegation"
        );

        let runtime_id = "ihub-plugin-display-capture-runtime";
        let (runtime_root, mut runtime_bundle) = temporary_bundle(runtime_id, false);
        runtime_bundle.allows_display_capture = true;
        let declared_runtime = server
            .issue(runtime_bundle, PluginFrontendPurpose::Runtime)
            .expect("declared runtime lease should issue");
        assert!(
            !declared_runtime.allows_display_capture,
            "a hidden runtime must receive no display-capture delegation even when its manifest declares the permission"
        );

        assert_eq!(
            server.release(&undeclared_surface.lease_id).as_deref(),
            Some(undeclared_id)
        );
        assert_eq!(
            server.release(&declared_surface.lease_id).as_deref(),
            Some(declared_id)
        );
        assert_eq!(
            server.release(&declared_runtime.lease_id).as_deref(),
            Some(runtime_id)
        );
        let _ = fs::remove_dir_all(undeclared_root);
        let _ = fs::remove_dir_all(declared_root);
        let _ = fs::remove_dir_all(runtime_root);
    }

    #[test]
    fn microphone_delegation_requires_both_manifest_permission_and_surface_purpose() {
        let server = PluginAssetServer::new();

        let undeclared_id = "ihub-plugin-microphone-undeclared";
        let (undeclared_root, undeclared_bundle) = temporary_bundle(undeclared_id, false);
        let undeclared_surface = server
            .issue(undeclared_bundle, PluginFrontendPurpose::Surface)
            .expect("undeclared surface lease should issue");
        assert!(
            !undeclared_surface.allows_microphone,
            "a visible surface without the manifest permission must receive no microphone delegation"
        );

        let declared_id = "ihub-plugin-microphone-declared";
        let (declared_root, mut declared_bundle) = temporary_bundle(declared_id, false);
        declared_bundle.allows_microphone = true;
        let declared_surface = server
            .issue(declared_bundle, PluginFrontendPurpose::Surface)
            .expect("declared surface lease should issue");
        assert!(
            declared_surface.allows_microphone,
            "a visible surface with the validated manifest permission should receive microphone delegation"
        );

        let runtime_id = "ihub-plugin-microphone-runtime";
        let (runtime_root, mut runtime_bundle) = temporary_bundle(runtime_id, false);
        runtime_bundle.allows_microphone = true;
        let declared_runtime = server
            .issue(runtime_bundle, PluginFrontendPurpose::Runtime)
            .expect("declared runtime lease should issue");
        assert!(
            !declared_runtime.allows_microphone,
            "a hidden runtime must receive no microphone delegation even when its manifest declares the permission"
        );

        assert_eq!(
            server.release(&undeclared_surface.lease_id).as_deref(),
            Some(undeclared_id)
        );
        assert_eq!(
            server.release(&declared_surface.lease_id).as_deref(),
            Some(declared_id)
        );
        assert_eq!(
            server.release(&declared_runtime.lease_id).as_deref(),
            Some(runtime_id)
        );
        let _ = fs::remove_dir_all(undeclared_root);
        let _ = fs::remove_dir_all(declared_root);
        let _ = fs::remove_dir_all(runtime_root);
    }

    #[test]
    fn bridge_operations_for_distinct_plugins_do_not_serialize_each_other() {
        let server = PluginAssetServer::new();
        let (first_entered_sender, first_entered_receiver) = mpsc::channel();
        let (release_first_sender, release_first_receiver) = mpsc::channel();
        let first_server = server.clone();
        let first = thread::spawn(move || {
            first_server.with_plugin_bridge_operation("ihub-plugin-first", || {
                first_entered_sender
                    .send(())
                    .expect("test should observe the first bridge operation");
                release_first_receiver
                    .recv()
                    .expect("test should release the first bridge operation");
            });
        });

        first_entered_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("first bridge operation should start");

        let (second_entered_sender, second_entered_receiver) = mpsc::channel();
        let second_server = server.clone();
        let second = thread::spawn(move || {
            second_server.with_plugin_bridge_operation("ihub-plugin-second", || {
                second_entered_sender
                    .send(())
                    .expect("test should observe the second bridge operation");
            });
        });

        second_entered_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("a bridge operation for another plugin must not wait for the first one");

        release_first_sender
            .send(())
            .expect("first bridge operation should still be waiting");
        first.join().expect("first bridge operation should finish");
        second
            .join()
            .expect("second bridge operation should finish");
    }

    #[test]
    fn native_command_reservations_are_bounded_and_release_on_drop() {
        let server = PluginAssetServer::new();
        let first = server
            .begin_native_command("ihub-plugin-first")
            .expect("first worker should reserve a slot");
        let same_plugin_error = server
            .begin_native_command("ihub-plugin-first")
            .err()
            .expect("one plugin must not run two workers concurrently");
        assert!(same_plugin_error.contains("already has a native command"));

        let second = server
            .begin_native_command("ihub-plugin-second")
            .expect("second plugin should reserve a slot");
        let third = server
            .begin_native_command("ihub-plugin-third")
            .expect("third plugin should reserve a slot");
        let fourth = server
            .begin_native_command("ihub-plugin-fourth")
            .expect("fourth plugin should reserve the last global slot");
        let global_limit_error = server
            .begin_native_command("ihub-plugin-fifth")
            .err()
            .expect("the global worker cap must be enforced");
        assert!(global_limit_error.contains("Too many native plugin commands"));
        assert!(server
            .with_plugin_source_operation(|| Ok::<_, String>(()))
            .expect_err("source changes must fail while a native worker is active")
            .contains("native plugin command"));

        drop(first);
        drop(second);
        drop(third);
        drop(fourth);
        server
            .with_plugin_source_operation(|| Ok::<_, String>(()))
            .expect("dropping each lease must unblock source changes");
    }
}
