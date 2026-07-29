use std::{
    cmp::Ordering as CompareOrdering,
    collections::{BinaryHeap, HashMap, HashSet, VecDeque},
    env, fs,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError, TrySendError},
        Arc, Mutex, OnceLock, RwLock,
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use crate::{
    models::{IndexStatus, SearchResult},
    ntfs_usn,
};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};
use ib_pinyin::{
    matcher::PinyinMatcher,
    pinyin::{PinyinData, PinyinNotation},
};
use ignore::{WalkBuilder, WalkState};
use notify::{recommended_watcher, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

const MAX_INDEXED_ENTRIES: usize = 500_000;
const DEFAULT_RESULT_LIMIT: usize = 50;
const MAX_RESULT_LIMIT: usize = 200;
// A compact per-entry ASCII character signature lets normal launcher queries
// reject impossible fuzzy candidates before constructing Skim's dynamic
// programming state. For a Chinese entry the signature also includes its
// bounded in-memory pinyin keys. It is deliberately only a necessary-condition
// filter: every query with a missing/stale signature falls back to the full
// scan and lazily rebuilds pinyin, so path-only, Unicode, phonetic, and content
// matches cannot be hidden by it.
const SEARCH_ASCII_LETTER_COUNT: u32 = 26;
const SEARCH_ASCII_DIGIT_COUNT: u32 = 10;
const SEARCH_ASCII_SIGNATURE_BITS: u32 = SEARCH_ASCII_LETTER_COUNT + SEARCH_ASCII_DIGIT_COUNT;
const _: () = assert!(SEARCH_ASCII_SIGNATURE_BITS <= u64::BITS);
const ALL_SEARCH_ASCII_SIGNATURE_BITS: u64 = (1_u64 << SEARCH_ASCII_SIGNATURE_BITS) - 1;
// Pinyin is an in-memory search projection, never part of the persistent path
// snapshot. The projection adds every dictionary reading to the existing
// two u64 candidate signatures; it does not allocate aliases per entry.
// Explicit caps keep hostile synthetic paths from monopolizing index-build
// time. A capped value sets every bit and therefore retains false positives
// rather than hiding a possible result.
const MAX_PINYIN_NAME_SOURCE_CHARS: usize = 320;
const MAX_PINYIN_PATH_SOURCE_CHARS: usize = 1_024;
// Direct Unicode name/path matches remain the primary ranking signal. Pinyin
// is a lower-scoring fallback so `zwjh` can find 中文计划 without allowing a
// phonetic match to outrank an exact English or on-disk name.
const PINYIN_NAME_FULL_BASE_SCORE: f64 = 360.0;
const PINYIN_NAME_INITIAL_BASE_SCORE: f64 = 300.0;
const PINYIN_PATH_FULL_BASE_SCORE: f64 = 230.0;
const PINYIN_PATH_INITIAL_BASE_SCORE: f64 = 190.0;
const PINYIN_NAME_FULL_PREFIX_BOOST: f64 = 110.0;
const PINYIN_NAME_INITIAL_PREFIX_BOOST: f64 = 80.0;
const PINYIN_PARTIAL_PENALTY: f64 = 55.0;
const PINYIN_NOTATIONS: PinyinNotation =
    PinyinNotation::Ascii.union(PinyinNotation::AsciiFirstLetter);
// `SkimMatcherV2` uses a dynamic-programming matrix. A launcher may search a
// large path index, so let unusually long path/query combinations fall back to
// the matcher library's linear algorithm instead of multiplying latency by the
// full matrix size for every candidate.
const FUZZY_MATCHER_ELEMENT_LIMIT: usize = 4_096;
// Ranking is intentionally filename-first: users normally type what appears
// in Finder/Explorer, not a parent-directory fragment. The fuzzy score still
// permits path-only discovery, but direct filename matches get deterministic
// boosts that cannot be eclipsed by a long path with favorable separators.
const EXACT_NAME_MATCH_BOOST: f64 = 1_200.0;
const NAME_PREFIX_MATCH_BOOST: f64 = 500.0;
const NAME_WORD_BOUNDARY_MATCH_BOOST: f64 = 160.0;
const PATH_ONLY_SCORE_WEIGHT: f64 = 0.85;
const CONTENT_MATCH_BASE_SCORE: f64 = 720.0;
// Start Menu trees are normally small, but this protects startup from an
// unexpectedly large or recursively linked shortcut hierarchy. Application
// discovery is deliberately separate from content indexing so it can become
// searchable before a large Documents/Desktop scan has completed.
const MAX_APPLICATION_ENTRIES: usize = 2_000;
#[cfg(any(windows, test))]
const MAX_APPLICATION_DIRECTORIES: usize = 1_000;
#[cfg(any(windows, test))]
const MAX_START_MENU_DEPTH: usize = 8;
// v3 atomically binds a complete path snapshot to a Windows P1e stable-path
// projection.  The former v2 file is deliberately left untouched and is not
// read as a P1e source: it has only a metadata-only P1d cursor, so it cannot
// prove that a changed journal can be replayed safely after restart.
const SNAPSHOT_FILE_NAME: &str = "local-index-v3.json";
const SNAPSHOT_SCHEMA_VERSION: u8 = 3;
/// v2 carries only a metadata-only P1d baseline. It is never a replay source,
/// but a one-time read-only path-cache fallback avoids throwing away useful
/// ordinary search results before the first v3 full scan completes.
const LEGACY_SNAPSHOT_FILE_NAME: &str = "local-index-v2.json";
const LEGACY_SNAPSHOT_SCHEMA_VERSION: u8 = 2;
const ROOTS_FILE_NAME: &str = "index-roots-v1.json";
const ROOTS_SCHEMA_VERSION: u8 = 1;
const MAX_CONFIGURED_ROOTS: usize = 32;
const MAX_ROOTS_FILE_BYTES: u64 = 64 * 1024;
// `notify` selects ReadDirectoryChangesW on Windows and FSEvents/kqueue on
// macOS. Individual, in-scope paths are reconciled after this quiet period;
// explicit overflow/rescan hints still take the conservative full-scan path.
const WATCH_EVENT_BUFFER: usize = 512;
const WATCH_CONTROL_POLL_INTERVAL: Duration = Duration::from_millis(125);
const WATCH_DEBOUNCE: Duration = Duration::from_millis(650);
const WATCH_MAX_BATCH_DELAY: Duration = Duration::from_secs(5);
const WATCH_SCAN_RETRY_DELAY: Duration = Duration::from_millis(400);
const MAX_INCREMENTAL_WATCH_PATHS: usize = 2_048;
const INCREMENTAL_SNAPSHOT_DEBOUNCE: Duration = Duration::from_secs(2);
const INCREMENTAL_SNAPSHOT_MAX_DELAY: Duration = Duration::from_secs(12);
const INCREMENTAL_SNAPSHOT_RETRY_DELAY: Duration = Duration::from_secs(5);
// A malformed or unexpectedly large index must never make startup allocate an
// unbounded amount of memory.  This is deliberately larger than ordinary
// path-only snapshots for the 500k-entry cap, while still rejecting nonsense.
const MAX_SNAPSHOT_BYTES: u64 = 256 * 1024 * 1024;
// A cached path list is an acceleration, never an authority.  If it has not
// been refreshed for this long (or appears to be from a meaningfully future
// clock), ignore it and let the normal authorized-root scan rebuild it.
const MAX_SNAPSHOT_AGE_DAYS: i64 = 30;
const MAX_SNAPSHOT_FUTURE_SKEW_MINUTES: i64 = 5;
const MAX_ATOMIC_REPLACE_ATTEMPTS: usize = 32;
// Full text is intentionally a separate, opt-in mode. It is never persisted
// into the path snapshot and it never delays a normal filename/path search.
// The limits make the memory/privacy cost explicit while still covering the
// small text, source, note, and configuration files that dominate launcher
// search use cases.
const MAX_CONTENT_INDEXED_FILES: usize = 8_000;
const MAX_CONTENT_SOURCE_FILE_BYTES: u64 = 1024 * 1024;
const MAX_CONTENT_BYTES_PER_FILE: usize = 48 * 1024;
const MAX_CONTENT_INDEX_BYTES: usize = 48 * 1024 * 1024;
const CONTENT_RESULT_PREVIEW_CHARS: usize = 180;
#[cfg(target_os = "macos")]
const MAX_MACOS_APPLICATION_DEPTH: usize = 2;

// `SearchIndex` serializes its own snapshot publishers, but a unique suffix
// also protects the state file when a previous process crashed after creating
// a temporary file or an external maintenance tool is inspecting the cache.
static ATOMIC_REPLACE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static PINYIN_DATA: OnceLock<PinyinData> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndexedEntry {
    id: String,
    path: String,
    name: String,
    kind: String,
    metadata: String,
    #[serde(default)]
    modified_at: Option<String>,
    #[serde(default)]
    extension: Option<String>,
    #[serde(default)]
    size_bytes: u64,
    /// Text bodies deliberately live only in memory. A persisted path
    /// snapshot must never become an undisclosed copy of the user's files.
    #[serde(skip)]
    content: Option<IndexedContent>,
}

/// Host-only source metadata for a launcher shortcut. It never crosses the
/// Tauri IPC boundary: the renderer receives only the shortcut's opaque ID
/// and display fields from the dedicated shortcut store.
#[derive(Debug, Clone)]
pub(crate) struct LauncherShortcutSource {
    pub(crate) id: String,
    pub(crate) path: PathBuf,
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) metadata: String,
}

/// A renderer-selected result resolved back through the current native index.
/// The path remains host-private and is used only by the Shell icon worker.
#[derive(Debug, Clone)]
pub(crate) struct ResolvedSystemIconSource {
    pub(crate) response_id: String,
    pub(crate) path: PathBuf,
    pub(crate) kind: String,
}

#[derive(Debug, Clone)]
struct IndexedContent {
    /// Whitespace-compacted original UTF-8 text for a small result preview.
    text: String,
    /// Pre-folded form prevents lowercasing every document on every query.
    folded: String,
    /// The actual resident byte cost of `text` + `folded`.
    memory_bytes: usize,
}

#[derive(Debug, Clone)]
struct ContentCandidate {
    id: String,
    path: PathBuf,
}

impl IndexedEntry {
    fn is_valid(&self) -> bool {
        !self.id.is_empty()
            && !self.path.is_empty()
            && !self.name.is_empty()
            && matches!(self.kind.as_str(), "file" | "folder" | "application")
    }

    fn extension_lower(&self) -> Option<String> {
        self.extension
            .as_deref()
            .or_else(|| {
                Path::new(&self.path)
                    .extension()
                    .and_then(|value| value.to_str())
            })
            .filter(|value| !value.is_empty())
            .map(|value| value.trim_start_matches('.').to_lowercase())
    }

    fn is_launcher_shortcut_eligible(&self) -> bool {
        match self.kind.as_str() {
            "file" => file_is_launcher_shortcut_eligible(Path::new(&self.path)),
            "folder" => true,
            "application" => application_is_launcher_shortcut_eligible(Path::new(&self.path)),
            _ => false,
        }
    }
}

#[derive(Debug)]
struct PersistedIndexSnapshot {
    schema_version: u8,
    roots: Vec<String>,
    last_indexed_at: String,
    /// Optional P1e replay metadata is intentionally part of the same atomic
    /// replacement as `entries`: an independently written cursor or identity
    /// projection could describe a different path snapshot.
    usn_binding: Option<PersistedUsnSnapshotBinding>,
    entries: Vec<IndexedEntry>,
}

/// Decode the cache envelope separately from its optional replay binding.
/// That lets a malformed, unknown-field, or old optional binding be rejected
/// without throwing away otherwise valid path results from the same atomic
/// snapshot.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedIndexSnapshotWire {
    schema_version: u8,
    roots: Vec<String>,
    last_indexed_at: String,
    #[serde(default)]
    usn_binding: Option<serde_json::Value>,
    entries: Vec<IndexedEntry>,
}

/// Deliberately excludes `usnBinding`: Serde skips unknown fields here, so a
/// legacy v2 cache can contribute only its ordinary entries and never has its
/// old metadata baseline decoded, migrated, or considered for fast start.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyPersistedIndexSnapshotWire {
    schema_version: u8,
    roots: Vec<String>,
    last_indexed_at: String,
    entries: Vec<IndexedEntry>,
}

/// A Windows-only fast-start proof. The outer fields bind it to the exact
/// snapshot scope, while the native payload contains the bounded identity
/// projection and the MFT initialization-window cutoff checkpoints. The
/// nested native type rejects unknown/partial fields before it is usable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PersistedUsnSnapshotBinding {
    schema_version: u8,
    roots: Vec<String>,
    replay: ntfs_usn::UsnReplayBinding,
}

const USN_SNAPSHOT_BINDING_SCHEMA_VERSION: u8 = 2;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedIndexSnapshotRef<'a> {
    schema_version: u8,
    roots: Vec<String>,
    last_indexed_at: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    usn_binding: Option<&'a PersistedUsnSnapshotBinding>,
    entries: &'a [IndexedEntry],
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedIndexRoots {
    schema_version: u8,
    roots: Vec<String>,
}

/// `Default` is distinct from an unavailable user-selected scope. If a
/// removable drive disappears or the saved configuration becomes malformed,
/// falling back to Desktop/Documents would silently broaden what iHub scans.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RootSelection {
    Default,
    Custom(Vec<PathBuf>),
    Unavailable,
}

impl RootSelection {
    fn active_roots(&self) -> Vec<PathBuf> {
        match self {
            Self::Default => default_roots(),
            Self::Custom(roots) => roots.clone(),
            Self::Unavailable => Vec::new(),
        }
    }

    fn is_unavailable(&self) -> bool {
        matches!(self, Self::Unavailable)
    }
}

#[derive(Debug)]
struct IndexInner {
    /// The entries and their non-persisted candidate signatures are published
    /// under one lock. A query can therefore never pair a same-length but
    /// stale signature vector with a newer path snapshot.
    entries: RwLock<InMemorySearchSnapshot>,
    status: RwLock<IndexStatus>,
    generation: AtomicU64,
    /// Increments on every body-index invalidation. Workers must match both
    /// this revision and the path-index generation before publishing text.
    content_revision: AtomicU64,
    snapshot_path: Option<PathBuf>,
    snapshot_lock: Mutex<()>,
    /// iHub-owned state must not become an indexed result or cause the
    /// recursive user-root watcher to react to its own atomic snapshots.
    /// These are host state directories, never a user-visible content scope.
    internal_state_roots: Vec<PathBuf>,
    /// Restored only from the atomically-bound v3 snapshot. It is consumed
    /// once, after the watcher has registered the current roots; every failed
    /// verification falls through to the existing full rebuild.
    startup_usn_binding: Mutex<Option<PersistedUsnSnapshotBinding>>,
    configured_roots: RwLock<RootSelection>,
    roots_path: Option<PathBuf>,
    roots_lock: Mutex<()>,
    /// Windows-only metadata cache for the explicit NTFS USN P1a probe. It
    /// contains volume IDs/watermarks only, never file paths or records.
    #[cfg(windows)]
    usn_checkpoint_path: Option<PathBuf>,
    #[cfg(windows)]
    usn_checkpoint_lock: Mutex<()>,
    // The watcher owns its native handle in a dedicated thread. Keeping only
    // this small control sender in the shared index makes root changes
    // explicit and avoids holding an OS watcher while scanning/searching.
    watcher_control: Mutex<Option<Sender<WatchControl>>>,
    watcher_requested: AtomicBool,
}

mod in_memory_search_snapshot {
    use std::collections::HashMap;

    use super::{
        build_search_ascii_signatures, IndexedContent, IndexedEntry, SearchAsciiSignature,
    };

    /// The ordinary entry records and their lightweight search projection form
    /// a single in-memory publication unit. The signature can always be
    /// recreated from `records`, so it is deliberately absent from persistent
    /// snapshots. Its fields stay private to this module: callers can publish
    /// path changes only through `replace`, which rebuilds the projection.
    #[derive(Debug)]
    pub(super) struct InMemorySearchSnapshot {
        records: Vec<IndexedEntry>,
        search_ascii_signatures: Vec<SearchAsciiSignature>,
    }

    impl InMemorySearchSnapshot {
        pub(super) fn new(records: Vec<IndexedEntry>) -> Self {
            let search_ascii_signatures = build_search_ascii_signatures(&records);
            Self {
                records,
                search_ascii_signatures,
            }
        }

        /// Replaces all filename/path records and atomically refreshes their
        /// projection before the enclosing write lock can be released.
        pub(super) fn replace(&mut self, records: Vec<IndexedEntry>) {
            self.search_ascii_signatures = build_search_ascii_signatures(&records);
            self.records = records;
        }

        /// Temporarily moves the records out while an exclusive writer
        /// prepares a bounded merge or root-scope change. Call `replace`
        /// before releasing the surrounding write lock so readers always
        /// observe a matching pair.
        pub(super) fn take_records(&mut self) -> Vec<IndexedEntry> {
            self.search_ascii_signatures.clear();
            std::mem::take(&mut self.records)
        }

        /// Body text is intentionally excluded from the filename/path
        /// signature, so content-only workers can safely clear it without
        /// rebuilding the path candidate projection. Keeping this operation
        /// narrow prevents a future content worker from mutating name/path
        /// records behind `replace`.
        pub(super) fn clear_content(&mut self) {
            for entry in &mut self.records {
                entry.content = None;
            }
        }

        /// Installs the bounded in-memory body projection by opaque entry ID.
        /// This deliberately cannot alter display names or paths.
        pub(super) fn replace_content_by_id(
            &mut self,
            documents: &mut HashMap<String, IndexedContent>,
        ) {
            for entry in &mut self.records {
                entry.content = documents.remove(&entry.id);
            }
        }

        pub(super) fn search_ascii_signatures(&self) -> &[SearchAsciiSignature] {
            &self.search_ascii_signatures
        }

        #[cfg(test)]
        pub(super) fn clear_search_ascii_signatures_for_test(&mut self) {
            self.search_ascii_signatures.clear();
        }

        #[cfg(test)]
        pub(super) fn clone_records_for_test(&self) -> Vec<IndexedEntry> {
            self.records.clone()
        }
    }

    impl std::ops::Deref for InMemorySearchSnapshot {
        type Target = Vec<IndexedEntry>;

        fn deref(&self) -> &Self::Target {
            &self.records
        }
    }
}

use in_memory_search_snapshot::InMemorySearchSnapshot;

/// Build a compact, necessary-condition signature for a single indexed entry.
/// The signature is intentionally lossy: it covers ASCII letters and digits in
/// the display name/path and, when present, their bounded pinyin aliases. A bit
/// being absent proves that a case-insensitive fuzzy match containing that
/// ASCII character is impossible; a bit being present proves nothing and still
/// proceeds to the normal matcher.
#[derive(Debug, Clone)]
struct SearchAsciiSignature {
    /// Keep names and paths separate. Each fuzzy term must be satisfiable by
    /// one complete target string; combining their character sets would retain
    /// false candidates whose letters are split between a name and a path.
    name: u64,
    path: u64,
    /// The normal scorer uses this folded value for exact/prefix/word-boundary
    /// boosts after a fuzzy hit. Building it once per entry moves a large
    /// per-keystroke allocation cost to the atomic index publication path.
    name_folded: String,
    /// Non-ASCII normalization and pinyin aliases are needed only by a subset
    /// of entries. Keeping them behind one pointer avoids adding several
    /// always-present `String` fields to every record in a 500k-entry index.
    extended: Option<Box<SearchExtendedProjection>>,
}

#[derive(Debug, Clone)]
struct SearchExtendedProjection {
    /// Canonical NFKC + lowercase form. The visible path remains untouched;
    /// this copy exists solely to make canonically equivalent queries match.
    path_folded: String,
}

impl SearchAsciiSignature {
    fn can_match_all_terms(&self, required_term_signatures: &[u64]) -> bool {
        required_term_signatures
            .iter()
            .all(|required| self.name & required == *required || self.path & required == *required)
    }

    fn name_folded(&self) -> &str {
        &self.name_folded
    }

    fn path_folded(&self) -> Option<&str> {
        self.extended
            .as_deref()
            .map(|projection| projection.path_folded.as_str())
    }
}

fn search_ascii_signature(entry: &IndexedEntry) -> SearchAsciiSignature {
    let name_folded = fold_search_text(&entry.name);
    let needs_extended_projection = !entry.name.is_ascii() || !entry.path.is_ascii();
    let extended = needs_extended_projection.then(|| {
        Box::new(SearchExtendedProjection {
            path_folded: fold_search_text(&entry.path),
        })
    });

    let mut name_signature = ascii_search_signature_for_text(&name_folded);
    let mut path_signature = ascii_search_signature_for_text(
        extended
            .as_deref()
            .map(|projection| projection.path_folded.as_str())
            .unwrap_or(&entry.path),
    );
    add_pinyin_signature(
        &entry.name,
        MAX_PINYIN_NAME_SOURCE_CHARS,
        &mut name_signature,
    );
    add_pinyin_signature(
        &entry.path,
        MAX_PINYIN_PATH_SOURCE_CHARS,
        &mut path_signature,
    );

    SearchAsciiSignature {
        name: name_signature,
        path: path_signature,
        name_folded,
        extended,
    }
}

fn pinyin_data() -> &'static PinyinData {
    PINYIN_DATA.get_or_init(|| PinyinData::new(PINYIN_NOTATIONS))
}

fn add_pinyin_signature(value: &str, max_source_chars: usize, signature: &mut u64) {
    if value.is_ascii() {
        return;
    }

    let mut characters = value.chars();
    for character in characters.by_ref().take(max_source_chars) {
        pinyin_data().get_pinyins_and_for_each(character, |pinyin| {
            if let Some(spelling) = pinyin.notation(PinyinNotation::Ascii) {
                *signature |= ascii_search_signature_for_text(spelling);
            }
        });
    }
    if characters.next().is_some() {
        *signature |= ALL_SEARCH_ASCII_SIGNATURE_BITS;
    }
}

fn ascii_search_signature_for_text(value: &str) -> u64 {
    value.bytes().fold(0_u64, |signature, byte| {
        let normalized = byte.to_ascii_lowercase();
        let bit = match normalized {
            b'a'..=b'z' => u32::from(normalized - b'a'),
            b'0'..=b'9' => SEARCH_ASCII_LETTER_COUNT + u32::from(normalized - b'0'),
            _ => return signature,
        };
        signature | (1_u64 << bit)
    })
}

fn build_search_ascii_signatures(entries: &[IndexedEntry]) -> Vec<SearchAsciiSignature> {
    entries.par_iter().map(search_ascii_signature).collect()
}

#[derive(Debug)]
enum WatchControl {
    SetRoots(Vec<PathBuf>),
}

#[derive(Debug)]
struct PendingWatchRebuild {
    first_change_at: Instant,
    deadline: Instant,
    changed_paths: HashSet<PathBuf>,
    requires_full_rebuild: bool,
}

/// Snapshot writes intentionally trail in-memory reconciliation. Serializing a
/// 500k-path JSON snapshot after every small filesystem notification would
/// make the "incremental" hot path slower than a targeted directory walk.
#[derive(Debug, Clone, Copy)]
struct PendingIncrementalSnapshot {
    first_change_at: Instant,
    deadline: Instant,
}

impl PendingIncrementalSnapshot {
    fn new(now: Instant) -> Self {
        Self {
            first_change_at: now,
            deadline: now + INCREMENTAL_SNAPSHOT_DEBOUNCE,
        }
    }

    fn record_change(&mut self, now: Instant) {
        self.deadline = (now + INCREMENTAL_SNAPSHOT_DEBOUNCE)
            .min(self.first_change_at + INCREMENTAL_SNAPSHOT_MAX_DELAY);
    }

    fn retry_after_write_failure(&mut self, now: Instant) {
        self.deadline = now + INCREMENTAL_SNAPSHOT_RETRY_DELAY;
    }

    fn is_due(&self, now: Instant) -> bool {
        now >= self.deadline
    }
}

impl PendingWatchRebuild {
    fn new(now: Instant) -> Self {
        Self {
            first_change_at: now,
            deadline: now + WATCH_DEBOUNCE,
            changed_paths: HashSet::new(),
            requires_full_rebuild: false,
        }
    }

    fn record_change(&mut self, now: Instant) {
        // Repeated writes to a build directory should normally coalesce into
        // one scan. A hard upper bound still lets long-running writes become
        // searchable instead of postponing refresh indefinitely.
        self.deadline = (now + WATCH_DEBOUNCE).min(self.first_change_at + WATCH_MAX_BATCH_DELAY);
    }

    fn retry_after_scan(&mut self, now: Instant) {
        self.deadline = now + WATCH_SCAN_RETRY_DELAY;
    }

    fn is_due(&self, now: Instant) -> bool {
        now >= self.deadline
    }

    fn require_full_rebuild(&mut self, now: Instant) {
        self.record_change(now);
        self.requires_full_rebuild = true;
        self.changed_paths.clear();
    }

