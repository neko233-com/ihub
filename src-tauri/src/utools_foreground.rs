//! Bounded Windows projection of the application that owned the foreground
//! immediately before iHub revealed its launcher.
//!
//! uTools exposes the current Explorer folder and browser URL while its own
//! window is foreground. iHub therefore records only the prior top-level HWND
//! at the trusted native reveal boundary, then revalidates that the same HWND
//! still belongs to the same process before any later read. No keyboard input,
//! clipboard mutation, window enumeration, or browser extension is involved.

#[cfg(windows)]
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ForegroundWindowTarget {
    hwnd: isize,
    process_id: u32,
}

#[cfg(windows)]
pub(crate) fn capture_external_foreground_window() -> Option<ForegroundWindowTarget> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowThreadProcessId, IsWindow,
    };

    // SAFETY: these calls inspect one current top-level window and write one
    // process identifier into live stack storage.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() || IsWindow(hwnd) == 0 {
            return None;
        }
        let mut process_id = 0_u32;
        let _ = GetWindowThreadProcessId(hwnd, &mut process_id);
        if process_id == 0 || process_id == std::process::id() {
            return None;
        }
        Some(ForegroundWindowTarget {
            hwnd: hwnd as isize,
            process_id,
        })
    }
}

#[cfg(not(windows))]
pub(crate) fn capture_external_foreground_window() -> Option<ForegroundWindowTarget> {
    None
}

#[cfg(windows)]
fn validate_live_target(target: ForegroundWindowTarget) -> Result<(), String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, IsWindow};

    let hwnd = target.hwnd as *mut core::ffi::c_void;
    // SAFETY: the stored integer is used only as an opaque HWND and is checked
    // with IsWindow before the process identifier is read.
    unsafe {
        if hwnd.is_null() || IsWindow(hwnd) == 0 {
            return Err("The window that preceded iHub is no longer available.".to_owned());
        }
        let mut process_id = 0_u32;
        let _ = GetWindowThreadProcessId(hwnd, &mut process_id);
        if process_id == 0 || process_id != target.process_id {
            return Err("The window that preceded iHub has changed ownership.".to_owned());
        }
    }
    Ok(())
}

#[cfg(windows)]
fn process_image_name(process_id: u32) -> Result<String, String> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    };

    // SAFETY: the returned process handle is checked and always closed. The
    // buffer length is passed exactly as allocated.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id);
        if process.is_null() {
            return Err("Windows denied access to the preceding window process.".to_owned());
        }
        struct ProcessHandle(windows_sys::Win32::Foundation::HANDLE);
        impl Drop for ProcessHandle {
            fn drop(&mut self) {
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
        let process = ProcessHandle(process);
        let mut buffer = vec![0_u16; 32_768];
        let mut length = buffer.len() as u32;
        if QueryFullProcessImageNameW(process.0, 0, buffer.as_mut_ptr(), &mut length) == 0
            || length == 0
            || length as usize > buffer.len()
        {
            return Err("Windows could not identify the preceding window process.".to_owned());
        }
        let path = PathBuf::from(String::from_utf16_lossy(&buffer[..length as usize]));
        path.file_name()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .ok_or_else(|| "The preceding window process name is unavailable.".to_owned())
    }
}

#[cfg(windows)]
struct ComApartment;

#[cfg(windows)]
impl ComApartment {
    fn initialize() -> Result<Self, String> {
        use windows::Win32::System::Com::{
            CoInitializeEx, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
        };
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE)
                .ok()
                .map_err(|error| format!("Windows COM initialization failed: {error}"))?;
        }
        Ok(Self)
    }
}

#[cfg(windows)]
impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe {
            windows::Win32::System::Com::CoUninitialize();
        }
    }
}

