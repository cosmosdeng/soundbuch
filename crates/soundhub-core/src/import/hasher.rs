use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use crate::error::{Error, Result};

/// SHA-256 of a file, hex-encoded. Streaming so 2 GB recordings stay cheap.
pub fn hash_file(path: &Path) -> Result<String> {
    let file = File::open(path)?;
    let meta = file.metadata()?;
    if meta.len() == 0 {
        // Empty files hash deterministically; still treat as valid content id.
        return Ok(hash_bytes(b""));
    }
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 256];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub fn hash_bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// Copy `src` → `dst`, then re-hash `dst` and require it to match `expected`.
/// Only after this returns `Ok` may an asset become `ready`.
pub fn copy_and_verify(src: &Path, dst: &Path, expected_hash: &str) -> Result<String> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(src, dst)?;
    let actual = hash_file(dst)?;
    if !actual.eq_ignore_ascii_case(expected_hash) {
        // Do not leave a corrupt copy that looks valid.
        let _ = std::fs::remove_file(dst);
        return Err(Error::IntegrityMismatch {
            path: dst.display().to_string(),
            expected: expected_hash.to_string(),
            actual,
        });
    }
    Ok(actual)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn hash_is_stable() {
        let h1 = hash_bytes(b"hello");
        let h2 = hash_bytes(b"hello");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn copy_and_verify_detects_corruption() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src.wav");
        let dst = dir.path().join("dst.wav");
        let mut f = File::create(&src).unwrap();
        f.write_all(b"RIFF....WAVEfmt ").unwrap();
        let expected = hash_file(&src).unwrap();
        copy_and_verify(&src, &dst, &expected).unwrap();

        // Corrupt the source hash expectation → must fail and remove dst.
        let bad = copy_and_verify(&src, &dst, &"0".repeat(64));
        assert!(bad.is_err());
        assert!(!dst.exists());
    }
}
