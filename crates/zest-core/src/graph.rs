//! Static conversion graph and wheel model builder. Cheap enough for the UI thread.

use crate::format::{Category, FormatId};
use crate::ops::{ActionKind, Operation, ToolKind};
use crate::route::{Route, RouteTable};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// Maximum sectors on one ring (spec 1.6).
pub const MAX_SECTORS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EngineId {
    Vips,
    Ffmpeg,
    /// `zest-worker.exe` with PDFium: PDF pages to images, PDF merge.
    Pdfium,
    /// `soffice.exe`: Office documents. Optional engine.
    LibreOffice,
    /// `pandoc.exe`: Markdown, HTML, EPUB, DOCX. Optional engine.
    Pandoc,
}

impl EngineId {
    /// All engines.
    pub const ALL: [EngineId; 5] = [
        EngineId::Vips,
        EngineId::Ffmpeg,
        EngineId::Pdfium,
        EngineId::LibreOffice,
        EngineId::Pandoc,
    ];

    /// Name for logs and messages.
    pub fn name(self) -> &'static str {
        match self {
            EngineId::Vips => "libvips",
            EngineId::Ffmpeg => "FFmpeg",
            EngineId::Pdfium => "PDFium",
            EngineId::LibreOffice => "LibreOffice",
            EngineId::Pandoc => "Pandoc",
        }
    }
}

/// One directed conversion `from -> to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    pub from: FormatId,
    pub to: FormatId,
    pub engine: EngineId,
    pub lossy: bool,
}

use FormatId::*;

const IMAGE_IN: [FormatId; 9] = [Png, Jpeg, Webp, Bmp, Tiff, Gif, Heic, Heif, Avif];
const IMAGE_OUT: [FormatId; 5] = [Png, Jpeg, Webp, Avif, Tiff];
const VIDEO_IN: [FormatId; 5] = [Mp4, Mov, Mkv, Webm, Avi];
const VIDEO_OUT: [FormatId; 6] = [Mp4, Webm, Mkv, Gif, Mp3, Wav];
const AUDIO_IN: [FormatId; 6] = [Mp3, Wav, Flac, M4a, Ogg, Opus];
const AUDIO_OUT: [FormatId; 5] = [Mp3, Wav, Flac, M4a, Opus];
/// PDF pages as images (one folder per PDF).
const PDF_OUT: [FormatId; 2] = [Png, Jpeg];
// LibreOffice (only when installed).
const WRITER_IN: [FormatId; 5] = [Docx, Doc, Odt, Rtf, Txt];
const WRITER_OUT: [FormatId; 3] = [Pdf, Docx, Odt];
const IMPRESS_IN: [FormatId; 3] = [Pptx, Ppt, Odp];
const IMPRESS_OUT: [FormatId; 2] = [Pdf, Pptx];
const CALC_IN: [FormatId; 4] = [Xlsx, Xls, Ods, Csv];
const CALC_OUT: [FormatId; 3] = [Pdf, Xlsx, Csv];

/// Output formats that lose information when written.
fn target_is_lossy(to: FormatId) -> bool {
    matches!(
        to,
        Jpeg | Webp | Avif | Gif | Mp4 | Webm | Mp3 | M4a | Opus | Ogg
    )
}

/// What the user may pick on the wheel: a conversion, a tool or an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WheelItem {
    pub op: Operation,
    pub label: &'static str,
    pub lossy: bool,
    /// Engine that runs it. `None` for actions.
    pub engine: Option<EngineId>,
    /// Override job quality (Compress High/Medium/Small sub-ring).
    pub quality: Option<crate::Quality>,
}

impl WheelItem {
    /// Target format of a conversion item.
    pub fn target(&self) -> Option<FormatId> {
        self.op.target()
    }

    fn action(a: ActionKind) -> WheelItem {
        WheelItem {
            op: Operation::Action(a),
            label: a.label(),
            lossy: false,
            engine: None,
            quality: None,
        }
    }
}

impl From<&Edge> for WheelItem {
    fn from(e: &Edge) -> Self {
        WheelItem {
            op: Operation::Convert(e.to),
            label: e.to.label(),
            lossy: e.lossy,
            engine: Some(e.engine),
            quality: None,
        }
    }
}

/// Label of the Tools group sector (the renderer adds the arrow).
pub const TOOLS_LABEL: &str = "Tools";
/// Label of the Actions group sector.
pub const ACTIONS_LABEL: &str = "Actions";

/// A sector on the wheel: a direct item, "More" holding the overflow, or a
/// named group ("Tools", "Actions") that opens a sub-ring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WheelEntry {
    Item(WheelItem),
    More(Vec<WheelItem>),
    Group {
        label: &'static str,
        items: Vec<WheelItem>,
    },
}

/// One input of the wheel: a file (format from its extension, `None` if
/// unknown) or a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    File(Option<FormatId>),
    Folder,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WheelGroup {
    pub category: Category,
    /// Files of this category that have targets.
    pub file_count: usize,
    pub items: Vec<WheelEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WheelModel {
    /// One category: conversions, then the "Tools" and "Actions" groups.
    /// Selections with no convertible file (folders, unknown files): the actions only.
    /// `items` is empty only for an empty selection.
    Flat {
        items: Vec<WheelEntry>,
        skipped: usize,
    },
    /// Several categories: one sector per category, then one "Actions"
    /// sector (`actions`, works on every item).
    Groups {
        groups: Vec<WheelGroup>,
        actions: Vec<WheelItem>,
        skipped: usize,
    },
}

impl WheelModel {
    /// Items that have no conversion target (they still work with actions).
    pub fn skipped(&self) -> usize {
        match self {
            WheelModel::Flat { skipped, .. } | WheelModel::Groups { skipped, .. } => *skipped,
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            WheelModel::Flat { items, .. } => items.is_empty(),
            WheelModel::Groups { groups, .. } => groups.is_empty(),
        }
    }
}

/// User preferences that change the order of wheel items.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WheelPrefs {
    /// input format -> last picked target
    pub last_used: HashMap<FormatId, FormatId>,
    /// input format -> favorite target
    pub favorites: HashMap<FormatId, FormatId>,
}

