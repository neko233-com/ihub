use std::{
    env, fs,
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models::{PluginCommandInfo, PluginCommandResult, PluginInfo};

const MANIFEST_NAMES: [&str; 2] = ["ihub.plugin.json", "plugin.json"];
const SOURCE_RECORD: &str = ".ihub-source.json";
const MAX_CAPTURED_OUTPUT_BYTES: usize = 1_000_000;

#[derive(Clone, Debug)]
pub struct PluginManager {
    root: Arc<PathBuf>,
    install_lock: Arc<Mutex<()>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginManifest {
    id: String,
    name: String,
    #[serde(default = "default_version")]
    version: String,
    description: Option<String>,
    /// Legacy frontend declaration. v1 manifests use `entry.frontend` instead.
    frontend: Option<FrontendDeclaration>,
    entry: Option<EntryDeclaration>,
    backend: Option<BackendDeclaration>,
    contributes: Option<PluginContributions>,
    /// Legacy command declaration. v1 manifests use `contributes.commands`.
    #[serde(default)]
    commands: Vec<PluginCommandDeclaration>,
    /// v1 permissions are deliberately optional here so legacy manifests keep
    /// working. Missing declarations grant no sensitive frontend-host access.
    #[serde(default)]
    permissions: PluginPermissions,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginPermissions {
    clipboard: Option<ClipboardPermissions>,
    shell: Option<ShellPermissions>,
    #[serde(default)]
    notifications: bool,
    process: Option<ProcessPermissions>,
}

#[derive(Debug, Default, Deserialize)]
struct ClipboardPermissions {
    #[serde(default)]
    read: bool,
    #[serde(default)]
    write: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShellPermissions {
    #[serde(default)]
    open_path: bool,
    #[serde(default)]
    open_external: bool,
}

#[derive(Debug, Default, Deserialize)]
struct ProcessPermissions {
    #[serde(default)]
    spawn: bool,
}

#[derive(Debug, Deserialize)]
struct EntryDeclaration {
    frontend: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum FrontendDeclaration {
    Entry(String),
    Detailed { entry: String },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BackendDeclaration {
    /// Legacy single-binary declaration.
    binary: Option<String>,
    protocol: Option<String>,
    #[serde(default)]
    binaries: Vec<PluginBinaryDeclaration>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginBinaryDeclaration {
    target: String,
    path: String,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PluginContributions {
    #[serde(default)]
    commands: Vec<PluginCommandDeclaration>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PluginCommandDeclaration {
    id: String,
    #[serde(default)]
    name: Option<String>,
    title: Option<String>,
    description: Option<String>,
    subtitle: Option<String>,
    binary: Option<String>,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceRecord {
    source: String,
    installed_at: String,
    #[serde(default)]
    commit: Option<String>,
}

impl PluginManager {
    pub fn new() -> Self {
        Self {
            root: Arc::new(default_plugin_root()),
            install_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn list(&self) -> Vec<PluginInfo> {
        if self.ensure_root().is_err() {
            return Vec::new();
        }
        let mut plugins = fs::read_dir(self.root.as_ref())
            .ok()
            .into_iter()
            .flat_map(|entries| entries.flatten())
            .filter_map(|entry| {
                let path = entry.path();
                if !path.is_dir() || is_internal_dir(&path) {
                    return None;
                }
                self.read_plugin_info(&path).ok()
            })
            .collect::<Vec<_>>();
        plugins.sort_unstable_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.id.cmp(&right.id))
        });
        plugins
    }

    pub fn install_from_git(&self, source: &str) -> Result<PluginInfo, String> {
        let source = normalize_git_source(source)?;
        let _install_guard = self
            .install_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.ensure_root()?;

        let staging = self.root.join(format!(".staging-{}", unique_suffix()));
        if staging.exists() {
            return Err(
                "A plugin installation staging directory already exists; try again.".to_owned(),
            );
        }
        let clone_output = Command::new("git")
            .args(["clone", "--depth", "1", &source])
            .arg(&staging)
            .output()
            .map_err(|error| format!("Unable to start git. Install Git and retry: {error}"))?;
        if !clone_output.status.success() {
            let _ = fs::remove_dir_all(&staging);
            return Err(format!(
                "Git clone failed: {}",
                readable_output(&clone_output.stderr)
            ));
        }
        let commit = git_revision(&staging);

        let installation = (|| {
            let manifest_path = find_manifest(&staging).ok_or_else(|| {
                "The repository does not contain ihub.plugin.json or plugin.json.".to_owned()
            })?;
            let manifest = read_manifest(&manifest_path)?;
            validate_manifest(&manifest)?;
            let plugin_id = manifest.id.clone();
            let destination = self.root.join(&plugin_id);
            let record = SourceRecord {
                source: source.clone(),
                installed_at: Utc::now().to_rfc3339(),
                commit: commit.clone(),
            };
            fs::write(
                staging.join(SOURCE_RECORD),
                serde_json::to_vec_pretty(&record).map_err(|error| {
                    format!("Could not serialize plugin source record: {error}")
                })?,
            )
            .map_err(|error| format!("Could not save plugin source record: {error}"))?;

            let backup = self
                .root
                .join(format!(".backup-{}-{}", plugin_id, unique_suffix()));
            let had_existing = destination.exists();
            if had_existing {
                fs::rename(&destination, &backup).map_err(|error| {
                    format!("Could not prepare the existing plugin for update: {error}")
                })?;
            }
            if let Err(error) = fs::rename(&staging, &destination) {
                if had_existing {
                    let _ = fs::rename(&backup, &destination);
                }
                return Err(format!("Could not activate the installed plugin: {error}"));
            }
            if had_existing {
                let _ = fs::remove_dir_all(&backup);
            }
            self.read_plugin_info(&destination)
        })();

        if installation.is_err() && staging.exists() {
            let _ = fs::remove_dir_all(&staging);
        }
        installation
    }

    pub fn run_command(
        &self,
        plugin_id: &str,
        command_id: &str,
        input: Option<Value>,
    ) -> Result<PluginCommandResult, String> {
        if !is_valid_identifier(plugin_id) || !is_valid_identifier(command_id) {
            return Err(
                "Plugin and command IDs must contain only letters, digits, '.', '_' or '-'."
                    .to_owned(),
            );
        }
        let plugin_dir = self.root.join(plugin_id);
        if !plugin_dir.is_dir() {
            return Err(format!("Plugin '{plugin_id}' is not installed."));
        }
        let manifest_path = find_manifest(&plugin_dir)
            .ok_or_else(|| format!("Plugin '{plugin_id}' has no manifest."))?;
        let manifest = read_manifest(&manifest_path)?;
        validate_manifest(&manifest)?;
        if manifest.id != plugin_id {
            return Err(format!("Plugin manifest ID does not match '{plugin_id}'."));
        }
        let command = declared_commands(&manifest)
            .iter()
            .find(|command| command.id == command_id)
            .ok_or_else(|| {
                format!("Plugin '{plugin_id}' does not expose command '{command_id}'.")
            })?;
        let package_root = manifest_path
            .parent()
            .ok_or_else(|| format!("Plugin '{plugin_id}' has an invalid manifest path."))?;
        let selected_backend = manifest.backend.as_ref().and_then(select_backend_binary);
        let binary_decl = command
            .binary
            .as_deref()
            .or_else(|| {
                manifest
                    .backend
                    .as_ref()
                    .and_then(|backend| backend.binary.as_deref())
            })
            .or_else(|| selected_backend.map(|binary| binary.path.as_str()))
            .ok_or_else(|| format!("Plugin command '{command_id}' has no backend binary."))?;
        let binary = resolve_plugin_path(package_root, binary_decl)?;
        if !binary.is_file() {
            return Err(format!(
                "Plugin binary does not exist: {}",
                binary.display()
            ));
        }

        let input_value = input.unwrap_or(Value::Null);
        let input_text = serde_json::to_string(&input_value)
            .map_err(|error| format!("Could not serialize plugin input: {error}"))?;
        let is_jsonl_rpc = manifest
            .backend
            .as_ref()
            .and_then(|backend| backend.protocol.as_deref())
            == Some("jsonl-rpc-v1")
            && command.binary.is_none();
        let stdin_text = if is_jsonl_rpc {
            format!(
                "{}\n",
                serde_json::to_string(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": next_rpc_id(),
                    "method": command_id,
                    "params": input_value,
                }))
                .map_err(|error| format!("Could not serialize JSON-RPC input: {error}"))?
            )
        } else {
            input_text.clone()
        };
        let mut args = selected_backend
            .map(|binary| binary.args.clone())
            .unwrap_or_default();
        args.extend(
            command
                .args
                .iter()
                .map(|argument| argument.replace("{{input}}", &input_text))
                .collect::<Vec<_>>(),
        );
        let mut child = Command::new(&binary)
            .args(args)
            .current_dir(package_root)
            .env("IHUB_PLUGIN_ID", plugin_id)
            .env("IHUB_COMMAND_ID", command_id)
            .env("IHUB_PLUGIN_INPUT", &input_text)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("Could not launch plugin command: {error}"))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(stdin_text.as_bytes())
                .map_err(|error| format!("Could not send input to plugin command: {error}"))?;
        }
        let output = child
            .wait_with_output()
            .map_err(|error| format!("Could not wait for plugin command: {error}"))?;
        let stdout = truncate_output(readable_output(&output.stdout));
        let stderr = truncate_output(readable_output(&output.stderr));
        let parsed_stdout = serde_json::from_str::<Value>(&stdout).ok();
        let parsed_output = if is_jsonl_rpc {
            parsed_stdout
                .as_ref()
                .and_then(|value| value.get("result"))
                .cloned()
                .or(parsed_stdout)
        } else {
            parsed_stdout
        };

        Ok(PluginCommandResult {
            plugin_id: plugin_id.to_owned(),
            command_id: command_id.to_owned(),
            success: output.status.success(),
            exit_code: output.status.code(),
            stdout,
            stderr,
            output: parsed_output,
        })
    }

    /// Resolves an installed plugin's frontend entry to a canonical file path.
    ///
    /// The package manifest can live one directory below a cloned repository,
    /// but both it and its frontend must resolve inside the installed plugin
    /// directory. Canonicalizing before returning prevents `..` components and
    /// symlinks from exposing an arbitrary local file through the asset protocol.
    pub fn frontend_path(&self, plugin_id: &str) -> Result<PathBuf, String> {
        if !is_valid_identifier(plugin_id) {
            return Err("Plugin ID must contain only letters, digits, '.', '_' or '-'.".to_owned());
        }

        self.ensure_root()?;
        let plugins_root = self.root.as_ref().canonicalize().map_err(|error| {
            format!(
                "Could not resolve the plugin directory {}: {error}",
                self.root.display()
            )
        })?;
        let plugin_root = plugins_root
            .join(plugin_id)
            .canonicalize()
            .map_err(|error| {
                format!("Plugin '{plugin_id}' is not installed or cannot be resolved: {error}")
            })?;
        if !plugin_root.is_dir() {
            return Err(format!("Plugin '{plugin_id}' is not installed."));
        }
        ensure_path_within(&plugin_root, &plugins_root, "Plugin root")?;

        let manifest_path = find_manifest(&plugin_root)
            .ok_or_else(|| format!("Plugin '{plugin_id}' has no manifest."))?
            .canonicalize()
            .map_err(|error| format!("Could not resolve plugin manifest: {error}"))?;
        ensure_path_within(&manifest_path, &plugin_root, "Plugin manifest")?;

        let manifest = read_manifest(&manifest_path)?;
        validate_manifest(&manifest)?;
        if manifest.id != plugin_id {
            return Err(format!("Plugin manifest ID does not match '{plugin_id}'."));
        }
        let frontend_entry = manifest_frontend_entry(&manifest)
            .ok_or_else(|| format!("Plugin '{plugin_id}' does not declare entry.frontend."))?;
        let package_root = manifest_path
            .parent()
            .ok_or_else(|| format!("Plugin '{plugin_id}' has an invalid manifest path."))?;
        ensure_path_within(package_root, &plugin_root, "Plugin package")?;

        let frontend_path = package_root
            .join(&frontend_entry)
            .canonicalize()
            .map_err(|error| {
                format!("Could not resolve plugin frontend '{frontend_entry}': {error}")
            })?;
        ensure_path_within(&frontend_path, &plugin_root, "Plugin frontend")?;
        if !frontend_path.is_file() {
            return Err(format!(
                "Plugin frontend is not a file: {}",
                frontend_path.display()
            ));
        }
        Ok(frontend_path)
    }

    /// Returns the manifest permission required for a sensitive frontend host
    /// method. Commands, search, settings, lifecycle, and logging stay
    /// permission-free; native plugin binaries are intentionally not covered
    /// by this bridge-level gate.
    pub fn required_permission_for_host_method(method: &str) -> Option<&'static str> {
        match method {
            "clipboard.readText" | "clipboard.read" => Some("clipboard.read"),
            "clipboard.writeText" | "clipboard.write" => Some("clipboard.write"),
            "shell.openPath" | "shell.open" => Some("shell.openPath"),
            "shell.openExternal" => Some("shell.openExternal"),
            "notifications.show" => Some("notifications"),
            "process.spawn" => Some("process.spawn"),
            _ => None,
        }
    }

    /// Looks up a concrete frontend host method against the installed plugin's
    /// manifest. IDs are constrained before joining paths, and both the
    /// plugin directory and manifest are canonicalized beneath the plugin root
    /// so a local symlink cannot make the host read an arbitrary manifest.
    pub fn allows_host_method(&self, plugin_id: &str, method: &str) -> Result<bool, String> {
        if !is_valid_identifier(plugin_id) {
            return Err("Plugin ID must contain only letters, digits, '.', '_' or '-'.".to_owned());
        }

        self.ensure_root()?;
        let plugins_root = self.root.as_ref().canonicalize().map_err(|error| {
            format!(
                "Could not resolve the plugin directory {}: {error}",
                self.root.display()
            )
        })?;
        let plugin_root = plugins_root
            .join(plugin_id)
            .canonicalize()
            .map_err(|error| {
                format!("Plugin '{plugin_id}' is not installed or cannot be resolved: {error}")
            })?;
        if !plugin_root.is_dir() {
            return Err(format!("Plugin '{plugin_id}' is not installed."));
        }
        ensure_path_within(&plugin_root, &plugins_root, "Plugin root")?;

        let manifest_path = find_manifest(&plugin_root)
            .ok_or_else(|| format!("Plugin '{plugin_id}' has no manifest."))?
            .canonicalize()
            .map_err(|error| format!("Could not resolve plugin manifest: {error}"))?;
        ensure_path_within(&manifest_path, &plugin_root, "Plugin manifest")?;
        let manifest = read_manifest(&manifest_path)?;
        validate_manifest(&manifest)?;
        if manifest.id != plugin_id {
            return Err(format!("Plugin manifest ID does not match '{plugin_id}'."));
        }

        Ok(match method {
            "clipboard.readText" | "clipboard.read" => manifest
                .permissions
                .clipboard
                .as_ref()
                .is_some_and(|clipboard| clipboard.read),
            "clipboard.writeText" | "clipboard.write" => manifest
                .permissions
                .clipboard
                .as_ref()
                .is_some_and(|clipboard| clipboard.write),
            "shell.openPath" | "shell.open" => manifest
                .permissions
                .shell
                .as_ref()
                .is_some_and(|shell| shell.open_path),
            "shell.openExternal" => manifest
                .permissions
                .shell
                .as_ref()
                .is_some_and(|shell| shell.open_external),
            "notifications.show" => manifest.permissions.notifications,
            "process.spawn" => manifest
                .permissions
                .process
                .as_ref()
                .is_some_and(|process| process.spawn),
            _ => true,
        })
    }

    fn ensure_root(&self) -> Result<(), String> {
        fs::create_dir_all(self.root.as_ref())
            .map_err(|error| format!("Could not create the plugin directory: {error}"))
    }

    fn read_plugin_info(&self, directory: &Path) -> Result<PluginInfo, String> {
        let manifest_path = find_manifest(directory)
            .ok_or_else(|| format!("{} has no plugin manifest", directory.display()))?;
        let manifest = read_manifest(&manifest_path)?;
        validate_manifest(&manifest)?;
        let source = read_source_record(directory).ok();
        let commands = declared_commands(&manifest)
            .iter()
            .map(|command| PluginCommandInfo {
                id: command.id.clone(),
                name: command_display_name(command),
                description: command
                    .description
                    .clone()
                    .or_else(|| command.subtitle.clone()),
            })
            .collect::<Vec<_>>();
        let has_native_worker = manifest
            .backend
            .as_ref()
            .is_some_and(|backend| backend.binary.is_some() || !backend.binaries.is_empty())
            || declared_commands(&manifest)
                .iter()
                .any(|command| command.binary.is_some());
        let frontend_entry = manifest_frontend_entry(&manifest);
        Ok(PluginInfo {
            id: manifest.id,
            name: manifest.name,
            version: manifest.version,
            description: manifest.description,
            source: source.as_ref().map(|record| record.source.clone()),
            commit: source.as_ref().and_then(|record| record.commit.clone()),
            installed_at: source.map(|record| record.installed_at),
            frontend_entry,
            enabled: true,
            has_native_worker,
            command_count: commands.len(),
            commands,
        })
    }
}

fn default_version() -> String {
    "0.0.0".to_owned()
}

fn default_plugin_root() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        return env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join("AppData/Local"))
            })
            .unwrap_or_else(env::temp_dir)
            .join("iHub/plugins");
    }
    #[cfg(target_os = "macos")]
    {
        return env::var_os("HOME")
            .map(|home| PathBuf::from(home).join("Library/Application Support/iHub/plugins"))
            .unwrap_or_else(env::temp_dir);
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
            .unwrap_or_else(env::temp_dir)
            .join("ihub/plugins")
    }
}

