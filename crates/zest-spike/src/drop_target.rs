//! `IDropTarget` for the wheel window.
//!
//! Rules (spec 1.2): the effect returned is always `DROPEFFECT_COPY` or
//! `DROPEFFECT_NONE`. `DROPEFFECT_MOVE` is never returned, so the drag source
//! never deletes the originals.

use windows::core::{implement, Result};
use windows::Win32::Foundation::{HWND, POINTL};
use windows::Win32::System::Com::{
    IDataObject, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL,
};
use windows::Win32::System::Ole::{
    IDropTarget, IDropTarget_Impl, ReleaseStgMedium, RegisterDragDrop, RevokeDragDrop, CF_HDROP, DROPEFFECT,
    DROPEFFECT_COPY, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

use crate::log;
use crate::wheel::{self, WheelState};
use std::path::Path;

#[implement(IDropTarget)]
struct DropTarget;

/// Register the drop target on the wheel window.
pub fn register(hwnd: HWND) -> Result<()> {
    let target: IDropTarget = DropTarget.into();
    // SAFETY: valid HWND created on this STA thread.
    unsafe { RegisterDragDrop(hwnd, &target) }
}

pub fn revoke(hwnd: HWND) {
    // SAFETY: valid HWND on the registering thread.
    if let Err(e) = unsafe { RevokeDragDrop(hwnd) } {
        log::event("revoke_drag_drop_error", &format!("err={e}"));
    }
}

/// Read file paths from CF_HDROP. `None` when the data object has no HDROP.
fn read_hdrop(data: &IDataObject) -> Option<Vec<String>> {
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
            paths.push(String::from_utf16_lossy(&buf[..n.min(buf.len())]));
        }
        ReleaseStgMedium(&mut medium);
        Some(paths)
    }
}

/// Local existing files only (folders and virtual items are ignored).
fn local_files(paths: Vec<String>) -> Vec<String> {
    paths.into_iter().filter(|p| Path::new(p).is_file()).collect()
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
        grfkeystate: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> Result<()> {
        let t0 = log::qpc_now();
        // SAFETY: OLE passes a valid pointer.
        let allowed = unsafe { *pdweffect };
        log::event(
            "drag_enter",
            &format!("grfKeyState=0x{:x} allowed_effect=0x{:x}", grfkeystate.0, allowed.0),
        );
        let all = pdataobj.and_then(read_hdrop);
        let total = all.as_ref().map_or(0, Vec::len);
        let files = all.map(local_files).unwrap_or_default();
        let first: Vec<&str> = files.iter().take(3).map(String::as_str).collect();
        log::event(
            "hdrop",
            &format!(
                "items={total} local_files={} read_ms={:.3} first3={:?}",
                files.len(),
                log::ms_since(t0),
                first
            ),
        );
        // No local files: stay invisible and refuse the drop.
        let effect = if files.is_empty() { DROPEFFECT_NONE } else { copy_if_allowed(allowed) };
        wheel::with(|w| w.on_drag_enter(files.len(), t0));
        // SAFETY: valid out pointer.
        unsafe { *pdweffect = effect };
        Ok(())
    }

    fn DragOver(
        &self,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> Result<()> {
        let t0 = log::qpc_now();
        // SAFETY: OLE passes a valid pointer.
        let allowed = unsafe { *pdweffect };
        // pt is in physical screen pixels (process is per-monitor DPI aware v2).
        let sector = wheel::with(|w| w.on_drag_over(pt.x, pt.y, t0)).flatten();
        let effect = if sector.is_some() { copy_if_allowed(allowed) } else { DROPEFFECT_NONE };
        // SAFETY: valid out pointer.
        unsafe { *pdweffect = effect };
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        // Like Filee: the wheel stays up (the drag may come back); the
        // drag-ended handling closes it.
        wheel::with(|w| w.on_drag_leave());
        log::event("drag_leave", "");
        Ok(())
    }

    fn Drop(
        &self,
        _pdataobj: Option<&IDataObject>,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> Result<()> {
        let t0 = log::qpc_now();
        // SAFETY: OLE passes a valid pointer.
        let allowed = unsafe { *pdweffect };
        let mut effect = DROPEFFECT_NONE;
        wheel::with(|w| {
            let sector = match w.state() {
                WheelState::Visible { .. } => w.sector_at_screen(pt.x, pt.y),
                _ => None,
            };
            match sector {
                Some(i) => {
                    log::event(
                        "drop",
                        &format!("sector={i} label={} files={}", w.label(i), w.file_count_shown()),
                    );
                    effect = copy_if_allowed(allowed);
                }
                None => log::event("drop_cancel", ""),
            }
            w.hide("drop");
        });
        log::event("hide_ms", &format!("from_drop={:.3}", log::ms_since(t0)));
        // SAFETY: valid out pointer.
        unsafe { *pdweffect = effect };
        Ok(())
    }
}
