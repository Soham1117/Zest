//! Integration tests for PDF jobs, Copy as... and the optional document engines.
//! PDF tests need `zest-worker.exe` (built by `cargo test --workspace`) and `pdfium.dll`.
//! LibreOffice and Pandoc tests run only when the tool is installed.
//! Without a tool, a test prints "SKIPPED" and passes.

use crossbeam_channel::Receiver;
use std::fs;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use zest_broker::{Broker, BrokerConfig, BrokerEvent, ToolOverrides, ToolPaths};
use zest_core::{
    is_zest_temp, ActionKind, EngineId, FormatGraph, FormatId, JobId, JobRequest, Operation,
    Quality, ToolKind,
};

static COUNTER: AtomicU32 = AtomicU32::new(1);

fn fresh_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "zest-doc-{}-{}-{name}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(r"..\..\tests\fixtures")
}

/// A small valid PDF with `pages` empty pages.
fn make_pdf(pages: usize, w: u32, h: u32) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    let mut obj = |out: &mut Vec<u8>, body: String| {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", offsets.len(), body).as_bytes());
    };
    obj(&mut out, "<< /Type /Catalog /Pages 2 0 R >>".to_owned());
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 3 + i)).collect();
    obj(
        &mut out,
        format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), pages),
    );
    for _ in 0..pages {
        obj(
            &mut out,
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] >>"),
        );
    }
    let xref = out.len();
    let n = offsets.len() + 1;
    out.extend_from_slice(format!("xref\n0 {n}\n0000000000 65535 f \n").as_bytes());
    for o in &offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {n} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}

struct Ctx {
    broker: Broker,
    rx: Receiver<BrokerEvent>,
    engines: Vec<EngineId>,
    clipboard: PathBuf,
    next_id: u64,
}

fn setup() -> Ctx {
    let tools = ToolPaths::resolve(&ToolOverrides::default());
    let engines = tools.probe();
    let mut cfg = BrokerConfig::new(tools.clone());
    cfg.fallback_dir = fresh_dir("fallback");
    cfg.scratch_dir = fresh_dir("scratch");
    let clipboard = fresh_dir("clipboard");
    cfg.clipboard_dir = clipboard.clone();
    let (broker, rx) = Broker::new(cfg, Arc::new(FormatGraph::with_available(&engines)));
    Ctx {
        broker,
        rx,
        engines,
        clipboard,
        next_id: 1,
    }
}

struct Outcome {
    events: Vec<BrokerEvent>,
}

impl Outcome {
    fn done(&self) -> (&Vec<PathBuf>, &Vec<(PathBuf, String)>) {
        self.events
            .iter()
            .find_map(|e| match e {
                BrokerEvent::JobDone { outputs, errors, .. } => Some((outputs, errors)),
                _ => None,
            })
            .expect("no JobDone")
    }
    fn outputs(&self) -> &Vec<PathBuf> {
        self.done().0
    }
    fn errors(&self) -> &Vec<(PathBuf, String)> {
        self.done().1
    }
    fn max_progress(&self) -> f32 {
        self.events
            .iter()
            .filter_map(|e| match e {
                BrokerEvent::Progress { percent: Some(p), .. } => Some(*p),
                _ => None,
            })
            .fold(0.0, f32::max)
    }
}

impl Ctx {
    fn has(&self, e: EngineId) -> bool {
        if !self.engines.contains(&e) {
            eprintln!("SKIPPED: {} not found (probe: {:?})", e.name(), self.engines);
            return false;
        }
        true
    }

    fn run(&mut self, files: Vec<PathBuf>, op: Operation, out_dir: Option<PathBuf>) -> Outcome {
        let id = JobId(self.next_id);
        self.next_id += 1;
        let req = JobRequest {
            id,
            files,
            op,
            quality: Quality::High,
            out_dir,
            page_range: None,
        };
        self.broker.submit(req);
        let mut events = Vec::new();
        let end = Instant::now() + Duration::from_secs(240);
        loop {
            let left = end.saturating_duration_since(Instant::now());
            let ev = self
                .rx
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("timeout; got {events:?}"));
            let done = matches!(&ev, BrokerEvent::JobDone { id: i, .. } if *i == id);
            events.push(ev);
            if done {
                return Outcome { events };
            }
        }
    }
}

fn leftover_temps(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| is_zest_temp(p)).collect())
        .unwrap_or_default()
}

