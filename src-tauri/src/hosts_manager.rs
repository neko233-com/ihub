//! Windows hosts-file management with bounded parsing and an explicit UAC
//! helper. The renderer edits only an iHub-owned block and never supplies a
//! filesystem path. Existing comments and non-iHub lines remain byte-for-byte
//! unchanged.

use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    net::IpAddr,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const MAX_HOSTS_BYTES: usize = 1024 * 1024;
const MAX_MANAGED_ENTRIES: usize = 256;
const MAX_DOMAINS_PER_ENTRY: usize = 8;
const MAX_COMMENT_CHARS: usize = 160;
const MAX_HELPER_REQUEST_BYTES: u64 = 2 * 1024 * 1024;
const HELPER_REQUEST_TTL_MS: u64 = 5 * 60 * 1000;
const START_MARKER: &str = "# >>> iHub managed hosts >>>";
const END_MARKER: &str = "# <<< iHub managed hosts <<<";
const DISABLED_PREFIX: &str = "# ihub-disabled ";

#[derive(Debug, Default)]
pub struct HostsManagerState {
    applying: AtomicBool,
    request_file: Mutex<Option<File>>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HostsEntryView {
    id: String,
    ip: String,
    domains: Vec<String>,
    comment: Option<String>,
    enabled: bool,
    managed: bool,
    line_number: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HostsSnapshot {
    fingerprint: String,
    entries: Vec<HostsEntryView>,
    managed_entries: Vec<HostsEntryView>,
    size_bytes: usize,
    line_ending: String,
    can_write_directly: bool,
    backup_available: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostsManagedEntryInput {
    ip: String,
    domains: Vec<String>,
    comment: Option<String>,
    enabled: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HostsApplyResult {
    snapshot: HostsSnapshot,
    elevated: bool,
    backup_created: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HelperRequest {
    version: u8,
    request_id: String,
    expires_at_epoch_ms: u64,
    expected_fingerprint: String,
    action: HelperAction,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum HelperAction {
    Apply { content_base64: String },
    RestoreBackup,
}

struct ApplyLease<'a>(&'a AtomicBool);

impl Drop for ApplyLease<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub fn get_hosts_snapshot() -> Result<HostsSnapshot, String> {
    snapshot_from_disk()
}

pub fn apply_hosts_entries(
    state: &HostsManagerState,
    expected_fingerprint: String,
    entries: Vec<HostsManagedEntryInput>,
) -> Result<HostsApplyResult, String> {
    let _lease = begin_apply(state)?;
    validate_fingerprint(&expected_fingerprint)?;
    let normalized = validate_entries(entries)?;
    let current = read_hosts_bytes()?;
    ensure_fingerprint(&current, &expected_fingerprint)?;
    reject_external_conflicts(&current, &normalized)?;
    let desired = replace_managed_block(&current, &normalized)?;
    validate_hosts_payload(&desired)?;
    let elevated = apply_with_privilege(
        state,
        &expected_fingerprint,
        HelperAction::Apply {
            content_base64: BASE64.encode(&desired),
        },
    )?;
    Ok(HostsApplyResult {
        snapshot: snapshot_from_disk()?,
        elevated,
        backup_created: hosts_backup_path()?.is_file(),
    })
}

pub fn restore_hosts_backup(
    state: &HostsManagerState,
    expected_fingerprint: String,
) -> Result<HostsApplyResult, String> {
    let _lease = begin_apply(state)?;
    validate_fingerprint(&expected_fingerprint)?;
    let current = read_hosts_bytes()?;
    ensure_fingerprint(&current, &expected_fingerprint)?;
    if !hosts_backup_path()?.is_file() {
        return Err("没有可恢复的 iHub hosts 备份。".to_owned());
    }
    let elevated = apply_with_privilege(state, &expected_fingerprint, HelperAction::RestoreBackup)?;
    Ok(HostsApplyResult {
        snapshot: snapshot_from_disk()?,
        elevated,
        backup_created: hosts_backup_path()?.is_file(),
    })
}

fn begin_apply(state: &HostsManagerState) -> Result<ApplyLease<'_>, String> {
    state
        .applying
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| "已有 hosts 写入请求正在等待完成。".to_owned())?;
    Ok(ApplyLease(&state.applying))
}

fn snapshot_from_disk() -> Result<HostsSnapshot, String> {
    let bytes = read_hosts_bytes()?;
    let (entries, managed_entries) = parse_entries(&bytes)?;
    Ok(HostsSnapshot {
        fingerprint: fingerprint(&bytes),
        entries,
        managed_entries,
        size_bytes: bytes.len(),
        line_ending: if bytes.windows(2).any(|pair| pair == b"\r\n") {
            "CRLF".to_owned()
        } else {
            "LF".to_owned()
        },
        can_write_directly: can_write_hosts_directly(),
        backup_available: hosts_backup_path()?.is_file(),
    })
}

fn read_hosts_bytes() -> Result<Vec<u8>, String> {
    read_bounded_file(&hosts_path()?, "hosts 文件")
}

fn read_bounded_file(path: &Path, label: &str) -> Result<Vec<u8>, String> {
    let metadata = fs::metadata(path).map_err(|error| format!("无法读取{label}：{error}"))?;
    if !metadata.is_file() || metadata.len() > MAX_HOSTS_BYTES as u64 {
        return Err(format!("{label}必须是且只能是 1 MiB 以内的普通文件。"));
    }
    let file = File::open(path).map_err(|error| format!("无法打开{label}：{error}"))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_HOSTS_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("无法读取{label}：{error}"))?;
    validate_hosts_payload(&bytes)?;
    Ok(bytes)
}

fn validate_hosts_payload(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_HOSTS_BYTES {
        return Err("hosts 文件不能超过 1 MiB。".to_owned());
    }
    if bytes.contains(&0) {
        return Err("hosts 文件包含不支持的 NUL 字节。".to_owned());
    }
    Ok(())
}

fn validate_entries(
    entries: Vec<HostsManagedEntryInput>,
) -> Result<Vec<HostsManagedEntryInput>, String> {
    if entries.len() > MAX_MANAGED_ENTRIES {
        return Err(format!(
            "iHub 最多管理 {MAX_MANAGED_ENTRIES} 条 hosts 映射。"
        ));
    }
    let mut domains = HashSet::new();
    entries
        .into_iter()
        .map(|entry| {
            let ip = entry
                .ip
                .trim()
                .parse::<IpAddr>()
                .map_err(|_| format!("“{}”不是有效的 IPv4 或 IPv6 地址。", entry.ip))?
                .to_string();
            if entry.domains.is_empty() || entry.domains.len() > MAX_DOMAINS_PER_ENTRY {
                return Err(format!(
                    "每条映射必须包含 1–{MAX_DOMAINS_PER_ENTRY} 个域名。"
                ));
            }
            let normalized_domains = entry
                .domains
                .into_iter()
                .map(|domain| normalize_domain(&domain))
                .collect::<Result<Vec<_>, _>>()?;
            for domain in &normalized_domains {
                if !domains.insert(domain.clone()) {
                    return Err(format!("域名“{domain}”在 iHub 管理区中重复。"));
                }
            }
            let comment = entry
                .comment
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty());
            if comment.as_ref().is_some_and(|value| {
                value.chars().count() > MAX_COMMENT_CHARS
                    || value.chars().any(char::is_control)
                    || value.contains('#')
            }) {
                return Err(format!(
                    "备注最多 {MAX_COMMENT_CHARS} 个字符，且不能包含控制字符或 #。"
                ));
            }
            Ok(HostsManagedEntryInput {
                ip,
                domains: normalized_domains,
                comment,
                enabled: entry.enabled,
            })
        })
        .collect()
}

fn normalize_domain(value: &str) -> Result<String, String> {
    let domain = value.trim().trim_end_matches('.').to_ascii_lowercase();
    if domain.is_empty() || domain.len() > 253 || domain.contains('*') {
        return Err(format!("“{value}”不是有效的 hosts 域名；不支持通配符。"));
    }
    for label in domain.split('.') {
        if label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(format!("“{value}”不是有效的 ASCII hosts 域名。"));
        }
    }
    Ok(domain)
}

