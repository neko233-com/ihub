//! Opt-in global long-right-click gesture for the dTools-style Super Panel.
//!
//! The listener observes only secondary-button and pointer-location events. It
//! never injects keystrokes, replaces the clipboard, suppresses another
//! application's input, or exposes a general-purpose hook to plugins. The host
//! decides whether a trigger may reveal iHub and which bounded clipboard
//! context, if any, can be consumed.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

pub(crate) const SUPER_PANEL_HOLD_MS: u64 = 460;
const SUPER_PANEL_MOVE_TOLERANCE_PHYSICAL_PX: i32 = 10;
const DETECTOR_POLL_INTERVAL: Duration = Duration::from_millis(20);
const LISTENER_START_TIMEOUT: Duration = Duration::from_secs(2);
const LISTENER_STOP_TIMEOUT: Duration = Duration::from_secs(2);
const SUPER_PANEL_CONTEXT_TTL: Duration = Duration::from_secs(8);
const SUPER_PANEL_PREFERENCE_SCHEMA_VERSION: u8 = 1;
const SUPER_PANEL_PREFERENCE_MAX_BYTES: u64 = 1_024;
const SUPER_PANEL_PREFERENCE_FILE: &str = "super-panel.json";

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedSuperPanelPreference {
    schema_version: u8,
    enabled: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SuperPanelTrigger {
    pub physical_x: i32,
    pub physical_y: i32,
    pub held_ms: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SuperPanelStatus {
    pub enabled: bool,
    pub listener_running: bool,
    pub hold_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SuperPanelEvent {
    pub context_token: String,
    pub physical_x: i32,
    pub physical_y: i32,
    pub expires_in_ms: u64,
}

#[derive(Clone, Debug)]
struct PendingContext {
    token: String,
    expires_at: Instant,
}

pub(crate) struct SuperPanelState {
    enabled: AtomicBool,
    listener_accepting: AtomicBool,
    listener: Mutex<Option<GlobalRightHoldListener>>,
    last_error: Mutex<Option<String>>,
    pending: Mutex<Option<PendingContext>>,
    preference_path: Option<PathBuf>,
}

impl Default for SuperPanelState {
    fn default() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            listener_accepting: AtomicBool::new(false),
            listener: Mutex::new(None),
            last_error: Mutex::new(None),
            pending: Mutex::new(None),
            preference_path: None,
        }
    }
}

impl SuperPanelState {
    pub(crate) fn with_storage(app_data_dir: PathBuf) -> Self {
        let preference_path = app_data_dir.join(SUPER_PANEL_PREFERENCE_FILE);
        let mut state = Self::default();
        state.preference_path = Some(preference_path.clone());
        match load_preference(&preference_path) {
            Ok(Some(enabled)) => state.enabled.store(enabled, Ordering::Release),
            Ok(None) => {}
            Err(error) => {
                *state
                    .last_error
                    .get_mut()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error);
            }
        }
        state
    }

    pub(crate) fn status(&self) -> SuperPanelStatus {
        let listener_running = self
            .listener
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .is_some_and(GlobalRightHoldListener::is_running);
        SuperPanelStatus {
            enabled: self.enabled.load(Ordering::Acquire),
            listener_running,
            hold_ms: SUPER_PANEL_HOLD_MS,
            error: self
                .last_error
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone(),
        }
    }

    pub(crate) fn set_enabled(&self, enabled: bool) -> Result<(), String> {
        self.enabled.store(enabled, Ordering::Release);
        if !enabled {
            *self
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
            return self.stop_listener();
        }
        Ok(())
    }

    pub(crate) fn set_enabled_persisted(&self, enabled: bool) -> Result<(), String> {
        if enabled {
            // Enabling is persisted before the native listener starts. A
            // storage failure therefore cannot orphan an active OS hook.
            if let Some(path) = self.preference_path.as_deref() {
                persist_preference(path, true)?;
            }
            return self.set_enabled(true);
        }

        // Runtime cancellation is fail-safe and must happen even when the
        // preference file cannot be updated. Attempt both operations and
        // return every relevant error to the caller.
        let stop_result = self.set_enabled(false);
        let persist_result = self
            .preference_path
            .as_deref()
            .map(|path| persist_preference(path, false))
            .unwrap_or(Ok(()));
        combine_results(stop_result, persist_result)
    }

    pub(crate) fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    pub(crate) fn ensure_listener(
        &self,
        on_trigger: impl Fn(SuperPanelTrigger) + Send + Sync + 'static,
    ) -> Result<(), String> {
        if !self.enabled() {
            return Err("The Super Panel listener cannot start while it is disabled.".to_owned());
        }
        self.ensure_listener_with(|| start_global_right_hold_listener(on_trigger))
    }

    pub(crate) fn listener_failed(&self, error: String) {
        let stop_error = self.set_enabled(false).err();
        if let Some(path) = self.preference_path.as_deref() {
            if let Err(persist_error) = persist_preference(path, false) {
                eprintln!(
                    "iHub could not persist the disabled Super Panel state after listener failure: {persist_error}"
                );
            }
        }
        *self
            .last_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(match stop_error {
            Some(stop_error) => format!("{error} Listener cleanup also failed: {stop_error}"),
            None => error,
        });
    }

    pub(crate) fn shutdown_listener(&self) -> Result<(), String> {
        self.listener_accepting.store(false, Ordering::Release);
        *self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        self.stop_listener()
    }

    fn ensure_listener_with(
        &self,
        start: impl FnOnce() -> Result<GlobalRightHoldListener, ListenerStartFailure>,
    ) -> Result<(), String> {
        let mut slot = self
            .listener
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot
            .as_ref()
            .is_some_and(GlobalRightHoldListener::is_usable)
        {
            self.listener_accepting.store(true, Ordering::Release);
            return Ok(());
        }
        if let Some(stale) = slot.as_mut() {
            stale.stop()?;
            *slot = None;
        }
        match start() {
            Ok(listener) => {
                *slot = Some(listener);
                self.listener_accepting.store(true, Ordering::Release);
                *self
                    .last_error
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
                Ok(())
            }
            Err(failure) => {
                let error = failure.message;
                *slot = failure.listener;
                *self
                    .last_error
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error.clone());
                Err(error)
            }
        }
    }

    fn stop_listener(&self) -> Result<(), String> {
        // Close the callback gate before waiting for either native thread.
        // A trigger already entering `issue_context` now rejects without
        // acquiring the lifecycle mutex, so disable cannot deadlock against
        // the detector callback it is joining.
        self.listener_accepting.store(false, Ordering::Release);
        let mut slot = self
            .listener
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(listener) = slot.as_mut() else {
            return Ok(());
        };
        match listener.stop() {
            Ok(()) => {
                *slot = None;
                Ok(())
            }
            Err(error) => {
                *self
                    .last_error
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error.clone());
                Err(error)
            }
        }
    }

    pub(crate) fn issue_context(&self, trigger: SuperPanelTrigger) -> Option<SuperPanelEvent> {
        if !self.enabled() || !self.listener_accepting.load(Ordering::Acquire) {
            return None;
        }
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Close the race where disable revoked the previous token after this
        // callback's optimistic gate read but before it acquired `pending`.
        if !self.enabled() || !self.listener_accepting.load(Ordering::Acquire) {
            return None;
        }
        let token = uuid::Uuid::new_v4().to_string();
        *pending = Some(PendingContext {
            token: token.clone(),
            expires_at: Instant::now() + SUPER_PANEL_CONTEXT_TTL,
        });
        Some(SuperPanelEvent {
            context_token: token,
            physical_x: trigger.physical_x,
            physical_y: trigger.physical_y,
            expires_in_ms: SUPER_PANEL_CONTEXT_TTL.as_millis() as u64,
        })
    }

    pub(crate) fn consume_context(&self, token: &str) -> Result<(), String> {
        if token.len() > 128 || uuid::Uuid::parse_str(token).is_err() {
            return Err("The Super Panel context token is invalid.".to_owned());
        }
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(context) = pending.take() else {
            return Err("The Super Panel context is no longer available.".to_owned());
        };
        if context.expires_at <= Instant::now() {
            return Err("The Super Panel context expired.".to_owned());
        }
        if context.token != token {
            // A wrong token must not consume the real pending gesture.
            *pending = Some(context);
            return Err("The Super Panel context token does not match.".to_owned());
        }
        Ok(())
    }
}

