//! The Settings window: a vertical form with sections. It edits config.toml
//! with `toml_edit` (comments and unknown keys stay), writes it atomically,
//! and the config watcher of the app applies it.

use crate::dialogs::watch_dialog;
use crate::model_settings::{
    edit_config_text, format_chord, key_name_from_vk, Field, FieldError, ModChoice, SettingsForm, WatchRow,
    IMAGE_WORKERS_MAX, RADIUS_MAX, RADIUS_MIN, VIDEO_WORKERS_MAX,
};
use crate::win::{self, Handler, Kind, Mailbox, Theme};
use crate::{EngineLine, PanelHost, SettingsInput};
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, EndPaint, FillRect,
    GetStockObject, RoundRect, SelectObject, SetBkColor, SetBkMode, SetTextColor, TextOutW, CreatePen, HDC,
    HGDIOBJ, NULL_BRUSH, PAINTSTRUCT, PS_SOLID, SRCCOPY, TRANSPARENT,
};
use windows::Win32::UI::Controls::{SetScrollInfo, DRAWITEMSTRUCT, TRACKBAR_CLASSW};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, SetFocus};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyWindow, GetScrollInfo, IsWindowVisible, SetForegroundWindow, ShowWindow, MINMAXINFO,
    SCROLLINFO, SIF_ALL, SIF_PAGE, SIF_POS, SIF_RANGE, SIF_TRACKPOS, SW_HIDE, SW_RESTORE, SW_SHOW, WM_CLOSE,
    WM_COMMAND, WM_CREATE, WM_CTLCOLORBTN, WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DESTROY,
    WM_DPICHANGED, WM_DRAWITEM, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_HSCROLL, WM_MOUSEWHEEL, WM_PAINT,
    WM_SETTINGCHANGE, WM_SIZE, WM_VSCROLL,
};

// ----- ids -----------------------------------------------------------------------
const ID_SAVE: i32 = 1; // IDOK
const ID_CANCEL: i32 = 2; // IDCANCEL
const ID_RESET: i32 = 3;
const ID_DRAG: i32 = 10;
const ID_HOLD: i32 = 11;
const ID_CAPTURE: i32 = 12;
const ID_PAUSED: i32 = 13;
const ID_QUIET: i32 = 28;
const ID_RADIO_NEXT: i32 = 20;
const ID_RADIO_FOLDER: i32 = 21;
const ID_BROWSE: i32 = 22;
const ID_SHELF: i32 = 23;
const ID_PRIVACY: i32 = 24;
const ID_STAMPS: i32 = 25;
const ID_AUTOSTART: i32 = 30;
const ID_CONTEXT: i32 = 31;
const ID_TOOLS: i32 = 40;
const ID_PANDOC: i32 = 41;
const ID_LIBRE: i32 = 42;
const ID_WATCH_LIST: i32 = 50;
const ID_WATCH_ADD: i32 = 51;
const ID_WATCH_EDIT: i32 = 52;
const ID_WATCH_REMOVE: i32 = 53;
const ID_PLAIN: i32 = 99;

// ----- raw constants -----------------------------------------------------------------
const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
const WS_THICKFRAME: u32 = 0x0004_0000;
const WS_MAXIMIZEBOX: u32 = 0x0001_0000;
const WS_VSCROLL: u32 = 0x0020_0000;
const WS_CLIPCHILDREN: u32 = 0x0200_0000;
const WS_CLIPSIBLINGS: u32 = 0x0400_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const WS_EX_CLIENTEDGE: u32 = 0x200;
const BS_DEFPUSHBUTTON: u32 = 1;
const ES_AUTOHSCROLL: u32 = 0x80;
const SS_CENTERIMAGE: u32 = 0x200;
const SS_NOPREFIX: u32 = 0x80;
const SS_ENDELLIPSIS: u32 = 0x4000;
const CBS_DROPDOWNLIST: u32 = 3;
const TBS_NOTICKS: u32 = 0x10;
const CB_ADDSTRING: u32 = 0x143;
const CB_GETCURSEL: u32 = 0x147;
const CB_SETCURSEL: u32 = 0x14E;
const TBM_GETPOS: u32 = 0x400;
const TBM_SETPOS: u32 = 0x405;
const TBM_SETRANGE: u32 = 0x406;
const TBM_SETPAGESIZE: u32 = 0x415;
const EM_SETREADONLY: u32 = 0xCF;
const BN_CLICKED: u16 = 0;
const LBN_SELCHANGE: u16 = 1;
const LBN_DBLCLK: u16 = 2;
const LBS_NOTIFY: u32 = 1;
const LBS_NOINTEGRALHEIGHT: u32 = 0x100;
const LB_ADDSTRING: u32 = 0x180;
const LB_RESETCONTENT: u32 = 0x184;
const LB_SETCURSEL: u32 = 0x186;
const LB_GETCURSEL: u32 = 0x188;
const PBM_SETRANGE32: u32 = 0x406;
const PBM_SETPOS: u32 = 0x402;
const DM_GETDEFID: u32 = 0x400;
const DC_HASDEFID: isize = 0x534B;
const WM_KEYDOWN: u32 = 0x100;
const WM_KEYUP: u32 = 0x101;
const WM_CHAR: u32 = 0x102;
const WM_SYSKEYDOWN: u32 = 0x104;
const WM_SYSKEYUP: u32 = 0x105;
const WM_SYSCHAR: u32 = 0x106;
const WM_KILLFOCUS: u32 = 0x8;
const WM_GETDLGCODE: u32 = 0x87;
const DLGC_WANTALLKEYS: isize = 4;

// ----- layout (logical pixels) ------------------------------------------------------------
const MARGIN: i32 = 16;
const CONTENT_W: i32 = 524;
const PAD: i32 = 16;
const INNER_X: i32 = MARGIN + PAD;
const INNER_W: i32 = CONTENT_W - 2 * PAD;
const ROW: i32 = 30;
const ERR_H: i32 = 18;
const TITLE_H: i32 = 38;
const SECTION_GAP: i32 = 14;
const BAR_H: i32 = 60;
const LABEL_W: i32 = 140;