fn replace_managed_block(
    bytes: &[u8],
    entries: &[HostsManagedEntryInput],
) -> Result<Vec<u8>, String> {
    let range = managed_block_range(bytes)?;
    let newline = if bytes.windows(2).any(|pair| pair == b"\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut block = String::new();
    block.push_str(START_MARKER);
    block.push_str(newline);
    block.push_str(
        "# Managed by iHub. Edit these entries in iHub so validation and backup remain active.",
    );
    block.push_str(newline);
    for entry in entries {
        if !entry.enabled {
            block.push_str(DISABLED_PREFIX);
        }
        block.push_str(&entry.ip);
        block.push(' ');
        block.push_str(&entry.domains.join(" "));
        if let Some(comment) = &entry.comment {
            block.push_str(" # ");
            block.push_str(comment);
        }
        block.push_str(newline);
    }
    block.push_str(END_MARKER);
    block.push_str(newline);

    let mut output = Vec::with_capacity(bytes.len() + block.len());
    match range {
        Some((start, end)) => {
            output.extend_from_slice(&bytes[..start]);
            output.extend_from_slice(block.as_bytes());
            output.extend_from_slice(&bytes[end..]);
        }
        None => {
            output.extend_from_slice(bytes);
            if !output.is_empty() && !output.ends_with(b"\n") {
                output.extend_from_slice(newline.as_bytes());
            }
            output.extend_from_slice(block.as_bytes());
        }
    }
    Ok(output)
}

fn managed_block_range(bytes: &[u8]) -> Result<Option<(usize, usize)>, String> {
    let mut offset = 0_usize;
    let mut start = None;
    let mut end = None;
    for line_with_ending in bytes.split_inclusive(|byte| *byte == b'\n') {
        let mut trimmed = line_with_ending;
        if let Some(without_newline) = trimmed.strip_suffix(b"\n") {
            trimmed = without_newline;
        }
        if let Some(without_carriage_return) = trimmed.strip_suffix(b"\r") {
            trimmed = without_carriage_return;
        }
        if trimmed == START_MARKER.as_bytes() {
            if start.replace(offset).is_some() {
                return Err("hosts 文件包含重复的 iHub 管理区起始标记。".to_owned());
            }
        } else if trimmed == END_MARKER.as_bytes()
            && end.replace(offset + line_with_ending.len()).is_some()
        {
            return Err("hosts 文件包含重复的 iHub 管理区结束标记。".to_owned());
        }
        offset += line_with_ending.len();
    }
    match (start, end) {
        (None, None) => Ok(None),
        (Some(start), Some(end)) if start < end => Ok(Some((start, end))),
        _ => Err(
            "hosts 文件中的 iHub 管理区标记不完整或顺序错误；为避免损坏，已拒绝写入。".to_owned(),
        ),
    }
}

fn parse_entries(bytes: &[u8]) -> Result<(Vec<HostsEntryView>, Vec<HostsEntryView>), String> {
    managed_block_range(bytes)?;
    let text = String::from_utf8_lossy(bytes);
    let mut inside = false;
    let mut all = Vec::new();
    let mut managed = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let trimmed = raw.trim();
        if trimmed == START_MARKER {
            inside = true;
            continue;
        }
        if trimmed == END_MARKER {
            inside = false;
            continue;
        }
        let (candidate, enabled) = if inside && trimmed.starts_with(DISABLED_PREFIX) {
            (&trimmed[DISABLED_PREFIX.len()..], false)
        } else if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        } else {
            (trimmed, true)
        };
        let (mapping, comment) = candidate
            .split_once('#')
            .map_or((candidate, None), |(left, right)| {
                (left, Some(right.trim().to_owned()))
            });
        let mut fields = mapping.split_whitespace();
        let Some(ip) = fields.next() else { continue };
        if ip.parse::<IpAddr>().is_err() {
            continue;
        }
        let domains = fields.map(str::to_owned).collect::<Vec<_>>();
        if domains.is_empty() {
            continue;
        }
        let view = HostsEntryView {
            id: format!(
                "{}-{}",
                if inside { "managed" } else { "external" },
                index + 1
            ),
            ip: ip.to_owned(),
            domains,
            comment: comment.filter(|value| !value.is_empty()),
            enabled,
            managed: inside,
            line_number: index + 1,
        };
        if inside {
            managed.push(view.clone());
        }
        if all.len() < 2_000 {
            all.push(view);
        }
    }
    if managed.len() > MAX_MANAGED_ENTRIES {
        return Err(format!(
            "iHub 管理区包含超过 {MAX_MANAGED_ENTRIES} 条映射；请先手动修复该区块。"
        ));
    }
    Ok((all, managed))
}