fn file_names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn pdf_to_png_makes_a_pages_folder() {
    let mut ctx = setup();
    if !ctx.has(EngineId::Pdfium) {
        return;
    }
    let dir = fresh_dir("pages");
    let pdf = dir.join("Report.pdf");
    fs::write(&pdf, make_pdf(3, 200, 300)).unwrap();
    let before = fs::read(&pdf).unwrap();

    let out = ctx.run(vec![pdf.clone()], Operation::Convert(FormatId::Png), None);
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    let folder = dir.join("Report (pages)");
    assert_eq!(out.outputs(), &vec![folder.clone()]);
    assert_eq!(
        file_names(&folder),
        ["Report-p001.png", "Report-p002.png", "Report-p003.png"]
    );
    let png = fs::read(folder.join("Report-p002.png")).unwrap();
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
    assert!(leftover_temps(&dir).is_empty());
    assert_eq!(fs::read(&pdf).unwrap(), before, "source must not change");
    assert!(out.max_progress() > 0.0);

    // Again: the folder name never collides.
    let out2 = ctx.run(vec![pdf.clone()], Operation::Convert(FormatId::Jpeg), None);
    assert!(out2.errors().is_empty(), "{:?}", out2.errors());
    let folder2 = dir.join("Report (pages) (1)");
    assert_eq!(out2.outputs(), &vec![folder2.clone()]);
    let jpg = fs::read(folder2.join("Report-p001.jpg")).unwrap();
    assert!(jpg.starts_with(&[0xFF, 0xD8, 0xFF]));

    // Output folder from the config.
    let od = fresh_dir("pages-out");
    let out3 = ctx.run(vec![pdf], Operation::Convert(FormatId::Png), Some(od.clone()));
    assert_eq!(out3.outputs(), &vec![od.join("Report (pages)")]);
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&od);
}

