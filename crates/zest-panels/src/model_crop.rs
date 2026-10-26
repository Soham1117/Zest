//! Crop selection geometry (pure, no Windows calls). All numbers are pixels of
//! the full-size image (after the EXIF rotation), as `f64`.

/// A rectangle by its position and size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sel {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Sel {
    pub fn right(&self) -> f64 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
    pub fn contains(&self, px: f64, py: f64) -> bool {
        px >= self.x && px <= self.right() && py >= self.y && py <= self.bottom()
    }
}

/// What the pointer grabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grab {
    /// Outside: a drag makes a new selection.
    New,
    /// Inside: a drag moves the selection.
    Move,
    N,
    S,
    E,
    W,
    NE,
    NW,
    SE,
    SW,
}

/// Aspect presets. `ratio` is width / height; `None` = free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aspect {
    Free,
    Square,
    FourThree,
    SixteenNine,
}

impl Aspect {
    pub const ALL: [Aspect; 4] = [Aspect::Free, Aspect::Square, Aspect::FourThree, Aspect::SixteenNine];

    pub fn label(self) -> &'static str {
        match self {
            Aspect::Free => "Free",
            Aspect::Square => "1:1",
            Aspect::FourThree => "4:3",
            Aspect::SixteenNine => "16:9",
        }
    }

    pub fn ratio(self) -> Option<f64> {
        match self {
            Aspect::Free => None,
            Aspect::Square => Some(1.0),
            Aspect::FourThree => Some(4.0 / 3.0),
            Aspect::SixteenNine => Some(16.0 / 9.0),
        }
    }
}

/// Smallest selection edge in image pixels.
pub const MIN_SIDE: f64 = 1.0;

/// What is under the point (image pixels). `tol` is the grab distance in
/// image pixels (the handle size on screen divided by the zoom).
pub fn grab_at(sel: Option<Sel>, px: f64, py: f64, tol: f64) -> Grab {
    let Some(s) = sel else { return Grab::New };
    let near = |a: f64, b: f64| (a - b).abs() <= tol;
    let in_x = px >= s.x - tol && px <= s.right() + tol;
    let in_y = py >= s.y - tol && py <= s.bottom() + tol;
    if !in_x || !in_y {
        return Grab::New;
    }
    // A selection smaller than the grab zones: the middle moves.
    let (l, r, t, b) = (near(px, s.x), near(px, s.right()), near(py, s.y), near(py, s.bottom()));
    match (l, r, t, b) {
        (true, _, true, _) => Grab::NW,
        (_, true, true, _) => Grab::NE,
        (true, _, _, true) => Grab::SW,
        (_, true, _, true) => Grab::SE,
        (true, ..) => Grab::W,
        (_, true, ..) => Grab::E,
        (_, _, true, _) => Grab::N,
        (.., true) => Grab::S,
        _ => {
            if s.contains(px, py) {
                Grab::Move
            } else {
                Grab::New
            }
        }
    }
}

/// The rectangle between a fixed corner and a moving point. With an aspect
/// ratio the rectangle keeps it, as large as the image allows.
pub fn from_corner(anchor: (f64, f64), cur: (f64, f64), bounds: (f64, f64), aspect: Option<f64>) -> Sel {
    let (ax, ay) = (anchor.0.clamp(0.0, bounds.0), anchor.1.clamp(0.0, bounds.1));
    let (cx, cy) = (cur.0.clamp(0.0, bounds.0), cur.1.clamp(0.0, bounds.1));
    let sx = if cx >= ax { 1.0 } else { -1.0 };
    let sy = if cy >= ay { 1.0 } else { -1.0 };
    let (mut w, mut h) = ((cx - ax).abs(), (cy - ay).abs());
    if let Some(r) = aspect {
        // Room from the anchor in the drag direction.
        let max_w = if sx > 0.0 { bounds.0 - ax } else { ax };
        let max_h = if sy > 0.0 { bounds.1 - ay } else { ay };
        // Follow the larger movement.
        if w / r >= h {
            h = w / r;
        } else {
            w = h * r;
        }
        let k = (max_w / w.max(f64::EPSILON)).min(max_h / h.max(f64::EPSILON)).min(1.0);
        w *= k;
        h *= k;
    }
    Sel { x: if sx > 0.0 { ax } else { ax - w }, y: if sy > 0.0 { ay } else { ay - h }, w, h }
}

