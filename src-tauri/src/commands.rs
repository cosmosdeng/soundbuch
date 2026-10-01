use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use soundhub_core::db::Repo;
use soundhub_core::import::{hash_file, ImportPipeline};
use soundhub_core::library::Library;
use soundhub_core::models::{AssetStatus, DuplicateAction};
use soundhub_core::search::{search, SearchQuery};
use soundhub_core::{AssetId, ImportJob, RowId};
use tauri::{AppHandle, Emitter, Manager, State};

pub struct AppState {
    pub inner: Mutex<Option<OpenLibrary>>,
}

pub struct OpenLibrary {
    pub library: Library,
    pub conn: rusqlite::Connection,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct LibraryInfo {
    pub root: String,
    pub asset_count: u64,
}

#[derive(Debug, Serialize)]
pub struct AssetSummary {
    pub id: String,
    pub filename: String,
    pub duration_ms: Option<u64>,
    pub sample_rate: Option<u32>,
    pub bit_depth: Option<u16>,
    pub channels: Option<u16>,
    pub recorded_at: Option<String>,
    pub imported_at: String,
    pub status: String,
    pub file_size: u64,
    pub hash: String,
    pub original_path: String,
    pub library_relpath: String,
}

#[derive(Debug, Serialize)]
pub struct AssetDetail {
    pub summary: AssetSummary,
    pub codec: Option<String>,
    pub container: Option<String>,
    pub bitrate: Option<u32>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub altitude: Option<f64>,
    pub timezone: Option<String>,
    pub parse_errors: Vec<String>,
    pub raw_metadata: serde_json::Value,
    pub tags: Vec<NamedRef>,
    pub people: Vec<NamedRef>,
    pub collections: Vec<NamedRef>,
}

#[derive(Debug, Serialize, Clone)]
pub struct NamedRef {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct ScanPreview {
    pub ready: u32,
    pub duplicate: u32,
    pub unsupported: u32,
    pub error: u32,
    pub files: Vec<ScannedPreview>,
}

#[derive(Debug, Serialize)]
pub struct ScannedPreview {
    pub path: String,
    pub size: u64,
    pub supported: bool,
    pub duplicate_of: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ImportResultDto {
    pub job_id: String,
    pub total: u32,
    pub success: u32,
    pub duplicate: u32,
    pub unsupported: u32,
    pub failed: u32,
    pub cancelled: bool,
}

fn with_open<T>(
    state: &State<'_, AppState>,
    f: impl FnOnce(&OpenLibrary) -> soundhub_core::Result<T>,
) -> Result<T, String> {
    let guard = state.inner.lock().map_err(|e| e.to_string())?;
    let open = guard.as_ref().ok_or("Library not open")?;
    f(open).map_err(|e| e.to_string())
}

/// Run blocking library work off the async runtime so the UI stays responsive.
/// Locks `AppState` for the duration of the work (import is the long writer).
async fn run_blocking<T, F>(app: AppHandle, f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&OpenLibrary, &AppHandle) -> soundhub_core::Result<T> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let guard = state.inner.lock().map_err(|e| e.to_string())?;
        let open = guard.as_ref().ok_or("Library not open")?;
        f(open, &app).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("background task failed: {e}"))?
}

fn parse_dup(action: &str) -> DuplicateAction {
    match action {
        "skip" => DuplicateAction::Skip,
        "import_as_duplicate" => DuplicateAction::ImportAsDuplicate,
        "cancel" => DuplicateAction::Cancel,
        _ => DuplicateAction::Skip,
    }
}

fn emit_progress(app: &AppHandle, kind: &str, processed: u32, total: u32, current: &str) {
    let _ = app.emit(
        "import-progress",
        serde_json::json!({
            "kind": kind,
            "processed": processed,
            "total": total,
            "current": current,
        }),
    );
}

// ── Library ─────────────────────────────────────────────────────────────────

#[tauri::command]
fn create_library(state: State<'_, AppState>, path: String) -> Result<LibraryInfo, String> {
    let library = Library::create(PathBuf::from(&path)).map_err(|e| e.to_string())?;
    let conn = library.open_db().map_err(|e| e.to_string())?;
    let count = Repo::new(&conn).count_ready_assets().unwrap_or(0);
    let info = LibraryInfo {
        root: library.root.display().to_string(),
        asset_count: count,
    };
    crate::library_setup::push_recent(&info.root, Some(count));
    *state.inner.lock().map_err(|e| e.to_string())? = Some(OpenLibrary { library, conn });
    Ok(info)
}

#[tauri::command]
fn open_library(state: State<'_, AppState>, path: String) -> Result<LibraryInfo, String> {
    let library = Library::open(PathBuf::from(&path)).map_err(|e| e.to_string())?;
    let conn = library.open_db().map_err(|e| e.to_string())?;
    let count = Repo::new(&conn).count_ready_assets().unwrap_or(0);
    let info = LibraryInfo {
        root: library.root.display().to_string(),
        asset_count: count,
    };
    crate::library_setup::push_recent(&info.root, Some(count));
    *state.inner.lock().map_err(|e| e.to_string())? = Some(OpenLibrary { library, conn });
    Ok(info)
}

#[tauri::command]
fn library_info(state: State<'_, AppState>) -> Result<Option<LibraryInfo>, String> {
    let guard = state.inner.lock().map_err(|e| e.to_string())?;
    Ok(guard.as_ref().map(|o| LibraryInfo {
        root: o.library.root.display().to_string(),
        asset_count: Repo::new(&o.conn).count_ready_assets().unwrap_or(0),
    }))
}

// ── Assets ──────────────────────────────────────────────────────────────────

#[tauri::command]
fn list_assets(
    state: State<'_, AppState>,
    limit: Option<u32>,
    offset: Option<u32>,
) -> Result<Vec<AssetSummary>, String> {
    with_open(&state, |o| {
        let assets = Repo::new(&o.conn).list_assets(limit.unwrap_or(100), offset.unwrap_or(0))?;
        Ok(assets.into_iter().map(asset_to_summary).collect())
    })
}

#[tauri::command]
fn get_asset(state: State<'_, AppState>, id: String) -> Result<AssetDetail, String> {
    with_open(&state, |o| {
        let aid = AssetId::parse(&id)?;
        let repo = Repo::new(&o.conn);
        let asset = repo
            .get_asset(&aid)?
            .ok_or_else(|| soundhub_core::Error::AssetNotFound(id.clone()))?;

        // Tags / people / collections via simple joins (id + name for removal).
        let tags = query_named_refs(&o.conn, "SELECT t.id, t.name FROM tags t JOIN asset_tags at ON at.tag_id = t.id WHERE at.asset_id = ?1 ORDER BY t.name", &id)?;
        let people = query_named_refs(&o.conn, "SELECT p.id, p.name FROM people p JOIN asset_people ap ON ap.person_id = p.id WHERE ap.asset_id = ?1 ORDER BY p.name", &id)?;
        let collections = query_named_refs(&o.conn, "SELECT c.id, c.name FROM collections c JOIN asset_collections ac ON ac.collection_id = c.id WHERE ac.asset_id = ?1 ORDER BY c.name", &id)?;

        Ok(AssetDetail {
            summary: asset_to_summary(asset.clone()),
            codec: asset.codec.clone(),
            container: asset.container.clone(),
            bitrate: asset.bitrate,
            latitude: asset.latitude,
            longitude: asset.longitude,
            altitude: asset.altitude,
            timezone: asset.timezone.clone(),
            parse_errors: asset.parse_errors.clone(),
            raw_metadata: serde_json::Value::Object(
                asset
                    .raw_metadata
                    .clone()
                    .into_iter()
                    .collect::<serde_json::Map<String, serde_json::Value>>(),
            ),
            tags,
            people,
            collections,
        })
    })
}

fn query_named_refs(
    conn: &rusqlite::Connection,
    sql: &str,
    id: &str,
) -> soundhub_core::Result<Vec<NamedRef>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([id], |r| {
        Ok(NamedRef {
            id: r.get(0)?,
            name: r.get(1)?,
        })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}

fn asset_to_summary(a: soundhub_core::Asset) -> AssetSummary {
    AssetSummary {
        id: a.id.to_string(),
        filename: a.filename.clone(),
        duration_ms: a.duration_ms,
        sample_rate: a.sample_rate,
        bit_depth: a.bit_depth,
        channels: a.channels,
        recorded_at: a.recorded_at.map(|d| d.to_rfc3339()),
        imported_at: a.imported_at.to_rfc3339(),
        status: a.status.as_str().to_string(),
        file_size: a.file_size,
        hash: a.hash.clone(),
        original_path: a.original_path.clone(),
        library_relpath: a.library_relpath.clone(),
    }
}

// ── Import ──────────────────────────────────────────────────────────────────

#[tauri::command]
async fn scan_paths(
    app: AppHandle,
    state: State<'_, AppState>,
    paths: Vec<String>,
) -> Result<ScanPreview, String> {
    let _ = state; // presence check via run_blocking
    run_blocking(app, move |o, _app| {
        let pb: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
        let mut summary = soundhub_core::import::scan_paths(&pb)
            .map_err(|e| soundhub_core::Error::other(e.to_string()))?;
        {
            let repo = Repo::new(&o.conn);
            soundhub_core::import::annotate_duplicates(&mut summary, &|hash| {
                repo.find_by_hash(hash)
                    .ok()
                    .flatten()
                    .map(|id| id.to_string())
            });
        }
        Ok(ScanPreview {
            ready: summary.ready,
            duplicate: summary.duplicate,
            unsupported: summary.unsupported,
            error: summary.error,
            files: summary
                .files
                .into_iter()
                .map(|f| ScannedPreview {
                    path: f.path,
                    size: f.size,
                    supported: f.supported,
                    duplicate_of: f.duplicate_of,
                    error: f.error,
                })
                .collect(),
        })
    })
    .await
}

#[tauri::command]
async fn import_paths(
    app: AppHandle,
    state: State<'_, AppState>,
    paths: Vec<String>,
    on_duplicate: String,
) -> Result<ImportResultDto, String> {
    let _ = state;
    run_blocking(app, move |o, app| {
        let action = parse_dup(&on_duplicate);
        let pb: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
        let total_estimate = pb.len() as u32;
        emit_progress(app, "import", 0, total_estimate, "starting");
        let pipeline = ImportPipeline::new(&o.library, &o.conn);
        let app2 = app.clone();
        let r = pipeline.import_paths_with_progress(
            &pb,
            action,
            &move |processed, total, current| {
                emit_progress(&app2, "import", processed, total, current);
            },
        )?;
        emit_progress(app, "import", r.total, r.total, "done");
        Ok(ImportResultDto {
            job_id: r.job_id,
            total: r.total,
            success: r.success,
            duplicate: r.duplicate,
            unsupported: r.unsupported,
            failed: r.failed,
            cancelled: r.cancelled,
        })
    })
    .await
}

#[tauri::command]
async fn incomplete_jobs(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<Vec<ImportJob>, String> {
    let _ = state;
    run_blocking(app, |o, _| Repo::new(&o.conn).list_incomplete_import_jobs()).await
}

/// Full startup recovery picture: incomplete jobs + non-ready assets.
#[tauri::command]
async fn recovery_report(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<RecoveryReportDto, String> {
    let _ = state;
    run_blocking(app, |o, _| {
        let report = o.library.inspect_recovery(&o.conn)?;
        Ok(RecoveryReportDto {
            incomplete_jobs: report.incomplete_jobs.len() as u32,
            incomplete_assets: report.incomplete_assets.len() as u32,
            library_files: report.library_files.len() as u32,
        })
    })
    .await
}

#[derive(Debug, Serialize)]
pub struct RecoveryReportDto {
    pub incomplete_jobs: u32,
    pub incomplete_assets: u32,
    pub library_files: u32,
}

#[tauri::command]
async fn resume_job(
    app: AppHandle,
    state: State<'_, AppState>,
    job_id: String,
) -> Result<ImportResultDto, String> {
    let _ = state;
    run_blocking(app, move |o, app| {
        let jid = RowId::parse(&job_id)?;
        let pipeline = ImportPipeline::new(&o.library, &o.conn);
        let app2 = app.clone();
        let r =
            pipeline.resume_incomplete_with_progress(&jid, &move |processed, total, current| {
                emit_progress(&app2, "resume", processed, total, current);
            })?;
        Ok(ImportResultDto {
            job_id: r.job_id,
            total: r.total,
            success: r.success,
            duplicate: r.duplicate,
            unsupported: r.unsupported,
            failed: r.failed,
            cancelled: r.cancelled,
        })
    })
    .await
}

#[tauri::command]
async fn retry_job(
    app: AppHandle,
    state: State<'_, AppState>,
    job_id: String,
) -> Result<ImportResultDto, String> {
    let _ = state;
    run_blocking(app, move |o, app| {
        let jid = RowId::parse(&job_id)?;
        let pipeline = ImportPipeline::new(&o.library, &o.conn);
        let app2 = app.clone();
        let r = pipeline.retry_failed_with_progress(&jid, &move |processed, total, current| {
            emit_progress(&app2, "retry", processed, total, current);
        })?;
        Ok(ImportResultDto {
            job_id: r.job_id,
            total: r.total,
            success: r.success,
            duplicate: r.duplicate,
            unsupported: r.unsupported,
            failed: r.failed,
            cancelled: r.cancelled,
        })
    })
    .await
}

/// Drop partial work for an incomplete job. Ready assets are never touched.
#[tauri::command]
async fn cleanup_job(
    app: AppHandle,
    state: State<'_, AppState>,
    job_id: String,
) -> Result<u32, String> {
    let _ = state;
    run_blocking(app, move |o, _| {
        let jid = RowId::parse(&job_id)?;
        let pipeline = ImportPipeline::new(&o.library, &o.conn);
        pipeline.cleanup_incomplete(&jid)
    })
    .await
}

// ── Search ──────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SearchInput {
    pub text: Option<String>,
    pub sample_rate: Option<u32>,
    pub tag: Option<String>,
    pub tags: Option<Vec<String>>,
    pub tags_all: Option<bool>,
    pub collection_id: Option<String>,
    pub person_id: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[tauri::command]
fn search_assets(
    state: State<'_, AppState>,
    query: SearchInput,
) -> Result<Vec<AssetSummary>, String> {
    with_open(&state, |o| {
        let q = SearchQuery {
            text: query.text,
            sample_rate: query.sample_rate,
            tag: query.tag,
            tags: query.tags.unwrap_or_default(),
            tags_all: query.tags_all.unwrap_or(false),
            collection_id: query.collection_id,
            person_id: query.person_id,
            limit: query.limit.unwrap_or(50),
            offset: query.offset.unwrap_or(0),
            ..Default::default()
        };
        let ids = search(&o.conn, &q)?;
        let repo = Repo::new(&o.conn);
        let mut out = Vec::new();
        for id in ids {
            if let Some(a) = repo.get_asset(&id)? {
                out.push(asset_to_summary(a));
            }
        }
        Ok(out)
    })
}

// ── Organization ────────────────────────────────────────────────────────────

#[tauri::command]
fn add_tag(state: State<'_, AppState>, asset_id: String, tag: String) -> Result<(), String> {
    with_open(&state, |o| {
        let aid = AssetId::parse(&asset_id)?;
        soundhub_core::undo::add_tag(&o.conn, &aid, &tag)
    })
}

#[tauri::command]
fn create_collection(
    state: State<'_, AppState>,
    name: String,
    asset_id: Option<String>,
) -> Result<String, String> {
    with_open(&state, |o| {
        let repo = Repo::new(&o.conn);
        let c = repo.create_collection(&name, None)?;
        if let Some(aid) = asset_id {
            let aid = AssetId::parse(&aid)?;
            repo.add_asset_collection(&aid, &c.id)?;
        }
        Ok(c.id.to_string())
    })
}

#[tauri::command]
fn add_to_collection(
    state: State<'_, AppState>,
    asset_id: String,
    collection_id: String,
) -> Result<(), String> {
    with_open(&state, |o| {
        let aid = AssetId::parse(&asset_id)?;
        let cid = RowId::parse(&collection_id)?;
        soundhub_core::undo::add_to_collection(&o.conn, &aid, &cid)
    })
}

#[tauri::command]
fn add_person(
    state: State<'_, AppState>,
    asset_id: String,
    name: String,
) -> Result<String, String> {
    with_open(&state, |o| {
        let aid = AssetId::parse(&asset_id)?;
        soundhub_core::undo::add_person(&o.conn, &aid, &name)?;
        // Return the person id so the UI can refresh.
        let p = Repo::new(&o.conn)
            .list_people()?
            .into_iter()
            .find(|p| p.name == name)
            .map(|p| p.id.to_string())
            .unwrap_or_default();
        Ok(p)
    })
}

#[derive(Debug, Serialize)]
pub struct TagDto {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct CollectionDto {
    pub id: String,
    pub name: String,
    pub collection_type: String,
    pub rules: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct PersonDto {
    pub id: String,
    pub name: String,
}

#[tauri::command]
fn list_tags(state: State<'_, AppState>) -> Result<Vec<TagDto>, String> {
    with_open(&state, |o| {
        Ok(Repo::new(&o.conn)
            .list_tags()?
            .into_iter()
            .map(|t| TagDto {
                id: t.id.to_string(),
                name: t.name,
            })
            .collect())
    })
}

#[derive(Debug, Serialize)]
pub struct TagUsageDto {
    pub id: String,
    pub name: String,
    pub asset_count: u64,
}

#[tauri::command]
fn list_tags_with_usage(state: State<'_, AppState>) -> Result<Vec<TagUsageDto>, String> {
    with_open(&state, |o| {
        Ok(Repo::new(&o.conn)
            .list_tags_with_usage()?
            .into_iter()
            .map(|t| TagUsageDto {
                id: t.id.to_string(),
                name: t.name,
                asset_count: t.asset_count,
            })
            .collect())
    })
}

#[tauri::command]
fn rename_tag(
    state: State<'_, AppState>,
    tag_id: String,
    new_name: String,
) -> Result<String, String> {
    with_open(&state, |o| {
        let tid = RowId::parse(&tag_id)?;
        soundhub_core::undo::rename_tag(&o.conn, &tid, &new_name)
    })
}

#[tauri::command]
fn delete_tag(state: State<'_, AppState>, tag_id: String) -> Result<String, String> {
    with_open(&state, |o| {
        let tid = RowId::parse(&tag_id)?;
        soundhub_core::undo::delete_tag(&o.conn, &tid)
    })
}

#[tauri::command]
fn merge_tags(state: State<'_, AppState>, from_id: String, to_id: String) -> Result<(), String> {
    with_open(&state, |o| {
        let from = RowId::parse(&from_id)?;
        let to = RowId::parse(&to_id)?;
        soundhub_core::undo::merge_tags(&o.conn, &from, &to)
    })
}

#[tauri::command]
fn list_collections(state: State<'_, AppState>) -> Result<Vec<CollectionDto>, String> {
    with_open(&state, |o| {
        Ok(Repo::new(&o.conn)
            .list_collections()?
            .into_iter()
            .map(|c| CollectionDto {
                id: c.id.to_string(),
                name: c.name,
                collection_type: c.collection_type.as_str().to_string(),
                rules: c.rules,
            })
            .collect())
    })
}

#[tauri::command]
fn list_people(state: State<'_, AppState>) -> Result<Vec<PersonDto>, String> {
    with_open(&state, |o| {
        Ok(Repo::new(&o.conn)
            .list_people()?
            .into_iter()
            .map(|p| PersonDto {
                id: p.id.to_string(),
                name: p.name,
            })
            .collect())
    })
}

/// Unlink only — never deletes the asset file (PRD rule 11).
#[tauri::command]
fn remove_from_collection(
    state: State<'_, AppState>,
    asset_id: String,
    collection_id: String,
) -> Result<(), String> {
    with_open(&state, |o| {
        let aid = AssetId::parse(&asset_id)?;
        let cid = RowId::parse(&collection_id)?;
        soundhub_core::undo::remove_from_collection(&o.conn, &aid, &cid)
    })
}

#[tauri::command]
fn remove_tag(state: State<'_, AppState>, asset_id: String, tag_id: String) -> Result<(), String> {
    with_open(&state, |o| {
        let aid = AssetId::parse(&asset_id)?;
        let tid = RowId::parse(&tag_id)?;
        soundhub_core::undo::remove_tag(&o.conn, &aid, &tid)
    })
}

#[tauri::command]
fn remove_person(
    state: State<'_, AppState>,
    asset_id: String,
    person_id: String,
) -> Result<(), String> {
    with_open(&state, |o| {
        let aid = AssetId::parse(&asset_id)?;
        let pid = RowId::parse(&person_id)?;
        soundhub_core::undo::remove_person(&o.conn, &aid, &pid)
    })
}

// ── Recycle bin + Undo ─────────────────────────────────────────────────────

#[tauri::command]
fn soft_delete_assets(state: State<'_, AppState>, asset_ids: Vec<String>) -> Result<u32, String> {
    let ids = parse_asset_ids(&asset_ids)?;
    with_open(&state, |o| soundhub_core::undo::soft_delete(&o.conn, &ids))
}

#[tauri::command]
fn restore_assets(state: State<'_, AppState>, asset_ids: Vec<String>) -> Result<u32, String> {
    let ids = parse_asset_ids(&asset_ids)?;
    with_open(&state, |o| soundhub_core::undo::restore(&o.conn, &ids))
}

#[tauri::command]
fn list_deleted_assets(state: State<'_, AppState>) -> Result<Vec<AssetSummary>, String> {
    with_open(&state, |o| {
        Ok(Repo::new(&o.conn)
            .list_deleted_assets(200, 0)?
            .into_iter()
            .map(asset_to_summary)
            .collect())
    })
}

#[tauri::command]
fn purge_assets(state: State<'_, AppState>, asset_ids: Vec<String>) -> Result<u32, String> {
    let ids = parse_asset_ids(&asset_ids)?;
    with_open(&state, |o| {
        let repo = Repo::new(&o.conn);
        let mut n = 0u32;
        for id in &ids {
            if let Some(a) = repo.get_asset(id)? {
                if a.deleted_at.is_some() {
                    let abs = o.library.asset_abspath(&a.library_relpath);
                    repo.purge_asset(id, &abs)?;
                    n += 1;
                }
            }
        }
        Ok(n)
    })
}

#[tauri::command]
fn empty_trash(state: State<'_, AppState>) -> Result<u32, String> {
    with_open(&state, |o| {
        let repo = Repo::new(&o.conn);
        let deleted = repo.list_deleted_assets(10_000, 0)?;
        let mut n = 0u32;
        for a in deleted {
            let abs = o.library.asset_abspath(&a.library_relpath);
            repo.purge_asset(&a.id, &abs)?;
            n += 1;
        }
        Ok(n)
    })
}

#[tauri::command]
fn undo_last(state: State<'_, AppState>) -> Result<String, String> {
    with_open(&state, |o| {
        soundhub_core::undo::undo_last(&o.conn)?
            .ok_or_else(|| soundhub_core::Error::other("nothing to undo"))
    })
}

#[tauri::command]
fn undo_peek(state: State<'_, AppState>) -> Result<Option<String>, String> {
    with_open(&state, |o| soundhub_core::undo::peek_label(&o.conn))
}

// ── Duplicates ─────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct DuplicateAssetDto {
    pub id: String,
    pub filename: String,
    pub original_path: String,
    pub imported_at: String,
    pub file_size: u64,
}

#[derive(Debug, Serialize)]
pub struct DuplicateGroupDto {
    pub hash: String,
    pub file_size: u64,
    pub assets: Vec<DuplicateAssetDto>,
}

fn dup_asset_dto(a: &soundhub_core::models::Asset) -> DuplicateAssetDto {
    DuplicateAssetDto {
        id: a.id.to_string(),
        filename: a.filename.clone(),
        original_path: a.original_path.clone(),
        imported_at: a.imported_at.to_rfc3339(),
        file_size: a.file_size,
    }
}

#[tauri::command]
fn list_duplicate_groups(state: State<'_, AppState>) -> Result<Vec<DuplicateGroupDto>, String> {
    with_open(&state, |o| {
        Ok(soundhub_core::duplicates::find_groups(&o.conn)?
            .into_iter()
            .map(|g| DuplicateGroupDto {
                hash: g.hash,
                file_size: g.file_size,
                assets: g.assets.iter().map(dup_asset_dto).collect(),
            })
            .collect())
    })
}

/// `strategy`: "oldest" | "newest" | "lowest-id". Optional `hash` limits to one group.
#[tauri::command]
fn dedupe_library(
    state: State<'_, AppState>,
    strategy: String,
    hash: Option<String>,
) -> Result<u32, String> {
    with_open(&state, |o| {
        use soundhub_core::duplicates::KeepStrategy;
        let strat = match strategy.as_str() {
            "oldest" => KeepStrategy::Oldest,
            "newest" => KeepStrategy::Newest,
            "lowest-id" => KeepStrategy::LowestId,
            other => {
                return Err(soundhub_core::Error::other(format!(
                    "bad strategy: {other}"
                )))
            }
        };
        match hash {
            Some(h) => soundhub_core::duplicates::dedupe_group(&o.conn, &h, strat),
            None => soundhub_core::duplicates::dedupe(&o.conn, strat).map(|(_, t)| t),
        }
    })
}

// ── Playlists ──────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct PlaylistDto {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub track_count: u64,
}

#[derive(Debug, Serialize)]
pub struct PlaylistTrackDto {
    pub position: u32,
    pub asset_id: String,
    pub filename: String,
    pub duration_ms: Option<u64>,
    pub file_size: u64,
}

#[tauri::command]
fn list_playlists(state: State<'_, AppState>) -> Result<Vec<PlaylistDto>, String> {
    with_open(&state, |o| {
        Ok(Repo::new(&o.conn)
            .list_playlists()?
            .into_iter()
            .map(|p| PlaylistDto {
                id: p.id.to_string(),
                name: p.name,
                created_at: p.created_at,
                track_count: p.track_count,
            })
            .collect())
    })
}

#[tauri::command]
fn create_playlist(state: State<'_, AppState>, name: String) -> Result<String, String> {
    with_open(&state, |o| {
        Ok(Repo::new(&o.conn).create_playlist(&name)?.id.to_string())
    })
}

#[tauri::command]
fn rename_playlist(
    state: State<'_, AppState>,
    playlist_id: String,
    name: String,
) -> Result<(), String> {
    with_open(&state, |o| {
        let pid = RowId::parse(&playlist_id)?;
        Repo::new(&o.conn).rename_playlist(&pid, &name)
    })
}

#[tauri::command]
fn delete_playlist(state: State<'_, AppState>, playlist_id: String) -> Result<(), String> {
    with_open(&state, |o| {
        let pid = RowId::parse(&playlist_id)?;
        Repo::new(&o.conn).delete_playlist(&pid)
    })
}

#[tauri::command]
fn list_playlist_tracks(
    state: State<'_, AppState>,
    playlist_id: String,
) -> Result<Vec<PlaylistTrackDto>, String> {
    with_open(&state, |o| {
        let pid = RowId::parse(&playlist_id)?;
        Ok(Repo::new(&o.conn)
            .list_playlist_tracks(&pid)?
            .into_iter()
            .map(|t| PlaylistTrackDto {
                position: t.position,
                asset_id: t.asset_id.to_string(),
                filename: t.filename,
                duration_ms: t.duration_ms,
                file_size: t.file_size,
            })
            .collect())
    })
}

#[tauri::command]
fn playlist_add_tracks(
    state: State<'_, AppState>,
    playlist_id: String,
    asset_ids: Vec<String>,
) -> Result<u32, String> {
    with_open(&state, |o| {
        let pid = RowId::parse(&playlist_id)?;
        let ids = parse_asset_ids_core(&asset_ids)?;
        let repo = Repo::new(&o.conn);
        let mut n = 0u32;
        for id in &ids {
            repo.playlist_add_track(&pid, id)?;
            n += 1;
        }
        Ok(n)
    })
}

#[tauri::command]
fn playlist_remove_track(
    state: State<'_, AppState>,
    playlist_id: String,
    asset_id: String,
) -> Result<bool, String> {
    with_open(&state, |o| {
        let pid = RowId::parse(&playlist_id)?;
        let aid = AssetId::parse(&asset_id)?;
        Repo::new(&o.conn).playlist_remove_track(&pid, &aid)
    })
}

#[tauri::command]
fn playlist_move_track(
    state: State<'_, AppState>,
    playlist_id: String,
    from: u32,
    to: u32,
) -> Result<(), String> {
    with_open(&state, |o| {
        let pid = RowId::parse(&playlist_id)?;
        Repo::new(&o.conn).playlist_move_track(&pid, from, to)
    })
}

#[tauri::command]
fn playlist_reorder(
    state: State<'_, AppState>,
    playlist_id: String,
    asset_ids: Vec<String>,
) -> Result<(), String> {
    with_open(&state, |o| {
        let pid = RowId::parse(&playlist_id)?;
        let ids = parse_asset_ids_core(&asset_ids)?;
        Repo::new(&o.conn).playlist_reorder(&pid, &ids)
    })
}

// ── Library setup (first launch) ───────────────────────────────────────────

#[tauri::command]
fn recent_libraries() -> Result<Vec<crate::library_setup::RecentLibrary>, String> {
    Ok(crate::library_setup::load_recent())
}

#[tauri::command]
fn forget_recent_library(path: String) -> Result<(), String> {
    crate::library_setup::remove_recent(&path);
    Ok(())
}

#[tauri::command]
fn inspect_library_path(path: String) -> Result<crate::library_setup::LibraryInspection, String> {
    Ok(crate::library_setup::inspect(&path))
}

#[tauri::command]
fn suggest_library_locations() -> Result<Vec<crate::library_setup::DefaultLocation>, String> {
    Ok(crate::library_setup::suggest_locations())
}

#[tauri::command]
fn compose_library_path(parent: String, name: String) -> Result<String, String> {
    Ok(crate::library_setup::compose_library_path(&parent, &name))
}

#[tauri::command]
fn creation_preview(path: String) -> Result<Vec<String>, String> {
    Ok(crate::library_setup::creation_preview(&path))
}

/// Native folder picker (works even if dialog JS plugin is unavailable).
#[tauri::command]
fn pick_directory(title: Option<String>) -> Result<Option<String>, String> {
    let mut dlg = rfd::FileDialog::new();
    if let Some(t) = title {
        dlg = dlg.set_title(t);
    }
    Ok(dlg.pick_folder().map(|p| p.display().to_string()))
}

/// Native multi-file picker for audio.
#[tauri::command]
fn pick_audio_files(title: Option<String>) -> Result<Vec<String>, String> {
    let mut dlg = rfd::FileDialog::new();
    if let Some(t) = title {
        dlg = dlg.set_title(t);
    }
    let picked = dlg
        .add_filter(
            "Audio",
            &[
                "wav", "bwf", "aif", "aiff", "flac", "mp3", "m4a", "aac", "caf",
            ],
        )
        .add_filter("All files", &["*"])
        .pick_files()
        .unwrap_or_default();
    Ok(picked
        .into_iter()
        .map(|p| p.display().to_string())
        .collect())
}

// ── Misc ────────────────────────────────────────────────────────────────────

/// Liveness probe for the async command path (used by Import UI diagnostics).
#[tauri::command]
async fn debug_ping(app: AppHandle) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let guard = state.inner.lock().map_err(|e| e.to_string())?;
        let open = guard.is_some();
        Ok(if open {
            "pong:library-open".into()
        } else {
            "pong:no-library".into()
        })
    })
    .await
    .map_err(|e| format!("join: {e}"))?
}

#[tauri::command]
fn hash_file_cmd(path: String) -> Result<String, String> {
    hash_file(&PathBuf::from(path)).map_err(|e| e.to_string())
}

#[tauri::command]
fn asset_file_exists(state: State<'_, AppState>, id: String) -> Result<bool, String> {
    with_open(&state, |o| {
        let aid = AssetId::parse(&id)?;
        let asset = Repo::new(&o.conn).get_asset(&aid)?;
        Ok(match asset {
            Some(a) => {
                a.status == AssetStatus::Ready
                    && o.library.asset_abspath(&a.library_relpath).exists()
            }
            None => false,
        })
    })
}

// ── Playback / waveform ─────────────────────────────────────────────────────

fn ready_asset_path(
    o: &OpenLibrary,
    id: &str,
) -> soundhub_core::Result<(AssetId, PathBuf, String)> {
    let aid = AssetId::parse(id)?;
    let asset = Repo::new(&o.conn)
        .get_asset(&aid)?
        .ok_or_else(|| soundhub_core::Error::AssetNotFound(id.to_string()))?;
    if asset.status != AssetStatus::Ready {
        return Err(soundhub_core::Error::other("asset is not ready"));
    }
    let abs = o.library.asset_abspath(&asset.library_relpath);
    if !abs.exists() {
        return Err(soundhub_core::Error::other(
            "asset file missing from library",
        ));
    }
    Ok((aid, abs, asset.filename.clone()))
}

/// Absolute filesystem path of a ready asset — for `convertFileSrc` streaming.
/// Preferred playback path: the asset protocol serves this with HTTP Range,
/// so large files never need to be buffered whole.
#[tauri::command]
fn asset_file_path(state: State<'_, AppState>, id: String) -> Result<String, String> {
    with_open(&state, |o| {
        let (_, abs, _) = ready_asset_path(o, &id)?;
        Ok(abs.display().to_string())
    })
}

/// Cap for the blob-URL fallback. Above this, refuse to buffer the file over
/// IPC — callers must use `asset_file_path` + `convertFileSrc` instead.
const MAX_INLINE_AUDIO_BYTES: u64 = 64 * 1024 * 1024;

/// Raw file bytes as an IPC response (blob-URL playback fallback).
/// Only for small files / environments without the asset protocol.
#[tauri::command]
async fn asset_audio_bytes(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<tauri::ipc::Response, String> {
    let _ = state;
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let guard = state.inner.lock().map_err(|e| e.to_string())?;
        let open = guard.as_ref().ok_or("Library not open")?;
        let (_, abs, _) = ready_asset_path(open, &id).map_err(|e| e.to_string())?;
        let len = std::fs::metadata(&abs).map_err(|e| e.to_string())?.len();
        if len > MAX_INLINE_AUDIO_BYTES {
            return Err(format!(
                "audio file is {len} bytes; inline playback is capped at \
                 {MAX_INLINE_AUDIO_BYTES}. Use asset_file_path + convertFileSrc."
            ));
        }
        use std::io::Read;
        let mut bytes = Vec::with_capacity(len as usize);
        let mut f = std::fs::File::open(&abs).map_err(|e| e.to_string())?;
        f.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        Ok(tauri::ipc::Response::new(bytes))
    })
    .await
    .map_err(|e| format!("background task failed: {e}"))?
}

