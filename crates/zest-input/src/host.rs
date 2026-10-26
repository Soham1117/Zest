//! The hidden window that hosts Raw Input, the hold timer and the hotkeys.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::mem::size_of;
use std::panic::{catch_unwind, AssertUnwindSafe};

use anyhow::{Context, Result};
use windows::core::{w, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_CLASS_ALREADY_EXISTS, FALSE, GetLastError, HWND, LPARAM, LRESULT, POINT,
    WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL,
    MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
};
use windows::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE,
    RAWINPUTHEADER, RIDEV_INPUTSINK, RID_INPUT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetAncestor, GetCursorPos,
    GetForegroundWindow, GetSystemMetrics, GetWindowLongPtrW, GetWindowThreadProcessId, KillTimer,
    RegisterClassW, SetTimer, SetWindowLongPtrW, WindowFromPoint, GA_ROOT, GWLP_USERDATA,
    SM_CXDRAG, SM_CYDRAG, SM_SWAPBUTTON, WINDOW_EX_STYLE, WM_TIMER, WNDCLASSW, WS_POPUP,
};
use zest_core::config::{Config, Scope};

use crate::gesture::{GestureDetector, Mods, Out, Pt};
use crate::plan::{build_plan, scope_allows, Btn, ChordSpec, Plan, SlotKind, SlotSpec};

const WM_INPUT: u32 = 0x00FF;
const WM_HOTKEY: u32 = 0x0312;
const WM_DESTROY: u32 = 0x0002;
/// Hold timers use ids `TIMER_BASE + slot index`.
const TIMER_BASE: usize = 1;
/// Hotkey ids use `HOTKEY_BASE + chord index`.
const HOTKEY_BASE: i32 = 100;
/// Reserved id for the undo chord (`ui.undo_chord`).
const UNDO_HOTKEY_ID: i32 = 99;

// Raw Input flag values (kept local so the field types match the structs).
const RI_MOUSE_LEFT_BUTTON_DOWN: u16 = 0x0001;
const RI_MOUSE_LEFT_BUTTON_UP: u16 = 0x0002;
const RI_MOUSE_RIGHT_BUTTON_DOWN: u16 = 0x0004;
const RI_MOUSE_RIGHT_BUTTON_UP: u16 = 0x0008;
const RI_MOUSE_MIDDLE_BUTTON_DOWN: u16 = 0x0010;
const RI_MOUSE_MIDDLE_BUTTON_UP: u16 = 0x0020;
const RIM_TYPEMOUSE: u32 = 0;

const VK_CONTROL: i32 = 0x11;
const VK_MENU: i32 = 0x12;
const VK_SHIFT: i32 = 0x10;
const VK_LWIN: i32 = 0x5B;
const VK_RWIN: i32 = 0x5C;

/// What the input layer reports. Coordinates are physical screen pixels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InputEvent {
    /// A drag gesture started. `late`: the modifier was pressed after the drag began.
    DragStarted { x: i32, y: i32, late: bool },
    /// The button of a drag gesture was released.
    DragEnded { x: i32, y: i32 },
    /// The hold gesture elapsed (position of the press).
    HoldTriggered { x: i32, y: i32 },
    /// A registered key chord was pressed (pointer position).
    KeyChord { x: i32, y: i32 },
    /// Undo last job (config `ui.undo_chord`, default Ctrl+Alt+Shift+Z). App ignores while the wheel is open.
    Undo,
}

// ---------------------------------------------------------------------------
// Windows helpers
// ---------------------------------------------------------------------------

fn current_mods() -> Mods {
    let down = |vk: i32| {
        // SAFETY: plain key state query.
        (unsafe { GetAsyncKeyState(vk) } as u16) & 0x8000 != 0
    };
    Mods {
        ctrl: down(VK_CONTROL),
        alt: down(VK_MENU),
        shift: down(VK_SHIFT),
        win: down(VK_LWIN) || down(VK_RWIN),
    }
}