    fn record_paths(&mut self, paths: impl IntoIterator<Item = PathBuf>, now: Instant) {
        self.record_change(now);
        if self.requires_full_rebuild {
            return;
        }
        self.changed_paths.extend(paths);
        if self.changed_paths.len() > MAX_INCREMENTAL_WATCH_PATHS {
            self.requires_full_rebuild = true;
            self.changed_paths.clear();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WatchedRebuildDecision {
    Started,
    DeferredWhileScanning,
    DiscardedForDifferentScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WatchedIncrementalDecision {
    Applied,
    DeferredWhileScanning,
    DiscardedForDifferentScope,
    RequiresFullRebuild,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IncrementalSnapshotDecision {
    Persisted,
    RetryAfterWriteFailure,
    SupersededByFullScan,
}

#[derive(Debug, Default)]
struct WatchRegistration {
    watched: usize,
    first_error: Option<String>,
}

/// The path source selected for one full rebuild. On Windows a direct,
/// explicitly authorised drive root may complete through read-only MFT data;
/// every narrow or failed root stays with the established scoped walker.
#[derive(Debug)]
struct FullScanCollection {
    entries: Vec<IndexedEntry>,
    /// Metadata that was successfully read for an MFT-projected path, kept
    /// with its source identity until the snapshot is finalized.  The normal
    /// index only needs `entries`; P1e uses this one-to-one pairing to avoid
    /// reverse-engineering NTFS file references from path strings.
    mft_indexed_pairs: Vec<MftIndexedEntry>,
    /// Exact direct-drive root identities and the USN cutoffs emitted after
    /// the MFT initialization window has been replayed. They are never
    /// inferred from a later general P1a checkpoint.
    mft_replay_seeds: Vec<ntfs_usn::UsnReplayVolumeSeed>,
    mft_status: &'static str,
    /// Only a completely successful direct-volume P1c enumeration can bind a
    /// snapshot to P1d/P1e restart state. Mixed, narrow, or fallback scopes
    /// deliberately persist no cross-restart proof.
    mft_snapshot_eligible: bool,
    mft_enumerated_records: usize,
    mft_replayed_usn_records: usize,
    mft_indexed_paths: usize,
    mft_message: String,
}

#[derive(Debug, Clone)]
struct MftIndexedEntry {
    entry: IndexedEntry,
    path: ntfs_usn::MftPathEntry,
}

/// Publication facts reported after a complete cross-restart P1e replay. The
/// replay payload itself is persisted atomically with the updated snapshot;
/// this small summary exists only for status text.
#[cfg(windows)]
#[derive(Debug, Clone, Copy)]
struct StartupUsnReplaySummary {
    volume_count: usize,
    replayed_records: usize,
    dirty_path_count: usize,
    dirty_file_reference_count: usize,
    indexed_paths: usize,
}

/// An in-memory local-content and application-launcher index. The content
/// scanner is deliberately independent from platform adapters so a future
/// NTFS/USN or Spotlight backend can replace it without changing the frontend
/// command surface.
#[derive(Clone, Debug)]
pub struct SearchIndex {
    inner: Arc<IndexInner>,
}

impl SearchIndex {
    #[cfg(test)]
    pub fn new() -> Self {
        Self::with_storage_paths(None, None, None)
    }

    /// Loads the last complete, locally-owned path snapshot synchronously so
    /// the launcher has useful results before its background verification scan
    /// completes. A bad or old snapshot is ignored rather than blocking app
    /// startup; the normal scan will replace it with a fresh one.
    pub fn with_storage(app_data_dir: PathBuf) -> Self {
        Self::with_storage_paths_with_legacy(
            Some(app_data_dir.join(SNAPSHOT_FILE_NAME)),
            Some(app_data_dir.join(LEGACY_SNAPSHOT_FILE_NAME)),
            Some(app_data_dir.join(ROOTS_FILE_NAME)),
            Some(app_data_dir.join(ntfs_usn::CHECKPOINT_FILE_NAME)),
        )
    }

    #[cfg(test)]
    fn with_storage_paths(
        snapshot_path: Option<PathBuf>,
        roots_path: Option<PathBuf>,
        usn_checkpoint_path: Option<PathBuf>,
    ) -> Self {
        Self::with_storage_paths_with_legacy(snapshot_path, None, roots_path, usn_checkpoint_path)
    }

    fn with_storage_paths_with_legacy(
        snapshot_path: Option<PathBuf>,
        legacy_snapshot_path: Option<PathBuf>,
        roots_path: Option<PathBuf>,
        usn_checkpoint_path: Option<PathBuf>,
    ) -> Self {
        let internal_state_roots = managed_state_roots(&[
            snapshot_path.as_deref(),
            legacy_snapshot_path.as_deref(),
            roots_path.as_deref(),
            usn_checkpoint_path.as_deref(),
        ]);
        let configured_roots = roots_path
            .as_deref()
            .map(load_persisted_roots)
            .unwrap_or(RootSelection::Default);
        let active_roots = configured_roots.active_roots();
        let active_root_names = active_roots
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        let restored_v3 = (!configured_roots.is_unavailable())
            .then(|| {
                snapshot_path.as_deref().and_then(|path| {
                    load_persisted_snapshot(path, &internal_state_roots, &active_roots)
                })
            })
            .flatten();
        // A snapshot is only safe to reuse when it was built for the active
        // root scope. In particular, changing a user-managed root must not
        // briefly expose paths from a directory the user just removed. The
        // legacy v2 path cache is a fallback only: a usable v3 snapshot always
        // wins, and v2 has no binding to offer to P1d/P1e.
        let restored = restored_v3.or_else(|| {
            (!configured_roots.is_unavailable())
                .then(|| {
                    legacy_snapshot_path.as_deref().and_then(|path| {
                        load_legacy_persisted_snapshot(path, &internal_state_roots, &active_roots)
                    })
                })
                .flatten()
        });
        // An unreadable or malformed optional binding must never make a valid
        // path snapshot unavailable. We simply decline P1d/P1e reuse and
        // retain the established complete-scan verification path.
        let startup_usn_binding = restored
            .as_ref()
            .and_then(|snapshot| snapshot.usn_binding.clone())
            .filter(|binding| snapshot_usn_binding_matches_scope(binding, &active_roots));
        let (initial_usn_status, initial_usn_message) = initial_usn_status();
        let (initial_mft_status, initial_mft_message) = initial_mft_status();
        let (entries, status) = if let Some(snapshot) = restored {
            let indexed_files = snapshot.entries.len();
            (
                snapshot.entries,
                IndexStatus {
                    indexed_files,
                    content_indexed_files: 0,
                    content_indexed_bytes: 0,
                    content_status: "idle".to_owned(),
                    roots: active_root_names.clone(),
                    phase: "ready".to_owned(),
                    last_indexed_at: Some(snapshot.last_indexed_at),
                    watch_status: "not-started".to_owned(),
                    watch_message: None,
                    usn_status: initial_usn_status.to_owned(),
                    usn_eligible_volumes: 0,
                    usn_checkpointed_volumes: 0,
                    usn_message: initial_usn_message.clone(),
                    mft_status: initial_mft_status.to_owned(),
                    mft_enumerated_records: 0,
                    mft_replayed_usn_records: 0,
                    mft_indexed_paths: 0,
                    mft_message: initial_mft_message.clone(),
                    content_message: Some(
                        "正文仅在本次运行的内存中建立；等待后台扫描。".to_owned(),
                    ),
                },
            )
        } else {
            (
                Vec::new(),
                IndexStatus {
                    indexed_files: 0,
                    content_indexed_files: 0,
                    content_indexed_bytes: 0,
                    content_status: "idle".to_owned(),
                    roots: active_root_names,
                    phase: if configured_roots.is_unavailable() {
                        "error".to_owned()
                    } else {
                        "idle".to_owned()
                    },
                    last_indexed_at: None,
                    watch_status: "not-started".to_owned(),
                    watch_message: None,
                    usn_status: initial_usn_status.to_owned(),
                    usn_eligible_volumes: 0,
                    usn_checkpointed_volumes: 0,
                    usn_message: initial_usn_message,
                    mft_status: initial_mft_status.to_owned(),
                    mft_enumerated_records: 0,
                    mft_replayed_usn_records: 0,
                    mft_indexed_paths: 0,
                    mft_message: initial_mft_message,
                    content_message: None,
                },
            )
        };
        Self {
            inner: Arc::new(IndexInner {
                entries: RwLock::new(InMemorySearchSnapshot::new(entries)),
                status: RwLock::new(status),
                generation: AtomicU64::new(0),
                content_revision: AtomicU64::new(0),
                snapshot_path,
                snapshot_lock: Mutex::new(()),
                internal_state_roots,
                startup_usn_binding: Mutex::new(startup_usn_binding),
                configured_roots: RwLock::new(configured_roots),
                roots_path,
                roots_lock: Mutex::new(()),
                #[cfg(windows)]
                usn_checkpoint_path,
                #[cfg(windows)]
                usn_checkpoint_lock: Mutex::new(()),
                watcher_control: Mutex::new(None),
                watcher_requested: AtomicBool::new(false),
            }),
        }
    }

    pub fn status(&self) -> IndexStatus {
        self.inner
            .status
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Starts a platform watcher for the already-authorized roots. The
    /// watcher never expands the index scope: every event is checked against
    /// the configured roots again before it can request a background scan.
    ///
    /// `notify` chooses the operating-system backend (ReadDirectoryChangesW
    /// on Windows and FSEvents/kqueue on macOS). The watcher reconciles
    /// bounded concrete path batches, retaining a scoped full scan for
    /// overflow/recovery. It is not a USN/FSEvents checkpoint store.
    pub fn start_change_watcher(&self) {
        self.inner.watcher_requested.store(true, Ordering::Release);
        let roots = self.active_roots();
        let mut control = self
            .inner
            .watcher_control
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if let Some(sender) = control.as_ref() {
            if sender.send(WatchControl::SetRoots(roots.clone())).is_ok() {
                return;
            }
            // The worker only exits when its native watcher cannot continue.
            // Clearing the stale sender lets a later root change retry setup
            // instead of silently pretending freshness is still enabled.
            *control = None;
        }

        let (event_sender, event_receiver) = mpsc::sync_channel(WATCH_EVENT_BUFFER);
        let event_overflow = Arc::new(AtomicBool::new(false));
        let overflow_for_callback = Arc::clone(&event_overflow);
        set_watch_status(&self.inner, "starting", None);
        let mut watcher = match recommended_watcher(move |event| {
            match event_sender.try_send(event) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    // One queued refresh is enough to converge a path-only
                    // index. Remember a saturated callback queue so the
                    // worker schedules it once it catches up.
                    overflow_for_callback.store(true, Ordering::Release);
                }
                Err(TrySendError::Disconnected(_)) => {}
            }
        }) {
            Ok(watcher) => watcher,
            Err(error) => {
                eprintln!("iHub could not create the local search change watcher: {error}");
                set_watch_status(
                    &self.inner,
                    "unavailable",
                    Some(format!("无法创建系统文件监听：{error}")),
                );
                return;
            }
        };
        let mut watched_roots = Vec::new();
        let registration = replace_watched_roots(&mut watcher, &mut watched_roots, &roots);
        update_watch_registration_status(&self.inner, &roots, registration);
        let (control_sender, control_receiver) = mpsc::channel();
        let inner = Arc::clone(&self.inner);
        match thread::Builder::new()
            .name("ihub-file-index-watcher".to_owned())
            .spawn(move || {
                run_change_watcher(
                    inner,
                    watcher,
                    roots,
                    watched_roots,
                    control_receiver,
                    event_receiver,
                    event_overflow,
                )
            }) {
            Ok(_) => {
                // The initial roots are registered before this thread starts,
                // closing the gap between startup indexing and event capture.
                *control = Some(control_sender);
            }
            Err(error) => {
                eprintln!("iHub could not start the local search watcher thread: {error}");
                set_watch_status(
                    &self.inner,
                    "unavailable",
                    Some(format!("无法启动文件监听线程：{error}")),
                );
            }
        }
    }

    pub fn rebuild_default_roots(&self) -> IndexStatus {
        let _roots_guard = self
            .inner
            .roots_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.root_configuration_is_unavailable() {
            return self.status();
        }
        let roots = self.active_roots();
        if self.try_resume_startup_snapshot(&roots) {
            return self.status();
        }
        self.rebuild_locked(roots);
        self.status()
    }

    /// Replaces the user-managed path roots. Passing an empty list resets to
    /// the conservative default folders. Every requested root is canonicalized
    /// and verified to be an existing directory before it is persisted.
    pub fn set_roots(&self, requested_roots: Vec<String>) -> Result<IndexStatus, String> {
        let roots = normalize_configured_roots(requested_roots)?;
        let _roots_guard = self
            .inner
            .roots_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // A restored P1d/P1e proof is scoped to the roots that existed at
        // process startup. A user-directed scope change is an authorization
        // boundary, so it must never be reused afterwards.
        *self
            .inner
            .startup_usn_binding
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        if let Some(path) = self.inner.roots_path.as_deref() {
            persist_roots(path, &roots)?;
        }
        *self
            .inner
            .configured_roots
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = if roots.is_empty() {
            RootSelection::Default
        } else {
            RootSelection::Custom(roots)
        };
        let active_roots = self.active_roots();
        self.discard_entries_outside_scope(&active_roots);
        self.update_change_watcher_roots(&active_roots);
        self.rebuild_locked(active_roots);
        Ok(self.status())
    }

    fn update_change_watcher_roots(&self, roots: &[PathBuf]) {
        let sender = self
            .inner
            .watcher_control
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let Some(sender) = sender else {
            // Unit-test indexes deliberately do not start native watchers.
            // Production startup calls `start_change_watcher`; a failed
            // watcher setup may be retried the next time the user changes the
            // explicitly authorized scope.
            if self.inner.watcher_requested.load(Ordering::Acquire) {
                self.start_change_watcher();
            }
            return;
        };
        if sender.send(WatchControl::SetRoots(roots.to_vec())).is_err() {
            let mut control = self
                .inner
                .watcher_control
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *control = None;
            drop(control);
            if self.inner.watcher_requested.load(Ordering::Acquire) {
                self.start_change_watcher();
            }
        }
    }

    fn active_roots(&self) -> Vec<PathBuf> {
        self.inner
            .configured_roots
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .active_roots()
    }

    fn root_configuration_is_unavailable(&self) -> bool {
        self.inner
            .configured_roots
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_unavailable()
    }

    /// A v3 snapshot may be reused only at process startup, after the native
    /// watcher has registered the exact current scope. The paired P1e binding
    /// is intentionally consumed even when verification or replay fails: a
    /// later manual refresh must perform the usual complete scan rather than
    /// retry an old proof against a moving filesystem.
    fn try_resume_startup_snapshot(&self, roots: &[PathBuf]) -> bool {
        // P1d and P1e both require an external state directory. If iHub's
        // atomic state files live under an authorised drive root, writing the
        // snapshot would advance that same Journal. Do not pretend such a
        // cache can prove a quiet cutoff; the normal full scan remains the
        // correct path for that common C: configuration.
        if !zero_change_storage_is_external(&self.inner, roots)
            || self.status().watch_status != "watching"
        {
            return false;
        }
        let binding = self
            .inner
            .startup_usn_binding
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(binding) = binding else {
            return false;
        };
        if !snapshot_usn_binding_matches_scope(&binding, roots) {
            return false;
        }

        #[cfg(windows)]
        {
            // A changed journal watermark is not automatically stale. P1e
            // may replay the complete stable-path binding; any ambiguity
            // returns to the existing zero-change/full-scan fallback below.
            let zero_change_error =
                ntfs_usn::verify_zero_change_baseline(roots, &binding.replay.checkpoints).err();
            if let Some(zero_change_error) = zero_change_error.as_deref() {
                match self.try_resume_changed_startup_snapshot(roots, &binding) {
                    Ok(summary) => {
                        set_usn_status(
                            self.inner.as_ref(),
                            "available",
                            summary.volume_count,
                            summary.volume_count,
                            Some(format!(
                                "已跨重启回放 {} 条 USN 记录并复核 {} 个明确授权的 NTFS 盘符根目录；{} 条受影响路径已重新读取，跳过本次全量扫描。",
                                summary.replayed_records,
                                summary.volume_count,
                                summary.dirty_path_count,
                            )),
                        );
                        set_mft_status(
                            self.inner.as_ref(),
                            "available",
                            0,
                            summary.replayed_records,
                            summary.indexed_paths,
                            Some(format!(
                                "P1e 已将完整快照的稳定路径绑定回放到新的静默 USN 截止点：{} 条记录、{} 条路径、{} 个文件引用；已用同一份条目和绑定原子更新本地缓存。",
                                summary.replayed_records,
                                summary.dirty_path_count,
                                summary.dirty_file_reference_count,
                            )),
                        );
                        schedule_application_entry_refresh(
                            &self.inner,
                            self.inner.generation.load(Ordering::SeqCst),
                        );
                        schedule_content_index_rebuild(
                            &self.inner,
                            self.inner.generation.load(Ordering::SeqCst),
                        );
                        return true;
                    }
                    Err(replay_error) => {
                        eprintln!(
                            "iHub declined cross-restart USN replay after the zero-change proof failed ({zero_change_error}): {replay_error}"
                        );
                    }
                }
            }
            // A failed P1e attempt never weakens the existing proof: a reset
            // Journal, truncated history, unknown topology, scope ambiguity,
            // or any other replay error falls through to the complete scan.
            if zero_change_error.is_some() {
                return false;
            }

            let indexed_files = self
                .inner
                .entries
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .len();
            let volume_count = binding.replay.checkpoints.len();
            set_usn_status(
                self.inner.as_ref(),
                "available",
                volume_count,
                volume_count,
                Some(format!(
                    "已验证 {volume_count} 个明确授权的 NTFS 盘符根目录自完整快照以来没有 USN 变化；跳过本次全量扫描。持续变化仍由文件监听处理。"
                )),
            );
            set_mft_status(
                self.inner.as_ref(),
                "available",
                0,
                0,
                0,
                Some(format!(
                    "已验证完整快照对应的 {volume_count} 个授权盘符根目录没有 USN 变化，复用 {indexed_files} 条现有路径索引；这不是跨重启 USN 增量回放。"
                )),
            );
            // Applications are discovered from OS-owned Start Menu roots,
            // outside the content-root USN proof. Refresh them separately so
            // a quiet D: index never preserves an old C: application list.
            // This worker intentionally does not persist the changed entries:
            // the old binding remains valid only for its original snapshot.
            schedule_application_entry_refresh(
                &self.inner,
                self.inner.generation.load(Ordering::SeqCst),
            );
            // Snapshot paths are ready immediately, but the bounded body
            // projection remains process-local by design and is rebuilt for
            // this process just as it is after a normal complete scan.
            schedule_content_index_rebuild(
                &self.inner,
                self.inner.generation.load(Ordering::SeqCst),
            );
            true
        }

        #[cfg(not(windows))]
        {
            let _ = (binding, roots);
            false
        }
    }

    /// Applies an exact P1e delta only while the caller owns the root lock.
    /// The native watcher has already registered the current roots, but cannot
    /// reconcile a notification until this publication has either completed
    /// or failed back to the normal complete scan.
    #[cfg(windows)]
    fn try_resume_changed_startup_snapshot(
        &self,
        roots: &[PathBuf],
        previous_binding: &PersistedUsnSnapshotBinding,
    ) -> Result<StartupUsnReplaySummary, String> {
        let current_entries = self
            .inner
            .entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if !snapshot_entries_match_replay_binding(&current_entries, previous_binding) {
            return Err("启动时内存路径快照不再与已保存的 USN 稳定路径绑定完全对应".to_owned());
        }

        let ntfs_usn::UsnReplayOutcome {
            binding: replay_binding,
            dirty_paths,
            dirty_file_references,
            replayed_records,
        } = ntfs_usn::replay_binding_to_quiet_cutoff(roots, &previous_binding.replay)
            .map_err(|error| format!("无法安全回放保存的 USN 路径绑定：{error}"))?;
        let updated_binding = PersistedUsnSnapshotBinding {
            schema_version: USN_SNAPSHOT_BINDING_SCHEMA_VERSION,
            roots: roots
                .iter()
                .map(|root| root.to_string_lossy().to_string())
                .collect(),
            replay: replay_binding,
        };
        if !snapshot_usn_binding_matches_scope(&updated_binding, roots) {
            return Err("USN 回放后的稳定路径绑定不再匹配当前授权范围".to_owned());
        }

        let mut updated_entries = reconcile_replayed_snapshot_entries(
            current_entries,
            roots,
            &self.inner.internal_state_roots,
            &updated_binding,
            &dirty_paths,
        )?;
        sort_and_deduplicate_entries(&mut updated_entries);
        let indexed_paths = updated_entries
            .iter()
            .filter(|entry| entry.kind != "application")
            .count();
        // The stable binding represents every persisted content entry. Never
        // trim a replay result and leave a partial identity projection behind.
        if indexed_paths > MAX_INDEXED_ENTRIES {
            return Err("USN 回放后的内容路径数量超过安全索引上限".to_owned());
        }
        if !snapshot_entries_match_replay_binding(&updated_entries, &updated_binding) {
            return Err("USN 回放后的路径条目无法与新的稳定路径绑定精确对应".to_owned());
        }

        let snapshot_path = self
            .inner
            .snapshot_path
            .as_deref()
            .ok_or_else(|| "没有可原子更新的本地索引快照路径".to_owned())?;
        let completed_at = now_iso();
        let indexed_files = updated_entries.len();
        let volume_count = updated_binding.replay.checkpoints.len();
        let dirty_path_count = dirty_paths.len();
        let dirty_file_reference_count = dirty_file_references.len();

        // Keep the established root-lock then snapshot-lock order. The state
        // directory was proven external before this method is called, so this
        // write cannot itself advance an authorised volume Journal.
        let _snapshot_guard = self
            .inner
            .snapshot_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !root_scopes_match(roots, &self.active_roots()) {
            return Err("USN 回放期间索引授权范围已变更".to_owned());
        }
        let entries_still_match = {
            let entries = self
                .inner
                .entries
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            snapshot_entries_match_replay_binding(&entries, previous_binding)
        };
        if !entries_still_match {
            return Err("USN 回放期间路径快照已被其他更新替换".to_owned());
        }
        // The native reader already performs an all-volume cutoff check. Run
        // it again after metadata reconciliation and immediately before the
        // atomic replacement, covering both cross-volume ordering and every
        // path read performed above.
        ntfs_usn::verify_zero_change_baseline(roots, &updated_binding.replay.checkpoints)
            .map_err(|error| format!("USN 回放快照发布前最终校验失败：{error}"))?;
        persist_snapshot(
            snapshot_path,
            roots,
            &completed_at,
            &updated_entries,
            Some(&updated_binding),
        )
        .map_err(|error| format!("无法原子保存 USN 回放后的本地索引快照：{error}"))?;

        {
            let mut entries = self
                .inner
                .entries
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            entries.replace(updated_entries);
        }
        {
            let mut status = self
                .inner
                .status
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            status.indexed_files = indexed_files;
            status.phase = "ready".to_owned();
            status.last_indexed_at = Some(completed_at);
        }

        Ok(StartupUsnReplaySummary {
            volume_count,
            replayed_records,
            dirty_path_count,
            dirty_file_reference_count,
            indexed_paths,
        })
    }

    fn rebuild_from_watched_scope(&self, watched_scope: &[PathBuf]) -> WatchedRebuildDecision {
        let _roots_guard = self
            .inner
            .roots_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let active_roots = self.active_roots();
        if !root_scopes_match(&active_roots, watched_scope) {
            return WatchedRebuildDecision::DiscardedForDifferentScope;
        }
        if self.status().phase == "scanning" {
            return WatchedRebuildDecision::DeferredWhileScanning;
        }
        self.rebuild_locked(active_roots);
        WatchedRebuildDecision::Started
    }

    /// Reconciles a bounded batch of concrete filesystem paths without
    /// rescanning every authorized root. The watcher supplies only paths that
    /// are already known to be inside its explicit scope; we nevertheless
    /// check the scope again under `roots_lock` before touching the index.
    ///
    /// A path that names an index root itself, an event overflow, an unreadable
    /// path, or an oversized batch returns `RequiresFullRebuild`. The caller
    /// then uses the existing scoped scanner rather than guessing about an
    /// incomplete filesystem change.
    fn reconcile_watched_paths(
        &self,
        watched_scope: &[PathBuf],
        changed_paths: &HashSet<PathBuf>,
    ) -> WatchedIncrementalDecision {
        let changed_paths = changed_paths
            .iter()
            .filter(|path| !path_is_in_managed_state(path, &self.inner.internal_state_roots))
            .cloned()
            .collect::<HashSet<_>>();
        if changed_paths.is_empty() {
            // iHub's own atomic snapshot/checkpoint writes are excluded from
            // the user index and must not trigger a self-refresh loop.
            return WatchedIncrementalDecision::DiscardedForDifferentScope;
        }

        let _roots_guard = self
            .inner
            .roots_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let active_roots = self.active_roots();
        if !root_scopes_match(&active_roots, watched_scope) {
            return WatchedIncrementalDecision::DiscardedForDifferentScope;
        }
        if self.status().phase == "scanning" {
            return WatchedIncrementalDecision::DeferredWhileScanning;
        }

        // A root itself being renamed, removed, or replaced changes the
        // configured scope's availability. Let the full scanner re-evaluate
        // that boundary instead of treating it as an ordinary subtree delta.
        if changed_paths.iter().any(|path| {
            active_roots
                .iter()
                .any(|root| path_is_within_root(root, path))
        }) {
            return WatchedIncrementalDecision::RequiresFullRebuild;
        }

        let changed_paths = coalesce_incremental_paths(&changed_paths, &active_roots);
        if changed_paths.is_empty() {
            return WatchedIncrementalDecision::DiscardedForDifferentScope;
        }

        let generation = self.inner.generation.load(Ordering::SeqCst);
        // Bump the body revision before publishing any replacement path. An
        // older content worker may be waiting on the entry write lock; without
        // this invalidation it could attach an old file body to the new record
        // in the small window before the later rebuild is scheduled.
        invalidate_content_index(&self.inner, "检测到路径变更；正在重建本机内存正文索引。");
        let retained_content_count = self
            .inner
            .entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|entry| {
                entry.kind != "application"
                    && !path_is_in_managed_state(
                        Path::new(&entry.path),
                        &self.inner.internal_state_roots,
                    )
                    && !changed_paths
                        .iter()
                        .any(|changed| path_is_within_root(Path::new(&entry.path), changed))
            })
            .count();

        let mut replacements = Vec::new();
        for changed_path in &changed_paths {
            let metadata = match fs::symlink_metadata(changed_path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return WatchedIncrementalDecision::RequiresFullRebuild,
            };
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                // The full scanner also skips symlinks, so remove any stale
                // path record but never turn a new link into an index escape.
                continue;
            }
            if file_type.is_dir() {
                let initial_count = retained_content_count + replacements.len();
                let mut scanned = collect_entries(
                    std::slice::from_ref(changed_path),
                    &self.inner,
                    generation,
                    initial_count,
                    false,
                );
                replacements.append(&mut scanned);
            } else if file_type.is_file() {
                if let Some(entry) = indexed_entry_from_path(changed_path, &metadata) {
                    replacements.push(entry);
                }
            }
        }

        // Starting a manual rebuild acquires the same root lock before it
        // increments generation. This final check is defensive for future
        // callers that may share the index without that path.
        if self.inner.generation.load(Ordering::SeqCst) != generation {
            return WatchedIncrementalDecision::DeferredWhileScanning;
        }

        // `collect_entries` returns a sorted subtree, but a batch can contain
        // several independent paths. Sort that bounded replacement set before
        // acquiring the shared index write lock. The existing index is already
        // sorted after every complete scan/snapshot publication.
        sort_and_deduplicate_entries(&mut replacements);

        let completed_at = now_iso();
        let indexed_count = {
            let mut entries = self
                .inner
                .entries
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // A ReadDirectoryChangesW batch is commonly one file. Re-sorting
            // every retained record made that hot path O(N log N) for an N-item
            // index. Retaining preserves the existing order, so merge it with
            // the small sorted replacement set in O(N + M) instead.
            let mut retained = entries.take_records();
            retained.retain(|entry| {
                entry.kind == "application"
                    || (!path_is_in_managed_state(
                        Path::new(&entry.path),
                        &self.inner.internal_state_roots,
                    ) && !changed_paths
                        .iter()
                        .any(|changed| path_is_within_root(Path::new(&entry.path), changed)))
            });
            let mut merged = merge_sorted_entries(retained, replacements);
            trim_to_index_limit(&mut merged);
            let indexed_count = merged.len();
            entries.replace(merged);
            indexed_count
        };

        let mut status = self
            .inner
            .status
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        status.indexed_files = indexed_count;
        status.phase = "ready".to_owned();
        status.last_indexed_at = Some(completed_at);
        drop(status);
        // A watcher batch can change a file body without changing its path.
        // Rebuild the bounded in-memory text projection separately instead of
        // leaving `content:` results stale after an incremental update.
        schedule_content_index_rebuild(&self.inner, generation);
        WatchedIncrementalDecision::Applied
    }

    /// Writes the latest already-reconciled index state after the watcher has
    /// been quiet for a short interval. The root lock is held only until the
    /// snapshot lock has been acquired and the payload has been captured. A
    /// root transition that begins after that point will write its own complete
    /// snapshot after this one; if it wins before capture, this pending write is
    /// discarded instead. That keeps a new root configuration from ever being
    /// paired with paths from its previous scope.
    fn persist_pending_incremental_snapshot(&self) -> IncrementalSnapshotDecision {
        let Some(snapshot_path) = self.inner.snapshot_path.as_deref() else {
            return IncrementalSnapshotDecision::Persisted;
        };

        let roots_guard = self
            .inner
            .roots_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.status().phase == "scanning" {
            return IncrementalSnapshotDecision::SupersededByFullScan;
        }

        let active_roots = self.active_roots();
        let generation = self.inner.generation.load(Ordering::SeqCst);
        let completed_at = now_iso();
        // Copy while holding only the read lock. JSON serialization and fsync
        // happen after this lock is released, so searches stay available and a
        // new narrow event batch does not wait for disk I/O.
        let entries = self
            .inner
            .entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        // Keep the established `roots_lock -> snapshot_lock` order. Once this
        // lock is held, a full-scan publisher cannot overwrite this write; a
        // later root change will wait, then publish a snapshot for its new
        // scope after the current write completes.
        let snapshot_guard = self
            .inner
            .snapshot_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.inner.generation.load(Ordering::SeqCst) != generation
            || self.status().phase == "scanning"
            || !root_scopes_match(&active_roots, &self.active_roots())
        {
            return IncrementalSnapshotDecision::SupersededByFullScan;
        }
        drop(roots_guard);

        // Incremental watcher reconciliation is intentionally not paired with
        // a journal cutoff. Dropping a prior fast-start binding is conservative:
        // the next process start will verify by a full scan rather than assume
        // this independently persisted mutation is journal-complete.
        let result = persist_snapshot(snapshot_path, &active_roots, &completed_at, &entries, None);
        drop(snapshot_guard);
        match result {
            Ok(()) => IncrementalSnapshotDecision::Persisted,
            Err(error) => {
                eprintln!("iHub could not persist an incremental local-index update: {error}");
                IncrementalSnapshotDecision::RetryAfterWriteFailure
            }
        }
    }

    /// A root-scope change is also a privacy boundary. Preserve launcher
    /// applications, but immediately remove old file/folder records before
    /// the background replacement scan begins. Refreshing an unchanged scope
    /// continues to keep its last full snapshot available.
    fn discard_entries_outside_scope(&self, roots: &[PathBuf]) {
        // Invalidate before touching the visible path set, so an old body
        // worker cannot attach data from a removed scope to a replacement
        // record while it was waiting for the entry write lock.
        invalidate_content_index(&self.inner, "索引范围已变更；旧正文已从内存移除。");
        let retained = {
            let mut entries = self
                .inner
                .entries
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut retained = entries.take_records();
            retained.retain(|entry| {
                entry.kind == "application"
                    || (entry_is_within_any_root(entry, roots)
                        && !path_is_in_managed_state(
                            Path::new(&entry.path),
                            &self.inner.internal_state_roots,
                        ))
            });
            let retained_count = retained.len();
            entries.replace(retained);
            retained_count
        };
        let mut status = self
            .inner
            .status
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        status.indexed_files = retained;
        // The old timestamp described a different root set, so do not present
        // it as a complete index for the newly selected scope.
        status.last_indexed_at = None;
    }

    /// Starts a scan for an already-authorized scope. Callers must hold
    /// `roots_lock` while choosing the roots and incrementing its generation.
    fn rebuild_locked(&self, roots: Vec<PathBuf>) {
        let generation = self.inner.generation.fetch_add(1, Ordering::SeqCst) + 1;
        // A full path rebuild may represent a new authorization scope. Clear
        // body text before it starts; name/path snapshot results stay usable.
        invalidate_content_index(&self.inner, "正在等待新的路径扫描后重建正文索引。");
        let root_names: Vec<String> = roots
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect();

        {
            let current_count = self
                .inner
                .entries
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .len();
            let mut status = self
                .inner
                .status
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // Keep the last complete snapshot visible while a new scan runs.
            // A refresh must not make all file results disappear merely
            // because a slow directory tree is being checked in the background.
            status.indexed_files = current_count;
            status.roots = root_names;
            status.phase = "scanning".to_owned();
        }

        // P1a validates journal continuity separately from the path scan. P1c
        // may use a bounded, read-only MFT initialization-window replay only
        // for an exact user-authorised drive root; all other roots retain the
        // scoped walker and watcher below.
        schedule_usn_checkpoint_refresh(&self.inner, roots.clone(), generation);
        begin_mft_initialization(&self.inner);

        let inner = Arc::clone(&self.inner);
        let _ = thread::Builder::new()
            .name("ihub-file-indexer".to_owned())
            .spawn(move || {
                let zero_change_storage_is_external =
                    zero_change_storage_is_external(&inner, &roots);

                let application_entries = collect_application_entries();
                if inner.generation.load(Ordering::SeqCst) != generation {
                    return;
                }

                // On the first run there is no snapshot to search yet. Publish
                // bounded real application entries straight away; after that,
                // preserve the old snapshot until the replacement is complete.
                let has_snapshot = !inner
                    .entries
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .is_empty();
                if !has_snapshot {
                    {
                        let mut indexed = inner
                            .entries
                            .write()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        indexed.replace(application_entries.clone());
                    }
                    let mut status = inner
                        .status
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if inner.generation.load(Ordering::SeqCst) == generation {
                        status.indexed_files = application_entries.len();
                    }
                }

                let FullScanCollection {
                    mut entries,
                    mft_indexed_pairs,
                    mft_replay_seeds,
                    mft_status,
                    mft_snapshot_eligible,
                    mft_enumerated_records,
                    mft_replayed_usn_records,
                    mft_indexed_paths,
                    mut mft_message,
                } = collect_full_scan_entries(
                    &roots,
                    &inner,
                    generation,
                    application_entries.len(),
                    true,
                );
                entries.extend(application_entries);
                sort_and_deduplicate_entries(&mut entries);
                if inner.generation.load(Ordering::SeqCst) != generation {
                    return;
                }

                let count = entries.len();
                let completed_at = now_iso();
                // The MFT enumerator closes its own initialization window and
                // emits the exact cutoff checkpoint that belongs to its
                // stable identity projection. Do not substitute a scan-start
                // P1e cutoff here: it would leave an unaccounted race.
                // Building failure is conservative; the useful ordinary path
                // snapshot still persists without a restart replay proof.
                let candidate_usn_binding = if mft_snapshot_eligible
                    && zero_change_storage_is_external
                {
                    match build_usn_snapshot_binding(
                        &roots,
                        &entries,
                        &mft_indexed_pairs,
                        &mft_replay_seeds,
                    ) {
                        Ok(binding) => Some(binding),
                        Err(error) => {
                            mft_message.push_str(&format!(
                                " P1e 稳定路径绑定未保存（{error}）；继续使用完整扫描与文件监听。"
                            ));
                            None
                        }
                    }
                } else {
                    None
                };
                #[cfg(not(windows))]
                let _ = &candidate_usn_binding;
                // A root-scope transition is serialized with the final
                // publication. Without this guard an older scan can pass the
                // generation check, then race a user removing its root and
                // write those now-out-of-scope paths back into memory.
                let _roots_guard = inner
                    .roots_lock
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let current_roots = inner
                    .configured_roots
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .active_roots();
                if inner.generation.load(Ordering::SeqCst) != generation
                    || !root_scopes_match(&roots, &current_roots)
                {
                    return;
                }
                if let Some(snapshot_path) = inner.snapshot_path.as_deref() {
                    // A slower, superseded scan must not overwrite a newer
                    // snapshot. The root guard above ensures this snapshot
                    // remains tied to the still-authorized scope.
                    let _snapshot_guard = inner
                        .snapshot_lock
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let current_roots = inner
                        .configured_roots
                        .read()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .active_roots();
                    if inner.generation.load(Ordering::SeqCst) != generation
                        || !root_scopes_match(&roots, &current_roots)
                    {
                        return;
                    }
                    // Verify from the MFT cutoff through this exact snapshot
                    // publication point. The state directory is required to
                    // be external above, so the atomic write below cannot
                    // advance an authorised volume's USN after the proof.
                    #[cfg(windows)]
                    let binding_for_snapshot = match candidate_usn_binding.as_ref() {
                        Some(binding) => match ntfs_usn::verify_zero_change_baseline(
                            &roots,
                            &binding.replay.checkpoints,
                        ) {
                            Ok(()) => Some(binding),
                            Err(error) => {
                                mft_message.push_str(&format!(
                                    " MFT 截止点到快照发布前出现 USN 变化（{error}）；本次仅保存普通缓存。"
                                ));
                                None
                            }
                        },
                        None => None,
                    };
                    #[cfg(not(windows))]
                    let binding_for_snapshot: Option<&PersistedUsnSnapshotBinding> = None;
                    if let Err(error) = persist_snapshot(
                        snapshot_path,
                        &roots,
                        &completed_at,
                        &entries,
                        binding_for_snapshot,
                    )
                    {
                        eprintln!("iHub could not persist the local index snapshot: {error}");
                    }
                }
                {
                    let mut indexed = inner
                        .entries
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    indexed.replace(entries);
                }
                let mut status = inner
                    .status
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                status.indexed_files = count;
                status.phase = "ready".to_owned();
                status.last_indexed_at = Some(completed_at);
                status.mft_status = mft_status.to_owned();
                status.mft_enumerated_records = mft_enumerated_records;
                status.mft_replayed_usn_records = mft_replayed_usn_records;
                status.mft_indexed_paths = mft_indexed_paths;
                status.mft_message = Some(truncate_mft_message(&mft_message));
                drop(status);
                drop(_roots_guard);
                schedule_content_index_rebuild(&inner, generation);
            });
    }

    pub fn search(&self, query: &str, requested_limit: Option<usize>) -> Vec<SearchResult> {
        let limit = requested_limit
            .unwrap_or(DEFAULT_RESULT_LIMIT)
            .clamp(1, MAX_RESULT_LIMIT);
        let parsed_query = ParsedQuery::parse(query);
        let pinyin_matchers = parsed_query.pinyin_matchers();
        let entries = self
            .inner
            .entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let required_ascii_term_signatures = parsed_query.required_ascii_term_signatures();
        // `entries` and the candidate projection share one read lock, so a
        // query never sees a same-length but stale signature vector. Keep the
        // length guard as a fail-closed defense against a future in-memory
        // invariant bug: it simply routes that query through the full scorer.
        let search_ascii_signatures = entries.search_ascii_signatures();
        let signatures_are_current = search_ascii_signatures.len() == entries.len();

        let top_matches = if parsed_query.has_scored_terms() {
            entries
                .par_iter()
                .enumerate()
                .map_init(new_search_matcher, |matcher, (position, entry)| {
                    // The signature is only a necessary condition for the
                    // regular fuzzy name/path/pinyin matcher. It has no
                    // bearing on content terms, non-ASCII text, or any query
                    // when the projection is unavailable; those continue
                    // through the identical full scoring path.
                    let can_match_ascii = !signatures_are_current
                        || required_ascii_term_signatures.is_empty()
                        || search_ascii_signatures[position]
                            .can_match_all_terms(&required_ascii_term_signatures);
                    let search_projection =
                        signatures_are_current.then(|| &search_ascii_signatures[position]);
                    can_match_ascii
                        .then(|| {
                            parsed_query
                                .score_entry_with_projection(
                                    matcher,
                                    entry,
                                    search_projection,
                                    &pinyin_matchers,
                                )
                                .map(|score| SearchMatch { entry, score })
                        })
                        .flatten()
                })
                .fold(
                    || TopMatches::new(limit),
                    |mut matches, candidate| {
                        if let Some(candidate) = candidate {
                            matches.consider(candidate);
                        }
                        matches
                    },
                )
                .reduce(|| TopMatches::new(limit), TopMatches::merge)
        } else {
            entries
                .par_iter()
                .fold(
                    || TopMatches::new(limit),
                    |mut matches, entry| {
                        if parsed_query.matches_filters(entry) {
                            matches.consider(SearchMatch { entry, score: 0.0 });
                        }
                        matches
                    },
                )
                .reduce(|| TopMatches::new(limit), TopMatches::merge)
        };

        top_matches.into_results_for_content(&parsed_query.content_terms)
    }

    /// Resolves only an exact, currently indexed, host-eligible result for a
    /// persistent launcher shortcut. This stays inside the native index so a
    /// renderer cannot create a shortcut by supplying an arbitrary path.
    pub(crate) fn resolve_launcher_shortcut_source(
        &self,
        source_id: &str,
    ) -> Option<LauncherShortcutSource> {
        if source_id.is_empty() || source_id.len() > 8 * 1024 {
            return None;
        }
        let entries = self
            .inner
            .entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = entries
            .iter()
            .find(|entry| entry.id == source_id && entry.is_launcher_shortcut_eligible())?;
        Some(LauncherShortcutSource {
            id: entry.id.clone(),
            path: PathBuf::from(&entry.path),
            name: entry.name.clone(),
            kind: entry.kind.clone(),
            metadata: entry.metadata.clone(),
        })
    }

    /// Resolves only current result IDs. The renderer cannot use this method
    /// to make the Shell worker inspect an arbitrary filesystem path.
    pub(crate) fn resolve_system_icon_sources(
        &self,
        source_ids: &[String],
    ) -> Vec<ResolvedSystemIconSource> {
        if source_ids.len() > 12
            || source_ids
                .iter()
                .any(|source_id| source_id.is_empty() || source_id.len() > 8 * 1024)
        {
            return Vec::new();
        }
        let requested = source_ids
            .iter()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let entries = self
            .inner
            .entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        entries
            .iter()
            .filter(|entry| requested.contains(entry.id.as_str()) && entry.is_valid())
            .map(|entry| ResolvedSystemIconSource {
                response_id: entry.id.clone(),
                path: PathBuf::from(&entry.path),
                kind: entry.kind.clone(),
            })
            .collect()
    }

    /// Content roots are an explicit user authorization boundary. A live
    /// shortcut target must stay inside the current root set after resolving
    /// the filesystem path; applications retain their separate OS-owned
    /// discovery boundary and are screened by their supported bundle shape.
    pub(crate) fn launcher_shortcut_path_is_authorized(
        &self,
        source: &LauncherShortcutSource,
        canonical_path: &Path,
    ) -> bool {
        if source.kind == "application" {
            return application_is_launcher_shortcut_eligible(canonical_path);
        }
        self.active_roots()
            .iter()
            .any(|root| path_is_within_root(canonical_path, root))
    }
}

/// Owns the OS watcher handle for the process lifetime. A root update first
/// removes old subscriptions and clears any pending event batch, then watches
/// the new explicit scope. Events are only a hint: the eventual scan takes
/// `roots_lock` and rechecks both the scope and generation before publishing.
fn run_change_watcher(
    inner: Arc<IndexInner>,
    mut watcher: RecommendedWatcher,
    mut configured_roots: Vec<PathBuf>,
    mut watched_roots: Vec<PathBuf>,
    control_receiver: Receiver<WatchControl>,
    event_receiver: Receiver<notify::Result<Event>>,
    event_overflow: Arc<AtomicBool>,
) {
    let mut pending_rebuild: Option<PendingWatchRebuild> = None;
    let mut pending_snapshot: Option<PendingIncrementalSnapshot> = None;

    loop {
        // Root changes are infrequent but safety-critical. Process every
        // queued control message before looking at filesystem events so an
        // old directory cannot cause a refresh for a newly removed scope.
        loop {
            match control_receiver.try_recv() {
                Ok(WatchControl::SetRoots(roots)) => {
                    configured_roots = roots;
                    let registration =
                        replace_watched_roots(&mut watcher, &mut watched_roots, &configured_roots);
                    update_watch_registration_status(&inner, &configured_roots, registration);
                    pending_rebuild = None;
                    pending_snapshot = None;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        if event_overflow.swap(false, Ordering::AcqRel) && !watched_roots.is_empty() {
            // The callback queue lost ordering/path detail, so reconcile with
            // the existing safe full-scan fallback instead of applying a
            // partial mutation.
            record_watched_full_rebuild(&mut pending_rebuild, Instant::now());
            pending_snapshot = None;
        }

        let now = Instant::now();
        if pending_rebuild
            .as_ref()
            .is_some_and(|pending| pending.is_due(now))
        {
            let mut pending = pending_rebuild
                .take()
                .expect("a due watch batch must still exist");
            let index = SearchIndex {
                inner: Arc::clone(&inner),
            };
            let deferred = if pending.requires_full_rebuild {
                // A complete scoped scan owns the next durable snapshot. Do
                // not serialize an older incremental capture while it runs.
                pending_snapshot = None;
                match index.rebuild_from_watched_scope(&configured_roots) {
                    WatchedRebuildDecision::Started
                    | WatchedRebuildDecision::DiscardedForDifferentScope => false,
                    WatchedRebuildDecision::DeferredWhileScanning => true,
                }
            } else {
                match index.reconcile_watched_paths(&configured_roots, &pending.changed_paths) {
                    WatchedIncrementalDecision::Applied => {
                        record_incremental_snapshot(&mut pending_snapshot, now);
                        false
                    }
                    WatchedIncrementalDecision::DiscardedForDifferentScope => false,
                    WatchedIncrementalDecision::DeferredWhileScanning => true,
                    WatchedIncrementalDecision::RequiresFullRebuild => {
                        pending.requires_full_rebuild = true;
                        pending.changed_paths.clear();
                        pending_snapshot = None;
                        match index.rebuild_from_watched_scope(&configured_roots) {
                            WatchedRebuildDecision::Started
                            | WatchedRebuildDecision::DiscardedForDifferentScope => false,
                            WatchedRebuildDecision::DeferredWhileScanning => true,
                        }
                    }
                }
            };
            if deferred {
                pending.retry_after_scan(now);
                pending_rebuild = Some(pending);
            }
            continue;
        }

        if pending_snapshot
            .as_ref()
            .is_some_and(|pending| pending.is_due(now))
        {
            let mut pending = pending_snapshot
                .take()
                .expect("a due incremental snapshot must still exist");
            let index = SearchIndex {
                inner: Arc::clone(&inner),
            };
            match index.persist_pending_incremental_snapshot() {
                IncrementalSnapshotDecision::Persisted
                | IncrementalSnapshotDecision::SupersededByFullScan => {}
                IncrementalSnapshotDecision::RetryAfterWriteFailure => {
                    pending.retry_after_write_failure(now);
                    pending_snapshot = Some(pending);
                }
            }
            continue;
        }

        let mut timeout = WATCH_CONTROL_POLL_INTERVAL;
        if let Some(pending) = pending_rebuild.as_ref() {
            timeout = timeout.min(pending.deadline.saturating_duration_since(Instant::now()));
        }
        if let Some(pending) = pending_snapshot.as_ref() {
            timeout = timeout.min(pending.deadline.saturating_duration_since(Instant::now()));
        }
        match event_receiver.recv_timeout(timeout) {
            Ok(Ok(event)) => {
                if watch_event_requires_refresh(&event)
                    && watch_event_affects_roots(&event, &watched_roots)
                {
                    record_watched_event(
                        &mut pending_rebuild,
                        &event,
                        &watched_roots,
                        &inner.internal_state_roots,
                        Instant::now(),
                    );
                    if pending_rebuild
                        .as_ref()
                        .is_some_and(|pending| pending.requires_full_rebuild)
                    {
                        pending_snapshot = None;
                    }
                }
            }
            Ok(Err(error)) => {
                // Backends surface overflows and watch failures as errors. A
                // full scan of the same roots is the safe recovery; it never
                // broadens to a default directory.
                eprintln!("iHub local search watcher reported an event error: {error}");
                if !watched_roots.is_empty() {
                    record_watched_full_rebuild(&mut pending_rebuild, Instant::now());
                    pending_snapshot = None;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn replace_watched_roots(
    watcher: &mut RecommendedWatcher,
    watched_roots: &mut Vec<PathBuf>,
    configured_roots: &[PathBuf],
) -> WatchRegistration {
    let mut registration = WatchRegistration::default();
    for old_root in watched_roots.drain(..) {
        // A removable root can disappear before its unwatch call. The path
        // filter below is still authoritative, so this is diagnostic only.
        if let Err(error) = watcher.unwatch(&old_root) {
            eprintln!(
                "iHub could not stop watching local search root '{}': {error}",
                old_root.display()
            );
        }
    }

    for root in unique_paths(configured_roots.to_vec()) {
        if !root.is_dir() {
            let message = format!(
                "索引目录 '{}' 当前不可用；请重新选择或恢复该目录。",
                root.display()
            );
            eprintln!("iHub local search {message}");
            registration.first_error.get_or_insert(message);
            continue;
        }
        match watcher.watch(&root, RecursiveMode::Recursive) {
            Ok(()) => {
                watched_roots.push(root);
                registration.watched += 1;
            }
            Err(error) => {
                let message = format!("无法监听索引目录 '{}': {error}", root.display());
                eprintln!("iHub local search {message}");
                registration.first_error.get_or_insert(message);
            }
        }
    }
    registration
}

fn update_watch_registration_status(
    inner: &IndexInner,
    configured_roots: &[PathBuf],
    registration: WatchRegistration,
) {
    if configured_roots.is_empty() {
        set_watch_status(inner, "inactive", None);
    } else if registration.watched == configured_roots.len() {
        set_watch_status(inner, "watching", None);
    } else if registration.watched > 0 {
        set_watch_status(
            inner,
            "degraded",
            registration
                .first_error
                .or_else(|| Some("部分已授权目录无法建立文件监听；可手动重新扫描。".to_owned())),
        );
    } else {
        set_watch_status(
            inner,
            "unavailable",
            registration
                .first_error
                .or_else(|| Some("没有已授权目录可建立文件监听；可手动重新扫描。".to_owned())),
        );
    }
}

fn set_watch_status(inner: &IndexInner, watch_status: &str, watch_message: Option<String>) {
    let mut status = inner
        .status
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    status.watch_status = watch_status.to_owned();
    status.watch_message = watch_message.map(|message| truncate_watch_message(&message));
}

fn initial_usn_status() -> (&'static str, Option<String>) {
    #[cfg(windows)]
    {
        ("not-started", None)
    }
    #[cfg(not(windows))]
    {
        (
            "unsupported",
            Some("当前平台不使用 NTFS USN；本地搜索继续使用目录扫描和文件监听。".to_owned()),
        )
    }
}

fn initial_mft_status() -> (&'static str, Option<String>) {
    #[cfg(windows)]
    {
        ("not-started", None)
    }
    #[cfg(not(windows))]
    {
        (
            "unsupported",
            Some("当前平台不使用 NTFS MFT 初始化；本地搜索继续使用目录扫描和文件监听。".to_owned()),
        )
    }
}

fn set_usn_status(
    inner: &IndexInner,
    usn_status: &str,
    eligible_volumes: usize,
    checkpointed_volumes: usize,
    usn_message: Option<String>,
) {
    let mut status = inner
        .status
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    status.usn_status = usn_status.to_owned();
    status.usn_eligible_volumes = eligible_volumes;
    status.usn_checkpointed_volumes = checkpointed_volumes;
    status.usn_message = usn_message.map(|message| truncate_usn_message(&message));
}

fn set_mft_status(
    inner: &IndexInner,
    mft_status: &str,
    enumerated_records: usize,
    replayed_usn_records: usize,
    indexed_paths: usize,
    mft_message: Option<String>,
) {
    let mut status = inner
        .status
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    status.mft_status = mft_status.to_owned();
    status.mft_enumerated_records = enumerated_records;
    status.mft_replayed_usn_records = replayed_usn_records;
    status.mft_indexed_paths = indexed_paths;
    status.mft_message = mft_message.map(|message| truncate_mft_message(&message));
}

fn begin_mft_initialization(inner: &Arc<IndexInner>) {
    #[cfg(windows)]
    set_mft_status(
        inner.as_ref(),
        "scanning",
        0,
        0,
        0,
        Some(
            "正在判断是否有被明确授权的盘符根目录可使用只读 MFT 初始化；窄目录不会扩大为全卷读取。"
                .to_owned(),
        ),
    );

    #[cfg(not(windows))]
    set_mft_status(
        inner.as_ref(),
        "unsupported",
        0,
        0,
        0,
        Some("当前平台不使用 NTFS MFT 初始化；继续使用目录扫描和文件监听。".to_owned()),
    );
}

/// Begins a Windows P1a journal probe without itself changing the path source.
/// P1c MFT initialization is selected independently for an exact authorised
/// drive root. Its Journal read only closes that one initialization window;
/// the scoped walker and ReadDirectoryChangesW-backed watcher still own
/// narrow roots, continuous updates and cross-restart recovery.
fn schedule_usn_checkpoint_refresh(inner: &Arc<IndexInner>, roots: Vec<PathBuf>, generation: u64) {
    #[cfg(windows)]
    {
        let Some(checkpoint_path) = inner.usn_checkpoint_path.clone() else {
            // `SearchIndex::new()` is used by unit tests without app storage.
            // Do not query the host volume when there is nowhere to persist a
            // baseline that can be validated on the next run.
            set_usn_status(
                inner.as_ref(),
                "inactive",
                0,
                0,
                Some("USN P1a 未配置持久化状态目录；继续使用目录扫描和文件监听。".to_owned()),
            );
            return;
        };
        if roots.is_empty() {
            set_usn_status(
                inner.as_ref(),
                "inactive",
                0,
                0,
                Some("没有已授权的本地搜索目录；未查询 USN Journal。".to_owned()),
            );
            return;
        }

        set_usn_status(
            inner.as_ref(),
            "probing",
            0,
            0,
            Some("正在验证已授权 NTFS 卷的 USN Journal 水位；目录搜索不受影响。".to_owned()),
        );
        let worker_inner = Arc::clone(inner);
        if let Err(error) = thread::Builder::new()
            .name("ihub-ntfs-usn-probe".to_owned())
            .spawn(move || {
                refresh_usn_checkpoints(worker_inner, roots, generation, checkpoint_path)
            })
        {
            set_usn_status(
                inner.as_ref(),
                "fallback",
                0,
                0,
                Some(format!(
                    "无法启动 NTFS USN 检查：{error}；继续使用目录扫描和文件监听。"
                )),
            );
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (roots, generation);
        set_usn_status(
            inner.as_ref(),
            "unsupported",
            0,
            0,
            Some("当前平台不使用 NTFS USN；继续使用目录扫描和文件监听。".to_owned()),
        );
    }
}

#[cfg(windows)]
fn refresh_usn_checkpoints(
    inner: Arc<IndexInner>,
    roots: Vec<PathBuf>,
    generation: u64,
    checkpoint_path: PathBuf,
) {
    let loaded = {
        let _checkpoint_guard = inner
            .usn_checkpoint_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ntfs_usn::load_checkpoints(&checkpoint_path)
    };
    let outcome =
        ntfs_usn::probe_authorized_roots(&roots, &loaded.checkpoints, loaded.warning.as_deref());

    // A root transition increments generation while holding this lock. Do not
    // publish a probe or checkpoint for a scope that has since been removed.
    let _roots_guard = inner
        .roots_lock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let current_roots = inner
        .configured_roots
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .active_roots();
    if inner.generation.load(Ordering::SeqCst) != generation
        || !root_scopes_match(&roots, &current_roots)
    {
        return;
    }

    let persist_result = {
        let _checkpoint_guard = inner
            .usn_checkpoint_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if inner.generation.load(Ordering::SeqCst) != generation
            || !root_scopes_match(&roots, &current_roots)
        {
            return;
        }
        ntfs_usn::encode_checkpoints(&outcome.checkpoints)
            .and_then(|bytes| replace_file_atomically(&checkpoint_path, &bytes, "NTFS USN 检查点"))
    };

    match persist_result {
        Ok(()) => set_usn_status(
            inner.as_ref(),
            outcome.status,
            outcome.eligible_volumes,
            outcome.checkpointed_volumes,
            Some(outcome.message),
        ),
        Err(error) => {
            let status = if outcome.eligible_volumes > 0 {
                "degraded"
            } else {
                outcome.status
            };
            set_usn_status(
                inner.as_ref(),
                status,
                outcome.eligible_volumes,
                outcome.checkpointed_volumes,
                Some(format!(
                    "{} 无法保存 USN 检查点：{error}；继续使用目录扫描和文件监听。",
                    outcome.message
                )),
            );
        }
    }
}

/// Publishes content-index status only while the worker that produced it is
/// still current. Keeping the revision check inside the status write lock
/// prevents an older worker from overwriting a newer `indexing`/`stale` state
/// after its entries have already been discarded.
fn set_content_status_if_current(
    inner: &IndexInner,
    expected_generation: Option<u64>,
    expected_revision: u64,
    content_status: &str,
    indexed_files: usize,
    indexed_bytes: usize,
    content_message: Option<String>,
) -> bool {
    let mut status = inner
        .status
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if expected_generation
        .is_some_and(|generation| inner.generation.load(Ordering::SeqCst) != generation)
        || inner.content_revision.load(Ordering::SeqCst) != expected_revision
    {
        return false;
    }
    status.content_status = content_status.to_owned();
    status.content_indexed_files = indexed_files;
    status.content_indexed_bytes = indexed_bytes;
    status.content_message = content_message;
    true
}

/// Text bodies are a different privacy domain from file names and paths. When
/// the authorized root scope changes, discard the process-local copies before
/// the replacement walk begins. Their revision also prevents a worker for the
/// old scope from publishing after a later root change.
fn invalidate_content_index(inner: &Arc<IndexInner>, message: &str) {
    let revision = inner.content_revision.fetch_add(1, Ordering::SeqCst) + 1;
    {
        let mut entries = inner
            .entries
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        entries.clear_content();
    }
    let _ = set_content_status_if_current(
        inner,
        None,
        revision,
        "stale",
        0,
        0,
        Some(message.to_owned()),
    );
}

/// Starts a separately bounded, in-memory body scan after a path index has
/// been published. File name/path search is therefore ready first, and a
/// failed body read never makes the normal launcher index unavailable.
fn schedule_content_index_rebuild(inner: &Arc<IndexInner>, generation: u64) {
    let revision = inner.content_revision.fetch_add(1, Ordering::SeqCst) + 1;
    let _ = set_content_status_if_current(
        inner,
        Some(generation),
        revision,
        "indexing",
        0,
        0,
        Some("正在建立本机内存正文索引；文件名搜索不受影响。".to_owned()),
    );

    let worker_inner = Arc::clone(inner);
    if let Err(error) = thread::Builder::new()
        .name("ihub-content-indexer".to_owned())
        .spawn(move || rebuild_content_index(worker_inner, generation, revision))
    {
        let _ = set_content_status_if_current(
            inner,
            Some(generation),
            revision,
            "stale",
            0,
            0,
            Some(format!("无法启动正文索引：{error}")),
        );
    }
}

/// A P1d/P1e proof covers only explicitly-authorised content roots. Start
/// Menu applications are a separate OS-owned source, so refresh them after a
/// resumed content snapshot without changing that snapshot or its binding on
/// disk.
#[cfg(windows)]
fn schedule_application_entry_refresh(inner: &Arc<IndexInner>, generation: u64) {
    let worker_inner = Arc::clone(inner);
    if let Err(error) = thread::Builder::new()
        .name("ihub-application-refresh".to_owned())
        .spawn(move || refresh_application_entries(worker_inner, generation))
    {
        eprintln!("iHub could not start the application refresh worker: {error}");
    }
}

#[cfg(windows)]
fn refresh_application_entries(inner: Arc<IndexInner>, generation: u64) {
    let applications = collect_application_entries();
    if inner.generation.load(Ordering::SeqCst) != generation {
        return;
    }

    let indexed_files = {
        let mut entries = inner
            .entries
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if inner.generation.load(Ordering::SeqCst) != generation {
            return;
        }
        let mut refreshed = entries.take_records();
        refreshed.retain(|entry| entry.kind != "application");
        refreshed.extend(applications);
        sort_and_deduplicate_entries(&mut refreshed);
        let indexed_count = refreshed.len();
        entries.replace(refreshed);
        indexed_count
    };

    let mut status = inner
        .status
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if inner.generation.load(Ordering::SeqCst) == generation {
        status.indexed_files = indexed_files;
    }
}

fn rebuild_content_index(inner: Arc<IndexInner>, generation: u64, revision: u64) {
    let candidates = {
        let entries = inner
            .entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        entries
            .iter()
            .filter(|entry| content_candidate_is_supported(entry))
            .take(MAX_CONTENT_INDEXED_FILES)
            .map(|entry| ContentCandidate {
                id: entry.id.clone(),
                path: PathBuf::from(&entry.path),
            })
            .collect::<Vec<_>>()
    };

    let mut documents = HashMap::<String, IndexedContent>::with_capacity(candidates.len());
    let mut indexed_bytes = 0_usize;
    for candidate in candidates {
        if inner.generation.load(Ordering::SeqCst) != generation
            || inner.content_revision.load(Ordering::SeqCst) != revision
        {
            return;
        }
        let Some(content) = read_indexed_content(&candidate.path) else {
            continue;
        };
        let next_bytes = indexed_bytes.saturating_add(content.memory_bytes);
        if next_bytes > MAX_CONTENT_INDEX_BYTES {
            break;
        }
        indexed_bytes = next_bytes;
        documents.insert(candidate.id, content);
    }

    if inner.generation.load(Ordering::SeqCst) != generation
        || inner.content_revision.load(Ordering::SeqCst) != revision
    {
        return;
    }

    let indexed_files = documents.len();
    {
        let mut entries = inner
            .entries
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if inner.generation.load(Ordering::SeqCst) != generation
            || inner.content_revision.load(Ordering::SeqCst) != revision
        {
            return;
        }
        entries.replace_content_by_id(&mut documents);
    }

    let message = if indexed_files == 0 {
        "没有符合范围的 UTF-8 文本文件；`content:` 只检索本次运行的受限内存索引。".to_owned()
    } else {
        format!(
            "正文只保留在本次运行的内存中：{indexed_files} 个文件，{}。",
            human_size(indexed_bytes as u64)
        )
    };
    let _ = set_content_status_if_current(
        inner.as_ref(),
        Some(generation),
        revision,
        "ready",
        indexed_files,
        indexed_bytes,
        Some(message),
    );
}

fn content_candidate_is_supported(entry: &IndexedEntry) -> bool {
    if entry.kind != "file"
        || entry.size_bytes == 0
        || entry.size_bytes > MAX_CONTENT_SOURCE_FILE_BYTES
    {
        return false;
    }
    let extension = entry
        .extension
        .as_deref()
        .or_else(|| {
            Path::new(&entry.path)
                .extension()
                .and_then(|value| value.to_str())
        })
        .map(|value| value.trim_start_matches('.').to_ascii_lowercase());
    extension.as_deref().is_some_and(is_content_extension)
}

fn is_content_extension(extension: &str) -> bool {
    matches!(
        extension,
        "txt"
            | "md"
            | "markdown"
            | "rst"
            | "log"
            | "csv"
            | "tsv"
            | "json"
            | "jsonc"
            | "yaml"
            | "yml"
            | "toml"
            | "ini"
            | "conf"
            | "xml"
            | "html"
            | "htm"
            | "css"
            | "js"
            | "jsx"
            | "ts"
            | "tsx"
            | "vue"
            | "svelte"
            | "rs"
            | "py"
            | "go"
            | "java"
            | "c"
            | "cc"
            | "cpp"
            | "h"
            | "hpp"
            | "cs"
            | "php"
            | "rb"
            | "swift"
            | "kt"
            | "kts"
            | "sh"
            | "zsh"
            | "fish"
            | "ps1"
            | "sql"
    )
}

fn read_indexed_content(path: &Path) -> Option<IndexedContent> {
    let mut file = fs::File::open(path).ok()?;
    let mut bytes = Vec::with_capacity(MAX_CONTENT_BYTES_PER_FILE);
    file.by_ref()
        .take(MAX_CONTENT_BYTES_PER_FILE as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    let decoded = decode_indexed_text(&bytes)?;
    let text = compact_indexed_text(&decoded);
    if text.is_empty() {
        return None;
    }
    let folded = fold_search_text(&text);
    let memory_bytes = text.len().saturating_add(folded.len());
    Some(IndexedContent {
        text,
        folded,
        memory_bytes,
    })
}

fn decode_indexed_text(bytes: &[u8]) -> Option<String> {
    if bytes.starts_with(&[0xff, 0xfe]) {
        let words = bytes[2..]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]));
        return char::decode_utf16(words)
            .collect::<Result<String, _>>()
            .ok();
    }
    if bytes.starts_with(&[0xfe, 0xff]) {
        let words = bytes[2..]
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]));
        return char::decode_utf16(words)
            .collect::<Result<String, _>>()
            .ok();
    }
    // NUL is a strong binary signal for an otherwise UTF-8 text candidate.
    if bytes.contains(&0) {
        return None;
    }
    String::from_utf8(bytes.to_vec()).ok()
}

fn compact_indexed_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn content_preview(value: &str) -> String {
    let mut preview = value
        .chars()
        .take(CONTENT_RESULT_PREVIEW_CHARS)
        .collect::<String>();
    if value.chars().count() > CONTENT_RESULT_PREVIEW_CHARS {
        preview.push('…');
    }
    preview
}

fn truncate_watch_message(message: &str) -> String {
    const MAX_WATCH_STATUS_CHARS: usize = 240;
    if message.chars().count() <= MAX_WATCH_STATUS_CHARS {
        return message.to_owned();
    }
    let shortened = message
        .chars()
        .take(MAX_WATCH_STATUS_CHARS.saturating_sub(1))
        .collect::<String>();
    format!("{shortened}…")
}

fn truncate_usn_message(message: &str) -> String {
    const MAX_USN_STATUS_CHARS: usize = 420;
    if message.chars().count() <= MAX_USN_STATUS_CHARS {
        return message.to_owned();
    }
    let shortened = message
        .chars()
        .take(MAX_USN_STATUS_CHARS.saturating_sub(1))
        .collect::<String>();
    format!("{shortened}…")
}

fn truncate_mft_message(message: &str) -> String {
    const MAX_MFT_STATUS_CHARS: usize = 520;
    if message.chars().count() <= MAX_MFT_STATUS_CHARS {
        return message.to_owned();
    }
    let shortened = message
        .chars()
        .take(MAX_MFT_STATUS_CHARS.saturating_sub(1))
        .collect::<String>();
    format!("{shortened}…")
}

fn record_watched_full_rebuild(pending: &mut Option<PendingWatchRebuild>, now: Instant) {
    let pending = pending.get_or_insert_with(|| PendingWatchRebuild::new(now));
    pending.require_full_rebuild(now);
}

fn record_incremental_snapshot(pending: &mut Option<PendingIncrementalSnapshot>, now: Instant) {
    let pending = pending.get_or_insert_with(|| PendingIncrementalSnapshot::new(now));
    pending.record_change(now);
}

fn record_watched_event(
    pending: &mut Option<PendingWatchRebuild>,
    event: &Event,
    roots: &[PathBuf],
    managed_state_roots: &[PathBuf],
    now: Instant,
) {
    if event.need_rescan() || event.paths.is_empty() {
        record_watched_full_rebuild(pending, now);
        return;
    }

    let mut changed_paths = Vec::new();
    for path in &event.paths {
        if path_is_in_managed_state(path, managed_state_roots) {
            continue;
        }
        let path_is_root_or_ancestor = roots.iter().any(|root| path_is_within_root(root, path));
        if path_is_root_or_ancestor {
            record_watched_full_rebuild(pending, now);
            return;
        }
        if roots.iter().any(|root| path_is_within_root(path, root)) {
            changed_paths.push(path.clone());
        }
    }
    if changed_paths.is_empty() {
        return;
    }

    let pending = pending.get_or_insert_with(|| PendingWatchRebuild::new(now));
    pending.record_paths(changed_paths, now);
}

fn watch_event_requires_refresh(event: &Event) -> bool {
    // File opens/reads can be very frequent and do not alter the indexed
    // name/path/metadata projection. A backend's explicit rescan hint wins
    // even if it happens to carry an access kind, because it means some
    // preceding events may have been lost. All other non-access kinds,
    // including generic `Other`, safely request scoped reconciliation.
    event.need_rescan() || !matches!(event.kind, EventKind::Access(_))
}

fn watch_event_affects_roots(event: &Event, roots: &[PathBuf]) -> bool {
    if roots.is_empty() {
        return false;
    }
    // Some backends report a pathless event for an overflow. It cannot prove
    // a specific file changed, but rescanning only the existing authorized
    // roots is a conservative recovery.
    if event.paths.is_empty() {
        return true;
    }
    event.paths.iter().any(|path| {
        roots.iter().any(|root| {
            // `notify` supplies absolute paths for the native backends. If a
            // backend ever provides a relative path, both comparisons fail
            // closed instead of treating the process CWD as an index root.
            path_is_within_root(path, root) || path_is_within_root(root, path)
        })
    })
}

/// A directory change subsumes all of its descendants. Keeping only the
/// shallowest paths avoids repeatedly walking a whole subtree when a backend
/// reports both the directory rename/create and its individual children.
fn coalesce_incremental_paths(changed_paths: &HashSet<PathBuf>, roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = changed_paths
        .iter()
        .filter(|path| roots.iter().any(|root| path_is_within_root(path, root)))
        .cloned()
        .collect::<Vec<_>>();
    paths.sort_unstable_by(|left, right| {
        left.components()
            .count()
            .cmp(&right.components().count())
            .then_with(|| root_scope_key(left).cmp(&root_scope_key(right)))
    });

    let mut coalesced: Vec<PathBuf> = Vec::with_capacity(paths.len());
    for path in paths {
        if coalesced
            .iter()
            .any(|ancestor| path_is_within_root(&path, ancestor))
        {
            continue;
        }
        coalesced.push(path);
    }
    coalesced
}

/// The P0 grammar intentionally stays local and data-only. Unknown fields
/// remain ordinary fuzzy text instead of being sent to a database or plugin.
/// That keeps `path:`, `ext:`, `kind:`, `modified:` and `size:` predictable
/// even when a user pastes a filename containing a colon.
#[derive(Debug, Default)]
struct ParsedQuery {
    positive_terms: Vec<QueryTerm>,
    /// Explicit body terms. They never silently turn an ordinary filename
    /// query into a full-text scan; use `content:meeting` or
    /// `content:"project plan"` when file text is intended.
    content_terms: Vec<QueryTerm>,
    negative_terms: Vec<String>,
    path_filters: Vec<String>,
    extensions: Vec<String>,
    kinds: Vec<String>,
    modified_after: Option<DateTime<Utc>>,
    size_filters: Vec<SizeFilter>,
}

/// Keep the display/query spelling used by the fuzzy matcher while folding it
/// once during parsing for exact-prefix ranking. Re-folding each term for every
/// candidate dominates allocation time for high-volume path queries.
#[derive(Debug)]
struct QueryTerm {
    text: String,
    folded: String,
    can_match_pinyin: bool,
}

struct PinyinTermMatchers<'a> {
    full: PinyinMatcher<'a>,
    initials: PinyinMatcher<'a>,
}

impl QueryTerm {
    fn new(text: String) -> Self {
        let folded = fold_search_text(&text);
        Self {
            can_match_pinyin: folded.is_ascii()
                && folded.bytes().any(|byte| byte.is_ascii_alphabetic()),
            folded,
            text,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SizeFilter {
    comparison: SizeComparison,
    bytes: u64,
}

#[derive(Debug, Clone, Copy)]
enum SizeComparison {
    LessThan,
    LessOrEqual,
    Equal,
    GreaterOrEqual,
    GreaterThan,
}

impl ParsedQuery {
    fn parse(input: &str) -> Self {
        let mut parsed = Self::default();
        for raw_token in tokenize_query(input) {
            if raw_token.is_empty() {
                continue;
            }

            let (is_negative, token) = raw_token
                .strip_prefix('-')
                .map(|value| (true, value))
                .unwrap_or((false, raw_token.as_str()));
            if token.is_empty() {
                continue;
            }

            // Exclusion is deliberately text-only for now. A negative
            // `path:` is still usable as literal text rather than quietly
            // inventing filter semantics that the UI cannot explain yet.
            if !is_negative {
                if let Some((field, value)) = token.split_once(':') {
                    let value = value.trim();
                    if !value.is_empty() && parsed.apply_filter(field, value) {
                        continue;
                    }
                }
            }

            if is_negative {
                parsed.negative_terms.push(fold_search_text(token));
            } else {
                parsed.positive_terms.push(QueryTerm::new(token.to_owned()));
            }
        }
        parsed
    }

    fn apply_filter(&mut self, field: &str, value: &str) -> bool {
        match field.to_ascii_lowercase().as_str() {
            "path" | "in" => {
                self.path_filters.push(fold_search_text(value));
                true
            }
            "content" | "body" => {
                let folded = fold_search_text(value);
                if folded.is_empty() {
                    false
                } else {
                    self.content_terms.push(QueryTerm {
                        text: value.to_owned(),
                        folded,
                        can_match_pinyin: false,
                    });
                    true
                }
            }
            "ext" => {
                let values = split_filter_values(value)
                    .map(|extension| fold_search_text(extension.trim_start_matches('.')))
                    .filter(|extension| !extension.is_empty())
                    .collect::<Vec<_>>();
                if values.is_empty() {
                    false
                } else {
                    self.extensions.extend(values);
                    true
                }
            }
            "kind" => {
                let values = split_filter_values(value)
                    .map(|kind| match kind.to_ascii_lowercase().as_str() {
                        "app" => "application".to_owned(),
                        value => value.to_owned(),
                    })
                    .filter(|kind| matches!(kind.as_str(), "file" | "folder" | "application"))
                    .collect::<Vec<_>>();
                if values.is_empty() {
                    false
                } else {
                    self.kinds.extend(values);
                    true
                }
            }
            "type"
                if value.eq_ignore_ascii_case("app")
                    || value.eq_ignore_ascii_case("application") =>
            {
                self.kinds.push("application".to_owned());
                true
            }
            "modified" => {
                let Some(after) = parse_modified_after(value) else {
                    return false;
                };
                self.modified_after = Some(
                    self.modified_after
                        .map(|current| current.max(after))
                        .unwrap_or(after),
                );
                true
            }
            "size" => {
                let Some(filter) = parse_size_filter(value) else {
                    return false;
                };
                self.size_filters.push(filter);
                true
            }
            _ => false,
        }
    }

    fn has_scored_terms(&self) -> bool {
        !self.positive_terms.is_empty() || !self.content_terms.is_empty()
    }

    /// A nonzero result is a safe, deliberately weak prefilter for ordinary
    /// fuzzy terms. Each positive term must still match either the name or
    /// path in the normal scorer, so every ASCII letter/digit it contains must
    /// occur in one whole target string's signature. Content terms are
    /// excluded because their text lives in a separate in-memory projection
    /// rather than either filename/path signature.
    fn required_ascii_term_signatures(&self) -> Vec<u64> {
        let mut signatures = self
            .positive_terms
            .iter()
            .map(|term| ascii_search_signature_for_text(&term.text))
            .filter(|signature| *signature != 0)
            .collect::<Vec<_>>();
        signatures.sort_unstable();
        signatures.dedup();
        signatures
    }

    fn pinyin_matchers(&self) -> Vec<Option<PinyinTermMatchers<'_>>> {
        self.positive_terms
            .iter()
            .map(|term| {
                term.can_match_pinyin.then(|| PinyinTermMatchers {
                    full: PinyinMatcher::builder(term.folded.as_str())
                        .pinyin_data(pinyin_data())
                        .pinyin_notations(PinyinNotation::Ascii)
                        .is_pattern_partial(true)
                        .build(),
                    initials: PinyinMatcher::builder(term.folded.as_str())
                        .pinyin_data(pinyin_data())
                        .pinyin_notations(PinyinNotation::AsciiFirstLetter)
                        .is_pattern_partial(true)
                        .build(),
                })
            })
            .collect()
    }

    #[cfg(test)]
    fn score_entry(&self, matcher: &mut SkimMatcherV2, entry: &IndexedEntry) -> Option<f64> {
        let pinyin_matchers = self.pinyin_matchers();
        self.score_entry_with_projection(matcher, entry, None, &pinyin_matchers)
    }

    /// The optional projection belongs to the same atomically published
    /// in-memory entry snapshot. It supplies canonical name/path forms and
    /// bounded pinyin aliases without changing the persisted entry or the
    /// visible result. Direct unit callers retain the previous lazy folding
    /// behavior by passing no projection.
    fn score_entry_with_projection(
        &self,
        matcher: &mut SkimMatcherV2,
        entry: &IndexedEntry,
        projection: Option<&SearchAsciiSignature>,
        pinyin_matchers: &[Option<PinyinTermMatchers<'_>>],
    ) -> Option<f64> {
        if !self.matches_metadata_filters(entry) {
            return None;
        }

        let content_score = self.score_content(entry)?;

        // A normal query borrows the pre-folded name from the published
        // projection. Direct unit callers retain the old lazy behavior: when
        // a name has not matched any term, no lowercase string is allocated.
        let cached_name_folded = projection.map(SearchAsciiSignature::name_folded);
        let dynamically_folded_name = (projection.is_none() && !self.negative_terms.is_empty())
            .then(|| fold_search_text(&entry.name));
        let name_folded = cached_name_folded.or(dynamically_folded_name.as_deref());
        let cached_path_folded = projection.and_then(SearchAsciiSignature::path_folded);
        let dynamically_folded_path = (self.requires_folded_path() && cached_path_folded.is_none())
            .then(|| fold_search_text(&entry.path));
        let path_folded = cached_path_folded.or(dynamically_folded_path.as_deref());
        if !self.matches_text_filters(name_folded, path_folded) {
            return None;
        }

        let mut score = content_score;
        for (term_index, term) in self.positive_terms.iter().enumerate() {
            // Filename matches are both the expected launcher behavior and
            // the common fast path. Only search the usually much longer full
            // path when that term cannot be satisfied by the visible name.
            // This keeps folder/path discovery available for multi-term
            // searches while preventing a deep path's separator bonuses from
            // outranking a direct filename match.
            let name_target = cached_name_folded.unwrap_or(&entry.name);
            let path_target = cached_path_folded.unwrap_or(&entry.path);
            let term_target = if projection.is_some() {
                term.folded.as_str()
            } else {
                term.text.as_str()
            };
            let term_score = match matcher.fuzzy_match(name_target, term_target) {
                Some(name_score) => name_score as f64,
                None => match matcher.fuzzy_match(path_target, term_target) {
                    Some(path_score) => path_score as f64 * PATH_ONLY_SCORE_WEIGHT,
                    None if term.can_match_pinyin => score_pinyin_term(
                        entry,
                        pinyin_matchers.get(term_index).and_then(Option::as_ref),
                    )?,
                    None => return None,
                },
            };
            score += term_score;
        }

        let fallback_name_folded = name_folded.is_none().then(|| fold_search_text(&entry.name));
        let name_folded = name_folded
            .or(fallback_name_folded.as_deref())
            .expect("a matching entry must have a folded display name");
        for term in &self.positive_terms {
            score += name_match_boost(name_folded, &entry.kind, &term.folded);
        }
        Some(score)
    }

    fn matches_filters(&self, entry: &IndexedEntry) -> bool {
        if !self.matches_metadata_filters(entry) {
            return false;
        }
        if self.score_content(entry).is_none() {
            return false;
        }
        let name_folded = (!self.negative_terms.is_empty()).then(|| fold_search_text(&entry.name));
        let path_folded = self
            .requires_folded_path()
            .then(|| fold_search_text(&entry.path));
        self.matches_text_filters(name_folded.as_deref(), path_folded.as_deref())
    }

    /// Returns a neutral zero for path-only queries, and a stable relevance
    /// score when every explicit `content:` phrase is present. The body is
    /// pre-folded during the background index build so this fast path never
    /// lowercases a document on the keystroke thread.
    fn score_content(&self, entry: &IndexedEntry) -> Option<f64> {
        if self.content_terms.is_empty() {
            return Some(0.0);
        }
        let content = entry.content.as_ref()?;
        let mut score = CONTENT_MATCH_BASE_SCORE;
        for term in &self.content_terms {
            let position = content.folded.find(&term.folded)?;
            let proximity = (position as f64).min(4_000.0);
            score += 280.0 - proximity * 0.04;
        }
        Some(score)
    }

    fn requires_folded_path(&self) -> bool {
        !self.negative_terms.is_empty() || !self.path_filters.is_empty()
    }

    fn matches_text_filters(&self, name_folded: Option<&str>, path_folded: Option<&str>) -> bool {
        if !self.negative_terms.is_empty() {
            let (Some(name_folded), Some(path_folded)) = (name_folded, path_folded) else {
                return false;
            };
            if self
                .negative_terms
                .iter()
                .any(|term| name_folded.contains(term) || path_folded.contains(term))
            {
                return false;
            }
        }
        if !self.path_filters.is_empty() {
            let Some(path_folded) = path_folded else {
                return false;
            };
            if !self
                .path_filters
                .iter()
                .all(|filter| path_folded.contains(filter))
            {
                return false;
            }
        }

        true
    }

    fn matches_metadata_filters(&self, entry: &IndexedEntry) -> bool {
        if !self.extensions.is_empty()
            && !entry.extension_lower().is_some_and(|extension| {
                self.extensions
                    .iter()
                    .any(|expected| expected == &extension)
            })
        {
            return false;
        }

        if !self.kinds.is_empty() && !self.kinds.iter().any(|kind| kind == &entry.kind) {
            return false;
        }

        if let Some(after) = self.modified_after {
            let modified_at = entry
                .modified_at
                .as_deref()
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .map(|value| value.with_timezone(&Utc));
            if modified_at.map_or(true, |value| value < after) {
                return false;
            }
        }

        self.size_filters
            .iter()
            .all(|filter| filter.matches(entry.size_bytes))
    }
}

fn new_search_matcher() -> SkimMatcherV2 {
    // File and application names are conventionally case-insensitive on the
    // platforms iHub targets. `SkimMatcherV2` defaults to smart-case, which
    // unexpectedly hides `README.md` when a user types `Readme`; keep search
    // behavior predictable regardless of the query's capitalization.
    SkimMatcherV2::default()
        .ignore_case()
        .element_limit(FUZZY_MATCHER_ELEMENT_LIMIT)
}

fn score_pinyin_term(
    entry: &IndexedEntry,
    matchers: Option<&PinyinTermMatchers<'_>>,
) -> Option<f64> {
    let matchers = matchers?;
    let mut best: Option<f64> = None;
    let mut consider =
        |matcher: &PinyinMatcher<'_>, candidate: &str, base_score: f64, prefix_boost: f64| {
            let Some(matched) = matcher.find(candidate) else {
                return;
            };
            let position_penalty = (matched.start() as f64).min(240.0) * 0.35;
            let partial_penalty = if matched.is_pattern_partial() {
                PINYIN_PARTIAL_PENALTY
            } else {
                0.0
            };
            let score = base_score - position_penalty - partial_penalty
                + if matched.start() == 0 {
                    prefix_boost
                } else {
                    0.0
                };
            best = Some(best.map_or(score, |current| current.max(score)));
        };

    consider(
        &matchers.full,
        &entry.name,
        PINYIN_NAME_FULL_BASE_SCORE,
        PINYIN_NAME_FULL_PREFIX_BOOST,
    );
    consider(
        &matchers.initials,
        &entry.name,
        PINYIN_NAME_INITIAL_BASE_SCORE,
        PINYIN_NAME_INITIAL_PREFIX_BOOST,
    );
    consider(
        &matchers.full,
        &entry.path,
        PINYIN_PATH_FULL_BASE_SCORE,
        0.0,
    );
    consider(
        &matchers.initials,
        &entry.path,
        PINYIN_PATH_INITIAL_BASE_SCORE,
        0.0,
    );

    best
}

fn name_match_boost(name_folded: &str, kind: &str, term_folded: &str) -> f64 {
    if term_folded.is_empty() {
        return 0.0;
    }

    let stem = searchable_name_stem(name_folded, kind);
    if name_folded == term_folded || stem == term_folded {
        return EXACT_NAME_MATCH_BOOST;
    }
    if name_folded.starts_with(term_folded) {
        return NAME_PREFIX_MATCH_BOOST;
    }
    if name_contains_term_at_word_boundary(name_folded, term_folded) {
        return NAME_WORD_BOUNDARY_MATCH_BOOST;
    }
    0.0
}

fn searchable_name_stem<'a>(name: &'a str, kind: &str) -> &'a str {
    if kind != "file" {
        return name;
    }

    match name.rfind('.') {
        Some(position) if position > 0 => &name[..position],
        _ => name,
    }
}

fn name_contains_term_at_word_boundary(name: &str, term: &str) -> bool {
    name.match_indices(term).any(|(position, _)| {
        position == 0
            || name[..position]
                .chars()
                .next_back()
                .is_some_and(|previous| !previous.is_alphanumeric() && previous != '_')
    })
}

#[inline]
fn fold_search_text(value: &str) -> String {
    #[cfg(test)]
    SEARCH_TEXT_FOLD_COUNT.with(|count| count.set(count.get().saturating_add(1)));
    // Compatibility-normalize before lowercasing so full-width Latin text,
    // ligatures and canonically equivalent filesystem spellings share one
    // in-memory key. The visible/persisted path is never rewritten.
    value.nfkc().flat_map(char::to_lowercase).collect()
}

#[cfg(test)]
thread_local! {
    static SEARCH_TEXT_FOLD_COUNT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn reset_search_text_fold_count() {
    SEARCH_TEXT_FOLD_COUNT.with(|count| count.set(0));
}

#[cfg(test)]
fn search_text_fold_count() -> usize {
    SEARCH_TEXT_FOLD_COUNT.with(std::cell::Cell::get)
}

impl SizeFilter {
    fn matches(self, actual: u64) -> bool {
        match self.comparison {
            SizeComparison::LessThan => actual < self.bytes,
            SizeComparison::LessOrEqual => actual <= self.bytes,
            SizeComparison::Equal => actual == self.bytes,
            SizeComparison::GreaterOrEqual => actual >= self.bytes,
            SizeComparison::GreaterThan => actual > self.bytes,
        }
    }
}

fn tokenize_query(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for character in input.chars() {
        match character {
            '"' => quoted = !quoted,
            character if character.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            character => current.push(character),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn split_filter_values(value: &str) -> impl Iterator<Item = &str> {
    value.split(['|', ',']).map(str::trim)
}

fn parse_modified_after(value: &str) -> Option<DateTime<Utc>> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized == "today" {
        return Utc::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .map(|value| value.and_utc());
    }
    let days = normalized.strip_suffix('d')?.parse::<i64>().ok()?;
    if !(1..=36_500).contains(&days) {
        return None;
    }
    Some(Utc::now() - ChronoDuration::days(days))
}

fn parse_size_filter(value: &str) -> Option<SizeFilter> {
    let normalized = value.trim().to_ascii_lowercase();
    let (comparison, raw_size) = if let Some(value) = normalized.strip_prefix(">=") {
        (SizeComparison::GreaterOrEqual, value)
    } else if let Some(value) = normalized.strip_prefix("<=") {
        (SizeComparison::LessOrEqual, value)
    } else if let Some(value) = normalized.strip_prefix('>') {
        (SizeComparison::GreaterThan, value)
    } else if let Some(value) = normalized.strip_prefix('<') {
        (SizeComparison::LessThan, value)
    } else if let Some(value) = normalized.strip_prefix('=') {
        (SizeComparison::Equal, value)
    } else {
        (SizeComparison::Equal, normalized.as_str())
    };
    let raw_size = raw_size.trim();
    let (number, multiplier) = if let Some(number) = raw_size.strip_suffix("kb") {
        (number, 1024_f64)
    } else if let Some(number) = raw_size.strip_suffix('k') {
        (number, 1024_f64)
    } else if let Some(number) = raw_size.strip_suffix("mb") {
        (number, 1024_f64.powi(2))
    } else if let Some(number) = raw_size.strip_suffix('m') {
        (number, 1024_f64.powi(2))
    } else if let Some(number) = raw_size.strip_suffix("gb") {
        (number, 1024_f64.powi(3))
    } else if let Some(number) = raw_size.strip_suffix('g') {
        (number, 1024_f64.powi(3))
    } else if let Some(number) = raw_size.strip_suffix("tb") {
        (number, 1024_f64.powi(4))
    } else if let Some(number) = raw_size.strip_suffix('t') {
        (number, 1024_f64.powi(4))
    } else if let Some(number) = raw_size.strip_suffix('b') {
        (number, 1_f64)
    } else {
        (raw_size, 1_f64)
    };
    let bytes = number.trim().parse::<f64>().ok()? * multiplier;
    if !bytes.is_finite() || bytes < 0.0 || bytes > u64::MAX as f64 {
        return None;
    }
    Some(SizeFilter {
        comparison,
        bytes: bytes.round() as u64,
    })
}

/// A borrowed search hit kept while selecting the best results.  It avoids
/// cloning path and metadata strings for entries that will fall outside of the
/// requested result window.
#[derive(Clone, Copy)]
struct SearchMatch<'a> {
    entry: &'a IndexedEntry,
    score: f64,
}

impl SearchMatch<'_> {
    /// The public result ordering: highest score first, then name and path in
    /// ascending lexical order.  `BinaryHeap` exposes its greatest member, so
    /// this ordering deliberately makes its head the *worst* retained hit.
    fn result_order(&self, other: &Self) -> CompareOrdering {
        other
            .score
            .total_cmp(&self.score)
            .then_with(|| self.entry.name.cmp(&other.entry.name))
            .then_with(|| self.entry.path.cmp(&other.entry.path))
    }
}

impl PartialEq for SearchMatch<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.result_order(other) == CompareOrdering::Equal
    }
}

impl Eq for SearchMatch<'_> {}

impl PartialOrd for SearchMatch<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<CompareOrdering> {
        Some(self.cmp(other))
    }
}

impl Ord for SearchMatch<'_> {
    fn cmp(&self, other: &Self) -> CompareOrdering {
        self.result_order(other)
    }
}

/// A bounded max-heap whose root is the worst result currently selected.
/// Rayon builds one per worker and merges those small heaps, keeping memory at
/// O(workers × limit) instead of O(number of matching entries).
struct TopMatches<'a> {
    limit: usize,
    matches: BinaryHeap<SearchMatch<'a>>,
}

impl<'a> TopMatches<'a> {
    fn new(limit: usize) -> Self {
        debug_assert!(limit > 0);
        Self {
            limit,
            matches: BinaryHeap::with_capacity(limit),
        }
    }

