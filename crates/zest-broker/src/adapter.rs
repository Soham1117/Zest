//! Engine adapter trait (spec 5.4): pure command planning and progress parsing.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use zest_core::{EngineId, FormatId, JobRequest, Quality, ToolKind};

/// One process to run: program plus argument array (no shell).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

/// Why a command line could not be planned.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("cannot convert {from} to {to}")]
    Unsupported { from: FormatId, to: FormatId },
    #[error("{} does not work on {from} files", .tool.label())]
    ToolUnsupported { tool: ToolKind, from: FormatId },
    #[error("invalid path: {0}")]
    BadPath(String),
    /// Target size: the video is too long for the size limit.
    #[error("Too long to fit in {0} MB")]
    TooLong(u32),
    #[error("{0}")]
    Other(String),
}

/// Extra inputs of `plan` that are not part of the job request.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanOptions {
    /// The real format of the input (from the signature check).
    pub input_format: FormatId,
    /// Strip metadata from the output.
    pub privacy_mode: bool,
    /// libvips worker threads per process. `None` = libvips default.
    pub vips_threads: Option<u32>,
    /// Length of the input in seconds, if known (target size needs it).
    pub duration: Option<f64>,
    /// A private folder for intermediate files of this one file. The pipeline
    /// creates it when [`Adapter::needs_scratch`] says so, and deletes it afterwards.
    pub scratch_dir: Option<PathBuf>,
    /// Input has non-square SAR and/or rotation side data. Remux targets (MKV)
    /// must re-encode so players show the same picture as the source.
    pub bake_display_aspect: bool,
    /// Job quality (Compress High/Medium/Small).
    pub quality: zest_core::Quality,
}

impl PlanOptions {
    pub fn new(input_format: FormatId) -> Self {
        PlanOptions {
            input_format,
            privacy_mode: false,
            vips_threads: Some(4),
            duration: None,
            scratch_dir: None,
            bake_display_aspect: false,
            quality: zest_core::Quality::High,
        }
    }
}

pub trait Adapter: Send + Sync {
    fn engine(&self) -> EngineId;

    /// Build the command lines. Pure function.
    /// `input` and `tmp_out` must be absolute paths.
    fn plan(
        &self,
        req: &JobRequest,
        input: &Path,
        tmp_out: &Path,
        opts: &PlanOptions,
    ) -> Result<Vec<CommandSpec>, PlanError>;

    /// Turn one stdout line into a percent value (0 to 100).
    /// `duration` is the length of the output in seconds, if known.
    fn parse_progress(&self, line: &str, duration: Option<f64>) -> Option<f32>;

    /// Length in seconds that the output will have. Default: the probed input duration.
    fn output_duration(&self, _req: &JobRequest, probed: Option<f64>) -> Option<f64> {
        probed
    }

    /// Whether this engine needs a duration probe before the run.
    fn needs_duration(&self) -> bool {
        false
    }

    /// Whether the plan for this request needs a scratch folder.
    fn needs_scratch(&self, _req: &JobRequest, _input_format: FormatId) -> bool {
        false
    }

    /// Time limit for one command. `None` = no limit.
    fn timeout(&self) -> Option<std::time::Duration> {
        None
    }

    /// Runs after all commands succeeded. An engine that cannot name its output
    /// file moves the result onto `tmp_out` here.
    fn finish(
        &self,
        _req: &JobRequest,
        _input: &Path,
        _tmp_out: &Path,
        _opts: &PlanOptions,
    ) -> Result<(), String> {
        Ok(())
    }
}

/// Make a path safe as a tool argument: a path that starts with `-` gets a `.\` prefix.
pub(crate) fn safe_arg(p: &Path) -> Result<OsString, PlanError> {
    let s = p.as_os_str();
    let lossy = s.to_string_lossy();
    if lossy.is_empty() {
        return Err(PlanError::BadPath("empty path".into()));
    }
    if lossy.starts_with('-') {
        let mut o = OsString::from(".\\");
        o.push(s);
        return Ok(o);
    }
    Ok(s.to_os_string())
}

pub(crate) fn q3<T: Copy>(q: Quality, high: T, medium: T, small: T) -> T {
    match q {
        Quality::High => high,
        Quality::Medium => medium,
        Quality::Small => small,
    }
}

pub(crate) fn os(s: &str) -> OsString {
    OsString::from(s)
}
