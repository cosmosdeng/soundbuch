//! Import pipeline integration tests (PRD Test 02-06, 10).

use std::fs::File;
use std::io::Write;
use std::path::Path;

use soundhub_core::db::Repo;
use soundhub_core::import::{hash_file, ImportPipeline};
use soundhub_core::library::Library;
use soundhub_core::models::{AssetStatus, DuplicateAction};
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
    // Deterministic payload so hashes are content-based, not name-based.
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
fn test_02_single_file_import() {
    let tmp = tempfile::tempdir().unwrap();
    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let wav = src_dir.join("rec001.wav");
    write_min_wav(&wav, 48000, 2, 24, 48000);

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let pipeline = ImportPipeline::new(&library, &conn);

    let result = pipeline
        .import_paths(std::slice::from_ref(&wav), DuplicateAction::Skip)
        .unwrap();
    assert_eq!(result.success, 1, "{:?}", result.files);
    assert_eq!(result.failed, 0);

    let asset_id = result.files[0].asset_id.clone().unwrap();
    let asset_id = AssetId::parse(&asset_id).unwrap();
    let asset = soundhub_core::db::Repo::new(&conn)
        .get_asset(&asset_id)
        .unwrap()
        .unwrap();

    assert_eq!(asset.status, AssetStatus::Ready);
    assert_eq!(asset.filename, "rec001.wav");
    assert_eq!(asset.sample_rate, Some(48000));
    assert_eq!(asset.bit_depth, Some(24));
    assert_eq!(asset.channels, Some(2));
    assert_eq!(asset.duration_ms, Some(1000));

    // Integrity: library copy hash == source hash.
    let src_hash = hash_file(&wav).unwrap();
    assert_eq!(asset.hash, src_hash);
    let dest = library.asset_abspath(&asset.library_relpath);
    assert!(dest.exists());
    assert_eq!(hash_file(&dest).unwrap(), src_hash);

    // Source file must be untouched (Copy, not Move).
    assert!(wav.exists());
}

#[test]
fn test_03_folder_with_many_files() {
    let tmp = tempfile::tempdir().unwrap();
    let src_dir = tmp.path().join("sound");
    std::fs::create_dir_all(&src_dir).unwrap();
    for i in 0..100 {
        // Distinct frame counts so every hash differs.
        write_min_wav(
            &src_dir.join(format!("{i:05}.wav")),
            48000,
            1,
            16,
            100 + i as u32,
        );
    }

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let pipeline = ImportPipeline::new(&library, &conn);

    let result = pipeline
        .import_paths(&[src_dir], DuplicateAction::Skip)
        .unwrap();
    assert_eq!(result.total, 100);
    assert_eq!(result.success, 100, "failed={:?}", result.files);
    assert_eq!(
        soundhub_core::db::Repo::new(&conn)
            .count_ready_assets()
            .unwrap(),
        100
    );
}

#[test]
fn test_04_recursive_subdirectories() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Sound");
    std::fs::create_dir_all(root.join("A")).unwrap();
    std::fs::create_dir_all(root.join("B").join("C")).unwrap();
    write_min_wav(&root.join("A").join("001.wav"), 48000, 2, 24, 4800);
    write_min_wav(
        &root.join("B").join("C").join("002.wav"),
        44100,
        1,
        16,
        2205,
    );

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let pipeline = ImportPipeline::new(&library, &conn);
    let result = pipeline
        .import_paths(&[root], DuplicateAction::Skip)
        .unwrap();
    assert_eq!(result.success, 2);
}

#[test]
fn test_05_duplicate_detection_by_sha256() {
    let tmp = tempfile::tempdir().unwrap();
    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let wav = src_dir.join("0001.wav");
    write_min_wav(&wav, 48000, 2, 24, 4800);

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let pipeline = ImportPipeline::new(&library, &conn);

    let first = pipeline
        .import_paths(std::slice::from_ref(&wav), DuplicateAction::Skip)
        .unwrap();
    assert_eq!(first.success, 1);

    // Same bytes under a different name is still a duplicate.
    let copy_path = src_dir.join("renamed.wav");
    std::fs::copy(&wav, &copy_path).unwrap();
    let second = pipeline
        .import_paths(&[copy_path], DuplicateAction::Skip)
        .unwrap();
    assert_eq!(second.success, 0);
    assert_eq!(second.duplicate, 1);

    // ImportAsDuplicate must be allowed but still track both.
    let copy_path2 = src_dir.join("renamed2.wav");
    std::fs::copy(&wav, &copy_path2).unwrap();
    let third = pipeline
        .import_paths(&[copy_path2], DuplicateAction::ImportAsDuplicate)
        .unwrap();
    assert_eq!(third.success, 1);
}

