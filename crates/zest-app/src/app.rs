//! The tray app: window, message loop and wiring of all parts.
//!
//! Everything in `App` lives on the UI thread. Other threads (broker reader,
//! pipe server, config watcher, engine probe) send `UiEvent`s through a
//! channel and wake the UI thread with one posted message.

use crate::collect::Collector;
use crate::history::{self, HistoryRecord};
use crate::io::{self, Io};
use crate::request::{self, build_request, format_of};
use crate::state::State;
use crate::tray::{self, cmd, MenuState};
use crate::watchfolders::{Produced, WatchHit, WatchSet};
use crate::{ipc, paths, watcher};
use anyhow::{Context, Result};
use crossbeam_channel::{unbounded, Receiver, Sender};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use windows::core::w;
use windows::Win32::Foundation::{GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
    KillTimer, PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SetTimer,
    TranslateMessage, MSG, WINDOW_EX_STYLE, WM_APP, WM_COMMAND, WM_DESTROY, WM_TIMER, WNDCLASSW,
    WS_POPUP,
};
use zest_broker::{Broker, BrokerConfig, BrokerEvent, ToolOverrides, ToolPaths};
use zest_core::{
    ActionKind, Category, Config, EngineId, FormatGraph, FormatId, JobId, Operation, WheelModel,
    WheelPrefs,
};
use zest_input::{InputEvent, InputHost};
use zest_overlay::{Callbacks, Outcome, Overlay, Pick};

mod panels;
mod tooldlg;

pub const WM_UI: u32 = WM_APP + 2;
const TIMER_COLLECT: usize = 1;
const TIMER_HANDOFF: usize = 2;
/// The progress ring gives the job to the tray balloon after this time.
pub const HANDOFF_MS: u32 = 5000;

// ---------------------------------------------------------------------------
// Events from other threads
// ---------------------------------------------------------------------------

