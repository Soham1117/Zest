# Zest — Manual Test Plan

Automated tests pass (about 440 tests, `cargo test --workspace`). The items below need a person, because they use the real desktop, Explorer and dialogs.

Mark each line ✓ / ✕. For each ✕, send the matching lines from `%LOCALAPPDATA%\Zest\logs\` (newest file).

## 0. Install

```
powershell -ExecutionPolicy Bypass -File tools\install.ps1
```

- [ ] The script finishes without errors. The Zest icon (orange circle with white Z) shows in the tray.
- [ ] Right-click the tray icon. The menu shows: Pause gestures, Show shelf, History…, Settings…, Open settings file, Open log folder, Start at sign-in, Add to right-click menu, Pause watch folders, Quit.

## 1. Gestures and wheel (most important)

- [ ] Hold **Ctrl** and drag a JPG in Explorer. The wheel opens under the pointer.
- [ ] Drop it on "PNG". A progress ring shows, then ✓. `photo.png` appears next to the original. **The original is still there.**
- [ ] Start a normal drag, then press Ctrl. The wheel opens.
- [ ] Hold Ctrl and press on a file for half a second without moving. The wheel opens for the selection (click mode). Click a sector.
- [ ] Select files and press **Win+Alt+Z**. The wheel opens. Arrows, 1–8 and Enter work. Esc closes it.
- [ ] Press Win+Alt+Z with no Explorer window in front. The "Drop files here" drop zone opens.
- [ ] Drop in the center of the wheel, or outside it. Nothing is converted, and no file moves.
- [ ] Ctrl+drag text in a browser. Nothing appears, and your next click works normally.
- [ ] Hover "More ›", "Tools ›" or "Actions ›" for a moment. The sub-ring opens. The 10-item rings are readable.
- [ ] Select a JPG and an MP4 together. The wheel shows "Images (1) ›", "Videos (1) ›" and "Actions ›".
- [ ] Routed targets show "↝" (for example on documents, if LibreOffice/Pandoc are installed).
- [ ] Multi-monitor and 125–150 % scaling: the wheel is sharp and fully on screen near the edges.

## 2. Conversions

- [ ] Image: HEIC (a real iPhone photo) → JPG. The photo is not rotated wrongly.
- [ ] Image: PNG → WebP, AVIF, TIFF.
- [ ] Video: MOV → MP4, MP4 → WebM, MP4 → GIF, MP4 → MP3. The progress ring moves.
- [ ] Audio: FLAC → MP3, WAV → M4A, MP3 → Opus.
- [ ] Convert the same file twice. The second output is `name (1).ext`.
- [ ] A long video (> 5 s job): the ring hands off, and a balloon "Converted 1 file" appears. Click the balloon: Explorer opens with the file selected.
- [ ] A corrupt or renamed file: a balloon shows a clear error, and no `.zest-tmp` file is left.
- [ ] PDF → PNG: a folder `name (pages)` appears. Select 2 PDFs → Tools → Merge: `name-merged.pdf`.

## 3. Tools and actions

- [ ] Tools: Compress, 50 %, Max 1920, Strip meta, Rotate on a JPG. Output names: `-compressed`, `-50pct`, `-1920`, `-stripped`, `-rotated`.
- [ ] Video → Tools → 10 MB: the result is 10 MB or less.
- [ ] Crop… on a photo: the dialog opens, the selection box and the aspect presets work, and the result matches what you selected (also on a rotated phone photo).
- [ ] Trim… on a video: the time fields and sliders stay in sync, the thumbnails show, and the result has the right length.
- [ ] Actions: Copy path (paste into Notepad), Copy files (paste in Explorer), Copy to…, Move to… (Explorer's own dialog; Ctrl+Z in Explorer undoes it), Terminal, Zip.
- [ ] Copy as PNG: paste into Paint or a chat app.
- [ ] Share: the Windows share sheet opens. **This is the most likely item to fail.**
- [ ] Select a folder: only the actions show.

## 4. Shelf, History, Settings

- [ ] Shelf: drop files on it, then drag them out to a folder. **The file is copied, not moved.** It survives a restart of Zest.
- [ ] History: jobs are listed. Show in folder, Re-run, Retry failed, and Undo (outputs go to the Recycle Bin, after a confirm). Search works. The taskbar progress shows while a job runs.
- [ ] Settings: change the gesture modifier, outer radius and output folder, then Save. The change applies without a restart, and the comments in `config.toml` are kept.
- [ ] Settings → Engines: "Download Pandoc (40 MB)" asks first, shows progress, then a ✓. "Get LibreOffice…" opens the browser.
- [ ] Dark mode: the windows and controls are readable.

## 5. Watch folders

- [ ] Settings → Watch folders → Add: a folder, `convert:jpg`, pattern `*.heic`. Save.
- [ ] Copy a HEIC into that folder. A JPG appears after about 2 s and shows in History. No ring is shown.
- [ ] Tray → Pause watch folders: new files are skipped.

## 6. Right-click menu and CLI

- [ ] Tray → "Add to right-click menu". Right-click a file → Show more options → "Convert with Zest". Select 5 files: one wheel opens for all 5.
- [ ] Send To → Zest works.
- [ ] `zest convert --to webp file.png` and `zest formats` print results.
- [ ] Optional, Windows 11 top-level menu:
  1. `winget install Microsoft.WindowsSDK.10.0.26100` (once)
  2. In an **administrator** PowerShell: `powershell -ExecutionPolicy Bypass -File tools\register-win11-menu.ps1`
  3. `Stop-Process -Name explorer -Force`
  4. Right-click a file. "Convert with Zest" is in the top-level menu.

## 7. Uninstall

```
powershell -ExecutionPolicy Bypass -File tools\uninstall.ps1
```

- [ ] The tray icon is gone, and the right-click entries are gone. Your config stays (the script asks).
