//! Job queue: two worker pools, per-file tasks, events.

use crate::pipeline::{
    cleanup_stale_scratch, convert_file, copy_as_file, merge_pdfs, zip_files, Engines, FileError, Stage,
};
use crate::process::DEFAULT_MEMORY_LIMIT;
use crate::tools::ToolPaths;
use crossbeam_channel::{unbounded, Receiver, Sender};
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use zest_core::{
    ActionKind, Category, Config, FormatGraph, FormatId, JobEvent, JobId, JobRequest, JobState,
    JobsConfig, Operation,
};

/// Minimum time between two progress events of one job (10 per second).
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub struct BrokerConfig {
    pub tools: ToolPaths,
    /// Threads in the video pool (at least 1).
    pub video_workers: usize,
    /// Threads in the image and audio pool (at least 1).
    pub image_workers: usize,
    /// Strip metadata from outputs.
    pub privacy_mode: bool,
    /// Copy the modified time of the source to the output.
    pub keep_timestamps: bool,
    pub on_duplicate: zest_core::DuplicatePolicy,
    pub allow_replace: bool,
    /// Memory limit per tool process. `None` = no limit.
    pub memory_limit: Option<u64>,
    /// Used when the output folder is not writable.
    pub fallback_dir: PathBuf,
    /// libvips threads per process.
    pub vips_threads: Option<u32>,
    /// Parent folder of the scratch folders for intermediate files
    /// (resize, rotate, target size).
    pub scratch_dir: PathBuf,
    /// Parent folder of the per-job folders of "Copy as..." (`<job id>\`).
    /// Folders older than one day are deleted when a broker starts.
    pub clipboard_dir: PathBuf,
}

/// `%LOCALAPPDATA%\Zest\clipboard`
pub fn default_clipboard_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Zest")
        .join("clipboard")
}

/// `%LOCALAPPDATA%\Zest\tmp`
pub fn default_scratch_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Zest")
        .join("tmp")
}

/// `%USERPROFILE%\Downloads\Zest`
pub fn default_fallback_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join("Downloads")
        .join("Zest")
}

fn cpus() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get())
}

impl BrokerConfig {
    /// Defaults: worker counts from `JobsConfig::default()`, 4 GB memory limit.
    pub fn new(tools: ToolPaths) -> Self {
        Self::with_jobs(tools, &JobsConfig::default())
    }

    pub fn with_jobs(tools: ToolPaths, jobs: &JobsConfig) -> Self {
        let n = cpus();
        BrokerConfig {
            tools,
            video_workers: jobs.video_workers(n).max(1),
            image_workers: jobs.image_workers(n).max(1),
            privacy_mode: false,
            keep_timestamps: false,
            on_duplicate: zest_core::DuplicatePolicy::Ask,
            allow_replace: false,
            memory_limit: Some(DEFAULT_MEMORY_LIMIT),
            fallback_dir: default_fallback_dir(),
            vips_threads: Some(4),
            scratch_dir: default_scratch_dir(),
            clipboard_dir: default_clipboard_dir(),
        }
    }

    /// Take workers, privacy mode and timestamp setting from the user config.
    pub fn from_config(tools: ToolPaths, cfg: &Config) -> Self {
        let mut c = Self::with_jobs(tools, &cfg.jobs);
        c.privacy_mode = cfg.output.privacy_mode;
        c.keep_timestamps = cfg.output.keep_timestamps;
        c.on_duplicate = cfg.output.on_duplicate;
        c.allow_replace = cfg.output.allow_replace;
        c
    }
}

/// Everything the broker tells the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum BrokerEvent {
    /// State change of one file of a job (`file_index` counts in `JobRequest::files`).
    /// `Running { percent }` is 0 to 100.
    Job(JobEvent),
    /// Mean progress of all files of the job, 0 to 100. At most 10 per second.
    Progress { id: JobId, percent: Option<f32> },
    /// One output file is in place (the caller may notify the shell).
    FileDone {
        id: JobId,
        file_index: usize,
        output: PathBuf,
        note: Option<String>,
    },
    /// All files of the job are finished.
    JobDone {
        id: JobId,
        outputs: Vec<PathBuf>,
        errors: Vec<(PathBuf, String)>,
        cancelled: bool,
    },
}

struct JobProgress {
    percents: Vec<f32>,
    outputs: Vec<Option<PathBuf>>,
    errors: Vec<(usize, PathBuf, String)>,
    remaining: usize,
    last_emit: Option<Instant>,
}

struct JobShared {
    req: JobRequest,
    cancel: AtomicBool,
    progress: Mutex<JobProgress>,
}