#[test]
fn test_06_external_source_can_be_removed_after_import() {
    let tmp = tempfile::tempdir().unwrap();
    // Simulate a USB / external volume that we delete after import.
    let usb = tmp.path().join("USB");
    std::fs::create_dir_all(&usb).unwrap();
    let wav = usb.join("field.wav");
    write_min_wav(&wav, 96000, 2, 24, 9600);

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let pipeline = ImportPipeline::new(&library, &conn);
    let result = pipeline
        .import_paths(std::slice::from_ref(&wav), DuplicateAction::Skip)
        .unwrap();
    let asset_id = AssetId::parse(result.files[0].asset_id.as_ref().unwrap()).unwrap();

    // Unplug the source device.
    std::fs::remove_dir_all(&usb).unwrap();

    let asset = soundhub_core::db::Repo::new(&conn)
        .get_asset(&asset_id)
        .unwrap()
        .unwrap();
    assert_eq!(asset.status, AssetStatus::Ready);
    assert_eq!(asset.original_path, wav.display().to_string());

    // Library copy must still be playable/readable.
    let dest = library.asset_abspath(&asset.library_relpath);
    assert!(dest.exists());
    assert_eq!(hash_file(&dest).unwrap(), asset.hash);
}

#[test]
fn test_10_crash_leaves_recoverable_state_never_ready_without_file() {
    let tmp = tempfile::tempdir().unwrap();
    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let wav = src_dir.join("crash.wav");
    write_min_wav(&wav, 48000, 2, 16, 4800);

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);

    // Simulate a crash mid-copy: job + import_file row exist with status
    // `copying`, but no `ready` asset and no verified library file.
    let job_id = RowId::new();
    {
        let repo = soundhub_core::db::Repo::new(&conn);
        let tx = conn.unchecked_transaction().unwrap();
        repo.insert_import_job(
            &tx,
            &soundhub_core::ImportJob {
                id: job_id.clone(),
                created_at: chrono::Utc::now(),
                source: wav.display().to_string(),
                total_files: 1,
                processed_files: 0,
                success_count: 0,
                failed_count: 0,
                skipped_count: 0,
                status: soundhub_core::ImportJobStatus::Copying,
            },
        )
        .unwrap();
        repo.insert_import_file(
            &tx,
            &soundhub_core::ImportFileRecord {
                id: RowId::new(),
                job_id: job_id.clone(),
                source_path: wav.display().to_string(),
                asset_id: None,
                status: soundhub_core::ImportFileStatus::Copying,
                error: None,
            },
        )
        .unwrap();
        tx.commit().unwrap();
    }

    // Startup recovery must surface the incomplete job.
    let report = library.inspect_recovery(&conn).unwrap();
    assert!(
        report.incomplete_jobs.iter().any(|j| j.id == job_id),
        "expected the simulated crash job to be listed"
    );

    // Resume the stuck job: source file still exists and completes.
    let pipeline = ImportPipeline::new(&library, &conn);
    let resumed = pipeline.resume_incomplete(&job_id).unwrap();
    assert!(
        resumed.success + resumed.failed + resumed.duplicate > 0,
        "resume should process the stuck file: {resumed:?}"
    );
    assert_eq!(resumed.success, 1, "{resumed:?}");

    let job = soundhub_core::db::Repo::new(&conn)
        .get_import_job(&job_id)
        .unwrap()
        .unwrap();
    assert_eq!(job.status, soundhub_core::ImportJobStatus::Completed);

    // The recovered asset must be ready and fully verified on disk.
    let files = soundhub_core::db::Repo::new(&conn)
        .list_import_files(&job_id)
        .unwrap();
    assert_eq!(files[0].status, soundhub_core::ImportFileStatus::Ready);
    let aid = AssetId::parse(files[0].asset_id.as_ref().unwrap()).unwrap();
    let asset = soundhub_core::db::Repo::new(&conn)
        .get_asset(&aid)
        .unwrap()
        .unwrap();
    assert_eq!(asset.status, AssetStatus::Ready);
    let dest = library.asset_abspath(&asset.library_relpath);
    assert!(dest.exists());
    assert_eq!(hash_file(&dest).unwrap(), asset.hash);
}

