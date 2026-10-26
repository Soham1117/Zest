//! Drag gesture detection and the hidden main window (hosts Raw Input, the
//! hotkey and the tray callbacks).
//!
//! The gesture design is ported from Filee (MIT), `GestureDetector.cs` and
//! `TriggerService.cs`. Zest never swallows or injects input. Raw Input with
//! `RIDEV_INPUTSINK` only observes the mouse.
//!
//! - Drag: left button down with exactly Ctrl held, then moving past the drag
//!   threshold. Ctrl pressed after the drag already started also counts
//!   ("late modifier").
//! - Hold: the same press kept still for 450 ms opens the wheel for the
//!   Explorer selection.
//! - Chord: Ctrl+Alt+Space (registered hotkey) does the same.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::{Context, Result};
use windows::core::{w, PWSTR};
use windows::Win32::Foundation::{CloseHandle, FALSE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_WIN,
};
use windows::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE,
    RAWINPUTHEADER, RIDEV_INPUTSINK, RID_INPUT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetAncestor, GetCursorPos, GetSystemMetrics,
    GetWindowThreadProcessId, KillTimer, PostQuitMessage, RegisterClassW, RegisterWindowMessageW,
    SetTimer, WindowFromPoint, GA_ROOT, SM_CXDRAG, SM_CYDRAG, SM_SWAPBUTTON, WINDOW_EX_STYLE,
    WM_APP, WM_COMMAND, WM_DESTROY, WM_TIMER, WNDCLASSW, WS_POPUP,
};

use crate::log;
use crate::wheel;

/// Tray callback message.
pub const WM_TRAY: u32 = WM_APP + 1;
const WM_INPUT: u32 = 0x00FF;
const WM_HOTKEY: u32 = 0x0312;
const HOTKEY_ID: i32 = 1;
const VK_SPACE: u32 = 0x20;
const VK_Z: u32 = 0x5A;
const HOLD_TIMER_ID: usize = 1;
/// Press-and-hold time (Filee default).
const HOLD_MS: u32 = 450;

// Raw Input flag values (kept local so the field types match the structs).
const RI_MOUSE_LEFT_BUTTON_DOWN: u16 = 0x0001;
const RI_MOUSE_LEFT_BUTTON_UP: u16 = 0x0002;
const RI_MOUSE_RIGHT_BUTTON_DOWN: u16 = 0x0004;
const RI_MOUSE_RIGHT_BUTTON_UP: u16 = 0x0008;
const RIM_TYPEMOUSE: u32 = 0;

const VK_CONTROL: i32 = 0x11;
const VK_MENU: i32 = 0x12;
const VK_SHIFT: i32 = 0x10;
const VK_LWIN: i32 = 0x5B;
const VK_RWIN: i32 = 0x5C;

// ---------------------------------------------------------------------------
// Pure gesture detector (no Windows calls; unit-tested)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Pt {
    pub x: i32,
    pub y: i32,
}

/// Modifier state. Left and right variants are the same.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
}

impl Mods {
    pub const NONE: Mods = Mods { ctrl: false, alt: false, shift: false, win: false };
    pub const CTRL: Mods = Mods { ctrl: true, alt: false, shift: false, win: false };
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Out {
    None,
    /// A press armed the gesture: start the hold timer for this token.
    HoldRequested(u64),
    DragStarted { late: bool },
    DragEnded,
    HoldTriggered(Pt),
}

#[derive(Clone, Copy, Debug)]
struct Armed {
    x: i32,
    y: i32,
    token: u64,
}

#[derive(Debug)]
pub struct GestureDetector {
    mods: Mods,
    thr_x: i32,
    thr_y: i32,
    armed: Option<Armed>,
    drag_active: bool,
    late_rejected: bool,
    next_token: u64,
}

impl GestureDetector {
    pub fn new(mods: Mods, thr_x: i32, thr_y: i32) -> Self {
        Self { mods, thr_x, thr_y, armed: None, drag_active: false, late_rejected: false, next_token: 0 }
    }

