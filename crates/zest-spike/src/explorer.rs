//! Hotkey handling and Explorer selection through supported shell COM APIs
//! (`IShellWindows` -> `IShellBrowser` -> `IFolderView`). No injection.

use anyhow::{anyhow, Result};
use windows::core::{Interface, VARIANT};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, IServiceProvider, CLSCTX_ALL};
use windows::Win32::UI::Shell::{
    IFolderView, IShellBrowser, IShellItemArray, IShellView, IShellWindows, ShellWindows,
    SID_STopLevelBrowser, SIGDN_FILESYSPATH, SVGIO_SELECTION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetClassNameW, GetForegroundWindow, IsWindowVisible, GA_ROOT,
};

use crate::log;
use crate::wheel;

/// Hotkey (Ctrl+Alt+Space) or hold gesture: open the wheel for the Explorer
/// selection in keyboard/click mode.
pub fn open_for_selection(source: &str) {
    let t0 = log::qpc_now();
    // SAFETY: simple query.
    let fg = unsafe { GetForegroundWindow() };
    let mut class = [0u16; 64];
    // SAFETY: buffer is valid.
    let n = unsafe { GetClassNameW(fg, &mut class) }.max(0) as usize;
    log::event(
        "open_for_selection",
        &format!("source={source} fg=0x{:x} class={}", fg.0 as usize, String::from_utf16_lossy(&class[..n])),
    );

    let paths = match selection_for(fg) {
        Ok(Some(p)) => {
            log::event("explorer_selection", &format!("count={} paths={:?}", p.len(), p));
            p
        }
        Ok(None) => {
            log::event("explorer_not_foreground", "");
            Vec::new()
        }
        Err(e) => {
            log::event("explorer_error", &format!("err={e:#}"));
            Vec::new()
        }
    };
    log::event("selection_ms", &format!("{:.3}", log::ms_since(t0)));
    let n = paths.len();
    wheel::with(|w| w.show_keyboard(n, n == 0));
}

/// Selected paths of the Explorer window that is the foreground window.
/// `Ok(None)` when the foreground window is not an Explorer window.
fn selection_for(fg: HWND) -> Result<Option<Vec<String>>> {
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
            log::event(
                "explorer_match",
                &format!("index={i} tab=0x{:x} visible={visible}", tab.0 as usize),
            );
            matches.push((tab, visible, sb));
        }
        if matches.is_empty() {
            return Ok(None);
        }
        let chosen = matches
            .iter()
            .find(|m| m.1)
            .unwrap_or(&matches[0]);
        log::event(
            "explorer_chosen",
            &format!("tab=0x{:x} matches={}", chosen.0 .0 as usize, matches.len()),
        );
        let view: IShellView = chosen.2.QueryActiveShellView()?;
        let fv: IFolderView = view.cast()?;
        // Fails (or is empty) when nothing is selected.
        let items: IShellItemArray = match fv.Items(SVGIO_SELECTION) {
            Ok(i) => i,
            Err(_) => return Ok(Some(Vec::new())),
        };
        let n = items.GetCount().map_err(|e| anyhow!("GetCount: {e}"))?;
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n {
            let item = items.GetItemAt(i)?;
            match item.GetDisplayName(SIGDN_FILESYSPATH) {
                Ok(name) => {
                    out.push(name.to_string().unwrap_or_default());
                    CoTaskMemFree(Some(name.0 as *const _));
                }
                Err(e) => log::event("item_no_path", &format!("index={i} err={e}")),
            }
        }
        Ok(Some(out))
    }
}

