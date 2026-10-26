//! History list model (pure, no Windows calls): rows, text, filter, retry
//! requests and the numbers for the taskbar progress.

use std::path::{Path, PathBuf};
use zest_core::{
    ActionKind, FormatId, JobId, JobRequest, Operation, Quality, ToolKind,
};

/// The most jobs the window keeps.
pub const MAX_ENTRIES: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    /// `percent` is 0..=100 or unknown.
    Running { percent: Option<u8> },
    Done,
    Cancelled,
    /// The outputs went to the Recycle Bin (Undo).
    Undone,
    /// The job has a start line but no end line in the history file.
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobEntry {
    pub id: u64,
    /// Start time, UTC, `2026-10-18T12:34:56Z`.
    pub time: String,
    /// `Operation::key()` as written in history.jsonl.
    pub op_key: String,
    /// The operation, if this version knows the key (needed for Re-run).
    pub op: Option<Operation>,
    pub inputs: Vec<PathBuf>,
    pub outputs: Vec<PathBuf>,
    /// Failed inputs and the reason.
    pub errors: Vec<(PathBuf, String)>,
    pub status: JobStatus,
}

impl JobEntry {
    /// A job that starts now (or is read from a `started` line).
    pub fn started(id: u64, time: String, op_key: &str, inputs: Vec<PathBuf>) -> Self {
        JobEntry {
            id,
            time,
            op: parse_op_key(op_key),
            op_key: op_key.to_owned(),
            inputs,
            outputs: Vec::new(),
            errors: Vec::new(),
            status: JobStatus::Running { percent: None },
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self.status, JobStatus::Running { .. })
    }

    /// Done or not, did any input fail?
    pub fn failed(&self) -> bool {
        !self.errors.is_empty()
    }

    pub fn can_cancel(&self) -> bool {
        self.is_running()
    }

    pub fn can_undo(&self) -> bool {
        !self.is_running() && self.status != JobStatus::Undone && !self.outputs.is_empty()
    }

    pub fn can_rerun(&self) -> bool {
        !self.is_running() && self.op.is_some() && !self.inputs.is_empty()
    }

    pub fn can_retry_failed(&self) -> bool {
        self.can_rerun() && self.failed()
    }
}

// ---------------------------------------------------------------------------
// Operation names
// ---------------------------------------------------------------------------

/// Inverse of `Operation::key()`. Keys of operations that this version does
/// not know give `None`; the row is still shown, only Re-run is off.
pub fn parse_op_key(key: &str) -> Option<Operation> {
    let k = key.trim();
    if let Some(t) = ToolKind::from_slug(k) {
        return Some(Operation::Tool(t));
    }
    if let Some(a) = ActionKind::ALL.iter().chain(ActionKind::COPY_AS.iter()).find(|a| a.slug() == k) {
        return Some(Operation::Action(*a));
    }
    FormatId::from_extension(k).map(Operation::Convert)
}

fn tool_name(t: ToolKind) -> String {
    match t {
        ToolKind::Compress => "Compress".into(),
        ToolKind::Resize50 => "Resize 50%".into(),
        ToolKind::Resize25 => "Resize 25%".into(),
        ToolKind::MaxEdge1920 => "Max 1920".into(),
        ToolKind::MaxEdge1280 => "Max 1280".into(),
        ToolKind::StripMetadata => "Strip metadata".into(),
        ToolKind::RotateCw => "Rotate 90\u{B0} right".into(),
        ToolKind::RotateCcw => "Rotate 90\u{B0} left".into(),
        ToolKind::Rotate180 => "Rotate 180\u{B0}".into(),
        ToolKind::VideoTargetSize(mb) => format!("Video to {mb} MB"),
        ToolKind::Trim { start_ms, end_ms } => {
            let to = if end_ms == 0 { "end".to_owned() } else { zest_core::format_time_ms(end_ms) };
            format!("Trim {} \u{2013} {to}", zest_core::format_time_ms(start_ms))
        }
        ToolKind::Crop { w, h, .. } => format!("Crop {w} \u{D7} {h}"),
        // Tools that are added later: the wheel label is a fair name.
        other => other.label().to_owned(),
    }
}

