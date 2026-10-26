//! Settings form model (pure, no Windows calls): the form values, checks,
//! and how they are written into `config.toml` with `toml_edit` so that
//! comments, order and unknown keys stay.
//!
//! Only the values that the user changed are written (the form is compared
//! with the form that the window opened with). All other lines are not touched.

use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, TableLike, Value};
use zest_core::watch::{parse_watch_op, watch_op_text};
use zest_core::{
    Concurrency, Config, FormatGraph, FormatId, Modifier, Operation, OutputLocation, Quality, ReduceAnimations, Scope,
    ToolKind, TriggerKind, WatchEntry,
};

pub const RADIUS_MIN: u32 = 110;
pub const RADIUS_MAX: u32 = 200;
pub const VIDEO_WORKERS_MAX: u32 = 8;
pub const IMAGE_WORKERS_MAX: u32 = 16;
pub const HOLD_MS_MIN: u32 = 100;
pub const HOLD_MS_MAX: u32 = 3000;

/// The modifier choices of the drag and hold gestures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModChoice {
    Ctrl,
    Alt,
    Shift,
    CtrlShift,
}

impl ModChoice {
    pub const ALL: [ModChoice; 4] = [ModChoice::Ctrl, ModChoice::Alt, ModChoice::Shift, ModChoice::CtrlShift];

    pub fn label(self) -> &'static str {
        match self {
            ModChoice::Ctrl => "Ctrl",
            ModChoice::Alt => "Alt",
            ModChoice::Shift => "Shift",
            ModChoice::CtrlShift => "Ctrl+Shift",
        }
    }

    pub fn modifiers(self) -> Vec<Modifier> {
        match self {
            ModChoice::Ctrl => vec![Modifier::Ctrl],
            ModChoice::Alt => vec![Modifier::Alt],
            ModChoice::Shift => vec![Modifier::Shift],
            ModChoice::CtrlShift => vec![Modifier::Ctrl, Modifier::Shift],
        }
    }

    /// The choice for a modifier list (order does not matter). `None` for
    /// lists that the form cannot show (for example Win).
    pub fn from_modifiers(m: &[Modifier]) -> Option<ModChoice> {
        let has = |x: Modifier| m.contains(&x);
        let (c, a, s, w) = (has(Modifier::Ctrl), has(Modifier::Alt), has(Modifier::Shift), has(Modifier::Win));
        match (c, a, s, w) {
            (true, false, false, false) => Some(ModChoice::Ctrl),
            (false, true, false, false) => Some(ModChoice::Alt),
            (false, false, true, false) => Some(ModChoice::Shift),
            (true, false, true, false) => Some(ModChoice::CtrlShift),
            _ => None,
        }
    }
}

/// Which field an error belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    HoldMs,
    Chord,
    Fallback,
    Excluded,
    Folder,
    Watch,
}

/// One row of the "Watch folders" list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchRow {
    pub folder: String,
    /// `convert:jpg` or a tool name.
    pub op: String,
    /// Patterns, separated by commas. Empty = every file the operation can handle.
    pub include: String,
    pub recursive: bool,
    pub enabled: bool,
}

impl WatchRow {
    pub fn from_entry(e: &WatchEntry) -> WatchRow {
        WatchRow {
            folder: e.folder.to_string_lossy().into_owned(),
            op: e.op.clone(),
            include: e.include.join(", "),
            recursive: e.recursive,
            enabled: e.enabled,
        }
    }

    pub fn patterns(&self) -> Vec<String> {
        self.include.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect()
    }

    /// The problem of this row, if any.
    pub fn validate(&self) -> Result<(), String> {
        let f = self.folder.trim();
        if f.is_empty() {
            return Err("Choose a folder.".to_owned());
        }
        if !std::path::Path::new(f).is_absolute() {
            return Err("The folder must be a full path, like D:\\Inbox.".to_owned());
        }
        if parse_watch_op(&self.op).is_none() {
            return Err("Choose an operation.".to_owned());
        }
        if let Some(p) = self.patterns().iter().find(|p| p.contains(['\\', '/', ':', '"'])) {
            return Err(format!("\"{p}\": a pattern is a file name like *.heic, not a path."));
        }
        Ok(())
    }

    /// Text of the list entry: `D:\Inbox  ->  Convert to JPG  (*.heic; with subfolders)`.
    pub fn describe(&self) -> String {
        let op = parse_watch_op(&self.op).map_or_else(|| self.op.clone(), op_name);
        let mut extra: Vec<String> = Vec::new();
        let pats = self.patterns();
        if !pats.is_empty() {
            extra.push(pats.join(", "));
        }
        if self.recursive {
            extra.push("with subfolders".to_owned());
        }
        let mut t = format!("{}  \u{2192}  {op}", self.folder.trim());
        if !extra.is_empty() {
            t.push_str(&format!("  ({})", extra.join("; ")));
        }
        if !self.enabled {
            t.push_str("  [off]");
        }
        t
    }
}

fn op_name(op: Operation) -> String {
    crate::model_history::op_label(&op.key(), &[]).replace("Convert \u{2192} ", "Convert to ")
}