    fn consider(&mut self, candidate: SearchMatch<'a>) {
        if self.matches.len() < self.limit {
            self.matches.push(candidate);
            return;
        }

        let should_replace = self.matches.peek().is_some_and(|worst| candidate < *worst);
        if should_replace {
            // `PeekMut` restores the heap invariant when it is dropped.
            *self
                .matches
                .peek_mut()
                .expect("a full top-match heap must have a root") = candidate;
        }
    }

    fn merge(mut self, other: Self) -> Self {
        for candidate in other.matches {
            self.consider(candidate);
        }
        self
    }

    fn into_results_for_content(self, content_terms: &[QueryTerm]) -> Vec<SearchResult> {
        let mut matches = self.matches.into_vec();
        matches.sort_unstable();
        matches
            .into_iter()
            .map(|candidate| candidate.entry.to_result(candidate.score, content_terms))
            .collect()
    }
}

impl IndexedEntry {
    fn to_result(&self, score: f64, content_terms: &[QueryTerm]) -> SearchResult {
        let metadata = if content_terms.is_empty() {
            self.metadata.clone()
        } else if let Some(content) = self.content.as_ref() {
            format!(
                "{} · 正文命中：{}",
                self.metadata,
                content_preview(&content.text)
            )
        } else {
            self.metadata.clone()
        };
        SearchResult {
            id: self.id.clone(),
            path: self.path.clone(),
            name: self.name.clone(),
            kind: self.kind.clone(),
            pin_eligible: self.is_launcher_shortcut_eligible(),
            pinned_shortcut_id: None,
            score,
            metadata,
            modified_at: self.modified_at.clone(),
        }
    }
}

fn load_persisted_snapshot(
    path: &Path,
    managed_state_roots: &[PathBuf],
    active_roots: &[PathBuf],
) -> Option<PersistedIndexSnapshot> {
    let bytes = read_snapshot_bytes(path)?;
    let wire = serde_json::from_slice::<PersistedIndexSnapshotWire>(&bytes).ok()?;
    // The optional binding uses a strict native schema. Decode it separately
    // from the cache envelope so a malformed or future binding cannot discard
    // otherwise valid ordinary path entries.
    let usn_binding = wire.usn_binding.and_then(|raw_binding| {
        serde_json::from_value::<PersistedUsnSnapshotBinding>(raw_binding).ok()
    });
    validate_loaded_snapshot(
        PersistedIndexSnapshot {
            schema_version: wire.schema_version,
            roots: wire.roots,
            last_indexed_at: wire.last_indexed_at,
            usn_binding,
            entries: wire.entries,
        },
        SNAPSHOT_SCHEMA_VERSION,
        managed_state_roots,
        active_roots,
    )
}

/// Restores only the ordinary path-cache fields from the former v2 file. Its
/// `usnBinding` field is not represented in `LegacyPersistedIndexSnapshotWire`
/// at all, so this code cannot accidentally deserialize, validate, migrate,
/// or reuse a metadata-only P1d baseline as a P1e replay proof.
fn load_legacy_persisted_snapshot(
    path: &Path,
    managed_state_roots: &[PathBuf],
    active_roots: &[PathBuf],
) -> Option<PersistedIndexSnapshot> {
    let bytes = read_snapshot_bytes(path)?;
    let wire = serde_json::from_slice::<LegacyPersistedIndexSnapshotWire>(&bytes).ok()?;
    validate_loaded_snapshot(
        PersistedIndexSnapshot {
            schema_version: wire.schema_version,
            roots: wire.roots,
            last_indexed_at: wire.last_indexed_at,
            usn_binding: None,
            entries: wire.entries,
        },
        LEGACY_SNAPSHOT_SCHEMA_VERSION,
        managed_state_roots,
        active_roots,
    )
}

fn read_snapshot_bytes(path: &Path) -> Option<Vec<u8>> {
    read_regular_state_file(path, MAX_SNAPSHOT_BYTES)
}

/// Read a local state file without following a pre-existing symlink and with
/// a second, streaming size bound after open. The second limit matters when a
/// file is replaced between metadata inspection and the read: cache loading
/// must never turn into an unbounded startup allocation.
fn read_regular_state_file(path: &Path, max_bytes: u64) -> Option<Vec<u8>> {
    let link_metadata = fs::symlink_metadata(path).ok()?;
    if !link_metadata.file_type().is_file() || link_metadata.len() > max_bytes {
        return None;
    }

    let file = fs::File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > max_bytes {
        return None;
    }

    let capacity = usize::try_from(metadata.len().min(max_bytes)).ok()?;
    let mut bytes = Vec::with_capacity(capacity);
    let mut reader = file.take(max_bytes.saturating_add(1));
    reader.read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= max_bytes).then_some(bytes)
}

