use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ScreenPoint {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ScreenSize {
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ScreenRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UtoolsDisplay {
    pub accelerometer_support: &'static str,
    pub bounds: ScreenRect,
    pub color_depth: u32,
    pub color_space: &'static str,
    pub depth_per_component: u32,
    pub detected: bool,
    pub display_frequency: u32,
    pub id: i64,
    pub internal: bool,
    pub label: String,
    pub maximum_cursor_size: ScreenSize,
    pub native_origin: ScreenPoint,
    pub rotation: u32,
    pub scale_factor: f64,
    pub touch_support: &'static str,
    pub monochrome: bool,
    pub size: ScreenSize,
    pub work_area: ScreenRect,
    pub work_area_size: ScreenSize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UtoolsDisplayMetric {
    pub id: i64,
    pub physical_bounds: ScreenRect,
    pub dip_bounds: ScreenRect,
    pub scale_factor: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UtoolsScreenSnapshot {
    pub displays: Vec<UtoolsDisplay>,
    pub metrics: Vec<UtoolsDisplayMetric>,
    pub primary_display_id: i64,
    pub cursor_screen_point: ScreenPoint,
}

fn scale_coordinate(value: i32, scale_factor: f64) -> i32 {
    (f64::from(value) / scale_factor).round() as i32
}

fn scale_dimension(value: u32, scale_factor: f64) -> u32 {
    (f64::from(value) / scale_factor).round().max(1.0) as u32
}

fn dip_rect(physical: ScreenRect, scale_factor: f64) -> ScreenRect {
    ScreenRect {
        x: scale_coordinate(physical.x, scale_factor),
        y: scale_coordinate(physical.y, scale_factor),
        width: scale_dimension(physical.width, scale_factor),
        height: scale_dimension(physical.height, scale_factor),
    }
}

fn point_distance_squared(point: ScreenPoint, rect: ScreenRect) -> i128 {
    let left = i64::from(rect.x);
    let top = i64::from(rect.y);
    let right = left + i64::from(rect.width);
    let bottom = top + i64::from(rect.height);
    let x = i64::from(point.x);
    let y = i64::from(point.y);
    let dx = if x < left {
        left - x
    } else if x >= right {
        x - right + 1
    } else {
        0
    };
    let dy = if y < top {
        top - y
    } else if y >= bottom {
        y - bottom + 1
    } else {
        0
    };
    i128::from(dx) * i128::from(dx) + i128::from(dy) * i128::from(dy)
}

fn physical_to_dip(point: ScreenPoint, metric: &UtoolsDisplayMetric) -> ScreenPoint {
    ScreenPoint {
        x: metric.dip_bounds.x
            + (f64::from(point.x - metric.physical_bounds.x) / metric.scale_factor).round() as i32,
        y: metric.dip_bounds.y
            + (f64::from(point.y - metric.physical_bounds.y) / metric.scale_factor).round() as i32,
    }
}

#[cfg(windows)]
pub fn screen_snapshot() -> Result<UtoolsScreenSnapshot, String> {
    use std::mem;

    use windows_sys::Win32::{
        Foundation::{LPARAM, POINT},
        Graphics::Gdi::{
            EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW, DEVMODEW, DMDO_180,
            DMDO_270, DMDO_90, ENUM_CURRENT_SETTINGS, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
        },
        UI::{
            HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
            WindowsAndMessaging::{
                GetCursorPos, GetSystemMetrics, MONITORINFOF_PRIMARY, SM_CXCURSOR, SM_CYCURSOR,
            },
        },
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
    if unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(collect_monitor),
            (&mut monitors as *mut Vec<HMONITOR>) as LPARAM,
        )
    } == 0
    {
        return Err(format!(
            "Windows could not enumerate displays: {}",
            std::io::Error::last_os_error()
        ));
    }
    if monitors.is_empty() || monitors.len() > 32 {
        return Err("Windows returned an invalid number of active displays.".to_owned());
    }

    let cursor_size = ScreenSize {
        width: u32::try_from(unsafe { GetSystemMetrics(SM_CXCURSOR) }.max(1)).unwrap_or(32),
        height: u32::try_from(unsafe { GetSystemMetrics(SM_CYCURSOR) }.max(1)).unwrap_or(32),
    };
    let mut displays = Vec::with_capacity(monitors.len());
    let mut metrics = Vec::with_capacity(monitors.len());
    let mut primary_display_id = None;

    for monitor in monitors {
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = mem::size_of::<MONITORINFOEXW>() as u32;
        if unsafe {
            GetMonitorInfoW(
                monitor,
                (&mut info as *mut MONITORINFOEXW).cast::<MONITORINFO>(),
            )
        } == 0
        {
            return Err(format!(
                "Windows could not inspect an active display: {}",
                std::io::Error::last_os_error()
            ));
        }
        let label_length = info
            .szDevice
            .iter()
            .position(|value| *value == 0)
            .unwrap_or(info.szDevice.len());
        let label = String::from_utf16_lossy(&info.szDevice[..label_length]);
        let id = stable_display_id(&label);
        let mut display_mode = DEVMODEW {
            dmSize: mem::size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        let has_display_mode = unsafe {
            EnumDisplaySettingsW(
                info.szDevice.as_ptr(),
                ENUM_CURRENT_SETTINGS,
                &mut display_mode,
            )
        } != 0;
        let (color_depth, display_frequency, rotation) = if has_display_mode {
            let orientation = unsafe { display_mode.Anonymous1.Anonymous2.dmDisplayOrientation };
            let rotation = match orientation {
                DMDO_90 => 90,
                DMDO_180 => 180,
                DMDO_270 => 270,
                _ => 0,
            };
            (
                display_mode.dmBitsPerPel.max(1),
                display_mode.dmDisplayFrequency,
                rotation,
            )
        } else {
            (32, 0, 0)
        };
        let physical_bounds = rect_from_windows(info.monitorInfo.rcMonitor)?;
        let physical_work_area = rect_from_windows(info.monitorInfo.rcWork)?;
        let mut dpi_x = 96_u32;
        let mut dpi_y = 96_u32;
        if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) } < 0 {
            dpi_x = 96;
            dpi_y = 96;
        }
        let scale_factor = (f64::from(dpi_x.max(dpi_y)) / 96.0).clamp(0.5, 8.0);
        let bounds = dip_rect(physical_bounds, scale_factor);
        let work_area = dip_rect(physical_work_area, scale_factor);
        let primary = info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0;
        if primary {
            primary_display_id = Some(id);
        }
        displays.push(UtoolsDisplay {
            accelerometer_support: "unavailable",
            bounds,
            color_depth,
            color_space: "srgb",
            depth_per_component: 8,
            detected: true,
            display_frequency,
            id,
            internal: false,
            label: if label.is_empty() {
                format!("Display {id}")
            } else {
                label
            },
            maximum_cursor_size: cursor_size,
            native_origin: ScreenPoint {
                x: physical_bounds.x,
                y: physical_bounds.y,
            },
            rotation,
            scale_factor,
            touch_support: "unknown",
            monochrome: false,
            size: ScreenSize {
                width: bounds.width,
                height: bounds.height,
            },
            work_area,
            work_area_size: ScreenSize {
                width: work_area.width,
                height: work_area.height,
            },
        });
        metrics.push(UtoolsDisplayMetric {
            id,
            physical_bounds,
            dip_bounds: bounds,
            scale_factor,
        });
    }

    let primary_display_id = primary_display_id.unwrap_or(displays[0].id);
    let mut cursor = POINT::default();
    if unsafe { GetCursorPos(&mut cursor) } == 0 {
        return Err(format!(
            "Windows could not read the cursor position: {}",
            std::io::Error::last_os_error()
        ));
    }
    let physical_cursor = ScreenPoint {
        x: cursor.x,
        y: cursor.y,
    };
    let metric = metrics
        .iter()
        .min_by_key(|metric| point_distance_squared(physical_cursor, metric.physical_bounds))
        .ok_or_else(|| "Windows returned no usable display metrics.".to_owned())?;
    let cursor_screen_point = physical_to_dip(physical_cursor, metric);
    Ok(UtoolsScreenSnapshot {
        displays,
        metrics,
        primary_display_id,
        cursor_screen_point,
    })
}

#[cfg(windows)]
fn rect_from_windows(rect: windows_sys::Win32::Foundation::RECT) -> Result<ScreenRect, String> {
    let width = rect
        .right
        .checked_sub(rect.left)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| "Windows returned invalid display width.".to_owned())?;
    let height = rect
        .bottom
        .checked_sub(rect.top)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| "Windows returned invalid display height.".to_owned())?;
    Ok(ScreenRect {
        x: rect.left,
        y: rect.top,
        width,
        height,
    })
}

