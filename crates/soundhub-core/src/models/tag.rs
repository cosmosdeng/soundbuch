use serde::{Deserialize, Serialize};

use crate::ids::RowId;

/// Flat labels describing an asset (`#ocean`, `#wind`). No hierarchy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub id: RowId,
    pub name: String,
}

/// Tag plus how many live assets carry it — for the tag manager UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagUsage {
    pub id: RowId,
    pub name: String,
    pub asset_count: u64,
}