fn cursor() -> Option<Pt> {
    let mut p = POINT::default();
    // SAFETY: out-pointer to a local POINT.
    unsafe { GetCursorPos(&mut p) }.ok()?;
    Some(Pt { x: p.x, y: p.y })
}

fn process_name(pid: u32) -> String {
    // SAFETY: handle is closed below; the buffer is valid for `len` chars.
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        if ok.is_err() {
            return String::new();
        }
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        full.rsplit(['\\', '/']).next().unwrap_or("").to_string()
    }
}

/// Scope filter state: caches the last process id -> name lookup.
#[derive(Default)]
struct ScopeFilter {
    excluded: Vec<String>,
    file_managers: Vec<String>,
    /// (pid, own process, name)
    cache: Option<(u32, bool, String)>,
}

impl ScopeFilter {
    fn info(&mut self, pid: u32) -> (bool, String) {
        if let Some((cp, own, name)) = &self.cache {
            if *cp == pid {
                return (*own, name.clone());
            }
        }
        let name = process_name(pid);
        // SAFETY: trivial query.
        let own = pid == unsafe { GetCurrentProcessId() };
        self.cache = Some((pid, own, name.clone()));
        (own, name)
    }

    fn check_pid(&mut self, pid: u32, scope: Scope) -> bool {
        let (own, name) = self.info(pid);
        let ok = scope_allows(scope, &name, own, &self.excluded, &self.file_managers);
        if !ok {
            tracing::debug!(process = %name, pid, ?scope, "scope_reject");
        }
        ok
    }

    /// Scope check for the top-level window under `p`.
    fn check_point(&mut self, p: Pt, scope: Scope) -> bool {
        // SAFETY: plain window queries.
        let pid = unsafe {
            let hw = WindowFromPoint(POINT { x: p.x, y: p.y });
            if hw.is_invalid() {
                return false;
            }
            let root = GetAncestor(hw, GA_ROOT);
            let root = if root.is_invalid() { hw } else { root };
            let mut pid = 0u32;
            GetWindowThreadProcessId(root, Some(&mut pid));
            pid
        };
        self.check_pid(pid, scope)
    }

    /// Scope check for the foreground window (key chords).
    fn check_foreground(&mut self, scope: Scope) -> bool {
        // SAFETY: plain window queries.
        let pid = unsafe {
            let fg = GetForegroundWindow();
            if fg.is_invalid() {
                return scope == Scope::Anywhere;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(fg, Some(&mut pid));
            pid
        };
        self.check_pid(pid, scope)
    }
}

fn hotkey_mods(m: Mods) -> HOT_KEY_MODIFIERS {
    let mut f = MOD_NOREPEAT;
    if m.ctrl {
        f |= MOD_CONTROL;
    }
    if m.alt {
        f |= MOD_ALT;
    }
    if m.shift {
        f |= MOD_SHIFT;
    }
    if m.win {
        f |= MOD_WIN;
    }
    f
}

// ---------------------------------------------------------------------------
// Shared state (owned by InputHost, reached from the window procedure)
// ---------------------------------------------------------------------------

struct SlotState {
    spec: SlotSpec,
    det: GestureDetector,
    /// Token of the pending hold timer.
    token: u64,
}

struct Shared {
    hwnd: Cell<HWND>,
    callback: RefCell<Box<dyn FnMut(InputEvent)>>,
    paused: Cell<bool>,
    /// True between DragStarted and DragEnded (undo is ignored).
    mid_drag: Cell<bool>,
    slots: RefCell<Vec<SlotState>>,
    chords: RefCell<Vec<ChordSpec>>,
    registered: RefCell<Vec<i32>>,
    undo: Cell<Option<(Mods, u32)>>,
    scope: RefCell<ScopeFilter>,
    /// Button state: primary, secondary, middle.
    held: [Cell<bool>; 3],
}

fn btn_index(b: Btn) -> usize {
    match b {
        Btn::Primary => 0,
        Btn::Secondary => 1,
        Btn::Middle => 2,
    }
}

impl Shared {
    fn emit(&self, ev: InputEvent) {
        let Ok(mut cb) = self.callback.try_borrow_mut() else {
            tracing::warn!(?ev, "input event dropped: callback re-entered");
            return;
        };
        // A panic must not unwind through the window procedure.
        if catch_unwind(AssertUnwindSafe(|| (cb)(ev))).is_err() {
            tracing::error!(?ev, "input callback panicked");
        }
    }

