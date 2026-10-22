use crate::email_management::oauth_common::*;
use oauth2::basic::BasicClient;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl,
    RefreshToken, Scope, TokenResponse, TokenUrl,
};
use serde_json::Value;
use std::path::PathBuf;

/// All settings come from the environment (or the `.env` file).
struct Config {
    client_id: String,
    client_secret: String,
    auth_url: String,
    token_url: String,
    redirect_url: String,
    profile_url: String,
    scopes: Vec<String>,
    accounts_file: PathBuf,
    browser_cmd: String,
}

impl Config {
    fn load() -> Res<Self> {
        Ok(Self {
            client_id: var("GOOGLE_CLIENT_ID")?,
            client_secret: var("GOOGLE_CLIENT_SECRET")?,
            auth_url: var_or("GOOGLE_AUTH_URL", "https://accounts.google.com/o/oauth2/v2/auth"),
            token_url: var_or("GOOGLE_TOKEN_URL", "https://oauth2.googleapis.com/token"),
            redirect_url: var_or("GOOGLE_REDIRECT_URI", "http://127.0.0.1:0/"),
            profile_url: var_or(
                "GMAIL_PROFILE_URL",
                "https://gmail.googleapis.com/gmail/v1/users/me/profile",
            ),
            scopes: var_or(
                "GOOGLE_SCOPES",
                "https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/gmail.send",
            )
            .split_whitespace()
            .map(str::to_string)
            .collect(),
            accounts_file: accounts_path("ACCOUNTS_FILE", "accounts.json")?,
            browser_cmd: var_or("BROWSER_CMD", default_browser_cmd()),
        })
    }
}

/// Builds the OAuth client from a `Config`. `$redirect` is the redirect URL to use.
macro_rules! oauth_client {
    ($cfg:expr, $redirect:expr) => {
        BasicClient::new(ClientId::new($cfg.client_id.clone()))
            .set_client_secret(ClientSecret::new($cfg.client_secret.clone()))
            .set_auth_uri(AuthUrl::new($cfg.auth_url.clone())?)
            .set_token_uri(TokenUrl::new($cfg.token_url.clone())?)
            .set_redirect_uri(RedirectUrl::new($redirect)?)
    };
}

/// Builds the Google login URL for one redirect URL.
fn authorize(
    cfg: &Config,
    redirect: String,
    make_state: impl FnOnce(&str) -> String,
) -> Res<(String, CsrfToken, PkceCodeVerifier)> {
    let client = oauth_client!(cfg, redirect);
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let state = make_state(verifier.secret());
    let mut req = client.authorize_url(|| CsrfToken::new(state));
    for scope in &cfg.scopes {
        req = req.add_scope(Scope::new(scope.clone()));
    }
    let (auth_url, csrf) = req
        .set_pkce_challenge(challenge)
        .add_extra_param("access_type", "offline")
        // Always ask, so Google gives a refresh token and lets us pick the account.
        .add_extra_param("prompt", "consent select_account")
        .url();
    Ok((auth_url.to_string(), csrf, verifier))
}

/// Trades the login code for tokens and reads the email address.
async fn exchange(cfg: &Config, redirect: String, code: String, verifier: PkceCodeVerifier) -> Res<Connected> {
    let client = oauth_client!(cfg, redirect);
    let http = http_client()?;
    let token = client
        .exchange_code(AuthorizationCode::new(code))
        .set_pkce_verifier(verifier)
        .request_async(&http)
        .await?;
    let refresh = token.refresh_token().ok_or("Google gave no refresh token")?;
    let email = fetch_email(cfg, token.access_token().secret()).await?;
    Ok(Connected {
        email,
        access_token: token.access_token().secret().clone(),
        refresh_token: refresh.secret().clone(),
        expires_in: token.expires_in().map(|d| d.as_secs()),
        scope: token
            .scopes()
            .map(|s| s.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(" ")),
    })
}