fn reject_external_conflicts(
    bytes: &[u8],
    managed: &[HostsManagedEntryInput],
) -> Result<(), String> {
    managed_block_range(bytes)?;
    let text = String::from_utf8_lossy(bytes);
    let mut inside = false;
    let mut external = HashSet::new();
    for raw in text.lines() {
        let trimmed = raw.trim();
        if trimmed == START_MARKER {
            inside = true;
            continue;
        }
        if trimmed == END_MARKER {
            inside = false;
            continue;
        }
        if inside || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let mapping = trimmed.split_once('#').map_or(trimmed, |(left, _)| left);
        let mut fields = mapping.split_whitespace();
        if fields
            .next()
            .and_then(|ip| ip.parse::<IpAddr>().ok())
            .is_none()
        {
            continue;
        }
        external.extend(fields.map(|domain| domain.to_ascii_lowercase()));
    }
    for entry in managed.iter().filter(|entry| entry.enabled) {
        if let Some(domain) = entry
            .domains
            .iter()
            .find(|domain| external.contains(*domain))
        {
            return Err(format!(
                "域名“{domain}”已存在于 iHub 管理区之外；请先手动处理原行，避免顺序歧义。"
            ));
        }
    }
    Ok(())
}

fn fingerprint(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn validate_fingerprint(value: &str) -> Result<(), String> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("hosts 快照指纹无效，请刷新后重试。".to_owned());
    }
    Ok(())
}