impl Drop for SuperPanelState {
    fn drop(&mut self) {
        if let Err(error) = self.stop_listener() {
            eprintln!("iHub could not stop the Super Panel listener during shutdown: {error}");
        }
    }
}

fn combine_results(first: Result<(), String>, second: Result<(), String>) -> Result<(), String> {
    match (first, second) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(first), Err(second)) => Err(format!("{first} {second}")),
    }
}

fn load_preference(path: &Path) -> Result<Option<bool>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "Could not inspect the Super Panel preference: {error}"
            ))
        }
    };
    if !metadata.file_type().is_file() || metadata.len() > SUPER_PANEL_PREFERENCE_MAX_BYTES {
        return Err("The Super Panel preference is not a bounded regular file.".to_owned());
    }
    let encoded = fs::read(path)
        .map_err(|error| format!("Could not read the Super Panel preference: {error}"))?;
    if encoded.len() as u64 > SUPER_PANEL_PREFERENCE_MAX_BYTES {
        return Err("The Super Panel preference exceeds its local size limit.".to_owned());
    }
    let preference: PersistedSuperPanelPreference = serde_json::from_slice(&encoded)
        .map_err(|error| format!("Could not parse the Super Panel preference: {error}"))?;
    if preference.schema_version != SUPER_PANEL_PREFERENCE_SCHEMA_VERSION {
        return Err("The Super Panel preference version is unsupported.".to_owned());
    }
    Ok(Some(preference.enabled))
}

fn persist_preference(path: &Path, enabled: bool) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Could not determine the Super Panel preference directory.".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| {
        format!("Could not create the Super Panel preference directory: {error}")
    })?;
    let encoded = serde_json::to_vec_pretty(&PersistedSuperPanelPreference {
        schema_version: SUPER_PANEL_PREFERENCE_SCHEMA_VERSION,
        enabled,
    })
    .map_err(|error| format!("Could not encode the Super Panel preference: {error}"))?;
    if encoded.len() as u64 > SUPER_PANEL_PREFERENCE_MAX_BYTES {
        return Err("The Super Panel preference exceeds its local size limit.".to_owned());
    }

    let temporary = parent.join(format!(
        ".super-panel-{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    fs::write(&temporary, &encoded)
        .map_err(|error| format!("Could not stage the Super Panel preference: {error}"))?;
    if !path_entry_exists(path)? {
        return fs::rename(&temporary, path).map_err(|error| {
            let _ = fs::remove_file(&temporary);
            format!("Could not save the Super Panel preference: {error}")
        });
    }

    let existing = fs::symlink_metadata(path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        format!("Could not inspect the existing Super Panel preference: {error}")
    })?;
    if !existing.file_type().is_file() {
        let _ = fs::remove_file(&temporary);
        return Err("The existing Super Panel preference is not a regular file.".to_owned());
    }

    let backup = parent.join(format!(
        ".super-panel-{}.bak",
        uuid::Uuid::new_v4().simple()
    ));
    fs::rename(path, &backup).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        format!("Could not prepare the Super Panel preference update: {error}")
    })?;
    if let Err(error) = fs::rename(&temporary, path) {
        let restore = fs::rename(&backup, path);
        let _ = fs::remove_file(&temporary);
        return Err(match restore {
            Ok(()) => format!("Could not save the Super Panel preference: {error}"),
            Err(restore_error) => format!(
                "Could not save the Super Panel preference ({error}) or restore the previous file ({restore_error})."
            ),
        });
    }
    if let Err(error) = fs::remove_file(&backup) {
        eprintln!("iHub could not remove an old Super Panel preference backup: {error}");
    }
    Ok(())
}

fn path_entry_exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "Could not inspect the Super Panel preference path: {error}"
        )),
    }
}

#[derive(Clone, Copy, Debug)]
enum RawPointerEvent {
    Down {
        point: (i32, i32),
        observed_at: Instant,
    },
    Move {
        point: (i32, i32),
        observed_at: Instant,
    },
    Up {
        observed_at: Instant,
    },
}

#[derive(Clone, Copy, Debug)]
struct PendingHold {
    point: (i32, i32),
    started_at: Instant,
    last_observed_at: Instant,
    cancelled: bool,
    fired: bool,
}

#[derive(Default)]
struct RightHoldDetector {
    pending: Option<PendingHold>,
}

