//! `%LOCALAPPDATA%\Zest\state.json`: small runtime state (last picked target
//! per input format, last folder of "Copy to" / "Move to"). Atomic writes.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use zest_core::FormatId;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// input extension -> last picked target extension
    #[serde(default)]
    pub last_used: BTreeMap<String, String>,
    /// Folder the "Copy to" / "Move to" picker chose last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_folder: Option<String>,
    /// The Shelf: held items, window position, option.
    #[serde(default)]
    pub shelf: zest_panels::ShelfState,
}

impl State {
    pub fn load(path: &Path) -> State {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    pub fn typed(&self) -> HashMap<FormatId, FormatId> {
        self.last_used
            .iter()
            .filter_map(|(k, v)| Some((FormatId::from_extension(k)?, FormatId::from_extension(v)?)))
            .collect()
    }

    pub fn set(&mut self, input: FormatId, target: FormatId) {
        self.last_used.insert(input.extension().to_owned(), target.extension().to_owned());
    }

    /// The folder where the picker starts, if one was chosen before.
    pub fn last_folder(&self) -> Option<PathBuf> {
        self.last_folder.as_deref().filter(|s| !s.is_empty()).map(PathBuf::from)
    }

    pub fn set_last_folder(&mut self, folder: &Path) {
        self.last_folder = Some(folder.to_string_lossy().into_owned());
    }
}

/// Write `path.tmp`, then rename over `path`.
pub fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_typed() {
        let mut s = State::default();
        s.set(FormatId::Heic, FormatId::Jpeg);
        assert_eq!(s.typed().get(&FormatId::Heic), Some(&FormatId::Jpeg));
        assert!(s.to_json().contains("heic"));
    }

    #[test]
    fn last_folder_round_trip_and_old_files() {
        let mut s = State::default();
        assert_eq!(s.last_folder(), None);
        s.set_last_folder(Path::new(r"D:\Target Folder"));
        assert_eq!(s.last_folder(), Some(PathBuf::from(r"D:\Target Folder")));
        let back: State = serde_json::from_str(&s.to_json()).unwrap();
        assert_eq!(back, s);
        // A state.json from an older version has no such field.
        let old: State = serde_json::from_str(r#"{"last_used":{"heic":"jpg"}}"#).unwrap();
        assert_eq!(old.last_folder(), None);
        assert_eq!(old.typed().get(&FormatId::Heic), Some(&FormatId::Jpeg));
        // The field is left out while unset.
        assert!(!State::default().to_json().contains("last_folder"));
    }

    #[test]
    fn atomic_write_and_load() {
        let dir = std::env::temp_dir().join(format!("zest-state-{}", std::process::id()));
        let path = dir.join("state.json");
        let mut s = State::default();
        s.set(FormatId::Png, FormatId::Webp);
        write_atomic(&path, &s.to_json()).unwrap();
        assert_eq!(State::load(&path), s);
        assert!(!dir.join("state.json.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_or_bad_file_is_default() {
        assert_eq!(State::load(Path::new(r"Z:\nope\state.json")), State::default());
    }
}
