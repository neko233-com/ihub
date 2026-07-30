//! Bounded operating-system icons for launcher results.
//!
//! Windows Shell/COM work and macOS application-bundle artwork decoding stay
//! off the command thread. Callers submit trusted native-index paths through a
//! bounded queue and receive only a bounded PNG data URL.

use std::{
    collections::{HashMap, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
        Arc, OnceLock,
    },
    time::{Duration, Instant, SystemTime},
};

const ICON_EDGE: u32 = 48;
const WORK_QUEUE_CAPACITY: usize = 16;
const POSITIVE_CACHE_TTL: Duration = Duration::from_secs(15 * 60);
const NEGATIVE_CACHE_TTL: Duration = Duration::from_secs(20);
const MAX_ICON_CACHE_ENTRIES: usize = 512;
const MAX_ICON_CACHE_BYTES: usize = 12 * 1024 * 1024;
const MAX_ICON_DATA_URL_BYTES: usize = 128 * 1024;
const MAX_PATH_CODE_UNITS: usize = 32_767;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct IconCacheKey {
    path: PathBuf,
    kind: String,
    modified: Option<SystemTime>,
    size: Option<u64>,
    edge: u32,
}

struct CacheEntry {
    value: Option<String>,
    expires_at: Instant,
    bytes: usize,
}

struct IconCache {
    values: HashMap<IconCacheKey, CacheEntry>,
    lru: VecDeque<IconCacheKey>,
    total_bytes: usize,
    max_entries: usize,
    max_bytes: usize,
    positive_ttl: Duration,
    negative_ttl: Duration,
}

impl IconCache {
    fn bounded() -> Self {
        Self::new(
            MAX_ICON_CACHE_ENTRIES,
            MAX_ICON_CACHE_BYTES,
            POSITIVE_CACHE_TTL,
            NEGATIVE_CACHE_TTL,
        )
    }

    fn new(
        max_entries: usize,
        max_bytes: usize,
        positive_ttl: Duration,
        negative_ttl: Duration,
    ) -> Self {
        Self {
            values: HashMap::new(),
            lru: VecDeque::new(),
            total_bytes: 0,
            max_entries,
            max_bytes,
            positive_ttl,
            negative_ttl,
        }
    }

    fn get(&mut self, key: &IconCacheKey, now: Instant) -> Option<Option<String>> {
        let entry = self.values.get(key)?;
        if entry.expires_at <= now {
            self.remove(key);
            return None;
        }
        let value = entry.value.clone();
        self.touch(key);
        Some(value)
    }

    fn insert(&mut self, key: IconCacheKey, value: Option<String>, now: Instant) {
        self.remove(&key);
        self.purge_expired(now);

        if self.max_entries == 0 {
            return;
        }
        let bytes = value.as_ref().map_or(0, String::len);
        if bytes > self.max_bytes || bytes > MAX_ICON_DATA_URL_BYTES {
            return;
        }

        while self.values.len() >= self.max_entries
            || self.total_bytes.saturating_add(bytes) > self.max_bytes
        {
            let Some(oldest) = self.lru.pop_front() else {
                self.values.clear();
                self.total_bytes = 0;
                break;
            };
            if let Some(removed) = self.values.remove(&oldest) {
                self.total_bytes = self.total_bytes.saturating_sub(removed.bytes);
            }
        }

        let ttl = if value.is_some() {
            self.positive_ttl
        } else {
            self.negative_ttl
        };
        self.total_bytes = self.total_bytes.saturating_add(bytes);
        self.lru.push_back(key.clone());
        self.values.insert(
            key,
            CacheEntry {
                value,
                expires_at: now + ttl,
                bytes,
            },
        );
    }

    fn purge_expired(&mut self, now: Instant) {
        let expired = self
            .values
            .iter()
            .filter(|(_, entry)| entry.expires_at <= now)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in expired {
            self.remove(&key);
        }
    }

    fn touch(&mut self, key: &IconCacheKey) {
        self.lru.retain(|candidate| candidate != key);
        self.lru.push_back(key.clone());
    }

    fn remove(&mut self, key: &IconCacheKey) {
        self.lru.retain(|candidate| candidate != key);
        if let Some(removed) = self.values.remove(key) {
            self.total_bytes = self.total_bytes.saturating_sub(removed.bytes);
        }
    }
}

struct IconRequest {
    id: u64,
    path: PathBuf,
    kind: String,
    reply: SyncSender<Option<String>>,
}

#[derive(Default)]
struct WorkerState {
    stalled: AtomicBool,
    next_id: AtomicU64,
    last_completed_id: AtomicU64,
}

/// A pending native-icon request.
///
/// Waiting is explicitly bounded. If a third-party Shell extension stalls past
/// the timeout, the service temporarily rejects new work until its STA worker
/// finishes the outstanding request.
pub(crate) struct NativeIconPending {
    id: u64,
    receiver: Receiver<Option<String>>,
    worker_state: Arc<WorkerState>,
}

impl NativeIconPending {
    pub(crate) fn wait_timeout(self, timeout: Duration) -> Option<String> {
        match self.receiver.recv_timeout(timeout) {
            Ok(value) => value,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if self.worker_state.last_completed_id.load(Ordering::Acquire) < self.id {
                    self.worker_state.stalled.store(true, Ordering::Release);
                }
                None
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.worker_state.stalled.store(true, Ordering::Release);
                None
            }
        }
    }
}

/// Process-wide access to operating-system icons.
///
/// `try_request` never waits for queue capacity. Consumers retain the returned
/// pending request and wait on a background task, keeping Shell work out of the
/// command/UI thread.
pub(crate) struct NativeIconService {
    sender: Option<SyncSender<IconRequest>>,
    worker_state: Arc<WorkerState>,
}

impl NativeIconService {
    pub(crate) fn shared() -> &'static Self {
        static SERVICE: OnceLock<NativeIconService> = OnceLock::new();
        SERVICE.get_or_init(Self::new)
    }

    pub(crate) fn new() -> Self {
        let worker_state = Arc::new(WorkerState::default());
        let sender = start_worker(Arc::clone(&worker_state));
        if sender.is_none() && cfg!(any(target_os = "windows", target_os = "macos")) {
            worker_state.stalled.store(true, Ordering::Release);
        }
        Self {
            sender,
            worker_state,
        }
    }

    pub(crate) fn try_request(&self, path: &Path, kind: &str) -> Option<NativeIconPending> {
        if !valid_icon_input(path, kind) || self.worker_state.stalled.load(Ordering::Acquire) {
            return None;
        }
        let sender = self.sender.as_ref()?;
        let id = self
            .worker_state
            .next_id
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let (reply, receiver) = mpsc::sync_channel(1);
        let request = IconRequest {
            id,
            path: path.to_path_buf(),
            kind: kind.to_owned(),
            reply,
        };
        match sender.try_send(request) {
            Ok(()) => Some(NativeIconPending {
                id,
                receiver,
                worker_state: Arc::clone(&self.worker_state),
            }),
            Err(TrySendError::Full(_)) => None,
            Err(TrySendError::Disconnected(_)) => {
                self.worker_state.stalled.store(true, Ordering::Release);
                None
            }
        }
    }
}

impl Default for NativeIconService {
    fn default() -> Self {
        Self::new()
    }
}

