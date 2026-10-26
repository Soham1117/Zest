//! `zest-worker`: PDF jobs for the Zest broker, using PDFium through `pdfium-render`.
//!
//! ```text
//! zest-worker --version
//! zest-worker [--pdfium <pdfium.dll>] pdf-to-images --format png|jpg [--dpi 150] [--quality 90] <in.pdf> <out_dir>
//! zest-worker [--pdfium <pdfium.dll>] merge-pdf <out.pdf> <in1.pdf> <in2.pdf> ...
//! ```
//!
//! `pdf-to-images` writes one image per page (`name-p001.png`, ...) and prints
//! `progress <percent>` lines on stdout. Errors go to stderr, exit code 1.
//! Usage errors give exit code 2.

use image::codecs::jpeg::JpegEncoder;
use image::{ExtendedColorType, ImageEncoder};
use pdfium_render::prelude::*;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Most pages one PDF may have for `pdf-to-images`.
const MAX_PAGES: usize = 500;
/// Most pixels one rendered page may have (50 megapixels).
const MAX_PIXELS: u64 = 50_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fmt {
    Png,
    Jpg,
}

impl Fmt {
    fn ext(self) -> &'static str {
        match self {
            Fmt::Png => "png",
            Fmt::Jpg => "jpg",
        }
    }
}

#[derive(Debug)]
enum Cmd {
    Version,
    ToImages {
        fmt: Fmt,
        dpi: u32,
        quality: u8,
        /// Optional 1-based inclusive range `"1-3"` / `"2"`.
        pages: Option<String>,
        input: PathBuf,
        out_dir: PathBuf,
    },
    Merge {
        out: PathBuf,
        inputs: Vec<PathBuf>,
    },
}

#[derive(Debug)]
struct Parsed {
    pdfium: Option<PathBuf>,
    cmd: Cmd,
}

fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut it = args.iter().peekable();
    let mut pdfium = None;
    let mut cmd_name = None;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--version" | "-V" => {
                return Ok(Parsed {
                    pdfium,
                    cmd: Cmd::Version,
                })
            }
            "--pdfium" => {
                pdfium = Some(PathBuf::from(it.next().ok_or("--pdfium needs a path")?));
            }
            other => {
                cmd_name = Some(other.to_owned());
                break;
            }
        }
    }
    let rest: Vec<&String> = it.collect();
    match cmd_name.as_deref() {
        Some("pdf-to-images") => {
            let mut fmt = None;
            let mut dpi = 150u32;
            let mut quality = 90u8;
            let mut pages = None;
            let mut pos: Vec<PathBuf> = Vec::new();
            let mut i = 0;
            while i < rest.len() {
                let a = rest[i].as_str();
                let mut value = |name: &str| -> Result<&str, String> {
                    i += 1;
                    rest.get(i)
                        .map(|s| s.as_str())
                        .ok_or_else(|| format!("{name} needs a value"))
                };
                match a {
                    "--format" => {
                        fmt = Some(match value("--format")? {
                            "png" => Fmt::Png,
                            "jpg" | "jpeg" => Fmt::Jpg,
                            o => return Err(format!("unknown format \"{o}\"")),
                        })
                    }
                    "--dpi" => {
                        dpi = value("--dpi")?.parse().map_err(|_| "bad --dpi".to_owned())?;
                        if !(10..=1200).contains(&dpi) {
                            return Err("--dpi must be 10 to 1200".to_owned());
                        }
                    }
                    "--quality" => {
                        let q: u32 = value("--quality")?.parse().map_err(|_| "bad --quality".to_owned())?;
                        quality = q.clamp(1, 100) as u8;
                    }
                    "--pages" => {
                        pages = Some(value("--pages")?.to_owned());
                    }
                    _ => pos.push(PathBuf::from(a)),
                }
                i += 1;
            }
            let [input, out_dir] = <[PathBuf; 2]>::try_from(pos)
                .map_err(|_| "pdf-to-images needs <in.pdf> <out_dir>".to_owned())?;
            Ok(Parsed {
                pdfium,
                cmd: Cmd::ToImages {
                    fmt: fmt.ok_or("--format is required")?,
                    dpi,
                    quality,
                    pages,
                    input,
                    out_dir,
                },
            })
        }
        Some("merge-pdf") => {
            let mut paths = rest.iter().map(|s| PathBuf::from(s.as_str()));
            let out = paths.next().ok_or("merge-pdf needs <out.pdf> <in.pdf>...")?;
            let inputs: Vec<PathBuf> = paths.collect();
            if inputs.len() < 2 {
                return Err("merge-pdf needs at least two input files".to_owned());
            }
            Ok(Parsed {
                pdfium,
                cmd: Cmd::Merge { out, inputs },
            })
        }
        Some(other) => Err(format!("unknown command \"{other}\"")),
        None => Err("no command".to_owned()),
    }
}