/// Precomputed graph: input format -> edges (in table order), plus the
/// multi-hop routes (planned once per engine set).
#[derive(Debug, Clone)]
pub struct FormatGraph {
    edges: HashMap<FormatId, Vec<Edge>>,
    engines: Vec<EngineId>,
    routes: Arc<RouteTable>,
}

/// Route tables by engine set (bit i = `EngineId::ALL[i]`), so the planner runs once per set.
fn cached_routes(engines: &[EngineId], edges: &HashMap<FormatId, Vec<Edge>>) -> Arc<RouteTable> {
    static CACHE: OnceLock<Mutex<HashMap<u8, Arc<RouteTable>>>> = OnceLock::new();
    let mask = EngineId::ALL
        .iter()
        .enumerate()
        .filter(|(_, e)| engines.contains(e))
        .fold(0u8, |m, (i, _)| m | (1 << i));
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    Arc::clone(
        cache
            .entry(mask)
            .or_insert_with(|| Arc::new(RouteTable::build(edges))),
    )
}

impl Default for FormatGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl FormatGraph {
    /// Graph with all engines available.
    pub fn new() -> Self {
        Self::with_available(&EngineId::ALL)
    }

    /// Graph with only the edges whose engine is in `engines`.
    pub fn with_available(engines: &[EngineId]) -> Self {
        let mut edges: HashMap<FormatId, Vec<Edge>> = HashMap::new();
        let mut add = |inputs: &[FormatId], outputs: &[FormatId], engine: EngineId| {
            if !engines.contains(&engine) {
                return;
            }
            for &from in inputs {
                let list = edges.entry(from).or_default();
                for &to in outputs {
                    if to != from {
                        list.push(Edge {
                            from,
                            to,
                            engine,
                            lossy: target_is_lossy(to),
                        });
                    }
                }
            }
        };
        add(&IMAGE_IN, &IMAGE_OUT, EngineId::Vips);
        add(&VIDEO_IN, &VIDEO_OUT, EngineId::Ffmpeg);
        add(&AUDIO_IN, &AUDIO_OUT, EngineId::Ffmpeg);
        add(&[Pdf], &PDF_OUT, EngineId::Pdfium);
        add(&WRITER_IN, &WRITER_OUT, EngineId::LibreOffice);
        add(&IMPRESS_IN, &IMPRESS_OUT, EngineId::LibreOffice);
        add(&CALC_IN, &CALC_OUT, EngineId::LibreOffice);
        add(&[Md], &[Docx, Html, Epub, Odt], EngineId::Pandoc);
        add(&[Docx], &[Md, Html], EngineId::Pandoc);
        add(&[Html], &[Md, Docx], EngineId::Pandoc);
        add(&[Epub], &[Md, Html], EngineId::Pandoc);
        let routes = cached_routes(engines, &edges);
        FormatGraph {
            edges,
            engines: engines.to_vec(),
            routes,
        }
    }

    /// Copy of this graph without the direct edge `from -> to`; the routes are
    /// planned again. For tests and diagnostics (to force a multi-hop route).
    pub fn without_edge(&self, from: FormatId, to: FormatId) -> FormatGraph {
        let mut g = self.clone();
        if let Some(list) = g.edges.get_mut(&from) {
            list.retain(|e| e.to != to);
        }
        g.routes = Arc::new(RouteTable::build(&g.edges));
        g
    }

    /// Best conversion route `from -> to`. A direct edge always wins; otherwise
    /// the cheapest chain of at most 3 steps (see the `route` module for the rules).
    pub fn route(&self, from: FormatId, to: FormatId) -> Option<Route> {
        if let Some(e) = self.edge(from, to) {
            return Some(Route { steps: vec![*e] });
        }
        self.routes.get(from, to).cloned()
    }

    /// Number of steps of the best route (1 = direct edge), or `None`.
    /// The overlay can mark items with more than one step (an arrow like "~>").
    pub fn route_hops(&self, from: FormatId, to: FormatId) -> Option<usize> {
        if self.edge(from, to).is_some() {
            return Some(1);
        }
        self.routes.get(from, to).map(Route::hops)
    }

    /// Targets that `from` reaches only through two or more steps, best first.
    pub fn multi_hop_targets(&self, from: FormatId) -> &[FormatId] {
        self.routes.via_targets(from)
    }

    /// Whether an engine is available in this graph.
    pub fn has_engine(&self, e: EngineId) -> bool {
        self.engines.contains(&e)
    }

    /// Whether `tool` can run on a file of format `f` with the available engines.
    pub fn tool_available(&self, tool: ToolKind, f: FormatId) -> bool {
        tool.applies_to(f) && self.has_engine(tool.engine(f))
    }

    /// Edges that start at `from`, in table order.
    pub fn targets(&self, from: FormatId) -> &[Edge] {
        self.edges.get(&from).map_or(&[], |v| v.as_slice())
    }

    /// The edge `from -> to`, if present.
    pub fn edge(&self, from: FormatId, to: FormatId) -> Option<&Edge> {
        self.targets(from).iter().find(|e| e.to == to)
    }

