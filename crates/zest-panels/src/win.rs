//! Small Win32 helpers shared by the three panels: window classes, fonts,
//! per-monitor DPI, dark mode, owner-drawn check boxes, list view helpers,
//! background-thread mailbox, clipboard and shell launch.

#![allow(clippy::too_many_arguments)]

use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};

use windows::core::{w, PCSTR, PCWSTR};
use windows::Win32::Foundation::{BOOL, COLORREF, HANDLE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWINDOWATTRIBUTE,
};
use windows::Win32::Graphics::Gdi::{
    CreatePen, CreateSolidBrush, DeleteObject, DrawTextW, Ellipse, FillRect, GetDC, GetTextFaceW,
    InvalidateRect, LineTo, MoveToEx, ReleaseDC, RoundRect, SelectObject, SetBkMode, SetTextColor, DT_LEFT,
    DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, HBRUSH, HFONT, HGDIOBJ, PS_SOLID, TRANSPARENT,
};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::System::Time::GetTimeZoneInformation;
use windows::Win32::UI::Controls::{SetWindowTheme, DRAWITEMSTRUCT, ODS_DISABLED, ODS_FOCUS};
use windows::Win32::UI::HiDpi::{GetDpiForWindow, SystemParametersInfoForDpi};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetClientRect, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW,
    LoadIconW, LoadCursorW, PostMessageW, RegisterClassExW, SendMessageW, SetWindowLongPtrW, SetWindowPos,
    SetWindowTextW, CREATESTRUCTW, GWLP_USERDATA, HMENU, IDC_ARROW, NONCLIENTMETRICSW,
    SPI_GETNONCLIENTMETRICS, SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOWNORMAL,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_NCCREATE, WM_NCDESTROY, WM_SETFONT, WNDCLASSEXW, WS_CHILD,
    WS_VISIBLE,
};

/// Posted to a panel window when its [`Mailbox`] has new items.
pub const WM_MAILBOX: u32 = WM_APP + 40;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn wide_path(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

pub fn from_wide(buf: &[u16]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

pub const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

pub fn loword(v: usize) -> u16 {
    (v & 0xFFFF) as u16
}

pub fn hiword(v: usize) -> u16 {
    ((v >> 16) & 0xFFFF) as u16
}

// ---------------------------------------------------------------------------
// Mailbox: results from short-lived threads
// ---------------------------------------------------------------------------

/// A queue that threads fill and the window drains. `post` wakes the window
/// with [`WM_MAILBOX`].
pub struct Mailbox<T> {
    q: Arc<Mutex<Vec<T>>>,
    hwnd: Arc<AtomicIsize>,
}

impl<T> Clone for Mailbox<T> {
    fn clone(&self) -> Self {
        Mailbox { q: Arc::clone(&self.q), hwnd: Arc::clone(&self.hwnd) }
    }
}

impl<T> Default for Mailbox<T> {
    fn default() -> Self {
        Mailbox { q: Arc::new(Mutex::new(Vec::new())), hwnd: Arc::new(AtomicIsize::new(0)) }
    }
}

impl<T> Mailbox<T> {
    pub fn set_hwnd(&self, hwnd: HWND) {
        self.hwnd.store(hwnd.0 as isize, Ordering::SeqCst);
    }

    pub fn post(&self, item: T) {
        if let Ok(mut q) = self.q.lock() {
            q.push(item);
        }
        let h = self.hwnd.load(Ordering::SeqCst);
        if h != 0 {
            // SAFETY: PostMessageW is thread safe; a dead window only makes it fail.
            unsafe {
                let _ = PostMessageW(HWND(h as *mut c_void), WM_MAILBOX, WPARAM(0), LPARAM(0));
            }
        }
    }

    pub fn drain(&self) -> Vec<T> {
        self.q.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default()
    }
}

/// Run `f` on a short-lived thread (COM is initialised on it).
pub fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    let r = std::thread::Builder::new().name(name.into()).spawn(move || {
        // SAFETY: balanced by CoUninitialize below when it succeeded.
        let com = unsafe {
            windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED)
        };
        let _ = catch_unwind(AssertUnwindSafe(f));
        if com.is_ok() {
            // SAFETY: matches the CoInitializeEx above.
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
    });
    if r.is_err() {
        tracing::error!("cannot start the {name} thread");
    }
}

// ---------------------------------------------------------------------------
// Dark mode and colors
// ---------------------------------------------------------------------------

/// `AppsUseLightTheme` == 0.
pub fn system_uses_dark() -> bool {
    let mut data: u32 = 1;
    let mut size = 4u32;
    // SAFETY: out-pointers to locals.
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    };
    r.is_ok() && data == 0
}

