use serde::{Deserialize, Serialize};

use crate::ids::RowId;

/// People are first-class entities — never a comma-separated string.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Person {
    pub id: RowId,
    pub name: String,
    pub note: Option<String>,
    pub metadata: Option<serde_json::Value>,
}
