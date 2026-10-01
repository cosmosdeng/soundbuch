use chrono::Utc;
use std::path::{Path, PathBuf};

use crate::db::Repo;
use crate::error::{Error, Result};
use crate::ids::{AssetId, RowId};
use crate::import::hasher::{copy_and_verify, hash_file};
use crate::import::scanner::{annotate_duplicates, is_supported_extension, scan_paths};
use crate::library::Library;
use crate::metadata;
use crate::models::*;

/// Transactional import pipeline.
///
/// Ordering (PRD §10 / §11):
///   1. create pending asset
///   2. copy
///   3. verify hash
///   4. write metadata
///   5. mark asset ready
///
/// A crash at any step leaves a recoverable `pending`/`failed` row — never
/// a `ready` asset with a missing or corrupt file.
///
/// Each scanned file owns exactly one `import_files` row. That row is updated
/// in place through its lifecycle (pending → copying → ready/failed/skipped);
/// nothing ever inserts a second row for the same (job_id, source_path).
pub struct ImportPipeline<'a> {
    pub library: &'a Library,
    pub conn: &'a rusqlite::Connection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum ImportOutcome {
    Imported,
    SkippedDuplicate,
    SkippedUnsupported,
    Failed,
    Cancelled,
}

#[derive(Debug, serde::Serialize)]
pub struct FileImportResult {
    pub source_path: String,
    pub outcome: ImportOutcome,
    pub asset_id: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct ImportJobResult {
    pub job_id: String,
    pub total: u32,
    pub success: u32,
    pub duplicate: u32,
    pub unsupported: u32,
    pub failed: u32,
    pub cancelled: bool,
    pub files: Vec<FileImportResult>,
}

/// Progress callback: (processed_files, total_files, current_source_path).
pub type ProgressFn<'a> = dyn Fn(u32, u32, &str) + 'a;

impl<'a> ImportPipeline<'a> {
    pub fn new(library: &'a Library, conn: &'a rusqlite::Connection) -> Self {
        Self { library, conn }
    }

    /// Scan paths, then import every supported file.
    pub fn import_paths(
        &self,
        paths: &[PathBuf],
        duplicate_action: DuplicateAction,
    ) -> Result<ImportJobResult> {
        self.import_paths_with_progress(paths, duplicate_action, &|_, _, _| {})
    }

    /// Same as `import_paths` but reports progress after each file.
    pub fn import_paths_with_progress(
        &self,
        paths: &[PathBuf],
        duplicate_action: DuplicateAction,
        on_progress: &ProgressFn<'_>,
    ) -> Result<ImportJobResult> {
        let mut summary = scan_paths(paths).map_err(|e| Error::other(e.to_string()))?;
        {
            let repo = Repo::new(self.conn);
            annotate_duplicates(&mut summary, &|hash| {
                repo.find_by_hash(hash).ok().flatten().map(|id| id.to_string())
            });
        }

        let source = paths
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(";");
        self.import_scanned_with_progress(summary, source, duplicate_action, on_progress)
    }

    /// Import an already-scanned batch (user confirmed after seeing the summary).
    pub fn import_scanned(
        &self,
        summary: ScanSummary,
        source: String,
        duplicate_action: DuplicateAction,
    ) -> Result<ImportJobResult> {
        self.import_scanned_with_progress(summary, source, duplicate_action, &|_, _, _| {})
    }

