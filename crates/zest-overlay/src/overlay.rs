//! The wheel window and its state machine.

use std::cell::RefCell;
use std::ffi::c_void;
use std::mem::size_of;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::time::Instant;

use anyhow::{Context, Result};
use windows::core::w;
use windows::Win32::Foundation::{
    GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, TRUE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreateEllipticRgn, DeleteObject, GetMonitorInfoW, MonitorFromPoint, SetWindowRgn, ValidateRect,
    HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW, KillTimer,
    LoadCursorW, RegisterClassW, SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, SystemParametersInfoW, GWL_EXSTYLE, HWND_TOPMOST, IDC_ARROW, SPI_GETCLIENTAREAANIMATION,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_HIDE, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    WM_ACTIVATE, WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE,
    WM_MOUSEMOVE, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use zest_core::graph::{WheelItem, WheelModel};
use zest_shell::DropSink;

use crate::geometry::{
    hit_test, region_radius, window_origin, window_px, Hit, Layout, Rect, SubRing,
    DEFAULT_OUTER_LOGICAL, PROGRESS_RADIUS_LOGICAL,
};
use crate::model::{pick_target, prepare, resolve_drop, step_index, DropResolve, Prepared};
use crate::render::{Gfx, Hover, ModelGpu, Scene, Visual};
use crate::theme::Theme;
use crate::{Callbacks, HoverPreviewReq, Outcome, Pick};

const WM_MOUSELEAVE: u32 = 0x02A3;

const TIMER_ANIM: usize = 1;
const TIMER_EMPTY: usize = 2;
const TIMER_END: usize = 3;
const TIMER_DWELL: usize = 4;
const TIMER_DONE: usize = 5;
const TIMER_FADE: usize = 6;
const TIMER_WATCHDOG: usize = 7;
const TIMER_ZONE: usize = 8;
const TIMER_PREVIEW: usize = 9;
const N_TIMERS: usize = 10;
const PREVIEW_DEBOUNCE_MS: u32 = 120;

/// Drag that never brought files: hide after this long (missed button release).
const EMPTY_LIMIT_MS: u32 = 10_000;
/// After the button release with files shown: wait this long for the drop.
const END_GRACE_MS: u32 = 250;
/// Hover time before a sub-ring opens.
const DWELL_MS: u32 = 300;
/// Result glyph time.
const DONE_MS: u32 = 800;
const FADE_MS: f32 = 140.0;
const FADE_TICK_MS: u32 = 16;
const ANIM_TICK_MS: u32 = 16;
/// Progress display catches the broker target over this many ms.
const PROGRESS_LERP_MS: f32 = 150.0;
/// Wheel appear (spring overshoot lives in `wheel_scale`).
const SHOW_MS: f32 = 280.0;
/// Hover accent ease.
const HOVER_MS: f32 = 140.0;
/// Sub-ring fade-in.
const SUB_MS: f32 = 160.0;
/// Progress ring without updates is hidden after this long.
const WATCHDOG_MS: u32 = 60_000;
/// Drop zone idle limit.
const ZONE_MS: u32 = 30_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Hidden,
    /// Window shown but fully transparent, waiting for `DragEnter` with files.
    Probe,
    Wheel,
    Progress,
    Done,
}

enum Action {
    Pick(Pick),
    Cancel,
    ProgressClicked,
    HoverPreview(Option<HoverPreviewReq>),
}

struct State {
    hwnd: HWND,
    gfx: Gfx,
    scale: f32,
    outer_logical: f32,
    layout: Layout,
    px: i32,
    /// Window top-left in screen pixels.
    origin: (i32, i32),
    mode: Mode,
    /// Click mode: window activated, mouse and keys work.
    keyboard: bool,
    /// Opened as a drop zone.
    zone: bool,
    noactivate_removed: bool,
    theme: Theme,
    /// Bumped whenever the window is shown or hidden; stale timers compare it.
    generation: u32,
    timer_gen: [u32; N_TIMERS],
    prepared: Prepared,
    gpu: Option<ModelGpu>,
    files: Vec<PathBuf>,
    hover: Hover,
    sub_open: Option<usize>,
    /// Broker progress target (0..=100).
    progress_target: f32,
    /// Displayed progress (lerped toward target).
    progress_display: f32,
    outcome: Outcome,
    alpha: f32,
    /// Wheel content fade (0..=1).
    content_alpha: f32,
    content_target: f32,
    hover_boost: f32,
    hover_target: f32,
    sub_alpha: f32,
    sub_target: f32,
    /// Appear scale (spring toward 1.0).
    wheel_scale: f32,
    fade_start: Option<Instant>,
    anim_last: Option<Instant>,
    show_started: Option<Instant>,
    tracking: bool,
    first_frame_from: Option<Instant>,
    /// Keyboard type-ahead filter (cleared on Esc / close).
    typeahead: String,
    /// Hover preview debounce generation.
    preview_token: u64,
    /// Text shown in the hover preview chip (empty = hidden).
    preview_text: String,
}

struct Inner {
    st: RefCell<State>,
    cb: RefCell<Callbacks>,
    hwnd: HWND,
}

thread_local! {
    static CURRENT: RefCell<Weak<Inner>> = const { RefCell::new(Weak::new()) };
}

fn with_inner<R>(f: impl FnOnce(&Rc<Inner>) -> R) -> Option<R> {
    let rc = CURRENT.with(|c| c.borrow().upgrade())?;
    Some(f(&rc))
}

/// Config `wheel.reduce_animations = "always"`: no animations, whatever Windows says.
static REDUCE_ALWAYS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Set the "always reduce animations" option (UI thread or any thread).
pub fn set_reduce_animations(always: bool) {
    REDUCE_ALWAYS.store(always, std::sync::atomic::Ordering::Relaxed);
}