fn valid_icon_input(path: &Path, kind: &str) -> bool {
    matches!(kind, "file" | "folder" | "application")
        && path.is_absolute()
        && !path.as_os_str().is_empty()
        && path_code_units(path) <= MAX_PATH_CODE_UNITS
}

#[cfg(target_os = "windows")]
fn path_code_units(path: &Path) -> usize {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().count()
}

#[cfg(not(target_os = "windows"))]
fn path_code_units(path: &Path) -> usize {
    path.as_os_str().to_string_lossy().encode_utf16().count()
}

fn cache_key(path: PathBuf, kind: String, metadata: Option<&fs::Metadata>) -> IconCacheKey {
    IconCacheKey {
        path,
        kind,
        modified: metadata.and_then(|value| value.modified().ok()),
        size: metadata.map(fs::Metadata::len),
        edge: ICON_EDGE,
    }
}

#[cfg(target_os = "windows")]
fn start_worker(worker_state: Arc<WorkerState>) -> Option<SyncSender<IconRequest>> {
    let (sender, receiver) = mpsc::sync_channel(WORK_QUEUE_CAPACITY);
    std::thread::Builder::new()
        .name("ihub-native-icon-sta".to_owned())
        .spawn(move || icon_worker(receiver, worker_state))
        .ok()?;
    Some(sender)
}

