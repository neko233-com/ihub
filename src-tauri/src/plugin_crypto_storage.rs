//! Per-plugin encrypted key/value storage for the uTools compatibility layer.
//!
//! One random AES-256 key per plugin lives in Windows Credential Manager or
//! macOS Keychain. App data contains only AES-GCM ciphertext, a random nonce,
//! and the plugin namespace needed for lifecycle cleanup. Both the encrypted
//! plaintext envelope and AEAD additional data bind the ciphertext to its
//! plugin ID, so copying one namespace over another cannot disclose values.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use ring::{
    aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM},
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::host_log;

const STORAGE_FILE_NAME: &str = "plugin-crypto-storage-v1.json";
const STORAGE_SCHEMA_VERSION: u32 = 1;
const PLAINTEXT_SCHEMA_VERSION: u32 = 1;
const KEYRING_SERVICE: &str = "com.neko233.ihub.plugin-crypto-storage.v1";
const AAD_DOMAIN: &[u8] = b"com.neko233.ihub.plugin-crypto-storage.v1\0";
const KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 12;
const TAG_BYTES: usize = 16;

pub const MAX_KEYS_PER_PLUGIN: usize = 128;
pub const MAX_KEY_BYTES: usize = 48;
pub const MAX_VALUE_BYTES: usize = 64 * 1024;
const MAX_PLUGIN_ID_BYTES: usize = 256;
const MAX_PLUGIN_PLAINTEXT_BYTES: usize = 512 * 1024;
const MAX_PLUGINS: usize = 512;
const MAX_STORAGE_FILE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedStorage {
    #[serde(default = "storage_schema_version")]
    schema_version: u32,
    #[serde(default)]
    plugins: BTreeMap<String, EncryptedNamespace>,
}

