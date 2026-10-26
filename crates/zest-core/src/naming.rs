//! Output path rules (spec 2.4): never overwrite, never equal the source.

use crate::format::FormatId;
use crate::ops::{ActionKind, Operation};
use std::path::{Path, PathBuf};

/// Suffix of in-progress output files.
pub const TEMP_SUFFIX: &str = ".zest-tmp";
/// Highest `(n)` counter tried.
pub const MAX_COLLISION: u32 = 9999;

fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").to_lowercase();
    norm(a) == norm(b)
}

/// Choose the output path for `src` converted to `target`.
/// `photo.heic` -> `photo.jpg`, then `photo (1).jpg`, `photo (2).jpg`, ...
/// `exists` tells whether a path is taken. The result never equals `src`
/// (compared case-insensitive, as on Windows). If all 9999 names are taken, the last name is returned.
pub fn output_path(
    src: &Path,
    target: FormatId,
    out_dir: Option<&Path>,
    exists: impl Fn(&Path) -> bool,
) -> PathBuf {
    output_path_for(src, Operation::Convert(target), out_dir, exists)
}

/// Preferred output path with no collision suffix (`photo.jpg`), never equal to `src`.
pub fn preferred_output_path(src: &Path, op: Operation, out_dir: Option<&Path>) -> PathBuf {
    output_path_for(src, op, out_dir, |_| false)
}

fn dir_of(src: &Path, out_dir: Option<&Path>) -> PathBuf {
    match out_dir {
        Some(d) => d.to_path_buf(),
        None => src.parent().map(Path::to_path_buf).unwrap_or_default(),
    }
}

/// Pick the first free `<stem><suffix>.<ext>`, then `<stem><suffix> (n).<ext>`.
fn first_free(
    src: &Path,
    dir: &Path,
    stem: &str,
    ext: &str,
    exists: impl Fn(&Path) -> bool,
) -> PathBuf {
    let name = |n: u32| {
        let counter = if n == 0 { String::new() } else { format!(" ({n})") };
        if ext.is_empty() {
            format!("{stem}{counter}")
        } else {
            format!("{stem}{counter}.{ext}")
        }
    };
    let mut candidate = dir.join(name(0));
    for n in 0..=MAX_COLLISION {
        candidate = dir.join(name(n));
        if !same_path(&candidate, src) && !exists(&candidate) {
            break;
        }
    }
    candidate
}

/// Choose the output path for `src` and an operation.
///
/// - `Convert(f)`: `photo.heic` -> `photo.jpg`.
/// - `Tool(t)`: a suffix is added and the format is kept: `photo-compressed.jpg`,
///   `photo-50pct.jpg`, `photo-1920.jpg`, `photo-stripped.jpg`, `photo-rotated.jpg`,
///   `clip-10MB.mp4`. Video compress and target size always write `.mp4`.
///   If the source extension is not a known format, it is kept as it is.
/// - `Action(_)`: `<stem>.zip` (only Zip creates a file).
///
/// A taken name gets ` (1)`, ` (2)`, ... The result never equals `src`.
pub fn output_path_for(
    src: &Path,
    op: Operation,
    out_dir: Option<&Path>,
    exists: impl Fn(&Path) -> bool,
) -> PathBuf {
    let dir = dir_of(src, out_dir);
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".to_owned());
    match op {
        Operation::Convert(target) => first_free(src, &dir, &stem, target.extension(), exists),
        Operation::Tool(tool) => {
            let src_ext = src.extension().map(|e| e.to_string_lossy().into_owned());
            let ext = match src_ext.as_deref().and_then(FormatId::from_extension) {
                Some(f) => {
                    let out = tool.output_format(f);
                    if out == f {
                        // Same format: keep the extension as the user wrote it.
                        src_ext.clone().unwrap_or_else(|| out.extension().to_owned())
                    } else {
                        out.extension().to_owned()
                    }
                }
                None => src_ext.unwrap_or_default(),
            };
            first_free(src, &dir, &format!("{stem}-{}", tool.suffix()), &ext, exists)
        }
        Operation::Action(ActionKind::CopyAs(target)) => {
            first_free(src, &dir, &stem, target.extension(), exists)
        }
        Operation::Action(_) => first_free(src, &dir, &stem, "zip", exists),
    }
}