    fn hwnd(&self) -> HWND {
        self.hwnd.get()
    }

    fn kill_timer(&self, idx: usize) {
        // SAFETY: our own window.
        unsafe {
            let _ = KillTimer(self.hwnd(), TIMER_BASE + idx);
        }
    }

    fn kill_all_timers(&self) {
        let n = self.slots.borrow().len();
        for i in 0..n {
            self.kill_timer(i);
        }
    }

    /// Apply a detector result for slot `idx`.
    fn handle(&self, idx: usize, out: Out, at: Pt) {
        let (kind, hold_ms) = {
            let slots = self.slots.borrow();
            let Some(s) = slots.get(idx) else { return };
            (s.spec.kind, s.spec.hold_ms)
        };
        match out {
            Out::None => {}
            Out::HoldRequested(token) => {
                if kind != SlotKind::Hold {
                    return;
                }
                if let Some(s) = self.slots.borrow_mut().get_mut(idx) {
                    s.token = token;
                }
                // SAFETY: our own window; WM_TIMER goes to the window procedure.
                unsafe {
                    SetTimer(self.hwnd(), TIMER_BASE + idx, hold_ms, None);
                }
                tracing::debug!(token, "armed");
            }
            Out::DragStarted { late } => {
                self.kill_timer(idx);
                if kind == SlotKind::Drag {
                    tracing::debug!(late, "drag_started");
                    self.mid_drag.set(true);
                    self.emit(InputEvent::DragStarted { x: at.x, y: at.y, late });
                }
            }
            Out::DragEnded => {
                self.kill_timer(idx);
                if kind == SlotKind::Drag {
                    tracing::debug!("drag_ended");
                    self.mid_drag.set(false);
                    self.emit(InputEvent::DragEnded { x: at.x, y: at.y });
                }
            }
            Out::HoldTriggered(p) => {
                if kind == SlotKind::Hold {
                    tracing::debug!(x = p.x, y = p.y, "hold_triggered");
                    self.emit(InputEvent::HoldTriggered { x: p.x, y: p.y });
                }
            }
        }
    }

    fn on_hold_timer(&self, idx: usize) {
        self.kill_timer(idx);
        if self.paused.get() {
            return;
        }
        let out = {
            let mut slots = self.slots.borrow_mut();
            let Some(s) = slots.get_mut(idx) else { return };
            let token = s.token;
            s.det.hold_elapsed(token)
        };
        self.handle(idx, out, Pt { x: 0, y: 0 });
    }

    fn on_hotkey(&self, id: i32) {
        if self.paused.get() {
            return;
        }
        if id == UNDO_HOTKEY_ID {
            if self.mid_drag.get() || self.held.iter().any(Cell::get) {
                tracing::debug!("undo ignored (mid-gesture)");
                return;
            }
            tracing::debug!("undo_hotkey");
            self.emit(InputEvent::Undo);
            return;
        }
        let idx = (id - HOTKEY_BASE) as usize;
        let Some(scope) = self.chords.borrow().get(idx).map(|c| c.scope) else { return };
        if !self.scope.borrow_mut().check_foreground(scope) {
            return;
        }
        let p = cursor().unwrap_or(Pt { x: 0, y: 0 });
        tracing::debug!("key_chord");
        self.emit(InputEvent::KeyChord { x: p.x, y: p.y });
    }

