//! Cross-platform child-process construction for work that belongs to iHub.
//!
//! The desktop binary uses the Windows GUI subsystem in release builds. A
//! console-subsystem child started from that process would otherwise allocate
//! a visible console window. Every production child—including plugin workers,
//! Git, and the platform opener—must therefore flow through this module.

use std::{ffi::OsStr, process::Command};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// `CREATE_NO_WINDOW` from WinBase.h.
///
/// Keep the value local instead of broadening the Windows crate feature set
/// solely for one stable process-creation flag.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Builds a child process that cannot allocate a Windows console window.
///
/// On macOS and Linux this is intentionally a plain `Command`: iHub never
/// delegates background work through a terminal-host application.
pub(crate) fn background_command<S: AsRef<OsStr>>(program: S) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    fn source_files(root: &Path) -> Vec<PathBuf> {
        let mut pending = vec![root.to_path_buf()];
        let mut files = Vec::new();
        while let Some(directory) = pending.pop() {
            for entry in
                fs::read_dir(&directory).expect("maintained source directory should be readable")
            {
                let entry = entry.expect("maintained source entry should be readable");
                let path = entry.path();
                if entry
                    .file_type()
                    .expect("maintained source type should be readable")
                    .is_dir()
                {
                    pending.push(path);
                } else {
                    files.push(path);
                }
            }
        }
        files
    }

    #[test]
    fn every_rust_child_process_uses_the_background_constructor() {
        let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut violations = Vec::new();
        for path in source_files(&source_root) {
            if path.extension().and_then(|extension| extension.to_str()) != Some("rs")
                || path.file_name().and_then(|name| name.to_str()) == Some("background_process.rs")
            {
                continue;
            }
            let source = fs::read_to_string(&path).expect("Rust source should be UTF-8");
            if source.contains("Command::new(") || source.contains("std::process::Command::new(") {
                violations.push(
                    path.file_name()
                        .expect("source file should have a name")
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
        assert!(
            violations.is_empty(),
            "child processes bypass background_command in: {}",
            violations.join(", ")
        );
    }

    #[test]
    fn tauri_shell_process_api_is_not_linked_into_the_host() {
        let cargo_manifest =
            fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
                .expect("Cargo.toml should be readable");
        assert!(
            !cargo_manifest
                .to_ascii_lowercase()
                .contains("tauri-plugin-shell"),
            "the unrestricted Tauri shell process API must remain absent"
        );
    }

    #[test]
    fn maintained_background_sources_never_launch_a_terminal_host() {
        let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let repository_root = crate_root
            .parent()
            .expect("src-tauri should have a repository parent");
        let source_roots = [crate_root.join("src"), repository_root.join("scripts")];
        let forbidden = [
            "open -a terminal",
            "open --application terminal",
            "tell application \"terminal",
            "tell application 'terminal",
            "application(\"terminal\")",
            "application('terminal')",
            "terminal.app",
        ];
        let mut violations = Vec::new();

        for source_root in source_roots {
            for path in source_files(&source_root) {
                let extension = path.extension().and_then(|extension| extension.to_str());
                if !matches!(extension, Some("rs" | "ps1" | "sh" | "js" | "mjs" | "cjs"))
                    || path.file_name().and_then(|name| name.to_str())
                        == Some("background_process.rs")
                {
                    continue;
                }
                let source = fs::read_to_string(&path)
                    .expect("maintained process-launching source should be UTF-8")
                    .to_ascii_lowercase();
                if forbidden.iter().any(|pattern| source.contains(pattern)) {
                    violations.push(path.display().to_string());
                }
            }
        }

        assert!(
            violations.is_empty(),
            "background sources explicitly launch a terminal host: {}",
            violations.join(", ")
        );
    }
}
