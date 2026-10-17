//! Plain IMAP (read) and SMTP (send) accounts with a host, port, user and password.
//! IMAP and SMTP are both optional, so an account can be send-only or read-only.
//! Passwords are encrypted (AES-256-GCM) before they are saved. The key is `ACCOUNTS_KEY` in `.env`.
use crate::email_management::oauth_common::*;
use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, Tokio1Executor};
use rustls_pki_types::ServerName;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_rustls::{TlsConnector, client::TlsStream};

/// `Tls` = encrypted from the first byte (IMAP 993, SMTP 465).
/// `StartTls` = starts plain, then upgrades (IMAP 143, SMTP 587).
/// There is no "no encryption" option on purpose.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Security {
    Tls,
    StartTls,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Server {
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub username: String,
    /// Plain text in memory. Encrypted in the file.
    pub password: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Account {
    pub email: String,
    pub imap: Option<Server>,
    pub smtp: Option<Server>,
}

fn accounts_file() -> Res<PathBuf> {
    accounts_path("IMAP_ACCOUNTS_FILE", "imap_smtp_accounts.json")
}

// ---------- password encryption ----------

fn cipher() -> Res<Aes256Gcm> {
    let key = B64
        .decode(var("ACCOUNTS_KEY")?)
        .map_err(|_| "ACCOUNTS_KEY is not valid base64")?;
    if key.len() != 32 {
        return Err("ACCOUNTS_KEY must be 32 bytes. Make one with: openssl rand -base64 32".into());
    }
    Ok(Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key)))
}

fn encrypt(plain: &str) -> Res<String> {
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let mut out = nonce.to_vec();
    out.extend(cipher()?.encrypt(&nonce, plain.as_bytes()).map_err(|_| "encrypt failed")?);
    Ok(B64.encode(out))
}

fn decrypt(stored: &str) -> Res<String> {
    let raw = B64.decode(stored)?;
    if raw.len() < 12 {
        return Err("saved password is damaged".into());
    }
    let (nonce, data) = raw.split_at(12);
    let plain = cipher()?
        .decrypt(Nonce::from_slice(nonce), data)
        .map_err(|_| "cannot decrypt password (wrong ACCOUNTS_KEY?)")?;
    Ok(String::from_utf8(plain)?)
}

// ---------- saved accounts ----------

fn read_file() -> Res<BTreeMap<String, Account>> {
    match std::fs::read_to_string(accounts_file()?) {
        Ok(text) => Ok(serde_json::from_str(&text)?),
        Err(_) => Ok(BTreeMap::new()),
    }
}

fn write_file(map: &BTreeMap<String, Account>) -> Res<()> {
    // Reuse the shared writer (it sets the file to owner-only).
    let value = match serde_json::to_value(map)? {
        serde_json::Value::Object(m) => m,
        _ => unreachable!(),
    };
    save_accounts(&accounts_file()?, &value)
}

/// Emails of all saved accounts.
pub fn list_accounts() -> Res<Vec<String>> {
    Ok(read_file()?.into_keys().collect())
}

/// One saved account, with passwords decrypted.
pub fn load_account(email: &str) -> Res<Account> {
    let mut acct = read_file()?
        .remove(email)
        .ok_or_else(|| format!("no saved account: {email}"))?;
    for s in [&mut acct.imap, &mut acct.smtp].into_iter().flatten() {
        s.password = decrypt(&s.password)?;
    }
    Ok(acct)
}

/// Removes one saved account.
pub fn remove_account(email: &str) -> Res<bool> {
    let mut map = read_file()?;
    let found = map.remove(email).is_some();
    write_file(&map)?;
    Ok(found)
}

/// Tests the login first. Saves only if it works.
/// Adding the same email again replaces the old settings.
pub async fn add_account(mut acct: Account) -> Res<()> {
    if acct.imap.is_none() && acct.smtp.is_none() {
        return Err("give IMAP settings, SMTP settings, or both".into());
    }
    test_account(&acct).await?;
    for s in [&mut acct.imap, &mut acct.smtp].into_iter().flatten() {
        s.password = encrypt(&s.password)?;
    }
    let mut map = read_file()?;
    map.insert(acct.email.clone(), acct);
    write_file(&map)
}

