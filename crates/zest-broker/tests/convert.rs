//! Integration tests against the real tools and the fixtures in `tests/fixtures`.
//! Every test works in a fresh temp folder. Without the tools, tests print a message and pass.

use crossbeam_channel::Receiver;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use zest_broker::{Broker, BrokerConfig, BrokerEvent, ToolOverrides, ToolPaths};
use zest_core::{
    detect_signature, is_zest_temp, signature_compatible, EngineId, FormatGraph,
    FormatId, JobId, JobRequest, JobState, Operation, Quality,
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
        "zest-it-{}-{}-{name}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// Copy a fixture into `dir` (optionally under another name) and return the new path.
fn copy_fixture(name: &str, dir: &Path, as_name: Option<&str>) -> PathBuf {
    let dst = dir.join(as_name.unwrap_or(name));
    fs::copy(fixtures().join(name), &dst).unwrap();
    dst
}

struct Ctx {
    broker: Broker,
    rx: Receiver<BrokerEvent>,
    tools: ToolPaths,
    next_id: u64,
    fallback: PathBuf,
}

fn setup() -> Option<Ctx> {
    let tools = ToolPaths::resolve(&ToolOverrides::default());
    let engines = tools.probe();
    if !engines.contains(&EngineId::Vips) || !engines.contains(&EngineId::Ffmpeg) {
        eprintln!("SKIPPED: vips/ffmpeg not found (probe: {engines:?})");
        return None;
    }
    let fallback = fresh_dir("fallback");
    let mut cfg = BrokerConfig::new(tools.clone());
    cfg.fallback_dir = fallback.clone();
    let (broker, rx) = Broker::new(cfg, Arc::new(FormatGraph::new()));
    Some(Ctx {
        broker,
        rx,
        tools,
        next_id: 1,
        fallback,
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
    fn notes(&self) -> Vec<String> {
        self.events
            .iter()
            .filter_map(|e| match e {
                BrokerEvent::FileDone { note, .. } => note.clone(),
                _ => None,
            })
            .collect()
    }
}

impl Ctx {
    fn request(&mut self, files: Vec<PathBuf>, target: FormatId, q: Quality) -> JobRequest {
        let id = JobId(self.next_id);
        self.next_id += 1;
        JobRequest {
            id,
            files,
            op: Operation::Convert(target),
            quality: q,
            out_dir: None,
            page_range: None,
        }
    }

    fn wait(&self, id: JobId, timeout: Duration) -> Outcome {
        let mut events = Vec::new();
        let end = Instant::now() + timeout;
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

    fn run(&mut self, files: Vec<PathBuf>, target: FormatId, q: Quality) -> Outcome {
        let req = self.request(files, target, q);
        let id = self.broker.submit(req);
        self.wait(id, Duration::from_secs(240))
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

fn assert_valid_output(path: &Path, target: FormatId) {
    let bytes = fs::read(path).unwrap();
    assert!(!bytes.is_empty(), "{path:?} is empty");
    let det = detect_signature(&bytes[..bytes.len().min(64)]);
    assert!(
        det.is_some_and(|d| signature_compatible(target, d)),
        "{path:?} has signature {det:?}, wanted {target:?}"
    );
    assert_eq!(
        path.extension().unwrap().to_string_lossy(),
        target.extension()
    );
}

fn convert_ok(ctx: &mut Ctx, fixture: &str, target: FormatId, q: Quality) -> PathBuf {
    let dir = fresh_dir(&format!("{fixture}-{target:?}"));
    let src = copy_fixture(fixture, &dir, None);
    let before = fs::read(&src).unwrap();
    let out = ctx.run(vec![src.clone()], target, q);
    assert!(
        out.errors().is_empty(),
        "{fixture} -> {target:?}: {:?}",
        out.errors()
    );
    assert_eq!(out.outputs().len(), 1, "{fixture} -> {target:?}");
    let o = out.outputs()[0].clone();
    assert_eq!(o.parent().unwrap(), dir);
    assert_valid_output(&o, target);
    assert_eq!(fs::read(&src).unwrap(), before, "source changed");
    assert!(leftover_temps(&dir).is_empty());
    o
}

#[test]
fn every_image_edge_from_png_and_jpg() {
    let Some(mut ctx) = setup() else { return };
    let graph = FormatGraph::new();
    for fixture in ["sample.png", "sample.jpg"] {
        let from = if fixture.ends_with("png") {
            FormatId::Png
        } else {
            FormatId::Jpeg
        };
        for e in graph.targets(from) {
            for q in [Quality::High, Quality::Small] {
                convert_ok(&mut ctx, fixture, e.to, q);
            }
        }
    }
}

#[test]
fn all_image_inputs_to_every_target() {
    let Some(mut ctx) = setup() else { return };
    let graph = FormatGraph::new();
    for (fixture, from) in [
        ("sample.bmp", FormatId::Bmp),
        ("sample.gif", FormatId::Gif),
        ("sample.tiff", FormatId::Tiff),
        ("sample.webp", FormatId::Webp),
        ("sample.avif", FormatId::Avif),
    ] {
        for e in graph.targets(from) {
            convert_ok(&mut ctx, fixture, e.to, Quality::Medium);
        }
    }
}

#[test]
fn image_privacy_mode_converts() {
    let Some(ctx) = setup() else { return };
    let mut cfg = BrokerConfig::new(ctx.tools.clone());
    cfg.privacy_mode = true;
    cfg.fallback_dir = ctx.fallback.clone();
    let (broker, rx) = Broker::new(cfg, Arc::new(FormatGraph::new()));
    let dir = fresh_dir("privacy");
    let src = copy_fixture("sample.jpg", &dir, None);
    let id = broker.submit(JobRequest {
        id: JobId(1),
        files: vec![src],
        op: Operation::Convert(FormatId::Webp),
        quality: Quality::High,
        out_dir: None,
        page_range: None,
    });
    loop {
        if let BrokerEvent::JobDone { id: i, errors, outputs, .. } =
            rx.recv_timeout(Duration::from_secs(60)).unwrap()
        {
            assert_eq!(i, id);
            assert!(errors.is_empty(), "{errors:?}");
            assert_valid_output(&outputs[0], FormatId::Webp);
            break;
        }
    }
}

#[test]
fn video_conversions() {
    let Some(mut ctx) = setup() else { return };
    use FormatId::*;
    for (fixture, target) in [
        ("sample.mp4", Webm),
        ("sample.mov", Mp4),
        ("sample.mp4", Mkv), // remux
        ("sample.avi", Mp4),
        ("sample.mp4", Gif),
        ("sample.mp4", Mp3),
        ("sample.mkv", Mp4),
        ("sample.webm", Mp4),
        ("sample.mov", Wav),
        ("sample.mkv", Webm),
    ] {
        convert_ok(&mut ctx, fixture, target, Quality::Medium);
    }
}

#[test]
fn video_privacy_mode_converts() {
    let Some(ctx) = setup() else { return };
    let mut cfg = BrokerConfig::new(ctx.tools.clone());
    cfg.privacy_mode = true;
    cfg.keep_timestamps = true;
    cfg.fallback_dir = ctx.fallback.clone();
    let (broker, rx) = Broker::new(cfg, Arc::new(FormatGraph::new()));
    let dir = fresh_dir("vprivacy");
    let src = copy_fixture("sample.mov", &dir, None);
    let src_time = fs::metadata(&src).unwrap().modified().unwrap();
    broker.submit(JobRequest {
        id: JobId(1),
        files: vec![src],
        op: Operation::Convert(FormatId::Mkv),
        quality: Quality::High,
        out_dir: None,
        page_range: None,
    });
    loop {
        if let BrokerEvent::JobDone { errors, outputs, .. } =
            rx.recv_timeout(Duration::from_secs(60)).unwrap()
        {
            assert!(errors.is_empty(), "{errors:?}");
            assert_valid_output(&outputs[0], FormatId::Mkv);
            let t = fs::metadata(&outputs[0]).unwrap().modified().unwrap();
            let diff = t
                .duration_since(src_time)
                .unwrap_or_else(|e| e.duration());
            assert!(diff < Duration::from_secs(2), "timestamp not kept: {diff:?}");
            break;
        }
    }
}

#[test]
fn audio_to_every_target() {
    let Some(mut ctx) = setup() else { return };
    let graph = FormatGraph::new();
    for (fixture, from) in [
        ("sample.mp3", FormatId::Mp3),
        ("sample.wav", FormatId::Wav),
        ("sample.flac", FormatId::Flac),
        ("sample.m4a", FormatId::M4a),
        ("sample.ogg", FormatId::Ogg),
        ("sample.opus", FormatId::Opus),
    ] {
        for e in graph.targets(from) {
            convert_ok(&mut ctx, fixture, e.to, Quality::Medium);
        }
    }
}

#[test]
fn corrupt_file_fails_cleanly() {
    let Some(mut ctx) = setup() else { return };
    let dir = fresh_dir("corrupt");
    let src = copy_fixture("corrupt.jpg", &dir, None);
    let out = ctx.run(vec![src.clone()], FormatId::Png, Quality::High);
    assert!(out.outputs().is_empty());
    assert_eq!(out.errors().len(), 1);
    assert_eq!(out.errors()[0].0, src);
    assert!(!out.errors()[0].1.is_empty());
    assert!(out.events.iter().any(|e| matches!(
        e,
        BrokerEvent::Job(j) if matches!(j.state, JobState::Failed { .. })
    )));
    assert!(leftover_temps(&dir).is_empty(), "{:?}", leftover_temps(&dir));
    let names: Vec<_> = fs::read_dir(&dir).unwrap().flatten().collect();
    assert_eq!(names.len(), 1, "only the source must remain");
}

#[test]
fn not_a_media_file_is_rejected() {
    let Some(mut ctx) = setup() else { return };
    let dir = fresh_dir("text");
    let src = dir.join("note.png");
    fs::write(&src, b"this is plain text, not a PNG").unwrap();
    let out = ctx.run(vec![src], FormatId::Jpeg, Quality::High);
    assert_eq!(out.errors().len(), 1);
    assert!(out.errors()[0].1.contains("Not a valid PNG file"), "{:?}", out.errors());
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
}

#[test]
fn fake_png_uses_detected_format() {
    let Some(mut ctx) = setup() else { return };
    let dir = fresh_dir("fake");
    let src = copy_fixture("fake.png", &dir, None);
    let out = ctx.run(vec![src], FormatId::Webp, Quality::High);
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    assert_valid_output(&out.outputs()[0], FormatId::Webp);
    assert_eq!(out.outputs()[0].file_name().unwrap(), "fake.webp");
    // The real format is JPEG, so JPEG -> JPG has no edge.
    let dir2 = fresh_dir("fake2");
    let src2 = copy_fixture("fake.png", &dir2, None);
    let out2 = ctx.run(vec![src2], FormatId::Jpeg, Quality::High);
    assert_eq!(out2.errors().len(), 1);
}

#[test]
fn second_conversion_gets_counter() {
    let Some(mut ctx) = setup() else { return };
    let dir = fresh_dir("collision");
    let src = copy_fixture("sample.png", &dir, None);
    let a = ctx.run(vec![src.clone()], FormatId::Webp, Quality::High);
    let b = ctx.run(vec![src.clone()], FormatId::Webp, Quality::High);
    assert_eq!(a.outputs()[0], dir.join("sample.webp"));
    assert_eq!(b.outputs()[0], dir.join("sample (1).webp"));
    assert!(leftover_temps(&dir).is_empty());
}

#[test]
fn same_file_twice_in_one_job_gets_two_names() {
    let Some(mut ctx) = setup() else { return };
    let dir = fresh_dir("dup");
    let src = copy_fixture("sample.png", &dir, None);
    let out = ctx.run(vec![src.clone(), src], FormatId::Webp, Quality::High);
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    let mut names: Vec<String> = out
        .outputs()
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["sample (1).webp", "sample.webp"]);
}

#[test]
fn out_dir_and_unwritable_fallback() {
    let Some(mut ctx) = setup() else { return };
    let dir = fresh_dir("outdir");
    let src = copy_fixture("sample.png", &dir, None);
    let target_dir = dir.join("made").join("here");
    let mut req = ctx.request(vec![src.clone()], FormatId::Jpeg, Quality::High);
    req.out_dir = Some(target_dir.clone());
    let id = ctx.broker.submit(req);
    let out = ctx.wait(id, Duration::from_secs(60));
    assert_eq!(out.outputs()[0], target_dir.join("sample.jpg"));
    assert!(out.notes().is_empty());

    // An out_dir that is a file cannot be written: fallback folder + note.
    let blocker = dir.join("blocker");
    fs::write(&blocker, b"x").unwrap();
    let mut req = ctx.request(vec![src], FormatId::Jpeg, Quality::High);
    req.out_dir = Some(blocker);
    let id = ctx.broker.submit(req);
    let out = ctx.wait(id, Duration::from_secs(60));
    assert_eq!(out.outputs()[0], ctx.fallback.join("sample.jpg"));
    assert_eq!(out.notes().len(), 1);
}

#[test]
fn batch_of_twenty() {
    let Some(mut ctx) = setup() else { return };
    let dir = fresh_dir("batch");
    let files: Vec<PathBuf> = (0..20)
        .map(|i| copy_fixture("sample.png", &dir, Some(&format!("img{i:02}.png"))))
        .collect();
    let out = ctx.run(files, FormatId::Webp, Quality::Medium);
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    assert_eq!(out.outputs().len(), 20);
    let done = out
        .events
        .iter()
        .filter(|e| matches!(e, BrokerEvent::FileDone { .. }))
        .count();
    assert_eq!(done, 20);
    for o in out.outputs() {
        assert_valid_output(o, FormatId::Webp);
    }
    assert!(leftover_temps(&dir).is_empty());
}

fn make_clip(tools: &ToolPaths, path: &Path, seconds: u32, size: &str) {
    let st = Command::new(&tools.ffmpeg)
        .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i"])
        .arg(format!("testsrc2=size={size}:rate=30:duration={seconds}"))
        .args(["-f", "lavfi", "-i"])
        .arg(format!("sine=frequency=440:duration={seconds}"))
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"])
        .args(["-c:a", "aac", "-shortest"])
        .arg(path)
        .status()
        .unwrap();
    assert!(st.success());
}

fn long_clip(tools: &ToolPaths) -> &'static Path {
    static CLIP: OnceLock<PathBuf> = OnceLock::new();
    CLIP.get_or_init(|| {
        let dir = fresh_dir("clip");
        let p = dir.join("long.mp4");
        make_clip(tools, &p, 20, "640x360");
        p
    })
}

#[test]
fn progress_is_monotone_and_rate_limited() {
    let Some(mut ctx) = setup() else { return };
    let clip = long_clip(&ctx.tools);
    let dir = fresh_dir("progress");
    let src = dir.join("long.mov"); // video pool; ftyp isom is in the Mp4 family, so rename is fine
    fs::copy(clip, &src).unwrap();
    let req = ctx.request(vec![src], FormatId::Mp4, Quality::Medium);
    // Mov -> Mp4 is an edge; the file is really MP4, and the declared Mov is compatible.
    ctx.broker.submit(req);
    let mut stamps: Vec<Instant> = Vec::new();
    let mut last = -1.0f32;
    let mut events = Vec::new();
    loop {
        let ev = ctx.rx.recv_timeout(Duration::from_secs(240)).unwrap();
        if let BrokerEvent::Progress { percent: Some(p), .. } = &ev {
            assert!(*p >= last - 0.001 && *p <= 100.0, "progress {p} after {last}");
            last = *p;
            stamps.push(Instant::now());
        }
        let done = matches!(ev, BrokerEvent::JobDone { .. });
        events.push(ev);
        if done {
            break;
        }
    }
    assert!(stamps.len() >= 2, "only {} progress events", stamps.len());
    // No more than 10 per second in any 1-second window (small slack for scheduling).
    for (i, s) in stamps.iter().enumerate() {
        let n = stamps[i..]
            .iter()
            .take_while(|t| t.duration_since(*s) < Duration::from_secs(1))
            .count();
        assert!(n <= 11, "{n} progress events within one second");
    }
    assert!(events.iter().any(|e| matches!(
        e,
        BrokerEvent::JobDone { errors, outputs, .. } if errors.is_empty() && outputs.len() == 1
    )));
}

fn ffmpeg_processes_mentioning(needle: &str) -> usize {
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_Process -Filter \"Name='ffmpeg.exe'\" | ForEach-Object { $_.CommandLine }",
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains(needle))
        .count()
}

#[test]
fn cancel_mid_video_job_cleans_up() {
    let Some(mut ctx) = setup() else { return };
    let dir = fresh_dir("cancel");
    let src = dir.join("big.mp4");
    make_clip(&ctx.tools, &src, 120, "1280x720");
    let dir_name = dir.file_name().unwrap().to_string_lossy().into_owned();

    let req = ctx.request(vec![src.clone()], FormatId::Webm, Quality::High);
    let id = ctx.broker.submit(req);
    // Wait until the encoder runs, then give it time to write output.
    let t0 = Instant::now();
    let mut events = Vec::new();
    loop {
        let ev = ctx.rx.recv_timeout(Duration::from_secs(60)).unwrap();
        let running = matches!(&ev, BrokerEvent::Job(j) if matches!(j.state, JobState::Running { .. }));
        events.push(ev);
        if running {
            break;
        }
    }
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(
        ffmpeg_processes_mentioning(&dir_name),
        1,
        "ffmpeg should be running"
    );
    ctx.broker.cancel(id);
    let out = ctx.wait(id, Duration::from_secs(30));
    assert!(t0.elapsed() < Duration::from_secs(60));
    let (outputs, errors, cancelled) = out.done();
    assert!(cancelled);
    assert!(outputs.is_empty());
    assert!(errors.is_empty());
    assert!(out.events.iter().any(|e| matches!(
        e,
        BrokerEvent::Job(j) if j.state == JobState::Cancelled
    )));
    assert!(leftover_temps(&dir).is_empty(), "{:?}", leftover_temps(&dir));
    let mut names: Vec<_> = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["big.mp4"]);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(ffmpeg_processes_mentioning(&dir_name), 0, "orphan ffmpeg");
}

#[test]
fn queued_files_are_cancelled_too() {
    let Some(ctx) = setup() else { return };
    let mut cfg = BrokerConfig::new(ctx.tools.clone());
    cfg.image_workers = 1;
    cfg.fallback_dir = ctx.fallback.clone();
    let (broker, rx) = Broker::new(cfg, Arc::new(FormatGraph::new()));
    let dir = fresh_dir("cancelq");
    let files: Vec<PathBuf> = (0..30)
        .map(|i| copy_fixture("sample.bmp", &dir, Some(&format!("b{i:02}.bmp"))))
        .collect();
    let id = broker.submit(JobRequest {
        id: JobId(9),
        files,
        op: Operation::Convert(FormatId::Avif),
        quality: Quality::High,
        out_dir: None,
        page_range: None,
    });
    broker.cancel(id);
    loop {
        if let BrokerEvent::JobDone { cancelled, outputs, .. } =
            rx.recv_timeout(Duration::from_secs(120)).unwrap()
        {
            assert!(cancelled);
            assert!(outputs.len() < 30);
            break;
        }
    }
    drop(broker);
    assert!(leftover_temps(&dir).is_empty());
}

#[test]
fn image_task_not_blocked_by_video_queue() {
    let Some(ctx) = setup() else { return };
    let mut cfg = BrokerConfig::new(ctx.tools.clone());
    cfg.video_workers = 1;
    cfg.image_workers = 1;
    cfg.fallback_dir = ctx.fallback.clone();
    let (broker, rx) = Broker::new(cfg, Arc::new(FormatGraph::new()));
    let dir = fresh_dir("pools");
    let big = dir.join("big.mp4");
    make_clip(&ctx.tools, &big, 60, "1280x720");
    let img = copy_fixture("sample.png", &dir, None);
    broker.submit(JobRequest {
        id: JobId(1),
        files: vec![big],
        op: Operation::Convert(FormatId::Webm),
        quality: Quality::High,
        out_dir: None,
        page_range: None,
    });
    broker.submit(JobRequest {
        id: JobId(2),
        files: vec![img],
        op: Operation::Convert(FormatId::Jpeg),
        quality: Quality::High,
        out_dir: None,
        page_range: None,
    });
    // The image job must finish while the video job still runs.
    let end = Instant::now() + Duration::from_secs(60);
    loop {
        let ev = rx.recv_timeout(end.saturating_duration_since(Instant::now())).unwrap();
        if let BrokerEvent::JobDone { id, errors, .. } = ev {
            assert_eq!(id, JobId(2), "video finished before the image");
            assert!(errors.is_empty());
            break;
        }
    }
    broker.cancel(JobId(1));
}

#[test]
fn probe_finds_bundled_tools() {
    let tools = ToolPaths::resolve(&ToolOverrides::default());
    if !tools.vips.is_file() || !tools.ffmpeg.is_file() || !tools.ffprobe.is_file() {
        eprintln!("SKIPPED: tools not installed");
        return;
    }
    let engines = tools.probe();
    assert!(engines.contains(&EngineId::Vips), "{engines:?}");
    assert!(engines.contains(&EngineId::Ffmpeg), "{engines:?}");
}

fn ffprobe_field(ffprobe: &Path, file: &Path, entries: &str) -> String {
    let out = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            entries,
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(file)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn non_square_sar_is_baked_into_pixels() {
    let Some(mut ctx) = setup() else { return };
    let dir = fresh_dir("sar");
    let src = dir.join("anamorphic.mp4");
    // 320x240 storage with SAR 4:3 → display ~426x240. Remux alone keeps 320x240+SAR.
    let st = Command::new(&ctx.tools.ffmpeg)
        .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "testsrc2=size=320x240:rate=5:duration=1"])
        .args(["-vf", "setsar=4/3"])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-an"])
        .arg(&src)
        .status()
        .unwrap();
    assert!(st.success());
    let sar_in = ffprobe_field(&ctx.tools.ffprobe, &src, "stream=sample_aspect_ratio");
    assert_ne!(sar_in, "1:1", "fixture must be non-square SAR, got {sar_in}");

    // WebM always re-encodes with the SAR filter.
    let out = ctx.run(vec![src.clone()], FormatId::Webm, Quality::Medium);
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    let outputs = out.outputs();
    assert_eq!(outputs.len(), 1);
    let sar_out = ffprobe_field(&ctx.tools.ffprobe, &outputs[0], "stream=sample_aspect_ratio");
    assert!(
        sar_out == "1:1" || sar_out.eq_ignore_ascii_case("N/A"),
        "expected square SAR after convert, got {sar_out}"
    );
    let w: u32 = ffprobe_field(&ctx.tools.ffprobe, &outputs[0], "stream=width")
        .parse()
        .unwrap();
    assert!(
        w >= 420 && w <= 432,
        "display width should be ~426 after baking SAR, got {w}"
    );

    // MKV must bake (not remux) when SAR is non-square.
    let out = ctx.run(vec![src], FormatId::Mkv, Quality::Medium);
    assert!(out.errors().is_empty(), "{:?}", out.errors());
    let sar_mkv = ffprobe_field(&ctx.tools.ffprobe, &out.outputs()[0], "stream=sample_aspect_ratio");
    assert!(
        sar_mkv == "1:1" || sar_mkv.eq_ignore_ascii_case("N/A"),
        "mkv should bake SAR, got {sar_mkv}"
    );
    let w_mkv: u32 = ffprobe_field(&ctx.tools.ffprobe, &out.outputs()[0], "stream=width")
        .parse()
        .unwrap();
    assert!(
        w_mkv >= 420 && w_mkv <= 432,
        "mkv display width should be ~426 after baking SAR, got {w_mkv}"
    );
}