impl RightHoldDetector {
    fn observe(&mut self, event: RawPointerEvent) -> Option<SuperPanelTrigger> {
        match event {
            RawPointerEvent::Down { point, observed_at } => {
                self.pending = Some(PendingHold {
                    point,
                    started_at: observed_at,
                    last_observed_at: observed_at,
                    cancelled: false,
                    fired: false,
                });
                None
            }
            RawPointerEvent::Move { point, observed_at } => {
                let pending = self.pending.as_mut()?;
                pending.last_observed_at = observed_at;
                let dx = i64::from(point.0) - i64::from(pending.point.0);
                let dy = i64::from(point.1) - i64::from(pending.point.1);
                let tolerance = i64::from(SUPER_PANEL_MOVE_TOLERANCE_PHYSICAL_PX);
                if dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy))
                    > tolerance.saturating_mul(tolerance)
                {
                    pending.cancelled = true;
                }
                self.poll(observed_at)
            }
            RawPointerEvent::Up { observed_at } => {
                let trigger = self.poll(observed_at);
                self.pending = None;
                trigger
            }
        }
    }

    fn poll(&mut self, observed_at: Instant) -> Option<SuperPanelTrigger> {
        let pending = self.pending.as_mut()?;
        pending.last_observed_at = pending.last_observed_at.max(observed_at);
        if pending.cancelled || pending.fired {
            return None;
        }
        let held = observed_at.saturating_duration_since(pending.started_at);
        if held < Duration::from_millis(SUPER_PANEL_HOLD_MS) {
            return None;
        }
        pending.fired = true;
        Some(SuperPanelTrigger {
            physical_x: pending.point.0,
            physical_y: pending.point.1,
            held_ms: held.as_millis().min(u128::from(u64::MAX)) as u64,
        })
    }
}

fn run_detector(
    receiver: mpsc::Receiver<RawPointerEvent>,
    on_trigger: Arc<dyn Fn(SuperPanelTrigger) + Send + Sync>,
    stop_requested: Arc<AtomicBool>,
) {
    let mut detector = RightHoldDetector::default();
    while !stop_requested.load(Ordering::Acquire) {
        match receiver.recv_timeout(DETECTOR_POLL_INTERVAL) {
            Ok(event) => {
                if let Some(trigger) = detector.observe(event) {
                    if !stop_requested.load(Ordering::Acquire) {
                        on_trigger(trigger);
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Some(trigger) = detector.poll(Instant::now()) {
                    if !stop_requested.load(Ordering::Acquire) {
                        on_trigger(trigger);
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

trait ListenerControl: Send {
    fn is_running(&self) -> bool;
    fn is_usable(&self) -> bool;
    fn stop(&mut self) -> Result<(), String>;
}

struct GlobalRightHoldListener {
    control: Box<dyn ListenerControl>,
    stopped: bool,
}

impl GlobalRightHoldListener {
    fn is_running(&self) -> bool {
        self.control.is_running()
    }

    fn is_usable(&self) -> bool {
        self.control.is_usable()
    }

    fn stop(&mut self) -> Result<(), String> {
        if self.stopped {
            return Ok(());
        }
        let result = self.control.stop();
        if result.is_ok() {
            self.stopped = true;
        }
        result
    }
}

impl Drop for GlobalRightHoldListener {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            eprintln!("iHub could not finish stopping a Super Panel listener: {error}");
        }
    }
}

struct ListenerStartFailure {
    message: String,
    /// A partially started session is retained when bounded cleanup could not
    /// prove that every native resource exited. `SuperPanelState` stores it
    /// with callbacks gated off so status remains honest and a later retry can
    /// finish cleanup instead of orphaning a hook.
    listener: Option<GlobalRightHoldListener>,
}

impl ListenerStartFailure {
    fn clean(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            listener: None,
        }
    }
}

struct NativeListenerControl {
    active: Arc<AtomicBool>,
    stop_requested: Arc<AtomicBool>,
    detector_running: Arc<AtomicBool>,
    detector_done: mpsc::Receiver<Result<(), String>>,
    detector_thread: Option<JoinHandle<()>>,
    platform: Option<platform::Listener>,
}

impl ListenerControl for NativeListenerControl {
    fn is_running(&self) -> bool {
        self.platform
            .as_ref()
            .is_some_and(platform::Listener::is_running)
    }

    fn is_usable(&self) -> bool {
        self.active.load(Ordering::Acquire)
            && !self.stop_requested.load(Ordering::Acquire)
            && self.detector_running.load(Ordering::Acquire)
            && self
                .platform
                .as_ref()
                .is_some_and(platform::Listener::is_running)
    }

    fn stop(&mut self) -> Result<(), String> {
        self.active.store(false, Ordering::Release);
        self.stop_requested.store(true, Ordering::Release);
        let platform_result = self
            .platform
            .as_mut()
            .map(platform::Listener::stop)
            .unwrap_or(Ok(()));
        let detector_result = wait_for_listener_thread(
            "Super Panel detector",
            &self.detector_done,
            &mut self.detector_thread,
        );
        combine_results(platform_result, detector_result)
    }
}

fn wait_for_listener_thread(
    label: &str,
    completed: &mpsc::Receiver<Result<(), String>>,
    thread: &mut Option<JoinHandle<()>>,
) -> Result<(), String> {
    if thread.is_none() {
        return Ok(());
    }
    let thread_result = match completed.recv_timeout(LISTENER_STOP_TIMEOUT) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            return Err(format!(
                "{label} did not stop within {} ms.",
                LISTENER_STOP_TIMEOUT.as_millis()
            ))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(format!("{label} ended without a cleanup result."))
        }
    };
    let join_result = thread
        .take()
        .expect("checked listener thread")
        .join()
        .map_err(|_| format!("{label} panicked while stopping."));
    combine_results(thread_result, join_result)
}

fn start_global_right_hold_listener(
    on_trigger: impl Fn(SuperPanelTrigger) + Send + Sync + 'static,
) -> Result<GlobalRightHoldListener, ListenerStartFailure> {
    let (sender, receiver) = mpsc::sync_channel(64);
    let on_trigger: Arc<dyn Fn(SuperPanelTrigger) + Send + Sync> = Arc::new(on_trigger);
    let active = Arc::new(AtomicBool::new(true));
    let stop_requested = Arc::new(AtomicBool::new(false));
    let detector_running = Arc::new(AtomicBool::new(true));
    let detector_stop = Arc::clone(&stop_requested);
    let detector_alive = Arc::clone(&detector_running);
    let (detector_done_sender, detector_done) = mpsc::sync_channel(1);
    let detector_thread = std::thread::Builder::new()
        .name("ihub-super-panel-detector".to_owned())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_detector(receiver, on_trigger, detector_stop);
            }))
            .map_err(|_| "The Super Panel detector panicked.".to_owned());
            detector_alive.store(false, Ordering::Release);
            let _ = detector_done_sender.try_send(result);
        })
        .map_err(|error| {
            ListenerStartFailure::clean(format!("Could not start Super Panel detector: {error}"))
        })?;
    let platform = match platform::start(sender) {
        Ok(platform) => platform,
        Err(mut failure) => {
            stop_requested.store(true, Ordering::Release);
            let mut detector_thread = Some(detector_thread);
            let cleanup = wait_for_listener_thread(
                "Super Panel detector",
                &detector_done,
                &mut detector_thread,
            );
            let message = match cleanup {
                Ok(()) => failure.message,
                Err(cleanup) => {
                    format!(
                        "{} Detector cleanup also failed: {cleanup}",
                        failure.message
                    )
                }
            };
            let listener = if failure.listener.is_some() || detector_thread.is_some() {
                Some(GlobalRightHoldListener {
                    control: Box::new(NativeListenerControl {
                        active: Arc::new(AtomicBool::new(false)),
                        stop_requested,
                        detector_running,
                        detector_done,
                        detector_thread,
                        platform: failure.listener.take(),
                    }),
                    stopped: false,
                })
            } else {
                None
            };
            return Err(ListenerStartFailure { message, listener });
        }
    };
    Ok(GlobalRightHoldListener {
        control: Box::new(NativeListenerControl {
            active,
            stop_requested,
            detector_running,
            detector_done,
            detector_thread: Some(detector_thread),
            platform: Some(platform),
        }),
        stopped: false,
    })
}

