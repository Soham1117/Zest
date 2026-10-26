//! libvips adapter. Uses the saver operation by name (`jpegsave`, ...), so the
//! output file name (`*.zest-tmp`) does not matter. Verified with libvips 8.18.7.
//!
//! Tools that change pixels (resize, rotate) need two commands, because the
//! `vips` command line picks the writer from the file name and the temp name
//! has no image suffix: first the operation writes a scratch `.v` file, then
//! the saver writes the real output.

use crate::adapter::{os, q3, safe_arg, Adapter, CommandSpec, PlanError, PlanOptions};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use zest_core::{EngineId, FormatId, JobRequest, Operation, Quality, ToolKind};

pub struct VipsAdapter {
    program: PathBuf,
}

impl VipsAdapter {
    pub fn new(program: PathBuf) -> Self {
        VipsAdapter { program }
    }
}

fn jpeg_q(q: Quality) -> u32 {
    q3(q, 92, 82, 70)
}
fn webp_q(q: Quality) -> u32 {
    q3(q, 90, 80, 65)
}
fn avif_q(q: Quality) -> u32 {
    q3(q, 60, 50, 40)
}

/// Loaders that can apply the EXIF orientation themselves.
fn supports_autorotate(f: FormatId) -> bool {
    matches!(
        f,
        FormatId::Jpeg | FormatId::Heic | FormatId::Heif | FormatId::Avif
    )
}

/// Name of the scratch file inside the scratch folder.
const SCRATCH_NAME: &str = "step.v";

/// Saver name and options for writing `to`. `compress` asks for the smallest
/// lossless variant where one exists (PNG, WebP effort).
fn saver(to: FormatId, q: Quality, compress: bool) -> Option<(&'static str, Vec<String>)> {
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<String>>();
    Some(match to {
        FormatId::Png if compress => (
            "pngsave",
            v(&["--compression", "9", "--effort", "10", "--filter", "all"]),
        ),
        FormatId::Png => ("pngsave", v(&["--compression", "6"])),
        FormatId::Jpeg => {
            let mut a = v(&["--Q", &jpeg_q(q).to_string(), "--background", "255"]);
            if compress {
                a.push("--optimize-coding".into());
            }
            ("jpegsave", a)
        }
        FormatId::Webp => (
            "webpsave",
            v(&["--Q", &webp_q(q).to_string(), "--effort", if compress { "6" } else { "4" }]),
        ),
        FormatId::Avif => (
            "heifsave",
            v(&["--Q", &avif_q(q).to_string(), "--compression", "av1", "--effort", "4"]),
        ),
        FormatId::Tiff => ("tiffsave", v(&["--compression", "deflate"])),
        _ => return None,
    })
}

impl VipsAdapter {
    fn threads(&self, opts: &PlanOptions) -> Option<OsString> {
        opts.vips_threads
            .map(|n| OsString::from(format!("--vips-concurrency={n}")))
    }

    fn cmd(&self, opts: &PlanOptions, args: Vec<OsString>) -> CommandSpec {
        let mut all: Vec<OsString> = self.threads(opts).into_iter().collect();
        all.extend(args);
        CommandSpec {
            program: self.program.clone(),
            args: all,
        }
    }

