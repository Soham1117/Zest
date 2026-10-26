//! Shell actions of the wheel: clipboard, folder picker, copy/move through
//! Explorer's own file operation, and "Terminal here".
//!
//! | Function | What |
//! |---|---|
//! | [`copy_paths_as_text`] | Clipboard text: one path per line (CRLF), quoted if it has spaces |
//! | [`copy_files_to_clipboard`] | `CF_HDROP` + `Preferred DropEffect` = COPY, so Paste in Explorer copies |
//! | [`pick_folder`] | Folder picker (`IFileOpenDialog`, `FOS_PICKFOLDERS`) |
//! | [`copy_items`], [`move_items`] | `IFileOperation` with `FOF_ALLOWUNDO`: Explorer's progress dialog and conflict UI; Ctrl+Z in Explorer undoes |
//! | [`open_terminal`] | Windows Terminal (`wt.exe -d <dir>`) or PowerShell, no `cmd.exe` |
//!
//! The clipboard functions need a window handle and run on the UI thread.
//! The folder picker and the file operations block until the user is done:
//! run them on their own thread. They start COM (STA) themselves.

use std::ffi::c_void;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT, DROPEFFECT_COPY};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VIRTUAL_KEY,
};
use windows::Win32::UI::Shell::{
    FileOpenDialog, FileOperation, IFileOpenDialog, IFileOperation, ILCreateFromPathW, ILFree,
    SHCreateItemFromParsingName, SHCreateShellItemArrayFromIDLists, FILEOPERATION_FLAGS,
    FOF_ALLOWUNDO, FOS_FORCEFILESYSTEM, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::Shell::{IShellItem, DROPFILES};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;

use crate::{wide_path, ComScope, Result, ShellError};

/// `ERROR_CANCELLED` as an HRESULT: the user closed the folder dialog.
const HRESULT_CANCELLED: i32 = 0x8007_04C7_u32 as i32;
/// `CREATE_NEW_CONSOLE`: a PowerShell window of its own.
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
/// F24 is not on any keyboard: harmless as a synthetic key.
const VK_F24: u16 = 0x87;

// ---------------------------------------------------------------------------
// Pure helpers (unit-tested)
// ---------------------------------------------------------------------------

/// Text for "Copy path": one path per line, lines joined with CRLF (no
/// trailing line break). A path with white space is put in double quotes.
pub fn clipboard_text_for_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| {
            let s = p.to_string_lossy();
            if s.chars().any(char::is_whitespace) {
                format!("\"{s}\"")
            } else {
                s.into_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\r\n")
}

/// The bytes of a `CF_HDROP` block: a `DROPFILES` header (20 bytes, wide
/// characters), the paths each ended by NUL, and a final NUL.
pub fn build_dropfiles(paths: &[PathBuf]) -> Vec<u8> {
    let mut wide: Vec<u16> = Vec::new();
    for p in paths {
        wide.extend(wide_path(p)); // includes the terminating NUL
    }
    wide.push(0);
    let header = std::mem::size_of::<DROPFILES>();
    let mut out = Vec::with_capacity(header + wide.len() * 2);
    out.extend_from_slice(&(header as u32).to_le_bytes()); // pFiles
    out.extend_from_slice(&[0u8; 8]); // pt (x, y)
    out.extend_from_slice(&0u32.to_le_bytes()); // fNC
    out.extend_from_slice(&1u32.to_le_bytes()); // fWide
    for w in wide {
        out.extend_from_slice(&w.to_le_bytes());
    }
    out
}

/// Folder for "Terminal here": the first item if it is a folder, else the folder of the first item.
pub fn terminal_dir(items: &[PathBuf], is_dir: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let first = items.first()?;
    if is_dir(first) {
        Some(first.clone())
    } else {
        first.parent().filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf)
    }
}

/// A PowerShell single-quoted string. Single quotes and the typographic quotes
/// that PowerShell treats as quotes are doubled.
pub fn powershell_literal(s: &str) -> String {
    let mut out = String::from("'");
    for c in s.chars() {
        out.push(c);
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}') {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// The `-Command` text that opens PowerShell in `dir`.
pub fn powershell_command(dir: &Path) -> String {
    format!("Set-Location -LiteralPath {}", powershell_literal(&dir.to_string_lossy()))
}

/// Folder path as Windows Terminal wants it for `-d`: no trailing backslash
/// (it would escape the closing quote), and `;` escaped (it separates wt commands).
pub fn wt_dir_arg(dir: &Path) -> String {
    let s = dir.to_string_lossy();
    let trimmed = if s.len() > 3 { s.trim_end_matches('\\') } else { &s };
    // `C:\` keeps its slash. It is written as `C:\.` so that quoting stays safe.
    let base = if trimmed.ends_with('\\') {
        format!("{trimmed}.")
    } else {
        trimmed.to_owned()
    };
    base.replace(';', "\\;")
}

// ---------------------------------------------------------------------------
// Clipboard
// ---------------------------------------------------------------------------

/// Open the clipboard, retrying a few times (another program may hold it).
pub(crate) fn open_clipboard(owner: HWND) -> Result<()> {
    let mut last = None;
    for _ in 0..10 {
        // SAFETY: plain Win32 call; the clipboard is closed by `Clip` on drop.
        match unsafe { OpenClipboard(owner) } {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(15));
            }
        }
    }
    Err(ShellError::Win32(last.expect("loop ran at least once")))
}

