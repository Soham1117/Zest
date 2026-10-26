//! The Trim and Crop windows: pick a tool with settings in the Tools ring,
//! prepare the data on a thread (picture or length), show the window, and run
//! the job with the values from it.

use super::*;
use std::sync::Arc;
use zest_broker::preview::{self, CropPreview};
use zest_core::{Category, ToolKind};
use zest_panels::{CropInput, TrimInput};

/// Data for a window, made on a thread.
pub enum ToolPrep {
    Trim { file: PathBuf, duration_ms: Option<u64>, video: bool },
    Crop { file: PathBuf, preview: CropPreview },
    Failed(String),
}

/// Folder for preview pictures and frames.
fn preview_dir() -> PathBuf {
    std::env::temp_dir().join("zest-preview")
}

impl App {
    /// The user picked Trim or Crop. The ring is not wanted: the window comes first.
    pub(super) fn begin_tool_dialog(&self, tool: ToolKind, pick: &Pick) {
        if let Some(o) = self.overlay() {
            o.hand_off();
        }
        if self.tool_dialog.get() {
            return;
        }
        // Custom video size: ask for MB (1–9999), then run like a normal tool pick.
        if matches!(tool, ToolKind::VideoTargetSize(0)) {
            let Some(mb) = ask_size_mb() else { return };
            if !(1..=9999).contains(&mb) {
                self.notify("Zest", "Size must be between 1 and 9999 MB.", true, None);
                return;
            }
            let mut pick = pick.clone();
            pick.item.op = Operation::Tool(ToolKind::VideoTargetSize(mb));
            self.on_pick(pick);
            return;
        }
        let graph = self.graph.borrow().clone();
        let file = pick
            .files
            .iter()
            .find(|p| {
                format_of(p).is_some_and(|f| {
                    pick.category.is_none_or(|c| f.category() == c) && graph.tool_available(tool, f)
                })
            })
            .cloned();
        let Some(file) = file else {
            self.notify("Zest", "That tool does not work on this file.", true, None);
            return;
        };
        let Some(tools) = self.tools.borrow().clone() else {
            self.notify("Zest", "Zest is still starting. Try again in a moment.", false, None);
            return;
        };
        self.tool_dialog.set(true);
        let ui = self.ui.clone();
        spawn_action("zest-tool-prepare", move || {
            let prep = match tool {
                ToolKind::Trim { .. } => ToolPrep::Trim {
                    duration_ms: preview::duration_ms(&tools.ffprobe, &file),
                    video: format_of(&file).is_some_and(|f| f.category() == Category::Video),
                    file,
                },
                _ => match preview::crop_preview(&tools.vips, &file, &preview_dir()) {
                    Ok(p) => ToolPrep::Crop { file, preview: p },
                    Err(e) => ToolPrep::Failed(format!("Cannot open the crop window: {e}")),
                },
            };
            ui.send(UiEvent::ToolPrepared(Box::new(prep)));
        });
    }

    /// Show the window (a nested message loop) and start the job if the user pressed OK.
    pub(super) fn on_tool_prepared(&self, prep: ToolPrep) {
        let owner = self.hwnd.get();
        match prep {
            ToolPrep::Failed(text) => {
                self.tool_dialog.set(false);
                self.notify("Zest", &text, true, None);
            }
            ToolPrep::Trim { file, duration_ms, video } => {
                let frame: Option<zest_panels::FrameFn> = match (video, self.tools.borrow().as_ref()) {
                    (true, Some(t)) => {
                        let (ffmpeg, src) = (t.ffmpeg.clone(), file.clone());
                        Some(Arc::new(move |ms| preview::video_frame(&ffmpeg, &src, ms, &preview_dir())))
                    }
                    _ => None,
                };
                let input = TrimInput { title: name_of(&file), duration_ms, frame };
                let result = zest_panels::trim_dialog(owner, input);
                self.tool_dialog.set(false);
                if let Some((start_ms, end_ms)) = result {
                    self.submit_tool(ToolKind::Trim { start_ms, end_ms }, file);
                }
            }
            ToolPrep::Crop { file, preview } => {
                let input = CropInput {
                    title: name_of(&file),
                    preview: preview.png.clone(),
                    width: preview.width,
                    height: preview.height,
                };
                let result = zest_panels::crop_dialog(owner, input);
                let _ = std::fs::remove_file(&preview.png);
                self.tool_dialog.set(false);
                if let Some((x, y, w, h)) = result {
                    self.submit_tool(ToolKind::Crop { x, y, w, h }, file);
                }
            }
        }
    }
}

/// Ask for a target size in MB via a tiny PowerShell InputBox (1–9999).
fn ask_size_mb() -> Option<u32> {
    let script = r#"
Add-Type -AssemblyName Microsoft.VisualBasic
$r = [Microsoft.VisualBasic.Interaction]::InputBox('Target size in MB (1-9999):', 'Zest', '25')
if ($r) { $r } else { '' }
"#;
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", script])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if s.is_empty() {
        return None;
    }
    s.parse().ok()
}

/// Result of the PDF → image “Pages…” prompt.
pub(super) enum PageRangeAsk {
    /// User cancelled the dialog (or PowerShell failed).
    Cancelled,
    /// Typed something that is not `all` / `1-3` / `1,3,5`.
    Invalid,
    /// Convert every page.
    All,
    /// Worker `--pages` value.
    Spec(String),
}

/// PDF → image page range dialog (default `all`).
pub(super) fn ask_page_range() -> PageRangeAsk {
    let script = r#"
Add-Type -AssemblyName Microsoft.VisualBasic
$r = [Microsoft.VisualBasic.Interaction]::InputBox('Pages (e.g. 1-3 or 1,3,5). Type all for every page. Cancel aborts.', 'Zest', 'all')
if ($null -eq $r) { '' } elseif ($r) { $r } else { '' }
"#;
    let Ok(out) = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", script])
        .output()
    else {
        return PageRangeAsk::Cancelled;
    };
    if !out.status.success() {
        return PageRangeAsk::Cancelled;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if s.is_empty() {
        return PageRangeAsk::Cancelled;
    }
    if s.eq_ignore_ascii_case("all") {
        return PageRangeAsk::All;
    }
    if page_range_looks_ok(&s) {
        PageRangeAsk::Spec(s)
    } else {
        PageRangeAsk::Invalid
    }
}

fn page_range_looks_ok(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_digit() || c == '-' || c == ',' || c == ' ')
        && s.chars().any(|c| c.is_ascii_digit())
}