/// Let menus and common controls follow the system theme (best effort:
/// `SetPreferredAppMode` and `FlushMenuThemes` are exported by ordinal).
pub fn init_dark_mode_support() {
    // SAFETY: the ordinals are stable since Windows 10 1903; a missing export is skipped.
    unsafe {
        let Ok(h) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        if let Some(f) = GetProcAddress(h, PCSTR(135 as *const u8)) {
            let f: extern "system" fn(i32) -> i32 = std::mem::transmute(f);
            f(1); // AllowDark: follow the system setting
        }
        if let Some(f) = GetProcAddress(h, PCSTR(136 as *const u8)) {
            let f: extern "system" fn() = std::mem::transmute(f);
            f();
        }
    }
}

pub struct Theme {
    pub dark: bool,
    pub bg: COLORREF,
    pub fg: COLORREF,
    pub edit_bg: COLORREF,
    pub muted: COLORREF,
    pub border: COLORREF,
    pub accent: COLORREF,
    pub error: COLORREF,
    pub bg_brush: HBRUSH,
    pub edit_brush: HBRUSH,
}

impl Theme {
    pub fn new(dark: bool) -> Theme {
        let (bg, fg, edit_bg, muted, border, error) = if dark {
            (rgb(32, 32, 32), rgb(255, 255, 255), rgb(45, 45, 45), rgb(170, 170, 170), rgb(75, 75, 75), rgb(255, 120, 120))
        } else {
            (rgb(243, 243, 243), rgb(0, 0, 0), rgb(255, 255, 255), rgb(96, 96, 96), rgb(205, 205, 205), rgb(190, 0, 0))
        };
        // SAFETY: plain GDI brush creation; deleted in Drop.
        let (bg_brush, edit_brush) = unsafe { (CreateSolidBrush(bg), CreateSolidBrush(edit_bg)) };
        Theme { dark, bg, fg, edit_bg, muted, border, accent: rgb(0, 103, 192), error, bg_brush, edit_brush }
    }
}

impl Drop for Theme {
    fn drop(&mut self) {
        // SAFETY: brushes created in `new`.
        unsafe {
            let _ = DeleteObject(self.bg_brush);
            let _ = DeleteObject(self.edit_brush);
        }
    }
}

const DWMWA_SYSTEMBACKDROP_TYPE_ID: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(38);

/// Dark title bar and Mica backdrop (best effort; older builds ignore both).
pub fn style_frame(hwnd: HWND, dark: bool) {
    let d = BOOL::from(dark);
    let mica: i32 = 2; // DWMSBT_MAINWINDOW
    // SAFETY: pointers to locals with the sizes DWM expects.
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &d as *const BOOL as *const c_void, 4);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_SYSTEMBACKDROP_TYPE_ID, &mica as *const i32 as *const c_void, 4);
    }
}

/// Visual style of a control for the theme.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Button,
    Edit,
    Combo,
    List,
    Header,
}

pub fn theme_control(h: HWND, kind: Kind, dark: bool) {
    let name = match (kind, dark) {
        (Kind::Edit | Kind::Combo, true) => w!("DarkMode_CFD"),
        (Kind::Header, true) => w!("DarkMode_ItemsView"),
        (_, true) => w!("DarkMode_Explorer"),
        (Kind::Edit | Kind::Combo | Kind::Header, false) => w!("Explorer"),
        (_, false) => w!("Explorer"),
    };
    // SAFETY: valid window handle, static strings.
    unsafe {
        let _ = SetWindowTheme(h, name, PCWSTR::null());
    }
}

// ---------------------------------------------------------------------------
// DPI and fonts
// ---------------------------------------------------------------------------

