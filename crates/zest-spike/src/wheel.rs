//! The wheel window: Win32 window, DirectComposition + Direct2D rendering,
//! wheel state, and the pure geometry helpers.
//!
//! The window and all GPU resources are created once at startup and stay
//! alive. The window is hidden between uses. Rendering happens only on state
//! changes.

use std::cell::RefCell;
use std::f32::consts::PI;
use std::mem::{size_of, ManuallyDrop};

use anyhow::{anyhow, Context, Result};
use windows::core::{w, Interface};
use windows::Win32::Foundation::{HMODULE, HWND, LPARAM, LRESULT, POINT, TRUE, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED,
    D2D1_FIGURE_END_CLOSED, D2D1_PIXEL_FORMAT, D2D_POINT_2F, D2D_RECT_F, D2D_SIZE_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1DeviceContext, ID2D1Factory1, ID2D1Image, ID2D1PathGeometry,
    ID2D1SolidColorBrush, D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_SMALL, D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
    D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1, D2D1_DEVICE_CONTEXT_OPTIONS_NONE,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_SWEEP_DIRECTION_CLOCKWISE, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_CENTER,
};
use windows::Win32::Graphics::Dwm::DwmGetColorizationColor;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN,
    DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIAdapter, IDXGIDevice, IDXGIFactory2, IDXGIOutput, IDXGISurface, IDXGISwapChain1,
    DXGI_PRESENT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows::Win32::Graphics::Gdi::{
    CreateEllipticRgn, GetMonitorInfoW, MonitorFromPoint, SetWindowRgn, ValidateRect, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetWindowLongPtrW, KillTimer,
    LoadCursorW, RegisterClassW, SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, GWL_EXSTYLE, HWND_TOPMOST, IDC_ARROW,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_HIDE, WM_ACTIVATE, WM_DPICHANGED, WM_ERASEBKGND,
    WM_KEYDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_PAINT, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE,
    WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::log;

pub const OUTER_LOGICAL: f32 = 140.0;
pub const DEAD_LOGICAL: f32 = 32.0;
pub const SECTORS: usize = 6;
/// Drag that never brought files: hide after this long (missed button release).
const TIMER_EMPTY: usize = 2;
const EMPTY_LIMIT_MS: u32 = 10_000;
/// After the button release with files shown: wait this long for the drop.
const TIMER_END: usize = 3;
const END_GRACE_MS: u32 = 250;
const TITLE: &str = "Convert";
pub const LABELS: [&str; SECTORS] = ["PNG", "JPG", "WEBP", "AVIF", "MP4", "MP3"];

// ---------------------------------------------------------------------------
// Pure model and geometry (unit-tested)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum WheelState {
    Hidden,
    /// Window shown but fully transparent, waiting for `DragEnter` with files.
    Probe,
    Visible {
        file_count: usize,
        hovered: Option<usize>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// Which sector is under the point (dx, dy) relative to the wheel center.
/// Screen coordinates (y grows down). Sector 0 starts at the top and sectors
/// run clockwise. Returns `None` inside the dead zone or outside `outer`.
pub fn sector_at(dx: f32, dy: f32, dead_zone: f32, outer: f32, count: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let r = (dx * dx + dy * dy).sqrt();
    if r < dead_zone || r > outer {
        return None;
    }
    // Angle clockwise from "up".
    let mut ang = dx.atan2(-dy);
    if ang < 0.0 {
        ang += 2.0 * PI;
    }
    let step = 2.0 * PI / count as f32;
    let idx = (ang / step).floor() as usize;
    Some(idx.min(count - 1))
}

/// Top-left of a `size` x `size` window centered on (cx, cy), shifted so that
/// it stays inside `work`. If the work area is smaller than the window, the
/// top-left corner of the work area wins.
pub fn clamp_origin(cx: i32, cy: i32, size: i32, work: Rect) -> (i32, i32) {
    let x = (cx - size / 2).min(work.right - size).max(work.left);
    let y = (cy - size / 2).min(work.bottom - size).max(work.top);
    (x, y)
}

/// Window size in physical pixels for a DPI scale (1.0 = 96 dpi).
pub fn px_for_scale(scale: f32) -> i32 {
    (2.0 * OUTER_LOGICAL * scale).round() as i32
}

// ---------------------------------------------------------------------------
// Graphics resources
// ---------------------------------------------------------------------------

const TRANSPARENT: D2D1_COLOR_F = D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };

struct Gfx {
    _d3d: ID3D11Device,
    swap: IDXGISwapChain1,
    _dcomp: IDCompositionDevice,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    factory: ID2D1Factory1,
    dc: ID2D1DeviceContext,
    dwrite: IDWriteFactory,
    fmt_label: IDWriteTextFormat,
    fmt_small: IDWriteTextFormat,
    br_disc: ID2D1SolidColorBrush,
    br_dead: ID2D1SolidColorBrush,
    br_text: ID2D1SolidColorBrush,
    br_line: ID2D1SolidColorBrush,
    br_accent: ID2D1SolidColorBrush,
    geoms: Vec<ID2D1PathGeometry>,
    px: i32,
    scale: f32,
}

fn color(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

fn polar(c: f32, r: f32, ang: f32) -> D2D_POINT_2F {
    D2D_POINT_2F {
        x: c + r * ang.sin(),
        y: c - r * ang.cos(),
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

impl Gfx {
    /// SAFETY: call on the UI thread with a valid HWND.
    unsafe fn new(hwnd: HWND, scale: f32) -> Result<Self> {
        let px = px_for_scale(scale);

        let mut device: Option<ID3D11Device> = None;
        let hw = D3D11CreateDevice(
            None::<&IDXGIAdapter>,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        );
        if hw.is_err() || device.is_none() {
            log::event("d3d_hardware_failed_using_warp", "");
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
            .context("D3D11CreateDevice (WARP)")?;
        }
        let device = device.ok_or_else(|| anyhow!("no D3D11 device"))?;
        let dxgi_device: IDXGIDevice = device.cast()?;
        let adapter = dxgi_device.GetAdapter()?;
        let factory: IDXGIFactory2 = adapter.GetParent()?;

        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: px as u32,
            Height: px as u32,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: false.into(),
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            AlphaMode: DXGI_ALPHA_MODE_PREMULTIPLIED,
            Flags: 0,
        };
        let swap = factory
            .CreateSwapChainForComposition(&device, &desc, None::<&IDXGIOutput>)
            .context("CreateSwapChainForComposition")?;

        let dcomp: IDCompositionDevice =
            DCompositionCreateDevice(&dxgi_device).context("DCompositionCreateDevice")?;
        let target = dcomp.CreateTargetForHwnd(hwnd, TRUE)?;
        let visual = dcomp.CreateVisual()?;
        visual.SetContent(&swap)?;
        target.SetRoot(&visual)?;
        dcomp.Commit()?;

        let d2d_factory: ID2D1Factory1 =
            D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)
                .context("D2D1CreateFactory")?;
        let d2d_device = d2d_factory.CreateDevice(&dxgi_device)?;
        let dc = d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;

        let dwrite: IDWriteFactory =
            DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).context("DWriteCreateFactory")?;
        let (fmt_label, fmt_small) = make_formats(&dwrite, scale)?;

        let br = |c: D2D1_COLOR_F| dc.CreateSolidColorBrush(&c, None);
        let br_disc = br(color(0.08, 0.09, 0.11, 0.90))?;
        let br_dead = br(color(0.03, 0.03, 0.04, 0.95))?;
        let br_text = br(color(1.0, 1.0, 1.0, 0.96))?;
        let br_line = br(color(1.0, 1.0, 1.0, 0.14))?;
        let br_accent = br(color(0.0, 0.47, 0.84, 0.95))?;

        let mut gfx = Self {
            _d3d: device,
            swap,
            _dcomp: dcomp,
            _target: target,
            _visual: visual,
            factory: d2d_factory,
            dc,
            dwrite,
            fmt_label,
            fmt_small,
            br_disc,
            br_dead,
            br_text,
            br_line,
            br_accent,
            geoms: Vec::new(),
            px,
            scale,
        };
        gfx.geoms = gfx.build_geoms()?;
        Ok(gfx)
    }

    /// Resize the swap chain and rebuild scale-dependent resources.
    /// SAFETY: UI thread; no outstanding references to the swap chain buffers.
    unsafe fn set_scale(&mut self, scale: f32) -> Result<()> {
        let px = px_for_scale(scale);
        if px != self.px {
            self.swap
                .ResizeBuffers(0, px as u32, px as u32, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0))
                .context("ResizeBuffers")?;
            self.px = px;
        }
        self.scale = scale;
        let (l, s) = make_formats(&self.dwrite, scale)?;
        self.fmt_label = l;
        self.fmt_small = s;
        self.geoms = self.build_geoms()?;
        Ok(())
    }

    fn center(&self) -> f32 {
        self.px as f32 / 2.0
    }

    fn outer_draw(&self) -> f32 {
        self.center() - 1.0
    }

    fn dead(&self) -> f32 {
        DEAD_LOGICAL * self.scale
    }

    /// One annular sector path per wheel sector.
    /// SAFETY: UI thread.
    unsafe fn build_geoms(&self) -> Result<Vec<ID2D1PathGeometry>> {
        let c = self.center();
        let (ro, ri) = (self.outer_draw(), self.dead());
        let step = 2.0 * PI / SECTORS as f32;
        let mut out = Vec::with_capacity(SECTORS);
        for i in 0..SECTORS {
            let (a0, a1) = (i as f32 * step, (i + 1) as f32 * step);
            let geom: ID2D1PathGeometry = self.factory.CreatePathGeometry()?.into();
            let sink = geom.Open()?;
            sink.BeginFigure(polar(c, ro, a0), D2D1_FIGURE_BEGIN_FILLED);
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: polar(c, ro, a1),
                size: D2D_SIZE_F { width: ro, height: ro },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: D2D1_ARC_SIZE_SMALL,
            });
            sink.AddLine(polar(c, ri, a1));
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: polar(c, ri, a0),
                size: D2D_SIZE_F { width: ri, height: ri },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
                arcSize: D2D1_ARC_SIZE_SMALL,
            });
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            sink.Close()?;
            out.push(geom);
        }
        Ok(out)
    }

    /// Draw one frame for `state` and present it.
    /// SAFETY: UI thread.
    unsafe fn draw(
        &self,
        state: &WheelState,
        drop_zone: bool,
        accent: D2D1_COLOR_F,
    ) -> Result<()> {
        let surface: IDXGISurface = self.swap.GetBuffer(0)?;
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            colorContext: ManuallyDrop::new(None),
        };
        let bitmap = self.dc.CreateBitmapFromDxgiSurface(&surface, Some(&props))?;
        self.dc.SetTarget(&bitmap);
        self.dc.BeginDraw();
        self.dc.Clear(Some(&TRANSPARENT));
        match state {
            WheelState::Hidden | WheelState::Probe => {}
            WheelState::Visible { file_count, hovered } => {
                self.draw_wheel(*file_count, *hovered, drop_zone, accent);
            }
        }
        let end = self.dc.EndDraw(None, None);
        // Release the buffer reference before Present (flip model).
        self.dc.SetTarget(None::<&ID2D1Image>);
        drop(bitmap);
        drop(surface);
        end.context("EndDraw")?;
        self.swap.Present(0, DXGI_PRESENT(0)).ok().context("Present")?;
        Ok(())
    }

    unsafe fn draw_disc(&self, brush: &ID2D1SolidColorBrush) {
        let c = self.center();
        let r = self.outer_draw();
        self.dc.FillEllipse(
            &D2D1_ELLIPSE {
                point: D2D_POINT_2F { x: c, y: c },
                radiusX: r,
                radiusY: r,
            },
            brush,
        );
    }

    unsafe fn text(&self, s: &str, fmt: &IDWriteTextFormat, cx: f32, cy: f32, hw: f32, hh: f32) {
        let rect = D2D_RECT_F {
            left: cx - hw,
            top: cy - hh,
            right: cx + hw,
            bottom: cy + hh,
        };
        self.dc.DrawText(
            &wide(s),
            fmt,
            &rect,
            &self.br_text,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }

    unsafe fn draw_wheel(
        &self,
        file_count: usize,
        hovered: Option<usize>,
        drop_zone: bool,
        accent: D2D1_COLOR_F,
    ) {
        let c = self.center();
        let (ro, ri) = (self.outer_draw(), self.dead());
        let s = self.scale;
        self.draw_disc(&self.br_disc);

        if let Some(i) = hovered {
            if let Some(g) = self.geoms.get(i) {
                self.br_accent.SetColor(&accent);
                self.dc.FillGeometry(g, &self.br_accent, None::<&windows::Win32::Graphics::Direct2D::ID2D1Brush>);
            }
        }

        let step = 2.0 * PI / SECTORS as f32;
        for i in 0..SECTORS {
            let a = i as f32 * step;
            self.dc
                .DrawLine(polar(c, ri, a), polar(c, ro, a), &self.br_line, 1.0 * s, None::<&windows::Win32::Graphics::Direct2D::ID2D1StrokeStyle>);
        }

        // Dead zone.
        self.dc.FillEllipse(
            &D2D1_ELLIPSE {
                point: D2D_POINT_2F { x: c, y: c },
                radiusX: ri,
                radiusY: ri,
            },
            &self.br_dead,
        );

        // Sector labels.
        let rm = ri + (ro - ri) * 0.52;
        for (i, label) in LABELS.iter().enumerate() {
            let a = (i as f32 + 0.5) * step;
            let p = polar(c, rm, a);
            self.text(label, &self.fmt_label, p.x, p.y, 38.0 * s, 12.0 * s);
        }

        // Center text.
        let center = if drop_zone {
            "Drop files here".to_string()
        } else if file_count == 1 {
            format!("{TITLE}\n1 file")
        } else {
            format!("{TITLE}\n{file_count} files")
        };
        self.text(&center, &self.fmt_small, c, c, 30.0 * s, 20.0 * s);
    }

}

