//! Helpers shared by the Gmail and Outlook connectors.
use oauth2::reqwest;
use serde_json::{Map, Value};
use std::error::Error;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// `Send + Sync`, so the connect functions can be awaited inside HTTP handlers.
pub type Res<T> = Result<T, Box<dyn Error + Send + Sync>>;

pub fn var(name: &str) -> Res<String> {
    std::env::var(name).map_err(|_| format!("set {name} in .env").into())
}

pub fn var_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

/// `~/.config/aikido_email/<file_name>`, unless `env_name` is set.
pub fn accounts_path(env_name: &str, file_name: &str) -> Res<PathBuf> {
    match std::env::var(env_name) {
        Ok(p) => Ok(PathBuf::from(p)),
        Err(_) => Ok(PathBuf::from(var("HOME")?).join(".config/aikido_email").join(file_name)),
    }
}

/// Saved accounts: { "<email>": "<refresh_token>", ... }
pub fn load_accounts(path: &Path) -> Res<Map<String, Value>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(serde_json::from_str(&text)?),
        Err(_) => Ok(Map::new()),
    }
}

pub fn save_accounts(path: &Path, accounts: &Map<String, Value>) -> Res<()> {
    use std::os::unix::fs::PermissionsExt;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(accounts)?)?;
    // Refresh tokens are secrets: only the owner may read the file.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

pub fn http_client() -> Res<reqwest::Client> {
    Ok(reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

/// Starts the local server that catches the login redirect.
/// Port 0 in `redirect_url` means "pick any free port".
/// Returns the listener and the real redirect URL (with the real port).
pub async fn bind_redirect(redirect_url: &str) -> Res<(TcpListener, String)> {
    let mut url = oauth2::url::Url::parse(redirect_url)?;
    let host = url.host_str().ok_or("redirect URI has no host")?;
    // "localhost" in the URL, but listen on the IPv4 loopback.
    let bind_host = if host == "localhost" { "127.0.0.1" } else { host };
    let listener = TcpListener::bind((bind_host, url.port().unwrap_or(0))).await?;
    url.set_port(Some(listener.local_addr()?.port()))
        .map_err(|_| "bad redirect URI")?;
    Ok((listener, url.to_string()))
}

/// Opens the browser. If it fails, the user can open the printed link.
pub fn open_browser(cmd: &str, url: &str) {
    println!("Opening browser. If nothing opens, go to:\n{url}");
    let _ = std::process::Command::new(cmd).arg(url).spawn();
}

/// Waits for the browser to hit our local server. Returns (code, state).
pub async fn wait_for_redirect(listener: &TcpListener) -> Res<(String, String)> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let mut buf = vec![0u8; 8192];
        let n = stream.read(&mut buf).await?;
        let req = String::from_utf8_lossy(&buf[..n]);
        // First line looks like: GET /?code=...&state=... HTTP/1.1
        let target = req.split_whitespace().nth(1).unwrap_or("/");
        let url = oauth2::url::Url::parse(&format!("http://127.0.0.1{target}"))?;
        let find = |key: &str| {
            url.query_pairs()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.into_owned())
        };

        let reply = |msg: &str| {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{msg}",
                msg.len()
            )
        };

        if let Some(err) = find("error") {
            stream.write_all(reply("Login failed. Close this tab.").as_bytes()).await?;
            let desc = find("error_description").unwrap_or_default();
            return Err(format!("login error: {err} {desc}").into());
        }
        if let (Some(code), Some(state)) = (find("code"), find("state")) {
            stream.write_all(reply("Done. You can close this tab.").as_bytes()).await?;
            return Ok((code, state));
        }
        // Something else (like /favicon.ico): ignore and keep waiting.
        stream.write_all(reply("").as_bytes()).await?;
    }
}

// ---------- hooks for the sending engine ----------
// The connectors return these values. The engine saves them in the encrypted account store
// (`accounts::repo::upsert_connected`). The connectors themselves know nothing about the DB.

/// A finished OAuth login. Holds secrets: it has no `Debug` output for them.
pub struct Connected {
    pub email: String,
    pub access_token: String,
    pub refresh_token: String,
    /// Seconds until the access token ends.
    pub expires_in: Option<u64>,
    pub scope: Option<String>,
}

impl std::fmt::Debug for Connected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connected").field("email", &self.email).finish_non_exhaustive()
    }
}

/// A new access token from a refresh token.
pub struct Refreshed {
    pub access_token: String,
    /// Some providers (Microsoft) send a new refresh token. Save it.
    pub refresh_token: Option<String>,
    pub expires_in: Option<u64>,
}

impl std::fmt::Debug for Refreshed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Refreshed([REDACTED])")
    }
}

#[derive(Debug)]
pub enum RefreshError {
    /// The provider says the login is over. The user must connect again.
    InvalidGrant(String),
    /// Network error or another provider error. Safe to try again later.
    Other(String),
}

impl std::fmt::Display for RefreshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RefreshError::InvalidGrant(m) => write!(f, "invalid_grant: {m}"),
            RefreshError::Other(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for RefreshError {}

pub type TokenRequestError =
    oauth2::RequestTokenError<oauth2::HttpClientError<oauth2::reqwest::Error>, oauth2::basic::BasicErrorResponse>;

pub fn classify_refresh_error(e: TokenRequestError) -> RefreshError {
    match e {
        oauth2::RequestTokenError::ServerResponse(r)
            if *r.error() == oauth2::basic::BasicErrorResponseType::InvalidGrant =>
        {
            RefreshError::InvalidGrant(r.error_description().cloned().unwrap_or_default())
        }
        other => RefreshError::Other(other.to_string()),
    }
}
