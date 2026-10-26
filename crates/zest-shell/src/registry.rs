//! Per-user integration. HKCU only, no admin rights.
//!
//! - Explorer context menu verb ("Show more options" on Windows 11).
//! - Send To shortcut.
//! - Auto-start entry.

use std::path::Path;

use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, IPersistFile, CLSCTX_ALL};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegOpenKeyExW,
    RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows::Win32::UI::Shell::{
    FOLDERID_SendTo, IShellLinkW, SHGetKnownFolderPath, ShellLink, KF_FLAG_DEFAULT,
};

use crate::{wide_path, wide_z, ComScope, Result, ShellError};

const VERB_KEY: &str = r"Software\Classes\*\shell\Zest";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "Zest";
const SENDTO_NAME: &str = "Zest.lnk";

fn check(code: windows::Win32::Foundation::WIN32_ERROR, what: &str) -> Result<()> {
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(ShellError::Win32(windows::core::Error::from(code.to_hresult()))).map_err(|e| {
            tracing::warn!("{what}: {e}");
            e
        })
    }
}

/// Open or create `HKCU\<sub>` for writing.
fn create_key(sub: &str) -> Result<HKEY> {
    let wsub = wide_z(sub);
    let mut key = HKEY::default();
    // SAFETY: NUL-terminated key name, valid out pointer.
    let r = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(wsub.as_ptr()),
            0,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        )
    };
    check(r, "RegCreateKeyExW")?;
    Ok(key)
}

/// Set a REG_SZ value (`name` empty = the key's default value).
fn set_string(key: HKEY, name: &str, value: &str) -> Result<()> {
    let wname = wide_z(name);
    let wval = wide_z(value);
    // SAFETY: REG_SZ data must include the terminating NUL; bytes view of `wval`.
    let bytes =
        unsafe { std::slice::from_raw_parts(wval.as_ptr() as *const u8, wval.len() * 2) };
    // SAFETY: `key` is open for writing.
    let r = unsafe {
        RegSetValueExW(
            key,
            if name.is_empty() { PCWSTR::null() } else { PCWSTR(wname.as_ptr()) },
            0,
            REG_SZ,
            Some(bytes),
        )
    };
    check(r, "RegSetValueExW")
}

fn close(key: HKEY) {
    // SAFETY: key came from RegCreateKeyExW.
    unsafe {
        let _ = RegCloseKey(key);
    }
}

fn with_key(sub: &str, f: impl FnOnce(HKEY) -> Result<()>) -> Result<()> {
    let key = create_key(sub)?;
    let r = f(key);
    close(key);
    r
}

fn exe_str(exe: &Path) -> Result<String> {
    exe.to_str()
        .map(str::to_owned)
        .ok_or_else(|| ShellError::Invalid("exe path is not valid Unicode".into()))
}

/// Add `HKCU\Software\Classes\*\shell\Zest` ("Convert with Zest").
/// Runs `"<exe>" --convert "%1"`; `MultiSelectModel=Player` starts one
/// process per selected file (the running instance collects them).
pub fn install_context_menu(exe: &Path) -> Result<()> {
    let exe = exe_str(exe)?;
    with_key(VERB_KEY, |k| {
        set_string(k, "", "Convert with Zest")?;
        set_string(k, "Icon", &format!("\"{exe}\""))?;
        set_string(k, "MultiSelectModel", "Player")
    })?;
    with_key(&format!(r"{VERB_KEY}\command"), |k| {
        set_string(k, "", &format!("\"{exe}\" --convert \"%1\""))
    })?;
    tracing::info!("context menu verb installed");
    Ok(())
}

/// Remove the context menu verb. Not an error if it is absent.
pub fn uninstall_context_menu() -> Result<()> {
    let wsub = wide_z(VERB_KEY);
    // SAFETY: NUL-terminated key name.
    let r = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(wsub.as_ptr())) };
    if r == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    check(r, "RegDeleteTreeW")
}

fn sendto_path() -> Result<std::path::PathBuf> {
    // SAFETY: the returned buffer is freed with CoTaskMemFree.
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_SendTo, KF_FLAG_DEFAULT, None)?;
        let s = p.to_string();
        CoTaskMemFree(Some(p.0 as *const _));
        let dir = s.map_err(|e| ShellError::Invalid(format!("SendTo path: {e}")))?;
        Ok(std::path::PathBuf::from(dir).join(SENDTO_NAME))
    }
}

/// Create `Zest.lnk` in the SendTo folder. The shortcut starts
/// `"<exe>" --convert` and Windows appends all selected files, so the app
/// sees `--convert <file> <file> ...`.
pub fn install_send_to(exe: &Path) -> Result<()> {
    let _com = ComScope::new();
    let lnk = sendto_path()?;
    let wexe = wide_path(exe);
    let wargs = wide_z("--convert");
    let wdesc = wide_z("Convert with Zest");
    let wlnk = wide_path(&lnk);
    // SAFETY: COM objects are owned values; wide strings outlive the calls.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_ALL)?;
        link.SetPath(PCWSTR(wexe.as_ptr()))?;
        link.SetArguments(PCWSTR(wargs.as_ptr()))?;
        link.SetDescription(PCWSTR(wdesc.as_ptr()))?;
        link.SetIconLocation(PCWSTR(wexe.as_ptr()), 0)?;
        let file: IPersistFile = link.cast()?;
        file.Save(PCWSTR(wlnk.as_ptr()), true)?;
    }
    tracing::info!(path = %lnk.display(), "send-to shortcut installed");
    Ok(())
}

/// Delete `Zest.lnk` from the SendTo folder. Not an error if it is absent.
pub fn uninstall_send_to() -> Result<()> {
    let lnk = sendto_path()?;
    match std::fs::remove_file(&lnk) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(ShellError::Invalid(format!("remove {}: {e}", lnk.display()))),
    }
}

/// Add or remove the `HKCU\...\Run` entry that starts Zest at sign-in.
pub fn set_autostart(exe: &Path, enable: bool) -> Result<()> {
    if enable {
        let exe = exe_str(exe)?;
        return with_key(RUN_KEY, |k| set_string(k, RUN_VALUE, &format!("\"{exe}\"")));
    }
    let key = create_key(RUN_KEY)?;
    let wname = wide_z(RUN_VALUE);
    // SAFETY: `key` is open; NUL-terminated name.
    let r = unsafe { RegDeleteValueW(key, PCWSTR(wname.as_ptr())) };
    close(key);
    if r == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    check(r, "RegDeleteValueW")
}

/// Open `HKCU\<sub>` for reading. `None` if it does not exist.
fn open_read(sub: &str) -> Option<HKEY> {
    let wsub = wide_z(sub);
    let mut key = HKEY::default();
    // SAFETY: NUL-terminated key name, valid out pointer.
    let r = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(wsub.as_ptr()), 0, KEY_READ, &mut key) };
    (r == ERROR_SUCCESS).then_some(key)
}

/// Whether the `HKCU\...\Run` entry for Zest exists.
pub fn is_autostart_enabled() -> bool {
    let Some(key) = open_read(RUN_KEY) else { return false };
    let wname = wide_z(RUN_VALUE);
    // SAFETY: `key` is open; NUL-terminated name; no data is requested.
    let r = unsafe { RegQueryValueExW(key, PCWSTR(wname.as_ptr()), None, None, None, None) };
    close(key);
    r == ERROR_SUCCESS
}

/// Whether the right-click verb is installed.
pub fn is_context_menu_installed() -> bool {
    match open_read(VERB_KEY) {
        Some(key) => {
            close(key);
            true
        }
        None => false,
    }
}