/// SAFETY: UI thread. Text formats are cached per scale.
unsafe fn make_formats(
    dwrite: &IDWriteFactory,
    scale: f32,
) -> Result<(IDWriteTextFormat, IDWriteTextFormat)> {
    let mk = |size: f32| -> Result<IDWriteTextFormat> {
        let f = dwrite.CreateTextFormat(
            w!("Segoe UI"),
            None::<&windows::Win32::Graphics::DirectWrite::IDWriteFontCollection>,
            DWRITE_FONT_WEIGHT_SEMI_BOLD,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size * scale,
            w!("en-us"),
        )?;
        f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
        f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        Ok(f)
    };
    Ok((mk(12.5)?, mk(10.0)?))
}

// ---------------------------------------------------------------------------
// Wheel (window + state)
// ---------------------------------------------------------------------------

pub struct Wheel {
    hwnd: HWND,
    gfx: Gfx,
    scale: f32,
    px: i32,
    /// Window top-left in screen pixels.
    origin: (i32, i32),
    state: WheelState,
    drop_zone: bool,
    keyboard: bool,
    noactivate_removed: bool,
    accent: D2D1_COLOR_F,
    /// Bumped whenever the window is shown or hidden; stale timers compare it.
    generation: u32,
    timer_gen: u32,
    /// The drag gesture's button is still down.
    drag_active: bool,
    t_drag_started: i64,
    t_drag_enter: i64,
    pending_first_frame: bool,
}

