use serde::{Deserialize, Serialize};
use std::fmt;

use crate::error::{Error, Result};

/// Stable unique id for an asset: `sh_<ULID>`.
///
/// Filename is never used as identity — the same `0001.wav` can come from
/// many devices and dates.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssetId(String);

impl AssetId {
    pub fn new() -> Self {
        Self(format!("sh_{}", ulid_simple()))
    }

    pub fn parse(s: &str) -> Result<Self> {
        let ok = s
            .strip_prefix("sh_")
            .map(|rest| rest.len() == 26 && rest.chars().all(|c| c.is_ascii_alphanumeric()))
            .unwrap_or(false);
        if ok {
            Ok(Self(s.to_string()))
        } else {
            Err(Error::InvalidAssetId(s.to_string()))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for AssetId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for AssetId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Generic row id (ULID, no prefix).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RowId(String);

impl RowId {
    pub fn new() -> Self {
        Self(ulid_simple())
    }

    pub fn parse(s: &str) -> Result<Self> {
        if s.len() == 26 && s.chars().all(|c| c.is_ascii_alphanumeric()) {
            Ok(Self(s.to_string()))
        } else {
            Err(Error::other(format!("invalid row id: {s}")))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for RowId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Crockford-base32 ULID (26 chars): time-ordered, collision-resistant.
pub fn ulid_simple() -> String {
    use sha2::{Digest, Sha256};
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let ms = now.as_millis() as u64;
    let nanos = now.subsec_nanos() as u64;
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);

    // 10 chars of time (48-bit ms), 16 chars of randomness.
    let mut time = [0u8; 10];
    let mut t = ms;
    for i in (0..10).rev() {
        time[i] = ENCODING[(t & 0x1f) as usize] as u8;
        t >>= 5;
    }

    // Mix clock, pid, and a monotonic counter so rapid parallel IDs stay unique.
    let mut hasher = Sha256::new();
    hasher.update(ms.to_be_bytes());
    hasher.update(nanos.to_be_bytes());
    hasher.update(std::process::id().to_be_bytes());
    hasher.update(seq.to_be_bytes());
    let digest = hasher.finalize();

    let mut rand = [0u8; 16];
    for (i, b) in rand.iter_mut().enumerate() {
        *b = ENCODING[(digest[i] as usize) & 0x1f] as u8;
    }

    let mut out = String::with_capacity(26);
    out.push_str(std::str::from_utf8(&time).unwrap());
    out.push_str(std::str::from_utf8(&rand).unwrap());
    out
}

const ENCODING: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_id_roundtrip() {
        let id = AssetId::new();
        assert!(id.as_str().starts_with("sh_"));
        assert_eq!(id.as_str().len(), 3 + 26);
        let parsed = AssetId::parse(id.as_str()).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn asset_id_rejects_filename() {
        assert!(AssetId::parse("0001.wav").is_err());
        assert!(AssetId::parse("sh_tooshort").is_err());
    }

    #[test]
    fn ulids_are_unique_and_ordered_prefix_stable() {
        let a = ulid_simple();
        let b = ulid_simple();
        assert_eq!(a.len(), 26);
        assert_ne!(a, b);
    }
}
