//! Glue between the app and `zest-panels`: the Shelf, the History window and
//! the Settings window. The app feeds them data and runs what they ask for.

use super::*;
use std::ffi::OsString;
use std::rc::Weak;
use std::time::Duration;
use zest_broker::CommandSpec;
use zest_panels::{EngineLine, JobEntry, PanelHost, Panels, SettingsInput, ShelfState};

/// State of the panels inside `App`.
#[derive(Default)]
pub(super) struct PanelsState {
    panels: OnceCell<Panels>,
    /// Engines that the last probe found.
    engines: RefCell<Vec<EngineId>>,
    /// "libvips" -> "8.16.0" (from the version thread).
    versions: RefCell<Vec<(String, String)>>,
    /// The last balloon was an error: a click opens the History window.
    balloon_error: Cell<bool>,
}

impl PanelsState {
    pub(super) fn set_balloon_error(&self, error: bool) {
        self.balloon_error.set(error);
    }

    pub(super) fn balloon_error(&self) -> bool {
        self.balloon_error.get()
    }
}

/// What the panels ask of the app.
struct Host {
    app: Weak<App>,
}

impl PanelHost for Host {
    fn shelf_changed(&self, state: ShelfState) {
        if let Some(app) = self.app.upgrade() {
            let mut st = app.state.borrow_mut();
            st.shelf = state;
            app.io.state(st.to_json());
        }
    }

    fn history_load(&self) {
        let Some(app) = self.app.upgrade() else { return };
        let ui = app.ui.clone();
        spawn_action("zest-history-load", move || {
            let text = crate::io::read_tail_text(&paths::history_path(), 4 * 1024 * 1024).unwrap_or_default();
            let records = history::parse_tail(&text, 2000);
            ui.send(UiEvent::HistoryLoaded(history::job_entries(&records)));
        });
    }

    fn history_rerun(&self, op: Operation, files: Vec<PathBuf>, skipped: usize) {
        if let Some(app) = self.app.upgrade() {
            app.submit_rerun(op, files, skipped);
        }
    }

    fn history_cancel(&self, id: u64) {
        if let Some(app) = self.app.upgrade() {
            let jobs = app.jobs.borrow();
            if let Some(job) = jobs.get(&JobId(id)) {
                if let Some(b) = app.brokers.borrow().get(&job.broker_gen) {
                    b.cancel(JobId(id));
                    tracing::info!(job = id, "job cancelled from the History window");
                }
            }
        }
    }

    fn history_undone(&self, id: u64, outputs: Vec<PathBuf>) {
        if let Some(app) = self.app.upgrade() {
            tracing::info!(job = id, files = outputs.len(), "outputs moved to the Recycle Bin");
            app.io.history(HistoryRecord::undone(id, history::utc_now(), &outputs).to_line());
        }
    }

    fn notice(&self, text: &str, error: bool) {
        if let Some(app) = self.app.upgrade() {
            app.notify("Zest", text, error, None);
        }
    }

    fn engine_download(&self, name: &str) {
        if let Some(app) = self.app.upgrade() {
            app.start_engine_download(name.to_owned());
        }
    }
}

fn version_of(program: &Path, flag: &str, pick: impl Fn(&str) -> Option<String>) -> Option<String> {
    let spec = CommandSpec { program: program.to_path_buf(), args: vec![OsString::from(flag)] };
    let out = zest_broker::process::run_capture(&spec, Duration::from_secs(3))?;
    pick(out.lines().next().unwrap_or(""))
}

/// First number-like token, cut at the first '-' ("7.1-essentials" -> "7.1").
fn short_version(token: &str) -> Option<String> {
    let t = token.trim_start_matches(['v', 'V']);
    let first = t.split('-').next().unwrap_or(t);
    first.chars().next().filter(char::is_ascii_digit).map(|_| first.to_owned())
}

