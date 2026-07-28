//! Durable, namespaced storage for non-secret plugin settings.
//!
//! Frontend plugins use the narrow host bridge instead of direct filesystem
//! access. Keeping ordinary settings here makes that bridge useful across
//! iHub restarts while retaining a per-plugin namespace. Secret material is
//! intentionally out of scope for this JSON store: it belongs in a platform
//! credential vault, never in an app-data file.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::UNIX_EPOCH,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

const SETTINGS_FILE_NAME: &str = "plugin-settings-v1.json";
const SETTINGS_SCHEMA_VERSION: u32 = 1;
const MAX_PLUGINS_WITH_SETTINGS: usize = 512;
const MAX_SETTINGS_PER_PLUGIN: usize = 128;
const MAX_SETTING_KEY_BYTES: usize = 128;
const MAX_SETTING_VALUE_BYTES: usize = 64 * 1024;
const MAX_SETTINGS_FILE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedPluginSettings {
    #[serde(default = "default_schema_version")]
    schema_version: u32,
    #[serde(default)]
    plugins: BTreeMap<String, BTreeMap<String, Value>>,
}

impl Default for PersistedPluginSettings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            plugins: BTreeMap::new(),
        }
    }
}

fn default_schema_version() -> u32 {
    SETTINGS_SCHEMA_VERSION
}

/// A small app-data store with atomic writes. Its data shape is deliberately
/// JSON-only because it crosses the plugin host IPC boundary unchanged.
#[derive(Clone)]
pub struct PluginSettingsStore {
    data_path: Arc<PathBuf>,
    state: Arc<Mutex<PersistedPluginSettings>>,
}

impl PluginSettingsStore {
    pub fn new(app_data_dir: PathBuf) -> Self {
        let data_path = app_data_dir.join(SETTINGS_FILE_NAME);
        let state = load_state(&data_path).unwrap_or_else(|error| {
            eprintln!("iHub could not restore plugin settings: {error}");
            PersistedPluginSettings::default()
        });
        Self {
            data_path: Arc::new(data_path),
            state: Arc::new(Mutex::new(state)),
        }
    }

    pub fn get(&self, plugin_id: &str, key: &str) -> Option<Value> {
        self.lock_state()
            .plugins
            .get(plugin_id)
            .and_then(|settings| settings.get(key))
            .cloned()
    }

    pub fn set(&self, plugin_id: &str, key: &str, value: Value) -> Result<(), String> {
        Self::validate_entry(key, &value)?;

        let mut state = self.lock_state();
        let mut next = state.clone();
        if !next.plugins.contains_key(plugin_id) && next.plugins.len() >= MAX_PLUGINS_WITH_SETTINGS
        {
            return Err(format!(
                "iHub stores settings for at most {MAX_PLUGINS_WITH_SETTINGS} plugins."
            ));
        }
        let plugin_settings = next.plugins.entry(plugin_id.to_owned()).or_default();
        if !plugin_settings.contains_key(key) && plugin_settings.len() >= MAX_SETTINGS_PER_PLUGIN {
            return Err(format!(
                "A plugin may store at most {MAX_SETTINGS_PER_PLUGIN} settings."
            ));
        }
        plugin_settings.insert(key.to_owned(), value);
        self.persist(&next)?;
        *state = next;
        Ok(())
    }

    /// Validates an entry before it is placed in either the durable store or
    /// the host's session-only secret setting map. Keeping the bounds shared
    /// prevents a secret declaration from becoming a route around the normal
    /// bridge storage limits.
    pub fn validate_entry(key: &str, value: &Value) -> Result<(), String> {
        validate_setting_key(key)?;
        let value_size = serde_json::to_vec(value)
            .map_err(|error| format!("Could not encode plugin setting: {error}"))?
            .len();
        if value_size > MAX_SETTING_VALUE_BYTES {
            return Err(format!(
                "Plugin setting values must not exceed {MAX_SETTING_VALUE_BYTES} bytes."
            ));
        }
        Ok(())
    }

    /// Removes the host-owned namespace only after a managed plugin has been
    /// uninstalled. Local development links and normal updates preserve it so
    /// a developer's configuration survives rebuilds and ref changes.
    pub fn remove_plugin(&self, plugin_id: &str) -> Result<bool, String> {
        let mut state = self.lock_state();
        if !state.plugins.contains_key(plugin_id) {
            return Ok(false);
        }
        let mut next = state.clone();
        next.plugins.remove(plugin_id);
        self.persist(&next)?;
        *state = next;
        Ok(true)
    }

