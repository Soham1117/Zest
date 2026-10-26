//! Shelf list model (pure, no Windows calls).
//!
//! The Shelf holds paths only. It never copies, moves or changes a file.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The most items the Shelf keeps.
pub const MAX_ITEMS: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShelfItem {
    pub path: PathBuf,
    /// File name for the list.
    pub name: String,
    /// The file or folder no longer exists (shown grey).
    pub missing: bool,
}

impl ShelfItem {
    fn new(path: PathBuf) -> Self {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        ShelfItem { path, name, missing: false }
    }
}

/// Case-insensitive key of a path without a trailing separator.
pub fn path_key(p: &Path) -> String {
    let s = p.to_string_lossy().to_lowercase().replace('/', "\\");
    let t = s.trim_end_matches('\\');
    if t.is_empty() || t.ends_with(':') {
        // `C:\` keeps its separator so it differs from the bare drive.
        s
    } else {
        t.to_owned()
    }
}

#[derive(Debug, Default, Clone)]
pub struct ShelfModel {
    items: Vec<ShelfItem>,
}

impl ShelfModel {
    pub fn from_paths<I: IntoIterator<Item = PathBuf>>(paths: I) -> Self {
        let mut m = ShelfModel::default();
        m.add(paths);
        m
    }

    pub fn items(&self) -> &[ShelfItem] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn get(&self, i: usize) -> Option<&ShelfItem> {
        self.items.get(i)
    }

    pub fn paths(&self) -> Vec<PathBuf> {
        self.items.iter().map(|i| i.path.clone()).collect()
    }

    /// Add paths at the end. A path that is already held is skipped (case
    /// does not matter). Returns how many were added. The list keeps at most
    /// [`MAX_ITEMS`]; the oldest items go first.
    pub fn add<I: IntoIterator<Item = PathBuf>>(&mut self, paths: I) -> usize {
        let mut added = 0;
        for p in paths {
            if p.as_os_str().is_empty() {
                continue;
            }
            let key = path_key(&p);
            if self.items.iter().any(|i| path_key(&i.path) == key) {
                continue;
            }
            self.items.push(ShelfItem::new(p));
            added += 1;
        }
        if self.items.len() > MAX_ITEMS {
            let extra = self.items.len() - MAX_ITEMS;
            self.items.drain(..extra);
        }
        added
    }

    /// Remove the items at these indices (any order, duplicates and bad
    /// indices are ignored). Returns the removed paths.
    pub fn remove_indices(&mut self, indices: &[usize]) -> Vec<PathBuf> {
        let mut idx: Vec<usize> = indices.iter().copied().filter(|&i| i < self.items.len()).collect();
        idx.sort_unstable();
        idx.dedup();
        let mut removed = Vec::new();
        for &i in idx.iter().rev() {
            removed.push(self.items.remove(i).path);
        }
        removed.reverse();
        removed
    }

    pub fn remove_paths(&mut self, paths: &[PathBuf]) -> usize {
        let keys: Vec<String> = paths.iter().map(|p| path_key(p)).collect();
        let before = self.items.len();
        self.items.retain(|i| !keys.contains(&path_key(&i.path)));
        before - self.items.len()
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Mark items that no longer exist. `exists` is a closure so that tests
    /// need no disk. Returns true if any flag changed.
    pub fn refresh_missing(&mut self, exists: impl Fn(&Path) -> bool) -> bool {
        let mut changed = false;
        for i in &mut self.items {
            let missing = !exists(&i.path);
            if i.missing != missing {
                i.missing = missing;
                changed = true;
            }
        }
        changed
    }

    /// Set the flags from a list of paths that are known to be missing.
    pub fn apply_missing(&mut self, missing: &[PathBuf]) -> bool {
        let keys: Vec<String> = missing.iter().map(|p| path_key(p)).collect();
        self.refresh_missing_by_key(|k| keys.iter().any(|m| m == k))
    }

    /// Update the flags of the items that a scan covered (`scanned` are the
    /// keys the scan looked at, `missing` the paths it did not find). Items
    /// that were added after the scan started keep their flag.
    pub fn refresh_missing_for(&mut self, scanned: &[String], missing: &[PathBuf]) -> bool {
        let miss: Vec<String> = missing.iter().map(|p| path_key(p)).collect();
        let mut changed = false;
        for i in &mut self.items {
            let k = path_key(&i.path);
            if !scanned.contains(&k) {
                continue;
            }
            let m = miss.contains(&k);
            if i.missing != m {
                i.missing = m;
                changed = true;
            }
        }
        changed
    }

    fn refresh_missing_by_key(&mut self, is_missing: impl Fn(&str) -> bool) -> bool {
        let mut changed = false;
        for i in &mut self.items {
            let m = is_missing(&path_key(&i.path));
            if i.missing != m {
                i.missing = m;
                changed = true;
            }
        }
        changed
    }

    /// Remove the items that are marked missing. Returns how many.
    pub fn prune_missing(&mut self) -> usize {
        let before = self.items.len();
        self.items.retain(|i| !i.missing);
        before - self.items.len()
    }

    pub fn missing_count(&self) -> usize {
        self.items.iter().filter(|i| i.missing).count()
    }

    /// Title text: "Shelf" or "Shelf (3)".
    pub fn title(&self) -> String {
        if self.items.is_empty() {
            "Shelf".to_owned()
        } else {
            format!("Shelf ({})", self.items.len())
        }
    }

    /// Text of the count label: "No items", "1 item", "3 items (1 missing)".
    pub fn count_text(&self) -> String {
        let n = self.items.len();
        let miss = self.missing_count();
        let base = match n {
            0 => return "Drop files here".to_owned(),
            1 => "1 item".to_owned(),
            n => format!("{n} items"),
        };
        if miss > 0 {
            format!("{base} ({miss} missing)")
        } else {
            base
        }
    }
}

/// What `state.json` keeps about the Shelf.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShelfState {
    #[serde(default)]
    pub items: Vec<String>,
    /// Window rectangle in physical pixels: left, top, width, height.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rect: Option<[i32; 4]>,
    /// Option "Remove after drag out".
    #[serde(default)]
    pub remove_after_drag: bool,
}

