//! Tag management (rename / delete / merge / usage) + multi-tag filter tests.

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

/// Library with 3 assets: a.wav (tags: alpha, shared), b.wav (beta, shared), c.wav (gamma).
fn setup_three() -> (
    tempfile::TempDir,
    Library,
    rusqlite::Connection,
    Vec<AssetId>,
) {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    write_min_wav(&src.join("a.wav"), 48000, 1, 16, 40);
    write_min_wav(&src.join("b.wav"), 48000, 1, 16, 40);
    write_min_wav(&src.join("c.wav"), 48000, 1, 16, 40);

    let lib_dir = tmp.path().join("Library");
    let library = Library::create(&lib_dir).unwrap();
    let conn = library.open_db().unwrap();
    let pipeline = ImportPipeline::new(&library, &conn);
    pipeline
        .import_paths(&[src], DuplicateAction::Skip)
        .unwrap();

    let repo = Repo::new(&conn);
    let mut assets = repo.list_assets(10, 0).unwrap();
    assets.sort_by(|x, y| x.filename.cmp(&y.filename));
    let ids: Vec<AssetId> = assets.into_iter().map(|a| a.id).collect();
    assert_eq!(ids.len(), 3);

    undo::add_tag(&conn, &ids[0], "alpha").unwrap();
    undo::add_tag(&conn, &ids[0], "shared").unwrap();
    undo::add_tag(&conn, &ids[1], "beta").unwrap();
    undo::add_tag(&conn, &ids[1], "shared").unwrap();
    undo::add_tag(&conn, &ids[2], "gamma").unwrap();

    (tmp, library, conn, ids)
}

#[test]
fn list_tags_with_usage_counts_live_assets() {
    let (_tmp, _lib, conn, ids) = setup_three();
    let repo = Repo::new(&conn);
    let usage = repo.list_tags_with_usage().unwrap();
    let by_name = |n: &str| usage.iter().find(|u| u.name == n).unwrap().clone();
    assert_eq!(by_name("shared").asset_count, 2);
    assert_eq!(by_name("alpha").asset_count, 1);
    assert_eq!(by_name("gamma").asset_count, 1);

    // Trashed assets stop counting. ids[0] = a.wav carries "alpha".
    undo::soft_delete(&conn, &[ids[0].clone()]).unwrap();
    let usage2 = repo.list_tags_with_usage().unwrap();
    assert_eq!(
        usage2
            .iter()
            .find(|u| u.name == "alpha")
            .unwrap()
            .asset_count,
        0
    );
    assert_eq!(
        usage2
            .iter()
            .find(|u| u.name == "shared")
            .unwrap()
            .asset_count,
        1
    );
}

#[test]
fn rename_tag_updates_fts_and_is_undoable() {
    let (_tmp, _lib, conn, ids) = setup_three();
    let repo = Repo::new(&conn);
    let alpha = repo
        .list_tags()
        .unwrap()
        .into_iter()
        .find(|t| t.name == "alpha")
        .unwrap();

    // Old name searchable, new name not.
    assert!(search(&conn, &SearchQuery::text("alpha"))
        .unwrap()
        .contains(&ids[0]));
    assert!(!search(&conn, &SearchQuery::text("zeta"))
        .unwrap()
        .contains(&ids[0]));

    undo::rename_tag(&conn, &alpha.id, "zeta").unwrap();
    assert!(!search(&conn, &SearchQuery::text("alpha"))
        .unwrap()
        .contains(&ids[0]));
    assert!(search(&conn, &SearchQuery::text("zeta"))
        .unwrap()
        .contains(&ids[0]));

    undo::undo_last(&conn).unwrap().unwrap();
    assert!(search(&conn, &SearchQuery::text("alpha"))
        .unwrap()
        .contains(&ids[0]));
}

#[test]
fn rename_tag_rejects_collision() {
    let (_tmp, _lib, conn, _ids) = setup_three();
    let repo = Repo::new(&conn);
    let alpha = repo
        .list_tags()
        .unwrap()
        .into_iter()
        .find(|t| t.name == "alpha")
        .unwrap();
    assert!(undo::rename_tag(&conn, &alpha.id, "beta").is_err());
}

