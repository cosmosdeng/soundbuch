use std::path::Path;
use walkdir::WalkDir;

use crate::import::hasher::hash_file;
use crate::models::{ScanSummary, ScannedFile};

/// Audio extensions accepted in MVP. Anything else is `unsupported` and
/// recorded — never silently dropped.
pub const SUPPORTED_EXTENSIONS: &[&str] = &[
    "wav", "bwf", "aif", "aiff", "flac", "mp3", "m4a", "aac", "caf", "wma", "ogg", "opus",
];

pub fn is_supported_extension(ext: &str) -> bool {
    let e = ext.to_ascii_lowercase();
    SUPPORTED_EXTENSIONS.contains(&e.as_str())
}

/// Recursively discover audio files under `path` (file or folder).
/// Scanning never mutates the source.
pub fn scan_paths(paths: &[std::path::PathBuf]) -> Result<ScanSummary, std::io::Error> {
    let mut files = Vec::new();
    for p in paths {
        if p.is_file() {
            files.push(p.clone());
        } else if p.is_dir() {
            for entry in WalkDir::new(p).into_iter().flatten() {
                if entry.file_type().is_file() {
                    files.push(entry.path().to_path_buf());
                }
            }
        }
    }
    files.sort();
    files.dedup();

    let mut summary = ScanSummary::default();
    for path in files {
        let scanned = scan_one(&path);
        if scanned.supported && scanned.error.is_none() {
            summary.ready += 1;
        } else if scanned.duplicate_of.is_some() {
            summary.duplicate += 1;
        } else if !scanned.supported {
            summary.unsupported += 1;
        } else {
            summary.error += 1;
        }
        summary.files.push(scanned);
    }
    Ok(summary)
}

fn scan_one(path: &Path) -> ScannedFile {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

    let supported = ext.as_deref().map(is_supported_extension).unwrap_or(false);
    if !supported {
        return ScannedFile {
            path: path.display().to_string(),
            size,
            supported: false,
            hash: None,
            duplicate_of: None,
            error: Some(format!(
                "unsupported extension: {}",
                ext.unwrap_or_default()
            )),
        };
    }

    match hash_file(path) {
        Ok(hash) => ScannedFile {
            path: path.display().to_string(),
            size,
            supported: true,
            hash: Some(hash),
            duplicate_of: None,
            error: None,
        },
        Err(e) => ScannedFile {
            path: path.display().to_string(),
            size,
            supported: true,
            hash: None,
            duplicate_of: None,
            error: Some(e.to_string()),
        },
    }
}

/// Mark scan entries whose SHA-256 already exists in the library.
pub fn annotate_duplicates(
    summary: &mut ScanSummary,
    existing: &dyn Fn(&str) -> Option<String>,
) {
    for f in summary.files.iter_mut() {
        if let Some(hash) = f.hash.clone() {
            if let Some(existing_id) = existing(&hash) {
                f.duplicate_of = Some(existing_id);
            }
        }
    }
    // Recount categories after annotation.
    summary.ready = 0;
    summary.duplicate = 0;
    summary.unsupported = 0;
    summary.error = 0;
    for f in &summary.files {
        if f.duplicate_of.is_some() {
            summary.duplicate += 1;
        } else if !f.supported {
            summary.unsupported += 1;
        } else if f.error.is_some() {
            summary.error += 1;
        } else {
            summary.ready += 1;
        }
    }
}