    pub fn import_scanned_with_progress(
        &self,
        summary: ScanSummary,
        source: String,
        duplicate_action: DuplicateAction,
        on_progress: &ProgressFn<'_>,
    ) -> Result<ImportJobResult> {
        let job_id = RowId::new();
        let now = Utc::now();
        let mut job = ImportJob {
            id: job_id.clone(),
            created_at: now,
            source,
            total_files: summary.files.len() as u32,
            processed_files: 0,
            success_count: 0,
            failed_count: 0,
            skipped_count: 0,
            status: ImportJobStatus::Queued,
        };

        // One Pending record per scanned file. import_one updates these in
        // place — it never inserts a second row for the same source_path.
        let mut record_ids: Vec<RowId> = Vec::with_capacity(summary.files.len());
        let repo = Repo::new(self.conn);
        {
            let tx = self.conn.unchecked_transaction()?;
            repo.insert_import_job(&tx, &job)?;
            for f in &summary.files {
                let rec_id = RowId::new();
                let rec = ImportFileRecord {
                    id: rec_id.clone(),
                    job_id: job_id.clone(),
                    source_path: f.path.clone(),
                    asset_id: None,
                    status: ImportFileStatus::Pending,
                    error: f.error.clone(),
                };
                repo.insert_import_file(&tx, &rec)?;
                record_ids.push(rec_id);
            }
            tx.commit()?;
        }

        let mut results = Vec::new();
        let mut cancelled = false;

        for (rec_id, f) in record_ids.iter().zip(summary.files.iter()) {
            if cancelled {
                results.push(FileImportResult {
                    source_path: f.path.clone(),
                    outcome: ImportOutcome::Cancelled,
                    asset_id: None,
                    error: Some("import cancelled".into()),
                });
                continue;
            }

            // Duplicate policy.
            if f.duplicate_of.is_some() {
                match duplicate_action {
                    DuplicateAction::Skip => {
                        job.skipped_count += 1;
                        job.processed_files += 1;
                        results.push(FileImportResult {
                            source_path: f.path.clone(),
                            outcome: ImportOutcome::SkippedDuplicate,
                            asset_id: f.duplicate_of.clone(),
                            error: None,
                        });
                        {
                            let tx = self.conn.unchecked_transaction()?;
                            repo.update_import_file(
                                &tx,
                                rec_id,
                                ImportFileStatus::SkippedDuplicate,
                                f.duplicate_of.as_deref(),
                                None,
                            )?;
                            tx.commit()?;
                        }
                        self.persist_progress(&mut job)?;
                        on_progress(job.processed_files, job.total_files, &f.path);
                        continue;
                    }
                    DuplicateAction::Cancel => {
                        cancelled = true;
                        results.push(FileImportResult {
                            source_path: f.path.clone(),
                            outcome: ImportOutcome::Cancelled,
                            asset_id: None,
                            error: Some("cancelled on duplicate".into()),
                        });
                        continue;
                    }
                    DuplicateAction::ImportAsDuplicate => {}
                }
            }

            if !f.supported {
                job.skipped_count += 1;
                job.processed_files += 1;
                results.push(FileImportResult {
                    source_path: f.path.clone(),
                    outcome: ImportOutcome::SkippedUnsupported,
                    asset_id: None,
                    error: f.error.clone(),
                });
                {
                    let tx = self.conn.unchecked_transaction()?;
                    repo.update_import_file(
                        &tx,
                        rec_id,
                        ImportFileStatus::SkippedUnsupported,
                        None,
                        f.error.as_deref(),
                    )?;
                    tx.commit()?;
                }
                self.persist_progress(&mut job)?;
                on_progress(job.processed_files, job.total_files, &f.path);
                continue;
            }

            match self.import_one(rec_id, Path::new(&f.path), &f.hash) {
                Ok(asset_id) => {
                    job.success_count += 1;
                    job.processed_files += 1;
                    results.push(FileImportResult {
                        source_path: f.path.clone(),
                        outcome: ImportOutcome::Imported,
                        asset_id: Some(asset_id),
                        error: None,
                    });
                }
                Err(Error::Cancelled) => {
                    cancelled = true;
                    results.push(FileImportResult {
                        source_path: f.path.clone(),
                        outcome: ImportOutcome::Cancelled,
                        asset_id: None,
                        error: Some("cancelled".into()),
                    });
                }
                Err(e) => {
                    // One file failing must NOT abort the whole job (PRD §38).
                    job.failed_count += 1;
                    job.processed_files += 1;
                    results.push(FileImportResult {
                        source_path: f.path.clone(),
                        outcome: ImportOutcome::Failed,
                        asset_id: None,
                        error: Some(e.to_string()),
                    });
                }
            }
            self.persist_progress(&mut job)?;
            on_progress(job.processed_files, job.total_files, &f.path);
        }

        job.status = if cancelled {
            ImportJobStatus::Cancelled
        } else if job.failed_count > 0 && job.success_count == 0 {
            ImportJobStatus::Failed
        } else {
            ImportJobStatus::Completed
        };
        self.persist_progress(&mut job)?;

        Ok(ImportJobResult {
            job_id: job_id.to_string(),
            total: job.total_files,
            success: job.success_count,
            duplicate: job.skipped_count
                - results
                    .iter()
                    .filter(|r| r.outcome == ImportOutcome::SkippedUnsupported)
                    .count() as u32,
            unsupported: results
                .iter()
                .filter(|r| r.outcome == ImportOutcome::SkippedUnsupported)
                .count() as u32,
            failed: job.failed_count,
            cancelled,
            files: results,
        })
    }