pub enum UiEvent {
    Broker(BrokerEvent),
    Engines { tools: ToolPaths, engines: Vec<EngineId> },
    Config(Result<Config, String>),
    Paths(Vec<PathBuf>),
    /// A message from an action thread (balloon).
    Notice { text: String, error: bool },
    /// The folder that "Copy to" / "Move to" chose (remembered in state.json).
    LastFolder(PathBuf),
    /// Jobs read from history.jsonl for the History window.
    HistoryLoaded(Vec<zest_panels::JobEntry>),
    /// Engine versions for the Settings window.
    EngineVersions(Vec<(String, String)>),
    /// Debug run (`ZEST_DEBUG_OPEN_PANELS`): close the panels again.
    DebugClosePanels,
    /// A file in a watch folder is complete.
    WatchReady(WatchHit),
    /// Progress of the Pandoc download (shown in the Settings window).
    EngineDownload { percent: Option<f32>, text: &'static str },
    /// The download ended.
    EngineDownloadDone(Result<(), String>),
    /// The data for the Trim or Crop window is ready.
    ToolPrepared(Box<tooldlg::ToolPrep>),
    /// Wheel hover preview text (token must match).
    HoverPreview { token: u64, text: Option<String> },
}

/// Sends events to the UI thread. One `WM_UI` message is pending at most.
#[derive(Clone)]
pub struct UiSender {
    tx: Sender<UiEvent>,
    hwnd: isize,
    pending: Arc<AtomicBool>,
}

impl UiSender {
    pub fn send(&self, ev: UiEvent) {
        if self.tx.send(ev).is_err() {
            return;
        }
        if !self.pending.swap(true, Ordering::SeqCst) {
            // SAFETY: PostMessageW is thread safe; a dead window only makes it fail.
            unsafe {
                let _ = PostMessageW(HWND(self.hwnd as *mut c_void), WM_UI, WPARAM(0), LPARAM(0));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

struct ActiveJob {
    inputs: Vec<PathBuf>,
    op: Operation,
    started: Instant,
    /// The ring no longer shows this job: the balloon reports the result.
    handed_off: bool,
    broker_gen: u64,
    /// A watch folder job: only a failure shows a balloon.
    silent: bool,
}

type BrokerKey = (usize, usize, bool, bool);

fn broker_key(cfg: &Config) -> BrokerKey {
    let n = std::thread::available_parallelism().map_or(4, |n| n.get());
    (
        cfg.jobs.video_workers(n).max(1),
        cfg.jobs.image_workers(n).max(1),
        cfg.output.privacy_mode,
        cfg.output.keep_timestamps,
    )
}

pub struct App {
    hwnd: Cell<HWND>,
    ui: UiSender,
    ui_rx: Receiver<UiEvent>,
    io: Io,
    overlay: OnceCell<Overlay>,
    input: OnceCell<InputHost>,
    cfg: RefCell<Config>,
    state: RefCell<State>,
    graph: RefCell<Arc<FormatGraph>>,
    tools: RefCell<Option<ToolPaths>>,
    engines_ready: Cell<bool>,
    pending_open: RefCell<Vec<PathBuf>>,
    collector: RefCell<Collector>,
    brokers: RefCell<HashMap<u64, Broker>>,
    current_broker: Cell<Option<u64>>,
    broker_key: Cell<BrokerKey>,
    next_broker_gen: Cell<u64>,
    jobs: RefCell<HashMap<JobId, ActiveJob>>,
    ring_job: Cell<Option<JobId>>,
    next_job: Cell<u64>,
    paused: Cell<bool>,
    last_output: RefCell<Option<PathBuf>>,
    balloon_target: RefCell<Option<PathBuf>>,
    taskbar_created: u32,
    ui_panels: panels::PanelsState,
    /// Running watch folder watchers.
    watch: RefCell<Option<WatchSet>>,
    watch_paused: Arc<AtomicBool>,
    /// Files that Zest made itself (watchers ignore them).
    produced: Produced,
    /// Files that were ready before the engines were known.
    pending_watch: RefCell<Vec<WatchHit>>,
    engine_download_running: Cell<bool>,
    /// The Trim or Crop window is open (or being prepared).
    tool_dialog: Cell<bool>,
}

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

/// Run a closure; a panic is logged and does not unwind into Windows code.
fn guard<R>(what: &str, f: impl FnOnce() -> R) -> Option<R> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => Some(r),
        Err(_) => {
            tracing::error!("panic caught in {what}");
            None
        }
    }
}

fn cursor() -> (i32, i32) {
    let mut p = POINT::default();
    // SAFETY: out-pointer to a local POINT.
    let _ = unsafe { GetCursorPos(&mut p) };
    (p.x, p.y)
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

/// Text of the balloon for a finished job.
/// `output` is the first created file (the Zip message names it).
pub fn job_summary(op: Operation, ok: usize, errors: &[(PathBuf, String)], output: Option<&Path>) -> String {
    if let Operation::Action(ActionKind::CopyAs(_)) = op {
        return match errors.first() {
            Some((_, first)) => format!("Copy failed: {}", truncate(first, 160)),
            None => format!("Copied {ok} image{}", plural(ok)),
        };
    }
    if op == Operation::Action(ActionKind::Zip) {
        return match (errors.first(), output) {
            (Some((_, first)), _) => format!("Zip failed: {}", truncate(first, 160)),
            (None, Some(p)) => format!("Created {}", name_of(p)),
            (None, None) => "Zip file created".to_owned(),
        };
    }
    let verb = op.past_tense();
    match errors.first() {
        None => format!("{verb} {ok} file{}", plural(ok)),
        Some((_, first)) if ok == 0 => {
            format!("{} failed: {}", errors.len(), truncate(first, 160))
        }
        Some((_, first)) => format!(
            "{ok} {}, {} failed: {}",
            verb.to_lowercase(),
            errors.len(),
            truncate(first, 160)
        ),
    }
}

impl App {
    fn overlay(&self) -> Option<&Overlay> {
        self.overlay.get()
    }

    fn prefs(&self) -> WheelPrefs {
        WheelPrefs {
            last_used: self.state.borrow().typed(),
            favorites: self.cfg.borrow().wheel.favorites_typed(),
        }
    }

    fn notify(&self, title: &str, text: &str, error: bool, target: Option<PathBuf>) {
        // Quiet mode: suppress success balloons; errors always notify.
        if !error && self.cfg.borrow().ui.quiet {
            return;
        }
        *self.balloon_target.borrow_mut() = target;
        self.ui_panels.set_balloon_error(error);
        tray::balloon(self.hwnd.get(), title, text, error);
    }

    // ----- input events -----------------------------------------------------

    fn on_input(&self, ev: InputEvent) {
        match ev {
            InputEvent::DragStarted { x, y, .. } => {
                self.handoff_ring();
                if let Some(o) = self.overlay() {
                    o.show_for_drag(x, y);
                }
            }
            InputEvent::DragEnded { .. } => {
                if let Some(o) = self.overlay() {
                    o.on_drag_ended();
                }
            }
            InputEvent::HoldTriggered { x, y } => self.open_from_selection(x, y, false),
            InputEvent::KeyChord { x, y } => self.open_from_selection(x, y, true),
            InputEvent::Undo => self.undo_last_job(),
        }
    }

    fn undo_last_job(&self) {
        if self.overlay().is_some_and(|o| o.blocks_undo()) {
            return;
        }
        match self.panels().map(|p| p.history_undo_last()).unwrap_or(Ok(0)) {
            Ok(n) if n > 0 => {
                self.notify("Zest", &format!("Undid last job ({n} file{} to Recycle Bin).", if n == 1 { "" } else { "s" }), false, None);
            }
            Ok(_) => self.notify("Zest", "Nothing to undo.", false, None),
            Err(e) => self.notify("Zest", &format!("Undo failed: {e}"), true, None),
        }
    }

    fn convert_clipboard(&self) {
        let tmp = paths::logs_dir().parent().unwrap_or(Path::new(".")).join("tmp").join("clipboard");
        match zest_shell::read_clipboard_source(self.hwnd.get(), &tmp) {
            Ok(zest_shell::ClipboardSource::Files(paths)) => {
                let (x, y) = cursor();
                if !self.open_wheel(x, y, paths) {
                    self.notify("Zest", "There is nothing to do with these clipboard items.", false, None);
                }
            }
            Ok(zest_shell::ClipboardSource::Image(path)) => {
                let (x, y) = cursor();
                if !self.open_wheel(x, y, vec![path]) {
                    self.notify("Zest", "Cannot convert the clipboard image.", true, None);
                }
            }
            Err(e) => self.notify("Zest", &format!("Clipboard: {e}"), true, None),
        }
    }

    fn toggle_win11_menu(&self) {
        let want = !self.cfg.borrow().ui.win11_top_level_menu;
        self.cfg.borrow_mut().ui.win11_top_level_menu = want;
        let script = if want {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("tools").join("register-win11-menu.ps1")
        } else {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("tools").join("unregister-win11-menu.ps1")
        };
        // Installed builds: look next to the exe's tools folder, then next to source layout.
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let candidates = [
            exe_dir.as_ref().map(|d| d.join("tools").join(script.file_name().unwrap())),
            Some(script),
        ];
        let Some(path) = candidates.into_iter().flatten().find(|p| p.is_file()) else {
            self.notify(
                "Zest",
                if want {
                    "Run tools\\register-win11-menu.ps1 (admin once) for the top-level menu."
                } else {
                    "Run tools\\unregister-win11-menu.ps1 to remove the top-level menu."
                },
                false,
                None,
            );
            return;
        };
        let arg = format!(
            "-ExecutionPolicy Bypass -File \"{}\"",
            path.to_string_lossy()
        );
        let wide_arg: Vec<u16> = arg.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: NUL-terminated; ShellExecuteW with runas for elevation when registering.
        let verb = if want { windows::core::w!("runas") } else { windows::core::w!("open") };
        let r = unsafe {
            windows::Win32::UI::Shell::ShellExecuteW(
                None,
                verb,
                windows::core::w!("powershell.exe"),
                windows::core::PCWSTR(wide_arg.as_ptr()),
                None,
                windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
            )
        };
        if (r.0 as isize) <= 32 {
            self.notify("Zest", "Could not start the Win11 menu script.", true, None);
        } else {
            self.notify(
                "Zest",
                if want {
                    "Top-level menu: follow the PowerShell window (admin may be required once)."
                } else {
                    "Unregistering the top-level Win11 menu…"
                },
                false,
                None,
            );
        }
    }

    fn on_hover_preview(&self, req: Option<zest_overlay::HoverPreviewReq>) {
        let Some(req) = req else {
            if let Some(o) = self.overlay() {
                // Clear is handled by overlay token bump; nothing to do.
                let _ = o;
            }
            return;
        };
        let Some(tools) = self.tools.borrow().clone() else { return };
        let ui = self.ui.clone();
        let token = req.token;
        let file = req.files.first().cloned();
        let Some(file) = file else { return };
        let label = req.item.label.to_string();
        let cat = match req.item.op {
            Operation::Convert(f) | Operation::Action(zest_core::ActionKind::CopyAs(f)) => f.category(),
            Operation::Tool(_) => {
                // Infer from first file extension.
                file.extension()
                    .and_then(|e| zest_core::FormatId::from_extension(&e.to_string_lossy()))
                    .map(|f| f.category())
                    .unwrap_or(zest_core::Category::Image)
            }
            Operation::Action(_) => {
                ui.send(UiEvent::HoverPreview { token, text: Some(label) });
                return;
            }
        };
        std::thread::Builder::new()
            .name("zest-hover-preview".into())
            .spawn(move || {
                let text = match cat {
                    zest_core::Category::Image => {
                        // Prefer a short label; thumbnail path is written for future bitmap chip.
                        let dir = std::env::temp_dir().join("Zest").join("hover");
                        match zest_broker::preview::hover_thumbnail(&tools.vips, &file, &dir) {
                            Ok(_png) => Some(format!("{label} · preview")),
                            Err(_) => Some(label),
                        }
                    }
                    zest_core::Category::Video | zest_core::Category::Audio => {
                        zest_broker::preview::media_hover_text(&tools.ffprobe, &file)
                            .map(|m| format!("{label} · {m}"))
                            .or(Some(label))
                    }
                    _ => Some(label),
                };
                ui.send(UiEvent::HoverPreview { token, text });
            })
            .ok();
    }

    fn open_from_selection(&self, x: i32, y: i32, chord: bool) {
        let paths: Vec<PathBuf> = match zest_shell::foreground_selection() {
            Ok(sel) => sel.paths().to_vec(),
            Err(e) => {
                tracing::warn!("selection: {e}");
                Vec::new()
            }
        };
        tracing::info!(chord, files = paths.len(), "selection read");
        if !paths.is_empty() {
            if !self.engines_ready.get() {
                self.notify("Zest", "Zest is still starting. Try again in a moment.", false, None);
            } else if !self.open_wheel(x, y, paths) {
                self.notify("Zest", "There is nothing to do with the selected items.", false, None);
            }
            return;
        }
        if chord {
            self.handoff_ring();
            if let Some(o) = self.overlay() {
                o.show_drop_zone(x, y);
            }
        }
    }

    /// Open the wheel in click mode. False if no conversion exists for the files.
    fn open_wheel(&self, x: i32, y: i32, files: Vec<PathBuf>) -> bool {
        let Some(overlay) = self.overlay() else { return false };
        let graph = self.graph.borrow().clone();
        let prefs = self.prefs();
        let Some((model, count)) = request::model_for(&graph, &files, &prefs) else {
            return false;
        };
        self.handoff_ring();
        overlay.show_for_files(x, y, files, model, count);
        true
    }

    // ----- overlay callbacks --------------------------------------------------

    fn on_files_entered(&self, files: &[PathBuf]) -> Option<(WheelModel, usize)> {
        let graph = self.graph.borrow().clone();
        let prefs = self.prefs();
        request::model_for(&graph, files, &prefs)
    }

    fn on_pick(&self, pick: Pick) {
        // Copy path, Copy files, Copy to, Move to, Terminal: the app does them itself.
        if let Operation::Action(action) = pick.item.op {
            if pick.item.op.is_shell_action() {
                self.run_action(action, pick.files);
                return;
            }
        }
        // Trim and Crop ask for their settings in a window first.
        if let Operation::Tool(tool) = pick.item.op {
            if tool.needs_dialog() {
                self.begin_tool_dialog(tool, &pick);
                return;
            }
        }
        // PDF → image: optional page range (worker `--pages`).
        let page_range = if let Operation::Convert(target) = pick.item.op {
            let pdf_to_image = target.category() == Category::Image
                && pick.files.iter().any(|p| format_of(p) == Some(FormatId::Pdf));
            if pdf_to_image {
                match tooldlg::ask_page_range() {
                    tooldlg::PageRangeAsk::Cancelled => return,
                    tooldlg::PageRangeAsk::Invalid => {
                        self.notify(
                            "Zest",
                            "Use pages like 1-3 or 1,3,5 (or all).",
                            true,
                            None,
                        );
                        return;
                    }
                    tooldlg::PageRangeAsk::All => None,
                    tooldlg::PageRangeAsk::Spec(s) => Some(s),
                }
            } else {
                None
            }
        } else {
            None
        };
        let graph = self.graph.borrow().clone();
        let (out_dir, quality) = {
            let cfg = self.cfg.borrow();
            (cfg.output.location.out_dir().map(Path::to_path_buf), cfg.output.quality)
        };
        let id = JobId(self.next_job.get());
        self.next_job.set(id.0 + 1);
        let Some(mut req) = build_request(id, &pick, &graph, out_dir.as_deref(), quality) else {
            tracing::warn!("pick without usable files");
            self.notify("Zest", "None of the items work with that choice.", true, None);
            return;
        };
        req.page_range = page_range;
        let Some(gen) = self.ensure_broker() else {
            tracing::error!("pick before the converter is ready");
            self.notify("Zest", "The converter is not ready yet.", true, None);
            return;
        };

        // Remember the choice per input format (disk write on the I/O thread).
        if let Some(target) = req.target() {
            let mut st = self.state.borrow_mut();
            for f in req.files.iter().filter_map(|p| format_of(p)) {
                st.set(f, target);
            }
            self.io.state(st.to_json());
        }

        // A new pick while a ring shows: the old job goes to the balloon.
        self.handoff_ring();
        self.ring_job.set(Some(id));
        // SAFETY: timer on our own window.
        unsafe {
            SetTimer(self.hwnd.get(), TIMER_HANDOFF, HANDOFF_MS, None);
        }

        let key = req.op.key();
        tracing::info!(job = id.0, files = req.files.len(), op = %key, "job submitted");
        self.io.history(HistoryRecord::started(id.0, history::utc_now(), &req.files, &key).to_line());
        self.panels_job_started(id, &key, &req.files);
        self.jobs.borrow_mut().insert(
            id,
            ActiveJob {
                inputs: req.files.clone(),
                op: req.op,
                started: Instant::now(),
                handed_off: false,
                broker_gen: gen,
                silent: false,
            },
        );
        if let Some(b) = self.brokers.borrow().get(&gen) {
            b.submit(req);
        }
    }

    // ----- actions ------------------------------------------------------------

    /// Run an action that is not a broker job. Failures show a balloon;
    /// success is silent. Slow or blocking work runs on its own thread.
    fn run_action(&self, action: ActionKind, files: Vec<PathBuf>) {
        tracing::info!(action = action.slug(), items = files.len(), "action");
        let hwnd = self.hwnd.get();
        let fail = |what: &str, e: &dyn std::fmt::Display| {
            tracing::warn!("{what}: {e}");
            self.notify("Zest", &format!("{what}: {e}"), true, None);
        };
        match action {
            ActionKind::CopyPath => {
                if let Err(e) = zest_shell::copy_paths_as_text(hwnd, &files) {
                    fail("Cannot copy the path", &e);
                }
            }
            ActionKind::CopyFiles => {
                if let Err(e) = zest_shell::copy_files_to_clipboard(hwnd, &files) {
                    fail("Cannot copy the files", &e);
                }
            }
            ActionKind::TerminalHere => {
                let ui = self.ui.clone();
                spawn_action("zest-terminal", move || {
                    let result = match zest_shell::terminal_dir(&files, Path::is_dir) {
                        Some(dir) => zest_shell::open_terminal(&dir).map_err(|e| e.to_string()),
                        None => Err("no folder for the terminal".to_owned()),
                    };
                    if let Err(e) = result {
                        ui.send(UiEvent::Notice { text: format!("Cannot open the terminal: {e}"), error: true });
                    }
                });
            }
            ActionKind::CopyTo | ActionKind::MoveTo => {
                let start = self.state.borrow().last_folder();
                let ui = self.ui.clone();
                let copy = action == ActionKind::CopyTo;
                spawn_action("zest-transfer", move || transfer_files(&ui, files, start, copy));
            }
            // Hold the items on the Shelf (a floating panel).
            ActionKind::SendToShelf => self.shelf_add_files(files, true),
            // The Windows share sheet (UI thread). Folders are skipped.
            ActionKind::Share => {
                // SAFETY: our own window; the share sheet needs a foreground owner.
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(hwnd);
                }
                if let Err(e) = zest_shell::share_files(hwnd, &files) {
                    tracing::warn!("share: {e}");
                    let text = match e {
                        zest_shell::ShellError::Invalid(m) => m,
                        _ => "Share is not available".to_owned(),
                    };
                    self.notify("Zest", &text, true, None);
                }
            }
            ActionKind::ExtractHere | ActionKind::ExtractTo => {
                let zips: Vec<PathBuf> = files
                    .into_iter()
                    .filter(|p| {
                        p.extension()
                            .and_then(|e| e.to_str())
                            .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
                    })
                    .collect();
                if zips.is_empty() {
                    self.notify("Zest", "Select a .zip file to extract.", true, None);
                    return;
                }
                let pick_folder = action == ActionKind::ExtractTo;
                let ui = self.ui.clone();
                let start = self.state.borrow().last_folder();
                spawn_action("zest-extract", move || {
                    let dest_base = if pick_folder {
                        match zest_shell::pick_folder("Extract to…", start.as_deref()) {
                            Ok(Some(d)) => {
                                ui.send(UiEvent::LastFolder(d.clone()));
                                d
                            }
                            Ok(None) => return,
                            Err(e) => {
                                ui.send(UiEvent::Notice {
                                    text: format!("Cannot pick a folder: {e}"),
                                    error: true,
                                });
                                return;
                            }
                        }
                    } else {
                        PathBuf::new()
                    };
                    let cancel = std::sync::atomic::AtomicBool::new(false);
                    for zip in &zips {
                        let dest = if pick_folder {
                            dest_base.clone()
                        } else {
                            zip.parent().unwrap_or(Path::new(".")).to_path_buf()
                        };
                        let stem = zip.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "extracted".into());
                        let out = dest.join(format!("{stem}-extracted"));
                        match zest_broker::archive::extract_zip(zip, &out, &cancel) {
                            Ok(n) => ui.send(UiEvent::Notice {
                                text: format!("Extracted {n} file{} to {}", if n == 1 { "" } else { "s" }, out.display()),
                                error: false,
                            }),
                            Err(e) => ui.send(UiEvent::Notice {
                                text: format!("Extract failed: {e}"),
                                error: true,
                            }),
                        }
                    }
                });
            }
            // Zip and Copy as... are broker jobs and never get here.
            ActionKind::Zip | ActionKind::CopyAs(_) => tracing::error!("a broker job reached run_action"),
        }
    }

    fn on_progress_clicked(&self) {
        self.open_last_output();
    }

    fn open_last_output(&self) {
        let target = self.last_output.borrow().clone();
        if let Some(p) = target {
            open_folder_select_async(p);
        }
    }

    // ----- ring ---------------------------------------------------------------

    /// The progress ring stops showing its job. The balloon reports the result.
    fn handoff_ring(&self) {
        if let Some(id) = self.ring_job.take() {
            // SAFETY: timer on our own window.
            unsafe {
                let _ = KillTimer(self.hwnd.get(), TIMER_HANDOFF);
            }
            if let Some(j) = self.jobs.borrow_mut().get_mut(&id) {
                j.handed_off = true;
            }
            if let Some(o) = self.overlay() {
                o.hand_off();
            }
        }
    }

    // ----- broker -------------------------------------------------------------

    fn ensure_broker(&self) -> Option<u64> {
        if let Some(g) = self.current_broker.get() {
            return Some(g);
        }
        let tools = self.tools.borrow().clone()?;
        let graph = self.graph.borrow().clone();
        let cfg = self.cfg.borrow().clone();
        self.broker_key.set(broker_key(&cfg));
        let (broker, rx) = Broker::new(BrokerConfig::from_config(tools, &cfg), graph);
        let gen = self.next_broker_gen.get();
        self.next_broker_gen.set(gen + 1);
        let ui = self.ui.clone();
        let produced = self.produced.clone();
        let spawned = std::thread::Builder::new().name("zest-events".into()).spawn(move || {
            for ev in rx {
                if let BrokerEvent::FileDone { output, .. } = &ev {
                    // Watch folders must not convert what Zest just made.
                    produced.note(output);
                    // Tell Explorer about the new file (off the UI thread).
                    zest_shell::notify_created(output);
                }
                ui.send(UiEvent::Broker(ev));
            }
        });
        if spawned.is_err() {
            tracing::error!("cannot start the broker event thread");
        }
        self.brokers.borrow_mut().insert(gen, broker);
        self.current_broker.set(Some(gen));
        Some(gen)
    }

    /// Drop brokers that are not current and have no running job (on a thread,
    /// because dropping joins the workers).
    fn reap_brokers(&self) {
        let cur = self.current_broker.get();
        let busy: HashSet<u64> = self.jobs.borrow().values().map(|j| j.broker_gen).collect();
        let dead: Vec<Broker> = {
            let mut b = self.brokers.borrow_mut();
            let gens: Vec<u64> =
                b.keys().copied().filter(|g| Some(*g) != cur && !busy.contains(g)).collect();
            gens.into_iter().filter_map(|g| b.remove(&g)).collect()
        };
        for br in dead {
            let _ = std::thread::Builder::new().name("zest-broker-drop".into()).spawn(move || drop(br));
        }
    }

    fn cancel_all(&self) {
        let jobs = self.jobs.borrow();
        let brokers = self.brokers.borrow();
        for (id, job) in jobs.iter() {
            if let Some(b) = brokers.get(&job.broker_gen) {
                b.cancel(*id);
            }
        }
        tracing::info!(count = jobs.len(), "cancel all jobs");
    }

    fn on_broker(&self, ev: BrokerEvent) {
        match ev {
            BrokerEvent::Progress { id, percent } => {
                self.panels_job_progress(id, percent);
                if self.ring_job.get() == Some(id) {
                    if let (Some(p), Some(o)) = (percent, self.overlay()) {
                        o.set_progress(p);
                    }
                }
            }
            BrokerEvent::FileDone { output, .. } => {
                *self.last_output.borrow_mut() = Some(output);
            }
            BrokerEvent::Job(_) => {}
            BrokerEvent::JobDone { id, outputs, errors, cancelled } => {
                self.finish_job(id, outputs, errors, cancelled);
            }
        }
    }

    fn finish_job(&self, id: JobId, outputs: Vec<PathBuf>, errors: Vec<(PathBuf, String)>, cancelled: bool) {
        let Some(job) = self.jobs.borrow_mut().remove(&id) else { return };
        tracing::info!(
            job = id.0,
            ok = outputs.len(),
            failed = errors.len(),
            cancelled,
            secs = job.started.elapsed().as_secs_f32(),
            "job finished"
        );
        for (p, e) in &errors {
            tracing::warn!(job = id.0, file = %name_of(p), "failed: {e}");
        }
        self.io.history(
            HistoryRecord::finished(
                id.0,
                history::utc_now(),
                &job.inputs,
                &outputs,
                &job.op.key(),
                &errors,
                cancelled,
            )
            .to_line(),
        );

        self.panels_job_finished(id, job.op, &outputs, &errors, cancelled);

        let outcome = if cancelled {
            Outcome::Cancelled
        } else if !errors.is_empty() {
            Outcome::Failed
        } else {
            Outcome::Succeeded
        };
        if self.ring_job.get() == Some(id) {
            self.ring_job.set(None);
            // SAFETY: timer on our own window.
            unsafe {
                let _ = KillTimer(self.hwnd.get(), TIMER_HANDOFF);
            }
            if let Some(o) = self.overlay() {
                o.complete(outcome);
            }
        }
        // Copy as...: the result goes to the clipboard. Success shows nothing.
        if let Operation::Action(ActionKind::CopyAs(_)) = job.op {
            if !cancelled && errors.is_empty() {
                self.put_on_clipboard(&outputs);
                self.reap_brokers();
                return;
            }
        }
        // Errors always show a balloon. Success shows one only when the ring is gone.
        if !errors.is_empty() {
            let text = job_summary(job.op, outputs.len(), &errors, outputs.first().map(PathBuf::as_path));
            self.notify("Zest", &text, true, outputs.first().cloned());
        } else if job.handed_off && !cancelled && !job.silent {
            let text = job_summary(job.op, outputs.len(), &errors, outputs.first().map(PathBuf::as_path));
            self.notify("Zest", &text, false, outputs.first().cloned());
        }
        self.reap_brokers();
    }

    /// Copy as...: files for every output, plus the pixels when it is one image.
    /// The decode of one image runs on the UI thread (a few ms for a photo).
    fn put_on_clipboard(&self, outputs: &[PathBuf]) {
        let image = match outputs {
            [one] => Some(one.as_path()),
            _ => None,
        };
        if let Err(e) = zest_shell::copy_files_and_image(self.hwnd.get(), outputs, image) {
            tracing::warn!("clipboard: {e}");
            self.notify("Zest", &format!("Copy failed: {e}"), true, None);
        }
    }

    // ----- engines, config, paths ---------------------------------------------

    fn on_engines(&self, tools: ToolPaths, engines: Vec<EngineId>) {
        let names: Vec<&str> = engines.iter().map(|e| e.name()).collect();
        tracing::info!("engines found: {}", if names.is_empty() { "none".to_owned() } else { names.join(", ") });
        *self.graph.borrow_mut() = Arc::new(FormatGraph::with_available(&engines));
        self.engines_probed(&tools, &engines);
        *self.tools.borrow_mut() = Some(tools);
        self.current_broker.set(None);
        self.engines_ready.set(true);
        self.push_engine_lines();

        // LibreOffice and Pandoc are optional: no message when they are missing.
        let missing: Vec<&str> = [EngineId::Vips, EngineId::Ffmpeg, EngineId::Pdfium]
            .into_iter()
            .filter(|e| !engines.contains(e))
            .map(EngineId::name)
            .collect();
        if !missing.is_empty() {
            let what = missing.join(" and ");
            self.notify("Zest", &format!("{what} not found — run tools\\fetch-deps.ps1"), true, None);
        }

        let pending = std::mem::take(&mut *self.pending_open.borrow_mut());
        if !pending.is_empty() {
            self.open_paths(pending);
        }
        for hit in std::mem::take(&mut *self.pending_watch.borrow_mut()) {
            self.on_watch_ready(hit);
        }
    }

    // ----- watch folders --------------------------------------------------------

    /// (Re)start the watchers for `[[watch]]` in the config.
    fn restart_watchers(&self) {
        // Dropping the old set stops its threads.
        *self.watch.borrow_mut() = None;
        let entries = self.cfg.borrow().watch.clone();
        if !entries.iter().any(|e| e.enabled) {
            return;
        }
        let ui = self.ui.clone();
        let on_ready: Arc<dyn Fn(WatchHit) + Send + Sync> = Arc::new(move |hit| ui.send(UiEvent::WatchReady(hit)));
        let set = WatchSet::start(&entries, self.produced.clone(), Arc::clone(&self.watch_paused), on_ready);
        if set.is_empty() {
            tracing::warn!("watch folders: no folder could be watched (check the folder paths and operations)");
        }
        *self.watch.borrow_mut() = Some(set);
    }

    /// A file in a watch folder is complete: run the operation if it fits the file.
    fn on_watch_ready(&self, hit: WatchHit) {
        if !self.engines_ready.get() {
            self.pending_watch.borrow_mut().push(hit);
            return;
        }
        let Some(f) = format_of(&hit.path) else { return };
        let graph = self.graph.borrow().clone();
        let fits = match hit.op {
            Operation::Convert(target) => graph.route(f, target).is_some(),
            Operation::Tool(tool) => graph.tool_available(tool, f),
            Operation::Action(_) => false,
        };
        if !fits {
            tracing::debug!(file = %name_of(&hit.path), "watch folder: the operation does not fit this file");
            return;
        }
        self.submit_watch(hit.op, hit.path);
    }

    fn on_config(&self, result: Result<Config, String>) {
        let cfg = match result {
            Ok(c) => c,
            Err(text) => {
                self.notify("Zest", &text, true, None);
                return;
            }
        };
        if *self.cfg.borrow() == cfg {
            return;
        }
        // New tool paths in `[tools]`: look for the engines again.
        if self.cfg.borrow().tools != cfg.tools {
            start_probe(self.ui.clone(), ToolOverrides::from(&cfg.tools));
        }
        if let Some(input) = self.input.get() {
            input.reconfigure(&cfg);
        }
        self.paused.set(cfg.gestures.paused);
        if let Some(o) = self.overlay() {
            o.set_outer_radius(cfg.wheel.outer_radius);
        }
        self.apply_panel_config(&cfg);
        if broker_key(&cfg) != self.broker_key.get() {
            // Running jobs keep their broker; new jobs get a new one.
            self.current_broker.set(None);
        }
        let watch_changed = self.cfg.borrow().watch != cfg.watch;
        *self.cfg.borrow_mut() = cfg;
        if watch_changed {
            self.restart_watchers();
        }
        self.reap_brokers();
        tracing::info!("config applied");
    }

    fn on_paths(&self, paths: Vec<PathBuf>) {
        self.collector.borrow_mut().push(paths, Instant::now());
        self.arm_collect_timer();
    }

    fn arm_collect_timer(&self) {
        let Some(deadline) = self.collector.borrow().deadline() else { return };
        let ms = deadline.saturating_duration_since(Instant::now()).as_millis() as u32;
        // SAFETY: timer on our own window.
        unsafe {
            SetTimer(self.hwnd.get(), TIMER_COLLECT, ms.max(1), None);
        }
    }

    fn on_collect_timer(&self) {
        // SAFETY: timer on our own window.
        unsafe {
            let _ = KillTimer(self.hwnd.get(), TIMER_COLLECT);
        }
        let ready = self.collector.borrow_mut().poll(Instant::now());
        match ready {
            Some(paths) => self.open_paths(paths),
            None => self.arm_collect_timer(),
        }
    }

    /// Paths from `--convert`: open the wheel at the cursor.
    fn open_paths(&self, paths: Vec<PathBuf>) {
        if !self.engines_ready.get() {
            self.pending_open.borrow_mut().extend(paths);
            return;
        }
        tracing::info!(files = paths.len(), "open wheel for forwarded files");
        let (x, y) = cursor();
        if !self.open_wheel(x, y, paths) {
            self.notify("Zest", "There is nothing to do with these items.", false, None);
        }
    }

    fn drain(&self) {
        while let Ok(ev) = self.ui_rx.try_recv() {
            match ev {
                UiEvent::Broker(b) => self.on_broker(b),
                UiEvent::Engines { tools, engines } => self.on_engines(tools, engines),
                UiEvent::Config(c) => self.on_config(c),
                UiEvent::Paths(p) => self.on_paths(p),
                UiEvent::Notice { text, error } => self.notify("Zest", &text, error, None),
                UiEvent::LastFolder(dir) => {
                    let mut st = self.state.borrow_mut();
                    st.set_last_folder(&dir);
                    self.io.state(st.to_json());
                }
                UiEvent::HistoryLoaded(entries) => self.on_history_loaded(entries),
                UiEvent::EngineVersions(v) => self.on_engine_versions(v),
                UiEvent::DebugClosePanels => self.debug_close_panels(),
                UiEvent::WatchReady(hit) => self.on_watch_ready(hit),
                UiEvent::EngineDownload { percent, text } => self.on_engine_download(percent, text),
                UiEvent::EngineDownloadDone(r) => self.on_engine_download_done(r),
                UiEvent::HoverPreview { token, text } => {
                    if let Some(o) = self.overlay() {
                        o.set_hover_preview(token, text);
                    }
                }
                UiEvent::ToolPrepared(prep) => self.on_tool_prepared(*prep),
            }
        }
    }

    // ----- tray ---------------------------------------------------------------

    fn on_tray(&self, lparam: LPARAM) {
        match (lparam.0 as u32) & 0xFFFF {
            tray::WM_RBUTTONUP => {
                let st = MenuState {
                    paused: self.paused.get(),
                    jobs_running: !self.jobs.borrow().is_empty(),
                    autostart: zest_shell::is_autostart_enabled(),
                    context_menu: zest_shell::is_context_menu_installed(),
                    watch_paused: self.watch_paused.load(Ordering::SeqCst),
                    quiet: self.cfg.borrow().ui.quiet,
                    clipboard_ready: zest_shell::clipboard_convertible(self.hwnd.get()),
                    win11_menu: self.cfg.borrow().ui.win11_top_level_menu,
                };
                tray::show_menu(self.hwnd.get(), st);
            }
            tray::NIN_BALLOONUSERCLICK => {
                let target = self.balloon_target.borrow_mut().take();
                if self.ui_panels.balloon_error() {
                    // An error balloon opens the History window (the job panel).
                    self.show_history();
                } else if let Some(p) = target {
                    open_folder_select_async(p);
                }
            }
            _ => {}
        }
    }

    fn on_command(&self, id: u16) {
        match id {
            cmd::PAUSE => {
                let p = !self.paused.get();
                self.paused.set(p);
                if let Some(i) = self.input.get() {
                    i.pause(p);
                }
                if p {
                    if let Some(o) = self.overlay() {
                        o.hide();
                    }
                }
            }
            cmd::WATCH_PAUSE => {
                let p = !self.watch_paused.load(Ordering::SeqCst);
                self.watch_paused.store(p, Ordering::SeqCst);
                tracing::info!(paused = p, "watch folders pause changed");
            }
            cmd::QUIET => {
                let quiet = !self.cfg.borrow().ui.quiet;
                self.cfg.borrow_mut().ui.quiet = quiet;
                tracing::info!(quiet, "quiet mode toggled (tray; save Settings to persist)");
            }
            cmd::CLIPBOARD => self.convert_clipboard(),
            cmd::WIN11_MENU => self.toggle_win11_menu(),
            cmd::CANCEL => self.cancel_all(),
            cmd::SHELF => self.show_shelf(),
            cmd::HISTORY => self.show_history(),
            cmd::SETTINGS_WINDOW => self.show_settings(),
            cmd::SETTINGS => {
                let p = paths::config_path();
                if !tray::shell_open(&p) {
                    tray::open_in_notepad(&p);
                }
            }
            cmd::LOGS => {
                let d = paths::logs_dir();
                let _ = std::fs::create_dir_all(&d);
                tray::shell_open(&d);
            }
            cmd::AUTOSTART => {
                // Registry work off the UI thread.
                let _ = std::thread::Builder::new().name("zest-autostart".into()).spawn(|| {
                    let Ok(exe) = std::env::current_exe() else { return };
                    let enable = !zest_shell::is_autostart_enabled();
                    match zest_shell::set_autostart(&exe, enable) {
                        Ok(()) => tracing::info!(enable, "autostart changed"),
                        Err(e) => tracing::warn!("autostart: {e}"),
                    }
                });
            }
            cmd::CONTEXT => {
                let _ = std::thread::Builder::new().name("zest-verbs".into()).spawn(|| {
                    let Ok(exe) = std::env::current_exe() else { return };
                    let r = if zest_shell::is_context_menu_installed() {
                        zest_shell::uninstall_context_menu().and(zest_shell::uninstall_send_to())
                    } else {
                        zest_shell::install_context_menu(&exe).and(zest_shell::install_send_to(&exe))
                    };
                    match r {
                        Ok(()) => tracing::info!("right-click menu changed"),
                        Err(e) => tracing::warn!("right-click menu: {e}"),
                    }
                });
            }
            cmd::QUIT => {
                // SAFETY: our own window.
                unsafe {
                    let _ = DestroyWindow(self.hwnd.get());
                }
            }
            _ => {}
        }
    }

    fn on_timer(&self, id: usize) {
        match id {
            TIMER_COLLECT => self.on_collect_timer(),
            TIMER_HANDOFF => {
                // SAFETY: timer on our own window.
                unsafe {
                    let _ = KillTimer(self.hwnd.get(), TIMER_HANDOFF);
                }
                self.handoff_ring();
            }
            _ => {}
        }
    }
}

fn spawn_action(name: &str, f: impl FnOnce() + Send + 'static) {
    if std::thread::Builder::new().name(name.into()).spawn(f).is_err() {
        tracing::error!("cannot start the {name} thread");
    }
}

/// "Copy to" / "Move to" on its own thread: pick the folder, then let Explorer
/// copy or move with its progress dialog and conflict questions.
fn transfer_files(ui: &UiSender, files: Vec<PathBuf>, start: Option<PathBuf>, copy: bool) {
    let title = if copy { "Copy to" } else { "Move to" };
    let dest = match zest_shell::pick_folder(title, start.as_deref()) {
        Ok(Some(d)) => d,
        Ok(None) => return, // the user closed the picker
        Err(e) => {
            ui.send(UiEvent::Notice { text: format!("Cannot open the folder picker: {e}"), error: true });
            return;
        }
    };
    ui.send(UiEvent::LastFolder(dest.clone()));
    let result = if copy {
        zest_shell::copy_items(&files, &dest)
    } else {
        zest_shell::move_items(&files, &dest)
    };
    if let Err(e) = result {
        let what = if copy { "Cannot copy" } else { "Cannot move" };
        ui.send(UiEvent::Notice { text: format!("{what}: {e}"), error: true });
    }
}

fn open_folder_select_async(p: PathBuf) {
    let _ = std::thread::Builder::new().name("zest-open-folder".into()).spawn(move || {
        if let Err(e) = zest_shell::open_folder_select(&p) {
            tracing::warn!("open folder: {e}");
        }
    });
}

// ---------------------------------------------------------------------------
// Window and message loop
// ---------------------------------------------------------------------------

fn handle_message(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> bool {
    let Some(app) = APP.with(|a| a.borrow().clone()) else { return false };
    match msg {
        tray::WM_TRAY => app.on_tray(lparam),
        WM_UI => {
            app.ui.pending.store(false, Ordering::SeqCst);
            app.drain();
        }
        WM_COMMAND => app.on_command((wparam.0 & 0xFFFF) as u16),
        WM_TIMER => app.on_timer(wparam.0),
        WM_DESTROY => {
            tray::remove(hwnd);
            // SAFETY: ends the message loop of this thread.
            unsafe { PostQuitMessage(0) };
        }
        m if m == app.taskbar_created && m != 0 => {
            tracing::info!("explorer restarted, tray icon added again");
            tray::add(hwnd);
        }
        _ => return false,
    }
    true
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let handled = guard("window procedure", || handle_message(hwnd, msg, wparam, lparam)).unwrap_or(false);
    if handled {
        LRESULT(0)
    } else {
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }
}

fn create_window() -> Result<HWND> {
    // SAFETY: standard class registration and window creation on the UI thread.
    unsafe {
        let hinst = GetModuleHandleW(None).context("GetModuleHandleW")?;
        let class = w!("ZestApp");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: class,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
            anyhow::bail!("RegisterClassW: {}", windows::core::Error::from_win32());
        }
        CreateWindowExW(WINDOW_EX_STYLE(0), class, w!("Zest"), WS_POPUP, 0, 0, 0, 0, None, None, hinst, None)
            .context("CreateWindowExW")
    }
}

/// Engine probe on a background thread.
fn start_probe(ui: UiSender, overrides: ToolOverrides) {
    let spawned = std::thread::Builder::new().name("zest-probe".into()).spawn(move || {
        let t = Instant::now();
        let tools = ToolPaths::resolve(&overrides);
        let engines = tools.probe();
        tracing::info!(ms = t.elapsed().as_millis() as u64, "engine probe done");
        ui.send(UiEvent::Engines { tools, engines });
    });
    if spawned.is_err() {
        tracing::error!("cannot start the probe thread");
    }
}

/// Run the tray app until Quit. `initial` are files from the command line.
pub fn run(sid: &str, initial: Vec<PathBuf>) -> Result<()> {
    // SAFETY: first COM call on this thread; makes it an STA (needed for OLE drag and drop).
    unsafe { OleInitialize(None) }.context("OleInitialize")?;
    let result = run_inner(sid, initial);
    // SAFETY: balances OleInitialize.
    unsafe { OleUninitialize() };
    result
}

fn run_inner(sid: &str, initial: Vec<PathBuf>) -> Result<()> {
    let _ = std::fs::create_dir_all(paths::local_dir());
    // Config: write the template on first run.
    let cfg_path = paths::config_path();
    if !cfg_path.exists() {
        if let Some(dir) = cfg_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&cfg_path, Config::default_toml()) {
            tracing::warn!("cannot write the default config: {e}");
        }
    }
    let (cfg, startup_error) = match Config::load(&cfg_path) {
        Ok(c) => (c, None),
        Err(e) => {
            tracing::warn!("config: {e}");
            (Config::default(), Some(watcher::error_text(&e)))
        }
    };
    let tool_overrides = ToolOverrides::from(&cfg.tools);
    let state = State::load(&paths::state_path());

