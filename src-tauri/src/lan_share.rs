//! Explicit, tokenized LAN file sharing for the trusted built-in workbench.
//!
//! Files are chosen by the native system picker and opened immediately. The
//! renderer receives only basenames, sizes and one random HTTP URL. The server
//! accepts private/loopback peers, exposes no upload or path API, and expires
//! automatically after thirty minutes.

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use uuid::Uuid;

pub const MAX_LAN_SHARE_FILES: usize = 32;
const MAX_LAN_SHARE_TOTAL_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const SHARE_TTL: Duration = Duration::from_secs(30 * 60);
const MAX_REQUEST_HEADER_BYTES: usize = 16 * 1024;
const MAX_CONCURRENT_CONNECTIONS: usize = 4;
const FILE_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LanSharedFileView {
    name: String,
    size: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LanFileShareView {
    url: String,
    files: Vec<LanSharedFileView>,
    total_bytes: u64,
    download_count: u64,
    bytes_sent: u64,
    started_at_epoch_ms: u64,
    expires_at_epoch_ms: u64,
    expires_in_seconds: u64,
}

#[derive(Debug)]
struct SharedFile {
    name: String,
    size: u64,
    file: Arc<Mutex<File>>,
}

#[derive(Debug)]
struct ShareRuntime {
    token: String,
    url: String,
    files: Arc<Vec<SharedFile>>,
    total_bytes: u64,
    started_at_epoch_ms: u64,
    expires_at_epoch_ms: u64,
    deadline: Instant,
    stop: AtomicBool,
    download_count: AtomicU64,
    bytes_sent: AtomicU64,
    connections: AtomicUsize,
}

impl ShareRuntime {
    fn view(&self) -> LanFileShareView {
        LanFileShareView {
            url: self.url.clone(),
            files: self
                .files
                .iter()
                .map(|file| LanSharedFileView {
                    name: file.name.clone(),
                    size: file.size,
                })
                .collect(),
            total_bytes: self.total_bytes,
            download_count: self.download_count.load(Ordering::Acquire),
            bytes_sent: self.bytes_sent.load(Ordering::Acquire),
            started_at_epoch_ms: self.started_at_epoch_ms,
            expires_at_epoch_ms: self.expires_at_epoch_ms,
            expires_in_seconds: self
                .deadline
                .saturating_duration_since(Instant::now())
                .as_secs(),
        }
    }

    fn active(&self) -> bool {
        !self.stop.load(Ordering::Acquire) && Instant::now() < self.deadline
    }
}

#[derive(Debug)]
struct ActiveShare {
    runtime: Arc<ShareRuntime>,
    listener_thread: Option<JoinHandle<()>>,
}

#[derive(Debug, Default)]
pub struct LanFileShareState {
    active: Mutex<Option<ActiveShare>>,
}

impl LanFileShareState {
    pub fn start(&self, paths: Vec<PathBuf>) -> Result<LanFileShareView, String> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cleanup_finished_share(&mut active);
        if active.is_some() {
            return Err("已有内网文件分享正在运行，请先停止后再选择新文件。".to_owned());
        }
        let files = Arc::new(open_selected_files(paths)?);
        let total_bytes = files.iter().try_fold(0_u64, |total, file| {
            total
                .checked_add(file.size)
                .ok_or_else(|| "分享文件总大小溢出。".to_owned())
        })?;
        if total_bytes > MAX_LAN_SHARE_TOTAL_BYTES {
            return Err("单次内网分享文件总大小不能超过 64 GiB。".to_owned());
        }
        let local_ip = preferred_lan_ipv4()?;
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))
            .map_err(|error| format!("无法启动内网分享端口：{error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("无法配置内网分享端口：{error}"))?;
        let port = listener
            .local_addr()
            .map_err(|error| format!("无法读取内网分享端口：{error}"))?
            .port();
        let token = Uuid::new_v4().simple().to_string();
        let url = format!("http://{local_ip}:{port}/{token}/");
        let started_at_epoch_ms = epoch_millis();
        let expires_at_epoch_ms = started_at_epoch_ms.saturating_add(SHARE_TTL.as_millis() as u64);
        let runtime = Arc::new(ShareRuntime {
            token,
            url,
            files,
            total_bytes,
            started_at_epoch_ms,
            expires_at_epoch_ms,
            deadline: Instant::now() + SHARE_TTL,
            stop: AtomicBool::new(false),
            download_count: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
            connections: AtomicUsize::new(0),
        });
        let thread_runtime = runtime.clone();
        let listener_thread = thread::Builder::new()
            .name("ihub-lan-share".to_owned())
            .spawn(move || run_listener(listener, thread_runtime))
            .map_err(|error| format!("无法启动内网分享线程：{error}"))?;
        let view = runtime.view();
        *active = Some(ActiveShare {
            runtime,
            listener_thread: Some(listener_thread),
        });
        Ok(view)
    }

    pub fn status(&self) -> Option<LanFileShareView> {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cleanup_finished_share(&mut active);
        active.as_ref().map(|share| share.runtime.view())
    }

    pub fn stop(&self) -> Result<(), String> {
        let mut share = self
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(active) = share.as_mut() {
            active.runtime.stop.store(true, Ordering::Release);
            if let Some(handle) = active.listener_thread.take() {
                handle
                    .join()
                    .map_err(|_| "内网分享线程未能正常结束。".to_owned())?;
            }
        }
        Ok(())
    }
}