pub fn window_dpi(h: HWND) -> u32 {
    // SAFETY: plain query.
    let d = unsafe { GetDpiForWindow(h) };
    if d == 0 {
        96
    } else {
        d
    }
}

pub fn scale(v: i32, dpi: u32) -> i32 {
    ((i64::from(v) * i64::from(dpi) + 48) / 96) as i32
}

/// The message font of the system at this DPI. Windows 11 gets "Segoe UI
/// Variable Text" when it is there. Free the result with `DeleteObject`.
pub fn create_font(dpi: u32, bold: bool) -> HFONT {
    // SAFETY: NONCLIENTMETRICSW is filled by the system; GDI objects are freed below.
    unsafe {
        let mut ncm = NONCLIENTMETRICSW { cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32, ..Default::default() };
        let ok = SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS.0,
            ncm.cbSize,
            Some(&mut ncm as *mut _ as *mut c_void),
            0,
            dpi,
        )
        .is_ok();
        let mut lf = if ok { ncm.lfMessageFont } else { Default::default() };
        if !ok {
            lf.lfHeight = -scale(12, dpi);
            for (i, c) in "Segoe UI".encode_utf16().enumerate() {
                lf.lfFaceName[i] = c;
            }
        }
        if bold {
            lf.lfWeight = 600;
        }
        let plain = windows::Win32::Graphics::Gdi::CreateFontIndirectW(&lf);
        // Try the variable face; keep it only if Windows really used it.
        let mut vf = lf;
        vf.lfFaceName = [0; 32];
        let want = "Segoe UI Variable Text";
        for (i, c) in want.encode_utf16().enumerate() {
            vf.lfFaceName[i] = c;
        }
        let variable = windows::Win32::Graphics::Gdi::CreateFontIndirectW(&vf);
        let dc = GetDC(None);
        let old = SelectObject(dc, HGDIOBJ(variable.0));
        let mut buf = [0u16; 64];
        let n = GetTextFaceW(dc, Some(&mut buf));
        SelectObject(dc, old);
        ReleaseDC(None, dc);
        if n > 0 && from_wide(&buf).eq_ignore_ascii_case(want) {
            let _ = DeleteObject(HGDIOBJ(plain.0));
            variable
        } else {
            let _ = DeleteObject(HGDIOBJ(variable.0));
            plain
        }
    }
}

pub fn delete_font(f: HFONT) {
    if !f.is_invalid() {
        // SAFETY: font created by `create_font`.
        unsafe {
            let _ = DeleteObject(HGDIOBJ(f.0));
        }
    }
}

pub fn set_font(h: HWND, f: HFONT) {
    // SAFETY: valid window; the font outlives the control (owner frees it after destroying controls).
    unsafe {
        SendMessageW(h, WM_SETFONT, WPARAM(f.0 as usize), LPARAM(1));
    }
}

// ---------------------------------------------------------------------------
// Windows and controls
// ---------------------------------------------------------------------------

/// Implemented by the three panel windows.
pub trait Handler: 'static {
    fn attach(&self, hwnd: HWND);
    /// `None` = let `DefWindowProc` handle it.
    fn message(&self, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT>;
}

unsafe extern "system" fn proc_for<T: Handler>(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lp.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        if !cs.lpCreateParams.is_null() {
            (*(cs.lpCreateParams as *const T)).attach(hwnd);
        }
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const T;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wp, lp);
    }
    // SAFETY: the owner (`Rc<T>`) outlives the window; it destroys the window first.
    let this = &*ptr;
    let r = catch_unwind(AssertUnwindSafe(|| this.message(hwnd, msg, wp, lp)));
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
    }
    match r {
        Ok(Some(l)) => l,
        Ok(None) => DefWindowProcW(hwnd, msg, wp, lp),
        Err(_) => {
            tracing::error!("panic caught in a panel window procedure (message {msg:#x})");
            DefWindowProcW(hwnd, msg, wp, lp)
        }
    }
}