/// Versions for the Engines section. Runs on a thread (starts child processes).
fn read_versions(tools: &ToolPaths) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(v) = version_of(&tools.vips, "--version", |l| short_version(l.strip_prefix("vips-")?)) {
        out.push((EngineId::Vips.name().to_owned(), v));
    }
    if let Some(v) = version_of(&tools.ffmpeg, "-version", |l| short_version(l.split_whitespace().nth(2)?)) {
        out.push((EngineId::Ffmpeg.name().to_owned(), v));
    }
    if let Some(p) = &tools.pandoc {
        if let Some(v) = version_of(p, "--version", |l| short_version(l.split_whitespace().nth(1)?)) {
            out.push((EngineId::Pandoc.name().to_owned(), v));
        }
    }
    out
}

impl App {
    pub(super) fn panels(&self) -> Option<&Panels> {
        self.ui_panels.panels.get()
    }

    /// Create the owner of the panel windows (the windows open on demand).
    pub(super) fn init_panels(self: &Rc<Self>) {
        let host: Rc<dyn PanelHost> = Rc::new(Host { app: Rc::downgrade(self) });
        let shelf = self.state.borrow().shelf.clone();
        let _ = self.ui_panels.panels.set(Panels::new(host, &shelf));
        self.apply_panel_config(&self.cfg.borrow());
    }

    /// Config settings that the wheel reads.
    pub(super) fn apply_panel_config(&self, cfg: &Config) {
        zest_overlay::set_reduce_animations(cfg.wheel.reduce_animations == zest_core::ReduceAnimations::Always);
    }

    pub(super) fn show_shelf(&self) {
        if let Some(p) = self.panels() {
            p.shelf_show(true);
        }
    }

    pub(super) fn show_history(&self) {
        if let Some(p) = self.panels() {
            p.history_show();
        }
    }

    /// Hold these items on the Shelf. `activate` = the user asked for it now.
    pub(super) fn shelf_add_files(&self, files: Vec<PathBuf>, activate: bool) {
        let Some(p) = self.panels() else { return };
        let added = p.shelf_add(files);
        tracing::info!(added, "items sent to the Shelf");
        p.shelf_show(activate);
    }

    pub(super) fn show_settings(&self) {
        let Some(p) = self.panels() else { return };
        p.settings_show(SettingsInput {
            config_path: paths::config_path(),
            config: self.cfg.borrow().clone(),
            engines: self.engine_lines(),
            tools_dir: self.tools_dir(),
            autostart: zest_shell::is_autostart_enabled(),
            context_menu: zest_shell::is_context_menu_installed(),
            libreoffice_url: zest_broker::engines::libreoffice_download_url().to_owned(),
        });
    }

    /// Send the engine status to an open Settings window.
    pub(super) fn push_engine_lines(&self) {
        if let Some(p) = self.panels() {
            p.settings_engines(self.engine_lines());
        }
    }

    // ----- Pandoc download (only after the user agreed in the Settings window) ------------

