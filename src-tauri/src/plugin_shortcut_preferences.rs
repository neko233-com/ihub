//! Host-owned user overrides for plugin command global shortcuts.
//!
//! Plugin packages declare defaults, but only the trusted settings surface can
//! persist an override. The file never becomes part of a plugin namespace.

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{launcher_hotkey::normalize_plugin_hotkey, models::PluginInfo};

const FILE_NAME: &str = "plugin-shortcut-preferences-v1.json";
const SCHEMA_VERSION: u32 = 1;
const MAX_FILE_BYTES: usize = 128 * 1024;
const MAX_ENTRIES: usize = 128;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreferenceEntry {
    plugin_id: String,
    command_id: String,
    accelerator: Option<String>,
    #[serde(default)]
    auto_copy: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedPreferences {
    schema_version: u32,
    entries: Vec<PreferenceEntry>,
}

#[derive(Clone)]
pub(crate) struct PluginShortcutPreferenceStore {
    path: Arc<PathBuf>,
    lock: Arc<Mutex<()>>,
}

impl PluginShortcutPreferenceStore {
    pub(crate) fn new(app_data_dir: PathBuf) -> Self {
        Self::with_path(app_data_dir.join(FILE_NAME))
    }

    fn with_path(path: PathBuf) -> Self {
        Self {
            path: Arc::new(path),
            lock: Arc::new(Mutex::new(())),
        }
    }

    pub(crate) fn apply_to_plugins(&self, plugins: &mut [PluginInfo]) -> Result<(), String> {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entries = load_entries(&self.path)?;
        let by_target = entries
            .into_iter()
            .map(|entry| ((entry.plugin_id, entry.command_id), entry.accelerator))
            .collect::<HashMap<_, _>>();
        for plugin in plugins {
            for command in &mut plugin.commands {
                if let Some(accelerator) = by_target.get(&(plugin.id.clone(), command.id.clone())) {
                    command.shortcut.clone_from(accelerator);
                }
            }
        }
        Ok(())
    }

    pub(crate) fn auto_copy_targets(&self) -> Result<HashSet<(String, String)>, String> {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Ok(load_entries(&self.path)?
            .into_iter()
            .filter(|entry| entry.auto_copy && entry.accelerator.is_some())
            .map(|entry| (entry.plugin_id, entry.command_id))
            .collect())
    }

    pub(crate) fn set(
        &self,
        plugin_id: &str,
        command_id: &str,
        accelerator: Option<&str>,
        auto_copy: bool,
    ) -> Result<(), String> {
        validate_id(plugin_id)?;
        validate_id(command_id)?;
        let accelerator = accelerator.map(normalize_plugin_hotkey).transpose()?;
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut entries = load_entries(&self.path)?;
        if let Some(entry) = entries
            .iter_mut()
            .find(|entry| entry.plugin_id == plugin_id && entry.command_id == command_id)
        {
            entry.accelerator = accelerator;
            entry.auto_copy = auto_copy;
        } else {
            if entries.len() >= MAX_ENTRIES {
                return Err(format!("插件快捷键自定义项不能超过 {MAX_ENTRIES} 个。"));
            }
            entries.push(PreferenceEntry {
                plugin_id: plugin_id.to_owned(),
                command_id: command_id.to_owned(),
                accelerator,
                auto_copy,
            });
        }
        entries.sort_by(|left, right| {
            (&left.plugin_id, &left.command_id).cmp(&(&right.plugin_id, &right.command_id))
        });
        persist(&self.path, &entries)
    }

    pub(crate) fn reset(&self, plugin_id: &str, command_id: &str) -> Result<(), String> {
        validate_id(plugin_id)?;
        validate_id(command_id)?;
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut entries = load_entries(&self.path)?;
        entries.retain(|entry| entry.plugin_id != plugin_id || entry.command_id != command_id);
        persist(&self.path, &entries)
    }
}

fn validate_id(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value.is_ascii()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err("插件或指令标识无效。".to_owned());
    }
    Ok(())
}

fn load_entries(path: &Path) -> Result<Vec<PreferenceEntry>, String> {
    recover_interrupted_replace(path)?;
    load_entries_file(path)
}

fn load_entries_file(path: &Path) -> Result<Vec<PreferenceEntry>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("无法检查插件快捷键设置：{error}")),
    };
    if !metadata.file_type().is_file() || metadata.len() > MAX_FILE_BYTES as u64 {
        return Err("插件快捷键设置不是普通文件或超过大小上限。".to_owned());
    }
    let bytes = fs::read(path).map_err(|error| format!("无法读取插件快捷键设置：{error}"))?;
    let persisted: PersistedPreferences = serde_json::from_slice(&bytes)
        .map_err(|error| format!("无法解析插件快捷键设置：{error}"))?;
    if persisted.schema_version != SCHEMA_VERSION || persisted.entries.len() > MAX_ENTRIES {
        return Err("插件快捷键设置版本或条目数量无效。".to_owned());
    }
    let mut seen = HashSet::new();
    for entry in &persisted.entries {
        validate_id(&entry.plugin_id)?;
        validate_id(&entry.command_id)?;
        if let Some(accelerator) = &entry.accelerator {
            if normalize_plugin_hotkey(accelerator)? != *accelerator {
                return Err("插件快捷键设置包含非规范组合键。".to_owned());
            }
        }
        if !seen.insert((&entry.plugin_id, &entry.command_id)) {
            return Err("插件快捷键设置包含重复目标。".to_owned());
        }
    }
    Ok(persisted.entries)
}