    /// Build the wheel for a selection (one entry per item).
    ///
    /// - One category: the conversions valid for all files, a "Tools" group
    ///   (tools valid for all files) and an "Actions" group. At most 8
    ///   sectors: if conversions and groups do not fit, the conversions
    ///   overflow into "More".
    /// - Several categories: one group per category (conversions only) and an "Actions" group.
    /// - Folders and unknown files only: the actions, directly on the ring.
    pub fn wheel_model(&self, items: &[InputKind], prefs: &WheelPrefs) -> WheelModel {
        let mut skipped = 0usize;
        let mut counts = [0usize; CATEGORIES.len()];
        let mut distinct: [Vec<FormatId>; CATEGORIES.len()] =
            [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        for &k in items {
            match k {
                InputKind::File(Some(f)) if !self.targets(f).is_empty() => {
                    let i = cat_index(f.category());
                    counts[i] += 1;
                    if !distinct[i].contains(&f) {
                        distinct[i].push(f);
                    }
                }
                _ => skipped += 1,
            }
        }
        if items.is_empty() {
            return WheelModel::Flat {
                items: Vec::new(),
                skipped: 0,
            };
        }
        let actions: Vec<WheelItem> = ActionKind::ALL.iter().map(|&a| WheelItem::action(a)).collect();
        let present = counts.iter().filter(|&&c| c > 0).count();
        if present == 0 {
            // Actions alone: still capped at 8 sectors (overflow → More).
            return WheelModel::Flat {
                items: fit_ring(actions, Vec::new()),
                skipped,
            };
        }
        if present == 1 {
            let i = (0..CATEGORIES.len()).find(|&i| counts[i] > 0).unwrap_or(0);
            let conversions = self.ranked_items(&distinct[i], prefs);
            let mut groups: Vec<WheelEntry> = Vec::new();
            let mut tools = self.tool_items(&distinct[i]);
            // Merge PDFs: only for two or more PDFs and nothing else.
            if distinct[i] == [Pdf] && counts[i] >= 2 && self.has_engine(EngineId::Pdfium) {
                tools.push(WheelItem {
                    op: Operation::Tool(ToolKind::MergePdf),
                    label: ToolKind::MergePdf.label(),
                    lossy: false,
                    engine: Some(EngineId::Pdfium),
                    quality: None,
                });
            }
            if distinct[i] == [Pdf] && counts[i] >= 1 && self.has_engine(EngineId::Pdfium) {
                for t in [ToolKind::SplitPdf, ToolKind::CompressPdf] {
                    tools.push(WheelItem {
                        op: Operation::Tool(t),
                        label: t.label(),
                        lossy: false,
                        engine: Some(EngineId::Pdfium),
                        quality: None,
                    });
                }
            }
            // Trim and Crop: one file only (a dialog asks for the range or the rectangle).
            if counts[i] == 1 && skipped == 0 {
                for item in [ToolKind::TRIM_ITEM, ToolKind::CROP_ITEM] {
                    if distinct[i].iter().all(|&f| self.tool_available(item, f)) {
                        tools.push(WheelItem {
                            op: Operation::Tool(item),
                            label: item.label(),
                            lossy: distinct[i].iter().any(|&f| item.lossy_for(f)),
                            engine: distinct[i].first().map(|&f| item.engine(f)),
                            quality: None,
                        });
                    }
                }
            }
            // Copy as PNG / JPG: only when every item is an image.
            let mut actions = actions;
            if CATEGORIES[i] == Category::Image && skipped == 0 && self.has_engine(EngineId::Vips) {
                // After "Copy files", before "Copy to...".
                let at = actions
                    .iter()
                    .position(|a| a.op == Operation::Action(ActionKind::CopyTo))
                    .unwrap_or(actions.len());
                for (k, a) in ActionKind::COPY_AS.iter().enumerate() {
                    actions.insert(at + k, WheelItem::action(*a));
                }
            }
            if !tools.is_empty() {
                groups.push(WheelEntry::Group {
                    label: TOOLS_LABEL,
                    items: tools,
                });
            }
            groups.push(WheelEntry::Group {
                label: ACTIONS_LABEL,
                items: actions,
            });
            return WheelModel::Flat {
                items: fit_ring(conversions, groups),
                skipped,
            };
        }
        let mut groups = Vec::with_capacity(present);
        for (i, cat) in CATEGORIES.into_iter().enumerate() {
            if counts[i] == 0 {
                continue;
            }
            groups.push(WheelGroup {
                category: cat,
                file_count: counts[i],
                items: self.entries_for(&distinct[i], prefs),
            });
        }
        WheelModel::Groups {
            groups,
            actions,
            skipped,
        }
    }

    /// Tools valid for all `inputs` whose engine is available, in display order.
    /// Compress expands to High / Medium / Small (quality on the item).
    fn tool_items(&self, inputs: &[FormatId]) -> Vec<WheelItem> {
        let mut out = Vec::new();
        for &t in &ToolKind::PRESETS {
            if !inputs.iter().all(|&f| self.tool_available(t, f)) {
                continue;
            }
            let lossy = inputs.iter().any(|&f| t.lossy_for(f));
            let engine = inputs.first().map(|&f| t.engine(f));
            if t == ToolKind::Compress {
                use crate::Quality::*;
                for (label, q) in [("High", High), ("Medium", Medium), ("Small", Small)] {
                    out.push(WheelItem {
                        op: Operation::Tool(ToolKind::Compress),
                        label,
                        lossy,
                        engine,
                        quality: Some(q),
                    });
                }
            } else {
                out.push(WheelItem {
                    op: Operation::Tool(t),
                    label: t.label(),
                    lossy,
                    engine,
                    quality: None,
                });
            }
        }
        out
    }

    /// Targets valid for all `inputs`, ordered and trimmed to the ring size.
    fn entries_for(&self, inputs: &[FormatId], prefs: &WheelPrefs) -> Vec<WheelEntry> {
        fit_ring(self.ranked_items(inputs, prefs), Vec::new())
    }

    /// Whether `f` converts to `to`, directly or through a route.
    fn reaches(&self, f: FormatId, to: FormatId) -> bool {
        self.edge(f, to).is_some() || self.routes.get(f, to).is_some()
    }

    /// Conversions valid for all `inputs`: last used first, then favorite, then
    /// table order (direct targets first, then multi-hop targets).
    fn ranked_items(&self, inputs: &[FormatId], prefs: &WheelPrefs) -> Vec<WheelItem> {
        let Some((&first, rest)) = inputs.split_first() else {
            return Vec::new();
        };
        let direct = self.targets(first).iter().map(|e| (e.to, WheelItem::from(e)));
        let multi = self
            .multi_hop_targets(first)
            .iter()
            .filter(|&&to| self.edge(first, to).is_none())
            .filter_map(|&to| {
                let r = self.routes.get(first, to)?;
                let item = WheelItem {
                    op: Operation::Convert(to),
                    // The arrow marks a target that needs two or more steps.
                    label: to.routed_label(),
                    lossy: r.lossy(),
                    engine: r.steps.first().map(|e| e.engine),
                    quality: None,
                };
                Some((to, item))
            });
        let mut ranked: Vec<((u8, usize), WheelItem)> = direct
            .chain(multi)
            .filter(|(to, _)| rest.iter().all(|&f| self.reaches(f, *to)))
            .map(|(to, item)| {
                let mut rank = (2u8, 0usize);
                for (i, f) in inputs.iter().enumerate() {
                    if prefs.last_used.get(f) == Some(&to) {
                        rank = rank.min((0, i));
                    } else if prefs.favorites.get(f) == Some(&to) {
                        rank = rank.min((1, 0));
                    }
                }
                (rank, item)
            })
            .collect();
        ranked.sort_by_key(|(r, _)| *r); // stable: keeps table order inside a rank
        ranked.into_iter().map(|(_, i)| i).collect()
    }
}

/// Put `items` and the extra sectors `tail` on one ring of at most 8 sectors.
/// If they do not fit, the last items that fit are replaced by "More".
fn fit_ring(mut items: Vec<WheelItem>, tail: Vec<WheelEntry>) -> Vec<WheelEntry> {
    let budget = MAX_SECTORS.saturating_sub(tail.len());
    let mut out: Vec<WheelEntry> = Vec::new();
    if items.len() <= budget {
        out.extend(items.into_iter().map(WheelEntry::Item));
    } else {
        let more = items.split_off(budget.saturating_sub(1));
        out.extend(items.into_iter().map(WheelEntry::Item));
        out.push(WheelEntry::More(more));
    }
    out.extend(tail);
    out
}

/// Categories in wheel order.
const CATEGORIES: [Category; 4] = [
    Category::Image,
    Category::Video,
    Category::Audio,
    Category::Document,
];

fn cat_index(c: Category) -> usize {
    match c {
        Category::Image => 0,
        Category::Video => 1,
        Category::Audio => 2,
        Category::Document => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(v: &[FormatId]) -> Vec<InputKind> {
        v.iter().map(|&f| InputKind::File(Some(f))).collect()
    }

    /// Direct conversion targets of the entries.
    fn targets_of(entries: &[WheelEntry]) -> Vec<FormatId> {
        entries
            .iter()
            .filter_map(|e| match e {
                WheelEntry::Item(i) => i.target(),
                _ => None,
            })
            .collect()
    }

    fn group<'a>(entries: &'a [WheelEntry], name: &str) -> Option<&'a Vec<WheelItem>> {
        entries.iter().find_map(|e| match e {
            WheelEntry::Group { label, items } if *label == name => Some(items),
            _ => None,
        })
    }