/// Move `start` by (dx, dy) and keep it inside the image.
pub fn moved(start: Sel, dx: f64, dy: f64, bounds: (f64, f64)) -> Sel {
    Sel {
        x: (start.x + dx).clamp(0.0, (bounds.0 - start.w).max(0.0)),
        y: (start.y + dy).clamp(0.0, (bounds.1 - start.h).max(0.0)),
        ..start
    }
}

/// Drag a handle of `start` by (dx, dy). The opposite side stays.
pub fn resized(start: Sel, grab: Grab, dx: f64, dy: f64, bounds: (f64, f64), aspect: Option<f64>) -> Sel {
    let (l, r, t, b) = (start.x, start.right(), start.y, start.bottom());
    match grab {
        Grab::NW => from_corner((r, b), (l + dx, t + dy), bounds, aspect),
        Grab::NE => from_corner((l, b), (r + dx, t + dy), bounds, aspect),
        Grab::SW => from_corner((r, t), (l + dx, b + dy), bounds, aspect),
        Grab::SE => from_corner((l, t), (r + dx, b + dy), bounds, aspect),
        Grab::E | Grab::W => {
            let (fixed, moving) = if grab == Grab::E { (l, r + dx) } else { (r, l + dx) };
            let (fixed, moving) = (fixed, moving.clamp(0.0, bounds.0));
            let w = (moving - fixed).abs().max(MIN_SIDE);
            let x = fixed.min(moving);
            match aspect {
                None => Sel { x, y: start.y, w, h: start.h },
                Some(ratio) => {
                    // Height follows the width around the old center line.
                    let cy = start.y + start.h / 2.0;
                    let max_h = 2.0 * cy.min(bounds.1 - cy);
                    let h = (w / ratio).min(max_h).max(MIN_SIDE);
                    let w = h * ratio;
                    let x = if grab == Grab::E { fixed } else { fixed - w };
                    Sel { x: x.clamp(0.0, (bounds.0 - w).max(0.0)), y: cy - h / 2.0, w, h }
                }
            }
        }
        Grab::N | Grab::S => {
            let (fixed, moving) = if grab == Grab::S { (t, b + dy) } else { (b, t + dy) };
            let moving = moving.clamp(0.0, bounds.1);
            let h = (moving - fixed).abs().max(MIN_SIDE);
            let y = fixed.min(moving);
            match aspect {
                None => Sel { x: start.x, y, w: start.w, h },
                Some(ratio) => {
                    let cx = start.x + start.w / 2.0;
                    let max_w = 2.0 * cx.min(bounds.0 - cx);
                    let w = (h * ratio).min(max_w).max(MIN_SIDE);
                    let h = w / ratio;
                    let y = if grab == Grab::S { fixed } else { fixed - h };
                    Sel { x: cx - w / 2.0, y: y.clamp(0.0, (bounds.1 - h).max(0.0)), w, h }
                }
            }
        }
        Grab::Move => moved(start, dx, dy, bounds),
        Grab::New => start,
    }
}

/// The largest centered selection with this ratio (when an aspect preset is picked).
pub fn fit_aspect(sel: Sel, ratio: f64, bounds: (f64, f64)) -> Sel {
    let (cx, cy) = (sel.x + sel.w / 2.0, sel.y + sel.h / 2.0);
    let (mut w, mut h) = (sel.w, sel.w / ratio);
    if h > sel.h {
        h = sel.h;
        w = h * ratio;
    }
    let max_w = 2.0 * cx.min(bounds.0 - cx);
    let max_h = 2.0 * cy.min(bounds.1 - cy);
    let k = (max_w / w.max(f64::EPSILON)).min(max_h / h.max(f64::EPSILON)).min(1.0);
    let (w, h) = (w * k, h * k);
    Sel { x: cx - w / 2.0, y: cy - h / 2.0, w, h }
}