fn ensure_fingerprint(bytes: &[u8], expected: &str) -> Result<(), String> {
    if fingerprint(bytes) != expected.to_ascii_lowercase() {
        return Err("hosts 文件已被其他程序修改；iHub 未覆盖新内容，请刷新后重新预览。".to_owned());
    }
    Ok(())
}

fn epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(windows)]
fn windows_root() -> Result<PathBuf, String> {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| "无法解析 Windows 系统目录。".to_owned())
}

#[cfg(not(windows))]
fn windows_root() -> Result<PathBuf, String> {
    Err("hosts 管理目前只在 Windows 10/11 上提供。".to_owned())
}

fn hosts_path() -> Result<PathBuf, String> {
    Ok(windows_root()?
        .join("System32")
        .join("drivers")
        .join("etc")
        .join("hosts"))
}

fn hosts_backup_path() -> Result<PathBuf, String> {
    Ok(hosts_path()?.with_file_name("hosts.ihub-backup"))
}

#[cfg(windows)]
fn can_write_hosts_directly() -> bool {
    use std::{ffi::c_void, mem::size_of};
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return false;
    }
    let mut elevation = TOKEN_ELEVATION::default();
    let mut returned = 0_u32;
    let elevated = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut TOKEN_ELEVATION as *mut c_void,
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        ) != 0
            && returned as usize == size_of::<TOKEN_ELEVATION>()
            && elevation.TokenIsElevated != 0
    };
    unsafe { CloseHandle(token) };
    elevated
}

#[cfg(not(windows))]
fn can_write_hosts_directly() -> bool {
    false
}

fn apply_with_privilege(
    state: &HostsManagerState,
    expected_fingerprint: &str,
    action: HelperAction,
) -> Result<bool, String> {
    let request = HelperRequest {
        version: 1,
        request_id: Uuid::new_v4().to_string(),
        expires_at_epoch_ms: epoch_millis().saturating_add(HELPER_REQUEST_TTL_MS),
        expected_fingerprint: expected_fingerprint.to_owned(),
        action,
    };
    if can_write_hosts_directly() {
        execute_helper_request(request).map_err(|failure| match failure {
            HelperFailure::Stale => "hosts 文件已发生变化；iHub 未覆盖它。".to_owned(),
            HelperFailure::Backup => "hosts 备份不存在或无法读取。".to_owned(),
            HelperFailure::Other => "无法原子写入 hosts 文件；原文件保持不变。".to_owned(),
        })?;
        return Ok(false);
    }
    launch_elevated_helper(state, request)?;
    Ok(true)
}

