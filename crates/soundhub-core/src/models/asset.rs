use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::ids::AssetId;

/// Lifecycle of a single asset inside the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetStatus {
    /// Row created, copy not finished (crash-recoverable).
    Pending,
    /// Bytes are being copied into the library.
    Copying,
    /// Copy finished, hash verification in progress.
    Verifying,
    /// Fully imported and verified. The only "normal" browsing state.
    Ready,
    /// Import failed; source may be gone. Never looks like a ready asset.
    Failed,
}

impl AssetStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Copying => "copying",
            Self::Verifying => "verifying",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "copying" => Some(Self::Copying),
            "verifying" => Some(Self::Verifying),
            "ready" => Some(Self::Ready),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// Where a metadata value came from. User edits never destroy the original.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetadataSource {
    Filesystem,
    Bwf,
    Ixml,
    Exif,
    QuickTime,
    User,
    Derived,
}

impl MetadataSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Filesystem => "filesystem",
            Self::Bwf => "bwf",
            Self::Ixml => "ixml",
            Self::Exif => "exif",
            Self::QuickTime => "quicktime",
            Self::User => "user",
            Self::Derived => "derived",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "filesystem" => Some(Self::Filesystem),
            "bwf" => Some(Self::Bwf),
            "ixml" => Some(Self::Ixml),
            "exif" => Some(Self::Exif),
            "quicktime" => Some(Self::QuickTime),
            "user" => Some(Self::User),
            "derived" => Some(Self::Derived),
            _ => None,
        }
    }
}

/// One value of a field with provenance. Same key can exist multiple times
/// from different sources; `user` wins for display, originals are kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataValue {
    pub key: String,
    pub value: String,
    pub source: MetadataSource,
    pub recorded_at: Option<DateTime<Utc>>,
}

/// Filesystem facts captured at import time. `original_path` is historical
/// only — the source volume may be gone forever.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilesystemMeta {
    pub filename: String,
    pub extension: Option<String>,
    pub file_size: u64,
    pub created_at: Option<DateTime<Utc>>,
    pub modified_at: Option<DateTime<Utc>>,
    pub accessed_at: Option<DateTime<Utc>>,
    pub original_path: String,
    pub source_volume: Option<String>,
}

/// Audio technical parameters. Unknown fields stay `None`, never guessed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AudioMeta {
    pub duration_ms: Option<u64>,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u16>,
    pub channels: Option<u16>,
    pub channel_layout: Option<String>,
    pub codec: Option<String>,
    pub container: Option<String>,
    pub bitrate: Option<u32>,
    pub compression: Option<String>,
}

/// GPS as recorded in the file. Derived reverse-geocode never overwrites this.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GpsMeta {
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub altitude: Option<f64>,
    pub accuracy: Option<f64>,
    pub raw: Option<serde_json::Value>,
}

/// Full asset record — the single source of truth in the DB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub id: AssetId,
    pub filename: String,
    pub extension: Option<String>,
    pub file_size: u64,
    pub hash: String,
    pub mime_type: Option<String>,

    pub duration_ms: Option<u64>,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u16>,
    pub channels: Option<u16>,
    pub channel_layout: Option<String>,
    pub codec: Option<String>,
    pub container: Option<String>,
    pub bitrate: Option<u32>,
    pub compression: Option<String>,

    /// Times: never guessed. NULL when unknown.
    pub recorded_at: Option<DateTime<Utc>>,
    pub recording_start: Option<DateTime<Utc>>,
    pub recording_end: Option<DateTime<Utc>>,
    pub timezone: Option<String>,
    pub utc_offset_minutes: Option<i32>,

    pub file_created_at: Option<DateTime<Utc>>,
    pub file_modified_at: Option<DateTime<Utc>>,
    pub imported_at: DateTime<Utc>,

    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub altitude: Option<f64>,
    pub gps_accuracy: Option<f64>,

    /// Path on the source device at import time (historical).
    pub original_path: String,
    pub source_volume: Option<String>,
    /// Path relative to the Library root, e.g. `assets/ab/sh_01J….wav`.
    pub library_relpath: String,

    pub status: AssetStatus,
    pub parse_errors: Vec<String>,
    /// Unrecognized / format-specific metadata kept as JSON — never dropped.
    pub raw_metadata: BTreeMap<String, serde_json::Value>,
    /// Provenance-tracked field values.
    pub metadata: Vec<MetadataValue>,
    /// Set when the asset is in the recycle bin. `None` = live.
    pub deleted_at: Option<DateTime<Utc>>,
}

impl Asset {
    /// Content identity: two files with the same SHA-256 are the same bytes.
    pub fn content_hash(&self) -> &str {
        &self.hash
    }
}
