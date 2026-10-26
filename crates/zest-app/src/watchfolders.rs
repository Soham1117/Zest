//! Watch folders: new files in a folder are processed by themselves.
//!
//! One thread per folder blocks in `ReadDirectoryChangesW` and sends the paths
//! that appeared or changed. One decider thread keeps the waiting files and
//! applies Filee's rule (`zest_core::watch::Stability`): a file is ready when
//! its size and time stay the same for 2 s and it opens with no sharing. It
//! tries again with a growing pause for up to 5 minutes. Ready files go to the
//! app (UI thread), which hands them to the broker like any other job.
//!
//! Never converted: Zest's own temp files, hidden and system files, files
//! inside output folders (`(pages)`), files that Zest itself just made
//! ([`Produced`]), and files whose name does not pass `include`.

use crossbeam_channel::{unbounded, Receiver, RecvTimeoutError, Sender};
use std::collections::HashMap;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY,
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FILE_NOTIFY_INFORMATION,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::IO::CancelIoEx;
use zest_core::watch::{ignored_name, in_output_folder, parse_watch_op, FileSig, Stability, Verdict};
use zest_core::{FormatId, Operation, WatchEntry};

const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
/// How long Zest remembers a file that it made itself.
const PRODUCED_TTL: Duration = Duration::from_secs(15 * 60);

/// A file that is ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchHit {
    pub path: PathBuf,
    pub op: Operation,
}

/// Files that Zest made (the broker's outputs). A watcher ignores them.
#[derive(Clone, Default)]
pub struct Produced(Arc<Mutex<HashMap<String, Instant>>>);

fn key(p: &Path) -> String {
    p.to_string_lossy().to_lowercase()
}

impl Produced {
    pub fn note(&self, p: &Path) {
        if let Ok(mut m) = self.0.lock() {
            let now = Instant::now();
            m.retain(|_, t| now.duration_since(*t) < PRODUCED_TTL);
            m.insert(key(p), now);
        }
    }

    pub fn contains(&self, p: &Path) -> bool {
        self.0.lock().is_ok_and(|m| m.get(&key(p)).is_some_and(|t| t.elapsed() < PRODUCED_TTL))
    }
}

/// A path event from a folder thread.
struct Raw {
    entry: usize,
    path: PathBuf,
    /// The file is gone (deleted or renamed away).
    gone: bool,
}

struct Rule {
    root: PathBuf,
    entry: WatchEntry,
    op: Operation,
}

/// Running watchers. Dropping it stops them.
pub struct WatchSet {
    stop: Arc<AtomicBool>,
    folders: Vec<FolderThread>,
    decider: Option<std::thread::JoinHandle<()>>,
}

struct FolderThread {
    handle: Arc<AtomicIsize>,
    done: Arc<AtomicBool>,
}

impl WatchSet {
    /// Start a watcher for every enabled entry with a known operation and an existing folder.
    pub fn start(
        entries: &[WatchEntry],
        produced: Produced,
        paused: Arc<AtomicBool>,
        on_ready: Arc<dyn Fn(WatchHit) + Send + Sync>,
    ) -> WatchSet {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = unbounded::<Raw>();
        let mut rules = Vec::new();
        let mut folders = Vec::new();
        for e in entries.iter().filter(|e| e.enabled) {
            let Some(op) = parse_watch_op(&e.op) else {
                tracing::warn!(op = %e.op, "watch folder: unknown operation, entry skipped");
                continue;
            };
            if !e.folder.is_dir() {
                tracing::warn!(folder = %e.folder.display(), "watch folder: folder not found, entry skipped");
                continue;
            }
            let idx = rules.len();
            rules.push(Rule { root: e.folder.clone(), entry: e.clone(), op });
            let ft = FolderThread { handle: Arc::new(AtomicIsize::new(0)), done: Arc::new(AtomicBool::new(false)) };
            let (h, d, s, t) = (Arc::clone(&ft.handle), Arc::clone(&ft.done), Arc::clone(&stop), tx.clone());
            let (folder, recursive) = (e.folder.clone(), e.recursive);
            let spawned = std::thread::Builder::new().name("zest-watch-dir".into()).spawn(move || {
                watch_dir(idx, &folder, recursive, &h, &s, &t);
                d.store(true, Ordering::SeqCst);
            });
            if spawned.is_ok() {
                tracing::info!(folder = %e.folder.display(), op = %e.op, recursive, "watching folder");
                folders.push(ft);
            } else {
                tracing::error!("cannot start a watch thread");
            }
        }
        drop(tx);
        let s = Arc::clone(&stop);
        let decider = std::thread::Builder::new()
            .name("zest-watch-decide".into())
            .spawn(move || decide(&rx, &rules, &produced, &paused, &s, &*on_ready))
            .ok();
        WatchSet { stop, folders, decider }
    }

