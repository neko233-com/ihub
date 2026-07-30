use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
#[cfg(windows)]
use std::os::windows::{
    fs::{MetadataExt, OpenOptionsExt},
    io::AsRawHandle,
};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_OPEN_REPARSE_POINT,
};

use chrono::Utc;
use regex::{Captures, Regex};
use serde::Serialize;

const LOG_DIRECTORY_NAME: &str = "logs";
const ACTIVE_LOG_FILE_NAME: &str = "ihub.log";
const DEFAULT_MAX_FILE_BYTES: u64 = 256 * 1024;
const DEFAULT_MAX_FILES: usize = 4;
const DEFAULT_MAX_ENTRIES: usize = 1_000;
const MAX_MESSAGE_CHARS: usize = 2_048;
const MAX_COMPONENT_CHARS: usize = 48;
const MAX_ENCODED_ENTRY_BYTES: u64 = 16 * 1024;

static HOST_LOG: OnceLock<RollingHostLog> = OnceLock::new();

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HostLogEntry {
    pub timestamp: String,
    pub level: String,
    pub component: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HostLogSnapshot {
    pub generated_at: String,
    pub entries: Vec<HostLogEntry>,
    pub truncated: bool,
    pub total_bytes: u64,
    pub active_file_bytes: u64,
    pub max_file_bytes: u64,
    pub max_files: usize,
    pub write_failures: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_write_error: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct LogLimits {
    max_file_bytes: u64,
    max_files: usize,
    max_entries: usize,
}

impl Default for LogLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_files: DEFAULT_MAX_FILES,
            max_entries: DEFAULT_MAX_ENTRIES,
        }
    }
}

#[derive(Debug)]
pub(crate) struct RollingHostLog {
    directory: PathBuf,
    limits: LogLimits,
    operation: Mutex<()>,
    write_failures: AtomicU64,
    last_write_error: Mutex<Option<String>>,
}

impl RollingHostLog {
    fn new(directory: PathBuf, limits: LogLimits) -> Self {
        Self {
            directory,
            limits: LogLimits {
                max_file_bytes: limits.max_file_bytes.max(1),
                max_files: limits.max_files.max(1),
                max_entries: limits.max_entries.max(1),
            },
            operation: Mutex::new(()),
            write_failures: AtomicU64::new(0),
            last_write_error: Mutex::new(None),
        }
    }

    fn ensure_storage(&self) -> io::Result<()> {
        let _guard = self
            .operation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_storage_locked()
    }

