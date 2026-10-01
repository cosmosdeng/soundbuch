//! In-library duplicate detection and cleanup.
//!
//! Import-time dedup (`DuplicateAction::Skip`) already blocks most copies.
//! This module finds assets that share a SHA-256 inside the library —
//! e.g. imported via `ImportAsDuplicate`, or from an older build — and
//! lets the user collapse each group down to one keeper.

use rusqlite::Connection;

use crate::db::Repo;
use crate::error::Result;
use crate::ids::AssetId;
use crate::models::Asset;
use crate::undo;

/// One content-identical cluster.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DuplicateGroup {
    /// SHA-256 shared by every member.
    pub hash: String,
    pub file_size: u64,
    /// Members sorted oldest-import first (the natural default keeper).
    pub assets: Vec<Asset>,
}

/// How to pick the survivor when collapsing a group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeepStrategy {
    /// Earliest `imported_at`.
    Oldest,
    /// Latest `imported_at`.
    Newest,
    /// Lowest asset id (stable, deterministic).
    LowestId,
    /// Keep the named asset, trash the rest of its group.
    Id(AssetId),
}

/// All ready, non-trashed groups with more than one member.
pub fn find_groups(conn: &Connection) -> Result<Vec<DuplicateGroup>> {
    let mut stmt = conn.prepare(
        "SELECT hash, COUNT(*) AS n FROM assets \
         WHERE status = 'ready' AND deleted_at IS NULL \
         GROUP BY hash HAVING n > 1 \
         ORDER BY n DESC, hash",
    )?;
    let hashes: Vec<String> = {
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };

    let repo = Repo::new(conn);
    let mut out = Vec::new();
    for hash in hashes {
        let assets = repo.list_ready_by_hash(&hash)?;
        if assets.len() < 2 {
            continue;
        }
        let file_size = assets[0].file_size;
        out.push(DuplicateGroup {
            hash,
            file_size,
            assets,
        });
    }
    Ok(out)
}

/// Collapse every group to one keeper. Returns (groups_processed, assets_trashed).
/// Trashed copies go to the recycle bin (soft-delete) and are undoable as one step.
pub fn dedupe(conn: &Connection, strategy: KeepStrategy) -> Result<(u32, u32)> {
    let groups = find_groups(conn)?;
    let mut groups_n = 0u32;
    let mut extras_all: Vec<AssetId> = Vec::new();

    for g in &groups {
        let keeper = pick_keeper(g, &strategy);
        for a in &g.assets {
            if a.id != keeper {
                extras_all.push(a.id.clone());
            }
        }
        groups_n += 1;
    }

    if extras_all.is_empty() {
        return Ok((0, 0));
    }
    // One undo entry covers the whole cleanup.
    let trashed = undo::soft_delete(conn, &extras_all)?;
    Ok((groups_n, trashed))
}

/// Collapse a single group by hash. Returns how many copies were trashed.
pub fn dedupe_group(conn: &Connection, hash: &str, strategy: KeepStrategy) -> Result<u32> {
    let repo = Repo::new(conn);
    let assets = repo.list_ready_by_hash(hash)?;
    if assets.len() < 2 {
        return Ok(0);
    }
    let g = DuplicateGroup {
        hash: hash.to_string(),
        file_size: assets[0].file_size,
        assets,
    };
    let keeper = pick_keeper(&g, &strategy);
    let extras: Vec<AssetId> = g
        .assets
        .iter()
        .filter(|a| a.id != keeper)
        .map(|a| a.id.clone())
        .collect();
    undo::soft_delete(conn, &extras)
}

fn pick_keeper(g: &DuplicateGroup, strategy: &KeepStrategy) -> AssetId {
    match strategy {
        KeepStrategy::Oldest => g.assets[0].id.clone(),
        KeepStrategy::Newest => g.assets.last().expect("non-empty group").id.clone(),
        KeepStrategy::LowestId => {
            let mut min = &g.assets[0];
            for a in &g.assets[1..] {
                if a.id.as_str() < min.id.as_str() {
                    min = a;
                }
            }
            min.id.clone()
        }
        KeepStrategy::Id(id) => {
            if g.assets.iter().any(|a| a.id == *id) {
                id.clone()
            } else {
                g.assets[0].id.clone()
            }
        }
    }
}

/// Summary line for CLI/UI.
pub fn group_summary(g: &DuplicateGroup) -> String {
    format!(
        "{} × {}  {:.1} KB  hash={}",
        g.assets.len(),
        g.assets[0].filename,
        g.file_size as f64 / 1024.0,
        &g.hash[..12.min(g.hash.len())]
    )
}