#[test]
fn pdf_errors_are_clear_and_leave_nothing_behind() {
    let mut ctx = setup();
    if !ctx.has(EngineId::Pdfium) {
        return;
    }
    let dir = fresh_dir("pdf-bad");
    // Not a PDF at all.
    let fake = dir.join("fake.pdf");
    fs::write(&fake, b"hello, not a pdf").unwrap();
    let out = ctx.run(vec![fake], Operation::Convert(FormatId::Png), None);
    assert!(out.outputs().is_empty());
    assert!(out.errors()[0].1.contains("Not a valid PDF"), "{:?}", out.errors());
    // A page far above the 50 MP limit.
    let big = dir.join("big.pdf");
    fs::write(&big, make_pdf(1, 14400, 14400)).unwrap();
    let out = ctx.run(vec![big], Operation::Convert(FormatId::Png), None);
    assert!(out.errors()[0].1.contains("50 megapixels"), "{:?}", out.errors());
    assert_eq!(file_names(&dir), ["big.pdf", "fake.pdf"], "no folder or temp left");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn merge_pdfs_in_selection_order() {
    let mut ctx = setup();
    if !ctx.has(EngineId::Pdfium) || !ctx.has(EngineId::Vips) {
        return;
    }
    let dir = fresh_dir("merge");
    let (a, b, c) = (dir.join("a.pdf"), dir.join("b.pdf"), dir.join("c.pdf"));
    fs::write(&a, make_pdf(1, 200, 300)).unwrap();
    fs::write(&b, make_pdf(2, 400, 100)).unwrap();
    fs::write(&c, make_pdf(1, 300, 300)).unwrap();
    // Selection order c, a, b.
    let op = Operation::Tool(ToolKind::MergePdf);
    let out = ctx.run(vec![c.clone(), a.clone(), b.clone()], op, None);
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    let merged = dir.join("c-merged.pdf");
    assert_eq!(out.outputs(), &vec![merged.clone()]);
    assert!(fs::read(&merged).unwrap().starts_with(b"%PDF-"));
    assert!(leftover_temps(&dir).is_empty());
    // Check the order by rendering the result.
    let pages = ctx.run(vec![merged], Operation::Convert(FormatId::Png), None);
    assert!(pages.errors().is_empty(), "{:?}", pages.errors());
    let folder = &pages.outputs()[0];
    let size = |name: &str| -> (u32, u32) {
        let b = fs::read(folder.join(name)).unwrap();
        (
            u32::from_be_bytes(b[16..20].try_into().unwrap()),
            u32::from_be_bytes(b[20..24].try_into().unwrap()),
        )
    };
    assert_eq!(size("c-merged-p001.png"), (625, 625));
    assert_eq!(file_names(folder).len(), 4);
    // Page sizes at 150 DPI: points * 150 / 72. Order is c, a, b (2 pages).
    assert_eq!(size("c-merged-p002.png"), (417, 625));
    assert_eq!(size("c-merged-p003.png"), (833, 208));
    // Merging again gets a new name.
    let again = ctx.run(vec![a.clone(), b.clone()], op, None);
    assert_eq!(again.outputs(), &vec![dir.join("a-merged.pdf")]);
    // One file is not a merge.
    let one = ctx.run(vec![a], op, None);
    assert!(one.errors()[0].1.contains("at least two"), "{:?}", one.errors());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn copy_as_converts_into_the_clipboard_folder() {
    let mut ctx = setup();
    if !ctx.has(EngineId::Vips) {
        return;
    }
    let dir = fresh_dir("copyas");
    let jpg = dir.join("photo.jpg");
    fs::copy(fixtures().join("sample.jpg"), &jpg).unwrap();
    let png = dir.join("pic.png");
    fs::copy(fixtures().join("sample.png"), &png).unwrap();

    // Next job id is 1: the outputs go to <clipboard>\1\.
    let out = ctx.run(
        vec![jpg.clone(), png.clone()],
        Operation::Action(ActionKind::CopyAs(FormatId::Png)),
        None,
    );
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    assert_eq!(out.outputs().len(), 2);
    let job_dir = ctx.clipboard.join("1");
    for o in out.outputs() {
        assert!(o.starts_with(&job_dir), "{o:?}");
        assert!(fs::read(o).unwrap().starts_with(&[0x89, b'P', b'N', b'G']));
    }
    assert_eq!(file_names(&job_dir), ["photo.png", "pic.png"]);
    // Nothing is written next to the sources.
    assert_eq!(file_names(&dir), ["photo.jpg", "pic.png"]);

    // JPG from a PNG.
    let out = ctx.run(
        vec![png],
        Operation::Action(ActionKind::CopyAs(FormatId::Jpeg)),
        None,
    );
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    assert_eq!(out.outputs(), &vec![ctx.clipboard.join("2").join("pic.jpg")]);
    assert!(fs::read(&out.outputs()[0]).unwrap().starts_with(&[0xFF, 0xD8, 0xFF]));

    // A file that is not an image fails with a clear text.
    let txt = dir.join("note.txt");
    fs::write(&txt, b"hello").unwrap();
    let out = ctx.run(vec![txt], Operation::Action(ActionKind::CopyAs(FormatId::Png)), None);
    assert!(out.outputs().is_empty());
    assert!(!out.errors().is_empty());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn old_clipboard_folders_are_cleaned_at_start() {
    let tools = ToolPaths::resolve(&ToolOverrides::default());
    let clip = fresh_dir("clip-clean");
    let old = clip.join("7");
    fs::create_dir_all(&old).unwrap();
    fs::write(old.join("x.png"), b"x").unwrap();
    let two_days = std::time::SystemTime::now() - Duration::from_secs(2 * 24 * 3600);
    // A folder needs FILE_FLAG_BACKUP_SEMANTICS to be opened.
    std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(0x0200_0000)
        .open(&old)
        .unwrap()
        .set_modified(two_days)
        .unwrap();
    let fresh = clip.join("8");
    fs::create_dir_all(&fresh).unwrap();
    let mut cfg = BrokerConfig::new(tools);
    cfg.clipboard_dir = clip.clone();
    cfg.scratch_dir = fresh_dir("scratch2");
    let (_broker, _rx) = Broker::new(cfg, Arc::new(FormatGraph::with_available(&[])));
    assert!(!old.exists(), "old folder must be deleted");
    assert!(fresh.exists());
    let _ = fs::remove_dir_all(&clip);
}

#[test]
fn pandoc_markdown_to_docx_and_html() {
    let mut ctx = setup();
    if !ctx.has(EngineId::Pandoc) {
        return;
    }
    let dir = fresh_dir("pandoc");
    let md = dir.join("notes.md");
    fs::write(&md, "# Title\n\nSome *text* here.\n").unwrap();
    let out = ctx.run(vec![md.clone()], Operation::Convert(FormatId::Docx), None);
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    assert!(fs::read(dir.join("notes.docx")).unwrap().starts_with(b"PK\x03\x04"));
    let out = ctx.run(vec![md], Operation::Convert(FormatId::Html), None);
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    let html = fs::read_to_string(dir.join("notes.html")).unwrap();
    assert!(html.contains("<em>text</em>"));
    // And back: DOCX to Markdown.
    let out = ctx.run(vec![dir.join("notes.docx")], Operation::Convert(FormatId::Md), None);
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    assert!(fs::read_to_string(dir.join("notes (1).md")).unwrap().contains("Title"));
    assert!(leftover_temps(&dir).is_empty());
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn libreoffice_text_to_pdf_and_docx() {
    let mut ctx = setup();
    if !ctx.has(EngineId::LibreOffice) {
        return;
    }
    let dir = fresh_dir("lo");
    let txt = dir.join("letter.txt");
    fs::write(&txt, "Dear Zest,\n\nHello.\n").unwrap();
    let out = ctx.run(vec![txt.clone()], Operation::Convert(FormatId::Pdf), None);
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    assert!(fs::read(dir.join("letter.pdf")).unwrap().starts_with(b"%PDF-"));
    let out = ctx.run(vec![txt], Operation::Convert(FormatId::Docx), None);
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    assert!(fs::read(dir.join("letter.docx")).unwrap().starts_with(b"PK\x03\x04"));
    assert!(leftover_temps(&dir).is_empty());
    let _ = fs::remove_dir_all(&dir);
}
