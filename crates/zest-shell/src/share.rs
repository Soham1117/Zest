//! The Windows share sheet for files (`DataTransferManager` for a window).
//!
//! `IDataTransferManagerInterop::ShowShareUIForWindow` opens the sheet for an
//! HWND. The `DataRequested` handler then fills the `DataPackage` with
//! `StorageFile`s. `DataPackage::SetStorageItems` wants an `IIterable<IStorageItem>`.
//! windows-rs 0.58 cannot build one, so a small read-only COM object is
//! written by hand here (an iterable and its iterator over a list).
//!
//! Only files can be shared (folders are ignored). Call from the UI thread.

use crate::{ComScope, Result, ShellError};
use std::cell::RefCell;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;
use windows::core::{Interface, Result as WinResult, GUID, HRESULT, HSTRING};
use windows::ApplicationModel::DataTransfer::{DataRequestedEventArgs, DataTransferManager};
use windows::Foundation::Collections::{IIterable, IIterator};
use windows::Foundation::{EventRegistrationToken, TypedEventHandler};
use windows::Storage::{IStorageItem, StorageFile};
use windows::Win32::Foundation::{E_NOINTERFACE, E_POINTER, HWND, S_OK};
use windows::Win32::UI::Shell::IDataTransferManagerInterop;

// ---------------------------------------------------------------------------
// IIterable<IStorageItem> over a list
// ---------------------------------------------------------------------------

#[repr(C)]
struct InspectableVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_iids: unsafe extern "system" fn(*mut c_void, *mut u32, *mut *mut GUID) -> HRESULT,
    get_runtime_class_name: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    get_trust_level: unsafe extern "system" fn(*mut c_void, *mut i32) -> HRESULT,
}

#[repr(C)]
struct IterableVtbl {
    base: InspectableVtbl,
    first: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
}

#[repr(C)]
struct IteratorVtbl {
    base: InspectableVtbl,
    current: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
    has_current: unsafe extern "system" fn(*mut c_void, *mut bool) -> HRESULT,
    move_next: unsafe extern "system" fn(*mut c_void, *mut bool) -> HRESULT,
    get_many: unsafe extern "system" fn(*mut c_void, u32, *mut *mut c_void, *mut u32) -> HRESULT,
}

/// One COM object: either the iterable or its iterator. `vtbl` must be the first field.
#[repr(C)]
struct ListObject {
    vtbl: *const c_void,
    refs: AtomicU32,
    /// The IID of the interface that this object implements besides IUnknown and IInspectable.
    iid: GUID,
    items: Arc<Vec<IStorageItem>>,
    pos: AtomicUsize,
}

const IID_IUNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
const IID_IINSPECTABLE: GUID = GUID::from_u128(0xaf86e2e0_b12d_4c6a_9c5a_d7aa65101e90);

static ITERABLE_VTBL: IterableVtbl = IterableVtbl {
    base: InspectableVtbl {
        query_interface: qi,
        add_ref,
        release,
        get_iids,
        get_runtime_class_name,
        get_trust_level,
    },
    first,
};

static ITERATOR_VTBL: IteratorVtbl = IteratorVtbl {
    base: InspectableVtbl {
        query_interface: qi,
        add_ref,
        release,
        get_iids,
        get_runtime_class_name,
        get_trust_level,
    },
    current,
    has_current,
    move_next,
    get_many,
};

fn new_object(vtbl: *const c_void, iid: GUID, items: Arc<Vec<IStorageItem>>) -> *mut c_void {
    Box::into_raw(Box::new(ListObject { vtbl, refs: AtomicU32::new(1), iid, items, pos: AtomicUsize::new(0) }))
        as *mut c_void
}

unsafe extern "system" fn qi(this: *mut c_void, riid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    if riid.is_null() || out.is_null() {
        return E_POINTER;
    }
    let obj = &*(this as *const ListObject);
    let id = *riid;
    if id == IID_IUNKNOWN || id == IID_IINSPECTABLE || id == obj.iid {
        obj.refs.fetch_add(1, Ordering::AcqRel);
        *out = this;
        S_OK
    } else {
        *out = std::ptr::null_mut();
        E_NOINTERFACE
    }
}

unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
    (*(this as *const ListObject)).refs.fetch_add(1, Ordering::AcqRel) + 1
}

unsafe extern "system" fn release(this: *mut c_void) -> u32 {
    let left = (*(this as *const ListObject)).refs.fetch_sub(1, Ordering::AcqRel) - 1;
    if left == 0 {
        drop(Box::from_raw(this as *mut ListObject));
    }
    left
}

unsafe extern "system" fn get_iids(_: *mut c_void, count: *mut u32, iids: *mut *mut GUID) -> HRESULT {
    if !count.is_null() {
        *count = 0;
    }
    if !iids.is_null() {
        *iids = std::ptr::null_mut();
    }
    S_OK
}

unsafe extern "system" fn get_runtime_class_name(_: *mut c_void, name: *mut *mut c_void) -> HRESULT {
    if !name.is_null() {
        *name = std::ptr::null_mut();
    }
    S_OK
}

unsafe extern "system" fn get_trust_level(_: *mut c_void, level: *mut i32) -> HRESULT {
    if !level.is_null() {
        *level = 0; // BaseTrust
    }
    S_OK
}

unsafe extern "system" fn first(this: *mut c_void, out: *mut *mut c_void) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    let obj = &*(this as *const ListObject);
    *out = new_object(
        &ITERATOR_VTBL as *const IteratorVtbl as *const c_void,
        <IIterator<IStorageItem> as Interface>::IID,
        Arc::clone(&obj.items),
    );
    S_OK
}

unsafe extern "system" fn current(this: *mut c_void, out: *mut *mut c_void) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    let obj = &*(this as *const ListObject);
    match obj.items.get(obj.pos.load(Ordering::Acquire)) {
        Some(item) => {
            *out = item.clone().into_raw();
            S_OK
        }
        None => {
            *out = std::ptr::null_mut();
            windows::Win32::Foundation::E_BOUNDS
        }
    }
}

unsafe extern "system" fn has_current(this: *mut c_void, out: *mut bool) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    let obj = &*(this as *const ListObject);
    *out = obj.pos.load(Ordering::Acquire) < obj.items.len();
    S_OK
}

unsafe extern "system" fn move_next(this: *mut c_void, out: *mut bool) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    let obj = &*(this as *const ListObject);
    let next = (obj.pos.load(Ordering::Acquire) + 1).min(obj.items.len());
    obj.pos.store(next, Ordering::Release);
    *out = next < obj.items.len();
    S_OK
}

unsafe extern "system" fn get_many(this: *mut c_void, capacity: u32, items: *mut *mut c_void, actual: *mut u32) -> HRESULT {
    if items.is_null() || actual.is_null() {
        return E_POINTER;
    }
    let obj = &*(this as *const ListObject);
    let start = obj.pos.load(Ordering::Acquire);
    let n = obj.items.len().saturating_sub(start).min(capacity as usize);
    for i in 0..n {
        *items.add(i) = obj.items[start + i].clone().into_raw();
    }
    obj.pos.store(start + n, Ordering::Release);
    *actual = n as u32;
    S_OK
}

/// An `IIterable<IStorageItem>` over these items.
fn iterable_of(items: Vec<IStorageItem>) -> IIterable<IStorageItem> {
    // StorageFile objects are agile, so the list may be used from any thread.
    #[allow(clippy::arc_with_non_send_sync)]
    let list = Arc::new(items);
    let raw = new_object(
        &ITERABLE_VTBL as *const IterableVtbl as *const c_void,
        <IIterable<IStorageItem> as Interface>::IID,
        list,
    );
    // SAFETY: `raw` is a valid COM object with one reference, which the returned interface owns.
    unsafe { IIterable::<IStorageItem>::from_raw(raw) }
}

// ---------------------------------------------------------------------------
// Share
// ---------------------------------------------------------------------------

/// The list is only touched by the `DataRequested` handler. WinRT runs it on the
/// thread that registered it (the UI thread), and the list is read-only.
struct SendIterable(IIterable<IStorageItem>);

// SAFETY: see above; the object itself is thread safe (atomic counters, immutable list).
unsafe impl Send for SendIterable {}

