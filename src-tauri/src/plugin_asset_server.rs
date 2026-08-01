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

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::plugins::{PluginFrontendAssetBundle, UtoolsCompatRuntimeConfig};

const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(16);
const CONNECTION_READ_TIMEOUT: Duration = Duration::from_millis(250);
const CONNECTION_WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const FRONTEND_LEASE_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const ASSET_STREAM_CHUNK_BYTES: usize = 64 * 1024;
const MAX_HTTP_HEADER_BYTES: usize = 16 * 1024;
const MAX_COMPAT_ENTRY_BYTES: usize = 2 * 1024 * 1024;
const UTOOLS_COMPAT_SCRIPT_NAME: &str = "__ihub_utools_compat.js";
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
}

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
    asset_root: PathBuf,
    entry: PathBuf,
    blocked_asset_paths: Vec<PathBuf>,
    route_token: String,
    allows_remote_network: bool,
    utools_compat_script: Option<Vec<u8>>,
}

#[derive(Clone, Copy)]
enum HttpMethod {
    Get,
    Head,
}

impl PluginAssetServer {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(PluginAssetServerInner {
                leases: Mutex::new(HashMap::new()),
                operation: RwLock::new(()),
                transitions: Mutex::new(HashMap::new()),
                native_commands: Mutex::new(HashMap::new()),
            }),
        }
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

    /// Starts a one-origin server for an already-validated bundle. Only one
    /// lease for a plugin may be active at a time: the renderer intentionally
    /// hands ownership between its hidden search runtime and visible surface.
    /// Replacing the old lease prevents a delayed `lifecycle.dispose` from an
    /// obsolete document from clearing the new runtime's registration.
    ///
    /// The random port is intentional: separate plugins must not share an HTTP
    /// origin, otherwise one iframe could impersonate another over postMessage.
    pub(crate) fn issue(
        &self,
        bundle: PluginFrontendAssetBundle,
        purpose: PluginFrontendPurpose,
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
            blocked_asset_paths,
            allows_display_capture,
            allows_microphone,
            allows_remote_network,
            utools_compat,
        } = bundle;
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
            asset_root,
            entry,
            blocked_asset_paths,
            route_token,
            allows_remote_network,
            utools_compat_script,
        };
        let worker = thread::Builder::new()
            .name("ihub-plugin-assets".to_owned())
            .spawn(move || serve_loop(listener, worker_bundle, worker_shutdown, worker_heartbeat))
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
            let replacement_ids = leases
                .iter()
                .filter(|(_, existing)| existing.plugin_id == plugin_id)
                .map(|(existing_lease_id, _)| existing_lease_id.clone())
                .collect::<Vec<_>>();
            removed.extend(
                replacement_ids
                    .into_iter()
                    .filter_map(|existing_lease_id| leases.remove(&existing_lease_id)),
            );

            if leases.len() >= MAX_ACTIVE_FRONTEND_LEASES {
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
    pub(crate) fn release(&self, lease_id: &str) -> Option<String> {
        let lease = self
            .inner
            .leases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(lease_id);
        if let Some(lease) = lease {
            let plugin_id = lease.plugin_id.clone();
            stop_lease(lease);
            Some(plugin_id)
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
) {
    while !shutdown.load(Ordering::Acquire) && heartbeat_is_fresh(&last_heartbeat) {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_read_timeout(Some(CONNECTION_READ_TIMEOUT));
                let _ = stream.set_write_timeout(Some(CONNECTION_WRITE_TIMEOUT));
                if shutdown.load(Ordering::Acquire) || !heartbeat_is_fresh(&last_heartbeat) {
                    break;
                }
                handle_connection(&mut stream, &bundle, &shutdown, &last_heartbeat);
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
) {
    let Some((method, target)) = read_request(stream).ok().flatten() else {
        let _ = write_status(stream, "400 Bad Request");
        return;
    };
    if shutdown.load(Ordering::Acquire) || !heartbeat_is_fresh(last_heartbeat) {
        return;
    }
    if is_utools_compat_script_request(bundle, &target) {
        let Some(script) = bundle.utools_compat_script.as_deref() else {
            let _ = write_status(stream, "404 Not Found");
            return;
        };
        let _ = serve_memory_asset(
            stream,
            method,
            script,
            "text/javascript; charset=utf-8",
            bundle.allows_remote_network,
        );
        return;
    }
    let Some(path) = resolve_asset_path(bundle, &target) else {
        let _ = write_status(stream, "404 Not Found");
        return;
    };
    // A file can disappear between canonicalization and opening during a
    // local development rebuild. Close the connection without writing a
    // second HTTP status after a partial 200 response.
    let _ = serve_asset(
        stream,
        method,
        &path,
        bundle.allows_remote_network,
        bundle
            .utools_compat_script
            .as_deref()
            .filter(|_| path == bundle.entry),
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

fn read_request(stream: &mut TcpStream) -> io::Result<Option<(HttpMethod, String)>> {
    let mut header = Vec::with_capacity(1024);
    let mut buffer = [0_u8; 1024];
    loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Ok(None);
        }
        header.extend_from_slice(&buffer[..read]);
        if header.len() > MAX_HTTP_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP header is too large",
            ));
        }
        if header.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }

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
    Ok(Some((method, target.to_owned())))
}