pub(crate) struct Clip;

impl Drop for Clip {
    fn drop(&mut self) {
        // SAFETY: the clipboard was opened by this thread.
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

/// Move `bytes` into a new global memory block.
///
/// SAFETY: the caller hands the block to the clipboard or frees it.
unsafe fn global_with(bytes: &[u8]) -> Result<HGLOBAL> {
    let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len())?;
    let p = GlobalLock(h);
    if p.is_null() {
        let _ = GlobalFree(h);
        return Err(ShellError::Win32(windows::core::Error::from_win32()));
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, bytes.len());
    let _ = GlobalUnlock(h);
    Ok(h)
}

/// SAFETY: the clipboard must be open and owned by `owner`.
pub(crate) unsafe fn set_clipboard(format: u32, bytes: &[u8]) -> Result<()> {
    let h = global_with(bytes)?;
    if let Err(e) = SetClipboardData(format, HANDLE(h.0)) {
        let _ = GlobalFree(h);
        return Err(ShellError::Win32(e));
    }
    Ok(()) // the system owns the memory now
}

/// Put the paths on the clipboard as text. See [`clipboard_text_for_paths`].
/// UI thread; `owner` is any window of this process.
pub fn copy_paths_as_text(owner: HWND, paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() {
        return Err(ShellError::Invalid("nothing to copy".into()));
    }
    let text = clipboard_text_for_paths(paths);
    let mut bytes: Vec<u8> = Vec::with_capacity(text.len() * 2 + 2);
    for w in text.encode_utf16().chain(std::iter::once(0)) {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    open_clipboard(owner)?;
    let _clip = Clip;
    // SAFETY: the clipboard is open.
    unsafe {
        EmptyClipboard()?;
        set_clipboard(u32::from(CF_UNICODETEXT.0), &bytes)
    }
}

/// Put the files on the clipboard as a file list (`CF_HDROP`) and mark the
/// drop effect as COPY, so that Paste in Explorer copies them.
/// UI thread; `owner` is any window of this process.
pub fn copy_files_to_clipboard(owner: HWND, paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() {
        return Err(ShellError::Invalid("nothing to copy".into()));
    }
    let drop = build_dropfiles(paths);
    let effect = DROPEFFECT_COPY.0.to_le_bytes();
    let name = crate::wide_z("Preferred DropEffect");
    // SAFETY: NUL-terminated name.
    let effect_format = unsafe { RegisterClipboardFormatW(PCWSTR(name.as_ptr())) };
    if effect_format == 0 {
        return Err(ShellError::Win32(windows::core::Error::from_win32()));
    }
    open_clipboard(owner)?;
    let _clip = Clip;
    // SAFETY: the clipboard is open.
    unsafe {
        EmptyClipboard()?;
        set_clipboard(u32::from(CF_HDROP.0), &drop)?;
        set_clipboard(effect_format, &effect)
    }
}

// ---------------------------------------------------------------------------
// Folder picker
// ---------------------------------------------------------------------------

/// A synthetic F24 key press. It makes this process the one that got the last
/// input, so that the dialog is allowed to come to the front even when the
/// drop happened in another program.
fn nudge_foreground() {
    let key = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(VK_F24), wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    };
    let inputs = [key(Default::default()), key(KEYEVENTF_KEYUP)];
    // SAFETY: valid INPUT array and size.
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

/// Show the folder picker. `initial` is the folder it starts in (ignored if it
/// does not exist). Returns `None` if the user cancels. Blocks; run it on its own thread.
pub fn pick_folder(title: &str, initial: Option<&Path>) -> Result<Option<PathBuf>> {
    let _com = ComScope::new();
    nudge_foreground();
    // SAFETY: COM objects are owned values; strings outlive the calls.
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_ALL)?;
        let opts = dialog.GetOptions()?;
        dialog.SetOptions(opts | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST)?;
        let t = crate::wide_z(title);
        dialog.SetTitle(PCWSTR(t.as_ptr()))?;
        if let Some(dir) = initial.filter(|d| d.is_dir()) {
            let w = wide_path(dir);
            if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(w.as_ptr()), None) {
                let _ = dialog.SetFolder(&item);
            }
        }
        match dialog.Show(HWND::default()) {
            Ok(()) => {}
            Err(e) if e.code().0 == HRESULT_CANCELLED => return Ok(None),
            Err(e) => return Err(e.into()),
        }
        let item = dialog.GetResult()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = PathBuf::from(name.to_string().unwrap_or_default());
        CoTaskMemFree(Some(name.0 as *const c_void));
        if path.as_os_str().is_empty() {
            return Err(ShellError::Invalid("the chosen folder has no file system path".into()));
        }
        Ok(Some(path))
    }
}

