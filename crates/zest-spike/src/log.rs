//! Timestamped logging.
//!
//! Every line starts with the time in ms (3 decimals) since process start,
//! measured with `QueryPerformanceCounter`. Lines go to stdout AND to
//! `%LOCALAPPDATA%\Zest\spike.log` through a background thread, so the UI
//! thread only does a channel send.

use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;

use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

struct Clock {
    start: i64,
    freq: i64,
}

enum Msg {
    Line(String),
    Quit,
}

static CLOCK: OnceLock<Clock> = OnceLock::new();
static SENDER: OnceLock<Sender<Msg>> = OnceLock::new();
static WRITER: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Log an event: `zlog!("name")` or `zlog!("name", "key={} other={}", a, b)`.
macro_rules! zlog {
    ($name:expr) => {
        $crate::log::event($name, "")
    };
    ($name:expr, $($arg:tt)*) => {
        $crate::log::event($name, &format!($($arg)*))
    };
}

/// Raw QPC counter value.
pub fn qpc_now() -> i64 {
    let mut v = 0i64;
    // SAFETY: plain out-pointer to a local.
    let _ = unsafe { QueryPerformanceCounter(&mut v) };
    v
}

/// Milliseconds between two QPC values.
pub fn ms_between(from: i64, to: i64) -> f64 {
    let freq = CLOCK.get().map_or(1, |c| c.freq).max(1);
    (to - from) as f64 * 1000.0 / freq as f64
}

/// Milliseconds since `from` (a QPC value).
pub fn ms_since(from: i64) -> f64 {
    ms_between(from, qpc_now())
}

fn now_ms() -> f64 {
    match CLOCK.get() {
        Some(c) => (qpc_now() - c.start) as f64 * 1000.0 / c.freq.max(1) as f64,
        None => 0.0,
    }
}

/// Path of the log file, once `init` has run.
pub fn log_path() -> Option<PathBuf> {
    LOG_PATH.get().cloned()
}

/// Start the clock and the writer thread. Call once, first thing in `main`.
pub fn init() {
    let mut freq = 0i64;
    // SAFETY: plain out-pointer to a local.
    let _ = unsafe { QueryPerformanceFrequency(&mut freq) };
    let _ = CLOCK.set(Clock {
        start: qpc_now(),
        freq: freq.max(1),
    });

    let dir = std::env::var_os("LOCALAPPDATA")
        .map(|p| PathBuf::from(p).join("Zest"))
        .unwrap_or_else(|| PathBuf::from("."));
    let _ = fs::create_dir_all(&dir);
    let path = dir.join("spike.log");
    let _ = LOG_PATH.set(path.clone());
    let file = File::options().create(true).append(true).open(&path).ok();

    let (tx, rx) = mpsc::channel::<Msg>();
    let handle = std::thread::Builder::new()
        .name("zest-log".into())
        .spawn(move || {
            let mut file = file;
            while let Ok(msg) = rx.recv() {
                match msg {
                    Msg::Line(line) => {
                        println!("{line}");
                        if let Some(f) = file.as_mut() {
                            let _ = writeln!(f, "{line}");
                        }
                    }
                    Msg::Quit => break,
                }
            }
            if let Some(f) = file.as_mut() {
                let _ = f.flush();
            }
        })
        .ok();
    let _ = SENDER.set(tx);
    if let Ok(mut w) = WRITER.lock() {
        *w = handle;
    }
    event("log_start", &format!("path={}", path.display()));
}

/// Queue one log line. Never blocks on I/O.
pub fn event(name: &str, fields: &str) {
    let t = now_ms();
    let line = if fields.is_empty() {
        format!("[{t:11.3}] {name}")
    } else {
        format!("[{t:11.3}] {name} {fields}")
    };
    if let Some(tx) = SENDER.get() {
        let _ = tx.send(Msg::Line(line));
    }
}

/// Flush everything and stop the writer thread.
pub fn shutdown() {
    event("log_stop", "");
    if let Some(tx) = SENDER.get() {
        let _ = tx.send(Msg::Quit);
    }
    let handle = WRITER.lock().ok().and_then(|mut w| w.take());
    if let Some(h) = handle {
        let _ = h.join();
    }
}