    fn write(&self, level: &str, component: &str, message: &str) -> io::Result<()> {
        let _guard = self
            .operation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_storage_locked()?;

        let entry = HostLogEntry {
            timestamp: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            level: normalize_level(level).to_owned(),
            component: sanitize_component(component),
            message: sanitize_message(message),
        };
        let mut encoded = serde_json::to_vec(&entry)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        encoded.push(b'\n');
        if encoded.len() as u64 > MAX_ENCODED_ENTRY_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "The sanitized host log entry exceeded its encoded size limit.",
            ));
        }

        let active_path = self.active_path();
        let active_bytes = verified_regular_file_length_or_missing(&active_path)?.unwrap_or(0);
        if active_bytes > 0
            && active_bytes.saturating_add(encoded.len() as u64) > self.limits.max_file_bytes
        {
            self.rotate_locked()?;
        }

        let mut file = open_regular_file(&active_path, RegularFileOpenMode::Append)?;
        file.write_all(&encoded)?;
        file.flush()
    }

    fn snapshot(&self) -> io::Result<HostLogSnapshot> {
        let _guard = self
            .operation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_storage_locked()?;

        let mut entries = Vec::new();
        let mut total_bytes = 0_u64;
        let mut available_entry_count = 0_usize;
        // Read the oldest retained backup first so the UI gets chronological
        // order even after several rotations.
        for slot in (1..self.limits.max_files).rev() {
            let path = self.rotated_path(slot);
            self.read_file_locked(
                &path,
                &mut entries,
                &mut available_entry_count,
                &mut total_bytes,
            )?;
        }
        let active_file_bytes = self.read_file_locked(
            &self.active_path(),
            &mut entries,
            &mut available_entry_count,
            &mut total_bytes,
        )?;

        let truncated = available_entry_count > self.limits.max_entries;
        if entries.len() > self.limits.max_entries {
            entries.drain(..entries.len() - self.limits.max_entries);
        }
        Ok(HostLogSnapshot {
            generated_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            entries,
            truncated,
            total_bytes,
            active_file_bytes,
            max_file_bytes: self.limits.max_file_bytes,
            max_files: self.limits.max_files,
            write_failures: self.write_failures.load(Ordering::Acquire),
            last_write_error: self
                .last_write_error
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone(),
        })
    }

    fn clear(&self) -> io::Result<HostLogSnapshot> {
        let _guard = self
            .operation
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_storage_locked()?;
        remove_file_if_present(&self.active_path())?;
        for slot in 1..self.limits.max_files {
            remove_file_if_present(&self.rotated_path(slot))?;
        }
        // Leave a fixed empty active file behind. This proves that the
        // directory remains writable without adding a synthetic log line
        // immediately after the user explicitly cleared diagnostics.
        open_regular_file(&self.active_path(), RegularFileOpenMode::Truncate)?.sync_all()?;
        self.write_failures.store(0, Ordering::Release);
        *self
            .last_write_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        Ok(HostLogSnapshot {
            generated_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            entries: Vec::new(),
            truncated: false,
            total_bytes: 0,
            active_file_bytes: 0,
            max_file_bytes: self.limits.max_file_bytes,
            max_files: self.limits.max_files,
            write_failures: 0,
            last_write_error: None,
        })
    }

    fn read_file_locked(
        &self,
        path: &Path,
        entries: &mut Vec<HostLogEntry>,
        available_entry_count: &mut usize,
        total_bytes: &mut u64,
    ) -> io::Result<u64> {
        let max_read_bytes = self
            .limits
            .max_file_bytes
            .saturating_add(MAX_ENCODED_ENTRY_BYTES);
        let file = match open_regular_file(path, RegularFileOpenMode::Read) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error),
        };
        let file_length = opened_regular_file_length(&file)?;
        if file_length > max_read_bytes {
            return Err(oversized_log_error());
        }
        *total_bytes = total_bytes.saturating_add(file_length);
        // The metadata check above is not the bound: another same-user
        // process can grow a regular file after it has been opened. `take`
        // makes the byte ceiling authoritative on the already verified
        // handle, and the extra byte lets us distinguish EOF from overflow.
        let mut reader = BufReader::new(file.take(max_read_bytes.saturating_add(1)));
        let mut bytes_read = 0_u64;
        let mut line = Vec::new();
        loop {
            line.clear();
            let read = reader.read_until(b'\n', &mut line)?;
            if read == 0 {
                break;
            }
            bytes_read = bytes_read.saturating_add(read as u64);
            if bytes_read > max_read_bytes {
                return Err(oversized_log_error());
            }
            while matches!(line.last(), Some(b'\r' | b'\n')) {
                line.pop();
            }
            let Ok(entry) = serde_json::from_slice::<HostLogEntryWire>(&line) else {
                // A partial final line or invalid UTF-8 can remain after an
                // abrupt power loss. Never fail the entire diagnostics
                // surface because one retained record is malformed.
                continue;
            };
            *available_entry_count = available_entry_count.saturating_add(1);
            entries.push(HostLogEntry {
                timestamp: entry.timestamp,
                level: normalize_level(&entry.level).to_owned(),
                component: sanitize_component(&entry.component),
                message: sanitize_message(&entry.message),
            });
        }
        Ok(file_length)
    }

    fn rotate_locked(&self) -> io::Result<()> {
        if self.limits.max_files <= 1 {
            remove_file_if_present(&self.active_path())?;
            return Ok(());
        }
        remove_file_if_present(&self.rotated_path(self.limits.max_files - 1))?;
        for slot in (1..self.limits.max_files - 1).rev() {
            let source = self.rotated_path(slot);
            if verified_regular_file_length_or_missing(&source)?.is_some() {
                fs::rename(source, self.rotated_path(slot + 1))?;
            }
        }
        let active = self.active_path();
        if verified_regular_file_length_or_missing(&active)?.is_some() {
            fs::rename(active, self.rotated_path(1))?;
        }
        Ok(())
    }

    fn ensure_storage_locked(&self) -> io::Result<()> {
        fs::create_dir_all(&self.directory)?;
        let metadata = fs::symlink_metadata(&self.directory)?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "The host diagnostics directory is not a regular directory.",
            ));
        }
        #[cfg(windows)]
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "The host diagnostics directory must not be a reparse point.",
            ));
        }
        #[cfg(unix)]
        fs::set_permissions(&self.directory, fs::Permissions::from_mode(0o700))?;
        Ok(())
    }

    fn note_write_failure(&self, error: &io::Error) {
        self.write_failures.fetch_add(1, Ordering::AcqRel);
        *self
            .last_write_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Some(sanitize_message(&error.to_string()));
    }

    fn active_path(&self) -> PathBuf {
        self.directory.join(ACTIVE_LOG_FILE_NAME)
    }

    fn rotated_path(&self, slot: usize) -> PathBuf {
        self.directory.join(format!("ihub.{slot}.log"))
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct HostLogEntryWire {
    timestamp: String,
    level: String,
    component: String,
    message: String,
}

pub(crate) fn initialize(app_data_dir: &Path) -> Result<(), String> {
    let log = HOST_LOG.get_or_init(|| {
        RollingHostLog::new(app_data_dir.join(LOG_DIRECTORY_NAME), LogLimits::default())
    });
    log.ensure_storage().map_err(|error| {
        log.note_write_failure(&error);
        format!("Could not initialize bounded host diagnostics: {error}")
    })
}

pub(crate) fn debug(component: &str, message: impl AsRef<str>) {
    write_global("debug", component, message.as_ref());
}

pub(crate) fn info(component: &str, message: impl AsRef<str>) {
    write_global("info", component, message.as_ref());
}

pub(crate) fn warn(component: &str, message: impl AsRef<str>) {
    write_global("warn", component, message.as_ref());
}

pub(crate) fn error(component: &str, message: impl AsRef<str>) {
    write_global("error", component, message.as_ref());
}

pub(crate) fn snapshot() -> Result<HostLogSnapshot, String> {
    global_log()?
        .snapshot()
        .map_err(|error| format!("Could not read bounded host diagnostics: {error}"))
}

pub(crate) fn clear() -> Result<HostLogSnapshot, String> {
    global_log()?
        .clear()
        .map_err(|error| format!("Could not clear bounded host diagnostics: {error}"))
}

fn global_log() -> Result<&'static RollingHostLog, String> {
    HOST_LOG
        .get()
        .ok_or_else(|| "Host diagnostics are not initialized.".to_owned())
}

