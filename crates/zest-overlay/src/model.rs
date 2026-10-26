//! Display model: turns a `zest_core::graph::WheelModel` into sectors with
//! ready-made strings. Pure (no Windows calls), unit-tested.

use crate::geometry::Hit;
use zest_core::format::Category;
use zest_core::graph::{WheelEntry, WheelItem, WheelModel, ACTIONS_LABEL, MAX_SECTORS};
use zest_core::ops::Operation;

/// What a main sector does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SectorKind {
    /// A direct target.
    Item(WheelItem),
    /// Opens a sub-ring (a category group, a "Tools" / "Actions" group, or "More").
    Expand {
        /// Index into `WheelModel::Groups` for a category sector.
        group: Option<usize>,
        category: Option<Category>,
        items: Vec<WheelItem>,
        /// Center text while this sector is hovered.
        center: String,
    },
}

/// One sector of the main ring.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectorSpec {
    pub label: String,
    /// Lossy target (shows a marker dot).
    pub lossy: bool,
    pub kind: SectorKind,
}

impl SectorSpec {
    pub fn expands(&self) -> bool {
        matches!(self.kind, SectorKind::Expand { .. })
    }

    /// Items of the sub-ring (empty for direct targets).
    pub fn sub_items(&self) -> &[WheelItem] {
        match &self.kind {
            SectorKind::Expand { items, .. } => items,
            SectorKind::Item(_) => &[],
        }
    }
}

/// Everything the renderer and the state machine need from the model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prepared {
    pub sectors: Vec<SectorSpec>,
    /// Center text when nothing special is hovered.
    pub center: String,
    pub file_count: usize,
}

impl Prepared {
    /// No model yet: the drop zone.
    pub fn empty() -> Prepared {
        Prepared { sectors: Vec::new(), center: DROP_HERE.to_owned(), file_count: 0 }
    }
}

pub const DROP_HERE: &str = "Drop files here";
pub const NOTHING: &str = "Nothing to convert";
pub const TITLE: &str = "Convert";
/// Center title when the ring holds only actions (folders, unknown files).
pub const ACTIONS_TITLE: &str = "Actions";
/// Added to the label of every sector that opens a sub-ring.
pub const ARROW: &str = " \u{203A}";

pub fn category_label(c: Category) -> &'static str {
    match c {
        Category::Image => "Images",
        Category::Video => "Videos",
        Category::Audio => "Audio",
        Category::Document => "Docs",
    }
}

pub fn files_text(n: usize) -> String {
    if n == 1 {
        "1 file".to_owned()
    } else {
        format!("{n} files")
    }
}

fn item_sector(it: &WheelItem) -> SectorSpec {
    SectorSpec { label: it.label.to_owned(), lossy: it.lossy, kind: SectorKind::Item(*it) }
}

/// Flatten entries for a sub-ring: a `More` inside a group is spread into it.
fn flatten(entries: &[WheelEntry]) -> Vec<WheelItem> {
    let mut out = Vec::new();
    for e in entries {
        match e {
            WheelEntry::Item(i) => out.push(*i),
            WheelEntry::More(v) | WheelEntry::Group { items: v, .. } => out.extend(v.iter().copied()),
        }
    }
    out
}

fn expand_sector(label: String, group: Option<usize>, category: Option<Category>, items: Vec<WheelItem>, center: String) -> SectorSpec {
    SectorSpec {
        label,
        lossy: false,
        kind: SectorKind::Expand { group, category, items, center },
    }
}

