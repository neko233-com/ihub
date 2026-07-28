//! First-party WebDAV transport and native Cloud Drive sessions.
//!
//! A password may cross IPC only while the trusted built-in surface creates a
//! connection. Later requests carry an opaque UUID and resolve credentials
//! inside this module. Saved passwords stay in the OS credential store through
//! `CloudCredentialVault`; neither session credentials nor the vault are
//! reachable from the plugin bridge.

use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use reqwest::{
    header::{ACCEPT, CONTENT_LENGTH, CONTENT_TYPE, IF_NONE_MATCH},
    redirect::Policy,
    Body, Client, Method,
};
use serde::{Deserialize, Serialize};
use tokio::fs::File as TokioFile;
use tokio_util::io::ReaderStream;
use url::Url;
use uuid::{Uuid, Version};
use zeroize::Zeroizing;

use crate::cloud_credentials::{CloudCredentialVault, CloudProfileView};

const MAX_ENDPOINT_BYTES: usize = 2_048;
const MAX_ACCOUNT_BYTES: usize = 1_024;
const MAX_PASSWORD_BYTES: usize = 4_096;
const MAX_STORED_PASSWORD_BYTES: usize = 2_048;
const MAX_PROFILE_LABEL_BYTES: usize = 96;
const MAX_DIRECTORY_XML_BYTES: usize = 8 * 1024 * 1024;
const MAX_DOWNLOAD_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_SESSIONS: usize = 4;
const SESSION_IDLE_TTL: Duration = Duration::from_secs(30 * 60);
const SESSION_HARD_TTL: Duration = Duration::from_secs(8 * 60 * 60);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebDavConnectRequest {
    endpoint: String,
    username: String,
    password: String,
    remember: bool,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloudProfileConnectRequest {
    profile_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebDavListRequest {
    connection_id: String,
    #[serde(default)]
    directory: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebDavDisconnectRequest {
    connection_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloudProfileForgetRequest {
    profile_id: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebDavConnectResponse {
    connection_id: String,
    profile: Option<CloudProfileView>,
    endpoint: String,
    directory: String,
    xml: String,
}

/// IPC-facing name used by the app command layer.
pub type WebDavConnectResult = WebDavConnectResponse;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebDavDirectoryResponse {
    endpoint: String,
    directory: String,
    /// Raw, bounded XML is parsed only by the trusted built-in renderer. This
    /// keeps request control in Rust without adding a second XML parser here.
    xml: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebDavDownloadRequest {
    connection_id: String,
    remote_url: String,
    suggested_filename: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebDavDownloadResult {
    cancelled: bool,
    bytes_written: u64,
    filename: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebDavUploadRequest {
    connection_id: String,
    directory: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebDavUploadResult {
    cancelled: bool,
    bytes_written: u64,
    filename: Option<String>,
}

impl WebDavUploadResult {
    pub fn cancelled() -> Self {
        Self {
            cancelled: true,
            bytes_written: 0,
            filename: None,
        }
    }
}

impl WebDavDownloadResult {
    pub fn cancelled() -> Self {
        Self {
            cancelled: true,
            bytes_written: 0,
            filename: None,
        }
    }
}

#[derive(Clone)]
pub struct CloudDriveState {
    vault: CloudCredentialVault,
    sessions: Arc<SessionTable>,
}

impl CloudDriveState {
    pub fn new(app_data_dir: PathBuf) -> Self {
        Self {
            vault: CloudCredentialVault::new(app_data_dir),
            sessions: Arc::new(SessionTable::new()),
        }
    }

    /// Returns non-secret profile metadata only.
    pub async fn list_profiles(&self) -> Result<Vec<CloudProfileView>, String> {
        let vault = self.vault.clone();
        run_vault_task(move || vault.list_profiles()).await
    }

    /// Validates a one-shot credential, performs the initial PROPFIND, then
    /// creates a native session. Persistence happens only after the server has
    /// accepted the credentials.
    pub async fn connect_webdav(
        &self,
        request: WebDavConnectRequest,
    ) -> Result<WebDavConnectResponse, String> {
        let WebDavConnectRequest {
            endpoint,
            username,
            password,
            remember,
            label,
        } = request;
        validate_credential_field("账号", &username, MAX_ACCOUNT_BYTES)?;
        validate_credential_field("密码", &password, MAX_PASSWORD_BYTES)?;
        if remember && password.len() > MAX_STORED_PASSWORD_BYTES {
            return Err(format!(
                "需要保存的密码不能超过 {MAX_STORED_PASSWORD_BYTES} 字节；仍可取消保存后临时连接。"
            ));
        }

        let endpoint = normalize_endpoint(&endpoint)?;
        let profile_label = normalize_profile_label(label.as_deref(), &endpoint)?;
        let credentials = WebDavSessionSnapshot {
            endpoint: endpoint.clone(),
            username,
            password: Arc::new(Zeroizing::new(password)),
            profile_id: None,
        };
        let listing = list_directory_for_session(&credentials, None).await?;
        let connection_id = self.sessions.insert(credentials.clone())?;

        let profile = if remember {
            let vault = self.vault.clone();
            let label = profile_label;
            let endpoint = endpoint.to_string();
            let username = credentials.username.clone();
            let password = credentials.password.clone();
            let saved = run_vault_task(move || {
                vault.save_webdav_profile(&label, &endpoint, &username, password.as_ref().as_str())
            })
            .await;
            let saved = match saved {
                Ok(saved) => saved,
                Err(error) => {
                    let _ = self.sessions.remove(&connection_id);
                    return Err(error);
                }
            };

            if let Err(attach_error) = self
                .sessions
                .attach_profile(&connection_id, saved.id.clone())
            {
                let _ = self.sessions.remove(&connection_id);
                let vault = self.vault.clone();
                let profile_id = saved.id.clone();
                let rollback = run_vault_task(move || vault.delete_profile(&profile_id)).await;
                return Err(match rollback {
                    Ok(()) => attach_error,
                    Err(rollback_error) => {
                        format!("{attach_error}；同时无法回滚已保存的云盘账号：{rollback_error}")
                    }
                });
            }
            Some(saved)
        } else {
            None
        };

        Ok(WebDavConnectResponse {
            connection_id,
            profile,
            endpoint: listing.endpoint,
            directory: listing.directory,
            xml: listing.xml,
        })
    }

    /// Loads a password in a blocking vault task, verifies it with PROPFIND,
    /// then retains only a zeroizing native copy for this process session.
    pub async fn connect_cloud_profile(
        &self,
        request: CloudProfileConnectRequest,
    ) -> Result<WebDavConnectResponse, String> {
        validate_canonical_uuid_v4(&request.profile_id, "云盘账号 ID")?;
        let profile_id = request.profile_id;
        let vault = self.vault.clone();
        let lookup_id = profile_id.clone();
        let (profile, password) = run_vault_task(move || {
            let profile = vault
                .list_profiles()?
                .into_iter()
                .find(|profile| profile.id == lookup_id)
                .ok_or_else(|| "找不到该云盘账号。".to_owned())?;
            let password = vault.load_webdav_password(&profile.id)?;
            Ok((profile, password))
        })
        .await?;

        let endpoint = normalize_endpoint(&profile.endpoint)?;
        validate_credential_field("账号", &profile.username, MAX_ACCOUNT_BYTES)?;
        validate_credential_field("密码", password.as_str(), MAX_STORED_PASSWORD_BYTES)?;
        let credentials = WebDavSessionSnapshot {
            endpoint,
            username: profile.username.clone(),
            password: Arc::new(password),
            profile_id: Some(profile_id),
        };
        let listing = list_directory_for_session(&credentials, None).await?;
        let connection_id = self.sessions.insert(credentials)?;

        Ok(WebDavConnectResponse {
            connection_id,
            profile: Some(profile),
            endpoint: listing.endpoint,
            directory: listing.directory,
            xml: listing.xml,
        })
    }

    pub async fn list_directory(
        &self,
        request: WebDavListRequest,
    ) -> Result<WebDavDirectoryResponse, String> {
        let session = self.sessions.resolve(&request.connection_id)?;
        list_directory_for_session(&session, request.directory.as_deref()).await
    }

    /// Disconnecting never forgets a saved account.
    pub fn disconnect(&self, request: WebDavDisconnectRequest) -> Result<(), String> {
        self.sessions.remove(&request.connection_id)
    }

    /// Revoke all live sessions first, then remove the OS credential and its
    /// non-secret metadata. If the vault refuses deletion the sessions remain
    /// revoked, which is the safer partial state.
    pub async fn forget_profile(&self, request: CloudProfileForgetRequest) -> Result<(), String> {
        validate_canonical_uuid_v4(&request.profile_id, "云盘账号 ID")?;
        self.sessions.revoke_profile(&request.profile_id);
        let vault = self.vault.clone();
        run_vault_task(move || vault.delete_profile(&request.profile_id)).await
    }

    /// Validates a download before the host opens a save dialog. The returned
    /// name is only a suggestion; the person still chooses the destination.
    pub fn validated_webdav_download_filename(
        &self,
        request: &WebDavDownloadRequest,
    ) -> Result<String, String> {
        self.normalized_download_request(request)
            .map(|(_, _, filename)| filename)
    }

    /// Streams one explicit remote file to a host-selected `.part` sibling,
    /// then publishes it only after a complete response.
    pub async fn download_webdav_to_path(
        &self,
        request: WebDavDownloadRequest,
        destination: PathBuf,
    ) -> Result<WebDavDownloadResult, String> {
        let (session, remote_url, filename) = self.normalized_download_request(&request)?;
        let destination_parent = destination
            .parent()
            .filter(|parent| parent.is_dir())
            .ok_or_else(|| "所选下载位置的文件夹不存在。".to_owned())?;
        if destination.exists() {
            return Err("所选文件已经存在；为避免覆盖，请使用新的文件名。".to_owned());
        }

        let temporary =
            destination_parent.join(format!(".{filename}.ihub-{}.part", Uuid::new_v4()));
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("无法创建下载临时文件：{error}"))?;

        let transfer = async {
            let response = with_basic_auth(
                webdav_client()?.get(remote_url),
                &session.username,
                session.password.as_ref().as_str(),
            )
            .send()
            .await
            .map_err(|error| format!("WebDAV 下载失败：{}", request_error_message(&error)))?;
            let status = response.status();
            if status.is_redirection() {
                return Err(
                    "WebDAV 文件下载要求跳转；为保护账号密码，iHub 不会跟随跳转。".to_owned(),
                );
            }
            if status.as_u16() == 401 || status.as_u16() == 403 {
                return Err("WebDAV 服务拒绝了账号、应用专用密码或下载权限。".to_owned());
            }
            if !status.is_success() {
                return Err(format!("WebDAV 文件下载返回了 HTTP {}。", status.as_u16()));
            }
            if let Some(length) = response.content_length() {
                if length > MAX_DOWNLOAD_BYTES {
                    return Err(format!(
                        "单次下载上限为 {} GiB。请使用桌面客户端或拆分文件后重试。",
                        MAX_DOWNLOAD_BYTES / (1024 * 1024 * 1024)
                    ));
                }
            }

            let mut response = response;
            let mut bytes_written = 0_u64;
            while let Some(chunk) = response.chunk().await.map_err(|error| {
                format!("读取 WebDAV 文件失败：{}", request_error_message(&error))
            })? {
                bytes_written = bytes_written
                    .checked_add(
                        u64::try_from(chunk.len()).map_err(|_| "下载文件过大。".to_owned())?,
                    )
                    .ok_or_else(|| "下载文件长度溢出。".to_owned())?;
                if bytes_written > MAX_DOWNLOAD_BYTES {
                    return Err(format!(
                        "单次下载上限为 {} GiB。请使用桌面客户端或拆分文件后重试。",
                        MAX_DOWNLOAD_BYTES / (1024 * 1024 * 1024)
                    ));
                }
                output
                    .write_all(&chunk)
                    .map_err(|error| format!("写入下载临时文件失败：{error}"))?;
            }
            output
                .sync_all()
                .map_err(|error| format!("无法完成下载临时文件：{error}"))?;
            Ok(bytes_written)
        }
        .await;
        // Close the handle before publishing. Windows can reject hard-linking
        // while the process still owns an open write handle.
        drop(output);

        match transfer {
            Ok(bytes_written) => {
                // `rename` overwrites on some platforms. A same-directory hard
                // link gives us the promised no-clobber creation semantics.
                if let Err(error) = fs::hard_link(&temporary, &destination) {
                    let _ = fs::remove_file(&temporary);
                    return Err(format!("无法完成下载文件（目标可能已存在）：{error}"));
                }
                let _ = fs::remove_file(&temporary);
                Ok(WebDavDownloadResult {
                    cancelled: false,
                    bytes_written,
                    filename: Some(filename),
                })
            }
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                Err(error)
            }
        }
    }

    /// Checks the connection shape before opening a native upload picker.
    pub fn validate_webdav_upload_request(
        &self,
        request: &WebDavUploadRequest,
    ) -> Result<(), String> {
        let session = self.sessions.resolve(&request.connection_id)?;
        let _ = normalize_directory(&session.endpoint, Some(&request.directory))?;
        Ok(())
    }

    /// Streams a native-picker file to a unique remote staging name, then uses
    /// WebDAV MOVE with `Overwrite: F` to publish it without clobbering.
    pub async fn upload_webdav_from_path(
        &self,
        request: WebDavUploadRequest,
        source: PathBuf,
    ) -> Result<WebDavUploadResult, String> {
        let session = self.sessions.resolve(&request.connection_id)?;
        let directory = normalize_directory(&session.endpoint, Some(&request.directory))?;
        let source_metadata =
            fs::metadata(&source).map_err(|error| format!("无法读取所选上传文件：{error}"))?;
        if !source_metadata.is_file() {
            return Err("所选上传对象不是普通文件。".to_owned());
        }
        if source_metadata.len() > MAX_DOWNLOAD_BYTES {
            return Err(format!(
                "单次上传上限为 {} GiB。请拆分文件后重试。",
                MAX_DOWNLOAD_BYTES / (1024 * 1024 * 1024)
            ));
        }
        let filename = source
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| "所选上传文件名不是有效 UTF-8。".to_owned())?;
        let filename = safe_download_filename(filename)?;
        let target_url = remote_child_url(&directory, &filename)?;
        let staging_filename = format!(".{filename}.ihub-upload-{}.part", Uuid::new_v4());
        let staging_url = remote_child_url(&directory, &staging_filename)?;
        let file = TokioFile::open(&source)
            .await
            .map_err(|error| format!("无法打开所选上传文件：{error}"))?;

        let client = webdav_client()?;
        let upload_response = with_basic_auth(
            client
                .put(staging_url.clone())
                .header(IF_NONE_MATCH, "*")
                .header(CONTENT_LENGTH, source_metadata.len())
                .header(CONTENT_TYPE, "application/octet-stream")
                .body(Body::wrap_stream(ReaderStream::new(file))),
            &session.username,
            session.password.as_ref().as_str(),
        )
        .send()
        .await
        .map_err(|error| format!("WebDAV 上传失败：{}", request_error_message(&error)))?;
        let upload_status = upload_response.status();
        if upload_status.is_redirection() {
            return Err("WebDAV 上传要求跳转；为保护账号密码，iHub 不会跟随跳转。".to_owned());
        }
        if upload_status.as_u16() == 401 || upload_status.as_u16() == 403 {
            return Err("WebDAV 服务拒绝了账号、应用专用密码或上传权限。".to_owned());
        }
        if upload_status.as_u16() == 412 {
            return Err("WebDAV 服务已存在同名临时上传文件；请稍后重试。".to_owned());
        }
        if !upload_status.is_success() {
            return Err(format!(
                "WebDAV 上传返回了 HTTP {}。",
                upload_status.as_u16()
            ));
        }

        let move_response = with_basic_auth(
            client
                .request(
                    Method::from_bytes(b"MOVE")
                        .map_err(|error| format!("无法创建 WebDAV 发布请求：{error}"))?,
                    staging_url.clone(),
                )
                .header("Destination", target_url.as_str())
                .header("Overwrite", "F"),
            &session.username,
            session.password.as_ref().as_str(),
        )
        .send()
        .await;
        let move_response = match move_response {
            Ok(response) => response,
            Err(error) => {
                cleanup_staging_upload(
                    &client,
                    &staging_url,
                    &session.username,
                    session.password.as_ref().as_str(),
                )
                .await;
                return Err(format!(
                    "WebDAV 上传发布失败：{}",
                    request_error_message(&error)
                ));
            }
        };
        let move_status = move_response.status();
        if move_status.as_u16() == 412 {
            cleanup_staging_upload(
                &client,
                &staging_url,
                &session.username,
                session.password.as_ref().as_str(),
            )
            .await;
            return Err("云盘中已存在同名文件；iHub 已拒绝覆盖。请重命名后再上传。".to_owned());
        }
        if move_status.is_redirection() || !move_status.is_success() {
            cleanup_staging_upload(
                &client,
                &staging_url,
                &session.username,
                session.password.as_ref().as_str(),
            )
            .await;
            return Err(format!(
                "WebDAV 上传发布返回了 HTTP {}。",
                move_status.as_u16()
            ));
        }

        Ok(WebDavUploadResult {
            cancelled: false,
            bytes_written: source_metadata.len(),
            filename: Some(filename),
        })
    }

    fn normalized_download_request(
        &self,
        request: &WebDavDownloadRequest,
    ) -> Result<(WebDavSessionSnapshot, Url, String), String> {
        let session = self.sessions.resolve(&request.connection_id)?;
        let remote_url = normalize_remote_file_url(&session.endpoint, &request.remote_url)?;
        let filename = safe_download_filename(&request.suggested_filename)?;
        Ok((session, remote_url, filename))
    }
}

#[derive(Clone)]
struct WebDavSessionSnapshot {
    endpoint: Url,
    username: String,
    password: Arc<Zeroizing<String>>,
    profile_id: Option<String>,
}

struct WebDavSession {
    snapshot: WebDavSessionSnapshot,
    created_at: Duration,
    last_accessed_at: Duration,
}

trait SessionClock: Send + Sync {
    fn now(&self) -> Duration;
}

struct MonotonicClock {
    origin: Instant,
}

impl MonotonicClock {
    fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl SessionClock for MonotonicClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

struct SessionTable {
    entries: Mutex<HashMap<String, WebDavSession>>,
    clock: Arc<dyn SessionClock>,
}

impl SessionTable {
    fn new() -> Self {
        Self::with_clock(Arc::new(MonotonicClock::new()))
    }

    fn with_clock(clock: Arc<dyn SessionClock>) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            clock,
        }
    }

    fn insert(&self, snapshot: WebDavSessionSnapshot) -> Result<String, String> {
        let now = self.clock.now();
        let mut entries = self.lock_entries();
        remove_expired_sessions(&mut entries, now);
        if entries.len() >= MAX_SESSIONS {
            return Err(format!(
                "同时最多保留 {MAX_SESSIONS} 个云盘连接；请先断开一个连接。"
            ));
        }

        let connection_id = loop {
            let candidate = Uuid::new_v4().hyphenated().to_string();
            if !entries.contains_key(&candidate) {
                break candidate;
            }
        };
        entries.insert(
            connection_id.clone(),
            WebDavSession {
                snapshot,
                created_at: now,
                last_accessed_at: now,
            },
        );
        Ok(connection_id)
    }

    fn resolve(&self, connection_id: &str) -> Result<WebDavSessionSnapshot, String> {
        validate_canonical_uuid_v4(connection_id, "云盘连接 ID")?;
        let now = self.clock.now();
        let mut entries = self.lock_entries();
        remove_expired_sessions(&mut entries, now);
        let session = entries
            .get_mut(connection_id)
            .ok_or_else(|| "云盘连接已断开或过期，请重新连接。".to_owned())?;
        session.last_accessed_at = now;
        Ok(session.snapshot.clone())
    }

    fn attach_profile(&self, connection_id: &str, profile_id: String) -> Result<(), String> {
        validate_canonical_uuid_v4(connection_id, "云盘连接 ID")?;
        validate_canonical_uuid_v4(&profile_id, "云盘账号 ID")?;
        let now = self.clock.now();
        let mut entries = self.lock_entries();
        remove_expired_sessions(&mut entries, now);
        let session = entries
            .get_mut(connection_id)
            .ok_or_else(|| "云盘连接在保存账号时已过期，请重新连接。".to_owned())?;
        session.snapshot.profile_id = Some(profile_id);
        session.last_accessed_at = now;
        Ok(())
    }

    fn remove(&self, connection_id: &str) -> Result<(), String> {
        validate_canonical_uuid_v4(connection_id, "云盘连接 ID")?;
        self.lock_entries().remove(connection_id);
        Ok(())
    }

    fn revoke_profile(&self, profile_id: &str) {
        self.lock_entries()
            .retain(|_, session| session.snapshot.profile_id.as_deref() != Some(profile_id));
    }

    fn lock_entries(&self) -> MutexGuard<'_, HashMap<String, WebDavSession>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn remove_expired_sessions(entries: &mut HashMap<String, WebDavSession>, now: Duration) {
    entries.retain(|_, session| {
        elapsed_since(now, session.last_accessed_at) < SESSION_IDLE_TTL
            && elapsed_since(now, session.created_at) < SESSION_HARD_TTL
    });
}

fn elapsed_since(now: Duration, then: Duration) -> Duration {
    now.checked_sub(then).unwrap_or_default()
}

async fn run_vault_task<T, F>(task: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(task)
        .await
        .map_err(|error| format!("云盘凭据任务未完成：{error}"))?
}

async fn list_directory_for_session(
    session: &WebDavSessionSnapshot,
    raw_directory: Option<&str>,
) -> Result<WebDavDirectoryResponse, String> {
    let directory = normalize_directory(&session.endpoint, raw_directory)?;
    let client = webdav_client()?;
    let request_builder = client
        .request(
            Method::from_bytes(b"PROPFIND")
                .map_err(|error| format!("无法创建 WebDAV 请求：{error}"))?,
            directory.clone(),
        )
        .header("Depth", "1")
        .header(ACCEPT, "application/xml, text/xml, */*;q=0.1")
        .header(CONTENT_TYPE, "application/xml; charset=utf-8")
        .body("<?xml version=\"1.0\" encoding=\"utf-8\"?><propfind xmlns=\"DAV:\"><prop><resourcetype/><getcontentlength/><getcontenttype/><getlastmodified/></prop></propfind>");
    // Do not turn an anonymous server into an authenticated request with the
    // synthetic empty Basic credential (`Og==`).
    let request_builder = with_basic_auth(
        request_builder,
        &session.username,
        session.password.as_ref().as_str(),
    );
    let response = request_builder
        .send()
        .await
        .map_err(|error| format!("WebDAV 目录请求失败：{}", request_error_message(&error)))?;

    let status = response.status();
    if status.is_redirection() {
        return Err(
            "WebDAV 服务要求跳转；为保护账号密码，iHub 不会跟随跳转。请填写最终 HTTPS 地址。"
                .to_owned(),
        );
    }
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err("WebDAV 服务拒绝了账号、应用专用密码或访问权限。".to_owned());
    }
    if !(status.is_success() || status.as_u16() == 207) {
        return Err(format!("WebDAV 服务返回了 HTTP {}。", status.as_u16()));
    }
    if let Some(length) = response.content_length() {
        if length > MAX_DIRECTORY_XML_BYTES as u64 {
            return Err(format!(
                "WebDAV 目录响应超过 {} MiB 上限。请缩小目录范围后重试。",
                MAX_DIRECTORY_XML_BYTES / (1024 * 1024)
            ));
        }
    }

    let xml = read_limited_xml(response).await?;
    Ok(WebDavDirectoryResponse {
        endpoint: session.endpoint.to_string(),
        directory: directory.to_string(),
        xml,
    })
}

async fn cleanup_staging_upload(client: &Client, url: &Url, username: &str, password: &str) {
    let _ = with_basic_auth(client.delete(url.clone()), username, password)
        .send()
        .await;
}

fn remote_child_url(directory: &Url, filename: &str) -> Result<Url, String> {
    let mut target = directory.clone();
    target
        .path_segments_mut()
        .map_err(|_| "WebDAV 目录不能用于文件传输。".to_owned())?
        .pop_if_empty()
        .push(filename);
    Ok(target)
}

fn webdav_client() -> Result<Client, String> {
    // `reqwest` deliberately uses `rustls-no-provider`; iHub installs the
    // lightweight ring provider once for this process.
    let _ = rustls::crypto::ring::default_provider().install_default();
    Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .redirect(Policy::none())
        // Never silently route a cloud password through ambient proxy config.
        .no_proxy()
        .user_agent("iHub Cloud Drive/0.1")
        .build()
        .map_err(|error| format!("无法创建 WebDAV 连接：{error}"))
}

fn with_basic_auth(
    request_builder: reqwest::RequestBuilder,
    username: &str,
    password: &str,
) -> reqwest::RequestBuilder {
    if username.is_empty() && password.is_empty() {
        request_builder
    } else {
        request_builder.basic_auth(username, Some(password))
    }
}

async fn read_limited_xml(mut response: reqwest::Response) -> Result<String, String> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        format!(
            "读取 WebDAV 目录响应失败：{}",
            request_error_message(&error)
        )
    })? {
        let next_length = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| "WebDAV 目录响应长度溢出。".to_owned())?;
        if next_length > MAX_DIRECTORY_XML_BYTES {
            return Err(format!(
                "WebDAV 目录响应超过 {} MiB 上限。请缩小目录范围后重试。",
                MAX_DIRECTORY_XML_BYTES / (1024 * 1024)
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|_| "WebDAV 服务返回的目录 XML 不是 UTF-8。".to_owned())
}

fn request_error_message(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "连接超时。".to_owned()
    } else if error.is_connect() {
        "无法连接到该 WebDAV 服务。".to_owned()
    } else {
        "请求未完成。".to_owned()
    }
}

fn validate_credential_field(label: &str, value: &str, maximum: usize) -> Result<(), String> {
    if value.len() > maximum {
        return Err(format!("{label}不能超过 {maximum} 字节。"));
    }
    if value.contains('\0') {
        return Err(format!("{label}不能包含空字符。"));
    }
    Ok(())
}

fn normalize_profile_label(raw: Option<&str>, endpoint: &Url) -> Result<String, String> {
    let fallback = endpoint.host_str().unwrap_or("WebDAV");
    let label = raw.unwrap_or(fallback).trim();
    if label.is_empty() || label.len() > MAX_PROFILE_LABEL_BYTES {
        return Err(format!(
            "云盘名称必须为 1 到 {MAX_PROFILE_LABEL_BYTES} 个 UTF-8 字节。"
        ));
    }
    if label.chars().any(char::is_control) {
        return Err("云盘名称不能包含控制字符。".to_owned());
    }
    Ok(label.to_owned())
}

fn validate_canonical_uuid_v4(value: &str, label: &str) -> Result<(), String> {
    let id = Uuid::parse_str(value).map_err(|_| format!("{label}无效。"))?;
    if id.get_version() != Some(Version::Random) || id.hyphenated().to_string() != value {
        return Err(format!("{label}必须是规范的 UUID v4。"));
    }
    Ok(())
}

fn normalize_remote_file_url(root: &Url, raw: &str) -> Result<Url, String> {
    if raw.len() > MAX_ENDPOINT_BYTES {
        return Err("WebDAV 文件地址过长。".to_owned());
    }
    let remote = Url::parse(raw).map_err(|_| "WebDAV 文件地址无效；请重新浏览目录。".to_owned())?;
    if remote.username() != ""
        || remote.password().is_some()
        || remote.query().is_some()
        || remote.fragment().is_some()
    {
        return Err("WebDAV 文件地址包含不允许的凭据或查询信息；请重新浏览目录。".to_owned());
    }
    if remote.origin() != root.origin() || !remote.path().starts_with(root.path()) {
        return Err("WebDAV 文件必须位于当前连接根目录内。".to_owned());
    }
    Ok(remote)
}

fn safe_download_filename(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok("ihub-download".to_owned());
    }
    let mut filename = value
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
            {
                '_'
            } else {
                character
            }
        })
        .collect::<String>();
    filename = filename.trim().trim_end_matches('.').to_owned();
    if filename.is_empty() || filename == "." || filename == ".." {
        return Ok("ihub-download".to_owned());
    }
    while filename.len() > 180 {
        filename.pop();
    }
    if filename.is_empty() {
        return Ok("ihub-download".to_owned());
    }
    if Path::new(&filename).components().count() != 1 {
        return Err("下载文件名无效。".to_owned());
    }
    Ok(filename)
}