    /// Primary button went down. `scope` decides whether the point is in scope.
    pub fn mouse_down(&mut self, p: Pt, mods: Mods, scope: &mut impl FnMut(Pt) -> bool) -> Out {
        self.armed = None;
        self.late_rejected = false;
        if mods != self.mods || !scope(p) {
            return Out::None;
        }
        self.next_token += 1;
        self.armed = Some(Armed { x: p.x, y: p.y, token: self.next_token });
        Out::HoldRequested(self.next_token)
    }

    /// Pointer moved. `held` = the primary button is currently down.
    pub fn mouse_move(
        &mut self,
        p: Pt,
        mods: Mods,
        held: bool,
        scope: &mut impl FnMut(Pt) -> bool,
    ) -> Out {
        if let Some(a) = self.armed {
            // Filee: strictly greater than the threshold on either axis.
            if (p.x - a.x).abs() <= self.thr_x && (p.y - a.y).abs() <= self.thr_y {
                return Out::None;
            }
            // Passing the threshold also cancels the hold (token is gone).
            self.armed = None;
            self.drag_active = true;
            return Out::DragStarted { late: false };
        }
        // Late modifier: the user started dragging first, then pressed Ctrl.
        if self.drag_active || self.late_rejected || !held || mods == Mods::NONE || mods != self.mods {
            return Out::None;
        }
        if scope(p) {
            self.drag_active = true;
            Out::DragStarted { late: true }
        } else {
            self.late_rejected = true;
            Out::None
        }
    }

    pub fn mouse_up(&mut self) -> Out {
        self.armed = None;
        self.late_rejected = false;
        let ended = self.drag_active;
        self.drag_active = false;
        if ended { Out::DragEnded } else { Out::None }
    }

    /// The hold timer for `token` fired. Stale tokens are ignored.
    pub fn hold_elapsed(&mut self, token: u64) -> Out {
        match self.armed {
            Some(a) if a.token == token => {
                self.armed = None;
                Out::HoldTriggered(Pt { x: a.x, y: a.y })
            }
            _ => Out::None,
        }
    }
}

// ---------------------------------------------------------------------------
// Windows glue
// ---------------------------------------------------------------------------

thread_local! {
    static DETECTOR: RefCell<Option<GestureDetector>> = const { RefCell::new(None) };
    static BUTTON_HELD: Cell<bool> = const { Cell::new(false) };
    static MAIN_HWND: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
    static HOLD_TOKEN: Cell<u64> = const { Cell::new(0) };
    /// (pid, is explorer, process name) of the last window checked.
    static SCOPE_CACHE: RefCell<Option<(u32, bool, String)>> = const { RefCell::new(None) };
}

static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

fn current_mods() -> Mods {
    let down = |vk: i32| (unsafe { GetAsyncKeyState(vk) } as u16) & 0x8000 != 0;
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

/// Scope filter: only explorer.exe (Explorer windows and the Desktop) and
/// never our own process. Caches the last process id.
fn in_scope(p: Pt) -> bool {
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
    let (ok, name) = SCOPE_CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if let Some((cp, is_explorer, name)) = c.as_ref() {
            if *cp == pid {
                return (*is_explorer, name.clone());
            }
        }
        let name = process_name(pid);
        // SAFETY: trivial query.
        let own = pid == unsafe { GetCurrentProcessId() };
        let is_explorer = !own && name.eq_ignore_ascii_case("explorer.exe");
        *c = Some((pid, is_explorer, name.clone()));
        (is_explorer, name)
    });
    if !ok {
        log::event("scope_reject", &format!("process={name} pid={pid}"));
    }
    ok
}

fn with_detector<R>(f: impl FnOnce(&mut GestureDetector) -> R) -> Option<R> {
    DETECTOR.with(|d| {
        let mut d = d.try_borrow_mut().ok()?;
        d.as_mut().map(f)
    })
}

fn kill_hold_timer() {
    let hwnd = MAIN_HWND.with(Cell::get);
    // SAFETY: our own window.
    unsafe {
        let _ = KillTimer(hwnd, HOLD_TIMER_ID);
    }
}

