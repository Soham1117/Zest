//! Per-file pipeline: validate, plan, run, check, atomic rename.

use crate::adapter::{Adapter, CommandSpec, PlanOptions};
use crate::archive::{self, ZipError};
use crate::broker::BrokerConfig;
use crate::ffmpeg::FfmpegAdapter;
use crate::office::{LibreOfficeAdapter, PandocAdapter};
use crate::pdfium;
use crate::process::{run_capture, run_tool, RunLimits, RunOutcome};
use crate::vips::VipsAdapter;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::Storage::FileSystem::{
    MoveFileExW, SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_NORMAL,
    MOVEFILE_WRITE_THROUGH,
};
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONQUESTION, MB_YESNOCANCEL, MESSAGEBOX_RESULT};
use zest_core::{
    detect_signature, document_signature_ok, fingerprint, is_zest_temp, output_path_for,
    pages_dir_path, preferred_output_path, signature_compatible, signature_ok, temp_path,
    zip_output_path, ActionKind, Category, DuplicatePolicy, Edge, EngineId, FormatGraph, FormatId,
    JobRequest, Operation, Route, ToolKind, SIGNATURE_BYTES,
};

/// Progress of one file, reported to the broker.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stage {
    Validating,
    Running(Option<f32>),
    Finalizing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSuccess {
    pub output: PathBuf,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileError {
    Cancelled,
    Failed(String),
}

impl From<String> for FileError {
    fn from(s: String) -> Self {
        FileError::Failed(s)
    }
}

pub(crate) struct Engines {
    pub vips: VipsAdapter,
    pub ffmpeg: FfmpegAdapter,
    pub libreoffice: LibreOfficeAdapter,
    pub pandoc: PandocAdapter,
}

impl Engines {
    pub fn new(cfg: &BrokerConfig) -> Self {
        Engines {
            vips: VipsAdapter::new(cfg.tools.vips.clone()),
            ffmpeg: FfmpegAdapter::new(cfg.tools.ffmpeg.clone()),
            libreoffice: LibreOfficeAdapter::new(cfg.tools.soffice.clone().unwrap_or_default()),
            pandoc: PandocAdapter::new(cfg.tools.pandoc.clone().unwrap_or_default()),
        }
    }

    /// The adapter of an engine. PDFium has none: the worker runs in its own flow.
    fn get(&self, e: EngineId) -> Option<&dyn Adapter> {
        match e {
            EngineId::Vips => Some(&self.vips),
            EngineId::Ffmpeg => Some(&self.ffmpeg),
            EngineId::LibreOffice => Some(&self.libreoffice),
            EngineId::Pandoc => Some(&self.pandoc),
            EngineId::Pdfium => None,
        }
    }
}

/// Decide the real input format from the extension and the first bytes.
/// A readable signature wins. If it is not compatible with the extension, the
/// detected format is used. Without a known signature the file is rejected.
pub fn resolve_input_format(ext: Option<&str>, head: &[u8]) -> Result<FormatId, String> {
    let declared = ext.and_then(FormatId::from_extension);
    // Documents are checked by the rules of their own format (ZIP, OLE2, UTF-8 text).
    if let Some(d) = declared {
        if d.category() == Category::Document && document_signature_ok(d, head) {
            return Ok(d);
        }
    }
    let detected = detect_signature(head);
    match (declared, detected) {
        (Some(decl), Some(det)) if signature_compatible(decl, det) => Ok(decl),
        (_, Some(det)) => Ok(det),
        (Some(decl), None) => Err(format!("Not a valid {} file", decl.label())),
        (None, None) => Err("Not a supported media file".to_owned()),
    }
}

fn read_head(path: &Path) -> io::Result<Vec<u8>> {
    let mut f = File::open(path)?;
    let mut buf = vec![0u8; SIGNATURE_BYTES];
    let mut n = 0;
    while n < buf.len() {
        let k = f.read(&mut buf[n..])?;
        if k == 0 {
            break;
        }
        n += k;
    }
    buf.truncate(n);
    Ok(buf)
}

fn wide(p: &Path) -> Vec<u16> {
    p.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn set_hidden(p: &Path, hidden: bool) {
    let w = wide(p);
    let attr = if hidden {
        FILE_ATTRIBUTE_HIDDEN
    } else {
        FILE_ATTRIBUTE_NORMAL
    };
    // SAFETY: `w` is a NUL-terminated wide string that lives for the call.
    unsafe {
        let _ = SetFileAttributesW(PCWSTR(w.as_ptr()), attr);
    }
}

/// Rename without replacing. Fails with `AlreadyExists` if `to` exists.
fn move_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    let (a, b) = (wide(from), wide(to));
    // SAFETY: both are NUL-terminated wide strings that live for the call.
    let r = unsafe { MoveFileExW(PCWSTR(a.as_ptr()), PCWSTR(b.as_ptr()), MOVEFILE_WRITE_THROUGH) };
    r.map_err(|e| {
        let code = (e.code().0 as u32) & 0xFFFF;
        match code {
            80 | 183 => io::Error::from(io::ErrorKind::AlreadyExists),
            _ => io::Error::from_raw_os_error(code as i32),
        }
    })
}

/// Deletes the temp file on drop unless disarmed.
struct TempGuard {
    path: PathBuf,
    armed: bool,
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Deletes the temp folder on drop unless disarmed.
struct TempDirGuard {
    path: PathBuf,
    armed: bool,
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn taken(p: &Path) -> bool {
    p.exists() || temp_path(p).exists()
}

/// How the output of one file is named.
type Namer<'a> = &'a dyn Fn(&Path) -> PathBuf;

/// Reserve `final` and its temp file in `dir` (the temp file is created empty;
/// with `folder` the temp item is an empty folder).
/// `name_in` gives the first free final path for a folder.
fn reserve_in(name_in: Namer, dir: &Path, folder: bool) -> io::Result<(PathBuf, PathBuf)> {
    for _ in 0..200 {
        let fin = name_in(dir);
        let tmp = temp_path(&fin);
        let made = if folder {
            fs::create_dir(&tmp)
        } else {
            OpenOptions::new().write(true).create_new(true).open(&tmp).map(|_| ())
        };
        match made {
            Ok(()) => return Ok((fin, tmp)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("too many name collisions"))
}

/// Pick output and temp path. Falls back to the Downloads\Zest folder.
/// `default_dir` is used when `out_dir` is `None`.
fn reserve_with(
    name_in: Namer,
    default_dir: PathBuf,
    out_dir: Option<&Path>,
    fallback: &Path,
) -> Result<(PathBuf, PathBuf, Option<String>), String> {
    reserve_with_kind(name_in, default_dir, out_dir, fallback, false)
}

fn reserve_with_kind(
    name_in: Namer,
    default_dir: PathBuf,
    out_dir: Option<&Path>,
    fallback: &Path,
    folder: bool,
) -> Result<(PathBuf, PathBuf, Option<String>), String> {
    let first: PathBuf = match out_dir {
        Some(d) => d.to_path_buf(),
        None => default_dir,
    };
    let attempt = fs::create_dir_all(&first).and_then(|_| reserve_in(name_in, &first, folder));
    match attempt {
        Ok((f, t)) => Ok((f, t, None)),
        Err(first_err) => {
            let r = fs::create_dir_all(fallback)
                .and_then(|_| reserve_in(name_in, fallback, folder));
            match r {
                Ok((f, t)) => Ok((
                    f,
                    t,
                    Some(format!(
                        "Output folder is not writable; saved to {}",
                        fallback.display()
                    )),
                )),
                Err(e) => Err(format!(
                    "Cannot write output: {first_err}; fallback folder also failed: {e}"
                )),
            }
        }
    }
}

/// Reserve the output of a conversion or tool for `name_src`.
fn reserve(
    name_src: &Path,
    op: Operation,
    out_dir: Option<&Path>,
    fallback: &Path,
) -> Result<(PathBuf, PathBuf, Option<String>), String> {
    let name_in = |dir: &Path| output_path_for(name_src, op, Some(dir), taken);
    let default_dir = name_src.parent().map(Path::to_path_buf).unwrap_or_default();
    reserve_with(&name_in, default_dir, out_dir, fallback)
}

/// The path that decides the output name and extension. If the real format of
/// the input does not fit the extension (`fake.png` that is a JPEG), the
/// extension of the real format is used.
fn name_source(src: &Path, input_format: FormatId) -> PathBuf {
    let declared = src
        .extension()
        .and_then(|e| FormatId::from_extension(&e.to_string_lossy()));
    match declared {
        Some(d) if signature_compatible(d, input_format) => src.to_path_buf(),
        _ => src.with_extension(input_format.extension()),
    }
}

fn probe_duration(ffprobe: &Path, input: &Path) -> Option<f64> {
    let mut arg = OsString::new();
    if input.to_string_lossy().starts_with('-') {
        arg.push(".\\");
    }
    arg.push(input);
    let spec = CommandSpec {
        program: ffprobe.to_path_buf(),
        args: [
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ]
        .iter()
        .map(OsString::from)
        .chain(Some(arg))
        .collect(),
    };
    let out = run_capture(&spec, Duration::from_secs(15))?;
    parse_duration(&out)
}

/// True when the video has non-square SAR or a non-zero rotate tag / side data.
pub(crate) fn needs_aspect_bake(ffprobe: &Path, input: &Path) -> bool {
    let mut arg = OsString::new();
    if input.to_string_lossy().starts_with('-') {
        arg.push(".\\");
    }
    arg.push(input);
    let spec = CommandSpec {
        program: ffprobe.to_path_buf(),
        args: [
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=sample_aspect_ratio:stream_tags=rotate:stream_side_data=rotation",
            "-of",
            "default=noprint_wrappers=1",
        ]
        .iter()
        .map(OsString::from)
        .chain(Some(arg))
        .collect(),
    };
    let Some(out) = run_capture(&spec, Duration::from_secs(15)) else {
        return false;
    };
    parse_aspect_bake_needed(&out)
}

pub(crate) fn parse_aspect_bake_needed(out: &str) -> bool {
    for line in out.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("sample_aspect_ratio=") {
            let v = v.trim();
            if !v.is_empty()
                && !v.eq_ignore_ascii_case("N/A")
                && v != "1:1"
                && v != "0:1"
                && v != "1"
            {
                return true;
            }
        }
        if let Some(v) = line
            .strip_prefix("TAG:rotate=")
            .or_else(|| line.strip_prefix("rotate="))
        {
            if let Ok(deg) = v.trim().parse::<i32>() {
                if deg.rem_euclid(360) != 0 {
                    return true;
                }
            }
        }
        if let Some(v) = line.strip_prefix("rotation=") {
            if let Ok(deg) = v.trim().parse::<f64>() {
                if deg.abs() > 0.5 {
                    return true;
                }
            }
        }
    }
    false
}

pub(crate) fn parse_duration(out: &str) -> Option<f64> {
    let d: f64 = out.lines().next()?.trim().parse().ok()?;
    (d.is_finite() && d > 0.0).then_some(d)
}

/// Short error text from the tail of stderr.
pub(crate) fn error_text(tool: &str, stderr: &str, code: Option<i32>) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains("VIPS-WARNING"))
        .collect();
    let mut start = lines.len().saturating_sub(3);
    // Drop repeated lines at the front of the window.
    while start + 1 < lines.len() && lines[start] == lines[start + 1] {
        start += 1;
    }
    let mut msg = lines[start..].join("; ");
    if msg.is_empty() {
        msg = match code {
            Some(c) => format!("{tool} failed with exit code {c}"),
            None => format!("{tool} stopped without an exit code"),
        };
    }
    if msg.chars().count() > 400 {
        msg = msg.chars().take(400).collect::<String>() + "...";
    }
    msg
}

/// Folder for intermediate files of one input file. Deleted on drop.
struct ScratchGuard {
    dir: PathBuf,
}

impl Drop for ScratchGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

fn make_scratch(root: &Path) -> Result<ScratchGuard, String> {
    let dir = root.join(format!(
        "{}-{}",
        std::process::id(),
        SCRATCH_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&dir).map_err(|e| format!("Cannot create a scratch folder: {e}"))?;
    Ok(ScratchGuard { dir })
}

/// Delete scratch folders under `root` that are older than `max_age` (left by a crash).
/// Returns the number of deleted folders.
pub fn cleanup_stale_scratch(root: &Path, max_age: Duration) -> usize {
    let now = SystemTime::now();
    let Ok(rd) = fs::read_dir(root) else { return 0 };
    let mut deleted = 0;
    for e in rd.flatten() {
        let Ok(meta) = e.metadata() else { continue };
        if !meta.is_dir() {
            continue;
        }
        let age = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .unwrap_or_default();
        if age >= max_age && fs::remove_dir_all(e.path()).is_ok() {
            deleted += 1;
        }
    }
    deleted
}

/// Run one command. `timeout` (if any) ends the process and gives an error text.
fn run_limited(
    cmd: &CommandSpec,
    limits: &RunLimits,
    cancel: &AtomicBool,
    timeout: Option<Duration>,
    on_stdout: &(dyn Fn(&str) + Sync),
    on_tick: &mut dyn FnMut(),
) -> io::Result<(RunOutcome, bool)> {
    let local = AtomicBool::new(cancel.load(Ordering::SeqCst));
    let timed_out = AtomicBool::new(false);
    let start = Instant::now();
    let outcome = run_tool(cmd, limits, &local, on_stdout, &mut || {
        if cancel.load(Ordering::SeqCst) {
            local.store(true, Ordering::SeqCst);
        }
        if timeout.is_some_and(|t| start.elapsed() > t) {
            timed_out.store(true, Ordering::SeqCst);
            local.store(true, Ordering::SeqCst);
        }
        on_tick();
    })?;
    Ok((outcome, timed_out.load(Ordering::SeqCst)))
}

/// Text for a tool that ran too long.
fn timeout_text(tool: &str, limit: Option<Duration>) -> String {
    match limit {
        Some(t) => format!("{tool} did not finish within {} seconds", t.as_secs()),
        None => format!("{tool} did not finish in time"),
    }
}

/// Share of the size limit above which a target-size result is rejected (5 % over).
const TARGET_SIZE_SLACK: f64 = 1.05;

/// Process one file of a job: a conversion or a tool. Never touches the source.
pub(crate) fn convert_file(
    cfg: &BrokerConfig,
    engines: &Engines,
    graph: &FormatGraph,
    req: &JobRequest,
    src: &Path,
    cancel: &AtomicBool,
    report: &(dyn Fn(Stage) + Sync),
) -> Result<FileSuccess, FileError> {
    report(Stage::Validating);
    let src: PathBuf = std::path::absolute(src).unwrap_or_else(|_| src.to_path_buf());
    let meta = fs::metadata(&src).map_err(|e| format!("Cannot read file: {e}"))?;
    if !meta.is_file() {
        return Err("Not a file".to_owned().into());
    }
    let head = read_head(&src).map_err(|e| format!("Cannot read file: {e}"))?;
    let ext = src.extension().map(|e| e.to_string_lossy().into_owned());
    let input_format = resolve_input_format(ext.as_deref(), &head)?;
    if let Operation::Tool(ToolKind::SplitPdf) = req.op {
        if input_format != FormatId::Pdf {
            return Err("Split PDF needs a PDF file".to_owned().into());
        }
        let name_src = name_source(&src, input_format);
        let input = StepInput {
            path: &src,
            format: input_format,
            orig: &src,
            name_src,
            modified: meta.modified().ok(),
        };
        return pdf_pages(cfg, req, &src, input.orig, input.format, FormatId::Png, cancel, report);
    }
    if let Operation::Tool(ToolKind::CompressPdf) = req.op {
        if input_format != FormatId::Pdf {
            return Err("Compress PDF needs a PDF file".to_owned().into());
        }
        // Best-effort: write a copy with the compressed suffix. True PDF
        // recompression needs Ghostscript; PDFium re-save is often a no-op.
        let name_src = name_source(&src, input_format);
        let (final0, tmp, note) = reserve(&name_src, req.op, req.out_dir.as_deref(), &cfg.fallback_dir)?;
        let mut guard = TempGuard { path: tmp.clone(), armed: true };
        report(Stage::Running(None));
        fs::copy(&src, &tmp).map_err(|e| format!("Cannot copy the PDF: {e}"))?;
        report(Stage::Finalizing);
        let mut final_path = final0;
        for _ in 0..200 {
            match move_no_replace(&tmp, &final_path) {
                Ok(()) => {
                    guard.armed = false;
                    return Ok(FileSuccess {
                        output: final_path,
                        note: Some(
                            note.unwrap_or_else(|| {
                                "Saved a copy (lossy PDF compress needs Ghostscript)".into()
                            }),
                        ),
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    let dir = final_path.parent().map(Path::to_path_buf).unwrap_or_default();
                    final_path = output_path_for(&name_src, req.op, Some(&dir), taken);
                }
                Err(e) => return Err(format!("Cannot save the PDF: {e}").into()),
            }
        }
        return Err("Cannot find a free output name".to_owned().into());
    }
    // Engine and output format of this operation.
    let (engine, out_format) = match req.op {
        Operation::Convert(target) => match graph.edge(input_format, target) {
            Some(edge) => (edge.engine, target),
            None => {
                // No direct edge: run the planned route step by step.
                let route = graph
                    .route(input_format, target)
                    .ok_or_else(|| format!("Cannot convert {} to {}", input_format, target))?;
                return convert_routed(
                    cfg,
                    engines,
                    req,
                    &src,
                    input_format,
                    meta.modified().ok(),
                    &route,
                    cancel,
                    report,
                );
            }
        },
        Operation::Tool(tool) => {
            if !graph.tool_available(tool, input_format) {
                return Err(format!("{} does not work on {} files", tool.label(), input_format).into());
            }
            (tool.engine(input_format), tool.output_format(input_format))
        }
        Operation::Action(a) => {
            return Err(format!("\"{}\" is not a file conversion", a.label()).into());
        }
    };
    let input = StepInput {
        path: &src,
        format: input_format,
        orig: &src,
        name_src: name_source(&src, input_format),
        modified: meta.modified().ok(),
    };
    convert_core(cfg, engines, req, &input, engine, out_format, cancel, report)
}

/// The input of the last (or only) step of a conversion.
struct StepInput<'a> {
    /// File the engine reads (the source, or the output of the step before).
    path: &'a Path,
    /// Format of `path`.
    format: FormatId,
    /// The file the user picked. Names the output.
    orig: &'a Path,
    /// Path that decides the output name and extension.
    name_src: PathBuf,
    /// Modified time of the original (for "keep timestamps").
    modified: Option<SystemTime>,
}

/// Run the engine on a prepared temp file. Plan, run all commands, let the
/// engine finish. Used by the last step and by the intermediate steps.
#[allow(clippy::too_many_arguments)]
fn run_engine(
    cfg: &BrokerConfig,
    adapter: &dyn Adapter,
    req: &JobRequest,
    src: &Path,
    input_format: FormatId,
    tmp: &Path,
    cancel: &AtomicBool,
    report: &(dyn Fn(Stage) + Sync),
) -> Result<(), FileError> {
    let scratch = if adapter.needs_scratch(req, input_format) {
        Some(make_scratch(&cfg.scratch_dir)?)
    } else {
        None
    };

    let probed = if adapter.needs_duration() {
        probe_duration(&cfg.tools.ffprobe, src)
    } else {
        None
    };
    let duration = adapter.output_duration(req, probed);
    if cancel.load(Ordering::SeqCst) {
        return Err(FileError::Cancelled);
    }

    let mut opts = PlanOptions::new(input_format);
    opts.privacy_mode = cfg.privacy_mode;
    opts.vips_threads = cfg.vips_threads;
    opts.duration = probed;
    opts.scratch_dir = scratch.as_ref().map(|s| s.dir.clone());
    opts.quality = req.quality;
    if matches!(req.op, Operation::Convert(FormatId::Mkv))
        && input_format.category() == Category::Video
    {
        opts.bake_display_aspect = needs_aspect_bake(&cfg.tools.ffprobe, src);
    }
    let cmds = adapter
        .plan(req, src, tmp, &opts)
        .map_err(|e| e.to_string())?;

    report(Stage::Running(None));
    let limits = RunLimits {
        memory_bytes: cfg.memory_limit,
    };
    let n_cmds = cmds.len().max(1) as f32;
    for (ci, cmd) in cmds.iter().enumerate() {
        let tool = cmd
            .program
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "tool".to_owned());
        let mut hidden = false;
        let (outcome, timed_out) = run_limited(
            cmd,
            &limits,
            cancel,
            adapter.timeout(),
            &|line| {
                if let Some(p) = adapter.parse_progress(line, duration) {
                    // With several commands each one covers an equal part of 0..100.
                    report(Stage::Running(Some((ci as f32 * 100.0 + p) / n_cmds)));
                }
            },
            &mut || {
                // A hidden file cannot be opened for write, so hide it once the tool has it.
                if !hidden && fs::metadata(tmp).map(|m| m.len() > 0).unwrap_or(false) {
                    set_hidden(tmp, true);
                    hidden = true;
                }
            },
        )
        .map_err(|e| format!("Cannot start {tool}: {e}"))?;
        if timed_out {
            return Err(timeout_text(&tool, adapter.timeout()).into());
        }
        if outcome.cancelled {
            return Err(FileError::Cancelled);
        }
        if outcome.exit_code != Some(0) {
            return Err(error_text(&tool, &outcome.stderr_tail, outcome.exit_code).into());
        }
    }
    adapter
        .finish(req, src, tmp, &opts)
        .map_err(FileError::Failed)?;
    drop(scratch);
    Ok(())
}

/// One conversion step with a known engine and output format: reserve the
/// output next to the source, run, check, and rename into place.
#[allow(clippy::too_many_arguments)]
fn convert_core(
    cfg: &BrokerConfig,
    engines: &Engines,
    req: &JobRequest,
    input: &StepInput,
    engine: EngineId,
    out_format: FormatId,
    cancel: &AtomicBool,
    report: &(dyn Fn(Stage) + Sync),
) -> Result<FileSuccess, FileError> {
    let src = input.path;
    if engine == EngineId::Pdfium {
        return pdf_pages(cfg, req, src, input.orig, input.format, out_format, cancel, report);
    }
    let adapter = engines
        .get(engine)
        .ok_or_else(|| format!("{} is not available", engine.name()))?;
    if cancel.load(Ordering::SeqCst) {
        return Err(FileError::Cancelled);
    }

    let name_src = input.name_src.clone();
    let (final0, tmp, note) = reserve(&name_src, req.op, req.out_dir.as_deref(), &cfg.fallback_dir)?;
    let mut guard = TempGuard {
        path: tmp.clone(),
        armed: true,
    };
    run_engine(cfg, adapter, req, src, input.format, &tmp, cancel, report)?;

    report(Stage::Finalizing);
    if cancel.load(Ordering::SeqCst) {
        return Err(FileError::Cancelled);
    }
    set_hidden(&tmp, false);
    let size = fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
    if size == 0 {
        return Err("The converter produced an empty file".to_owned().into());
    }
    let out_head = read_head(&tmp).map_err(|e| format!("Cannot read output: {e}"))?;
    if !signature_ok(out_format, &out_head) {
        return Err(format!("The output is not a valid {} file", out_format).into());
    }
    if let Operation::Tool(ToolKind::VideoTargetSize(mb)) = req.op {
        let limit = f64::from(mb) * 1024.0 * 1024.0;
        if size as f64 > limit * TARGET_SIZE_SLACK {
            return Err(format!(
                "Could not reach {mb} MB: the result is {:.1} MB",
                size as f64 / (1024.0 * 1024.0)
            )
            .into());
        }
    }
    if cfg.keep_timestamps {
        if let Some(modified) = input.modified {
            if let Ok(f) = OpenOptions::new().write(true).open(&tmp) {
                let _ = f.set_modified(modified);
            }
        }
    }

    match resolve_duplicate(cfg, &name_src, req.op, req.out_dir.as_deref(), &tmp, &final0, note)? {
        DupResolve::Done(s) => {
            guard.armed = false;
            Ok(s)
        }
        DupResolve::Continue { final_path, note } => {
            let mut final_path = final_path;
            for _ in 0..200 {
                match move_no_replace(&tmp, &final_path) {
                    Ok(()) => {
                        guard.armed = false;
                        return Ok(FileSuccess {
                            output: final_path,
                            note,
                        });
                    }
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                        let dir = final_path.parent().map(Path::to_path_buf).unwrap_or_default();
                        final_path = output_path_for(&name_src, req.op, Some(&dir), taken);
                    }
                    Err(e) => return Err(format!("Cannot save the output: {e}").into()),
                }
            }
            Err("Cannot find a free output name".to_owned().into())
        }
    }
}

enum DupResolve {
    Done(FileSuccess),
    Continue { final_path: PathBuf, note: Option<String> },
}

/// When the preferred name already exists and matches the new temp (size + hash),
/// apply `on_duplicate` / ask the user. Replace only when `allow_replace`.
fn resolve_duplicate(
    cfg: &BrokerConfig,
    name_src: &Path,
    op: Operation,
    out_dir: Option<&Path>,
    tmp: &Path,
    reserved: &Path,
    note: Option<String>,
) -> Result<DupResolve, FileError> {
    let preferred = preferred_output_path(name_src, op, out_dir);
    if !preferred.exists() {
        return Ok(DupResolve::Continue { final_path: reserved.to_path_buf(), note });
    }
    let Some(fp_new) = fingerprint(tmp) else {
        return Ok(DupResolve::Continue { final_path: reserved.to_path_buf(), note });
    };
    let Some(fp_old) = fingerprint(&preferred) else {
        return Ok(DupResolve::Continue { final_path: reserved.to_path_buf(), note });
    };
    if fp_new != fp_old {
        return Ok(DupResolve::Continue { final_path: reserved.to_path_buf(), note });
    }

    let policy = match cfg.on_duplicate {
        DuplicatePolicy::Ask => ask_duplicate_choice(&preferred, cfg.allow_replace),
        DuplicatePolicy::Skip => DupChoice::Skip,
        DuplicatePolicy::Number => DupChoice::Number,
        DuplicatePolicy::Replace if cfg.allow_replace => DupChoice::Replace,
        DuplicatePolicy::Replace => DupChoice::Number,
    };

    match policy {
        DupChoice::Skip => {
            let _ = fs::remove_file(tmp);
            Ok(DupResolve::Done(FileSuccess {
                output: preferred,
                note: Some("Identical file already exists — skipped".into()),
            }))
        }
        DupChoice::Number => Ok(DupResolve::Continue { final_path: reserved.to_path_buf(), note }),
        DupChoice::Replace => {
            // Replace existing: delete preferred, then move tmp onto that name.
            let _ = fs::remove_file(&preferred);
            match move_no_replace(tmp, &preferred) {
                Ok(()) => Ok(DupResolve::Done(FileSuccess {
                    output: preferred,
                    note: Some("Replaced identical existing file".into()),
                })),
                Err(e) => Err(format!("Cannot replace the existing file: {e}").into()),
            }
        }
    }
}

#[derive(Clone, Copy)]
enum DupChoice {
    Skip,
    Number,
    Replace,
}

fn ask_duplicate_choice(existing: &Path, allow_replace: bool) -> DupChoice {
    let name = existing.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let text = if allow_replace {
        format!(
            "An identical file already exists:\n{name}\n\nYes = Skip (keep existing)\nNo = Write anyway (new name)\nCancel = Replace existing"
        )
    } else {
        format!(
            "An identical file already exists:\n{name}\n\nYes = Skip (keep existing)\nNo = Write anyway (new name)"
        )
    };
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: NUL-terminated message; no owner window (worker thread).
    let r = unsafe {
        MessageBoxW(
            HWND::default(),
            PCWSTR(wide.as_ptr()),
            w!("Zest — duplicate file"),
            MB_YESNOCANCEL | MB_ICONQUESTION,
        )
    };
    match r {
        MESSAGEBOX_RESULT(6) => DupChoice::Skip,  // IDYES
        MESSAGEBOX_RESULT(7) => DupChoice::Number, // IDNO
        MESSAGEBOX_RESULT(2) if allow_replace => DupChoice::Replace, // IDCANCEL
        _ => DupChoice::Skip,
    }
}

/// Run a route of two or more steps for one file.
///
/// Steps before the last write into a private scratch folder (named like a
/// normal temp output, then renamed inside the folder). Only the last step goes
/// through a temp file and an atomic rename next to the source. The scratch
/// folder is deleted on success, on failure and on cancel. Progress is split
/// evenly across the steps. The PDF pages step makes a folder, so it can only be last.
#[allow(clippy::too_many_arguments)]
fn convert_routed(
    cfg: &BrokerConfig,
    engines: &Engines,
    req: &JobRequest,
    src: &Path,
    input_format: FormatId,
    modified: Option<SystemTime>,
    route: &Route,
    cancel: &AtomicBool,
    report: &(dyn Fn(Stage) + Sync),
) -> Result<FileSuccess, FileError> {
    let n = route.steps.len();
    if n < 2 {
        return Err("Not a multi-step route".to_owned().into());
    }
    if route.steps[..n - 1].iter().any(|e| e.engine == EngineId::Pdfium) {
        return Err("A folder of pages can only be the last step".to_owned().into());
    }
    let scratch = make_scratch(&cfg.scratch_dir)?;
    let mut current: PathBuf = src.to_path_buf();
    let mut current_format = input_format;
    for (i, edge) in route.steps.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            return Err(FileError::Cancelled);
        }
        let last = i + 1 == n;
        let step_req = JobRequest {
            op: Operation::Convert(edge.to),
            ..req.clone()
        };
        let share = |p: f32| (i as f32 * 100.0 + p) / n as f32;
        let step_report = |stage: Stage| match stage {
            Stage::Validating => {}
            Stage::Running(None) => report(Stage::Running(if i == 0 { None } else { Some(share(0.0)) })),
            Stage::Running(Some(p)) => report(Stage::Running(Some(share(p)))),
            Stage::Finalizing => {
                if last {
                    report(Stage::Finalizing);
                }
            }
        };
        if last {
            let input = StepInput {
                path: &current,
                format: current_format,
                orig: src,
                name_src: name_source(src, input_format),
                modified,
            };
            return convert_core(
                cfg,
                engines,
                &step_req,
                &input,
                edge.engine,
                edge.to,
                cancel,
                &step_report,
            );
        }
        let out = scratch.dir.join(format!("step{}.{}", i + 1, edge.to.extension()));
        run_intermediate(
            cfg,
            engines,
            &step_req,
            &current,
            current_format,
            edge,
            &out,
            cancel,
            &step_report,
        )?;
        current = out;
        current_format = edge.to;
    }
    Err("Empty route".to_owned().into())
}

