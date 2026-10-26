//! Optional document engines: LibreOffice (`soffice --headless --convert-to`) and Pandoc.
//!
//! Both are only used when installed. Each job gets its own LibreOffice user
//! profile inside the scratch folder, so jobs can run side by side and Zest never
//! touches the profile of a running LibreOffice.

use crate::adapter::{os, safe_arg, Adapter, CommandSpec, PlanError, PlanOptions};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use zest_core::{EngineId, FormatId, JobRequest, Operation};

/// Time allowed for one LibreOffice or Pandoc run.
pub const OFFICE_TIMEOUT: Duration = Duration::from_secs(120);

/// Folder inside the scratch folder where LibreOffice writes its result.
const OUT_SUBDIR: &str = "out";
/// LibreOffice user profile inside the scratch folder.
const PROFILE_SUBDIR: &str = "profile";

/// `file:///C:/Users/me/AppData/Local/Zest/tmp/1-2/profile` with unsafe bytes as `%XX`.
pub(crate) fn file_url(p: &Path) -> String {
    let text = p.to_string_lossy().replace('\\', "/");
    let mut out = String::from("file:///");
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The `--convert-to` value for an output format.
fn lo_filter(to: FormatId) -> Option<&'static str> {
    Some(match to {
        FormatId::Pdf => "pdf",
        FormatId::Docx => "docx",
        FormatId::Odt => "odt",
        FormatId::Pptx => "pptx",
        FormatId::Xlsx => "xlsx",
        // Comma, double quote, UTF-8, first line is a header.
        FormatId::Csv => "csv:Text - txt - csv (StarCalc):44,34,76,1",
        _ => return None,
    })
}

pub struct LibreOfficeAdapter {
    program: PathBuf,
}

impl LibreOfficeAdapter {
    pub fn new(program: PathBuf) -> Self {
        LibreOfficeAdapter { program }
    }

    fn out_dir(scratch: &Path) -> PathBuf {
        scratch.join(OUT_SUBDIR)
    }
}

impl Adapter for LibreOfficeAdapter {
    fn engine(&self) -> EngineId {
        EngineId::LibreOffice
    }

    fn plan(
        &self,
        req: &JobRequest,
        input: &Path,
        _tmp_out: &Path,
        opts: &PlanOptions,
    ) -> Result<Vec<CommandSpec>, PlanError> {
        let from = opts.input_format;
        let Operation::Convert(to) = req.op else {
            return Err(PlanError::Other("not a conversion".to_owned()));
        };
        let filter = lo_filter(to).ok_or(PlanError::Unsupported { from, to })?;
        let scratch = opts
            .scratch_dir
            .as_ref()
            .ok_or_else(|| PlanError::Other("no scratch folder".to_owned()))?;
        let profile = scratch.join(PROFILE_SUBDIR);
        let args: Vec<OsString> = vec![
            os("--headless"),
            os("--norestore"),
            os("--nologo"),
            os(&format!("-env:UserInstallation={}", file_url(&profile))),
            os("--convert-to"),
            os(filter),
            os("--outdir"),
            Self::out_dir(scratch).into_os_string(),
            safe_arg(input)?,
        ];
        Ok(vec![CommandSpec {
            program: self.program.clone(),
            args,
        }])
    }

    fn parse_progress(&self, _line: &str, _duration: Option<f64>) -> Option<f32> {
        None
    }

    fn needs_scratch(&self, _req: &JobRequest, _input_format: FormatId) -> bool {
        true
    }

    fn timeout(&self) -> Option<Duration> {
        Some(OFFICE_TIMEOUT)
    }

    fn finish(
        &self,
        req: &JobRequest,
        _input: &Path,
        tmp_out: &Path,
        opts: &PlanOptions,
    ) -> Result<(), String> {
        let Operation::Convert(to) = req.op else {
            return Err("not a conversion".to_owned());
        };
        let scratch = opts.scratch_dir.as_ref().ok_or("no scratch folder")?;
        let dir = Self::out_dir(scratch);
        let found = fs::read_dir(&dir)
            .map_err(|_| "LibreOffice did not write a result".to_owned())?
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case(to.extension()))
            })
            .ok_or_else(|| "LibreOffice did not write a result".to_owned())?;
        move_over(&found, tmp_out).map_err(|e| format!("Cannot store the result: {e}"))
    }
}

