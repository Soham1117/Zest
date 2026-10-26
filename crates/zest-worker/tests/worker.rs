//! Runs the real worker with the real PDFium library. Skipped when `pdfium.dll` is missing.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn worker() -> Command {
    Command::new(env!("CARGO_BIN_EXE_zest-worker"))
}

fn dll() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let p = root.join(r"third_party\bin\pdfium\bin\pdfium.dll");
    p.is_file().then_some(p)
}

/// A small valid PDF with `pages` empty pages of 200 x 300 points.
fn make_pdf(pages: usize, w: u32, h: u32) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    let mut obj = |out: &mut Vec<u8>, body: String| {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", offsets.len(), body).as_bytes());
    };
    obj(&mut out, "<< /Type /Catalog /Pages 2 0 R >>".to_owned());
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 3 + i)).collect();
    obj(
        &mut out,
        format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), pages),
    );
    for _ in 0..pages {
        obj(
            &mut out,
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] >>"),
        );
    }
    let xref = out.len();
    let n = offsets.len() + 1;
    out.extend_from_slice(format!("xref\n0 {n}\n0000000000 65535 f \n").as_bytes());
    for o in &offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {n} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("zest-worker-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn version_runs() {
    let o = worker().arg("--version").output().unwrap();
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).starts_with("zest-worker"));
}

#[test]
fn usage_error_has_exit_code_2() {
    let o = worker().arg("bogus").output().unwrap();
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn pages_to_png_and_jpg() {
    let Some(dll) = dll() else {
        eprintln!("skipped: pdfium.dll not found (run tools\\fetch-deps.ps1)");
        return;
    };
    let d = tmp("pages");
    let pdf = d.join("doc.pdf");
    fs::write(&pdf, make_pdf(3, 200, 300)).unwrap();
    for (fmt, ext, magic) in [("png", "png", &[0x89u8, b'P'][..]), ("jpg", "jpg", &[0xFFu8, 0xD8][..])] {
        let out = d.join(format!("out-{fmt}"));
        fs::create_dir_all(&out).unwrap();
        let o = worker()
            .args(["--pdfium"])
            .arg(&dll)
            .args(["pdf-to-images", "--format", fmt, "--dpi", "72", "--quality", "80"])
            .arg(&pdf)
            .arg(&out)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let stdout = String::from_utf8_lossy(&o.stdout).into_owned();
        assert!(stdout.contains("progress 100.0"), "{stdout}");
        for i in 1..=3 {
            let f = out.join(format!("doc-p{i:03}.{ext}"));
            let bytes = fs::read(&f).unwrap();
            assert!(bytes.starts_with(magic), "{}", f.display());
        }
    }
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn page_over_pixel_limit_fails_clearly() {
    let Some(dll) = dll() else {
        eprintln!("skipped: pdfium.dll not found");
        return;
    };
    let d = tmp("big");
    let pdf = d.join("big.pdf");
    // 14400 x 14400 points (the PDF maximum) at 150 DPI is far above 50 MP.
    fs::write(&pdf, make_pdf(1, 14400, 14400)).unwrap();
    let out = d.join("out");
    fs::create_dir_all(&out).unwrap();
    let o = worker()
        .arg("--pdfium")
        .arg(&dll)
        .args(["pdf-to-images", "--format", "png", "--dpi", "150"])
        .arg(&pdf)
        .arg(&out)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("50 megapixels"));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn merge_keeps_order_and_counts_pages() {
    let Some(dll) = dll() else {
        eprintln!("skipped: pdfium.dll not found");
        return;
    };
    let d = tmp("merge");
    let (a, b) = (d.join("a.pdf"), d.join("b.pdf"));
    fs::write(&a, make_pdf(2, 200, 300)).unwrap();
    fs::write(&b, make_pdf(1, 400, 100)).unwrap();
    let merged = d.join("merged.pdf");
    let o = worker()
        .arg("--pdfium")
        .arg(&dll)
        .arg("merge-pdf")
        .arg(&merged)
        .arg(&a)
        .arg(&b)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(fs::read(&merged).unwrap().starts_with(b"%PDF-"));
    // Render the merged file: 3 pages; page 3 is the wide one (400 x 100 at 72 DPI).
    let out = d.join("pages");
    fs::create_dir_all(&out).unwrap();
    let o = worker()
        .arg("--pdfium")
        .arg(&dll)
        .args(["pdf-to-images", "--format", "png", "--dpi", "72"])
        .arg(&merged)
        .arg(&out)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let png_size = |i: u32| -> (u32, u32) {
        let b = fs::read(out.join(format!("merged-p{i:03}.png"))).unwrap();
        (
            u32::from_be_bytes(b[16..20].try_into().unwrap()),
            u32::from_be_bytes(b[20..24].try_into().unwrap()),
        )
    };
    assert_eq!(png_size(1), (200, 300));
    assert_eq!(png_size(3), (400, 100));
    assert!(!out.join("merged-p004.png").exists());
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn damaged_pdf_fails() {
    let Some(dll) = dll() else {
        eprintln!("skipped: pdfium.dll not found");
        return;
    };
    let d = tmp("bad");
    let pdf = d.join("bad.pdf");
    fs::write(&pdf, b"%PDF-1.4\nnot really").unwrap();
    let out = d.join("out");
    fs::create_dir_all(&out).unwrap();
    let o = worker()
        .arg("--pdfium")
        .arg(&dll)
        .args(["pdf-to-images", "--format", "png"])
        .arg(&pdf)
        .arg(&out)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("Cannot open the PDF"));
    let _ = fs::remove_dir_all(&d);
}
