//! Console output for the CLI when the exe uses the Windows subsystem
//! (release build): attach to the parent console and, when the standard
//! handles are not set (not redirected), point them at the console.

use windows::core::w;
use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Console::{
    AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE,
    STD_HANDLE, STD_OUTPUT_HANDLE,
};

fn missing(which: STD_HANDLE) -> bool {
    // SAFETY: plain query.
    match unsafe { GetStdHandle(which) } {
        Ok(h) => h.is_invalid() || h == INVALID_HANDLE_VALUE,
        Err(_) => true,
    }
}

pub fn attach_parent_console() {
    // SAFETY: console API calls with valid arguments. Fails harmlessly when
    // there is no parent console (Explorer launch) or one is already attached.
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            return;
        }
        for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            if missing(which) {
                let h: Result<HANDLE, _> = CreateFileW(
                    w!("CONOUT$"),
                    GENERIC_READ.0 | GENERIC_WRITE.0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    None,
                );
                if let Ok(h) = h {
                    let _ = SetStdHandle(which, h);
                }
            }
        }
    }
}
