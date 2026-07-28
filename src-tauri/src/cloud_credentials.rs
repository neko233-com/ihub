//! OS-backed credentials for Cloud Drive profiles.
//!
//! Profile metadata is deliberately separated from secrets: the bounded JSON
//! file in app data is safe to return to the renderer, while passwords stay in
//! Windows Credential Manager or macOS Keychain. Future OAuth adapters can use
//! a distinct `secretKind` without accidentally accepting a WebDAV password as
//! a token.

use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{ErrorKind, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use uuid::{Uuid, Version};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

const PROFILE_FILE_NAME: &str = "cloud-profiles-v1.json";
const PROFILE_SCHEMA_VERSION: u32 = 1;
const SECRET_SCHEMA_VERSION: u32 = 1;
const KEYRING_SERVICE: &str = "com.neko233.ihub.cloud-drive.v1";
const WEBDAV_PROVIDER: &str = "webdav";
const WEBDAV_PASSWORD_KIND: &str = "webdav-basic-password";

const MAX_PROFILES: usize = 32;
const MAX_PROFILE_FILE_BYTES: usize = 256 * 1024;
const MAX_SECRET_ENVELOPE_BYTES: usize = 2_400;
const MAX_LABEL_BYTES: usize = 96;
const MAX_ENDPOINT_BYTES: usize = 2_048;
const MAX_USERNAME_BYTES: usize = 1_024;
// Windows generic credentials allow 2,560 bytes. Keep room for the strict JSON
// envelope and reject the encoded envelope again before it reaches keyring.
const MAX_STORED_PASSWORD_BYTES: usize = 2_048;

const TEMPORARY_FILE_PREFIX: &str = ".cloud-profiles-";
const TEMPORARY_FILE_SUFFIX: &str = ".tmp";
const BACKUP_FILE_SUFFIX: &str = ".backup";

/// Non-secret Cloud Drive profile data that may be returned to the renderer.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CloudProfileView {
    pub id: String,
    pub provider: String,
    pub label: String,
    pub endpoint: String,
    pub username: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedCloudProfiles {
    schema_version: u32,
    profiles: Vec<CloudProfileView>,
}

#[derive(Debug, Deserialize, Serialize, Zeroize, ZeroizeOnDrop)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SecretEnvelope {
    schema_version: u32,
    provider: String,
    secret_kind: String,
    profile_id: String,
    secret: String,
}

#[derive(Debug)]
enum CredentialBackendError {
    NoEntry,
    Other(String),
}

trait CredentialBackend: Send + Sync {
    fn set_secret(
        &self,
        service: &str,
        user: &str,
        secret: &[u8],
    ) -> Result<(), CredentialBackendError>;

    fn get_secret(
        &self,
        service: &str,
        user: &str,
    ) -> Result<Zeroizing<Vec<u8>>, CredentialBackendError>;

    fn delete_secret(&self, service: &str, user: &str) -> Result<(), CredentialBackendError>;
}

#[cfg(any(windows, target_os = "macos"))]
struct PlatformCredentialBackend;

#[cfg(any(windows, target_os = "macos"))]
impl PlatformCredentialBackend {
    fn entry(service: &str, user: &str) -> Result<keyring::Entry, CredentialBackendError> {
        keyring::Entry::new(service, user).map_err(map_keyring_error)
    }
}

#[cfg(any(windows, target_os = "macos"))]
impl CredentialBackend for PlatformCredentialBackend {
    fn set_secret(
        &self,
        service: &str,
        user: &str,
        secret: &[u8],
    ) -> Result<(), CredentialBackendError> {
        Self::entry(service, user)?
            .set_secret(secret)
            .map_err(map_keyring_error)
    }

    fn get_secret(
        &self,
        service: &str,
        user: &str,
    ) -> Result<Zeroizing<Vec<u8>>, CredentialBackendError> {
        Self::entry(service, user)?
            .get_secret()
            .map(Zeroizing::new)
            .map_err(map_keyring_error)
    }

