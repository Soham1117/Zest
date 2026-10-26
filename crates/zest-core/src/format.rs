//! File formats, extension mapping and signature (magic byte) detection.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Broad kind of a format. Decides how mixed selections are grouped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Image,
    Video,
    Audio,
    /// PDF, Office, OpenDocument and text documents.
    Document,
}

/// Identifier of a file format Zest knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FormatId {
    Png,
    Jpeg,
    Webp,
    Avif,
    Tiff,
    Bmp,
    Gif,
    Heic,
    Heif,
    Mp4,
    Mov,
    Mkv,
    Webm,
    Avi,
    Mp3,
    Wav,
    Flac,
    M4a,
    Ogg,
    Opus,
    Pdf,
    Docx,
    Doc,
    Odt,
    Rtf,
    Pptx,
    Ppt,
    Odp,
    Xlsx,
    Xls,
    Ods,
    Csv,
    Md,
    Html,
    Txt,
    Epub,
}

impl FormatId {
    /// All formats, in declaration order.
    pub const ALL: [FormatId; 36] = [
        FormatId::Png,
        FormatId::Jpeg,
        FormatId::Webp,
        FormatId::Avif,
        FormatId::Tiff,
        FormatId::Bmp,
        FormatId::Gif,
        FormatId::Heic,
        FormatId::Heif,
        FormatId::Mp4,
        FormatId::Mov,
        FormatId::Mkv,
        FormatId::Webm,
        FormatId::Avi,
        FormatId::Mp3,
        FormatId::Wav,
        FormatId::Flac,
        FormatId::M4a,
        FormatId::Ogg,
        FormatId::Opus,
        FormatId::Pdf,
        FormatId::Docx,
        FormatId::Doc,
        FormatId::Odt,
        FormatId::Rtf,
        FormatId::Pptx,
        FormatId::Ppt,
        FormatId::Odp,
        FormatId::Xlsx,
        FormatId::Xls,
        FormatId::Ods,
        FormatId::Csv,
        FormatId::Md,
        FormatId::Html,
        FormatId::Txt,
        FormatId::Epub,
    ];

    /// Map a file extension (with or without leading dot, any case) to a format.
    pub fn from_extension(ext: &str) -> Option<FormatId> {
        let ext = ext.strip_prefix('.').unwrap_or(ext);
        if ext.is_empty() || ext.len() > 8 {
            return None;
        }
        let mut buf = [0u8; 8];
        for (d, s) in buf.iter_mut().zip(ext.bytes()) {
            *d = s.to_ascii_lowercase();
        }
        let lower = std::str::from_utf8(&buf[..ext.len()]).ok()?;
        Some(match lower {
            "png" => FormatId::Png,
            "jpg" | "jpeg" | "jfif" => FormatId::Jpeg,
            "webp" => FormatId::Webp,
            "avif" => FormatId::Avif,
            "tif" | "tiff" => FormatId::Tiff,
            "bmp" => FormatId::Bmp,
            "gif" => FormatId::Gif,
            "heic" => FormatId::Heic,
            "heif" => FormatId::Heif,
            "mp4" => FormatId::Mp4,
            "mov" => FormatId::Mov,
            "mkv" => FormatId::Mkv,
            "webm" => FormatId::Webm,
            "avi" => FormatId::Avi,
            "mp3" => FormatId::Mp3,
            "wav" => FormatId::Wav,
            "flac" => FormatId::Flac,
            "m4a" => FormatId::M4a,
            "ogg" | "oga" => FormatId::Ogg,
            "opus" => FormatId::Opus,
            "pdf" => FormatId::Pdf,
            "docx" => FormatId::Docx,
            "doc" => FormatId::Doc,
            "odt" => FormatId::Odt,
            "rtf" => FormatId::Rtf,
            "pptx" => FormatId::Pptx,
            "ppt" => FormatId::Ppt,
            "odp" => FormatId::Odp,
            "xlsx" => FormatId::Xlsx,
            "xls" => FormatId::Xls,
            "ods" => FormatId::Ods,
            "csv" => FormatId::Csv,
            "md" | "markdown" => FormatId::Md,
            "html" | "htm" => FormatId::Html,
            "txt" | "text" => FormatId::Txt,
            "epub" => FormatId::Epub,
            _ => return None,
        })
    }

