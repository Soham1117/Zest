//! Shell file helpers: change notification, Recycle Bin, "show in folder".

use std::ffi::c_void;
use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::GetDriveTypeW;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::UI::Shell::{
    FileOperation, IFileOperation, ILCreateFromPathW, ILFree, SHChangeNotify,
    SHCreateItemFromParsingName, SHOpenFolderAndSelectItems, FILEOPERATION_FLAGS, FOF_ALLOWUNDO,
    FOF_NOERRORUI, FOF_SILENT, SHCNE_CREATE, SHCNE_MKDIR, SHCNF_PATHW,
};

use crate::{wide_path, ComScope, Result, ShellError};

/// `GetDriveTypeW` result for a local fixed disk.
const DRIVE_FIXED: u32 = 3;
/// `FOFX_RECYCLEONDELETE` (Windows 8+).
const FOFX_RECYCLEONDELETE: u32 = 0x0008_0000;

/// Tell Explorer that `path` was created, so it shows the new file (or folder) at once.
pub fn notify_created(path: &Path) {
    let w = wide_path(path);
    let event = if path.is_dir() { SHCNE_MKDIR } else { SHCNE_CREATE };
    // SAFETY: `w` is a NUL-terminated UTF-16 path that outlives the call.
    unsafe {
        SHChangeNotify(event, SHCNF_PATHW, Some(w.as_ptr() as *const c_void), None);
    }
}

/// Send `path` to the Recycle Bin. Never deletes permanently: items on drives
/// without a Recycle Bin (network, removable) are refused, and the shell is
/// not told to skip its confirmation, so a "delete permanently" prompt can
/// never be answered silently.
///
/// Callable from any thread (initializes COM if needed). Used by Undo.
pub fn recycle(path: &Path) -> Result<()> {
    if !path.exists() {
        return Err(ShellError::Invalid(format!("{} does not exist", path.display())));
    }
    ensure_recyclable(path)?;
    let _com = ComScope::new();
    let wpath = wide_path(path);
    // SAFETY: COM objects are owned values; `wpath` outlives the calls.
    unsafe {
        let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
        let flags = FILEOPERATION_FLAGS(FOF_ALLOWUNDO.0 | FOF_SILENT.0 | FOF_NOERRORUI.0 | FOFX_RECYCLEONDELETE);
        op.SetOperationFlags(flags)?;
        let item: windows::Win32::UI::Shell::IShellItem =
            SHCreateItemFromParsingName(PCWSTR(wpath.as_ptr()), None::<&windows::Win32::System::Com::IBindCtx>)?;
        op.DeleteItem(&item, None)?;
        op.PerformOperations()?;
        if op.GetAnyOperationsAborted()?.as_bool() {
            return Err(ShellError::Invalid(format!("recycling {} was aborted", path.display())));
        }
    }
    Ok(())
}

fn ensure_recyclable(path: &Path) -> Result<()> {
    let abs = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let s = abs.to_string_lossy();
    // `\\?\C:\dir\file` -> root `C:\`
    let trimmed = s.strip_prefix(r"\\?\").unwrap_or(&s);
    if trimmed.starts_with(r"UNC\") || trimmed.starts_with(r"\\") {
        return Err(ShellError::Invalid("network paths have no Recycle Bin".into()));
    }
    let root: String = trimmed.chars().take(3).collect();
    let wroot: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: NUL-terminated root string.
    let kind = unsafe { GetDriveTypeW(PCWSTR(wroot.as_ptr())) };
    if kind != DRIVE_FIXED {
        return Err(ShellError::Invalid(format!("drive {root} has no Recycle Bin")));
    }
    Ok(())
}

/// Open the parent folder in Explorer with `path` selected.
pub fn open_folder_select(path: &Path) -> Result<()> {
    let _com = ComScope::new();
    let w = wide_path(path);
    // SAFETY: the PIDL is freed below; `w` is NUL-terminated.
    unsafe {
        let pidl = ILCreateFromPathW(PCWSTR(w.as_ptr()));
        if pidl.is_null() {
            return Err(ShellError::Invalid(format!("cannot resolve {}", path.display())));
        }
        // With no children, the item itself is selected in its parent folder.
        let r = SHOpenFolderAndSelectItems(pidl, None, 0);
        ILFree(Some(pidl));
        r?;
    }
    Ok(())
}