/// Opens the Google login page in the browser and waits for the user to sign in.
/// Returns the tokens. Nothing is saved.
pub async fn login() -> Res<Connected> {
    let cfg = Config::load()?;
    let (listener, redirect) = bind_redirect(&cfg.redirect_url).await?;
    let (auth_url, csrf, verifier) = authorize(&cfg, redirect.clone(), |_| CsrfToken::new_random().secret().clone())?;

    open_browser(&cfg.browser_cmd, &auth_url);

    let (code, state) = wait_for_redirect(&listener).await?;
    if state != *csrf.secret() {
        return Err("login state did not match".into());
    }
    exchange(&cfg, redirect, code, verifier).await
}

/// Saves the refresh token in the accounts file (the old, file-based store).
pub fn save_to_file(c: &Connected) -> Res<()> {
    let cfg = Config::load()?;
    let mut accounts = load_accounts(&cfg.accounts_file)?;
    accounts.insert(c.email.clone(), Value::String(c.refresh_token.clone()));
    save_accounts(&cfg.accounts_file, &accounts)
}

/// Opens the Google login page, waits for the user to sign in,
/// and saves the account. Returns the email address of the account.
/// Call it again to add another account.
pub async fn add_account() -> Res<String> {
    let c = login().await?;
    save_to_file(&c)?;
    Ok(c.email)
}

// ---------- web flow: the login ends at our own server, not at a local port ----------

/// Step 1 of the web flow. `redirect` is `<AIKIDO_PUBLIC_URL>/v1/accounts/oauth/gmail/callback`.
/// `make_state` gets the PKCE verifier and returns the `state` value for the login URL.
/// The server seals the verifier inside `state`, so it needs no table. Returns the login URL.
pub fn web_start(redirect: &str, make_state: impl FnOnce(&str) -> String) -> Res<String> {
    let cfg = Config::load()?;
    let (auth_url, _csrf, _verifier) = authorize(&cfg, redirect.to_string(), make_state)?;
    Ok(auth_url)
}

/// Step 2 of the web flow: the callback has a `code`. Trade it for tokens.
pub async fn web_finish(redirect: &str, code: String, pkce_verifier: String) -> Res<Connected> {
    let cfg = Config::load()?;
    exchange(&cfg, redirect.to_string(), code, PkceCodeVerifier::new(pkce_verifier)).await
}

/// Emails of all saved accounts.
pub fn list_accounts() -> Res<Vec<String>> {
    Ok(load_accounts(&Config::load()?.accounts_file)?.keys().cloned().collect())
}

/// A new access token from a refresh token. No file is used (the engine keeps the token in the DB).
pub async fn refresh(refresh_token: &str) -> Result<Refreshed, RefreshError> {
    let other = |e: Box<dyn std::error::Error + Send + Sync>| RefreshError::Other(e.to_string());
    let cfg = Config::load().map_err(other)?;
    let client = (|| -> Res<_> { Ok(oauth_client!(cfg, cfg.redirect_url.clone())) })().map_err(other)?;
    let token = client
        .exchange_refresh_token(&RefreshToken::new(refresh_token.to_string()))
        .request_async(&http_client().map_err(other)?)
        .await
        .map_err(classify_refresh_error)?;
    Ok(Refreshed {
        access_token: token.access_token().secret().clone(),
        refresh_token: token.refresh_token().map(|t| t.secret().clone()),
        expires_in: token.expires_in().map(|d| d.as_secs()),
    })
}

/// Returns a fresh access token for one saved account.
pub async fn access_token(email: &str) -> Res<String> {
    let cfg = Config::load()?;
    let accounts = load_accounts(&cfg.accounts_file)?;
    let refresh_token = accounts
        .get(email)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("no saved account: {email}"))?;
    Ok(refresh(refresh_token).await?.access_token)
}

/// Removes one saved account.
pub fn remove_account(email: &str) -> Res<bool> {
    let cfg = Config::load()?;
    let mut accounts = load_accounts(&cfg.accounts_file)?;
    let found = accounts.remove(email).is_some();
    save_accounts(&cfg.accounts_file, &accounts)?;
    Ok(found)
}

async fn fetch_email(cfg: &Config, access_token: &str) -> Res<String> {
    let body = http_client()?
        .get(&cfg.profile_url)
        .bearer_auth(access_token)
        .send()
        .await?
        .text()
        .await?;
    let json: Value = serde_json::from_str(&body)?;
    json["emailAddress"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("no email in profile: {body}").into())
}
