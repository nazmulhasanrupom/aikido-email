//! The public routes of the tracking server. No key, no cookie. They answer on the tracking domains
//! (`https://media.acme.com/i/<token>.png`), and also on the main address.
//!
//! On each request: look up the token, save the open in the background (time, IP, user agent, proxy or real),
//! and send the image. An unknown token still gets an image (a clear 1 pixel PNG), never an error.
use super::proxy;
use super::render::blank_png;
use crate::state::AppState;
use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use sqlx::FromRow;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn public_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(text_image)).routes(routes!(signature_image)).routes(routes!(acme_challenge))
}

/// Only these three routes, for the tracking listeners (HTTP and HTTPS). The API and the web app are not on them.
pub fn public_app(st: AppState) -> Router {
    let (router, _) = public_router().split_for_parts();
    router.fallback(|| async { StatusCode::NOT_FOUND }).with_state(st)
}

/// A token is 22 letters and digits.
pub fn valid_token(t: &str) -> bool {
    t.len() == 22 && t.bytes().all(|b| b.is_ascii_alphanumeric())
}

#[derive(FromRow)]
struct Hit {
    send_job_id: Uuid,
    campaign_id: Option<Uuid>,
    lead_id: Option<Uuid>,
    file: Vec<u8>,
    content_type: String,
    sent_at: Option<DateTime<Utc>>,
}

async fn lookup(st: &AppState, token: &str) -> Result<Option<Hit>, sqlx::Error> {
    sqlx::query_as(
        "SELECT t.send_job_id, t.campaign_id, t.lead_id, a.file, a.content_type, j.sent_at
           FROM tracking_tokens t JOIN tracking_assets a ON a.id = t.asset_id LEFT JOIN send_jobs j ON j.id = t.send_job_id
          WHERE t.token = $1",
    )
    .bind(token)
    .fetch_optional(&st.pool)
    .await
}

fn image_response(bytes: Vec<u8>, content_type: &str) -> Response {
    let mut r = (StatusCode::OK, bytes).into_response();
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_str(content_type).unwrap_or(HeaderValue::from_static("image/png")));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache, no-store, must-revalidate"));
    h.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    h.insert(header::EXPIRES, HeaderValue::from_static("0"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    // `Content-Length` is set by the body
    r
}

async fn serve(st: AppState, token: &str, req: Request) -> Response {
    if !valid_token(token) {
        return image_response(blank_png(), "image/png");
    }
    match lookup(&st, token).await {
        Ok(Some(hit)) => {
            if req.method() == Method::GET {
                let ip = crate::users::session::client_ip(&st.config, req.headers(), req.extensions());
                let ua = crate::users::session::user_agent(req.headers());
                let token = token.to_string();
                let (job, campaign, lead, sent_at) = (hit.send_job_id, hit.campaign_id, hit.lead_id, hit.sent_at);
                // the image does not wait for the database
                tokio::spawn(async move { record_open(&st, &token, job, campaign, lead, sent_at, &ip, &ua).await });
            }
            image_response(hit.file, &hit.content_type)
        }
        Ok(None) => image_response(blank_png(), "image/png"),
        Err(e) => {
            tracing::warn!(error = %e, "tracking lookup failed");
            image_response(blank_png(), "image/png")
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn record_open(st: &AppState, token: &str, job: Uuid, campaign: Option<Uuid>, lead: Option<Uuid>, sent_at: Option<DateTime<Utc>>, ip: &str, ua: &str) {
    let now = st.now();
    let extra: Vec<proxy::Cidr> = st.config.tracking.proxy_cidrs.iter().filter_map(|c| proxy::Cidr::parse(c)).collect();
    let ip_opt = Some(ip).filter(|i| *i != "unknown");
    let kind = proxy::classify(ip_opt, ua, sent_at.map(|s| (now - s).num_seconds()), &proxy::apple_ranges(&extra));
    let res = sqlx::query(
        "INSERT INTO email_opens (token, send_job_id, lead_id, campaign_id, ip, user_agent, is_proxy, proxy_kind, opened_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(token)
    .bind(job)
    .bind(lead)
    .bind(campaign)
    .bind(ip_opt)
    .bind(Some(ua).filter(|u| !u.is_empty()))
    .bind(kind.is_some())
    .bind(kind.map(|k| k.as_str()))
    .bind(now)
    .execute(&st.pool)
    .await;
    if let Err(e) = res {
        tracing::warn!(error = %e, "could not save an open");
    }
}

/// The text image of Type 1.
#[utoipa::path(get, path = "/i/{file}", tag = "tracking",
    params(("file" = String, Path, description = "`<token>.png`")),
    responses((status = 200, description = "The image (PNG). An unknown token gets a transparent 1 pixel PNG, never an error.", content_type = "image/png")),
    description = "No key needed. Public, on the tracking domains. Saves an open in the background. `Cache-Control: no-cache, no-store, must-revalidate`.")]
async fn text_image(State(st): State<AppState>, Path(file): Path<String>, req: Request) -> Response {
    let token = file.strip_suffix(".png").unwrap_or(&file).to_string();
    serve(st, &token, req).await
}

/// The signature image of Type 2: the stored copy of the image in the email. No redirect.
#[utoipa::path(get, path = "/p/{token}", tag = "tracking",
    params(("token" = String, Path, description = "The token of the email")),
    responses((status = 200, description = "The stored image. An unknown token gets a transparent 1 pixel PNG, never an error.", content_type = "image/png")),
    description = "No key needed. Public, on the tracking domains. Saves an open in the background.")]
async fn signature_image(State(st): State<AppState>, Path(token): Path<String>, req: Request) -> Response {
    serve(st, &token, req).await
}

/// The answer to a Let's Encrypt check (HTTP-01).
#[utoipa::path(get, path = "/.well-known/acme-challenge/{token}", tag = "tracking",
    params(("token" = String, Path, description = "The challenge token")),
    responses((status = 200, description = "The key authorization, plain text"), (status = 404, description = "Unknown token")),
    description = "No key needed. Let's Encrypt calls it on port 80 to check that we own a tracking domain.")]
async fn acme_challenge(State(st): State<AppState>, Path(token): Path<String>) -> Response {
    if token.len() > 100 || !token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return StatusCode::NOT_FOUND.into_response();
    }
    match super::acme::challenge_answer(&st.pool, &token).await {
        Ok(Some(answer)) => ([(header::CONTENT_TYPE, "text/plain")], answer).into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_22_letters_and_digits() {
        assert!(valid_token("aB3dE6gH9jK2mN5pQ8sT1u"));
        assert!(!valid_token("short"));
        assert!(!valid_token("aB3dE6gH9jK2mN5pQ8sT1!"));
        assert!(!valid_token("aB3dE6gH9jK2mN5pQ8sT1uX"));
    }

    #[test]
    fn the_image_headers_stop_caching() {
        let r = image_response(vec![1, 2, 3], "image/png");
        assert_eq!(r.headers()[header::CACHE_CONTROL], "no-cache, no-store, must-revalidate");
        assert_eq!(r.headers()[header::CONTENT_TYPE], "image/png");
    }
}
