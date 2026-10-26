//! FFmpeg adapter. Command lines follow `docs/ENGINE_COMMANDS.md`.
//! The temp output ends in `.zest-tmp`, so the muxer is always named with `-f`.

use crate::adapter::{os, q3, safe_arg, Adapter, CommandSpec, PlanError, PlanOptions};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use zest_core::{Category, EngineId, FormatId, JobRequest, Operation, Quality, ToolKind};

/// Longest GIF made from a video, in seconds.
pub const GIF_MAX_SECONDS: f64 = 15.0;
const GIF_FILTER: &str = "fps=12,scale='min(iw,480)':-1:flags=lanczos,split[a][b];\
[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=5";

/// Even dimensions with non-square SAR baked into pixels (phone / anamorphic video).
pub const EVEN_SAR_SCALE: &str = "scale=trunc(iw*sar/2)*2:trunc(ih/2)*2,setsar=1";

/// Max-width scale that still honors SAR, then normalizes to square pixels.
pub fn max_width_sar_scale(w: u32) -> String {
    format!("scale=w='min({w},trunc(iw*sar/2)*2)':h=-2,setsar=1")
}

pub struct FfmpegAdapter {
    program: PathBuf,
}

impl FfmpegAdapter {
    pub fn new(program: PathBuf) -> Self {
        FfmpegAdapter { program }
    }
}

fn push(args: &mut Vec<OsString>, items: &[&str]) {
    args.extend(items.iter().map(|s| os(s)));
}

fn h264_crf(q: Quality) -> &'static str {
    q3(q, "20", "23", "28")
}
fn vp9_crf(q: Quality) -> &'static str {
    q3(q, "31", "35", "40")
}
fn mp3_v(q: Quality) -> &'static str {
    q3(q, "0", "2", "5")
}
fn aac_rate(q: Quality) -> &'static str {
    q3(q, "256k", "192k", "128k")
}
fn opus_rate(q: Quality) -> &'static str {
    q3(q, "160k", "128k", "96k")
}

/// Audio-only output (also used to extract audio from a video).
/// Returns the muxer name, or `None` if `to` is not an audio target.
fn audio_args(to: FormatId, q: Quality, args: &mut Vec<OsString>) -> Option<&'static str> {
    push(args, &["-vn", "-map", "0:a:0"]);
    match to {
        FormatId::Mp3 => {
            push(args, &["-c:a", "libmp3lame", "-q:a", mp3_v(q)]);
            Some("mp3")
        }
        FormatId::Wav => {
            push(args, &["-c:a", "pcm_s16le"]);
            Some("wav")
        }
        FormatId::Flac => {
            push(args, &["-c:a", "flac", "-compression_level", "5"]);
            Some("flac")
        }
        FormatId::M4a => {
            push(
                args,
                &["-c:a", "aac", "-b:a", aac_rate(q), "-movflags", "+faststart"],
            );
            Some("ipod")
        }
        FormatId::Opus => {
            push(args, &["-c:a", "libopus", "-b:a", opus_rate(q)]);
            Some("opus")
        }
        _ => None,
    }
}

/// Fixed audio rate of the target-size encode (kbit/s).
pub const TARGET_AUDIO_KBPS: f64 = 128.0;
/// Share of the size limit that the streams may use (the rest is container overhead).
pub const TARGET_FILL: f64 = 0.96;
/// Below this video rate (kbit/s) the video is too long for the size limit.
pub const MIN_VIDEO_KBPS: f64 = 150.0;

/// Video bit rate (kbit/s) and optional maximum width for a target size.
/// `total kbit = mb * 8192 * 0.96`, `video = total / seconds - 128`.
/// Fails with `TooLong` below 150 kbit/s. Below 1500 kbit/s the width is limited to 1280,
/// below 700 kbit/s to 854.
pub fn target_size_rates(mb: u32, seconds: f64) -> Result<(u32, Option<u32>), PlanError> {
    if !seconds.is_finite() || seconds <= 0.0 {
        return Err(PlanError::Other("Cannot read the video length".to_owned()));
    }
    let total_kbit = f64::from(mb) * 8192.0 * TARGET_FILL;
    let video = total_kbit / seconds - TARGET_AUDIO_KBPS;
    if video < MIN_VIDEO_KBPS {
        return Err(PlanError::TooLong(mb));
    }
    let max_width = if video < 700.0 {
        Some(854)
    } else if video < 1500.0 {
        Some(1280)
    } else {
        None
    };
    Ok((video.floor() as u32, max_width))
}

/// Milliseconds as seconds with 3 decimals: `1.500`.
fn secs(ms: u64) -> String {
    format!("{}.{:03}", ms / 1000, ms % 1000)
}

/// Muxer name for stream-copy output in the same container.
fn same_container_muxer(f: FormatId) -> Option<&'static str> {
    Some(match f {
        FormatId::Mp4 => "mp4",
        FormatId::Mov => "mov",
        FormatId::Mkv => "matroska",
        FormatId::Webm => "webm",
        FormatId::Avi => "avi",
        FormatId::Mp3 => "mp3",
        FormatId::Wav => "wav",
        FormatId::Flac => "flac",
        FormatId::M4a => "ipod",
        FormatId::Ogg => "ogg",
        FormatId::Opus => "opus",
        _ => return None,
    })
}