    fn delete_secret(&self, service: &str, user: &str) -> Result<(), CredentialBackendError> {
        Self::entry(service, user)?
            .delete_credential()
            .map_err(map_keyring_error)
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn map_keyring_error(error: keyring::Error) -> CredentialBackendError {
    match error {
        keyring::Error::NoEntry => CredentialBackendError::NoEntry,
        error => CredentialBackendError::Other(error.to_string()),
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
struct UnsupportedCredentialBackend;

#[cfg(not(any(windows, target_os = "macos")))]
impl CredentialBackend for UnsupportedCredentialBackend {
    fn set_secret(
        &self,
        _service: &str,
        _user: &str,
        _secret: &[u8],
    ) -> Result<(), CredentialBackendError> {
        Err(CredentialBackendError::Other(
            "Cloud Drive credential storage is supported only on Windows and macOS.".to_owned(),
        ))
    }

    fn get_secret(
        &self,
        _service: &str,
        _user: &str,
    ) -> Result<Zeroizing<Vec<u8>>, CredentialBackendError> {
        Err(CredentialBackendError::Other(
            "Cloud Drive credential storage is supported only on Windows and macOS.".to_owned(),
        ))
    }

    fn delete_secret(&self, _service: &str, _user: &str) -> Result<(), CredentialBackendError> {
        Err(CredentialBackendError::Other(
            "Cloud Drive credential storage is supported only on Windows and macOS.".to_owned(),
        ))
    }
}

/// Serialized profile/credential operations for one Cloud Drive vault.
///
/// The shared lock is intentional. Windows Credential Manager has observable
/// ordering between writes and deletes, so clones of one vault must not race a
/// metadata commit against a credential mutation.
#[derive(Clone)]
pub struct CloudCredentialVault {
    metadata_path: Arc<PathBuf>,
    backend: Arc<dyn CredentialBackend>,
    operation_lock: Arc<Mutex<()>>,
}

impl CloudCredentialVault {
    pub fn new(app_data_dir: PathBuf) -> Self {
        #[cfg(any(windows, target_os = "macos"))]
        let backend: Arc<dyn CredentialBackend> = Arc::new(PlatformCredentialBackend);
        #[cfg(not(any(windows, target_os = "macos")))]
        let backend: Arc<dyn CredentialBackend> = Arc::new(UnsupportedCredentialBackend);

        Self::with_backend(app_data_dir.join(PROFILE_FILE_NAME), backend)
    }

    fn with_backend(metadata_path: PathBuf, backend: Arc<dyn CredentialBackend>) -> Self {
        Self {
            metadata_path: Arc::new(metadata_path),
            backend,
            operation_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Lists validated metadata only. No credential backend read occurs.
    pub fn list_profiles(&self) -> Result<Vec<CloudProfileView>, String> {
        let _guard = self.lock_operations();
        Ok(load_profile_state(&self.metadata_path)?.profiles)
    }

    /// Saves a new WebDAV profile and its password as one recoverable
    /// transaction.
    pub fn save_webdav_profile(
        &self,
        label: &str,
        canonical_endpoint: &str,
        username: &str,
        password: &str,
    ) -> Result<CloudProfileView, String> {
        validate_field("名称", label, 1, MAX_LABEL_BYTES)?;
        validate_field("WebDAV 地址", canonical_endpoint, 1, MAX_ENDPOINT_BYTES)?;
        validate_field("账号", username, 0, MAX_USERNAME_BYTES)?;
        validate_field("密码", password, 0, MAX_STORED_PASSWORD_BYTES)?;

        let _guard = self.lock_operations();
        let mut state = load_profile_state(&self.metadata_path)?;
        if state.profiles.len() >= MAX_PROFILES {
            return Err(format!("云盘账号最多保存 {MAX_PROFILES} 个。"));
        }

        let id = Uuid::new_v4().hyphenated().to_string();
        let now = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let profile = CloudProfileView {
            id: id.clone(),
            provider: WEBDAV_PROVIDER.to_owned(),
            label: label.to_owned(),
            endpoint: canonical_endpoint.to_owned(),
            username: username.to_owned(),
            created_at: now.clone(),
            updated_at: now,
        };

        let envelope = SecretEnvelope {
            schema_version: SECRET_SCHEMA_VERSION,
            provider: WEBDAV_PROVIDER.to_owned(),
            secret_kind: WEBDAV_PASSWORD_KIND.to_owned(),
            profile_id: id.clone(),
            secret: password.to_owned(),
        };
        let encoded = Zeroizing::new(
            serde_json::to_vec(&envelope).map_err(|error| format!("无法编码云盘凭据：{error}"))?,
        );
        if encoded.len() > MAX_SECRET_ENVELOPE_BYTES {
            return Err(format!(
                "云盘凭据封装超过 {MAX_SECRET_ENVELOPE_BYTES} 字节的系统上限。"
            ));
        }

        self.backend
            .set_secret(KEYRING_SERVICE, &id, encoded.as_slice())
            .map_err(credential_write_error)?;

        state.profiles.push(profile.clone());
        if let Err(metadata_error) = save_profile_state(&self.metadata_path, &state) {
            let rollback = self.backend.delete_secret(KEYRING_SERVICE, &id);
            return Err(match rollback {
                Ok(()) | Err(CredentialBackendError::NoEntry) => metadata_error,
                Err(CredentialBackendError::Other(rollback_error)) => {
                    format!("{metadata_error}；同时无法回滚系统凭据：{rollback_error}")
                }
            });
        }

        Ok(profile)
    }

    /// Loads and verifies one WebDAV password without exposing the serialized
    /// envelope or raw keyring bytes to the renderer.
    pub fn load_webdav_password(&self, profile_id: &str) -> Result<Zeroizing<String>, String> {
        validate_profile_id(profile_id)?;

        let _guard = self.lock_operations();
        let state = load_profile_state(&self.metadata_path)?;
        let profile = state
            .profiles
            .iter()
            .find(|profile| profile.id == profile_id)
            .ok_or_else(|| "找不到该云盘账号。".to_owned())?;
        if profile.provider != WEBDAV_PROVIDER {
            return Err("该云盘账号不是 WebDAV 类型。".to_owned());
        }

        let encoded = match self.backend.get_secret(KEYRING_SERVICE, profile_id) {
            Ok(encoded) => encoded,
            Err(CredentialBackendError::NoEntry) => {
                return Err("系统凭据库中找不到该云盘密码。".to_owned());
            }
            Err(CredentialBackendError::Other(error)) => {
                return Err(format!("无法读取系统凭据库：{error}"));
            }
        };
        if encoded.len() > MAX_SECRET_ENVELOPE_BYTES {
            return Err("系统凭据库中的云盘凭据超过安全大小上限。".to_owned());
        }

        let mut envelope: SecretEnvelope = serde_json::from_slice(encoded.as_slice())
            .map_err(|error| format!("系统凭据库中的云盘凭据无效：{error}"))?;
        if envelope.schema_version != SECRET_SCHEMA_VERSION
            || envelope.provider != WEBDAV_PROVIDER
            || envelope.secret_kind != WEBDAV_PASSWORD_KIND
            || envelope.profile_id != profile_id
        {
            return Err("系统凭据库中的云盘凭据与账号不匹配。".to_owned());
        }
        validate_profile_id(&envelope.profile_id)?;
        validate_field("密码", &envelope.secret, 0, MAX_STORED_PASSWORD_BYTES)?;

        Ok(Zeroizing::new(std::mem::take(&mut envelope.secret)))
    }

    /// Deletes the OS credential first, then commits the metadata removal.
    /// A missing credential is already the desired state and is idempotent.
    pub fn delete_profile(&self, profile_id: &str) -> Result<(), String> {
        validate_profile_id(profile_id)?;

        let _guard = self.lock_operations();
        let mut state = load_profile_state(&self.metadata_path)?;
        match self.backend.delete_secret(KEYRING_SERVICE, profile_id) {
            Ok(()) | Err(CredentialBackendError::NoEntry) => {}
            Err(CredentialBackendError::Other(error)) => {
                return Err(format!("无法删除系统凭据：{error}"));
            }
        }

        let original_len = state.profiles.len();
        state.profiles.retain(|profile| profile.id != profile_id);
        if state.profiles.len() != original_len {
            save_profile_state(&self.metadata_path, &state)?;
        }
        Ok(())
    }

    fn lock_operations(&self) -> MutexGuard<'_, ()> {
        self.operation_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn credential_write_error(error: CredentialBackendError) -> String {
    match error {
        CredentialBackendError::NoEntry => "系统凭据库拒绝保存云盘凭据。".to_owned(),
        CredentialBackendError::Other(error) => {
            format!("无法保存云盘凭据到系统凭据库：{error}")
        }
    }
}

fn empty_profile_state() -> PersistedCloudProfiles {
    PersistedCloudProfiles {
        schema_version: PROFILE_SCHEMA_VERSION,
        profiles: Vec::new(),
    }
}

fn validate_field(
    label: &str,
    value: &str,
    minimum_bytes: usize,
    maximum_bytes: usize,
) -> Result<(), String> {
    if value.len() < minimum_bytes || value.len() > maximum_bytes {
        return Err(format!(
            "{label}必须为 {minimum_bytes} 到 {maximum_bytes} 个 UTF-8 字节。"
        ));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{label}不能包含控制字符。"));
    }
    Ok(())
}

fn validate_profile_id(profile_id: &str) -> Result<(), String> {
    let id = Uuid::parse_str(profile_id).map_err(|_| "云盘账号 ID 无效。".to_owned())?;
    if id.get_version() != Some(Version::Random) || id.hyphenated().to_string() != profile_id {
        return Err("云盘账号 ID 必须是规范的 UUID v4。".to_owned());
    }
    Ok(())
}

fn validate_profile(profile: &CloudProfileView) -> Result<(), String> {
    validate_profile_id(&profile.id)?;
    if profile.provider != WEBDAV_PROVIDER {
        return Err("云盘账号包含不受支持的 provider。".to_owned());
    }
    validate_field("名称", &profile.label, 1, MAX_LABEL_BYTES)?;
    validate_field("WebDAV 地址", &profile.endpoint, 1, MAX_ENDPOINT_BYTES)?;
    validate_field("账号", &profile.username, 0, MAX_USERNAME_BYTES)?;
    validate_timestamp("创建时间", &profile.created_at)?;
    validate_timestamp("更新时间", &profile.updated_at)?;

    let created = DateTime::parse_from_rfc3339(&profile.created_at)
        .map_err(|_| "云盘账号的创建时间无效。".to_owned())?;
    let updated = DateTime::parse_from_rfc3339(&profile.updated_at)
        .map_err(|_| "云盘账号的更新时间无效。".to_owned())?;
    if updated < created {
        return Err("云盘账号的更新时间早于创建时间。".to_owned());
    }
    Ok(())
}

fn validate_timestamp(label: &str, value: &str) -> Result<(), String> {
    validate_field(label, value, 1, 64)?;
    DateTime::parse_from_rfc3339(value)
        .map(|_| ())
        .map_err(|_| format!("{label}不是有效的 RFC 3339 时间。"))
}

fn validate_profile_state(state: &PersistedCloudProfiles) -> Result<(), String> {
    if state.schema_version != PROFILE_SCHEMA_VERSION {
        return Err("云盘账号文件版本不受支持。".to_owned());
    }
    if state.profiles.len() > MAX_PROFILES {
        return Err(format!("云盘账号文件超过 {MAX_PROFILES} 个账号的上限。"));
    }

    let mut ids = HashSet::with_capacity(state.profiles.len());
    for profile in &state.profiles {
        validate_profile(profile)?;
        if !ids.insert(profile.id.as_str()) {
            return Err("云盘账号文件包含重复 ID。".to_owned());
        }
    }
    Ok(())
}

fn load_profile_state(path: &Path) -> Result<PersistedCloudProfiles, String> {
    if !path_entry_exists(path)? {
        recover_interrupted_replace(path)?;
    }
    if !path_entry_exists(path)? {
        return Ok(empty_profile_state());
    }
    load_profile_file(path)
}

fn load_profile_file(path: &Path) -> Result<PersistedCloudProfiles, String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| format!("无法读取云盘账号文件：{error}"))?;
    if !metadata.file_type().is_file() {
        return Err("云盘账号路径不是普通文件，拒绝读取。".to_owned());
    }
    if metadata.len() > MAX_PROFILE_FILE_BYTES as u64 {
        return Err(format!(
            "云盘账号文件超过 {MAX_PROFILE_FILE_BYTES} 字节的上限。"
        ));
    }

    let file = OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|error| format!("无法打开云盘账号文件：{error}"))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((MAX_PROFILE_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("无法读取云盘账号文件：{error}"))?;
    if bytes.len() > MAX_PROFILE_FILE_BYTES {
        return Err(format!(
            "云盘账号文件超过 {MAX_PROFILE_FILE_BYTES} 字节的上限。"
        ));
    }

    let state: PersistedCloudProfiles =
        serde_json::from_slice(&bytes).map_err(|error| format!("无法解析云盘账号文件：{error}"))?;
    validate_profile_state(&state)?;
    Ok(state)
}

fn save_profile_state(path: &Path, state: &PersistedCloudProfiles) -> Result<(), String> {
    validate_profile_state(state)?;
    let encoded = serde_json::to_vec_pretty(state)
        .map_err(|error| format!("无法编码云盘账号文件：{error}"))?;
    if encoded.len() > MAX_PROFILE_FILE_BYTES {
        return Err(format!(
            "云盘账号文件超过 {MAX_PROFILE_FILE_BYTES} 字节的上限。"
        ));
    }
    persist_atomically(path, &encoded)
}

fn persist_atomically(path: &Path, encoded: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "无法确定云盘账号目录。".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| format!("无法创建云盘账号目录：{error}"))?;

    let temporary = parent.join(format!(
        "{TEMPORARY_FILE_PREFIX}{}{TEMPORARY_FILE_SUFFIX}",
        Uuid::new_v4().simple()
    ));
    let mut temporary_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|error| format!("无法暂存云盘账号文件：{error}"))?;
    if let Err(error) = temporary_file
        .write_all(encoded)
        .and_then(|_| temporary_file.sync_all())
    {
        drop(temporary_file);
        let _ = fs::remove_file(&temporary);
        return Err(format!("无法暂存云盘账号文件：{error}"));
    }
    drop(temporary_file);

    if !path_entry_exists(path)? {
        return fs::rename(&temporary, path).map_err(|error| {
            let _ = fs::remove_file(&temporary);
            format!("无法保存云盘账号文件：{error}")
        });
    }

    let existing_metadata = fs::symlink_metadata(path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        format!("无法检查现有云盘账号文件：{error}")
    })?;
    if !existing_metadata.file_type().is_file() {
        let _ = fs::remove_file(&temporary);
        return Err("现有云盘账号路径不是普通文件，拒绝覆盖。".to_owned());
    }

    // Windows cannot rename over an existing destination. Keep the prior
    // complete file until the staged replacement is promoted.
    let backup = parent.join(format!(
        "{TEMPORARY_FILE_PREFIX}{}{BACKUP_FILE_SUFFIX}",
        Uuid::new_v4().simple()
    ));
    fs::rename(path, &backup).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        format!("无法准备云盘账号文件更新：{error}")
    })?;
    if let Err(error) = fs::rename(&temporary, path) {
        let restore = fs::rename(&backup, path);
        let _ = fs::remove_file(&temporary);
        return Err(match restore {
            Ok(()) => format!("无法保存云盘账号文件：{error}"),
            Err(restore_error) => {
                format!("无法保存云盘账号文件（{error}），且无法恢复旧文件（{restore_error}）。")
            }
        });
    }
    if let Err(error) = fs::remove_file(&backup) {
        eprintln!("iHub could not remove a replaced Cloud Drive profile backup: {error}");
    }
    Ok(())
}