    fn ops(items: &[WheelItem]) -> Vec<Operation> {
        items.iter().map(|i| i.op).collect()
    }

    fn flat(m: WheelModel) -> (Vec<WheelEntry>, usize) {
        match m {
            WheelModel::Flat { items, skipped } => (items, skipped),
            other => panic!("expected flat, got {other:?}"),
        }
    }

    #[test]
    fn no_self_edges_and_all_have_engine() {
        let g = FormatGraph::new();
        let mut total = 0;
        for f in FormatId::ALL {
            for e in g.targets(f) {
                assert_ne!(e.from, e.to);
                assert_eq!(e.from, f);
                total += 1;
            }
        }
        // images 40, video 27, audio 25, PDF 2, Writer 13, Impress 5, Calc 10, Pandoc 10
        assert_eq!(total, 40 + 27 + 25 + 2 + 13 + 5 + 10 + 10);
    }

    #[test]
    fn images_use_vips_media_uses_ffmpeg() {
        let g = FormatGraph::new();
        for e in g.targets(Heic) {
            assert_eq!(e.engine, EngineId::Vips);
        }
        for e in g.targets(Mov).iter().chain(g.targets(Flac)) {
            assert_eq!(e.engine, EngineId::Ffmpeg);
        }
        assert!(g.edge(Png, Jpeg).unwrap().lossy);
        assert!(!g.edge(Jpeg, Png).unwrap().lossy);
        assert!(g.edge(Mp4, Png).is_none());
        assert!(g.edge(Png, Png).is_none());
    }

