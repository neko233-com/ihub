//! Race-resistant opening of an already-authorized local filesystem path.
//!
//! Authorization still belongs to the caller. This module closes the gap
//! between that authorization and handing the path to the operating system:
//! path components are inspected without following reparse points, native
//! guards are retained wherever Windows ACLs allow them, and the strict final
//! target guard stays alive until the synchronous launcher call returns.
//!
//! Windows profile ACLs sometimes allow traversal while denying any handle to
//! the profile directory itself. Only such `ERROR_ACCESS_DENIED` intermediate
//! components use a bounded metadata-before/after fallback; the final target
//! never does. This closes the host's validation-to-Shell handoff race, but it
//! cannot promise that an arbitrary third-party application will not reopen a
//! pathname later, after `ShellExecuteExW` returns and these guards are dropped.

use std::{
    fs,
    path::{Component, Path, PathBuf},
};

const MAX_GUARDED_COMPONENTS: usize = 256;

/// The filesystem object type proved by a prepared local open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalOpenKind {
    File,
    Folder,
}

impl LocalOpenKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Folder => "folder",
        }
    }
}

/// Stable object identity captured from the final open handle.
///
/// Windows prefers `FileIdInfo`'s 128-bit identifier. Filesystems that do not
/// implement `FileIdInfo` fall back to the legacy 64-bit file index; that
/// fallback is useful for replacement detection on common filesystems but is
/// not claimed to be a ReFS-strength globally unique identifier. Unix uses the
/// device and inode number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalPathIdentity {
    storage_id: u64,
    file_id: [u8; 16],
    scheme: LocalPathIdentityScheme,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalPathIdentityScheme {
    WindowsFileId128,
    WindowsLegacyFileIndex,
    UnixInode,
    PortableFallback,
}

impl LocalPathIdentity {
    pub fn storage_id(self) -> u64 {
        self.storage_id
    }

    pub fn file_id(self) -> [u8; 16] {
        self.file_id
    }

    pub fn scheme(self) -> LocalPathIdentityScheme {
        self.scheme
    }
}

/// A normalized local path whose object and ancestor bindings are guarded.
///
/// Keep this value alive through any API that consumes `path()`. Dropping it
/// releases the component handles and therefore the Windows rename/delete
/// guards.
#[derive(Debug)]
pub struct PreparedLocalOpen {
    normalized_path: PathBuf,
    kind: LocalOpenKind,
    identity: LocalPathIdentity,
    #[cfg(windows)]
    _guards: Vec<std::os::windows::io::OwnedHandle>,
    #[cfg(windows)]
    traversal_only_components: Vec<WindowsTraversalOnlyComponent>,
    #[cfg(not(windows))]
    _guards: Vec<fs::File>,
}

impl PreparedLocalOpen {
    pub fn path(&self) -> &Path {
        &self.normalized_path
    }

    pub fn kind(&self) -> LocalOpenKind {
        self.kind
    }

    pub fn identity(&self) -> LocalPathIdentity {
        self.identity
    }

    /// Launches this already-prepared target without releasing its guards.
    pub fn launch(&self) -> Result<(), String> {
        self.launch_with(launch_with_platform_shell)
    }

    /// Runs an injectable synchronous launcher while all component guards live.
    pub fn launch_with<F>(&self, launcher: F) -> Result<(), String>
    where
        F: FnOnce(&PreparedLocalOpen) -> Result<(), String>,
    {
        #[cfg(windows)]
        self.revalidate_traversal_only_components()?;
        launcher(self)
    }
}

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowsTraversalOnlyComponent {
    path: PathBuf,
    file_attributes: u32,
    creation_time: u64,
    last_write_time: u64,
}

#[cfg(windows)]
impl PreparedLocalOpen {
    fn revalidate_traversal_only_components(&self) -> Result<(), String> {
        for expected in &self.traversal_only_components {
            let current = windows_traversal_only_component(&expected.path)?;
            if &current != expected {
                return Err(format!(
                    "A traversal-only local path component changed before launch: {}",
                    expected.path.display()
                ));
            }
        }
        Ok(())
    }
}

