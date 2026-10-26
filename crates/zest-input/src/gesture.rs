//! Pure drag/hold gesture detector. No Windows calls.
//!
//! Ported from Filee (MIT) `GestureDetector.cs` through the Phase 0 spike.
//! Behavior is unchanged from the spike; do not "improve" it without the
//! owner re-testing the gesture.

/// Screen point in physical pixels.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

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
