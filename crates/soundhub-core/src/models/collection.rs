use serde::{Deserialize, Serialize};

use crate::ids::RowId;

/// Virtual grouping. An asset may belong to any number of collections;
/// the physical file is never duplicated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collection {
    pub id: RowId,
    pub name: String,
    pub parent_id: Option<RowId>,
    pub collection_type: CollectionType,
    /// Smart-collection rules (JSON). Unused for `Static`.
    pub rules: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionType {
    Static,
    Smart,
}

impl CollectionType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Smart => "smart",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "static" => Some(Self::Static),
            "smart" => Some(Self::Smart),
            _ => None,
        }
    }
}

/// How conditions combine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MatchMode {
    #[default]
    All,
    Any,
}

/// Field a smart-collection condition tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmartField {
    /// Full-text over filename / path / tags / people / collections / metadata.
    Text,
    Filename,
    Tag,
    Person,
    Collection,
    SampleRate,
    RecordedAfter,
    RecordedBefore,
    HasGps,
}

/// Comparison for a smart-collection condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmartOp {
    Contains,
    Is,
    IsNot,
    Eq,
    Neq,
    Gte,
    Lte,
}

/// One condition: `field` `op` `value`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SmartCondition {
    pub field: SmartField,
    pub op: SmartOp,
    pub value: serde_json::Value,
}

/// Rule set stored in `collections.rules` for smart collections.
///
/// Example:
/// ```json
/// { "match": "all", "conditions": [
///   { "field": "tag", "op": "is", "value": "ocean" },
///   { "field": "sample_rate", "op": "gte", "value": 48000 }
/// ] }
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SmartRules {
    #[serde(default, rename = "match", alias = "match_mode")]
    pub match_mode: MatchMode,
    pub conditions: Vec<SmartCondition>,
}

impl SmartRules {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".into())
    }

    pub fn from_json(s: &str) -> Option<Self> {
        serde_json::from_str(s).ok()
    }
}