fn storage_schema_version() -> u32 {
    STORAGE_SCHEMA_VERSION
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EncryptedNamespace {
    nonce_base64: String,
    ciphertext_base64: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PlaintextNamespace {
    schema_version: u32,
    plugin_id: String,
    values: BTreeMap<String, Value>,
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
        Err(unsupported_backend_error())
    }

    fn get_secret(
        &self,
        _service: &str,
        _user: &str,
    ) -> Result<Zeroizing<Vec<u8>>, CredentialBackendError> {
        Err(unsupported_backend_error())
    }

    fn delete_secret(&self, _service: &str, _user: &str) -> Result<(), CredentialBackendError> {
        Err(unsupported_backend_error())
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn unsupported_backend_error() -> CredentialBackendError {
    CredentialBackendError::Other(
        "Encrypted plugin storage is supported only on Windows and macOS.".to_owned(),
    )
}

/// Serialized encrypted storage operations shared by all plugin surfaces.
#[derive(Clone)]
pub struct PluginCryptoStorage {
    data_path: Arc<PathBuf>,
    backend: Arc<dyn CredentialBackend>,
    operation_lock: Arc<Mutex<()>>,
}

impl PluginCryptoStorage {
    pub fn new(app_data_dir: PathBuf) -> Self {
        #[cfg(any(windows, target_os = "macos"))]
        let backend: Arc<dyn CredentialBackend> = Arc::new(PlatformCredentialBackend);
        #[cfg(not(any(windows, target_os = "macos")))]
        let backend: Arc<dyn CredentialBackend> = Arc::new(UnsupportedCredentialBackend);

        Self::with_backend(app_data_dir.join(STORAGE_FILE_NAME), backend)
    }

    fn with_backend(data_path: PathBuf, backend: Arc<dyn CredentialBackend>) -> Self {
        Self {
            data_path: Arc::new(data_path),
            backend,
            operation_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn snapshot(&self, plugin_id: &str) -> Result<BTreeMap<String, Value>, String> {
        validate_plugin_id(plugin_id)?;
        let _guard = self.lock_operations();
        let state = load_storage(&self.data_path)?;
        let Some(encrypted) = state.plugins.get(plugin_id) else {
            return Ok(BTreeMap::new());
        };
        let key = self.required_key(plugin_id)?;
        decrypt_namespace(plugin_id, encrypted, key.as_slice())
    }

    pub fn set(&self, plugin_id: &str, key: &str, value: Value) -> Result<(), String> {
        validate_plugin_id(plugin_id)?;
        validate_key(key)?;
        validate_value(&value)?;

        let _guard = self.lock_operations();
        let mut state = load_storage(&self.data_path)?;
        let existing = state.plugins.get(plugin_id);
        if existing.is_none() && state.plugins.len() >= MAX_PLUGINS {
            return Err(format!(
                "Encrypted plugin storage supports at most {MAX_PLUGINS} plugin namespaces."
            ));
        }

        let (key_material, created_key) = if existing.is_some() {
            (self.required_key(plugin_id)?, false)
        } else {
            self.load_or_create_key(plugin_id)?
        };
        let mut values = match existing {
            Some(encrypted) => decrypt_namespace(plugin_id, encrypted, key_material.as_slice())?,
            None => BTreeMap::new(),
        };
        if !values.contains_key(key) && values.len() >= MAX_KEYS_PER_PLUGIN {
            if created_key {
                self.rollback_new_key(plugin_id);
            }
            return Err(format!(
                "uTools dbCryptoStorage supports at most {MAX_KEYS_PER_PLUGIN} keys per plugin."
            ));
        }
        values.insert(key.to_owned(), value);
        let encrypted = match encrypt_namespace(plugin_id, &values, key_material.as_slice()) {
            Ok(encrypted) => encrypted,
            Err(error) => {
                if created_key {
                    self.rollback_new_key(plugin_id);
                }
                return Err(error);
            }
        };
        state.plugins.insert(plugin_id.to_owned(), encrypted);
        if let Err(error) = persist_storage(&self.data_path, &state) {
            if created_key {
                self.rollback_new_key(plugin_id);
            }
            return Err(error);
        }
        Ok(())
    }

    pub fn remove(&self, plugin_id: &str, key: &str) -> Result<bool, String> {
        validate_plugin_id(plugin_id)?;
        validate_key(key)?;

        let _guard = self.lock_operations();
        let mut state = load_storage(&self.data_path)?;
        let Some(encrypted) = state.plugins.get(plugin_id) else {
            return Ok(false);
        };
        let key_material = self.required_key(plugin_id)?;
        let mut values = decrypt_namespace(plugin_id, encrypted, key_material.as_slice())?;
        if values.remove(key).is_none() {
            return Ok(false);
        }
        let encrypted = encrypt_namespace(plugin_id, &values, key_material.as_slice())?;
        state.plugins.insert(plugin_id.to_owned(), encrypted);
        persist_storage(&self.data_path, &state)?;
        Ok(true)
    }

    /// Removes ciphertext first and then the now-orphaned platform key. A key
    /// deletion failure is reported for diagnostics, but can no longer reveal
    /// data because the only ciphertext has already been atomically removed.
    pub fn remove_plugin(&self, plugin_id: &str) -> Result<bool, String> {
        validate_plugin_id(plugin_id)?;
        let _guard = self.lock_operations();
        let mut state = load_storage(&self.data_path)?;
        let removed = state.plugins.remove(plugin_id).is_some();
        if removed {
            persist_storage(&self.data_path, &state)?;
        }
        match self
            .backend
            .delete_secret(KEYRING_SERVICE, &credential_user(plugin_id))
        {
            Ok(()) | Err(CredentialBackendError::NoEntry) => Ok(removed),
            Err(CredentialBackendError::Other(error)) => Err(format!(
                "Encrypted plugin data was removed, but its orphaned system credential could not be deleted: {error}"
            )),
        }
    }

    fn load_or_create_key(&self, plugin_id: &str) -> Result<(Zeroizing<Vec<u8>>, bool), String> {
        let user = credential_user(plugin_id);
        match self.backend.get_secret(KEYRING_SERVICE, &user) {
            Ok(key) => {
                validate_key_material(key.as_slice())?;
                Ok((key, false))
            }
            Err(CredentialBackendError::NoEntry) => {
                let mut key = Zeroizing::new(vec![0_u8; KEY_BYTES]);
                SystemRandom::new().fill(key.as_mut_slice()).map_err(|_| {
                    "Could not generate an encrypted plugin storage key.".to_owned()
                })?;
                self.backend
                    .set_secret(KEYRING_SERVICE, &user, key.as_slice())
                    .map_err(credential_write_error)?;
                Ok((key, true))
            }
            Err(CredentialBackendError::Other(error)) => Err(format!(
                "Could not read the system credential store: {error}"
            )),
        }
    }

    fn required_key(&self, plugin_id: &str) -> Result<Zeroizing<Vec<u8>>, String> {
        match self
            .backend
            .get_secret(KEYRING_SERVICE, &credential_user(plugin_id))
        {
            Ok(key) => {
                validate_key_material(key.as_slice())?;
                Ok(key)
            }
            Err(CredentialBackendError::NoEntry) => Err(
                "Encrypted plugin data exists, but its system credential is missing; refusing to replace or expose it."
                    .to_owned(),
            ),
            Err(CredentialBackendError::Other(error)) => {
                Err(format!("Could not read the system credential store: {error}"))
            }
        }
    }

    fn rollback_new_key(&self, plugin_id: &str) {
        match self
            .backend
            .delete_secret(KEYRING_SERVICE, &credential_user(plugin_id))
        {
            Ok(()) | Err(CredentialBackendError::NoEntry) => {}
            Err(CredentialBackendError::Other(error)) => host_log::warn(
                "plugins",
                format!(
                    "Could not roll back a newly-created encrypted storage credential for '{plugin_id}': {error}"
                ),
            ),
        }
    }

    fn lock_operations(&self) -> MutexGuard<'_, ()> {
        self.operation_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn encrypt_namespace(
    plugin_id: &str,
    values: &BTreeMap<String, Value>,
    key_material: &[u8],
) -> Result<EncryptedNamespace, String> {
    validate_values(values)?;
    validate_key_material(key_material)?;
    let plaintext = PlaintextNamespace {
        schema_version: PLAINTEXT_SCHEMA_VERSION,
        plugin_id: plugin_id.to_owned(),
        values: values.clone(),
    };
    let mut encoded = Zeroizing::new(
        serde_json::to_vec(&plaintext)
            .map_err(|error| format!("Could not encode encrypted plugin storage: {error}"))?,
    );
    if encoded.len() > MAX_PLUGIN_PLAINTEXT_BYTES {
        return Err(format!(
            "One plugin's encrypted storage must not exceed {MAX_PLUGIN_PLAINTEXT_BYTES} plaintext bytes."
        ));
    }

    let mut nonce_bytes = [0_u8; NONCE_BYTES];
    SystemRandom::new()
        .fill(&mut nonce_bytes)
        .map_err(|_| "Could not generate an encrypted plugin storage nonce.".to_owned())?;
    let key = aead_key(key_material)?;
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce_bytes),
        Aad::from(aad(plugin_id).as_slice()),
        &mut *encoded,
    )
    .map_err(|_| "Could not encrypt plugin storage.".to_owned())?;

    Ok(EncryptedNamespace {
        nonce_base64: BASE64_STANDARD.encode(nonce_bytes),
        ciphertext_base64: BASE64_STANDARD.encode(encoded.as_slice()),
    })
}

fn decrypt_namespace(
    plugin_id: &str,
    encrypted: &EncryptedNamespace,
    key_material: &[u8],
) -> Result<BTreeMap<String, Value>, String> {
    validate_key_material(key_material)?;
    let nonce = BASE64_STANDARD
        .decode(&encrypted.nonce_base64)
        .map_err(|_| "Encrypted plugin storage contains an invalid nonce.".to_owned())?;
    let nonce: [u8; NONCE_BYTES] = nonce
        .try_into()
        .map_err(|_| "Encrypted plugin storage contains a nonce of the wrong size.".to_owned())?;
    let mut ciphertext = Zeroizing::new(
        BASE64_STANDARD
            .decode(&encrypted.ciphertext_base64)
            .map_err(|_| "Encrypted plugin storage contains invalid ciphertext.".to_owned())?,
    );
    if ciphertext.len() < TAG_BYTES || ciphertext.len() > MAX_PLUGIN_PLAINTEXT_BYTES + TAG_BYTES {
        return Err("Encrypted plugin storage ciphertext exceeds its safe size bounds.".to_owned());
    }
    let key = aead_key(key_material)?;
    let plaintext_len = {
        let plaintext = key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad(plugin_id).as_slice()),
                ciphertext.as_mut(),
            )
            .map_err(|_| {
                "Encrypted plugin storage authentication failed; data was not replaced or exposed."
                    .to_owned()
            })?;
        plaintext.len()
    };
    ciphertext.truncate(plaintext_len);
    let plaintext = serde_json::from_slice::<PlaintextNamespace>(ciphertext.as_slice())
        .map_err(|error| format!("Encrypted plugin storage plaintext is invalid: {error}"))?;
    if plaintext.schema_version != PLAINTEXT_SCHEMA_VERSION || plaintext.plugin_id != plugin_id {
        return Err("Encrypted plugin storage belongs to a different plugin or schema.".to_owned());
    }
    validate_values(&plaintext.values)?;
    Ok(plaintext.values)
}

fn aead_key(key_material: &[u8]) -> Result<LessSafeKey, String> {
    UnboundKey::new(&AES_256_GCM, key_material)
        .map(LessSafeKey::new)
        .map_err(|_| "Encrypted plugin storage key material is invalid.".to_owned())
}

fn aad(plugin_id: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(AAD_DOMAIN.len() + plugin_id.len());
    aad.extend_from_slice(AAD_DOMAIN);
    aad.extend_from_slice(plugin_id.as_bytes());
    aad
}

fn credential_user(plugin_id: &str) -> String {
    format!("{:x}", Sha256::digest(plugin_id.as_bytes()))
}

fn validate_plugin_id(plugin_id: &str) -> Result<(), String> {
    if plugin_id.is_empty()
        || plugin_id.len() > MAX_PLUGIN_ID_BYTES
        || plugin_id.chars().any(char::is_control)
    {
        return Err("Encrypted plugin storage received an invalid plugin identity.".to_owned());
    }
    Ok(())
}

pub fn validate_key(key: &str) -> Result<(), String> {
    if key.len() > MAX_KEY_BYTES {
        return Err(format!(
            "uTools dbCryptoStorage keys must not exceed {MAX_KEY_BYTES} UTF-8 bytes."
        ));
    }
    Ok(())
}

pub fn validate_value(value: &Value) -> Result<(), String> {
    let encoded = serde_json::to_vec(value)
        .map_err(|error| format!("Could not encode uTools dbCryptoStorage value: {error}"))?;
    if encoded.len() > MAX_VALUE_BYTES {
        return Err(format!(
            "uTools dbCryptoStorage values must not exceed {MAX_VALUE_BYTES} bytes."
        ));
    }
    Ok(())
}

fn validate_values(values: &BTreeMap<String, Value>) -> Result<(), String> {
    if values.len() > MAX_KEYS_PER_PLUGIN {
        return Err(format!(
            "uTools dbCryptoStorage contains more than {MAX_KEYS_PER_PLUGIN} keys."
        ));
    }
    for (key, value) in values {
        validate_key(key)?;
        validate_value(value)?;
    }
    Ok(())
}

fn validate_key_material(key: &[u8]) -> Result<(), String> {
    if key.len() != KEY_BYTES {
        return Err("The encrypted plugin storage credential has the wrong size.".to_owned());
    }
    Ok(())
}

fn credential_write_error(error: CredentialBackendError) -> String {
    match error {
        CredentialBackendError::NoEntry => {
            "The system credential store rejected the encrypted storage key.".to_owned()
        }
        CredentialBackendError::Other(error) => {
            format!("Could not save the encrypted storage key: {error}")
        }
    }
}

fn load_storage(path: &Path) -> Result<PersistedStorage, String> {
    if !path.exists() {
        recover_interrupted_replace(path)?;
    }
    if !path.exists() {
        return Ok(PersistedStorage {
            schema_version: STORAGE_SCHEMA_VERSION,
            plugins: BTreeMap::new(),
        });
    }
    load_storage_file(path)
}

fn load_storage_file(path: &Path) -> Result<PersistedStorage, String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("Could not read encrypted plugin storage: {error}"))?;
    if bytes.len() > MAX_STORAGE_FILE_BYTES {
        return Err(format!(
            "Encrypted plugin storage exceeds the {MAX_STORAGE_FILE_BYTES}-byte host limit."
        ));
    }
    let state = serde_json::from_slice::<PersistedStorage>(&bytes)
        .map_err(|error| format!("Encrypted plugin storage metadata is invalid: {error}"))?;
    validate_persisted_storage(&state)?;
    Ok(state)
}

