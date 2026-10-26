//! Direct3D 11 / DXGI composition swap chain / DirectComposition / Direct2D /
//! DirectWrite renderer.
//!
//! GPU resources are created once. Text layouts and path geometries for a
//! model are built when the model is set ([`ModelGpu`]), so a frame does not
//! allocate (apart from the flip-model target bitmap, as in the spike).

use std::cell::RefCell;
use std::f32::consts::PI;
use std::mem::ManuallyDrop;

use anyhow::{anyhow, Context, Result};
use windows::core::{w, Interface};
use windows::Win32::Foundation::{HMODULE, HWND, TRUE};
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_BEGIN_HOLLOW,
    D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN, D2D1_PIXEL_FORMAT, D2D_POINT_2F, D2D_SIZE_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Brush, ID2D1DeviceContext, ID2D1Factory1, ID2D1Image,
    ID2D1PathGeometry, ID2D1SolidColorBrush, ID2D1StrokeStyle, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
    D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL, D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
    D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1, D2D1_CAP_STYLE_ROUND,
    D2D1_DASH_STYLE_SOLID, D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_LINE_JOIN_ROUND,
    D2D1_QUADRATIC_BEZIER_SEGMENT, D2D1_STROKE_STYLE_PROPERTIES1, D2D1_STROKE_TRANSFORM_TYPE_NORMAL,
    D2D1_SWEEP_DIRECTION_CLOCKWISE, D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteFontCollection, IDWriteTextFormat,
    IDWriteTextLayout, DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_CENTER,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN,
    DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIAdapter, IDXGIDevice, IDXGIFactory2, IDXGIOutput, IDXGISurface, IDXGISwapChain1,
    DXGI_PRESENT, DXGI_SCALING_STRETCH, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};

use crate::geometry::{sub_span, Layout, PROGRESS_RADIUS_LOGICAL};
use crate::model::Prepared;
use crate::theme::{Rgba, Theme};
use crate::Outcome;

const TRANSPARENT: D2D1_COLOR_F = D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };

// Logical sizes of text boxes.
const LABEL_W: f32 = 80.0;
const LABEL_H: f32 = 36.0;
const SUB_LABEL_W: f32 = 52.0;
const SUB_LABEL_H: f32 = 36.0;
const CENTER_W: f32 = 60.0;
const CENTER_H: f32 = 40.0;
const PCT_W: f32 = 48.0;
const PCT_H: f32 = 22.0;
/// Gap between the main ring and the sub-ring (logical).
const SUB_GAP: f32 = 3.0;
const RING_THICKNESS: f32 = 5.0;
/// Soft fill under the progress track (not a solid second disc).
const PROGRESS_DISC_ALPHA: f32 = 0.18;
/// Angular gap between slices (radians at mid-radius ≈ 2.5 logical px).
const SLICE_GAP_PX: f32 = 2.8;
/// Corner radius of each slice (logical px).
const SLICE_CORNER_LOGICAL: f32 = 8.0;

/// What is hovered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hover {
    None,
    Sector(usize),
    /// (parent sector, item)
    Sub(usize, usize),
}

/// What to draw.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Visual {
    /// Fully transparent (waiting for DragEnter).
    Probe,
    Wheel,
    /// Progress ring, `frac` in 0..=1.
    Progress { frac: f32 },
    /// Result glyph, `alpha` in 0..=1 (fade-out).
    Done { outcome: Outcome, alpha: f32 },
}

pub struct Scene<'a> {
    pub visual: Visual,
    pub prepared: &'a Prepared,
    pub gpu: Option<&'a ModelGpu>,
    pub hover: Hover,
    pub sub_open: Option<usize>,
    pub theme: &'a Theme,
    pub layout: Layout,
    pub scale: f32,
    /// Wheel appear / content fade (0..=1).
    pub content_alpha: f32,
    /// Hover sector fill strength (0..=1).
    pub hover_boost: f32,
    /// Sub-ring fade-in (0..=1).
    pub sub_alpha: f32,
    /// Appear spring scale (1 = rest). Drawn about the window center.
    pub wheel_scale: f32,
    /// Seconds since show — drives sheen / stagger phase.
    pub anim_time: f32,
    /// Optional hover preview chip text (empty = none).
    pub preview_text: &'a str,
}

