//! Recycle bin + undo stack integration tests.

use std::fs::File;
use std::io::Write;
use std::path::Path;

use soundhub_core::db::Repo;
use soundhub_core::import::ImportPipeline;
use soundhub_core::library::Library;
use soundhub_core::models::*;
use soundhub_core::search::{search, SearchQuery};
use soundhub_core::undo;
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

fn setup_with_one() -> (tempfile::TempDir, Library, rusqlite::Connection, AssetId) {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let wav = src.join("clip.wav");
    write_min_wav(&wav, 48000, 1, 16, 50);

    let lib_dir = tmp.path().join("Library");
    let library = Library::create(&lib_dir).unwrap();
    let conn = library.open_db().unwrap();
    let pipeline = ImportPipeline::new(&library, &conn);
    let result = pipeline
        .import_paths(&[wav], DuplicateAction::Skip)
        .unwrap();
    let id = AssetId::parse(result.files[0].asset_id.as_ref().unwrap()).unwrap();
    (tmp, library, conn, id)
}

#[test]
fn soft_delete_hides_from_search_and_list_restores() {
    let (_tmp, _lib, conn, id) = setup_with_one();
    let repo = Repo::new(&conn);

    assert_eq!(repo.count_ready_assets().unwrap(), 1);
    assert!(search(&conn, &SearchQuery::text("clip"))
        .unwrap()
        .contains(&id));

    let n = repo.soft_delete_assets(std::slice::from_ref(&id)).unwrap();
    assert_eq!(n, 1);

    assert_eq!(repo.count_ready_assets().unwrap(), 0);
    assert_eq!(repo.count_deleted_assets().unwrap(), 1);
    assert!(!search(&conn, &SearchQuery::text("clip"))
        .unwrap()
        .contains(&id));
    assert!(repo.list_deleted_assets(10, 0).unwrap().len() == 1);

    // Soft-delete is idempotent.
    assert_eq!(
        repo.soft_delete_assets(std::slice::from_ref(&id)).unwrap(),
        0
    );

    assert_eq!(repo.restore_assets(std::slice::from_ref(&id)).unwrap(), 1);
    assert_eq!(repo.count_ready_assets().unwrap(), 1);
    assert_eq!(repo.count_deleted_assets().unwrap(), 0);
    assert!(search(&conn, &SearchQuery::text("clip"))
        .unwrap()
        .contains(&id));
}

#[test]
fn purge_removes_file_and_row_but_not_when_live() {
    let (_tmp, library, conn, id) = setup_with_one();
    let repo = Repo::new(&conn);
    let asset = repo.get_asset(&id).unwrap().unwrap();
    let abs = library.asset_abspath(&asset.library_relpath);
    assert!(
        abs.exists(),
        "library copy should exist at {}",
        abs.display()
    );

    // Live assets cannot be purged.
    repo.purge_asset(&id, &abs).unwrap();
    assert!(
        repo.get_asset(&id).unwrap().is_some(),
        "live asset survives purge"
    );
    assert!(abs.exists(), "live library copy survives purge");

    repo.soft_delete_assets(std::slice::from_ref(&id)).unwrap();
    repo.purge_asset(&id, &abs).unwrap();
    assert!(repo.get_asset(&id).unwrap().is_none());
    assert!(!abs.exists(), "library copy removed");
    assert_eq!(repo.count_deleted_assets().unwrap(), 0);
}

#[test]
fn undo_add_tag_removes_it_again() {
    let (_tmp, _lib, conn, id) = setup_with_one();
    assert!(undo::add_tag(&conn, &id, "rain").is_ok());
    assert!(search(&conn, &SearchQuery::text("rain"))
        .unwrap()
        .contains(&id));

    let label = undo::undo_last(&conn).unwrap().expect("has undo");
    assert!(label.contains("rain") || label.contains("tag"));
    assert!(!search(&conn, &SearchQuery::text("rain"))
        .unwrap()
        .contains(&id));

    assert!(undo::undo_last(&conn).unwrap().is_none(), "stack empty");
}

