//! Pure wheel geometry: hit-testing, sub-ring layout, window sizing and
//! clamping. No Windows calls, fully unit-tested.
//!
//! Screen coordinates (y grows down). Sector 0 starts at the top and sectors
//! run clockwise. Angles are radians measured clockwise from "up".

use std::f32::consts::PI;

/// Inner dead zone radius (logical px).
pub const DEAD_LOGICAL: f32 = 32.0;
/// Default outer radius of the main ring (logical px). Configurable.
pub const DEFAULT_OUTER_LOGICAL: f32 = 140.0;
/// Radial thickness of the sub-ring (logical px).
pub const SUB_THICKNESS_LOGICAL: f32 = 60.0;
/// Minimum angle per item in a sub-ring (degrees). Wide sub-rings spread out.
pub const SUB_MIN_ITEM_DEG: f32 = 30.0;
/// Largest angle a sub-ring may span (radians).
const SUB_MAX_SPAN: f32 = 2.0 * PI - 30.0 * PI / 180.0;
/// Outer radius of the progress ring (logical px).
pub const PROGRESS_RADIUS_LOGICAL: f32 = 38.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// What is under a point.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    /// The dead zone in the middle (or the ring area when there are no sectors).
    Center,
    /// Sector `i` of the main ring.
    Sector(usize),
    /// Item `sub` of the open sub-ring around sector `group`.
    SubSector { group: usize, sub: usize },
    /// Outside the wheel.
    Outside,
}

/// The open sub-ring: around main sector `parent`, holding `count` items.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SubRing {
    pub parent: usize,
    pub count: usize,
}

/// Radii in physical pixels.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Layout {
    pub dead: f32,
    /// Outer radius of the main ring.
    pub r_in: f32,
    /// Outer radius of the sub-ring (also the radius of the window disc).
    pub r_sub: f32,
}

impl Layout {
    pub fn new(scale: f32, outer_logical: f32) -> Layout {
        Layout {
            dead: DEAD_LOGICAL * scale,
            r_in: inner_px(scale, outer_logical) as f32 / 2.0,
            r_sub: window_px(scale, outer_logical) as f32 / 2.0,
        }
    }
}

/// Which sector is under the point (dx, dy) relative to the wheel center.
/// Returns `None` inside the dead zone or outside `outer`.
pub fn sector_at(dx: f32, dy: f32, dead_zone: f32, outer: f32, count: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let r = (dx * dx + dy * dy).sqrt();
    if r < dead_zone || r > outer {
        return None;
    }
    let ang = angle_of(dx, dy);
    let step = 2.0 * PI / count as f32;
    let idx = (ang / step).floor() as usize;
    Some(idx.min(count - 1))
}

/// Angle clockwise from "up" in `[0, 2*PI)`.
pub fn angle_of(dx: f32, dy: f32) -> f32 {
    let mut ang = dx.atan2(-dy);
    if ang < 0.0 {
        ang += 2.0 * PI;
    }
    ang
}

/// Start angle and span (radians) of the sub-ring around `parent`. The ring is
/// centered on the parent sector. It is at least as wide as the parent and
/// widens so that every item gets [`SUB_MIN_ITEM_DEG`] degrees.
pub fn sub_span(sectors: usize, parent: usize, count: usize) -> (f32, f32) {
    let step = 2.0 * PI / sectors.max(1) as f32;
    let mid = (parent as f32 + 0.5) * step;
    let wanted = count as f32 * SUB_MIN_ITEM_DEG.to_radians();
    let span = step.max(wanted).min(SUB_MAX_SPAN);
    (mid - span / 2.0, span)
}

/// Inner fraction of the dead zone that stays Center; outside that, sector
/// hits pull inward (magnetic). Center drops still resolve to favorites.
const MAGNET_DEAD_FRAC: f32 = 0.45;