#[cfg(target_os = "macos")]
fn start_worker(worker_state: Arc<WorkerState>) -> Option<SyncSender<IconRequest>> {
    let (sender, receiver) = mpsc::sync_channel(WORK_QUEUE_CAPACITY);
    std::thread::Builder::new()
        .name("ihub-native-icon-macos".to_owned())
        .spawn(move || macos_icon_worker(receiver, worker_state))
        .ok()?;
    Some(sender)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn start_worker(_worker_state: Arc<WorkerState>) -> Option<SyncSender<IconRequest>> {
    None
}

#[cfg(target_os = "windows")]
fn icon_worker(receiver: Receiver<IconRequest>, worker_state: Arc<WorkerState>) {
    let Some(_apartment) = windows_backend::StaApartment::initialize() else {
        worker_state.stalled.store(true, Ordering::Release);
        return;
    };
    let mut cache = IconCache::bounded();
    while let Ok(request) = receiver.recv() {
        let metadata = fs::metadata(&request.path).ok();
        let key = cache_key(
            request.path.clone(),
            request.kind.clone(),
            metadata.as_ref(),
        );
        let now = Instant::now();
        let value = if let Some(cached) = cache.get(&key, now) {
            cached
        } else {
            let valid_type = metadata.as_ref().is_some_and(|value| {
                if request.kind == "folder" {
                    value.is_dir()
                } else {
                    value.is_file()
                }
            });
            let extracted = valid_type
                .then(|| windows_backend::icon_data_url(&request.path))
                .flatten();
            cache.insert(key, extracted.clone(), now);
            extracted
        };

        worker_state
            .last_completed_id
            .store(request.id, Ordering::Release);
        worker_state.stalled.store(false, Ordering::Release);
        let _ = request.reply.try_send(value);
    }
}

#[cfg(target_os = "macos")]
fn macos_icon_worker(receiver: Receiver<IconRequest>, worker_state: Arc<WorkerState>) {
    let mut cache = IconCache::bounded();
    while let Ok(request) = receiver.recv() {
        let metadata = fs::metadata(&request.path).ok();
        let key = cache_key(
            request.path.clone(),
            request.kind.clone(),
            metadata.as_ref(),
        );
        let now = Instant::now();
        let value = if let Some(cached) = cache.get(&key, now) {
            cached
        } else {
            // dTools only resolves bundle artwork for applications on macOS.
            // Files and folders intentionally retain the neutral host fallback
            // until their native Finder icon contract is implemented.
            let extracted = (request.kind == "application"
                && metadata.as_ref().is_some_and(fs::Metadata::is_dir))
            .then(|| macos_backend::application_icon_data_url(&request.path))
            .flatten();
            cache.insert(key, extracted.clone(), now);
            extracted
        };

        worker_state
            .last_completed_id
            .store(request.id, Ordering::Release);
        worker_state.stalled.store(false, Ordering::Release);
        let _ = request.reply.try_send(value);
    }
}

#[cfg(target_os = "windows")]
mod windows_backend {
    use std::{
        ffi::{c_void, OsString},
        iter,
        mem::size_of,
        os::windows::ffi::{OsStrExt, OsStringExt},
        path::{Component, Path, PathBuf, Prefix},
        ptr,
    };

    use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
    use image::{
        codecs::png::PngEncoder,
        imageops::{self, FilterType},
        ColorType, ImageEncoder, RgbaImage,
    };
    use windows::{
        core::{Interface, PCWSTR},
        Win32::{
            Foundation::SIZE,
            Graphics::Gdi::{
                CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDIBits,
                GetObjectW, SelectObject, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
                DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
            },
            Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES,
            System::Com::{
                CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile,
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, STGM_READ,
            },
            System::Environment::ExpandEnvironmentStringsW,
            System::WindowsProgramming::{GetPrivateProfileIntW, GetPrivateProfileStringW},
            UI::{
                Shell::{
                    IShellItemImageFactory, IShellLinkW, SHCreateItemFromParsingName,
                    SHDefExtractIconW, SHGetFileInfoW, ShellLink, SHFILEINFOW, SHGFI_ICON,
                    SHGFI_LARGEICON, SIIGBF_ICONONLY,
                },
                WindowsAndMessaging::{DestroyIcon, DrawIconEx, DI_NORMAL, HICON},
            },
        },
    };

    use super::{ICON_EDGE, MAX_ICON_DATA_URL_BYTES, MAX_PATH_CODE_UNITS};

    pub(super) struct StaApartment;

    impl StaApartment {
        pub(super) fn initialize() -> Option<Self> {
            // SAFETY: this is called exactly once at the beginning of the
            // dedicated worker thread. Every COM interface is created, used,
            // and released on this same thread.
            unsafe {
                CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE)
                    .ok()
                    .ok()?;
            }
            Some(Self)
        }
    }

    impl Drop for StaApartment {
        fn drop(&mut self) {
            // SAFETY: paired with this thread's successful CoInitializeEx.
            unsafe {
                CoUninitialize();
            }
        }
    }

    struct OwnedBitmap(HBITMAP);

    impl Drop for OwnedBitmap {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                // SAFETY: IShellItemImageFactory/CreateDIBSection transferred
                // ownership of this HBITMAP to the caller.
                unsafe {
                    let _ = DeleteObject(self.0.into());
                }
            }
        }
    }

    struct OwnedDc(HDC);

    impl Drop for OwnedDc {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                // SAFETY: this DC was created by CreateCompatibleDC.
                unsafe {
                    let _ = DeleteDC(self.0);
                }
            }
        }
    }

    struct OwnedIcon(HICON);

    impl Drop for OwnedIcon {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                // SAFETY: the Windows Shell transferred ownership of this icon.
                unsafe {
                    let _ = DestroyIcon(self.0);
                }
            }
        }
    }

    struct DibSurface {
        bitmap: OwnedBitmap,
        dc: OwnedDc,
        previous: HGDIOBJ,
        bits: *mut u8,
        byte_len: usize,
    }

    impl DibSurface {
        fn new(edge: u32) -> Option<Self> {
            let edge_i32 = i32::try_from(edge).ok()?;
            let byte_len = usize::try_from(edge)
                .ok()?
                .checked_mul(usize::try_from(edge).ok()?)?
                .checked_mul(4)?;
            // SAFETY: all returned handles are checked and immediately wrapped
            // in RAII owners. The top-down 32-bit DIB owns byte_len bytes.
            unsafe {
                let dc = OwnedDc(CreateCompatibleDC(None));
                if dc.0.is_invalid() {
                    return None;
                }
                let info = BITMAPINFO {
                    bmiHeader: BITMAPINFOHEADER {
                        biSize: size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: edge_i32,
                        biHeight: -edge_i32,
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB.0,
                        biSizeImage: u32::try_from(byte_len).ok()?,
                        ..BITMAPINFOHEADER::default()
                    },
                    ..BITMAPINFO::default()
                };
                let mut bits: *mut c_void = ptr::null_mut();
                let bitmap = OwnedBitmap(
                    CreateDIBSection(Some(dc.0), &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?,
                );
                if bits.is_null() {
                    return None;
                }
                let previous = SelectObject(dc.0, bitmap.0.into());
                if previous.is_invalid() {
                    return None;
                }
                Some(Self {
                    bitmap,
                    dc,
                    previous,
                    bits: bits.cast(),
                    byte_len,
                })
            }
        }

        fn fill(&mut self, channel: u8) {
            // SAFETY: bits points to this live DIB's exact byte_len allocation.
            let pixels = unsafe { std::slice::from_raw_parts_mut(self.bits, self.byte_len) };
            for pixel in pixels.chunks_exact_mut(4) {
                pixel.copy_from_slice(&[channel, channel, channel, u8::MAX]);
            }
        }

        fn bytes(&self) -> Vec<u8> {
            // SAFETY: bits remains valid until this surface is dropped.
            unsafe { std::slice::from_raw_parts(self.bits, self.byte_len).to_vec() }
        }
    }

    impl Drop for DibSurface {
        fn drop(&mut self) {
            // SAFETY: restoring the previous object before the owned bitmap is
            // deleted keeps both GDI handles valid on every return path.
            unsafe {
                let _ = SelectObject(self.dc.0, self.previous);
            }
            let _ = &self.bitmap;
        }
    }

    #[derive(Debug)]
    struct ShortcutIconSources {
        target: Option<PathBuf>,
        custom_icon: Option<(PathBuf, i32)>,
    }

    pub(super) fn icon_data_url(path: &Path) -> Option<String> {
        let wide_path = nul_terminated_wide(path)?;
        // Windows shortcuts may explicitly point at an icon resource different
        // from their launch target (for example, a PowerShell launcher with the
        // product's own .ico). Honor that native source and resource index
        // first, then use the target program glyph without a shortcut overlay.
        // The original .lnk remains the bounded fallback for unusual links.
        let shortcut_sources = shortcut_icon_sources(path, &wide_path);
        let internet_shortcut_icon = internet_shortcut_icon_source(path, &wide_path);
        let extract =
            |source: &[u16]| shell_item_image(source).or_else(|| shell_icon_fallback(source));
        let image = shortcut_sources
            .as_ref()
            .and_then(|sources| sources.custom_icon.as_ref())
            .and_then(|(icon_path, icon_index)| indexed_icon_image(icon_path, *icon_index))
            .or_else(|| {
                shortcut_sources
                    .as_ref()
                    .and_then(|sources| sources.target.as_deref())
                    .and_then(nul_terminated_wide)
                    .as_deref()
                    .and_then(extract)
            })
            .or_else(|| {
                internet_shortcut_icon
                    .as_ref()
                    .and_then(|(icon_path, icon_index)| indexed_icon_image(icon_path, *icon_index))
            })
            .or_else(|| extract(&wide_path))?;
        encode_png_data_url(image.0, image.1, image.2)
    }

    fn nul_terminated_wide(path: &Path) -> Option<Vec<u16>> {
        let wide = path
            .as_os_str()
            .encode_wide()
            .chain(iter::once(0))
            .collect::<Vec<_>>();
        (wide.len() <= MAX_PATH_CODE_UNITS + 1).then_some(wide)
    }

    fn shortcut_icon_sources(path: &Path, wide_path: &[u16]) -> Option<ShortcutIconSources> {
        if !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("lnk"))
        {
            return None;
        }

        // SAFETY: this function only runs on the dedicated STA. The shortcut
        // and persistence interfaces are created, loaded, used, and released
        // on that same thread. GetPath writes into the bounded live buffer.
        let (target, custom_icon) = unsafe {
            let shortcut: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
            let persistence: IPersistFile = shortcut.cast().ok()?;
            persistence
                .Load(PCWSTR(wide_path.as_ptr()), STGM_READ)
                .ok()?;

            let mut target = vec![0_u16; MAX_PATH_CODE_UNITS + 1];
            let target = shortcut
                .GetPath(&mut target, ptr::null_mut(), 0)
                .ok()
                .and_then(|_| path_from_wide_buffer(&target));

            let mut icon_path = vec![0_u16; MAX_PATH_CODE_UNITS + 1];
            let mut icon_index = 0_i32;
            let custom_icon = shortcut
                .GetIconLocation(&mut icon_path, &mut icon_index)
                .ok()
                .and_then(|_| path_from_wide_buffer(&icon_path))
                .map(|icon_path| (icon_path, icon_index));

            (target, custom_icon)
        };

        let sources = ShortcutIconSources {
            target: target.and_then(expanded_local_file),
            custom_icon: custom_icon.and_then(|(icon_path, icon_index)| {
                expanded_local_file(icon_path).map(|icon_path| (icon_path, icon_index))
            }),
        };
        (sources.target.is_some() || sources.custom_icon.is_some()).then_some(sources)
    }

    fn internet_shortcut_icon_source(path: &Path, wide_path: &[u16]) -> Option<(PathBuf, i32)> {
        if !path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("url"))
        {
            return None;
        }

        let section = nul_terminated_text("InternetShortcut");
        let icon_file_key = nul_terminated_text("IconFile");
        let icon_index_key = nul_terminated_text("IconIndex");
        let empty = [0_u16];
        let mut icon_path = vec![0_u16; MAX_PATH_CODE_UNITS + 1];
        // SAFETY: all strings are NUL-terminated and stay live. The destination
        // is a bounded UTF-16 buffer, and the profile APIs only read this local
        // regular Internet Shortcut file.
        let (length, icon_index) = unsafe {
            let length = GetPrivateProfileStringW(
                PCWSTR(section.as_ptr()),
                PCWSTR(icon_file_key.as_ptr()),
                PCWSTR(empty.as_ptr()),
                Some(icon_path.as_mut_slice()),
                PCWSTR(wide_path.as_ptr()),
            );
            let icon_index = GetPrivateProfileIntW(
                PCWSTR(section.as_ptr()),
                PCWSTR(icon_index_key.as_ptr()),
                0,
                PCWSTR(wide_path.as_ptr()),
            );
            (length, icon_index)
        };
        if length == 0 || length as usize >= icon_path.len().saturating_sub(1) {
            return None;
        }

        let icon_path = path_from_wide_buffer(&icon_path)?;
        expanded_local_file(icon_path).map(|icon_path| (icon_path, icon_index))
    }

    fn nul_terminated_text(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(iter::once(0)).collect()
    }

    fn path_from_wide_buffer(buffer: &[u16]) -> Option<PathBuf> {
        let end = buffer.iter().position(|unit| *unit == 0)?;
        let value = &buffer[..end];
        let quote = b'"' as u16;
        let value =
            if value.len() >= 2 && value.first() == Some(&quote) && value.last() == Some(&quote) {
                &value[1..value.len() - 1]
            } else {
                value
            };
        (!value.is_empty()).then(|| PathBuf::from(OsString::from_wide(value)))
    }

    fn expanded_local_file(path: PathBuf) -> Option<PathBuf> {
        let source = nul_terminated_wide(&path)?;
        // SAFETY: source is NUL-terminated. The first call only requests the
        // required length; the second receives a buffer of exactly that size.
        let expanded = unsafe {
            let required = ExpandEnvironmentStringsW(PCWSTR(source.as_ptr()), None);
            if required == 0 || required as usize > MAX_PATH_CODE_UNITS + 1 {
                return None;
            }
            let mut buffer = vec![0_u16; required as usize];
            let written =
                ExpandEnvironmentStringsW(PCWSTR(source.as_ptr()), Some(buffer.as_mut_slice()));
            if written == 0 || written > required {
                return None;
            }
            path_from_wide_buffer(&buffer)?
        };

        local_regular_file(expanded)
    }

    fn local_regular_file(path: PathBuf) -> Option<PathBuf> {
        let mut components = path.components();
        let local_drive = matches!(
            components.next(),
            Some(Component::Prefix(prefix))
                if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
        );
        if !local_drive
            || !matches!(components.next(), Some(Component::RootDir))
            || path.as_os_str().encode_wide().count() > MAX_PATH_CODE_UNITS
        {
            return None;
        }
        let metadata = std::fs::symlink_metadata(&path).ok()?;
        (metadata.is_file() && !metadata.file_type().is_symlink()).then_some(path)
    }

    fn indexed_icon_image(path: &Path, icon_index: i32) -> Option<(Vec<u8>, u32, u32)> {
        let wide_path = nul_terminated_wide(path)?;
        let mut handle = HICON::default();
        // SAFETY: the path is NUL-terminated and remains live. The successful
        // call transfers one large HICON to the caller, wrapped immediately.
        unsafe {
            SHDefExtractIconW(
                PCWSTR(wide_path.as_ptr()),
                icon_index,
                0,
                Some(&mut handle),
                None,
                ICON_EDGE,
            )
            .ok()
            .ok()?;
        }
        if handle.is_invalid() {
            return None;
        }
        icon_to_rgba(OwnedIcon(handle))
    }

    fn shell_item_image(wide_path: &[u16]) -> Option<(Vec<u8>, u32, u32)> {
        // SAFETY: the path is NUL-terminated and stays alive for the call.
        // The returned COM interface and HBITMAP are released on this STA.
        unsafe {
            let factory: IShellItemImageFactory =
                SHCreateItemFromParsingName(PCWSTR(wide_path.as_ptr()), None).ok()?;
            let bitmap = OwnedBitmap(
                factory
                    .GetImage(
                        SIZE {
                            cx: ICON_EDGE as i32,
                            cy: ICON_EDGE as i32,
                        },
                        SIIGBF_ICONONLY,
                    )
                    .ok()?,
            );
            bitmap_to_rgba(bitmap.0)
        }
    }

    fn bitmap_to_rgba(bitmap: HBITMAP) -> Option<(Vec<u8>, u32, u32)> {
        // SAFETY: bitmap is a live HBITMAP. GetObjectW and GetDIBits only write
        // to the correctly sized structures/buffer supplied here.
        unsafe {
            let mut descriptor = BITMAP::default();
            if GetObjectW(
                bitmap.into(),
                size_of::<BITMAP>() as i32,
                Some((&mut descriptor as *mut BITMAP).cast()),
            ) != size_of::<BITMAP>() as i32
            {
                return None;
            }
            let width = u32::try_from(descriptor.bmWidth).ok()?;
            let height = u32::try_from(descriptor.bmHeight.checked_abs()?).ok()?;
            if width == 0 || height == 0 || width > 512 || height > 512 {
                return None;
            }
            let byte_len = usize::try_from(width)
                .ok()?
                .checked_mul(usize::try_from(height).ok()?)?
                .checked_mul(4)?;
            let mut pixels = vec![0_u8; byte_len];
            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: i32::try_from(width).ok()?,
                    biHeight: -i32::try_from(height).ok()?,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    biSizeImage: u32::try_from(byte_len).ok()?,
                    ..BITMAPINFOHEADER::default()
                },
                ..BITMAPINFO::default()
            };
            let dc = OwnedDc(CreateCompatibleDC(None));
            if dc.0.is_invalid()
                || GetDIBits(
                    dc.0,
                    bitmap,
                    0,
                    height,
                    Some(pixels.as_mut_ptr().cast()),
                    &mut info,
                    DIB_RGB_COLORS,
                ) != height as i32
            {
                return None;
            }
            Some((premultiplied_bgra_to_rgba(&pixels)?, width, height))
        }
    }

    fn shell_icon_fallback(wide_path: &[u16]) -> Option<(Vec<u8>, u32, u32)> {
        let mut info = SHFILEINFOW::default();
        // SAFETY: the path is NUL-terminated and info has the documented size.
        let result = unsafe {
            SHGetFileInfoW(
                PCWSTR(wide_path.as_ptr()),
                FILE_FLAGS_AND_ATTRIBUTES(0),
                Some(&mut info),
                size_of::<SHFILEINFOW>() as u32,
                SHGFI_ICON | SHGFI_LARGEICON,
            )
        };
        if result == 0 || info.hIcon.is_invalid() {
            return None;
        }
        icon_to_rgba(OwnedIcon(info.hIcon))
    }

    fn icon_to_rgba(icon: OwnedIcon) -> Option<(Vec<u8>, u32, u32)> {
        let black = draw_icon(icon.0, 0)?;
        let white = draw_icon(icon.0, u8::MAX)?;
        Some((
            dual_background_bgra_to_rgba(&black, &white)?,
            ICON_EDGE,
            ICON_EDGE,
        ))
    }

    fn draw_icon(icon: HICON, background: u8) -> Option<Vec<u8>> {
        let mut surface = DibSurface::new(ICON_EDGE)?;
        surface.fill(background);
        // SAFETY: the icon and destination DC are live, and the requested
        // dimensions match the backing DIB.
        unsafe {
            DrawIconEx(
                surface.dc.0,
                0,
                0,
                icon,
                ICON_EDGE as i32,
                ICON_EDGE as i32,
                0,
                None,
                DI_NORMAL,
            )
            .ok()?;
        }
        Some(surface.bytes())
    }

    pub(super) fn premultiplied_bgra_to_rgba(bgra: &[u8]) -> Option<Vec<u8>> {
        if bgra.len() % 4 != 0 || !bgra.chunks_exact(4).any(|pixel| pixel[3] != 0) {
            return None;
        }
        let mut rgba = Vec::with_capacity(bgra.len());
        for pixel in bgra.chunks_exact(4) {
            let alpha = pixel[3];
            let unpremultiply = |channel: u8| {
                if alpha == 0 || alpha == u8::MAX {
                    channel
                } else {
                    ((u32::from(channel) * 255 + u32::from(alpha) / 2) / u32::from(alpha)).min(255)
                        as u8
                }
            };
            rgba.extend_from_slice(&[
                unpremultiply(pixel[2]),
                unpremultiply(pixel[1]),
                unpremultiply(pixel[0]),
                alpha,
            ]);
        }
        Some(rgba)
    }

    pub(super) fn dual_background_bgra_to_rgba(black: &[u8], white: &[u8]) -> Option<Vec<u8>> {
        if black.len() != white.len() || black.len() % 4 != 0 {
            return None;
        }
        let mut rgba = Vec::with_capacity(black.len());
        for (dark, light) in black.chunks_exact(4).zip(white.chunks_exact(4)) {
            let difference = (u16::from(light[0].saturating_sub(dark[0]))
                + u16::from(light[1].saturating_sub(dark[1]))
                + u16::from(light[2].saturating_sub(dark[2]))
                + 1)
                / 3;
            let alpha = u8::MAX.saturating_sub(difference.min(255) as u8);
            let unpremultiply = |channel: u8| {
                if alpha == 0 || alpha == u8::MAX {
                    channel
                } else {
                    ((u32::from(channel) * 255 + u32::from(alpha) / 2) / u32::from(alpha)).min(255)
                        as u8
                }
            };
            rgba.extend_from_slice(&[
                unpremultiply(dark[2]),
                unpremultiply(dark[1]),
                unpremultiply(dark[0]),
                alpha,
            ]);
        }
        Some(rgba)
    }

    fn encode_png_data_url(rgba: Vec<u8>, width: u32, height: u32) -> Option<String> {
        let source = RgbaImage::from_raw(width, height, rgba)?;
        let output = if width == ICON_EDGE && height == ICON_EDGE {
            source
        } else {
            let scale = (ICON_EDGE as f64 / width as f64).min(ICON_EDGE as f64 / height as f64);
            let resized_width = ((width as f64 * scale).round() as u32).clamp(1, ICON_EDGE);
            let resized_height = ((height as f64 * scale).round() as u32).clamp(1, ICON_EDGE);
            let resized =
                imageops::resize(&source, resized_width, resized_height, FilterType::Lanczos3);
            let mut canvas = RgbaImage::new(ICON_EDGE, ICON_EDGE);
            imageops::overlay(
                &mut canvas,
                &resized,
                i64::from((ICON_EDGE - resized_width) / 2),
                i64::from((ICON_EDGE - resized_height) / 2),
            );
            canvas
        };

        let mut png = Vec::with_capacity(output.as_raw().len());
        PngEncoder::new(&mut png)
            .write_image(
                output.as_raw(),
                ICON_EDGE,
                ICON_EDGE,
                ColorType::Rgba8.into(),
            )
            .ok()?;
        let data_url = format!("data:image/png;base64,{}", BASE64_STANDARD.encode(png));
        (data_url.len() <= MAX_ICON_DATA_URL_BYTES).then_some(data_url)
    }

    #[cfg(test)]
    pub(super) fn test_encode_png_data_url(
        rgba: Vec<u8>,
        width: u32,
        height: u32,
    ) -> Option<String> {
        encode_png_data_url(rgba, width, height)
    }

    #[cfg(test)]
    pub(super) fn test_create_shortcut(
        shortcut_path: &Path,
        target_path: &Path,
        custom_icon: Option<(&Path, i32)>,
    ) -> Option<()> {
        let shortcut_wide = nul_terminated_wide(shortcut_path)?;
        let target_wide = nul_terminated_wide(target_path)?;
        let custom_icon = match custom_icon {
            Some((path, index)) => Some((nul_terminated_wide(path)?, index)),
            None => None,
        };
        // SAFETY: the caller owns a live STA apartment. The COM objects and
        // NUL-terminated path buffers remain alive for every call.
        unsafe {
            let shortcut: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
            shortcut.SetPath(PCWSTR(target_wide.as_ptr())).ok()?;
            if let Some((icon_path, icon_index)) = custom_icon.as_ref() {
                shortcut
                    .SetIconLocation(PCWSTR(icon_path.as_ptr()), *icon_index)
                    .ok()?;
            }
            let persistence: IPersistFile = shortcut.cast().ok()?;
            persistence
                .Save(PCWSTR(shortcut_wide.as_ptr()), true)
                .ok()?;
        }
        Some(())
    }

    #[cfg(test)]
    pub(super) fn test_shortcut_target_path(path: &Path) -> Option<PathBuf> {
        shortcut_icon_sources(path, &nul_terminated_wide(path)?)?.target
    }

    #[cfg(test)]
    pub(super) fn test_shortcut_custom_icon(path: &Path) -> Option<(PathBuf, i32)> {
        shortcut_icon_sources(path, &nul_terminated_wide(path)?)?.custom_icon
    }

    #[cfg(test)]
    pub(super) fn test_indexed_icon_data_url(path: &Path, index: i32) -> Option<String> {
        let image = indexed_icon_image(path, index)?;
        encode_png_data_url(image.0, image.1, image.2)
    }

    #[cfg(test)]
    pub(super) fn test_internet_shortcut_icon(path: &Path) -> Option<(PathBuf, i32)> {
        internet_shortcut_icon_source(path, &nul_terminated_wide(path)?)
    }
}

