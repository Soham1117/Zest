//! Route planner: multi-hop conversions over the edges of the format graph.
//!
//! A route is a chain of at most [`MAX_HOPS`] edges, found with Dijkstra on the
//! states `(format, hops used)`. Cost of an edge: 1.0, +0.5 if it is lossy, and
//! a tiny engine tie-break (Vips < Ffmpeg < Pdfium < Pandoc < LibreOffice).
//! A direct edge always wins: it costs at most 1.5 and any chain costs at least 2.0.
//!
//! Rules:
//! - A lossless target is never reached through a lossy step (no PNG -> JPG -> TIFF).
//!   So lossless targets are searched on the lossless edges only.
//! - HEIC, HEIF and GIF are never intermediate formats.
//! - Categories do not mix, except two bridges: Video -> Audio (extract) and
//!   PDF -> image pages (Pdfium). Document -> PDF -> pages is the second bridge.
//! - The PDF pages step writes a folder, so it can only be the last step.

use crate::format::{Category, FormatId};
use crate::graph::{Edge, EngineId};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

/// Most edges in one route.
pub const MAX_HOPS: usize = 3;

/// A chain of conversions: `steps[0].from -> ... -> steps[n-1].to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub steps: Vec<Edge>,
}

impl Route {
    /// Number of steps.
    pub fn hops(&self) -> usize {
        self.steps.len()
    }

    /// Input format of the first step.
    pub fn from(&self) -> Option<FormatId> {
        self.steps.first().map(|e| e.from)
    }

    /// Output format of the last step.
    pub fn to(&self) -> Option<FormatId> {
        self.steps.last().map(|e| e.to)
    }

    /// True if any step is lossy.
    pub fn lossy(&self) -> bool {
        self.steps.iter().any(|e| e.lossy)
    }

    /// Planner cost (1.0 per step, +0.5 per lossy step, engine tie-break).
    pub fn cost(&self) -> f32 {
        self.steps.iter().map(edge_cost).sum::<u32>() as f32 / 1000.0
    }
}

fn engine_rank(e: EngineId) -> u32 {
    match e {
        EngineId::Vips => 0,
        EngineId::Ffmpeg => 1,
        EngineId::Pdfium => 2,
        EngineId::Pandoc => 3,
        EngineId::LibreOffice => 4,
    }
}

/// Cost in thousandths.
fn edge_cost(e: &Edge) -> u32 {
    1000 + if e.lossy { 500 } else { 0 } + engine_rank(e.engine)
}

/// Formats that may not be the middle of a route.
fn banned_intermediate(f: FormatId) -> bool {
    matches!(f, FormatId::Heic | FormatId::Heif | FormatId::Gif)
}

/// Whether a step of a chain (not the first step) may cross from one category to another.
fn step_allowed(e: &Edge) -> bool {
    let (a, b) = (e.from.category(), e.to.category());
    a == b
        || (a == Category::Video && b == Category::Audio)
        || (e.engine == EngineId::Pdfium && e.from == FormatId::Pdf)
}

/// All best routes of one engine set.
#[derive(Debug, Default)]
pub(crate) struct RouteTable {
    /// Best route per `(from, to)`. Direct edges are included.
    routes: HashMap<(FormatId, FormatId), Route>,
    /// Targets reached only by chains of two or more steps, best first.
    via: HashMap<FormatId, Vec<FormatId>>,
}

impl RouteTable {
    pub fn get(&self, from: FormatId, to: FormatId) -> Option<&Route> {
        self.routes.get(&(from, to))
    }

    pub fn via_targets(&self, from: FormatId) -> &[FormatId] {
        self.via.get(&from).map_or(&[], |v| v.as_slice())
    }

    /// Plan the routes for every input format.
    pub fn build(edges: &HashMap<FormatId, Vec<Edge>>) -> RouteTable {
        let mut table = RouteTable::default();
        for source in FormatId::ALL {
            if edges.get(&source).is_none_or(|v| v.is_empty()) {
                continue;
            }
            let mut found: Vec<(u32, FormatId, Route)> = Vec::new();
            for lossless_only in [true, false] {
                for (to, cost, route) in search(edges, source, lossless_only) {
                    found.push((cost, to, route));
                }
            }
            let mut via: Vec<(u32, usize, FormatId)> = Vec::new();
            for (cost, to, route) in found {
                if route.hops() >= 2 {
                    let order = FormatId::ALL.iter().position(|&f| f == to).unwrap_or(0);
                    via.push((cost, order, to));
                }
                table.routes.insert((source, to), route);
            }
            via.sort();
            if !via.is_empty() {
                table.via.insert(source, via.into_iter().map(|v| v.2).collect());
            }
        }
        table
    }
}

