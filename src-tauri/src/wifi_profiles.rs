//! Explicit Windows Native Wi-Fi profile inspection. Profile lists never
//! contain keys. A single key is requested only after a direct user action;
//! non-elevated hosts use the same iHub executable as a short-lived UAC helper
//! and return the secret over a PID-verified, local named pipe.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

const MAX_WIFI_INTERFACES: usize = 32;
const MAX_WIFI_PROFILES: usize = 512;
const MAX_PROFILE_XML_U16: usize = 256 * 1024;
const MAX_SECRET_RESPONSE_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WifiProfileView {
    id: String,
    name: String,
    interface_name: String,
    authentication: String,
    encryption: String,
    can_reveal: bool,
    group_policy: bool,
}

#[derive(Serialize, Zeroize, ZeroizeOnDrop)]
#[serde(rename_all = "camelCase")]
pub struct WifiPasswordReveal {
    #[zeroize(skip)]
    profile_id: String,
    #[zeroize(skip)]
    profile_name: String,
    password: String,
}

#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(rename_all = "camelCase")]
struct SecretPipeResponse {
    #[zeroize(skip)]
    profile_id: String,
    #[zeroize(skip)]
    profile_name: String,
    password: String,
    #[zeroize(skip)]
    error: Option<String>,
}

#[cfg(windows)]
#[derive(Clone)]
struct ProfileIdentity {
    interface_guid: windows::core::GUID,
    interface_name: String,
    name: String,
    flags: u32,
}

#[tauri::command]
pub fn list_wifi_profiles() -> Result<Vec<WifiProfileView>, String> {
    #[cfg(windows)]
    {
        windows_list_profiles()
    }
    #[cfg(not(windows))]
    {
        Err("Wi-Fi 密码查看目前只在 Windows 10/11 上提供。".to_owned())
    }
}

#[tauri::command]
pub fn reveal_wifi_password(profile_id: String) -> Result<WifiPasswordReveal, String> {
    validate_profile_id(&profile_id)?;
    #[cfg(windows)]
    {
        if crate::hosts_manager::process_is_elevated() {
            return reveal_profile_native(&profile_id);
        }
        reveal_via_elevated_pipe(&profile_id)
    }
    #[cfg(not(windows))]
    {
        let _ = profile_id;
        Err("Wi-Fi 密码查看目前只在 Windows 10/11 上提供。".to_owned())
    }
}

fn validate_profile_id(value: &str) -> Result<(), String> {
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Wi-Fi 配置 ID 无效，请刷新列表后重试。".to_owned());
    }
    Ok(())
}

#[cfg(windows)]
fn windows_list_profiles() -> Result<Vec<WifiProfileView>, String> {
    let handle = WlanClient::open()?;
    let identities = enumerate_profile_identities(&handle)?;
    identities
        .into_iter()
        .map(|profile| {
            let xml = get_profile_xml(&handle, &profile, false)
                .ok()
                .map(Zeroizing::new);
            let authentication = xml
                .as_deref()
                .and_then(|value| xml_value(value, "authentication"))
                .unwrap_or_else(|| "未知".to_owned());
            let encryption = xml
                .as_deref()
                .and_then(|value| xml_value(value, "encryption"))
                .unwrap_or_else(|| "未知".to_owned());
            let can_reveal = xml
                .as_ref()
                .and_then(|value| shared_key_section(value.as_str()))
                .and_then(|value| xml_value(value, "keyMaterial"))
                .is_some();
            Ok(WifiProfileView {
                id: profile_id(&profile),
                name: profile.name,
                interface_name: profile.interface_name,
                authentication,
                encryption,
                can_reveal,
                group_policy: profile.flags
                    & windows::Win32::NetworkManagement::WiFi::WLAN_PROFILE_GROUP_POLICY
                    != 0,
            })
        })
        .collect()
}