struct Item {
    hwnd: HWND,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

struct Section {
    title: &'static str,
    y0: i32,
    y1: i32,
}

#[derive(Default)]
struct Ctl {
    drag_on: HWND,
    drag_mod: HWND,
    hold_on: HWND,
    hold_ms: HWND,
    scope: HWND,
    chord: HWND,
    capture: HWND,
    fallback: HWND,
    paused: HWND,
    quiet: HWND,
    excluded: HWND,
    radius: HWND,
    radius_val: HWND,
    reduce: HWND,
    radio_next: HWND,
    radio_folder: HWND,
    folder: HWND,
    browse: HWND,
    shelf: HWND,
    privacy: HWND,
    stamps: HWND,
    quality: HWND,
    video: HWND,
    image: HWND,
    autostart: HWND,
    context: HWND,
    engines: Vec<HWND>,
    tools: HWND,
    dl_pandoc: HWND,
    get_libre: HWND,
    progress: HWND,
    dl_status: HWND,
    watch_list: HWND,
    watch_add: HWND,
    watch_edit: HWND,
    watch_remove: HWND,
    /// Error labels: hold ms, chord, fallback, excluded, folder, watch folders.
    errs: [HWND; 6],
    // Bar (not scrolled)
    bar_cover: HWND,
    message: HWND,
    save: HWND,
    cancel: HWND,
    reset: HWND,
}

enum Msg {
    Saved(Result<bool, String>),
    Browsed(Option<PathBuf>),
}

const ENGINE_SLOTS: usize = 5;

pub struct SettingsWin {
    host: Rc<dyn PanelHost>,
    watch_rows: RefCell<Vec<WatchRow>>,
    downloading: Cell<bool>,
    hwnd: Cell<HWND>,
    theme: RefCell<Theme>,
    dpi: Cell<u32>,
    font: Cell<windows::Win32::Graphics::Gdi::HFONT>,
    font_bold: Cell<windows::Win32::Graphics::Gdi::HFONT>,
    items: RefCell<Vec<Item>>,
    sections: RefCell<Vec<Section>>,
    ctl: RefCell<Ctl>,
    content_h: Cell<i32>,
    scroll: Cell<i32>,
    input: RefCell<Option<SettingsInput>>,
    orig: RefCell<Option<SettingsForm>>,
    mail: Mailbox<Msg>,
    capturing: Cell<bool>,
    capture_prev: RefCell<String>,
    saving: Cell<bool>,
    message_is_error: Cell<bool>,
}

impl SettingsWin {
    pub fn new(host: Rc<dyn PanelHost>) -> Rc<SettingsWin> {
        Rc::new(SettingsWin {
            host,
            watch_rows: RefCell::new(Vec::new()),
            downloading: Cell::new(false),
            hwnd: Cell::new(HWND::default()),
            theme: RefCell::new(Theme::new(win::system_uses_dark())),
            dpi: Cell::new(96),
            font: Cell::new(Default::default()),
            font_bold: Cell::new(Default::default()),
            items: RefCell::new(Vec::new()),
            sections: RefCell::new(Vec::new()),
            ctl: RefCell::new(Ctl::default()),
            content_h: Cell::new(0),
            scroll: Cell::new(0),
            input: RefCell::new(None),
            orig: RefCell::new(None),
            mail: Mailbox::default(),
            capturing: Cell::new(false),
            capture_prev: RefCell::new(String::new()),
            saving: Cell::new(false),
            message_is_error: Cell::new(false),
        })
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd.get()
    }

    pub fn is_visible(&self) -> bool {
        let h = self.hwnd.get();
        // SAFETY: plain query on a handle we own (or null).
        !h.is_invalid() && unsafe { IsWindowVisible(h).as_bool() }
    }

    /// Open the window, or bring it to the front. A window that is already
    /// open keeps what the user typed.
    pub fn show(self: &Rc<Self>, input: SettingsInput) {
        let already_open = self.is_visible();
        if self.hwnd.get().is_invalid() && !self.create() {
            return;
        }
        let h = self.hwnd.get();
        if !already_open {
            let form = SettingsForm::from_config(&input.config, input.autostart, input.context_menu);
            self.set_engines(&input.engines);
            *self.input.borrow_mut() = Some(input);
            *self.orig.borrow_mut() = Some(form.clone());
            self.write_form(&form);
            self.show_errors(&[]);
            self.set_message("", false);
            self.set_scroll(0);
        }
        // SAFETY: handle of our own window.
        unsafe {
            let _ = ShowWindow(h, SW_RESTORE);
            let _ = ShowWindow(h, SW_SHOW);
            let _ = SetForegroundWindow(h);
        }
    }

    pub fn hide(&self) {
        let h = self.hwnd.get();
        if h.is_invalid() {
            return;
        }
        // SAFETY: handle of our own window.
        unsafe {
            let _ = ShowWindow(h, SW_HIDE);
        }
    }

    pub fn destroy(&self) {
        let h = self.hwnd.get();
        if h.is_invalid() {
            return;
        }
        // SAFETY: our own window.
        unsafe {
            let _ = DestroyWindow(h);
        }
        self.hwnd.set(HWND::default());
    }

    fn create(self: &Rc<Self>) -> bool {
        let style = (WS_OVERLAPPEDWINDOW & !WS_MAXIMIZEBOX) | WS_THICKFRAME | WS_VSCROLL | WS_CLIPCHILDREN;
        let Some(h) = win::create_window(
            Rc::as_ptr(self),
            "ZestShell_Settings",
            "Zest settings",
            style,
            0,
            (i32::MIN, i32::MIN, 640, 760),
            None,
        ) else {
            tracing::error!("cannot create the Settings window");
            return false;
        };
        self.hwnd.set(h);
        self.mail.set_hwnd(h);
        let dpi = win::window_dpi(h);
        self.dpi.set(dpi);
        // Client width for the content, height 80 % of the work area at most.
        let (_, work_h) = work_area_size(h);
        let want_h = win::scale(780, dpi).min(work_h * 9 / 10);
        let frame_w = win::scale(CONTENT_W + 2 * MARGIN + 30, dpi);
        win::move_to_size(h, frame_w, want_h);
        self.layout(h);
        true
    }

    // ----- building the form -----------------------------------------------------------

