//! Host-owned, opaque launcher shortcuts.
//!
//! The renderer can request that a *currently indexed* result be pinned, but
//! it never receives or persists the durable target path. Each open resolves
//! the saved source ID through the current native index and revalidates the
//! filesystem object before the system opener is called.

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    host_log,
    indexer::{LauncherShortcutSource, ResolvedSystemIconSource, SearchIndex},
    system_open::{LocalOpenKind, PreparedLocalOpen},
};

const SHORTCUTS_FILE_NAME: &str = "launcher-shortcuts-v1.json";
const SHORTCUTS_SCHEMA_VERSION: u32 = 1;
pub const MAX_LAUNCHER_SHORTCUTS: usize = 18;
const MAX_SHORTCUTS_FILE_BYTES: usize = 128 * 1024;
const MAX_SOURCE_ID_BYTES: usize = 8 * 1024;
const MAX_LABEL_BYTES: usize = 512;
const MAX_METADATA_BYTES: usize = 1024;

/// Resolves an immediate renderer selection back through the current native
/// index, then revalidates its live object and authorization scope. The
/// renderer contributes only the current result ID, never an openable path.
pub(crate) fn resolve_current_search_result_open_target(
    search_result_id: &str,
    index: &SearchIndex,
) -> Result<PreparedLocalOpen, String> {
    let source = index
        .resolve_current_search_result_source(search_result_id)
        .ok_or_else(|| {
            "该搜索结果已过期、不在当前授权索引中，或不支持直接打开。请重新搜索。".to_owned()
        })?;
    prepare_live_source_for(&source, index, LiveSourcePurpose::ImmediateSearchResult)
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LauncherShortcutView {
    /// Opaque host-generated UUID. It is not a filesystem path or index ID.
    pub id: String,
    pub name: String,
    pub kind: String,
    pub metadata: String,
    /// `ready` means the current native index still resolves an eligible
    /// source; `unavailable` deliberately does not disclose the old path.
    pub status: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedLauncherShortcut {
    id: String,
    /// Host-private lookup key. A file result currently uses its indexed path
    /// as this key, so it must never be returned to a WebView.
    source_id: String,
    name: String,
    kind: String,
    metadata: String,
    created_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedLauncherShortcuts {
    schema_version: u32,
    #[serde(default)]
    shortcuts: Vec<PersistedLauncherShortcut>,
}

impl Default for PersistedLauncherShortcuts {
    fn default() -> Self {
        Self {
            schema_version: SHORTCUTS_SCHEMA_VERSION,
            shortcuts: Vec::new(),
        }
    }
}

/// A bounded app-data registry with portable atomic replacement. Its content
/// is not an authorization boundary: every subsequent use still resolves and
/// validates the live native target.
#[derive(Clone)]
pub struct LauncherShortcutStore {
    data_path: Arc<PathBuf>,
    state: Arc<Mutex<PersistedLauncherShortcuts>>,
}

impl LauncherShortcutStore {
    pub fn new(app_data_dir: PathBuf) -> Self {
        Self::with_path(app_data_dir.join(SHORTCUTS_FILE_NAME))
    }

    fn with_path(data_path: PathBuf) -> Self {
        let state = load_state(&data_path).unwrap_or_else(|error| {
            host_log::warn(
                "shortcuts",
                format!("Could not restore launcher shortcuts: {error}"),
            );
            PersistedLauncherShortcuts::default()
        });
        Self {
            data_path: Arc::new(data_path),
            state: Arc::new(Mutex::new(state)),
        }
    }

    pub fn list(&self, index: &SearchIndex) -> Vec<LauncherShortcutView> {
        self.lock_state()
            .shortcuts
            .iter()
            .map(|shortcut| shortcut_view(shortcut, index))
            .collect()
    }

    /// Resolves renderer-visible shortcut UUIDs back to live, authorized
    /// sources for native icon rendering. Stored source IDs and paths never
    /// cross the IPC boundary.
    pub(crate) fn resolve_system_icon_sources(
        &self,
        shortcut_ids: &[String],
        index: &SearchIndex,
    ) -> Vec<ResolvedSystemIconSource> {
        if shortcut_ids.len() > 12
            || shortcut_ids
                .iter()
                .any(|shortcut_id| shortcut_id.is_empty() || shortcut_id.len() > 128)
        {
            return Vec::new();
        }
        let requested = shortcut_ids
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        self.lock_state()
            .shortcuts
            .iter()
            .filter(|shortcut| requested.contains(shortcut.id.as_str()))
            .filter_map(|shortcut| {
                let source = index.resolve_launcher_shortcut_source(&shortcut.source_id)?;
                if source.kind != shortcut.kind {
                    return None;
                }
                let prepared = prepare_live_source(&source, index).ok()?;
                Some(ResolvedSystemIconSource {
                    response_id: shortcut.id.clone(),
                    kind: shortcut.kind.clone(),
                    prepared,
                })
            })
            .collect()
    }

    /// Maps an exact current index source to an opaque shortcut ID. The app
    /// command uses this only to let a search-result context action say
    /// “取消固定”; it never exposes the stored source key itself.
    pub fn shortcut_id_for_source(&self, source_id: &str) -> Option<String> {
        self.lock_state()
            .shortcuts
            .iter()
            .find(|shortcut| shortcut.source_id == source_id)
            .map(|shortcut| shortcut.id.clone())
    }

    /// Pins a source only when it is presently present in the host-owned
    /// index and passes a live filesystem validation. The webview contributes
    /// an ID, never a path, command line, shell string, or icon URL.
    pub fn pin_from_search(
        &self,
        source_id: &str,
        index: &SearchIndex,
    ) -> Result<LauncherShortcutView, String> {
        let source = index
            .resolve_launcher_shortcut_source(source_id)
            .ok_or_else(|| "该搜索结果已不在当前索引中，或不支持固定到启动页。".to_owned())?;
        let _prepared = prepare_live_source(&source, index)?;

        let mut state = self.lock_state();
        if let Some(existing) = state
            .shortcuts
            .iter()
            .find(|shortcut| shortcut.source_id == source.id)
        {
            return Ok(shortcut_view(existing, index));
        }
        if state.shortcuts.len() >= MAX_LAUNCHER_SHORTCUTS {
            return Err(format!(
                "文件启动最多固定 {MAX_LAUNCHER_SHORTCUTS} 个项目；请先取消固定一个项目。"
            ));
        }

        let shortcut = PersistedLauncherShortcut {
            id: Uuid::new_v4().to_string(),
            source_id: source.id,
            name: source.name,
            kind: source.kind,
            metadata: source.metadata,
            created_at: Utc::now().to_rfc3339(),
        };
        validate_shortcut(&shortcut)?;
        let mut next = state.clone();
        next.shortcuts.insert(0, shortcut.clone());
        self.persist(&next)?;
        *state = next;
        Ok(shortcut_view(&shortcut, index))
    }

    /// Returns a revalidated target path for the exact opaque shortcut. This
    /// method never takes a renderer-provided path and intentionally leaves a
    /// stale record intact so the person can decide whether to unpin it.
    pub fn resolve_open_target(
        &self,
        shortcut_id: &str,
        index: &SearchIndex,
    ) -> Result<PreparedLocalOpen, String> {
        if shortcut_id.len() > 128 {
            return Err("启动器快捷项标识无效。".to_owned());
        }
        let shortcut = self
            .lock_state()
            .shortcuts
            .iter()
            .find(|shortcut| shortcut.id == shortcut_id)
            .cloned()
            .ok_or_else(|| "找不到该文件启动项；它可能已被取消固定。".to_owned())?;
        let source = index
            .resolve_launcher_shortcut_source(&shortcut.source_id)
            .ok_or_else(|| {
                "固定目标当前不在已授权索引中；请恢复索引后重试或取消固定。".to_owned()
            })?;
        if source.kind != shortcut.kind {
            return Err("固定目标的类型已变化；为安全起见不会打开它。请重新固定。".to_owned());
        }
        prepare_live_source(&source, index)
    }

    /// Removes only the host-owned registry record. It never deletes or
    /// changes the target file, directory, application, or its parent.
    pub fn unpin(&self, shortcut_id: &str) -> Result<bool, String> {
        if shortcut_id.len() > 128 {
            return Err("启动器快捷项标识无效。".to_owned());
        }
        let mut state = self.lock_state();
        let Some(position) = state
            .shortcuts
            .iter()
            .position(|shortcut| shortcut.id == shortcut_id)
        else {
            return Ok(false);
        };
        let mut next = state.clone();
        next.shortcuts.remove(position);
        self.persist(&next)?;
        *state = next;
        Ok(true)
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, PersistedLauncherShortcuts> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn persist(&self, state: &PersistedLauncherShortcuts) -> Result<(), String> {
        let parent = self
            .data_path
            .parent()
            .ok_or_else(|| "无法确定文件启动数据目录。".to_owned())?;
        fs::create_dir_all(parent).map_err(|error| format!("无法创建文件启动数据目录：{error}"))?;
        let encoded = serde_json::to_vec_pretty(state)
            .map_err(|error| format!("无法编码文件启动数据：{error}"))?;
        if encoded.len() > MAX_SHORTCUTS_FILE_BYTES {
            return Err(format!(
                "文件启动数据超过 {MAX_SHORTCUTS_FILE_BYTES} 字节的本机上限。"
            ));
        }

        let temporary = parent.join(format!(
            ".launcher-shortcuts-{}.tmp",
            Uuid::new_v4().simple()
        ));
        fs::write(&temporary, encoded).map_err(|error| format!("无法暂存文件启动数据：{error}"))?;
        if !self.data_path.exists() {
            return fs::rename(&temporary, self.data_path.as_ref()).map_err(|error| {
                let _ = fs::remove_file(&temporary);
                format!("无法保存文件启动数据：{error}")
            });
        }

        // Windows does not let `rename` replace an existing primary file.
        // Keep a validated backup recovery path for the small move/promote
        // interval rather than truncating the only copy in place.
        let backup = parent.join(format!(
            ".launcher-shortcuts-{}.backup",
            Uuid::new_v4().simple()
        ));
        fs::rename(self.data_path.as_ref(), &backup).map_err(|error| {
            let _ = fs::remove_file(&temporary);
            format!("无法准备文件启动数据更新：{error}")
        })?;
        if let Err(error) = fs::rename(&temporary, self.data_path.as_ref()) {
            let restore = fs::rename(&backup, self.data_path.as_ref());
            let _ = fs::remove_file(&temporary);
            return Err(match restore {
                Ok(()) => format!("无法保存文件启动数据：{error}"),
                Err(restore_error) => format!(
                    "无法保存文件启动数据（{error}），且无法恢复旧数据（{restore_error}）。"
                ),
            });
        }
        if let Err(error) = fs::remove_file(&backup) {
            host_log::warn(
                "shortcuts",
                format!("Could not remove a replaced launcher shortcut backup: {error}"),
            );
        }
        Ok(())
    }
}

fn shortcut_view(
    shortcut: &PersistedLauncherShortcut,
    index: &SearchIndex,
) -> LauncherShortcutView {
    let available = index
        .resolve_launcher_shortcut_source(&shortcut.source_id)
        .filter(|source| source.kind == shortcut.kind)
        .and_then(|source| prepare_live_source(&source, index).ok())
        .is_some();
    let status = if available { "ready" } else { "unavailable" }.to_owned();
    LauncherShortcutView {
        id: shortcut.id.clone(),
        name: shortcut.name.clone(),
        kind: shortcut.kind.clone(),
        metadata: shortcut.metadata.clone(),
        status,
    }
}

#[derive(Clone, Copy)]
enum LiveSourcePurpose {
    ImmediateSearchResult,
    PersistentShortcut,
}

fn prepare_live_source(
    source: &LauncherShortcutSource,
    index: &SearchIndex,
) -> Result<PreparedLocalOpen, String> {
    prepare_live_source_for(source, index, LiveSourcePurpose::PersistentShortcut)
}

fn prepare_live_source_for(
    source: &LauncherShortcutSource,
    index: &SearchIndex,
    purpose: LiveSourcePurpose,
) -> Result<PreparedLocalOpen, String> {
    let expected_kind = local_open_kind_for_source(source)?;
    let prepared = crate::system_open::prepare_local_open(&source.path, Some(expected_kind))
        .map_err(|error| format!("无法以受保护方式重新验证打开目标：{error}"))?;
    let supported_shape = match source.kind.as_str() {
        "file" | "folder" => true,
        "application" => match purpose {
            LiveSourcePurpose::ImmediateSearchResult => {
                supported_immediate_application_shape(prepared.path(), prepared.kind())
            }
            LiveSourcePurpose::PersistentShortcut => {
                supported_application_shape(prepared.path(), prepared.kind())
            }
        },
        _ => false,
    };
    if !supported_shape {
        return Err("固定目标的文件类型已变化或不再受支持；请重新固定。".to_owned());
    }
    let path_is_authorized = match purpose {
        LiveSourcePurpose::ImmediateSearchResult => {
            index.current_search_result_path_is_authorized(source, prepared.path())
        }
        LiveSourcePurpose::PersistentShortcut => {
            index.launcher_shortcut_path_is_authorized(source, prepared.path())
        }
    };
    if !path_is_authorized {
        return Err("固定目标不再位于当前授权范围内；为安全起见不会打开它。".to_owned());
    }
    Ok(prepared)
}

fn supported_application_shape(path: &Path, kind: LocalOpenKind) -> bool {
    #[cfg(target_os = "windows")]
    {
        kind == LocalOpenKind::File
            && path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    }

    #[cfg(target_os = "macos")]
    {
        kind == LocalOpenKind::Folder
            && path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = (path, kind);
        false
    }
}

fn supported_immediate_application_shape(path: &Path, kind: LocalOpenKind) -> bool {
    #[cfg(target_os = "windows")]
    {
        kind == LocalOpenKind::File
            && path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    matches!(
                        extension.to_ascii_lowercase().as_str(),
                        "lnk" | "url" | "appref-ms" | "exe"
                    )
                })
    }

    #[cfg(not(target_os = "windows"))]
    {
        supported_application_shape(path, kind)
    }
}

fn local_open_kind_for_source(source: &LauncherShortcutSource) -> Result<LocalOpenKind, String> {
    match source.kind.as_str() {
        "file" => Ok(LocalOpenKind::File),
        "folder" => Ok(LocalOpenKind::Folder),
        "application" => {
            #[cfg(target_os = "windows")]
            {
                Ok(LocalOpenKind::File)
            }
            #[cfg(target_os = "macos")]
            {
                Ok(LocalOpenKind::Folder)
            }
            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
            {
                Err("当前平台不支持直接打开该应用。".to_owned())
            }
        }
        _ => Err("打开目标的文件类型不受支持。".to_owned()),
    }
}

fn validate_shortcut(shortcut: &PersistedLauncherShortcut) -> Result<(), String> {
    if Uuid::parse_str(&shortcut.id).is_err()
        || shortcut.source_id.is_empty()
        || shortcut.source_id.len() > MAX_SOURCE_ID_BYTES
        || has_control_character(&shortcut.source_id)
        || shortcut.name.is_empty()
        || shortcut.name.len() > MAX_LABEL_BYTES
        || has_control_character(&shortcut.name)
        || shortcut.metadata.len() > MAX_METADATA_BYTES
        || has_control_character(&shortcut.metadata)
        || !matches!(shortcut.kind.as_str(), "file" | "folder" | "application")
        || shortcut.created_at.is_empty()
    {
        return Err("文件启动数据无效。".to_owned());
    }
    Ok(())
}

fn has_control_character(value: &str) -> bool {
    value.chars().any(char::is_control)
}

fn load_state(path: &Path) -> Result<PersistedLauncherShortcuts, String> {
    if !path.exists() {
        recover_interrupted_replace(path)?;
    }
    if !path.exists() {
        return Ok(PersistedLauncherShortcuts::default());
    }
    load_state_file(path)
}

fn load_state_file(path: &Path) -> Result<PersistedLauncherShortcuts, String> {
    let metadata = fs::metadata(path).map_err(|error| format!("无法读取文件启动数据：{error}"))?;
    if !metadata.is_file() || metadata.len() > MAX_SHORTCUTS_FILE_BYTES as u64 {
        return Err("文件启动数据文件无效或超过大小上限。".to_owned());
    }
    let bytes = fs::read(path).map_err(|error| format!("无法读取文件启动数据：{error}"))?;
    let parsed: PersistedLauncherShortcuts =
        serde_json::from_slice(&bytes).map_err(|error| format!("无法解析文件启动数据：{error}"))?;
    if parsed.schema_version != SHORTCUTS_SCHEMA_VERSION {
        return Err("文件启动数据版本不受支持。".to_owned());
    }

    let mut ids = HashSet::new();
    let mut source_ids = HashSet::new();
    let mut shortcuts = Vec::new();
    for shortcut in parsed.shortcuts {
        if validate_shortcut(&shortcut).is_err()
            || !ids.insert(shortcut.id.clone())
            || !source_ids.insert(shortcut.source_id.clone())
        {
            continue;
        }
        shortcuts.push(shortcut);
        if shortcuts.len() >= MAX_LAUNCHER_SHORTCUTS {
            break;
        }
    }
    Ok(PersistedLauncherShortcuts {
        schema_version: SHORTCUTS_SCHEMA_VERSION,
        shortcuts,
    })
}

fn recover_interrupted_replace(path: &Path) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    let Some(parent) = path.parent() else {
        return Err("无法确定文件启动数据目录。".to_owned());
    };
    if !parent.exists() {
        return Ok(());
    }
    let mut valid_backups = Vec::new();
    for entry in fs::read_dir(parent).map_err(|error| format!("无法检查文件启动备份：{error}"))?
    {
        let entry = entry.map_err(|error| format!("无法读取文件启动备份：{error}"))?;
        let backup = entry.path();
        let Some(name) = backup.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with(".launcher-shortcuts-") || !name.ends_with(".backup") {
            continue;
        }
        if load_state_file(&backup).is_ok() {
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
            .map_err(|error| format!("无法恢复中断前的文件启动数据：{error}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        thread,
        time::{Duration, Instant},
    };

    use super::*;

    fn temporary_store_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "ihub-launcher-shortcuts-test-{}.json",
            Uuid::new_v4()
        ))
    }

    fn temporary_search_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ihub-search-open-{label}-{}", Uuid::new_v4()))
    }

    fn wait_for_search_index_ready(index: &SearchIndex) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while index.status().phase != "ready" && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            index.status().phase,
            "ready",
            "the bounded test root should finish indexing",
        );
    }

    fn shortcut(id: &str, source_id: &str) -> PersistedLauncherShortcut {
        PersistedLauncherShortcut {
            id: id.to_owned(),
            source_id: source_id.to_owned(),
            name: "Project".to_owned(),
            kind: "folder".to_owned(),
            metadata: "Folder".to_owned(),
            created_at: "2026-01-01T00:00:00Z".to_owned(),
        }
    }

    #[test]
    fn restores_only_bounded_unique_host_shortcuts() {
        let path = temporary_store_path();
        let id = Uuid::new_v4().to_string();
        let duplicate_id = id.clone();
        let second_id = Uuid::new_v4().to_string();
        let payload = PersistedLauncherShortcuts {
            schema_version: SHORTCUTS_SCHEMA_VERSION,
            shortcuts: vec![
                shortcut(&id, "C:/Projects"),
                shortcut(&duplicate_id, "C:/Other"),
                shortcut(&second_id, "C:/Projects"),
                shortcut("not-a-uuid", "C:/Ignored"),
            ],
        };
        fs::write(&path, serde_json::to_vec(&payload).unwrap()).unwrap();

        let restored = load_state(&path).unwrap();
        assert_eq!(restored.shortcuts.len(), 1);
        assert_eq!(restored.shortcuts[0].source_id, "C:/Projects");

        let _ = fs::remove_file(path);
    }

    #[test]
    fn invalid_display_payload_cannot_be_restored() {
        let mut invalid = shortcut(&Uuid::new_v4().to_string(), "C:/Projects");
        invalid.name = "bad\nlabel".to_owned();
        assert!(validate_shortcut(&invalid).is_err());
    }

    #[test]
    fn unpin_only_removes_the_host_registry_record() {
        let path = temporary_store_path();
        let store = LauncherShortcutStore::with_path(path.clone());
        let id = Uuid::new_v4().to_string();
        {
            let mut state = store.lock_state();
            state.shortcuts.push(shortcut(&id, "C:/Projects"));
        }

        assert!(store.unpin(&id).unwrap());
        assert!(!store.unpin(&id).unwrap());
        assert!(load_state(&path).unwrap().shortcuts.is_empty());

        let _ = fs::remove_file(path);
    }

    #[test]
    fn immediate_search_open_requires_an_exact_current_index_result() {
        let first_root = temporary_search_root("first");
        let second_root = temporary_search_root("second");
        fs::create_dir_all(&first_root).unwrap();
        fs::create_dir_all(&second_root).unwrap();
        let target = first_root.join("open-me.txt");
        fs::write(&target, b"current result").unwrap();
        fs::write(second_root.join("other.txt"), b"replacement scope").unwrap();

        let index = SearchIndex::new();
        index
            .set_roots(vec![first_root.to_string_lossy().to_string()])
            .unwrap();
        wait_for_search_index_ready(&index);
        let result = index
            .search("open-me", Some(10))
            .into_iter()
            .find(|result| result.name == "open-me.txt")
            .expect("the current root should publish the test file");

        let resolved = resolve_current_search_result_open_target(&result.id, &index).unwrap();
        assert_eq!(resolved.path(), target.canonicalize().unwrap());
        assert_eq!(resolved.kind(), LocalOpenKind::File);
        assert!(
            resolve_current_search_result_open_target("unknown-result-id", &index).is_err(),
            "a renderer cannot invent an indexed target",
        );

        index
            .set_roots(vec![second_root.to_string_lossy().to_string()])
            .unwrap();
        wait_for_search_index_ready(&index);
        assert!(
            resolve_current_search_result_open_target(&result.id, &index).is_err(),
            "a result from the previous authorization scope must expire",
        );

        let _ = fs::remove_dir_all(first_root);
        let _ = fs::remove_dir_all(second_root);
    }

    #[test]
    fn live_search_target_must_remain_inside_the_active_root() {
        let authorized_root = temporary_search_root("authorized");
        let outside_root = temporary_search_root("outside");
        fs::create_dir_all(&authorized_root).unwrap();
        fs::create_dir_all(&outside_root).unwrap();
        let outside_file = outside_root.join("outside.txt");
        fs::write(&outside_file, b"not authorized").unwrap();

        let index = SearchIndex::new();
        index
            .set_roots(vec![authorized_root.to_string_lossy().to_string()])
            .unwrap();
        let source = LauncherShortcutSource {
            id: "outside".to_owned(),
            path: outside_file,
            name: "outside.txt".to_owned(),
            kind: "file".to_owned(),
            metadata: "File".to_owned(),
        };
        assert!(prepare_live_source(&source, &index).is_err());
        assert!(
            prepare_live_source_for(&source, &index, LiveSourcePurpose::ImmediateSearchResult)
                .is_err(),
            "an immediate result must not relax the active content-root boundary",
        );

        let _ = fs::remove_dir_all(authorized_root);
        let _ = fs::remove_dir_all(outside_root);
    }

    #[cfg(windows)]
    #[test]
    fn windows_immediate_application_shapes_accept_links_while_pins_remain_exe_only() {
        let root = temporary_search_root("windows-application-shapes");
        fs::create_dir_all(&root).unwrap();

        for (name, can_pin) in [
            ("Shortcut.lnk", false),
            ("Website.url", false),
            ("Deployment.appref-ms", false),
            ("Direct.exe", true),
        ] {
            let path = root.join(name);
            fs::write(&path, b"launch item").unwrap();
            assert!(
                supported_immediate_application_shape(&path, LocalOpenKind::File),
                "{name} must remain immediately openable",
            );
            assert_eq!(
                supported_application_shape(&path, LocalOpenKind::File),
                can_pin,
                "{name} must keep the persistent pin shape",
            );
        }

        let unsupported = root.join("Readme.txt");
        fs::write(&unsupported, b"not an application").unwrap();
        assert!(!supported_immediate_application_shape(
            &unsupported,
            LocalOpenKind::File,
        ));

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn live_search_target_does_not_follow_a_symlink_escape() {
        use std::os::unix::fs::symlink;

        let authorized_root = temporary_search_root("symlink-authorized");
        let outside_root = temporary_search_root("symlink-outside");
        fs::create_dir_all(&authorized_root).unwrap();
        fs::create_dir_all(&outside_root).unwrap();
        let outside_file = outside_root.join("outside.txt");
        fs::write(&outside_file, b"not authorized").unwrap();
        let link = authorized_root.join("link.txt");
        symlink(&outside_file, &link).unwrap();

        let index = SearchIndex::new();
        index
            .set_roots(vec![authorized_root.to_string_lossy().to_string()])
            .unwrap();
        let source = LauncherShortcutSource {
            id: "symlink".to_owned(),
            path: link,
            name: "link.txt".to_owned(),
            kind: "file".to_owned(),
            metadata: "File".to_owned(),
        };
        assert!(prepare_live_source(&source, &index).is_err());

        let _ = fs::remove_dir_all(authorized_root);
        let _ = fs::remove_dir_all(outside_root);
    }
}