/// Logs in to each server that is set. Saves nothing.
pub async fn test_account(acct: &Account) -> Res<()> {
    if let Some(imap) = &acct.imap {
        test_imap(imap).await.map_err(|e| format!("IMAP failed: {e}"))?;
    }
    if let Some(smtp) = &acct.smtp {
        test_smtp(smtp).await.map_err(|e| format!("SMTP failed: {e}"))?;
    }
    Ok(())
}

/// Test a saved account by email.
pub async fn test_saved_account(email: &str) -> Res<()> {
    test_account(&load_account(email)?).await
}

// ---------- connections ----------

fn tls_connector() -> TlsConnector {
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    TlsConnector::from(Arc::new(config))
}

async fn imap_connect(s: &Server) -> Res<async_imap::Session<TlsStream<TcpStream>>> {
    let tcp = TcpStream::connect((s.host.as_str(), s.port)).await?;
    let name = ServerName::try_from(s.host.clone())?;
    let tls = tls_connector();

    let client = match s.security {
        Security::Tls => {
            let mut c = async_imap::Client::new(tls.connect(name, tcp).await?);
            c.read_response().await?; // server greeting
            c
        }
        Security::StartTls => {
            let mut c = async_imap::Client::new(tcp);
            c.read_response().await?; // server greeting
            c.run_command_and_check_ok("STARTTLS", None).await?;
            async_imap::Client::new(tls.connect(name, c.into_inner()).await?)
        }
    };
    client
        .login(&s.username, &s.password)
        .await
        .map_err(|(e, _)| e.into())
}

pub async fn test_imap(s: &Server) -> Res<()> {
    let mut session = imap_connect(s).await?;
    session.logout().await?;
    Ok(())
}

/// Builds the SMTP sender for an account. Use it to send mail later.
pub fn smtp_transport(s: &Server) -> Res<AsyncSmtpTransport<Tokio1Executor>> {
    let builder = match s.security {
        Security::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&s.host)?,
        Security::StartTls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&s.host)?,
    };
    Ok(builder
        .port(s.port)
        .credentials(Credentials::new(s.username.clone(), s.password.clone()))
        .build())
}

pub async fn test_smtp(s: &Server) -> Res<()> {
    if !smtp_transport(s)?.test_connection().await? {
        return Err("server did not answer".into());
    }
    Ok(())
}

// ---------- ask the user (no UI, just the terminal) ----------

fn ask(label: &str, default: &str) -> Res<String> {
    if default.is_empty() {
        print!("{label}: ");
    } else {
        print!("{label} [{default}]: ");
    }
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let line = line.trim();
    Ok(if line.is_empty() { default.to_string() } else { line.to_string() })
}

fn ask_server(kind: &str, email: &str, tls_port: u16, starttls_port: u16) -> Res<Option<Server>> {
    let host = ask(&format!("{kind} host (empty to skip {kind})"), "")?;
    if host.is_empty() {
        return Ok(None);
    }
    let security = match ask(&format!("{kind} security: tls or starttls"), "tls")?.as_str() {
        "tls" => Security::Tls,
        "starttls" => Security::StartTls,
        other => return Err(format!("unknown security: {other}").into()),
    };
    let default_port = if security == Security::Tls { tls_port } else { starttls_port };
    let port = ask(&format!("{kind} port"), &default_port.to_string())?.parse()?;
    let username = ask(&format!("{kind} username"), email)?;
    let password = rpassword::prompt_password(format!("{kind} password: "))?;
    Ok(Some(Server { host, port, security, username, password }))
}

/// Asks questions in the terminal, tests the login, and saves the account.
pub async fn add_account_interactive() -> Res<String> {
    let email = ask("Email address", "")?;
    if email.is_empty() {
        return Err("email is required".into());
    }
    let imap = ask_server("IMAP", &email, 993, 143)?;
    let smtp = ask_server("SMTP", &email, 465, 587)?;
    add_account(Account { email: email.clone(), imap, smtp }).await?;
    Ok(email)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_round_trip() {
        unsafe { std::env::set_var("ACCOUNTS_KEY", B64.encode([7u8; 32])) };
        let enc = encrypt("secret pw").unwrap();
        assert_ne!(enc, "secret pw");
        assert_eq!(decrypt(&enc).unwrap(), "secret pw");
    }
}
