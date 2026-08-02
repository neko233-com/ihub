//! Native file drag support for the uTools compatibility surface.
//!
//! Authorization is deliberately outside this module. The caller supplies
//! already-prepared local objects whose identity was matched to paths returned
//! by the current lease's native file picker. Their guards remain alive while
//! Windows Shell owns the modal OLE drag loop.

use crate::system_open::PreparedLocalOpen;

pub(crate) fn start_file_drag(
    window: &tauri::WebviewWindow,
    prepared: &[PreparedLocalOpen],
) -> Result<(), String> {
    if prepared.is_empty() {
        return Err("A native file drag requires at least one prepared object.".to_owned());
    }

    #[cfg(windows)]
    {
        start_windows_file_drag(window, prepared)
    }
    #[cfg(not(windows))]
    {
        let _ = window;
        Err("uTools startDrag has not been runtime-verified on this platform.".to_owned())
    }
}

#[cfg(windows)]
fn start_windows_file_drag(
    window: &tauri::WebviewWindow,
    prepared: &[PreparedLocalOpen],
) -> Result<(), String> {
    use std::{os::windows::ffi::OsStrExt, ptr};

    use windows::{
        core::PCWSTR,
        Win32::{
            System::{
                Com::{CoTaskMemFree, IBindCtx, IDataObject},
                Ole::{
                    IDropSource, OleInitialize, OleUninitialize, DROPEFFECT_COPY, DROPEFFECT_LINK,
                    DROPEFFECT_MOVE,
                },
            },
            UI::Shell::{
                BHID_DataObject, Common::ITEMIDLIST, SHCreateShellItemArrayFromIDLists,
                SHDoDragDrop, SHParseDisplayName,
            },
        },
    };

    struct OleApartment;
    impl Drop for OleApartment {
        fn drop(&mut self) {
            // SAFETY: one successful OleInitialize call is balanced on the
            // same Tauri UI thread before this stack frame returns.
            unsafe { OleUninitialize() };
        }
    }

    struct OwnedItemIdList(*mut ITEMIDLIST);
    impl Drop for OwnedItemIdList {
        fn drop(&mut self) {
            // SAFETY: SHParseDisplayName allocates the absolute PIDL with the
            // COM task allocator and transfers ownership to the caller.
            unsafe { CoTaskMemFree(Some(self.0.cast())) };
        }
    }

    // SAFETY: Tauri dispatches this function to its Windows UI thread. The
    // initialization result is balanced by OleApartment, including S_FALSE.
    unsafe { OleInitialize(None) }
        .map_err(|error| format!("Could not initialize Windows OLE drag support: {error}"))?;
    let _apartment = OleApartment;

    let mut pidls = Vec::with_capacity(prepared.len());
    for item in prepared {
        let mut wide = item.path().as_os_str().encode_wide().collect::<Vec<_>>();
        wide.push(0);
        let mut pidl = ptr::null_mut();
        // SAFETY: the path is NUL-terminated and remains alive for the call;
        // the returned PIDL is checked and immediately placed in an RAII owner.
        unsafe { SHParseDisplayName(PCWSTR(wide.as_ptr()), None::<&IBindCtx>, &mut pidl, 0, None) }
            .map_err(|error| format!("Windows Shell could not prepare a drag item: {error}"))?;
        if pidl.is_null() {
            return Err("Windows Shell returned an empty drag item identity.".to_owned());
        }
        pidls.push(OwnedItemIdList(pidl));
    }

    let raw_pidls = pidls
        .iter()
        .map(|pidl| pidl.0.cast_const())
        .collect::<Vec<_>>();
    // SAFETY: every pointer is a live absolute PIDL retained in `pidls` until
    // after both the shell array and modal drag operation are finished.
    let shell_items = unsafe { SHCreateShellItemArrayFromIDLists(&raw_pidls) }
        .map_err(|error| format!("Windows Shell could not group the drag items: {error}"))?;
    // SAFETY: BHID_DataObject is the documented handler for an IShellItemArray
    // and the requested COM interface is expressed by the result type.
    let data_object: IDataObject =
        unsafe { shell_items.BindToHandler(None::<&IBindCtx>, &BHID_DataObject) }
            .map_err(|error| format!("Windows Shell could not create file drag data: {error}"))?;
    let hwnd = window
        .hwnd()
        .map_err(|error| format!("Could not resolve the iHub window for file dragging: {error}"))?;
    // SAFETY: the HWND, IDataObject, PIDLs, and guarded filesystem objects all
    // remain alive while SHDoDragDrop runs its modal OLE loop. Passing no
    // custom IDropSource asks Shell to provide its standard source behavior.
    unsafe {
        SHDoDragDrop(
            Some(hwnd),
            &data_object,
            None::<&IDropSource>,
            DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK,
        )
    }
    .map_err(|error| format!("The Windows file drag operation failed: {error}"))?;
    Ok(())
}
