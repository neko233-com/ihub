//! Serializes short-lived access to the platform clipboard.
//!
//! Windows exposes one global clipboard and `arboard` documents that
//! concurrent access can fail while another operation is in flight. iHub has
//! an opt-in history sampler plus explicit user actions, so they share this
//! tiny retrying gate instead of making a paste intermittently fail.

use std::{
    sync::{Mutex, OnceLock, TryLockError},
    thread,
    time::Duration,
};

const CLIPBOARD_ATTEMPTS: usize = 3;
const CLIPBOARD_RETRY_DELAY: Duration = Duration::from_millis(12);

/// Limits used exclusively by the opt-in *background* clipboard sampler.
/// Explicit paste/copy actions retain their existing behavior because a
/// person has just asked iHub to read that format. Background work must be
/// much more conservative: it either proves the Windows source is bounded or
/// skips the entire poll.
#[derive(Debug, Clone, Copy)]
pub struct BackgroundClipboardReadLimits {
    /// Maximum UTF-16 source allocation iHub will allow arboard to read.
    /// This may be larger than the persisted UTF-8 text limit because UTF-16
    /// and UTF-8 use different byte widths; the caller still applies its
    /// stricter post-read history cap.
    pub max_text_source_bytes: usize,
    pub image: Option<BackgroundClipboardImageLimits>,
    pub max_file_list_source_bytes: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
pub struct BackgroundClipboardImageLimits {
    /// Maximum raw clipboard source (PNG/DIBV5) that arboard may allocate.
    pub max_source_bytes: usize,
    pub max_edge: usize,
    pub max_pixels: usize,
    pub max_rgba_bytes: usize,
}

static CLIPBOARD_GATE: OnceLock<Mutex<()>> = OnceLock::new();

/// Runs one narrow platform clipboard operation. The lock only covers the
/// native clipboard call; callers must not perform disk or UI work inside the
/// closure. A couple of short retries smooth over an external app briefly
/// holding Windows' clipboard without turning a permanent unsupported format
/// into an unbounded wait.
pub fn with_clipboard<T>(
    mut operation: impl FnMut(&mut arboard::Clipboard) -> Result<T, arboard::Error>,
) -> Result<T, arboard::Error> {
    let gate = CLIPBOARD_GATE.get_or_init(|| Mutex::new(()));
    let _guard = gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    for attempt in 0..CLIPBOARD_ATTEMPTS {
        match arboard::Clipboard::new().and_then(|mut clipboard| operation(&mut clipboard)) {
            Ok(value) => return Ok(value),
            Err(error)
                if attempt + 1 < CLIPBOARD_ATTEMPTS
                    && matches!(error, arboard::Error::ClipboardOccupied) =>
            {
                thread::sleep(CLIPBOARD_RETRY_DELAY);
            }
            Err(error) => return Err(error),
        }
    }

    unreachable!("clipboard retries always return on the final attempt")
}

/// Runs a bounded background clipboard read. On Windows this does a native
/// size/dimension preflight under the system clipboard lock before arboard is
/// allowed to allocate or decode text, image, or HDROP data. The sequence
/// number is checked before and after the arboard operation so a clipboard
/// change in the small handoff window is discarded rather than persisted.
///
/// arboard 3 exposes no equivalent cross-platform preflight API. On macOS
/// and other targets the caller's post-read caps still reject oversize values,
/// but the platform API may already have allocated them. Keep this limitation
/// explicit; do not describe non-Windows polling as pre-allocation bounded.
/// Even on Windows, arboard's public API cannot read while iHub retains the
/// native preflight lock, so an external clipboard writer can theoretically
/// race the handoff. The sequence checks discard that result; replacing
/// arboard's internal decoder would be required for a formal zero-allocation
/// guarantee across that race.
pub fn try_with_bounded_background_clipboard<T>(
    limits: BackgroundClipboardReadLimits,
    operation: impl FnOnce(&mut arboard::Clipboard) -> Result<T, arboard::Error>,
) -> Option<Result<T, arboard::Error>> {
    let gate = CLIPBOARD_GATE.get_or_init(|| Mutex::new(()));
    let _guard = match gate.try_lock() {
        Ok(guard) => guard,
        Err(TryLockError::WouldBlock) => return None,
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
    };

    #[cfg(target_os = "windows")]
    let sequence = windows_background_clipboard_preflight(limits)?;
    #[cfg(not(target_os = "windows"))]
    let _ = (
        limits.max_text_source_bytes,
        limits.image.map(|image| {
            (
                image.max_source_bytes,
                image.max_edge,
                image.max_pixels,
                image.max_rgba_bytes,
            )
        }),
        limits.max_file_list_source_bytes,
    );

    #[cfg(target_os = "windows")]
    if windows_clipboard_sequence() != Some(sequence) {
        return None;
    }

    let result = arboard::Clipboard::new().and_then(|mut clipboard| operation(&mut clipboard));

    #[cfg(target_os = "windows")]
    if windows_clipboard_sequence() != Some(sequence) {
        // The source changed after native preflight. Even a successful read
        // could now describe a different clipboard state, so fail closed.
        return None;
    }

    Some(result)
}

/// Reads a native clipboard file list only after proving its source payload
/// is bounded. Unlike the background sampler this ignores unrelated text and
/// image formats, so an explicit visible-surface file query is not rejected
/// merely because Explorer also advertised auxiliary clipboard data.
///
/// `None` means the shared clipboard gate was busy, the Windows preflight
/// could not prove a stable bounded source, or the clipboard changed during
/// the handoff to arboard. Callers should treat that as no available list.
pub fn try_with_bounded_file_clipboard<T>(
    max_file_list_source_bytes: usize,
    operation: impl FnOnce(&mut arboard::Clipboard) -> Result<T, arboard::Error>,
) -> Option<Result<T, arboard::Error>> {
    let gate = CLIPBOARD_GATE.get_or_init(|| Mutex::new(()));
    let _guard = match gate.try_lock() {
        Ok(guard) => guard,
        Err(TryLockError::WouldBlock) => return None,
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
    };

    #[cfg(target_os = "windows")]
    let sequence = windows_file_clipboard_preflight(max_file_list_source_bytes)?;
    #[cfg(not(target_os = "windows"))]
    let _ = max_file_list_source_bytes;

    #[cfg(target_os = "windows")]
    if windows_clipboard_sequence() != Some(sequence) {
        return None;
    }

    let result = arboard::Clipboard::new().and_then(|mut clipboard| operation(&mut clipboard));

    #[cfg(target_os = "windows")]
    if windows_clipboard_sequence() != Some(sequence) {
        return None;
    }

    Some(result)
}

#[cfg(any(target_os = "windows", test))]
fn image_dimensions_within_limits(
    width: u32,
    height: u32,
    limits: BackgroundClipboardImageLimits,
) -> bool {
    let width = usize::try_from(width).ok();
    let height = usize::try_from(height).ok();
    let Some((width, height)) = width.zip(height) else {
        return false;
    };
    if width == 0 || height == 0 || width > limits.max_edge || height > limits.max_edge {
        return false;
    }
    let Some(pixels) = width.checked_mul(height) else {
        return false;
    };
    if pixels > limits.max_pixels {
        return false;
    }
    pixels
        .checked_mul(4)
        .is_some_and(|bytes| bytes <= limits.max_rgba_bytes)
}

#[cfg(any(target_os = "windows", test))]
fn png_dimensions_from_header(bytes: &[u8]) -> Option<(u32, u32)> {
    const PNG_SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];
    const PNG_IHDR_PREFIX: [u8; 8] = [0, 0, 0, 13, b'I', b'H', b'D', b'R'];
    let header = bytes.get(..24)?;
    if header[..8] != PNG_SIGNATURE || header[8..16] != PNG_IHDR_PREFIX {
        return None;
    }
    let width = u32::from_be_bytes(header[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(header[20..24].try_into().ok()?);
    Some((width, height))
}

#[cfg(target_os = "windows")]
fn windows_clipboard_sequence() -> Option<u32> {
    use windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber;

    let sequence = unsafe { GetClipboardSequenceNumber() };
    (sequence != 0).then_some(sequence)
}

#[cfg(target_os = "windows")]
fn windows_background_clipboard_preflight(limits: BackgroundClipboardReadLimits) -> Option<u32> {
    let sequence = windows_clipboard_sequence()?;
    let _clipboard = WindowsClipboardGuard::open()?;

    if !windows_format_is_within_limit(CF_UNICODETEXT, limits.max_text_source_bytes, |_| true) {
        return None;
    }

    if let Some(image_limits) = limits.image {
        let png_format = windows_png_format()?;
        let png_present = windows_format_is_present(png_format)?;
        let image_ok = if png_present {
            windows_format_is_within_limit(png_format, image_limits.max_source_bytes, |bytes| {
                png_dimensions_from_header(bytes).is_some_and(|(width, height)| {
                    image_dimensions_within_limits(width, height, image_limits)
                })
            })
        } else {
            windows_format_is_within_limit(CF_DIBV5, image_limits.max_source_bytes, |bytes| {
                dibv5_dimensions(bytes).is_some_and(|(width, height)| {
                    image_dimensions_within_limits(width, height, image_limits)
                })
            })
        };
        if !image_ok {
            return None;
        }
    }

    if let Some(max_file_list_source_bytes) = limits.max_file_list_source_bytes {
        if !windows_format_is_within_limit(CF_HDROP, max_file_list_source_bytes, |_| true) {
            return None;
        }
    }

    // `OpenClipboard` prevents the source from changing during inspection;
    // this final check detects a change that happened before the lock opened.
    (windows_clipboard_sequence() == Some(sequence)).then_some(sequence)
}

#[cfg(target_os = "windows")]
fn windows_file_clipboard_preflight(max_file_list_source_bytes: usize) -> Option<u32> {
    let sequence = windows_clipboard_sequence()?;
    let _clipboard = WindowsClipboardGuard::open()?;
    if !windows_format_is_present(CF_HDROP)?
        || !windows_format_is_within_limit(CF_HDROP, max_file_list_source_bytes, |_| true)
    {
        return None;
    }
    (windows_clipboard_sequence() == Some(sequence)).then_some(sequence)
}

#[cfg(target_os = "windows")]
const CF_UNICODETEXT: u32 = 13;
#[cfg(target_os = "windows")]
const CF_HDROP: u32 = 15;
#[cfg(target_os = "windows")]
const CF_DIBV5: u32 = 17;

#[cfg(target_os = "windows")]
struct WindowsClipboardGuard;

#[cfg(target_os = "windows")]
impl WindowsClipboardGuard {
    fn open() -> Option<Self> {
        use windows_sys::Win32::System::DataExchange::OpenClipboard;

        (unsafe { OpenClipboard(std::ptr::null_mut()) } != 0).then_some(Self)
    }
}

#[cfg(target_os = "windows")]
impl Drop for WindowsClipboardGuard {
    fn drop(&mut self) {
        use windows_sys::Win32::System::DataExchange::CloseClipboard;

        unsafe {
            let _ = CloseClipboard();
        }
    }
}

#[cfg(target_os = "windows")]
fn windows_png_format() -> Option<u32> {
    use windows_sys::Win32::System::DataExchange::RegisterClipboardFormatW;

    const PNG_FORMAT_NAME: [u16; 4] = [b'P' as u16, b'N' as u16, b'G' as u16, 0];
    let format = unsafe { RegisterClipboardFormatW(PNG_FORMAT_NAME.as_ptr()) };
    (format != 0).then_some(format)
}

#[cfg(target_os = "windows")]
fn windows_format_is_present(format: u32) -> Option<bool> {
    use windows_sys::Win32::System::DataExchange::IsClipboardFormatAvailable;

    // The Windows API only reports availability here. A later null handle or
    // failed lock is intentionally treated as uncertain by the caller.
    Some(unsafe { IsClipboardFormatAvailable(format) != 0 })
}

#[cfg(target_os = "windows")]
fn windows_format_is_within_limit(
    format: u32,
    limit: usize,
    validate_prefix: impl FnOnce(&[u8]) -> bool,
) -> bool {
    use windows_sys::Win32::System::{
        DataExchange::{GetClipboardData, IsClipboardFormatAvailable},
        Memory::{GlobalLock, GlobalSize, GlobalUnlock},
    };

    unsafe {
        if IsClipboardFormatAvailable(format) == 0 {
            return true;
        }
        let handle = GetClipboardData(format);
        if handle.is_null() {
            return false;
        }
        let pointer = GlobalLock(handle);
        if pointer.is_null() {
            return false;
        }
        let size = GlobalSize(handle);
        if size == 0 || size > limit {
            let _ = GlobalUnlock(handle);
            return false;
        }
        // The caller only inspects a fixed small header for image formats.
        // It never creates a slice longer than the source cap validated above.
        let bytes = std::slice::from_raw_parts(pointer.cast::<u8>(), size);
        let result = validate_prefix(bytes);
        let _ = GlobalUnlock(handle);
        result
    }
}

#[cfg(target_os = "windows")]
fn dibv5_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    // BITMAPV5HEADER has a fixed 124-byte layout. Only width/height are
    // needed before arboard's BMP decoder allocates a RGBA output buffer.
    let header = bytes.get(..124)?;
    if u32::from_le_bytes(header[..4].try_into().ok()?) < 124 {
        return None;
    }
    let width = i32::from_le_bytes(header[4..8].try_into().ok()?);
    let height = i32::from_le_bytes(header[8..12].try_into().ok()?);
    let width = u32::try_from(width).ok()?;
    let height = height
        .checked_abs()
        .and_then(|height| u32::try_from(height).ok())?;
    Some((width, height))
}

#[cfg(test)]
mod tests {
    use super::{
        image_dimensions_within_limits, png_dimensions_from_header, BackgroundClipboardImageLimits,
    };

    const IMAGE_LIMITS: BackgroundClipboardImageLimits = BackgroundClipboardImageLimits {
        max_source_bytes: 64,
        max_edge: 4,
        max_pixels: 12,
        max_rgba_bytes: 48,
    };

    #[test]
    fn image_preflight_rejects_overflowing_or_oversize_dimensions() {
        assert!(image_dimensions_within_limits(3, 4, IMAGE_LIMITS));
        assert!(!image_dimensions_within_limits(5, 1, IMAGE_LIMITS));
        assert!(!image_dimensions_within_limits(4, 4, IMAGE_LIMITS));
        assert!(!image_dimensions_within_limits(0, 1, IMAGE_LIMITS));
    }

    #[test]
    fn png_header_parser_requires_a_real_ihdr() {
        let mut png = vec![
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, b'I', b'H', b'D', b'R',
        ];
        png.extend_from_slice(&3u32.to_be_bytes());
        png.extend_from_slice(&4u32.to_be_bytes());
        assert_eq!(png_dimensions_from_header(&png), Some((3, 4)));
        png[12] = b'X';
        assert_eq!(png_dimensions_from_header(&png), None);
    }
}