/// Register the class once (a second call is fine) and create the window.
/// `obj` must stay alive until the window is destroyed.
pub fn create_window<T: Handler>(
    obj: *const T,
    class: &str,
    title: &str,
    style: u32,
    ex_style: u32,
    (x, y, w, h): (i32, i32, i32, i32),
    parent: Option<HWND>,
) -> Option<HWND> {
    let class_w = wide(class);
    let title_w = wide(title);
    // SAFETY: standard class registration and window creation on the UI thread.
    unsafe {
        let hinst = GetModuleHandleW(None).ok()?;
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc_for::<T>),
            hInstance: hinst.into(),
            lpszClassName: PCWSTR(class_w.as_ptr()),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            // Resource 1 is the icon that build.rs embeds in zest.exe.
            hIcon: LoadIconW(hinst, PCWSTR(std::ptr::without_provenance(1))).unwrap_or_default(),
            ..Default::default()
        };
        // 0 with ERROR_CLASS_ALREADY_EXISTS is fine.
        RegisterClassExW(&wc);
        CreateWindowExW(
            WINDOW_EX_STYLE(ex_style),
            PCWSTR(class_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            WINDOW_STYLE(style),
            x,
            y,
            w,
            h,
            parent.unwrap_or_default(),
            None,
            hinst,
            Some(obj as *const c_void),
        )
        .ok()
    }
}

/// A child control. `class` is a window class name such as "BUTTON".
pub fn create_child(parent: HWND, class: PCWSTR, text: &str, style: u32, ex_style: u32, id: i32) -> HWND {
    let t = wide(text);
    // SAFETY: standard child window creation.
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        CreateWindowExW(
            WINDOW_EX_STYLE(ex_style),
            class,
            PCWSTR(t.as_ptr()),
            WINDOW_STYLE(style | WS_CHILD.0 | WS_VISIBLE.0),
            0,
            0,
            10,
            10,
            parent,
            HMENU(id as isize as *mut c_void),
            hinst,
            None,
        )
        .unwrap_or_default()
    }
}

