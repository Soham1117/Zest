//! Integration tests for multi-hop routes (several engines in a row).
//! A test that needs a tool which is not installed prints "SKIPPED" and passes.

use crossbeam_channel::Receiver;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use zest_broker::{Broker, BrokerConfig, BrokerEvent, ToolOverrides, ToolPaths};
use zest_core::{
    is_zest_temp, EngineId, FormatGraph, FormatId, JobId, JobRequest, JobState, Operation, Quality,
};

static COUNTER: AtomicU32 = AtomicU32::new(1);

fn fresh_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "zest-route-{}-{}-{name}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn fixture(name: &str, dir: &Path) -> PathBuf {
    let from = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(r"..\..\tests\fixtures")
        .join(name);
    let to = dir.join(name);
    fs::copy(from, &to).unwrap();
    to
}

struct Ctx {
    broker: Broker,
    rx: Receiver<BrokerEvent>,
    scratch: PathBuf,
    next_id: u64,
}

/// Start a broker with `graph`. `None` (and a message) if a needed engine is missing.
fn setup(need: &[EngineId], make_graph: impl Fn(FormatGraph) -> FormatGraph) -> Option<Ctx> {
    let tools = ToolPaths::resolve(&ToolOverrides::default());
    let engines = tools.probe();
    for n in need {
        if !engines.contains(n) {
            eprintln!("SKIPPED: {} not found (probe: {engines:?})", n.name());
            return None;
        }
    }
    let mut cfg = BrokerConfig::new(tools);
    cfg.fallback_dir = fresh_dir("fallback");
    cfg.scratch_dir = fresh_dir("scratch");
    cfg.clipboard_dir = fresh_dir("clipboard");
    let scratch = cfg.scratch_dir.clone();
    let graph = make_graph(FormatGraph::with_available(&engines));
    let (broker, rx) = Broker::new(cfg, Arc::new(graph));
    Some(Ctx {
        broker,
        rx,
        scratch,
        next_id: 1,
    })
}

struct Outcome {
    events: Vec<BrokerEvent>,
}

impl Outcome {
    fn done(&self) -> (&Vec<PathBuf>, &Vec<(PathBuf, String)>, bool) {
        self.events
            .iter()
            .find_map(|e| match e {
                BrokerEvent::JobDone {
                    outputs,
                    errors,
                    cancelled,
                    ..
                } => Some((outputs, errors, *cancelled)),
                _ => None,
            })
            .expect("no JobDone")
    }

