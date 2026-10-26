//! Tray icon with a small context menu.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, LoadIconW, PostMessageW,
    SetForegroundWindow, TrackPopupMenu, IDI_APPLICATION, MF_STRING, SW_SHOWNORMAL,
    TPM_BOTTOMALIGN, TPM_RIGHTBUTTON, WM_NULL,
};

use crate::input::WM_TRAY;
use crate::log;

const TRAY_ID: u32 = 1;
const CMD_OPEN_LOG: u16 = 101;
const CMD_QUIT: u16 = 102;
const WM_RBUTTONUP: isize = 0x0205;

fn data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ID,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        ..Default::default()
    };
    // SAFETY: loading a stock icon.
    nid.hIcon = unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap_or_default();
    for (i, c) in "Zest spike".encode_utf16().enumerate() {
        nid.szTip[i] = c;
    }
    nid
}

pub fn add(hwnd: HWND) {
    let nid = data(hwnd);
    // SAFETY: nid is fully initialised.
    if !unsafe { Shell_NotifyIconW(NIM_ADD, &nid) }.as_bool() {
        log::event("tray_add_failed", "");
    }
}

pub fn remove(hwnd: HWND) {
    let nid = data(hwnd);
    // SAFETY: nid is fully initialised.
    let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &nid) };
}

pub fn on_message(hwnd: HWND, lparam: windows::Win32::Foundation::LPARAM) {
    if lparam.0 & 0xFFFF == WM_RBUTTONUP {
        show_menu(hwnd);
    }
}

fn show_menu(hwnd: HWND) {
    // SAFETY: standard popup menu on the UI thread.
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN_LOG as usize, w!("Open log"));
        let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT as usize, w!("Quit"));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Required so the menu closes when the user clicks elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pt.x, pt.y, 0, hwnd, None);
        let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
    }
}

pub fn on_command(hwnd: HWND, id: u16) {
    match id {
        CMD_OPEN_LOG => {
            if let Some(path) = log::log_path() {
                let wide: Vec<u16> = path
                    .to_string_lossy()
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect();
                // SAFETY: wide is NUL-terminated and outlives the call.
                unsafe {
                    ShellExecuteW(
                        None,
                        w!("open"),
                        PCWSTR(wide.as_ptr()),
                        None,
                        None,
                        SW_SHOWNORMAL,
                    );
                }
            }
        }
        CMD_QUIT => {
            log::event("quit", "");
            remove(hwnd);
            crate::input::close_main_window(hwnd);
        }
        _ => {}
    }
}