impl SendIterable {
    /// A method (not the field) so that a closure captures the whole wrapper.
    fn get(&self) -> &IIterable<IStorageItem> {
        &self.0
    }
}

thread_local! {
    /// The `DataRequested` registration of the last share, so a new share replaces it.
    static REGISTERED: RefCell<Option<(DataTransferManager, EventRegistrationToken)>> = const { RefCell::new(None) };
}

fn storage_file(path: &Path) -> WinResult<IStorageItem> {
    let p = HSTRING::from(path.as_os_str());
    let file: StorageFile = StorageFile::GetFileFromPathAsync(&p)?.get()?;
    file.cast::<IStorageItem>()
}

/// Open the share sheet for these files. Folders and missing files are skipped.
/// `owner` is the window the sheet belongs to (it can be hidden). UI thread only.
pub fn share_files(owner: HWND, files: &[PathBuf]) -> Result<()> {
    let _com = ComScope::new();
    let paths: Vec<&PathBuf> = files.iter().filter(|p| p.is_file()).collect();
    if paths.is_empty() {
        return Err(ShellError::Invalid("Share needs files, not folders".to_owned()));
    }
    let mut items = Vec::with_capacity(paths.len());
    for p in &paths {
        let abs = std::path::absolute(p).unwrap_or_else(|_| (*p).clone());
        items.push(storage_file(&abs)?);
    }
    let title = if items.len() == 1 {
        paths[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    } else {
        format!("{} files", items.len())
    };

    let interop = windows::core::factory::<DataTransferManager, IDataTransferManagerInterop>()?;
    // SAFETY: `owner` is a window of this process.
    let manager: DataTransferManager = unsafe { interop.GetForWindow(owner)? };
    REGISTERED.with(|r| {
        if let Some((m, token)) = r.borrow_mut().take() {
            let _ = m.RemoveDataRequested(token);
        }
    });
    let iterable = SendIterable(iterable_of(items));
    let handler = TypedEventHandler::<DataTransferManager, DataRequestedEventArgs>::new(move |_, args| {
        if let Some(args) = args.as_ref() {
            let data = args.Request()?.Data()?;
            data.Properties()?.SetTitle(&HSTRING::from(title.as_str()))?;
            data.SetStorageItems(iterable.get(), true)?;
        }
        Ok(())
    });
    let token = manager.DataRequested(&handler)?;
    REGISTERED.with(|r| *r.borrow_mut() = Some((manager, token)));
    // SAFETY: `owner` is a window of this process.
    unsafe { interop.ShowShareUIForWindow(owner)? };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_object_iterates_like_a_winrt_collection() {
        let _com = ComScope::new();
        let dir = std::env::temp_dir().join(format!("zest-share-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths: Vec<PathBuf> = ["a.txt", "b.txt", "c.txt"]
            .iter()
            .map(|n| {
                let p = dir.join(n);
                std::fs::write(&p, n).unwrap();
                p
            })
            .collect();
        let items: Vec<IStorageItem> = paths.iter().map(|p| storage_file(p).expect("storage file")).collect();
        let list = iterable_of(items);
        // One by one.
        let it = list.First().unwrap();
        let mut names = Vec::new();
        while it.HasCurrent().unwrap() {
            names.push(it.Current().unwrap().Name().unwrap().to_string());
            it.MoveNext().unwrap();
        }
        assert_eq!(names, ["a.txt", "b.txt", "c.txt"]);
        // A second iterator starts again at the first item.
        let again = list.First().unwrap();
        assert_eq!(again.Current().unwrap().Name().unwrap().to_string(), "a.txt");
        // In one call.
        let mut buf: [Option<IStorageItem>; 2] = [None, None];
        let it = list.First().unwrap();
        assert_eq!(it.GetMany(&mut buf).unwrap(), 2);
        assert_eq!(buf[1].as_ref().unwrap().Name().unwrap().to_string(), "b.txt");
        drop((it, again, list));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn folders_and_missing_files_are_not_shared() {
        let dir = std::env::temp_dir();
        let r = share_files(HWND::default(), &[dir, PathBuf::from(r"C:\no\such\file.txt")]);
        assert!(matches!(r, Err(ShellError::Invalid(_))));
    }
}