#[derive(Debug, Serialize)]
pub struct PeaksDto {
    pub peaks: Option<Vec<f32>>,
    pub source: Option<String>,
}

/// Waveform peaks for an asset. Computes from WAV/AIFF PCM and caches under
/// `cache/`; compressed formats return `peaks: null` so the UI can use Web Audio.
#[tauri::command]
async fn asset_peaks(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    buckets: Option<u32>,
) -> Result<PeaksDto, String> {
    let _ = state;
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let guard = state.inner.lock().map_err(|e| e.to_string())?;
        let open = guard.as_ref().ok_or("Library not open")?;
        let (aid, abs, _) = ready_asset_path(open, &id).map_err(|e| e.to_string())?;

        if let Some(cached) = open.library.load_peaks(&aid) {
            return Ok(PeaksDto {
                peaks: Some(cached.peaks),
                source: Some(cached.source),
            });
        }

        let n = buckets.unwrap_or(soundhub_core::audio::DEFAULT_BUCKETS as u32) as usize;
        match soundhub_core::audio::compute_file_peaks(&abs, n) {
            Ok(peaks) => {
                let cache = soundhub_core::audio::PeaksCache {
                    version: 1,
                    source: "pcm".into(),
                    peaks: peaks.clone(),
                };
                let _ = open.library.save_peaks(&aid, &cache);
                Ok(PeaksDto {
                    peaks: Some(peaks),
                    source: Some("pcm".into()),
                })
            }
            Err(_) => Ok(PeaksDto {
                peaks: None,
                source: None,
            }),
        }
    })
    .await
    .map_err(|e| format!("background task failed: {e}"))?
}