fn write_global(level: &str, component: &str, message: &str) {
    let Some(log) = HOST_LOG.get() else {
        return;
    };
    // Diagnostics must never crash or block the resident launcher because a
    // disk is full or the app-data directory temporarily becomes unavailable.
    if let Err(error) = log.write(level, component, message) {
        log.note_write_failure(&error);
    }
}

fn normalize_level(level: &str) -> &'static str {
    match level.trim().to_ascii_lowercase().as_str() {
        "debug" => "debug",
        "warn" | "warning" => "warn",
        "error" => "error",
        _ => "info",
    }
}

fn sanitize_component(component: &str) -> String {
    let component = component
        .chars()
        .take(MAX_COMPONENT_CHARS)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | ':' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if component.is_empty() {
        "host".to_owned()
    } else {
        component
    }
}

fn sanitize_message(message: &str) -> String {
    let mut characters = message.chars();
    let normalized = characters
        .by_ref()
        .take(MAX_MESSAGE_CHARS)
        .filter_map(|character| match character {
            '\r' | '\n' | '\t' => Some(' '),
            character if character.is_control() => None,
            character => Some(character),
        })
        .collect::<String>();
    let truncated = characters.next().is_some();

    let mut redacted = authorization_regex()
        .replace_all(&normalized, "$1 [REDACTED]")
        .into_owned();
    redacted = sensitive_assignment_regex()
        .replace_all(&redacted, |captures: &Captures<'_>| {
            format!("{}{}[REDACTED]", &captures[1], &captures[2])
        })
        .into_owned();
    redacted = url_userinfo_regex()
        .replace_all(&redacted, "$1[REDACTED]@")
        .into_owned();
    redacted = jwt_regex()
        .replace_all(&redacted, "[REDACTED_TOKEN]")
        .into_owned();
    redacted = windows_path_regex()
        .replace_all(&redacted, "[PATH]")
        .into_owned();
    redacted = home_path_regex()
        .replace_all(&redacted, "[PATH]")
        .into_owned();
    redacted = unix_path_regex()
        .replace_all(&redacted, "[PATH]")
        .into_owned();
    redacted = absolute_unix_path_regex()
        .replace_all(&redacted, "$1[PATH]")
        .into_owned();
    if truncated {
        redacted.push('…');
    }
    redacted
}

