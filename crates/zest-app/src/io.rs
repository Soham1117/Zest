//! Background disk work: state and history writes, startup cleanup.
//! The UI thread only sends messages; it never touches these files.

use crate::history::{self, TAIL_LINES};
use crate::state::write_atomic;
use crossbeam_channel::{unbounded, Sender};
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub enum IoMsg {
    /// One history line (no newline).
    History(String),
    /// Full `state.json` text.
    State(String),
}

#[derive(Clone)]
pub struct Io {
    tx: Sender<IoMsg>,
}

impl Io {
    pub fn history(&self, line: String) {
        let _ = self.tx.send(IoMsg::History(line));
    }

    pub fn state(&self, json: String) {
        let _ = self.tx.send(IoMsg::State(json));
    }
}

/// Read at most the last `max_bytes` of a text file (whole lines only).
pub fn read_tail_text(path: &Path, max_bytes: u64) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let cut = len > max_bytes;
    if cut {
        f.seek(SeekFrom::Start(len - max_bytes))?;
    }
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    if cut {
        // The first line is probably partial.
        return Ok(text.split_once('\n').map(|(_, rest)| rest.to_owned()).unwrap_or_default());
    }
    Ok(text)
}

fn startup_cleanup(history_path: &Path) {
    let text = match read_tail_text(history_path, 512 * 1024) {
        Ok(t) => t,
        Err(_) => return,
    };
    let records = history::parse_tail(&text, TAIL_LINES);
    let open = history::unfinished(&records);
    if !open.is_empty() {
        tracing::info!(count = open.len(), "history: jobs without a finish line (interrupted); nothing to resume");
    }
    let dirs: Vec<PathBuf> = history::output_folders(&records);
    let removed = zest_broker::cleanup_stale_temps(&dirs);
    tracing::info!(folders = dirs.len(), removed, "stale temp file cleanup");
}

/// Start the I/O thread. It first runs the startup cleanup, then handles messages.
pub fn start(history_path: PathBuf, state_path: PathBuf) -> Io {
    let (tx, rx) = unbounded::<IoMsg>();
    let spawned = std::thread::Builder::new().name("zest-io".into()).spawn(move || {
        startup_cleanup(&history_path);
        for msg in rx {
            match msg {
                IoMsg::History(line) => {
                    let r = (|| -> std::io::Result<()> {
                        if let Some(dir) = history_path.parent() {
                            std::fs::create_dir_all(dir)?;
                        }
                        let mut f = OpenOptions::new().create(true).append(true).open(&history_path)?;
                        f.write_all(line.as_bytes())?;
                        f.write_all(b"\n")
                    })();
                    if let Err(e) = r {
                        tracing::warn!("history write failed: {e}");
                    }
                }
                IoMsg::State(json) => {
                    if let Err(e) = write_atomic(&state_path, &json) {
                        tracing::warn!("state write failed: {e}");
                    }
                }
            }
        }
    });
    if spawned.is_err() {
        tracing::error!("cannot start the I/O thread");
    }
    Io { tx }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_reads_whole_lines() {
        let dir = std::env::temp_dir().join(format!("zest-io-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("h.jsonl");
        std::fs::write(&p, "aaaa\nbbbb\ncccc\n").unwrap();
        assert_eq!(read_tail_text(&p, 1000).unwrap(), "aaaa\nbbbb\ncccc\n");
        assert_eq!(read_tail_text(&p, 8).unwrap(), "cccc\n");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