#[cfg(windows)]
pub(crate) fn read_folder_path(target: ForegroundWindowTarget) -> Result<String, String> {
    use windows::{
        core::Interface,
        Win32::{
            System::{
                Com::{CoCreateInstance, CLSCTX_LOCAL_SERVER},
                Variant::VARIANT,
            },
            UI::Shell::{IShellWindows, IWebBrowserApp, ShellWindows},
        },
    };

    validate_live_target(target)?;
    if process_image_name(target.process_id)? != "explorer.exe" {
        return Err("The window that preceded iHub is not Windows File Explorer.".to_owned());
    }
    let _apartment = ComApartment::initialize()?;
    let windows: IShellWindows = unsafe {
        CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER)
            .map_err(|error| format!("Windows Explorer discovery failed: {error}"))?
    };
    let count = unsafe { windows.Count() }
        .map_err(|error| format!("Windows Explorer window count failed: {error}"))?
        .clamp(0, 256);
    for index in 0..count {
        let item = unsafe { windows.Item(&VARIANT::from(index)) };
        let Ok(item) = item else {
            continue;
        };
        let Ok(browser) = item.cast::<IWebBrowserApp>() else {
            continue;
        };
        let Ok(hwnd) = (unsafe { browser.HWND() }) else {
            continue;
        };
        if hwnd.0 != target.hwnd {
            continue;
        }
        let location = unsafe { browser.LocationURL() }
            .map_err(|error| format!("Windows Explorer location read failed: {error}"))?
            .to_string();
        let url = url::Url::parse(&location)
            .map_err(|_| "Windows Explorer did not expose a filesystem location.".to_owned())?;
        if url.scheme() != "file" {
            return Err("The active Explorer view is not a local filesystem folder.".to_owned());
        }
        let path = url
            .to_file_path()
            .map_err(|_| "Windows Explorer returned an invalid local folder URL.".to_owned())?;
        let prepared = crate::system_open::prepare_local_open(
            &path,
            Some(crate::system_open::LocalOpenKind::Folder),
        )?;
        let display = path.to_string_lossy().into_owned();
        if display.is_empty() || display.len() > 8192 || prepared.path().as_os_str().is_empty() {
            return Err("Windows Explorer returned an invalid folder path.".to_owned());
        }
        return Ok(display);
    }
    Err("The preceding Explorer window no longer has a readable folder view.".to_owned())
}

#[cfg(not(windows))]
pub(crate) fn read_folder_path(_target: ForegroundWindowTarget) -> Result<String, String> {
    Err("readCurrentFolderPath has been runtime-verified on Windows only.".to_owned())
}

#[cfg(windows)]
fn supported_browser_process(name: &str) -> bool {
    matches!(
        name,
        "chrome.exe"
            | "msedge.exe"
            | "firefox.exe"
            | "iexplore.exe"
            | "opera.exe"
            | "brave.exe"
            | "vivaldi.exe"
    )
}

#[cfg(windows)]
fn address_element_marker(automation_id: &str, name: &str, class_name: &str) -> bool {
    let automation_id = automation_id.to_ascii_lowercase();
    let name = name.to_ascii_lowercase();
    let class_name = class_name.to_ascii_lowercase();
    automation_id.contains("address")
        || automation_id.contains("urlbar")
        || automation_id.contains("omnibox")
        || name.contains("address")
        || name.contains("url")
        || name.contains("地址")
        || name.contains("网址")
        || class_name.contains("omnibox")
}

#[cfg(windows)]
fn validate_browser_url(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 8192
        || value.chars().any(char::is_control)
        || url::Url::parse(value).is_err()
    {
        return None;
    }
    Some(value.to_owned())
}