fn validate_loaded_snapshot(
    mut snapshot: PersistedIndexSnapshot,
    expected_schema_version: u8,
    managed_state_roots: &[PathBuf],
    active_roots: &[PathBuf],
) -> Option<PersistedIndexSnapshot> {
    if snapshot.schema_version != expected_schema_version
        || snapshot.entries.len() > MAX_INDEXED_ENTRIES + MAX_APPLICATION_ENTRIES
        || !snapshot_timestamp_is_usable(&snapshot.last_indexed_at, Utc::now())
    {
        return None;
    }
    // A snapshot is cache data, not an authority to expand the content scope.
    // Checking only its root list is not enough: a partial/corrupt write could
    // still contain a syntactically valid file record from a previous scope.
    // Applications intentionally remain separate because their discovery roots
    // are OS-owned Start Menu / Applications locations, not content roots.
    let snapshot_roots = snapshot.roots.iter().map(PathBuf::from).collect::<Vec<_>>();
    if snapshot_roots.iter().any(|root| !root.is_absolute())
        || root_scope_keys(&snapshot_roots).len() != snapshot_roots.len()
        || !snapshot_scope_matches_active_roots(&snapshot.roots, active_roots)
    {
        return None;
    }
    // Cache records must be exactly the metadata projection produced by the
    // authorized scanner.  A partial, hand-edited, or stale snapshot is not
    // repaired in place: keeping even its valid subset would make it look
    // ready for a root set it no longer proves.  Declining it makes startup
    // take the normal scoped rebuild path instead.
    if !snapshot_entries_are_valid_for_scope(
        &snapshot.entries,
        &snapshot_roots,
        managed_state_roots,
    ) {
        return None;
    }
    // The path snapshot stays usable if optional P1e metadata is malformed,
    // but no malformed or differently-scoped identity projection can qualify
    // it for a zero-change restart shortcut.
    if snapshot
        .usn_binding
        .as_ref()
        .is_some_and(|binding| !snapshot_usn_binding_matches_scope(binding, &snapshot_roots))
    {
        snapshot.usn_binding = None;
    }
    let snapshot_entry_count = snapshot.entries.len();
    sort_and_deduplicate_entries(&mut snapshot.entries);
    // `snapshot_entries_are_valid_for_scope` rejects duplicate logical paths
    // before this sort. Keep this guard adjacent to the replay proof so an
    // accidental future change cannot turn a repaired partial cache into a
    // journal-qualified fast-start source.
    if snapshot.entries.len() != snapshot_entry_count {
        return None;
    }
    // A same-length edit can preserve the ordinary entry count (for example,
    // replacing one valid in-scope path with another). The persisted replay
    // identity is safe only when it still maps one-for-one to every final
    // non-application entry the launcher will expose from this snapshot.
    if snapshot
        .usn_binding
        .as_ref()
        .is_some_and(|binding| !snapshot_entries_match_replay_binding(&snapshot.entries, binding))
    {
        snapshot.usn_binding = None;
    }
    Some(snapshot)
}