fn validate_persisted_storage(state: &PersistedStorage) -> Result<(), String> {
    if state.schema_version != STORAGE_SCHEMA_VERSION {
        return Err(format!(
            "Unsupported encrypted plugin storage schema version {}.",
            state.schema_version
        ));
    }
    if state.plugins.len() > MAX_PLUGINS {
        return Err(format!(
            "Encrypted plugin storage contains more than {MAX_PLUGINS} plugin namespaces."
        ));
    }
    for (plugin_id, encrypted) in &state.plugins {
        validate_plugin_id(plugin_id)?;
        let nonce = BASE64_STANDARD
            .decode(&encrypted.nonce_base64)
            .map_err(|_| "Encrypted plugin storage contains invalid nonce metadata.".to_owned())?;
        if nonce.len() != NONCE_BYTES {
            return Err("Encrypted plugin storage contains invalid nonce metadata.".to_owned());
        }
        let max_ciphertext_base64 = (MAX_PLUGIN_PLAINTEXT_BYTES + TAG_BYTES).div_ceil(3) * 4;
        if encrypted.ciphertext_base64.is_empty()
            || encrypted.ciphertext_base64.len() > max_ciphertext_base64
        {
            return Err(
                "Encrypted plugin storage contains oversized ciphertext metadata.".to_owned(),
            );
        }
    }
    Ok(())
}

