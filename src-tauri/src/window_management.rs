use serde::Serialize;
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize};

const MIN_WINDOW_WIDTH: u32 = 560;
const MIN_WINDOW_HEIGHT: u32 = 420;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowManagementAction {
    Center,
    SnapLeft,
    SnapRight,
    ToggleAlwaysOnTop,
}

impl WindowManagementAction {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "center" => Ok(Self::Center),
            "snap-left" => Ok(Self::SnapLeft),
            "snap-right" => Ok(Self::SnapRight),
            "toggle-always-on-top" => Ok(Self::ToggleAlwaysOnTop),
            _ => Err(
                "Unsupported window action. Use center, snap-left, snap-right, or toggle-always-on-top."
                    .to_owned(),
            ),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Center => "center",
            Self::SnapLeft => "snap-left",
            Self::SnapRight => "snap-right",
            Self::ToggleAlwaysOnTop => "toggle-always-on-top",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WindowManagementResult {
    pub action: String,
    pub always_on_top: bool,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PhysicalRect {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

impl PhysicalRect {
    fn fitted_size(self, desired: PhysicalSize<u32>) -> PhysicalSize<u32> {
        PhysicalSize::new(
            desired
                .width
                .clamp(MIN_WINDOW_WIDTH.min(self.width), self.width),
            desired
                .height
                .clamp(MIN_WINDOW_HEIGHT.min(self.height), self.height),
        )
    }

    fn centered_position(self, size: PhysicalSize<u32>) -> PhysicalPosition<i32> {
        PhysicalPosition::new(
            self.x + ((self.width.saturating_sub(size.width) / 2) as i32),
            self.y + ((self.height.saturating_sub(size.height) / 2) as i32),
        )
    }
}

/// Performs one of the small, deliberately bounded layout actions on iHub's
/// own launcher window. This bridge never enumerates, reads, or changes any
/// other application's windows; that keeps the permission meaningful on both
/// Windows and macOS without requiring Accessibility privileges.
pub(crate) fn manage_launcher_window(
    app: &AppHandle,
    requested_action: &str,
) -> Result<WindowManagementResult, String> {
    let action = WindowManagementAction::parse(requested_action)?;
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| "The iHub launcher window is unavailable.".to_owned())?;

    let monitor = window
        .current_monitor()
        .map_err(|error| format!("Could not find the current monitor: {error}"))?
        .or_else(|| window.primary_monitor().ok().flatten())
        .ok_or_else(|| "No display is available for the iHub launcher.".to_owned())?;
    let work_area = monitor.work_area();
    let bounds = PhysicalRect {
        x: work_area.position.x,
        y: work_area.position.y,
        width: work_area.size.width,
        height: work_area.size.height,
    };

    if bounds.width == 0 || bounds.height == 0 {
        return Err("The current display has no usable work area.".to_owned());
    }

    let current_size = window
        .outer_size()
        .map_err(|error| format!("Could not read the launcher size: {error}"))?;
    let mut result_size = bounds.fitted_size(current_size);
    let mut result_position = bounds.centered_position(result_size);
    let always_on_top = match action {
        WindowManagementAction::Center => {
            window
                .set_size(result_size)
                .map_err(|error| format!("Could not resize the launcher: {error}"))?;
            window
                .set_position(result_position)
                .map_err(|error| format!("Could not center the launcher: {error}"))?;
            window
                .is_always_on_top()
                .map_err(|error| format!("Could not read launcher pin state: {error}"))?
        }
        WindowManagementAction::SnapLeft | WindowManagementAction::SnapRight => {
            let half_width = (bounds.width / 2).max(1);
            result_size = bounds.fitted_size(PhysicalSize::new(half_width, bounds.height));
            result_position = PhysicalPosition::new(
                if action == WindowManagementAction::SnapLeft {
                    bounds.x
                } else {
                    bounds.x + bounds.width.saturating_sub(result_size.width) as i32
                },
                bounds.y,
            );
            window
                .set_size(result_size)
                .map_err(|error| format!("Could not resize the launcher: {error}"))?;
            window
                .set_position(result_position)
                .map_err(|error| format!("Could not snap the launcher: {error}"))?;
            window
                .is_always_on_top()
                .map_err(|error| format!("Could not read launcher pin state: {error}"))?
        }
        WindowManagementAction::ToggleAlwaysOnTop => {
            let next = !window
                .is_always_on_top()
                .map_err(|error| format!("Could not read launcher pin state: {error}"))?;
            window
                .set_always_on_top(next)
                .map_err(|error| format!("Could not update launcher pin state: {error}"))?;
            next
        }
    };

    window
        .set_focus()
        .map_err(|error| format!("Could not focus the launcher: {error}"))?;

    Ok(WindowManagementResult {
        action: action.as_str().to_owned(),
        always_on_top,
        x: result_position.x,
        y: result_position.y,
        width: result_size.width,
        height: result_size.height,
    })
}

#[cfg(test)]
mod tests {
    use super::{PhysicalRect, WindowManagementAction};
    use tauri::PhysicalSize;

    #[test]
    fn accepts_only_the_bounded_launcher_actions() {
        assert_eq!(
            WindowManagementAction::parse("snap-left").expect("known action"),
            WindowManagementAction::SnapLeft
        );
        assert!(WindowManagementAction::parse("move-to-123,456").is_err());
        assert!(WindowManagementAction::parse("enumerate-windows").is_err());
    }

    #[test]
    fn fitting_never_escapes_the_monitor_work_area() {
        let bounds = PhysicalRect {
            x: -1600,
            y: 24,
            width: 800,
            height: 600,
        };
        assert_eq!(
            bounds.fitted_size(PhysicalSize::new(1_400, 900)),
            PhysicalSize::new(800, 600)
        );
        assert_eq!(
            bounds.fitted_size(PhysicalSize::new(10, 10)),
            PhysicalSize::new(560, 420)
        );
    }

    #[test]
    fn centering_uses_the_work_area_including_negative_monitors() {
        let bounds = PhysicalRect {
            x: -1600,
            y: 24,
            width: 800,
            height: 600,
        };
        let position = bounds.centered_position(PhysicalSize::new(560, 420));
        assert_eq!((position.x, position.y), (-1480, 114));
    }
}
