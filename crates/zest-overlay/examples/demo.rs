//! Hand-run demo of the wheel overlay.
//!
//! `cargo run -p zest-overlay --example demo` cycles every 8 s through: a flat
//! wheel (with a "More" sub-ring), a groups wheel (with sub-rings), and a fake
//! progress ring. Click mode: use the mouse, arrows, 1-8, Enter, Esc.
//! Picking a target starts a fake progress ring.
//!
//! `--auto-exit` runs the same steps quickly and quits after about 4 seconds.
//! The drop zone step accepts files dragged from Explorer (flat model).

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetCursorPos, GetMessageW, KillTimer, PostQuitMessage, SetTimer,
    TranslateMessage, MSG,
};
use zest_core::format::FormatId;
use zest_core::graph::{FormatGraph, InputKind, WheelModel, WheelPrefs};
use zest_overlay::{Callbacks, Outcome, Overlay};

fn model(kinds: &[InputKind]) -> WheelModel {
    FormatGraph::new().wheel_model(kinds, &WheelPrefs::default())
}

/// One JPEG: conversions, "Tools" and "Actions" groups.
fn flat() -> WheelModel {
    model(&[InputKind::File(Some(FormatId::Jpeg))])
}

/// Images, a video and a song: category groups and "Actions".
fn groups() -> WheelModel {
    use FormatId::*;
    model(&[
        InputKind::File(Some(Png)),
        InputKind::File(Some(Jpeg)),
        InputKind::File(Some(Mp4)),
        InputKind::File(Some(Mp3)),
    ])
}

struct Demo {
    overlay: Overlay,
    step: Cell<u32>,
    auto: bool,
    started: Instant,
    progress: Cell<f32>,
}

thread_local! {
    static DEMO: RefCell<Option<Demo>> = const { RefCell::new(None) };
}

fn center() -> (i32, i32) {
    let mut p = POINT::default();
    // SAFETY: out-pointer to a local.
    let _ = unsafe { GetCursorPos(&mut p) };
    (p.x, p.y)
}

unsafe extern "system" fn tick(_: HWND, _: u32, _: usize, _: u32) {
    DEMO.with(|d| {
        let d = d.borrow();
        let Some(d) = d.as_ref() else { return };
        if d.auto && d.started.elapsed() > Duration::from_secs(4) {
            PostQuitMessage(0);
            return;
        }
        let n = d.step.get();
        d.step.set(n + 1);
        let (x, y) = center();
        match n % 4 {
            0 => d.overlay.show_for_files(x, y, vec!["demo.png".into()], flat(), 1),
            1 => d.overlay.show_for_files(x, y, vec!["a.png".into(), "b.mp4".into()], groups(), 6),
            2 => d.overlay.show_drop_zone(x, y),
            _ => {
                d.overlay.hide();
            }
        }
    });
}

/// Fake progress: advances every 50 ms after a pick.
unsafe extern "system" fn progress_tick(_: HWND, _: u32, id: usize, _: u32) {
    DEMO.with(|d| {
        let d = d.borrow();
        let Some(d) = d.as_ref() else { return };
        let p = d.progress.get() + 4.0;
        d.progress.set(p);
        if p >= 100.0 {
            let _ = KillTimer(None, id);
            d.overlay.set_progress(100.0);
            d.overlay.complete(Outcome::Succeeded);
        } else {
            d.overlay.set_progress(p);
        }
    });
}

fn main() -> anyhow::Result<()> {
    let auto = std::env::args().any(|a| a == "--auto-exit");
    // SAFETY: process-wide setting before any window exists.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        OleInitialize(None)?;
    }
    let cb = Callbacks {
        on_files_entered: Box::new(|paths| Some((flat(), paths.len()))),
        on_pick: Box::new(|pick| {
            println!("picked {} (files: {}, group: {:?})", pick.item.label, pick.files.len(), pick.group);
            DEMO.with(|d| {
                if let Some(d) = d.borrow().as_ref() {
                    d.progress.set(0.0);
                }
            });
            // SAFETY: thread timer with a plain callback.
            unsafe { SetTimer(None, 0, 50, Some(progress_tick)) };
        }),
        on_cancel: Box::new(|| println!("cancelled")),
        on_progress_clicked: Box::new(|| println!("progress check clicked")),
        on_hover_preview: Box::new(|_| {}),
    };
    let overlay = Overlay::new(cb)?;
    DEMO.with(|d| {
        *d.borrow_mut() = Some(Demo {
            overlay: overlay.clone(),
            step: Cell::new(0),
            auto,
            started: Instant::now(),
            progress: Cell::new(0.0),
        });
    });
    let period = if auto { 700 } else { 8000 };
    // SAFETY: thread timer with a plain callback; the message loop runs below.
    unsafe {
        SetTimer(None, 0, period, Some(tick));
        tick(HWND::default(), 0, 0, 0);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    DEMO.with(|d| *d.borrow_mut() = None);
    drop(overlay);
    // SAFETY: balances OleInitialize.
    unsafe { OleUninitialize() };
    Ok(())
}