    fn plan_convert(
        &self,
        to: FormatId,
        q: Quality,
        input: &Path,
        tmp_out: &Path,
        opts: &PlanOptions,
    ) -> Result<Vec<CommandSpec>, PlanError> {
        let from = opts.input_format;
        let Some((saver, mut extra)) = saver(to, q, false) else {
            return Err(PlanError::Unsupported { from, to });
        };
        if opts.privacy_mode {
            extra.push("--keep".into());
            extra.push("none".into());
        }
        let mut args: Vec<OsString> = vec![os(saver)];
        let mut inp = safe_arg(input)?;
        if supports_autorotate(from) {
            inp.push("[autorotate]");
        }
        args.push(inp);
        args.push(safe_arg(tmp_out)?);
        args.extend(extra.into_iter().map(OsString::from));
        Ok(vec![self.cmd(opts, args)])
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
        if !tool.applies_to(from) || tool.engine(from) != EngineId::Vips {
            return Err(unsupported);
        }
        // Quality: Compress uses the job quality (wheel High/Medium/Small); else High.
        let (q, compress) = match tool {
            ToolKind::Compress => (opts.quality, true),
            _ => (Quality::High, false),
        };
        let (saver_name, mut extra) = saver(from, q, compress).ok_or(unsupported.clone())?;
        // Metadata: Strip keeps only the color profile. Privacy mode strips everything.
        if opts.privacy_mode {
            extra.extend(["--keep".to_owned(), "none".to_owned()]);
        } else if tool == ToolKind::StripMetadata {
            extra.extend(["--keep".to_owned(), "icc".to_owned()]);
        }

        let mut inp = safe_arg(input)?;
        // Rotate and Strip must apply the EXIF orientation first, or the result would be turned.
        let autorotate = matches!(
            tool,
            ToolKind::RotateCw
                | ToolKind::RotateCcw
                | ToolKind::Rotate180
                | ToolKind::StripMetadata
                | ToolKind::Crop { .. }
        ) && supports_autorotate(from);
        if autorotate {
            inp.push("[autorotate]");
        }

        let save = |src: OsString| -> Result<CommandSpec, PlanError> {
            let mut args: Vec<OsString> = vec![os(saver_name), src, safe_arg(tmp_out)?];
            args.extend(extra.iter().cloned().map(OsString::from));
            Ok(self.cmd(opts, args))
        };

        // Compress and Strip need only the saver.
        if matches!(tool, ToolKind::Compress | ToolKind::StripMetadata) {
            return Ok(vec![save(inp)?]);
        }

        // The others write a scratch file first.
        let scratch = opts
            .scratch_dir
            .as_ref()
            .ok_or_else(|| PlanError::Other("no scratch folder".to_owned()))?
            .join(SCRATCH_NAME);
        let scratch_os = scratch.into_os_string();
        let first: Vec<OsString> = match tool {
            ToolKind::Resize50 | ToolKind::Resize25 => {
                let scale = if tool == ToolKind::Resize50 { "0.5" } else { "0.25" };
                vec![os("resize"), inp, scratch_os.clone(), os(scale)]
            }
            ToolKind::MaxEdge1920 | ToolKind::MaxEdge1280 => {
                let edge = if tool == ToolKind::MaxEdge1920 { "1920" } else { "1280" };
                // thumbnail reads the file itself (and applies the EXIF orientation).
                let plain = safe_arg(input)?;
                vec![
                    os("thumbnail"),
                    plain,
                    scratch_os.clone(),
                    os(edge),
                    os("--height"),
                    os(edge),
                    os("--size"),
                    os("down"),
                ]
            }
            ToolKind::Crop { x, y, w, h } => {
                if w == 0 || h == 0 {
                    return Err(PlanError::Other("The crop area is empty".to_owned()));
                }
                // The rectangle is in the pixels of the turned image (autorotate above).
                let n = |v: u32| os(&v.to_string());
                vec![os("crop"), inp, scratch_os.clone(), n(x), n(y), n(w), n(h)]
            }
            ToolKind::RotateCw | ToolKind::RotateCcw | ToolKind::Rotate180 => {
                let angle = match tool {
                    ToolKind::RotateCw => "d90",
                    ToolKind::RotateCcw => "d270",
                    _ => "d180",
                };
                vec![os("rot"), inp, scratch_os.clone(), os(angle)]
            }
            _ => return Err(unsupported),
        };
        Ok(vec![self.cmd(opts, first), save(scratch_os)?])
    }
}