/// Validates and prepares a local filesystem target without launching it.
pub fn prepare_local_open(
    requested_path: &Path,
    expected_kind: Option<LocalOpenKind>,
) -> Result<PreparedLocalOpen, String> {
    #[cfg(windows)]
    {
        prepare_windows_local_open(requested_path, expected_kind)
    }
    #[cfg(not(windows))]
    {
        prepare_non_windows_local_open(requested_path, expected_kind)
    }
}

/// Prepares a local target and launches it with the platform shell.
///
/// On Windows `ShellExecuteExW` is called with `SEE_MASK_NOASYNC`, so the
/// prepared handles remain alive through Shell execution rather than merely
/// through creation of an asynchronous helper process.
pub fn open_local_path(
    requested_path: &Path,
    expected_kind: Option<LocalOpenKind>,
) -> Result<(), String> {
    open_local_path_with(requested_path, expected_kind, launch_with_platform_shell)
}

/// Variant used by callers and tests that need an explicit synchronous
/// launcher while retaining the same prepared-path lifetime contract.
pub fn open_local_path_with<F>(
    requested_path: &Path,
    expected_kind: Option<LocalOpenKind>,
    launcher: F,
) -> Result<(), String>
where
    F: FnOnce(&PreparedLocalOpen) -> Result<(), String>,
{
    let prepared = prepare_local_open(requested_path, expected_kind)?;
    prepared.launch_with(launcher)
}

/// Reveals one already-validated local object in the platform file manager
/// while retaining the same path-identity guards used for ordinary opens.
pub fn show_local_item_in_folder(requested_path: &Path) -> Result<(), String> {
    let prepared = prepare_local_open(requested_path, None)?;
    prepared.launch_with(show_prepared_item_in_folder)
}

/// Moves one validated local object to the operating-system recycle bin. This
/// never falls back to permanent deletion.
///
/// Windows cannot recycle an object while our strict final guard deliberately
/// denies delete sharing. Keep every guard through the last revalidation,
/// capture the Shell-compatible path, then release them immediately before
/// the single recycle call. Unlike ordinary open/reveal, the legacy Shell
/// recycle API is necessarily path-based across that final handoff.
pub fn trash_local_item(requested_path: &Path) -> Result<(), String> {
    let prepared = prepare_local_open(requested_path, None)?;
    #[cfg(windows)]
    prepared.revalidate_traversal_only_components()?;
    let path = {
        #[cfg(windows)]
        {
            shell_compatible_windows_path(prepared.path())
        }
        #[cfg(not(windows))]
        {
            prepared.path().to_path_buf()
        }
    };
    drop(prepared);
    trash_validated_item(&path)
}

#[cfg(windows)]
fn show_prepared_item_in_folder(prepared: &PreparedLocalOpen) -> Result<(), String> {
    let path = shell_compatible_windows_path(prepared.path());
    let mut command = crate::background_process::background_command("explorer.exe");
    command.arg("/select,").arg(path);
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Windows could not reveal the local item: {error}"))
}

#[cfg(target_os = "macos")]
fn show_prepared_item_in_folder(prepared: &PreparedLocalOpen) -> Result<(), String> {
    crate::background_process::background_command("open")
        .arg("-R")
        .arg(prepared.path())
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("macOS could not reveal the local item: {error}"))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn show_prepared_item_in_folder(prepared: &PreparedLocalOpen) -> Result<(), String> {
    let parent = if prepared.kind() == LocalOpenKind::Folder {
        prepared.path()
    } else {
        prepared
            .path()
            .parent()
            .ok_or_else(|| "The local item has no parent folder.".to_owned())?
    };
    crate::background_process::background_command("xdg-open")
        .arg(parent)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("The file manager could not reveal the local item: {error}"))
}

