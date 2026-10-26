//! Commands without UI: `convert`, `tool`, `zip`, `formats`, `--install`, `--uninstall`.
//! Exit codes: 0 all ok, 1 some failed, 2 usage error.

use crate::cli::{ConvertSpec, ToolSpec, ZipSpec};
use crate::paths;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zest_broker::{Broker, BrokerConfig, BrokerEvent, ToolOverrides, ToolPaths};
use zest_core::{
    ActionKind, Config, EngineId, FormatGraph, FormatId, JobId, JobRequest, JobState, Operation, Quality,
};

pub const EXIT_OK: i32 = 0;
pub const EXIT_FAILED: i32 = 1;
pub const EXIT_USAGE: i32 = 2;

const MISSING_TOOLS: &str = "No conversion engine found. Run tools\\fetch-deps.ps1.";

fn load_config() -> Config {
    match Config::load(&paths::config_path()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("warning: {e}; using default settings");
            Config::default()
        }
    }
}

fn probe(cfg: &Config) -> (ToolPaths, Vec<EngineId>) {
    let tools = ToolPaths::resolve(&ToolOverrides::from(&cfg.tools));
    let engines = tools.probe();
    (tools, engines)
}

/// `zest formats`
pub fn formats() -> i32 {
    let (_, engines) = probe(&load_config());
    let names: Vec<&str> = engines.iter().map(|e| e.name()).collect();
    if names.is_empty() {
        println!("Engines: none found");
    } else {
        println!("Engines: {}", names.join(", "));
    }
    let graph = FormatGraph::with_available(&engines);
    for from in FormatId::ALL {
        let targets = graph.targets(from);
        if targets.is_empty() {
            continue;
        }
        let list: Vec<&str> = targets.iter().map(|e| e.to.extension()).collect();
        println!("{:<5} -> {}", from.extension(), list.join(", "));
    }
    if engines.is_empty() {
        eprintln!("{MISSING_TOOLS}");
        return EXIT_FAILED;
    }
    EXIT_OK
}

/// `zest convert ...`
pub fn convert(spec: ConvertSpec) -> i32 {
    let Some(target) = FormatId::from_extension(&spec.to) else {
        eprintln!("error: unknown format \"{}\"", spec.to);
        return EXIT_USAGE;
    };
    run_job(Operation::Convert(target), spec.quality, spec.out, spec.files)
}

/// `zest tool <tool> ...`
pub fn tool(spec: ToolSpec) -> i32 {
    run_job(Operation::Tool(spec.tool), Quality::High, spec.out, spec.files)
}

/// `zest zip ...`
pub fn zip(spec: ZipSpec) -> i32 {
    run_job(Operation::Action(ActionKind::Zip), Quality::High, spec.out, spec.items)
}

/// Run one job with the broker and print the result. Exit code 0 if all files
/// went well, 1 if some failed.
fn run_job(op: Operation, quality: Quality, out: Option<PathBuf>, inputs: Vec<PathBuf>) -> i32 {
    let zip = op == Operation::Action(ActionKind::Zip);
    // Zip and Merge PDFs make one output from all files.
    let batch = zip || matches!(op, Operation::Tool(t) if t.is_batch());
    let cfg = load_config();
    let (tools, engines) = probe(&cfg);
    // Zip is file work of the broker itself: it does not need an engine.
    if engines.is_empty() && !zip {
        eprintln!("{MISSING_TOOLS}");
        return EXIT_FAILED;
    }
    let out_dir: Option<PathBuf> = out.or_else(|| cfg.output.location.out_dir().map(Path::to_path_buf));
    if let Some(dir) = &out_dir {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("error: cannot create output folder {}: {e}", dir.display());
            return EXIT_FAILED;
        }
    }

    let mut failed = 0usize;
    let mut files = Vec::new();
    for f in &inputs {
        // Zip takes folders too.
        if f.is_file() || (zip && f.is_dir()) {
            files.push(f.clone());
        } else {
            eprintln!("FAIL {}: file not found", f.display());
            failed += 1;
        }
    }
    if files.is_empty() {
        return EXIT_FAILED;
    }

    let graph = Arc::new(FormatGraph::with_available(&engines));
    let (broker, rx) = Broker::new(BrokerConfig::from_config(tools, &cfg), graph);
    let req = JobRequest {
        id: JobId(1),
        files: files.clone(),
        op,
        quality,
        out_dir,
        page_range: None,
    };
    broker.submit(req);

    let tty = std::io::stderr().is_terminal();
    let mut ok = 0usize;
    let mut cancelled = false;
    let src_of = |i: usize| -> String {
        if batch {
            format!("{} item{}", files.len(), if files.len() == 1 { "" } else { "s" })
        } else {
            files.get(i).map(|p| p.display().to_string()).unwrap_or_default()
        }
    };
    for ev in rx {
        match ev {
            BrokerEvent::FileDone { file_index, output, note, .. } => {
                clear_progress(tty);
                let src = src_of(file_index);
                match note {
                    Some(n) => println!("ok   {src} -> {} ({n})", output.display()),
                    None => println!("ok   {src} -> {}", output.display()),
                }
                ok += 1;
            }
            BrokerEvent::Job(ev) => match ev.state {
                JobState::Failed { error } => {
                    clear_progress(tty);
                    eprintln!("FAIL {}: {error}", src_of(ev.file_index));
                    failed += 1;
                }
                JobState::Cancelled => {
                    clear_progress(tty);
                    eprintln!("CANCELLED {}", src_of(ev.file_index));
                }
                _ => {}
            },
            BrokerEvent::Progress { percent: Some(p), .. } if tty => {
                eprint!("\r  {p:>3.0}%");
                let _ = std::io::stderr().flush();
            }
            BrokerEvent::Progress { .. } => {}
            BrokerEvent::JobDone { cancelled: c, .. } => {
                cancelled = c;
                break;
            }
        }
    }
    clear_progress(tty);
    println!("{ok} {}, {failed} failed", op.past_tense().to_lowercase());
    if failed == 0 && !cancelled {
        EXIT_OK
    } else {
        EXIT_FAILED
    }
}

fn clear_progress(tty: bool) {
    if tty {
        eprint!("\r      \r");
    }
}

/// `zest --install` / `zest --uninstall`
pub fn install(enable: bool) -> i32 {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error: cannot find the program path: {e}");
            return EXIT_FAILED;
        }
    };
    let mut bad = 0;
    let mut step = |name: &str, r: zest_shell::Result<()>| match r {
        Ok(()) => println!("ok   {name}"),
        Err(e) => {
            eprintln!("FAIL {name}: {e}");
            bad += 1;
        }
    };
    if enable {
        step("right-click menu", zest_shell::install_context_menu(&exe));
        step("Send To shortcut", zest_shell::install_send_to(&exe));
        step("start at sign-in", zest_shell::set_autostart(&exe, true));
    } else {
        step("right-click menu removed", zest_shell::uninstall_context_menu());
        step("Send To shortcut removed", zest_shell::uninstall_send_to());
        step("start at sign-in removed", zest_shell::set_autostart(&exe, false));
    }
    if bad == 0 {
        EXIT_OK
    } else {
        EXIT_FAILED
    }
}
