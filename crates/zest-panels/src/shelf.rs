//! The Shelf: a small floating window that holds paths. Drop files on it,
//! drag them out to another program. It never copies or moves a file.

use crate::model_shelf::{path_key, ShelfModel, ShelfState};
use crate::win::{self, Handler, Kind, Mailbox, Theme};
use crate::PanelHost;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use windows::core::{implement, w, PCWSTR};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, RECT, S_OK, WPARAM};
use windows::Win32::Graphics::Gdi::{FillRect, ScreenToClient, HDC, SetBkColor, SetTextColor};
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_NORMAL, FILE_FLAGS_AND_ATTRIBUTES};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, IBindCtx, IDataObject, CLSCTX_INPROC_SERVER};
use windows::Win32::System::Ole::{
    DoDragDrop, IDropSource, IDropSource_Impl, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK,
    DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Controls::{
    IImageList, NMHDR, NMLVCUSTOMDRAW, NMLVDISPINFOW, NMLVGETINFOTIPW, NMLVKEYDOWN, LVIF_IMAGE,
    LVIF_TEXT, NM_CUSTOMDRAW, NM_DBLCLK, NM_RCLICK, WC_LISTVIEWW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_DELETE, VK_RETURN};
use windows::Win32::UI::Shell::{
    BHID_DataObject, CLSID_DragDropHelper, IDragSourceHelper, SHCreateShellItemArrayFromIDLists, SHGetFileInfoW,
    SHGetImageList, SHParseDisplayName, SHFILEINFOW, SHGFI_SYSICONINDEX, SHGFI_USEFILEATTRIBUTES, SHIL_EXTRALARGE,
};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, DestroyWindow, GetCursorPos, GetWindowRect, IsWindowVisible,
    KillTimer, SetForegroundWindow, SetTimer, SetWindowPos, ShowWindow, TrackPopupMenu, HWND_TOPMOST,
    MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, MINMAXINFO, MENU_ITEM_FLAGS, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SW_HIDE, SW_SHOW, SW_SHOWNOACTIVATE, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_CLOSE, WM_COMMAND,
    WM_CREATE, WM_CTLCOLORBTN, WM_CTLCOLORSTATIC, WM_DPICHANGED, WM_ERASEBKGND, WM_EXITSIZEMOVE, WM_GETMINMAXINFO,
    WM_NOTIFY, WM_SETTINGCHANGE, WM_SIZE, WM_TIMER, WM_DRAWITEM,
};

const ID_LIST: i32 = 100;
const ID_CLEAR: i32 = 101;
const ID_CLOSE: i32 = 102;
const ID_COUNT: i32 = 103;

const CMD_OPEN: usize = 201;
const CMD_FOLDER: usize = 202;
const CMD_REMOVE: usize = 203;
const CMD_AFTER_DRAG: usize = 204;
const CMD_PRUNE: usize = 205;
const CMD_CLEAR: usize = 206;

const TIMER_SCAN: usize = 1;
const SCAN_MS: u32 = 4000;

// Window styles (raw values keep the types simple).
const WS_CAPTION: u32 = 0x00C0_0000;
const WS_SYSMENU: u32 = 0x0008_0000;
const WS_THICKFRAME: u32 = 0x0004_0000;
const WS_CLIPCHILDREN: u32 = 0x0200_0000;
const WS_CLIPSIBLINGS: u32 = 0x0400_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const WS_EX_TOOLWINDOW: u32 = 0x80;
const WS_EX_TOPMOST: u32 = 0x8;
const SS_ENDELLIPSIS: u32 = 0x4000;
const SS_CENTERIMAGE: u32 = 0x200;

pub const DEFAULT_W: i32 = 260;
pub const DEFAULT_H: i32 = 320;

enum Msg {
    /// Result of the background scan: (path, exists, icon index).
    Scanned(Vec<(PathBuf, bool, Option<i32>)>),
}

