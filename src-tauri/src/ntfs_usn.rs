#![cfg_attr(not(windows), allow(dead_code))]

//! Windows NTFS USN Journal support.
//!
//! Non-Windows builds retain the serialized replay model and validators so a
//! portable snapshot can be rejected safely, while native journal operations
//! remain Windows-only. The replay/parser implementation also stays available
//! to cross-platform tests, so expected non-Windows dead code is allowed only
//! inside this platform module.
//!
//! P1a verifies that an explicitly authorised root belongs to a local NTFS
//! volume, captures the volume serial plus live journal watermarks, and
//! persists only that metadata. P1c additionally has a *read-only* MFT path
//! enumerator, but it is deliberately restricted to an explicitly authorised
//! drive root such as `C:\\`. A narrower root such as `C:\\Users\\me` must
//! never cause iHub to inspect the rest of the volume's file-name metadata, so
//! it remains on the scoped directory scanner. P1c closes only the short race
//! while a direct-drive MFT initialization is running by replaying a bounded,
//! read-only in-memory USN window. P1c itself does not persist file IDs or raw
//! USN records. P1e exposes a separately validated stable-path identity
//! binding for the indexer to opt into after it has indexed those paths; the
//! watcher and scoped scanner remain the continuous and recovery authorities.
//! P1d uses the saved checkpoint to prove a zero-change restart. When that
//! proof observes a bounded, continuous change, P1e can instead replay the
//! separately validated stable-path binding to a new quiet cutoff; any
//! unknown topology or continuity failure remains an explicit fallback.

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

use chrono::Utc;
use serde::{Deserialize, Serialize};

pub(crate) const CHECKPOINT_FILE_NAME: &str = "ntfs-usn-checkpoints-v1.json";
const CHECKPOINT_SCHEMA_VERSION: u8 = 1;
const MAX_CHECKPOINT_FILE_BYTES: u64 = 64 * 1024;
const MAX_CHECKPOINT_VOLUMES: usize = 32;
const MAX_FAILURE_DETAILS: usize = 3;
const MAX_MFT_ENUM_BUFFER_BYTES: usize = 64 * 1024;
const MAX_MFT_ENUM_CALLS: usize = 16_384;
const MAX_MFT_PATH_DEPTH: usize = 256;
const MAX_USN_REPLAY_BUFFER_BYTES: usize = 64 * 1024;
const MAX_USN_REPLAY_CALLS: usize = 512;
const MAX_USN_REPLAY_RECORDS: usize = 50_000;
/// The persistent P1e projection is intentionally bounded independently of
/// the JSON snapshot reader. It contains only paths the indexer already
/// indexed, plus the minimum NTFS identity metadata needed to replay a
/// strictly contiguous USN interval after restart.
pub(crate) const MAX_USN_REPLAY_STABLE_PATHS: usize = 500_000;
pub(crate) const USN_REPLAY_BINDING_SCHEMA_VERSION: u8 = 1;
const MAX_USN_REPLAY_PATH_MUTATIONS: usize = 1_000_000;
const NTFS_FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
const NTFS_FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
const USN_REASON_FILE_CREATE: u32 = 0x0000_0100;
const USN_REASON_FILE_DELETE: u32 = 0x0000_0200;
const USN_REASON_DATA_OVERWRITE: u32 = 0x0000_0001;
const USN_REASON_DATA_EXTEND: u32 = 0x0000_0002;
const USN_REASON_DATA_TRUNCATION: u32 = 0x0000_0004;
const USN_REASON_NAMED_DATA_OVERWRITE: u32 = 0x0000_0010;
const USN_REASON_NAMED_DATA_EXTEND: u32 = 0x0000_0020;
const USN_REASON_NAMED_DATA_TRUNCATION: u32 = 0x0000_0040;
const USN_REASON_EA_CHANGE: u32 = 0x0000_0400;
const USN_REASON_SECURITY_CHANGE: u32 = 0x0000_0800;
const USN_REASON_RENAME_OLD_NAME: u32 = 0x0000_1000;
const USN_REASON_RENAME_NEW_NAME: u32 = 0x0000_2000;
const USN_REASON_INDEXABLE_CHANGE: u32 = 0x0000_4000;
const USN_REASON_BASIC_INFO_CHANGE: u32 = 0x0000_8000;
const USN_REASON_HARD_LINK_CHANGE: u32 = 0x0001_0000;
const USN_REASON_COMPRESSION_CHANGE: u32 = 0x0002_0000;
const USN_REASON_ENCRYPTION_CHANGE: u32 = 0x0004_0000;
const USN_REASON_OBJECT_ID_CHANGE: u32 = 0x0008_0000;
const USN_REASON_REPARSE_POINT_CHANGE: u32 = 0x0010_0000;
const USN_REASON_STREAM_CHANGE: u32 = 0x0020_0000;
const USN_REASON_TRANSACTED_CHANGE: u32 = 0x0040_0000;
const USN_REASON_INTEGRITY_CHANGE: u32 = 0x0080_0000;
const USN_REASON_DESIRED_STORAGE_CLASS_CHANGE: u32 = 0x0100_0000;
const USN_REASON_CLOSE: u32 = 0x8000_0000;
const USN_PATH_TOPOLOGY_REASONS: u32 = USN_REASON_FILE_CREATE
    | USN_REASON_FILE_DELETE
    | USN_REASON_RENAME_OLD_NAME
    | USN_REASON_RENAME_NEW_NAME
    | USN_REASON_HARD_LINK_CHANGE
    | USN_REASON_REPARSE_POINT_CHANGE;
const USN_KNOWN_REASON_MASK: u32 = USN_REASON_DATA_OVERWRITE
    | USN_REASON_DATA_EXTEND
    | USN_REASON_DATA_TRUNCATION
    | USN_REASON_NAMED_DATA_OVERWRITE
    | USN_REASON_NAMED_DATA_EXTEND
    | USN_REASON_NAMED_DATA_TRUNCATION
    | USN_REASON_FILE_CREATE
    | USN_REASON_FILE_DELETE
    | USN_REASON_EA_CHANGE
    | USN_REASON_SECURITY_CHANGE
    | USN_REASON_RENAME_OLD_NAME
    | USN_REASON_RENAME_NEW_NAME
    | USN_REASON_INDEXABLE_CHANGE
    | USN_REASON_BASIC_INFO_CHANGE
    | USN_REASON_HARD_LINK_CHANGE
    | USN_REASON_COMPRESSION_CHANGE
    | USN_REASON_ENCRYPTION_CHANGE
    | USN_REASON_OBJECT_ID_CHANGE
    | USN_REASON_REPARSE_POINT_CHANGE
    | USN_REASON_STREAM_CHANGE
    | USN_REASON_TRANSACTED_CHANGE
    | USN_REASON_INTEGRITY_CHANGE
    | USN_REASON_DESIRED_STORAGE_CLASS_CHANGE
    | USN_REASON_CLOSE;

/// Only volume metadata is persisted. It intentionally contains no file IDs,
/// paths, journal records, or user content.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UsnCheckpoint {
    pub volume_key: String,
    pub volume_serial_number: u32,
    pub journal_id: u64,
    pub next_usn: i64,
    pub lowest_valid_usn: i64,
    pub observed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedUsnCheckpoints {
    schema_version: u8,
    checkpoints: Vec<UsnCheckpoint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UsnJournalState {
    volume_key: String,
    volume_serial_number: u32,
    journal_id: u64,
    next_usn: i64,
    lowest_valid_usn: i64,
}

#[derive(Debug, Default)]
pub(crate) struct LoadedUsnCheckpoints {
    pub checkpoints: Vec<UsnCheckpoint>,
    pub warning: Option<String>,
}

#[derive(Debug)]
pub(crate) struct UsnProbeOutcome {
    pub status: &'static str,
    pub message: String,
    pub eligible_volumes: usize,
    pub checkpointed_volumes: usize,
    pub checkpoints: Vec<UsnCheckpoint>,
}

/// A path projected from a live MFT record.
///
/// P1c uses `path` alone for its transient fast enumeration. P1e deliberately
/// keeps the complete identity tuple here so the indexer can construct a
/// stable-path binding *only* for entries it has successfully indexed. The
/// raw USN records remain transient; this is not a complete MFT graph.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct MftPathEntry {
    pub volume_key: String,
    pub path: PathBuf,
    pub file_reference_number: u64,
    pub parent_file_reference_number: u64,
    pub name: String,
    pub is_directory: bool,
    /// The authorised drive root is emitted as a synthetic directory entry.
    /// Its file and parent reference are both `root_file_reference_number`.
    pub is_root: bool,
}

/// The exact direct-drive root identity emitted alongside P1c MFT paths. It
/// lets the indexer create a replay binding without trying to reverse-engineer
/// an FRN from a reconstructed string path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UsnReplayVolumeSeed {
    pub volume_key: String,
    pub volume_root: PathBuf,
    pub root_file_reference_number: u64,
    /// Journal watermark captured after the P1c MFT initialization window was
    /// closed and replayed. A later P1e reader must start exactly here rather
    /// than from an unrelated final snapshot checkpoint.
    pub cutoff: UsnCheckpoint,
}

/// One indexed stable path in a P1e replay binding. `is_root` represents the
/// synthetic authorised drive root; every other path has a safe single name
/// component and a parent FRN that resolves through a directory chain in the
/// same volume projection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UsnReplayStablePath {
    pub path: PathBuf,
    pub file_reference_number: u64,
    pub parent_file_reference_number: u64,
    pub name: String,
    pub is_directory: bool,
    pub is_root: bool,
}

/// A bounded direct-drive identity projection for one NTFS USN Journal. It
/// intentionally stores only the successfully indexed aliases rather than a
/// full MFT graph or any raw Journal data.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UsnReplayVolume {
    pub volume_key: String,
    pub volume_root: PathBuf,
    pub root_file_reference_number: u64,
    pub paths: Vec<UsnReplayStablePath>,
}

/// The persisted P1e identity binding. The outer local-index snapshot owns
/// its atomic replacement; this type merely makes its replay-critical fields
/// explicit and schema-checked.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UsnReplayBinding {
    pub schema_version: u8,
    pub checkpoints: Vec<UsnCheckpoint>,
    pub volumes: Vec<UsnReplayVolume>,
}

/// A volume-qualified FRN in a replay result. FRNs are volume-local, so a
/// plain `u64` would be ambiguous for callers with several authorised drives.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct UsnReplayFileReference {
    pub volume_key: String,
    pub file_reference_number: u64,
}

/// The completed Windows P1e replay handoff. The native reader returns an
/// exact replacement binding plus every path and FRN whose user-visible
/// metadata must be reconciled before the indexer can publish a fast start.
#[derive(Debug, Clone)]
pub(crate) struct UsnReplayOutcome {
    pub binding: UsnReplayBinding,
    pub dirty_paths: Vec<PathBuf>,
    pub dirty_file_references: Vec<UsnReplayFileReference>,
    pub replayed_records: usize,
}

/// Result of the bounded, direct-drive MFT initialization path. `covered_roots`
/// contains only roots that were completely enumerated; every other root must
/// retain the normal scoped walker as its source of truth.
#[cfg(windows)]
#[derive(Debug)]
pub(crate) struct MftEnumerationOutcome {
    pub status: &'static str,
    pub message: String,
    pub covered_roots: Vec<PathBuf>,
    pub paths: Vec<MftPathEntry>,
    /// Root FRNs for the identity projection behind `paths`. These are still
    /// obtained only from explicit drive roots and are never inferred from
    /// strings by the caller.
    pub replay_seeds: Vec<UsnReplayVolumeSeed>,
    pub enumerated_records: usize,
    /// Records replayed only from the journal window opened before and closed
    /// immediately after this one MFT initialization. It is always zero for
    /// a quiet volume and is never persisted for a later run.
    pub replayed_usn_records: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VolumeTarget {
    volume_key: String,
    volume_root: String,
    device_path: String,
    /// An authorised path used only to prove it is on this direct drive-letter
    /// volume, rather than beneath an NTFS mount point on another volume.
    sample_root: String,
}

#[derive(Debug, Default)]
struct VolumeTargets {
    targets: Vec<VolumeTarget>,
    skipped_roots: usize,
}

#[cfg(windows)]
#[derive(Debug, Default)]
struct DirectVolumeTargets {
    targets: Vec<VolumeTarget>,
    skipped_roots: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MftRecord {
    file_reference_number: u64,
    parent_file_reference_number: u64,
    name: String,
    is_directory: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UsnDeltaRecord {
    file_reference_number: u64,
    parent_file_reference_number: u64,
    usn: i64,
    reason: u32,
    attributes: u32,
    name: String,
}

#[derive(Debug)]
struct ParsedMftReply {
    next_start_file_reference_number: u64,
    records: Vec<MftRecord>,
    record_count: usize,
}

#[derive(Debug)]
struct ParsedUsnDeltaReply {
    next_usn: i64,
    records: Vec<UsnDeltaRecord>,
    record_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckpointValidation {
    Missing,
    Valid,
    VolumeSerialChanged,
    JournalRecreated,
    AgedOut,
    AheadOfJournal,
}

/// Reads a bounded, schema-checked local checkpoint cache. Corruption is not
/// fatal: the next live probe creates fresh baselines and keeps the existing
/// directory scanner as the authoritative path source.
pub(crate) fn load_checkpoints(path: &Path) -> LoadedUsnCheckpoints {
    if !path.exists() {
        return LoadedUsnCheckpoints::default();
    }

    let metadata = match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_CHECKPOINT_FILE_BYTES => {
            metadata
        }
        Ok(_) => {
            return LoadedUsnCheckpoints {
                checkpoints: Vec::new(),
                warning: Some("USN 检查点文件不可用，已改为重新建立基线。".to_owned()),
            };
        }
        Err(error) => {
            return LoadedUsnCheckpoints {
                checkpoints: Vec::new(),
                warning: Some(format!("无法读取 USN 检查点，将重新建立基线：{error}")),
            };
        }
    };
    let _ = metadata;

    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return LoadedUsnCheckpoints {
                checkpoints: Vec::new(),
                warning: Some(format!("无法读取 USN 检查点，将重新建立基线：{error}")),
            };
        }
    };
    let payload = match serde_json::from_slice::<PersistedUsnCheckpoints>(&bytes) {
        Ok(payload) if payload.schema_version == CHECKPOINT_SCHEMA_VERSION => payload,
        _ => {
            return LoadedUsnCheckpoints {
                checkpoints: Vec::new(),
                warning: Some("USN 检查点格式已失效，已改为重新建立基线。".to_owned()),
            };
        }
    };
    if let Err(reason) = validate_checkpoint_set(&payload.checkpoints) {
        return LoadedUsnCheckpoints {
            checkpoints: Vec::new(),
            warning: Some(format!("USN 检查点校验失败（{reason}），已重新建立基线。")),
        };
    }