    /// Replace the slots with the ones from `plan`; keeps nothing of old gestures.
    fn apply_plan(&self, plan: &Plan) {
        self.kill_all_timers();
        *self.slots.borrow_mut() = plan
            .slots
            .iter()
            .map(|s| SlotState {
                spec: s.clone(),
                det: GestureDetector::new(s.mods, s.threshold.0, s.threshold.1),
                token: 0,
            })
            .collect();
        for h in &self.held {
            h.set(false);
        }
        {
            let mut sf = self.scope.borrow_mut();
            sf.excluded = plan.excluded.clone();
            sf.file_managers = plan.file_managers.clone();
            sf.cache = None;
        }
        self.paused.set(plan.paused);
        self.undo.set(plan.undo);
        self.register_hotkeys(&plan.chords, plan.undo);
    }

    fn unregister_hotkeys(&self) {
        for id in self.registered.borrow_mut().drain(..) {
            // SAFETY: registered on this window.
            unsafe {
                let _ = UnregisterHotKey(self.hwnd(), id);
            }
        }
    }

    fn register_hotkeys(&self, chords: &[ChordSpec], undo: Option<(Mods, u32)>) {
        self.unregister_hotkeys();
        *self.chords.borrow_mut() = chords.to_vec();
        for (i, c) in chords.iter().enumerate() {
            let id = HOTKEY_BASE + i as i32;
            // SAFETY: valid window of this thread.
            let primary = unsafe { RegisterHotKey(self.hwnd(), id, hotkey_mods(c.mods), c.vk) };
            match primary {
                Ok(()) => {
                    tracing::info!(vk = c.vk, ?c.mods, "hotkey_registered");
                    self.registered.borrow_mut().push(id);
                }
                Err(e) => {
                    tracing::warn!(vk = c.vk, ?c.mods, "hotkey_error: {e}");
                    // Another app often owns Ctrl+Alt+Space: use the fallback chord (same id).
                    if let Some((fm, fvk)) = c.fallback {
                        // SAFETY: as above.
                        match unsafe { RegisterHotKey(self.hwnd(), id, hotkey_mods(fm), fvk) } {
                            Ok(()) => {
                                tracing::info!(vk = fvk, mods = ?fm, "hotkey_registered (fallback)");
                                self.registered.borrow_mut().push(id);
                            }
                            Err(e) => tracing::warn!(vk = fvk, "fallback hotkey_error: {e}"),
                        }
                    }
                }
            }
        }
        if let Some((mods, vk)) = undo {
            // SAFETY: valid window of this thread.
            match unsafe { RegisterHotKey(self.hwnd(), UNDO_HOTKEY_ID, hotkey_mods(mods), vk) } {
                Ok(()) => {
                    tracing::info!(vk, ?mods, "undo_hotkey_registered");
                    self.registered.borrow_mut().push(UNDO_HOTKEY_ID);
                }
                Err(e) => tracing::warn!(vk, ?mods, "undo_hotkey_error: {e}"),
            }
        }
    }