pub struct ShelfWin {
    hwnd: Cell<HWND>,
    host: Rc<dyn PanelHost>,
    model: RefCell<ShelfModel>,
    icons: RefCell<HashMap<String, i32>>,
    remove_after: Cell<bool>,
    rect: Cell<Option<[i32; 4]>>,
    theme: RefCell<Theme>,
    dpi: Cell<u32>,
    font: Cell<windows::Win32::Graphics::Gdi::HFONT>,
    list: Cell<HWND>,
    count: Cell<HWND>,
    clear: Cell<HWND>,
    close: Cell<HWND>,
    mail: Mailbox<Msg>,
    scanning: Cell<bool>,
    sink_registered: Cell<bool>,
    list_sink_registered: Cell<bool>,
}

impl ShelfWin {
    pub fn new(host: Rc<dyn PanelHost>, state: &ShelfState) -> Rc<ShelfWin> {
        Rc::new(ShelfWin {
            hwnd: Cell::new(HWND::default()),
            host,
            model: RefCell::new(ShelfModel::from_paths(state.paths())),
            icons: RefCell::new(HashMap::new()),
            remove_after: Cell::new(state.remove_after_drag),
            rect: Cell::new(state.rect),
            theme: RefCell::new(Theme::new(win::system_uses_dark())),
            dpi: Cell::new(96),
            font: Cell::new(Default::default()),
            list: Cell::new(HWND::default()),
            count: Cell::new(HWND::default()),
            clear: Cell::new(HWND::default()),
            close: Cell::new(HWND::default()),
            mail: Mailbox::default(),
            scanning: Cell::new(false),
            sink_registered: Cell::new(false),
            list_sink_registered: Cell::new(false),
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

    /// What `state.json` keeps.
    pub fn state(&self) -> ShelfState {
        ShelfState {
            items: self.model.borrow().paths().iter().map(|p| p.to_string_lossy().into_owned()).collect(),
            rect: self.rect.get(),
            remove_after_drag: self.remove_after.get(),
        }
    }

    fn persist(&self) {
        self.host.shelf_changed(self.state());
    }

    // ----- show / hide ---------------------------------------------------------

    pub fn show(self: &Rc<Self>, activate: bool) {
        if self.hwnd.get().is_invalid() && !self.create() {
            return;
        }
        let h = self.hwnd.get();
        // SAFETY: handle of our own window.
        unsafe {
            let _ = ShowWindow(h, if activate { SW_SHOW } else { SW_SHOWNOACTIVATE });
            let _ = SetWindowPos(h, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            if activate {
                let _ = SetForegroundWindow(h);
            }
            SetTimer(h, TIMER_SCAN, SCAN_MS, None);
        }
        self.scan();
    }

    pub fn hide(&self) {
        let h = self.hwnd.get();
        if h.is_invalid() {
            return;
        }
        self.save_rect();
        // SAFETY: handle of our own window.
        unsafe {
            let _ = KillTimer(h, TIMER_SCAN);
            let _ = ShowWindow(h, SW_HIDE);
        }
        self.persist();
    }

    pub fn destroy(&self) {
        let h = self.hwnd.get();
        if h.is_invalid() {
            return;
        }
        if self.list_sink_registered.replace(false) {
            zest_shell::revoke_drop_target(self.list.get());
        }
        if self.sink_registered.replace(false) {
            zest_shell::revoke_drop_target(h);
        }
        // SAFETY: our own window; WM_DESTROY frees the font.
        unsafe {
            let _ = DestroyWindow(h);
        }
        self.hwnd.set(HWND::default());
    }

    fn save_rect(&self) {
        let h = self.hwnd.get();
        let mut r = RECT::default();
        // SAFETY: out-pointer to a local.
        if h.is_invalid() || unsafe { GetWindowRect(h, &mut r) }.is_err() {
            return;
        }
        self.rect.set(Some([r.left, r.top, r.right - r.left, r.bottom - r.top]));
    }

    fn create(self: &Rc<Self>) -> bool {
        let style = WS_CAPTION | WS_SYSMENU | WS_THICKFRAME | WS_CLIPCHILDREN | WS_CLIPSIBLINGS;
        let Some(h) = win::create_window(
            Rc::as_ptr(self),
            "ZestShell_Shelf",
            "Shelf",
            style,
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            (0, 0, 300, 360),
            None,
        ) else {
            tracing::error!("cannot create the Shelf window");
            return false;
        };
        self.hwnd.set(h);
        self.mail.set_hwnd(h);
        // Size and place: the saved rectangle if it is still on a screen, else bottom-right.
        let dpi = win::window_dpi(h);
        self.dpi.set(dpi);
        let (w, hh) = (win::scale(DEFAULT_W, dpi), win::scale(DEFAULT_H, dpi));
        let r = match self.rect.get().filter(|r| r[2] > 100 && r[3] > 100 && win::rect_on_screen(*r)) {
            Some(r) => r,
            None => {
                let (x, y) = win::bottom_right_rect(w, hh, win::scale(16, dpi));
                [x, y, w, hh]
            }
        };
        win::move_to(h, r[0], r[1], r[2], r[3]);
        self.rect.set(Some(r));
        // Drop target (COPY only, never MOVE).
        let sink: Rc<dyn zest_shell::DropSink> = Rc::new(ShelfSink { win: Rc::downgrade(self) });
        match zest_shell::register_drop_target(h, Rc::clone(&sink)) {
            Ok(()) => self.sink_registered.set(true),
            Err(e) => tracing::warn!("Shelf drop target: {e}"),
        }
        // The list covers most of the window: it takes drops, too.
        if zest_shell::register_drop_target(self.list.get(), sink).is_ok() {
            self.list_sink_registered.set(true);
        }
        true
    }

    // ----- items --------------------------------------------------------------

    pub fn add(self: &Rc<Self>, paths: Vec<PathBuf>) -> usize {
        // A first guess for the icon needs no disk access.
        let added = {
            let mut icons = self.icons.borrow_mut();
            for p in &paths {
                icons.entry(path_key(p)).or_insert_with(|| quick_icon(p));
            }
            drop(icons);
            self.model.borrow_mut().add(paths)
        };
        if added > 0 {
            self.refresh_list();
            self.persist();
            if self.is_visible() {
                self.scan();
            }
        }
        added
    }

    pub fn remove_items(&self, paths: &[PathBuf]) {
        if self.model.borrow_mut().remove_paths(paths) > 0 {
            self.refresh_list();
            self.persist();
        }
    }

    fn remove_selected(&self) {
        let sel = win::lv_selected(self.list.get());
        if sel.is_empty() {
            return;
        }
        self.model.borrow_mut().remove_indices(&sel);
        self.refresh_list();
        self.persist();
    }

    fn selected_paths(&self) -> Vec<PathBuf> {
        let m = self.model.borrow();
        win::lv_selected(self.list.get()).into_iter().filter_map(|i| m.get(i).map(|x| x.path.clone())).collect()
    }

    fn refresh_list(&self) {
        let (n, title, count) = {
            let m = self.model.borrow();
            (m.len(), m.title(), m.count_text())
        };
        let h = self.hwnd.get();
        if h.is_invalid() {
            return;
        }
        win::lv_set_count(self.list.get(), n);
        win::set_text(h, &title);
        win::set_text(self.count.get(), &count);
        win::set_text_enabled(self.clear.get(), n > 0);
    }

    /// Check for missing items and find the real icons, off the UI thread.
    fn scan(&self) {
        if self.scanning.replace(true) {
            return;
        }
        let paths = self.model.borrow().paths();
        let mail = self.mail.clone();
        win::spawn("zest-shelf-scan", move || {
            let mut out = Vec::with_capacity(paths.len());
            for p in paths {
                let meta = std::fs::metadata(&p);
                let exists = meta.is_ok();
                let icon = if exists { real_icon(&p) } else { None };
                out.push((p, exists, icon));
            }
            mail.post(Msg::Scanned(out));
        });
    }

    fn on_mail(&self) {
        for m in self.mail.drain() {
            match m {
                Msg::Scanned(list) => {
                    self.scanning.set(false);
                    let mut changed = false;
                    {
                        let mut icons = self.icons.borrow_mut();
                        for (p, _, icon) in &list {
                            if let Some(i) = icon {
                                changed |= icons.insert(path_key(p), *i) != Some(*i);
                            }
                        }
                    }
                    let missing: Vec<PathBuf> = list.iter().filter(|x| !x.1).map(|x| x.0.clone()).collect();
                    let mut model = self.model.borrow_mut();
                    // Items added while the scan ran are not in `list`: leave their flag alone.
                    let scanned: Vec<String> = list.iter().map(|x| path_key(&x.0)).collect();
                    changed |= model.refresh_missing_for(&scanned, &missing);
                    drop(model);
                    if changed {
                        self.refresh_list();
                    }
                }
            }
        }
    }

    // ----- drag out -----------------------------------------------------------

    fn begin_drag(&self) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        let Ok(data) = data_object_for(&paths) else {
            tracing::warn!("Shelf: cannot make a data object for the drag");
            return;
        };
        // Drag picture (best effort).
        // SAFETY: COM calls on the UI thread; every failure is ignored.
        unsafe {
            if let Ok(helper) = CoCreateInstance::<_, IDragSourceHelper>(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER) {
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let list = self.list.get();
                let _ = ScreenToClient(list, &mut pt);
                let _ = helper.InitializeFromWindow(list, Some(&pt), &data);
            }
        }
        let source: IDropSource = DropSource.into();
        let mut effect = DROPEFFECT_NONE;
        // The target decides: copy, move or link. Nothing is held (borrowed) here, because
        // this call runs a nested message loop.
        // SAFETY: valid COM objects; the loop ends when the button is released.
        let hr = unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY | DROPEFFECT_LINK, &mut effect) };
        let dropped = hr.0 == 0x0004_0100 && effect != DROPEFFECT_NONE; // DRAGDROP_S_DROP
        tracing::debug!(items = paths.len(), dropped, effect = effect.0, "shelf drag out");
        if dropped && self.remove_after.get() {
            self.model.borrow_mut().remove_paths(&paths);
            self.refresh_list();
            self.persist();
        }
    }

    /// Esc hides the Shelf; Tab and the arrow keys work as in a dialog.
    pub fn pre_translate(&self, msg: &windows::Win32::UI::WindowsAndMessaging::MSG) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::WM_KEYDOWN;
        let h = self.hwnd.get();
        if h.is_invalid() {
            return false;
        }
        if msg.message == WM_KEYDOWN && msg.wParam.0 == 0x1B {
            self.hide();
            return true;
        }
        // SAFETY: valid window and message.
        unsafe { windows::Win32::UI::WindowsAndMessaging::IsDialogMessageW(h, msg).as_bool() }
    }