fn stable_display_id(label: &str) -> i64 {
    let mut hash = 2_166_136_261_u32;
    for byte in label.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    i64::from((hash & 0x7fff_ffff).max(1))
}

#[cfg(not(windows))]
pub fn screen_snapshot() -> Result<UtoolsScreenSnapshot, String> {
    Err(
        "uTools display and cursor compatibility is currently available on Windows only."
            .to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        dip_rect, physical_to_dip, point_distance_squared, stable_display_id, ScreenPoint,
        ScreenRect, UtoolsDisplayMetric,
    };

    #[test]
    fn display_ids_are_stable_positive_and_label_scoped() {
        assert_eq!(
            stable_display_id(r"\\.\DISPLAY1"),
            stable_display_id(r"\\.\DISPLAY1")
        );
        assert_ne!(
            stable_display_id(r"\\.\DISPLAY1"),
            stable_display_id(r"\\.\DISPLAY2")
        );
        assert!(stable_display_id("") > 0);
    }

    #[test]
    fn dip_projection_preserves_monitor_relative_coordinates() {
        let physical = ScreenRect {
            x: -1920,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let dip = dip_rect(physical, 1.5);
        let metric = UtoolsDisplayMetric {
            id: 1,
            physical_bounds: physical,
            dip_bounds: dip,
            scale_factor: 1.5,
        };
        assert_eq!(dip.x, -1280);
        assert_eq!(dip.width, 1280);
        assert_eq!(
            physical_to_dip(ScreenPoint { x: -960, y: 540 }, &metric),
            ScreenPoint { x: -640, y: 360 }
        );
    }

    #[test]
    fn distance_is_zero_inside_and_increases_outside() {
        let rect = ScreenRect {
            x: 10,
            y: 20,
            width: 100,
            height: 80,
        };
        assert_eq!(
            point_distance_squared(ScreenPoint { x: 10, y: 20 }, rect),
            0
        );
        assert!(point_distance_squared(ScreenPoint { x: -100, y: 20 }, rect) > 0);
    }
}