/// Cache peaks computed in the UI (Web Audio) for formats the core cannot decode.
#[tauri::command]
fn save_asset_peaks(state: State<'_, AppState>, id: String, peaks: Vec<f32>) -> Result<(), String> {
    with_open(&state, |o| {
        let aid = AssetId::parse(&id)?;
        let cache = soundhub_core::audio::PeaksCache {
            version: 1,
            source: "web_audio".into(),
            peaks,
        };
        o.library.save_peaks(&aid, &cache)
    })
}

// ── Smart Collections ──────────────────────────────────────────────────────

fn parse_smart_rules(
    rules: Option<serde_json::Value>,
) -> Result<soundhub_core::SmartRules, String> {
    let raw = rules.ok_or("rules are required")?;
    soundhub_core::collections::parse_rules(&raw)
        .ok_or_else(|| "invalid smart-collection rules JSON".to_string())
}

#[tauri::command]
fn create_smart_collection(
    state: State<'_, AppState>,
    name: String,
    rules: Option<serde_json::Value>,
) -> Result<String, String> {
    let rules = parse_smart_rules(rules)?;
    with_open(&state, |o| {
        let c = Repo::new(&o.conn).create_smart_collection(&name, &rules)?;
        Ok(c.id.to_string())
    })
}

#[tauri::command]
fn update_smart_collection_rules(
    state: State<'_, AppState>,
    collection_id: String,
    rules: Option<serde_json::Value>,
) -> Result<(), String> {
    let rules = parse_smart_rules(rules)?;
    with_open(&state, |o| {
        let cid = RowId::parse(&collection_id)?;
        Repo::new(&o.conn).update_collection_rules(&cid, &rules)
    })
}

