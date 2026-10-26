//! The History window (the "job panel"): the last jobs, live progress,
//! Re-run, Retry failed, Undo (Recycle Bin only), Cancel, taskbar progress.

use crate::model_history::{
    cell_text, error_text, plan_rerun, HistoryModel, JobEntry, JobStatus, RerunError, RerunPlan, TaskbarState,
    COLUMNS,
};
use crate::win::{self, Handler, Kind, Mailbox, Theme};
use crate::PanelHost;
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::path::PathBuf;
use std::rc::Rc;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{FillRect, SetBkColor, SetTextColor, HDC};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::Input::KeyboardAndMouse::GetFocus;
use windows::Win32::UI::Controls::{
    NMHDR, NMLVCUSTOMDRAW, NMLVDISPINFOW, LVIF_TEXT, NM_CUSTOMDRAW, NM_DBLCLK, NM_RCLICK, WC_LISTVIEWW,
};
use windows::Win32::UI::Shell::{
    ITaskbarList3, TaskbarList, TBPF_ERROR, TBPF_INDETERMINATE, TBPF_NOPROGRESS, TBPF_NORMAL,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, DestroyWindow, GetCursorPos, IsWindowVisible,
    RegisterWindowMessageW, SetForegroundWindow, ShowWindow, TrackPopupMenu, MENU_ITEM_FLAGS, MF_GRAYED,
    MF_SEPARATOR, MF_STRING, MINMAXINFO, SW_HIDE, SW_RESTORE, SW_SHOW, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_ACTIVATE,
    WM_CLOSE, WM_COMMAND, WM_CREATE, WM_CTLCOLOREDIT, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND,
    WM_GETMINMAXINFO, WM_NOTIFY, WM_SETTINGCHANGE, WM_SIZE,
};

const ID_LIST: i32 = 100;
const ID_SEARCH: i32 = 101;
const ID_FOLDER: i32 = 110;
const ID_OPEN: i32 = 111;
const ID_RERUN: i32 = 112;
const ID_RETRY: i32 = 113;
const ID_UNDO: i32 = 114;
const ID_CANCEL: i32 = 115;
const ID_COPYERR: i32 = 116;

const WS_OVERLAPPEDWINDOW: u32 = 0x00CF_0000;
const WS_CLIPCHILDREN: u32 = 0x0200_0000;
const WS_TABSTOP: u32 = 0x0001_0000;
const LVS_SINGLESEL: u32 = 4;
const ES_AUTOHSCROLL: u32 = 0x80;
const EM_SETCUEBANNER: u32 = 0x1501;
const EN_CHANGE: u16 = 0x0300;
const CW_USEDEFAULT: i32 = i32::MIN;
const LVN_ITEMCHANGED: u32 = (-101i32) as u32;

enum Msg {
    Plan(Result<RerunPlan, RerunError>),
    Undone { id: u64, recycled: Vec<PathBuf>, first_error: Option<String> },
}

pub struct HistoryWin {
    hwnd: Cell<HWND>,
    host: Rc<dyn PanelHost>,
    model: RefCell<HistoryModel>,
    visible: RefCell<Vec<usize>>,
    query: RefCell<String>,
    theme: RefCell<Theme>,
    dpi: Cell<u32>,
    font: Cell<windows::Win32::Graphics::Gdi::HFONT>,
    list: Cell<HWND>,
    search: Cell<HWND>,
    buttons: RefCell<Vec<(i32, HWND)>>,
    loaded: Cell<bool>,
    mail: Mailbox<Msg>,
    tz_offset: Cell<i32>,
    taskbar: RefCell<Option<ITaskbarList3>>,
    taskbar_msg: u32,
    taskbar_ready: Cell<bool>,
    last_taskbar: Cell<Option<TaskbarState>>,
}

