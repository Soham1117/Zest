//! Job history: one JSON line per event in `history.jsonl` (pure formatting
//! and parsing here; the file is written by the I/O thread).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use zest_panels::{JobEntry, JobStatus};

/// How many lines at the end of the file are read at startup.
pub const TAIL_LINES: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryError {
    pub path: String,
    pub error: String,
}

/// `event` is `"started"` or `"finished"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryRecord {
    pub event: String,
    pub id: u64,
    /// UTC, `2026-10-18T12:34:56Z`.
    pub time: String,
    pub inputs: Vec<String>,
    #[serde(default)]
    pub outputs: Vec<String>,
    pub target: String,
    #[serde(default)]
    pub errors: Vec<HistoryError>,
    #[serde(default)]
    pub cancelled: bool,
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

impl HistoryRecord {
    pub fn started(id: u64, time: String, inputs: &[PathBuf], target: &str) -> Self {
        HistoryRecord {
            event: "started".into(),
            id,
            time,
            inputs: inputs.iter().map(|p| s(p)).collect(),
            outputs: Vec::new(),
            target: target.into(),
            errors: Vec::new(),
            cancelled: false,
        }
    }

    pub fn finished(
        id: u64,
        time: String,
        inputs: &[PathBuf],
        outputs: &[PathBuf],
        target: &str,
        errors: &[(PathBuf, String)],
        cancelled: bool,
    ) -> Self {
        HistoryRecord {
            event: "finished".into(),
            id,
            time,
            inputs: inputs.iter().map(|p| s(p)).collect(),
            outputs: outputs.iter().map(|p| s(p)).collect(),
            target: target.into(),
            errors: errors
                .iter()
                .map(|(p, e)| HistoryError { path: s(p), error: e.clone() })
                .collect(),
            cancelled,
        }
    }

    /// The outputs of a job went to the Recycle Bin (History window, Undo).
    pub fn undone(id: u64, time: String, outputs: &[PathBuf]) -> Self {
        HistoryRecord {
            event: "undone".into(),
            id,
            time,
            inputs: Vec::new(),
            outputs: outputs.iter().map(|p| s(p)).collect(),
            target: String::new(),
            errors: Vec::new(),
            cancelled: false,
        }
    }

    /// One line, no trailing newline.
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// The last `max` lines that parse. Bad lines are skipped.
pub fn parse_tail(text: &str, max: usize) -> Vec<HistoryRecord> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let skip = lines.len().saturating_sub(max);
    lines[skip..]
        .iter()
        .filter_map(|l| serde_json::from_str::<HistoryRecord>(l).ok())
        .collect()
}

/// Ids that have a `started` line but no `finished` line.
pub fn unfinished(records: &[HistoryRecord]) -> Vec<u64> {
    records
        .iter()
        .filter(|r| r.event == "started")
        .filter(|r| !records.iter().any(|f| f.event == "finished" && f.id == r.id))
        .map(|r| r.id)
        .collect()
}

/// The jobs for the History window, oldest first. A job with a `started` line
/// and no `finished` line was interrupted (Zest does not resume jobs).
pub fn job_entries(records: &[HistoryRecord]) -> Vec<JobEntry> {
    let path = |s: &String| PathBuf::from(s);
    let mut out: Vec<JobEntry> = Vec::new();
    for r in records {
        let pos = out.iter().position(|e| e.id == r.id);
        match r.event.as_str() {
            "started" => {
                let mut e = JobEntry::started(r.id, r.time.clone(), &r.target, r.inputs.iter().map(path).collect());
                e.status = JobStatus::Interrupted;
                match pos {
                    Some(i) => out[i] = e,
                    None => out.push(e),
                }
            }
            "finished" => {
                let i = pos.unwrap_or_else(|| {
                    out.push(JobEntry::started(r.id, r.time.clone(), &r.target, r.inputs.iter().map(path).collect()));
                    out.len() - 1
                });
                let e = &mut out[i];
                e.outputs = r.outputs.iter().map(path).collect();
                e.errors = r.errors.iter().map(|x| (PathBuf::from(&x.path), x.error.clone())).collect();
                e.status = if r.cancelled { JobStatus::Cancelled } else { JobStatus::Done };
            }
            "undone" => {
                if let Some(i) = pos {
                    out[i].status = JobStatus::Undone;
                }
            }
            _ => {}
        }
    }
    out
}

