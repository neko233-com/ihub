use std::{
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use serde::Serialize;
use uuid::Uuid;

pub const MAX_CURSOR_COLOR_DELAY_MS: u64 = 5_000;
pub const CURSOR_COLOR_NEIGHBORHOOD_EDGE: usize = 9;
pub const CURSOR_COLOR_NEIGHBORHOOD_PIXELS: usize =
    CURSOR_COLOR_NEIGHBORHOOD_EDGE * CURSOR_COLOR_NEIGHBORHOOD_EDGE;
pub const MIN_CURSOR_COLOR_SAMPLE_INTERVAL_MS: u64 = 72;
pub const MAX_CURSOR_COLOR_PICKER_SESSION_MS: u64 = 30_000;

/// A single color sample made at the cursor's screen coordinates.
///
/// This value is returned directly to the caller. It is never persisted and
/// the sampler does not retain or poll cursor data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorColorSample {
    pub hex: String,
    pub rgb: String,
    pub x: i32,
    pub y: i32,
}

/// A short, host-owned capability for a live color-picker session.
///
/// The token is issued only after a direct built-in UI action. A session lasts
/// at most 30 seconds, permits one 9×9 sample every 72 ms, and is not available
/// through the third-party plugin bridge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorColorPickerSession {
    pub session_id: String,
    pub sample_edge: usize,
    pub minimum_interval_ms: u64,
    pub expires_after_ms: u64,
}

/// One fixed-size cursor neighborhood for the frontend magnifier. Pixel colors
/// are row-major uppercase HEX values, with the cursor at index 40.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorColorNeighborhoodSample {
    pub hex: String,
    pub rgb: String,
    pub x: i32,
    pub y: i32,
    pub sample_edge: usize,
    pub pixels: Vec<String>,
    pub left_pressed: bool,
    pub right_pressed: bool,
    pub escape_pressed: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PickerInputState {
    left_pressed: bool,
    right_pressed: bool,
    escape_pressed: bool,
}

#[derive(Debug)]
struct ActivePickerSession {
    id: String,
    expires_at: Instant,
    last_sample_at: Option<Instant>,
}

#[derive(Debug, Default)]
struct PickerSessionRegistry {
    active: Option<ActivePickerSession>,
}

impl PickerSessionRegistry {
    fn begin(&mut self, id: String, now: Instant) -> Result<CursorColorPickerSession, String> {
        if self
            .active
            .as_ref()
            .is_some_and(|session| session.expires_at > now)
        {
            return Err("A native color picker is already active.".to_owned());
        }

        self.active = Some(ActivePickerSession {
            id: id.clone(),
            expires_at: now + Duration::from_millis(MAX_CURSOR_COLOR_PICKER_SESSION_MS),
            last_sample_at: None,
        });
        Ok(CursorColorPickerSession {
            session_id: id,
            sample_edge: CURSOR_COLOR_NEIGHBORHOOD_EDGE,
            minimum_interval_ms: MIN_CURSOR_COLOR_SAMPLE_INTERVAL_MS,
            expires_after_ms: MAX_CURSOR_COLOR_PICKER_SESSION_MS,
        })
    }

    fn reserve_sample(&mut self, id: &str, now: Instant) -> Result<(), String> {
        let Some(session) = self.active.as_mut() else {
            return Err("The native color picker session is not active.".to_owned());
        };
        if session.id != id {
            return Err("The native color picker session token is invalid.".to_owned());
        }
        if session.expires_at <= now {
            self.active = None;
            return Err("The native color picker session expired.".to_owned());
        }
        if session.last_sample_at.is_some_and(|last_sample| {
            now.saturating_duration_since(last_sample)
                < Duration::from_millis(MIN_CURSOR_COLOR_SAMPLE_INTERVAL_MS)
        }) {
            return Err(format!(
                "Native color samples are limited to one every {MIN_CURSOR_COLOR_SAMPLE_INTERVAL_MS} milliseconds."
            ));
        }
        session.last_sample_at = Some(now);
        Ok(())
    }

    fn end(&mut self, id: &str) {
        if self.active.as_ref().is_some_and(|session| session.id == id) {
            self.active = None;
        }
    }
}

