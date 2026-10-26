//! Operations: conversions, tools and actions (spec 5.3).
//!
//! `Convert` changes the format. `Tool` changes a file and keeps its format
//! (except video compress, which writes MP4). `Action` works on any item,
//! including folders.

use crate::format::{Category, FormatId};
use crate::graph::EngineId;
use serde::{Deserialize, Serialize};

/// What a job does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Convert(FormatId),
    Tool(ToolKind),
    Action(ActionKind),
}

impl Operation {
    /// Target format of a conversion.
    pub fn target(self) -> Option<FormatId> {
        match self {
            Operation::Convert(f) => Some(f),
            _ => None,
        }
    }

    /// Short stable name for history and logs: `jpg`, `compress`, `zip`.
    pub fn key(self) -> String {
        match self {
            Operation::Convert(f) => f.extension().to_owned(),
            Operation::Tool(t) => t.slug(),
            Operation::Action(a) => a.slug().to_owned(),
        }
    }

    /// Verb for result messages: "Converted", "Compressed", "Zipped".
    pub fn past_tense(self) -> &'static str {
        match self {
            Operation::Convert(_) => "Converted",
            Operation::Tool(t) => t.past_tense(),
            Operation::Action(ActionKind::Zip) => "Zipped",
            Operation::Action(ActionKind::CopyAs(_)) => "Copied",
            Operation::Action(_) => "Done",
        }
    }

    /// True for the actions that the app runs itself (not the broker).
    /// Zip and Copy as... are broker jobs.
    pub fn is_shell_action(self) -> bool {
        matches!(self, Operation::Action(a) if !a.is_broker_job())
    }
}

/// A tool: changes a file, keeps its format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    /// Same format, medium quality. Video: H.264 CRF 28 (MP4). Audio: lower bit rate.
    Compress,
    Resize50,
    Resize25,
    MaxEdge1920,
    MaxEdge1280,
    /// Images: re-save with no EXIF/GPS/XMP. Video and audio: stream copy without tags.
    StripMetadata,
    RotateCw,
    RotateCcw,
    Rotate180,
    /// Video: two-pass encode to fit in this many MB.
    VideoTargetSize(u32),
    /// PDF: join the selected PDFs (in selection order) into one file.
    /// One job for all files; valid only when two or more PDFs are selected.
    MergePdf,
    /// PDF: write each page as its own PDF in a folder.
    SplitPdf,
    /// PDF: re-save through PDFium (best-effort size reduction; may be a no-op).
    CompressPdf,
    /// Video and audio: keep the part from `start_ms` to `end_ms` (0 = to the end).
    /// Re-encoded for an exact cut. Single file; the parameters come from a dialog.
    Trim { start_ms: u64, end_ms: u64 },
    /// Images: keep the rectangle `x, y, w, h` (pixels, after the EXIF rotation).
    /// Single file; the rectangle comes from a dialog.
    Crop { x: u32, y: u32, w: u32, h: u32 },
}

const IMAGE_ONLY: &[Category] = &[Category::Image];
const ALL_CATEGORIES: &[Category] = &[Category::Image, Category::Video, Category::Audio];
const VIDEO_ONLY: &[Category] = &[Category::Video];
const VIDEO_AUDIO: &[Category] = &[Category::Video, Category::Audio];
const DOCUMENT_ONLY: &[Category] = &[Category::Document];

impl ToolKind {
    /// The tools on the wheel, in display order.
    pub const PRESETS: [ToolKind; 13] = [
        ToolKind::Compress,
        ToolKind::Resize50,
        ToolKind::Resize25,
        ToolKind::MaxEdge1920,
        ToolKind::MaxEdge1280,
        ToolKind::StripMetadata,
        ToolKind::RotateCw,
        ToolKind::RotateCcw,
        ToolKind::Rotate180,
        ToolKind::VideoTargetSize(10),
        ToolKind::VideoTargetSize(25),
        ToolKind::VideoTargetSize(50),
        // Sentinel: opens a size dialog (1–9999 MB).
        ToolKind::VideoTargetSize(0),
    ];

