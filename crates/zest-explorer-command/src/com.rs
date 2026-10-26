use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use windows::core::{implement, IUnknown, Interface, GUID, HRESULT, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, BOOL, CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_FAIL, E_NOINTERFACE,
    E_NOTIMPL, E_POINTER, HMODULE, S_FALSE, S_OK,
};
use windows::Win32::System::Com::{CoTaskMemFree, IBindCtx, IClassFactory, IClassFactory_Impl};
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
    GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};
use windows::Win32::System::Threading::{
    CreateProcessW, GetCurrentProcessId, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT,
    PROCESS_INFORMATION, STARTUPINFOW,
};
use windows::Win32::UI::Shell::{
    IEnumExplorerCommand, IExplorerCommand, IExplorerCommand_Impl, IShellItemArray, SHStrDupW,
    SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{AllowSetForegroundWindow, ASFW_ANY};

use crate::args;

/// Fixed CLSID. Must match `tools/sparse/AppxManifest.xml`.
const CLSID_ZEST_COMMAND: GUID = GUID::from_u128(0x7c3e9b52_4d1a_4f6b_9a2e_5b8d0c71e4a3);

const ECS_ENABLED: u32 = 0;
const ECS_HIDDEN: u32 = 2;

static LIVE_OBJECTS: AtomicU32 = AtomicU32::new(0);
static LIST_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Folder that holds this DLL (and zest.exe).
fn module_dir() -> Option<PathBuf> {
    let mut module = HMODULE::default();
    // SAFETY: the address belongs to this DLL; the refcount is not changed.
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(module_dir as *const () as *const u16),
            &mut module,
        )
        .ok()?;
    }
    let mut buf = vec![0u16; 1024];
    // SAFETY: valid buffer slice.
    let n = unsafe { GetModuleFileNameW(module, &mut buf) } as usize;
    if n == 0 || n >= buf.len() {
        return None;
    }
    let dll = PathBuf::from(String::from_utf16(&buf[..n]).ok()?);
    dll.parent().map(PathBuf::from)
}

fn exe_path() -> Option<String> {
    Some(module_dir()?.join("zest.exe").to_str()?.to_owned())
}

fn dup(s: &str) -> windows::core::Result<PWSTR> {
    let w: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: NUL-terminated; the shell frees the result with CoTaskMemFree.
    unsafe { SHStrDupW(PCWSTR(w.as_ptr())) }
}

fn count_items(items: Option<&IShellItemArray>) -> u32 {
    // SAFETY: plain COM call on a live interface.
    items.and_then(|a| unsafe { a.GetCount().ok() }).unwrap_or(0)
}

fn collect_paths(items: &IShellItemArray) -> Vec<String> {
    let mut out = Vec::new();
    // SAFETY: COM calls on live interfaces; the name buffer is freed once.
    unsafe {
        let n = items.GetCount().unwrap_or(0);
        for i in 0..n {
            let Ok(item) = items.GetItemAt(i) else { continue };
            let Ok(name) = item.GetDisplayName(SIGDN_FILESYSPATH) else { continue };
            if let Ok(s) = name.to_string() {
                out.push(s);
            }
            CoTaskMemFree(Some(name.0 as *const c_void));
        }
    }
    out
}

fn write_list(paths: &[String]) -> Option<String> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    // SAFETY: no arguments.
    let pid = unsafe { GetCurrentProcessId() };
    let seq = LIST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = format!("zest-list-{nanos:x}{pid:x}{seq:x}.txt");
    let path = std::env::temp_dir().join(name);
    std::fs::write(&path, args::list_file_text(paths)).ok()?;
    path.to_str().map(str::to_owned)
}

fn launch(items: &IShellItemArray) -> Option<()> {
    let exe = exe_path()?;
    let paths = collect_paths(items);
    if paths.is_empty() {
        return None;
    }
    let cmd = if args::needs_list_file(paths.len()) {
        args::convert_list_command_line(&exe, &write_list(&paths)?)
    } else {
        args::convert_command_line(&exe, &paths)
    };
    let mut wcmd: Vec<u16> = cmd.encode_utf16().chain(std::iter::once(0)).collect();
    let wexe: Vec<u16> = exe.encode_utf16().chain(std::iter::once(0)).collect();
    let si = STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOW>() as u32, ..Default::default() };
    let mut pi = PROCESS_INFORMATION::default();
    // SAFETY: wide buffers are NUL-terminated and outlive the calls; the
    // command line buffer is mutable as CreateProcessW requires.
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
        CreateProcessW(
            PCWSTR(wexe.as_ptr()),
            PWSTR(wcmd.as_mut_ptr()),
            None,
            None,
            false,
            CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
            None,
            PCWSTR::null(),
            &si,
            &mut pi,
        )
        .ok()?;
        let _ = CloseHandle(pi.hProcess);
        let _ = CloseHandle(pi.hThread);
    }
    Some(())
}

