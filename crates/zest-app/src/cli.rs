//! Command line (clap derive).

use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use zest_core::{parse_time_ms, Quality, ToolKind};

#[derive(Parser, Debug)]
#[command(
    name = "zest",
    version,
    about = "Zest: radial drag-and-drop file converter",
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    /// Open the wheel for these files (used by the right-click verb and Send To).
    #[arg(long, num_args = 1.., value_name = "FILE", conflicts_with_all = ["convert_list", "install", "uninstall"])]
    pub convert: Vec<PathBuf>,

    /// Open the wheel for the files listed in this text file (one path per line).
    #[arg(long, value_name = "LISTFILE", conflicts_with_all = ["install", "uninstall"])]
    pub convert_list: Option<PathBuf>,

    /// Add the right-click verb, the Send To shortcut and the sign-in entry.
    #[arg(long, conflicts_with = "uninstall")]
    pub install: bool,

    /// Remove the right-click verb, the Send To shortcut and the sign-in entry.
    #[arg(long)]
    pub uninstall: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Convert files without any UI.
    Convert(ConvertArgs),
    /// Run a tool (compress, resize, strip, rotate, target size) without any UI.
    Tool(ToolArgs),
    /// Put files and folders into one zip file.
    Zip(ZipArgs),
    /// Print the conversions the installed engines can do.
    Formats,
}

#[derive(Args, Debug)]
pub struct ConvertArgs {
    /// Target format, for example webp, jpg, mp4.
    #[arg(long, value_name = "EXT")]
    pub to: String,
    /// Quality preset.
    #[arg(long, value_enum, default_value_t = QualityArg::High)]
    pub quality: QualityArg,
    /// Output folder. Default: the config setting (next to the source).
    #[arg(long, value_name = "DIR")]
    pub out: Option<PathBuf>,
    /// Files to convert.
    #[arg(required = true, value_name = "FILES")]
    pub files: Vec<PathBuf>,
}

#[derive(Args, Debug)]
pub struct ToolArgs {
    /// The tool to run.
    #[arg(value_enum, value_name = "TOOL")]
    pub tool: ToolArg,
    /// Output folder. Default: the config setting (next to the source).
    #[arg(long, value_name = "DIR")]
    pub out: Option<PathBuf>,
    /// trim: start time (`SS`, `MM:SS` or `HH:MM:SS`, with `.mmm` if you like). Default: 0.
    #[arg(long, value_name = "TIME")]
    pub start: Option<String>,
    /// trim: end time. Default: the end of the file.
    #[arg(long, value_name = "TIME")]
    pub end: Option<String>,
    /// crop: the area to keep in pixels, `x,y,w,h`.
    #[arg(long, value_name = "X,Y,W,H")]
    pub rect: Option<String>,
    /// Files to process.
    #[arg(required = true, value_name = "FILES")]
    pub files: Vec<PathBuf>,
}

impl ToolArgs {
    /// The tool with its parameters (`--start`, `--end`, `--rect`).
    pub fn tool_kind(&self) -> Result<ToolKind, String> {
        let wrong = |what: &str, tool: &str| {
            format!("{what} is only for the {tool} tool")
        };
        match self.tool {
            ToolArg::Trim => {
                if self.rect.is_some() {
                    return Err(wrong("--rect", "crop"));
                }
                let time = |name: &str, v: &Option<String>| -> Result<Option<u64>, String> {
                    v.as_deref()
                        .map(|t| parse_time_ms(t).ok_or_else(|| format!("{name}: \"{t}\" is not a time (use MM:SS or MM:SS.mmm)")))
                        .transpose()
                };
                let start_ms = time("--start", &self.start)?.unwrap_or(0);
                let end_ms = time("--end", &self.end)?.unwrap_or(0);
                if end_ms != 0 && end_ms <= start_ms {
                    return Err("--end must be after --start".to_owned());
                }
                Ok(ToolKind::Trim { start_ms, end_ms })
            }
            ToolArg::Crop => {
                if self.start.is_some() || self.end.is_some() {
                    return Err(wrong("--start and --end", "trim"));
                }
                let text = self.rect.as_deref().ok_or("crop needs --rect x,y,w,h")?;
                let n: Vec<u32> = text
                    .split(',')
                    .map(|v| v.trim().parse::<u32>())
                    .collect::<Result<_, _>>()
                    .map_err(|_| format!("--rect: \"{text}\" is not x,y,w,h (whole numbers)"))?;
                match n.as_slice() {
                    [x, y, w, h] if *w > 0 && *h > 0 => Ok(ToolKind::Crop { x: *x, y: *y, w: *w, h: *h }),
                    [_, _, _, _] => Err("--rect: width and height must be more than 0".to_owned()),
                    _ => Err(format!("--rect: \"{text}\" is not x,y,w,h")),
                }
            }
            other => {
                if self.start.is_some() || self.end.is_some() {
                    return Err(wrong("--start and --end", "trim"));
                }
                if self.rect.is_some() {
                    return Err(wrong("--rect", "crop"));
                }
                Ok(other.into())
            }
        }
    }
}

