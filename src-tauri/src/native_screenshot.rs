use std::io::{self, Write};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use image::{codecs::png::PngEncoder, ColorType, ImageEncoder};
use serde::{Deserialize, Serialize};

/// The largest single display frame the host will keep in memory. This is a
/// deliberately conservative guard around an explicitly requested capture,
/// not a policy for a future recording pipeline.
pub const MAX_NATIVE_SCREENSHOT_EDGE: u32 = 8_192;
pub const MAX_NATIVE_SCREENSHOT_PIXELS: u64 = 24_000_000;
pub const MAX_NATIVE_SCREENSHOT_RAW_BYTES: usize =
    (MAX_NATIVE_SCREENSHOT_PIXELS as usize) * std::mem::size_of::<[u8; 4]>();
pub const MAX_NATIVE_SCREENSHOT_PNG_BYTES: usize = 16 * 1024 * 1024;
const _: () = assert!(MAX_NATIVE_SCREENSHOT_PNG_BYTES > 0);

/// Selects one active display by its zero-based system display-list index.
///
/// This request is intentionally small: callers cannot provide a rectangle,
/// a window handle, a capture duration, or a polling interval. A host UI can
/// invoke it after an explicit user action, while plugins continue to use the
/// browser-owned `getDisplayMedia` picker and are not granted native pixels.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeScreenshotRequest {
    pub display_index: Option<u32>,
}

/// A one-shot PNG returned through Tauri IPC.
///
/// The data URL is bounded before base64 encoding. Nothing is persisted to
/// disk, retained by this module, or captured in the background.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeScreenshot {
    pub data_url: String,
    pub name: String,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub display_index: u32,
}

#[derive(Debug)]
struct CapturedRgbaFrame {
    display_index: u32,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

/// Captures exactly one active monitor after a direct host invocation.
///
/// There is deliberately no timer, global shortcut listener, background
/// worker, file writer, or plugin bridge for this operation. Unsupported
/// platforms fail closed with an actionable message instead of falling back
/// to a hidden browser or desktop capture.
pub fn capture_native_screenshot(
    request: NativeScreenshotRequest,
) -> Result<NativeScreenshot, String> {
    let display_index = request.display_index.unwrap_or(0);
    let frame = capture_platform_monitor(display_index)?;
    png_payload_from_rgba(frame.display_index, frame.width, frame.height, &frame.rgba)
}

fn validate_capture_dimensions(width: u32, height: u32) -> Result<usize, String> {
    if width == 0 || height == 0 {
        return Err("The selected display has no capturable pixels.".to_owned());
    }
    if width > MAX_NATIVE_SCREENSHOT_EDGE || height > MAX_NATIVE_SCREENSHOT_EDGE {
        return Err(format!(
            "The selected display exceeds the {MAX_NATIVE_SCREENSHOT_EDGE}px per-edge screenshot limit."
        ));
    }

    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| {
            "The selected display dimensions overflow the supported range.".to_owned()
        })?;
    if pixels > MAX_NATIVE_SCREENSHOT_PIXELS {
        return Err(format!(
            "The selected display exceeds the {} megapixel screenshot limit.",
            MAX_NATIVE_SCREENSHOT_PIXELS / 1_000_000
        ));
    }

    let raw_bytes = pixels.checked_mul(4).ok_or_else(|| {
        "The selected display byte size overflows the supported range.".to_owned()
    })?;
    let raw_bytes = usize::try_from(raw_bytes).map_err(|_| {
        "The selected display byte size is unsupported on this platform.".to_owned()
    })?;
    if raw_bytes > MAX_NATIVE_SCREENSHOT_RAW_BYTES {
        return Err("The selected display would use too much screenshot memory.".to_owned());
    }

    Ok(raw_bytes)
}

fn png_payload_from_rgba(
    display_index: u32,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<NativeScreenshot, String> {
    let expected_raw_bytes = validate_capture_dimensions(width, height)?;
    if rgba.len() != expected_raw_bytes {
        return Err("Native screenshot capture returned an invalid RGBA pixel buffer.".to_owned());
    }

    let mut png = LimitedPngBuffer::new(MAX_NATIVE_SCREENSHOT_PNG_BYTES);
    let result =
        PngEncoder::new(&mut png).write_image(rgba, width, height, ColorType::Rgba8.into());
    if let Err(error) = result {
        if png.limit_exceeded {
            return Err(format!(
                "The selected display PNG exceeds the {} MiB payload limit.",
                MAX_NATIVE_SCREENSHOT_PNG_BYTES / (1024 * 1024)
            ));
        }
        return Err(format!(
            "The native screenshot could not be encoded as PNG: {error}"
        ));
    }

    Ok(NativeScreenshot {
        data_url: format!(
            "data:image/png;base64,{}",
            BASE64_STANDARD.encode(png.bytes)
        ),
        name: format!("ihub-monitor-{display_index}.png"),
        mime_type: "image/png".to_owned(),
        width,
        height,
        display_index,
    })
}

/// Lets the PNG encoder fail before a single high-entropy display capture can
/// retain an unbounded compressed output buffer in the iHub process.
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
        let remaining = self.limit.saturating_sub(self.bytes.len());
        let writable = remaining.min(buffer.len());
        self.bytes.extend_from_slice(&buffer[..writable]);
        if writable < buffer.len() {
            self.limit_exceeded = true;
        }
        Ok(writable)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(windows)]
