# Engine command lines (tested)

Tested on 2026-10-18 with libvips 8.18.7 and FFmpeg n8.1.3 (GPL, BtbN win64 build).
Tools live in `third_party/bin/vips/bin/vips.exe` and `third_party/bin/ffmpeg/bin/{ffmpeg,ffprobe}.exe`.
Every command below ran with exit code 0 on the files in `tests/fixtures/`.
`<in>` = input path, `<out>` = output path. Pass each argument as its own argv item (no shell).

## Rules for the broker

- Write to a temp name in the output folder, then rename when done.
- FFmpeg: always use `-hide_banner -loglevel error -nostdin -y`. Use `-y` only after the broker picked a free name.
- Set the child process to hide its console window (`CREATE_NO_WINDOW`).
- To cancel: kill the process, delete the temp output.
- Exit code 0 means success. Any other code means failure. Read stderr for the reason.

## Images (libvips)

General form: `vips copy <in> <out>[options]`. The output extension picks the saver.
Options go in square brackets, comma separated, directly after the output path.
Quote the argument as one argv item, for example `C:\o\a.jpg[Q=85,strip]`.

| Target | Command | Notes |
|---|---|---|
| PNG | `vips copy <in> <out>.png[compression=6]` | lossless |
| JPEG | `vips copy <in> <out>.jpg[Q=85,strip]` | Q 1-100, default 75. Use `strip` to drop metadata. |
| WebP | `vips copy <in> <out>.webp[Q=80,effort=4]` | add `lossless` for lossless. effort 0-6. |
| AVIF | `vips copy <in> <out>.avif[Q=50,compression=av1,effort=4]` | AV1 encode works (libaom). Add `lossless` for lossless. |
| TIFF | `vips copy <in> <out>.tiff[compression=deflate]` | also `lzw`, `jpeg`, `none` |
| GIF | `vips copy <in> <out>.gif` | works (cgif) |
| BMP | `vips copy <in> <out>.bmp` | works (via ImageMagick loader/saver in the "all" build) |

Notes:

- Decode works for PNG, JPEG, WebP, TIFF, GIF, BMP, AVIF. Same `vips copy` form.
- JPEG output from an image with alpha: flatten first, for example `vips flatten <in> <out>.jpg[Q=85] --background "255 255 255"` (not tested here).
- Auto-rotate by EXIF: `vips autorot <in> <out>` (not tested here).
- Set threads: `--vips-concurrency=2` (tested, accepted).
- Progress on stderr: add `--vips-progress`. Output looks like:
  `vips.exe temp-6: 320 x 240 pixels, 24 threads, 320 x 16 tiles, 512 lines in buffer`
  then `vips.exe temp-6: 80% complete`. Images are small and fast, so a spinner is enough.
- Wrong extension: `fake.png` (really JPEG) loads fine in vips, because vips sniffs the content. The broker should still check the signature itself.
- Corrupt file: `corrupt.jpg` gives exit code 1 and stderr lines starting with `VipsJpeg:` (for example `Unsupported marker type 0x1b`). stderr also holds `VIPS-WARNING` lines. Treat exit code 1 as failure.

### HEIC / HEIF

- HEIC encode: NOT possible. `vips copy a.png a.heic[compression=hevc]` gives `heifsave: Unsupported compression` (no x265 in libheif). Do not offer HEIC as an output.
- HEIC decode: not tested. No HEIC sample exists (we cannot encode one, and we do not download images). `libheif.dll` contains the libde265 decoder strings, so decode should work. Test with a real iPhone photo: `vips copy photo.heic out.jpg[Q=90]`.
- AVIF decode and encode: works (tested).

## Video (FFmpeg)

Base prefix: `ffmpeg -hide_banner -loglevel error -nostdin -y -progress pipe:1 -nostats -i <in> ... <out>`