fn ext_label(p: &Path) -> Option<&'static str> {
    p.extension().and_then(|e| FormatId::from_extension(&e.to_string_lossy())).map(FormatId::label)
}

/// "HEIC \u{2192} JPG", "Compress", "Zip".
pub fn op_label(op_key: &str, inputs: &[PathBuf]) -> String {
    match parse_op_key(op_key) {
        Some(Operation::Convert(target)) => {
            let mut from: Vec<&str> = inputs.iter().filter_map(|p| ext_label(p)).collect();
            from.dedup();
            from.sort_unstable();
            from.dedup();
            match from.as_slice() {
                [one] => format!("{one} \u{2192} {}", target.label()),
                _ => format!("Convert \u{2192} {}", target.label()),
            }
        }
        Some(Operation::Tool(t)) => tool_name(t),
        Some(Operation::Action(a)) => a.label().trim_end_matches('\u{2026}').to_owned(),
        None => op_key.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Times
// ---------------------------------------------------------------------------

/// Seconds since 1970 for `YYYY-MM-DDTHH:MM:SSZ`.
pub fn parse_utc(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b' ') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (h, mi, se) = (n(11..13)?, n(14..16)?, n(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    // Days from civil date (Howard Hinnant).
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2.rem_euclid(400);
    let mp = if mo > 2 { mo - 3 } else { mo + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + se)
}

fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// Local time for the list. `now_utc` is the current time in seconds,
/// `offset_min` is the local offset to UTC in minutes. Today: "14:05".
/// This year: "Oct 17, 14:05". Older: "2025-10-17 14:05".
pub fn format_time(iso: &str, now_utc: i64, offset_min: i32) -> String {
    let Some(t) = parse_utc(iso) else { return iso.to_owned() };
    let off = i64::from(offset_min) * 60;
    let local = t + off;
    let now_local = now_utc + off;
    let (day, now_day) = (local.div_euclid(86_400), now_local.div_euclid(86_400));
    let secs = local.rem_euclid(86_400);
    let hm = format!("{:02}:{:02}", secs / 3600, secs % 3600 / 60);
    let (y, m, d) = civil(day);
    let (ny, _, _) = civil(now_day);
    if day == now_day {
        hm
    } else if y == ny {
        format!("{} {d}, {hm}", MONTHS[(m - 1) as usize])
    } else {
        format!("{y:04}-{m:02}-{d:02} {hm}")
    }
}

// ---------------------------------------------------------------------------
// Row text
// ---------------------------------------------------------------------------

pub const COLUMNS: [&str; 4] = ["Time", "Operation", "Items", "Result"];

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string_lossy().into_owned())
}

/// "photo.heic" or "3 files \u{B7} photo.heic".
pub fn items_text(inputs: &[PathBuf]) -> String {
    match inputs {
        [] => String::new(),
        [one] => file_name(one),
        [first, ..] => format!("{} files \u{B7} {}", inputs.len(), file_name(first)),
    }
}

/// "\u{2713}", "\u{2715} 2 failed", "Cancelled", "Running 40 %", "Undone".
pub fn result_text(e: &JobEntry) -> String {
    match e.status {
        JobStatus::Running { percent: Some(p) } => format!("Running {p} %"),
        JobStatus::Running { percent: None } => "Running\u{2026}".to_owned(),
        JobStatus::Cancelled => "Cancelled".to_owned(),
        JobStatus::Undone => "Undone".to_owned(),
        JobStatus::Interrupted => "Interrupted".to_owned(),
        JobStatus::Done if e.failed() => format!("\u{2715} {} failed", e.errors.len()),
        JobStatus::Done => "\u{2713}".to_owned(),
    }
}

pub fn cell_text(e: &JobEntry, col: usize, now_utc: i64, offset_min: i32) -> String {
    match col {
        0 => format_time(&e.time, now_utc, offset_min),
        1 => op_label(&e.op_key, &e.inputs),
        2 => items_text(&e.inputs),
        3 => result_text(e),
        _ => String::new(),
    }
}

/// Does the entry match the search text? Empty text matches all. The text
/// is searched in the names of the inputs and outputs and in the operation name.
pub fn matches(e: &JobEntry, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return true;
    }
    e.inputs.iter().chain(e.outputs.iter()).any(|p| file_name(p).to_lowercase().contains(&q))
        || op_label(&e.op_key, &e.inputs).to_lowercase().contains(&q)
}

