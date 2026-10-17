//! The store of LLM providers. The API key is encrypted with AES-256-GCM (AAD = the provider id), is write only,
//! and is never returned or logged.
use super::llm::{ProviderConfig, ProviderKind};
use crate::crypto::Crypto;
use chrono::{DateTime, Utc};
use secrecy::SecretString;
use serde::Serialize;
use sqlx::{FromRow, PgConnection, PgPool};
use utoipa::ToSchema;
use uuid::Uuid;

pub const COLS: &str = "id, kind, name, base_url, model, (api_key_ciphertext IS NOT NULL) AS has_api_key, status, max_concurrency, timeout_secs, json_mode, created_at, updated_at";

#[derive(Clone, Debug, FromRow, Serialize, ToSchema)]
pub struct Provider {
    pub id: Uuid,
    /// `anthropic` or `openai_compatible`.
    pub kind: String,
    pub name: String,
    pub base_url: String,
    pub model: String,
    /// The key is write only. This only says that one is saved.
    pub has_api_key: bool,
    /// `ok`, or `auth_failed` (the provider said 401 or 403; the generator is paused until the key is replaced or a test succeeds).
    pub status: String,
    pub max_concurrency: i32,
    pub timeout_secs: i32,
    /// Ask the server for JSON mode (`response_format`). Only for `openai_compatible`.
    pub json_mode: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Provider>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {COLS} FROM llm_providers WHERE id = $1")).bind(id).fetch_optional(conn).await
}

pub async fn list(conn: &mut PgConnection) -> Result<Vec<Provider>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {COLS} FROM llm_providers ORDER BY created_at, id")).fetch_all(conn).await
}

pub async fn set_status(pool: &PgPool, id: Uuid, status: &str, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE llm_providers SET status = $2, updated_at = $3 WHERE id = $1 AND status <> $2").bind(id).bind(status).bind(now).execute(pool).await?;
    Ok(())
}

/// Called when a provider answers 401 or 403.
pub async fn mark_auth_failed(pool: &PgPool, id: Uuid, now: DateTime<Utc>) {
    match set_status(pool, id, "auth_failed", now).await {
        Ok(()) => tracing::error!(provider_id = %id, "LLM provider refused the key (401/403): the generator is paused until the key is replaced"),
        Err(e) => tracing::error!(provider_id = %id, error = %e, "could not mark the provider as auth_failed"),
    }
}

/// Encrypt a key for the database: (ciphertext, nonce, key version).
pub fn seal_key(crypto: &Crypto, id: Uuid, key: &str) -> Result<(Vec<u8>, Vec<u8>, i32), crate::crypto::CryptoError> {
    let sealed = crypto.encrypt(key.as_bytes(), id.as_bytes())?;
    Ok((sealed.ciphertext, sealed.nonce, sealed.key_version))
}

#[derive(Debug)]
pub enum ConfigError {
    NotFound,
    /// The saved key can not be decrypted (a wrong master key, or a damaged row).
    BadKey,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for ConfigError {
    fn from(e: sqlx::Error) -> Self {
        ConfigError::Db(e)
    }
}

/// Everything needed to call a provider. The key is decrypted here, just before the call.
pub async fn load_config(pool: &PgPool, crypto: &Crypto, id: Uuid, max_attempts: u32) -> Result<(ProviderConfig, String), ConfigError> {
    #[allow(clippy::type_complexity)]
    let row: Option<(String, String, String, Option<Vec<u8>>, Option<Vec<u8>>, Option<i32>, String, i32, bool)> = sqlx::query_as(
        "SELECT kind, base_url, model, api_key_ciphertext, api_key_nonce, key_version, status, timeout_secs, json_mode FROM llm_providers WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    let Some((kind, base_url, model, ct, nonce, version, status, timeout_secs, json_mode)) = row else { return Err(ConfigError::NotFound) };
    let kind = ProviderKind::parse(&kind).ok_or(ConfigError::BadKey)?;
    let api_key = match (ct, nonce, version) {
        (Some(ct), Some(nonce), Some(v)) => {
            let plain = crypto.decrypt(&ct, &nonce, v, id.as_bytes()).map_err(|_| ConfigError::BadKey)?;
            let text = String::from_utf8(plain.to_vec()).map_err(|_| ConfigError::BadKey)?;
            Some(SecretString::from(text))
        }
        _ => None,
    };
    Ok((ProviderConfig { id, kind, base_url, model, api_key, timeout_secs: timeout_secs as u64, json_mode, max_attempts }, status))
}