// ---------------------------------------------------------------------------
// Copy / move through IFileOperation
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Transfer {
    Copy,
    Move,
}

fn transfer(paths: &[PathBuf], dest: &Path, kind: Transfer) -> Result<()> {
    if paths.is_empty() {
        return Err(ShellError::Invalid("nothing to transfer".into()));
    }
    if !dest.is_dir() {
        return Err(ShellError::Invalid(format!("{} is not a folder", dest.display())));
    }
    let _com = ComScope::new();
    // PIDLs of the sources. Freed below on every path.
    let mut pidls: Vec<*mut ITEMIDLIST> = Vec::with_capacity(paths.len());
    let mut missing: Option<&Path> = None;
    for p in paths {
        let w = wide_path(p);
        // SAFETY: NUL-terminated wide path.
        let pidl = unsafe { ILCreateFromPathW(PCWSTR(w.as_ptr())) };
        if pidl.is_null() {
            missing.get_or_insert(p);
        } else {
            pidls.push(pidl);
        }
    }
    let result = (|| -> Result<()> {
        if let Some(p) = missing {
            return Err(ShellError::Invalid(format!("cannot find {}", p.display())));
        }
        let wd = wide_path(dest);
        // SAFETY: COM objects are owned values; `pidls` and `wd` outlive the calls.
        unsafe {
            let const_pidls: Vec<*const ITEMIDLIST> = pidls.iter().map(|p| *p as *const ITEMIDLIST).collect();
            let array = SHCreateShellItemArrayFromIDLists(&const_pidls)?;
            let dest_item: IShellItem =
                SHCreateItemFromParsingName(PCWSTR(wd.as_ptr()), None::<&windows::Win32::System::Com::IBindCtx>)?;
            let op: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
            op.SetOperationFlags(FILEOPERATION_FLAGS(FOF_ALLOWUNDO.0))?;
            match kind {
                Transfer::Copy => op.CopyItems(&array, &dest_item)?,
                Transfer::Move => op.MoveItems(&array, &dest_item)?,
            }
            // Explorer's own progress dialog and conflict questions appear here.
            op.PerformOperations()?;
            // The user may have cancelled in that dialog: not an error.
            let _ = op.GetAnyOperationsAborted();
        }
        Ok(())
    })();
    for p in pidls {
        // SAFETY: created by ILCreateFromPathW above.
        unsafe { ILFree(Some(p)) };
    }
    result
}

/// Copy `paths` into the folder `dest` with Explorer's progress and conflict
/// dialogs (`FOF_ALLOWUNDO`). Blocks until done; run it on its own thread.
pub fn copy_items(paths: &[PathBuf], dest: &Path) -> Result<()> {
    transfer(paths, dest, Transfer::Copy)
}

/// Move `paths` into the folder `dest`. See [`copy_items`].
pub fn move_items(paths: &[PathBuf], dest: &Path) -> Result<()> {
    transfer(paths, dest, Transfer::Move)
}

// ---------------------------------------------------------------------------
// Terminal
// ---------------------------------------------------------------------------

/// `wt.exe` on the PATH, if Windows Terminal is installed.
pub fn find_windows_terminal() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join("wt.exe"))
        .find(|p| p.exists())
}