fn normalize_endpoint(raw: &str) -> Result<Url, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("请输入 WebDAV 地址。".to_owned());
    }
    if raw.len() > MAX_ENDPOINT_BYTES {
        return Err(format!("WebDAV 地址不能超过 {MAX_ENDPOINT_BYTES} 字节。"));
    }
    let mut endpoint =
        Url::parse(raw).map_err(|_| "WebDAV 地址不是有效 URL。请包含 https://。".to_owned())?;
    if endpoint.username() != "" || endpoint.password().is_some() {
        return Err("请不要把账号或密码写进 WebDAV 地址；请使用单独的账号和密码字段。".to_owned());
    }
    if endpoint.query().is_some() || endpoint.fragment().is_some() {
        return Err("WebDAV 地址不能包含查询参数或 # 片段。".to_owned());
    }
    let host = endpoint
        .host_str()
        .ok_or_else(|| "WebDAV 地址必须包含服务器主机名。".to_owned())?;
    match endpoint.scheme() {
        "https" => {}
        "http" if is_loopback_host(host) => {}
        "http" => return Err("为保护账号密码，非本机 WebDAV 必须使用 HTTPS。".to_owned()),
        _ => return Err("WebDAV 仅支持 HTTPS；HTTP 只允许本机调试服务。".to_owned()),
    }
    ensure_directory_path(&mut endpoint);
    Ok(endpoint)
}