    /// Label on the sub-ring sector.
    pub fn label(self) -> &'static str {
        match self {
            ToolKind::Compress => "Compress",
            ToolKind::Resize50 => "50%",
            ToolKind::Resize25 => "25%",
            ToolKind::MaxEdge1920 => "Max 1920",
            ToolKind::MaxEdge1280 => "Max 1280",
            ToolKind::StripMetadata => "Strip meta",
            ToolKind::RotateCw => "\u{27F3} 90\u{B0}",
            ToolKind::RotateCcw => "\u{27F2} 90\u{B0}",
            ToolKind::Rotate180 => "180\u{B0}",
            ToolKind::VideoTargetSize(0) => "Custom\u{2026}",
            ToolKind::VideoTargetSize(1) => "1 MB",
            ToolKind::VideoTargetSize(10) => "10 MB",
            ToolKind::VideoTargetSize(25) => "25 MB",
            ToolKind::VideoTargetSize(50) => "50 MB",
            ToolKind::VideoTargetSize(_) => "Target size",
            ToolKind::MergePdf => "Merge PDFs",
            ToolKind::SplitPdf => "Split PDF",
            ToolKind::CompressPdf => "Compress PDF",
            ToolKind::Trim { .. } => "Trim\u{2026}",
            ToolKind::Crop { .. } => "Crop\u{2026}",
        }
    }

    /// The wheel item that opens the Trim dialog (no range yet).
    pub const TRIM_ITEM: ToolKind = ToolKind::Trim { start_ms: 0, end_ms: 0 };
    /// The wheel item that opens the Crop dialog (no rectangle yet).
    pub const CROP_ITEM: ToolKind = ToolKind::Crop { x: 0, y: 0, w: 0, h: 0 };

    /// True for a tool that needs a dialog first (Trim, Crop). The wheel item has empty parameters.
    pub fn needs_dialog(self) -> bool {
        matches!(
            self,
            ToolKind::Trim { .. } | ToolKind::Crop { .. } | ToolKind::VideoTargetSize(0)
        )
    }

    /// Categories this tool applies to.
    pub fn categories(self) -> &'static [Category] {
        match self {
            ToolKind::Compress | ToolKind::StripMetadata => ALL_CATEGORIES,
            ToolKind::VideoTargetSize(_) => VIDEO_ONLY,
            ToolKind::MergePdf | ToolKind::SplitPdf | ToolKind::CompressPdf => DOCUMENT_ONLY,
            ToolKind::Trim { .. } => VIDEO_AUDIO,
            _ => IMAGE_ONLY,
        }
    }

    /// Whether the tool can run on a file of this format.
    ///
    /// Image tools write the same format again, so they need a format libvips
    /// can write here: PNG, JPEG, WebP, AVIF, TIFF (not BMP, GIF, HEIC, HEIF).
    /// WAV cannot be compressed.
    pub fn applies_to(self, f: FormatId) -> bool {
        if !self.categories().contains(&f.category()) {
            return false;
        }
        match f.category() {
            Category::Image => image_resavable(f),
            Category::Video => true,
            Category::Audio => !(self == ToolKind::Compress && f == FormatId::Wav),
            Category::Document => {
                matches!(
                    self,
                    ToolKind::MergePdf | ToolKind::SplitPdf | ToolKind::CompressPdf
                ) && f == FormatId::Pdf
            }
        }
    }

    /// True for a tool that takes all files of the job together (one output).
    pub fn is_batch(self) -> bool {
        self == ToolKind::MergePdf
    }

    /// Engine that runs the tool for this input format.
    pub fn engine(self, f: FormatId) -> EngineId {
        match f.category() {
            Category::Image => EngineId::Vips,
            Category::Document => EngineId::Pdfium,
            _ => EngineId::Ffmpeg,
        }
    }

    /// Format of the output for an input of format `f`.
    /// Video compress and target size write MP4 (H.264/AAC). Everything else keeps the format.
    pub fn output_format(self, f: FormatId) -> FormatId {
        match (self, f.category()) {
            (ToolKind::Compress | ToolKind::VideoTargetSize(_) | ToolKind::Trim { .. }, Category::Video) => {
                FormatId::Mp4
            }
            _ => f,
        }
    }

    /// True if the output loses information (shows the marker dot).
    pub fn lossy_for(self, f: FormatId) -> bool {
        match f.category() {
            Category::Image => matches!(f, FormatId::Jpeg | FormatId::Webp | FormatId::Avif),
            Category::Document => false,
            Category::Video => !matches!(self, ToolKind::StripMetadata),
            Category::Audio => match self {
                ToolKind::StripMetadata => false,
                ToolKind::Trim { .. } => !matches!(f, FormatId::Flac | FormatId::Wav),
                _ => f != FormatId::Flac,
            },
        }
    }

    /// Text added to the file name: `photo-compressed.jpg`.
    pub fn suffix(self) -> String {
        match self {
            ToolKind::Compress => "compressed".to_owned(),
            ToolKind::Resize50 => "50pct".to_owned(),
            ToolKind::Resize25 => "25pct".to_owned(),
            ToolKind::MaxEdge1920 => "1920".to_owned(),
            ToolKind::MaxEdge1280 => "1280".to_owned(),
            ToolKind::StripMetadata => "stripped".to_owned(),
            ToolKind::RotateCw | ToolKind::RotateCcw | ToolKind::Rotate180 => "rotated".to_owned(),
            ToolKind::VideoTargetSize(0) => "customMB".to_owned(),
            ToolKind::VideoTargetSize(mb) => format!("{mb}MB"),
            ToolKind::MergePdf => "merged".to_owned(),
            ToolKind::SplitPdf => "split".to_owned(),
            ToolKind::CompressPdf => "pdf-compressed".to_owned(),
            ToolKind::Trim { .. } => "trim".to_owned(),
            ToolKind::Crop { .. } => "crop".to_owned(),
        }
    }

    /// Command line name: `compress`, `resize50`, `size10`.
    pub fn slug(self) -> String {
        match self {
            ToolKind::Compress => "compress".to_owned(),
            ToolKind::Resize50 => "resize50".to_owned(),
            ToolKind::Resize25 => "resize25".to_owned(),
            ToolKind::MaxEdge1920 => "max1920".to_owned(),
            ToolKind::MaxEdge1280 => "max1280".to_owned(),
            ToolKind::StripMetadata => "strip".to_owned(),
            ToolKind::RotateCw => "rotate-cw".to_owned(),
            ToolKind::RotateCcw => "rotate-ccw".to_owned(),
            ToolKind::Rotate180 => "rotate180".to_owned(),
            ToolKind::VideoTargetSize(0) => "size-custom".to_owned(),
            ToolKind::VideoTargetSize(mb) => format!("size{mb}"),
            ToolKind::MergePdf => "merge-pdf".to_owned(),
            ToolKind::SplitPdf => "split-pdf".to_owned(),
            ToolKind::CompressPdf => "compress-pdf".to_owned(),
            ToolKind::Trim { start_ms, end_ms } => format!("trim-{start_ms}-{end_ms}"),
            ToolKind::Crop { x, y, w, h } => format!("crop-{x}-{y}-{w}-{h}"),
        }
    }

    /// Inverse of [`ToolKind::slug`]. `sizeN` accepts any N from 1 to 9999.
    pub fn from_slug(s: &str) -> Option<ToolKind> {
        let s = s.trim().to_ascii_lowercase();
        if s == "size-custom" {
            return Some(ToolKind::VideoTargetSize(0));
        }
        if let Some(n) = s.strip_prefix("size") {
            return n.parse::<u32>().ok().filter(|n| (1..=9999).contains(n)).map(ToolKind::VideoTargetSize);
        }
        if s == "merge-pdf" {
            return Some(ToolKind::MergePdf);
        }
        if s == "split-pdf" {
            return Some(ToolKind::SplitPdf);
        }
        if s == "compress-pdf" {
            return Some(ToolKind::CompressPdf);
        }
        if let Some(rest) = s.strip_prefix("trim-") {
            let (a, b) = rest.split_once('-')?;
            return Some(ToolKind::Trim { start_ms: a.parse().ok()?, end_ms: b.parse().ok()? });
        }
        if let Some(rest) = s.strip_prefix("crop-") {
            let mut it = rest.split('-').map(|n| n.parse::<u32>().ok());
            let (x, y, w, h) = (it.next()??, it.next()??, it.next()??, it.next()??);
            return it.next().is_none().then_some(ToolKind::Crop { x, y, w, h });
        }
        ToolKind::PRESETS.into_iter().find(|t| t.slug() == s)
    }

    /// Verb for result messages.
    pub fn past_tense(self) -> &'static str {
        match self {
            ToolKind::Compress | ToolKind::CompressPdf => "Compressed",
            ToolKind::Resize50 | ToolKind::Resize25 | ToolKind::MaxEdge1920 | ToolKind::MaxEdge1280 => {
                "Resized"
            }
            ToolKind::StripMetadata => "Stripped",
            ToolKind::RotateCw | ToolKind::RotateCcw | ToolKind::Rotate180 => "Rotated",
            ToolKind::VideoTargetSize(_) => "Compressed",
            ToolKind::MergePdf => "Merged",
            ToolKind::SplitPdf => "Split",
            ToolKind::Trim { .. } => "Trimmed",
            ToolKind::Crop { .. } => "Cropped",
        }
    }
}

