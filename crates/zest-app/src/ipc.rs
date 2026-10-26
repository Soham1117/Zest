//! Single instance: named mutex + named pipe, both per user (SID).
//!
//! A second launch with `--convert` sends its paths over the pipe and exits.
//! The pipe accepts the current user only (protected DACL).

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, LocalFree, ERROR_ALREADY_EXISTS, ERROR_BROKEN_PIPE,
    ERROR_FILE_NOT_FOUND, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_WRITE, HANDLE, HLOCAL,
    INVALID_HANDLE_VALUE,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
    TOKEN_USER,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAGS_AND_ATTRIBUTES, FILE_FLAG_FIRST_PIPE_INSTANCE,
    FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_INBOUND,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, WaitNamedPipeW, NAMED_PIPE_MODE,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES,
    PIPE_WAIT,
};
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;

/// Largest message the server accepts.
pub const MAX_MESSAGE: usize = 8 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Message encoding (pure)
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct Message {
    v: u32,
    paths: Vec<String>,
}

/// Bytes sent over the pipe: one JSON object.
pub fn encode_message(paths: &[PathBuf]) -> Vec<u8> {
    let msg = Message {
        v: 1,
        paths: paths.iter().map(|p| p.to_string_lossy().into_owned()).collect(),
    };
    serde_json::to_vec(&msg).unwrap_or_default()
}

/// Parse a pipe message. Empty path strings are dropped.
pub fn decode_message(bytes: &[u8]) -> Result<Vec<PathBuf>> {
    if bytes.len() > MAX_MESSAGE {
        bail!("message too large");
    }
    let msg: Message = serde_json::from_slice(bytes).context("bad message")?;
    if msg.v != 1 {
        bail!("unknown message version {}", msg.v);
    }
    Ok(msg.paths.into_iter().filter(|p| !p.is_empty()).map(PathBuf::from).collect())
}

// ---------------------------------------------------------------------------
// Names
// ---------------------------------------------------------------------------

pub fn mutex_name(sid: &str) -> String {
    format!("Local\\Zest-{sid}")
}

pub fn pipe_name(sid: &str) -> String {
    format!("\\\\.\\pipe\\zest-{sid}")
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// SID of the current user as a string (`S-1-5-21-...`).
pub fn user_sid() -> Result<String> {
    // SAFETY: standard token query; all buffers are local and sized by the API.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).context("OpenProcessToken")?;
        let result = (|| {
            let mut len = 0u32;
            let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
            if len == 0 {
                bail!("GetTokenInformation: no size");
            }
            // u64 buffer keeps the alignment TOKEN_USER needs.
            let mut buf = vec![0u64; (len as usize).div_ceil(8)];
            GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr() as *mut c_void), len, &mut len)
                .context("GetTokenInformation")?;
            let tu = &*(buf.as_ptr() as *const TOKEN_USER);
            let mut s = PWSTR::null();
            ConvertSidToStringSidW(tu.User.Sid, &mut s).context("ConvertSidToStringSidW")?;
            let out = s.to_string().context("SID string");
            let _ = LocalFree(HLOCAL(s.0 as *mut c_void));
            out
        })();
        let _ = CloseHandle(token);
        result
    }
}

// ---------------------------------------------------------------------------
// Mutex
// ---------------------------------------------------------------------------