fn persist_storage(path: &Path, state: &PersistedStorage) -> Result<(), String> {
    validate_persisted_storage(state)?;
    let parent = path
        .parent()
        .ok_or_else(|| "Could not determine the encrypted plugin storage directory.".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| {
        format!("Could not create the encrypted plugin storage directory: {error}")
    })?;
    let encoded = serde_json::to_vec_pretty(state)
        .map_err(|error| format!("Could not encode encrypted plugin storage: {error}"))?;
    if encoded.len() > MAX_STORAGE_FILE_BYTES {
        return Err(format!(
            "Encrypted plugin storage exceeds the {MAX_STORAGE_FILE_BYTES}-byte host limit."
        ));
    }

    let temporary = parent.join(format!(
        ".plugin-crypto-storage-{}.tmp",
        Uuid::new_v4().simple()
    ));
    fs::write(&temporary, encoded)
        .map_err(|error| format!("Could not stage encrypted plugin storage: {error}"))?;
    if !path.exists() {
        return fs::rename(&temporary, path).map_err(|error| {
            let _ = fs::remove_file(&temporary);
            format!("Could not save encrypted plugin storage: {error}")
        });
    }

    let backup = parent.join(format!(
        ".plugin-crypto-storage-{}.backup",
        Uuid::new_v4().simple()
    ));
    fs::rename(path, &backup).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        format!("Could not prepare encrypted plugin storage update: {error}")
    })?;
    if let Err(error) = fs::rename(&temporary, path) {
        let restore = fs::rename(&backup, path);
        let _ = fs::remove_file(&temporary);
        return Err(match restore {
            Ok(()) => format!("Could not save encrypted plugin storage: {error}"),
            Err(restore_error) => format!(
                "Could not save encrypted plugin storage ({error}) and could not restore its prior file ({restore_error})."
            ),
        });
    }
    if let Err(error) = fs::remove_file(&backup) {
        host_log::warn(
            "plugins",
            format!("Could not remove an encrypted storage backup: {error}"),
        );
    }
    Ok(())
}

