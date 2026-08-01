//! Durable, plugin-scoped document storage for imported uTools packages.
//!
//! The compatibility database is deliberately separate from ordinary plugin
//! settings: uTools documents may be as large as 1 MiB and use optimistic
//! `_rev` updates. Each validated plugin owns one bounded JSON database file;
//! writes stage and atomically replace that file so a crash cannot publish a
//! partially encoded document set.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::UNIX_EPOCH,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::host_log;

const DATABASE_SCHEMA_VERSION: u32 = 1;
const DATABASE_DIRECTORY: &str = "utools-document-db-v1";
const MAX_DOCUMENT_ID_BYTES: usize = 512;
const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;
const MAX_DOCUMENTS_PER_PLUGIN: usize = 2_048;
const MAX_DATABASE_FILE_BYTES: usize = 32 * 1024 * 1024;
const MAX_BULK_DOCUMENTS: usize = 16;
const MAX_BULK_INPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_ALL_DOC_IDS: usize = 256;
pub const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
const MAX_ATTACHMENT_CONTENT_TYPE_BYTES: usize = 255;

#[derive(Clone)]
pub struct UtoolsDocumentStore {
    root: Arc<PathBuf>,
    databases: Arc<Mutex<HashMap<String, PersistedDatabase>>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedDatabase {
    schema_version: u32,
    plugin_id: String,
    documents: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    attachments: BTreeMap<String, PersistedAttachment>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedAttachment {
    content_type: String,
    byte_length: u64,
    sha256: String,
}

impl PersistedDatabase {
    fn empty(plugin_id: &str) -> Self {
        Self {
            schema_version: DATABASE_SCHEMA_VERSION,
            plugin_id: plugin_id.to_owned(),
            documents: BTreeMap::new(),
            attachments: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UtoolsDbResult {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl UtoolsDbResult {
    fn success(id: String, rev: String) -> Self {
        Self {
            id,
            rev: Some(rev),
            ok: Some(true),
            error: None,
            name: None,
            message: None,
        }
    }

    fn failure(id: impl Into<String>, name: &str, message: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            rev: None,
            ok: None,
            error: Some(true),
            name: Some(name.to_owned()),
            message: Some(message.into()),
        }
    }
}

impl UtoolsDocumentStore {
    pub fn new(app_data_dir: PathBuf) -> Self {
        Self {
            root: Arc::new(app_data_dir.join(DATABASE_DIRECTORY)),
            databases: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn get(&self, plugin_id: &str, id: &str) -> Result<Option<Value>, String> {
        validate_plugin_id(plugin_id)?;
        validate_document_id(id)?;
        let mut databases = self.lock_databases();
        let database = self.load_locked(&mut databases, plugin_id)?;
        Ok(database.documents.get(id).cloned())
    }

    pub fn put(&self, plugin_id: &str, document: Value) -> Result<UtoolsDbResult, String> {
        validate_plugin_id(plugin_id)?;
        let mut databases = self.lock_databases();
        let current = self.load_locked(&mut databases, plugin_id)?.clone();
        let mut next = current;
        let result = apply_put(&mut next, document);
        if result.ok == Some(true) {
            self.persist(plugin_id, &next)?;
            databases.insert(plugin_id.to_owned(), next);
        }
        Ok(result)
    }

    pub fn remove(&self, plugin_id: &str, target: &Value) -> Result<UtoolsDbResult, String> {
        validate_plugin_id(plugin_id)?;
        let (id, supplied_rev) = remove_target(target)?;
        let mut databases = self.lock_databases();
        let current = self.load_locked(&mut databases, plugin_id)?.clone();
        let Some(document) = current.documents.get(&id) else {
            return Ok(UtoolsDbResult::failure(
                id,
                "not_found",
                "The document does not exist.",
            ));
        };
        let current_rev = document_revision(document)
            .ok_or_else(|| "A stored uTools document has an invalid revision.".to_owned())?
            .to_owned();
        if supplied_rev
            .as_deref()
            .is_some_and(|rev| rev != current_rev)
        {
            return Ok(conflict_result(&id));
        }

        let attachment = current.attachments.get(&id).cloned();
        let mut next = current;
        next.documents.remove(&id);
        next.attachments.remove(&id);
        self.persist(plugin_id, &next)?;
        databases.insert(plugin_id.to_owned(), next);
        if attachment.is_some() {
            let path = self.attachment_path(plugin_id, &id)?;
            if path.exists() {
                if let Err(error) = validate_regular_attachment_file(&path).and_then(|()| {
                    fs::remove_file(&path)
                        .map_err(|error| format!("Could not remove the uTools attachment: {error}"))
                }) {
                    host_log::warn("plugins", error);
                }
            }
        }
        Ok(UtoolsDbResult::success(id, current_rev))
    }

    pub fn post_attachment(
        &self,
        plugin_id: &str,
        id: &str,
        bytes: &[u8],
        content_type: &str,
    ) -> Result<UtoolsDbResult, String> {
        validate_plugin_id(plugin_id)?;
        validate_document_id(id)?;
        validate_attachment_content_type(content_type)?;
        if bytes.is_empty() || bytes.len() > MAX_ATTACHMENT_BYTES {
            return Err(format!(
                "uTools attachments must contain 1-{MAX_ATTACHMENT_BYTES} bytes."
            ));
        }

        let mut databases = self.lock_databases();
        let current = self.load_locked(&mut databases, plugin_id)?.clone();
        if current.documents.contains_key(id) || current.attachments.contains_key(id) {
            return Ok(conflict_result(id));
        }
        let mut next = current;
        let digest = format!("{:x}", Sha256::digest(bytes));
        let result = apply_put(
            &mut next,
            serde_json::json!({
                "_id": id,
                "_attachments": {
                    "attachment": {
                        "content_type": content_type,
                        "digest": format!("sha256-{digest}"),
                        "length": bytes.len(),
                        "stub": true,
                    }
                }
            }),
        );
        if result.ok != Some(true) {
            return Ok(result);
        }
        next.attachments.insert(
            id.to_owned(),
            PersistedAttachment {
                content_type: content_type.to_owned(),
                byte_length: bytes.len() as u64,
                sha256: digest,
            },
        );

        let attachment_path = self.write_attachment(plugin_id, id, bytes)?;
        if let Err(error) = self.persist(plugin_id, &next) {
            if let Err(cleanup_error) = fs::remove_file(&attachment_path) {
                host_log::warn(
                    "plugins",
                    format!(
                        "Could not remove an attachment after its database write failed: {cleanup_error}"
                    ),
                );
            }
            return Err(error);
        }
        databases.insert(plugin_id.to_owned(), next);
        Ok(result)
    }

    pub fn get_attachment(&self, plugin_id: &str, id: &str) -> Result<Option<Vec<u8>>, String> {
        validate_plugin_id(plugin_id)?;
        validate_document_id(id)?;
        let mut databases = self.lock_databases();
        let metadata = self
            .load_locked(&mut databases, plugin_id)?
            .attachments
            .get(id)
            .cloned();
        let Some(metadata) = metadata else {
            return Ok(None);
        };
        let bytes = read_attachment_file(&self.attachment_path(plugin_id, id)?, &metadata)?;
        Ok(Some(bytes))
    }

    pub fn get_attachment_type(&self, plugin_id: &str, id: &str) -> Result<Option<String>, String> {
        validate_plugin_id(plugin_id)?;
        validate_document_id(id)?;
        let mut databases = self.lock_databases();
        Ok(self
            .load_locked(&mut databases, plugin_id)?
            .attachments
            .get(id)
            .map(|metadata| metadata.content_type.clone()))
    }

    pub fn bulk_docs(
        &self,
        plugin_id: &str,
        documents: Vec<Value>,
    ) -> Result<Vec<UtoolsDbResult>, String> {
        validate_plugin_id(plugin_id)?;
        if documents.is_empty() || documents.len() > MAX_BULK_DOCUMENTS {
            return Err(format!(
                "uTools bulkDocs accepts between 1 and {MAX_BULK_DOCUMENTS} documents."
            ));
        }
        let encoded_size = serde_json::to_vec(&documents)
            .map_err(|error| format!("Could not encode uTools bulk documents: {error}"))?
            .len();
        if encoded_size > MAX_BULK_INPUT_BYTES {
            return Err(format!(
                "uTools bulkDocs input exceeds the {MAX_BULK_INPUT_BYTES}-byte host limit."
            ));
        }

        let mut databases = self.lock_databases();
        let current = self.load_locked(&mut databases, plugin_id)?.clone();
        let mut next = current;
        let mut changed = false;
        let results = documents
            .into_iter()
            .map(|document| {
                let result = apply_put(&mut next, document);
                changed |= result.ok == Some(true);
                result
            })
            .collect::<Vec<_>>();
        if changed {
            self.persist(plugin_id, &next)?;
            databases.insert(plugin_id.to_owned(), next);
        }
        Ok(results)
    }

    pub fn all_docs(
        &self,
        plugin_id: &str,
        selector: Option<&Value>,
    ) -> Result<Vec<Value>, String> {
        validate_plugin_id(plugin_id)?;
        let mut databases = self.lock_databases();
        let database = self.load_locked(&mut databases, plugin_id)?;
        match selector {
            None | Some(Value::Null) => Ok(database.documents.values().cloned().collect()),
            Some(Value::String(prefix)) => {
                if prefix.len() > MAX_DOCUMENT_ID_BYTES || prefix.chars().any(char::is_control) {
                    return Err("uTools allDocs prefix is invalid or too long.".to_owned());
                }
                Ok(database
                    .documents
                    .range(prefix.clone()..)
                    .take_while(|(id, _)| id.starts_with(prefix))
                    .map(|(_, document)| document.clone())
                    .collect())
            }
            Some(Value::Array(ids)) => {
                if ids.len() > MAX_ALL_DOC_IDS {
                    return Err(format!(
                        "uTools allDocs accepts at most {MAX_ALL_DOC_IDS} document IDs."
                    ));
                }
                let mut seen = HashSet::new();
                let mut documents = Vec::new();
                for id in ids {
                    let Some(id) = id.as_str() else {
                        return Err("uTools allDocs IDs must be strings.".to_owned());
                    };
                    validate_document_id(id)?;
                    if seen.insert(id) {
                        if let Some(document) = database.documents.get(id) {
                            documents.push(document.clone());
                        }
                    }
                }
                Ok(documents)
            }
            Some(_) => Err("uTools allDocs accepts a prefix string or ID array.".to_owned()),
        }
    }

    pub fn remove_plugin(&self, plugin_id: &str) -> Result<bool, String> {
        validate_plugin_id(plugin_id)?;
        self.lock_databases().remove(plugin_id);
        if !self.root.exists() {
            return Ok(false);
        }
        validate_database_directory(self.root.as_ref())?;
        let primary_name = format!("{plugin_id}.json");
        let mut removed = false;
        for entry in fs::read_dir(self.root.as_ref())
            .map_err(|error| format!("Could not inspect the uTools database directory: {error}"))?
        {
            let entry = entry.map_err(|error| {
                format!("Could not inspect a uTools database artifact: {error}")
            })?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if name != primary_name && !is_database_sidecar_name(&name, plugin_id) {
                continue;
            }
            let path = entry.path();
            validate_regular_database_file(&path)?;
            fs::remove_file(&path)
                .map_err(|error| format!("Could not remove the uTools plugin database: {error}"))?;
            removed = true;
        }
        Ok(removed)
    }

    fn lock_databases(&self) -> std::sync::MutexGuard<'_, HashMap<String, PersistedDatabase>> {
        self.databases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn load_locked<'a>(
        &self,
        databases: &'a mut HashMap<String, PersistedDatabase>,
        plugin_id: &str,
    ) -> Result<&'a PersistedDatabase, String> {
        if !databases.contains_key(plugin_id) {
            let path = self.database_path(plugin_id)?;
            let database = load_database(&path, plugin_id)?;
            databases.insert(plugin_id.to_owned(), database);
        }
        Ok(databases
            .get(plugin_id)
            .expect("the requested database was loaded while locked"))
    }

    fn database_path(&self, plugin_id: &str) -> Result<PathBuf, String> {
        validate_plugin_id(plugin_id)?;
        Ok(self.root.join(format!("{plugin_id}.json")))
    }

    fn attachment_path(&self, plugin_id: &str, id: &str) -> Result<PathBuf, String> {
        validate_plugin_id(plugin_id)?;
        validate_document_id(id)?;
        Ok(attachment_path(self.root.as_ref(), plugin_id, id))
    }

    fn write_attachment(&self, plugin_id: &str, id: &str, bytes: &[u8]) -> Result<PathBuf, String> {
        fs::create_dir_all(self.root.as_ref())
            .map_err(|error| format!("Could not create the uTools database directory: {error}"))?;
        validate_database_directory(self.root.as_ref())?;
        let final_path = self.attachment_path(plugin_id, id)?;
        if final_path.exists() {
            validate_regular_attachment_file(&final_path)?;
            fs::remove_file(&final_path).map_err(|error| {
                format!("Could not replace an orphaned uTools attachment: {error}")
            })?;
        }
        let digest = attachment_id_digest(id);
        let temporary = self.root.join(format!(
            ".{plugin_id}.attachment.{digest}.{}.tmp",
            Uuid::new_v4().simple()
        ));
        let mut staged = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("Could not stage the uTools attachment: {error}"))?;
        if let Err(error) = staged.write_all(bytes).and_then(|()| staged.sync_all()) {
            drop(staged);
            let _ = fs::remove_file(&temporary);
            return Err(format!(
                "Could not flush the staged uTools attachment: {error}"
            ));
        }
        drop(staged);
        fs::rename(&temporary, &final_path).map_err(|error| {
            let _ = fs::remove_file(&temporary);
            format!("Could not save the uTools attachment: {error}")
        })?;
        Ok(final_path)
    }

    fn persist(&self, plugin_id: &str, database: &PersistedDatabase) -> Result<(), String> {
        validate_database(database, plugin_id)?;
        fs::create_dir_all(self.root.as_ref())
            .map_err(|error| format!("Could not create the uTools database directory: {error}"))?;
        validate_database_directory(self.root.as_ref())?;
        let encoded = serde_json::to_vec(database)
            .map_err(|error| format!("Could not encode the uTools database: {error}"))?;
        if encoded.len() > MAX_DATABASE_FILE_BYTES {
            return Err(format!(
                "The uTools plugin database exceeds the {MAX_DATABASE_FILE_BYTES}-byte host limit."
            ));
        }

        let primary = self.database_path(plugin_id)?;
        let token = Uuid::new_v4().simple();
        let temporary = self.root.join(format!(".{plugin_id}.{token}.tmp"));
        let mut staged = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("Could not stage the uTools database: {error}"))?;
        if let Err(error) = staged.write_all(&encoded).and_then(|()| staged.sync_all()) {
            drop(staged);
            let _ = fs::remove_file(&temporary);
            return Err(format!(
                "Could not flush the staged uTools database: {error}"
            ));
        }
        drop(staged);
        if !primary.exists() {
            return fs::rename(&temporary, &primary).map_err(|error| {
                let _ = fs::remove_file(&temporary);
                format!("Could not save the uTools database: {error}")
            });
        }
        validate_regular_database_file(&primary)?;
        let backup = self.root.join(format!(".{plugin_id}.{token}.backup"));
        fs::rename(&primary, &backup).map_err(|error| {
            let _ = fs::remove_file(&temporary);
            format!("Could not prepare the uTools database update: {error}")
        })?;
        if let Err(error) = fs::rename(&temporary, &primary) {
            let restore = fs::rename(&backup, &primary);
            let _ = fs::remove_file(&temporary);
            return Err(match restore {
                Ok(()) => format!("Could not save the uTools database: {error}"),
                Err(restore_error) => format!(
                    "Could not save the uTools database ({error}) or restore its prior file ({restore_error})."
                ),
            });
        }
        if let Err(error) = fs::remove_file(&backup) {
            host_log::warn(
                "plugins",
                format!("Could not remove a replaced uTools database backup: {error}"),
            );
        }
        Ok(())
    }
}

fn apply_put(database: &mut PersistedDatabase, document: Value) -> UtoolsDbResult {
    let Some(mut document) = document.as_object().cloned() else {
        return UtoolsDbResult::failure(
            "",
            "bad_request",
            "A database document must be an object.",
        );
    };
    let id = match document.get("_id") {
        None => {
            let id = Uuid::new_v4().to_string();
            document.insert("_id".to_owned(), Value::String(id.clone()));
            id
        }
        Some(Value::String(id)) if validate_document_id(id).is_ok() => id.clone(),
        Some(_) => {
            return UtoolsDbResult::failure(
                "",
                "bad_request",
                "Document _id must be a bounded nonempty string.",
            )
        }
    };
    let supplied_rev = match document.get("_rev") {
        None => None,
        Some(Value::String(rev)) if valid_revision(rev) => Some(rev.clone()),
        Some(_) => {
            return UtoolsDbResult::failure(
                id,
                "bad_request",
                "Document _rev must be a valid revision string.",
            )
        }
    };

    if database.attachments.contains_key(&id) {
        return UtoolsDbResult::failure(
            id,
            "conflict",
            "Attachment documents cannot be updated; remove and recreate the attachment.",
        );
    }

    let generation = match database.documents.get(&id) {
        None if supplied_rev.is_some() => return conflict_result(&id),
        None => {
            if database.documents.len() >= MAX_DOCUMENTS_PER_PLUGIN {
                return UtoolsDbResult::failure(
                    id,
                    "database_full",
                    format!(
                        "A plugin database may contain at most {MAX_DOCUMENTS_PER_PLUGIN} documents."
                    ),
                );
            }
            1
        }
        Some(current) => {
            let Some(current_rev) = document_revision(current) else {
                return UtoolsDbResult::failure(
                    id,
                    "corrupt_document",
                    "The stored document revision is invalid.",
                );
            };
            if supplied_rev.as_deref() != Some(current_rev) {
                return conflict_result(&id);
            }
            revision_generation(current_rev).saturating_add(1)
        }
    };
    let revision = format!("{generation}-{}", Uuid::new_v4().simple());
    document.insert("_rev".to_owned(), Value::String(revision.clone()));
    let document = Value::Object(document);
    match validate_document(&id, &document) {
        Ok(()) => {
            database.documents.insert(id.clone(), document);
            UtoolsDbResult::success(id, revision)
        }
        Err(message) => UtoolsDbResult::failure(id, "bad_request", message),
    }
}

fn remove_target(target: &Value) -> Result<(String, Option<String>), String> {
    match target {
        Value::String(id) => {
            validate_document_id(id)?;
            Ok((id.clone(), None))
        }
        Value::Object(document) => {
            let id = document
                .get("_id")
                .and_then(Value::as_str)
                .ok_or_else(|| "uTools remove document requires _id.".to_owned())?;
            validate_document_id(id)?;
            let revision = match document.get("_rev") {
                None => None,
                Some(Value::String(rev)) if valid_revision(rev) => Some(rev.clone()),
                Some(_) => return Err("uTools remove document _rev is invalid.".to_owned()),
            };
            Ok((id.to_owned(), revision))
        }
        _ => Err("uTools remove accepts a document ID or document object.".to_owned()),
    }
}

fn conflict_result(id: &str) -> UtoolsDbResult {
    UtoolsDbResult::failure(
        id,
        "conflict",
        "Document update conflict: fetch the current revision and retry.",
    )
}

fn document_revision(document: &Value) -> Option<&str> {
    document.get("_rev").and_then(Value::as_str)
}

fn revision_generation(revision: &str) -> u64 {
    revision
        .split_once('-')
        .and_then(|(generation, _)| generation.parse().ok())
        .unwrap_or(0)
}

fn valid_revision(revision: &str) -> bool {
    let Some((generation, token)) = revision.split_once('-') else {
        return false;
    };
    generation.parse::<u64>().is_ok()
        && !token.is_empty()
        && token.len() <= 64
        && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_document_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > MAX_DOCUMENT_ID_BYTES || id.chars().any(char::is_control) {
        return Err(format!(
            "uTools document IDs must be nonempty, control-free, and at most {MAX_DOCUMENT_ID_BYTES} UTF-8 bytes."
        ));
    }
    Ok(())
}

fn validate_document(id: &str, document: &Value) -> Result<(), String> {
    validate_document_id(id)?;
    let object = document
        .as_object()
        .ok_or_else(|| "A uTools database document must be an object.".to_owned())?;
    if object.get("_id").and_then(Value::as_str) != Some(id) {
        return Err("A uTools database document _id does not match its key.".to_owned());
    }
    let revision = object
        .get("_rev")
        .and_then(Value::as_str)
        .filter(|revision| valid_revision(revision))
        .ok_or_else(|| "A uTools database document has an invalid _rev.".to_owned())?;
    if revision_generation(revision) == 0 {
        return Err("A uTools database revision generation must be positive.".to_owned());
    }
    let bytes = serde_json::to_vec(document)
        .map_err(|error| format!("Could not encode the uTools document: {error}"))?
        .len();
    if bytes > MAX_DOCUMENT_BYTES {
        return Err(format!(
            "A uTools document exceeds the {MAX_DOCUMENT_BYTES}-byte limit."
        ));
    }
    Ok(())
}

fn validate_database(database: &PersistedDatabase, plugin_id: &str) -> Result<(), String> {
    if database.schema_version != DATABASE_SCHEMA_VERSION || database.plugin_id != plugin_id {
        return Err("The uTools database identity or schema is invalid.".to_owned());
    }
    if database.documents.len() > MAX_DOCUMENTS_PER_PLUGIN {
        return Err("The uTools database contains too many documents.".to_owned());
    }
    for (id, document) in &database.documents {
        validate_document(id, document)?;
    }
    for (id, attachment) in &database.attachments {
        if !database.documents.contains_key(id) {
            return Err("A uTools attachment has no matching database document.".to_owned());
        }
        validate_attachment_metadata(attachment)?;
    }
    Ok(())
}

fn validate_attachment_content_type(content_type: &str) -> Result<(), String> {
    let Some((category, subtype)) = content_type.split_once('/') else {
        return Err("uTools attachment types must be MIME types such as image/png.".to_owned());
    };
    let valid_component = |value: &str| {
        !value.is_empty()
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(
                        byte,
                        b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-'
                    )
            })
    };
    if content_type.len() > MAX_ATTACHMENT_CONTENT_TYPE_BYTES
        || !content_type.is_ascii()
        || !valid_component(category)
        || !valid_component(subtype)
    {
        return Err("uTools attachment MIME type is invalid or too long.".to_owned());
    }
    Ok(())
}

fn validate_attachment_metadata(metadata: &PersistedAttachment) -> Result<(), String> {
    validate_attachment_content_type(&metadata.content_type)?;
    if metadata.byte_length == 0 || metadata.byte_length > MAX_ATTACHMENT_BYTES as u64 {
        return Err("A stored uTools attachment has an invalid byte length.".to_owned());
    }
    if metadata.sha256.len() != 64 || !metadata.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("A stored uTools attachment has an invalid digest.".to_owned());
    }
    Ok(())
}