    LoadedUsnCheckpoints {
        checkpoints: payload.checkpoints,
        warning: None,
    }
}

/// Serializes a replacement checkpoint set. The caller owns atomic file
/// replacement so this module stays focused on eligibility and validation.
pub(crate) fn encode_checkpoints(checkpoints: &[UsnCheckpoint]) -> Result<Vec<u8>, String> {
    validate_checkpoint_set(checkpoints)?;
    let mut checkpoints = checkpoints.to_vec();
    checkpoints.sort_unstable_by(|left, right| left.volume_key.cmp(&right.volume_key));
    let bytes = serde_json::to_vec(&PersistedUsnCheckpoints {
        schema_version: CHECKPOINT_SCHEMA_VERSION,
        checkpoints,
    })
    .map_err(|error| format!("无法序列化 USN 检查点：{error}"))?;
    if bytes.len() as u64 > MAX_CHECKPOINT_FILE_BYTES {
        return Err("USN 检查点超出安全大小上限".to_owned());
    }
    Ok(bytes)
}

/// Validates the metadata-only payload used by P1d's zero-change fast-start
/// gate. This intentionally reuses the P1a checkpoint wire shape, but is
/// stricter about requiring at least one checkpoint. The payload contains
/// volume identity and journal watermarks only; it cannot carry a path, FRN,
/// or USN record.
pub(crate) fn validate_zero_change_checkpoint_payload(
    checkpoints: &[UsnCheckpoint],
) -> Result<(), String> {
    validate_checkpoint_set(checkpoints)?;
    if checkpoints.is_empty() {
        return Err("零变更快启缺少 USN 基线".to_owned());
    }
    Ok(())
}

/// Proves that a newly queried set of journal metadata is exactly compatible
/// with a saved P1d zero-change baseline. Unlike P1a checkpoint validation,
/// this is deliberately *not* a catch-up predicate: `next_usn` must match
/// exactly, so any write since capture causes an error and lets the caller use
/// its normal scanner/watcher recovery path instead.
///
/// This is platform-independent and pure so callers can validate a decoded
/// payload before attempting native volume access.
pub(crate) fn validate_zero_change_baseline(
    saved: &[UsnCheckpoint],
    current: &[UsnCheckpoint],
) -> Result<(), String> {
    validate_zero_change_checkpoint_payload(saved)?;
    validate_zero_change_checkpoint_payload(current)?;
    if saved.len() != current.len() {
        return Err("零变更快启的 USN 基线与当前卷数量不一致".to_owned());
    }

    let current_by_volume = current
        .iter()
        .map(|checkpoint| (checkpoint.volume_key.as_str(), checkpoint))
        .collect::<HashMap<_, _>>();

    for saved_checkpoint in saved {
        let Some(current_checkpoint) = current_by_volume.get(saved_checkpoint.volume_key.as_str())
        else {
            return Err("零变更快启缺少已保存的卷基线".to_owned());
        };
        if current_checkpoint.volume_serial_number != saved_checkpoint.volume_serial_number {
            return Err("零变更快启检测到卷序列号变化".to_owned());
        }
        if current_checkpoint.journal_id != saved_checkpoint.journal_id {
            return Err("零变更快启检测到 USN Journal 重建".to_owned());
        }
        if current_checkpoint.lowest_valid_usn > saved_checkpoint.next_usn {
            return Err("零变更快启的已保存 USN 水位已过期".to_owned());
        }
        if current_checkpoint.next_usn != saved_checkpoint.next_usn {
            return Err("零变更快启检测到自基线以来的文件变化".to_owned());
        }
    }

    Ok(())
}

/// Validates a decoded P1e stable-path binding before any native handle is
/// opened. This is deliberately stricter than the P1d zero-change proof: a
/// changed journal is permitted only when every saved identity, alias and
/// parent chain can be replayed without guessing.
///
/// The check is pure. It neither canonicalizes paths through the filesystem
/// nor opens a volume, so a malformed snapshot cannot broaden the user's
/// authorised scope before the Windows reader has a chance to fail closed.
pub(crate) fn validate_replay_binding(
    roots: &[PathBuf],
    binding: &UsnReplayBinding,
) -> Result<(), String> {
    if binding.schema_version != USN_REPLAY_BINDING_SCHEMA_VERSION {
        return Err("USN 增量回放绑定版本不受支持".to_owned());
    }
    validate_replay_volumes(roots, &binding.checkpoints, &binding.volumes)
}

/// Validates the independently persisted pieces of a P1e stable-path
/// binding. It is public to the indexer so it can reject an old or partially
/// written snapshot before handing its paths to the native replay layer.
pub(crate) fn validate_replay_volumes(
    roots: &[PathBuf],
    checkpoints: &[UsnCheckpoint],
    volumes: &[UsnReplayVolume],
) -> Result<(), String> {
    validate_zero_change_checkpoint_payload(checkpoints)?;
    let targets = collect_strict_direct_volume_targets(roots)?;
    validate_zero_change_baseline_targets(&targets, checkpoints)?;
    if volumes.len() != targets.len() {
        return Err("USN 增量回放卷投影与授权盘符根目录数量不一致".to_owned());
    }

    let targets_by_key = targets
        .iter()
        .map(|target| (target.volume_key.as_str(), target))
        .collect::<HashMap<_, _>>();
    let checkpoints_by_key = checkpoints
        .iter()
        .map(|checkpoint| (checkpoint.volume_key.as_str(), checkpoint))
        .collect::<HashMap<_, _>>();
    let mut seen_volumes = HashSet::new();
    let mut stable_path_count = 0usize;

    for volume in volumes {
        if !is_canonical_volume_key(&volume.volume_key) {
            return Err("USN 增量回放投影包含无效卷标识".to_owned());
        }
        if !seen_volumes.insert(volume.volume_key.clone()) {
            return Err("USN 增量回放投影包含重复卷标识".to_owned());
        }
        let Some(target) = targets_by_key.get(volume.volume_key.as_str()) else {
            return Err("USN 增量回放投影包含未授权卷".to_owned());
        };
        if !checkpoints_by_key.contains_key(volume.volume_key.as_str()) {
            return Err("USN 增量回放投影缺少对应的 Journal 检查点".to_owned());
        }
        let expected_root = normalized_windows_path_key(Path::new(&target.volume_root))?;
        let actual_root = normalized_windows_path_key(&volume.volume_root)?;
        if actual_root != expected_root {
            return Err("USN 增量回放投影的卷根目录不是显式授权盘符根目录".to_owned());
        }
        validate_replay_volume(volume, &actual_root)?;
        stable_path_count = stable_path_count.saturating_add(volume.paths.len());
        if stable_path_count > MAX_USN_REPLAY_STABLE_PATHS {
            return Err(format!(
                "USN 增量回放稳定路径超过安全上限 {MAX_USN_REPLAY_STABLE_PATHS}"
            ));
        }
    }

    if seen_volumes.len() != targets.len() {
        return Err("USN 增量回放投影缺少已授权卷".to_owned());
    }
    Ok(())
}

fn validate_replay_volume(volume: &UsnReplayVolume, volume_root_key: &str) -> Result<(), String> {
    if volume.root_file_reference_number == 0 {
        return Err("USN 增量回放投影缺少有效盘符根目录引用".to_owned());
    }
    if volume.paths.is_empty() {
        return Err("USN 增量回放投影缺少盘符根目录稳定路径".to_owned());
    }
    if volume.paths.len() > MAX_USN_REPLAY_STABLE_PATHS {
        return Err(format!(
            "单卷 USN 增量回放稳定路径超过安全上限 {MAX_USN_REPLAY_STABLE_PATHS}"
        ));
    }

    let mut paths_by_key = HashMap::<String, &UsnReplayStablePath>::new();
    let mut aliases_by_reference = HashMap::<u64, Vec<&UsnReplayStablePath>>::new();
    let mut root_count = 0usize;

    for path in &volume.paths {
        let path_key = normalized_windows_path_key(&path.path)?;
        if paths_by_key.insert(path_key.clone(), path).is_some() {
            return Err("USN 增量回放投影包含重复或大小写别名路径".to_owned());
        }
        if path.file_reference_number == 0 || path.parent_file_reference_number == 0 {
            return Err("USN 增量回放稳定路径包含无效文件引用".to_owned());
        }
        if path.is_root {
            root_count = root_count.saturating_add(1);
            if path.file_reference_number != volume.root_file_reference_number
                || path.parent_file_reference_number != volume.root_file_reference_number
                || !path.is_directory
                || !path.name.is_empty()
                || path_key != volume_root_key
            {
                return Err("USN 增量回放盘符根目录合成路径无效".to_owned());
            }
        } else {
            if path.file_reference_number == volume.root_file_reference_number
                || path.parent_file_reference_number == path.file_reference_number
                || !is_safe_mft_path_component(&path.name)
            {
                return Err("USN 增量回放稳定路径包含无效名称或根目录引用".to_owned());
            }
            if !is_path_key_within_root(&path_key, volume_root_key) {
                return Err("USN 增量回放稳定路径越过授权盘符根目录".to_owned());
            }
        }
        aliases_by_reference
            .entry(path.file_reference_number)
            .or_default()
            .push(path);
    }

    if root_count != 1 {
        return Err("USN 增量回放投影必须恰好包含一个盘符根目录合成路径".to_owned());
    }

    for (reference, aliases) in &aliases_by_reference {
        let Some(first) = aliases.first() else {
            return Err("USN 增量回放别名集合为空".to_owned());
        };
        if aliases
            .iter()
            .any(|alias| alias.is_directory != first.is_directory || alias.is_root != first.is_root)
        {
            return Err("USN 增量回放别名的目录属性不一致".to_owned());
        }
        if first.is_root && aliases.len() != 1 {
            return Err("USN 增量回放盘符根目录不能存在别名".to_owned());
        }
        // Directory aliases make descendant path reconstruction ambiguous.
        // File aliases are retained only as exact stable paths; a later
        // HARD_LINK_CHANGE record will still make the native replay fail
        // closed instead of trying to infer an additional alias.
        if first.is_directory && aliases.len() != 1 {
            return Err(format!("USN 增量回放目录引用 {reference} 存在多个别名"));
        }

        let mut alias_names = HashSet::new();
        for alias in aliases {
            if !alias_names.insert((
                alias.parent_file_reference_number,
                alias.name.to_uppercase(),
            )) {
                return Err("USN 增量回放投影包含重复文件引用别名".to_owned());
            }
        }
    }

    for path in &volume.paths {
        if path.is_root {
            continue;
        }
        let parent = aliases_by_reference
            .get(&path.parent_file_reference_number)
            .ok_or_else(|| "USN 增量回放稳定路径缺少父目录链".to_owned())?;
        if parent.len() != 1 || !parent[0].is_directory {
            return Err("USN 增量回放稳定路径的父引用不是唯一目录".to_owned());
        }
        let parent_key = normalized_windows_path_key(&parent[0].path)?;
        let expected_path = join_windows_path_key(&parent_key, &path.name)?;
        let actual_path = normalized_windows_path_key(&path.path)?;
        if expected_path != actual_path {
            return Err("USN 增量回放稳定路径与父目录/文件名不一致".to_owned());
        }
        validate_replay_parent_chain(
            path,
            &aliases_by_reference,
            volume.root_file_reference_number,
        )?;
    }

    Ok(())
}

fn validate_replay_parent_chain(
    path: &UsnReplayStablePath,
    aliases_by_reference: &HashMap<u64, Vec<&UsnReplayStablePath>>,
    root_reference: u64,
) -> Result<(), String> {
    let mut current = path.parent_file_reference_number;
    let mut visited = HashSet::new();
    for _ in 0..MAX_MFT_PATH_DEPTH {
        if current == root_reference {
            let root = aliases_by_reference
                .get(&current)
                .and_then(|aliases| aliases.first())
                .ok_or_else(|| "USN 增量回放父目录链缺少盘符根目录".to_owned())?;
            if root.is_root && root.is_directory {
                return Ok(());
            }
            return Err("USN 增量回放根目录引用不是合成目录路径".to_owned());
        }
        if !visited.insert(current) {
            return Err("USN 增量回放父目录链包含循环".to_owned());
        }
        let aliases = aliases_by_reference
            .get(&current)
            .ok_or_else(|| "USN 增量回放父目录链不完整".to_owned())?;
        if aliases.len() != 1 || !aliases[0].is_directory || aliases[0].is_root {
            return Err("USN 增量回放父目录链包含非唯一目录".to_owned());
        }
        current = aliases[0].parent_file_reference_number;
    }
    Err("USN 增量回放父目录链超过安全深度上限".to_owned())
}

/// Produces a lexical, case-insensitive Windows path key without resolving
/// symlinks, junctions or the filesystem. The replay binding uses it only to
/// reject ambiguous/corrupt snapshot paths; native metadata reads remain the
/// later authority for actual file existence and attributes.
fn normalized_windows_path_key(path: &Path) -> Result<String, String> {
    let raw = path.to_string_lossy();
    if raw.is_empty() || raw.contains('\0') {
        return Err("USN 增量回放路径为空或包含 NUL".to_owned());
    }
    // The normal verbatim path spelling is accepted, but the Win32 device
    // namespace is not: later metadata reads use ordinary paths rather than
    // raw device handles.
    if raw.starts_with(r"\\.\") {
        return Err("USN 增量回放路径不能使用 Win32 设备命名空间".to_owned());
    }
    let raw = raw.strip_prefix(r"\\?\").unwrap_or(&raw).replace('/', "\\");
    let bytes = raw.as_bytes();
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' || bytes[2] != b'\\' {
        return Err("USN 增量回放路径不是绝对本地盘符路径".to_owned());
    }
    let drive = (bytes[0] as char).to_ascii_uppercase();
    let suffix = &raw[3..];
    if suffix.is_empty() {
        return Ok(format!("{drive}:\\"));
    }
    let mut components = Vec::new();
    for component in suffix.split('\\') {
        if !is_safe_mft_path_component(component) {
            return Err("USN 增量回放路径包含非规范组件".to_owned());
        }
        components.push(component.to_uppercase());
    }
    Ok(format!("{drive}:\\{}", components.join("\\")))
}

fn join_windows_path_key(parent: &str, name: &str) -> Result<String, String> {
    if !is_safe_mft_path_component(name) {
        return Err("USN 增量回放路径名称不安全".to_owned());
    }
    let separator = if parent.ends_with('\\') { "" } else { "\\" };
    Ok(format!("{parent}{separator}{}", name.to_uppercase()))
}

fn is_path_key_within_root(path: &str, root: &str) -> bool {
    path != root
        && path.strip_prefix(root).is_some_and(|suffix| {
            !suffix.is_empty() && (root.ends_with('\\') || suffix.starts_with('\\'))
        })
}

#[derive(Debug, Clone)]
struct PendingReplayRename {
    old: UsnReplayStablePath,
    old_path_key: String,
}

#[derive(Debug, Default)]
struct ReplayDirtySet {
    // Keep the raw spelling rather than folding by Windows path key. A
    // case-only rename (for example `Foo` -> `FOO`) needs both the stale and
    // replacement spellings handed to the indexer; folding them here would
    // silently drop the new path and leave the snapshot incomplete.
    paths: HashSet<PathBuf>,
    file_references: HashSet<UsnReplayFileReference>,
}

impl ReplayDirtySet {
    fn mark(&mut self, volume_key: &str, path: &UsnReplayStablePath) -> Result<(), String> {
        let _ = normalized_windows_path_key(&path.path)?;
        self.paths.insert(path.path.clone());
        self.file_references.insert(UsnReplayFileReference {
            volume_key: volume_key.to_owned(),
            file_reference_number: path.file_reference_number,
        });
        Ok(())
    }

    fn mark_many(
        &mut self,
        volume_key: &str,
        paths: impl IntoIterator<Item = UsnReplayStablePath>,
    ) -> Result<(), String> {
        for path in paths {
            self.mark(volume_key, &path)?;
        }
        Ok(())
    }

    fn into_parts(mut self) -> (Vec<PathBuf>, Vec<UsnReplayFileReference>) {
        let mut paths = self.paths.drain().collect::<Vec<_>>();
        paths.sort_unstable_by(|left, right| left.to_string_lossy().cmp(&right.to_string_lossy()));
        let mut file_references = self.file_references.drain().collect::<Vec<_>>();
        file_references.sort_unstable_by(|left, right| {
            left.volume_key
                .cmp(&right.volume_key)
                .then_with(|| left.file_reference_number.cmp(&right.file_reference_number))
        });
        (paths, file_references)
    }
}

/// A mutable, bounded projection of the stable paths persisted by P1e. It
/// retains only those aliases the indexer actually indexed; it never asks the
/// filesystem to fill a gap. Any USN event that cannot be represented exactly
/// returns an error so the caller can discard the entire fast-start attempt.
#[derive(Debug)]
struct StableReplayPathState {
    volume_key: String,
    volume_root: PathBuf,
    root_file_reference_number: u64,
    aliases_by_reference: HashMap<u64, Vec<UsnReplayStablePath>>,
    path_keys: HashMap<String, u64>,
    path_count: usize,
    pending_renames: HashMap<u64, PendingReplayRename>,
    deleted_aliases: HashMap<(u64, u64, String), UsnReplayStablePath>,
    mutation_count: usize,
}

impl StableReplayPathState {
    fn from_volume(volume: &UsnReplayVolume) -> Result<Self, String> {
        let root_key = normalized_windows_path_key(&volume.volume_root)?;
        validate_replay_volume(volume, &root_key)?;

        let mut aliases_by_reference = HashMap::<u64, Vec<UsnReplayStablePath>>::new();
        let mut path_keys = HashMap::<String, u64>::new();
        for path in &volume.paths {
            let path_key = normalized_windows_path_key(&path.path)?;
            if path_keys
                .insert(path_key, path.file_reference_number)
                .is_some()
            {
                return Err("USN 增量回放稳定路径包含重复路径".to_owned());
            }
            aliases_by_reference
                .entry(path.file_reference_number)
                .or_default()
                .push(path.clone());
        }

        Ok(Self {
            volume_key: volume.volume_key.clone(),
            volume_root: volume.volume_root.clone(),
            root_file_reference_number: volume.root_file_reference_number,
            aliases_by_reference,
            path_keys,
            path_count: volume.paths.len(),
            pending_renames: HashMap::new(),
            deleted_aliases: HashMap::new(),
            mutation_count: 0,
        })
    }

    fn into_volume(self) -> Result<UsnReplayVolume, String> {
        if !self.pending_renames.is_empty() {
            return Err("USN 增量回放存在未配对的重命名事件".to_owned());
        }
        let mut paths = self
            .aliases_by_reference
            .into_values()
            .flatten()
            .collect::<Vec<_>>();
        paths.sort_unstable_by(|left, right| {
            left.is_root
                .cmp(&right.is_root)
                .reverse()
                .then_with(|| left.path.cmp(&right.path))
                .then_with(|| left.file_reference_number.cmp(&right.file_reference_number))
                .then_with(|| {
                    left.parent_file_reference_number
                        .cmp(&right.parent_file_reference_number)
                })
                .then_with(|| left.name.cmp(&right.name))
        });
        let volume = UsnReplayVolume {
            volume_key: self.volume_key,
            volume_root: self.volume_root,
            root_file_reference_number: self.root_file_reference_number,
            paths,
        };
        let root_key = normalized_windows_path_key(&volume.volume_root)?;
        validate_replay_volume(&volume, &root_key)?;
        Ok(volume)
    }

    fn finish(&self) -> Result<(), String> {
        if self.pending_renames.is_empty() {
            Ok(())
        } else {
            Err("USN 增量回放存在未配对的重命名事件".to_owned())
        }
    }

    fn mutation_count(&self) -> usize {
        self.mutation_count
    }

    fn apply_delta(
        &mut self,
        record: &UsnDeltaRecord,
        dirty: &mut ReplayDirtySet,
    ) -> Result<(), String> {
        self.validate_delta_record(record)?;
        let action = record.reason & USN_PATH_TOPOLOGY_REASONS;
        if action != 0 && action.count_ones() != 1 {
            return Err("USN 增量回放记录同时包含多个路径拓扑动作".to_owned());
        }
        if self
            .pending_renames
            .contains_key(&record.file_reference_number)
            && action != USN_REASON_RENAME_NEW_NAME
        {
            return Err("USN 增量回放重命名旧名称未立即获得匹配的新名称".to_owned());
        }

        match action {
            0 => self.apply_metadata_delta(record, dirty),
            USN_REASON_FILE_CREATE => self.apply_create_delta(record, dirty),
            USN_REASON_FILE_DELETE => self.apply_delete_delta(record, dirty),
            USN_REASON_RENAME_OLD_NAME => self.apply_rename_old_delta(record, dirty),
            USN_REASON_RENAME_NEW_NAME => self.apply_rename_new_delta(record, dirty),
            _ => Err("USN 增量回放包含不支持的路径拓扑动作".to_owned()),
        }
    }

    fn validate_delta_record(&self, record: &UsnDeltaRecord) -> Result<(), String> {
        if record.usn < 0 {
            return Err("USN 增量回放记录包含负水位".to_owned());
        }
        if record.reason == 0 || record.reason & !USN_KNOWN_REASON_MASK != 0 {
            return Err("USN 增量回放记录包含未知原因位".to_owned());
        }
        if record.reason & (USN_REASON_HARD_LINK_CHANGE | USN_REASON_REPARSE_POINT_CHANGE) != 0 {
            return Err("USN 增量回放不支持硬链接或重解析点变更".to_owned());
        }
        if record.attributes & NTFS_FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err("USN 增量回放记录涉及重解析点".to_owned());
        }
        if record.file_reference_number == 0
            || record.parent_file_reference_number == 0
            || record.file_reference_number == self.root_file_reference_number
            || record.parent_file_reference_number == record.file_reference_number
            || !is_safe_mft_path_component(&record.name)
        {
            return Err("USN 增量回放记录包含无效文件引用、根目录或名称".to_owned());
        }
        Ok(())
    }

    fn apply_metadata_delta(
        &mut self,
        record: &UsnDeltaRecord,
        dirty: &mut ReplayDirtySet,
    ) -> Result<(), String> {
        let alias = self.find_exact_alias(record)?;
        self.assert_directory_kind(&alias, record)?;
        let aliases = self
            .aliases_by_reference
            .get(&record.file_reference_number)
            .cloned()
            .ok_or_else(|| "USN 增量回放遇到未知文件引用".to_owned())?;
        self.consume_mutation_budget(aliases.len())?;
        dirty.mark_many(&self.volume_key, aliases)
    }

    fn apply_create_delta(
        &mut self,
        record: &UsnDeltaRecord,
        dirty: &mut ReplayDirtySet,
    ) -> Result<(), String> {
        if self
            .aliases_by_reference
            .contains_key(&record.file_reference_number)
            || self
                .deleted_aliases
                .keys()
                .any(|(reference, _, _)| *reference == record.file_reference_number)
        {
            return Err("USN 增量回放创建事件重用了已知文件引用".to_owned());
        }
        let parent = self.require_parent_directory(record.parent_file_reference_number)?;
        let path = parent.path.join(&record.name);
        let path_key = normalized_windows_path_key(&path)?;
        if self.path_keys.contains_key(&path_key) {
            return Err("USN 增量回放创建事件与现有稳定路径冲突".to_owned());
        }
        if self.path_count >= MAX_USN_REPLAY_STABLE_PATHS {
            return Err("USN 增量回放稳定路径超过安全上限".to_owned());
        }
        self.consume_mutation_budget(1)?;
        let path = UsnReplayStablePath {
            path,
            file_reference_number: record.file_reference_number,
            parent_file_reference_number: record.parent_file_reference_number,
            name: record.name.clone(),
            is_directory: record.attributes & NTFS_FILE_ATTRIBUTE_DIRECTORY != 0,
            is_root: false,
        };
        self.path_keys.insert(path_key, path.file_reference_number);
        self.aliases_by_reference
            .insert(path.file_reference_number, vec![path.clone()]);
        self.path_count = self.path_count.saturating_add(1);
        dirty.mark(&self.volume_key, &path)
    }

    fn apply_delete_delta(
        &mut self,
        record: &UsnDeltaRecord,
        dirty: &mut ReplayDirtySet,
    ) -> Result<(), String> {
        let alias_key = replay_alias_key(
            record.file_reference_number,
            record.parent_file_reference_number,
            &record.name,
        );
        let alias = match self
            .aliases_by_reference
            .contains_key(&record.file_reference_number)
        {
            true => self.find_exact_alias(record)?,
            false => {
                let Some(deleted) = self.deleted_aliases.remove(&alias_key) else {
                    return Err("USN 增量回放删除事件引用未知稳定路径".to_owned());
                };
                self.assert_directory_kind(&deleted, record)?;
                self.consume_mutation_budget(1)?;
                return dirty.mark(&self.volume_key, &deleted);
            }
        };
        self.assert_directory_kind(&alias, record)?;
        let removed = if alias.is_directory {
            self.subtree_entries(&alias)?
        } else {
            vec![alias]
        };
        self.consume_mutation_budget(removed.len())?;
        dirty.mark_many(&self.volume_key, removed.clone())?;
        self.remove_entries(removed)?;
        Ok(())
    }

