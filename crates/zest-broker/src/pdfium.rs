//! PDF jobs: command lines for `zest-worker` (PDFium) and its progress lines.
//!
//! The worker is started like vips and FFmpeg, inside the same Job Object limits.
//! It is not an [`crate::Adapter`]: its output is a folder (page images) or a file
//! built from several inputs (merge), so the pipeline runs it with its own flow.

use crate::adapter::{os, q3, safe_arg, CommandSpec, PlanError};
use std::ffi::OsString;
use std::path::Path;
use zest_core::{FormatId, Quality};

/// Resolution of the page images.
pub const PAGE_DPI: u32 = 150;

/// `zest-worker --pdfium <dll> pdf-to-images --format png|jpg --dpi 150 --quality N <in> <out_dir>`
pub fn pages_command(
    worker: &Path,
    dll: &Path,
    format: FormatId,
    quality: Quality,
    input: &Path,
    out_dir: &Path,
) -> Result<CommandSpec, PlanError> {
    pages_command_range(worker, dll, format, quality, input, out_dir, None)
}

/// Like [`pages_command`], with optional `--pages 1-3` (1-based inclusive).
pub fn pages_command_range(
    worker: &Path,
    dll: &Path,
    format: FormatId,
    quality: Quality,
    input: &Path,
    out_dir: &Path,
    pages: Option<&str>,
) -> Result<CommandSpec, PlanError> {
    let fmt = match format {
        FormatId::Png => "png",
        FormatId::Jpeg => "jpg",
        other => {
            return Err(PlanError::Unsupported {
                from: FormatId::Pdf,
                to: other,
            })
        }
    };
    let jpeg_q = q3(quality, 92, 82, 70);
    let mut args: Vec<OsString> = vec![
        os("--pdfium"),
        dll.as_os_str().to_os_string(),
        os("pdf-to-images"),
        os("--format"),
        os(fmt),
        os("--dpi"),
        os(&PAGE_DPI.to_string()),
        os("--quality"),
        os(&jpeg_q.to_string()),
    ];
    if let Some(p) = pages.filter(|s| !s.is_empty()) {
        args.push(os("--pages"));
        args.push(os(p));
    }
    args.push(safe_arg(input)?);
    args.push(safe_arg(out_dir)?);
    Ok(CommandSpec {
        program: worker.to_path_buf(),
        args,
    })
}

/// Split one PDF into per-page PDFs in `out_dir`.
pub fn split_command(worker: &Path, dll: &Path, input: &Path, out_dir: &Path) -> Result<CommandSpec, PlanError> {
    Ok(CommandSpec {
        program: worker.to_path_buf(),
        args: vec![
            os("--pdfium"),
            dll.as_os_str().to_os_string(),
            os("split-pdf"),
            safe_arg(input)?,
            safe_arg(out_dir)?,
        ],
    })
}

/// Re-save a PDF (best-effort compress).
pub fn compress_pdf_command(worker: &Path, dll: &Path, input: &Path, out: &Path) -> Result<CommandSpec, PlanError> {
    Ok(CommandSpec {
        program: worker.to_path_buf(),
        args: vec![
            os("--pdfium"),
            dll.as_os_str().to_os_string(),
            os("compress-pdf"),
            safe_arg(input)?,
            safe_arg(out)?,
        ],
    })
}

/// `zest-worker --pdfium <dll> merge-pdf <out> <in1> <in2> ...` (inputs in the given order).
pub fn merge_command(
    worker: &Path,
    dll: &Path,
    out: &Path,
    inputs: &[std::path::PathBuf],
) -> Result<CommandSpec, PlanError> {
    let mut args: Vec<OsString> = vec![
        os("--pdfium"),
        dll.as_os_str().to_os_string(),
        os("merge-pdf"),
        safe_arg(out)?,
    ];
    for i in inputs {
        args.push(safe_arg(i)?);
    }
    Ok(CommandSpec {
        program: worker.to_path_buf(),
        args,
    })
}

/// `progress 42.5` -> 42.5
pub fn parse_progress(line: &str) -> Option<f32> {
    let v: f32 = line.trim().strip_prefix("progress ")?.trim().parse().ok()?;
    v.is_finite().then(|| v.clamp(0.0, 100.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn pages_command_line() {
        let c = pages_command(
            Path::new(r"C:\z\zest-worker.exe"),
            Path::new(r"C:\z\pdfium.dll"),
            FormatId::Jpeg,
            Quality::Medium,
            Path::new(r"C:\d\a.pdf"),
            Path::new(r"C:\d\a (pages).zest-tmp"),
        )
        .unwrap();
        let args: Vec<String> = c.args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(
            args,
            [
                "--pdfium",
                r"C:\z\pdfium.dll",
                "pdf-to-images",
                "--format",
                "jpg",
                "--dpi",
                "150",
                "--quality",
                "82",
                r"C:\d\a.pdf",
                r"C:\d\a (pages).zest-tmp",
            ]
        );
        assert!(pages_command(Path::new("w"), Path::new("d"), FormatId::Webp, Quality::High, Path::new("a"), Path::new("b")).is_err());
    }

    #[test]
    fn merge_command_keeps_order() {
        let ins = [PathBuf::from(r"C:\b.pdf"), PathBuf::from(r"C:\a.pdf"), PathBuf::from("-c.pdf")];
        let c = merge_command(Path::new("w.exe"), Path::new("d.dll"), Path::new(r"C:\o.zest-tmp"), &ins).unwrap();
        let args: Vec<String> = c.args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(
            args,
            ["--pdfium", "d.dll", "merge-pdf", r"C:\o.zest-tmp", r"C:\b.pdf", r"C:\a.pdf", r".\-c.pdf"]
        );
    }

    #[test]
    fn progress_lines() {
        assert_eq!(parse_progress("progress 42.5"), Some(42.5));
        assert_eq!(parse_progress("progress 100.0\r"), Some(100.0));
        assert_eq!(parse_progress("progress 250"), Some(100.0));
        assert_eq!(parse_progress("progress x"), None);
        assert_eq!(parse_progress("done"), None);
    }
}
