//! Verify on import, and the short "latest result" the lead tables show.
use super::settings::VerifySettings;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use sqlx::PgConnection;
use std::collections::HashMap;
use utoipa::ToSchema;
use uuid::Uuid;

/// Called by `upsert_batch` with the ids of the leads a CSV import, the API or a manual add touched, inside its transaction.
/// Does nothing when the setting is off or Reoon is not installed. Leads that have no fresh result (see `reuse_days`)
/// go into the open auto run, or into a new one. This covers leads that already existed (an import in "skip" mode
/// does not change them, but they still need a result before they send). A run is made only when there is something to check.
pub async fn enqueue_new(conn: &mut PgConnection, lead_ids: &[Uuid], now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    if lead_ids.is_empty() {
        return Ok(());
    }
    let (stored,): (serde_json::Value,) = sqlx::query_as("SELECT settings FROM verify_defaults WHERE id = 1").fetch_one(&mut *conn).await?;
    let cfg = VerifySettings::from_stored(stored);
    if !cfg.auto_verify {
        return Ok(());
    }
    let active: Option<(i32,)> = sqlx::query_as("SELECT 1 FROM integrations WHERE app = 'reoon' AND status = 'installed'").fetch_optional(&mut *conn).await?;
    if active.is_none() {
        return Ok(());
    }
    // a result newer than `fresh_after` is reused; with reuse_days = 0 nothing is fresh
    let fresh_after = if cfg.reuse_days > 0 { now - Duration::days(cfg.reuse_days as i64) } else { now + Duration::days(36_500) };
    let todo: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT l.id FROM leads l WHERE l.id = ANY($1)
            AND NOT EXISTS (SELECT 1 FROM email_verifications v WHERE v.lead_id = l.id AND v.provider = 'reoon' AND v.email = l.email::text AND v.checked_at > $2)",
    )
    .bind(lead_ids)
    .bind(fresh_after)
    .fetch_all(&mut *conn)
    .await?;
    if todo.is_empty() {
        return Ok(());
    }
    let todo: Vec<Uuid> = todo.into_iter().map(|t| t.0).collect();
    let open: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM verification_runs WHERE auto AND status IN ('queued', 'running') ORDER BY created_at DESC LIMIT 1 FOR UPDATE").fetch_optional(&mut *conn).await?;
    let run_id = match open {
        Some((id,)) => id,
        None => {
            let id = Uuid::now_v7();
            sqlx::query("INSERT INTO verification_runs (id, mode, auto, created_at) VALUES ($1, $2, true, $3)").bind(id).bind(cfg.mode.as_str()).bind(now).execute(&mut *conn).await?;
            id
        }
    };
    let made = sqlx::query("INSERT INTO verification_items (run_id, lead_id, email) SELECT $1, id, email::text FROM leads WHERE id = ANY($2) ON CONFLICT DO NOTHING")
        .bind(run_id)
        .bind(&todo)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    sqlx::query("UPDATE verification_runs SET total = total + $2 WHERE id = $1").bind(run_id).bind(made as i32).execute(&mut *conn).await?;
    Ok(())
}

/// The latest Reoon result of a lead, for the lead tables and the drawer.
#[derive(Serialize, ToSchema, Clone, Debug, sqlx::FromRow)]
pub struct Brief {
    #[serde(skip)]
    pub lead_id: Uuid,
    pub status: String,
    pub score: Option<i32>,
    pub checked_at: DateTime<Utc>,
}

/// Results that still match the current address of the lead.
pub async fn briefs(conn: &mut PgConnection, lead_ids: &[Uuid]) -> Result<HashMap<Uuid, Brief>, sqlx::Error> {
    let rows: Vec<Brief> = sqlx::query_as(
        "SELECT v.lead_id, v.status, v.score, v.checked_at FROM email_verifications v JOIN leads l ON l.id = v.lead_id
          WHERE v.provider = 'reoon' AND v.lead_id = ANY($1) AND v.email = l.email::text",
    )
    .bind(lead_ids)
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(|b| (b.lead_id, b)).collect())
}

/// SQL for "status is in this list (`none` = no result)". `$n` is a `text[]` or null. `l` is the leads alias.
pub fn status_cond(n: usize) -> String {
    let latest = "SELECT 1 FROM email_verifications v WHERE v.lead_id = l.id AND v.provider = 'reoon' AND v.email = l.email::text";
    format!("(${n}::text[] IS NULL OR EXISTS ({latest} AND v.status = ANY(${n})) OR ('none' = ANY(${n}) AND NOT EXISTS ({latest})))")
}

/// Split `safe,none` into a list. Empty text gives `None`.
pub fn parse_statuses(text: Option<&str>) -> Option<Vec<String>> {
    let list: Vec<String> = text?.split(',').map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()).collect();
    (!list.is_empty()).then_some(list)
}