fn picker_sessions() -> &'static Mutex<PickerSessionRegistry> {
    static SESSIONS: OnceLock<Mutex<PickerSessionRegistry>> = OnceLock::new();
    SESSIONS.get_or_init(|| Mutex::new(PickerSessionRegistry::default()))
}

fn with_picker_sessions<T>(
    operation: impl FnOnce(&mut PickerSessionRegistry) -> Result<T, String>,
) -> Result<T, String> {
    let mut sessions = picker_sessions()
        .lock()
        .map_err(|_| "The native color picker session state is unavailable.".to_owned())?;
    operation(&mut sessions)
}

pub fn begin_cursor_color_picker() -> Result<CursorColorPickerSession, String> {
    with_picker_sessions(|sessions| sessions.begin(Uuid::new_v4().to_string(), Instant::now()))
}

pub fn end_cursor_color_picker(session_id: &str) -> Result<(), String> {
    let normalized = session_id.trim();
    if normalized.is_empty() || normalized.len() > 64 {
        return Err("The native color picker session token is invalid.".to_owned());
    }
    with_picker_sessions(|sessions| {
        sessions.end(normalized);
        Ok(())
    })
}

pub fn sample_cursor_color_neighborhood(
    session_id: &str,
) -> Result<CursorColorNeighborhoodSample, String> {
    let normalized = session_id.trim();
    if normalized.is_empty() || normalized.len() > 64 {
        return Err("The native color picker session token is invalid.".to_owned());
    }
    with_picker_sessions(|sessions| sessions.reserve_sample(normalized, Instant::now()))?;
    sample_platform_cursor_neighborhood()
}

pub fn validate_cursor_color_delay(delay_ms: u64) -> Result<u64, String> {
    if delay_ms > MAX_CURSOR_COLOR_DELAY_MS {
        return Err(format!(
            "Cursor color sampling delay must be between 0 and {MAX_CURSOR_COLOR_DELAY_MS} milliseconds."
        ));
    }
    Ok(delay_ms)
}

/// Waits for the explicit, bounded delay and then samples exactly one pixel at
/// the current cursor position. The caller is responsible for scheduling this
/// on a blocking worker so the desktop command stays responsive.
pub fn sample_cursor_color(delay_ms: u64) -> Result<CursorColorSample, String> {
    let delay_ms = validate_cursor_color_delay(delay_ms)?;

    #[cfg(windows)]
    {
        if delay_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        }
        sample_windows_cursor_pixel()
    }

    #[cfg(target_os = "macos")]
    {
        if delay_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        }
        sample_macos_cursor_pixel()
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = delay_ms;
        Err(
            "Native cursor color sampling is currently supported on Windows and macOS only."
                .to_owned(),
        )
    }
}

#[cfg(any(windows, test))]
fn sample_from_colorref(colorref: u32, x: i32, y: i32) -> CursorColorSample {
    // A Windows COLORREF stores red in the low byte, then green, then blue.
    let red = (colorref & 0xff) as u8;
    let green = ((colorref >> 8) & 0xff) as u8;
    let blue = ((colorref >> 16) & 0xff) as u8;

    sample_from_rgb(red, green, blue, x, y)
}

fn sample_from_rgb(red: u8, green: u8, blue: u8, x: i32, y: i32) -> CursorColorSample {
    CursorColorSample {
        hex: format!("#{red:02X}{green:02X}{blue:02X}"),
        rgb: format!("rgb({red}, {green}, {blue})"),
        x,
        y,
    }
}

fn neighborhood_from_rgb(
    pixels: Vec<[u8; 3]>,
    x: i32,
    y: i32,
    input: PickerInputState,
) -> Result<CursorColorNeighborhoodSample, String> {
    if pixels.len() != CURSOR_COLOR_NEIGHBORHOOD_PIXELS {
        return Err("Native color sampling returned an invalid 9×9 pixel grid.".to_owned());
    }
    let center = pixels[CURSOR_COLOR_NEIGHBORHOOD_PIXELS / 2];
    let sample = sample_from_rgb(center[0], center[1], center[2], x, y);
    Ok(CursorColorNeighborhoodSample {
        hex: sample.hex,
        rgb: sample.rgb,
        x,
        y,
        sample_edge: CURSOR_COLOR_NEIGHBORHOOD_EDGE,
        pixels: pixels
            .into_iter()
            .map(|[red, green, blue]| format!("#{red:02X}{green:02X}{blue:02X}"))
            .collect(),
        left_pressed: input.left_pressed,
        right_pressed: input.right_pressed,
        escape_pressed: input.escape_pressed,
    })
}