impl Drop for LanFileShareState {
    fn drop(&mut self) {
        if let Ok(active) = self.active.get_mut() {
            if let Some(mut share) = active.take() {
                share.runtime.stop.store(true, Ordering::Release);
                if let Some(handle) = share.listener_thread.take() {
                    let _ = handle.join();
                }
            }
        }
    }
}

fn cleanup_finished_share(active: &mut Option<ActiveShare>) {
    if active.as_ref().is_some_and(|share| !share.runtime.active()) {
        if let Some(mut finished) = active.take() {
            finished.runtime.stop.store(true, Ordering::Release);
            if let Some(handle) = finished.listener_thread.take() {
                let _ = handle.join();
            }
        }
    }
}

fn open_selected_files(paths: Vec<PathBuf>) -> Result<Vec<SharedFile>, String> {
    if paths.is_empty() {
        return Err("请至少选择一个要分享的文件。".to_owned());
    }
    if paths.len() > MAX_LAN_SHARE_FILES {
        return Err(format!("单次最多选择 {MAX_LAN_SHARE_FILES} 个文件。"));
    }
    paths
        .into_iter()
        .map(|path| {
            let canonical = path
                .canonicalize()
                .map_err(|error| format!("无法读取所选文件：{error}"))?;
            let name = canonical
                .file_name()
                .and_then(|value| value.to_str())
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "所选文件没有可显示的 UTF-8 文件名。".to_owned())?;
            let name = name
                .chars()
                .map(|character| {
                    if character.is_control() {
                        '�'
                    } else {
                        character
                    }
                })
                .take(255)
                .collect::<String>();
            let file = File::open(&canonical)
                .map_err(|error| format!("无法打开“{name}”用于内网分享：{error}"))?;
            let metadata = file
                .metadata()
                .map_err(|error| format!("无法检查所选文件：{error}"))?;
            if !metadata.is_file() {
                return Err("内网分享只接受系统选择器选中的普通文件。".to_owned());
            }
            Ok(SharedFile {
                name,
                size: metadata.len(),
                file: Arc::new(Mutex::new(file)),
            })
        })
        .collect()
}

fn preferred_lan_ipv4() -> Result<Ipv4Addr, String> {
    let socket = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))
        .map_err(|_| "无法检查本机内网路由。".to_owned())?;
    socket
        .connect(SocketAddr::from((Ipv4Addr::new(1, 1, 1, 1), 443)))
        .map_err(|_| "当前没有可用于内网分享的 IPv4 路由。".to_owned())?;
    match socket.local_addr().map(|address| address.ip()) {
        Ok(IpAddr::V4(ip)) if !ip.is_unspecified() && !ip.is_loopback() => Ok(ip),
        _ => Err("当前没有可用于内网分享的 IPv4 地址。".to_owned()),
    }
}