fn attachment_id_digest(id: &str) -> String {
    format!("{:x}", Sha256::digest(id.as_bytes()))
}

fn attachment_path(root: &Path, plugin_id: &str, id: &str) -> PathBuf {
    root.join(format!(
        ".{plugin_id}.attachment.{}.bin",
        attachment_id_digest(id)
    ))
}

fn validate_regular_attachment_file(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("Could not inspect the uTools attachment file: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("The uTools attachment path must be a regular non-symlink file.".to_owned());
    }
    if metadata.len() == 0 || metadata.len() > MAX_ATTACHMENT_BYTES as u64 {
        return Err("The uTools attachment file exceeds the host size limit.".to_owned());
    }
    Ok(())
}

fn read_attachment_file(path: &Path, metadata: &PersistedAttachment) -> Result<Vec<u8>, String> {
    validate_regular_attachment_file(path)?;
    let bytes = fs::read(path)
        .map_err(|error| format!("Could not read the uTools attachment file: {error}"))?;
    if bytes.len() as u64 != metadata.byte_length
        || format!("{:x}", Sha256::digest(&bytes)) != metadata.sha256
    {
        return Err(
            "The uTools attachment does not match its persisted integrity metadata.".to_owned(),
        );
    }
    Ok(bytes)
}

fn validate_attachment_file_metadata(
    path: &Path,
    metadata: &PersistedAttachment,
) -> Result<(), String> {
    validate_regular_attachment_file(path)?;
    let length = fs::symlink_metadata(path)
        .map_err(|error| format!("Could not inspect the uTools attachment file: {error}"))?
        .len();
    if length != metadata.byte_length {
        return Err(
            "The uTools attachment length does not match its database metadata.".to_owned(),
        );
    }
    Ok(())
}