    // ----- window procedure -----------------------------------------------------

    fn on_create(&self, hwnd: HWND) {
        let dpi = win::window_dpi(hwnd);
        self.dpi.set(dpi);
        self.font.set(win::create_font(dpi, false));
        let list_style = win::LVS_REPORT | win::LVS_OWNERDATA | win::LVS_NOCOLUMNHEADER | win::LVS_SHOWSELALWAYS | win::LVS_SHAREIMAGELISTS | WS_TABSTOP;
        let list = win::create_child(hwnd, WC_LISTVIEWW, "", list_style, 0, ID_LIST);
        win::send(
            list,
            win::LVM_SETEXTENDEDLISTVIEWSTYLE,
            0,
            win::LVS_EX_FULLROWSELECT | win::LVS_EX_DOUBLEBUFFER | win::LVS_EX_INFOTIP,
        );
        win::lv_add_column(list, 0, "Name", 200);
        attach_icons(list);
        self.list.set(list);
        let count = win::create_child(hwnd, w!("STATIC"), "", SS_ENDELLIPSIS | SS_CENTERIMAGE, 0, ID_COUNT);
        self.count.set(count);
        self.clear.set(win::create_child(hwnd, w!("BUTTON"), "Clear", WS_TABSTOP, 0, ID_CLEAR));
        self.close.set(win::create_child(hwnd, w!("BUTTON"), "\u{D7}", WS_TABSTOP, 0, ID_CLOSE));
        let tt = win::tooltip_create(hwnd);
        win::tooltip_add(tt, hwnd, self.clear.get(), "Remove all items from the Shelf (the files stay where they are)");
        win::tooltip_add(tt, hwnd, self.close.get(), "Hide the Shelf");
        self.apply_fonts();
        self.apply_theme(hwnd);
        self.layout(hwnd);
        self.refresh_list();
    }