#[derive(Args, Debug)]
pub struct ZipArgs {
    /// Output folder. Default: the config setting (next to the first item).
    #[arg(long, value_name = "DIR")]
    pub out: Option<PathBuf>,
    /// Files and folders to put into the zip file.
    #[arg(required = true, value_name = "ITEMS")]
    pub items: Vec<PathBuf>,
}

/// Tool names on the command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ToolArg {
    #[value(name = "compress")]
    Compress,
    #[value(name = "resize50")]
    Resize50,
    #[value(name = "resize25")]
    Resize25,
    #[value(name = "max1920")]
    Max1920,
    #[value(name = "max1280")]
    Max1280,
    #[value(name = "strip")]
    Strip,
    #[value(name = "rotate-cw")]
    RotateCw,
    #[value(name = "rotate-ccw")]
    RotateCcw,
    #[value(name = "rotate180")]
    Rotate180,
    #[value(name = "size10")]
    Size10,
    #[value(name = "size25")]
    Size25,
    #[value(name = "size50")]
    Size50,
    #[value(name = "merge-pdf")]
    MergePdf,
    #[value(name = "trim")]
    Trim,
    #[value(name = "crop")]
    Crop,
}

impl From<ToolArg> for ToolKind {
    fn from(t: ToolArg) -> ToolKind {
        match t {
            ToolArg::Compress => ToolKind::Compress,
            ToolArg::Resize50 => ToolKind::Resize50,
            ToolArg::Resize25 => ToolKind::Resize25,
            ToolArg::Max1920 => ToolKind::MaxEdge1920,
            ToolArg::Max1280 => ToolKind::MaxEdge1280,
            ToolArg::Strip => ToolKind::StripMetadata,
            ToolArg::RotateCw => ToolKind::RotateCw,
            ToolArg::RotateCcw => ToolKind::RotateCcw,
            ToolArg::Rotate180 => ToolKind::Rotate180,
            ToolArg::Size10 => ToolKind::VideoTargetSize(10),
            ToolArg::Size25 => ToolKind::VideoTargetSize(25),
            ToolArg::Size50 => ToolKind::VideoTargetSize(50),
            ToolArg::MergePdf => ToolKind::MergePdf,
            // Placeholders: `ToolArgs::tool_kind` fills in the parameters.
            ToolArg::Trim => ToolKind::TRIM_ITEM,
            ToolArg::Crop => ToolKind::CROP_ITEM,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum QualityArg {
    High,
    Medium,
    Small,
}

impl From<QualityArg> for Quality {
    fn from(q: QualityArg) -> Quality {
        match q {
            QualityArg::High => Quality::High,
            QualityArg::Medium => Quality::Medium,
            QualityArg::Small => Quality::Small,
        }
    }
}

/// What the process must do.
#[derive(Debug, PartialEq)]
pub enum Action {
    /// Run the tray app (and open the wheel for `paths`, if any).
    Tray { paths: Vec<PathBuf> },
    Install,
    Uninstall,
    Convert(Box<ConvertSpec>),
    Tool(Box<ToolSpec>),
    Zip(Box<ZipSpec>),
    Formats,
}

#[derive(Debug, PartialEq)]
pub struct ToolSpec {
    pub tool: ToolKind,
    pub out: Option<PathBuf>,
    pub files: Vec<PathBuf>,
}

#[derive(Debug, PartialEq)]
pub struct ZipSpec {
    pub out: Option<PathBuf>,
    pub items: Vec<PathBuf>,
}

#[derive(Debug, PartialEq)]
pub struct ConvertSpec {
    pub to: String,
    pub quality: Quality,
    pub out: Option<PathBuf>,
    pub files: Vec<PathBuf>,
}

impl Cli {
    /// Turn the parsed arguments into an action. `read_list` reads a
    /// `--convert-list` file (an error message on failure).
    pub fn into_action(
        self,
        read_list: impl Fn(&std::path::Path) -> Result<Vec<PathBuf>, String>,
    ) -> Result<Action, String> {
        if let Some(cmd) = self.command {
            return Ok(match cmd {
                Command::Formats => Action::Formats,
                Command::Convert(a) => Action::Convert(Box::new(ConvertSpec {
                    to: a.to,
                    quality: a.quality.into(),
                    out: a.out,
                    files: a.files,
                })),
                Command::Tool(a) => Action::Tool(Box::new(ToolSpec {
                    tool: a.tool_kind()?,
                    out: a.out,
                    files: a.files,
                })),
                Command::Zip(a) => Action::Zip(Box::new(ZipSpec { out: a.out, items: a.items })),
            });
        }
        if self.install {
            return Ok(Action::Install);
        }
        if self.uninstall {
            return Ok(Action::Uninstall);
        }
        let mut paths = self.convert;
        if let Some(list) = self.convert_list {
            paths.extend(read_list(&list)?);
        }
        Ok(Action::Tray { paths })
    }
}

/// Parse the text of a `--convert-list` file: one path per line, blank lines
/// and a UTF-8 BOM ignored.
pub fn parse_list(text: &str) -> Vec<PathBuf> {
    text.trim_start_matches('\u{feff}')
        .lines()
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty())
        .map(PathBuf::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("zest").chain(args.iter().copied()))
    }

    fn no_list(_: &std::path::Path) -> Result<Vec<PathBuf>, String> {
        Err("no list".into())
    }

    #[test]
    fn plain_start_is_tray() {
        let a = parse(&[]).unwrap().into_action(no_list).unwrap();
        assert_eq!(a, Action::Tray { paths: vec![] });
    }

    #[test]
    fn convert_flag_takes_many_files() {
        let a = parse(&["--convert", "a.png", "b b.png"]).unwrap().into_action(no_list).unwrap();
        assert_eq!(
            a,
            Action::Tray { paths: vec![PathBuf::from("a.png"), PathBuf::from("b b.png")] }
        );
    }

    #[test]
    fn convert_list_is_read() {
        let a = parse(&["--convert-list", "l.txt"])
            .unwrap()
            .into_action(|_| Ok(vec![PathBuf::from("x.jpg")]))
            .unwrap();
        assert_eq!(a, Action::Tray { paths: vec![PathBuf::from("x.jpg")] });
        assert!(parse(&["--convert-list", "l.txt"]).unwrap().into_action(no_list).is_err());
    }

    #[test]
    fn convert_subcommand_full() {
        let a = parse(&["convert", "--to", "webp", "--quality", "small", "--out", "D:\\o", "a.png", "b.png"])
            .unwrap()
            .into_action(no_list)
            .unwrap();
        assert_eq!(
            a,
            Action::Convert(Box::new(ConvertSpec {
                to: "webp".into(),
                quality: Quality::Small,
                out: Some(PathBuf::from("D:\\o")),
                files: vec![PathBuf::from("a.png"), PathBuf::from("b.png")],
            }))
        );
    }

    #[test]
    fn convert_defaults_to_high_quality() {
        let a = parse(&["convert", "--to", "jpg", "a.png"]).unwrap().into_action(no_list).unwrap();
        match a {
            Action::Convert(s) => {
                assert_eq!(s.quality, Quality::High);
                assert_eq!(s.out, None);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn tool_subcommand() {
        let a = parse(&["tool", "resize50", "--out", "D:\\o", "a.png", "b b.jpg"])
            .unwrap()
            .into_action(no_list)
            .unwrap();
        assert_eq!(
            a,
            Action::Tool(Box::new(ToolSpec {
                tool: ToolKind::Resize50,
                out: Some(PathBuf::from("D:\\o")),
                files: vec![PathBuf::from("a.png"), PathBuf::from("b b.jpg")],
            }))
        );
        let a = parse(&["tool", "compress", "a.png"]).unwrap().into_action(no_list).unwrap();
        assert_eq!(
            a,
            Action::Tool(Box::new(ToolSpec {
                tool: ToolKind::Compress,
                out: None,
                files: vec![PathBuf::from("a.png")]
            }))
        );
    }

    #[test]
    fn every_tool_name_maps_to_its_tool() {
        for (name, want) in [
            ("compress", ToolKind::Compress),
            ("resize50", ToolKind::Resize50),
            ("resize25", ToolKind::Resize25),
            ("max1920", ToolKind::MaxEdge1920),
            ("max1280", ToolKind::MaxEdge1280),
            ("strip", ToolKind::StripMetadata),
            ("rotate-cw", ToolKind::RotateCw),
            ("rotate-ccw", ToolKind::RotateCcw),
            ("rotate180", ToolKind::Rotate180),
            ("size10", ToolKind::VideoTargetSize(10)),
            ("size25", ToolKind::VideoTargetSize(25)),
            ("size50", ToolKind::VideoTargetSize(50)),
            ("merge-pdf", ToolKind::MergePdf),
        ] {
            let a = parse(&["tool", name, "x.mp4"]).unwrap().into_action(no_list).unwrap();
            match a {
                Action::Tool(t) => assert_eq!(t.tool, want, "{name}"),
                other => panic!("unexpected {other:?}"),
            }
            // The names are the slugs used in the history and in core.
            assert_eq!(want.slug(), name);
        }
    }

    #[test]
    fn trim_and_crop_parameters() {
        let tool = |args: &[&str]| -> Result<ToolKind, String> {
            let mut v = vec!["tool"];
            v.extend_from_slice(args);
            match parse(&v).unwrap().into_action(no_list)? {
                Action::Tool(t) => Ok(t.tool),
                other => panic!("unexpected {other:?}"),
            }
        };
        assert_eq!(
            tool(&["trim", "--start", "00:01", "--end", "00:05", "a.mp4"]),
            Ok(ToolKind::Trim { start_ms: 1000, end_ms: 5000 })
        );
        assert_eq!(
            tool(&["trim", "--start", "1:02.5", "a.mp4"]),
            Ok(ToolKind::Trim { start_ms: 62_500, end_ms: 0 })
        );
        assert_eq!(tool(&["trim", "a.mp4"]), Ok(ToolKind::Trim { start_ms: 0, end_ms: 0 }));
        assert!(tool(&["trim", "--start", "9", "--end", "5", "a.mp4"]).is_err());
        assert!(tool(&["trim", "--start", "x", "a.mp4"]).is_err());
        assert!(tool(&["trim", "--rect", "1,2,3,4", "a.mp4"]).is_err());
        assert_eq!(
            tool(&["crop", "--rect", "10,20,300,200", "a.png"]),
            Ok(ToolKind::Crop { x: 10, y: 20, w: 300, h: 200 })
        );
        assert!(tool(&["crop", "a.png"]).is_err());
        assert!(tool(&["crop", "--rect", "1,2,3", "a.png"]).is_err());
        assert!(tool(&["crop", "--rect", "1,2,0,3", "a.png"]).is_err());
        assert!(tool(&["crop", "--rect", "1,2,3,4", "--start", "1", "a.png"]).is_err());
        assert!(tool(&["compress", "--start", "1", "a.png"]).is_err());
    }

    #[test]
    fn zip_subcommand() {
        let a = parse(&["zip", "a.png", "My Folder"]).unwrap().into_action(no_list).unwrap();
        assert_eq!(
            a,
            Action::Zip(Box::new(ZipSpec {
                out: None,
                items: vec![PathBuf::from("a.png"), PathBuf::from("My Folder")]
            }))
        );
        let a = parse(&["zip", "--out", "D:\\z", "a"]).unwrap().into_action(no_list).unwrap();
        match a {
            Action::Zip(z) => assert_eq!(z.out, Some(PathBuf::from("D:\\z"))),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn tool_and_zip_usage_errors() {
        assert!(parse(&["tool"]).is_err());
        assert!(parse(&["tool", "resize50"]).is_err()); // no files
        assert!(parse(&["tool", "sharpen", "a.png"]).is_err()); // unknown tool
        assert!(parse(&["tool", "size75", "a.mp4"]).is_err()); // only the presets
        assert!(parse(&["zip"]).is_err());
    }

    #[test]
    fn usage_errors() {
        assert!(parse(&["convert", "a.png"]).is_err()); // no --to
        assert!(parse(&["convert", "--to", "webp"]).is_err()); // no files
        assert!(parse(&["convert", "--to", "webp", "--quality", "ultra", "a.png"]).is_err());
        assert!(parse(&["--install", "--uninstall"]).is_err());
        assert!(parse(&["--convert", "a.png", "--install"]).is_err());
        assert!(parse(&["--bogus"]).is_err());
    }

    #[test]
    fn install_uninstall_formats() {
        assert_eq!(parse(&["--install"]).unwrap().into_action(no_list).unwrap(), Action::Install);
        assert_eq!(parse(&["--uninstall"]).unwrap().into_action(no_list).unwrap(), Action::Uninstall);
        assert_eq!(parse(&["formats"]).unwrap().into_action(no_list).unwrap(), Action::Formats);
    }

    #[test]
    fn list_parsing() {
        let l = parse_list("\u{feff}C:\\a.png\r\n\r\n  \nC:\\b c.png\n");
        assert_eq!(l, vec![PathBuf::from("C:\\a.png"), PathBuf::from("C:\\b c.png")]);
    }
}
