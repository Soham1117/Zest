# Zest — Product & Engineering Specification

**Status:** Implementation-ready design. Phase 0 must prove the OS integration before other work starts.
**Platform:** Windows 11 x64. Windows 10 is not a target.
**Use:** Personal, local use on the owner's machines only. No commercial or public distribution.
**Language:** Rust (stable, MSVC toolchain).
**Product goal:** Hold Ctrl and drag files in Explorer. A radial wheel opens under the pointer. Drop on a sector to convert. All processing is local. No cloud dependency.
**Reference products:** [Filee](https://github.com/KnifeLemon/Filee) (MIT, C#) — Zest copies its gesture **exactly**. [Tangerine for Mac](https://tangerineformac.com/) — source of the Tools/Actions ideas.
**Non-goals:** Write our own codecs. Host a web service. Reuse VERT code.

---

## 1. Interaction model (gesture taken from Filee)

The first design used a Tangerine-style **tap** of Shift during a running drag. In tests it was unreliable, so Zest now uses Filee's gesture. Filee's gesture logic is a pure state machine (`GestureDetector.cs`); Zest ports its behavior to Rust.

### 1.1 Gestures

| Gesture | Default | What it does |
|---|---|---|
| **Drag** | Hold **Ctrl**, press the left button on a file, drag past the system drag threshold | Opens the wheel for the dragged files (drag mode) |
| **Late modifier** | Start a normal drag, **then** press Ctrl while you keep dragging | Same as Drag. Common in Explorer, where people press Ctrl to copy. |
| **Hold** | Hold **Ctrl**, press the left button and do not move for **450 ms** | Opens the wheel for the current Explorer selection (click mode) |
| **Key chord** | **Ctrl+Alt+Space** | Opens the wheel for the current Explorer selection (click mode), or the drop zone if no Explorer window is in front |

Rules (same as Filee):

- **Exact modifier match.** The held modifiers must equal the configured set. Ctrl+Shift does not match a Ctrl gesture. Left and right modifier keys are equal.
- **Threshold:** the gesture fires when `|dx|` or `|dy|` is more than `SM_CXDRAG`/`SM_CYDRAG` from the button-down point.
- **Scope = file manager.** The gesture arms only when the window under the pointer belongs to `explorer.exe` (Explorer windows and the Desktop). Zest's own windows and excluded apps never arm it. If the scope check fails for a late modifier, Zest does not check again until the button goes up.
- Gestures are configurable (modifiers, button, threshold, hold time, scope = file manager / anywhere, excluded apps). A tray "Pause" stops all detection.

### 1.2 Why Ctrl is safe during an OLE drag

During `DoDragDrop`, Ctrl means "copy" to Explorer. That matches what Zest wants. When the wheel is under the pointer, the Zest window is the drop target, and Zest alone decides the drop effect. Zest returns only `DROPEFFECT_COPY` or `DROPEFFECT_NONE`. Zest **never** returns `DROPEFFECT_MOVE`, so the source never deletes the originals. If the user drops outside the wheel, Explorer does a normal Ctrl-drag (copy), which is the normal Windows behavior.

### 1.3 Step-by-step flow (drag mode)

1. The user holds Ctrl, presses the left button on files in Explorer, and starts to drag.
2. Zest's input thread sees: button down with exactly Ctrl, scope is `explorer.exe` → **armed**. The pointer moves past the threshold → **DragStarted**.
3. Zest places its wheel window centered on the pointer, **invisible** (transparent), not activated, and sets it topmost again (`SetWindowPos(HWND_TOPMOST)`). Windows can push a topmost window down after some apps run, and then the drop never reaches it.
4. On the next pointer move, the OLE drag loop finds the Zest window and calls `IDropTarget::DragEnter` with the `IDataObject`.
5. Zest reads `CF_HDROP` and keeps only local files and folders that exist (virtual items are ignored). With items, the wheel becomes visible. Without items (a text drag, a rubber-band selection), nothing is ever shown.
6. Zest receives pointer positions through `DragOver` and computes the hovered sector. No polling.
7. **Release over a sector:** Zest accepts the drop (`DROPEFFECT_COPY`), then shows a progress ring in the same place and queues the job.
   **Release in the center or outside the wheel:** `DROPEFFECT_NONE`, the wheel closes.
   **Esc:** the OLE loop cancels the drag. Zest gets `DragLeave` and hides the wheel.
8. Window lifetime:
   - Button up with no files received → hide at once (the invisible window would otherwise catch clicks).
   - Button up with files but no drop → wait 250 ms (the drop arrives at about the same time as the button release), then hide.
   - Safety limit: if no files arrive within **10 s**, hide. This covers a missed button-up (for example, a UAC prompt took the desktop).
   - A generation counter makes sure that old timers do nothing.
9. By default, output goes next to the source file. Zest calls `SHChangeNotify` so Explorer shows the new file.
10. Progress ring: if the job finishes within 5 s, a ✓ shows in the center and the ring fades. If it takes longer, fails, or the user presses Esc, the job panel near the tray takes over. The panel has **Open folder**, **Retry**, and **Undo**. Undo sends the created output to the Recycle Bin. It never deletes permanently, and it never touches the source.

### 1.4 The wheel, groups, Tools and Actions

The drag wheel shows **conversion targets**. Tools (Compress, Resize, Strip metadata, Rotate, Merge PDF, Video to GIF, Compress to N MB) and Actions (Copy path, Copy files, Copy to…, Move to…, Open terminal here, Zip, Send to shelf) are added later as **group sectors**: hover on "Tools ›" for a short time and a sub-ring opens with the tools. This replaces the earlier "three wheels" design, because the drag gesture has no key to switch wheels.

### 1.5 Other entry points (always present)

These do not need the drag gesture. They also stay as the primary path if the drag gesture fails on a machine.

| Entry point | How it works |
|---|---|
| **Hold gesture or Ctrl+Alt+Space on an Explorer selection** | Zest reads the selected items of the foreground Explorer window through `IShellWindows` → `IServiceProvider` → `IShellBrowser` → `IFolderView`. These are supported COM APIs, not injection. The wheel opens at the pointer, activated (click mode). Click a sector, or use the arrows / 1–8 and Enter. Esc or a click outside closes it. |
| **Key chord with no Explorer window** | Opens a floating drop zone (the wheel with a "Drop files here" center) at the pointer. |
| **Right-click → Zest** | Classic registry verb under `HKCU\Software\Classes\*\shell\Zest` (no admin, no DLL). On Windows 11 it shows under "Show more options". Multi-select: each launched `zest.exe --convert <path>` forwards its path to the running instance over a named pipe. The running instance collects paths for 150 ms, then opens one wheel. Long selections use `--convert-list <tempfile>`. |
| **Send To → Zest** | A shortcut in `shell:sendto`. It passes all files on one command line. |
| **CLI** | `zest.exe convert --to webp <files...>`, `zest.exe tool compress --quality 80 <files...>`. Good for scripts and tests. |

Post-MVP option: top-level Windows 11 context menu through `IExplorerCommand` and a sparse package with a self-signed certificate.

### 1.6 Wheel geometry

- Up to **8 sectors** per ring. If more targets exist, a second outer ring opens on hover over "More".
- Inner dead zone: radius 32 logical px, DPI-scaled. Outer radius: 140 logical px (configurable).
- The window shape is a circle, set with `SetWindowRgn`, so it hit-tests only inside the wheel.
- Near a monitor edge, the wheel shifts so it is fully visible. The pointer stays inside the dead zone.
- The center shows a type icon and a short label ("JPG", "3 files", "2 types").
- Sector 1 is the last target the user picked for this input type. Favorites can be pinned to fixed sectors.
- Accessibility: keyboard digits 1–8 select sectors in keyboard mode. Text labels on every sector. Respect "reduce animations" (`SPI_GETCLIENTAREAANIMATION`).
- Animations are 100 ms or less. Shadows and glyphs are cached.

### 1.7 Mixed selections

An intersection of all targets is often empty (for example, 1 JPG and 1 MP4). Rule:

- One category (all images, or all videos): show the targets that are valid for **all** files.
- Several categories: the wheel shows one sector per category ("Images (3) →", "Videos (1) →"). Hover opens that category's ring. The chosen action applies only to the files of that category. Other files are not changed.
- Files with no valid targets are listed in the job result as "skipped", with the reason.
- The Actions group works on any selection, including folders and files of unknown type. With only folders or unknown files, the actions are shown directly on the ring (no group). With one category, the ring has the conversions, "Tools ›" (tools valid for all files) and "Actions ›"; extra conversions go into "More ›" so that the ring never has more than 8 sectors. With several categories, the ring has one sector per category (conversions only) and "Actions ›".

---

## 2. Format and tool policy

Targets come from a static, vetted **directed conversion graph**. Do not show "convert anything to anything". Zest checks file signatures (magic bytes) before it runs a job, not only extensions. A tool's capability probe at startup removes edges whose tool is missing (for example, no LibreOffice installed → no DOCX → PDF edge).

### 2.1 MVP conversions

| Input | Outputs | Engine |
|---|---|---|
| PNG, JPEG, WebP, BMP, TIFF, GIF (first frame) | PNG, JPEG, WebP, AVIF, TIFF | libvips (`vips.exe`) |
| HEIC, HEIF, AVIF | JPEG, PNG, WebP | libvips (with libheif) |
| MP4, MOV, MKV, WebM, AVI | MP4 (H.264/AAC), WebM (VP9/Opus), MKV (remux), GIF, MP3, WAV | FFmpeg |
| MP3, WAV, FLAC, M4A, OGG, Opus | MP3, WAV, FLAC, M4A (AAC), Opus | FFmpeg |
| PDF | PNG, JPEG (one file per page, 150 DPI default) | PDFium |
| DOCX, DOC, ODT, RTF, TXT | PDF, DOCX, ODT | LibreOffice headless (optional) |
| PPTX, PPT, ODP | PDF, PPTX | LibreOffice headless (optional) |
| XLSX, XLS, ODS, CSV | PDF, XLSX, CSV | LibreOffice headless (optional) |
| MD | DOCX, HTML, EPUB, ODT | Pandoc (optional) |
| DOCX | MD, HTML | Pandoc (optional) |
| HTML | MD, DOCX | Pandoc (optional) |
| EPUB | MD, HTML | Pandoc (optional) |

**Status after Phase 3 (backend):** Done. PDF to PNG/JPEG: `zest-worker.exe` with `pdfium-render` (PDFium chromium/8100, 150 DPI, at most 500 pages and 50 megapixels per page). The pages go into the folder `<name> (pages)\` next to the PDF. The Office and Pandoc rows above are done and exist only when the engine is installed (`soffice.exe` in `Program Files\LibreOffice\program`, `pandoc.exe` on PATH or in `%LOCALAPPDATA%\Pandoc`, or `[tools]` in the config). Timeout 120 s for each. Integration tests for LibreOffice and Pandoc run only when the tool is installed (neither was installed on the development PC, so these two engines are tested at command-line level only).

### 2.2 MVP tools

| Tool | Inputs | Notes |
|---|---|---|
| Compress image | JPEG, PNG, WebP, AVIF | Quality preset: High / Medium / Small |
| Resize image | Images | 50%, 25%, or max edge 1920/1280 px |
| Strip metadata | Images, audio, video | Lossless copy without EXIF/GPS/tags |
| Rotate | Images | 90° steps, lossless for JPEG where possible |
| Compress video to N MB | Videos | Two-pass encode. Presets: 10 MB, 25 MB, 50 MB, custom |
| Video to GIF | Videos | Palette-based, max 15 s, 480 px wide default |
| Merge PDFs | 2+ PDFs | Order = selection order, then name |

**Status after Phase 2** (the tools are the "Tools ›" sub-ring; the Actions are the "Actions ›" sub-ring, spec 1.4):

| Tool / action | Status | Differences from the table above |
|---|---|---|
| Compress | Done | Fixed Medium quality, no preset menu. Images: JPEG, PNG (lossless: zlib 9, all row filters), WebP, AVIF, TIFF. Video: H.264 CRF 28 in an MP4. Audio: lower bit rate in the same format (MP3, M4A 128k, Opus 64k, Ogg q3, FLAC level 12). WAV is not offered. |
| Resize | Done | 50 %, 25 %, max edge 1920, max edge 1280 (never enlarges). |
| Strip metadata | Done | Images are saved again and keep only the color profile. Video and audio are a stream copy (`-map_metadata -1`) in the same container. |
| Rotate | Done | 90° clockwise, 90° counter-clockwise, 180°. The image is saved again (JPEG is not rotated losslessly). |
| Compress video to N MB | Done | Presets 10, 25 and 50 MB. No custom size yet. Two passes (0-50 % and 50-100 % progress). "Too long to fit in N MB" below 150 kbit/s of video. |
| Video to GIF | Not a tool | It is a conversion target of videos (GIF on the wheel), as in Phase 1. |
| Merge PDFs | Done (Phase 3) | In the "Tools ›" ring only when 2 or more PDFs (and nothing else) are selected. Order = selection order (no sort by name). Output `<first name>-merged.pdf`. Runs in `zest-worker.exe`. |
| Copy as PNG / Copy as JPG | Done (Phase 3) | In the "Actions ›" ring when all items are images. Converts into `%LOCALAPPDATA%\Zest\clipboard\<job id>\`, then the app puts `CF_HDROP` (COPY) on the clipboard; one image also gets `CF_DIBV5` and, for PNG, the `PNG` format. Success is silent, a failure shows a balloon. |
| Copy path, Copy files, Copy to, Move to, Terminal, Zip | Done | "Send to shelf" is Phase 3. |

Image tools work on formats that libvips can write again here: PNG, JPEG, WebP, AVIF, TIFF. They are not offered for BMP, GIF, HEIC and HEIF.

### 2.3 Post-MVP

**Done in Phase 3:** Office documents (LibreOffice headless, if installed) and Markdown/EPUB/HTML/DOCX (Pandoc, if installed), see 2.1. Still open: SVG rasterize, subtitles, archives (extract), trim/crop with a preview UI, OCR, PDF split/compress.

### 2.4 Output rules

- Lossy or lossless is shown on the sector (small marker).
- Metadata is kept by default. "Privacy mode" in config strips it on every conversion.
- File timestamps are not copied by default (option in config).
- Never "convert" by renaming an extension.

---

## 3. Latency and performance targets

"Zero latency" is not possible. The goal: UI latency the user cannot feel, independent of codec speed.

| Metric | Target | Rule |
|---|---:|---|
| Tray process ready after login | ≤ 1 s | No tool probes on the UI thread; probes run in the background |
| DragStarted → invisible window placed | P95 ≤ 16 ms | Window and GPU resources exist before use, hidden |
| `DragEnter` → first visible wheel frame | P50 ≤ 16 ms, P95 ≤ 33 ms | Format graph lookup from cache only |
| Pointer motion → highlight | Within **one vsync** of the display (60–165 Hz) | No allocations, no disk I/O in the frame path |
| Drop → wheel hidden | P95 ≤ 1 frame + 16 ms | Queue the job; do not wait |
| Drop → progress shown in job panel | P95 ≤ 100 ms | Tool startup can still be pending |
| Idle CPU | < 0.1% | Event-driven only; no polling loops |
| Idle memory (private bytes) | < 25 MB | Rust, no runtime, no WebView |
| Frame drops during active wheel | < 1% under 80% CPU load | Test with a synthetic load |

Reference machine: Windows 11, SSD, 16 GB RAM, 4–8 core CPU, 100% and 150% DPI. These are objectives, not measured results. Record P50/P95/P99 with `QueryPerformanceCounter` timestamps.

### 3.1 Hot-path rules

- The UI thread is an STA thread (`OleInitialize`) with a normal message pump. It owns the wheel window and the `IDropTarget`.
- Mouse input uses **Raw Input** with `RIDEV_INPUTSINK` on a dedicated hidden window. Positions come from `GetCursorPos` (physical pixels); modifiers come from `GetAsyncKeyState`. Raw Input has no hook timeout and cannot block other apps. (Filee uses low-level hooks through SharpHook; if Raw Input misses events during an Explorer drag, fall back to `WH_MOUSE_LL` on its own thread that only posts a message and returns.)
- The scope check (process under the pointer) runs only on button-down and on a late modifier, and caches the last process id → name.
- Create the wheel window, the D3D11 device, the DirectComposition visual tree, the Direct2D brushes, and the DirectWrite text layouts at startup. Hide the window; do not destroy it.
- Render only when the state changes or an animation runs.
- Never spawn a tool, read file contents, load a thumbnail, use the network, or flush logs while the wheel is visible.
- `GetData(CF_HDROP)` is the only data read in `DragEnter`. It is fast for Explorer. Measure it for other sources.
- Show targets from extensions at once. Check signatures in the broker before the job runs.
- No telemetry. Logs go to a non-blocking file writer (`tracing-appender`).

### 3.2 Jobs and throughput

- Concurrency: heavy video jobs `max(1, min(2, logical_cpus / 4))`. Image and audio jobs `max(2, logical_cpus / 2)`. A small image job may start before a long video queue.
- Every tool process runs inside a **Windows Job Object** with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, a memory limit, and below-normal priority. Cancel = terminate that job object only.
- FFmpeg progress: `-progress pipe:1 -nostats`, parsed on a broker thread. UI updates are limited to 10 per second.
- Atomic output: write to `<name>.zest-tmp` (hidden attribute) in the target folder, check the output (size > 0, tool exit code 0, signature matches the target format), then `MoveFileExW` to the final name. Delete temp files on cancel and on failure. At startup, delete stale `*.zest-tmp` files that are older than 1 day in folders listed in the job history.
- Name collisions: `photo.heic` → `photo.jpg`. If that exists: `photo (1).jpg`, `photo (2).jpg`, and so on. Never overwrite. Never write over the source.
- Guard against decompression bombs: libvips `--vips-max-coord`/pixel limit, FFmpeg input probe limits, Job Object memory cap.

---

## 4. Process architecture

```text
Explorer / other drag source
        |
   OLE drag-drop (IDataObject, CF_HDROP)
        v
+-----------------------------------------+
| zest.exe  (tray, always running)        |
|  UI thread (STA):                       |
|   - Raw Input gesture detection         |
|   - wheel HWND + DirectComposition      |
|   - IDropTarget                         |
|   - tray icon, job panel, hotkey        |
|  Broker thread(s):                      |
|   - job queue, signature checks         |
|   - spawn tools in Job Objects          |
|   - progress parsing, cancel, cleanup   |
|  Pipe thread: --open forwarding, CLI    |
+-------------------+---------------------+
                    | process spawn (argument arrays, no shell)
     +--------------+---------------+---------------------+
     v              v               v                     v
  vips.exe      ffmpeg.exe   zest-worker.exe (PDFium)  [soffice / pandoc, optional]
     |              |               |
     +-------> temp output -> check -> atomic rename -> SHChangeNotify
```

- One resident process. No server, no database, no TCP port.
- `zest-worker.exe` hosts in-process libraries (PDFium) so a crash or a malicious PDF cannot take down the tray process.
- Single instance: a named mutex plus the named pipe `\\.\pipe\zest-<user SID>`. The pipe ACL allows only the current user.
- Job history: a small JSON Lines file, `%LOCALAPPDATA%\Zest\history.jsonl`, appended after the UI acknowledges the drop. At startup, interrupted jobs show as "Failed — retry?". They are never resumed silently.

---

## 5. Rust implementation

### 5.1 Crates (dependencies)

| Purpose | Crate |
|---|---|
| Win32, COM, OLE, D3D11, DXGI, DirectComposition, Direct2D, DirectWrite, Shell | `windows` (official Microsoft bindings; `#[implement]` for `IDropTarget`) |
| Config | `serde`, `toml` |
| Job history, IPC messages | `serde_json` |
| Channels | `crossbeam-channel` |
| Logging | `tracing`, `tracing-subscriber`, `tracing-appender` |
| CLI | `clap` |
| File signatures | `infer` |
| PDF | `pdfium-render` (in `zest-worker` only) |
| Errors | `thiserror` (libraries), `anyhow` (binaries) |
| Tests | built-in `#[test]`, `insta` for argument snapshots |

No async runtime. The design uses plain threads and channels: one UI thread, N broker workers, one pipe thread.

### 5.2 Workspace layout

```text
zest/
  Cargo.toml                 # workspace
  README.md
  docs/
    DRAG_DROP_FEASIBILITY.md # Phase 0 results and compatibility matrix
    FORMAT_MATRIX.md         # generated from the format graph
    PERFORMANCE.md           # measured numbers, machine details
  crates/
    zest-core/       # format graph, jobs, naming, config types. No Windows deps -> fully unit-testable
    zest-input/      # Raw Input, gesture state machine (port of Filee's GestureDetector), scope filter
    zest-overlay/    # wheel window, DirectComposition/Direct2D rendering, hit-testing
    zest-shell/      # IDropTarget, CF_HDROP, Explorer selection, SHChangeNotify, Recycle Bin, registry verbs
    zest-broker/     # queue, Job Objects, adapters (vips, ffmpeg, pdf, office), progress parsing
    zest-app/        # bin "zest": tray, job panel, hotkeys, pipe server, CLI, wiring
    zest-worker/     # bin "zest-worker": PDFium operations
    zest-spike/      # bin: Phase 0 prototype (no conversion)
  tools/
    fetch-deps.ps1   # downloads pinned FFmpeg / libvips / PDFium builds, checks SHA-256
    install.ps1      # copies to %LOCALAPPDATA%\Programs\Zest, registers verbs, Send To, auto-start
    uninstall.ps1
  tests/
    fixtures/        # small sample files of every input format
  third_party/
    bin/             # filled by fetch-deps.ps1 (not committed)
```

### 5.3 Core types

```rust
pub struct InputFile {
    pub path: PathBuf,
    pub size_bytes: Option<u64>,
    pub format: FormatId,          // from extension first, signature later
}

pub enum Operation {
    Convert { to: FormatId },
    Tool(ToolKind),                // Compress, Resize, StripMetadata, Rotate, VideoToTargetSize, VideoToGif, MergePdf
    Action(ActionKind),            // CopyPath, CopyFiles, CopyTo, MoveTo, Terminal, Zip, Shelf
}

pub struct Edge {
    pub from: FormatId,
    pub op: Operation,
    pub engine: EngineId,          // Vips, Ffmpeg, Pdfium, Office, Pandoc, Builtin
    pub lossy: bool,
    pub preset: PresetId,
}

pub struct JobRequest {
    pub id: JobId,
    pub files: Vec<InputFile>,
    pub op: Operation,
    pub options: Options,
    pub output_dir: OutputDir,     // NextToSource | Fixed(PathBuf)
}

pub enum JobState { Accepted, Queued, Validating, Running { percent: Option<f32> },
                    Finalizing, Succeeded { outputs: Vec<PathBuf> },
                    Failed { error: String }, Cancelled }
```

### 5.4 Traits

```rust
pub trait FormatGraph {
    /// Must be O(1)-ish from a precomputed cache. Called on the UI thread.
    fn targets(&self, files: &[InputFile]) -> WheelModel;
}

pub trait Adapter: Send + Sync {
    fn engine(&self) -> EngineId;
    fn available(&self) -> bool;                          // set by the startup probe
    /// Builds the argument array. Pure function -> snapshot-tested.
    fn plan(&self, job: &JobRequest, file: &InputFile, tmp_out: &Path) -> Result<Vec<Command>, PlanError>;
    fn parse_progress(&self, line: &str) -> Option<f32>;
}
```

All dispatch goes through adapters. Unsupported combinations return a clear error.

### 5.5 Rendering details

- Window styles: `WS_POPUP`, extended `WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP`. Show with `SW_SHOWNOACTIVATE`. **No** `WS_EX_TRANSPARENT`, because the OLE loop must find the window with `WindowFromPoint`.
- D3D11 device → DXGI swap chain from `CreateSwapChainForComposition` (premultiplied alpha) → DirectComposition target and visual → Direct2D device context draws the wheel.
- Per-monitor DPI awareness v2 (application manifest). Recompute geometry on `WM_DPICHANGED` and when the wheel opens on a monitor with a different DPI.
- Light and dark theme from the system setting (`AppsUseLightTheme`). Accent color from `DwmGetColorizationColor`.

### 5.6 Configuration

- File: `%APPDATA%\Zest\config.toml`. Atomic writes (temp file + rename). Reloaded when it changes (`ReadDirectoryChangesW`).
- Tray menu: "Open settings file", "Open log folder", "Pause triggers", "Quit".
- MVP has no settings window. Post-MVP: a small settings window.
- Example:

```toml
[[triggers]]                     # same model as Filee's TriggerGesture
kind = "drag"                    # drag | hold | key_chord
modifiers = ["Ctrl"]             # exact match
button = "left"
drag_threshold = 0               # 0 = system default (SM_CXDRAG)
scope = "file_manager"           # file_manager | anywhere

[[triggers]]
kind = "hold"
modifiers = ["Ctrl"]
button = "left"
hold_ms = 450
scope = "file_manager"

[[triggers]]
kind = "key_chord"
modifiers = ["Ctrl", "Alt"]
key = "Space"

[gestures]
paused = false
excluded_processes = []

[wheel]
outer_radius = 140
favorites = { heic = "jpg", mov = "mp4" }

[output]
location = "next_to_source"      # or a folder path
privacy_mode = false
keep_timestamps = false

[jobs]
video_concurrency = "auto"
```

### 5.7 Build and setup

- Rust stable (`x86_64-pc-windows-msvc`), Visual Studio Build Tools (C++ workload).
- `.\tools\fetch-deps.ps1` → `cargo build --release` → `.\tools\install.ps1`.
- Install is per-user. It needs no admin rights. It adds an auto-start entry under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
- No code signing is needed for personal use. SmartScreen does not block binaries you build locally.

---

## 6. Phase 0 — feasibility spike (mandatory, before any conversion code)

Build `zest-spike`: tray icon, Raw Input gesture detection (Filee's gesture), invisible wheel window, `IDropTarget`, a wheel with 6 sectors. Each event logs a timestamp. There is no conversion.

Answer these questions and record the answers in `docs/DRAG_DROP_FEASIBILITY.md`:

1. Does a window shown with `SW_SHOWNOACTIVATE` during an Explorer drag become the drop target, without cancelling the drag?
2. After DragStarted, how many pointer moves pass before `DragEnter` arrives? Does it ever not arrive?
3. Does Raw Input receive button and move events while Explorer's drag loop runs? Do Ctrl+drag and the late-modifier case both fire? Compare with low-level hooks if not.
4. Drop on a sector, drop in the center, drop outside the wheel: the source file must stay in place in every case. Outside the wheel, Explorer does a normal Ctrl-drag copy.
5. False triggers: Ctrl+drag of text, a rubber-band selection in Explorer, Ctrl+drag outside Explorer (must not arm: scope). Nothing may be visible, and the invisible window must not catch the next click.
6. Sources: Explorer, Desktop, Explorer zip folder, Chrome/Edge download bar, Outlook attachment, 7-Zip, Teams. Mark each as "works", "partial" or "not supported".
7. Paths: local, network share (SMB), OneDrive placeholder (cloud-only), long path (> 260 chars), Unicode names.
8. Displays: multi-monitor with negative coordinates, mixed DPI (100% + 150%), auto-hide taskbar, fullscreen app, Remote Desktop.
9. Elevation: Zest runs at medium integrity. Confirm drops from normal Explorer work. Zest must **never** run elevated (UIPI blocks drops into elevated windows).
10. Hold gesture and Ctrl+Alt+Space: does `IShellWindows`/`IFolderView` return the correct selection for the foreground window, including tabbed Explorer on Windows 11?

**Exit criteria:** the default gesture works reliably from Explorer and the Desktop. P95 latencies are recorded. No case changes or deletes a source file.

**Go/no-go:** If the drag gesture fails, ship the hotkey-on-selection, right-click, and drop zone paths as the MVP. Keep the drag gesture as a research item. Do not inject code into Explorer.

**Manual check:** The owner (not CI) runs the drag matrix. The shell behavior cannot be reproduced reliably in automated tests.

---

## 7. Milestones

### Phase 0 — Feasibility
See section 6.

### Phase 1 — Minimal usable product
- Tray app, single instance, auto-start, config file.
- Drag, late-modifier and hold gestures (if Phase 0 passes), Ctrl+Alt+Space on selection, drop zone, right-click verb, Send To, CLI.
- Convert wheel for images (libvips), with signature checks. Progress ring at the drop point.
- Job queue, Job Objects, atomic output, collisions, cancel, job panel with Open folder / Retry / Undo.
- **Exit:** a 100-file batch works. No source file is changed. The wheel never freezes while jobs run.

### Phase 2 — Media, tools, actions
- FFmpeg video and audio conversions with progress.
- Tools group (sub-ring): compress, resize, strip metadata, rotate, compress video to N MB, video to GIF.
- Actions group (sub-ring): copy path, copy files, copy to, move to, terminal here, zip, shelf.
- Mixed-selection category rings.
- **Exit:** several large video jobs run while the wheel still meets the latency targets.

### Phase 3 — PDF, documents, polish
- PDF to images and Merge PDFs (`zest-worker` + PDFium).
- Optional LibreOffice and Pandoc edges, when installed.
- Shelf: a small floating panel. Drop files on it to hold them, then drag them out to another app. Converted outputs can go to the shelf.
- "Copy as…": convert and put the result on the clipboard.
- History panel (re-run, open folder).
- Taskbar progress (`ITaskbarList3`) for long jobs.
- Settings window.

### Phase 4 — Later ideas
- Watch folders (auto-convert new files in a folder). Convert a file only when its size and timestamp have stopped changing and it opens without sharing (Filee's rule). Ignore output folders. **Status: done** (`[[watch]]` in the config and Settings, `zest_core::watch` for the stability rule and patterns, `zest-app` `watchfolders.rs`; tray item "Pause watch folders").
- Route planner: shortest path over converter edges, at most 3 hops (Filee uses Dijkstra), for example HEIC → PNG → AVIF. **Status: backend done** (`zest-core` `FormatGraph::route`, broker runs routes in the scratch folder; see `docs/ENGINE_COMMANDS.md`). **App: done** (routed targets are accepted; the wheel marks them with an arrow, `PNG ↝`).
- Download large engines (LibreOffice, Pandoc) on demand, only when a conversion needs them. **Status: done** (`zest_broker::engines::download_optional`; Settings > Engines has "Download Pandoc (40 MB)" with a confirm dialog and a progress bar, then the engines are probed again). LibreOffice: the app opens `engines::libreoffice_download_url()`. This download is the only network call in Zest, and it runs only after the user agrees.
- Trim/crop with a preview. **Status: done** (Tools ring: "Trim…" for one video or audio file, "Crop…" for one image; `zest tool trim|crop`).
- Top-level Windows 11 context menu (`IExplorerCommand` + sparse package).
- Windows Share sheet action. **Status: done** (Actions ring: "Share", `DataTransferManager`; files only).

---

## 8. Verification

**Automated (`cargo test`):**
- Format graph: every edge has an adapter. No edge targets its own input format with the same settings.
- Wheel model for single, multi, and mixed selections.
- Adapter argument arrays (snapshot tests). Paths with spaces, Unicode, a leading `-`, and `..` are passed safely.
- Output naming and collision handling.
- Signature mismatch (a PNG named `.jpg`) → correct detection or a clear error.
- Integration tests run the real tools on `tests/fixtures`: success, corrupt input, missing tool, cancel mid-job, low disk (temp dir on a small VHD), no temp files left behind.

**Benchmarks:** cold and warm tray start, trigger-to-frame, hover-to-frame, drop-to-hide, queue-to-start. 1 vs 100 files. Small JPEG vs 4K HEIC. 100 MB vs multi-GB video. SSD vs SMB. Idle vs 80% CPU load. 100/150/200% DPI. Record results in `docs/PERFORMANCE.md`.

**Manual:** the Phase 0 drag matrix, run again before each phase exit.

---

## 9. Security and privacy

- All processing is local. Zest makes no network calls.
- Source files are never modified, moved, or deleted by conversions or tools. "Move to…" in the Actions group is the only operation that moves files, and the user selects it explicitly.
- Tools run with argument arrays (`std::process::Command`). Never through `cmd.exe` or PowerShell. Tool paths are absolute and come from `third_party\bin` or from the config file only.
- Insert `--` (or the tool's equivalent) before file arguments where the tool supports it. Otherwise prefix paths with `.\` or use absolute paths, so a file named `-rf.jpg` cannot become an option.
- Tool processes: Job Object limits, below-normal priority, no elevation, temp folder inside `%LOCALAPPDATA%\Zest\tmp`.
- The named pipe accepts connections from the current user only.
- Logs do not contain full file paths unless `log_paths = true` in config.

## 10. Licensing (personal use)

Zest is for personal use and is not distributed. So:

- A full GPL FFmpeg build (with libx264, libx265, libvpx, libopus) is fine.
- libvips (LGPL), PDFium (BSD), LibreOffice (MPL), and Pandoc (GPL) are fine.
- HEVC/HEIC: libheif in the libvips "all" build decodes HEIC. Confirm in Phase 1 that the chosen build includes it.
- VERT (AGPL-3.0) and vertd (GPL-3.0) are used only as a reference for which conversions are useful. No code is reused.
- **If Zest is ever shared with other people**, review all licenses and codec patents again before you share it.

## 11. Non-negotiables

1. Prove the Windows drag-and-drop behavior first (Phase 0).
2. Rendering never waits for a conversion, a tool, or a file read.
3. No hosted backend. No network calls.
4. The wheel shows only targets that the installed tools can actually do.
5. No silent overwrite. No source deletion. No elevation. No shell command strings.
6. Every performance claim has a measurement and a stated machine.
7. The hotkey and right-click fallbacks always work, even if the drag gesture is turned off.

## 12. Open decisions

- Default output location for read-only sources (for example, a network share without write access): ask once, or fall back to `Downloads\Zest`? Proposed: fall back to `Downloads\Zest` and say so in the job result.
- Order of Phase 2 work: video first or tools first? Proposed: video conversions first.

## References (checked 2026-10-18)

- Filee (gesture source, MIT): https://github.com/KnifeLemon/Filee
- Tangerine for Mac: https://tangerineformac.com/
- `windows` crate (Rust for Windows): https://github.com/microsoft/windows-rs
- Microsoft Learn: `IDropTarget`, `RegisterDragDrop`, `DoDragDrop`, `IDataObject`, Raw Input, DirectComposition, `IShellWindows`, Job Objects
- FFmpeg: https://ffmpeg.org/
- libvips: https://www.libvips.org/
- PDFium: https://pdfium.googlesource.com/pdfium/
- VERT (reference only): https://github.com/VERT-sh/VERT

**Bottom line:** The wheel is the product. Conversions are replaceable local tools behind adapters. The first engineering work is the Phase 0 spike, because the drag gesture is both the main feature and the main risk.