fn now_utc() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl HistoryWin {
    pub fn new(host: Rc<dyn PanelHost>) -> Rc<HistoryWin> {
        // SAFETY: registers a window message name.
        let taskbar_msg = unsafe { RegisterWindowMessageW(w!("TaskbarButtonCreated")) };
        Rc::new(HistoryWin {
            hwnd: Cell::new(HWND::default()),
            host,
            model: RefCell::new(HistoryModel::default()),
            visible: RefCell::new(Vec::new()),
            query: RefCell::new(String::new()),
            theme: RefCell::new(Theme::new(win::system_uses_dark())),
            dpi: Cell::new(96),
            font: Cell::new(Default::default()),
            list: Cell::new(HWND::default()),
            search: Cell::new(HWND::default()),
            buttons: RefCell::new(Vec::new()),
            loaded: Cell::new(false),
            mail: Mailbox::default(),
            tz_offset: Cell::new(0),
            taskbar: RefCell::new(None),
            taskbar_msg,
            taskbar_ready: Cell::new(false),
            last_taskbar: Cell::new(None),
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

    pub fn show(self: &Rc<Self>) {
        if self.hwnd.get().is_invalid() && !self.create() {
            return;
        }
        let h = self.hwnd.get();
        // SAFETY: handle of our own window.
        unsafe {
            let _ = ShowWindow(h, SW_RESTORE);
            let _ = ShowWindow(h, SW_SHOW);
            let _ = SetForegroundWindow(h);
        }
        if !self.loaded.replace(true) {
            self.host.history_load();
        }
        self.refresh();
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
        self.update_taskbar();
    }

    pub fn destroy(&self) {
        let h = self.hwnd.get();
        if h.is_invalid() {
            return;
        }
        *self.taskbar.borrow_mut() = None;
        // SAFETY: our own window.
        unsafe {
            let _ = DestroyWindow(h);
        }
        self.hwnd.set(HWND::default());
    }

    fn create(self: &Rc<Self>) -> bool {
        let Some(h) = win::create_window(
            Rc::as_ptr(self),
            "ZestShell_History",
            "History",
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            0,
            (CW_USEDEFAULT, CW_USEDEFAULT, 900, 540),
            None,
        ) else {
            tracing::error!("cannot create the History window");
            return false;
        };
        self.hwnd.set(h);
        self.mail.set_hwnd(h);
        let dpi = win::window_dpi(h);
        self.dpi.set(dpi);
        win::move_to_size(h, win::scale(900, dpi), win::scale(540, dpi));
        self.layout(h);
        true
    }

    // ----- feed from the app -------------------------------------------------------

    pub fn loaded(&self, entries: Vec<JobEntry>) {
        self.model.borrow_mut().merge_loaded(entries);
        self.refresh();
    }

    pub fn job_started(&self, e: JobEntry) {
        self.model.borrow_mut().start(e);
        self.refresh();
    }

    pub fn job_progress(&self, id: u64, fraction: Option<f32>) {
        let changed = self.model.borrow_mut().progress(id, fraction);
        if !changed || self.hwnd.get().is_invalid() {
            return;
        }
        let pos = {
            let m = self.model.borrow();
            self.visible.borrow().iter().position(|&i| m.entries().get(i).is_some_and(|e| e.id == id))
        };
        if let Some(p) = pos {
            win::send(self.list.get(), win::LVM_REDRAWITEMS, p, p as isize);
        }
        self.update_taskbar();
    }

    pub fn job_finished(&self, id: u64, outputs: Vec<PathBuf>, errors: Vec<(PathBuf, String)>, cancelled: bool) {
        self.model.borrow_mut().finish(id, outputs, errors, cancelled);
        self.refresh();
    }

    // ----- list ------------------------------------------------------------------

    fn selected_entry(&self) -> Option<JobEntry> {
        let sel = win::lv_selected(self.list.get());
        let idx = *self.visible.borrow().get(*sel.first()?)?;
        self.model.borrow().entries().get(idx).cloned()
    }

    fn refresh(&self) {
        if self.hwnd.get().is_invalid() {
            self.update_taskbar();
            return;
        }
        let sel_id = self.selected_entry().map(|e| e.id);
        let q = self.query.borrow().clone();
        let vis = self.model.borrow().visible(&q);
        self.tz_offset.set(win::local_offset_minutes());
        *self.visible.borrow_mut() = vis.clone();
        win::lv_set_count(self.list.get(), vis.len());
        if let Some(id) = sel_id {
            let pos = {
                let m = self.model.borrow();
                vis.iter().position(|&i| m.entries().get(i).is_some_and(|e| e.id == id))
            };
            if let Some(p) = pos {
                win::lv_select_only(self.list.get(), p);
            }
        }
        self.update_buttons();
        self.update_taskbar();
    }

    fn update_buttons(&self) {
        let e = self.selected_entry();
        let on = |f: &dyn Fn(&JobEntry) -> bool| e.as_ref().is_some_and(f);
        for (id, h) in self.buttons.borrow().iter() {
            let enabled = match *id {
                ID_FOLDER => on(&|e| !e.outputs.is_empty() || !e.inputs.is_empty()),
                ID_OPEN => on(&|e| !e.outputs.is_empty() && e.status != JobStatus::Undone),
                ID_RERUN => on(&|e| e.can_rerun()),
                ID_RETRY => on(&|e| e.can_retry_failed()),
                ID_UNDO => on(&|e| e.can_undo()),
                ID_CANCEL => on(&|e| e.can_cancel()),
                ID_COPYERR => on(&|e| e.failed()),
                _ => true,
            };
            win::set_text_enabled(*h, enabled);
        }
    }

    // ----- taskbar -----------------------------------------------------------------

    fn update_taskbar(&self) {
        let h = self.hwnd.get();
        if h.is_invalid() {
            return;
        }
        let state = if self.is_visible() { self.model.borrow().taskbar() } else { TaskbarState::None };
        if self.last_taskbar.get() == Some(state) {
            return;
        }
        if self.taskbar.borrow().is_none() {
            // SAFETY: COM call on the UI thread (STA).
            let tb: Option<ITaskbarList3> = unsafe { CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER) }.ok();
            if let Some(t) = &tb {
                // SAFETY: freshly created interface.
                let _ = unsafe { t.HrInit() };
            }
            *self.taskbar.borrow_mut() = tb;
        }
        let tb = self.taskbar.borrow();
        let Some(tb) = tb.as_ref() else { return };
        // SAFETY: valid ITaskbarList3 and window handle.
        unsafe {
            match state {
                TaskbarState::None => {
                    let _ = tb.SetProgressState(h, TBPF_NOPROGRESS);
                }
                TaskbarState::Indeterminate => {
                    let _ = tb.SetProgressState(h, TBPF_INDETERMINATE);
                }
                TaskbarState::Normal(p) => {
                    let _ = tb.SetProgressState(h, TBPF_NORMAL);
                    let _ = tb.SetProgressValue(h, u64::from(p), 100);
                }
                TaskbarState::Error(p) => {
                    let _ = tb.SetProgressState(h, TBPF_ERROR);
                    let _ = tb.SetProgressValue(h, u64::from(p), 100);
                }
            }
        }
        self.last_taskbar.set(Some(state));
    }

    // ----- actions -----------------------------------------------------------------

    fn show_in_folder(&self) {
        let Some(e) = self.selected_entry() else { return };
        let target = if e.status != JobStatus::Undone { e.outputs.first() } else { None };
        if let Some(p) = target.or(e.inputs.first()) {
            win::show_in_folder_async(p.clone());
        }
    }

    fn open_output(&self) {
        if let Some(p) = self.selected_entry().and_then(|e| e.outputs.first().cloned()) {
            win::shell_open_async(p);
        }
    }

    fn rerun(&self, failed_only: bool) {
        let Some(e) = self.selected_entry() else { return };
        let mail = self.mail.clone();
        win::spawn("zest-history-plan", move || {
            let plan = plan_rerun(&e, failed_only, |p| p.exists());
            mail.post(Msg::Plan(plan));
        });
    }

    /// Undo the newest job that still has outputs (no confirm dialog).
    pub fn undo_last(&self) -> Result<usize, String> {
        let e = {
            let m = self.model.borrow();
            m.entries()
                .iter()
                .rev()
                .find(|e| e.can_undo())
                .cloned()
        };
        let Some(e) = e else {
            return Ok(0);
        };
        let mut recycled = Vec::new();
        let mut first_error = None;
        for p in &e.outputs {
            if !p.exists() {
                continue;
            }
            match zest_shell::recycle(p) {
                Ok(()) => recycled.push(p.clone()),
                Err(err) => {
                    first_error.get_or_insert(format!("{}: {err}", p.display()));
                }
            }
        }
        if let Some(err) = first_error {
            return Err(err);
        }
        let n = recycled.len();
        self.model.borrow_mut().mark_undone(e.id);
        self.host.history_undone(e.id, recycled);
        self.refresh();
        Ok(n)
    }

    fn undo(&self) {
        let Some(e) = self.selected_entry() else { return };
        if !e.can_undo() {
            return;
        }
        let n = e.outputs.len();
        let names: Vec<String> = e
            .outputs
            .iter()
            .take(5)
            .map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
            .collect();
        let more = if n > names.len() { format!("\n... and {} more", n - names.len()) } else { String::new() };
        let ok = win::confirm(
            self.hwnd.get(),
            "Zest",
            &format!("Move {n} file{} to the Recycle Bin?", if n == 1 { "" } else { "s" }),
            &format!(
                "{}{more}\n\nThese are the files that this job made. You can restore them from the Recycle Bin. Your original files are not touched.",
                names.join("\n")
            ),
            "Move to Recycle Bin",
        );
        if !ok {
            return;
        }
        let mail = self.mail.clone();
        win::spawn("zest-history-undo", move || {
            let mut recycled = Vec::new();
            let mut first_error = None;
            for p in &e.outputs {
                if !p.exists() {
                    continue; // already gone
                }
                match zest_shell::recycle(p) {
                    Ok(()) => recycled.push(p.clone()),
                    Err(err) => {
                        first_error.get_or_insert(format!("{}: {err}", p.display()));
                    }
                }
            }
            mail.post(Msg::Undone { id: e.id, recycled, first_error });
        });
    }

    fn cancel(&self) {
        if let Some(e) = self.selected_entry().filter(JobEntry::can_cancel) {
            self.host.history_cancel(e.id);
        }
    }

    fn copy_error(&self) {
        if let Some(e) = self.selected_entry().filter(JobEntry::failed) {
            win::set_clipboard_text(self.hwnd.get(), &error_text(&e));
        }
    }

    fn on_mail(&self) {
        for m in self.mail.drain() {
            match m {
                Msg::Plan(plan) => match plan {
                    Ok(p) => self.host.history_rerun(p.op, p.files, p.skipped),
                    Err(RerunError::NoInputs) => {
                        self.host.notice("The input files are not there any more.", true);
                    }
                    Err(RerunError::UnknownOperation) => {
                        self.host.notice("This version of Zest cannot run that job again.", true);
                    }
                },
                Msg::Undone { id, recycled, first_error } => match first_error {
                    None => {
                        self.model.borrow_mut().mark_undone(id);
                        self.host.history_undone(id, recycled);
                        self.refresh();
                    }
                    Some(e) => {
                        self.host.notice(&format!("Undo failed: {e}"), true);
                    }
                },
            }
        }
    }

    fn command(&self, id: i32) {
        match id {
            ID_FOLDER => self.show_in_folder(),
            ID_OPEN => self.open_output(),
            ID_RERUN => self.rerun(false),
            ID_RETRY => self.rerun(true),
            ID_UNDO => self.undo(),
            ID_CANCEL => self.cancel(),
            ID_COPYERR => self.copy_error(),
            _ => {}
        }
    }

    /// Keys that the list handles before the dialog logic sees them.
    /// True if the message was used.
    pub fn pre_translate(&self, msg: &windows::Win32::UI::WindowsAndMessaging::MSG) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::WM_KEYDOWN;
        let h = self.hwnd.get();
        if h.is_invalid() {
            return false;
        }
        if msg.message == WM_KEYDOWN {
            let vk = msg.wParam.0 as u32;
            // SAFETY: plain query.
            let focus = unsafe { GetFocus() };
            if vk == 0x0D && focus == self.list.get() {
                self.show_in_folder();
                return true;
            }
            if vk == 0x1B {
                if focus == self.search.get() && !self.query.borrow().is_empty() {
                    win::set_text(self.search.get(), "");
                } else {
                    self.hide();
                }
                return true;
            }
        }
        // SAFETY: valid window and message.
        unsafe { windows::Win32::UI::WindowsAndMessaging::IsDialogMessageW(h, msg).as_bool() }
    }

    // ----- window -------------------------------------------------------------------

    fn on_create(&self, hwnd: HWND) {
        let dpi = win::window_dpi(hwnd);
        self.dpi.set(dpi);
        self.font.set(win::create_font(dpi, false));
        let search = win::create_child(hwnd, w!("EDIT"), "", ES_AUTOHSCROLL | WS_TABSTOP, 0x200, ID_SEARCH);
        win::send(search, EM_SETCUEBANNER, 1, w!("Search by file name").as_ptr() as isize);
        self.search.set(search);
        let list_style = win::LVS_REPORT | win::LVS_OWNERDATA | win::LVS_SHOWSELALWAYS | LVS_SINGLESEL | WS_TABSTOP;
        let list = win::create_child(hwnd, WC_LISTVIEWW, "", list_style, 0, ID_LIST);
        win::send(list, win::LVM_SETEXTENDEDLISTVIEWSTYLE, 0, win::LVS_EX_FULLROWSELECT | win::LVS_EX_DOUBLEBUFFER);
        for (i, c) in COLUMNS.iter().enumerate() {
            win::lv_add_column(list, i, c, 100);
        }
        self.list.set(list);
        let defs: [(i32, &str); 7] = [
            (ID_FOLDER, "Show in folder"),
            (ID_OPEN, "Open"),
            (ID_RERUN, "Re-run"),
            (ID_RETRY, "Retry failed"),
            (ID_UNDO, "Undo"),
            (ID_CANCEL, "Cancel job"),
            (ID_COPYERR, "Copy error"),
        ];
        let mut btns = Vec::new();
        for (id, text) in defs {
            btns.push((id, win::create_child(hwnd, w!("BUTTON"), text, WS_TABSTOP, 0, id)));
        }
        *self.buttons.borrow_mut() = btns;
        self.apply_fonts();
        self.apply_theme(hwnd);
        self.layout(hwnd);
        self.refresh();
    }

    fn apply_fonts(&self) {
        let f = self.font.get();
        win::set_font(self.list.get(), f);
        win::set_font(self.search.get(), f);
        for (_, h) in self.buttons.borrow().iter() {
            win::set_font(*h, f);
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
        win::theme_control(self.search.get(), Kind::Edit, t.dark);
        for (_, h) in self.buttons.borrow().iter() {
            win::theme_control(*h, Kind::Button, t.dark);
        }
        win::invalidate(hwnd);
    }

    fn layout(&self, hwnd: HWND) {
        let dpi = self.dpi.get();
        let s = |v: i32| win::scale(v, dpi);
        let (w, h) = win::client_size(hwnd);
        let pad = s(10);
        let row = s(28);
        win::move_to(self.search.get(), pad, pad, (w - pad * 2).clamp(s(100), s(360)), row);
        let btn_y = h - pad - s(32);
        let widths = [112, 64, 72, 96, 64, 88, 88];
        let mut x = pad;
        for ((_, hb), bw) in self.buttons.borrow().iter().zip(widths) {
            win::move_to(*hb, x, btn_y, s(bw), s(32));
            x += s(bw) + s(8);
        }
        let top = pad + row + pad;
        win::move_to(self.list.get(), pad, top, (w - pad * 2).max(10), (btn_y - pad - top).max(10));
        // Columns: the item column takes the rest.
        let (time, op, result) = (s(120), s(170), s(130));
        let items = (w - pad * 2 - time - op - result - s(24)).max(s(120));
        for (i, cw) in [time, op, items, result].into_iter().enumerate() {
            win::send(self.list.get(), win::LVM_SETCOLUMNWIDTH, i, cw as isize);
        }
    }

    fn context_menu(&self, hwnd: HWND) {
        let Some(e) = self.selected_entry() else { return };
        let f = |on: bool| if on { MF_STRING } else { MENU_ITEM_FLAGS(MF_STRING.0 | MF_GRAYED.0) };
        // SAFETY: standard popup menu on the UI thread.
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let _ = AppendMenuW(menu, f(true), ID_FOLDER as usize, w!("Show in folder"));
            let _ = AppendMenuW(menu, f(!e.outputs.is_empty() && e.status != JobStatus::Undone), ID_OPEN as usize, w!("Open"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, f(e.can_rerun()), ID_RERUN as usize, w!("Re-run"));
            let _ = AppendMenuW(menu, f(e.can_retry_failed()), ID_RETRY as usize, w!("Retry failed"));
            let _ = AppendMenuW(menu, f(e.can_undo()), ID_UNDO as usize, w!("Undo (move outputs to Recycle Bin)"));
            let _ = AppendMenuW(menu, f(e.can_cancel()), ID_CANCEL as usize, w!("Cancel job"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, f(e.failed()), ID_COPYERR as usize, w!("Copy error"));
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let _ = SetForegroundWindow(hwnd);
            let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, None);
            let _ = DestroyMenu(menu);
            if cmd.0 > 0 {
                self.command(cmd.0);
            }
        }
    }

    fn notify(&self, hwnd: HWND, lp: LPARAM) -> Option<LRESULT> {
        // SAFETY: WM_NOTIFY carries a valid NMHDR (and the larger struct its code names).
        let hdr = unsafe { &*(lp.0 as *const NMHDR) };
        if hdr.hwndFrom != self.list.get() {
            return None;
        }
        match hdr.code {
            c if c == NM_DBLCLK => {
                self.show_in_folder();
                Some(LRESULT(0))
            }
            c if c == NM_RCLICK => {
                // Select the row under the cursor first.
                self.context_menu(hwnd);
                Some(LRESULT(0))
            }
            LVN_ITEMCHANGED => {
                self.update_buttons();
                Some(LRESULT(0))
            }
            c if c == windows::Win32::UI::Controls::LVN_GETDISPINFOW => {
                // SAFETY: LVN_GETDISPINFOW carries an NMLVDISPINFOW.
                let di = unsafe { &mut *(lp.0 as *mut NMLVDISPINFOW) };
                if di.item.mask.0 & LVIF_TEXT.0 != 0 && !di.item.pszText.is_null() && di.item.cchTextMax > 0 {
                    let text = {
                        let vis = self.visible.borrow();
                        let m = self.model.borrow();
                        usize::try_from(di.item.iItem)
                            .ok()
                            .and_then(|r| vis.get(r))
                            .and_then(|&i| m.entries().get(i))
                            .map(|e| cell_text(e, di.item.iSubItem.max(0) as usize, now_utc(), self.tz_offset.get()))
                            .unwrap_or_default()
                    };
                    let mut buf: Vec<u16> = text.encode_utf16().collect();
                    buf.truncate(di.item.cchTextMax as usize - 1);
                    buf.push(0);
                    // SAFETY: pszText has room for cchTextMax characters.
                    unsafe { std::ptr::copy_nonoverlapping(buf.as_ptr(), di.item.pszText.0, buf.len()) };
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
                        let color = {
                            let vis = self.visible.borrow();
                            let m = self.model.borrow();
                            let t = self.theme.borrow();
                            vis.get(cd.nmcd.dwItemSpec)
                                .and_then(|&i| m.entries().get(i))
                                .and_then(|e| match e.status {
                                    JobStatus::Done if e.failed() => Some(t.error),
                                    JobStatus::Cancelled | JobStatus::Undone | JobStatus::Interrupted => Some(t.muted),
                                    _ => None,
                                })
                        };
                        if let Some(c) = color {
                            cd.clrText = c;
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

impl Handler for HistoryWin {
    fn attach(&self, hwnd: HWND) {
        self.hwnd.set(hwnd);
    }

    fn message(&self, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        if msg == self.taskbar_msg && msg != 0 {
            // The taskbar button exists now (also after Explorer restarts).
            self.taskbar_ready.set(true);
            *self.taskbar.borrow_mut() = None;
            self.last_taskbar.set(None);
            self.update_taskbar();
            return Some(LRESULT(0));
        }
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
                mm.ptMinTrackSize = POINT { x: win::scale(700, dpi), y: win::scale(320, dpi) };
                Some(LRESULT(0))
            }
            WM_COMMAND => {
                let id = i32::from(win::loword(wp.0));
                let code = win::hiword(wp.0);
                if id == ID_SEARCH {
                    if code == EN_CHANGE {
                        *self.query.borrow_mut() = win::get_text(self.search.get());
                        self.refresh();
                    }
                } else if code == 0 {
                    self.command(id);
                }
                Some(LRESULT(0))
            }
            WM_NOTIFY => self.notify(hwnd, lp),
            win::WM_MAILBOX => {
                self.on_mail();
                Some(LRESULT(0))
            }
            WM_ACTIVATE => {
                if win::loword(wp.0) != 0 {
                    self.model.borrow_mut().acknowledge();
                    self.update_taskbar();
                }
                None
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
            WM_CTLCOLOREDIT => {
                let t = self.theme.borrow();
                // SAFETY: wParam is the HDC of the control.
                unsafe {
                    SetTextColor(HDC(wp.0 as *mut c_void), t.fg);
                    SetBkColor(HDC(wp.0 as *mut c_void), t.edit_bg);
                }
                Some(LRESULT(t.edit_brush.0 as isize))
            }
            WM_CTLCOLORSTATIC => {
                let t = self.theme.borrow();
                // SAFETY: wParam is the HDC of the control.
                unsafe {
                    SetTextColor(HDC(wp.0 as *mut c_void), t.fg);
                    SetBkColor(HDC(wp.0 as *mut c_void), t.bg);
                }
                Some(LRESULT(t.bg_brush.0 as isize))
            }
            WM_DESTROY => {
                win::delete_font(self.font.replace(Default::default()));
                None
            }
            _ => None,
        }
    }
}