/// Parse a time: `90`, `1.5`, `01:30`, `1:02:03.250`, `00:05.5`. Milliseconds, at most 99 hours.
pub fn parse_time_ms(text: &str) -> Option<u64> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let parts: Vec<&str> = t.split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    let (last, head) = parts.split_last()?;
    // Seconds: digits with an optional fraction of up to 3 digits.
    let (sec, frac) = match last.split_once('.') {
        Some((s, f)) => (s, f),
        None => (*last, ""),
    };
    if sec.is_empty() || !sec.bytes().all(|b| b.is_ascii_digit()) || frac.len() > 3 {
        return None;
    }
    if !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut ms: u64 = sec.parse::<u64>().ok()?.checked_mul(1000)?;
    if !frac.is_empty() {
        let padded = format!("{frac:0<3}");
        ms += padded.parse::<u64>().ok()?;
    }
    let mut unit = 60_000u64;
    for p in head.iter().rev() {
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        ms = ms.checked_add(p.parse::<u64>().ok()?.checked_mul(unit)?)?;
        unit *= 60;
    }
    (ms <= 99 * 3_600_000 + 3_599_999).then_some(ms)
}

/// `mm:ss.mmm`, or `h:mm:ss.mmm` from one hour.
pub fn format_time_ms(ms: u64) -> String {
    let (h, m, s, f) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{f:03}")
    } else {
        format!("{m:02}:{s:02}.{f:03}")
    }
}