    fn on_create(&self, hwnd: HWND) {
        let dpi = win::window_dpi(hwnd);
        self.dpi.set(dpi);
        self.font.set(win::create_font(dpi, false));
        self.font_bold.set(win::create_font(dpi, true));
        let mut b = Builder { parent: hwnd, y: MARGIN, items: Vec::new(), sections: Vec::new() };
        let mut c = Ctl::default();

        // ----- Gestures
        b.begin("Gestures");
        let y = b.y;
        c.drag_on = b.check("Drag gesture", ID_DRAG, INNER_X, y, 150);
        b.label("Modifier", INNER_X + 170, y, 70);
        c.drag_mod = b.combo(INNER_X + 245, y, 140, &ModChoice::ALL.map(ModChoice::label));
        b.y += ROW;
        let y = b.y;
        c.hold_on = b.check("Hold gesture", ID_HOLD, INNER_X, y, 150);
        b.label("Hold time (ms)", INNER_X + 170, y, 110);
        c.hold_ms = b.edit(INNER_X + 285, y, 80);
        b.y += ROW;
        c.errs[0] = b.err_label();
        b.row_label("Scope");
        c.scope = b.combo(INNER_X + LABEL_W, b.y, 220, &["File managers", "Anywhere"]);
        b.y += ROW;
        b.row_label("Key chord");
        c.chord = b.edit(INNER_X + LABEL_W, b.y, 210);
        c.capture = b.button("Press keys\u{2026}", ID_CAPTURE, INNER_X + LABEL_W + 218, b.y, 120);
        b.y += ROW;
        c.errs[1] = b.err_label();
        b.row_label("Fallback chord");
        c.fallback = b.edit(INNER_X + LABEL_W, b.y, 210);
        b.y += ROW;
        c.errs[2] = b.err_label();
        let y = b.y;
        c.paused = b.check("Pause gestures", ID_PAUSED, INNER_X, y, 250);
        b.y += ROW;
        let y = b.y;
        c.quiet = b.check("Quiet mode (no success balloons)", ID_QUIET, INNER_X, y, INNER_W);
        b.y += ROW;
        b.row_label("Excluded apps");
        c.excluded = b.edit(INNER_X + LABEL_W, b.y, INNER_W - LABEL_W);
        b.y += ROW;
        c.errs[3] = b.err_label();
        b.end();

        // ----- Wheel
        b.begin("Wheel");
        b.row_label("Outer radius");
        c.radius = b.trackbar(INNER_X + LABEL_W, b.y, 250);
        c.radius_val = b.label("140 px", INNER_X + LABEL_W + 258, b.y, 70);
        b.y += ROW;
        b.row_label("Reduce animations");
        c.reduce = b.combo(INNER_X + LABEL_W, b.y, 220, &["Follow system", "Always"]);
        b.y += ROW;
        b.end();

        // ----- Output
        b.begin("Output");
        let y = b.y;
        c.radio_next = b.radio("Next to source file", ID_RADIO_NEXT, INNER_X, y, 300);
        b.y += ROW;
        let y = b.y;
        c.radio_folder = b.radio("Folder:", ID_RADIO_FOLDER, INNER_X, y, 90);
        c.folder = b.edit(INNER_X + 100, y, INNER_W - 100 - 108);
        c.browse = b.button("Browse\u{2026}", ID_BROWSE, INNER_X + INNER_W - 100, y, 100);
        b.y += ROW;
        c.errs[4] = b.err_label();
        let y = b.y;
        c.shelf = b.check("Send finished outputs to the Shelf", ID_SHELF, INNER_X, y, INNER_W);
        b.y += ROW;
        let y = b.y;
        c.privacy = b.check("Privacy mode (strip metadata from outputs)", ID_PRIVACY, INNER_X, y, INNER_W);
        b.y += ROW;
        let y = b.y;
        c.stamps = b.check("Keep file timestamps", ID_STAMPS, INNER_X, y, INNER_W);
        b.y += ROW;
        b.row_label("Default quality");
        c.quality = b.combo(INNER_X + LABEL_W, b.y, 220, &["High", "Medium", "Small"]);
        b.y += ROW;
        b.end();

        // ----- Jobs
        b.begin("Jobs");
        let video: Vec<String> = std::iter::once("Auto".to_owned()).chain((1..=VIDEO_WORKERS_MAX).map(|n| n.to_string())).collect();
        let image: Vec<String> = std::iter::once("Auto".to_owned()).chain((1..=IMAGE_WORKERS_MAX).map(|n| n.to_string())).collect();
        b.row_label("Video workers");
        c.video = b.combo(INNER_X + LABEL_W, b.y, 120, &video.iter().map(String::as_str).collect::<Vec<_>>());
        b.y += ROW;
        b.row_label("Image workers");
        c.image = b.combo(INNER_X + LABEL_W, b.y, 120, &image.iter().map(String::as_str).collect::<Vec<_>>());
        b.y += ROW;
        b.end();

        // ----- System
        b.begin("System");
        let y = b.y;
        c.autostart = b.check("Start at sign-in", ID_AUTOSTART, INNER_X, y, INNER_W);
        b.y += ROW;
        let y = b.y;
        c.context = b.check("Add to right-click menu and Send To", ID_CONTEXT, INNER_X, y, INNER_W);
        b.y += ROW;
        b.end();

        // ----- Watch folders
        b.begin("Watch folders");
        let y = b.y;
        b.label("New files in these folders are converted by themselves (see README).", INNER_X, y, INNER_W);
        b.y += ROW - 4;
        let y = b.y;
        c.watch_list = b.listbox(INNER_X, y, INNER_W, 96);
        b.y += 96 + 6;
        let y = b.y;
        c.watch_add = b.button("Add\u{2026}", ID_WATCH_ADD, INNER_X, y, 90);
        c.watch_edit = b.button("Edit\u{2026}", ID_WATCH_EDIT, INNER_X + 98, y, 90);
        c.watch_remove = b.button("Remove", ID_WATCH_REMOVE, INNER_X + 196, y, 90);
        b.y += ROW;
        c.errs[5] = b.err_label();
        b.end();

        // ----- Engines
        b.begin("Engines");
        for _ in 0..ENGINE_SLOTS {
            let h = b.label("", INNER_X, b.y, INNER_W);
            c.engines.push(h);
            b.y += 24;
        }
        b.y += 6;
        let y = b.y;
        c.tools = b.button("Open tools folder", ID_TOOLS, INNER_X, y, 150);
        c.dl_pandoc = b.button("Download Pandoc (40 MB)", ID_PANDOC, INNER_X + 158, y, 200);
        c.get_libre = b.button("Get LibreOffice\u{2026}", ID_LIBRE, INNER_X + 366, y, 126);
        b.y += ROW;
        let y = b.y;
        c.progress = b.progress(INNER_X, y + 4, 330);
        c.dl_status = b.label("", INNER_X + 338, y, INNER_W - 338);
        b.y += ROW;
        b.end();

        let content_h = b.y + MARGIN;
        *self.items.borrow_mut() = std::mem::take(&mut b.items);
        *self.sections.borrow_mut() = std::mem::take(&mut b.sections);
        self.content_h.set(content_h);

        // ----- Bar (created last: above the scrolled controls)
        let cover = win::create_child(hwnd, w!("STATIC"), "", WS_CLIPSIBLINGS, 0, ID_PLAIN);
        c.bar_cover = cover;
        c.message = win::create_child(hwnd, w!("STATIC"), "", SS_CENTERIMAGE | SS_ENDELLIPSIS | SS_NOPREFIX, 0, ID_PLAIN);
        c.reset = win::create_child(hwnd, w!("BUTTON"), "Reset to defaults", WS_TABSTOP, 0, ID_RESET);
        c.save = win::create_child(hwnd, w!("BUTTON"), "Save", WS_TABSTOP | BS_DEFPUSHBUTTON, 0, ID_SAVE);
        c.cancel = win::create_child(hwnd, w!("BUTTON"), "Cancel", WS_TABSTOP, 0, ID_CANCEL);

        // Tooltips.
        let tt = win::tooltip_create(hwnd);
        win::tooltip_add(tt, hwnd, c.excluded, "Programs where Zest does not react, separated by commas. Example: game.exe, photoshop.exe");
        win::tooltip_add(tt, hwnd, c.chord, "Example: Ctrl+Alt+Space. Use at least one modifier. Empty = no key chord.");
        win::tooltip_add(tt, hwnd, c.fallback, "Used when the key chord is taken by another program. Empty = none.");
        win::tooltip_add(tt, hwnd, c.capture, "Click, then press the keys you want");
        win::tooltip_add(tt, hwnd, c.hold_ms, "100 to 3000 milliseconds");
        win::tooltip_add(tt, hwnd, c.shelf, "Add the files of every finished job to the Shelf and open it");
        win::tooltip_add(tt, hwnd, c.browse, "Choose the output folder");
        win::tooltip_add(tt, hwnd, c.dl_pandoc, "Downloads Pandoc from github.com after you agree. Nothing else is downloaded.");
        win::tooltip_add(tt, hwnd, c.get_libre, "Opens the LibreOffice download page in your browser");
        win::tooltip_add(tt, hwnd, c.watch_list, "Double-click to edit. Settings are saved with Save.");
        win::send(c.progress, PBM_SETRANGE32, 0, 100);
        // The download controls start hidden.
        for h in [c.dl_pandoc, c.get_libre, c.progress, c.dl_status] {
            win::show(h, false);
        }

        // Combos and edits that we fill.
        win::send(c.radius, TBM_SETRANGE, 1, ((RADIUS_MAX << 16) | RADIUS_MIN) as isize);
        win::send(c.radius, TBM_SETPAGESIZE, 0, 10);

        let capture_edit = c.chord;
        *self.ctl.borrow_mut() = c;
        // SAFETY: subclass of our own edit control; removed in on_destroy. `self` outlives the window.
        unsafe {
            let _ = SetWindowSubclass(capture_edit, Some(capture_proc), 1, self as *const SettingsWin as usize);
        }
        self.apply_fonts();
        self.apply_theme(hwnd);
        self.layout(hwnd);
    }

    fn all_windows(&self) -> Vec<HWND> {
        let c = self.ctl.borrow();
        let mut v: Vec<HWND> = self.items.borrow().iter().map(|i| i.hwnd).collect();
        v.extend([c.bar_cover, c.message, c.save, c.cancel, c.reset]);
        v
    }

    fn apply_fonts(&self) {
        for h in self.all_windows() {
            win::set_font(h, self.font.get());
        }
    }

