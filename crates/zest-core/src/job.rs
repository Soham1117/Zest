//! Job types shared by the broker and the UI (spec 5.3).

use crate::format::FormatId;
use crate::ops::Operation;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct JobId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    #[default]
    High,
    Medium,
    Small,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobRequest {
    pub id: JobId,
    pub files: Vec<PathBuf>,
    pub op: Operation,
    #[serde(default)]
    pub quality: Quality,
    /// `None` = next to the source file.
    #[serde(default)]
    pub out_dir: Option<PathBuf>,
    /// Optional 1-based page range for PDF→image (`"1-3"`, `"2"`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_range: Option<String>,
}

impl JobRequest {
    /// Target format of a conversion; `None` for tools and actions.
    pub fn target(&self) -> Option<FormatId> {
        self.op.target()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum JobState {
    Accepted,
    Queued,
    Validating,
    Running { percent: Option<f32> },
    Finalizing,
    Succeeded { outputs: Vec<PathBuf> },
    Failed { error: String },
    Cancelled,
}

impl JobState {
    /// True for Succeeded, Failed and Cancelled.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            JobState::Succeeded { .. } | JobState::Failed { .. } | JobState::Cancelled
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobEvent {
    pub id: JobId,
    pub file_index: usize,
    pub state: JobState,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_default_is_high() {
        assert_eq!(Quality::default(), Quality::High);
    }

    #[test]
    fn terminal_states() {
        assert!(!JobState::Queued.is_terminal());
        assert!(!JobState::Running { percent: Some(0.5) }.is_terminal());
        assert!(JobState::Cancelled.is_terminal());
        assert!(JobState::Failed { error: "x".into() }.is_terminal());
        assert!(JobState::Succeeded { outputs: vec![] }.is_terminal());
    }

    #[test]
    fn serde_round_trip_and_defaults() {
        let req = JobRequest {
            id: JobId(7),
            files: vec![PathBuf::from("a.heic")],
            op: Operation::Convert(FormatId::Jpeg),
            quality: Quality::Small,
            out_dir: None,
            page_range: None,
        };
        let s = toml::to_string(&req).unwrap();
        let back: JobRequest = toml::from_str(&s).unwrap();
        assert_eq!(req, back);
        let minimal: JobRequest =
            toml::from_str("id = 1\nfiles = [\"a.png\"]\nop = { convert = \"webp\" }\n").unwrap();
        assert_eq!(minimal.quality, Quality::High);
        assert_eq!(minimal.target(), Some(FormatId::Webp));
        assert_eq!(minimal.out_dir, None);
    }

    #[test]
    fn event_round_trip() {
        let ev = JobEvent {
            id: JobId(3),
            file_index: 2,
            state: JobState::Succeeded {
                outputs: vec![PathBuf::from("a.jpg")],
            },
        };
        let s = toml::to_string(&ev).unwrap();
        assert_eq!(toml::from_str::<JobEvent>(&s).unwrap(), ev);
        let ev2 = JobEvent {
            id: JobId(3),
            file_index: 0,
            state: JobState::Running { percent: None },
        };
        let s = toml::to_string(&ev2).unwrap();
        assert_eq!(toml::from_str::<JobEvent>(&s).unwrap(), ev2);
    }
}