pub fn move_to(h: HWND, x: i32, y: i32, w: i32, hgt: i32) {
    // SAFETY: valid window handle.
    unsafe {
        let _ = SetWindowPos(h, None, x, y, w, hgt, SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

pub fn client_size(h: HWND) -> (i32, i32) {
    let mut r = RECT::default();
    // SAFETY: out-pointer to a local.
    unsafe {
        let _ = GetClientRect(h, &mut r);
    }
    (r.right - r.left, r.bottom - r.top)
}

pub fn set_text(h: HWND, text: &str) {
    let t = wide(text);
    // SAFETY: NUL-terminated buffer outlives the call.
    unsafe {
        let _ = SetWindowTextW(h, PCWSTR(t.as_ptr()));
    }
}

pub fn get_text(h: HWND) -> String {
    // SAFETY: buffer sized from GetWindowTextLengthW.
    unsafe {
        let n = GetWindowTextLengthW(h);
        if n <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; n as usize + 1];
        let got = GetWindowTextW(h, &mut buf);
        String::from_utf16_lossy(&buf[..got.max(0) as usize])
    }
}

pub fn send(h: HWND, msg: u32, wp: usize, lp: isize) -> isize {
    // SAFETY: valid window handle; the caller passes valid message data.
    unsafe { SendMessageW(h, msg, WPARAM(wp), LPARAM(lp)).0 }
}

pub fn invalidate(h: HWND) {
    // SAFETY: valid window handle.
    unsafe {
        let _ = InvalidateRect(h, None, true);
    }
}

// ----- owner-drawn check boxes and radio buttons ---------------------------

pub const BS_OWNERDRAW: u32 = 0x0B;

pub fn check_get(h: HWND) -> bool {
    // SAFETY: user data of a button control is free for us.
    unsafe { GetWindowLongPtrW(h, GWLP_USERDATA) != 0 }
}

pub fn check_set(h: HWND, on: bool) {
    // SAFETY: as above.
    unsafe {
        SetWindowLongPtrW(h, GWLP_USERDATA, isize::from(on));
    }
    invalidate(h);
}

/// Draw one check box or radio button (`WM_DRAWITEM`).
pub fn draw_check(di: &DRAWITEMSTRUCT, theme: &Theme, dpi: u32, radio: bool, text: &str) {
    let on = check_get(di.hwndItem);
    let disabled = di.itemState.0 & ODS_DISABLED.0 != 0;
    let focused = di.itemState.0 & ODS_FOCUS.0 != 0;
    let r = di.rcItem;
    let box_px = scale(16, dpi);
    let top = r.top + ((r.bottom - r.top) - box_px) / 2;
    // SAFETY: GDI calls on the DC that WM_DRAWITEM gave us; objects are deleted.
    unsafe {
        FillRect(di.hDC, &r, theme.bg_brush);
        let line = if disabled { theme.border } else { theme.muted };
        let pen = CreatePen(PS_SOLID, 1, line);
        let old_pen = SelectObject(di.hDC, HGDIOBJ(pen.0));
        let fill = CreateSolidBrush(if on && !disabled { theme.accent } else { theme.edit_bg });
        let old_brush = SelectObject(di.hDC, HGDIOBJ(fill.0));
        if radio {
            let _ = Ellipse(di.hDC, r.left + 1, top, r.left + 1 + box_px, top + box_px);
        } else {
            let rad = scale(4, dpi);
            let _ = RoundRect(di.hDC, r.left + 1, top, r.left + 1 + box_px, top + box_px, rad, rad);
        }
        SelectObject(di.hDC, old_brush);
        SelectObject(di.hDC, old_pen);
        let _ = DeleteObject(HGDIOBJ(fill.0));
        let _ = DeleteObject(HGDIOBJ(pen.0));
        if on {
            let mark = if radio { theme.edit_bg } else { rgb(255, 255, 255) };
            let mark = if disabled { theme.muted } else { mark };
            if radio {
                let b = CreateSolidBrush(mark);
                let ob = SelectObject(di.hDC, HGDIOBJ(b.0));
                let c = r.left + 1 + box_px / 2;
                let m = top + box_px / 2;
                let k = box_px / 5 + 1;
                let _ = Ellipse(di.hDC, c - k, m - k, c + k, m + k);
                SelectObject(di.hDC, ob);
                let _ = DeleteObject(HGDIOBJ(b.0));
            } else {
                let tick = CreatePen(PS_SOLID, (scale(2, dpi)).max(2), mark);
                let ot = SelectObject(di.hDC, HGDIOBJ(tick.0));
                let x0 = r.left + 1;
                let _ = MoveToEx(di.hDC, x0 + box_px * 22 / 100, top + box_px * 52 / 100, None);
                let _ = LineTo(di.hDC, x0 + box_px * 42 / 100, top + box_px * 72 / 100);
                let _ = LineTo(di.hDC, x0 + box_px * 80 / 100, top + box_px * 30 / 100);
                SelectObject(di.hDC, ot);
                let _ = DeleteObject(HGDIOBJ(tick.0));
            }
        }
        let mut tr = RECT { left: r.left + box_px + scale(8, dpi), top: r.top, right: r.right, bottom: r.bottom };
        SetBkMode(di.hDC, TRANSPARENT);
        SetTextColor(di.hDC, if disabled { theme.muted } else { theme.fg });
        let mut tw: Vec<u16> = text.encode_utf16().collect();
        DrawTextW(di.hDC, &mut tw, &mut tr, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
        if focused {
            let fr = RECT { left: tr.left - 2, top: top - 2, right: tr.right.min(tr.left + scale(8, dpi) + text_extent_hint(text, dpi)), bottom: top + box_px + 2 };
            let _ = windows::Win32::Graphics::Gdi::DrawFocusRect(di.hDC, &fr);
        }
    }
}

/// Rough width of the label for the focus rectangle (no font metrics needed).
fn text_extent_hint(text: &str, dpi: u32) -> i32 {
    scale(7, dpi) * text.chars().count() as i32
}

// ----- list view -------------------------------------------------------------

pub const LVM_SETBKCOLOR: u32 = 0x1001;
pub const LVM_SETIMAGELIST: u32 = 0x1003;
pub const LVM_GETNEXTITEM: u32 = 0x100C;
pub const LVM_ENSUREVISIBLE: u32 = 0x1013;
pub const LVM_REDRAWITEMS: u32 = 0x1015;
pub const LVM_GETHEADER: u32 = 0x101F;
pub const LVM_SETCOLUMNWIDTH: u32 = 0x101E;
pub const LVM_SETTEXTCOLOR: u32 = 0x1024;
pub const LVM_SETTEXTBKCOLOR: u32 = 0x1026;
pub const LVM_SETITEMSTATE: u32 = 0x102B;
pub const LVM_GETITEMSTATE: u32 = 0x102C;
pub const LVM_SETITEMCOUNT: u32 = 0x102F;
pub const LVM_SETEXTENDEDLISTVIEWSTYLE: u32 = 0x1036;
pub const LVM_GETSELECTEDCOUNT: u32 = 0x1032;
pub const LVM_INSERTCOLUMNW: u32 = 0x1061;
pub const LVM_SETCOLUMNW: u32 = 0x1060;
pub const LVM_SETSELECTEDCOLUMN: u32 = 0x108C;
pub const LVNI_SELECTED: isize = 2;
pub const LVIS_SELECTED: u32 = 2;
pub const LVIS_FOCUSED: u32 = 1;
pub const LVS_REPORT: u32 = 1;
pub const LVS_SHOWSELALWAYS: u32 = 8;
pub const LVS_OWNERDATA: u32 = 0x1000;
pub const LVS_NOCOLUMNHEADER: u32 = 0x4000;
pub const LVS_SHAREIMAGELISTS: u32 = 0x40;
pub const LVS_EX_FULLROWSELECT: isize = 0x20;
pub const LVS_EX_DOUBLEBUFFER: isize = 0x10000;
pub const LVS_EX_INFOTIP: isize = 0x400;
pub const LVS_EX_LABELTIP: isize = 0x4000;
pub const LVSIL_SMALL: usize = 1;

/// Set the number of rows of a virtual list view without a full repaint jump.
pub fn lv_set_count(h: HWND, n: usize) {
    // LVSICF_NOSCROLL keeps the scroll position.
    send(h, LVM_SETITEMCOUNT, n, 2);
    invalidate(h);
}

/// Indices of the selected rows, in order.
pub fn lv_selected(h: HWND) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i: isize = -1;
    loop {
        i = send(h, LVM_GETNEXTITEM, i as usize, LVNI_SELECTED);
        if i < 0 {
            break;
        }
        out.push(i as usize);
    }
    out
}

pub fn lv_set_state(h: HWND, index: isize, state: u32, mask: u32) {
    use windows::Win32::UI::Controls::{LVITEMW, LIST_VIEW_ITEM_STATE_FLAGS};
    let item = LVITEMW {
        stateMask: LIST_VIEW_ITEM_STATE_FLAGS(mask),
        state: LIST_VIEW_ITEM_STATE_FLAGS(state),
        ..Default::default()
    };
    send(h, LVM_SETITEMSTATE, index as usize, &item as *const LVITEMW as isize);
}

pub fn lv_select_all(h: HWND) {
    lv_set_state(h, -1, LVIS_SELECTED, LVIS_SELECTED);
}

pub fn lv_select_only(h: HWND, index: usize) {
    lv_set_state(h, -1, 0, LVIS_SELECTED);
    lv_set_state(h, index as isize, LVIS_SELECTED | LVIS_FOCUSED, LVIS_SELECTED | LVIS_FOCUSED);
}

pub fn lv_add_column(h: HWND, index: usize, title: &str, width: i32) {
    use windows::Win32::UI::Controls::{LVCFMT_LEFT, LVCF_FMT, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW};
    let mut t = wide(title);
    let col = LVCOLUMNW {
        mask: LVCF_FMT | LVCF_TEXT | LVCF_WIDTH,
        fmt: LVCFMT_LEFT,
        cx: width,
        pszText: windows::core::PWSTR(t.as_mut_ptr()),
        ..Default::default()
    };
    send(h, LVM_INSERTCOLUMNW, index, &col as *const LVCOLUMNW as isize);
}

pub fn lv_apply_theme(h: HWND, dark: bool, theme: &Theme) {
    theme_control(h, Kind::List, dark);
    send(h, LVM_SETBKCOLOR, 0, theme.edit_bg.0 as isize);
    send(h, LVM_SETTEXTBKCOLOR, 0, theme.edit_bg.0 as isize);
    send(h, LVM_SETTEXTCOLOR, 0, theme.fg.0 as isize);
    let header = HWND(send(h, LVM_GETHEADER, 0, 0) as *mut c_void);
    if !header.is_invalid() {
        theme_control(header, Kind::Header, dark);
    }
    invalidate(h);
}

// ---------------------------------------------------------------------------
// Time zone, clipboard, shell
// ---------------------------------------------------------------------------

/// Minutes to add to UTC to get local time (daylight saving included).
pub fn local_offset_minutes() -> i32 {
    let mut tz = Default::default();
    // SAFETY: out-pointer to a local.
    let id = unsafe { GetTimeZoneInformation(&mut tz) };
    // TIME_ZONE_ID_DAYLIGHT == 2
    let extra = if id == 2 { tz.DaylightBias } else { tz.StandardBias };
    -(tz.Bias + extra)
}

/// Put text on the clipboard (UI thread).
pub fn set_clipboard_text(owner: HWND, text: &str) -> bool {
    let w = wide(text);
    let bytes = w.len() * 2;
    // SAFETY: standard clipboard sequence; the global block is owned by the clipboard after SetClipboardData.
    unsafe {
        if OpenClipboard(owner).is_err() {
            return false;
        }
        let _ = EmptyClipboard();
        let ok = (|| {
            let mem = GlobalAlloc(GMEM_MOVEABLE, bytes).ok()?;
            let ptr = GlobalLock(mem) as *mut u16;
            if ptr.is_null() {
                return None;
            }
            std::ptr::copy_nonoverlapping(w.as_ptr(), ptr, w.len());
            let _ = GlobalUnlock(mem);
            // CF_UNICODETEXT == 13
            SetClipboardData(13, HANDLE(mem.0)).ok()
        })()
        .is_some();
        let _ = CloseClipboard();
        ok
    }
}

/// `ShellExecute` "open" on a background thread (files, folders, URLs).
pub fn shell_open_async(path: PathBuf) {
    spawn("zest-panel-open", move || {
        let w = wide_path(&path);
        // SAFETY: NUL-terminated string that outlives the call.
        let r = unsafe { ShellExecuteW(None, w!("open"), PCWSTR(w.as_ptr()), None, None, SW_SHOWNORMAL) };
        if r.0 as isize <= 32 {
            tracing::warn!("cannot open {}", path.display());
        }
    });
}

/// Explorer with the item selected, on a background thread.
pub fn show_in_folder_async(path: PathBuf) {
    spawn("zest-panel-folder", move || {
        // A deleted file: open its folder instead.
        let target = if path.exists() { path.clone() } else { path.parent().map(Path::to_path_buf).unwrap_or(path.clone()) };
        let r = if target.is_dir() && !path.exists() {
            let w = wide_path(&target);
            // SAFETY: NUL-terminated string that outlives the call.
            let r = unsafe { ShellExecuteW(None, w!("open"), PCWSTR(w.as_ptr()), None, None, SW_SHOWNORMAL) };
            if r.0 as isize > 32 {
                Ok(())
            } else {
                Err(zest_shell::ShellError::Invalid("cannot open the folder".into()))
            }
        } else {
            zest_shell::open_folder_select(&target)
        };
        if let Err(e) = r {
            tracing::warn!("show in folder: {e}");
        }
    });
}

/// Where the next top-level window goes: logical size in the work area of the monitor
/// under the cursor, bottom-right, 16 px from the edges.
pub fn bottom_right_rect(w_px: i32, h_px: i32, margin: i32) -> (i32, i32) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    // SAFETY: out-pointers to locals.
    unsafe {
        let mut p = POINT::default();
        let _ = GetCursorPos(&mut p);
        let mon = MonitorFromPoint(p, MONITOR_DEFAULTTOPRIMARY);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            (mi.rcWork.right - w_px - margin, mi.rcWork.bottom - h_px - margin)
        } else {
            (100, 100)
        }
    }
}