/// Pure-Rust, bounded `.icns` extraction for macOS application bundles.
///
/// dTools looks for `App.icns`, `AppIcon.icns`, the bundle name, and finally
/// the first `.icns` resource. We keep that compatibility order, but reject
/// linked resource directories, linked icon files, and containment escapes,
/// and decode only bounded PNG-backed ICNS entries. Modern application bundles
/// ship these PNG representations; older raw/JP2-only files safely fall back
/// to the host placeholder.
#[cfg(any(target_os = "macos", test))]
mod macos_backend {
    use std::{
        fs::{self, File},
        io::{Cursor, Read},
        path::{Path, PathBuf},
    };

    use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
    #[cfg(test)]
    use image::GenericImageView;
    use image::{
        codecs::png::PngEncoder,
        imageops::{self, FilterType},
        ColorType, ImageEncoder, ImageFormat, ImageReader, RgbaImage,
    };

    use super::{ICON_EDGE, MAX_ICON_DATA_URL_BYTES};

    const ICNS_MAGIC: &[u8; 4] = b"icns";
    const PNG_MAGIC: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    const MAX_ICNS_FILE_BYTES: u64 = 16 * 1024 * 1024;
    const MAX_ICNS_PNG_BYTES: usize = 8 * 1024 * 1024;
    const MAX_ICNS_EDGE: u32 = 2_048;
    const MAX_ICNS_PIXELS: u64 = 4_194_304;
    const MAX_ICNS_CHUNKS: usize = 256;
    const MAX_RESOURCE_ENTRIES: usize = 128;