fn sensitive_assignment_regex() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r#"(?i)(^|[^A-Za-z0-9_.-])((?:["']?[A-Za-z0-9_.-]*(?:password|passwd|pwd|secret|token|api[_-]?key|access[_-]?key|authorization|cookie|session|credential|private[_\s-]?key)[A-Za-z0-9_.-]*["']?)\s*[:=]\s*)(?:"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|"[^,;}\]]*|'[^,;}\]]*|[^,;}\]]+)"#,
        )
        .expect("sensitive-assignment regex is valid")
    })
}

fn authorization_regex() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]{6,}")
            .expect("authorization regex is valid")
    })
}

fn url_userinfo_regex() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"(?i)\b(https?://)[^@/\s]+@").expect("URL userinfo regex is valid")
    })
}

fn jwt_regex() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"\b[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{10,}\b")
            .expect("JWT regex is valid")
    })
}

fn windows_path_regex() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        // Paths may legally contain spaces. Prefer over-redacting the
        // diagnostic remainder until a structural delimiter over leaking the
        // tail of a user profile or document path.
        Regex::new(r#"(?i)(?:\b[a-z]:[\\/]|\\\\)[^"'<>|,;)\]\r\n]*"#)
            .expect("Windows path regex is valid")
    })
}

fn home_path_regex() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r#"(?i)(?:~[\\/])[^"'<>|,;)\]\r\n]*"#).expect("home path regex is valid")
    })
}

fn unix_path_regex() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r#"(?i)/(?:Users|home|root|var|tmp|private|Volumes|opt|etc|usr|mnt|srv|Applications|Library|System)/[^"'<>|,;)\]\r\n]*"#,
        )
        .expect("Unix path regex is valid")
    })
}

fn absolute_unix_path_regex() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        // Keep the prefix in capture 1 so URLs (`https://`) do not look like
        // local paths: the first slash after `:` is followed by another slash
        // and therefore cannot match the required first path character.
        Regex::new(r#"(^|[\s("'=:])/[^\s/"'<>|,;)][^"'<>|,;)\]\r\n]*"#)
            .expect("absolute Unix path regex is valid")
    })
}

#[derive(Debug, Clone, Copy)]
enum RegularFileOpenMode {
    Read,
    Append,
    Truncate,
}