#[cfg(windows)]
fn sample_platform_cursor_neighborhood() -> Result<CursorColorNeighborhoodSample, String> {
    sample_windows_cursor_neighborhood()
}

#[cfg(target_os = "macos")]
fn sample_platform_cursor_neighborhood() -> Result<CursorColorNeighborhoodSample, String> {
    sample_macos_cursor_neighborhood()
}

#[cfg(not(any(windows, target_os = "macos")))]
fn sample_platform_cursor_neighborhood() -> Result<CursorColorNeighborhoodSample, String> {
    Err(
        "Live native cursor color sampling is currently supported on Windows and macOS only."
            .to_owned(),
    )
}

#[cfg(windows)]
fn sample_windows_cursor_pixel() -> Result<CursorColorSample, String> {
    use windows_sys::Win32::{
        Foundation::POINT,
        Graphics::Gdi::{GetDC, GetPixel, ReleaseDC, CLR_INVALID},
        UI::WindowsAndMessaging::GetCursorPos,
    };

    let mut point = POINT { x: 0, y: 0 };
    if unsafe { GetCursorPos(&mut point) } == 0 {
        return Err(format!(
            "GetCursorPos failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    let desktop_dc = unsafe { GetDC(std::ptr::null_mut()) };
    if desktop_dc.is_null() {
        return Err(format!("GetDC failed: {}", std::io::Error::last_os_error()));
    }

    let colorref = unsafe { GetPixel(desktop_dc, point.x, point.y) };
    let released = unsafe { ReleaseDC(std::ptr::null_mut(), desktop_dc) };

    if colorref == CLR_INVALID {
        return Err("GetPixel returned CLR_INVALID for the cursor position.".to_owned());
    }
    if released == 0 {
        return Err(format!(
            "ReleaseDC failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    Ok(sample_from_colorref(colorref, point.x, point.y))
}

#[cfg(windows)]
fn sample_windows_cursor_neighborhood() -> Result<CursorColorNeighborhoodSample, String> {
    use windows_sys::Win32::{
        Foundation::POINT,
        Graphics::Gdi::{GetDC, GetPixel, ReleaseDC, CLR_INVALID},
        UI::{
            Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE, VK_LBUTTON, VK_RBUTTON},
            WindowsAndMessaging::{
                GetCursorPos, GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
                SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
            },
        },
    };

    let mut point = POINT { x: 0, y: 0 };
    if unsafe { GetCursorPos(&mut point) } == 0 {
        return Err(format!(
            "GetCursorPos failed: {}",
            std::io::Error::last_os_error()
        ));
    }

    let left = unsafe { GetSystemMetrics(SM_XVIRTUALSCREEN) };
    let top = unsafe { GetSystemMetrics(SM_YVIRTUALSCREEN) };
    let width = unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) };
    let height = unsafe { GetSystemMetrics(SM_CYVIRTUALSCREEN) };
    let right = left
        .checked_add(width)
        .and_then(|value| value.checked_sub(1))
        .filter(|_| width > 0)
        .ok_or_else(|| "Windows reported invalid virtual-screen width.".to_owned())?;
    let bottom = top
        .checked_add(height)
        .and_then(|value| value.checked_sub(1))
        .filter(|_| height > 0)
        .ok_or_else(|| "Windows reported invalid virtual-screen height.".to_owned())?;

    let desktop_dc = unsafe { GetDC(std::ptr::null_mut()) };
    if desktop_dc.is_null() {
        return Err(format!("GetDC failed: {}", std::io::Error::last_os_error()));
    }

    let capture = (|| {
        let center_color = unsafe { GetPixel(desktop_dc, point.x, point.y) };
        if center_color == CLR_INVALID {
            return Err("GetPixel returned CLR_INVALID for the cursor position.".to_owned());
        }
        let mut pixels = Vec::with_capacity(CURSOR_COLOR_NEIGHBORHOOD_PIXELS);
        let radius = (CURSOR_COLOR_NEIGHBORHOOD_EDGE / 2) as i32;
        for row in -radius..=radius {
            for column in -radius..=radius {
                let x = point.x.saturating_add(column).clamp(left, right);
                let y = point.y.saturating_add(row).clamp(top, bottom);
                let sampled = unsafe { GetPixel(desktop_dc, x, y) };
                // Monitor layouts can contain gaps inside the virtual-screen
                // rectangle. Replicating the valid center pixel keeps the
                // fixed 9×9 payload bounded without inventing transparency.
                let color = if sampled == CLR_INVALID {
                    center_color
                } else {
                    sampled
                };
                pixels.push([
                    (color & 0xff) as u8,
                    ((color >> 8) & 0xff) as u8,
                    ((color >> 16) & 0xff) as u8,
                ]);
            }
        }
        Ok(pixels)
    })();

    let released = unsafe { ReleaseDC(std::ptr::null_mut(), desktop_dc) };
    if released == 0 {
        return Err(format!(
            "ReleaseDC failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    let pixels = capture?;
    let pressed = |key: u16| unsafe { GetAsyncKeyState(i32::from(key)) as u16 & 0x8000 != 0 };
    neighborhood_from_rgb(
        pixels,
        point.x,
        point.y,
        PickerInputState {
            left_pressed: pressed(VK_LBUTTON),
            right_pressed: pressed(VK_RBUTTON),
            escape_pressed: pressed(VK_ESCAPE),
        },
    )
}

#[cfg(target_os = "macos")]
fn sample_macos_cursor_pixel() -> Result<CursorColorSample, String> {
    use core_graphics::{
        access::ScreenCaptureAccess,
        base::kCGImageAlphaPremultipliedLast,
        color_space::CGColorSpace,
        context::CGContext,
        display::CGDisplay,
        event::CGEvent,
        event_source::{CGEventSource, CGEventSourceStateID},
        geometry::{CGPoint, CGRect, CGSize},
    };

    // macOS treats a one-pixel capture as screen-content access. This command
    // is invoked only from the user's explicit picker click, never from a
    // background timer or a global mouse listener.
    let screen_access = ScreenCaptureAccess;
    if !screen_access.preflight() && !screen_access.request() {
        return Err(
            "macOS needs Screen Recording permission to sample the pixel under the cursor. Grant it for iHub in System Settings > Privacy & Security > Screen Recording, then try again."
                .to_owned(),
        );
    }

    let event_source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
        .map_err(|_| "macOS could not create a cursor event source.".to_owned())?;
    let event = CGEvent::new(event_source)
        .map_err(|_| "macOS could not read the cursor position.".to_owned())?;
    let cursor = event.location();

    let (displays, matching_display_count) = CGDisplay::displays_with_point(cursor, 1)
        .map_err(|error| format!("macOS could not find the display under the cursor: {error}"))?;
    if matching_display_count == 0 {
        return Err("macOS could not find an active display under the cursor.".to_owned());
    }
    let display = CGDisplay::new(
        *displays
            .first()
            .ok_or_else(|| "macOS returned no display for the cursor position.".to_owned())?,
    );

    // CGEventGetLocation and CGDisplayCreateImageForRect both operate in the
    // global display coordinate space, so this remains correct on Retina and
    // multi-monitor setups without manually guessing a scale factor.
    let source_rect = CGRect::new(&cursor, &CGSize::new(1.0, 1.0));
    let image = display.image_for_rect(source_rect).ok_or_else(|| {
        "macOS could not capture the requested cursor pixel. Check Screen Recording permission."
            .to_owned()
    })?;

    // Render into a known device-RGB, RGBA byte layout instead of assuming the
    // display image's native channel order or color format.
    let color_space = CGColorSpace::create_device_rgb();
    let mut rgba = [0_u8; 4];
    let bitmap_rect = CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(1.0, 1.0));
    let context = CGContext::create_bitmap_context(
        Some(rgba.as_mut_ptr().cast()),
        1,
        1,
        8,
        rgba.len(),
        &color_space,
        kCGImageAlphaPremultipliedLast,
    );
    context.draw_image(bitmap_rect, &image);
    context.flush();

    Ok(sample_from_rgb(
        rgba[0],
        rgba[1],
        rgba[2],
        cursor.x.round().clamp(i32::MIN as f64, i32::MAX as f64) as i32,
        cursor.y.round().clamp(i32::MIN as f64, i32::MAX as f64) as i32,
    ))
}

#[cfg(target_os = "macos")]
fn sample_macos_cursor_neighborhood() -> Result<CursorColorNeighborhoodSample, String> {
    use core_graphics::{
        access::ScreenCaptureAccess,
        base::kCGImageAlphaPremultipliedLast,
        color_space::CGColorSpace,
        context::CGContext,
        display::CGDisplay,
        event::CGEvent,
        event_source::{CGEventSource, CGEventSourceStateID},
        geometry::{CGPoint, CGRect, CGSize},
    };

    let screen_access = ScreenCaptureAccess;
    if !screen_access.preflight() && !screen_access.request() {
        return Err(
            "macOS needs Screen Recording permission for the live color magnifier. Grant it for iHub in System Settings > Privacy & Security > Screen Recording, then try again."
                .to_owned(),
        );
    }

    let event_source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)
        .map_err(|_| "macOS could not create a cursor event source.".to_owned())?;
    let event = CGEvent::new(event_source)
        .map_err(|_| "macOS could not read the cursor position.".to_owned())?;
    let cursor = event.location();
    let (displays, matching_display_count) = CGDisplay::displays_with_point(cursor, 1)
        .map_err(|error| format!("macOS could not find the display under the cursor: {error}"))?;
    if matching_display_count == 0 {
        return Err("macOS could not find an active display under the cursor.".to_owned());
    }
    let display = CGDisplay::new(
        *displays
            .first()
            .ok_or_else(|| "macOS returned no display for the cursor position.".to_owned())?,
    );
    let bounds = display.bounds();
    let edge = CURSOR_COLOR_NEIGHBORHOOD_EDGE as f64;
    if bounds.size.width < edge || bounds.size.height < edge {
        return Err("The display is too small for a 9×9 cursor color sample.".to_owned());
    }
    let radius = edge / 2.0;
    let origin_x =
        (cursor.x - radius).clamp(bounds.origin.x, bounds.origin.x + bounds.size.width - edge);
    let origin_y =
        (cursor.y - radius).clamp(bounds.origin.y, bounds.origin.y + bounds.size.height - edge);
    let source_rect = CGRect::new(&CGPoint::new(origin_x, origin_y), &CGSize::new(edge, edge));
    let image = display.image_for_rect(source_rect).ok_or_else(|| {
        "macOS could not capture the cursor neighborhood. Check Screen Recording permission."
            .to_owned()
    })?;

    let row_bytes = CURSOR_COLOR_NEIGHBORHOOD_EDGE * 4;
    let mut rgba = vec![0_u8; CURSOR_COLOR_NEIGHBORHOOD_PIXELS * 4];
    let color_space = CGColorSpace::create_device_rgb();
    let context = CGContext::create_bitmap_context(
        Some(rgba.as_mut_ptr().cast()),
        CURSOR_COLOR_NEIGHBORHOOD_EDGE,
        CURSOR_COLOR_NEIGHBORHOOD_EDGE,
        8,
        row_bytes,
        &color_space,
        kCGImageAlphaPremultipliedLast,
    );
    let destination = CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(edge, edge));
    context.draw_image(destination, &image);
    context.flush();

    let pixels = rgba
        .chunks_exact(4)
        .map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect();
    let x = cursor.x.round().clamp(i32::MIN as f64, i32::MAX as f64) as i32;
    let y = cursor.y.round().clamp(i32::MIN as f64, i32::MAX as f64) as i32;

    // These CoreGraphics APIs only read current state; iHub never posts or
    // synthesizes an input event. Foreground Escape/right-click handlers in
    // React remain the fallback if macOS privacy policy returns `false`.
    extern "C" {
        fn CGEventSourceButtonState(state_id: i32, button: u32) -> bool;
        fn CGEventSourceKeyState(state_id: i32, key: u16) -> bool;
    }
    let input = unsafe {
        PickerInputState {
            left_pressed: CGEventSourceButtonState(0, 0),
            right_pressed: CGEventSourceButtonState(0, 1),
            // kVK_Escape from HIToolbox/Events.h.
            escape_pressed: CGEventSourceKeyState(0, 0x35),
        }
    };
    neighborhood_from_rgb(pixels, x, y, input)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{
        neighborhood_from_rgb, sample_from_colorref, sample_from_rgb, validate_cursor_color_delay,
        CursorColorSample, PickerInputState, PickerSessionRegistry,
        CURSOR_COLOR_NEIGHBORHOOD_PIXELS, MAX_CURSOR_COLOR_DELAY_MS,
        MAX_CURSOR_COLOR_PICKER_SESSION_MS, MIN_CURSOR_COLOR_SAMPLE_INTERVAL_MS,
    };

    #[test]
    fn colorref_is_decoded_in_windows_bgr_byte_order() {
        assert_eq!(
            sample_from_colorref(0x0056_3412, 12, -8),
            CursorColorSample {
                hex: "#123456".to_owned(),
                rgb: "rgb(18, 52, 86)".to_owned(),
                x: 12,
                y: -8,
            }
        );
    }

    #[test]
    fn rgb_samples_keep_channel_order_and_screen_coordinates() {
        assert_eq!(
            sample_from_rgb(17, 34, 51, -4, 88),
            CursorColorSample {
                hex: "#112233".to_owned(),
                rgb: "rgb(17, 34, 51)".to_owned(),
                x: -4,
                y: 88,
            }
        );
    }

    #[test]
    fn delay_is_bounded_before_any_native_call() {
        assert_eq!(validate_cursor_color_delay(0), Ok(0));
        assert_eq!(
            validate_cursor_color_delay(MAX_CURSOR_COLOR_DELAY_MS),
            Ok(MAX_CURSOR_COLOR_DELAY_MS)
        );
        assert!(validate_cursor_color_delay(MAX_CURSOR_COLOR_DELAY_MS + 1).is_err());
    }

    #[test]
    fn neighborhood_is_always_a_bounded_nine_by_nine_grid_with_center_color() {
        let mut pixels = vec![[1, 2, 3]; CURSOR_COLOR_NEIGHBORHOOD_PIXELS];
        pixels[CURSOR_COLOR_NEIGHBORHOOD_PIXELS / 2] = [0x12, 0xab, 0x34];
        let sample = neighborhood_from_rgb(
            pixels,
            -1920,
            880,
            PickerInputState {
                left_pressed: true,
                right_pressed: false,
                escape_pressed: false,
            },
        )
        .expect("a fixed 9x9 grid should serialize");
        assert_eq!(sample.sample_edge, 9);
        assert_eq!(sample.pixels.len(), 81);
        assert_eq!(sample.pixels[40], "#12AB34");
        assert_eq!(sample.hex, "#12AB34");
        assert!(sample.left_pressed);
        assert!(neighborhood_from_rgb(
            vec![[0, 0, 0]; CURSOR_COLOR_NEIGHBORHOOD_PIXELS - 1],
            0,
            0,
            PickerInputState::default()
        )
        .is_err());
    }

    #[test]
    fn live_picker_sessions_expire_rate_limit_and_require_the_exact_token() {
        let now = Instant::now();
        let mut sessions = PickerSessionRegistry::default();
        let issued = sessions
            .begin("session-a".to_owned(), now)
            .expect("first session");
        assert_eq!(issued.sample_edge, 9);
        assert_eq!(
            issued.minimum_interval_ms,
            MIN_CURSOR_COLOR_SAMPLE_INTERVAL_MS
        );
        assert!(sessions.begin("session-b".to_owned(), now).is_err());
        assert!(sessions.reserve_sample("wrong", now).is_err());
        sessions
            .reserve_sample("session-a", now)
            .expect("first sample is immediate");
        assert!(sessions.reserve_sample("session-a", now).is_err());
        sessions
            .reserve_sample(
                "session-a",
                now + Duration::from_millis(MIN_CURSOR_COLOR_SAMPLE_INTERVAL_MS),
            )
            .expect("the documented interval is allowed");
        assert!(sessions
            .reserve_sample(
                "session-a",
                now + Duration::from_millis(MAX_CURSOR_COLOR_PICKER_SESSION_MS + 1),
            )
            .is_err());
        sessions
            .begin(
                "session-b".to_owned(),
                now + Duration::from_millis(MAX_CURSOR_COLOR_PICKER_SESSION_MS + 1),
            )
            .expect("an expired session does not block a new explicit picker");
        sessions.end("wrong");
        assert!(sessions.active.is_some());
        sessions.end("session-b");
        assert!(sessions.active.is_none());
    }
}