    /// Canonical extension for output files (lowercase, no dot).
    pub fn extension(self) -> &'static str {
        match self {
            FormatId::Png => "png",
            FormatId::Jpeg => "jpg",
            FormatId::Webp => "webp",
            FormatId::Avif => "avif",
            FormatId::Tiff => "tiff",
            FormatId::Bmp => "bmp",
            FormatId::Gif => "gif",
            FormatId::Heic => "heic",
            FormatId::Heif => "heif",
            FormatId::Mp4 => "mp4",
            FormatId::Mov => "mov",
            FormatId::Mkv => "mkv",
            FormatId::Webm => "webm",
            FormatId::Avi => "avi",
            FormatId::Mp3 => "mp3",
            FormatId::Wav => "wav",
            FormatId::Flac => "flac",
            FormatId::M4a => "m4a",
            FormatId::Ogg => "ogg",
            FormatId::Opus => "opus",
            FormatId::Pdf => "pdf",
            FormatId::Docx => "docx",
            FormatId::Doc => "doc",
            FormatId::Odt => "odt",
            FormatId::Rtf => "rtf",
            FormatId::Pptx => "pptx",
            FormatId::Ppt => "ppt",
            FormatId::Odp => "odp",
            FormatId::Xlsx => "xlsx",
            FormatId::Xls => "xls",
            FormatId::Ods => "ods",
            FormatId::Csv => "csv",
            FormatId::Md => "md",
            FormatId::Html => "html",
            FormatId::Txt => "txt",
            FormatId::Epub => "epub",
        }
    }

    /// Label of a target that is reached through two or more steps: `"AVIF \u{219D}"`.
    /// The strings are made once (36 short strings for the whole run).
    pub fn routed_label(self) -> &'static str {
        static CACHE: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
        let all = CACHE.get_or_init(|| {
            FormatId::ALL
                .iter()
                .map(|f| &*Box::leak(format!("{} \u{219D}", f.label()).into_boxed_str()))
                .collect()
        });
        FormatId::ALL.iter().position(|f| *f == self).map_or(self.label(), |i| all[i])
    }

    /// Short upper-case label for the wheel.
    pub fn label(self) -> &'static str {
        match self {
            FormatId::Png => "PNG",
            FormatId::Jpeg => "JPG",
            FormatId::Webp => "WEBP",
            FormatId::Avif => "AVIF",
            FormatId::Tiff => "TIFF",
            FormatId::Bmp => "BMP",
            FormatId::Gif => "GIF",
            FormatId::Heic => "HEIC",
            FormatId::Heif => "HEIF",
            FormatId::Mp4 => "MP4",
            FormatId::Mov => "MOV",
            FormatId::Mkv => "MKV",
            FormatId::Webm => "WEBM",
            FormatId::Avi => "AVI",
            FormatId::Mp3 => "MP3",
            FormatId::Wav => "WAV",
            FormatId::Flac => "FLAC",
            FormatId::M4a => "M4A",
            FormatId::Ogg => "OGG",
            FormatId::Opus => "OPUS",
            FormatId::Pdf => "PDF",
            FormatId::Docx => "DOCX",
            FormatId::Doc => "DOC",
            FormatId::Odt => "ODT",
            FormatId::Rtf => "RTF",
            FormatId::Pptx => "PPTX",
            FormatId::Ppt => "PPT",
            FormatId::Odp => "ODP",
            FormatId::Xlsx => "XLSX",
            FormatId::Xls => "XLS",
            FormatId::Ods => "ODS",
            FormatId::Csv => "CSV",
            FormatId::Md => "MD",
            FormatId::Html => "HTML",
            FormatId::Txt => "TXT",
            FormatId::Epub => "EPUB",
        }
    }

    /// Category of this format. GIF counts as an image.
    pub fn category(self) -> Category {
        match self {
            FormatId::Png
            | FormatId::Jpeg
            | FormatId::Webp
            | FormatId::Avif
            | FormatId::Tiff
            | FormatId::Bmp
            | FormatId::Gif
            | FormatId::Heic
            | FormatId::Heif => Category::Image,
            FormatId::Mp4 | FormatId::Mov | FormatId::Mkv | FormatId::Webm | FormatId::Avi => {
                Category::Video
            }
            FormatId::Mp3
            | FormatId::Wav
            | FormatId::Flac
            | FormatId::M4a
            | FormatId::Ogg
            | FormatId::Opus => Category::Audio,
            FormatId::Pdf
            | FormatId::Docx
            | FormatId::Doc
            | FormatId::Odt
            | FormatId::Rtf
            | FormatId::Pptx
            | FormatId::Ppt
            | FormatId::Odp
            | FormatId::Xlsx
            | FormatId::Xls
            | FormatId::Ods
            | FormatId::Csv
            | FormatId::Md
            | FormatId::Html
            | FormatId::Txt
            | FormatId::Epub => Category::Document,
        }
    }
}

