use serde::Serialize;

pub const MAX_CURSOR_COLOR_DELAY_MS: u64 = 5_000;

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

#[cfg(test)]
mod tests {
    use super::{
        sample_from_colorref, sample_from_rgb, validate_cursor_color_delay, CursorColorSample,
        MAX_CURSOR_COLOR_DELAY_MS,
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
}
