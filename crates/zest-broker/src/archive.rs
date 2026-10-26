//! The Zip action: put files and folders into one `.zip` (deflate).
//!
//! Folders are added recursively with relative paths and an entry for every
//! folder (empty folders too). Symbolic links and junctions are skipped, so a
//! link cycle cannot loop. Progress is counted in bytes. The caller writes to
//! a temp file and renames it afterwards.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
use windows::Win32::System::Time::FileTimeToSystemTime;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

/// Bytes copied between two cancel checks and progress reports.
const CHUNK: usize = 256 * 1024;

#[derive(Debug)]
pub enum ZipError {
    Cancelled,
    Failed(String),
}

impl std::fmt::Display for ZipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ZipError::Cancelled => write!(f, "Cancelled"),
            ZipError::Failed(s) => write!(f, "{s}"),
        }
    }
}

impl From<String> for ZipError {
    fn from(s: String) -> Self {
        ZipError::Failed(s)
    }
}

/// One thing that goes into the archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Source on disk.
    pub path: PathBuf,
    /// Name inside the archive, `/` separated, no leading slash. Folders end with `/`.
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").to_lowercase();
    norm(a) == norm(b)
}

/// `name`, or `name (1)`, `name (2)` ... when `taken` has it (case-insensitive).
/// A file keeps its extension: `a.txt`, `a (1).txt`.
fn unique_name(name: &str, is_dir: bool, taken: &[String]) -> String {
    let has = |n: &str| taken.iter().any(|t| t.eq_ignore_ascii_case(n));
    if !has(name) {
        return name.to_owned();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if !is_dir && i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..)
        .map(|n| format!("{stem} ({n}){ext}"))
        .find(|c| !has(c))
        .unwrap_or_else(|| name.to_owned())
}

/// List the entries for `items`, in a stable order: every item, folders depth
/// first with their contents sorted by name. `skip` (the output file) is left out.
pub fn collect(items: &[PathBuf], skip: Option<&Path>) -> Result<Vec<Entry>, String> {
    let mut out: Vec<Entry> = Vec::new();
    let mut top_names: Vec<String> = Vec::new();
    for item in items {
        let meta = fs::symlink_metadata(item)
            .map_err(|e| format!("Cannot read {}: {e}", item.display()))?;
        let raw = item
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.is_empty())
            .ok_or_else(|| format!("Cannot zip {}", item.display()))?;
        if meta.file_type().is_symlink() {
            return Err(format!("Cannot zip a link: {}", item.display()));
        }
        let is_dir = meta.is_dir();
        let top = unique_name(&raw, is_dir, &top_names);
        top_names.push(top.clone());
        if is_dir {
            add_dir(item, &top, skip, &mut out)?;
        } else if !skip.is_some_and(|s| same_path(s, item)) {
            out.push(Entry { path: item.clone(), name: top, is_dir: false, size: meta.len() });
        }
    }
    Ok(out)
}

fn add_dir(dir: &Path, name: &str, skip: Option<&Path>, out: &mut Vec<Entry>) -> Result<(), String> {
    out.push(Entry { path: dir.to_path_buf(), name: format!("{name}/"), is_dir: true, size: 0 });
    let mut children: Vec<(String, PathBuf, fs::FileType, u64)> = Vec::new();
    for e in fs::read_dir(dir).map_err(|e| format!("Cannot read {}: {e}", dir.display()))? {
        let e = e.map_err(|e| format!("Cannot read {}: {e}", dir.display()))?;
        let ft = e
            .file_type()
            .map_err(|er| format!("Cannot read {}: {er}", e.path().display()))?;
        if ft.is_symlink() {
            continue; // links and junctions: not followed
        }
        let len = if ft.is_file() {
            e.metadata().map_err(|er| format!("Cannot read {}: {er}", e.path().display()))?.len()
        } else {
            0
        };
        children.push((e.file_name().to_string_lossy().into_owned(), e.path(), ft, len));
    }
    children.sort_by_key(|a| a.0.to_lowercase());
    for (child, path, ft, len) in children {
        let full = format!("{name}/{child}");
        if ft.is_dir() {
            add_dir(&path, &full, skip, out)?;
        } else if ft.is_file() && !skip.is_some_and(|s| same_path(s, &path)) {
            out.push(Entry { path, name: full, is_dir: false, size: len });
        }
    }
    Ok(())
}