/// Build the display model. `file_count` is the number shown in the center.
pub fn prepare(model: &WheelModel, file_count: usize) -> Prepared {
    let mut sectors = Vec::new();
    let files = files_text(file_count);
    match model {
        WheelModel::Flat { items, .. } => {
            for e in items.iter().take(MAX_SECTORS) {
                sectors.push(match e {
                    WheelEntry::Item(i) => item_sector(i),
                    WheelEntry::More(v) => expand_sector(
                        format!("More{ARROW}"),
                        None,
                        None,
                        v.clone(),
                        format!("{TITLE}\n{files}"),
                    ),
                    WheelEntry::Group { label, items } => expand_sector(
                        format!("{label}{ARROW}"),
                        None,
                        None,
                        items.clone(),
                        format!("{label}\n{files}"),
                    ),
                });
            }
        }
        WheelModel::Groups { groups, actions, .. } => {
            for (gi, g) in groups.iter().take(MAX_SECTORS).enumerate() {
                let name = category_label(g.category);
                sectors.push(expand_sector(
                    format!("{name} ({}){ARROW}", g.file_count),
                    Some(gi),
                    Some(g.category),
                    flatten(&g.items),
                    format!("{name}\n{}", files_text(g.file_count)),
                ));
            }
            if !actions.is_empty() && sectors.len() < MAX_SECTORS {
                sectors.push(expand_sector(
                    format!("{ACTIONS_LABEL}{ARROW}"),
                    None,
                    None,
                    actions.clone(),
                    format!("{ACTIONS_LABEL}\n{files}"),
                ));
            }
        }
    }
    let is_action = |i: &WheelItem| matches!(i.op, Operation::Action(_));
    let only_actions = !sectors.is_empty()
        && sectors.iter().all(|s| match &s.kind {
            SectorKind::Item(i) => is_action(i),
            // Actions-only overflow ("More") still counts as an actions ring.
            SectorKind::Expand { items, .. } => !items.is_empty() && items.iter().all(is_action),
        });
    let center = if sectors.is_empty() {
        NOTHING.to_owned()
    } else if only_actions {
        format!("{ACTIONS_TITLE}\n{files}")
    } else {
        format!("{TITLE}\n{files}")
    };
    Prepared { sectors, center, file_count }
}

/// What a drop/click on `hit` selects: (group index, category, item).
/// `None` for the center, the outside, or a sector that only opens a sub-ring.
pub fn pick_target(p: &Prepared, hit: Hit) -> Option<(Option<usize>, Option<Category>, WheelItem)> {
    match hit {
        Hit::Sector(i) => match &p.sectors.get(i)?.kind {
            SectorKind::Item(it) => Some((None, None, *it)),
            SectorKind::Expand { .. } => None,
        },
        Hit::SubSector { group, sub } => match &p.sectors.get(group)?.kind {
            SectorKind::Expand { group, category, items, .. } => {
                Some((*group, *category, *items.get(sub)?))
            }
            SectorKind::Item(_) => None,
        },
        Hit::Center | Hit::Outside => None,
    }
}

/// Result of resolving a drag-drop hit (not used for click mode).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropResolve {
    /// Commit this target.
    Pick(Option<usize>, Option<Category>, WheelItem),
    /// Center over an expanding sector 0: open its sub-ring, keep the wheel.
    OpenSub(usize),
    /// Outside or an unusable expand hit: cancel the wheel.
    Cancel,
}

/// Drag-drop pick rules. The pointer often sits in the dead center by design;
/// dropping there commits sector 0 (favorite / last target) when it is a
/// direct item. Expanding groups still need a sub-item (or open the sub-ring).
pub fn resolve_drop(p: &Prepared, hit: Hit) -> DropResolve {
    match hit {
        Hit::Outside => DropResolve::Cancel,
        Hit::Center => match p.sectors.first().map(|s| &s.kind) {
            Some(SectorKind::Item(it)) => DropResolve::Pick(None, None, *it),
            Some(SectorKind::Expand { .. }) => DropResolve::OpenSub(0),
            None => DropResolve::Cancel,
        },
        Hit::Sector(i) | Hit::SubSector { group: i, .. } => match pick_target(p, hit) {
            Some((g, c, it)) => DropResolve::Pick(g, c, it),
            None if matches!(hit, Hit::Sector(_)) && p.sectors.get(i).is_some_and(|s| s.expands()) => {
                DropResolve::OpenSub(i)
            }
            None => DropResolve::Cancel,
        },
    }
}

/// Whether a drag-over at `hit` should show COPY (accepted drop affordance).
pub fn drop_accepts(p: &Prepared, hit: Hit) -> bool {
    !matches!(resolve_drop(p, hit), DropResolve::Cancel)
}