fn normalize_git_source(source: &str) -> Result<String, String> {
    let source = source.trim();
    if source.is_empty() || source.chars().any(char::is_whitespace) || source.starts_with('-') {
        return Err("Enter a GitHub repository (owner/repo or a git URL).".to_owned());
    }
    let github_shorthand = source.strip_prefix("github:").unwrap_or(source);
    if is_github_shorthand(github_shorthand) {
        return Ok(format!("https://github.com/{github_shorthand}.git"));
    }
    let supported_protocol = source.starts_with("https://")
        || source.starts_with("http://")
        || source.starts_with("ssh://")
        || source.starts_with("git@");
    if !supported_protocol {
        return Err(
            "Only remote Git URLs are accepted; local filesystem paths are not plugin sources."
                .to_owned(),
        );
    }
    Ok(source.to_owned())
}

fn git_revision(directory: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["-C"])
        .arg(directory)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| readable_output(&output.stdout))
        .filter(|revision| !revision.is_empty())
}

fn is_github_shorthand(source: &str) -> bool {
    let mut segments = source.split('/');
    let Some(owner) = segments.next() else {
        return false;
    };
    let Some(repository) = segments.next() else {
        return false;
    };
    segments.next().is_none() && is_valid_identifier(owner) && is_valid_identifier(repository)
}