/// The text of "Copy error": one line per failed input.
pub fn error_text(e: &JobEntry) -> String {
    e.errors
        .iter()
        .map(|(p, msg)| format!("{}: {}", file_name(p), msg))
        .collect::<Vec<_>>()
        .join("\r\n")
}

// ---------------------------------------------------------------------------
// Re-run and retry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RerunError {
    /// This version does not know the operation.
    UnknownOperation,
    /// Every input is gone (or no input failed, for "retry failed").
    NoInputs,
}

/// Inputs for Re-run (all inputs) or Retry failed (only the failed ones).
/// Inputs that no longer exist are skipped. Order is kept, duplicates go.
pub fn rerun_inputs(e: &JobEntry, failed_only: bool, exists: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let source: Vec<&PathBuf> = if failed_only {
        e.errors.iter().map(|(p, _)| p).collect()
    } else {
        e.inputs.iter().collect()
    };
    let mut out: Vec<PathBuf> = Vec::new();
    for p in source {
        if !out.contains(p) && exists(p) {
            out.push(p.clone());
        }
    }
    out
}

/// What "Re-run" / "Retry failed" will do: the operation and the inputs
/// that still exist. `skipped` inputs are gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RerunPlan {
    pub op: Operation,
    pub files: Vec<PathBuf>,
    pub skipped: usize,
}

impl RerunPlan {
    /// The same job again. `id` is a new job id.
    pub fn into_request(self, id: JobId, out_dir: Option<&Path>, quality: Quality) -> JobRequest {
        JobRequest {
            id,
            files: self.files,
            op: self.op,
            quality,
            out_dir: out_dir.map(Path::to_path_buf),
            page_range: None,
        }
    }
}

pub fn plan_rerun(
    e: &JobEntry,
    failed_only: bool,
    exists: impl Fn(&Path) -> bool,
) -> Result<RerunPlan, RerunError> {
    let op = e.op.ok_or(RerunError::UnknownOperation)?;
    let wanted = rerun_inputs(e, failed_only, |_| true).len();
    let files = rerun_inputs(e, failed_only, exists);
    if files.is_empty() {
        return Err(RerunError::NoInputs);
    }
    let skipped = wanted - files.len();
    Ok(RerunPlan { op, files, skipped })
}

// ---------------------------------------------------------------------------
// Taskbar
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskbarState {
    None,
    Indeterminate,
    Normal(u8),
    Error(u8),
}

// ---------------------------------------------------------------------------
// The list
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct HistoryModel {
    entries: Vec<JobEntry>,
    /// A job failed and the user has not looked at the window since.
    unseen_failure: bool,
}

impl HistoryModel {
    pub fn entries(&self) -> &[JobEntry] {
        &self.entries
    }

    pub fn get(&self, id: u64) -> Option<&JobEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    fn get_mut(&mut self, id: u64) -> Option<&mut JobEntry> {
        self.entries.iter_mut().find(|e| e.id == id)
    }

    fn sort_and_cap(&mut self) {
        // Newest first: start time, then id.
        self.entries.sort_by(|a, b| b.time.cmp(&a.time).then(b.id.cmp(&a.id)));
        self.entries.truncate(MAX_ENTRIES);
    }

