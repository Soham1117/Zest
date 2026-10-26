//! Zest: radial drag-and-drop file converter. Tray app and CLI.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod cli;
mod collect;
mod console;
mod headless;
mod history;
mod io;
mod ipc;
mod logging;
mod paths;
mod request;
mod state;
mod tray;
mod watcher;
mod watchfolders;

use clap::Parser;
use cli::{Action, Cli};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

fn read_list(path: &std::path::Path) -> Result<Vec<PathBuf>, String> {
    std::fs::read_to_string(path)
        .map(|t| cli::parse_list(&t))
        .map_err(|e| format!("cannot read {}: {e}", path.display()))
}

fn absolute(p: PathBuf) -> PathBuf {
    std::path::absolute(&p).unwrap_or(p)
}

fn main() -> ExitCode {
    // Output of the CLI (and `--version`, usage errors) needs the parent console.
    if std::env::args_os().len() > 1 {
        console::attach_parent_console();
    }
    let cli = Cli::parse(); // usage errors exit with code 2
    let action = match cli.into_action(read_list) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(headless::EXIT_USAGE as u8);
        }
    };

    // SAFETY: process-wide setting; the manifest already asks for this, so a
    // failure here only means it was set.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };

    let code = match action {
        Action::Convert(spec) => {
            logging::init_cli();
            headless::convert(*spec)
        }
        Action::Tool(spec) => {
            logging::init_cli();
            headless::tool(*spec)
        }
        Action::Zip(spec) => {
            logging::init_cli();
            headless::zip(*spec)
        }
        Action::Formats => {
            logging::init_cli();
            headless::formats()
        }
        Action::Install => headless::install(true),
        Action::Uninstall => headless::install(false),
        Action::Tray { paths } => run_tray(paths),
    };
    ExitCode::from(code as u8)
}

fn run_tray(paths: Vec<PathBuf>) -> i32 {
    let paths: Vec<PathBuf> = paths.into_iter().map(absolute).collect();
    let sid = match ipc::user_sid() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e:#}");
            return headless::EXIT_FAILED;
        }
    };
    let _guard = match ipc::acquire_instance(&sid) {
        Ok(Some(g)) => g,
        Ok(None) => {
            // Another instance runs: hand the files over and exit.
            if paths.is_empty() {
                return headless::EXIT_OK;
            }
            return match ipc::forward(&sid, &paths, Duration::from_secs(5)) {
                Ok(()) => headless::EXIT_OK,
                Err(e) => {
                    eprintln!("error: {e:#}");
                    headless::EXIT_FAILED
                }
            };
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            return headless::EXIT_FAILED;
        }
    };

    logging::init_file();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "Zest starting");
    let code = match app::run(&sid, paths) {
        Ok(()) => headless::EXIT_OK,
        Err(e) => {
            tracing::error!("fatal: {e:#}");
            eprintln!("error: {e:#}");
            headless::EXIT_FAILED
        }
    };
    logging::shutdown();
    code
}
