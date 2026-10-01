use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use std::path::Path;

use crate::error::{Error, Result};
use crate::ids::{AssetId, RowId};
use crate::models::*;

/// Thin data-access layer over SQLite. Every write that must be atomic goes
/// through a `Transaction` created by the caller (see `ImportPipeline`).
pub struct Repo<'a> {
    pub conn: &'a Connection,
}

impl<'a> Repo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    // ── Assets ─────────────────────────────────────────────────────────────

    /// Insert a pending asset. The copy has NOT been verified yet.
    pub fn insert_pending_asset(&self, tx: &Transaction<'_>, asset: &Asset) -> Result<()> {
        tx.execute(
            r#"
            INSERT INTO assets (
                id, filename, extension, file_size, hash, mime_type,
                duration_ms, sample_rate, bit_depth, channels, channel_layout,
                codec, container, bitrate, compression,
                recorded_at, recording_start, recording_end,
                timezone, utc_offset_minutes,
                file_created_at, file_modified_at, imported_at,
                latitude, longitude, altitude, gps_accuracy,
                original_path, source_volume, library_relpath,
                status, parse_errors, raw_metadata
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6,
                ?7, ?8, ?9, ?10, ?11,
                ?12, ?13, ?14, ?15,
                ?16, ?17, ?18,
                ?19, ?20,
                ?21, ?22, ?23,
                ?24, ?25, ?26, ?27,
                ?28, ?29, ?30,
                ?31, ?32, ?33
            )
            "#,
            params![
                asset.id.as_str(),
                asset.filename,
                asset.extension,
                asset.file_size as i64,
                asset.hash,
                asset.mime_type,
                asset.duration_ms.map(|v| v as i64),
                asset.sample_rate.map(|v| v as i64),
                asset.bit_depth.map(|v| v as i64),
                asset.channels.map(|v| v as i64),
                asset.channel_layout,
                asset.codec,
                asset.container,
                asset.bitrate.map(|v| v as i64),
                asset.compression,
                opt_dt(asset.recorded_at),
                opt_dt(asset.recording_start),
                opt_dt(asset.recording_end),
                asset.timezone,
                asset.utc_offset_minutes,
                opt_dt(asset.file_created_at),
                opt_dt(asset.file_modified_at),
                opt_dt(Some(asset.imported_at)),
                asset.latitude,
                asset.longitude,
                asset.altitude,
                asset.gps_accuracy,
                asset.original_path,
                asset.source_volume,
                asset.library_relpath,
                asset.status.as_str(),
                serde_json::to_string(&asset.parse_errors)?,
                serde_json::to_string(&asset.raw_metadata)?,
            ],
        )?;

        for mv in &asset.metadata {
            tx.execute(
                r#"
                INSERT INTO metadata_values (id, asset_id, key, value, source, recorded_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                "#,
                params![
                    RowId::new().as_str(),
                    asset.id.as_str(),
                    mv.key,
                    mv.value,
                    mv.source.as_str(),
                    opt_dt(mv.recorded_at),
                ],
            )?;
        }
        Ok(())
    }

    /// Flip status after a phase completes. Uses the same connection
    /// transaction so a crash mid-copy cannot leave `ready` without a file.
    pub fn set_asset_status(
        &self,
        tx: &Transaction<'_>,
        id: &AssetId,
        status: AssetStatus,
    ) -> Result<()> {
        let n = tx.execute(
            "UPDATE assets SET status = ?1 WHERE id = ?2",
            params![status.as_str(), id.as_str()],
        )?;
        if n == 0 {
            return Err(Error::AssetNotFound(id.to_string()));
        }
        Ok(())
    }

    pub fn get_asset(&self, id: &AssetId) -> Result<Option<Asset>> {
        let mut stmt = self.conn.prepare("SELECT * FROM assets WHERE id = ?1")?;
        let mut rows = stmt.query_map(params![id.as_str()], row_to_asset)?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    /// Find an existing ready asset with the same SHA-256 (duplicate check).
    pub fn find_by_hash(&self, hash: &str) -> Result<Option<AssetId>> {
        let id: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM assets WHERE hash = ?1 AND status = 'ready' AND deleted_at IS NULL LIMIT 1",
                params![hash],
                |r| r.get(0),
            )
            .optional()?;
        Ok(id.and_then(|s| AssetId::parse(&s).ok()))
    }

    /// All live ready assets with this content hash, oldest import first.
    pub fn list_ready_by_hash(&self, hash: &str) -> Result<Vec<Asset>> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM assets \
             WHERE hash = ?1 AND status = 'ready' AND deleted_at IS NULL \
             ORDER BY imported_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(params![hash], row_to_asset)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn list_assets(&self, limit: u32, offset: u32) -> Result<Vec<Asset>> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM assets WHERE status = 'ready' AND deleted_at IS NULL \
             ORDER BY imported_at DESC LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt.query_map(params![limit as i64, offset as i64], row_to_asset)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn count_ready_assets(&self) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM assets WHERE status = 'ready' AND deleted_at IS NULL",
            [],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    // ── Import jobs ────────────────────────────────────────────────────────

    pub fn insert_import_job(&self, tx: &Transaction<'_>, job: &ImportJob) -> Result<()> {
        tx.execute(
            r#"
            INSERT INTO import_jobs
                (id, created_at, source, total_files, processed_files,
                 success_count, failed_count, skipped_count, status)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
            params![
                job.id.as_str(),
                opt_dt(Some(job.created_at)),
                job.source,
                job.total_files as i64,
                job.processed_files as i64,
                job.success_count as i64,
                job.failed_count as i64,
                job.skipped_count as i64,
                job.status.as_str(),
            ],
        )?;
        Ok(())
    }

    pub fn insert_import_file(&self, tx: &Transaction<'_>, rec: &ImportFileRecord) -> Result<()> {
        tx.execute(
            r#"
            INSERT INTO import_files (id, job_id, source_path, asset_id, status, error)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
            params![
                rec.id.as_str(),
                rec.job_id.as_str(),
                rec.source_path,
                rec.asset_id,
                rec.status.as_str(),
                rec.error,
            ],
        )?;
        Ok(())
    }

    pub fn update_import_file(
        &self,
        tx: &Transaction<'_>,
        id: &RowId,
        status: ImportFileStatus,
        asset_id: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        tx.execute(
            "UPDATE import_files SET status = ?1, asset_id = ?2, error = ?3 WHERE id = ?4",
            params![status.as_str(), asset_id, error, id.as_str()],
        )?;
        Ok(())
    }

    pub fn update_import_job_progress(&self, tx: &Transaction<'_>, job: &ImportJob) -> Result<()> {
        tx.execute(
            r#"
            UPDATE import_jobs SET
                total_files = ?1, processed_files = ?2, success_count = ?3,
                failed_count = ?4, skipped_count = ?5, status = ?6
            WHERE id = ?7
            "#,
            params![
                job.total_files as i64,
                job.processed_files as i64,
                job.success_count as i64,
                job.failed_count as i64,
                job.skipped_count as i64,
                job.status.as_str(),
                job.id.as_str(),
            ],
        )?;
        Ok(())
    }

    pub fn get_import_job(&self, id: &RowId) -> Result<Option<ImportJob>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM import_jobs WHERE id = ?1")?;
        let mut rows = stmt.query_map(params![id.as_str()], row_to_job)?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    /// Jobs left in a non-terminal state after a crash.
    pub fn list_incomplete_import_jobs(&self) -> Result<Vec<ImportJob>> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM import_jobs WHERE status NOT IN ('completed','failed','cancelled') ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], row_to_job)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn list_import_files(&self, job_id: &RowId) -> Result<Vec<ImportFileRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM import_files WHERE job_id = ?1 ORDER BY id")?;
        let rows = stmt.query_map(params![job_id.as_str()], row_to_import_file)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Assets stuck in non-ready states — needed for crash cleanup.
    pub fn list_incomplete_assets(&self) -> Result<Vec<Asset>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM assets WHERE status != 'ready'")?;
        let rows = stmt.query_map([], row_to_asset)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // ── Organization ───────────────────────────────────────────────────────

    pub fn upsert_tag(&self, name: &str) -> Result<Tag> {
        let existing: Option<String> = self
            .conn
            .query_row("SELECT id FROM tags WHERE name = ?1", params![name], |r| {
                r.get(0)
            })
            .optional()?;
        if let Some(id) = existing {
            return Ok(Tag {
                id: RowId::parse(&id)?,
                name: name.to_string(),
            });
        }
        let id = RowId::new();
        self.conn.execute(
            "INSERT INTO tags (id, name) VALUES (?1, ?2)",
            params![id.as_str(), name],
        )?;
        Ok(Tag {
            id,
            name: name.to_string(),
        })
    }

    pub fn add_asset_tag(&self, asset_id: &AssetId, tag_id: &RowId) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO asset_tags (asset_id, tag_id) VALUES (?1, ?2)",
            params![asset_id.as_str(), tag_id.as_str()],
        )?;
        self.reindex_asset_in(&tx, asset_id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn create_person(&self, name: &str, note: Option<&str>) -> Result<Person> {
        let id = RowId::new();
        self.conn.execute(
            "INSERT INTO people (id, name, note, metadata) VALUES (?1, ?2, ?3, NULL)",
            params![id.as_str(), name, note],
        )?;
        Ok(Person {
            id,
            name: name.to_string(),
            note: note.map(|s| s.to_string()),
            metadata: None,
        })
    }

    pub fn link_asset_person(&self, asset_id: &AssetId, person_id: &RowId) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO asset_people (asset_id, person_id) VALUES (?1, ?2)",
            params![asset_id.as_str(), person_id.as_str()],
        )?;
        self.reindex_asset_in(&tx, asset_id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn create_collection(&self, name: &str, parent_id: Option<&RowId>) -> Result<Collection> {
        let id = RowId::new();
        self.conn.execute(
            "INSERT INTO collections (id, name, parent_id, collection_type, rules) VALUES (?1, ?2, ?3, 'static', NULL)",
            params![id.as_str(), name, parent_id.map(|p| p.as_str())],
        )?;
        Ok(Collection {
            id,
            name: name.to_string(),
            parent_id: parent_id.cloned(),
            collection_type: CollectionType::Static,
            rules: None,
        })
    }

    // ── Playlists ──────────────────────────────────────────────────────────

    pub fn create_playlist(&self, name: &str) -> Result<Playlist> {
        let id = RowId::new();
        let created_at = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO playlists (id, name, created_at) VALUES (?1, ?2, ?3)",
            params![id.as_str(), name, created_at],
        )?;
        Ok(Playlist {
            id,
            name: name.to_string(),
            created_at,
            track_count: 0,
        })
    }

    pub fn rename_playlist(&self, playlist_id: &RowId, new_name: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE playlists SET name = ?1 WHERE id = ?2",
            params![new_name, playlist_id.as_str()],
        )?;
        if n == 0 {
            return Err(Error::other(format!("playlist {playlist_id} not found")));
        }
        Ok(())
    }

    pub fn delete_playlist(&self, playlist_id: &RowId) -> Result<()> {
        // playlist_tracks cascades.
        self.conn.execute(
            "DELETE FROM playlists WHERE id = ?1",
            params![playlist_id.as_str()],
        )?;
        Ok(())
    }

    pub fn get_playlist(&self, playlist_id: &RowId) -> Result<Option<Playlist>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.id, p.name, p.created_at, \
               (SELECT COUNT(*) FROM playlist_tracks t WHERE t.playlist_id = p.id) AS n \
             FROM playlists p WHERE p.id = ?1",
        )?;
        let mut rows = stmt.query_map(params![playlist_id.as_str()], |r| {
            Ok(Playlist {
                id: RowId::parse(&r.get::<_, String>(0)?).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                name: r.get(1)?,
                created_at: r.get(2)?,
                track_count: r.get::<_, i64>(3)? as u64,
            })
        })?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    pub fn list_playlists(&self) -> Result<Vec<Playlist>> {
        let mut stmt = self.conn.prepare(
            "SELECT p.id, p.name, p.created_at, \
               (SELECT COUNT(*) FROM playlist_tracks t WHERE t.playlist_id = p.id) AS n \
             FROM playlists p ORDER BY p.created_at ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Playlist {
                id: RowId::parse(&r.get::<_, String>(0)?).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                name: r.get(1)?,
                created_at: r.get(2)?,
                track_count: r.get::<_, i64>(3)? as u64,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Append (or no-op if already present). Returns the new position.
    pub fn playlist_add_track(&self, playlist_id: &RowId, asset_id: &AssetId) -> Result<u32> {
        let exists: Option<i64> = self
            .conn
            .query_row(
                "SELECT position FROM playlist_tracks WHERE playlist_id = ?1 AND asset_id = ?2",
                params![playlist_id.as_str(), asset_id.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(pos) = exists {
            return Ok(pos as u32);
        }
        let next: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM playlist_tracks WHERE playlist_id = ?1",
            params![playlist_id.as_str()],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO playlist_tracks (playlist_id, asset_id, position) VALUES (?1, ?2, ?3)",
            params![playlist_id.as_str(), asset_id.as_str(), next],
        )?;
        Ok(next as u32)
    }

    pub fn playlist_remove_track(&self, playlist_id: &RowId, asset_id: &AssetId) -> Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM playlist_tracks WHERE playlist_id = ?1 AND asset_id = ?2",
            params![playlist_id.as_str(), asset_id.as_str()],
        )?;
        if n == 0 {
            return Ok(false);
        }
        self.renumber_playlist(playlist_id)?;
        Ok(true)
    }

    /// Replace the track order with `asset_ids` (must be the full membership).
    pub fn playlist_reorder(&self, playlist_id: &RowId, asset_ids: &[AssetId]) -> Result<()> {
        let current: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT asset_id FROM playlist_tracks WHERE playlist_id = ?1")?;
            let rows = stmt.query_map(params![playlist_id.as_str()], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut wanted: Vec<String> = asset_ids.iter().map(|a| a.to_string()).collect();
        wanted.sort();
        let mut have = current.clone();
        have.sort();
        if wanted != have {
            return Err(Error::other(
                "playlist_reorder: asset list must match current membership exactly",
            ));
        }

        let tx = self.conn.unchecked_transaction()?;
        for (i, id) in asset_ids.iter().enumerate() {
            tx.execute(
                "UPDATE playlist_tracks SET position = ?1 WHERE playlist_id = ?2 AND asset_id = ?3",
                params![i as i64, playlist_id.as_str(), id.as_str()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Move a track from one index to another (shifts the rest).
    pub fn playlist_move_track(&self, playlist_id: &RowId, from: u32, to: u32) -> Result<()> {
        let mut tracks = self.list_playlist_tracks(playlist_id)?;
        if from as usize >= tracks.len() || to as usize >= tracks.len() {
            return Err(Error::other("playlist_move_track: index out of range"));
        }
        if from == to {
            return Ok(());
        }
        let item = tracks.remove(from as usize);
        tracks.insert(to as usize, item);
        let order: Vec<AssetId> = tracks.into_iter().map(|t| t.asset_id).collect();
        self.playlist_reorder(playlist_id, &order)
    }

    pub fn list_playlist_tracks(&self, playlist_id: &RowId) -> Result<Vec<PlaylistTrack>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.position, a.id, a.filename, a.duration_ms, a.file_size \
             FROM playlist_tracks t \
             JOIN assets a ON a.id = t.asset_id \
             WHERE t.playlist_id = ?1 \
             ORDER BY t.position ASC",
        )?;
        let rows = stmt.query_map(params![playlist_id.as_str()], |r| {
            Ok(PlaylistTrack {
                position: r.get::<_, i64>(0)? as u32,
                asset_id: AssetId::parse(&r.get::<_, String>(1)?).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                filename: r.get(2)?,
                duration_ms: r.get::<_, Option<i64>>(3)?.map(|v| v as u64),
                file_size: r.get::<_, i64>(4)? as u64,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    fn renumber_playlist(&self, playlist_id: &RowId) -> Result<()> {
        let ids: Vec<String> = {
            let mut stmt = self.conn.prepare(
                "SELECT asset_id FROM playlist_tracks WHERE playlist_id = ?1 ORDER BY position ASC",
            )?;
            let rows = stmt.query_map(params![playlist_id.as_str()], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let tx = self.conn.unchecked_transaction()?;
        for (i, id) in ids.iter().enumerate() {
            tx.execute(
                "UPDATE playlist_tracks SET position = ?1 WHERE playlist_id = ?2 AND asset_id = ?3",
                params![i as i64, playlist_id.as_str(), id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Create a rule-driven collection. Membership is computed on read —
    /// nothing is written to `asset_collections`.
    pub fn create_smart_collection(&self, name: &str, rules: &SmartRules) -> Result<Collection> {
        let id = RowId::new();
        let rules_json = crate::collections::rules_to_value(rules);
        self.conn.execute(
            "INSERT INTO collections (id, name, parent_id, collection_type, rules) \
             VALUES (?1, ?2, NULL, 'smart', ?3)",
            params![id.as_str(), name, rules_json.to_string()],
        )?;
        Ok(Collection {
            id,
            name: name.to_string(),
            parent_id: None,
            collection_type: CollectionType::Smart,
            rules: Some(rules_json),
        })
    }

    /// Replace the rules of a smart collection.
    pub fn update_collection_rules(&self, collection_id: &RowId, rules: &SmartRules) -> Result<()> {
        let rules_json = crate::collections::rules_to_value(rules);
        let n = self.conn.execute(
            "UPDATE collections SET rules = ?1 WHERE id = ?2 AND collection_type = 'smart'",
            params![rules_json.to_string(), collection_id.as_str()],
        )?;
        if n == 0 {
            return Err(Error::other(format!(
                "collection {collection_id} is not a smart collection"
            )));
        }
        Ok(())
    }

    pub fn get_collection(&self, id: &RowId) -> Result<Option<Collection>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, parent_id, collection_type, rules FROM collections WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id.as_str()], |r| {
            let id_s: String = r.get(0)?;
            let parent: Option<String> = r.get(2)?;
            let ctype: String = r.get(3)?;
            let rules: Option<String> = r.get(4)?;
            Ok(Collection {
                id: RowId::parse(&id_s).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                name: r.get(1)?,
                parent_id: parent.and_then(|p| RowId::parse(&p).ok()),
                collection_type: CollectionType::parse(&ctype).unwrap_or(CollectionType::Static),
                rules: rules.and_then(|s| serde_json::from_str(&s).ok()),
            })
        })?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    /// Assets in a collection. Static = membership table; Smart = evaluate rules.
    pub fn list_collection_assets(&self, collection_id: &RowId) -> Result<Vec<AssetId>> {
        let col = self
            .get_collection(collection_id)?
            .ok_or_else(|| Error::other(format!("collection {collection_id} not found")))?;
        match col.collection_type {
            CollectionType::Static => {
                let mut stmt = self
                    .conn
                    .prepare("SELECT asset_id FROM asset_collections WHERE collection_id = ?1")?;
                let rows =
                    stmt.query_map(params![collection_id.as_str()], |r| r.get::<_, String>(0))?;
                let mut out = Vec::new();
                for row in rows {
                    let s = row?;
                    if let Ok(id) = AssetId::parse(&s) {
                        out.push(id);
                    }
                }
                Ok(out)
            }
            CollectionType::Smart => {
                let rules = col
                    .rules
                    .as_ref()
                    .and_then(crate::collections::parse_rules)
                    .unwrap_or(SmartRules {
                        match_mode: MatchMode::default(),
                        conditions: vec![],
                    });
                crate::collections::evaluate(self.conn, &rules)
            }
        }
    }

    pub fn add_asset_collection(&self, asset_id: &AssetId, collection_id: &RowId) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO asset_collections (asset_id, collection_id) VALUES (?1, ?2)",
            params![asset_id.as_str(), collection_id.as_str()],
        )?;
        self.reindex_asset_in(&tx, asset_id)?;
        tx.commit()?;
        Ok(())
    }

    /// Removing an asset from a Collection must NOT delete the asset file.
    pub fn remove_asset_from_collection(
        &self,
        asset_id: &AssetId,
        collection_id: &RowId,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM asset_collections WHERE asset_id = ?1 AND collection_id = ?2",
            params![asset_id.as_str(), collection_id.as_str()],
        )?;
        self.reindex_asset_in(&tx, asset_id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn remove_asset_tag(&self, asset_id: &AssetId, tag_id: &RowId) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM asset_tags WHERE asset_id = ?1 AND tag_id = ?2",
            params![asset_id.as_str(), tag_id.as_str()],
        )?;
        self.reindex_asset_in(&tx, asset_id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn remove_asset_person(&self, asset_id: &AssetId, person_id: &RowId) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM asset_people WHERE asset_id = ?1 AND person_id = ?2",
            params![asset_id.as_str(), person_id.as_str()],
        )?;
        self.reindex_asset_in(&tx, asset_id)?;
        tx.commit()?;
        Ok(())
    }

    /// Delete an incomplete asset row (and cascades). Never call for `ready`.
    /// Also drops the FTS row — `assets_fts` is a virtual table with no FK cascade.
    pub fn delete_asset(&self, asset_id: &AssetId) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let n = tx.execute(
            "DELETE FROM assets WHERE id = ?1 AND status != 'ready'",
            params![asset_id.as_str()],
        )?;
        if n > 0 {
            tx.execute(
                "DELETE FROM assets_fts WHERE asset_id = ?1",
                params![asset_id.as_str()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    // ── Recycle bin ────────────────────────────────────────────────────────

    /// Soft-delete ready assets: stamp `deleted_at`, drop from FTS.
    /// The library file is kept until `purge_asset` / `empty_trash`.
    pub fn soft_delete_assets(&self, asset_ids: &[AssetId]) -> Result<u32> {
        let now = Utc::now().to_rfc3339();
        let tx = self.conn.unchecked_transaction()?;
        let mut n = 0u32;
        for id in asset_ids {
            let changed = tx.execute(
                "UPDATE assets SET deleted_at = ?1 WHERE id = ?2 AND status = 'ready' AND deleted_at IS NULL",
                params![now, id.as_str()],
            )?;
            if changed > 0 {
                tx.execute(
                    "DELETE FROM assets_fts WHERE asset_id = ?1",
                    params![id.as_str()],
                )?;
                n += 1;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// Bring assets back from the recycle bin and reindex them.
    pub fn restore_assets(&self, asset_ids: &[AssetId]) -> Result<u32> {
        let tx = self.conn.unchecked_transaction()?;
        let mut n = 0u32;
        for id in asset_ids {
            let changed = tx.execute(
                "UPDATE assets SET deleted_at = NULL WHERE id = ?1 AND deleted_at IS NOT NULL",
                params![id.as_str()],
            )?;
            if changed > 0 {
                self.reindex_asset_in(&tx, id)?;
                n += 1;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// Permanently remove a trashed asset: DB rows + FTS + library copy.
    /// Never touches the original source file. Live (non-trashed) assets
    /// are left alone — including their library file.
    pub fn purge_asset(&self, asset_id: &AssetId, abs_path: &Path) -> Result<()> {
        let trashed: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM assets WHERE id = ?1 AND deleted_at IS NOT NULL",
                params![asset_id.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        if trashed.is_none() {
            return Ok(()); // not in the recycle bin — nothing to purge
        }
        if abs_path.exists() {
            let _ = std::fs::remove_file(abs_path);
        }
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM assets_fts WHERE asset_id = ?1",
            params![asset_id.as_str()],
        )?;
        // CASCADE takes metadata_values / asset_* join rows.
        tx.execute(
            "DELETE FROM assets WHERE id = ?1 AND deleted_at IS NOT NULL",
            params![asset_id.as_str()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_deleted_assets(&self, limit: u32, offset: u32) -> Result<Vec<Asset>> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM assets WHERE deleted_at IS NOT NULL \
             ORDER BY deleted_at DESC LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt.query_map(params![limit as i64, offset as i64], row_to_asset)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn count_deleted_assets(&self) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM assets WHERE deleted_at IS NOT NULL",
            [],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    // ── Undo log ───────────────────────────────────────────────────────────

    /// Record a reversible action. `payload` is JSON of `UndoAction` or
    /// `{"actions": [UndoAction, ...]}` for batches.
    pub fn push_undo(
        &self,
        kind: UndoKind,
        label: &str,
        payload: &serde_json::Value,
    ) -> Result<String> {
        let id = RowId::new();
        self.conn.execute(
            "INSERT INTO undo_log (id, created_at, kind, label, payload) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id.as_str(),
                Utc::now().to_rfc3339(),
                kind.as_str(),
                label,
                payload.to_string(),
            ],
        )?;
        Ok(id.to_string())
    }

    /// Most recent undo entry, if any.
    pub fn peek_undo(&self) -> Result<Option<UndoEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, kind, label, payload FROM undo_log \
             ORDER BY created_at DESC, rowid DESC LIMIT 1",
        )?;
        let mut rows = stmt.query_map([], row_to_undo)?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    /// Remove the newest undo entry and return it.
    pub fn pop_undo(&self) -> Result<Option<UndoEntry>> {
        let entry = self.peek_undo()?;
        if let Some(e) = &entry {
            self.conn
                .execute("DELETE FROM undo_log WHERE id = ?1", params![e.id])?;
        }
        Ok(entry)
    }

    pub fn count_undo_entries(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM undo_log", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    /// Reset failed import files back to pending so they can be retried.
    pub fn reset_failed_import_files(&self, job_id: &RowId) -> Result<u32> {
        let n = self.conn.execute(
            "UPDATE import_files SET status = 'pending', error = NULL WHERE job_id = ?1 AND status = 'failed'",
            params![job_id.as_str()],
        )?;
        Ok(n as u32)
    }

    pub fn set_import_job_status(
        &self,
        tx: &Transaction<'_>,
        job_id: &RowId,
        status: ImportJobStatus,
    ) -> Result<()> {
        tx.execute(
            "UPDATE import_jobs SET status = ?1 WHERE id = ?2",
            params![status.as_str(), job_id.as_str()],
        )?;
        Ok(())
    }

    pub fn list_tags(&self) -> Result<Vec<Tag>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name FROM tags ORDER BY name COLLATE NOCASE")?;
        let rows = stmt.query_map([], |r| {
            Ok(Tag {
                id: RowId::parse(&r.get::<_, String>(0)?).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                name: r.get(1)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Tags with live-asset usage counts (for the tag manager).
    pub fn list_tags_with_usage(&self) -> Result<Vec<TagUsage>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id, t.name, \
               (SELECT COUNT(*) FROM asset_tags at \
                JOIN assets a ON a.id = at.asset_id \
                WHERE at.tag_id = t.id \
                  AND a.deleted_at IS NULL AND a.status = 'ready') AS n \
             FROM tags t \
             ORDER BY t.name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(TagUsage {
                id: RowId::parse(&r.get::<_, String>(0)?).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                name: r.get(1)?,
                asset_count: r.get::<_, i64>(2)? as u64,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Rename a tag. Returns the previous name so the change is undoable.
    pub fn rename_tag(&self, tag_id: &RowId, new_name: &str) -> Result<String> {
        let old: String = self
            .conn
            .query_row(
                "SELECT name FROM tags WHERE id = ?1",
                params![tag_id.as_str()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| Error::other(format!("tag {tag_id} not found")))?;

        // Unique constraint on tags.name — refuse collisions.
        let clash: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM tags WHERE name = ?1 AND id != ?2",
                params![new_name, tag_id.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        if clash.is_some() {
            return Err(Error::other(format!("tag “{new_name}” already exists")));
        }

        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE tags SET name = ?1 WHERE id = ?2",
            params![new_name, tag_id.as_str()],
        )?;
        // Refresh FTS for every asset carrying this tag.
        let asset_ids: Vec<String> = {
            let mut stmt = tx.prepare("SELECT asset_id FROM asset_tags WHERE tag_id = ?1")?;
            let rows = stmt.query_map(params![tag_id.as_str()], |r| r.get::<_, String>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        for aid in asset_ids {
            if let Ok(a) = AssetId::parse(&aid) {
                self.reindex_asset_in(&tx, &a)?;
            }
        }
        tx.commit()?;
        Ok(old)
    }

    /// Delete a tag entirely: unlink from all assets, drop the row, reindex.
    /// Returns (tag_name, affected_asset_ids) for undo.
    pub fn delete_tag(&self, tag_id: &RowId) -> Result<(String, Vec<AssetId>)> {
        let name: String = self
            .conn
            .query_row(
                "SELECT name FROM tags WHERE id = ?1",
                params![tag_id.as_str()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| Error::other(format!("tag {tag_id} not found")))?;

        let asset_ids: Vec<AssetId> = {
            let mut stmt = self
                .conn
                .prepare("SELECT asset_id FROM asset_tags WHERE tag_id = ?1")?;
            let rows = stmt.query_map(params![tag_id.as_str()], |r| r.get::<_, String>(0))?;
            let mut out = Vec::new();
            for row in rows {
                let s = row?;
                if let Ok(id) = AssetId::parse(&s) {
                    out.push(id);
                }
            }
            out
        };

        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM asset_tags WHERE tag_id = ?1",
            params![tag_id.as_str()],
        )?;
        tx.execute("DELETE FROM tags WHERE id = ?1", params![tag_id.as_str()])?;
        for id in &asset_ids {
            self.reindex_asset_in(&tx, id)?;
        }
        tx.commit()?;
        Ok((name, asset_ids))
    }

    /// Merge `from_id` into `to_id`: move links, delete the source tag.
    /// Returns (from_name, to_id, asset_ids_that_had_from) for undo.
    pub fn merge_tags(
        &self,
        from_id: &RowId,
        to_id: &RowId,
    ) -> Result<(String, RowId, Vec<AssetId>)> {
        if from_id == to_id {
            return Err(Error::other("cannot merge a tag into itself"));
        }
        let from_name: String = self
            .conn
            .query_row(
                "SELECT name FROM tags WHERE id = ?1",
                params![from_id.as_str()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| Error::other(format!("tag {from_id} not found")))?;
        let _: String = self
            .conn
            .query_row(
                "SELECT name FROM tags WHERE id = ?1",
                params![to_id.as_str()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| Error::other(format!("tag {to_id} not found")))?;

        let affected: Vec<AssetId> = {
            let mut stmt = self
                .conn
                .prepare("SELECT asset_id FROM asset_tags WHERE tag_id = ?1")?;
            let rows = stmt.query_map(params![from_id.as_str()], |r| r.get::<_, String>(0))?;
            let mut out = Vec::new();
            for row in rows {
                let s = row?;
                if let Ok(id) = AssetId::parse(&s) {
                    out.push(id);
                }
            }
            out
        };

        let tx = self.conn.unchecked_transaction()?;
        // INSERT OR IGNORE so assets already carrying both tags stay clean.
        tx.execute(
            "INSERT OR IGNORE INTO asset_tags (asset_id, tag_id) \
             SELECT asset_id, ?2 FROM asset_tags WHERE tag_id = ?1",
            params![from_id.as_str(), to_id.as_str()],
        )?;
        tx.execute(
            "DELETE FROM asset_tags WHERE tag_id = ?1",
            params![from_id.as_str()],
        )?;
        tx.execute("DELETE FROM tags WHERE id = ?1", params![from_id.as_str()])?;
        for id in &affected {
            self.reindex_asset_in(&tx, id)?;
        }
        tx.commit()?;
        Ok((from_name, to_id.clone(), affected))
    }

    pub fn list_collections(&self) -> Result<Vec<Collection>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, parent_id, collection_type, rules FROM collections ORDER BY name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |r| {
            let id: String = r.get(0)?;
            let parent: Option<String> = r.get(2)?;
            let ctype: String = r.get(3)?;
            let rules: Option<String> = r.get(4)?;
            Ok(Collection {
                id: RowId::parse(&id).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                name: r.get(1)?,
                parent_id: parent.and_then(|p| RowId::parse(&p).ok()),
                collection_type: CollectionType::parse(&ctype).unwrap_or(CollectionType::Static),
                rules: rules.and_then(|s| serde_json::from_str(&s).ok()),
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    pub fn list_people(&self) -> Result<Vec<Person>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, note, metadata FROM people ORDER BY name COLLATE NOCASE")?;
        let rows = stmt.query_map([], |r| {
            let id: String = r.get(0)?;
            let metadata: Option<String> = r.get(3)?;
            Ok(Person {
                id: RowId::parse(&id).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
                name: r.get(1)?,
                note: r.get(2)?,
                metadata: metadata.and_then(|s| serde_json::from_str(&s).ok()),
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // ── Search ─────────────────────────────────────────────────────────────

    /// Rebuild the FTS row for one asset from current DB state.
    ///
    /// Single entry point for every relation mutation and for import
    /// completion — never sprinkle `index_asset` calls with hand-built strings.
    /// Reads tags / people / collections / metadata_values from the DB so the
    /// index always matches the relations actually linked.
    pub fn reindex_asset_in(&self, tx: &Transaction<'_>, asset_id: &AssetId) -> Result<()> {
        let asset = self
            .get_asset(asset_id)?
            .ok_or_else(|| Error::AssetNotFound(asset_id.to_string()))?;

        let people_names = join_names(
            tx,
            "SELECT p.name FROM people p \
             JOIN asset_people ap ON ap.person_id = p.id \
             WHERE ap.asset_id = ?1",
            asset_id,
        )?;
        let tags = join_names(
            tx,
            "SELECT t.name FROM tags t \
             JOIN asset_tags at ON at.tag_id = t.id \
             WHERE at.asset_id = ?1",
            asset_id,
        )?;
        let collections = join_names(
            tx,
            "SELECT c.name FROM collections c \
             JOIN asset_collections ac ON ac.collection_id = c.id \
             WHERE ac.asset_id = ?1",
            asset_id,
        )?;

        let mut loc_parts = Vec::new();
        if let Some(lat) = asset.latitude {
            loc_parts.push(lat.to_string());
        }
        if let Some(lon) = asset.longitude {
            loc_parts.push(lon.to_string());
        }
        let location_text = loc_parts.join(" ");

        let mut meta_parts = Vec::new();
        if let Some(codec) = &asset.codec {
            meta_parts.push(codec.clone());
        }
        if let Some(container) = &asset.container {
            meta_parts.push(container.clone());
        }
        {
            let mut stmt =
                tx.prepare("SELECT key, value FROM metadata_values WHERE asset_id = ?1")?;
            let rows = stmt.query_map(params![asset_id.as_str()], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (k, v) = row?;
                meta_parts.push(format!("{k} {v}"));
            }
        }
        let metadata_text = meta_parts.join(" ");

        // No user-notes column yet; BWF description is the closest free text.
        let notes = asset
            .raw_metadata
            .get("bwf_description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        self.index_asset(
            tx,
            asset_id,
            &asset.filename,
            &asset.original_path,
            &people_names,
            &tags,
            &collections,
            &location_text,
            &notes,
            &metadata_text,
        )
    }

    /// Convenience: reindex one asset in its own transaction.
    pub fn reindex_asset(&self, asset_id: &AssetId) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        self.reindex_asset_in(&tx, asset_id)?;
        tx.commit()?;
        Ok(())
    }

    // ── Geo ────────────────────────────────────────────────────────────────

    /// Ready assets that carry GPS coordinates.
    pub fn list_assets_with_gps(&self) -> Result<Vec<(Asset, f64, f64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM assets \
             WHERE status = 'ready' AND deleted_at IS NULL \
               AND latitude IS NOT NULL AND longitude IS NOT NULL \
             ORDER BY imported_at DESC",
        )?;
        let rows = stmt.query_map([], row_to_asset)?;
        let mut out = Vec::new();
        for row in rows {
            let a = row?;
            if let (Some(lat), Some(lon)) = (a.latitude, a.longitude) {
                out.push((a, lat, lon));
            }
        }
        Ok(out)
    }

    /// Ready assets inside a lat/lon bounding box (inclusive).
    pub fn assets_in_bbox(
        &self,
        min_lat: f64,
        max_lat: f64,
        min_lon: f64,
        max_lon: f64,
    ) -> Result<Vec<(Asset, f64, f64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM assets \
             WHERE status = 'ready' AND deleted_at IS NULL \
               AND latitude BETWEEN ?1 AND ?2 \
               AND longitude BETWEEN ?3 AND ?4 \
             ORDER BY imported_at DESC",
        )?;
        let rows = stmt.query_map(params![min_lat, max_lat, min_lon, max_lon], row_to_asset)?;
        let mut out = Vec::new();
        for row in rows {
            let a = row?;
            if let (Some(lat), Some(lon)) = (a.latitude, a.longitude) {
                out.push((a, lat, lon));
            }
        }
        Ok(out)
    }

    /// Batch helpers used by multi-select editing. Each call is one transaction
    /// and reindexes every affected asset.
    pub fn batch_add_tag(&self, asset_ids: &[AssetId], tag_name: &str) -> Result<u32> {
        let tag = self.upsert_tag(tag_name)?;
        let mut n = 0u32;
        for id in asset_ids {
            self.add_asset_tag(id, &tag.id)?;
            n += 1;
        }
        Ok(n)
    }

    pub fn batch_remove_tag(&self, asset_ids: &[AssetId], tag_name: &str) -> Result<u32> {
        let tag_id: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM tags WHERE name = ?1",
                params![tag_name],
                |r| r.get(0),
            )
            .optional()?;
        let Some(tag_id) = tag_id else { return Ok(0) };
        let tag_id = RowId::parse(&tag_id)?;
        let mut n = 0u32;
        for id in asset_ids {
            self.remove_asset_tag(id, &tag_id)?;
            n += 1;
        }
        Ok(n)
    }

    pub fn batch_add_person(&self, asset_ids: &[AssetId], person_name: &str) -> Result<u32> {
        // Reuse an existing person with the same name when present.
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM people WHERE name = ?1",
                params![person_name],
                |r| r.get(0),
            )
            .optional()?;
        let person_id = match existing {
            Some(id) => RowId::parse(&id)?,
            None => self.create_person(person_name, None)?.id,
        };
        let mut n = 0u32;
        for id in asset_ids {
            self.link_asset_person(id, &person_id)?;
            n += 1;
        }
        Ok(n)
    }

    pub fn batch_add_to_collection(
        &self,
        asset_ids: &[AssetId],
        collection_id: &RowId,
    ) -> Result<u32> {
        let mut n = 0u32;
        for id in asset_ids {
            self.add_asset_collection(id, collection_id)?;
            n += 1;
        }
        Ok(n)
    }

    pub fn batch_remove_from_collection(
        &self,
        asset_ids: &[AssetId],
        collection_id: &RowId,
    ) -> Result<u32> {
        let mut n = 0u32;
        for id in asset_ids {
            self.remove_asset_from_collection(id, collection_id)?;
            n += 1;
        }
        Ok(n)
    }

    /// Index an asset into FTS. Called after the asset is `ready`.
    /// Prefer `reindex_asset_in` — this is the raw writer it delegates to.
    #[allow(clippy::too_many_arguments)]
    pub fn index_asset(
        &self,
        tx: &Transaction<'_>,
        asset_id: &AssetId,
        filename: &str,
        original_path: &str,
        people_names: &str,
        tags: &str,
        collections: &str,
        location_text: &str,
        notes: &str,
        metadata_text: &str,
    ) -> Result<()> {
        tx.execute(
            "DELETE FROM assets_fts WHERE asset_id = ?1",
            params![asset_id.as_str()],
        )?;
        tx.execute(
            r#"
            INSERT INTO assets_fts (
                asset_id, filename, original_path, people_names, tags,
                collections, location_text, notes, metadata_text
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
            params![
                asset_id.as_str(),
                filename,
                original_path,
                people_names,
                tags,
                collections,
                location_text,
                notes,
                metadata_text,
            ],
        )?;
        Ok(())
    }
}

// ── row mappers ─────────────────────────────────────────────────────────────

fn row_to_undo(row: &rusqlite::Row<'_>) -> rusqlite::Result<UndoEntry> {
    let kind_s: String = row.get(2)?;
    let payload: String = row.get(4)?;
    Ok(UndoEntry {
        id: row.get(0)?,
        created_at: row.get(1)?,
        kind: UndoKind::parse(&kind_s).unwrap_or(UndoKind::Batch),
        label: row.get(3)?,
        payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
    })
}

fn join_names(tx: &Transaction<'_>, sql: &str, asset_id: &AssetId) -> Result<String> {
    let mut stmt = tx.prepare(sql)?;
    let rows = stmt.query_map(params![asset_id.as_str()], |r| r.get::<_, String>(0))?;
    let mut names = Vec::new();
    for row in rows {
        names.push(row?);
    }
    Ok(names.join(" "))
}

fn opt_dt(dt: Option<DateTime<Utc>>) -> Option<String> {
    dt.map(|d| d.to_rfc3339())
}

fn parse_dt(s: Option<String>) -> Option<DateTime<Utc>> {
    s.and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
        .map(|d| d.with_timezone(&Utc))
}

fn row_to_asset(row: &rusqlite::Row<'_>) -> rusqlite::Result<Asset> {
    let parse_errors: String = row.get("parse_errors")?;
    let raw_metadata: String = row.get("raw_metadata")?;
    let id: String = row.get("id")?;
    Ok(Asset {
        id: AssetId::parse(&id).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        filename: row.get("filename")?,
        extension: row.get("extension")?,
        file_size: row.get::<_, i64>("file_size")? as u64,
        hash: row.get("hash")?,
        mime_type: row.get("mime_type")?,
        duration_ms: row.get::<_, Option<i64>>("duration_ms")?.map(|v| v as u64),
        sample_rate: row.get::<_, Option<i64>>("sample_rate")?.map(|v| v as u32),
        bit_depth: row.get::<_, Option<i64>>("bit_depth")?.map(|v| v as u16),
        channels: row.get::<_, Option<i64>>("channels")?.map(|v| v as u16),
        channel_layout: row.get("channel_layout")?,
        codec: row.get("codec")?,
        container: row.get("container")?,
        bitrate: row.get::<_, Option<i64>>("bitrate")?.map(|v| v as u32),
        compression: row.get("compression")?,
        recorded_at: parse_dt(row.get("recorded_at")?),
        recording_start: parse_dt(row.get("recording_start")?),
        recording_end: parse_dt(row.get("recording_end")?),
        timezone: row.get("timezone")?,
        utc_offset_minutes: row.get("utc_offset_minutes")?,
        file_created_at: parse_dt(row.get("file_created_at")?),
        file_modified_at: parse_dt(row.get("file_modified_at")?),
        imported_at: parse_dt(row.get("imported_at")?).unwrap_or_else(Utc::now),
        latitude: row.get("latitude")?,
        longitude: row.get("longitude")?,
        altitude: row.get("altitude")?,
        gps_accuracy: row.get("gps_accuracy")?,
        original_path: row.get("original_path")?,
        source_volume: row.get("source_volume")?,
        library_relpath: row.get("library_relpath")?,
        status: AssetStatus::parse(&row.get::<_, String>("status")?).unwrap_or(AssetStatus::Failed),
        parse_errors: serde_json::from_str(&parse_errors).unwrap_or_default(),
        raw_metadata: serde_json::from_str(&raw_metadata).unwrap_or_default(),
        metadata: Vec::new(),
        // Tolerate pre-v4 rows during schema upgrade (column may not exist yet).
        deleted_at: parse_dt(row.get::<_, Option<String>>("deleted_at").ok().flatten()),
    })
}

fn row_to_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImportJob> {
    Ok(ImportJob {
        id: RowId::parse(&row.get::<_, String>("id")?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        created_at: parse_dt(row.get("created_at")?).unwrap_or_else(Utc::now),
        source: row.get("source")?,
        total_files: row.get::<_, i64>("total_files")? as u32,
        processed_files: row.get::<_, i64>("processed_files")? as u32,
        success_count: row.get::<_, i64>("success_count")? as u32,
        failed_count: row.get::<_, i64>("failed_count")? as u32,
        skipped_count: row.get::<_, i64>("skipped_count")? as u32,
        status: ImportJobStatus::parse(&row.get::<_, String>("status")?)
            .unwrap_or(ImportJobStatus::Failed),
    })
}

fn row_to_import_file(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImportFileRecord> {
    Ok(ImportFileRecord {
        id: RowId::parse(&row.get::<_, String>("id")?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        job_id: RowId::parse(&row.get::<_, String>("job_id")?).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        source_path: row.get("source_path")?,
        asset_id: row.get("asset_id")?,
        status: ImportFileStatus::parse(&row.get::<_, String>("status")?)
            .unwrap_or(ImportFileStatus::Failed),
        error: row.get("error")?,
    })
}
