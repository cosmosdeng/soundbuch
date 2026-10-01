//! Organization + search tests (PRD Test 08-09).

use std::fs::File;
use std::io::Write;
use std::path::Path;

use soundhub_core::db::Repo;
use soundhub_core::import::ImportPipeline;
use soundhub_core::library::Library;
use soundhub_core::models::DuplicateAction;
use soundhub_core::search::{search, SearchQuery};
use soundhub_core::{AssetId, RowId};

fn write_min_wav(path: &Path, sample_rate: u32, channels: u16, bits: u16, frames: u32) {
    let block_align = channels * bits / 8;
    let byte_rate = sample_rate * block_align as u32;
    let data_size = frames * block_align as u32;
    let riff_size = 36 + data_size;
    let mut f = File::create(path).unwrap();
    let mut buf = Vec::new();
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&riff_size.to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&1u16.to_le_bytes());
    buf.extend_from_slice(&channels.to_le_bytes());
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&block_align.to_le_bytes());
    buf.extend_from_slice(&bits.to_le_bytes());
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_size.to_le_bytes());
    for i in 0..data_size {
        buf.push((i % 251) as u8);
    }
    f.write_all(&buf).unwrap();
}

fn setup_library(root: &Path) -> (Library, rusqlite::Connection) {
    let library = Library::create(root).unwrap();
    let conn = library.open_db().unwrap();
    (library, conn)
}

#[test]
fn test_08_multiple_collections_share_one_file() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let wav = src.join("assetA.wav");
    write_min_wav(&wav, 48000, 2, 24, 4800);

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let pipeline = ImportPipeline::new(&library, &conn);
    let result = pipeline
        .import_paths(&[wav], DuplicateAction::Skip)
        .unwrap();
    let asset_id = AssetId::parse(result.files[0].asset_id.as_ref().unwrap()).unwrap();

    let repo = Repo::new(&conn);
    let c1 = repo.create_collection("A", None).unwrap();
    let c2 = repo.create_collection("B", None).unwrap();
    let c3 = repo.create_collection("C", None).unwrap();
    repo.add_asset_collection(&asset_id, &c1.id).unwrap();
    repo.add_asset_collection(&asset_id, &c2.id).unwrap();
    repo.add_asset_collection(&asset_id, &c3.id).unwrap();

    // Still exactly one physical file.
    let asset = repo.get_asset(&asset_id).unwrap().unwrap();
    let dest = library.asset_abspath(&asset.library_relpath);
    assert!(dest.exists());
    let mut n = 0;
    for e in walkdir::WalkDir::new(library.root.join("assets")) {
        if e.unwrap().file_type().is_file() {
            n += 1;
        }
    }
    assert_eq!(n, 1);
}

#[test]
fn test_09_search_by_filename_tag_person_location() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let wav = src.join("jeju_ocean.wav");
    write_min_wav(&wav, 48000, 2, 24, 4800);

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let pipeline = ImportPipeline::new(&library, &conn);
    let result = pipeline
        .import_paths(&[wav], DuplicateAction::Skip)
        .unwrap();
    let asset_id = AssetId::parse(result.files[0].asset_id.as_ref().unwrap()).unwrap();

    let repo = Repo::new(&conn);
    // Tag
    let tag = repo.upsert_tag("ocean").unwrap();
    repo.add_asset_tag(&asset_id, &tag.id).unwrap();

    // Person
    let person = repo.create_person("Zhang San", None).unwrap();
    repo.link_asset_person(&asset_id, &person.id).unwrap();

    // Re-index with names so FTS can find them.
    {
        let tx = conn.unchecked_transaction().unwrap();
        repo.index_asset(
            &tx,
            &asset_id,
            "jeju_ocean.wav",
            "/usb/jeju_ocean.wav",
            "Zhang San",
            "ocean",
            "",
            "Jeju Island",
            "",
            "PCM WAV",
        )
        .unwrap();
        tx.commit().unwrap();
    }

    // Filename
    let hits = search(&conn, &SearchQuery::text("jeju")).unwrap();
    assert!(hits.contains(&asset_id), "filename search should hit");

    // Tag
    let hits = search(&conn, &SearchQuery::text("ocean")).unwrap();
    assert!(hits.contains(&asset_id), "tag search should hit");

    // Person
    let hits = search(&conn, &SearchQuery::text("Zhang")).unwrap();
    assert!(hits.contains(&asset_id), "person search should hit");

    // Location text
    let hits = search(&conn, &SearchQuery::text("Island")).unwrap();
    assert!(hits.contains(&asset_id), "location search should hit");
}