    fn apply_rename_old_delta(
        &mut self,
        record: &UsnDeltaRecord,
        dirty: &mut ReplayDirtySet,
    ) -> Result<(), String> {
        if self
            .pending_renames
            .contains_key(&record.file_reference_number)
        {
            return Err("USN 增量回放检测到重复的重命名旧名称".to_owned());
        }
        let old = self.find_exact_alias(record)?;
        self.assert_directory_kind(&old, record)?;
        let old_path_key = normalized_windows_path_key(&old.path)?;
        let changed = if old.is_directory {
            self.subtree_entries(&old)?
        } else {
            vec![old.clone()]
        };
        self.consume_mutation_budget(changed.len())?;
        dirty.mark_many(&self.volume_key, changed)?;
        self.pending_renames.insert(
            record.file_reference_number,
            PendingReplayRename { old, old_path_key },
        );
        Ok(())
    }

    fn apply_rename_new_delta(
        &mut self,
        record: &UsnDeltaRecord,
        dirty: &mut ReplayDirtySet,
    ) -> Result<(), String> {
        let pending = self
            .pending_renames
            .remove(&record.file_reference_number)
            .ok_or_else(|| "USN 增量回放重命名新名称没有匹配的旧名称".to_owned())?;
        self.assert_directory_kind(&pending.old, record)?;
        let parent = self.require_parent_directory(record.parent_file_reference_number)?;
        let parent_key = normalized_windows_path_key(&parent.path)?;
        if pending.old.is_directory
            && (parent_key == pending.old_path_key
                || is_path_key_within_root(&parent_key, &pending.old_path_key))
        {
            return Err("USN 增量回放目录重命名会形成父目录循环".to_owned());
        }

        let new_root_path = parent.path.join(&record.name);
        let _new_root_key = normalized_windows_path_key(&new_root_path)?;
        let changed = if pending.old.is_directory {
            self.subtree_entries(&pending.old)?
        } else {
            vec![pending.old.clone()]
        };
        self.consume_mutation_budget(changed.len())?;

        let changed_keys = changed
            .iter()
            .map(|entry| normalized_windows_path_key(&entry.path))
            .collect::<Result<HashSet<_>, _>>()?;
        let mut mutations = Vec::with_capacity(changed.len());
        let mut new_keys = HashSet::new();
        for entry in changed {
            let old_key = normalized_windows_path_key(&entry.path)?;
            let new_path = if old_key == pending.old_path_key {
                new_root_path.clone()
            } else {
                replace_stable_path_prefix(&entry.path, &pending.old.path, &new_root_path)?
            };
            let new_key = normalized_windows_path_key(&new_path)?;
            if !new_keys.insert(new_key.clone()) {
                return Err("USN 增量回放目录重命名产生重复路径".to_owned());
            }
            if self
                .path_keys
                .get(&new_key)
                .is_some_and(|_| !changed_keys.contains(&new_key))
            {
                return Err("USN 增量回放重命名与现有稳定路径冲突".to_owned());
            }
            mutations.push((entry, old_key, new_path, new_key));
        }

        for (_, old_key, _, _) in &mutations {
            self.path_keys.remove(old_key);
        }
        let mut updated = Vec::with_capacity(mutations.len());
        for (old, _old_key, new_path, new_key) in mutations {
            let aliases = self
                .aliases_by_reference
                .get_mut(&old.file_reference_number)
                .ok_or_else(|| "USN 增量回放重命名期间丢失文件引用".to_owned())?;
            let slot = aliases
                .iter_mut()
                .find(|candidate| {
                    candidate.parent_file_reference_number == old.parent_file_reference_number
                        && candidate.name == old.name
                        && candidate.path == old.path
                })
                .ok_or_else(|| "USN 增量回放重命名期间丢失稳定别名".to_owned())?;
            slot.path = new_path;
            if old.file_reference_number == record.file_reference_number
                && old.parent_file_reference_number == pending.old.parent_file_reference_number
                && old.name == pending.old.name
            {
                slot.parent_file_reference_number = record.parent_file_reference_number;
                slot.name = record.name.clone();
            }
            self.path_keys.insert(new_key, old.file_reference_number);
            updated.push(slot.clone());
        }
        dirty.mark_many(&self.volume_key, updated)
    }

    fn find_exact_alias(&self, record: &UsnDeltaRecord) -> Result<UsnReplayStablePath, String> {
        let aliases = self
            .aliases_by_reference
            .get(&record.file_reference_number)
            .ok_or_else(|| "USN 增量回放遇到未知文件引用".to_owned())?;
        let expected_name = record.name.to_uppercase();
        let mut matches = aliases
            .iter()
            .filter(|alias| {
                alias.parent_file_reference_number == record.parent_file_reference_number
                    && alias.name.to_uppercase() == expected_name
            })
            .cloned();
        let alias = matches
            .next()
            .ok_or_else(|| "USN 增量回放记录不匹配任何稳定别名".to_owned())?;
        if matches.next().is_some() {
            return Err("USN 增量回放记录匹配多个稳定别名".to_owned());
        }
        Ok(alias)
    }

    fn assert_directory_kind(
        &self,
        stable: &UsnReplayStablePath,
        record: &UsnDeltaRecord,
    ) -> Result<(), String> {
        if stable.is_directory != (record.attributes & NTFS_FILE_ATTRIBUTE_DIRECTORY != 0) {
            return Err("USN 增量回放记录的目录属性与稳定路径不一致".to_owned());
        }
        Ok(())
    }

    fn require_parent_directory(
        &self,
        parent_reference: u64,
    ) -> Result<UsnReplayStablePath, String> {
        let aliases = self
            .aliases_by_reference
            .get(&parent_reference)
            .ok_or_else(|| "USN 增量回放记录缺少已知父目录".to_owned())?;
        if aliases.len() != 1 || !aliases[0].is_directory {
            return Err("USN 增量回放记录的父引用不是唯一目录".to_owned());
        }
        Ok(aliases[0].clone())
    }

    fn subtree_entries(
        &self,
        root: &UsnReplayStablePath,
    ) -> Result<Vec<UsnReplayStablePath>, String> {
        let root_key = normalized_windows_path_key(&root.path)?;
        let mut entries = Vec::new();
        for entry in self.aliases_by_reference.values().flatten() {
            let key = normalized_windows_path_key(&entry.path)?;
            if key == root_key || is_path_key_within_root(&key, &root_key) {
                entries.push(entry.clone());
            }
        }
        if entries.is_empty() {
            return Err("USN 增量回放目录子树不存在".to_owned());
        }
        entries.sort_unstable_by(|left, right| left.path.cmp(&right.path));
        if entries.len() > MAX_USN_REPLAY_STABLE_PATHS {
            return Err("USN 增量回放目录子树超过安全上限".to_owned());
        }
        Ok(entries)
    }

    fn remove_entries(&mut self, entries: Vec<UsnReplayStablePath>) -> Result<(), String> {
        let mut removed_keys = HashSet::new();
        for entry in &entries {
            let path_key = normalized_windows_path_key(&entry.path)?;
            removed_keys.insert(path_key);
            self.deleted_aliases.insert(
                replay_alias_key(
                    entry.file_reference_number,
                    entry.parent_file_reference_number,
                    &entry.name,
                ),
                entry.clone(),
            );
        }
        if self.deleted_aliases.len() > MAX_USN_REPLAY_STABLE_PATHS {
            return Err("USN 增量回放删除墓碑超过安全上限".to_owned());
        }
        for key in &removed_keys {
            self.path_keys.remove(key);
        }
        let references = self
            .aliases_by_reference
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for reference in references {
            let mut remove_reference = false;
            if let Some(aliases) = self.aliases_by_reference.get_mut(&reference) {
                let mut retained = Vec::with_capacity(aliases.len());
                for entry in std::mem::take(aliases) {
                    let key = normalized_windows_path_key(&entry.path)?;
                    if !removed_keys.contains(&key) {
                        retained.push(entry);
                    }
                }
                *aliases = retained;
                remove_reference = aliases.is_empty();
            }
            if remove_reference {
                self.aliases_by_reference.remove(&reference);
            }
        }
        self.path_count = self.path_count.saturating_sub(removed_keys.len());
        Ok(())
    }

    fn consume_mutation_budget(&mut self, amount: usize) -> Result<(), String> {
        self.mutation_count = self.mutation_count.saturating_add(amount);
        if self.mutation_count > MAX_USN_REPLAY_PATH_MUTATIONS {
            return Err("USN 增量回放路径变更超过安全上限".to_owned());
        }
        Ok(())
    }
}

fn replay_alias_key(
    file_reference_number: u64,
    parent_file_reference_number: u64,
    name: &str,
) -> (u64, u64, String) {
    (
        file_reference_number,
        parent_file_reference_number,
        name.to_uppercase(),
    )
}

fn replace_stable_path_prefix(
    path: &Path,
    old_prefix: &Path,
    new_prefix: &Path,
) -> Result<PathBuf, String> {
    let suffix = path
        .strip_prefix(old_prefix)
        .map_err(|_| "USN 增量回放目录重命名无法重建子路径".to_owned())?;
    if suffix.as_os_str().is_empty() {
        Ok(new_prefix.to_path_buf())
    } else {
        Ok(new_prefix.join(suffix))
    }
}

/// Replays a saved stable-path identity binding to one quiet Windows USN
/// cutoff. The reader opens only explicitly authorised direct-drive NTFS
/// volumes and performs no Journal mutation. A write during the read, a root
/// identity change, a Journal gap, or a topology event that cannot be applied
/// exactly returns `Err`; callers must then retain their ordinary scanner and
/// watcher recovery path.
#[cfg(windows)]
pub(crate) fn replay_binding_to_quiet_cutoff(
    roots: &[PathBuf],
    binding: &UsnReplayBinding,
) -> Result<UsnReplayOutcome, String> {
    validate_replay_binding(roots, binding)?;
    let targets = collect_strict_direct_volume_targets(roots)?;
    let checkpoints = binding
        .checkpoints
        .iter()
        .map(|checkpoint| (checkpoint.volume_key.as_str(), checkpoint))
        .collect::<HashMap<_, _>>();
    let volumes = binding
        .volumes
        .iter()
        .map(|volume| (volume.volume_key.as_str(), volume))
        .collect::<HashMap<_, _>>();

    let mut dirty = ReplayDirtySet::default();
    let mut replayed_records = 0usize;
    let mut replayed_path_mutations = 0usize;
    let mut updated_volumes = Vec::with_capacity(targets.len());
    let mut cutoff_checkpoints = Vec::with_capacity(targets.len());

    for target in targets {
        let checkpoint = checkpoints
            .get(target.volume_key.as_str())
            .ok_or_else(|| "USN 增量回放缺少已授权卷的检查点".to_owned())?;
        let volume = volumes
            .get(target.volume_key.as_str())
            .ok_or_else(|| "USN 增量回放缺少已授权卷的稳定路径投影".to_owned())?;
        let initial = query_usn_journal(&target).map_err(|error| {
            format!(
                "无法读取 USN 增量回放起始水位（{}）：{}",
                target.volume_key,
                error.describe()
            )
        })?;
        validate_persisted_replay_window(checkpoint, &initial)?;
        let root_reference =
            volume_root_file_reference(&target.volume_root, initial.volume_serial_number).map_err(
                |error| {
                    format!(
                        "无法验证 USN 增量回放盘符根目录（{}）：{}",
                        target.volume_key,
                        error.describe()
                    )
                },
            )?;
        if root_reference != volume.root_file_reference_number {
            return Err(format!(
                "USN 增量回放检测到盘符根目录引用变化（{}）",
                target.volume_key
            ));
        }

        let handle = open_readonly_volume(&target).map_err(|error| {
            format!(
                "无法以只读方式打开 USN 增量回放卷（{}）：{}",
                target.volume_key,
                error.describe()
            )
        })?;
        let mut state = StableReplayPathState::from_volume(volume)?;
        replayed_records = replayed_records.saturating_add(replay_stable_deltas(
            handle.0, checkpoint, &initial, &mut state, &mut dirty,
        )?);
        if replayed_records > MAX_USN_REPLAY_RECORDS {
            return Err("USN 增量回放记录超过全局安全上限".to_owned());
        }
        state.finish()?;
        replayed_path_mutations = replayed_path_mutations.saturating_add(state.mutation_count());
        if replayed_path_mutations > MAX_USN_REPLAY_PATH_MUTATIONS {
            return Err("USN 增量回放路径变更超过全局安全上限".to_owned());
        }

        let after = query_usn_journal(&target).map_err(|error| {
            format!(
                "无法确认 USN 增量回放截止水位（{}）：{}",
                target.volume_key,
                error.describe()
            )
        })?;
        validate_quiet_replay_cutoff(&initial, &after)?;
        let after_root_reference =
            volume_root_file_reference(&target.volume_root, after.volume_serial_number).map_err(
                |error| {
                    format!(
                        "无法确认 USN 增量回放盘符根目录（{}）：{}",
                        target.volume_key,
                        error.describe()
                    )
                },
            )?;
        if after_root_reference != root_reference {
            return Err(format!(
                "USN 增量回放期间盘符根目录引用发生变化（{}）",
                target.volume_key
            ));
        }

        cutoff_checkpoints.push(UsnCheckpoint::from(initial));
        updated_volumes.push(state.into_volume()?);
    }

    cutoff_checkpoints.sort_unstable_by(|left, right| left.volume_key.cmp(&right.volume_key));
    updated_volumes.sort_unstable_by(|left, right| left.volume_key.cmp(&right.volume_key));
    let binding = UsnReplayBinding {
        schema_version: USN_REPLAY_BINDING_SCHEMA_VERSION,
        checkpoints: cutoff_checkpoints,
        volumes: updated_volumes,
    };
    validate_replay_binding(roots, &binding)?;
    // Volumes are replayed serially. A per-volume quiet check is not enough:
    // while D: is being read, C: could advance after its own check. Re-query
    // the complete authorised set after the final projection has been built,
    // so callers never receive a mixed-cutoff binding. The indexer performs a
    // second check immediately before its atomic snapshot replacement to
    // cover the later metadata reconciliation and serialization window.
    verify_zero_change_baseline(roots, &binding.checkpoints)
        .map_err(|error| format!("USN 增量回放最终多卷截止校验失败：{error}"))?;
    let (dirty_paths, dirty_file_references) = dirty.into_parts();
    Ok(UsnReplayOutcome {
        binding,
        dirty_paths,
        dirty_file_references,
        replayed_records,
    })
}

/// Non-Windows platforms cannot prove NTFS Journal continuity. Returning an
/// explicit error keeps the caller on the ordinary scoped scan path instead
/// of silently accepting a stale stable-path projection.
#[cfg(not(windows))]
pub(crate) fn replay_binding_to_quiet_cutoff(
    _roots: &[PathBuf],
    _binding: &UsnReplayBinding,
) -> Result<UsnReplayOutcome, String> {
    Err("USN 增量回放仅支持 Windows NTFS 直接盘符根目录".to_owned())
}

fn validate_persisted_replay_window(
    saved: &UsnCheckpoint,
    cutoff: &UsnJournalState,
) -> Result<(), String> {
    if saved.volume_key != cutoff.volume_key {
        return Err("USN 增量回放卷标识发生变化".to_owned());
    }
    if saved.volume_serial_number != cutoff.volume_serial_number {
        return Err("USN 增量回放卷序列号发生变化".to_owned());
    }
    if saved.journal_id != cutoff.journal_id {
        return Err("USN 增量回放检测到 Journal 重建".to_owned());
    }
    if saved.next_usn < saved.lowest_valid_usn
        || cutoff.next_usn < cutoff.lowest_valid_usn
        || cutoff.lowest_valid_usn > saved.next_usn
    {
        return Err("USN 增量回放所需 Journal 区间已失效".to_owned());
    }
    if cutoff.next_usn < saved.next_usn {
        return Err("USN 增量回放 Journal 水位倒退".to_owned());
    }
    Ok(())
}

fn validate_quiet_replay_cutoff(
    cutoff: &UsnJournalState,
    after: &UsnJournalState,
) -> Result<(), String> {
    if cutoff.volume_key != after.volume_key
        || cutoff.volume_serial_number != after.volume_serial_number
        || cutoff.journal_id != after.journal_id
    {
        return Err("USN 增量回放期间卷或 Journal 标识发生变化".to_owned());
    }
    if after.lowest_valid_usn > cutoff.next_usn || after.next_usn != cutoff.next_usn {
        return Err("USN 增量回放期间 Journal 截止水位发生变化".to_owned());
    }
    Ok(())
}

/// Reads a bounded contiguous Journal interval from a persisted checkpoint to
/// an already queried cutoff. It is intentionally separate from P1c's
/// transient MFT-window reader: this applies only the validated stable-path
/// state and fails on every unrepresentable record.
#[cfg(windows)]
fn replay_stable_deltas(
    handle: windows_sys::Win32::Foundation::HANDLE,
    saved: &UsnCheckpoint,
    cutoff: &UsnJournalState,
    state: &mut StableReplayPathState,
    dirty: &mut ReplayDirtySet,
) -> Result<usize, String> {
    use std::{ffi::c_void, mem::size_of, ptr};

    use windows_sys::Win32::{
        Foundation::{GetLastError, ERROR_HANDLE_EOF, ERROR_JOURNAL_ENTRY_DELETED},
        System::{
            Ioctl::{FSCTL_READ_USN_JOURNAL, READ_USN_JOURNAL_DATA_V0},
            IO::DeviceIoControl,
        },
    };

    if saved.next_usn == cutoff.next_usn {
        return Ok(0);
    }

    let mut cursor = saved.next_usn;
    let mut replayed_records = 0usize;
    let mut output = vec![0u8; MAX_USN_REPLAY_BUFFER_BYTES];
    for _ in 0..MAX_USN_REPLAY_CALLS {
        if cursor == cutoff.next_usn {
            return Ok(replayed_records);
        }
        if cursor > cutoff.next_usn {
            return Err("USN 增量回放游标越过截止水位".to_owned());
        }
        let request = READ_USN_JOURNAL_DATA_V0 {
            StartUsn: cursor,
            ReasonMask: u32::MAX,
            ReturnOnlyOnClose: 0,
            Timeout: 0,
            BytesToWaitFor: 0,
            UsnJournalID: saved.journal_id,
        };
        let mut bytes_returned = 0u32;
        let result = unsafe {
            DeviceIoControl(
                handle,
                FSCTL_READ_USN_JOURNAL,
                (&request as *const READ_USN_JOURNAL_DATA_V0).cast::<c_void>(),
                size_of::<READ_USN_JOURNAL_DATA_V0>() as u32,
                output.as_mut_ptr().cast::<c_void>(),
                output.len() as u32,
                &mut bytes_returned,
                ptr::null_mut(),
            )
        };
        if result == 0 {
            let error = unsafe { GetLastError() };
            if matches!(error, ERROR_HANDLE_EOF | ERROR_JOURNAL_ENTRY_DELETED) {
                return Err("USN 增量回放在截止水位前结束或所需记录已失效".to_owned());
            }
            return Err(format!(
                "无法读取 USN 增量回放记录（{}，Win32 错误 {error}）",
                win32_error_hint(error)
            ));
        }
        let used = bytes_returned as usize;
        if used > output.len() {
            return Err("USN 增量回放输出长度超过调用缓冲区".to_owned());
        }
        let reply = parse_usn_delta_reply(&output[..used])
            .map_err(|reason| format!("USN 增量回放记录无效（{reason}）"))?;
        if reply.next_usn <= cursor || reply.next_usn > cutoff.next_usn {
            return Err("USN 增量回放游标没有连续推进到截止水位".to_owned());
        }
        if reply.record_count == 0 {
            return Err("USN 增量回放响应未包含覆盖截止水位的记录".to_owned());
        }

        let mut previous_usn = None;
        for record in reply.records {
            if record.usn < cursor || record.usn >= reply.next_usn || record.usn >= cutoff.next_usn
            {
                return Err("USN 增量回放记录不在请求的 Journal 区间内".to_owned());
            }
            if let Some(previous_usn) = previous_usn {
                if record.usn <= previous_usn {
                    return Err("USN 增量回放记录水位未严格递增".to_owned());
                }
            }
            previous_usn = Some(record.usn);
            replayed_records = replayed_records.saturating_add(1);
            if replayed_records > MAX_USN_REPLAY_RECORDS {
                return Err("USN 增量回放记录超过安全上限".to_owned());
            }
            state.apply_delta(&record, dirty)?;
        }
        cursor = reply.next_usn;
    }
    Err("USN 增量回放调用次数超过安全上限".to_owned())
}