impl FfmpegAdapter {
    /// Arguments up to and including the input file.
    fn head(input: &Path) -> Result<Vec<OsString>, PlanError> {
        let mut args: Vec<OsString> = Vec::new();
        push(
            &mut args,
            &[
                "-hide_banner",
                "-nostdin",
                "-loglevel",
                "error",
                "-progress",
                "pipe:1",
                "-nostats",
                "-y",
                "-i",
            ],
        );
        args.push(safe_arg(input)?);
        Ok(args)
    }

    fn command(&self, args: Vec<OsString>) -> CommandSpec {
        CommandSpec {
            program: self.program.clone(),
            args,
        }
    }

    /// Body of a conversion: the encoder options and the muxer name.
    fn convert_body(
        from: FormatId,
        to: FormatId,
        q: Quality,
    ) -> Result<(Vec<OsString>, &'static str), PlanError> {
        let unsupported = PlanError::Unsupported { from, to };
        let mut body: Vec<OsString> = Vec::new();
        let muxer: &str = match from.category() {
            Category::Video => match to {
                FormatId::Mp4 => {
                    push(
                        &mut body,
                        &[
                            "-map",
                            "0:v:0?",
                            "-map",
                            "0:a:0?",
                            "-c:v",
                            "libx264",
                            "-crf",
                            h264_crf(q),
                            "-preset",
                            "medium",
                            "-pix_fmt",
                            "yuv420p",
                            "-vf",
                            EVEN_SAR_SCALE,
                            "-metadata:s:v:0",
                            "rotate=0",
                            "-c:a",
                            "aac",
                            "-b:a",
                            aac_rate(q),
                            "-movflags",
                            "+faststart",
                        ],
                    );
                    "mp4"
                }
                FormatId::Webm => {
                    push(
                        &mut body,
                        &[
                            "-map",
                            "0:v:0?",
                            "-map",
                            "0:a:0?",
                            "-c:v",
                            "libvpx-vp9",
                            "-crf",
                            vp9_crf(q),
                            "-b:v",
                            "0",
                            "-row-mt",
                            "1",
                            "-cpu-used",
                            "2",
                            "-pix_fmt",
                            "yuv420p",
                            "-vf",
                            EVEN_SAR_SCALE,
                            "-metadata:s:v:0",
                            "rotate=0",
                            "-c:a",
                            "libopus",
                            "-b:a",
                            opus_rate(q),
                        ],
                    );
                    "webm"
                }
                FormatId::Mkv => {
                    // Remux is fast. `plan_convert` may replace this with a
                    // re-encode when `PlanOptions::bake_display_aspect` is set.
                    push(&mut body, &["-map", "0", "-c", "copy", "-map_metadata", "0"]);
                    "matroska"
                }
                FormatId::Gif => {
                    push(
                        &mut body,
                        &["-an", "-t", "15", "-vf", GIF_FILTER, "-loop", "0"],
                    );
                    "gif"
                }
                FormatId::Mp3 | FormatId::Wav => audio_args(to, q, &mut body).ok_or(unsupported)?,
                _ => return Err(unsupported),
            },
            Category::Audio => {
                if to == FormatId::Ogg {
                    return Err(unsupported);
                }
                audio_args(to, q, &mut body).ok_or(unsupported)?
            }
            Category::Image | Category::Document => return Err(unsupported),
        };
        Ok((body, muxer))
    }

    fn plan_convert(
        &self,
        to: FormatId,
        q: Quality,
        input: &Path,
        tmp_out: &Path,
        opts: &PlanOptions,
    ) -> Result<Vec<CommandSpec>, PlanError> {
        let mut args = Self::head(input)?;
        // Phone/camera video often has non-square SAR or rotation side data.
        // Matroska remux can lose that; re-encode with SAR-aware scale instead.
        let (body, muxer) = if to == FormatId::Mkv && opts.bake_display_aspect {
            Self::mkv_reencode_body(q)?
        } else {
            Self::convert_body(opts.input_format, to, q)?
        };
        args.extend(body);
        if opts.privacy_mode {
            push(&mut args, &["-map_metadata", "-1", "-map_chapters", "-1"]);
        }
        push(&mut args, &["-f", muxer]);
        args.push(safe_arg(tmp_out)?);
        Ok(vec![self.command(args)])
    }

    /// Light H.264 re-encode into Matroska that bakes display aspect into pixels.
    fn mkv_reencode_body(q: Quality) -> Result<(Vec<OsString>, &'static str), PlanError> {
        let mut body: Vec<OsString> = Vec::new();
        push(
            &mut body,
            &[
                "-map",
                "0:v:0?",
                "-map",
                "0:a:0?",
                "-c:v",
                "libx264",
                "-crf",
                h264_crf(q),
                "-preset",
                "medium",
                "-pix_fmt",
                "yuv420p",
                "-vf",
                EVEN_SAR_SCALE,
                "-metadata:s:v:0",
                "rotate=0",
                "-c:a",
                "aac",
                "-b:a",
                aac_rate(q),
            ],
        );
        Ok((body, "matroska"))
    }