/// Next or previous index in a ring of `n`, starting from `cur` (wraps).
/// With no current index, forward starts at 0 and backward at the last one.
pub fn step_index(cur: Option<usize>, n: usize, forward: bool) -> usize {
    if n == 0 {
        return 0;
    }
    match (cur, forward) {
        (None, true) => 0,
        (None, false) => n - 1,
        (Some(c), true) => (c + 1) % n,
        (Some(c), false) => (c + n - 1) % n,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zest_core::format::FormatId;
    use zest_core::graph::{EngineId, WheelGroup};
    use zest_core::ops::{ActionKind, ToolKind};

    fn item(f: FormatId, lossy: bool) -> WheelItem {
        WheelItem { op: Operation::Convert(f), label: f.label(), lossy, engine: Some(EngineId::Vips), quality: None }
    }

    fn tool(t: ToolKind) -> WheelItem {
        WheelItem { op: Operation::Tool(t), label: t.label(), lossy: false, engine: Some(EngineId::Vips), quality: None }
    }

    fn action(a: ActionKind) -> WheelItem {
        WheelItem { op: Operation::Action(a), label: a.label(), lossy: false, engine: None, quality: None }
    }

    #[test]
    fn flat_items_and_more() {
        let m = WheelModel::Flat {
            items: vec![
                WheelEntry::Item(item(FormatId::Png, false)),
                WheelEntry::Item(item(FormatId::Jpeg, true)),
                WheelEntry::More(vec![item(FormatId::Tiff, false), item(FormatId::Bmp, false)]),
            ],
            skipped: 0,
        };
        let p = prepare(&m, 2);
        assert_eq!(p.sectors.len(), 3);
        assert_eq!(p.sectors[0].label, "PNG");
        assert!(!p.sectors[0].lossy);
        assert!(p.sectors[1].lossy);
        assert_eq!(p.sectors[2].label, "More \u{203A}");
        assert!(p.sectors[2].expands());
        assert_eq!(p.sectors[2].sub_items().len(), 2);
        assert_eq!(p.center, "Convert\n2 files");
    }

    #[test]
    fn groups_become_sectors() {
        let m = WheelModel::Groups {
            groups: vec![
                WheelGroup {
                    category: Category::Image,
                    file_count: 3,
                    items: vec![
                        WheelEntry::Item(item(FormatId::Png, false)),
                        WheelEntry::More(vec![item(FormatId::Tiff, false)]),
                    ],
                },
                WheelGroup {
                    category: Category::Video,
                    file_count: 1,
                    items: vec![WheelEntry::Item(item(FormatId::Mp4, true))],
                },
            ],
            actions: vec![],
            skipped: 1,
        };
        let p = prepare(&m, 4);
        assert_eq!(p.sectors.len(), 2);
        assert_eq!(p.sectors[0].label, "Images (3) \u{203A}");
        assert_eq!(p.sectors[1].label, "Videos (1) \u{203A}");
        // More inside a group is flattened into its sub-ring
        assert_eq!(p.sectors[0].sub_items().len(), 2);
        match &p.sectors[0].kind {
            SectorKind::Expand { group, category, center, .. } => {
                assert_eq!(*group, Some(0));
                assert_eq!(*category, Some(Category::Image));
                assert_eq!(center, "Images\n3 files");
            }
            _ => panic!("expected group"),
        }
        match &p.sectors[1].kind {
            SectorKind::Expand { center, .. } => assert_eq!(center, "Videos\n1 file"),
            _ => panic!(),
        }
    }

    #[test]
    fn document_group_is_labelled_docs() {
        let m = WheelModel::Groups {
            groups: vec![
                WheelGroup {
                    category: Category::Image,
                    file_count: 1,
                    items: vec![WheelEntry::Item(item(FormatId::Png, false))],
                },
                WheelGroup {
                    category: Category::Document,
                    file_count: 2,
                    items: vec![WheelEntry::Item(item(FormatId::Pdf, false))],
                },
            ],
            actions: vec![action(ActionKind::CopyPath)],
            skipped: 0,
        };
        let p = prepare(&m, 3);
        assert_eq!(p.sectors.len(), 3);
        assert_eq!(p.sectors[1].label, "Docs (2) \u{203A}");
        assert_eq!(category_label(Category::Document), "Docs");
        match &p.sectors[1].kind {
            SectorKind::Expand { category, center, .. } => {
                assert_eq!(*category, Some(Category::Document));
                assert_eq!(center, "Docs\n2 files");
            }
            _ => panic!("expected group"),
        }
    }

    #[test]
    fn empty_model_and_ring_limit() {
        let p = prepare(&WheelModel::Flat { items: vec![], skipped: 2 }, 2);
        assert!(p.sectors.is_empty());
        assert_eq!(p.center, NOTHING);
        let many: Vec<WheelEntry> =
            (0..12).map(|_| WheelEntry::Item(item(FormatId::Png, false))).collect();
        assert_eq!(prepare(&WheelModel::Flat { items: many, skipped: 0 }, 1).sectors.len(), MAX_SECTORS);
        assert_eq!(Prepared::empty().center, DROP_HERE);
        assert_eq!(files_text(1), "1 file");
    }

    #[test]
    fn pick_targets() {
        let m = WheelModel::Groups {
            groups: vec![WheelGroup {
                category: Category::Image,
                file_count: 2,
                items: vec![
                    WheelEntry::Item(item(FormatId::Png, false)),
                    WheelEntry::Item(item(FormatId::Jpeg, true)),
                ],
            }],
            actions: vec![],
            skipped: 0,
        };
        let p = prepare(&m, 2);
        // group sector itself is not pickable
        assert_eq!(pick_target(&p, Hit::Sector(0)), None);
        let (g, c, it) = pick_target(&p, Hit::SubSector { group: 0, sub: 1 }).unwrap();
        assert_eq!((g, c), (Some(0), Some(Category::Image)));
        assert_eq!(it.target(), Some(FormatId::Jpeg));
        assert_eq!(pick_target(&p, Hit::SubSector { group: 0, sub: 5 }), None);
        assert_eq!(pick_target(&p, Hit::Center), None);
        assert_eq!(pick_target(&p, Hit::Outside), None);

        let flat = WheelModel::Flat {
            items: vec![
                WheelEntry::Item(item(FormatId::Png, false)),
                WheelEntry::More(vec![item(FormatId::Bmp, false)]),
            ],
            skipped: 0,
        };
        let p = prepare(&flat, 1);
        let (g, c, it) = pick_target(&p, Hit::Sector(0)).unwrap();
        assert_eq!((g, c, it.target()), (None, None, Some(FormatId::Png)));
        assert_eq!(pick_target(&p, Hit::Sector(1)), None);
        let (g, _, it) = pick_target(&p, Hit::SubSector { group: 1, sub: 0 }).unwrap();
        assert_eq!((g, it.target()), (None, Some(FormatId::Bmp)));
    }

    #[test]
    fn drop_on_center_commits_favorite_sector() {
        let flat = WheelModel::Flat {
            items: vec![
                WheelEntry::Item(item(FormatId::Png, false)),
                WheelEntry::Item(item(FormatId::Jpeg, true)),
            ],
            skipped: 0,
        };
        let p = prepare(&flat, 1);
        match resolve_drop(&p, Hit::Center) {
            DropResolve::Pick(None, None, it) => assert_eq!(it.target(), Some(FormatId::Png)),
            other => panic!("expected Pick favorite, got {other:?}"),
        }
        assert!(drop_accepts(&p, Hit::Center));
        assert!(matches!(resolve_drop(&p, Hit::Outside), DropResolve::Cancel));
        assert!(!drop_accepts(&p, Hit::Outside));
        // Click rules unchanged: center still picks nothing.
        assert_eq!(pick_target(&p, Hit::Center), None);
    }

    #[test]
    fn drop_on_center_opens_expanding_sector_zero() {
        let m = WheelModel::Groups {
            groups: vec![WheelGroup {
                category: Category::Image,
                file_count: 2,
                items: vec![
                    WheelEntry::Item(item(FormatId::Png, false)),
                    WheelEntry::Item(item(FormatId::Jpeg, true)),
                ],
            }],
            actions: vec![],
            skipped: 0,
        };
        let p = prepare(&m, 2);
        assert!(matches!(resolve_drop(&p, Hit::Center), DropResolve::OpenSub(0)));
        assert!(drop_accepts(&p, Hit::Center));
        assert!(matches!(resolve_drop(&p, Hit::Sector(0)), DropResolve::OpenSub(0)));
        match resolve_drop(&p, Hit::SubSector { group: 0, sub: 0 }) {
            DropResolve::Pick(Some(0), Some(Category::Image), it) => {
                assert_eq!(it.target(), Some(FormatId::Png));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn tools_and_actions_groups_open_sub_rings() {
        let m = WheelModel::Flat {
            items: vec![
                WheelEntry::Item(item(FormatId::Png, false)),
                WheelEntry::Group {
                    label: "Tools",
                    items: vec![tool(ToolKind::Compress), tool(ToolKind::Resize50), tool(ToolKind::Rotate180)],
                },
                WheelEntry::Group {
                    label: "Actions",
                    items: vec![action(ActionKind::CopyPath), action(ActionKind::Zip)],
                },
            ],
            skipped: 0,
        };
        let p = prepare(&m, 3);
        assert_eq!(p.sectors.len(), 3);
        assert_eq!(p.sectors[1].label, "Tools \u{203A}");
        assert_eq!(p.sectors[2].label, "Actions \u{203A}");
        assert!(!p.sectors[0].expands());
        assert!(p.sectors[1].expands() && p.sectors[2].expands());
        assert_eq!(p.sectors[1].sub_items().len(), 3);
        assert_eq!(p.sectors[2].sub_items().len(), 2);
        match &p.sectors[1].kind {
            SectorKind::Expand { group, category, center, .. } => {
                assert_eq!((*group, *category), (None, None));
                assert_eq!(center, "Tools\n3 files");
            }
            _ => panic!(),
        }
        assert_eq!(p.center, "Convert\n3 files");
        // A group sector itself is not pickable; its sub items are.
        assert_eq!(pick_target(&p, Hit::Sector(1)), None);
        let (g, c, it) = pick_target(&p, Hit::SubSector { group: 1, sub: 1 }).unwrap();
        assert_eq!((g, c), (None, None));
        assert_eq!(it.op, Operation::Tool(ToolKind::Resize50));
        let (_, _, it) = pick_target(&p, Hit::SubSector { group: 2, sub: 1 }).unwrap();
        assert_eq!(it.op, Operation::Action(ActionKind::Zip));
        assert_eq!(pick_target(&p, Hit::SubSector { group: 2, sub: 2 }), None);
    }

    #[test]
    fn mixed_selection_gets_an_actions_sector() {
        let m = WheelModel::Groups {
            groups: vec![WheelGroup {
                category: Category::Image,
                file_count: 2,
                items: vec![WheelEntry::Item(item(FormatId::Webp, true))],
            }],
            actions: vec![action(ActionKind::CopyFiles), action(ActionKind::MoveTo)],
            skipped: 0,
        };
        let p = prepare(&m, 5);
        assert_eq!(p.sectors.len(), 2);
        assert_eq!(p.sectors[1].label, "Actions \u{203A}");
        match &p.sectors[1].kind {
            SectorKind::Expand { group, category, items, center } => {
                assert_eq!((*group, *category), (None, None));
                assert_eq!(items.len(), 2);
                assert_eq!(center, "Actions\n5 files");
            }
            _ => panic!(),
        }
        let (g, c, it) = pick_target(&p, Hit::SubSector { group: 1, sub: 0 }).unwrap();
        assert_eq!((g, c), (None, None));
        assert_eq!(it.op, Operation::Action(ActionKind::CopyFiles));
        // The image group still reports its category.
        let (g, c, _) = pick_target(&p, Hit::SubSector { group: 0, sub: 0 }).unwrap();
        assert_eq!((g, c), (Some(0), Some(Category::Image)));
    }

    #[test]
    fn actions_only_ring_is_direct() {
        // First 7 actions fit as direct sectors; remaining spill into More (MAX_SECTORS = 8).
        let mut items: Vec<WheelEntry> = ActionKind::ALL[..7]
            .iter()
            .map(|&a| WheelEntry::Item(action(a)))
            .collect();
        items.push(WheelEntry::More(
            ActionKind::ALL[7..].iter().map(|&a| action(a)).collect(),
        ));
        let m = WheelModel::Flat { items, skipped: 1 };
        let p = prepare(&m, 1);
        assert_eq!(p.sectors.len(), MAX_SECTORS);
        assert!(p.sectors[..7].iter().all(|s| !s.expands()));
        assert!(p.sectors[7].expands());
        assert_eq!(p.sectors[0].label, "Copy path");
        assert_eq!(p.center, "Actions\n1 file");
        let (_, _, it) = pick_target(&p, Hit::Sector(5)).unwrap();
        assert_eq!(it.op, Operation::Action(ActionKind::Zip));
    }

    #[test]
    fn step_wraps() {
        assert_eq!(step_index(None, 6, true), 0);
        assert_eq!(step_index(None, 6, false), 5);
        assert_eq!(step_index(Some(5), 6, true), 0);
        assert_eq!(step_index(Some(0), 6, false), 5);
        assert_eq!(step_index(Some(2), 6, true), 3);
        assert_eq!(step_index(Some(2), 0, true), 0);
    }
}