    pub fn is_empty(&self) -> bool {
        self.folders.is_empty()
    }
}

impl Drop for WatchSet {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Cancel the blocked reads until every folder thread has left.
        for _ in 0..100 {
            let mut all = true;
            for f in &self.folders {
                if !f.done.load(Ordering::SeqCst) {
                    all = false;
                    let h = f.handle.load(Ordering::SeqCst);
                    if h != 0 {
                        // SAFETY: the handle stays open until its thread has set `done`.
                        unsafe {
                            let _ = CancelIoEx(HANDLE(h as *mut _), None);
                        }
                    }
                }
            }
            if all {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if let Some(d) = self.decider.take() {
            let _ = d.join();
        }
    }
}

fn watch_dir(idx: usize, folder: &Path, recursive: bool, handle_out: &AtomicIsize, stop: &AtomicBool, tx: &Sender<Raw>) {
    let wdir: Vec<u16> = {
        use std::os::windows::ffi::OsStrExt;
        folder.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
    };
    // SAFETY: NUL-terminated path; the handle is closed below.
    let h = unsafe {
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
    let h = match h {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(folder = %folder.display(), "watch folder: cannot open: {e}");
            return;
        }
    };
    handle_out.store(h.0 as isize, Ordering::SeqCst);
    // DWORD aligned, as the kernel writes FILE_NOTIFY_INFORMATION records.
    let mut buf = vec![0u32; 16 * 1024];
    while !stop.load(Ordering::SeqCst) {
        let mut bytes = 0u32;
        // SAFETY: valid handle and buffer; the call blocks until a change or CancelIoEx.
        let r = unsafe {
            ReadDirectoryChangesW(
                h,
                buf.as_mut_ptr() as *mut _,
                (buf.len() * 4) as u32,
                recursive,
                FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_SIZE,
                Some(&mut bytes),
                None,
                None,
            )
        };
        if stop.load(Ordering::SeqCst) {
            break;
        }
        if let Err(e) = r {
            tracing::warn!(folder = %folder.display(), "watch folder: {e}");
            break;
        }
        if bytes == 0 {
            // Too many changes for the buffer: those files are not seen.
            tracing::warn!(folder = %folder.display(), "watch folder: change buffer overflow");
            continue;
        }
        // SAFETY: the kernel filled `bytes` bytes of the buffer with records.
        let bytes_view = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, bytes as usize) };
        for (action, name) in parse_records(bytes_view) {
            let gone = matches!(action, 2 | 4); // removed, renamed away
            if tx.send(Raw { entry: idx, path: folder.join(name), gone }).is_err() {
                break;
            }
        }
    }
    handle_out.store(0, Ordering::SeqCst);
    // SAFETY: handle from CreateFileW.
    unsafe {
        let _ = CloseHandle(h);
    }
}

