//! Free HTTPS certificates from Let's Encrypt (ACME, HTTP-01).
//!
//! Let's Encrypt asks `http://<tracking host>/.well-known/acme-challenge/<token>` (port 80 of this server, because
//! the CNAME points here). The answer is kept in the database, so any server copy can give it.
//! The ACME account key is saved encrypted (AES-256-GCM), like the certificates.
//!
//! Not tested against a real ACME server: the code follows `instant-acme`. Use the Let's Encrypt staging address
//! (`AIKIDO_ACME_DIRECTORY`) for a first try.
use crate::state::AppState;
use chrono::{DateTime, Utc};
use instant_acme::{Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, NewAccount, NewOrder, OrderStatus, RetryPolicy};

pub struct Issued {
    /// The certificate and the chain, PEM.
    pub chain_pem: String,
    /// The private key, PEM (PKCS#8).
    pub key_pem: String,
    pub expires_at: DateTime<Utc>,
}

/// The expiry date of the first certificate in a PEM chain.
pub fn expiry_of(chain_pem: &str) -> Result<DateTime<Utc>, String> {
    let first = pem::parse_many(chain_pem).map_err(|e| e.to_string())?.into_iter().find(|p| p.tag() == "CERTIFICATE").ok_or("no certificate in the answer")?;
    let (_, cert) = x509_parser::parse_x509_certificate(first.contents()).map_err(|e| e.to_string())?;
    DateTime::from_timestamp(cert.validity().not_after.timestamp(), 0).ok_or_else(|| "bad expiry date".to_string())
}

async fn account(st: &AppState) -> Result<Account, String> {
    let dir = &st.config.tracking.acme_directory;
    let saved: Option<(Vec<u8>,)> = sqlx::query_as("SELECT credentials FROM tracking_acme_account WHERE directory = $1").bind(dir).fetch_optional(&st.pool).await.map_err(|e| e.to_string())?;
    let builder = || Account::builder().map_err(|e| e.to_string());
    if let Some((blob,)) = saved {
        let plain = super::open(&st.crypto, &blob, dir.as_bytes())?;
        let creds: AccountCredentials = serde_json::from_slice(&plain).map_err(|e| e.to_string())?;
        return builder()?.from_credentials(creds).await.map_err(|e| e.to_string());
    }
    let contact = st.config.tracking.acme_email.as_ref().map(|e| format!("mailto:{e}"));
    let contacts: Vec<&str> = contact.iter().map(String::as_str).collect();
    let (account, creds) = builder()?
        .create(&NewAccount { contact: &contacts, terms_of_service_agreed: true, only_return_existing: false }, dir.clone(), None)
        .await
        .map_err(|e| e.to_string())?;
    let json = serde_json::to_vec(&creds).map_err(|e| e.to_string())?;
    let blob = super::seal(&st.crypto, &json, dir.as_bytes())?;
    sqlx::query("INSERT INTO tracking_acme_account (directory, credentials) VALUES ($1, $2) ON CONFLICT (directory) DO NOTHING")
        .bind(dir)
        .bind(blob)
        .execute(&st.pool)
        .await
        .map_err(|e| e.to_string())?;
    Ok(account)
}

/// Ask for a certificate for one host. Waits for the answer (a few seconds to a minute).
pub async fn issue(st: &AppState, host: &str) -> Result<Issued, String> {
    let account = account(st).await?;
    let mut tokens: Vec<String> = Vec::new();
    let result = order(st, &account, host, &mut tokens).await;
    if !tokens.is_empty() {
        let _ = sqlx::query("DELETE FROM tracking_acme_challenges WHERE token = ANY($1)").bind(&tokens).execute(&st.pool).await;
    }
    result
}

async fn order(st: &AppState, account: &Account, host: &str, tokens: &mut Vec<String>) -> Result<Issued, String> {
    let err = |e: instant_acme::Error| e.to_string();
    let identifiers = [Identifier::Dns(host.to_string())];
    let mut order = account.new_order(&NewOrder::new(&identifiers)).await.map_err(err)?;
    {
        let mut authorizations = order.authorizations();
        while let Some(next) = authorizations.next().await {
            let mut authz = next.map_err(err)?;
            match authz.status {
                AuthorizationStatus::Pending => {}
                AuthorizationStatus::Valid => continue,
                other => return Err(format!("Let's Encrypt refused {host} ({other:?})")),
            }
            let mut challenge = authz.challenge(ChallengeType::Http01).ok_or("Let's Encrypt offered no HTTP-01 challenge")?;
            let token = challenge.token.clone();
            let key_authorization = challenge.key_authorization().as_str().to_string();
            sqlx::query("INSERT INTO tracking_acme_challenges (token, key_authorization) VALUES ($1, $2) ON CONFLICT (token) DO UPDATE SET key_authorization = $2")
                .bind(&token)
                .bind(&key_authorization)
                .execute(&st.pool)
                .await
                .map_err(|e| e.to_string())?;
            tokens.push(token);
            challenge.set_ready().await.map_err(err)?;
        }
    }
    let status = order.poll_ready(&RetryPolicy::default()).await.map_err(err)?;
    if status != OrderStatus::Ready {
        return Err(format!("Let's Encrypt could not check {host} (order is {status:?}). Port 80 of this server must be open to the internet (AIKIDO_TRACKING_HTTP_ADDR)."));
    }
    let key_pem = order.finalize().await.map_err(err)?;
    let chain_pem = order.poll_certificate(&RetryPolicy::default()).await.map_err(err)?;
    let expires_at = expiry_of(&chain_pem)?;
    Ok(Issued { chain_pem, key_pem, expires_at })
}

/// The answer to a challenge request, or `None`.
pub async fn challenge_answer(pool: &sqlx::PgPool, token: &str) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT key_authorization FROM tracking_acme_challenges WHERE token = $1").bind(token).fetch_optional(pool).await
}