/// Move `from` onto the existing file `to`; copy and delete across volumes.
fn move_over(from: &Path, to: &Path) -> std::io::Result<()> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    fs::copy(from, to)?;
    let _ = fs::remove_file(from);
    Ok(())
}

pub struct PandocAdapter {
    program: PathBuf,
}

impl PandocAdapter {
    pub fn new(program: PathBuf) -> Self {
        PandocAdapter { program }
    }
}

/// Pandoc reader name for an input format.
fn pandoc_reader(f: FormatId) -> Option<&'static str> {
    Some(match f {
        FormatId::Md => "markdown",
        FormatId::Docx => "docx",
        FormatId::Html => "html",
        FormatId::Epub => "epub",
        _ => return None,
    })
}

/// Pandoc writer name and extra options for an output format.
fn pandoc_writer(f: FormatId) -> Option<(&'static str, &'static [&'static str])> {
    Some(match f {
        FormatId::Docx => ("docx", &[]),
        FormatId::Odt => ("odt", &[]),
        FormatId::Epub => ("epub", &[]),
        FormatId::Html => ("html", &["--standalone"]),
        FormatId::Md => ("gfm", &["--wrap=none"]),
        _ => return None,
    })
}

impl Adapter for PandocAdapter {
    fn engine(&self) -> EngineId {
        EngineId::Pandoc
    }

    fn plan(
        &self,
        req: &JobRequest,
        input: &Path,
        tmp_out: &Path,
        opts: &PlanOptions,
    ) -> Result<Vec<CommandSpec>, PlanError> {
        let from = opts.input_format;
        let Operation::Convert(to) = req.op else {
            return Err(PlanError::Other("not a conversion".to_owned()));
        };
        let unsupported = PlanError::Unsupported { from, to };
        let reader = pandoc_reader(from).ok_or(unsupported.clone())?;
        let (writer, extra) = pandoc_writer(to).ok_or(unsupported)?;
        let mut args: Vec<OsString> = vec![os("-f"), os(reader), os("-t"), os(writer)];
        args.extend(extra.iter().map(|a| os(a)));
        // Relative images and links in the source resolve next to the source file.
        if let Some(dir) = input.parent() {
            let mut r = OsString::from("--resource-path=");
            r.push(dir);
            args.push(r);
        }
        args.push(os("-o"));
        args.push(safe_arg(tmp_out)?);
        args.push(safe_arg(input)?);
        Ok(vec![CommandSpec {
            program: self.program.clone(),
            args,
        }])
    }

    fn parse_progress(&self, _line: &str, _duration: Option<f64>) -> Option<f32> {
        None
    }

    fn timeout(&self) -> Option<Duration> {
        Some(OFFICE_TIMEOUT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zest_core::{JobId, Quality};

    fn req(to: FormatId) -> JobRequest {
        JobRequest {
            id: JobId(1),
            files: vec![],
            op: Operation::Convert(to),
            quality: Quality::High,
            out_dir: None,
            page_range: None,
        }
    }

    fn strs(c: &CommandSpec) -> Vec<String> {
        c.args.iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn file_url_encodes_spaces_and_slashes() {
        assert_eq!(
            file_url(Path::new(r"C:\Users\Jo Doe\AppData\Local\Zest\tmp\1-2\profile")),
            "file:///C:/Users/Jo%20Doe/AppData/Local/Zest/tmp/1-2/profile"
        );
        assert_eq!(file_url(Path::new(r"D:\a&b\\é")), "file:///D:/a%26b//%C3%A9");
    }

    #[test]
    fn libreoffice_command() {
        let a = LibreOfficeAdapter::new(PathBuf::from(r"C:\LO\soffice.exe"));
        let mut opts = PlanOptions::new(FormatId::Docx);
        opts.scratch_dir = Some(PathBuf::from(r"C:\tmp\1-1"));
        let cmds = a
            .plan(&req(FormatId::Pdf), Path::new(r"C:\docs\a b.docx"), Path::new(r"C:\docs\a b.pdf.zest-tmp"), &opts)
            .unwrap();
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].program, PathBuf::from(r"C:\LO\soffice.exe"));
        assert_eq!(
            strs(&cmds[0]),
            [
                "--headless",
                "--norestore",
                "--nologo",
                "-env:UserInstallation=file:///C:/tmp/1-1/profile",
                "--convert-to",
                "pdf",
                "--outdir",
                r"C:\tmp\1-1\out",
                r"C:\docs\a b.docx",
            ]
        );
        assert!(a.needs_scratch(&req(FormatId::Pdf), FormatId::Docx));
        assert_eq!(a.timeout(), Some(Duration::from_secs(120)));
        // Needs the scratch folder; refuses formats it cannot write.
        assert!(a
            .plan(&req(FormatId::Pdf), Path::new(r"C:\a.docx"), Path::new("t"), &PlanOptions::new(FormatId::Docx))
            .is_err());
        assert!(a.plan(&req(FormatId::Md), Path::new(r"C:\a.docx"), Path::new("t"), &opts).is_err());
        let csv = a.plan(&req(FormatId::Csv), Path::new(r"C:\a.xlsx"), Path::new("t"), &opts).unwrap();
        assert!(strs(&csv[0]).contains(&"csv:Text - txt - csv (StarCalc):44,34,76,1".to_owned()));
    }

    #[test]
    fn libreoffice_finish_moves_the_result() {
        let root = std::env::temp_dir().join(format!("zest-lo-finish-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(OUT_SUBDIR)).unwrap();
        fs::write(root.join(OUT_SUBDIR).join("a.pdf"), b"%PDF-1.4").unwrap();
        let tmp = root.join("a.pdf.zest-tmp");
        fs::write(&tmp, b"").unwrap();
        let a = LibreOfficeAdapter::new(PathBuf::new());
        let mut opts = PlanOptions::new(FormatId::Docx);
        opts.scratch_dir = Some(root.clone());
        a.finish(&req(FormatId::Pdf), Path::new("a.docx"), &tmp, &opts).unwrap();
        assert_eq!(fs::read(&tmp).unwrap(), b"%PDF-1.4");
        // No result file: an error, not a panic.
        assert!(a.finish(&req(FormatId::Pdf), Path::new("a.docx"), &tmp, &opts).is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn pandoc_command() {
        let a = PandocAdapter::new(PathBuf::from(r"C:\P\pandoc.exe"));
        let opts = PlanOptions::new(FormatId::Md);
        let cmds = a
            .plan(&req(FormatId::Docx), Path::new(r"C:\n\x.md"), Path::new(r"C:\n\x.docx.zest-tmp"), &opts)
            .unwrap();
        assert_eq!(
            strs(&cmds[0]),
            [
                "-f",
                "markdown",
                "-t",
                "docx",
                r"--resource-path=C:\n",
                "-o",
                r"C:\n\x.docx.zest-tmp",
                r"C:\n\x.md",
            ]
        );
        let html = a.plan(&req(FormatId::Html), Path::new(r"C:\n\x.md"), Path::new("t"), &opts).unwrap();
        assert!(strs(&html[0]).contains(&"--standalone".to_owned()));
        let opts = PlanOptions::new(FormatId::Docx);
        let md = a.plan(&req(FormatId::Md), Path::new(r"C:\n\x.docx"), Path::new("t"), &opts).unwrap();
        let s = strs(&md[0]);
        assert!(s.windows(2).any(|w| w == ["-t", "gfm"]));
        assert!(s.contains(&"--wrap=none".to_owned()));
        // No PDF from pandoc.
        assert!(a.plan(&req(FormatId::Pdf), Path::new(r"C:\n\x.md"), Path::new("t"), &PlanOptions::new(FormatId::Md)).is_err());
        assert!(a.plan(&req(FormatId::Md), Path::new(r"C:\n\x.txt"), Path::new("t"), &PlanOptions::new(FormatId::Txt)).is_err());
        assert_eq!(a.timeout(), Some(OFFICE_TIMEOUT));
    }
}
