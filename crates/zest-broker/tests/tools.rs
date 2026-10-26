//! Integration tests for tools and the Zip action, against the real tools and
//! the fixtures in `tests/fixtures`. Every test works in a fresh temp folder.
//! Without the tools, tests print a message and pass.

use crossbeam_channel::Receiver;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use zest_broker::{Broker, BrokerConfig, BrokerEvent, ToolOverrides, ToolPaths};
use zest_core::{
    detect_signature, is_zest_temp, signature_compatible, ActionKind, EngineId, FormatGraph,
    FormatId, JobId, JobRequest, Operation, Quality, ToolKind,
};

static COUNTER: AtomicU32 = AtomicU32::new(1);

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
}

fn fresh_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "zest-tools-it-{}-{}-{name}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn copy_fixture(name: &str, dir: &Path) -> PathBuf {
    let dst = dir.join(name);
    fs::copy(fixtures().join(name), &dst).unwrap();
    dst
}

struct Ctx {
    broker: Broker,
    rx: Receiver<BrokerEvent>,
    tools: ToolPaths,
    scratch: PathBuf,
    next_id: u64,
}

fn setup() -> Option<Ctx> {
    let tools = ToolPaths::resolve(&ToolOverrides::default());
    let engines = tools.probe();
    if !engines.contains(&EngineId::Vips) || !engines.contains(&EngineId::Ffmpeg) {
        eprintln!("SKIPPED: vips/ffmpeg not found (probe: {engines:?})");
        return None;
    }
    let scratch = fresh_dir("scratch");
    let mut cfg = BrokerConfig::new(tools.clone());
    cfg.fallback_dir = fresh_dir("fallback");
    cfg.scratch_dir = scratch.clone();
    let (broker, rx) = Broker::new(cfg, Arc::new(FormatGraph::new()));
    Some(Ctx {
        broker,
        rx,
        tools,
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
    fn outputs(&self) -> &Vec<PathBuf> {
        self.done().0
    }
    fn errors(&self) -> &Vec<(PathBuf, String)> {
        self.done().1
    }
    fn progress(&self) -> Vec<f32> {
        self.events
            .iter()
            .filter_map(|e| match e {
                BrokerEvent::Progress { percent: Some(p), .. } => Some(*p),
                _ => None,
            })
            .collect()
    }
}

impl Ctx {
    fn run(&mut self, files: Vec<PathBuf>, op: Operation, out_dir: Option<PathBuf>) -> Outcome {
        let id = JobId(self.next_id);
        self.next_id += 1;
        self.broker.submit(JobRequest {
            id,
            files,
            op,
            quality: Quality::High,
            out_dir,
            page_range: None,
        });
        let mut events = Vec::new();
        let end = Instant::now() + Duration::from_secs(300);
        loop {
            let left = end.saturating_duration_since(Instant::now());
            let ev = self
                .rx
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("timeout waiting for job {id:?}; got {events:?}"));
            let is_done = matches!(&ev, BrokerEvent::JobDone { id: i, .. } if *i == id);
            events.push(ev);
            if is_done {
                return Outcome { events };
            }
        }
    }

    fn tool(&mut self, file: &Path, tool: ToolKind) -> Outcome {
        self.run(vec![file.to_path_buf()], Operation::Tool(tool), None)
    }

    /// The scratch root must be empty after a job.
    fn assert_no_scratch(&self) {
        let left: Vec<_> = fs::read_dir(&self.scratch)
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        assert!(left.is_empty(), "scratch left behind: {left:?}");
    }
}

fn leftover_temps(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| is_zest_temp(p))
                .collect()
        })
        .unwrap_or_default()
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn assert_signature(path: &Path, format: FormatId) {
    let bytes = fs::read(path).unwrap();
    assert!(!bytes.is_empty(), "{path:?} is empty");
    let det = detect_signature(&bytes[..bytes.len().min(64)]);
    assert!(
        det.is_some_and(|d| signature_compatible(format, d)),
        "{path:?} has signature {det:?}, wanted {format:?}"
    );
}

