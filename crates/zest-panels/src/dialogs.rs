//! Small modal dialogs: the watch folder editor (used by Settings), the Crop
//! window and the Trim window (used by the app when you pick "Crop..." or
//! "Trim..." in the Tools ring).
//!
//! Each dialog is a plain Win32 window with a nested message loop ([`run_modal`]).
//! The owner is disabled while the dialog is open. The dialogs have a fixed size.

#![allow(clippy::too_many_arguments)]

use crate::model_crop::{self, Aspect, Grab, Sel};
use crate::model_settings::{watch_op_choices, WatchRow};
use crate::win::{self, Handler, Kind, Mailbox, Theme};
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use windows::core::w;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AlphaBlend, BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreatePen, CreateSolidBrush, DeleteDC,
    DeleteObject, EndPaint, FillRect, GetStockObject, Rectangle, SelectObject, SetBkColor, SetBkMode,
    SetBrushOrgEx, SetStretchBltMode, SetTextColor, StretchDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    BLENDFUNCTION, DIB_RGB_COLORS, HALFTONE, HDC, HFONT, HGDIOBJ, NULL_BRUSH, PAINTSTRUCT, PS_SOLID, SRCCOPY,
    TRANSPARENT,
};
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, TRACKBAR_CLASSW};
use windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, ReleaseCapture, SetCapture, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, IsDialogMessageW,
    KillTimer, LoadCursorW, PostQuitMessage, SetCursor, SetForegroundWindow, SetTimer, ShowWindow, TranslateMessage,
    IDC_ARROW, IDC_CROSS, IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, MSG, SW_SHOW, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLORBTN, WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX,
    WM_CTLCOLORSTATIC, WM_DRAWITEM, WM_ERASEBKGND, WM_HSCROLL, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT,
    WM_SETCURSOR, WM_TIMER,
};

const ID_OK: i32 = 1;
const ID_CANCEL: i32 = 2;
const ID_PLAIN: i32 = 99;
const WS_POPUP_CAPTION: u32 = 0x80C8_0000; // WS_POPUP | WS_CAPTION | WS_SYSMENU
const WS_CLIPCHILDREN: u32 = 0x0200_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const WS_VSCROLL: u32 = 0x0020_0000;
const WS_EX_CLIENTEDGE: u32 = 0x200;
const BS_DEFPUSHBUTTON: u32 = 1;
const ES_AUTOHSCROLL: u32 = 0x80;
const SS_NOPREFIX: u32 = 0x80;
const SS_CENTERIMAGE: u32 = 0x200;
const SS_ENDELLIPSIS: u32 = 0x4000;
const CBS_DROPDOWNLIST: u32 = 3;
const CB_ADDSTRING: u32 = 0x143;
const CB_GETCURSEL: u32 = 0x147;
const CB_SETCURSEL: u32 = 0x14E;
const TBS_NOTICKS: u32 = 0x10;
const TBM_GETPOS: u32 = 0x400;
const TBM_SETPOS: u32 = 0x405;
const TBM_SETRANGE: u32 = 0x406;
const EN_CHANGE: u16 = 0x0300;
const CBN_SELCHANGE: u16 = 1;
const BN_CLICKED: u16 = 0;
const DM_GETDEFID: u32 = 0x400;
const DC_HASDEFID: isize = 0x534B;

// ---------------------------------------------------------------------------
// Shared scaffolding
// ---------------------------------------------------------------------------