/// Find `pdfium.dll`: explicit path, next to the exe, `pdfium\bin` next to the exe,
/// then `third_party\bin\pdfium\bin` in the exe folder and its parents.
fn find_pdfium(explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(p) = explicit {
        return if p.is_file() {
            Ok(p.to_path_buf())
        } else {
            Err(format!("PDFium library not found: {}", p.display()))
        };
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut dir = exe.parent();
    while let Some(d) = dir {
        for rel in ["pdfium.dll", r"pdfium\bin\pdfium.dll", r"third_party\bin\pdfium\bin\pdfium.dll"] {
            let c = d.join(rel);
            if c.is_file() {
                return Ok(c);
            }
        }
        dir = d.parent();
    }
    Err("PDFium library (pdfium.dll) not found. Run tools\\fetch-deps.ps1.".to_owned())
}

fn bind(explicit: Option<&Path>) -> Result<Pdfium, String> {
    let dll = find_pdfium(explicit)?;
    let bindings = Pdfium::bind_to_library(&dll)
        .map_err(|e| format!("Cannot load {}: {e}", dll.display()))?;
    Ok(Pdfium::new(bindings))
}

fn open_error(e: PdfiumError) -> String {
    if matches!(
        e,
        PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError)
    ) {
        "The PDF is password protected".to_owned()
    } else {
        format!("Cannot open the PDF: {e}")
    }
}

fn progress(percent: f32) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "progress {percent:.1}");
    let _ = out.flush();
}

/// Pixel size of a page at `dpi`.
fn pixel_size(width_pt: f32, height_pt: f32, dpi: u32) -> (u32, u32) {
    let scale = dpi as f32 / 72.0;
    (
        ((width_pt * scale).round() as u32).max(1),
        ((height_pt * scale).round() as u32).max(1),
    )
}

fn page_file_name(stem: &str, page: usize, total: usize, ext: &str) -> String {
    let width = total.to_string().len().max(3);
    format!("{stem}-p{page:0width$}.{ext}")
}

fn parse_page_range(spec: &str, total: usize) -> Result<Vec<usize>, String> {
    let mut out = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((a, b)) = part.split_once('-') {
            let start: usize = a.trim().parse().map_err(|_| format!("bad page range \"{spec}\""))?;
            let end: usize = b.trim().parse().map_err(|_| format!("bad page range \"{spec}\""))?;
            if start == 0 || end == 0 || start > end || end > total {
                return Err(format!("page range \"{spec}\" is outside 1..{total}"));
            }
            out.extend((start - 1)..end);
        } else {
            let n: usize = part.parse().map_err(|_| format!("bad page \"{part}\""))?;
            if n == 0 || n > total {
                return Err(format!("page {n} is outside 1..{total}"));
            }
            out.push(n - 1);
        }
    }
    if out.is_empty() {
        return Err("empty --pages".to_owned());
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

fn pdf_to_images(
    pdfium: &Pdfium,
    fmt: Fmt,
    dpi: u32,
    quality: u8,
    pages_spec: Option<&str>,
    input: &Path,
    out_dir: &Path,
) -> Result<(), String> {
    let doc = pdfium.load_pdf_from_file(input, None).map_err(open_error)?;
    let pages = doc.pages();
    let total = pages.len() as usize;
    if total == 0 {
        return Err("The PDF has no pages".to_owned());
    }
    if total > MAX_PAGES {
        return Err(format!("The PDF has {total} pages; the limit is {MAX_PAGES}"));
    }
    let indices: Vec<usize> = match pages_spec {
        Some(s) => parse_page_range(s, total)?,
        None => (0..total).collect(),
    };
    // Check every selected page size first, so a too large page fails before any work.
    let mut sizes = Vec::with_capacity(indices.len());
    for &i in &indices {
        let page = pages.get(i as i32).map_err(|e| format!("Cannot read page {}: {e}", i + 1))?;
        let (w, h) = pixel_size(page.width().value, page.height().value, dpi);
        if u64::from(w) * u64::from(h) > MAX_PIXELS {
            return Err(format!(
                "Page {} is too large at {dpi} DPI ({w} x {h} pixels; the limit is 50 megapixels)",
                i + 1
            ));
        }
        sizes.push((w, h));
    }
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "page".to_owned());
    for (k, &i) in indices.iter().enumerate() {
        let page = pages.get(i as i32).map_err(|e| format!("Cannot read page {}: {e}", i + 1))?;
        let (w, h) = sizes[k];
        let cfg = PdfRenderConfig::new()
            .set_clear_color(PdfColor::WHITE)
            .set_target_size(w as i32, h as i32);
        let bitmap = page
            .render_with_config(&cfg)
            .map_err(|e| format!("Cannot render page {}: {e}", i + 1))?;
        let rgb = bitmap
            .as_image()
            .map_err(|e| format!("Cannot read page {}: {e}", i + 1))?
            .to_rgb8();
        let path = out_dir.join(page_file_name(&stem, i + 1, total, fmt.ext()));
        let file = File::create(&path).map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
        let mut w_out = BufWriter::new(file);
        match fmt {
            Fmt::Png => image::codecs::png::PngEncoder::new(&mut w_out)
                .write_image(rgb.as_raw(), rgb.width(), rgb.height(), ExtendedColorType::Rgb8),
            Fmt::Jpg => JpegEncoder::new_with_quality(&mut w_out, quality).write_image(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                ExtendedColorType::Rgb8,
            ),
        }
        .map_err(|e| format!("Cannot encode page {}: {e}", i + 1))?;
        w_out
            .flush()
            .map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
        progress((k + 1) as f32 * 100.0 / indices.len().max(1) as f32);
    }
    Ok(())
}

