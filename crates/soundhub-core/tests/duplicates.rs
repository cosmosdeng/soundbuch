//! In-library duplicate detection and cleanup tests.

use std::fs::File;
use std::io::Write;
use std::path::Path;

use soundhub_core::db::Repo;
use soundhub_core::duplicates::{self, KeepStrategy};
use soundhub_core::import::ImportPipeline;
use soundhub_core::library::Library;
use soundhub_core::models::*;
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

/// Import the same bytes twice via ImportAsDuplicate so the library
/// legitimately holds two assets with one SHA-256.
fn import_pair_with_same_content() -> (
    tempfile::TempDir,
    Library,
    rusqlite::Connection,
    Vec<AssetId>,
) {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    // Identical content, different filenames.
    write_min_wav(&src.join("copy_a.wav"), 48000, 1, 16, 80);
    std::fs::copy(src.join("copy_a.wav"), src.join("copy_b.wav")).unwrap();
    write_min_wav(&src.join("unique.wav"), 44100, 1, 16, 80);

    let lib_dir = tmp.path().join("Library");
    let library = Library::create(&lib_dir).unwrap();
    let conn = library.open_db().unwrap();
    let pipeline = ImportPipeline::new(&library, &conn);

    // First pass: import everything, keep duplicates.
    let r = pipeline
        .import_paths(
            std::slice::from_ref(&src),
            DuplicateAction::ImportAsDuplicate,
        )
        .unwrap();
    assert_eq!(r.success, 3, "{r:?}");

    let repo = Repo::new(&conn);
    let assets = repo.list_assets(20, 0).unwrap();
    let ids: Vec<AssetId> = assets.into_iter().map(|a| a.id).collect();
    (tmp, library, conn, ids)
}

#[test]
fn find_groups_reports_content_identical_clusters() {
    let (_tmp, _lib, conn, ids) = import_pair_with_same_content();
    assert_eq!(ids.len(), 3);

    let groups = duplicates::find_groups(&conn).unwrap();
    assert_eq!(groups.len(), 1, "exactly one hash shared by 2 assets");
    assert_eq!(groups[0].assets.len(), 2);
    // Unique file is not in any group.
    let grouped: Vec<&AssetId> = groups[0].assets.iter().map(|a| &a.id).collect();
    let uniques: Vec<&AssetId> = ids.iter().filter(|i| !grouped.contains(i)).collect();
    assert_eq!(uniques.len(), 1);
}

#[test]
fn dedupe_keep_oldest_trashes_later_copies() {
    let (_tmp, _lib, conn, _ids) = import_pair_with_same_content();
    let groups = duplicates::find_groups(&conn).unwrap();
    let oldest = groups[0].assets[0].id.clone();
    let newest = groups[0].assets[1].id.clone();

    let (g, trashed) = duplicates::dedupe(&conn, KeepStrategy::Oldest).unwrap();
    assert_eq!(g, 1);
    assert_eq!(trashed, 1);

    let repo = Repo::new(&conn);
    assert!(repo
        .get_asset(&oldest)
        .unwrap()
        .unwrap()
        .deleted_at
        .is_none());
    assert!(repo
        .get_asset(&newest)
        .unwrap()
        .unwrap()
        .deleted_at
        .is_some());
    assert!(duplicates::find_groups(&conn).unwrap().is_empty());
    // Unique asset untouched.
    assert_eq!(repo.count_ready_assets().unwrap(), 2);
}

#[test]
fn dedupe_keep_newest_picks_the_other_copy() {
    let (_tmp, _lib, conn, _ids) = import_pair_with_same_content();
    let groups = duplicates::find_groups(&conn).unwrap();
    let oldest = groups[0].assets[0].id.clone();
    let newest = groups[0].assets[1].id.clone();

    duplicates::dedupe(&conn, KeepStrategy::Newest).unwrap();
    let repo = Repo::new(&conn);
    assert!(repo
        .get_asset(&newest)
        .unwrap()
        .unwrap()
        .deleted_at
        .is_none());
    assert!(repo
        .get_asset(&oldest)
        .unwrap()
        .unwrap()
        .deleted_at
        .is_some());
}

#[test]
fn dedupe_group_keep_specific_id() {
    let (_tmp, _lib, conn, _ids) = import_pair_with_same_content();
    let groups = duplicates::find_groups(&conn).unwrap();
    let hash = groups[0].hash.clone();
    let keep = groups[0].assets[1].id.clone();
    let drop = groups[0].assets[0].id.clone();

    let n = duplicates::dedupe_group(&conn, &hash, KeepStrategy::Id(keep.clone())).unwrap();
    assert_eq!(n, 1);
    let repo = Repo::new(&conn);
    assert!(repo.get_asset(&keep).unwrap().unwrap().deleted_at.is_none());
    assert!(repo.get_asset(&drop).unwrap().unwrap().deleted_at.is_some());
}

#[test]
fn dedupe_is_undoable() {
    let (_tmp, _lib, conn, _ids) = import_pair_with_same_content();
    duplicates::dedupe(&conn, KeepStrategy::Oldest).unwrap();
    assert!(duplicates::find_groups(&conn).unwrap().is_empty());

    undo::undo_last(&conn).unwrap().unwrap();
    assert_eq!(
        duplicates::find_groups(&conn).unwrap().len(),
        1,
        "undo restores the trashed copy → group reappears"
    );
}

#[test]
fn find_groups_ignores_trashed_members() {
    let (_tmp, _lib, conn, _ids) = import_pair_with_same_content();
    let groups = duplicates::find_groups(&conn).unwrap();
    let drop = groups[0].assets[1].id.clone();
    undo::soft_delete(&conn, &[drop]).unwrap();

    assert!(
        duplicates::find_groups(&conn).unwrap().is_empty(),
        "one live + one trashed copy is not a live duplicate group"
    );
}