#[test]
fn cleanup_incomplete_discards_partial_but_keeps_ready() {
    let tmp = tempfile::tempdir().unwrap();
    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let good = src_dir.join("good.wav");
    let stuck = src_dir.join("stuck.wav");
    write_min_wav(&good, 48000, 2, 16, 4800);
    write_min_wav(&stuck, 48000, 2, 16, 4800);

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let pipeline = ImportPipeline::new(&library, &conn);

    // Fully import one asset first — this must survive cleanup.
    let ok = pipeline
        .import_paths(std::slice::from_ref(&good), DuplicateAction::Skip)
        .unwrap();
    assert_eq!(ok.success, 1);
    let ready_id = AssetId::parse(ok.files[0].asset_id.as_ref().unwrap()).unwrap();

    // Simulate a crash mid-copy on a second file.
    let job_id = RowId::new();
    {
        let repo = soundhub_core::db::Repo::new(&conn);
        let tx = conn.unchecked_transaction().unwrap();
        repo.insert_import_job(
            &tx,
            &soundhub_core::ImportJob {
                id: job_id.clone(),
                created_at: chrono::Utc::now(),
                source: stuck.display().to_string(),
                total_files: 1,
                processed_files: 0,
                success_count: 0,
                failed_count: 0,
                skipped_count: 0,
                status: soundhub_core::ImportJobStatus::Copying,
            },
        )
        .unwrap();
        repo.insert_import_file(
            &tx,
            &soundhub_core::ImportFileRecord {
                id: RowId::new(),
                job_id: job_id.clone(),
                source_path: stuck.display().to_string(),
                asset_id: None,
                status: soundhub_core::ImportFileStatus::Copying,
                error: None,
            },
        )
        .unwrap();
        tx.commit().unwrap();
    }

    let removed = pipeline.cleanup_incomplete(&job_id).unwrap();
    assert_eq!(removed, 0, "no asset row existed yet");

    let job = soundhub_core::db::Repo::new(&conn)
        .get_import_job(&job_id)
        .unwrap()
        .unwrap();
    assert_eq!(job.status, soundhub_core::ImportJobStatus::Cancelled);

    // Ready asset untouched.
    let still = soundhub_core::db::Repo::new(&conn)
        .get_asset(&ready_id)
        .unwrap()
        .unwrap();
    assert_eq!(still.status, AssetStatus::Ready);
    assert!(library.asset_abspath(&still.library_relpath).exists());

    // Cancelled job no longer shows as incomplete.
    let incomplete = soundhub_core::db::Repo::new(&conn)
        .list_incomplete_import_jobs()
        .unwrap();
    assert!(incomplete.iter().all(|j| j.id != job_id));
}

#[test]
fn retry_failed_reimports_only_failed_files() {
    let tmp = tempfile::tempdir().unwrap();
    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let wav = src_dir.join("retry_me.wav");
    write_min_wav(&wav, 44100, 1, 16, 2400);

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);

    // Job with one failed file (source is valid — a prior attempt failed).
    let job_id = RowId::new();
    {
        let repo = soundhub_core::db::Repo::new(&conn);
        let tx = conn.unchecked_transaction().unwrap();
        repo.insert_import_job(
            &tx,
            &soundhub_core::ImportJob {
                id: job_id.clone(),
                created_at: chrono::Utc::now(),
                source: wav.display().to_string(),
                total_files: 1,
                processed_files: 1,
                success_count: 0,
                failed_count: 1,
                skipped_count: 0,
                status: soundhub_core::ImportJobStatus::Failed,
            },
        )
        .unwrap();
        repo.insert_import_file(
            &tx,
            &soundhub_core::ImportFileRecord {
                id: RowId::new(),
                job_id: job_id.clone(),
                source_path: wav.display().to_string(),
                asset_id: None,
                status: soundhub_core::ImportFileStatus::Failed,
                error: Some("simulated failure".into()),
            },
        )
        .unwrap();
        // Force job back to non-terminal so recovery sees it.
        repo.set_import_job_status(&tx, &job_id, soundhub_core::ImportJobStatus::Copying)
            .unwrap();
        tx.commit().unwrap();
    }

    let pipeline = ImportPipeline::new(&library, &conn);
    let r = pipeline.retry_failed(&job_id).unwrap();
    assert_eq!(r.success, 1, "{r:?}");
    assert_eq!(r.failed, 0, "{r:?}");

    let files = soundhub_core::db::Repo::new(&conn)
        .list_import_files(&job_id)
        .unwrap();
    assert_eq!(files[0].status, soundhub_core::ImportFileStatus::Ready);
    assert!(files[0].asset_id.is_some());
}