#[test]
fn search_filter_by_tag_and_sample_rate() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    write_min_wav(&src.join("a.wav"), 48000, 2, 24, 100);
    write_min_wav(&src.join("b.wav"), 44100, 1, 16, 200);

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let pipeline = ImportPipeline::new(&library, &conn);
    pipeline
        .import_paths(&[src], DuplicateAction::Skip)
        .unwrap();

    let repo = Repo::new(&conn);
    let assets = repo.list_assets(10, 0).unwrap();
    assert_eq!(assets.len(), 2);

    // Tag only one of them.
    let a = assets
        .iter()
        .find(|a| a.sample_rate == Some(48000))
        .unwrap();
    let tag = repo.upsert_tag("wind").unwrap();
    repo.add_asset_tag(&a.id, &tag.id).unwrap();

    let q = SearchQuery {
        tag: Some("wind".into()),
        ..Default::default()
    };
    let hits = search(&conn, &q).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0], a.id);

    let q2 = SearchQuery {
        sample_rate: Some(44100),
        ..Default::default()
    };
    let hits2 = search(&conn, &q2).unwrap();
    assert_eq!(hits2.len(), 1);
    assert_ne!(hits2[0], a.id);
}

#[test]
fn tags_people_collections_persist() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    write_min_wav(&src.join("x.wav"), 48000, 1, 16, 50);

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let pipeline = ImportPipeline::new(&library, &conn);
    let result = pipeline
        .import_paths(&[src], DuplicateAction::Skip)
        .unwrap();
    let asset_id = AssetId::parse(result.files[0].asset_id.as_ref().unwrap()).unwrap();

    let repo = Repo::new(&conn);
    let t = repo.upsert_tag("night").unwrap();
    repo.add_asset_tag(&asset_id, &t.id).unwrap();
    let t2 = repo.upsert_tag("night").unwrap();
    assert_eq!(t.id, t2.id, "tags must be unique by name");

    let person = repo.create_person("Li Si", Some("recordist")).unwrap();
    repo.link_asset_person(&asset_id, &person.id).unwrap();

    let col = repo.create_collection("Travel", None).unwrap();
    repo.add_asset_collection(&asset_id, &col.id).unwrap();

    // IDs are stable RowIds, not names.
    assert_eq!(t.id.as_str().len(), 26);
    assert_eq!(person.id.as_str().len(), 26);
    assert_ne!(person.id.as_str(), RowId::new().as_str());

    // List helpers return what was created.
    let tags = repo.list_tags().unwrap();
    assert!(tags.iter().any(|t| t.name == "night"));
    let people = repo.list_people().unwrap();
    assert!(people.iter().any(|p| p.name == "Li Si"));
    let cols = repo.list_collections().unwrap();
    assert!(cols.iter().any(|c| c.name == "Travel"));
}

#[test]
fn list_tags_collections_people_are_sorted_and_complete() {
    let tmp = tempfile::tempdir().unwrap();
    let lib_dir = tmp.path().join("Library");
    let (_library, conn) = setup_library(&lib_dir);
    let repo = Repo::new(&conn);

    repo.upsert_tag("zeta").unwrap();
    repo.upsert_tag("alpha").unwrap();
    repo.create_collection("B trip", None).unwrap();
    repo.create_collection("A trip", None).unwrap();
    repo.create_person("Bob", None).unwrap();
    repo.create_person("Alice", None).unwrap();

    let tags = repo.list_tags().unwrap();
    assert_eq!(tags.len(), 2);
    assert_eq!(tags[0].name, "alpha");
    assert_eq!(tags[1].name, "zeta");

    let cols = repo.list_collections().unwrap();
    assert_eq!(cols.len(), 2);
    assert_eq!(cols[0].name, "A trip");

    let people = repo.list_people().unwrap();
    assert_eq!(people.len(), 2);
    assert_eq!(people[0].name, "Alice");
}

