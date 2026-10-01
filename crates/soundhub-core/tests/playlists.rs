//! Playlist create / add tracks / reorder tests.

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

/// 3 assets, ids sorted by filename: a, b, c.
fn setup_three() -> (tempfile::TempDir, Library, rusqlite::Connection, Vec<AssetId>) {
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
    pipeline.import_paths(&[src], DuplicateAction::Skip).unwrap();

    let repo = Repo::new(&conn);
    let mut assets = repo.list_assets(10, 0).unwrap();
    assets.sort_by(|x, y| x.filename.cmp(&y.filename));
    let ids: Vec<AssetId> = assets.into_iter().map(|a| a.id).collect();
    (tmp, library, conn, ids)
}

fn names(tracks: &[PlaylistTrack]) -> Vec<&str> {
    tracks.iter().map(|t| t.filename.as_str()).collect()
}

#[test]
fn create_playlist_and_add_tracks() {
    let (_tmp, _lib, conn, ids) = setup_three();
    let repo = Repo::new(&conn);

    let pl = repo.create_playlist("Road Trip").unwrap();
    assert_eq!(pl.track_count, 0);
    assert!(repo.get_playlist(&pl.id).unwrap().unwrap().track_count == 0);

    // Append in order a, b, c.
    assert_eq!(repo.playlist_add_track(&pl.id, &ids[0]).unwrap(), 0);
    assert_eq!(repo.playlist_add_track(&pl.id, &ids[1]).unwrap(), 1);
    assert_eq!(repo.playlist_add_track(&pl.id, &ids[2]).unwrap(), 2);

    let tracks = repo.list_playlist_tracks(&pl.id).unwrap();
    assert_eq!(names(&tracks), vec!["a.wav", "b.wav", "c.wav"]);
    assert_eq!(repo.get_playlist(&pl.id).unwrap().unwrap().track_count, 3);

    // Adding a duplicate is a no-op at the same position.
    assert_eq!(repo.playlist_add_track(&pl.id, &ids[1]).unwrap(), 1);
    assert_eq!(repo.list_playlist_tracks(&pl.id).unwrap().len(), 3);
}

#[test]
fn remove_track_compacts_positions() {
    let (_tmp, _lib, conn, ids) = setup_three();
    let repo = Repo::new(&conn);
    let pl = repo.create_playlist("P").unwrap();
    for id in &ids {
        repo.playlist_add_track(&pl.id, id).unwrap();
    }

    assert!(repo.playlist_remove_track(&pl.id, &ids[0]).unwrap());
    let tracks = repo.list_playlist_tracks(&pl.id).unwrap();
    assert_eq!(names(&tracks), vec!["b.wav", "c.wav"]);
    assert_eq!(tracks[0].position, 0);
    assert_eq!(tracks[1].position, 1);

    assert!(!repo.playlist_remove_track(&pl.id, &ids[0]).unwrap());
}

#[test]
fn reorder_replaces_full_order() {
    let (_tmp, _lib, conn, ids) = setup_three();
    let repo = Repo::new(&conn);
    let pl = repo.create_playlist("P").unwrap();
    for id in &ids {
        repo.playlist_add_track(&pl.id, id).unwrap();
    }

    // Reverse: c, b, a
    let new_order = vec![ids[2].clone(), ids[1].clone(), ids[0].clone()];
    repo.playlist_reorder(&pl.id, &new_order).unwrap();
    assert_eq!(
        names(&repo.list_playlist_tracks(&pl.id).unwrap()),
        vec!["c.wav", "b.wav", "a.wav"]
    );

    // Mismatched membership is rejected.
    let bad = vec![ids[0].clone()];
    assert!(repo.playlist_reorder(&pl.id, &bad).is_err());
}

#[test]
fn move_track_shifts_neighbours() {
    let (_tmp, _lib, conn, ids) = setup_three();
    let repo = Repo::new(&conn);
    let pl = repo.create_playlist("P").unwrap();
    for id in &ids {
        repo.playlist_add_track(&pl.id, id).unwrap();
    }
    // a b c → move first to last: b c a
    repo.playlist_move_track(&pl.id, 0, 2).unwrap();
    assert_eq!(
        names(&repo.list_playlist_tracks(&pl.id).unwrap()),
        vec!["b.wav", "c.wav", "a.wav"]
    );
    // Move back: a b c
    repo.playlist_move_track(&pl.id, 2, 0).unwrap();
    assert_eq!(
        names(&repo.list_playlist_tracks(&pl.id).unwrap()),
        vec!["a.wav", "b.wav", "c.wav"]
    );
    // Out of range errors.
    assert!(repo.playlist_move_track(&pl.id, 0, 9).is_err());
}

#[test]
fn rename_and_delete_playlist() {
    let (_tmp, _lib, conn, ids) = setup_three();
    let repo = Repo::new(&conn);
    let pl = repo.create_playlist("Old Name").unwrap();
    repo.playlist_add_track(&pl.id, &ids[0]).unwrap();

    repo.rename_playlist(&pl.id, "New Name").unwrap();
    assert_eq!(repo.get_playlist(&pl.id).unwrap().unwrap().name, "New Name");

    repo.delete_playlist(&pl.id).unwrap();
    assert!(repo.get_playlist(&pl.id).unwrap().is_none());
    assert!(repo.list_playlists().unwrap().is_empty());
    // Asset itself survives playlist deletion.
    assert!(repo.get_asset(&ids[0]).unwrap().is_some());
}

#[test]
fn list_playlists_counts_tracks() {
    let (_tmp, _lib, conn, ids) = setup_three();
    let repo = Repo::new(&conn);
    let p1 = repo.create_playlist("One").unwrap();
    let p2 = repo.create_playlist("Two").unwrap();
    repo.playlist_add_track(&p1.id, &ids[0]).unwrap();
    repo.playlist_add_track(&p1.id, &ids[1]).unwrap();
    repo.playlist_add_track(&p2.id, &ids[2]).unwrap();

    let all = repo.list_playlists().unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].name, "One");
    assert_eq!(all[0].track_count, 2);
    assert_eq!(all[1].track_count, 1);
}