    fn plan_tool(
        &self,
        tool: ToolKind,
        input: &Path,
        tmp_out: &Path,
        opts: &PlanOptions,
    ) -> Result<Vec<CommandSpec>, PlanError> {
        let from = opts.input_format;
        let unsupported = PlanError::ToolUnsupported { tool, from };
        if !tool.applies_to(from) || tool.engine(from) != EngineId::Ffmpeg {
            return Err(unsupported);
        }
        let video = from.category() == Category::Video;
        match tool {
            // Video: H.264 in an MP4 at the requested quality (default Small).
            ToolKind::Compress if video => {
                self.plan_convert(FormatId::Mp4, opts.quality, input, tmp_out, opts)
            }
            // Audio: a lower bit rate in the same format.
            ToolKind::Compress => {
                let mut args = Self::head(input)?;
                push(&mut args, &["-vn", "-map", "0:a:0"]);
                let muxer = match from {
                    FormatId::Mp3 => {
                        push(&mut args, &["-c:a", "libmp3lame", "-b:a", "128k"]);
                        "mp3"
                    }
                    FormatId::M4a => {
                        push(
                            &mut args,
                            &["-c:a", "aac", "-b:a", "128k", "-movflags", "+faststart"],
                        );
                        "ipod"
                    }
                    FormatId::Opus => {
                        push(&mut args, &["-c:a", "libopus", "-b:a", "64k"]);
                        "opus"
                    }
                    FormatId::Ogg => {
                        push(&mut args, &["-c:a", "libvorbis", "-q:a", "3"]);
                        "ogg"
                    }
                    FormatId::Flac => {
                        push(&mut args, &["-c:a", "flac", "-compression_level", "12"]);
                        "flac"
                    }
                    _ => return Err(unsupported),
                };
                if opts.privacy_mode {
                    push(&mut args, &["-map_metadata", "-1", "-map_chapters", "-1"]);
                }
                push(&mut args, &["-f", muxer]);
                args.push(safe_arg(tmp_out)?);
                Ok(vec![self.command(args)])
            }
            ToolKind::Trim { start_ms, end_ms } => {
                if end_ms != 0 && end_ms <= start_ms {
                    return Err(PlanError::Other("The end time must be after the start time".to_owned()));
                }
                if let Some(d) = opts.duration {
                    if start_ms as f64 / 1000.0 >= d {
                        return Err(PlanError::Other("The start time is after the end of the file".to_owned()));
                    }
                }
                let mut cmds = if video {
                    // Same codecs as the MP4 conversion at High quality (an exact cut needs a re-encode).
                    self.plan_convert(FormatId::Mp4, Quality::High, input, tmp_out, opts)?
                } else {
                    let mut args = Self::head(input)?;
                    push(&mut args, &["-vn", "-map", "0:a:0"]);
                    let muxer = match from {
                        FormatId::Mp3 => {
                            push(&mut args, &["-c:a", "libmp3lame", "-q:a", "2"]);
                            "mp3"
                        }
                        FormatId::M4a => {
                            push(&mut args, &["-c:a", "aac", "-b:a", "256k", "-movflags", "+faststart"]);
                            "ipod"
                        }
                        FormatId::Opus => {
                            push(&mut args, &["-c:a", "libopus", "-b:a", "160k"]);
                            "opus"
                        }
                        FormatId::Ogg => {
                            push(&mut args, &["-c:a", "libvorbis", "-q:a", "6"]);
                            "ogg"
                        }
                        FormatId::Flac => {
                            push(&mut args, &["-c:a", "flac"]);
                            "flac"
                        }
                        FormatId::Wav => {
                            push(&mut args, &["-c:a", "pcm_s16le"]);
                            "wav"
                        }
                        _ => return Err(unsupported),
                    };
                    if opts.privacy_mode {
                        push(&mut args, &["-map_metadata", "-1", "-map_chapters", "-1"]);
                    }
                    push(&mut args, &["-f", muxer]);
                    args.push(safe_arg(tmp_out)?);
                    vec![self.command(args)]
                };
                for cmd in &mut cmds {
                    // `-ss` before `-i` seeks the input; the output then starts at 0, so `-t` is the length.
                    if let Some(i) = cmd.args.iter().position(|a| a == "-i") {
                        cmd.args.insert(i, os(&secs(start_ms)));
                        cmd.args.insert(i, os("-ss"));
                    }
                    if end_ms != 0 {
                        if let Some(f) = cmd.args.iter().rposition(|a| a == "-f") {
                            cmd.args.insert(f, os(&secs(end_ms - start_ms)));
                            cmd.args.insert(f, os("-t"));
                        }
                    }
                }
                Ok(cmds)
            }
            // Stream copy without tags, in the same container.
            ToolKind::StripMetadata => {
                let muxer = same_container_muxer(from).ok_or(unsupported)?;
                let mut args = Self::head(input)?;
                if video {
                    push(&mut args, &["-map", "0:v?", "-map", "0:a?"]);
                    if from == FormatId::Mkv {
                        push(&mut args, &["-map", "0:s?"]);
                    }
                } else {
                    push(&mut args, &["-vn", "-map", "0:a:0"]);
                }
                push(
                    &mut args,
                    &[
                        "-c",
                        "copy",
                        "-map_metadata",
                        "-1",
                        "-map_chapters",
                        "-1",
                        "-fflags",
                        "+bitexact",
                    ],
                );
                if matches!(from, FormatId::Mp4 | FormatId::Mov | FormatId::M4a) {
                    push(&mut args, &["-movflags", "+faststart"]);
                }
                push(&mut args, &["-f", muxer]);
                args.push(safe_arg(tmp_out)?);
                Ok(vec![self.command(args)])
            }
            ToolKind::VideoTargetSize(mb) => {
                let seconds = opts
                    .duration
                    .ok_or_else(|| PlanError::Other("Cannot read the video length".to_owned()))?;
                let (kbps, max_width) = target_size_rates(mb, seconds)?;
                let scratch = opts
                    .scratch_dir
                    .as_ref()
                    .ok_or_else(|| PlanError::Other("no scratch folder".to_owned()))?
                    .join("pass");
                let vf = match max_width {
                    Some(w) => max_width_sar_scale(w),
                    None => EVEN_SAR_SCALE.to_owned(),
                };
                let rate = format!("{kbps}k");
                let video_opts = |pass: &str, args: &mut Vec<OsString>| {
                    push(
                        args,
                        &[
                            "-map",
                            "0:v:0",
                            "-c:v",
                            "libx264",
                            "-b:v",
                            &rate,
                            "-preset",
                            "medium",
                            "-pix_fmt",
                            "yuv420p",
                            "-vf",
                            &vf,
                            "-pass",
                            pass,
                            "-passlogfile",
                        ],
                    );
                    args.push(scratch.clone().into_os_string());
                };
                // Pass 1: video only, no output file.
                let mut p1 = Self::head(input)?;
                video_opts("1", &mut p1);
                push(&mut p1, &["-an", "-f", "null", "NUL"]);
                // Pass 2: video and audio into the temp output.
                let mut p2 = Self::head(input)?;
                video_opts("2", &mut p2);
                push(
                    &mut p2,
                    &[
                        "-map",
                        "0:a:0?",
                        "-c:a",
                        "aac",
                        "-b:a",
                        "128k",
                        "-movflags",
                        "+faststart",
                    ],
                );
                if opts.privacy_mode {
                    push(&mut p2, &["-map_metadata", "-1", "-map_chapters", "-1"]);
                }
                push(&mut p2, &["-f", "mp4"]);
                p2.push(safe_arg(tmp_out)?);
                Ok(vec![self.command(p1), self.command(p2)])
            }
            _ => Err(unsupported),
        }
    }
}

