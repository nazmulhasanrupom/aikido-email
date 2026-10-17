//! DB read and write for reply settings, `inbound_messages` and the sync state.
use super::settings::ReplySettings;
use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

pub async fn load_settings(conn: &mut PgConnection, account_id: Uuid) -> Result<ReplySettings, sqlx::Error> {
    let row: Option<(Value,)> = sqlx::query_as("SELECT settings FROM account_reply_settings WHERE account_id = $1")
        .bind(account_id)
        .fetch_optional(&mut *conn)
        .await?;
    match row {
        Some((v,)) => Ok(ReplySettings::from_stored(v)),
        None => load_defaults(conn).await,
    }
}

pub async fn load_defaults(conn: &mut PgConnection) -> Result<ReplySettings, sqlx::Error> {
    let (v,): (Value,) = sqlx::query_as("SELECT settings FROM reply_defaults WHERE id = 1").fetch_one(conn).await?;
    Ok(ReplySettings::from_stored(v))
}

pub async fn engine_paused(pool: &PgPool) -> Result<bool, sqlx::Error> {
    let (p,): (bool,) = sqlx::query_as("SELECT engine_paused FROM reply_defaults WHERE id = 1").fetch_one(pool).await?;
    Ok(p)
}

// ---------- sync state ----------

use crate::accounts::model::{ACCOUNT_COLS, EmailAccount, Provider};
use chrono::{DateTime, Duration, Utc};

/// How long a claim holds a sync row. A crashed sync is retried after this.
pub const LEASE: Duration = Duration::minutes(10);

#[derive(Clone, Debug, sqlx::FromRow)]
pub struct SyncState {
    pub account_id: Uuid,
    pub folder: String,
    pub cursor: Option<Value>,
    pub paused: bool,
    pub next_sync_at: Option<DateTime<Utc>>,
    pub sync_started_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub consecutive_errors: i32,
}

pub const SYNC_COLS: &str =
    "account_id, folder, cursor, paused, next_sync_at, sync_started_at, last_success_at, last_error, consecutive_errors";

/// The sync folders an account needs. Gmail has one mailbox, so `inbox` and `spam` become one row, `all`.
pub fn wanted_folders(provider: Provider, s: &ReplySettings) -> Vec<String> {
    if provider == Provider::Gmail {
        return vec!["all".into()];
    }
    s.folders.clone()
}

/// Make the sync rows an account needs, and remove the ones it does not need any more.
pub async fn ensure_sync_rows(conn: &mut PgConnection, account: &EmailAccount, s: &ReplySettings) -> Result<(), sqlx::Error> {
    let wanted = wanted_folders(account.provider, s);
    for f in &wanted {
        sqlx::query("INSERT INTO inbox_sync_state (account_id, folder) VALUES ($1, $2) ON CONFLICT DO NOTHING")
            .bind(account.id)
            .bind(f)
            .execute(&mut *conn)
            .await?;
    }
    sqlx::query("DELETE FROM inbox_sync_state WHERE account_id = $1 AND NOT (folder = ANY($2))")
        .bind(account.id)
        .bind(&wanted)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Accounts that can be read but have no sync rows yet (new accounts, or old accounts after the migration).
pub async fn accounts_without_sync_rows(pool: &PgPool) -> Result<Vec<EmailAccount>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {ACCOUNT_COLS} FROM email_accounts a
         WHERE a.can_read AND a.deleted_at IS NULL AND a.status <> 'disconnected'
           AND NOT EXISTS (SELECT 1 FROM inbox_sync_state s WHERE s.account_id = a.id)"
    ))
    .fetch_all(pool)
    .await
}

/// Accounts whose read access was never checked (connected before the Reply Check step).
pub async fn accounts_unchecked(pool: &PgPool) -> Result<Vec<EmailAccount>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {ACCOUNT_COLS} FROM email_accounts WHERE read_status IS NULL AND deleted_at IS NULL AND status <> 'disconnected'"
    ))
    .fetch_all(pool)
    .await
}

pub async fn set_read_access(
    pool: &PgPool,
    account_id: Uuid,
    scopes: Option<&[String]>,
    can_read: bool,
    status: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE email_accounts SET can_read = $2, read_status = $3, granted_scopes = COALESCE($4, granted_scopes), updated_at = now() WHERE id = $1",
    )
    .bind(account_id)
    .bind(can_read)
    .bind(status)
    .bind(scopes)
    .execute(pool)
    .await?;
    Ok(())
}

