use serde::{Deserialize, Serialize};

/// What a logged action did — used to pick the reverse operation on undo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UndoKind {
    AddTag,
    RemoveTag,
    RenameTag,
    DeleteTag,
    MergeTags,
    AddPerson,
    RemovePerson,
    AddToCollection,
    RemoveFromCollection,
    SoftDelete,
    Restore,
    /// Bundle of reverse actions applied in order (batch edit).
    Batch,
}

impl UndoKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AddTag => "add_tag",
            Self::RemoveTag => "remove_tag",
            Self::RenameTag => "rename_tag",
            Self::DeleteTag => "delete_tag",
            Self::MergeTags => "merge_tags",
            Self::AddPerson => "add_person",
            Self::RemovePerson => "remove_person",
            Self::AddToCollection => "add_to_collection",
            Self::RemoveFromCollection => "remove_from_collection",
            Self::SoftDelete => "soft_delete",
            Self::Restore => "restore",
            Self::Batch => "batch",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "add_tag" => Some(Self::AddTag),
            "remove_tag" => Some(Self::RemoveTag),
            "rename_tag" => Some(Self::RenameTag),
            "delete_tag" => Some(Self::DeleteTag),
            "merge_tags" => Some(Self::MergeTags),
            "add_person" => Some(Self::AddPerson),
            "remove_person" => Some(Self::RemovePerson),
            "add_to_collection" => Some(Self::AddToCollection),
            "remove_from_collection" => Some(Self::RemoveFromCollection),
            "soft_delete" => Some(Self::SoftDelete),
            "restore" => Some(Self::Restore),
            "batch" => Some(Self::Batch),
            _ => None,
        }
    }
}

/// One reversible action stored in `undo_log`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoEntry {
    pub id: String,
    pub created_at: String,
    pub kind: UndoKind,
    pub label: String,
    /// JSON describing the reverse operation(s).
    pub payload: serde_json::Value,
}

/// Single reverse step inside an undo payload (also used for batch).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UndoAction {
    /// Undo of `add_tag` → remove the tag.
    RemoveTag {
        asset_id: String,
        tag_id: String,
    },
    /// Undo of `remove_tag` → re-link the existing tag.
    AddTag {
        asset_id: String,
        tag_id: String,
        tag_name: String,
    },
    /// Undo of `rename_tag` → rename back.
    RenameTag {
        tag_id: String,
        old_name: String,
        new_name: String,
    },
    /// Undo of `delete_tag` → recreate tag + relink (carries asset list).
    RestoreTag {
        tag_id: String,
        tag_name: String,
        asset_ids: Vec<String>,
    },
    /// Undo of `merge_tags` → split back: re-create source tag + relink.
    UnmergeTag {
        from_tag_id: String,
        from_tag_name: String,
        to_tag_id: String,
        asset_ids: Vec<String>,
    },
    RemovePerson {
        asset_id: String,
        person_id: String,
    },
    AddPerson {
        asset_id: String,
        person_id: String,
        person_name: String,
    },
    RemoveFromCollection {
        asset_id: String,
        collection_id: String,
    },
    AddToCollection {
        asset_id: String,
        collection_id: String,
    },
    Restore {
        asset_id: String,
    },
    SoftDelete {
        asset_id: String,
    },
}

impl UndoAction {
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}
