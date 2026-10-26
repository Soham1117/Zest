# Zest

Zest is a personal file converter for Windows 11. Hold Ctrl, drag files in Explorer, and a radial wheel opens
under the pointer. Drop the files on a sector to convert them. Everything runs on your PC. Zest makes no
network calls on its own. The one exception is the optional Pandoc download, and only after you agree (see
[Download Pandoc](#download-pandoc)).

Besides conversions, the wheel has **Tools** (compress, resize, rotate, strip metadata, video to N MB) and
**Actions** (copy path, copy files, copy to, move to, terminal, zip). They open as sub-rings.

Source: `windows_radial_converter_spec.md`. For personal use only.

## What it converts

| Input | Output | Engine |
|---|---|---|
| PNG, JPEG, WebP, BMP, TIFF, GIF, HEIC, HEIF, AVIF | PNG, JPEG, WebP, AVIF, TIFF | libvips |
| MP4, MOV, MKV, WebM, AVI | MP4, WebM, MKV, GIF, MP3, WAV | FFmpeg |
| MP3, WAV, FLAC, M4A, OGG, Opus | MP3, WAV, FLAC, M4A, Opus | FFmpeg |
| PDF | PNG, JPG (one image per page, 150 DPI) | PDFium (`zest-worker.exe`) |
| DOCX, DOC, ODT, RTF, TXT | PDF, DOCX, ODT | LibreOffice (optional) |
| PPTX, PPT, ODP | PDF, PPTX | LibreOffice (optional) |
| XLSX, XLS, ODS, CSV | PDF, XLSX, CSV | LibreOffice (optional) |
| MD | DOCX, HTML, EPUB, ODT | Pandoc (optional) |
| DOCX | MD, HTML | Pandoc (optional) |
| HTML | MD, DOCX | Pandoc (optional) |
| EPUB | MD, HTML | Pandoc (optional) |

The wheel only shows conversions that the installed tools can do. Run `zest formats` to see the list.

**Routed conversions.** If there is no direct conversion, Zest looks for a chain of at most 3 steps, for example
DOCX to PDF to PNG, or HEIC to PNG to AVIF. The target sector has an arrow after its name (`PNG ↝`). Zest runs the
steps one after the other in a private scratch folder, so only the final file appears next to the source. A
chain that would lose quality for a lossless target is not offered. The `zest convert` command and the History
window (Re-run) accept routed targets in the same way.
Zest never overwrites a file: if `photo.jpg` exists, the output is `photo (1).jpg`. It never changes or
deletes the source file.

### PDF, Office and text documents

- **PDF to PNG or JPG**: the pages go into a new folder next to the PDF, named `<name> (pages)`, as
  `<name>-p001.png`, `<name>-p002.png`, and so on. The pages are 150 DPI. A PDF may have up to 500 pages, and a page
  may have up to 50 megapixels; otherwise the job fails with a clear message. A password-protected PDF fails, too.
  The folder is built as `<name> (pages).zest-tmp` first and renamed when all pages are done.
- **Merge PDFs** (Tools ›): select two or more PDFs and pick it. The output is `<first name>-merged.pdf`, with the
  PDFs in the order you selected them.
- **Office and text documents** are optional. They need **LibreOffice** (`soffice.exe` in
  `C:\Program Files\LibreOffice\program\` or the x86 folder) or **Pandoc** (`pandoc.exe` on `PATH`, in
  `%LOCALAPPDATA%\Pandoc\` or `C:\Program Files\Pandoc\`). If a tool is not installed, its conversions do not
  show on the wheel, and Zest says nothing. You can set the path in the `[tools]` section of the config file.
  LibreOffice runs headless with its own temporary profile for each job (it never touches your LibreOffice
  settings) and gets 120 seconds. Pandoc gets 120 seconds, too. There is no PDF output from Pandoc.
- A mixed selection (for example images and PDFs) shows one sector per kind, such as **Docs (2) ›**.
- ZIP-based documents (DOCX, PPTX, XLSX, ODT, ODP, ODS, EPUB) are accepted when the file starts with a ZIP
  signature, old Office files (DOC, PPT, XLS) when they start with the OLE2 signature, and CSV, MD, HTML and TXT
  when the start of the file is valid UTF-8.

## Tools and actions

Hover on **Tools ›** or **Actions ›** for 300 ms (or click it, or press Enter) and a sub-ring opens around it.
The main ring has at most 8 sectors. If there are too many conversions, the last ones move into **More ›**.

**Tools** keep the file format and write a new file with a suffix. They work on the files of one kind (images,
videos or audio). A tool is shown only if it works on all selected files.

| Tool | Label | Output | Files |
|---|---|---|---|
| Compress | Compress | `photo-compressed.jpg` | Images (JPEG, PNG, WebP, AVIF, TIFF), video, audio |
| Resize to half / quarter | 50%, 25% | `photo-50pct.jpg`, `photo-25pct.jpg` | Images |
| Limit the long edge | Max 1920, Max 1280 | `photo-1920.jpg`, `photo-1280.jpg` | Images (never enlarged) |
| Remove metadata | Strip meta | `photo-stripped.jpg` | Images, video, audio |
| Rotate | ⟳ 90°, ⟲ 90°, 180° | `photo-rotated.jpg` | Images |
| Video to a size | 10 MB, 25 MB, 50 MB | `clip-10MB.mp4` | Video |
| Merge PDFs | Merge PDFs | `first-merged.pdf` | 2 or more PDFs (selection order) |
| Trim | Trim… | `clip-trim.mp4`, `song-trim.mp3` | One video or audio file |
| Crop | Crop… | `photo-crop.jpg` | One image (JPEG, PNG, WebP, AVIF, TIFF) |

- Compress: images use medium quality (PNG stays lossless and only gets smaller); video is re-encoded as H.264
  (CRF 28) in an MP4; audio gets a lower bit rate in the same format (WAV is not offered).
- Strip meta: images are saved again without EXIF, GPS and XMP (the color profile stays). Video and audio are copied
  without changes to the streams, only the tags are removed.
- Video to a size: a two-pass H.264 encode with AAC audio. The progress ring shows pass 1 as 0-50 % and pass 2 as
  50-100 %. Large videos are scaled down (1280 or 854 px wide) when the bit rate is low. If the video is too long
  (less than 150 kbit/s left for the picture), the job fails with "Too long to fit in N MB". A result up to 5 %
  over the limit is accepted.
- **Trim…** (one video or audio file) opens a window. Type the start and the end (`mm:ss.mmm`), or move the two
  sliders. The length of the file (from ffprobe) is shown. For a video, the window also shows the frame at the start
  and at the end. An empty end means "to the end". The cut is exact, so the part is encoded again: a video becomes
  H.264/AAC in an MP4 at the High preset (CRF 20, like a High conversion), audio keeps its format with a high
  bit rate (WAV and FLAC stay lossless). Progress counts the kept part only.
- **Crop…** (one image) opens a window with the picture, scaled to fit. Drag on the picture to draw the area, drag
  the handles or the inside to change it, and pick **Free, 1:1, 4:3** or **16:9** for the shape. The size in pixels
  is shown below. The numbers are pixels of the whole image after the EXIF turn (a phone photo that is stored
  sideways shows upright, and the crop follows what you see). OK is off until there is an area. libvips does the cut.
- BMP, GIF, HEIC and HEIF files have no image tools (libvips cannot write them again here). Convert them first.
- Zest never changes the source file. A taken name gets ` (1)`, ` (2)`, and so on.

**Actions** work on any selection, including folders and files of unknown type. If you drop only folders or
unknown files, the actions (without Copy as...) are on the main ring.

| Action | What happens |
|---|---|
| Copy path | The paths go to the clipboard as text, one per line. A path with spaces is put in double quotes. |
| Copy files | The files go to the clipboard like Explorer's Copy: Paste in Explorer copies them. |
| Copy to… / Move to… | A folder picker opens (it starts in the last folder you chose). Explorer then copies or moves the items with its own progress and conflict dialogs. Ctrl+Z in Explorer undoes it. "Move to…" is the only place where Zest moves your files. |
| Terminal | Windows Terminal (or PowerShell, if Terminal is not installed) opens in the folder of the first item, or in the folder itself. |
| Zip | One `.zip` next to the first item: `name.zip` for one item, `<parent folder>.zip` (or `Archive.zip`) for several. Folders are added with all their content. The progress ring shows the work. |
| Shelf | The items go to the Shelf window (see [Shelf](#shelf)). Nothing is copied. |
| Share | Opens the Windows share sheet (nearby sharing, mail, apps) with the selected files. Folders are skipped. If Windows cannot show the sheet, a balloon says "Share is not available". |
| *(inbound)* | **Tray → Convert clipboard** opens the wheel for `CF_HDROP` file lists or a clipboard image. Receiving via the system Share target is not packaged yet — copy/share into the clipboard, then Convert clipboard. |

**Copy as PNG / Copy as JPG** are in the **Actions ›** ring when all selected items are images. Zest converts the
images into `%LOCALAPPDATA%\Zest\clipboard\<job id>\` (not next to the source) and puts them on the clipboard:
the files (so Paste in Explorer copies them) and, for one image, the pixels, too (`CF_DIBV5`, plus the `PNG`
format for a PNG), so that Paste works in chat programs and image editors. An image that already has the target
format is copied as it is. Nothing shows when it works; an error shows a balloon. Folders in the clipboard
folder that are older than one day are deleted when Zest starts.

The wheel closes at once when you pick an action, except Zip and Copy as, which show the progress ring like a conversion. Errors
show a balloon. Actions that finish without a problem show nothing.

If you pick from a wheel for mixed files (images, videos, audio, documents), the wheel has one sector per kind
(**Images**, **Videos**, **Audio**, **Docs**) and an **Actions ›** sector. Tools are not shown there: choose one kind first.

## Install

You need Rust (MSVC toolchain) and the VS 2022 Build Tools (C++ workload).

```powershell
.\tools\fetch-deps.ps1     # downloads libvips, FFmpeg and PDFium into third_party\bin
.\tools\install.ps1        # builds, copies to %LOCALAPPDATA%\Programs\Zest, registers, starts Zest
```

`install.ps1` needs no admin rights. It does this:

1. `cargo build --release -p zest-app -p zest-worker`.
2. Stops a running `zest.exe`.
3. Copies `zest.exe`, `zest-worker.exe` and `third_party\bin\` to `%LOCALAPPDATA%\Programs\Zest\`.
4. Runs `zest.exe --install` (right-click verb, Send To shortcut, start at sign-in).
5. If the Windows SDK is present, offers to run `register-win11-menu.ps1` (admin once) for the top-level Win11 "Convert with Zest" entry; classic verb remains the fallback.
5. Starts Zest. A tray icon (orange circle with a Z) appears.

Do not run Zest as administrator. Windows blocks drops into elevated windows.

### Top-level Windows 11 menu (optional)

By default the right-click verb is under "Show more options". To get "Convert with Zest" in the main menu:

1. Install the Windows SDK once: `winget install Microsoft.WindowsSDK.10.0.26100`.
2. Run `.\tools\install.ps1` (it copies `zest_explorer_command.dll`).
3. In an administrator PowerShell, run `.\tools\register-win11-menu.ps1`. It makes a self-signed certificate
   "CN=Zest Local", trusts it, and registers a sparse package. Later runs do not need admin rights.

Remove it with `.\tools\unregister-win11-menu.ps1`.

## Use

| Gesture | What happens |
|---|---|
| **Ctrl + drag** files in Explorer, File Pilot, Desktop, or another allowed file manager | The wheel opens when you start to drag. Drop on a sector. |
| **Late Ctrl**: start a normal drag, then press Ctrl | Same as Ctrl + drag. |
| **Ctrl + hold**: press the left button on a file with Ctrl held, do not move for 450 ms | The wheel opens for the selected files. Click a sector, or use the arrow keys / 1-8 and Enter. Esc closes. |
| **Ctrl+Alt+Space** | The wheel opens for the selection of the Explorer window in front. With no selection, a "Drop files here" wheel opens. If another app owns this key, Zest uses **Win+Alt+Z**. |
| **Right-click a file > Show more options > Convert with Zest** | The wheel opens at the pointer. Several selected files open one wheel. |
| **Send To > Zest** | Same, from the Send To menu. |

The modifiers must match exactly: Ctrl+Shift+drag does nothing. When you drop on the wheel, a thin ring shows the
progress. After 5 seconds, or if you start something new, the ring goes away and a tray balloon tells you the
result ("Converted 3 files"). Click the balloon (or the check mark on the ring) to open the folder with the new
file selected. Errors always show a balloon.

If you pick a sector for mixed files (for example images and videos), the wheel has one sector per kind
("Images (3)", "Videos (1)"). Hover to open its targets. Only the files of that kind are converted.

### Tray menu

Right-click the tray icon:

- **Pause gestures** (check): stops all detection.
- **Pause watch folders** (check): the watch folders ignore new files until you switch it off again (see [Watch folders](#watch-folders)).
- **Cancel all jobs**: active only while jobs run.
- **Show shelf**, **History…**, **Settings…**: the three windows (see below).
- **Open settings file**, **Open log folder**.
- **Start at sign-in** (check).
- **Add to right-click menu** (check): installs or removes the right-click verb and the Send To shortcut.
- **Quit**.

### Command line

```powershell
zest convert --to webp --quality medium --out D:\out a.png b.jpg
zest tool resize50 --out D:\out a.png b.jpg   # run a tool
zest zip "My Folder" notes.txt                  # one zip file next to the first item
zest formats                  # what the installed engines can convert
zest --install                # right-click verb, Send To, start at sign-in
zest --uninstall
zest --version
zest --convert a.png b.png    # open the wheel for these files (the right-click verb uses this)
zest --convert-list list.txt  # same, one path per line (for long selections)
```

`--quality` is `high` (default), `medium` or `small`. Without `--out`, the output goes where the config says
(default: next to the source file). Exit codes: `0` all files converted, `1` some files failed, `2` wrong usage.

Tools for `zest tool`: `compress`, `resize50`, `resize25`, `max1920`, `max1280`, `strip`, `rotate-cw`,
`rotate-ccw`, `rotate180`, `size10`, `size25`, `size50`, `merge-pdf` (two or more PDFs, in the order given),
`trim` and `crop`:

```powershell
zest tool trim --start 00:01 --end 00:05 clip.mp4     # --end is optional (default: to the end)
zest tool crop --rect 100,50,800,600 photo.jpg        # x,y,width,height in pixels (after the EXIF turn)
```

Times are `SS`, `MM:SS` or `HH:MM:SS`, with `.mmm` for milliseconds. A bad value is a usage error (exit code 2).
`zest convert --to png report.pdf` writes the page images. `zest zip` takes files and folders and has `--out`, too.
The other actions (copy, move, terminal) are only in the wheel.

`zest.exe` is a Windows-subsystem program. In PowerShell it does not wait for the command to end. To get the exit code
use `Start-Process zest.exe -ArgumentList '...' -Wait -PassThru -NoNewWindow`, or add `| Out-Host` to the command
(`zest convert ... | Out-Host; $LASTEXITCODE`). In `cmd.exe` use `start /wait`.

## Shelf

The Shelf is a small window that holds files for you. It is always on top. Open it from the tray menu (**Show shelf**),
with the **Shelf** action on the wheel (in the **Actions ›** ring), or with `send_to_shelf = true` in the config.

- **Put files on it:** drag files or folders onto the Shelf. It keeps the paths only. It does not copy or move
  the files, and a drop on the Shelf is always a copy (never a move).
- **Take files out:** select one or more items (Ctrl or Shift click, Ctrl+A) and drag them to another program or
  folder. The target decides if it copies, moves or links. The items stay on the Shelf, unless you turn on
  **Remove after drag out** in the right-click menu.
- **Right-click an item:** Open, Show in folder, Remove from Shelf. Delete removes the selected items from the
  Shelf (the files stay where they are). Double-click opens the item.
- **Missing files** (deleted or moved) are grey. **Remove missing items** and **Clear** take them away.
- **Clear** removes all items from the Shelf. **×** hides the Shelf. Esc hides it, too.
- With `send_to_shelf = true`, the outputs of every finished job go to the Shelf and the Shelf opens (it does not
  take the keyboard focus).

The items, the window position and size, and the **Remove after drag out** option are kept in `state.json`.

## History

**History…** in the tray menu opens the job window. A click on an error balloon opens it, too. It shows the last 500
jobs from `history.jsonl` and the jobs that run now, newest first.

| Column | Text |
|---|---|
| Time | The start time (local). Today: `14:05`. |
| Operation | `HEIC → JPG`, `Compress`, `Zip`, … |
| Items | The first file name, with the number of files. |
| Result | `✓`, `✕ 2 failed`, `Cancelled`, `Running 40 %`, `Undone`, `Interrupted` |

Type in the search box to filter by file name (inputs and outputs). Double-click a row, or press Enter, to show the
output in Explorer. The buttons and the right-click menu:

| Button | What it does |
|---|---|
| Show in folder / Open | Explorer with the output selected / opens the first output. |
| Re-run | Starts the same operation again on the same inputs. Inputs that do not exist any more are skipped (a balloon says how many). |
| Retry failed | The same, but only for the inputs that failed. |
| Undo | After a confirmation ("Move N files to the Recycle Bin?"), the files that the job made go to the Recycle Bin. Zest never deletes for good, and it never touches the source files. The row shows `Undone`, and `history.jsonl` gets an `undone` line. |
| Cancel job | Stops a running job. |
| Copy error | Puts the error messages of the job on the clipboard. |

Press **Ctrl+Alt+Shift+Z** to undo the last Zest job. Normal **Ctrl+Z** and **Ctrl+Shift+Z**
stay available to other apps. Older `ui.undo_chord` settings using those shortcuts use the new Zest shortcut.

While the window is open and jobs run, its taskbar button shows the progress of all running jobs. It turns red when
a job failed, until you look at the window.

## Watch folders

A watch folder converts the new files of a folder by itself. Add one in **Settings > Watch folders** (**Add…**: the folder,
the operation from a list, file name patterns such as `*.heic`, and **Include subfolders**), or write it in `config.toml`:

```toml
[[watch]]
folder = 'C:\Users\you\Pictures\Inbox'
op = "convert:jpg"            # "convert:<extension>" or a tool name: compress, resize50, strip, size25, ...
include = ["*.heic"]          # file name patterns (* and ?); leave out = every file the operation can handle
recursive = false
enabled = true
```

- Only files that appear or change after the watcher started are used. Files that are already in the folder stay as they are.
- A file is processed when it is complete: its size and time have not changed for 2 seconds **and** it opens with no
  sharing (no other program holds it). If not, Zest tries again after 1, 2, 4 … 30 seconds, for up to 5 minutes, then gives up.
- Never processed: `*.zest-tmp`, partial downloads (`.crdownload`, `.part`, `.tmp`), hidden and system files, anything in
  an output folder such as `<name> (pages)`, and every file that Zest made itself (it remembers its outputs). A file whose
  type the operation cannot handle (for example a PNG in a "convert to JPG" folder after the first run) is skipped
  without a message.
- The output goes where the config says (default: next to the source). The jobs run in the broker like any other job
  and show in **History**. There is no progress ring. A balloon shows only when a job fails.
- **Pause watch folders** in the tray menu stops all watchers for the moment. Files that appear while paused are skipped.
- Changes to `[[watch]]` in the file or in Settings apply at once. A folder that does not exist is skipped (the log says so).

## Download Pandoc

Pandoc (Markdown, HTML, EPUB, DOCX) is optional. If it is not installed, **Settings > Engines** shows
**Download Pandoc (40 MB)**. A dialog asks first. After you press **Download**, Zest fetches the pinned version from
github.com over HTTPS, checks the SHA-256 hash, and unpacks it to `%LOCALAPPDATA%\Zest\engines\pandoc`. A progress bar
shows the steps. Then Zest looks for its engines again and Pandoc gets its check mark. This is the only network call
in Zest, and it never happens without your answer. For LibreOffice (too big to bundle), **Get LibreOffice…** opens the
download page in your browser. Nothing else is downloaded, and the command line never downloads.

## Settings

**Settings…** in the tray menu opens a form for the config file. **Open settings file** is still there if you want to
edit the text.

| Section | What you can set |
|---|---|
| Gestures | Drag gesture and Hold gesture on/off, the modifier (Ctrl, Alt, Shift, Ctrl+Shift), the hold time (100 to 3000 ms), the scope (File managers — Explorer, File Pilot, and `[gestures] file_managers` — or Anywhere), the key chord and the fallback chord (type `Ctrl+Alt+Space`, or press **Press keys…** and then the keys), pause gestures, quiet mode, programs to ignore (comma-separated, for example `game.exe`). Hold/hotkey selection still uses Explorer COM only; drag works in any allowed file manager. |
| Wheel | Outer radius (110 to 200), reduce animations (follow Windows, or always). |
| Output | Next to the source file, or a folder (**Browse…**); send outputs to the Shelf; privacy mode; keep timestamps; default quality (High, Medium, Small). |
| Jobs | Video workers (Auto, 1 to 8), image workers (Auto, 1 to 16). |
| System | Start at sign-in; add to the right-click menu and Send To (the same switches as in the tray menu). |
| Watch folders | The list of watch folders with **Add…**, **Edit…** and **Remove** (see [Watch folders](#watch-folders)). |
| Engines | Status of libvips, FFmpeg, PDFium, LibreOffice and Pandoc, **Open tools folder**, and for the optional engines that are not installed: **Download Pandoc (40 MB)** and **Get LibreOffice…**. |

**Save** writes `config.toml`. Zest changes only the lines you changed. Your comments, the order of the lines and
keys that the form does not know stay as they are. The file is written to a temporary file first and then renamed. The
file watcher loads it, so the new settings work at once. A wrong value shows a red message under the field and nothing is
saved. If `config.toml` has a syntax error, Save tells you and does not touch the file. **Reset to defaults** asks first and
only fills the form; press Save to keep it. **Cancel** (or Esc) closes the window without a change.

The windows follow the Windows theme (light or dark) and the monitor DPI.

## Config file

`%APPDATA%\Zest\config.toml`. Zest writes a commented file on the first run and reloads it when you save it. If
the file has an error, a balloon says "Config error on line N" and Zest keeps the old settings.

```toml
[[triggers]]                      # kind: drag | hold | key_chord
kind = "drag"
modifiers = ["Ctrl"]              # exact match; Ctrl, Alt, Shift, Win
button = "left"
drag_threshold = 0                # 0 = Windows default
scope = "file_manager"            # file managers (Explorer, File Pilot, …) | anywhere
# Hotkey/hold selection uses Explorer COM only; drag works in any allowed file manager.

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
scope = "anywhere"
fallback_key_chord = { modifiers = ["Win", "Alt"], key = "Z" }

[gestures]
paused = false
excluded_processes = []           # for example ["photoshop.exe"]
# Extra file managers beyond explorer.exe / FPilot.exe / FilePilot.exe:
# file_managers = ["totalcmd64.exe"]

[ui]
quiet = false                     # true = no success balloons (errors still notify)

[wheel]
outer_radius = 140                # logical pixels
reduce_animations = "system"      # "system" (follow Windows) or "always"
favorites = { heic = "jpg", mov = "mp4" }

[output]
location = "next_to_source"       # or a folder path
privacy_mode = false              # true = strip metadata on every conversion
keep_timestamps = false
send_to_shelf = false             # true = finished outputs go to the Shelf
quality = "high"                  # high | medium | small (for new jobs)

[jobs]
video_concurrency = "auto"        # "auto" or a number
image_concurrency = "auto"

# Watch folders (see the Watch folders section): one table per folder.
# [[watch]]
# folder = 'C:\Users\you\Pictures\Inbox'
# op = "convert:jpg"
# include = ["*.heic"]
# recursive = false
# enabled = true

[tools]                           # optional full paths; leave out for automatic search
# soffice = 'C:\Program Files\LibreOffice\program\soffice.exe'
# pandoc = 'C:\Users\you\AppData\Local\Pandoc\pandoc.exe'
# vips = 'D:\tools\vips\bin\vips.exe'
# ffmpeg = 'D:\tools\ffmpeg\bin\ffmpeg.exe'
# ffprobe = 'D:\tools\ffmpeg\bin\ffprobe.exe'
```

`[tools]` changes are picked up when you save the file (Zest looks for the engines again). `soffice` and `pandoc`
are the optional document engines; the others override the bundled tools in `third_party\bin`.

Sector 1 of the wheel is the favorite for the input type, or the last target you picked for it.

## Files and logs

| Path | What |
|---|---|
| `%APPDATA%\Zest\config.toml` | Settings. |
| `%LOCALAPPDATA%\Zest\logs\zest.<date>.log` | Daily log, the last 7 files. Set the level with the `ZEST_LOG` environment variable (`error`, `warn`, `info`, `debug`). The log holds file names, not full paths. |
| `%LOCALAPPDATA%\Zest\history.jsonl` | One JSON line when a job starts and one when it ends (id, time, inputs, outputs, target, errors). Undo adds an `undone` line. |
| `%LOCALAPPDATA%\Zest\state.json` | The last target you picked for each input type, the last folder of "Copy to" / "Move to", and the Shelf (items, position, option). |
| `%LOCALAPPDATA%\Zest\tmp` | Scratch folders for resize, rotate and video-size jobs. Each job deletes its own; folders older than a day are removed at start. |

Zest does not resume interrupted jobs. At start it deletes old `*.zest-tmp` files in the folders of recent jobs.

For a quick smoke test, set the environment variable `ZEST_DEBUG_OPEN_PANELS=1` and start `zest.exe`. It opens the
Shelf, History and Settings windows once, writes a line to the log, and closes them after about 2 seconds.

## Uninstall

```powershell
.\tools\uninstall.ps1
```

It removes the registry entries and the Send To shortcut, stops Zest and deletes `%LOCALAPPDATA%\Programs\Zest`.
It asks before it deletes `%APPDATA%\Zest` (default: keep).

## Project layout

| Crate | Purpose |
|---|---|
| `zest-core` | Formats, conversion graph, operations (conversions, tools, actions), naming, config. No Windows code. |
| `zest-broker` | Job queue, tool discovery, libvips, FFmpeg, PDFium (worker), LibreOffice and Pandoc adapters, the Zip and Copy as actions, Job Objects. |
| `zest-worker` | `zest-worker.exe`: PDF pages to images and PDF merge with PDFium (`pdfium-render`). Started by the broker, never by hand. |
| `zest-input` | Raw Input gesture detector, hold timer, key chords. |
| `zest-overlay` | The wheel window (Direct2D / DirectComposition) and progress ring. |
| `zest-shell` | Drop target, Explorer selection, change notify, registry verbs, wheel actions (clipboard, folder picker, copy/move, terminal), the Windows share sheet. |
| `zest-panels` | The Shelf, History and Settings windows, the Crop, Trim and watch folder dialogs (plain Win32 and Common Controls v6), and their list and form models (`toml_edit` for the config). |
| `zest-app` | The `zest` program: tray, single instance, CLI, wiring. |
| `zest-spike` | Phase 0 test program (see below). |

```powershell
cargo build --release
cargo test --workspace
cargo clippy --workspace --all-targets
```

## zest-spike (Phase 0)

`zest-spike` tested one question: does the drag gesture work on Windows 11? It has no conversion code. Build it with
`cargo build --release -p zest-spike` and run `target\release\zest-spike.exe` (a console program that logs to
`%LOCALAPPDATA%\Zest\spike.log`). Do not run it together with `zest.exe`: both want the same gestures and hotkeys.
The test steps and results are in `docs\DRAG_DROP_FEASIBILITY.md`.

## Credits

- [Filee](https://github.com/KnifeLemon/Filee) (MIT) by KnifeLemon: the gesture design (Ctrl + drag, late
  modifier, 450 ms hold, file-manager scope, window lifetime) is taken from it.
- [libvips](https://www.libvips.org/) (LGPL): image conversion.
- [FFmpeg](https://ffmpeg.org/) (GPL build): video and audio conversion.
- [Tangerine for Mac](https://tangerineformac.com/): inspiration for the tools and actions ideas.

Zest is for personal use. If you ever share it, check the licenses of the tools and codecs again.