fn validate_plugin_id(plugin_id: &str) -> Result<(), String> {
    if !(2..=96).contains(&plugin_id.len())
        || !plugin_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err("Invalid plugin ID for the uTools database.".to_owned());
    }
    Ok(())
}

fn validate_regular_database_file(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("Could not inspect the uTools database file: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("The uTools database path must be a regular non-symlink file.".to_owned());
    }
    if metadata.len() > MAX_DATABASE_FILE_BYTES as u64 {
        return Err("The uTools database file exceeds the host size limit.".to_owned());
    }
    Ok(())
}

fn validate_database_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("Could not inspect the uTools database directory: {error}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("The uTools database root must be a regular non-symlink directory.".to_owned());
    }
    Ok(())
}

fn is_database_sidecar_name(name: &str, plugin_id: &str) -> bool {
    let prefix = format!(".{plugin_id}.");
    let Some(value) = name.strip_prefix(&prefix) else {
        return false;
    };
    let (token, recognized_suffix) = if let Some(token) = value.strip_suffix(".backup") {
        (token, true)
    } else if let Some(token) = value.strip_suffix(".tmp") {
        (token, true)
    } else {
        ("", false)
    };
    if recognized_suffix && token.len() == 32 && token.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return true;
    }

    let Some(attachment) = value.strip_prefix("attachment.") else {
        return false;
    };
    if let Some(digest) = attachment.strip_suffix(".bin") {
        return digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit());
    }
    let mut parts = attachment.split('.');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(digest), Some(token), Some("tmp"), None)
            if digest.len() == 64
                && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                && token.len() == 32
                && token.bytes().all(|byte| byte.is_ascii_hexdigit())
    )
}