/// Members of a collection. Static = stored links; Smart = live rule evaluation.
#[tauri::command]
fn list_collection_assets(
    state: State<'_, AppState>,
    collection_id: String,
) -> Result<Vec<AssetSummary>, String> {
    with_open(&state, |o| {
        let cid = RowId::parse(&collection_id)?;
        let ids = Repo::new(&o.conn).list_collection_assets(&cid)?;
        let repo = Repo::new(&o.conn);
        let mut out = Vec::new();
        for id in ids {
            if let Some(a) = repo.get_asset(&id)? {
                out.push(asset_to_summary(a));
            }
        }
        Ok(out)
    })
}

// ── GPS / Map ──────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct GpsAssetDto {
    pub id: String,
    pub filename: String,
    pub latitude: f64,
    pub longitude: f64,
    pub altitude: Option<f64>,
    pub recorded_at: Option<String>,
}

fn gps_to_dto(a: soundhub_core::models::Asset, lat: f64, lon: f64) -> GpsAssetDto {
    GpsAssetDto {
        id: a.id.to_string(),
        filename: a.filename,
        latitude: lat,
        longitude: lon,
        altitude: a.altitude,
        recorded_at: a.recorded_at.map(|t| t.to_rfc3339()),
    }
}

#[tauri::command]
fn list_assets_with_gps(state: State<'_, AppState>) -> Result<Vec<GpsAssetDto>, String> {
    with_open(&state, |o| {
        Ok(Repo::new(&o.conn)
            .list_assets_with_gps()?
            .into_iter()
            .map(|(a, lat, lon)| gps_to_dto(a, lat, lon))
            .collect())
    })
}