    fn apply_fonts(&self) {
        for h in [self.list.get(), self.count.get(), self.clear.get(), self.close.get()] {
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
        win::lv_apply_theme(self.list.get(), t.dark, &t);
        win::theme_control(self.clear.get(), Kind::Button, t.dark);
        win::theme_control(self.close.get(), Kind::Button, t.dark);
        win::invalidate(hwnd);
    }

    fn layout(&self, hwnd: HWND) {
        let dpi = self.dpi.get();
        let (w, h) = win::client_size(hwnd);
        let bar = win::scale(40, dpi);
        let pad = win::scale(8, dpi);
        let btn_h = win::scale(26, dpi);
        let y = (bar - btn_h) / 2;
        let close_w = win::scale(30, dpi);
        let clear_w = win::scale(64, dpi);
        win::move_to(self.close.get(), w - pad - close_w, y, close_w, btn_h);
        win::move_to(self.clear.get(), w - pad - close_w - pad / 2 - clear_w, y, clear_w, btn_h);
        win::move_to(self.count.get(), pad, 0, (w - pad * 3 - close_w - clear_w).max(10), bar);
        win::move_to(self.list.get(), 0, bar, w, (h - bar).max(10));
        win::send(self.list.get(), win::LVM_SETCOLUMNWIDTH, 0, (w - win::scale(4, dpi)).max(50) as isize);
    }

    fn context_menu(&self, hwnd: HWND) {
        let sel = !win::lv_selected(self.list.get()).is_empty();
        let (n, missing) = {
            let m = self.model.borrow();
            (m.len(), m.missing_count())
        };
        let flag = |on: bool| if on { MF_STRING } else { MENU_ITEM_FLAGS(MF_STRING.0 | MF_GRAYED.0) };
        // SAFETY: standard popup menu on the UI thread.
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let _ = AppendMenuW(menu, flag(sel), CMD_OPEN, w!("Open"));
            let _ = AppendMenuW(menu, flag(sel), CMD_FOLDER, w!("Show in folder"));
            let _ = AppendMenuW(menu, flag(sel), CMD_REMOVE, w!("Remove from Shelf"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let after = if self.remove_after.get() { MENU_ITEM_FLAGS(MF_STRING.0 | MF_CHECKED.0) } else { MF_STRING };
            let _ = AppendMenuW(menu, after, CMD_AFTER_DRAG, w!("Remove after drag out"));
            let _ = AppendMenuW(menu, flag(missing > 0), CMD_PRUNE, w!("Remove missing items"));
            let _ = AppendMenuW(menu, flag(n > 0), CMD_CLEAR, w!("Clear"));
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = SetForegroundWindow(hwnd);
            let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, None);
            let _ = DestroyMenu(menu);
            self.command(hwnd, cmd.0 as usize);
        }
    }

    fn command(&self, hwnd: HWND, id: usize) {
        match id {
            x if x == ID_CLEAR as usize || x == CMD_CLEAR => {
                self.model.borrow_mut().clear();
                self.refresh_list();
                self.persist();
            }
            x if x == ID_CLOSE as usize => self.hide(),
            CMD_OPEN => self.selected_paths().into_iter().for_each(win::shell_open_async),
            CMD_FOLDER => {
                if let Some(p) = self.selected_paths().into_iter().next() {
                    win::show_in_folder_async(p);
                }
            }
            CMD_REMOVE => self.remove_selected(),
            CMD_AFTER_DRAG => {
                self.remove_after.set(!self.remove_after.get());
                self.persist();
            }
            CMD_PRUNE => {
                self.model.borrow_mut().prune_missing();
                self.refresh_list();
                self.persist();
            }
            _ => {}
        }
        let _ = hwnd;
    }

    fn notify(&self, hwnd: HWND, lp: LPARAM) -> Option<LRESULT> {
        // SAFETY: WM_NOTIFY carries a valid NMHDR (and the larger struct its code names).
        let hdr = unsafe { &*(lp.0 as *const NMHDR) };
        if hdr.hwndFrom != self.list.get() {
            return None;
        }
        match hdr.code {
            c if c == NM_DBLCLK => {
                self.selected_paths().into_iter().for_each(win::shell_open_async);
                Some(LRESULT(0))
            }
            c if c == NM_RCLICK => {
                self.context_menu(hwnd);
                Some(LRESULT(0))
            }
            c if c == windows::Win32::UI::Controls::LVN_BEGINDRAG => {
                self.begin_drag();
                Some(LRESULT(0))
            }
            c if c == windows::Win32::UI::Controls::LVN_KEYDOWN => {
                // SAFETY: LVN_KEYDOWN carries an NMLVKEYDOWN.
                let k = unsafe { &*(lp.0 as *const NMLVKEYDOWN) };
                // SAFETY: plain key state query.
                let ctrl = unsafe { GetKeyState(i32::from(VK_CONTROL.0)) } < 0;
                let vkey = k.wVKey;
                match vkey {
                    v if v == VK_DELETE.0 => self.remove_selected(),
                    v if v == VK_RETURN.0 => self.selected_paths().into_iter().for_each(win::shell_open_async),
                    0x41 if ctrl => win::lv_select_all(self.list.get()),
                    _ => {}
                }
                Some(LRESULT(0))
            }
            c if c == windows::Win32::UI::Controls::LVN_GETDISPINFOW => {
                // SAFETY: LVN_GETDISPINFOW carries an NMLVDISPINFOW.
                let di = unsafe { &mut *(lp.0 as *mut NMLVDISPINFOW) };
                let m = self.model.borrow();
                if let Some(item) = usize::try_from(di.item.iItem).ok().and_then(|i| m.get(i)) {
                    if di.item.mask.0 & LVIF_TEXT.0 != 0 && !di.item.pszText.is_null() && di.item.cchTextMax > 0 {
                        let mut buf: Vec<u16> = item.name.encode_utf16().collect();
                        buf.truncate(di.item.cchTextMax as usize - 1);
                        buf.push(0);
                        // SAFETY: pszText has room for cchTextMax characters.
                        unsafe { std::ptr::copy_nonoverlapping(buf.as_ptr(), di.item.pszText.0, buf.len()) };
                    }
                    if di.item.mask.0 & LVIF_IMAGE.0 != 0 {
                        di.item.iImage = self.icons.borrow().get(&path_key(&item.path)).copied().unwrap_or(0);
                    }
                }
                Some(LRESULT(0))
            }
            c if c == windows::Win32::UI::Controls::LVN_GETINFOTIPW => {
                // SAFETY: LVN_GETINFOTIP carries an NMLVGETINFOTIPW.
                let tip = unsafe { &mut *(lp.0 as *mut NMLVGETINFOTIPW) };
                let m = self.model.borrow();
                if let Some(item) = usize::try_from(tip.iItem).ok().and_then(|i| m.get(i)) {
                    let mut text = item.path.to_string_lossy().into_owned();
                    if item.missing {
                        text.push_str("\r\n(missing)");
                    }
                    let mut buf: Vec<u16> = text.encode_utf16().collect();
                    buf.truncate((tip.cchTextMax.max(1) as usize) - 1);
                    buf.push(0);
                    // SAFETY: pszText has room for cchTextMax characters.
                    unsafe { std::ptr::copy_nonoverlapping(buf.as_ptr(), tip.pszText.0, buf.len()) };
                }
                Some(LRESULT(0))
            }
            c if c == NM_CUSTOMDRAW => {
                // SAFETY: NM_CUSTOMDRAW of a list view carries an NMLVCUSTOMDRAW.
                let cd = unsafe { &mut *(lp.0 as *mut NMLVCUSTOMDRAW) };
                const CDDS_PREPAINT: u32 = 1;
                const CDDS_ITEMPREPAINT: u32 = 0x10001;
                const CDRF_NOTIFYITEMDRAW: isize = 0x20;
                match cd.nmcd.dwDrawStage.0 {
                    s if s == CDDS_PREPAINT => Some(LRESULT(CDRF_NOTIFYITEMDRAW)),
                    s if s == CDDS_ITEMPREPAINT => {
                        let gone = self.model.borrow().get(cd.nmcd.dwItemSpec).is_some_and(|i| i.missing);
                        if gone {
                            cd.clrText = self.theme.borrow().muted;
                        }
                        Some(LRESULT(0))
                    }
                    _ => Some(LRESULT(0)),
                }
            }
            _ => None,
        }
    }
}

impl Handler for ShelfWin {
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
                mm.ptMinTrackSize = POINT { x: win::scale(200, self.dpi.get()), y: win::scale(200, self.dpi.get()) };
                Some(LRESULT(0))
            }
            WM_COMMAND => {
                self.command(hwnd, win::loword(wp.0) as usize);
                Some(LRESULT(0))
            }
            WM_NOTIFY => self.notify(hwnd, lp),
            win::WM_MAILBOX => {
                self.on_mail();
                Some(LRESULT(0))
            }
            WM_TIMER if wp.0 == TIMER_SCAN => {
                self.scan();
                Some(LRESULT(0))
            }
            WM_CLOSE => {
                self.hide();
                Some(LRESULT(0))
            }
            WM_EXITSIZEMOVE => {
                self.save_rect();
                self.persist();
                Some(LRESULT(0))
            }
            WM_DPICHANGED => {
                let dpi = u32::from(win::loword(wp.0));
                self.dpi.set(dpi);
                // SAFETY: lParam points to the suggested RECT.
                let r = unsafe { &*(lp.0 as *const RECT) };
                win::move_to(hwnd, r.left, r.top, r.right - r.left, r.bottom - r.top);
                let old = self.font.replace(win::create_font(dpi, false));
                self.apply_fonts();
                win::delete_font(old);
                self.layout(hwnd);
                Some(LRESULT(0))
            }
            WM_SETTINGCHANGE => {
                self.apply_theme(hwnd);
                None
            }
            WM_ERASEBKGND => {
                let mut r = RECT::default();
                // SAFETY: wParam is the HDC of this paint; out-pointer to a local.
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut r);
                    FillRect(HDC(wp.0 as *mut c_void), &r, self.theme.borrow().bg_brush);
                }
                Some(LRESULT(1))
            }
            WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
                let t = self.theme.borrow();
                // SAFETY: wParam is the HDC of the control.
                unsafe {
                    SetTextColor(HDC(wp.0 as *mut c_void), t.fg);
                    SetBkColor(HDC(wp.0 as *mut c_void), t.bg);
                }
                Some(LRESULT(t.bg_brush.0 as isize))
            }
            WM_DRAWITEM => None,
            windows::Win32::UI::WindowsAndMessaging::WM_DESTROY => {
                win::delete_font(self.font.replace(Default::default()));
                None
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Drop target sink
// ---------------------------------------------------------------------------

struct ShelfSink {
    win: Weak<ShelfWin>,
}

impl zest_shell::DropSink for ShelfSink {
    fn drag_enter(&self, files: &[PathBuf]) -> bool {
        !files.is_empty()
    }

    fn drag_over(&self, _x: i32, _y: i32) -> bool {
        true
    }

    fn drag_leave(&self) {}

    fn drop_files(&self, files: &[PathBuf], _x: i32, _y: i32) -> bool {
        match self.win.upgrade() {
            Some(w) => {
                w.add(files.to_vec());
                true
            }
            None => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Drag source
// ---------------------------------------------------------------------------

#[implement(IDropSource)]
struct DropSource;

#[allow(non_snake_case)]
impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(&self, fescapepressed: BOOL, grfkeystate: MODIFIERKEYS_FLAGS) -> windows::core::HRESULT {
        const DRAGDROP_S_DROP: windows::core::HRESULT = windows::core::HRESULT(0x0004_0100);
        const DRAGDROP_S_CANCEL: windows::core::HRESULT = windows::core::HRESULT(0x0004_0101);
        if fescapepressed.as_bool() {
            DRAGDROP_S_CANCEL
        } else if grfkeystate.0 & MK_LBUTTON.0 == 0 {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> windows::core::HRESULT {
        // DRAGDROP_S_USEDEFAULTCURSORS
        windows::core::HRESULT(0x0004_0102)
    }
}

/// A data object that the shell makes for these paths (the same one Explorer
/// gives when you drag them there): the target decides copy, move or link.
fn data_object_for(paths: &[PathBuf]) -> windows::core::Result<IDataObject> {
    let mut pidls: Vec<*mut ITEMIDLIST> = Vec::new();
    for p in paths {
        let w = win::wide_path(p);
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        // SAFETY: NUL-terminated path; out-pointer to a local.
        let ok = unsafe { SHParseDisplayName(PCWSTR(w.as_ptr()), None::<&IBindCtx>, &mut pidl, 0, None) }.is_ok();
        if ok && !pidl.is_null() {
            pidls.push(pidl);
        }
    }
    if pidls.is_empty() {
        return Err(windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL));
    }
    let consts: Vec<*const ITEMIDLIST> = pidls.iter().map(|p| *p as *const ITEMIDLIST).collect();
    // SAFETY: the PIDLs stay valid until freed below.
    let result = unsafe {
        SHCreateShellItemArrayFromIDLists(&consts)
            .and_then(|arr| arr.BindToHandler::<_, IDataObject>(None::<&IBindCtx>, &BHID_DataObject))
    };
    for p in pidls {
        // SAFETY: allocated by SHParseDisplayName.
        unsafe { CoTaskMemFree(Some(p as *const c_void)) };
    }
    result
}

// ---------------------------------------------------------------------------
// Icons
// ---------------------------------------------------------------------------

fn file_info(path: &Path, flags: windows::Win32::UI::Shell::SHGFI_FLAGS, attrs: FILE_FLAGS_AND_ATTRIBUTES) -> Option<i32> {
    let w = win::wide_path(path);
    let mut sfi = SHFILEINFOW::default();
    // SAFETY: NUL-terminated path; out-struct is a local.
    let r = unsafe {
        SHGetFileInfoW(PCWSTR(w.as_ptr()), attrs, Some(&mut sfi), std::mem::size_of::<SHFILEINFOW>() as u32, flags)
    };
    (r != 0).then_some(sfi.iIcon)
}

/// Icon from the file name only (no disk access).
fn quick_icon(path: &Path) -> i32 {
    file_info(path, SHGFI_SYSICONINDEX | SHGFI_USEFILEATTRIBUTES, FILE_ATTRIBUTE_NORMAL).unwrap_or(0)
}

/// The icon the shell shows for the real item (reads the disk: call off the UI thread).
fn real_icon(path: &Path) -> Option<i32> {
    file_info(path, SHGFI_SYSICONINDEX, FILE_FLAGS_AND_ATTRIBUTES(0))
}

/// The 48 px system image list becomes the small image list of the list view.
fn attach_icons(list: HWND) {
    // SAFETY: SHGetImageList fills the out-pointer with an IImageList; the list view only borrows it
    // (LVS_SHAREIMAGELISTS), and the system keeps the list alive.
    unsafe {
        if let Ok(il) = SHGetImageList::<IImageList>(SHIL_EXTRALARGE as i32) {
            let raw = windows::core::Interface::as_raw(&il);
            win::send(list, win::LVM_SETIMAGELIST, win::LVSIL_SMALL, raw as isize);
            std::mem::forget(il);
        }
    }
}
