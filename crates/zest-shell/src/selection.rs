//! Selected items of the foreground Explorer window or the Desktop, through
//! supported shell COM APIs (`IShellWindows` -> `IShellBrowser` ->
//! `IFolderView`). No injection.

use std::path::PathBuf;

use windows::core::{Interface, VARIANT};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, IServiceProvider, CLSCTX_ALL};
use windows::Win32::UI::Shell::{
    IFolderView, IShellBrowser, IShellItemArray, IShellView, IShellWindows, ShellWindows,
    SID_STopLevelBrowser, SIGDN_FILESYSPATH, SVGIO_SELECTION, SWC_DESKTOP, SWFO_NEEDDISPATCH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetClassNameW, GetForegroundWindow, IsWindowVisible, GA_ROOT,
};

use crate::Result;

/// What the foreground window is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// An Explorer window (possibly empty selection).
    Explorer(Vec<PathBuf>),
    /// The Desktop (possibly empty selection).
    Desktop(Vec<PathBuf>),
    /// Anything else.
    NotShell,
}

impl Selection {
    /// Selected paths (empty for `NotShell`).
    pub fn paths(&self) -> &[PathBuf] {
        match self {
            Selection::Explorer(p) | Selection::Desktop(p) => p,
            Selection::NotShell => &[],
        }
    }
}

fn class_name(hwnd: HWND) -> String {
    let mut class = [0u16; 64];
    // SAFETY: buffer is valid for its length.
    let n = unsafe { GetClassNameW(hwnd, &mut class) }.max(0) as usize;
    String::from_utf16_lossy(&class[..n])
}

/// Selection of the current foreground window. Call on the UI (STA) thread.
pub fn foreground_selection() -> Result<Selection> {
    // SAFETY: simple query.
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_invalid() {
        return Ok(Selection::NotShell);
    }
    let class = class_name(fg);
    tracing::debug!(fg = fg.0 as usize, class = %class, "foreground_selection");
    if class == "Progman" || class == "WorkerW" {
        // The Desktop: Progman, or a WorkerW when the wallpaper host is active.
        return Ok(match desktop_selection()? {
            Some(p) => Selection::Desktop(p),
            None => Selection::NotShell,
        });
    }
    Ok(match explorer_selection(fg)? {
        Some(p) => Selection::Explorer(p),
        None => Selection::NotShell,
    })
}

/// Selected paths of the Explorer window `fg` (a top-level window).
/// `Ok(None)` when `fg` is not an Explorer window.
pub fn explorer_selection(fg: HWND) -> Result<Option<Vec<PathBuf>>> {
    // SAFETY: COM calls on the STA thread; all interfaces are owned values.
    unsafe {
        let windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL)?;
        let count = windows.Count()?;
        // (shell tab window, visible, browser)
        let mut matches: Vec<(HWND, bool, IShellBrowser)> = Vec::new();
        for i in 0..count {
            let Ok(disp) = windows.Item(&VARIANT::from(i)) else { continue };
            let Ok(sp) = disp.cast::<IServiceProvider>() else { continue };
            let Ok(sb) = sp.QueryService::<IShellBrowser>(&SID_STopLevelBrowser) else { continue };
            let Ok(tab) = sb.GetWindow() else { continue };
            let root = GetAncestor(tab, GA_ROOT);
            if root != fg {
                continue;
            }
            let visible = IsWindowVisible(tab).as_bool();
            tracing::debug!(index = i, tab = tab.0 as usize, visible, "explorer_match");
            matches.push((tab, visible, sb));
        }
        if matches.is_empty() {
            return Ok(None);
        }
        // Tabbed Explorer: several shell tabs share one top-level window; the
        // visible one is the active tab.
        let chosen = matches.iter().find(|m| m.1).unwrap_or(&matches[0]);
        tracing::debug!(tab = chosen.0 .0 as usize, matches = matches.len(), "explorer_chosen");
        Ok(Some(selection_of_browser(&chosen.2)?))
    }
}

/// Selected paths on the Desktop (`SWC_DESKTOP`). `Ok(None)` if the desktop
/// view cannot be reached.
pub fn desktop_selection() -> Result<Option<Vec<PathBuf>>> {
    // SAFETY: COM calls on the STA thread; all interfaces are owned values.
    unsafe {
        let windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL)?;
        let loc = VARIANT::from(0i32); // CSIDL_DESKTOP
        let root = VARIANT::default();
        let mut hwnd = 0i32;
        let disp = match windows.FindWindowSW(&loc, &root, SWC_DESKTOP, &mut hwnd, SWFO_NEEDDISPATCH)
        {
            Ok(d) => d,
            Err(e) => {
                tracing::debug!("FindWindowSW(desktop) failed: {e}");
                return Ok(None);
            }
        };
        let sp: IServiceProvider = disp.cast()?;
        let sb: IShellBrowser = sp.QueryService(&SID_STopLevelBrowser)?;
        Ok(Some(selection_of_browser(&sb)?))
    }
}

/// SAFETY: call on the STA thread that owns `sb`.
unsafe fn selection_of_browser(sb: &IShellBrowser) -> Result<Vec<PathBuf>> {
    let view: IShellView = sb.QueryActiveShellView()?;
    let fv: IFolderView = view.cast()?;
    // Fails (or is empty) when nothing is selected.
    let items: IShellItemArray = match fv.Items(SVGIO_SELECTION) {
        Ok(i) => i,
        Err(_) => return Ok(Vec::new()),
    };
    let n = items.GetCount()?;
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        let item = items.GetItemAt(i)?;
        match item.GetDisplayName(SIGDN_FILESYSPATH) {
            Ok(name) => {
                out.push(PathBuf::from(name.to_string().unwrap_or_default()));
                CoTaskMemFree(Some(name.0 as *const _));
            }
            // Virtual items (This PC, Recycle Bin, ...) have no file system path.
            Err(e) => tracing::debug!(index = i, "item_no_path: {e}"),
        }
    }
    Ok(out)
}