    fn apply_theme(&self, hwnd: HWND) {
        let dark = win::system_uses_dark();
        {
            let mut t = self.theme.borrow_mut();
            if t.dark != dark {
                *t = Theme::new(dark);
            }
        }
        let t = self.theme.borrow();
        win::style_frame(hwnd, t.dark);
        let c = self.ctl.borrow();
        for h in [c.drag_mod, c.scope, c.reduce, c.quality, c.video, c.image] {
            win::theme_control(h, Kind::Combo, t.dark);
        }
        win::theme_control(c.watch_list, Kind::List, t.dark);
        for h in [c.hold_ms, c.chord, c.fallback, c.excluded, c.folder] {
            win::theme_control(h, Kind::Edit, t.dark);
        }
        for h in [c.capture, c.browse, c.tools, c.save, c.cancel, c.reset, c.dl_pandoc, c.get_libre, c.watch_add, c.watch_edit, c.watch_remove] {
            win::theme_control(h, Kind::Button, t.dark);
        }
        win::invalidate(hwnd);
        for i in self.items.borrow().iter() {
            win::invalidate(i.hwnd);
        }
    }

    // ----- layout and scrolling --------------------------------------------------------------

    fn view_h(&self, hwnd: HWND) -> i32 {
        let (_, h) = win::client_size(hwnd);
        (h - win::scale(BAR_H, self.dpi.get())).max(10)
    }

    fn max_scroll(&self, hwnd: HWND) -> i32 {
        (win::scale(self.content_h.get(), self.dpi.get()) - self.view_h(hwnd)).max(0)
    }

    fn layout(&self, hwnd: HWND) {
        if self.ctl.borrow().save.is_invalid() {
            return;
        }
        let dpi = self.dpi.get();
        let s = |v: i32| win::scale(v, dpi);
        let (w, h) = win::client_size(hwnd);
        // The scroll position stays inside the new range.
        let max = self.max_scroll(hwnd);
        self.scroll.set(self.scroll.get().clamp(0, max));
        let off = self.scroll.get();
        for it in self.items.borrow().iter() {
            win::move_to(it.hwnd, s(it.x), s(it.y) - off, s(it.w), s(it.h));
        }
        let c = self.ctl.borrow();
        let bar_y = h - s(BAR_H);
        win::move_to(c.bar_cover, 0, bar_y, w, s(BAR_H));
        let btn_h = s(32);
        let by = bar_y + (s(BAR_H) - btn_h) / 2;
        let m = s(MARGIN);
        win::move_to(c.cancel, w - m - s(96), by, s(96), btn_h);
        win::move_to(c.save, w - m - s(96) * 2 - s(8), by, s(96), btn_h);
        win::move_to(c.reset, m, by, s(140), btn_h);
        let msg_x = m + s(140) + s(10);
        win::move_to(c.message, msg_x, by, (w - m - s(96) * 2 - s(8) - s(10) - msg_x).max(10), btn_h);
        drop(c);
        self.update_scrollbar(hwnd);
        win::invalidate(hwnd);
    }

    fn update_scrollbar(&self, hwnd: HWND) {
        let dpi = self.dpi.get();
        let total = win::scale(self.content_h.get(), dpi);
        let si = SCROLLINFO {
            cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
            nMin: 0,
            nMax: total - 1,
            nPage: self.view_h(hwnd) as u32,
            nPos: self.scroll.get(),
            nTrackPos: 0,
        };
        // SAFETY: valid window and SCROLLINFO.
        unsafe {
            SetScrollInfo(hwnd, windows::Win32::UI::WindowsAndMessaging::SB_VERT, &si, true);
        }
    }

    fn set_scroll(&self, pos: i32) {
        let hwnd = self.hwnd.get();
        if hwnd.is_invalid() {
            return;
        }
        let pos = pos.clamp(0, self.max_scroll(hwnd));
        if pos == self.scroll.get() {
            return;
        }
        self.scroll.set(pos);
        self.layout(hwnd);
    }

    fn scroll_item_into_view(&self, ctl: HWND) {
        let hwnd = self.hwnd.get();
        let dpi = self.dpi.get();
        let found = self.items.borrow().iter().find(|i| i.hwnd == ctl).map(|i| (win::scale(i.y, dpi), win::scale(i.h.min(ROW), dpi)));
        if let Some((top, h)) = found {
            let view = self.view_h(hwnd);
            let cur = self.scroll.get();
            if top < cur {
                self.set_scroll(top - win::scale(MARGIN, dpi));
            } else if top + h > cur + view {
                self.set_scroll(top + h - view + win::scale(MARGIN, dpi));
            }
        }
    }

    fn on_vscroll(&self, hwnd: HWND, code: u32) {
        let dpi = self.dpi.get();
        let line = win::scale(40, dpi);
        let page = self.view_h(hwnd);
        let cur = self.scroll.get();
        let mut si = SCROLLINFO { cbSize: std::mem::size_of::<SCROLLINFO>() as u32, fMask: SIF_ALL | SIF_TRACKPOS, ..Default::default() };
        // SAFETY: valid window and SCROLLINFO.
        unsafe {
            let _ = GetScrollInfo(hwnd, windows::Win32::UI::WindowsAndMessaging::SB_VERT, &mut si);
        }
        let new = match code {
            0 => cur - line,        // SB_LINEUP
            1 => cur + line,        // SB_LINEDOWN
            2 => cur - page,        // SB_PAGEUP
            3 => cur + page,        // SB_PAGEDOWN
            4 | 5 => si.nTrackPos,  // SB_THUMBPOSITION, SB_THUMBTRACK
            6 => 0,                 // SB_TOP
            7 => i32::MAX,          // SB_BOTTOM
            _ => cur,
        };
        self.set_scroll(new);
    }

    // ----- values -----------------------------------------------------------------------------

    fn set_engines(&self, lines: &[EngineLine]) {
        let c = self.ctl.borrow();
        // "Download Pandoc" and "Get LibreOffice" show only for an engine that is not installed.
        let missing = |name: &str| lines.iter().any(|l| l.name == name && !l.ok);
        win::show(c.dl_pandoc, missing("Pandoc") && !self.downloading.get());
        win::show(c.get_libre, missing("LibreOffice"));
        for (i, h) in c.engines.iter().enumerate() {
            match lines.get(i) {
                Some(l) => {
                    let mark = if l.ok { "\u{2713}" } else { "\u{2715}" };
                    let text = if l.detail.is_empty() { format!("{mark}  {}", l.name) } else { format!("{mark}  {}  \u{2013}  {}", l.name, l.detail) };
                    win::set_text(*h, &text);
                }
                None => win::set_text(*h, ""),
            }
        }
    }

    /// Fill the list of watch folders.
    fn fill_watch_list(&self, select: Option<usize>) {
        let c = self.ctl.borrow();
        win::send(c.watch_list, LB_RESETCONTENT, 0, 0);
        for r in self.watch_rows.borrow().iter() {
            let t = win::wide(&r.describe());
            win::send(c.watch_list, LB_ADDSTRING, 0, t.as_ptr() as isize);
        }
        if let Some(i) = select {
            win::send(c.watch_list, LB_SETCURSEL, i, 0);
        }
        let has = select.is_some();
        win::set_text_enabled(c.watch_edit, has);
        win::set_text_enabled(c.watch_remove, has);
    }

    fn watch_selected(&self) -> Option<usize> {
        let i = win::send(self.ctl.borrow().watch_list, LB_GETCURSEL, 0, 0);
        usize::try_from(i).ok().filter(|i| *i < self.watch_rows.borrow().len())
    }

    fn watch_add(&self) {
        let blank = WatchRow { folder: String::new(), op: "convert:jpg".into(), include: String::new(), recursive: false, enabled: true };
        if let Some(row) = watch_dialog(self.hwnd.get(), "Add watch folder", blank) {
            self.watch_rows.borrow_mut().push(row);
            let last = self.watch_rows.borrow().len() - 1;
            self.fill_watch_list(Some(last));
        }
    }

