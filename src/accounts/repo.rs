//! DB read and write for accounts. Encrypts on save, decrypts on load.
//! Secrets are decrypted only when the caller asks (`load_secrets`).
use super::model::*;
use crate::crypto::Crypto;
use anyhow::Context;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// The non-secret part of a connected account.
#[derive(Clone, Debug)]
pub struct NewAccount {
    pub email: String,
    pub provider: Provider,
    pub display_name: String,
    pub smtp: Option<HostInfo>,
    pub imap: Option<HostInfo>,
    pub token_expires_at: Option<DateTime<Utc>>,
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    e.as_database_error().and_then(|d| d.code()).is_some_and(|c| c == "23505")
}

/// Hook for `email_management`: when a connect flow finishes, call this.
/// Upsert by email + provider: connecting the same inbox twice updates it, it does not duplicate it.
/// New accounts get a copy of the sending defaults. A reconnect clears `disconnected` / `error`.
pub async fn upsert_connected(
    pool: &PgPool,
    crypto: &Crypto,
    new: NewAccount,
    secrets: &Secrets,
) -> anyhow::Result<EmailAccount> {
    let email = new.email.trim().to_lowercase();
    for attempt in 0..2 {
        match try_upsert(pool, crypto, &new, &email, secrets).await {
            Err(e) if attempt == 0 && e.downcast_ref::<sqlx::Error>().is_some_and(is_unique_violation) => continue,
            other => return other,
        }
    }
    unreachable!("the loop returns on the second attempt")
}

