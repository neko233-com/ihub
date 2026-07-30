//! Privacy-first, opt-in clipboard history.
//!
//! Text capture remains the default history mode. Image and file-list capture
//! are independent, explicit opt-ins: image pixels are stored as bounded local
//! PNG files, while file entries retain only native-private references and
//! small display metadata. Nothing here opens a file or materializes an image
//! into the renderer without a separate, user-triggered command.

use std::{
    borrow::Cow,
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, Cursor, Read, Write},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    time::UNIX_EPOCH,
};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono::Utc;
use image::{codecs::png::PngEncoder, ColorType, ImageEncoder};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    models::ClipboardImage,
    system_open::{LocalOpenKind, PreparedLocalOpen},
};

const HISTORY_FILE_NAME: &str = "clipboard-history-v2.json";
const LEGACY_HISTORY_FILE_NAME: &str = "clipboard-history-v1.json";
const IMAGE_DIRECTORY_NAME: &str = "clipboard-history-images-v2";
const MAX_HISTORY_ITEMS: usize = 100;
const MAX_CAPTURED_TEXT_BYTES: usize = 100_000;
const MAX_IMAGE_HISTORY_ITEMS: usize = 12;
const MAX_FILE_HISTORY_ITEMS: usize = 32;
const MAX_FILES_PER_HISTORY_ITEM: usize = 16;
const MAX_HISTORY_IMAGE_EDGE: usize = 4_096;
const MAX_HISTORY_IMAGE_PIXELS: usize = 8_000_000;
const MAX_HISTORY_IMAGE_RAW_BYTES: usize = 32 * 1024 * 1024;
const MAX_HISTORY_IMAGE_PNG_BYTES: usize = 4 * 1024 * 1024;
const MAX_HISTORY_FILE_NAME_CHARS: usize = 180;
const MAX_HISTORY_FILE_PATH_CHARS: usize = 2_048;
// Windows preflight permits enough UTF-16 source data for any persisted
// 100 KiB UTF-8 string, while keeping arboard's conversion allocation bounded
// even when a source text value is ultimately rejected by the history cap.
const MAX_CLIPBOARD_TEXT_SOURCE_BYTES: usize = MAX_CAPTURED_TEXT_BYTES * 2 + 2;
// DIBV5 includes a 124-byte header; leave a small fixed margin for equivalent
// native source metadata while still bounding arboard's input allocation.
const MAX_CLIPBOARD_IMAGE_SOURCE_BYTES: usize = MAX_HISTORY_IMAGE_RAW_BYTES + 4_096;
// A 256 KiB HDROP source admits the bounded file-list history without letting
// a background poll create an arbitrarily large PathBuf vector.
const MAX_CLIPBOARD_FILE_LIST_SOURCE_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ClipboardHistoryItemKind {
    #[default]
    Text,
    Image,
    Files,
}

/// Deliberately small image metadata exposed to the WebView. The native image
/// filename and its pixel checksum stay in the persisted host record.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardHistoryImageMetadata {
    pub width: u32,
    pub height: u32,
    pub byte_length: u64,
}