impl Adapter for FfmpegAdapter {
    fn engine(&self) -> EngineId {
        EngineId::Ffmpeg
    }

    fn plan(
        &self,
        req: &JobRequest,
        input: &Path,
        tmp_out: &Path,
        opts: &PlanOptions,
    ) -> Result<Vec<CommandSpec>, PlanError> {
        match req.op {
            Operation::Convert(to) => self.plan_convert(to, req.quality, input, tmp_out, opts),
            Operation::Tool(tool) => self.plan_tool(tool, input, tmp_out, opts),
            Operation::Action(_) => Err(PlanError::Other("actions are not run by FFmpeg".to_owned())),
        }
    }

    fn parse_progress(&self, line: &str, duration: Option<f64>) -> Option<f32> {
        let line = line.trim();
        if line == "progress=end" {
            return Some(100.0);
        }
        let value = line.strip_prefix("out_time_us=")?.trim();
        if value.is_empty() || value.eq_ignore_ascii_case("N/A") {
            return None;
        }
        let us: i64 = value.parse().ok()?;
        let d = duration?;
        if !d.is_finite() || d <= 0.0 {
            return None;
        }
        let pct = (us.max(0) as f64) / (d * 1_000_000.0) * 100.0;
        Some(pct.clamp(0.0, 100.0) as f32)
    }

    fn output_duration(&self, req: &JobRequest, probed: Option<f64>) -> Option<f64> {
        match req.op {
            Operation::Convert(FormatId::Gif) => probed.map(|d| d.min(GIF_MAX_SECONDS)),
            Operation::Tool(ToolKind::Trim { start_ms, end_ms }) => probed.map(|d| {
                let from = start_ms as f64 / 1000.0;
                let to = if end_ms == 0 { d } else { d.min(end_ms as f64 / 1000.0) };
                (to - from).max(0.001)
            }),
            _ => probed,
        }
    }

    fn needs_duration(&self) -> bool {
        true
    }