fn recover_interrupted_replace(path: &Path) -> Result<(), String> {
    if path_entry_exists(path)? {
        return Ok(());
    }
    let Some(parent) = path.parent() else {
        return Err("无法确定插件快捷键设置目录。".to_owned());
    };
    if !path_entry_exists(parent)? {
        return Ok(());
    }
    let mut backups = Vec::new();
    for entry in
        fs::read_dir(parent).map_err(|error| format!("无法检查插件快捷键设置备份：{error}"))?
    {
        let entry = entry.map_err(|error| format!("无法读取插件快捷键设置备份：{error}"))?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with(".plugin-shortcuts-") || !name.ends_with(".backup") {
            continue;
        }
        if load_entries_file(&path).is_ok() {
            backups.push((
                entry
                    .metadata()
                    .ok()
                    .and_then(|value| value.modified().ok()),
                path,
            ));
        }
    }
    backups.sort_by_key(|(modified, _)| *modified);
    if let Some((_, backup)) = backups.pop() {
        fs::rename(backup, path)
            .map_err(|error| format!("无法恢复中断前的插件快捷键设置：{error}"))?;
    }
    Ok(())
}

fn persist(path: &Path, entries: &[PreferenceEntry]) -> Result<(), String> {
    let encoded = serde_json::to_vec_pretty(&PersistedPreferences {
        schema_version: SCHEMA_VERSION,
        entries: entries.to_vec(),
    })
    .map_err(|error| format!("无法编码插件快捷键设置：{error}"))?;
    if encoded.len() > MAX_FILE_BYTES {
        return Err("插件快捷键设置超过大小上限。".to_owned());
    }
    let parent = path
        .parent()
        .ok_or_else(|| "无法确定插件快捷键设置目录。".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| format!("无法创建插件快捷键设置目录：{error}"))?;
    let staged = parent.join(format!(".plugin-shortcuts-{}.tmp", Uuid::new_v4().simple()));
    fs::write(&staged, encoded).map_err(|error| format!("无法暂存插件快捷键设置：{error}"))?;
    if !path_entry_exists(path)? {
        return fs::rename(&staged, path).map_err(|error| {
            let _ = fs::remove_file(&staged);
            format!("无法保存插件快捷键设置：{error}")
        });
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("无法检查现有插件快捷键设置：{error}"))?;
    if !metadata.file_type().is_file() {
        let _ = fs::remove_file(&staged);
        return Err("现有插件快捷键设置不是普通文件，拒绝覆盖。".to_owned());
    }
    let backup = parent.join(format!(
        ".plugin-shortcuts-{}.backup",
        Uuid::new_v4().simple()
    ));
    fs::rename(path, &backup).map_err(|error| {
        let _ = fs::remove_file(&staged);
        format!("无法准备插件快捷键设置更新：{error}")
    })?;
    if let Err(error) = fs::rename(&staged, path) {
        let _ = fs::rename(&backup, path);
        let _ = fs::remove_file(&staged);
        return Err(format!("无法保存插件快捷键设置：{error}"));
    }
    let _ = fs::remove_file(backup);
    Ok(())
}

fn path_entry_exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("无法检查插件快捷键设置：{error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::PluginCommandInfo;

    #[test]
    fn overrides_and_disables_only_the_exact_plugin_command() {
        let root = std::env::temp_dir().join(format!("ihub-plugin-shortcuts-{}", Uuid::new_v4()));
        let store = PluginShortcutPreferenceStore::with_path(root.join(FILE_NAME));
        store
            .set("plugin-a", "open", Some("Alt+KeyK"), true)
            .unwrap();
        assert!(store
            .auto_copy_targets()
            .unwrap()
            .contains(&("plugin-a".to_owned(), "open".to_owned())));
        fs::rename(
            store.path.as_ref(),
            root.join(".plugin-shortcuts-interrupted.backup"),
        )
        .unwrap();
        let mut plugin = PluginInfo {
            id: "plugin-a".to_owned(),
            name: "Plugin".to_owned(),
            version: "1".to_owned(),
            description: None,
            icon_src: None,
            source: None,
            commit: None,
            installed_at: None,
            source_lock: None,
            is_development_link: false,
            local_link_status: None,
            local_link_error: None,
            uses_managed_snapshot_fallback: false,
            local_path: None,
            frontend_entry: None,
            enabled: true,
            has_native_worker: false,
            update_channel: None,
            auto_update: false,
            command_count: 1,
            tool_count: 0,
            commands: vec![PluginCommandInfo {
                id: "open".to_owned(),
                name: "Open".to_owned(),
                description: None,
                icon_src: None,
                execution: "frontend".to_owned(),
                keywords: Vec::new(),
                shortcut: Some("Alt+KeyO".to_owned()),
                shortcut_registration: None,
                shortcut_error: None,
            }],
            global_shortcuts: Vec::new(),
            search_providers: Vec::new(),
            launcher_context: None,
        };
        store
            .apply_to_plugins(std::slice::from_mut(&mut plugin))
            .unwrap();
        assert_eq!(plugin.commands[0].shortcut.as_deref(), Some("Alt+KeyK"));
        store.set("plugin-a", "open", None, false).unwrap();
        assert!(store.auto_copy_targets().unwrap().is_empty());
        store
            .apply_to_plugins(std::slice::from_mut(&mut plugin))
            .unwrap();
        assert!(plugin.commands[0].shortcut.is_none());
        store.reset("plugin-a", "open").unwrap();
        assert!(load_entries(&store.path).unwrap().is_empty());
        let _ = fs::remove_dir_all(root);
    }
}