/// A file-list item exposed to the WebView. Paths and fingerprints are kept
/// native-only so a renderer cannot turn history display into an arbitrary
/// open-path capability.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardHistoryFileMetadata {
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardHistoryItem {
    pub id: String,
    pub kind: ClipboardHistoryItemKind,
    /// Present for text records only. Keeping an empty string for non-text
    /// records preserves the v1 renderer shape without exposing binary data.
    pub text: String,
    pub captured_at: String,
    pub pinned: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<ClipboardHistoryImageMetadata>,
    pub files: Vec<ClipboardHistoryFileMetadata>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardHistorySnapshot {
    pub enabled: bool,
    pub image_history_enabled: bool,
    pub file_history_enabled: bool,
    pub items: Vec<ClipboardHistoryItem>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardHistoryRestoreResult {
    pub kind: ClipboardHistoryItemKind,
    pub restored_count: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredClipboardHistoryImage {
    file_name: String,
    width: u32,
    height: u32,
    byte_length: u64,
    /// A raw-PNG digest is distinct from the pixel digest below. It protects
    /// explicit preview/restore reads before handing the encoded bytes to a
    /// decoder or the renderer. Older v2 records did not have this field and
    /// are retained only behind the pixel-digest compatibility check.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    png_digest: String,
    digest: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct StoredClipboardHistoryFile {
    path: String,
    name: String,
    kind: String,
    /// File fingerprints make a later restore fail closed if the original
    /// file changed under the same path. Directories are still canonicalized
    /// and type-checked, but their mutable timestamps are intentionally not
    /// used as a fingerprint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    byte_length: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    modified_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredClipboardHistoryItem {
    id: String,
    #[serde(default)]
    kind: ClipboardHistoryItemKind,
    #[serde(default)]
    text: String,
    captured_at: String,
    pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image: Option<StoredClipboardHistoryImage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    files: Vec<StoredClipboardHistoryFile>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedClipboardHistory {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    image_history_enabled: bool,
    #[serde(default)]
    file_history_enabled: bool,
    #[serde(default)]
    items: Vec<StoredClipboardHistoryItem>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyPersistedClipboardHistory {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    items: Vec<LegacyClipboardHistoryItem>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyClipboardHistoryItem {
    id: String,
    text: String,
    captured_at: String,
    pinned: bool,
}

enum ClipboardPollValue<T> {
    Present(T),
    Absent,
    /// An unsupported or temporarily unreadable format must not be mistaken
    /// for a real clipboard transition. Keeping the previous fingerprint
    /// avoids a later transient error causing a duplicate history insertion.
    Unavailable,
}

struct ClipboardPollSample {
    text: ClipboardPollValue<String>,
    image: ClipboardPollValue<arboard::ImageData<'static>>,
    files: ClipboardPollValue<Vec<PathBuf>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClipboardPollTextFingerprint {
    Absent,
    Capturable { byte_length: usize, digest: String },
    Rejected { byte_length: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClipboardPollImageFingerprint {
    Absent,
    Capturable {
        width: usize,
        height: usize,
        digest: String,
    },
    Rejected {
        width: usize,
        height: usize,
        byte_length: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClipboardPollFilesFingerprint {
    Absent,
    Capturable { count: usize, digest: String },
    Rejected { count: usize },
}

#[derive(Debug, Default)]
struct ClipboardPollState {
    text: Option<ClipboardPollTextFingerprint>,
    image: Option<ClipboardPollImageFingerprint>,
    files: Option<ClipboardPollFilesFingerprint>,
}

/// A deliberately opt-in local clipboard history. Text is the only default
/// capture mode. Image and file-list capture must each be enabled after text;
/// no clipboard raw file contents are read, copied, or persisted.
#[derive(Clone)]
pub struct ClipboardHistory {
    data_path: Arc<PathBuf>,
    image_directory: Arc<PathBuf>,
    state: Arc<Mutex<PersistedClipboardHistory>>,
    /// Poll fingerprints are deliberately memory-only. They prevent a static
    /// system clipboard from becoming a 750ms persistence loop, while a
    /// restart or an intervening different value still produces a normal
    /// history transition.
    poll_state: Arc<Mutex<ClipboardPollState>>,
}

impl ClipboardHistory {
    pub fn new(app_data_dir: PathBuf) -> Self {
        let data_path = app_data_dir.join(HISTORY_FILE_NAME);
        let image_directory = app_data_dir.join(IMAGE_DIRECTORY_NAME);
        let mut state =
            load_history_state(&data_path, &app_data_dir.join(LEGACY_HISTORY_FILE_NAME));
        let _ = trim_to_limits(&mut state.items);
        Self {
            data_path: Arc::new(data_path),
            image_directory: Arc::new(image_directory),
            state: Arc::new(Mutex::new(state)),
            poll_state: Arc::new(Mutex::new(ClipboardPollState::default())),
        }
    }

    pub fn snapshot(&self, limit: Option<usize>) -> ClipboardHistorySnapshot {
        let state = self.lock_state();
        snapshot_from_state(&state, limit, false)
    }

    /// Plugins receive only the long-standing text projection. This keeps
    /// image pixels, file paths, file names, and type metadata host-private
    /// even for a plugin granted the narrow history-snapshot permission.
    pub fn text_snapshot(&self, limit: Option<usize>) -> ClipboardHistorySnapshot {
        let state = self.lock_state();
        snapshot_from_state(&state, limit, true)
    }

    pub fn set_enabled(&self, enabled: bool) -> Result<ClipboardHistorySnapshot, String> {
        let mut state = self.lock_state();
        let original = state.clone();
        let poll_configuration_changed = state.enabled != enabled
            || (!enabled && (state.image_history_enabled || state.file_history_enabled));
        state.enabled = enabled;
        // Disabling history is a privacy boundary: a later re-enable starts
        // with text only, so images/files always require fresh explicit consent.
        if !enabled {
            state.image_history_enabled = false;
            state.file_history_enabled = false;
        }
        if let Err(error) = self.persist(&state) {
            *state = original;
            return Err(error);
        }
        drop(state);
        if poll_configuration_changed {
            self.reset_poll_state();
        }
        Ok(self.snapshot(Some(MAX_HISTORY_ITEMS)))
    }

    pub fn set_capture_options(
        &self,
        image_history_enabled: bool,
        file_history_enabled: bool,
    ) -> Result<ClipboardHistorySnapshot, String> {
        let mut state = self.lock_state();
        if !state.enabled {
            return Err(
                "Enable local text history before enabling image or file history.".to_owned(),
            );
        }
        let original = state.clone();
        let poll_configuration_changed = state.image_history_enabled != image_history_enabled
            || state.file_history_enabled != file_history_enabled;
        state.image_history_enabled = image_history_enabled;
        state.file_history_enabled = file_history_enabled;
        if let Err(error) = self.persist(&state) {
            *state = original;
            return Err(error);
        }
        drop(state);
        if poll_configuration_changed {
            self.reset_poll_state();
        }
        Ok(self.snapshot(Some(MAX_HISTORY_ITEMS)))
    }

    /// Samples only formats that the user explicitly enabled. The short native
    /// clipboard closure returns owned values; image encoding, filesystem
    /// metadata, and persistence all happen after the clipboard is released.
    pub fn poll_system_clipboard(&self) {
        let (enabled, images_enabled, files_enabled) = {
            let state = self.lock_state();
            (
                state.enabled,
                state.image_history_enabled,
                state.file_history_enabled,
            )
        };
        if !enabled {
            return;
        }

        let limits = crate::clipboard_access::BackgroundClipboardReadLimits {
            max_text_source_bytes: MAX_CLIPBOARD_TEXT_SOURCE_BYTES,
            image: images_enabled.then_some(
                crate::clipboard_access::BackgroundClipboardImageLimits {
                    max_source_bytes: MAX_CLIPBOARD_IMAGE_SOURCE_BYTES,
                    max_edge: MAX_HISTORY_IMAGE_EDGE,
                    max_pixels: MAX_HISTORY_IMAGE_PIXELS,
                    max_rgba_bytes: MAX_HISTORY_IMAGE_RAW_BYTES,
                },
            ),
            max_file_list_source_bytes: files_enabled
                .then_some(MAX_CLIPBOARD_FILE_LIST_SOURCE_BYTES),
        };
        let Some(Ok(sample)) =
            crate::clipboard_access::try_with_bounded_background_clipboard(limits, |clipboard| {
                Ok(ClipboardPollSample {
                    text: clipboard_poll_value(clipboard.get_text()),
                    image: if images_enabled {
                        clipboard_poll_value(clipboard.get_image())
                    } else {
                        ClipboardPollValue::Unavailable
                    },
                    files: if files_enabled {
                        clipboard_poll_value(clipboard.get().file_list())
                    } else {
                        ClipboardPollValue::Unavailable
                    },
                })
            })
        else {
            return;
        };

        self.capture_polled_sample(sample, images_enabled, files_enabled);
    }

    /// Applies a sampled clipboard value only when that *format* transitioned.
    /// The memory-only state is committed after a successful capture (or a
    /// deliberate rejection such as an empty/oversize value), so transient
    /// persistence errors can retry without turning a static clipboard into a
    /// repeated ID/timestamp refresh.
    fn capture_polled_sample(
        &self,
        sample: ClipboardPollSample,
        images_enabled: bool,
        files_enabled: bool,
    ) {
        self.capture_polled_text(sample.text);
        if images_enabled {
            self.capture_polled_image(sample.image);
        }
        if files_enabled {
            self.capture_polled_files(sample.files);
        }
    }

    fn capture_polled_text(&self, value: ClipboardPollValue<String>) {
        let (fingerprint, text) = match value {
            ClipboardPollValue::Present(text) => (clipboard_text_fingerprint(&text), Some(text)),
            ClipboardPollValue::Absent => (ClipboardPollTextFingerprint::Absent, None),
            ClipboardPollValue::Unavailable => return,
        };
        let mut poll_state = self.lock_poll_state();
        if poll_state.text.as_ref() == Some(&fingerprint) {
            return;
        }
        let result = text.map_or(Ok(false), |text| self.capture_text(text));
        if result.is_ok() {
            poll_state.text = Some(fingerprint);
        }
    }

    fn capture_polled_image(&self, value: ClipboardPollValue<arboard::ImageData<'static>>) {
        let (fingerprint, image) = match value {
            ClipboardPollValue::Present(image) => {
                let fingerprint = clipboard_image_fingerprint(&image);
                let should_capture = matches!(
                    &fingerprint,
                    ClipboardPollImageFingerprint::Capturable { .. }
                );
                (fingerprint, should_capture.then_some(image))
            }
            ClipboardPollValue::Absent => (ClipboardPollImageFingerprint::Absent, None),
            ClipboardPollValue::Unavailable => return,
        };
        let mut poll_state = self.lock_poll_state();
        if poll_state.image.as_ref() == Some(&fingerprint) {
            return;
        }
        let result = image.map_or(Ok(false), |image| self.capture_image(image));
        if result.is_ok() {
            poll_state.image = Some(fingerprint);
        }
    }

    fn capture_polled_files(&self, value: ClipboardPollValue<Vec<PathBuf>>) {
        let (fingerprint, files) = match value {
            ClipboardPollValue::Present(paths) if paths.is_empty() => {
                (ClipboardPollFilesFingerprint::Absent, None)
            }
            ClipboardPollValue::Present(paths) => {
                let count = paths.len();
                // Preserve the history's existing bounded-list behavior, but
                // do not walk an arbitrarily long OS list looking for later
                // valid paths after Windows/macOS returned it to us.
                let files = stored_files_from_paths(
                    paths.into_iter().take(MAX_FILES_PER_HISTORY_ITEM).collect(),
                );
                if files.is_empty() {
                    (ClipboardPollFilesFingerprint::Rejected { count }, None)
                } else {
                    (
                        ClipboardPollFilesFingerprint::Capturable {
                            count: files.len(),
                            digest: stored_files_digest(&files),
                        },
                        Some(files),
                    )
                }
            }
            ClipboardPollValue::Absent => (ClipboardPollFilesFingerprint::Absent, None),
            ClipboardPollValue::Unavailable => return,
        };
        let mut poll_state = self.lock_poll_state();
        if poll_state.files.as_ref() == Some(&fingerprint) {
            return;
        }
        let result = files.map_or(Ok(false), |files| self.capture_stored_files(files));
        if result.is_ok() {
            poll_state.files = Some(fingerprint);
        }
    }

    pub fn capture_text(&self, text: String) -> Result<bool, String> {
        if text.trim().is_empty() || text.len() > MAX_CAPTURED_TEXT_BYTES {
            return Ok(false);
        }

        let mut state = self.lock_state();
        if !state.enabled {
            return Ok(false);
        }

        let original = state.clone();
        let pinned = state
            .items
            .iter()
            .position(|item| item.kind == ClipboardHistoryItemKind::Text && item.text == text)
            .map(|index| state.items.remove(index).pinned)
            .unwrap_or(false);
        state.items.insert(
            0,
            StoredClipboardHistoryItem {
                id: new_history_id(),
                kind: ClipboardHistoryItemKind::Text,
                text,
                captured_at: Utc::now().to_rfc3339(),
                pinned,
                image: None,
                files: Vec::new(),
            },
        );
        let removed_images = trim_to_limits(&mut state.items);
        if let Err(error) = self.persist(&state) {
            *state = original;
            return Err(error);
        }
        drop(state);
        self.cleanup_unreferenced_images(&removed_images);
        Ok(true)
    }

    /// Stores a bounded PNG only after image history has been explicitly
    /// enabled. The renderer never receives this PNG during polling; a user
    /// must request a preview or restoration later.
    pub fn capture_image(&self, image: arboard::ImageData<'static>) -> Result<bool, String> {
        validate_history_image_rgba(&image)?;
        let digest = history_image_digest(&image);

        // Avoid PNG work on every background poll after an unchanged image.
        {
            let mut state = self.lock_state();
            if !state.enabled || !state.image_history_enabled {
                return Ok(false);
            }
            if let Some(index) = state.items.iter().position(|item| {
                item.kind == ClipboardHistoryItemKind::Image
                    && item
                        .image
                        .as_ref()
                        .is_some_and(|stored| stored.digest == digest)
            }) {
                let original = state.clone();
                let mut item = state.items.remove(index);
                item.captured_at = Utc::now().to_rfc3339();
                state.items.insert(0, item);
                if let Err(error) = self.persist(&state) {
                    *state = original;
                    return Err(error);
                }
                return Ok(true);
            }
        }

        let png = encode_history_png(&image)?;
        let id = new_history_id();
        let file_name = format!("{id}.png");
        let image_path = self.image_path(&file_name)?;
        atomic_write(&image_path, &png)?;

        let mut state = self.lock_state();
        if !state.enabled || !state.image_history_enabled {
            let _ = fs::remove_file(&image_path);
            return Ok(false);
        }
        // A concurrent capture may have stored the same image while PNG was
        // encoding. Keep the existing image and remove this unreferenced file.
        if let Some(index) = state.items.iter().position(|item| {
            item.kind == ClipboardHistoryItemKind::Image
                && item
                    .image
                    .as_ref()
                    .is_some_and(|stored| stored.digest == digest)
        }) {
            let original = state.clone();
            let mut item = state.items.remove(index);
            item.captured_at = Utc::now().to_rfc3339();
            state.items.insert(0, item);
            if let Err(error) = self.persist(&state) {
                *state = original;
                let _ = fs::remove_file(&image_path);
                return Err(error);
            }
            drop(state);
            let _ = fs::remove_file(&image_path);
            return Ok(true);
        }

        let original = state.clone();
        state.items.insert(
            0,
            StoredClipboardHistoryItem {
                id,
                kind: ClipboardHistoryItemKind::Image,
                text: String::new(),
                captured_at: Utc::now().to_rfc3339(),
                pinned: false,
                image: Some(StoredClipboardHistoryImage {
                    file_name,
                    width: u32::try_from(image.width)
                        .map_err(|_| "Clipboard image width is unsupported.".to_owned())?,
                    height: u32::try_from(image.height)
                        .map_err(|_| "Clipboard image height is unsupported.".to_owned())?,
                    byte_length: u64::try_from(png.len()).unwrap_or(u64::MAX),
                    png_digest: history_png_digest(&png),
                    digest,
                }),
                files: Vec::new(),
            },
        );
        let removed_images = trim_to_limits(&mut state.items);
        if let Err(error) = self.persist(&state) {
            *state = original;
            let _ = fs::remove_file(&image_path);
            return Err(error);
        }
        drop(state);
        self.cleanup_unreferenced_images(&removed_images);
        Ok(true)
    }

    /// Persists already-bounded canonical file/folder references. This never
    /// reads file contents; the native-only path and fingerprint are checked
    /// again before any copy/open action can happen.
    fn capture_stored_files(&self, files: Vec<StoredClipboardHistoryFile>) -> Result<bool, String> {
        if files.is_empty() {
            return Ok(false);
        }

        let mut state = self.lock_state();
        if !state.enabled || !state.file_history_enabled {
            return Ok(false);
        }
        let original = state.clone();
        let pinned = state
            .items
            .iter()
            .position(|item| item.kind == ClipboardHistoryItemKind::Files && item.files == files)
            .map(|index| state.items.remove(index).pinned)
            .unwrap_or(false);
        state.items.insert(
            0,
            StoredClipboardHistoryItem {
                id: new_history_id(),
                kind: ClipboardHistoryItemKind::Files,
                text: String::new(),
                captured_at: Utc::now().to_rfc3339(),
                pinned,
                image: None,
                files,
            },
        );
        let removed_images = trim_to_limits(&mut state.items);
        if let Err(error) = self.persist(&state) {
            *state = original;
            return Err(error);
        }
        drop(state);
        self.cleanup_unreferenced_images(&removed_images);
        Ok(true)
    }

    /// Kept for v1 callers: only text records may use the legacy copy command.
    pub fn copy_to_system_clipboard(&self, id: &str) -> Result<String, String> {
        let text = {
            let state = self.lock_state();
            state
                .items
                .iter()
                .find(|item| item.id == id && item.kind == ClipboardHistoryItemKind::Text)
                .map(|item| item.text.clone())
                .ok_or_else(|| "The clipboard history text item no longer exists.".to_owned())?
        };

        crate::clipboard_access::with_clipboard(|clipboard| clipboard.set_text(text.clone()))
            .map_err(|error| format!("Could not write to the system clipboard: {error}"))?;
        let _ = self.capture_text(text.clone());
        self.record_explicit_text_clipboard(&text);
        Ok(text)
    }

    /// Restores a previously captured image or file list only after an
    /// explicit UI action. File references are revalidated immediately before
    /// they are handed to the system clipboard.
    pub fn restore_to_system_clipboard(
        &self,
        id: &str,
    ) -> Result<ClipboardHistoryRestoreResult, String> {
        let item = self.stored_item(id)?;
        match item.kind {
            ClipboardHistoryItemKind::Text => {
                self.copy_to_system_clipboard(id)?;
                Ok(ClipboardHistoryRestoreResult {
                    kind: ClipboardHistoryItemKind::Text,
                    restored_count: 1,
                })
            }
            ClipboardHistoryItemKind::Image => {
                let image = item.image.as_ref().ok_or_else(|| {
                    "The clipboard history image metadata is unavailable.".to_owned()
                })?;
                let image = self.load_history_image(image)?;
                crate::clipboard_access::with_clipboard(|clipboard| {
                    clipboard.set_image(image.clone())
                })
                .map_err(|error| {
                    format!("Could not restore the image to the system clipboard: {error}")
                })?;
                self.record_explicit_image_clipboard(&image);
                Ok(ClipboardHistoryRestoreResult {
                    kind: ClipboardHistoryItemKind::Image,
                    restored_count: 1,
                })
            }
            ClipboardHistoryItemKind::Files => {
                let paths = revalidate_stored_files(&item.files)?;
                crate::clipboard_access::with_clipboard(|clipboard| {
                    clipboard.set().file_list(&paths)
                })
                .map_err(|error| {
                    format!("Could not restore the file list to the system clipboard: {error}")
                })?;
                self.record_explicit_files_clipboard(&item.files);
                Ok(ClipboardHistoryRestoreResult {
                    kind: ClipboardHistoryItemKind::Files,
                    restored_count: paths.len(),
                })
            }
        }
    }

    /// Reads a persisted PNG only after the user asked to preview it. The PNG
    /// is read from a validated host path, size-bounded again, and emitted as a
    /// local data URL for the current renderer session only.
    pub fn image_preview(&self, id: &str) -> Result<ClipboardImage, String> {
        let item = self.stored_item(id)?;
        if item.kind != ClipboardHistoryItemKind::Image {
            return Err("This clipboard history item is not an image.".to_owned());
        }
        let image = item
            .image
            .as_ref()
            .ok_or_else(|| "The clipboard history image metadata is unavailable.".to_owned())?;
        let (png, _) = self.load_verified_history_png(image)?;
        Ok(ClipboardImage {
            data_url: format!("data:image/png;base64,{}", BASE64_STANDARD.encode(png)),
            name: "ihub-clipboard-history.png".to_owned(),
            mime_type: "image/png".to_owned(),
            width: image.width,
            height: image.height,
        })
    }

    /// Resolves exactly one native-private file entry. The returned path is
    /// never exposed to the renderer; callers use it immediately for an
    /// explicit system open/reveal action.
    #[cfg(test)]
    pub fn revalidated_file_entry_path(
        &self,
        id: &str,
        file_index: usize,
    ) -> Result<PathBuf, String> {
        self.prepare_file_entry_open(id, file_index)
            .map(|prepared| prepared.path().to_path_buf())
    }

    /// Prepares, fingerprints, and returns one clipboard entry while the same
    /// native handle still guards the object that a caller will launch.
    pub fn prepare_file_entry_open(
        &self,
        id: &str,
        file_index: usize,
    ) -> Result<PreparedLocalOpen, String> {
        let item = self.stored_item(id)?;
        if item.kind != ClipboardHistoryItemKind::Files {
            return Err("This clipboard history item is not a file list.".to_owned());
        }
        let file = item.files.get(file_index).ok_or_else(|| {
            "The selected clipboard history file entry no longer exists.".to_owned()
        })?;
        let kind = match file.kind.as_str() {
            "file" => LocalOpenKind::File,
            "folder" => LocalOpenKind::Folder,
            _ => return Err("The stored clipboard file entry type is invalid.".to_owned()),
        };
        let prepared = crate::system_open::prepare_local_open(Path::new(&file.path), Some(kind))?;
        let current = stored_file_from_path(prepared.path()).ok_or_else(|| {
            format!(
                "The original clipboard item “{}” can no longer be verified.",
                file.name
            )
        })?;
        validate_stored_file_snapshot(file, &current)?;
        Ok(prepared)
    }

    pub fn set_pinned(&self, id: &str, pinned: bool) -> Result<ClipboardHistorySnapshot, String> {
        let mut state = self.lock_state();
        let original = state.clone();
        let item = state
            .items
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or_else(|| "The clipboard history item no longer exists.".to_owned())?;
        item.pinned = pinned;
        if let Err(error) = self.persist(&state) {
            *state = original;
            return Err(error);
        }
        drop(state);
        Ok(self.snapshot(Some(MAX_HISTORY_ITEMS)))
    }

    pub fn delete(&self, id: &str) -> Result<ClipboardHistorySnapshot, String> {
        let mut state = self.lock_state();
        let original = state.clone();
        let index = state
            .items
            .iter()
            .position(|item| item.id == id)
            .ok_or_else(|| "The clipboard history item no longer exists.".to_owned())?;
        let removed = state.items.remove(index);
        let removed_images = image_file_names(std::slice::from_ref(&removed));
        if let Err(error) = self.persist(&state) {
            *state = original;
            return Err(error);
        }
        drop(state);
        self.cleanup_unreferenced_images(&removed_images);
        Ok(self.snapshot(Some(MAX_HISTORY_ITEMS)))
    }

    pub fn clear_unpinned(&self) -> Result<ClipboardHistorySnapshot, String> {
        let mut state = self.lock_state();
        let original = state.clone();
        let removed: Vec<_> = state
            .items
            .iter()
            .filter(|item| !item.pinned)
            .cloned()
            .collect();
        state.items.retain(|item| item.pinned);
        let removed_images = image_file_names(&removed);
        if let Err(error) = self.persist(&state) {
            *state = original;
            return Err(error);
        }
        drop(state);
        self.cleanup_unreferenced_images(&removed_images);
        Ok(self.snapshot(Some(MAX_HISTORY_ITEMS)))
    }

    fn stored_item(&self, id: &str) -> Result<StoredClipboardHistoryItem, String> {
        self.lock_state()
            .items
            .iter()
            .find(|item| item.id == id)
            .cloned()
            .ok_or_else(|| "The clipboard history item no longer exists.".to_owned())
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, PersistedClipboardHistory> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_poll_state(&self) -> std::sync::MutexGuard<'_, ClipboardPollState> {
        self.poll_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn reset_poll_state(&self) {
        *self.lock_poll_state() = ClipboardPollState::default();
    }

    /// Successful explicit writes already created their intentional history
    /// transition. Remember their exact format state so the background poll
    /// does not immediately create a second entry for the same system write.
    fn record_explicit_text_clipboard(&self, text: &str) {
        let mut poll_state = self.lock_poll_state();
        poll_state.text = Some(clipboard_text_fingerprint(text));
        poll_state.image = Some(ClipboardPollImageFingerprint::Absent);
        poll_state.files = Some(ClipboardPollFilesFingerprint::Absent);
    }

    fn record_explicit_image_clipboard(&self, image: &arboard::ImageData<'static>) {
        let mut poll_state = self.lock_poll_state();
        poll_state.text = Some(ClipboardPollTextFingerprint::Absent);
        poll_state.image = Some(clipboard_image_fingerprint(image));
        poll_state.files = Some(ClipboardPollFilesFingerprint::Absent);
    }

    fn record_explicit_files_clipboard(&self, files: &[StoredClipboardHistoryFile]) {
        let mut poll_state = self.lock_poll_state();
        poll_state.text = Some(ClipboardPollTextFingerprint::Absent);
        poll_state.image = Some(ClipboardPollImageFingerprint::Absent);
        poll_state.files = Some(if files.is_empty() {
            ClipboardPollFilesFingerprint::Absent
        } else {
            ClipboardPollFilesFingerprint::Capturable {
                count: files.len(),
                digest: stored_files_digest(files),
            }
        });
    }

    fn image_path(&self, file_name: &str) -> Result<PathBuf, String> {
        safe_image_file_name(file_name)?;
        Ok(self.image_directory.join(file_name))
    }

    fn load_history_image(
        &self,
        metadata: &StoredClipboardHistoryImage,
    ) -> Result<arboard::ImageData<'static>, String> {
        self.load_verified_history_png(metadata)
            .map(|(_, image)| image)
    }

    fn load_verified_history_png(
        &self,
        metadata: &StoredClipboardHistoryImage,
    ) -> Result<(Vec<u8>, arboard::ImageData<'static>), String> {
        let path = self.image_path(&metadata.file_name)?;
        let png = read_bounded_regular_file(&path, MAX_HISTORY_IMAGE_PNG_BYTES)?;
        if u64::try_from(png.len()).ok() != Some(metadata.byte_length) {
            return Err(
                "The stored clipboard image no longer matches its byte-length metadata.".to_owned(),
            );
        }
        if !metadata.png_digest.is_empty() && history_png_digest(&png) != metadata.png_digest {
            return Err("The stored clipboard image failed its integrity digest check.".to_owned());
        }
        let decoded = decode_history_png(&png)?;
        if u32::try_from(decoded.width).ok() != Some(metadata.width)
            || u32::try_from(decoded.height).ok() != Some(metadata.height)
        {
            return Err("The stored clipboard image no longer matches its metadata.".to_owned());
        }
        // v2 records written before `pngDigest` existed remain readable only
        // if their older pixel checksum also verifies. New records verify both
        // the PNG bytes above and the decoded image below.
        if history_image_digest(&decoded) != metadata.digest {
            return Err("The stored clipboard image failed its pixel integrity check.".to_owned());
        }
        Ok((png, decoded))
    }

    fn persist(&self, state: &PersistedClipboardHistory) -> Result<(), String> {
        let parent = self.data_path.parent().ok_or_else(|| {
            "Could not determine the clipboard history data directory.".to_owned()
        })?;
        fs::create_dir_all(parent).map_err(|error| {
            format!("Could not create the clipboard history data directory: {error}")
        })?;
        let serialized = serde_json::to_vec_pretty(state)
            .map_err(|error| format!("Could not encode clipboard history: {error}"))?;
        atomic_write(self.data_path.as_ref(), &serialized)
    }

    fn cleanup_unreferenced_images(&self, candidates: &[String]) {
        if candidates.is_empty() {
            return;
        }
        let referenced: HashSet<_> = self
            .lock_state()
            .items
            .iter()
            .filter_map(|item| item.image.as_ref().map(|image| image.file_name.clone()))
            .collect();
        for file_name in candidates {
            if referenced.contains(file_name) || safe_image_file_name(file_name).is_err() {
                continue;
            }
            let path = self.image_directory.join(file_name);
            if fs::remove_file(path).is_err() {
                // A missing or temporarily locked old PNG is harmless. It can
                // never be reached through the history state again.
            }
        }
    }
}

fn clipboard_poll_value<T>(result: Result<T, arboard::Error>) -> ClipboardPollValue<T> {
    match result {
        Ok(value) => ClipboardPollValue::Present(value),
        Err(arboard::Error::ContentNotAvailable) => ClipboardPollValue::Absent,
        Err(_) => ClipboardPollValue::Unavailable,
    }
}

fn clipboard_text_fingerprint(text: &str) -> ClipboardPollTextFingerprint {
    if text.trim().is_empty() || text.len() > MAX_CAPTURED_TEXT_BYTES {
        ClipboardPollTextFingerprint::Rejected {
            byte_length: text.len(),
        }
    } else {
        ClipboardPollTextFingerprint::Capturable {
            byte_length: text.len(),
            digest: sha256_digest(text.as_bytes()),
        }
    }
}

fn clipboard_image_fingerprint(
    image: &arboard::ImageData<'static>,
) -> ClipboardPollImageFingerprint {
    if validate_history_image_rgba(image).is_err() {
        return ClipboardPollImageFingerprint::Rejected {
            width: image.width,
            height: image.height,
            byte_length: image.bytes.len(),
        };
    }
    ClipboardPollImageFingerprint::Capturable {
        width: image.width,
        height: image.height,
        digest: history_image_digest(image),
    }
}

/// Hashes the file-list *clipboard payload*, not mutable file contents. A
/// file changing on disk must not make a static CF_HDROP list look like it was
/// copied again every poll; capture-time metadata still protects an explicit
/// later restore.
fn stored_files_digest(files: &[StoredClipboardHistoryFile]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(u64::try_from(files.len()).unwrap_or(u64::MAX).to_le_bytes());
    for file in files {
        hash_component(&mut hasher, file.path.as_bytes());
        hash_component(&mut hasher, file.name.as_bytes());
        hash_component(&mut hasher, file.kind.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn hash_component(hasher: &mut Sha256, component: &[u8]) {
    hasher.update(
        u64::try_from(component.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    hasher.update(component);
}

fn sha256_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn load_history_state(data_path: &Path, legacy_path: &Path) -> PersistedClipboardHistory {
    if let Some(state) = fs::read_to_string(data_path)
        .ok()
        .and_then(|text| serde_json::from_str::<PersistedClipboardHistory>(&text).ok())
    {
        return state;
    }

    fs::read_to_string(legacy_path)
        .ok()
        .and_then(|text| serde_json::from_str::<LegacyPersistedClipboardHistory>(&text).ok())
        .map(|legacy| PersistedClipboardHistory {
            enabled: legacy.enabled,
            image_history_enabled: false,
            file_history_enabled: false,
            items: legacy
                .items
                .into_iter()
                .filter(|item| {
                    !item.text.trim().is_empty() && item.text.len() <= MAX_CAPTURED_TEXT_BYTES
                })
                .map(|item| StoredClipboardHistoryItem {
                    id: item.id,
                    kind: ClipboardHistoryItemKind::Text,
                    text: item.text,
                    captured_at: item.captured_at,
                    pinned: item.pinned,
                    image: None,
                    files: Vec::new(),
                })
                .collect(),
        })
        .unwrap_or_default()
}

fn snapshot_from_state(
    state: &PersistedClipboardHistory,
    limit: Option<usize>,
    text_only: bool,
) -> ClipboardHistorySnapshot {
    let mut items: Vec<_> = state
        .items
        .iter()
        .filter(|item| !text_only || item.kind == ClipboardHistoryItemKind::Text)
        .cloned()
        .collect();
    items.sort_by_key(|item| std::cmp::Reverse(item.pinned));
    let limit = limit.unwrap_or(60).clamp(1, MAX_HISTORY_ITEMS);
    items.truncate(limit);
    ClipboardHistorySnapshot {
        enabled: state.enabled,
        image_history_enabled: if text_only {
            false
        } else {
            state.image_history_enabled
        },
        file_history_enabled: if text_only {
            false
        } else {
            state.file_history_enabled
        },
        items: items.into_iter().map(public_item_from_stored).collect(),
    }
}

fn public_item_from_stored(item: StoredClipboardHistoryItem) -> ClipboardHistoryItem {
    ClipboardHistoryItem {
        id: item.id,
        kind: item.kind,
        text: item.text,
        captured_at: item.captured_at,
        pinned: item.pinned,
        image: item.image.map(|image| ClipboardHistoryImageMetadata {
            width: image.width,
            height: image.height,
            byte_length: image.byte_length,
        }),
        files: item
            .files
            .into_iter()
            .map(|file| ClipboardHistoryFileMetadata {
                name: file.name,
                kind: file.kind,
            })
            .collect(),
    }
}

fn new_history_id() -> String {
    format!("clip-{}", Uuid::new_v4())
}

fn trim_to_limits(items: &mut Vec<StoredClipboardHistoryItem>) -> Vec<String> {
    let mut removed = Vec::new();
    trim_kind_to_limit(
        items,
        ClipboardHistoryItemKind::Image,
        MAX_IMAGE_HISTORY_ITEMS,
        &mut removed,
    );
    trim_kind_to_limit(
        items,
        ClipboardHistoryItemKind::Files,
        MAX_FILE_HISTORY_ITEMS,
        &mut removed,
    );
    while items.len() > MAX_HISTORY_ITEMS {
        let index = items
            .iter()
            .rposition(|item| !item.pinned)
            .unwrap_or_else(|| items.len().saturating_sub(1));
        let item = items.remove(index);
        removed.extend(image_file_names(std::slice::from_ref(&item)));
    }
    removed
}

fn trim_kind_to_limit(
    items: &mut Vec<StoredClipboardHistoryItem>,
    kind: ClipboardHistoryItemKind,
    limit: usize,
    removed: &mut Vec<String>,
) {
    while items.iter().filter(|item| item.kind == kind).count() > limit {
        let index = items
            .iter()
            .rposition(|item| item.kind == kind && !item.pinned)
            .or_else(|| items.iter().rposition(|item| item.kind == kind));
        let Some(index) = index else {
            break;
        };
        let item = items.remove(index);
        removed.extend(image_file_names(std::slice::from_ref(&item)));
    }
}

fn image_file_names(items: &[StoredClipboardHistoryItem]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| item.image.as_ref().map(|image| image.file_name.clone()))
        .collect()
}

fn validate_history_image_rgba(image: &arboard::ImageData<'static>) -> Result<(), String> {
    if image.width == 0 || image.height == 0 {
        return Err("The clipboard image has no pixels.".to_owned());
    }
    if image.width > MAX_HISTORY_IMAGE_EDGE || image.height > MAX_HISTORY_IMAGE_EDGE {
        return Err(format!(
            "The clipboard image is larger than the {MAX_HISTORY_IMAGE_EDGE}px history edge limit."
        ));
    }
    let pixels = image
        .width
        .checked_mul(image.height)
        .ok_or_else(|| "The clipboard image dimensions overflow the supported range.".to_owned())?;
    if pixels > MAX_HISTORY_IMAGE_PIXELS {
        return Err("The clipboard image exceeds the history pixel limit.".to_owned());
    }
    let expected = pixels
        .checked_mul(4)
        .ok_or_else(|| "The clipboard image byte size overflows the supported range.".to_owned())?;
    if expected > MAX_HISTORY_IMAGE_RAW_BYTES {
        return Err("The clipboard image uses too much raw memory for history.".to_owned());
    }
    if image.bytes.len() != expected {
        return Err("The clipboard image has an invalid RGBA pixel buffer.".to_owned());
    }
    Ok(())
}

fn history_image_digest(image: &arboard::ImageData<'static>) -> String {
    let mut hasher = Sha256::new();
    hasher.update((image.width as u64).to_le_bytes());
    hasher.update((image.height as u64).to_le_bytes());
    hasher.update(image.bytes.as_ref());
    format!("{:x}", hasher.finalize())
}

fn history_png_digest(png: &[u8]) -> String {
    sha256_digest(png)
}

fn encode_history_png(image: &arboard::ImageData<'static>) -> Result<Vec<u8>, String> {
    validate_history_image_rgba(image)?;
    let width = u32::try_from(image.width)
        .map_err(|_| "Clipboard image width is unsupported.".to_owned())?;
    let height = u32::try_from(image.height)
        .map_err(|_| "Clipboard image height is unsupported.".to_owned())?;
    let mut png = LimitedHistoryPngBuffer::new(MAX_HISTORY_IMAGE_PNG_BYTES);
    match PngEncoder::new(&mut png).write_image(
        image.bytes.as_ref(),
        width,
        height,
        ColorType::Rgba8.into(),
    ) {
        Ok(()) => Ok(png.bytes),
        Err(_error) if png.limit_exceeded => {
            Err("The clipboard image remains larger than the 4 MiB history PNG limit.".to_owned())
        }
        Err(error) => Err(format!(
            "The clipboard image could not be encoded as PNG: {error}"
        )),
    }
}

struct LimitedHistoryPngBuffer {
    bytes: Vec<u8>,
    limit: usize,
    limit_exceeded: bool,
}

impl LimitedHistoryPngBuffer {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            limit_exceeded: false,
        }
    }
}

impl Write for LimitedHistoryPngBuffer {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if buffer.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.limit_exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "encoded PNG exceeds the clipboard history limit",
            ));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn decode_history_png(png: &[u8]) -> Result<arboard::ImageData<'static>, String> {
    if png.len() > MAX_HISTORY_IMAGE_PNG_BYTES {
        return Err("The stored clipboard image exceeds the configured size limit.".to_owned());
    }
    // PNG dimensions and decoder allocation are constrained *before* the
    // compressed stream is expanded. Stored files are local, but may still be
    // interrupted/corrupted, so never trust the previous metadata alone.
    let mut reader =
        image::ImageReader::with_format(BufReader::new(Cursor::new(png)), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_HISTORY_IMAGE_EDGE as u32);
    limits.max_image_height = Some(MAX_HISTORY_IMAGE_EDGE as u32);
    limits.max_alloc = Some(MAX_HISTORY_IMAGE_RAW_BYTES as u64);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|error| format!("The stored clipboard image is not a valid bounded PNG: {error}"))?
        .to_rgba8();
    let image = arboard::ImageData {
        width: usize::try_from(decoded.width())
            .map_err(|_| "The stored clipboard image width is unsupported.".to_owned())?,
        height: usize::try_from(decoded.height())
            .map_err(|_| "The stored clipboard image height is unsupported.".to_owned())?,
        bytes: Cow::Owned(decoded.into_raw()),
    };
    validate_history_image_rgba(&image)?;
    Ok(image)
}

fn stored_files_from_paths(paths: Vec<PathBuf>) -> Vec<StoredClipboardHistoryFile> {
    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter_map(|path| stored_file_from_path(&path))
        .filter(|file| seen.insert(file.path.clone()))
        .take(MAX_FILES_PER_HISTORY_ITEM)
        .collect()
}

fn stored_file_from_path(path: &Path) -> Option<StoredClipboardHistoryFile> {
    let path = path.canonicalize().ok()?;
    let metadata = fs::metadata(&path).ok()?;
    let kind = if metadata.is_dir() {
        "folder"
    } else if metadata.is_file() {
        "file"
    } else {
        return None;
    };
    let name = path.file_name()?.to_string_lossy().into_owned();
    let path = path.to_string_lossy().into_owned();
    if name.trim().is_empty()
        || name.chars().count() > MAX_HISTORY_FILE_NAME_CHARS
        || path.chars().count() > MAX_HISTORY_FILE_PATH_CHARS
    {
        return None;
    }

    let (byte_length, modified_unix_ms) = if kind == "file" {
        let modified_unix_ms = metadata
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_millis()
            .try_into()
            .ok()?;
        (Some(metadata.len()), Some(modified_unix_ms))
    } else {
        (None, None)
    };
    Some(StoredClipboardHistoryFile {
        path,
        name,
        kind: kind.to_owned(),
        byte_length,
        modified_unix_ms,
    })
}

fn revalidate_stored_files(files: &[StoredClipboardHistoryFile]) -> Result<Vec<PathBuf>, String> {
    if files.is_empty() || files.len() > MAX_FILES_PER_HISTORY_ITEM {
        return Err("The stored clipboard file list is invalid.".to_owned());
    }
    files.iter().map(revalidate_stored_file).collect()
}

fn revalidate_stored_file(file: &StoredClipboardHistoryFile) -> Result<PathBuf, String> {
    let canonical = PathBuf::from(&file.path).canonicalize().map_err(|_| {
        format!(
            "The original clipboard item “{}” is no longer available.",
            file.name
        )
    })?;
    let current = stored_file_from_path(&canonical).ok_or_else(|| {
        format!(
            "The original clipboard item “{}” can no longer be verified.",
            file.name
        )
    })?;
    validate_stored_file_snapshot(file, &current)?;
    Ok(canonical)
}

fn validate_stored_file_snapshot(
    file: &StoredClipboardHistoryFile,
    current: &StoredClipboardHistoryFile,
) -> Result<(), String> {
    if current.path != file.path || current.name != file.name || current.kind != file.kind {
        return Err(format!(
            "The original clipboard item “{}” changed location or type.",
            file.name
        ));
    }
    if file.kind == "file"
        && (current.byte_length != file.byte_length
            || current.modified_unix_ms != file.modified_unix_ms)
    {
        return Err(format!(
            "The original clipboard file “{}” changed after capture.",
            file.name
        ));
    }
    Ok(())
}

fn safe_image_file_name(file_name: &str) -> Result<(), String> {
    let path = Path::new(file_name);
    if path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
        || path
            .extension()
            .map_or(true, |extension| extension != "png")
        || !file_name.starts_with("clip-")
    {
        return Err("The stored clipboard image reference is invalid.".to_owned());
    }
    Ok(())
}

/// Opens only a regular local image file, checks its size before allocating,
/// and uses a capped stream read as a second line of defense against a race or
/// a modified file. This is intentionally not `fs::read`: persisted history
/// is local, but corrupted state must not turn an explicit preview into an
/// unbounded allocation.
fn read_bounded_regular_file(path: &Path, maximum_bytes: usize) -> Result<Vec<u8>, String> {
    let link_metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("Could not inspect the stored clipboard image: {error}"))?;
    if !link_metadata.file_type().is_file() {
        return Err("The stored clipboard image is not a regular file.".to_owned());
    }
    if link_metadata.len() > u64::try_from(maximum_bytes).unwrap_or(u64::MAX) {
        return Err("The stored clipboard image exceeds the configured size limit.".to_owned());
    }

    let mut file = File::open(path)
        .map_err(|error| format!("Could not read the stored clipboard image: {error}"))?;
    let opened_metadata = file
        .metadata()
        .map_err(|error| format!("Could not inspect the stored clipboard image: {error}"))?;
    if !opened_metadata.file_type().is_file()
        || opened_metadata.len() > u64::try_from(maximum_bytes).unwrap_or(u64::MAX)
    {
        return Err("The stored clipboard image is not a bounded regular file.".to_owned());
    }
    let expected_length = usize::try_from(opened_metadata.len())
        .map_err(|_| "The stored clipboard image length is unsupported.".to_owned())?;
    let mut bytes = Vec::with_capacity(expected_length);
    let read_limit = u64::try_from(maximum_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    Read::by_ref(&mut file)
        .take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("Could not read the stored clipboard image: {error}"))?;
    if bytes.len() > maximum_bytes {
        return Err("The stored clipboard image exceeds the configured size limit.".to_owned());
    }
    if bytes.len() != expected_length {
        return Err("The stored clipboard image changed while it was being read.".to_owned());
    }
    Ok(bytes)
}

/// Writes the replacement file next to the old one, flushes it, then renames
/// it in place. If rename fails, the previous JSON/PNG remains intact and the
/// caller rolls its in-memory mutation back.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Could not determine the clipboard history data directory.".to_owned())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("Could not create clipboard history storage: {error}"))?;
    let temp_path = parent.join(format!(
        ".{}-{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("clipboard"),
        Uuid::new_v4()
    ));
    let result = (|| {
        let mut file: File = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)
            .map_err(|error| {
                format!("Could not create temporary clipboard history data: {error}")
            })?;
        file.write_all(bytes).map_err(|error| {
            format!("Could not write temporary clipboard history data: {error}")
        })?;
        file.sync_all()
            .map_err(|error| format!("Could not flush clipboard history data: {error}"))?;
        drop(file);
        fs::rename(&temp_path, path).map_err(|error| {
            format!("Could not atomically replace clipboard history data: {error}")
        })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::{
        borrow::Cow,
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use crate::system_open::LocalOpenKind;

    use super::{
        stored_files_from_paths, ClipboardHistory, ClipboardHistoryItemKind, ClipboardPollSample,
        ClipboardPollValue, HISTORY_FILE_NAME, IMAGE_DIRECTORY_NAME, MAX_HISTORY_IMAGE_EDGE,
    };

    fn temporary_directory(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "ihub-{label}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    fn one_pixel(red: u8, green: u8, blue: u8) -> arboard::ImageData<'static> {
        arboard::ImageData {
            width: 1,
            height: 1,
            bytes: Cow::Owned(vec![red, green, blue, 0xff]),
        }
    }

    fn polled_text(text: &str) -> ClipboardPollSample {
        ClipboardPollSample {
            text: ClipboardPollValue::Present(text.to_owned()),
            image: ClipboardPollValue::Unavailable,
            files: ClipboardPollValue::Unavailable,
        }
    }

    fn polled_image(image: arboard::ImageData<'static>) -> ClipboardPollSample {
        ClipboardPollSample {
            text: ClipboardPollValue::Unavailable,
            image: ClipboardPollValue::Present(image),
            files: ClipboardPollValue::Unavailable,
        }
    }

    fn polled_files(paths: Vec<std::path::PathBuf>) -> ClipboardPollSample {
        ClipboardPollSample {
            text: ClipboardPollValue::Unavailable,
            image: ClipboardPollValue::Unavailable,
            files: ClipboardPollValue::Present(paths),
        }
    }

    #[test]
    fn text_history_is_opt_in_and_preserves_pinned_duplicates() {
        let directory = temporary_directory("clipboard-history-text");
        let history = ClipboardHistory::new(directory.clone());
        assert!(!history
            .capture_text("first".to_owned())
            .expect("capture disabled"));

        history.set_enabled(true).expect("enable");
        assert!(history
            .capture_text("first".to_owned())
            .expect("capture first"));
        let first = history.snapshot(Some(10)).items.remove(0);
        history.set_pinned(&first.id, true).expect("pin");
        history
            .capture_text("second".to_owned())
            .expect("capture second");
        history
            .capture_text("first".to_owned())
            .expect("capture duplicate");

        let snapshot = history.snapshot(Some(10));
        assert_eq!(snapshot.items.len(), 2);
        assert_eq!(snapshot.items[0].kind, ClipboardHistoryItemKind::Text);
        assert_eq!(snapshot.items[0].text, "first");
        assert!(snapshot.items[0].pinned);
        fs::remove_dir_all(directory).expect("cleanup test directory");
    }

    #[test]
    fn polling_deduplicates_static_text_and_readds_after_a_real_transition() {
        let directory = temporary_directory("clipboard-history-poll-text");
        let history = ClipboardHistory::new(directory.clone());
        history.set_enabled(true).expect("enable");

        history.capture_polled_sample(polled_text("first"), false, false);
        let first = history
            .snapshot(Some(10))
            .items
            .into_iter()
            .find(|item| item.text == "first")
            .expect("first item");
        let persisted_before_duplicate =
            fs::read_to_string(directory.join(HISTORY_FILE_NAME)).expect("state before duplicate");

        history.capture_polled_sample(polled_text("first"), false, false);
        let duplicate = history
            .snapshot(Some(10))
            .items
            .into_iter()
            .find(|item| item.text == "first")
            .expect("deduplicated first item");
        assert_eq!(duplicate.id, first.id);
        assert_eq!(duplicate.captured_at, first.captured_at);
        assert_eq!(
            fs::read_to_string(directory.join(HISTORY_FILE_NAME)).expect("state after duplicate"),
            persisted_before_duplicate,
            "an unchanged poll must not rewrite the history record"
        );

        history.set_pinned(&first.id, true).expect("pin first");
        history.capture_polled_sample(polled_text("second"), false, false);
        history.capture_polled_sample(polled_text("first"), false, false);
        let readded = history
            .snapshot(Some(10))
            .items
            .into_iter()
            .find(|item| item.text == "first")
            .expect("first after transition");
        assert_ne!(readded.id, first.id);
        assert!(readded.pinned, "a real transition must preserve pinning");
        assert_eq!(
            readded.files.len(),
            0,
            "empty file lists stay explicit in IPC"
        );
        assert!(
            serde_json::to_string(&readded)
                .expect("serialize IPC item")
                .contains("\"files\":[]"),
            "the required frontend files field must never be omitted"
        );
        fs::remove_dir_all(directory).expect("cleanup test directory");
    }

    #[test]
    fn polling_deduplicates_image_and_file_formats_independently() {
        let directory = temporary_directory("clipboard-history-poll-formats");
        let file_one = directory.join("first.txt");
        let file_two = directory.join("second.txt");
        fs::create_dir_all(&directory).expect("directory");
        fs::write(&file_one, "first").expect("first file");
        fs::write(&file_two, "second").expect("second file");

        let history = ClipboardHistory::new(directory.clone());
        history.set_enabled(true).expect("enable");
        history
            .set_capture_options(true, true)
            .expect("enable image/files");

        history.capture_polled_sample(polled_image(one_pixel(1, 2, 3)), true, false);
        let image = history
            .snapshot(Some(10))
            .items
            .into_iter()
            .find(|item| item.kind == ClipboardHistoryItemKind::Image)
            .expect("image item");
        let image_state = fs::read_to_string(directory.join(HISTORY_FILE_NAME))
            .expect("image state before duplicate");
        history.capture_polled_sample(polled_image(one_pixel(1, 2, 3)), true, false);
        let duplicate_image = history
            .snapshot(Some(10))
            .items
            .into_iter()
            .find(|item| item.kind == ClipboardHistoryItemKind::Image)
            .expect("duplicate image item");
        assert_eq!(duplicate_image.id, image.id);
        assert_eq!(
            fs::read_to_string(directory.join(HISTORY_FILE_NAME))
                .expect("image state after duplicate"),
            image_state
        );
        history.capture_polled_sample(polled_image(one_pixel(4, 5, 6)), true, false);
        history.capture_polled_sample(polled_image(one_pixel(1, 2, 3)), true, false);
        let images: Vec<_> = history
            .snapshot(Some(10))
            .items
            .into_iter()
            .filter(|item| item.kind == ClipboardHistoryItemKind::Image)
            .collect();
        assert_eq!(images.len(), 2, "a changed image must be sampled");
        assert_eq!(
            images[0].id, image.id,
            "the original image returns after a transition"
        );

        history.capture_polled_sample(polled_files(vec![file_one.clone()]), false, true);
        let files = history
            .snapshot(Some(10))
            .items
            .into_iter()
            .find(|item| item.kind == ClipboardHistoryItemKind::Files)
            .expect("file item");
        history.capture_polled_sample(polled_files(vec![file_one.clone()]), false, true);
        let duplicate_files = history
            .snapshot(Some(10))
            .items
            .into_iter()
            .find(|item| item.kind == ClipboardHistoryItemKind::Files)
            .expect("duplicate file item");
        assert_eq!(duplicate_files.id, files.id);

        fs::write(&file_one, "first file changed after the copy")
            .expect("change original file without changing CF_HDROP");
        history.capture_polled_sample(polled_files(vec![file_one.clone()]), false, true);
        let unchanged_payload_files = history
            .snapshot(Some(10))
            .items
            .into_iter()
            .find(|item| item.kind == ClipboardHistoryItemKind::Files)
            .expect("static file-list item");
        assert_eq!(
            unchanged_payload_files.id, files.id,
            "a mutable target file must not turn an unchanged file-list clipboard payload into a new poll"
        );

        history.capture_polled_sample(polled_files(vec![file_two]), false, true);
        history.capture_polled_sample(polled_files(vec![file_one]), false, true);
        let transitioned_files: Vec<_> = history
            .snapshot(Some(10))
            .items
            .into_iter()
            .filter(|item| item.kind == ClipboardHistoryItemKind::Files)
            .collect();
        assert!(
            transitioned_files.len() >= 2,
            "a changed file list must be sampled"
        );
        assert_ne!(transitioned_files[0].id, files.id);
        fs::remove_dir_all(directory).expect("cleanup test directory");
    }

    #[test]
    fn images_require_a_separate_opt_in_are_bounded_and_persist_atomically() {
        let directory = temporary_directory("clipboard-history-image");
        let history = ClipboardHistory::new(directory.clone());
        history.set_enabled(true).expect("enable text history");
        assert!(!history
            .capture_image(one_pixel(0x12, 0x34, 0x56))
            .expect("image disabled"));
        history
            .set_capture_options(true, false)
            .expect("enable image history");
        assert!(history
            .capture_image(one_pixel(0x12, 0x34, 0x56))
            .expect("capture image"));

        let snapshot = history.snapshot(Some(10));
        let image = snapshot.items.first().expect("image record");
        assert_eq!(image.kind, ClipboardHistoryItemKind::Image);
        assert!(image.text.is_empty());
        assert!(
            image
                .image
                .as_ref()
                .expect("public image metadata")
                .byte_length
                > 0
        );
        assert!(image.files.is_empty());
        assert!(
            !serde_json::to_string(image)
                .expect("serialize public image")
                .contains("data:image"),
            "a snapshot must never include image pixels"
        );
        assert!(history
            .image_preview(&image.id)
            .expect("explicit preview")
            .data_url
            .starts_with("data:image/png;base64,"));

        let persisted = fs::read_to_string(directory.join(HISTORY_FILE_NAME)).expect("state file");
        assert!(persisted.contains("imageHistoryEnabled"));
        assert!(fs::read_dir(directory.join(IMAGE_DIRECTORY_NAME))
            .expect("image directory")
            .any(|entry| entry
                .expect("entry")
                .path()
                .extension()
                .is_some_and(|extension| extension == "png")));
        assert!(
            !fs::read_dir(directory.clone())
                .expect("directory")
                .any(|entry| entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")),
            "atomic persistence must not leave temporary state files after success"
        );

        let reloaded = ClipboardHistory::new(directory.clone());
        assert_eq!(reloaded.snapshot(Some(10)).items.len(), 1);
        let oversized = arboard::ImageData {
            width: MAX_HISTORY_IMAGE_EDGE + 1,
            height: 1,
            bytes: Cow::Owned(Vec::new()),
        };
        assert!(reloaded
            .capture_image(oversized)
            .expect_err("oversized image must fail")
            .contains("edge limit"));
        fs::remove_dir_all(directory).expect("cleanup test directory");
    }

    #[test]
    fn persisted_image_preview_fails_closed_for_tampering_or_non_regular_files() {
        let directory = temporary_directory("clipboard-history-image-integrity");
        let history = ClipboardHistory::new(directory.clone());
        history.set_enabled(true).expect("enable");
        history
            .set_capture_options(true, false)
            .expect("enable image history");
        history
            .capture_image(one_pixel(0xaa, 0xbb, 0xcc))
            .expect("capture image");
        let id = history.snapshot(Some(10)).items.remove(0).id;
        let persisted = fs::read_to_string(directory.join(HISTORY_FILE_NAME)).expect("state file");
        assert!(persisted.contains("pngDigest"));

        let image_path = fs::read_dir(directory.join(IMAGE_DIRECTORY_NAME))
            .expect("image directory")
            .next()
            .expect("image file")
            .expect("image entry")
            .path();
        let mut png = fs::read(&image_path).expect("stored PNG");
        let last = png.len().checked_sub(1).expect("PNG has data");
        png[last] ^= 0x01;
        fs::write(&image_path, png).expect("tamper PNG");
        assert!(history
            .image_preview(&id)
            .expect_err("tampered preview must fail before IPC")
            .contains("integrity digest"));
        assert!(history
            .restore_to_system_clipboard(&id)
            .expect_err("tampered restore must fail before clipboard write")
            .contains("integrity digest"));

        fs::remove_file(&image_path).expect("remove tampered image");
        fs::create_dir(&image_path).expect("replace image with directory");
        assert!(history
            .image_preview(&id)
            .expect_err("non-regular preview must fail")
            .contains("regular file"));
        fs::remove_dir_all(directory).expect("cleanup test directory");
    }

    #[test]
    fn file_history_keeps_no_contents_and_revalidates_before_restore() {
        let directory = temporary_directory("clipboard-history-files");
        let source = directory.join("source");
        let file = source.join("private.txt");
        fs::create_dir_all(&source).expect("source directory");
        fs::write(&file, "very-private-file-content").expect("source file");

        let history = ClipboardHistory::new(directory.clone());
        history.set_enabled(true).expect("enable text history");
        history
            .set_capture_options(false, true)
            .expect("enable file history");
        assert!(history
            .capture_stored_files(stored_files_from_paths(vec![file.clone(), source.clone()]))
            .expect("capture file list"));

        let snapshot = history.snapshot(Some(10));
        let item = snapshot.items.first().expect("file item");
        assert_eq!(item.kind, ClipboardHistoryItemKind::Files);
        assert_eq!(item.files.len(), 2);
        let public_json = serde_json::to_string(item).expect("public item JSON");
        assert!(!public_json.contains("private-file-content"));
        let public_path = file.to_string_lossy();
        assert!(!public_json.contains(public_path.as_ref()));
        assert_eq!(
            history
                .prepare_file_entry_open(&item.id, 0)
                .expect("file target")
                .kind(),
            LocalOpenKind::File
        );
        assert_eq!(
            history
                .prepare_file_entry_open(&item.id, 1)
                .expect("folder target")
                .kind(),
            LocalOpenKind::Folder
        );

        fs::write(&file, "changed").expect("mutate source file");
        assert!(history.revalidated_file_entry_path(&item.id, 0).is_err());
        let persisted = fs::read_to_string(directory.join(HISTORY_FILE_NAME)).expect("state file");
        assert!(!persisted.contains("very-private-file-content"));
        fs::remove_dir_all(directory).expect("cleanup test directory");
    }

    #[test]
    fn plugin_text_projection_never_includes_images_or_files() {
        let directory = temporary_directory("clipboard-history-plugin-projection");
        let file = directory.join("entry.txt");
        fs::create_dir_all(&directory).expect("directory");
        fs::write(&file, "content").expect("file");
        let history = ClipboardHistory::new(directory.clone());
        history.set_enabled(true).expect("enable");
        history
            .set_capture_options(true, true)
            .expect("enable optional types");
        history.capture_text("text only".to_owned()).expect("text");
        history.capture_image(one_pixel(1, 2, 3)).expect("image");
        history
            .capture_stored_files(stored_files_from_paths(vec![file]))
            .expect("files");

        let plugin_snapshot = history.text_snapshot(Some(10));
        assert_eq!(plugin_snapshot.items.len(), 1);
        assert_eq!(
            plugin_snapshot.items[0].kind,
            ClipboardHistoryItemKind::Text
        );
        assert!(!plugin_snapshot.image_history_enabled);
        assert!(!plugin_snapshot.file_history_enabled);
        fs::remove_dir_all(directory).expect("cleanup test directory");
    }
}
