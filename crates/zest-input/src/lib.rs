//! Zest input: global drag gesture, hold gesture and key chord (Windows only).
//!
//! The design is ported from Filee (MIT) through the Phase 0 spike. Zest never
//! swallows or injects input. Raw Input with `RIDEV_INPUTSINK` only observes
//! the mouse; modifiers come from `GetAsyncKeyState`; positions from
//! `GetCursorPos` (physical pixels, so the process must be per-monitor DPI
//! aware).
//!
//! # Use
//!
//! ```no_run
//! use zest_input::{InputHost, InputEvent};
//! let cfg = zest_core::Config::default();
//! let host = InputHost::new(&cfg, Box::new(|ev| match ev {
//!     InputEvent::DragStarted { x, y, late } => { /* show invisible wheel */ }
//!     InputEvent::DragEnded { x, y } => {}
//!     InputEvent::HoldTriggered { x, y } => {}
//!     InputEvent::KeyChord { x, y } => {}
//!     InputEvent::Undo => {}
//! })).unwrap();
//! // run the normal message loop on this thread ...
//! host.pause(true);                // tray "Pause"
//! host.reconfigure(&cfg);          // config reload
//! ```
//!
//! `InputHost::new` must run on the UI thread. It creates a hidden window
//! there, so the callback runs on that thread, from the message loop. Do not
//! block in the callback.
//!
//! # Config mapping
//!
//! - `kind = "drag"`: left button (or the configured button) with the exact
//!   modifier set, then movement past the threshold (`drag_threshold`, 0 or
//!   none = `SM_CXDRAG`/`SM_CYDRAG`). A modifier pressed after the drag has
//!   started also counts (late modifier).
//! - `kind = "hold"`: same press, no movement, `hold_ms` (default 450).
//! - `kind = "key_chord"`: `RegisterHotKey`. If the primary chord is taken,
//!   `fallback_key_chord` is tried.
//! - `scope`: `file_manager` = the window under the pointer belongs to a
//!   known file manager (`explorer.exe`, `FPilot.exe` / `FilePilot.exe`, plus
//!   `[gestures] file_managers`); `anywhere` = any process. Zest's own process
//!   and `gestures.excluded_processes` never arm. For key chords, the
//!   foreground window is checked.
//! - `gestures.paused` is applied by `new` and `reconfigure`.

mod gesture;
mod host;
mod plan;

pub use gesture::{GestureDetector, Mods, Out, Pt};
pub use host::{InputEvent, InputHost};
pub use plan::{build_plan, parse_key, scope_allows, Plan};