#[tauri::command]
fn assets_in_bbox(
    state: State<'_, AppState>,
    min_lat: f64,
    max_lat: f64,
    min_lon: f64,
    max_lon: f64,
) -> Result<Vec<GpsAssetDto>, String> {
    with_open(&state, |o| {
        Ok(Repo::new(&o.conn)
            .assets_in_bbox(min_lat, max_lat, min_lon, max_lon)?
            .into_iter()
            .map(|(a, lat, lon)| gps_to_dto(a, lat, lon))
            .collect())
    })
}

// ── Batch editing ──────────────────────────────────────────────────────────

fn parse_asset_ids(ids: &[String]) -> Result<Vec<AssetId>, String> {
    ids.iter()
        .map(|s| AssetId::parse(s).map_err(|e| e.to_string()))
        .collect()
}

fn parse_asset_ids_core(ids: &[String]) -> Result<Vec<AssetId>, soundhub_core::Error> {
    ids.iter().map(|s| AssetId::parse(s)).collect()
}

#[tauri::command]
fn batch_add_tag(
    state: State<'_, AppState>,
    asset_ids: Vec<String>,
    tag: String,
) -> Result<u32, String> {
    let ids = parse_asset_ids(&asset_ids)?;
    with_open(&state, |o| Repo::new(&o.conn).batch_add_tag(&ids, &tag))
}

