//! API keys: make, hash, check, rotate. Only the SHA-256 hash is stored.
//! SHA-256 is fine here because the keys are long and random.
use crate::config::Config;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64URL};
use chrono::{DateTime, Utc};
use rand::RngCore;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use subtle::ConstantTimeEq;
use utoipa::ToSchema;
use uuid::Uuid;

/// One scope per step. Add a variant here (and to `ALL`) for a later step.
/// The bootstrap makes one key for every scope in `ALL` that has no active key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Admin,
    Accounts,
    Sending,
    Replies,
    Followups,
    Leads,
    Campaigns,
    Generator,
    Integrations,
}

impl Scope {
    pub const ALL: &'static [Scope] = &[Scope::Admin, Scope::Accounts, Scope::Sending, Scope::Replies, Scope::Followups, Scope::Leads, Scope::Campaigns, Scope::Generator, Scope::Integrations];

    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Admin => "admin",
            Scope::Accounts => "accounts",
            Scope::Sending => "sending",
            Scope::Replies => "replies",
            Scope::Followups => "followups",
            Scope::Leads => "leads",
            Scope::Campaigns => "campaigns",
            Scope::Generator => "generator",
            Scope::Integrations => "integrations",
        }
    }

    pub fn parse(s: &str) -> Option<Scope> {
        Scope::ALL.iter().copied().find(|x| x.as_str() == s)
    }

    pub fn env_var(self) -> String {
        format!("AIKIDO_{}_API_KEY", self.as_str().to_uppercase())
    }

    /// `admin` passes every scope.
    pub fn allows(self, required: Scope) -> bool {
        self == Scope::Admin || self == required
    }
}

pub fn hash_key(key: &str) -> Vec<u8> {
    Sha256::digest(key.as_bytes()).to_vec()
}

/// Returns (plain key, key_prefix, hash).
/// The prefix is `aik_<scope>_` plus 6 random characters, so people can tell keys apart.
pub fn generate_key(scope: Scope) -> (String, String, Vec<u8>) {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let random = B64URL.encode(bytes);
    let key = format!("aik_{}_{}", scope.as_str(), random);
    let prefix = format!("aik_{}_{}", scope.as_str(), &random[..6]);
    let hash = hash_key(&key);
    (key, prefix, hash)
}