fn handle(out: Out, t0: i64) {
    match out {
        Out::None => {}
        Out::HoldRequested(token) => {
            HOLD_TOKEN.with(|t| t.set(token));
            let hwnd = MAIN_HWND.with(Cell::get);
            // SAFETY: our own window; WM_TIMER is delivered to main_wndproc.
            unsafe {
                SetTimer(hwnd, HOLD_TIMER_ID, HOLD_MS, None);
            }
            log::event("armed", &format!("token={token}"));
        }
        Out::DragStarted { late } => {
            kill_hold_timer();
            log::event("drag_started", &format!("late={late}"));
            wheel::with(|w| w.show_drag(t0));
        }
        Out::DragEnded => {
            kill_hold_timer();
            log::event("drag_ended", "");
            wheel::with(|w| w.on_drag_ended());
        }
        Out::HoldTriggered(p) => {
            log::event("hold_triggered", &format!("x={} y={}", p.x, p.y));
            crate::explorer::open_for_selection("hold");
        }
    }
}

/// Create the hidden main window, register Raw Input (mouse) and the hotkey.
pub fn create_main_window() -> Result<HWND> {
    // SAFETY: standard window class registration and creation on the UI thread.
    unsafe {
        let hinst = GetModuleHandleW(None).context("GetModuleHandleW")?;
        let class = w!("ZestSpikeMain");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(main_wndproc),
            hInstance: hinst.into(),
            lpszClassName: class,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            anyhow::bail!("RegisterClassW failed: {}", windows::core::Error::from_win32());
        }
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("Zest spike main"),
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
        .context("CreateWindowExW (main)")?;
        MAIN_HWND.with(|h| h.set(hwnd));

        let msg = RegisterWindowMessageW(w!("TaskbarCreated"));
        TASKBAR_CREATED.store(msg, Ordering::Relaxed);

        let dw = GetSystemMetrics(SM_CXDRAG).max(1);
        let dh = GetSystemMetrics(SM_CYDRAG).max(1);
        DETECTOR.with(|d| *d.borrow_mut() = Some(GestureDetector::new(Mods::CTRL, dw, dh)));
        log::event("drag_threshold", &format!("cx={dw} cy={dh}"));

        let devices = [RAWINPUTDEVICE {
            usUsagePage: 1,
            usUsage: 2, // mouse
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: hwnd,
        }];
        match RegisterRawInputDevices(&devices, size_of::<RAWINPUTDEVICE>() as u32) {
            Ok(()) => log::event("raw_input_registered", "device=mouse"),
            Err(e) => log::event("raw_input_error", &format!("err={e}")),
        }

        match RegisterHotKey(hwnd, HOTKEY_ID, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, VK_SPACE) {
            Ok(()) => log::event("hotkey_registered", "combo=Ctrl+Alt+Space"),
            Err(e) => {
                log::event("hotkey_error", &format!("combo=Ctrl+Alt+Space err={e}"));
                // Another app often owns Ctrl+Alt+Space; fall back to Win+Alt+Z (same HOTKEY_ID).
                match RegisterHotKey(hwnd, HOTKEY_ID, MOD_WIN | MOD_ALT | MOD_NOREPEAT, VK_Z) {
                    Ok(()) => log::event("hotkey_registered", "combo=Win+Alt+Z fallback=true"),
                    Err(e) => log::event("hotkey_error", &format!("combo=Win+Alt+Z err={e}")),
                }
            }
        }
        Ok(hwnd)
    }
}