/// Is any part of the rectangle on a monitor?
pub fn rect_on_screen(r: [i32; 4]) -> bool {
    use windows::Win32::Graphics::Gdi::{MonitorFromRect, MONITOR_DEFAULTTONULL};
    let rc = RECT { left: r[0], top: r[1], right: r[0] + r[2], bottom: r[1] + r[3] };
    // SAFETY: pointer to a local RECT.
    unsafe { !MonitorFromRect(&rc, MONITOR_DEFAULTTONULL).is_invalid() }
}

/// Unused handle kinds silence: keep `LRESULT` imported for callers.
pub fn lresult(v: isize) -> LRESULT {
    LRESULT(v)
}

// ----- tooltips ---------------------------------------------------------------

/// A tooltip control for the children of `parent`.
pub fn tooltip_create(parent: HWND) -> HWND {
    use windows::Win32::UI::Controls::TOOLTIPS_CLASSW;
    // WS_POPUP | TTS_NOPREFIX | TTS_ALWAYSTIP
    // SAFETY: standard tooltip window creation.
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            TOOLTIPS_CLASSW,
            PCWSTR::null(),
            WINDOW_STYLE(0x8000_0000 | 0x02 | 0x01),
            0,
            0,
            0,
            0,
            parent,
            None,
            hinst,
            None,
        )
        .unwrap_or_default()
    }
}