impl fmt::Display for FormatId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

/// How many bytes of a file the signature checks look at.
pub const SIGNATURE_BYTES: usize = 4096;

/// True if `b` looks like UTF-8 text: no NUL byte, valid UTF-8 (a character
/// cut off at the end of the buffer is allowed). An empty buffer is not text.
fn looks_like_text(b: &[u8]) -> bool {
    if b.is_empty() || b.contains(&0) {
        return false;
    }
    let b = &b[..b.len().min(SIGNATURE_BYTES)];
    match std::str::from_utf8(b) {
        Ok(_) => true,
        // `error_len() == None` means the buffer ends inside a character.
        Err(e) => e.error_len().is_none(),
    }
}

const ZIP_SIGNATURE: &[u8] = b"PK\x03\x04";
const OLE2_SIGNATURE: &[u8] = &[0xD0, 0xCF, 0x11, 0xE0];

/// Whether `head` fits a document format declared by its extension.
///
/// ZIP based formats (OOXML, ODF, EPUB) accept any ZIP signature; the entry
/// names cannot be read reliably from the first bytes. Old Office formats need
/// the OLE2 signature. Text formats (CSV, Markdown, HTML, TXT) need valid UTF-8.
/// `false` for formats that are not documents.
pub fn document_signature_ok(declared: FormatId, head: &[u8]) -> bool {
    use FormatId::*;
    match declared {
        Pdf => head.starts_with(b"%PDF-"),
        Docx | Odt | Pptx | Odp | Xlsx | Ods | Epub => head.starts_with(ZIP_SIGNATURE),
        Doc | Ppt | Xls => head.starts_with(OLE2_SIGNATURE),
        Rtf => head.starts_with(b"{\\rtf"),
        Csv | Md | Html | Txt => looks_like_text(head),
        _ => false,
    }
}

/// Whether `head` is a valid start of a file of format `f` (any category).
pub fn signature_ok(f: FormatId, head: &[u8]) -> bool {
    if f.category() == Category::Document {
        return document_signature_ok(f, head);
    }
    detect_signature(head).is_some_and(|d| signature_compatible(f, d))
}

