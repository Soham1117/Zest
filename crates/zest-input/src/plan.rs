//! Pure translation of `zest_core::config::Config` triggers into a runtime
//! plan. No Windows calls (system metrics are passed in), fully unit-tested.

use zest_core::config::{Button, Config, KeyChord, Modifier, Scope, Trigger, TriggerKind};

use crate::gesture::Mods;

/// Hold time when the config does not give one (Filee default).
pub const DEFAULT_HOLD_MS: u32 = 450;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SlotKind {
    Drag,
    Hold,
}

/// Which physical/logical button a trigger listens to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Btn {
    /// Primary button (the physical right one when the user swapped buttons).
    Primary,
    /// Secondary button.
    Secondary,
    Middle,
}

/// One drag or hold trigger.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SlotSpec {
    pub kind: SlotKind,
    pub mods: Mods,
    pub button: Btn,
    pub scope: Scope,
    /// Movement threshold in pixels (x, y).
    pub threshold: (i32, i32),
    pub hold_ms: u32,
}

/// One registered hotkey (with an optional fallback).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ChordSpec {
    pub mods: Mods,
    pub vk: u32,
    pub fallback: Option<(Mods, u32)>,
    pub scope: Scope,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Plan {
    pub slots: Vec<SlotSpec>,
    pub chords: Vec<ChordSpec>,
    /// Lower-case process names.
    pub excluded: Vec<String>,
    /// Extra file-manager process names (lower-case), plus built-ins.
    pub file_managers: Vec<String>,
    pub paused: bool,
    /// Global undo hotkey (`ui.undo_chord`), if parseable.
    pub undo: Option<(Mods, u32)>,
}

pub fn mods_from(list: &[Modifier]) -> Mods {
    let mut m = Mods::NONE;
    for x in list {
        match x {
            Modifier::Ctrl => m.ctrl = true,
            Modifier::Alt => m.alt = true,
            Modifier::Shift => m.shift = true,
            Modifier::Win => m.win = true,
        }
    }
    m
}

/// Virtual-key code for a config key name ("Space", "Z", "F5", ...).
pub fn parse_key(name: &str) -> Option<u32> {
    let n = name.trim();
    if n.chars().count() == 1 {
        let c = n.chars().next()?.to_ascii_uppercase();
        if c.is_ascii_alphanumeric() {
            return Some(c as u32); // VK_A..VK_Z and VK_0..VK_9 equal their ASCII codes
        }
        return None;
    }
    let lower = n.to_ascii_lowercase();
    if let Some(num) = lower.strip_prefix('f') {
        if let Ok(k) = num.parse::<u32>() {
            if (1..=24).contains(&k) {
                return Some(0x70 + k - 1);
            }
        }
    }
    Some(match lower.as_str() {
        "space" => 0x20,
        "enter" | "return" => 0x0D,
        "tab" => 0x09,
        "esc" | "escape" => 0x1B,
        "backspace" => 0x08,
        "delete" | "del" => 0x2E,
        "insert" | "ins" => 0x2D,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        _ => return None,
    })
}

fn button_of(b: Option<Button>) -> Btn {
    match b.unwrap_or(Button::Left) {
        Button::Left => Btn::Primary,
        Button::Right => Btn::Secondary,
        Button::Middle => Btn::Middle,
    }
}

fn chord_of(t: &Trigger) -> Option<ChordSpec> {
    let vk = parse_key(t.key.as_deref()?)?;
    let fallback = t.fallback_key_chord.as_ref().and_then(|KeyChord { modifiers, key }| {
        Some((mods_from(modifiers), parse_key(key)?))
    });
    Some(ChordSpec { mods: mods_from(&t.modifiers), vk, fallback, scope: t.scope })
}

