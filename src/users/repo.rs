//! Database access for users, sessions, invites, resets and the app state row.
use super::model::{USER_COLS, UserRow};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64URL};
use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use rand::rngs::OsRng;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

pub const IDLE_TIMEOUT_DAYS: i64 = 3;
pub const ABSOLUTE_TIMEOUT_DAYS: i64 = 30;
pub const INVITE_DAYS: i64 = 7;
pub const RESET_HOURS: i64 = 1;

pub fn sha256(text: &str) -> Vec<u8> {
    Sha256::digest(text.as_bytes()).to_vec()
}

/// 32 random bytes as base64url.
pub fn random_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    B64URL.encode(bytes)
}

pub fn clean_email(email: &str) -> String {
    email.trim().to_lowercase()
}

pub fn valid_email(email: &str) -> bool {
    let e = email.trim();
    if e.len() > 254 || e.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    match e.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty() && local.len() <= 64 && !domain.contains('@') && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
        }
        None => false,
    }
}

pub async fn count_users(pool: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM users").fetch_one(pool).await
}

pub async fn find_by_email(pool: &PgPool, email: &str) -> Result<Option<UserRow>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {USER_COLS} FROM users WHERE email = $1")).bind(clean_email(email)).fetch_optional(pool).await
}

pub async fn find(pool: &PgPool, id: Uuid) -> Result<Option<UserRow>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {USER_COLS} FROM users WHERE id = $1")).bind(id).fetch_optional(pool).await
}

pub async fn list(pool: &PgPool) -> Result<Vec<UserRow>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {USER_COLS} FROM users ORDER BY CASE role WHEN 'owner' THEN 0 WHEN 'admin' THEN 1 ELSE 2 END, created_at, id"
    ))
    .fetch_all(pool)
    .await
}

pub async fn insert_user<'e, E: sqlx::Executor<'e, Database = sqlx::Postgres>>(
    ex: E,
    email: &str,
    name: &str,
    password_hash: &str,
    role: &str,
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, email, name, password_hash, role) VALUES ($1, $2, $3, $4, $5)")
        .bind(id)
        .bind(clean_email(email))
        .bind(name.trim())
        .bind(password_hash)
        .bind(role)
        .execute(ex)
        .await?;
    Ok(id)
}

pub async fn set_password(pool: &PgPool, id: Uuid, hash: &str, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE users SET password_hash = $2, password_changed_at = $3, updated_at = $3 WHERE id = $1")
        .bind(id)
        .bind(hash)
        .bind(now)
        .execute(pool)
        .await?;
    Ok(())
}

/// Only swaps the hash (a rehash with new parameters). Does not count as a password change.
pub async fn replace_hash(pool: &PgPool, id: Uuid, hash: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE users SET password_hash = $2 WHERE id = $1").bind(id).bind(hash).execute(pool).await?;
    Ok(())
}

// ---------- sessions ----------

#[derive(Clone, FromRow)]
pub struct SessionRow {
    pub session_id: Uuid,
    pub user_id: Uuid,
    pub csrf_token_hash: Vec<u8>,
    pub last_seen_at: DateTime<Utc>,
}

pub async fn create_session(
    pool: &PgPool,
    user_id: Uuid,
    token: &str,
    csrf_hash: &[u8],
    ip: &str,
    user_agent: &str,
    now: DateTime<Utc>,
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO user_sessions (id, user_id, token_hash, csrf_token_hash, created_at, last_seen_at, expires_at, idle_expires_at, ip, user_agent)
         VALUES ($1, $2, $3, $4, $5, $5, $6, $7, $8, $9)",
    )
    .bind(id)
    .bind(user_id)
    .bind(sha256(token))
    .bind(csrf_hash)
    .bind(now)
    .bind(now + Duration::days(ABSOLUTE_TIMEOUT_DAYS))
    .bind(now + Duration::days(IDLE_TIMEOUT_DAYS))
    .bind(ip.chars().take(64).collect::<String>())
    .bind(user_agent.chars().take(200).collect::<String>())
    .execute(pool)
    .await?;
    Ok(id)
}