    fn on_raw_input(&self, lparam: LPARAM) {
        // RAWINPUT needs 8-byte alignment; a u64 array gives that.
        let mut buf = [0u64; 32];
        let mut size = size_of::<[u64; 32]>() as u32;
        // SAFETY: buffer and size describe a valid writable region.
        let n = unsafe {
            GetRawInputData(
                HRAWINPUT(lparam.0 as *mut c_void),
                RID_INPUT,
                Some(buf.as_mut_ptr() as *mut c_void),
                &mut size,
                size_of::<RAWINPUTHEADER>() as u32,
            )
        };
        if n == u32::MAX || n == 0 {
            return;
        }
        // SAFETY: GetRawInputData filled `buf` with a RAWINPUT; buf is 8-aligned.
        let raw = unsafe { &*(buf.as_ptr() as *const RAWINPUT) };
        if raw.header.dwType != RIM_TYPEMOUSE {
            return;
        }
        // SAFETY: dwType says the mouse member is the active one.
        let mouse = unsafe { raw.data.mouse };
        // SAFETY: reading the button flags view of the union.
        let flags = unsafe { mouse.Anonymous.Anonymous.usButtonFlags };
        // SAFETY: simple metric query. The primary button is the physical right
        // button when the user swapped buttons.
        let swapped = unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0;
        let (pd, pu, sd, su) = if swapped {
            (RI_MOUSE_RIGHT_BUTTON_DOWN, RI_MOUSE_RIGHT_BUTTON_UP, RI_MOUSE_LEFT_BUTTON_DOWN, RI_MOUSE_LEFT_BUTTON_UP)
        } else {
            (RI_MOUSE_LEFT_BUTTON_DOWN, RI_MOUSE_LEFT_BUTTON_UP, RI_MOUSE_RIGHT_BUTTON_DOWN, RI_MOUSE_RIGHT_BUTTON_UP)
        };
        let buttons = [
            (Btn::Primary, pd, pu),
            (Btn::Secondary, sd, su),
            (Btn::Middle, RI_MOUSE_MIDDLE_BUTTON_DOWN, RI_MOUSE_MIDDLE_BUTTON_UP),
        ];

        for (btn, down_flag, up_flag) in buttons {
            if flags & down_flag != 0 {
                self.held[btn_index(btn)].set(true);
                if let Some(p) = cursor() {
                    let mods = current_mods();
                    self.dispatch(p, |s, scope| s.det.mouse_down(p, mods, &mut |q| scope(q, s.spec.scope)), btn, true);
                }
            }
            if flags & up_flag != 0 {
                self.held[btn_index(btn)].set(false);
                let at = cursor().unwrap_or(Pt { x: 0, y: 0 });
                self.dispatch(at, |s, _| s.det.mouse_up(), btn, true);
            }
        }
        let moved = mouse.lLastX != 0 || mouse.lLastY != 0;
        if moved && self.held.iter().any(Cell::get) {
            if let Some(p) = cursor() {
                let mods = current_mods();
                for btn in [Btn::Primary, Btn::Secondary, Btn::Middle] {
                    if !self.held[btn_index(btn)].get() {
                        continue;
                    }
                    self.dispatch(
                        p,
                        |s, scope| s.det.mouse_move(p, mods, true, &mut |q| scope(q, s.spec.scope)),
                        btn,
                        false,
                    );
                }
            }
        }
    }