    fn watch_edit(&self) {
        let Some(i) = self.watch_selected() else { return };
        let cur = self.watch_rows.borrow()[i].clone();
        if let Some(row) = watch_dialog(self.hwnd.get(), "Edit watch folder", cur) {
            self.watch_rows.borrow_mut()[i] = row;
            self.fill_watch_list(Some(i));
        }
    }

    fn watch_remove(&self) {
        let Some(i) = self.watch_selected() else { return };
        self.watch_rows.borrow_mut().remove(i);
        let n = self.watch_rows.borrow().len();
        self.fill_watch_list(if n == 0 { None } else { Some(i.min(n - 1)) });
    }

    // ----- engine download ------------------------------------------------------------------

    /// New engine status from the app. Keeps what the user typed in the rest of the form.
    pub fn update_engines(&self, lines: Vec<EngineLine>) {
        if let Some(i) = self.input.borrow_mut().as_mut() {
            i.engines = lines.clone();
        }
        if !self.hwnd.get().is_invalid() {
            self.set_engines(&lines);
        }
    }

    pub fn download_progress(&self, percent: Option<f32>, text: &str) {
        if self.hwnd.get().is_invalid() {
            return;
        }
        let c = self.ctl.borrow();
        win::show(c.progress, true);
        win::show(c.dl_status, true);
        win::send(c.progress, PBM_SETPOS, percent.map_or(100, |p| p.clamp(0.0, 100.0) as usize), 0);
        let shown = match percent {
            Some(p) => format!("{text} {p:.0} %"),
            None => text.to_owned(),
        };
        win::set_text(c.dl_status, &shown);
    }

    pub fn download_done(&self, result: Result<(), String>) {
        self.downloading.set(false);
        if self.hwnd.get().is_invalid() {
            return;
        }
        {
            let c = self.ctl.borrow();
            win::show(c.progress, false);
            win::set_text(c.dl_status, "");
            win::show(c.dl_status, false);
        }
        match result {
            Ok(()) => self.set_message("Pandoc is installed.", false),
            Err(e) => self.set_message(&e, true),
        }
        // The app sends the new engine status after its check; show the button again until then.
        let lines = self.input.borrow().as_ref().map(|i| i.engines.clone()).unwrap_or_default();
        self.set_engines(&lines);
    }

    fn start_pandoc_download(&self) {
        if self.downloading.get() {
            return;
        }
        let ok = win::confirm(
            self.hwnd.get(),
            "Download Pandoc",
            "Download Pandoc (about 40 MB)?",
            "Zest downloads Pandoc from github.com and checks its checksum. It is saved in your Zest data folder. This is the only network use of Zest, and it happens only because you agreed.",
            "Download",
        );
        if !ok {
            return;
        }
        self.downloading.set(true);
        win::show(self.ctl.borrow().dl_pandoc, false);
        self.set_message("", false);
        self.download_progress(Some(0.0), "Downloading");
        self.host.engine_download("pandoc");
    }

    fn write_form(&self, f: &SettingsForm) {
        *self.watch_rows.borrow_mut() = f.watch.clone();
        self.fill_watch_list(None);
        let c = self.ctl.borrow();
        win::check_set(c.drag_on, f.drag_on);
        let idx = ModChoice::ALL.iter().position(|m| *m == f.drag_mod).unwrap_or(0);
        win::send(c.drag_mod, CB_SETCURSEL, idx, 0);
        win::check_set(c.hold_on, f.hold_on);
        win::set_text(c.hold_ms, &f.hold_ms);
        win::send(c.scope, CB_SETCURSEL, usize::from(f.scope == zest_core::Scope::Anywhere), 0);
        win::set_text(c.chord, &f.chord);
        win::set_text(c.fallback, &f.fallback_chord);
        win::check_set(c.paused, f.paused);
        win::check_set(c.quiet, f.quiet);
        win::set_text(c.excluded, &f.excluded);
        win::send(c.radius, TBM_SETPOS, 1, f.outer_radius.clamp(RADIUS_MIN, RADIUS_MAX) as isize);
        win::set_text(c.radius_val, &format!("{} px", f.outer_radius.clamp(RADIUS_MIN, RADIUS_MAX)));
        win::send(c.reduce, CB_SETCURSEL, usize::from(f.reduce_always), 0);
        win::check_set(c.radio_next, !f.folder_mode);
        win::check_set(c.radio_folder, f.folder_mode);
        win::set_text(c.folder, &f.folder);
        win::check_set(c.shelf, f.send_to_shelf);
        win::check_set(c.privacy, f.privacy_mode);
        win::check_set(c.stamps, f.keep_timestamps);
        let q = match f.quality {
            zest_core::Quality::High => 0,
            zest_core::Quality::Medium => 1,
            zest_core::Quality::Small => 2,
        };
        win::send(c.quality, CB_SETCURSEL, q, 0);
        win::send(c.video, CB_SETCURSEL, f.video_workers.unwrap_or(0).min(VIDEO_WORKERS_MAX) as usize, 0);
        win::send(c.image, CB_SETCURSEL, f.image_workers.unwrap_or(0).min(IMAGE_WORKERS_MAX) as usize, 0);
        win::check_set(c.autostart, f.autostart);
        win::check_set(c.context, f.context_menu);
        drop(c);
        self.sync_enabled();
    }

    fn read_form(&self) -> SettingsForm {
        let c = self.ctl.borrow();
        let sel = |h: HWND| win::send(h, CB_GETCURSEL, 0, 0).max(0) as usize;
        let workers = |h: HWND| match sel(h) {
            0 => None,
            n => Some(n as u32),
        };
        SettingsForm {
            drag_on: win::check_get(c.drag_on),
            drag_mod: ModChoice::ALL[sel(c.drag_mod).min(ModChoice::ALL.len() - 1)],
            hold_on: win::check_get(c.hold_on),
            hold_ms: win::get_text(c.hold_ms),
            scope: if sel(c.scope) == 1 { zest_core::Scope::Anywhere } else { zest_core::Scope::FileManager },
            chord: win::get_text(c.chord),
            fallback_chord: win::get_text(c.fallback),
            paused: win::check_get(c.paused),
            quiet: win::check_get(c.quiet),
            excluded: win::get_text(c.excluded),
            outer_radius: win::send(c.radius, TBM_GETPOS, 0, 0).max(0) as u32,
            reduce_always: sel(c.reduce) == 1,
            folder_mode: win::check_get(c.radio_folder),
            folder: win::get_text(c.folder),
            send_to_shelf: win::check_get(c.shelf),
            privacy_mode: win::check_get(c.privacy),
            keep_timestamps: win::check_get(c.stamps),
            quality: match sel(c.quality) {
                1 => zest_core::Quality::Medium,
                2 => zest_core::Quality::Small,
                _ => zest_core::Quality::High,
            },
            video_workers: workers(c.video),
            image_workers: workers(c.image),
            autostart: win::check_get(c.autostart),
            context_menu: win::check_get(c.context),
            watch: self.watch_rows.borrow().clone(),
        }
    }

    /// Edits that depend on a switch are greyed while the switch is off.
    fn sync_enabled(&self) {
        let c = self.ctl.borrow();
        win::set_text_enabled(c.hold_ms, win::check_get(c.hold_on));
        let folder = win::check_get(c.radio_folder);
        win::set_text_enabled(c.folder, folder);
        win::set_text_enabled(c.browse, folder);
    }

    fn show_errors(&self, errs: &[FieldError]) {
        let c = self.ctl.borrow();
        for h in c.errs {
            win::set_text(h, "");
        }
        for e in errs {
            let slot = match e.field {
                Field::HoldMs => 0,
                Field::Chord => 1,
                Field::Fallback => 2,
                Field::Excluded => 3,
                Field::Folder => 4,
                Field::Watch => 5,
            };
            win::set_text(c.errs[slot], &e.message);
        }
    }