#[cfg(windows)]
fn reveal_profile_native(profile_id_value: &str) -> Result<WifiPasswordReveal, String> {
    let handle = WlanClient::open()?;
    let profiles = enumerate_profile_identities(&handle)?;
    let profile = profiles
        .into_iter()
        .find(|profile| profile_id(profile) == profile_id_value)
        .ok_or_else(|| "该 Wi-Fi 配置已不存在，请刷新列表。".to_owned())?;
    let xml = Zeroizing::new(get_profile_xml(&handle, &profile, true)?);
    let shared = shared_key_section(&xml)
        .ok_or_else(|| "该配置没有预共享密钥；企业 802.1X 凭据不在此工具范围内。".to_owned())?;
    if xml_value(shared, "protected").as_deref() != Some("false") {
        return Err("Windows 策略未授予明文 Wi-Fi 密钥权限；iHub 不会绕过系统 DACL。".to_owned());
    }
    let password = xml_value(shared, "keyMaterial")
        .filter(|value| !value.is_empty() && value.len() <= 4_096)
        .ok_or_else(|| {
            "该配置没有可显示的预共享密钥；企业 802.1X 凭据不在此工具范围内。".to_owned()
        })?;
    Ok(WifiPasswordReveal {
        profile_id: profile_id_value.to_owned(),
        profile_name: profile.name,
        password,
    })
}

#[cfg(windows)]
struct WlanClient(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl WlanClient {
    fn open() -> Result<Self, String> {
        use windows::Win32::{Foundation::HANDLE, NetworkManagement::WiFi::WlanOpenHandle};
        let mut negotiated = 0_u32;
        let mut handle = HANDLE::default();
        let code = unsafe { WlanOpenHandle(2, None, &mut negotiated, &mut handle) };
        if code != 0 {
            return Err(wlan_error("无法打开 Windows Native Wi-Fi 服务", code));
        }
        if negotiated < 2 || handle.is_invalid() {
            return Err("Windows Native Wi-Fi 服务未返回受支持的会话。".to_owned());
        }
        Ok(Self(handle))
    }
}

#[cfg(windows)]
impl Drop for WlanClient {
    fn drop(&mut self) {
        unsafe { windows::Win32::NetworkManagement::WiFi::WlanCloseHandle(self.0, None) };
    }
}

#[cfg(windows)]
struct WlanMemory(*mut core::ffi::c_void);

#[cfg(windows)]
impl Drop for WlanMemory {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { windows::Win32::NetworkManagement::WiFi::WlanFreeMemory(self.0) };
        }
    }
}

#[cfg(windows)]
fn enumerate_profile_identities(client: &WlanClient) -> Result<Vec<ProfileIdentity>, String> {
    use std::slice;
    use windows::Win32::NetworkManagement::WiFi::{
        WlanEnumInterfaces, WlanGetProfileList, WLAN_INTERFACE_INFO_LIST, WLAN_PROFILE_INFO_LIST,
    };
    let mut interfaces = std::ptr::null_mut::<WLAN_INTERFACE_INFO_LIST>();
    let code = unsafe { WlanEnumInterfaces(client.0, None, &mut interfaces) };
    if code != 0 {
        return Err(wlan_error("无法枚举 Wi-Fi 适配器", code));
    }
    let _interfaces_memory = WlanMemory(interfaces.cast());
    if interfaces.is_null() {
        return Err("Windows Native Wi-Fi 服务返回了空适配器列表。".to_owned());
    }
    let interface_count = unsafe { (*interfaces).dwNumberOfItems as usize };
    if interface_count > MAX_WIFI_INTERFACES {
        return Err("Wi-Fi 适配器数量超过安全上限。".to_owned());
    }
    let interface_slice =
        unsafe { slice::from_raw_parts((*interfaces).InterfaceInfo.as_ptr(), interface_count) };
    let mut output = Vec::new();
    for interface in interface_slice {
        let mut profiles = std::ptr::null_mut::<WLAN_PROFILE_INFO_LIST>();
        let code =
            unsafe { WlanGetProfileList(client.0, &interface.InterfaceGuid, None, &mut profiles) };
        if code != 0 {
            continue;
        }
        let _profiles_memory = WlanMemory(profiles.cast());
        if profiles.is_null() {
            continue;
        }
        let count = unsafe { (*profiles).dwNumberOfItems as usize };
        if output.len().saturating_add(count) > MAX_WIFI_PROFILES {
            return Err("Wi-Fi 配置数量超过安全上限。".to_owned());
        }
        let profile_slice =
            unsafe { slice::from_raw_parts((*profiles).ProfileInfo.as_ptr(), count) };
        let interface_name = utf16_array(&interface.strInterfaceDescription);
        for profile in profile_slice {
            let name = utf16_array(&profile.strProfileName);
            if !name.is_empty() {
                output.push(ProfileIdentity {
                    interface_guid: interface.InterfaceGuid,
                    interface_name: interface_name.clone(),
                    name,
                    flags: profile.dwFlags,
                });
            }
        }
    }
    Ok(output)
}