/// Local-time DOS date for a file time, if it is inside the zip range (1980 to 2107).
fn dos_time(t: SystemTime) -> Option<DateTime> {
    let since = t.duration_since(UNIX_EPOCH).ok()?;
    // 100 ns ticks since 1601-01-01.
    let ticks = (since.as_secs() + 11_644_473_600) * 10_000_000 + u64::from(since.subsec_nanos() / 100);
    let utc = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let mut local = FILETIME::default();
    let mut st = SYSTEMTIME::default();
    // SAFETY: valid pointers to local structs for the duration of the calls.
    unsafe {
        FileTimeToLocalFileTime(&utc, &mut local).ok()?;
        FileTimeToSystemTime(&local, &mut st).ok()?;
    }
    DateTime::from_date_and_time(
        st.wYear,
        st.wMonth as u8,
        st.wDay as u8,
        st.wHour as u8,
        st.wMinute as u8,
        st.wSecond as u8,
    )
    .ok()
}

fn options_for(modified: Option<SystemTime>) -> SimpleFileOptions {
    let mut o = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .large_file(false);
    if let Some(dt) = modified.and_then(dos_time) {
        o = o.last_modified_time(dt);
    }
    o
}

fn zerr(e: zip::result::ZipError) -> ZipError {
    ZipError::Failed(format!("Cannot write the zip file: {e}"))
}

/// Write `entries` to `out_path` (the file is created or truncated).
/// `progress` gets 0 to 100, about once per 256 KB. Checks `cancel` between chunks.
pub fn write_zip(
    entries: &[Entry],
    out_path: &Path,
    cancel: &AtomicBool,
    progress: &dyn Fn(f32),
) -> Result<(), ZipError> {
    let file = File::create(out_path)
        .map_err(|e| ZipError::Failed(format!("Cannot create the zip file: {e}")))?;
    let mut zip = ZipWriter::new(file).set_auto_large_file();
    let total: u64 = entries.iter().map(|e| e.size).sum::<u64>().max(1);
    let mut done: u64 = 0;
    let mut buf = vec![0u8; CHUNK];
    for entry in entries {
        if cancel.load(Ordering::SeqCst) {
            return Err(ZipError::Cancelled);
        }
        let modified = fs::metadata(&entry.path).and_then(|m| m.modified()).ok();
        let opts = options_for(modified);
        if entry.is_dir {
            zip.add_directory(entry.name.clone(), opts).map_err(zerr)?;
            continue;
        }
        let mut src = File::open(&entry.path)
            .map_err(|e| ZipError::Failed(format!("Cannot read {}: {e}", entry.path.display())))?;
        zip.start_file(entry.name.clone(), opts).map_err(zerr)?;
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err(ZipError::Cancelled);
            }
            let n = src
                .read(&mut buf)
                .map_err(|e| ZipError::Failed(format!("Cannot read {}: {e}", entry.path.display())))?;
            if n == 0 {
                break;
            }
            zip.write_all(&buf[..n])
                .map_err(|e| ZipError::Failed(format!("Cannot write the zip file: {e}")))?;
            done += n as u64;
            progress((done as f64 / total as f64 * 100.0).min(100.0) as f32);
        }
    }
    let mut file = zip.finish().map_err(zerr)?;
    file.flush()
        .map_err(|e| ZipError::Failed(format!("Cannot write the zip file: {e}")))?;
    progress(100.0);
    Ok(())
}