#[test]
fn import_reports_progress_per_file() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    let tmp = tempfile::tempdir().unwrap();
    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    write_min_wav(&src_dir.join("a.wav"), 48000, 1, 16, 100);
    write_min_wav(&src_dir.join("b.wav"), 48000, 1, 16, 100);
    write_min_wav(&src_dir.join("c.wav"), 48000, 1, 16, 100);

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let pipeline = ImportPipeline::new(&library, &conn);

    let calls = Arc::new(AtomicU32::new(0));
    let c2 = calls.clone();
    let result = pipeline
        .import_paths_with_progress(
            &[src_dir],
            DuplicateAction::Skip,
            &move |processed, total, _cur| {
                c2.fetch_add(1, Ordering::SeqCst);
                assert!(processed <= total);
                assert_eq!(total, 3);
            },
        )
        .unwrap();

    assert_eq!(result.success, 3);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "one progress tick per file"
    );
}

#[test]
fn asset_identity_is_id_not_filename() {
    let tmp = tempfile::tempdir().unwrap();
    let a_dir = tmp.path().join("devA");
    let b_dir = tmp.path().join("devB");
    std::fs::create_dir_all(&a_dir).unwrap();
    std::fs::create_dir_all(&b_dir).unwrap();
    // Same filename from two devices, different content.
    write_min_wav(&a_dir.join("0001.wav"), 48000, 2, 24, 100);
    write_min_wav(&b_dir.join("0001.wav"), 48000, 2, 24, 200);

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let pipeline = ImportPipeline::new(&library, &conn);
    let r1 = pipeline
        .import_paths(&[a_dir], DuplicateAction::Skip)
        .unwrap();
    let r2 = pipeline
        .import_paths(&[b_dir], DuplicateAction::Skip)
        .unwrap();
    assert_eq!(r1.success, 1);
    assert_eq!(r2.success, 1);

    let id1 = r1.files[0].asset_id.clone().unwrap();
    let id2 = r2.files[0].asset_id.clone().unwrap();
    assert_ne!(id1, id2, "same filename must not collide");
    // Physical paths must also differ.
    let a1 = soundhub_core::db::Repo::new(&conn)
        .get_asset(&AssetId::parse(&id1).unwrap())
        .unwrap()
        .unwrap();
    let a2 = soundhub_core::db::Repo::new(&conn)
        .get_asset(&AssetId::parse(&id2).unwrap())
        .unwrap()
        .unwrap();
    assert_ne!(a1.library_relpath, a2.library_relpath);
}

#[test]
fn collection_membership_does_not_copy_file() {
    use soundhub_core::db::Repo;

    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let wav = src.join("once.wav");
    write_min_wav(&wav, 48000, 2, 24, 4800);

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let pipeline = ImportPipeline::new(&library, &conn);
    let result = pipeline
        .import_paths(&[wav], DuplicateAction::Skip)
        .unwrap();
    let asset_id = AssetId::parse(result.files[0].asset_id.as_ref().unwrap()).unwrap();

    let repo = Repo::new(&conn);
    let c1 = repo.create_collection("Jeju", None).unwrap();
    let c2 = repo.create_collection("Ocean", None).unwrap();
    let c3 = repo.create_collection("Travel", None).unwrap();
    repo.add_asset_collection(&asset_id, &c1.id).unwrap();
    repo.add_asset_collection(&asset_id, &c2.id).unwrap();
    repo.add_asset_collection(&asset_id, &c3.id).unwrap();

    // One physical file only.
    let asset = repo.get_asset(&asset_id).unwrap().unwrap();
    let dest = library.asset_abspath(&asset.library_relpath);
    assert!(dest.exists());
    let mut count = 0;
    for entry in walkdir::WalkDir::new(library.root.join("assets")) {
        if entry.unwrap().file_type().is_file() {
            count += 1;
        }
    }
    assert_eq!(count, 1, "collections must not duplicate files");

    // Removing from a collection must not delete the asset.
    repo.remove_asset_from_collection(&asset_id, &c1.id)
        .unwrap();
    assert!(dest.exists());
    assert!(repo.get_asset(&asset_id).unwrap().is_some());
}