#[implement(IExplorerCommand)]
struct ZestCommand;

impl ZestCommand {
    fn new() -> Self {
        LIVE_OBJECTS.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Drop for ZestCommand {
    fn drop(&mut self) {
        LIVE_OBJECTS.fetch_sub(1, Ordering::SeqCst);
    }
}

#[allow(non_snake_case)]
impl IExplorerCommand_Impl for ZestCommand_Impl {
    fn GetTitle(&self, _items: Option<&IShellItemArray>) -> windows::core::Result<PWSTR> {
        dup("Convert with Zest")
    }

    fn GetIcon(&self, _items: Option<&IShellItemArray>) -> windows::core::Result<PWSTR> {
        match exe_path() {
            Some(p) => dup(&format!("{p},0")),
            None => Err(E_FAIL.into()),
        }
    }

    fn GetToolTip(&self, _items: Option<&IShellItemArray>) -> windows::core::Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }

    fn GetCanonicalName(&self) -> windows::core::Result<GUID> {
        Ok(CLSID_ZEST_COMMAND)
    }

    fn GetState(&self, items: Option<&IShellItemArray>, _slow: BOOL) -> windows::core::Result<u32> {
        let n = catch_unwind(AssertUnwindSafe(|| count_items(items))).unwrap_or(0);
        Ok(if n == 0 { ECS_HIDDEN } else { ECS_ENABLED })
    }

    fn Invoke(&self, items: Option<&IShellItemArray>, _ctx: Option<&IBindCtx>) -> windows::core::Result<()> {
        let Some(items) = items else { return Ok(()) };
        match catch_unwind(AssertUnwindSafe(|| launch(items))) {
            Ok(Some(())) => Ok(()),
            _ => Err(E_FAIL.into()),
        }
    }

    fn GetFlags(&self) -> windows::core::Result<u32> {
        Ok(0)
    }

    fn EnumSubCommands(&self) -> windows::core::Result<IEnumExplorerCommand> {
        Err(E_NOTIMPL.into())
    }
}

#[implement(IClassFactory)]
struct Factory;

impl Factory {
    fn new() -> Self {
        LIVE_OBJECTS.fetch_add(1, Ordering::SeqCst);
        Self
    }
}

impl Drop for Factory {
    fn drop(&mut self) {
        LIVE_OBJECTS.fetch_sub(1, Ordering::SeqCst);
    }
}

#[allow(non_snake_case)]
impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Option<&IUnknown>,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> windows::core::Result<()> {
        if ppv.is_null() || riid.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: checked non-null.
        unsafe { *ppv = std::ptr::null_mut() };
        if outer.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let cmd: IExplorerCommand = ZestCommand::new().into();
        // SAFETY: valid riid and ppv.
        unsafe { cmd.query(riid, ppv).ok() }
    }

    fn LockServer(&self, lock: BOOL) -> windows::core::Result<()> {
        if lock.as_bool() {
            LIVE_OBJECTS.fetch_add(1, Ordering::SeqCst);
        } else {
            LIVE_OBJECTS.fetch_sub(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

/// # Safety
/// Called by COM with valid (or null) pointers.
#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    let r = catch_unwind(|| {
        if rclsid.is_null() || riid.is_null() || ppv.is_null() {
            return E_POINTER;
        }
        *ppv = std::ptr::null_mut();
        if *rclsid != CLSID_ZEST_COMMAND {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        let factory: IClassFactory = Factory::new().into();
        factory.query(riid, ppv)
    });
    r.unwrap_or(E_NOINTERFACE)
}

#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if LIVE_OBJECTS.load(Ordering::SeqCst) == 0 {
        S_OK
    } else {
        S_FALSE
    }
}
