//! Zest panels: the Shelf, the History window and the Settings window.
//!
//! Plain Win32 and Common Controls v6. Everything runs on the UI thread of
//! the app. Slow work (disk, shell, registry) runs on short-lived threads that
//! report back through a mailbox. The app talks to the panels through
//! [`Panels`] and listens through [`PanelHost`].

pub mod dialogs;
mod history;
pub mod model_crop;
pub mod model_history;
pub mod model_settings;
pub mod model_shelf;
mod settings;
mod shelf;
pub mod win;

pub use dialogs::{crop_dialog, trim_dialog, CropInput, FrameFn, TrimInput};
pub use model_history::{JobEntry, JobStatus};
pub use model_shelf::ShelfState;
pub use settings::{save_to_file, write_atomic};

use std::path::PathBuf;
use std::rc::Rc;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{IsChild, MSG, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP};
use zest_core::{Config, Operation};

/// What the panels need from the app. Called on the UI thread.
pub trait PanelHost {
    /// The Shelf changed (items, position, option): save `state.json`.
    fn shelf_changed(&self, state: ShelfState);
    /// The History window opened for the first time: read history.jsonl on a
    /// thread and answer with [`Panels::history_loaded`].
    fn history_load(&self);
    /// Run a job again with these inputs (they exist). `skipped` inputs are gone.
    fn history_rerun(&self, op: Operation, files: Vec<PathBuf>, skipped: usize);
    fn history_cancel(&self, id: u64);
    /// The outputs of this job went to the Recycle Bin: record it in history.jsonl.
    fn history_undone(&self, id: u64, outputs: Vec<PathBuf>);
    /// A balloon (errors that a window cannot show itself).
    fn notice(&self, text: &str, error: bool);
    /// The user agreed to download an optional engine (`"pandoc"`). Run it on a
    /// thread and report with [`Panels::settings_download_progress`] and
    /// [`Panels::settings_download_done`].
    fn engine_download(&self, name: &str);
}

/// One line of the "Engines" section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineLine {
    pub name: String,
    pub ok: bool,
    /// Version, or "not installed".
    pub detail: String,
}

/// What the Settings window needs when it opens.
#[derive(Debug, Clone)]
pub struct SettingsInput {
    pub config_path: PathBuf,
    /// The settings that the app uses now.
    pub config: Config,
    pub engines: Vec<EngineLine>,
    /// Folder for the "Open tools folder" button.
    pub tools_dir: PathBuf,
    pub autostart: bool,
    pub context_menu: bool,
    /// Page that "Get LibreOffice..." opens in the browser.
    pub libreoffice_url: String,
}

/// The three windows. Windows are created when they are first shown.
pub struct Panels {
    shelf: Rc<shelf::ShelfWin>,
    history: Rc<history::HistoryWin>,
    settings: Rc<settings::SettingsWin>,
}

impl Panels {
    pub fn new(host: Rc<dyn PanelHost>, shelf: &ShelfState) -> Panels {
        win::init_dark_mode_support();
        Panels {
            shelf: shelf::ShelfWin::new(Rc::clone(&host), shelf),
            history: history::HistoryWin::new(Rc::clone(&host)),
            settings: settings::SettingsWin::new(Rc::clone(&host)),
        }
    }

    // ----- Shelf -----

    /// Show the Shelf. `activate` = take the focus (from the tray menu); false
    /// for a job that finished in the background.
    pub fn shelf_show(&self, activate: bool) {
        self.shelf.show(activate);
    }

    pub fn shelf_hide(&self) {
        self.shelf.hide();
    }

    /// Remove these paths from the Shelf (the files stay where they are).
    pub fn shelf_remove(&self, paths: &[PathBuf]) {
        self.shelf.remove_items(paths);
    }

    pub fn shelf_visible(&self) -> bool {
        self.shelf.is_visible()
    }

    /// Hold these paths. Returns how many were new.
    pub fn shelf_add(&self, paths: Vec<PathBuf>) -> usize {
        self.shelf.add(paths)
    }

    // ----- History -----

    pub fn history_show(&self) {
        self.history.show();
    }

    /// Undo the most recent undoable job (Recycle Bin). Returns how many outputs were recycled.
    pub fn history_undo_last(&self) -> Result<usize, String> {
        self.history.undo_last()
    }

    pub fn history_visible(&self) -> bool {
        self.history.is_visible()
    }

    /// Jobs read from history.jsonl (oldest first). Live jobs win.
    pub fn history_loaded(&self, entries: Vec<JobEntry>) {
        self.history.loaded(entries);
    }

    pub fn history_job_started(&self, entry: JobEntry) {
        self.history.job_started(entry);
    }

    pub fn history_job_progress(&self, id: u64, fraction: Option<f32>) {
        self.history.job_progress(id, fraction);
    }

    pub fn history_job_finished(
        &self,
        id: u64,
        outputs: Vec<PathBuf>,
        errors: Vec<(PathBuf, String)>,
        cancelled: bool,
    ) {
        self.history.job_finished(id, outputs, errors, cancelled);
    }

    // ----- Settings -----

    pub fn settings_show(&self, input: SettingsInput) {
        self.settings.show(input);
    }

    pub fn settings_visible(&self) -> bool {
        self.settings.is_visible()
    }

    /// New engine status (after a probe or a download). Updates an open Settings window.
    pub fn settings_engines(&self, lines: Vec<EngineLine>) {
        self.settings.update_engines(lines);
    }

    /// Progress of the engine download: `percent` 0 to 100, or `None` if not known; `text` says the step.
    pub fn settings_download_progress(&self, percent: Option<f32>, text: &str) {
        self.settings.download_progress(percent, text);
    }

    /// The engine download ended.
    pub fn settings_download_done(&self, result: Result<(), String>) {
        self.settings.download_done(result);
    }

    // ----- message loop -----

    /// Call for every message before `TranslateMessage`. True = handled.
    /// Gives the panels Tab, Enter and Esc.
    pub fn pre_translate(&self, msg: &MSG) -> bool {
        if !matches!(msg.message, WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP) {
            return false;
        }
        let belongs = |top: HWND| {
            // SAFETY: plain queries on window handles.
            !top.is_invalid() && (msg.hwnd == top || unsafe { IsChild(top, msg.hwnd).as_bool() })
        };
        if belongs(self.history.hwnd()) {
            return self.history.pre_translate(msg);
        }
        if belongs(self.settings.hwnd()) {
            return self.settings.pre_translate(msg);
        }
        if belongs(self.shelf.hwnd()) {
            return self.shelf.pre_translate(msg);
        }
        false
    }

    /// Destroy the windows (before the app quits).
    pub fn shutdown(&self) {
        self.shelf.hide();
        self.shelf.destroy();
        self.history.destroy();
        self.settings.destroy();
    }
}
