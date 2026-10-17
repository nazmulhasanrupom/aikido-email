//! The bounce-rate guard. Reads `bounced_at` and `bounce_kind` on the last `bounce_window` sent jobs.
//! Two paths set those columns, and each bounce counts once:
//! a send-time hard bounce (`queue::apply_plan`) and a late bounce read from the inbox (`replies::actions`).
use super::queue::{log_event, pause_state};
use super::rules;
use super::settings::SendingSettings;
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::PgConnection;
use uuid::Uuid;

/// Why the account should pause, if the bounce rate is over the limit. Pure read.
/// Looks at the last `bounce_window` jobs that were sent or bounced. Counts `hard` bounces,
/// plus `block` bounces when `count_blocks` is true. Needs at least 20 sends (see `rules::bounce_rate_exceeded`).
pub async fn bounce_pause_reason(
    conn: &mut PgConnection,
    account_id: Uuid,
    s: &SendingSettings,
    count_blocks: bool,
) -> Result<Option<String>, sqlx::Error> {
    let rows: Vec<(Option<String>,)> = sqlx::query_as(
        "SELECT bounce_kind::text FROM send_jobs
         WHERE account_id = $1 AND (status = 'sent' OR bounced_at IS NOT NULL)
         ORDER BY COALESCE(sent_at, bounced_at) DESC, id DESC LIMIT $2",
    )
    .bind(account_id)
    .bind(s.bounce_window as i64)
    .fetch_all(conn)
    .await?;
    let total = rows.len() as u32;
    let bounces = rows
        .iter()
        .filter(|r| match r.0.as_deref() {
            Some("hard") => true,
            Some("block") => count_blocks,
            _ => false,
        })
        .count() as u32;
    Ok(rules::bounce_rate_exceeded(s, total, bounces).then(|| {
        format!(
            "bounce_rate: Bounce rate too high: {bounces} bounces in the last {total} sends (limit {}%).",
            s.pause_bounce_rate_pct
        )
    }))
}

/// Pause the account and log it. Does nothing if it is already paused.
pub async fn pause_for_reason(
    conn: &mut PgConnection,
    account_id: Uuid,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let (paused,): (bool,) = sqlx::query_as("SELECT paused FROM account_send_state WHERE account_id = $1 FOR UPDATE")
        .bind(account_id)
        .fetch_one(&mut *conn)
        .await?;
    if paused {
        return Ok(false);
    }
    sqlx::query("UPDATE email_accounts SET status = 'error', last_error = $2, updated_at = $3 WHERE id = $1 AND status = 'active'")
        .bind(account_id)
        .bind(reason)
        .bind(now)
        .execute(&mut *conn)
        .await?;
    pause_state(conn, account_id, reason, now).await?;
    log_event(conn, None, account_id, "paused", json!({ "reason": reason }), now).await?;
    Ok(true)
}

/// Look at the bounce rate of one account again and pause it if it is too high.
/// Call it after a late bounce was written. Returns the pause reason if the account was paused now.
pub async fn recheck(conn: &mut PgConnection, account_id: Uuid, now: DateTime<Utc>) -> Result<Option<String>, sqlx::Error> {
    let s = super::queue::load_settings(conn, account_id).await?;
    let count_blocks = crate::replies::repo::load_settings(conn, account_id).await?.count_blocks_in_bounce_rate;
    let Some(reason) = bounce_pause_reason(conn, account_id, &s, count_blocks).await? else {
        return Ok(None);
    };
    Ok(pause_for_reason(conn, account_id, &reason, now).await?.then_some(reason))
}