/// Holds the named mutex until dropped.
pub struct InstanceGuard(HANDLE);

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        // SAFETY: handle from CreateMutexW.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// `Ok(Some(guard))` = we are the first instance. `Ok(None)` = one is running.
pub fn acquire_instance(sid: &str) -> Result<Option<InstanceGuard>> {
    let name = wide(&mutex_name(sid));
    // SAFETY: NUL-terminated name; last error read right after the call.
    unsafe {
        let h = CreateMutexW(None, false, PCWSTR(name.as_ptr())).context("CreateMutexW")?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(h);
            return Ok(None);
        }
        Ok(Some(InstanceGuard(h)))
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Send paths to the running instance. Retries for up to `timeout` (the
/// instance may still be creating its pipe).
pub fn forward(sid: &str, paths: &[PathBuf], timeout: Duration) -> Result<()> {
    let name = wide(&pipe_name(sid));
    let data = encode_message(paths);
    if data.len() > MAX_MESSAGE {
        bail!("too many files");
    }
    let start = Instant::now();
    // SAFETY: the instance was started by Explorer or by us; lets the running
    // instance take the foreground when it opens the wheel.
    unsafe {
        let _ = AllowSetForegroundWindow(u32::MAX);
    }
    let handle = loop {
        // SAFETY: NUL-terminated pipe name.
        let r = unsafe {
            CreateFileW(
                PCWSTR(name.as_ptr()),
                GENERIC_WRITE.0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
        };
        match r {
            Ok(h) => break h,
            Err(e) => {
                let code = (e.code().0 as u32) & 0xFFFF;
                if start.elapsed() >= timeout {
                    return Err(e).context("cannot reach the running instance");
                }
                if code == ERROR_PIPE_BUSY.0 {
                    // SAFETY: NUL-terminated pipe name.
                    let _ = unsafe { WaitNamedPipeW(PCWSTR(name.as_ptr()), 200) };
                } else if code == ERROR_FILE_NOT_FOUND.0 {
                    std::thread::sleep(Duration::from_millis(50));
                } else {
                    return Err(e).context("cannot open the pipe");
                }
            }
        }
    };
    let mut written_total = 0usize;
    let mut result = Ok(());
    while written_total < data.len() {
        let mut n = 0u32;
        // SAFETY: valid handle and buffer.
        let r = unsafe { WriteFile(handle, Some(&data[written_total..]), Some(&mut n), None) };
        if let Err(e) = r {
            result = Err(e).context("WriteFile");
            break;
        }
        written_total += n as usize;
    }
    // SAFETY: handle from CreateFileW.
    unsafe {
        let _ = CloseHandle(handle);
    }
    result
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

struct PipeSecurity {
    sd: PSECURITY_DESCRIPTOR,
}

// SAFETY: the descriptor is immutable after creation and only read.
unsafe impl Send for PipeSecurity {}

impl PipeSecurity {
    /// Protected DACL: full access for the current user only.
    fn new(sid: &str) -> Result<PipeSecurity> {
        let sddl = wide(&format!("D:P(A;;GA;;;{sid})"));
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: NUL-terminated SDDL; the result is freed in Drop.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )
            .context("ConvertStringSecurityDescriptorToSecurityDescriptorW")?;
        }
        Ok(PipeSecurity { sd })
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.sd.0,
            bInheritHandle: false.into(),
        }
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        // SAFETY: allocated by the convert function with LocalAlloc.
        unsafe {
            let _ = LocalFree(HLOCAL(self.sd.0));
        }
    }
}

fn create_instance(name: &[u16], sec: &PipeSecurity, first: bool) -> Result<HANDLE> {
    let sa = sec.attributes();
    let mut open = PIPE_ACCESS_INBOUND;
    if first {
        open |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    // SAFETY: NUL-terminated name; `sa` outlives the call.
    let h = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            open,
            NAMED_PIPE_MODE(
                PIPE_TYPE_BYTE.0 | PIPE_READMODE_BYTE.0 | PIPE_WAIT.0 | PIPE_REJECT_REMOTE_CLIENTS.0,
            ),
            PIPE_UNLIMITED_INSTANCES,
            0,
            64 * 1024,
            0,
            Some(&sa),
        )
    };
    if h == INVALID_HANDLE_VALUE || h.is_invalid() {
        bail!("CreateNamedPipeW: {}", windows::core::Error::from_win32());
    }
    Ok(h)
}

fn read_all(h: HANDLE) -> Vec<u8> {
    let mut data = Vec::new();
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let mut n = 0u32;
        // SAFETY: valid handle and buffer.
        match unsafe { ReadFile(h, Some(&mut buf), Some(&mut n), None) } {
            Ok(()) if n > 0 => {
                data.extend_from_slice(&buf[..n as usize]);
                if data.len() > MAX_MESSAGE {
                    break;
                }
            }
            Ok(()) => break,
            Err(e) => {
                if (e.code().0 as u32) & 0xFFFF != ERROR_BROKEN_PIPE.0 {
                    tracing::warn!("pipe read: {e}");
                }
                break;
            }
        }
    }
    data
}

