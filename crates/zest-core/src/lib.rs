//! Zest core: pure Rust types and logic, no OS dependencies.

pub mod config;
pub mod fingerprint;
pub mod format;
pub mod graph;
pub mod job;
pub mod naming;
pub mod ops;
pub mod route;
pub mod watch;

pub use config::{
    Button, Concurrency, Config, ConfigError, DuplicatePolicy, GestureConfig, JobsConfig, KeyChord,
    Modifier, OutputConfig, OutputLocation, ReduceAnimations, Scope, ToolsConfig, Trigger,
    TriggerKind, UiConfig, WatchEntry, WheelConfig,
};
pub use fingerprint::{fingerprint, Fingerprint};
pub use format::{
    detect_signature, document_signature_ok, signature_compatible, signature_ok, Category, FormatId,
    SIGNATURE_BYTES,
};
pub use graph::{
    Edge, EngineId, FormatGraph, InputKind, WheelEntry, WheelGroup, WheelItem, WheelModel,
    WheelPrefs, ACTIONS_LABEL, MAX_SECTORS, TOOLS_LABEL,
};
pub use job::{JobEvent, JobId, JobRequest, JobState, Quality};
pub use naming::{
    is_zest_temp, output_path, output_path_for, pages_dir_path, preferred_output_path, temp_path,
    zip_output_path,
};
pub use ops::{format_time_ms, parse_time_ms, ActionKind, Operation, ToolKind};
pub use route::{Route, MAX_HOPS};
