//! Reads and writes of the `integrations` table, and the decrypted client of an installed app.
use crate::state::AppState;
use crate::verify_reoon::client::EmailVerifier;
use chrono::{DateTime, Utc};
use secrecy::SecretString;
use serde::Serialize;
use sqlx::{FromRow, PgPool};
use std::sync::Arc;
use utoipa::ToSchema;
use uuid::Uuid;

/// An app the code knows. `sidebar`: the web app shows a menu item when it is installed.
pub struct CatalogApp {
    pub app: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub sidebar: bool,
}

pub const CATALOG: &[CatalogApp] = &[CatalogApp {
    app: "reoon",
    name: "Reoon Email Verifier",
    description: "Check if lead emails exist before you send.",
    sidebar: true,
}];

pub fn catalog_app(app: &str) -> Option<&'static CatalogApp> {
    CATALOG.iter().find(|a| a.app == app)
}

#[derive(Clone, Debug, FromRow)]
pub struct Row {
    pub id: Uuid,
    pub app: String,
    pub status: String,
    pub settings: serde_json::Value,
    pub installed_by: Option<Uuid>,
    pub installed_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const COLS: &str = "id, app, status, settings, installed_by, installed_at, updated_at";

pub async fn get(pool: &PgPool, app: &str) -> Result<Option<Row>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {COLS} FROM integrations WHERE app = $1")).bind(app).fetch_optional(pool).await
}

pub async fn list(pool: &PgPool) -> Result<Vec<Row>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {COLS} FROM integrations ORDER BY installed_at")).fetch_all(pool).await
}

/// `true` when the app is installed and its key works (the send rule and the worker need this).
pub async fn is_active(pool: &PgPool, app: &str) -> Result<bool, sqlx::Error> {
    let r: Option<(String,)> = sqlx::query_as("SELECT status FROM integrations WHERE app = $1").bind(app).fetch_optional(pool).await?;
    Ok(matches!(r, Some((s,)) if s == "installed"))
}

pub async fn set_status(pool: &PgPool, id: Uuid, status: &str, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE integrations SET status = $2, updated_at = $3 WHERE id = $1 AND status <> $2").bind(id).bind(status).bind(now).execute(pool).await?;
    Ok(())
}

/// Called when Reoon refused the key.
pub async fn mark_auth_failed(state: &AppState, id: Uuid) {
    match set_status(&state.pool, id, "auth_failed", state.now()).await {
        Ok(()) => tracing::error!(integration_id = %id, "Reoon refused the key: the verifier is stopped until the key is replaced"),
        Err(e) => tracing::error!(integration_id = %id, error = %e, "could not mark the integration as auth_failed"),
    }
}

pub enum LoadError {
    NotInstalled,
    /// The saved key can not be decrypted (a wrong master key, or a damaged row).
    BadKey,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for LoadError {
    fn from(e: sqlx::Error) -> Self {
        LoadError::Db(e)
    }
}

/// The row id, its status and a client with the decrypted key.
pub async fn load_client(state: &AppState, app: &str) -> Result<(Uuid, String, Arc<dyn EmailVerifier>), LoadError> {
    let row: Option<(Uuid, String, Vec<u8>, Vec<u8>, i32)> =
        sqlx::query_as("SELECT id, status, secret_ciphertext, secret_nonce, key_version FROM integrations WHERE app = $1").bind(app).fetch_optional(&state.pool).await?;
    let Some((id, status, ct, nonce, version)) = row else { return Err(LoadError::NotInstalled) };
    let plain = state.crypto.decrypt(&ct, &nonce, version, id.as_bytes()).map_err(|_| LoadError::BadKey)?;
    let key = String::from_utf8(plain.to_vec()).map_err(|_| LoadError::BadKey)?;
    Ok((id, status, state.verifier.build(SecretString::from(key))))
}

#[derive(Serialize, ToSchema)]
pub struct AppInfo {
    /// `reoon`
    pub app: String,
    pub name: String,
    pub description: String,
    /// The web app shows a menu item for this app when it is installed.
    pub sidebar: bool,
    pub installed: bool,
    /// `installed`, `auth_failed` or `disabled`. Null when not installed.
    pub status: Option<String>,
    pub installed_at: Option<DateTime<Utc>>,
}

pub fn info(app: &CatalogApp, row: Option<&Row>) -> AppInfo {
    AppInfo {
        app: app.app.into(),
        name: app.name.into(),
        description: app.description.into(),
        sidebar: app.sidebar,
        installed: row.is_some(),
        status: row.map(|r| r.status.clone()),
        installed_at: row.map(|r| r.installed_at),
    }
}