/// Start the pipe server on a background thread. `on_paths` runs on that
/// thread for each received message. The pipe is created before this returns.
pub fn start_server(sid: &str, on_paths: Arc<dyn Fn(Vec<PathBuf>) + Send + Sync>) -> Result<()> {
    let name = wide(&pipe_name(sid));
    let sec = PipeSecurity::new(sid)?;
    let first = create_instance(&name, &sec, true)?;
    let first = first.0 as isize;
    std::thread::Builder::new()
        .name("zest-pipe".into())
        .spawn(move || {
            let mut next = Some(HANDLE(first as *mut c_void));
            loop {
                let h = match next.take() {
                    Some(h) => h,
                    None => match create_instance(&name, &sec, false) {
                        Ok(h) => h,
                        Err(e) => {
                            tracing::error!("pipe server stopped: {e}");
                            return;
                        }
                    },
                };
                // SAFETY: valid pipe handle; blocks until a client connects.
                let connected = match unsafe { ConnectNamedPipe(h, None) } {
                    Ok(()) => true,
                    Err(e) => (e.code().0 as u32) & 0xFFFF == ERROR_PIPE_CONNECTED.0,
                };
                if !connected {
                    // SAFETY: our handle.
                    unsafe {
                        let _ = CloseHandle(h);
                    }
                    continue;
                }
                let hv = h.0 as isize;
                let cb = Arc::clone(&on_paths);
                let spawned = std::thread::Builder::new().name("zest-pipe-client".into()).spawn(move || {
                    let h = HANDLE(hv as *mut c_void);
                    let data = read_all(h);
                    // SAFETY: our handle, used only on this thread.
                    unsafe {
                        let _ = DisconnectNamedPipe(h);
                        let _ = CloseHandle(h);
                    }
                    match decode_message(&data) {
                        Ok(paths) if !paths.is_empty() => cb(paths),
                        Ok(_) => {}
                        Err(e) => tracing::warn!("pipe message ignored: {e}"),
                    }
                });
                if spawned.is_err() {
                    tracing::error!("cannot start pipe client thread");
                }
            }
        })
        .context("pipe thread")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_round_trip() {
        let paths = vec![
            PathBuf::from(r"C:\Users\me\My Photos\a b.png"),
            PathBuf::from(r"D:\Ünï\日本語.heic"),
            PathBuf::from("-rf.jpg"),
        ];
        let bytes = encode_message(&paths);
        assert_eq!(decode_message(&bytes).unwrap(), paths);
    }

    #[test]
    fn message_is_json_with_version() {
        let bytes = encode_message(&[PathBuf::from("a")]);
        assert_eq!(String::from_utf8(bytes).unwrap(), r#"{"v":1,"paths":["a"]}"#);
    }

    #[test]
    fn bad_messages_are_errors() {
        assert!(decode_message(b"").is_err());
        assert!(decode_message(b"not json").is_err());
        assert!(decode_message(br#"{"v":2,"paths":[]}"#).is_err());
        assert!(decode_message(br#"{"paths":["a"]}"#).is_err());
    }

    #[test]
    fn empty_strings_dropped() {
        assert_eq!(decode_message(br#"{"v":1,"paths":["","a"]}"#).unwrap(), vec![PathBuf::from("a")]);
    }

    #[test]
    fn names_contain_sid() {
        assert_eq!(mutex_name("S-1-5-21-1"), "Local\\Zest-S-1-5-21-1");
        assert_eq!(pipe_name("S-1-5-21-1"), "\\\\.\\pipe\\zest-S-1-5-21-1");
    }

    #[test]
    fn sid_looks_like_a_sid() {
        let s = user_sid().unwrap();
        assert!(s.starts_with("S-1-"), "{s}");
    }
}