    /// Removes one durable value. The host uses this to scrub any plaintext
    /// value left by a pre-v1 build once a manifest declares that key secret.
    pub fn remove(&self, plugin_id: &str, key: &str) -> Result<bool, String> {
        let mut state = self.lock_state();
        let Some(settings) = state.plugins.get(plugin_id) else {
            return Ok(false);
        };
        if !settings.contains_key(key) {
            return Ok(false);
        }

        let mut next = state.clone();
        let remove_namespace = if let Some(settings) = next.plugins.get_mut(plugin_id) {
            settings.remove(key);
            settings.is_empty()
        } else {
            false
        };
        if remove_namespace {
            next.plugins.remove(plugin_id);
        }
        self.persist(&next)?;
        *state = next;
        Ok(true)
    }

    /// Scrubs a batch of keys in one atomic write. It is used during startup
    /// migration so a previously persisted secret is not retained until its
    /// plugin happens to be opened again.
    pub fn remove_declared_secrets<I>(&self, entries: I) -> Result<usize, String>
    where
        I: IntoIterator<Item = (String, String)>,
    {
        let mut state = self.lock_state();
        let mut next = state.clone();
        let mut removed = 0;
        for (plugin_id, key) in entries {
            let mut remove_namespace = false;
            if let Some(settings) = next.plugins.get_mut(&plugin_id) {
                if settings.remove(&key).is_some() {
                    removed += 1;
                }
                remove_namespace = settings.is_empty();
            }
            if remove_namespace {
                next.plugins.remove(&plugin_id);
            }
        }
        if removed == 0 {
            return Ok(0);
        }
        self.persist(&next)?;
        *state = next;
        Ok(removed)
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, PersistedPluginSettings> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn persist(&self, state: &PersistedPluginSettings) -> Result<(), String> {
        let parent = self
            .data_path
            .parent()
            .ok_or_else(|| "Could not determine the plugin settings data directory.".to_owned())?;
        fs::create_dir_all(parent).map_err(|error| {
            format!("Could not create the plugin settings data directory: {error}")
        })?;
        let encoded = serde_json::to_vec_pretty(state)
            .map_err(|error| format!("Could not encode plugin settings: {error}"))?;
        if encoded.len() > MAX_SETTINGS_FILE_BYTES {
            return Err(format!(
                "Plugin settings exceed the {MAX_SETTINGS_FILE_BYTES}-byte host limit."
            ));
        }

        let temporary = parent.join(format!(".plugin-settings-{}.tmp", Uuid::new_v4().simple()));
        fs::write(&temporary, encoded)
            .map_err(|error| format!("Could not stage plugin settings: {error}"))?;

        if !self.data_path.exists() {
            return fs::rename(&temporary, self.data_path.as_ref()).map_err(|error| {
                let _ = fs::remove_file(&temporary);
                format!("Could not save plugin settings: {error}")
            });
        }

        let backup = parent.join(format!(
            ".plugin-settings-{}.backup",
            Uuid::new_v4().simple()
        ));
        fs::rename(self.data_path.as_ref(), &backup).map_err(|error| {
            let _ = fs::remove_file(&temporary);
            format!("Could not prepare plugin settings update: {error}")
        })?;
        if let Err(error) = fs::rename(&temporary, self.data_path.as_ref()) {
            let restore = fs::rename(&backup, self.data_path.as_ref());
            let _ = fs::remove_file(&temporary);
            return Err(match restore {
                Ok(()) => format!("Could not save plugin settings: {error}"),
                Err(restore_error) => format!(
                    "Could not save plugin settings ({error}) and could not restore the prior file ({restore_error})."
                ),
            });
        }
        if let Err(error) = fs::remove_file(&backup) {
            eprintln!("iHub could not remove the replaced plugin settings backup: {error}");
        }
        Ok(())
    }
}

fn load_state(path: &Path) -> Result<PersistedPluginSettings, String> {
    if !path.exists() {
        recover_interrupted_replace(path)?;
    }
    if !path.exists() {
        return Ok(PersistedPluginSettings::default());
    }

    load_state_file(path)
}

/// Windows cannot replace an existing file with the same `rename` primitive
/// used by the portable store, so `persist` first moves the old primary aside
/// and then promotes a staged temporary file. If iHub exits in that tiny
/// interval, the next startup restores the newest *validated* host-owned
/// backup. A present primary is deliberately never replaced here: it is more
/// authoritative than any leftover backup from an earlier successful write.
fn recover_interrupted_replace(path: &Path) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    let Some(parent) = path.parent() else {
        return Err("Could not determine the plugin settings data directory.".to_owned());
    };
    if !parent.exists() {
        return Ok(());
    }