/// One step of a route that is not the last: write `out` inside the scratch folder.
#[allow(clippy::too_many_arguments)]
fn run_intermediate(
    cfg: &BrokerConfig,
    engines: &Engines,
    req: &JobRequest,
    input: &Path,
    input_format: FormatId,
    edge: &Edge,
    out: &Path,
    cancel: &AtomicBool,
    report: &(dyn Fn(Stage) + Sync),
) -> Result<(), FileError> {
    let adapter = engines
        .get(edge.engine)
        .ok_or_else(|| format!("{} is not available", edge.engine.name()))?;
    let tmp = temp_path(out);
    File::create(&tmp).map_err(|e| format!("Cannot create a scratch file: {e}"))?;
    run_engine(cfg, adapter, req, input, input_format, &tmp, cancel, report)?;
    if cancel.load(Ordering::SeqCst) {
        return Err(FileError::Cancelled);
    }
    set_hidden(&tmp, false);
    let size = fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
    if size == 0 {
        return Err("The converter produced an empty file".to_owned().into());
    }
    let head = read_head(&tmp).map_err(|e| format!("Cannot read output: {e}"))?;
    if !signature_ok(edge.to, &head) {
        return Err(format!("The output is not a valid {} file", edge.to).into());
    }
    fs::rename(&tmp, out).map_err(|e| format!("Cannot store the intermediate file: {e}"))?;
    Ok(())
}