/// Build the plan. `system_threshold` is (`SM_CXDRAG`, `SM_CYDRAG`).
pub fn build_plan(cfg: &Config, system_threshold: (i32, i32)) -> Plan {
    let sys = (system_threshold.0.max(1), system_threshold.1.max(1));
    let mut plan = Plan {
        paused: cfg.gestures.paused,
        excluded: cfg
            .gestures
            .excluded_processes
            .iter()
            .map(|s| normalize_process(s))
            .filter(|s| !s.is_empty())
            .collect(),
        file_managers: cfg
            .gestures
            .file_managers
            .iter()
            .map(|s| normalize_process(s))
            .filter(|s| !s.is_empty())
            .collect(),
        undo: parse_undo_chord(&cfg.ui.undo_chord),
        ..Plan::default()
    };
    for t in &cfg.triggers {
        match t.kind {
            TriggerKind::Drag => {
                // threshold 0 or missing = system default
                let thr = match t.drag_threshold {
                    Some(n) if n > 0 => (n as i32, n as i32),
                    _ => sys,
                };
                plan.slots.push(SlotSpec {
                    kind: SlotKind::Drag,
                    mods: mods_from(&t.modifiers),
                    button: button_of(t.button),
                    scope: t.scope,
                    threshold: thr,
                    hold_ms: 0,
                });
            }
            TriggerKind::Hold => plan.slots.push(SlotSpec {
                kind: SlotKind::Hold,
                mods: mods_from(&t.modifiers),
                button: button_of(t.button),
                scope: t.scope,
                threshold: sys,
                hold_ms: t.hold_ms.filter(|&m| m > 0).unwrap_or(DEFAULT_HOLD_MS),
            }),
            TriggerKind::KeyChord => match chord_of(t) {
                Some(c) => plan.chords.push(c),
                None => tracing::warn!("key_chord trigger ignored: unknown key"),
            },
        }
    }
    plan
}

/// Lower-case file name, trimmed.
pub fn normalize_process(s: &str) -> String {
    s.trim().rsplit(['\\', '/']).next().unwrap_or("").to_ascii_lowercase()
}

/// Parse `Ctrl+Z` / `Ctrl+Shift+Z` style undo chords.
pub fn parse_undo_chord(text: &str) -> Option<(Mods, u32)> {
    let t = text.trim();
    if t.is_empty() {
        return Some((Mods { ctrl: true, ..Mods::NONE }, 0x5A)); // Ctrl+Z
    }
    let mut mods = Mods::NONE;
    let mut key: Option<u32> = None;
    for part in t.split('+') {
        let p = part.trim();
        match p.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods.ctrl = true,
            "alt" => mods.alt = true,
            "shift" => mods.shift = true,
            "win" | "windows" | "super" => mods.win = true,
            other => {
                if key.is_some() {
                    return None;
                }
                key = parse_key(other);
            }
        }
    }
    Some((mods, key?))
}

fn strip_exe(s: &str) -> &str {
    s.strip_suffix(".exe").unwrap_or(s)
}

/// `name` is the process file name; `excluded` holds normalized names.
pub fn is_excluded(name: &str, excluded: &[String]) -> bool {
    let n = name.to_ascii_lowercase();
    excluded.iter().any(|e| e == &n || strip_exe(e) == strip_exe(&n))
}

/// Built-in file managers for `scope = file_manager` (plus config allowlist).
const BUILTIN_FILE_MANAGERS: &[&str] = &["explorer.exe", "fpilot.exe", "filepilot.exe"];

fn is_file_manager(name: &str, extra: &[String]) -> bool {
    let n = name.to_ascii_lowercase();
    let stem = strip_exe(&n);
    BUILTIN_FILE_MANAGERS.iter().any(|b| {
        let b = b.to_ascii_lowercase();
        n == b || stem == strip_exe(&b)
    }) || extra.iter().any(|e| e == &n || strip_exe(e) == stem)
}