#[cfg(windows)]
fn get_profile_xml(
    client: &WlanClient,
    profile: &ProfileIdentity,
    plaintext: bool,
) -> Result<String, String> {
    use windows::{
        core::{PCWSTR, PWSTR},
        Win32::NetworkManagement::WiFi::{WlanGetProfile, WLAN_PROFILE_GET_PLAINTEXT_KEY},
    };
    let name = profile
        .name
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut xml_pointer = PWSTR::null();
    let mut flags = if plaintext {
        WLAN_PROFILE_GET_PLAINTEXT_KEY
    } else {
        0
    };
    let mut access = 0_u32;
    let code = unsafe {
        WlanGetProfile(
            client.0,
            &profile.interface_guid,
            PCWSTR(name.as_ptr()),
            None,
            &mut xml_pointer,
            Some(&mut flags),
            Some(&mut access),
        )
    };
    if code != 0 {
        return Err(wlan_error("无法读取 Wi-Fi 配置", code));
    }
    let _xml_memory = WlanMemory(xml_pointer.0.cast());
    if xml_pointer.is_null() {
        return Err("Windows Native Wi-Fi 服务返回了空配置。".to_owned());
    }
    let mut length = 0_usize;
    while length < MAX_PROFILE_XML_U16 && unsafe { *xml_pointer.0.add(length) } != 0 {
        length += 1;
    }
    if length == MAX_PROFILE_XML_U16 {
        return Err("Wi-Fi 配置 XML 超过安全上限。".to_owned());
    }
    let units = unsafe { std::slice::from_raw_parts(xml_pointer.0, length) };
    let decoded =
        String::from_utf16(units).map_err(|_| "Wi-Fi 配置 XML 不是有效 UTF-16。".to_owned());
    for index in 0..length {
        unsafe { xml_pointer.0.add(index).write_volatile(0) };
    }
    decoded
}

#[cfg(windows)]
fn profile_id(profile: &ProfileIdentity) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"ihub-wifi-profile-v1\0");
    hasher.update(profile.interface_guid.to_u128().to_be_bytes());
    for unit in profile.name.encode_utf16() {
        hasher.update(unit.to_le_bytes());
    }
    let digest = hasher.finalize();
    digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(windows)]
fn utf16_array(value: &[u16]) -> String {
    let length = value
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(value.len());
    String::from_utf16_lossy(&value[..length])
}

fn xml_value(xml: &str, tag: &str) -> Option<String> {
    let opening = format!("<{tag}>");
    let closing = format!("</{tag}>");
    let start = xml.find(&opening)? + opening.len();
    let end = xml[start..].find(&closing)? + start;
    xml_unescape(&xml[start..end])
}

fn shared_key_section(xml: &str) -> Option<&str> {
    let start = xml.find("<sharedKey>")?;
    let end = xml[start..].find("</sharedKey>")? + start + "</sharedKey>".len();
    Some(&xml[start..end])
}

