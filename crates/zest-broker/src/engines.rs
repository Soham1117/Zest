//! Optional engines: where they live and how to download them (Pandoc).
//!
//! Zest makes no network calls on its own. [`download_optional`] runs only
//! after the user said yes. It reads the pinned URL and SHA-256 from
//! `tools/deps.lock.json` (embedded at build time), downloads over HTTPS,
//! checks the hash, and unpacks into `%LOCALAPPDATA%\Zest\engines\<name>\`.
//! LibreOffice is too big for this: the app opens [`libreoffice_download_url`].

use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// The pinned dependency list, embedded at build time.
const LOCK_JSON: &str = include_str!("../../../tools/deps.lock.json");

/// Upper limit for a download (1 GiB).
const MAX_DOWNLOAD: u64 = 1 << 30;

/// Page where the user gets LibreOffice. The app opens it in the browser.
pub fn libreoffice_download_url() -> &'static str {
    "https://www.libreoffice.org/download/download-libreoffice/"
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("\"{0}\" is not an optional engine that Zest can download")]
    Unknown(String),
    #[error("The engine list is damaged: {0}")]
    BadLock(String),
    #[error("Download failed: {0}")]
    Network(String),
    #[error("The download is corrupt (SHA-256 mismatch)")]
    HashMismatch,
    #[error("The download is too large")]
    TooLarge,
    #[error("The archive is not valid: {0}")]
    Archive(String),
    #[error("File error: {0}")]
    Io(#[from] io::Error),
}

/// Step of a download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnginePhase {
    Downloading,
    Verifying,
    Extracting,
    Done,
}

/// Progress report. `percent` is 0 to 100 (`None` if the size is not known).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EngineProgress {
    pub phase: EnginePhase,
    pub percent: Option<f32>,
}

/// One entry of the lock file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockEntry {
    pub name: String,
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub dest: String,
    pub exe: String,
    pub optional: bool,
}

/// `%LOCALAPPDATA%\Zest\engines`
pub fn engines_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Zest")
        .join("engines")
}

fn find_entry(lock: &str, name: &str) -> Result<LockEntry, EngineError> {
    let v: serde_json::Value =
        serde_json::from_str(lock).map_err(|e| EngineError::BadLock(e.to_string()))?;
    let list = v
        .get("dependencies")
        .and_then(|d| d.as_array())
        .ok_or_else(|| EngineError::BadLock("no dependencies".to_owned()))?;
    for d in list {
        let s = |k: &str| d.get(k).and_then(|x| x.as_str()).map(str::to_owned);
        if s("name").as_deref() != Some(name) {
            continue;
        }
        let optional = d.get("optional").and_then(|x| x.as_bool()).unwrap_or(false);
        if !optional {
            return Err(EngineError::Unknown(name.to_owned()));
        }
        let field =
            |k: &str| s(k).ok_or_else(|| EngineError::BadLock(format!("{name}: missing {k}")));
        let entry = LockEntry {
            name: name.to_owned(),
            version: field("version")?,
            url: field("url")?,
            sha256: field("sha256")?.to_ascii_lowercase(),
            dest: field("dest")?,
            exe: field("exe")?,
            optional,
        };
        if !entry.url.starts_with("https://") {
            return Err(EngineError::BadLock(format!("{name}: URL is not https")));
        }
        if entry.sha256.len() != 64 || !entry.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(EngineError::BadLock(format!("{name}: bad SHA-256")));
        }
        return Ok(entry);
    }
    Err(EngineError::Unknown(name.to_owned()))
}

/// Path of the exe of a downloaded engine, if the lock file knows it
/// (the file may not exist).
pub fn installed_path(name: &str) -> Option<PathBuf> {
    let e = find_entry(LOCK_JSON, name).ok()?;
    Some(engines_dir().join(&e.dest).join(e.exe.replace('/', "\\")))
}

static STAGING_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Delete a folder on drop.
struct DirGuard(PathBuf);

impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Download an optional engine (now: `"pandoc"`) and return the path of its exe.
/// If it is already installed, the path is returned at once.
/// `progress` is called from the calling thread, at most about once per percent.
/// This blocks: run it on a worker thread.
pub fn download_optional(
    name: &str,
    progress: &mut dyn FnMut(EngineProgress),
) -> Result<PathBuf, EngineError> {
    download_with(LOCK_JSON, &engines_dir(), name, progress)
}