    let hwnd = create_window()?;
    let (tx, rx) = unbounded();
    let ui = UiSender { tx, hwnd: hwnd.0 as isize, pending: Arc::new(AtomicBool::new(false)) };
    // SAFETY: registers a window message name.
    let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };

    let id_seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| d.as_millis() as u64);
    let app = Rc::new(App {
        hwnd: Cell::new(hwnd),
        ui: ui.clone(),
        ui_rx: rx,
        io: io::start(paths::history_path(), paths::state_path()),
        overlay: OnceCell::new(),
        input: OnceCell::new(),
        cfg: RefCell::new(cfg.clone()),
        state: RefCell::new(state),
        graph: RefCell::new(Arc::new(FormatGraph::with_available(&[]))),
        tools: RefCell::new(None),
        engines_ready: Cell::new(false),
        pending_open: RefCell::new(Vec::new()),
        collector: RefCell::new(Collector::default()),
        brokers: RefCell::new(HashMap::new()),
        current_broker: Cell::new(None),
        broker_key: Cell::new(broker_key(&cfg)),
        next_broker_gen: Cell::new(1),
        jobs: RefCell::new(HashMap::new()),
        ring_job: Cell::new(None),
        next_job: Cell::new(id_seed),
        paused: Cell::new(cfg.gestures.paused),
        last_output: RefCell::new(None),
        balloon_target: RefCell::new(None),
        taskbar_created,
        ui_panels: panels::PanelsState::default(),
        watch: RefCell::new(None),
        watch_paused: Arc::new(AtomicBool::new(false)),
        produced: Produced::default(),
        pending_watch: RefCell::new(Vec::new()),
        engine_download_running: Cell::new(false),
        tool_dialog: Cell::new(false),
    });
    app.init_panels();
    APP.with(|a| *a.borrow_mut() = Some(Rc::clone(&app)));

    // Overlay (the wheel window).
    let weak = Rc::downgrade(&app);
    let mut cb = Callbacks::default();
    {
        let w = weak.clone();
        cb.on_files_entered = Box::new(move |files| {
            w.upgrade().and_then(|a| guard("on_files_entered", || a.on_files_entered(files)).flatten())
        });
        let w = weak.clone();
        cb.on_pick = Box::new(move |pick| {
            if let Some(a) = w.upgrade() {
                guard("on_pick", || a.on_pick(pick));
            }
        });
        cb.on_cancel = Box::new(|| tracing::debug!("wheel closed without a pick"));
        let w = weak.clone();
        cb.on_progress_clicked = Box::new(move || {
            if let Some(a) = w.upgrade() {
                guard("on_progress_clicked", || a.on_progress_clicked());
            }
        });
        let w = weak.clone();
        cb.on_hover_preview = Box::new(move |req| {
            if let Some(a) = w.upgrade() {
                guard("on_hover_preview", || a.on_hover_preview(req));
            }
        });
    }
    let overlay = Overlay::new(cb).context("cannot create the wheel window")?;
    overlay.set_outer_radius(cfg.wheel.outer_radius);
    let _ = app.overlay.set(overlay);

    // Input (gestures, hotkeys).
    let w = weak.clone();
    let input = InputHost::new(
        &cfg,
        Box::new(move |ev| {
            if let Some(a) = w.upgrade() {
                guard("input event", || a.on_input(ev));
            }
        }),
    )
    .context("cannot start gesture detection")?;
    let _ = app.input.set(input);

    tray::add(hwnd);
    if let Some(text) = startup_error {
        app.notify("Zest", &text, true, None);
    }

    // Helper threads.
    {
        let ui2 = ui.clone();
        let cb: Arc<dyn Fn(Vec<PathBuf>) + Send + Sync> = Arc::new(move |p| ui2.send(UiEvent::Paths(p)));
        if let Err(e) = ipc::start_server(sid, cb) {
            tracing::error!("pipe server: {e:#}");
        }
    }
    watcher::start(cfg_path, ui.clone());
    start_probe(ui.clone(), tool_overrides);
    app.restart_watchers();
    if !initial.is_empty() {
        ui.send(UiEvent::Paths(initial));
    }
    tracing::info!("ready");
    if std::env::var_os("ZEST_DEBUG_OPEN_PANELS").is_some() {
        app.debug_open_panels();
    }

    let mut msg = MSG::default();
    // SAFETY: standard message loop on the UI thread.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            // Tab, Enter and Esc for the Shelf, History and Settings windows.
            if guard("panel keys", || app.panels_pre_translate(&msg)).unwrap_or(false) {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    tracing::info!("quit");
    app.shutdown_panels();
    tray::remove(hwnd);
    APP.with(|a| *a.borrow_mut() = None);
    drop(app); // overlay, input and brokers are dropped here (running jobs are cancelled)
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zest_core::{FormatId, ToolKind};

    fn err(msg: &str) -> Vec<(PathBuf, String)> {
        vec![(PathBuf::from(r"C:\a\x.mp4"), msg.to_owned())]
    }

    #[test]
    fn conversion_and_tool_messages() {
        let conv = Operation::Convert(FormatId::Jpeg);
        assert_eq!(job_summary(conv, 3, &[], None), "Converted 3 files");
        assert_eq!(job_summary(conv, 1, &[], None), "Converted 1 file");
        assert_eq!(
            job_summary(Operation::Tool(ToolKind::Compress), 2, &[], None),
            "Compressed 2 files"
        );
        assert_eq!(
            job_summary(Operation::Tool(ToolKind::Rotate180), 1, &[], None),
            "Rotated 1 file"
        );
        assert_eq!(
            job_summary(Operation::Tool(ToolKind::StripMetadata), 4, &[], None),
            "Stripped 4 files"
        );
        assert_eq!(
            job_summary(Operation::Tool(ToolKind::MaxEdge1280), 1, &[], None),
            "Resized 1 file"
        );
    }

    #[test]
    fn failure_messages() {
        let size = Operation::Tool(ToolKind::VideoTargetSize(10));
        assert_eq!(
            job_summary(size, 0, &err("Too long to fit in 10 MB"), None),
            "1 failed: Too long to fit in 10 MB"
        );
        assert_eq!(
            job_summary(size, 2, &err("Too long to fit in 10 MB"), None),
            "2 compressed, 1 failed: Too long to fit in 10 MB"
        );
        let long = "x".repeat(300);
        assert!(job_summary(size, 0, &err(&long), None).chars().count() < 200);
    }

    #[test]
    fn zip_messages_name_the_archive() {
        let zip = Operation::Action(ActionKind::Zip);
        assert_eq!(
            job_summary(zip, 1, &[], Some(Path::new(r"C:\a\Photos.zip"))),
            "Created Photos.zip"
        );
        assert_eq!(job_summary(zip, 0, &[], None), "Zip file created");
        assert_eq!(
            job_summary(zip, 0, &err("Cannot read C:\\a\\b"), None),
            "Zip failed: Cannot read C:\\a\\b"
        );
    }
}