#[cfg(target_os = "windows")]
mod platform {
    use std::{
        mem, ptr,
        sync::{
            atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
            mpsc, Arc, Mutex, OnceLock,
        },
        thread::JoinHandle,
        time::Instant,
    };

    use windows_sys::Win32::{
        Foundation::{LPARAM, LRESULT, WPARAM},
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
        UI::WindowsAndMessaging::{
            CallNextHookEx, DispatchMessageW, GetMessageW, PeekMessageW, PostThreadMessageW,
            SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, MSG, MSLLHOOKSTRUCT,
            PM_NOREMOVE, WH_MOUSE_LL, WM_MOUSEMOVE, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP,
        },
    };

    use super::{wait_for_listener_thread, RawPointerEvent, LISTENER_START_TIMEOUT};

    struct PointerEventSink {
        listener_id: usize,
        sender: mpsc::SyncSender<RawPointerEvent>,
    }

    static POINTER_EVENTS: OnceLock<Mutex<Option<PointerEventSink>>> = OnceLock::new();
    static NEXT_LISTENER_ID: AtomicUsize = AtomicUsize::new(1);

    pub(super) struct Listener {
        listener_id: usize,
        thread_id: Arc<AtomicU32>,
        hook: Arc<Mutex<Option<isize>>>,
        stop_requested: Arc<AtomicBool>,
        running: Arc<AtomicBool>,
        completed: mpsc::Receiver<Result<(), String>>,
        thread: Option<JoinHandle<()>>,
    }

    pub(super) struct StartFailure {
        pub(super) message: String,
        pub(super) listener: Option<Listener>,
    }

    impl Listener {
        pub(super) fn is_running(&self) -> bool {
            self.running.load(Ordering::Acquire)
                && self
                    .hook
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .is_some()
        }

        pub(super) fn stop(&mut self) -> Result<(), String> {
            self.stop_requested.store(true, Ordering::Release);
            clear_pointer_sender(self.listener_id);

            // Unhook synchronously on the disabling caller. This is the
            // fail-safe boundary: even if WM_QUIT cannot wake the hook thread,
            // Windows no longer observes global pointer messages.
            let unhook_result = unhook_once(&self.hook);
            let thread_id = self.thread_id.load(Ordering::Acquire);
            let wake_result = if self.running.load(Ordering::Acquire) && thread_id != 0 {
                // SAFETY: the hook thread creates its message queue before it
                // reports readiness, and WM_QUIT contains no borrowed data.
                let posted = unsafe { PostThreadMessageW(thread_id, WM_QUIT, 0, 0) };
                if posted == 0 && self.running.load(Ordering::Acquire) {
                    Err(format!(
                        "Could not wake the Windows Super Panel listener: {}",
                        std::io::Error::last_os_error()
                    ))
                } else {
                    Ok(())
                }
            } else {
                Ok(())
            };
            let thread_result = wait_for_listener_thread(
                "Windows Super Panel hook",
                &self.completed,
                &mut self.thread,
            );

            // A caller-side Unhook failure is recoverable only when the hook
            // thread subsequently removed the handle itself.
            let hook_still_present = self
                .hook
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_some();
            let unhook_result = if hook_still_present {
                unhook_result
            } else {
                Ok(())
            };
            super::combine_results(
                super::combine_results(unhook_result, wake_result),
                thread_result,
            )
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            let _ = self.stop();
        }
    }