fn snapshot_timestamp_is_usable(value: &str, now: DateTime<Utc>) -> bool {
    let Ok(timestamp) = DateTime::parse_from_rfc3339(value) else {
        return false;
    };
    let timestamp = timestamp.with_timezone(&Utc);
    timestamp <= now + ChronoDuration::minutes(MAX_SNAPSHOT_FUTURE_SKEW_MINUTES)
        && timestamp >= now - ChronoDuration::days(MAX_SNAPSHOT_AGE_DAYS)
}

fn snapshot_entries_are_valid_for_scope(
    entries: &[IndexedEntry],
    roots: &[PathBuf],
    managed_state_roots: &[PathBuf],
) -> bool {
    let mut content_entries = 0usize;
    let mut application_entries = 0usize;
    let mut paths = HashSet::with_capacity(entries.len());

    entries.iter().all(|entry| {
        if !snapshot_entry_is_valid_for_scope(entry, roots, managed_state_roots) {
            return false;
        }
        (match entry.kind.as_str() {
            "application" => {
                application_entries += 1;
                application_entries <= MAX_APPLICATION_ENTRIES
            }
            "file" | "folder" => {
                content_entries += 1;
                content_entries <= MAX_INDEXED_ENTRIES
            }
            _ => false,
        }) && paths.insert(snapshot_entry_path_key(entry))
    })
}

fn snapshot_entry_is_valid_for_scope(
    entry: &IndexedEntry,
    roots: &[PathBuf],
    managed_state_roots: &[PathBuf],
) -> bool {
    let path = Path::new(&entry.path);
    let expected_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| entry.path.clone());
    let expected_extension = path
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase());
    let modified_at_is_valid = entry
        .modified_at
        .as_deref()
        .map(|value| DateTime::parse_from_rfc3339(value).is_ok())
        .unwrap_or(true);
    let has_safe_components = !path
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir));

    entry.is_valid()
        && path.is_absolute()
        && has_safe_components
        && entry.name == expected_name
        && entry.extension == expected_extension
        && modified_at_is_valid
        && !path_is_in_managed_state(path, managed_state_roots)
        && match entry.kind.as_str() {
            "application" => entry.id == format!("application:{}", entry.path),
            "file" | "folder" => entry.id == entry.path && entry_is_within_any_root(entry, roots),
            _ => false,
        }
}

fn snapshot_entry_path_key(entry: &IndexedEntry) -> String {
    root_scope_key(Path::new(&entry.path))
}

fn snapshot_scope_matches_active_roots(
    snapshot_roots: &[String],
    active_roots: &[PathBuf],
) -> bool {
    if snapshot_roots.len() != active_roots.len() {
        return false;
    }
    let snapshot = snapshot_roots
        .iter()
        .map(|root| root_scope_key(Path::new(root)))
        .collect::<HashSet<_>>();
    snapshot.len() == snapshot_roots.len() && snapshot == root_scope_keys(active_roots)
}

fn snapshot_usn_binding_matches_scope(
    binding: &PersistedUsnSnapshotBinding,
    active_roots: &[PathBuf],
) -> bool {
    binding.schema_version == USN_SNAPSHOT_BINDING_SCHEMA_VERSION
        && snapshot_scope_matches_active_roots(&binding.roots, active_roots)
        && ntfs_usn::validate_replay_binding(active_roots, &binding.replay).is_ok()
}

fn build_usn_snapshot_binding(
    roots: &[PathBuf],
    entries: &[IndexedEntry],
    indexed_pairs: &[MftIndexedEntry],
    replay_seeds: &[ntfs_usn::UsnReplayVolumeSeed],
) -> Result<PersistedUsnSnapshotBinding, String> {
    let content_entries = entries
        .iter()
        .filter(|entry| entry.kind != "application")
        .collect::<Vec<_>>();
    if content_entries.len() > ntfs_usn::MAX_USN_REPLAY_STABLE_PATHS
        || indexed_pairs.len() > ntfs_usn::MAX_USN_REPLAY_STABLE_PATHS
    {
        return Err(format!(
            "稳定路径数量超过安全上限 {}",
            ntfs_usn::MAX_USN_REPLAY_STABLE_PATHS
        ));
    }
    if content_entries.len() != indexed_pairs.len() {
        return Err("MFT 成功索引路径与快照内容条目数量不一致".to_owned());
    }
    if content_entries.is_empty() || replay_seeds.is_empty() {
        return Err("MFT 稳定路径投影或卷截止点为空".to_owned());
    }

    let mut entries_by_path = HashMap::<String, &IndexedEntry>::new();
    for entry in &content_entries {
        if !entry.is_valid() || !matches!(entry.kind.as_str(), "file" | "folder") {
            return Err("快照内容条目不是有效的文件或文件夹".to_owned());
        }
        let key = replay_binding_path_key(Path::new(&entry.path));
        if key.is_empty() || entries_by_path.insert(key, *entry).is_some() {
            return Err("快照内容条目包含重复或大小写歧义路径".to_owned());
        }
    }

    let mut volumes = Vec::with_capacity(replay_seeds.len());
    let mut seen_volume_keys = HashSet::new();
    for seed in replay_seeds {
        if seed.volume_key.is_empty()
            || seed.root_file_reference_number == 0
            || seed.cutoff.volume_key != seed.volume_key
        {
            return Err("MFT 卷种子或其截止检查点不匹配".to_owned());
        }
        if !seen_volume_keys.insert(seed.volume_key.clone()) {
            return Err("MFT 卷种子包含重复盘符根目录".to_owned());
        }
        volumes.push(ntfs_usn::UsnReplayVolume {
            volume_key: seed.volume_key.clone(),
            volume_root: seed.volume_root.clone(),
            root_file_reference_number: seed.root_file_reference_number,
            paths: Vec::new(),
        });
    }

    let mut pairs_by_path = HashMap::<String, &MftIndexedEntry>::new();
    let mut directory_references = HashMap::<(String, u64), String>::new();
    for pair in indexed_pairs {
        let entry_key = replay_binding_path_key(Path::new(&pair.entry.path));
        let mft_key = replay_binding_path_key(&pair.path.path);
        if entry_key.is_empty() || entry_key != mft_key {
            return Err("MFT 身份路径与已索引条目路径不一致".to_owned());
        }
        if pairs_by_path.insert(entry_key.clone(), pair).is_some() {
            return Err("MFT 成功索引路径包含重复或大小写歧义别名".to_owned());
        }
        let Some(snapshot_entry) = entries_by_path.get(&entry_key) else {
            return Err("MFT 身份路径不在最终内容快照中".to_owned());
        };
        if snapshot_entry.id != pair.entry.id
            || snapshot_entry.path != pair.entry.path
            || snapshot_entry.name != pair.entry.name
            || snapshot_entry.kind != pair.entry.kind
        {
            return Err("MFT 身份路径未与最终快照条目精确配对".to_owned());
        }
        let expected_kind = if pair.path.is_directory {
            "folder"
        } else {
            "file"
        };
        if pair.entry.kind != expected_kind {
            return Err("MFT 目录属性与已索引条目类型不一致".to_owned());
        }
        if !pair.path.is_root && pair.entry.name != pair.path.name {
            return Err("MFT 文件名与已索引条目名称不一致".to_owned());
        }
        if pair.path.is_directory
            && directory_references
                .insert(
                    (
                        pair.path.volume_key.clone(),
                        pair.path.file_reference_number,
                    ),
                    entry_key,
                )
                .is_some()
        {
            return Err("MFT 目录引用存在多个路径别名".to_owned());
        }

        let Some(volume) = volumes
            .iter_mut()
            .find(|volume| volume.volume_key == pair.path.volume_key)
        else {
            return Err("MFT 身份路径不属于已完成的卷种子".to_owned());
        };
        if pair.path.is_root
            && (pair.path.path != volume.volume_root
                || !pair.path.is_directory
                || !pair.path.name.is_empty()
                || pair.path.file_reference_number != volume.root_file_reference_number
                || pair.path.parent_file_reference_number != volume.root_file_reference_number)
        {
            return Err("MFT 盘符根目录身份与卷种子不匹配".to_owned());
        }
        volume.paths.push(ntfs_usn::UsnReplayStablePath {
            path: pair.path.path.clone(),
            file_reference_number: pair.path.file_reference_number,
            parent_file_reference_number: pair.path.parent_file_reference_number,
            name: pair.path.name.clone(),
            is_directory: pair.path.is_directory,
            is_root: pair.path.is_root,
        });
    }

    if pairs_by_path.len() != entries_by_path.len() {
        return Err("MFT 身份路径与最终内容快照不是双向精确对应".to_owned());
    }
    for volume in &mut volumes {
        volume.paths.sort_unstable_by(|left, right| {
            replay_binding_path_key(&left.path).cmp(&replay_binding_path_key(&right.path))
        });
        let root_paths = volume.paths.iter().filter(|path| path.is_root).count();
        if root_paths != 1 {
            return Err("MFT 卷稳定路径缺少或重复盘符根目录".to_owned());
        }
    }
    volumes.sort_unstable_by(|left, right| left.volume_key.cmp(&right.volume_key));

    let mut checkpoints = replay_seeds
        .iter()
        .map(|seed| seed.cutoff.clone())
        .collect::<Vec<_>>();
    checkpoints.sort_unstable_by(|left, right| left.volume_key.cmp(&right.volume_key));
    let replay = ntfs_usn::UsnReplayBinding {
        schema_version: ntfs_usn::USN_REPLAY_BINDING_SCHEMA_VERSION,
        checkpoints,
        volumes,
    };
    ntfs_usn::validate_replay_binding(roots, &replay)
        .map_err(|error| format!("MFT 稳定路径绑定校验失败：{error}"))?;

    let binding = PersistedUsnSnapshotBinding {
        schema_version: USN_SNAPSHOT_BINDING_SCHEMA_VERSION,
        roots: roots
            .iter()
            .map(|root| root.to_string_lossy().to_string())
            .collect(),
        replay,
    };
    if !snapshot_usn_binding_matches_scope(&binding, roots) {
        return Err("MFT 稳定路径绑定与授权范围不匹配".to_owned());
    }
    if !snapshot_entries_match_replay_binding(entries, &binding) {
        return Err("MFT 稳定路径与最终内容快照不是精确对应".to_owned());
    }
    Ok(binding)
}

/// A lexical case-insensitive key used only to associate already-authorized
/// MFT path projections with their serialized index entries. Native replay
/// validation performs the stricter Windows component and root checks later.
fn replay_binding_path_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_start_matches(r"\\?\")
        .to_ascii_lowercase()
}

/// Confirms that the validated ordinary snapshot projection is still
/// exactly the stable-path projection that was atomically saved alongside it.
/// The native validator owns FRN, root, alias, and parent-chain correctness;
/// this layer binds that already-valid native identity graph to the visible
/// `IndexedEntry` file/folder set so a same-length cache edit cannot retain a
/// replay proof for different paths.
fn snapshot_entries_match_replay_binding(
    entries: &[IndexedEntry],
    binding: &PersistedUsnSnapshotBinding,
) -> bool {
    let mut entries_by_path = HashMap::<String, &IndexedEntry>::new();
    for entry in entries.iter().filter(|entry| entry.kind != "application") {
        if !entry.is_valid()
            || !matches!(entry.kind.as_str(), "file" | "folder")
            || entry.id != entry.path
        {
            return false;
        }
        let key = replay_binding_path_key(Path::new(&entry.path));
        if key.is_empty() || entries_by_path.insert(key, entry).is_some() {
            return false;
        }
    }

    let mut stable_by_path = HashMap::<String, &ntfs_usn::UsnReplayStablePath>::new();
    for volume in &binding.replay.volumes {
        for stable_path in &volume.paths {
            let key = replay_binding_path_key(&stable_path.path);
            if key.is_empty() || stable_by_path.insert(key, stable_path).is_some() {
                return false;
            }
        }
    }
    if entries_by_path.len() != stable_by_path.len() {
        return false;
    }

    entries_by_path.into_iter().all(|(key, entry)| {
        let Some(stable_path) = stable_by_path.get(&key) else {
            return false;
        };
        let expected_kind = if stable_path.is_directory {
            "folder"
        } else {
            "file"
        };
        entry.path == stable_path.path.to_string_lossy()
            && entry.kind == expected_kind
            && if stable_path.is_root {
                // `IndexedEntry` gives a drive root a visible display name
                // while the MFT synthetic root correctly has an empty NTFS
                // component name. Both serialized values must retain that
                // exact convention rather than accepting a case-folded alias.
                entry.name == entry.path
            } else {
                entry.name == stable_path.name
            }
    })
}

/// Reconciles only paths that the native replay marked as affected. The old
/// spelling of a rename may no longer exist in the new binding, while its new
/// spelling must be restatted from the exact stable projection. Raw strings
/// are intentional: a case-only rename needs the stale and replacement paths
/// treated as two distinct cache operations even though Windows folds them for
/// identity comparisons.
#[cfg(windows)]
fn reconcile_replayed_snapshot_entries(
    entries: Vec<IndexedEntry>,
    roots: &[PathBuf],
    internal_state_roots: &[PathBuf],
    binding: &PersistedUsnSnapshotBinding,
    dirty_paths: &[PathBuf],
) -> Result<Vec<IndexedEntry>, String> {
    let mut stable_by_raw_path = HashMap::<String, &ntfs_usn::UsnReplayStablePath>::new();
    for volume in &binding.replay.volumes {
        for stable_path in &volume.paths {
            let raw_path = stable_path.path.to_string_lossy().to_string();
            if raw_path.is_empty() || stable_by_raw_path.insert(raw_path, stable_path).is_some() {
                return Err("USN 回放稳定路径包含重复或空的序列化路径".to_owned());
            }
        }
    }

    let dirty_raw_paths = dirty_paths
        .iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect::<HashSet<_>>();
    if dirty_raw_paths.len() != dirty_paths.len() {
        return Err("USN 回放返回重复的受影响路径".to_owned());
    }
    let mut next_entries = entries
        .into_iter()
        .filter(|entry| entry.kind == "application" || !dirty_raw_paths.contains(&entry.path))
        .collect::<Vec<_>>();

    for dirty_path in dirty_paths {
        let raw_path = dirty_path.to_string_lossy().to_string();
        let Some(stable_path) = stable_by_raw_path.get(&raw_path) else {
            // A deletion or the old side of a rename deliberately has no
            // surviving stable path. Its old cached entry was removed above.
            continue;
        };
        if stable_path.is_root {
            return Err("USN 回放将授权盘符根目录标记为普通受影响路径".to_owned());
        }
        if path_is_in_managed_state(&stable_path.path, internal_state_roots)
            || !entry_path_is_within_any_root(&stable_path.path, roots)
        {
            return Err("USN 回放后的稳定路径越过当前授权或状态目录边界".to_owned());
        }
        let metadata = fs::symlink_metadata(&stable_path.path).map_err(|error| {
            format!(
                "无法重新读取 USN 回放后的路径 {}：{error}",
                stable_path.path.display()
            )
        })?;
        let entry = indexed_entry_from_path(&stable_path.path, &metadata).ok_or_else(|| {
            format!(
                "USN 回放后的路径不再是可安全索引的普通文件或目录：{}",
                stable_path.path.display()
            )
        })?;
        let expected_kind = if stable_path.is_directory {
            "folder"
        } else {
            "file"
        };
        if entry.path != raw_path
            || entry.id != entry.path
            || entry.kind != expected_kind
            || entry.name != stable_path.name
        {
            return Err(format!(
                "USN 回放后的元数据与稳定路径身份不一致：{}",
                stable_path.path.display()
            ));
        }
        next_entries.push(entry);
    }

    Ok(next_entries)
}

#[cfg(windows)]
fn entry_path_is_within_any_root(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path_is_within_root(path, root))
}

fn root_scopes_match(left: &[PathBuf], right: &[PathBuf]) -> bool {
    left.len() == right.len()
        && root_scope_keys(left).len() == left.len()
        && root_scope_keys(left) == root_scope_keys(right)
}

fn root_scope_keys(roots: &[PathBuf]) -> HashSet<String> {
    roots.iter().map(|root| root_scope_key(root)).collect()
}

fn managed_state_roots(paths: &[Option<&Path>]) -> Vec<PathBuf> {
    unique_paths(
        paths
            .iter()
            .flatten()
            .filter_map(|path| path.parent())
            .filter(|parent| parent.is_absolute())
            .map(|parent| {
                parent
                    .canonicalize()
                    .unwrap_or_else(|_| parent.to_path_buf())
            })
            .collect(),
    )
}

fn path_is_in_managed_state(path: &Path, managed_state_roots: &[PathBuf]) -> bool {
    managed_state_roots
        .iter()
        .any(|state_root| path_is_within_root(path, state_root))
}

fn zero_change_storage_is_external(inner: &IndexInner, roots: &[PathBuf]) -> bool {
    inner.snapshot_path.is_some()
        && !roots.is_empty()
        && !inner.internal_state_roots.is_empty()
        && inner.internal_state_roots.iter().all(|state_root| {
            !roots
                .iter()
                .any(|root| path_is_within_root(state_root, root))
        })
}

fn entry_is_within_any_root(entry: &IndexedEntry, roots: &[PathBuf]) -> bool {
    let entry_path = Path::new(&entry.path);
    roots
        .iter()
        .any(|root| path_is_within_root(entry_path, root))
}

fn path_is_within_root(path: &Path, root: &Path) -> bool {
    #[cfg(target_os = "windows")]
    {
        let path = root_scope_key(path);
        let root = root_scope_key(root).trim_end_matches('\\').to_owned();
        path == root
            || path
                .strip_prefix(&root)
                .is_some_and(|remaining| remaining.starts_with('\\'))
    }
    #[cfg(not(target_os = "windows"))]
    {
        path.starts_with(root)
    }
}

fn root_scope_key(path: &Path) -> String {
    let display = path.to_string_lossy();
    #[cfg(target_os = "windows")]
    {
        display
            .replace('/', "\\")
            .trim_start_matches(r"\\?\")
            .to_ascii_lowercase()
    }
    #[cfg(not(target_os = "windows"))]
    {
        display.into_owned()
    }
}

fn load_persisted_roots(path: &Path) -> RootSelection {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return RootSelection::Default;
        }
        Err(_) => return RootSelection::Unavailable,
        Ok(_) => {}
    }
    let Some(bytes) = read_regular_state_file(path, MAX_ROOTS_FILE_BYTES) else {
        return RootSelection::Unavailable;
    };
    let stored = match serde_json::from_slice::<PersistedIndexRoots>(&bytes) {
        Ok(stored) if stored.schema_version == ROOTS_SCHEMA_VERSION => stored,
        _ => return RootSelection::Unavailable,
    };
    if stored.roots.len() > MAX_CONFIGURED_ROOTS {
        return RootSelection::Unavailable;
    }
    if stored.roots.is_empty() {
        return RootSelection::Default;
    }
    match normalize_configured_roots(stored.roots) {
        Ok(roots) if !roots.is_empty() => RootSelection::Custom(roots),
        _ => RootSelection::Unavailable,
    }
}

fn normalize_configured_roots(requested_roots: Vec<String>) -> Result<Vec<PathBuf>, String> {
    if requested_roots.len() > MAX_CONFIGURED_ROOTS {
        return Err(format!(
            "Choose at most {MAX_CONFIGURED_ROOTS} local index folders."
        ));
    }

    let mut roots = Vec::with_capacity(requested_roots.len());
    for raw_root in requested_roots {
        let raw_root = raw_root.trim();
        if raw_root.is_empty() {
            continue;
        }
        let requested = PathBuf::from(raw_root);
        if !requested.is_absolute() {
            return Err(format!(
                "Index folder '{}' must use an absolute path.",
                requested.display()
            ));
        }
        let canonical = requested.canonicalize().map_err(|error| {
            format!(
                "Could not resolve index folder '{}': {error}",
                requested.display()
            )
        })?;
        if !canonical.is_dir() {
            return Err(format!(
                "Index folder '{}' is not a directory.",
                canonical.display()
            ));
        }
        roots.push(canonical);
    }
    Ok(unique_paths(roots))
}

fn persist_roots(path: &Path, roots: &[PathBuf]) -> Result<(), String> {
    let payload = PersistedIndexRoots {
        schema_version: ROOTS_SCHEMA_VERSION,
        roots: roots
            .iter()
            .map(|root| root.to_string_lossy().to_string())
            .collect(),
    };
    let bytes = serde_json::to_vec(&payload)
        .map_err(|error| format!("could not serialize index folders: {error}"))?;
    if bytes.len() as u64 > MAX_ROOTS_FILE_BYTES {
        return Err("index folder configuration exceeds the safety limit".to_owned());
    }
    replace_file_atomically(path, &bytes, "index folder configuration")
}

fn persist_snapshot(
    path: &Path,
    roots: &[PathBuf],
    last_indexed_at: &str,
    entries: &[IndexedEntry],
    usn_binding: Option<&PersistedUsnSnapshotBinding>,
) -> Result<(), String> {
    if !snapshot_timestamp_is_usable(last_indexed_at, Utc::now()) {
        return Err(
            "snapshot timestamp is missing, invalid, stale, or too far in the future".to_owned(),
        );
    }
    if entries.len() > MAX_INDEXED_ENTRIES + MAX_APPLICATION_ENTRIES {
        return Err("snapshot entry count exceeds the safety limit".to_owned());
    }
    let snapshot = PersistedIndexSnapshotRef {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        roots: roots
            .iter()
            .map(|root| root.to_string_lossy().to_string())
            .collect(),
        last_indexed_at,
        usn_binding,
        entries,
    };
    let bytes = serde_json::to_vec(&snapshot)
        .map_err(|error| format!("could not serialize the snapshot: {error}"))?;
    if bytes.len() as u64 > MAX_SNAPSHOT_BYTES {
        return Err(format!(
            "snapshot is {} MiB, exceeding the {} MiB safety limit",
            bytes.len() / (1024 * 1024),
            MAX_SNAPSHOT_BYTES / (1024 * 1024)
        ));
    }
    replace_file_atomically(path, &bytes, "snapshot")
}

fn replace_file_atomically(path: &Path, bytes: &[u8], label: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{label} path has no parent directory"))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("could not create {label} directory: {error}"))?;

    if let Ok(existing) = fs::symlink_metadata(path) {
        if !existing.file_type().is_file() {
            return Err(format!(
                "refusing to replace non-regular {label} state path: {}",
                path.display()
            ));
        }
    }

    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("ihub-state.json");
    let mut temporary = None;
    let mut file = None;
    for _ in 0..MAX_ATOMIC_REPLACE_ATTEMPTS {
        let sequence = ATOMIC_REPLACE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".{file_name}.{}.{}.tmp",
            std::process::id(),
            sequence
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(candidate_file) => {
                temporary = Some(candidate);
                file = Some(candidate_file);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!("could not create temporary {label}: {error}"));
            }
        }
    }
    let temporary = temporary.ok_or_else(|| {
        format!(
            "could not reserve a unique temporary file for {label} after {MAX_ATOMIC_REPLACE_ATTEMPTS} attempts"
        )
    })?;
    let mut file = file.expect("a reserved temporary path must have an open file");
    if let Err(error) = std::io::Write::write_all(&mut file, bytes).and_then(|()| file.sync_all()) {
        let _ = fs::remove_file(&temporary);
        return Err(format!(
            "could not write and sync temporary {label}: {error}"
        ));
    }
    drop(file);

    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("could not replace previous {label}: {error}"));
    }
    sync_state_directory(parent, label)?;
    Ok(())
}

/// On Unix, syncing the containing directory makes the rename durable across
/// a crash as well as atomic to readers. Windows' rename is journaled, while
/// opening a directory as a synchronizable file is not portable there.
fn sync_state_directory(parent: &Path, label: &str) -> Result<(), String> {
    #[cfg(unix)]
    {
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("could not sync {label} directory: {error}"))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (parent, label);
    }
    Ok(())
}

/// Selects the fastest safe source for a full rebuild. The Windows MFT path
/// is intentionally opt-in by scope rather than by a hidden global setting:
/// it is allowed only when a whole drive root was explicitly authorised.
fn collect_full_scan_entries(
    roots: &[PathBuf],
    inner: &Arc<IndexInner>,
    generation: u64,
    initial_count: usize,
    report_progress: bool,
) -> FullScanCollection {
    #[cfg(windows)]
    {
        let scan_roots = collection_roots(roots);
        let mft_limit = MAX_INDEXED_ENTRIES.saturating_sub(initial_count);
        let mft = ntfs_usn::enumerate_authorized_volume_roots(&scan_roots, mft_limit);
        let mft_status = mft.status;
        let mft_enumerated_records = mft.enumerated_records;
        let mft_replayed_usn_records = mft.replayed_usn_records;
        let covered_roots = mft.covered_roots;
        let mft_paths = mft.paths;
        let mft_replay_seeds = mft.replay_seeds;
        // P1d/P1e only pair restart state with an exact direct-volume P1c
        // result.
        // `available` alone is deliberately not enough: assert that the MFT
        // result covers every effective scan root and emitted exactly one
        // replay seed per covered root before persisting any fast-start proof.
        let mft_snapshot_eligible = mft_status == "available"
            && root_scopes_match(&scan_roots, &covered_roots)
            && mft_replay_seeds.len() == covered_roots.len();
        let mut mft_message = mft.message;
        let mft_indexed_pairs =
            collect_entries_from_mft_paths(&mft_paths, &covered_roots, inner, generation);
        let mut entries = mft_indexed_pairs
            .iter()
            .map(|pair| pair.entry.clone())
            .collect::<Vec<_>>();
        let mft_indexed_paths = entries.len();
        if mft_indexed_paths < mft_paths.len() {
            mft_message.push_str(&format!(
                " 其中 {} 条投影路径无法按当前权限读取元数据，已按常规扫描器规则跳过。",
                mft_paths.len() - mft_indexed_paths
            ));
        }
        if mft_snapshot_eligible && !zero_change_storage_is_external(inner, roots) {
            mft_message.push_str(
                " iHub 状态目录位于已授权盘符根目录内，写入快照会推进同卷 USN；已禁用 P1d/P1e 跨重启快启，继续使用完整扫描与文件监听。",
            );
        }

        let fallback_roots = scan_roots
            .into_iter()
            .filter(|root| {
                !covered_roots
                    .iter()
                    .any(|covered| path_is_within_root(root, covered))
            })
            .collect::<Vec<_>>();
        if !fallback_roots.is_empty()
            && inner.generation.load(Ordering::Relaxed) == generation
            && initial_count.saturating_add(entries.len()) < MAX_INDEXED_ENTRIES
        {
            let mut fallback_entries = collect_entries(
                &fallback_roots,
                inner,
                generation,
                initial_count.saturating_add(entries.len()),
                report_progress,
            );
            entries.append(&mut fallback_entries);
            sort_and_deduplicate_entries(&mut entries);
        }

        FullScanCollection {
            entries,
            mft_indexed_pairs,
            mft_replay_seeds,
            mft_status,
            mft_snapshot_eligible,
            mft_enumerated_records,
            mft_replayed_usn_records,
            mft_indexed_paths,
            mft_message,
        }
    }

    #[cfg(not(windows))]
    {
        FullScanCollection {
            entries: collect_entries(roots, inner, generation, initial_count, report_progress),
            mft_indexed_pairs: Vec::new(),
            mft_replay_seeds: Vec::new(),
            mft_status: "unsupported",
            mft_snapshot_eligible: false,
            mft_enumerated_records: 0,
            mft_replayed_usn_records: 0,
            mft_indexed_paths: 0,
            mft_message: "当前平台不使用 NTFS MFT 初始化；继续使用目录扫描和文件监听。".to_owned(),
        }
    }
}