#[test]
fn text_search_ands_with_tag_filter() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    write_min_wav(&src.join("ocean_wave.wav"), 48000, 2, 24, 100);
    write_min_wav(&src.join("ocean_wind.wav"), 48000, 2, 24, 100);

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let pipeline = ImportPipeline::new(&library, &conn);
    pipeline
        .import_paths(&[src], DuplicateAction::Skip)
        .unwrap();

    let repo = Repo::new(&conn);
    let assets = repo.list_assets(10, 0).unwrap();
    assert_eq!(assets.len(), 2);
    let tagged = assets.iter().find(|a| a.filename.contains("wave")).unwrap();
    let tag = repo.upsert_tag("calm").unwrap();
    repo.add_asset_tag(&tagged.id, &tag.id).unwrap();

    // Text alone matches both.
    let q = SearchQuery {
        text: Some("ocean".into()),
        limit: 50,
        ..Default::default()
    };
    let hits = search(&conn, &q).unwrap();
    assert_eq!(hits.len(), 2);

    // Text + tag filter → only the tagged one.
    let q2 = SearchQuery {
        text: Some("ocean".into()),
        tag: Some("calm".into()),
        limit: 50,
        ..Default::default()
    };
    let hits2 = search(&conn, &q2).unwrap();
    assert_eq!(hits2.len(), 1);
    assert_eq!(hits2[0], tagged.id);
}

#[test]
fn remove_tag_person_and_collection_unlink_only() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    write_min_wav(&src.join("x.wav"), 48000, 1, 16, 50);

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let pipeline = ImportPipeline::new(&library, &conn);
    let result = pipeline
        .import_paths(&[src], DuplicateAction::Skip)
        .unwrap();
    let asset_id = AssetId::parse(result.files[0].asset_id.as_ref().unwrap()).unwrap();

    let repo = Repo::new(&conn);
    let tag = repo.upsert_tag("rain").unwrap();
    repo.add_asset_tag(&asset_id, &tag.id).unwrap();
    let person = repo.create_person("Wang Wu", None).unwrap();
    repo.link_asset_person(&asset_id, &person.id).unwrap();
    let col = repo.create_collection("Field", None).unwrap();
    repo.add_asset_collection(&asset_id, &col.id).unwrap();

    repo.remove_asset_tag(&asset_id, &tag.id).unwrap();
    repo.remove_asset_person(&asset_id, &person.id).unwrap();
    repo.remove_asset_from_collection(&asset_id, &col.id)
        .unwrap();

    // Asset file and row remain — only links were dropped.
    let asset = repo.get_asset(&asset_id).unwrap().unwrap();
    assert_eq!(asset.status, soundhub_core::models::AssetStatus::Ready);
    assert!(library.asset_abspath(&asset.library_relpath).exists());

    // Tags/people/collections themselves are not deleted.
    assert!(repo.list_tags().unwrap().iter().any(|t| t.name == "rain"));
    assert!(repo
        .list_people()
        .unwrap()
        .iter()
        .any(|p| p.name == "Wang Wu"));
    assert!(repo
        .list_collections()
        .unwrap()
        .iter()
        .any(|c| c.name == "Field"));
}

