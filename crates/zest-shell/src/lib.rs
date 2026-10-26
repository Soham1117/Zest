//! Zest shell integration (Windows only).
//!
//! No dependency on the other Zest UI crates. `zest-overlay` depends on this
//! crate and implements [`DropSink`] for its wheel window.
//!
//! | Module | What |
//! |---|---|
//! | [`drop_target`] | `IDropTarget` (COPY or NONE, never MOVE; existing files and folders, virtual items ignored) |
//! | [`selection`] | Selection of the foreground Explorer window or the Desktop |
//! | [`fileops`] | `notify_created`, `recycle` (Recycle Bin only), `open_folder_select` |
//! | [`registry`] | HKCU verbs: context menu, Send To shortcut, autostart |
//! | [`actions`] | Wheel actions: clipboard (text, files), folder picker, copy/move with Explorer's dialogs, terminal |
//! | [`share`] | The Windows share sheet for files (`DataTransferManager`) |
//! | [`clipboard_image`] | Copy as PNG/JPG: files plus `CF_DIBV5` and `PNG` pixels on the clipboard |
//!
//! Threading: everything that touches windows or `IDropTarget` must run on
//! the UI thread, which must be an STA (`OleInitialize`). The functions in
//! [`fileops`] and [`registry`] initialize COM themselves if the calling
//! thread has not done so, so they can be used from worker threads too.

pub mod actions;
pub mod clipboard_image;
pub mod clipboard_inbound;
pub mod drop_target;
pub mod fileops;
pub mod registry;
pub mod selection;
pub mod share;

pub use actions::{
    clipboard_text_for_paths, copy_files_to_clipboard, copy_items, copy_paths_as_text, move_items,
    open_terminal, pick_folder, terminal_dir,
};
pub use clipboard_image::{build_dibv5, copy_files_and_image, load_bgra};
pub use clipboard_inbound::{clipboard_convertible, read_clipboard_source, ClipboardSource};
pub use drop_target::{register_drop_target, revoke_drop_target, DropSink};
pub use fileops::{notify_created, open_folder_select, recycle};
pub use registry::{
    install_context_menu, install_send_to, is_autostart_enabled, is_context_menu_installed,
    set_autostart, uninstall_context_menu, uninstall_send_to,
};
pub use selection::{foreground_selection, Selection};
pub use share::share_files;

/// Error type for shell operations.
#[derive(Debug, thiserror::Error)]
pub enum ShellError {
    #[error("Win32 error: {0}")]
    Win32(#[from] windows::core::Error),
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, ShellError>;

/// Encode a string as a NUL-terminated UTF-16 vector.
pub(crate) fn wide_z(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub(crate) fn wide_path(p: &std::path::Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

/// RAII COM initialization for functions that may run on any thread.
/// If the thread is already initialized (any apartment), this does nothing.
pub(crate) struct ComScope(bool);

impl ComScope {
    pub(crate) fn new() -> ComScope {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        // SAFETY: balanced by CoUninitialize in Drop only when S_OK/S_FALSE returned.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        ComScope(hr.is_ok())
    }
}

impl Drop for ComScope {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: matches the successful CoInitializeEx in `new`.
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
    }
}
