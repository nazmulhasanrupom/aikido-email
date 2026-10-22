//! Read and write the `app_settings` table. Secrets are sealed with the setting key as AAD.
use crate::crypto::Crypto;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// Every saved value, decrypted. A row that cannot be read is skipped and logged.
pub async fn load_all(pool: &PgPool, crypto: &Crypto) -> anyhow::Result<HashMap<String, String>> {
    let rows: Vec<(String, Option<String>, Option<Vec<u8>>, Option<Vec<u8>>, Option<i32>)> =
        sqlx::query_as("SELECT key, value, sealed, nonce, key_version FROM app_settings").fetch_all(pool).await?;
    let mut out = HashMap::new();
    for (key, value, sealed, nonce, version) in rows {
        if super::item(&key).is_none() {
            continue;
        }
        let plain = match (value, sealed, nonce, version) {
            (Some(v), ..) => Some(v),
            (None, Some(c), Some(n), Some(ver)) => match crypto.decrypt(&c, &n, ver, key.as_bytes()) {
                Ok(p) => String::from_utf8(p.to_vec()).ok(),
                Err(_) => {
                    tracing::warn!(setting = %key, "a saved setting cannot be decrypted (was the encryption key changed?)");
                    None
                }
            },
            _ => None,
        };
        if let Some(p) = plain {
            out.insert(key, p);
        }
    }
    Ok(out)
}

/// Load the saved values into the process-wide overlay.
pub async fn refresh(pool: &PgPool, crypto: &Crypto) -> anyhow::Result<()> {
    super::set_overlay(load_all(pool, crypto).await?);
    Ok(())
}

/// Save changes in one transaction: `Some` saves a value, `None` removes it.
/// The keys must be checked by the caller (they must be in the catalog).
pub async fn apply(pool: &PgPool, crypto: &Crypto, user: Option<Uuid>, changes: &[(String, Option<String>)]) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    for (key, value) in changes {
        let Some(value) = value else {
            sqlx::query("DELETE FROM app_settings WHERE key = $1").bind(key).execute(&mut *tx).await?;
            continue;
        };
        let secret = super::item(key).is_some_and(|i| i.secret);
        let (plain, sealed, nonce, version) = if secret {
            let s = crypto.encrypt(value.as_bytes(), key.as_bytes())?;
            (None, Some(s.ciphertext), Some(s.nonce), Some(s.key_version))
        } else {
            (Some(value.clone()), None, None, None)
        };
        sqlx::query(
            "INSERT INTO app_settings (key, value, sealed, nonce, key_version, updated_by, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, now())
             ON CONFLICT (key) DO UPDATE SET value = $2, sealed = $3, nonce = $4, key_version = $5, updated_by = $6, updated_at = now()",
        )
        .bind(key)
        .bind(plain)
        .bind(sealed)
        .bind(nonce)
        .bind(version)
        .bind(user)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Load the overlay now, then every 60 seconds. Another server copy may have saved a change.
pub fn spawn_reload_loop(pool: PgPool, crypto: Arc<Crypto>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            if let Err(e) = refresh(&pool, &crypto).await {
                tracing::warn!(error = %format!("{e:#}"), "could not reload the app settings");
            }
        }
    });
}
