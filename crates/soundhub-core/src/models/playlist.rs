use serde::{Deserialize, Serialize};

use crate::ids::{AssetId, RowId};

/// Ordered track list. Unlike Collections (unordered virtual groups),
/// a Playlist has a deliberate sequence for playback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Playlist {
    pub id: RowId,
    pub name: String,
    pub created_at: String,
    pub track_count: u64,
}

/// One entry in a playlist, with its 0-based position.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistTrack {
    pub position: u32,
    pub asset_id: AssetId,
    pub filename: String,
    pub duration_ms: Option<u64>,
    pub file_size: u64,
}