fn open_regular_file(path: &Path, mode: RegularFileOpenMode) -> io::Result<File> {
    let mut options = OpenOptions::new();
    match mode {
        RegularFileOpenMode::Read => {
            options.read(true);
        }
        RegularFileOpenMode::Append => {
            options.create(true).append(true);
        }
        RegularFileOpenMode::Truncate => {
            // Do not request truncate during open: a same-user process could
            // race a hard link into place after clear() removes the old file.
            // Validate the opened handle first, then truncate that exact
            // already-approved file below.
            options.create(true).write(true);
        }
    }
    configure_regular_file_open(&mut options);
    let file = options.open(path)?;
    opened_regular_file_length(&file)?;
    if matches!(mode, RegularFileOpenMode::Truncate) {
        file.set_len(0)?;
    }
    Ok(file)
}

fn configure_regular_file_open(options: &mut OpenOptions) {
    #[cfg(unix)]
    {
        // O_NOFOLLOW closes the check/open symlink race. O_NONBLOCK makes a
        // raced FIFO/device open return promptly so the handle-type check can
        // reject it without freezing the resident launcher.
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        // Open the reparse point itself instead of following it, then reject
        // the opened handle below when FILE_ATTRIBUTE_REPARSE_POINT is set.
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
}

fn opened_regular_file_length(file: &File) -> io::Result<u64> {
    let metadata = file.metadata()?;
    #[cfg(windows)]
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(non_regular_log_error());
    }
    if !metadata.file_type().is_file() {
        return Err(non_regular_log_error());
    }
    #[cfg(unix)]
    if metadata.nlink() != 1 {
        return Err(multiply_linked_log_error());
    }
    #[cfg(windows)]
    {
        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        let result =
            unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut information) };
        if result == 0 {
            return Err(io::Error::last_os_error());
        }
        if information.nNumberOfLinks != 1 {
            return Err(multiply_linked_log_error());
        }
    }
    Ok(metadata.len())
}

fn non_regular_log_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "A host diagnostics file is not a regular file.",
    )
}

fn oversized_log_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "A retained host log file exceeded its bounded read limit.",
    )
}

fn multiply_linked_log_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "A host diagnostics file has more than one filesystem link.",
    )
}