/// Claim due sync rows in one short transaction (`FOR UPDATE SKIP LOCKED`, so other server copies skip them).
/// A claimed row gets `sync_started_at = now` and `next_sync_at = now + 10 minutes` as a lease.
/// Due means: readable account that is not `disconnected`, reply check on, folder not paused,
/// `next_sync_at` empty or past, and at least one `sent` job in the last `match_window_days`.
pub async fn claim_due(pool: &PgPool, now: DateTime<Utc>, limit: i64) -> Result<Vec<(Uuid, String)>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "WITH due AS (
             SELECT s.account_id, s.folder
             FROM inbox_sync_state s
             JOIN email_accounts a ON a.id = s.account_id
             LEFT JOIN account_reply_settings rs ON rs.account_id = a.id
             CROSS JOIN reply_defaults rd
             WHERE a.can_read AND a.deleted_at IS NULL AND a.status <> 'disconnected'
               AND NOT s.paused
               AND COALESCE(rs.settings->>'enabled', rd.settings->>'enabled', 'true')::boolean
               AND (s.next_sync_at IS NULL OR s.next_sync_at <= $1)
               AND (EXISTS (
                   SELECT 1 FROM send_jobs j
                   WHERE j.account_id = a.id AND j.status = 'sent'
                     AND j.sent_at > $1 - make_interval(days => COALESCE(rs.settings->>'match_window_days', rd.settings->>'match_window_days', '30')::int))
                    -- A follow-up that waits for a fresh inbox read needs the sync to run.
                    OR EXISTS (SELECT 1 FROM send_jobs j WHERE j.account_id = a.id AND j.status = 'queued' AND j.in_reply_to IS NOT NULL))
             ORDER BY s.next_sync_at NULLS FIRST, s.account_id, s.folder
             LIMIT $2
             FOR UPDATE OF s SKIP LOCKED
         )
         UPDATE inbox_sync_state s SET sync_started_at = $1, next_sync_at = $1 + make_interval(secs => $3)
         FROM due WHERE s.account_id = due.account_id AND s.folder = due.folder
         RETURNING s.account_id, s.folder",
    )
    .bind(now)
    .bind(limit)
    .bind(LEASE.num_seconds() as f64)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

pub async fn load_sync_state(pool: &PgPool, account_id: Uuid, folder: &str) -> Result<Option<SyncState>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {SYNC_COLS} FROM inbox_sync_state WHERE account_id = $1 AND folder = $2"))
        .bind(account_id)
        .bind(folder)
        .fetch_optional(pool)
        .await
}

pub async fn save_cursor(pool: &PgPool, account_id: Uuid, folder: &str, cursor: &Value) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE inbox_sync_state SET cursor = $3 WHERE account_id = $1 AND folder = $2")
        .bind(account_id)
        .bind(folder)
        .bind(cursor)
        .execute(pool)
        .await?;
    Ok(())
}

/// Hold the lease longer while a long sync is still working.
pub async fn extend_lease(pool: &PgPool, account_id: Uuid, folder: &str, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE inbox_sync_state SET next_sync_at = $3 WHERE account_id = $1 AND folder = $2")
        .bind(account_id)
        .bind(folder)
        .bind(now + LEASE)
        .execute(pool)
        .await?;
    Ok(())
}

/// The sync worked: clear the errors, release the lease, and say when to look again.
pub async fn sync_ok(pool: &PgPool, account_id: Uuid, folder: &str, now: DateTime<Utc>, next: DateTime<Utc>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE inbox_sync_state SET last_success_at = $3, consecutive_errors = 0, last_error = NULL,
                sync_started_at = NULL, next_sync_at = $4 WHERE account_id = $1 AND folder = $2",
    )
    .bind(account_id)
    .bind(folder)
    .bind(now)
    .bind(next)
    .execute(pool)
    .await?;
    sqlx::query("UPDATE email_accounts SET read_status = 'ok' WHERE id = $1 AND can_read AND read_status IS DISTINCT FROM 'ok'")
        .bind(account_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// The sync failed: save the reason, release the lease, and say when to try again.
pub async fn sync_failed(
    pool: &PgPool,
    account_id: Uuid,
    folder: &str,
    error: &str,
    next: DateTime<Utc>,
    count_error: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE inbox_sync_state SET last_error = $3, sync_started_at = NULL, next_sync_at = $4,
                consecutive_errors = consecutive_errors + $5 WHERE account_id = $1 AND folder = $2",
    )
    .bind(account_id)
    .bind(folder)
    .bind(error)
    .bind(next)
    .bind(i32::from(count_error))
    .execute(pool)
    .await?;
    Ok(())
}
