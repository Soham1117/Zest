//! Turn the wheel's input and output into jobs (pure, no Windows calls).

use std::path::{Path, PathBuf};
use zest_core::{
    ActionKind, Category, EngineId, FormatGraph, FormatId, InputKind, JobId, JobRequest, Operation, Quality,
    WheelModel, WheelPrefs,
};
use zest_overlay::Pick;

/// Input format of a path, from its extension.
pub fn format_of(path: &Path) -> Option<FormatId> {
    path.extension().and_then(|e| FormatId::from_extension(&e.to_string_lossy()))
}

/// What a path is for the wheel. A path with a media extension is a file (no
/// disk access); anything else is looked at once to tell a folder from a file.
pub fn kind_of(path: &Path, is_dir: impl Fn(&Path) -> bool) -> InputKind {
    match format_of(path) {
        Some(f) => InputKind::File(Some(f)),
        None if is_dir(path) => InputKind::Folder,
        None => InputKind::File(None),
    }
}

/// Wheel model for a set of paths (files and folders). Returns the model and
/// the number of items shown in the center. `None` if there is no item.
pub fn model_for(
    graph: &FormatGraph,
    paths: &[PathBuf],
    prefs: &WheelPrefs,
) -> Option<(WheelModel, usize)> {
    model_for_with(graph, paths, prefs, |p| p.is_dir())
}

fn model_for_with(
    graph: &FormatGraph,
    paths: &[PathBuf],
    prefs: &WheelPrefs,
    is_dir: impl Fn(&Path) -> bool,
) -> Option<(WheelModel, usize)> {
    let kinds: Vec<InputKind> = paths.iter().map(|p| kind_of(p, &is_dir)).collect();
    let model = graph.wheel_model(&kinds, prefs);
    if model.is_empty() {
        tracing::debug!(items = paths.len(), "no wheel: nothing selected");
        return None;
    }
    tracing::debug!(items = paths.len(), skipped = model.skipped(), "wheel model built");
    Some((model, paths.len()))
}