impl Adapter for VipsAdapter {
    fn engine(&self) -> EngineId {
        EngineId::Vips
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
            Operation::Action(_) => Err(PlanError::Other("actions are not run by libvips".to_owned())),
        }
    }

    fn parse_progress(&self, _line: &str, _duration: Option<f64>) -> Option<f32> {
        None
    }

    fn needs_scratch(&self, req: &JobRequest, _input_format: FormatId) -> bool {
        matches!(
            req.op,
            Operation::Tool(
                ToolKind::Resize50
                    | ToolKind::Resize25
                    | ToolKind::MaxEdge1920
                    | ToolKind::MaxEdge1280
                    | ToolKind::RotateCw
                    | ToolKind::RotateCcw
                    | ToolKind::Rotate180
                    | ToolKind::Crop { .. }
            )
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zest_core::JobId;

    fn req(op: Operation, quality: Quality) -> JobRequest {
        JobRequest {
            id: JobId(1),
            files: vec![],
            op,
            quality,
            out_dir: None,
            page_range: None,
        }
    }

    fn strings(cmds: &[CommandSpec]) -> Vec<Vec<String>> {
        cmds.iter()
            .map(|c| c.args.iter().map(|a| a.to_string_lossy().into_owned()).collect())
            .collect()
    }

    fn plan(from: FormatId, to: FormatId, q: Quality, privacy: bool) -> Vec<String> {
        let a = VipsAdapter::new(PathBuf::from(r"C:\t\vips.exe"));
        let mut o = PlanOptions::new(from);
        o.privacy_mode = privacy;
        let cmds = a
            .plan(
                &req(Operation::Convert(to), q),
                Path::new(r"C:\in\a.png"),
                Path::new(r"C:\out\a.jpg.zest-tmp"),
                &o,
            )
            .unwrap();
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].program, PathBuf::from(r"C:\t\vips.exe"));
        strings(&cmds).remove(0)
    }

    fn plan_tool(from: FormatId, tool: ToolKind, privacy: bool) -> Result<Vec<Vec<String>>, PlanError> {
        let a = VipsAdapter::new(PathBuf::from(r"C:\t\vips.exe"));
        let mut o = PlanOptions::new(from);
        o.quality = if tool == ToolKind::Compress { Quality::Medium } else { Quality::Small };
        o.privacy_mode = privacy;
        o.scratch_dir = Some(PathBuf::from(r"C:\s\job1"));
        let cmds = a.plan(
            &req(Operation::Tool(tool), o.quality),
            Path::new(r"C:\in\a.src"),
            Path::new(r"C:\out\a.src.zest-tmp"),
            &o,
        )?;
        Ok(strings(&cmds))
    }

    #[test]
    fn crop_runs_after_autorotate() {
        let crop = ToolKind::Crop { x: 10, y: 20, w: 300, h: 200 };
        let c = plan_tool(FormatId::Jpeg, crop, false).unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(
            c[0],
            [
                "--vips-concurrency=4",
                "crop",
                r"C:\in\a.src[autorotate]",
                r"C:\s\job1\step.v",
                "10",
                "20",
                "300",
                "200"
            ]
        );
        assert_eq!(c[1][1], "jpegsave");
        assert_eq!(c[1][2], r"C:\s\job1\step.v");
        // PNG has no orientation: plain input.
        let c = plan_tool(FormatId::Png, crop, false).unwrap();
        assert_eq!(c[0][2], r"C:\in\a.src");
        assert_eq!(c[1][1], "pngsave");
        assert!(plan_tool(FormatId::Png, ToolKind::Crop { x: 0, y: 0, w: 0, h: 5 }, false).is_err());
        assert!(plan_tool(FormatId::Heic, crop, false).is_err());
        let a = VipsAdapter::new(PathBuf::from("v"));
        assert!(a.needs_scratch(&req(Operation::Tool(crop), Quality::High), FormatId::Png));
    }

    #[test]
    fn jpeg_all_qualities() {
        let tail = [r"C:\in\a.png", r"C:\out\a.jpg.zest-tmp"];
        for (q, n) in [(Quality::High, "92"), (Quality::Medium, "82"), (Quality::Small, "70")] {
            let mut want: Vec<String> = vec!["--vips-concurrency=4".into(), "jpegsave".into()];
            want.extend(tail.iter().map(|s| s.to_string()));
            want.extend(["--Q", n, "--background", "255"].map(String::from));
            assert_eq!(plan(FormatId::Png, FormatId::Jpeg, q, false), want);
        }
    }

    #[test]
    fn webp_all_qualities() {
        for (q, n) in [(Quality::High, "90"), (Quality::Medium, "80"), (Quality::Small, "65")] {
            assert_eq!(
                plan(FormatId::Png, FormatId::Webp, q, false),
                [
                    "--vips-concurrency=4",
                    "webpsave",
                    r"C:\in\a.png",
                    r"C:\out\a.jpg.zest-tmp",
                    "--Q",
                    n,
                    "--effort",
                    "4"
                ]
            );
        }
    }

    #[test]
    fn avif_all_qualities() {
        for (q, n) in [(Quality::High, "60"), (Quality::Medium, "50"), (Quality::Small, "40")] {
            assert_eq!(
                plan(FormatId::Webp, FormatId::Avif, q, false),
                [
                    "--vips-concurrency=4",
                    "heifsave",
                    r"C:\in\a.png",
                    r"C:\out\a.jpg.zest-tmp",
                    "--Q",
                    n,
                    "--compression",
                    "av1",
                    "--effort",
                    "4"
                ]
            );
        }
    }

    #[test]
    fn png_and_tiff() {
        assert_eq!(
            plan(FormatId::Jpeg, FormatId::Png, Quality::Small, false),
            [
                "--vips-concurrency=4",
                "pngsave",
                r"C:\in\a.png[autorotate]",
                r"C:\out\a.jpg.zest-tmp",
                "--compression",
                "6"
            ]
        );
        assert_eq!(
            plan(FormatId::Bmp, FormatId::Tiff, Quality::High, false),
            [
                "--vips-concurrency=4",
                "tiffsave",
                r"C:\in\a.png",
                r"C:\out\a.jpg.zest-tmp",
                "--compression",
                "deflate"
            ]
        );
    }

    #[test]
    fn privacy_strips_metadata() {
        let a = plan(FormatId::Heic, FormatId::Jpeg, Quality::High, true);
        assert_eq!(&a[a.len() - 2..], ["--keep", "none"]);
        assert_eq!(a[2], r"C:\in\a.png[autorotate]");
    }

    #[test]
    fn unsupported_target_and_dash_path() {
        let a = VipsAdapter::new(PathBuf::from("vips.exe"));
        let e = a.plan(
            &req(Operation::Convert(FormatId::Gif), Quality::High),
            Path::new("a.png"),
            Path::new("b"),
            &PlanOptions::new(FormatId::Png),
        );
        assert!(matches!(e, Err(PlanError::Unsupported { .. })));
        let c = a
            .plan(
                &req(Operation::Convert(FormatId::Png), Quality::High),
                Path::new("-rf.jpg"),
                Path::new("b"),
                &PlanOptions::new(FormatId::Bmp),
            )
            .unwrap();
        assert_eq!(c[0].args[2], OsString::from(r".\-rf.jpg"));
    }

    #[test]
    fn no_progress() {
        let a = VipsAdapter::new(PathBuf::new());
        assert_eq!(a.parse_progress("anything", Some(1.0)), None);
    }

    // ----- tools ------------------------------------------------------------

    #[test]
    fn compress_uses_medium_quality_and_one_command() {
        let c = plan_tool(FormatId::Jpeg, ToolKind::Compress, false).unwrap();
        assert_eq!(
            c,
            [[
                "--vips-concurrency=4",
                "jpegsave",
                r"C:\in\a.src",
                r"C:\out\a.src.zest-tmp",
                "--Q",
                "82",
                "--background",
                "255",
                "--optimize-coding"
            ]]
        );
        let c = plan_tool(FormatId::Png, ToolKind::Compress, false).unwrap();
        assert_eq!(
            c[0][1..],
            [
                "pngsave",
                r"C:\in\a.src",
                r"C:\out\a.src.zest-tmp",
                "--compression",
                "9",
                "--effort",
                "10",
                "--filter",
                "all"
            ]
        );
        let c = plan_tool(FormatId::Webp, ToolKind::Compress, false).unwrap();
        assert_eq!(&c[0][1..2], ["webpsave"]);
        assert!(c[0].contains(&"80".to_owned()));
        let c = plan_tool(FormatId::Avif, ToolKind::Compress, false).unwrap();
        assert_eq!(&c[0][1..2], ["heifsave"]);
        assert!(c[0].contains(&"50".to_owned()));
    }

    #[test]
    fn resize_writes_scratch_then_saves() {
        let c = plan_tool(FormatId::Jpeg, ToolKind::Resize50, false).unwrap();
        assert_eq!(c.len(), 2);
        assert_eq!(
            c[0],
            ["--vips-concurrency=4", "resize", r"C:\in\a.src", r"C:\s\job1\step.v", "0.5"]
        );
        assert_eq!(
            c[1],
            [
                "--vips-concurrency=4",
                "jpegsave",
                r"C:\s\job1\step.v",
                r"C:\out\a.src.zest-tmp",
                "--Q",
                "92",
                "--background",
                "255"
            ]
        );
        let c = plan_tool(FormatId::Png, ToolKind::Resize25, false).unwrap();
        assert_eq!(c[0][4], "0.25");
        assert_eq!(c[1][1], "pngsave");
    }

    #[test]
    fn max_edge_uses_thumbnail_down_only() {
        let c = plan_tool(FormatId::Png, ToolKind::MaxEdge1920, false).unwrap();
        assert_eq!(
            c[0],
            [
                "--vips-concurrency=4",
                "thumbnail",
                r"C:\in\a.src",
                r"C:\s\job1\step.v",
                "1920",
                "--height",
                "1920",
                "--size",
                "down"
            ]
        );
        let c = plan_tool(FormatId::Webp, ToolKind::MaxEdge1280, false).unwrap();
        assert_eq!(&c[0][4..], ["1280", "--height", "1280", "--size", "down"]);
        assert_eq!(c[1][1], "webpsave");
    }

    #[test]
    fn rotate_angles_and_autorotate() {
        for (tool, angle) in [
            (ToolKind::RotateCw, "d90"),
            (ToolKind::RotateCcw, "d270"),
            (ToolKind::Rotate180, "d180"),
        ] {
            let c = plan_tool(FormatId::Jpeg, tool, false).unwrap();
            assert_eq!(
                c[0],
                [
                    "--vips-concurrency=4",
                    "rot",
                    r"C:\in\a.src[autorotate]",
                    r"C:\s\job1\step.v",
                    angle
                ]
            );
        }
        // PNG has no orientation loader option.
        let c = plan_tool(FormatId::Png, ToolKind::RotateCw, false).unwrap();
        assert_eq!(c[0][2], r"C:\in\a.src");
    }

    #[test]
    fn strip_keeps_only_the_color_profile() {
        let c = plan_tool(FormatId::Jpeg, ToolKind::StripMetadata, false).unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0][2], r"C:\in\a.src[autorotate]");
        assert_eq!(&c[0][c[0].len() - 2..], ["--keep", "icc"]);
        assert!(c[0].contains(&"92".to_owned()));
        let c = plan_tool(FormatId::Tiff, ToolKind::StripMetadata, true).unwrap();
        assert_eq!(&c[0][c[0].len() - 2..], ["--keep", "none"]);
    }

    #[test]
    fn privacy_mode_applies_to_other_tools() {
        let c = plan_tool(FormatId::Png, ToolKind::Resize50, true).unwrap();
        assert_eq!(&c[1][c[1].len() - 2..], ["--keep", "none"]);
        assert!(!c[0].contains(&"--keep".to_owned()));
    }

    #[test]
    fn tool_errors() {
        // Formats libvips cannot write again.
        for f in [FormatId::Heic, FormatId::Bmp, FormatId::Gif, FormatId::Heif] {
            assert!(matches!(
                plan_tool(f, ToolKind::Resize50, false),
                Err(PlanError::ToolUnsupported { .. })
            ));
        }
        // Not an image tool.
        assert!(plan_tool(FormatId::Mp4, ToolKind::Compress, false).is_err());
        assert!(plan_tool(FormatId::Png, ToolKind::VideoTargetSize(10), false).is_err());
        // Missing scratch folder.
        let a = VipsAdapter::new(PathBuf::from("vips.exe"));
        let e = a.plan(
            &req(Operation::Tool(ToolKind::Resize50), Quality::High),
            Path::new("a.png"),
            Path::new("b"),
            &PlanOptions::new(FormatId::Png),
        );
        assert!(matches!(e, Err(PlanError::Other(_))));
        // Actions are not vips work.
        assert!(a
            .plan(
                &req(Operation::Action(zest_core::ActionKind::Zip), Quality::High),
                Path::new("a.png"),
                Path::new("b"),
                &PlanOptions::new(FormatId::Png)
            )
            .is_err());
    }

    #[test]
    fn tool_paths_are_safe() {
        let a = VipsAdapter::new(PathBuf::from("vips.exe"));
        let mut o = PlanOptions::new(FormatId::Png);
        o.scratch_dir = Some(PathBuf::from(r"C:\s"));
        let c = a
            .plan(
                &req(Operation::Tool(ToolKind::MaxEdge1280), Quality::High),
                Path::new("-rf.png"),
                Path::new("-out"),
                &o,
            )
            .unwrap();
        assert_eq!(c[0].args[2], OsString::from(r".\-rf.png"));
        assert_eq!(c[1].args[3], OsString::from(r".\-out"));
    }

    #[test]
    fn scratch_is_needed_only_for_two_step_tools() {
        let a = VipsAdapter::new(PathBuf::new());
        let needs = |t| a.needs_scratch(&req(Operation::Tool(t), Quality::High), FormatId::Png);
        assert!(needs(ToolKind::Resize50));
        assert!(needs(ToolKind::MaxEdge1280));
        assert!(needs(ToolKind::Rotate180));
        assert!(!needs(ToolKind::Compress));
        assert!(!needs(ToolKind::StripMetadata));
        assert!(!a.needs_scratch(&req(Operation::Convert(FormatId::Jpeg), Quality::High), FormatId::Png));
    }
}