#[test]
fn delete_tag_unlinks_everywhere_and_is_undoable() {
    let (_tmp, _lib, conn, ids) = setup_three();
    let repo = Repo::new(&conn);
    let shared = repo
        .list_tags()
        .unwrap()
        .into_iter()
        .find(|t| t.name == "shared")
        .unwrap();

    undo::delete_tag(&conn, &shared.id).unwrap();
    assert!(!search(&conn, &SearchQuery::text("shared"))
        .unwrap()
        .contains(&ids[0]));
    assert!(!search(&conn, &SearchQuery::text("shared"))
        .unwrap()
        .contains(&ids[1]));
    assert!(repo.list_tags().unwrap().iter().all(|t| t.name != "shared"));

    undo::undo_last(&conn).unwrap().unwrap();
    assert!(search(&conn, &SearchQuery::text("shared"))
        .unwrap()
        .contains(&ids[0]));
    assert!(search(&conn, &SearchQuery::text("shared"))
        .unwrap()
        .contains(&ids[1]));
}

#[test]
fn merge_tags_moves_links_and_is_undoable() {
    let (_tmp, _lib, conn, ids) = setup_three();
    let repo = Repo::new(&conn);
    let alpha = repo
        .list_tags()
        .unwrap()
        .into_iter()
        .find(|t| t.name == "alpha")
        .unwrap();
    let shared = repo
        .list_tags()
        .unwrap()
        .into_iter()
        .find(|t| t.name == "shared")
        .unwrap();

    // a.wav has alpha+shared; merge alpha→shared should leave a.wav with just shared.
    undo::merge_tags(&conn, &alpha.id, &shared.id).unwrap();
    assert!(repo.list_tags().unwrap().iter().all(|t| t.name != "alpha"));
    assert!(search(&conn, &SearchQuery::text("shared"))
        .unwrap()
        .contains(&ids[0]));
    assert!(!search(&conn, &SearchQuery::text("alpha"))
        .unwrap()
        .contains(&ids[0]));

    undo::undo_last(&conn).unwrap().unwrap();
    assert!(repo.list_tags().unwrap().iter().any(|t| t.name == "alpha"));
    assert!(search(&conn, &SearchQuery::text("alpha"))
        .unwrap()
        .contains(&ids[0]));
}

#[test]
fn multi_tag_filter_and_mode() {
    let (_tmp, _lib, conn, ids) = setup_three();
    // ids[0]=a has alpha+shared; ids[1]=b has beta+shared; ids[2]=c has gamma.

    // AND: must have both alpha and shared → only a.
    let q = SearchQuery {
        tags: vec!["alpha".into(), "shared".into()],
        tags_all: true,
        limit: 50,
        ..Default::default()
    };
    let hits = search(&conn, &q).unwrap();
    assert_eq!(hits, vec![ids[0].clone()]);

    // OR: alpha or beta → a and b.
    let q = SearchQuery {
        tags: vec!["alpha".into(), "beta".into()],
        tags_all: false,
        limit: 50,
        ..Default::default()
    };
    let mut hits = search(&conn, &q).unwrap();
    hits.sort_by(|x, y| x.as_str().cmp(y.as_str()));
    let mut expected = vec![ids[0].clone(), ids[1].clone()];
    expected.sort_by(|x, y| x.as_str().cmp(y.as_str()));
    assert_eq!(hits, expected);

    // OR: shared or gamma → all three.
    let q = SearchQuery {
        tags: vec!["shared".into(), "gamma".into()],
        tags_all: false,
        limit: 50,
        ..Default::default()
    };
    assert_eq!(search(&conn, &q).unwrap().len(), 3);

    // Legacy single `tag` still works.
    let q = SearchQuery {
        tag: Some("gamma".into()),
        limit: 50,
        ..Default::default()
    };
    assert_eq!(search(&conn, &q).unwrap(), vec![ids[2].clone()]);
}

#[test]
fn multi_tag_filter_with_text() {
    let (_tmp, _lib, conn, ids) = setup_three();
    // Text "a.wav" + tag shared (OR among tags is irrelevant — one tag) → a.
    let q = SearchQuery {
        text: Some("a.wav".into()),
        tags: vec!["shared".into()],
        tags_all: true,
        limit: 50,
        ..Default::default()
    };
    let hits = search(&conn, &q).unwrap();
    assert_eq!(hits, vec![ids[0].clone()]);
}
