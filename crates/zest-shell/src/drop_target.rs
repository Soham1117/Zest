//! `IDropTarget` for the wheel window.
//!
//! Rules (spec 1.2): the effect returned is always `DROPEFFECT_COPY` or
//! `DROPEFFECT_NONE`. `DROPEFFECT_MOVE` is never returned, so the drag source
//! never deletes the originals. Only local items that exist are passed on:
//! files and folders. Virtual items (no file system path) are ignored.
//!
//! The target knows nothing about the wheel. It forwards to a [`DropSink`].

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use windows::core::{implement, Result as WinResult};
use windows::Win32::Foundation::{HWND, POINTL};
use windows::Win32::System::Com::{IDataObject, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
use windows::Win32::System::Ole::{
    IDropTarget, IDropTarget_Impl, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop, CF_HDROP,
    DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

/// Receiver of drag events. Called on the UI thread. Coordinates are physical
/// screen pixels (the process must be per-monitor DPI aware).
pub trait DropSink {
    /// A drag with `files` (local existing files and folders, possibly empty)
    /// entered the window. Return `true` to take part in the drag. `false` makes the target
    /// answer NONE until the drag leaves.
    fn drag_enter(&self, files: &[PathBuf]) -> bool;
    /// Pointer moved. Return `true` if a drop at this point would be accepted
    /// (the target then answers COPY if the source allows it).
    fn drag_over(&self, x: i32, y: i32) -> bool;
    /// The drag left the window or was cancelled.
    fn drag_leave(&self);
    /// The user released over the window. Return `true` to accept (COPY).
    fn drop_files(&self, files: &[PathBuf], x: i32, y: i32) -> bool;
}

#[implement(IDropTarget)]
struct DropTarget {
    sink: Rc<dyn DropSink>,
    /// Files seen at `DragEnter` (also used by `Drop`).
    files: RefCell<Vec<PathBuf>>,
    /// The sink declined this drag at `DragEnter`.
    declined: RefCell<bool>,
}

/// Register a drop target for `hwnd` (UI thread, STA). Replaces nothing: call
/// [`revoke_drop_target`] first when re-registering.
pub fn register_drop_target(hwnd: HWND, sink: Rc<dyn DropSink>) -> crate::Result<()> {
    let target: IDropTarget = DropTarget {
        sink,
        files: RefCell::new(Vec::new()),
        declined: RefCell::new(false),
    }
    .into();
    // SAFETY: valid HWND created on this STA thread.
    unsafe { RegisterDragDrop(hwnd, &target) }?;
    Ok(())
}

/// Remove the drop target again. Errors are logged, not returned.
pub fn revoke_drop_target(hwnd: HWND) {
    // SAFETY: valid HWND on the registering thread.
    if let Err(e) = unsafe { RevokeDragDrop(hwnd) } {
        tracing::warn!("RevokeDragDrop failed: {e}");
    }
}

/// Read file paths from CF_HDROP. `None` when the data object has no HDROP.
fn read_hdrop(data: &IDataObject) -> Option<Vec<PathBuf>> {
    let fmt = FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    // SAFETY: standard OLE data retrieval; the medium is released below.
    unsafe {
        let mut medium = data.GetData(&fmt).ok()?;
        let hdrop = HDROP(medium.u.hGlobal.0);
        let count = DragQueryFileW(hdrop, 0xFFFF_FFFF, None);
        let mut paths = Vec::with_capacity(count as usize);
        for i in 0..count {
            let len = DragQueryFileW(hdrop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            let n = DragQueryFileW(hdrop, i, Some(&mut buf)) as usize;
            paths.push(PathBuf::from(String::from_utf16_lossy(&buf[..n.min(buf.len())])));
        }
        ReleaseStgMedium(&mut medium);
        Some(paths)
    }
}

/// Local existing files and folders. Virtual items have no path in `CF_HDROP`,
/// and anything that does not exist is dropped.
fn local_items(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.into_iter().filter(|p| keep_item(p)).collect()
}

fn keep_item(p: &Path) -> bool {
    p.is_file() || p.is_dir()
}

/// Copy if the source allows it, otherwise none. Never MOVE.
fn copy_if_allowed(allowed: DROPEFFECT) -> DROPEFFECT {
    if allowed.0 & DROPEFFECT_COPY.0 != 0 {
        DROPEFFECT_COPY
    } else {
        DROPEFFECT_NONE
    }
}

#[allow(non_snake_case)]
impl IDropTarget_Impl for DropTarget_Impl {
    fn DragEnter(
        &self,
        pdataobj: Option<&IDataObject>,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> WinResult<()> {
        let all = pdataobj.and_then(read_hdrop);
        let total = all.as_ref().map_or(0, Vec::len);
        let files = all.map(local_items).unwrap_or_default();
        tracing::debug!(items = total, local_items = files.len(), "drag_enter");
        // No local files: refuse (the sink still hears about it so it can stay invisible).
        let take = self.sink.drag_enter(&files) && !files.is_empty();
        *self.declined.borrow_mut() = !take;
        *self.files.borrow_mut() = files;
        // SAFETY: OLE passes a valid pointer. DragOver refines the effect right after.
        unsafe {
            let allowed = *pdweffect;
            *pdweffect = if take { copy_if_allowed(allowed) } else { DROPEFFECT_NONE };
        }
        Ok(())
    }

    fn DragOver(
        &self,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> WinResult<()> {
        // SAFETY: OLE passes a valid pointer.
        let allowed = unsafe { *pdweffect };
        let effect = if *self.declined.borrow() {
            DROPEFFECT_NONE
        } else if self.sink.drag_over(pt.x, pt.y) {
            copy_if_allowed(allowed)
        } else {
            DROPEFFECT_NONE
        };
        // SAFETY: valid out pointer.
        unsafe { *pdweffect = effect };
        Ok(())
    }

    fn DragLeave(&self) -> WinResult<()> {
        self.sink.drag_leave();
        *self.declined.borrow_mut() = false;
        tracing::debug!("drag_leave");
        Ok(())
    }

    fn Drop(
        &self,
        _pdataobj: Option<&IDataObject>,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> WinResult<()> {
        // SAFETY: OLE passes a valid pointer.
        let allowed = unsafe { *pdweffect };
        let files = std::mem::take(&mut *self.files.borrow_mut());
        let accepted = !*self.declined.borrow() && self.sink.drop_files(&files, pt.x, pt.y);
        *self.declined.borrow_mut() = false;
        let effect = if accepted { copy_if_allowed(allowed) } else { DROPEFFECT_NONE };
        // SAFETY: valid out pointer.
        unsafe { *pdweffect = effect };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_and_folders_are_kept_missing_items_are_not() {
        let dir = std::env::temp_dir().join(format!("zest-drop-{}", std::process::id()));
        let sub = dir.join("a folder");
        std::fs::create_dir_all(&sub).unwrap();
        let file = dir.join("f.txt");
        std::fs::write(&file, b"x").unwrap();
        let missing = dir.join("nope.txt");
        let kept = local_items(vec![file.clone(), sub.clone(), missing, PathBuf::from("::{virtual}")]);
        assert_eq!(kept, vec![file, sub]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