/// Sector index for angle, snapped to the nearest sector mid when the pointer
/// sits within half a step of a mid (always true for equal sectors — this is
/// the same partition as [`sector_at`], expressed via mid distance).
fn magnetic_main_sector(dx: f32, dy: f32, count: usize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let ang = angle_of(dx, dy);
    let step = 2.0 * PI / count as f32;
    let mut best = 0usize;
    let mut best_d = f32::MAX;
    for i in 0..count {
        let mid = (i as f32 + 0.5) * step;
        let mut d = (ang - mid).abs();
        if d > PI {
            d = 2.0 * PI - d;
        }
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    // Only accept when within half a sector of that mid (always, for equal wedges).
    if best_d <= step * 0.5 + 1e-4 {
        Some(best)
    } else {
        None
    }
}

/// Hit test for the point (dx, dy) relative to the wheel center.
/// `sectors` is the number of main sectors, `sub` the open sub-ring if any.
/// Main-ring hits magnetically prefer the nearest sector mid and extend
/// slightly into the dead zone (true center still wins closer in).
pub fn hit_test(dx: f32, dy: f32, l: &Layout, sectors: usize, sub: Option<SubRing>) -> Hit {
    let r = (dx * dx + dy * dy).sqrt();
    let magnet_inner = l.dead * MAGNET_DEAD_FRAC;
    if r < magnet_inner {
        return Hit::Center;
    }
    if r <= l.r_in {
        return match magnetic_main_sector(dx, dy, sectors) {
            Some(i) => Hit::Sector(i),
            None => Hit::Center,
        };
    }
    if let Some(s) = sub {
        if r <= l.r_sub && s.count > 0 && s.parent < sectors {
            let (start, span) = sub_span(sectors, s.parent, s.count);
            let rel = (angle_of(dx, dy) - start).rem_euclid(2.0 * PI);
            if rel <= span {
                let idx = (rel / (span / s.count as f32)).floor() as usize;
                return Hit::SubSector { group: s.parent, sub: idx.min(s.count - 1) };
            }
        }
    }
    Hit::Outside
}

/// Top-left of a `size` x `size` window centered on (cx, cy), shifted so that
/// it stays inside `work`. If the work area is smaller than the window, the
/// top-left corner of the work area wins.
pub fn clamp_origin(cx: i32, cy: i32, size: i32, work: Rect) -> (i32, i32) {
    let x = (cx - size / 2).min(work.right - size).max(work.left);
    let y = (cy - size / 2).min(work.bottom - size).max(work.top);
    (x, y)
}

/// Window origin for a window of `px` pixels that holds a wheel disc of
/// `inner` pixels in its middle. Only the inner disc is kept on screen: a
/// sub-ring near the edge may be clipped, but the wheel does not move away
/// from the pointer more than in the spike.
pub fn window_origin(cx: i32, cy: i32, px: i32, inner: i32, work: Rect) -> (i32, i32) {
    let (ix, iy) = clamp_origin(cx, cy, inner, work);
    (ix + inner / 2 - px / 2, iy + inner / 2 - px / 2)
}

/// Diameter of the main wheel disc in physical pixels (even).
pub fn inner_px(scale: f32, outer_logical: f32) -> i32 {
    2 * (outer_logical * scale).round() as i32
}

/// Window size in physical pixels: big enough for the main ring plus the
/// sub-ring (even).
pub fn window_px(scale: f32, outer_logical: f32) -> i32 {
    2 * ((outer_logical + SUB_THICKNESS_LOGICAL) * scale).round() as i32
}

/// Radius of the window region for the current state.
pub fn region_radius(l: &Layout, sub_open: bool) -> f32 {
    if sub_open {
        l.r_sub
    } else {
        l.r_in
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

    // ----- spike tests (kept) ------------------------------------------

    #[test]
    fn six_sectors_clockwise_from_top() {
        for (deg, want) in [(30.0, 0), (90.0, 1), (150.0, 2), (210.0, 3), (270.0, 4), (330.0, 5)] {
            let (dx, dy) = at(deg, 90.0);
            assert_eq!(sector_at(dx, dy, DEAD, OUTER, 6), Some(want), "deg={deg}");
        }
    }

    #[test]
    fn sector_zero_starts_at_top() {
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
    fn inner_size_scales_with_dpi() {
        // The spike's window size is now the size of the inner disc.
        assert_eq!(inner_px(1.0, 140.0), 280);
        assert_eq!(inner_px(1.5, 140.0), 420);
        assert_eq!(inner_px(2.0, 140.0), 560);
    }

    // ----- window sizing --------------------------------------------------

    #[test]
    fn window_holds_inner_ring_plus_sub_ring() {
        assert_eq!(window_px(1.0, 140.0), 400);
        assert_eq!(window_px(1.5, 140.0), 600);
        assert_eq!(window_px(2.0, 140.0), 800);
        for s in [1.0, 1.25, 1.5, 1.75, 2.0, 3.0] {
            let w = window_px(s, 140.0);
            let i = inner_px(s, 140.0);
            assert_eq!(w % 2, 0);
            assert_eq!(i % 2, 0);
            assert!(w > i + (SUB_THICKNESS_LOGICAL * s) as i32 - 2);
        }
        // configurable outer radius
        assert_eq!(window_px(1.0, 100.0), 320);
    }

    #[test]
    fn layout_radii() {
        let l = Layout::new(1.0, 140.0);
        assert_eq!(l.dead, 32.0);
        assert_eq!(l.r_in, 140.0);
        assert_eq!(l.r_sub, 200.0);
        assert_eq!(region_radius(&l, false), 140.0);
        assert_eq!(region_radius(&l, true), 200.0);
    }

    #[test]
    fn window_origin_keeps_wheel_centered_on_pointer() {
        // Not near an edge: center == pointer.
        let (x, y) = window_origin(960, 500, 400, 280, work());
        assert_eq!((x + 200, y + 200), (960, 500));
    }

    #[test]
    fn window_origin_clamps_the_inner_disc_only() {
        // Pointer at the left edge: the inner disc ends at x = 0, the window may stick out.
        let (x, y) = window_origin(5, 500, 400, 280, work());
        assert_eq!(x + 200 - 140, 0);
        assert_eq!(y + 200, 500);
        // Same result as the spike's clamp for the inner disc.
        let (ix, _) = clamp_origin(5, 500, 280, work());
        assert_eq!(ix, 0);
    }

    // ----- hit testing ----------------------------------------------------

    fn layout() -> Layout {
        Layout::new(1.0, 140.0)
    }

    #[test]
    fn hit_main_ring() {
        let l = layout();
        assert_eq!(hit_test(0.0, 0.0, &l, 6, None), Hit::Center);
        assert_eq!(hit_test(10.0, -10.0, &l, 6, None), Hit::Center);
        // Magnetic: outer dead zone still hits a sector (center stays inward).
        let (dx, dy) = at(30.0, l.dead * 0.7);
        assert_eq!(hit_test(dx, dy, &l, 6, None), Hit::Sector(0));
        let (dx, dy) = at(30.0, 90.0);
        assert_eq!(hit_test(dx, dy, &l, 6, None), Hit::Sector(0));
        let (dx, dy) = at(330.0, 90.0);
        assert_eq!(hit_test(dx, dy, &l, 6, None), Hit::Sector(5));
        // outside the main ring with no sub-ring
        let (dx, dy) = at(30.0, 170.0);
        assert_eq!(hit_test(dx, dy, &l, 6, None), Hit::Outside);
        // no sectors: ring area is "center"
        assert_eq!(hit_test(0.0, -90.0, &l, 0, None), Hit::Center);
    }

    #[test]
    fn ten_sub_ring_items_stay_legible() {
        // Images: Tools (Crop included) and Actions (Share included) have 10 items.
        // Each item gets 30 degrees; the arc at the middle of the ring must be wider
        // than a label (52 logical px), even at the smallest wheel radius.
        for outer in [110.0f32, 140.0, 200.0] {
            let l = Layout::new(1.0, outer);
            let (_, span) = sub_span(8, 3, 10);
            assert!((span - 300f32.to_radians()).abs() < 1e-4);
            let mid_r = (l.r_in + l.r_sub) / 2.0;
            let arc = mid_r * span / 10.0;
            assert!(arc > 60.0, "outer {outer}: arc {arc}");
        }
        // 11 items still fit in the 330 degrees.
        let (_, span) = sub_span(8, 3, 11);
        assert!((span - 330f32.to_radians()).abs() < 1e-4);
    }

    #[test]
    fn sub_span_is_centered_and_widens() {
        // 6 sectors (60 degrees), parent 0 -> centered on 30 degrees.
        let (start, span) = sub_span(6, 0, 1);
        assert!((span - 60f32.to_radians()).abs() < 1e-5);
        assert!((start - 0.0).abs() < 1e-5);
        // 3 items fit in 90 degrees (30 each): wider than the parent.
        let (start, span) = sub_span(6, 0, 3);
        assert!((span - 90f32.to_radians()).abs() < 1e-5);
        assert!((start - (-15f32).to_radians()).abs() < 1e-5);
        // never more than 330 degrees
        let (_, span) = sub_span(6, 0, 20);
        assert!(span <= 330f32.to_radians() + 1e-5);
    }

    #[test]
    fn hit_sub_ring_items() {
        let l = layout();
        let sub = Some(SubRing { parent: 0, count: 3 });
        // span -15..75 degrees, 30 each: items at 0, 30, 60 degrees (centers)
        for (deg, want) in [(0.0, 0), (30.0, 1), (60.0, 2)] {
            let (dx, dy) = at(deg, 170.0);
            assert_eq!(
                hit_test(dx, dy, &l, 6, sub),
                Hit::SubSector { group: 0, sub: want },
                "deg={deg}"
            );
        }
        // main ring stays reachable while the sub-ring is open
        let (dx, dy) = at(30.0, 100.0);
        assert_eq!(hit_test(dx, dy, &l, 6, sub), Hit::Sector(0));
    }

    #[test]
    fn hit_sub_ring_boundaries() {
        let l = layout();
        let sub = Some(SubRing { parent: 1, count: 2 });
        // parent 1 of 6: center 90 degrees, span 60 -> 60..120 degrees
        let (dx, dy) = at(75.0, 160.0);
        assert_eq!(hit_test(dx, dy, &l, 6, sub), Hit::SubSector { group: 1, sub: 0 });
        let (dx, dy) = at(105.0, 160.0);
        assert_eq!(hit_test(dx, dy, &l, 6, sub), Hit::SubSector { group: 1, sub: 1 });
        // just outside the angular span
        let (dx, dy) = at(125.0, 160.0);
        assert_eq!(hit_test(dx, dy, &l, 6, sub), Hit::Outside);
        let (dx, dy) = at(55.0, 160.0);
        assert_eq!(hit_test(dx, dy, &l, 6, sub), Hit::Outside);
        // radius edges: just beyond the main ring is the sub-ring, beyond r_sub is outside
        let (dx, dy) = at(90.0, 140.5);
        assert!(matches!(hit_test(dx, dy, &l, 6, sub), Hit::SubSector { .. }));
        let (dx, dy) = at(90.0, 200.5);
        assert_eq!(hit_test(dx, dy, &l, 6, sub), Hit::Outside);
        let (dx, dy) = at(90.0, 139.5);
        assert_eq!(hit_test(dx, dy, &l, 6, sub), Hit::Sector(1));
    }

    #[test]
    fn hit_sub_ring_wraps_around_the_top() {
        // Parent 0 with 4 items: span 120 degrees -> -30..90 (wraps through 360).
        let l = layout();
        let sub = Some(SubRing { parent: 0, count: 4 });
        let (dx, dy) = at(345.0, 170.0);
        assert_eq!(hit_test(dx, dy, &l, 6, sub), Hit::SubSector { group: 0, sub: 0 });
        let (dx, dy) = at(15.0, 170.0);
        assert_eq!(hit_test(dx, dy, &l, 6, sub), Hit::SubSector { group: 0, sub: 1 });
        let (dx, dy) = at(80.0, 170.0);
        assert_eq!(hit_test(dx, dy, &l, 6, sub), Hit::SubSector { group: 0, sub: 3 });
        let (dx, dy) = at(200.0, 170.0);
        assert_eq!(hit_test(dx, dy, &l, 6, sub), Hit::Outside);
    }

    #[test]
    fn hit_sub_ring_ignores_bad_state() {
        let l = layout();
        let (dx, dy) = at(30.0, 170.0);
        assert_eq!(hit_test(dx, dy, &l, 6, Some(SubRing { parent: 9, count: 3 })), Hit::Outside);
        assert_eq!(hit_test(dx, dy, &l, 6, Some(SubRing { parent: 0, count: 0 })), Hit::Outside);
    }

    #[test]
    fn hit_sub_ring_scales_with_dpi() {
        let l = Layout::new(1.5, 140.0);
        let sub = Some(SubRing { parent: 0, count: 3 });
        let (dx, dy) = at(30.0, 255.0); // 170 logical
        assert_eq!(hit_test(dx, dy, &l, 6, sub), Hit::SubSector { group: 0, sub: 1 });
        let (dx, dy) = at(30.0, 305.0); // beyond 200 logical = 300 px
        assert_eq!(hit_test(dx, dy, &l, 6, sub), Hit::Outside);
    }

    #[test]
    fn wide_tools_sub_ring_nine_items_on_eight_sector_ring() {
        // Tools group (9 image tools) as sector 6 of 8: the ring widens to 270 degrees,
        // centered on the sector, and every item can be hit.
        let l = layout();
        let (start, span) = sub_span(8, 6, 9);
        assert!((span - 270f32.to_radians()).abs() < 1e-4);
        let mid = 6.5 * (360.0 / 8.0);
        assert!(((start + span / 2.0).to_degrees() - mid).abs() < 1e-3);
        let sub = Some(SubRing { parent: 6, count: 9 });
        for j in 0..9 {
            let deg = (start.to_degrees() + (j as f32 + 0.5) * 30.0).rem_euclid(360.0);
            let (dx, dy) = at(deg, 170.0);
            assert_eq!(hit_test(dx, dy, &l, 8, sub), Hit::SubSector { group: 6, sub: j }, "item {j}");
        }
    }
}