/// Image formats that Zest can write again (same-format tools).
fn image_resavable(f: FormatId) -> bool {
    matches!(
        f,
        FormatId::Png | FormatId::Jpeg | FormatId::Webp | FormatId::Avif | FormatId::Tiff
    )
}

/// An action: works on any item, including folders and unknown files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    CopyPath,
    CopyFiles,
    CopyTo,
    MoveTo,
    TerminalHere,
    Zip,
    /// Unzip into the folder that holds the archive.
    ExtractHere,
    /// Unzip into a folder the user picks.
    ExtractTo,
    /// Hold the items on the Shelf (a floating panel). Works on any item.
    SendToShelf,
    /// Windows share sheet for the files (folders are skipped).
    Share,
    /// Images only: convert into a temp folder and put the result on the clipboard
    /// (file list plus pixels for a single image). Only PNG and JPG are offered.
    CopyAs(FormatId),
}

impl ActionKind {
    /// The actions on the wheel, in display order.
    pub const ALL: [ActionKind; 10] = [
        ActionKind::CopyPath,
        ActionKind::CopyFiles,
        ActionKind::CopyTo,
        ActionKind::MoveTo,
        ActionKind::TerminalHere,
        ActionKind::Zip,
        ActionKind::ExtractHere,
        ActionKind::ExtractTo,
        ActionKind::SendToShelf,
        ActionKind::Share,
    ];