    /// Feed one event to every slot listening on `btn`, then apply results.
    /// `_down_or_up` only documents the call sites.
    fn dispatch(
        &self,
        at: Pt,
        mut f: impl FnMut(&mut SlotState, &mut dyn FnMut(Pt, Scope) -> bool) -> Out,
        btn: Btn,
        _down_or_up: bool,
    ) {
        let mut results: Vec<(usize, Out)> = Vec::new();
        {
            let mut slots = self.slots.borrow_mut();
            let mut sf = self.scope.borrow_mut();
            let mut scope = |p: Pt, sc: Scope| sf.check_point(p, sc);
            for (i, s) in slots.iter_mut().enumerate() {
                if s.spec.button != btn {
                    continue;
                }
                let out = f(s, &mut scope);
                if out != Out::None {
                    results.push((i, out));
                }
            }
        }
        for (i, out) in results {
            self.handle(i, out, at);
        }
    }
}

// ---------------------------------------------------------------------------
// Public host
// ---------------------------------------------------------------------------

/// Owns the hidden input window. Create and drop it on the UI thread.
pub struct InputHost {
    shared: Box<Shared>,
    hwnd: HWND,
}

impl InputHost {
    /// Create the hidden window, register Raw Input (mouse) and the hotkeys.
    /// `callback` is invoked on the calling thread, from its message loop.
    pub fn new(config: &Config, callback: Box<dyn FnMut(InputEvent)>) -> Result<InputHost> {
        let shared = Box::new(Shared {
            hwnd: Cell::new(HWND::default()),
            callback: RefCell::new(callback),
            paused: Cell::new(false),
            mid_drag: Cell::new(false),
            slots: RefCell::new(Vec::new()),
            chords: RefCell::new(Vec::new()),
            registered: RefCell::new(Vec::new()),
            undo: Cell::new(None),
            scope: RefCell::new(ScopeFilter::default()),
            held: [Cell::new(false), Cell::new(false), Cell::new(false)],
        });
        // SAFETY: standard window class registration and creation on the UI thread.
        let hwnd = unsafe {
            let hinst = GetModuleHandleW(None).context("GetModuleHandleW")?;
            let class = w!("ZestInputHost");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(input_wndproc),
                hInstance: hinst.into(),
                lpszClassName: class,
                ..Default::default()
            };
            if RegisterClassW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
                anyhow::bail!("RegisterClassW failed: {}", windows::core::Error::from_win32());
            }
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                w!("Zest input"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                hinst,
                None,
            )
            .context("CreateWindowExW (input)")?
        };
        shared.hwnd.set(hwnd);
        // SAFETY: the Box outlives the window (Drop clears this before destroying it).
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, &*shared as *const Shared as isize);
        }

        // SAFETY: simple metric queries.
        let sys = unsafe { (GetSystemMetrics(SM_CXDRAG).max(1), GetSystemMetrics(SM_CYDRAG).max(1)) };
        tracing::debug!(cx = sys.0, cy = sys.1, "drag_threshold");
        let host = InputHost { shared, hwnd };
        host.shared.apply_plan(&build_plan(config, sys));

        let devices = [RAWINPUTDEVICE {
            usUsagePage: 1,
            usUsage: 2, // mouse
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: hwnd,
        }];
        // SAFETY: valid device array and window.
        match unsafe { RegisterRawInputDevices(&devices, size_of::<RAWINPUTDEVICE>() as u32) } {
            Ok(()) => tracing::info!("raw_input_registered (mouse)"),
            Err(e) => tracing::error!("raw_input_error: {e}"),
        }
        Ok(host)
    }

    /// The hidden window.
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// Stop (or resume) all detection. Pausing during a drag ends it: a
    /// `DragEnded` event is sent so the wheel can close.
    pub fn pause(&self, paused: bool) {
        let was = self.shared.paused.replace(paused);
        if paused && !was {
            self.shared.kill_all_timers();
            let at = cursor().unwrap_or(Pt { x: 0, y: 0 });
            let n = self.shared.slots.borrow().len();
            for i in 0..n {
                let out = {
                    let mut slots = self.shared.slots.borrow_mut();
                    slots[i].det.mouse_up()
                };
                self.shared.handle(i, out, at);
            }
            for h in &self.shared.held {
                h.set(false);
            }
        }
        tracing::info!(paused, "input pause");
    }

    pub fn is_paused(&self) -> bool {
        self.shared.paused.get()
    }

    /// Apply a reloaded config: triggers, hotkeys, excluded processes and the
    /// paused flag. Running gestures are dropped.
    pub fn reconfigure(&self, config: &Config) {
        // SAFETY: simple metric queries.
        let sys = unsafe { (GetSystemMetrics(SM_CXDRAG).max(1), GetSystemMetrics(SM_CYDRAG).max(1)) };
        self.shared.apply_plan(&build_plan(config, sys));
        tracing::info!("input reconfigured");
    }
}

impl Drop for InputHost {
    fn drop(&mut self) {
        self.shared.unregister_hotkeys();
        self.shared.kill_all_timers();
        // SAFETY: our own window on its thread. Clearing USERDATA first stops
        // the window procedure from touching the Box during destruction.
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

unsafe extern "system" fn input_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: USERDATA is either 0 or a pointer to the Shared owned by InputHost.
    let shared = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Shared;
    if shared.is_null() || msg == WM_DESTROY {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let shared = &*shared;
    match msg {
        WM_INPUT => {
            if !shared.paused.get() {
                shared.on_raw_input(lparam);
            }
            // Raw Input requires DefWindowProc for cleanup.
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_TIMER if wparam.0 >= TIMER_BASE => {
            shared.on_hold_timer(wparam.0 - TIMER_BASE);
            LRESULT(0)
        }
        WM_HOTKEY => {
            shared.on_hotkey(wparam.0 as i32);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
