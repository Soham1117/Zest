//! Zest overlay: the radial wheel window (Windows only).
//!
//! Rendering: D3D11 device -> DXGI composition swap chain -> DirectComposition
//! -> Direct2D/DirectWrite. The window is created once and hidden between
//! uses. It is a topmost, non-activating tool window with an inflated
//! `SetWindowRgn` disc (padding so GDI does not clip D2D antialiasing).
//! Slices are rounded glass tiles with emboss rims; labels use Inter.
//!
//! Dependency direction: `zest-overlay -> zest-shell` (the overlay implements
//! `zest_shell::DropSink` for its window), `zest-overlay -> zest-core`.
//! `zest-shell` and `zest-input` know nothing about the overlay.
//!
//! # Threading
//!
//! Everything runs on the UI thread, which must be an STA (`OleInitialize`)
//! with a normal message loop, and the process must be per-monitor DPI aware.
//! [`Overlay`] is `!Send`. Callbacks run on the UI thread; they may call
//! `Overlay` methods. Do not block in them: `on_files_entered` and `on_pick`
//! run inside OLE drag calls of the drag source.
//!
//! # API
//!
//! ```no_run
//! use zest_overlay::{Overlay, Callbacks, Outcome};
//! let mut cb = Callbacks::default();
//! cb.on_files_entered = Box::new(|_paths| None); // app builds a WheelModel
//! cb.on_pick = Box::new(|pick| { /* queue job for pick.files, pick.item */ });
//! let overlay = Overlay::new(cb).unwrap();
//! // input layer events:
//! overlay.show_for_drag(100, 100);      // DragStarted: invisible window
//! overlay.on_drag_ended();              // DragEnded
//! // later, from job progress (0..=100):
//! overlay.set_progress(42.0);
//! overlay.complete(Outcome::Succeeded);
//! ```
//!
//! | Call | Mode |
//! |---|---|
//! | `show_for_drag(x, y)` | Drag mode: invisible until `DragEnter` brings local files and `on_files_entered` returns a model |
//! | `show_for_files(x, y, files, model, count)` | Click mode (hold gesture, chord on a selection): activated window, mouse click, arrows / 1-8 / Enter / Esc, closes on deactivate |
//! | `show_drop_zone(x, y)` | Click mode with no files: center "Drop files here", accepts a drag; stays open until Esc, a center click, a pick or 30 s |
//! | `set_model(model, count)` | Replace the model of the shown wheel |
//! | `on_drag_ended()` | Close rules: no files -> hide at once; files -> hide after 250 ms unless a drop arrives; a 10 s limit covers a missed button-up |
//! | `hide()` | Hide at once (no callback) |
//! | `set_progress(0..=100)`, `complete(outcome)`, `hand_off()` | Progress ring after a pick |
//!
//! After a pick (drop on a sector, click, Enter) the wheel becomes a thin
//! progress ring at the same place and `on_pick` is called. The ring hides on
//! its own 60 s after the last `set_progress`. `complete` shows a check mark
//! (or a cross) for about 600 ms, then fades (no fade if the system has
//! client-area animations off). `hand_off` hides the ring at once. A click on
//! the check mark calls `on_progress_clicked`.
//!
//! Drag-drop: the pointer often sits in the dead center. Dropping there
//! commits sector 0 (favorite / last target) when it is a direct item. Drop
//! outside cancels. Expanding groups ("Tools ›", …) open their sub-ring on
//! drop instead of inventing a pick. Click mode still treats a center click
//! as cancel.
//!
//! Sub-rings: hovering a group sector ("More", "Tools", "Actions", or a
//! category such as "Images") for 300 ms (immediately on a click or Enter)
//! opens an outer ring around that sector. Every group works the same way.
//! Hit-testing ([`geometry::hit_test`]) is pure and returns [`Hit`].
//!
//! Actions: picking an action (Copy path, Copy files, Copy to, Move to,
//! Terminal) closes the wheel at once, with no progress ring, because the app
//! does the work itself. Picking Zip shows the progress ring like a
//! conversion.
//!
//! Drop effect: COPY when a drop would pick or open a sub-ring; NEVER MOVE.

mod fonts;
pub mod geometry;
pub mod model;
mod overlay;
mod render;
pub mod theme;

use std::path::PathBuf;

use zest_core::format::Category;
use zest_core::graph::{WheelItem, WheelModel};

pub use geometry::{hit_test, Hit, Layout, SubRing};
pub use overlay::{set_reduce_animations, Overlay};

/// How a job ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Succeeded,
    Failed,
    Cancelled,
}

/// The user's choice.
#[derive(Clone, Debug, PartialEq)]
pub struct Pick {
    /// All items that came with the wheel (files and folders). The app filters
    /// them: by category for a category group, by what the operation accepts for
    /// conversions and tools. Actions use all of them.
    pub files: Vec<PathBuf>,
    /// Index into `WheelModel::Groups` when picked from a category group's
    /// sub-ring. `None` for "More", "Tools" and "Actions".
    pub group: Option<usize>,
    pub category: Option<Category>,
    /// The chosen conversion, tool or action (`item.op`).
    pub item: WheelItem,
}

/// `on_files_entered` callback type.
pub type FilesEnteredFn = Box<dyn FnMut(&[PathBuf]) -> Option<(WheelModel, usize)>>;

/// Callbacks, invoked on the UI thread.
pub struct Callbacks {
    /// Local files entered the wheel (drag mode or drop zone). Return the
    /// model and the file count to show, or `None` if not supported (the
    /// wheel then stays invisible).
    pub on_files_entered: FilesEnteredFn,
    /// A target was picked.
    pub on_pick: Box<dyn FnMut(Pick)>,
    /// The wheel closed without a pick (Esc, click in the middle, focus lost,
    /// drop outside, button released without a drop). Not called by `hide()`.
    pub on_cancel: Box<dyn FnMut()>,
    /// The check mark of the finished progress ring was clicked.
    pub on_progress_clicked: Box<dyn FnMut()>,
    /// Hover settled on a sector (debounced ~120 ms). `None` clears the preview.
    /// `token` must be echoed back via [`Overlay::set_hover_preview`].
    pub on_hover_preview: Box<dyn FnMut(Option<HoverPreviewReq>)>,
}

/// Request for a live hover preview chip.
#[derive(Clone, Debug)]
pub struct HoverPreviewReq {
    pub token: u64,
    pub files: Vec<PathBuf>,
    pub item: WheelItem,
}

impl Default for Callbacks {
    fn default() -> Self {
        Callbacks {
            on_files_entered: Box::new(|_| None),
            on_pick: Box::new(|_| {}),
            on_cancel: Box::new(|| {}),
            on_progress_clicked: Box::new(|| {}),
            on_hover_preview: Box::new(|_| {}),
        }
    }
}