fn run_listener(listener: TcpListener, runtime: Arc<ShareRuntime>) {
    while runtime.active() {
        match listener.accept() {
            Ok((stream, peer)) => {
                if !is_lan_peer(peer.ip()) {
                    let _ = respond_status(stream, 403, "Forbidden", b"LAN access only");
                    continue;
                }
                if runtime.connections.fetch_add(1, Ordering::AcqRel) >= MAX_CONCURRENT_CONNECTIONS
                {
                    runtime.connections.fetch_sub(1, Ordering::AcqRel);
                    let _ = respond_status(stream, 429, "Too Many Requests", b"Try again later");
                    continue;
                }
                let connection_runtime = runtime.clone();
                if thread::Builder::new()
                    .name("ihub-lan-download".to_owned())
                    .spawn(move || {
                        let _lease = ConnectionLease(&connection_runtime.connections);
                        let _ = handle_connection(stream, &connection_runtime);
                    })
                    .is_err()
                {
                    runtime.connections.fetch_sub(1, Ordering::AcqRel);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(_) => break,
        }
    }
    runtime.stop.store(true, Ordering::Release);
}

struct ConnectionLease<'a>(&'a AtomicUsize);

impl Drop for ConnectionLease<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn handle_connection(mut stream: TcpStream, runtime: &ShareRuntime) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(60)))?;
    let request = read_request(&mut stream)?;
    let Some((method, path)) = parse_request_line(&request) else {
        return respond_status(stream, 400, "Bad Request", b"Bad request");
    };
    if method != "GET" && method != "HEAD" {
        return respond_status(stream, 405, "Method Not Allowed", b"GET and HEAD only");
    }
    let root = format!("/{}/", runtime.token);
    if path == root {
        let body = share_page(runtime).into_bytes();
        write_headers(
            &mut stream,
            200,
            "OK",
            "text/html; charset=utf-8",
            body.len() as u64,
            &["Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'"],
        )?;
        if method == "GET" {
            stream.write_all(&body)?;
        }
        return Ok(());
    }
    let prefix = format!("/{}/file/", runtime.token);
    let Some(index) = path
        .strip_prefix(&prefix)
        .and_then(|value| value.parse::<usize>().ok())
    else {
        return respond_status(stream, 404, "Not Found", b"Not found");
    };
    let Some(shared) = runtime.files.get(index) else {
        return respond_status(stream, 404, "Not Found", b"Not found");
    };
    let disposition = content_disposition(&shared.name);
    write_headers(
        &mut stream,
        200,
        "OK",
        "application/octet-stream",
        shared.size,
        &[&disposition],
    )?;
    if method == "HEAD" {
        return Ok(());
    }
    // A duplicated Windows file handle may share its seek cursor with the
    // original. Serializing reads per selected file and rewinding the retained
    // handle guarantees that concurrent clients never splice each other's
    // byte ranges while still avoiding a second path lookup after selection.
    let mut file = shared
        .file
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    file.seek(SeekFrom::Start(0))?;
    let mut buffer = vec![0_u8; FILE_BUFFER_BYTES];
    let mut sent = 0_u64;
    while runtime.active() {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            runtime.download_count.fetch_add(1, Ordering::AcqRel);
            break;
        }
        stream.write_all(&buffer[..read])?;
        sent = sent.saturating_add(read as u64);
        runtime.bytes_sent.fetch_add(read as u64, Ordering::AcqRel);
    }
    if sent != shared.size && runtime.active() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "shared file changed length during transfer",
        ));
    }
    Ok(())
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    while bytes.len() < MAX_REQUEST_HEADER_BYTES {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(bytes);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "request headers exceeded the limit",
    ))
}

fn parse_request_line(request: &[u8]) -> Option<(&str, &str)> {
    let line_end = request.windows(2).position(|window| window == b"\r\n")?;
    let line = std::str::from_utf8(&request[..line_end]).ok()?;
    let mut parts = line.split(' ');
    let method = parts.next()?;
    let path = parts.next()?;
    let version = parts.next()?;
    if parts.next().is_some()
        || !matches!(method, "GET" | "HEAD")
        || !path.starts_with('/')
        || path.contains(['?', '#'])
        || version != "HTTP/1.1"
    {
        return None;
    }
    Some((method, path))
}

fn respond_status(
    mut stream: TcpStream,
    code: u16,
    reason: &str,
    body: &[u8],
) -> std::io::Result<()> {
    write_headers(
        &mut stream,
        code,
        reason,
        "text/plain; charset=utf-8",
        body.len() as u64,
        &[],
    )?;
    stream.write_all(body)
}

fn write_headers(
    stream: &mut TcpStream,
    code: u16,
    reason: &str,
    content_type: &str,
    content_length: u64,
    extra: &[&str],
) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {code} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {content_length}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n"
    )?;
    for header in extra {
        write!(stream, "{header}\r\n")?;
    }
    write!(stream, "\r\n")
}