#[cfg(windows)]
fn collect_entries_from_mft_paths(
    paths: &[ntfs_usn::MftPathEntry],
    covered_roots: &[PathBuf],
    inner: &Arc<IndexInner>,
    generation: u64,
) -> Vec<MftIndexedEntry> {
    if paths.is_empty() || covered_roots.is_empty() {
        return Vec::new();
    }
    let mut entries = paths
        .par_iter()
        .filter_map(|candidate| {
            if inner.generation.load(Ordering::Relaxed) != generation {
                return None;
            }
            if path_is_in_managed_state(&candidate.path, &inner.internal_state_roots) {
                return None;
            }
            let metadata = fs::symlink_metadata(&candidate.path).ok()?;
            let entry = indexed_entry_from_path(&candidate.path, &metadata)?;
            entry_is_within_any_root(&entry, covered_roots).then_some(MftIndexedEntry {
                entry,
                path: candidate.clone(),
            })
        })
        .collect::<Vec<_>>();
    entries.sort_unstable_by(|left, right| {
        compare_indexed_entries(&left.entry, &right.entry).then_with(|| {
            replay_binding_path_key(&left.path.path).cmp(&replay_binding_path_key(&right.path.path))
        })
    });
    entries
}

fn collect_entries(
    roots: &[PathBuf],
    inner: &Arc<IndexInner>,
    generation: u64,
    initial_count: usize,
    report_progress: bool,
) -> Vec<IndexedEntry> {
    // `WalkBuilder` runs several visitors in parallel. Accumulating each
    // discovered entry directly in one shared `Vec` turns a large scan into a
    // mutex convoy. Each visitor keeps a private chunk and publishes it once
    // when it is dropped; only the bounded counter remains contended per path.
    let collected = Arc::new(Mutex::new(Vec::<Vec<IndexedEntry>>::new()));
    let count = Arc::new(AtomicUsize::new(initial_count));

    // `ignore` explicitly recommends a multi-path `WalkBuilder`: all roots
    // share compiled ignore state and its worker pool. The earlier one-root
    // loop started and drained a complete parallel walk before moving to the
    // next root. On a large Documents folder that could make Desktop or
    // Downloads look stale for most of a rebuild. One combined walker lets
    // its work queue interleave the authorized roots while avoiding repeated
    // worker setup.
    let scan_roots = collection_roots(roots);
    if !scan_roots.is_empty() && count.load(Ordering::Relaxed) < MAX_INDEXED_ENTRIES {
        let walker = WalkBuilder::from_iter(scan_roots)
            .hidden(false)
            .ignore(false)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .parents(false)
            .follow_links(false)
            .build_parallel();

        let collected = Arc::clone(&collected);
        let count = Arc::clone(&count);
        let inner = Arc::clone(inner);
        walker.run(move || {
            let collected = Arc::clone(&collected);
            let count = Arc::clone(&count);
            let inner = Arc::clone(&inner);
            let mut local = ThreadEntryBuffer::new(collected);
            Box::new(move |result| {
                if inner.generation.load(Ordering::Relaxed) != generation {
                    return WalkState::Quit;
                }
                let entry = match result {
                    Ok(entry) => entry,
                    Err(_) => return WalkState::Continue,
                };
                let file_type = match entry.file_type() {
                    Some(file_type) if !file_type.is_symlink() => file_type,
                    _ => return WalkState::Continue,
                };
                if !file_type.is_file() && !file_type.is_dir() {
                    return WalkState::Continue;
                }
                if path_is_in_managed_state(entry.path(), &inner.internal_state_roots) {
                    return if file_type.is_dir() {
                        WalkState::Skip
                    } else {
                        WalkState::Continue
                    };
                }

                let current = count.fetch_add(1, Ordering::Relaxed);
                if current >= MAX_INDEXED_ENTRIES {
                    return WalkState::Quit;
                }
                if report_progress && current % 1_000 == 0 {
                    let mut status = inner
                        .status
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if inner.generation.load(Ordering::Relaxed) == generation {
                        status.indexed_files = current + 1;
                    }
                }

                let path = entry.path().to_path_buf();
                let name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.to_string_lossy().to_string());
                let metadata = entry.metadata().ok();
                let modified_at = metadata
                    .as_ref()
                    .and_then(|metadata| metadata.modified().ok())
                    .map(system_time_to_iso);
                let size = metadata
                    .as_ref()
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
                let kind = if file_type.is_dir() { "folder" } else { "file" }.to_owned();
                let extension = path
                    .extension()
                    .and_then(|value| value.to_str())
                    .filter(|value| !value.is_empty())
                    .map(|value| value.to_ascii_uppercase());
                let normalized_extension =
                    extension.as_deref().map(|value| value.to_ascii_lowercase());
                let metadata_text = if file_type.is_dir() {
                    "Folder".to_owned()
                } else {
                    let file_type = extension.unwrap_or_else(|| "File".to_owned());
                    format!("{file_type} · {}", human_size(size))
                };
                let path = path.to_string_lossy().to_string();
                let indexed = IndexedEntry {
                    id: path.clone(),
                    path,
                    name,
                    kind,
                    metadata: metadata_text,
                    modified_at,
                    extension: normalized_extension,
                    size_bytes: size,
                    content: None,
                };
                local.entries.push(indexed);
                WalkState::Continue
            })
        });
    }

    let chunks = std::mem::take(
        &mut *collected
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    );
    let total_entries = chunks.iter().map(Vec::len).sum();
    let mut entries = Vec::with_capacity(total_entries);
    for mut chunk in chunks {
        entries.append(&mut chunk);
    }
    sort_and_deduplicate_entries(&mut entries);
    entries
}

/// The search scope remains the full user-selected root list, but a child
/// root never needs a second physical traversal when one of its ancestors is
/// already being scanned. This is especially important for a custom scope
/// such as `C:\\Users\\me` plus `C:\\Users\\me\\Documents`: scanning both used
/// to spend time on the same descendants twice before final de-duplication.
///
/// Do not canonicalize here. Root selection canonicalizes user input before
/// it is persisted; a rescan must not turn a transient I/O error into a scope
/// expansion or silently drop an authorized root. Nonexistent roots are
/// simply ignored by the walker just as they were before this optimization.
fn collection_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut candidates = roots
        .iter()
        .filter(|root| root.is_dir())
        .cloned()
        .collect::<Vec<_>>();
    candidates.sort_unstable_by(|left, right| {
        left.components()
            .count()
            .cmp(&right.components().count())
            .then_with(|| root_scope_key(left).cmp(&root_scope_key(right)))
    });

    let mut selected: Vec<PathBuf> = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if selected
            .iter()
            .any(|ancestor| path_is_within_root(&candidate, ancestor))
        {
            continue;
        }
        selected.push(candidate);
    }
    selected
}

/// Builds the same path-only projection used by the full walker for one
/// concrete filesystem object. `symlink_metadata` is intentional: an event
/// must never let an incremental update traverse a link outside the roots the
/// user explicitly authorized.
fn indexed_entry_from_path(path: &Path, metadata: &fs::Metadata) -> Option<IndexedEntry> {
    let file_type = metadata.file_type();
    if file_type.is_symlink() || (!file_type.is_file() && !file_type.is_dir()) {
        return None;
    }

    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());
    let modified_at = metadata.modified().ok().map(system_time_to_iso);
    let size = metadata.len();
    let kind = if file_type.is_dir() { "folder" } else { "file" }.to_owned();
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_uppercase());
    let normalized_extension = extension.as_deref().map(|value| value.to_ascii_lowercase());
    let metadata_text = if file_type.is_dir() {
        "Folder".to_owned()
    } else {
        let file_type = extension.unwrap_or_else(|| "File".to_owned());
        format!("{file_type} · {}", human_size(size))
    };
    let path = path.to_string_lossy().to_string();
    Some(IndexedEntry {
        id: path.clone(),
        path,
        name,
        kind,
        metadata: metadata_text,
        modified_at,
        extension: normalized_extension,
        size_bytes: size,
        content: None,
    })
}

/// Owns one `ignore` walker visitor's private output. The parallel walker
/// destroys the callback after it finishes a thread, which gives us a safe
/// flush point without locking once for every filesystem entry.
struct ThreadEntryBuffer {
    entries: Vec<IndexedEntry>,
    destination: Arc<Mutex<Vec<Vec<IndexedEntry>>>>,
}

impl ThreadEntryBuffer {
    fn new(destination: Arc<Mutex<Vec<Vec<IndexedEntry>>>>) -> Self {
        Self {
            entries: Vec::with_capacity(256),
            destination,
        }
    }
}

impl Drop for ThreadEntryBuffer {
    fn drop(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let entries = std::mem::take(&mut self.entries);
        self.destination
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(entries);
    }
}

/// User-visible launcher items are discovered from the operating system's
/// application entry points rather than from a hard-coded catalog. The result
/// carries the shortcut or application-bundle path itself, so it only opens
/// when the frontend explicitly selects it via `open_path`.
fn collect_application_entries() -> Vec<IndexedEntry> {
    #[cfg(target_os = "windows")]
    {
        collect_start_menu_entries(&windows_start_menu_roots(), MAX_APPLICATION_ENTRIES)
    }

    #[cfg(target_os = "macos")]
    {
        collect_macos_application_entries()
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Vec::new()
    }
}

#[derive(Clone)]
struct ApplicationRoot {
    path: PathBuf,
    metadata: &'static str,
}

#[cfg(target_os = "windows")]
fn windows_start_menu_roots() -> Vec<ApplicationRoot> {
    let mut roots = Vec::new();
    if let Some(app_data) = env::var_os("APPDATA") {
        roots.push(ApplicationRoot {
            path: PathBuf::from(app_data).join("Microsoft/Windows/Start Menu/Programs"),
            metadata: "开始菜单 · 当前用户",
        });
    }
    if let Some(program_data) = env::var_os("PROGRAMDATA") {
        roots.push(ApplicationRoot {
            path: PathBuf::from(program_data).join("Microsoft/Windows/Start Menu/Programs"),
            metadata: "开始菜单 · 所有用户",
        });
    }
    roots
}

/// Recursively reads only the two Start Menu program trees. It does not
/// resolve a `.lnk` target, follow directory links, or scan Program Files;
/// that keeps discovery fast and means every surfaced entry is something the
/// user can already see and launch from Start Menu.
#[cfg(any(windows, test))]
fn collect_start_menu_entries(roots: &[ApplicationRoot], limit: usize) -> Vec<IndexedEntry> {
    let mut applications = Vec::new();
    let mut pending = VecDeque::new();
    let mut visited = HashSet::new();

    for root in roots {
        if root.path.is_dir() {
            pending.push_back((root.path.clone(), root.metadata, 0_usize));
        }
    }

    while let Some((directory, metadata, depth)) = pending.pop_front() {
        if applications.len() >= limit || visited.len() >= MAX_APPLICATION_DIRECTORIES {
            break;
        }
        let directory_key = directory.to_string_lossy().to_ascii_lowercase();
        if !visited.insert(directory_key) {
            continue;
        }

        let mut children = match fs::read_dir(&directory) {
            Ok(children) => children.filter_map(Result::ok).collect::<Vec<_>>(),
            Err(_) => continue,
        };
        // The filesystem does not guarantee a directory order. Sorting makes
        // the capped result set stable across rebuilds.
        children.sort_unstable_by_key(|entry| entry.file_name());

        for child in children {
            if applications.len() >= limit {
                break;
            }
            let file_type = match child.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            if file_type.is_symlink() {
                continue;
            }
            let path = child.path();
            if file_type.is_dir() {
                if depth < MAX_START_MENU_DEPTH {
                    pending.push_back((path, metadata, depth + 1));
                }
                continue;
            }
            if !file_type.is_file() || !is_start_menu_launch_item(&path) {
                continue;
            }
            applications.push(application_entry(path, metadata));
        }
    }

    sort_and_deduplicate_entries(&mut applications);
    applications
}

#[cfg(any(windows, test))]
fn is_start_menu_launch_item(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some("lnk" | "url" | "appref-ms" | "exe")
    )
}

/// Search may show a Windows shortcut or URL document as an ordinary file
/// when it comes from a user-selected content root. It remains openable as an
/// immediate explicit search result, but a durable launcher pin must not keep
/// a mutable link that can later redirect to a different target.
fn file_is_launcher_shortcut_eligible(path: &Path) -> bool {
    !matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some("lnk" | "url" | "appref-ms")
    )
}

/// A persistent launcher shortcut must not retain a mutable Start Menu link
/// and later follow a different target. Direct executables on Windows and
/// application bundles on macOS have a stable, locally verifiable shape;
/// `.lnk`, `.url` and deployment links remain searchable but are intentionally
/// not eligible for this first native-pinned shortcut boundary.
fn application_is_launcher_shortcut_eligible(path: &Path) -> bool {
    #[cfg(target_os = "windows")]
    {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
    }

    #[cfg(target_os = "macos")]
    {
        is_macos_app_bundle(path)
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = path;
        false
    }
}

#[cfg(target_os = "macos")]
fn collect_macos_application_entries() -> Vec<IndexedEntry> {
    let mut roots = vec![
        ApplicationRoot {
            path: PathBuf::from("/System/Applications"),
            metadata: "系统应用程序",
        },
        ApplicationRoot {
            path: PathBuf::from("/Applications"),
            metadata: "应用程序",
        },
        ApplicationRoot {
            path: PathBuf::from("/System/Library/PreferencePanes"),
            metadata: "系统偏好设置面板",
        },
        ApplicationRoot {
            path: PathBuf::from("/Library/PreferencePanes"),
            metadata: "偏好设置面板",
        },
    ];
    if let Some(home) = env::var_os("HOME") {
        roots.push(ApplicationRoot {
            path: PathBuf::from(&home).join("Applications"),
            metadata: "应用程序 · 当前用户",
        });
        roots.push(ApplicationRoot {
            path: PathBuf::from(home).join("Library/PreferencePanes"),
            metadata: "偏好设置面板 · 当前用户",
        });
    }

    let mut applications = Vec::new();
    let mut pending = VecDeque::new();
    for root in roots {
        if root.path.is_dir() {
            pending.push_back((root.path, root.metadata, 0_usize));
        }
    }

    while let Some((directory, metadata, depth)) = pending.pop_front() {
        if applications.len() >= MAX_APPLICATION_ENTRIES {
            break;
        }
        let mut children = match fs::read_dir(&directory) {
            Ok(children) => children.filter_map(Result::ok).collect::<Vec<_>>(),
            Err(_) => continue,
        };
        children.sort_unstable_by_key(|entry| entry.file_name());

        for child in children {
            if applications.len() >= MAX_APPLICATION_ENTRIES {
                break;
            }
            let file_type = match child.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            let path = child.path();
            // A symlinked .app is a visible application alias. Keep the item
            // but never traverse it, avoiding directory cycles in /Applications.
            if file_type.is_symlink() {
                if is_macos_app_bundle(&path) {
                    applications.push(application_entry(path, metadata));
                }
                continue;
            }
            if !file_type.is_dir() {
                continue;
            }
            if is_macos_app_bundle(&path) {
                applications.push(application_entry(path, metadata));
            } else if depth < MAX_MACOS_APPLICATION_DEPTH {
                pending.push_back((path, metadata, depth + 1));
            }
        }
    }

    sort_and_deduplicate_entries(&mut applications);
    applications
}

#[cfg(any(target_os = "macos", test))]
fn is_macos_app_bundle(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("app") || extension.eq_ignore_ascii_case("prefPane")
        })
}

fn application_entry(path: PathBuf, metadata: &str) -> IndexedEntry {
    let name = path
        .file_stem()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());
    let file_metadata = fs::metadata(&path).ok();
    let modified_at = file_metadata
        .as_ref()
        .and_then(|metadata| metadata.modified().ok())
        .map(system_time_to_iso);
    let size_bytes = file_metadata
        .as_ref()
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase());
    let path = path.to_string_lossy().to_string();
    IndexedEntry {
        id: format!("application:{path}"),
        path,
        name,
        kind: "application".to_owned(),
        metadata: metadata.to_owned(),
        modified_at,
        extension,
        size_bytes,
        content: None,
    }
}

fn compare_indexed_entries(left: &IndexedEntry, right: &IndexedEntry) -> CompareOrdering {
    left.path.cmp(&right.path).then_with(|| {
        let left_priority = usize::from(left.kind != "application");
        let right_priority = usize::from(right.kind != "application");
        left_priority.cmp(&right_priority)
    })
}

fn sort_and_deduplicate_entries(entries: &mut Vec<IndexedEntry>) {
    // If a macOS application bundle was also encountered by a content root,
    // retain the launcher-specific entry so selection still opens the app.
    entries.sort_unstable_by(compare_indexed_entries);
    entries.dedup_by(|left, right| left.path == right.path);
}

/// Merges an already-sorted index projection with a sorted incremental
/// replacement set. The normal scanner, snapshot loader, and previous
/// incremental publication all establish the sorted-input invariant. Keeping
/// this separate from `sort_and_deduplicate_entries` means a single Windows
/// file notification does not repeatedly sort a 500k-entry vector.
fn merge_sorted_entries(
    existing: Vec<IndexedEntry>,
    replacements: Vec<IndexedEntry>,
) -> Vec<IndexedEntry> {
    let mut existing = existing.into_iter().peekable();
    let mut replacements = replacements.into_iter().peekable();
    let mut merged = Vec::with_capacity(
        existing
            .size_hint()
            .0
            .saturating_add(replacements.size_hint().0),
    );

    loop {
        let next = match (existing.peek(), replacements.peek()) {
            (Some(left), Some(right)) if compare_indexed_entries(left, right).is_gt() => {
                replacements.next()
            }
            (Some(_), Some(_)) => existing.next(),
            (Some(_), None) => existing.next(),
            (None, Some(_)) => replacements.next(),
            (None, None) => break,
        };
        let Some(next) = next else {
            break;
        };
        // The comparison puts applications before ordinary content for the
        // same path, preserving the launcher-specific precedence while
        // removing a duplicate without another global sort.
        if merged
            .last()
            .map_or(true, |previous: &IndexedEntry| previous.path != next.path)
        {
            merged.push(next);
        }
    }

    merged
}

/// Retain all application-launcher records and the bounded content portion of
/// the index. A normal full scan enforces this while walking; incremental
/// directory replacements need the same limit after merging into a snapshot.
fn trim_to_index_limit(entries: &mut Vec<IndexedEntry>) {
    let mut content_entries = 0usize;
    entries.retain(|entry| {
        if entry.kind == "application" {
            return true;
        }
        content_entries += 1;
        content_entries <= MAX_INDEXED_ENTRIES
    });
}

pub fn default_roots() -> Vec<PathBuf> {
    if let Some(configured) = env::var_os("IHUB_INDEX_ROOTS") {
        let separator = if cfg!(windows) { ';' } else { ':' };
        let roots: Vec<PathBuf> = configured
            .to_string_lossy()
            .split(separator)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .filter_map(|path| path.canonicalize().ok())
            .filter(|path| path.is_dir())
            .collect();
        if !roots.is_empty() {
            return unique_paths(roots);
        }
    }

    let home = env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from);
    let mut roots = Vec::new();
    if let Some(home) = home {
        for folder in [
            "Desktop",
            "Documents",
            "Downloads",
            "Pictures",
            "Music",
            "Videos",
        ] {
            let candidate = home.join(folder);
            if candidate.is_dir() {
                roots.push(candidate);
            }
        }
        if roots.is_empty() && home.is_dir() {
            roots.push(home);
        }
    }
    unique_paths(roots)
}

pub fn default_root_strings() -> Vec<String> {
    default_roots()
        .into_iter()
        .map(|path| path.to_string_lossy().to_string())
        .collect()
}

fn unique_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|path| {
            let value = root_scope_key(path);
            seen.insert(value)
        })
        .collect()
}

fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