    /// Import a single source file with full transactional safety.
    ///
    /// `file_record_id` must already exist (created as `Pending` at scan time).
    /// This method updates that row through Copying → Ready/Failed and never
    /// inserts a new `import_files` row.
    pub fn import_one(
        &self,
        file_record_id: &RowId,
        source: &Path,
        known_hash: &Option<String>,
    ) -> Result<String> {
        let repo = Repo::new(self.conn);
        let source_path = source.display().to_string();

        if !source.is_file() {
            let e = Error::other(format!("not a file: {source_path}"));
            self.mark_import_file_failed(file_record_id, &e.to_string())?;
            return Err(e);
        }

        let ext = source
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase());
        if !ext.as_deref().map(is_supported_extension).unwrap_or(false) {
            let e = Error::UnsupportedFormat(source_path);
            self.mark_import_file_failed(file_record_id, &e.to_string())?;
            return Err(e);
        }

        let file_meta = std::fs::metadata(source)?;
        let hash = match known_hash {
            Some(h) => h.clone(),
            None => hash_file(source)?,
        };

        let asset_id = AssetId::new();
        let relpath = self
            .library
            .asset_relpath(&asset_id, ext.as_deref());
        let abs_dest = self.library.prepare_asset_dir(&relpath)?;

        // Extract metadata. Parse failures are recorded, never block import.
        let (audio, gps, raw, parse_errors) = metadata::extract(source);
        let fs_meta = metadata::filesystem_meta(source, &file_meta);

        let mut asset = Asset {
            id: asset_id.clone(),
            filename: fs_meta.filename.clone(),
            extension: fs_meta.extension.clone(),
            file_size: fs_meta.file_size,
            hash: hash.clone(),
            mime_type: ext.as_deref().and_then(metadata::mime_for_extension),
            duration_ms: audio.duration_ms,
            sample_rate: audio.sample_rate,
            bit_depth: audio.bit_depth,
            channels: audio.channels,
            channel_layout: audio.channel_layout.clone(),
            codec: audio.codec.clone(),
            container: audio.container.clone(),
            bitrate: audio.bitrate,
            compression: audio.compression.clone(),
            recorded_at: None,
            recording_start: None,
            recording_end: None,
            timezone: None,
            utc_offset_minutes: None,
            file_created_at: fs_meta.created_at,
            file_modified_at: fs_meta.modified_at,
            imported_at: Utc::now(),
            latitude: gps.latitude,
            longitude: gps.longitude,
            altitude: gps.altitude,
            gps_accuracy: gps.accuracy,
            original_path: fs_meta.original_path.clone(),
            source_volume: fs_meta.source_volume.clone(),
            library_relpath: relpath.clone(),
            status: AssetStatus::Pending,
            parse_errors,
            raw_metadata: raw,
            metadata: metadata::build_metadata_values(&fs_meta, &audio, &gps),
            deleted_at: None,
        };

        // 1. Create pending asset and claim the import-file record (one txn).
        {
            let tx = self.conn.unchecked_transaction()?;
            repo.insert_pending_asset(&tx, &asset)?;
            repo.update_import_file(
                &tx,
                file_record_id,
                ImportFileStatus::Copying,
                Some(asset_id.as_str()),
                None,
            )?;
            tx.commit()?;
        }

        // 2–3. Copy + verify. Only then flip to verifying/ready.
        {
            let tx = self.conn.unchecked_transaction()?;
            repo.set_asset_status(&tx, &asset_id, AssetStatus::Copying)?;
            tx.commit()?;
        }

