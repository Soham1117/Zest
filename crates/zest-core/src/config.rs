//! `config.toml` model (spec 5.6): load, atomic save, first-run template.

use crate::format::FormatId;
use crate::job::Quality;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot access config file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("config parse error at line {line}, column {column}: {message}")]
    Parse {
        line: usize,
        column: usize,
        message: String,
    },
    #[error("cannot write config: {0}")]
    Serialize(#[from] toml::ser::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    Drag,
    Hold,
    KeyChord,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Modifier {
    Ctrl,
    Alt,
    Shift,
    Win,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Button {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    #[default]
    FileManager,
    Anywhere,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyChord {
    pub modifiers: Vec<Modifier>,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trigger {
    pub kind: TriggerKind,
    #[serde(default)]
    pub modifiers: Vec<Modifier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub button: Option<Button>,
    /// Drag: pixels before the gesture starts. 0 = system default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drag_threshold: Option<u32>,
    /// Hold: milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_ms: Option<u32>,
    #[serde(default)]
    pub scope: Scope,
    /// Key chord: main key name, for example "Space".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Key chord: used when the primary chord cannot be registered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_key_chord: Option<KeyChord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GestureConfig {
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub excluded_processes: Vec<String>,
    /// Extra process names treated as file managers when `scope = file_manager`
    /// (on top of explorer.exe, FPilot.exe, FilePilot.exe). Lower-case names OK.
    #[serde(default)]
    pub file_managers: Vec<String>,
}

/// UI preferences (balloons, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiConfig {
    /// Suppress success balloons; errors still notify.
    #[serde(default)]
    pub quiet: bool,
    /// Hotkey that undoes the last job (Recycle Bin). Empty uses `DEFAULT_UNDO_CHORD`.
    #[serde(default = "default_undo_chord")]
    pub undo_chord: String,
    /// User wants the Win11 top-level "Convert with Zest" sparse package menu.
    #[serde(default)]
    pub win11_top_level_menu: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        UiConfig {
            quiet: false,
            undo_chord: default_undo_chord(),
            win11_top_level_menu: false,
        }
    }
}

pub const DEFAULT_UNDO_CHORD: &str = "Ctrl+Alt+Shift+Z";

fn default_undo_chord() -> String {
    DEFAULT_UNDO_CHORD.into()
}

/// What to do when a new output matches an existing file (size + quick hash).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DuplicatePolicy {
    /// Ask Skip / Write anyway (/ Replace when allowed).
    #[default]
    Ask,
    /// Drop the new file; keep the existing one.
    Skip,
    /// Always write under a `(n)` name.
    Number,
    /// Overwrite the existing file (only when `allow_replace` is also true).
    Replace,
}

/// When the wheel and the progress ring skip their animations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReduceAnimations {
    /// Follow the Windows setting "Animation effects".
    #[default]
    System,
    /// Never animate.
    Always,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WheelConfig {
    #[serde(default = "default_outer_radius")]
    pub outer_radius: u32,
    /// `system` (follow Windows) or `always` (no animations).
    #[serde(default)]
    pub reduce_animations: ReduceAnimations,
    /// input extension -> output extension, for example `heic = "jpg"`.
    #[serde(default)]
    pub favorites: HashMap<String, String>,
}

fn default_outer_radius() -> u32 {
    140
}

impl Default for WheelConfig {
    fn default() -> Self {
        WheelConfig {
            outer_radius: default_outer_radius(),
            reduce_animations: ReduceAnimations::System,
            favorites: HashMap::new(),
        }
    }
}

impl WheelConfig {
    /// Favorites as formats. Entries with unknown extensions are dropped.
    pub fn favorites_typed(&self) -> HashMap<FormatId, FormatId> {
        self.favorites
            .iter()
            .filter_map(|(k, v)| Some((FormatId::from_extension(k)?, FormatId::from_extension(v)?)))
            .collect()
    }
}

/// Where outputs go. TOML: `"next_to_source"` or a folder path.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum OutputLocation {
    #[default]
    NextToSource,
    Folder(PathBuf),
}

impl From<String> for OutputLocation {
    fn from(s: String) -> Self {
        if s.trim().eq_ignore_ascii_case("next_to_source") || s.trim().is_empty() {
            OutputLocation::NextToSource
        } else {
            OutputLocation::Folder(PathBuf::from(s))
        }
    }
}

impl From<OutputLocation> for String {
    fn from(l: OutputLocation) -> Self {
        match l {
            OutputLocation::NextToSource => "next_to_source".to_owned(),
            OutputLocation::Folder(p) => p.to_string_lossy().into_owned(),
        }
    }
}