fn normalize_directory(root: &Url, raw: Option<&str>) -> Result<Url, String> {
    let Some(raw) = raw else {
        return Ok(root.clone());
    };
    if raw.len() > MAX_ENDPOINT_BYTES {
        return Err("WebDAV 目录地址过长。".to_owned());
    }
    let mut directory =
        Url::parse(raw).map_err(|_| "WebDAV 目录地址无效；请重新连接。".to_owned())?;
    if directory.username() != ""
        || directory.password().is_some()
        || directory.query().is_some()
        || directory.fragment().is_some()
    {
        return Err("WebDAV 目录地址包含不允许的凭据或查询信息；请重新连接。".to_owned());
    }
    ensure_directory_path(&mut directory);
    if directory.origin() != root.origin() || !directory.path().starts_with(root.path()) {
        return Err("WebDAV 目录必须位于当前连接根目录内。".to_owned());
    }
    Ok(directory)
}

fn ensure_directory_path(url: &mut Url) {
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
}

fn is_loopback_host(host: &str) -> bool {
    host.trim_end_matches('.').eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .map(|address| address.is_loopback())
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::{Read, Write},
        net::TcpListener,
        sync::atomic::{AtomicU64, Ordering},
        thread,
    };

    use super::*;

    struct ManualClock {
        seconds: AtomicU64,
    }

    impl ManualClock {
        fn new() -> Self {
            Self {
                seconds: AtomicU64::new(0),
            }
        }

        fn advance(&self, duration: Duration) {
            self.seconds.fetch_add(duration.as_secs(), Ordering::SeqCst);
        }
    }

    impl SessionClock for ManualClock {
        fn now(&self) -> Duration {
            Duration::from_secs(self.seconds.load(Ordering::SeqCst))
        }
    }

    fn session(endpoint: &str, profile_id: Option<String>) -> WebDavSessionSnapshot {
        WebDavSessionSnapshot {
            endpoint: normalize_endpoint(endpoint).expect("valid test endpoint"),
            username: String::new(),
            password: Arc::new(Zeroizing::new(String::new())),
            profile_id,
        }
    }

    fn test_state(endpoint: &str) -> (CloudDriveState, String) {
        let clock = Arc::new(ManualClock::new());
        let sessions = Arc::new(SessionTable::with_clock(clock));
        let connection_id = sessions
            .insert(session(endpoint, None))
            .expect("test session inserted");
        let app_data_dir =
            std::env::temp_dir().join(format!("ihub-cloud-state-{}", Uuid::new_v4()));
        (
            CloudDriveState {
                vault: CloudCredentialVault::new(app_data_dir),
                sessions,
            },
            connection_id,
        )
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1_024];
        loop {
            let count = stream.read(&mut chunk).expect("read local test request");
            assert!(count > 0, "test client closed before its headers arrived");
            request.extend_from_slice(&chunk[..count]);
            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                return String::from_utf8(request).expect("local request is HTTP text");
            }
        }
    }

    fn read_http_request_with_body(stream: &mut std::net::TcpStream) -> Vec<u8> {
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1_024];
        let mut expected_length = None;
        loop {
            let count = stream.read(&mut chunk).expect("read local test request");
            assert!(count > 0, "test client closed before its request arrived");
            request.extend_from_slice(&chunk[..count]);
            if expected_length.is_none() {
                if let Some(header_end) =
                    request.windows(4).position(|window| window == b"\r\n\r\n")
                {
                    let headers = String::from_utf8_lossy(&request[..header_end + 4]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length: ")
                                .or_else(|| line.strip_prefix("Content-Length: "))
                        })
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .expect("upload request includes content length");
                    expected_length = Some(header_end + 4 + content_length);
                }
            }
            if expected_length.is_some_and(|length| request.len() >= length) {
                return request;
            }
        }
    }

    #[test]
    fn normalizes_https_root_and_rejects_escape() {
        let root = normalize_endpoint("https://dav.example.test/remote.php/dav/files/neo")
            .expect("valid root");
        assert_eq!(
            root.as_str(),
            "https://dav.example.test/remote.php/dav/files/neo/"
        );
        let child = normalize_directory(
            &root,
            Some("https://dav.example.test/remote.php/dav/files/neo/docs"),
        )
        .expect("valid child");
        assert_eq!(
            child.as_str(),
            "https://dav.example.test/remote.php/dav/files/neo/docs/"
        );
        assert!(normalize_directory(&root, Some("https://dav.example.test/other-root/")).is_err());
    }

    #[test]
    fn rejects_insecure_remote_and_credentials_in_urls() {
        assert!(normalize_endpoint("http://nas.example.test/dav/").is_err());
        assert!(normalize_endpoint("https://neo:secret@dav.example.test/dav/").is_err());
        assert!(normalize_endpoint("http://127.0.0.1:1900/dav/").is_ok());
    }

    #[test]
    fn makes_remote_download_names_portable() {
        assert_eq!(
            safe_download_filename("report:2026?.txt").expect("sanitized name"),
            "report_2026_.txt"
        );
        assert_eq!(
            safe_download_filename("...").expect("fallback name"),
            "ihub-download"
        );
    }

    #[test]
    fn session_table_enforces_limit_and_cleans_idle_sessions() {
        let clock = Arc::new(ManualClock::new());
        let table = SessionTable::with_clock(clock.clone());
        let mut ids = Vec::new();
        for _ in 0..MAX_SESSIONS {
            ids.push(
                table
                    .insert(session("https://dav.example.test/root/", None))
                    .unwrap(),
            );
        }
        assert!(table
            .insert(session("https://dav.example.test/root/", None))
            .is_err());

        clock.advance(SESSION_IDLE_TTL - Duration::from_secs(1));
        table.resolve(&ids[0]).expect("first session touched");
        clock.advance(Duration::from_secs(2));
        assert!(table.resolve(&ids[0]).is_ok());
        assert!(table.resolve(&ids[1]).is_err());
        assert!(table
            .insert(session("https://dav.example.test/root/", None))
            .is_ok());
    }

    #[test]
    fn session_hard_ttl_wins_over_repeated_access() {
        let clock = Arc::new(ManualClock::new());
        let table = SessionTable::with_clock(clock.clone());
        let id = table
            .insert(session("https://dav.example.test/root/", None))
            .unwrap();
        for _ in 0..16 {
            clock.advance(Duration::from_secs(30 * 60 - 1));
            table.resolve(&id).expect("idle TTL refreshed");
        }
        clock.advance(Duration::from_secs(16));
        assert!(table.resolve(&id).is_err());
    }

    #[test]
    fn disconnect_is_idempotent_and_forget_revokes_only_matching_profile() {
        let table = SessionTable::with_clock(Arc::new(ManualClock::new()));
        let profile_id = Uuid::new_v4().hyphenated().to_string();
        let other_profile_id = Uuid::new_v4().hyphenated().to_string();
        let matching = table
            .insert(session(
                "https://dav.example.test/root/",
                Some(profile_id.clone()),
            ))
            .unwrap();
        let other = table
            .insert(session(
                "https://dav.example.test/root/",
                Some(other_profile_id),
            ))
            .unwrap();

        table.revoke_profile(&profile_id);
        assert!(table.resolve(&matching).is_err());
        assert!(table.resolve(&other).is_ok());
        table.remove(&other).unwrap();
        table.remove(&other).unwrap();
        assert!(table.remove("not-a-uuid").is_err());
    }

    #[test]
    fn sends_propfind_only_to_explicit_loopback_webdav_root() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local test WebDAV");
        let address = listener.local_addr().expect("local address");
        let body = "<?xml version=\"1.0\"?><d:multistatus xmlns:d=\"DAV:\"></d:multistatus>";
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept local request");
            let request = read_http_request(&mut stream);
            assert!(request.starts_with("PROPFIND /dav/ HTTP/1.1"));
            let request_lower = request.to_ascii_lowercase();
            assert!(request_lower.contains("\r\ndepth: 1"));
            assert!(!request_lower.contains("\r\nauthorization:"));
            let response = format!(
                "HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write local response");
        });

        let app_data_dir =
            std::env::temp_dir().join(format!("ihub-cloud-connect-{}", Uuid::new_v4()));
        let state = CloudDriveState::new(app_data_dir);
        let response = tauri::async_runtime::block_on(state.connect_webdav(WebDavConnectRequest {
            endpoint: format!("http://{address}/dav/"),
            username: String::new(),
            password: String::new(),
            remember: false,
            label: None,
        }))
        .expect("loopback WebDAV connect succeeds");
        assert_eq!(response.endpoint, format!("http://{address}/dav/"));
        assert!(response.xml.contains("multistatus"));
        server.join().expect("local WebDAV server completes");
    }

    #[test]
    fn streams_a_download_to_a_new_local_destination() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local download server");
        let address = listener.local_addr().expect("local address");
        let payload = b"iHub cloud drive test payload";
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept local download");
            let request = read_http_request(&mut stream);
            assert!(request.starts_with("GET /dav/report.txt HTTP/1.1"));
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                payload.len()
            );
            stream
                .write_all(headers.as_bytes())
                .expect("write local headers");
            stream.write_all(payload).expect("write local payload");
        });

        let (state, connection_id) = test_state(&format!("http://{address}/dav/"));
        let temporary_directory =
            std::env::temp_dir().join(format!("ihub-cloud-drive-test-{}", Uuid::new_v4()));
        fs::create_dir(&temporary_directory).expect("create test download directory");
        let destination = temporary_directory.join("report.txt");
        let result = tauri::async_runtime::block_on(state.download_webdav_to_path(
            WebDavDownloadRequest {
                connection_id,
                remote_url: format!("http://{address}/dav/report.txt"),
                suggested_filename: "report.txt".to_owned(),
            },
            destination.clone(),
        ))
        .expect("loopback WebDAV download succeeds");
        assert!(!result.cancelled);
        assert_eq!(result.bytes_written, payload.len() as u64);
        assert_eq!(fs::read(&destination).expect("downloaded bytes"), payload);
        server.join().expect("local download server completes");
        fs::remove_dir_all(temporary_directory).expect("remove test download directory");
    }

    #[test]
    fn streams_upload_to_staging_then_publishes_without_overwrite() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local upload server");
        let address = listener.local_addr().expect("local address");
        let payload = b"iHub staged upload payload";
        let server = thread::spawn(move || {
            let (mut upload_stream, _) = listener.accept().expect("accept upload");
            let upload = read_http_request_with_body(&mut upload_stream);
            let header_end = upload
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .expect("upload header end");
            let upload_headers = String::from_utf8_lossy(&upload[..header_end + 4]);
            let upload_headers_lower = upload_headers.to_ascii_lowercase();
            assert!(upload_headers.starts_with("PUT /dav/.report.txt.ihub-upload-"));
            assert!(upload_headers_lower.contains("\r\nif-none-match: *"));
            assert_eq!(&upload[header_end + 4..], payload);
            upload_stream
                .write_all(
                    b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("accept staged upload");

            let (mut move_stream, _) = listener.accept().expect("accept WebDAV move");
            let publish = read_http_request(&mut move_stream);
            let publish_lower = publish.to_ascii_lowercase();
            assert!(publish.starts_with("MOVE /dav/.report.txt.ihub-upload-"));
            assert!(publish_lower.contains("\r\noverwrite: f"));
            assert!(publish_lower
                .contains(&format!("\r\ndestination: http://{address}/dav/report.txt")));
            move_stream
                .write_all(
                    b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("accept staged publish");
        });

        let (state, connection_id) = test_state(&format!("http://{address}/dav/"));
        let temporary_directory =
            std::env::temp_dir().join(format!("ihub-cloud-upload-test-{}", Uuid::new_v4()));
        fs::create_dir(&temporary_directory).expect("create test upload directory");
        let source = temporary_directory.join("report.txt");
        fs::write(&source, payload).expect("write upload source");
        let result = tauri::async_runtime::block_on(state.upload_webdav_from_path(
            WebDavUploadRequest {
                connection_id,
                directory: format!("http://{address}/dav/"),
            },
            source,
        ))
        .expect("loopback WebDAV upload succeeds");
        assert!(!result.cancelled);
        assert_eq!(result.bytes_written, payload.len() as u64);
        assert_eq!(result.filename.as_deref(), Some("report.txt"));
        server.join().expect("local upload server completes");
        fs::remove_dir_all(temporary_directory).expect("remove test upload directory");
    }
}