async fn try_upsert(
    pool: &PgPool,
    crypto: &Crypto,
    new: &NewAccount,
    email: &str,
    secrets: &Secrets,
) -> anyhow::Result<EmailAccount> {
    let mut tx = pool.begin().await?;
    let existing: Option<(Uuid, AccountStatus)> = sqlx::query_as(
        "SELECT id, status FROM email_accounts
         WHERE email = $1::citext AND provider = $2 AND deleted_at IS NULL FOR UPDATE",
    )
    .bind(email)
    .bind(new.provider)
    .fetch_optional(&mut *tx)
    .await?;
    let id = existing.map(|e| e.0).unwrap_or_else(Uuid::now_v7);
    let sealed = crypto.encrypt(&secrets.to_plaintext(), id.as_bytes())?;
    let (smtp, imap) = (new.smtp.as_ref(), new.imap.as_ref());
    let (scopes, can_read, read_status) = EmailAccount::read_access(new.provider, secrets, imap.is_some());

    let account: EmailAccount = if let Some((_, old_status)) = existing {
        let acct = sqlx::query_as(&format!(
            "UPDATE email_accounts SET
                display_name = CASE WHEN $2 = '' THEN display_name ELSE $2 END,
                status = CASE WHEN status IN ('disconnected', 'error') THEN 'active'::account_status ELSE status END,
                smtp_host = $3, smtp_port = $4, smtp_tls = $5, imap_host = $6, imap_port = $7, imap_tls = $8,
                secret_ciphertext = $9, secret_nonce = $10, key_version = $11,
                token_expires_at = $12, last_error = NULL, updated_at = now(),
                granted_scopes = $13, can_read = $14, read_status = $15
             WHERE id = $1 RETURNING {ACCOUNT_COLS}"
        ))
        .bind(id)
        .bind(&new.display_name)
        .bind(smtp.map(|h| h.host.clone()))
        .bind(smtp.map(|h| h.port as i32))
        .bind(smtp.map(|h| h.tls))
        .bind(imap.map(|h| h.host.clone()))
        .bind(imap.map(|h| h.port as i32))
        .bind(imap.map(|h| h.tls))
        .bind(&sealed.ciphertext)
        .bind(&sealed.nonce)
        .bind(sealed.key_version)
        .bind(new.token_expires_at)
        .bind(&scopes)
        .bind(can_read)
        .bind(read_status)
        .fetch_one(&mut *tx)
        .await?;
        // A reconnect gives the inbox another chance at once.
        sqlx::query(
            "UPDATE inbox_sync_state SET consecutive_errors = 0, last_error = NULL, next_sync_at = NULL, sync_started_at = NULL
             WHERE account_id = $1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        if matches!(old_status, AccountStatus::Disconnected | AccountStatus::Error) {
            sqlx::query(
                "UPDATE account_send_state SET paused = false, paused_reason = NULL, paused_at = NULL,
                        consecutive_failures = 0 WHERE account_id = $1",
            )
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
        acct
    } else {
        let acct = sqlx::query_as(&format!(
            "INSERT INTO email_accounts (id, email, provider, display_name, smtp_host, smtp_port, smtp_tls,
                imap_host, imap_port, imap_tls, secret_ciphertext, secret_nonce, key_version, token_expires_at,
                granted_scopes, can_read, read_status)
             VALUES ($1, $2::citext, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
             RETURNING {ACCOUNT_COLS}"
        ))
        .bind(id)
        .bind(email)
        .bind(new.provider)
        .bind(&new.display_name)
        .bind(smtp.map(|h| h.host.clone()))
        .bind(smtp.map(|h| h.port as i32))
        .bind(smtp.map(|h| h.tls))
        .bind(imap.map(|h| h.host.clone()))
        .bind(imap.map(|h| h.port as i32))
        .bind(imap.map(|h| h.tls))
        .bind(&sealed.ciphertext)
        .bind(&sealed.nonce)
        .bind(sealed.key_version)
        .bind(new.token_expires_at)
        .bind(&scopes)
        .bind(can_read)
        .bind(read_status)
        .fetch_one(&mut *tx)
        .await?;
        // New accounts get a copy of the sending defaults and of the reply defaults.
        sqlx::query("INSERT INTO account_sending_settings (account_id, settings) SELECT $1, settings FROM sending_defaults WHERE id = 1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO account_send_state (account_id) VALUES ($1)").bind(id).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO account_reply_settings (account_id, settings) SELECT $1, settings FROM reply_defaults WHERE id = 1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        acct
    };
    tx.commit().await?;
    Ok(account)
}

pub async fn get(pool: &PgPool, id: Uuid) -> Result<Option<EmailAccount>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {ACCOUNT_COLS} FROM email_accounts WHERE id = $1 AND deleted_at IS NULL"))
        .bind(id)
        .fetch_optional(pool)
        .await
}

pub async fn list(pool: &PgPool) -> Result<Vec<EmailAccount>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {ACCOUNT_COLS} FROM email_accounts WHERE deleted_at IS NULL ORDER BY created_at, id"))
        .fetch_all(pool)
        .await
}

/// Decrypt the secrets of one account. Call this only right before a send or a token refresh,
/// and drop the result right after.
pub fn load_secrets(crypto: &Crypto, account: &EmailAccount) -> anyhow::Result<Secrets> {
    let plain = crypto
        .decrypt(&account.secret_ciphertext, &account.secret_nonce, account.key_version, account.id.as_bytes())
        .context("cannot decrypt the account secrets")?;
    Secrets::from_plaintext(account.provider, &plain).context("the account secrets are damaged")
}