fn resolve_asset_path(bundle: &ServedBundle, target: &str) -> Option<PathBuf> {
    let relative = route_relative_path(bundle, target)?;
    if relative.is_empty() {
        return Some(bundle.entry.clone());
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

fn serve_asset(
    stream: &mut TcpStream,
    method: HttpMethod,
    path: &Path,
    allows_remote_network: bool,
    utools_compat_script: Option<&[u8]>,
    shutdown: &AtomicBool,
    last_heartbeat: &Mutex<Instant>,
) -> io::Result<()> {
    if utools_compat_script.is_some() {
        let document = inject_utools_compat_script(path)?;
        return serve_memory_asset(
            stream,
            method,
            &document,
            "text/html; charset=utf-8",
            allows_remote_network,
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
        plugin_csp(allows_remote_network),
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
) -> io::Result<()> {
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: {}\r\nConnection: close\r\n\r\n",
        body.len(),
        plugin_csp(allows_remote_network),
    );
    stream.write_all(header.as_bytes())?;
    if matches!(method, HttpMethod::Get) {
        stream.write_all(body)?;
    }
    Ok(())
}

fn inject_utools_compat_script(entry: &Path) -> io::Result<Vec<u8>> {
    let metadata = entry.metadata()?;
    if metadata.len() > MAX_COMPAT_ENTRY_BYTES as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "uTools-compatible plugin HTML entry exceeds the 2 MiB injection limit",
        ));
    }
    let mut document = Vec::with_capacity(metadata.len() as usize + 96);
    File::open(entry)?.read_to_end(&mut document)?;
    let bootstrap = format!("<script src=\"{UTOOLS_COMPAT_SCRIPT_NAME}\"></script>").into_bytes();
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
    Ok(document)
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
let sequence = 0;
const pending = new Map();
const readyCallbacks = [];
const enterCallbacks = [];
const outCallbacks = [];
const detachCallbacks = [];
let pluginOutDispatched = false;
let pluginDetachDispatched = false;
let subInputChangeCallback = null;
let currentWindowType = "main";
function call(method, params) {{
  const id = "utools-compat-" + (++sequence).toString(36);
  return new Promise((resolve, reject) => {{
    const timeout = window.setTimeout(() => {{ pending.delete(id); reject(new Error("iHub host bridge timed out.")); }}, 15000);
    pending.set(id, {{ resolve, reject, timeout }});
    window.parent.postMessage({{ channel: requestChannel, type: "call", id, request: {{ pluginId: config.pluginId, method, params: params || {{}} }} }}, "*");
  }});
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
  if (message.name === "ihub://plugin/" + config.pluginId + "/event/subInput.change") {{
    if (typeof subInputChangeCallback === "function") {{
      const text = message.payload && typeof message.payload.text === "string" ? message.payload.text : "";
      try {{ subInputChangeCallback({{ text }}); }} catch (error) {{ console.error("uTools compatibility sub-input callback failed", error); }}
    }}
    return;
  }}
  if (message.name === "ihub://plugin/" + config.pluginId + "/event/utools.windowType") {{
    const value = message.payload && message.payload.windowType;
    if (value === "main" || value === "detach") {{
      currentWindowType = value;
      if (value === "detach") invokePluginDetach();
    }}
    return;
  }}
  if (message.name !== "ihub://plugin/" + config.pluginId + "/command") return;
  const commandId = message.payload && message.payload.commandId;
  const command = config.commands.find((candidate) => candidate.commandId === commandId)
    || Array.from(dynamicFeatures.values()).find((candidate) => candidate.commandId === commandId);
  if (!command) return;
  const input = message.payload && message.payload.input;
  invoke(enterCallbacks, {{ code: command.code, type: "text", payload: typeof input === "string" ? input : "", from: "main" }});
}});
const utools = Object.freeze({{
  dbStorage,
  onPluginReady(callback) {{ if (typeof callback === "function") readyCallbacks.push(callback); }},
  onPluginEnter(callback) {{ if (typeof callback === "function") enterCallbacks.push(callback); }},
  onPluginOut(callback) {{ if (typeof callback === "function") outCallbacks.push(callback); }},
  onPluginDetach(callback) {{
    if (typeof callback !== "function") return;
    if (pluginDetachDispatched) {{
      try {{ callback(); }} catch (error) {{ console.error("uTools compatibility detach callback failed", error); }}
      return;
    }}
    detachCallbacks.push(callback);
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
    void call("compatibility.utools.features.set", {{ feature: publicDynamicFeature(feature) }}).catch((error) => {{
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
    void call("compatibility.utools.input.pasteText", {{ value }})
      .catch((error) => console.error("iHub compatibility text paste failed", error));
    return true;
  }},
  hideMainWindowTypeString(value) {{
    if (typeof value !== "string" || Array.from(value).length > 4096 || value.includes("\u0000")) return false;
    void call("compatibility.utools.input.typeString", {{ value }})
      .catch((error) => console.error("iHub compatibility text input failed", error));
    return true;
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
  copyText(value) {{
    if (typeof value !== "string" || new TextEncoder().encode(value).byteLength > 49152) return false;
    void call("compatibility.utools.clipboard.writeText", {{ value }})
      .catch((error) => console.error("iHub compatibility clipboard write failed", error));
    return true;
  }},
  copyImage(value) {{
    const dataUrl = pngDataUrlForCopyImage(value);
    if (!dataUrl) return false;
    void call("compatibility.utools.clipboard.writeImage", {{ dataUrl }})
      .catch((error) => console.error("iHub compatibility image copy failed", error));
    return true;
  }},
  copyFile(value) {{
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
    void call("compatibility.utools.clipboard.writeFiles", {{ paths: normalized }})
      .catch((error) => console.error("iHub compatibility file copy failed", error));
    return true;
  }},
  showNotification(body, clickFeatureCode) {{
    if (typeof body !== "string") return;
    const trimmedBody = body.trim();
    if (trimmedBody.length === 0 || Array.from(trimmedBody).length > 1000) return;
    if (clickFeatureCode !== undefined) {{
      console.error("iHub compatibility notification click routing is not supported yet.");
      return;
    }}
    void call("compatibility.utools.notification.show", {{ body: trimmedBody }})
      .catch((error) => console.error("iHub compatibility notification failed", error));
  }},
  shellOpenExternal(url) {{
    if (typeof url !== "string" || url.length === 0 || Array.from(url).length > 2048 || /[\u0000-\u001f\u007f]/.test(url)) return;
    void call("compatibility.utools.shell.openExternal", {{ url }})
      .catch((error) => console.error("iHub compatibility external URL failed", error));
  }},
  shellBeep() {{
    void call("compatibility.utools.shell.beep", {{}})
      .catch((error) => console.error("iHub compatibility system beep failed", error));
  }},
  screenColorPick(callback) {{
    if (typeof callback !== "function") return;
    void call("cursorColor.sampleOnce", {{}}).then((color) => callback(color)).catch((error) => console.error("iHub compatibility color pick failed", error));
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
  isDev() {{ return false; }},
  isDarkColors() {{ return typeof window.matchMedia === "function" && window.matchMedia("(prefers-color-scheme: dark)").matches; }},
  isWindows() {{ return /\\bwindows?\\b|\\bwin(?:32|64)\\b/.test((navigator.platform + " " + navigator.userAgent).toLowerCase()); }},
  isMacOS() {{ const platform = (navigator.platform + " " + navigator.userAgent).toLowerCase(); return platform.includes("mac") || platform.includes("darwin"); }},
  isLinux() {{ return (navigator.platform + " " + navigator.userAgent).toLowerCase().includes("linux"); }}
}});
Object.defineProperties(window, {{
  utools: {{ value: utools, configurable: false, writable: false }},
  rubick: {{ value: utools, configurable: false, writable: false }}
}});
Promise.all([
  call("compatibility.utools.dbStorage.snapshot", {{}}),
  call("compatibility.utools.features.snapshot", {{}})
])
  .then(([snapshot, features]) => {{
    if (snapshot && typeof snapshot === "object" && !Array.isArray(snapshot)) {{
      for (const [key, value] of Object.entries(snapshot)) {{
        if (!dbStorageVersions.has(key)) dbStorageState[key] = value;
      }}
    }}
    if (Array.isArray(features)) {{
      for (const value of features) {{
        const feature = normalizeDynamicFeature(value);
        if (feature && !dynamicFeatureVersions.has(feature.code)) dynamicFeatures.set(feature.code, feature);
      }}
    }}
  }})
  .catch((error) => console.error("iHub compatibility dbStorage restore failed", error))
  .then(() => call("lifecycle.ready", {{}}))
  .then(() => invoke(readyCallbacks, undefined))
  .catch((error) => console.error("iHub uTools compatibility bootstrap failed", error));
window.addEventListener("pagehide", () => {{ invokePluginOut(false); void call("lifecycle.dispose", {{}}).catch(() => undefined); }}, {{ once: true }});
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

fn plugin_csp(allows_remote_network: bool) -> &'static str {
    if allows_remote_network {
        NETWORKED_PLUGIN_CSP
    } else {
        LOCKED_PLUGIN_CSP
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
        sync::mpsc,
        thread,
        time::Duration,
    };

    use super::{
        inject_utools_compat_script, render_utools_compat_script, resolve_asset_path,
        PluginAssetServer, PluginFrontendAssetBundle, PluginFrontendPurpose, ServedBundle,
        LOCKED_PLUGIN_CSP, NETWORKED_PLUGIN_CSP,
    };
    use crate::plugins::{UtoolsCompatCommand, UtoolsCompatRuntimeConfig};

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
                blocked_asset_paths: Vec::new(),
                allows_display_capture: false,
                allows_microphone: false,
                allows_remote_network,
                utools_compat: None,
            },
        )
    }

    #[test]
    fn utools_bootstrap_is_host_owned_and_precedes_page_scripts() {
        let config = UtoolsCompatRuntimeConfig {
            app_version: "0.1.0".to_owned(),
            plugin_id: "utools-color-picker".to_owned(),
            commands: vec![UtoolsCompatCommand {
                command_id: "utools-feature-1".to_owned(),
                code: "pick-color".to_owned(),
            }],
            native_id: "ihub-0123456789abcdef0123456789abcdef".to_owned(),
            paths: [("home".to_owned(), "C:\\Users\\Tester".to_owned())]
                .into_iter()
                .collect(),
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
        assert!(script.contains("value instanceof Uint8Array"));
        assert!(script.contains("compatibility.utools.clipboard.writeImage"));
        assert!(script.contains("copyFile"));
        assert!(script.contains("compatibility.utools.clipboard.writeFiles"));
        assert!(script.contains("showNotification"));
        assert!(script.contains("Array.from(trimmedBody).length > 1000"));
        assert!(script.contains("compatibility.utools.notification.show"));
        assert!(script.contains("shellOpenExternal"));
        assert!(script.contains("compatibility.utools.shell.openExternal"));
        assert!(script.contains("shellBeep"));
        assert!(script.contains("compatibility.utools.shell.beep"));
        assert!(script.contains("screenColorPick"));
        assert!(script.contains("onPluginDetach"));
        assert!(script.contains("invokePluginDetach"));
        assert!(script.contains("cursorColor.sampleOnce"));
        assert!(script.contains("dbStorage"));
        assert!(script.contains("compatibility.utools.dbStorage.set"));
        assert!(script.contains("compatibility.utools.dbStorage.remove"));
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
        assert!(script.contains("hideMainWindowTypeString"));
        assert!(script.contains("compatibility.utools.input.typeString"));
        assert!(script.contains("setSubInput"));
        assert!(script.contains("subInputSelect"));
        assert!(script.contains("compatibility.utools.window.hideMain"));
        assert!(script.contains("setExpendHeight"));
        assert!(script.contains("compatibility.utools.window.setHeight"));
        assert!(script.contains("compatibility.utools.window.outPlugin"));
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
        let feature_snapshot = script
            .find("compatibility.utools.features.snapshot")
            .expect("bootstrap should hydrate dynamic features");
        assert!(feature_snapshot < lifecycle_ready);
        assert!(script.contains("utools-color-picker"));
        assert!(!script.contains("require("));
        assert!(!script.contains("electron"));

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
            inject_utools_compat_script(&entry).expect("entry should receive bootstrap tag"),
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
        let _ = fs::remove_dir_all(root);
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
            asset_root: asset_root.clone(),
            entry: entry.canonicalize().expect("entry should canonicalize"),
            blocked_asset_paths: vec![preload.canonicalize().expect("preload should canonicalize")],
            route_token: "route-token".to_owned(),
            allows_remote_network: false,
            utools_compat_script: None,
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