    /// Entries read from history.jsonl. Entries that are already known
    /// (live jobs) win.
    pub fn merge_loaded(&mut self, loaded: Vec<JobEntry>) {
        for e in loaded {
            if self.get(e.id).is_none() {
                self.entries.push(e);
            }
        }
        self.sort_and_cap();
    }

    /// A job that just started.
    pub fn start(&mut self, e: JobEntry) {
        self.entries.retain(|x| x.id != e.id);
        self.entries.push(e);
        self.sort_and_cap();
    }

    /// Progress 0.0..=1.0 (or unknown). True if the row changed.
    pub fn progress(&mut self, id: u64, fraction: Option<f32>) -> bool {
        let Some(e) = self.get_mut(id) else { return false };
        if !e.is_running() {
            return false;
        }
        let percent = fraction.map(|f| (f.clamp(0.0, 1.0) * 100.0).round() as u8);
        let new = JobStatus::Running { percent };
        if e.status == new {
            return false;
        }
        e.status = new;
        true
    }

    /// A live job ended.
    pub fn finish(&mut self, id: u64, outputs: Vec<PathBuf>, errors: Vec<(PathBuf, String)>, cancelled: bool) {
        let failed = !errors.is_empty();
        if let Some(e) = self.get_mut(id) {
            e.outputs = outputs;
            e.errors = errors;
            e.status = if cancelled { JobStatus::Cancelled } else { JobStatus::Done };
        }
        if failed && !cancelled {
            self.unseen_failure = true;
        }
    }

    pub fn mark_undone(&mut self, id: u64) {
        if let Some(e) = self.get_mut(id) {
            e.status = JobStatus::Undone;
        }
    }

    /// The user looked at the window: the red taskbar state can go.
    pub fn acknowledge(&mut self) {
        self.unseen_failure = false;
    }

    pub fn running_count(&self) -> usize {
        self.entries.iter().filter(|e| e.is_running()).count()
    }

    /// Row indices (into `entries`) that match the search text.
    pub fn visible(&self, query: &str) -> Vec<usize> {
        self.entries.iter().enumerate().filter(|(_, e)| matches(e, query)).map(|(i, _)| i).collect()
    }