/// PDF to PNG/JPG: the worker writes one image per page into `<name> (pages).zest-tmp`,
/// then the folder is renamed to `<name> (pages)`.
///
/// `src` is the PDF to read. `name_src` is the file the user picked: it names the
/// folder (they differ when the PDF is a step of a longer route).
#[allow(clippy::too_many_arguments)]
fn pdf_pages(
    cfg: &BrokerConfig,
    req: &JobRequest,
    src: &Path,
    name_src: &Path,
    input_format: FormatId,
    out_format: FormatId,
    cancel: &AtomicBool,
    report: &(dyn Fn(Stage) + Sync),
) -> Result<FileSuccess, FileError> {
    if input_format != FormatId::Pdf {
        return Err(format!("Cannot convert {input_format} to {out_format}").into());
    }
    let name_in = |dir: &Path| pages_dir_path(name_src, Some(dir), taken);
    let default_dir = name_src.parent().map(Path::to_path_buf).unwrap_or_default();
    let (final0, tmp, note) = reserve_with_kind(
        &name_in,
        default_dir,
        req.out_dir.as_deref(),
        &cfg.fallback_dir,
        true,
    )?;
    let mut guard = TempDirGuard {
        path: tmp.clone(),
        armed: true,
    };
    let cmd = pdfium::pages_command_range(
        &cfg.tools.worker,
        &cfg.tools.pdfium,
        out_format,
        req.quality,
        src,
        &tmp,
        req.page_range.as_deref(),
    )
        .map_err(|e| e.to_string())?;
    report(Stage::Running(None));
    let limits = RunLimits {
        memory_bytes: cfg.memory_limit,
    };
    let (outcome, _) = run_limited(
        &cmd,
        &limits,
        cancel,
        None,
        &|line| {
            if let Some(p) = pdfium::parse_progress(line) {
                report(Stage::Running(Some(p)));
            }
        },
        &mut || {},
    )
    .map_err(|e| format!("Cannot start zest-worker: {e}"))?;
    if outcome.cancelled {
        return Err(FileError::Cancelled);
    }
    if outcome.exit_code != Some(0) {
        return Err(worker_error(&outcome).into());
    }

    report(Stage::Finalizing);
    if cancel.load(Ordering::SeqCst) {
        return Err(FileError::Cancelled);
    }
    // Every page must be a valid image.
    let mut pages = 0usize;
    for e in fs::read_dir(&tmp).map_err(|e| format!("Cannot read the result: {e}"))?.flatten() {
        let head = read_head(&e.path()).map_err(|e| format!("Cannot read the result: {e}"))?;
        if !signature_ok(out_format, &head) {
            return Err(format!("A page is not a valid {out_format} file").into());
        }
        pages += 1;
    }
    if pages == 0 {
        return Err("The PDF produced no pages".to_owned().into());
    }
    let mut final_path = final0;
    for _ in 0..200 {
        match move_no_replace(&tmp, &final_path) {
            Ok(()) => {
                guard.armed = false;
                return Ok(FileSuccess {
                    output: final_path,
                    note,
                });
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let dir = final_path.parent().map(Path::to_path_buf).unwrap_or_default();
                final_path = pages_dir_path(name_src, Some(&dir), taken);
            }
            Err(e) => return Err(format!("Cannot save the pages: {e}").into()),
        }
    }
    Err("Cannot find a free output name".to_owned().into())
}

