//! Strict, portable persistence for the launcher global hotkey.
//!
//! The renderer records `KeyboardEvent.code` values, while the host accepts
//! only a deliberately small cross-platform grammar. This keeps preferences
//! deterministic across Windows and macOS and prevents a malformed settings
//! file from reaching the global-shortcut parser.

use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const DEFAULT_LAUNCHER_HOTKEY: &str = "Alt+Space";

const HOTKEY_FILE_NAME: &str = "launcher-hotkey-v1.json";
const HOTKEY_SCHEMA_VERSION: u32 = 1;
const MAX_HOTKEY_FILE_BYTES: usize = 64 * 1024;
const MAX_HOTKEY_INPUT_BYTES: usize = 128;
const TEMPORARY_FILE_PREFIX: &str = ".launcher-hotkey-";
const TEMPORARY_FILE_SUFFIX: &str = ".tmp";
const BACKUP_FILE_SUFFIX: &str = ".backup";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedLauncherHotkey {
    schema_version: u32,
    accelerator: String,
}

/// App-data backed launcher-hotkey preference.
///
/// Missing, unsupported, corrupt, oversized, or otherwise invalid state is
/// treated as no preference. The caller can then use
/// [`DEFAULT_LAUNCHER_HOTKEY`] without allowing untrusted JSON to influence
/// global shortcut registration.
#[derive(Clone)]
pub struct LauncherHotkeyStore {
    data_path: Arc<PathBuf>,
    io_lock: Arc<Mutex<()>>,
}

impl LauncherHotkeyStore {
    pub fn new(app_data_dir: PathBuf) -> Self {
        Self::with_path(app_data_dir.join(HOTKEY_FILE_NAME))
    }

