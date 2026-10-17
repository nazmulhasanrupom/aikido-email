//! Saved mappings. The same headers (same order) get the saved mapping: no AI call.
use super::proposal::{MapTarget, TemplateColumn};
use crate::leads::fields::FieldDefs;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use uuid::Uuid;

/// SHA-256 of the headers (trimmed, lower case) in file order.
pub fn header_hash(headers: &[String]) -> String {
    let mut h = Sha256::new();
    for s in headers {
        let s = s.trim().to_lowercase();
        h.update((s.len() as u64).to_le_bytes());
        h.update(s.as_bytes());
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// A saved mapping fits when every custom field it names exists. A `new_field` whose key exists now (with the
/// same kind) becomes `custom`. `None` means: it does not fit.
pub fn fit(cols: Vec<TemplateColumn>, defs: &FieldDefs, ncols: usize) -> Option<Vec<TemplateColumn>> {
    let mut out = Vec::new();
    for mut c in cols {
        if c.index >= ncols {
            return None;
        }
        c.target = match c.target {
            MapTarget::Custom { key } if defs.by_key.contains_key(&key) => MapTarget::Custom { key },
            MapTarget::Custom { .. } => return None,
            MapTarget::NewField { key, field_kind, .. } => match defs.by_key.get(&key) {
                Some(f) if f.kind == field_kind => MapTarget::Custom { key },
                Some(_) => return None,
                None => MapTarget::NewField { key, label: None, field_kind },
            },
            t => t,
        };
        out.push(c);
    }
    Some(out)
}

/// The template of these headers, with its columns.
pub async fn find(conn: &mut PgConnection, hash: &str) -> Result<Option<(Uuid, Vec<TemplateColumn>)>, sqlx::Error> {
    let row: Option<(Uuid, Value)> = sqlx::query_as("SELECT id, mapping FROM import_mapping_templates WHERE header_hash = $1").bind(hash).fetch_optional(conn).await?;
    Ok(row.and_then(|(id, m)| serde_json::from_value(m).ok().map(|cols| (id, cols))))
}

pub async fn touch(conn: &mut PgConnection, id: Uuid, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE import_mapping_templates SET used_count = used_count + 1, last_used_at = $2 WHERE id = $1").bind(id).bind(now).execute(conn).await?;
    Ok(())
}

pub async fn save(conn: &mut PgConnection, headers: &[String], cols: &[TemplateColumn], now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO import_mapping_templates (id, header_hash, headers, mapping, created_at) VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (header_hash) DO UPDATE SET mapping = EXCLUDED.mapping, headers = EXCLUDED.headers",
    )
    .bind(Uuid::now_v7())
    .bind(header_hash(headers))
    .bind(serde_json::to_value(headers).unwrap_or_default())
    .bind(serde_json::to_value(cols).unwrap_or_default())
    .bind(now)
    .execute(conn)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_headers_same_hash_and_a_new_order_is_new() {
        let a = header_hash(&["Email".into(), "Org".into()]);
        assert_eq!(a, header_hash(&[" email ".into(), "ORG".into()]));
        assert_ne!(a, header_hash(&["Org".into(), "Email".into()]));
    }
}
