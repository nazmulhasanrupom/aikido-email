//! The lead timeline (`lead_events`). A failed write here is logged by the caller, never a reason to
//! block a send or a stop.
use super::model::LeadEvent;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

pub async fn log(conn: &mut PgConnection, lead_id: Uuid, kind: &str, ref_id: Option<Uuid>, detail: Value, at: DateTime<Utc>) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO lead_events (lead_id, kind, ref_id, detail, at) VALUES ($1, $2, $3, $4, $5)")
        .bind(lead_id)
        .bind(kind)
        .bind(ref_id)
        .bind(detail)
        .bind(at)
        .execute(conn)
        .await?;
    Ok(())
}

/// The same event for many leads, in one statement.
pub async fn log_many(conn: &mut PgConnection, lead_ids: &[Uuid], kind: &str, ref_id: Option<Uuid>, detail: Value, at: DateTime<Utc>) -> Result<(), sqlx::Error> {
    if lead_ids.is_empty() {
        return Ok(());
    }
    sqlx::query("INSERT INTO lead_events (lead_id, kind, ref_id, detail, at) SELECT unnest($1::uuid[]), $2, $3, $4, $5")
        .bind(lead_ids)
        .bind(kind)
        .bind(ref_id)
        .bind(detail)
        .bind(at)
        .execute(conn)
        .await?;
    Ok(())
}

pub const EVENT_COLS: &str = "id, lead_id, kind, ref_id, detail, at";

/// Newest first. `cursor` is the id of the last event of the previous page.
pub async fn list(conn: &mut PgConnection, lead_id: Uuid, cursor: Option<i64>, limit: i64) -> Result<Vec<LeadEvent>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {EVENT_COLS} FROM lead_events WHERE lead_id = $1 AND ($2::bigint IS NULL OR id < $2) ORDER BY id DESC LIMIT $3"
    ))
    .bind(lead_id)
    .bind(cursor)
    .bind(limit)
    .fetch_all(conn)
    .await
}