#[cfg(windows)]
fn launch_elevated_helper(state: &HostsManagerState, request: HelperRequest) -> Result<(), String> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;

    let request_dir = std::env::temp_dir().join("iHub-hosts-actions");
    fs::create_dir_all(&request_dir)
        .map_err(|error| format!("无法创建 hosts 请求目录：{error}"))?;
    let request_path = request_dir.join(format!("{}.json", request.request_id));
    let encoded =
        serde_json::to_vec(&request).map_err(|error| format!("无法编码 hosts 请求：{error}"))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(FILE_SHARE_READ.0)
        .open(&request_path)
        .map_err(|error| format!("无法创建 hosts 请求：{error}"))?;
    file.write_all(&encoded)
        .map_err(|error| format!("无法写入 hosts 请求：{error}"))?;
    file.sync_all()
        .map_err(|error| format!("无法同步 hosts 请求：{error}"))?;
    *state
        .request_file
        .lock()
        .unwrap_or_else(|value| value.into_inner()) = Some(file);
    let result = windows_launch_runas(&request_path);
    state
        .request_file
        .lock()
        .unwrap_or_else(|value| value.into_inner())
        .take();
    let _ = fs::remove_file(&request_path);
    result
}

#[cfg(not(windows))]
fn launch_elevated_helper(
    _state: &HostsManagerState,
    _request: HelperRequest,
) -> Result<(), String> {
    Err("hosts 管理目前只在 Windows 10/11 上提供。".to_owned())
}

#[cfg(windows)]
fn windows_launch_runas(request_path: &Path) -> Result<(), String> {
    use std::mem::size_of;
    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::{CloseHandle, ERROR_CANCELLED, WAIT_OBJECT_0, WAIT_TIMEOUT},
            System::Threading::{GetExitCodeProcess, WaitForSingleObject},
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
        "--ihub-hosts-apply {}",
        quote_windows_argument(&request_path.to_string_lossy())
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
            "已取消 Windows 管理员授权；hosts 未更改。".to_owned()
        } else {
            format!("无法启动 hosts 管理员辅助程序：{error}")
        }
    })?;
    let process = info.hProcess;
    if process.is_invalid() {
        return Err("Windows 未返回 hosts 辅助程序句柄。".to_owned());
    }
    let wait = unsafe { WaitForSingleObject(process, 120_000) };
    if wait == WAIT_TIMEOUT {
        unsafe { CloseHandle(process) }.ok();
        return Err("hosts 管理员辅助程序等待超时；请刷新文件确认状态。".to_owned());
    }
    if wait != WAIT_OBJECT_0 {
        unsafe { CloseHandle(process) }.ok();
        return Err("无法等待 hosts 管理员辅助程序。".to_owned());
    }
    let mut code = 1_u32;
    let read_code = unsafe { GetExitCodeProcess(process, &mut code) };
    unsafe { CloseHandle(process) }.ok();
    read_code.map_err(|error| format!("无法读取 hosts 辅助程序结果：{error}"))?;
    match code {
        0 => Ok(()),
        20 => Err("hosts 文件已在 UAC 确认期间发生变化；iHub 未覆盖它。".to_owned()),
        21 => Err("hosts 备份不存在或无法读取。".to_owned()),
        _ => Err("hosts 管理员辅助程序未能完成写入；原文件保持不变。".to_owned()),
    }
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
    let mut arguments = std::env::args_os();
    let _executable = arguments.next();
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("--ihub-hosts-apply")) {
        return None;
    }
    let Some(path) = arguments.next().map(PathBuf::from) else {
        return Some(10);
    };
    if arguments.next().is_some() {
        return Some(10);
    }
    Some(
        match read_helper_request(&path).and_then(execute_helper_request) {
            Ok(()) => 0,
            Err(HelperFailure::Stale) => 20,
            Err(HelperFailure::Backup) => 21,
            Err(HelperFailure::Other) => 22,
        },
    )
}