fn recover_database_backup(path: &Path, plugin_id: &str) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    let Some(root) = path.parent() else {
        return Err("The uTools database path has no parent directory.".to_owned());
    };
    if !root.exists() {
        return Ok(());
    }
    validate_database_directory(root)?;

    let backup_suffix = ".backup";
    let prefix = format!(".{plugin_id}.");
    let mut candidates = Vec::new();
    let mut invalid_candidates = 0usize;
    for entry in fs::read_dir(root)
        .map_err(|error| format!("Could not inspect uTools database backups: {error}"))?
    {
        let entry = entry
            .map_err(|error| format!("Could not inspect a uTools database backup: {error}"))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(token) = name
            .strip_prefix(&prefix)
            .and_then(|value| value.strip_suffix(backup_suffix))
        else {
            continue;
        };
        if token.len() != 32 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            continue;
        }
        let candidate = entry.path();
        let database = load_database_file(&candidate, plugin_id);
        match database {
            Ok(_) => {
                let modified = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(UNIX_EPOCH);
                candidates.push((modified, name, candidate));
            }
            Err(error) => {
                invalid_candidates += 1;
                host_log::warn(
                    "plugins",
                    format!("Ignored an invalid interrupted uTools database backup: {error}"),
                );
            }
        }
    }
    candidates.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    if let Some((_, _, candidate)) = candidates.pop() {
        fs::rename(&candidate, path)
            .map_err(|error| format!("Could not recover the uTools database backup: {error}"))?;
        return Ok(());
    }
    if invalid_candidates > 0 {
        return Err("No valid interrupted uTools database backup could be recovered.".to_owned());
    }
    Ok(())
}