/// The operations a watch folder can run: `(text for config.toml, label)`.
/// Every conversion target of the graph, then the tools without a dialog.
pub fn watch_op_choices() -> Vec<(String, String)> {
    let g = FormatGraph::new();
    let mut seen: Vec<FormatId> = Vec::new();
    for from in FormatId::ALL {
        for e in g.targets(from) {
            if !seen.contains(&e.to) {
                seen.push(e.to);
            }
        }
    }
    let mut out: Vec<(String, String)> = seen
        .into_iter()
        .filter_map(|f| {
            let op = Operation::Convert(f);
            Some((watch_op_text(op)?, op_name(op)))
        })
        .collect();
    out.extend(ToolKind::PRESETS.iter().filter_map(|t| {
        let op = Operation::Tool(*t);
        Some((watch_op_text(op)?, op_name(op)))
    }));
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    pub field: Field,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SettingsForm {
    // Gestures
    pub drag_on: bool,
    pub drag_mod: ModChoice,
    pub hold_on: bool,
    pub hold_ms: String,
    pub scope: Scope,
    pub chord: String,
    pub fallback_chord: String,
    pub paused: bool,
    pub quiet: bool,
    pub excluded: String,
    // Wheel
    pub outer_radius: u32,
    pub reduce_always: bool,
    // Output
    pub folder_mode: bool,
    pub folder: String,
    pub send_to_shelf: bool,
    pub privacy_mode: bool,
    pub keep_timestamps: bool,
    pub quality: Quality,
    // Jobs (None = auto)
    pub video_workers: Option<u32>,
    pub image_workers: Option<u32>,
    // System (not in config.toml)
    pub autostart: bool,
    pub context_menu: bool,
    // Watch folders
    pub watch: Vec<WatchRow>,
}

// ---------------------------------------------------------------------------
// Key chords
// ---------------------------------------------------------------------------

const NAMED_KEYS: [&str; 15] = [
    "Space", "Enter", "Tab", "Esc", "Backspace", "Delete", "Insert", "Home", "End", "PageUp", "PageDown", "Left",
    "Up", "Right", "Down",
];

/// Canonical key name for text ("z" -> "Z", "f5" -> "F5", "return" -> "Enter"), or `None`.
/// The names are the ones that `zest-input` accepts.
pub fn canonical_key(name: &str) -> Option<String> {
    let n = name.trim();
    if n.chars().count() == 1 {
        let c = n.chars().next()?;
        return c.is_ascii_alphanumeric().then(|| c.to_ascii_uppercase().to_string());
    }
    let lower = n.to_ascii_lowercase();
    if let Some(num) = lower.strip_prefix('f') {
        if let Ok(k) = num.parse::<u32>() {
            if (1..=24).contains(&k) {
                return Some(format!("F{k}"));
            }
        }
    }
    let alias = match lower.as_str() {
        "return" => "enter",
        "escape" => "esc",
        "del" => "delete",
        "ins" => "insert",
        other => other,
    };
    NAMED_KEYS.iter().find(|k| k.to_ascii_lowercase() == alias).map(|k| (*k).to_owned())
}

/// Key name for a virtual-key code (for "Press keys\u{2026}"), `None` for modifier keys and others.
pub fn key_name_from_vk(vk: u32) -> Option<String> {
    match vk {
        0x30..=0x39 | 0x41..=0x5A => char::from_u32(vk).map(|c| c.to_string()),
        0x70..=0x87 => Some(format!("F{}", vk - 0x70 + 1)),
        0x20 => Some("Space".into()),
        0x0D => Some("Enter".into()),
        0x09 => Some("Tab".into()),
        0x1B => Some("Esc".into()),
        0x08 => Some("Backspace".into()),
        0x2E => Some("Delete".into()),
        0x2D => Some("Insert".into()),
        0x24 => Some("Home".into()),
        0x23 => Some("End".into()),
        0x21 => Some("PageUp".into()),
        0x22 => Some("PageDown".into()),
        0x25 => Some("Left".into()),
        0x26 => Some("Up".into()),
        0x27 => Some("Right".into()),
        0x28 => Some("Down".into()),
        _ => None,
    }
}

fn modifier_of(word: &str) -> Option<Modifier> {
    match word.trim().to_ascii_lowercase().as_str() {
        "ctrl" | "control" => Some(Modifier::Ctrl),
        "alt" => Some(Modifier::Alt),
        "shift" => Some(Modifier::Shift),
        "win" | "windows" | "super" => Some(Modifier::Win),
        _ => None,
    }
}

fn modifier_name(m: Modifier) -> &'static str {
    match m {
        Modifier::Ctrl => "Ctrl",
        Modifier::Alt => "Alt",
        Modifier::Shift => "Shift",
        Modifier::Win => "Win",
    }
}