        match copy_and_verify(source, &abs_dest, &hash) {
            Ok(_actual) => {
                // 4–5. Metadata already written; mark ready + index in one txn.
                asset.status = AssetStatus::Ready;
                let tx = self.conn.unchecked_transaction()?;
                repo.set_asset_status(&tx, &asset_id, AssetStatus::Ready)?;
                repo.reindex_asset_in(&tx, &asset_id)?;
                repo.update_import_file(
                    &tx,
                    file_record_id,
                    ImportFileStatus::Ready,
                    Some(asset_id.as_str()),
                    None,
                )?;
                tx.commit()?;
                Ok(asset_id.to_string())
            }
            Err(e) => {
                // Never leave a half-written dest or a ready-looking asset.
                if abs_dest.exists() {
                    let _ = std::fs::remove_file(&abs_dest);
                }
                let tx = self.conn.unchecked_transaction()?;
                repo.set_asset_status(&tx, &asset_id, AssetStatus::Failed)?;
                repo.update_import_file(
                    &tx,
                    file_record_id,
                    ImportFileStatus::Failed,
                    Some(asset_id.as_str()),
                    Some(&e.to_string()),
                )?;
                tx.commit()?;
                Err(e)
            }
        }
    }

    /// Mark an import-file row terminal-failed (early import_one exits).
    fn mark_import_file_failed(&self, file_record_id: &RowId, error: &str) -> Result<()> {
        let repo = Repo::new(self.conn);
        let tx = self.conn.unchecked_transaction()?;
        repo.update_import_file(&tx, file_record_id, ImportFileStatus::Failed, None, Some(error))?;
        tx.commit()?;
        Ok(())
    }

    /// Resume work interrupted by a crash. Re-imports `pending`/`copying`/
    /// `verifying` files whose source still exists; marks the rest failed.
    /// Already-terminal records (ready / failed / skipped_*) are left alone.
    pub fn resume_incomplete(&self, job_id: &RowId) -> Result<ImportJobResult> {
        self.resume_incomplete_with_progress(job_id, &|_, _, _| {})
    }

