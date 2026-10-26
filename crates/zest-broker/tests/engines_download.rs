//! Network test: downloads Pandoc (about 40 MB) into `%LOCALAPPDATA%\Zest\engines`.
//! Run by hand: `cargo test -p zest-broker --test engines_download -- --ignored --nocapture`

use zest_broker::engines::{download_optional, EnginePhase};

#[test]
#[ignore = "downloads about 40 MB from github.com"]
fn downloads_and_runs_pandoc() {
    let mut last = None;
    let exe = download_optional("pandoc", &mut |p| {
        if last != Some(p.phase) {
            eprintln!("phase: {:?}", p.phase);
            last = Some(p.phase);
        }
    })
    .expect("download failed");
    assert_eq!(last, Some(EnginePhase::Done));
    assert!(exe.is_file());
    let out = std::process::Command::new(&exe).arg("--version").output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("pandoc"));
}
