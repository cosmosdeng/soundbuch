use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::RowId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportJobStatus {
    Queued,
    Scanning,
    Copying,
    Verifying,
    Indexing,
    Completed,
    Failed,
    Cancelled,
}

impl ImportJobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Scanning => "scanning",
            Self::Copying => "copying",
            Self::Verifying => "verifying",
            Self::Indexing => "indexing",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "queued" => Some(Self::Queued),
            "scanning" => Some(Self::Scanning),
            "copying" => Some(Self::Copying),
            "verifying" => Some(Self::Verifying),
            "indexing" => Some(Self::Indexing),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// One import run. Survives crashes so the user can Resume / Retry / Clean Up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportJob {
    pub id: RowId,
    pub created_at: DateTime<Utc>,
    pub source: String,
    pub total_files: u32,
    pub processed_files: u32,
    pub success_count: u32,
    pub failed_count: u32,
    pub skipped_count: u32,
    pub status: ImportJobStatus,
}

/// Per-file outcome inside a job. This is what makes crash recovery possible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportFileStatus {
    Pending,
    Copying,
    Verifying,
    Ready,
    Failed,
    SkippedDuplicate,
    SkippedUnsupported,
}

impl ImportFileStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Copying => "copying",
            Self::Verifying => "verifying",
            Self::Ready => "ready",
            Self::Failed => "failed",
            Self::SkippedDuplicate => "skipped_duplicate",
            Self::SkippedUnsupported => "skipped_unsupported",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "copying" => Some(Self::Copying),
            "verifying" => Some(Self::Verifying),
            "ready" => Some(Self::Ready),
            "failed" => Some(Self::Failed),
            "skipped_duplicate" => Some(Self::SkippedDuplicate),
            "skipped_unsupported" => Some(Self::SkippedUnsupported),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportFileRecord {
    pub id: RowId,
    pub job_id: RowId,
    pub source_path: String,
    pub asset_id: Option<String>,
    pub status: ImportFileStatus,
    pub error: Option<String>,
}

/// User choice when SHA-256 says the bytes already exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DuplicateAction {
    Skip,
    ImportAsDuplicate,
    Cancel,
}

/// What the scanner found, shown before the user confirms import.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanSummary {
    pub ready: u32,
    pub duplicate: u32,
    pub unsupported: u32,
    pub error: u32,
    pub files: Vec<ScannedFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannedFile {
    pub path: String,
    pub size: u64,
    pub supported: bool,
    pub hash: Option<String>,
    pub duplicate_of: Option<String>,
    pub error: Option<String>,
}