    pub fn resume_incomplete_with_progress(
        &self,
        job_id: &RowId,
        on_progress: &ProgressFn<'_>,
    ) -> Result<ImportJobResult> {
        let repo = Repo::new(self.conn);
        let job = repo
            .get_import_job(job_id)?
            .ok_or_else(|| Error::ImportJobNotFound(job_id.to_string()))?;
        let files = repo.list_import_files(job_id)?;

        let mut results = Vec::new();
        let mut job = job;
        // Track attempted-this-run counts for live progress; final tallies are
        // recomputed from the file records so they can never drift.
        for rec in files {
            match rec.status {
                ImportFileStatus::Ready => {
                    results.push(FileImportResult {
                        source_path: rec.source_path.clone(),
                        outcome: ImportOutcome::Imported,
                        asset_id: rec.asset_id.clone(),
                        error: None,
                    });
                    continue;
                }
                ImportFileStatus::SkippedDuplicate => {
                    results.push(FileImportResult {
                        source_path: rec.source_path.clone(),
                        outcome: ImportOutcome::SkippedDuplicate,
                        asset_id: rec.asset_id.clone(),
                        error: None,
                    });
                    continue;
                }
                ImportFileStatus::SkippedUnsupported => {
                    results.push(FileImportResult {
                        source_path: rec.source_path.clone(),
                        outcome: ImportOutcome::SkippedUnsupported,
                        asset_id: None,
                        error: rec.error.clone(),
                    });
                    continue;
                }
                ImportFileStatus::Failed => {
                    // Terminal for resume; `retry_failed` is the explicit path.
                    results.push(FileImportResult {
                        source_path: rec.source_path.clone(),
                        outcome: ImportOutcome::Failed,
                        asset_id: rec.asset_id.clone(),
                        error: rec.error.clone(),
                    });
                    continue;
                }
                // Pending / Copying / Verifying — interrupted mid-flight.
                _ => {}
            }

            let path = PathBuf::from(&rec.source_path);
            if !path.exists() {
                job.failed_count += 1;
                job.processed_files += 1;
                let tx = self.conn.unchecked_transaction()?;
                repo.update_import_file(
                    &tx,
                    &rec.id,
                    ImportFileStatus::Failed,
                    None,
                    Some("source disappeared before resume"),
                )?;
                tx.commit()?;
                // Drop any stale non-ready asset row + FTS left by the
                // interrupted attempt (virtual table has no FK cascade).
                if let Some(aid) = &rec.asset_id {
                    if let Ok(id) = AssetId::parse(aid) {
                        if let Some(asset) = repo.get_asset(&id)? {
                            if asset.status != AssetStatus::Ready {
                                let dest = self.library.asset_abspath(&asset.library_relpath);
                                if dest.exists() {
                                    let _ = std::fs::remove_file(&dest);
                                }
                                repo.delete_asset(&id)?;
                            }
                        }
                    }
                }
                results.push(FileImportResult {
                    source_path: rec.source_path.clone(),
                    outcome: ImportOutcome::Failed,
                    asset_id: None,
                    error: Some("source disappeared before resume".into()),
                });
                continue;
            }

            // Clean any half-written dest from the interrupted run first.
            if let Some(aid) = &rec.asset_id {
                if let Ok(id) = AssetId::parse(aid) {
                    if let Some(asset) = repo.get_asset(&id)? {
                        let dest = self.library.asset_abspath(&asset.library_relpath);
                        if dest.exists() {
                            let _ = std::fs::remove_file(&dest);
                        }
                    }
                }
            }

            match self.import_one(&rec.id, &path, &None) {
                Ok(asset_id) => {
                    job.success_count += 1;
                    job.processed_files += 1;
                    results.push(FileImportResult {
                        source_path: rec.source_path.clone(),
                        outcome: ImportOutcome::Imported,
                        asset_id: Some(asset_id),
                        error: None,
                    });
                }
                Err(e) => {
                    job.failed_count += 1;
                    job.processed_files += 1;
                    results.push(FileImportResult {
                        source_path: rec.source_path.clone(),
                        outcome: ImportOutcome::Failed,
                        asset_id: None,
                        error: Some(e.to_string()),
                    });
                }
            }
            self.persist_progress(&mut job)?;
            on_progress(job.processed_files, job.total_files, &rec.source_path);
        }

        // Recompute tallies from the authoritative file records so the job
        // row can never disagree with import_files (e.g. after a partial run).
        self.finalize_job_from_files(&mut job)?;

        let unsupported = results
            .iter()
            .filter(|r| r.outcome == ImportOutcome::SkippedUnsupported)
            .count() as u32;
        let duplicate = results
            .iter()
            .filter(|r| r.outcome == ImportOutcome::SkippedDuplicate)
            .count() as u32;

        Ok(ImportJobResult {
            job_id: job_id.to_string(),
            total: job.total_files,
            success: job.success_count,
            duplicate,
            unsupported,
            failed: job.failed_count,
            cancelled: false,
            files: results,
        })
    }

    /// Derive job counters and terminal status from `import_files` rows.
    fn finalize_job_from_files(&self, job: &mut ImportJob) -> Result<()> {
        let repo = Repo::new(self.conn);
        let files = repo.list_import_files(&job.id)?;

        let mut success = 0u32;
        let mut failed = 0u32;
        let mut skipped = 0u32;
        let mut incomplete = 0u32;
        for f in &files {
            match f.status {
                ImportFileStatus::Ready => success += 1,
                ImportFileStatus::Failed => failed += 1,
                ImportFileStatus::SkippedDuplicate | ImportFileStatus::SkippedUnsupported => {
                    skipped += 1
                }
                _ => incomplete += 1,
            }
        }

        job.total_files = files.len() as u32;
        job.success_count = success;
        job.failed_count = failed;
        job.skipped_count = skipped;
        job.processed_files = success + failed + skipped;
        job.status = if incomplete > 0 {
            // Still work left (e.g. cancelled mid-run with pending rows).
            ImportJobStatus::Copying
        } else if success == 0 && failed > 0 {
            ImportJobStatus::Failed
        } else {
            ImportJobStatus::Completed
        };
        self.persist_progress(job)
    }

    fn persist_progress(&self, job: &mut ImportJob) -> Result<()> {
        let repo = Repo::new(self.conn);
        let tx = self.conn.unchecked_transaction()?;
        repo.update_import_job_progress(&tx, job)?;
        tx.commit()?;
        Ok(())
    }