/// "Ctrl+Alt+Space" -> modifiers and key. At least one modifier and one key
/// (a chord without a modifier would take a normal key away from every program).
pub fn parse_chord(text: &str) -> Result<(Vec<Modifier>, String), String> {
    let mut mods: Vec<Modifier> = Vec::new();
    let mut key: Option<String> = None;
    for part in text.split('+') {
        if part.trim().is_empty() {
            return Err("Use the form Ctrl+Alt+Space.".to_owned());
        }
        if let Some(m) = modifier_of(part) {
            if !mods.contains(&m) {
                mods.push(m);
            }
        } else if key.is_none() {
            key = Some(canonical_key(part).ok_or_else(|| format!("Unknown key \"{}\".", part.trim()))?);
        } else {
            return Err("Use one key and modifiers, for example Ctrl+Alt+Space.".to_owned());
        }
    }
    let key = key.ok_or_else(|| "The key is missing.".to_owned())?;
    if mods.is_empty() {
        return Err("Add a modifier (Ctrl, Alt, Shift or Win).".to_owned());
    }
    mods.sort_by_key(|m| match m {
        Modifier::Ctrl => 0,
        Modifier::Alt => 1,
        Modifier::Shift => 2,
        Modifier::Win => 3,
    });
    Ok((mods, key))
}

/// "Ctrl+Alt+Space" (fixed order: Ctrl, Alt, Shift, Win).
pub fn format_chord(mods: &[Modifier], key: &str) -> String {
    let mut out = String::new();
    for m in [Modifier::Ctrl, Modifier::Alt, Modifier::Shift, Modifier::Win] {
        if mods.contains(&m) {
            out.push_str(modifier_name(m));
            out.push('+');
        }
    }
    out.push_str(key);
    out
}

/// "a.exe, b.exe" -> ["a.exe", "b.exe"].
pub fn parse_excluded(text: &str) -> Vec<String> {
    text.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect()
}

// ---------------------------------------------------------------------------
// Form <-> config
// ---------------------------------------------------------------------------

impl SettingsForm {
    pub fn from_config(cfg: &Config, autostart: bool, context_menu: bool) -> SettingsForm {
        let find = |k: TriggerKind| cfg.triggers.iter().find(|t| t.kind == k);
        let drag = find(TriggerKind::Drag);
        let hold = find(TriggerKind::Hold);
        let chord = find(TriggerKind::KeyChord);
        let scope = drag.or(hold).map_or(Scope::FileManager, |t| t.scope);
        let chord_text = chord
            .and_then(|t| t.key.as_deref().map(|k| format_chord(&t.modifiers, k)))
            .unwrap_or_default();
        let fallback_text = chord
            .and_then(|t| t.fallback_key_chord.as_ref())
            .map(|f| format_chord(&f.modifiers, &f.key))
            .unwrap_or_default();
        let (folder_mode, folder) = match &cfg.output.location {
            OutputLocation::NextToSource => (false, String::new()),
            OutputLocation::Folder(p) => (true, p.to_string_lossy().into_owned()),
        };
        let workers = |c: Concurrency| match c {
            Concurrency::Auto => None,
            Concurrency::Fixed(n) => Some(n),
        };
        SettingsForm {
            drag_on: drag.is_some(),
            drag_mod: drag.and_then(|t| ModChoice::from_modifiers(&t.modifiers)).unwrap_or(ModChoice::Ctrl),
            hold_on: hold.is_some(),
            hold_ms: hold.and_then(|t| t.hold_ms).unwrap_or(450).to_string(),
            scope,
            chord: chord_text,
            fallback_chord: fallback_text,
            paused: cfg.gestures.paused,
            quiet: cfg.ui.quiet,
            excluded: cfg.gestures.excluded_processes.join(", "),
            outer_radius: cfg.wheel.outer_radius,
            reduce_always: cfg.wheel.reduce_animations == ReduceAnimations::Always,
            folder_mode,
            folder,
            send_to_shelf: cfg.output.send_to_shelf,
            privacy_mode: cfg.output.privacy_mode,
            keep_timestamps: cfg.output.keep_timestamps,
            quality: cfg.output.quality,
            video_workers: workers(cfg.jobs.video_concurrency),
            image_workers: workers(cfg.jobs.image_concurrency),
            autostart,
            context_menu,
            watch: cfg.watch.iter().map(WatchRow::from_entry).collect(),
        }
    }

    /// Default settings. The two system switches keep their current value.
    pub fn defaults(&self) -> SettingsForm {
        SettingsForm::from_config(&Config::default(), self.autostart, self.context_menu)
    }

    /// Output location text for config.toml.
    fn location_value(&self) -> String {
        if self.folder_mode {
            self.folder.trim().to_owned()
        } else {
            "next_to_source".to_owned()
        }
    }

    /// Problems of the form. An empty list means it can be saved.
    pub fn validate(&self) -> Vec<FieldError> {
        let mut errs = Vec::new();
        let mut err = |field, message: String| errs.push(FieldError { field, message });
        if self.hold_on {
            match self.hold_ms.trim().parse::<u32>() {
                Ok(ms) if (HOLD_MS_MIN..=HOLD_MS_MAX).contains(&ms) => {}
                _ => err(Field::HoldMs, format!("Hold time must be a number from {HOLD_MS_MIN} to {HOLD_MS_MAX} ms.")),
            }
        }
        if !self.chord.trim().is_empty() {
            if let Err(m) = parse_chord(&self.chord) {
                err(Field::Chord, m);
            }
        }
        if !self.fallback_chord.trim().is_empty() {
            if let Err(m) = parse_chord(&self.fallback_chord) {
                err(Field::Fallback, m);
            }
        }
        for name in parse_excluded(&self.excluded) {
            if name.contains(['\\', '/', '"', ':']) {
                err(Field::Excluded, format!("\"{name}\": give the program name only, like game.exe."));
                break;
            }
        }
        if self.folder_mode {
            let f = self.folder.trim();
            if f.is_empty() {
                err(Field::Folder, "Choose an output folder.".to_owned());
            } else if !std::path::Path::new(f).is_absolute() {
                err(Field::Folder, "The output folder must be a full path, like D:\\Converted.".to_owned());
            }
        }
        for (i, row) in self.watch.iter().enumerate() {
            if let Err(m) = row.validate() {
                err(Field::Watch, format!("Watch folder {}: {m}", i + 1));
                break;
            }
        }
        errs
    }
}