#[derive(Debug, Clone, FromRow, Serialize, ToSchema)]
pub struct KeyInfo {
    pub id: Uuid,
    pub name: String,
    pub scope: String,
    pub key_prefix: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

const KEY_COLS: &str = "id, name, scope, key_prefix, created_at, last_used_at, revoked_at";

/// A key counts as active until `revoked_at`. A future `revoked_at` is a grace period.
const ACTIVE: &str = "(revoked_at IS NULL OR revoked_at > now())";

pub struct Authenticated {
    pub id: Uuid,
    pub scope: Scope,
}

pub enum AuthError {
    Invalid,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for AuthError {
    fn from(e: sqlx::Error) -> Self {
        AuthError::Db(e)
    }
}

/// Hash the key, look up the hash, check it is not revoked. Compare in constant time.
pub async fn authenticate(pool: &PgPool, key: &str) -> Result<Authenticated, AuthError> {
    let hash = hash_key(key);
    let row: Option<(Uuid, String, Vec<u8>, Option<DateTime<Utc>>)> =
        sqlx::query_as("SELECT id, scope, key_hash, revoked_at FROM api_keys WHERE key_hash = $1")
            .bind(&hash)
            .fetch_optional(pool)
            .await?;
    let Some((id, scope, stored, revoked_at)) = row else {
        return Err(AuthError::Invalid);
    };
    if !bool::from(stored.ct_eq(&hash)) {
        return Err(AuthError::Invalid);
    }
    if revoked_at.is_some_and(|t| t <= Utc::now()) {
        return Err(AuthError::Invalid);
    }
    let scope = Scope::parse(&scope).ok_or(AuthError::Invalid)?;
    Ok(Authenticated { id, scope })
}

pub async fn touch_last_used(pool: &PgPool, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE api_keys SET last_used_at = now() WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn list(pool: &PgPool) -> Result<Vec<KeyInfo>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {KEY_COLS} FROM api_keys ORDER BY created_at, id"))
        .fetch_all(pool)
        .await
}

/// Makes a key. Returns the info row and the plain key (shown once, never stored).
pub async fn create(pool: &PgPool, scope: Scope, name: &str) -> Result<(KeyInfo, String), sqlx::Error> {
    let (key, prefix, hash) = generate_key(scope);
    let info = sqlx::query_as(&format!(
        "INSERT INTO api_keys (id, name, scope, key_prefix, key_hash) VALUES ($1, $2, $3, $4, $5) RETURNING {KEY_COLS}"
    ))
    .bind(Uuid::now_v7())
    .bind(name)
    .bind(scope.as_str())
    .bind(prefix)
    .bind(hash)
    .fetch_one(pool)
    .await?;
    Ok((info, key))
}

pub enum KeyChangeError {
    NotFound,
    AlreadyRevoked,
    LastAdmin,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for KeyChangeError {
    fn from(e: sqlx::Error) -> Self {
        KeyChangeError::Db(e)
    }
}

/// Locks all admin rows, so two revokes at the same time cannot remove the last admin key.
async fn lock_admin_keys(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> Result<Vec<Uuid>, sqlx::Error> {
    let rows: Vec<(Uuid,)> = sqlx::query_as(&format!(
        "SELECT id FROM api_keys WHERE scope = 'admin' AND {ACTIVE} ORDER BY id FOR UPDATE"
    ))
    .fetch_all(&mut **tx)
    .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

/// Revoke a key. Refuses to revoke the last active admin key.
pub async fn revoke(pool: &PgPool, id: Uuid) -> Result<(), KeyChangeError> {
    let mut tx = pool.begin().await?;
    let admins = lock_admin_keys(&mut tx).await?;
    let row: Option<(String, Option<DateTime<Utc>>)> =
        sqlx::query_as("SELECT scope, revoked_at FROM api_keys WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((scope, revoked_at)) = row else { return Err(KeyChangeError::NotFound) };
    if revoked_at.is_some_and(|t| t <= Utc::now()) {
        return Err(KeyChangeError::AlreadyRevoked);
    }
    if scope == "admin" && admins == [id] {
        return Err(KeyChangeError::LastAdmin);
    }
    sqlx::query("UPDATE api_keys SET revoked_at = now() WHERE id = $1").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Make a new key with the same scope and name. Revoke the old one after `grace_hours`.
pub async fn rotate(pool: &PgPool, id: Uuid, grace_hours: i64) -> Result<(KeyInfo, String), KeyChangeError> {
    let mut tx = pool.begin().await?;
    let row: Option<(String, String, Option<DateTime<Utc>>)> =
        sqlx::query_as("SELECT scope, name, revoked_at FROM api_keys WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((scope, name, revoked_at)) = row else { return Err(KeyChangeError::NotFound) };
    if revoked_at.is_some_and(|t| t <= Utc::now()) {
        return Err(KeyChangeError::AlreadyRevoked);
    }
    let scope = Scope::parse(&scope).ok_or(KeyChangeError::NotFound)?;
    let (key, prefix, hash) = generate_key(scope);
    let info: KeyInfo = sqlx::query_as(&format!(
        "INSERT INTO api_keys (id, name, scope, key_prefix, key_hash) VALUES ($1, $2, $3, $4, $5) RETURNING {KEY_COLS}"
    ))
    .bind(Uuid::now_v7())
    .bind(name)
    .bind(scope.as_str())
    .bind(prefix)
    .bind(hash)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("UPDATE api_keys SET revoked_at = now() + make_interval(hours => $2::int) WHERE id = $1")
        .bind(id)
        .bind(grace_hours as i32)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok((info, key))
}

/// First boot: make one key for each scope that has no active key.
/// Returns the new (scope, plain key) pairs that were made by the server itself.
/// Keys from env vars are used as they are and are never printed.
pub async fn bootstrap(pool: &PgPool, cfg: &Config) -> anyhow::Result<Vec<(Scope, String)>> {
    let mut tx = pool.begin().await?;
    // One server at a time, so two copies booting together do not both make an admin key.
    sqlx::query("SELECT pg_advisory_xact_lock(7306001)").execute(&mut *tx).await?;
    let mut made = Vec::new();
    for &scope in Scope::ALL {
        let (active,): (i64,) = sqlx::query_as(&format!(
            "SELECT count(*) FROM api_keys WHERE scope = $1 AND {ACTIVE}"
        ))
        .bind(scope.as_str())
        .fetch_one(&mut *tx)
        .await?;
        if active > 0 {
            continue;
        }
        let (key, prefix, hash, from_env) = match cfg.env_api_keys.get(&scope) {
            Some(env_key) => {
                let prefix: String = env_key.chars().take(12).collect();
                (env_key.clone(), prefix, hash_key(env_key), true)
            }
            None => {
                let (k, p, h) = generate_key(scope);
                (k, p, h, false)
            }
        };
        sqlx::query(
            "INSERT INTO api_keys (id, name, scope, key_prefix, key_hash) VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (key_hash) DO NOTHING",
        )
        .bind(Uuid::now_v7())
        .bind(if from_env { "bootstrap (env)" } else { "bootstrap" })
        .bind(scope.as_str())
        .bind(prefix)
        .bind(hash)
        .execute(&mut *tx)
        .await?;
        if !from_env {
            made.push((scope, key));
        }
    }
    tx.commit().await?;
    Ok(made)
}

/// Print new keys ONE time in a box, and save them to `<data_dir>/initial_keys.txt` (mode 600).
pub fn announce_new_keys(cfg: &Config, keys: &[(Scope, String)]) -> anyhow::Result<()> {
    if keys.is_empty() {
        return Ok(());
    }
    let mut lines = vec!["AIKIDO EMAIL: NEW API KEYS (shown one time only)".to_string(), String::new()];
    for (scope, key) in keys {
        lines.push(format!("{:<9} {}", scope.as_str(), key));
    }
    lines.push(String::new());
    lines.push("Lost a key? Rotate it with the admin key. It is not stored in plain form.".into());
    let width = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let bar = format!("+{}+", "-".repeat(width + 2));
    println!("{bar}");
    for l in &lines {
        println!("| {l:<width$} |");
    }
    println!("{bar}");

    std::fs::create_dir_all(&cfg.data_dir)?;
    let path = cfg.data_dir.join("initial_keys.txt");
    let mut file = std::fs::OpenOptions::new().write(true).create(true).append(true).mode(0o600).open(&path)?;
    for (scope, key) in keys {
        writeln!(file, "{}={}", scope.env_var(), key)?;
    }
    println!("Also saved to {} (mode 600).", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_format() {
        let (key, prefix, hash) = generate_key(Scope::Sending);
        assert!(key.starts_with("aik_sending_"));
        assert!(key.len() >= 12 + 43);
        assert!(key.starts_with(&prefix));
        assert_eq!(hash, hash_key(&key));
        assert_ne!(key, generate_key(Scope::Sending).0);
    }

    #[test]
    fn admin_passes_every_scope() {
        for s in Scope::ALL {
            assert!(Scope::Admin.allows(*s));
        }
        assert!(Scope::Sending.allows(Scope::Sending));
        assert!(!Scope::Sending.allows(Scope::Accounts));
    }
}