    fn field_control(&self, f: Field) -> HWND {
        let c = self.ctl.borrow();
        match f {
            Field::HoldMs => c.hold_ms,
            Field::Chord => c.chord,
            Field::Fallback => c.fallback,
            Field::Excluded => c.excluded,
            Field::Folder => c.folder,
            Field::Watch => c.watch_list,
        }
    }

    fn set_message(&self, text: &str, error: bool) {
        let c = self.ctl.borrow();
        win::set_text(c.message, text);
        drop(c);
        self.message_is_error.set(error);
    }

    // ----- actions ----------------------------------------------------------------------------

    fn save(&self) {
        if self.saving.get() {
            return;
        }
        let Some(orig) = self.orig.borrow().clone() else { return };
        let Some(path) = self.input.borrow().as_ref().map(|i| i.config_path.clone()) else { return };
        let new = self.read_form();
        let errs = new.validate();
        self.show_errors(&errs);
        if let Some(first) = errs.first() {
            self.set_message("Fix the marked fields first.", true);
            let ctl = self.field_control(first.field);
            self.scroll_item_into_view(ctl);
            // SAFETY: valid control handle.
            unsafe {
                let _ = SetFocus(ctl);
            }
            return;
        }
        self.saving.set(true);
        win::set_text_enabled(self.ctl.borrow().save, false);
        self.set_message("Saving\u{2026}", false);
        let mail = self.mail.clone();
        win::spawn("zest-settings-save", move || {
            let r = save_to_file(&path, &orig, &new);
            mail.post(Msg::Saved(r));
        });
    }

    fn reset(&self) {
        let ok = win::confirm(
            self.hwnd.get(),
            "Zest settings",
            "Reset all settings to the defaults?",
            "The form shows the default values. Nothing changes until you press Save. Comments and unknown keys in config.toml stay.",
            "Reset",
        );
        if !ok {
            return;
        }
        if let Some(orig) = self.orig.borrow().clone() {
            self.write_form(&orig.defaults());
            self.show_errors(&[]);
            self.set_message("Defaults shown. Press Save to keep them.", false);
        }
    }

    fn browse(&self) {
        let start = {
            let t = win::get_text(self.ctl.borrow().folder);
            let p = PathBuf::from(t.trim());
            p.is_absolute().then_some(p)
        };
        let mail = self.mail.clone();
        win::spawn("zest-settings-browse", move || {
            let r = zest_shell::pick_folder("Output folder", start.as_deref()).ok().flatten();
            mail.post(Msg::Browsed(r));
        });
    }

    fn on_mail(&self) {
        for m in self.mail.drain() {
            match m {
                Msg::Saved(r) => {
                    self.saving.set(false);
                    win::set_text_enabled(self.ctl.borrow().save, true);
                    match r {
                        Ok(_) => {
                            self.set_message("", false);
                            self.hide();
                        }
                        Err(e) => self.set_message(&e, true),
                    }
                }
                Msg::Browsed(Some(p)) => {
                    let c = self.ctl.borrow();
                    win::set_text(c.folder, &p.to_string_lossy());
                    win::check_set(c.radio_folder, true);
                    win::check_set(c.radio_next, false);
                    drop(c);
                    self.sync_enabled();
                }
                Msg::Browsed(None) => {}
            }
        }
    }

    fn command(&self, hwnd: HWND, id: i32, code: u16, ctl: HWND) {
        match id {
            ID_SAVE if code == BN_CLICKED => self.save(),
            ID_CANCEL => self.hide(),
            ID_RESET => self.reset(),
            ID_DRAG | ID_HOLD | ID_PAUSED | ID_QUIET | ID_SHELF | ID_PRIVACY | ID_STAMPS | ID_AUTOSTART | ID_CONTEXT => {
                win::check_set(ctl, !win::check_get(ctl));
                self.sync_enabled();
            }
            ID_RADIO_NEXT | ID_RADIO_FOLDER => {
                let c = self.ctl.borrow();
                win::check_set(c.radio_next, id == ID_RADIO_NEXT);
                win::check_set(c.radio_folder, id == ID_RADIO_FOLDER);
                drop(c);
                self.sync_enabled();
            }
            ID_BROWSE => self.browse(),
            ID_CAPTURE => self.begin_capture(),
            ID_WATCH_ADD => self.watch_add(),
            ID_WATCH_EDIT => self.watch_edit(),
            ID_WATCH_REMOVE => self.watch_remove(),
            ID_WATCH_LIST => match code {
                LBN_DBLCLK => self.watch_edit(),
                LBN_SELCHANGE => {
                    let has = self.watch_selected().is_some();
                    let c = self.ctl.borrow();
                    win::set_text_enabled(c.watch_edit, has);
                    win::set_text_enabled(c.watch_remove, has);
                }
                _ => {}
            },
            ID_PANDOC => self.start_pandoc_download(),
            ID_LIBRE => {
                let url = self.input.borrow().as_ref().map(|i| i.libreoffice_url.clone());
                if let Some(u) = url {
                    win::shell_open_async(PathBuf::from(u));
                }
            }
            ID_TOOLS => {
                if let Some(p) = self.input.borrow().as_ref().map(|i| i.tools_dir.clone()) {
                    win::shell_open_async(p);
                }
            }
            _ => {}
        }
        let _ = hwnd;
    }

    // ----- key capture ---------------------------------------------------------------------------

    fn begin_capture(&self) {
        let chord = self.ctl.borrow().chord;
        *self.capture_prev.borrow_mut() = win::get_text(chord);
        self.capturing.set(true);
        win::set_text(chord, "Press the keys\u{2026}");
        win::send(chord, EM_SETREADONLY, 1, 0);
        // SAFETY: valid control handle.
        unsafe {
            let _ = SetFocus(chord);
        }
    }

    fn end_capture(&self, text: Option<String>) {
        if !self.capturing.replace(false) {
            return;
        }
        let chord = self.ctl.borrow().chord;
        win::send(chord, EM_SETREADONLY, 0, 0);
        let t = text.unwrap_or_else(|| self.capture_prev.borrow().clone());
        win::set_text(chord, &t);
    }

    /// Returns true if the message was used.
    fn capture_message(&self, msg: u32, wp: usize) -> bool {
        if !self.capturing.get() {
            return false;
        }
        match msg {
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let vk = wp as u32;
                let down = |k: i32| {
                    // SAFETY: plain key state query.
                    unsafe { GetKeyState(k) < 0 }
                };
                let mut mods = Vec::new();
                if down(0x11) {
                    mods.push(zest_core::Modifier::Ctrl);
                }
                if down(0x12) {
                    mods.push(zest_core::Modifier::Alt);
                }
                if down(0x10) {
                    mods.push(zest_core::Modifier::Shift);
                }
                if down(0x5B) || down(0x5C) {
                    mods.push(zest_core::Modifier::Win);
                }
                let chord = self.ctl.borrow().chord;
                match key_name_from_vk(vk) {
                    None => {
                        // A modifier alone: show what is held.
                        let held = if mods.is_empty() { "Press the keys\u{2026}".to_owned() } else { format!("{}+\u{2026}", format_chord(&mods, "").trim_end_matches('+')) };
                        win::set_text(chord, &held);
                    }
                    Some(key) if key == "Esc" && mods.is_empty() => self.end_capture(None),
                    Some(_) if mods.is_empty() => win::set_text(chord, "Add Ctrl, Alt, Shift or Win\u{2026}"),
                    Some(key) => self.end_capture(Some(format_chord(&mods, &key))),
                }
                true
            }
            WM_CHAR | WM_SYSCHAR | WM_KEYUP | WM_SYSKEYUP => true,
            WM_KILLFOCUS => {
                self.end_capture(None);
                false
            }
            _ => false,
        }
    }