struct Node {
    fmt: FormatId,
    hops: usize,
    cost: u32,
    parent: Option<usize>,
    edge: Option<Edge>,
}

/// Dijkstra from `source`. With `lossless_only` only lossless edges are used and
/// the results are lossless targets; otherwise all edges are used and the
/// results are the lossy targets.
fn search(
    edges: &HashMap<FormatId, Vec<Edge>>,
    source: FormatId,
    lossless_only: bool,
) -> Vec<(FormatId, u32, Route)> {
    let mut arena: Vec<Node> = vec![Node {
        fmt: source,
        hops: 0,
        cost: 0,
        parent: None,
        edge: None,
    }];
    let mut heap: BinaryHeap<Reverse<(u32, usize, usize)>> = BinaryHeap::new(); // (cost, seq, node)
    heap.push(Reverse((0, 0, 0)));
    let mut settled: HashSet<(FormatId, usize)> = HashSet::new();
    // best per target: (cost, hops, node)
    let mut best: HashMap<FormatId, (u32, usize, usize)> = HashMap::new();
    let mut seq = 1usize;
    while let Some(Reverse((_, _, idx))) = heap.pop() {
        let (fmt, hops, cost) = (arena[idx].fmt, arena[idx].hops, arena[idx].cost);
        if !settled.insert((fmt, hops)) {
            continue;
        }
        for e in edges.get(&fmt).map_or(&[][..], |v| v.as_slice()) {
            if e.to == source || hops + 1 > MAX_HOPS {
                continue;
            }
            if lossless_only && e.lossy {
                continue;
            }
            // The first step is a plain edge of the graph; later steps follow the bridge rules.
            if hops > 0 && !step_allowed(e) {
                continue;
            }
            let c = cost + edge_cost(e);
            let node = arena.len();
            arena.push(Node {
                fmt: e.to,
                hops: hops + 1,
                cost: c,
                parent: Some(idx),
                edge: Some(*e),
            });
            if lossless_only || e.lossy {
                let better = best
                    .get(&e.to)
                    .is_none_or(|&(bc, bh, _)| (c, hops + 1) < (bc, bh));
                if better {
                    best.insert(e.to, (c, hops + 1, node));
                }
            }
            let can_expand = !banned_intermediate(e.to)
                && e.engine != EngineId::Pdfium
                && hops + 1 < MAX_HOPS;
            if can_expand {
                heap.push(Reverse((c, seq, node)));
                seq += 1;
            }
        }
    }
    let mut out = Vec::new();
    for (to, (cost, _, node)) in best {
        let mut steps = Vec::new();
        let mut cur = Some(node);
        while let Some(i) = cur {
            if let Some(e) = arena[i].edge {
                steps.push(e);
            }
            cur = arena[i].parent;
        }
        steps.reverse();
        out.push((to, cost, Route { steps }));
    }
    out.sort_by_key(|(to, c, _)| (*c, *to));
    out
}

#[cfg(test)]
mod tests {
    use crate::format::FormatId::*;
    use crate::format::{Category, FormatId};
    use crate::graph::{EngineId, FormatGraph, InputKind, WheelEntry, WheelModel, WheelPrefs};
    use crate::ops::Operation;

    fn path(g: &FormatGraph, a: FormatId, b: FormatId) -> Vec<FormatId> {
        let r = g.route(a, b).unwrap_or_else(|| panic!("no route {a:?} -> {b:?}"));
        let mut v = vec![a];
        v.extend(r.steps.iter().map(|e| e.to));
        v
    }