/// Error text of a failed worker: its `error: ...` line.
fn worker_error(o: &RunOutcome) -> String {
    let line = o
        .stderr_tail
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let msg = line.strip_prefix("error: ").unwrap_or(line);
    if msg.is_empty() {
        error_text("zest-worker", &o.stderr_tail, o.exit_code)
    } else {
        msg.to_owned()
    }
}

/// Merge PDFs: all files of the job, in the given order, become `<first name>-merged.pdf`.
pub(crate) fn merge_pdfs(
    cfg: &BrokerConfig,
    req: &JobRequest,
    cancel: &AtomicBool,
    report: &(dyn Fn(Stage) + Sync),
) -> Result<FileSuccess, FileError> {
    report(Stage::Validating);
    let items: Vec<PathBuf> = req
        .files
        .iter()
        .map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone()))
        .collect();
    if items.len() < 2 {
        return Err("Merging needs at least two PDF files".to_owned().into());
    }
    for it in &items {
        let head = read_head(it).map_err(|e| format!("Cannot read {}: {e}", it.display()))?;
        if !document_signature_ok(FormatId::Pdf, &head) {
            return Err(format!("Not a valid PDF file: {}", it.display()).into());
        }
    }
    if cancel.load(Ordering::SeqCst) {
        return Err(FileError::Cancelled);
    }
    let op = req.op;
    let first = items[0].clone();
    let (final0, tmp, note) = reserve(&first, op, req.out_dir.as_deref(), &cfg.fallback_dir)?;
    let mut guard = TempGuard {
        path: tmp.clone(),
        armed: true,
    };
    let cmd = pdfium::merge_command(&cfg.tools.worker, &cfg.tools.pdfium, &tmp, &items)
        .map_err(|e| e.to_string())?;
    report(Stage::Running(None));
    let limits = RunLimits {
        memory_bytes: cfg.memory_limit,
    };
    let (outcome, _) = run_limited(
        &cmd,
        &limits,
        cancel,
        None,
        &|line| {
            if let Some(p) = pdfium::parse_progress(line) {
                report(Stage::Running(Some(p)));
            }
        },
        &mut || {},
    )
    .map_err(|e| format!("Cannot start zest-worker: {e}"))?;
    if outcome.cancelled {
        return Err(FileError::Cancelled);
    }
    if outcome.exit_code != Some(0) {
        return Err(worker_error(&outcome).into());
    }

    report(Stage::Finalizing);
    if cancel.load(Ordering::SeqCst) {
        return Err(FileError::Cancelled);
    }
    let head = read_head(&tmp).map_err(|e| format!("Cannot read output: {e}"))?;
    if !document_signature_ok(FormatId::Pdf, &head) {
        return Err("The output is not a valid PDF file".to_owned().into());
    }
    let mut final_path = final0;
    for _ in 0..200 {
        match move_no_replace(&tmp, &final_path) {
            Ok(()) => {
                guard.armed = false;
                return Ok(FileSuccess {
                    output: final_path,
                    note,
                });
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let dir = final_path.parent().map(Path::to_path_buf).unwrap_or_default();
                final_path = output_path_for(&first, op, Some(&dir), taken);
            }
            Err(e) => return Err(format!("Cannot save the merged PDF: {e}").into()),
        }
    }
    Err("Cannot find a free output name".to_owned().into())
}

