//! Quick content fingerprint for duplicate detection (size + sampled hash).

use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const SAMPLE: u64 = 64 * 1024;

/// Size plus a SHA-256 over head/mid/tail samples. Cheap enough for hover-path
/// and post-convert checks; not a full-file hash.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Fingerprint {
    pub size: u64,
    pub hash: [u8; 32],
}

/// Read a fingerprint from an existing file. `None` if the file cannot be read.
pub fn fingerprint(path: &Path) -> Option<Fingerprint> {
    let mut f = File::open(path).ok()?;
    let size = f.metadata().ok()?.len();
    let mut hasher = Sha256::new();
    hasher.update(size.to_le_bytes());
    sample(&mut f, &mut hasher, 0, SAMPLE.min(size))?;
    if size > SAMPLE * 2 {
        let mid = size / 2;
        let start = mid.saturating_sub(SAMPLE / 2);
        sample(&mut f, &mut hasher, start, SAMPLE)?;
    }
    if size > SAMPLE {
        let start = size.saturating_sub(SAMPLE);
        sample(&mut f, &mut hasher, start, SAMPLE)?;
    }
    let digest = hasher.finalize();
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&digest);
    Some(Fingerprint { size, hash })
}

fn sample(f: &mut File, hasher: &mut Sha256, offset: u64, len: u64) -> Option<()> {
    f.seek(SeekFrom::Start(offset)).ok()?;
    let mut left = len as usize;
    let mut buf = [0u8; 8192];
    while left > 0 {
        let chunk = left.min(buf.len());
        let n = f.read(&mut buf[..chunk]).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        left -= n;
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn same_bytes_same_fingerprint() {
        let dir = std::env::temp_dir().join(format!("zest-fp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.bin");
        let b = dir.join("b.bin");
        let data: Vec<u8> = (0..100_000).map(|i| (i % 251) as u8).collect();
        std::fs::File::create(&a).unwrap().write_all(&data).unwrap();
        std::fs::File::create(&b).unwrap().write_all(&data).unwrap();
        assert_eq!(fingerprint(&a), fingerprint(&b));
        let mut changed = data;
        changed[50_000] ^= 0xFF;
        std::fs::File::create(&b).unwrap().write_all(&changed).unwrap();
        assert_ne!(fingerprint(&a), fingerprint(&b));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