#[test]
fn import_file_record_lifecycle_one_row_per_file() {
    use soundhub_core::db::Repo;
    use soundhub_core::{ImportFileStatus, ImportJobStatus};

    let tmp = tempfile::tempdir().unwrap();
    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    write_min_wav(&src_dir.join("a.wav"), 48000, 1, 16, 100);
    write_min_wav(&src_dir.join("b.wav"), 48000, 1, 16, 200);
    // Unsupported extension gets its own skip record.
    std::fs::write(src_dir.join("notes.txt"), b"not audio").unwrap();

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let pipeline = ImportPipeline::new(&library, &conn);
    let result = pipeline
        .import_paths(&[src_dir], DuplicateAction::Skip)
        .unwrap();
    assert_eq!(result.success, 2, "{:?}", result.files);
    assert_eq!(result.unsupported, 1, "{:?}", result.files);

    let repo = Repo::new(&conn);
    let job_id = RowId::parse(&result.job_id).unwrap();
    let files = repo.list_import_files(&job_id).unwrap();
    assert_eq!(
        files.len(),
        3,
        "exactly one record per scanned file: {files:?}"
    );

    let mut ready = 0;
    let mut skipped = 0;
    for f in &files {
        match f.status {
            ImportFileStatus::Ready => {
                ready += 1;
                assert!(f.asset_id.is_some(), "ready record must carry asset_id");
            }
            ImportFileStatus::SkippedUnsupported => skipped += 1,
            other => panic!("unexpected terminal status {other:?} for {f:?}"),
        }
    }
    assert_eq!(ready, 2);
    assert_eq!(skipped, 1);

    // Resume of a fully terminal job must not re-import anything.
    let before = repo.count_ready_assets().unwrap();
    let resumed = pipeline.resume_incomplete(&job_id).unwrap();
    assert_eq!(resumed.success, 2, "{resumed:?}");
    assert_eq!(resumed.unsupported, 1, "{resumed:?}");
    assert_eq!(resumed.failed, 0, "{resumed:?}");
    assert_eq!(
        repo.count_ready_assets().unwrap(),
        before,
        "no double import"
    );
    assert_eq!(repo.list_import_files(&job_id).unwrap().len(), 3);

    let job = repo.get_import_job(&job_id).unwrap().unwrap();
    assert_eq!(job.status, ImportJobStatus::Completed);
    assert_eq!(job.success_count, 2);
    assert_eq!(job.skipped_count, 1);
    assert_eq!(job.failed_count, 0);
    assert_eq!(job.processed_files, 3);
}

