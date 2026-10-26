//! Watch the config folder, debounce, reload.
//!
//! One thread blocks in `ReadDirectoryChangesW` and signals a channel. A
//! second thread waits until the channel is quiet for 300 ms, loads the file
//! and sends the result to the UI thread. Nothing here runs on the UI thread.

use crate::app::{UiEvent, UiSender};
use crossbeam_channel::{unbounded, Receiver};
use std::path::{Path, PathBuf};
use std::time::Duration;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY,
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
    FILE_NOTIFY_INFORMATION, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use zest_core::{Config, ConfigError};

pub const DEBOUNCE: Duration = Duration::from_millis(300);

/// Wait for a signal, then until `quiet` passes with no further signal.
/// Calls `fire` once per burst. Returns when the channel closes.
pub fn debounce(rx: &Receiver<()>, quiet: Duration, mut fire: impl FnMut()) {
    while rx.recv().is_ok() {
        while rx.recv_timeout(quiet).is_ok() {}
        fire();
    }
}

/// Text for the balloon when the config cannot be used.
pub fn error_text(e: &ConfigError) -> String {
    match e {
        ConfigError::Parse { line, .. } => format!("Config error on line {line}"),
        ConfigError::Io { .. } => "Config file cannot be read".to_owned(),
        ConfigError::Serialize(_) => "Config error".to_owned(),
    }
}

fn load(path: &Path) -> Result<Config, String> {
    let mut last = None;
    for _ in 0..3 {
        match Config::load(path) {
            Ok(c) => return Ok(c),
            // A file that is still being written can be locked for a moment.
            Err(e @ ConfigError::Io { .. }) => {
                last = Some(e);
                std::thread::sleep(Duration::from_millis(150));
            }
            Err(e) => {
                tracing::warn!("config: {e}");
                return Err(error_text(&e));
            }
        }
    }
    let e = last.expect("loop ran");
    tracing::warn!("config: {e}");
    Err(error_text(&e))
}

/// Does a notification buffer name the config file?
fn mentions(buf: &[u8], len: usize, file_name: &std::ffi::OsStr) -> bool {
    if len == 0 {
        return true; // buffer overflow: assume it did
    }
    let mut off = 0usize;
    loop {
        if off + std::mem::size_of::<FILE_NOTIFY_INFORMATION>() > len.min(buf.len()) {
            return false;
        }
        // SAFETY: the kernel filled the buffer with consecutive records; the
        // offsets are checked against `len`.
        let (next, name_len, name_ptr) = unsafe {
            let rec = buf.as_ptr().add(off) as *const FILE_NOTIFY_INFORMATION;
            (
                std::ptr::read_unaligned(std::ptr::addr_of!((*rec).NextEntryOffset)) as usize,
                std::ptr::read_unaligned(std::ptr::addr_of!((*rec).FileNameLength)) as usize,
                std::ptr::addr_of!((*rec).FileName) as *const u8,
            )
        };
        if off + 12 + name_len <= buf.len() {
            let units: Vec<u16> = (0..name_len / 2)
                // SAFETY: bounds checked just above.
                .map(|i| unsafe { std::ptr::read_unaligned(name_ptr.add(i * 2) as *const u16) })
                .collect();
            let name = String::from_utf16_lossy(&units);
            if name.eq_ignore_ascii_case(&file_name.to_string_lossy()) {
                return true;
            }
        }
        if next == 0 {
            return false;
        }
        off += next;
    }
}

fn watch_loop(dir: &Path, file_name: &std::ffi::OsStr, signal: &crossbeam_channel::Sender<()>) {
    let wdir: Vec<u16> = {
        use std::os::windows::ffi::OsStrExt;
        dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
    };
    // SAFETY: NUL-terminated path; the handle is closed below.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wdir.as_ptr()),
            GENERIC_READ.0 | FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    };
    let handle: HANDLE = match handle {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("config watcher: cannot open folder: {e}");
            std::thread::sleep(Duration::from_secs(5));
            return;
        }
    };
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let mut bytes = 0u32;
        // SAFETY: valid handle and buffer; synchronous call.
        let r = unsafe {
            ReadDirectoryChangesW(
                handle,
                buf.as_mut_ptr() as *mut _,
                buf.len() as u32,
                false,
                FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_SIZE,
                Some(&mut bytes),
                None,
                None,
            )
        };
        if let Err(e) = r {
            tracing::warn!("config watcher: {e}");
            break;
        }
        if mentions(&buf, bytes as usize, file_name) && signal.send(()).is_err() {
            break;
        }
    }
    // SAFETY: handle from CreateFileW.
    unsafe {
        let _ = CloseHandle(handle);
    }
}

/// Start watching `config_path`. Results go to the UI thread as `UiEvent::Config`.
pub fn start(config_path: PathBuf, ui: UiSender) {
    let (tx, rx) = unbounded::<()>();
    let path = config_path.clone();
    let _ = std::thread::Builder::new().name("zest-config-debounce".into()).spawn(move || {
        debounce(&rx, DEBOUNCE, || {
            tracing::info!("config changed, reloading");
            ui.send(UiEvent::Config(load(&path)));
        });
    });
    let _ = std::thread::Builder::new().name("zest-config-watch".into()).spawn(move || {
        let (Some(dir), Some(name)) = (config_path.parent(), config_path.file_name()) else {
            return;
        };
        // If the watch breaks (folder removed), try again every few seconds.
        loop {
            watch_loop(dir, name, &tx);
            std::thread::sleep(Duration::from_secs(5));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn burst_fires_once_after_quiet() {
        let (tx, rx) = unbounded::<()>();
        let count = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&count);
        let t = std::thread::spawn(move || debounce(&rx, Duration::from_millis(60), || {
            c.fetch_add(1, Ordering::SeqCst);
        }));
        for _ in 0..5 {
            tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(15));
        }
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(count.load(Ordering::SeqCst), 2);
        drop(tx);
        t.join().unwrap();
    }

    #[test]
    fn error_texts() {
        let e = ConfigError::Parse { line: 12, column: 3, message: "x".into() };
        assert_eq!(error_text(&e), "Config error on line 12");
    }

    #[test]
    fn config_error_line_from_bad_toml() {
        let err = Config::from_toml_str("[gestures]\npaused = maybe\n").unwrap_err();
        assert_eq!(error_text(&err), "Config error on line 2");
    }
}