/// Re-queries every explicitly authorised direct drive-letter root and
/// verifies the saved P1d baseline without reading any USN records. A result
/// of `Ok(())` means all journal identities and watermarks are unchanged;
/// every other condition is an explicit fallback error.
#[cfg(windows)]
pub(crate) fn verify_zero_change_baseline(
    roots: &[PathBuf],
    saved: &[UsnCheckpoint],
) -> Result<(), String> {
    validate_zero_change_checkpoint_payload(saved)?;
    let targets = collect_strict_direct_volume_targets(roots)?;
    validate_zero_change_baseline_targets(&targets, saved)?;

    let mut current = Vec::with_capacity(targets.len());
    for target in &targets {
        let state = query_usn_journal(target).map_err(|error| {
            format!(
                "无法验证零变更快启 USN 基线（{}）：{}",
                target.volume_key,
                error.describe()
            )
        })?;
        current.push(UsnCheckpoint::from(state));
    }
    validate_zero_change_baseline(saved, &current)
}

/// Non-Windows builds retain the API but fail closed rather than treating an
/// unavailable NTFS journal as a valid zero-change restart.
#[cfg(not(windows))]
pub(crate) fn verify_zero_change_baseline(
    _roots: &[PathBuf],
    _saved: &[UsnCheckpoint],
) -> Result<(), String> {
    Err("零变更 USN 快启仅支持 Windows NTFS 直接盘符根目录".to_owned())
}

#[cfg(windows)]
pub(crate) fn probe_authorized_roots(
    roots: &[PathBuf],
    previous: &[UsnCheckpoint],
    load_warning: Option<&str>,
) -> UsnProbeOutcome {
    let volume_targets = collect_volume_targets(roots);
    if volume_targets.targets.is_empty() {
        return UsnProbeOutcome {
            status: "inactive",
            message: append_load_warning(
                "当前授权目录不在可直接查询的本地盘符卷上；继续使用目录扫描和文件监听。".to_owned(),
                load_warning,
            ),
            eligible_volumes: 0,
            checkpointed_volumes: 0,
            checkpoints: Vec::new(),
        };
    }

    let mut checkpoints = Vec::with_capacity(volume_targets.targets.len());
    let mut valid_checkpoints = 0usize;
    let mut baseline_count = 0usize;
    let mut failures = Vec::new();

    for target in &volume_targets.targets {
        match query_usn_journal(target) {
            Ok(state) => {
                let previous_checkpoint = previous
                    .iter()
                    .find(|checkpoint| checkpoint.volume_key == state.volume_key);
                match validate_checkpoint(previous_checkpoint, &state) {
                    CheckpointValidation::Valid => valid_checkpoints += 1,
                    CheckpointValidation::Missing
                    | CheckpointValidation::VolumeSerialChanged
                    | CheckpointValidation::JournalRecreated
                    | CheckpointValidation::AgedOut
                    | CheckpointValidation::AheadOfJournal => baseline_count += 1,
                }
                checkpoints.push(UsnCheckpoint::from(state));
            }
            Err(error) => failures.push(format!("{}：{}", target.volume_key, error.describe())),
        }
    }

    let successful = checkpoints.len();
    let status = match (successful, failures.is_empty()) {
        (0, _) => "fallback",
        (_, true) => "available",
        _ => "degraded",
    };
    let mut message = if successful == 0 {
        "没有可用的 NTFS USN Journal；当前范围继续使用目录扫描和文件监听。".to_owned()
    } else if baseline_count > 0 {
        format!(
            "已验证 {successful} 个 NTFS USN 卷；{baseline_count} 个检查点已重新建立。P1c 仅在明确授权盘符根目录的单次 MFT 初始化中关闭短暂 USN 窗口；跨重启回放尚未实现。"
        )
    } else {
        format!(
            "已验证 {successful} 个 NTFS USN 卷；{valid_checkpoints} 个检查点连续。P1c 仅在明确授权盘符根目录的单次 MFT 初始化中关闭短暂 USN 窗口；跨重启回放尚未实现。"
        )
    };
    if volume_targets.skipped_roots > 0 {
        message.push_str(" 部分非本地盘符目录保持目录扫描回退。");
    }
    if !failures.is_empty() {
        let details = failures
            .iter()
            .take(MAX_FAILURE_DETAILS)
            .cloned()
            .collect::<Vec<_>>()
            .join("；");
        message.push_str(&format!(" 失败卷：{details}。"));
    }

    UsnProbeOutcome {
        status,
        message: append_load_warning(message, load_warning),
        eligible_volumes: successful,
        checkpointed_volumes: valid_checkpoints,
        checkpoints,
    }
}

#[cfg(not(windows))]
pub(crate) fn probe_authorized_roots(
    _roots: &[PathBuf],
    _previous: &[UsnCheckpoint],
    _load_warning: Option<&str>,
) -> UsnProbeOutcome {
    UsnProbeOutcome {
        status: "unsupported",
        message: "当前平台不使用 NTFS USN；继续使用本机目录扫描和文件监听。".to_owned(),
        eligible_volumes: 0,
        checkpointed_volumes: 0,
        checkpoints: Vec::new(),
    }
}

fn append_load_warning(mut message: String, warning: Option<&str>) -> String {
    if let Some(warning) = warning.filter(|warning| !warning.is_empty()) {
        message.push(' ');
        message.push_str(warning);
    }
    message
}

impl From<UsnJournalState> for UsnCheckpoint {
    fn from(state: UsnJournalState) -> Self {
        Self {
            volume_key: state.volume_key,
            volume_serial_number: state.volume_serial_number,
            journal_id: state.journal_id,
            next_usn: state.next_usn,
            lowest_valid_usn: state.lowest_valid_usn,
            observed_at: Utc::now().to_rfc3339(),
        }
    }
}

fn validate_checkpoint_set(checkpoints: &[UsnCheckpoint]) -> Result<(), String> {
    if checkpoints.len() > MAX_CHECKPOINT_VOLUMES {
        return Err(format!("卷数量超过 {MAX_CHECKPOINT_VOLUMES} 个"));
    }
    let mut volumes = HashSet::new();
    for checkpoint in checkpoints {
        if !is_canonical_volume_key(&checkpoint.volume_key) {
            return Err("卷标识不是本地盘符".to_owned());
        }
        if !volumes.insert(checkpoint.volume_key.clone()) {
            return Err("包含重复卷标识".to_owned());
        }
        if checkpoint.observed_at.trim().is_empty() {
            return Err("缺少观察时间".to_owned());
        }
        if checkpoint.lowest_valid_usn < 0 || checkpoint.next_usn < checkpoint.lowest_valid_usn {
            return Err("USN 水位无效".to_owned());
        }
    }
    Ok(())
}

fn is_canonical_volume_key(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 2 && bytes[0].is_ascii_uppercase() && bytes[1] == b':'
}

fn validate_checkpoint(
    checkpoint: Option<&UsnCheckpoint>,
    state: &UsnJournalState,
) -> CheckpointValidation {
    let Some(checkpoint) = checkpoint else {
        return CheckpointValidation::Missing;
    };
    if checkpoint.volume_serial_number != state.volume_serial_number {
        return CheckpointValidation::VolumeSerialChanged;
    }
    if checkpoint.journal_id != state.journal_id {
        return CheckpointValidation::JournalRecreated;
    }
    if checkpoint.next_usn < state.lowest_valid_usn {
        return CheckpointValidation::AgedOut;
    }
    if checkpoint.next_usn > state.next_usn {
        return CheckpointValidation::AheadOfJournal;
    }
    CheckpointValidation::Valid
}

fn collect_volume_targets(roots: &[PathBuf]) -> VolumeTargets {
    let mut targets = VolumeTargets::default();
    let mut seen = HashSet::new();
    for root in roots {
        let Some(target) = drive_target_from_root(&root.to_string_lossy()) else {
            targets.skipped_roots += 1;
            continue;
        };
        if seen.insert(target.volume_key.clone()) {
            targets.targets.push(target);
        }
    }
    targets
}

/// Collects the exact root set allowed by P1d. Unlike P1a/P1c's discovery
/// helpers, this never skips a root and continues: P1d is valid only when the
/// complete explicitly authorised set consists of unique, direct drive-letter
/// roots. Narrow folders, UNC paths, volume GUID paths, and a duplicated drive
/// therefore fail the entire fast-start attempt instead of being silently
/// broadened or omitted.
fn collect_strict_direct_volume_targets(roots: &[PathBuf]) -> Result<Vec<VolumeTarget>, String> {
    if roots.is_empty() {
        return Err("零变更快启没有可验证的授权盘符根目录".to_owned());
    }
    if roots.len() > MAX_CHECKPOINT_VOLUMES {
        return Err(format!(
            "零变更快启授权盘符根目录超过 {MAX_CHECKPOINT_VOLUMES} 个"
        ));
    }

    let mut targets = Vec::with_capacity(roots.len());
    let mut seen = HashSet::new();
    for root in roots {
        let root_text = root.to_string_lossy();
        let Some(target) = drive_target_from_root(&root_text) else {
            return Err("零变更快启仅允许直接本地盘符根目录".to_owned());
        };
        if !is_direct_volume_root(&root_text, &target) {
            return Err("零变更快启不允许窄目录或挂载卷根目录".to_owned());
        }
        if !seen.insert(target.volume_key.clone()) {
            return Err("零变更快启包含重复盘符根目录".to_owned());
        }
        targets.push(target);
    }
    Ok(targets)
}

/// Confirms that the saved metadata has an exact one-to-one mapping to the
/// explicitly authorised root set before native journal queries begin.
fn validate_zero_change_baseline_targets(
    targets: &[VolumeTarget],
    saved: &[UsnCheckpoint],
) -> Result<(), String> {
    validate_zero_change_checkpoint_payload(saved)?;
    if targets.len() != saved.len() {
        return Err("零变更快启的授权根目录与已保存 USN 基线不一致".to_owned());
    }

    let target_volumes = targets
        .iter()
        .map(|target| target.volume_key.as_str())
        .collect::<HashSet<_>>();
    if saved
        .iter()
        .any(|checkpoint| !target_volumes.contains(checkpoint.volume_key.as_str()))
    {
        return Err("零变更快启缺少或包含未授权的 USN 卷基线".to_owned());
    }
    Ok(())
}

/// MFT enumeration exposes every name on the underlying volume. To retain
/// the same explicit-scope privacy boundary as the ordinary walker, it is
/// available only if the user has authorized that whole direct drive root.
/// A narrower path is intentionally counted as a scanner fallback rather than
/// being broadened to its volume.
#[cfg(windows)]
fn collect_direct_volume_targets(roots: &[PathBuf]) -> DirectVolumeTargets {
    let mut targets = DirectVolumeTargets::default();
    let mut seen = HashSet::new();
    for root in roots {
        let root_text = root.to_string_lossy();
        let Some(target) = drive_target_from_root(&root_text) else {
            targets.skipped_roots += 1;
            continue;
        };
        if !is_direct_volume_root(&root_text, &target) {
            targets.skipped_roots += 1;
            continue;
        }
        if seen.insert(target.volume_key.clone()) {
            targets.targets.push(target);
        }
    }
    targets
}

/// Performs bounded, read-only `FSCTL_ENUM_USN_DATA` initialization for
/// explicitly authorized drive roots. The resulting paths retain their MFT
/// identity tuple so the indexer may later persist only the entries it really
/// indexed. A root is considered covered only after the native stream reaches
/// EOF; limits, malformed data, access errors and narrow scopes all leave that
/// root for the normal walker.
#[cfg(windows)]
pub(crate) fn enumerate_authorized_volume_roots(
    roots: &[PathBuf],
    max_paths: usize,
) -> MftEnumerationOutcome {
    if max_paths == 0 {
        return MftEnumerationOutcome {
            status: "inactive",
            message: "MFT 初始化没有可用的路径预算；继续使用授权目录扫描。".to_owned(),
            covered_roots: Vec::new(),
            paths: Vec::new(),
            replay_seeds: Vec::new(),
            enumerated_records: 0,
            replayed_usn_records: 0,
        };
    }

    let targets = collect_direct_volume_targets(roots);
    if targets.targets.is_empty() {
        return MftEnumerationOutcome {
            status: "inactive",
            message: "MFT P1c 仅在用户明确授权盘符根目录（例如 C:\\）时启用；当前窄目录继续使用授权目录扫描和文件监听。".to_owned(),
            covered_roots: Vec::new(),
            paths: Vec::new(),
            replay_seeds: Vec::new(),
            enumerated_records: 0,
            replayed_usn_records: 0,
        };
    }

    let direct_target_count = targets.targets.len();
    let mut covered_roots = Vec::with_capacity(direct_target_count);
    let mut paths = Vec::new();
    let mut replay_seeds = Vec::with_capacity(direct_target_count);
    let mut enumerated_records = 0usize;
    let mut replayed_usn_records = 0usize;
    let mut failures = Vec::new();

    for target in targets.targets {
        let remaining = max_paths.saturating_sub(paths.len());
        if remaining == 0 {
            failures.push(format!("{}：路径数量达到本地索引上限", target.volume_key));
            continue;
        }
        match enumerate_volume_root(&target, remaining) {
            Ok(volume) => {
                enumerated_records = enumerated_records.saturating_add(volume.record_count);
                replayed_usn_records =
                    replayed_usn_records.saturating_add(volume.replayed_usn_records);
                covered_roots.push(PathBuf::from(&target.volume_root));
                replay_seeds.push(UsnReplayVolumeSeed {
                    volume_key: target.volume_key.clone(),
                    volume_root: PathBuf::from(&target.volume_root),
                    root_file_reference_number: volume.root_file_reference_number,
                    cutoff: volume.cutoff,
                });
                paths.extend(volume.paths);
            }
            Err(error) => failures.push(format!("{}：{}", target.volume_key, error.describe())),
        }
    }

    let status = match (
        covered_roots.is_empty(),
        failures.is_empty(),
        targets.skipped_roots,
    ) {
        (true, _, _) => "fallback",
        (false, true, 0) => "available",
        _ => "degraded",
    };
    let mut message = if covered_roots.is_empty() {
        "MFT P1c 未能完整枚举并收敛任何已授权盘符根目录；继续使用授权目录扫描和文件监听。"
            .to_owned()
    } else {
        format!(
            "已只读枚举 {} 个显式授权 NTFS 盘符根目录的 {enumerated_records} 条 MFT 元数据记录，并在本次初始化窗口内处理 {replayed_usn_records} 条 USN 记录后投影 {} 条路径；完整且外置状态的快照可建立 P1e 跨重启回放绑定，持续刷新仍由文件监听和受限目录扫描负责。",
            covered_roots.len(),
            paths.len(),
        )
    };
    if targets.skipped_roots > 0 {
        message.push_str(" 其他非盘符根目录没有扩大为全卷 MFT 读取，保持目录扫描回退。");
    }
    if !failures.is_empty() {
        let details = failures
            .iter()
            .take(MAX_FAILURE_DETAILS)
            .cloned()
            .collect::<Vec<_>>()
            .join("；");
        message.push_str(&format!(" 未覆盖卷：{details}。"));
    }

    MftEnumerationOutcome {
        status,
        message,
        covered_roots,
        paths,
        replay_seeds,
        enumerated_records,
        replayed_usn_records,
    }
}

