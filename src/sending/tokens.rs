//! OAuth token refresh, for Gmail and Microsoft.
//! Before each send: if the access token ends in less than 5 minutes, refresh first.
use crate::accounts::model::{EmailAccount, OAuthSecrets, Provider};
use crate::accounts::repo;
use crate::crypto::Crypto;
use crate::email_management::oauth_common::{RefreshError, Refreshed};
use crate::email_management::{gmailconnection::connect as gmail, msconnection::connect as microsoft};
use chrono::{DateTime, Duration, Utc};
use secrecy::{ExposeSecret, SecretString};
use sqlx::PgPool;

pub fn needs_refresh(expires_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    expires_at.is_none_or(|e| e - now < Duration::minutes(5))
}

/// Gets a new access token and saves it (and a new refresh token, if the provider sends one).
/// `Err(RefreshError::InvalidGrant)` means the user must connect the account again.
pub async fn refresh(
    pool: &PgPool,
    crypto: &Crypto,
    account: &EmailAccount,
    secrets: &OAuthSecrets,
    now: DateTime<Utc>,
) -> Result<OAuthSecrets, RefreshError> {
    let Some(refresh_token) = &secrets.refresh_token else {
        return Err(RefreshError::InvalidGrant("no refresh token is saved".into()));
    };
    let token = refresh_token.expose_secret();
    let fresh: Refreshed = match account.provider {
        Provider::Gmail => gmail::refresh(token).await?,
        Provider::Microsoft => microsoft::refresh(token).await?,
        Provider::Smtp => return Err(RefreshError::Other("SMTP accounts have no token".into())),
    };
    let expires_at = fresh.expires_in.map(|s| now + Duration::seconds(s as i64));
    repo::save_oauth_tokens(pool, crypto, account.id, fresh.access_token.clone(), fresh.refresh_token.clone(), expires_at)
        .await
        .map_err(|e| RefreshError::Other(format!("could not save the new token: {e:#}")))?;
    Ok(OAuthSecrets {
        access_token: SecretString::from(fresh.access_token),
        refresh_token: fresh.refresh_token.map(SecretString::from).or_else(|| secrets.refresh_token.clone()),
        scope: secrets.scope.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn refresh_window_is_five_minutes() {
        let now = Utc.with_ymd_and_hms(2026, 10, 19, 10, 0, 0).unwrap();
        assert!(needs_refresh(None, now));
        assert!(needs_refresh(Some(now + Duration::minutes(4)), now));
        assert!(needs_refresh(Some(now - Duration::minutes(1)), now));
        assert!(!needs_refresh(Some(now + Duration::minutes(6)), now));
    }
}