// ---------------------------------------------------------------------------
// Writing into the TOML document
// ---------------------------------------------------------------------------

/// Replace a value and keep the comment after it.
fn set_val(t: &mut dyn TableLike, key: &str, mut new: Value) {
    if let Some(Item::Value(old)) = t.get_mut(key) {
        *new.decor_mut() = old.decor().clone();
        *old = new;
    } else {
        t.insert(key, Item::Value(new));
    }
}

fn section<'a>(doc: &'a mut DocumentMut, name: &str) -> &'a mut dyn TableLike {
    let root = doc.as_table_mut();
    if root.get(name).and_then(Item::as_table_like).is_none() {
        root.insert(name, Item::Table(Table::new()));
    }
    root.get_mut(name).and_then(Item::as_table_like_mut).expect("section is a table")
}

fn mods_array(mods: &[Modifier]) -> Array {
    mods.iter().map(|m| modifier_name(*m)).collect()
}

fn trigger_index(doc: &DocumentMut, kind: &str) -> Option<usize> {
    let aot = doc.get("triggers")?.as_array_of_tables()?;
    aot.iter().position(|t| t.get("kind").and_then(Item::as_str) == Some(kind))
}

fn trigger_mut<'a>(doc: &'a mut DocumentMut, kind: &str) -> Option<&'a mut Table> {
    let idx = trigger_index(doc, kind)?;
    doc.get_mut("triggers")?.as_array_of_tables_mut()?.get_mut(idx)
}

fn remove_trigger(doc: &mut DocumentMut, kind: &str) {
    while let Some(idx) = trigger_index(doc, kind) {
        if let Some(aot) = doc.get_mut("triggers").and_then(Item::as_array_of_tables_mut) {
            aot.remove(idx);
        }
    }
    // An empty list must stay an empty list: a missing `triggers` key means "the defaults".
    let empty = doc.get("triggers").and_then(Item::as_array_of_tables).is_some_and(|a| a.is_empty());
    if empty {
        doc.as_table_mut().insert("triggers", Item::Value(Value::Array(Array::new())));
    }
}

/// A file without `triggers` uses the default triggers. Before one of them
/// is changed, they are written into the file.
fn ensure_triggers(doc: &mut DocumentMut) {
    if doc.get("triggers").is_some() {
        return;
    }
    let defaults: Option<Item> = Config::default_toml().parse::<DocumentMut>().ok().and_then(|d| d.get("triggers").cloned());
    if let Some(item) = defaults {
        doc.as_table_mut().insert("triggers", item);
    }
}

fn push_trigger(doc: &mut DocumentMut, t: Table) {
    let root = doc.as_table_mut();
    if root.get("triggers").and_then(Item::as_array_of_tables).is_none() {
        root.insert("triggers", Item::ArrayOfTables(toml_edit::ArrayOfTables::new()));
    }
    if let Some(aot) = root.get_mut("triggers").and_then(Item::as_array_of_tables_mut) {
        aot.push(t);
    }
}

fn scope_str(s: Scope) -> &'static str {
    match s {
        Scope::FileManager => "file_manager",
        Scope::Anywhere => "anywhere",
    }
}

fn quality_str(q: Quality) -> &'static str {
    match q {
        Quality::High => "high",
        Quality::Medium => "medium",
        Quality::Small => "small",
    }
}

fn workers_value(n: Option<u32>) -> Value {
    match n {
        None => Value::from("auto"),
        Some(n) => Value::from(i64::from(n)),
    }
}

fn fallback_value(mods: &[Modifier], key: &str) -> Value {
    let mut t = InlineTable::new();
    t.insert("modifiers", Value::Array(mods_array(mods)));
    t.insert("key", Value::from(key));
    Value::InlineTable(t)
}