    pub(super) fn start(
        sender: mpsc::SyncSender<RawPointerEvent>,
    ) -> Result<Listener, StartFailure> {
        let listener_id = NEXT_LISTENER_ID.fetch_add(1, Ordering::AcqRel);
        let slot = POINTER_EVENTS.get_or_init(|| Mutex::new(None));
        {
            let mut active = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if active.is_some() {
                return Err(StartFailure {
                    message: "The Super Panel pointer listener is already running.".to_owned(),
                    listener: None,
                });
            }
            *active = Some(PointerEventSink {
                listener_id,
                sender,
            });
        }

        let thread_id = Arc::new(AtomicU32::new(0));
        let hook = Arc::new(Mutex::new(None));
        let stop_requested = Arc::new(AtomicBool::new(false));
        let running = Arc::new(AtomicBool::new(false));
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let (completed_sender, completed) = mpsc::sync_channel(1);
        let hook_thread_id = Arc::clone(&thread_id);
        let hook_handle = Arc::clone(&hook);
        let hook_stop = Arc::clone(&stop_requested);
        let hook_running = Arc::clone(&running);
        let thread = match std::thread::Builder::new()
            .name("ihub-super-panel-windows-hook".to_owned())
            .spawn(move || {
                let loop_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    windows_hook_loop(
                        &hook_thread_id,
                        &hook_handle,
                        &hook_stop,
                        &hook_running,
                        ready_sender,
                    )
                }))
                .unwrap_or_else(|_| {
                    Err("The Windows Super Panel hook thread panicked.".to_owned())
                });
                let cleanup_result = unhook_once(&hook_handle);
                let hook_still_present = hook_handle
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .is_some();
                hook_running.store(hook_still_present, Ordering::Release);
                clear_pointer_sender(listener_id);
                let _ =
                    completed_sender.try_send(super::combine_results(loop_result, cleanup_result));
            }) {
            Ok(thread) => thread,
            Err(error) => {
                clear_pointer_sender(listener_id);
                return Err(StartFailure {
                    message: format!("Could not start the Windows pointer listener: {error}"),
                    listener: None,
                });
            }
        };

        let mut listener = Listener {
            listener_id,
            thread_id,
            hook,
            stop_requested,
            running,
            completed,
            thread: Some(thread),
        };
        let ready_result = match ready_receiver.recv_timeout(LISTENER_START_TIMEOUT) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err("The Windows pointer listener did not start in time.".to_owned())
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err("The Windows pointer listener ended before reporting readiness.".to_owned())
            }
        };
        if let Err(error) = ready_result {
            let cleanup = listener.stop();
            return Err(match cleanup {
                Ok(()) => StartFailure {
                    message: error,
                    listener: None,
                },
                Err(cleanup) => StartFailure {
                    message: format!("{error} Listener cleanup also failed: {cleanup}"),
                    listener: Some(listener),
                },
            });
        }
        Ok(listener)
    }

    fn windows_hook_loop(
        thread_id: &AtomicU32,
        hook_slot: &Mutex<Option<isize>>,
        stop_requested: &AtomicBool,
        running: &AtomicBool,
        ready: mpsc::SyncSender<Result<(), String>>,
    ) -> Result<(), String> {
        // `PostThreadMessageW` fails until a thread owns a message queue.
        // Peek once before readiness so every returned listener is cancelable.
        let current_thread_id = unsafe { GetCurrentThreadId() };
        thread_id.store(current_thread_id, Ordering::Release);
        unsafe {
            let mut message: MSG = mem::zeroed();
            let _ = PeekMessageW(&mut message, ptr::null_mut(), 0, 0, PM_NOREMOVE);
        }
        if stop_requested.load(Ordering::Acquire) {
            let error = "The Windows pointer listener was cancelled before startup.".to_owned();
            let _ = ready.try_send(Err(error.clone()));
            return Err(error);
        }

        // SAFETY: a WH_MOUSE_LL hook may live in this executable. The callback
        // has static storage and the thread owns a cancelable message loop.
        let hook = unsafe {
            let module = GetModuleHandleW(ptr::null());
            SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), module, 0)
        };
        if hook.is_null() {
            let error = format!(
                "Could not install the Windows Super Panel listener: {}",
                std::io::Error::last_os_error()
            );
            let _ = ready.try_send(Err(error.clone()));
            return Err(error);
        }
        *hook_slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(hook as isize);
        running.store(true, Ordering::Release);
        if stop_requested.load(Ordering::Acquire) {
            let error = "The Windows pointer listener was cancelled during startup.".to_owned();
            let _ = ready.try_send(Err(error.clone()));
            return Err(error);
        }
        let _ = ready.try_send(Ok(()));

        // SAFETY: `message` is initialized for GetMessageW, and this thread
        // owns its message queue. The disabling caller unhooks first and then
        // posts WM_QUIT, so a failed wake cannot retain a global hook.
        unsafe {
            let mut message: MSG = mem::zeroed();
            loop {
                let result = GetMessageW(&mut message, ptr::null_mut(), 0, 0);
                if result == 0 {
                    break;
                }
                if result == -1 {
                    return Err(format!(
                        "The Windows Super Panel message loop failed: {}",
                        std::io::Error::last_os_error()
                    ));
                }
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        Ok(())
    }

    fn unhook_once(hook: &Mutex<Option<isize>>) -> Result<(), String> {
        let raw = hook
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(raw) = raw else {
            return Ok(());
        };
        // SAFETY: the value was produced by SetWindowsHookExW in this process.
        if unsafe { UnhookWindowsHookEx(raw as _) } != 0 {
            return Ok(());
        }
        *hook.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(raw);
        Err(format!(
            "Could not remove the Windows Super Panel hook: {}",
            std::io::Error::last_os_error()
        ))
    }

    fn clear_pointer_sender(listener_id: usize) {
        let Some(slot) = POINTER_EVENTS.get() else {
            return;
        };
        let mut active = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        clear_matching_pointer_sender(&mut active, listener_id);
    }

    fn clear_matching_pointer_sender(active: &mut Option<PointerEventSink>, listener_id: usize) {
        if active
            .as_ref()
            .is_some_and(|sink| sink.listener_id == listener_id)
        {
            *active = None;
        }
    }

    unsafe extern "system" fn mouse_hook(code: i32, message: WPARAM, payload: LPARAM) -> LRESULT {
        if code >= 0 && payload != 0 {
            // SAFETY: Windows supplies an MSLLHOOKSTRUCT for WH_MOUSE_LL.
            let mouse = unsafe { &*(payload as *const MSLLHOOKSTRUCT) };
            let point = (mouse.pt.x, mouse.pt.y);
            let observed_at = Instant::now();
            let event = match message as u32 {
                WM_RBUTTONDOWN => Some(RawPointerEvent::Down { point, observed_at }),
                WM_MOUSEMOVE => Some(RawPointerEvent::Move { point, observed_at }),
                WM_RBUTTONUP => Some(RawPointerEvent::Up { observed_at }),
                _ => None,
            };
            if let (Some(slot), Some(event)) = (POINTER_EVENTS.get(), event) {
                if let Ok(active) = slot.try_lock() {
                    if let Some(sink) = active.as_ref() {
                        let _ = sink.sender.try_send(event);
                    }
                }
            }
        }
        // Listen only. Never suppress, replace, or synthesize another
        // application's pointer messages.
        unsafe { CallNextHookEx(ptr::null_mut(), code, message, payload) }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn stale_listener_cleanup_cannot_clear_a_new_pointer_sender() {
            let (sender, _receiver) = mpsc::sync_channel(1);
            let mut active = Some(PointerEventSink {
                listener_id: 22,
                sender,
            });
            clear_matching_pointer_sender(&mut active, 21);
            assert_eq!(
                active.as_ref().map(|sink| sink.listener_id),
                Some(22),
                "an old hook thread must not clear the replacement generation"
            );
            clear_matching_pointer_sender(&mut active, 22);
            assert!(active.is_none());
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Arc, Mutex,
        },
        thread::JoinHandle,
        time::Instant,
    };

    use core_foundation::runloop::CFRunLoop;
    use core_graphics::event::{
        CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventType,
        CallbackResult,
    };

    use super::{wait_for_listener_thread, RawPointerEvent, LISTENER_START_TIMEOUT};

    pub(super) struct Listener {
        run_loop: Arc<Mutex<Option<CFRunLoop>>>,
        accepting_events: Arc<AtomicBool>,
        stop_requested: Arc<AtomicBool>,
        running: Arc<AtomicBool>,
        completed: mpsc::Receiver<Result<(), String>>,
        thread: Option<JoinHandle<()>>,
    }

    pub(super) struct StartFailure {
        pub(super) message: String,
        pub(super) listener: Option<Listener>,
    }

    impl Listener {
        pub(super) fn is_running(&self) -> bool {
            self.running.load(Ordering::Acquire)
        }

        pub(super) fn stop(&mut self) -> Result<(), String> {
            self.accepting_events.store(false, Ordering::Release);
            self.stop_requested.store(true, Ordering::Release);
            if let Some(run_loop) = self
                .run_loop
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
            {
                run_loop.stop();
            }
            wait_for_listener_thread(
                "macOS Super Panel event tap",
                &self.completed,
                &mut self.thread,
            )
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            let _ = self.stop();
        }
    }

    pub(super) fn start(
        sender: mpsc::SyncSender<RawPointerEvent>,
    ) -> Result<Listener, StartFailure> {
        let run_loop = Arc::new(Mutex::new(None));
        let accepting_events = Arc::new(AtomicBool::new(true));
        let stop_requested = Arc::new(AtomicBool::new(false));
        let running = Arc::new(AtomicBool::new(false));
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let (completed_sender, completed) = mpsc::sync_channel(1);
        let tap_run_loop = Arc::clone(&run_loop);
        let tap_accepting = Arc::clone(&accepting_events);
        let tap_stop = Arc::clone(&stop_requested);
        let tap_running = Arc::clone(&running);
        let thread = std::thread::Builder::new()
            .name("ihub-super-panel-macos-tap".to_owned())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    macos_event_loop(
                        sender,
                        ready_sender,
                        &tap_run_loop,
                        &tap_accepting,
                        &tap_stop,
                        &tap_running,
                    )
                }))
                .unwrap_or_else(|_| {
                    Err("The macOS Super Panel event tap thread panicked.".to_owned())
                });
                tap_accepting.store(false, Ordering::Release);
                tap_running.store(false, Ordering::Release);
                *tap_run_loop
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
                let _ = completed_sender.try_send(result);
            })
            .map_err(|error| StartFailure {
                message: format!("Could not start the macOS pointer listener: {error}"),
                listener: None,
            })?;

        let mut listener = Listener {
            run_loop,
            accepting_events,
            stop_requested,
            running,
            completed,
            thread: Some(thread),
        };
        let ready_result = match ready_receiver.recv_timeout(LISTENER_START_TIMEOUT) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                Err("The macOS pointer listener did not start in time.".to_owned())
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err("The macOS pointer listener ended before reporting readiness.".to_owned())
            }
        };
        if let Err(error) = ready_result {
            let cleanup = listener.stop();
            return Err(match cleanup {
                Ok(()) => StartFailure {
                    message: error,
                    listener: None,
                },
                Err(cleanup) => StartFailure {
                    message: format!("{error} Listener cleanup also failed: {cleanup}"),
                    listener: Some(listener),
                },
            });
        }
        Ok(listener)
    }

    fn macos_event_loop(
        sender: mpsc::SyncSender<RawPointerEvent>,
        ready: mpsc::SyncSender<Result<(), String>>,
        run_loop_slot: &Mutex<Option<CFRunLoop>>,
        accepting_events: &AtomicBool,
        stop_requested: &AtomicBool,
        running: &AtomicBool,
    ) -> Result<(), String> {
        let callback_accepting = accepting_events;
        let callback_run_loop = run_loop_slot;
        let result = CGEventTap::with_enabled(
            CGEventTapLocation::HID,
            CGEventTapPlacement::TailAppendEventTap,
            CGEventTapOptions::ListenOnly,
            vec![
                CGEventType::RightMouseDown,
                CGEventType::RightMouseDragged,
                CGEventType::RightMouseUp,
            ],
            move |_proxy, event_type, event| {
                if matches!(
                    event_type,
                    CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput
                ) {
                    callback_accepting.store(false, Ordering::Release);
                    if let Some(run_loop) = callback_run_loop
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .clone()
                    {
                        run_loop.stop();
                    }
                    return CallbackResult::Keep;
                }
                if !callback_accepting.load(Ordering::Acquire) {
                    return CallbackResult::Keep;
                }
                let location = event.location();
                let point = (
                    location
                        .x
                        .round()
                        .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32,
                    location
                        .y
                        .round()
                        .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32,
                );
                let observed_at = Instant::now();
                let event = match event_type {
                    CGEventType::RightMouseDown => {
                        Some(RawPointerEvent::Down { point, observed_at })
                    }
                    CGEventType::RightMouseDragged => {
                        Some(RawPointerEvent::Move { point, observed_at })
                    }
                    CGEventType::RightMouseUp => Some(RawPointerEvent::Up { observed_at }),
                    _ => None,
                };
                if let Some(event) = event {
                    let _ = sender.try_send(event);
                }
                CallbackResult::Keep
            },
            || {
                let run_loop = CFRunLoop::get_current();
                *run_loop_slot
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(run_loop.clone());
                running.store(true, Ordering::Release);
                let _ = ready.try_send(Ok(()));
                if !stop_requested.load(Ordering::Acquire) {
                    CFRunLoop::run_current();
                }
            },
        );
        if result.is_ok() {
            return Ok(());
        }
        let error =
            "macOS denied the listen-only Super Panel event tap. Enable Input Monitoring for iHub."
                .to_owned();
        let _ = ready.try_send(Err(error.clone()));
        Err(error)
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod platform {
    use std::sync::mpsc;

    use super::RawPointerEvent;

    pub(super) struct Listener;
    pub(super) struct StartFailure {
        pub(super) message: String,
        pub(super) listener: Option<Listener>,
    }

    impl Listener {
        pub(super) fn is_running(&self) -> bool {
            false
        }

        pub(super) fn stop(&mut self) -> Result<(), String> {
            Ok(())
        }
    }

    pub(super) fn start(
        _sender: mpsc::SyncSender<RawPointerEvent>,
    ) -> Result<Listener, StartFailure> {
        Err(StartFailure {
            message: "The global Super Panel gesture is supported on Windows and macOS.".to_owned(),
            listener: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    struct FakeListenerControl {
        running: Arc<AtomicBool>,
        usable: Arc<AtomicBool>,
        stop_calls: Arc<AtomicUsize>,
        stop_failures_remaining: Arc<AtomicUsize>,
    }

    impl ListenerControl for FakeListenerControl {
        fn is_running(&self) -> bool {
            self.running.load(Ordering::Acquire)
        }

        fn is_usable(&self) -> bool {
            self.usable.load(Ordering::Acquire) && self.is_running()
        }

        fn stop(&mut self) -> Result<(), String> {
            self.stop_calls.fetch_add(1, Ordering::AcqRel);
            self.usable.store(false, Ordering::Release);
            if self
                .stop_failures_remaining
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
            {
                return Err("simulated listener stop failure".to_owned());
            }
            self.running.store(false, Ordering::Release);
            Ok(())
        }
    }

    fn fake_listener(
        stop_calls: Arc<AtomicUsize>,
        stop_failures: usize,
    ) -> GlobalRightHoldListener {
        GlobalRightHoldListener {
            control: Box::new(FakeListenerControl {
                running: Arc::new(AtomicBool::new(true)),
                usable: Arc::new(AtomicBool::new(true)),
                stop_calls,
                stop_failures_remaining: Arc::new(AtomicUsize::new(stop_failures)),
            }),
            stopped: false,
        }
    }

    #[test]
    fn a_stationary_hold_fires_once_after_the_deliberate_delay() {
        let start = Instant::now();
        let mut detector = RightHoldDetector::default();
        assert!(detector
            .observe(RawPointerEvent::Down {
                point: (120, -40),
                observed_at: start,
            })
            .is_none());
        assert!(detector
            .poll(start + Duration::from_millis(SUPER_PANEL_HOLD_MS - 1))
            .is_none());
        assert_eq!(
            detector.poll(start + Duration::from_millis(SUPER_PANEL_HOLD_MS)),
            Some(SuperPanelTrigger {
                physical_x: 120,
                physical_y: -40,
                held_ms: SUPER_PANEL_HOLD_MS,
            })
        );
        assert!(detector
            .poll(start + Duration::from_millis(SUPER_PANEL_HOLD_MS + 500))
            .is_none());
    }

    #[test]
    fn movement_beyond_the_tolerance_cancels_the_hold() {
        let start = Instant::now();
        let mut detector = RightHoldDetector::default();
        detector.observe(RawPointerEvent::Down {
            point: (10, 10),
            observed_at: start,
        });
        detector.observe(RawPointerEvent::Move {
            point: (30, 10),
            observed_at: start + Duration::from_millis(50),
        });
        assert!(detector.poll(start + Duration::from_secs(1)).is_none());
    }

    #[test]
    fn releasing_before_the_delay_does_not_fire_and_resets_the_gesture() {
        let start = Instant::now();
        let mut detector = RightHoldDetector::default();
        detector.observe(RawPointerEvent::Down {
            point: (1, 2),
            observed_at: start,
        });
        assert!(detector
            .observe(RawPointerEvent::Up {
                observed_at: start + Duration::from_millis(120),
            })
            .is_none());
        detector.observe(RawPointerEvent::Down {
            point: (3, 4),
            observed_at: start + Duration::from_secs(2),
        });
        assert_eq!(
            detector
                .poll(start + Duration::from_secs(2) + Duration::from_millis(SUPER_PANEL_HOLD_MS)),
            Some(SuperPanelTrigger {
                physical_x: 3,
                physical_y: 4,
                held_ms: SUPER_PANEL_HOLD_MS,
            })
        );
    }

    #[test]
    fn cancelling_the_detector_discards_a_pending_hold_without_a_late_trigger() {
        let (sender, receiver) = mpsc::sync_channel(4);
        let stop_requested = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop_requested);
        let triggers = Arc::new(AtomicUsize::new(0));
        let triggers_for_thread = Arc::clone(&triggers);
        let detector = std::thread::spawn(move || {
            run_detector(
                receiver,
                Arc::new(move |_| {
                    triggers_for_thread.fetch_add(1, Ordering::AcqRel);
                }),
                stop_for_thread,
            );
        });
        sender
            .send(RawPointerEvent::Down {
                point: (10, 20),
                observed_at: Instant::now(),
            })
            .expect("queue pending gesture");
        std::thread::sleep(Duration::from_millis(30));
        stop_requested.store(true, Ordering::Release);
        drop(sender);
        detector.join().expect("detector stops");
        assert_eq!(triggers.load(Ordering::Acquire), 0);
    }

    #[test]
    fn context_tokens_are_opt_in_single_use_and_ownerless() {
        let state = SuperPanelState::default();
        state.set_enabled(true).expect("enable runtime");
        assert!(state
            .issue_context(SuperPanelTrigger {
                physical_x: 1,
                physical_y: 2,
                held_ms: SUPER_PANEL_HOLD_MS,
            })
            .is_none());

        let stop_calls = Arc::new(AtomicUsize::new(0));
        state
            .ensure_listener_with(|| Ok(fake_listener(Arc::clone(&stop_calls), 0)))
            .expect("install fake listener");
        let event = state
            .issue_context(SuperPanelTrigger {
                physical_x: 30,
                physical_y: 40,
                held_ms: SUPER_PANEL_HOLD_MS,
            })
            .expect("enabled context");
        assert_eq!((event.physical_x, event.physical_y), (30, 40));
        assert!(state.consume_context("not-a-token").is_err());
        assert!(state.consume_context(&event.context_token).is_ok());
        assert!(state.consume_context(&event.context_token).is_err());
        state.set_enabled(false).expect("stop fake listener");
        assert_eq!(stop_calls.load(Ordering::Acquire), 1);
    }

    #[test]
    fn disabling_stops_the_listener_and_revokes_a_pending_context() {
        let state = SuperPanelState::default();
        let stop_calls = Arc::new(AtomicUsize::new(0));
        state.set_enabled(true).expect("enable runtime");
        state
            .ensure_listener_with(|| Ok(fake_listener(Arc::clone(&stop_calls), 0)))
            .expect("install fake listener");
        let event = state
            .issue_context(SuperPanelTrigger {
                physical_x: 0,
                physical_y: 0,
                held_ms: SUPER_PANEL_HOLD_MS,
            })
            .expect("context");
        state.set_enabled(false).expect("disable and stop listener");
        assert!(state.consume_context(&event.context_token).is_err());
        assert!(!state.status().enabled);
        assert!(!state.status().listener_running);
        assert_eq!(stop_calls.load(Ordering::Acquire), 1);
    }

    #[test]
    fn failed_stop_stays_honest_and_a_retry_can_restart_the_listener() {
        let state = SuperPanelState::default();
        let first_stop_calls = Arc::new(AtomicUsize::new(0));
        state.set_enabled(true).expect("enable runtime");
        state
            .ensure_listener_with(|| Ok(fake_listener(Arc::clone(&first_stop_calls), 1)))
            .expect("install first listener");

        assert!(state.set_enabled(false).is_err());
        let failed_status = state.status();
        assert!(!failed_status.enabled);
        assert!(failed_status.listener_running);
        assert!(failed_status.error.is_some());

        state.set_enabled(true).expect("re-enable runtime");
        let second_stop_calls = Arc::new(AtomicUsize::new(0));
        state
            .ensure_listener_with(|| Ok(fake_listener(Arc::clone(&second_stop_calls), 0)))
            .expect("cleanup stale listener and restart");
        assert_eq!(first_stop_calls.load(Ordering::Acquire), 2);
        assert!(state.status().listener_running);
        assert!(state.status().error.is_none());

        state.set_enabled(false).expect("stop restarted listener");
        assert_eq!(second_stop_calls.load(Ordering::Acquire), 1);
        assert!(!state.status().listener_running);
    }

    #[test]
    fn duplicate_ensure_does_not_start_a_second_listener() {
        let state = SuperPanelState::default();
        state.set_enabled(true).expect("enable runtime");
        let starts = Arc::new(AtomicUsize::new(0));
        let stop_calls = Arc::new(AtomicUsize::new(0));
        let first_starts = Arc::clone(&starts);
        state
            .ensure_listener_with(|| {
                first_starts.fetch_add(1, Ordering::AcqRel);
                Ok(fake_listener(Arc::clone(&stop_calls), 0))
            })
            .expect("start listener");
        let duplicate_starts = Arc::clone(&starts);
        state
            .ensure_listener_with(|| {
                duplicate_starts.fetch_add(1, Ordering::AcqRel);
                Ok(fake_listener(Arc::new(AtomicUsize::new(0)), 0))
            })
            .expect("reuse listener");
        assert_eq!(starts.load(Ordering::Acquire), 1);
        state.set_enabled(false).expect("cleanup");
    }

    #[test]
    fn failed_start_leaves_no_listener_and_a_later_start_can_succeed() {
        let state = SuperPanelState::default();
        state.set_enabled(true).expect("enable runtime");
        assert!(state
            .ensure_listener_with(|| Err(ListenerStartFailure::clean("simulated ready timeout")))
            .is_err());
        assert!(!state.status().listener_running);
        assert!(!state.listener_accepting.load(Ordering::Acquire));

        let stop_calls = Arc::new(AtomicUsize::new(0));
        state
            .ensure_listener_with(|| Ok(fake_listener(Arc::clone(&stop_calls), 0)))
            .expect("retry listener start");
        assert!(state.status().listener_running);
        state.set_enabled(false).expect("cleanup retry");
        assert_eq!(stop_calls.load(Ordering::Acquire), 1);
    }

    #[test]
    fn partially_started_listener_is_quarantined_until_cleanup_can_finish() {
        let state = SuperPanelState::default();
        state.set_enabled(true).expect("enable runtime");
        let stale_stop_calls = Arc::new(AtomicUsize::new(0));
        let error = state
            .ensure_listener_with(|| {
                Err(ListenerStartFailure {
                    message: "simulated ready-timeout cleanup failure".to_owned(),
                    listener: Some(fake_listener(Arc::clone(&stale_stop_calls), 1)),
                })
            })
            .expect_err("partial start must fail");
        assert!(state.status().listener_running);
        assert!(!state.listener_accepting.load(Ordering::Acquire));

        state.listener_failed(error);
        assert!(!state.enabled());
        assert!(state.status().listener_running);
        assert_eq!(stale_stop_calls.load(Ordering::Acquire), 1);

        state.set_enabled(true).expect("re-enable runtime");
        let replacement_stop_calls = Arc::new(AtomicUsize::new(0));
        state
            .ensure_listener_with(|| Ok(fake_listener(Arc::clone(&replacement_stop_calls), 0)))
            .expect("retry cleanup and install replacement");
        assert_eq!(stale_stop_calls.load(Ordering::Acquire), 2);
        assert!(state.status().listener_running);
        state.set_enabled(false).expect("cleanup replacement");
        assert_eq!(replacement_stop_calls.load(Ordering::Acquire), 1);
    }

    #[test]
    fn shutdown_stops_runtime_without_erasing_the_enabled_preference() {
        let directory = std::env::temp_dir().join(format!(
            "ihub-super-panel-shutdown-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&directory).expect("test directory");
        let state = SuperPanelState::with_storage(directory.clone());
        state
            .set_enabled_persisted(true)
            .expect("persist enabled preference");
        let stop_calls = Arc::new(AtomicUsize::new(0));
        state
            .ensure_listener_with(|| Ok(fake_listener(Arc::clone(&stop_calls), 0)))
            .expect("install fake listener");

        state.shutdown_listener().expect("bounded shutdown");
        assert!(state.enabled());
        assert!(!state.status().listener_running);
        assert_eq!(stop_calls.load(Ordering::Acquire), 1);
        assert!(SuperPanelState::with_storage(directory.clone()).enabled());

        drop(state);
        fs::remove_file(directory.join(SUPER_PANEL_PREFERENCE_FILE)).expect("remove preference");
        fs::remove_dir(directory).expect("remove test directory");
    }

    #[test]
    fn listener_start_failure_rolls_back_the_persisted_opt_in() {
        let directory = std::env::temp_dir().join(format!(
            "ihub-super-panel-start-failure-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&directory).expect("test directory");
        let state = SuperPanelState::with_storage(directory.clone());
        state
            .set_enabled_persisted(true)
            .expect("persist enabled preference");
        state.listener_failed("simulated native listener failure".to_owned());

        assert!(!state.enabled());
        assert!(!state.status().listener_running);
        assert!(state
            .status()
            .error
            .is_some_and(|error| error.contains("simulated native listener failure")));
        assert!(!SuperPanelState::with_storage(directory.clone()).enabled());

        drop(state);
        fs::remove_file(directory.join(SUPER_PANEL_PREFERENCE_FILE)).expect("remove preference");
        fs::remove_dir(directory).expect("remove test directory");
    }

    #[test]
    fn enabled_preference_round_trips_without_following_non_files() {
        let directory = std::env::temp_dir().join(format!(
            "ihub-super-panel-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&directory).expect("test directory");

        let state = SuperPanelState::with_storage(directory.clone());
        assert!(!state.enabled());
        state
            .set_enabled_persisted(true)
            .expect("persist enabled preference");
        assert!(SuperPanelState::with_storage(directory.clone()).enabled());

        let preference = directory.join(SUPER_PANEL_PREFERENCE_FILE);
        fs::remove_file(&preference).expect("remove preference");
        fs::create_dir(&preference).expect("replace preference with directory");
        let rejected = SuperPanelState::with_storage(directory.clone());
        assert!(!rejected.enabled());
        assert!(rejected.status().error.is_some());
        assert!(rejected.set_enabled_persisted(true).is_err());
        assert!(!rejected.enabled());
        assert!(!rejected.status().listener_running);

        fs::remove_dir(&preference).expect("remove fake preference directory");
        fs::remove_dir(&directory).expect("remove test directory");
    }
}