/// Save a new access token (and a new refresh token, if the provider sent one).
pub async fn save_oauth_tokens(
    pool: &PgPool,
    crypto: &Crypto,
    account_id: Uuid,
    access_token: String,
    new_refresh_token: Option<String>,
    expires_at: Option<DateTime<Utc>>,
) -> anyhow::Result<()> {
    use secrecy::SecretString;
    let mut tx = pool.begin().await?;
    let account: EmailAccount =
        sqlx::query_as(&format!("SELECT {ACCOUNT_COLS} FROM email_accounts WHERE id = $1 FOR UPDATE"))
            .bind(account_id)
            .fetch_one(&mut *tx)
            .await?;
    let Secrets::OAuth(mut o) = load_secrets(crypto, &account)? else {
        anyhow::bail!("not an OAuth account");
    };
    o.access_token = SecretString::from(access_token);
    if let Some(r) = new_refresh_token {
        o.refresh_token = Some(SecretString::from(r));
    }
    let sealed = crypto.encrypt(&Secrets::OAuth(o).to_plaintext(), account_id.as_bytes())?;
    sqlx::query(
        "UPDATE email_accounts SET secret_ciphertext = $2, secret_nonce = $3, key_version = $4,
                token_expires_at = $5, updated_at = now() WHERE id = $1",
    )
    .bind(account_id)
    .bind(&sealed.ciphertext)
    .bind(&sealed.nonce)
    .bind(sealed.key_version)
    .bind(expires_at)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// What `PATCH /v1/accounts/{id}` may change. `None` = no change.
#[derive(Default)]
pub struct AccountChanges {
    pub display_name: Option<String>,
    pub smtp: Option<HostInfo>,
    pub imap: Option<HostInfo>,
    pub secrets: Option<Secrets>,
    /// The SMTP login was tested and works: bring a disconnected account back.
    pub reconnect: bool,
    /// The IMAP login works (or was saved untested on localhost): the inbox can be read.
    pub read_ok: bool,
}

pub async fn apply_changes(
    pool: &PgPool,
    crypto: &Crypto,
    id: Uuid,
    changes: AccountChanges,
) -> anyhow::Result<Option<EmailAccount>> {
    let mut tx = pool.begin().await?;
    let found: Option<(Uuid, AccountStatus)> =
        sqlx::query_as("SELECT id, status FROM email_accounts WHERE id = $1 AND deleted_at IS NULL FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((_, old_status)) = found else { return Ok(None) };
    if let Some(name) = &changes.display_name {
        sqlx::query("UPDATE email_accounts SET display_name = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(name)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(h) = &changes.smtp {
        sqlx::query("UPDATE email_accounts SET smtp_host = $2, smtp_port = $3, smtp_tls = $4, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(&h.host)
            .bind(h.port as i32)
            .bind(h.tls)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(h) = &changes.imap {
        sqlx::query("UPDATE email_accounts SET imap_host = $2, imap_port = $3, imap_tls = $4, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(&h.host)
            .bind(h.port as i32)
            .bind(h.tls)
            .execute(&mut *tx)
            .await?;
    }
    if let Some(secrets) = &changes.secrets {
        let sealed = crypto.encrypt(&secrets.to_plaintext(), id.as_bytes())?;
        sqlx::query(
            "UPDATE email_accounts SET secret_ciphertext = $2, secret_nonce = $3, key_version = $4,
                    last_error = NULL, updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(&sealed.ciphertext)
        .bind(&sealed.nonce)
        .bind(sealed.key_version)
        .execute(&mut *tx)
        .await?;
    }
    // Like a reconnect: a working login brings the account back. An account the user paused stays paused.
    if changes.reconnect && matches!(old_status, AccountStatus::Disconnected | AccountStatus::Error) {
        sqlx::query("UPDATE email_accounts SET status = 'active'::account_status, last_error = NULL, updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE account_send_state SET paused = false, paused_reason = NULL, paused_at = NULL,
                    consecutive_failures = 0 WHERE account_id = $1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    if changes.read_ok {
        sqlx::query("UPDATE email_accounts SET can_read = true, read_status = 'ok', updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        // The inbox gets another chance at once.
        sqlx::query(
            "UPDATE inbox_sync_state SET consecutive_errors = 0, last_error = NULL, next_sync_at = NULL, sync_started_at = NULL
             WHERE account_id = $1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    let account = sqlx::query_as(&format!("SELECT {ACCOUNT_COLS} FROM email_accounts WHERE id = $1"))
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Some(account))
}

/// Wipes the secrets at once, soft-deletes the row, and cancels its queued jobs.
/// The row stays, so old sent jobs still point to something.
/// Returns the number of cancelled jobs, or `None` if the account does not exist.
pub async fn soft_delete(pool: &PgPool, id: Uuid) -> Result<Option<u64>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let wiped = sqlx::query(
        "UPDATE email_accounts SET deleted_at = now(), status = 'disconnected', secret_ciphertext = ''::bytea,
                secret_nonce = ''::bytea, key_version = 0, updated_at = now()
         WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    if wiped.rows_affected() == 0 {
        return Ok(None);
    }
    // `subject` and `reply_text` are personal data: wipe them with the account.
    sqlx::query("UPDATE inbound_messages SET reply_text = NULL, subject = NULL, updated_at = now() WHERE account_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM inbox_sync_state WHERE account_id = $1").bind(id).execute(&mut *tx).await?;
    // Follow-up enrollments of this account can never send again. They go first: lock order is enrollment, then job.
    crate::followups::on_account_deleted(&mut tx, id, chrono::Utc::now()).await?;
    let cancelled: Vec<(Uuid,)> = sqlx::query_as(
        "UPDATE send_jobs SET status = 'cancelled', last_error = 'account deleted', updated_at = now()
         WHERE account_id = $1 AND status = 'queued' RETURNING id",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    for (job_id,) in &cancelled {
        sqlx::query("INSERT INTO send_events (job_id, account_id, event, detail) VALUES ($1, $2, 'cancelled', '{\"reason\":\"account deleted\"}')")
            .bind(job_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query(
        "UPDATE account_send_state SET paused = true, paused_reason = 'account deleted', paused_at = now() WHERE account_id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(cancelled.len() as u64))
}

// ---------- hooks for `email_management` ----------
// The connect flows (OAuth login, IMAP/SMTP setup) call these when they finish.

/// A finished Gmail / Microsoft login: save the tokens in the encrypted store.
pub async fn save_oauth_login(
    pool: &PgPool,
    crypto: &Crypto,
    provider: Provider,
    c: crate::email_management::oauth_common::Connected,
) -> anyhow::Result<EmailAccount> {
    use secrecy::SecretString;
    let secrets = Secrets::OAuth(OAuthSecrets {
        access_token: SecretString::from(c.access_token),
        refresh_token: Some(SecretString::from(c.refresh_token)),
        scope: c.scope,
    });
    let new = NewAccount {
        email: c.email,
        provider,
        display_name: String::new(),
        smtp: None,
        imap: None,
        token_expires_at: c.expires_in.map(|s| Utc::now() + chrono::Duration::seconds(s as i64)),
    };
    upsert_connected(pool, crypto, new, &secrets).await
}

fn tls_of(s: crate::email_management::imapsmtp::connect::Security) -> TlsMode {
    match s {
        crate::email_management::imapsmtp::connect::Security::Tls => TlsMode::Implicit,
        crate::email_management::imapsmtp::connect::Security::StartTls => TlsMode::Starttls,
    }
}

/// A finished IMAP/SMTP setup (the old CLI flow): save it in the encrypted store.
/// An account without SMTP settings cannot send, so it is refused.
pub async fn save_imap_account(
    pool: &PgPool,
    crypto: &Crypto,
    a: crate::email_management::imapsmtp::connect::Account,
) -> anyhow::Result<EmailAccount> {
    use secrecy::SecretString;
    let smtp = a.smtp.context("this account has no SMTP settings, so the engine cannot send from it")?;
    let secrets = Secrets::Password(PasswordSecrets {
        smtp_username: Some(smtp.username.clone()),
        smtp_password: Some(SecretString::from(smtp.password.clone())),
        imap_username: a.imap.as_ref().map(|i| i.username.clone()),
        imap_password: a.imap.as_ref().map(|i| SecretString::from(i.password.clone())),
    });
    let new = NewAccount {
        email: a.email,
        provider: Provider::Smtp,
        display_name: String::new(),
        smtp: Some(HostInfo { host: smtp.host.clone(), port: smtp.port, tls: tls_of(smtp.security) }),
        imap: a.imap.as_ref().map(|i| HostInfo { host: i.host.clone(), port: i.port, tls: tls_of(i.security) }),
        token_expires_at: None,
    };
    upsert_connected(pool, crypto, new, &secrets).await
}