    pub(super) fn application_icon_data_url(bundle: &Path) -> Option<String> {
        let icon_path = icon_resource_path(bundle)?;
        let bytes = read_bounded_file(&icon_path, MAX_ICNS_FILE_BYTES)?;
        icon_data_url_from_icns(&bytes)
    }

    fn is_supported_bundle(bundle: &Path) -> bool {
        bundle
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("app") || extension.eq_ignore_ascii_case("prefPane")
            })
    }

    fn icon_resource_path(bundle: &Path) -> Option<PathBuf> {
        if !bundle.is_absolute() || !is_supported_bundle(bundle) {
            return None;
        }
        let bundle_metadata = fs::metadata(bundle).ok()?;
        if !bundle_metadata.is_dir() {
            return None;
        }
        let canonical_bundle = fs::canonicalize(bundle).ok()?;
        let resources = bundle.join("Contents/Resources");
        let resource_metadata = fs::symlink_metadata(&resources).ok()?;
        if resource_metadata.file_type().is_symlink() || !resource_metadata.is_dir() {
            return None;
        }
        let canonical_resources = fs::canonicalize(&resources).ok()?;
        if !canonical_resources.starts_with(&canonical_bundle) {
            return None;
        }

        let bundle_stem = bundle.file_stem()?.to_string_lossy();
        let compact_stem = bundle_stem
            .chars()
            .filter(|value| *value != ' ')
            .collect::<String>();
        let mut preferred = vec![
            "App.icns".to_owned(),
            "AppIcon.icns".to_owned(),
            format!("{bundle_stem}.icns"),
        ];
        if compact_stem != bundle_stem {
            preferred.push(format!("{compact_stem}.icns"));
        }
        for name in preferred {
            let candidate = resources.join(name);
            if let Some(verified) = verify_icon_resource(&canonical_resources, &candidate) {
                return Some(verified);
            }
        }

        let mut entries = fs::read_dir(&resources)
            .ok()?
            .filter_map(Result::ok)
            .take(MAX_RESOURCE_ENTRIES + 1)
            .collect::<Vec<_>>();
        if entries.len() > MAX_RESOURCE_ENTRIES {
            return None;
        }
        entries.sort_unstable_by_key(fs::DirEntry::file_name);
        entries.into_iter().find_map(|entry| {
            let path = entry.path();
            let is_icns = path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("icns"));
            is_icns
                .then(|| verify_icon_resource(&canonical_resources, &path))
                .flatten()
        })
    }

    fn verify_icon_resource(canonical_resources: &Path, candidate: &Path) -> Option<PathBuf> {
        let metadata = fs::symlink_metadata(candidate).ok()?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() == 0
            || metadata.len() > MAX_ICNS_FILE_BYTES
        {
            return None;
        }
        let canonical = fs::canonicalize(candidate).ok()?;
        canonical
            .starts_with(canonical_resources)
            .then_some(canonical)
    }

    fn read_bounded_file(path: &Path, limit: u64) -> Option<Vec<u8>> {
        let mut bytes = Vec::new();
        File::open(path)
            .ok()?
            .take(limit.saturating_add(1))
            .read_to_end(&mut bytes)
            .ok()?;
        (bytes.len() as u64 <= limit).then_some(bytes)
    }

    fn icon_data_url_from_icns(bytes: &[u8]) -> Option<String> {
        if bytes.len() < 8 || &bytes[..4] != ICNS_MAGIC {
            return None;
        }
        let declared_length = u32::from_be_bytes(bytes[4..8].try_into().ok()?) as usize;
        if declared_length != bytes.len() {
            return None;
        }

        let mut cursor = 8_usize;
        let mut chunk_count = 0_usize;
        let mut best: Option<(&[u8], u64)> = None;
        while cursor < bytes.len() {
            chunk_count = chunk_count.checked_add(1)?;
            if chunk_count > MAX_ICNS_CHUNKS {
                return None;
            }
            let header_end = cursor.checked_add(8)?;
            if header_end > bytes.len() {
                return None;
            }
            let block_length =
                u32::from_be_bytes(bytes[cursor + 4..header_end].try_into().ok()?) as usize;
            if block_length < 8 {
                return None;
            }
            let block_end = cursor.checked_add(block_length)?;
            if block_end > bytes.len() {
                return None;
            }
            let payload = &bytes[header_end..block_end];
            if payload.len() <= MAX_ICNS_PNG_BYTES && payload.starts_with(PNG_MAGIC) {
                let reader = ImageReader::with_format(Cursor::new(payload), ImageFormat::Png);
                if let Ok((width, height)) = reader.into_dimensions() {
                    let pixels = u64::from(width).saturating_mul(u64::from(height));
                    if width > 0
                        && height > 0
                        && width <= MAX_ICNS_EDGE
                        && height <= MAX_ICNS_EDGE
                        && pixels <= MAX_ICNS_PIXELS
                        && best.map_or(true, |(_, best_pixels)| pixels > best_pixels)
                    {
                        best = Some((payload, pixels));
                    }
                }
            }
            cursor = block_end;
        }

        let (payload, _) = best?;
        let image = image::load_from_memory_with_format(payload, ImageFormat::Png)
            .ok()?
            .into_rgba8();
        encode_png_data_url(image)
    }

    fn encode_png_data_url(source: RgbaImage) -> Option<String> {
        let (width, height) = source.dimensions();
        if width == 0 || height == 0 {
            return None;
        }
        let output = if width == ICON_EDGE && height == ICON_EDGE {
            source
        } else {
            let scale = (ICON_EDGE as f64 / width as f64).min(ICON_EDGE as f64 / height as f64);
            let resized_width = ((width as f64 * scale).round() as u32).clamp(1, ICON_EDGE);
            let resized_height = ((height as f64 * scale).round() as u32).clamp(1, ICON_EDGE);
            let resized =
                imageops::resize(&source, resized_width, resized_height, FilterType::Lanczos3);
            let mut canvas = RgbaImage::new(ICON_EDGE, ICON_EDGE);
            imageops::overlay(
                &mut canvas,
                &resized,
                i64::from((ICON_EDGE - resized_width) / 2),
                i64::from((ICON_EDGE - resized_height) / 2),
            );
            canvas
        };

        let mut png = Vec::with_capacity(output.as_raw().len());
        PngEncoder::new(&mut png)
            .write_image(
                output.as_raw(),
                ICON_EDGE,
                ICON_EDGE,
                ColorType::Rgba8.into(),
            )
            .ok()?;
        let data_url = format!("data:image/png;base64,{}", BASE64_STANDARD.encode(png));
        (data_url.len() <= MAX_ICON_DATA_URL_BYTES).then_some(data_url)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn test_png(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
            let image = RgbaImage::from_pixel(width, height, image::Rgba(rgba));
            let mut png = Vec::new();
            PngEncoder::new(&mut png)
                .write_image(image.as_raw(), width, height, ColorType::Rgba8.into())
                .expect("encode test png");
            png
        }

        fn test_icns(chunks: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
            let total = 8_usize
                + chunks
                    .iter()
                    .map(|(_, payload)| 8_usize + payload.len())
                    .sum::<usize>();
            let mut bytes = Vec::with_capacity(total);
            bytes.extend_from_slice(ICNS_MAGIC);
            bytes.extend_from_slice(&(total as u32).to_be_bytes());
            for (kind, payload) in chunks {
                bytes.extend_from_slice(*kind);
                bytes.extend_from_slice(&((payload.len() + 8) as u32).to_be_bytes());
                bytes.extend_from_slice(payload);
            }
            bytes
        }

        #[test]
        fn selects_the_largest_bounded_png_and_normalizes_to_48_pixels() {
            let small = test_png(16, 16, [255, 0, 0, 255]);
            let large = test_png(256, 128, [0, 128, 255, 255]);
            let bytes = test_icns(&[(b"icp4", small), (b"ic08", large)]);
            let data_url = icon_data_url_from_icns(&bytes).expect("decoded icon");
            let decoded = BASE64_STANDARD
                .decode(
                    data_url
                        .strip_prefix("data:image/png;base64,")
                        .expect("png data url"),
                )
                .expect("base64");
            let image = image::load_from_memory_with_format(&decoded, ImageFormat::Png)
                .expect("normalized png");
            assert_eq!(image.dimensions(), (ICON_EDGE, ICON_EDGE));
            let center = image.to_rgba8().get_pixel(24, 24).0;
            assert_eq!(center, [0, 128, 255, 255]);
        }

        #[test]
        fn rejects_malformed_lengths_and_unbounded_dimensions() {
            let mut malformed = test_icns(&[(b"ic08", test_png(32, 32, [1, 2, 3, 255]))]);
            malformed[7] = malformed[7].wrapping_add(1);
            assert!(icon_data_url_from_icns(&malformed).is_none());

            let huge_header_only = {
                let mut png = test_png(1, 1, [1, 2, 3, 255]);
                // PNG IHDR width and height are big-endian at offsets 16/20.
                png[16..20].copy_from_slice(&4_096_u32.to_be_bytes());
                png[20..24].copy_from_slice(&4_096_u32.to_be_bytes());
                test_icns(&[(b"ic10", png)])
            };
            assert!(icon_data_url_from_icns(&huge_header_only).is_none());
        }

        #[test]
        fn rejects_an_excessive_number_of_icns_chunks() {
            let chunks = (0..=MAX_ICNS_CHUNKS)
                .map(|_| (b"TOC ", Vec::new()))
                .collect::<Vec<_>>();
            assert!(icon_data_url_from_icns(&test_icns(&chunks)).is_none());
        }

        #[test]
        fn resolves_dtools_compatible_bundle_candidates_without_leaving_resources() {
            let bundle =
                std::env::temp_dir().join(format!("ihub-macos-icon-{}.app", uuid::Uuid::new_v4()));
            let outside_icon = std::env::temp_dir().join(format!(
                "ihub-macos-icon-outside-{}.icns",
                uuid::Uuid::new_v4()
            ));
            let resources = bundle.join("Contents/Resources");
            fs::create_dir_all(&resources).expect("create resources");
            let icns = test_icns(&[(b"ic08", test_png(64, 64, [24, 80, 160, 255]))]);
            fs::write(resources.join("AppIcon.icns"), &icns).expect("write icns");
            fs::write(&outside_icon, icns).expect("write outside icns");

            let resolved = icon_resource_path(&bundle).expect("resolve icon");
            assert_eq!(
                resolved.file_name().and_then(|value| value.to_str()),
                Some("AppIcon.icns")
            );
            let canonical_resources =
                fs::canonicalize(&resources).expect("canonical resources directory");
            assert!(verify_icon_resource(&canonical_resources, &outside_icon).is_none());
            assert!(application_icon_data_url(&bundle)
                .expect("bundle icon")
                .starts_with("data:image/png;base64,"));

            fs::remove_file(outside_icon).expect("cleanup outside icon");
            fs::remove_dir_all(&bundle).expect("cleanup bundle");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key(name: &str, size: u64) -> IconCacheKey {
        IconCacheKey {
            path: PathBuf::from(name),
            kind: "file".to_owned(),
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(size)),
            size: Some(size),
            edge: ICON_EDGE,
        }
    }

    #[test]
    fn input_requires_an_absolute_native_path_and_known_kind() {
        let executable = std::env::current_exe().expect("current executable");
        assert!(valid_icon_input(&executable, "application"));
        assert!(valid_icon_input(&executable, "file"));
        assert!(!valid_icon_input(&executable, "plugin"));
        assert!(!valid_icon_input(Path::new("relative.exe"), "application"));
        assert!(!valid_icon_input(Path::new(""), "file"));
    }

    #[test]
    fn cache_key_includes_exact_path_kind_metadata_and_edge() {
        let base = test_key("C:/one/example.exe", 42);
        let mut changed = base.clone();
        changed.path = PathBuf::from("C:/two/example.exe");
        assert_ne!(base, changed);
        changed = base.clone();
        changed.kind = "application".to_owned();
        assert_ne!(base, changed);
        changed = base.clone();
        changed.modified = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(43));
        assert_ne!(base, changed);
        changed = base.clone();
        changed.size = Some(43);
        assert_ne!(base, changed);
        changed = base.clone();
        changed.edge = 32;
        assert_ne!(base, changed);
    }

    #[test]
    fn cache_obeys_positive_and_negative_ttls() {
        let now = Instant::now();
        let mut cache = IconCache::new(4, 1024, Duration::from_secs(10), Duration::from_secs(2));
        let positive = test_key("positive", 1);
        let negative = test_key("negative", 2);
        cache.insert(positive.clone(), Some("png".to_owned()), now);
        cache.insert(negative.clone(), None, now);

        assert_eq!(
            cache.get(&positive, now + Duration::from_secs(9)),
            Some(Some("png".to_owned()))
        );
        assert_eq!(
            cache.get(&negative, now + Duration::from_secs(1)),
            Some(None)
        );
        assert_eq!(cache.get(&negative, now + Duration::from_secs(2)), None);
        assert_eq!(cache.get(&positive, now + Duration::from_secs(10)), None);
    }

    #[test]
    fn cache_bounds_entries_and_total_bytes_with_lru_eviction() {
        let now = Instant::now();
        let mut cache = IconCache::new(2, 7, Duration::from_secs(10), Duration::from_secs(2));
        let first = test_key("first", 1);
        let second = test_key("second", 2);
        let third = test_key("third", 3);
        cache.insert(first.clone(), Some("111".to_owned()), now);
        cache.insert(second.clone(), Some("222".to_owned()), now);
        assert_eq!(cache.get(&first, now), Some(Some("111".to_owned())));
        cache.insert(third.clone(), Some("3333".to_owned()), now);

        assert!(cache.values.len() <= 2);
        assert!(cache.total_bytes <= 7);
        assert!(cache.get(&first, now).is_some());
        assert!(cache.get(&second, now).is_none());
        assert!(cache.get(&third, now).is_some());
    }

    #[test]
    fn oversized_values_are_not_cached() {
        let now = Instant::now();
        let mut cache = IconCache::new(2, 3, Duration::from_secs(10), Duration::from_secs(2));
        let key = test_key("large", 1);
        cache.insert(key.clone(), Some("1234".to_owned()), now);
        assert_eq!(cache.get(&key, now), None);
        assert_eq!(cache.total_bytes, 0);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn alpha_conversion_preserves_black_and_unpremultiplies_edges() {
        let bgra = [
            0, 0, 0, 255, // opaque black
            32, 64, 96, 128, // premultiplied color
            0, 0, 0, 0, // transparent
        ];
        let rgba = windows_backend::premultiplied_bgra_to_rgba(&bgra).expect("valid alpha");
        assert_eq!(&rgba[0..4], &[0, 0, 0, 255]);
        assert_eq!(&rgba[4..8], &[191, 128, 64, 128]);
        assert_eq!(&rgba[8..12], &[0, 0, 0, 0]);
        assert!(windows_backend::premultiplied_bgra_to_rgba(&[0, 0, 0, 0]).is_none());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn dual_background_alpha_preserves_opaque_black() {
        let black = [
            0, 0, 0, 255, // opaque black
            0, 0, 0, 255, // transparent over black
            0, 0, 100, 255, // half-transparent red over black
        ];
        let white = [
            0, 0, 0, 255, // opaque black
            255, 255, 255, 255, // transparent over white
            127, 127, 227, 255, // half-transparent red over white
        ];
        let rgba =
            windows_backend::dual_background_bgra_to_rgba(&black, &white).expect("valid pair");
        assert_eq!(&rgba[0..4], &[0, 0, 0, 255]);
        assert_eq!(&rgba[4..8], &[0, 0, 0, 0]);
        assert_eq!(&rgba[8..12], &[199, 0, 0, 128]);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn png_encoder_always_emits_a_bounded_48_pixel_image() {
        use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};

        let data_url = windows_backend::test_encode_png_data_url(vec![0, 0, 0, 255], 1, 1)
            .expect("PNG data URL");
        assert!(data_url.len() <= MAX_ICON_DATA_URL_BYTES);
        let encoded = data_url
            .strip_prefix("data:image/png;base64,")
            .expect("safe PNG prefix");
        let png = BASE64_STANDARD.decode(encoded).expect("valid base64");
        let decoded = image::load_from_memory(&png).expect("valid PNG");
        assert_eq!((decoded.width(), decoded.height()), (ICON_EDGE, ICON_EDGE));
        assert_eq!(decoded.to_rgba8().get_pixel(24, 24).0, [0, 0, 0, 255]);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn shortcut_icon_resolves_to_its_local_target() {
        struct TempShortcut(PathBuf);

        impl Drop for TempShortcut {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }

        let _apartment =
            windows_backend::StaApartment::initialize().expect("Windows STA apartment");
        let target = std::env::current_exe().expect("current executable");
        let shortcut = TempShortcut(
            std::env::temp_dir().join(format!("ihub-native-icon-{}.lnk", uuid::Uuid::new_v4())),
        );
        windows_backend::test_create_shortcut(&shortcut.0, &target, None)
            .expect("create test shortcut");

        let resolved =
            windows_backend::test_shortcut_target_path(&shortcut.0).expect("shortcut target");
        assert_eq!(
            std::fs::canonicalize(resolved).expect("resolved target"),
            std::fs::canonicalize(&target).expect("expected target")
        );
        assert_eq!(
            windows_backend::icon_data_url(&shortcut.0),
            windows_backend::icon_data_url(&target)
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn shortcut_icon_honors_its_native_custom_icon_resource() {
        struct TempShortcut(PathBuf);

        impl Drop for TempShortcut {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }

        let _apartment =
            windows_backend::StaApartment::initialize().expect("Windows STA apartment");
        let target = std::env::current_exe().expect("current executable");
        let custom_icon = Path::new(env!("CARGO_MANIFEST_DIR")).join("icons/icon.ico");
        let shortcut = TempShortcut(
            std::env::temp_dir().join(format!("ihub-custom-icon-{}.lnk", uuid::Uuid::new_v4())),
        );
        windows_backend::test_create_shortcut(&shortcut.0, &target, Some((&custom_icon, 0)))
            .expect("create custom-icon shortcut");

        let resolved_icon =
            windows_backend::test_shortcut_custom_icon(&shortcut.0).expect("shortcut custom icon");
        assert_eq!(
            std::fs::canonicalize(resolved_icon.0).expect("resolved icon"),
            std::fs::canonicalize(&custom_icon).expect("expected icon")
        );
        assert_eq!(resolved_icon.1, 0);
        assert_eq!(
            windows_backend::icon_data_url(&shortcut.0),
            windows_backend::test_indexed_icon_data_url(&custom_icon, 0)
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn internet_shortcut_icon_honors_icon_file_and_index() {
        struct TempInternetShortcut(PathBuf);

        impl Drop for TempInternetShortcut {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }

        let _apartment =
            windows_backend::StaApartment::initialize().expect("Windows STA apartment");
        let custom_icon = Path::new(env!("CARGO_MANIFEST_DIR")).join("icons/icon.ico");
        let shortcut = TempInternetShortcut(
            std::env::temp_dir().join(format!("ihub-custom-icon-{}.url", uuid::Uuid::new_v4())),
        );
        let source = format!(
            "[InternetShortcut]\r\nURL=https://example.invalid/\r\nIconFile={}\r\nIconIndex=0\r\n",
            custom_icon.display()
        );
        let mut encoded = vec![0xff, 0xfe];
        encoded.extend(
            source
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        std::fs::write(&shortcut.0, encoded).expect("write Internet Shortcut");

        let resolved_icon = windows_backend::test_internet_shortcut_icon(&shortcut.0)
            .expect("Internet Shortcut custom icon");
        assert_eq!(
            std::fs::canonicalize(resolved_icon.0).expect("resolved icon"),
            std::fs::canonicalize(&custom_icon).expect("expected icon")
        );
        assert_eq!(resolved_icon.1, 0);
        assert_eq!(
            windows_backend::icon_data_url(&shortcut.0),
            windows_backend::test_indexed_icon_data_url(&custom_icon, 0)
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn sta_service_extracts_a_real_shell_item_icon() {
        let shell_item = std::env::var_os("IHUB_NATIVE_ICON_TEST_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_exe().expect("current executable"));
        let service = NativeIconService::new();
        let pending = service
            .try_request(&shell_item, "application")
            .expect("STA worker accepted request");
        let icon = pending
            .wait_timeout(Duration::from_secs(5))
            .expect("Windows Shell executable icon");
        assert!(icon.starts_with("data:image/png;base64,"));
        assert!(icon.len() <= MAX_ICON_DATA_URL_BYTES);

        if shell_item
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("lnk"))
        {
            let _apartment =
                windows_backend::StaApartment::initialize().expect("Windows STA apartment");
            if let Some((custom_icon, icon_index)) =
                windows_backend::test_shortcut_custom_icon(&shell_item)
            {
                assert_eq!(
                    Some(icon),
                    windows_backend::test_indexed_icon_data_url(&custom_icon, icon_index),
                    "a launcher shortcut must honor its native custom icon resource"
                );
            } else {
                let target = windows_backend::test_shortcut_target_path(&shell_item)
                    .expect("local shortcut target");
                let target_icon = service
                    .try_request(&target, "application")
                    .expect("STA worker accepted target request")
                    .wait_timeout(Duration::from_secs(5))
                    .expect("Windows Shell target icon");
                assert_eq!(
                    icon, target_icon,
                    "a launcher shortcut must render the same glyph as its target EXE"
                );
            }
        }
    }
}