impl ShelfState {
    pub fn paths(&self) -> Vec<PathBuf> {
        self.items.iter().filter(|s| !s.is_empty()).map(PathBuf::from).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn add_dedupes_ignoring_case_and_trailing_slash() {
        let mut m = ShelfModel::default();
        assert_eq!(m.add([p(r"C:\a\x.png"), p(r"C:\A\X.PNG"), p(r"C:\a\y.png")]), 2);
        assert_eq!(m.add([p(r"c:/a/y.png"), p(r"C:\dir\"), p(r"C:\dir")]), 1);
        assert_eq!(m.len(), 3);
        assert_eq!(m.items()[0].name, "x.png");
        assert_eq!(m.add([p("")]), 0);
    }

    #[test]
    fn remove_by_index_handles_bad_input() {
        let mut m = ShelfModel::from_paths([p("a"), p("b"), p("c"), p("d")]);
        let gone = m.remove_indices(&[2, 0, 2, 99]);
        assert_eq!(gone, vec![p("a"), p("c")]);
        assert_eq!(m.paths(), vec![p("b"), p("d")]);
        assert_eq!(m.remove_paths(&[p("D"), p("zz")]), 1);
        assert_eq!(m.paths(), vec![p("b")]);
        m.clear();
        assert!(m.is_empty());
    }

    #[test]
    fn missing_items_are_flagged_and_pruned() {
        let mut m = ShelfModel::from_paths([p(r"C:\a"), p(r"C:\b"), p(r"C:\c")]);
        assert!(m.refresh_missing(|x| x != Path::new(r"C:\b")));
        assert_eq!(m.missing_count(), 1);
        assert!(m.items()[1].missing);
        // Same answer again: no change.
        assert!(!m.refresh_missing(|x| x != Path::new(r"C:\b")));
        assert_eq!(m.count_text(), "3 items (1 missing)");
        assert_eq!(m.prune_missing(), 1);
        assert_eq!(m.paths(), vec![p(r"C:\a"), p(r"C:\c")]);
        assert!(m.apply_missing(&[p(r"c:\A")]));
        assert_eq!(m.missing_count(), 1);
    }

    #[test]
    fn scan_result_only_touches_scanned_items() {
        let mut m = ShelfModel::from_paths([p(r"C:\a"), p(r"C:\b")]);
        m.add([p(r"C:\late")]);
        let scanned = vec![path_key(&p(r"C:\a")), path_key(&p(r"C:\b"))];
        assert!(m.refresh_missing_for(&scanned, &[p(r"C:\B")]));
        assert!(!m.items()[0].missing && m.items()[1].missing && !m.items()[2].missing);
        assert!(m.refresh_missing_for(&scanned, &[]));
        assert_eq!(m.missing_count(), 0);
    }

    #[test]
    fn list_is_capped_and_texts() {
        let mut m = ShelfModel::default();
        assert_eq!(m.count_text(), "Drop files here");
        assert_eq!(m.title(), "Shelf");
        m.add((0..MAX_ITEMS + 5).map(|i| p(&format!(r"C:\f{i}"))));
        assert_eq!(m.len(), MAX_ITEMS);
        assert_eq!(m.items()[0].name, "f5");
        assert_eq!(m.title(), format!("Shelf ({MAX_ITEMS})"));
        let one = ShelfModel::from_paths([p("x")]);
        assert_eq!(one.count_text(), "1 item");
    }

    #[test]
    fn state_round_trip_and_old_files() {
        let st = ShelfState {
            items: vec![r"C:\a\b.png".into()],
            rect: Some([10, 20, 300, 400]),
            remove_after_drag: true,
        };
        let json = serde_json::to_string(&st).unwrap();
        assert_eq!(serde_json::from_str::<ShelfState>(&json).unwrap(), st);
        let old: ShelfState = serde_json::from_str("{}").unwrap();
        assert_eq!(old, ShelfState::default());
    }
}