/// Detect a format from the first bytes of a file (64 bytes are enough).
/// Only PDF is detected among the documents; the others need the extension
/// (see [`document_signature_ok`]).
/// HEIF files with a plain `heic` brand map to `Heic`; `Heif` is never returned.
pub fn detect_signature(b: &[u8]) -> Option<FormatId> {
    if b.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some(FormatId::Png);
    }
    if b.starts_with(b"%PDF-") {
        return Some(FormatId::Pdf);
    }
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(FormatId::Jpeg);
    }
    if b.starts_with(b"GIF8") {
        return Some(FormatId::Gif);
    }
    if b.starts_with(b"BM") {
        return Some(FormatId::Bmp);
    }
    if b.starts_with(&[b'I', b'I', 0x2A, 0x00]) || b.starts_with(&[b'M', b'M', 0x00, 0x2A]) {
        return Some(FormatId::Tiff);
    }
    if b.len() >= 12 && &b[0..4] == b"RIFF" {
        return match &b[8..12] {
            b"WEBP" => Some(FormatId::Webp),
            b"WAVE" => Some(FormatId::Wav),
            b"AVI " => Some(FormatId::Avi),
            _ => None,
        };
    }
    if b.len() >= 12 && &b[4..8] == b"ftyp" {
        return match &b[8..12] {
            b"heic" | b"heix" | b"mif1" | b"msf1" => Some(FormatId::Heic),
            b"avif" | b"avis" => Some(FormatId::Avif),
            b"qt  " => Some(FormatId::Mov),
            b"M4A " => Some(FormatId::M4a),
            b"isom" | b"mp41" | b"mp42" | b"avc1" => Some(FormatId::Mp4),
            _ => None,
        };
    }
    if b.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        let head = &b[..b.len().min(64)];
        return Some(if contains(head, b"webm") {
            FormatId::Webm
        } else {
            FormatId::Mkv
        });
    }
    if b.starts_with(b"fLaC") {
        return Some(FormatId::Flac);
    }
    if b.starts_with(b"OggS") {
        let head = &b[..b.len().min(64)];
        return Some(if contains(head, b"OpusHead") {
            FormatId::Opus
        } else {
            FormatId::Ogg
        });
    }
    if b.starts_with(b"ID3") {
        return Some(FormatId::Mp3);
    }
    // MPEG audio frame sync: 11 bits set, layer bits not 00 (00 would be ADTS AAC).
    if b.len() >= 2 && b[0] == 0xFF && (b[1] & 0xE0) == 0xE0 && (b[1] >> 1) & 0x03 != 0 {
        return Some(FormatId::Mp3);
    }
    None
}