    let mut valid_backups = Vec::new();
    for entry in fs::read_dir(parent)
        .map_err(|error| format!("Could not inspect plugin settings backups: {error}"))?
    {
        let entry = entry
            .map_err(|error| format!("Could not inspect a plugin settings backup: {error}"))?;
        let backup = entry.path();
        if !is_settings_backup_file(&backup) {
            continue;
        }
        match load_state_file(&backup) {
            Ok(_) => {
                let modified = entry
                    .metadata()
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .unwrap_or(UNIX_EPOCH);
                valid_backups.push((modified, backup));
            }
            Err(error) => eprintln!(
                "iHub ignored an invalid interrupted plugin settings backup {}: {error}",
                backup.display()
            ),
        }
    }

    // Multiple stale backups can remain if the OS interrupted cleanup. The
    // latest valid one is the only candidate from the most recent replace.
    valid_backups.sort_by(|(left_time, left_path), (right_time, right_path)| {
        right_time
            .cmp(left_time)
            .then_with(|| right_path.cmp(left_path))
    });

    for (_, backup) in valid_backups {
        // Do not allow a concurrent writer to have its valid primary replaced
        // by recovery. Tauri runs one resident instance, but this check keeps
        // recovery conservative if a second process is ever introduced.
        if path.exists() {
            return Ok(());
        }
        match fs::rename(&backup, path) {
            Ok(()) => {
                eprintln!(
                    "iHub recovered plugin settings from interrupted backup {}.",
                    backup.display()
                );
                return Ok(());
            }
            Err(_) if path.exists() => return Ok(()),
            Err(error) => eprintln!(
                "iHub could not recover plugin settings backup {}: {error}",
                backup.display()
            ),
        }
    }
    Ok(())
}

fn is_settings_backup_file(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    let Some(identifier) = file_name
        .strip_prefix(".plugin-settings-")
        .and_then(|value| value.strip_suffix(".backup"))
    else {
        return false;
    };
    Uuid::parse_str(identifier).is_ok()
}

fn load_state_file(path: &Path) -> Result<PersistedPluginSettings, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("Could not read plugin settings: {error}"))?;
    if bytes.len() > MAX_SETTINGS_FILE_BYTES {
        return Err(format!(
            "Plugin settings file exceeds the {MAX_SETTINGS_FILE_BYTES}-byte host limit."
        ));
    }
    let state = serde_json::from_slice::<PersistedPluginSettings>(&bytes)
        .map_err(|error| format!("Invalid plugin settings file: {error}"))?;
    if state.schema_version != SETTINGS_SCHEMA_VERSION {
        return Err(format!(
            "Unsupported plugin settings schema version {}.",
            state.schema_version
        ));
    }
    validate_state(&state)?;
    Ok(state)
}

fn validate_state(state: &PersistedPluginSettings) -> Result<(), String> {
    if state.plugins.len() > MAX_PLUGINS_WITH_SETTINGS {
        return Err(format!(
            "Plugin settings contain more than {MAX_PLUGINS_WITH_SETTINGS} plugin namespaces."
        ));
    }
    for (plugin_id, settings) in &state.plugins {
        if plugin_id.trim().is_empty() {
            return Err("Plugin settings contain an empty plugin namespace.".to_owned());
        }
        if settings.len() > MAX_SETTINGS_PER_PLUGIN {
            return Err(format!(
                "Plugin '{plugin_id}' has more than {MAX_SETTINGS_PER_PLUGIN} settings."
            ));
        }
        for (key, value) in settings {
            validate_setting_key(key)?;
            let value_size = serde_json::to_vec(value)
                .map_err(|error| format!("Could not read plugin setting: {error}"))?
                .len();
            if value_size > MAX_SETTING_VALUE_BYTES {
                return Err(format!(
                    "Plugin setting '{plugin_id}/{key}' exceeds {MAX_SETTING_VALUE_BYTES} bytes."
                ));
            }
        }
    }
    Ok(())
}