/// Copy as PNG/JPG: convert one image into `clip_dir` (the clipboard folder of the job).
/// An image that already has the target format is copied as it is.
pub(crate) fn copy_as_file(
    cfg: &BrokerConfig,
    engines: &Engines,
    graph: &FormatGraph,
    req: &JobRequest,
    src: &Path,
    cancel: &AtomicBool,
    report: &(dyn Fn(Stage) + Sync),
) -> Result<FileSuccess, FileError> {
    let Operation::Action(ActionKind::CopyAs(target)) = req.op else {
        return Err("Not a Copy as job".to_owned().into());
    };
    let clip_dir = req
        .out_dir
        .clone()
        .ok_or_else(|| "No clipboard folder".to_owned())?;
    let src_abs: PathBuf = std::path::absolute(src).unwrap_or_else(|_| src.to_path_buf());
    report(Stage::Validating);
    let head = read_head(&src_abs).map_err(|e| format!("Cannot read file: {e}"))?;
    let ext = src_abs.extension().map(|e| e.to_string_lossy().into_owned());
    let input_format = resolve_input_format(ext.as_deref(), &head)?;
    if input_format.category() != Category::Image {
        return Err(format!("{input_format} is not an image").into());
    }
    if input_format != target {
        let convert = JobRequest {
            op: Operation::Convert(target),
            out_dir: Some(clip_dir),
            ..req.clone()
        };
        return convert_file(cfg, engines, graph, &convert, &src_abs, cancel, report);
    }
    // Same format: copy through a temp file.
    let name_src = name_source(&src_abs, input_format);
    let (final0, tmp, _) = reserve(&name_src, req.op, Some(&clip_dir), &cfg.fallback_dir)?;
    let mut guard = TempGuard {
        path: tmp.clone(),
        armed: true,
    };
    report(Stage::Running(None));
    fs::copy(&src_abs, &tmp).map_err(|e| format!("Cannot copy the file: {e}"))?;
    let mut final_path = final0;
    for _ in 0..200 {
        match move_no_replace(&tmp, &final_path) {
            Ok(()) => {
                guard.armed = false;
                return Ok(FileSuccess {
                    output: final_path,
                    note: None,
                });
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let dir = final_path.parent().map(Path::to_path_buf).unwrap_or_default();
                final_path = output_path_for(&name_src, req.op, Some(&dir), taken);
            }
            Err(e) => return Err(format!("Cannot save the copy: {e}").into()),
        }
    }
    Err("Cannot find a free output name".to_owned().into())
}