fn load_database_file(path: &Path, plugin_id: &str) -> Result<PersistedDatabase, String> {
    validate_regular_database_file(path)?;
    let bytes = fs::read(path)
        .map_err(|error| format!("Could not read the uTools database file: {error}"))?;
    let database = serde_json::from_slice::<PersistedDatabase>(&bytes)
        .map_err(|error| format!("Could not parse the uTools database file: {error}"))?;
    validate_database(&database, plugin_id)?;
    let root = path
        .parent()
        .ok_or_else(|| "The uTools database file has no parent directory.".to_owned())?;
    for (id, metadata) in &database.attachments {
        validate_attachment_file_metadata(&attachment_path(root, plugin_id, id), metadata)?;
    }
    Ok(database)
}

fn load_database(path: &Path, plugin_id: &str) -> Result<PersistedDatabase, String> {
    recover_database_backup(path, plugin_id)?;
    if !path.exists() {
        return Ok(PersistedDatabase::empty(plugin_id));
    }
    load_database_file(path, plugin_id)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::{json, Value};

    use super::{attachment_path, UtoolsDocumentStore, DATABASE_DIRECTORY};

    fn fixture(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("ihub-{label}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn documents_persist_and_require_the_current_revision() {
        let root = fixture("utools-documents");
        let plugin_id = "utools-documents-test";
        let store = UtoolsDocumentStore::new(root.clone());
        let created = store
            .put(plugin_id, json!({ "_id": "note/1", "text": "first" }))
            .expect("create document");
        assert_eq!(created.ok, Some(true));
        let revision = created.rev.expect("created revision");
        let conflict = store
            .put(plugin_id, json!({ "_id": "note/1", "text": "stale" }))
            .expect("conflict is a database result");
        assert_eq!(conflict.error, Some(true));
        assert_eq!(conflict.name.as_deref(), Some("conflict"));
        let updated = store
            .put(
                plugin_id,
                json!({ "_id": "note/1", "_rev": revision, "text": "updated" }),
            )
            .expect("update current revision");
        assert_eq!(updated.ok, Some(true));
        drop(store);

        let reopened = UtoolsDocumentStore::new(root.clone());
        assert_eq!(
            reopened
                .get(plugin_id, "note/1")
                .expect("read persisted document")
                .and_then(|value| value.get("text").cloned()),
            Some(json!("updated"))
        );
        fs::remove_dir_all(root).expect("cleanup document fixture");
    }

    #[test]
    fn bulk_and_all_docs_are_bounded_plugin_scoped_and_sorted() {
        let root = fixture("utools-document-bulk");
        let store = UtoolsDocumentStore::new(root.clone());
        let results = store
            .bulk_docs(
                "utools-plugin-one",
                vec![
                    json!({ "_id": "task/b", "value": 2 }),
                    json!({ "_id": "task/a", "value": 1 }),
                    json!({ "_id": "other/c", "value": 3 }),
                ],
            )
            .expect("bulk create documents");
        assert!(results.iter().all(|result| result.ok == Some(true)));
        let prefixed = store
            .all_docs("utools-plugin-one", Some(&json!("task/")))
            .expect("prefix documents");
        assert_eq!(
            prefixed
                .iter()
                .filter_map(|document| document.get("_id").and_then(Value::as_str))
                .collect::<Vec<_>>(),
            vec!["task/a", "task/b"]
        );
        assert!(store
            .all_docs("utools-plugin-two", None)
            .expect("neighbor database")
            .is_empty());
        fs::remove_dir_all(root).expect("cleanup bulk fixture");
    }

    #[test]
    fn remove_accepts_id_and_rejects_stale_document_revisions() {
        let root = fixture("utools-document-remove");
        let store = UtoolsDocumentStore::new(root.clone());
        let created = store
            .put("utools-remove-test", json!({ "_id": "doc", "value": true }))
            .expect("create removable document");
        let stale = store
            .remove(
                "utools-remove-test",
                &json!({ "_id": "doc", "_rev": "1-deadbeef" }),
            )
            .expect("stale removal result");
        assert_eq!(stale.name.as_deref(), Some("conflict"));
        let removed = store
            .remove("utools-remove-test", &json!("doc"))
            .expect("remove by id");
        assert_eq!(removed.ok, Some(true));
        assert_eq!(removed.rev, created.rev);
        assert!(store
            .get("utools-remove-test", "doc")
            .expect("read removed document")
            .is_none());
        fs::remove_dir_all(root).expect("cleanup remove fixture");
    }

    #[test]
    fn reopens_from_a_valid_interrupted_backup() {
        let root = fixture("utools-document-recovery");
        let plugin_id = "utools-recovery-test";
        let store = UtoolsDocumentStore::new(root.clone());
        store
            .put(plugin_id, json!({ "_id": "saved", "value": 42 }))
            .expect("persist recoverable document");
        drop(store);

        let database_root = root.join(DATABASE_DIRECTORY);
        fs::rename(
            database_root.join(format!("{plugin_id}.json")),
            database_root.join(format!(
                ".{plugin_id}.{}.backup",
                uuid::Uuid::new_v4().simple()
            )),
        )
        .expect("simulate interrupted database replacement");

        let reopened = UtoolsDocumentStore::new(root.clone());
        assert_eq!(
            reopened
                .get(plugin_id, "saved")
                .expect("recover database")
                .and_then(|value| value.get("value").cloned()),
            Some(json!(42))
        );
        fs::remove_dir_all(root).expect("cleanup recovery fixture");
    }

    #[test]
    fn attachments_are_immutable_integrity_checked_and_removed_with_their_document() {
        let root = fixture("utools-attachments");
        let plugin_id = "utools-attachment-test";
        let store = UtoolsDocumentStore::new(root.clone());
        let created = store
            .post_attachment(plugin_id, "asset/logo", b"safe attachment", "text/plain")
            .expect("create attachment");
        assert_eq!(created.ok, Some(true));
        assert_eq!(
            store
                .get_attachment_type(plugin_id, "asset/logo")
                .expect("attachment type"),
            Some("text/plain".to_owned())
        );
        assert_eq!(
            store
                .get_attachment(plugin_id, "asset/logo")
                .expect("attachment bytes"),
            Some(b"safe attachment".to_vec())
        );
        assert!(store
            .get(plugin_id, "asset/logo")
            .expect("attachment document")
            .and_then(|document| document.get("_attachments").cloned())
            .is_some());
        assert_eq!(
            store
                .put(
                    plugin_id,
                    json!({ "_id": "asset/logo", "_rev": created.rev, "changed": true }),
                )
                .expect("immutable update result")
                .name
                .as_deref(),
            Some("conflict")
        );
        assert_eq!(
            store
                .post_attachment(plugin_id, "asset/logo", b"replacement", "text/plain")
                .expect("duplicate attachment result")
                .name
                .as_deref(),
            Some("conflict")
        );
        drop(store);

        let reopened = UtoolsDocumentStore::new(root.clone());
        assert_eq!(
            reopened
                .get_attachment(plugin_id, "asset/logo")
                .expect("reopened attachment"),
            Some(b"safe attachment".to_vec())
        );
        let stored_attachment =
            attachment_path(&root.join(DATABASE_DIRECTORY), plugin_id, "asset/logo");
        fs::write(&stored_attachment, b"evil attachment").expect("tamper attachment bytes");
        assert!(reopened
            .get_attachment(plugin_id, "asset/logo")
            .expect_err("same-size tampering must fail")
            .contains("integrity metadata"));
        fs::write(&stored_attachment, b"safe attachment").expect("restore attachment fixture");
        assert_eq!(
            reopened
                .remove(plugin_id, &json!("asset/logo"))
                .expect("remove attachment document")
                .ok,
            Some(true)
        );
        assert!(reopened
            .get_attachment(plugin_id, "asset/logo")
            .expect("removed attachment")
            .is_none());
        fs::remove_dir_all(root).expect("cleanup attachment fixture");
    }
}