impl SettingsForm {
    /// Write the changes (this form compared with `orig`) into `doc`.
    /// Call [`SettingsForm::validate`] first; bad values give an error here, too.
    pub fn apply(&self, orig: &SettingsForm, doc: &mut DocumentMut) -> Result<(), String> {
        // ----- triggers
        let trigger_change = self.drag_on != orig.drag_on
            || self.hold_on != orig.hold_on
            || self.drag_mod != orig.drag_mod
            || self.hold_ms.trim() != orig.hold_ms.trim()
            || self.scope != orig.scope
            || self.chord.trim() != orig.chord.trim()
            || self.fallback_chord.trim() != orig.fallback_chord.trim();
        if trigger_change {
            ensure_triggers(doc);
        }
        if self.drag_on != orig.drag_on {
            if self.drag_on {
                let mut t = Table::new();
                t.insert("kind", Item::Value("drag".into()));
                t.insert("modifiers", Item::Value(Value::Array(mods_array(&self.drag_mod.modifiers()))));
                t.insert("button", Item::Value("left".into()));
                t.insert("drag_threshold", Item::Value(0.into()));
                t.insert("scope", Item::Value(scope_str(self.scope).into()));
                push_trigger(doc, t);
            } else {
                remove_trigger(doc, "drag");
            }
        } else if self.drag_on && self.drag_mod != orig.drag_mod {
            if let Some(t) = trigger_mut(doc, "drag") {
                set_val(t, "modifiers", Value::Array(mods_array(&self.drag_mod.modifiers())));
            }
        }

        let hold_ms: Option<u32> = self.hold_ms.trim().parse().ok();
        if self.hold_on != orig.hold_on {
            if self.hold_on {
                let ms = hold_ms.ok_or("Hold time is not a number.")?;
                let mut t = Table::new();
                t.insert("kind", Item::Value("hold".into()));
                t.insert("modifiers", Item::Value(Value::Array(mods_array(&self.drag_mod.modifiers()))));
                t.insert("button", Item::Value("left".into()));
                t.insert("hold_ms", Item::Value(i64::from(ms).into()));
                t.insert("scope", Item::Value(scope_str(self.scope).into()));
                push_trigger(doc, t);
            } else {
                remove_trigger(doc, "hold");
            }
        } else if self.hold_on && self.hold_ms.trim() != orig.hold_ms.trim() {
            let ms = hold_ms.ok_or("Hold time is not a number.")?;
            if let Some(t) = trigger_mut(doc, "hold") {
                set_val(t, "hold_ms", Value::from(i64::from(ms)));
            }
        }
        // The hold gesture uses the same modifier as the drag gesture.
        if self.drag_mod != orig.drag_mod && self.hold_on && orig.hold_on {
            if let Some(t) = trigger_mut(doc, "hold") {
                set_val(t, "modifiers", Value::Array(mods_array(&self.drag_mod.modifiers())));
            }
        }
        if self.scope != orig.scope {
            for kind in ["drag", "hold"] {
                if let Some(t) = trigger_mut(doc, kind) {
                    set_val(t, "scope", Value::from(scope_str(self.scope)));
                }
            }
        }
        if self.chord.trim() != orig.chord.trim() {
            if self.chord.trim().is_empty() {
                remove_trigger(doc, "key_chord");
            } else {
                let (mods, key) = parse_chord(&self.chord)?;
                if let Some(t) = trigger_mut(doc, "key_chord") {
                    set_val(t, "modifiers", Value::Array(mods_array(&mods)));
                    set_val(t, "key", Value::from(key.as_str()));
                } else {
                    let mut t = Table::new();
                    t.insert("kind", Item::Value("key_chord".into()));
                    t.insert("modifiers", Item::Value(Value::Array(mods_array(&mods))));
                    t.insert("key", Item::Value(key.as_str().into()));
                    t.insert("scope", Item::Value("anywhere".into()));
                    push_trigger(doc, t);
                }
            }
        }
        let chord_created = orig.chord.trim().is_empty() && !self.chord.trim().is_empty();
        if self.fallback_chord.trim() != orig.fallback_chord.trim()
            || (chord_created && !self.fallback_chord.trim().is_empty())
        {
            if let Some(t) = trigger_mut(doc, "key_chord") {
                if self.fallback_chord.trim().is_empty() {
                    t.remove("fallback_key_chord");
                } else {
                    let (mods, key) = parse_chord(&self.fallback_chord)?;
                    set_val(t, "fallback_key_chord", fallback_value(&mods, &key));
                }
            }
        }

        // ----- gestures
        if self.paused != orig.paused {
            set_val(section(doc, "gestures"), "paused", Value::from(self.paused));
        }
        if self.excluded.trim() != orig.excluded.trim() {
            let arr: Array = parse_excluded(&self.excluded).iter().map(String::as_str).collect();
            set_val(section(doc, "gestures"), "excluded_processes", Value::Array(arr));
        }

        // ----- ui
        if self.quiet != orig.quiet {
            set_val(section(doc, "ui"), "quiet", Value::from(self.quiet));
        }

        // ----- wheel
        if self.outer_radius != orig.outer_radius {
            set_val(section(doc, "wheel"), "outer_radius", Value::from(i64::from(self.outer_radius)));
        }
        if self.reduce_always != orig.reduce_always {
            let v = if self.reduce_always { "always" } else { "system" };
            set_val(section(doc, "wheel"), "reduce_animations", Value::from(v));
        }

        // ----- output
        if self.location_value() != orig.location_value() {
            set_val(section(doc, "output"), "location", Value::from(self.location_value()));
        }
        if self.send_to_shelf != orig.send_to_shelf {
            set_val(section(doc, "output"), "send_to_shelf", Value::from(self.send_to_shelf));
        }
        if self.privacy_mode != orig.privacy_mode {
            set_val(section(doc, "output"), "privacy_mode", Value::from(self.privacy_mode));
        }
        if self.keep_timestamps != orig.keep_timestamps {
            set_val(section(doc, "output"), "keep_timestamps", Value::from(self.keep_timestamps));
        }
        if self.quality != orig.quality {
            set_val(section(doc, "output"), "quality", Value::from(quality_str(self.quality)));
        }

        // ----- jobs
        if self.video_workers != orig.video_workers {
            set_val(section(doc, "jobs"), "video_concurrency", workers_value(self.video_workers));
        }
        if self.image_workers != orig.image_workers {
            set_val(section(doc, "jobs"), "image_concurrency", workers_value(self.image_workers));
        }

        // ----- watch folders (the whole list is written again when it changed)
        if self.watch != orig.watch {
            let root = doc.as_table_mut();
            root.remove("watch");
            if !self.watch.is_empty() {
                let mut aot = toml_edit::ArrayOfTables::new();
                for r in &self.watch {
                    let mut t = Table::new();
                    t.insert("folder", Item::Value(Value::from(r.folder.trim())));
                    t.insert("op", Item::Value(Value::from(r.op.trim())));
                    let pats: Array = r.patterns().iter().map(String::as_str).collect();
                    t.insert("include", Item::Value(Value::Array(pats)));
                    t.insert("recursive", Item::Value(Value::from(r.recursive)));
                    t.insert("enabled", Item::Value(Value::from(r.enabled)));
                    aot.push(t);
                }
                root.insert("watch", Item::ArrayOfTables(aot));
            }
        }
        Ok(())
    }
}

