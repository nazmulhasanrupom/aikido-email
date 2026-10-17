use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

#[derive(sqlx::Type, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, ToSchema)]
#[sqlx(type_name = "email_provider", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Gmail,
    Microsoft,
    Smtp,
}

impl Provider {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "gmail" => Some(Self::Gmail),
            "microsoft" => Some(Self::Microsoft),
            "smtp" => Some(Self::Smtp),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gmail => "gmail",
            Self::Microsoft => "microsoft",
            Self::Smtp => "smtp",
        }
    }
    pub fn is_oauth(self) -> bool {
        matches!(self, Self::Gmail | Self::Microsoft)
    }
}

#[derive(sqlx::Type, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, ToSchema)]
#[sqlx(type_name = "account_status", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum AccountStatus {
    Active,
    Paused,
    Error,
    Disconnected,
}

#[derive(sqlx::Type, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, ToSchema)]
#[sqlx(type_name = "tls_mode", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum TlsMode {
    Implicit,
    Starttls,
    #[sqlx(rename = "none")]
    #[serde(rename = "none")]
    Plain,
}

/// Columns to select for `EmailAccount` (citext is cast to text).
pub const ACCOUNT_COLS: &str = "id, email::text AS email, provider, display_name, status, smtp_host, smtp_port, smtp_tls, \
    imap_host, imap_port, imap_tls, secret_ciphertext, secret_nonce, key_version, token_expires_at, last_error, \
    created_at, updated_at, deleted_at, granted_scopes, can_read, read_status";

/// A row of `email_accounts`. Holds only ciphertext. Never serialize this to the API.
#[derive(Clone, FromRow)]
pub struct EmailAccount {
    pub id: Uuid,
    pub email: String,
    pub provider: Provider,
    pub display_name: String,
    pub status: AccountStatus,
    pub smtp_host: Option<String>,
    pub smtp_port: Option<i32>,
    pub smtp_tls: Option<TlsMode>,
    pub imap_host: Option<String>,
    pub imap_port: Option<i32>,
    pub imap_tls: Option<TlsMode>,
    pub secret_ciphertext: Vec<u8>,
    pub secret_nonce: Vec<u8>,
    pub key_version: i32,
    pub token_expires_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    /// The OAuth scopes the provider really granted.
    pub granted_scopes: Vec<String>,
    /// `true` when the inbox can be read (read scope granted, or the IMAP login works).
    pub can_read: bool,
    /// `ok`, `reconnect_needed`, `login_failed`, `not_configured`. `NULL` = not checked yet.
    pub read_status: Option<String>,
}

// A hand-written Debug, so the ciphertext never lands in a log by accident.
impl std::fmt::Debug for EmailAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmailAccount")
            .field("id", &self.id)
            .field("email", &self.email)
            .field("provider", &self.provider)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl EmailAccount {
    pub fn has_credentials(&self) -> bool {
        !self.secret_ciphertext.is_empty()
    }
    /// `can_read` and `read_status` for a login that was just saved.
    /// OAuth: read access needs a read scope. SMTP/IMAP: needs IMAP details (the login was tested before saving).
    pub fn read_access(provider: Provider, secrets: &Secrets, has_imap: bool) -> (Vec<String>, bool, &'static str) {
        match secrets {
            Secrets::OAuth(o) => {
                let granted: Vec<String> =
                    o.scope.as_deref().unwrap_or_default().split_whitespace().map(str::to_string).collect();
                let can_read = has_read_scope(provider, &granted);
                (granted, can_read, if can_read { "ok" } else { "reconnect_needed" })
            }
            Secrets::Password(_) => (vec![], has_imap, if has_imap { "ok" } else { "not_configured" }),
        }
    }

    pub fn domain(&self) -> &str {
        self.email.rsplit('@').next().unwrap_or("localhost")
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, ToSchema)]
pub struct HostInfo {
    pub host: String,
    pub port: u16,
    pub tls: TlsMode,
}

