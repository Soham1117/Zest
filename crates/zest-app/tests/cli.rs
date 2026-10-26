//! Runs the built `zest` binary (needs libvips in third_party\bin).

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(name)
}

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("zest-cli-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn zest() -> Command {
    Command::new(env!("CARGO_BIN_EXE_zest"))
}

#[test]
fn converts_png_to_webp() {
    let out = temp_dir("ok");
    let r = zest()
        .args(["convert", "--to", "webp", "--out"])
        .arg(&out)
        .arg(fixture("sample.png"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&r.stdout);
    assert_eq!(r.status.code(), Some(0), "stdout: {stdout}\nstderr: {}", String::from_utf8_lossy(&r.stderr));
    let webp = out.join("sample.webp");
    assert!(webp.is_file(), "output missing; stdout: {stdout}");
    let head = std::fs::read(&webp).unwrap();
    assert_eq!(&head[..4], b"RIFF");
    assert_eq!(&head[8..12], b"WEBP");
    assert!(stdout.contains("1 converted, 0 failed"), "{stdout}");
    // No temp files are left behind.
    let leftovers: Vec<_> = std::fs::read_dir(&out)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".zest-tmp"))
        .collect();
    assert!(leftovers.is_empty());
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn corrupt_input_gives_exit_code_1() {
    let out = temp_dir("bad");
    let r = zest()
        .args(["convert", "--to", "png", "--out"])
        .arg(&out)
        .arg(fixture("corrupt.jpg"))
        .output()
        .unwrap();
    assert_eq!(r.status.code(), Some(1), "{}", String::from_utf8_lossy(&r.stderr));
    assert!(String::from_utf8_lossy(&r.stderr).contains("FAIL"));
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn usage_errors_give_exit_code_2() {
    let r = zest().args(["convert", "--to", "webp"]).output().unwrap();
    assert_eq!(r.status.code(), Some(2));
    let r = zest().args(["convert", "--to", "nonsense", "x.png"]).output().unwrap();
    assert_eq!(r.status.code(), Some(2));
    let r = zest().arg("--bogus").output().unwrap();
    assert_eq!(r.status.code(), Some(2));
}

#[test]
fn version_and_formats() {
    let r = zest().arg("--version").output().unwrap();
    assert_eq!(r.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&r.stdout).starts_with("zest "));
    let r = zest().arg("formats").output().unwrap();
    let out = String::from_utf8_lossy(&r.stdout);
    assert!(out.contains("Engines:"), "{out}");
}

fn lines(r: &std::process::Output) -> String {
    format!(
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&r.stdout),
        String::from_utf8_lossy(&r.stderr)
    )
}

fn leftovers(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".zest-tmp"))
        .count()
}