fn download_with(
    lock: &str,
    root: &Path,
    name: &str,
    progress: &mut dyn FnMut(EngineProgress),
) -> Result<PathBuf, EngineError> {
    let entry = find_entry(lock, name)?;
    let dest = root.join(&entry.dest);
    let exe = dest.join(entry.exe.replace('/', "\\"));
    if exe.is_file() {
        progress(EngineProgress {
            phase: EnginePhase::Done,
            percent: Some(100.0),
        });
        return Ok(exe);
    }
    fs::create_dir_all(root)?;
    let staging = root.join(format!(
        ".staging-{}-{}",
        std::process::id(),
        STAGING_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging)?;
    let _guard = DirGuard(staging.clone());

    let zip_path = staging.join("download.zip");
    fetch_verified(&entry, &zip_path, progress)?;
    progress(EngineProgress {
        phase: EnginePhase::Extracting,
        percent: None,
    });
    let unpacked = staging.join("unpacked");
    extract_zip(&zip_path, &unpacked, progress)?;
    // One top-level folder: use it as the root.
    let mut top = unpacked.clone();
    let kids: Vec<_> = fs::read_dir(&unpacked)?.flatten().collect();
    if kids.len() == 1 && kids[0].path().is_dir() {
        top = kids[0].path();
    }
    if !top.join(entry.exe.replace('/', "\\")).is_file() {
        return Err(EngineError::Archive(format!(
            "{} not found in the archive",
            entry.exe
        )));
    }
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
    fs::rename(&top, &dest)?;
    progress(EngineProgress {
        phase: EnginePhase::Done,
        percent: Some(100.0),
    });
    Ok(exe)
}

/// Download `entry.url` to `out`, hash while writing, compare with the pin.
fn fetch_verified(
    entry: &LockEntry,
    out: &Path,
    progress: &mut dyn FnMut(EngineProgress),
) -> Result<(), EngineError> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(60))
        .redirects(5)
        .user_agent("Zest")
        .build();
    let resp = agent
        .get(&entry.url)
        .call()
        .map_err(|e| EngineError::Network(e.to_string()))?;
    let total: Option<u64> = resp.header("Content-Length").and_then(|v| v.parse().ok());
    if total.is_some_and(|t| t > MAX_DOWNLOAD) {
        return Err(EngineError::TooLarge);
    }
    let mut reader = resp.into_reader();
    let mut file = File::create(out)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut done: u64 = 0;
    let mut last_pct: i32 = -1;
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| EngineError::Network(e.to_string()))?;
        if n == 0 {
            break;
        }
        done += n as u64;
        if done > MAX_DOWNLOAD {
            return Err(EngineError::TooLarge);
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])?;
        let pct = total.map_or(0, |t| (done as f64 * 100.0 / t.max(1) as f64) as i32);
        if pct != last_pct {
            last_pct = pct;
            progress(EngineProgress {
                phase: EnginePhase::Downloading,
                percent: total.map(|_| pct as f32),
            });
        }
    }
    file.flush()?;
    drop(file);
    progress(EngineProgress {
        phase: EnginePhase::Verifying,
        percent: None,
    });
    if hex(&hasher.finalize()) != entry.sha256 {
        return Err(EngineError::HashMismatch);
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Unpack `zip_path` into `dest`. Entries that would leave `dest` are rejected.
fn extract_zip(
    zip_path: &Path,
    dest: &Path,
    progress: &mut dyn FnMut(EngineProgress),
) -> Result<(), EngineError> {
    let zerr = |e: zip::result::ZipError| EngineError::Archive(e.to_string());
    let mut archive = zip::ZipArchive::new(File::open(zip_path)?).map_err(zerr)?;
    fs::create_dir_all(dest)?;
    let count = archive.len();
    let mut last_pct = -1;
    for i in 0..count {
        let mut entry = archive.by_index(i).map_err(zerr)?;
        let rel = entry
            .enclosed_name()
            .ok_or_else(|| EngineError::Archive("unsafe path in the archive".to_owned()))?;
        if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
            return Err(EngineError::Archive("unsafe path in the archive".to_owned()));
        }
        let target = dest.join(&rel);
        if entry.is_dir() {
            fs::create_dir_all(&target)?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut f = File::create(&target)?;
            io::copy(&mut entry, &mut f)?;
        }
        let pct = (i + 1) * 100 / count.max(1);
        if pct as i32 != last_pct {
            last_pct = pct as i32;
            progress(EngineProgress {
                phase: EnginePhase::Extracting,
                percent: Some(pct as f32),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::write::SimpleFileOptions;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("zest-engines-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn make_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let mut w = zip::ZipWriter::new(File::create(path).unwrap());
        let opt = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (n, data) in entries {
            w.start_file(*n, opt).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap();
    }

    #[test]
    fn embedded_lock_has_pandoc() {
        let e = find_entry(LOCK_JSON, "pandoc").unwrap();
        assert!(e.optional);
        assert_eq!(e.exe, "pandoc.exe");
        assert!(e
            .url
            .starts_with("https://github.com/jgm/pandoc/releases/download/"));
        assert!(e.url.contains(&e.version));
        assert_eq!(e.sha256.len(), 64);
        // Required tools are not downloadable this way; unknown names too.
        assert!(matches!(find_entry(LOCK_JSON, "ffmpeg"), Err(EngineError::Unknown(_))));
        assert!(matches!(find_entry(LOCK_JSON, "nope"), Err(EngineError::Unknown(_))));
        assert!(installed_path("pandoc")
            .unwrap()
            .ends_with(r"Zest\engines\pandoc\pandoc.exe"));
        assert!(libreoffice_download_url().starts_with("https://www.libreoffice.org/"));
    }

    #[test]
    fn bad_lock_entries_are_rejected() {
        let mk = |url: &str, sha: &str| {
            format!(
                r#"{{"dependencies":[{{"name":"x","version":"1","url":"{url}","sha256":"{sha}","dest":"x","exe":"x.exe","optional":true}}]}}"#
            )
        };
        let ok_sha = "a".repeat(64);
        assert!(find_entry(&mk("https://a/b.zip", &ok_sha), "x").is_ok());
        assert!(matches!(
            find_entry(&mk("http://a/b.zip", &ok_sha), "x"),
            Err(EngineError::BadLock(_))
        ));
        assert!(matches!(
            find_entry(&mk("https://a/b.zip", "abc"), "x"),
            Err(EngineError::BadLock(_))
        ));
        assert!(matches!(find_entry("not json", "x"), Err(EngineError::BadLock(_))));
    }

    #[test]
    fn extract_and_reject_unsafe_paths() {
        let d = tmp("extract");
        let z = d.join("a.zip");
        make_zip(&z, &[("top/pandoc.exe", b"MZ"), ("top/sub/readme.txt", b"hi")]);
        let out = d.join("out");
        extract_zip(&z, &out, &mut |_| {}).unwrap();
        assert_eq!(fs::read(out.join("top/pandoc.exe")).unwrap(), b"MZ");
        assert_eq!(fs::read(out.join("top/sub/readme.txt")).unwrap(), b"hi");
        let bad = d.join("bad.zip");
        make_zip(&bad, &[("../evil.txt", b"x")]);
        let r = extract_zip(&bad, &d.join("out2"), &mut |_| {});
        assert!(matches!(r, Err(EngineError::Archive(_))));
        assert!(!d.join("evil.txt").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn sha256_hex() {
        let mut h = Sha256::new();
        h.update(b"abc");
        assert_eq!(
            hex(&h.finalize()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn already_installed_returns_at_once() {
        let root = tmp("present");
        let exe = root.join("x").join("x.exe");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, b"MZ").unwrap();
        let lock = format!(
            r#"{{"dependencies":[{{"name":"x","version":"1","url":"https://a/b.zip","sha256":"{}","dest":"x","exe":"x.exe","optional":true}}]}}"#,
            "b".repeat(64)
        );
        let mut seen = Vec::new();
        let got = download_with(&lock, &root, "x", &mut |p| seen.push(p.phase)).unwrap();
        assert_eq!(got, exe);
        assert_eq!(seen, vec![EnginePhase::Done]);
        let _ = fs::remove_dir_all(&root);
    }
}