/// `(action, relative name)` for every record of a notification buffer.
fn parse_records(buf: &[u8]) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    let mut off = 0usize;
    let header = std::mem::offset_of!(FILE_NOTIFY_INFORMATION, FileName);
    loop {
        if off + header > buf.len() {
            break;
        }
        let rd = |at: usize| u32::from_le_bytes([buf[off + at], buf[off + at + 1], buf[off + at + 2], buf[off + at + 3]]);
        let next = rd(std::mem::offset_of!(FILE_NOTIFY_INFORMATION, NextEntryOffset)) as usize;
        let action = rd(std::mem::offset_of!(FILE_NOTIFY_INFORMATION, Action));
        let name_len = rd(std::mem::offset_of!(FILE_NOTIFY_INFORMATION, FileNameLength)) as usize;
        if off + header + name_len > buf.len() {
            break;
        }
        let units: Vec<u16> = buf[off + header..off + header + name_len]
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c.get(1).copied().unwrap_or(0)]))
            .collect();
        out.push((action, String::from_utf16_lossy(&units)));
        if next == 0 {
            break;
        }
        off += next;
    }
    out
}

struct Waiting {
    rule: usize,
    st: Stability,
}

fn file_sig(path: &Path) -> Option<(FileSig, u32)> {
    let m = std::fs::metadata(path).ok()?;
    let mtime = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as u64);
    Some((FileSig { size: m.len(), mtime }, m.file_attributes()))
}

/// Opens with share mode 0: fails while any other program has the file open.
fn can_open_exclusive(path: &Path) -> bool {
    std::fs::OpenOptions::new().read(true).share_mode(0).open(path).is_ok()
}

/// Cheap checks on a name before a file starts waiting.
fn worth_waiting(rule: &Rule, path: &Path, produced: &Produced) -> bool {
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else { return false };
    if ignored_name(&name) || !rule.entry.accepts(&name) {
        return false;
    }
    if in_output_folder(&rule.root, path) || produced.contains(path) {
        return false;
    }
    // Only files with a known type; the app checks the operation.
    path.extension().is_some_and(|e| FormatId::from_extension(&e.to_string_lossy()).is_some())
}

