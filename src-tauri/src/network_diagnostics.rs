//! Bounded first-party IP and connection-quality diagnostics.
//!
//! The renderer cannot choose a URL or byte count. Public IP lookup and the
//! speed test only contact Cloudflare's documented fixed endpoints after an
//! explicit click in the trusted built-in workbench.

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use reqwest::{redirect::Policy, Client};
use serde::Serialize;

const TRACE_URL: &str = "https://speed.cloudflare.com/cdn-cgi/trace";
const DOWNLOAD_URL: &str = "https://speed.cloudflare.com/__down?bytes=10000000";
const LATENCY_URL: &str = "https://speed.cloudflare.com/__down?bytes=0";
const UPLOAD_URL: &str = "https://speed.cloudflare.com/__up";
const LATENCY_SAMPLES: usize = 6;
const DOWNLOAD_BYTES: usize = 10_000_000;
const UPLOAD_BYTES: usize = 5_000_000;
const MAX_TRACE_BYTES: usize = 8 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(18);

static NETWORK_TEST_RUNNING: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LocalNetworkInfo {
    preferred_ipv4: Option<String>,
    preferred_ipv6: Option<String>,
    online_route_available: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PublicNetworkInfo {
    public_ip: String,
    address_family: String,
    edge_location: Option<String>,
    tls_version: Option<String>,
    http_protocol: Option<String>,
    provider: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NetworkSpeedResult {
    latency_ms: f64,
    jitter_ms: f64,
    download_mbps: f64,
    upload_mbps: f64,
    download_bytes: usize,
    upload_bytes: usize,
    duration_ms: u64,
    provider: String,
}

struct NetworkTestLease;

impl NetworkTestLease {
    fn acquire() -> Result<Self, String> {
        NETWORK_TEST_RUNNING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self)
            .map_err(|_| "已有一项网络测速正在运行，请等待它完成。".to_owned())
    }
}

impl Drop for NetworkTestLease {
    fn drop(&mut self) {
        NETWORK_TEST_RUNNING.store(false, Ordering::Release);
    }
}

#[tauri::command]
pub fn get_local_network_info() -> LocalNetworkInfo {
    let preferred_ipv4 = preferred_route_ip(SocketAddr::from((Ipv4Addr::new(1, 1, 1, 1), 443)));
    let preferred_ipv6 = preferred_route_ip(SocketAddr::from((
        Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111),
        443,
    )));
    LocalNetworkInfo {
        online_route_available: preferred_ipv4.is_some() || preferred_ipv6.is_some(),
        preferred_ipv4,
        preferred_ipv6,
    }
}

#[tauri::command]
pub async fn get_public_network_info() -> Result<PublicNetworkInfo, String> {
    let client = network_client()?;
    let response = client
        .get(TRACE_URL)
        .send()
        .await
        .map_err(|error| request_error("无法查询公网 IP", &error))?;
    if !response.status().is_success() {
        return Err(format!(
            "Cloudflare 公网 IP 服务返回了 HTTP {}。",
            response.status().as_u16()
        ));
    }
    let body = read_bounded_response(response, MAX_TRACE_BYTES).await?;
    parse_trace(&body)
}