    fn convert_targets(m: WheelModel) -> Vec<FormatId> {
        let WheelModel::Flat { items, .. } = m else {
            panic!("expected a flat wheel")
        };
        items
            .iter()
            .flat_map(|e| match e {
                WheelEntry::Item(i) => vec![i.op],
                WheelEntry::More(v) => v.iter().map(|i| i.op).collect(),
                _ => vec![],
            })
            .filter_map(|op| match op {
                Operation::Convert(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn direct_edge_wins() {
        let g = FormatGraph::new();
        for (a, b) in [(Png, Jpeg), (Heic, Tiff), (Mp4, Mp3), (Docx, Pdf), (Pdf, Png), (Mkv, Gif)] {
            let r = g.route(a, b).unwrap();
            assert_eq!(r.hops(), 1, "{a:?} -> {b:?}");
            assert_eq!(r.steps[0], *g.edge(a, b).unwrap());
        }
        assert!(g.route(Png, Png).is_none());
    }

    #[test]
    fn disabled_direct_edge_uses_a_chain() {
        let g = FormatGraph::new().without_edge(Jpeg, Webp);
        assert!(g.edge(Jpeg, Webp).is_none());
        assert_eq!(path(&g, Jpeg, Webp), vec![Jpeg, Png, Webp]);
        assert_eq!(g.route_hops(Jpeg, Webp), Some(2));
        assert!(g.multi_hop_targets(Jpeg).contains(&Webp));
        // The wheel shows it after the direct targets.
        let m = g.wheel_model(&[InputKind::File(Some(Jpeg))], &WheelPrefs::default());
        assert_eq!(convert_targets(m), vec![Png, Avif, Tiff, Webp]);
    }

    #[test]
    fn lossless_target_never_goes_through_a_lossy_step() {
        // PNG -> TIFF has no lossless detour: every other image output is lossy.
        let g = FormatGraph::new().without_edge(Png, Tiff);
        assert!(g.route(Png, Tiff).is_none());
        // Same for a lossless audio target: MP4 -> FLAC goes through WAV, not MP3.
        let g = FormatGraph::new();
        assert_eq!(path(&g, Mp4, Flac), vec![Mp4, Wav, Flac]);
        for a in FormatId::ALL {
            for t in FormatId::ALL {
                if let Some(r) = g.route(a, t) {
                    if !r.steps.last().unwrap().lossy {
                        assert!(r.steps.iter().all(|e| !e.lossy), "{a:?} -> {t:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn lossy_chain_is_allowed_for_a_lossy_target() {
        let g = FormatGraph::new();
        let r = g.route(Mp4, M4a).unwrap();
        assert_eq!(r.hops(), 2);
        assert_eq!(r.steps[0].to, Wav); // lossless extract first, cheaper than MP3
        assert!(r.lossy());
        assert!(r.cost() > 2.0 && r.cost() < 3.0);
    }

    #[test]
    fn intermediates_are_never_heic_heif_or_gif() {
        let g = FormatGraph::new();
        for a in FormatId::ALL {
            for b in FormatId::ALL {
                if let Some(r) = g.route(a, b) {
                    for e in &r.steps[..r.steps.len() - 1] {
                        assert!(!matches!(e.to, Heic | Heif | Gif), "{a:?} -> {b:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn max_three_hops() {
        let g = FormatGraph::new();
        let mut longest = 0;
        for a in FormatId::ALL {
            for b in FormatId::ALL {
                if let Some(r) = g.route(a, b) {
                    assert!(r.hops() <= 3, "{a:?} -> {b:?}");
                    longest = longest.max(r.hops());
                }
            }
        }
        assert_eq!(longest, 3);
        // EPUB -> PDF: Epub -> Md -> Docx -> Pdf (three steps).
        assert_eq!(path(&g, Epub, Pdf), vec![Epub, Md, Docx, Pdf]);
        // Four steps is too many: EPUB -> Md -> Docx -> Pdf -> PNG.
        assert!(g.route(Epub, Png).is_none());
    }

    #[test]
    fn category_bridges() {
        let g = FormatGraph::new();
        // Document -> PDF -> image pages.
        assert_eq!(path(&g, Docx, Png), vec![Docx, Pdf, Png]);
        assert_eq!(path(&g, Pptx, Jpeg), vec![Pptx, Pdf, Jpeg]);
        // Markdown -> DOCX -> PDF (Pandoc + LibreOffice).
        let r = g.route(Md, Pdf).unwrap();
        assert_eq!(r.steps[0].engine, EngineId::Pandoc);
        assert_eq!(r.steps[1].engine, EngineId::LibreOffice);
        assert_eq!(r.steps[1].to, Pdf);
        // Video -> audio extract, then audio -> audio.
        assert!(g.route(Mov, Opus).is_some());
        // No other crossing.
        for a in FormatId::ALL {
            for t in g.multi_hop_targets(a) {
                let ok = a.category() == t.category()
                    || (a.category() == Category::Video && t.category() == Category::Audio)
                    || (a.category() == Category::Document && t.category() == Category::Image);
                assert!(ok, "{a:?} -> {t:?}");
            }
        }
        assert!(g.route(Png, Pdf).is_none());
        assert!(g.route(Mp3, Mp4).is_none());
        assert!(g.route(Pdf, Docx).is_none());
        // Pages are a folder: nothing can follow them.
        assert!(g.route(Pdf, Webp).is_none());
    }

    #[test]
    fn unavailable_engine_removes_routes() {
        let no_lo = FormatGraph::with_available(&[
            EngineId::Vips,
            EngineId::Ffmpeg,
            EngineId::Pdfium,
            EngineId::Pandoc,
        ]);
        assert!(no_lo.route(Docx, Png).is_none());
        assert!(no_lo.route(Md, Pdf).is_none());
        assert!(no_lo.route(Md, Html).is_some());
        let no_pdfium = FormatGraph::with_available(&[EngineId::LibreOffice]);
        assert!(no_pdfium.route(Docx, Png).is_none());
        let no_ffmpeg = FormatGraph::with_available(&[EngineId::Vips]);
        assert!(no_ffmpeg.route(Mp4, Flac).is_none());
        assert!(FormatGraph::with_available(&[]).route(Png, Jpeg).is_none());
        // Routes are cached per set but stay independent.
        assert!(FormatGraph::new().route(Docx, Png).is_some());
    }

    #[test]
    fn engine_tie_break_is_stable() {
        // Md -> Pdf: Docx and Odt are both possible middles; the result must not change.
        let a = FormatGraph::new().route(Md, Pdf).unwrap();
        for _ in 0..5 {
            assert_eq!(FormatGraph::new().without_edge(Png, Gif).route(Md, Pdf).unwrap(), a);
        }
        assert_eq!(a.steps[0].to, Docx);
    }

    #[test]
    fn wheel_lists_multi_hop_targets_after_direct_ones() {
        let g = FormatGraph::new();
        let m = g.wheel_model(&[InputKind::File(Some(Docx))], &WheelPrefs::default());
        let targets = convert_targets(m);
        assert_eq!(&targets[..4], &[Pdf, Odt, Md, Html]);
        assert!(targets.contains(&Png) && targets.contains(&Epub));
        assert_eq!(g.route_hops(Docx, Png), Some(2));
        assert_eq!(g.route_hops(Docx, Pdf), Some(1));
        // A mixed selection keeps only what every file reaches.
        let m = g.wheel_model(
            &[InputKind::File(Some(Docx)), InputKind::File(Some(Md))],
            &WheelPrefs::default(),
        );
        let all = convert_targets(m);
        assert!(all.contains(&Pdf));
        assert!(!all.contains(&Docx)); // Docx is an input
    }

    #[test]
    fn multi_hop_items_carry_the_arrow() {
        let g = FormatGraph::new();
        let m = g.wheel_model(&[InputKind::File(Some(Docx))], &WheelPrefs::default());
        let crate::graph::WheelModel::Flat { items, .. } = m else { panic!("flat") };
        let label_of = |f: FormatId| {
            items.iter().find_map(|e| match e {
                crate::graph::WheelEntry::Item(i) if i.op == crate::ops::Operation::Convert(f) => Some(i.label),
                crate::graph::WheelEntry::More(v) => {
                    v.iter().find(|i| i.op == crate::ops::Operation::Convert(f)).map(|i| i.label)
                }
                _ => None,
            })
        };
        assert_eq!(label_of(Pdf), Some("PDF"));
        let png = label_of(Png).or_else(|| label_of(Epub)).expect("a routed target");
        assert!(png.ends_with('\u{219D}'), "{png}");
        assert_eq!(Png.routed_label(), "PNG \u{219D}");
    }
}