| Target | Command (after `-i <in>`) |
|---|---|
| MP4 (H.264/AAC) | `-map 0:v:0? -map 0:a:0? -c:v libx264 -crf 23 -preset medium -pix_fmt yuv420p -vf "scale=trunc(iw*sar/2)*2:trunc(ih/2)*2,setsar=1" -metadata:s:v:0 rotate=0 -c:a aac -b:a 192k -movflags +faststart <out>.mp4` |
| WebM (VP9/Opus) | `-map 0:v:0? -map 0:a:0? -c:v libvpx-vp9 -crf 35 -b:v 0 -row-mt 1 -cpu-used 2 -pix_fmt yuv420p -vf "scale=trunc(iw*sar/2)*2:trunc(ih/2)*2,setsar=1" -metadata:s:v:0 rotate=0 -c:a libopus -b:a 128k <out>.webm` |
| MKV remux (square SAR, no rotation) | `-map 0 -c copy -map_metadata 0 <out>.mkv` |
| MKV re-encode (non-square SAR or rotate) | same as MP4 body but `-f matroska` (no `+faststart`); broker sets this when ffprobe shows `sample_aspect_ratio` ≠ `1:1` or a non-zero `rotate` / `rotation` |
| GIF (video, palette) | `-an -vf "fps=12,scale=min(iw,480):-1:flags=lanczos,…" -loop 0 <out>.gif` |
| MP3 from video | `-vn -c:a libmp3lame -q:a 2 <out>.mp3` |
| WAV from video | `-vn -c:a pcm_s16le <out>.wav` |

Notes:

- Re-encodes use **SAR-aware scale** (`iw*sar`) then `setsar=1`, and clear stale `rotate` metadata, so phone / anamorphic video keeps the same display ratio as the source. Without this, players often show a different ratio after convert.
- MKV prefers stream copy. If ffprobe reports a non-square SAR or rotation, the broker re-encodes instead (Matroska remux can drop DisplayMatrix / rotate tags).
- Target-size (video to N MB) uses the same SAR filter, with an optional max width: `scale=w='min(W,trunc(iw*sar/2)*2)':h=-2,setsar=1`.
- `-map 0 -c copy` can fail for MP4 with data or subtitle streams. Use `-map 0:v? -map 0:a?` for a safer remux.
- GIF: for a width limit, change `scale=320:-1`. Drop `fps` lower for smaller files. A 2 s 320x240 GIF was about 150 KB.
- `scale` for H.264 needs even width and height; the SAR filter already rounds to even.

## Audio (FFmpeg)

Same prefix. Add `-vn` to ignore cover art and video.

| Target | Command (after `-i <in>`) |
|---|---|
| MP3 | `-vn -map_metadata 0 -c:a libmp3lame -b:a 192k <out>.mp3` (VBR option: `-q:a 2`) |
| WAV | `-vn -c:a pcm_s16le <out>.wav` |
| FLAC | `-vn -c:a flac -compression_level 5 <out>.flac` |
| M4A (AAC) | `-vn -c:a aac -b:a 192k -movflags +faststart <out>.m4a` |
| Opus | `-vn -c:a libopus -b:a 128k <out>.opus` |
| OGG (Vorbis) | `-vn -c:a libvorbis -q:a 5 <out>.ogg` |

Also works: image to image with FFmpeg (`-i in.jpg out.png`). Use vips for images, though.

## Tools (added in Phase 2; the broker tests run all of them)