    /// Per-file percent values in the order they arrived.
    fn percents(&self) -> Vec<f32> {
        self.events
            .iter()
            .filter_map(|e| match e {
                BrokerEvent::Job(j) => match j.state {
                    JobState::Running { percent: Some(p) } => Some(p),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }
}

impl Ctx {
    fn submit(&mut self, file: &Path, target: FormatId) -> JobId {
        let id = JobId(self.next_id);
        self.next_id += 1;
        self.broker.submit(JobRequest {
            id,
            files: vec![file.to_path_buf()],
            op: Operation::Convert(target),
            quality: Quality::Medium,
            out_dir: None,
            page_range: None,
        })
    }

    fn wait(&self, id: JobId) -> Outcome {
        let mut events = Vec::new();
        let end = Instant::now() + Duration::from_secs(240);
        loop {
            let left = end.saturating_duration_since(Instant::now());
            let ev = self
                .rx
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("timeout waiting for job {id:?}"));
            let is_done = matches!(&ev, BrokerEvent::JobDone { id: i, .. } if *i == id);
            events.push(ev);
            if is_done {
                return Outcome { events };
            }
        }
    }

    fn run(&mut self, file: &Path, target: FormatId) -> Outcome {
        let id = self.submit(file, target);
        self.wait(id)
    }

    /// Scratch folders of finished jobs must be gone.
    fn assert_scratch_empty(&self) {
        let left: Vec<_> = fs::read_dir(&self.scratch)
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        assert!(left.is_empty(), "scratch not cleaned: {left:?}");
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn no_temps(dir: &Path) {
    let temps: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| is_zest_temp(p))
        .collect();
    assert!(temps.is_empty(), "temp files left: {temps:?}");
}

#[test]
fn image_chain_when_the_direct_edge_is_missing() {
    let Some(mut ctx) = setup(&[EngineId::Vips], |g| g.without_edge(FormatId::Jpeg, FormatId::Webp)) else {
        return;
    };
    let dir = fresh_dir("img");
    let src = fixture("sample.jpg", &dir);
    let before = fs::read(&src).unwrap();
    let out = ctx.run(&src, FormatId::Webp);
    let (outputs, errors, cancelled) = out.done();
    assert!(errors.is_empty(), "{errors:?}");
    assert!(!cancelled);
    assert_eq!(outputs, &vec![dir.join("sample.webp")]);
    let head = fs::read(&outputs[0]).unwrap();
    assert_eq!(&head[..4], b"RIFF");
    assert_eq!(&head[8..12], b"WEBP");
    // Only the source and the final output: the PNG step stayed in the scratch folder.
    assert_eq!(names(&dir), vec!["sample.jpg", "sample.webp"]);
    assert_eq!(fs::read(&src).unwrap(), before);
    no_temps(&dir);
    ctx.assert_scratch_empty();
    // Progress never goes back and stays within 0..=100.
    let p = out.percents();
    assert!(p.windows(2).all(|w| w[0] <= w[1]), "{p:?}");
    assert!(p.iter().all(|&v| (0.0..=100.0).contains(&v)), "{p:?}");
}

#[test]
fn video_to_flac_goes_through_wav() {
    let Some(mut ctx) = setup(&[EngineId::Ffmpeg], |g| g) else {
        return;
    };
    let graph = FormatGraph::new();
    assert_eq!(graph.route_hops(FormatId::Mp4, FormatId::Flac), Some(2));
    let dir = fresh_dir("flac");
    let src = fixture("sample.mp4", &dir);
    let out = ctx.run(&src, FormatId::Flac);
    let (outputs, errors, _) = out.done();
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(outputs, &vec![dir.join("sample.flac")]);
    assert_eq!(&fs::read(&outputs[0]).unwrap()[..4], b"fLaC");
    assert_eq!(names(&dir), vec!["sample.flac", "sample.mp4"]);
    no_temps(&dir);
    ctx.assert_scratch_empty();
}

#[test]
fn failing_first_step_leaves_nothing_behind() {
    let Some(mut ctx) = setup(&[EngineId::Vips], |g| g.without_edge(FormatId::Jpeg, FormatId::Webp)) else {
        return;
    };
    let dir = fresh_dir("fail");
    let src = fixture("corrupt.jpg", &dir);
    let out = ctx.run(&src, FormatId::Webp);
    let (outputs, errors, _) = out.done();
    assert!(outputs.is_empty());
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(names(&dir), vec!["corrupt.jpg"]);
    no_temps(&dir);
    ctx.assert_scratch_empty();
}

#[test]
fn cancel_cleans_the_scratch_folder() {
    let Some(mut ctx) = setup(&[EngineId::Ffmpeg], |g| g) else {
        return;
    };
    let dir = fresh_dir("cancel");
    let src = fixture("sample.avi", &dir);
    let id = ctx.submit(&src, FormatId::Flac);
    ctx.broker.cancel(id);
    let out = ctx.wait(id);
    let (outputs, _, _) = out.done();
    // Either the cancel won (no output) or the tiny job finished first (one output).
    assert!(outputs.len() <= 1);
    if outputs.is_empty() {
        assert_eq!(names(&dir), vec!["sample.avi"]);
    }
    no_temps(&dir);
    ctx.assert_scratch_empty();
}

#[test]
fn unreachable_target_is_a_clear_error() {
    let Some(mut ctx) = setup(&[EngineId::Vips], |g| g) else {
        return;
    };
    let dir = fresh_dir("noroute");
    let src = fixture("sample.png", &dir);
    let out = ctx.run(&src, FormatId::Pdf);
    let (_, errors, _) = out.done();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].1.contains("Cannot convert"), "{errors:?}");
    assert_eq!(names(&dir), vec!["sample.png"]);
}

#[test]
fn pandoc_chain_docx_to_epub_through_markdown() {
    let Some(mut ctx) = setup(&[EngineId::Pandoc], |g| g) else {
        return;
    };
    let dir = fresh_dir("pandoc");
    let md = dir.join("note.md");
    fs::write(&md, "# Title\n\nSome *text* in a note.\n").unwrap();
    // Markdown -> DOCX is a direct Pandoc edge.
    let first = ctx.run(&md, FormatId::Docx);
    assert!(first.done().1.is_empty(), "{:?}", first.done().1);
    let docx = dir.join("note.docx");
    assert!(docx.is_file());
    // DOCX -> EPUB has no edge: DOCX -> MD -> EPUB.
    assert_eq!(FormatGraph::new().route_hops(FormatId::Docx, FormatId::Epub), Some(2));
    let out = ctx.run(&docx, FormatId::Epub);
    let (outputs, errors, _) = out.done();
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(outputs, &vec![dir.join("note.epub")]);
    assert_eq!(&fs::read(&outputs[0]).unwrap()[..2], b"PK");
    assert_eq!(names(&dir), vec!["note.docx", "note.epub", "note.md"]);
    no_temps(&dir);
    ctx.assert_scratch_empty();
}

#[test]
fn text_to_page_images_through_pdf() {
    // Needs LibreOffice (TXT -> PDF) and PDFium (PDF -> PNG pages).
    let Some(mut ctx) = setup(&[EngineId::LibreOffice, EngineId::Pdfium], |g| g) else {
        return;
    };
    let dir = fresh_dir("pages");
    let src = dir.join("letter.txt");
    fs::write(&src, "Hello from a plain text file.\n").unwrap();
    let out = ctx.run(&src, FormatId::Png);
    let (outputs, errors, _) = out.done();
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(outputs.len(), 1);
    // The result is the pages folder; the intermediate PDF is gone.
    assert!(outputs[0].is_dir(), "{outputs:?}");
    assert_eq!(names(&dir).len(), 2, "{:?}", names(&dir));
    no_temps(&dir);
    ctx.assert_scratch_empty();
}