unsafe extern "system" fn main_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_INPUT => {
            on_raw_input(lparam);
            // Raw Input requires DefWindowProc for cleanup.
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_TIMER if wparam.0 == HOLD_TIMER_ID => {
            let _ = KillTimer(hwnd, HOLD_TIMER_ID);
            let token = HOLD_TOKEN.with(Cell::get);
            if let Some(out) = with_detector(|d| d.hold_elapsed(token)) {
                handle(out, log::qpc_now());
            }
            LRESULT(0)
        }
        WM_HOTKEY => {
            if wparam.0 as i32 == HOTKEY_ID {
                crate::explorer::open_for_selection("hotkey");
            }
            LRESULT(0)
        }
        WM_TRAY => {
            crate::tray::on_message(hwnd, lparam);
            LRESULT(0)
        }
        WM_COMMAND => {
            crate::tray::on_command(hwnd, (wparam.0 & 0xFFFF) as u16);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = UnregisterHotKey(hwnd, HOTKEY_ID);
            let _ = KillTimer(hwnd, HOLD_TIMER_ID);
            PostQuitMessage(0);
            LRESULT(0)
        }
        m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => {
            crate::tray::add(hwnd);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Ask the main window to close (used by the tray "Quit" item).
pub fn close_main_window(hwnd: HWND) {
    // SAFETY: hwnd is our own window, on the UI thread.
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

fn on_raw_input(lparam: LPARAM) {
    let t0 = log::qpc_now();
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
    let (down_flag, up_flag) = if swapped {
        (RI_MOUSE_RIGHT_BUTTON_DOWN, RI_MOUSE_RIGHT_BUTTON_UP)
    } else {
        (RI_MOUSE_LEFT_BUTTON_DOWN, RI_MOUSE_LEFT_BUTTON_UP)
    };

    if flags & down_flag != 0 {
        BUTTON_HELD.with(|h| h.set(true));
        if let Some(p) = cursor() {
            let mods = current_mods();
            // The ctrl check inside mouse_down runs before the scope filter,
            // so ordinary clicks cost no process lookup.
            if let Some(out) = with_detector(|d| d.mouse_down(p, mods, &mut in_scope)) {
                handle(out, t0);
            }
        }
    }
    if flags & up_flag != 0 {
        BUTTON_HELD.with(|h| h.set(false));
        kill_hold_timer();
        if let Some(out) = with_detector(|d| d.mouse_up()) {
            handle(out, t0);
        }
    }
    let moved = mouse.lLastX != 0 || mouse.lLastY != 0;
    if moved && BUTTON_HELD.with(Cell::get) {
        if let Some(p) = cursor() {
            let mods = current_mods();
            if let Some(out) = with_detector(|d| d.mouse_move(p, mods, true, &mut in_scope)) {
                handle(out, t0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: i32, y: i32) -> Pt {
        Pt { x, y }
    }

    fn det() -> GestureDetector {
        GestureDetector::new(Mods::CTRL, 4, 4)
    }

    fn yes(_: Pt) -> bool {
        true
    }

    #[test]
    fn exact_modifier_match_arms() {
        let mut d = det();
        assert!(matches!(d.mouse_down(p(10, 10), Mods::CTRL, &mut yes), Out::HoldRequested(_)));
    }

    #[test]
    fn extra_or_missing_modifiers_do_not_arm() {
        for m in [
            Mods::NONE,
            Mods { ctrl: true, shift: true, ..Mods::NONE },
            Mods { ctrl: true, alt: true, ..Mods::NONE },
            Mods { ctrl: true, win: true, ..Mods::NONE },
            Mods { shift: true, ..Mods::NONE },
        ] {
            let mut d = det();
            assert_eq!(d.mouse_down(p(0, 0), m, &mut yes), Out::None);
            // Moving far must not start a drag either (and held, but mods differ).
            assert_eq!(d.mouse_move(p(100, 100), m, true, &mut yes), Out::None);
        }
    }

    #[test]
    fn threshold_is_strictly_greater_on_either_axis() {
        let mut d = det();
        d.mouse_down(p(100, 100), Mods::CTRL, &mut yes);
        assert_eq!(d.mouse_move(p(104, 104), Mods::CTRL, true, &mut yes), Out::None);
        assert_eq!(d.mouse_move(p(96, 96), Mods::CTRL, true, &mut yes), Out::None);
        assert_eq!(
            d.mouse_move(p(105, 100), Mods::CTRL, true, &mut yes),
            Out::DragStarted { late: false }
        );
        let mut d = det();
        d.mouse_down(p(100, 100), Mods::CTRL, &mut yes);
        assert_eq!(
            d.mouse_move(p(100, 94), Mods::CTRL, true, &mut yes),
            Out::DragStarted { late: false }
        );
    }

    #[test]
    fn drag_started_fires_once() {
        let mut d = det();
        d.mouse_down(p(0, 0), Mods::CTRL, &mut yes);
        assert_eq!(d.mouse_move(p(50, 0), Mods::CTRL, true, &mut yes), Out::DragStarted { late: false });
        assert_eq!(d.mouse_move(p(80, 0), Mods::CTRL, true, &mut yes), Out::None);
        assert_eq!(d.mouse_move(p(120, 0), Mods::NONE, true, &mut yes), Out::None);
    }

    #[test]
    fn scope_reject_at_down_does_not_arm() {
        let mut d = det();
        assert_eq!(d.mouse_down(p(0, 0), Mods::CTRL, &mut |_| false), Out::None);
        assert_eq!(d.mouse_move(p(50, 0), Mods::CTRL, true, &mut |_| false), Out::None);
    }

    #[test]
    fn late_modifier_starts_drag() {
        let mut d = det();
        // Press without Ctrl: not armed.
        assert_eq!(d.mouse_down(p(0, 0), Mods::NONE, &mut yes), Out::None);
        assert_eq!(d.mouse_move(p(30, 0), Mods::NONE, true, &mut yes), Out::None);
        // Ctrl pressed mid-drag.
        assert_eq!(d.mouse_move(p(40, 0), Mods::CTRL, true, &mut yes), Out::DragStarted { late: true });
        // Already active: no second start.
        assert_eq!(d.mouse_move(p(50, 0), Mods::CTRL, true, &mut yes), Out::None);
    }

    #[test]
    fn late_modifier_needs_held_button_and_exact_mods() {
        let mut d = det();
        assert_eq!(d.mouse_move(p(5, 5), Mods::CTRL, false, &mut yes), Out::None);
        let both = Mods { ctrl: true, shift: true, ..Mods::NONE };
        assert_eq!(d.mouse_move(p(5, 5), both, true, &mut yes), Out::None);
    }

    #[test]
    fn late_reject_is_not_rechecked_until_mouse_up() {
        let mut d = det();
        let calls = Cell::new(0);
        let mut no = |_: Pt| {
            calls.set(calls.get() + 1);
            false
        };
        assert_eq!(d.mouse_move(p(1, 1), Mods::CTRL, true, &mut no), Out::None);
        assert_eq!(d.mouse_move(p(2, 2), Mods::CTRL, true, &mut no), Out::None);
        assert_eq!(d.mouse_move(p(3, 3), Mods::CTRL, true, &mut no), Out::None);
        assert_eq!(calls.get(), 1);
        d.mouse_up();
        assert_eq!(d.mouse_move(p(4, 4), Mods::CTRL, true, &mut no), Out::None);
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn hold_fires_only_without_movement() {
        let mut d = det();
        let Out::HoldRequested(t) = d.mouse_down(p(7, 9), Mods::CTRL, &mut yes) else { panic!() };
        // Small jitter inside the threshold keeps the hold alive.
        d.mouse_move(p(9, 9), Mods::CTRL, true, &mut yes);
        assert_eq!(d.hold_elapsed(t), Out::HoldTriggered(p(7, 9)));
        // Fires only once.
        assert_eq!(d.hold_elapsed(t), Out::None);

        let mut d = det();
        let Out::HoldRequested(t) = d.mouse_down(p(0, 0), Mods::CTRL, &mut yes) else { panic!() };
        d.mouse_move(p(60, 0), Mods::CTRL, true, &mut yes);
        assert_eq!(d.hold_elapsed(t), Out::None);
    }

    #[test]
    fn stale_hold_token_is_ignored() {
        let mut d = det();
        let Out::HoldRequested(old) = d.mouse_down(p(0, 0), Mods::CTRL, &mut yes) else { panic!() };
        d.mouse_up();
        let Out::HoldRequested(new) = d.mouse_down(p(5, 5), Mods::CTRL, &mut yes) else { panic!() };
        assert_ne!(old, new);
        assert_eq!(d.hold_elapsed(old), Out::None);
        assert_eq!(d.hold_elapsed(new), Out::HoldTriggered(p(5, 5)));
    }

    #[test]
    fn mouse_up_ends_drag_and_cancels_hold() {
        let mut d = det();
        d.mouse_down(p(0, 0), Mods::CTRL, &mut yes);
        d.mouse_move(p(50, 0), Mods::CTRL, true, &mut yes);
        assert_eq!(d.mouse_up(), Out::DragEnded);
        assert_eq!(d.mouse_up(), Out::None);

        let mut d = det();
        let Out::HoldRequested(t) = d.mouse_down(p(0, 0), Mods::CTRL, &mut yes) else { panic!() };
        assert_eq!(d.mouse_up(), Out::None);
        assert_eq!(d.hold_elapsed(t), Out::None);
    }
}

