//! Tray icon, balloon notifications and the tray menu.

use std::path::Path;
use std::sync::OnceLock;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
use windows::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_ERROR,
    NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, LoadImageW,
    PostMessageW, SetForegroundWindow, TrackPopupMenu, HICON, IMAGE_ICON, LR_DEFAULTCOLOR,
    MENU_ITEM_FLAGS, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, SM_CXSMICON, SW_SHOWNORMAL,
    TPM_BOTTOMALIGN, TPM_RIGHTBUTTON, WM_APP, WM_NULL,
};

pub const WM_TRAY: u32 = WM_APP + 1;
pub const WM_RBUTTONUP: u32 = 0x0205;
pub const NIN_BALLOONUSERCLICK: u32 = 0x0405;

const TRAY_ID: u32 = 1;

pub mod cmd {
    pub const PAUSE: u16 = 101;
    pub const CANCEL: u16 = 102;
    pub const SETTINGS: u16 = 103;
    pub const LOGS: u16 = 104;
    pub const AUTOSTART: u16 = 105;
    pub const CONTEXT: u16 = 106;
    pub const QUIT: u16 = 107;
    pub const SHELF: u16 = 108;
    pub const HISTORY: u16 = 109;
    pub const SETTINGS_WINDOW: u16 = 110;
    pub const WATCH_PAUSE: u16 = 111;
    pub const QUIET: u16 = 112;
    pub const CLIPBOARD: u16 = 113;
    pub const WIN11_MENU: u16 = 114;
}

/// What the menu shows.
#[derive(Clone, Copy, Debug, Default)]
pub struct MenuState {
    pub paused: bool,
    pub jobs_running: bool,
    pub autostart: bool,
    pub context_menu: bool,
    pub watch_paused: bool,
    pub quiet: bool,
    pub clipboard_ready: bool,
    pub win11_menu: bool,
}

fn icon() -> HICON {
    static ICON: OnceLock<isize> = OnceLock::new();
    let raw = *ICON.get_or_init(|| {
        // SAFETY: resource id 1 is the icon embedded by build.rs.
        unsafe {
            let size = GetSystemMetricsForDpi(SM_CXSMICON, GetDpiForSystem());
            let hinst = GetModuleHandleW(None).unwrap_or_default();
            LoadImageW(hinst, PCWSTR(std::ptr::without_provenance(1)), IMAGE_ICON, size, size, LR_DEFAULTCOLOR)
                .map(|h| h.0 as isize)
                .unwrap_or(0)
        }
    });
    HICON(raw as *mut std::ffi::c_void)
}

fn copy_wide<const N: usize>(dst: &mut [u16; N], text: &str) {
    dst.fill(0);
    for (i, c) in text.encode_utf16().take(N - 1).enumerate() {
        dst[i] = c;
    }
}

fn base(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ID,
        ..Default::default()
    }
}

/// Add the icon. Also used again when Explorer restarts (`TaskbarCreated`).
pub fn add(hwnd: HWND) {
    let mut nid = base(hwnd);
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = icon();
    copy_wide(&mut nid.szTip, "Zest");
    // SAFETY: nid is fully initialised.
    if !unsafe { Shell_NotifyIconW(NIM_ADD, &nid) }.as_bool() {
        tracing::warn!("tray icon could not be added");
    }
}

pub fn remove(hwnd: HWND) {
    let nid = base(hwnd);
    // SAFETY: nid is fully initialised.
    let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &nid) };
}

/// Show a balloon (toast) next to the tray icon.
pub fn balloon(hwnd: HWND, title: &str, text: &str, error: bool) {
    let mut nid = base(hwnd);
    nid.uFlags = NIF_INFO;
    copy_wide(&mut nid.szInfoTitle, title);
    copy_wide(&mut nid.szInfo, text);
    nid.dwInfoFlags = if error { NIIF_ERROR } else { NIIF_INFO };
    // SAFETY: nid is fully initialised.
    if !unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) }.as_bool() {
        tracing::warn!("balloon could not be shown");
    }
}

/// Show the context menu at the cursor. The chosen item arrives as `WM_COMMAND`.
pub fn show_menu(hwnd: HWND, st: MenuState) {
    let flag = |checked: bool| {
        if checked {
            MENU_ITEM_FLAGS(MF_STRING.0 | MF_CHECKED.0)
        } else {
            MF_STRING
        }
    };
    // SAFETY: standard popup menu on the UI thread; strings are static.
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let _ = AppendMenuW(menu, flag(st.paused), cmd::PAUSE as usize, w!("Pause gestures"));
        let _ = AppendMenuW(menu, flag(st.watch_paused), cmd::WATCH_PAUSE as usize, w!("Pause watch folders"));
        let _ = AppendMenuW(menu, flag(st.quiet), cmd::QUIET as usize, w!("Quiet mode"));
        if st.clipboard_ready {
            let _ = AppendMenuW(menu, MF_STRING, cmd::CLIPBOARD as usize, w!("Convert clipboard"));
        } else {
            let _ = AppendMenuW(
                menu,
                MENU_ITEM_FLAGS(MF_STRING.0 | MF_GRAYED.0),
                cmd::CLIPBOARD as usize,
                w!("Convert clipboard"),
            );
        }
        let _ = AppendMenuW(
            menu,
            flag(st.win11_menu),
            cmd::WIN11_MENU as usize,
            w!("Top-level Convert with Zest"),
        );
        if st.jobs_running {
            let _ = AppendMenuW(menu, MF_STRING, cmd::CANCEL as usize, w!("Cancel all jobs"));
        } else {
            let _ = AppendMenuW(
                menu,
                MENU_ITEM_FLAGS(MF_STRING.0 | MF_GRAYED.0),
                cmd::CANCEL as usize,
                w!("Cancel all jobs"),
            );
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, cmd::SHELF as usize, w!("Show shelf"));
        let _ = AppendMenuW(menu, MF_STRING, cmd::HISTORY as usize, w!("History…"));
        let _ = AppendMenuW(menu, MF_STRING, cmd::SETTINGS_WINDOW as usize, w!("Settings…"));
        let _ = AppendMenuW(menu, MF_STRING, cmd::SETTINGS as usize, w!("Open settings file"));
        let _ = AppendMenuW(menu, MF_STRING, cmd::LOGS as usize, w!("Open log folder"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, flag(st.autostart), cmd::AUTOSTART as usize, w!("Start at sign-in"));
        let _ = AppendMenuW(menu, flag(st.context_menu), cmd::CONTEXT as usize, w!("Add to right-click menu"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, cmd::QUIT as usize, w!("Quit"));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Needed so the menu closes when the user clicks elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pt.x, pt.y, 0, hwnd, None);
        let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
    }
}

/// Open a file or folder with its default program. Returns false on failure.
pub fn shell_open(path: &Path) -> bool {
    let wide: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: NUL-terminated string that outlives the call.
    let r = unsafe { ShellExecuteW(None, w!("open"), PCWSTR(wide.as_ptr()), None, None, SW_SHOWNORMAL) };
    r.0 as isize > 32
}

/// Open a file in Notepad (when no program is set for its type).
pub fn open_in_notepad(path: &Path) -> bool {
    let quoted = format!("\"{}\"", path.as_os_str().to_string_lossy());
    let wide: Vec<u16> = quoted.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: NUL-terminated string that outlives the call.
    let r = unsafe {
        ShellExecuteW(None, w!("open"), w!("notepad.exe"), PCWSTR(wide.as_ptr()), None, SW_SHOWNORMAL)
    };
    r.0 as isize > 32
}