#[cfg(windows)]
fn trash_validated_item(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::UI::Shell::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE,
        SHFILEOPSTRUCTW,
    };

    let mut from = path.as_os_str().encode_wide().collect::<Vec<_>>();
    // SHFileOperationW consumes a double-NUL-terminated path list.
    from.extend_from_slice(&[0, 0]);
    let mut operation = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: from.as_ptr(),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOF_SILENT) as u16,
        ..Default::default()
    };
    let result = unsafe { SHFileOperationW(&mut operation) };
    if operation.fAnyOperationsAborted != 0 {
        return Err("Windows cancelled the recycle-bin operation.".to_owned());
    }
    if result != 0 {
        return Err(format!(
            "Windows could not move the local item to the recycle bin (Shell code {result})."
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn trash_validated_item(_path: &Path) -> Result<(), String> {
    Err("Recycle-bin compatibility has been runtime-verified on Windows only.".to_owned())
}

#[cfg(windows)]
fn shell_compatible_windows_path(path: &Path) -> PathBuf {
    use std::{
        ffi::OsString,
        os::windows::ffi::{OsStrExt, OsStringExt},
    };

    const VERBATIM_PREFIX: [u16; 4] = [b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
    let wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if wide.starts_with(&VERBATIM_PREFIX) {
        return PathBuf::from(OsString::from_wide(&wide[VERBATIM_PREFIX.len()..]));
    }
    path.to_path_buf()
}

fn enforce_expected_kind(
    actual: LocalOpenKind,
    expected: Option<LocalOpenKind>,
) -> Result<(), String> {
    if let Some(expected) = expected {
        if expected != actual {
            return Err(format!(
                "The selected target changed type; expected a {}.",
                expected.as_str()
            ));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn prepare_windows_local_open(
    requested_path: &Path,
    expected_kind: Option<LocalOpenKind>,
) -> Result<PreparedLocalOpen, String> {
    use std::{
        ffi::OsString,
        os::windows::{
            ffi::OsStringExt,
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
        },
        ptr,
    };

    use windows_sys::Win32::{
        Foundation::{ERROR_ACCESS_DENIED, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{
            CreateFileW, FileIdInfo, GetDriveTypeW, GetFileInformationByHandle,
            GetFileInformationByHandleEx, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY,
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_GENERIC_READ, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        },
        System::WindowsProgramming::{DRIVE_CDROM, DRIVE_FIXED, DRIVE_RAMDISK, DRIVE_REMOVABLE},
    };

    let (drive_letter, component_paths) = guarded_windows_component_paths(requested_path)?;
    if component_paths.len() > MAX_GUARDED_COMPONENTS {
        return Err(format!(
            "The selected path has too many components (maximum {MAX_GUARDED_COMPONENTS})."
        ));
    }

    let volume_root = [
        u16::from(drive_letter.to_ascii_uppercase()),
        u16::from(b':'),
        u16::from(b'\\'),
        0,
    ];
    let drive_type = unsafe { GetDriveTypeW(volume_root.as_ptr()) };
    if !matches!(
        drive_type,
        DRIVE_FIXED | DRIVE_REMOVABLE | DRIVE_CDROM | DRIVE_RAMDISK
    ) {
        return Err("Only an absolute path on a local disk can be opened.".to_owned());
    }

    let mut guards = Vec::with_capacity(component_paths.len());
    let mut traversal_only_components = Vec::new();
    let mut final_information = None;
    for (index, component_path) in component_paths.iter().enumerate() {
        let is_final = index + 1 == component_paths.len();
        let wide = windows_path_wide(component_path)?;
        let raw_handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                if is_final {
                    // A metadata-only handle does not participate in Windows'
                    // read/write share checks. Request real read access on the
                    // final target so omitting SHARE_WRITE below actually
                    // prevents a concurrent writer until launch returns.
                    FILE_GENERIC_READ
                } else {
                    // Zero desired access avoids ACL-dependent read failures
                    // on profile ancestors while still giving us metadata.
                    0
                },
                if is_final {
                    // The final target deliberately shares neither WRITE nor
                    // DELETE, keeping its binding and contents stable.
                    FILE_SHARE_READ
                } else {
                    // Windows keeps DELETE-access handles on common profile
                    // ancestors. Refusing DELETE sharing here would make every
                    // target below such a profile impossible to prepare. The
                    // final target's stricter handle below prevents its parent
                    // subtree from being renamed during launch; every ancestor
                    // is still held and inspected no-follow to reject reparse
                    // traversal.
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
                },
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                ptr::null_mut(),
            )
        };
        if raw_handle == INVALID_HANDLE_VALUE {
            let error = std::io::Error::last_os_error();
            // A normal Windows profile can grant traverse rights without
            // allowing its directory object to be opened at all. Keep this
            // fallback limited to ACCESS_DENIED ancestors, prove their native
            // metadata twice, and still require a strict final handle. Other
            // failures and every final-target failure remain fail closed.
            if !is_final && error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32) {
                traversal_only_components.push(windows_traversal_only_component(component_path)?);
                continue;
            }
            return Err(format!(
                "Could not prepare local path component {}: {}",
                component_path.display(),
                error
            ));
        }

        let handle = unsafe { OwnedHandle::from_raw_handle(raw_handle as _) };
        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        if unsafe { GetFileInformationByHandle(handle.as_raw_handle() as _, &mut information) } == 0
        {
            return Err(format!(
                "Could not inspect local path component {}: {}",
                component_path.display(),
                std::io::Error::last_os_error()
            ));
        }
        if information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(format!(
                "Filesystem reparse points cannot be opened: {}",
                component_path.display()
            ));
        }

        if !is_final && information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Err(format!(
                "A local path parent is not a directory: {}",
                component_path.display()
            ));
        }
        if is_final {
            final_information = Some(information);
        }
        guards.push(handle);
    }

    let final_information =
        final_information.ok_or_else(|| "The selected local path is empty.".to_owned())?;
    let final_handle = guards
        .last()
        .ok_or_else(|| "The selected local path is empty.".to_owned())?;
    for expected in &traversal_only_components {
        let current = windows_traversal_only_component(&expected.path)?;
        if &current != expected {
            return Err(format!(
                "A traversal-only local path component changed during preparation: {}",
                expected.path.display()
            ));
        }
    }
    let normalized_wide = final_path_name_by_handle(final_handle)?;
    // Preserve GetFinalPathNameByHandleW's normalized DOS spelling, including
    // its `\\?\` prefix. This matches Rust's Windows canonical paths and lets
    // authorization roots compare the exact handle-derived path without a
    // second name lookup.
    let normalized_path = PathBuf::from(OsString::from_wide(&normalized_wide));
    let normalized_drive = windows_drive_letter(&normalized_path)
        .ok_or_else(|| "The prepared target did not resolve to a local DOS path.".to_owned())?;
    if !normalized_drive.eq_ignore_ascii_case(&drive_letter) {
        return Err("The prepared target resolved onto a different local volume.".to_owned());
    }

    let kind = if final_information.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        LocalOpenKind::Folder
    } else {
        LocalOpenKind::File
    };
    enforce_expected_kind(kind, expected_kind)?;
    let (storage_id, file_id, scheme) = windows_file_identity(
        final_handle,
        &final_information,
        GetFileInformationByHandleEx,
        FileIdInfo,
    );

    Ok(PreparedLocalOpen {
        normalized_path,
        kind,
        identity: LocalPathIdentity {
            storage_id,
            file_id,
            scheme,
        },
        _guards: guards,
        traversal_only_components,
    })
}

#[cfg(windows)]
fn windows_traversal_only_component(path: &Path) -> Result<WindowsTraversalOnlyComponent, String> {
    use std::os::windows::fs::MetadataExt;

    let metadata = fs::symlink_metadata(path).map_err(|error| {
        format!(
            "Could not validate traversal-only local path component {}: {error}",
            path.display()
        )
    })?;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(format!(
            "A traversal-only local path component is not a regular directory: {}",
            path.display()
        ));
    }
    Ok(WindowsTraversalOnlyComponent {
        path: path.to_path_buf(),
        file_attributes: metadata.file_attributes(),
        creation_time: metadata.creation_time(),
        last_write_time: metadata.last_write_time(),
    })
}

#[cfg(windows)]
fn windows_file_identity(
    handle: &std::os::windows::io::OwnedHandle,
    legacy: &windows_sys::Win32::Storage::FileSystem::BY_HANDLE_FILE_INFORMATION,
    get_information: unsafe extern "system" fn(
        windows_sys::Win32::Foundation::HANDLE,
        windows_sys::Win32::Storage::FileSystem::FILE_INFO_BY_HANDLE_CLASS,
        *mut std::ffi::c_void,
        u32,
    ) -> windows_sys::core::BOOL,
    file_id_class: windows_sys::Win32::Storage::FileSystem::FILE_INFO_BY_HANDLE_CLASS,
) -> (u64, [u8; 16], LocalPathIdentityScheme) {
    use std::{mem::size_of, os::windows::io::AsRawHandle, ptr::addr_of_mut};

    use windows_sys::Win32::Storage::FileSystem::FILE_ID_INFO;

    let mut extended = FILE_ID_INFO::default();
    if unsafe {
        get_information(
            handle.as_raw_handle() as _,
            file_id_class,
            addr_of_mut!(extended).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    } != 0
    {
        return (
            extended.VolumeSerialNumber,
            extended.FileId.Identifier,
            LocalPathIdentityScheme::WindowsFileId128,
        );
    }

    let legacy_index = ((legacy.nFileIndexHigh as u64) << 32) | legacy.nFileIndexLow as u64;
    let mut file_id = [0_u8; 16];
    file_id[..8].copy_from_slice(&legacy_index.to_le_bytes());
    (
        u64::from(legacy.dwVolumeSerialNumber),
        file_id,
        LocalPathIdentityScheme::WindowsLegacyFileIndex,
    )
}

#[cfg(windows)]
fn guarded_windows_component_paths(path: &Path) -> Result<(u8, Vec<PathBuf>), String> {
    use std::path::Prefix;

    let mut components = path.components();
    let (drive_letter, mut current) = match components.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(letter) => (letter, PathBuf::from(format!("{}:\\", char::from(letter)))),
            Prefix::VerbatimDisk(letter) => (
                letter,
                PathBuf::from(format!(r"\\?\{}:\", char::from(letter))),
            ),
            _ => {
                return Err(
                    "UNC, device, and non-DOS paths cannot be opened as local targets.".to_owned(),
                )
            }
        },
        _ => return Err("Only an absolute path on a local disk can be opened.".to_owned()),
    };
    if !matches!(components.next(), Some(Component::RootDir)) {
        return Err("Drive-relative paths cannot be opened.".to_owned());
    }

    let mut paths = vec![current.clone()];
    for component in components {
        match component {
            Component::Normal(value) => {
                validate_windows_component(value)?;
                current.push(value);
                paths.push(current.clone());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(
                    "Parent-directory traversal is not accepted for local opens.".to_owned(),
                )
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err("The selected local path has an invalid component.".to_owned())
            }
        }
    }
    Ok((drive_letter, paths))
}

#[cfg(windows)]
fn validate_windows_component(value: &std::ffi::OsStr) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;

    let wide = value.encode_wide().collect::<Vec<_>>();
    if wide.is_empty()
        || wide.contains(&0)
        || wide.contains(&(b':' as u16))
        || matches!(wide.last(), Some(value) if *value == b'.' as u16 || *value == b' ' as u16)
    {
        return Err("The selected local path contains an ambiguous Windows component.".to_owned());
    }
    Ok(())
}

#[cfg(windows)]
fn windows_path_wide(path: &Path) -> Result<Vec<u16>, String> {
    use std::os::windows::ffi::OsStrExt;

    let mut wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if wide.contains(&0) {
        return Err("The selected local path contains an embedded NUL.".to_owned());
    }
    if wide.len() >= 32_767 {
        return Err("The selected local path is too long.".to_owned());
    }
    wide.push(0);
    Ok(wide)
}

#[cfg(windows)]
fn windows_drive_letter(path: &Path) -> Option<u8> {
    use std::path::Prefix;

    match path.components().next()? {
        Component::Prefix(prefix) => match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => Some(letter),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(windows)]
fn final_path_name_by_handle(
    handle: &std::os::windows::io::OwnedHandle,
) -> Result<Vec<u16>, String> {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Storage::FileSystem::{
        GetFinalPathNameByHandleW, FILE_NAME_NORMALIZED, VOLUME_NAME_DOS,
    };

    let mut buffer = vec![0_u16; 512];
    loop {
        let written = unsafe {
            GetFinalPathNameByHandleW(
                handle.as_raw_handle() as _,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        };
        if written == 0 {
            return Err(format!(
                "Could not resolve the prepared local path: {}",
                std::io::Error::last_os_error()
            ));
        }
        let written = written as usize;
        if written < buffer.len() {
            buffer.truncate(written);
            return Ok(buffer);
        }
        if written >= 32_767 {
            return Err("The prepared local path is too long.".to_owned());
        }
        buffer.resize(written + 1, 0);
    }
}

#[cfg(not(windows))]
fn prepare_non_windows_local_open(
    requested_path: &Path,
    expected_kind: Option<LocalOpenKind>,
) -> Result<PreparedLocalOpen, String> {
    if !requested_path.is_absolute() {
        return Err("Only an absolute local path can be opened.".to_owned());
    }
    reject_non_windows_link_components(requested_path)?;
    let normalized_path = requested_path
        .canonicalize()
        .map_err(|error| format!("The selected filesystem target is unavailable: {error}"))?;
    reject_non_windows_link_components(&normalized_path)?;

    let component_paths = non_windows_component_paths(&normalized_path)?;
    if component_paths.len() > MAX_GUARDED_COMPONENTS {
        return Err(format!(
            "The selected path has too many components (maximum {MAX_GUARDED_COMPONENTS})."
        ));
    }

    let mut guards = Vec::with_capacity(component_paths.len());
    for (index, component_path) in component_paths.iter().enumerate() {
        let file = open_non_windows_component(component_path).map_err(|error| {
            format!(
                "Could not prepare local path component {}: {error}",
                component_path.display()
            )
        })?;
        let metadata = file.metadata().map_err(|error| {
            format!(
                "Could not inspect local path component {}: {error}",
                component_path.display()
            )
        })?;
        if index + 1 != component_paths.len() && !metadata.is_dir() {
            return Err(format!(
                "A local path parent is not a directory: {}",
                component_path.display()
            ));
        }
        guards.push(file);
    }

    let final_file = guards
        .last()
        .ok_or_else(|| "The selected local path is empty.".to_owned())?;
    let metadata = final_file
        .metadata()
        .map_err(|error| format!("Could not inspect the selected local path: {error}"))?;
    let kind = if metadata.is_file() {
        LocalOpenKind::File
    } else if metadata.is_dir() {
        LocalOpenKind::Folder
    } else {
        return Err("The selected target is not a regular file or directory.".to_owned());
    };
    enforce_expected_kind(kind, expected_kind)?;

    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        LocalPathIdentity {
            storage_id: metadata.dev(),
            file_id: u128::from(metadata.ino()).to_le_bytes(),
            scheme: LocalPathIdentityScheme::UnixInode,
        }
    };
    #[cfg(not(unix))]
    let identity = LocalPathIdentity {
        storage_id: 0,
        file_id: [0; 16],
        scheme: LocalPathIdentityScheme::PortableFallback,
    };

    Ok(PreparedLocalOpen {
        normalized_path,
        kind,
        identity,
        _guards: guards,
        #[cfg(windows)]
        traversal_only_components: Vec::new(),
    })
}

#[cfg(not(windows))]
fn reject_non_windows_link_components(path: &Path) -> Result<(), String> {
    let component_paths = non_windows_component_paths(path)?;
    for component_path in component_paths {
        let metadata = fs::symlink_metadata(&component_path).map_err(|error| {
            format!(
                "Could not validate local path component {}: {error}",
                component_path.display()
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "Symbolic links cannot be opened: {}",
                component_path.display()
            ));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn non_windows_component_paths(path: &Path) -> Result<Vec<PathBuf>, String> {
    if !path.is_absolute() {
        return Err("Only an absolute local path can be opened.".to_owned());
    }
    let mut current = PathBuf::new();
    let mut paths = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                current.push(component.as_os_str());
                paths.push(current.clone());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(
                    "Parent-directory traversal is not accepted for local opens.".to_owned(),
                )
            }
        }
    }
    Ok(paths)
}

#[cfg(all(not(windows), unix))]
fn open_non_windows_component(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
}

#[cfg(not(any(windows, unix)))]
fn open_non_windows_component(path: &Path) -> std::io::Result<fs::File> {
    fs::File::open(path)
}

#[cfg(windows)]
fn launch_with_platform_shell(prepared: &PreparedLocalOpen) -> Result<(), String> {
    use std::mem::size_of;

    use windows::{
        core::PCWSTR,
        Win32::{
            Foundation::RPC_E_CHANGED_MODE,
            System::Com::{
                CoInitializeEx, CoUninitialize, COINIT, COINIT_APARTMENTTHREADED,
                COINIT_DISABLE_OLE1DDE,
            },
            UI::{
                Shell::{
                    ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW,
                },
                WindowsAndMessaging::SW_SHOWNORMAL,
            },
        },
    };

    enum ComApartment {
        /// This call initialized COM and therefore owns one matching uninit.
        Owned,
        /// The thread already uses another apartment model. ShellExecuteExW
        /// with NOASYNC can safely use that existing initialized apartment.
        ExistingDifferentModel,
    }
    impl Drop for ComApartment {
        fn drop(&mut self) {
            if matches!(self, Self::Owned) {
                unsafe { CoUninitialize() };
            }
        }
    }

    let coinit = COINIT(COINIT_APARTMENTTHREADED.0 | COINIT_DISABLE_OLE1DDE.0);
    let result = unsafe { CoInitializeEx(None, coinit) };
    let _apartment = if result.is_ok() {
        ComApartment::Owned
    } else if result == RPC_E_CHANGED_MODE {
        // Record the non-owned state so Drop cannot unbalance the pre-existing
        // apartment initialization.
        ComApartment::ExistingDifferentModel
    } else {
        return Err(format!(
            "Could not initialize the Windows shell apartment: {}",
            windows::core::Error::from(result)
        ));
    };

    let path = windows_path_wide(prepared.path())?;
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpFile: PCWSTR(path.as_ptr()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }
        .map_err(|error| format!("Could not open {}: {error}", prepared.path().display()))
}

#[cfg(target_os = "macos")]
fn launch_with_platform_shell(prepared: &PreparedLocalOpen) -> Result<(), String> {
    crate::background_process::background_command("open")
        .arg(prepared.path())
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Could not open {}: {error}", prepared.path().display()))
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn launch_with_platform_shell(prepared: &PreparedLocalOpen) -> Result<(), String> {
    crate::background_process::background_command("xdg-open")
        .arg(prepared.path())
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Could not open {}: {error}", prepared.path().display()))
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        fs,
        path::{Path, PathBuf},
    };

    use uuid::Uuid;

    use super::{open_local_path_with, prepare_local_open, LocalOpenKind, PreparedLocalOpen};

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("ihub-system-open-{label}-{}", Uuid::new_v4()));
            fs::create_dir(&root).expect("create system-open fixture");
            Self { root }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn prepares_files_and_folders_with_normalized_identity() {
        let fixture = Fixture::new("identity");
        let folder = fixture.root.join("folder");
        let file = folder.join("sample.txt");
        fs::create_dir(&folder).unwrap();
        fs::write(&file, b"sample").unwrap();

        let prepared_file =
            prepare_local_open(&file, Some(LocalOpenKind::File)).expect("prepare regular file");
        assert_eq!(prepared_file.kind(), LocalOpenKind::File);
        assert_eq!(
            prepared_file.path(),
            normalized_for_comparison(&file).as_path()
        );

        let prepared_folder = prepare_local_open(&folder, Some(LocalOpenKind::Folder))
            .expect("prepare regular folder");
        assert_eq!(prepared_folder.kind(), LocalOpenKind::Folder);
        assert_eq!(
            prepared_folder.path(),
            normalized_for_comparison(&folder).as_path()
        );
        assert_ne!(prepared_file.identity(), prepared_folder.identity());
    }

    #[test]
    fn rejects_relative_paths_and_kind_changes() {
        let relative = Path::new("relative.txt");
        assert!(prepare_local_open(relative, None).is_err());

        let fixture = Fixture::new("kind");
        let file = fixture.root.join("sample.txt");
        fs::write(&file, b"sample").unwrap();
        let error = prepare_local_open(&file, Some(LocalOpenKind::Folder))
            .expect_err("file must not satisfy folder authorization");
        assert!(error.contains("changed type"), "{error}");
    }

    #[cfg(windows)]
    #[test]
    fn rejects_unc_and_device_namespaces_before_any_open() {
        for path in [
            Path::new(r"\\server\share\file.txt"),
            Path::new(r"\\?\UNC\server\share\file.txt"),
            Path::new(r"\\.\C:\Windows"),
        ] {
            assert!(
                prepare_local_open(path, None).is_err(),
                "{} must not enter the local-open boundary",
                path.display()
            );
        }
    }

    #[test]
    fn injectable_launcher_receives_the_proved_path_synchronously() {
        let fixture = Fixture::new("launcher");
        let file = fixture.root.join("sample.txt");
        fs::write(&file, b"sample").unwrap();
        let called = Cell::new(false);

        open_local_path_with(&file, Some(LocalOpenKind::File), |prepared| {
            assert_eq!(prepared.path(), normalized_for_comparison(&file));
            assert_eq!(prepared.kind(), LocalOpenKind::File);
            called.set(true);
            Ok(())
        })
        .expect("injected launcher should succeed");

        assert!(called.get(), "launcher must run before the API returns");
    }

    #[cfg(windows)]
    #[test]
    fn windows_component_guards_block_writes_and_renames_until_launcher_returns() {
        let fixture = Fixture::new("guard-lifetime");
        let parent = fixture.root.join("parent");
        let file = parent.join("sample.txt");
        let renamed_file = parent.join("renamed.txt");
        let renamed_parent = fixture.root.join("renamed-parent");
        fs::create_dir(&parent).unwrap();
        fs::write(&file, b"sample").unwrap();

        open_local_path_with(&file, Some(LocalOpenKind::File), |_prepared| {
            assert!(
                fs::OpenOptions::new().write(true).open(&file).is_err(),
                "final target must not share writes during launch"
            );
            assert!(
                fs::rename(&file, &renamed_file).is_err(),
                "final target must be delete-share guarded during launch"
            );
            assert!(
                fs::rename(&parent, &renamed_parent).is_err(),
                "parent components must be delete-share guarded during launch"
            );
            Ok(())
        })
        .expect("injected launcher should succeed");

        fs::OpenOptions::new()
            .write(true)
            .open(&file)
            .expect("write guard releases after launch");
        fs::rename(&file, &renamed_file).expect("target guard releases after launch");
        fs::rename(&parent, &renamed_parent).expect("parent guard releases after launch");
    }

    #[cfg(windows)]
    #[test]
    fn windows_folder_guard_blocks_replacement_but_allows_child_mutation() {
        let fixture = Fixture::new("folder-guard");
        let folder = fixture.root.join("selected");
        let renamed = fixture.root.join("renamed");
        fs::create_dir(&folder).unwrap();

        open_local_path_with(&folder, Some(LocalOpenKind::Folder), |_prepared| {
            assert!(
                fs::rename(&folder, &renamed).is_err(),
                "selected folder binding must remain stable"
            );
            fs::write(folder.join("new-child.txt"), b"child mutation")
                .expect("guarded folder must still permit an authorized child write");
            Ok(())
        })
        .expect("injected folder operation should succeed");

        fs::rename(&folder, &renamed).expect("folder guard releases after operation");
    }

    #[test]
    fn rejects_symbolic_link_components_when_the_platform_can_create_them() {
        let fixture = Fixture::new("link");
        let real = fixture.root.join("real");
        let link = fixture.root.join("link");
        fs::create_dir(&real).unwrap();

        #[cfg(windows)]
        let created = std::os::windows::fs::symlink_dir(&real, &link).is_ok();
        #[cfg(unix)]
        let created = std::os::unix::fs::symlink(&real, &link).is_ok();
        #[cfg(not(any(windows, unix)))]
        let created = false;

        if created {
            let error = prepare_local_open(&link, Some(LocalOpenKind::Folder))
                .expect_err("link target must be rejected");
            assert!(
                error.contains("reparse") || error.contains("Symbolic"),
                "{error}"
            );
        }
    }

    #[cfg(windows)]
    fn normalized_for_comparison(path: &Path) -> PathBuf {
        path.canonicalize().unwrap()
    }

    #[cfg(not(windows))]
    fn normalized_for_comparison(path: &Path) -> PathBuf {
        path.canonicalize().unwrap()
    }

    #[allow(dead_code)]
    fn assert_prepared_is_borrowed(_prepared: &PreparedLocalOpen) {}
}
