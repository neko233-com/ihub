use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{ser::PrettyFormatter, Value};

const MAX_BATCH_RENAME_ITEMS: usize = 5_000;
const BATCH_RENAME_PREVIEW_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_RENAME_SEQUENCE_PADDING: u8 = 12;
const MAX_JSON_FORMAT_INPUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_JSON_QUERY_INPUT_BYTES: usize = 2 * 1024 * 1024;
const MAX_JSON_QUERY_SELECTOR_BYTES: usize = 512;
const MAX_JSON_QUERY_STEPS: usize = 48;
const MAX_JSON_QUERY_MATCHES: usize = 1_000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonFormatResult {
    pub valid: bool,
    pub formatted: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JsonQueryResult {
    pub valid: bool,
    pub matches: usize,
    pub formatted: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BatchRenameItem {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchRenamePreview {
    pub directory: String,
    pub items: Vec<BatchRenameItem>,
    pub can_apply: bool,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchRenameResult {
    pub renamed: usize,
    pub items: Vec<BatchRenameItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClipboardWriteResult {
    pub written: bool,
}

#[derive(Debug, Clone)]
struct PreviewRecord {
    directory: String,
    items: Vec<BatchRenameItem>,
    created_at: Instant,
}

#[derive(Debug)]
struct ResolvedRenameItem {
    from: PathBuf,
    to: PathBuf,
}

#[derive(Debug)]
struct StagedRenameItem {
    from: PathBuf,
    to: PathBuf,
    temporary: PathBuf,
}

static BATCH_RENAME_PREVIEWS: OnceLock<Mutex<Vec<PreviewRecord>>> = OnceLock::new();

#[tauri::command]
pub fn format_json(input: String, indent: Option<usize>) -> JsonFormatResult {
    format_json_text(&input, indent)
}

#[tauri::command]
pub fn query_json(input: String, selector: String) -> JsonQueryResult {
    query_json_text(&input, &selector)
}

pub fn preview_batch_rename(
    directory: String,
    find: String,
    replace: String,
    use_regex: Option<bool>,
    sequence_start: Option<u32>,
    sequence_padding: Option<u8>,
) -> Result<BatchRenamePreview, String> {
    let directory = canonical_directory(&directory)?;
    let matcher = RenameMatcher::new(&find, use_regex.unwrap_or(false))?;
    let sequence = RenameSequence::from_replacement(&replace, sequence_start, sequence_padding)?;
    let mut items = Vec::new();
    let mut errors = Vec::new();
    let mut candidates = Vec::new();
    let entries = fs::read_dir(&directory)
        .map_err(|error| format!("Could not read directory {}: {error}", directory.display()))?;

    for entry in entries {
        let entry = entry.map_err(|error| format!("Could not read a directory entry: {error}"))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("Could not inspect {}: {error}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }

        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            errors.push(format!(
                "Skipped {} because its file name is not valid UTF-8.",
                path.display()
            ));
            continue;
        };
        let file_name = file_name.to_owned();
        if !matcher.is_match(&file_name) {
            continue;
        }
        if candidates.len() >= MAX_BATCH_RENAME_ITEMS {
            errors.push(format!(
                "Preview is limited to {MAX_BATCH_RENAME_ITEMS} files. Narrow the rule before applying."
            ));
            break;
        }
        candidates.push((path, file_name));
    }

    // Filesystems do not promise an enumeration order. Sort before expanding
    // `{n}` so repeated previews always assign the same sequence numbers.
    candidates.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));

    for (index, (path, file_name)) in candidates.into_iter().enumerate() {
        let replacement = match sequence
            .as_ref()
            .map(|sequence| sequence.expand(&replace, index))
            .transpose()
        {
            Ok(Some(replacement)) => replacement,
            Ok(None) => replace.clone(),
            Err(error) => {
                errors.push(error);
                break;
            }
        };
        let Some(next_name) = matcher.replace(&file_name, &replacement) else {
            continue;
        };
        if next_name == file_name {
            continue;
        }
        if let Err(error) = validate_portable_file_name(&next_name) {
            errors.push(format!("{}: {error}", path.display()));
            continue;
        }

        let from = path.canonicalize().map_err(|error| {
            format!(
                "Could not resolve source file {} while building the preview: {error}",
                path.display()
            )
        })?;
        if from.parent() != Some(directory.as_path()) {
            errors.push(format!(
                "Skipped {} because it resolves outside the selected directory.",
                path.display()
            ));
            continue;
        }
        let to = directory.join(&next_name);
        items.push(BatchRenameItem {
            from: from.to_string_lossy().into_owned(),
            to: to.to_string_lossy().into_owned(),
        });
    }

    items.sort_by(|left, right| left.from.cmp(&right.from));
    validate_preview_destinations(&directory, &items, &mut errors);
    if items.is_empty() && errors.is_empty() {
        errors.push("No regular files matched this rename rule.".to_owned());
    }

    let preview = BatchRenamePreview {
        directory: directory.to_string_lossy().into_owned(),
        can_apply: !items.is_empty() && errors.is_empty(),
        items,
        errors,
    };
    if preview.can_apply {
        remember_preview(&preview);
    }
    Ok(preview)
}

pub fn apply_batch_rename(
    directory: String,
    items: Vec<BatchRenameItem>,
) -> Result<BatchRenameResult, String> {
    let directory = canonical_directory(&directory)?;
    take_preview(&directory, &items)?;
    let resolved = validate_batch_for_apply(&directory, &items)?;
    rename_batch(&directory, &resolved)?;
    Ok(BatchRenameResult {
        renamed: items.len(),
        items,
    })
}

#[tauri::command]
pub fn write_clipboard_text(text: String) -> Result<ClipboardWriteResult, String> {
    crate::clipboard_access::with_clipboard(|clipboard| clipboard.set_text(text.clone()))
        .map_err(|error| format!("Could not write to the system clipboard: {error}"))?;
    Ok(ClipboardWriteResult { written: true })
}

fn format_json_text(input: &str, indent: Option<usize>) -> JsonFormatResult {
    if input.len() > MAX_JSON_FORMAT_INPUT_BYTES {
        return JsonFormatResult {
            valid: false,
            formatted: None,
            error: Some(format!(
                "JSON format input is limited to {MAX_JSON_FORMAT_INPUT_BYTES} bytes."
            )),
        };
    }

    let value = match serde_json::from_str::<Value>(input) {
        Ok(value) => value,
        Err(error) => {
            return JsonFormatResult {
                valid: false,
                formatted: None,
                error: Some(format!("Invalid JSON: {error}")),
            }
        }
    };
    let indentation = vec![b' '; indent.unwrap_or(2).clamp(1, 8)];
    let formatter = PrettyFormatter::with_indent(&indentation);
    let mut output = Vec::new();
    if let Err(error) = value.serialize(&mut serde_json::Serializer::with_formatter(
        &mut output,
        formatter,
    )) {
        return JsonFormatResult {
            valid: false,
            formatted: None,
            error: Some(format!("Could not format JSON: {error}")),
        };
    }
    match String::from_utf8(output) {
        Ok(formatted) => JsonFormatResult {
            valid: true,
            formatted: Some(formatted),
            error: None,
        },
        Err(error) => JsonFormatResult {
            valid: false,
            formatted: None,
            error: Some(format!("Could not encode formatted JSON: {error}")),
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum JsonQueryStep {
    Field(String),
    Index(usize),
    Wildcard,
}

fn query_json_text(input: &str, selector: &str) -> JsonQueryResult {
    if input.len() > MAX_JSON_QUERY_INPUT_BYTES {
        return json_query_error(format!(
            "JSON query input is limited to {MAX_JSON_QUERY_INPUT_BYTES} bytes."
        ));
    }

    let value = match serde_json::from_str::<Value>(input) {
        Ok(value) => value,
        Err(error) => return json_query_error(format!("Invalid JSON: {error}")),
    };
    let steps = match parse_json_selector(selector) {
        Ok(steps) => steps,
        Err(error) => return json_query_error(error),
    };

    let mut current = vec![&value];
    for step in steps {
        let mut next = Vec::new();
        for candidate in current {
            match &step {
                JsonQueryStep::Field(field) => {
                    if let Some(value) = candidate.as_object().and_then(|object| object.get(field))
                    {
                        next.push(value);
                    }
                }
                JsonQueryStep::Index(index) => {
                    if let Some(value) = candidate.as_array().and_then(|array| array.get(*index)) {
                        next.push(value);
                    }
                }
                JsonQueryStep::Wildcard => match candidate {
                    Value::Array(values) => next.extend(values.iter()),
                    Value::Object(values) => next.extend(values.values()),
                    _ => {}
                },
            }
            if next.len() > MAX_JSON_QUERY_MATCHES {
                return json_query_error(format!(
                    "Selector matched more than {MAX_JSON_QUERY_MATCHES} values. Narrow the path before querying."
                ));
            }
        }
        current = next;
    }

    let matches = current.len();
    let output = if matches == 1 {
        current[0].clone()
    } else {
        Value::Array(current.into_iter().cloned().collect())
    };
    match serde_json::to_string_pretty(&output) {
        Ok(formatted) => JsonQueryResult {
            valid: true,
            matches,
            formatted: Some(formatted),
            error: None,
        },
        Err(error) => json_query_error(format!("Could not serialize query result: {error}")),
    }
}

fn json_query_error(error: impl Into<String>) -> JsonQueryResult {
    JsonQueryResult {
        valid: false,
        matches: 0,
        formatted: None,
        error: Some(error.into()),
    }
}

fn parse_json_selector(selector: &str) -> Result<Vec<JsonQueryStep>, String> {
    let selector = selector.trim();
    if selector.is_empty() {
        return Err("Enter a JSON selector starting with `$`.".to_owned());
    }
    if selector.len() > MAX_JSON_QUERY_SELECTOR_BYTES {
        return Err(format!(
            "JSON selectors are limited to {MAX_JSON_QUERY_SELECTOR_BYTES} bytes."
        ));
    }

    let characters: Vec<char> = selector.chars().collect();
    if characters.first() != Some(&'$') {
        return Err("JSON selectors must start with `$`, for example `$.items[*].id`.".to_owned());
    }

    let mut cursor = 1;
    let mut steps = Vec::new();
    while cursor < characters.len() {
        if steps.len() >= MAX_JSON_QUERY_STEPS {
            return Err(format!(
                "JSON selectors are limited to {MAX_JSON_QUERY_STEPS} path steps."
            ));
        }

        match characters[cursor] {
            '.' => {
                cursor += 1;
                let start = cursor;
                while cursor < characters.len() && is_json_dot_field_character(characters[cursor]) {
                    cursor += 1;
                }
                if start == cursor {
                    return Err("Expected a field name after `.` in the JSON selector.".to_owned());
                }
                steps.push(JsonQueryStep::Field(
                    characters[start..cursor].iter().collect(),
                ));
            }
            '[' => {
                cursor += 1;
                if cursor >= characters.len() {
                    return Err("Unclosed `[` in the JSON selector.".to_owned());
                }
                match characters[cursor] {
                    '*' => {
                        cursor += 1;
                        expect_json_selector_character(&characters, &mut cursor, ']')?;
                        steps.push(JsonQueryStep::Wildcard);
                    }
                    '\'' | '"' => {
                        let quote = characters[cursor];
                        cursor += 1;
                        let field = parse_json_bracket_field(&characters, &mut cursor, quote)?;
                        expect_json_selector_character(&characters, &mut cursor, ']')?;
                        steps.push(JsonQueryStep::Field(field));
                    }
                    character if character.is_ascii_digit() => {
                        let start = cursor;
                        while cursor < characters.len() && characters[cursor].is_ascii_digit() {
                            cursor += 1;
                        }
                        let raw_index: String = characters[start..cursor].iter().collect();
                        let index = raw_index.parse::<usize>().map_err(|_| {
                            "JSON array indexes must be non-negative integers that fit this platform."
                                .to_owned()
                        })?;
                        expect_json_selector_character(&characters, &mut cursor, ']')?;
                        steps.push(JsonQueryStep::Index(index));
                    }
                    _ => {
                        return Err(
                            "Bracket selectors only support a quoted field, a non-negative index, or `[*]`."
                                .to_owned(),
                        );
                    }
                }
            }
            _ => {
                return Err(
                    "Expected `.` or `[` after a JSON selector step; filters and executable expressions are not supported."
                        .to_owned(),
                );
            }
        }
    }

    Ok(steps)
}

fn is_json_dot_field_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '$')
}

fn parse_json_bracket_field(
    characters: &[char],
    cursor: &mut usize,
    quote: char,
) -> Result<String, String> {
    let mut field = String::new();
    let mut escaped = false;
    while *cursor < characters.len() {
        let character = characters[*cursor];
        *cursor += 1;
        if escaped {
            let decoded = match character {
                '\\' => '\\',
                '\'' => '\'',
                '"' => '"',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                _ => {
                    return Err(
                        r#"Quoted JSON selector fields support only \n, \r, \t, \\, \" and \' escapes."#
                            .to_owned(),
                    )
                }
            };
            field.push(decoded);
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if character == quote {
            return Ok(field);
        }
        if character.is_control() {
            return Err(
                "Quoted JSON selector fields cannot contain control characters.".to_owned(),
            );
        }
        field.push(character);
    }
    Err("Unclosed quoted field in the JSON selector.".to_owned())
}

fn expect_json_selector_character(
    characters: &[char],
    cursor: &mut usize,
    expected: char,
) -> Result<(), String> {
    if characters.get(*cursor) != Some(&expected) {
        return Err(format!("Expected `{expected}` in the JSON selector."));
    }
    *cursor += 1;
    Ok(())
}

fn canonical_directory(value: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if !path.is_absolute() {
        return Err("Choose an absolute directory path for batch rename.".to_owned());
    }
    let path = path
        .canonicalize()
        .map_err(|error| format!("Could not resolve selected directory: {error}"))?;
    let metadata = fs::metadata(&path)
        .map_err(|error| format!("Could not inspect selected directory: {error}"))?;
    if !metadata.is_dir() {
        return Err("The selected batch-rename path must be a directory.".to_owned());
    }
    Ok(path)
}

enum RenameMatcher {
    Literal(String),
    Regex(Regex),
}

impl RenameMatcher {
    fn new(find: &str, use_regex: bool) -> Result<Self, String> {
        if find.is_empty() {
            return Err(
                "Enter text or a regular expression to find; empty rules are not allowed."
                    .to_owned(),
            );
        }
        if use_regex {
            Regex::new(find)
                .map(Self::Regex)
                .map_err(|error| format!("Invalid regular expression: {error}"))
        } else {
            Ok(Self::Literal(find.to_owned()))
        }
    }

    fn replace(&self, source: &str, replacement: &str) -> Option<String> {
        match self {
            Self::Literal(find) => source
                .contains(find)
                .then(|| source.replace(find, replacement)),
            Self::Regex(expression) => expression
                .is_match(source)
                .then(|| expression.replace_all(source, replacement).into_owned()),
        }
    }

    fn is_match(&self, source: &str) -> bool {
        match self {
            Self::Literal(find) => source.contains(find),
            Self::Regex(expression) => expression.is_match(source),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct RenameSequence {
    start: u32,
    padding: u8,
}

impl RenameSequence {
    fn from_replacement(
        replacement: &str,
        sequence_start: Option<u32>,
        sequence_padding: Option<u8>,
    ) -> Result<Option<Self>, String> {
        if !replacement.contains("{n}") {
            return Ok(None);
        }

        let start = sequence_start.unwrap_or(1);
        if start == 0 {
            return Err("Numbered rename sequences must start at 1 or later.".to_owned());
        }
        let padding = sequence_padding.unwrap_or(3);
        if padding > MAX_RENAME_SEQUENCE_PADDING {
            return Err(format!(
                "Number padding is limited to {MAX_RENAME_SEQUENCE_PADDING} digits."
            ));
        }
        Ok(Some(Self { start, padding }))
    }

    fn expand(&self, replacement: &str, index: usize) -> Result<String, String> {
        let offset = u32::try_from(index)
            .map_err(|_| "Too many files matched this numbered rename sequence.".to_owned())?;
        let number = self.start.checked_add(offset).ok_or_else(|| {
            "The numbered rename sequence would exceed its supported range.".to_owned()
        })?;
        let number = number.to_string();
        let padding = usize::from(self.padding).saturating_sub(number.len());
        let number = format!("{}{}", "0".repeat(padding), number);
        Ok(replacement.replace("{n}", &number))
    }
}

fn validate_preview_destinations(
    directory: &Path,
    items: &[BatchRenameItem],
    errors: &mut Vec<String>,
) {
    let mut destinations = HashSet::new();
    for item in items {
        let target = PathBuf::from(&item.to);
        if !destinations.insert(target.clone()) {
            errors.push(format!(
                "More than one file would become {}.",
                target.display()
            ));
            continue;
        }
        match fs::symlink_metadata(&target) {
            Ok(_) => errors.push(format!(
                "Cannot rename {} because destination {} already exists.",
                item.from,
                target.display()
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => errors.push(format!(
                "Could not check destination {}: {error}",
                target.display()
            )),
        }
        if target.parent() != Some(directory) {
            errors.push(format!(
                "Destination {} escapes the selected directory.",
                target.display()
            ));
        }
    }
}

fn remember_preview(preview: &BatchRenamePreview) {
    let mut previews = preview_store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    remove_expired_previews(&mut previews);
    previews
        .retain(|record| !(record.directory == preview.directory && record.items == preview.items));
    previews.push(PreviewRecord {
        directory: preview.directory.clone(),
        items: preview.items.clone(),
        created_at: Instant::now(),
    });
    // Keeping a short history lets a user re-open the drawer without retaining
    // unbounded paths in memory, while still requiring an exact preview match.
    if previews.len() > 24 {
        let remove_count = previews.len() - 24;
        previews.drain(..remove_count);
    }
}

fn take_preview(directory: &Path, items: &[BatchRenameItem]) -> Result<(), String> {
    let mut previews = preview_store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    remove_expired_previews(&mut previews);
    let directory = directory.to_string_lossy();
    let Some(index) = previews
        .iter()
        .position(|record| record.directory == directory && record.items == items)
    else {
        return Err(
            "This batch has not been previewed recently. Preview the exact rename list again before applying it."
                .to_owned(),
        );
    };
    previews.remove(index);
    Ok(())
}

fn preview_store() -> &'static Mutex<Vec<PreviewRecord>> {
    BATCH_RENAME_PREVIEWS.get_or_init(|| Mutex::new(Vec::new()))
}

fn remove_expired_previews(previews: &mut Vec<PreviewRecord>) {
    previews.retain(|record| record.created_at.elapsed() <= BATCH_RENAME_PREVIEW_TTL);
}

fn validate_batch_for_apply(
    directory: &Path,
    items: &[BatchRenameItem],
) -> Result<Vec<ResolvedRenameItem>, String> {
    if items.is_empty() {
        return Err("There are no previewed files to rename.".to_owned());
    }
    if items.len() > MAX_BATCH_RENAME_ITEMS {
        return Err(format!(
            "A single batch rename is limited to {MAX_BATCH_RENAME_ITEMS} files."
        ));
    }

    let mut sources = HashSet::new();
    let mut destinations = HashSet::new();
    let mut resolved = Vec::with_capacity(items.len());
    for item in items {
        let from = PathBuf::from(&item.from);
        let to = PathBuf::from(&item.to);
        let source_name = direct_child_name(&from, directory, "source")?;
        let target_name = direct_child_name(&to, directory, "destination")?;
        let source_name = source_name
            .to_str()
            .ok_or_else(|| "Batch rename source names must be valid UTF-8.".to_owned())?;
        let target_name = target_name
            .to_str()
            .ok_or_else(|| "Batch rename destination names must be valid UTF-8.".to_owned())?;
        validate_portable_file_name(source_name)?;
        validate_portable_file_name(target_name)?;
        if from == to {
            return Err(format!("{} has an unchanged destination.", from.display()));
        }
        if !sources.insert(from.clone()) {
            return Err(format!(
                "{} appears more than once in this batch.",
                from.display()
            ));
        }
        if !destinations.insert(to.clone()) {
            return Err(format!("More than one file would become {}.", to.display()));
        }

        let metadata = fs::symlink_metadata(&from)
            .map_err(|error| format!("Could not inspect source {}: {error}", from.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!(
                "{} is not a regular file and cannot be batch renamed.",
                from.display()
            ));
        }
        let canonical_from = from
            .canonicalize()
            .map_err(|error| format!("Could not resolve source {}: {error}", from.display()))?;
        if canonical_from != from || canonical_from.parent() != Some(directory) {
            return Err(format!(
                "{} does not resolve to a direct file inside the selected directory.",
                from.display()
            ));
        }
        match fs::symlink_metadata(&to) {
            Ok(_) => {
                return Err(format!(
                    "Destination {} already exists; no files were renamed.",
                    to.display()
                ))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "Could not check destination {}: {error}",
                    to.display()
                ))
            }
        }
        resolved.push(ResolvedRenameItem { from, to });
    }
    Ok(resolved)
}

fn direct_child_name(
    path: &Path,
    directory: &Path,
    label: &str,
) -> Result<std::ffi::OsString, String> {
    if !path.is_absolute() {
        return Err(format!("Batch rename {label} paths must be absolute."));
    }
    let name = path
        .file_name()
        .ok_or_else(|| format!("Batch rename {label} paths must name a file."))?;
    if path != directory.join(name) {
        return Err(format!(
            "Batch rename {label} path {} escapes the selected directory.",
            path.display()
        ));
    }
    Ok(name.to_owned())
}

fn validate_portable_file_name(value: &str) -> Result<(), String> {
    if value.is_empty() || value == "." || value == ".." {
        return Err("File names cannot be empty, '.' or '..'.".to_owned());
    }
    if value.ends_with([' ', '.']) {
        return Err("File names cannot end with a space or period.".to_owned());
    }
    if value.chars().any(|character| {
        character.is_control()
            || matches!(
                character,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
            )
    }) {
        return Err("File names cannot contain path separators or reserved characters.".to_owned());
    }
    let base = value
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if matches!(
        base.as_str(),
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
    ) {
        return Err("File names cannot use a reserved Windows device name.".to_owned());
    }
    Ok(())
}

fn rename_batch(directory: &Path, items: &[ResolvedRenameItem]) -> Result<(), String> {
    let operation_id = unique_operation_id();
    let mut staged = Vec::with_capacity(items.len());
    for (index, item) in items.iter().enumerate() {
        let temporary = temporary_path(directory, &operation_id, index)?;
        if let Err(error) = move_file_without_overwrite(&item.from, &temporary) {
            let rollback_errors = rollback_staging(&staged);
            return Err(rename_failure_message(
                "stage",
                &item.from,
                error,
                rollback_errors,
            ));
        }
        staged.push(StagedRenameItem {
            from: item.from.clone(),
            to: item.to.clone(),
            temporary,
        });
    }

    for index in 0..staged.len() {
        if let Err(error) = move_file_without_overwrite(&staged[index].temporary, &staged[index].to)
        {
            let rollback_errors = rollback_finalizing(&staged, index);
            return Err(rename_failure_message(
                "finalize",
                &staged[index].to,
                error,
                rollback_errors,
            ));
        }
    }
    Ok(())
}

fn temporary_path(directory: &Path, operation_id: &str, index: usize) -> Result<PathBuf, String> {
    for attempt in 0..100 {
        let candidate =
            directory.join(format!(".ihub-rename-{operation_id}-{index}-{attempt}.tmp"));
        match fs::symlink_metadata(&candidate) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(candidate),
            Ok(_) => continue,
            Err(error) => {
                return Err(format!(
                    "Could not reserve a temporary rename path {}: {error}",
                    candidate.display()
                ))
            }
        }
    }
    Err("Could not allocate a safe temporary path for batch rename.".to_owned())
}

fn rollback_staging(staged: &[StagedRenameItem]) -> Vec<String> {
    staged
        .iter()
        .rev()
        .filter_map(|item| move_file_without_overwrite(&item.temporary, &item.from).err())
        .collect()
}

fn rollback_finalizing(staged: &[StagedRenameItem], failed_index: usize) -> Vec<String> {
    let mut errors = Vec::new();
    for item in staged[..failed_index].iter().rev() {
        if let Err(error) = move_file_without_overwrite(&item.to, &item.from) {
            errors.push(error);
        }
    }
    for item in staged[failed_index..].iter().rev() {
        if let Err(error) = move_file_without_overwrite(&item.temporary, &item.from) {
            errors.push(error);
        }
    }
    errors
}

/// Uses a hard link plus source removal instead of `rename`. Creating the link
/// fails if the destination appears after validation, so we never replace an
/// unrelated existing file on platforms where `rename` silently overwrites.
fn move_file_without_overwrite(source: &Path, destination: &Path) -> Result<(), String> {
    fs::hard_link(source, destination).map_err(|error| {
        format!(
            "Could not create a no-overwrite link from {} to {}: {error}",
            source.display(),
            destination.display()
        )
    })?;
    if let Err(error) = fs::remove_file(source) {
        let cleanup = fs::remove_file(destination);
        let cleanup_note = match cleanup {
            Ok(()) => " The temporary destination was removed.".to_owned(),
            Err(cleanup_error) => format!(
                " Cleanup of {} also failed: {cleanup_error}.",
                destination.display()
            ),
        };
        return Err(format!(
            "Could not remove original file {} after safely linking it: {error}.{cleanup_note}",
            source.display()
        ));
    }
    Ok(())
}

fn rename_failure_message(
    phase: &str,
    path: &Path,
    error: String,
    rollback_errors: Vec<String>,
) -> String {
    if rollback_errors.is_empty() {
        format!(
            "Could not {phase} batch rename at {}: {error}. The completed changes were rolled back.",
            path.display()
        )
    } else {
        format!(
            "Could not {phase} batch rename at {}: {error}. Automatic rollback was incomplete: {}.",
            path.display(),
            rollback_errors.join("; ")
        )
    }
}

fn unique_operation_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{
        apply_batch_rename, format_json_text, preview_batch_rename, query_json_text,
        validate_portable_file_name, MAX_JSON_FORMAT_INPUT_BYTES, MAX_JSON_QUERY_MATCHES,
    };

    #[test]
    fn formats_valid_json_with_requested_indentation() {
        let result = format_json_text(r#"{"tools":["json"]}"#, Some(4));
        assert!(result.valid);
        assert_eq!(
            result.formatted.as_deref(),
            Some("{\n    \"tools\": [\n        \"json\"\n    ]\n}")
        );
        assert!(result.error.is_none());
    }

    #[test]
    fn reports_invalid_json_without_throwing() {
        let result = format_json_text("{ nope", None);
        assert!(!result.valid);
        assert!(result.formatted.is_none());
        assert!(result
            .error
            .as_deref()
            .is_some_and(|error| error.starts_with("Invalid JSON:")));
    }

    #[test]
    fn json_format_rejects_oversized_input_before_parsing() {
        let input = format!("\"{}\"", "x".repeat(MAX_JSON_FORMAT_INPUT_BYTES));

        let result = format_json_text(&input, None);

        assert!(!result.valid);
        assert!(result.formatted.is_none());
        assert_eq!(
            result.error.as_deref(),
            Some("JSON format input is limited to 2097152 bytes.")
        );
    }

    #[test]
    fn json_query_reads_nested_fields_and_indexes() {
        let result = query_json_text(
            r#"{"items":[{"id":"first"},{"id":"second"}]}"#,
            "$.items[1].id",
        );
        assert!(result.valid, "{result:?}");
        assert_eq!(result.matches, 1);
        assert_eq!(result.formatted.as_deref(), Some("\"second\""));
    }

    #[test]
    fn json_query_supports_quoted_fields_and_wildcards() {
        let result = query_json_text(
            r#"{"first key":[{"display-name":"A"},{"display-name":"B"}]}"#,
            "$['first key'][*]['display-name']",
        );
        assert!(result.valid, "{result:?}");
        assert_eq!(result.matches, 2);
        assert_eq!(result.formatted.as_deref(), Some("[\n  \"A\",\n  \"B\"\n]"));
    }

    #[test]
    fn json_query_returns_an_empty_array_when_nothing_matches() {
        let result = query_json_text(r#"{"items":[]}"#, "$.items[*].id");
        assert!(result.valid, "{result:?}");
        assert_eq!(result.matches, 0);
        assert_eq!(result.formatted.as_deref(), Some("[]"));
    }

    #[test]
    fn json_query_rejects_expressions_and_bounds_excessive_matches() {
        for selector in ["items", "$..items", "$.items[?(@.id)]", "$.items[-1]"] {
            let result = query_json_text(r#"{"items":[1]}"#, selector);
            assert!(!result.valid, "{selector} should be rejected");
            assert!(result.error.is_some());
        }

        let input = serde_json::to_string(&serde_json::json!({
            "items": (0..=MAX_JSON_QUERY_MATCHES).collect::<Vec<_>>()
        }))
        .expect("serialize large JSON test input");
        let result = query_json_text(&input, "$.items[*]");
        assert!(!result.valid);
        assert!(result
            .error
            .as_deref()
            .is_some_and(|error| error.contains("Narrow the path")));
    }

    #[test]
    fn rejects_unsafe_or_nonportable_file_names() {
        for name in [
            "",
            ".",
            "..",
            "../escape",
            "a/b",
            "a\\b",
            "NUL.txt",
            "report. ",
        ] {
            assert!(
                validate_portable_file_name(name).is_err(),
                "{name} should be rejected"
            );
        }
        assert!(validate_portable_file_name("2026-report.json").is_ok());
    }

    #[test]
    fn only_applies_an_exact_previewed_rename_plan() {
        let directory = temporary_test_directory("batch-rename");
        let source = directory.join("draft-note.txt");
        fs::write(&source, "iHub").expect("write source test file");

        let preview = preview_batch_rename(
            directory.to_string_lossy().into_owned(),
            "draft-".to_owned(),
            "final-".to_owned(),
            Some(false),
            None,
            None,
        )
        .expect("create preview");
        assert!(preview.can_apply);
        assert_eq!(preview.items.len(), 1);

        let error = apply_batch_rename(directory.to_string_lossy().into_owned(), Vec::new())
            .expect_err("unpreviewed item list must be rejected");
        assert!(error.contains("has not been previewed"));

        let result = apply_batch_rename(directory.to_string_lossy().into_owned(), preview.items)
            .expect("apply exact preview");
        assert_eq!(result.renamed, 1);
        assert!(!source.exists());
        assert_eq!(
            fs::read_to_string(directory.join("final-note.txt")).expect("read renamed file"),
            "iHub"
        );

        fs::remove_dir_all(&directory).expect("remove scoped test directory");
    }

    #[test]
    fn previews_numbered_renames_in_a_stable_sorted_order() {
        let directory = temporary_test_directory("batch-rename-numbered");
        for name in ["IMG_z.png", "IMG_a.png", "IMG_m.png"] {
            fs::write(directory.join(name), name).expect("write numbered rename source");
        }

        let preview = preview_batch_rename(
            directory.to_string_lossy().into_owned(),
            "IMG_".to_owned(),
            "trip-{n}-".to_owned(),
            Some(false),
            Some(7),
            Some(3),
        )
        .expect("create numbered preview");

        assert!(preview.can_apply, "{preview:?}");
        let renamed: Vec<(String, String)> = preview
            .items
            .iter()
            .map(|item| {
                let from = PathBuf::from(&item.from)
                    .file_name()
                    .expect("source name")
                    .to_string_lossy()
                    .into_owned();
                let to = PathBuf::from(&item.to)
                    .file_name()
                    .expect("destination name")
                    .to_string_lossy()
                    .into_owned();
                (from, to)
            })
            .collect();
        assert_eq!(
            renamed,
            vec![
                ("IMG_a.png".to_owned(), "trip-007-a.png".to_owned()),
                ("IMG_m.png".to_owned(), "trip-008-m.png".to_owned()),
                ("IMG_z.png".to_owned(), "trip-009-z.png".to_owned()),
            ]
        );

        fs::remove_dir_all(&directory).expect("remove scoped test directory");
    }

    #[test]
    fn rejects_invalid_numbered_rename_sequence_options() {
        let directory = temporary_test_directory("batch-rename-numbered-options");
        fs::write(directory.join("IMG_a.png"), "a").expect("write numbered rename source");

        let error = preview_batch_rename(
            directory.to_string_lossy().into_owned(),
            "IMG_".to_owned(),
            "trip-{n}-".to_owned(),
            Some(false),
            Some(0),
            Some(3),
        )
        .expect_err("zero sequence start must be rejected");
        assert!(error.contains("start at 1"));

        fs::remove_dir_all(&directory).expect("remove scoped test directory");
    }

    fn temporary_test_directory(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("ihub-{label}-{}-{nanos}", std::process::id()));
        fs::create_dir(&directory).expect("create scoped test directory");
        directory
    }
}