/// New text of config.toml: `current` text with the changes of the form.
/// `current` that does not parse is an error (the user must fix the file).
/// The result is checked: it has to load as a `Config`.
pub fn edit_config_text(current: &str, orig: &SettingsForm, new: &SettingsForm) -> Result<String, String> {
    let mut doc: DocumentMut = current.parse().map_err(|e: toml_edit::TomlError| format!("config.toml has an error: {e}"))?;
    new.apply(orig, &mut doc)?;
    let text = doc.to_string();
    Config::from_toml_str(&text).map_err(|e| e.to_string())?;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form_of(text: &str) -> SettingsForm {
        SettingsForm::from_config(&Config::from_toml_str(text).unwrap(), false, false)
    }

    fn default_form() -> SettingsForm {
        form_of(Config::default_toml())
    }

    #[test]
    fn default_form_matches_default_config() {
        let f = default_form();
        assert!(f.drag_on && f.hold_on);
        assert_eq!(f.drag_mod, ModChoice::Ctrl);
        assert_eq!(f.hold_ms, "450");
        assert_eq!(f.scope, Scope::FileManager);
        assert_eq!(f.chord, "Ctrl+Alt+Space");
        assert_eq!(f.fallback_chord, "Alt+Win+Z");
        assert_eq!(f.outer_radius, 140);
        assert!(!f.folder_mode && !f.send_to_shelf && !f.reduce_always);
        assert_eq!(f.quality, Quality::High);
        assert_eq!((f.video_workers, f.image_workers), (None, None));
        assert!(f.validate().is_empty());
        assert_eq!(f.defaults().with_system(false, false), f);
    }

    impl SettingsForm {
        fn with_system(mut self, a: bool, c: bool) -> Self {
            self.autostart = a;
            self.context_menu = c;
            self
        }
    }

    #[test]
    fn no_change_leaves_the_text_byte_for_byte() {
        let f = default_form();
        let out = edit_config_text(Config::default_toml(), &f, &f.clone()).unwrap();
        assert_eq!(out, Config::default_toml());
    }

    #[test]
    fn changes_keep_comments_and_unknown_keys() {
        let text = format!("# my notes\nunknown_top = 1\n{}\n[extra]\nfoo = \"bar\"  # keep me\n", Config::default_toml());
        let orig = form_of(&text);
        let mut f = orig.clone();
        f.outer_radius = 170;
        f.paused = true;
        f.privacy_mode = true;
        f.send_to_shelf = true;
        f.quality = Quality::Small;
        f.reduce_always = true;
        f.video_workers = Some(3);
        f.image_workers = Some(12);
        f.folder_mode = true;
        f.folder = r"D:\Converted".into();
        f.excluded = "game.exe, tool.exe".into();
        f.scope = Scope::Anywhere;
        f.drag_mod = ModChoice::CtrlShift;
        f.hold_ms = "600".into();
        f.chord = "ctrl+shift+f5".into();
        f.fallback_chord = String::new();
        assert!(f.validate().is_empty());
        let out = edit_config_text(&text, &orig, &f).unwrap();
        // Comments and unknown keys stay.
        for keep in [
            "# my notes",
            "unknown_top = 1",
            "foo = \"bar\"  # keep me",
            "# Triggers open the wheel.",
            "# logical pixels",
            "# true = strip metadata on every conversion",
            "# or a folder path",
            "# \"auto\" or a number",
        ] {
            assert!(out.contains(keep), "lost {keep:?} in\n{out}");
        }
        // The values are the new ones, and nothing else moved.
        let cfg = Config::from_toml_str(&out).unwrap();
        let back = SettingsForm::from_config(&cfg, false, false);
        let mut want = f.clone();
        want.chord = "Ctrl+Shift+F5".into();
        assert_eq!(back, want);
        assert_eq!(cfg.output.location, OutputLocation::Folder(r"D:\Converted".into()));
        assert_eq!(cfg.gestures.excluded_processes, vec!["game.exe", "tool.exe"]);
        // Saving again with the new form changes nothing.
        assert_eq!(edit_config_text(&out, &back, &back).unwrap(), out);
    }

    #[test]
    fn watch_rows_are_written_and_read_back() {
        let text = format!("{}\n# my note\n", Config::default_toml());
        let orig = form_of(&text);
        assert!(orig.watch.is_empty());
        let mut f = orig.clone();
        f.watch.push(WatchRow {
            folder: r"D:\Inbox".into(),
            op: "convert:jpg".into(),
            include: "*.heic, *.HEIF".into(),
            recursive: true,
            enabled: true,
        });
        f.watch.push(WatchRow {
            folder: r"D:\Two".into(),
            op: "compress".into(),
            include: String::new(),
            recursive: false,
            enabled: false,
        });
        assert!(f.validate().is_empty());
        let out = edit_config_text(&text, &orig, &f).unwrap();
        assert!(out.contains("# my note") && out.lines().any(|l| l.starts_with("[[watch]]")), "{out}");
        let cfg = Config::from_toml_str(&out).unwrap();
        assert_eq!(cfg.watch.len(), 2);
        assert_eq!(cfg.watch[0].include, vec!["*.heic", "*.HEIF"]);
        assert!(!cfg.watch[1].enabled);
        let back = SettingsForm::from_config(&cfg, false, false);
        assert_eq!(back.watch, f.watch);
        // Unchanged: nothing moves. Removing the last rows removes the tables.
        assert_eq!(edit_config_text(&out, &back, &back).unwrap(), out);
        let mut none = back.clone();
        none.watch.clear();
        let cleared = edit_config_text(&out, &back, &none).unwrap();
        assert!(!cleared.lines().any(|l| l.starts_with("[[watch]]")), "{cleared}");
        assert!(Config::from_toml_str(&cleared).unwrap().watch.is_empty());
    }

    #[test]
    fn watch_rows_are_checked() {
        let ok = WatchRow {
            folder: r"D:\In".into(),
            op: "convert:png".into(),
            include: "*.bmp".into(),
            recursive: false,
            enabled: true,
        };
        assert!(ok.validate().is_ok());
        let mut r = ok.clone();
        r.folder = "relative".into();
        assert!(r.validate().is_err());
        r = ok.clone();
        r.op = "nonsense".into();
        assert!(r.validate().is_err());
        r = ok.clone();
        r.op = "merge-pdf".into();
        assert!(r.validate().is_err());
        r = ok.clone();
        r.include = r"C:\x\*.bmp".into();
        assert!(r.validate().is_err());
        let mut f = default_form();
        f.watch.push(r);
        assert_eq!(f.validate()[0].field, Field::Watch);
        assert_eq!(ok.describe(), "D:\\In  \u{2192}  Convert to PNG  (*.bmp)");
    }

    #[test]
    fn watch_op_choices_cover_conversions_and_tools() {
        let c = watch_op_choices();
        assert!(c.iter().any(|(v, l)| v == "convert:jpg" && l == "Convert to JPG"));
        assert!(c.iter().any(|(v, _)| v == "convert:pdf"));
        assert!(c.iter().any(|(v, l)| v == "compress" && l == "Compress"));
        assert!(!c.iter().any(|(v, _)| v.starts_with("trim") || v.starts_with("crop") || v == "merge-pdf"));
        for (v, _) in &c {
            assert!(parse_watch_op(v).is_some(), "{v}");
        }
    }

    #[test]
    fn triggers_can_be_removed_and_added() {
        let text = Config::default_toml();
        let orig = default_form();
        let mut f = orig.clone();
        f.drag_on = false;
        f.hold_on = false;
        f.chord = String::new();
        let out = edit_config_text(text, &orig, &f).unwrap();
        let cfg = Config::from_toml_str(&out).unwrap();
        assert!(cfg.triggers.is_empty(), "{out}");
        // And back on.
        let off = SettingsForm::from_config(&cfg, false, false);
        let mut on = off.clone();
        on.drag_on = true;
        on.drag_mod = ModChoice::Alt;
        on.hold_on = true;
        on.hold_ms = "500".into();
        on.chord = "Alt+Win+Z".into();
        on.fallback_chord = "Ctrl+Alt+Space".into();
        on.scope = Scope::Anywhere;
        let out2 = edit_config_text(&out, &off, &on).unwrap();
        let cfg2 = Config::from_toml_str(&out2).unwrap();
        assert_eq!(cfg2.triggers.len(), 3);
        assert_eq!(SettingsForm::from_config(&cfg2, false, false), on);
    }

    #[test]
    fn fallback_follows_a_new_chord() {
        let text = "triggers = []
[gestures]
paused = false
";
        let orig = form_of(text);
        assert_eq!(orig.chord, "");
        let mut f = orig.clone();
        f.chord = "Ctrl+Alt+Q".into();
        f.fallback_chord = "Alt+Win+Q".into();
        let out = edit_config_text(text, &orig, &f).unwrap();
        let back = form_of(&out);
        assert_eq!(back.chord, "Ctrl+Alt+Q");
        assert_eq!(back.fallback_chord, "Alt+Win+Q");
    }

    #[test]
    fn a_file_without_triggers_gets_the_defaults_before_an_edit() {
        let text = "[gestures]
paused = false
";
        let orig = form_of(text);
        assert_eq!(orig.drag_mod, ModChoice::Ctrl);
        let mut f = orig.clone();
        f.drag_mod = ModChoice::Alt;
        let out = edit_config_text(text, &orig, &f).unwrap();
        let back = form_of(&out);
        assert_eq!(back.drag_mod, ModChoice::Alt);
        assert!(back.hold_on && !back.chord.is_empty());
    }

    #[test]
    fn missing_sections_are_created_and_inline_tables_work() {
        let orig = form_of("");
        let mut f = orig.clone();
        f.outer_radius = 120;
        f.quality = Quality::Medium;
        f.video_workers = Some(2);
        let out = edit_config_text("", &orig, &f).unwrap();
        let cfg = Config::from_toml_str(&out).unwrap();
        assert_eq!(cfg.wheel.outer_radius, 120);
        assert_eq!(cfg.output.quality, Quality::Medium);
        // `wheel` written as an inline table.
        let text = "wheel = { outer_radius = 150, favorites = { heic = \"jpg\" } }\n";
        let orig = form_of(text);
        let mut f = orig.clone();
        f.outer_radius = 160;
        let out = edit_config_text(text, &orig, &f).unwrap();
        let cfg = Config::from_toml_str(&out).unwrap();
        assert_eq!(cfg.wheel.outer_radius, 160);
        assert_eq!(cfg.wheel.favorites.get("heic").map(String::as_str), Some("jpg"));
    }

    #[test]
    fn a_broken_file_is_not_overwritten() {
        let f = default_form();
        let mut g = f.clone();
        g.paused = true;
        let err = edit_config_text("[wheel\nouter_radius = ", &f, &g).unwrap_err();
        assert!(err.contains("config.toml has an error"), "{err}");
    }

    #[test]
    fn validation_messages() {
        let mut f = default_form();
        f.hold_ms = "20".into();
        f.chord = "Space".into();
        f.fallback_chord = "Ctrl+Nope".into();
        f.excluded = r"C:\games\game.exe".into();
        f.folder_mode = true;
        f.folder = "relative\\dir".into();
        let errs = f.validate();
        let fields: Vec<Field> = errs.iter().map(|e| e.field).collect();
        assert_eq!(fields, vec![Field::HoldMs, Field::Chord, Field::Fallback, Field::Excluded, Field::Folder]);
        f.folder = String::new();
        assert!(f.validate().iter().any(|e| e.field == Field::Folder && e.message.contains("Choose")));
        // A switched-off hold gesture needs no valid time.
        let mut g = default_form();
        g.hold_on = false;
        g.hold_ms = "abc".into();
        assert!(g.validate().is_empty());
    }

    #[test]
    fn chord_text() {
        assert_eq!(parse_chord("ctrl + alt + space").unwrap(), (vec![Modifier::Ctrl, Modifier::Alt], "Space".into()));
        assert_eq!(parse_chord("Z+Win+Alt").unwrap(), (vec![Modifier::Alt, Modifier::Win], "Z".into()));
        assert_eq!(parse_chord("Shift+f12").unwrap().1, "F12");
        assert!(parse_chord("Ctrl+").is_err());
        assert!(parse_chord("Ctrl+A+B").is_err());
        assert!(parse_chord("Ctrl+Alt").is_err());
        assert!(parse_chord("A").is_err());
        assert!(parse_chord("Ctrl+F25").is_err());
        assert_eq!(format_chord(&[Modifier::Win, Modifier::Ctrl], "K"), "Ctrl+Win+K");
        assert_eq!(canonical_key("return").as_deref(), Some("Enter"));
        assert_eq!(key_name_from_vk(0x41).as_deref(), Some("A"));
        assert_eq!(key_name_from_vk(0x74).as_deref(), Some("F5"));
        assert_eq!(key_name_from_vk(0x11), None); // Ctrl itself
        for vk in 0u32..256 {
            if let Some(n) = key_name_from_vk(vk) {
                assert_eq!(canonical_key(&n).as_deref(), Some(n.as_str()), "vk {vk:#x}");
            }
        }
    }

    #[test]
    fn modifier_choices() {
        for c in ModChoice::ALL {
            assert_eq!(ModChoice::from_modifiers(&c.modifiers()), Some(c));
        }
        assert_eq!(ModChoice::from_modifiers(&[Modifier::Shift, Modifier::Ctrl]), Some(ModChoice::CtrlShift));
        assert_eq!(ModChoice::from_modifiers(&[Modifier::Win]), None);
        // A Win+drag trigger is shown as Ctrl but is not changed by an unrelated save.
        let text = "[[triggers]]\nkind = \"drag\"\nmodifiers = [\"Win\"]\n";
        let orig = form_of(text);
        let mut f = orig.clone();
        f.paused = true;
        let out = edit_config_text(text, &orig, &f).unwrap();
        assert!(out.contains("modifiers = [\"Win\"]"), "{out}");
    }

    #[test]
    fn excluded_text() {
        assert_eq!(parse_excluded(" a.exe,, b.exe ,"), vec!["a.exe", "b.exe"]);
        assert!(parse_excluded("").is_empty());
    }
}