/// `vipsheader -f <field> <file>`
fn vips_field(tools: &ToolPaths, path: &Path, field: &str) -> String {
    let header = tools.vips.with_file_name("vipsheader.exe");
    let out = Command::new(header)
        .args(["-f", field])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn dims(tools: &ToolPaths, path: &Path) -> (u32, u32) {
    (
        vips_field(tools, path, "width").parse().unwrap(),
        vips_field(tools, path, "height").parse().unwrap(),
    )
}

fn has_exif(tools: &ToolPaths, path: &Path) -> bool {
    let header = tools.vips.with_file_name("vipsheader.exe");
    let out = Command::new(header).arg("-a").arg(path).output().unwrap();
    String::from_utf8_lossy(&out.stdout).contains("exif-data")
}

fn format_of(path: &Path) -> FormatId {
    FormatId::from_extension(&path.extension().unwrap().to_string_lossy()).unwrap()
}

const IMAGE_FIXTURES: [&str; 5] = ["sample.png", "sample.jpg", "sample.webp", "sample.avif", "sample.tiff"];

fn image_tools() -> Vec<ToolKind> {
    ToolKind::PRESETS[..9].to_vec()
}

fn suffix_name(fixture: &str, tool: ToolKind) -> String {
    let p = Path::new(fixture);
    format!(
        "{}-{}.{}",
        p.file_stem().unwrap().to_string_lossy(),
        tool.suffix(),
        p.extension().unwrap().to_string_lossy()
    )
}

#[test]
fn every_image_tool_on_every_writable_format() {
    let Some(mut ctx) = setup() else { return };
    for fixture in IMAGE_FIXTURES {
        let dir = fresh_dir(fixture);
        let src = copy_fixture(fixture, &dir);
        let before = fs::read(&src).unwrap();
        let fmt = format_of(&src);
        let (w, h) = dims(&ctx.tools, &src);
        for tool in image_tools() {
            let out = ctx.tool(&src, tool);
            assert!(out.errors().is_empty(), "{fixture} {tool:?}: {:?}", out.errors());
            assert_eq!(out.outputs().len(), 1, "{fixture} {tool:?}");
            let o = &out.outputs()[0];
            assert_eq!(o.parent().unwrap(), dir);
            assert_eq!(o.file_name().unwrap().to_string_lossy(), suffix_name(fixture, tool));
            assert_signature(o, fmt);
            let (ow, oh) = dims(&ctx.tools, o);
            let want = match tool {
                ToolKind::Resize50 => (w / 2, h / 2),
                ToolKind::Resize25 => (w / 4, h / 4),
                ToolKind::RotateCw | ToolKind::RotateCcw => (h, w),
                _ => (w, h), // small image: max edge does not enlarge
            };
            assert_eq!((ow, oh), want, "{fixture} {tool:?}");
            ctx.assert_no_scratch();
            assert!(leftover_temps(&dir).is_empty());
            // Remove the output so the next tool gets the plain name again.
            fs::remove_file(o).unwrap();
        }
        assert_eq!(fs::read(&src).unwrap(), before, "source changed: {fixture}");
        assert_eq!(names_in(&dir), [fixture.to_owned()]);
    }
}

#[test]
fn max_edge_shrinks_big_images_and_never_enlarges() {
    let Some(mut ctx) = setup() else { return };
    let dir = fresh_dir("maxedge");
    let big = dir.join("big.png");
    let st = Command::new(&ctx.tools.vips)
        .args(["black"])
        .arg(&big)
        .args(["2400", "1200"])
        .status()
        .unwrap();
    assert!(st.success());
    assert_eq!(dims(&ctx.tools, &big), (2400, 1200));
    let o = ctx.tool(&big, ToolKind::MaxEdge1920);
    assert!(o.errors().is_empty(), "{:?}", o.errors());
    assert_eq!(dims(&ctx.tools, &o.outputs()[0]), (1920, 960));
    assert_eq!(o.outputs()[0].file_name().unwrap(), "big-1920.png");
    let o = ctx.tool(&big, ToolKind::MaxEdge1280);
    assert_eq!(dims(&ctx.tools, &o.outputs()[0]), (1280, 640));
    // Portrait: the long edge counts.
    let tall = dir.join("tall.png");
    let st = Command::new(&ctx.tools.vips)
        .args(["black"])
        .arg(&tall)
        .args(["900", "3000"])
        .status()
        .unwrap();
    assert!(st.success());
    let o = ctx.tool(&tall, ToolKind::MaxEdge1280);
    assert_eq!(dims(&ctx.tools, &o.outputs()[0]), (384, 1280));
    // 50% of 2400x1200 and a rotate on a non-square image.
    let o = ctx.tool(&big, ToolKind::Resize25);
    assert_eq!(dims(&ctx.tools, &o.outputs()[0]), (600, 300));
    let o = ctx.tool(&big, ToolKind::RotateCw);
    assert_eq!(dims(&ctx.tools, &o.outputs()[0]), (1200, 2400));
    ctx.assert_no_scratch();
}

#[test]
fn rotate_directions_and_180_are_different_images() {
    let Some(mut ctx) = setup() else { return };
    let dir = fresh_dir("rot");
    let src = copy_fixture("sample.png", &dir);
    let vips = ctx.tools.vips.clone();
    let read_png = |p: &Path| -> Vec<u8> {
        // Raw pixels via vips so that the compression does not matter.
        let raw = p.with_extension("raw");
        let st = Command::new(&vips)
            .arg("rawsave")
            .arg(p)
            .arg(&raw)
            .status()
            .unwrap();
        assert!(st.success());
        let v = fs::read(&raw).unwrap();
        fs::remove_file(raw).unwrap();
        v
    };
    let cw = ctx.tool(&src, ToolKind::RotateCw).outputs()[0].clone();
    let ccw = ctx.tool(&src, ToolKind::RotateCcw).outputs()[0].clone();
    let r180 = ctx.tool(&src, ToolKind::Rotate180).outputs()[0].clone();
    assert_eq!(cw.file_name().unwrap(), "sample-rotated.png");
    assert_eq!(ccw.file_name().unwrap(), "sample-rotated (1).png");
    assert_eq!(r180.file_name().unwrap(), "sample-rotated (2).png");
    let (a, b, c, o) = (read_png(&cw), read_png(&ccw), read_png(&r180), read_png(&src));
    assert_ne!(a, b);
    assert_ne!(a, c);
    assert_ne!(b, c);
    assert_ne!(o, c);
    // CW then CCW is the original.
    let back = ctx.tool(&cw, ToolKind::RotateCcw).outputs()[0].clone();
    assert_eq!(read_png(&back), o);
}

#[test]
fn compress_keeps_metadata_and_strip_removes_it() {
    let Some(mut ctx) = setup() else { return };
    let dir = fresh_dir("meta");
    let src = copy_fixture("sample.jpg", &dir);
    assert!(has_exif(&ctx.tools, &src), "fixture should carry EXIF");
    let c = ctx.tool(&src, ToolKind::Compress);
    assert!(c.errors().is_empty(), "{:?}", c.errors());
    assert!(has_exif(&ctx.tools, &c.outputs()[0]), "Compress must keep metadata");
    let s = ctx.tool(&src, ToolKind::StripMetadata);
    assert!(s.errors().is_empty(), "{:?}", s.errors());
    assert!(!has_exif(&ctx.tools, &s.outputs()[0]), "Strip must remove EXIF");
    assert_eq!(s.outputs()[0].file_name().unwrap(), "sample-stripped.jpg");
    assert_signature(&s.outputs()[0], FormatId::Jpeg);
    ctx.assert_no_scratch();
}

#[test]
fn png_compress_is_lossless() {
    let Some(mut ctx) = setup() else { return };
    let dir = fresh_dir("pngc");
    let src = copy_fixture("sample.png", &dir);
    let out = ctx.tool(&src, ToolKind::Compress);
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    let o = &out.outputs()[0];
    assert_eq!(o.file_name().unwrap(), "sample-compressed.png");
    let raw = |p: &Path| {
        let r = p.with_extension("raw");
        let st = Command::new(&ctx.tools.vips).arg("rawsave").arg(p).arg(&r).status().unwrap();
        assert!(st.success());
        fs::read(r).unwrap()
    };
    assert_eq!(raw(o), raw(&src), "PNG compress must not change pixels");
}

#[test]
fn tool_names_collide_with_counter_and_keep_the_source() {
    let Some(mut ctx) = setup() else { return };
    let dir = fresh_dir("coll");
    let src = copy_fixture("sample.jpg", &dir);
    let before = fs::read(&src).unwrap();
    let a = ctx.tool(&src, ToolKind::Resize50);
    let b = ctx.tool(&src, ToolKind::Resize50);
    assert_eq!(a.outputs()[0], dir.join("sample-50pct.jpg"));
    assert_eq!(b.outputs()[0], dir.join("sample-50pct (1).jpg"));
    // A tool on its own output: new name, not the source.
    let c = ctx.tool(&a.outputs()[0].clone(), ToolKind::Resize50);
    assert_eq!(c.outputs()[0], dir.join("sample-50pct-50pct.jpg"));
    assert_eq!(fs::read(&src).unwrap(), before);
    assert!(leftover_temps(&dir).is_empty());
}

#[test]
fn tool_with_out_dir_and_fake_extension() {
    let Some(mut ctx) = setup() else { return };
    let dir = fresh_dir("outdir");
    let src = copy_fixture("fake.png", &dir); // really a JPEG
    let target = dir.join("made");
    let out = ctx.run(
        vec![src.clone()],
        Operation::Tool(ToolKind::Resize50),
        Some(target.clone()),
    );
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    assert_eq!(out.outputs()[0], target.join("fake-50pct.jpg"));
    assert_signature(&out.outputs()[0], FormatId::Jpeg);
}

#[test]
fn tool_errors_are_clear() {
    let Some(mut ctx) = setup() else { return };
    let dir = fresh_dir("errs");
    // BMP cannot be written again: no tool.
    let bmp = copy_fixture("sample.bmp", &dir);
    let out = ctx.tool(&bmp, ToolKind::Resize50);
    assert_eq!(out.errors().len(), 1);
    assert!(out.errors()[0].1.contains("does not work on BMP"), "{:?}", out.errors());
    // A video tool on an image.
    let png = copy_fixture("sample.png", &dir);
    let out = ctx.tool(&png, ToolKind::VideoTargetSize(10));
    assert_eq!(out.errors().len(), 1);
    // Corrupt input.
    let bad = copy_fixture("corrupt.jpg", &dir);
    let out = ctx.tool(&bad, ToolKind::Rotate180);
    assert_eq!(out.errors().len(), 1);
    assert!(!out.errors()[0].1.is_empty());
    // Plain text named .png.
    let txt = dir.join("note.png");
    fs::write(&txt, b"not an image").unwrap();
    let out = ctx.tool(&txt, ToolKind::Compress);
    assert!(out.errors()[0].1.contains("Not a valid PNG"), "{:?}", out.errors());
    // An action is not a file conversion.
    let out = ctx.run(vec![png], Operation::Action(ActionKind::CopyPath), None);
    assert_eq!(out.errors().len(), 1);
    assert_eq!(
        names_in(&dir),
        ["corrupt.jpg", "note.png", "sample.bmp", "sample.png"]
    );
    ctx.assert_no_scratch();
}

// ----- video and audio tools -------------------------------------------------

fn ffprobe(tools: &ToolPaths, path: &Path, args: &[&str]) -> String {
    let out = Command::new(&tools.ffprobe)
        .args(["-v", "error"])
        .args(args)
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn make_clip(tools: &ToolPaths, path: &Path, seconds: u32, size: &str, title: Option<&str>) {
    let mut c = Command::new(&tools.ffmpeg);
    c.args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i"])
        .arg(format!("testsrc2=size={size}:rate=30:duration={seconds}"))
        .args(["-f", "lavfi", "-i"])
        .arg(format!("sine=frequency=440:duration={seconds}"))
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"])
        .args(["-c:a", "aac", "-shortest"]);
    if let Some(t) = title {
        c.args(["-metadata", &format!("title={t}")]);
    }
    assert!(c.arg(path).status().unwrap().success());
}

#[test]
fn video_compress_writes_h264_mp4() {
    let Some(mut ctx) = setup() else { return };
    for fixture in ["sample.mp4", "sample.mov", "sample.mkv", "sample.webm", "sample.avi"] {
        let dir = fresh_dir(fixture);
        let src = copy_fixture(fixture, &dir);
        let out = ctx.tool(&src, ToolKind::Compress);
        assert!(out.errors().is_empty(), "{fixture}: {:?}", out.errors());
        let o = &out.outputs()[0];
        let stem = Path::new(fixture).file_stem().unwrap().to_string_lossy().into_owned();
        assert_eq!(o.file_name().unwrap().to_string_lossy(), format!("{stem}-compressed.mp4"));
        assert_signature(o, FormatId::Mp4);
        assert_eq!(
            ffprobe(&ctx.tools, o, &["-select_streams", "v:0", "-show_entries", "stream=codec_name", "-of", "csv=p=0"]),
            "h264",
            "{fixture}"
        );
        assert!(leftover_temps(&dir).is_empty());
    }
}

#[test]
fn strip_removes_tags_from_video_and_audio() {
    let Some(mut ctx) = setup() else { return };
    let dir = fresh_dir("vstrip");
    let clip = dir.join("tagged.mp4");
    make_clip(&ctx.tools, &clip, 2, "160x120", Some("Secret Title"));
    let tools = ctx.tools.clone();
    let tag = |p: &Path| ffprobe(&tools, p, &["-show_entries", "format_tags=title", "-of", "csv=p=0"]);
    assert_eq!(tag(&clip), "Secret Title");
    let out = ctx.tool(&clip, ToolKind::StripMetadata);
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    let o = &out.outputs()[0];
    assert_eq!(o.file_name().unwrap(), "tagged-stripped.mp4");
    assert_eq!(tag(o), "", "title tag must be gone");
    assert_signature(o, FormatId::Mp4);
    // Stream copy: the codec is unchanged.
    assert_eq!(
        ffprobe(&ctx.tools, o, &["-select_streams", "v:0", "-show_entries", "stream=codec_name", "-of", "csv=p=0"]),
        "h264"
    );

    // Containers and audio formats from the fixtures.
    for fixture in [
        "sample.mov", "sample.mkv", "sample.webm", "sample.avi", "sample.mp3", "sample.wav",
        "sample.flac", "sample.m4a", "sample.ogg", "sample.opus",
    ] {
        let d = fresh_dir(fixture);
        let src = copy_fixture(fixture, &d);
        let out = ctx.tool(&src, ToolKind::StripMetadata);
        assert!(out.errors().is_empty(), "{fixture}: {:?}", out.errors());
        let o = &out.outputs()[0];
        let stem = Path::new(fixture).file_stem().unwrap().to_string_lossy().into_owned();
        let ext = Path::new(fixture).extension().unwrap().to_string_lossy().into_owned();
        assert_eq!(o.file_name().unwrap().to_string_lossy(), format!("{stem}-stripped.{ext}"));
        assert_signature(o, format_of(&src));
        assert!(leftover_temps(&d).is_empty());
    }

    // MP3 with a title: gone after Strip.
    let d = fresh_dir("mp3tag");
    let mp3 = d.join("song.mp3");
    let st = Command::new(&ctx.tools.ffmpeg)
        .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y", "-i"])
        .arg(fixtures().join("sample.mp3"))
        .args(["-c", "copy", "-metadata", "title=Secret Song"])
        .arg(&mp3)
        .status()
        .unwrap();
    assert!(st.success());
    assert_eq!(tag(&mp3), "Secret Song");
    let out = ctx.tool(&mp3, ToolKind::StripMetadata);
    assert_eq!(tag(&out.outputs()[0]), "");
}

#[test]
fn audio_compress_same_format() {
    let Some(mut ctx) = setup() else { return };
    for fixture in ["sample.mp3", "sample.m4a", "sample.ogg", "sample.opus", "sample.flac"] {
        let d = fresh_dir(fixture);
        let src = copy_fixture(fixture, &d);
        let out = ctx.tool(&src, ToolKind::Compress);
        assert!(out.errors().is_empty(), "{fixture}: {:?}", out.errors());
        let o = &out.outputs()[0];
        let stem = Path::new(fixture).file_stem().unwrap().to_string_lossy().into_owned();
        let ext = Path::new(fixture).extension().unwrap().to_string_lossy().into_owned();
        assert_eq!(o.file_name().unwrap().to_string_lossy(), format!("{stem}-compressed.{ext}"));
        assert_signature(o, format_of(&src));
        assert!(leftover_temps(&d).is_empty());
    }
    // WAV cannot be compressed.
    let d = fresh_dir("wav");
    let wav = copy_fixture("sample.wav", &d);
    let out = ctx.tool(&wav, ToolKind::Compress);
    assert_eq!(out.errors().len(), 1);
}

// ----- target size -------------------------------------------------------------

#[test]
fn target_size_on_the_sample_video() {
    let Some(mut ctx) = setup() else { return };
    let dir = fresh_dir("size1");
    let src = copy_fixture("sample.mp4", &dir);
    let before = fs::read(&src).unwrap();
    // 1 MB is a test-only preset: sample.mp4 is 2 s long, so about 3.8 Mbit/s are allowed.
    let out = ctx.tool(&src, ToolKind::VideoTargetSize(1));
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    let o = &out.outputs()[0];
    assert_eq!(o.file_name().unwrap(), "sample-1MB.mp4");
    assert_signature(o, FormatId::Mp4);
    let size = fs::metadata(o).unwrap().len();
    assert!(size > 0 && size as f64 <= 1.05 * 1024.0 * 1024.0, "size {size}");
    assert_eq!(fs::read(&src).unwrap(), before);
    assert_eq!(names_in(&dir), ["sample-1MB.mp4", "sample.mp4"]);
    ctx.assert_no_scratch(); // pass log files are gone
    assert!(leftover_temps(&dir).is_empty());
    // The audio stays.
    assert_eq!(
        ffprobe(&ctx.tools, o, &["-select_streams", "a:0", "-show_entries", "stream=codec_name", "-of", "csv=p=0"]),
        "aac"
    );
}

#[test]
fn target_size_of_a_longer_clip_fits_and_reports_two_passes() {
    let Some(mut ctx) = setup() else { return };
    let dir = fresh_dir("size10");
    let src = dir.join("long.mov");
    make_clip(&ctx.tools, &src, 20, "640x360", None);
    let out = ctx.tool(&src, ToolKind::VideoTargetSize(10));
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    let o = &out.outputs()[0];
    assert_eq!(o.file_name().unwrap(), "long-10MB.mp4");
    let size = fs::metadata(o).unwrap().len();
    assert!(size as f64 <= 10.0 * 1.05 * 1024.0 * 1024.0, "size {size}");
    // Progress: monotone, passes 1 and 2 map to 0-50 and 50-100.
    let p = out.progress();
    assert!(p.windows(2).all(|w| w[1] >= w[0] - 0.001), "not monotone: {p:?}");
    assert!(p.iter().any(|&v| v < 50.0), "no pass 1 progress: {p:?}");
    assert!(p.iter().any(|&v| v >= 50.0), "no pass 2 progress: {p:?}");
    ctx.assert_no_scratch();
}

#[test]
fn target_size_too_long_fails_cleanly() {
    let Some(mut ctx) = setup() else { return };
    let dir = fresh_dir("toolong");
    let src = dir.join("long.mp4");
    // 60 s into 1 MB: 7864 kbit / 60 s = 131 kbit/s, less than the audio rate.
    make_clip(&ctx.tools, &src, 60, "160x90", None);
    let out = ctx.tool(&src, ToolKind::VideoTargetSize(1));
    assert!(out.outputs().is_empty());
    assert_eq!(out.errors().len(), 1);
    assert_eq!(out.errors()[0].1, "Too long to fit in 1 MB");
    assert_eq!(names_in(&dir), ["long.mp4"]);
    ctx.assert_no_scratch();
}

// ----- zip -----------------------------------------------------------------------

fn zip_names(path: &Path) -> Vec<String> {
    let f = fs::File::open(path).unwrap();
    let mut a = zip::ZipArchive::new(f).unwrap();
    (0..a.len())
        .map(|i| a.by_index(i).unwrap().name().unwrap().into_owned())
        .collect()
}

fn zip_entry(path: &Path, name: &str) -> Vec<u8> {
    let f = fs::File::open(path).unwrap();
    let mut a = zip::ZipArchive::new(f).unwrap();
    let mut e = a.by_name(name).unwrap();
    let mut v = Vec::new();
    e.read_to_end(&mut v).unwrap();
    v
}

#[test]
fn zip_a_folder_recursively() {
    let Some(mut ctx) = setup() else { return };
    let dir = fresh_dir("zipfolder");
    let root = dir.join("Holiday Photos");
    fs::create_dir_all(root.join("day 1").join("raw")).unwrap();
    fs::create_dir_all(root.join("empty")).unwrap();
    let a = copy_fixture("sample.png", &root);
    copy_fixture("sample.jpg", &root.join("day 1"));
    fs::write(root.join("day 1").join("raw").join("notes.txt"), b"some notes").unwrap();
    let before = fs::read(&a).unwrap();

    let out = ctx.run(vec![root.clone()], Operation::Action(ActionKind::Zip), None);
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    assert_eq!(out.outputs().len(), 1);
    assert_eq!(out.outputs()[0], dir.join("Holiday Photos.zip"));
    let mut names = zip_names(&out.outputs()[0]);
    names.sort();
    assert_eq!(
        names,
        [
            "Holiday Photos/",
            "Holiday Photos/day 1/",
            "Holiday Photos/day 1/raw/",
            "Holiday Photos/day 1/raw/notes.txt",
            "Holiday Photos/day 1/sample.jpg",
            "Holiday Photos/empty/",
            "Holiday Photos/sample.png",
        ]
    );
    assert_eq!(
        zip_entry(&out.outputs()[0], "Holiday Photos/day 1/raw/notes.txt"),
        b"some notes"
    );
    assert_eq!(zip_entry(&out.outputs()[0], "Holiday Photos/sample.png"), before);
    // The job reports file done once, and progress ends at 100.
    let done = out
        .events
        .iter()
        .filter(|e| matches!(e, BrokerEvent::FileDone { .. }))
        .count();
    assert_eq!(done, 1);
    assert!(leftover_temps(&dir).is_empty());
    // The folder is untouched.
    assert_eq!(fs::read(&a).unwrap(), before);
    assert_eq!(names_in(&root), ["day 1", "empty", "sample.png"]);

    // Second run: counter.
    let again = ctx.run(vec![root], Operation::Action(ActionKind::Zip), None);
    assert_eq!(again.outputs()[0], dir.join("Holiday Photos (1).zip"));
    // The first archive is not inside the second one (it is next to the folder).
    assert!(!zip_names(&again.outputs()[0]).iter().any(|n| n.ends_with(".zip")));
}

#[test]
fn zip_several_files_is_named_after_the_parent_folder() {
    let Some(mut ctx) = setup() else { return };
    let dir = fresh_dir("zipmulti");
    let trip = dir.join("Trip");
    fs::create_dir_all(&trip).unwrap();
    let a = copy_fixture("sample.png", &trip);
    let b = copy_fixture("sample.mp3", &trip);
    let c = trip.join("readme.txt");
    fs::write(&c, b"hi").unwrap();
    let out = ctx.run(
        vec![a.clone(), b.clone(), c.clone()],
        Operation::Action(ActionKind::Zip),
        None,
    );
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    assert_eq!(out.outputs(), &vec![trip.join("Trip.zip")]);
    assert_eq!(zip_names(&out.outputs()[0]), ["sample.png", "sample.mp3", "readme.txt"]);
    assert_eq!(zip_entry(&out.outputs()[0], "readme.txt"), b"hi");
    assert_eq!(fs::read(&a).unwrap(), fs::read(fixtures().join("sample.png")).unwrap());
}

#[test]
fn zip_single_file_and_out_dir() {
    let Some(mut ctx) = setup() else { return };
    let dir = fresh_dir("zipone");
    let src = copy_fixture("sample.png", &dir);
    let out = ctx.run(vec![src.clone()], Operation::Action(ActionKind::Zip), None);
    assert_eq!(out.outputs()[0], dir.join("sample.zip"));
    assert_eq!(zip_names(&out.outputs()[0]), ["sample.png"]);
    // Output folder.
    let od = dir.join("out").join("deeper");
    let out = ctx.run(vec![src.clone()], Operation::Action(ActionKind::Zip), Some(od.clone()));
    assert_eq!(out.outputs()[0], od.join("sample.zip"));
    // Zipping a zip does not pick its own name.
    let z = dir.join("sample.zip");
    let out = ctx.run(vec![z.clone()], Operation::Action(ActionKind::Zip), None);
    assert_eq!(out.outputs()[0], dir.join("sample (1).zip"));
    // A missing item fails and leaves nothing.
    let out = ctx.run(vec![dir.join("nope.txt")], Operation::Action(ActionKind::Zip), None);
    assert_eq!(out.errors().len(), 1);
    assert!(out.errors()[0].1.contains("nope.txt"));
    assert!(leftover_temps(&dir).is_empty());
}

#[test]
fn zip_reports_progress_by_bytes_and_can_be_cancelled() {
    let Some(mut ctx) = setup() else { return };
    let dir = fresh_dir("zipbig");
    let big = dir.join("big.bin");
    // Incompressible-ish data so that compression takes a moment.
    let data: Vec<u8> = (0..24_000_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    fs::write(&big, &data).unwrap();
    let out = ctx.run(vec![big.clone()], Operation::Action(ActionKind::Zip), None);
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    assert_eq!(zip_entry(&out.outputs()[0], "big.bin"), data);
    let p = out.progress();
    assert!(p.windows(2).all(|w| w[1] >= w[0] - 0.001), "not monotone: {p:?}");
    fs::remove_file(&out.outputs()[0]).unwrap();

    // Cancel: no archive and no temp file stay.
    let id = JobId(900);
    ctx.broker.submit(JobRequest {
        id,
        files: vec![big],
        op: Operation::Action(ActionKind::Zip),
        quality: Quality::High,
        out_dir: None,
        page_range: None,
    });
    ctx.broker.cancel(id);
    loop {
        if let BrokerEvent::JobDone { id: i, .. } = ctx.rx.recv_timeout(Duration::from_secs(60)).unwrap() {
            if i == id {
                break;
            }
        }
    }
    assert!(leftover_temps(&dir).is_empty());
    let left = names_in(&dir);
    assert!(left == ["big.bin"] || left == ["big.bin", "big.zip"], "{left:?}");
}

fn media_seconds(tools: &ToolPaths, path: &Path) -> f64 {
    let out = Command::new(&tools.ffprobe)
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "default=nw=1:nk=1"])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap()
}

#[test]
fn crop_keeps_the_rectangle() {
    let Some(mut ctx) = setup() else { return };
    for fixture in ["sample.png", "sample.jpg", "sample.webp"] {
        let dir = fresh_dir("crop");
        let src = copy_fixture(fixture, &dir);
        let (w, h) = dims(&ctx.tools, &src);
        let (cw, ch) = (w / 2, h / 2);
        let out = ctx.tool(&src, ToolKind::Crop { x: 2, y: 3, w: cw, h: ch });
        assert!(out.errors().is_empty(), "{fixture}: {:?}", out.errors());
        let o = &out.outputs()[0];
        assert_eq!(o.file_name().unwrap().to_string_lossy(), suffix_name(fixture, ToolKind::Crop { x: 0, y: 0, w: 1, h: 1 }));
        assert_signature(o, format_of(&src));
        assert_eq!(dims(&ctx.tools, o), (cw, ch), "{fixture}");
        ctx.assert_no_scratch();
        // A rectangle outside the image fails and leaves nothing behind.
        let bad = ctx.tool(&src, ToolKind::Crop { x: w, y: 0, w: 10, h: 10 });
        assert_eq!(bad.errors().len(), 1, "{fixture}");
        assert!(leftover_temps(&dir).is_empty());
        ctx.assert_no_scratch();
    }
}

#[test]
fn trim_cuts_video_and_audio() {
    let Some(mut ctx) = setup() else { return };
    let first = fresh_dir("trim");
    for fixture in ["sample.mp4", "sample.mkv", "sample.mp3", "sample.m4a", "sample.wav", "sample.flac"] {
        let dir = fresh_dir("trim");
        let src = copy_fixture(fixture, &dir);
        let total = media_seconds(&ctx.tools, &src);
        let video = matches!(fixture, "sample.mp4" | "sample.mkv");
        // Audio fixtures are 1 s long.
        let (start, end, kept) = if video { (400, 1400, 1.0) } else { (200, 700, 0.5) };
        assert!(total > end as f64 / 1000.0 + 0.05, "fixture {fixture} is {total} s");
        let out = ctx.tool(&src, ToolKind::Trim { start_ms: start, end_ms: end });
        assert!(out.errors().is_empty(), "{fixture}: {:?}", out.errors());
        let o = &out.outputs()[0];
        let ext = if video { "mp4" } else { fixture.rsplit('.').next().unwrap() };
        assert_eq!(
            o.file_name().unwrap().to_string_lossy(),
            format!("{}-trim.{ext}", fixture.split('.').next().unwrap()),
        );
        let got = media_seconds(&ctx.tools, o);
        assert!((got - kept).abs() < 0.2, "{fixture}: kept {got} s, wanted {kept}");
        // To the end.
        let rest = ctx.tool(&src, ToolKind::Trim { start_ms: start, end_ms: 0 });
        assert!(rest.errors().is_empty(), "{fixture}: {:?}", rest.errors());
        let got = media_seconds(&ctx.tools, &rest.outputs()[0]);
        assert!((got - (total - start as f64 / 1000.0)).abs() < 0.25, "{fixture}: kept {got} s of {total}");
        assert!(leftover_temps(&dir).is_empty());
    }
    // An end before the start fails with a message.
    let src = copy_fixture("sample.mp4", &first);
    let bad = ctx.tool(&src, ToolKind::Trim { start_ms: 1500, end_ms: 1000 });
    assert_eq!(bad.errors().len(), 1);
    assert!(bad.errors()[0].1.contains("end time"), "{:?}", bad.errors());
}

#[test]
fn preview_helpers_on_the_fixtures() {
    let Some(ctx) = setup() else { return };
    let dir = fresh_dir("preview");
    for fixture in ["sample.jpg", "sample.png"] {
        let src = copy_fixture(fixture, &dir);
        let (w, h) = dims(&ctx.tools, &src);
        let p = zest_broker::preview::crop_preview(&ctx.tools.vips, &src, &dir).expect(fixture);
        assert!(p.png.is_file());
        assert_eq!((p.width, p.height), (w, h), "{fixture}: fixtures have no EXIF turn");
        let (pw, ph) = dims(&ctx.tools, &p.png);
        assert!(pw.max(ph) <= zest_broker::preview::PREVIEW_EDGE);
        assert!((i64::from(pw) * i64::from(h) - i64::from(ph) * i64::from(w)).abs() <= i64::from(h.max(w)), "{fixture}: same shape");
    }
    let mp4 = copy_fixture("sample.mp4", &dir);
    let ms = zest_broker::preview::duration_ms(&ctx.tools.ffprobe, &mp4).expect("duration");
    assert!((1000..60_000).contains(&ms), "{ms}");
    let f = zest_broker::preview::video_frame(&ctx.tools.ffmpeg, &mp4, 300, &dir).expect("frame");
    assert!(f.is_file());
    assert_signature(&f, FormatId::Png);
}