struct Task {
    job: Arc<JobShared>,
    file_index: usize,
}

struct Inner {
    cfg: BrokerConfig,
    graph: Arc<FormatGraph>,
    engines: Engines,
    events: Sender<BrokerEvent>,
    jobs: Mutex<HashMap<JobId, Arc<JobShared>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct Broker {
    inner: Arc<Inner>,
    video_tx: Option<Sender<Task>>,
    image_tx: Option<Sender<Task>>,
    threads: Vec<JoinHandle<()>>,
}

impl Broker {
    /// Start the worker pools. Events arrive on the returned receiver.
    pub fn new(config: BrokerConfig, graph: Arc<FormatGraph>) -> (Broker, Receiver<BrokerEvent>) {
        let (ev_tx, ev_rx) = unbounded();
        // Scratch folders left by a crash (older than one day).
        cleanup_stale_scratch(&config.scratch_dir, Duration::from_secs(24 * 60 * 60));
        cleanup_stale_scratch(&config.clipboard_dir, Duration::from_secs(24 * 60 * 60));
        let inner = Arc::new(Inner {
            engines: Engines::new(&config),
            cfg: config,
            graph,
            events: ev_tx,
            jobs: Mutex::new(HashMap::new()),
        });
        let (video_tx, video_rx) = unbounded::<Task>();
        let (image_tx, image_rx) = unbounded::<Task>();
        let mut threads = Vec::new();
        let pools = [
            (inner.cfg.video_workers.max(1), video_rx, "video"),
            (inner.cfg.image_workers.max(1), image_rx, "image"),
        ];
        for (n, rx, name) in pools {
            for i in 0..n {
                let rx = rx.clone();
                let inner = Arc::clone(&inner);
                let t = std::thread::Builder::new()
                    .name(format!("zest-{name}-{i}"))
                    .spawn(move || {
                        for task in rx {
                            inner.run_task(task);
                        }
                    })
                    .expect("cannot start broker worker");
                threads.push(t);
            }
        }
        (
            Broker {
                inner,
                video_tx: Some(video_tx),
                image_tx: Some(image_tx),
                threads,
            },
            ev_rx,
        )
    }

    /// Queue a job and return at once. One task per file.
    pub fn submit(&self, req: JobRequest) -> JobId {
        let id = req.id;
        // Zip and Merge PDFs are one task for all items; everything else is one task per file.
        let zip = is_batch_op(req.op);
        let n = if zip { req.files.len().min(1) } else { req.files.len() };
        // Copy as...: a fresh folder per job (an older run may have used the same id).
        if let Operation::Action(ActionKind::CopyAs(_)) = req.op {
            let dir = self.inner.clipboard_job_dir(id);
            let _ = std::fs::remove_dir_all(&dir);
            let _ = std::fs::create_dir_all(&dir);
        }
        let pools: Vec<bool> = if zip {
            vec![false; n]
        } else {
            req.files.iter().map(|p| is_video_path(p)).collect()
        };
        let job = Arc::new(JobShared {
            progress: Mutex::new(JobProgress {
                percents: vec![0.0; n],
                outputs: vec![None; n],
                errors: Vec::new(),
                remaining: n,
                last_emit: None,
            }),
            cancel: AtomicBool::new(false),
            req,
        });
        self.inner.send_job(id, 0, JobState::Accepted);
        if n == 0 {
            self.inner.send(BrokerEvent::JobDone {
                id,
                outputs: Vec::new(),
                errors: Vec::new(),
                cancelled: false,
            });
            return id;
        }
        lock(&self.inner.jobs).insert(id, Arc::clone(&job));
        for (i, video) in pools.into_iter().enumerate() {
            self.inner.send_job(id, i, JobState::Queued);
            let tx = if video { &self.video_tx } else { &self.image_tx };
            if let Some(tx) = tx {
                let _ = tx.send(Task {
                    job: Arc::clone(&job),
                    file_index: i,
                });
            }
        }
        id
    }