fn xml_unescape(value: &str) -> Option<String> {
    let mut output = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(index) = rest.find('&') {
        output.push_str(&rest[..index]);
        rest = &rest[index + 1..];
        let end = rest.find(';')?;
        let entity = &rest[..end];
        let character = match entity {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            value if value.starts_with("#x") => {
                char::from_u32(u32::from_str_radix(&value[2..], 16).ok()?)?
            }
            value if value.starts_with('#') => char::from_u32(value[1..].parse().ok()?)?,
            _ => return None,
        };
        if character == '\0' {
            return None;
        }
        output.push(character);
        rest = &rest[end + 1..];
    }
    output.push_str(rest);
    Some(output)
}

#[cfg(windows)]
fn wlan_error(context: &str, code: u32) -> String {
    format!(
        "{context}：{}（{code}）",
        std::io::Error::from_raw_os_error(code as i32)
    )
}

#[cfg(windows)]
fn reveal_via_elevated_pipe(profile_id: &str) -> Result<WifiPasswordReveal, String> {
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::{
                ERROR_NO_DATA, ERROR_PIPE_CONNECTED, ERROR_PIPE_LISTENING, WAIT_OBJECT_0,
                WAIT_TIMEOUT,
            },
            Storage::FileSystem::{ReadFile, FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND},
            System::{
                Pipes::{
                    ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_NOWAIT,
                    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
                },
                Threading::{GetProcessId, WaitForSingleObject},
            },
        },
    };
    let pipe_name = format!(r"\\.\pipe\ihub-wifi-{}", uuid::Uuid::new_v4().simple());
    let pipe_wide = wide(&pipe_name);
    let pipe = unsafe {
        CreateNamedPipeW(
            PCWSTR(pipe_wide.as_ptr()),
            PIPE_ACCESS_INBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            0,
            MAX_SECRET_RESPONSE_BYTES as u32,
            30_000,
            None,
        )
    };
    if pipe.is_invalid() {
        return Err("无法创建本机 Wi-Fi 密钥响应通道。".to_owned());
    }
    let _pipe_guard = HandleGuard(pipe);
    let process = launch_wifi_helper(profile_id, &pipe_name)?;
    let _process_guard = HandleGuard(process);
    let expected_pid = unsafe { GetProcessId(process) };
    if expected_pid == 0 {
        return Err("无法识别 Wi-Fi 管理员辅助程序。".to_owned());
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        match unsafe { ConnectNamedPipe(pipe, None) } {
            Ok(()) => break,
            Err(error)
                if error.code() == windows::core::HRESULT::from_win32(ERROR_PIPE_CONNECTED.0) =>
            {
                break;
            }
            Err(error)
                if error.code() == windows::core::HRESULT::from_win32(ERROR_PIPE_LISTENING.0) =>
            {
                if unsafe { WaitForSingleObject(process, 0) } == WAIT_OBJECT_0 {
                    return Err("Wi-Fi 管理员辅助程序未能连接安全响应通道。".to_owned());
                }
                if std::time::Instant::now() >= deadline {
                    return Err("等待 Wi-Fi 密钥响应通道超时。".to_owned());
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(error) => {
                return Err(format!("无法连接 Wi-Fi 密钥响应通道：{error}"));
            }
        }
    }
    let mut client_pid = 0_u32;
    unsafe { GetNamedPipeClientProcessId(pipe, &mut client_pid) }
        .map_err(|error| format!("无法验证 Wi-Fi 辅助程序身份：{error}"))?;
    if client_pid != expected_pid {
        return Err("Wi-Fi 密钥响应通道被非预期进程占用；已拒绝读取。".to_owned());
    }
    let mut buffer = Zeroizing::new(vec![0_u8; MAX_SECRET_RESPONSE_BYTES]);
    let mut read = 0_u32;
    loop {
        match unsafe { ReadFile(pipe, Some(&mut buffer), Some(&mut read), None) } {
            Ok(()) => break,
            Err(error) if error.code() == windows::core::HRESULT::from_win32(ERROR_NO_DATA.0) => {
                if unsafe { WaitForSingleObject(process, 0) } == WAIT_OBJECT_0 {
                    return Err("Wi-Fi 管理员辅助程序退出时没有返回密钥响应。".to_owned());
                }
                if std::time::Instant::now() >= deadline {
                    return Err("读取 Wi-Fi 密钥响应超时。".to_owned());
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            Err(error) => return Err(format!("无法读取 Wi-Fi 密钥响应：{error}")),
        }
    }
    if read == 0 || read as usize >= MAX_SECRET_RESPONSE_BYTES {
        return Err("Wi-Fi 密钥响应为空或超过上限。".to_owned());
    }
    buffer.truncate(read as usize);
    let wait = unsafe { WaitForSingleObject(process, 30_000) };
    if wait == WAIT_TIMEOUT {
        return Err("Wi-Fi 管理员辅助程序等待超时。".to_owned());
    }
    if wait != WAIT_OBJECT_0 {
        return Err("无法等待 Wi-Fi 管理员辅助程序。".to_owned());
    }
    let response: SecretPipeResponse =
        serde_json::from_slice(&buffer).map_err(|_| "Wi-Fi 密钥响应格式无效。".to_owned())?;
    if response.profile_id != profile_id {
        return Err("Wi-Fi 密钥响应与所选配置不匹配。".to_owned());
    }
    if let Some(error) = response.error.as_deref() {
        return Err(error.to_owned());
    }
    if response.password.is_empty() {
        return Err("Wi-Fi 密钥响应没有密码。".to_owned());
    }
    let reveal = WifiPasswordReveal {
        profile_id: response.profile_id.clone(),
        profile_name: response.profile_name.clone(),
        password: response.password.clone(),
    };
    Ok(reveal)
}

#[cfg(windows)]
struct HandleGuard(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for HandleGuard {
    fn drop(&mut self) {
        unsafe { windows::Win32::Foundation::CloseHandle(self.0) }.ok();
    }
}

#[cfg(windows)]
fn launch_wifi_helper(
    profile_id: &str,
    pipe_name: &str,
) -> Result<windows::Win32::Foundation::HANDLE, String> {
    use std::mem::size_of;
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::ERROR_CANCELLED,
            UI::{
                Shell::{
                    ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC,
                    SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
                },
                WindowsAndMessaging::SW_HIDE,
            },
        },
    };
    let executable =
        std::env::current_exe().map_err(|error| format!("无法定位 iHub 可执行文件：{error}"))?;
    let verb = wide("runas");
    let executable = wide(&executable.to_string_lossy());
    let parameters = wide(&format!(
        "--ihub-wifi-reveal {profile_id} {}",
        quote_windows_argument(pipe_name)
    ));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(executable.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }.map_err(|error| {
        if error.code() == windows::core::HRESULT::from_win32(ERROR_CANCELLED.0) {
            "已取消 Windows 管理员授权；未读取 Wi-Fi 密码。".to_owned()
        } else {
            format!("无法启动 Wi-Fi 管理员辅助程序：{error}")
        }
    })?;
    if info.hProcess.is_invalid() {
        return Err("Windows 未返回 Wi-Fi 辅助程序句柄。".to_owned());
    }
    Ok(info.hProcess)
}

#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

#[cfg(windows)]
fn quote_windows_argument(value: &str) -> String {
    let mut result = String::from("\"");
    let mut slashes = 0;
    for character in value.chars() {
        if character == '\\' {
            slashes += 1;
            continue;
        }
        if character == '"' {
            result.push_str(&"\\".repeat(slashes * 2 + 1));
            result.push('"');
        } else {
            result.push_str(&"\\".repeat(slashes));
            result.push(character);
        }
        slashes = 0;
    }
    result.push_str(&"\\".repeat(slashes * 2));
    result.push('"');
    result
}

pub fn elevated_helper_exit_code_from_args() -> Option<i32> {
    let mut args = std::env::args_os();
    let _ = args.next();
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--ihub-wifi-reveal")) {
        return None;
    }
    let Some(profile_id) = args.next().and_then(|value| value.into_string().ok()) else {
        return Some(30);
    };
    let Some(pipe_name) = args.next().and_then(|value| value.into_string().ok()) else {
        return Some(30);
    };
    if args.next().is_some()
        || validate_profile_id(&profile_id).is_err()
        || !valid_pipe_name(&pipe_name)
    {
        return Some(30);
    }
    #[cfg(windows)]
    {
        Some(match helper_write_secret(&profile_id, &pipe_name) {
            Ok(()) => 0,
            Err(()) => 31,
        })
    }
    #[cfg(not(windows))]
    {
        Some(30)
    }
}

fn valid_pipe_name(value: &str) -> bool {
    let Some(id) = value.strip_prefix(r"\\.\pipe\ihub-wifi-") else {
        return false;
    };
    id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(windows)]
fn helper_write_secret(profile_id: &str, pipe_name: &str) -> Result<(), ()> {
    use windows::{
        core::PCWSTR,
        Win32::Storage::FileSystem::{
            CreateFileW, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE, FILE_SHARE_NONE,
            OPEN_EXISTING,
        },
    };
    let pipe = unsafe {
        CreateFileW(
            PCWSTR(wide(pipe_name).as_ptr()),
            FILE_GENERIC_WRITE.0,
            FILE_SHARE_NONE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(|_| ())?;
    let _guard = HandleGuard(pipe);
    let mut response = match reveal_profile_native(profile_id) {
        Ok(reveal) => SecretPipeResponse {
            profile_id: reveal.profile_id.clone(),
            profile_name: reveal.profile_name.clone(),
            password: reveal.password.clone(),
            error: None,
        },
        Err(error) => SecretPipeResponse {
            profile_id: profile_id.to_owned(),
            profile_name: String::new(),
            password: String::new(),
            error: Some(error),
        },
    };
    let bytes = Zeroizing::new(serde_json::to_vec(&response).map_err(|_| ())?);
    if bytes.is_empty() || bytes.len() >= MAX_SECRET_RESPONSE_BYTES {
        return Err(());
    }
    let expected_length = bytes.len();
    let mut written = 0_u32;
    let result = unsafe { WriteFile(pipe, Some(&bytes), Some(&mut written), None) };
    response.zeroize();
    result.map_err(|_| ())?;
    if written as usize != expected_length {
        return Err(());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_xml_entities_and_numeric_characters() {
        assert_eq!(
            xml_unescape("a&amp;b&lt;c&#x21;&#33;"),
            Some("a&b<c!!".to_owned())
        );
        assert_eq!(
            xml_value("<keyMaterial>a&amp;b</keyMaterial>", "keyMaterial"),
            Some("a&b".to_owned())
        );
        assert!(xml_unescape("&unknown;").is_none());
    }

    #[test]
    fn accepts_only_opaque_profile_ids_and_local_random_pipe_names() {
        assert!(validate_profile_id(&"a".repeat(32)).is_ok());
        assert!(validate_profile_id("wifi-name").is_err());
        assert!(valid_pipe_name(&format!(
            r"\\.\pipe\ihub-wifi-{}",
            "b".repeat(32)
        )));
        assert!(!valid_pipe_name(
            r"\\server\pipe\ihub-wifi-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ));
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "enumerates real Windows Wi-Fi profile metadata without requesting keys"]
    fn enumerates_current_wifi_profiles_without_secrets() {
        let profiles =
            list_wifi_profiles().expect("Native Wi-Fi profile metadata should enumerate");
        assert!(profiles.len() <= MAX_WIFI_PROFILES);
        assert!(profiles
            .iter()
            .all(|profile| validate_profile_id(&profile.id).is_ok()));
    }
}