#[tauri::command]
fn batch_remove_tag(
    state: State<'_, AppState>,
    asset_ids: Vec<String>,
    tag: String,
) -> Result<u32, String> {
    let ids = parse_asset_ids(&asset_ids)?;
    with_open(&state, |o| Repo::new(&o.conn).batch_remove_tag(&ids, &tag))
}

#[tauri::command]
fn batch_add_person(
    state: State<'_, AppState>,
    asset_ids: Vec<String>,
    name: String,
) -> Result<u32, String> {
    let ids = parse_asset_ids(&asset_ids)?;
    with_open(&state, |o| Repo::new(&o.conn).batch_add_person(&ids, &name))
}

#[tauri::command]
fn batch_add_to_collection(
    state: State<'_, AppState>,
    asset_ids: Vec<String>,
    collection_id: String,
) -> Result<u32, String> {
    let ids = parse_asset_ids(&asset_ids)?;
    with_open(&state, |o| {
        let cid = RowId::parse(&collection_id)?;
        Repo::new(&o.conn).batch_add_to_collection(&ids, &cid)
    })
}

#[tauri::command]
fn batch_remove_from_collection(
    state: State<'_, AppState>,
    asset_ids: Vec<String>,
    collection_id: String,
) -> Result<u32, String> {
    let ids = parse_asset_ids(&asset_ids)?;
    with_open(&state, |o| {
        let cid = RowId::parse(&collection_id)?;
        Repo::new(&o.conn).batch_remove_from_collection(&ids, &cid)
    })
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            create_library,
            open_library,
            library_info,
            recent_libraries,
            forget_recent_library,
            inspect_library_path,
            suggest_library_locations,
            compose_library_path,
            creation_preview,
            pick_directory,
            pick_audio_files,
            list_assets,
            get_asset,
            scan_paths,
            import_paths,
            incomplete_jobs,
            recovery_report,
            resume_job,
            retry_job,
            cleanup_job,
            search_assets,
            add_tag,
            remove_tag,
            create_collection,
            add_to_collection,
            remove_from_collection,
            add_person,
            remove_person,
            list_tags,
            list_tags_with_usage,
            rename_tag,
            delete_tag,
            merge_tags,
            list_collections,
            list_people,
            hash_file_cmd,
            asset_file_exists,
            asset_file_path,
            asset_audio_bytes,
            asset_peaks,
            save_asset_peaks,
            create_smart_collection,
            update_smart_collection_rules,
            list_collection_assets,
            list_assets_with_gps,
            assets_in_bbox,
            batch_add_tag,
            batch_remove_tag,
            batch_add_person,
            batch_add_to_collection,
            batch_remove_from_collection,
            soft_delete_assets,
            restore_assets,
            list_deleted_assets,
            purge_assets,
            empty_trash,
            undo_last,
            undo_peek,
            list_duplicate_groups,
            dedupe_library,
            list_playlists,
            create_playlist,
            rename_playlist,
            delete_playlist,
            list_playlist_tracks,
            playlist_add_tracks,
            playlist_remove_track,
            playlist_move_track,
            playlist_reorder,
            debug_ping,
        ])
        .run(tauri::generate_context!())
        .expect("error while running soundbuch");
}