fn merge(pdfium: &Pdfium, out: &Path, inputs: &[PathBuf]) -> Result<(), String> {
    let mut target = pdfium
        .create_new_pdf()
        .map_err(|e| format!("Cannot create the PDF: {e}"))?;
    for (i, path) in inputs.iter().enumerate() {
        let doc = pdfium
            .load_pdf_from_file(path, None)
            .map_err(|e| format!("{}: {}", path.display(), open_error(e)))?;
        target
            .pages_mut()
            .append(&doc)
            .map_err(|e| format!("Cannot add {}: {e}", path.display()))?;
        progress((i + 1) as f32 * 100.0 / inputs.len() as f32);
    }
    target
        .save_to_file(out)
        .map_err(|e| format!("Cannot save the merged PDF: {e}"))
}

fn run(args: &[String]) -> Result<(), (u8, String)> {
    let parsed = parse(args).map_err(|e| (2, e))?;
    match parsed.cmd {
        Cmd::Version => {
            println!("zest-worker {} (PDFium)", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Cmd::ToImages {
            fmt,
            dpi,
            quality,
            pages,
            input,
            out_dir,
        } => {
            let pdfium = bind(parsed.pdfium.as_deref()).map_err(|e| (1, e))?;
            pdf_to_images(&pdfium, fmt, dpi, quality, pages.as_deref(), &input, &out_dir)
                .map_err(|e| (1, e))
        }
        Cmd::Merge { out, inputs } => {
            let pdfium = bind(parsed.pdfium.as_deref()).map_err(|e| (1, e))?;
            merge(&pdfium, &out, &inputs).map_err(|e| (1, e))
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, msg)) => {
            eprintln!("error: {msg}");
            ExitCode::from(code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_pdf_to_images() {
        let p = parse(&s(&["pdf-to-images", "--format", "jpg", "--dpi", "200", "--quality", "80", "a.pdf", "out"]))
            .unwrap();
        match p.cmd {
            Cmd::ToImages { fmt, dpi, quality, pages: _, input, out_dir } => {
                assert_eq!(fmt, Fmt::Jpg);
                assert_eq!(dpi, 200);
                assert_eq!(quality, 80);
                assert_eq!(input, PathBuf::from("a.pdf"));
                assert_eq!(out_dir, PathBuf::from("out"));
            }
            _ => panic!(),
        }
        assert!(parse(&s(&["pdf-to-images", "a.pdf", "out"])).is_err()); // no --format
        assert!(parse(&s(&["pdf-to-images", "--format", "gif", "a", "b"])).is_err());
        assert!(parse(&s(&["pdf-to-images", "--format", "png", "--dpi", "5", "a", "b"])).is_err());
    }

    #[test]
    fn parses_merge_and_version() {
        assert!(matches!(parse(&s(&["--version"])).unwrap().cmd, Cmd::Version));
        let p = parse(&s(&["--pdfium", "x.dll", "merge-pdf", "o.pdf", "a.pdf", "b.pdf"])).unwrap();
        assert_eq!(p.pdfium, Some(PathBuf::from("x.dll")));
        assert!(matches!(p.cmd, Cmd::Merge { ref inputs, .. } if inputs.len() == 2));
        assert!(parse(&s(&["merge-pdf", "o.pdf", "a.pdf"])).is_err());
        assert!(parse(&s(&["nope"])).is_err());
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn page_names_and_sizes() {
        assert_eq!(page_file_name("doc", 1, 12, "png"), "doc-p001.png");
        assert_eq!(page_file_name("doc", 7, 1200, "jpg"), "doc-p0007.jpg");
        assert_eq!(pixel_size(612.0, 792.0, 72), (612, 792));
        assert_eq!(pixel_size(612.0, 792.0, 150), (1275, 1650));
    }
}