impl OutputLocation {
    /// Folder for `JobRequest::out_dir`; `None` = next to source.
    pub fn out_dir(&self) -> Option<&Path> {
        match self {
            OutputLocation::NextToSource => None,
            OutputLocation::Folder(p) => Some(p),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputConfig {
    #[serde(default)]
    pub location: OutputLocation,
    #[serde(default)]
    pub privacy_mode: bool,
    #[serde(default)]
    pub keep_timestamps: bool,
    /// true = finished job outputs are added to the Shelf (and the Shelf opens).
    #[serde(default)]
    pub send_to_shelf: bool,
    /// Quality of new jobs: high, medium or small.
    #[serde(default)]
    pub quality: Quality,
    /// When a finished output matches an existing file at the preferred name.
    #[serde(default)]
    pub on_duplicate: DuplicatePolicy,
    /// Allow the Replace choice for identical/conflicting outputs.
    #[serde(default)]
    pub allow_replace: bool,
}

impl Default for OutputConfig {
    fn default() -> Self {
        OutputConfig {
            location: OutputLocation::NextToSource,
            privacy_mode: false,
            keep_timestamps: false,
            send_to_shelf: false,
            quality: Quality::High,
            on_duplicate: DuplicatePolicy::Ask,
            allow_replace: false,
        }
    }
}

/// `"auto"` or a number of parallel jobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(try_from = "ConcurrencyRepr", into = "ConcurrencyRepr")]
pub enum Concurrency {
    #[default]
    Auto,
    Fixed(u32),
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum ConcurrencyRepr {
    Num(u32),
    Text(String),
}

impl TryFrom<ConcurrencyRepr> for Concurrency {
    type Error = String;
    fn try_from(r: ConcurrencyRepr) -> Result<Self, String> {
        match r {
            ConcurrencyRepr::Num(0) => Err("concurrency must be at least 1".to_owned()),
            ConcurrencyRepr::Num(n) => Ok(Concurrency::Fixed(n)),
            ConcurrencyRepr::Text(t) if t.eq_ignore_ascii_case("auto") => Ok(Concurrency::Auto),
            ConcurrencyRepr::Text(t) => Err(format!("expected \"auto\" or a number, got \"{t}\"")),
        }
    }
}

impl From<Concurrency> for ConcurrencyRepr {
    fn from(c: Concurrency) -> Self {
        match c {
            Concurrency::Auto => ConcurrencyRepr::Text("auto".to_owned()),
            Concurrency::Fixed(n) => ConcurrencyRepr::Num(n),
        }
    }
}

impl Concurrency {
    /// Fixed value, or `auto_value` for Auto. Never below 1.
    pub fn resolve(self, auto_value: usize) -> usize {
        match self {
            Concurrency::Auto => auto_value.max(1),
            Concurrency::Fixed(n) => (n as usize).max(1),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobsConfig {
    #[serde(default)]
    pub video_concurrency: Concurrency,
    #[serde(default)]
    pub image_concurrency: Concurrency,
}

impl Default for JobsConfig {
    fn default() -> Self {
        JobsConfig {
            video_concurrency: Concurrency::Auto,
            image_concurrency: Concurrency::Auto,
        }
    }
}

impl JobsConfig {
    /// Heavy video jobs: `max(1, min(2, cpus / 4))` when auto (spec 3.2).
    pub fn video_workers(&self, logical_cpus: usize) -> usize {
        self.video_concurrency.resolve((logical_cpus / 4).min(2))
    }

    /// Image and audio jobs: `max(2, cpus / 2)` when auto (spec 3.2).
    pub fn image_workers(&self, logical_cpus: usize) -> usize {
        self.image_concurrency.resolve((logical_cpus / 2).max(2))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_triggers")]
    pub triggers: Vec<Trigger>,
    #[serde(default)]
    pub gestures: GestureConfig,
    #[serde(default)]
    pub wheel: WheelConfig,
    #[serde(default)]
    pub output: OutputConfig,
    #[serde(default)]
    pub jobs: JobsConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(default)]
    pub ui: UiConfig,
    /// `[[watch]]`: folders whose new files are processed by themselves.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub watch: Vec<WatchEntry>,
}

/// One `[[watch]]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchEntry {
    pub folder: PathBuf,
    /// `convert:jpg` (any target extension) or the name of a tool (`compress`, `resize50`, ...).
    pub op: String,
    #[serde(default)]
    pub recursive: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// File name patterns (`*` and `?`). Empty = every file the operation can handle.
    #[serde(default)]
    pub include: Vec<String>,
}

fn default_true() -> bool {
    true
}

/// `[tools]`: full paths of external tools, when Zest should not look for them itself.
/// A missing value means "find it automatically".
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ToolsConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vips: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ffmpeg: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ffprobe: Option<PathBuf>,
    /// LibreOffice `soffice.exe` (Office documents to PDF and other formats).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soffice: Option<PathBuf>,
    /// `pandoc.exe` (Markdown, HTML, EPUB, DOCX).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pandoc: Option<PathBuf>,
}

fn default_triggers() -> Vec<Trigger> {
    vec![
        Trigger {
            kind: TriggerKind::Drag,
            modifiers: vec![Modifier::Ctrl],
            button: Some(Button::Left),
            drag_threshold: Some(0),
            hold_ms: None,
            scope: Scope::FileManager,
            key: None,
            fallback_key_chord: None,
        },
        Trigger {
            kind: TriggerKind::Hold,
            modifiers: vec![Modifier::Ctrl],
            button: Some(Button::Left),
            drag_threshold: None,
            hold_ms: Some(450),
            scope: Scope::FileManager,
            key: None,
            fallback_key_chord: None,
        },
        Trigger {
            kind: TriggerKind::KeyChord,
            modifiers: vec![Modifier::Ctrl, Modifier::Alt],
            button: None,
            drag_threshold: None,
            hold_ms: None,
            scope: Scope::Anywhere,
            key: Some("Space".to_owned()),
            fallback_key_chord: Some(KeyChord {
                modifiers: vec![Modifier::Win, Modifier::Alt],
                key: "Z".to_owned(),
            }),
        },
    ]
}

impl Default for Config {
    fn default() -> Self {
        Config {
            triggers: default_triggers(),
            gestures: GestureConfig::default(),
            wheel: WheelConfig::default(),
            output: OutputConfig::default(),
            jobs: JobsConfig::default(),
            tools: ToolsConfig::default(),
            ui: UiConfig::default(),
            watch: Vec::new(),
        }
    }
}

const DEFAULT_TOML: &str = r#"# Zest configuration. Zest reloads this file when you save it.

# Triggers open the wheel. Modifiers must match exactly.
# kind: drag | hold | key_chord
# scope: file_manager | anywhere

[[triggers]]
kind = "drag"
modifiers = ["Ctrl"]
button = "left"
drag_threshold = 0               # 0 = system default
scope = "file_manager"

[[triggers]]
kind = "hold"
modifiers = ["Ctrl"]
button = "left"
hold_ms = 450
scope = "file_manager"

[[triggers]]
kind = "key_chord"
modifiers = ["Ctrl", "Alt"]
key = "Space"
scope = "anywhere"
# Used when the chord above is already taken by another program.
fallback_key_chord = { modifiers = ["Win", "Alt"], key = "Z" }

[gestures]
paused = false
excluded_processes = []          # for example ["game.exe"]
# Extra file managers (explorer.exe / FPilot.exe / FilePilot.exe always count):
# file_managers = ["totalcmd64.exe"]

[ui]
quiet = false                    # true = no success balloons (errors still notify)
undo_chord = "Ctrl+Alt+Shift+Z"  # undo last Zest job (Recycle Bin); leaves Ctrl+Z to other apps

[wheel]
outer_radius = 140               # logical pixels
reduce_animations = "system"     # "system" (follow Windows) or "always"
favorites = {}                   # for example { heic = "jpg", mov = "mp4" }

[output]
location = "next_to_source"      # or a folder path
privacy_mode = false             # true = strip metadata on every conversion
keep_timestamps = false
send_to_shelf = false            # true = finished outputs go to the Shelf
quality = "high"                 # high | medium | small
on_duplicate = "ask"             # ask | skip | number | replace (replace needs allow_replace)
allow_replace = false

[jobs]
video_concurrency = "auto"       # "auto" or a number
image_concurrency = "auto"

# Optional: full paths of tools that Zest cannot find itself (leave out = search automatically).
# LibreOffice (soffice) and Pandoc are optional. The document conversions they give
# show on the wheel only when the tool is installed.
[tools]
# soffice = 'C:\Program Files\LibreOffice\program\soffice.exe'
# pandoc = 'C:\Users\you\AppData\Local\Pandoc\pandoc.exe'
# vips = 'D:\tools\vips\bin\vips.exe'
# ffmpeg = 'D:\tools\ffmpeg\bin\ffmpeg.exe'
# ffprobe = 'D:\tools\ffmpeg\bin\ffprobe.exe'

# Watch folders: a new file in the folder is processed by itself, once it is complete
# (its size and time stay the same for 2 seconds and no other program holds it open).
# Files that are already there are not touched. Results show in History; only failures show a balloon.
# op: "convert:jpg" (any target extension) or a tool name: compress, resize50, resize25,
#     max1920, max1280, strip, rotate-cw, rotate-ccw, rotate180, size10, size25, size50
# include: file name patterns, for example ["*.heic"]; leave out = every file the operation can handle
# [[watch]]
# folder = 'C:\Users\you\Pictures\Inbox'
# op = "convert:jpg"
# include = ["*.heic"]
# recursive = false
# enabled = true
"#;

impl Config {
    /// Commented template written on first run. Parses to `Config::default()`.
    pub fn default_toml() -> &'static str {
        DEFAULT_TOML
    }

    /// Parse TOML text. Errors carry line and column.
    pub fn from_toml_str(text: &str) -> Result<Config, ConfigError> {
        toml::from_str(text).map_err(|e| {
            let (line, column) = e
                .span()
                .map(|s| line_col(text, s.start))
                .unwrap_or((0, 0));
            ConfigError::Parse {
                line,
                column,
                message: e.message().to_owned(),
            }
        })
    }

    /// Load from `path`. A missing file gives the defaults.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Config::from_toml_str(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(source) => Err(ConfigError::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Write `path.tmp`, then rename over `path`. Creates the parent folder.
    pub fn save_atomic(&self, path: &Path) -> Result<(), ConfigError> {
        let io = |source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        };
        let text = toml::to_string_pretty(self)?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(io)?;
            }
        }
        let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
        tmp_name.push(".tmp");
        let tmp = path.with_file_name(tmp_name);
        std::fs::write(&tmp, text).map_err(io)?;
        std::fs::rename(&tmp, path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            io(e)
        })
    }
}

