//! Undo stack for reversible library edits.
//!
//! Every user-facing mutation that can be reversed logs a `UndoAction` (or a
//! batch of them). `undo_last` pops the newest entry and applies the reverse
//! steps in order.

use rusqlite::{Connection, OptionalExtension};

use crate::db::Repo;
use crate::error::{Error, Result};
use crate::ids::{AssetId, RowId};
use crate::models::*;

/// Record + perform: add a tag (undo → remove).
pub fn add_tag(conn: &Connection, asset_id: &AssetId, tag_name: &str) -> Result<()> {
    let repo = Repo::new(conn);
    let tag = repo.upsert_tag(tag_name)?;
    repo.add_asset_tag(asset_id, &tag.id)?;
    let action = UndoAction::RemoveTag {
        asset_id: asset_id.to_string(),
        tag_id: tag.id.to_string(),
    };
    repo.push_undo(
        UndoKind::AddTag,
        &format!("Add tag “{tag_name}”"),
        &action.to_value(),
    )?;
    Ok(())
}

/// Record + perform: remove a tag (undo → re-link).
pub fn remove_tag(conn: &Connection, asset_id: &AssetId, tag_id: &RowId) -> Result<()> {
    let repo = Repo::new(conn);
    let tag_name: Option<String> = conn
        .query_row(
            "SELECT name FROM tags WHERE id = ?1",
            [tag_id.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    repo.remove_asset_tag(asset_id, tag_id)?;
    let action = UndoAction::AddTag {
        asset_id: asset_id.to_string(),
        tag_id: tag_id.to_string(),
        tag_name: tag_name.unwrap_or_default(),
    };
    repo.push_undo(UndoKind::RemoveTag, "Remove tag", &action.to_value())?;
    Ok(())
}

pub fn add_person(conn: &Connection, asset_id: &AssetId, name: &str) -> Result<()> {
    let repo = Repo::new(conn);
    let p = repo.create_person(name, None)?;
    repo.link_asset_person(asset_id, &p.id)?;
    let action = UndoAction::RemovePerson {
        asset_id: asset_id.to_string(),
        person_id: p.id.to_string(),
    };
    repo.push_undo(
        UndoKind::AddPerson,
        &format!("Add person “{name}”"),
        &action.to_value(),
    )?;
    Ok(())
}

/// Rename a tag (undo → rename back). Reindexes affected assets.
pub fn rename_tag(conn: &Connection, tag_id: &RowId, new_name: &str) -> Result<String> {
    let repo = Repo::new(conn);
    let old = repo.rename_tag(tag_id, new_name)?;
    let action = UndoAction::RenameTag {
        tag_id: tag_id.to_string(),
        old_name: old.clone(),
        new_name: new_name.to_string(),
    };
    repo.push_undo(
        UndoKind::RenameTag,
        &format!("Rename tag to “{new_name}”"),
        &action.to_value(),
    )?;
    Ok(old)
}

/// Delete a tag everywhere (undo → recreate + relink).
pub fn delete_tag(conn: &Connection, tag_id: &RowId) -> Result<String> {
    let repo = Repo::new(conn);
    let (name, asset_ids) = repo.delete_tag(tag_id)?;
    let action = UndoAction::RestoreTag {
        tag_id: tag_id.to_string(),
        tag_name: name.clone(),
        asset_ids: asset_ids.iter().map(|a| a.to_string()).collect(),
    };
    repo.push_undo(
        UndoKind::DeleteTag,
        &format!("Delete tag “{name}”"),
        &action.to_value(),
    )?;
    Ok(name)
}

/// Merge `from` into `to` (undo → split back).
pub fn merge_tags(conn: &Connection, from_id: &RowId, to_id: &RowId) -> Result<()> {
    let repo = Repo::new(conn);
    let (from_name, to_id, asset_ids) = repo.merge_tags(from_id, to_id)?;
    let action = UndoAction::UnmergeTag {
        from_tag_id: from_id.to_string(),
        from_tag_name: from_name.clone(),
        to_tag_id: to_id.to_string(),
        asset_ids: asset_ids.iter().map(|a| a.to_string()).collect(),
    };
    repo.push_undo(
        UndoKind::MergeTags,
        &format!("Merge tag “{from_name}”"),
        &action.to_value(),
    )?;
    Ok(())
}

pub fn remove_person(conn: &Connection, asset_id: &AssetId, person_id: &RowId) -> Result<()> {
    let repo = Repo::new(conn);
    let name: Option<String> = conn
        .query_row(
            "SELECT name FROM people WHERE id = ?1",
            [person_id.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    repo.remove_asset_person(asset_id, person_id)?;
    let action = UndoAction::AddPerson {
        asset_id: asset_id.to_string(),
        person_id: person_id.to_string(),
        person_name: name.unwrap_or_default(),
    };
    repo.push_undo(UndoKind::RemovePerson, "Remove person", &action.to_value())?;
    Ok(())
}

pub fn add_to_collection(
    conn: &Connection,
    asset_id: &AssetId,
    collection_id: &RowId,
) -> Result<()> {
    let repo = Repo::new(conn);
    repo.add_asset_collection(asset_id, collection_id)?;
    let action = UndoAction::RemoveFromCollection {
        asset_id: asset_id.to_string(),
        collection_id: collection_id.to_string(),
    };
    repo.push_undo(
        UndoKind::AddToCollection,
        "Add to collection",
        &action.to_value(),
    )?;
    Ok(())
}

pub fn remove_from_collection(
    conn: &Connection,
    asset_id: &AssetId,
    collection_id: &RowId,
) -> Result<()> {
    let repo = Repo::new(conn);
    repo.remove_asset_from_collection(asset_id, collection_id)?;
    let action = UndoAction::AddToCollection {
        asset_id: asset_id.to_string(),
        collection_id: collection_id.to_string(),
    };
    repo.push_undo(
        UndoKind::RemoveFromCollection,
        "Remove from collection",
        &action.to_value(),
    )?;
    Ok(())
}

/// Soft-delete one or more assets (undo → restore).
pub fn soft_delete(conn: &Connection, asset_ids: &[AssetId]) -> Result<u32> {
    let repo = Repo::new(conn);
    let n = repo.soft_delete_assets(asset_ids)?;
    if n == 0 {
        return Ok(0);
    }
    let actions: Vec<UndoAction> = asset_ids
        .iter()
        .map(|id| UndoAction::Restore {
            asset_id: id.to_string(),
        })
        .collect();
    let payload = serde_json::json!({ "actions": actions });
    let label = if n == 1 {
        "Move to recycle bin".to_string()
    } else {
        format!("Move {n} items to recycle bin")
    };
    repo.push_undo(UndoKind::SoftDelete, &label, &payload)?;
    Ok(n)
}

pub fn restore(conn: &Connection, asset_ids: &[AssetId]) -> Result<u32> {
    let repo = Repo::new(conn);
    let n = repo.restore_assets(asset_ids)?;
    if n == 0 {
        return Ok(0);
    }
    let actions: Vec<UndoAction> = asset_ids
        .iter()
        .map(|id| UndoAction::SoftDelete {
            asset_id: id.to_string(),
        })
        .collect();
    let payload = serde_json::json!({ "actions": actions });
    repo.push_undo(UndoKind::Restore, "Restore from recycle bin", &payload)?;
    Ok(n)
}

/// Undo the newest recorded action. Returns a human-readable label.
pub fn undo_last(conn: &Connection) -> Result<Option<String>> {
    let repo = Repo::new(conn);
    let Some(entry) = repo.pop_undo()? else {
        return Ok(None);
    };
    let mut actions: Vec<UndoAction> = Vec::new();
    if let Some(arr) = entry.payload.get("actions").and_then(|v| v.as_array()) {
        for a in arr {
            if let Ok(act) = serde_json::from_value::<UndoAction>(a.clone()) {
                actions.push(act);
            }
        }
    } else if let Ok(act) = serde_json::from_value::<UndoAction>(entry.payload.clone()) {
        actions.push(act);
    }

    for act in actions {
        apply_action(conn, act)?;
    }
    Ok(Some(entry.label))
}

fn apply_action(conn: &Connection, act: UndoAction) -> Result<()> {
    let repo = Repo::new(conn);
    match act {
        UndoAction::RemoveTag { asset_id, tag_id } => {
            let aid = AssetId::parse(&asset_id)?;
            let tid = RowId::parse(&tag_id)?;
            repo.remove_asset_tag(&aid, &tid)
        }
        UndoAction::AddTag {
            asset_id,
            tag_id,
            tag_name,
        } => {
            let aid = AssetId::parse(&asset_id)?;
            let tid = RowId::parse(&tag_id)?;
            // Ensure the tag row still exists (it may have been left in place).
            if !tag_name.is_empty() {
                repo.upsert_tag(&tag_name)?;
            }
            repo.add_asset_tag(&aid, &tid)
        }
        UndoAction::RenameTag {
            tag_id,
            old_name,
            new_name: _,
        } => {
            let tid = RowId::parse(&tag_id)?;
            if old_name.is_empty() {
                return Ok(());
            }
            repo.rename_tag(&tid, &old_name).map(|_| ())
        }
        UndoAction::RestoreTag {
            tag_id,
            tag_name,
            asset_ids,
        } => {
            let tid = RowId::parse(&tag_id)?;
            // Recreate the tag row; upsert_tag mints a new id if missing.
            let tag = repo.upsert_tag(&tag_name)?;
            let live_id = if tag.id == tid {
                tid
            } else {
                // Name collision created a different id — use the new one.
                tag.id
            };
            for s in asset_ids {
                let aid = AssetId::parse(&s)?;
                repo.add_asset_tag(&aid, &live_id)?;
            }
            Ok(())
        }
        UndoAction::UnmergeTag {
            from_tag_id,
            from_tag_name,
            to_tag_id,
            asset_ids,
        } => {
            let from_id = RowId::parse(&from_tag_id)?;
            let to_id = RowId::parse(&to_tag_id)?;
            let tag = repo.upsert_tag(&from_tag_name)?;
            // Prefer the original id if the row still exists under that id.
            let live_from = if tag.id == from_id { from_id } else { tag.id };
            for s in asset_ids {
                let aid = AssetId::parse(&s)?;
                // These assets had the source tag; put it back and leave `to`.
                repo.add_asset_tag(&aid, &live_from)?;
                // Drop the merged-in tag from those that only got it via merge?
                // Keep both — merge undo restores the original label set for
                // assets that had `from`; assets that already had `to` stay.
                let _ = &to_id;
            }
            Ok(())
        }
        UndoAction::RemovePerson {
            asset_id,
            person_id,
        } => {
            let aid = AssetId::parse(&asset_id)?;
            let pid = RowId::parse(&person_id)?;
            repo.remove_asset_person(&aid, &pid)
        }
        UndoAction::AddPerson {
            asset_id,
            person_id,
            person_name,
        } => {
            let aid = AssetId::parse(&asset_id)?;
            let pid = RowId::parse(&person_id)?;
            if !person_name.is_empty() {
                // Re-create person with same id if it was purged — not supported;
                // people rows are not deleted on unlink, so just re-link.
                let exists: Option<String> = conn
                    .query_row("SELECT id FROM people WHERE id = ?1", [pid.as_str()], |r| {
                        r.get(0)
                    })
                    .optional()?;
                if exists.is_none() && !person_name.is_empty() {
                    // Fallback: create a new person (id will differ).
                    let p = repo.create_person(&person_name, None)?;
                    return repo.link_asset_person(&aid, &p.id);
                }
            }
            repo.link_asset_person(&aid, &pid)
        }
        UndoAction::RemoveFromCollection {
            asset_id,
            collection_id,
        } => {
            let aid = AssetId::parse(&asset_id)?;
            let cid = RowId::parse(&collection_id)?;
            repo.remove_asset_from_collection(&aid, &cid)
        }
        UndoAction::AddToCollection {
            asset_id,
            collection_id,
        } => {
            let aid = AssetId::parse(&asset_id)?;
            let cid = RowId::parse(&collection_id)?;
            repo.add_asset_collection(&aid, &cid)
        }
        UndoAction::Restore { asset_id } => {
            let aid = AssetId::parse(&asset_id)?;
            repo.restore_assets(&[aid]).map(|_| ())
        }
        UndoAction::SoftDelete { asset_id } => {
            let aid = AssetId::parse(&asset_id)?;
            repo.soft_delete_assets(&[aid]).map(|_| ())
        }
    }
}

/// Peek at what the next undo would reverse (for UI tooltip).
pub fn peek_label(conn: &Connection) -> Result<Option<String>> {
    Ok(Repo::new(conn).peek_undo()?.map(|e| e.label))
}

/// Hard error helper used by callers when undo is empty.
pub fn undo_last_or_err(conn: &Connection) -> Result<String> {
    match undo_last(conn)? {
        Some(label) => Ok(label),
        None => Err(Error::other("nothing to undo")),
    }
}