fn recover_interrupted_replace(path: &Path) -> Result<(), String> {
    if path_entry_exists(path)? {
        return Ok(());
    }
    let Some(parent) = path.parent() else {
        return Err("无法确定云盘账号目录。".to_owned());
    };
    if !path_entry_exists(parent)? {
        return Ok(());
    }

    let mut valid_backups = Vec::new();
    for entry in fs::read_dir(parent).map_err(|error| format!("无法检查云盘账号备份：{error}"))?
    {
        let entry = entry.map_err(|error| format!("无法读取云盘账号备份：{error}"))?;
        let backup = entry.path();
        let Some(name) = backup.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with(TEMPORARY_FILE_PREFIX) || !name.ends_with(BACKUP_FILE_SUFFIX) {
            continue;
        }
        if load_profile_file(&backup).is_ok() {
            valid_backups.push((
                entry
                    .metadata()
                    .ok()
                    .and_then(|metadata| metadata.modified().ok()),
                backup,
            ));
        }
    }

    valid_backups.sort_by_key(|(modified, _)| *modified);
    if let Some((_, backup)) = valid_backups.pop() {
        fs::rename(&backup, path)
            .map_err(|error| format!("无法恢复中断前的云盘账号文件：{error}"))?;
    }
    Ok(())
}