    #[test]
    fn unavailable_engine_hides_edges() {
        let g = FormatGraph::with_available(&[EngineId::Vips]);
        assert!(!g.targets(Png).is_empty());
        assert!(g.targets(Mp4).is_empty());
        assert!(g.targets(Mp3).is_empty());
        assert!(g.tool_available(ToolKind::Resize50, Png));
        assert!(!g.tool_available(ToolKind::Compress, Mp4));
        let g = FormatGraph::with_available(&[]);
        assert!(g.targets(Png).is_empty());
        assert!(!g.has_engine(EngineId::Vips));
        // Nothing can be converted, but the actions still work.
        let m = g.wheel_model(&files(&[Png, Mp4]), &WheelPrefs::default());
        assert!(!m.is_empty());
        assert_eq!(m.skipped(), 2);
        let (items, _) = flat(m);
        assert_eq!(items.len(), MAX_SECTORS);
        assert!(matches!(items.last(), Some(WheelEntry::More(_))));
        assert!(items.iter().all(|e| match e {
            WheelEntry::Item(i) => matches!(i.op, Operation::Action(_)),
            WheelEntry::More(v) => v.iter().all(|i| matches!(i.op, Operation::Action(_))),
            _ => false,
        }));
    }

    #[test]
    fn single_format_has_conversions_tools_and_actions() {
        let g = FormatGraph::new();
        let (items, skipped) = flat(g.wheel_model(&files(&[Jpeg]), &WheelPrefs::default()));
        assert_eq!(skipped, 0);
        assert_eq!(targets_of(&items), vec![Png, Webp, Avif, Tiff]);
        assert_eq!(items.len(), 6); // 4 conversions + Tools + Actions
        let tools = group(&items, TOOLS_LABEL).unwrap();
        // Compress expands to High/Medium/Small; one file: Crop... comes last.
        let mut want = vec![
            Operation::Tool(ToolKind::Compress),
            Operation::Tool(ToolKind::Compress),
            Operation::Tool(ToolKind::Compress),
        ];
        want.extend(ToolKind::PRESETS[1..9].iter().map(|&t| Operation::Tool(t)));
        want.push(Operation::Tool(ToolKind::CROP_ITEM));
        assert_eq!(ops(tools), want);
        assert_eq!(tools[0].label, "High");
        assert_eq!(tools[0].quality, Some(crate::Quality::High));
        // Two files: no Crop.
        let (items2, _) = flat(g.wheel_model(&files(&[Jpeg, Jpeg]), &WheelPrefs::default()));
        let mut want2 = vec![
            Operation::Tool(ToolKind::Compress),
            Operation::Tool(ToolKind::Compress),
            Operation::Tool(ToolKind::Compress),
        ];
        want2.extend(ToolKind::PRESETS[1..9].iter().map(|&t| Operation::Tool(t)));
        assert_eq!(ops(group(&items2, TOOLS_LABEL).unwrap()), want2);
        let actions = group(&items, ACTIONS_LABEL).unwrap();
        // All items are images: "Copy as PNG / JPG" sit before "Copy to...".
        let mut want: Vec<Operation> = ActionKind::ALL.iter().map(|&a| Operation::Action(a)).collect();
        let at = want.iter().position(|o| *o == Operation::Action(ActionKind::CopyTo)).unwrap();
        want.insert(at, Operation::Action(ActionKind::CopyAs(Jpeg)));
        want.insert(at, Operation::Action(ActionKind::CopyAs(Png)));
        assert_eq!(ops(actions), want);
        assert!(actions.iter().all(|a| a.engine.is_none()));
        // Tools come before Actions, both after the conversions.
        assert!(matches!(items[4], WheelEntry::Group { label: TOOLS_LABEL, .. }));
        assert!(matches!(items[5], WheelEntry::Group { label: ACTIONS_LABEL, .. }));
    }

    #[test]
    fn heic_has_no_tools_group() {
        // HEIC cannot be written again, so no same-format tool applies.
        let g = FormatGraph::new();
        let (items, _) = flat(g.wheel_model(&files(&[Heic]), &WheelPrefs::default()));
        assert_eq!(targets_of(&items), vec![Png, Jpeg, Webp, Avif, Tiff]);
        assert!(group(&items, TOOLS_LABEL).is_none());
        assert!(group(&items, ACTIONS_LABEL).is_some());
        assert_eq!(items.len(), 6);
    }

    #[test]
    fn video_and_audio_tools() {
        let g = FormatGraph::new();
        let (items, _) = flat(g.wheel_model(&files(&[Mp4]), &WheelPrefs::default()));
        let tools = group(&items, TOOLS_LABEL).unwrap();
        assert_eq!(
            ops(tools),
            vec![
                Operation::Tool(ToolKind::Compress),
                Operation::Tool(ToolKind::Compress),
                Operation::Tool(ToolKind::Compress),
                Operation::Tool(ToolKind::StripMetadata),
                Operation::Tool(ToolKind::VideoTargetSize(10)),
                Operation::Tool(ToolKind::VideoTargetSize(25)),
                Operation::Tool(ToolKind::VideoTargetSize(50)),
                Operation::Tool(ToolKind::VideoTargetSize(0)),
                Operation::Tool(ToolKind::TRIM_ITEM),
            ]
        );
        assert!(tools.iter().all(|t| t.engine == Some(EngineId::Ffmpeg)));
        assert_eq!(tools[0].label, "High");
        assert_eq!(tools[4].label, "10 MB");
        assert!(tools[0].lossy);
        assert!(!tools[3].lossy); // Strip meta
        let (items, _) = flat(g.wheel_model(&files(&[Flac]), &WheelPrefs::default()));
        assert_eq!(
            ops(group(&items, TOOLS_LABEL).unwrap()),
            vec![
                Operation::Tool(ToolKind::Compress),
                Operation::Tool(ToolKind::Compress),
                Operation::Tool(ToolKind::Compress),
                Operation::Tool(ToolKind::StripMetadata),
                Operation::Tool(ToolKind::TRIM_ITEM)
            ]
        );
        // WAV: Strip and Trim (no Compress).
        let (items, _) = flat(g.wheel_model(&files(&[Wav]), &WheelPrefs::default()));
        assert_eq!(
            ops(group(&items, TOOLS_LABEL).unwrap()),
            vec![Operation::Tool(ToolKind::StripMetadata), Operation::Tool(ToolKind::TRIM_ITEM)]
        );
    }