/// Text layouts and path geometries of one model, built once per model/scale.
pub struct ModelGpu {
    main_geoms: Vec<ID2D1PathGeometry>,
    main_labels: Vec<IDWriteTextLayout>,
    sub_geoms: Vec<Vec<ID2D1PathGeometry>>,
    sub_labels: Vec<Vec<IDWriteTextLayout>>,
    center_main: IDWriteTextLayout,
    center_by_sector: Vec<Option<IDWriteTextLayout>>,
}

struct Formats {
    label: IDWriteTextFormat,
    small: IDWriteTextFormat,
    sub: IDWriteTextFormat,
    pct: IDWriteTextFormat,
}

struct PctCache {
    key: u32,
    arc: Option<ID2D1PathGeometry>,
    text: IDWriteTextLayout,
}

pub struct Gfx {
    _d3d: ID3D11Device,
    swap: IDXGISwapChain1,
    _dcomp: IDCompositionDevice,
    _target: IDCompositionTarget,
    _visual: IDCompositionVisual,
    factory: ID2D1Factory1,
    dc: ID2D1DeviceContext,
    dwrite: IDWriteFactory,
    fmt: Formats,
    brush: ID2D1SolidColorBrush,
    round_stroke: ID2D1StrokeStyle,
    pct: RefCell<Option<PctCache>>,
    px: i32,
}

fn polar(c: f32, r: f32, ang: f32) -> D2D_POINT_2F {
    D2D_POINT_2F { x: c + r * ang.sin(), y: c - r * ang.cos() }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn d2d(c: Rgba, alpha: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: c.r, g: c.g, b: c.b, a: c.a * alpha }
}

impl Gfx {
    /// SAFETY: call on the UI thread with a valid HWND.
    pub unsafe fn new(hwnd: HWND, scale: f32, px: i32) -> Result<Self> {
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
            tracing::warn!("D3D11 hardware device failed, using WARP");
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
            D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None).context("D2D1CreateFactory")?;
        let d2d_device = d2d_factory.CreateDevice(&dxgi_device)?;
        let dc = d2d_device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;