#[test]
fn resume_does_not_reimport_already_ready_files() {
    use soundhub_core::db::Repo;
    use soundhub_core::{ImportFileStatus, ImportJob, ImportJobStatus};

    let tmp = tempfile::tempdir().unwrap();
    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let done = src_dir.join("done.wav");
    let stuck = src_dir.join("stuck.wav");
    write_min_wav(&done, 48000, 2, 16, 4800);
    write_min_wav(&stuck, 48000, 2, 16, 9600);

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let pipeline = ImportPipeline::new(&library, &conn);

    // Fully import the first file via the normal path.
    let ok = pipeline
        .import_paths(std::slice::from_ref(&done), DuplicateAction::Skip)
        .unwrap();
    assert_eq!(ok.success, 1);
    let done_asset = ok.files[0].asset_id.clone().unwrap();

    // Build a job that has: one already-ready record + one stuck copying record.
    let job_id = RowId::new();
    let ready_rec = RowId::new();
    let stuck_rec = RowId::new();
    {
        let repo = Repo::new(&conn);
        let tx = conn.unchecked_transaction().unwrap();
        repo.insert_import_job(
            &tx,
            &ImportJob {
                id: job_id.clone(),
                created_at: chrono::Utc::now(),
                source: "mixed".into(),
                total_files: 2,
                processed_files: 1,
                success_count: 1,
                failed_count: 0,
                skipped_count: 0,
                status: ImportJobStatus::Copying,
            },
        )
        .unwrap();
        // The done file's record is already terminal — resume must leave it alone.
        repo.insert_import_file(
            &tx,
            &soundhub_core::ImportFileRecord {
                id: ready_rec.clone(),
                job_id: job_id.clone(),
                source_path: done.display().to_string(),
                asset_id: Some(done_asset.clone()),
                status: ImportFileStatus::Ready,
                error: None,
            },
        )
        .unwrap();
        repo.insert_import_file(
            &tx,
            &soundhub_core::ImportFileRecord {
                id: stuck_rec.clone(),
                job_id: job_id.clone(),
                source_path: stuck.display().to_string(),
                asset_id: None,
                status: ImportFileStatus::Copying,
                error: None,
            },
        )
        .unwrap();
        tx.commit().unwrap();
    }

    let assets_before = Repo::new(&conn).count_ready_assets().unwrap();
    let resumed = pipeline.resume_incomplete(&job_id).unwrap();
    assert_eq!(resumed.success, 2, "{resumed:?}"); // 1 already-done + 1 recovered
    assert_eq!(resumed.failed, 0, "{resumed:?}");
    assert_eq!(
        Repo::new(&conn).count_ready_assets().unwrap(),
        assets_before + 1,
        "only the stuck file is newly imported"
    );

    // Both records still exist and are Ready; the done file kept its asset_id.
    let files = Repo::new(&conn).list_import_files(&job_id).unwrap();
    assert_eq!(files.len(), 2, "resume must not add rows: {files:?}");
    let done_row = files.iter().find(|f| f.id == ready_rec).unwrap();
    assert_eq!(done_row.status, ImportFileStatus::Ready);
    assert_eq!(done_row.asset_id.as_deref(), Some(done_asset.as_str()));
    let stuck_row = files.iter().find(|f| f.id == stuck_rec).unwrap();
    assert_eq!(stuck_row.status, ImportFileStatus::Ready);
    assert!(stuck_row.asset_id.is_some());
    assert_ne!(stuck_row.asset_id, done_row.asset_id);

    let job = Repo::new(&conn).get_import_job(&job_id).unwrap().unwrap();
    assert_eq!(job.status, ImportJobStatus::Completed);
    assert_eq!(job.success_count, 2);
    assert_eq!(job.processed_files, 2);
}

#[test]
fn unicode_filenames_import_search_and_reopen() {
    use soundhub_core::search::{search, SearchQuery};

    let tmp = tempfile::tempdir().unwrap();
    let src_dir = tmp.path().join("录音 素材");
    std::fs::create_dir_all(&src_dir).unwrap();
    // CJK, Japanese, spaces, hyphens, long name.
    let names = [
        "采访 01.wav",
        "山田太郎 01.wav",
        "Scene 12 - Exterior.wav",
        "über-mäßige Aufnahme (2024).wav",
    ];
    for n in &names {
        write_min_wav(&src_dir.join(n), 48000, 1, 16, 50);
    }

    let lib_dir = tmp.path().join("Library 目录");
    let (library, conn) = setup_library(&lib_dir);
    let pipeline = ImportPipeline::new(&library, &conn);
    let result = pipeline
        .import_paths(&[src_dir], DuplicateAction::Skip)
        .unwrap();
    assert_eq!(result.success, 4, "{result:?}");

    let repo = Repo::new(&conn);
    let assets = repo.list_assets(10, 0).unwrap();
    assert_eq!(assets.len(), 4);
    for n in &names {
        assert!(
            assets.iter().any(|a| &a.filename == n),
            "filename preserved: {n}"
        );
    }

    // Search by a CJK token from filename.
    let hits = search(&conn, &SearchQuery::text("采访")).unwrap();
    assert_eq!(hits.len(), 1);

    // Library copy exists at the resolved path (Unicode source path is fine).
    for a in &assets {
        let abs = library.asset_abspath(&a.library_relpath);
        assert!(abs.exists(), "missing library copy for {}", abs.display());
    }
}

