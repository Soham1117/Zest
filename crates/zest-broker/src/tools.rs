//! Locate the external tools and probe which engines work.

use crate::adapter::CommandSpec;
use crate::process::run_capture;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;
use zest_core::{EngineId, ToolsConfig};

/// Time allowed for `--version` during the probe.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Optional explicit paths from the config file (`[tools]`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolOverrides {
    pub vips: Option<PathBuf>,
    pub ffmpeg: Option<PathBuf>,
    pub ffprobe: Option<PathBuf>,
    pub soffice: Option<PathBuf>,
    pub pandoc: Option<PathBuf>,
}

impl From<&ToolsConfig> for ToolOverrides {
    fn from(t: &ToolsConfig) -> Self {
        // An empty path in the config means "not set".
        let some = |p: &Option<PathBuf>| p.clone().filter(|p| !p.as_os_str().is_empty());
        ToolOverrides {
            vips: some(&t.vips),
            ffmpeg: some(&t.ffmpeg),
            ffprobe: some(&t.ffprobe),
            soffice: some(&t.soffice),
            pandoc: some(&t.pandoc),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolPaths {
    pub vips: PathBuf,
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    /// `zest-worker.exe` (PDF jobs).
    pub worker: PathBuf,
    /// `pdfium.dll`, passed to the worker.
    pub pdfium: PathBuf,
    /// LibreOffice `soffice.exe`, if found. Optional engine.
    pub soffice: Option<PathBuf>,
    /// `pandoc.exe`, if found. Optional engine.
    pub pandoc: Option<PathBuf>,
}

const VIPS_REL: &str = r"vips\bin\vips.exe";
const FFMPEG_REL: &str = r"ffmpeg\bin\ffmpeg.exe";
const FFPROBE_REL: &str = r"ffmpeg\bin\ffprobe.exe";
const PDFIUM_REL: &str = r"pdfium\bin\pdfium.dll";
const PANDOC_REL: &str = r"pandoc\pandoc.exe";
const WORKER_EXE: &str = "zest-worker.exe";

/// `soffice.exe` in `%ProgramFiles%` or `%ProgramFiles(x86)%`.
fn find_soffice(program_files: &[PathBuf]) -> Option<PathBuf> {
    program_files
        .iter()
        .map(|pf| pf.join("LibreOffice").join("program").join("soffice.exe"))
        .find(|p| p.is_file())
}

/// `pandoc.exe` on `PATH`, then in the extra folders
/// (`%LOCALAPPDATA%\Pandoc`, `%ProgramFiles%\Pandoc`).
fn find_pandoc(path_var: Option<&OsStr>, extra_dirs: &[PathBuf]) -> Option<PathBuf> {
    let from_path = path_var
        .map(|v| std::env::split_paths(v).collect::<Vec<_>>())
        .unwrap_or_default();
    from_path
        .iter()
        .chain(extra_dirs.iter())
        .map(|d| d.join("pandoc.exe"))
        .find(|p| p.is_file())
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// `zest-worker.exe`: next to the exe, one folder up (test binaries live in
/// `target\<profile>\deps`), then `target\release` and `target\debug` of every parent folder.
fn find_worker(exe_dir: &Path) -> PathBuf {
    let mut cands: Vec<PathBuf> = vec![exe_dir.join(WORKER_EXE)];
    if let Some(up) = exe_dir.parent() {
        cands.push(up.join(WORKER_EXE));
    }
    let mut cur: Option<&Path> = Some(exe_dir);
    while let Some(d) = cur {
        for profile in ["release", "debug"] {
            cands.push(d.join("target").join(profile).join(WORKER_EXE));
        }
        cur = d.parent();
    }
    cands
        .iter()
        .find(|p| p.is_file())
        .cloned()
        .unwrap_or_else(|| exe_dir.join(WORKER_EXE))
}

impl ToolPaths {
    /// Resolve with the path of the running executable.
    /// Order: config override, `<exe dir>\third_party\bin`, then every parent folder of the exe.
    pub fn resolve(overrides: &ToolOverrides) -> ToolPaths {
        let exe = std::env::current_exe().unwrap_or_default();
        Self::resolve_from(&exe, overrides)
    }

    /// Same as [`ToolPaths::resolve`] for a given executable path (testable).
    pub fn resolve_from(exe: &Path, overrides: &ToolOverrides) -> ToolPaths {
        let exe_dir = exe.parent().unwrap_or(Path::new("")).to_path_buf();
        // Candidate `third_party\bin` folders, nearest first.
        let mut bases: Vec<PathBuf> = Vec::new();
        let mut cur: Option<&Path> = Some(&exe_dir);
        while let Some(d) = cur {
            bases.push(d.join("third_party").join("bin"));
            cur = d.parent();
        }
        let find = |rel: &str| -> PathBuf {
            bases
                .iter()
                .map(|b| b.join(rel))
                .find(|p| p.is_file())
                .unwrap_or_else(|| exe_dir.join("third_party").join("bin").join(rel))
        };
        let ffmpeg = overrides.ffmpeg.clone().unwrap_or_else(|| find(FFMPEG_REL));
        let ffprobe = overrides.ffprobe.clone().unwrap_or_else(|| {
            // Next to an overridden ffmpeg, if there.
            overrides
                .ffmpeg
                .as_ref()
                .and_then(|f| f.parent().map(|d| d.join("ffprobe.exe")))
                .filter(|p| p.is_file())
                .unwrap_or_else(|| find(FFPROBE_REL))
        });
        let program_files: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)"]
            .iter()
            .filter_map(|n| env_path(n))
            .collect();
        let mut pandoc_dirs: Vec<PathBuf> = Vec::new();
        if let Some(l) = env_path("LOCALAPPDATA") {
            pandoc_dirs.push(l.join("Pandoc"));
        }
        pandoc_dirs.extend(program_files.iter().map(|p| p.join("Pandoc")));
        ToolPaths {
            vips: overrides.vips.clone().unwrap_or_else(|| find(VIPS_REL)),
            ffmpeg,
            ffprobe,
            worker: find_worker(&exe_dir),
            pdfium: find(PDFIUM_REL),
            soffice: overrides
                .soffice
                .clone()
                .or_else(|| find_soffice(&program_files)),
            // Order: config, `third_party\bin\pandoc`, downloaded engine, PATH and install folders.
            pandoc: overrides
                .pandoc
                .clone()
                .or_else(|| bases.iter().map(|b| b.join(PANDOC_REL)).find(|p| p.is_file()))
                .or_else(|| {
                    crate::engines::installed_path("pandoc").filter(|p| p.is_file())
                })
                .or_else(|| find_pandoc(std::env::var_os("PATH").as_deref(), &pandoc_dirs)),
        }
    }

    /// Engines that work: the exe exists and `--version` runs within 3 seconds.
    /// PDFium needs the DLL and a worker that starts. LibreOffice counts when
    /// `soffice.exe` exists (its start is too slow to probe). Pandoc must answer `--version`.
    /// This starts child processes: call it off the UI thread.
    pub fn probe(&self) -> Vec<EngineId> {
        let (vips, ffmpeg, ffprobe, worker, pandoc) = std::thread::scope(|s| {
            let v = s.spawn(|| version_ok(&self.vips, "--version"));
            let f = s.spawn(|| version_ok(&self.ffmpeg, "-version"));
            let p = s.spawn(|| version_ok(&self.ffprobe, "-version"));
            let w = s.spawn(|| self.pdfium.is_file() && version_ok(&self.worker, "--version"));
            let d = s.spawn(|| {
                self.pandoc
                    .as_deref()
                    .is_some_and(|p| version_ok(p, "--version"))
            });
            (
                v.join().unwrap_or(false),
                f.join().unwrap_or(false),
                p.join().unwrap_or(false),
                w.join().unwrap_or(false),
                d.join().unwrap_or(false),
            )
        });
        let mut out = Vec::new();
        if vips {
            out.push(EngineId::Vips);
        }
        if ffmpeg && ffprobe {
            out.push(EngineId::Ffmpeg);
        }
        if worker {
            out.push(EngineId::Pdfium);
        }
        if self.soffice.as_deref().is_some_and(Path::is_file) {
            out.push(EngineId::LibreOffice);
        }
        if pandoc {
            out.push(EngineId::Pandoc);
        }
        out
    }
}

/// `flag` is `--version` for vips and `-version` for the FFmpeg tools.
fn version_ok(exe: &Path, flag: &str) -> bool {
    if !exe.is_file() {
        return false;
    }
    let spec = CommandSpec {
        program: exe.to_path_buf(),
        args: vec![OsString::from(flag)],
    };
    run_capture(&spec, PROBE_TIMEOUT).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(p: &Path) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"x").unwrap();
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("zest-tools-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn exe_dir_wins_over_parent() {
        let root = tmp("a");
        let exe = root.join("target").join("release").join("zest.exe");
        touch(&root.join(r"third_party\bin").join(VIPS_REL));
        touch(&exe.parent().unwrap().join(r"third_party\bin").join(VIPS_REL));
        let t = ToolPaths::resolve_from(&exe, &ToolOverrides::default());
        assert!(t.vips.starts_with(exe.parent().unwrap()));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn walks_up_to_workspace_root() {
        let root = tmp("b");
        let exe = root.join("target").join("release").join("zest.exe");
        touch(&root.join(r"third_party\bin").join(FFMPEG_REL));
        touch(&root.join(r"third_party\bin").join(FFPROBE_REL));
        let t = ToolPaths::resolve_from(&exe, &ToolOverrides::default());
        assert_eq!(t.ffmpeg, root.join(r"third_party\bin").join(FFMPEG_REL));
        assert_eq!(t.ffprobe, root.join(r"third_party\bin").join(FFPROBE_REL));
        // vips missing: falls back to the exe dir candidate.
        assert!(t.vips.starts_with(exe.parent().unwrap()));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn override_wins() {
        let root = tmp("c");
        let exe = root.join("zest.exe");
        touch(&root.join(r"third_party\bin").join(VIPS_REL));
        let o = ToolOverrides {
            vips: Some(PathBuf::from(r"X:\my\vips.exe")),
            ..Default::default()
        };
        let t = ToolPaths::resolve_from(&exe, &o);
        assert_eq!(t.vips, PathBuf::from(r"X:\my\vips.exe"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn probe_missing_tools_is_empty() {
        let t = ToolPaths {
            vips: PathBuf::from(r"Z:\none\vips.exe"),
            ffmpeg: PathBuf::from(r"Z:\none\ffmpeg.exe"),
            ffprobe: PathBuf::from(r"Z:\none\ffprobe.exe"),
            worker: PathBuf::from(r"Z:\none\zest-worker.exe"),
            pdfium: PathBuf::from(r"Z:\none\pdfium.dll"),
            soffice: Some(PathBuf::from(r"Z:\none\soffice.exe")),
            pandoc: None,
        };
        assert!(t.probe().is_empty());
    }

    #[test]
    fn worker_next_to_exe_or_in_target() {
        let root = tmp("w");
        let exe = root.join("target").join("debug").join("deps").join("t.exe");
        touch(&root.join(r"target\debug").join(WORKER_EXE));
        assert_eq!(find_worker(exe.parent().unwrap()), root.join(r"target\debug").join(WORKER_EXE));
        // Next to the exe wins.
        let app = root.join("app").join("zest.exe");
        touch(&root.join("app").join(WORKER_EXE));
        assert_eq!(find_worker(app.parent().unwrap()), root.join("app").join(WORKER_EXE));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn optional_tools_are_found_or_none() {
        let root = tmp("opt");
        let pf = root.join("Program Files");
        assert_eq!(find_soffice(std::slice::from_ref(&pf)), None);
        let so = pf.join(r"LibreOffice\program\soffice.exe");
        touch(&so);
        assert_eq!(find_soffice(&[root.join("nope"), pf.clone()]), Some(so));
        // Pandoc: PATH first, then the extra folders.
        let bin = root.join("bin");
        let local = root.join(r"Local\Pandoc");
        assert_eq!(find_pandoc(None, std::slice::from_ref(&local)), None);
        touch(&local.join("pandoc.exe"));
        assert_eq!(find_pandoc(None, std::slice::from_ref(&local)), Some(local.join("pandoc.exe")));
        touch(&bin.join("pandoc.exe"));
        let path = std::env::join_paths([root.join("empty"), bin.clone()]).unwrap();
        assert_eq!(find_pandoc(Some(&path), &[local]), Some(bin.join("pandoc.exe")));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn bundled_pandoc_is_found() {
        let root = tmp("pandoc");
        let exe = root.join("target").join("release").join("zest.exe");
        touch(&root.join(r"third_party\bin").join(PANDOC_REL));
        let t = ToolPaths::resolve_from(&exe, &ToolOverrides::default());
        assert_eq!(t.pandoc, Some(root.join(r"third_party\bin").join(PANDOC_REL)));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn config_overrides_reach_the_resolver() {
        let root = tmp("cfg");
        let cfg = ToolsConfig {
            soffice: Some(PathBuf::from(r"X:\lo\soffice.exe")),
            pandoc: Some(PathBuf::new()), // empty = not set
            ..Default::default()
        };
        let o = ToolOverrides::from(&cfg);
        assert_eq!(o.soffice, Some(PathBuf::from(r"X:\lo\soffice.exe")));
        assert_eq!(o.pandoc, None);
        let t = ToolPaths::resolve_from(&root.join("zest.exe"), &o);
        assert_eq!(t.soffice, Some(PathBuf::from(r"X:\lo\soffice.exe")));
        // An override that does not exist is kept (the probe drops the engine).
        assert!(!t.probe().contains(&EngineId::LibreOffice));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn probe_pdfium_needs_dll_and_worker() {
        let t = ToolPaths {
            vips: PathBuf::from(r"Z:\none\vips.exe"),
            ffmpeg: PathBuf::from(r"Z:\none\ffmpeg.exe"),
            ffprobe: PathBuf::from(r"Z:\none\ffprobe.exe"),
            worker: PathBuf::from(r"Z:\none\zest-worker.exe"),
            pdfium: PathBuf::from(r"Z:\none\pdfium.dll"),
            soffice: None,
            pandoc: None,
        };
        assert!(!t.probe().contains(&EngineId::Pdfium));
    }
}