/// Zip `items` into `out_path`.
pub fn zip_items(
    items: &[PathBuf],
    out_path: &Path,
    cancel: &AtomicBool,
    progress: &dyn Fn(f32),
) -> Result<(), ZipError> {
    if items.is_empty() {
        return Err(ZipError::Failed("Nothing to zip".to_owned()));
    }
    let entries = collect(items, Some(out_path))?;
    write_zip(&entries, out_path, cancel, progress)
}

/// Extract a zip archive into `dest` (created if needed). Rejects path traversal.
pub fn extract_zip(zip_path: &Path, dest: &Path, cancel: &AtomicBool) -> Result<usize, ZipError> {
    use std::io::copy;
    let file = File::open(zip_path).map_err(|e| ZipError::Failed(format!("Cannot open zip: {e}")))?;
    let mut archive = zip::ZipArchive::new(file).map_err(zerr)?;
    fs::create_dir_all(dest).map_err(|e| ZipError::Failed(format!("Cannot create folder: {e}")))?;
    let dest = dest.canonicalize().unwrap_or_else(|_| dest.to_path_buf());
    let mut count = 0usize;
    for i in 0..archive.len() {
        if cancel.load(Ordering::SeqCst) {
            return Err(ZipError::Cancelled);
        }
        let mut entry = archive.by_index(i).map_err(zerr)?;
        let Some(name) = entry.enclosed_name() else {
            return Err(ZipError::Failed("Unsafe path inside the zip".to_owned()));
        };
        let out = dest.join(name);
        if !out.starts_with(&dest) {
            return Err(ZipError::Failed("Unsafe path inside the zip".to_owned()));
        }
        if entry.is_dir() {
            fs::create_dir_all(&out).map_err(|e| ZipError::Failed(format!("Cannot create folder: {e}")))?;
        } else {
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| ZipError::Failed(format!("Cannot create folder: {e}")))?;
            }
            let mut outfile =
                File::create(&out).map_err(|e| ZipError::Failed(format!("Cannot write: {e}")))?;
            copy(&mut entry, &mut outfile).map_err(|e| ZipError::Failed(format!("Cannot write: {e}")))?;
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;
    use std::sync::Mutex;

    static N: AtomicU32 = AtomicU32::new(0);

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "zest-zip-{name}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    fn read_back(zip_path: &Path) -> Vec<(String, Vec<u8>)> {
        let f = File::open(zip_path).unwrap();
        let mut a = zip::ZipArchive::new(f).unwrap();
        (0..a.len())
            .map(|i| {
                let mut e = a.by_index(i).unwrap();
                let mut v = Vec::new();
                e.read_to_end(&mut v).unwrap();
                (e.name().unwrap().into_owned(), v)
            })
            .collect()
    }

    #[test]
    fn unique_names() {
        let taken = vec!["a.txt".to_owned(), "A (1).txt".to_owned(), "dir".to_owned()];
        assert_eq!(unique_name("b.txt", false, &taken), "b.txt");
        assert_eq!(unique_name("a.txt", false, &taken), "a (2).txt");
        assert_eq!(unique_name("dir", true, &taken), "dir (1)");
        assert_eq!(unique_name("noext", false, &["noext".to_owned()]), "noext (1)");
    }

    #[test]
    fn single_file_and_multi_file() {
        let d = tmp("files");
        fs::write(d.join("a.txt"), b"hello").unwrap();
        fs::write(d.join("b.bin"), vec![7u8; 300_000]).unwrap();
        let out = d.join("out.zip");
        let seen = Mutex::new(Vec::new());
        zip_items(&[d.join("a.txt"), d.join("b.bin")], &out, &AtomicBool::new(false), &|p| {
            seen.lock().unwrap().push(p)
        })
        .unwrap();
        let back = read_back(&out);
        assert_eq!(back.len(), 2);
        assert_eq!(back[0], ("a.txt".to_owned(), b"hello".to_vec()));
        assert_eq!(back[1].0, "b.bin");
        assert_eq!(back[1].1, vec![7u8; 300_000]);
        let seen = seen.lock().unwrap();
        assert_eq!(*seen.last().unwrap(), 100.0);
        assert!(seen.windows(2).all(|w| w[0] <= w[1]), "{seen:?}");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn folder_is_recursive_with_relative_paths() {
        let d = tmp("folder");
        let root = d.join("Photos");
        fs::create_dir_all(root.join("sub").join("deep")).unwrap();
        fs::create_dir_all(root.join("empty")).unwrap();
        fs::write(root.join("x.txt"), b"x").unwrap();
        fs::write(root.join("sub").join("y.txt"), b"yy").unwrap();
        fs::write(root.join("sub").join("deep").join("z.txt"), b"zzz").unwrap();
        let entries = collect(std::slice::from_ref(&root), None).unwrap();
        assert_eq!(
            names(&entries),
            [
                "Photos/",
                "Photos/empty/",
                "Photos/sub/",
                "Photos/sub/deep/",
                "Photos/sub/deep/z.txt",
                "Photos/sub/y.txt",
                "Photos/x.txt"
            ]
        );
        let out = d.join("Photos.zip");
        zip_items(&[root], &out, &AtomicBool::new(false), &|_| {}).unwrap();
        let back = read_back(&out);
        let z = back.iter().find(|(n, _)| n == "Photos/sub/deep/z.txt").unwrap();
        assert_eq!(z.1, b"zzz");
        assert!(back.iter().any(|(n, _)| n == "Photos/empty/"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn same_names_get_a_counter_and_output_is_skipped() {
        let d = tmp("dups");
        fs::create_dir_all(d.join("one")).unwrap();
        fs::create_dir_all(d.join("two")).unwrap();
        fs::write(d.join("one").join("a.txt"), b"1").unwrap();
        fs::write(d.join("two").join("a.txt"), b"2").unwrap();
        let entries = collect(&[d.join("one").join("a.txt"), d.join("two").join("a.txt")], None).unwrap();
        assert_eq!(names(&entries), ["a.txt", "a (1).txt"]);
        // The output file inside a zipped folder is not added to itself.
        let out = d.join("one").join("self.zip");
        fs::write(&out, b"").unwrap();
        let entries = collect(&[d.join("one")], Some(&out)).unwrap();
        assert_eq!(names(&entries), ["one/", "one/a.txt"]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn unicode_names_and_cancel() {
        let d = tmp("uni");
        let f = d.join("caf\u{e9} \u{65e5}\u{672c}.txt");
        fs::write(&f, b"data").unwrap();
        let out = d.join("u.zip");
        zip_items(std::slice::from_ref(&f), &out, &AtomicBool::new(false), &|_| {}).unwrap();
        assert_eq!(read_back(&out)[0].0, "caf\u{e9} \u{65e5}\u{672c}.txt");
        let cancel = AtomicBool::new(true);
        let r = zip_items(std::slice::from_ref(&f), &d.join("c.zip"), &cancel, &|_| {});
        assert!(matches!(r, Err(ZipError::Cancelled)));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_item_and_empty_list_fail() {
        let d = tmp("missing");
        let r = zip_items(&[d.join("nope.txt")], &d.join("o.zip"), &AtomicBool::new(false), &|_| {});
        assert!(matches!(r, Err(ZipError::Failed(m)) if m.contains("nope.txt")));
        let r = zip_items(&[], &d.join("o.zip"), &AtomicBool::new(false), &|_| {});
        assert!(matches!(r, Err(ZipError::Failed(_))));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn dos_time_is_in_range() {
        let dt = dos_time(SystemTime::now()).unwrap();
        assert!(dt.year() >= 2024);
        assert!(dos_time(UNIX_EPOCH).is_none()); // 1970 is before 1980
    }
}