fn path_entry_exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("无法检查云盘账号路径：{error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct MemoryBackend {
        entries: Mutex<HashMap<(String, String), Vec<u8>>>,
        fail_set: Mutex<bool>,
        fail_get: Mutex<bool>,
        fail_delete: Mutex<bool>,
    }

    impl MemoryBackend {
        fn contains(&self, user: &str) -> bool {
            self.entries
                .lock()
                .unwrap()
                .contains_key(&(KEYRING_SERVICE.to_owned(), user.to_owned()))
        }

        fn remove_raw(&self, user: &str) {
            self.entries
                .lock()
                .unwrap()
                .remove(&(KEYRING_SERVICE.to_owned(), user.to_owned()));
        }

        fn replace_raw(&self, user: &str, bytes: Vec<u8>) {
            self.entries
                .lock()
                .unwrap()
                .insert((KEYRING_SERVICE.to_owned(), user.to_owned()), bytes);
        }
    }

    impl CredentialBackend for MemoryBackend {
        fn set_secret(
            &self,
            service: &str,
            user: &str,
            secret: &[u8],
        ) -> Result<(), CredentialBackendError> {
            if *self.fail_set.lock().unwrap() {
                return Err(CredentialBackendError::Other(
                    "injected set failure".to_owned(),
                ));
            }
            self.entries
                .lock()
                .unwrap()
                .insert((service.to_owned(), user.to_owned()), secret.to_vec());
            Ok(())
        }

        fn get_secret(
            &self,
            service: &str,
            user: &str,
        ) -> Result<Zeroizing<Vec<u8>>, CredentialBackendError> {
            if *self.fail_get.lock().unwrap() {
                return Err(CredentialBackendError::Other(
                    "injected get failure".to_owned(),
                ));
            }
            self.entries
                .lock()
                .unwrap()
                .get(&(service.to_owned(), user.to_owned()))
                .cloned()
                .map(Zeroizing::new)
                .ok_or(CredentialBackendError::NoEntry)
        }

        fn delete_secret(&self, service: &str, user: &str) -> Result<(), CredentialBackendError> {
            if *self.fail_delete.lock().unwrap() {
                return Err(CredentialBackendError::Other(
                    "injected delete failure".to_owned(),
                ));
            }
            if self
                .entries
                .lock()
                .unwrap()
                .remove(&(service.to_owned(), user.to_owned()))
                .is_some()
            {
                Ok(())
            } else {
                Err(CredentialBackendError::NoEntry)
            }
        }
    }

    struct SabotagingBackend {
        memory: MemoryBackend,
        metadata_path: PathBuf,
    }

    impl CredentialBackend for SabotagingBackend {
        fn set_secret(
            &self,
            service: &str,
            user: &str,
            secret: &[u8],
        ) -> Result<(), CredentialBackendError> {
            self.memory.set_secret(service, user, secret)?;
            fs::create_dir(&self.metadata_path)
                .map_err(|error| CredentialBackendError::Other(error.to_string()))
        }

        fn get_secret(
            &self,
            service: &str,
            user: &str,
        ) -> Result<Zeroizing<Vec<u8>>, CredentialBackendError> {
            self.memory.get_secret(service, user)
        }

        fn delete_secret(&self, service: &str, user: &str) -> Result<(), CredentialBackendError> {
            self.memory.delete_secret(service, user)
        }
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("ihub-cloud-vault-{label}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).expect("temporary test directory should be created");
        path
    }

    fn test_vault(label: &str) -> (PathBuf, Arc<MemoryBackend>, CloudCredentialVault) {
        let directory = temporary_directory(label);
        let backend = Arc::new(MemoryBackend::default());
        let vault =
            CloudCredentialVault::with_backend(directory.join(PROFILE_FILE_NAME), backend.clone());
        (directory, backend, vault)
    }

    fn one_profile_state() -> PersistedCloudProfiles {
        let now = "2026-07-28T12:00:00.000Z".to_owned();
        PersistedCloudProfiles {
            schema_version: PROFILE_SCHEMA_VERSION,
            profiles: vec![CloudProfileView {
                id: Uuid::new_v4().hyphenated().to_string(),
                provider: WEBDAV_PROVIDER.to_owned(),
                label: "Work NAS".to_owned(),
                endpoint: "https://dav.example.test/root/".to_owned(),
                username: "neko".to_owned(),
                created_at: now.clone(),
                updated_at: now,
            }],
        }
    }

    #[test]
    fn round_trips_profile_and_password_without_exposing_the_envelope() {
        let (directory, backend, vault) = test_vault("roundtrip");
        let profile = vault
            .save_webdav_profile(
                "工作云盘",
                "https://dav.example.test/root/",
                "neko",
                "correct horse battery staple",
            )
            .unwrap();

        assert_eq!(profile.provider, "webdav");
        assert_eq!(vault.list_profiles().unwrap(), vec![profile.clone()]);
        assert_eq!(
            vault.load_webdav_password(&profile.id).unwrap().as_str(),
            "correct horse battery staple"
        );
        assert!(backend.contains(&profile.id));

        let metadata = fs::read_to_string(directory.join(PROFILE_FILE_NAME)).unwrap();
        assert!(!metadata.contains("correct horse battery staple"));
        assert!(!metadata.contains("\"password\""));
        assert!(!metadata.contains("\"secret\""));
        assert!(!metadata.contains("\"token\""));

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rejects_field_limits_controls_and_more_than_thirty_two_profiles() {
        let (directory, _backend, vault) = test_vault("limits");
        assert!(vault
            .save_webdav_profile(&"a".repeat(97), "https://x.test/", "", "")
            .is_err());
        assert!(vault
            .save_webdav_profile("ok", &"x".repeat(2_049), "", "")
            .is_err());
        assert!(vault
            .save_webdav_profile("ok", "https://x.test/", &"u".repeat(1_025), "")
            .is_err());
        assert!(vault
            .save_webdav_profile("ok", "https://x.test/", "", &"p".repeat(2_049))
            .is_err());
        for value in ["bad\0label", "bad\nlabel", "bad\u{0085}label"] {
            assert!(vault
                .save_webdav_profile(value, "https://x.test/", "", "")
                .is_err());
        }

        for index in 0..MAX_PROFILES {
            vault
                .save_webdav_profile(&format!("profile-{index}"), "https://x.test/", "", "")
                .unwrap();
        }
        assert!(vault
            .save_webdav_profile("one-too-many", "https://x.test/", "", "")
            .is_err());

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rejects_corrupt_unknown_and_oversized_metadata() {
        let (directory, _backend, vault) = test_vault("corrupt");
        let path = directory.join(PROFILE_FILE_NAME);

        fs::write(&path, b"{not-json").unwrap();
        assert!(vault.list_profiles().is_err());

        fs::write(
            &path,
            br#"{"schemaVersion":1,"profiles":[],"unexpected":true}"#,
        )
        .unwrap();
        assert!(vault.list_profiles().is_err());

        fs::write(&path, vec![b' '; MAX_PROFILE_FILE_BYTES + 1]).unwrap();
        assert!(vault.list_profiles().is_err());

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rejects_directory_and_symlink_metadata_targets() {
        let (directory, _backend, vault) = test_vault("path-kinds");
        let path = directory.join(PROFILE_FILE_NAME);
        fs::create_dir(&path).unwrap();
        assert!(vault.list_profiles().is_err());
        fs::remove_dir(&path).unwrap();

        let target = directory.join("real-profiles.json");
        save_profile_state(&target, &one_profile_state()).unwrap();

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&target, &path).unwrap();
            assert!(vault.list_profiles().is_err());
            fs::remove_file(&path).unwrap();
        }
        #[cfg(windows)]
        {
            if std::os::windows::fs::symlink_file(&target, &path).is_ok() {
                assert!(vault.list_profiles().is_err());
                fs::remove_file(&path).unwrap();
            }
        }

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn restores_a_valid_backup_after_an_interrupted_replacement() {
        let (directory, _backend, vault) = test_vault("backup");
        let state = one_profile_state();
        let backup = directory.join(format!(
            "{TEMPORARY_FILE_PREFIX}{}{BACKUP_FILE_SUFFIX}",
            Uuid::new_v4().simple()
        ));
        save_profile_state(&backup, &state).unwrap();

        assert_eq!(vault.list_profiles().unwrap(), state.profiles);
        assert!(directory.join(PROFILE_FILE_NAME).is_file());
        assert!(!backup.exists());

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rolls_back_the_secret_when_metadata_commit_fails() {
        let directory = temporary_directory("rollback");
        let metadata_path = directory.join(PROFILE_FILE_NAME);
        let backend = Arc::new(SabotagingBackend {
            memory: MemoryBackend::default(),
            metadata_path: metadata_path.clone(),
        });
        let vault = CloudCredentialVault::with_backend(metadata_path, backend.clone());

        assert!(vault
            .save_webdav_profile("Work", "https://dav.example.test/", "neko", "secret")
            .is_err());
        assert!(backend.memory.entries.lock().unwrap().is_empty());

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn delete_is_no_entry_idempotent_and_does_not_drop_metadata_on_backend_error() {
        let (directory, backend, vault) = test_vault("delete");
        let profile = vault
            .save_webdav_profile("Work", "https://dav.example.test/", "neko", "secret")
            .unwrap();

        backend.remove_raw(&profile.id);
        vault.delete_profile(&profile.id).unwrap();
        assert!(vault.list_profiles().unwrap().is_empty());
        vault.delete_profile(&profile.id).unwrap();

        let second = vault
            .save_webdav_profile("Home", "https://home.example.test/", "neko", "secret")
            .unwrap();
        *backend.fail_delete.lock().unwrap() = true;
        assert!(vault.delete_profile(&second.id).is_err());
        assert_eq!(vault.list_profiles().unwrap(), vec![second]);

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rejects_mismatched_or_malformed_secret_envelopes() {
        let (directory, backend, vault) = test_vault("envelope");
        let profile = vault
            .save_webdav_profile("Work", "https://dav.example.test/", "neko", "secret")
            .unwrap();

        for envelope in [
            SecretEnvelope {
                schema_version: SECRET_SCHEMA_VERSION,
                provider: "onedrive".to_owned(),
                secret_kind: WEBDAV_PASSWORD_KIND.to_owned(),
                profile_id: profile.id.clone(),
                secret: "secret".to_owned(),
            },
            SecretEnvelope {
                schema_version: SECRET_SCHEMA_VERSION,
                provider: WEBDAV_PROVIDER.to_owned(),
                secret_kind: "oauth-refresh-token".to_owned(),
                profile_id: profile.id.clone(),
                secret: "secret".to_owned(),
            },
            SecretEnvelope {
                schema_version: SECRET_SCHEMA_VERSION,
                provider: WEBDAV_PROVIDER.to_owned(),
                secret_kind: WEBDAV_PASSWORD_KIND.to_owned(),
                profile_id: Uuid::new_v4().hyphenated().to_string(),
                secret: "secret".to_owned(),
            },
        ] {
            backend.replace_raw(&profile.id, serde_json::to_vec(&envelope).unwrap());
            assert!(vault.load_webdav_password(&profile.id).is_err());
        }

        backend.replace_raw(
            &profile.id,
            br#"{"schemaVersion":1,"provider":"webdav","secretKind":"webdav-basic-password","profileId":"bad","secret":"x","extra":true}"#
                .to_vec(),
        );
        assert!(vault.load_webdav_password(&profile.id).is_err());
        backend.replace_raw(&profile.id, vec![b'x'; MAX_SECRET_ENVELOPE_BYTES + 1]);
        assert!(vault.load_webdav_password(&profile.id).is_err());

        fs::remove_dir_all(directory).unwrap();
    }
}