enum HelperFailure {
    Stale,
    Backup,
    Other,
}

fn read_helper_request(path: &Path) -> Result<HelperRequest, HelperFailure> {
    let expected_directory = std::env::temp_dir().join("iHub-hosts-actions");
    let canonical_directory = expected_directory
        .canonicalize()
        .map_err(|_| HelperFailure::Other)?;
    let canonical_path = path.canonicalize().map_err(|_| HelperFailure::Other)?;
    if canonical_path.parent() != Some(canonical_directory.as_path())
        || canonical_path.extension().and_then(|value| value.to_str()) != Some("json")
    {
        return Err(HelperFailure::Other);
    }
    let metadata = fs::metadata(path).map_err(|_| HelperFailure::Other)?;
    if !metadata.is_file() || metadata.len() > MAX_HELPER_REQUEST_BYTES {
        return Err(HelperFailure::Other);
    }
    let bytes = fs::read(path).map_err(|_| HelperFailure::Other)?;
    let request: HelperRequest =
        serde_json::from_slice(&bytes).map_err(|_| HelperFailure::Other)?;
    if request.version != 1
        || Uuid::parse_str(&request.request_id).is_err()
        || canonical_path.file_stem().and_then(|value| value.to_str())
            != Some(request.request_id.as_str())
        || request.expires_at_epoch_ms < epoch_millis()
        || validate_fingerprint(&request.expected_fingerprint).is_err()
    {
        return Err(HelperFailure::Other);
    }
    Ok(request)
}

fn execute_helper_request(request: HelperRequest) -> Result<(), HelperFailure> {
    let current = read_hosts_bytes().map_err(|_| HelperFailure::Other)?;
    ensure_fingerprint(&current, &request.expected_fingerprint)
        .map_err(|_| HelperFailure::Stale)?;
    let desired = match request.action {
        HelperAction::Apply { content_base64 } => BASE64
            .decode(content_base64)
            .map_err(|_| HelperFailure::Other)?,
        HelperAction::RestoreBackup => read_bounded_file(
            &hosts_backup_path().map_err(|_| HelperFailure::Backup)?,
            "hosts 备份",
        )
        .map_err(|_| HelperFailure::Backup)?,
    };
    validate_hosts_payload(&desired).map_err(|_| HelperFailure::Other)?;
    replace_hosts_atomically(&desired).map_err(|_| HelperFailure::Other)
}

#[cfg(windows)]
fn replace_hosts_atomically(desired: &[u8]) -> Result<(), String> {
    let hosts = hosts_path()?;
    let backup = hosts_backup_path()?;
    replace_file_atomically(&hosts, &backup, desired)
}