fn capture_platform_monitor(display_index: u32) -> Result<CapturedRgbaFrame, String> {
    capture_windows_monitor(display_index)
}

#[cfg(target_os = "macos")]
fn capture_platform_monitor(display_index: u32) -> Result<CapturedRgbaFrame, String> {
    capture_macos_monitor(display_index)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn capture_platform_monitor(_display_index: u32) -> Result<CapturedRgbaFrame, String> {
    Err("Native monitor screenshots are currently supported on Windows and macOS only.".to_owned())
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy)]
struct WindowsMonitorBounds {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
}

#[cfg(windows)]
fn capture_windows_monitor(display_index: u32) -> Result<CapturedRgbaFrame, String> {
    use windows_sys::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC,
        SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, SRCCOPY,
    };

    let bounds = windows_monitor_bounds(display_index)?;
    let raw_bytes = validate_capture_dimensions(bounds.width, bounds.height)?;
    let width = i32::try_from(bounds.width)
        .map_err(|_| "The selected display width is unsupported by Windows GDI.".to_owned())?;
    let height = i32::try_from(bounds.height)
        .map_err(|_| "The selected display height is unsupported by Windows GDI.".to_owned())?;
    let image_bytes = u32::try_from(raw_bytes)
        .map_err(|_| "The selected display byte size is unsupported by Windows GDI.".to_owned())?;

    // A top-down, 32-bit DIB gives us a predictable row order. GDI writes
    // BGRA/XRGB bytes; they are copied into a standalone RGBA buffer before
    // the native bitmap/DC resources are released.
    let desktop_dc = unsafe { GetDC(std::ptr::null_mut()) };
    if desktop_dc.is_null() {
        return Err(format!("GetDC failed: {}", std::io::Error::last_os_error()));
    }
    let memory_dc = unsafe { CreateCompatibleDC(desktop_dc) };
    if memory_dc.is_null() {
        let _ = unsafe { ReleaseDC(std::ptr::null_mut(), desktop_dc) };
        return Err(format!(
            "CreateCompatibleDC failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    let bitmap_info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: image_bytes,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pixels = std::ptr::null_mut();
    let bitmap = unsafe {
        CreateDIBSection(
            desktop_dc,
            &bitmap_info,
            DIB_RGB_COLORS,
            &mut pixels,
            std::ptr::null_mut(),
            0,
        )
    };
    if bitmap.is_null() || pixels.is_null() {
        if !bitmap.is_null() {
            let _ = unsafe { DeleteObject(bitmap) };
        }
        let _ = unsafe { DeleteDC(memory_dc) };
        let _ = unsafe { ReleaseDC(std::ptr::null_mut(), desktop_dc) };
        return Err(format!(
            "CreateDIBSection failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    let previous_bitmap = unsafe { SelectObject(memory_dc, bitmap) };
    if previous_bitmap.is_null() || previous_bitmap as isize == -1 {
        let _ = unsafe { DeleteDC(memory_dc) };
        let _ = unsafe { DeleteObject(bitmap) };
        let _ = unsafe { ReleaseDC(std::ptr::null_mut(), desktop_dc) };
        return Err(format!(
            "SelectObject failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    let capture_result = (|| {
        let copied = unsafe {
            BitBlt(
                memory_dc,
                0,
                0,
                width,
                height,
                desktop_dc,
                bounds.left,
                bounds.top,
                SRCCOPY | CAPTUREBLT,
            )
        };
        if copied == 0 {
            return Err(format!(
                "BitBlt failed: {}",
                std::io::Error::last_os_error()
            ));
        }

        let bgra = unsafe { std::slice::from_raw_parts(pixels.cast::<u8>(), raw_bytes) };
        let rgba = rgba_from_windows_bgra(bgra, raw_bytes)?;
        Ok(CapturedRgbaFrame {
            display_index,
            width: bounds.width,
            height: bounds.height,
            rgba,
        })
    })();

    // Always detach the bitmap before destroying GDI resources. If a cleanup
    // operation itself fails, preserve the capture error but fail a successful
    // capture rather than returning a frame after an incomplete release.
    let restored = unsafe { SelectObject(memory_dc, previous_bitmap) };
    let deleted_memory_dc = unsafe { DeleteDC(memory_dc) };
    let deleted_bitmap = unsafe { DeleteObject(bitmap) };
    let released_desktop_dc = unsafe { ReleaseDC(std::ptr::null_mut(), desktop_dc) };

    if let Err(error) = capture_result {
        return Err(error);
    }
    if restored.is_null()
        || restored as isize == -1
        || deleted_memory_dc == 0
        || deleted_bitmap == 0
        || released_desktop_dc == 0
    {
        return Err("Windows released a native screenshot resource incompletely.".to_owned());
    }

    capture_result
}

#[cfg(windows)]
fn rgba_from_windows_bgra(bgra: &[u8], expected_raw_bytes: usize) -> Result<Vec<u8>, String> {
    if bgra.len() != expected_raw_bytes || bgra.len() % 4 != 0 {
        return Err("Windows returned an invalid BGRA screenshot buffer.".to_owned());
    }

    let mut rgba = Vec::with_capacity(expected_raw_bytes);
    for pixel in bgra.chunks_exact(4) {
        // A BI_RGB 32-bit DIB stores blue, green, red and an ignored alpha/X
        // byte. Capture output is made fully opaque before PNG encoding.
        rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
    }
    Ok(rgba)
}

#[cfg(windows)]
fn windows_monitor_bounds(display_index: u32) -> Result<WindowsMonitorBounds, String> {
    use windows_sys::Win32::{
        Foundation::LPARAM,
        Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO},
    };

    unsafe extern "system" fn collect_monitor(
        monitor: HMONITOR,
        _monitor_dc: HDC,
        _monitor_rect: *mut windows_sys::Win32::Foundation::RECT,
        user_data: LPARAM,
    ) -> windows_sys::core::BOOL {
        let monitors = unsafe { &mut *(user_data as *mut Vec<HMONITOR>) };
        monitors.push(monitor);
        1
    }

    let mut monitors = Vec::new();
    let enumerated = unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(collect_monitor),
            (&mut monitors as *mut Vec<HMONITOR>) as LPARAM,
        )
    };
    if enumerated == 0 {
        return Err(format!(
            "EnumDisplayMonitors failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    let monitor = monitors.get(display_index as usize).ok_or_else(|| {
        format!(
            "Display index {display_index} is unavailable; Windows currently reports {} active display(s).",
            monitors.len()
        )
    })?;
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetMonitorInfoW(*monitor, &mut info) } == 0 {
        return Err(format!(
            "GetMonitorInfoW failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    let width = info
        .rcMonitor
        .right
        .checked_sub(info.rcMonitor.left)
        .filter(|value| *value > 0)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| "Windows reported invalid monitor width.".to_owned())?;
    let height = info
        .rcMonitor
        .bottom
        .checked_sub(info.rcMonitor.top)
        .filter(|value| *value > 0)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| "Windows reported invalid monitor height.".to_owned())?;

    Ok(WindowsMonitorBounds {
        left: info.rcMonitor.left,
        top: info.rcMonitor.top,
        width,
        height,
    })
}

#[cfg(target_os = "macos")]
fn capture_macos_monitor(display_index: u32) -> Result<CapturedRgbaFrame, String> {
    use core_graphics::{
        access::ScreenCaptureAccess,
        base::kCGImageAlphaPremultipliedLast,
        color_space::CGColorSpace,
        context::CGContext,
        display::CGDisplay,
        geometry::{CGPoint, CGRect, CGSize},
    };

    // macOS requires Screen Recording permission even for a single image.
    // This request is reached only by the Tauri command above, which has no
    // timer or plugin bridge, so it never becomes a background capture path.
    let screen_access = ScreenCaptureAccess;
    if !screen_access.preflight() && !screen_access.request() {
        return Err(
            "macOS needs Screen Recording permission to capture a display. Grant it for iHub in System Settings > Privacy & Security > Screen Recording, then try again."
                .to_owned(),
        );
    }

    let displays = CGDisplay::active_displays()
        .map_err(|error| format!("macOS could not list active displays: {error}"))?;
    let display_id = *displays.get(display_index as usize).ok_or_else(|| {
        format!(
            "Display index {display_index} is unavailable; macOS currently reports {} active display(s).",
            displays.len()
        )
    })?;
    let display = CGDisplay::new(display_id);
    let image = display.image().ok_or_else(|| {
        "macOS could not capture the selected display. Check Screen Recording permission."
            .to_owned()
    })?;
    let width = u32::try_from(image.width())
        .map_err(|_| "macOS reported an unsupported display width.".to_owned())?;
    let height = u32::try_from(image.height())
        .map_err(|_| "macOS reported an unsupported display height.".to_owned())?;
    let raw_bytes = validate_capture_dimensions(width, height)?;
    let bytes_per_row = usize::try_from(width)
        .ok()
        .and_then(|value| value.checked_mul(4))
        .ok_or_else(|| "macOS screenshot row bytes overflow the supported range.".to_owned())?;
    let mut rgba = vec![0_u8; raw_bytes];
    let color_space = CGColorSpace::create_device_rgb();
    let context = CGContext::create_bitmap_context(
        Some(rgba.as_mut_ptr().cast()),
        width as usize,
        height as usize,
        8,
        bytes_per_row,
        &color_space,
        kCGImageAlphaPremultipliedLast,
    );
    let destination = CGRect::new(
        &CGPoint::new(0.0, 0.0),
        &CGSize::new(f64::from(width), f64::from(height)),
    );
    context.draw_image(destination, &image);
    context.flush();

    Ok(CapturedRgbaFrame {
        display_index,
        width,
        height,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};

    use super::{
        png_payload_from_rgba, validate_capture_dimensions, LimitedPngBuffer,
        MAX_NATIVE_SCREENSHOT_EDGE, MAX_NATIVE_SCREENSHOT_PIXELS,
    };

    #[test]
    fn dimensions_reject_empty_oversized_and_over_pixel_frames_before_capture() {
        assert!(validate_capture_dimensions(0, 1).is_err());
        assert!(validate_capture_dimensions(1, 0).is_err());
        assert!(validate_capture_dimensions(MAX_NATIVE_SCREENSHOT_EDGE + 1, 1).is_err());

        let too_many_pixels_height =
            (MAX_NATIVE_SCREENSHOT_PIXELS / u64::from(MAX_NATIVE_SCREENSHOT_EDGE) + 1) as u32;
        assert!(
            validate_capture_dimensions(MAX_NATIVE_SCREENSHOT_EDGE, too_many_pixels_height)
                .is_err()
        );
    }

    #[test]
    fn png_payload_requires_an_exact_rgba_buffer_and_stays_a_png_data_url() {
        assert!(png_payload_from_rgba(0, 2, 2, &[0; 15]).is_err());

        let screenshot = png_payload_from_rgba(2, 1, 1, &[0x12, 0x34, 0x56, 0xff])
            .expect("a bounded one-pixel screenshot should encode");
        assert_eq!(screenshot.name, "ihub-monitor-2.png");
        assert_eq!(screenshot.mime_type, "image/png");
        assert_eq!(screenshot.width, 1);
        assert_eq!(screenshot.height, 1);
        assert_eq!(screenshot.display_index, 2);
        let encoded = screenshot
            .data_url
            .strip_prefix("data:image/png;base64,")
            .expect("PNG data URL prefix");
        let decoded = BASE64_STANDARD.decode(encoded).expect("valid base64 PNG");
        assert!(decoded.starts_with(b"\x89PNG\r\n\x1a\n"));
    }

    #[test]
    fn limited_png_writer_refuses_to_grow_past_its_cap() {
        let mut writer = LimitedPngBuffer::new(3);
        writer.write_all(&[1, 2, 3]).expect("initial bytes fit");
        assert!(writer.write_all(&[4]).is_err());
        assert!(writer.limit_exceeded);
        assert_eq!(writer.bytes, vec![1, 2, 3]);

        let mut zero_limit = LimitedPngBuffer::new(0);
        assert!(zero_limit.write_all(&[9]).is_err());
        assert!(zero_limit.limit_exceeded);
    }

    #[cfg(windows)]
    #[test]
    fn windows_dib_bgra_bytes_are_converted_without_leaking_the_unused_alpha_byte() {
        let rgba = super::rgba_from_windows_bgra(&[0x56, 0x34, 0x12, 0, 0xcc, 0xbb, 0xaa, 17], 8)
            .expect("two complete BGRA pixels should convert");
        assert_eq!(rgba, vec![0x12, 0x34, 0x56, 255, 0xaa, 0xbb, 0xcc, 255]);
        assert!(super::rgba_from_windows_bgra(&[0; 3], 3).is_err());
    }
}