/// Converts only a rooted local drive path (`C:\...` / `\\?\C:\...`) into
/// a safe volume query target. UNC, volume GUID, relative and drive-relative
/// paths are intentionally left to the existing scoped scanner.
fn drive_target_from_root(root: &str) -> Option<VolumeTarget> {
    let root = root.trim();
    let root = root
        .strip_prefix(r"\\?\")
        .or_else(|| root.strip_prefix(r"\\.\"))
        .unwrap_or(root);
    let bytes = root.as_bytes();
    if bytes.len() < 3
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || !matches!(bytes[2], b'\\' | b'/')
    {
        return None;
    }
    let drive = (bytes[0] as char).to_ascii_uppercase();
    Some(VolumeTarget {
        volume_key: format!("{drive}:"),
        volume_root: format!("{drive}:\\"),
        device_path: format!(r"\\.\{drive}:"),
        sample_root: root.to_owned(),
    })
}

fn is_direct_volume_root(root: &str, target: &VolumeTarget) -> bool {
    normalize_volume_root(root) == normalize_volume_root(&target.volume_root)
}

fn normalize_volume_root(value: &str) -> String {
    value
        .trim()
        .trim_start_matches(r"\\?\")
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_uppercase()
}

#[cfg(windows)]
#[derive(Debug)]
enum UsnProbeError {
    UnsupportedDriveType(u32),
    VolumePath(u32),
    MountedVolume(String),
    VolumeInformation(u32),
    NotNtfs(String),
    OpenVolume(u32),
    QueryJournal(u32),
    ShortJournalReply(u32),
    InvalidJournalWatermark(i64, i64),
}

#[cfg(windows)]
impl UsnProbeError {
    fn describe(&self) -> String {
        match self {
            Self::UnsupportedDriveType(kind) => format!("不是本地固定/可移动卷（类型 {kind}）"),
            Self::VolumePath(error) => format!("无法确认根目录所在卷（Win32 错误 {error}）"),
            Self::MountedVolume(root) => {
                format!("位于不带盘符的挂载卷 {root}，P1a 保持目录扫描回退")
            }
            Self::VolumeInformation(error) => format!("无法读取卷信息（Win32 错误 {error}）"),
            Self::NotNtfs(filesystem) => format!("文件系统为 {filesystem}，不是 NTFS"),
            Self::OpenVolume(error) => format!(
                "无法打开卷（{}，Win32 错误 {error}）",
                win32_error_hint(*error)
            ),
            Self::QueryJournal(error) => {
                format!(
                    "无法查询 USN Journal（{}，Win32 错误 {error}）",
                    win32_error_hint(*error)
                )
            }
            Self::ShortJournalReply(bytes) => format!("USN Journal 返回长度异常（{bytes} 字节）"),
            Self::InvalidJournalWatermark(lowest, next) => {
                format!("USN Journal 水位无效（最低 {lowest}，当前 {next}）")
            }
        }
    }
}

#[cfg(windows)]
#[derive(Debug)]
struct EnumeratedMftVolume {
    paths: Vec<MftPathEntry>,
    root_file_reference_number: u64,
    cutoff: UsnCheckpoint,
    record_count: usize,
    replayed_usn_records: usize,
}

#[cfg(windows)]
#[derive(Debug)]
enum MftEnumerationError {
    Journal(UsnProbeError),
    OpenVolume(u32),
    OpenRoot(u32),
    RootInformation(u32),
    RootVolumeChanged { expected: u32, actual: u32 },
    RootReferenceChanged { expected: u64, actual: u64 },
    InvalidRootReference,
    Enumerate(u32),
    InvalidReply(String),
    ReplayRead(u32),
    ReplayWindow(String),
    ReplayLimit,
    ReplayTopology(String),
    EntryLimit,
    CallLimit,
    PathBuild(String),
}

#[cfg(windows)]
impl MftEnumerationError {
    fn describe(&self) -> String {
        match self {
            Self::Journal(error) => error.describe(),
            Self::OpenVolume(error) => format!(
                "无法以只读方式打开卷进行 MFT 枚举（{}，Win32 错误 {error}）",
                win32_error_hint(*error)
            ),
            Self::OpenRoot(error) => {
                format!("无法读取已授权盘符根目录的稳定文件引用（Win32 错误 {error}）")
            }
            Self::RootInformation(error) => {
                format!("无法读取盘符根目录信息（Win32 错误 {error}）")
            }
            Self::RootVolumeChanged { expected, actual } => {
                format!("盘符在枚举前发生卷切换（原序列号 {expected}，当前 {actual}）")
            }
            Self::RootReferenceChanged { expected, actual } => {
                format!("盘符根目录在初始化期间发生变化（原引用 {expected}，当前 {actual}）")
            }
            Self::InvalidRootReference => "盘符根目录没有有效的 NTFS 文件引用".to_owned(),
            Self::Enumerate(error) => format!(
                "无法枚举 MFT（{}，Win32 错误 {error}）",
                win32_error_hint(*error)
            ),
            Self::InvalidReply(reason) => format!("MFT 返回的 USN 记录无效（{reason}）"),
            Self::ReplayRead(error) => format!(
                "无法只读回放 MFT 初始化窗口内的 USN 记录（{}，Win32 错误 {error}）",
                win32_error_hint(*error)
            ),
            Self::ReplayWindow(reason) => {
                format!("MFT 初始化期间的 USN Journal 窗口不连续（{reason}）")
            }
            Self::ReplayLimit => {
                "MFT 初始化窗口的 USN 回放超过安全上限；为保持完整性已回退".to_owned()
            }
            Self::ReplayTopology(reason) => {
                format!("MFT 初始化窗口包含无法安全投影的路径变更（{reason}）")
            }
            Self::EntryLimit => "MFT 路径数量超过本地索引上限；为保持完整性已回退".to_owned(),
            Self::CallLimit => "MFT 枚举超过安全调用上限；为保持完整性已回退".to_owned(),
            Self::PathBuild(reason) => format!("无法安全重建 MFT 路径（{reason}）"),
        }
    }
}

/// Opens only the exact raw device for a drive root after `query_usn_journal`
/// has verified its filesystem and live journal. The handle has read access
/// only and is closed by `Drop` on all success and error paths.
#[cfg(windows)]
struct ReadOnlyVolumeHandle(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for ReadOnlyVolumeHandle {
    fn drop(&mut self) {
        use windows_sys::Win32::Foundation::CloseHandle;

        let _ = unsafe { CloseHandle(self.0) };
    }
}

#[cfg(windows)]
fn enumerate_volume_root(
    target: &VolumeTarget,
    max_paths: usize,
) -> Result<EnumeratedMftVolume, MftEnumerationError> {
    if max_paths == 0 {
        return Err(MftEnumerationError::EntryLimit);
    }

    // Reuse P1a's complete direct-drive/NTFS/Journal eligibility check before
    // reading MFT metadata. It also makes an absent or disabled journal an
    // explicit fallback instead of silently changing the scan source.
    let initial_journal = query_usn_journal(target).map_err(MftEnumerationError::Journal)?;
    let root_reference =
        volume_root_file_reference(&target.volume_root, initial_journal.volume_serial_number)?;
    let handle = open_readonly_volume(target)?;
    let (records, record_count) = enumerate_mft_records(handle.0, max_paths.saturating_sub(1))?;
    let mut state = MftPathState::from_records(
        records,
        root_reference,
        PathBuf::from(&target.volume_root),
        max_paths,
    )
    .map_err(MftEnumerationError::PathBuild)?;

    // Materialize the initial projection before closing the Journal window.
    // This is intentionally a one-run race closure, not a stored replay
    // checkpoint: file references and USN records remain inside this function.
    {
        let _ = state
            .build_paths()
            .map_err(MftEnumerationError::PathBuild)?;
    }
    let cutoff_journal = query_usn_journal(target).map_err(MftEnumerationError::Journal)?;
    validate_initialization_replay_window(&initial_journal, &cutoff_journal)
        .map_err(MftEnumerationError::ReplayWindow)?;
    let cutoff_root_reference =
        volume_root_file_reference(&target.volume_root, cutoff_journal.volume_serial_number)?;
    if cutoff_root_reference != root_reference {
        return Err(MftEnumerationError::RootReferenceChanged {
            expected: root_reference,
            actual: cutoff_root_reference,
        });
    }
    let replayed_usn_records =
        replay_initialization_deltas(handle.0, &initial_journal, &cutoff_journal, &mut state)?;
    let cutoff = UsnCheckpoint::from(cutoff_journal);
    let mut paths = state
        .build_entries(&target.volume_key)
        .map_err(MftEnumerationError::PathBuild)?;
    if paths.len() > max_paths {
        return Err(MftEnumerationError::EntryLimit);
    }
    // `build_entries` deliberately includes the authorized root so this
    // projection has the same root-entry behavior as the regular walker.
    paths.sort_unstable_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.file_reference_number.cmp(&right.file_reference_number))
            .then_with(|| {
                left.parent_file_reference_number
                    .cmp(&right.parent_file_reference_number)
            })
            .then_with(|| left.name.cmp(&right.name))
    });
    paths.dedup();
    Ok(EnumeratedMftVolume {
        paths,
        root_file_reference_number: root_reference,
        cutoff,
        record_count,
        replayed_usn_records,
    })
}

#[cfg(windows)]
fn open_readonly_volume(
    target: &VolumeTarget,
) -> Result<ReadOnlyVolumeHandle, MftEnumerationError> {
    use std::ptr;

    use windows_sys::Win32::{
        Foundation::{GetLastError, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{
            CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        },
    };

    let device_path = wide_null(&target.device_path);
    let handle = unsafe {
        CreateFileW(
            device_path.as_ptr(),
            FILE_GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(MftEnumerationError::OpenVolume(unsafe { GetLastError() }));
    }
    Ok(ReadOnlyVolumeHandle(handle))
}

#[cfg(windows)]
fn volume_root_file_reference(
    volume_root: &str,
    expected_serial: u32,
) -> Result<u64, MftEnumerationError> {
    use std::ptr;

    use windows_sys::Win32::{
        Foundation::{CloseHandle, GetLastError, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{
            CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
            FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, OPEN_EXISTING,
        },
    };

    let root = wide_null(volume_root);
    let handle = unsafe {
        CreateFileW(
            root.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(MftEnumerationError::OpenRoot(unsafe { GetLastError() }));
    }

    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    let information_result = unsafe { GetFileInformationByHandle(handle, &mut information) };
    let information_error = if information_result == 0 {
        Some(unsafe { GetLastError() })
    } else {
        None
    };
    let _ = unsafe { CloseHandle(handle) };
    if let Some(error) = information_error {
        return Err(MftEnumerationError::RootInformation(error));
    }
    if information.dwVolumeSerialNumber != expected_serial {
        return Err(MftEnumerationError::RootVolumeChanged {
            expected: expected_serial,
            actual: information.dwVolumeSerialNumber,
        });
    }
    let reference = ((information.nFileIndexHigh as u64) << 32) | information.nFileIndexLow as u64;
    if reference == 0 {
        return Err(MftEnumerationError::InvalidRootReference);
    }
    Ok(reference)
}

#[cfg(windows)]
fn enumerate_mft_records(
    handle: windows_sys::Win32::Foundation::HANDLE,
    max_records: usize,
) -> Result<(Vec<MftRecord>, usize), MftEnumerationError> {
    use std::{ffi::c_void, mem::size_of, ptr};

    use windows_sys::Win32::{
        Foundation::{GetLastError, ERROR_HANDLE_EOF},
        System::{
            Ioctl::{FSCTL_ENUM_USN_DATA, MFT_ENUM_DATA_V0},
            IO::DeviceIoControl,
        },
    };

    if max_records == 0 {
        return Err(MftEnumerationError::EntryLimit);
    }

    let mut next_start_file_reference_number = 0u64;
    let mut record_count = 0usize;
    let mut records = Vec::new();
    let mut output = vec![0u8; MAX_MFT_ENUM_BUFFER_BYTES];

    for _ in 0..MAX_MFT_ENUM_CALLS {
        let request = MFT_ENUM_DATA_V0 {
            StartFileReferenceNumber: next_start_file_reference_number,
            LowUsn: 0,
            HighUsn: i64::MAX,
        };
        let mut bytes_returned = 0u32;
        let result = unsafe {
            DeviceIoControl(
                handle,
                FSCTL_ENUM_USN_DATA,
                (&request as *const MFT_ENUM_DATA_V0).cast::<c_void>(),
                size_of::<MFT_ENUM_DATA_V0>() as u32,
                output.as_mut_ptr().cast::<c_void>(),
                output.len() as u32,
                &mut bytes_returned,
                ptr::null_mut(),
            )
        };
        if result == 0 {
            let error = unsafe { GetLastError() };
            if error == ERROR_HANDLE_EOF {
                return Ok((records, record_count));
            }
            return Err(MftEnumerationError::Enumerate(error));
        }

        let used = bytes_returned as usize;
        if used > output.len() {
            return Err(MftEnumerationError::InvalidReply(
                "输出长度超过调用缓冲区".to_owned(),
            ));
        }
        let reply = parse_mft_reply(&output[..used]).map_err(MftEnumerationError::InvalidReply)?;
        if reply.record_count == 0 {
            return Ok((records, record_count));
        }
        if record_count.saturating_add(reply.record_count) > max_records {
            return Err(MftEnumerationError::EntryLimit);
        }
        if reply.next_start_file_reference_number <= next_start_file_reference_number {
            return Err(MftEnumerationError::InvalidReply(
                "下一个 MFT 起始引用没有前进".to_owned(),
            ));
        }
        record_count += reply.record_count;
        records.extend(reply.records);
        next_start_file_reference_number = reply.next_start_file_reference_number;
    }

    Err(MftEnumerationError::CallLimit)
}

/// Validates that a Journal still describes the exact contiguous interval that
/// began before MFT enumeration. This never compares or stores a previous app
/// run's file references or records: it only guards the current in-memory
/// initialization window.
fn validate_initialization_replay_window(
    initial: &UsnJournalState,
    cutoff: &UsnJournalState,
) -> Result<(), String> {
    if initial.volume_key != cutoff.volume_key {
        return Err("卷标识发生变化".to_owned());
    }
    if initial.volume_serial_number != cutoff.volume_serial_number {
        return Err("卷序列号发生变化".to_owned());
    }
    if initial.journal_id != cutoff.journal_id {
        return Err("USN Journal 已重建".to_owned());
    }
    if initial.next_usn < initial.lowest_valid_usn
        || cutoff.next_usn < cutoff.lowest_valid_usn
        || initial.next_usn < cutoff.lowest_valid_usn
    {
        return Err("起始水位已被截断或无效".to_owned());
    }
    if cutoff.next_usn < initial.next_usn {
        return Err("USN 水位倒退".to_owned());
    }
    Ok(())
}

/// Reads only enough of the already-live Journal to cover the interval between
/// the two queries around an MFT initialization. It never creates, deletes or
/// otherwise mutates a Journal. The caller discards the entire MFT projection
/// on any gap, record variant, topology ambiguity or resource limit.
#[cfg(windows)]
fn replay_initialization_deltas(
    handle: windows_sys::Win32::Foundation::HANDLE,
    initial: &UsnJournalState,
    cutoff: &UsnJournalState,
    state: &mut MftPathState,
) -> Result<usize, MftEnumerationError> {
    use std::{ffi::c_void, mem::size_of, ptr};

    use windows_sys::Win32::{
        Foundation::{GetLastError, ERROR_HANDLE_EOF, ERROR_JOURNAL_ENTRY_DELETED},
        System::{
            Ioctl::{FSCTL_READ_USN_JOURNAL, READ_USN_JOURNAL_DATA_V0},
            IO::DeviceIoControl,
        },
    };

    if initial.next_usn == cutoff.next_usn {
        return Ok(0);
    }

    let mut cursor = initial.next_usn;
    let mut replayed_records = 0usize;
    let mut observed_records = 0usize;
    let mut output = vec![0u8; MAX_USN_REPLAY_BUFFER_BYTES];

    for _ in 0..MAX_USN_REPLAY_CALLS {
        if cursor >= cutoff.next_usn {
            return Ok(replayed_records);
        }
        let request = READ_USN_JOURNAL_DATA_V0 {
            StartUsn: cursor,
            // Request every known record so a topology event cannot be
            // hidden by filtering. Unsupported records still fail closed in
            // `MftPathState::apply_delta`.
            ReasonMask: u32::MAX,
            ReturnOnlyOnClose: 0,
            Timeout: 0,
            BytesToWaitFor: 0,
            UsnJournalID: initial.journal_id,
        };
        let mut bytes_returned = 0u32;
        let result = unsafe {
            DeviceIoControl(
                handle,
                FSCTL_READ_USN_JOURNAL,
                (&request as *const READ_USN_JOURNAL_DATA_V0).cast::<c_void>(),
                size_of::<READ_USN_JOURNAL_DATA_V0>() as u32,
                output.as_mut_ptr().cast::<c_void>(),
                output.len() as u32,
                &mut bytes_returned,
                ptr::null_mut(),
            )
        };
        if result == 0 {
            let error = unsafe { GetLastError() };
            if matches!(error, ERROR_HANDLE_EOF | ERROR_JOURNAL_ENTRY_DELETED) {
                return Err(MftEnumerationError::ReplayWindow(
                    "回放在截止水位前结束或所需记录已失效".to_owned(),
                ));
            }
            return Err(MftEnumerationError::ReplayRead(error));
        }

        let used = bytes_returned as usize;
        if used > output.len() {
            return Err(MftEnumerationError::ReplayWindow(
                "USN 回放输出长度超过调用缓冲区".to_owned(),
            ));
        }
        let reply =
            parse_usn_delta_reply(&output[..used]).map_err(MftEnumerationError::ReplayWindow)?;
        if reply.next_usn <= cursor {
            return Err(MftEnumerationError::ReplayWindow(
                "USN 回放游标没有前进".to_owned(),
            ));
        }
        if reply.record_count == 0 {
            return Err(MftEnumerationError::ReplayWindow(
                "USN 回放响应未包含覆盖初始化窗口的记录".to_owned(),
            ));
        }
        observed_records = observed_records.saturating_add(reply.record_count);
        if observed_records > MAX_USN_REPLAY_RECORDS {
            return Err(MftEnumerationError::ReplayLimit);
        }

        let mut previous_usn = None;
        for record in reply.records {
            if record.usn < cursor || record.usn >= reply.next_usn {
                return Err(MftEnumerationError::ReplayWindow(
                    "USN 回放记录不在响应游标边界内".to_owned(),
                ));
            }
            if let Some(previous_usn) = previous_usn {
                if record.usn <= previous_usn {
                    return Err(MftEnumerationError::ReplayWindow(
                        "USN 回放记录水位未严格递增".to_owned(),
                    ));
                }
            }
            previous_usn = Some(record.usn);

            // Records at or after the second query belong to the existing
            // watcher/scanner handoff, not this initialization-only window.
            if record.usn >= cutoff.next_usn {
                continue;
            }
            state
                .apply_delta(&record)
                .map_err(MftEnumerationError::ReplayTopology)?;
            replayed_records = replayed_records.saturating_add(1);
            if replayed_records > MAX_USN_REPLAY_RECORDS {
                return Err(MftEnumerationError::ReplayLimit);
            }
        }

        cursor = reply.next_usn;
    }

    Err(MftEnumerationError::ReplayLimit)
}

/// Parses `FSCTL_READ_USN_JOURNAL` output without casting arbitrary kernel
/// bytes. Only V2 records have the 64-bit file-reference layout used by the
/// transient MFT map; V3/V4 and malformed records force the safe walker
/// fallback rather than being guessed at.
fn parse_usn_delta_reply(bytes: &[u8]) -> Result<ParsedUsnDeltaReply, String> {
    const REPLY_CURSOR_BYTES: usize = 8;
    const V2_FIXED_BYTES: usize = 60;

    if bytes.len() < REPLY_CURSOR_BYTES {
        return Err(format!("回复不足 {REPLY_CURSOR_BYTES} 字节"));
    }
    let next_usn = read_i64_le(bytes, 0).ok_or_else(|| "无法读取下一个 USN 水位".to_owned())?;
    if next_usn < 0 {
        return Err("下一个 USN 水位无效".to_owned());
    }

    let mut offset = REPLY_CURSOR_BYTES;
    let mut records = Vec::new();
    let mut record_count = 0usize;
    while offset < bytes.len() {
        let remaining = bytes.len() - offset;
        if remaining < 8 {
            return Err("USN 记录公共头不完整".to_owned());
        }
        let record_length =
            read_u32_le(bytes, offset).ok_or_else(|| "无法读取 USN 记录长度".to_owned())? as usize;
        let major_version =
            read_u16_le(bytes, offset + 4).ok_or_else(|| "无法读取 USN 记录版本".to_owned())?;
        let minor_version =
            read_u16_le(bytes, offset + 6).ok_or_else(|| "无法读取 USN 记录版本".to_owned())?;
        if major_version != 2 || minor_version != 0 {
            return Err(format!(
                "不支持的 USN 记录版本 {major_version}.{minor_version}"
            ));
        }
        // Windows USN records are 8-byte aligned. Do not try to recover from
        // a malformed length by advancing into a potentially desynchronised
        // reply: the caller will fall back to the scoped safe scanner instead.
        if record_length < V2_FIXED_BYTES || record_length > remaining || record_length % 8 != 0 {
            return Err(format!("USN 记录长度 {record_length} 超出回复边界"));
        }
        let record = &bytes[offset..offset + record_length];
        let file_reference_number =
            read_u64_le(record, 8).ok_or_else(|| "USN 记录缺少文件引用".to_owned())?;
        let parent_file_reference_number =
            read_u64_le(record, 16).ok_or_else(|| "USN 记录缺少父目录引用".to_owned())?;
        let usn = read_i64_le(record, 24).ok_or_else(|| "USN 记录缺少水位".to_owned())?;
        let reason = read_u32_le(record, 40).ok_or_else(|| "USN 记录缺少原因".to_owned())?;
        let attributes =
            read_u32_le(record, 52).ok_or_else(|| "USN 记录缺少文件属性".to_owned())?;
        let file_name_length =
            read_u16_le(record, 56).ok_or_else(|| "USN 记录缺少文件名长度".to_owned())? as usize;
        let file_name_offset =
            read_u16_le(record, 58).ok_or_else(|| "USN 记录缺少文件名偏移".to_owned())? as usize;
        if file_name_length % 2 != 0
            || file_name_offset < V2_FIXED_BYTES
            || file_name_offset.saturating_add(file_name_length) > record.len()
        {
            return Err("USN 记录文件名边界无效".to_owned());
        }
        let name = decode_utf16le(&record[file_name_offset..file_name_offset + file_name_length])?;
        records.push(UsnDeltaRecord {
            file_reference_number,
            parent_file_reference_number,
            usn,
            reason,
            attributes,
            name,
        });
        record_count = record_count.saturating_add(1);
        offset += record_length;
    }

    Ok(ParsedUsnDeltaReply {
        next_usn,
        records,
        record_count,
    })
}

/// Parses the byte layout returned by `FSCTL_ENUM_USN_DATA` without casting
/// arbitrary kernel bytes to Rust structs. NTFS emits V2 records here; V3/V4
/// have a different 128-bit file-reference layout and are rejected so a
/// future implementation cannot accidentally misinterpret their offsets.
fn parse_mft_reply(bytes: &[u8]) -> Result<ParsedMftReply, String> {
    const REPLY_CURSOR_BYTES: usize = 8;
    const V2_FIXED_BYTES: usize = 60;

    if bytes.len() < REPLY_CURSOR_BYTES {
        return Err(format!("回复不足 {REPLY_CURSOR_BYTES} 字节"));
    }
    let next_start_file_reference_number =
        read_u64_le(bytes, 0).ok_or_else(|| "无法读取下一个 MFT 起始引用".to_owned())?;
    let mut offset = REPLY_CURSOR_BYTES;
    let mut records = Vec::new();
    let mut record_count = 0usize;

    while offset < bytes.len() {
        let remaining = bytes.len() - offset;
        if remaining < 8 {
            return Err("USN 记录公共头不完整".to_owned());
        }
        let record_length =
            read_u32_le(bytes, offset).ok_or_else(|| "无法读取 USN 记录长度".to_owned())? as usize;
        let major_version =
            read_u16_le(bytes, offset + 4).ok_or_else(|| "无法读取 USN 记录版本".to_owned())?;
        let minor_version =
            read_u16_le(bytes, offset + 6).ok_or_else(|| "无法读取 USN 记录版本".to_owned())?;
        if major_version != 2 || minor_version != 0 {
            return Err(format!(
                "不支持的 USN 记录版本 {major_version}.{minor_version}"
            ));
        }
        // `FSCTL_ENUM_USN_DATA` carries the same aligned V2 record layout as
        // journal reads. A non-aligned entry is not trustworthy metadata.
        if record_length < V2_FIXED_BYTES || record_length > remaining || record_length % 8 != 0 {
            return Err(format!("USN 记录长度 {record_length} 超出回复边界"));
        }
        let record = &bytes[offset..offset + record_length];
        let file_reference_number =
            read_u64_le(record, 8).ok_or_else(|| "USN 记录缺少文件引用".to_owned())?;
        let parent_file_reference_number =
            read_u64_le(record, 16).ok_or_else(|| "USN 记录缺少父目录引用".to_owned())?;
        let attributes =
            read_u32_le(record, 52).ok_or_else(|| "USN 记录缺少文件属性".to_owned())?;
        let file_name_length =
            read_u16_le(record, 56).ok_or_else(|| "USN 记录缺少文件名长度".to_owned())? as usize;
        let file_name_offset =
            read_u16_le(record, 58).ok_or_else(|| "USN 记录缺少文件名偏移".to_owned())? as usize;
        if file_name_length % 2 != 0
            || file_name_offset < V2_FIXED_BYTES
            || file_name_offset.saturating_add(file_name_length) > record.len()
        {
            return Err("USN 记录文件名边界无效".to_owned());
        }
        let name = decode_utf16le(&record[file_name_offset..file_name_offset + file_name_length])?;

        record_count = record_count.saturating_add(1);
        // Reparse points may be directory junctions or symbolic links. Never
        // emit them from the MFT path, because a later metadata call could
        // turn them into a scope escape unlike the normal walker's behavior.
        if attributes & NTFS_FILE_ATTRIBUTE_REPARSE_POINT == 0
            && file_reference_number != 0
            && parent_file_reference_number != 0
            && is_safe_mft_path_component(&name)
        {
            records.push(MftRecord {
                file_reference_number,
                parent_file_reference_number,
                name,
                is_directory: attributes & NTFS_FILE_ATTRIBUTE_DIRECTORY != 0,
            });
        }
        offset += record_length;
    }

    Ok(ParsedMftReply {
        next_start_file_reference_number,
        records,
        record_count,
    })
}

fn read_u16_le(bytes: &[u8], offset: usize) -> Option<u16> {
    let slice = bytes.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_le_bytes([slice[0], slice[1]]))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Option<u32> {
    let slice = bytes.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_u64_le(bytes: &[u8], offset: usize) -> Option<u64> {
    let slice = bytes.get(offset..offset.checked_add(8)?)?;
    Some(u64::from_le_bytes([
        slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
    ]))
}

fn read_i64_le(bytes: &[u8], offset: usize) -> Option<i64> {
    let slice = bytes.get(offset..offset.checked_add(8)?)?;
    Some(i64::from_le_bytes([
        slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
    ]))
}

fn decode_utf16le(bytes: &[u8]) -> Result<String, String> {
    if bytes.len() % 2 != 0 {
        return Err("UTF-16 文件名长度不是偶数".to_owned());
    }
    let units = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    String::from_utf16(&units).map_err(|_| "USN 记录文件名不是有效 UTF-16".to_owned())
}

fn is_safe_mft_path_component(value: &str) -> bool {
    if value.is_empty()
        || matches!(value, "." | "..")
        // The renderer later opens this projection through ordinary Win32
        // path APIs, not an NT-native raw-name handle.  Trailing dots/spaces
        // and DOS device basenames are normalized by those APIs and could
        // therefore point at a different object than the MFT record.
        || value.ends_with('.')
        || value.ends_with(' ')
        || value.chars().any(|character| {
            character.is_control()
                || matches!(character, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        })
    {
        return false;
    }

    let basename = value
        .split_once('.')
        .map(|(basename, _)| basename)
        .unwrap_or(value)
        .to_ascii_uppercase();
    !matches!(
        basename.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

/// Reconstructs paths only through the explicitly authorized drive-root file
/// reference. Hard-linked names are retained when their parent chain reaches
/// that root; cycles, excessive depth and combinatorial path growth fail the
/// whole MFT root so the caller can safely fall back to its scoped walker.
#[cfg(test)]
fn build_mft_paths(
    records: Vec<MftRecord>,
    root_reference: u64,
    root_path: PathBuf,
    max_paths: usize,
) -> Result<Vec<PathBuf>, String> {
    MftPathState::from_records(records, root_reference, root_path, max_paths)?.build_paths()
}

/// The only in-memory representation that retains MFT references while a
/// direct-drive initialization is in flight. It is dropped before paths leave
/// this module and is never used by the checkpoint serializer.
#[derive(Debug)]
struct MftPathState {
    records: HashMap<u64, Vec<MftRecord>>,
    root_reference: u64,
    root_path: PathBuf,
    max_paths: usize,
    max_records: usize,
    record_count: usize,
}

impl MftPathState {
    fn from_records(
        records: Vec<MftRecord>,
        root_reference: u64,
        root_path: PathBuf,
        max_paths: usize,
    ) -> Result<Self, String> {
        if root_reference == 0 || max_paths == 0 {
            return Err("根目录引用或路径上限无效".to_owned());
        }

        // The MFT enumerator itself is limited to one fewer than the path
        // limit. Keeping this transient map at the same order of magnitude
        // bounds a busy initialization window even if it receives creates.
        let mut state = Self {
            records: HashMap::new(),
            root_reference,
            root_path,
            max_paths,
            max_records: max_paths,
            record_count: 0,
        };
        for record in records {
            state.insert_initial_record(record)?;
        }
        Ok(state)
    }

    fn insert_initial_record(&mut self, record: MftRecord) -> Result<(), String> {
        if record.file_reference_number == 0
            || record.parent_file_reference_number == 0
            || !is_safe_mft_path_component(&record.name)
        {
            return Err("MFT 初始记录包含无效路径元数据".to_owned());
        }
        let aliases = self
            .records
            .entry(record.file_reference_number)
            .or_default();
        if aliases.iter().any(|existing| existing == &record) {
            return Ok(());
        }
        if self.record_count >= self.max_records {
            return Err("MFT 临时路径元数据超过安全上限".to_owned());
        }
        aliases.push(record);
        self.record_count += 1;
        Ok(())
    }

    fn build_entries(&self, volume_key: &str) -> Result<Vec<MftPathEntry>, String> {
        let mut references = self.records.keys().copied().collect::<Vec<_>>();
        references.sort_unstable();

        let mut resolver = MftPathResolver {
            records: &self.records,
            root_reference: self.root_reference,
            root_path: &self.root_path,
            max_paths: self.max_paths,
            cache: HashMap::new(),
            visiting: HashSet::new(),
        };
        let root = MftPathEntry {
            volume_key: volume_key.to_owned(),
            path: self.root_path.clone(),
            file_reference_number: self.root_reference,
            parent_file_reference_number: self.root_reference,
            name: String::new(),
            is_directory: true,
            is_root: true,
        };
        let mut entries = Vec::with_capacity(references.len().min(self.max_paths));
        let mut seen = HashSet::new();
        entries.push(root.clone());
        seen.insert(root);
        for reference in references {
            if reference == self.root_reference {
                continue;
            }
            let records = self.records.get(&reference).cloned().unwrap_or_default();
            for record in records {
                for parent_path in resolver.resolve(record.parent_file_reference_number, 0)? {
                    let entry = MftPathEntry {
                        volume_key: volume_key.to_owned(),
                        path: parent_path.join(&record.name),
                        file_reference_number: record.file_reference_number,
                        parent_file_reference_number: record.parent_file_reference_number,
                        name: record.name.clone(),
                        is_directory: record.is_directory,
                        is_root: false,
                    };
                    if seen.insert(entry.clone()) {
                        if entries.len() >= self.max_paths {
                            return Err("重建后的路径数量超过上限".to_owned());
                        }
                        entries.push(entry);
                    }
                }
            }
        }
        if !self.records.is_empty() && entries.len() == 1 {
            return Err("没有任何 MFT 记录能够连接到已授权盘符根目录".to_owned());
        }
        Ok(entries)
    }

    fn build_paths(&self) -> Result<Vec<PathBuf>, String> {
        let entries = self.build_entries("MFT")?;
        let mut paths = Vec::with_capacity(entries.len());
        let mut seen = HashSet::new();
        for entry in entries {
            if seen.insert(entry.path.clone()) {
                paths.push(entry.path);
            }
        }
        Ok(paths)
    }

    /// Applies only those V2 USN reason combinations that can be projected as
    /// a name/parent topology update without guessing. Every unsupported or
    /// potentially ambiguous topology change fails the whole MFT root, so the
    /// existing scoped walker remains authoritative for that root.
    fn apply_delta(&mut self, record: &UsnDeltaRecord) -> Result<(), String> {
        if record.usn < 0 {
            return Err("USN 记录包含负水位".to_owned());
        }
        if record.reason == 0 || record.reason & !USN_KNOWN_REASON_MASK != 0 {
            return Err("USN 记录包含未知原因位".to_owned());
        }
        if record.reason & (USN_REASON_HARD_LINK_CHANGE | USN_REASON_REPARSE_POINT_CHANGE) != 0 {
            return Err("硬链接或重解析点变更不支持初始化窗口投影".to_owned());
        }

        let action = record.reason & USN_PATH_TOPOLOGY_REASONS;
        if action == 0 {
            return Ok(());
        }
        if action.count_ones() != 1 {
            return Err("USN 记录同时包含多个路径拓扑动作".to_owned());
        }
        if record.attributes & NTFS_FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err("USN 路径变更涉及重解析点".to_owned());
        }
        if record.file_reference_number == 0
            || record.parent_file_reference_number == 0
            || !is_safe_mft_path_component(&record.name)
        {
            return Err("USN 路径变更记录包含无效文件引用或名称".to_owned());
        }
        if record.file_reference_number == self.root_reference {
            return Err("USN 路径变更涉及已授权盘符根目录".to_owned());
        }

        if action == USN_REASON_FILE_CREATE || action == USN_REASON_RENAME_NEW_NAME {
            if record.parent_file_reference_number == record.file_reference_number {
                return Err("USN 路径变更形成自引用父目录".to_owned());
            }
            if record.parent_file_reference_number != self.root_reference
                && !self
                    .records
                    .contains_key(&record.parent_file_reference_number)
            {
                return Err("USN 路径变更的父目录不在 MFT 临时投影中".to_owned());
            }
            self.insert_delta_record(MftRecord {
                file_reference_number: record.file_reference_number,
                parent_file_reference_number: record.parent_file_reference_number,
                name: record.name.clone(),
                is_directory: record.attributes & NTFS_FILE_ATTRIBUTE_DIRECTORY != 0,
            })
        } else {
            self.remove_delta_record(
                record.file_reference_number,
                record.parent_file_reference_number,
                &record.name,
            );
            Ok(())
        }
    }

    fn insert_delta_record(&mut self, record: MftRecord) -> Result<(), String> {
        let aliases = self
            .records
            .entry(record.file_reference_number)
            .or_default();
        if aliases.iter().any(|existing| existing == &record) {
            return Ok(());
        }
        if self.record_count >= self.max_records {
            return Err("USN 初始化窗口的临时路径元数据超过安全上限".to_owned());
        }
        aliases.push(record);
        self.record_count += 1;
        Ok(())
    }

    fn remove_delta_record(&mut self, file_reference: u64, parent_reference: u64, name: &str) {
        let mut remove_reference = false;
        if let Some(aliases) = self.records.get_mut(&file_reference) {
            let original_len = aliases.len();
            aliases.retain(|existing| {
                existing.parent_file_reference_number != parent_reference || existing.name != name
            });
            self.record_count = self
                .record_count
                .saturating_sub(original_len.saturating_sub(aliases.len()));
            remove_reference = aliases.is_empty();
        }
        if remove_reference {
            self.records.remove(&file_reference);
        }
    }
}

struct MftPathResolver<'a> {
    records: &'a HashMap<u64, Vec<MftRecord>>,
    root_reference: u64,
    root_path: &'a PathBuf,
    max_paths: usize,
    cache: HashMap<u64, Vec<PathBuf>>,
    visiting: HashSet<u64>,
}

impl MftPathResolver<'_> {
    fn resolve(&mut self, reference: u64, depth: usize) -> Result<Vec<PathBuf>, String> {
        if reference == self.root_reference {
            return Ok(vec![self.root_path.clone()]);
        }
        if let Some(cached) = self.cache.get(&reference) {
            return Ok(cached.clone());
        }
        if depth >= MAX_MFT_PATH_DEPTH {
            return Err("目录层级超过安全上限".to_owned());
        }
        if !self.visiting.insert(reference) {
            return Err("检测到 MFT 父目录循环".to_owned());
        }

        let records = self.records.get(&reference).cloned().unwrap_or_default();
        let result = (|| {
            let mut resolved = Vec::new();
            let mut seen = HashSet::new();
            for record in records {
                for parent in self.resolve(record.parent_file_reference_number, depth + 1)? {
                    let path = parent.join(&record.name);
                    if seen.insert(path.clone()) {
                        if resolved.len() >= self.max_paths {
                            return Err("同一文件的硬链接路径超过安全上限".to_owned());
                        }
                        resolved.push(path);
                    }
                }
            }
            Ok(resolved)
        })();
        self.visiting.remove(&reference);
        let result = result?;
        self.cache.insert(reference, result.clone());
        Ok(result)
    }
}

#[cfg(windows)]
fn query_usn_journal(target: &VolumeTarget) -> Result<UsnJournalState, UsnProbeError> {
    use std::{ffi::c_void, mem::size_of, ptr};

    use windows_sys::Win32::{
        Foundation::{CloseHandle, GetLastError, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{
            CreateFileW, GetDriveTypeW, GetVolumeInformationW, GetVolumePathNameW,
            FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_SHARE_READ, FILE_SHARE_WRITE,
            OPEN_EXISTING,
        },
        System::{
            Ioctl::{FSCTL_QUERY_USN_JOURNAL, USN_JOURNAL_DATA_V0},
            WindowsProgramming::{DRIVE_FIXED, DRIVE_REMOVABLE},
            IO::DeviceIoControl,
        },
    };

    let sample_root = wide_null(&target.sample_root);
    let mut discovered_root = [0u16; 32_768];
    let volume_path_result = unsafe {
        GetVolumePathNameW(
            sample_root.as_ptr(),
            discovered_root.as_mut_ptr(),
            discovered_root.len() as u32,
        )
    };
    if volume_path_result == 0 {
        return Err(UsnProbeError::VolumePath(unsafe { GetLastError() }));
    }
    let discovered_root = String::from_utf16_lossy(
        &discovered_root[..discovered_root
            .iter()
            .position(|character| *character == 0)
            .unwrap_or(discovered_root.len())],
    );
    if normalize_volume_root(&discovered_root) != normalize_volume_root(&target.volume_root) {
        return Err(UsnProbeError::MountedVolume(discovered_root));
    }

    let volume_root = wide_null(&target.volume_root);
    let drive_type = unsafe { GetDriveTypeW(volume_root.as_ptr()) };
    if !matches!(drive_type, DRIVE_FIXED | DRIVE_REMOVABLE) {
        return Err(UsnProbeError::UnsupportedDriveType(drive_type));
    }

    let mut serial = 0u32;
    let mut filesystem_name = [0u16; 64];
    let volume_information = unsafe {
        GetVolumeInformationW(
            volume_root.as_ptr(),
            ptr::null_mut(),
            0,
            &mut serial,
            ptr::null_mut(),
            ptr::null_mut(),
            filesystem_name.as_mut_ptr(),
            filesystem_name.len() as u32,
        )
    };
    if volume_information == 0 {
        return Err(UsnProbeError::VolumeInformation(unsafe { GetLastError() }));
    }
    let filesystem = String::from_utf16_lossy(
        &filesystem_name[..filesystem_name
            .iter()
            .position(|character| *character == 0)
            .unwrap_or(filesystem_name.len())],
    );
    if !filesystem.eq_ignore_ascii_case("NTFS") {
        return Err(UsnProbeError::NotNtfs(filesystem));
    }

    let device_path = wide_null(&target.device_path);
    let handle = unsafe {
        CreateFileW(
            device_path.as_ptr(),
            FILE_GENERIC_READ,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(UsnProbeError::OpenVolume(unsafe { GetLastError() }));
    }

    let mut journal = USN_JOURNAL_DATA_V0::default();
    let mut bytes_returned = 0u32;
    let query_result = unsafe {
        DeviceIoControl(
            handle,
            FSCTL_QUERY_USN_JOURNAL,
            ptr::null(),
            0,
            (&mut journal as *mut USN_JOURNAL_DATA_V0).cast::<c_void>(),
            size_of::<USN_JOURNAL_DATA_V0>() as u32,
            &mut bytes_returned,
            ptr::null_mut(),
        )
    };
    let query_error = if query_result == 0 {
        Some(unsafe { GetLastError() })
    } else {
        None
    };
    let _ = unsafe { CloseHandle(handle) };

    if let Some(error) = query_error {
        return Err(UsnProbeError::QueryJournal(error));
    }
    if bytes_returned < size_of::<USN_JOURNAL_DATA_V0>() as u32 {
        return Err(UsnProbeError::ShortJournalReply(bytes_returned));
    }
    if journal.LowestValidUsn < 0 || journal.NextUsn < journal.LowestValidUsn {
        return Err(UsnProbeError::InvalidJournalWatermark(
            journal.LowestValidUsn,
            journal.NextUsn,
        ));
    }

    Ok(UsnJournalState {
        volume_key: target.volume_key.clone(),
        volume_serial_number: serial,
        journal_id: journal.UsnJournalID,
        next_usn: journal.NextUsn,
        lowest_valid_usn: journal.LowestValidUsn,
    })
}

#[cfg(windows)]
fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn win32_error_hint(error: u32) -> &'static str {
    use windows_sys::Win32::Foundation::{
        ERROR_ACCESS_DENIED, ERROR_INVALID_FUNCTION, ERROR_JOURNAL_DELETE_IN_PROGRESS,
        ERROR_JOURNAL_ENTRY_DELETED, ERROR_JOURNAL_NOT_ACTIVE,
    };

    match error {
        ERROR_ACCESS_DENIED => "权限不足",
        ERROR_INVALID_FUNCTION => "该卷不支持此控制码",
        ERROR_JOURNAL_NOT_ACTIVE => "该卷未启用 USN Journal",
        ERROR_JOURNAL_DELETE_IN_PROGRESS => "USN Journal 正在删除",
        ERROR_JOURNAL_ENTRY_DELETED => "所需 USN 水位已失效",
        _ => "系统调用失败",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(next_usn: i64, lowest_valid_usn: i64) -> UsnJournalState {
        UsnJournalState {
            volume_key: "C:".to_owned(),
            volume_serial_number: 42,
            journal_id: 99,
            next_usn,
            lowest_valid_usn,
        }
    }

    fn checkpoint(next_usn: i64) -> UsnCheckpoint {
        UsnCheckpoint {
            volume_key: "C:".to_owned(),
            volume_serial_number: 42,
            journal_id: 99,
            next_usn,
            lowest_valid_usn: 1,
            observed_at: "2026-01-01T00:00:00Z".to_owned(),
        }
    }

    fn replay_root() -> UsnReplayStablePath {
        UsnReplayStablePath {
            path: PathBuf::from(r"C:\"),
            file_reference_number: 5,
            parent_file_reference_number: 5,
            name: String::new(),
            is_directory: true,
            is_root: true,
        }
    }

    fn replay_path(
        path: &str,
        file_reference_number: u64,
        parent_file_reference_number: u64,
        name: &str,
        is_directory: bool,
    ) -> UsnReplayStablePath {
        UsnReplayStablePath {
            path: PathBuf::from(path),
            file_reference_number,
            parent_file_reference_number,
            name: name.to_owned(),
            is_directory,
            is_root: false,
        }
    }

    fn replay_binding(paths: Vec<UsnReplayStablePath>) -> UsnReplayBinding {
        UsnReplayBinding {
            schema_version: USN_REPLAY_BINDING_SCHEMA_VERSION,
            checkpoints: vec![checkpoint(50)],
            volumes: vec![UsnReplayVolume {
                volume_key: "C:".to_owned(),
                volume_root: PathBuf::from(r"C:\"),
                root_file_reference_number: 5,
                paths,
            }],
        }
    }

    #[test]
    fn only_rooted_local_drive_paths_become_volume_targets() {
        let canonical = drive_target_from_root(r"C:\Users\iHub").unwrap();
        let verbatim = drive_target_from_root(r"\\?\d:\Work").unwrap();
        assert_eq!(canonical.volume_key, "C:");
        assert_eq!(canonical.volume_root, r"C:\");
        assert_eq!(canonical.device_path, r"\\.\C:");
        assert_eq!(verbatim.volume_key, "D:");
        assert!(drive_target_from_root("C:").is_none());
        assert!(drive_target_from_root(r"\\server\share").is_none());
        assert!(drive_target_from_root(r"\\?\Volume{abc}\").is_none());
        assert!(drive_target_from_root("relative/path").is_none());
    }

    #[test]
    fn checkpoint_validation_rejects_recreated_or_truncated_journals() {
        assert_eq!(
            validate_checkpoint(None, &state(100, 10)),
            CheckpointValidation::Missing
        );
        assert_eq!(
            validate_checkpoint(Some(&checkpoint(50)), &state(100, 10)),
            CheckpointValidation::Valid
        );
        assert_eq!(
            validate_checkpoint(Some(&checkpoint(5)), &state(100, 10)),
            CheckpointValidation::AgedOut
        );
        assert_eq!(
            validate_checkpoint(Some(&checkpoint(120)), &state(100, 10)),
            CheckpointValidation::AheadOfJournal
        );

        let mut changed_serial = checkpoint(50);
        changed_serial.volume_serial_number = 7;
        assert_eq!(
            validate_checkpoint(Some(&changed_serial), &state(100, 10)),
            CheckpointValidation::VolumeSerialChanged
        );

        let mut changed_journal = checkpoint(50);
        changed_journal.journal_id = 7;
        assert_eq!(
            validate_checkpoint(Some(&changed_journal), &state(100, 10)),
            CheckpointValidation::JournalRecreated
        );
    }

    #[test]
    fn checkpoint_payload_is_bounded_and_contains_only_volume_metadata() {
        let bytes = encode_checkpoints(&[checkpoint(50)]).unwrap();
        let payload = String::from_utf8(bytes).unwrap();
        assert!(payload.contains("volumeSerialNumber"));
        assert!(payload.contains("journalId"));
        assert!(payload.contains("nextUsn"));
        assert!(!payload.contains("path"));
        assert!(!payload.contains("fileId"));
        assert!(!payload.contains("parentFile"));
        assert!(!payload.contains("replayedUsn"));
    }

    #[test]
    fn zero_change_payload_is_nonempty_metadata_only_and_bounded() {
        let baseline = vec![checkpoint(50)];
        validate_zero_change_checkpoint_payload(&baseline).unwrap();
        assert!(validate_zero_change_checkpoint_payload(&[]).is_err());

        let payload = String::from_utf8(encode_checkpoints(&baseline).unwrap()).unwrap();
        assert!(payload.contains("volumeKey"));
        assert!(payload.contains("volumeSerialNumber"));
        assert!(payload.contains("journalId"));
        assert!(payload.contains("nextUsn"));
        assert!(payload.contains("lowestValidUsn"));
        assert!(!payload.contains("path"));
        assert!(!payload.contains("fileReference"));
        assert!(!payload.contains("parentReference"));
        assert!(!payload.contains("recordCount"));
    }

    #[test]
    fn zero_change_baseline_requires_exact_quiet_journal_compatibility() {
        let saved = vec![checkpoint(50)];

        // A journal may retain older records, but it must still retain the
        // saved watermark and its next watermark must not have advanced.
        let mut unchanged = checkpoint(50);
        unchanged.lowest_valid_usn = 25;
        assert!(validate_zero_change_baseline(&saved, &[unchanged]).is_ok());

        let advanced = checkpoint(51);
        assert!(validate_zero_change_baseline(&saved, &[advanced]).is_err());

        let mut aged_out = checkpoint(50);
        aged_out.lowest_valid_usn = 51;
        assert!(validate_zero_change_baseline(&saved, &[aged_out]).is_err());

        let mut different_serial = checkpoint(50);
        different_serial.volume_serial_number += 1;
        assert!(validate_zero_change_baseline(&saved, &[different_serial]).is_err());

        let mut recreated_journal = checkpoint(50);
        recreated_journal.journal_id += 1;
        assert!(validate_zero_change_baseline(&saved, &[recreated_journal]).is_err());

        assert!(validate_zero_change_baseline(&saved, &[]).is_err());

        let mut unexpected_volume = checkpoint(50);
        unexpected_volume.volume_key = "D:".to_owned();
        assert!(validate_zero_change_baseline(&saved, &[unexpected_volume]).is_err());
    }

    #[test]
    fn final_multi_volume_quiet_check_rejects_another_volume_advancing() {
        let c = checkpoint(50);
        let mut d = checkpoint(70);
        d.volume_key = "D:".to_owned();
        let saved = vec![c.clone(), d.clone()];
        assert!(validate_zero_change_baseline(&saved, &[c.clone(), d.clone()]).is_ok());

        let mut advanced_d = d;
        advanced_d.next_usn += 1;
        assert!(validate_zero_change_baseline(&saved, &[c, advanced_d]).is_err());
    }

    #[test]
    fn zero_change_root_set_requires_unique_direct_drive_roots() {
        let roots = vec![PathBuf::from(r"C:\"), PathBuf::from(r"D:\")];
        let targets = collect_strict_direct_volume_targets(&roots).unwrap();
        assert_eq!(
            targets
                .iter()
                .map(|target| target.volume_key.as_str())
                .collect::<Vec<_>>(),
            ["C:", "D:"]
        );

        assert!(collect_strict_direct_volume_targets(&[]).is_err());
        assert!(collect_strict_direct_volume_targets(&[PathBuf::from(r"C:\Users\iHub")]).is_err());
        assert!(collect_strict_direct_volume_targets(&[PathBuf::from(r"\\server\share")]).is_err());
        assert!(collect_strict_direct_volume_targets(&[
            PathBuf::from(r"C:\"),
            PathBuf::from(r"\\?\c:\"),
        ])
        .is_err());
    }

    #[test]
    fn mft_names_must_be_safe_for_ordinary_win32_projection() {
        for unsafe_name in [
            "CON",
            "nul.txt",
            "COM1.log",
            "LPT9",
            "trailing.",
            "trailing ",
            "wild?.txt",
            "quote\".txt",
            "pipe|.txt",
            "line\nfeed.txt",
        ] {
            assert!(
                !is_safe_mft_path_component(unsafe_name),
                "{unsafe_name:?} must not be projected through ordinary Win32 paths"
            );
        }
        assert!(is_safe_mft_path_component("正常文件.txt"));
        assert!(is_safe_mft_path_component("project-01"));
        assert_eq!(
            normalized_windows_path_key(Path::new(r"\\?\C:\Projects\notes.md")).unwrap(),
            r"C:\PROJECTS\NOTES.MD"
        );
        assert!(normalized_windows_path_key(Path::new(r"\\.\C:\Projects\notes.md")).is_err());
    }

    #[test]
    fn replay_binding_accepts_a_complete_direct_drive_identity_projection() {
        let binding = replay_binding(vec![
            replay_root(),
            replay_path(r"C:\Projects", 10, 5, "Projects", true),
            replay_path(r"C:\Projects\notes.md", 20, 10, "notes.md", false),
        ]);

        validate_replay_binding(&[PathBuf::from(r"C:\")], &binding).unwrap();
        let payload = serde_json::to_string(&binding).unwrap();
        assert!(payload.contains("fileReferenceNumber"));
        assert!(payload.contains("parentFileReferenceNumber"));
        assert!(payload.contains("isRoot"));
        assert!(!payload.contains("replayedUsn"));
    }

    #[test]
    fn replay_binding_rejects_narrow_or_mismatched_authority() {
        let binding = replay_binding(vec![replay_root()]);
        assert!(validate_replay_binding(&[PathBuf::from(r"C:\Users\iHub")], &binding).is_err());

        let mut wrong_checkpoint = binding.clone();
        wrong_checkpoint.checkpoints[0].volume_key = "D:".to_owned();
        assert!(validate_replay_binding(&[PathBuf::from(r"C:\")], &wrong_checkpoint).is_err());

        let mut wrong_volume = binding;
        wrong_volume.volumes[0].volume_root = PathBuf::from(r"D:\");
        assert!(validate_replay_binding(&[PathBuf::from(r"C:\")], &wrong_volume).is_err());
    }

    #[test]
    fn replay_binding_rejects_ambiguous_directory_aliases_and_incomplete_chains() {
        let ambiguous_directory = replay_binding(vec![
            replay_root(),
            replay_path(r"C:\One", 10, 5, "One", true),
            replay_path(r"C:\Two", 10, 5, "Two", true),
        ]);
        let error =
            validate_replay_binding(&[PathBuf::from(r"C:\")], &ambiguous_directory).unwrap_err();
        assert!(error.contains("多个别名"));

        let missing_parent = replay_binding(vec![
            replay_root(),
            replay_path(r"C:\Missing\notes.md", 20, 10, "notes.md", false),
        ]);
        let error = validate_replay_binding(&[PathBuf::from(r"C:\")], &missing_parent).unwrap_err();
        assert!(error.contains("父目录链"));
    }

    #[test]
    fn replay_binding_rejects_path_escape_case_alias_and_parent_mismatch() {
        let escaped = replay_binding(vec![
            replay_root(),
            replay_path(r"D:\escape.txt", 20, 5, "escape.txt", false),
        ]);
        assert!(validate_replay_binding(&[PathBuf::from(r"C:\")], &escaped).is_err());

        let case_alias = replay_binding(vec![
            replay_root(),
            replay_path(r"C:\Readme.txt", 20, 5, "Readme.txt", false),
            replay_path(r"C:\README.TXT", 21, 5, "README.TXT", false),
        ]);
        assert!(validate_replay_binding(&[PathBuf::from(r"C:\")], &case_alias).is_err());

        let wrong_parent_name = replay_binding(vec![
            replay_root(),
            replay_path(r"C:\actual.txt", 20, 5, "different.txt", false),
        ]);
        assert!(validate_replay_binding(&[PathBuf::from(r"C:\")], &wrong_parent_name).is_err());
    }

    // These transition assertions depend on native Windows separator
    // semantics; the Windows CI job remains their execution authority.
    #[cfg(windows)]
    #[test]
    fn stable_replay_path_state_updates_create_delete_and_metadata_dirty_paths() {
        let binding = replay_binding(vec![
            replay_root(),
            replay_path(r"C:\Projects", 10, 5, "Projects", true),
            replay_path(r"C:\Projects\notes.md", 20, 10, "notes.md", false),
        ]);
        let mut state = StableReplayPathState::from_volume(&binding.volumes[0]).unwrap();
        let mut dirty = ReplayDirtySet::default();

        state
            .apply_delta(
                &delta(20, 10, 60, USN_REASON_DATA_OVERWRITE, "notes.md", 0),
                &mut dirty,
            )
            .unwrap();
        state
            .apply_delta(
                &delta(21, 10, 61, USN_REASON_FILE_CREATE, "new.txt", 0),
                &mut dirty,
            )
            .unwrap();
        state
            .apply_delta(
                &delta(21, 10, 62, USN_REASON_FILE_DELETE, "new.txt", 0),
                &mut dirty,
            )
            .unwrap();
        state.finish().unwrap();

        let volume = state.into_volume().unwrap();
        assert!(volume
            .paths
            .iter()
            .any(|path| path.path == Path::new(r"C:\Projects\notes.md")));
        assert!(!volume
            .paths
            .iter()
            .any(|path| path.path == Path::new(r"C:\Projects\new.txt")));
        let mut updated = binding;
        updated.volumes = vec![volume];
        validate_replay_binding(&[PathBuf::from(r"C:\")], &updated).unwrap();

        let (dirty_paths, dirty_references) = dirty.into_parts();
        assert!(dirty_paths.contains(&PathBuf::from(r"C:\Projects\notes.md")));
        assert!(dirty_paths.contains(&PathBuf::from(r"C:\Projects\new.txt")));
        assert!(dirty_references.contains(&UsnReplayFileReference {
            volume_key: "C:".to_owned(),
            file_reference_number: 20,
        }));
    }

    #[cfg(windows)]
    #[test]
    fn stable_replay_path_state_renames_a_directory_subtree_and_marks_both_sides_dirty() {
        let binding = replay_binding(vec![
            replay_root(),
            replay_path(r"C:\Projects", 10, 5, "Projects", true),
            replay_path(r"C:\Projects\Nested", 11, 10, "Nested", true),
            replay_path(r"C:\Projects\Nested\notes.md", 20, 11, "notes.md", false),
        ]);
        let mut state = StableReplayPathState::from_volume(&binding.volumes[0]).unwrap();
        let mut dirty = ReplayDirtySet::default();

        state
            .apply_delta(
                &delta(
                    10,
                    5,
                    60,
                    USN_REASON_RENAME_OLD_NAME,
                    "Projects",
                    NTFS_FILE_ATTRIBUTE_DIRECTORY,
                ),
                &mut dirty,
            )
            .unwrap();
        state
            .apply_delta(
                &delta(
                    10,
                    5,
                    61,
                    USN_REASON_RENAME_NEW_NAME,
                    "Archive",
                    NTFS_FILE_ATTRIBUTE_DIRECTORY,
                ),
                &mut dirty,
            )
            .unwrap();
        state.finish().unwrap();
        let volume = state.into_volume().unwrap();
        assert!(volume.paths.iter().any(|path| {
            path.path == Path::new(r"C:\Archive\Nested\notes.md")
                && path.parent_file_reference_number == 11
        }));
        assert!(!volume
            .paths
            .iter()
            .any(|path| path.path == Path::new(r"C:\Projects\Nested\notes.md")));

        let (dirty_paths, dirty_references) = dirty.into_parts();
        assert!(dirty_paths.contains(&PathBuf::from(r"C:\Projects\Nested\notes.md")));
        assert!(dirty_paths.contains(&PathBuf::from(r"C:\Archive\Nested\notes.md")));
        assert!(dirty_references.contains(&UsnReplayFileReference {
            volume_key: "C:".to_owned(),
            file_reference_number: 20,
        }));
    }

    #[cfg(windows)]
    #[test]
    fn stable_replay_path_state_keeps_both_raw_spellings_of_a_case_only_rename() {
        let binding = replay_binding(vec![
            replay_root(),
            replay_path(r"C:\Projects", 10, 5, "Projects", true),
            replay_path(r"C:\Projects\Readme.md", 20, 10, "Readme.md", false),
        ]);
        let mut state = StableReplayPathState::from_volume(&binding.volumes[0]).unwrap();
        let mut dirty = ReplayDirtySet::default();

        state
            .apply_delta(
                &delta(20, 10, 60, USN_REASON_RENAME_OLD_NAME, "Readme.md", 0),
                &mut dirty,
            )
            .unwrap();
        state
            .apply_delta(
                &delta(20, 10, 61, USN_REASON_RENAME_NEW_NAME, "README.md", 0),
                &mut dirty,
            )
            .unwrap();
        state.finish().unwrap();

        let volume = state.into_volume().unwrap();
        assert!(volume
            .paths
            .iter()
            .any(|path| path.path == Path::new(r"C:\Projects\README.md")));
        let (dirty_paths, _) = dirty.into_parts();
        assert!(dirty_paths.contains(&PathBuf::from(r"C:\Projects\Readme.md")));
        assert!(dirty_paths.contains(&PathBuf::from(r"C:\Projects\README.md")));
    }

    #[cfg(windows)]
    #[test]
    fn stable_replay_path_state_keeps_both_raw_spellings_of_a_case_only_directory_rename() {
        let binding = replay_binding(vec![
            replay_root(),
            replay_path(r"C:\Projects", 10, 5, "Projects", true),
            replay_path(r"C:\Projects\Readme.md", 20, 10, "Readme.md", false),
        ]);
        let mut state = StableReplayPathState::from_volume(&binding.volumes[0]).unwrap();
        let mut dirty = ReplayDirtySet::default();

        state
            .apply_delta(
                &delta(
                    10,
                    5,
                    60,
                    USN_REASON_RENAME_OLD_NAME,
                    "Projects",
                    NTFS_FILE_ATTRIBUTE_DIRECTORY,
                ),
                &mut dirty,
            )
            .unwrap();
        state
            .apply_delta(
                &delta(
                    10,
                    5,
                    61,
                    USN_REASON_RENAME_NEW_NAME,
                    "PROJECTS",
                    NTFS_FILE_ATTRIBUTE_DIRECTORY,
                ),
                &mut dirty,
            )
            .unwrap();
        let volume = state.into_volume().unwrap();
        assert!(volume
            .paths
            .iter()
            .any(|path| path.path == Path::new(r"C:\PROJECTS\Readme.md")));

        let (dirty_paths, _) = dirty.into_parts();
        assert!(dirty_paths.contains(&PathBuf::from(r"C:\Projects")));
        assert!(dirty_paths.contains(&PathBuf::from(r"C:\PROJECTS")));
        assert!(dirty_paths.contains(&PathBuf::from(r"C:\Projects\Readme.md")));
        assert!(dirty_paths.contains(&PathBuf::from(r"C:\PROJECTS\Readme.md")));
    }

    #[cfg(windows)]
    #[test]
    fn stable_replay_path_state_keeps_existing_file_hardlink_aliases_exact() {
        let binding = replay_binding(vec![
            replay_root(),
            replay_path(r"C:\One", 10, 5, "One", true),
            replay_path(r"C:\Two", 11, 5, "Two", true),
            replay_path(r"C:\One\shared.txt", 20, 10, "shared.txt", false),
            replay_path(r"C:\Two\shared.txt", 20, 11, "shared.txt", false),
        ]);
        let mut state = StableReplayPathState::from_volume(&binding.volumes[0]).unwrap();
        let mut dirty = ReplayDirtySet::default();

        state
            .apply_delta(
                &delta(20, 10, 60, USN_REASON_DATA_OVERWRITE, "shared.txt", 0),
                &mut dirty,
            )
            .unwrap();
        state
            .apply_delta(
                &delta(20, 10, 61, USN_REASON_RENAME_OLD_NAME, "shared.txt", 0),
                &mut dirty,
            )
            .unwrap();
        state
            .apply_delta(
                &delta(20, 10, 62, USN_REASON_RENAME_NEW_NAME, "renamed.txt", 0),
                &mut dirty,
            )
            .unwrap();
        let volume = state.into_volume().unwrap();
        assert!(volume
            .paths
            .iter()
            .any(|path| path.path == Path::new(r"C:\One\renamed.txt")));
        assert!(volume
            .paths
            .iter()
            .any(|path| path.path == Path::new(r"C:\Two\shared.txt")));
        let (dirty_paths, _) = dirty.into_parts();
        assert!(dirty_paths.contains(&PathBuf::from(r"C:\One\shared.txt")));
        assert!(dirty_paths.contains(&PathBuf::from(r"C:\Two\shared.txt")));
    }

    #[test]
    fn stable_replay_path_state_rejects_unpaired_or_unrepresentable_topology() {
        let binding = replay_binding(vec![
            replay_root(),
            replay_path(r"C:\Projects", 10, 5, "Projects", true),
            replay_path(r"C:\Projects\notes.md", 20, 10, "notes.md", false),
        ]);
        let mut state = StableReplayPathState::from_volume(&binding.volumes[0]).unwrap();
        let mut dirty = ReplayDirtySet::default();
        state
            .apply_delta(
                &delta(20, 10, 60, USN_REASON_RENAME_OLD_NAME, "notes.md", 0),
                &mut dirty,
            )
            .unwrap();
        assert!(state.finish().is_err());

        let mut state = StableReplayPathState::from_volume(&binding.volumes[0]).unwrap();
        assert!(state
            .apply_delta(
                &delta(20, 10, 60, USN_REASON_HARD_LINK_CHANGE, "notes.md", 0,),
                &mut dirty,
            )
            .is_err());
        assert!(state
            .apply_delta(
                &delta(30, 99, 61, USN_REASON_FILE_CREATE, "orphan.txt", 0),
                &mut dirty
            )
            .is_err());
    }

    #[test]
    fn stable_replay_path_state_keeps_bounded_delete_tombstones_for_directory_children() {
        let binding = replay_binding(vec![
            replay_root(),
            replay_path(r"C:\Projects", 10, 5, "Projects", true),
            replay_path(r"C:\Projects\notes.md", 20, 10, "notes.md", false),
        ]);
        let mut state = StableReplayPathState::from_volume(&binding.volumes[0]).unwrap();
        let mut dirty = ReplayDirtySet::default();

        // The directory event may arrive before an already-deleted child
        // event. Keep bounded tombstones so that ordering is representable,
        // but a second unknown alias still remains a fail-closed error.
        state
            .apply_delta(
                &delta(
                    10,
                    5,
                    60,
                    USN_REASON_FILE_DELETE,
                    "Projects",
                    NTFS_FILE_ATTRIBUTE_DIRECTORY,
                ),
                &mut dirty,
            )
            .unwrap();
        state
            .apply_delta(
                &delta(20, 10, 61, USN_REASON_FILE_DELETE, "notes.md", 0),
                &mut dirty,
            )
            .unwrap();
        state.finish().unwrap();
        let volume = state.into_volume().unwrap();
        assert_eq!(volume.paths, vec![replay_root()]);
    }

    #[test]
    fn persisted_replay_window_requires_contiguous_journal_and_quiet_cutoff() {
        let saved = checkpoint(50);
        let cutoff = state(90, 1);
        assert!(validate_persisted_replay_window(&saved, &cutoff).is_ok());
        assert!(validate_quiet_replay_cutoff(&cutoff, &cutoff).is_ok());

        let mut aged_out = cutoff.clone();
        aged_out.lowest_valid_usn = 51;
        assert!(validate_persisted_replay_window(&saved, &aged_out).is_err());

        let mut advanced = cutoff.clone();
        advanced.next_usn += 1;
        assert!(validate_quiet_replay_cutoff(&cutoff, &advanced).is_err());
    }

    fn v2_record(
        file_reference: u64,
        parent_reference: u64,
        name: &str,
        attributes: u32,
    ) -> Vec<u8> {
        let encoded_name = name.encode_utf16().collect::<Vec<_>>();
        let unpadded_length = 60 + encoded_name.len() * 2;
        let record_length = (unpadded_length + 7) & !7;
        let mut record = vec![0u8; record_length];
        record[0..4].copy_from_slice(&(record_length as u32).to_le_bytes());
        record[4..6].copy_from_slice(&2u16.to_le_bytes());
        record[8..16].copy_from_slice(&file_reference.to_le_bytes());
        record[16..24].copy_from_slice(&parent_reference.to_le_bytes());
        record[52..56].copy_from_slice(&attributes.to_le_bytes());
        record[56..58].copy_from_slice(&((encoded_name.len() * 2) as u16).to_le_bytes());
        record[58..60].copy_from_slice(&60u16.to_le_bytes());
        for (offset, unit) in encoded_name.into_iter().enumerate() {
            let start = 60 + offset * 2;
            record[start..start + 2].copy_from_slice(&unit.to_le_bytes());
        }
        record
    }

    fn v2_delta_record(
        file_reference: u64,
        parent_reference: u64,
        usn: i64,
        reason: u32,
        name: &str,
        attributes: u32,
    ) -> Vec<u8> {
        let mut record = v2_record(file_reference, parent_reference, name, attributes);
        record[24..32].copy_from_slice(&usn.to_le_bytes());
        record[40..44].copy_from_slice(&reason.to_le_bytes());
        record
    }

    fn mft_reply(next_reference: u64, records: impl IntoIterator<Item = Vec<u8>>) -> Vec<u8> {
        let mut bytes = next_reference.to_le_bytes().to_vec();
        for record in records {
            bytes.extend(record);
        }
        bytes
    }

    fn delta_reply(next_usn: i64, records: impl IntoIterator<Item = Vec<u8>>) -> Vec<u8> {
        let mut bytes = next_usn.to_le_bytes().to_vec();
        for record in records {
            bytes.extend(record);
        }
        bytes
    }

    fn delta(
        file_reference_number: u64,
        parent_file_reference_number: u64,
        usn: i64,
        reason: u32,
        name: &str,
        attributes: u32,
    ) -> UsnDeltaRecord {
        UsnDeltaRecord {
            file_reference_number,
            parent_file_reference_number,
            usn,
            reason,
            attributes,
            name: name.to_owned(),
        }
    }

    #[test]
    fn mft_path_is_limited_to_an_explicit_drive_root() {
        let target = drive_target_from_root(r"C:\").unwrap();
        assert!(is_direct_volume_root(r"C:\", &target));
        assert!(is_direct_volume_root(r"\\?\c:\", &target));
        assert!(!is_direct_volume_root(r"C:\Users\iHub", &target));
        assert!(!is_direct_volume_root(r"D:\", &target));
        assert!(!is_direct_volume_root(r"\\server\share", &target));
    }

    #[test]
    fn parses_only_bounded_v2_mft_records_and_skips_reparse_names() {
        let reply = mft_reply(
            44,
            [
                v2_record(10, 5, "Projects", NTFS_FILE_ATTRIBUTE_DIRECTORY),
                v2_record(11, 10, "junction", NTFS_FILE_ATTRIBUTE_REPARSE_POINT),
                v2_record(12, 10, "notes.md", 0),
            ],
        );

        let parsed = parse_mft_reply(&reply).expect("a valid V2 MFT reply should parse");
        assert_eq!(parsed.next_start_file_reference_number, 44);
        assert_eq!(parsed.record_count, 3);
        assert_eq!(
            parsed
                .records
                .iter()
                .map(|record| record.name.as_str())
                .collect::<Vec<_>>(),
            ["Projects", "notes.md"]
        );
        assert!(parsed.records[0].is_directory);
        assert!(!parsed.records[1].is_directory);
    }

    #[test]
    fn malformed_mft_name_bounds_fail_closed() {
        let mut record = v2_record(10, 5, "safe.txt", 0);
        record[58..60].copy_from_slice(&58u16.to_le_bytes());
        let error = parse_mft_reply(&mft_reply(12, [record])).unwrap_err();
        assert!(error.contains("文件名边界"));
    }

    #[test]
    fn unaligned_v2_record_lengths_fail_closed() {
        // The name itself remains within the truncated record, so this proves
        // we reject the invalid alignment rather than merely a later bounds
        // error caused by the shortened padding.
        let mut mft_record = v2_record(10, 5, "safe", 0);
        mft_record.truncate(mft_record.len() - 1);
        let mft_length = mft_record.len() as u32;
        mft_record[0..4].copy_from_slice(&mft_length.to_le_bytes());
        let mft_error = parse_mft_reply(&mft_reply(12, [mft_record])).unwrap_err();
        assert!(mft_error.contains("长度"));

        let mut delta_record = v2_delta_record(10, 5, 100, USN_REASON_FILE_CREATE, "safe", 0);
        delta_record.truncate(delta_record.len() - 1);
        let delta_length = delta_record.len() as u32;
        delta_record[0..4].copy_from_slice(&delta_length.to_le_bytes());
        let delta_error = parse_usn_delta_reply(&delta_reply(120, [delta_record])).unwrap_err();
        assert!(delta_error.contains("长度"));
    }

    #[test]
    fn parses_only_bounded_v2_initialization_window_records() {
        let reply = delta_reply(
            140,
            [
                v2_delta_record(10, 5, 100, USN_REASON_FILE_CREATE, "folder", 0),
                v2_delta_record(11, 10, 120, USN_REASON_CLOSE, "notes.md", 0),
            ],
        );

        let parsed = parse_usn_delta_reply(&reply)
            .expect("a valid V2 initialization-window reply should parse");
        assert_eq!(parsed.next_usn, 140);
        assert_eq!(parsed.record_count, 2);
        assert_eq!(parsed.records[0].reason, USN_REASON_FILE_CREATE);
        assert_eq!(parsed.records[1].usn, 120);
        assert_eq!(parsed.records[1].name, "notes.md");
    }

    #[test]
    fn malformed_or_newer_initialization_window_records_fail_closed() {
        let mut version_three = v2_delta_record(10, 5, 100, USN_REASON_FILE_CREATE, "safe", 0);
        version_three[4..6].copy_from_slice(&3u16.to_le_bytes());
        let version_error = parse_usn_delta_reply(&delta_reply(120, [version_three]))
            .expect_err("a V3 layout must never be decoded as V2");
        assert!(version_error.contains("版本"));

        let mut unknown_minor = v2_delta_record(10, 5, 100, USN_REASON_FILE_CREATE, "safe", 0);
        unknown_minor[6..8].copy_from_slice(&1u16.to_le_bytes());
        assert!(parse_usn_delta_reply(&delta_reply(120, [unknown_minor])).is_err());

        let mut malformed = v2_delta_record(10, 5, 100, USN_REASON_FILE_CREATE, "safe", 0);
        malformed[58..60].copy_from_slice(&58u16.to_le_bytes());
        let bounds_error = parse_usn_delta_reply(&delta_reply(120, [malformed]))
            .expect_err("invalid filename bounds must force the scoped scanner fallback");
        assert!(bounds_error.contains("文件名边界"));
    }

    #[test]
    fn initialization_window_requires_one_live_contiguous_journal() {
        let initial = state(100, 10);
        let cutoff = state(150, 10);
        assert!(validate_initialization_replay_window(&initial, &cutoff).is_ok());

        let mut recreated = cutoff.clone();
        recreated.journal_id += 1;
        assert!(validate_initialization_replay_window(&initial, &recreated).is_err());

        let truncated = state(150, 101);
        assert!(validate_initialization_replay_window(&initial, &truncated).is_err());

        let backwards = state(99, 10);
        assert!(validate_initialization_replay_window(&initial, &backwards).is_err());
    }

    #[test]
    fn initialization_window_topology_replay_updates_only_the_transient_projection() {
        let records = vec![
            MftRecord {
                file_reference_number: 10,
                parent_file_reference_number: 5,
                name: "folder".to_owned(),
                is_directory: true,
            },
            MftRecord {
                file_reference_number: 20,
                parent_file_reference_number: 10,
                name: "old.txt".to_owned(),
                is_directory: false,
            },
        ];
        let mut projection =
            MftPathState::from_records(records, 5, PathBuf::from("/authorised"), 16).unwrap();

        projection
            .apply_delta(&delta(
                20,
                10,
                100,
                USN_REASON_RENAME_OLD_NAME,
                "old.txt",
                0,
            ))
            .unwrap();
        projection
            .apply_delta(&delta(
                20,
                10,
                110,
                USN_REASON_RENAME_NEW_NAME,
                "new.txt",
                0,
            ))
            .unwrap();
        projection
            .apply_delta(&delta(
                21,
                10,
                120,
                USN_REASON_FILE_CREATE,
                "temporary.txt",
                0,
            ))
            .unwrap();
        projection
            .apply_delta(&delta(
                21,
                10,
                130,
                USN_REASON_FILE_DELETE,
                "temporary.txt",
                0,
            ))
            .unwrap();

        assert_eq!(
            projection.build_paths().unwrap(),
            vec![
                PathBuf::from("/authorised"),
                PathBuf::from("/authorised/folder"),
                PathBuf::from("/authorised/folder/new.txt"),
            ]
        );
    }

    #[test]
    fn initialization_window_delete_preserves_another_existing_hard_link() {
        let records = vec![
            MftRecord {
                file_reference_number: 10,
                parent_file_reference_number: 5,
                name: "one".to_owned(),
                is_directory: true,
            },
            MftRecord {
                file_reference_number: 11,
                parent_file_reference_number: 5,
                name: "two".to_owned(),
                is_directory: true,
            },
            MftRecord {
                file_reference_number: 20,
                parent_file_reference_number: 10,
                name: "shared.txt".to_owned(),
                is_directory: false,
            },
            MftRecord {
                file_reference_number: 20,
                parent_file_reference_number: 11,
                name: "shared.txt".to_owned(),
                is_directory: false,
            },
        ];
        let mut projection =
            MftPathState::from_records(records, 5, PathBuf::from("/authorised"), 16).unwrap();
        projection
            .apply_delta(&delta(20, 10, 100, USN_REASON_FILE_DELETE, "shared.txt", 0))
            .unwrap();

        let paths = projection.build_paths().unwrap();
        assert!(!paths.contains(&PathBuf::from("/authorised/one/shared.txt")));
        assert!(paths.contains(&PathBuf::from("/authorised/two/shared.txt")));
    }

    #[test]
    fn unsupported_initialization_window_topology_fails_closed() {
        let initial = vec![MftRecord {
            file_reference_number: 10,
            parent_file_reference_number: 5,
            name: "folder".to_owned(),
            is_directory: true,
        }];
        let mut projection =
            MftPathState::from_records(initial.clone(), 5, PathBuf::from("/authorised"), 16)
                .unwrap();
        let hard_link = projection
            .apply_delta(&delta(
                20,
                10,
                100,
                USN_REASON_HARD_LINK_CHANGE,
                "shared.txt",
                0,
            ))
            .unwrap_err();
        assert!(hard_link.contains("硬链接"));

        let mut projection =
            MftPathState::from_records(initial.clone(), 5, PathBuf::from("/authorised"), 16)
                .unwrap();
        let reparse = projection
            .apply_delta(&delta(
                20,
                10,
                100,
                USN_REASON_FILE_CREATE,
                "junction",
                NTFS_FILE_ATTRIBUTE_REPARSE_POINT,
            ))
            .unwrap_err();
        assert!(reparse.contains("重解析点"));

        let mut projection =
            MftPathState::from_records(initial, 5, PathBuf::from("/authorised"), 16).unwrap();
        let unknown = projection
            .apply_delta(&delta(20, 10, 100, 0x0200_0000, "unknown", 0))
            .unwrap_err();
        assert!(unknown.contains("未知原因"));
    }

    #[test]
    fn mft_path_reconstruction_keeps_authorized_root_and_hard_links() {
        let records = vec![
            MftRecord {
                file_reference_number: 10,
                parent_file_reference_number: 5,
                name: "one".to_owned(),
                is_directory: true,
            },
            MftRecord {
                file_reference_number: 11,
                parent_file_reference_number: 5,
                name: "two".to_owned(),
                is_directory: true,
            },
            MftRecord {
                file_reference_number: 20,
                parent_file_reference_number: 10,
                name: "shared.txt".to_owned(),
                is_directory: false,
            },
            MftRecord {
                file_reference_number: 20,
                parent_file_reference_number: 11,
                name: "shared.txt".to_owned(),
                is_directory: false,
            },
        ];
        let paths = build_mft_paths(records, 5, PathBuf::from("/authorised"), 16)
            .expect("a tree rooted in the authorised file reference should resolve");
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/authorised"),
                PathBuf::from("/authorised/one"),
                PathBuf::from("/authorised/two"),
                PathBuf::from("/authorised/one/shared.txt"),
                PathBuf::from("/authorised/two/shared.txt"),
            ]
        );
    }

    #[test]
    fn mft_identity_projection_keeps_root_and_record_tuple() {
        let records = vec![
            MftRecord {
                file_reference_number: 10,
                parent_file_reference_number: 5,
                name: "Projects".to_owned(),
                is_directory: true,
            },
            MftRecord {
                file_reference_number: 20,
                parent_file_reference_number: 10,
                name: "notes.md".to_owned(),
                is_directory: false,
            },
        ];
        let state = MftPathState::from_records(records, 5, PathBuf::from(r"C:\"), 16).unwrap();
        let entries = state.build_entries("C:").unwrap();

        assert_eq!(entries[0].path, PathBuf::from(r"C:\"));
        assert_eq!(entries[0].file_reference_number, 5);
        assert_eq!(entries[0].parent_file_reference_number, 5);
        assert!(entries[0].is_directory);
        assert!(entries[0].is_root);
        let file = entries
            .iter()
            .find(|entry| entry.name == "notes.md")
            .unwrap();
        assert_eq!(file.file_reference_number, 20);
        assert_eq!(file.parent_file_reference_number, 10);
        assert!(!file.is_directory);
        assert!(!file.is_root);
    }

    #[test]
    fn mft_path_reconstruction_rejects_parent_cycles() {
        let records = vec![
            MftRecord {
                file_reference_number: 10,
                parent_file_reference_number: 11,
                name: "one".to_owned(),
                is_directory: true,
            },
            MftRecord {
                file_reference_number: 11,
                parent_file_reference_number: 10,
                name: "two".to_owned(),
                is_directory: true,
            },
        ];
        let error = build_mft_paths(records, 5, PathBuf::from("/authorised"), 16).unwrap_err();
        assert!(error.contains("循环"));
    }

    #[test]
    fn mft_path_reconstruction_falls_back_when_root_reference_does_not_match() {
        let records = vec![MftRecord {
            file_reference_number: 10,
            parent_file_reference_number: 9,
            name: "orphaned.txt".to_owned(),
            is_directory: false,
        }];
        let error = build_mft_paths(records, 5, PathBuf::from("/authorised"), 16).unwrap_err();
        assert!(error.contains("根目录"));
    }

    #[test]
    fn persisted_checkpoint_metadata_reloads_only_after_schema_validation() {
        let directory =
            std::env::temp_dir().join(format!("ihub-usn-checkpoint-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(CHECKPOINT_FILE_NAME);
        std::fs::write(&path, encode_checkpoints(&[checkpoint(50)]).unwrap()).unwrap();

        let loaded = load_checkpoints(&path);
        assert!(loaded.warning.is_none());
        assert_eq!(loaded.checkpoints, vec![checkpoint(50)]);

        std::fs::write(&path, br#"{"schemaVersion":99,"checkpoints":[]}"#).unwrap();
        let invalid = load_checkpoints(&path);
        assert!(invalid.checkpoints.is_empty());
        assert!(invalid.warning.is_some());
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[cfg(windows)]
    #[test]
    fn native_probe_is_read_only_and_reports_a_real_or_fallback_state() {
        let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_owned());
        let root = PathBuf::from(format!("{drive}\\"));
        let outcome = probe_authorized_roots(&[root], &[], None);

        // This call only reads volume metadata. Whether the runner permits
        // opening its volume or has an enabled journal is intentionally not a
        // test prerequisite: either condition must surface as an explicit
        // fallback instead of affecting scoped directory search.
        assert!(matches!(
            outcome.status,
            "available" | "degraded" | "fallback" | "inactive"
        ));
        assert!(!outcome.message.is_empty());
        assert!(outcome
            .checkpoints
            .iter()
            .all(|checkpoint| checkpoint.volume_key.ends_with(':')));
    }

    #[test]
    fn invalid_or_duplicate_checkpoint_sets_are_rejected_before_persistence() {
        let duplicate = vec![checkpoint(50), checkpoint(60)];
        assert!(encode_checkpoints(&duplicate).is_err());

        let mut invalid_watermark = checkpoint(50);
        invalid_watermark.lowest_valid_usn = 51;
        assert!(encode_checkpoints(&[invalid_watermark]).is_err());
    }
}