/// Show `dlg` and run a message loop until `done` is set. The owner is disabled meanwhile.
fn run_modal(dlg: HWND, owner: HWND, done: &Cell<bool>) {
    // SAFETY: handles of windows we own (or the app's window as owner); standard message loop on the UI thread.
    unsafe {
        let has_owner = !owner.is_invalid();
        if has_owner {
            let _ = EnableWindow(owner, false);
        }
        let _ = ShowWindow(dlg, SW_SHOW);
        let _ = SetForegroundWindow(dlg);
        let mut msg = MSG::default();
        while !done.get() {
            let r = GetMessageW(&mut msg, None, 0, 0);
            if r.0 <= 0 {
                if r.0 == 0 {
                    // WM_QUIT belongs to the app's loop: pass it on.
                    PostQuitMessage(msg.wParam.0 as i32);
                }
                break;
            }
            if !IsDialogMessageW(dlg, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if has_owner {
            let _ = EnableWindow(owner, true);
        }
        let _ = DestroyWindow(dlg);
    }
}

/// Theme, fonts and child creation for a fixed-size dialog.
struct Ui {
    hwnd: Cell<HWND>,
    dpi: Cell<u32>,
    font: Cell<HFONT>,
    theme: RefCell<Theme>,
    themed: RefCell<Vec<(HWND, Kind)>>,
    errs: RefCell<Vec<HWND>>,
}

impl Ui {
    fn new() -> Ui {
        Ui {
            hwnd: Cell::new(HWND::default()),
            dpi: Cell::new(96),
            font: Cell::new(Default::default()),
            theme: RefCell::new(Theme::new(win::system_uses_dark())),
            themed: RefCell::new(Vec::new()),
            errs: RefCell::new(Vec::new()),
        }
    }

    fn init(&self, hwnd: HWND) {
        self.hwnd.set(hwnd);
        let dpi = win::window_dpi(hwnd);
        self.dpi.set(dpi);
        self.font.set(win::create_font(dpi, false));
        win::style_frame(hwnd, self.theme.borrow().dark);
    }

    fn s(&self, v: i32) -> i32 {
        win::scale(v, self.dpi.get())
    }

    /// A child control at a logical position.
    fn child(&self, class: windows::core::PCWSTR, text: &str, style: u32, ex: u32, id: i32, r: (i32, i32, i32, i32), kind: Option<Kind>) -> HWND {
        let h = win::create_child(self.hwnd.get(), class, text, style, ex, id);
        win::move_to(h, self.s(r.0), self.s(r.1), self.s(r.2), self.s(r.3));
        win::set_font(h, self.font.get());
        if let Some(k) = kind {
            win::theme_control(h, k, self.theme.borrow().dark);
            self.themed.borrow_mut().push((h, k));
        }
        h
    }

    fn label(&self, text: &str, r: (i32, i32, i32, i32)) -> HWND {
        self.child(w!("STATIC"), text, SS_CENTERIMAGE | SS_NOPREFIX | SS_ENDELLIPSIS, 0, ID_PLAIN, r, None)
    }

    fn error_label(&self, r: (i32, i32, i32, i32)) -> HWND {
        let h = self.child(w!("STATIC"), "", SS_CENTERIMAGE | SS_NOPREFIX | SS_ENDELLIPSIS, 0, ID_PLAIN, r, None);
        self.errs.borrow_mut().push(h);
        h
    }

    fn edit(&self, r: (i32, i32, i32, i32)) -> HWND {
        self.child(w!("EDIT"), "", ES_AUTOHSCROLL | WS_TABSTOP, WS_EX_CLIENTEDGE, ID_PLAIN, r, Some(Kind::Edit))
    }

    fn button(&self, text: &str, id: i32, r: (i32, i32, i32, i32), default: bool) -> HWND {
        let style = WS_TABSTOP | if default { BS_DEFPUSHBUTTON } else { 0 };
        self.child(w!("BUTTON"), text, style, 0, id, r, Some(Kind::Button))
    }

    fn check(&self, text: &str, id: i32, r: (i32, i32, i32, i32)) -> HWND {
        self.child(w!("BUTTON"), text, win::BS_OWNERDRAW | WS_TABSTOP, 0, id, r, None)
    }

    fn combo(&self, items: &[String], r: (i32, i32, i32, i32)) -> HWND {
        let h = self.child(w!("COMBOBOX"), "", CBS_DROPDOWNLIST | WS_VSCROLL | WS_TABSTOP, 0, ID_PLAIN, r, Some(Kind::Combo));
        for it in items {
            let t = win::wide(it);
            win::send(h, CB_ADDSTRING, 0, t.as_ptr() as isize);
        }
        h
    }

    fn color(&self, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        let t = self.theme.borrow();
        let hdc = HDC(wp.0 as *mut c_void);
        let target = HWND(lp.0 as *mut c_void);
        let is_edit = matches!(msg, WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX);
        let is_err = self.errs.borrow().contains(&target);
        // SAFETY: wParam is the HDC of the control.
        unsafe {
            SetTextColor(hdc, if is_err { t.error } else { t.fg });
            SetBkColor(hdc, if is_edit { t.edit_bg } else { t.bg });
        }
        LRESULT(if is_edit { t.edit_brush.0 } else { t.bg_brush.0 } as isize)
    }

    fn draw_check(&self, lp: LPARAM, radio: bool) {
        // SAFETY: WM_DRAWITEM carries a DRAWITEMSTRUCT.
        let di = unsafe { &*(lp.0 as *const DRAWITEMSTRUCT) };
        win::draw_check(di, &self.theme.borrow(), self.dpi.get(), radio, &win::get_text(di.hwndItem));
    }

    /// Size the window so that its client area has this logical size; center it on the cursor's monitor.
    fn size_client(&self, w: i32, h: i32, style: u32) {
        let mut rc = RECT { left: 0, top: 0, right: self.s(w), bottom: self.s(h) };
        // SAFETY: out-pointer to a local.
        unsafe {
            let _ = AdjustWindowRectExForDpi(&mut rc, WINDOW_STYLE(style), false, WINDOW_EX_STYLE(0), self.dpi.get());
        }
        let (ww, wh) = (rc.right - rc.left, rc.bottom - rc.top);
        let (x, y) = center_on_cursor_monitor(ww, wh);
        // SAFETY: valid window handle.
        unsafe {
            use windows::Win32::UI::WindowsAndMessaging::{SetWindowPos, SWP_NOZORDER};
            let _ = SetWindowPos(self.hwnd.get(), None, x, y, ww, wh, SWP_NOZORDER);
        }
    }

    fn fill_background(&self, hdc: HDC) {
        let (w, h) = win::client_size(self.hwnd.get());
        let r = RECT { left: 0, top: 0, right: w, bottom: h };
        // SAFETY: valid DC and brush.
        unsafe {
            FillRect(hdc, &r, self.theme.borrow().bg_brush);
        }
    }

    fn release(&self) {
        win::delete_font(self.font.replace(Default::default()));
    }
}

fn center_on_cursor_monitor(w: i32, h: i32) -> (i32, i32) {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
    let mut p = POINT::default();
    // SAFETY: out-pointers to locals.
    unsafe {
        let _ = GetCursorPos(&mut p);
        let mon = MonitorFromPoint(p, MONITOR_DEFAULTTOPRIMARY);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            let r = mi.rcWork;
            (r.left + (r.right - r.left - w) / 2, r.top + (r.bottom - r.top - h) / 2)
        } else {
            (100, 100)
        }
    }
}

/// A decoded picture ready for `StretchDIBits`: BGRA, top row first, alpha flattened on grey.
struct Picture {
    w: i32,
    h: i32,
    bgra: Vec<u8>,
}

impl Picture {
    fn load(path: &std::path::Path) -> Option<Picture> {
        let (w, h, mut bgra) = zest_shell::load_bgra(path).ok()?;
        for px in bgra.chunks_mut(4).filter(|p| p.len() == 4) {
            let a = u32::from(px[3]);
            if a < 255 {
                for c in &mut px[..3] {
                    *c = ((u32::from(*c) * a + 200 * (255 - a)) / 255) as u8;
                }
            }
        }
        Some(Picture { w: w as i32, h: h as i32, bgra })
    }

    /// Draw the whole picture into `dst` (client pixels of `hdc`).
    fn draw(&self, hdc: HDC, dst: (i32, i32, i32, i32)) {
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: self.w,
                biHeight: -self.h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        // SAFETY: the pixel buffer and header outlive the call.
        unsafe {
            SetStretchBltMode(hdc, HALFTONE);
            let _ = SetBrushOrgEx(hdc, 0, 0, None);
            StretchDIBits(
                hdc,
                dst.0,
                dst.1,
                dst.2,
                dst.3,
                0,
                0,
                self.w,
                self.h,
                Some(self.bgra.as_ptr() as *const c_void),
                &bmi,
                DIB_RGB_COLORS,
                SRCCOPY,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Watch folder editor
// ---------------------------------------------------------------------------

const W_BROWSE: i32 = 11;
const W_RECURSIVE: i32 = 12;
const W_ENABLED: i32 = 13;

struct WatchDlg {
    ui: Ui,
    done: Cell<bool>,
    result: RefCell<Option<WatchRow>>,
    start: WatchRow,
    choices: Vec<(String, String)>,
    folder: Cell<HWND>,
    op: Cell<HWND>,
    include: Cell<HWND>,
    recursive: Cell<HWND>,
    enabled: Cell<HWND>,
    err: Cell<HWND>,
}

/// Add or edit a watch folder. `None` = cancelled.
pub fn watch_dialog(owner: HWND, title: &str, start: WatchRow) -> Option<WatchRow> {
    let mut choices = watch_op_choices();
    let cur = start.op.trim().to_ascii_lowercase();
    if !cur.is_empty() && !choices.iter().any(|(v, _)| *v == cur) {
        choices.push((cur.clone(), cur));
    }
    let dlg = Rc::new(WatchDlg {
        ui: Ui::new(),
        done: Cell::new(false),
        result: RefCell::new(None),
        start,
        choices,
        folder: Cell::new(HWND::default()),
        op: Cell::new(HWND::default()),
        include: Cell::new(HWND::default()),
        recursive: Cell::new(HWND::default()),
        enabled: Cell::new(HWND::default()),
        err: Cell::new(HWND::default()),
    });
    let hwnd = win::create_window(Rc::as_ptr(&dlg), "ZestShell_WatchEdit", title, WS_POPUP_CAPTION | WS_CLIPCHILDREN, 0, (0, 0, 300, 200), Some(owner))?;
    run_modal(hwnd, owner, &dlg.done);
    dlg.ui.release();
    let r = dlg.result.borrow_mut().take();
    r
}

impl WatchDlg {
    fn build(&self, hwnd: HWND) {
        let ui = &self.ui;
        ui.init(hwnd);
        ui.label("Folder", (16, 16, 90, 26));
        self.folder.set(ui.edit((110, 16, 268, 26)));
        ui.button("Browse\u{2026}", W_BROWSE, (386, 15, 80, 28), false);
        ui.label("Operation", (16, 54, 90, 26));
        let labels: Vec<String> = self.choices.iter().map(|(_, l)| l.clone()).collect();
        self.op.set(ui.combo(&labels, (110, 54, 256, 220)));
        ui.label("Only files named", (16, 92, 94, 26));
        self.include.set(ui.edit((110, 92, 356, 26)));
        ui.label("Patterns like *.heic, separated by commas. Empty = every file that the operation can handle.", (110, 120, 356, 34));
        self.recursive.set(ui.check("Include subfolders", W_RECURSIVE, (110, 160, 250, 24)));
        self.enabled.set(ui.check("Active", W_ENABLED, (110, 188, 250, 24)));
        self.err.set(ui.error_label((16, 228, 270, 28)));
        ui.button("OK", ID_OK, (296, 226, 82, 30), true);
        ui.button("Cancel", ID_CANCEL, (384, 226, 82, 30), false);
        ui.size_client(482, 272, WS_POPUP_CAPTION);

        win::set_text(self.folder.get(), &self.start.folder);
        win::set_text(self.include.get(), &self.start.include);
        win::check_set(self.recursive.get(), self.start.recursive);
        win::check_set(self.enabled.get(), self.start.enabled);
        let idx = self.choices.iter().position(|(v, _)| *v == self.start.op.trim().to_ascii_lowercase()).unwrap_or(0);
        win::send(self.op.get(), CB_SETCURSEL, idx, 0);
        // SAFETY: valid control.
        unsafe {
            let _ = SetFocus(self.folder.get());
        }
    }

    fn row(&self) -> WatchRow {
        let sel = win::send(self.op.get(), CB_GETCURSEL, 0, 0).max(0) as usize;
        WatchRow {
            folder: win::get_text(self.folder.get()).trim().to_owned(),
            op: self.choices.get(sel).map(|(v, _)| v.clone()).unwrap_or_default(),
            include: win::get_text(self.include.get()).trim().to_owned(),
            recursive: win::check_get(self.recursive.get()),
            enabled: win::check_get(self.enabled.get()),
        }
    }

    fn ok(&self) {
        let row = self.row();
        match row.validate() {
            Ok(()) => {
                *self.result.borrow_mut() = Some(row);
                self.done.set(true);
            }
            Err(m) => win::set_text(self.err.get(), &m),
        }
    }

    fn browse(&self) {
        let cur = win::get_text(self.folder.get());
        let p = PathBuf::from(cur.trim());
        let start = p.is_absolute().then_some(p);
        if let Ok(Some(f)) = zest_shell::pick_folder("Folder to watch", start.as_deref()) {
            win::set_text(self.folder.get(), &f.to_string_lossy());
        }
    }
}

impl Handler for WatchDlg {
    fn attach(&self, hwnd: HWND) {
        self.ui.hwnd.set(hwnd);
    }

    fn message(&self, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_CREATE => {
                self.build(hwnd);
                Some(LRESULT(0))
            }
            WM_COMMAND => {
                let id = i32::from(win::loword(wp.0));
                match id {
                    ID_OK => self.ok(),
                    ID_CANCEL => self.done.set(true),
                    W_BROWSE => self.browse(),
                    W_RECURSIVE | W_ENABLED => {
                        let c = HWND(lp.0 as *mut c_void);
                        win::check_set(c, !win::check_get(c));
                    }
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_CLOSE => {
                self.done.set(true);
                Some(LRESULT(0))
            }
            WM_DRAWITEM => {
                self.ui.draw_check(lp, false);
                Some(LRESULT(1))
            }
            WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => Some(self.ui.color(msg, wp, lp)),
            WM_ERASEBKGND => {
                self.ui.fill_background(HDC(wp.0 as *mut c_void));
                Some(LRESULT(1))
            }
            DM_GETDEFID => Some(LRESULT((DC_HASDEFID << 16) | ID_OK as isize)),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Crop
// ---------------------------------------------------------------------------

/// What the Crop window shows.
pub struct CropInput {
    /// File name for the title.
    pub title: String,
    /// A PNG of the whole image (already turned by its EXIF orientation), at most about 1600 px.
    pub preview: PathBuf,
    /// Size of the full image in pixels (turned).
    pub width: u32,
    pub height: u32,
}

const AREA_W: i32 = 800;
const AREA_H: i32 = 520;
const HANDLE_PX: i32 = 8;

struct Drag {
    grab: Grab,
    /// Start point in image pixels.
    from: (f64, f64),
    start: Option<Sel>,
}

struct CropDlg {
    ui: Ui,
    done: Cell<bool>,
    result: Cell<Option<(u32, u32, u32, u32)>>,
    input: CropInput,
    pic: RefCell<Option<Picture>>,
    /// Picture rectangle in client pixels.
    view: Cell<(i32, i32, i32, i32)>,
    sel: Cell<Option<Sel>>,
    drag: RefCell<Option<Drag>>,
    hover: Cell<Grab>,
    aspect: Cell<Aspect>,
    combo: Cell<HWND>,
    size_label: Cell<HWND>,
    ok: Cell<HWND>,
}

/// Choose the area to keep. Returns `(x, y, w, h)` in pixels of the full image, or `None`.
pub fn crop_dialog(owner: HWND, input: CropInput) -> Option<(u32, u32, u32, u32)> {
    let title = format!("Crop \u{2013} {}", input.title);
    let dlg = Rc::new(CropDlg {
        ui: Ui::new(),
        done: Cell::new(false),
        result: Cell::new(None),
        input,
        pic: RefCell::new(None),
        view: Cell::new((0, 0, 0, 0)),
        sel: Cell::new(None),
        drag: RefCell::new(None),
        hover: Cell::new(Grab::New),
        aspect: Cell::new(Aspect::Free),
        combo: Cell::new(HWND::default()),
        size_label: Cell::new(HWND::default()),
        ok: Cell::new(HWND::default()),
    });
    let hwnd = win::create_window(Rc::as_ptr(&dlg), "ZestShell_Crop", &title, WS_POPUP_CAPTION | WS_CLIPCHILDREN, 0, (0, 0, 300, 200), Some(owner))?;
    run_modal(hwnd, owner, &dlg.done);
    dlg.ui.release();
    dlg.result.get()
}

impl CropDlg {
    fn build(&self, hwnd: HWND) {
        let ui = &self.ui;
        ui.init(hwnd);
        let by = AREA_H + 24;
        ui.label("Shape", (16, by, 50, 28));
        let labels: Vec<String> = Aspect::ALL.iter().map(|a| a.label().to_owned()).collect();
        self.combo.set(ui.combo(&labels, (70, by, 110, 200)));
        win::send(self.combo.get(), CB_SETCURSEL, 0, 0);
        self.size_label.set(ui.label("Drag on the picture to choose the area.", (200, by, 380, 28)));
        self.ok.set(ui.button("OK", ID_OK, (AREA_W - 180 + 16, by - 1, 82, 30), true));
        ui.button("Cancel", ID_CANCEL, (AREA_W - 90 + 16, by - 1, 82, 30), false);
        win::set_text_enabled(self.ok.get(), false);
        ui.size_client(AREA_W + 32, AREA_H + 66, WS_POPUP_CAPTION);

        // The picture is scaled to fit the area (never enlarged more than 2x).
        let pic = Picture::load(&self.input.preview);
        if let Some(p) = &pic {
            let (aw, ah) = (ui.s(AREA_W), ui.s(AREA_H));
            let k = (f64::from(aw) / f64::from(p.w)).min(f64::from(ah) / f64::from(p.h)).min(2.0 * f64::from(ui.dpi.get()) / 96.0);
            let (dw, dh) = ((f64::from(p.w) * k).round() as i32, (f64::from(p.h) * k).round() as i32);
            self.view.set((ui.s(16) + (aw - dw) / 2, ui.s(12) + (ah - dh) / 2, dw.max(1), dh.max(1)));
        } else {
            win::set_text(self.size_label.get(), "The picture cannot be shown.");
        }
        *self.pic.borrow_mut() = pic;
    }

    fn bounds(&self) -> (f64, f64) {
        (f64::from(self.input.width), f64::from(self.input.height))
    }

    /// Screen pixels per image pixel.
    fn zoom(&self) -> f64 {
        let v = self.view.get();
        f64::from(v.2) / f64::from(self.input.width.max(1))
    }

    fn to_image(&self, x: i32, y: i32) -> (f64, f64) {
        let v = self.view.get();
        let z = self.zoom().max(f64::EPSILON);
        (f64::from(x - v.0) / z, f64::from(y - v.1) / z)
    }

    fn to_client(&self, s: Sel) -> RECT {
        let v = self.view.get();
        let z = self.zoom();
        RECT {
            left: v.0 + (s.x * z).round() as i32,
            top: v.1 + (s.y * z).round() as i32,
            right: v.0 + (s.right() * z).round() as i32,
            bottom: v.1 + (s.bottom() * z).round() as i32,
        }
    }

    fn tol(&self) -> f64 {
        f64::from(self.ui.s(HANDLE_PX)) / self.zoom().max(f64::EPSILON)
    }

    fn set_sel(&self, s: Option<Sel>) {
        self.sel.set(s);
        let ok = s.is_some();
        win::set_text_enabled(self.ok.get(), ok);
        let text = match s {
            Some(s) => {
                let (x, y, w, h) = model_crop::to_pixels(s, (self.input.width, self.input.height));
                format!("{w} \u{D7} {h} px     from {x}, {y}     (image {} \u{D7} {})", self.input.width, self.input.height)
            }
            None => "Drag on the picture to choose the area.".to_owned(),
        };
        win::set_text(self.size_label.get(), &text);
        win::invalidate(self.ui.hwnd.get());
    }

    fn mouse_down(&self, x: i32, y: i32) {
        if self.pic.borrow().is_none() {
            return;
        }
        let (ix, iy) = self.to_image(x, y);
        let v = self.view.get();
        if x < v.0 || y < v.1 || x > v.0 + v.2 || y > v.1 + v.3 {
            return;
        }
        let grab = model_crop::grab_at(self.sel.get(), ix, iy, self.tol());
        let (bx, by) = self.bounds();
        *self.drag.borrow_mut() = Some(Drag { grab, from: (ix.clamp(0.0, bx), iy.clamp(0.0, by)), start: self.sel.get() });
        // SAFETY: our own window.
        unsafe {
            SetCapture(self.ui.hwnd.get());
        }
    }

    fn mouse_move(&self, x: i32, y: i32) {
        let (ix, iy) = self.to_image(x, y);
        let drag = self.drag.borrow();
        let Some(d) = drag.as_ref() else {
            self.hover.set(model_crop::grab_at(self.sel.get(), ix, iy, self.tol()));
            return;
        };
        let b = self.bounds();
        let ratio = self.aspect.get().ratio();
        let next = match (d.grab, d.start) {
            (Grab::New, _) | (_, None) => model_crop::from_corner(d.from, (ix, iy), b, ratio),
            (g, Some(start)) => model_crop::resized(start, g, ix - d.from.0, iy - d.from.1, b, ratio),
        };
        drop(drag);
        self.set_sel(Some(next));
    }

    fn mouse_up(&self) {
        if self.drag.borrow_mut().take().is_none() {
            return;
        }
        // SAFETY: releases the capture that mouse_down set.
        unsafe {
            let _ = ReleaseCapture();
        }
        // A click without a drag leaves no selection.
        if let Some(s) = self.sel.get() {
            if s.w * self.zoom() < 3.0 || s.h * self.zoom() < 3.0 {
                self.set_sel(None);
            }
        }
    }

    fn cursor(&self) {
        let id = match self.hover.get() {
            Grab::New => IDC_CROSS,
            Grab::Move => IDC_SIZEALL,
            Grab::N | Grab::S => IDC_SIZENS,
            Grab::E | Grab::W => IDC_SIZEWE,
            Grab::NW | Grab::SE => IDC_SIZENWSE,
            Grab::NE | Grab::SW => IDC_SIZENESW,
        };
        // SAFETY: stock cursor.
        unsafe {
            if let Ok(c) = LoadCursorW(None, id) {
                SetCursor(c);
            }
        }
    }

    fn aspect_changed(&self) {
        let i = win::send(self.combo.get(), CB_GETCURSEL, 0, 0).max(0) as usize;
        let a = Aspect::ALL[i.min(Aspect::ALL.len() - 1)];
        self.aspect.set(a);
        if let (Some(r), Some(s)) = (a.ratio(), self.sel.get()) {
            self.set_sel(Some(model_crop::fit_aspect(s, r, self.bounds())));
        }
    }

    fn ok(&self) {
        if let Some(s) = self.sel.get() {
            self.result.set(Some(model_crop::to_pixels(s, (self.input.width, self.input.height))));
            self.done.set(true);
        }
    }

    fn paint(&self, hwnd: HWND) {
        let t = self.ui.theme.borrow();
        // SAFETY: standard double-buffered paint; every GDI object is released.
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let (w, h) = win::client_size(hwnd);
            let mem = CreateCompatibleDC(hdc);
            let bmp = CreateCompatibleBitmap(hdc, w.max(1), h.max(1));
            let old_bmp = SelectObject(mem, HGDIOBJ(bmp.0));
            FillRect(mem, &RECT { left: 0, top: 0, right: w, bottom: h }, t.bg_brush);
            if let Some(p) = self.pic.borrow().as_ref() {
                let v = self.view.get();
                p.draw(mem, v);
                if let Some(s) = self.sel.get() {
                    let r = self.to_client(s);
                    // Dim the outside of the selection.
                    let dim_dc = CreateCompatibleDC(hdc);
                    let dim_bmp = CreateCompatibleBitmap(hdc, 1, 1);
                    let old_dim = SelectObject(dim_dc, HGDIOBJ(dim_bmp.0));
                    let black = CreateSolidBrush(COLORREF(0));
                    FillRect(dim_dc, &RECT { left: 0, top: 0, right: 1, bottom: 1 }, black);
                    let _ = DeleteObject(HGDIOBJ(black.0));
                    let blend = BLENDFUNCTION { BlendOp: 0, BlendFlags: 0, SourceConstantAlpha: 120, AlphaFormat: 0 };
                    let strips = [
                        (v.0, v.1, v.2, r.top - v.1),
                        (v.0, r.bottom, v.2, v.1 + v.3 - r.bottom),
                        (v.0, r.top, r.left - v.0, r.bottom - r.top),
                        (r.right, r.top, v.0 + v.2 - r.right, r.bottom - r.top),
                    ];
                    for (sx, sy, sw, sh) in strips {
                        if sw > 0 && sh > 0 {
                            let _ = AlphaBlend(mem, sx, sy, sw, sh, dim_dc, 0, 0, 1, 1, blend);
                        }
                    }
                    SelectObject(dim_dc, old_dim);
                    let _ = DeleteObject(HGDIOBJ(dim_bmp.0));
                    let _ = DeleteDC(dim_dc);

                    // Border: white outside, black inside.
                    let white = CreatePen(PS_SOLID, 1, COLORREF(0x00FF_FFFF));
                    let dark = CreatePen(PS_SOLID, 1, COLORREF(0));
                    let old_brush = SelectObject(mem, GetStockObject(NULL_BRUSH));
                    let old_pen = SelectObject(mem, HGDIOBJ(white.0));
                    let _ = Rectangle(mem, r.left, r.top, r.right + 1, r.bottom + 1);
                    SelectObject(mem, HGDIOBJ(dark.0));
                    let _ = Rectangle(mem, r.left + 1, r.top + 1, r.right, r.bottom);
                    // Handles.
                    let fill = CreateSolidBrush(COLORREF(0x00FF_FFFF));
                    SelectObject(mem, HGDIOBJ(fill.0));
                    let hs = self.ui.s(HANDLE_PX) / 2;
                    let (mx, my) = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
                    for (hx, hy) in [
                        (r.left, r.top),
                        (mx, r.top),
                        (r.right, r.top),
                        (r.left, my),
                        (r.right, my),
                        (r.left, r.bottom),
                        (mx, r.bottom),
                        (r.right, r.bottom),
                    ] {
                        let _ = Rectangle(mem, hx - hs, hy - hs, hx + hs + 1, hy + hs + 1);
                    }
                    SelectObject(mem, old_pen);
                    SelectObject(mem, old_brush);
                    let _ = DeleteObject(HGDIOBJ(fill.0));
                    let _ = DeleteObject(HGDIOBJ(white.0));
                    let _ = DeleteObject(HGDIOBJ(dark.0));
                }
            }
            let _ = BitBlt(hdc, 0, 0, w, h, mem, 0, 0, SRCCOPY);
            SelectObject(mem, old_bmp);
            let _ = DeleteObject(HGDIOBJ(bmp.0));
            let _ = DeleteDC(mem);
            let _ = EndPaint(hwnd, &ps);
        }
    }
}

fn xy(lp: LPARAM) -> (i32, i32) {
    (i32::from((lp.0 & 0xFFFF) as i16), i32::from(((lp.0 >> 16) & 0xFFFF) as i16))
}

impl Handler for CropDlg {
    fn attach(&self, hwnd: HWND) {
        self.ui.hwnd.set(hwnd);
    }

    fn message(&self, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_CREATE => {
                self.build(hwnd);
                Some(LRESULT(0))
            }
            WM_COMMAND => {
                let id = i32::from(win::loword(wp.0));
                let code = win::hiword(wp.0);
                match id {
                    ID_OK => self.ok(),
                    ID_CANCEL => self.done.set(true),
                    _ if HWND(lp.0 as *mut c_void) == self.combo.get() && code == CBN_SELCHANGE => self.aspect_changed(),
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN => {
                let (x, y) = xy(lp);
                self.mouse_down(x, y);
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                let (x, y) = xy(lp);
                self.mouse_move(x, y);
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                self.mouse_up();
                Some(LRESULT(0))
            }
            WM_SETCURSOR => {
                // Only over the picture; elsewhere the default cursor.
                let mut p = POINT::default();
                // SAFETY: out-pointer to a local.
                unsafe {
                    let _ = GetCursorPos(&mut p);
                    let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut p);
                }
                let v = self.view.get();
                if self.pic.borrow().is_some() && p.x >= v.0 && p.y >= v.1 && p.x <= v.0 + v.2 && p.y <= v.1 + v.3 {
                    if self.drag.borrow().is_none() {
                        let (ix, iy) = self.to_image(p.x, p.y);
                        self.hover.set(model_crop::grab_at(self.sel.get(), ix, iy, self.tol()));
                    }
                    self.cursor();
                    return Some(LRESULT(1));
                }
                // SAFETY: stock cursor.
                unsafe {
                    if let Ok(c) = LoadCursorW(None, IDC_ARROW) {
                        SetCursor(c);
                    }
                }
                Some(LRESULT(1))
            }
            WM_PAINT => {
                self.paint(hwnd);
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_CLOSE => {
                self.done.set(true);
                Some(LRESULT(0))
            }
            WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => Some(self.ui.color(msg, wp, lp)),
            DM_GETDEFID => Some(LRESULT((DC_HASDEFID << 16) | ID_OK as isize)),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Trim
// ---------------------------------------------------------------------------

/// Makes a PNG of the frame at this time (milliseconds), or `None`. Runs on a worker thread.
pub type FrameFn = Arc<dyn Fn(u64) -> Option<PathBuf> + Send + Sync>;

/// What the Trim window shows.
pub struct TrimInput {
    pub title: String,
    /// Length of the file, if known.
    pub duration_ms: Option<u64>,
    /// Frames for a video; `None` for audio (no pictures).
    pub frame: Option<FrameFn>,
}

const T_START_SLIDER: i32 = 32;
const T_END_SLIDER: i32 = 33;
const TIMER_FRAMES: usize = 7;
const SLIDER_MAX: u32 = 1000;
const THUMB_W: i32 = 228;
const THUMB_H: i32 = 128;

enum TrimMsg {
    Frame(usize, Option<PathBuf>),
}

struct TrimDlg {
    ui: Ui,
    done: Cell<bool>,
    result: Cell<Option<(u64, u64)>>,
    input: TrimInput,
    syncing: Cell<bool>,
    start_edit: Cell<HWND>,
    end_edit: Cell<HWND>,
    start_slider: Cell<HWND>,
    end_slider: Cell<HWND>,
    info: Cell<HWND>,
    err: Cell<HWND>,
    ok: Cell<HWND>,
    mail: Mailbox<TrimMsg>,
    /// Frame pictures for start and end.
    frames: RefCell<[Option<Picture>; 2]>,
    /// A frame is being made / another one is wanted when it arrives.
    busy: Cell<[bool; 2]>,
    stale: Cell<[bool; 2]>,
    /// The time shown in each frame.
    shown: Cell<[Option<u64>; 2]>,
}

/// Choose the part to keep. Returns `(start_ms, end_ms)`; `end_ms == 0` = to the end.
pub fn trim_dialog(owner: HWND, input: TrimInput) -> Option<(u64, u64)> {
    let title = format!("Trim \u{2013} {}", input.title);
    let dlg = Rc::new(TrimDlg {
        ui: Ui::new(),
        done: Cell::new(false),
        result: Cell::new(None),
        input,
        syncing: Cell::new(false),
        start_edit: Cell::new(HWND::default()),
        end_edit: Cell::new(HWND::default()),
        start_slider: Cell::new(HWND::default()),
        end_slider: Cell::new(HWND::default()),
        info: Cell::new(HWND::default()),
        err: Cell::new(HWND::default()),
        ok: Cell::new(HWND::default()),
        mail: Mailbox::default(),
        frames: RefCell::new([None, None]),
        busy: Cell::new([false; 2]),
        stale: Cell::new([false; 2]),
        shown: Cell::new([None; 2]),
    });
    let hwnd = win::create_window(Rc::as_ptr(&dlg), "ZestShell_Trim", &title, WS_POPUP_CAPTION | WS_CLIPCHILDREN, 0, (0, 0, 300, 200), Some(owner))?;
    dlg.mail.set_hwnd(hwnd);
    run_modal(hwnd, owner, &dlg.done);
    dlg.ui.release();
    dlg.result.get()
}

impl TrimDlg {
    fn has_frames(&self) -> bool {
        self.input.frame.is_some()
    }

    fn build(&self, hwnd: HWND) {
        let ui = &self.ui;
        ui.init(hwnd);
        let dur = self.input.duration_ms;
        let len = dur.map_or("unknown".to_owned(), zest_core::format_time_ms);
        self.info.set(ui.label(&format!("Length: {len}"), (16, 12, 480, 24)));

        ui.label("Start", (16, 46, 56, 28));
        self.start_edit.set(ui.edit((76, 46, 110, 28)));
        self.start_slider.set(ui.child(TRACKBAR_CLASSW, "", TBS_NOTICKS | WS_TABSTOP, 0, T_START_SLIDER, (196, 46, 300, 28), None));
        ui.label("End", (16, 82, 56, 28));
        self.end_edit.set(ui.edit((76, 82, 110, 28)));
        self.end_slider.set(ui.child(TRACKBAR_CLASSW, "", TBS_NOTICKS | WS_TABSTOP, 0, T_END_SLIDER, (196, 82, 300, 28), None));
        for s in [self.start_slider.get(), self.end_slider.get()] {
            win::send(s, TBM_SETRANGE, 1, (SLIDER_MAX << 16) as isize);
        }
        let mut y = 120;
        if self.has_frames() {
            y += THUMB_H + 34;
        }
        self.err.set(ui.error_label((16, y, 480, 24)));
        ui.label("Times look like 01:23.500 (minutes:seconds). The part is cut exactly, so it is encoded again.", (16, y + 26, 480, 24));
        self.ok.set(ui.button("OK", ID_OK, (322, y + 62, 82, 30), true));
        ui.button("Cancel", ID_CANCEL, (414, y + 62, 82, 30), false);
        ui.size_client(512, y + 104, WS_POPUP_CAPTION);

        self.syncing.set(true);
        win::set_text(self.start_edit.get(), &zest_core::format_time_ms(0));
        match dur {
            Some(d) => {
                win::set_text(self.end_edit.get(), &zest_core::format_time_ms(d));
                win::send(self.end_slider.get(), TBM_SETPOS, 1, SLIDER_MAX as isize);
            }
            None => {
                win::set_text(self.end_edit.get(), "");
                win::set_text_enabled(self.start_slider.get(), false);
                win::set_text_enabled(self.end_slider.get(), false);
            }
        }
        self.syncing.set(false);
        self.refresh_state();
        if self.has_frames() {
            self.request_frames(true, true);
        }
    }

    fn start_ms(&self) -> Option<u64> {
        zest_core::parse_time_ms(&win::get_text(self.start_edit.get()))
    }

    /// `Some(None)` = empty (to the end), `Some(Some(ms))` = a time, `None` = not a time.
    fn end_ms(&self) -> Option<Option<u64>> {
        let t = win::get_text(self.end_edit.get());
        if t.trim().is_empty() {
            return Some(None);
        }
        zest_core::parse_time_ms(&t).map(Some)
    }

    /// Check the fields, update the error text, the OK button and the sliders.
    fn refresh_state(&self) -> Option<(u64, u64)> {
        let dur = self.input.duration_ms;
        let start = self.start_ms();
        let end = self.end_ms();
        let mut problem: Option<&str> = None;
        let mut range = None;
        match (start, end) {
            (None, _) => problem = Some("The start is not a time."),
            (_, None) => problem = Some("The end is not a time."),
            (Some(s), Some(e)) => {
                if dur.is_some_and(|d| s >= d) {
                    problem = Some("The start is after the end of the file.");
                } else if e.is_some_and(|e| e <= s) {
                    problem = Some("The end must be after the start.");
                } else {
                    let e = e.filter(|e| dur.is_none_or(|d| *e < d)).unwrap_or(0);
                    range = Some((s, e));
                }
            }
        }
        win::set_text(self.err.get(), problem.unwrap_or(""));
        win::set_text_enabled(self.ok.get(), range.is_some());
        if let (Some(d), false) = (dur, self.syncing.get()) {
            let pos = |ms: u64| ((ms.min(d) as u128 * u128::from(SLIDER_MAX)) / u128::from(d.max(1))) as isize;
            if let Some(s) = start {
                win::send(self.start_slider.get(), TBM_SETPOS, 1, pos(s));
            }
            if let Some(e) = end {
                win::send(self.end_slider.get(), TBM_SETPOS, 1, e.map_or(SLIDER_MAX as isize, pos));
            }
        }
        range
    }

    fn slider_moved(&self, slider: HWND) {
        let Some(d) = self.input.duration_ms else { return };
        let pos = win::send(slider, TBM_GETPOS, 0, 0).max(0) as u128;
        let ms = (pos * u128::from(d) / u128::from(SLIDER_MAX)) as u64;
        let edit = if slider == self.start_slider.get() { self.start_edit.get() } else { self.end_edit.get() };
        self.syncing.set(true);
        win::set_text(edit, &zest_core::format_time_ms(ms));
        self.syncing.set(false);
        self.refresh_state();
        self.schedule_frames();
    }

    fn schedule_frames(&self) {
        if self.has_frames() {
            // SAFETY: timer on our own window.
            unsafe {
                SetTimer(self.ui.hwnd.get(), TIMER_FRAMES, 400, None);
            }
        }
    }

    /// The times for the two frames (end frame: a little before the end).
    fn frame_times(&self) -> [Option<u64>; 2] {
        let dur = self.input.duration_ms;
        let start = self.start_ms();
        let end = match self.end_ms() {
            Some(Some(e)) => Some(e),
            Some(None) => dur,
            None => None,
        };
        let last = dur.map(|d| d.saturating_sub(100));
        [start.map(|s| last.map_or(s, |l| s.min(l))), end.map(|e| last.map_or(e, |l| e.min(l)))]
    }

    fn request_frames(&self, a: bool, b: bool) {
        let Some(f) = self.input.frame.clone() else { return };
        let times = self.frame_times();
        for (i, want) in [a, b].into_iter().enumerate() {
            let Some(t) = times[i] else { continue };
            if !want || self.shown.get()[i] == Some(t) {
                continue;
            }
            let mut busy = self.busy.get();
            if busy[i] {
                let mut st = self.stale.get();
                st[i] = true;
                self.stale.set(st);
                continue;
            }
            busy[i] = true;
            self.busy.set(busy);
            let mut sh = self.shown.get();
            sh[i] = Some(t);
            self.shown.set(sh);
            let (mail, f) = (self.mail.clone(), Arc::clone(&f));
            win::spawn("zest-trim-frame", move || {
                mail.post(TrimMsg::Frame(i, f(t)));
            });
        }
    }

    fn on_mail(&self) {
        for m in self.mail.drain() {
            let TrimMsg::Frame(i, path) = m;
            let mut busy = self.busy.get();
            busy[i] = false;
            self.busy.set(busy);
            if let Some(p) = path {
                let pic = Picture::load(&p);
                let _ = std::fs::remove_file(&p);
                self.frames.borrow_mut()[i] = pic;
                win::invalidate(self.ui.hwnd.get());
            }
            let mut st = self.stale.get();
            if st[i] {
                st[i] = false;
                self.stale.set(st);
                let mut want = [false; 2];
                want[i] = true;
                self.request_frames(want[0], want[1]);
            }
        }
    }

    fn thumb_rect(&self, i: usize) -> (i32, i32, i32, i32) {
        let x = 16 + i as i32 * (THUMB_W + 24);
        (self.ui.s(x), self.ui.s(122 + 22), self.ui.s(THUMB_W), self.ui.s(THUMB_H))
    }

    fn paint(&self, hwnd: HWND) {
        let t = self.ui.theme.borrow();
        // SAFETY: standard paint; every GDI object is released.
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let (w, h) = win::client_size(hwnd);
            FillRect(hdc, &RECT { left: 0, top: 0, right: w, bottom: h }, t.bg_brush);
            if self.has_frames() {
                SetBkMode(hdc, TRANSPARENT);
                SetTextColor(hdc, t.muted);
                let old_font = SelectObject(hdc, HGDIOBJ(self.ui.font.get().0));
                for (i, name) in ["Start frame", "End frame"].into_iter().enumerate() {
                    let r = self.thumb_rect(i);
                    let rect = RECT { left: r.0, top: r.1, right: r.0 + r.2, bottom: r.1 + r.3 };
                    let back = CreateSolidBrush(COLORREF(0x0018_1818));
                    FillRect(hdc, &rect, back);
                    let _ = DeleteObject(HGDIOBJ(back.0));
                    if let Some(p) = self.frames.borrow()[i].as_ref() {
                        // Fit inside the box, keep the shape.
                        let k = (f64::from(r.2) / f64::from(p.w)).min(f64::from(r.3) / f64::from(p.h));
                        let (dw, dh) = ((f64::from(p.w) * k).round() as i32, (f64::from(p.h) * k).round() as i32);
                        p.draw(hdc, (r.0 + (r.2 - dw) / 2, r.1 + (r.3 - dh) / 2, dw.max(1), dh.max(1)));
                    }
                    let label: Vec<u16> = name.encode_utf16().collect();
                    let _ = windows::Win32::Graphics::Gdi::TextOutW(hdc, r.0, r.1 - self.ui.s(20), &label);
                }
                SelectObject(hdc, old_font);
            }
            let _ = EndPaint(hwnd, &ps);
        }
    }
}

impl Handler for TrimDlg {
    fn attach(&self, hwnd: HWND) {
        self.ui.hwnd.set(hwnd);
    }

    fn message(&self, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_CREATE => {
                self.build(hwnd);
                Some(LRESULT(0))
            }
            WM_COMMAND => {
                let id = i32::from(win::loword(wp.0));
                let code = win::hiword(wp.0);
                match id {
                    ID_OK if code == BN_CLICKED => {
                        if let Some(r) = self.refresh_state() {
                            self.result.set(Some(r));
                            self.done.set(true);
                        }
                    }
                    ID_CANCEL => self.done.set(true),
                    _ if code == EN_CHANGE && !self.syncing.get() => {
                        let c = HWND(lp.0 as *mut c_void);
                        if c == self.start_edit.get() || c == self.end_edit.get() {
                            self.refresh_state();
                            self.schedule_frames();
                        }
                    }
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_HSCROLL => {
                self.slider_moved(HWND(lp.0 as *mut c_void));
                Some(LRESULT(0))
            }
            WM_TIMER if wp.0 == TIMER_FRAMES => {
                // SAFETY: timer on our own window.
                unsafe {
                    let _ = KillTimer(hwnd, TIMER_FRAMES);
                }
                self.request_frames(true, true);
                Some(LRESULT(0))
            }
            win::WM_MAILBOX => {
                self.on_mail();
                Some(LRESULT(0))
            }
            WM_PAINT => {
                self.paint(hwnd);
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_CLOSE => {
                self.done.set(true);
                Some(LRESULT(0))
            }
            WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => Some(self.ui.color(msg, wp, lp)),
            DM_GETDEFID => Some(LRESULT((DC_HASDEFID << 16) | ID_OK as isize)),
            _ => None,
        }
    }
}