Image tools use the saver by name, because the temp name `*.zest-tmp` has no image suffix. Tools that change
pixels (resize, rotate) write a scratch `step.v` first (in `%LOCALAPPDATA%\Zest\tmp\<pid>-<n>\`), then a second
`vips` call saves the real output. The scratch folder is deleted after each file.

| Tool | Commands (the saver is `jpegsave`, `pngsave`, `webpsave`, `heifsave` or `tiffsave`) |
|---|---|
| Compress image | `vips <saver> <in> <out> <options>`. JPEG `--Q 82 --background 255 --optimize-coding`, PNG `--compression 9 --effort 10 --filter all` (lossless), WebP `--Q 80 --effort 6`, AVIF `--Q 50 --compression av1 --effort 4`, TIFF `--compression deflate`. |
| Resize 50 % / 25 % | `vips resize <in> step.v 0.5` then `vips <saver> step.v <out> <options>` (High quality) |
| Max edge 1920 / 1280 | `vips thumbnail <in> step.v 1920 --height 1920 --size down` then the saver |
| Rotate | `vips rot "<in>[autorotate]" step.v d90` (`d270`, `d180`) then the saver. `[autorotate]` only for JPEG, HEIC, HEIF, AVIF inputs. |
| Strip metadata (image) | `vips <saver> "<in>[autorotate]" <out> <options> --keep icc` (privacy mode: `--keep none`) |
| Crop | `vips crop "<in>[autorotate]" step.v <x> <y> <w> <h>` then the saver. `[autorotate]` only for JPEG, HEIC, HEIF, AVIF inputs, so the rectangle is in the pixels the user saw. A rectangle outside the image fails with the libvips message. |

| Tool | FFmpeg command (after the common prefix and `-i <in>`) |
|---|---|
| Compress video | MP4 conversion at the Small preset: `-map 0:v:0? -map 0:a:0? -c:v libx264 -crf 28 -preset medium -pix_fmt yuv420p -vf scale=trunc(iw/2)*2:trunc(ih/2)*2 -c:a aac -b:a 128k -movflags +faststart -f mp4 <out>` |
| Compress audio | `-vn -map 0:a:0` plus MP3 `-c:a libmp3lame -b:a 128k`, M4A `-c:a aac -b:a 128k -movflags +faststart` (`-f ipod`), Opus `-c:a libopus -b:a 64k`, Ogg `-c:a libvorbis -q:a 3`, FLAC `-c:a flac -compression_level 12`. WAV: not offered. |
| Strip metadata (video) | `-map 0:v? -map 0:a? [-map 0:s? for MKV] -c copy -map_metadata -1 -map_chapters -1 -fflags +bitexact [-movflags +faststart for MP4/MOV] -f <same muxer> <out>` |
| Strip metadata (audio) | `-vn -map 0:a:0 -c copy -map_metadata -1 -map_chapters -1 -fflags +bitexact -f <same muxer> <out>` (muxers: mp3, wav, flac, ipod, ogg, opus) |
| Trim video | The MP4 conversion at the High preset (CRF 20, AAC 256k) with `-ss <start>` placed **before** `-i` (fast seek, exact cut because the video is encoded again) and `-t <end - start>` before `-f mp4` (the output starts at 0 after the input seek, so `-t` is the length). No `-t` when the end is 0 (to the end). The start must be before the end of the file. |
| Trim audio | `-ss <start>` before `-i`, `-vn -map 0:a:0`, the same codec family as Compress audio at a higher quality (MP3 `-q:a 2`, M4A `aac 256k`, Opus `160k`, Ogg `-q:a 6`, FLAC, WAV `pcm_s16le`), `-t <length>`, the same muxer. |
| Video to N MB, pass 1 | `-map 0:v:0 -c:v libx264 -b:v <v>k -preset medium -pix_fmt yuv420p -vf <scale> -pass 1 -passlogfile <scratch>\pass -an -f null NUL` |
| Video to N MB, pass 2 | `-map 0:v:0 -c:v libx264 -b:v <v>k -preset medium -pix_fmt yuv420p -vf <scale> -pass 2 -passlogfile <scratch>\pass -map 0:a:0? -c:a aac -b:a 128k -movflags +faststart -f mp4 <out>` |

Video to N MB: `total kbit = N * 8192 * 0.96`, `v = total / seconds - 128` (kbit/s, audio is AAC 128k). `v < 150`
fails with "Too long to fit in N MB". `v < 700` limits the width to 854, `v < 1500` to 1280:
`scale=w='min(1280,trunc(iw/2)*2)':h=-2`. Otherwise `scale=trunc(iw/2)*2:trunc(ih/2)*2`. The progress of pass 1 is
mapped to 0-50 % and pass 2 to 50-100 %. A result larger than N MB (1 MB = 1048576 bytes) plus 5 % fails with the real size.

Preview helpers for the Crop and Trim windows (`zest_broker::preview`; they never change the source):
`vipsheader -f width|height|orientation <file>` and `vips thumbnail <in> <png> 1600 --height 1600 --size down [--no-rotate]`
(`--no-rotate` for the formats that the Crop job does not turn), `ffprobe -show_entries format=duration` and
`ffmpeg -ss <t> -i <in> -frames:v 1 -vf scale='min(480,iw)':-2 <png>`.

## PDF (`zest-worker.exe`, PDFium; added in Phase 3)

The worker is a small program of this project (`crates/zest-worker`, `pdfium-render`). It loads
`third_party\bin\pdfium\bin\pdfium.dll` (PDFium chromium/8100 from `bblanchon/pdfium-binaries`). The broker starts it
like vips and FFmpeg: argument array, no console window, below-normal priority, Job Object with a memory limit.
The probe needs the DLL and a worker that answers `zest-worker --version`.

| Job | Command |
|---|---|
| PDF to PNG/JPG | `zest-worker --pdfium <dll> pdf-to-images --format png\|jpg --dpi 150 --quality <N> <in.pdf> <out_dir>` |
| Merge PDFs | `zest-worker --pdfium <dll> merge-pdf <out> <in1.pdf> <in2.pdf> ...` (order as given) |

- `pdf-to-images` writes `<in stem>-p001.png` (at least 3 digits) for each page and prints `progress <percent>`
  after each page. The broker gives it the temp folder `<name> (pages).zest-tmp\` and renames the folder to
  `<name> (pages)\` when the worker is done and every page has a valid image signature. JPG quality: 92 / 82 / 70
  for High / Medium / Small. PNG ignores the quality.
- Limits: 500 pages, 50 megapixels per page at the given DPI. The worker checks all page sizes before it renders
  and fails with `Page N is too large at 150 DPI (W x H pixels; the limit is 50 megapixels)`.
- Errors: one line `error: <text>` on stderr, exit code 1 (exit code 2 = wrong arguments). A password-protected PDF gives
  `The PDF is password protected`.
- `merge-pdf` appends the pages of each input in the given order (`PdfPages::append`). The broker passes the temp file
  `<first name>-merged.pdf.zest-tmp` as `<out>` and checks the `%PDF-` signature of the result.

## Documents (optional engines; added in Phase 3)

Both run with a 120 s time limit (the process tree is ended through the Job Object when it is over).

**LibreOffice** (`soffice.exe` in `%ProgramFiles%\LibreOffice\program\` or the x86 folder, or `[tools] soffice`):

```text
soffice --headless --norestore --nologo -env:UserInstallation=file:///<scratch>/profile
        --convert-to <filter> --outdir <scratch>\out <in>
```

`<scratch>` is the private folder of the job (`%LOCALAPPDATA%\Zest\tmp\<pid>-<n>\`). Its `profile` is a fresh LibreOffice
user profile, so jobs can run in parallel and Zest never uses the profile of a running LibreOffice. `<filter>` is
`pdf`, `docx`, `odt`, `pptx`, `xlsx`, or for CSV `csv:Text - txt - csv (StarCalc):44,34,76,1` (comma, `"`, UTF-8).
LibreOffice names the result `<in stem>.<ext>`; the broker moves it onto the temp output file and deletes the scratch folder.

**Pandoc** (search order: `[tools] pandoc`, `third_party\bin\pandoc\pandoc.exe`, `%LOCALAPPDATA%\Zest\engines\pandoc\pandoc.exe`,
`PATH`, `%LOCALAPPDATA%\Pandoc\`, `%ProgramFiles%\Pandoc\`):

```text
pandoc -f <markdown|docx|html|epub> -t <docx|odt|epub|html|gfm> [--standalone (html)] [--wrap=none (gfm)]
       --resource-path=<folder of the input> -o <out.zest-tmp> <in>
```

The writer is named with `-t`, so the temp file name does not matter. Markdown output is GitHub flavored (`gfm`).

## Multi-hop routes (Phase 4)

If a conversion has no direct edge, `FormatGraph::route(from, to)` gives the cheapest chain of at most 3 edges
(`zest-core/src/route.rs`). Cost per edge: 1.0, +0.5 if lossy, plus a small engine tie-break
(Vips < Ffmpeg < Pdfium < Pandoc < LibreOffice). A direct edge always wins. Rules:

- A lossless target never goes through a lossy step (no PNG -> JPG -> TIFF). MP4 -> FLAC goes MP4 -> WAV -> FLAC.
- HEIC, HEIF and GIF are never a middle format.
- Categories do not mix, except Video -> Audio, and PDF -> image pages (DOCX -> PDF -> PNG pages: LibreOffice + PDFium).
  Markdown -> PDF goes MD -> DOCX -> PDF (Pandoc + LibreOffice).
- The PDF pages step writes a folder, so it is always the last step.
- `FormatGraph::wheel_model` lists the multi-hop targets after the direct ones. `route_hops(from, to)` tells the
  overlay how many steps an item needs (more than 1 = show a small arrow). `WheelItem` has no new field.

The broker runs a route inside the per-file scratch folder. Each middle step writes `step<n>.<ext>` there (through a
`.zest-tmp` file, same checks as a normal output). Only the last step writes next to the source (temp file, signature
check, atomic rename). Progress is split evenly: step `i` of `n` covers `i/n` to `(i+1)/n`. The scratch folder is
deleted on success, failure and cancel. The app must accept a `Convert` job when `graph.route(...)` is `Some`
(not only `graph.edge(...)`).

## Optional engine download (Phase 4)

`tools\fetch-deps.ps1 -Optional pandoc` downloads the pinned portable Pandoc zip into `third_party\bin\pandoc\`.
Entries with `"optional": true` in `tools\deps.lock.json` are skipped by default.

At run time the app can call `zest_broker::engines::download_optional("pandoc", &mut progress)` after the user agrees.
It reads the same lock file (embedded at build time), downloads over HTTPS (`ureq`, rustls) to a staging folder in
`%LOCALAPPDATA%\Zest\engines\`, checks SHA-256 (`sha2`), unpacks (`zip`, with a path check), and moves the result to
`%LOCALAPPDATA%\Zest\engines\pandoc\`. It returns the path of `pandoc.exe`. `engines::installed_path("pandoc")` gives the
expected path. LibreOffice is too big for this: the app opens `engines::libreoffice_download_url()`
(https://www.libreoffice.org/download/download-libreoffice/) and the user installs it.

## Progress output (`-progress pipe:1 -nostats`)

FFmpeg writes one block of `key=value` lines to stdout about every 0.5 s. Each block ends with `progress=continue`. The last block ends with `progress=end`.
Parse by reading lines. When you see `progress=...`, the block is complete.

Sample from a 20 s 1280x720 re-encode (`-c:v libx264 -preset veryslow -crf 18 -c:a aac`):

```
frame=0
fps=0.00
stream_0_0_q=0.0
bitrate=N/A
total_size=48
out_time_us=N/A
out_time_ms=N/A
out_time=N/A
dup_frames=0
drop_frames=0
speed=N/A
progress=continue
frame=39
fps=38.03
stream_0_0_q=24.0
bitrate=3401.1kbits/s
total_size=524336
out_time_us=1233333
out_time_ms=1233333
out_time=00:00:01.233333
dup_frames=0
drop_frames=0
speed= 1.2x
progress=continue
frame=82
fps=53.16
stream_0_0_q=24.0
bitrate=3932.3kbits/s
total_size=1310768
out_time_us=2666667
out_time_ms=2666667
out_time=00:00:02.666667
dup_frames=0
drop_frames=0
speed=1.73x
progress=continue
```

Final block of a short file (2 s sample):

```
frame=50
fps=0.00
stream_0_0_q=-1.0
bitrate= 133.1kbits/s
total_size=31939
out_time_us=1920000
out_time_ms=1920000
out_time=00:00:01.920000
dup_frames=0
drop_frames=0
speed=37.9x
progress=end
```

Rules:

- Percent = `out_time_us / (duration_seconds * 1_000_000)`. Use `out_time_us`.
- `out_time_ms` is NOT milliseconds. It has the same value as `out_time_us` (microseconds). Ignore it.
- `out_time_us` can be `N/A` (first block). Skip the block then.
- Values can have leading spaces (`speed= 1.2x`, `bitrate= 133.1kbits/s`). Trim them.
- Clamp percent to 0-100. On `progress=end`, set 100.
- For very short files, only the `progress=end` block may appear.

### Get the duration first (ffprobe)

```
ffprobe -v error -show_entries format=duration -of csv=p=0 <in>
```

Output for `sample.mp4`: `2.000000`. For the full stream info use
`ffprobe -v error -print_format json -show_format -show_streams <in>`.
If the duration is `N/A` or empty, show an indeterminate spinner.

### Errors

Corrupt input (`corrupt.jpg` through FFmpeg) gives exit code 69 (`Invalid data found when processing input`, `Nothing was written into output file`). Treat any non-zero code as failure and delete the temp output.