fn share_page(runtime: &ShareRuntime) -> String {
    let mut files = String::new();
    for (index, file) in runtime.files.iter().enumerate() {
        files.push_str(&format!(
            "<li><a href=\"file/{index}\"><span>{}</span><small>{}</small></a></li>",
            html_escape(&file.name),
            human_bytes(file.size)
        ));
    }
    format!(
        "<!doctype html><html lang=\"zh-CN\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>iHub 内网文件分享</title><style>*{{box-sizing:border-box}}body{{margin:0;background:linear-gradient(145deg,#0d245f,#271b6c);color:#1c1c1e;font:15px system-ui,sans-serif;min-height:100vh;padding:28px}}main{{background:rgba(255,255,255,.94);border:1px solid rgba(100,210,255,.3);border-radius:18px;box-shadow:0 24px 60px rgba(0,0,0,.24);margin:auto;max-width:620px;overflow:hidden}}header{{background:linear-gradient(135deg,#0a84ff,#5e5ce6);color:#fff;padding:24px}}h1{{font-size:22px;margin:0 0 5px}}p{{margin:0;opacity:.78}}ul{{list-style:none;margin:0;padding:16px}}li+li{{margin-top:9px}}a{{align-items:center;background:#f7f9ff;border:1px solid rgba(10,132,255,.16);border-radius:12px;color:#1c1c1e;display:flex;justify-content:space-between;padding:14px;text-decoration:none}}a:hover{{border-color:#0a84ff;background:#eef6ff}}span{{font-weight:700;overflow-wrap:anywhere}}small{{color:#636366;margin-left:12px;white-space:nowrap}}footer{{border-top:1px solid rgba(94,92,230,.12);color:#636366;font-size:12px;padding:15px 20px}}</style><main><header><h1>iHub 内网文件分享</h1><p>点击文件即可下载到当前设备</p></header><ul>{files}</ul><footer>随机链接 · 仅局域网 · 30 分钟自动停止 · 无广告</footer></main></html>"
    )
}

fn content_disposition(name: &str) -> String {
    let fallback = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .take(120)
        .collect::<String>();
    format!(
        "Content-Disposition: attachment; filename=\"{}\"; filename*=UTF-8''{}",
        if fallback.is_empty() {
            "download"
        } else {
            &fallback
        },
        percent_encode(name.as_bytes())
    )
}

fn percent_encode(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len());
    for byte in bytes {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            output.push(char::from(*byte));
        } else {
            output.push_str(&format!("%{byte:02X}"));
        }
    }
    output
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn human_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

fn is_lan_peer(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        IpAddr::V6(ip) => {
            let segments = ip.segments();
            ip.is_loopback() || (segments[0] & 0xfe00) == 0xfc00 || (segments[0] & 0xffc0) == 0xfe80
        }
    }
}

fn epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_private_loopback_or_link_local_peers() {
        assert!(is_lan_peer("192.168.1.20".parse().unwrap()));
        assert!(is_lan_peer("10.4.3.2".parse().unwrap()));
        assert!(is_lan_peer("127.0.0.1".parse().unwrap()));
        assert!(is_lan_peer("fe80::1".parse().unwrap()));
        assert!(!is_lan_peer("8.8.8.8".parse().unwrap()));
        assert!(!is_lan_peer("2001:4860:4860::8888".parse().unwrap()));
    }

    #[test]
    fn parses_only_exact_bounded_get_or_head_request_lines() {
        assert_eq!(
            parse_request_line(b"GET /token/file/0 HTTP/1.1\r\nHost: local\r\n\r\n"),
            Some(("GET", "/token/file/0"))
        );
        assert!(parse_request_line(b"POST /token/file/0 HTTP/1.1\r\n\r\n").is_none());
        assert!(parse_request_line(b"GET http://example.com/ HTTP/1.1\r\n\r\n").is_none());
        assert!(parse_request_line(b"GET /token/?path=x HTTP/1.1\r\n\r\n").is_none());
    }

    #[test]
    fn escapes_names_for_html_and_content_disposition() {
        assert_eq!(html_escape("<a&\"'>"), "&lt;a&amp;&quot;&#39;&gt;");
        let header = content_disposition("报告 1.txt");
        assert!(header.contains("filename=\"___1.txt\""));
        assert!(header.contains("%E6%8A%A5%E5%91%8A%201.txt"));
        assert!(!header.contains('\n'));
        assert!(!header.contains('\r'));
    }

    #[test]
    #[ignore = "manual LAN listener and local-route acceptance test"]
    fn serves_an_explicitly_selected_file_over_loopback() {
        let path = std::env::temp_dir().join(format!("ihub-lan-share-{}.txt", Uuid::new_v4()));
        std::fs::write(&path, b"iHub LAN fixture").expect("fixture should be created");
        let state = LanFileShareState::default();
        let view = state.start(vec![path.clone()]).expect("share should start");
        let url = url::Url::parse(&view.url).expect("share URL should parse");
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, url.port().unwrap()));
        for _ in 0..2 {
            let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))
                .expect("loopback client should connect");
            write!(
                stream,
                "GET {}file/0 HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
                url.path()
            )
            .expect("request should write");
            let mut response = Vec::new();
            stream
                .read_to_end(&mut response)
                .expect("response should read");
            assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
            assert!(response.ends_with(b"iHub LAN fixture"));
        }
        assert_eq!(
            state.status().expect("share stays active").download_count,
            2
        );
        state.stop().expect("share should stop");
        std::fs::remove_file(path).expect("fixture should be removed");
    }
}