thread_local! {
    static WHEEL: RefCell<Option<Wheel>> = const { RefCell::new(None) };
}

/// Run `f` on the wheel. Returns `None` if the wheel does not exist yet or is
/// already borrowed (re-entrant window message): callers just skip then.
pub fn with<R>(f: impl FnOnce(&mut Wheel) -> R) -> Option<R> {
    WHEEL.with(|c| {
        let mut g = c.try_borrow_mut().ok()?;
        let w = g.as_mut()?;
        Some(f(w))
    })
}

/// Create the window and all GPU resources. Returns the wheel HWND.
pub fn init() -> Result<HWND> {
    // SAFETY: standard window creation on the UI thread.
    let hwnd = unsafe {
        let hinst = GetModuleHandleW(None).context("GetModuleHandleW")?;
        let class = w!("ZestSpikeWheel");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wheel_wndproc),
            hInstance: hinst.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
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
            px_for_scale(1.0),
            px_for_scale(1.0),
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
    // SAFETY: UI thread, valid hwnd.
    let gfx = unsafe { Gfx::new(hwnd, scale)? };
    let px = gfx.px;
    let mut wheel = Wheel {
        hwnd,
        gfx,
        scale,
        px,
        origin: (0, 0),
        state: WheelState::Hidden,
        drop_zone: false,
        keyboard: false,
        noactivate_removed: false,
        accent: color(0.0, 0.47, 0.84, 0.95),
        generation: 0,
        timer_gen: 0,
        drag_active: false,
        t_drag_started: 0,
        t_drag_enter: 0,
        pending_first_frame: false,
    };
    wheel.apply_region();
    // Render once so the first show never has stale/blank buffers.
    wheel.render();
    WHEEL.with(|c| *c.borrow_mut() = Some(wheel));
    log::event("wheel_ready", &format!("dpi={dpi} px={px}"));
    Ok(hwnd)
}