    fn with_path(data_path: PathBuf) -> Self {
        Self {
            data_path: Arc::new(data_path),
            io_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Loads a validated, canonical accelerator or returns `None` so the app
    /// can safely fall back to the built-in default.
    pub fn load_preference(&self) -> Option<String> {
        let _guard = self.lock_io();
        match load_preference(&self.data_path) {
            Ok(preference) => preference,
            Err(error) => {
                eprintln!("iHub could not restore the launcher hotkey: {error}");
                None
            }
        }
    }

    /// Validates, canonicalizes, and atomically persists a preference.
    pub fn save_preference(&self, accelerator: &str) -> Result<(), String> {
        let normalized = normalize_launcher_hotkey(accelerator)?;
        let state = PersistedLauncherHotkey {
            schema_version: HOTKEY_SCHEMA_VERSION,
            accelerator: normalized,
        };
        let encoded = serde_json::to_vec_pretty(&state)
            .map_err(|error| format!("无法编码启动快捷键设置：{error}"))?;
        if encoded.len() > MAX_HOTKEY_FILE_BYTES {
            return Err(format!(
                "启动快捷键设置超过 {MAX_HOTKEY_FILE_BYTES} 字节的本机上限。"
            ));
        }

        let _guard = self.lock_io();
        persist_atomically(&self.data_path, &encoded)
    }

    /// Removes only the exact host-owned preference file. Missing state is
    /// already equivalent to the default and is therefore a successful no-op.
    pub fn clear_preference(&self) -> Result<(), String> {
        let _guard = self.lock_io();
        if !path_entry_exists(&self.data_path)? {
            return Ok(());
        }

        let metadata = fs::symlink_metadata(self.data_path.as_ref())
            .map_err(|error| format!("无法检查启动快捷键设置：{error}"))?;
        if !metadata.file_type().is_file() {
            return Err("启动快捷键设置不是普通文件，拒绝删除。".to_owned());
        }
        fs::remove_file(self.data_path.as_ref())
            .map_err(|error| format!("无法清除启动快捷键设置：{error}"))
    }

    fn lock_io(&self) -> MutexGuard<'_, ()> {
        self.io_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Converts an accelerator to the only grammar persisted by iHub:
///
/// `CmdOrCtrl? + Alt? + Shift? + <safe KeyboardEvent.code>`
///
/// At least one of `CmdOrCtrl` or `Alt` is required. Modifier order and token
/// casing are normalized, but aliases such as `Ctrl`, `Command`, and literal
/// character keys are intentionally rejected.
pub fn normalize_launcher_hotkey(accelerator: &str) -> Result<String, String> {
    if accelerator.is_empty()
        || accelerator.len() > MAX_HOTKEY_INPUT_BYTES
        || !accelerator.is_ascii()
        || accelerator.chars().any(char::is_control)
    {
        return Err("快捷键格式无效。".to_owned());
    }

    let mut command_or_control = false;
    let mut alt = false;
    let mut shift = false;
    let mut key = None;

    for raw_token in accelerator.split('+') {
        let token = raw_token.trim();
        if token.is_empty() {
            return Err("快捷键不能包含空按键。".to_owned());
        }

        if token.eq_ignore_ascii_case("CmdOrCtrl") {
            if command_or_control {
                return Err("快捷键不能重复包含 CmdOrCtrl。".to_owned());
            }
            command_or_control = true;
            continue;
        }
        if token.eq_ignore_ascii_case("Alt") {
            if alt {
                return Err("快捷键不能重复包含 Alt。".to_owned());
            }
            alt = true;
            continue;
        }
        if token.eq_ignore_ascii_case("Shift") {
            if shift {
                return Err("快捷键不能重复包含 Shift。".to_owned());
            }
            shift = true;
            continue;
        }

        let normalized_key = normalize_safe_key(token)
            .ok_or_else(|| format!("不支持的全局快捷键按键：{token}。"))?;
        if key.replace(normalized_key).is_some() {
            return Err("快捷键必须且只能包含一个普通按键。".to_owned());
        }
    }

    if !command_or_control && !alt {
        return Err("全局快捷键必须包含 CmdOrCtrl 或 Alt。".to_owned());
    }
    let key = key.ok_or_else(|| "快捷键必须包含一个普通按键。".to_owned())?;
    if alt && key == "F4" {
        return Err("Alt+F4 是系统关闭窗口快捷键，不能用于呼出 iHub。".to_owned());
    }

    let mut tokens = Vec::with_capacity(4);
    if command_or_control {
        tokens.push("CmdOrCtrl");
    }
    if alt {
        tokens.push("Alt");
    }
    if shift {
        tokens.push("Shift");
    }
    tokens.push(key);
    Ok(tokens.join("+"))
}

fn normalize_safe_key(token: &str) -> Option<&'static str> {
    const NAMED_KEYS: &[&str] = &[
        "Space",
        "Minus",
        "Equal",
        "Comma",
        "Period",
        "Semicolon",
        "Quote",
        "Slash",
        "Backslash",
        "BracketLeft",
        "BracketRight",
        "Backquote",
    ];
    const FUNCTION_KEYS: &[&str] = &[
        "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
    ];
    const LETTER_KEYS: &[&str] = &[
        "KeyA", "KeyB", "KeyC", "KeyD", "KeyE", "KeyF", "KeyG", "KeyH", "KeyI", "KeyJ", "KeyK",
        "KeyL", "KeyM", "KeyN", "KeyO", "KeyP", "KeyQ", "KeyR", "KeyS", "KeyT", "KeyU", "KeyV",
        "KeyW", "KeyX", "KeyY", "KeyZ",
    ];
    const DIGIT_KEYS: &[&str] = &[
        "Digit0", "Digit1", "Digit2", "Digit3", "Digit4", "Digit5", "Digit6", "Digit7", "Digit8",
        "Digit9",
    ];

    NAMED_KEYS
        .iter()
        .chain(FUNCTION_KEYS)
        .chain(LETTER_KEYS)
        .chain(DIGIT_KEYS)
        .copied()
        .find(|candidate| candidate.eq_ignore_ascii_case(token))
}

fn load_preference(path: &Path) -> Result<Option<String>, String> {
    if !path_entry_exists(path)? {
        recover_interrupted_replace(path)?;
    }
    if !path_entry_exists(path)? {
        return Ok(None);
    }
    load_preference_file(path).map(Some)
}

fn load_preference_file(path: &Path) -> Result<String, String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| format!("无法读取启动快捷键设置：{error}"))?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_HOTKEY_FILE_BYTES as u64 {
        return Err("启动快捷键设置文件无效或超过大小上限。".to_owned());
    }

