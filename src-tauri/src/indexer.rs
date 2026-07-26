use std::{
    cmp::Ordering as CompareOrdering,
    collections::{BinaryHeap, HashSet},
    env,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex, RwLock,
    },
    thread,
    time::SystemTime,
};

use crate::models::{IndexStatus, SearchResult};
use chrono::{DateTime, Utc};
use fuzzy_matcher::{skim::SkimMatcherV2, FuzzyMatcher};
use ignore::{WalkBuilder, WalkState};
use rayon::prelude::*;

const MAX_INDEXED_ENTRIES: usize = 500_000;
const DEFAULT_RESULT_LIMIT: usize = 50;
const MAX_RESULT_LIMIT: usize = 200;

#[derive(Debug, Clone)]
struct IndexedEntry {
    id: String,
    path: String,
    name: String,
    kind: String,
    metadata: String,
    modified_at: Option<String>,
}

#[derive(Debug)]
struct IndexInner {
    entries: RwLock<Vec<IndexedEntry>>,
    status: RwLock<IndexStatus>,
    generation: AtomicU64,
}

/// An in-memory filename index. The current scanner is deliberately independent
/// from platform adapters so a future NTFS/USN or Spotlight backend can replace
/// it without changing the frontend command surface.
#[derive(Clone, Debug)]
pub struct SearchIndex {
    inner: Arc<IndexInner>,
}

impl SearchIndex {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(IndexInner {
                entries: RwLock::new(Vec::new()),
                status: RwLock::new(IndexStatus {
                    indexed_files: 0,
                    roots: default_roots()
                        .into_iter()
                        .map(|path| path.to_string_lossy().to_string())
                        .collect(),
                    phase: "idle".to_owned(),
                    last_indexed_at: None,
                }),
                generation: AtomicU64::new(0),
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

    pub fn rebuild_default_roots(&self) -> IndexStatus {
        self.rebuild(default_roots());
        self.status()
    }

    pub fn rebuild(&self, roots: Vec<PathBuf>) {
        let generation = self.inner.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let root_names: Vec<String> = roots
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect();

        {
            let mut status = self
                .inner
                .status
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            status.indexed_files = 0;
            status.roots = root_names;
            status.phase = "scanning".to_owned();
            status.last_indexed_at = None;
        }

        let inner = Arc::clone(&self.inner);
        let _ = thread::Builder::new()
            .name("ihub-file-indexer".to_owned())
            .spawn(move || {
                let entries = collect_entries(&roots, &inner, generation);
                if inner.generation.load(Ordering::SeqCst) != generation {
                    return;
                }

                let count = entries.len();
                {
                    let mut indexed = inner
                        .entries
                        .write()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    *indexed = entries;
                }
                let mut status = inner
                    .status
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                status.indexed_files = count;
                status.phase = "ready".to_owned();
                status.last_indexed_at = Some(now_iso());
            });
    }

    pub fn search(&self, query: &str, requested_limit: Option<usize>) -> Vec<SearchResult> {
        let limit = requested_limit
            .unwrap_or(DEFAULT_RESULT_LIMIT)
            .clamp(1, MAX_RESULT_LIMIT);
        let query = query.trim();
        let entries = self
            .inner
            .entries
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let top_matches = if query.is_empty() {
            entries
                .par_iter()
                .fold(
                    || TopMatches::new(limit),
                    |mut matches, entry| {
                        matches.consider(SearchMatch { entry, score: 0.0 });
                        matches
                    },
                )
                .reduce(|| TopMatches::new(limit), TopMatches::merge)
        } else {
            let normalized_query = query.to_lowercase();
            entries
                .par_iter()
                .map_init(SkimMatcherV2::default, |matcher, entry| {
                    let name_score = matcher.fuzzy_match(&entry.name, query);
                    let path_score = matcher.fuzzy_match(&entry.path, query);
                    let score = match (name_score, path_score) {
                        (Some(name), Some(path)) => name.max(path) as f64,
                        (Some(name), None) => name as f64,
                        (None, Some(path)) => path as f64,
                        (None, None) => return None,
                    };

                    // Exact prefixes are common launcher searches and deserve a
                    // predictable boost over a fuzzy match deeper in a pathname.
                    let prefix_bonus = if entry.name.to_lowercase().starts_with(&normalized_query) {
                        500.0
                    } else {
                        0.0
                    };
                    Some(SearchMatch {
                        entry,
                        score: score + prefix_bonus,
                    })
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
        };

        top_matches.into_results()
    }
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

    fn into_results(self) -> Vec<SearchResult> {
        let mut matches = self.matches.into_vec();
        matches.sort_unstable();
        matches
            .into_iter()
            .map(|candidate| candidate.entry.to_result(candidate.score))
            .collect()
    }
}

impl IndexedEntry {
    fn to_result(&self, score: f64) -> SearchResult {
        SearchResult {
            id: self.id.clone(),
            path: self.path.clone(),
            name: self.name.clone(),
            kind: self.kind.clone(),
            score,
            metadata: self.metadata.clone(),
            modified_at: self.modified_at.clone(),
        }
    }
}

fn collect_entries(
    roots: &[PathBuf],
    inner: &Arc<IndexInner>,
    generation: u64,
) -> Vec<IndexedEntry> {
    let collected = Arc::new(Mutex::new(Vec::new()));
    let count = Arc::new(AtomicUsize::new(0));

    for root in roots {
        if count.load(Ordering::Relaxed) >= MAX_INDEXED_ENTRIES {
            break;
        }
        if !root.is_dir() {
            continue;
        }

        let walker = WalkBuilder::new(root)
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

                let current = count.fetch_add(1, Ordering::Relaxed);
                if current >= MAX_INDEXED_ENTRIES {
                    return WalkState::Quit;
                }
                if current % 1_000 == 0 {
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
                };
                collected
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(indexed);
                WalkState::Continue
            })
        });
    }

    let mut entries = collected
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    entries.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    entries.dedup_by(|left, right| left.path == right.path);
    entries
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
        #[cfg(target_os = "macos")]
        {
            let applications = home.join("Applications");
            if applications.is_dir() {
                roots.push(applications);
            }
        }
        if roots.is_empty() && home.is_dir() {
            roots.push(home);
        }
    }
    #[cfg(target_os = "macos")]
    {
        let applications = PathBuf::from("/Applications");
        if applications.is_dir() {
            roots.push(applications);
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
            let value = path.to_string_lossy().to_lowercase();
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

    fn entry(name: &str, path: &str) -> IndexedEntry {
        IndexedEntry {
            id: path.to_owned(),
            path: path.to_owned(),
            name: name.to_owned(),
            kind: "file".to_owned(),
            metadata: String::new(),
            modified_at: None,
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

        let results = matches.into_results();
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
        *index
            .inner
            .entries
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = vec![
            entry("zulu", "/zulu"),
            entry("alpha", "/alpha"),
            entry("apple", "/apple"),
        ];

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
}