pub fn tooltip_add(tt: HWND, parent: HWND, ctl: HWND, text: &str) {
    use windows::Win32::UI::Controls::{TTTOOLINFOW, TTF_IDISHWND, TTF_SUBCLASS, TTM_ADDTOOLW};
    let mut t = wide(text);
    let ti = TTTOOLINFOW {
        cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
        uFlags: TTF_IDISHWND | TTF_SUBCLASS,
        hwnd: parent,
        uId: ctl.0 as usize,
        lpszText: windows::core::PWSTR(t.as_mut_ptr()),
        ..Default::default()
    };
    send(tt, TTM_ADDTOOLW, 0, &ti as *const TTTOOLINFOW as isize);
}

/// Show or hide a window.
pub fn show(h: HWND, visible: bool) {
    use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE, SW_SHOWNA};
    // SAFETY: valid (or null) window handle.
    unsafe {
        let _ = ShowWindow(h, if visible { SW_SHOWNA } else { SW_HIDE });
    }
}

pub fn set_text_enabled(h: HWND, on: bool) {
    // SAFETY: valid window handle.
    unsafe {
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(h, on);
    }
}

// ----- confirm dialog ------------------------------------------------------------

/// A TaskDialog with one action button (`ok_label`) and Cancel. True if the
/// action button was pressed.
pub fn confirm(parent: HWND, title: &str, instruction: &str, content: &str, ok_label: &str) -> bool {
    use windows::Win32::UI::Controls::{
        TaskDialogIndirect, TASKDIALOGCONFIG, TASKDIALOG_BUTTON, TDCBF_CANCEL_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION,
    };
    const OK_ID: i32 = 1001;
    let (t, i, c, o) = (wide(title), wide(instruction), wide(content), wide(ok_label));
    let buttons = [TASKDIALOG_BUTTON { nButtonID: OK_ID, pszButtonText: PCWSTR(o.as_ptr()) }];
    let mut cfg = TASKDIALOGCONFIG { cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32, ..Default::default() };
    cfg.hwndParent = parent;
    cfg.dwFlags = TDF_ALLOW_DIALOG_CANCELLATION;
    cfg.dwCommonButtons = TDCBF_CANCEL_BUTTON;
    cfg.pszWindowTitle = PCWSTR(t.as_ptr());
    cfg.pszMainInstruction = PCWSTR(i.as_ptr());
    cfg.pszContent = PCWSTR(c.as_ptr());
    cfg.cButtons = 1;
    cfg.pButtons = buttons.as_ptr();
    cfg.nDefaultButton = OK_ID;
    let mut pressed = 0i32;
    // SAFETY: every string and the button array outlive the call.
    let r = unsafe { TaskDialogIndirect(&cfg, Some(&mut pressed), None, None) };
    r.is_ok() && pressed == OK_ID
}

pub fn move_to_size(h: HWND, w: i32, hgt: i32) {
    use windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE;
    // SAFETY: valid window handle.
    unsafe {
        let _ = SetWindowPos(h, None, 0, 0, w, hgt, SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOMOVE);
    }
}
