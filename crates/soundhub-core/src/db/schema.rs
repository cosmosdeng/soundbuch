use rusqlite::Connection;

use crate::error::{Error, Result};

pub const SCHEMA_VERSION: i64 = 5;

/// Full schema. Core tables first (data model before UI).
pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA journal_mode = WAL;")?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    conn.execute_batch("PRAGMA synchronous = NORMAL;")?;

    // Ensure the version table exists first so we can read the stored version.
    conn.execute_batch("CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);")?;

    let stored: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    if stored > SCHEMA_VERSION {
        return Err(Error::other(format!(
            "library schema version {stored} is newer than supported {SCHEMA_VERSION}"
        )));
    }

    if stored == 0 {
        // Fresh library: create everything at the current level.
        create_latest_schema(conn)?;
        conn.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            [SCHEMA_VERSION],
        )?;
    } else {
        // Existing library: run upgrade steps in order.
        if stored < 2 {
            upgrade_v1_to_v2(conn)?;
        }
        if stored < 3 {
            upgrade_v2_to_v3(conn)?;
        }
        if stored < 4 {
            upgrade_v3_to_v4(conn)?;
        }
        if stored < 5 {
            upgrade_v4_to_v5(conn)?;
        }
        // Future upgrades: if stored < 6 { upgrade_v5_to_v6(conn)?; } ...
        conn.execute("UPDATE schema_version SET version = ?1", [SCHEMA_VERSION])?;
    }

    Ok(())
}