/// Folder for the page images of a PDF: `<stem> (pages)`, then `<stem> (pages) (1)`, ...
/// The result never equals `src`.
pub fn pages_dir_path(
    src: &Path,
    out_dir: Option<&Path>,
    exists: impl Fn(&Path) -> bool,
) -> PathBuf {
    let dir = dir_of(src, out_dir);
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".to_owned());
    first_free(src, &dir, &format!("{stem} (pages)"), "", exists)
}

/// Archive path for the Zip action: next to the first item (or in `out_dir`).
/// One item: `<name>.zip` (a file loses its extension, a folder keeps its whole name).
/// Several items: `<parent folder name>.zip`, or `Archive.zip` if the parent has no name.
/// `is_dir` tells whether an item is a folder.
pub fn zip_output_path(
    items: &[PathBuf],
    is_dir: impl Fn(&Path) -> bool,
    out_dir: Option<&Path>,
    exists: impl Fn(&Path) -> bool,
) -> PathBuf {
    let Some(first) = items.first() else {
        return dir_of(Path::new(""), out_dir).join("Archive.zip");
    };
    let dir = dir_of(first, out_dir);
    let name_of = |p: &Path| p.file_name().map(|s| s.to_string_lossy().into_owned());
    let stem = if items.len() == 1 {
        if is_dir(first) {
            name_of(first)
        } else {
            first.file_stem().map(|s| s.to_string_lossy().into_owned())
        }
    } else {
        first.parent().and_then(name_of)
    }
    .filter(|s| !s.is_empty())
    .unwrap_or_else(|| "Archive".to_owned());
    // Never equal to any item (zipping `x.zip` alone must not pick `x.zip`).
    first_free(first, &dir, &stem, "zip", |c| {
        exists(c) || items.iter().any(|i| same_path(c, i))
    })
}

/// Temp file next to the final output: `<file name>.zest-tmp`.
pub fn temp_path(final_path: &Path) -> PathBuf {
    let mut name = final_path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    name.push(TEMP_SUFFIX);
    final_path.with_file_name(name)
}