/// Destroy the window and drop GPU resources. Call after `RevokeDragDrop`.
pub fn shutdown(hwnd: HWND) {
    WHEEL.with(|c| {
        if let Ok(mut g) = c.try_borrow_mut() {
            g.take();
        }
    });
    // SAFETY: our own window, on the UI thread.
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

impl Wheel {
    pub fn is_open(&self) -> bool {
        !matches!(self.state, WheelState::Hidden)
    }

    pub fn state(&self) -> WheelState {
        self.state
    }

    pub fn file_count_shown(&self) -> usize {
        match self.state {
            WheelState::Visible { file_count, .. } => file_count,
            _ => 0,
        }
    }

    fn apply_region(&self) {
        // SAFETY: the region is owned by the system after a successful
        // SetWindowRgn; on failure we free it ourselves (not needed here
        // because failure only occurs for invalid windows).
        unsafe {
            let rgn = CreateEllipticRgn(0, 0, self.px, self.px);
            if SetWindowRgn(self.hwnd, rgn, TRUE) == 0 {
                log::event("set_window_rgn_failed", "");
            }
        }
    }

    /// Pick the monitor under `pt`, adapt DPI scale if needed and compute the
    /// clamped window origin.
    fn prepare(&mut self, pt: POINT) -> (i32, i32) {
        // SAFETY: plain Win32 queries and a resize on the UI thread.
        unsafe {
            let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
            let (mut dx, mut dy) = (0u32, 0u32);
            let scale = match GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) {
                Ok(()) if dx > 0 => dx as f32 / 96.0,
                _ => self.scale,
            };
            if (scale - self.scale).abs() > f32::EPSILON {
                match self.gfx.set_scale(scale) {
                    Ok(()) => {
                        self.scale = scale;
                        self.px = self.gfx.px;
                        self.apply_region();
                        log::event("dpi_scale_change", &format!("dpi={dx} px={}", self.px));
                    }
                    Err(e) => log::event("dpi_scale_error", &format!("err={e}")),
                }
            }
            let mut mi = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
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
            let origin = clamp_origin(pt.x, pt.y, self.px, work);
            let half = self.px / 2;
            let dead = (DEAD_LOGICAL * self.scale) as i32;
            let (ox, oy) = (pt.x - (origin.0 + half), pt.y - (origin.1 + half));
            if ox * ox + oy * oy > dead * dead {
                log::event("pointer_outside_dead_zone", &format!("dx={ox} dy={oy}"));
            }
            origin
        }
    }

    fn show_window(&mut self, origin: (i32, i32)) {
        self.origin = origin;
        // SAFETY: our window; SWP_NOACTIVATE keeps focus where it is.
        let r = unsafe {
            SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                origin.0,
                origin.1,
                self.px,
                self.px,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
        };
        if let Err(e) = r {
            log::event("show_window_error", &format!("err={e}"));
        }
    }

    fn refresh_accent(&mut self) {
        let (mut argb, mut opaque) = (0u32, Default::default());
        // SAFETY: out-pointers to locals.
        if unsafe { DwmGetColorizationColor(&mut argb, &mut opaque) }.is_ok() {
            self.accent = color(
                ((argb >> 16) & 0xFF) as f32 / 255.0,
                ((argb >> 8) & 0xFF) as f32 / 255.0,
                (argb & 0xFF) as f32 / 255.0,
                0.95,
            );
        }
    }

    fn cursor() -> POINT {
        let mut p = POINT::default();
        // SAFETY: out-pointer to a local.
        let _ = unsafe { GetCursorPos(&mut p) };
        p
    }

    /// Draw the current state and present it.
    fn render(&mut self) {
        // SAFETY: UI thread, buffers are not referenced elsewhere.
        let r = unsafe { self.gfx.draw(&self.state, self.drop_zone, self.accent) };
        match r {
            Ok(()) => {
                if self.pending_first_frame {
                    self.pending_first_frame = false;
                    log::event(
                        "first_frame",
                        &format!("ms_since_drag_enter={:.3}", log::ms_since(self.t_drag_enter)),
                    );
                }
            }
            Err(e) => log::event("render_error", &format!("err={e:#}")),
        }
    }

    /// Drag gesture started: place the invisible window under the cursor. It
    /// stays until files arrive, the button is released, or the safety limit.
    pub fn show_drag(&mut self, t_start: i64) {
        if self.is_open() {
            self.hide("drag_restart");
        }
        self.keyboard = false;
        self.drop_zone = false;
        self.drag_active = true;
        self.t_drag_started = t_start;
        self.generation = self.generation.wrapping_add(1);
        let pt = Self::cursor();
        let origin = self.prepare(pt);
        self.state = WheelState::Probe;
        self.render(); // fully transparent clear
        self.show_window(origin);
        self.assert_topmost();
        self.arm_timer(TIMER_EMPTY, EMPTY_LIMIT_MS);
        log::event(
            "window_shown",
            &format!("ms_since_drag_started={:.3}", log::ms_since(t_start)),
        );
    }

    fn assert_topmost(&self) {
        // SAFETY: our own window; no move, resize or activation.
        let r = unsafe {
            SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
        };
        if let Err(e) = r {
            log::event("assert_topmost_error", &format!("err={e}"));
        }
    }

    fn arm_timer(&mut self, id: usize, ms: u32) {
        self.timer_gen = self.generation;
        // SAFETY: our own window; WM_TIMER goes to the wheel proc.
        unsafe {
            SetTimer(self.hwnd, id, ms, None);
        }
    }

    fn kill_timers(&self) {
        // SAFETY: our own window.
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_EMPTY);
            let _ = KillTimer(self.hwnd, TIMER_END);
        }
    }

    /// The button of the drag gesture was released.
    pub fn on_drag_ended(&mut self) {
        self.drag_active = false;
        if self.keyboard {
            return;
        }
        match self.state {
            WheelState::Probe => self.hide("drag_ended_no_files"),
            WheelState::Visible { .. } => {
                // A drop arrives around the same time as the release: give it a moment.
                self.kill_timers();
                self.arm_timer(TIMER_END, END_GRACE_MS);
            }
            WheelState::Hidden => {}
        }
    }

    /// Open the wheel from the hotkey or hold gesture (click/keyboard mode).
    /// The window is activated.
    pub fn show_keyboard(&mut self, file_count: usize, drop_zone: bool) {
        if self.is_open() {
            self.hide("keyboard_reopen");
        }
        self.keyboard = true;
        self.drop_zone = drop_zone;
        self.generation = self.generation.wrapping_add(1);
        self.refresh_accent();
        let pt = Self::cursor();
        let origin = self.prepare(pt);
        self.state = WheelState::Visible { file_count, hovered: None };
        self.render();
        // SAFETY: our own window; style bits only.
        unsafe {
            let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, ex & !(WS_EX_NOACTIVATE.0 as isize));
        }
        self.noactivate_removed = true;
        self.show_window(origin);
        // SAFETY: allowed after a hotkey; may be refused after the hold gesture.
        let ok = unsafe { SetForegroundWindow(self.hwnd) }.as_bool();
        log::event(
            "keyboard_mode",
            &format!("foreground_ok={ok} files={file_count} drop_zone={drop_zone}"),
        );
    }

    pub fn hide(&mut self, reason: &str) {
        self.generation = self.generation.wrapping_add(1);
        self.kill_timers();
        // SAFETY: our own window.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
            if self.noactivate_removed {
                let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE);
                SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, ex | WS_EX_NOACTIVATE.0 as isize);
            }
        }
        self.noactivate_removed = false;
        self.keyboard = false;
        self.drop_zone = false;
        self.pending_first_frame = false;
        if self.state != WheelState::Hidden {
            self.state = WheelState::Hidden;
            log::event("hide", &format!("reason={reason}"));
        }
    }

    fn on_timer(&mut self, id: usize) {
        // SAFETY: our own window.
        unsafe {
            let _ = KillTimer(self.hwnd, id);
        }
        if self.timer_gen != self.generation {
            return; // stale
        }
        match (id, self.state) {
            (TIMER_EMPTY, WheelState::Probe) => {
                log::event("empty_drag_timeout", &format!("after_ms={EMPTY_LIMIT_MS}"));
                self.hide("empty_drag_timeout");
            }
            (TIMER_END, WheelState::Visible { .. }) if !self.keyboard => {
                log::event("drag_ended_no_drop", &format!("grace_ms={END_GRACE_MS}"));
                self.hide("drag_ended_no_drop");
            }
            _ => {}
        }
    }

    /// Sector under a physical screen point, or `None`.
    pub fn sector_at_screen(&self, x: i32, y: i32) -> Option<usize> {
        let half = self.px as f32 / 2.0;
        let dx = x as f32 - (self.origin.0 as f32 + half);
        let dy = y as f32 - (self.origin.1 as f32 + half);
        sector_at(dx, dy, DEAD_LOGICAL * self.scale, half, SECTORS)
    }

    /// `DragEnter` with `files` local files. Zero files: stay invisible.
    pub fn on_drag_enter(&mut self, files: usize, t_enter: i64) {
        if files == 0 {
            return;
        }
        self.kill_timers();
        self.refresh_accent();
        self.t_drag_enter = t_enter;
        self.pending_first_frame = true;
        self.drop_zone = false;
        self.state = WheelState::Visible { file_count: files, hovered: None };
        self.render();
    }

    /// Update the hovered sector from a drag position. Returns the sector.
    pub fn on_drag_over(&mut self, x: i32, y: i32, t0: i64) -> Option<usize> {
        let WheelState::Visible { file_count, hovered } = self.state else {
            return None;
        };
        let sector = self.sector_at_screen(x, y);
        self.set_hover(sector, hovered, file_count, t0);
        sector
    }

    fn set_hover(&mut self, sector: Option<usize>, old: Option<usize>, file_count: usize, t0: i64) {
        if sector != old {
            self.state = WheelState::Visible { file_count, hovered: sector };
            self.render();
            log::event(
                "hover",
                &format!(
                    "sector={} latency_ms={:.3}",
                    sector.map_or("none".to_string(), |s| s.to_string()),
                    log::ms_since(t0)
                ),
            );
        }
    }

    /// Drag left the window: clear the highlight but keep the wheel.
    pub fn on_drag_leave(&mut self) {
        if let WheelState::Visible { file_count, hovered } = self.state {
            self.set_hover(None, hovered, file_count, log::qpc_now());
        }
    }

    pub fn label(&self, sector: usize) -> &'static str {
        LABELS.get(sector).copied().unwrap_or("?")
    }

    // ----- keyboard / click mode ------------------------------------------

    fn pick(&mut self, i: usize, file_count: usize, via: &str) {
        log::event(
            "keyboard_pick",
            &format!("sector={i} label={} files={file_count} via={via}", self.label(i)),
        );
        self.hide("keyboard_pick");
    }

    fn on_mouse_move(&mut self, cx: i32, cy: i32) {
        if !self.keyboard {
            return;
        }
        if let WheelState::Visible { file_count, hovered } = self.state {
            let s = self.sector_at_screen(self.origin.0 + cx, self.origin.1 + cy);
            self.set_hover(s, hovered, file_count, log::qpc_now());
        }
    }

    fn on_click(&mut self, cx: i32, cy: i32) {
        if !self.keyboard {
            return;
        }
        if let WheelState::Visible { file_count, .. } = self.state {
            match self.sector_at_screen(self.origin.0 + cx, self.origin.1 + cy) {
                Some(i) => self.pick(i, file_count, "click"),
                None => self.hide("click_center"),
            }
        }
    }

    fn on_key(&mut self, vk: u32) {
        let WheelState::Visible { file_count, hovered } = self.state else {
            return;
        };
        let set = |s: &mut Self, h: Option<usize>| {
            s.state = WheelState::Visible { file_count, hovered: h };
            s.render();
        };
        match vk {
            0x1B => self.hide("esc"),
            0x25 => {
                let n = hovered.map_or(SECTORS - 1, |h| (h + SECTORS - 1) % SECTORS);
                set(self, Some(n));
            }
            0x27 => {
                let n = hovered.map_or(0, |h| (h + 1) % SECTORS);
                set(self, Some(n));
            }
            0x31..=0x36 => set(self, Some((vk - 0x31) as usize)),
            0x61..=0x66 => set(self, Some((vk - 0x61) as usize)),
            0x0D => match hovered {
                Some(i) => self.pick(i, file_count, "key"),
                None => log::event("keyboard_pick_none", ""),
            },
            _ => {}
        }
    }
}

