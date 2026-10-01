use std::path::PathBuf;

use crate::error::{Error, Result};
use crate::ids::AssetId;

pub const DB_FILENAME: &str = "library.db";
pub const ASSETS_DIR: &str = "assets";
pub const METADATA_DIR: &str = "metadata";
pub const CACHE_DIR: &str = "cache";

/// Physical Library layout. Virtual Collections never create directories.
///
/// ```text
/// <Library>/
/// ├── library.db
/// ├── assets/
/// │   ├── ab/
/// │   │   └── sh_01J….wav
/// │   └── cd/
/// │       └── sh_01J….flac
/// ├── metadata/
/// └── cache/
/// ```
pub struct Library {
    pub root: PathBuf,
}

impl Library {
    /// Create a brand-new Library at `root`. Fails if `library.db` already exists.
    pub fn create(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let db_path = root.join(DB_FILENAME);
        if db_path.exists() {
            return Err(Error::LibraryExists(root.display().to_string()));
        }
        std::fs::create_dir_all(&root)?;
        std::fs::create_dir_all(root.join(ASSETS_DIR))?;
        std::fs::create_dir_all(root.join(METADATA_DIR))?;
        std::fs::create_dir_all(root.join(CACHE_DIR))?;
        let lib = Self { root };
        lib.open_db()?;
        Ok(lib)
    }

    /// Open an existing Library. Fails if `library.db` is missing.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let db_path = root.join(DB_FILENAME);
        if !db_path.exists() {
            return Err(Error::LibraryNotFound(root.display().to_string()));
        }
        let lib = Self { root };
        lib.open_db()?;
        Ok(lib)
    }

    /// Open or create — used by first-run flow and tests.
    pub fn open_or_create(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        if root.join(DB_FILENAME).exists() {
            Self::open(root)
        } else {
            Self::create(root)
        }
    }

    pub fn db_path(&self) -> PathBuf {
        self.root.join(DB_FILENAME)
    }

    pub fn open_db(&self) -> Result<rusqlite::Connection> {
        let conn = rusqlite::Connection::open(self.db_path())?;
        crate::db::schema::migrate(&conn)?;
        Ok(conn)
    }

    /// Physical path for an asset: `assets/<id[3..5]>/<id>.<ext>`.
    /// Asset id, not filename, is the on-disk identity — avoids collisions
    /// from devices that all emit `0001.wav`.
    pub fn asset_relpath(&self, id: &AssetId, extension: Option<&str>) -> String {
        let id_str = id.as_str();
        // `sh_` is 3 chars; next 2 chars form the shard folder.
        let shard: String = id_str
            .chars()
            .skip(3)
            .take(2)
            .collect::<String>()
            .to_ascii_lowercase();
        let name = match extension {
            Some(ext) if !ext.is_empty() => format!("{id_str}.{ext}"),
            _ => id_str.to_string(),
        };
        format!("{ASSETS_DIR}/{shard}/{name}")
    }

    pub fn asset_abspath(&self, relpath: &str) -> PathBuf {
        self.root.join(relpath)
    }

    /// Ensure parent directory exists for a relative asset path.
    pub fn prepare_asset_dir(&self, relpath: &str) -> Result<PathBuf> {
        let abs = self.asset_abspath(relpath);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(abs)
    }

    /// Relative path of the waveform peak cache: `cache/<asset_id>.peaks.json`.
    pub fn peaks_relpath(&self, id: &AssetId) -> String {
        format!("{CACHE_DIR}/{}.peaks.json", id.as_str())
    }

    pub fn peaks_abspath(&self, id: &AssetId) -> PathBuf {
        self.root.join(self.peaks_relpath(id))
    }

    /// Load cached peaks if present and well-formed.
    pub fn load_peaks(&self, id: &AssetId) -> Option<crate::audio::PeaksCache> {
        let path = self.peaks_abspath(id);
        let bytes = std::fs::read(path).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Persist computed peaks (recomputed later only if missing or invalid).
    pub fn save_peaks(&self, id: &AssetId, cache: &crate::audio::PeaksCache) -> Result<()> {
        let path = self.peaks_abspath(id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, serde_json::to_vec(cache)?)?;
        Ok(())
    }

    /// Called at startup: report (and optionally clean) half-finished work.
    /// Never marks incomplete assets as ready.
    pub fn inspect_recovery(&self, conn: &rusqlite::Connection) -> Result<RecoveryReport> {
        let repo = crate::db::Repo::new(conn);
        let jobs = repo.list_incomplete_import_jobs()?;
        let assets = repo.list_incomplete_assets()?;
        let mut orphan_files = Vec::new();

        // Files on disk whose asset row is not ready are "orphans" the user
        // can clean up — they must never be treated as valid assets.
        let assets_root = self.root.join(ASSETS_DIR);
        if assets_root.is_dir() {
            for entry in walkdir::WalkDir::new(&assets_root).into_iter().flatten() {
                if entry.file_type().is_file() {
                    orphan_files.push(entry.path().display().to_string());
                }
            }
        }

        Ok(RecoveryReport {
            incomplete_jobs: jobs,
            incomplete_assets: assets,
            library_files: orphan_files,
        })
    }
}

#[derive(Debug, serde::Serialize)]
pub struct RecoveryReport {
    pub incomplete_jobs: Vec<crate::models::ImportJob>,
    pub incomplete_assets: Vec<crate::models::Asset>,
    pub library_files: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::PeaksCache;

    #[test]
    fn peaks_cache_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::create(dir.path()).unwrap();
        let id = AssetId::new();
        assert!(lib.load_peaks(&id).is_none());

        let cache = PeaksCache {
            version: 1,
            source: "pcm".into(),
            peaks: vec![0.1, 0.5, 0.9],
        };
        lib.save_peaks(&id, &cache).unwrap();
        let loaded = lib.load_peaks(&id).expect("cache should load");
        assert_eq!(loaded.source, "pcm");
        assert_eq!(loaded.peaks, vec![0.1, 0.5, 0.9]);
        assert!(lib.peaks_abspath(&id).exists());
    }
}