fn remove_file_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn verified_regular_file_length_or_missing(path: &Path) -> io::Result<Option<u64>> {
    match open_regular_file(path, RegularFileOpenMode::Read) {
        Ok(file) => opened_regular_file_length(&file).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, OpenOptions},
        io::Write,
        sync::Arc,
        thread,
    };

    #[cfg(windows)]
    use std::io;
    #[cfg(windows)]
    use std::os::windows::fs::symlink_dir;
    use uuid::Uuid;

    use super::{sanitize_message, HostLogEntry, LogLimits, RollingHostLog, ACTIVE_LOG_FILE_NAME};

    struct TestDirectory {
        path: std::path::PathBuf,
    }

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("ihub-host-log-{}", Uuid::new_v4()));
            fs::create_dir_all(&path).expect("test log directory should be created");
            Self { path }
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn test_log(
        directory: &TestDirectory,
        max_file_bytes: u64,
        max_files: usize,
    ) -> RollingHostLog {
        RollingHostLog::new(
            directory.path.clone(),
            LogLimits {
                max_file_bytes,
                max_files,
                max_entries: 50,
            },
        )
    }

    #[test]
    fn redacts_sensitive_assignments_credentials_and_absolute_paths() {
        let message = concat!(
            "password=hunter2 token: abcdefghijklmnop ",
            "Authorization=Bearer abcdefghijklmnop ",
            "https://person:secret@example.test/api; ",
            r"C:\Users\alice\private\notes.txt; ",
            "/Users/alice/private/notes.txt; ",
            "path=/data/projects/private.txt"
        );
        let redacted = sanitize_message(message);
        assert!(!redacted.contains("hunter2"));
        assert!(!redacted.contains("abcdefghijklmnop"));
        assert!(!redacted.contains("person:secret"));
        assert!(!redacted.contains("alice"));
        assert!(redacted.contains("password=[REDACTED]"));
        assert!(redacted.matches("[PATH]").count() >= 3);
    }

    #[test]
    fn redacts_quoted_json_namespaced_secrets_and_unterminated_values() {
        let message = concat!(
            r#"{"password":"hunter2","OPENAI_API_KEY":"sk-live-secret"} "#,
            "GITHUB_TOKEN=ghp_supersecret; ",
            "AWS_SECRET_ACCESS_KEY=aws-secret; ",
            "password=\"hunter two; ",
            "https://ghp_token_only@example.test/repository.git"
        );
        let redacted = sanitize_message(message);
        for secret in [
            "hunter2",
            "sk-live-secret",
            "ghp_supersecret",
            "aws-secret",
            "hunter two",
            "ghp_token_only",
        ] {
            assert!(!redacted.contains(secret), "secret leaked: {secret}");
        }
        assert!(redacted.matches("[REDACTED]").count() >= 5);
    }

    #[test]
    fn redacts_the_complete_tail_of_paths_that_contain_spaces() {
        let redacted = sanitize_message(
            r"Could not open C:\Users\Alice Smith\private\notes.txt, retrying safely.",
        );
        assert!(!redacted.contains("Alice"));
        assert!(!redacted.contains("Smith"));
        assert!(!redacted.contains("private"));
        assert!(redacted.contains("[PATH], retrying safely."));
    }

    #[test]
    fn rotates_to_a_fixed_file_count_and_returns_entries_in_order() {
        let directory = TestDirectory::new();
        let log = test_log(&directory, 150, 3);
        for index in 0..12 {
            log.write("info", "rotation", &format!("event-{index:02}"))
                .expect("test entry should be written");
        }
        let retained_files = fs::read_dir(&directory.path)
            .expect("log directory should be readable")
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .count();
        assert!(retained_files <= 3);
        assert!(directory.path.join(ACTIVE_LOG_FILE_NAME).is_file());

        let snapshot = log.snapshot().expect("snapshot should be readable");
        assert!(!snapshot.entries.is_empty());
        let numbers = snapshot
            .entries
            .iter()
            .map(|entry| {
                entry
                    .message
                    .strip_prefix("event-")
                    .expect("test message prefix")
                    .parse::<u8>()
                    .expect("test message number")
            })
            .collect::<Vec<_>>();
        assert!(numbers.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(numbers.last(), Some(&11));
    }

    #[test]
    fn concurrent_writers_produce_complete_json_lines() {
        let directory = TestDirectory::new();
        let log = Arc::new(test_log(&directory, 64 * 1024, 2));
        let handles = (0..8)
            .map(|worker| {
                let log = Arc::clone(&log);
                thread::spawn(move || {
                    for event in 0..25 {
                        log.write(
                            "debug",
                            "threads",
                            &format!("worker-{worker}-event-{event}"),
                        )
                        .expect("concurrent write should succeed");
                    }
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().expect("writer should not panic");
        }
        let snapshot = log.snapshot().expect("snapshot should be readable");
        assert_eq!(snapshot.entries.len(), 50);
        assert!(snapshot.truncated);
        assert!(snapshot
            .entries
            .iter()
            .all(|entry| entry.component == "threads"));
    }

    #[test]
    fn clear_removes_every_retained_entry_and_keeps_an_empty_active_file() {
        let directory = TestDirectory::new();
        let log = test_log(&directory, 180, 4);
        for index in 0..10 {
            log.write("warn", "clear", &format!("event-{index}"))
                .expect("test entry should be written");
        }
        let cleared = log.clear().expect("clear should succeed");
        assert!(cleared.entries.is_empty());
        assert_eq!(cleared.total_bytes, 0);
        assert_eq!(
            fs::metadata(directory.path.join(ACTIVE_LOG_FILE_NAME))
                .expect("active file should remain")
                .len(),
            0
        );
        let reread = log.snapshot().expect("empty log should remain readable");
        assert_eq!(reread.entries, Vec::<HostLogEntry>::new());
    }

    #[test]
    fn refuses_non_regular_or_oversized_retained_files() {
        let directory = TestDirectory::new();
        let log = test_log(&directory, 128, 2);
        let active = directory.path.join(ACTIVE_LOG_FILE_NAME);
        fs::create_dir(&active).expect("directory-shaped log fixture should be created");
        let error = log
            .write("info", "security", "must not follow this entry")
            .expect_err("non-regular active entry must be rejected");
        assert!(
            matches!(
                error.kind(),
                std::io::ErrorKind::InvalidData | std::io::ErrorKind::PermissionDenied
            ),
            "a directory-shaped log target must be rejected before writing: {error}"
        );
        fs::remove_dir(&active).expect("directory-shaped fixture should be removable");

        fs::write(&active, vec![b'x'; 128 + 16 * 1024 + 1])
            .expect("oversized log fixture should be written");
        let error = log
            .snapshot()
            .expect_err("oversized retained file must not be read into memory");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn refuses_a_hard_link_to_another_file() {
        let directory = TestDirectory::new();
        let log = test_log(&directory, 4 * 1024, 2);
        let outside = directory.path.join("outside.txt");
        fs::write(&outside, b"must stay unchanged").expect("outside fixture should be written");
        fs::hard_link(&outside, directory.path.join(ACTIVE_LOG_FILE_NAME))
            .expect("hard-link fixture should be created");

        let error = log
            .write("info", "security", "must not append through a hard link")
            .expect_err("multiply linked active file must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            fs::read(&outside).expect("outside fixture should remain readable"),
            b"must stay unchanged"
        );
    }

    #[cfg(windows)]
    #[test]
    fn refuses_a_reparse_point_diagnostics_directory() {
        let directory = TestDirectory::new();
        let real = directory.path.join("real-logs");
        let linked = directory.path.join("linked-logs");
        fs::create_dir(&real).expect("real diagnostics fixture should be created");
        if symlink_dir(&real, &linked).is_err() {
            // Creating directory links can be disabled by local Windows
            // policy. The production metadata check remains compiled; other
            // Windows environments exercise the behavioral assertion.
            return;
        }
        let log = RollingHostLog::new(
            linked.clone(),
            LogLimits {
                max_file_bytes: 4 * 1024,
                max_files: 2,
                max_entries: 50,
            },
        );
        let error = log
            .write("info", "security", "must not write through a junction")
            .expect_err("directory reparse points must be rejected");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!real.join(ACTIVE_LOG_FILE_NAME).exists());
        fs::remove_dir(&linked).expect("directory link should be removable");
    }

    #[test]
    fn skips_invalid_utf8_lines_without_losing_adjacent_valid_entries() {
        let directory = TestDirectory::new();
        let log = test_log(&directory, 4 * 1024, 2);
        log.write("info", "utf8", "before")
            .expect("first valid entry should be written");
        let active = directory.path.join(ACTIVE_LOG_FILE_NAME);
        let mut file = OpenOptions::new()
            .append(true)
            .open(&active)
            .expect("active log should be appendable in the test");
        file.write_all(&[0xff, b'\n'])
            .expect("invalid UTF-8 fixture should be written");
        drop(file);
        log.write("info", "utf8", "after")
            .expect("second valid entry should be written");

        let snapshot = log.snapshot().expect("snapshot should skip the bad line");
        assert_eq!(
            snapshot
                .entries
                .iter()
                .map(|entry| entry.message.as_str())
                .collect::<Vec<_>>(),
            vec!["before", "after"]
        );
    }
}