    /// Keys for the message loop: while a chord is captured the dialog logic stays out of the way.
    pub fn pre_translate(&self, msg: &windows::Win32::UI::WindowsAndMessaging::MSG) -> bool {
        let h = self.hwnd.get();
        if h.is_invalid() || self.capturing.get() {
            return false;
        }
        // SAFETY: valid window and message.
        unsafe { windows::Win32::UI::WindowsAndMessaging::IsDialogMessageW(h, msg).as_bool() }
    }

    // ----- painting ---------------------------------------------------------------------------------

    fn paint(&self, hwnd: HWND) {
        let dpi = self.dpi.get();
        let s = |v: i32| win::scale(v, dpi);
        let t = self.theme.borrow();
        // SAFETY: standard double-buffered paint; every GDI object is released.
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let (w, h) = win::client_size(hwnd);
            let mem = CreateCompatibleDC(hdc);
            let bmp = CreateCompatibleBitmap(hdc, w.max(1), h.max(1));
            let old_bmp = SelectObject(mem, HGDIOBJ(bmp.0));
            let full = RECT { left: 0, top: 0, right: w, bottom: h };
            FillRect(mem, &full, t.bg_brush);
            let pen = CreatePen(PS_SOLID, 1, t.border);
            let old_pen = SelectObject(mem, HGDIOBJ(pen.0));
            let old_brush = SelectObject(mem, GetStockObject(NULL_BRUSH));
            let old_font = SelectObject(mem, HGDIOBJ(self.font_bold.get().0));
            SetBkMode(mem, TRANSPARENT);
            SetTextColor(mem, t.fg);
            let off = self.scroll.get();
            for sec in self.sections.borrow().iter() {
                let (l, top, r, bot) = (s(MARGIN), s(sec.y0) - off, s(MARGIN + CONTENT_W), s(sec.y1) - off);
                let _ = RoundRect(mem, l, top, r, bot, s(10), s(10));
                let title: Vec<u16> = sec.title.encode_utf16().collect();
                let _ = TextOutW(mem, l + s(PAD), top + s(9), &title);
            }
            SelectObject(mem, old_font);
            SelectObject(mem, old_brush);
            SelectObject(mem, old_pen);
            let _ = DeleteObject(HGDIOBJ(pen.0));
            let _ = BitBlt(hdc, 0, 0, w, h, mem, 0, 0, SRCCOPY);
            SelectObject(mem, old_bmp);
            let _ = DeleteObject(HGDIOBJ(bmp.0));
            let _ = DeleteDC(mem);
            let _ = EndPaint(hwnd, &ps);
        }
    }

    fn ctl_color(&self, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        let t = self.theme.borrow();
        let hdc = HDC(wp.0 as *mut c_void);
        let target = HWND(lp.0 as *mut c_void);
        let is_edit = matches!(msg, WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX);
        let is_err = self.ctl.borrow().errs.contains(&target) || (self.message_is_error.get() && self.ctl.borrow().message == target);
        // SAFETY: wParam is the HDC of the control.
        unsafe {
            SetTextColor(hdc, if is_err { t.error } else { t.fg });
            if is_edit {
                SetBkColor(hdc, t.edit_bg);
            } else {
                SetBkColor(hdc, t.bg);
            }
        }
        LRESULT(if is_edit { t.edit_brush.0 } else { t.bg_brush.0 } as isize)
    }

    fn draw_item(&self, lp: LPARAM) {
        // SAFETY: WM_DRAWITEM carries a DRAWITEMSTRUCT.
        let di = unsafe { &*(lp.0 as *const DRAWITEMSTRUCT) };
        let id = di.CtlID as i32;
        let radio = id == ID_RADIO_NEXT || id == ID_RADIO_FOLDER;
        win::draw_check(di, &self.theme.borrow(), self.dpi.get(), radio, &win::get_text(di.hwndItem));
    }
}

// ---------------------------------------------------------------------------
// Builder: positions in logical pixels, controls created in order
// ---------------------------------------------------------------------------

struct Builder {
    parent: HWND,
    y: i32,
    items: Vec<Item>,
    sections: Vec<Section>,
}

impl Builder {
    fn add(&mut self, h: HWND, x: i32, y: i32, w: i32, hgt: i32) -> HWND {
        self.items.push(Item { hwnd: h, x, y, w, h: hgt });
        h
    }

    fn begin(&mut self, title: &'static str) {
        self.sections.push(Section { title, y0: self.y, y1: self.y });
        self.y += TITLE_H;
    }

    fn end(&mut self) {
        self.y += 4;
        if let Some(s) = self.sections.last_mut() {
            s.y1 = self.y;
        }
        self.y += SECTION_GAP;
    }

    fn label(&mut self, text: &str, x: i32, y: i32, w: i32) -> HWND {
        let h = win::create_child(self.parent, w!("STATIC"), text, SS_CENTERIMAGE | SS_NOPREFIX | SS_ENDELLIPSIS, 0, ID_PLAIN);
        self.add(h, x, y, w, ROW - 4)
    }

    fn row_label(&mut self, text: &str) -> HWND {
        let y = self.y;
        self.label(text, INNER_X, y, LABEL_W - 8)
    }

    fn err_label(&mut self) -> HWND {
        let y = self.y;
        let h = win::create_child(self.parent, w!("STATIC"), "", SS_NOPREFIX | SS_ENDELLIPSIS, 0, ID_PLAIN);
        self.add(h, INNER_X + LABEL_W, y - 4, INNER_W - LABEL_W, ERR_H);
        self.y += ERR_H - 4;
        h
    }

    fn check(&mut self, text: &str, id: i32, x: i32, y: i32, w: i32) -> HWND {
        let h = win::create_child(self.parent, w!("BUTTON"), text, win::BS_OWNERDRAW | WS_TABSTOP, 0, id);
        self.add(h, x, y, w, ROW - 4)
    }

    fn radio(&mut self, text: &str, id: i32, x: i32, y: i32, w: i32) -> HWND {
        self.check(text, id, x, y, w)
    }

    fn button(&mut self, text: &str, id: i32, x: i32, y: i32, w: i32) -> HWND {
        let h = win::create_child(self.parent, w!("BUTTON"), text, WS_TABSTOP, 0, id);
        self.add(h, x, y, w, ROW - 2)
    }

    fn edit(&mut self, x: i32, y: i32, w: i32) -> HWND {
        let h = win::create_child(self.parent, w!("EDIT"), "", ES_AUTOHSCROLL | WS_TABSTOP, WS_EX_CLIENTEDGE, ID_PLAIN);
        self.add(h, x, y, w, ROW - 4)
    }

    fn combo(&mut self, x: i32, y: i32, w: i32, items: &[&str]) -> HWND {
        let h = win::create_child(self.parent, w!("COMBOBOX"), "", CBS_DROPDOWNLIST | WS_VSCROLL | WS_TABSTOP, 0, ID_PLAIN);
        for it in items {
            let t = win::wide(it);
            win::send(h, CB_ADDSTRING, 0, t.as_ptr() as isize);
        }
        win::send(h, CB_SETCURSEL, 0, 0);
        // The height includes the drop-down list.
        self.add(h, x, y, w, 220)
    }

    fn listbox(&mut self, x: i32, y: i32, w: i32, h: i32) -> HWND {
        let hw = win::create_child(
            self.parent,
            w!("LISTBOX"),
            "",
            LBS_NOTIFY | LBS_NOINTEGRALHEIGHT | WS_VSCROLL | WS_TABSTOP,
            WS_EX_CLIENTEDGE,
            ID_WATCH_LIST,
        );
        self.add(hw, x, y, w, h)
    }

