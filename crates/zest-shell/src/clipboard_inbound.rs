//! Read files / an image from the clipboard for "Convert clipboard".

use std::path::{Path, PathBuf};

use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::DataExchange::{GetClipboardData, IsClipboardFormatAvailable, RegisterClipboardFormatW};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::{CF_DIBV5, CF_HDROP};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

use crate::actions::{open_clipboard, Clip};
use crate::{wide_z, Result, ShellError};

/// What the clipboard can feed into the wheel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardSource {
    /// Existing file paths (`CF_HDROP`).
    Files(Vec<PathBuf>),
    /// A temporary image written from clipboard pixels.
    Image(PathBuf),
}

/// True when the clipboard has a file drop list or a PNG/DIB image.
pub fn clipboard_convertible(_owner: HWND) -> bool {
    // SAFETY: format probes only.
    unsafe {
        IsClipboardFormatAvailable(u32::from(CF_HDROP.0)).is_ok()
            || IsClipboardFormatAvailable(u32::from(CF_DIBV5.0)).is_ok()
            || png_format().is_some_and(|f| IsClipboardFormatAvailable(f).is_ok())
    }
}

fn png_format() -> Option<u32> {
    let name = wide_z("PNG");
    // SAFETY: NUL-terminated.
    let f = unsafe { RegisterClipboardFormatW(PCWSTR(name.as_ptr())) };
    (f != 0).then_some(f)
}

/// Read convertible clipboard content. Image pixels are written under `tmp_dir`.
pub fn read_clipboard_source(owner: HWND, tmp_dir: &Path) -> Result<ClipboardSource> {
    open_clipboard(owner)?;
    let _clip = Clip;
    // Prefer file lists.
    // SAFETY: clipboard open.
    if unsafe { IsClipboardFormatAvailable(u32::from(CF_HDROP.0)).is_ok() } {
        let paths = unsafe { read_hdrop() }?;
        if !paths.is_empty() {
            return Ok(ClipboardSource::Files(paths));
        }
    }
    if let Some(fmt) = png_format() {
        if unsafe { IsClipboardFormatAvailable(fmt).is_ok() } {
            let png = unsafe { read_global_bytes(fmt) }?;
            let path = write_temp(tmp_dir, "clipboard.png", &png)?;
            return Ok(ClipboardSource::Image(path));
        }
    }
    if unsafe { IsClipboardFormatAvailable(u32::from(CF_DIBV5.0)).is_ok() } {
        let dib = unsafe { read_global_bytes(u32::from(CF_DIBV5.0)) }?;
        let path = dib_to_bmp(tmp_dir, &dib)?;
        return Ok(ClipboardSource::Image(path));
    }
    Err(ShellError::Invalid("Clipboard has no files or image".into()))
}

/// SAFETY: clipboard open; `CF_HDROP` present.
unsafe fn read_hdrop() -> Result<Vec<PathBuf>> {
    let h = GetClipboardData(u32::from(CF_HDROP.0))?;
    let drop = HDROP(h.0);
    let n = DragQueryFileW(drop, 0xFFFF_FFFF, None);
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        let mut len = DragQueryFileW(drop, i, None);
        if len == 0 {
            continue;
        }
        len += 1;
        let mut buf = vec![0u16; len as usize];
        let written = DragQueryFileW(drop, i, Some(&mut buf));
        if written == 0 {
            continue;
        }
        buf.truncate(written as usize);
        out.push(PathBuf::from(String::from_utf16_lossy(&buf)));
    }
    Ok(out)
}

/// SAFETY: clipboard open; format available.
unsafe fn read_global_bytes(format: u32) -> Result<Vec<u8>> {
    let h = GetClipboardData(format)?;
    let hg = windows::Win32::Foundation::HGLOBAL(h.0);
    let size = GlobalSize(hg);
    if size == 0 {
        return Err(ShellError::Invalid("Empty clipboard data".into()));
    }
    let p = GlobalLock(hg);
    if p.is_null() {
        return Err(ShellError::Win32(windows::core::Error::from_win32()));
    }
    let slice = std::slice::from_raw_parts(p as *const u8, size);
    let v = slice.to_vec();
    let _ = GlobalUnlock(hg);
    Ok(v)
}

fn write_temp(dir: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf> {
    std::fs::create_dir_all(dir).map_err(|e| ShellError::Invalid(format!("temp dir: {e}")))?;
    let path = dir.join(format!("zest-{}-{}", std::process::id(), name));
    std::fs::write(&path, bytes).map_err(|e| ShellError::Invalid(format!("write temp: {e}")))?;
    Ok(path)
}

/// Wrap a DIB(V5) payload as a BMP file next to the source.
fn dib_to_bmp(dir: &Path, dib: &[u8]) -> Result<PathBuf> {
    if dib.len() < 40 {
        return Err(ShellError::Invalid("Clipboard DIB is too small".into()));
    }
    std::fs::create_dir_all(dir).map_err(|e| ShellError::Invalid(format!("temp dir: {e}")))?;
    let header_size = u32::from_le_bytes(dib[0..4].try_into().unwrap());
    let off = 14 + header_size;
    let file_size = (14 + dib.len()) as u32;
    let mut bmp = Vec::with_capacity(14 + dib.len());
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&file_size.to_le_bytes());
    bmp.extend_from_slice(&0u16.to_le_bytes());
    bmp.extend_from_slice(&0u16.to_le_bytes());
    bmp.extend_from_slice(&off.to_le_bytes());
    bmp.extend_from_slice(dib);
    let path = dir.join(format!("zest-{}-clipboard.bmp", std::process::id()));
    std::fs::write(&path, &bmp).map_err(|e| ShellError::Invalid(format!("write bmp: {e}")))?;
    Ok(path)
}