unsafe extern "system" fn wheel_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => {
            let keyboard = with(|w| w.keyboard).unwrap_or(false);
            if keyboard {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            } else {
                LRESULT(3) // MA_NOACTIVATE
            }
        }
        WM_TIMER if wparam.0 == TIMER_EMPTY || wparam.0 == TIMER_END => {
            with(|w| w.on_timer(wparam.0));
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = (lparam.0 as i16 as i32, (lparam.0 >> 16) as i16 as i32);
            with(|w| w.on_mouse_move(x, y));
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = (lparam.0 as i16 as i32, (lparam.0 >> 16) as i16 as i32);
            with(|w| w.on_click(x, y));
            LRESULT(0)
        }
        WM_KEYDOWN => {
            with(|w| w.on_key(wparam.0 as u32));
            LRESULT(0)
        }
        WM_ACTIVATE => {
            // WA_INACTIVE == 0: focus loss closes keyboard mode.
            if (wparam.0 & 0xFFFF) == 0 {
                with(|w| {
                    if w.keyboard {
                        w.hide("focus_lost");
                    }
                });
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_DPICHANGED => {
            // Size is recomputed at show time from the monitor under the cursor.
            log::event("wm_dpichanged", &format!("dpi={}", wparam.0 & 0xFFFF));
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

#[cfg(test)]
mod tests {
    use super::*;

    const DEAD: f32 = 32.0;
    const OUTER: f32 = 140.0;

    /// Point at `deg` degrees clockwise from up, radius r.
    fn at(deg: f32, r: f32) -> (f32, f32) {
        let a = deg.to_radians();
        (r * a.sin(), -r * a.cos())
    }

    #[test]
    fn six_sectors_clockwise_from_top() {
        for (deg, want) in [(30.0, 0), (90.0, 1), (150.0, 2), (210.0, 3), (270.0, 4), (330.0, 5)] {
            let (dx, dy) = at(deg, 90.0);
            assert_eq!(sector_at(dx, dy, DEAD, OUTER, 6), Some(want), "deg={deg}");
        }
    }

    #[test]
    fn sector_zero_starts_at_top() {
        // Just right of straight up is sector 0, just left is the last one.
        assert_eq!(sector_at(1.0, -80.0, DEAD, OUTER, 6), Some(0));
        assert_eq!(sector_at(-1.0, -80.0, DEAD, OUTER, 6), Some(5));
    }

    #[test]
    fn dead_zone_and_outside_are_none() {
        assert_eq!(sector_at(0.0, 0.0, DEAD, OUTER, 6), None);
        assert_eq!(sector_at(10.0, -10.0, DEAD, OUTER, 6), None);
        assert_eq!(sector_at(0.0, -141.0, DEAD, OUTER, 6), None);
        assert_eq!(sector_at(200.0, 200.0, DEAD, OUTER, 6), None);
    }

    #[test]
    fn radius_edges() {
        assert_eq!(sector_at(0.5, -32.0, DEAD, OUTER, 6), Some(0));
        assert_eq!(sector_at(0.5, -139.9, DEAD, OUTER, 6), Some(0));
    }

    #[test]
    fn works_for_other_counts() {
        // 8 sectors: 45 degrees each.
        for (deg, want) in [(22.5, 0), (67.5, 1), (112.5, 2), (337.5, 7)] {
            let (dx, dy) = at(deg, 90.0);
            assert_eq!(sector_at(dx, dy, DEAD, OUTER, 8), Some(want), "deg={deg}");
        }
        assert_eq!(sector_at(10.0, -90.0, DEAD, OUTER, 0), None);
    }

    fn work() -> Rect {
        Rect { left: 0, top: 0, right: 1920, bottom: 1040 }
    }

    #[test]
    fn clamp_keeps_center_when_inside() {
        assert_eq!(clamp_origin(960, 500, 280, work()), (820, 360));
    }

    #[test]
    fn clamp_shifts_at_edges() {
        assert_eq!(clamp_origin(5, 5, 280, work()), (0, 0));
        assert_eq!(clamp_origin(1915, 1035, 280, work()), (1640, 760));
        assert_eq!(clamp_origin(1915, 500, 280, work()), (1640, 360));
    }

    #[test]
    fn clamp_with_negative_monitor_coordinates() {
        let left = Rect { left: -1920, top: -200, right: 0, bottom: 880 };
        assert_eq!(clamp_origin(-3, 400, 280, left), (-280, 260));
        assert_eq!(clamp_origin(-1919, -199, 280, left), (-1920, -200));
    }

    #[test]
    fn clamp_when_work_area_is_smaller_than_window() {
        let tiny = Rect { left: 100, top: 100, right: 200, bottom: 200 };
        assert_eq!(clamp_origin(150, 150, 280, tiny), (100, 100));
    }

    #[test]
    fn size_scales_with_dpi() {
        assert_eq!(px_for_scale(1.0), 280);
        assert_eq!(px_for_scale(1.5), 420);
        assert_eq!(px_for_scale(2.0), 560);
    }

    #[test]
    fn labels_match_sector_count() {
        assert_eq!(LABELS.len(), SECTORS);
    }
}






