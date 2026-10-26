//! Private Inter font for the wheel. Bundled variable font is registered once
//! via `AddFontResourceExW(FR_PRIVATE)` so DirectWrite can resolve "Inter".

use std::fs;
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::OnceLock;

use windows::core::PCWSTR;
use windows::Win32::Graphics::Gdi::{AddFontResourceExW, FR_PRIVATE};

static INTER_READY: OnceLock<bool> = OnceLock::new();

const INTER_BYTES: &[u8] = include_bytes!("../assets/InterVariable.ttf");

/// Ensure Inter is available to DirectWrite for this process. Safe to call often.
pub fn ensure_inter() -> bool {
    *INTER_READY.get_or_init(register_inter)
}

fn register_inter() -> bool {
    let path = match materialize_font() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("inter font write failed: {e}");
            return false;
        }
    };
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: path is a null-terminated absolute path we just wrote; FR_PRIVATE
    // keeps the font process-local.
    let n = unsafe { AddFontResourceExW(PCWSTR(wide.as_ptr()), FR_PRIVATE, None) };
    if n == 0 {
        tracing::warn!("AddFontResourceExW(Inter) failed");
        false
    } else {
        tracing::debug!(?path, "inter font registered");
        true
    }
}

fn materialize_font() -> std::io::Result<PathBuf> {
    let dir = std::env::temp_dir().join("zest-fonts");
    fs::create_dir_all(&dir)?;
    let path = dir.join("InterVariable.ttf");
    if path.is_file() && fs::metadata(&path)?.len() == INTER_BYTES.len() as u64 {
        return Ok(path);
    }
    let tmp = dir.join("InterVariable.ttf.part");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(INTER_BYTES)?;
        f.sync_all()?;
    }
    let _ = fs::remove_file(&path);
    fs::rename(&tmp, &path)?;
    Ok(path)
}