fn decide(
    rx: &Receiver<Raw>,
    rules: &[Rule],
    produced: &Produced,
    paused: &AtomicBool,
    stop: &AtomicBool,
    on_ready: &(dyn Fn(WatchHit) + Send + Sync),
) {
    let start = Instant::now();
    let mut waiting: HashMap<PathBuf, Waiting> = HashMap::new();
    loop {
        let now = start.elapsed();
        let wait = waiting
            .values()
            .map(|w| w.st.next_check().saturating_sub(now))
            .min()
            .unwrap_or(Duration::from_millis(500))
            .clamp(Duration::from_millis(20), Duration::from_millis(500));
        match rx.recv_timeout(wait) {
            Ok(ev) => {
                let now = start.elapsed();
                if paused.load(Ordering::SeqCst) {
                    // Paused: files that arrive now are skipped.
                    waiting.clear();
                    continue;
                }
                if ev.gone {
                    waiting.remove(&ev.path);
                } else if let Some(rule) = rules.get(ev.entry) {
                    if let Some(w) = waiting.get_mut(&ev.path) {
                        w.st.touched(now);
                    } else if worth_waiting(rule, &ev.path, produced) {
                        waiting.insert(ev.path, Waiting { rule: ev.entry, st: Stability::new(now) });
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                if waiting.is_empty() || stop.load(Ordering::SeqCst) {
                    return;
                }
                // The folder threads are gone; finish the files that still wait.
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        if stop.load(Ordering::SeqCst) {
            return;
        }
        if paused.load(Ordering::SeqCst) {
            waiting.clear();
            continue;
        }
        let now = start.elapsed();
        let due: Vec<PathBuf> = waiting.iter().filter(|(_, w)| w.st.next_check() <= now).map(|(p, _)| p.clone()).collect();
        for path in due {
            let Some(w) = waiting.get_mut(&path) else { continue };
            let sig = file_sig(&path);
            let hidden = sig.is_some_and(|(_, a)| a & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM | FILE_ATTRIBUTE_DIRECTORY) != 0);
            if hidden {
                waiting.remove(&path);
                continue;
            }
            let verdict = w.st.check(now, sig.map(|(s, _)| s), || can_open_exclusive(&path));
            match verdict {
                Verdict::Wait => {}
                Verdict::Ready => {
                    let rule = &rules[w.rule];
                    waiting.remove(&path);
                    // A file that Zest made while it waited.
                    if produced.contains(&path) {
                        continue;
                    }
                    tracing::info!(file = %path.file_name().unwrap_or_default().to_string_lossy(), "watch folder: file is ready");
                    on_ready(WatchHit { path, op: rule.op });
                }
                Verdict::Gone => {
                    waiting.remove(&path);
                }
                Verdict::GiveUp => {
                    tracing::warn!(file = %path.file_name().unwrap_or_default().to_string_lossy(), "watch folder: gave up after 5 minutes");
                    waiting.remove(&path);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("zest-watch-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn entry(folder: &Path, op: &str, include: &[&str], recursive: bool) -> WatchEntry {
        WatchEntry {
            folder: folder.to_path_buf(),
            op: op.into(),
            recursive,
            enabled: true,
            include: include.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    type Seen = Arc<Mutex<Vec<PathBuf>>>;
    type Callback = Arc<dyn Fn(WatchHit) + Send + Sync>;

    fn hits() -> (Seen, Callback) {
        let v = Arc::new(Mutex::new(Vec::new()));
        let v2 = Arc::clone(&v);
        (v, Arc::new(move |h: WatchHit| v2.lock().unwrap().push(h.path)))
    }

    #[test]
    fn records_are_parsed() {
        // One record: action 1, name "a.png" (UTF-16).
        let name: Vec<u8> = "a.png".encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mut rec = Vec::new();
        rec.extend_from_slice(&0u32.to_le_bytes());
        rec.extend_from_slice(&1u32.to_le_bytes());
        rec.extend_from_slice(&(name.len() as u32).to_le_bytes());
        rec.extend_from_slice(&name);
        assert_eq!(parse_records(&rec), vec![(1, "a.png".to_owned())]);
        // Two records.
        let mut two = Vec::new();
        let first_len = 12 + name.len();
        two.extend_from_slice(&(first_len as u32).to_le_bytes());
        two.extend_from_slice(&3u32.to_le_bytes());
        two.extend_from_slice(&(name.len() as u32).to_le_bytes());
        two.extend_from_slice(&name);
        two.extend_from_slice(&rec);
        assert_eq!(parse_records(&two), vec![(3, "a.png".to_owned()), (1, "a.png".to_owned())]);
        assert!(parse_records(&[1, 2, 3]).is_empty());
    }

    #[test]
    fn produced_files_are_remembered_case_insensitively() {
        let p = Produced::default();
        assert!(!p.contains(Path::new(r"D:\In\a.jpg")));
        p.note(Path::new(r"D:\In\A.JPG"));
        assert!(p.contains(Path::new(r"d:\in\a.jpg")));
    }

    #[test]
    fn name_rules() {
        let rule = Rule { root: PathBuf::from(r"D:\In"), entry: entry(Path::new(r"D:\In"), "convert:jpg", &["*.heic"], true), op: parse_watch_op("convert:jpg").unwrap() };
        let produced = Produced::default();
        assert!(worth_waiting(&rule, Path::new(r"D:\In\a.heic"), &produced));
        assert!(worth_waiting(&rule, Path::new(r"D:\In\sub\a.HEIC"), &produced));
        assert!(!worth_waiting(&rule, Path::new(r"D:\In\a.png"), &produced));
        assert!(!worth_waiting(&rule, Path::new(r"D:\In\a.heic.zest-tmp"), &produced));
        assert!(!worth_waiting(&rule, Path::new(r"D:\In\scan (pages)\a.heic"), &produced));
        produced.note(Path::new(r"D:\In\b.heic"));
        assert!(!worth_waiting(&rule, Path::new(r"D:\In\b.heic"), &produced));
        let all = Rule { entry: entry(Path::new(r"D:\In"), "compress", &[], false), ..rule };
        assert!(!worth_waiting(&all, Path::new(r"D:\In\notes.xyz"), &produced), "unknown type");
        assert!(worth_waiting(&all, Path::new(r"D:\In\x.mp4"), &produced));
    }

    #[test]
    fn a_new_file_is_reported_after_it_is_stable() {
        let dir = temp("new");
        let (seen, cb) = hits();
        let produced = Produced::default();
        let set = WatchSet::start(
            &[entry(&dir, "convert:jpg", &["*.png"], false)],
            produced.clone(),
            Arc::new(AtomicBool::new(false)),
            cb,
        );
        assert!(!set.is_empty());
        std::thread::sleep(Duration::from_millis(300));
        // A file that is written in pieces, a file that is ignored, one that Zest made.
        let wanted = dir.join("a.png");
        std::fs::write(&wanted, b"1").unwrap();
        std::fs::write(dir.join("b.txt"), b"x").unwrap();
        std::fs::write(dir.join("c.png.zest-tmp"), b"x").unwrap();
        std::fs::write(dir.join("d.png"), b"x").unwrap();
        produced.note(&dir.join("d.png"));
        std::thread::sleep(Duration::from_millis(1200));
        std::fs::write(&wanted, b"12").unwrap(); // grows: waits another 2 s
        std::thread::sleep(Duration::from_millis(1200));
        assert!(seen.lock().unwrap().is_empty(), "reported too early: {:?}", seen.lock().unwrap());
        let deadline = Instant::now() + Duration::from_secs(6);
        while seen.lock().unwrap().is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        std::thread::sleep(Duration::from_millis(500));
        let got = seen.lock().unwrap().clone();
        assert_eq!(got, vec![wanted], "{got:?}");
        drop(set);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_held_open_waits_until_it_is_released() {
        let dir = temp("held");
        let (seen, cb) = hits();
        let set = WatchSet::start(
            &[entry(&dir, "compress", &[], false)],
            Produced::default(),
            Arc::new(AtomicBool::new(false)),
            cb,
        );
        std::thread::sleep(Duration::from_millis(300));
        let path = dir.join("busy.png");
        let writer = std::fs::File::create(&path).unwrap(); // share mode: others may read, nobody may open exclusively
        std::thread::sleep(Duration::from_millis(4500));
        assert!(seen.lock().unwrap().is_empty(), "reported while open");
        drop(writer);
        let deadline = Instant::now() + Duration::from_secs(8);
        while seen.lock().unwrap().is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(seen.lock().unwrap().clone(), vec![path]);
        drop(set);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pause_subfolders_and_stop() {
        let dir = temp("misc");
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let (seen, cb) = hits();
        let paused = Arc::new(AtomicBool::new(true));
        let set = WatchSet::start(
            &[entry(&dir, "compress", &[], true), entry(&dir.join("nope"), "compress", &[], false)],
            Produced::default(),
            Arc::clone(&paused),
            cb,
        );
        std::thread::sleep(Duration::from_millis(300));
        std::fs::write(sub.join("paused.png"), b"x").unwrap();
        std::thread::sleep(Duration::from_millis(3000));
        assert!(seen.lock().unwrap().is_empty(), "paused watchers report nothing");
        paused.store(false, Ordering::SeqCst);
        let later = sub.join("later.png");
        std::fs::write(&later, b"x").unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        while seen.lock().unwrap().is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(seen.lock().unwrap().clone(), vec![later], "subfolder file after the pause");
        // Stopping returns quickly and later files are not reported.
        let t = Instant::now();
        drop(set);
        assert!(t.elapsed() < Duration::from_secs(3));
        std::fs::write(dir.join("after.png"), b"x").unwrap();
        std::thread::sleep(Duration::from_millis(2800));
        assert_eq!(seen.lock().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
