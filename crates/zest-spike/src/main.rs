//! Zest Phase 0 feasibility spike. No conversion code.

#[macro_use]
mod log;
mod drop_target;
mod explorer;
mod input;
mod tray;
mod wheel;

use anyhow::{Context, Result};
use windows::core::w;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, TRUE};
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, GetMessageW, TranslateMessage, MSG};

fn main() -> Result<()> {
    log::init();
    let result = run();
    if let Err(e) = &result {
        zlog!("fatal", "err={:#}", e);
    }
    log::shutdown();
    result
}

fn run() -> Result<()> {
    // SAFETY: process-wide setting, done before any window exists.
    if let Err(e) = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) } {
        zlog!("dpi_awareness_error", "err={e}");
    }

    // SAFETY: named mutex for single instance; the handle lives until exit.
    let _mutex = unsafe {
        let h = CreateMutexW(None, TRUE, w!("Local\\ZestSpike")).context("CreateMutexW")?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            zlog!("already_running");
            return Ok(());
        }
        h
    };

    // SAFETY: first COM call on this thread; makes it an STA.
    unsafe { OleInitialize(None) }.context("OleInitialize")?;
    zlog!("startup");

    let wheel_hwnd = wheel::init()?;
    drop_target::register(wheel_hwnd).context("RegisterDragDrop")?;
    let main_hwnd = input::create_main_window()?;
    tray::add(main_hwnd);
    zlog!("ready");

    let mut msg = MSG::default();
    // SAFETY: standard message loop on the UI thread.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    tray::remove(main_hwnd);
    drop_target::revoke(wheel_hwnd);
    wheel::shutdown(wheel_hwnd);
    // SAFETY: balances OleInitialize above.
    unsafe { OleUninitialize() };
    Ok(())
}