    fn needs_scratch(&self, req: &JobRequest, _input_format: FormatId) -> bool {
        matches!(req.op, Operation::Tool(ToolKind::VideoTargetSize(_)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zest_core::JobId;

    fn run(
        from: FormatId,
        to: FormatId,
        q: Quality,
        privacy: bool,
    ) -> Result<Vec<String>, PlanError> {
        let a = FfmpegAdapter::new(PathBuf::from(r"C:\t\ffmpeg.exe"));
        let mut o = PlanOptions::new(from);
        o.privacy_mode = privacy;
        let req = JobRequest {
            id: JobId(1),
            files: vec![],
            op: Operation::Convert(to),
            quality: q,
            out_dir: None,
            page_range: None,
        };
        let c = a.plan(
            &req,
            Path::new(r"C:\in\a.src"),
            Path::new(r"C:\out\a.x.zest-tmp"),
            &o,
        )?;
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].program, PathBuf::from(r"C:\t\ffmpeg.exe"));
        Ok(c[0]
            .args
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect())
    }

    fn expect(body: &[&str], privacy: bool, muxer: &str) -> Vec<String> {
        let mut v: Vec<String> = [
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-progress",
            "pipe:1",
            "-nostats",
            "-y",
            "-i",
            r"C:\in\a.src",
        ]
        .map(String::from)
        .to_vec();
        v.extend(body.iter().map(|s| s.to_string()));
        if privacy {
            v.extend(["-map_metadata", "-1", "-map_chapters", "-1"].map(String::from));
        }
        v.extend([
            "-f".to_string(),
            muxer.to_string(),
            r"C:\out\a.x.zest-tmp".to_string(),
        ]);
        v
    }

    #[test]
    fn mp4_qualities() {
        for (q, crf, ab) in [
            (Quality::High, "20", "256k"),
            (Quality::Medium, "23", "192k"),
            (Quality::Small, "28", "128k"),
        ] {
            let want = expect(
                &[
                    "-map",
                    "0:v:0?",
                    "-map",
                    "0:a:0?",
                    "-c:v",
                    "libx264",
                    "-crf",
                    crf,
                    "-preset",
                    "medium",
                    "-pix_fmt",
                    "yuv420p",
                    "-vf",
                    EVEN_SAR_SCALE,
                    "-metadata:s:v:0",
                    "rotate=0",
                    "-c:a",
                    "aac",
                    "-b:a",
                    ab,
                    "-movflags",
                    "+faststart",
                ],
                false,
                "mp4",
            );
            assert_eq!(run(FormatId::Mov, FormatId::Mp4, q, false).unwrap(), want);
        }
    }

    #[test]
    fn webm_qualities() {
        for (q, crf, ab) in [
            (Quality::High, "31", "160k"),
            (Quality::Medium, "35", "128k"),
            (Quality::Small, "40", "96k"),
        ] {
            let want = expect(
                &[
                    "-map",
                    "0:v:0?",
                    "-map",
                    "0:a:0?",
                    "-c:v",
                    "libvpx-vp9",
                    "-crf",
                    crf,
                    "-b:v",
                    "0",
                    "-row-mt",
                    "1",
                    "-cpu-used",
                    "2",
                    "-pix_fmt",
                    "yuv420p",
                    "-vf",
                    EVEN_SAR_SCALE,
                    "-metadata:s:v:0",
                    "rotate=0",
                    "-c:a",
                    "libopus",
                    "-b:a",
                    ab,
                ],
                false,
                "webm",
            );
            assert_eq!(run(FormatId::Mp4, FormatId::Webm, q, false).unwrap(), want);
        }
    }

    #[test]
    fn mkv_remux_and_privacy() {
        assert_eq!(
            run(FormatId::Mp4, FormatId::Mkv, Quality::Small, false).unwrap(),
            expect(&["-map", "0", "-c", "copy", "-map_metadata", "0"], false, "matroska")
        );
        assert_eq!(
            run(FormatId::Avi, FormatId::Mkv, Quality::High, true).unwrap(),
            expect(&["-map", "0", "-c", "copy", "-map_metadata", "0"], true, "matroska")
        );
    }

    #[test]
    fn mkv_bakes_display_aspect_when_asked() {
        let a = FfmpegAdapter::new(PathBuf::from(r"C:\t\ffmpeg.exe"));
        let mut o = PlanOptions::new(FormatId::Mp4);
        o.bake_display_aspect = true;
        let req = JobRequest {
            id: JobId(1),
            files: vec![],
            op: Operation::Convert(FormatId::Mkv),
            quality: Quality::Medium,
            out_dir: None,
            page_range: None,
        };
        let c = a
            .plan(
                &req,
                Path::new(r"C:\in\a.mp4"),
                Path::new(r"C:\out\a.x.zest-tmp"),
                &o,
            )
            .unwrap();
        let args: Vec<String> = c[0].args.iter().map(|s| s.to_string_lossy().into_owned()).collect();
        assert!(args.windows(2).any(|w| w[0] == "-c:v" && w[1] == "libx264"));
        assert!(args.windows(2).any(|w| w[0] == "-vf" && w[1] == EVEN_SAR_SCALE));
        assert!(args.windows(2).any(|w| w[0] == "-f" && w[1] == "matroska"));
        assert!(!args.windows(2).any(|w| w[0] == "-c" && w[1] == "copy"));
    }

    #[test]
    fn gif_from_video() {
        let want = expect(
            &[
                "-an",
                "-t",
                "15",
                "-vf",
                "fps=12,scale='min(iw,480)':-1:flags=lanczos,split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=5",
                "-loop",
                "0",
            ],
            false,
            "gif",
        );
        assert_eq!(
            run(FormatId::Mp4, FormatId::Gif, Quality::High, false).unwrap(),
            want
        );
    }

    #[test]
    fn audio_from_video() {
        for (q, v) in [
            (Quality::High, "0"),
            (Quality::Medium, "2"),
            (Quality::Small, "5"),
        ] {
            assert_eq!(
                run(FormatId::Mp4, FormatId::Mp3, q, false).unwrap(),
                expect(
                    &["-vn", "-map", "0:a:0", "-c:a", "libmp3lame", "-q:a", v],
                    false,
                    "mp3"
                )
            );
        }
        assert_eq!(
            run(FormatId::Mkv, FormatId::Wav, Quality::High, false).unwrap(),
            expect(&["-vn", "-map", "0:a:0", "-c:a", "pcm_s16le"], false, "wav")
        );
    }

    #[test]
    fn audio_targets() {
        use FormatId::*;
        assert_eq!(
            run(Flac, Mp3, Quality::Medium, true).unwrap(),
            expect(
                &["-vn", "-map", "0:a:0", "-c:a", "libmp3lame", "-q:a", "2"],
                true,
                "mp3"
            )
        );
        assert_eq!(
            run(Wav, Flac, Quality::High, false).unwrap(),
            expect(
                &["-vn", "-map", "0:a:0", "-c:a", "flac", "-compression_level", "5"],
                false,
                "flac"
            )
        );
        for (q, ab) in [
            (Quality::High, "256k"),
            (Quality::Medium, "192k"),
            (Quality::Small, "128k"),
        ] {
            assert_eq!(
                run(Mp3, M4a, q, false).unwrap(),
                expect(
                    &[
                        "-vn",
                        "-map",
                        "0:a:0",
                        "-c:a",
                        "aac",
                        "-b:a",
                        ab,
                        "-movflags",
                        "+faststart"
                    ],
                    false,
                    "ipod"
                )
            );
        }
        for (q, ab) in [
            (Quality::High, "160k"),
            (Quality::Medium, "128k"),
            (Quality::Small, "96k"),
        ] {
            assert_eq!(
                run(Ogg, Opus, q, false).unwrap(),
                expect(
                    &["-vn", "-map", "0:a:0", "-c:a", "libopus", "-b:a", ab],
                    false,
                    "opus"
                )
            );
        }
        assert_eq!(
            run(M4a, Wav, Quality::High, false).unwrap(),
            expect(&["-vn", "-map", "0:a:0", "-c:a", "pcm_s16le"], false, "wav")
        );
    }

    #[test]
    fn unsupported() {
        use FormatId::*;
        assert!(run(Png, Mp4, Quality::High, false).is_err());
        assert!(run(Mp4, Flac, Quality::High, false).is_err());
        assert!(run(Mp3, Gif, Quality::High, false).is_err());
        assert!(run(Mp3, Ogg, Quality::High, false).is_err());
    }

    #[test]
    fn dash_paths_are_prefixed() {
        let a = FfmpegAdapter::new(PathBuf::from("ffmpeg.exe"));
        let req = JobRequest {
            id: JobId(1),
            files: vec![],
            op: Operation::Convert(FormatId::Wav),
            quality: Quality::High,
            out_dir: None,
            page_range: None,
        };
        let c = a
            .plan(
                &req,
                Path::new("-i.mp3"),
                Path::new("-o"),
                &PlanOptions::new(FormatId::Mp3),
            )
            .unwrap();
        assert!(c[0].args.contains(&OsString::from(".\\-i.mp3")));
        assert_eq!(c[0].args.last().unwrap(), &OsString::from(".\\-o"));
    }

    #[test]
    fn progress_parser() {
        let a = FfmpegAdapter::new(PathBuf::new());
        let d = Some(2.0);
        assert_eq!(a.parse_progress("out_time_us=1000000", d), Some(50.0));
        assert_eq!(a.parse_progress("  out_time_us= 500000 ", d), Some(25.0));
        assert_eq!(a.parse_progress("out_time_us=N/A", d), None);
        assert_eq!(a.parse_progress("out_time_us=", d), None);
        assert_eq!(a.parse_progress("out_time_us=9000000", d), Some(100.0));
        assert_eq!(a.parse_progress("out_time_us=-5", d), Some(0.0));
        assert_eq!(a.parse_progress("out_time_us=1000", None), None);
        assert_eq!(a.parse_progress("out_time_us=1000", Some(0.0)), None);
        assert_eq!(a.parse_progress("out_time_ms=1000000", d), None);
        assert_eq!(a.parse_progress("speed= 1.2x", d), None);
        assert_eq!(a.parse_progress("progress=continue", d), None);
        assert_eq!(a.parse_progress("progress=end", d), Some(100.0));
        assert_eq!(a.parse_progress("progress=end", None), Some(100.0));
        assert_eq!(a.parse_progress(" progress=end\r", None), Some(100.0));
    }

    #[test]
    fn gif_duration_is_capped() {
        let a = FfmpegAdapter::new(PathBuf::new());
        let mut req = JobRequest {
            id: JobId(1),
            files: vec![],
            op: Operation::Convert(FormatId::Gif),
            quality: Quality::High,
            out_dir: None,
            page_range: None,
        };
        assert_eq!(a.output_duration(&req, Some(60.0)), Some(15.0));
        assert_eq!(a.output_duration(&req, Some(3.0)), Some(3.0));
        req.op = Operation::Convert(FormatId::Mp4);
        assert_eq!(a.output_duration(&req, Some(60.0)), Some(60.0));
        assert_eq!(a.output_duration(&req, None), None);
    }

    // ----- tools ------------------------------------------------------------

    fn tool_plan(
        from: FormatId,
        tool: ToolKind,
        duration: Option<f64>,
        privacy: bool,
    ) -> Result<Vec<Vec<String>>, PlanError> {
        let a = FfmpegAdapter::new(PathBuf::from(r"C:\t\ffmpeg.exe"));
        let mut o = PlanOptions::new(from);
        o.privacy_mode = privacy;
        o.duration = duration;
        o.quality = if tool == ToolKind::Compress { Quality::Small } else { Quality::High };
        o.scratch_dir = Some(PathBuf::from(r"C:\s\job1"));
        let req = JobRequest {
            id: JobId(1),
            files: vec![],
            op: Operation::Tool(tool),
            quality: o.quality,
            out_dir: None,
            page_range: None,
        };
        let c = a.plan(
            &req,
            Path::new(r"C:\in\a.src"),
            Path::new(r"C:\out\a.x.zest-tmp"),
            &o,
        )?;
        Ok(c
            .iter()
            .map(|c| c.args.iter().map(|s| s.to_string_lossy().into_owned()).collect())
            .collect())
    }

    #[test]
    fn trim_seeks_before_the_input_and_limits_the_length() {
        let t = ToolKind::Trim { start_ms: 1500, end_ms: 4000 };
        let c = tool_plan(FormatId::Mkv, t, Some(10.0), false).unwrap();
        let a = &c[0];
        let pos = |x: &str| a.iter().position(|s| s == x).unwrap();
        assert_eq!(a[pos("-ss") + 1], "1.500");
        assert!(pos("-ss") < pos("-i"), "input seek");
        assert_eq!(a[pos("-t") + 1], "2.500");
        assert!(pos("-t") > pos("-i"));
        assert_eq!(a[pos("-c:v") + 1], "libx264");
        assert_eq!(a[pos("-crf") + 1], "20", "High quality");
        assert_eq!(a[pos("-f") + 1], "mp4");
        assert_eq!(a.last().unwrap(), r"C:\out\a.x.zest-tmp");
        // End 0 = to the end: no -t.
        let c = tool_plan(FormatId::Mp4, ToolKind::Trim { start_ms: 2000, end_ms: 0 }, None, false).unwrap();
        assert!(!c[0].contains(&"-t".to_owned()));
        assert!(c[0].contains(&"2.000".to_owned()));
        // Audio keeps its format.
        let c = tool_plan(FormatId::Mp3, t, None, false).unwrap();
        let a = &c[0];
        let pos = |x: &str| a.iter().position(|s| s == x).unwrap();
        assert_eq!(a[pos("-c:a") + 1], "libmp3lame");
        assert_eq!(a[pos("-f") + 1], "mp3");
        assert!(pos("-ss") < pos("-i"));
        // Bad ranges.
        assert!(tool_plan(FormatId::Mp4, ToolKind::Trim { start_ms: 5000, end_ms: 4000 }, None, false).is_err());
        assert!(tool_plan(FormatId::Mp4, ToolKind::Trim { start_ms: 20_000, end_ms: 0 }, Some(10.0), false).is_err());
        // Progress covers the kept part only.
        let ad = FfmpegAdapter::new(PathBuf::from("f"));
        let req = JobRequest {
            id: JobId(1),
            files: vec![],
            op: Operation::Tool(t),
            quality: Quality::High,
            out_dir: None,
            page_range: None,
        };
        assert_eq!(ad.output_duration(&req, Some(10.0)), Some(2.5));
        assert_eq!(ad.output_duration(&req, Some(3.0)), Some(1.5));
    }

    #[test]
    fn video_compress_is_crf_28_mp4() {
        let c = tool_plan(FormatId::Mkv, ToolKind::Compress, None, false).unwrap();
        assert_eq!(c.len(), 1);
        let a = &c[0];
        let pos = |x: &str| a.iter().position(|s| s == x).unwrap();
        assert_eq!(a[pos("-crf") + 1], "28");
        assert_eq!(a[pos("-c:v") + 1], "libx264");
        assert_eq!(a[pos("-b:a") + 1], "128k");
        assert_eq!(a[pos("-f") + 1], "mp4");
        assert_eq!(a.last().unwrap(), r"C:\out\a.x.zest-tmp");
    }

    #[test]
    fn audio_compress_lowers_the_bit_rate() {
        use FormatId::*;
        for (f, codec, rate_flag, rate, muxer) in [
            (Mp3, "libmp3lame", "-b:a", "128k", "mp3"),
            (M4a, "aac", "-b:a", "128k", "ipod"),
            (Opus, "libopus", "-b:a", "64k", "opus"),
            (Ogg, "libvorbis", "-q:a", "3", "ogg"),
            (Flac, "flac", "-compression_level", "12", "flac"),
        ] {
            let c = tool_plan(f, ToolKind::Compress, None, false).unwrap();
            let a = &c[0];
            let pos = |x: &str| a.iter().position(|s| s == x).unwrap();
            assert_eq!(a[pos("-c:a") + 1], codec, "{f:?}");
            assert_eq!(a[pos(rate_flag) + 1], rate, "{f:?}");
            assert_eq!(a[pos("-f") + 1], muxer, "{f:?}");
            assert!(a.contains(&"-vn".to_owned()));
        }
        assert!(tool_plan(Wav, ToolKind::Compress, None, false).is_err());
    }

    #[test]
    fn strip_is_a_stream_copy_in_the_same_container() {
        use FormatId::*;
        for (f, muxer) in [
            (Mp4, "mp4"),
            (Mov, "mov"),
            (Mkv, "matroska"),
            (Webm, "webm"),
            (Avi, "avi"),
            (Mp3, "mp3"),
            (Wav, "wav"),
            (Flac, "flac"),
            (M4a, "ipod"),
            (Ogg, "ogg"),
            (Opus, "opus"),
        ] {
            let c = tool_plan(f, ToolKind::StripMetadata, None, false).unwrap();
            assert_eq!(c.len(), 1);
            let a = &c[0];
            let pos = |x: &str| a.iter().position(|s| s == x).unwrap();
            assert_eq!(a[pos("-c") + 1], "copy", "{f:?}");
            assert_eq!(a[pos("-map_metadata") + 1], "-1", "{f:?}");
            assert_eq!(a[pos("-f") + 1], muxer, "{f:?}");
        }
        let c = tool_plan(Mkv, ToolKind::StripMetadata, None, false).unwrap();
        assert!(c[0].contains(&"0:s?".to_owned()));
        let c = tool_plan(Mp4, ToolKind::StripMetadata, None, false).unwrap();
        assert!(!c[0].contains(&"0:s?".to_owned()));
    }

    #[test]
    fn target_size_math() {
        // 10 MB for 60 s: 10*8192*0.96/60 - 128 = 1182.7 kbit/s -> 1280 wide.
        assert_eq!(target_size_rates(10, 60.0).unwrap(), (1182, Some(1280)));
        // 10 MB for 10 s: 7864.3 - 128 = 7736 kbit/s, no width limit.
        assert_eq!(target_size_rates(10, 10.0).unwrap(), (7736, None));
        // 10 MB for 120 s: 655.36 - 128 = 527 -> 854 wide.
        assert_eq!(target_size_rates(10, 120.0).unwrap(), (527, Some(854)));
        // 50 MB for 20 min: 327.68 - 128 = 199 kbit/s.
        assert_eq!(target_size_rates(50, 1200.0).unwrap().0, 199);
        // Too long: under 150 kbit/s video.
        assert_eq!(target_size_rates(10, 300.0), Err(PlanError::TooLong(10)));
        assert!(matches!(target_size_rates(10, 0.0), Err(PlanError::Other(_))));
        assert!(matches!(target_size_rates(10, f64::NAN), Err(PlanError::Other(_))));
        assert_eq!(PlanError::TooLong(25).to_string(), "Too long to fit in 25 MB");
    }

    #[test]
    fn target_size_is_a_two_pass_plan() {
        let c = tool_plan(FormatId::Mov, ToolKind::VideoTargetSize(10), Some(60.0), false).unwrap();
        assert_eq!(c.len(), 2);
        let (p1, p2) = (&c[0], &c[1]);
        let pos = |a: &Vec<String>, x: &str| a.iter().position(|s| s == x).unwrap();
        for a in [p1, p2] {
            assert_eq!(a[pos(a, "-b:v") + 1], "1182k");
            assert_eq!(a[pos(a, "-c:v") + 1], "libx264");
            assert_eq!(a[pos(a, "-passlogfile") + 1], r"C:\s\job1\pass");
            assert_eq!(a[pos(a, "-vf") + 1], max_width_sar_scale(1280));
            assert!(a.contains(&"-progress".to_owned()));
        }
        assert_eq!(p1[pos(p1, "-pass") + 1], "1");
        assert!(p1.contains(&"-an".to_owned()));
        assert_eq!(&p1[p1.len() - 3..], ["-f", "null", "NUL"]);
        assert_eq!(p2[pos(p2, "-pass") + 1], "2");
        assert_eq!(p2[pos(p2, "-c:a") + 1], "aac");
        assert_eq!(p2[pos(p2, "-b:a") + 1], "128k");
        assert_eq!(&p2[p2.len() - 3..], ["-f", "mp4", r"C:\out\a.x.zest-tmp"]);
        // Rich bit rate: no scale limit, SAR-aware even sizes.
        let c = tool_plan(FormatId::Mp4, ToolKind::VideoTargetSize(50), Some(30.0), false).unwrap();
        assert_eq!(c[0][pos(&c[0], "-vf") + 1], EVEN_SAR_SCALE);
        // 854 limit.
        let c = tool_plan(FormatId::Mp4, ToolKind::VideoTargetSize(10), Some(120.0), false).unwrap();
        assert_eq!(c[1][pos(&c[1], "-vf") + 1], max_width_sar_scale(854));
    }

    #[test]
    fn target_size_errors() {
        assert!(matches!(
            tool_plan(FormatId::Mp4, ToolKind::VideoTargetSize(10), Some(600.0), false),
            Err(PlanError::TooLong(10))
        ));
        assert!(matches!(
            tool_plan(FormatId::Mp4, ToolKind::VideoTargetSize(10), None, false),
            Err(PlanError::Other(_))
        ));
        assert!(matches!(
            tool_plan(FormatId::Mp3, ToolKind::VideoTargetSize(10), Some(10.0), false),
            Err(PlanError::ToolUnsupported { .. })
        ));
        // Image tools are not for ffmpeg.
        assert!(tool_plan(FormatId::Mp4, ToolKind::Resize50, None, false).is_err());
    }

    #[test]
    fn privacy_mode_applies_to_tool_output() {
        let c = tool_plan(FormatId::Mp3, ToolKind::Compress, None, true).unwrap();
        assert!(c[0].windows(2).any(|w| w == ["-map_metadata", "-1"]));
        let c = tool_plan(FormatId::Mp4, ToolKind::VideoTargetSize(25), Some(30.0), true).unwrap();
        assert!(!c[0].contains(&"-map_metadata".to_owned()));
        assert!(c[1].windows(2).any(|w| w == ["-map_metadata", "-1"]));
    }

    #[test]
    fn scratch_only_for_target_size() {
        let a = FfmpegAdapter::new(PathBuf::new());
        let r = |op| JobRequest {
            id: JobId(1),
            files: vec![],
            op,
            quality: Quality::High,
            out_dir: None,
            page_range: None,
        };
        assert!(a.needs_scratch(&r(Operation::Tool(ToolKind::VideoTargetSize(10))), FormatId::Mp4));
        assert!(!a.needs_scratch(&r(Operation::Tool(ToolKind::Compress)), FormatId::Mp4));
        assert!(!a.needs_scratch(&r(Operation::Convert(FormatId::Mkv)), FormatId::Mp4));
    }
}