#[cfg(windows)]
pub(crate) fn read_browser_url(target: ForegroundWindowTarget) -> Result<String, String> {
    use windows::Win32::{
        Foundation::HWND,
        System::{
            Com::{CoCreateInstance, CLSCTX_INPROC_SERVER},
            Variant::VARIANT,
        },
        UI::Accessibility::{
            CUIAutomation, IUIAutomation, IUIAutomationValuePattern, TreeScope_Descendants,
            UIA_ControlTypePropertyId, UIA_EditControlTypeId, UIA_ValuePatternId,
        },
    };

    validate_live_target(target)?;
    let process_name = process_image_name(target.process_id)?;
    if !supported_browser_process(&process_name) {
        return Err("The window that preceded iHub is not a supported browser.".to_owned());
    }
    let _apartment = ComApartment::initialize()?;
    let automation: IUIAutomation = unsafe {
        CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
            .map_err(|error| format!("Windows UI Automation initialization failed: {error}"))?
    };
    let root = unsafe { automation.ElementFromHandle(HWND(target.hwnd as *mut core::ffi::c_void)) }
        .map_err(|error| format!("The browser accessibility root is unavailable: {error}"))?;
    let condition = unsafe {
        automation.CreatePropertyCondition(
            UIA_ControlTypePropertyId,
            &VARIANT::from(UIA_EditControlTypeId.0),
        )
    }
    .map_err(|error| format!("The browser address-field query failed: {error}"))?;
    let elements = unsafe { root.FindAll(TreeScope_Descendants, &condition) }
        .map_err(|error| format!("The browser address-field search failed: {error}"))?;
    let length = unsafe { elements.Length() }
        .map_err(|error| format!("The browser address-field result failed: {error}"))?
        .clamp(0, 128);
    for index in 0..length {
        let Ok(element) = (unsafe { elements.GetElement(index) }) else {
            continue;
        };
        if unsafe { element.CurrentIsPassword() }
            .ok()
            .is_some_and(|value| value.as_bool())
        {
            continue;
        }
        let automation_id = unsafe { element.CurrentAutomationId() }
            .map(|value| value.to_string())
            .unwrap_or_default();
        let name = unsafe { element.CurrentName() }
            .map(|value| value.to_string())
            .unwrap_or_default();
        let class_name = unsafe { element.CurrentClassName() }
            .map(|value| value.to_string())
            .unwrap_or_default();
        if !address_element_marker(&automation_id, &name, &class_name) {
            continue;
        }
        let Ok(pattern) = (unsafe {
            element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
        }) else {
            continue;
        };
        let Ok(value) = (unsafe { pattern.CurrentValue() }) else {
            continue;
        };
        if let Some(value) = validate_browser_url(&value.to_string()) {
            return Ok(value);
        }
    }
    Err(
        "The supported browser did not expose its address field through Windows accessibility."
            .to_owned(),
    )
}

#[cfg(not(windows))]
pub(crate) fn read_browser_url(_target: ForegroundWindowTarget) -> Result<String, String> {
    Err("readCurrentBrowserUrl has been runtime-verified on Windows only.".to_owned())
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::{
        address_element_marker, capture_external_foreground_window, read_browser_url,
        read_folder_path, supported_browser_process, validate_browser_url,
    };

    #[cfg(windows)]
    #[test]
    fn browser_allowlist_and_address_markers_are_explicit() {
        for browser in [
            "chrome.exe",
            "msedge.exe",
            "firefox.exe",
            "iexplore.exe",
            "opera.exe",
            "brave.exe",
            "vivaldi.exe",
        ] {
            assert!(supported_browser_process(browser));
        }
        assert!(!supported_browser_process("notepad.exe"));
        assert!(address_element_marker("urlbar-input", "", ""));
        assert!(address_element_marker("", "Address and search bar", ""));
        assert!(address_element_marker("", "地址和搜索栏", ""));
        assert!(!address_element_marker("search", "Page search", "Edit"));
    }

    #[cfg(windows)]
    #[test]
    fn browser_url_projection_is_bounded_and_requires_a_url() {
        assert_eq!(
            validate_browser_url(" https://example.com/path?q=1 ").as_deref(),
            Some("https://example.com/path?q=1")
        );
        assert_eq!(
            validate_browser_url("chrome://settings").as_deref(),
            Some("chrome://settings")
        );
        assert!(validate_browser_url("search terms").is_none());
        assert!(validate_browser_url("https://example.com/\nsecret").is_none());
        assert!(
            validate_browser_url(&format!("https://example.com/{}", "a".repeat(8192))).is_none()
        );
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "reads the real Explorer window that is foreground when the test starts"]
    fn reads_current_real_explorer_folder() {
        let expected = std::env::var("IHUB_EXPECTED_FOREGROUND_FOLDER")
            .expect("set IHUB_EXPECTED_FOREGROUND_FOLDER for the manual acceptance test");
        let target = capture_external_foreground_window().expect("external foreground window");
        let actual = read_folder_path(target).expect("current Explorer folder");
        assert!(crate::indexer::paths_refer_to_same_location(
            std::path::Path::new(&actual),
            std::path::Path::new(&expected)
        ));
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "reads the real supported browser window that is foreground when the test starts"]
    fn reads_current_real_browser_url() {
        let target = capture_external_foreground_window().expect("external foreground window");
        let actual = read_browser_url(target).expect("current browser URL");
        assert!(validate_browser_url(&actual).is_some());
        if let Ok(expected) = std::env::var("IHUB_EXPECTED_FOREGROUND_URL") {
            assert!(
                actual.trim_end_matches('/') == expected.trim_end_matches('/'),
                "the current browser URL did not match the expected acceptance URL"
            );
        }
    }
}