/// The Zip action: all items of the job go into one archive.
/// The archive is written next to the first item (or in the output folder), as
/// `<name>.zip`, through a temp file and an atomic rename.
pub(crate) fn zip_files(
    cfg: &BrokerConfig,
    req: &JobRequest,
    cancel: &AtomicBool,
    report: &(dyn Fn(Stage) + Sync),
) -> Result<FileSuccess, FileError> {
    debug_assert_eq!(req.op, Operation::Action(ActionKind::Zip));
    report(Stage::Validating);
    let items: Vec<PathBuf> = req
        .files
        .iter()
        .map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone()))
        .collect();
    let Some(first) = items.first() else {
        return Err("Nothing to zip".to_owned().into());
    };
    for it in &items {
        if let Err(e) = fs::symlink_metadata(it) {
            return Err(format!("Cannot read {}: {e}", it.display()).into());
        }
    }
    if cancel.load(Ordering::SeqCst) {
        return Err(FileError::Cancelled);
    }

    let is_dir = |p: &Path| p.is_dir();
    let name_in = |dir: &Path| zip_output_path(&items, is_dir, Some(dir), taken);
    let default_dir = first.parent().map(Path::to_path_buf).unwrap_or_default();
    let (final0, tmp, note) = reserve_with(&name_in, default_dir, req.out_dir.as_deref(), &cfg.fallback_dir)?;
    let mut guard = TempGuard {
        path: tmp.clone(),
        armed: true,
    };

    report(Stage::Running(None));
    let result = archive::zip_items(&items, &tmp, cancel, &|p| report(Stage::Running(Some(p))));
    match result {
        Ok(()) => {}
        Err(ZipError::Cancelled) => return Err(FileError::Cancelled),
        Err(ZipError::Failed(m)) => return Err(FileError::Failed(m)),
    }

    report(Stage::Finalizing);
    if cancel.load(Ordering::SeqCst) {
        return Err(FileError::Cancelled);
    }
    let size = fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
    if size == 0 {
        return Err("The zip file is empty".to_owned().into());
    }
    let mut final_path = final0;
    for _ in 0..200 {
        match move_no_replace(&tmp, &final_path) {
            Ok(()) => {
                guard.armed = false;
                return Ok(FileSuccess {
                    output: final_path,
                    note,
                });
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let dir = final_path.parent().map(Path::to_path_buf).unwrap_or_default();
                final_path = zip_output_path(&items, is_dir, Some(&dir), taken);
            }
            Err(e) => return Err(format!("Cannot save the zip file: {e}").into()),
        }
    }
    Err("Cannot find a free output name".to_owned().into())
}