    fn progress(&mut self, x: i32, y: i32, w: i32) -> HWND {
        let hw = win::create_child(self.parent, w!("msctls_progress32"), "", 0, 0, ID_PLAIN);
        self.add(hw, x, y, w, 16)
    }

    fn trackbar(&mut self, x: i32, y: i32, w: i32) -> HWND {
        let h = win::create_child(self.parent, TRACKBAR_CLASSW, "", TBS_NOTICKS | WS_TABSTOP, 0, ID_PLAIN);
        self.add(h, x, y, w, ROW - 2)
    }
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

impl Handler for SettingsWin {
    fn attach(&self, hwnd: HWND) {
        self.hwnd.set(hwnd);
    }

    fn message(&self, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_CREATE => {
                self.on_create(hwnd);
                Some(LRESULT(0))
            }
            WM_SIZE => {
                if wp.0 != 1 {
                    self.layout(hwnd);
                }
                Some(LRESULT(0))
            }
            WM_GETMINMAXINFO => {
                // SAFETY: lParam points to a MINMAXINFO.
                let mm = unsafe { &mut *(lp.0 as *mut MINMAXINFO) };
                let dpi = self.dpi.get();
                mm.ptMinTrackSize = POINT { x: win::scale(CONTENT_W + 2 * MARGIN + 30, dpi), y: win::scale(320, dpi) };
                Some(LRESULT(0))
            }
            WM_COMMAND => {
                let id = i32::from(win::loword(wp.0));
                let code = win::hiword(wp.0);
                self.command(hwnd, id, code, HWND(lp.0 as *mut c_void));
                Some(LRESULT(0))
            }
            WM_VSCROLL => {
                self.on_vscroll(hwnd, u32::from(win::loword(wp.0)));
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                let delta = (win::hiword(wp.0) as i16) as i32;
                let step = win::scale(60, self.dpi.get());
                self.set_scroll(self.scroll.get() - delta * step / 120);
                Some(LRESULT(0))
            }
            WM_HSCROLL => {
                // The radius slider.
                let c = self.ctl.borrow();
                if HWND(lp.0 as *mut c_void) == c.radius {
                    let v = win::send(c.radius, TBM_GETPOS, 0, 0);
                    win::set_text(c.radius_val, &format!("{v} px"));
                }
                Some(LRESULT(0))
            }
            WM_DRAWITEM => {
                self.draw_item(lp);
                Some(LRESULT(1))
            }
            WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX => Some(self.ctl_color(msg, wp, lp)),
            WM_PAINT => {
                self.paint(hwnd);
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            DM_GETDEFID => Some(LRESULT((DC_HASDEFID << 16) | ID_SAVE as isize)),
            win::WM_MAILBOX => {
                self.on_mail();
                Some(LRESULT(0))
            }
            WM_CLOSE => {
                self.hide();
                Some(LRESULT(0))
            }
            WM_DPICHANGED => {
                let dpi = u32::from(win::loword(wp.0));
                self.dpi.set(dpi);
                // SAFETY: lParam points to the suggested RECT.
                let r = unsafe { &*(lp.0 as *const RECT) };
                win::move_to(hwnd, r.left, r.top, r.right - r.left, r.bottom - r.top);
                let (old, old_bold) = (self.font.replace(win::create_font(dpi, false)), self.font_bold.replace(win::create_font(dpi, true)));
                self.apply_fonts();
                win::delete_font(old);
                win::delete_font(old_bold);
                self.layout(hwnd);
                Some(LRESULT(0))
            }
            WM_SETTINGCHANGE => {
                self.apply_theme(hwnd);
                None
            }
            WM_DESTROY => {
                let chord = self.ctl.borrow().chord;
                // SAFETY: removes the subclass that on_create installed.
                unsafe {
                    let _ = RemoveWindowSubclass(chord, Some(capture_proc), 1);
                }
                win::delete_font(self.font.replace(Default::default()));
                win::delete_font(self.font_bold.replace(Default::default()));
                None
            }
            _ => None,
        }
    }
}

/// Subclass of the key chord edit control: while capturing, keys do not type text.
unsafe extern "system" fn capture_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _id: usize, data: usize) -> LRESULT {
    let this = &*(data as *const SettingsWin);
    if msg == WM_GETDLGCODE && this.capturing.get() {
        return LRESULT(DLGC_WANTALLKEYS);
    }
    let used = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| this.capture_message(msg, wp.0))).unwrap_or(false);
    if used {
        return LRESULT(0);
    }
    DefSubclassProc(hwnd, msg, wp, lp)
}

fn work_area_size(h: HWND) -> (i32, i32) {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
    // SAFETY: out-pointer to a local.
    unsafe {
        let mon = MonitorFromWindow(h, MONITOR_DEFAULTTOPRIMARY);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            (mi.rcWork.right - mi.rcWork.left, mi.rcWork.bottom - mi.rcWork.top)
        } else {
            (1280, 800)
        }
    }
}

// ---------------------------------------------------------------------------
// Saving (runs on a thread)
// ---------------------------------------------------------------------------

/// Write `text` to `path` through a temporary file (atomic on the same volume).
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Apply the form to the file. Returns whether the file changed. A file that
/// does not exist starts from the commented template. The two system
/// switches (start at sign-in, right-click menu) are applied here, too.
pub fn save_to_file(path: &Path, orig: &SettingsForm, new: &SettingsForm) -> Result<bool, String> {
    let current = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => zest_core::Config::default_toml().to_owned(),
        Err(e) => return Err(format!("Cannot read config.toml: {e}")),
    };
    let text = edit_config_text(&current, orig, new)?;
    let changed = text != current;
    if changed {
        write_atomic(path, &text).map_err(|e| format!("Cannot write config.toml: {e}"))?;
    }
    if new.autostart != orig.autostart || new.context_menu != orig.context_menu {
        let exe = std::env::current_exe().map_err(|e| format!("Cannot find zest.exe: {e}"))?;
        if new.autostart != orig.autostart {
            zest_shell::set_autostart(&exe, new.autostart).map_err(|e| format!("Start at sign-in: {e}"))?;
        }
        if new.context_menu != orig.context_menu {
            let r = if new.context_menu {
                zest_shell::install_context_menu(&exe).and(zest_shell::install_send_to(&exe))
            } else {
                zest_shell::uninstall_context_menu().and(zest_shell::uninstall_send_to())
            };
            r.map_err(|e| format!("Right-click menu: {e}"))?;
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zest_core::Config;

    #[test]
    fn saving_changes_the_file_and_keeps_its_comments() {
        let dir = std::env::temp_dir().join(format!("zest-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("Zest").join("config.toml");
        // No file yet: the template is the start.
        let orig = SettingsForm::from_config(&Config::default(), false, false);
        let mut new = orig.clone();
        new.outer_radius = 180;
        new.send_to_shelf = true;
        assert_eq!(save_to_file(&path, &orig, &new), Ok(true));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("outer_radius = 180               # logical pixels"), "{text}");
        assert!(text.contains("# Triggers open the wheel."));
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.wheel.outer_radius, 180);
        assert!(cfg.output.send_to_shelf);
        assert!(!dir.join("Zest").join("config.toml.tmp").exists());
        // The same form again: no write.
        let again = SettingsForm::from_config(&cfg, false, false);
        assert_eq!(save_to_file(&path, &again, &again), Ok(false));
        // A broken file is reported and left alone.
        std::fs::write(&path, "[wheel\n").unwrap();
        assert!(save_to_file(&path, &again, &new).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[wheel\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