    let bytes = fs::read(path).map_err(|error| format!("无法读取启动快捷键设置：{error}"))?;
    if bytes.len() > MAX_HOTKEY_FILE_BYTES {
        return Err("启动快捷键设置文件超过大小上限。".to_owned());
    }
    let state: PersistedLauncherHotkey = serde_json::from_slice(&bytes)
        .map_err(|error| format!("无法解析启动快捷键设置：{error}"))?;
    if state.schema_version != HOTKEY_SCHEMA_VERSION {
        return Err("启动快捷键设置版本不受支持。".to_owned());
    }
    normalize_launcher_hotkey(&state.accelerator)
}

fn persist_atomically(path: &Path, encoded: &[u8]) -> Result<(), String> {
    if encoded.len() > MAX_HOTKEY_FILE_BYTES {
        return Err(format!(
            "启动快捷键设置超过 {MAX_HOTKEY_FILE_BYTES} 字节的本机上限。"
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| "无法确定启动快捷键设置目录。".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| format!("无法创建启动快捷键设置目录：{error}"))?;

    let temporary = parent.join(format!(
        "{TEMPORARY_FILE_PREFIX}{}{TEMPORARY_FILE_SUFFIX}",
        Uuid::new_v4().simple()
    ));
    fs::write(&temporary, encoded).map_err(|error| format!("无法暂存启动快捷键设置：{error}"))?;

    if !path_entry_exists(path)? {
        return fs::rename(&temporary, path).map_err(|error| {
            let _ = fs::remove_file(&temporary);
            format!("无法保存启动快捷键设置：{error}")
        });
    }

    let existing_metadata = fs::symlink_metadata(path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        format!("无法检查现有启动快捷键设置：{error}")
    })?;
    if !existing_metadata.file_type().is_file() {
        let _ = fs::remove_file(&temporary);
        return Err("现有启动快捷键设置不是普通文件，拒绝覆盖。".to_owned());
    }

    // Windows does not replace an existing destination with `rename`.
    // Preserve the old complete file until the staged file is promoted.
    let backup = parent.join(format!(
        "{TEMPORARY_FILE_PREFIX}{}{BACKUP_FILE_SUFFIX}",
        Uuid::new_v4().simple()
    ));
    fs::rename(path, &backup).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        format!("无法准备启动快捷键设置更新：{error}")
    })?;
    if let Err(error) = fs::rename(&temporary, path) {
        let restore = fs::rename(&backup, path);
        let _ = fs::remove_file(&temporary);
        return Err(match restore {
            Ok(()) => format!("无法保存启动快捷键设置：{error}"),
            Err(restore_error) => {
                format!("无法保存启动快捷键设置（{error}），且无法恢复旧设置（{restore_error}）。")
            }
        });
    }
    if let Err(error) = fs::remove_file(&backup) {
        eprintln!("iHub could not remove the replaced launcher hotkey backup: {error}");
    }
    Ok(())
}

