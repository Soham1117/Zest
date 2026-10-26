//! Helpers for the Crop and Trim windows: a small picture of an image, the
//! length of a media file and a still frame of a video. They start the same
//! tools as the jobs (libvips, FFmpeg) and never change the source file.

use crate::adapter::CommandSpec;
use crate::process::run_capture;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;
use zest_core::FormatId;

/// Longest edge of the crop preview picture.
pub const PREVIEW_EDGE: u32 = 1600;
/// Longest edge of the wheel hover preview chip.
pub const HOVER_PREVIEW_EDGE: u32 = 160;
const TOOL_TIMEOUT: Duration = Duration::from_secs(60);

/// A preview picture of an image and the size of the whole image as the Crop job sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CropPreview {
    /// PNG, at most [`PREVIEW_EDGE`] on the long side, already turned by the EXIF orientation.
    pub png: PathBuf,
    /// Size of the whole image after the turn.
    pub width: u32,
    pub height: u32,
}

/// Formats where the Crop job applies the EXIF orientation first (see the libvips adapter).
fn turns_by_exif(f: FormatId) -> bool {
    matches!(f, FormatId::Jpeg | FormatId::Heic | FormatId::Heif | FormatId::Avif)
}

fn header_field(vips: &Path, src: &Path, field: &str) -> Option<i64> {
    let header = vips.with_file_name("vipsheader.exe");
    let args: Vec<OsString> = vec!["-f".into(), field.into(), src.as_os_str().to_owned()];
    let out = run_capture(&CommandSpec { program: header, args }, TOOL_TIMEOUT)?;
    out.trim().parse().ok()
}

/// Make the preview picture for `src` in `out_dir` (the file name is chosen here).
pub fn crop_preview(vips: &Path, src: &Path, out_dir: &Path) -> Result<CropPreview, String> {
    let ext = src.extension().and_then(|e| FormatId::from_extension(&e.to_string_lossy()));
    let fmt = ext.ok_or("This file type cannot be cropped")?;
    let (w, h) = (
        header_field(vips, src, "width").ok_or("Cannot read the image size")?,
        header_field(vips, src, "height").ok_or("Cannot read the image size")?,
    );
    let orientation = if turns_by_exif(fmt) { header_field(vips, src, "orientation").unwrap_or(1) } else { 1 };
    // EXIF orientations 5 to 8 turn the picture by 90 degrees.
    let (w, h) = if (5..=8).contains(&orientation) { (h, w) } else { (w, h) };
    let (w, h) = (u32::try_from(w).map_err(|_| "Bad image size")?, u32::try_from(h).map_err(|_| "Bad image size")?);
    if w == 0 || h == 0 {
        return Err("The image is empty".to_owned());
    }

    std::fs::create_dir_all(out_dir).map_err(|e| format!("Cannot make the preview: {e}"))?;
    let png = out_dir.join(format!("crop-preview-{}-{}.png", std::process::id(), nonce()));
    // `thumbnail` turns by EXIF by default; the formats that the job does not turn stay as they are.
    let mut args: Vec<OsString> = vec![
        "thumbnail".into(),
        src.as_os_str().to_owned(),
        png.as_os_str().to_owned(),
        PREVIEW_EDGE.to_string().into(),
        "--height".into(),
        PREVIEW_EDGE.to_string().into(),
        "--size".into(),
        "down".into(),
    ];
    if !turns_by_exif(fmt) {
        args.push("--no-rotate".into());
    }
    let cmd = CommandSpec { program: vips.to_path_buf(), args };
    run_capture(&cmd, TOOL_TIMEOUT).ok_or("Cannot make the preview picture")?;
    if !png.is_file() {
        return Err("Cannot make the preview picture".to_owned());
    }
    Ok(CropPreview { png, width: w, height: h })
}

fn nonce() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