/// Open a terminal in `dir`: Windows Terminal if it is installed, else
/// PowerShell in a new console window. Program and arguments are passed as an
/// argument list; there is no `cmd.exe` and no shell command string.
pub fn open_terminal(dir: &Path) -> Result<()> {
    if !dir.is_dir() {
        return Err(ShellError::Invalid(format!("{} is not a folder", dir.display())));
    }
    let mut cmd = match find_windows_terminal() {
        Some(wt) => {
            let mut c = Command::new(wt);
            c.arg("-d").arg(wt_dir_arg(dir));
            c
        }
        None => {
            let mut c = Command::new("powershell.exe");
            c.args(["-NoExit", "-Command"]).arg(powershell_command(dir));
            c.creation_flags(CREATE_NEW_CONSOLE);
            c
        }
    };
    cmd.current_dir(dir);
    cmd.spawn()
        .map(drop)
        .map_err(|e| ShellError::Invalid(format!("cannot start the terminal: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn clipboard_text_is_crlf_and_quotes_spaces() {
        assert_eq!(clipboard_text_for_paths(&[p(r"C:\a\b.txt")]), r"C:\a\b.txt");
        assert_eq!(
            clipboard_text_for_paths(&[p(r"C:\a\b.txt"), p(r"C:\My Files\c d.png"), p(r"D:\x")]),
            "C:\\a\\b.txt\r\n\"C:\\My Files\\c d.png\"\r\nD:\\x"
        );
        assert_eq!(clipboard_text_for_paths(&[]), "");
        // No trailing line break; Unicode is kept.
        assert_eq!(clipboard_text_for_paths(&[p("C:\\caf\u{e9}\\\u{65e5}.txt")]), "C:\\caf\u{e9}\\\u{65e5}.txt");
    }

    #[test]
    fn dropfiles_layout() {
        let b = build_dropfiles(&[p(r"C:\a.txt"), p(r"C:\b c.txt")]);
        assert_eq!(u32::from_le_bytes(b[0..4].try_into().unwrap()), 20); // pFiles
        assert_eq!(u32::from_le_bytes(b[16..20].try_into().unwrap()), 1); // fWide
        let wide: Vec<u16> = b[20..].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let text = String::from_utf16(&wide).unwrap();
        assert_eq!(text, "C:\\a.txt\0C:\\b c.txt\0\0");
        assert_eq!(b.len(), 20 + wide.len() * 2);
        // Even a lone path ends with a double NUL.
        let one = build_dropfiles(&[p(r"C:\a")]);
        assert_eq!(&one[one.len() - 4..], [0, 0, 0, 0]);
    }

    #[test]
    fn terminal_folder_rules() {
        let dirs = [r"C:\Work\Proj"];
        let is_dir = |q: &Path| dirs.iter().any(|d| Path::new(d) == q);
        assert_eq!(terminal_dir(&[p(r"C:\Work\Proj")], is_dir), Some(p(r"C:\Work\Proj")));
        assert_eq!(terminal_dir(&[p(r"C:\Work\a.txt"), p(r"C:\Work\Proj")], is_dir), Some(p(r"C:\Work")));
        assert_eq!(terminal_dir(&[], is_dir), None);
        assert_eq!(terminal_dir(&[p("a.txt")], is_dir), None);
    }

    #[test]
    fn powershell_quoting_and_command() {
        assert_eq!(powershell_literal(r"C:\a b"), r"'C:\a b'");
        assert_eq!(powershell_literal("it's"), "'it''s'");
        assert_eq!(powershell_literal("\u{2019}x\u{2018}"), "'\u{2019}\u{2019}x\u{2018}\u{2018}'");
        assert_eq!(
            powershell_command(Path::new(r"C:\Bob's $dir (1)")),
            r"Set-Location -LiteralPath 'C:\Bob''s $dir (1)'"
        );
        // `$`, backtick and `;` are inert inside a single-quoted literal.
        assert_eq!(powershell_literal("a;b`c$d"), "'a;b`c$d'");
    }

    #[test]
    fn windows_terminal_dir_argument() {
        assert_eq!(wt_dir_arg(Path::new(r"C:\Work\Proj")), r"C:\Work\Proj");
        assert_eq!(wt_dir_arg(Path::new(r"C:\Work\Proj\")), r"C:\Work\Proj");
        assert_eq!(wt_dir_arg(Path::new(r"C:\")), r"C:\.");
        assert_eq!(wt_dir_arg(Path::new(r"C:\a;b")), r"C:\a\;b");
    }

    #[test]
    fn empty_input_is_an_error() {
        assert!(copy_items(&[], Path::new(r"C:\")).is_err());
        assert!(move_items(&[p(r"C:\a.txt")], Path::new(r"C:\zest-no-such-folder-4711")).is_err());
        assert!(open_terminal(Path::new(r"C:\zest-no-such-folder-4711")).is_err());
        assert!(copy_files_to_clipboard(HWND::default(), &[]).is_err());
        assert!(copy_paths_as_text(HWND::default(), &[]).is_err());
    }
}
