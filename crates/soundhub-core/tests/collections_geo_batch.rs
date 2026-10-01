//! Smart Collections, GPS queries, and batch-edit integration tests.

use std::fs::File;
use std::io::Write;
use std::path::Path;

use soundhub_core::db::Repo;
use soundhub_core::import::ImportPipeline;
use soundhub_core::library::Library;
use soundhub_core::models::*;
use soundhub_core::AssetId;

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

fn import_one(library: &Library, conn: &rusqlite::Connection, wav: &Path) -> AssetId {
    let pipeline = ImportPipeline::new(library, conn);
    let result = pipeline
        .import_paths(&[wav.to_path_buf()], DuplicateAction::Skip)
        .unwrap();
    AssetId::parse(result.files[0].asset_id.as_ref().unwrap()).unwrap()
}

#[test]
fn smart_collection_matches_by_tag_and_sample_rate() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    write_min_wav(&src.join("hi.wav"), 96000, 1, 24, 50);
    write_min_wav(&src.join("lo.wav"), 44100, 1, 16, 50);

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let pipeline = ImportPipeline::new(&library, &conn);
    pipeline
        .import_paths(&[src], DuplicateAction::Skip)
        .unwrap();

    let repo = Repo::new(&conn);
    let assets = repo.list_assets(10, 0).unwrap();
    assert_eq!(assets.len(), 2);
    let hi = assets
        .iter()
        .find(|a| a.sample_rate == Some(96000))
        .unwrap()
        .id
        .clone();
    let tag = repo.upsert_tag("field").unwrap();
    repo.add_asset_tag(&hi, &tag.id).unwrap();

    let rules = SmartRules {
        match_mode: MatchMode::All,
        conditions: vec![
            SmartCondition {
                field: SmartField::Tag,
                op: SmartOp::Is,
                value: serde_json::json!("field"),
            },
            SmartCondition {
                field: SmartField::SampleRate,
                op: SmartOp::Gte,
                value: serde_json::json!(48000),
            },
        ],
    };
    let smart = repo.create_smart_collection("Field HD", &rules).unwrap();
    assert_eq!(smart.collection_type, CollectionType::Smart);

    let hits = repo.list_collection_assets(&smart.id).unwrap();
    assert_eq!(hits, vec![hi]);

    // Loosen sample_rate → still just the tagged one (other has no tag).
    let rules2 = SmartRules {
        match_mode: MatchMode::All,
        conditions: vec![SmartCondition {
            field: SmartField::Tag,
            op: SmartOp::Is,
            value: serde_json::json!("field"),
        }],
    };
    repo.update_collection_rules(&smart.id, &rules2).unwrap();
    let hits2 = repo.list_collection_assets(&smart.id).unwrap();
    assert_eq!(hits2.len(), 1);

    // Empty rules match nothing.
    let empty = repo
        .create_smart_collection(
            "Empty",
            &SmartRules {
                match_mode: MatchMode::All,
                conditions: vec![],
            },
        )
        .unwrap();
    assert!(repo.list_collection_assets(&empty.id).unwrap().is_empty());
}

#[test]
fn smart_collection_any_mode_ors_conditions() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    write_min_wav(&src.join("a.wav"), 48000, 1, 16, 50);
    write_min_wav(&src.join("b.wav"), 44100, 1, 16, 50);

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let pipeline = ImportPipeline::new(&library, &conn);
    pipeline
        .import_paths(&[src], DuplicateAction::Skip)
        .unwrap();

    let repo = Repo::new(&conn);
    let rules = SmartRules {
        match_mode: MatchMode::Any,
        conditions: vec![
            SmartCondition {
                field: SmartField::Filename,
                op: SmartOp::Is,
                value: serde_json::json!("a.wav"),
            },
            SmartCondition {
                field: SmartField::SampleRate,
                op: SmartOp::Eq,
                value: serde_json::json!(44100),
            },
        ],
    };
    let smart = repo.create_smart_collection("A or 44k", &rules).unwrap();
    let hits = repo.list_collection_assets(&smart.id).unwrap();
    assert_eq!(hits.len(), 2, "any-mode should match both");
}

#[test]
fn static_collection_still_uses_membership_table() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let wav = src.join("x.wav");
    write_min_wav(&wav, 48000, 1, 16, 50);

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let id = import_one(&library, &conn, &wav);

    let repo = Repo::new(&conn);
    let col = repo.create_collection("Static", None).unwrap();
    assert!(repo.list_collection_assets(&col.id).unwrap().is_empty());
    repo.add_asset_collection(&id, &col.id).unwrap();
    assert_eq!(repo.list_collection_assets(&col.id).unwrap(), vec![id]);
}