/// Whether a file declared as `declared` (by extension) is acceptable when its
/// signature says `detected`. Containers that share one byte layout are treated as one family.
pub fn signature_compatible(declared: FormatId, detected: FormatId) -> bool {
    use FormatId::*;
    if declared == detected {
        return true;
    }
    let family = |f: FormatId| -> u8 {
        match f {
            Mkv | Webm => 1,
            Heic | Heif | Avif => 2,
            Mp4 | Mov | M4a => 3,
            Ogg | Opus => 4,
            _ => 0,
        }
    };
    let (a, b) = (family(declared), family(detected));
    a != 0 && a == b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ftyp(brand: &[u8; 4]) -> Vec<u8> {
        let mut v = vec![0, 0, 0, 0x18];
        v.extend_from_slice(b"ftyp");
        v.extend_from_slice(brand);
        v.extend_from_slice(&[0; 16]);
        v
    }

    fn riff(kind: &[u8; 4]) -> Vec<u8> {
        let mut v = b"RIFF".to_vec();
        v.extend_from_slice(&[0x24, 0, 0, 0]);
        v.extend_from_slice(kind);
        v.extend_from_slice(&[0; 8]);
        v
    }

    #[test]
    fn detects_images() {
        let png = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0];
        assert_eq!(detect_signature(&png), Some(FormatId::Png));
        assert_eq!(detect_signature(&[0xFF, 0xD8, 0xFF, 0xE0, 0]), Some(FormatId::Jpeg));
        assert_eq!(detect_signature(b"GIF89a\x01\x00"), Some(FormatId::Gif));
        assert_eq!(detect_signature(b"GIF87a\x01\x00"), Some(FormatId::Gif));
        assert_eq!(detect_signature(b"BM\x36\x00\x00\x00"), Some(FormatId::Bmp));
        assert_eq!(detect_signature(&[b'I', b'I', 0x2A, 0, 8, 0]), Some(FormatId::Tiff));
        assert_eq!(detect_signature(&[b'M', b'M', 0, 0x2A, 0, 8]), Some(FormatId::Tiff));
        assert_eq!(detect_signature(&riff(b"WEBP")), Some(FormatId::Webp));
        assert_eq!(detect_signature(&ftyp(b"heic")), Some(FormatId::Heic));
        assert_eq!(detect_signature(&ftyp(b"heix")), Some(FormatId::Heic));
        assert_eq!(detect_signature(&ftyp(b"mif1")), Some(FormatId::Heic));
        assert_eq!(detect_signature(&ftyp(b"msf1")), Some(FormatId::Heic));
        assert_eq!(detect_signature(&ftyp(b"avif")), Some(FormatId::Avif));
        assert_eq!(detect_signature(&ftyp(b"avis")), Some(FormatId::Avif));
    }

    #[test]
    fn detects_video() {
        assert_eq!(detect_signature(&ftyp(b"qt  ")), Some(FormatId::Mov));
        for b in [b"isom", b"mp41", b"mp42", b"avc1"] {
            assert_eq!(detect_signature(&ftyp(b)), Some(FormatId::Mp4));
        }
        assert_eq!(detect_signature(&riff(b"AVI ")), Some(FormatId::Avi));
        let mut mkv = vec![0x1A, 0x45, 0xDF, 0xA3, 0x9F, 0x42, 0x82, 0x88];
        mkv.extend_from_slice(b"matroska");
        assert_eq!(detect_signature(&mkv), Some(FormatId::Mkv));
        let mut webm = vec![0x1A, 0x45, 0xDF, 0xA3, 0x9F, 0x42, 0x82, 0x84];
        webm.extend_from_slice(b"webm");
        assert_eq!(detect_signature(&webm), Some(FormatId::Webm));
    }

    #[test]
    fn detects_audio() {
        assert_eq!(detect_signature(&ftyp(b"M4A ")), Some(FormatId::M4a));
        assert_eq!(detect_signature(&riff(b"WAVE")), Some(FormatId::Wav));
        assert_eq!(detect_signature(b"fLaC\0\0\0\x22"), Some(FormatId::Flac));
        assert_eq!(detect_signature(b"ID3\x03\0\0\0\0"), Some(FormatId::Mp3));
        assert_eq!(detect_signature(&[0xFF, 0xFB, 0x90, 0x00]), Some(FormatId::Mp3));
        assert_eq!(detect_signature(&[0xFF, 0xF1, 0x50, 0x80]), None); // ADTS AAC, not MP3
        let mut ogg = b"OggS\0\x02".to_vec();
        ogg.extend_from_slice(&[0; 20]);
        ogg.extend_from_slice(b"\x01vorbis");
        assert_eq!(detect_signature(&ogg), Some(FormatId::Ogg));
        let mut opus = b"OggS\0\x02".to_vec();
        opus.extend_from_slice(&[0; 20]);
        opus.extend_from_slice(b"OpusHead");
        assert_eq!(detect_signature(&opus), Some(FormatId::Opus));
    }

    #[test]
    fn detects_pdf_and_checks_documents() {
        assert_eq!(detect_signature(b"%PDF-1.7\n"), Some(FormatId::Pdf));
        assert!(document_signature_ok(FormatId::Pdf, b"%PDF-1.4"));
        assert!(!document_signature_ok(FormatId::Pdf, b"hello"));
        for f in [FormatId::Docx, FormatId::Odt, FormatId::Pptx, FormatId::Odp, FormatId::Xlsx, FormatId::Ods, FormatId::Epub] {
            assert!(document_signature_ok(f, b"PK\x03\x04rest"), "{f:?}");
            assert!(!document_signature_ok(f, b"%PDF-"), "{f:?}");
        }
        let ole = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
        for f in [FormatId::Doc, FormatId::Ppt, FormatId::Xls] {
            assert!(document_signature_ok(f, &ole), "{f:?}");
            assert!(!document_signature_ok(f, b"PK\x03\x04"), "{f:?}");
        }
        assert!(document_signature_ok(FormatId::Rtf, b"{\\rtf1\\ansi"));
        assert!(!document_signature_ok(FormatId::Rtf, b"plain"));
        // Text: UTF-8 only, no NUL; a cut character at the end is fine.
        assert!(document_signature_ok(FormatId::Txt, "h\u{e9}llo".as_bytes()));
        let cut = &"caf\u{e9}".as_bytes()[..4];
        assert!(document_signature_ok(FormatId::Md, cut));
        assert!(!document_signature_ok(FormatId::Csv, &[0xFF, 0xFE, 0x00, 0x41]));
        assert!(!document_signature_ok(FormatId::Html, b"a\0b"));
        assert!(!document_signature_ok(FormatId::Txt, &[b'a', 0xC3, b'a']));
        assert!(!document_signature_ok(FormatId::Txt, b""));
        assert!(!document_signature_ok(FormatId::Png, b"text"));
        // signature_ok covers both kinds.
        assert!(signature_ok(FormatId::Png, &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]));
        assert!(signature_ok(FormatId::Docx, b"PK\x03\x04"));
        assert!(!signature_ok(FormatId::Jpeg, b"%PDF-"));
    }

    #[test]
    fn rejects_unknown_and_short() {
        assert_eq!(detect_signature(&[]), None);
        assert_eq!(detect_signature(&[0xFF]), None);
        assert_eq!(detect_signature(b"hello world, plain text"), None);
        assert_eq!(detect_signature(&ftyp(b"zzzz")), None);
        assert_eq!(detect_signature(&riff(b"XXXX")), None);
    }

    #[test]
    fn extension_mapping() {
        assert_eq!(FormatId::from_extension("JPG"), Some(FormatId::Jpeg));
        assert_eq!(FormatId::from_extension("jpeg"), Some(FormatId::Jpeg));
        assert_eq!(FormatId::from_extension("JFIF"), Some(FormatId::Jpeg));
        assert_eq!(FormatId::from_extension(".Tif"), Some(FormatId::Tiff));
        assert_eq!(FormatId::from_extension("tiff"), Some(FormatId::Tiff));
        assert_eq!(FormatId::from_extension("HEIF"), Some(FormatId::Heif));
        assert_eq!(FormatId::from_extension("m4a"), Some(FormatId::M4a));
        assert_eq!(FormatId::from_extension("aac"), None);
        assert_eq!(FormatId::from_extension("PDF"), Some(FormatId::Pdf));
        assert_eq!(FormatId::from_extension("htm"), Some(FormatId::Html));
        assert_eq!(FormatId::from_extension("markdown"), Some(FormatId::Md));
        assert_eq!(FormatId::from_extension("text"), Some(FormatId::Txt));
        assert_eq!(FormatId::from_extension(""), None);
        assert_eq!(FormatId::from_extension("toolongextension"), None);
        assert_eq!(FormatId::from_extension("exe"), None);
    }

    #[test]
    fn canonical_extension_round_trips() {
        for f in FormatId::ALL {
            assert_eq!(FormatId::from_extension(f.extension()), Some(f), "{f:?}");
            assert!(!f.label().is_empty());
        }
        assert_eq!(FormatId::Jpeg.label(), "JPG");
        assert_eq!(FormatId::Jpeg.extension(), "jpg");
    }

    #[test]
    fn categories() {
        assert_eq!(FormatId::Gif.category(), Category::Image);
        assert_eq!(FormatId::Mkv.category(), Category::Video);
        assert_eq!(FormatId::Opus.category(), Category::Audio);
        for f in &FormatId::ALL[20..] {
            assert_eq!(f.category(), Category::Document, "{f:?}");
        }
        assert_eq!(FormatId::ALL.len(), 36);
    }

    #[test]
    fn compatibility() {
        use FormatId::*;
        assert!(signature_compatible(Png, Png));
        assert!(signature_compatible(Mkv, Webm));
        assert!(signature_compatible(Webm, Mkv));
        assert!(signature_compatible(Heif, Heic));
        assert!(signature_compatible(Heic, Avif));
        assert!(signature_compatible(Mp4, Mov));
        assert!(signature_compatible(M4a, Mp4));
        assert!(signature_compatible(Ogg, Opus));
        assert!(!signature_compatible(Png, Jpeg));
        assert!(!signature_compatible(Mp4, Mkv));
        assert!(!signature_compatible(Mp3, Wav));
        assert!(!signature_compatible(Jpeg, Heic));
    }
}