#[cfg(windows)]
fn replace_file_atomically(hosts: &Path, backup: &Path, desired: &[u8]) -> Result<(), String> {
    use windows::{
        core::PCWSTR,
        Win32::Storage::FileSystem::{ReplaceFileW, REPLACEFILE_IGNORE_MERGE_ERRORS},
    };
    let parent = hosts
        .parent()
        .ok_or_else(|| "hosts 文件没有父目录。".to_owned())?;
    let temporary = parent.join(format!("hosts.ihub-{}.tmp", Uuid::new_v4().simple()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| format!("无法创建 hosts 临时文件：{error}"))?;
    file.write_all(desired)
        .map_err(|error| format!("无法写入 hosts 临时文件：{error}"))?;
    file.sync_all()
        .map_err(|error| format!("无法同步 hosts 临时文件：{error}"))?;
    drop(file);
    if backup.exists() {
        fs::remove_file(backup).map_err(|error| format!("无法轮换 hosts 备份：{error}"))?;
    }
    let hosts_wide = wide(&hosts.to_string_lossy());
    let temporary_wide = wide(&temporary.to_string_lossy());
    let backup_wide = wide(&backup.to_string_lossy());
    let result = unsafe {
        ReplaceFileW(
            PCWSTR(hosts_wide.as_ptr()),
            PCWSTR(temporary_wide.as_ptr()),
            PCWSTR(backup_wide.as_ptr()),
            REPLACEFILE_IGNORE_MERGE_ERRORS,
            None,
            None,
        )
    };
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(format!("无法原子替换 hosts 文件：{error}"));
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_hosts_atomically(_desired: &[u8]) -> Result<(), String> {
    Err("hosts 管理目前只在 Windows 10/11 上提供。".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_only_the_managed_block_and_preserves_external_bytes() {
        let original = b"# system\r\n127.0.0.1 localhost\r\n# >>> iHub managed hosts >>>\r\n1.1.1.1 old.test\r\n# <<< iHub managed hosts <<<\r\n# tail\r\n";
        let desired = replace_managed_block(
            original,
            &[HostsManagedEntryInput {
                ip: "127.0.0.1".to_owned(),
                domains: vec!["ads.test".to_owned()],
                comment: Some("local".to_owned()),
                enabled: true,
            }],
        )
        .unwrap();
        assert!(desired.starts_with(b"# system\r\n127.0.0.1 localhost\r\n"));
        assert!(desired.ends_with(b"# tail\r\n"));
        assert!(String::from_utf8(desired)
            .unwrap()
            .contains("127.0.0.1 ads.test # local\r\n"));
    }

    #[test]
    fn keeps_non_utf8_external_comments_byte_exact() {
        let original = b"# legacy \x80 comment\r\n# >>> iHub managed hosts >>>\r\n1.1.1.1 old.test\r\n# <<< iHub managed hosts <<<\r\n";
        let desired = replace_managed_block(original, &[]).unwrap();
        assert!(desired.starts_with(b"# legacy \x80 comment\r\n"));
        assert_eq!(desired.iter().filter(|byte| **byte == 0x80).count(), 1);
    }

    #[test]
    fn validates_domains_duplicates_and_comments() {
        assert_eq!(
            normalize_domain("Example.COM."),
            Ok("example.com".to_owned())
        );
        assert!(normalize_domain("*.example.com").is_err());
        assert!(validate_entries(vec![
            HostsManagedEntryInput {
                ip: "127.0.0.1".to_owned(),
                domains: vec!["same.test".to_owned()],
                comment: None,
                enabled: true
            },
            HostsManagedEntryInput {
                ip: "::1".to_owned(),
                domains: vec!["SAME.TEST".to_owned()],
                comment: None,
                enabled: true
            },
        ])
        .is_err());
    }

    #[test]
    fn parses_enabled_disabled_and_external_rows() {
        let bytes = b"127.0.0.1 localhost\n# >>> iHub managed hosts >>>\n# ihub-disabled 0.0.0.0 ads.test # paused\n::1 ipv6.test\n# <<< iHub managed hosts <<<\n";
        let (all, managed) = parse_entries(bytes).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(managed.len(), 2);
        assert!(!managed[0].enabled);
        assert_eq!(managed[0].comment.as_deref(), Some("paused"));
    }

    #[cfg(windows)]
    #[test]
    fn quotes_windows_arguments_without_losing_trailing_slashes() {
        assert_eq!(
            quote_windows_argument(r#"C:\Program Files\iHub\"#),
            r#""C:\Program Files\iHub\\""#
        );
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "reads the real Windows hosts file without modifying it"]
    fn reads_current_windows_hosts_snapshot() {
        let snapshot =
            get_hosts_snapshot().expect("the fixed Windows hosts path should be readable");
        assert_eq!(snapshot.fingerprint.len(), 64);
        assert!(snapshot.size_bytes <= MAX_HOSTS_BYTES);
    }

    #[cfg(windows)]
    #[test]
    fn atomically_replaces_a_fixture_and_keeps_the_previous_bytes() {
        let directory = std::env::temp_dir().join(format!("ihub-hosts-test-{}", Uuid::new_v4()));
        fs::create_dir(&directory).unwrap();
        let target = directory.join("hosts");
        let backup = directory.join("hosts.ihub-backup");
        fs::write(&target, b"127.0.0.1 old.test\r\n").unwrap();
        replace_file_atomically(&target, &backup, b"127.0.0.1 new.test\r\n").unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"127.0.0.1 new.test\r\n");
        assert_eq!(fs::read(&backup).unwrap(), b"127.0.0.1 old.test\r\n");
        fs::remove_file(target).unwrap();
        fs::remove_file(backup).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
