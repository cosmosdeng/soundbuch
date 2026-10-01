//! First-launch / Library setup support: recent libraries, path inspection,
//! and sensible default locations. Stored outside the Library itself so a
//! missing Library never loses this list.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const RECENT_FILE: &str = "recent_libraries.json";
const MAX_RECENT: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecentLibrary {
    pub path: String,
    pub opened_at: String,
    pub asset_count: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LibraryInspection {
    pub path: String,
    pub exists: bool,
    pub is_directory: bool,
    pub is_library: bool,
    pub has_db: bool,
    pub asset_count: Option<u64>,
    pub db_size_bytes: Option<u64>,
    pub free_space_bytes: Option<u64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DefaultLocation {
    pub path: String,
    pub label: String,
}

fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("soundbuch")
}

fn recent_path() -> PathBuf {
    config_dir().join(RECENT_FILE)
}

pub fn load_recent() -> Vec<RecentLibrary> {
    let p = recent_path();
    let Ok(bytes) = std::fs::read(&p) else {
        return Vec::new();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

pub fn push_recent(path: &str, asset_count: Option<u64>) {
    let mut list = load_recent();
    list.retain(|r| r.path != path);
    list.insert(
        0,
        RecentLibrary {
            path: path.to_string(),
            opened_at: chrono::Utc::now().to_rfc3339(),
            asset_count,
        },
    );
    list.truncate(MAX_RECENT);
    let dir = config_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(json) = serde_json::to_vec_pretty(&list) {
        let _ = std::fs::write(recent_path(), json);
    }
}

pub fn remove_recent(path: &str) {
    let mut list = load_recent();
    list.retain(|r| r.path != path);
    let dir = config_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(json) = serde_json::to_vec_pretty(&list) {
        let _ = std::fs::write(recent_path(), json);
    }
}

/// Inspect a path without opening it as the active Library.
pub fn inspect(path: &str) -> LibraryInspection {
    let p = PathBuf::from(path);
    let exists = p.exists();
    let is_directory = p.is_dir();
    let db = p.join("library.db");
    let has_db = db.is_file();

    let mut asset_count = None;
    let mut db_size_bytes = None;
    let mut error = None;

    if exists && !is_directory {
        error = Some("Path is not a folder".into());
    } else if exists && is_directory && !has_db {
        // Not a library yet — not an error for the Create flow.
        error = None;
    } else if has_db {
        db_size_bytes = std::fs::metadata(&db).ok().map(|m| m.len());
        // Cheap count via SQLite open; ignore failures (e.g. locked).
        match rusqlite::Connection::open_with_flags(
            &db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) {
            Ok(conn) => {
                asset_count = conn
                    .query_row(
                        "SELECT COUNT(*) FROM assets WHERE status = 'ready'",
                        [],
                        |r| r.get::<_, i64>(0),
                    )
                    .ok()
                    .map(|n| n as u64);
            }
            Err(e) => error = Some(format!("Cannot read library database: {e}")),
        }
    }

    LibraryInspection {
        path: path.to_string(),
        exists,
        is_directory,
        is_library: has_db,
        has_db,
        asset_count,
        db_size_bytes,
        free_space_bytes: free_space_hint(&p),
        error,
    }
}

fn free_space_hint(_path: &Path) -> Option<u64> {
    // Cross-platform free-space is awkward without extra crates.
    // Keep None for now; UI shows "—" when missing.
    None
}

/// Suggested parent folders for creating a new Library.
pub fn suggest_locations() -> Vec<DefaultLocation> {
    let mut out = Vec::new();

    if let Some(docs) = dirs::document_dir() {
        out.push(DefaultLocation {
            path: docs.display().to_string(),
            label: "Documents".into(),
        });
    }
    if let Some(home) = dirs::home_dir() {
        out.push(DefaultLocation {
            path: home.display().to_string(),
            label: "Home".into(),
        });
    }
    // Windows drive root hint
    if cfg!(windows) {
        out.push(DefaultLocation {
            path: "D:\\".into(),
            label: "D:\\".into(),
        });
    }
    out
}

/// Build the concrete Library path from a parent folder + name.
/// `name` empty → use parent as the Library root directly.
pub fn compose_library_path(parent: &str, name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        return parent.to_string();
    }
    let parent = Path::new(parent);
    // Avoid duplicating the name if user already selected a folder named that.
    if parent
        .file_name()
        .map(|n| n.to_string_lossy().eq_ignore_ascii_case(name))
        .unwrap_or(false)
    {
        return parent.display().to_string();
    }
    parent.join(name).display().to_string()
}

/// What `Library::create` will place on disk — shown before confirming.
pub fn creation_preview(path: &str) -> Vec<String> {
    vec![
        format!("{path}/library.db"),
        format!("{path}/assets/"),
        format!("{path}/metadata/"),
        format!("{path}/cache/"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_joins_parent_and_name() {
        let p = compose_library_path("D:\\", "soundbuch");
        assert!(p.ends_with("soundbuch"));
    }

    #[test]
    fn compose_empty_name_uses_parent() {
        assert_eq!(compose_library_path("/Volumes/AudioSSD", ""), "/Volumes/AudioSSD");
    }

    #[test]
    fn compose_avoids_duplicate_name_suffix() {
        let p = compose_library_path("/data/soundbuch", "soundbuch");
        assert_eq!(p, "/data/soundbuch");
    }

    #[test]
    fn inspect_missing_folder_is_not_library() {
        let insp = inspect("/definitely/not/a/real/path/soundhub");
        assert!(!insp.exists);
        assert!(!insp.is_library);
    }

    #[test]
    fn inspect_folder_without_db_is_not_library() {
        let dir = tempfile::tempdir().unwrap();
        let insp = inspect(dir.path().to_str().unwrap());
        assert!(insp.exists);
        assert!(insp.is_directory);
        assert!(!insp.is_library);
    }
}