/// Small PNG thumbnail for the wheel hover chip (images only).
pub fn hover_thumbnail(vips: &Path, src: &Path, out_dir: &Path) -> Result<PathBuf, String> {
    let ext = src.extension().and_then(|e| FormatId::from_extension(&e.to_string_lossy()));
    let fmt = ext.ok_or("Not an image")?;
    if !matches!(
        fmt,
        FormatId::Jpeg
            | FormatId::Png
            | FormatId::Webp
            | FormatId::Bmp
            | FormatId::Tiff
            | FormatId::Gif
            | FormatId::Heic
            | FormatId::Heif
            | FormatId::Avif
    ) {
        return Err("Not an image".into());
    }
    std::fs::create_dir_all(out_dir).map_err(|e| format!("Cannot make the preview: {e}"))?;
    let png = out_dir.join(format!("hover-{}-{}.png", std::process::id(), nonce()));
    let mut args: Vec<OsString> = vec![
        "thumbnail".into(),
        src.as_os_str().to_owned(),
        png.as_os_str().to_owned(),
        HOVER_PREVIEW_EDGE.to_string().into(),
        "--height".into(),
        HOVER_PREVIEW_EDGE.to_string().into(),
        "--size".into(),
        "down".into(),
    ];
    if !turns_by_exif(fmt) {
        args.push("--no-rotate".into());
    }
    let cmd = CommandSpec { program: vips.to_path_buf(), args };
    run_capture(&cmd, Duration::from_secs(15)).ok_or("Cannot make the preview picture")?;
    if !png.is_file() {
        return Err("Cannot make the preview picture".to_owned());
    }
    Ok(png)
}

/// One-line media summary for hover (duration and/or file size). No frame decode.
pub fn media_hover_text(ffprobe: &Path, src: &Path) -> Option<String> {
    let dur = duration_ms(ffprobe, src).map(|ms| {
        let s = ms / 1000;
        format!("{}:{:02}", s / 60, s % 60)
    });
    let size = std::fs::metadata(src).ok().map(|m| {
        let b = m.len() as f64;
        if b >= 1024.0 * 1024.0 {
            format!("{:.1} MB", b / (1024.0 * 1024.0))
        } else if b >= 1024.0 {
            format!("{:.0} KB", b / 1024.0)
        } else {
            format!("{b} B")
        }
    });
    match (dur, size) {
        (Some(d), Some(sz)) => Some(format!("{d} · {sz}")),
        (Some(d), None) => Some(d),
        (None, Some(sz)) => Some(sz),
        (None, None) => None,
    }
}

/// Length of a media file in milliseconds.
pub fn duration_ms(ffprobe: &Path, src: &Path) -> Option<u64> {
    let args: Vec<OsString> = vec![
        "-v".into(),
        "error".into(),
        "-show_entries".into(),
        "format=duration".into(),
        "-of".into(),
        "default=nw=1:nk=1".into(),
        src.as_os_str().to_owned(),
    ];
    let out = run_capture(&CommandSpec { program: ffprobe.to_path_buf(), args }, TOOL_TIMEOUT)?;
    let secs: f64 = out.trim().parse().ok()?;
    (secs.is_finite() && secs > 0.0).then(|| (secs * 1000.0).round() as u64)
}

/// Write the frame at `t_ms` as a small PNG (at most 480 px wide) to `out_dir`. `None` on failure.
pub fn video_frame(ffmpeg: &Path, src: &Path, t_ms: u64, out_dir: &Path) -> Option<PathBuf> {
    std::fs::create_dir_all(out_dir).ok()?;
    let png = out_dir.join(format!("trim-frame-{}-{}.png", std::process::id(), nonce()));
    let secs = format!("{}.{:03}", t_ms / 1000, t_ms % 1000);
    let args: Vec<OsString> = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-loglevel".into(),
        "error".into(),
        "-y".into(),
        "-ss".into(),
        secs.into(),
        "-i".into(),
        src.as_os_str().to_owned(),
        "-frames:v".into(),
        "1".into(),
        "-vf".into(),
        "scale='min(480,iw)':-2".into(),
        png.as_os_str().to_owned(),
    ];
    run_capture(&CommandSpec { program: ffmpeg.to_path_buf(), args }, TOOL_TIMEOUT)?;
    png.is_file().then_some(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exif_turn_applies_to_the_formats_the_job_turns() {
        assert!(turns_by_exif(FormatId::Jpeg));
        assert!(turns_by_exif(FormatId::Avif));
        assert!(!turns_by_exif(FormatId::Png));
        assert!(!turns_by_exif(FormatId::Webp));
    }

    #[test]
    fn missing_tools_give_errors_not_panics() {
        let nowhere = Path::new(r"C:\no\such\vips.exe");
        let r = crop_preview(nowhere, Path::new(r"C:\no\a.jpg"), &std::env::temp_dir());
        assert!(r.is_err());
        assert!(crop_preview(nowhere, Path::new(r"C:\no\a.xyz"), &std::env::temp_dir()).is_err());
        assert_eq!(duration_ms(Path::new(r"C:\no\ffprobe.exe"), Path::new("x.mp4")), None);
        assert_eq!(video_frame(Path::new(r"C:\no\ffmpeg.exe"), Path::new("x.mp4"), 1000, &std::env::temp_dir()), None);
    }
}
