use crate::error::Result;
use crate::ids::AssetId;

/// Structured filter used by the asset browser.
/// MVP: a few AND-ed conditions; OR/NOT come with Advanced Search later.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SearchQuery {
    /// Full-text query over FTS5 index.
    pub text: Option<String>,
    pub recorded_after: Option<chrono::DateTime<chrono::Utc>>,
    pub recorded_before: Option<chrono::DateTime<chrono::Utc>>,
    pub sample_rate: Option<u32>,
    /// Single tag name (legacy). Prefer `tags`.
    pub tag: Option<String>,
    /// Multiple tag names. Combined with `tags_all`.
    pub tags: Vec<String>,
    /// true = asset must carry EVERY tag (AND). false = ANY tag (OR).
    #[serde(default)]
    pub tags_all: bool,
    pub collection_id: Option<String>,
    pub person_id: Option<String>,
    pub limit: u32,
    pub offset: u32,
}

impl SearchQuery {
    pub fn text(q: impl Into<String>) -> Self {
        Self {
            text: Some(q.into()),
            limit: 50,
            ..Default::default()
        }
    }
}

/// Search assets. Text (FTS) and structured filters AND together in one SQL
/// statement so `LIMIT`/`OFFSET` paginate the true result set — never a
/// pre-truncated candidate list.
pub fn search(conn: &rusqlite::Connection, query: &SearchQuery) -> Result<Vec<AssetId>> {
    let limit = if query.limit == 0 { 50 } else { query.limit };
    let text = query.text.as_deref().filter(|t| !t.trim().is_empty());

    let mut sql = String::new();
    let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(raw) = text {
        sql.push_str(
            "SELECT assets_fts.asset_id FROM assets_fts \
             JOIN assets a ON a.id = assets_fts.asset_id \
             WHERE assets_fts MATCH ? AND a.status = 'ready' AND a.deleted_at IS NULL",
        );
        binds.push(Box::new(fts_query(raw)));
    } else {
        sql.push_str(
            "SELECT a.id FROM assets a WHERE a.status = 'ready' AND a.deleted_at IS NULL",
        );
    }

    if let Some(after) = query.recorded_after {
        sql.push_str(" AND a.recorded_at >= ?");
        binds.push(Box::new(after.to_rfc3339()));
    }
    if let Some(before) = query.recorded_before {
        sql.push_str(" AND a.recorded_at < ?");
        binds.push(Box::new(before.to_rfc3339()));
    }
    if let Some(sr) = query.sample_rate {
        sql.push_str(" AND a.sample_rate = ?");
        binds.push(Box::new(sr as i64));
    }
    // Tag filter: `tags` plus legacy `tag` (merged). AND = one EXISTS per tag;
    // OR = single EXISTS with IN (...).
    let mut tag_names: Vec<String> = query
        .tags
        .iter()
        .filter(|t| !t.trim().is_empty())
        .cloned()
        .collect();
    if let Some(tag) = &query.tag {
        if !tag.trim().is_empty() {
            tag_names.push(tag.clone());
        }
    }
    tag_names.sort();
    tag_names.dedup();
    if query.tags_all {
        for name in &tag_names {
            sql.push_str(
                " AND EXISTS (SELECT 1 FROM asset_tags at \
                              JOIN tags t ON t.id = at.tag_id \
                              WHERE at.asset_id = a.id AND t.name = ?)",
            );
            binds.push(Box::new(name.clone()));
        }
    } else if !tag_names.is_empty() {
        let placeholders = tag_names.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        sql.push_str(
            " AND EXISTS (SELECT 1 FROM asset_tags at \
                          JOIN tags t ON t.id = at.tag_id \
                          WHERE at.asset_id = a.id AND t.name IN (",
        );
        sql.push_str(&placeholders);
        sql.push_str("))");
        for name in &tag_names {
            binds.push(Box::new(name.clone()));
        }
    }
    if let Some(cid) = &query.collection_id {
        sql.push_str(
            " AND EXISTS (SELECT 1 FROM asset_collections ac \
                          WHERE ac.asset_id = a.id AND ac.collection_id = ?)",
        );
        binds.push(Box::new(cid.clone()));
    }
    if let Some(pid) = &query.person_id {
        sql.push_str(
            " AND EXISTS (SELECT 1 FROM asset_people ap \
                          WHERE ap.asset_id = a.id AND ap.person_id = ?)",
        );
        binds.push(Box::new(pid.clone()));
    }

    if text.is_some() {
        sql.push_str(" ORDER BY rank");
    } else {
        sql.push_str(" ORDER BY a.imported_at DESC");
    }
    sql.push_str(" LIMIT ? OFFSET ?");
    binds.push(Box::new(limit as i64));
    binds.push(Box::new(query.offset as i64));

    let mut stmt = conn.prepare(&sql)?;
    let params_refs: Vec<&dyn rusqlite::ToSql> = binds.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(params_refs.as_slice(), |r| r.get::<_, String>(0))?;

    let mut out = Vec::new();
    for row in rows {
        let s = row?;
        if let Ok(id) = AssetId::parse(&s) {
            out.push(id);
        }
    }
    Ok(out)
}

/// Prepare user text for FTS5. If it contains punctuation or operators,
/// wrap as a phrase so `a.wav` doesn't hit `fts5: syntax error`.
fn fts_query(raw: &str) -> String {
    let trimmed = raw.trim();
    let needs_phrase = trimmed
        .chars()
        .any(|c| !(c.is_alphanumeric() || c == '_' || c == ' '));
    if needs_phrase {
        format!("\"{}\"", trimmed.replace('"', "\"\""))
    } else {
        trimmed.to_string()
    }
}