#[test]
fn relation_changes_rewrite_fts_without_manual_reindex() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    write_min_wav(&src.join("quiet.wav"), 48000, 1, 16, 50);

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let pipeline = ImportPipeline::new(&library, &conn);
    let result = pipeline
        .import_paths(&[src], DuplicateAction::Skip)
        .unwrap();
    let asset_id = AssetId::parse(result.files[0].asset_id.as_ref().unwrap()).unwrap();

    let repo = Repo::new(&conn);

    // Not linked yet — relation names must not hit.
    assert!(!search(&conn, &SearchQuery::text("seagull"))
        .unwrap()
        .contains(&asset_id));
    assert!(!search(&conn, &SearchQuery::text("Ornithologist"))
        .unwrap()
        .contains(&asset_id));
    assert!(!search(&conn, &SearchQuery::text("Birdsong"))
        .unwrap()
        .contains(&asset_id));

    // Link tag / person / collection — FTS must pick them up automatically.
    let tag = repo.upsert_tag("seagull").unwrap();
    repo.add_asset_tag(&asset_id, &tag.id).unwrap();
    let person = repo.create_person("Ornithologist", None).unwrap();
    repo.link_asset_person(&asset_id, &person.id).unwrap();
    let col = repo.create_collection("Birdsong", None).unwrap();
    repo.add_asset_collection(&asset_id, &col.id).unwrap();

    assert!(
        search(&conn, &SearchQuery::text("seagull"))
            .unwrap()
            .contains(&asset_id),
        "tag name must be searchable right after add_asset_tag"
    );
    assert!(
        search(&conn, &SearchQuery::text("Ornithologist"))
            .unwrap()
            .contains(&asset_id),
        "person name must be searchable right after link_asset_person"
    );
    assert!(
        search(&conn, &SearchQuery::text("Birdsong"))
            .unwrap()
            .contains(&asset_id),
        "collection name must be searchable right after add_asset_collection"
    );

    // Unlink — names must drop out of the index.
    repo.remove_asset_tag(&asset_id, &tag.id).unwrap();
    repo.remove_asset_person(&asset_id, &person.id).unwrap();
    repo.remove_asset_from_collection(&asset_id, &col.id)
        .unwrap();

    assert!(!search(&conn, &SearchQuery::text("seagull"))
        .unwrap()
        .contains(&asset_id));
    assert!(!search(&conn, &SearchQuery::text("Ornithologist"))
        .unwrap()
        .contains(&asset_id));
    assert!(!search(&conn, &SearchQuery::text("Birdsong"))
        .unwrap()
        .contains(&asset_id));

    // Filename still hits after unlink (reindex keeps the static fields).
    assert!(search(&conn, &SearchQuery::text("quiet"))
        .unwrap()
        .contains(&asset_id));
}

#[test]
fn text_search_respects_offset_and_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    for i in 0..10 {
        write_min_wav(&src.join(format!("wave_{i:02}.wav")), 48000, 1, 16, 50);
    }

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let pipeline = ImportPipeline::new(&library, &conn);
    pipeline
        .import_paths(&[src], DuplicateAction::Skip)
        .unwrap();

    // Page through the full result set; pages must not overlap or drop rows.
    let mut seen = Vec::new();
    for page in 0..5u32 {
        let q = SearchQuery {
            text: Some("wave".into()),
            limit: 2,
            offset: page * 2,
            ..Default::default()
        };
        let hits = search(&conn, &q).unwrap();
        assert_eq!(hits.len(), 2, "page {page} should be full");
        seen.extend(hits);
    }
    let unique: std::collections::HashSet<_> = seen.iter().collect();
    assert_eq!(seen.len(), 10, "10 assets across 5 pages of 2");
    assert_eq!(unique.len(), 10, "pages must not overlap");

    // Offset past the end yields empty, not the first page again.
    let q = SearchQuery {
        text: Some("wave".into()),
        limit: 2,
        offset: 100,
        ..Default::default()
    };
    assert!(search(&conn, &q).unwrap().is_empty());
}

#[test]
fn text_and_structured_filters_paginate_true_result_set() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    // 30 files share the token "note"; only the last 6 carry the tag.
    // If candidates were pre-capped at limit*4, a small limit would miss them.
    for i in 0..30 {
        write_min_wav(&src.join(format!("note_{i:02}.wav")), 48000, 1, 16, 50);
    }

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let pipeline = ImportPipeline::new(&library, &conn);
    pipeline
        .import_paths(&[src], DuplicateAction::Skip)
        .unwrap();

    let repo = Repo::new(&conn);
    let assets = repo.list_assets(50, 0).unwrap();
    assert_eq!(assets.len(), 30);
    // Tag the 6 most recently imported (highest imported_at, but FTS rank
    // order is independent — the point is they must all be findable).
    let tag = repo.upsert_tag("keeper").unwrap();
    for a in assets.iter().take(6) {
        repo.add_asset_tag(&a.id, &tag.id).unwrap();
    }

    // limit=2 → old limit*4 would only look at 8 candidates and could miss
    // tagged rows outside that window. SQL-side filtering finds all 6.
    let q = SearchQuery {
        text: Some("note".into()),
        tag: Some("keeper".into()),
        limit: 2,
        offset: 0,
        ..Default::default()
    };
    let page1 = search(&conn, &q).unwrap();
    assert_eq!(page1.len(), 2);

    let q2 = SearchQuery {
        text: Some("note".into()),
        tag: Some("keeper".into()),
        limit: 2,
        offset: 2,
        ..Default::default()
    };
    let page2 = search(&conn, &q2).unwrap();
    assert_eq!(page2.len(), 2);

    let q3 = SearchQuery {
        text: Some("note".into()),
        tag: Some("keeper".into()),
        limit: 10,
        offset: 0,
        ..Default::default()
    };
    let all = search(&conn, &q3).unwrap();
    assert_eq!(all.len(), 6, "every tagged row must be reachable");
}