/// Distinct folders of all inputs and outputs (where temp files may remain).
pub fn output_folders(records: &[HistoryRecord]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for r in records {
        for p in r.inputs.iter().chain(r.outputs.iter()) {
            if let Some(dir) = Path::new(p).parent() {
                if !dir.as_os_str().is_empty() && !out.iter().any(|d| d == dir) {
                    out.push(dir.to_path_buf());
                }
            }
        }
    }
    out
}

/// Current UTC time as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn utc_now() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    format_utc(secs)
}

pub fn format_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_formatting() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(format_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_utc(1_781_785_496), "2026-06-18T12:24:56Z");
    }

    #[test]
    fn finished_line_format() {
        let r = HistoryRecord::finished(
            7,
            "2026-10-18T12:00:00Z".into(),
            &[PathBuf::from(r"C:\a\x.heic")],
            &[PathBuf::from(r"C:\a\x.jpg")],
            "jpg",
            &[(PathBuf::from(r"C:\a\y.heic"), "bad file".into())],
            false,
        );
        assert_eq!(
            r.to_line(),
            r#"{"event":"finished","id":7,"time":"2026-10-18T12:00:00Z","inputs":["C:\\a\\x.heic"],"outputs":["C:\\a\\x.jpg"],"target":"jpg","errors":[{"path":"C:\\a\\y.heic","error":"bad file"}],"cancelled":false}"#
        );
        assert!(!r.to_line().contains('\n'));
    }

    #[test]
    fn started_line_round_trip() {
        let r = HistoryRecord::started(1, "t".into(), &[PathBuf::from("a.png")], "webp");
        let back: HistoryRecord = serde_json::from_str(&r.to_line()).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn entries_for_the_history_window() {
        let a = HistoryRecord::started(1, "2026-10-18T10:00:00Z".into(), &[PathBuf::from(r"C:\x\a.heic")], "jpg");
        let a2 = HistoryRecord::finished(
            1,
            "2026-10-18T10:00:05Z".into(),
            &[PathBuf::from(r"C:\x\a.heic")],
            &[PathBuf::from(r"C:\x\a.jpg")],
            "jpg",
            &[],
            false,
        );
        let u = HistoryRecord::undone(1, "2026-10-18T10:05:00Z".into(), &[PathBuf::from(r"C:\x\a.jpg")]);
        let b = HistoryRecord::started(2, "2026-10-18T11:00:00Z".into(), &[PathBuf::from(r"C:\x\b.mp4")], "compress");
        let c = HistoryRecord::finished(
            3,
            "2026-10-18T12:00:00Z".into(),
            &[PathBuf::from(r"C:\x\c.png"), PathBuf::from(r"C:\x\d.png")],
            &[],
            "webp",
            &[(PathBuf::from(r"C:\x\d.png"), "bad".into())],
            false,
        );
        let line = u.to_line();
        assert!(line.contains(r#""event":"undone""#));
        let list = job_entries(&[a, a2, u, b, c]);
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].status, JobStatus::Undone);
        assert_eq!(list[0].outputs, vec![PathBuf::from(r"C:\x\a.jpg")]);
        assert_eq!(list[1].status, JobStatus::Interrupted);
        assert!(list[1].op.is_some());
        // A finish line without a start line still gives a row.
        assert_eq!(list[2].status, JobStatus::Done);
        assert_eq!(list[2].errors.len(), 1);
        assert_eq!(list[2].inputs.len(), 2);
    }

    #[test]
    fn tail_and_unfinished() {
        let a = HistoryRecord::started(1, "t".into(), &[PathBuf::from(r"C:\x\a.png")], "webp");
        let a2 = HistoryRecord::finished(
            1,
            "t".into(),
            &[PathBuf::from(r"C:\x\a.png")],
            &[PathBuf::from(r"D:\out\a.webp")],
            "webp",
            &[],
            false,
        );
        let b = HistoryRecord::started(2, "t".into(), &[PathBuf::from(r"E:\y\b.png")], "jpg");
        let text = format!("{}\nnot json\n\n{}\n{}\n", a.to_line(), a2.to_line(), b.to_line());
        let all = parse_tail(&text, 200);
        assert_eq!(all.len(), 3);
        assert_eq!(unfinished(&all), vec![2]);
        // Tail keeps the last lines only (bad lines count as lines).
        assert_eq!(parse_tail(&text, 2).len(), 2);
        let dirs = output_folders(&all);
        assert_eq!(
            dirs,
            vec![PathBuf::from(r"C:\x"), PathBuf::from(r"D:\out"), PathBuf::from(r"E:\y")]
        );
    }
}