    /// What the taskbar button shows.
    pub fn taskbar(&self) -> TaskbarState {
        let running: Vec<&JobEntry> = self.entries.iter().filter(|e| e.is_running()).collect();
        if running.is_empty() {
            return if self.unseen_failure { TaskbarState::Error(100) } else { TaskbarState::None };
        }
        let known: Vec<u32> = running
            .iter()
            .filter_map(|e| match e.status {
                JobStatus::Running { percent: Some(p) } => Some(u32::from(p)),
                _ => None,
            })
            .collect();
        if known.is_empty() {
            return if self.unseen_failure { TaskbarState::Error(100) } else { TaskbarState::Indeterminate };
        }
        // Jobs without a percent count as 0 so the bar does not jump ahead.
        let avg = (known.iter().sum::<u32>() / running.len() as u32).min(100) as u8;
        if self.unseen_failure {
            TaskbarState::Error(avg)
        } else {
            TaskbarState::Normal(avg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(v: &[&str]) -> Vec<PathBuf> {
        v.iter().map(PathBuf::from).collect()
    }

    fn entry(id: u64, time: &str, key: &str, inputs: &[&str]) -> JobEntry {
        JobEntry::started(id, time.into(), key, paths(inputs))
    }

    fn done(mut e: JobEntry, outputs: &[&str], errors: &[(&str, &str)]) -> JobEntry {
        e.outputs = paths(outputs);
        e.errors = errors.iter().map(|(p, m)| (PathBuf::from(p), (*m).to_owned())).collect();
        e.status = JobStatus::Done;
        e
    }

    #[test]
    fn operation_keys() {
        assert_eq!(parse_op_key("jpg"), Some(Operation::Convert(FormatId::Jpeg)));
        assert_eq!(parse_op_key("compress"), Some(Operation::Tool(ToolKind::Compress)));
        assert_eq!(parse_op_key("size25"), Some(Operation::Tool(ToolKind::VideoTargetSize(25))));
        assert_eq!(parse_op_key("zip"), Some(Operation::Action(ActionKind::Zip)));
        assert_eq!(parse_op_key("nonsense"), None);
        // Every key that Zest writes comes back.
        for t in ToolKind::PRESETS {
            assert_eq!(parse_op_key(&Operation::Tool(t).key()), Some(Operation::Tool(t)));
        }
        let merge = Operation::Tool(ToolKind::MergePdf);
        assert_eq!(parse_op_key(&merge.key()), Some(merge));
        for t in [ToolKind::Trim { start_ms: 1500, end_ms: 0 }, ToolKind::Crop { x: 1, y: 2, w: 30, h: 40 }] {
            assert_eq!(parse_op_key(&Operation::Tool(t).key()), Some(Operation::Tool(t)));
        }
        assert_eq!(op_label("trim-1500-65000", &[]), "Trim 00:01.500 \u{2013} 01:05.000");
        assert_eq!(op_label("crop-1-2-30-40", &[]), "Crop 30 \u{D7} 40");
        for a in ActionKind::ALL.into_iter().chain(ActionKind::COPY_AS) {
            assert_eq!(parse_op_key(&Operation::Action(a).key()), Some(Operation::Action(a)));
        }
        for f in FormatId::ALL {
            assert_eq!(parse_op_key(&Operation::Convert(f).key()), Some(Operation::Convert(f)));
        }
    }

    #[test]
    fn operation_labels() {
        let heic = paths(&[r"C:\a\x.heic", r"C:\a\y.HEIC"]);
        assert_eq!(op_label("jpg", &heic), "HEIC \u{2192} JPG");
        assert_eq!(op_label("jpg", &paths(&["a.png", "b.heic"])), "Convert \u{2192} JPG");
        assert_eq!(op_label("jpg", &[]), "Convert \u{2192} JPG");
        assert_eq!(op_label("compress", &heic), "Compress");
        assert_eq!(op_label("zip", &heic), "Zip");
        assert_eq!(op_label("size10", &heic), "Video to 10 MB");
        assert_eq!(op_label("copy-to", &heic), "Copy to");
        assert_eq!(op_label("weird", &heic), "weird");
    }

    #[test]
    fn time_parse_and_format() {
        let t = parse_utc("2026-10-18T12:34:56Z").unwrap();
        assert_eq!(t, 1_792_326_896);
        assert_eq!(parse_utc("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_utc("2000-02-29T00:00:00Z"), Some(951_782_400));
        assert_eq!(parse_utc("garbage"), None);
        assert_eq!(parse_utc("2026-13-18T12:34:56Z"), None);
        // Same day -> time only; UTC-5 offset moves 12:34 to 07:34.
        assert_eq!(format_time("2026-10-18T12:34:56Z", t + 60, -300), "07:34");
        // Yesterday, same year.
        assert_eq!(format_time("2026-10-17T12:34:56Z", t, 0), "Oct 17, 12:34");
        // Other year.
        assert_eq!(format_time("2025-10-17T23:05:00Z", t, 0), "2025-10-17 23:05");
        // The offset can move the date: 23:30 UTC is already tomorrow at UTC+2 (today).
        assert_eq!(format_time("2026-10-17T23:30:00Z", t, 120), "01:30");
        assert_eq!(format_time("2026-10-17T21:30:00Z", t, 120), "Oct 17, 23:30");
        // Unreadable text is shown as it is.
        assert_eq!(format_time("soon", t, 0), "soon");
    }

    #[test]
    fn row_text() {
        let mut e = done(entry(1, "2026-10-18T12:00:00Z", "jpg", &[r"C:\a\x.heic"]), &[r"C:\a\x.jpg"], &[]);
        assert_eq!(result_text(&e), "\u{2713}");
        assert_eq!(items_text(&e.inputs), "x.heic");
        e.inputs = paths(&[r"C:\a\x.heic", r"C:\a\y.heic", r"C:\a\z.heic"]);
        assert_eq!(items_text(&e.inputs), "3 files \u{B7} x.heic");
        e.errors = vec![(PathBuf::from(r"C:\a\y.heic"), "bad".into())];
        assert_eq!(result_text(&e), "\u{2715} 1 failed");
        e.status = JobStatus::Cancelled;
        assert_eq!(result_text(&e), "Cancelled");
        e.status = JobStatus::Running { percent: Some(40) };
        assert_eq!(result_text(&e), "Running 40 %");
        e.status = JobStatus::Running { percent: None };
        assert_eq!(result_text(&e), "Running\u{2026}");
        e.status = JobStatus::Undone;
        assert_eq!(result_text(&e), "Undone");
        assert_eq!(cell_text(&e, 1, 0, 0), "HEIC \u{2192} JPG");
        assert_eq!(cell_text(&e, 9, 0, 0), "");
    }

    #[test]
    fn filter_by_file_name() {
        let a = done(entry(1, "2026-10-18T12:00:00Z", "jpg", &[r"C:\pics\Holiday.heic"]), &[r"C:\pics\Holiday.jpg"], &[]);
        let b = done(entry(2, "2026-10-18T12:01:00Z", "zip", &[r"C:\docs\notes.txt"]), &[r"C:\docs\notes.zip"], &[]);
        assert!(matches(&a, ""));
        assert!(matches(&a, "  "));
        assert!(matches(&a, "holiday"));
        assert!(!matches(&a, "notes"));
        assert!(matches(&b, "NOTES.zip"));
        assert!(matches(&b, "zip"));
        // Folder names in the path are not searched.
        assert!(!matches(&a, "pics"));
        let mut m = HistoryModel::default();
        m.merge_loaded(vec![a, b]);
        // Newest first.
        assert_eq!(m.entries()[0].id, 2);
        assert_eq!(m.visible(""), vec![0, 1]);
        assert_eq!(m.visible("holiday"), vec![1]);
        assert!(m.visible("nothing").is_empty());
    }

    #[test]
    fn live_jobs_win_over_loaded_and_list_is_capped() {
        let mut m = HistoryModel::default();
        m.start(entry(5, "2026-10-18T12:00:00Z", "jpg", &["a.heic"]));
        let old = done(entry(5, "2026-10-18T11:00:00Z", "jpg", &["a.heic"]), &["a.jpg"], &[]);
        let mut loaded = vec![old];
        for i in 0..(MAX_ENTRIES as u64 + 20) {
            loaded.push(done(entry(1000 + i, "2026-10-01T00:00:00Z", "png", &["x.png"]), &[], &[]));
        }
        m.merge_loaded(loaded);
        assert_eq!(m.entries().len(), MAX_ENTRIES);
        assert!(m.get(5).unwrap().is_running());
        assert_eq!(m.entries()[0].id, 5);
    }

    #[test]
    fn progress_finish_undo() {
        let mut m = HistoryModel::default();
        m.start(entry(1, "2026-10-18T12:00:00Z", "compress", &["a.mp4"]));
        assert!(m.progress(1, Some(0.4)));
        assert!(!m.progress(1, Some(0.4)));
        assert_eq!(m.get(1).unwrap().status, JobStatus::Running { percent: Some(40) });
        assert!(!m.progress(9, Some(0.4)));
        assert!(m.get(1).unwrap().can_cancel());
        m.finish(1, paths(&["a-compressed.mp4"]), vec![], false);
        let e = m.get(1).unwrap();
        assert_eq!(e.status, JobStatus::Done);
        assert!(e.can_undo() && e.can_rerun() && !e.can_cancel() && !e.can_retry_failed());
        assert!(!m.progress(1, Some(0.9)));
        m.mark_undone(1);
        let e = m.get(1).unwrap();
        assert_eq!(e.status, JobStatus::Undone);
        assert!(!e.can_undo());
    }

    #[test]
    fn rerun_and_retry_requests() {
        let e = done(
            entry(7, "2026-10-18T12:00:00Z", "jpg", &["a.heic", "b.heic", "gone.heic", "a.heic"]),
            &["a.jpg"],
            &[("b.heic", "bad file"), ("gone.heic", "x"), ("b.heic", "again")],
        );
        let exists = |p: &Path| p != Path::new("gone.heic");
        let all = plan_rerun(&e, false, exists).unwrap();
        assert_eq!(all.files, paths(&["a.heic", "b.heic"]));
        assert_eq!(all.skipped, 1);
        assert_eq!(all.op, Operation::Convert(FormatId::Jpeg));
        let req = all.into_request(JobId(9), Some(Path::new(r"D:\out")), Quality::Medium);
        assert_eq!(req.id, JobId(9));
        assert_eq!(req.quality, Quality::Medium);
        assert_eq!(req.out_dir, Some(PathBuf::from(r"D:\out")));
        assert_eq!(req.op, Operation::Convert(FormatId::Jpeg));
        let retry = plan_rerun(&e, true, exists).unwrap();
        assert_eq!(retry.files, paths(&["b.heic"]));
        assert_eq!(retry.skipped, 1);
        assert_eq!(retry.into_request(JobId(10), None, Quality::High).out_dir, None);
        // Nothing left.
        assert_eq!(plan_rerun(&e, false, |_| false), Err(RerunError::NoInputs));
        let ok = done(entry(8, "t", "jpg", &["a.heic"]), &["a.jpg"], &[]);
        assert_eq!(plan_rerun(&ok, true, |_| true), Err(RerunError::NoInputs));
        let unknown = done(entry(8, "t", "mystery", &["a.heic"]), &[], &[]);
        assert_eq!(plan_rerun(&unknown, false, |_| true), Err(RerunError::UnknownOperation));
        assert!(!unknown.can_rerun());
        assert!(e.can_retry_failed());
    }

    #[test]
    fn error_lines() {
        let e = done(
            entry(1, "t", "jpg", &["a.heic"]),
            &[],
            &[(r"C:\x\a.heic", "bad file"), (r"C:\x\b.heic", "no codec")],
        );
        assert_eq!(error_text(&e), "a.heic: bad file\r\nb.heic: no codec");
    }

    #[test]
    fn taskbar_state() {
        let mut m = HistoryModel::default();
        assert_eq!(m.taskbar(), TaskbarState::None);
        m.start(entry(1, "2026-10-18T12:00:00Z", "mp4", &["a.mov"]));
        assert_eq!(m.taskbar(), TaskbarState::Indeterminate);
        m.progress(1, Some(0.5));
        assert_eq!(m.taskbar(), TaskbarState::Normal(50));
        m.start(entry(2, "2026-10-18T12:00:01Z", "mp4", &["b.mov"]));
        m.progress(2, Some(0.1));
        assert_eq!(m.taskbar(), TaskbarState::Normal(30));
        // One job fails while the other runs: red.
        m.finish(2, vec![], vec![(PathBuf::from("b.mov"), "x".into())], false);
        assert_eq!(m.taskbar(), TaskbarState::Error(50));
        m.finish(1, paths(&["a.mp4"]), vec![], false);
        assert_eq!(m.taskbar(), TaskbarState::Error(100));
        m.acknowledge();
        assert_eq!(m.taskbar(), TaskbarState::None);
        // A cancelled job is not an error.
        m.start(entry(3, "2026-10-18T12:00:02Z", "mp4", &["c.mov"]));
        m.finish(3, vec![], vec![], true);
        assert_eq!(m.taskbar(), TaskbarState::None);
        assert_eq!(m.running_count(), 0);
    }
}
