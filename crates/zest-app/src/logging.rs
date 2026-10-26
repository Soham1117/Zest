//! Logging: non-blocking daily file in `%LOCALAPPDATA%\Zest\logs\` (7 files),
//! level from `ZEST_LOG` (default `info`). A panic hook logs panics.

use std::sync::Mutex;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::EnvFilter;

static GUARD: Mutex<Option<WorkerGuard>> = Mutex::new(None);

fn filter() -> EnvFilter {
    EnvFilter::try_from_env("ZEST_LOG").unwrap_or_else(|_| EnvFilter::new("info"))
}

/// Log to the daily file. Keep the process alive until `shutdown`.
pub fn init_file() {
    let dir = crate::paths::logs_dir();
    let _ = std::fs::create_dir_all(&dir);
    let appender = Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix("zest")
        .filename_suffix("log")
        .max_log_files(7)
        .build(&dir);
    let Ok(appender) = appender else {
        eprintln!("zest: cannot open the log folder {}", dir.display());
        return;
    };
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let ok = tracing_subscriber::fmt()
        .with_env_filter(filter())
        .with_writer(writer)
        .with_ansi(false)
        .with_target(false)
        .try_init()
        .is_ok();
    if ok {
        if let Ok(mut g) = GUARD.lock() {
            *g = Some(guard);
        }
    }
    install_panic_hook();
}

/// CLI mode: no log file. Only when `ZEST_LOG` is set, log to stderr.
pub fn init_cli() {
    if std::env::var_os("ZEST_LOG").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter())
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .try_init();
    }
}

fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let loc = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown".to_owned());
        tracing::error!(location = %loc, "panic: {msg}");
        default(info);
    }));
}

/// Flush the log. Call before the process ends.
pub fn shutdown() {
    if let Ok(mut g) = GUARD.lock() {
        g.take(); // dropping the guard flushes the buffer
    }
}