#[test]
fn tool_resize_and_strip() {
    let out = temp_dir("tool");
    let r = zest()
        .args(["tool", "resize50", "--out"])
        .arg(&out)
        .arg(fixture("sample.png"))
        .output()
        .unwrap();
    assert_eq!(r.status.code(), Some(0), "{}", lines(&r));
    let resized = out.join("sample-50pct.png");
    assert!(resized.is_file(), "{}", lines(&r));
    assert!(String::from_utf8_lossy(&r.stdout).contains("1 resized, 0 failed"), "{}", lines(&r));

    let r = zest()
        .args(["tool", "strip", "--out"])
        .arg(&out)
        .arg(fixture("sample.jpg"))
        .arg(fixture("sample.mp4"))
        .output()
        .unwrap();
    assert_eq!(r.status.code(), Some(0), "{}", lines(&r));
    assert!(out.join("sample-stripped.jpg").is_file(), "{}", lines(&r));
    assert!(out.join("sample-stripped.mp4").is_file(), "{}", lines(&r));
    assert!(String::from_utf8_lossy(&r.stdout).contains("2 stripped, 0 failed"), "{}", lines(&r));
    assert_eq!(leftovers(&out), 0);
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn tool_on_the_wrong_kind_of_file_exits_with_1() {
    let out = temp_dir("toolbad");
    // A video tool on an image.
    let r = zest()
        .args(["tool", "size10", "--out"])
        .arg(&out)
        .arg(fixture("sample.png"))
        .output()
        .unwrap();
    assert_eq!(r.status.code(), Some(1), "{}", lines(&r));
    assert!(String::from_utf8_lossy(&r.stderr).contains("FAIL"), "{}", lines(&r));
    // A missing file.
    let r = zest().args(["tool", "compress"]).arg(out.join("missing.png")).output().unwrap();
    assert_eq!(r.status.code(), Some(1));
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn tool_usage_errors_give_exit_code_2() {
    let r = zest().args(["tool", "sharpen", "a.png"]).output().unwrap();
    assert_eq!(r.status.code(), Some(2));
    let r = zest().args(["tool", "compress"]).output().unwrap();
    assert_eq!(r.status.code(), Some(2));
    let r = zest().args(["zip"]).output().unwrap();
    assert_eq!(r.status.code(), Some(2));
}

#[test]
fn zip_folder_and_files() {
    let root = temp_dir("zip");
    let folder = root.join("My Folder");
    std::fs::create_dir_all(folder.join("inner")).unwrap();
    std::fs::copy(fixture("sample.png"), folder.join("sample.png")).unwrap();
    std::fs::write(folder.join("inner").join("a.txt"), b"hello").unwrap();

    let r = zest().arg("zip").arg(&folder).output().unwrap();
    assert_eq!(r.status.code(), Some(0), "{}", lines(&r));
    let archive = root.join("My Folder.zip");
    assert!(archive.is_file(), "{}", lines(&r));
    let head = std::fs::read(&archive).unwrap();
    assert_eq!(&head[..2], b"PK");
    assert!(String::from_utf8_lossy(&r.stdout).contains("1 zipped, 0 failed"), "{}", lines(&r));

    // Several items into another folder.
    let out = root.join("out");
    let r = zest()
        .arg("zip")
        .arg("--out")
        .arg(&out)
        .arg(fixture("sample.png"))
        .arg(fixture("sample.jpg"))
        .output()
        .unwrap();
    assert_eq!(r.status.code(), Some(0), "{}", lines(&r));
    let zips: Vec<_> = std::fs::read_dir(&out).unwrap().flatten().collect();
    assert_eq!(zips.len(), 1, "{}", lines(&r));
    // A missing item: nothing is made, exit code 1.
    let r = zest().arg("zip").arg(root.join("nope")).output().unwrap();
    assert_eq!(r.status.code(), Some(1));
    assert_eq!(leftovers(&root), 0);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn tool_crop_and_trim_from_the_command_line() {
    let out = temp_dir("croptrim");
    // Crop the sample picture.
    let png = out.join("sample.png");
    std::fs::copy(fixture("sample.png"), &png).unwrap();
    let r = zest().args(["tool", "crop", "--rect", "1,2,20,10"]).arg(&png).output().unwrap();
    assert_eq!(r.status.code(), Some(0), "{}", lines(&r));
    assert!(out.join("sample-crop.png").is_file(), "{}", lines(&r));
    assert!(String::from_utf8_lossy(&r.stdout).contains("1 cropped, 0 failed"), "{}", lines(&r));
    // Trim the sample video.
    let mp4 = out.join("sample.mp4");
    std::fs::copy(fixture("sample.mp4"), &mp4).unwrap();
    let r = zest().args(["tool", "trim", "--start", "00:00.2", "--end", "00:00.8"]).arg(&mp4).output().unwrap();
    assert_eq!(r.status.code(), Some(0), "{}", lines(&r));
    assert!(out.join("sample-trim.mp4").is_file(), "{}", lines(&r));
    // Wrong parameters are usage errors (exit code 2).
    let r = zest().args(["tool", "crop", "a.png"]).output().unwrap();
    assert_eq!(r.status.code(), Some(2));
    let r = zest().args(["tool", "trim", "--start", "nonsense", "a.mp4"]).output().unwrap();
    assert_eq!(r.status.code(), Some(2));
    let _ = std::fs::remove_dir_all(&out);
}