/// Build the job for a pick of a conversion, a tool or the Zip action.
///
/// - Conversion: only files that have an edge to the target are used.
/// - Tool: only files the tool works on (and whose engine is available).
/// - Zip: every item, files and folders.
/// - Copy as...: the image files.
/// - Merge PDFs: the PDFs, in selection order, and only when there are two or more.
///
/// When the pick comes from a category group, only files of that category are
/// used. `None` if no item is left. The actions that the app runs itself
/// (Copy path, ...) give `None`: they are not jobs.
pub fn build_request(
    id: JobId,
    pick: &Pick,
    graph: &FormatGraph,
    out_dir: Option<&Path>,
    quality: Quality,
) -> Option<JobRequest> {
    let op = pick.item.op;
    let wanted: Option<Category> = pick.category;
    let usable = |p: &Path| -> bool {
        let Some(f) = format_of(p) else { return false };
        if !wanted.is_none_or(|c| f.category() == c) {
            return false;
        }
        match op {
            Operation::Convert(target) => graph.route(f, target).is_some(),
            Operation::Tool(tool) => graph.tool_available(tool, f),
            // Copy as...: images only.
            Operation::Action(ActionKind::CopyAs(_)) => {
                f.category() == Category::Image && graph.has_engine(EngineId::Vips)
            }
            Operation::Action(_) => false,
        }
    };
    let files: Vec<PathBuf> = match op {
        Operation::Action(ActionKind::Zip) => pick.files.clone(),
        Operation::Action(ActionKind::CopyAs(_)) => pick.files.iter().filter(|p| usable(p)).cloned().collect(),
        Operation::Action(_) => return None,
        _ => pick.files.iter().filter(|p| usable(p)).cloned().collect(),
    };
    if files.is_empty() {
        return None;
    }
    // Merge PDFs joins two or more files; a single one is not a merge.
    if matches!(op, Operation::Tool(t) if t.is_batch()) && files.len() < 2 {
        return None;
    }
    let quality = pick.item.quality.unwrap_or(quality);
    Some(JobRequest {
        id,
        files,
        op,
        quality,
        out_dir: out_dir.map(Path::to_path_buf),
        page_range: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use zest_core::{EngineId, ToolKind, WheelEntry, WheelItem};

    fn item(target: FormatId) -> WheelItem {
        WheelItem {
            op: Operation::Convert(target),
            label: target.label(),
            lossy: false,
            engine: Some(EngineId::Vips),
            quality: None,
        }
    }

    fn op_item(op: Operation) -> WheelItem {
        WheelItem { op, label: "x", lossy: false, engine: None, quality: None }
    }

    fn pick_op(files: &[&str], category: Option<Category>, item: WheelItem) -> Pick {
        Pick {
            files: files.iter().map(PathBuf::from).collect(),
            group: category.map(|_| 0),
            category,
            item,
        }
    }

    fn pick(files: &[&str], category: Option<Category>, target: FormatId) -> Pick {
        pick_op(files, category, item(target))
    }

    #[test]
    fn copy_as_uses_image_files_only() {
        let g = FormatGraph::new();
        let op = Operation::Action(ActionKind::CopyAs(FormatId::Png));
        let p = pick_op(&["a.jpg", "b.heic", "c.pdf", "d.mp4"], None, op_item(op));
        let r = build_request(JobId(3), &p, &g, None, Quality::High).unwrap();
        assert_eq!(r.op, op);
        assert_eq!(r.files, vec![PathBuf::from("a.jpg"), PathBuf::from("b.heic")]);
        // No image: no job. No libvips: no job.
        assert!(build_request(JobId(3), &pick_op(&["c.pdf"], None, op_item(op)), &g, None, Quality::High).is_none());
        let no_vips = FormatGraph::with_available(&[EngineId::Ffmpeg]);
        assert!(build_request(JobId(3), &pick_op(&["a.jpg"], None, op_item(op)), &no_vips, None, Quality::High).is_none());
    }

    #[test]
    fn merge_pdf_needs_two_pdfs_in_order() {
        let g = FormatGraph::new();
        let op = Operation::Tool(ToolKind::MergePdf);
        let p = pick_op(&["z.pdf", "a.pdf", "m.PDF"], None, op_item(op));
        let r = build_request(JobId(4), &p, &g, None, Quality::High).unwrap();
        let want: Vec<PathBuf> = ["z.pdf", "a.pdf", "m.PDF"].iter().map(PathBuf::from).collect();
        assert_eq!(r.files, want, "selection order is kept");
        assert!(build_request(JobId(4), &pick_op(&["z.pdf"], None, op_item(op)), &g, None, Quality::High).is_none());
        // Other files are dropped, which can leave a single PDF.
        assert!(build_request(JobId(4), &pick_op(&["z.pdf", "a.docx"], None, op_item(op)), &g, None, Quality::High).is_none());
    }

    #[test]
    fn document_conversion_requests() {
        let g = FormatGraph::new();
        let p = pick(&["a.pdf", "b.pdf"], Some(Category::Document), FormatId::Png);
        let r = build_request(JobId(9), &p, &g, None, Quality::High).unwrap();
        assert_eq!(r.files.len(), 2);
        // Mixed selection: the Docs group only takes documents.
        let p = pick(&["a.pdf", "b.png"], Some(Category::Document), FormatId::Jpeg);
        let r = build_request(JobId(9), &p, &g, None, Quality::High).unwrap();
        assert_eq!(r.files, vec![PathBuf::from("a.pdf")]);
        // Without LibreOffice a Word file has no edge.
        let no_office = FormatGraph::with_available(&[EngineId::Vips, EngineId::Pdfium]);
        let p = pick(&["a.docx"], None, FormatId::Pdf);
        assert!(build_request(JobId(9), &p, &no_office, None, Quality::High).is_none());
        assert!(build_request(JobId(9), &p, &g, None, Quality::High).is_some());
    }

    #[test]
    fn routed_conversions_are_accepted() {
        let g = FormatGraph::new();
        // DOCX -> PNG has no direct edge: DOCX -> PDF -> PNG.
        assert!(g.edge(FormatId::Docx, FormatId::Png).is_none());
        let p = pick(&["a.docx", "b.png"], None, FormatId::Png);
        let r = build_request(JobId(6), &p, &g, None, Quality::High).unwrap();
        assert_eq!(r.files, vec![PathBuf::from("a.docx")]);
        // Without the first engine of the route there is no job.
        let pdf_only = FormatGraph::with_available(&[EngineId::Vips, EngineId::Pdfium]);
        assert!(build_request(JobId(6), &p, &pdf_only, None, Quality::High).is_none());
        // A direct edge removed on purpose: the route takes over.
        let g2 = g.without_edge(FormatId::Png, FormatId::Avif);
        let p = pick(&["a.png"], None, FormatId::Avif);
        assert!(build_request(JobId(6), &p, &g2, None, Quality::High).is_some() == g2.route(FormatId::Png, FormatId::Avif).is_some());
    }

    #[test]
    fn trim_and_crop_use_the_files_they_fit() {
        let g = FormatGraph::new();
        let trim = Operation::Tool(ToolKind::Trim { start_ms: 100, end_ms: 900 });
        let r = build_request(JobId(7), &pick_op(&["a.mp4", "b.png"], None, op_item(trim)), &g, None, Quality::High).unwrap();
        assert_eq!(r.files, vec![PathBuf::from("a.mp4")]);
        let crop = Operation::Tool(ToolKind::Crop { x: 1, y: 2, w: 30, h: 40 });
        let r = build_request(JobId(7), &pick_op(&["a.mp4", "b.png", "c.heic"], None, op_item(crop)), &g, None, Quality::High).unwrap();
        assert_eq!(r.files, vec![PathBuf::from("b.png")]);
        assert!(build_request(JobId(7), &pick_op(&["a.png"], None, op_item(trim)), &g, None, Quality::High).is_none());
    }

    #[test]
    fn flat_pick_keeps_convertible_files_only() {
        let g = FormatGraph::new();
        let p = pick(&["a.png", "b.JPG", "c.txt", "d.mp4", "e.webp"], None, FormatId::Webp);
        let r = build_request(JobId(5), &p, &g, None, Quality::High).unwrap();
        // d.mp4 has no mp4 -> webp edge, c.txt is unknown, e.webp -> webp has no edge.
        assert_eq!(r.files, vec![PathBuf::from("a.png"), PathBuf::from("b.JPG")]);
        assert_eq!(r.op, Operation::Convert(FormatId::Webp));
        assert_eq!(r.target(), Some(FormatId::Webp));
        assert_eq!(r.id, JobId(5));
        assert_eq!(r.out_dir, None);
        assert_eq!(r.quality, Quality::High);
    }

    #[test]
    fn group_pick_filters_by_category() {
        let g = FormatGraph::new();
        // Gif is a target of videos and an input of images; picking "Gif" in the
        // Video group must not take the image gif file.
        let p = pick(&["a.mp4", "b.png", "c.mkv", "d.gif"], Some(Category::Video), FormatId::Gif);
        let r = build_request(JobId(1), &p, &g, None, Quality::High).unwrap();
        assert_eq!(r.files, vec![PathBuf::from("a.mp4"), PathBuf::from("c.mkv")]);
        let p = pick(&["a.mp4", "b.png", "c.mkv"], Some(Category::Image), FormatId::Jpeg);
        let r = build_request(JobId(1), &p, &g, None, Quality::High).unwrap();
        assert_eq!(r.files, vec![PathBuf::from("b.png")]);
    }

    #[test]
    fn out_dir_and_quality_pass_through() {
        let g = FormatGraph::new();
        let p = pick(&["a.png"], None, FormatId::Jpeg);
        let r = build_request(JobId(2), &p, &g, Some(Path::new(r"D:\out")), Quality::Small).unwrap();
        assert_eq!(r.out_dir, Some(PathBuf::from(r"D:\out")));
        assert_eq!(r.quality, Quality::Small);
    }

    #[test]
    fn nothing_left_is_none() {
        let g = FormatGraph::new();
        assert!(build_request(JobId(1), &pick(&["a.xyz"], None, FormatId::Jpeg), &g, None, Quality::High).is_none());
        let empty = FormatGraph::with_available(&[]);
        assert!(build_request(JobId(1), &pick(&["a.png"], None, FormatId::Jpeg), &empty, None, Quality::High).is_none());
    }

    #[test]
    fn tool_pick_keeps_files_the_tool_works_on() {
        let g = FormatGraph::new();
        let p = pick_op(
            &["a.png", "b.mp4", "c.heic", "d.bmp", "e.JPG", "f.txt"],
            None,
            op_item(Operation::Tool(ToolKind::Resize50)),
        );
        let r = build_request(JobId(3), &p, &g, None, Quality::High).unwrap();
        assert_eq!(r.files, vec![PathBuf::from("a.png"), PathBuf::from("e.JPG")]);
        assert_eq!(r.op, Operation::Tool(ToolKind::Resize50));
        assert_eq!(r.target(), None);
        // Video tool: only videos.
        let p = pick_op(
            &["a.png", "b.mp4", "c.mkv", "d.mp3"],
            None,
            op_item(Operation::Tool(ToolKind::VideoTargetSize(25))),
        );
        let r = build_request(JobId(3), &p, &g, None, Quality::High).unwrap();
        assert_eq!(r.files, vec![PathBuf::from("b.mp4"), PathBuf::from("c.mkv")]);
        // Compress works on all three kinds.
        let p = pick_op(&["a.png", "b.mp4", "d.mp3", "e.wav"], None, op_item(Operation::Tool(ToolKind::Compress)));
        let r = build_request(JobId(3), &p, &g, None, Quality::High).unwrap();
        assert_eq!(r.files.len(), 3);
        // Engine missing: no job.
        let vips_only = FormatGraph::with_available(&[EngineId::Vips]);
        let p = pick_op(&["b.mp4"], None, op_item(Operation::Tool(ToolKind::Compress)));
        assert!(build_request(JobId(3), &p, &vips_only, None, Quality::High).is_none());
    }

    #[test]
    fn zip_takes_every_item_even_folders_and_unknown_files() {
        let g = FormatGraph::with_available(&[]);
        let p = pick_op(
            &[r"C:\a\Photos", r"C:\a\notes.xyz", r"C:\a\x.png"],
            None,
            op_item(Operation::Action(ActionKind::Zip)),
        );
        let r = build_request(JobId(4), &p, &g, Some(Path::new(r"D:\o")), Quality::High).unwrap();
        assert_eq!(r.files.len(), 3);
        assert_eq!(r.op, Operation::Action(ActionKind::Zip));
        assert_eq!(r.out_dir, Some(PathBuf::from(r"D:\o")));
        // Empty pick: no job.
        let p = pick_op(&[], None, op_item(Operation::Action(ActionKind::Zip)));
        assert!(build_request(JobId(4), &p, &g, None, Quality::High).is_none());
    }

    #[test]
    fn shell_actions_are_not_jobs() {
        let g = FormatGraph::new();
        for a in [
            ActionKind::CopyPath,
            ActionKind::CopyFiles,
            ActionKind::CopyTo,
            ActionKind::MoveTo,
            ActionKind::TerminalHere,
        ] {
            let p = pick_op(&["a.png"], None, op_item(Operation::Action(a)));
            assert!(build_request(JobId(1), &p, &g, None, Quality::High).is_none(), "{a:?}");
        }
    }

    #[test]
    fn kind_of_looks_at_the_disk_only_for_unknown_extensions() {
        let calls = std::cell::Cell::new(0);
        let is_dir = |p: &Path| {
            calls.set(calls.get() + 1);
            p.to_string_lossy().ends_with("Folder")
        };
        assert_eq!(kind_of(Path::new(r"C:\a\x.png"), is_dir), InputKind::File(Some(FormatId::Png)));
        assert_eq!(calls.get(), 0);
        assert_eq!(kind_of(Path::new(r"C:\a\My Folder"), is_dir), InputKind::Folder);
        assert_eq!(kind_of(Path::new(r"C:\a\notes.xyz"), is_dir), InputKind::File(None));
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn model_has_conversions_tools_and_actions() {
        let g = FormatGraph::new();
        let paths: Vec<PathBuf> = ["a.png", "b.jpg", "c.xyz"].iter().map(PathBuf::from).collect();
        let (model, n) = model_for_with(&g, &paths, &WheelPrefs::default(), |_| false).unwrap();
        assert_eq!(n, 3);
        assert_eq!(model.skipped(), 1); // c.xyz: unknown file, actions only
        let WheelModel::Flat { items, .. } = model else { panic!("flat expected") };
        assert!(items.iter().any(|e| matches!(e, WheelEntry::Group { label: "Tools", .. })));
        assert!(items.iter().any(|e| matches!(e, WheelEntry::Group { label: "Actions", .. })));
    }

    #[test]
    fn folders_and_missing_engines_still_get_actions() {
        let g = FormatGraph::new();
        let (model, n) = model_for_with(&g, &[PathBuf::from(r"C:\a\Folder")], &WheelPrefs::default(), |_| true).unwrap();
        assert_eq!(n, 1);
        let WheelModel::Flat { items, .. } = model else { panic!() };
        assert!(items.len() <= 8);
        let actions: Vec<Operation> = items.iter().flat_map(|entry| match entry {
            WheelEntry::Item(item) => vec![item.op],
            WheelEntry::More(items) => items.iter().map(|item| item.op).collect(),
            other => panic!("action items expected, got {other:?}"),
        }).collect();
        assert_eq!(actions, ActionKind::ALL.map(Operation::Action));
        // No engine at all: the actions remain.
        let empty = FormatGraph::with_available(&[]);
        let paths = vec![PathBuf::from("a.png")];
        assert!(model_for_with(&empty, &paths, &WheelPrefs::default(), |_| false).is_some());
        // Nothing selected: no wheel.
        assert!(model_for_with(&g, &[], &WheelPrefs::default(), |_| false).is_none());
    }
}