    /// Re-attempt only the `failed` files in a job. Completed work is kept.
    pub fn retry_failed(&self, job_id: &RowId) -> Result<ImportJobResult> {
        self.retry_failed_with_progress(job_id, &|_, _, _| {})
    }

    pub fn retry_failed_with_progress(
        &self,
        job_id: &RowId,
        on_progress: &ProgressFn<'_>,
    ) -> Result<ImportJobResult> {
        let repo = Repo::new(self.conn);
        let n = repo.reset_failed_import_files(job_id)?;
        if n == 0 {
            let job = repo
                .get_import_job(job_id)?
                .ok_or_else(|| Error::ImportJobNotFound(job_id.to_string()))?;
            let files = repo.list_import_files(job_id)?;
            return Ok(build_result_from_files(&job, &files));
        }
        self.resume_incomplete_with_progress(job_id, on_progress)
    }

    /// Abandon an incomplete job: drop partial library copies and non-ready
    /// asset rows, leave `ready` assets untouched, mark the job cancelled.
    pub fn cleanup_incomplete(&self, job_id: &RowId) -> Result<u32> {
        let repo = Repo::new(self.conn);
        let job = repo
            .get_import_job(job_id)?
            .ok_or_else(|| Error::ImportJobNotFound(job_id.to_string()))?;
        let files = repo.list_import_files(job_id)?;

        let mut removed = 0u32;
        for rec in &files {
            if rec.status == ImportFileStatus::Ready
                || rec.status == ImportFileStatus::SkippedDuplicate
                || rec.status == ImportFileStatus::SkippedUnsupported
            {
                continue;
            }
            if let Some(aid) = &rec.asset_id {
                if let Ok(id) = AssetId::parse(aid) {
                    if let Some(asset) = repo.get_asset(&id)? {
                        if asset.status != AssetStatus::Ready {
                            let dest = self.library.asset_abspath(&asset.library_relpath);
                            if dest.exists() {
                                let _ = std::fs::remove_file(&dest);
                            }
                            repo.delete_asset(&id)?;
                            removed += 1;
                        }
                    }
                }
            }
            let tx = self.conn.unchecked_transaction()?;
            repo.update_import_file(
                &tx,
                &rec.id,
                ImportFileStatus::Failed,
                rec.asset_id.as_deref(),
                Some("cleaned up"),
            )?;
            tx.commit()?;
        }

        let mut job = job;
        job.status = ImportJobStatus::Cancelled;
        self.persist_progress(&mut job)?;
        Ok(removed)
    }
}

fn build_result_from_files(job: &ImportJob, files: &[ImportFileRecord]) -> ImportJobResult {
    let mut results = Vec::with_capacity(files.len());
    let mut success = 0u32;
    let mut failed = 0u32;
    let mut duplicate = 0u32;
    let mut unsupported = 0u32;
    for f in files {
        let (outcome, counted) = match f.status {
            ImportFileStatus::Ready => {
                success += 1;
                (ImportOutcome::Imported, ())
            }
            ImportFileStatus::Failed => {
                failed += 1;
                (ImportOutcome::Failed, ())
            }
            ImportFileStatus::SkippedDuplicate => {
                duplicate += 1;
                (ImportOutcome::SkippedDuplicate, ())
            }
            ImportFileStatus::SkippedUnsupported => {
                unsupported += 1;
                (ImportOutcome::SkippedUnsupported, ())
            }
            _ => (ImportOutcome::Cancelled, ()),
        };
        let _ = counted;
        results.push(FileImportResult {
            source_path: f.source_path.clone(),
            outcome,
            asset_id: f.asset_id.clone(),
            error: f.error.clone(),
        });
    }
    ImportJobResult {
        job_id: job.id.to_string(),
        total: files.len() as u32,
        success,
        duplicate,
        unsupported,
        failed,
        cancelled: false,
        files: results,
    }
}

/// Convenience: create a library, open DB, run import (used by tests & CLI).
pub fn import_into_new_library(
    library_root: &Path,
    sources: &[PathBuf],
    duplicate_action: DuplicateAction,
) -> Result<(Library, ImportJobResult)> {
    let library = Library::create(library_root)?;
    let conn = library.open_db()?;
    let pipeline = ImportPipeline::new(&library, &conn);
    let result = pipeline.import_paths(sources, duplicate_action)?;
    Ok((library, result))
}