#[test]
fn gps_listing_and_bbox_filter() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    write_min_wav(&src.join("jeju.wav"), 48000, 1, 16, 50);
    write_min_wav(&src.join("paris.wav"), 48000, 1, 16, 50);
    write_min_wav(&src.join("nogps.wav"), 48000, 1, 16, 50);

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let pipeline = ImportPipeline::new(&library, &conn);
    pipeline
        .import_paths(&[src], DuplicateAction::Skip)
        .unwrap();

    let repo = Repo::new(&conn);
    let assets = repo.list_assets(10, 0).unwrap();
    let jeju = assets
        .iter()
        .find(|a| a.filename == "jeju.wav")
        .unwrap()
        .id
        .clone();
    let paris = assets
        .iter()
        .find(|a| a.filename == "paris.wav")
        .unwrap()
        .id
        .clone();

    // Stamp GPS onto two assets (import path may not have GPS in min WAV).
    for (id, lat, lon) in [(&jeju, 33.4996, 126.5312), (&paris, 48.8566, 2.3522)] {
        let asset = repo.get_asset(id).unwrap().unwrap();
        let mut a = asset;
        a.latitude = Some(lat);
        a.longitude = Some(lon);
        // Direct SQL update — no public setter for GPS yet.
        conn.execute(
            "UPDATE assets SET latitude = ?1, longitude = ?2 WHERE id = ?3",
            rusqlite::params![lat, lon, id.as_str()],
        )
        .unwrap();
        let _ = a;
    }

    let with_gps = repo.list_assets_with_gps().unwrap();
    assert_eq!(with_gps.len(), 2, "only two assets carry GPS");

    // Bounding box around Jeju.
    let near = repo.assets_in_bbox(33.0, 34.0, 126.0, 127.0).unwrap();
    assert_eq!(near.len(), 1);
    assert_eq!(near[0].0.id, jeju);

    // Wide box covers both.
    let wide = repo.assets_in_bbox(-90.0, 90.0, -180.0, 180.0).unwrap();
    assert_eq!(wide.len(), 2);
    assert!(wide.iter().any(|(a, _, _)| a.id == paris));
}

#[test]
fn batch_add_and_remove_tag_person_collection() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    write_min_wav(&src.join("a.wav"), 48000, 1, 16, 50);
    write_min_wav(&src.join("b.wav"), 48000, 1, 16, 50);
    write_min_wav(&src.join("c.wav"), 48000, 1, 16, 50);

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let pipeline = ImportPipeline::new(&library, &conn);
    pipeline
        .import_paths(&[src], DuplicateAction::Skip)
        .unwrap();

    let repo = Repo::new(&conn);
    let assets = repo.list_assets(10, 0).unwrap();
    assert_eq!(assets.len(), 3);
    let ids: Vec<AssetId> = assets.iter().map(|a| a.id.clone()).collect();

    // Batch tag all three.
    let n = repo.batch_add_tag(&ids, "reviewed").unwrap();
    assert_eq!(n, 3);
    for id in &ids {
        let names = {
            let mut stmt = conn
                .prepare(
                    "SELECT t.name FROM tags t JOIN asset_tags at ON at.tag_id = t.id \
                     WHERE at.asset_id = ?1",
                )
                .unwrap();
            let rows: Vec<String> = stmt
                .query_map([id.as_str()], |r| r.get(0))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();
            rows
        };
        assert!(names.contains(&"reviewed".to_string()));
    }

    // Batch person + collection.
    assert_eq!(repo.batch_add_person(&ids, "Field Team").unwrap(), 3);
    let col = repo.create_collection("Batch Col", None).unwrap();
    assert_eq!(repo.batch_add_to_collection(&ids, &col.id).unwrap(), 3);
    assert_eq!(repo.list_collection_assets(&col.id).unwrap().len(), 3);

    // Batch remove tag from a subset.
    let subset = &ids[..2];
    assert_eq!(repo.batch_remove_tag(subset, "reviewed").unwrap(), 2);
    assert_eq!(
        repo.list_collection_assets(&col.id).unwrap().len(),
        3,
        "collection membership untouched"
    );

    // Smart collection sees the remaining tagged asset.
    let rules = SmartRules {
        match_mode: MatchMode::All,
        conditions: vec![SmartCondition {
            field: SmartField::Tag,
            op: SmartOp::Is,
            value: serde_json::json!("reviewed"),
        }],
    };
    let smart = repo
        .create_smart_collection("Still reviewed", &rules)
        .unwrap();
    let hits = repo.list_collection_assets(&smart.id).unwrap();
    assert_eq!(hits, vec![ids[2].clone()]);

    // Batch unlink from collection.
    assert_eq!(repo.batch_remove_from_collection(&ids, &col.id).unwrap(), 3);
    assert!(repo.list_collection_assets(&col.id).unwrap().is_empty());
}
