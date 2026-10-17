//! SMTP sender (lettre). Port 465 = implicit TLS. Port 587 = STARTTLS.
//! `none` (no encryption) is allowed only for localhost, for tests.
//! A fresh connection is opened for every send: one email every 7 to 15 minutes needs no pool.
use super::{MailSender, OutgoingMessage, SendError, SendReceipt, clip};
use crate::accounts::model::{EmailAccount, PasswordSecrets, TlsMode};
use async_trait::async_trait;
use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::Error as SmtpError;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use secrecy::{ExposeSecret, SecretString};
use std::time::Duration;

pub fn is_local_host(host: &str) -> bool {
    matches!(host.to_ascii_lowercase().as_str(), "localhost" | "127.0.0.1" | "::1" | "[::1]")
}

pub enum SmtpAuth {
    Password { username: String, password: SecretString },
    /// OAuth access token. So OAuth accounts can fall back to SMTP later.
    Xoauth2 { username: String, token: SecretString },
    None,
}

pub fn build_transport(
    host: &str,
    port: u16,
    tls: TlsMode,
    auth: SmtpAuth,
) -> Result<AsyncSmtpTransport<Tokio1Executor>, SendError> {
    let perm = |m: String| SendError::Permanent { reason: m, hard_bounce: false };
    let builder = match tls {
        TlsMode::Implicit => AsyncSmtpTransport::<Tokio1Executor>::relay(host).map_err(|e| perm(e.to_string()))?,
        TlsMode::Starttls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host).map_err(|e| perm(e.to_string()))?,
        TlsMode::Plain if is_local_host(host) => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host),
        TlsMode::Plain => return Err(perm("SMTP without encryption is allowed only for localhost".into())),
    };
    let builder = builder.port(port).timeout(Some(Duration::from_secs(30)));
    // Unencrypted localhost is for test servers (like Mailpit) that have no login. A password
    // must never go over an unencrypted connection, so no login is tried there.
    let auth = if tls == TlsMode::Plain { SmtpAuth::None } else { auth };
    let builder = match auth {
        SmtpAuth::Password { username, password } => {
            builder.credentials(Credentials::new(username, password.expose_secret().to_string()))
        }
        SmtpAuth::Xoauth2 { username, token } => builder
            .credentials(Credentials::new(username, token.expose_secret().to_string()))
            .authentication(vec![Mechanism::Xoauth2]),
        SmtpAuth::None => builder,
    };
    Ok(builder.build())
}

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    /// Before the `DATA` command. Nothing was handed to the server yet.
    Connect,
    /// From `DATA` on. The server may have taken the email.
    Send,
}

/// Map an SMTP error to our `SendError`, by reply code.
fn map_error(e: &SmtpError, stage: Stage) -> SendError {
    let text = clip(&e.to_string());
    if let Some(code) = e.status() {
        let n = u16::from(code);
        return match n {
            530 | 534 | 535 | 538 => SendError::AuthFailed(text),
            421 | 450 | 451 | 452 => SendError::Transient(text),
            400..=499 => SendError::Transient(text),
            550 | 551 | 553 => SendError::Permanent { reason: text, hard_bounce: true },
            _ => SendError::Permanent { reason: text, hard_bounce: false },
        };
    }
    if e.is_client() {
        return SendError::Permanent { reason: text, hard_bounce: false };
    }
    match stage {
        Stage::Connect => SendError::Transient(text),
        // Timeout or broken connection after DATA: the server may have the email.
        Stage::Send => SendError::Unknown(text),
    }
}

/// Logs in and says nothing else. Sends no email.
pub async fn test_login(transport: &AsyncSmtpTransport<Tokio1Executor>) -> Result<(), SendError> {
    match transport.test_connection().await {
        Ok(true) => Ok(()),
        Ok(false) => Err(SendError::Transient("the server did not answer".into())),
        Err(e) => Err(map_error(&e, Stage::Connect)),
    }
}

pub struct SmtpSender {
    transport: AsyncSmtpTransport<Tokio1Executor>,
}

impl SmtpSender {
    pub fn new(transport: AsyncSmtpTransport<Tokio1Executor>) -> Self {
        Self { transport }
    }

    pub fn from_account(account: &EmailAccount, secrets: PasswordSecrets) -> Result<Self, SendError> {
        let (Some(host), Some(port), Some(tls)) = (&account.smtp_host, account.smtp_port, account.smtp_tls) else {
            return Err(SendError::Permanent { reason: "this account has no SMTP settings".into(), hard_bounce: false });
        };
        let port = u16::try_from(port)
            .map_err(|_| SendError::Permanent { reason: "bad SMTP port".into(), hard_bounce: false })?;
        let auth = match (secrets.smtp_username, secrets.smtp_password) {
            (Some(username), Some(password)) => SmtpAuth::Password { username, password },
            _ => SmtpAuth::None,
        };
        Ok(Self::new(build_transport(host, port, tls, auth)?))
    }
}

#[async_trait]
impl MailSender for SmtpSender {
    async fn send(&self, msg: &OutgoingMessage) -> Result<SendReceipt, SendError> {
        let message = msg.build()?;
        // Check the connection and the login first. A failure here is safe to retry:
        // it happened before DATA. After this, any error without a reply code is "unknown".
        test_login(&self.transport).await?;
        match self.transport.send(message).await {
            Ok(_) => Ok(SendReceipt::default()),
            Err(e) => Err(map_error(&e, Stage::Send)),
        }
    }
}