/// What the API shows about an account. No secrets.
#[derive(Serialize, ToSchema)]
pub struct AccountView {
    pub id: Uuid,
    pub email: String,
    pub provider: Provider,
    pub display_name: String,
    pub status: AccountStatus,
    pub smtp: Option<HostInfo>,
    pub imap: Option<HostInfo>,
    pub has_credentials: bool,
    pub token_expires_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub granted_scopes: Vec<String>,
    pub can_read: bool,
    pub read_status: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn host_info(host: &Option<String>, port: Option<i32>, tls: Option<TlsMode>) -> Option<HostInfo> {
    Some(HostInfo { host: host.clone()?, port: u16::try_from(port?).ok()?, tls: tls? })
}

impl From<&EmailAccount> for AccountView {
    fn from(a: &EmailAccount) -> Self {
        Self {
            id: a.id,
            email: a.email.clone(),
            provider: a.provider,
            display_name: a.display_name.clone(),
            status: a.status,
            smtp: host_info(&a.smtp_host, a.smtp_port, a.smtp_tls),
            imap: host_info(&a.imap_host, a.imap_port, a.imap_tls),
            has_credentials: a.has_credentials(),
            token_expires_at: a.token_expires_at,
            last_error: a.last_error.clone(),
            granted_scopes: a.granted_scopes.clone(),
            can_read: a.can_read,
            read_status: a.read_status.clone(),
            created_at: a.created_at,
            updated_at: a.updated_at,
        }
    }
}

// ---------- secrets ----------

/// Does one of the granted scopes allow reading mail?
/// Gmail: `gmail.readonly` (or a wider Gmail scope). Microsoft: `Mail.Read` (or `Mail.ReadWrite`).
pub fn has_read_scope(provider: Provider, scopes: &[String]) -> bool {
    scopes.iter().any(|s| {
        let s = s.to_lowercase();
        match provider {
            Provider::Gmail => {
                s.ends_with("/gmail.readonly") || s.ends_with("/gmail.modify") || s == "https://mail.google.com/"
            }
            Provider::Microsoft => s.ends_with("mail.read") || s.ends_with("mail.readwrite"),
            Provider::Smtp => false,
        }
    })
}

/// The JSON object that is encrypted as a whole. Plain strings live here only for a moment:
/// it is wiped on drop and never printed.
#[derive(Serialize, Deserialize, Default, Zeroize, ZeroizeOnDrop)]
struct Blob {
    #[serde(skip_serializing_if = "Option::is_none")]
    access_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    smtp_username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    smtp_password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    imap_username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    imap_password: Option<String>,
}

#[derive(Clone)]
pub struct OAuthSecrets {
    pub access_token: SecretString,
    pub refresh_token: Option<SecretString>,
    pub scope: Option<String>,
}

#[derive(Clone, Default)]
pub struct PasswordSecrets {
    pub smtp_username: Option<String>,
    pub smtp_password: Option<SecretString>,
    pub imap_username: Option<String>,
    pub imap_password: Option<SecretString>,
}

/// Decrypted secrets. `Debug` shows no values (SecretString hides them).
#[derive(Clone)]
pub enum Secrets {
    OAuth(OAuthSecrets),
    Password(PasswordSecrets),
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Secrets::OAuth(_) => f.write_str("Secrets::OAuth([REDACTED])"),
            Secrets::Password(_) => f.write_str("Secrets::Password([REDACTED])"),
        }
    }
}

fn secret(s: Option<String>) -> Option<SecretString> {
    s.map(SecretString::from)
}

fn expose(s: &Option<SecretString>) -> Option<String> {
    s.as_ref().map(|s| s.expose_secret().to_string())
}

impl Secrets {
    /// The bytes to encrypt: one JSON object.
    pub fn to_plaintext(&self) -> Zeroizing<Vec<u8>> {
        let mut blob = Blob::default();
        match self {
            Secrets::OAuth(o) => {
                blob.access_token = Some(o.access_token.expose_secret().to_string());
                blob.refresh_token = expose(&o.refresh_token);
                blob.scope = o.scope.clone();
            }
            Secrets::Password(p) => {
                blob.smtp_username = p.smtp_username.clone();
                blob.smtp_password = expose(&p.smtp_password);
                blob.imap_username = p.imap_username.clone();
                blob.imap_password = expose(&p.imap_password);
            }
        }
        Zeroizing::new(serde_json::to_vec(&blob).expect("a blob always serializes"))
    }

    pub fn from_plaintext(provider: Provider, bytes: &[u8]) -> Result<Self, serde_json::Error> {
        let mut blob: Blob = serde_json::from_slice(bytes)?;
        Ok(if provider.is_oauth() {
            Secrets::OAuth(OAuthSecrets {
                access_token: SecretString::from(blob.access_token.take().unwrap_or_default()),
                refresh_token: secret(blob.refresh_token.take()),
                scope: blob.scope.take(),
            })
        } else {
            Secrets::Password(PasswordSecrets {
                smtp_username: blob.smtp_username.take(),
                smtp_password: secret(blob.smtp_password.take()),
                imap_username: blob.imap_username.take(),
                imap_password: secret(blob.imap_password.take()),
            })
        })
    }
}