/// True if the file name ends with `.zest-tmp` (any case).
pub fn is_zest_temp(path: &Path) -> bool {
    path.file_name()
        .map(|n| n.to_string_lossy().to_lowercase().ends_with(TEMP_SUFFIX))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::ToolKind;

    #[test]
    fn pages_folder_copy_as_and_merge_names() {
        let src = Path::new(r"C:\a\Report.pdf");
        let base = PathBuf::from(r"C:\a");
        assert_eq!(pages_dir_path(src, None, |_| false), base.join("Report (pages)"));
        let taken = [base.join("Report (pages)")];
        assert_eq!(
            pages_dir_path(src, None, |p| taken.iter().any(|t| t == p)),
            base.join("Report (pages) (1)")
        );
        let out = Path::new(r"C:\clip\1");
        let copy_png = Operation::Action(ActionKind::CopyAs(FormatId::Png));
        assert_eq!(
            output_path_for(Path::new(r"C:\a\p.heic"), copy_png, Some(out), |_| false),
            out.join("p.png")
        );
        let merge = Operation::Tool(crate::ops::ToolKind::MergePdf);
        assert_eq!(output_path_for(src, merge, None, |_| false), base.join("Report-merged.pdf"));
        let to_pdf = Operation::Convert(FormatId::Pdf);
        assert_eq!(
            output_path_for(Path::new(r"C:\a\x.docx"), to_pdf, None, |_| false),
            base.join("x.pdf")
        );
    }

    #[test]
    fn basic_rename() {
        let p = output_path(Path::new(r"C:\a\photo.heic"), FormatId::Jpeg, None, |_| false);
        assert_eq!(p, PathBuf::from(r"C:\a").join("photo.jpg"));
    }

    #[test]
    fn collisions_count_up() {
        let base = PathBuf::from(r"C:\a");
        let t = [base.join("photo.jpg"), base.join("photo (1).jpg")];
        let ex = |p: &Path| t.iter().any(|x| x == p);
        let p = output_path(Path::new(r"C:\a\photo.heic"), FormatId::Jpeg, None, ex);
        assert_eq!(p, base.join("photo (2).jpg"));
    }

    #[test]
    fn never_equals_source_case_insensitive() {
        let p = output_path(Path::new(r"C:\a\Photo.JPG"), FormatId::Jpeg, None, |_| false);
        assert_eq!(p, PathBuf::from(r"C:\a").join("Photo (1).jpg"));
        let p = output_path(
            Path::new(r"C:\a\photo.jpg"),
            FormatId::Jpeg,
            Some(Path::new(r"C:\A")),
            |_| false,
        );
        assert_eq!(p, PathBuf::from(r"C:\A").join("photo (1).jpg"));
    }

    #[test]
    fn out_dir_is_used() {
        let p = output_path(
            Path::new(r"C:\a\clip.mov"),
            FormatId::Mp4,
            Some(Path::new(r"D:\out")),
            |_| false,
        );
        assert_eq!(p, PathBuf::from(r"D:\out").join("clip.mp4"));
    }

    #[test]
    fn keeps_dots_in_stem_and_handles_no_ext() {
        let p = output_path(Path::new(r"C:\a\my.photo.v2.png"), FormatId::Webp, None, |_| false);
        assert_eq!(p.file_name().unwrap(), "my.photo.v2.webp");
        let p = output_path(Path::new(r"C:\a\noext"), FormatId::Png, None, |_| false);
        assert_eq!(p.file_name().unwrap(), "noext.png");
    }

    #[test]
    fn gives_up_at_9999() {
        let p = output_path(Path::new(r"C:\a\x.png"), FormatId::Jpeg, None, |_| true);
        assert_eq!(p.file_name().unwrap(), "x (9999).jpg");
    }

    #[test]
    fn temp_paths() {
        let t = temp_path(Path::new(r"C:\a\photo.jpg"));
        assert_eq!(t, PathBuf::from(r"C:\a").join("photo.jpg.zest-tmp"));
        assert!(is_zest_temp(&t));
        assert!(is_zest_temp(Path::new(r"C:\a\X.JPG.ZEST-TMP")));
        assert!(!is_zest_temp(Path::new(r"C:\a\photo.jpg")));
    }

    fn tool_name(src: &str, t: ToolKind) -> String {
        output_path_for(Path::new(src), Operation::Tool(t), None, |_| false)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn tool_names() {
        assert_eq!(tool_name(r"C:\a\photo.jpg", ToolKind::Compress), "photo-compressed.jpg");
        assert_eq!(tool_name(r"C:\a\photo.jpg", ToolKind::Resize50), "photo-50pct.jpg");
        assert_eq!(tool_name(r"C:\a\photo.jpg", ToolKind::Resize25), "photo-25pct.jpg");
        assert_eq!(tool_name(r"C:\a\photo.jpg", ToolKind::MaxEdge1920), "photo-1920.jpg");
        assert_eq!(tool_name(r"C:\a\photo.jpg", ToolKind::MaxEdge1280), "photo-1280.jpg");
        assert_eq!(tool_name(r"C:\a\photo.jpg", ToolKind::StripMetadata), "photo-stripped.jpg");
        assert_eq!(tool_name(r"C:\a\photo.png", ToolKind::RotateCw), "photo-rotated.png");
        assert_eq!(tool_name(r"C:\a\photo.png", ToolKind::Rotate180), "photo-rotated.png");
        assert_eq!(tool_name(r"C:\a\clip.mp4", ToolKind::VideoTargetSize(10)), "clip-10MB.mp4");
        // Video compress writes MP4. Strip keeps the container.
        assert_eq!(tool_name(r"C:\a\clip.mkv", ToolKind::VideoTargetSize(25)), "clip-25MB.mp4");
        assert_eq!(tool_name(r"C:\a\clip.mov", ToolKind::Compress), "clip-compressed.mp4");
        assert_eq!(tool_name(r"C:\a\clip.mkv", ToolKind::StripMetadata), "clip-stripped.mkv");
        // Extension text is kept; dots in the stem are kept.
        assert_eq!(tool_name(r"C:\a\Photo.JPEG", ToolKind::Compress), "Photo-compressed.JPEG");
        assert_eq!(tool_name(r"C:\a\my.photo.v2.png", ToolKind::Resize50), "my.photo.v2-50pct.png");
        assert_eq!(tool_name(r"C:\a\song.mp3", ToolKind::Compress), "song-compressed.mp3");
    }

    #[test]
    fn tool_collisions_and_never_source() {
        let base = PathBuf::from(r"C:\a");
        let t = [base.join("photo-compressed.jpg"), base.join("photo-compressed (1).jpg")];
        let ex = |p: &Path| t.iter().any(|x| x == p);
        let p = output_path_for(
            Path::new(r"C:\a\photo.jpg"),
            Operation::Tool(ToolKind::Compress),
            None,
            ex,
        );
        assert_eq!(p, base.join("photo-compressed (2).jpg"));
        // Source already named like the output.
        let p = output_path_for(
            Path::new(r"C:\a\Photo-Compressed.JPG"),
            Operation::Tool(ToolKind::Compress),
            None,
            |_| false,
        );
        assert_ne!(p, PathBuf::from(r"C:\a\Photo-Compressed.JPG"));
        let p = output_path_for(
            Path::new(r"C:\a\x-stripped.png"),
            Operation::Tool(ToolKind::StripMetadata),
            Some(Path::new(r"D:\o")),
            |_| false,
        );
        assert_eq!(p, PathBuf::from(r"D:\o").join("x-stripped-stripped.png"));
        // Same folder, same name as the source: counter.
        let p = output_path_for(
            Path::new(r"C:\a\photo-compressed.jpg"),
            Operation::Tool(ToolKind::Compress),
            None,
            |_| false,
        );
        assert_eq!(p, base.join("photo-compressed-compressed.jpg"));
    }

    #[test]
    fn unknown_extension_is_kept_for_tools() {
        assert_eq!(tool_name(r"C:\a\data.xyz", ToolKind::Compress), "data-compressed.xyz");
        assert_eq!(tool_name(r"C:\a\noext", ToolKind::Compress), "noext-compressed");
    }

    #[test]
    fn convert_wrapper_matches_output_path_for() {
        let a = output_path(Path::new(r"C:\a\p.heic"), FormatId::Jpeg, None, |_| false);
        let b = output_path_for(Path::new(r"C:\a\p.heic"), Operation::Convert(FormatId::Jpeg), None, |_| false);
        assert_eq!(a, b);
    }

    #[test]
    fn zip_names() {
        let no_dir = |_: &Path| false;
        let one = [PathBuf::from(r"C:\a\report.pdf")];
        assert_eq!(
            zip_output_path(&one, no_dir, None, |_| false),
            PathBuf::from(r"C:\a\report.zip")
        );
        let folder = [PathBuf::from(r"C:\a\my.photos")];
        assert_eq!(
            zip_output_path(&folder, |_| true, None, |_| false),
            PathBuf::from(r"C:\a\my.photos.zip")
        );
        let many = [PathBuf::from(r"C:\a\Trip\x.png"), PathBuf::from(r"C:\a\Trip\y.png")];
        assert_eq!(
            zip_output_path(&many, no_dir, None, |_| false),
            PathBuf::from(r"C:\a\Trip\Trip.zip")
        );
        let root = [PathBuf::from(r"C:\x.png"), PathBuf::from(r"C:\y.png")];
        assert_eq!(
            zip_output_path(&root, no_dir, None, |_| false),
            PathBuf::from(r"C:\Archive.zip")
        );
        // Collision and out_dir.
        let taken = PathBuf::from(r"D:\o\report.zip");
        assert_eq!(
            zip_output_path(&one, no_dir, Some(Path::new(r"D:\o")), |p| p == taken),
            PathBuf::from(r"D:\o\report (1).zip")
        );
        // A lone zip file does not get its own name.
        let z = [PathBuf::from(r"C:\a\x.zip")];
        assert_eq!(
            zip_output_path(&z, no_dir, None, |_| false),
            PathBuf::from(r"C:\a\x (1).zip")
        );
        let none: [PathBuf; 0] = [];
        assert_eq!(zip_output_path(&none, no_dir, None, |_| false), PathBuf::from("Archive.zip"));
    }
}