/// Whole pixels for the job: `(x, y, w, h)` inside the image, at least 1 x 1.
pub fn to_pixels(sel: Sel, image: (u32, u32)) -> (u32, u32, u32, u32) {
    let (iw, ih) = (f64::from(image.0.max(1)), f64::from(image.1.max(1)));
    let x0 = sel.x.round().clamp(0.0, iw - 1.0);
    let y0 = sel.y.round().clamp(0.0, ih - 1.0);
    let x1 = sel.right().round().clamp(x0 + 1.0, iw);
    let y1 = sel.bottom().round().clamp(y0 + 1.0, ih);
    (x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: (f64, f64) = (1000.0, 600.0);
    fn sel(x: f64, y: f64, w: f64, h: f64) -> Sel {
        Sel { x, y, w, h }
    }
    fn close(a: Sel, b: Sel) {
        let d = |p: f64, q: f64| (p - q).abs() < 1e-6;
        assert!(d(a.x, b.x) && d(a.y, b.y) && d(a.w, b.w) && d(a.h, b.h), "{a:?} != {b:?}");
    }

    #[test]
    fn grabs() {
        let s = Some(sel(100.0, 100.0, 200.0, 100.0));
        assert_eq!(grab_at(None, 5.0, 5.0, 8.0), Grab::New);
        assert_eq!(grab_at(s, 200.0, 150.0, 8.0), Grab::Move);
        assert_eq!(grab_at(s, 102.0, 98.0, 8.0), Grab::NW);
        assert_eq!(grab_at(s, 301.0, 201.0, 8.0), Grab::SE);
        assert_eq!(grab_at(s, 300.0, 150.0, 8.0), Grab::E);
        assert_eq!(grab_at(s, 100.0, 150.0, 8.0), Grab::W);
        assert_eq!(grab_at(s, 200.0, 100.0, 8.0), Grab::N);
        assert_eq!(grab_at(s, 200.0, 201.0, 8.0), Grab::S);
        assert_eq!(grab_at(s, 500.0, 500.0, 8.0), Grab::New);
    }

    #[test]
    fn corner_drag_free_and_clamped() {
        close(from_corner((100.0, 100.0), (300.0, 250.0), B, None), sel(100.0, 100.0, 200.0, 150.0));
        // Dragging up and left of the anchor.
        close(from_corner((300.0, 250.0), (100.0, 100.0), B, None), sel(100.0, 100.0, 200.0, 150.0));
        // Outside the image: stops at the edge.
        close(from_corner((900.0, 500.0), (1200.0, 900.0), B, None), sel(900.0, 500.0, 100.0, 100.0));
        close(from_corner((100.0, 100.0), (-50.0, -50.0), B, None), sel(0.0, 0.0, 100.0, 100.0));
    }

    #[test]
    fn corner_drag_keeps_the_aspect() {
        let r = from_corner((100.0, 100.0), (500.0, 200.0), B, Some(1.0));
        assert!((r.w - r.h).abs() < 1e-6 && (r.w - 400.0).abs() < 1e-6, "{r:?}");
        let r = from_corner((100.0, 100.0), (200.0, 500.0), B, Some(16.0 / 9.0));
        assert!((r.w / r.h - 16.0 / 9.0).abs() < 1e-6, "{r:?}");
        // Near the bottom edge the rectangle shrinks but keeps the ratio and the corner.
        let r = from_corner((100.0, 500.0), (900.0, 590.0), B, Some(4.0 / 3.0));
        assert!((r.w / r.h - 4.0 / 3.0).abs() < 1e-6, "{r:?}");
        assert!(r.bottom() <= 600.0 + 1e-6 && r.x == 100.0 && r.y == 500.0, "{r:?}");
    }

    #[test]
    fn move_stays_inside() {
        let s = sel(100.0, 100.0, 200.0, 100.0);
        close(moved(s, 50.0, -20.0, B), sel(150.0, 80.0, 200.0, 100.0));
        close(moved(s, 5000.0, 5000.0, B), sel(800.0, 500.0, 200.0, 100.0));
        close(moved(s, -5000.0, -5000.0, B), sel(0.0, 0.0, 200.0, 100.0));
    }

    #[test]
    fn handles_keep_the_opposite_side() {
        let s = sel(100.0, 100.0, 200.0, 100.0);
        close(resized(s, Grab::E, 50.0, 0.0, B, None), sel(100.0, 100.0, 250.0, 100.0));
        close(resized(s, Grab::W, -50.0, 0.0, B, None), sel(50.0, 100.0, 250.0, 100.0));
        close(resized(s, Grab::S, 0.0, 30.0, B, None), sel(100.0, 100.0, 200.0, 130.0));
        close(resized(s, Grab::N, 0.0, -30.0, B, None), sel(100.0, 70.0, 200.0, 130.0));
        close(resized(s, Grab::SE, 10.0, 10.0, B, None), sel(100.0, 100.0, 210.0, 110.0));
        close(resized(s, Grab::NW, -10.0, -10.0, B, None), sel(90.0, 90.0, 210.0, 110.0));
        close(resized(s, Grab::Move, 10.0, 0.0, B, None), sel(110.0, 100.0, 200.0, 100.0));
        // Past the opposite side: the rectangle flips instead of going negative.
        let r = resized(s, Grab::E, -300.0, 0.0, B, None);
        assert!(r.w >= MIN_SIDE && r.x >= 0.0, "{r:?}");
    }

    #[test]
    fn edge_handles_keep_the_aspect() {
        let s = sel(100.0, 100.0, 200.0, 200.0);
        let r = resized(s, Grab::E, 100.0, 0.0, B, Some(1.0));
        assert!((r.w - 300.0).abs() < 1e-6 && (r.h - 300.0).abs() < 1e-6, "{r:?}");
        assert_eq!(r.x, 100.0);
        let r = resized(s, Grab::S, 0.0, 100.0, B, Some(1.0));
        assert!((r.w - 300.0).abs() < 1e-6 && (r.h - 300.0).abs() < 1e-6, "{r:?}");
        assert!(r.y == 100.0 && r.x >= 0.0 && r.right() <= 1000.0);
    }

    #[test]
    fn aspect_preset_fits_inside_the_selection() {
        let s = sel(100.0, 100.0, 400.0, 400.0);
        let r = fit_aspect(s, 16.0 / 9.0, B);
        assert!((r.w / r.h - 16.0 / 9.0).abs() < 1e-6 && r.w <= 400.0 + 1e-6, "{r:?}");
        assert!((r.x + r.w / 2.0 - 300.0).abs() < 1e-6 && (r.y + r.h / 2.0 - 300.0).abs() < 1e-6);
        let r = fit_aspect(sel(0.0, 0.0, 1000.0, 600.0), 1.0, B);
        assert!((r.w - 600.0).abs() < 1e-6 && (r.h - 600.0).abs() < 1e-6, "{r:?}");
    }

    #[test]
    fn pixels_are_whole_and_inside() {
        assert_eq!(to_pixels(sel(10.4, 20.6, 100.2, 50.4), (1000, 600)), (10, 21, 101, 50));
        assert_eq!(to_pixels(sel(990.0, 590.0, 50.0, 50.0), (1000, 600)), (990, 590, 10, 10));
        assert_eq!(to_pixels(sel(5.0, 5.0, 0.0, 0.0), (1000, 600)), (5, 5, 1, 1));
        assert_eq!(to_pixels(sel(-20.0, -20.0, 100.0, 100.0), (1000, 600)), (0, 0, 80, 80));
        assert_eq!(to_pixels(sel(999.9, 599.9, 5.0, 5.0), (1000, 600)), (999, 599, 1, 1));
    }

    #[test]
    fn aspect_presets() {
        assert_eq!(Aspect::ALL.map(Aspect::label), ["Free", "1:1", "4:3", "16:9"]);
        assert_eq!(Aspect::Free.ratio(), None);
        assert_eq!(Aspect::Square.ratio(), Some(1.0));
    }
}