/// The live session for a cookie token, with its user. `None` if the token is unknown, expired,
/// idle for too long, or the user is disabled.
pub async fn lookup_session(pool: &PgPool, token: &str, now: DateTime<Utc>) -> Result<Option<(SessionRow, UserRow)>, sqlx::Error> {
    let Some(s): Option<SessionRow> = sqlx::query_as(
        "SELECT s.id AS session_id, s.user_id, s.csrf_token_hash, s.last_seen_at FROM user_sessions s
         WHERE s.token_hash = $1 AND s.expires_at > $2 AND s.idle_expires_at > $2",
    )
    .bind(sha256(token))
    .bind(now)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };
    match find(pool, s.user_id).await? {
        Some(u) if u.active() => Ok(Some((s, u))),
        _ => Ok(None),
    }
}

pub async fn touch_session(pool: &PgPool, id: Uuid, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE user_sessions SET last_seen_at = $2, idle_expires_at = $3 WHERE id = $1")
        .bind(id)
        .bind(now)
        .bind(now + Duration::days(IDLE_TIMEOUT_DAYS))
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete_session(pool: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM user_sessions WHERE id = $1").bind(id).execute(pool).await?.rows_affected() > 0)
}

pub async fn delete_other_sessions(pool: &PgPool, user_id: Uuid, keep: Option<Uuid>) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM user_sessions WHERE user_id = $1 AND ($2::uuid IS NULL OR id <> $2)")
        .bind(user_id)
        .bind(keep)
        .execute(pool)
        .await?
        .rows_affected())
}

#[derive(FromRow, serde::Serialize, utoipa::ToSchema)]
pub struct SessionInfo {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub ip: String,
    pub user_agent: String,
    #[sqlx(default)]
    pub current: bool,
}

pub async fn list_sessions(pool: &PgPool, user_id: Uuid, now: DateTime<Utc>) -> Result<Vec<SessionInfo>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, created_at, last_seen_at, expires_at, ip, user_agent, false AS current FROM user_sessions
         WHERE user_id = $1 AND expires_at > $2 AND idle_expires_at > $2 ORDER BY last_seen_at DESC",
    )
    .bind(user_id)
    .bind(now)
    .fetch_all(pool)
    .await
}

// ---------- app state ----------

#[derive(FromRow)]
pub struct AppStateRow {
    pub workspace_name: String,
    pub workspace_logo: Option<String>,
    pub onboarding: serde_json::Value,
    pub setup_code_hash: Option<Vec<u8>>,
    pub setup_done_at: Option<DateTime<Utc>>,
}

pub async fn app_state(pool: &PgPool) -> Result<AppStateRow, sqlx::Error> {
    sqlx::query_as("SELECT workspace_name, workspace_logo, onboarding, setup_code_hash, setup_done_at FROM app_state WHERE id = 1")
        .fetch_one(pool)
        .await
}

/// Delete what is not needed any more: ended sessions, old invites and resets, old rate-limit rows.
pub async fn cleanup(pool: &PgPool, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM user_sessions WHERE expires_at < $1 OR idle_expires_at < $1").bind(now).execute(pool).await?;
    sqlx::query("DELETE FROM user_invites WHERE expires_at < $1 - interval '30 days' OR used_at < $1 - interval '30 days'").bind(now).execute(pool).await?;
    sqlx::query("DELETE FROM user_resets WHERE expires_at < $1 - interval '1 day'").bind(now).execute(pool).await?;
    sqlx::query("DELETE FROM login_attempts WHERE window_start < $1 - interval '1 day' AND (locked_until IS NULL OR locked_until < $1)")
        .bind(now)
        .execute(pool)
        .await?;
    Ok(())
}