        let dwrite: IDWriteFactory =
            DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).context("DWriteCreateFactory")?;
        let fmt = make_formats(&dwrite, scale)?;
        let brush = dc.CreateSolidColorBrush(&TRANSPARENT, None)?;
        let round_props = D2D1_STROKE_STYLE_PROPERTIES1 {
            startCap: D2D1_CAP_STYLE_ROUND,
            endCap: D2D1_CAP_STYLE_ROUND,
            dashCap: D2D1_CAP_STYLE_ROUND,
            lineJoin: D2D1_LINE_JOIN_ROUND,
            miterLimit: 10.0,
            dashStyle: D2D1_DASH_STYLE_SOLID,
            dashOffset: 0.0,
            transformType: D2D1_STROKE_TRANSFORM_TYPE_NORMAL,
        };
        let round_stroke: ID2D1StrokeStyle = d2d_factory.CreateStrokeStyle(&round_props, None)?.into();

        Ok(Self {
            _d3d: device,
            swap,
            _dcomp: dcomp,
            _target: target,
            _visual: visual,
            factory: d2d_factory,
            dc,
            dwrite,
            fmt,
            brush,
            round_stroke,
            pct: RefCell::new(None),
            px,
        })
    }

    pub fn px(&self) -> i32 {
        self.px
    }

    /// Resize the swap chain and rebuild scale-dependent text formats.
    /// SAFETY: UI thread; no outstanding references to the swap chain buffers.
    pub unsafe fn set_scale(&mut self, scale: f32, px: i32) -> Result<()> {
        if px != self.px {
            self.swap
                .ResizeBuffers(0, px as u32, px as u32, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0))
                .context("ResizeBuffers")?;
            self.px = px;
        }
        self.fmt = make_formats(&self.dwrite, scale)?;
        *self.pct.borrow_mut() = None;
        Ok(())
    }

    fn center(&self) -> f32 {
        self.px as f32 / 2.0
    }

    fn paint(&self, c: Rgba, alpha: f32) -> &ID2D1SolidColorBrush {
        // SAFETY: plain property set on our own brush.
        unsafe { self.brush.SetColor(&d2d(c, alpha)) };
        &self.brush
    }

    /// Annular sector with rounded corners and a small angular gap — glass-tile slices.
    /// SAFETY: UI thread.
    unsafe fn rounded_annulus(
        &self,
        c: f32,
        ri: f32,
        ro: f32,
        a0: f32,
        a1: f32,
        scale: f32,
    ) -> Result<ID2D1PathGeometry> {
        let a1 = a1.min(a0 + 2.0 * PI - 0.001);
        let span = (a1 - a0).max(0.02);
        let mid_r = (ri + ro) * 0.5;
        let gap = (SLICE_GAP_PX * scale / mid_r.max(1.0)).min(span * 0.35);
        let a0 = a0 + gap * 0.5;
        let a1 = a1 - gap * 0.5;
        let span = (a1 - a0).max(0.02);
        let mut cr = SLICE_CORNER_LOGICAL * scale;
        cr = cr.min((ro - ri) * 0.38).min(mid_r * span * 0.45).max(2.0 * scale);
        let ao = (cr / ro.max(1.0)).min(span * 0.28);
        let ai = (cr / ri.max(8.0 * scale)).min(span * 0.28);

        let large = if (a1 - ao) - (a0 + ao) > PI {
            D2D1_ARC_SIZE_LARGE
        } else {
            D2D1_ARC_SIZE_SMALL
        };
        let large_i = if (a1 - ai) - (a0 + ai) > PI {
            D2D1_ARC_SIZE_LARGE
        } else {
            D2D1_ARC_SIZE_SMALL
        };

        let geom: ID2D1PathGeometry = self.factory.CreatePathGeometry()?.into();
        let sink = geom.Open()?;
        // Outer arc, then quadratic fillets at the four corners.
        sink.BeginFigure(polar(c, ro, a0 + ao), D2D1_FIGURE_BEGIN_FILLED);
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: polar(c, ro, a1 - ao),
            size: D2D_SIZE_F { width: ro, height: ro },
            rotationAngle: 0.0,
            sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
            arcSize: large,
        });
        sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT {
            point1: polar(c, ro, a1),
            point2: polar(c, ro - cr, a1),
        });
        sink.AddLine(polar(c, ri + cr, a1));
        sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT {
            point1: polar(c, ri, a1),
            point2: polar(c, ri, a1 - ai),
        });
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: polar(c, ri, a0 + ai),
            size: D2D_SIZE_F { width: ri, height: ri },
            rotationAngle: 0.0,
            sweepDirection: D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE,
            arcSize: large_i,
        });
        sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT {
            point1: polar(c, ri, a0),
            point2: polar(c, ri + cr, a0),
        });
        sink.AddLine(polar(c, ro - cr, a0));
        sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT {
            point1: polar(c, ro, a0),
            point2: polar(c, ro, a0 + ao),
        });
        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        sink.Close()?;
        Ok(geom)
    }

    /// Open arc of `frac` (0..1) of a circle, starting at the top.
    /// SAFETY: UI thread.
    unsafe fn arc(&self, c: f32, r: f32, frac: f32) -> Result<ID2D1PathGeometry> {
        let ang = (frac * 2.0 * PI).clamp(0.01, 2.0 * PI - 0.01);
        let geom: ID2D1PathGeometry = self.factory.CreatePathGeometry()?.into();
        let sink = geom.Open()?;
        sink.BeginFigure(polar(c, r, 0.0), D2D1_FIGURE_BEGIN_HOLLOW);
        sink.AddArc(&D2D1_ARC_SEGMENT {
            point: polar(c, r, ang),
            size: D2D_SIZE_F { width: r, height: r },
            rotationAngle: 0.0,
            sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
            arcSize: if ang > PI { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
        });
        sink.EndFigure(D2D1_FIGURE_END_OPEN);
        sink.Close()?;
        Ok(geom)
    }

    unsafe fn text_layout(&self, s: &str, fmt: &IDWriteTextFormat, w: f32, h: f32) -> Result<IDWriteTextLayout> {
        Ok(self.dwrite.CreateTextLayout(&wide(s), fmt, w, h)?)
    }

    /// Build all layouts and geometries for a model.
    /// SAFETY: UI thread.
    pub unsafe fn build_model(&self, p: &Prepared, l: &Layout, scale: f32) -> Result<ModelGpu> {
        let c = self.center();
        let n = p.sectors.len();
        let step = 2.0 * PI / n.max(1) as f32;
        // Inset the ring so the soft outer AA rim of the disc stays visible.
        let rim = 2.0 * scale;
        let (ro, ri) = ((l.r_in - 1.0 - rim).max(l.dead + 8.0 * scale), l.dead + rim);
        let (so, si) = (l.r_sub - 1.0 - rim, l.r_in + SUB_GAP * scale);

        let mut main_geoms = Vec::with_capacity(n);
        let mut main_labels = Vec::with_capacity(n);
        let mut sub_geoms = Vec::with_capacity(n);
        let mut sub_labels = Vec::with_capacity(n);
        let mut center_by_sector = Vec::with_capacity(n);
        for (i, s) in p.sectors.iter().enumerate() {
            main_geoms.push(self.rounded_annulus(
                c,
                ri,
                ro,
                i as f32 * step,
                (i + 1) as f32 * step,
                scale,
            )?);
            main_labels.push(self.text_layout(&s.label, &self.fmt.label, LABEL_W * scale, LABEL_H * scale)?);
            let items = s.sub_items();
            let mut geoms = Vec::with_capacity(items.len());
            let mut labels = Vec::with_capacity(items.len());
            if !items.is_empty() {
                let (start, span) = sub_span(n, i, items.len());
                let each = span / items.len() as f32;
                for (j, it) in items.iter().enumerate() {
                    let a0 = start + j as f32 * each;
                    geoms.push(self.rounded_annulus(c, si, so, a0, a0 + each, scale)?);
                    labels.push(self.text_layout(it.label, &self.fmt.sub, SUB_LABEL_W * scale, SUB_LABEL_H * scale)?);
                }
            }
            sub_geoms.push(geoms);
            sub_labels.push(labels);
            center_by_sector.push(match &s.kind {
                crate::model::SectorKind::Expand { center, .. } => {
                    Some(self.text_layout(center, &self.fmt.small, CENTER_W * scale, CENTER_H * scale)?)
                }
                crate::model::SectorKind::Item(_) => None,
            });
        }
        let center_main = self.text_layout(&p.center, &self.fmt.small, CENTER_W * scale, CENTER_H * scale)?;
        Ok(ModelGpu { main_geoms, main_labels, sub_geoms, sub_labels, center_main, center_by_sector })
    }

    /// Draw one frame and present it.
    /// SAFETY: UI thread.
    pub unsafe fn draw(&self, scene: &Scene) -> Result<()> {
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
        self.dc.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
        self.dc.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        self.dc.Clear(Some(&TRANSPARENT));
        let c = self.center();
        let sc = scene.wheel_scale.clamp(0.75, 1.2);
        let m = if (sc - 1.0).abs() > 0.001 {
            Matrix3x2 {
                M11: sc,
                M12: 0.0,
                M21: 0.0,
                M22: sc,
                M31: c * (1.0 - sc),
                M32: c * (1.0 - sc),
            }
        } else {
            Matrix3x2 {
                M11: 1.0,
                M12: 0.0,
                M21: 0.0,
                M22: 1.0,
                M31: 0.0,
                M32: 0.0,
            }
        };
        self.dc.SetTransform(&m);
        let drawn = match scene.visual {
            Visual::Probe => Ok(()),
            Visual::Wheel => self.draw_wheel(scene),
            Visual::Progress { frac } => self.draw_progress(scene, frac),
            Visual::Done { outcome, alpha } => self.draw_done(scene, outcome, alpha),
        };
        self.dc.SetTransform(&Matrix3x2 {
            M11: 1.0,
            M12: 0.0,
            M21: 0.0,
            M22: 1.0,
            M31: 0.0,
            M32: 0.0,
        });
        let end = self.dc.EndDraw(None, None);
        // Release the buffer reference before Present (flip model).
        self.dc.SetTarget(None::<&ID2D1Image>);
        drop(bitmap);
        drop(surface);
        drawn?;
        end.context("EndDraw")?;
        self.swap.Present(0, DXGI_PRESENT(0)).ok().context("Present")?;
        Ok(())
    }

    unsafe fn fill_disc(&self, r: f32, color: Rgba, alpha: f32) {
        let c = self.center();
        self.dc.FillEllipse(
            &D2D1_ELLIPSE { point: D2D_POINT_2F { x: c, y: c }, radiusX: r, radiusY: r },
            self.paint(color, alpha),
        );
    }

    unsafe fn draw_layout(&self, layout: &IDWriteTextLayout, cx: f32, cy: f32, w: f32, h: f32, color: Rgba) {
        self.draw_layout_a(layout, cx, cy, w, h, color, 1.0);
    }

    unsafe fn draw_layout_a(
        &self,
        layout: &IDWriteTextLayout,
        cx: f32,
        cy: f32,
        w: f32,
        h: f32,
        color: Rgba,
        alpha: f32,
    ) {
        self.dc.DrawTextLayout(
            D2D_POINT_2F { x: cx - w / 2.0, y: cy - h / 2.0 },
            layout,
            self.paint(color, alpha),
            D2D1_DRAW_TEXT_OPTIONS_NONE,
        );
    }

    unsafe fn dot_a(&self, cx: f32, cy: f32, r: f32, color: Rgba, alpha: f32) {
        self.dc.FillEllipse(
            &D2D1_ELLIPSE { point: D2D_POINT_2F { x: cx, y: cy }, radiusX: r, radiusY: r },
            self.paint(color, alpha),
        );
    }

    unsafe fn draw_wheel(&self, sc: &Scene) -> Result<()> {
        let c = self.center();
        let s = sc.scale;
        let t = sc.theme;
        let l = &sc.layout;
        let ca = sc.content_alpha.clamp(0.0, 1.0);
        let rim = 2.0 * s;
        let ri = l.dead;
        let n = sc.prepared.sectors.len();
        let step = 2.0 * PI / n.max(1) as f32;

        // No base disc — only floating glass slices + center label.

        if let Some(gpu) = sc.gpu {
            let slice_ro = (l.r_in - 1.0 - rim).max(ri + 8.0 * s);
            let slice_ri = ri + rim;
            let rm = slice_ri + (slice_ro - slice_ri) * 0.52;

            for (i, spec) in sc.prepared.sectors.iter().enumerate() {
                let Some(g) = gpu.main_geoms.get(i) else { continue };
                // Staggered reveal — each tile blooms in after the previous.
                let stagger = ((sc.anim_time * 4.2) - i as f32 * 0.085).clamp(0.0, 1.0);
                let stagger = stagger * stagger * (3.0 - 2.0 * stagger);
                let a = ca * stagger;
                if a < 0.01 {
                    continue;
                }

                let hot = matches!(sc.hover, Hover::Sector(j) if j == i);
                let hot_amt = if hot { sc.hover_boost } else { 0.0 };

                // Depth wash under the tile, then glass body.
                self.dc.FillGeometry(g, self.paint(t.shadow, a * 0.22), None::<&ID2D1Brush>);
                self.dc.FillGeometry(g, self.paint(t.slice, a), None::<&ID2D1Brush>);
                if hot_amt > 0.01 {
                    self.dc.FillGeometry(
                        g,
                        self.paint(t.accent, a * hot_amt * 0.78),
                        None::<&ID2D1Brush>,
                    );
                    self.dc.FillGeometry(
                        g,
                        self.paint(t.slice_hot, a * hot_amt),
                        None::<&ID2D1Brush>,
                    );
                }

                // Specular rim + emboss (light outside, dark inside).
                self.dc.DrawGeometry(
                    g,
                    self.paint(t.glass_hi, a * (0.50 + 0.40 * hot_amt)),
                    1.35 * s,
                    Some(&self.round_stroke),
                );
                self.dc.DrawGeometry(
                    g,
                    self.paint(t.glass_lo, a * 0.45),
                    0.85 * s,
                    Some(&self.round_stroke),
                );

                let ang = (i as f32 + 0.5) * step;
                let p = polar(c, rm, ang);
                let color = if hot && hot_amt > 0.45 {
                    t.text_on_accent
                } else {
                    t.text
                };
                if let Some(lay) = gpu.main_labels.get(i) {
                    self.draw_layout_a(lay, p.x, p.y, LABEL_W * s, LABEL_H * s, color, a);
                }
                if spec.lossy {
                    let d = polar(c, slice_ro - 8.0 * s, ang);
                    self.dot_a(d.x, d.y, 2.6 * s, t.lossy, a);
                }
            }

            let center = match sc.hover {
                Hover::Sector(i) | Hover::Sub(i, _) => {
                    gpu.center_by_sector.get(i).and_then(Option::as_ref)
                }
                Hover::None => None,
            }
            .unwrap_or(&gpu.center_main);
            self.draw_layout_a(center, c, c, CENTER_W * s, CENTER_H * s, t.text, ca);

            if let Some(parent) = sc.sub_open {
                self.draw_sub_ring(sc, gpu, parent);
            }
        }

        if !sc.preview_text.is_empty() && ca > 0.2 {
            let chip_w = 140.0 * s;
            let chip_h = 28.0 * s;
            let x = c + l.r_in * 0.55;
            let y = c + l.r_in * 0.55;
            // Soft chip behind the label (bottom-right of the wheel).
            let brush = self.paint(t.disc, ca * 0.85);
            self.dc.FillRectangle(
                &windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F {
                    left: x - chip_w / 2.0,
                    top: y - chip_h / 2.0,
                    right: x + chip_w / 2.0,
                    bottom: y + chip_h / 2.0,
                },
                brush,
            );
            if let Ok(lay) = self.text_layout(sc.preview_text, &self.fmt.small, chip_w, chip_h) {
                self.draw_layout_a(&lay, x, y, chip_w, chip_h, t.text, ca);
            }
        }
        Ok(())
    }

    unsafe fn draw_sub_ring(&self, sc: &Scene, gpu: &ModelGpu, parent: usize) {
        let c = self.center();
        let s = sc.scale;
        let t = sc.theme;
        let sa = (sc.sub_alpha * sc.content_alpha).clamp(0.0, 1.0);
        if sa < 0.01 {
            return;
        }
        let n = sc.prepared.sectors.len();
        let (Some(geoms), Some(labels)) = (gpu.sub_geoms.get(parent), gpu.sub_labels.get(parent)) else {
            return;
        };
        let items = sc.prepared.sectors[parent].sub_items();
        let (si, so) = (sc.layout.r_in + SUB_GAP * s, sc.layout.r_sub - 1.0);
        let (start, span) = sub_span(n, parent, items.len());
        let each = span / items.len().max(1) as f32;
        let rm = si + (so - si) * 0.5;
        for (j, g) in geoms.iter().enumerate() {
            let stagger = ((sc.anim_time * 5.0) - j as f32 * 0.06).clamp(0.0, 1.0);
            let stagger = stagger * stagger * (3.0 - 2.0 * stagger);
            let a = sa * stagger;
            if a < 0.01 {
                continue;
            }
            let hovered = sc.hover == Hover::Sub(parent, j);
            self.dc.FillGeometry(g, self.paint(t.shadow, a * 0.2), None::<&ID2D1Brush>);
            self.dc.FillGeometry(g, self.paint(t.sub_disc, a), None::<&ID2D1Brush>);
            if hovered {
                self.dc.FillGeometry(g, self.paint(t.accent, a * 0.82), None::<&ID2D1Brush>);
                self.dc.FillGeometry(g, self.paint(t.slice_hot, a), None::<&ID2D1Brush>);
            }
            self.dc.DrawGeometry(
                g,
                self.paint(t.glass_hi, a * 0.55),
                1.1 * s,
                Some(&self.round_stroke),
            );
            self.dc.DrawGeometry(
                g,
                self.paint(t.glass_lo, a * 0.35),
                0.7 * s,
                Some(&self.round_stroke),
            );
            let ang = start + (j as f32 + 0.5) * each;
            let p = polar(c, rm, ang);
            let color = if hovered { t.text_on_accent } else { t.text };
            if let Some(lay) = labels.get(j) {
                self.draw_layout_a(lay, p.x, p.y, SUB_LABEL_W * s, SUB_LABEL_H * s, color, a);
            }
            if let Some(it) = items.get(j) {
                if it.lossy {
                    let d = polar(c, so - 7.0 * s, ang);
                    self.dot_a(d.x, d.y, 2.5 * s, t.lossy, a);
                }
            }
        }
    }

    /// Progress ring geometry/text for an integer percent, cached.
    unsafe fn pct_cache(&self, scale: f32, pct: u32, with_arc: bool) -> Result<()> {
        let mut slot = self.pct.borrow_mut();
        if let Some(pc) = slot.as_ref() {
            if pc.key == pct && (pc.arc.is_some() || !with_arc) {
                return Ok(());
            }
        }
        let c = self.center();
        let r_mid = (PROGRESS_RADIUS_LOGICAL - RING_THICKNESS / 2.0) * scale;
        let arc = if with_arc && pct > 0 { Some(self.arc(c, r_mid, pct as f32 / 100.0)?) } else { None };
        let text = self.text_layout(&format!("{pct}%"), &self.fmt.pct, PCT_W * scale, PCT_H * scale)?;
        *slot = Some(PctCache { key: pct, arc, text });
        Ok(())
    }

    unsafe fn ring_base(&self, sc: &Scene, alpha: f32) -> (f32, f32) {
        let s = sc.scale;
        let t = sc.theme;
        let c = self.center();
        let r_out = PROGRESS_RADIUS_LOGICAL * s;
        let thick = RING_THICKNESS * s;
        let r_mid = r_out - thick / 2.0;
        // Soft glass, not a solid badge covering the file.
        self.fill_disc(r_out, t.disc, alpha * PROGRESS_DISC_ALPHA);
        self.dc.DrawEllipse(
            &D2D1_ELLIPSE { point: D2D_POINT_2F { x: c, y: c }, radiusX: r_mid, radiusY: r_mid },
            self.paint(t.track, alpha),
            thick,
            Some(&self.round_stroke),
        );
        (r_mid, thick)
    }

    unsafe fn draw_progress(&self, sc: &Scene, frac: f32) -> Result<()> {
        let pct = (frac.clamp(0.0, 1.0) * 100.0).round() as u32;
        self.pct_cache(sc.scale, pct, true)?;
        let c = self.center();
        let t = sc.theme;
        let s = sc.scale;
        let r_out = PROGRESS_RADIUS_LOGICAL * s;
        // Soft disc shadow under the badge.
        self.dc.FillEllipse(
            &D2D1_ELLIPSE {
                point: D2D_POINT_2F { x: c, y: c + 2.0 * s },
                radiusX: r_out,
                radiusY: r_out,
            },
            self.paint(t.shadow, 0.35),
        );
        let (r_mid, thick) = self.ring_base(sc, 1.0);
        if let Some(pc) = self.pct.borrow().as_ref() {
            if let Some(arc) = &pc.arc {
                self.dc.DrawGeometry(
                    arc,
                    self.paint(t.accent, 1.0),
                    thick,
                    Some(&self.round_stroke),
                );
            }
            // Traveling sheen on the track.
            let a0 = (sc.anim_time * 2.4) % (2.0 * PI);
            let a1 = a0 + 0.55;
            let geom: ID2D1PathGeometry = self.factory.CreatePathGeometry()?.into();
            let sink = geom.Open()?;
            sink.BeginFigure(polar(c, r_mid, a0), D2D1_FIGURE_BEGIN_HOLLOW);
            sink.AddArc(&D2D1_ARC_SEGMENT {
                point: polar(c, r_mid, a1),
                size: D2D_SIZE_F { width: r_mid, height: r_mid },
                rotationAngle: 0.0,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: D2D1_ARC_SIZE_SMALL,
            });
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
            sink.Close()?;
            self.dc.DrawGeometry(
                &geom,
                self.paint(t.glass_hi, 0.55),
                thick * 0.85,
                Some(&self.round_stroke),
            );
            self.draw_layout(&pc.text, c, c, PCT_W * s, PCT_H * s, t.text);
        }
        Ok(())
    }

    unsafe fn draw_done(&self, sc: &Scene, outcome: Outcome, alpha: f32) -> Result<()> {
        let s = sc.scale;
        let t = sc.theme;
        let c = self.center();
        let color = match outcome {
            Outcome::Succeeded => t.ok,
            Outcome::Failed => t.fail,
            Outcome::Cancelled => t.muted,
        };
        let r_out = PROGRESS_RADIUS_LOGICAL * s;
        let thick = RING_THICKNESS * s;
        let r_mid = r_out - thick / 2.0;
        self.fill_disc(r_out, t.disc, alpha * PROGRESS_DISC_ALPHA);
        self.dc.DrawEllipse(
            &D2D1_ELLIPSE { point: D2D_POINT_2F { x: c, y: c }, radiusX: r_mid, radiusY: r_mid },
            self.paint(color, alpha),
            thick,
            Some(&self.round_stroke),
        );
        let pt = |x: f32, y: f32| D2D_POINT_2F { x: c + x * s, y: c + y * s };
        let w = 3.0 * s;
        let brush = self.paint(color, alpha);
        let line = |a: D2D_POINT_2F, b: D2D_POINT_2F| {
            self.dc.DrawLine(a, b, brush, w, Some(&self.round_stroke));
        };
        match outcome {
            Outcome::Succeeded => {
                line(pt(-9.0, 1.0), pt(-3.0, 7.0));
                line(pt(-3.0, 7.0), pt(9.5, -7.0));
            }
            Outcome::Failed | Outcome::Cancelled => {
                line(pt(-7.0, -7.0), pt(7.0, 7.0));
                line(pt(7.0, -7.0), pt(-7.0, 7.0));
            }
        }
        Ok(())
    }
}

/// SAFETY: UI thread. Text formats are cached per scale.
unsafe fn make_formats(dwrite: &IDWriteFactory, scale: f32) -> Result<Formats> {
    crate::fonts::ensure_inter();
    let mk = |size: f32| -> Result<IDWriteTextFormat> {
        // Inter (bundled) → Segoe UI Variable → Segoe UI.
        let families = [w!("Inter"), w!("Segoe UI Variable"), w!("Segoe UI")];
        let mut last = None;
        for family in families {
            match dwrite.CreateTextFormat(
                family,
                None::<&IDWriteFontCollection>,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size * scale,
                w!("en-us"),
            ) {
                Ok(f) => {
                    f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
                    f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                    return Ok(f);
                }
                Err(e) => last = Some(e),
            }
        }
        Err(anyhow!("CreateTextFormat failed: {last:?}"))
    };
    Ok(Formats {
        label: mk(12.75)?,
        small: mk(10.25)?,
        sub: mk(11.5)?,
        pct: mk(13.0)?,
    })
}