fn system_time_to_iso(time: SystemTime) -> String {
    DateTime::<Utc>::from(time).to_rfc3339()
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[allow(dead_code)]
fn is_path_within(root: &Path, candidate: &Path) -> bool {
    candidate.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{env as std_env, hint::black_box};

    const LOCAL_SEARCH_BENCHMARK_DEFAULT_ENTRIES: usize = 100_000;
    const LOCAL_SEARCH_BENCHMARK_MAX_ENTRIES: usize = 1_000_000;
    const LOCAL_SEARCH_BENCHMARK_DEFAULT_SAMPLES: usize = 21;

    /// The performance harness is intentionally a test-only, in-memory
    /// projection. It exercises the same sort/deduplicate/cap and `search`
    /// code as a full scan, but never enumerates a user path, starts a watcher,
    /// writes a snapshot, or launches the desktop application.
    fn local_search_benchmark_entries() -> usize {
        let configured = std_env::var("IHUB_SEARCH_BENCH_ENTRIES")
            .ok()
            .map(|value| {
                value.parse::<usize>().unwrap_or_else(|_| {
                    panic!(
                        "IHUB_SEARCH_BENCH_ENTRIES must be one of {LOCAL_SEARCH_BENCHMARK_DEFAULT_ENTRIES}, {MAX_INDEXED_ENTRIES}, or {LOCAL_SEARCH_BENCHMARK_MAX_ENTRIES}; received '{value}'."
                    )
                })
            })
            .unwrap_or(LOCAL_SEARCH_BENCHMARK_DEFAULT_ENTRIES);

        if !matches!(
            configured,
            LOCAL_SEARCH_BENCHMARK_DEFAULT_ENTRIES
                | MAX_INDEXED_ENTRIES
                | LOCAL_SEARCH_BENCHMARK_MAX_ENTRIES
        ) {
            panic!(
                "IHUB_SEARCH_BENCH_ENTRIES must be one of {LOCAL_SEARCH_BENCHMARK_DEFAULT_ENTRIES}, {MAX_INDEXED_ENTRIES}, or {LOCAL_SEARCH_BENCHMARK_MAX_ENTRIES}; received {configured}."
            );
        }
        configured
    }

    fn local_search_benchmark_samples() -> usize {
        let configured = std_env::var("IHUB_SEARCH_BENCH_SAMPLES")
            .ok()
            .map(|value| {
                value.parse::<usize>().unwrap_or_else(|_| {
                    panic!(
                        "IHUB_SEARCH_BENCH_SAMPLES must be an odd integer from 5 through 101; received '{value}'."
                    )
                })
            })
            .unwrap_or(LOCAL_SEARCH_BENCHMARK_DEFAULT_SAMPLES);

        if !(5..=101).contains(&configured) || configured % 2 == 0 {
            panic!(
                "IHUB_SEARCH_BENCH_SAMPLES must be an odd integer from 5 through 101; received {configured}."
            );
        }
        configured
    }

    fn synthetic_local_search_benchmark_entries(count: usize) -> Vec<IndexedEntry> {
        let mut entries = Vec::with_capacity(count);
        for ordinal in 0..count {
            let (name, extension) = if ordinal % 333 == 0 {
                (format!("项目计划-{ordinal:06}.md"), "md")
            } else if ordinal % 10 == 0 {
                (format!("roadmap-{ordinal:06}.md"), "md")
            } else {
                (format!("monthly-report-{ordinal:06}.txt"), "txt")
            };
            let project = ordinal % 64;
            let bucket = (ordinal / 64) % 256;
            entries.push(IndexedEntry {
                id: format!("synthetic-entry-{ordinal:06}"),
                path: format!(
                    "/ihub-synthetic-benchmark/projects/project-{project:02}/bucket-{bucket:03}/{name}"
                ),
                name,
                kind: "file".to_owned(),
                metadata: "synthetic benchmark entry".to_owned(),
                modified_at: None,
                extension: Some(extension.to_owned()),
                size_bytes: (ordinal as u64 % 4_096) * 1_024,
                content: None,
            });
        }
        entries
    }

    fn nearest_rank_percentile(samples: &mut [Duration], percentile: usize) -> Duration {
        assert!(
            !samples.is_empty(),
            "benchmark percentile needs at least one sample"
        );
        assert!(percentile > 0 && percentile <= 100);
        samples.sort_unstable();
        let rank = samples.len().saturating_mul(percentile).div_ceil(100);
        samples[rank.saturating_sub(1)]
    }

    fn milliseconds(duration: Duration) -> f64 {
        duration.as_secs_f64() * 1_000.0
    }

    fn search_ascii_candidate_window(index: &SearchIndex, query: &str) -> (usize, bool) {
        let parsed = ParsedQuery::parse(query);
        let required_term_signatures = parsed.required_ascii_term_signatures();
        let snapshot = index
            .inner
            .entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let signatures = snapshot.search_ascii_signatures();
        if required_term_signatures.is_empty() || signatures.len() != snapshot.len() {
            return (snapshot.len(), false);
        }
        (
            signatures
                .iter()
                .filter(|signature| signature.can_match_all_terms(&required_term_signatures))
                .count(),
            true,
        )
    }

    fn entry(name: &str, path: &str) -> IndexedEntry {
        IndexedEntry {
            id: path.to_owned(),
            path: path.to_owned(),
            name: name.to_owned(),
            kind: "file".to_owned(),
            metadata: String::new(),
            modified_at: None,
            extension: Path::new(path)
                .extension()
                .and_then(|value| value.to_str())
                .map(|value| value.to_ascii_lowercase()),
            size_bytes: 0,
            content: None,
        }
    }

    fn publish_entries_with_search_signatures(index: &SearchIndex, entries: Vec<IndexedEntry>) {
        let mut indexed = index
            .inner
            .entries
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        indexed.replace(entries);
    }

    fn result_ids(results: &[SearchResult]) -> Vec<&str> {
        results.iter().map(|result| result.id.as_str()).collect()
    }

    fn indexed_content(text: &str) -> IndexedContent {
        let text = compact_indexed_text(text);
        let folded = fold_search_text(&text);
        let memory_bytes = text.len() + folded.len();
        IndexedContent {
            text,
            folded,
            memory_bytes,
        }
    }

    #[test]
    fn system_icon_sources_resolve_only_current_bounded_result_ids() {
        let index = SearchIndex::new();
        let source_path = "C:/Program Files/iHub/iHub.exe";
        let mut application = entry("iHub", source_path);
        application.kind = "application".to_owned();
        publish_entries_with_search_signatures(&index, vec![application]);

        let sources = index.resolve_system_icon_sources(&[
            source_path.to_owned(),
            "C:/not-indexed.exe".to_owned(),
        ]);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].response_id, source_path);
        assert_eq!(sources[0].path, PathBuf::from(source_path));
        assert_eq!(sources[0].kind, "application");

        assert!(index
            .resolve_system_icon_sources(
                &(0..13)
                    .map(|ordinal| format!("result-{ordinal}"))
                    .collect::<Vec<_>>(),
            )
            .is_empty());
    }

    fn zero_change_checkpoint() -> ntfs_usn::UsnCheckpoint {
        ntfs_usn::UsnCheckpoint {
            volume_key: "C:".to_owned(),
            volume_serial_number: 42,
            journal_id: 99,
            next_usn: 1_024,
            lowest_valid_usn: 512,
            observed_at: "2026-07-28T00:00:00Z".to_owned(),
        }
    }

    fn replay_seed() -> ntfs_usn::UsnReplayVolumeSeed {
        ntfs_usn::UsnReplayVolumeSeed {
            volume_key: "C:".to_owned(),
            volume_root: PathBuf::from(r"C:\"),
            root_file_reference_number: 5,
            cutoff: zero_change_checkpoint(),
        }
    }

    fn mft_indexed_pair(
        path: &str,
        file_reference_number: u64,
        parent_file_reference_number: u64,
        name: &str,
        is_directory: bool,
        is_root: bool,
    ) -> MftIndexedEntry {
        let display_name = if is_root { path } else { name };
        let kind = if is_directory { "folder" } else { "file" };
        MftIndexedEntry {
            entry: IndexedEntry {
                id: path.to_owned(),
                path: path.to_owned(),
                name: display_name.to_owned(),
                kind: kind.to_owned(),
                metadata: String::new(),
                modified_at: None,
                extension: (!is_directory)
                    .then(|| {
                        Path::new(path)
                            .extension()
                            .and_then(|value| value.to_str())
                            .unwrap_or_default()
                            .to_ascii_lowercase()
                    })
                    .filter(|extension| !extension.is_empty()),
                size_bytes: 0,
                content: None,
            },
            path: ntfs_usn::MftPathEntry {
                volume_key: "C:".to_owned(),
                path: PathBuf::from(path),
                file_reference_number,
                parent_file_reference_number,
                name: name.to_owned(),
                is_directory,
                is_root,
            },
        }
    }

    fn complete_replay_projection() -> (
        Vec<PathBuf>,
        Vec<IndexedEntry>,
        Vec<MftIndexedEntry>,
        Vec<ntfs_usn::UsnReplayVolumeSeed>,
    ) {
        let pairs = vec![
            mft_indexed_pair(r"C:\", 5, 5, "", true, true),
            mft_indexed_pair(r"C:\Projects", 10, 5, "Projects", true, false),
            mft_indexed_pair(r"C:\Projects\notes.md", 20, 10, "notes.md", false, false),
        ];
        let entries = pairs.iter().map(|pair| pair.entry.clone()).collect();
        (
            vec![PathBuf::from(r"C:\")],
            entries,
            pairs,
            vec![replay_seed()],
        )
    }

    #[test]
    #[ignore = "manual local-search performance acceptance benchmark; run scripts/run-search-benchmark.ps1 or scripts/run-search-benchmark.sh"]
    fn local_search_performance_acceptance_benchmark() {
        let requested_entries = local_search_benchmark_entries();
        let sample_count = local_search_benchmark_samples();

        // This is the in-memory equivalent of a cold full-scan publication:
        // synthesize stable file metadata, establish the normal sorted/deduped
        // invariant, apply the production entry cap, then publish it to an
        // otherwise storage-free index. It deliberately does not call a
        // filesystem collector, watcher, or snapshot writer.
        let cold_build_started = Instant::now();
        let mut entries = synthetic_local_search_benchmark_entries(requested_entries);
        sort_and_deduplicate_entries(&mut entries);
        trim_to_index_limit(&mut entries);
        let indexed_entries = entries.len();
        let index = SearchIndex::new();
        assert!(index.inner.snapshot_path.is_none());
        assert!(!index.inner.watcher_requested.load(Ordering::Acquire));
        {
            let mut indexed = index
                .inner
                .entries
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            indexed.replace(entries);
        }
        let cold_build_elapsed = cold_build_started.elapsed();

        // Keep the cap visible in the benchmark instead of silently treating a
        // one-million-entry input as a supported one-million-entry resident
        // index. The optional 1M mode measures generation/sort/cap pressure;
        // the query phase always reflects the current production cap.
        assert_eq!(
            indexed_entries,
            requested_entries.min(MAX_INDEXED_ENTRIES),
            "the synthetic fixture must exercise the same cap as a production full scan"
        );

        println!("iHub local-search performance acceptance benchmark");
        println!("scope=synthetic-in-memory index_only=true user_root_scan=false watcher=false snapshot_write=false");
        println!(
            "input_entries={requested_entries} indexed_entries={indexed_entries} max_indexed_entries={MAX_INDEXED_ENTRIES} cold_build_ms={:.3} rayon_threads={} samples_per_query={sample_count}",
            milliseconds(cold_build_elapsed),
            rayon::current_num_threads(),
        );
        if requested_entries > MAX_INDEXED_ENTRIES {
            println!(
                "note=input exceeded the current production cap; query samples use the capped {indexed_entries}-entry projection"
            );
        }

        let scenarios = [
            (
                "exact_filename",
                "monthly-report-000001",
                Some("synthetic-entry-000001"),
            ),
            (
                "specific_filename",
                "roadmap-000010",
                Some("synthetic-entry-000010"),
            ),
            (
                "full_pinyin_filename",
                "xiangmujihua-000333",
                Some("synthetic-entry-000333"),
            ),
            (
                "pinyin_initials_filename",
                "xmjh-000333",
                Some("synthetic-entry-000333"),
            ),
            ("multi_term_filename", "monthly report", None),
            ("structured_filters", "ext:md kind:file", None),
        ];

        for (label, query, expected_id) in scenarios {
            let (candidate_count, used_ascii_prefilter) =
                search_ascii_candidate_window(&index, query);
            // Warm the Rayon worker pool and matcher allocation path before
            // measuring. Cold-build time is reported above; query percentiles
            // intentionally describe steady-state interactive searching.
            for _ in 0..3 {
                black_box(index.search(query, Some(DEFAULT_RESULT_LIMIT)));
            }

            let mut durations = Vec::with_capacity(sample_count);
            let mut first_result_count = 0usize;
            for sample_index in 0..sample_count {
                let started = Instant::now();
                let results = index.search(query, Some(DEFAULT_RESULT_LIMIT));
                durations.push(started.elapsed());

                assert!(
                    !results.is_empty(),
                    "synthetic benchmark query '{query}' unexpectedly returned no results"
                );
                assert!(
                    results.len() <= DEFAULT_RESULT_LIMIT,
                    "synthetic benchmark query '{query}' ignored its requested result limit"
                );
                if let Some(expected_id) = expected_id {
                    assert!(
                        results.iter().any(|result| result.id == expected_id),
                        "exact synthetic benchmark result '{expected_id}' was not returned for '{query}'"
                    );
                }
                if sample_index == 0 {
                    first_result_count = results.len();
                }
                black_box(results);
            }

            let min = *durations.iter().min().expect("benchmark samples");
            let max = *durations.iter().max().expect("benchmark samples");
            let p50 = nearest_rank_percentile(&mut durations.clone(), 50);
            let p95 = nearest_rank_percentile(&mut durations, 95);
            println!(
                "query={label} value={query:?} results={first_result_count} candidates={candidate_count}/{indexed_entries} ascii_prefilter={used_ascii_prefilter} min_ms={:.3} p50_ms={:.3} p95_ms={:.3} max_ms={:.3}",
                milliseconds(min),
                milliseconds(p50),
                milliseconds(p95),
                milliseconds(max),
            );
        }
    }

    #[test]
    fn top_matches_keeps_the_same_best_order_as_public_results() {
        let entries = [
            entry("zulu", "/zulu"),
            entry("alpha", "/alpha"),
            entry("aardvark", "/aardvark"),
            entry("beta", "/beta"),
        ];
        let scores = [40.0, 50.0, 50.0, 5.0];
        let mut matches = TopMatches::new(2);

        for (entry, score) in entries.iter().zip(scores) {
            matches.consider(SearchMatch { entry, score });
        }

        let results = matches.into_results_for_content(&[]);
        assert_eq!(
            results
                .iter()
                .map(|result| result.name.as_str())
                .collect::<Vec<_>>(),
            ["aardvark", "alpha"]
        );
        assert_eq!(
            results
                .iter()
                .map(|result| result.score)
                .collect::<Vec<_>>(),
            [50.0, 50.0]
        );
    }

    #[test]
    fn empty_search_selects_only_the_requested_best_window() {
        let index = SearchIndex::new();
        publish_entries_with_search_signatures(
            &index,
            vec![
                entry("zulu", "/zulu"),
                entry("alpha", "/alpha"),
                entry("apple", "/apple"),
            ],
        );

        let results = index.search("", Some(2));
        assert_eq!(
            results
                .iter()
                .map(|result| result.name.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "apple"]
        );
        assert!(results.iter().all(|result| result.score == 0.0));
    }

    #[test]
    fn ascii_candidate_signatures_preserve_fuzzy_path_unicode_and_content_results() {
        let index = SearchIndex::new();
        let mut content_entry = entry("release-notes.md", "C:/Notes/release-notes.md");
        content_entry.content = Some(indexed_content(
            "private needle phrase for the local body index",
        ));
        publish_entries_with_search_signatures(
            &index,
            vec![
                entry("Roadmap.md", "C:/Workspace/Planning/Roadmap.md"),
                entry("launcher-link", "C:/Tools/Target Console.exe"),
                entry("中文计划.txt", "C:/项目/中文计划.txt"),
                content_entry,
                entry("zzzzzz", "C:/Other/zzzzzz.bin"),
            ],
        );

        let fuzzy_query = ParsedQuery::parse("rdmp");
        let required_term_signatures = fuzzy_query.required_ascii_term_signatures();
        assert!(!required_term_signatures.is_empty());
        let snapshot = index
            .inner
            .entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let signatures = snapshot.search_ascii_signatures();
        assert_eq!(signatures.len(), 5);
        assert!(
            signatures
                .iter()
                .filter(|signature| signature.can_match_all_terms(&required_term_signatures))
                .count()
                < signatures.len(),
            "the fixture must exercise a real candidate rejection"
        );
        drop(snapshot);

        for query in [
            "rdmp",
            "target console",
            "中文计划",
            "zwjh",
            "zhongwenjihua",
            "content:\"needle phrase\"",
            "roadmap path:workspace",
        ] {
            let accelerated = index.search(query, Some(10));

            // Production publication cannot create this state because records
            // and signatures share one write lock. Deliberately corrupt it in
            // this module-private test to prove the defensive length guard
            // takes the established full-scoring path instead of hiding hits.
            let mut snapshot = index
                .inner
                .entries
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            snapshot.clear_search_ascii_signatures_for_test();
            drop(snapshot);
            let fallback = index.search(query, Some(10));
            assert_eq!(
                result_ids(&accelerated),
                result_ids(&fallback),
                "query: {query}"
            );

            let mut snapshot = index
                .inner
                .entries
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let records = snapshot.clone_records_for_test();
            snapshot.replace(records);
        }
    }

    #[test]
    fn structured_filters_narrow_the_path_index_without_hiding_text_search() {
        let index = SearchIndex::new();
        let mut current_markdown = entry("Roadmap.md", "C:/Project Notes/Roadmap.md");
        current_markdown.extension = Some("md".to_owned());
        current_markdown.size_bytes = 256 * 1024;
        current_markdown.modified_at = Some(now_iso());

        let mut old_markdown = entry("Archive.md", "C:/Project Notes/Archive.md");
        old_markdown.extension = Some("md".to_owned());
        old_markdown.size_bytes = 512 * 1024;
        old_markdown.modified_at = Some("2000-01-01T00:00:00+00:00".to_owned());

        let mut image = entry("Roadmap.png", "C:/Project Notes/Roadmap.png");
        image.extension = Some("png".to_owned());
        image.size_bytes = 512 * 1024;
        image.modified_at = Some(now_iso());

        publish_entries_with_search_signatures(&index, vec![old_markdown, image, current_markdown]);

        let filtered = index.search(
            "roadmap path:\"project notes\" ext:md kind:file modified:today size:>100kb",
            Some(10),
        );
        assert_eq!(
            filtered
                .iter()
                .map(|result| result.name.as_str())
                .collect::<Vec<_>>(),
            ["Roadmap.md"]
        );

        let excluded = index.search("ext:md -archive", Some(10));
        assert_eq!(
            excluded
                .iter()
                .map(|result| result.name.as_str())
                .collect::<Vec<_>>(),
            ["Roadmap.md"]
        );
    }

    #[test]
    fn content_queries_are_explicit_and_return_a_local_preview() {
        let index = SearchIndex::new();
        let mut roadmap = entry("Roadmap.md", "C:/Project/Roadmap.md");
        roadmap.metadata = "MD · 2.0 KB".to_owned();
        roadmap.content = Some(indexed_content(
            "Release checklist: blue comet is ready for review.",
        ));
        let readme = entry("README.md", "C:/Project/README.md");
        publish_entries_with_search_signatures(&index, vec![roadmap, readme]);

        // Ordinary launch queries stay filename/path-only. Users must opt in
        // to opening the bounded in-memory body index.
        assert!(index.search("blue comet", Some(10)).is_empty());

        let results = index.search("content:\"blue comet\" ext:md", Some(10));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Roadmap.md");
        assert!(results[0].metadata.contains("正文命中"));
        assert!(results[0].metadata.contains("blue comet"));
    }

    #[test]
    fn content_terms_do_not_match_unindexed_or_binary_entries() {
        let index = SearchIndex::new();
        let mut indexed = entry("notes.txt", "C:/Project/notes.txt");
        indexed.content = Some(indexed_content("本地内容搜索只保留在内存中"));
        let unindexed = entry("private.txt", "C:/Project/private.txt");
        publish_entries_with_search_signatures(&index, vec![indexed, unindexed]);

        assert_eq!(
            index
                .search("content:本地内容", Some(10))
                .iter()
                .map(|result| result.name.as_str())
                .collect::<Vec<_>>(),
            ["notes.txt"]
        );
        assert!(index.search("content:private", Some(10)).is_empty());
    }

    #[test]
    fn stale_content_worker_status_cannot_overwrite_a_newer_revision() {
        let index = SearchIndex::new();
        let generation = index.inner.generation.load(Ordering::SeqCst);
        let first_revision = index.inner.content_revision.fetch_add(1, Ordering::SeqCst) + 1;
        assert!(set_content_status_if_current(
            index.inner.as_ref(),
            Some(generation),
            first_revision,
            "indexing",
            0,
            0,
            Some("new worker is indexing".to_owned()),
        ));

        let newer_revision = index.inner.content_revision.fetch_add(1, Ordering::SeqCst) + 1;
        assert!(!set_content_status_if_current(
            index.inner.as_ref(),
            Some(generation),
            first_revision,
            "ready",
            9,
            99,
            Some("old worker must not win".to_owned()),
        ));
        assert_eq!(index.status().content_status, "indexing");

        assert!(set_content_status_if_current(
            index.inner.as_ref(),
            Some(generation),
            newer_revision,
            "stale",
            0,
            0,
            Some("newer invalidation wins".to_owned()),
        ));
        assert_eq!(index.status().content_status, "stale");
        assert_eq!(index.status().content_indexed_files, 0);
    }

    #[test]
    fn content_invalidation_bumps_revision_and_clears_bodies_before_path_publication() {
        let index = SearchIndex::new();
        let mut entry = entry("notes.md", "C:/Project/notes.md");
        entry.content = Some(indexed_content(
            "old body must not follow a path replacement",
        ));
        publish_entries_with_search_signatures(&index, vec![entry]);
        let previous_revision = index.inner.content_revision.load(Ordering::SeqCst);

        invalidate_content_index(&index.inner, "test invalidation");

        assert_eq!(
            index.inner.content_revision.load(Ordering::SeqCst),
            previous_revision + 1
        );
        let entries = index
            .inner
            .entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(entries.iter().all(|entry| entry.content.is_none()));
        assert_eq!(index.status().content_status, "stale");
    }

    #[test]
    fn persisted_path_snapshot_never_contains_text_bodies() {
        let mut visible = entry("Visible.md", "C:/Project/Visible.md");
        visible.content = Some(indexed_content(
            "do not write this body to the local snapshot",
        ));
        let serialized = serde_json::to_string(&PersistedIndexSnapshotRef {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            roots: vec!["C:/Project".to_owned()],
            last_indexed_at: "2026-01-01T00:00:00+00:00",
            usn_binding: None,
            entries: &[visible],
        })
        .unwrap();
        assert!(!serialized.contains("do not write this body"));

        let restored = serde_json::from_str::<PersistedIndexSnapshotWire>(&serialized).unwrap();
        assert_eq!(restored.entries.len(), 1);
        assert!(restored.entries[0].content.is_none());
    }

    #[test]
    fn text_decoder_supports_utf16_bom_and_rejects_binary_nuls() {
        assert_eq!(
            decode_indexed_text(&[0xff, 0xfe, b'i', 0, b'H', 0, b'u', 0, b'b', 0]),
            Some("iHub".to_owned())
        );
        assert_eq!(decode_indexed_text(b"not\0a text file"), None);
    }

    #[test]
    fn background_content_worker_indexes_only_memory_and_serves_content_queries() {
        let root = unique_test_directory("content-worker");
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("meeting.md");
        std::fs::write(&path, "Roadmap body: local-content-index sentinel").unwrap();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        let entry = indexed_entry_from_path(&path, &metadata).unwrap();
        let index = SearchIndex::new();
        publish_entries_with_search_signatures(&index, vec![entry]);

        schedule_content_index_rebuild(&index.inner, 0);
        for _ in 0..100 {
            if index.status().content_status == "ready" {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        let status = index.status();
        let results = index.search("content:sentinel", Some(10));
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(status.content_status, "ready");
        assert_eq!(status.content_indexed_files, 1);
        assert_eq!(
            results
                .iter()
                .map(|result| result.name.as_str())
                .collect::<Vec<_>>(),
            ["meeting.md"]
        );
    }

    #[test]
    fn local_search_is_case_insensitive_and_prioritizes_filename_matches() {
        let index = SearchIndex::new();
        publish_entries_with_search_signatures(
            &index,
            vec![
                entry("Archive.txt", "C:/Workspace/Readme/Archive.txt"),
                entry("README.md", "C:/Workspace/docs/README.md"),
                entry("Readme backup.txt", "C:/Workspace/old/Readme backup.txt"),
            ],
        );

        // `Readme` intentionally includes an uppercase letter. The matcher
        // must find case variants, then order an exact file stem before a
        // filename prefix and a parent-directory-only match.
        let results = index.search("Readme", Some(10));
        assert_eq!(
            results
                .iter()
                .map(|result| result.name.as_str())
                .collect::<Vec<_>>(),
            ["README.md", "Readme backup.txt", "Archive.txt"]
        );
        assert!(results[0].score > results[1].score);
        assert!(results[1].score > results[2].score);
    }

    #[test]
    fn local_search_matches_full_pinyin_initials_mixed_terms_and_parent_paths() {
        let index = SearchIndex::new();
        publish_entries_with_search_signatures(
            &index,
            vec![
                entry("zwjh.txt", "C:/Literal/zwjh.txt"),
                entry("中文计划.txt", "C:/资料/中文计划.txt"),
                entry("notes.txt", "C:/项目计划/notes.txt"),
                entry("中文纪要.txt", "C:/资料/中文纪要.txt"),
                entry("重庆指南.txt", "C:/资料/重庆指南.txt"),
            ],
        );

        let full = index.search("zhongwenjihua", Some(10));
        assert_eq!(full[0].name, "中文计划.txt");
        assert_eq!(
            index.search("zhongwenjih", Some(10))[0].name,
            "中文计划.txt",
            "an unfinished final syllable remains useful while typing"
        );

        let initials = index.search("zwjh", Some(10));
        assert_eq!(
            initials
                .iter()
                .map(|result| result.name.as_str())
                .collect::<Vec<_>>(),
            ["zwjh.txt", "中文计划.txt"]
        );
        assert!(
            initials[0].score > initials[1].score,
            "a literal filename match must outrank a pinyin alias"
        );

        let mixed = index.search("zhongwen 计划", Some(10));
        assert_eq!(mixed.len(), 1);
        assert_eq!(mixed[0].name, "中文计划.txt");

        let parent_path = index.search("xiangmujihua", Some(10));
        assert_eq!(parent_path.len(), 1);
        assert_eq!(parent_path[0].name, "notes.txt");

        assert_eq!(index.search("chongqing", Some(10))[0].name, "重庆指南.txt");
        assert_eq!(
            index.search("zhongqing", Some(10))[0].name,
            "重庆指南.txt",
            "all dictionary readings are recall candidates"
        );
    }

    #[test]
    fn local_search_normalizes_canonically_equivalent_unicode_names_and_paths() {
        let index = SearchIndex::new();
        publish_entries_with_search_signatures(
            &index,
            vec![
                entry(
                    "Cafe\u{301} Menu.txt",
                    "C:/Cafe\u{301}/Cafe\u{301} Menu.txt",
                ),
                entry("Résumé.md", "C:/Profiles/Résumé.md"),
                entry("ＩＨｕｂ ﬁle.txt", "C:/Profiles/ＩＨｕｂ ﬁle.txt"),
            ],
        );

        assert_eq!(
            index.search("Café Menu", Some(10))[0].name,
            "Cafe\u{301} Menu.txt"
        );
        assert_eq!(
            index.search("Re\u{301}sume\u{301}", Some(10))[0].name,
            "Résumé.md"
        );
        assert_eq!(
            index.search("path:Café menu", Some(10))[0].name,
            "Cafe\u{301} Menu.txt"
        );
        assert_eq!(
            index.search("ihub file", Some(10))[0].name,
            "ＩＨｕｂ ﬁle.txt",
            "NFKC keeps full-width Latin text and compatibility ligatures searchable"
        );
    }

    #[test]
    fn pinyin_signature_is_bounded_memory_only_and_not_serialized() {
        let visible = entry("中文计划.txt", "C:/项目/中文计划.txt");
        let projection = search_ascii_signature(&visible);
        let full = ascii_search_signature_for_text("zhongwenjihua");
        let initials = ascii_search_signature_for_text("zwjh");
        assert_eq!(projection.name & full, full);
        assert_eq!(projection.name & initials, initials);

        let mut overlong_signature = 0;
        add_pinyin_signature(
            &"中".repeat(MAX_PINYIN_NAME_SOURCE_CHARS + 1),
            MAX_PINYIN_NAME_SOURCE_CHARS,
            &mut overlong_signature,
        );
        assert_eq!(
            overlong_signature, ALL_SEARCH_ASCII_SIGNATURE_BITS,
            "overlong synthetic names retain candidates without storing aliases"
        );

        let serialized = serde_json::to_string(&PersistedIndexSnapshotRef {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            roots: vec!["C:/项目".to_owned()],
            last_indexed_at: "2026-01-01T00:00:00+00:00",
            usn_binding: None,
            entries: &[visible],
        })
        .unwrap();
        assert!(!serialized.contains("zhongwenjihua"));
        assert!(!serialized.contains("zwjh"));
    }

    #[test]
    fn local_search_keeps_parent_directory_discovery_as_a_term_fallback() {
        let index = SearchIndex::new();
        publish_entries_with_search_signatures(
            &index,
            vec![entry("Roadmap.md", "C:/Project Notes/Roadmap.md")],
        );

        // `project` exists only in the parent directory while `roadmap` is
        // the visible filename. Filename-first ranking must not make these
        // common multi-term folder searches disappear.
        let results = index.search("project roadmap", Some(10));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Roadmap.md");
    }

    #[test]
    fn simple_fuzzy_query_defers_entry_folding_until_a_candidate_matches() {
        let query = ParsedQuery::parse("needle notes");
        let mut matcher = new_search_matcher();
        let unrelated = entry("Archive.md", "C:/Projects/Archive.md");
        reset_search_text_fold_count();

        assert!(query.score_entry(&mut matcher, &unrelated).is_none());
        assert_eq!(
            search_text_fold_count(),
            0,
            "a simple query must not lowercase name/path text for a fuzzy non-match"
        );

        let matching = entry("Needle Notes.md", "C:/Projects/Needle Notes.md");
        assert!(query.score_entry(&mut matcher, &matching).is_some());
        assert_eq!(
            search_text_fold_count(),
            1,
            "all matching positive terms reuse one folded entry name"
        );

        let filtered = ParsedQuery::parse("needle path:projects -archive");
        reset_search_text_fold_count();
        assert!(filtered.score_entry(&mut matcher, &matching).is_some());
        assert_eq!(
            search_text_fold_count(),
            2,
            "negative/path filters fold each entry name and path once, then reuse the name"
        );
    }

    #[test]
    fn scan_worker_buffers_flush_once_when_the_visitor_finishes() {
        let destination = Arc::new(Mutex::new(Vec::new()));
        {
            let mut buffer = ThreadEntryBuffer::new(Arc::clone(&destination));
            buffer.entries.push(entry("Alpha", "C:/Scan/Alpha"));
            buffer.entries.push(entry("Beta", "C:/Scan/Beta"));
        }
        let chunks = destination
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 2);
    }

    #[test]
    fn parallel_file_scan_merges_all_worker_chunks() {
        let root = unique_test_directory("parallel-scan");
        std::fs::create_dir_all(root.join("nested")).unwrap();
        std::fs::write(root.join("alpha.txt"), b"alpha").unwrap();
        std::fs::write(root.join("nested").join("beta.md"), b"beta").unwrap();
        let index = SearchIndex::new();

        let entries = collect_entries(std::slice::from_ref(&root), &index.inner, 0, 0, true);
        let paths = entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<Vec<_>>();
        let _ = std::fs::remove_dir_all(&root);

        assert!(paths.iter().any(|path| path.ends_with("alpha.txt")));
        assert!(paths.iter().any(|path| path.ends_with("beta.md")));
    }

    #[test]
    fn collection_roots_elide_nested_scopes_without_losing_disjoint_roots() {
        let root = unique_test_directory("collection-root");
        let nested = root.join("nested");
        let sibling = unique_test_directory("collection-sibling");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();

        let selected = collection_roots(&[nested.clone(), sibling.clone(), root.clone()]);

        assert_eq!(selected.len(), 2);
        assert!(selected.iter().any(|path| path == &root));
        assert!(selected.iter().any(|path| path == &sibling));
        assert!(!selected.iter().any(|path| path == &nested));

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&sibling);
    }

    #[test]
    fn watcher_debounce_coalesces_bursts_without_starving_a_busy_root() {
        let start = Instant::now();
        let mut pending = Some(PendingWatchRebuild::new(start));
        record_watched_full_rebuild(&mut pending, start + Duration::from_millis(500));
        let pending_after_first_write = pending.as_ref().expect("a change must schedule a refresh");
        assert_eq!(
            pending_after_first_write.deadline.duration_since(start),
            Duration::from_millis(1_150)
        );

        // Continuous writes keep extending the quiet-period debounce, but no
        // further than the hard batch deadline from the first event.
        record_watched_full_rebuild(&mut pending, start + Duration::from_millis(4_900));
        let pending_after_burst = pending.as_ref().expect("the batch remains pending");
        assert_eq!(
            pending_after_burst.deadline.duration_since(start),
            WATCH_MAX_BATCH_DELAY
        );
        assert!(pending_after_burst.is_due(start + WATCH_MAX_BATCH_DELAY));
    }

    #[test]
    fn incremental_snapshot_debounce_avoids_serializing_every_event_batch() {
        let start = Instant::now();
        let mut pending = None;

        record_incremental_snapshot(&mut pending, start);
        let pending_after_first_change = pending.as_ref().expect("a snapshot must be scheduled");
        assert_eq!(
            pending_after_first_change.deadline.duration_since(start),
            INCREMENTAL_SNAPSHOT_DEBOUNCE
        );

        record_incremental_snapshot(&mut pending, start + Duration::from_secs(1));
        let pending_after_second_change = pending.as_ref().expect("the snapshot remains pending");
        assert_eq!(
            pending_after_second_change.deadline.duration_since(start),
            Duration::from_secs(3)
        );

        record_incremental_snapshot(&mut pending, start + Duration::from_secs(11));
        let pending_after_busy_period = pending.as_ref().expect("the snapshot remains pending");
        assert_eq!(
            pending_after_busy_period.deadline.duration_since(start),
            INCREMENTAL_SNAPSHOT_MAX_DELAY
        );

        let pending = pending.as_mut().expect("the snapshot remains pending");
        pending.retry_after_write_failure(start + INCREMENTAL_SNAPSHOT_MAX_DELAY);
        assert_eq!(
            pending.deadline.duration_since(start),
            INCREMENTAL_SNAPSHOT_MAX_DELAY + INCREMENTAL_SNAPSHOT_RETRY_DELAY
        );
    }

    #[test]
    fn watcher_events_are_filtered_to_the_authorized_root_before_rebuilding() {
        let root = unique_test_directory("watch-root");
        let sibling = root.with_file_name("ihub-indexer-watch-root-sibling");
        let nested = root.join("nested").join("changed.md");
        let outside = sibling.join("private.md");
        let roots = vec![root.clone()];

        let in_scope = Event::new(EventKind::Any).add_path(nested);
        let out_of_scope = Event::new(EventKind::Any).add_path(outside);
        let access_only = Event::new(EventKind::Access(notify::event::AccessKind::Read))
            .add_path(root.join("opened.txt"));
        let mut access_with_rescan = Event::new(EventKind::Access(notify::event::AccessKind::Read))
            .add_path(root.join("rescan.txt"));
        access_with_rescan
            .attrs
            .set_flag(notify::event::Flag::Rescan);
        let pathless_rescan_hint = Event::new(EventKind::Other);

        assert!(watch_event_affects_roots(&in_scope, &roots));
        assert!(!watch_event_affects_roots(&out_of_scope, &roots));
        assert!(watch_event_affects_roots(&access_only, &roots));
        assert!(!watch_event_requires_refresh(&access_only));
        assert!(watch_event_requires_refresh(&access_with_rescan));
        assert!(watch_event_affects_roots(&pathless_rescan_hint, &roots));
    }

    #[test]
    fn watcher_batches_concrete_paths_but_escalates_root_boundary_events() {
        let root = unique_test_directory("watch-batch");
        std::fs::create_dir_all(root.join("nested")).unwrap();
        let root = root.canonicalize().unwrap();
        let roots = vec![root.clone()];
        let changed = root.join("nested").join("changed.md");
        let mut pending = None;

        record_watched_event(
            &mut pending,
            &Event::new(EventKind::Any).add_path(changed.clone()),
            &roots,
            &[],
            Instant::now(),
        );
        let pending_batch = pending.as_ref().expect("in-scope changes must be buffered");
        assert!(!pending_batch.requires_full_rebuild);
        assert!(pending_batch.changed_paths.contains(&changed));

        record_watched_event(
            &mut pending,
            &Event::new(EventKind::Any).add_path(root.clone()),
            &roots,
            &[],
            Instant::now(),
        );
        let pending_batch = pending.as_ref().expect("the batch remains pending");
        assert!(pending_batch.requires_full_rebuild);
        assert!(pending_batch.changed_paths.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn incremental_reconciliation_handles_rename_and_delete_without_full_scan() {
        let root = unique_test_directory("incremental-rename");
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let roots = vec![root.clone()];
        let old_path = root.join("before.txt");
        let new_path = root.join("after.md");
        std::fs::write(&old_path, b"before").unwrap();

        let index = SearchIndex::new();
        *index
            .inner
            .configured_roots
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            RootSelection::Custom(roots.clone());
        let old_entry =
            indexed_entry_from_path(&old_path, &std::fs::symlink_metadata(&old_path).unwrap())
                .expect("a regular file must be indexable");
        publish_entries_with_search_signatures(&index, vec![old_entry]);

        std::fs::rename(&old_path, &new_path).unwrap();
        let old_display = old_path.to_string_lossy().to_string();
        let new_display = new_path.to_string_lossy().to_string();
        let renamed = index
            .reconcile_watched_paths(&roots, &HashSet::from([old_path.clone(), new_path.clone()]));
        assert_eq!(renamed, WatchedIncrementalDecision::Applied);
        let entries = index
            .inner
            .entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(entries.iter().any(|entry| entry.path == new_display));
        assert!(!entries.iter().any(|entry| entry.path == old_display));
        drop(entries);

        std::fs::remove_file(&new_path).unwrap();
        let removed = index.reconcile_watched_paths(&roots, &HashSet::from([new_path.clone()]));
        assert_eq!(removed, WatchedIncrementalDecision::Applied);
        assert!(index
            .inner
            .entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .all(|entry| entry.path != new_display));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn incremental_reconciliation_replaces_a_changed_directory_subtree() {
        let root = unique_test_directory("incremental-directory");
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("alpha.txt"), b"alpha").unwrap();
        let root = root.canonicalize().unwrap();
        let nested = root.join("nested");
        let roots = vec![root.clone()];
        let index = SearchIndex::new();
        *index
            .inner
            .configured_roots
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            RootSelection::Custom(roots.clone());

        let outcome = index.reconcile_watched_paths(&roots, &HashSet::from([nested.clone()]));
        assert_eq!(outcome, WatchedIncrementalDecision::Applied);
        let indexed_paths = index
            .inner
            .entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        assert!(indexed_paths.iter().any(|path| path.ends_with("nested")));
        assert!(indexed_paths.iter().any(|path| path.ends_with("alpha.txt")));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn watcher_cannot_trigger_a_scan_after_the_root_scope_changes() {
        let active_root = unique_test_directory("watch-active-root");
        let stale_root = unique_test_directory("watch-stale-root");
        let index = SearchIndex::new();
        *index
            .inner
            .configured_roots
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            RootSelection::Custom(vec![active_root]);

        assert_eq!(
            index.rebuild_from_watched_scope(&[stale_root]),
            WatchedRebuildDecision::DiscardedForDifferentScope
        );
        assert_eq!(index.inner.generation.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn watch_registration_status_exposes_partial_failures_without_hiding_search() {
        let index = SearchIndex::new();
        let configured = vec![PathBuf::from("C:/One"), PathBuf::from("C:/Two")];
        update_watch_registration_status(
            &index.inner,
            &configured,
            WatchRegistration {
                watched: 1,
                first_error: Some("Cannot watch C:/Two".to_owned()),
            },
        );
        let status = index.status();
        assert_eq!(status.watch_status, "degraded");
        assert_eq!(status.watch_message.as_deref(), Some("Cannot watch C:/Two"));
    }

    #[test]
    fn complete_snapshot_is_restored_and_replaced_safely() {
        let storage = unique_test_directory("snapshot-storage");
        let indexed_root = unique_test_directory("snapshot-root");
        std::fs::create_dir_all(&storage).unwrap();
        std::fs::create_dir_all(&indexed_root).unwrap();
        let snapshot_path = storage.join(SNAPSHOT_FILE_NAME);
        let roots = vec![indexed_root.canonicalize().unwrap()];
        let first_path = roots[0].join("First.md").to_string_lossy().into_owned();
        let first = entry("First.md", &first_path);
        let timestamp = now_iso();

        persist_roots(&storage.join(ROOTS_FILE_NAME), &roots).unwrap();
        persist_snapshot(&snapshot_path, &roots, &timestamp, &[first], None).unwrap();
        let restored = SearchIndex::with_storage(storage.clone());
        assert_eq!(restored.status().phase, "ready");
        assert_eq!(
            restored.status().last_indexed_at.as_deref(),
            Some(timestamp.as_str())
        );
        assert_eq!(
            restored
                .search("first", Some(10))
                .iter()
                .map(|result| result.name.as_str())
                .collect::<Vec<_>>(),
            ["First.md"]
        );

        let second_path = roots[0].join("Second.md").to_string_lossy().into_owned();
        let second = entry("Second.md", &second_path);
        persist_snapshot(&snapshot_path, &roots, &now_iso(), &[second], None).unwrap();
        let replaced = SearchIndex::with_storage(storage.clone());
        let replaced_entries = replaced
            .inner
            .entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone_records_for_test();
        assert!(replaced_entries
            .iter()
            .any(|entry| entry.path == second_path && entry.name == "Second.md"));
        assert!(replaced_entries
            .iter()
            .all(|entry| entry.path != first_path));
        assert!(std::fs::read_dir(&storage)
            .unwrap()
            .filter_map(Result::ok)
            .all(|entry| !entry.file_name().to_string_lossy().ends_with(".tmp")));

        let _ = std::fs::remove_dir_all(&storage);
        let _ = std::fs::remove_dir_all(&indexed_root);
    }

    #[test]
    fn corrupted_snapshot_falls_back_to_an_empty_restart_index() {
        let storage = unique_test_directory("snapshot-corrupt-storage");
        let indexed_root = unique_test_directory("snapshot-corrupt-root");
        std::fs::create_dir_all(&storage).unwrap();
        std::fs::create_dir_all(&indexed_root).unwrap();
        let indexed_root = indexed_root.canonicalize().unwrap();
        persist_roots(
            &storage.join(ROOTS_FILE_NAME),
            std::slice::from_ref(&indexed_root),
        )
        .unwrap();

        // This models a manually interrupted/externally damaged cache. A
        // normal atomic write cannot produce it, but startup must still avoid
        // exposing any stale result from a malformed state file.
        std::fs::write(storage.join(SNAPSHOT_FILE_NAME), b"{\"entries\":[").unwrap();

        let restored = SearchIndex::with_storage(storage.clone());
        assert_eq!(restored.status().phase, "idle");
        assert!(restored.search("anything", Some(10)).is_empty());

        let _ = std::fs::remove_dir_all(&storage);
        let _ = std::fs::remove_dir_all(&indexed_root);
    }

    #[test]
    fn stale_snapshot_is_rejected_before_restart_cache_reuse() {
        let storage = unique_test_directory("snapshot-stale-storage");
        let indexed_root = unique_test_directory("snapshot-stale-root");
        std::fs::create_dir_all(&storage).unwrap();
        std::fs::create_dir_all(&indexed_root).unwrap();
        let indexed_root = indexed_root.canonicalize().unwrap();
        let cached_path = indexed_root.join("Cached.md");
        std::fs::write(&cached_path, b"cache source").unwrap();
        persist_roots(
            &storage.join(ROOTS_FILE_NAME),
            std::slice::from_ref(&indexed_root),
        )
        .unwrap();

        let stale_at = (Utc::now() - ChronoDuration::days(MAX_SNAPSHOT_AGE_DAYS + 1)).to_rfc3339();
        assert!(persist_snapshot(
            &storage.join(SNAPSHOT_FILE_NAME),
            std::slice::from_ref(&indexed_root),
            &stale_at,
            &[entry("Cached.md", &cached_path.to_string_lossy())],
            None,
        )
        .is_err());

        // Bypass the writer to prove the read side also rejects a stale file
        // left behind by an older build or another process.
        let stale_entry = entry("Cached.md", &cached_path.to_string_lossy());
        let payload = PersistedIndexSnapshotRef {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            roots: vec![indexed_root.to_string_lossy().to_string()],
            last_indexed_at: &stale_at,
            usn_binding: None,
            entries: &[stale_entry],
        };
        std::fs::write(
            storage.join(SNAPSHOT_FILE_NAME),
            serde_json::to_vec(&payload).unwrap(),
        )
        .unwrap();

        let restored = SearchIndex::with_storage(storage.clone());
        assert_eq!(restored.status().phase, "idle");
        assert!(restored.search("cached", Some(10)).is_empty());

        let _ = std::fs::remove_dir_all(&storage);
        let _ = std::fs::remove_dir_all(&indexed_root);
    }

    #[test]
    fn replay_snapshot_binding_requires_matching_scope_schema_and_payload() {
        let (roots, entries, pairs, seeds) = complete_replay_projection();
        let binding = build_usn_snapshot_binding(&roots, &entries, &pairs, &seeds)
            .expect("a complete MFT identity projection should bind to its exact scope");
        assert!(snapshot_usn_binding_matches_scope(&binding, &roots));

        let different_scope = vec![PathBuf::from(r"D:\")];
        assert!(!snapshot_usn_binding_matches_scope(
            &binding,
            &different_scope
        ));

        let mut wrong_schema = binding.clone();
        wrong_schema.schema_version += 1;
        assert!(!snapshot_usn_binding_matches_scope(&wrong_schema, &roots));

        let mut missing_baseline = binding;
        missing_baseline.replay.checkpoints.clear();
        assert!(!snapshot_usn_binding_matches_scope(
            &missing_baseline,
            &roots
        ));
    }

    #[test]
    fn duplicate_snapshot_entries_fail_closed_instead_of_retaining_a_repaired_subset() {
        let storage = unique_test_directory("snapshot-duplicate-storage");
        let indexed_root = unique_test_directory("snapshot-duplicate-root");
        std::fs::create_dir_all(&storage).unwrap();
        std::fs::create_dir_all(&indexed_root).unwrap();
        let indexed_root = indexed_root.canonicalize().unwrap();
        let roots = vec![indexed_root.clone()];
        let duplicated_path = indexed_root.join("Duplicate.md");
        std::fs::write(&duplicated_path, b"duplicate").unwrap();
        let duplicated_path = duplicated_path.to_string_lossy().into_owned();
        let duplicated_entry = entry("Duplicate.md", &duplicated_path);
        let snapshot_path = storage.join(SNAPSHOT_FILE_NAME);
        let duplicated_entries = vec![duplicated_entry.clone(), duplicated_entry];

        persist_snapshot(
            &snapshot_path,
            &roots,
            &now_iso(),
            &duplicated_entries,
            None,
        )
        .unwrap();
        let managed_state_roots = managed_state_roots(&[Some(snapshot_path.as_path())]);
        assert!(load_persisted_snapshot(&snapshot_path, &managed_state_roots, &roots).is_none());
        let _ = std::fs::remove_dir_all(&storage);
        let _ = std::fs::remove_dir_all(&indexed_root);
    }

    #[cfg(windows)]
    #[test]
    fn malformed_optional_replay_binding_keeps_ordinary_snapshot_entries() {
        let storage = unique_test_directory("snapshot-malformed-binding-storage");
        std::fs::create_dir_all(&storage).unwrap();
        let (roots, entries, pairs, seeds) = complete_replay_projection();
        let binding = build_usn_snapshot_binding(&roots, &entries, &pairs, &seeds).unwrap();
        let snapshot_path = storage.join(SNAPSHOT_FILE_NAME);
        let timestamp = now_iso();
        let mut payload = serde_json::to_value(PersistedIndexSnapshotRef {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            roots: roots
                .iter()
                .map(|root| root.to_string_lossy().to_string())
                .collect(),
            last_indexed_at: &timestamp,
            usn_binding: Some(&binding),
            entries: &entries,
        })
        .unwrap();
        payload
            .get_mut("usnBinding")
            .and_then(serde_json::Value::as_object_mut)
            .unwrap()
            .insert("unexpectedField".to_owned(), serde_json::Value::Bool(true));
        std::fs::write(&snapshot_path, serde_json::to_vec(&payload).unwrap()).unwrap();

        let state_roots = managed_state_roots(&[Some(snapshot_path.as_path())]);
        let restored = load_persisted_snapshot(&snapshot_path, &state_roots, &roots).unwrap();
        assert_eq!(restored.entries.len(), entries.len());
        assert!(restored.usn_binding.is_none());
        let _ = std::fs::remove_dir_all(&storage);
    }

    #[cfg(windows)]
    #[test]
    fn same_length_snapshot_path_change_drops_the_replay_binding() {
        let storage = unique_test_directory("snapshot-replay-path-mismatch-storage");
        std::fs::create_dir_all(&storage).unwrap();
        let (roots, mut entries, pairs, seeds) = complete_replay_projection();
        let binding = build_usn_snapshot_binding(&roots, &entries, &pairs, &seeds).unwrap();
        let changed = entries
            .iter_mut()
            .find(|entry| entry.path == r"C:\Projects\notes.md")
            .unwrap();
        changed.path = r"C:\Projects\other.md".to_owned();
        changed.id = changed.path.clone();
        changed.name = "other.md".to_owned();
        let snapshot_path = storage.join(SNAPSHOT_FILE_NAME);
        persist_snapshot(&snapshot_path, &roots, &now_iso(), &entries, Some(&binding)).unwrap();

        let state_roots = managed_state_roots(&[Some(snapshot_path.as_path())]);
        let restored = load_persisted_snapshot(&snapshot_path, &state_roots, &roots).unwrap();
        assert_eq!(restored.entries.len(), entries.len());
        assert!(restored.usn_binding.is_none());
        let _ = std::fs::remove_dir_all(&storage);
    }

    #[test]
    fn v2_snapshot_is_not_accepted_as_a_v3_cache() {
        let storage = unique_test_directory("snapshot-v2-cache-storage");
        std::fs::create_dir_all(&storage).unwrap();
        let snapshot_path = storage.join(SNAPSHOT_FILE_NAME);
        let (roots, entries, _, _) = complete_replay_projection();
        let timestamp = now_iso();
        let payload = PersistedIndexSnapshotRef {
            schema_version: 2,
            roots: roots
                .iter()
                .map(|root| root.to_string_lossy().to_string())
                .collect(),
            last_indexed_at: &timestamp,
            usn_binding: None,
            entries: &entries,
        };
        std::fs::write(&snapshot_path, serde_json::to_vec(&payload).unwrap()).unwrap();

        let state_roots = managed_state_roots(&[Some(snapshot_path.as_path())]);
        assert!(load_persisted_snapshot(&snapshot_path, &state_roots, &roots).is_none());
        let _ = std::fs::remove_dir_all(&storage);
    }

    #[test]
    fn legacy_v2_entries_fall_back_without_ever_restoring_its_binding() {
        let storage = unique_test_directory("legacy-v2-cache-storage");
        let indexed_root = unique_test_directory("legacy-v2-cache-root");
        std::fs::create_dir_all(&storage).unwrap();
        std::fs::create_dir_all(&indexed_root).unwrap();
        let indexed_root = indexed_root.canonicalize().unwrap();
        let roots = vec![indexed_root.clone()];
        persist_roots(&storage.join(ROOTS_FILE_NAME), &roots).unwrap();

        let legacy_entry = entry(
            "Legacy.md",
            &indexed_root.join("Legacy.md").to_string_lossy(),
        );
        let timestamp = now_iso();
        let mut legacy_payload = serde_json::to_value(PersistedIndexSnapshotRef {
            schema_version: LEGACY_SNAPSHOT_SCHEMA_VERSION,
            roots: roots
                .iter()
                .map(|root| root.to_string_lossy().to_string())
                .collect(),
            last_indexed_at: &timestamp,
            usn_binding: None,
            entries: &[legacy_entry],
        })
        .unwrap();
        // This is intentionally malformed for the v3 binding schema. The
        // v2 fallback wire type has no binding field and must skip it rather
        // than deserialize or reinterpret this legacy data.
        legacy_payload
            .as_object_mut()
            .unwrap()
            .insert("usnBinding".to_owned(), serde_json::json!({"old": true}));
        std::fs::write(
            storage.join(LEGACY_SNAPSHOT_FILE_NAME),
            serde_json::to_vec(&legacy_payload).unwrap(),
        )
        .unwrap();

        let restored_legacy = SearchIndex::with_storage(storage.clone());
        assert_eq!(
            restored_legacy
                .search("legacy", Some(10))
                .iter()
                .map(|result| result.name.as_str())
                .collect::<Vec<_>>(),
            ["Legacy.md"]
        );
        assert!(restored_legacy
            .inner
            .startup_usn_binding
            .lock()
            .unwrap()
            .is_none());

        let v3_entry = entry("V3.md", &indexed_root.join("V3.md").to_string_lossy());
        persist_snapshot(
            &storage.join(SNAPSHOT_FILE_NAME),
            &roots,
            &now_iso(),
            &[v3_entry],
            None,
        )
        .unwrap();
        let restored_v3 = SearchIndex::with_storage(storage.clone());
        assert_eq!(
            restored_v3
                .search("v3", Some(10))
                .iter()
                .map(|result| result.name.as_str())
                .collect::<Vec<_>>(),
            ["V3.md"]
        );
        assert!(restored_v3
            .inner
            .entries
            .read()
            .unwrap()
            .iter()
            .all(|entry| entry.name != "Legacy.md"));

        let _ = std::fs::remove_dir_all(&storage);
        let _ = std::fs::remove_dir_all(&indexed_root);
    }

    #[test]
    fn replay_binding_builder_rejects_ambiguous_directories_and_missing_parent_chains() {
        let (roots, mut entries, mut pairs, seeds) = complete_replay_projection();
        let aliased_directory = mft_indexed_pair(r"C:\Alias", 10, 5, "Alias", true, false);
        entries.push(aliased_directory.entry.clone());
        pairs.push(aliased_directory);
        let error = build_usn_snapshot_binding(&roots, &entries, &pairs, &seeds).unwrap_err();
        assert!(error.contains("目录引用"));

        let (roots, mut entries, mut pairs, seeds) = complete_replay_projection();
        entries.retain(|entry| entry.path != r"C:\Projects");
        pairs.retain(|pair| pair.entry.path != r"C:\Projects");
        let error = build_usn_snapshot_binding(&roots, &entries, &pairs, &seeds).unwrap_err();
        assert!(error.contains("父目录链"));
    }

    #[test]
    fn watcher_ignores_i_hub_owned_state_paths_inside_a_user_root() {
        let root = unique_test_directory("watch-internal-state-root");
        let state_root = root.join(".ihub-state");
        std::fs::create_dir_all(&state_root).unwrap();
        let root = root.canonicalize().unwrap();
        let state_root = state_root.canonicalize().unwrap();
        let mut pending = None;

        record_watched_event(
            &mut pending,
            &Event::new(EventKind::Modify(notify::event::ModifyKind::Any))
                .add_path(state_root.join(SNAPSHOT_FILE_NAME)),
            std::slice::from_ref(&root),
            std::slice::from_ref(&state_root),
            Instant::now(),
        );

        assert!(pending.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn zero_change_fast_start_requires_state_outside_authorized_roots() {
        let storage = unique_test_directory("zero-change-state-storage");
        std::fs::create_dir_all(&storage).unwrap();
        let storage = storage.canonicalize().unwrap();
        let index =
            SearchIndex::with_storage_paths(Some(storage.join(SNAPSHOT_FILE_NAME)), None, None);

        assert!(!zero_change_storage_is_external(
            &index.inner,
            std::slice::from_ref(&storage)
        ));
        let external_root = storage.with_file_name("ihub-indexer-external-root");
        assert!(zero_change_storage_is_external(
            &index.inner,
            &[external_root]
        ));
        let _ = std::fs::remove_dir_all(&storage);
    }

    #[test]
    fn snapshot_with_out_of_scope_content_fails_closed_without_exposing_a_subset() {
        let storage = unique_test_directory("snapshot-scope-storage");
        let indexed_root = unique_test_directory("snapshot-scope-root");
        let outside_root = unique_test_directory("snapshot-scope-outside");
        std::fs::create_dir_all(&storage).unwrap();
        std::fs::create_dir_all(&indexed_root).unwrap();
        std::fs::create_dir_all(&outside_root).unwrap();
        let indexed_root = indexed_root.canonicalize().unwrap();
        let outside_root = outside_root.canonicalize().unwrap();

        persist_roots(
            &storage.join(ROOTS_FILE_NAME),
            std::slice::from_ref(&indexed_root),
        )
        .unwrap();
        persist_snapshot(
            &storage.join(SNAPSHOT_FILE_NAME),
            std::slice::from_ref(&indexed_root),
            &now_iso(),
            &[
                entry(
                    "Visible.md",
                    &indexed_root.join("Visible.md").to_string_lossy(),
                ),
                entry(
                    "Private.md",
                    &outside_root.join("Private.md").to_string_lossy(),
                ),
            ],
            None,
        )
        .unwrap();

        let restored = SearchIndex::with_storage(storage.clone());
        assert_eq!(restored.status().phase, "idle");
        assert!(restored.search("visible", Some(10)).is_empty());
        assert!(restored.search("private", Some(10)).is_empty());

        let _ = std::fs::remove_dir_all(&storage);
        let _ = std::fs::remove_dir_all(&indexed_root);
        let _ = std::fs::remove_dir_all(&outside_root);
    }

    #[test]
    fn configured_index_roots_are_persisted_and_restored() {
        let storage = unique_test_directory("configured-roots-storage");
        let root = unique_test_directory("configured-root");
        std::fs::create_dir_all(&storage).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let canonical_root = root.canonicalize().unwrap();
        let roots_path = storage.join(ROOTS_FILE_NAME);

        persist_roots(&roots_path, std::slice::from_ref(&canonical_root)).unwrap();
        let restored = SearchIndex::with_storage(storage.clone());

        assert_eq!(
            restored.status().roots,
            vec![canonical_root.to_string_lossy().to_string()]
        );
        assert_eq!(
            load_persisted_roots(&roots_path),
            RootSelection::Custom(vec![canonical_root])
        );

        let _ = std::fs::remove_dir_all(&storage);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn malformed_present_root_configuration_fails_closed_without_default_scope_fallback() {
        let storage = unique_test_directory("roots-malformed-storage");
        std::fs::create_dir_all(&storage).unwrap();
        let roots_path = storage.join(ROOTS_FILE_NAME);
        std::fs::write(&roots_path, b"{not valid json").unwrap();

        assert_eq!(
            load_persisted_roots(&roots_path),
            RootSelection::Unavailable
        );
        let restored = SearchIndex::with_storage(storage.clone());
        assert_eq!(restored.status().phase, "error");
        assert!(restored.status().roots.is_empty());

        let _ = std::fs::remove_dir_all(&storage);
    }

    #[test]
    fn setting_index_roots_updates_the_active_scope_and_disk_configuration() {
        let storage = unique_test_directory("set-roots-storage");
        let root = unique_test_directory("set-roots-root");
        std::fs::create_dir_all(&storage).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("readme.md"), b"iHub").unwrap();
        let canonical_root = root.canonicalize().unwrap();
        let index = SearchIndex::with_storage(storage.clone());

        let status = index
            .set_roots(vec![root.to_string_lossy().to_string()])
            .unwrap();

        assert_eq!(
            status.roots,
            vec![canonical_root.to_string_lossy().to_string()]
        );
        assert_eq!(
            load_persisted_roots(&storage.join(ROOTS_FILE_NAME)),
            RootSelection::Custom(vec![canonical_root])
        );

        // The background scanner is intentionally allowed to finish or observe
        // the cleanup as an empty root; either result is safe and does not
        // mutate files outside this test-owned directory.
        let _ = std::fs::remove_dir_all(&storage);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn changing_index_roots_discards_entries_outside_the_new_scope() {
        let old_root = unique_test_directory("old-scope");
        let new_root = unique_test_directory("new-scope");
        std::fs::create_dir_all(&old_root).unwrap();
        std::fs::create_dir_all(&new_root).unwrap();
        let old_path = old_root.join("private.txt");
        let new_path = new_root.join("keep.txt");
        std::fs::write(&old_path, b"old").unwrap();
        std::fs::write(&new_path, b"new").unwrap();
        let old_path = old_path.canonicalize().unwrap();
        let new_path = new_path.canonicalize().unwrap();
        let old_path_text = old_path.to_string_lossy().into_owned();
        let new_path_text = new_path.to_string_lossy().into_owned();
        let index = SearchIndex::new();
        publish_entries_with_search_signatures(
            &index,
            vec![
                entry("private.txt", &old_path_text),
                entry("keep.txt", &new_path_text),
            ],
        );

        index
            .set_roots(vec![new_root.to_string_lossy().to_string()])
            .unwrap();

        let current_entries = index
            .inner
            .entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone_records_for_test();
        assert!(current_entries
            .iter()
            .all(|entry| entry.path != old_path_text));
        assert!(current_entries
            .iter()
            .any(|entry| entry.path == new_path_text && entry.name == "keep.txt"));

        let _ = std::fs::remove_dir_all(&old_root);
        let _ = std::fs::remove_dir_all(&new_root);
    }

    #[test]
    fn snapshot_from_a_removed_root_is_not_restored_for_new_configured_scope() {
        let storage = unique_test_directory("scope-mismatch-storage");
        let old_root = unique_test_directory("scope-mismatch-old");
        let new_root = unique_test_directory("scope-mismatch-new");
        std::fs::create_dir_all(&storage).unwrap();
        std::fs::create_dir_all(&old_root).unwrap();
        std::fs::create_dir_all(&new_root).unwrap();
        let old_root = old_root.canonicalize().unwrap();
        let new_root = new_root.canonicalize().unwrap();
        let snapshot_path = storage.join(SNAPSHOT_FILE_NAME);

        persist_roots(
            &storage.join(ROOTS_FILE_NAME),
            std::slice::from_ref(&new_root),
        )
        .unwrap();
        persist_snapshot(
            &snapshot_path,
            std::slice::from_ref(&old_root),
            &now_iso(),
            &[entry("Private.md", "C:/Removed/Private.md")],
            None,
        )
        .unwrap();

        let restored = SearchIndex::with_storage(storage.clone());
        assert_eq!(restored.status().phase, "idle");
        assert!(restored.search("private", Some(10)).is_empty());

        let _ = std::fs::remove_dir_all(&storage);
        let _ = std::fs::remove_dir_all(&old_root);
        let _ = std::fs::remove_dir_all(&new_root);
    }

    #[test]
    fn unavailable_saved_custom_roots_fail_closed_without_restoring_a_snapshot() {
        let storage = unique_test_directory("unavailable-roots-storage");
        std::fs::create_dir_all(&storage).unwrap();
        let unavailable = unique_test_directory("unavailable-roots-target");
        let snapshot_path = storage.join(SNAPSHOT_FILE_NAME);

        // `persist_roots` deliberately writes the existing configuration as
        // it was chosen. The path disappearing afterwards must not make the
        // next startup fall back to the user's default folders.
        persist_roots(&storage.join(ROOTS_FILE_NAME), &[unavailable]).unwrap();
        persist_snapshot(
            &snapshot_path,
            &[],
            &now_iso(),
            &[entry("DefaultSnapshot.md", "C:/Default/DefaultSnapshot.md")],
            None,
        )
        .unwrap();

        let restored = SearchIndex::with_storage(storage.clone());
        assert_eq!(restored.status().phase, "error");
        assert!(restored.status().roots.is_empty());
        assert!(restored.search("defaultsnapshot", Some(10)).is_empty());

        let _ = std::fs::remove_dir_all(&storage);
    }

    #[test]
    fn configured_index_roots_require_absolute_existing_directories() {
        let error = normalize_configured_roots(vec!["relative-folder".to_owned()]).unwrap_err();
        assert!(error.contains("absolute path"));

        let missing = std::env::temp_dir().join("ihub-indexer-this-directory-does-not-exist");
        let error =
            normalize_configured_roots(vec![missing.to_string_lossy().to_string()]).unwrap_err();
        assert!(error.contains("Could not resolve index folder"));
    }

    #[test]
    fn start_menu_discovery_reads_only_existing_launchable_shortcuts() {
        let root = unique_test_directory("start-menu");
        std::fs::create_dir_all(root.join("Developer")).unwrap();
        std::fs::write(root.join("Developer").join("Code.lnk"), b"shortcut").unwrap();
        std::fs::write(root.join("Browser.URL"), b"internet shortcut").unwrap();
        std::fs::write(root.join("readme.txt"), b"not launchable").unwrap();
        std::fs::create_dir_all(root.join("not-a-shortcut.lnk")).unwrap();

        let applications = collect_start_menu_entries(
            &[ApplicationRoot {
                path: root.clone(),
                metadata: "测试开始菜单",
            }],
            10,
        );
        let paths_are_real = applications
            .iter()
            .all(|application| Path::new(&application.path).is_file());
        let names = applications
            .iter()
            .map(|application| application.name.as_str())
            .collect::<Vec<_>>();
        let kinds_are_applications = applications
            .iter()
            .all(|application| application.kind == "application");
        let metadata_is_preserved = applications
            .iter()
            .all(|application| application.metadata == "测试开始菜单");
        let _ = std::fs::remove_dir_all(&root);

        assert_eq!(names, ["Browser", "Code"]);
        assert!(paths_are_real);
        assert!(kinds_are_applications);
        assert!(metadata_is_preserved);
    }

    #[test]
    fn macos_application_bundle_shape_matches_apps_and_preference_panes() {
        assert!(is_macos_app_bundle(Path::new("/Applications/Example.app")));
        assert!(is_macos_app_bundle(Path::new(
            "/System/Library/PreferencePanes/Displays.prefPane"
        )));
        assert!(is_macos_app_bundle(Path::new(
            "/Library/PreferencePanes/Custom.PREFPANE"
        )));
        assert!(!is_macos_app_bundle(Path::new("/Applications/Archive.zip")));
    }

    #[test]
    fn mutable_link_documents_are_not_eligible_for_persistent_launcher_pins() {
        assert!(!file_is_launcher_shortcut_eligible(Path::new(
            "C:/Projects/Tool.lnk"
        )));
        assert!(!file_is_launcher_shortcut_eligible(Path::new(
            "C:/Projects/Website.url"
        )));
        assert!(!file_is_launcher_shortcut_eligible(Path::new(
            "C:/Projects/App.appref-ms"
        )));
        assert!(file_is_launcher_shortcut_eligible(Path::new(
            "C:/Projects/Notes.md"
        )));
    }

    #[test]
    fn start_menu_discovery_honors_its_bounded_result_limit() {
        let root = unique_test_directory("start-menu-limit");
        std::fs::create_dir_all(&root).unwrap();
        for name in ["Alpha.lnk", "Bravo.lnk", "Charlie.lnk"] {
            std::fs::write(root.join(name), b"shortcut").unwrap();
        }

        let applications = collect_start_menu_entries(
            &[ApplicationRoot {
                path: root.clone(),
                metadata: "测试开始菜单",
            }],
            2,
        );
        let names = applications
            .iter()
            .map(|application| application.name.as_str())
            .collect::<Vec<_>>();
        let _ = std::fs::remove_dir_all(&root);

        assert_eq!(names, ["Alpha", "Bravo"]);
    }

    #[test]
    fn launcher_application_entry_wins_over_an_equivalent_content_entry() {
        let mut entries = vec![
            entry("Code.lnk", "C:/Programs/Code.lnk"),
            IndexedEntry {
                id: "application:C:/Programs/Code.lnk".to_owned(),
                path: "C:/Programs/Code.lnk".to_owned(),
                name: "Code".to_owned(),
                kind: "application".to_owned(),
                metadata: "开始菜单 · 当前用户".to_owned(),
                modified_at: None,
                extension: Some("lnk".to_owned()),
                size_bytes: 0,
                content: None,
            },
        ];

        sort_and_deduplicate_entries(&mut entries);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].kind, "application");
        assert_eq!(entries[0].name, "Code");
    }

    #[test]
    fn incremental_merge_preserves_sorted_order_and_application_precedence() {
        let mut existing = vec![
            entry("Alpha.txt", "C:/Content/Alpha.txt"),
            entry("Code.lnk", "C:/Programs/Code.lnk"),
            IndexedEntry {
                id: "application:C:/Programs/Terminal.lnk".to_owned(),
                path: "C:/Programs/Terminal.lnk".to_owned(),
                name: "Terminal".to_owned(),
                kind: "application".to_owned(),
                metadata: "开始菜单 · 当前用户".to_owned(),
                modified_at: None,
                extension: Some("lnk".to_owned()),
                size_bytes: 0,
                content: None,
            },
            entry("Zulu.txt", "C:/Content/Zulu.txt"),
        ];
        let mut replacements = vec![
            entry("Beta.txt", "C:/Content/Beta.txt"),
            entry("Gamma.txt", "C:/Content/Gamma.txt"),
            IndexedEntry {
                id: "application:C:/Programs/Code.lnk".to_owned(),
                path: "C:/Programs/Code.lnk".to_owned(),
                name: "Code".to_owned(),
                kind: "application".to_owned(),
                metadata: "开始菜单 · 当前用户".to_owned(),
                modified_at: None,
                extension: Some("lnk".to_owned()),
                size_bytes: 0,
                content: None,
            },
            entry("Terminal.lnk", "C:/Programs/Terminal.lnk"),
        ];
        sort_and_deduplicate_entries(&mut existing);
        sort_and_deduplicate_entries(&mut replacements);

        let merged = merge_sorted_entries(existing, replacements);

        assert_eq!(
            merged
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            [
                "C:/Content/Alpha.txt",
                "C:/Content/Beta.txt",
                "C:/Content/Gamma.txt",
                "C:/Content/Zulu.txt",
                "C:/Programs/Code.lnk",
                "C:/Programs/Terminal.lnk",
            ]
        );
        assert!(merged.windows(2).all(|pair| {
            compare_indexed_entries(&pair[0], &pair[1]) != CompareOrdering::Greater
        }));
        assert_eq!(merged[4].kind, "application");
        assert_eq!(merged[5].kind, "application");
    }

    fn unique_test_directory(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "ihub-indexer-{label}-{}-{nonce}",
            std::process::id()
        ))
    }
}