/// Delete `*.zest-tmp` files older than one day in the given folders (not recursive).
/// Returns the number of deleted files.
pub fn cleanup_stale_temps(dirs: &[PathBuf]) -> usize {
    cleanup_stale_temps_older_than(dirs, Duration::from_secs(24 * 60 * 60))
}

/// Same as [`cleanup_stale_temps`] with a custom age limit.
pub fn cleanup_stale_temps_older_than(dirs: &[PathBuf], max_age: Duration) -> usize {
    let now = SystemTime::now();
    let mut deleted = 0;
    for dir in dirs {
        let Ok(rd) = fs::read_dir(dir) else { continue };
        for entry in rd.flatten() {
            let path = entry.path();
            if !is_zest_temp(&path) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let age = meta
                .modified()
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .unwrap_or_default();
            if age < max_age {
                continue;
            }
            if meta.is_dir() {
                // Page folder of a PDF job that was cut off.
                if fs::remove_dir_all(&path).is_ok() {
                    deleted += 1;
                }
            } else if meta.is_file() {
                // A hidden read-only file cannot be deleted; clear attributes first.
                set_hidden(&path, false);
                if fs::remove_file(&path).is_ok() {
                    deleted += 1;
                }
            }
        }
    }
    deleted
}

#[cfg(test)]
mod tests {
    use super::*;
    use zest_core::FormatId::*;

    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0];
    const JPG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0, 0];

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("zest-pipe-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn input_format_rules() {
        assert_eq!(resolve_input_format(Some("png"), PNG), Ok(Png));
        assert_eq!(resolve_input_format(Some("PNG"), JPG), Ok(Jpeg)); // fake.png
        assert_eq!(resolve_input_format(None, JPG), Ok(Jpeg));
        assert_eq!(resolve_input_format(Some("dat"), PNG), Ok(Png));
        assert_eq!(
            resolve_input_format(Some("jpg"), b"garbage bytes here"),
            Err("Not a valid JPG file".to_owned())
        );
        assert_eq!(
            resolve_input_format(Some("xyz"), b"garbage"),
            Err("Not a supported media file".to_owned())
        );
        // Same family keeps the declared name.
        let mut webm = vec![0x1A, 0x45, 0xDF, 0xA3, 0x9F, 0x42, 0x82, 0x84];
        webm.extend_from_slice(b"webm");
        assert_eq!(resolve_input_format(Some("mkv"), &webm), Ok(Mkv));
    }

    #[test]
    fn document_input_rules() {
        // Declared document formats use their own signature rules.
        assert_eq!(resolve_input_format(Some("pdf"), b"%PDF-1.7"), Ok(Pdf));
        assert_eq!(resolve_input_format(Some("docx"), b"PK\x03\x04..."), Ok(Docx));
        assert_eq!(resolve_input_format(Some("xlsx"), b"PK\x03\x04..."), Ok(Xlsx));
        assert_eq!(resolve_input_format(Some("epub"), b"PK\x03\x04..."), Ok(Epub));
        assert_eq!(resolve_input_format(Some("doc"), &[0xD0, 0xCF, 0x11, 0xE0, 1, 2]), Ok(Doc));
        assert_eq!(resolve_input_format(Some("md"), b"# Title\n\nBM is text too"), Ok(Md));
        assert_eq!(resolve_input_format(Some("txt"), "gr\u{fc}\u{df}e".as_bytes()), Ok(Txt));
        // Wrong content.
        assert_eq!(
            resolve_input_format(Some("docx"), b"not a zip"),
            Err("Not a valid DOCX file".to_owned())
        );
        assert_eq!(
            resolve_input_format(Some("txt"), &[b'a', 0xC3, b'a']),
            Err("Not a valid TXT file".to_owned())
        );
        // A PDF with a Word name is a PDF; a PNG with a text name is a PNG.
        assert_eq!(resolve_input_format(Some("docx"), b"%PDF-1.4"), Ok(Pdf));
        assert_eq!(resolve_input_format(Some("txt"), PNG), Ok(Png));
    }

    #[test]
    fn time_limit_ends_the_tool() {
        let cmd = CommandSpec {
            program: PathBuf::from("ping.exe"),
            args: ["-n", "8", "127.0.0.1"].iter().map(OsString::from).collect(),
        };
        let cancel = AtomicBool::new(false);
        let start = Instant::now();
        let (out, timed_out) = run_limited(
            &cmd,
            &RunLimits::default(),
            &cancel,
            Some(Duration::from_millis(400)),
            &|_| {},
            &mut || {},
        )
        .unwrap();
        assert!(timed_out);
        assert!(out.cancelled);
        assert!(start.elapsed() < Duration::from_secs(5));
        assert_eq!(
            timeout_text("soffice", Some(Duration::from_secs(120))),
            "soffice did not finish within 120 seconds"
        );
        // Without a limit and without cancel the tool just runs to the end.
        let quick = CommandSpec {
            program: PathBuf::from("ping.exe"),
            args: ["-n", "1", "127.0.0.1"].iter().map(OsString::from).collect(),
        };
        let (out, timed_out) =
            run_limited(&quick, &RunLimits::default(), &cancel, None, &|_| {}, &mut || {}).unwrap();
        assert!(!timed_out && !out.cancelled);
        assert_eq!(out.exit_code, Some(0));
        // A set cancel flag stops it at once.
        cancel.store(true, Ordering::SeqCst);
        let (out, _) =
            run_limited(&cmd, &RunLimits::default(), &cancel, None, &|_| {}, &mut || {}).unwrap();
        assert!(out.cancelled);
    }

    #[test]
    fn stale_page_folders_are_cleaned_too() {
        let d = tmpdir("pagesclean");
        let folder = d.join("a (pages).zest-tmp");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("a-p001.png"), b"x").unwrap();
        assert_eq!(cleanup_stale_temps(std::slice::from_ref(&d)), 0);
        assert_eq!(cleanup_stale_temps_older_than(std::slice::from_ref(&d), Duration::ZERO), 1);
        assert!(!folder.exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn duration_parser() {
        assert_eq!(parse_duration("2.000000\n"), Some(2.0));
        assert_eq!(parse_duration("N/A\n"), None);
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("0.000000"), None);
    }

    #[test]
    fn aspect_bake_parser() {
        assert!(!parse_aspect_bake_needed("sample_aspect_ratio=1:1\n"));
        assert!(!parse_aspect_bake_needed("sample_aspect_ratio=N/A\n"));
        assert!(!parse_aspect_bake_needed("sample_aspect_ratio=0:1\n"));
        assert!(parse_aspect_bake_needed("sample_aspect_ratio=118:117\n"));
        assert!(parse_aspect_bake_needed("sample_aspect_ratio=1:1\nTAG:rotate=90\n"));
        assert!(parse_aspect_bake_needed("rotation=-90\n"));
        assert!(!parse_aspect_bake_needed("TAG:rotate=0\n"));
        assert!(!parse_aspect_bake_needed("TAG:rotate=360\n"));
    }

    #[test]
    fn error_text_picks_last_lines() {
        let s = "\n(vips.exe:1): VIPS-WARNING **: x\nVipsJpeg: Unsupported marker type 0x1b\nVipsJpeg: Unsupported marker type 0x1b\n";
        assert_eq!(
            error_text("vips", s, Some(1)),
            "VipsJpeg: Unsupported marker type 0x1b"
        );
        assert_eq!(error_text("vips", "", Some(1)), "vips failed with exit code 1");
        assert_eq!(error_text("ffmpeg", "  \n", None), "ffmpeg stopped without an exit code");
    }

    #[test]
    fn naming_fallback_and_collision() {
        let d = tmpdir("name");
        let src = d.join("photo.png");
        fs::write(&src, PNG).unwrap();
        let fb = d.join("fallback");
        let jpg = Operation::Convert(Jpeg);
        let (f1, t1, n1) = reserve(&src, jpg, None, &fb).unwrap();
        assert_eq!(f1, d.join("photo.jpg"));
        assert_eq!(t1, d.join("photo.jpg.zest-tmp"));
        assert!(n1.is_none());
        // Second reservation sees the first temp file and moves on.
        let (f2, _t2, _) = reserve(&src, jpg, None, &fb).unwrap();
        assert_eq!(f2, d.join("photo (1).jpg"));
        // Out folder is a file: not usable, so the fallback is used.
        let blocker = d.join("blocker");
        fs::write(&blocker, b"x").unwrap();
        let (f3, _t3, n3) = reserve(&src, jpg, Some(&blocker), &fb).unwrap();
        assert_eq!(f3, fb.join("photo.jpg"));
        assert!(n3.unwrap().contains("not writable"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn move_does_not_replace() {
        let d = tmpdir("move");
        let (a, b) = (d.join("a.tmp"), d.join("b.out"));
        fs::write(&a, b"1").unwrap();
        fs::write(&b, b"2").unwrap();
        let e = move_no_replace(&a, &b).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&b).unwrap(), b"2");
        fs::remove_file(&b).unwrap();
        move_no_replace(&a, &b).unwrap();
        assert_eq!(fs::read(&b).unwrap(), b"1");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn stale_temp_cleanup() {
        let d = tmpdir("clean");
        let old = d.join("a.jpg.zest-tmp");
        let fresh = d.join("b.jpg.zest-tmp");
        let other = d.join("c.jpg");
        for p in [&old, &fresh, &other] {
            fs::write(p, b"x").unwrap();
        }
        let two_days = SystemTime::now() - Duration::from_secs(2 * 24 * 3600);
        File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(two_days)
            .unwrap();
        set_hidden(&old, true);
        assert_eq!(cleanup_stale_temps(std::slice::from_ref(&d)), 1);
        assert!(!old.exists());
        assert!(fresh.exists());
        assert!(other.exists());
        assert_eq!(cleanup_stale_temps_older_than(std::slice::from_ref(&d), Duration::ZERO), 1);
        assert!(!fresh.exists());
        assert!(other.exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn tool_names_and_fake_extension() {
        let d = tmpdir("toolname");
        let src = d.join("photo.jpg");
        fs::write(&src, JPG).unwrap();
        let fb = d.join("fallback");
        let op = Operation::Tool(ToolKind::Resize50);
        let (f1, t1, _) = reserve(&src, op, None, &fb).unwrap();
        assert_eq!(f1, d.join("photo-50pct.jpg"));
        assert_eq!(t1, d.join("photo-50pct.jpg.zest-tmp"));
        let (f2, _, _) = reserve(&src, op, None, &fb).unwrap();
        assert_eq!(f2, d.join("photo-50pct (1).jpg"));
        // A PNG name on JPEG data: the name follows the real format.
        let fake = d.join("fake.png");
        assert_eq!(name_source(&fake, Jpeg), d.join("fake.jpg"));
        assert_eq!(name_source(&src, Jpeg), src);
        assert_eq!(name_source(&d.join("a.MKV"), Webm), d.join("a.MKV"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn stale_scratch_cleanup() {
        let d = tmpdir("scratch");
        let old = d.join("1-1");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("step.v"), b"x").unwrap();
        assert_eq!(cleanup_stale_scratch(&d, Duration::from_secs(3600)), 0);
        assert!(old.exists());
        assert_eq!(cleanup_stale_scratch(&d, Duration::ZERO), 1);
        assert!(!old.exists());
        let g = make_scratch(&d).unwrap();
        let path = g.dir.clone();
        assert!(path.is_dir());
        drop(g);
        assert!(!path.exists());
        let _ = fs::remove_dir_all(&d);
    }
}