fn validate_setting_key(key: &str) -> Result<(), String> {
    let bytes = key.as_bytes();
    if bytes.is_empty()
        || bytes.len() > MAX_SETTING_KEY_BYTES
        || !bytes[0].is_ascii_alphabetic()
        || !bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(
            "Plugin setting keys must start with an ASCII letter and contain only letters, digits, '.', '_' or '-'."
                .to_owned(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use serde_json::json;

    use super::{PluginSettingsStore, SETTINGS_FILE_NAME};

    fn temporary_directory(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "ihub-{label}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    #[test]
    fn settings_persist_across_restarts_and_stay_plugin_scoped() {
        let directory = temporary_directory("plugin-settings");
        let settings = PluginSettingsStore::new(directory.clone());
        settings
            .set("ihub-plugin-one", "language", json!("zh-CN"))
            .expect("save first plugin setting");
        settings
            .set("ihub-plugin-two", "language", json!("en"))
            .expect("save second plugin setting");
        drop(settings);

        let restarted = PluginSettingsStore::new(directory.clone());
        assert_eq!(
            restarted.get("ihub-plugin-one", "language"),
            Some(json!("zh-CN"))
        );
        assert_eq!(
            restarted.get("ihub-plugin-two", "language"),
            Some(json!("en"))
        );
        assert!(restarted
            .remove_plugin("ihub-plugin-one")
            .expect("remove plugin namespace"));
        drop(restarted);

        let reopened = PluginSettingsStore::new(directory.clone());
        assert_eq!(reopened.get("ihub-plugin-one", "language"), None);
        assert_eq!(
            reopened.get("ihub-plugin-two", "language"),
            Some(json!("en"))
        );
        if directory.exists() {
            fs::remove_dir_all(directory).expect("cleanup test directory");
        }
    }

    #[test]
    fn settings_reject_unsafe_keys_and_large_values() {
        let directory = temporary_directory("plugin-settings-limits");
        let settings = PluginSettingsStore::new(directory.clone());
        assert!(settings
            .set("ihub-plugin-one", "invalid key", json!(true))
            .is_err());
        assert!(settings
            .set("ihub-plugin-one", "large", json!("x".repeat(64 * 1024)))
            .is_err());
        if directory.exists() {
            fs::remove_dir_all(directory).expect("cleanup test directory");
        }
    }

    #[test]
    fn interrupted_replace_recovers_a_valid_backup_without_overwriting_a_primary() {
        let directory = temporary_directory("plugin-settings-interrupted-replace");
        let primary = directory.join(SETTINGS_FILE_NAME);
        let backup = directory.join(format!(
            ".plugin-settings-{}.backup",
            uuid::Uuid::new_v4().simple()
        ));

        let settings = PluginSettingsStore::new(directory.clone());
        settings
            .set("ihub-plugin-one", "theme", json!("before-interruption"))
            .expect("save initial plugin setting");
        drop(settings);
        let old_primary = fs::read(&primary).expect("read initial settings primary");

        // This is the state left after the old primary was moved out of the
        // way but before the staged replacement was promoted.
        fs::rename(&primary, &backup).expect("simulate interrupted replace");
        let recovered = PluginSettingsStore::new(directory.clone());
        assert_eq!(
            recovered.get("ihub-plugin-one", "theme"),
            Some(json!("before-interruption"))
        );
        assert!(primary.is_file(), "a missing primary should be restored");
        assert!(
            !backup.exists(),
            "the restored backup should become the current primary"
        );

        recovered
            .set("ihub-plugin-one", "theme", json!("current-primary"))
            .expect("write a newer primary");
        drop(recovered);
        // A stale but valid backup must not take precedence over the primary.
        fs::write(&backup, old_primary).expect("create stale valid backup");
        let restarted = PluginSettingsStore::new(directory.clone());
        assert_eq!(
            restarted.get("ihub-plugin-one", "theme"),
            Some(json!("current-primary"))
        );
        assert!(
            backup.is_file(),
            "a valid primary must not be replaced by backup recovery"
        );
        drop(restarted);

        if directory.exists() {
            fs::remove_dir_all(directory).expect("cleanup test directory");
        }
    }

    #[test]
    fn declared_secret_cleanup_removes_only_matching_persisted_values() {
        let directory = temporary_directory("plugin-settings-secret-cleanup");
        let settings = PluginSettingsStore::new(directory.clone());
        settings
            .set("ihub-plugin-one", "apiKey", json!("legacy-secret"))
            .expect("save old plaintext secret");
        settings
            .set("ihub-plugin-one", "theme", json!("graphite"))
            .expect("save ordinary setting");
        settings
            .set("ihub-plugin-two", "apiKey", json!("different-plugin"))
            .expect("save other plugin value");

        assert_eq!(
            settings
                .remove_declared_secrets(vec![("ihub-plugin-one".to_owned(), "apiKey".to_owned(),)])
                .expect("scrub declared secret"),
            1
        );
        drop(settings);

        let reopened = PluginSettingsStore::new(directory.clone());
        assert_eq!(reopened.get("ihub-plugin-one", "apiKey"), None);
        assert_eq!(
            reopened.get("ihub-plugin-one", "theme"),
            Some(json!("graphite"))
        );
        assert_eq!(
            reopened.get("ihub-plugin-two", "apiKey"),
            Some(json!("different-plugin"))
        );
        drop(reopened);
        if directory.exists() {
            fs::remove_dir_all(directory).expect("cleanup test directory");
        }
    }
}