#[test]
fn delete_asset_removes_fts_row() {
    let tmp = tempfile::tempdir().unwrap();
    let lib_dir = tmp.path().join("Library");
    let (_library, conn) = setup_library(&lib_dir);
    let repo = Repo::new(&conn);

    // Incomplete asset (never ready) that has already been reindexed —
    // e.g. tagged mid-import then cleaned up.
    let id = AssetId::parse("sh_99999999999999999999999999").unwrap();
    {
        let asset = soundhub_core::models::Asset {
            id: id.clone(),
            filename: "orphan.wav".into(),
            extension: Some("wav".into()),
            file_size: 10,
            hash: "orphanhash".into(),
            mime_type: Some("audio/wav".into()),
            duration_ms: None,
            sample_rate: None,
            bit_depth: None,
            channels: None,
            channel_layout: None,
            codec: None,
            container: None,
            bitrate: None,
            compression: None,
            recorded_at: None,
            recording_start: None,
            recording_end: None,
            timezone: None,
            utc_offset_minutes: None,
            file_created_at: None,
            file_modified_at: None,
            imported_at: chrono::Utc::now(),
            latitude: None,
            longitude: None,
            altitude: None,
            gps_accuracy: None,
            original_path: "/src/orphan.wav".into(),
            source_volume: None,
            library_relpath: "assets/aa/orphan.wav".into(),
            status: soundhub_core::models::AssetStatus::Copying,
            parse_errors: vec![],
            raw_metadata: Default::default(),
            metadata: vec![],
            deleted_at: None,
        };
        let tx = conn.unchecked_transaction().unwrap();
        repo.insert_pending_asset(&tx, &asset).unwrap();
        tx.commit().unwrap();
    }
    repo.reindex_asset(&id).unwrap();

    let fts_count = |conn: &rusqlite::Connection| -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM assets_fts WHERE asset_id = ?1",
            [id.as_str()],
            |r| r.get(0),
        )
        .unwrap()
    };
    // search() filters status='ready', so assert on the FTS table directly.
    assert_eq!(
        fts_count(&conn),
        1,
        "precondition: incomplete asset is indexed"
    );

    repo.delete_asset(&id).unwrap();

    assert_eq!(
        fts_count(&conn),
        0,
        "delete_asset must drop the FTS row (virtual table has no FK cascade)"
    );
    assert!(repo.get_asset(&id).unwrap().is_none());
}

#[test]
fn import_indexes_filename_path_and_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    write_min_wav(&src.join("rain_on_tin.wav"), 48000, 2, 24, 100);

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let pipeline = ImportPipeline::new(&library, &conn);
    let result = pipeline
        .import_paths(&[src], DuplicateAction::Skip)
        .unwrap();
    let asset_id = AssetId::parse(result.files[0].asset_id.as_ref().unwrap()).unwrap();

    // Filename token.
    assert!(search(&conn, &SearchQuery::text("rain"))
        .unwrap()
        .contains(&asset_id));
    // Original path token (source dir name lands in original_path).
    assert!(
        search(&conn, &SearchQuery::text("src"))
            .unwrap()
            .contains(&asset_id),
        "original_path should be searchable"
    );
    // Technical metadata token (sample rate written into metadata_values).
    assert!(
        search(&conn, &SearchQuery::text("48000"))
            .unwrap()
            .contains(&asset_id),
        "metadata_text should carry sample_rate"
    );
}