    #[test]
    fn tools_must_be_valid_for_all_files() {
        let g = FormatGraph::new();
        // HEIC has targets but no tools: a HEIC + PNG selection has no Tools group.
        let (items, _) = flat(g.wheel_model(&files(&[Png, Heic]), &WheelPrefs::default()));
        assert!(group(&items, TOOLS_LABEL).is_none());
        // Compress on WAV + MP3 is not valid for all.
        let (items, _) = flat(g.wheel_model(&files(&[Mp3, Wav]), &WheelPrefs::default()));
        assert_eq!(
            ops(group(&items, TOOLS_LABEL).unwrap()),
            vec![Operation::Tool(ToolKind::StripMetadata)]
        );
    }

    #[test]
    fn intersection_for_multiple_formats() {
        let g = FormatGraph::new();
        let (items, _) = flat(g.wheel_model(&files(&[Png, Jpeg, Png]), &WheelPrefs::default()));
        // Png lacks Png, Jpeg lacks Jpeg
        assert_eq!(targets_of(&items), vec![Webp, Avif, Tiff]);
    }

    #[test]
    fn mixed_categories_make_groups_plus_actions() {
        let g = FormatGraph::new();
        let m = g.wheel_model(&files(&[Png, Jpeg, Mp4, Mp3, Png]), &WheelPrefs::default());
        let WheelModel::Groups { groups, actions, skipped } = m else {
            panic!("expected groups")
        };
        assert_eq!(skipped, 0);
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].category, Category::Image);
        assert_eq!(groups[0].file_count, 3);
        assert_eq!(targets_of(&groups[0].items), vec![Webp, Avif, Tiff]);
        assert_eq!(groups[1].category, Category::Video);
        assert_eq!(groups[1].file_count, 1);
        assert_eq!(groups[2].category, Category::Audio);
        assert_eq!(targets_of(&groups[2].items), vec![Wav, Flac, M4a, Opus]);
        // Conversions only inside the category groups.
        for gr in &groups {
            assert!(gr
                .items
                .iter()
                .all(|e| matches!(e, WheelEntry::Item(i) if i.target().is_some())));
        }
        assert_eq!(actions.len(), ActionKind::ALL.len());
        assert!(actions.iter().all(|a| matches!(a.op, Operation::Action(_))));
    }

    #[test]
    fn folders_and_unknown_files_give_actions_only() {
        let g = FormatGraph::new();
        for sel in [
            vec![InputKind::Folder],
            vec![InputKind::File(None)],
            vec![InputKind::Folder, InputKind::File(None), InputKind::Folder],
        ] {
            let n = sel.len();
            let m = g.wheel_model(&sel, &WheelPrefs::default());
            assert_eq!(m.skipped(), n);
            let (items, _) = flat(m);
            assert_eq!(items.len(), MAX_SECTORS);
            let want: Vec<Operation> = ActionKind::ALL.iter().map(|&a| Operation::Action(a)).collect();
            let mut got: Vec<Operation> = Vec::new();
            for e in &items {
                match e {
                    WheelEntry::Item(i) => got.push(i.op),
                    WheelEntry::More(v) => got.extend(v.iter().map(|i| i.op)),
                    other => panic!("action items expected, got {other:?}"),
                }
            }
            assert_eq!(got, want);
        }
    }

    #[test]
    fn folder_next_to_images_keeps_the_conversions() {
        let g = FormatGraph::new();
        let sel = [InputKind::File(Some(Png)), InputKind::Folder, InputKind::File(None)];
        let m = g.wheel_model(&sel, &WheelPrefs::default());
        assert_eq!(m.skipped(), 2);
        let (items, _) = flat(m);
        assert!(!targets_of(&items).is_empty());
        assert!(group(&items, ACTIONS_LABEL).is_some());
    }

    fn fake_edges(g: &mut FormatGraph, from: FormatId, to: &[FormatId], engine: EngineId) {
        let list: Vec<Edge> = to
            .iter()
            .map(|&to| Edge {
                from,
                to,
                engine,
                lossy: false,
            })
            .collect();
        g.edges.insert(from, list);
    }

    #[test]
    fn ring_never_exceeds_eight_sectors() {
        // 10 conversions + Tools + Actions: 5 direct, More, Tools, Actions.
        let mut g = FormatGraph::with_available(&[EngineId::Vips]);
        fake_edges(
            &mut g,
            Jpeg,
            &[Png, Jpeg, Webp, Avif, Tiff, Bmp, Gif, Heic, Heif, Mp4],
            EngineId::Vips,
        );
        let (items, _) = flat(g.wheel_model(&files(&[Jpeg]), &WheelPrefs::default()));
        assert_eq!(items.len(), MAX_SECTORS);
        assert_eq!(targets_of(&items).len(), 5);
        match &items[5] {
            WheelEntry::More(rest) => {
                assert_eq!(rest.len(), 10 - 5);
                assert_eq!(rest[0].target(), Some(Bmp));
            }
            other => panic!("sector 6 must be More, got {other:?}"),
        }
        assert!(matches!(items[6], WheelEntry::Group { label: TOOLS_LABEL, .. }));
        assert!(matches!(items[7], WheelEntry::Group { label: ACTIONS_LABEL, .. }));
    }

    #[test]
    fn exactly_fitting_conversions_have_no_more() {
        // 6 conversions + 2 groups = 8: no More.
        let mut g = FormatGraph::with_available(&[EngineId::Vips]);
        fake_edges(&mut g, Jpeg, &[Png, Webp, Avif, Tiff, Bmp, Gif], EngineId::Vips);
        let (items, _) = flat(g.wheel_model(&files(&[Jpeg]), &WheelPrefs::default()));
        assert_eq!(items.len(), 8);
        assert!(!items.iter().any(|e| matches!(e, WheelEntry::More(_))));
        // 7 conversions + 2 groups = 9: More appears and the ring stays at 8.
        fake_edges(&mut g, Jpeg, &[Png, Webp, Avif, Tiff, Bmp, Gif, Heic], EngineId::Vips);
        let (items, _) = flat(g.wheel_model(&files(&[Jpeg]), &WheelPrefs::default()));
        assert_eq!(items.len(), 8);
        assert!(items
            .iter()
            .any(|e| matches!(e, WheelEntry::More(v) if v.len() == 2)));
    }

    #[test]
    fn category_group_overflow_goes_to_more() {
        let mut g = FormatGraph::with_available(&[]);
        fake_edges(
            &mut g,
            Mov,
            &[Png, Jpeg, Webp, Avif, Tiff, Bmp, Gif, Heic, Heif, Mp4],
            EngineId::Ffmpeg,
        );
        let items = g.entries_for(&[Mov], &WheelPrefs::default());
        assert_eq!(items.len(), MAX_SECTORS);
        match items.last().unwrap() {
            WheelEntry::More(rest) => {
                assert_eq!(rest.len(), 10 - (MAX_SECTORS - 1));
                assert_eq!(rest[0].target(), Some(Heic));
            }
            _ => panic!("last must be More"),
        }
    }

    #[test]
    fn last_used_then_favorite_then_table_order() {
        let g = FormatGraph::new();
        let mut prefs = WheelPrefs::default();
        prefs.last_used.insert(Heic, Webp);
        prefs.favorites.insert(Heic, Tiff);
        let (items, _) = flat(g.wheel_model(&files(&[Heic]), &prefs));
        assert_eq!(targets_of(&items), vec![Webp, Tiff, Png, Jpeg, Avif]);
    }

    #[test]
    fn last_used_ignored_when_not_valid_target() {
        let g = FormatGraph::new();
        let mut prefs = WheelPrefs::default();
        prefs.last_used.insert(Png, Png); // never a target
        prefs.last_used.insert(Jpeg, Mp3); // wrong category
        let (items, _) = flat(g.wheel_model(&files(&[Png, Jpeg]), &prefs));
        assert_eq!(targets_of(&items), vec![Webp, Avif, Tiff]);
    }

    fn doc_targets(g: &FormatGraph, f: FormatId) -> Vec<FormatId> {
        g.targets(f).iter().map(|e| e.to).collect()
    }

    #[test]
    fn document_edges_follow_the_engines() {
        let g = FormatGraph::new();
        assert_eq!(doc_targets(&g, Pdf), vec![Png, Jpeg]);
        assert_eq!(g.edge(Pdf, Png).unwrap().engine, EngineId::Pdfium);
        assert!(g.edge(Pdf, Jpeg).unwrap().lossy);
        assert_eq!(doc_targets(&g, Docx), vec![Pdf, Odt, Md, Html]);
        assert_eq!(doc_targets(&g, Doc), vec![Pdf, Docx, Odt]);
        assert_eq!(doc_targets(&g, Txt), vec![Pdf, Docx, Odt]);
        assert_eq!(doc_targets(&g, Pptx), vec![Pdf]);
        assert_eq!(doc_targets(&g, Ppt), vec![Pdf, Pptx]);
        assert_eq!(doc_targets(&g, Odp), vec![Pdf, Pptx]);
        assert_eq!(doc_targets(&g, Xlsx), vec![Pdf, Csv]);
        assert_eq!(doc_targets(&g, Csv), vec![Pdf, Xlsx]);
        assert_eq!(doc_targets(&g, Md), vec![Docx, Html, Epub, Odt]);
        assert_eq!(doc_targets(&g, Html), vec![Md, Docx]);
        assert_eq!(doc_targets(&g, Epub), vec![Md, Html]);
        // No PDF from pandoc, nothing converts to a PDF but LibreOffice.
        for e in g.targets(Md) {
            assert_ne!(e.to, Pdf);
            assert_eq!(e.engine, EngineId::Pandoc);
        }
        assert_eq!(g.edge(Docx, Pdf).unwrap().engine, EngineId::LibreOffice);
        assert_eq!(g.edge(Docx, Md).unwrap().engine, EngineId::Pandoc);
        assert!(g.edge(Txt, Md).is_none());
        assert!(g.edge(Pdf, Docx).is_none());
    }

    #[test]
    fn optional_engines_missing_means_no_edges() {
        // Only the built-in engines: no Office or pandoc edges, PDF still works.
        let g = FormatGraph::with_available(&[EngineId::Vips, EngineId::Ffmpeg, EngineId::Pdfium]);
        assert_eq!(doc_targets(&g, Pdf), vec![Png, Jpeg]);
        for f in [Docx, Doc, Odt, Rtf, Pptx, Ppt, Odp, Xlsx, Xls, Ods, Csv, Md, Html, Txt, Epub] {
            assert!(g.targets(f).is_empty(), "{f:?}");
        }
        // Only LibreOffice: Docx converts, Md has nothing.
        let g = FormatGraph::with_available(&[EngineId::LibreOffice]);
        assert_eq!(doc_targets(&g, Docx), vec![Pdf, Odt]);
        assert!(g.targets(Md).is_empty());
        assert!(g.targets(Pdf).is_empty());
        // Only Pandoc.
        let g = FormatGraph::with_available(&[EngineId::Pandoc]);
        assert_eq!(doc_targets(&g, Docx), vec![Md, Html]);
        assert!(g.targets(Txt).is_empty());
        // A selection of Office files without any engine gives only the actions.
        let g = FormatGraph::with_available(&[EngineId::Vips]);
        let m = g.wheel_model(&files(&[Docx, Xlsx]), &WheelPrefs::default());
        assert_eq!(m.skipped(), 2);
        let (items, _) = flat(m);
        assert_eq!(items.len(), MAX_SECTORS);
        assert!(matches!(items.last(), Some(WheelEntry::More(_))));
    }

    #[test]
    fn documents_have_their_own_group_in_mixed_selections() {
        let g = FormatGraph::new();
        let m = g.wheel_model(&files(&[Png, Pdf, Pdf]), &WheelPrefs::default());
        let WheelModel::Groups { groups, actions, .. } = m else {
            panic!("expected groups")
        };
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].category, Category::Image);
        assert_eq!(groups[1].category, Category::Document);
        assert_eq!(groups[1].file_count, 2);
        assert_eq!(targets_of(&groups[1].items), vec![Png, Jpeg]);
        // No Copy as... and no Merge in mixed selections.
        assert_eq!(actions.len(), ActionKind::ALL.len());
    }

    #[test]
    fn merge_pdf_needs_two_pdfs() {
        let g = FormatGraph::new();
        let merge = Operation::Tool(ToolKind::MergePdf);
        let split = Operation::Tool(ToolKind::SplitPdf);
        let compress = Operation::Tool(ToolKind::CompressPdf);
        let (items, _) = flat(g.wheel_model(&files(&[Pdf]), &WheelPrefs::default()));
        // One PDF: Split + Compress PDF tools (no Merge).
        assert_eq!(ops(group(&items, TOOLS_LABEL).unwrap()), vec![split, compress]);
        assert_eq!(targets_of(&items), vec![Png, Jpeg]);
        let (items, _) = flat(g.wheel_model(&files(&[Pdf, Pdf]), &WheelPrefs::default()));
        assert_eq!(
            ops(group(&items, TOOLS_LABEL).unwrap()),
            vec![merge, split, compress]
        );
        // Actions of a document selection have no Copy as...
        let acts = group(&items, ACTIONS_LABEL).unwrap();
        assert_eq!(acts.len(), ActionKind::ALL.len());
        // PDF + Word is not "only PDFs".
        let (items, _) = flat(g.wheel_model(&files(&[Pdf, Docx]), &WheelPrefs::default()));
        assert!(group(&items, TOOLS_LABEL).is_none());
        // Without the PDF engine there are no PDF edges and no merge.
        let g = FormatGraph::with_available(&[EngineId::Vips]);
        let (items, _) = flat(g.wheel_model(&files(&[Pdf, Pdf]), &WheelPrefs::default()));
        assert!(group(&items, TOOLS_LABEL).is_none());
    }

    #[test]
    fn copy_as_only_when_all_items_are_images() {
        let g = FormatGraph::new();
        let copy_png = Operation::Action(ActionKind::CopyAs(Png));
        let has_copy_as = |sel: &[InputKind]| -> bool {
            let (items, _) = flat(g.wheel_model(sel, &WheelPrefs::default()));
            group(&items, ACTIONS_LABEL).is_some_and(|a| ops(a).contains(&copy_png))
        };
        assert!(has_copy_as(&files(&[Heic, Png])));
        assert!(!has_copy_as(&[InputKind::File(Some(Png)), InputKind::Folder]));
        assert!(!has_copy_as(&[InputKind::File(Some(Png)), InputKind::File(None)]));
        assert!(!has_copy_as(&files(&[Mp4])));
        assert!(!has_copy_as(&files(&[Pdf])));
        // Without libvips no image is offered at all (actions-only ring, no Copy as…).
        let g2 = FormatGraph::with_available(&[EngineId::Ffmpeg]);
        let (items, _) = flat(g2.wheel_model(&files(&[Png]), &WheelPrefs::default()));
        let no_copy_as = |i: &WheelItem| !matches!(i.op, Operation::Action(ActionKind::CopyAs(_)));
        assert!(items.iter().all(|e| match e {
            WheelEntry::Item(i) => no_copy_as(i),
            WheelEntry::More(v) => v.iter().all(no_copy_as),
            _ => true,
        }));
    }

    #[test]
    fn empty_selection_is_empty_flat() {
        let g = FormatGraph::new();
        let m = g.wheel_model(&[], &WheelPrefs::default());
        assert!(m.is_empty());
        assert_eq!(m.skipped(), 0);
    }

    #[test]
    fn items_carry_label_and_lossy() {
        let g = FormatGraph::new();
        let (items, _) = flat(g.wheel_model(&files(&[Png]), &WheelPrefs::default()));
        let WheelEntry::Item(jpg) = &items[0] else {
            panic!()
        };
        assert_eq!(jpg.target(), Some(Jpeg));
        assert_eq!(jpg.label, "JPG");
        assert!(jpg.lossy);
        assert_eq!(jpg.engine, Some(EngineId::Vips));
    }
}