    /// Cancel a job: running tools are terminated, queued files are skipped.
    pub fn cancel(&self, id: JobId) {
        if let Some(job) = lock(&self.inner.jobs).get(&id) {
            job.cancel.store(true, Ordering::SeqCst);
        }
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        for job in lock(&self.inner.jobs).values() {
            job.cancel.store(true, Ordering::SeqCst);
        }
        self.video_tx = None;
        self.image_tx = None;
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// Operations that run as one task for all files.
fn is_batch_op(op: Operation) -> bool {
    match op {
        Operation::Action(ActionKind::Zip) => true,
        Operation::Tool(t) => t.is_batch(),
        _ => false,
    }
}

fn is_video_path(p: &std::path::Path) -> bool {
    p.extension()
        .and_then(|e| FormatId::from_extension(&e.to_string_lossy()))
        .is_some_and(|f| f.category() == Category::Video)
}

impl Inner {
    /// Folder for the outputs of one "Copy as..." job.
    fn clipboard_job_dir(&self, id: JobId) -> PathBuf {
        self.cfg.clipboard_dir.join(id.0.to_string())
    }

    fn send(&self, ev: BrokerEvent) {
        let _ = self.events.send(ev);
    }

    fn send_job(&self, id: JobId, file_index: usize, state: JobState) {
        self.send(BrokerEvent::Job(JobEvent {
            id,
            file_index,
            state,
        }));
    }

    fn run_task(&self, task: Task) {
        let Task { job, file_index } = task;
        let id = job.req.id;
        let src = job.req.files[file_index].clone();

        let result = if job.cancel.load(Ordering::SeqCst) {
            Err(FileError::Cancelled)
        } else {
            let report = |stage: Stage| self.report(&job, file_index, stage);
            catch_unwind(AssertUnwindSafe(|| {
                if job.req.op == Operation::Action(ActionKind::Zip) {
                    zip_files(&self.cfg, &job.req, &job.cancel, &report)
                } else if job.req.op == Operation::Tool(zest_core::ToolKind::MergePdf) {
                    merge_pdfs(&self.cfg, &job.req, &job.cancel, &report)
                } else if let Operation::Action(ActionKind::CopyAs(_)) = job.req.op {
                    let mut req = job.req.clone();
                    req.out_dir = Some(self.clipboard_job_dir(id));
                    copy_as_file(
                        &self.cfg,
                        &self.engines,
                        &self.graph,
                        &req,
                        &src,
                        &job.cancel,
                        &report,
                    )
                } else {
                    convert_file(
                        &self.cfg,
                        &self.engines,
                        &self.graph,
                        &job.req,
                        &src,
                        &job.cancel,
                        &report,
                    )
                }
            }))
            .unwrap_or_else(|_| Err(FileError::Failed("Internal error".to_owned())))
        };

        match result {
            Ok(done) => {
                {
                    let mut p = lock(&job.progress);
                    p.percents[file_index] = 100.0;
                    p.outputs[file_index] = Some(done.output.clone());
                }
                self.send(BrokerEvent::FileDone {
                    id,
                    file_index,
                    output: done.output.clone(),
                    note: done.note,
                });
                self.send_job(
                    id,
                    file_index,
                    JobState::Succeeded {
                        outputs: vec![done.output],
                    },
                );
            }
            Err(FileError::Cancelled) => self.send_job(id, file_index, JobState::Cancelled),
            Err(FileError::Failed(error)) => {
                lock(&job.progress)
                    .errors
                    .push((file_index, src, error.clone()));
                self.send_job(id, file_index, JobState::Failed { error });
            }
        }

        let finished = {
            let mut p = lock(&job.progress);
            p.remaining -= 1;
            if p.remaining == 0 {
                let outputs: Vec<PathBuf> = p.outputs.iter().flatten().cloned().collect();
                let mut errs = std::mem::take(&mut p.errors);
                errs.sort_by_key(|e| e.0);
                let errors = errs.into_iter().map(|(_, path, msg)| (path, msg)).collect();
                Some((outputs, errors))
            } else {
                None
            }
        };
        if let Some((outputs, errors)) = finished {
            lock(&self.jobs).remove(&id);
            self.send(BrokerEvent::JobDone {
                id,
                outputs,
                errors,
                cancelled: job.cancel.load(Ordering::SeqCst),
            });
        }
    }

    fn report(&self, job: &JobShared, file_index: usize, stage: Stage) {
        let id = job.req.id;
        match stage {
            Stage::Validating => self.send_job(id, file_index, JobState::Validating),
            Stage::Finalizing => self.send_job(id, file_index, JobState::Finalizing),
            Stage::Running(None) => {
                self.send_job(id, file_index, JobState::Running { percent: None })
            }
            Stage::Running(Some(pct)) => {
                let mean = {
                    let mut p = lock(&job.progress);
                    p.percents[file_index] = pct;
                    let now = Instant::now();
                    if p.last_emit.is_some_and(|t| now.duration_since(t) < PROGRESS_INTERVAL) {
                        return;
                    }
                    p.last_emit = Some(now);
                    p.percents.iter().sum::<f32>() / p.percents.len() as f32
                };
                self.send_job(id, file_index, JobState::Running { percent: Some(pct) });
                self.send(BrokerEvent::Progress {
                    id,
                    percent: Some(mean),
                });
            }
        }
    }
}
