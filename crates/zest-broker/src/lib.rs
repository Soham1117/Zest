//! Zest broker: runs vips, ffmpeg, the PDF worker and the optional LibreOffice and Pandoc
//! for conversion and tool jobs, and the Zip and Copy as... actions.
//!
//! Percent values in events are 0 to 100.
//! After a `FileDone` event the caller should send the shell change notification.

pub mod adapter;
pub mod archive;
pub mod broker;
pub mod engines;
pub mod ffmpeg;
pub mod office;
pub mod pdfium;
pub mod pipeline;
pub mod preview;
pub mod process;
pub mod tools;
pub mod vips;

pub use adapter::{Adapter, CommandSpec, PlanError, PlanOptions};
pub use broker::{
    default_clipboard_dir, default_fallback_dir, default_scratch_dir, Broker, BrokerConfig,
    BrokerEvent,
};
pub use pipeline::{
    cleanup_stale_scratch, cleanup_stale_temps, cleanup_stale_temps_older_than, resolve_input_format,
};
pub use process::{JobObject, RunLimits, RunOutcome, DEFAULT_MEMORY_LIMIT};
pub use tools::{ToolOverrides, ToolPaths};