    /// The "Copy as..." actions, shown when all items are images.
    pub const COPY_AS: [ActionKind; 2] = [
        ActionKind::CopyAs(FormatId::Png),
        ActionKind::CopyAs(FormatId::Jpeg),
    ];

    /// True for the actions that the broker runs (Zip, Copy as...).
    pub fn is_broker_job(self) -> bool {
        matches!(self, ActionKind::Zip | ActionKind::CopyAs(_))
    }

    pub fn label(self) -> &'static str {
        match self {
            ActionKind::CopyAs(FormatId::Png) => "Copy as PNG",
            ActionKind::CopyAs(FormatId::Jpeg) => "Copy as JPG",
            ActionKind::CopyAs(_) => "Copy as\u{2026}",
            ActionKind::CopyPath => "Copy path",
            ActionKind::CopyFiles => "Copy files",
            ActionKind::CopyTo => "Copy to\u{2026}",
            ActionKind::MoveTo => "Move to\u{2026}",
            ActionKind::TerminalHere => "Terminal",
            ActionKind::Zip => "Zip",
            ActionKind::ExtractHere => "Extract here",
            ActionKind::ExtractTo => "Extract to\u{2026}",
            ActionKind::SendToShelf => "Shelf",
            ActionKind::Share => "Share",
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            ActionKind::CopyAs(FormatId::Png) => "copy-as-png",
            ActionKind::CopyAs(FormatId::Jpeg) => "copy-as-jpg",
            ActionKind::CopyAs(_) => "copy-as",
            ActionKind::CopyPath => "copy-path",
            ActionKind::CopyFiles => "copy-files",
            ActionKind::CopyTo => "copy-to",
            ActionKind::MoveTo => "move-to",
            ActionKind::TerminalHere => "terminal",
            ActionKind::Zip => "zip",
            ActionKind::ExtractHere => "extract-here",
            ActionKind::ExtractTo => "extract-to",
            ActionKind::SendToShelf => "shelf",
            ActionKind::Share => "share",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use FormatId::*;

    #[test]
    fn labels_match_spec() {
        let want = [
            "Compress",
            "50%",
            "25%",
            "Max 1920",
            "Max 1280",
            "Strip meta",
            "\u{27F3} 90\u{B0}",
            "\u{27F2} 90\u{B0}",
            "180\u{B0}",
            "10 MB",
            "25 MB",
            "50 MB",
            "Custom\u{2026}",
        ];
        let got: Vec<&str> = ToolKind::PRESETS.iter().map(|t| t.label()).collect();
        assert_eq!(got, want);
        let acts: Vec<&str> = ActionKind::ALL.iter().map(|a| a.label()).collect();
        assert_eq!(
            acts,
            [
                "Copy path",
                "Copy files",
                "Copy to\u{2026}",
                "Move to\u{2026}",
                "Terminal",
                "Zip",
                "Extract here",
                "Extract to\u{2026}",
                "Shelf",
                "Share"
            ]
        );
    }

    #[test]
    fn categories_and_formats() {
        assert!(ToolKind::Resize50.applies_to(Png));
        assert!(!ToolKind::Resize50.applies_to(Mp4));
        assert!(!ToolKind::Resize50.applies_to(Heic));
        assert!(!ToolKind::Rotate180.applies_to(Bmp));
        assert!(ToolKind::Compress.applies_to(Mkv));
        assert!(ToolKind::Compress.applies_to(Mp3));
        assert!(!ToolKind::Compress.applies_to(Wav));
        assert!(ToolKind::StripMetadata.applies_to(Wav));
        assert!(ToolKind::VideoTargetSize(10).applies_to(Avi));
        assert!(!ToolKind::VideoTargetSize(10).applies_to(Mp3));
        assert!(!ToolKind::VideoTargetSize(10).applies_to(Png));
        assert_eq!(ToolKind::VideoTargetSize(10).categories(), &[Category::Video]);
        assert_eq!(ToolKind::Compress.categories().len(), 3);
    }

    #[test]
    fn output_formats() {
        assert_eq!(ToolKind::Compress.output_format(Mov), Mp4);
        assert_eq!(ToolKind::VideoTargetSize(25).output_format(Mkv), Mp4);
        assert_eq!(ToolKind::StripMetadata.output_format(Mkv), Mkv);
        assert_eq!(ToolKind::Compress.output_format(Png), Png);
        assert_eq!(ToolKind::Compress.output_format(Flac), Flac);
    }

    #[test]
    fn suffixes_and_slugs() {
        let suffixes: Vec<String> = ToolKind::PRESETS.iter().map(|t| t.suffix()).collect();
        assert_eq!(
            suffixes,
            [
                "compressed",
                "50pct",
                "25pct",
                "1920",
                "1280",
                "stripped",
                "rotated",
                "rotated",
                "rotated",
                "10MB",
                "25MB",
                "50MB",
                "customMB",
            ]
        );
        for t in ToolKind::PRESETS {
            assert_eq!(ToolKind::from_slug(&t.slug()), Some(t), "{t:?}");
        }
        assert_eq!(ToolKind::from_slug("SIZE1"), Some(ToolKind::VideoTargetSize(1)));
        assert_eq!(ToolKind::from_slug("size0"), None); // sizeN requires 1..=9999; custom is size-custom
        assert_eq!(ToolKind::from_slug("nonsense"), None);
    }

    #[test]
    fn operation_helpers() {
        assert_eq!(Operation::Convert(Jpeg).target(), Some(Jpeg));
        assert_eq!(Operation::Tool(ToolKind::Compress).target(), None);
        assert_eq!(Operation::Convert(Jpeg).key(), "jpg");
        assert_eq!(Operation::Tool(ToolKind::VideoTargetSize(10)).key(), "size10");
        assert_eq!(Operation::Action(ActionKind::Zip).key(), "zip");
        assert!(Operation::Action(ActionKind::CopyPath).is_shell_action());
        assert!(!Operation::Action(ActionKind::Zip).is_shell_action());
        assert!(!Operation::Action(ActionKind::CopyAs(Png)).is_shell_action());
        assert_eq!(Operation::Action(ActionKind::CopyAs(Jpeg)).key(), "copy-as-jpg");
        assert_eq!(Operation::Action(ActionKind::CopyAs(Png)).past_tense(), "Copied");
        assert!(!Operation::Convert(Png).is_shell_action());
    }

    #[test]
    fn merge_pdf_tool() {
        let m = ToolKind::MergePdf;
        assert!(m.applies_to(Pdf));
        assert!(!m.applies_to(Docx));
        assert!(!m.applies_to(Png));
        assert!(!ToolKind::Compress.applies_to(Pdf));
        assert!(!ToolKind::Resize50.applies_to(Pdf));
        assert!(m.is_batch() && !ToolKind::Compress.is_batch());
        assert_eq!(m.engine(Pdf), EngineId::Pdfium);
        assert_eq!(m.output_format(Pdf), Pdf);
        assert_eq!(m.suffix(), "merged");
        assert_eq!(m.label(), "Merge PDFs");
        assert_eq!(ToolKind::from_slug("merge-pdf"), Some(m));
        assert_eq!(ToolKind::from_slug(&m.slug()), Some(m));
        assert!(!ToolKind::PRESETS.contains(&m));
        assert!(!m.lossy_for(Pdf));
        assert_eq!(m.past_tense(), "Merged");
    }

    #[test]
    fn trim_and_crop_tools() {
        let trim = ToolKind::Trim { start_ms: 1000, end_ms: 5500 };
        let crop = ToolKind::Crop { x: 10, y: 20, w: 300, h: 200 };
        assert!(trim.applies_to(Mp4) && trim.applies_to(Mp3));
        assert!(!trim.applies_to(Png) && trim.applies_to(Ogg));
        assert!(crop.applies_to(Jpeg) && crop.applies_to(Png));
        assert!(!crop.applies_to(Heic) && !crop.applies_to(Mp4));
        assert_eq!(trim.output_format(Mkv), Mp4);
        assert_eq!(trim.output_format(Mp3), Mp3);
        assert_eq!(crop.output_format(Jpeg), Jpeg);
        assert_eq!(trim.engine(Mp4), EngineId::Ffmpeg);
        assert_eq!(crop.engine(Png), EngineId::Vips);
        assert_eq!((trim.label(), crop.label()), ("Trim\u{2026}", "Crop\u{2026}"));
        assert_eq!((trim.suffix().as_str(), crop.suffix().as_str()), ("trim", "crop"));
        assert!(trim.needs_dialog() && crop.needs_dialog() && !ToolKind::Compress.needs_dialog());
        assert!(!ToolKind::PRESETS.contains(&trim));
        for t in [trim, crop, ToolKind::TRIM_ITEM, ToolKind::CROP_ITEM] {
            assert_eq!(ToolKind::from_slug(&t.slug()), Some(t), "{}", t.slug());
        }
        assert_eq!(trim.slug(), "trim-1000-5500");
        assert_eq!(crop.slug(), "crop-10-20-300-200");
        assert_eq!(ToolKind::from_slug("crop-1-2-3"), None);
        assert_eq!(ToolKind::from_slug("crop-1-2-3-4-5"), None);
        assert_eq!(ToolKind::from_slug("trim-x-5"), None);
    }

    #[test]
    fn time_text() {
        assert_eq!(parse_time_ms("90"), Some(90_000));
        assert_eq!(parse_time_ms("1.5"), Some(1_500));
        assert_eq!(parse_time_ms("00:01"), Some(1_000));
        assert_eq!(parse_time_ms("01:30"), Some(90_000));
        assert_eq!(parse_time_ms("1:02:03.250"), Some(3_723_250));
        assert_eq!(parse_time_ms("00:05.05"), Some(5_050));
        assert_eq!(parse_time_ms(" 0:00.001 "), Some(1));
        assert_eq!(parse_time_ms(""), None);
        assert_eq!(parse_time_ms("1:2:3:4"), None);
        assert_eq!(parse_time_ms("a"), None);
        assert_eq!(parse_time_ms("1.2345"), None);
        assert_eq!(parse_time_ms("-1"), None);
        assert_eq!(parse_time_ms("1:-2"), None);
        assert_eq!(format_time_ms(5_050), "00:05.050");
        assert_eq!(format_time_ms(3_723_250), "1:02:03.250");
        for ms in [0, 999, 61_001, 3_600_000] {
            assert_eq!(parse_time_ms(&format_time_ms(ms)), Some(ms));
        }
    }

    #[test]
    fn copy_as_labels() {
        assert_eq!(ActionKind::CopyAs(Png).label(), "Copy as PNG");
        assert_eq!(ActionKind::CopyAs(Jpeg).label(), "Copy as JPG");
        assert_eq!(ActionKind::COPY_AS.len(), 2);
        assert!(!ActionKind::ALL.iter().any(|a| matches!(a, ActionKind::CopyAs(_))));
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct W {
            op: Operation,
        }
        let w = W { op: Operation::Action(ActionKind::CopyAs(Png)) };
        let s = toml::to_string(&w).unwrap();
        assert_eq!(toml::from_str::<W>(&s).unwrap(), w, "{s}");
    }

    #[test]
    fn lossy_flags() {
        assert!(ToolKind::Compress.lossy_for(Jpeg));
        assert!(!ToolKind::Compress.lossy_for(Png));
        assert!(!ToolKind::StripMetadata.lossy_for(Mp4));
        assert!(ToolKind::Compress.lossy_for(Mp4));
        assert!(!ToolKind::Compress.lossy_for(Flac));
    }

    #[test]
    fn serde_names() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct W {
            op: Operation,
        }
        for op in [
            Operation::Convert(Webp),
            Operation::Tool(ToolKind::Rotate180),
            Operation::Tool(ToolKind::VideoTargetSize(10)),
            Operation::Action(ActionKind::CopyTo),
        ] {
            let s = toml::to_string(&W { op }).unwrap();
            assert_eq!(toml::from_str::<W>(&s).unwrap(), W { op }, "{s}");
        }
        let w: W = toml::from_str("op = { convert = \"webp\" }").unwrap();
        assert_eq!(w.op, Operation::Convert(Webp));
    }
}