#[test]
fn import_failure_marks_record_failed() {
    use soundhub_core::db::Repo;
    use soundhub_core::ImportFileStatus;

    let tmp = tempfile::tempdir().unwrap();
    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let wav = src_dir.join("ghost.wav");
    write_min_wav(&wav, 48000, 1, 16, 100);

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let repo = Repo::new(&conn);

    // A Pending record whose source vanishes before import_one runs.
    let job_id = RowId::new();
    let rec_id = RowId::new();
    {
        let tx = conn.unchecked_transaction().unwrap();
        repo.insert_import_job(
            &tx,
            &soundhub_core::ImportJob {
                id: job_id.clone(),
                created_at: chrono::Utc::now(),
                source: wav.display().to_string(),
                total_files: 1,
                processed_files: 0,
                success_count: 0,
                failed_count: 0,
                skipped_count: 0,
                status: soundhub_core::ImportJobStatus::Copying,
            },
        )
        .unwrap();
        repo.insert_import_file(
            &tx,
            &soundhub_core::ImportFileRecord {
                id: rec_id.clone(),
                job_id: job_id.clone(),
                source_path: wav.display().to_string(),
                asset_id: None,
                status: ImportFileStatus::Pending,
                error: None,
            },
        )
        .unwrap();
        tx.commit().unwrap();
    }

    // import_one must mark the record Failed on early exit (source gone).
    std::fs::remove_file(&wav).unwrap();
    let pipeline = ImportPipeline::new(&library, &conn);
    let err = pipeline.import_one(&rec_id, &wav, &None).unwrap_err();
    assert!(!err.to_string().is_empty());

    let files = repo.list_import_files(&job_id).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].status, ImportFileStatus::Failed);
    assert!(files[0].error.is_some(), "failure reason must be recorded");
}

#[test]
fn resume_deletes_stale_asset_rows_for_missing_source() {
    use soundhub_core::db::Repo;
    use soundhub_core::models::Asset;
    use soundhub_core::{ImportFileStatus, ImportJobStatus};

    let tmp = tempfile::tempdir().unwrap();
    let src_dir = tmp.path().join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    let gone = src_dir.join("gone.wav");
    write_min_wav(&gone, 48000, 1, 16, 100);

    let lib_dir = tmp.path().join("Library");
    let (library, conn) = setup_library(&lib_dir);
    let repo = Repo::new(&conn);

    // Simulate a crash mid-copy: non-ready asset + half-written dest + Pending
    // record, then the source disappears.
    let job_id = RowId::new();
    let rec_id = RowId::new();
    let asset_id = AssetId::parse("sh_77777777777777777777777777").unwrap();
    {
        let asset = Asset {
            id: asset_id.clone(),
            filename: "gone.wav".into(),
            extension: Some("wav".into()),
            file_size: 10,
            hash: "gonehash".into(),
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
            original_path: gone.display().to_string(),
            source_volume: None,
            library_relpath: "assets/ab/gone.wav".into(),
            status: AssetStatus::Copying,
            parse_errors: vec![],
            raw_metadata: Default::default(),
            metadata: vec![],
            deleted_at: None,
        };
        let tx = conn.unchecked_transaction().unwrap();
        repo.insert_import_job(
            &tx,
            &soundhub_core::ImportJob {
                id: job_id.clone(),
                created_at: chrono::Utc::now(),
                source: src_dir.display().to_string(),
                total_files: 1,
                processed_files: 0,
                success_count: 0,
                failed_count: 0,
                skipped_count: 0,
                status: ImportJobStatus::Copying,
            },
        )
        .unwrap();
        repo.insert_import_file(
            &tx,
            &soundhub_core::ImportFileRecord {
                id: rec_id.clone(),
                job_id: job_id.clone(),
                source_path: gone.display().to_string(),
                asset_id: Some(asset_id.to_string()),
                status: ImportFileStatus::Copying,
                error: None,
            },
        )
        .unwrap();
        repo.insert_pending_asset(&tx, &asset).unwrap();
        tx.commit().unwrap();

        let dest = library.asset_abspath(&asset.library_relpath);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, b"partial").unwrap();
    }
    // Reindex so the stale asset has an FTS row to prove cleanup.
    repo.reindex_asset(&asset_id).unwrap();

    std::fs::remove_file(&gone).unwrap();
    let pipeline = ImportPipeline::new(&library, &conn);
    let resumed = pipeline.resume_incomplete(&job_id).unwrap();
    assert_eq!(resumed.failed, 1, "{resumed:?}");

    // Asset row + FTS + half-written dest are gone; record is terminal Failed.
    assert!(repo.get_asset(&asset_id).unwrap().is_none());
    let fts: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM assets_fts WHERE asset_id = ?1",
            [asset_id.as_str()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(fts, 0, "stale FTS row must be deleted");
    let dest = library.asset_abspath("assets/ab/gone.wav");
    assert!(!dest.exists(), "half-written dest must be removed");

    let files = repo.list_import_files(&job_id).unwrap();
    assert_eq!(files[0].status, ImportFileStatus::Failed);
}