fn animations_enabled() -> bool {
    if REDUCE_ALWAYS.load(std::sync::atomic::Ordering::Relaxed) {
        return false;
    }
    let mut on = TRUE;
    // SAFETY: out-pointer to a local BOOL.
    let r = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some(&mut on as *mut _ as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    r.is_err() || on.as_bool()
}

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

impl State {
    fn expands(&self, i: usize) -> bool {
        self.prepared.sectors.get(i).is_some_and(|s| s.expands())
    }

    fn sub_ring(&self) -> Option<SubRing> {
        self.sub_open.map(|p| SubRing {
            parent: p,
            count: self.prepared.sectors.get(p).map_or(0, |s| s.sub_items().len()),
        })
    }

    fn hit_rel(&self, dx: f32, dy: f32) -> Hit {
        hit_test(dx, dy, &self.layout, self.prepared.sectors.len(), self.sub_ring())
    }

    fn hit_screen(&self, x: i32, y: i32) -> Hit {
        let half = self.px as f32 / 2.0;
        self.hit_rel(x as f32 - (self.origin.0 as f32 + half), y as f32 - (self.origin.1 as f32 + half))
    }

    fn hit_client(&self, cx: i32, cy: i32) -> Hit {
        let half = self.px as f32 / 2.0;
        self.hit_rel(cx as f32 - half, cy as f32 - half)
    }

    // ----- timers --------------------------------------------------------

    fn arm(&mut self, id: usize, ms: u32) {
        self.timer_gen[id] = self.generation;
        // SAFETY: our own window; WM_TIMER goes to the wheel proc.
        unsafe {
            SetTimer(self.hwnd, id, ms, None);
        }
    }

    fn kill(&self, id: usize) {
        // SAFETY: our own window.
        unsafe {
            let _ = KillTimer(self.hwnd, id);
        }
    }

    fn kill_timers(&self) {
        for id in TIMER_EMPTY..N_TIMERS {
            self.kill(id);
        }
    }

    // ----- window --------------------------------------------------------

    fn apply_region(&self) {
        let r = match self.mode {
            Mode::Progress | Mode::Done => PROGRESS_RADIUS_LOGICAL * self.scale,
            _ => region_radius(&self.layout, self.sub_open.is_some()),
        };
        let c = self.px / 2;
        // Inflate past the painted rim so GDI's jagged region does not clip
        // Direct2D's antialiased disc edge (that was the "rough circle").
        let pad = (5.0 * self.scale).ceil() as i32;
        let r = r.ceil() as i32 + pad;
        // SAFETY: the region is owned by the system after a successful
        // SetWindowRgn; on failure we free it ourselves.
        unsafe {
            let rgn = CreateEllipticRgn(c - r, c - r, c + r, c + r);
            if SetWindowRgn(self.hwnd, rgn, TRUE) == 0 {
                tracing::warn!("SetWindowRgn failed");
                let _ = DeleteObject(HGDIOBJ(rgn.0));
            }
        }
    }

    fn rebuild_gpu(&mut self) {
        // SAFETY: UI thread.
        self.gpu = match unsafe { self.gfx.build_model(&self.prepared, &self.layout, self.scale) } {
            Ok(g) => Some(g),
            Err(e) => {
                tracing::warn!("build_model failed: {e:#}");
                None
            }
        };
    }

    fn set_prepared(&mut self, p: Prepared) {
        self.prepared = p;
        self.hover = Hover::None;
        self.hover_boost = 0.0;
        self.hover_target = 0.0;
        self.sub_open = None;
        self.sub_alpha = 0.0;
        self.sub_target = 0.0;
        self.rebuild_gpu();
    }

    /// Apply a new scale and/or outer radius (resizes the swap chain).
    fn relayout(&mut self, scale: f32, outer: f32) {
        let px = window_px(scale, outer);
        if px != self.gfx.px() || (scale - self.scale).abs() > f32::EPSILON {
            // SAFETY: UI thread; no buffer references are held.
            if let Err(e) = unsafe { self.gfx.set_scale(scale, px) } {
                tracing::warn!("dpi_scale_error: {e:#}");
                return;
            }
        }
        self.scale = scale;
        self.outer_logical = outer;
        self.px = px;
        self.layout = Layout::new(scale, outer);
        self.apply_region();
        self.rebuild_gpu();
        tracing::debug!(px, scale, "relayout");
    }

    /// Pick the monitor under `pt`, adapt DPI scale and compute the clamped origin.
    fn prepare_window(&mut self, pt: POINT) -> (i32, i32) {
        // SAFETY: plain Win32 queries on the UI thread.
        unsafe {
            let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            let (mut dx, mut dy) = (0u32, 0u32);
            let scale = match GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) {
                Ok(()) if dx > 0 => dx as f32 / 96.0,
                _ => self.scale,
            };
            if (scale - self.scale).abs() > f32::EPSILON || window_px(scale, self.outer_logical) != self.px {
                self.relayout(scale, self.outer_logical);
            }
            let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
            let work = if GetMonitorInfoW(mon, &mut mi).as_bool() {
                Rect {
                    left: mi.rcWork.left,
                    top: mi.rcWork.top,
                    right: mi.rcWork.right,
                    bottom: mi.rcWork.bottom,
                }
            } else {
                Rect { left: pt.x - 10_000, top: pt.y - 10_000, right: pt.x + 10_000, bottom: pt.y + 10_000 }
            };
            let inner = 2 * self.layout.r_in.round() as i32;
            window_origin(pt.x, pt.y, self.px, inner, work)
        }
    }

    fn show_window(&mut self, origin: (i32, i32)) {
        self.origin = origin;
        // SAFETY: our window; SWP_NOACTIVATE keeps focus where it is.
        let r = unsafe {
            SetWindowPos(self.hwnd, HWND_TOPMOST, origin.0, origin.1, self.px, self.px, SWP_NOACTIVATE | SWP_SHOWWINDOW)
        };
        if let Err(e) = r {
            tracing::warn!("show_window_error: {e}");
        }
    }

    /// Windows can push a topmost window down after other apps run.
    fn assert_topmost(&self) {
        // SAFETY: our own window; no move, resize or activation.
        let r = unsafe {
            SetWindowPos(self.hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE)
        };
        if let Err(e) = r {
            tracing::warn!("assert_topmost_error: {e}");
        }
    }

    fn set_noactivate(&mut self, on: bool) {
        // SAFETY: our own window; style bits only.
        unsafe {
            let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            let bit = WS_EX_NOACTIVATE.0 as isize;
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, if on { ex | bit } else { ex & !bit });
        }
        self.noactivate_removed = !on;
    }

    fn render(&mut self) {
        let visual = match self.mode {
            Mode::Hidden => return,
            Mode::Probe => Visual::Probe,
            Mode::Wheel => Visual::Wheel,
            Mode::Progress => Visual::Progress { frac: self.progress_display / 100.0 },
            Mode::Done => Visual::Done { outcome: self.outcome, alpha: self.alpha },
        };
        self.render_visual(visual);
    }

    fn render_visual(&mut self, visual: Visual) {
        let scene = Scene {
            visual,
            prepared: &self.prepared,
            gpu: self.gpu.as_ref(),
            hover: self.hover,
            sub_open: self.sub_open,
            theme: &self.theme,
            layout: self.layout,
            scale: self.scale,
            content_alpha: self.content_alpha,
            hover_boost: self.hover_boost,
            sub_alpha: self.sub_alpha,
            wheel_scale: self.wheel_scale,
            anim_time: self
                .show_started
                .map(|t| t.elapsed().as_secs_f32())
                .unwrap_or(0.0),
            preview_text: &self.preview_text,
        };
        // SAFETY: UI thread, buffers are not referenced elsewhere.
        match unsafe { self.gfx.draw(&scene) } {
            Ok(()) => {
                if let Some(t) = self.first_frame_from.take() {
                    tracing::debug!(ms = t.elapsed().as_secs_f64() * 1000.0, "first_frame");
                }
            }
            Err(e) => tracing::warn!("render_error: {e:#}"),
        }
    }

    // ----- show / hide ---------------------------------------------------

    fn begin_show(&mut self, reason: &str) {
        if self.mode != Mode::Hidden {
            self.hide(reason);
        }
        self.generation = self.generation.wrapping_add(1);
        self.hover = Hover::None;
        self.sub_open = None;
        self.fade_start = None;
        self.anim_last = None;
        self.alpha = 1.0;
        self.content_alpha = 1.0;
        self.content_target = 1.0;
        self.hover_boost = 0.0;
        self.hover_target = 0.0;
        self.sub_alpha = 0.0;
        self.sub_target = 0.0;
        self.wheel_scale = 1.0;
        self.progress_target = 0.0;
        self.progress_display = 0.0;
        self.show_started = None;
    }

    fn start_wheel_appear(&mut self) {
        self.show_started = Some(Instant::now());
        if animations_enabled() {
            self.content_alpha = 0.0;
            self.content_target = 1.0;
            self.wheel_scale = 0.82;
            self.arm_anim();
        } else {
            self.content_alpha = 1.0;
            self.content_target = 1.0;
            self.wheel_scale = 1.0;
        }
    }

    /// Ease-out back: overshoots then settles (mac-like bloom).
    fn ease_out_back(t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        let c1 = 1.70158;
        let c3 = c1 + 1.0;
        1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
    }

    fn arm_anim(&mut self) {
        if self.anim_last.is_none() {
            self.anim_last = Some(Instant::now());
        }
        self.arm(TIMER_ANIM, ANIM_TICK_MS);
    }

    fn hover_wants_boost(hover: Hover) -> f32 {
        match hover {
            Hover::None => 0.0,
            Hover::Sector(_) | Hover::Sub(_, _) => 1.0,
        }
    }

    /// Step content/hover/sub/progress toward targets. Returns whether a redraw is needed.
    fn tick_anim(&mut self) -> bool {
        let dt_ms = self.anim_last.map_or(ANIM_TICK_MS as f32, |t| {
            t.elapsed().as_secs_f32() * 1000.0
        });
        self.anim_last = Some(Instant::now());
        let step = |cur: &mut f32, target: f32, ms: f32| -> bool {
            if (*cur - target).abs() < 0.004 {
                let changed = (*cur - target).abs() > f32::EPSILON;
                *cur = target;
                return changed;
            }
            let t = (dt_ms / ms).clamp(0.0, 1.0);
            *cur += (target - *cur) * (0.22 + 0.78 * t);
            true
        };
        let mut dirty = false;
        dirty |= step(&mut self.content_alpha, self.content_target, SHOW_MS);
        dirty |= step(&mut self.hover_boost, self.hover_target, HOVER_MS);
        dirty |= step(&mut self.sub_alpha, self.sub_target, SUB_MS);
        if self.mode == Mode::Progress {
            dirty |= step(&mut self.progress_display, self.progress_target, PROGRESS_LERP_MS);
        }

        // Spring scale from appear progress (overshoot then rest at 1).
        if let Some(t0) = self.show_started {
            if self.mode == Mode::Wheel && animations_enabled() {
                let u = (t0.elapsed().as_secs_f32() * 1000.0 / SHOW_MS).clamp(0.0, 1.0);
                let bloom = Self::ease_out_back(u);
                let next = 0.82 + 0.18 * bloom;
                if (next - self.wheel_scale).abs() > 0.001 {
                    self.wheel_scale = next;
                    dirty = true;
                }
                if u < 1.0 {
                    dirty = true;
                } else {
                    self.wheel_scale = 1.0;
                }
            }
        }

        // Keep ticking while stagger/sheen still needs frames.
        let appear_live = self
            .show_started
            .is_some_and(|t| t.elapsed().as_secs_f32() < 0.85);
        let sheen_live = self.mode == Mode::Progress && animations_enabled();

        let still = (self.content_alpha - self.content_target).abs() > 0.004
            || (self.hover_boost - self.hover_target).abs() > 0.004
            || (self.sub_alpha - self.sub_target).abs() > 0.004
            || (self.wheel_scale - 1.0).abs() > 0.004
            || appear_live
            || sheen_live
            || (self.mode == Mode::Progress
                && (self.progress_display - self.progress_target).abs() > 0.05);
        if still {
            self.arm(TIMER_ANIM, ANIM_TICK_MS);
        } else {
            self.kill(TIMER_ANIM);
            self.anim_last = None;
            self.content_alpha = self.content_target;
            self.hover_boost = self.hover_target;
            self.sub_alpha = self.sub_target;
            self.wheel_scale = 1.0;
            if self.mode == Mode::Progress {
                self.progress_display = self.progress_target;
            }
        }
        dirty
    }

    fn show_for_drag(&mut self, x: i32, y: i32) {
        self.begin_show("drag_restart");
        self.keyboard = false;
        self.zone = false;
        self.files.clear();
        self.prepared = Prepared::empty();
        self.gpu = None;
        let origin = self.prepare_window(POINT { x, y });
        self.mode = Mode::Probe;
        self.apply_region();
        self.render();
        self.show_window(origin);
        self.assert_topmost();
        self.arm(TIMER_EMPTY, EMPTY_LIMIT_MS);
        tracing::debug!("window_shown (drag)");
    }

    fn show_click(&mut self, x: i32, y: i32, zone: bool) {
        self.begin_show("click_reopen");
        self.keyboard = true;
        self.typeahead.clear();
        self.zone = zone;
        self.theme = Theme::from_system();
        let origin = self.prepare_window(POINT { x, y });
        self.mode = Mode::Wheel;
        self.start_wheel_appear();
        self.apply_region();
        self.render();
        self.set_noactivate(false);
        self.show_window(origin);
        self.assert_topmost();
        // SAFETY: allowed after a hotkey; may be refused after the hold gesture.
        let ok = unsafe { SetForegroundWindow(self.hwnd) }.as_bool();
        tracing::debug!(foreground_ok = ok, zone, "click_mode");
        if zone {
            self.arm(TIMER_ZONE, ZONE_MS);
        }
    }

    fn hide(&mut self, reason: &str) {
        self.generation = self.generation.wrapping_add(1);
        self.kill_timers();
        // SAFETY: our own window.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        if self.noactivate_removed {
            self.set_noactivate(true);
        }
        self.keyboard = false;
        self.zone = false;
        self.typeahead.clear();
        self.preview_text.clear();
        self.kill(TIMER_PREVIEW);
        self.first_frame_from = None;
        self.files.clear();
        self.hover = Hover::None;
        self.sub_open = None;
        self.fade_start = None;
        if self.mode != Mode::Hidden {
            self.mode = Mode::Hidden;
            // Restore the inner-disc region for the next show.
            self.apply_region();
            tracing::debug!(reason, "hide");
        }
    }

    /// Close because the user (or a timeout) cancelled. Reports a cancel only
    /// if files were shown.
    fn close_cancel(&mut self, reason: &str) -> Option<Action> {
        let was_wheel = self.mode == Mode::Wheel;
        self.hide(reason);
        was_wheel.then_some(Action::Cancel)
    }

    fn on_drag_ended(&mut self) -> Option<Action> {
        if self.keyboard {
            return None;
        }
        match self.mode {
            Mode::Probe => {
                self.hide("drag_ended_no_files");
                None
            }
            Mode::Wheel => {
                // A drop arrives around the same time as the release: give it a moment.
                self.kill_timers();
                self.arm(TIMER_END, END_GRACE_MS);
                None
            }
            _ => None,
        }
    }

    // ----- model ---------------------------------------------------------

    fn accepts_enter(&self) -> bool {
        self.mode == Mode::Probe || (self.mode == Mode::Wheel && self.zone && self.prepared.sectors.is_empty())
    }

    fn enter_with_model(&mut self, files: &[PathBuf], model: &WheelModel, count: usize, t0: Instant) {
        self.kill(TIMER_EMPTY);
        self.kill(TIMER_END);
        self.theme = Theme::from_system();
        self.files = files.to_vec();
        self.set_prepared(prepare(model, count));
        self.mode = Mode::Wheel;
        self.start_wheel_appear();
        self.apply_region();
        self.first_frame_from = Some(t0);
        self.render();
    }

    fn set_model(&mut self, model: &WheelModel, count: usize) {
        if self.mode != Mode::Wheel && self.mode != Mode::Probe {
            return;
        }
        self.set_prepared(prepare(model, count));
        self.apply_region();
        self.render();
    }

    // ----- hover ---------------------------------------------------------

    fn close_sub(&mut self) {
        if self.sub_open.take().is_some() {
            self.sub_alpha = 0.0;
            self.sub_target = 0.0;
            self.apply_region();
        }
    }

    fn open_sub(&mut self, i: usize) {
        if self.expands(i) && self.sub_open != Some(i) {
            self.sub_open = Some(i);
            self.sub_target = 1.0;
            if animations_enabled() {
                self.sub_alpha = 0.0;
                self.arm_anim();
            } else {
                self.sub_alpha = 1.0;
            }
            self.apply_region();
        }
    }

    /// Update hover and sub-ring from a hit. Returns whether something changed.
    fn apply_hit(&mut self, hit: Hit) -> bool {
        let old = (self.hover, self.sub_open);
        match hit {
            Hit::Sector(i) => {
                if self.sub_open.is_some_and(|p| p != i) {
                    self.close_sub();
                }
                self.hover = Hover::Sector(i);
                if self.expands(i) && self.sub_open != Some(i) {
                    if old.0 != Hover::Sector(i) {
                        self.arm(TIMER_DWELL, DWELL_MS);
                    }
                } else {
                    self.kill(TIMER_DWELL);
                }
            }
            Hit::SubSector { group, sub } => {
                self.hover = Hover::Sub(group, sub);
                self.kill(TIMER_DWELL);
            }
            Hit::Center => {
                self.close_sub();
                self.hover = Hover::None;
                self.kill(TIMER_DWELL);
            }
            Hit::Outside => {
                self.hover = Hover::None;
                self.kill(TIMER_DWELL);
            }
        }
        let want = Self::hover_wants_boost(self.hover);
        if (self.hover_target - want).abs() > 0.01 {
            self.hover_target = want;
            if animations_enabled() {
                self.arm_anim();
            } else {
                self.hover_boost = want;
            }
        }
        if old.0 != self.hover {
            self.arm_hover_preview();
        }
        old != (self.hover, self.sub_open)
    }

    fn arm_hover_preview(&mut self) {
        self.preview_token = self.preview_token.wrapping_add(1);
        if !self.preview_text.is_empty() {
            self.preview_text.clear();
        }
        match self.hover {
            Hover::Sector(_) | Hover::Sub(_, _) => {
                self.arm(TIMER_PREVIEW, PREVIEW_DEBOUNCE_MS);
            }
            Hover::None => {
                self.kill(TIMER_PREVIEW);
            }
        }
    }

    fn hover_preview_req(&self) -> Option<HoverPreviewReq> {
        let item = match self.hover {
            Hover::Sector(i) => {
                let s = self.prepared.sectors.get(i)?;
                if s.expands() {
                    return None;
                }
                match &s.kind {
                    crate::model::SectorKind::Item(it) => Some(*it),
                    _ => None,
                }?
            }
            Hover::Sub(g, j) => self.prepared.sectors.get(g)?.sub_items().get(j).copied()?,
            Hover::None => return None,
        };
        Some(HoverPreviewReq {
            token: self.preview_token,
            files: self.files.clone(),
            item,
        })
    }

    fn drag_over(&mut self, x: i32, y: i32) -> bool {
        if self.mode != Mode::Wheel || self.prepared.sectors.is_empty() {
            return false;
        }
        let hit = self.hit_screen(x, y);
        if self.apply_hit(hit) {
            self.render();
        }
        crate::model::drop_accepts(&self.prepared, hit)
    }

    fn drag_leave(&mut self) {
        if self.mode != Mode::Wheel {
            return;
        }
        if self.zone && !self.prepared.sectors.is_empty() {
            // Drop zone: the drag went away, back to the empty zone.
            self.files.clear();
            self.set_prepared(Prepared::empty());
            self.apply_region();
            self.render();
            return;
        }
        self.kill(TIMER_DWELL);
        let changed = self.hover != Hover::None || self.sub_open.is_some();
        self.hover = Hover::None;
        self.close_sub();
        if changed {
            self.render();
        }
    }

    fn on_drop(&mut self, files: &[PathBuf], x: i32, y: i32) -> (bool, Option<Action>) {
        if self.mode != Mode::Wheel || self.prepared.sectors.is_empty() {
            return (false, None);
        }
        let hit = self.hit_screen(x, y);
        match resolve_drop(&self.prepared, hit) {
            DropResolve::Pick(group, category, item) => {
                let f = if files.is_empty() { std::mem::take(&mut self.files) } else { files.to_vec() };
                tracing::debug!(label = item.label, files = f.len(), "drop");
                self.after_pick(&item);
                (true, Some(Action::Pick(Pick { files: f, group, category, item })))
            }
            DropResolve::OpenSub(i) => {
                tracing::debug!(sector = i, "drop_open_sub");
                self.open_sub(i);
                self.hover = Hover::Sector(i);
                self.kill(TIMER_DWELL);
                self.render();
                (false, None)
            }
            DropResolve::Cancel => {
                tracing::debug!("drop_cancel");
                (false, self.close_cancel("drop_cancel"))
            }
        }
    }

    /// What the wheel does after a pick. Conversions, tools and Zip show the
    /// progress ring. The other actions (Copy path, Copy files, Copy to, Move
    /// to, Terminal) are done by the app: the wheel closes at once.
    fn after_pick(&mut self, item: &WheelItem) {
        if item.op.is_shell_action() {
            self.hide("action_pick");
        } else {
            self.enter_progress();
        }
    }

    // ----- click / keyboard mode ----------------------------------------

    fn pick_now(&mut self, hit: Hit) -> Option<Action> {
        let (group, category, item) = pick_target(&self.prepared, hit)?;
        let files = std::mem::take(&mut self.files);
        tracing::debug!(label = item.label, "click_pick");
        self.after_pick(&item);
        Some(Action::Pick(Pick { files, group, category, item }))
    }

    fn on_mouse_move(&mut self, cx: i32, cy: i32) {
        if !(self.keyboard && self.mode == Mode::Wheel) {
            return;
        }
        if !self.tracking {
            let mut t = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            // SAFETY: valid struct for our window.
            self.tracking = unsafe { TrackMouseEvent(&mut t) }.is_ok();
        }
        let hit = self.hit_client(cx, cy);
        if self.apply_hit(hit) {
            self.render();
        }
    }

    fn on_mouse_leave(&mut self) {
        self.tracking = false;
        if self.keyboard && self.mode == Mode::Wheel && self.hover != Hover::None {
            self.hover = Hover::None;
            self.kill(TIMER_DWELL);
            self.render();
        }
    }

    fn on_click(&mut self, cx: i32, cy: i32) -> Option<Action> {
        match self.mode {
            Mode::Done if self.outcome == Outcome::Succeeded => {
                self.hide("progress_clicked");
                Some(Action::ProgressClicked)
            }
            Mode::Wheel if self.keyboard => {
                let hit = self.hit_client(cx, cy);
                match hit {
                    Hit::Sector(i) if self.expands(i) => {
                        self.open_sub(i);
                        self.hover = Hover::Sector(i);
                        self.kill(TIMER_DWELL);
                        self.render();
                        None
                    }
                    Hit::Sector(_) | Hit::SubSector { .. } => self.pick_now(hit),
                    Hit::Center => self.close_cancel("click_center"),
                    Hit::Outside => None,
                }
            }
            _ => None,
        }
    }

    fn typeahead_matches(&self) -> Vec<usize> {
        let q = self.typeahead.to_ascii_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        if let Some(p) = self.sub_open {
            self.prepared.sectors.get(p).map_or(Vec::new(), |sec| {
                sec.sub_items()
                    .iter()
                    .enumerate()
                    .filter(|(_, it)| it.label.to_ascii_lowercase().starts_with(&q))
                    .map(|(i, _)| i)
                    .collect()
            })
        } else {
            self.prepared
                .sectors
                .iter()
                .enumerate()
                .filter(|(_, s)| {
                    s.label
                        .lines()
                        .next()
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        .starts_with(&q)
                })
                .map(|(i, _)| i)
                .collect()
        }
    }

    fn apply_typeahead_hover(&mut self) {
        let matches = self.typeahead_matches();
        if matches.is_empty() {
            return;
        }
        if let Some(p) = self.sub_open {
            self.hover = Hover::Sub(p, matches[0]);
        } else {
            self.hover = Hover::Sector(matches[0]);
        }
    }

    fn on_key(&mut self, vk: u32) -> Option<Action> {
        if !(self.keyboard && self.mode == Mode::Wheel) {
            return None;
        }
        let n = self.prepared.sectors.len();
        let sub_count = self.sub_ring().map_or(0, |s| s.count);
        match vk {
            0x1B => {
                if !self.typeahead.is_empty() {
                    self.typeahead.clear();
                    self.render();
                    return None;
                }
                if let Some(p) = self.sub_open {
                    self.close_sub();
                    self.hover = Hover::Sector(p);
                    self.render();
                    None
                } else {
                    self.close_cancel("esc")
                }
            }
            0x08 => {
                // Backspace: trim type-ahead.
                if self.typeahead.pop().is_some() {
                    self.apply_typeahead_hover();
                    self.render();
                }
                None
            }
            0x25..=0x28 => {
                self.typeahead.clear();
                let fwd = vk == 0x27 || vk == 0x28;
                if let Some(p) = self.sub_open {
                    let cur = match self.hover {
                        Hover::Sub(_, j) => Some(j),
                        _ => None,
                    };
                    self.hover = Hover::Sub(p, step_index(cur, sub_count, fwd));
                } else if n > 0 {
                    let cur = match self.hover {
                        Hover::Sector(i) => Some(i),
                        _ => None,
                    };
                    self.hover = Hover::Sector(step_index(cur, n, fwd));
                }
                self.render();
                None
            }
            0x31..=0x38 | 0x61..=0x68 => {
                self.typeahead.clear();
                let idx = if vk >= 0x61 { vk - 0x61 } else { vk - 0x31 } as usize;
                if let Some(p) = self.sub_open {
                    if idx < sub_count {
                        self.hover = Hover::Sub(p, idx);
                    }
                } else if idx < n {
                    self.hover = Hover::Sector(idx);
                }
                self.render();
                None
            }
            0x0D => {
                let matches = self.typeahead_matches();
                if matches.len() == 1 {
                    let i = matches[0];
                    self.typeahead.clear();
                    return if let Some(p) = self.sub_open {
                        self.pick_now(Hit::SubSector { group: p, sub: i })
                    } else if self.expands(i) {
                        self.open_sub(i);
                        self.hover = Hover::Sub(i, 0);
                        self.render();
                        None
                    } else {
                        self.pick_now(Hit::Sector(i))
                    };
                }
                match self.hover {
                    Hover::Sector(i) if self.expands(i) => {
                        self.typeahead.clear();
                        self.open_sub(i);
                        self.hover = Hover::Sub(i, 0);
                        self.render();
                        None
                    }
                    Hover::Sector(i) => self.pick_now(Hit::Sector(i)),
                    Hover::Sub(g, s) => self.pick_now(Hit::SubSector { group: g, sub: s }),
                    Hover::None => None,
                }
            }
            0x41..=0x5A => {
                // Type-ahead filter on sector / sub-item labels.
                if let Some(c) = char::from_u32(vk) {
                    self.typeahead.push(c.to_ascii_lowercase());
                    self.apply_typeahead_hover();
                    self.render();
                }
                None
            }
            _ => None,
        }
    }

    fn on_deactivate(&mut self) -> Option<Action> {
        if self.keyboard && !self.zone && self.mode == Mode::Wheel {
            self.close_cancel("focus_lost")
        } else {
            None
        }
    }

    // ----- progress ring -------------------------------------------------

    fn enter_progress(&mut self) {
        self.kill_timers();
        self.generation = self.generation.wrapping_add(1);
        self.mode = Mode::Progress;
        self.progress_target = 0.0;
        self.progress_display = 0.0;
        self.alpha = 1.0;
        self.content_alpha = 1.0;
        self.content_target = 1.0;
        self.wheel_scale = 1.0;
        self.hover = Hover::None;
        self.hover_boost = 0.0;
        self.hover_target = 0.0;
        self.sub_open = None;
        self.sub_alpha = 0.0;
        self.sub_target = 0.0;
        self.show_started = Some(Instant::now());
        self.keyboard = false;
        self.zone = false;
        if self.noactivate_removed {
            self.set_noactivate(true);
        }
        self.apply_region();
        self.render();
        self.assert_topmost();
        self.arm(TIMER_WATCHDOG, WATCHDOG_MS);
        if animations_enabled() {
            self.arm_anim();
        }
    }

    fn set_progress(&mut self, percent: f32) {
        if self.mode != Mode::Progress {
            return;
        }
        let p = if percent.is_finite() { percent.clamp(0.0, 100.0) } else { 0.0 };
        self.progress_target = p;
        self.arm(TIMER_WATCHDOG, WATCHDOG_MS);
        if animations_enabled() {
            self.arm_anim();
        } else {
            self.progress_display = p;
            self.render();
        }
    }

    fn complete(&mut self, outcome: Outcome) {
        if self.mode != Mode::Progress {
            return;
        }
        self.kill(TIMER_WATCHDOG);
        self.mode = Mode::Done;
        self.outcome = outcome;
        self.alpha = 1.0;
        self.render();
        self.arm(TIMER_DONE, DONE_MS);
    }

    fn hand_off(&mut self) {
        if matches!(self.mode, Mode::Progress | Mode::Done) {
            self.hide("hand_off");
        }
    }

    fn on_timer(&mut self, id: usize) -> Option<Action> {
        if id != TIMER_FADE && id != TIMER_ANIM {
            self.kill(id);
        }
        if self.timer_gen[id] != self.generation {
            return None; // stale
        }
        match (id, self.mode) {
            (TIMER_ANIM, _) => {
                if self.tick_anim() {
                    self.render();
                }
                None
            }
            (TIMER_EMPTY, Mode::Probe) => {
                tracing::debug!("empty_drag_timeout");
                self.hide("empty_drag_timeout");
                None
            }
            (TIMER_END, Mode::Wheel) if !self.keyboard => {
                tracing::debug!("drag_ended_no_drop");
                self.close_cancel("drag_ended_no_drop")
            }
            (TIMER_DWELL, Mode::Wheel) => {
                if let Hover::Sector(i) = self.hover {
                    if self.expands(i) && self.sub_open.is_none() {
                        self.open_sub(i);
                        self.render();
                    }
                }
                None
            }
            (TIMER_DONE, Mode::Done) => {
                if animations_enabled() {
                    self.fade_start = Some(Instant::now());
                    self.arm(TIMER_FADE, FADE_TICK_MS);
                } else {
                    self.hide("done");
                }
                None
            }
            (TIMER_FADE, Mode::Done) => {
                let t = self.fade_start.map_or(1.0, |s| s.elapsed().as_secs_f32() * 1000.0 / FADE_MS);
                if t >= 1.0 {
                    self.hide("done_faded");
                } else {
                    self.alpha = 1.0 - t;
                    self.render();
                }
                None
            }
            (TIMER_WATCHDOG, Mode::Progress) => {
                tracing::warn!("progress ring timed out");
                self.hide("progress_watchdog");
                None
            }
            (TIMER_ZONE, Mode::Wheel) if self.zone => self.close_cancel("zone_timeout"),
            (TIMER_PREVIEW, Mode::Wheel) => {
                Some(Action::HoverPreview(self.hover_preview_req()))
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Inner: state + callbacks (callbacks run with no state borrow held)
// ---------------------------------------------------------------------------

impl Inner {
    fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> Option<R> {
        // A re-entrant window message (sent during SetWindowPos etc.) finds the
        // state borrowed and is skipped.
        let mut g = self.st.try_borrow_mut().ok()?;
        Some(f(&mut g))
    }

    fn act(&self, a: Option<Action>) {
        let Some(a) = a else { return };
        let Ok(mut cb) = self.cb.try_borrow_mut() else {
            tracing::warn!("callback re-entered, event dropped");
            return;
        };
        let r = catch_unwind(AssertUnwindSafe(|| match a {
            Action::Pick(p) => (cb.on_pick)(p),
            Action::Cancel => (cb.on_cancel)(),
            Action::ProgressClicked => (cb.on_progress_clicked)(),
            Action::HoverPreview(req) => (cb.on_hover_preview)(req),
        }));
        if r.is_err() {
            tracing::error!("overlay callback panicked");
        }
    }

    fn drag_enter(&self, files: &[PathBuf]) -> bool {
        let t0 = Instant::now();
        if files.is_empty() || !self.with(|s| s.accepts_enter()).unwrap_or(false) {
            return false;
        }
        let res = {
            let Ok(mut cb) = self.cb.try_borrow_mut() else { return false };
            catch_unwind(AssertUnwindSafe(|| (cb.on_files_entered)(files))).unwrap_or_else(|_| {
                tracing::error!("on_files_entered panicked");
                None
            })
        };
        let Some((model, count)) = res else { return false };
        self.with(|s| s.enter_with_model(files, &model, count, t0));
        true
    }
}

struct Sink(Weak<Inner>);

impl DropSink for Sink {
    fn drag_enter(&self, files: &[PathBuf]) -> bool {
        self.0.upgrade().is_some_and(|i| i.drag_enter(files))
    }

    fn drag_over(&self, x: i32, y: i32) -> bool {
        self.0.upgrade().and_then(|i| i.with(|s| s.drag_over(x, y))).unwrap_or(false)
    }

    fn drag_leave(&self) {
        if let Some(i) = self.0.upgrade() {
            i.with(|s| s.drag_leave());
        }
    }

    fn drop_files(&self, files: &[PathBuf], x: i32, y: i32) -> bool {
        let Some(i) = self.0.upgrade() else { return false };
        let (accepted, action) = i.with(|s| s.on_drop(files, x, y)).unwrap_or((false, None));
        i.act(action);
        accepted
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        zest_shell::revoke_drop_target(self.hwnd);
        // SAFETY: our own window, on the UI thread.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

// ---------------------------------------------------------------------------
// Public handle
// ---------------------------------------------------------------------------

/// The wheel overlay. Cheap to clone (shared handle). UI thread only.
#[derive(Clone)]
pub struct Overlay {
    inner: Rc<Inner>,
}

impl Overlay {
    /// Create the window and all GPU resources, and register the drop target.
    /// The thread must be an STA (`OleInitialize`).
    pub fn new(callbacks: Callbacks) -> Result<Overlay> {
        let px0 = window_px(1.0, DEFAULT_OUTER_LOGICAL);
        // SAFETY: standard window creation on the UI thread.
        let hwnd = unsafe {
            let hinst = GetModuleHandleW(None).context("GetModuleHandleW")?;
            let class = w!("ZestOverlayWheel");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wheel_wndproc),
                hInstance: hinst.into(),
                lpszClassName: class,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                ..Default::default()
            };
            if RegisterClassW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
                anyhow::bail!("RegisterClassW (wheel) failed: {}", windows::core::Error::from_win32());
            }
            // No WS_EX_TRANSPARENT and no WS_EX_LAYERED: the OLE drag loop must
            // find this window with WindowFromPoint.
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP,
                class,
                w!("Zest wheel"),
                WS_POPUP,
                0,
                0,
                px0,
                px0,
                None,
                None,
                hinst,
                None,
            )
            .context("CreateWindowExW (wheel)")?
        };

        // SAFETY: hwnd was just created.
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
        let outer = DEFAULT_OUTER_LOGICAL;
        let px = window_px(scale, outer);
        // SAFETY: UI thread, valid hwnd.
        let gfx = match unsafe { Gfx::new(hwnd, scale, px) } {
            Ok(g) => g,
            Err(e) => {
                // SAFETY: our window.
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                return Err(e);
            }
        };
        let state = State {
            hwnd,
            gfx,
            scale,
            outer_logical: outer,
            layout: Layout::new(scale, outer),
            px,
            origin: (0, 0),
            mode: Mode::Hidden,
            keyboard: false,
            zone: false,
            noactivate_removed: false,
            theme: Theme::from_system(),
            generation: 0,
            timer_gen: [0; N_TIMERS],
            prepared: Prepared::empty(),
            gpu: None,
            files: Vec::new(),
            hover: Hover::None,
            sub_open: None,
            progress_target: 0.0,
            progress_display: 0.0,
            outcome: Outcome::Succeeded,
            alpha: 1.0,
            content_alpha: 1.0,
            content_target: 1.0,
            hover_boost: 0.0,
            hover_target: 0.0,
            sub_alpha: 0.0,
            sub_target: 0.0,
            wheel_scale: 1.0,
            fade_start: None,
            anim_last: None,
            show_started: None,
            tracking: false,
            first_frame_from: None,
            typeahead: String::new(),
            preview_token: 0,
            preview_text: String::new(),
        };
        state.apply_region();
        let inner = Rc::new(Inner { st: RefCell::new(state), cb: RefCell::new(callbacks), hwnd });
        CURRENT.with(|c| *c.borrow_mut() = Rc::downgrade(&inner));
        zest_shell::register_drop_target(hwnd, Rc::new(Sink(Rc::downgrade(&inner))))
            .context("RegisterDragDrop")?;
        // Render once so the first show never has stale/blank buffers.
        inner.with(|s| s.render_visual(Visual::Probe));
        tracing::info!(dpi, px, "overlay ready");
        Ok(Overlay { inner })
    }

    /// The wheel window.
    pub fn hwnd(&self) -> HWND {
        self.inner.hwnd
    }

    /// Drag mode: place the invisible window centered on (x, y) (screen
    /// pixels). It stays until files arrive, `on_drag_ended`, or 10 s.
    pub fn show_for_drag(&self, x: i32, y: i32) {
        self.inner.with(|s| s.show_for_drag(x, y));
    }

    /// Click mode for known files (hold gesture, chord on a selection).
    /// `file_count` is the number shown in the center.
    pub fn show_for_files(&self, x: i32, y: i32, files: Vec<PathBuf>, model: WheelModel, file_count: usize) {
        self.inner.with(|s| {
            s.show_click(x, y, false);
            s.files = files;
            s.set_prepared(prepare(&model, file_count));
            s.render();
        });
    }

    /// Click mode with no files: "Drop files here".
    pub fn show_drop_zone(&self, x: i32, y: i32) {
        self.inner.with(|s| s.show_click(x, y, true));
    }

    /// Replace the model of the wheel that is shown.
    pub fn set_model(&self, model: WheelModel, file_count: usize) {
        self.inner.with(|s| s.set_model(&model, file_count));
    }

    /// The button of the drag gesture was released.
    pub fn on_drag_ended(&self) {
        let a = self.inner.with(|s| s.on_drag_ended()).flatten();
        self.inner.act(a);
    }

    /// Hide at once. No callback.
    /// True while a drag/wheel/progress UI would make global Undo unsafe.
    pub fn blocks_undo(&self) -> bool {
        self.inner
            .with(|s| !matches!(s.mode, Mode::Hidden))
            .unwrap_or(true)
    }

    /// Apply a hover preview chip. Ignored when `token` is stale.
    pub fn set_hover_preview(&self, token: u64, text: Option<String>) {
        self.inner.with(|s| {
            if s.preview_token != token {
                return;
            }
            let next = text.unwrap_or_default();
            if s.preview_text != next {
                s.preview_text = next;
                s.render();
            }
        });
    }

    pub fn hide(&self) {
        self.inner.with(|s| s.hide("api"));
    }

    /// Whether the window is shown in any mode.
    pub fn is_visible(&self) -> bool {
        self.inner.st.try_borrow().map_or(true, |s| s.mode != Mode::Hidden)
    }

    /// Progress in percent (0 to 100) for the ring shown after a pick.
    pub fn set_progress(&self, percent: f32) {
        self.inner.with(|s| s.set_progress(percent));
    }

    /// Show the result glyph, then fade out.
    pub fn complete(&self, outcome: Outcome) {
        self.inner.with(|s| s.complete(outcome));
    }

    /// Hide the progress ring at once (the app shows the job elsewhere).
    pub fn hand_off(&self) {
        self.inner.with(|s| s.hand_off());
    }

    /// Outer radius of the main ring in logical pixels (config
    /// `wheel.outer_radius`). Applied at the next show.
    pub fn set_outer_radius(&self, logical: u32) {
        self.inner.with(|s| {
            let r = (logical as f32).clamp(80.0, 400.0);
            if s.mode == Mode::Hidden {
                let scale = s.scale;
                s.relayout(scale, r);
            } else {
                s.outer_logical = r;
            }
        });
    }
}

unsafe extern "system" fn wheel_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let xy = || (lparam.0 as i16 as i32, (lparam.0 >> 16) as i16 as i32);
    match msg {
        WM_MOUSEACTIVATE => {
            let keyboard = with_inner(|i| i.with(|s| s.keyboard)).flatten().unwrap_or(false);
            if keyboard {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            } else {
                LRESULT(3) // MA_NOACTIVATE
            }
        }
        WM_TIMER if (TIMER_ANIM..N_TIMERS).contains(&wparam.0) => {
            with_inner(|i| {
                let a = i.with(|s| s.on_timer(wparam.0)).flatten();
                i.act(a);
            });
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = xy();
            with_inner(|i| i.with(|s| s.on_mouse_move(x, y)));
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            with_inner(|i| i.with(|s| s.on_mouse_leave()));
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = xy();
            with_inner(|i| {
                let a = i.with(|s| s.on_click(x, y)).flatten();
                i.act(a);
            });
            LRESULT(0)
        }
        WM_KEYDOWN => {
            with_inner(|i| {
                let a = i.with(|s| s.on_key(wparam.0 as u32)).flatten();
                i.act(a);
            });
            LRESULT(0)
        }
        WM_ACTIVATE => {
            // WA_INACTIVE == 0: focus loss closes click mode.
            if (wparam.0 & 0xFFFF) == 0 {
                with_inner(|i| {
                    let a = i.with(|s| s.on_deactivate()).flatten();
                    i.act(a);
                });
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_DPICHANGED => {
            // Size is recomputed at show time from the monitor under the cursor.
            tracing::debug!(dpi = wparam.0 & 0xFFFF, "wm_dpichanged");
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let _ = ValidateRect(hwnd, None);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