    pub(super) fn start_engine_download(&self, name: String) {
        if self.engine_download_running.replace(true) {
            return;
        }
        tracing::info!(engine = %name, "engine download started (user agreed)");
        let ui = self.ui.clone();
        spawn_action("zest-engine-download", move || {
            use zest_broker::engines::EnginePhase;
            let mut last: Option<(Option<i32>, &'static str)> = None;
            let result = zest_broker::engines::download_optional(&name, &mut |p| {
                let text = match p.phase {
                    EnginePhase::Downloading => "Downloading",
                    EnginePhase::Verifying => "Checking",
                    EnginePhase::Extracting => "Unpacking",
                    EnginePhase::Done => "Done",
                };
                let key = (p.percent.map(|x| x.round() as i32), text);
                if last != Some(key) {
                    last = Some(key);
                    ui.send(UiEvent::EngineDownload { percent: p.percent, text });
                }
            });
            ui.send(UiEvent::EngineDownloadDone(result.map(|_| ()).map_err(|e| e.to_string())));
        });
    }

    pub(super) fn on_engine_download(&self, percent: Option<f32>, text: &'static str) {
        if let Some(p) = self.panels() {
            p.settings_download_progress(percent, text);
        }
    }

    pub(super) fn on_engine_download_done(&self, result: Result<(), String>) {
        self.engine_download_running.set(false);
        if let Some(p) = self.panels() {
            p.settings_download_done(result.clone());
        }
        match result {
            Ok(()) => {
                tracing::info!("engine download done");
                // Look for the engines again: Pandoc is found in the engines folder.
                start_probe(self.ui.clone(), ToolOverrides::from(&self.cfg.borrow().tools));
                self.notify("Zest", "Pandoc is installed. Markdown, HTML and EPUB conversions are ready.", false, None);
            }
            Err(e) => {
                tracing::warn!("engine download failed: {e}");
                self.notify("Zest", &format!("Pandoc download failed: {e}"), true, None);
            }
        }
    }

    fn tools_dir(&self) -> PathBuf {
        let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)).unwrap_or_default();
        // `<tools>\vips\bin\vips.exe` -> `<tools>`
        self.tools
            .borrow()
            .as_ref()
            .and_then(|t| t.vips.ancestors().nth(3).map(Path::to_path_buf))
            .filter(|d| d.is_dir())
            .unwrap_or(exe_dir)
    }

    fn engine_lines(&self) -> Vec<EngineLine> {
        let found = self.ui_panels.engines.borrow();
        let versions = self.ui_panels.versions.borrow();
        let ready = self.engines_ready.get();
        EngineId::ALL
            .iter()
            .map(|e| {
                let ok = found.contains(e);
                let detail = if !ready {
                    "checking\u{2026}".to_owned()
                } else if ok {
                    versions
                        .iter()
                        .find(|(n, _)| n == e.name())
                        .map(|(_, v)| v.clone())
                        .unwrap_or_else(|| "ready".to_owned())
                } else {
                    "not installed".to_owned()
                };
                EngineLine { name: e.name().to_owned(), ok, detail }
            })
            .collect()
    }

    /// The probe finished: remember the result and read the versions on a thread.
    pub(super) fn engines_probed(&self, tools: &ToolPaths, engines: &[EngineId]) {
        *self.ui_panels.engines.borrow_mut() = engines.to_vec();
        let tools = tools.clone();
        let ui = self.ui.clone();
        spawn_action("zest-engine-versions", move || {
            ui.send(UiEvent::EngineVersions(read_versions(&tools)));
        });
    }

    pub(super) fn on_engine_versions(&self, v: Vec<(String, String)>) {
        *self.ui_panels.versions.borrow_mut() = v;
        self.push_engine_lines();
    }

    pub(super) fn on_history_loaded(&self, entries: Vec<JobEntry>) {
        if let Some(p) = self.panels() {
            p.history_loaded(entries);
        }
    }

    // ----- jobs -> History window ----------------------------------------------------

    pub(super) fn panels_job_started(&self, id: JobId, op_key: &str, inputs: &[PathBuf]) {
        if let Some(p) = self.panels() {
            p.history_job_started(JobEntry::started(id.0, history::utc_now(), op_key, inputs.to_vec()));
        }
    }

    pub(super) fn panels_job_progress(&self, id: JobId, percent: Option<f32>) {
        if let Some(p) = self.panels() {
            p.history_job_progress(id.0, percent.map(|x| x / 100.0));
        }
    }

    /// A job ended: tell the History window and, with `send_to_shelf`, put the outputs on the Shelf.
    pub(super) fn panels_job_finished(
        &self,
        id: JobId,
        op: Operation,
        outputs: &[PathBuf],
        errors: &[(PathBuf, String)],
        cancelled: bool,
    ) {
        let Some(p) = self.panels() else { return };
        p.history_job_finished(id.0, outputs.to_vec(), errors.to_vec(), cancelled);
        let clipboard_job = matches!(op, Operation::Action(ActionKind::CopyAs(_)));
        if self.cfg.borrow().output.send_to_shelf && !outputs.is_empty() && !cancelled && !clipboard_job {
            p.shelf_add(outputs.to_vec());
            // Do not take the focus: the user may be working in another window.
            p.shelf_show(false);
        }
    }

    /// Re-run / Retry failed: the same operation on files that still exist.
    /// There is no progress ring; the History window and the balloon report it.
    fn submit_rerun(&self, op: Operation, files: Vec<PathBuf>, skipped: usize) {
        if skipped > 0 {
            let text = format!("{skipped} input file{} not found and skipped.", plural(skipped));
            self.notify("Zest", &text, false, None);
        }
        self.submit_request(op, files, false);
    }

    /// A file of a watch folder: no ring, and only a failure shows a balloon.
    pub(super) fn submit_watch(&self, op: Operation, file: PathBuf) {
        self.submit_request(op, vec![file], true);
    }

    /// Trim or Crop with the values from their window (one file).
    pub(super) fn submit_tool(&self, tool: zest_core::ToolKind, file: PathBuf) {
        self.submit_request(Operation::Tool(tool), vec![file], false);
    }

    fn submit_request(&self, op: Operation, files: Vec<PathBuf>, silent: bool) {
        let (out_dir, quality) = {
            let cfg = self.cfg.borrow();
            (cfg.output.location.out_dir().map(Path::to_path_buf), cfg.output.quality)
        };
        let id = JobId(self.next_job.get());
        self.next_job.set(id.0 + 1);
        let req = zest_core::JobRequest {
            id,
            files,
            op,
            quality,
            out_dir,
            page_range: None,
        };
        let Some(gen) = self.ensure_broker() else {
            self.notify("Zest", "The converter is not ready yet.", true, None);
            return;
        };
        let key = req.op.key();
        tracing::info!(job = id.0, files = req.files.len(), op = %key, silent, "job submitted without a ring");
        self.io.history(HistoryRecord::started(id.0, history::utc_now(), &req.files, &key).to_line());
        self.panels_job_started(id, &key, &req.files);
        self.jobs.borrow_mut().insert(
            id,
            ActiveJob {
                inputs: req.files.clone(),
                op: req.op,
                started: Instant::now(),
                handed_off: true,
                broker_gen: gen,
                silent,
            },
        );
        if let Some(b) = self.brokers.borrow().get(&gen) {
            b.submit(req);
        }
    }

    // ----- message loop and shutdown ----------------------------------------------------

    pub(super) fn panels_pre_translate(&self, msg: &MSG) -> bool {
        self.panels().is_some_and(|p| p.pre_translate(msg))
    }

    pub(super) fn shutdown_panels(&self) {
        if let Some(p) = self.panels() {
            p.shutdown();
        }
    }

    // ----- debug ------------------------------------------------------------------------------

    /// `ZEST_DEBUG_OPEN_PANELS=1`: open each panel once and close them after about 2 seconds
    /// (a smoke test for the windows; the Shelf gets the program file as a test item).
    pub(super) fn debug_open_panels(&self) {
        let Some(p) = self.panels() else { return };
        tracing::info!("debug: opening the panels");
        if let Ok(exe) = std::env::current_exe() {
            p.shelf_add(vec![exe]);
        }
        p.shelf_show(false);
        p.history_show();
        self.show_settings();
        let ui = self.ui.clone();
        spawn_action("zest-debug-panels", move || {
            std::thread::sleep(Duration::from_millis(2200));
            ui.send(UiEvent::DebugClosePanels);
        });
    }

    pub(super) fn debug_close_panels(&self) {
        let Some(p) = self.panels() else { return };
        tracing::info!(
            shelf = p.shelf_visible(),
            history = p.history_visible(),
            settings = p.settings_visible(),
            "debug: closing the panels"
        );
        // Leave the Shelf as it was before the test.
        if let Ok(exe) = std::env::current_exe() {
            p.shelf_remove(&[exe]);
        }
        p.shutdown();
    }
}