fn recover_interrupted_replace(path: &Path) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Err("Could not determine the encrypted plugin storage directory.".to_owned());
    };
    if !parent.exists() || path.exists() {
        return Ok(());
    }
    let mut valid_backups = Vec::new();
    for entry in fs::read_dir(parent)
        .map_err(|error| format!("Could not inspect encrypted storage backups: {error}"))?
    {
        let entry = entry
            .map_err(|error| format!("Could not inspect an encrypted storage backup: {error}"))?;
        let backup = entry.path();
        let Some(name) = backup.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(id) = name
            .strip_prefix(".plugin-crypto-storage-")
            .and_then(|name| name.strip_suffix(".backup"))
        else {
            continue;
        };
        if Uuid::parse_str(id).is_err() || load_storage_file(&backup).is_err() {
            continue;
        }
        let modified = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .unwrap_or(std::time::UNIX_EPOCH);
        valid_backups.push((modified, backup));
    }
    valid_backups.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| right.1.cmp(&left.1)));
    for (_, backup) in valid_backups {
        if path.exists() {
            return Ok(());
        }
        match fs::rename(&backup, path) {
            Ok(()) => return Ok(()),
            Err(_) if path.exists() => return Ok(()),
            Err(_) => continue,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Mutex, time::SystemTime};

    use serde_json::json;

    use super::*;

    #[derive(Default)]
    struct MemoryBackend {
        values: Mutex<HashMap<(String, String), Vec<u8>>>,
    }

    impl CredentialBackend for MemoryBackend {
        fn set_secret(
            &self,
            service: &str,
            user: &str,
            secret: &[u8],
        ) -> Result<(), CredentialBackendError> {
            self.values
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
            self.values
                .lock()
                .unwrap()
                .get(&(service.to_owned(), user.to_owned()))
                .cloned()
                .map(Zeroizing::new)
                .ok_or(CredentialBackendError::NoEntry)
        }

        fn delete_secret(&self, service: &str, user: &str) -> Result<(), CredentialBackendError> {
            if self
                .values
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

    fn test_storage(label: &str) -> (PathBuf, Arc<MemoryBackend>, PluginCryptoStorage) {
        let directory = std::env::temp_dir().join(format!(
            "ihub-plugin-crypto-{label}-{}",
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let backend = Arc::new(MemoryBackend::default());
        let storage =
            PluginCryptoStorage::with_backend(directory.join(STORAGE_FILE_NAME), backend.clone());
        (directory, backend, storage)
    }

    #[test]
    fn values_persist_encrypted_and_stay_plugin_scoped() {
        let (directory, backend, storage) = test_storage("roundtrip");
        storage
            .set("plugin-one", "password", json!("swordfish"))
            .expect("save encrypted value");
        storage
            .set("plugin-two", "password", json!({"token": "neighbor"}))
            .expect("save neighboring value");

        let on_disk = fs::read_to_string(directory.join(STORAGE_FILE_NAME)).unwrap();
        assert!(!on_disk.contains("password"));
        assert!(!on_disk.contains("swordfish"));
        assert!(!on_disk.contains("neighbor"));
        let restarted =
            PluginCryptoStorage::with_backend(directory.join(STORAGE_FILE_NAME), backend);
        assert_eq!(
            restarted.snapshot("plugin-one").unwrap().get("password"),
            Some(&json!("swordfish"))
        );
        assert_eq!(
            restarted.snapshot("plugin-two").unwrap().get("password"),
            Some(&json!({"token": "neighbor"}))
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn authenticated_ciphertext_rejects_tampering_and_namespace_swaps() {
        let (directory, backend, storage) = test_storage("authentication");
        storage.set("plugin-one", "key", json!("one")).unwrap();
        storage.set("plugin-two", "key", json!("two")).unwrap();
        let mut state = load_storage(&directory.join(STORAGE_FILE_NAME)).unwrap();
        let first = state.plugins["plugin-one"].clone();
        state.plugins.insert("plugin-two".to_owned(), first);
        persist_storage(&directory.join(STORAGE_FILE_NAME), &state).unwrap();
        let restarted =
            PluginCryptoStorage::with_backend(directory.join(STORAGE_FILE_NAME), backend);
        assert!(restarted.snapshot("plugin-two").is_err());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn remove_and_uninstall_clear_only_the_requested_scope() {
        let (directory, backend, storage) = test_storage("remove");
        storage.set("plugin-one", "a", json!(1)).unwrap();
        storage.set("plugin-one", "b", json!(2)).unwrap();
        storage.set("plugin-two", "a", json!(3)).unwrap();
        assert!(storage.remove("plugin-one", "a").unwrap());
        assert_eq!(
            storage.snapshot("plugin-one").unwrap(),
            BTreeMap::from([("b".to_owned(), json!(2))])
        );
        assert!(storage.remove_plugin("plugin-one").unwrap());
        assert!(storage.snapshot("plugin-one").unwrap().is_empty());
        assert_eq!(
            storage.snapshot("plugin-two").unwrap().get("a"),
            Some(&json!(3))
        );
        assert!(!backend
            .values
            .lock()
            .unwrap()
            .contains_key(&(KEYRING_SERVICE.to_owned(), credential_user("plugin-one"))));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn rejects_oversized_keys_values_and_missing_credentials() {
        let (directory, backend, storage) = test_storage("limits");
        assert!(storage
            .set("plugin", &"x".repeat(MAX_KEY_BYTES + 1), json!(1))
            .is_err());
        assert!(storage
            .set("plugin", "large", json!("x".repeat(MAX_VALUE_BYTES)))
            .is_err());
        storage.set("plugin", "ok", json!(true)).unwrap();
        backend
            .values
            .lock()
            .unwrap()
            .remove(&(KEYRING_SERVICE.to_owned(), credential_user("plugin")));
        assert!(storage.snapshot("plugin").is_err());
        assert!(storage.set("plugin", "replacement", json!(false)).is_err());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn key_material_is_not_derived_from_plugin_identity() {
        let (directory, backend, storage) = test_storage("random-key");
        storage.set("plugin", "key", json!(1)).unwrap();
        let key = backend
            .values
            .lock()
            .unwrap()
            .get(&(KEYRING_SERVICE.to_owned(), credential_user("plugin")))
            .cloned()
            .unwrap();
        assert_eq!(key.len(), KEY_BYTES);
        assert_ne!(key, Sha256::digest(b"plugin").to_vec());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn failed_first_persist_rolls_back_the_new_system_credential() {
        let (directory, backend, _) = test_storage("persist-rollback");
        fs::create_dir_all(&directory).unwrap();
        let blocked_parent = directory.join("not-a-directory");
        fs::write(&blocked_parent, b"block directory creation").unwrap();
        let storage = PluginCryptoStorage::with_backend(
            blocked_parent.join(STORAGE_FILE_NAME),
            backend.clone(),
        );
        assert!(storage.set("plugin", "key", json!("secret")).is_err());
        assert!(!backend
            .values
            .lock()
            .unwrap()
            .contains_key(&(KEYRING_SERVICE.to_owned(), credential_user("plugin"))));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn corrupt_metadata_is_never_replaced_by_a_new_empty_namespace() {
        let (directory, backend, storage) = test_storage("corrupt-metadata");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(STORAGE_FILE_NAME);
        fs::write(&path, b"not valid json").unwrap();
        assert!(storage.set("plugin", "key", json!("secret")).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"not valid json");
        assert!(backend.values.lock().unwrap().is_empty());
        let _ = fs::remove_dir_all(directory);
    }
}