fn create_latest_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"

        CREATE TABLE IF NOT EXISTS library_meta (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        -- ── Assets ───────────────────────────────────────────────────────
        CREATE TABLE IF NOT EXISTS assets (
            id                    TEXT PRIMARY KEY,
            filename              TEXT NOT NULL,
            extension             TEXT,
            file_size             INTEGER NOT NULL,
            hash                  TEXT NOT NULL,
            mime_type             TEXT,
            duration_ms           INTEGER,
            sample_rate           INTEGER,
            bit_depth             INTEGER,
            channels              INTEGER,
            channel_layout        TEXT,
            codec                 TEXT,
            container             TEXT,
            bitrate               INTEGER,
            compression           TEXT,
            recorded_at           TEXT,
            recording_start       TEXT,
            recording_end         TEXT,
            timezone              TEXT,
            utc_offset_minutes    INTEGER,
            file_created_at       TEXT,
            file_modified_at      TEXT,
            imported_at           TEXT NOT NULL,
            latitude              REAL,
            longitude             REAL,
            altitude              REAL,
            gps_accuracy          REAL,
            original_path         TEXT NOT NULL,
            source_volume         TEXT,
            library_relpath       TEXT NOT NULL,
            status                TEXT NOT NULL,
            parse_errors          TEXT NOT NULL DEFAULT '[]',
            raw_metadata          TEXT NOT NULL DEFAULT '{}',
            deleted_at            TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_assets_hash      ON assets(hash);
        CREATE INDEX IF NOT EXISTS idx_assets_status    ON assets(status);
        CREATE INDEX IF NOT EXISTS idx_assets_recorded  ON assets(recorded_at);
        CREATE INDEX IF NOT EXISTS idx_assets_filename  ON assets(filename);
        CREATE INDEX IF NOT EXISTS idx_assets_deleted   ON assets(deleted_at);

        -- Provenance-tracked metadata values (source + value).
        CREATE TABLE IF NOT EXISTS metadata_values (
            id          TEXT PRIMARY KEY,
            asset_id    TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
            key         TEXT NOT NULL,
            value       TEXT NOT NULL,
            source      TEXT NOT NULL,
            recorded_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_meta_asset_key ON metadata_values(asset_id, key);

        -- ── Organization ─────────────────────────────────────────────────
        CREATE TABLE IF NOT EXISTS people (
            id       TEXT PRIMARY KEY,
            name     TEXT NOT NULL,
            note     TEXT,
            metadata TEXT
        );

        CREATE TABLE IF NOT EXISTS locations (
            id        TEXT PRIMARY KEY,
            country   TEXT,
            region    TEXT,
            city      TEXT,
            place     TEXT,
            latitude  REAL,
            longitude REAL,
            altitude  REAL,
            raw       TEXT
        );

        CREATE TABLE IF NOT EXISTS tags (
            id   TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE
        );

        CREATE TABLE IF NOT EXISTS collections (
            id              TEXT PRIMARY KEY,
            name            TEXT NOT NULL,
            parent_id       TEXT REFERENCES collections(id) ON DELETE SET NULL,
            collection_type TEXT NOT NULL DEFAULT 'static',
            rules           TEXT
        );

        CREATE TABLE IF NOT EXISTS asset_people (
            asset_id  TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
            person_id TEXT NOT NULL REFERENCES people(id) ON DELETE CASCADE,
            PRIMARY KEY (asset_id, person_id)
        );

        CREATE TABLE IF NOT EXISTS asset_tags (
            asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
            tag_id   TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
            PRIMARY KEY (asset_id, tag_id)
        );

        CREATE TABLE IF NOT EXISTS asset_collections (
            asset_id      TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
            collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
            PRIMARY KEY (asset_id, collection_id)
        );

        CREATE TABLE IF NOT EXISTS asset_locations (
            asset_id    TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
            location_id TEXT NOT NULL REFERENCES locations(id) ON DELETE CASCADE,
            PRIMARY KEY (asset_id, location_id)
        );

        -- ── Import jobs (crash recovery) ─────────────────────────────────
        CREATE TABLE IF NOT EXISTS import_jobs (
            id              TEXT PRIMARY KEY,
            created_at      TEXT NOT NULL,
            source          TEXT NOT NULL,
            total_files     INTEGER NOT NULL DEFAULT 0,
            processed_files INTEGER NOT NULL DEFAULT 0,
            success_count   INTEGER NOT NULL DEFAULT 0,
            failed_count    INTEGER NOT NULL DEFAULT 0,
            skipped_count   INTEGER NOT NULL DEFAULT 0,
            status          TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS import_files (
            id          TEXT PRIMARY KEY,
            job_id      TEXT NOT NULL REFERENCES import_jobs(id) ON DELETE CASCADE,
            source_path TEXT NOT NULL,
            asset_id    TEXT,
            status      TEXT NOT NULL,
            error       TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_import_files_job ON import_files(job_id);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_import_files_job_path ON import_files(job_id, source_path);

        -- ── Playlists (ordered tracks, distinct from Collections) ────────
        CREATE TABLE IF NOT EXISTS playlists (
            id         TEXT PRIMARY KEY,
            name       TEXT NOT NULL,
            created_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS playlist_tracks (
            playlist_id TEXT NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
            asset_id    TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
            position    INTEGER NOT NULL,
            PRIMARY KEY (playlist_id, asset_id)
        );
        CREATE INDEX IF NOT EXISTS idx_playlist_tracks_order
            ON playlist_tracks(playlist_id, position);

        -- ── Undo log ─────────────────────────────────────────────────────
        -- One row per reversible user action. `payload` is JSON describing
        -- how to reverse it. Newest row = next undo.
        CREATE TABLE IF NOT EXISTS undo_log (
            id         TEXT PRIMARY KEY,
            created_at TEXT NOT NULL,
            kind       TEXT NOT NULL,
            label      TEXT NOT NULL,
            payload    TEXT NOT NULL
        );

        -- ── Full-text search ─────────────────────────────────────────────
        CREATE VIRTUAL TABLE IF NOT EXISTS assets_fts USING fts5(
            asset_id UNINDEXED,
            filename,
            original_path,
            people_names,
            tags,
            collections,
            location_text,
            notes,
            metadata_text,
            tokenize = 'unicode61 remove_diacritics 2'
        );
        "#,
    )?;
    Ok(())
}

/// v1 → v2: deduplicate import_files and add a unique constraint.
fn upgrade_v1_to_v2(conn: &Connection) -> Result<()> {
    // Deduplicate: for each (job_id, source_path) group keep one row.
    // Prefer terminal statuses (ready/skipped_*) then lowest id.
    conn.execute_batch(
        r#"
        DELETE FROM import_files WHERE id NOT IN (
            SELECT id FROM (
                SELECT id,
                       ROW_NUMBER() OVER (
                           PARTITION BY job_id, source_path
                           ORDER BY
                               CASE status
                                   WHEN 'ready' THEN 0
                                   WHEN 'skipped_duplicate' THEN 1
                                   WHEN 'skipped_unsupported' THEN 2
                                   WHEN 'failed' THEN 3
                                   WHEN 'copying' THEN 4
                                   ELSE 5
                               END,
                               id
                       ) AS rn
                FROM import_files
            ) WHERE rn = 1
        );
        "#,
    )?;
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_import_files_job_path ON import_files(job_id, source_path);",
    )?;
    Ok(())
}

/// v2 → v3: rebuild FTS for every ready asset from live relation names.
///
/// v1/v2 imports wrote empty strings into `people_names`/`tags`/`collections`/`notes`
/// and relation mutations never touched the index. Re-running `reindex_asset`
/// repairs existing libraries so those columns match the actual links.
fn upgrade_v2_to_v3(conn: &Connection) -> Result<()> {
    let repo = crate::db::Repo::new(conn);
    let tx = conn.unchecked_transaction()?;
    let ids: Vec<String> = {
        let mut stmt = tx.prepare("SELECT id FROM assets WHERE status = 'ready'")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for id in ids {
        let aid = crate::ids::AssetId::parse(&id)?;
        repo.reindex_asset_in(&tx, &aid)?;
    }
    tx.commit()?;
    Ok(())
}

/// v3 → v4: recycle-bin column + undo log.
fn upgrade_v3_to_v4(conn: &Connection) -> Result<()> {
    // SQLite cannot ADD COLUMN with an index in one step; add then index.
    let has_col: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('assets') WHERE name = 'deleted_at'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if has_col == 0 {
        conn.execute_batch("ALTER TABLE assets ADD COLUMN deleted_at TEXT;")?;
    }
    conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_assets_deleted ON assets(deleted_at);")?;
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS undo_log (
            id         TEXT PRIMARY KEY,
            created_at TEXT NOT NULL,
            kind       TEXT NOT NULL,
            label      TEXT NOT NULL,
            payload    TEXT NOT NULL
        );
        "#,
    )?;
    Ok(())
}

/// v4 → v5: playlists with ordered tracks.
fn upgrade_v4_to_v5(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS playlists (
            id         TEXT PRIMARY KEY,
            name       TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS playlist_tracks (
            playlist_id TEXT NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
            asset_id    TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
            position    INTEGER NOT NULL,
            PRIMARY KEY (playlist_id, asset_id)
        );
        CREATE INDEX IF NOT EXISTS idx_playlist_tracks_order
            ON playlist_tracks(playlist_id, position);
        "#,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn stored_version(conn: &Connection) -> i64 {
        conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn has_unique_import_path_index(conn: &Connection) -> bool {
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'index' AND name = 'idx_import_files_job_path'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        n > 0
    }

    #[test]
    fn fresh_library_stamps_current_version() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        assert_eq!(stored_version(&conn), SCHEMA_VERSION);
        assert!(has_unique_import_path_index(&conn));
    }

    #[test]
    fn migrate_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        assert_eq!(stored_version(&conn), SCHEMA_VERSION);
    }

    #[test]
    fn v1_library_upgrades_and_dedupes_import_files() {
        let conn = Connection::open_in_memory().unwrap();
        // Hand-build a v1-shaped library: tables exist, no unique index,
        // duplicate (job_id, source_path) rows, version stamped at 1.
        conn.execute_batch(
            r#"
            CREATE TABLE schema_version (version INTEGER NOT NULL);
            INSERT INTO schema_version (version) VALUES (1);
            CREATE TABLE assets (
                id TEXT PRIMARY KEY,
                filename TEXT NOT NULL,
                extension TEXT,
                file_size INTEGER NOT NULL,
                hash TEXT NOT NULL,
                mime_type TEXT,
                duration_ms INTEGER,
                sample_rate INTEGER,
                bit_depth INTEGER,
                channels INTEGER,
                channel_layout TEXT,
                codec TEXT,
                container TEXT,
                bitrate INTEGER,
                compression TEXT,
                recorded_at TEXT,
                recording_start TEXT,
                recording_end TEXT,
                timezone TEXT,
                utc_offset_minutes INTEGER,
                file_created_at TEXT,
                file_modified_at TEXT,
                imported_at TEXT NOT NULL,
                latitude REAL,
                longitude REAL,
                altitude REAL,
                gps_accuracy REAL,
                original_path TEXT NOT NULL,
                source_volume TEXT,
                library_relpath TEXT NOT NULL,
                status TEXT NOT NULL,
                parse_errors TEXT NOT NULL DEFAULT '[]',
                raw_metadata TEXT NOT NULL DEFAULT '{}'
            );
            CREATE TABLE metadata_values (
                id TEXT PRIMARY KEY,
                asset_id TEXT NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                source TEXT NOT NULL,
                recorded_at TEXT
            );
            CREATE TABLE people (id TEXT PRIMARY KEY, name TEXT NOT NULL, note TEXT, metadata TEXT);
            CREATE TABLE tags (id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE);
            CREATE TABLE collections (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                parent_id TEXT,
                collection_type TEXT NOT NULL DEFAULT 'static',
                rules TEXT
            );
            CREATE TABLE asset_people (
                asset_id TEXT NOT NULL,
                person_id TEXT NOT NULL,
                PRIMARY KEY (asset_id, person_id)
            );
            CREATE TABLE asset_tags (
                asset_id TEXT NOT NULL,
                tag_id TEXT NOT NULL,
                PRIMARY KEY (asset_id, tag_id)
            );
            CREATE TABLE asset_collections (
                asset_id TEXT NOT NULL,
                collection_id TEXT NOT NULL,
                PRIMARY KEY (asset_id, collection_id)
            );
            CREATE VIRTUAL TABLE assets_fts USING fts5(
                asset_id UNINDEXED,
                filename,
                original_path,
                people_names,
                tags,
                collections,
                location_text,
                notes,
                metadata_text,
                tokenize = 'unicode61 remove_diacritics 2'
            );
            CREATE TABLE import_jobs (
                id TEXT PRIMARY KEY,
                created_at TEXT NOT NULL,
                source TEXT NOT NULL,
                total_files INTEGER NOT NULL DEFAULT 0,
                processed_files INTEGER NOT NULL DEFAULT 0,
                success_count INTEGER NOT NULL DEFAULT 0,
                failed_count INTEGER NOT NULL DEFAULT 0,
                skipped_count INTEGER NOT NULL DEFAULT 0,
                status TEXT NOT NULL
            );
            CREATE TABLE import_files (
                id TEXT PRIMARY KEY,
                job_id TEXT NOT NULL,
                source_path TEXT NOT NULL,
                asset_id TEXT,
                status TEXT NOT NULL,
                error TEXT
            );
            INSERT INTO import_jobs (id, created_at, source, status)
                VALUES ('job1', '2026-01-01T00:00:00Z', '/src', 'completed');
            -- Duplicate pair: the `ready` row must win.
            INSERT INTO import_files (id, job_id, source_path, status) VALUES
                ('f1', 'job1', '/src/a.wav', 'copying'),
                ('f2', 'job1', '/src/a.wav', 'ready'),
                ('f3', 'job1', '/src/b.wav', 'failed');
            "#,
        )
        .unwrap();
        assert!(!has_unique_import_path_index(&conn));

        migrate(&conn).unwrap();

        assert_eq!(stored_version(&conn), SCHEMA_VERSION);
        assert!(has_unique_import_path_index(&conn));

        // f1 (copying, higher id) dropped; f2 (ready) kept.
        let rows: Vec<(String, String)> = {
            let mut stmt = conn
                .prepare("SELECT id, status FROM import_files ORDER BY id")
                .unwrap();
            let mapped = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
            mapped.collect::<std::result::Result<Vec<_>, _>>().unwrap()
        };
        assert_eq!(
            rows,
            vec![
                ("f2".into(), "ready".into()),
                ("f3".into(), "failed".into()),
            ]
        );

        // The unique index is live: re-inserting the same path fails.
        let dup = conn.execute(
            "INSERT INTO import_files (id, job_id, source_path, status) \
             VALUES ('f4', 'job1', '/src/a.wav', 'pending')",
            params![],
        );
        assert!(
            dup.is_err(),
            "unique index must reject duplicate (job, path)"
        );
    }

    #[test]
    fn v2_to_v3_rebuilds_fts_relation_fields() {
        let conn = Connection::open_in_memory().unwrap();
        // v2 library: relations exist but the FTS row was written with empty
        // tags/people/collections (the pre-fix import path).
        conn.execute_batch(
            r#"
            CREATE TABLE schema_version (version INTEGER NOT NULL);
            INSERT INTO schema_version (version) VALUES (2);
            CREATE TABLE assets (
                id TEXT PRIMARY KEY,
                filename TEXT NOT NULL,
                extension TEXT,
                file_size INTEGER NOT NULL,
                hash TEXT NOT NULL,
                mime_type TEXT,
                duration_ms INTEGER,
                sample_rate INTEGER,
                bit_depth INTEGER,
                channels INTEGER,
                channel_layout TEXT,
                codec TEXT,
                container TEXT,
                bitrate INTEGER,
                compression TEXT,
                recorded_at TEXT,
                recording_start TEXT,
                recording_end TEXT,
                timezone TEXT,
                utc_offset_minutes INTEGER,
                file_created_at TEXT,
                file_modified_at TEXT,
                imported_at TEXT NOT NULL,
                latitude REAL,
                longitude REAL,
                altitude REAL,
                gps_accuracy REAL,
                original_path TEXT NOT NULL,
                source_volume TEXT,
                library_relpath TEXT NOT NULL,
                status TEXT NOT NULL,
                parse_errors TEXT NOT NULL DEFAULT '[]',
                raw_metadata TEXT NOT NULL DEFAULT '{}'
            );
            CREATE TABLE metadata_values (
                id TEXT PRIMARY KEY,
                asset_id TEXT NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                source TEXT NOT NULL,
                recorded_at TEXT
            );
            CREATE TABLE people (id TEXT PRIMARY KEY, name TEXT NOT NULL, note TEXT, metadata TEXT);
            CREATE TABLE tags (id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE);
            CREATE TABLE collections (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                parent_id TEXT,
                collection_type TEXT NOT NULL DEFAULT 'static',
                rules TEXT
            );
            CREATE TABLE asset_people (
                asset_id TEXT NOT NULL,
                person_id TEXT NOT NULL,
                PRIMARY KEY (asset_id, person_id)
            );
            CREATE TABLE asset_tags (
                asset_id TEXT NOT NULL,
                tag_id TEXT NOT NULL,
                PRIMARY KEY (asset_id, tag_id)
            );
            CREATE TABLE asset_collections (
                asset_id TEXT NOT NULL,
                collection_id TEXT NOT NULL,
                PRIMARY KEY (asset_id, collection_id)
            );
            CREATE VIRTUAL TABLE assets_fts USING fts5(
                asset_id UNINDEXED,
                filename,
                original_path,
                people_names,
                tags,
                collections,
                location_text,
                notes,
                metadata_text,
                tokenize = 'unicode61 remove_diacritics 2'
            );

            INSERT INTO assets (
                id, filename, file_size, hash, imported_at, original_path,
                library_relpath, status
            ) VALUES (
                'sh_01234567890123456789012345', 'field.wav', 10, 'abc',
                '2026-01-01T00:00:00Z', '/usb/field.wav',
                'assets/aa/sh_01234567890123456789012345.wav', 'ready'
            );
            INSERT INTO tags (id, name) VALUES ('tag1', 'seagull');
            INSERT INTO people (id, name) VALUES ('p1', 'Ornithologist');
            INSERT INTO collections (id, name) VALUES ('c1', 'Birdsong');
            INSERT INTO asset_tags (asset_id, tag_id) VALUES ('sh_01234567890123456789012345', 'tag1');
            INSERT INTO asset_people (asset_id, person_id) VALUES ('sh_01234567890123456789012345', 'p1');
            INSERT INTO asset_collections (asset_id, collection_id) VALUES ('sh_01234567890123456789012345', 'c1');
            -- Pre-fix index: relation columns empty.
            INSERT INTO assets_fts (
                asset_id, filename, original_path, people_names, tags,
                collections, location_text, notes, metadata_text
            ) VALUES (
                'sh_01234567890123456789012345', 'field.wav', '/usb/field.wav',
                '', '', '', '', '', ''
            );
            "#,
        )
        .unwrap();

        migrate(&conn).unwrap();
        assert_eq!(stored_version(&conn), SCHEMA_VERSION);

        let cols: (String, String, String) = conn
            .query_row(
                "SELECT people_names, tags, collections FROM assets_fts \
                 WHERE asset_id = 'sh_01234567890123456789012345'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(cols.0, "Ornithologist");
        assert_eq!(cols.1, "seagull");
        assert_eq!(cols.2, "Birdsong");
    }

    #[test]
    fn newer_schema_version_is_rejected() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_version (version INTEGER NOT NULL);
             INSERT INTO schema_version (version) VALUES (99);",
        )
        .unwrap();
        let err = migrate(&conn).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("99") && msg.contains("newer"), "got: {msg}");
    }
}