#[test]
fn undo_remove_tag_relinks() {
    let (_tmp, _lib, conn, id) = setup_with_one();
    undo::add_tag(&conn, &id, "wind").unwrap();
    let repo = Repo::new(&conn);
    let tag = repo.list_tags().unwrap()[0].clone();

    undo::remove_tag(&conn, &id, &tag.id).unwrap();
    assert!(!search(&conn, &SearchQuery::text("wind"))
        .unwrap()
        .contains(&id));

    undo::undo_last(&conn).unwrap().unwrap();
    assert!(search(&conn, &SearchQuery::text("wind"))
        .unwrap()
        .contains(&id));
}

#[test]
fn undo_soft_delete_restores() {
    let (_tmp, _library, conn, id) = setup_with_one();
    undo::soft_delete(&conn, std::slice::from_ref(&id)).unwrap();
    assert_eq!(Repo::new(&conn).count_deleted_assets().unwrap(), 1);

    undo::undo_last(&conn).unwrap().unwrap();
    assert_eq!(Repo::new(&conn).count_ready_assets().unwrap(), 1);
    assert!(search(&conn, &SearchQuery::text("clip"))
        .unwrap()
        .contains(&id));
}

#[test]
fn undo_batch_tag_is_single_step() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    write_min_wav(&src.join("a.wav"), 48000, 1, 16, 50);
    write_min_wav(&src.join("b.wav"), 48000, 1, 16, 50);

    let lib_dir = tmp.path().join("Library");
    let library = Library::create(&lib_dir).unwrap();
    let conn = library.open_db().unwrap();
    let pipeline = ImportPipeline::new(&library, &conn);
    pipeline
        .import_paths(&[src], DuplicateAction::Skip)
        .unwrap();
    let repo = Repo::new(&conn);
    let assets = repo.list_assets(10, 0).unwrap();
    let ids: Vec<AssetId> = assets.iter().map(|a| a.id.clone()).collect();
    assert_eq!(ids.len(), 2);

    // Batch-style: record one undo entry covering both adds.
    for id in &ids {
        let t = repo.upsert_tag("reviewed").unwrap();
        repo.add_asset_tag(id, &t.id).unwrap();
    }
    let tag = repo.list_tags().unwrap()[0].clone();
    let actions: Vec<UndoAction> = ids
        .iter()
        .map(|id| UndoAction::RemoveTag {
            asset_id: id.to_string(),
            tag_id: tag.id.to_string(),
        })
        .collect();
    repo.push_undo(
        UndoKind::AddTag,
        "Batch add tag",
        &serde_json::json!({ "actions": actions }),
    )
    .unwrap();

    assert!(search(&conn, &SearchQuery::text("reviewed")).unwrap().len() >= 2);

    undo::undo_last(&conn).unwrap().unwrap();
    assert!(
        search(&conn, &SearchQuery::text("reviewed"))
            .unwrap()
            .is_empty(),
        "one undo clears both tags"
    );
}

#[test]
fn undo_person_and_collection() {
    let (_tmp, _lib, conn, id) = setup_with_one();
    undo::add_person(&conn, &id, "Ada").unwrap();
    undo::add_to_collection(&conn, &id, &soundhub_core::RowId::new()).unwrap_err();
    // valid collection:
    let repo = Repo::new(&conn);
    let col = repo.create_collection("Trip", None).unwrap();
    undo::add_to_collection(&conn, &id, &col.id).unwrap();

    undo::undo_last(&conn).unwrap(); // remove from collection
    assert!(repo.list_collection_assets(&col.id).unwrap().is_empty());
    undo::undo_last(&conn).unwrap(); // remove person
    let people = repo.list_people().unwrap();
    assert!(people.iter().any(|p| p.name == "Ada"), "person row kept");
}