fn recover_interrupted_replace(path: &Path) -> Result<(), String> {
    if path_entry_exists(path)? {
        return Ok(());
    }
    let Some(parent) = path.parent() else {
        return Err("无法确定启动快捷键设置目录。".to_owned());
    };
    if !path_entry_exists(parent)? {
        return Ok(());
    }

    let mut valid_backups = Vec::new();
    for entry in
        fs::read_dir(parent).map_err(|error| format!("无法检查启动快捷键设置备份：{error}"))?
    {
        let entry = entry.map_err(|error| format!("无法读取启动快捷键设置备份：{error}"))?;
        let backup = entry.path();
        let Some(name) = backup.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with(TEMPORARY_FILE_PREFIX) || !name.ends_with(BACKUP_FILE_SUFFIX) {
            continue;
        }
        if load_preference_file(&backup).is_ok() {
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
            .map_err(|error| format!("无法恢复中断前的启动快捷键设置：{error}"))?;
    }
    Ok(())
}

fn path_entry_exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("无法检查启动快捷键设置文件：{error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_directory(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("ihub-launcher-hotkey-{label}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).expect("temporary test directory should be created");
        path
    }

    fn persisted(accelerator: &str) -> Vec<u8> {
        serde_json::to_vec_pretty(&PersistedLauncherHotkey {
            schema_version: HOTKEY_SCHEMA_VERSION,
            accelerator: accelerator.to_owned(),
        })
        .expect("hotkey state should serialize")
    }

    #[test]
    fn default_hotkey_is_valid_and_canonical() {
        assert_eq!(
            normalize_launcher_hotkey(DEFAULT_LAUNCHER_HOTKEY).unwrap(),
            DEFAULT_LAUNCHER_HOTKEY
        );
    }

    #[test]
    fn normalizes_modifier_order_and_ascii_casing() {
        assert_eq!(
            normalize_launcher_hotkey(" shift + ALT + cmdorctrl + keyk ").unwrap(),
            "CmdOrCtrl+Alt+Shift+KeyK"
        );
        assert_eq!(
            normalize_launcher_hotkey("alt+digit7").unwrap(),
            "Alt+Digit7"
        );
        assert_eq!(
            normalize_launcher_hotkey("CMDORCTRL+backquote").unwrap(),
            "CmdOrCtrl+Backquote"
        );
    }

    #[test]
    fn accepts_every_safe_key_family() {
        for letter in b'A'..=b'Z' {
            let accelerator = format!("CmdOrCtrl+Key{}", letter as char);
            assert_eq!(
                normalize_launcher_hotkey(&accelerator).unwrap(),
                accelerator
            );
        }
        for digit in 0..=9 {
            let accelerator = format!("Alt+Digit{digit}");
            assert_eq!(
                normalize_launcher_hotkey(&accelerator).unwrap(),
                accelerator
            );
        }
        for function in 1..=12 {
            let accelerator = format!("CmdOrCtrl+F{function}");
            assert_eq!(
                normalize_launcher_hotkey(&accelerator).unwrap(),
                accelerator
            );
        }
        for key in [
            "Space",
            "Minus",
            "Equal",
            "Comma",
            "Period",
            "Semicolon",
            "Quote",
            "Slash",
            "Backslash",
            "BracketLeft",
            "BracketRight",
            "Backquote",
        ] {
            let accelerator = format!("Alt+{key}");
            assert_eq!(
                normalize_launcher_hotkey(&accelerator).unwrap(),
                accelerator
            );
        }
    }

    #[test]
    fn rejects_duplicate_or_missing_parts() {
        for invalid in [
            "",
            "Alt",
            "CmdOrCtrl+Shift",
            "Shift+KeyA",
            "Alt+Alt+Space",
            "CmdOrCtrl+cmdorctrl+KeyA",
            "Alt+Shift+Shift+KeyA",
            "Alt+KeyA+KeyB",
            "Alt++KeyA",
            "+Alt+KeyA",
            "Alt+KeyA+",
        ] {
            assert!(
                normalize_launcher_hotkey(invalid).is_err(),
                "{invalid:?} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_unsafe_keys_and_modifier_aliases() {
        for invalid in [
            "Ctrl+KeyA",
            "Command+KeyA",
            "Meta+KeyA",
            "Alt+A",
            "Alt+Tab",
            "Alt+Escape",
            "Alt+Enter",
            "Alt+Delete",
            "Alt+ArrowUp",
            "Alt+ArrowDown",
            "Alt+ArrowLeft",
            "Alt+ArrowRight",
            "Alt+F13",
            "Alt+Numpad1",
            "Alt+IntlBackslash",
            "Alt+é",
            "Alt+\nKeyA",
        ] {
            assert!(
                normalize_launcher_hotkey(invalid).is_err(),
                "{invalid:?} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_any_alt_f4_variant() {
        for invalid in [
            "Alt+F4",
            "Shift+Alt+F4",
            "CmdOrCtrl+Alt+F4",
            "CmdOrCtrl+Alt+Shift+F4",
        ] {
            assert!(
                normalize_launcher_hotkey(invalid).is_err(),
                "{invalid:?} should be rejected"
            );
        }
        assert!(normalize_launcher_hotkey("CmdOrCtrl+F4").is_ok());
    }

    #[test]
    fn missing_preference_falls_back_without_creating_a_file() {
        let directory = temporary_directory("missing");
        let store = LauncherHotkeyStore::new(directory.clone());

        assert_eq!(store.load_preference(), None);
        assert!(!directory.join(HOTKEY_FILE_NAME).exists());

        fs::remove_dir_all(directory).expect("temporary test directory should be removed");
    }

    #[test]
    fn saves_canonical_state_and_reloads_it() {
        let directory = temporary_directory("round-trip");
        let store = LauncherHotkeyStore::new(directory.clone());

        store
            .save_preference("shift + alt + keyp")
            .expect("valid preference should be saved");
        assert_eq!(store.load_preference().as_deref(), Some("Alt+Shift+KeyP"));

        let path = directory.join(HOTKEY_FILE_NAME);
        let bytes = fs::read(&path).expect("saved preference should be readable");
        assert!(bytes.len() <= MAX_HOTKEY_FILE_BYTES);
        let state: PersistedLauncherHotkey =
            serde_json::from_slice(&bytes).expect("saved preference should be valid JSON");
        assert_eq!(state.accelerator, "Alt+Shift+KeyP");

        fs::remove_dir_all(directory).expect("temporary test directory should be removed");
    }

    #[test]
    fn invalid_save_does_not_replace_an_existing_preference() {
        let directory = temporary_directory("invalid-save");
        let store = LauncherHotkeyStore::new(directory.clone());
        store
            .save_preference("Alt+Space")
            .expect("initial preference should be saved");

        assert!(store.save_preference("Alt+Escape").is_err());
        assert_eq!(store.load_preference().as_deref(), Some("Alt+Space"));

        fs::remove_dir_all(directory).expect("temporary test directory should be removed");
    }

    #[test]
    fn clears_only_the_primary_preference_and_then_uses_the_default() {
        let directory = temporary_directory("clear");
        let store = LauncherHotkeyStore::new(directory.clone());
        let unrelated = directory.join("keep-me.json");
        fs::write(&unrelated, b"unrelated").expect("unrelated file should be written");
        store
            .save_preference("CmdOrCtrl+KeyI")
            .expect("preference should save");

        store.clear_preference().expect("preference should clear");

        assert_eq!(store.load_preference(), None);
        assert!(!directory.join(HOTKEY_FILE_NAME).exists());
        assert_eq!(
            fs::read(&unrelated).expect("unrelated file should remain"),
            b"unrelated"
        );

        fs::remove_dir_all(directory).expect("temporary test directory should be removed");
    }

    #[test]
    fn clearing_a_missing_preference_is_idempotent() {
        let directory = temporary_directory("clear-missing");
        let store = LauncherHotkeyStore::new(directory.clone());

        store
            .clear_preference()
            .expect("missing preference should be a successful no-op");
        store
            .clear_preference()
            .expect("repeated clear should remain successful");
        assert_eq!(store.load_preference(), None);

        fs::remove_dir_all(directory).expect("temporary test directory should be removed");
    }

    #[test]
    fn clear_refuses_a_directory_at_the_preference_path() {
        let directory = temporary_directory("clear-directory");
        let store = LauncherHotkeyStore::new(directory.clone());
        let preference_path = directory.join(HOTKEY_FILE_NAME);
        fs::create_dir(&preference_path).expect("directory target should be created");

        assert!(store.clear_preference().is_err());
        assert!(preference_path.is_dir());

        fs::remove_dir_all(directory).expect("temporary test directory should be removed");
    }

    #[test]
    fn clear_refuses_a_symbolic_link_when_the_platform_can_create_one() {
        let directory = temporary_directory("clear-symlink");
        let store = LauncherHotkeyStore::new(directory.clone());
        let target = directory.join("real-preference.json");
        let link = directory.join(HOTKEY_FILE_NAME);
        fs::write(&target, persisted("Alt+Space")).expect("symlink target should be written");

        #[cfg(unix)]
        let link_created = std::os::unix::fs::symlink(&target, &link).is_ok();
        #[cfg(windows)]
        let link_created = std::os::windows::fs::symlink_file(&target, &link).is_ok();
        #[cfg(not(any(unix, windows)))]
        let link_created = false;

        if link_created {
            assert!(store.clear_preference().is_err());
            assert!(fs::symlink_metadata(&link)
                .expect("symbolic link should remain")
                .file_type()
                .is_symlink());
            assert!(target.is_file());
            fs::remove_file(&link).expect("test symbolic link should be removed");
        }

        fs::remove_dir_all(directory).expect("temporary test directory should be removed");
    }

    #[test]
    fn corrupt_unsupported_and_oversized_files_fall_back_safely() {
        let directory = temporary_directory("invalid-files");
        let path = directory.join(HOTKEY_FILE_NAME);
        let store = LauncherHotkeyStore::new(directory.clone());

        for invalid in [
            b"{not-json".to_vec(),
            serde_json::to_vec(&serde_json::json!({
                "schemaVersion": HOTKEY_SCHEMA_VERSION + 1,
                "accelerator": "Alt+Space",
            }))
            .unwrap(),
            serde_json::to_vec(&serde_json::json!({
                "schemaVersion": HOTKEY_SCHEMA_VERSION,
                "accelerator": "Alt+Escape",
            }))
            .unwrap(),
            serde_json::to_vec(&serde_json::json!({
                "schemaVersion": HOTKEY_SCHEMA_VERSION,
                "accelerator": "Alt+Space",
                "unexpected": true,
            }))
            .unwrap(),
            vec![b'x'; MAX_HOTKEY_FILE_BYTES + 1],
        ] {
            fs::write(&path, &invalid).expect("invalid state should be written");
            assert_eq!(store.load_preference(), None);
            assert_eq!(
                fs::read(&path).expect("invalid file should remain untouched"),
                invalid
            );
        }

        fs::remove_dir_all(directory).expect("temporary test directory should be removed");
    }

    #[test]
    fn recovers_a_valid_backup_after_an_interrupted_replace() {
        let directory = temporary_directory("backup-recovery");
        let backup = directory.join(format!(
            "{TEMPORARY_FILE_PREFIX}{}{BACKUP_FILE_SUFFIX}",
            Uuid::new_v4().simple()
        ));
        fs::write(&backup, persisted("CmdOrCtrl+Shift+KeyI"))
            .expect("valid backup should be written");
        let store = LauncherHotkeyStore::new(directory.clone());

        assert_eq!(
            store.load_preference().as_deref(),
            Some("CmdOrCtrl+Shift+KeyI")
        );
        assert!(directory.join(HOTKEY_FILE_NAME).is_file());
        assert!(!backup.exists());

        fs::remove_dir_all(directory).expect("temporary test directory should be removed");
    }

    #[test]
    fn ignores_an_invalid_interrupted_backup() {
        let directory = temporary_directory("invalid-backup");
        let backup = directory.join(format!(
            "{TEMPORARY_FILE_PREFIX}{}{BACKUP_FILE_SUFFIX}",
            Uuid::new_v4().simple()
        ));
        fs::write(&backup, b"{broken").expect("invalid backup should be written");
        let store = LauncherHotkeyStore::new(directory.clone());

        assert_eq!(store.load_preference(), None);
        assert!(backup.exists());
        assert!(!directory.join(HOTKEY_FILE_NAME).exists());

        fs::remove_dir_all(directory).expect("temporary test directory should be removed");
    }

    #[test]
    fn repeated_save_leaves_only_the_primary_file() {
        let directory = temporary_directory("replace");
        let store = LauncherHotkeyStore::new(directory.clone());
        store
            .save_preference("Alt+Space")
            .expect("initial preference should save");
        store
            .save_preference("CmdOrCtrl+Shift+KeyK")
            .expect("replacement preference should save");

        assert_eq!(
            store.load_preference().as_deref(),
            Some("CmdOrCtrl+Shift+KeyK")
        );
        let entries = fs::read_dir(&directory)
            .expect("test directory should be readable")
            .collect::<Result<Vec<_>, _>>()
            .expect("directory entries should be readable");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].file_name(), HOTKEY_FILE_NAME);

        fs::remove_dir_all(directory).expect("temporary test directory should be removed");
    }
}