#[tauri::command]
pub async fn run_network_speed_test() -> Result<NetworkSpeedResult, String> {
    let _lease = NetworkTestLease::acquire()?;
    let client = network_client()?;
    let test_started = Instant::now();

    let mut latency_samples = Vec::with_capacity(LATENCY_SAMPLES);
    for _ in 0..LATENCY_SAMPLES {
        let started = Instant::now();
        let response = client
            .get(LATENCY_URL)
            .send()
            .await
            .map_err(|error| request_error("延迟测试失败", &error))?;
        if !response.status().is_success() {
            return Err(format!(
                "Cloudflare 延迟端点返回了 HTTP {}。",
                response.status().as_u16()
            ));
        }
        let _ = read_bounded_response(response, 64).await?;
        latency_samples.push(started.elapsed().as_secs_f64() * 1_000.0);
    }

    let download_started = Instant::now();
    let response = client
        .get(DOWNLOAD_URL)
        .send()
        .await
        .map_err(|error| request_error("下载测速失败", &error))?;
    if !response.status().is_success() {
        return Err(format!(
            "Cloudflare 下载端点返回了 HTTP {}。",
            response.status().as_u16()
        ));
    }
    let downloaded = read_bounded_response(response, DOWNLOAD_BYTES).await?;
    if downloaded.len() != DOWNLOAD_BYTES {
        return Err(format!(
            "下载测速只收到 {} 字节，未达到预期的 {} 字节。",
            downloaded.len(),
            DOWNLOAD_BYTES
        ));
    }
    let download_seconds = download_started.elapsed().as_secs_f64();

    // The bytes are deterministic and carry no local content. Their only
    // purpose is to measure transfer time against the fixed upload endpoint.
    let upload_payload = vec![0x5a_u8; UPLOAD_BYTES];
    let upload_started = Instant::now();
    let response = client
        .post(UPLOAD_URL)
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .body(upload_payload)
        .send()
        .await
        .map_err(|error| request_error("上传测速失败", &error))?;
    if !response.status().is_success() {
        return Err(format!(
            "Cloudflare 上传端点返回了 HTTP {}。",
            response.status().as_u16()
        ));
    }
    let _ = read_bounded_response(response, MAX_TRACE_BYTES).await?;
    let upload_seconds = upload_started.elapsed().as_secs_f64();

    let (latency_ms, jitter_ms) = latency_and_jitter(&latency_samples)?;
    Ok(NetworkSpeedResult {
        latency_ms: rounded(latency_ms),
        jitter_ms: rounded(jitter_ms),
        download_mbps: rounded(megabits_per_second(DOWNLOAD_BYTES, download_seconds)?),
        upload_mbps: rounded(megabits_per_second(UPLOAD_BYTES, upload_seconds)?),
        download_bytes: DOWNLOAD_BYTES,
        upload_bytes: UPLOAD_BYTES,
        duration_ms: test_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
        provider: "Cloudflare Edge".to_owned(),
    })
}

fn preferred_route_ip(destination: SocketAddr) -> Option<String> {
    let bind_address = if destination.is_ipv4() {
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0))
    } else {
        SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0))
    };
    let socket = UdpSocket::bind(bind_address).ok()?;
    socket.connect(destination).ok()?;
    let address = socket.local_addr().ok()?.ip();
    match address {
        IpAddr::V4(value) if !value.is_unspecified() && !value.is_loopback() => {
            Some(value.to_string())
        }
        IpAddr::V6(value) if !value.is_unspecified() && !value.is_loopback() => {
            Some(value.to_string())
        }
        _ => None,
    }
}

fn network_client() -> Result<Client, String> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .redirect(Policy::none())
        .user_agent("iHub Network Diagnostics/0.1")
        .build()
        .map_err(|error| format!("无法创建受限网络诊断连接：{error}"))
}

async fn read_bounded_response(
    mut response: reqwest::Response,
    maximum_bytes: usize,
) -> Result<Vec<u8>, String> {
    if response
        .content_length()
        .is_some_and(|length| length > maximum_bytes as u64)
    {
        return Err(format!("网络响应超过 {} 字节安全上限。", maximum_bytes));
    }
    let mut output = Vec::with_capacity(maximum_bytes.min(64 * 1024));
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| request_error("读取网络响应失败", &error))?
    {
        let next_length = output
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| "网络响应长度溢出。".to_owned())?;
        if next_length > maximum_bytes {
            return Err(format!("网络响应超过 {} 字节安全上限。", maximum_bytes));
        }
        output.extend_from_slice(&chunk);
    }
    Ok(output)
}