/// 1-based line and column of a byte offset.
fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(text.len());
    let before = &text.as_bytes()[..offset];
    let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
    let col = offset - before.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1) + 1;
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("zest-core-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn default_toml_parses_to_defaults() {
        let parsed = Config::from_toml_str(Config::default_toml()).unwrap();
        assert_eq!(parsed, Config::default());
    }

    #[test]
    fn defaults_match_spec() {
        let c = Config::default();
        assert_eq!(c.triggers.len(), 3);
        assert_eq!(c.triggers[0].kind, TriggerKind::Drag);
        assert_eq!(c.triggers[0].modifiers, vec![Modifier::Ctrl]);
        assert_eq!(c.triggers[0].button, Some(Button::Left));
        assert_eq!(c.triggers[1].hold_ms, Some(450));
        let chord = &c.triggers[2];
        assert_eq!(chord.key.as_deref(), Some("Space"));
        assert_eq!(chord.modifiers, vec![Modifier::Ctrl, Modifier::Alt]);
        let fb = chord.fallback_key_chord.as_ref().unwrap();
        assert_eq!(fb.modifiers, vec![Modifier::Win, Modifier::Alt]);
        assert_eq!(fb.key, "Z");
        assert_eq!(c.wheel.outer_radius, 140);
        assert!(!c.gestures.paused);
        assert_eq!(c.output.location, OutputLocation::NextToSource);
    }

    #[test]
    fn shelf_quality_and_animation_keys() {
        let c = Config::from_toml_str(
            "[output]
send_to_shelf = true
quality = \"small\"
[wheel]
reduce_animations = \"always\"
",
        )
        .unwrap();
        assert!(c.output.send_to_shelf);
        assert_eq!(c.output.quality, Quality::Small);
        assert_eq!(c.wheel.reduce_animations, ReduceAnimations::Always);
        let d = Config::default();
        assert!(!d.output.send_to_shelf);
        assert_eq!(d.output.quality, Quality::High);
        assert_eq!(d.wheel.reduce_animations, ReduceAnimations::System);
        assert!(Config::from_toml_str("[output]
quality = \"best\"
").is_err());
    }

    #[test]
    fn watch_entries() {
        let c = Config::from_toml_str(
            r#"[[watch]]
folder = 'D:\In'
op = "convert:jpg"
include = ["*.heic"]
[[watch]]
folder = 'D:\Two'
op = "compress"
recursive = true
enabled = false
"#,
        )
        .unwrap();
        assert_eq!(c.watch.len(), 2);
        assert!(c.watch[0].enabled && !c.watch[0].recursive);
        assert_eq!(c.watch[0].include, vec!["*.heic".to_owned()]);
        assert!(c.watch[1].recursive && !c.watch[1].enabled);
        assert!(Config::default().watch.is_empty());
        let text = toml::to_string_pretty(&c).unwrap();
        assert_eq!(Config::from_toml_str(&text).unwrap(), c);
    }

    #[test]
    fn empty_file_gives_defaults() {
        assert_eq!(Config::from_toml_str("").unwrap(), Config::default());
    }

    #[test]
    fn spec_example_parses() {
        let text = r#"
[[triggers]]
kind = "drag"
modifiers = ["Ctrl"]
button = "left"
drag_threshold = 0
scope = "file_manager"

[wheel]
outer_radius = 160
favorites = { heic = "jpg", mov = "mp4" }

[output]
location = 'D:\Converted'
privacy_mode = true

[jobs]
video_concurrency = 3
"#;
        let c = Config::from_toml_str(text).unwrap();
        assert_eq!(c.triggers.len(), 1);
        assert_eq!(c.wheel.outer_radius, 160);
        assert_eq!(
            c.wheel.favorites_typed().get(&FormatId::Heic),
            Some(&FormatId::Jpeg)
        );
        assert_eq!(
            c.output.location.out_dir(),
            Some(Path::new(r"D:\Converted"))
        );
        assert!(c.output.privacy_mode);
        assert_eq!(c.jobs.video_concurrency, Concurrency::Fixed(3));
        assert_eq!(c.jobs.image_concurrency, Concurrency::Auto);
    }

    #[test]
    fn parse_error_has_line() {
        let err = Config::from_toml_str("[wheel]\nouter_radius = \"big\"\n").unwrap_err();
        match &err {
            ConfigError::Parse { line, .. } => assert_eq!(*line, 2),
            other => panic!("wrong error: {other:?}"),
        }
        assert!(err.to_string().contains("line 2"));
        assert!(Config::from_toml_str("[jobs]\nvideo_concurrency = \"many\"\n").is_err());
        assert!(Config::from_toml_str("[jobs]\nvideo_concurrency = 0\n").is_err());
    }

    #[test]
    fn missing_file_gives_defaults() {
        let d = temp_dir("missing");
        let c = Config::load(&d.join("nope.toml")).unwrap();
        assert_eq!(c, Config::default());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn tools_section() {
        let text = "[tools]\nsoffice = 'D:\\LO\\program\\soffice.exe'\npandoc = 'D:\\pandoc.exe'\n";
        let c = Config::from_toml_str(text).unwrap();
        assert_eq!(c.tools.soffice, Some(PathBuf::from(r"D:\LO\program\soffice.exe")));
        assert_eq!(c.tools.pandoc, Some(PathBuf::from(r"D:\pandoc.exe")));
        assert_eq!(c.tools.vips, None);
        // Unset tools are not written back.
        let back = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(!back.contains("soffice"));
        assert_eq!(Config::from_toml_str("").unwrap().tools, ToolsConfig::default());
    }

    #[test]
    fn round_trip_save_load() {
        let d = temp_dir("roundtrip");
        let path = d.join("sub").join("config.toml");
        let mut c = Config::default();
        c.gestures.paused = true;
        c.gestures.excluded_processes = vec!["game.exe".into()];
        c.wheel.favorites.insert("heic".into(), "jpg".into());
        c.output.location = OutputLocation::Folder(PathBuf::from(r"D:\Out"));
        c.jobs.video_concurrency = Concurrency::Fixed(2);
        c.save_atomic(&path).unwrap();
        assert!(!d.join("sub").join("config.toml.tmp").exists());
        assert_eq!(Config::load(&path).unwrap(), c);
        // overwrite works
        c.output.privacy_mode = true;
        c.save_atomic(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), c);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn default_round_trips() {
        let c = Config::default();
        let text = toml::to_string_pretty(&c).unwrap();
        assert_eq!(Config::from_toml_str(&text).unwrap(), c);
    }

    #[test]
    fn worker_counts() {
        let j = JobsConfig::default();
        assert_eq!(j.video_workers(2), 1);
        assert_eq!(j.video_workers(8), 2);
        assert_eq!(j.video_workers(32), 2);
        assert_eq!(j.image_workers(2), 2);
        assert_eq!(j.image_workers(16), 8);
        let j = JobsConfig {
            video_concurrency: Concurrency::Fixed(4),
            image_concurrency: Concurrency::Fixed(1),
        };
        assert_eq!(j.video_workers(2), 4);
        assert_eq!(j.image_workers(16), 1);
    }

    #[test]
    fn favorites_drop_unknown() {
        let mut w = WheelConfig::default();
        w.favorites.insert("heic".into(), "jpg".into());
        w.favorites.insert("xyz".into(), "jpg".into());
        assert_eq!(w.favorites_typed().len(), 1);
    }
}