/// The scope decision for one process.
pub fn scope_allows(
    scope: Scope,
    name: &str,
    own: bool,
    excluded: &[String],
    file_managers: &[String],
) -> bool {
    if own || is_excluded(name, excluded) {
        return false;
    }
    match scope {
        Scope::FileManager => is_file_manager(name, file_managers),
        Scope::Anywhere => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_gives_spike_behavior() {
        let plan = build_plan(&Config::default(), (4, 4));
        assert_eq!(plan.slots.len(), 2);
        let drag = &plan.slots[0];
        assert_eq!(drag.kind, SlotKind::Drag);
        assert_eq!(drag.mods, Mods::CTRL);
        assert_eq!(drag.button, Btn::Primary);
        assert_eq!(drag.threshold, (4, 4)); // threshold 0 = system
        assert_eq!(drag.scope, Scope::FileManager);
        let hold = &plan.slots[1];
        assert_eq!(hold.kind, SlotKind::Hold);
        assert_eq!(hold.hold_ms, 450);
        assert_eq!(plan.chords.len(), 1);
        let c = &plan.chords[0];
        assert_eq!(c.vk, 0x20);
        assert_eq!(c.mods, Mods { ctrl: true, alt: true, ..Mods::NONE });
        assert_eq!(c.fallback, Some((Mods { win: true, alt: true, ..Mods::NONE }, 0x5A)));
        assert_eq!(c.scope, Scope::Anywhere);
        assert!(!plan.paused);
    }

    #[test]
    fn explicit_threshold_and_button() {
        let mut cfg = Config::default();
        cfg.triggers[0].drag_threshold = Some(12);
        cfg.triggers[0].button = Some(Button::Right);
        cfg.triggers[0].modifiers = vec![Modifier::Alt, Modifier::Shift];
        let plan = build_plan(&cfg, (4, 4));
        assert_eq!(plan.slots[0].threshold, (12, 12));
        assert_eq!(plan.slots[0].button, Btn::Secondary);
        assert_eq!(plan.slots[0].mods, Mods { alt: true, shift: true, ..Mods::NONE });
    }

    #[test]
    fn hold_ms_zero_falls_back_to_default() {
        let mut cfg = Config::default();
        cfg.triggers[1].hold_ms = Some(0);
        assert_eq!(build_plan(&cfg, (4, 4)).slots[1].hold_ms, DEFAULT_HOLD_MS);
        cfg.triggers[1].hold_ms = Some(700);
        assert_eq!(build_plan(&cfg, (4, 4)).slots[1].hold_ms, 700);
    }

    #[test]
    fn excluded_and_paused_come_from_config() {
        let mut cfg = Config::default();
        cfg.gestures.paused = true;
        cfg.gestures.excluded_processes = vec!["Code.exe".into(), r"C:\x\Game.EXE".into(), " ".into()];
        let plan = build_plan(&cfg, (4, 4));
        assert!(plan.paused);
        assert_eq!(plan.excluded, vec!["code.exe", "game.exe"]);
    }

    #[test]
    fn key_names() {
        assert_eq!(parse_key("Space"), Some(0x20));
        assert_eq!(parse_key("z"), Some(0x5A));
        assert_eq!(parse_key("Z"), Some(0x5A));
        assert_eq!(parse_key("7"), Some(0x37));
        assert_eq!(parse_key("F1"), Some(0x70));
        assert_eq!(parse_key("f12"), Some(0x7B));
        assert_eq!(parse_key("F25"), None);
        assert_eq!(parse_key("Enter"), Some(0x0D));
        assert_eq!(parse_key("nonsense"), None);
        assert_eq!(parse_key("-"), None);
    }

    #[test]
    fn unknown_chord_key_is_dropped() {
        let mut cfg = Config::default();
        cfg.triggers[2].key = Some("Bogus".into());
        assert!(build_plan(&cfg, (4, 4)).chords.is_empty());
    }

    #[test]
    fn scope_rules() {
        let none: Vec<String> = vec![];
        assert!(scope_allows(Scope::FileManager, "explorer.exe", false, &none, &none));
        assert!(scope_allows(Scope::FileManager, "Explorer.EXE", false, &none, &none));
        assert!(scope_allows(Scope::FileManager, "FPilot.exe", false, &none, &none));
        assert!(scope_allows(Scope::FileManager, "FilePilot.exe", false, &none, &none));
        assert!(!scope_allows(Scope::FileManager, "notepad.exe", false, &none, &none));
        let extra = vec!["totalcmd64.exe".to_string()];
        assert!(scope_allows(Scope::FileManager, "TOTALCMD64.EXE", false, &none, &extra));
        assert!(scope_allows(Scope::Anywhere, "notepad.exe", false, &none, &none));
        // own process is always rejected
        assert!(!scope_allows(Scope::Anywhere, "zest.exe", true, &none, &none));
        assert!(!scope_allows(Scope::FileManager, "explorer.exe", true, &none, &none));
        // excluded processes are rejected, with or without .exe
        let ex = vec!["notepad".to_string()];
        assert!(!scope_allows(Scope::Anywhere, "Notepad.exe", false, &ex, &none));
        let ex = vec!["explorer.exe".to_string()];
        assert!(!scope_allows(Scope::FileManager, "explorer.exe", false, &ex, &none));
        // unknown process name (lookup failed) is not a file manager
        assert!(!scope_allows(Scope::FileManager, "", false, &none, &none));
    }

    #[test]
    fn file_managers_come_from_config() {
        let mut cfg = Config::default();
        cfg.gestures.file_managers = vec!["TotalCmd64.EXE".into()];
        let plan = build_plan(&cfg, (4, 4));
        assert_eq!(plan.file_managers, vec!["totalcmd64.exe"]);
    }
}
