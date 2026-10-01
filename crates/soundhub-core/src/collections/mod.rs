//! Smart-collection rule evaluation.
//!
//! Rules are stored as JSON on `collections.rules`. Evaluation compiles the
//! conditions into one SQL statement over `assets` (+ relation tables) so a
//! smart collection always reflects current DB state — never a stale membership
//! table.

use rusqlite::Connection;

use crate::error::Result;
use crate::ids::AssetId;
use crate::models::{MatchMode, SmartCondition, SmartField, SmartOp, SmartRules};

/// Evaluate smart rules and return matching ready-asset ids.
///
/// `match_mode = all` ANDs the conditions; `any` ORs them. An empty rule set
/// matches nothing (a brand-new smart collection starts empty).
pub fn evaluate(conn: &Connection, rules: &SmartRules) -> Result<Vec<AssetId>> {
    if rules.conditions.is_empty() {
        return Ok(Vec::new());
    }

    let mut sql = String::from(
        "SELECT a.id FROM assets a WHERE a.status = 'ready' AND a.deleted_at IS NULL AND (",
    );
    let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    let joiner = match rules.match_mode {
        MatchMode::All => " AND ",
        MatchMode::Any => " OR ",
    };

    for (i, cond) in rules.conditions.iter().enumerate() {
        if i > 0 {
            sql.push_str(joiner);
        }
        compile_condition(&mut sql, &mut binds, cond)?;
    }
    sql.push_str(") ORDER BY a.imported_at DESC");

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

fn compile_condition(
    sql: &mut String,
    binds: &mut Vec<Box<dyn rusqlite::ToSql>>,
    cond: &SmartCondition,
) -> Result<()> {
    let as_str = |v: &serde_json::Value| -> String {
        v.as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| v.to_string())
    };

    match (cond.field, cond.op) {
        (SmartField::Text, SmartOp::Contains | SmartOp::Is) => {
            let q = as_str(&cond.value);
            sql.push_str("a.id IN (SELECT asset_id FROM assets_fts WHERE assets_fts MATCH ?)");
            binds.push(Box::new(q));
        }
        (SmartField::Filename, SmartOp::Contains) => {
            sql.push_str("a.filename LIKE ?");
            binds.push(Box::new(format!("%{}%", as_str(&cond.value))));
        }
        (SmartField::Filename, SmartOp::Is) => {
            sql.push_str("a.filename = ?");
            binds.push(Box::new(as_str(&cond.value)));
        }
        (SmartField::Tag, SmartOp::Is) => {
            sql.push_str(
                "EXISTS (SELECT 1 FROM asset_tags at JOIN tags t ON t.id = at.tag_id \
                 WHERE at.asset_id = a.id AND t.name = ?)",
            );
            binds.push(Box::new(as_str(&cond.value)));
        }
        (SmartField::Tag, SmartOp::IsNot) => {
            sql.push_str(
                "NOT EXISTS (SELECT 1 FROM asset_tags at JOIN tags t ON t.id = at.tag_id \
                 WHERE at.asset_id = a.id AND t.name = ?)",
            );
            binds.push(Box::new(as_str(&cond.value)));
        }
        (SmartField::Person, SmartOp::Is) => {
            sql.push_str(
                "EXISTS (SELECT 1 FROM asset_people ap JOIN people p ON p.id = ap.person_id \
                 WHERE ap.asset_id = a.id AND p.name = ?)",
            );
            binds.push(Box::new(as_str(&cond.value)));
        }
        (SmartField::Person, SmartOp::IsNot) => {
            sql.push_str(
                "NOT EXISTS (SELECT 1 FROM asset_people ap JOIN people p ON p.id = ap.person_id \
                 WHERE ap.asset_id = a.id AND p.name = ?)",
            );
            binds.push(Box::new(as_str(&cond.value)));
        }
        (SmartField::Collection, SmartOp::Is) => {
            sql.push_str(
                "EXISTS (SELECT 1 FROM asset_collections ac \
                 WHERE ac.asset_id = a.id AND ac.collection_id = ?)",
            );
            binds.push(Box::new(as_str(&cond.value)));
        }
        (SmartField::SampleRate, SmartOp::Eq) => {
            sql.push_str("a.sample_rate = ?");
            binds.push(Box::new(cond.value.as_i64().unwrap_or(0)));
        }
        (SmartField::SampleRate, SmartOp::Gte) => {
            sql.push_str("a.sample_rate >= ?");
            binds.push(Box::new(cond.value.as_i64().unwrap_or(0)));
        }
        (SmartField::SampleRate, SmartOp::Lte) => {
            sql.push_str("a.sample_rate <= ?");
            binds.push(Box::new(cond.value.as_i64().unwrap_or(0)));
        }
        (SmartField::RecordedAfter, SmartOp::Gte | SmartOp::Eq) => {
            sql.push_str("a.recorded_at >= ?");
            binds.push(Box::new(as_str(&cond.value)));
        }
        (SmartField::RecordedBefore, SmartOp::Lte | SmartOp::Eq) => {
            sql.push_str("a.recorded_at < ?");
            binds.push(Box::new(as_str(&cond.value)));
        }
        (SmartField::HasGps, SmartOp::Eq) => {
            let want = cond.value.as_bool().unwrap_or(true);
            if want {
                sql.push_str("a.latitude IS NOT NULL AND a.longitude IS NOT NULL");
            } else {
                sql.push_str("a.latitude IS NULL OR a.longitude IS NULL");
            }
        }
        _ => {
            // Unsupported field/op pair — treat as never-match rather than
            // silently widening the result set.
            sql.push('0');
        }
    }
    Ok(())
}

/// Parse a rules JSON value into `SmartRules`.
pub fn parse_rules(raw: &serde_json::Value) -> Option<SmartRules> {
    serde_json::from_value(raw.clone()).ok()
}

/// Serialize rules to the JSON stored in `collections.rules`.
pub fn rules_to_value(rules: &SmartRules) -> serde_json::Value {
    serde_json::to_value(rules).unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_accept_match_alias_and_roundtrip() {
        // Frontend sends `match`; the Rust field is `match_mode`.
        let raw = serde_json::json!({
            "match": "any",
            "conditions": [
                {"field": "tag", "op": "is", "value": "ocean"},
                {"field": "sample_rate", "op": "gte", "value": 48000},
                {"field": "has_gps", "op": "eq", "value": true}
            ]
        });
        let rules = parse_rules(&raw).expect("parse");
        assert_eq!(rules.match_mode, MatchMode::Any);
        assert_eq!(rules.conditions.len(), 3);
        assert_eq!(rules.conditions[0].field, SmartField::Tag);

        let back = rules_to_value(&rules);
        let again = parse_rules(&back).expect("roundtrip");
        assert_eq!(rules, again);
    }

    #[test]
    fn empty_rules_match_nothing() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::schema::migrate(&conn).unwrap();
        let rules = SmartRules {
            match_mode: MatchMode::All,
            conditions: vec![],
        };
        assert!(evaluate(&conn, &rules).unwrap().is_empty());
    }
}
