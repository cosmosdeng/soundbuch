use serde::{Deserialize, Serialize};

use crate::ids::RowId;

/// Place associated with an asset. GPS fields here are the *recorded* values;
/// reverse-geocode fields are derived and must not overwrite them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Location {
    pub id: RowId,
    pub country: Option<String>,
    pub region: Option<String>,
    pub city: Option<String>,
    pub place: Option<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub altitude: Option<f64>,
    pub raw: Option<serde_json::Value>,
}
