//! Session cookie, CSRF check, Origin check and the client IP.
use super::model::Role;
use super::repo;
use crate::config::Config;
use crate::error::ApiError;
use crate::state::AppState;
use axum::http::{HeaderMap, HeaderValue, Method, header};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64URL};
use chrono::Duration;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use uuid::Uuid;

/// Who is calling, when the call came with a session cookie. Put in the request extensions.
#[derive(Clone, Debug)]
pub struct SessionCtx {
    pub session_id: Uuid,
    pub user_id: Uuid,
    pub role: Role,
}

pub fn cookie_name(cfg: &Config) -> &'static str {
    if cfg.insecure_cookies { "aikido_session" } else { "__Host-aikido_session" }
}

/// The session token from the `Cookie` header.
pub fn cookie_token(cfg: &Config, headers: &HeaderMap) -> Option<String> {
    let name = cookie_name(cfg);
    headers.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).find_map(|pair| {
        let (k, v) = pair.trim().split_once('=')?;
        (k == name && !v.is_empty()).then(|| v.to_string())
    })
}

pub fn set_cookie(cfg: &Config, token: &str) -> HeaderValue {
    let secure = if cfg.insecure_cookies { "" } else { "; Secure" };
    let max_age = repo::ABSOLUTE_TIMEOUT_DAYS * 86_400;
    HeaderValue::from_str(&format!("{}={token}; HttpOnly{secure}; SameSite=Strict; Path=/; Max-Age={max_age}", cookie_name(cfg)))
        .expect("a base64url token is a valid header value")
}

pub fn clear_cookie(cfg: &Config) -> HeaderValue {
    let secure = if cfg.insecure_cookies { "" } else { "; Secure" };
    HeaderValue::from_str(&format!("{}=; HttpOnly{secure}; SameSite=Strict; Path=/; Max-Age=0", cookie_name(cfg))).expect("valid header value")
}

/// The CSRF token of a session. It is made from the cookie token with a key derived from the
/// master key, so `/v1/auth/state` can give it again after a page reload. The database holds its hash.
pub fn csrf_token(state: &AppState, session_token: &str) -> String {
    let key = state.crypto.derive_key("aikido-csrf-v1");
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&*key).expect("HMAC takes any key length");
    mac.update(session_token.as_bytes());
    B64URL.encode(mac.finalize().into_bytes())
}

/// The client address, for rate limits and the session list.
/// Behind your own proxy (`AIKIDO_TRUST_PROXY`) it is the last `X-Forwarded-For` entry.
pub fn client_ip(cfg: &Config, headers: &HeaderMap, ext: &axum::http::Extensions) -> String {
    if cfg.trust_proxy {
        if let Some(ip) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit(',').next())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty() && v.len() <= 64)
        {
            return ip;
        }
    }
    ext.get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|c| c.0.ip().to_string()).unwrap_or_else(|| "unknown".into())
}

pub fn user_agent(headers: &HeaderMap) -> String {
    headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or("").chars().take(200).collect()
}

fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    Some(format!("{}://{}", scheme.to_lowercase(), authority.to_lowercase()))
}

/// The page that sent this request: `Origin`, else the origin part of `Referer`.
fn sender_origin(headers: &HeaderMap) -> Option<String> {
    let get = |name| headers.get(name).and_then(|v| v.to_str().ok());
    match get(header::ORIGIN) {
        Some("null") => None,
        Some(o) => origin_of(o),
        None => get(header::REFERER).and_then(origin_of),
    }
}

/// `true` when the request comes from our own page: its origin equals `AIKIDO_PUBLIC_URL`
/// (or, if that is not set, the `Host` header).
pub fn origin_matches(cfg: &Config, headers: &HeaderMap) -> bool {
    let Some(sender) = sender_origin(headers) else { return false };
    if let Some(public) = cfg.public_url.as_deref().and_then(origin_of) {
        return sender == public;
    }
    let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).map(str::to_lowercase);
    host.is_some_and(|h| sender.ends_with(&format!("://{h}")))
}

pub fn has_origin_header(headers: &HeaderMap) -> bool {
    headers.contains_key(header::ORIGIN) || headers.contains_key(header::REFERER)
}

fn is_safe(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

fn csrf_failed() -> ApiError {
    ApiError::forbidden("csrf_failed", "The request did not come from this app. Reload the page and try again.")
}

/// Look up the session of a request. `Ok(None)` means no valid session cookie.
/// For a method that changes data it also checks `Origin` and the `X-CSRF-Token` header.
pub async fn resolve(state: &AppState, method: &Method, headers: &HeaderMap) -> Result<Option<SessionCtx>, ApiError> {
    let Some(token) = cookie_token(&state.config, headers) else { return Ok(None) };
    let Some((s, user)) = repo::lookup_session(&state.pool, &token, state.now()).await? else { return Ok(None) };
    if !is_safe(method) {
        if !origin_matches(&state.config, headers) {
            return Err(csrf_failed());
        }
        let sent = headers.get("x-csrf-token").and_then(|v| v.to_str().ok()).unwrap_or("");
        if sent.is_empty() || !bool::from(repo::sha256(sent).ct_eq(&s.csrf_token_hash)) {
            return Err(csrf_failed());
        }
    }
    // Write the time at most once a minute per session.
    if state.now() - s.last_seen_at >= Duration::minutes(1) {
        if let Err(e) = repo::touch_session(&state.pool, s.session_id, state.now()).await {
            tracing::warn!(error = %e, "could not update the session time");
        }
    }
    Ok(Some(SessionCtx { session_id: s.session_id, user_id: user.id, role: user.role() }))
}

/// Routes that only owner and admin may call with a session, though their key scope is not `admin`.
/// These are the system-wide switches and defaults. A member can read them.
pub fn admin_only_for_sessions(method: &Method, path: &str) -> bool {
    if is_safe(method) {
        return false;
    }
    const EXACT: &[&str] = &[
        "/v1/sending/engine/pause",
        "/v1/sending/engine/resume",
        "/v1/replies/engine/pause",
        "/v1/replies/engine/resume",
        "/v1/followups/pause",
        "/v1/followups/resume",
        "/v1/campaigns/feeder/pause",
        "/v1/campaigns/feeder/resume",
        "/v1/generator/pause",
        "/v1/generator/resume",
        "/v1/sending/defaults",
        "/v1/replies/defaults",
        "/v1/followups/settings",
        "/v1/verify/reoon/pause",
        "/v1/verify/reoon/resume",
    ];
    EXACT.contains(&path.trim_end_matches('/')) || path.starts_with("/v1/keys") || path.starts_with("/v1/integrations/apps") || path.starts_with("/v1/ui/onboarding") || path.starts_with("/v1/ui/workspace")
}