fn find_manifest(root: &Path) -> Option<PathBuf> {
    for name in MANIFEST_NAMES {
        let candidate = root.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // A repository may keep a package under one top-level folder. Do not walk
    // arbitrarily deep: it would make an embedded dependency look installable.
    let entries = fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let directory = entry.path();
        if !directory.is_dir() || is_internal_dir(&directory) {
            continue;
        }
        for name in MANIFEST_NAMES {
            let candidate = directory.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn read_manifest(path: &Path) -> Result<PluginManifest, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("Could not read plugin manifest {}: {error}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|error| format!("Invalid plugin manifest {}: {error}", path.display()))
}

fn read_source_record(directory: &Path) -> Result<SourceRecord, String> {
    let text = fs::read_to_string(directory.join(SOURCE_RECORD))
        .map_err(|error| format!("Could not read plugin source record: {error}"))?;
    serde_json::from_str(&text).map_err(|error| format!("Invalid plugin source record: {error}"))
}

fn validate_manifest(manifest: &PluginManifest) -> Result<(), String> {
    if !is_valid_identifier(&manifest.id) {
        return Err(
            "Plugin manifest ID must contain only letters, digits, '.', '_' or '-'.".to_owned(),
        );
    }
    if manifest.name.trim().is_empty() {
        return Err("Plugin manifest name cannot be empty.".to_owned());
    }
    for command in declared_commands(manifest) {
        if !is_valid_identifier(&command.id) {
            return Err(format!("Plugin command ID '{}' is invalid.", command.id));
        }
        if let Some(binary) = &command.binary {
            validate_relative_path(binary)?;
        }
    }
    if let Some(backend) = &manifest.backend {
        if let Some(binary) = &backend.binary {
            validate_relative_path(binary)?;
        }
        if !backend.binaries.is_empty() && backend.protocol.as_deref() != Some("jsonl-rpc-v1") {
            return Err("Plugin backend.binaries requires protocol 'jsonl-rpc-v1'.".to_owned());
        }
        for binary in &backend.binaries {
            validate_relative_path(&binary.path)?;
            if !is_supported_target(&binary.target) {
                return Err(format!(
                    "Plugin backend target '{}' is not supported.",
                    binary.target
                ));
            }
        }
    }
    if let Some(entry) = &manifest.entry {
        validate_relative_path(&entry.frontend)?;
    }
    if let Some(frontend) = &manifest.frontend {
        validate_relative_path(&frontend_entry(frontend))?;
    }
    Ok(())
}

fn is_valid_identifier(value: &str) -> bool {
    let length = value.len();
    (2..=96).contains(&length)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn validate_relative_path(value: &str) -> Result<(), String> {
    let path = Path::new(value);
    if value.trim().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(
            "Plugin package paths must be relative paths inside the plugin directory.".to_owned(),
        );
    }
    Ok(())
}

fn resolve_plugin_path(plugin_dir: &Path, value: &str) -> Result<PathBuf, String> {
    validate_relative_path(value)?;
    let resolved = plugin_dir.join(value);
    if !resolved.starts_with(plugin_dir) {
        return Err("Plugin binary path escapes the plugin directory.".to_owned());
    }
    Ok(resolved)
}

fn ensure_path_within(path: &Path, root: &Path, label: &str) -> Result<(), String> {
    if path.starts_with(root) {
        Ok(())
    } else {
        Err(format!("{label} escapes the installed plugin directory."))
    }
}

fn frontend_entry(frontend: &FrontendDeclaration) -> String {
    match frontend {
        FrontendDeclaration::Entry(entry) => entry.clone(),
        FrontendDeclaration::Detailed { entry } => entry.clone(),
    }
}

fn manifest_frontend_entry(manifest: &PluginManifest) -> Option<String> {
    manifest
        .entry
        .as_ref()
        .map(|entry| entry.frontend.clone())
        .or_else(|| manifest.frontend.as_ref().map(frontend_entry))
}

fn declared_commands(manifest: &PluginManifest) -> &[PluginCommandDeclaration] {
    manifest
        .contributes
        .as_ref()
        .filter(|contributes| !contributes.commands.is_empty())
        .map(|contributes| contributes.commands.as_slice())
        .unwrap_or(manifest.commands.as_slice())
}

fn command_display_name(command: &PluginCommandDeclaration) -> String {
    command
        .title
        .as_deref()
        .or(command.name.as_deref())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(&command.id)
        .to_owned()
}

fn select_backend_binary(backend: &BackendDeclaration) -> Option<&PluginBinaryDeclaration> {
    backend
        .binaries
        .iter()
        .find(|binary| binary.target == current_platform_target())
}

fn current_platform_target() -> &'static str {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "windows-x86_64"
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        "windows-aarch64"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "darwin-x86_64"
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "darwin-aarch64"
    }
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64")
    )))]
    {
        "unsupported"
    }
}

fn is_supported_target(target: &str) -> bool {
    matches!(
        target,
        "windows-x86_64" | "windows-aarch64" | "darwin-x86_64" | "darwin-aarch64"
    )
}

fn is_internal_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with('.'))
}

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

fn next_rpc_id() -> String {
    format!("rpc-{}", unique_suffix())
}

fn readable_output(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_owned()
}

fn truncate_output(mut text: String) -> String {
    if text.len() <= MAX_CAPTURED_OUTPUT_BYTES {
        return text;
    }
    let mut boundary = MAX_CAPTURED_OUTPUT_BYTES;
    while !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    text.truncate(boundary);
    text.push_str("\n[iHub truncated plugin output]");
    text
}