fn parse_trace(bytes: &[u8]) -> Result<PublicNetworkInfo, String> {
    let body = std::str::from_utf8(bytes)
        .map_err(|_| "Cloudflare 返回的公网 IP 信息不是 UTF-8。".to_owned())?;
    let mut ip = None;
    let mut edge_location = None;
    let mut tls_version = None;
    let mut http_protocol = None;
    for line in body.lines().take(64) {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "ip" => ip = value.parse::<IpAddr>().ok(),
            "colo" if valid_trace_token(value) => edge_location = Some(value.to_owned()),
            "tls" if valid_trace_token(value) => tls_version = Some(value.to_owned()),
            "http" if valid_trace_token(value) => http_protocol = Some(value.to_owned()),
            _ => {}
        }
    }
    let ip = ip.ok_or_else(|| "Cloudflare 响应中没有有效的公网 IP。".to_owned())?;
    Ok(PublicNetworkInfo {
        public_ip: ip.to_string(),
        address_family: if ip.is_ipv4() { "IPv4" } else { "IPv6" }.to_owned(),
        edge_location,
        tls_version,
        http_protocol,
        provider: "Cloudflare Trace".to_owned(),
    })
}

fn valid_trace_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn latency_and_jitter(samples: &[f64]) -> Result<(f64, f64), String> {
    if samples.len() < 2
        || samples
            .iter()
            .any(|value| !value.is_finite() || *value < 0.0)
    {
        return Err("延迟样本不足或无效。".to_owned());
    }
    let mut ordered = samples.to_vec();
    ordered.sort_by(f64::total_cmp);
    let median = if ordered.len() % 2 == 0 {
        (ordered[ordered.len() / 2 - 1] + ordered[ordered.len() / 2]) / 2.0
    } else {
        ordered[ordered.len() / 2]
    };
    let jitter = samples
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).abs())
        .sum::<f64>()
        / (samples.len() - 1) as f64;
    Ok((median, jitter))
}

fn megabits_per_second(bytes: usize, seconds: f64) -> Result<f64, String> {
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err("测速持续时间无效。".to_owned());
    }
    Ok((bytes as f64 * 8.0) / seconds / 1_000_000.0)
}

fn rounded(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

fn request_error(context: &str, error: &reqwest::Error) -> String {
    if error.is_timeout() {
        format!("{context}：请求超时。")
    } else if error.is_connect() {
        format!("{context}：无法连接 Cloudflare。")
    } else {
        format!("{context}：{error}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bounded_cloudflare_trace() {
        let result = parse_trace(b"fl=29f50\nip=203.0.113.4\ntls=TLSv1.3\nhttp=h2\ncolo=HKG\n")
            .expect("trace should parse");
        assert_eq!(result.public_ip, "203.0.113.4");
        assert_eq!(result.address_family, "IPv4");
        assert_eq!(result.edge_location.as_deref(), Some("HKG"));
        assert_eq!(result.tls_version.as_deref(), Some("TLSv1.3"));
        assert_eq!(result.http_protocol.as_deref(), Some("h2"));
    }

    #[test]
    fn rejects_trace_without_valid_ip() {
        assert!(parse_trace(b"ip=not-an-ip\ncolo=HKG\n").is_err());
    }

    #[test]
    fn calculates_median_latency_and_average_jitter() {
        let (latency, jitter) =
            latency_and_jitter(&[18.0, 22.0, 20.0, 25.0]).expect("valid samples should calculate");
        assert_eq!(latency, 21.0);
        assert_eq!(jitter, 11.0 / 3.0);
    }

    #[test]
    fn calculates_decimal_megabits_per_second() {
        assert_eq!(megabits_per_second(10_000_000, 1.0), Ok(80.0));
    }

    #[test]
    fn validates_trace_tokens() {
        assert!(valid_trace_token("TLSv1.3"));
        assert!(!valid_trace_token("HKG\nforged=value"));
        assert!(!valid_trace_token(&"x".repeat(33)));
    }
}
