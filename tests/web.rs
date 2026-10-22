mod common;
use axum::http::StatusCode;
use common::*;
use sqlx::PgPool;

#[cfg(feature = "ui")]
#[sqlx::test]
async fn the_app_page_has_security_headers_and_is_not_cached(pool: PgPool) {
    let state = app_state(pool);
    let (status, headers, body) = call_raw(&state, "/", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers["content-type"].to_str().unwrap().starts_with("text/html"));
    assert_eq!(headers["cache-control"], "no-cache");
    let csp = headers["content-security-policy"].to_str().unwrap();
    for part in ["default-src 'self'", "frame-ancestors 'none'", "base-uri 'none'", "form-action 'self'", "connect-src 'self'", "img-src 'self' data:"] {
        assert!(csp.contains(part), "{part} missing in {csp}");
    }
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(headers["referrer-policy"], "same-origin");
    assert!(headers["permissions-policy"].to_str().unwrap().contains("camera=()"));
    // the test config is http://localhost: no HSTS
    assert!(headers.get("strict-transport-security").is_none());
    assert!(String::from_utf8_lossy(&body).contains("<title>"));
}

#[cfg(feature = "ui")]
#[sqlx::test]
async fn hsts_only_for_an_https_public_url(pool: PgPool) {
    let mut state = app_state(pool);
    let mut cfg = (*state.config).clone();
    cfg.public_url = Some("https://mail.example.com".into());
    state.config = std::sync::Arc::new(cfg);
    let (_, headers, _) = call_raw(&state, "/", None).await;
    assert_eq!(headers["strict-transport-security"], "max-age=31536000");
}

#[cfg(feature = "ui")]
#[sqlx::test]
async fn app_routes_get_the_page_but_the_api_keeps_its_404(pool: PgPool) {
    let state = app_state(pool);
    for path in ["/w/default/campaigns", "/login", "/invite/abc", "/onboarding/design"] {
        let (status, headers, body) = call_raw(&state, path, None).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(headers["content-type"].to_str().unwrap().starts_with("text/html"), "{path}");
        assert!(String::from_utf8_lossy(&body).contains("<title>"), "{path}");
    }
    // an API path that does not exist is a JSON 404, never the page
    for path in ["/v1/nope", "/v1/campaigns/12345/nope", "/v1", "/v1/", "/docs/nope", "/health/x", "/openapi.json/x"] {
        let r = call(&state, "GET", path, None, None).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(r.body["error"]["code"], "not_found", "{path}");
    }
    // real API routes still answer as before
    assert_eq!(call(&state, "GET", "/v1/campaigns", None, None).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(call(&state, "GET", "/health", None, None).await.status, StatusCode::OK);
    assert_eq!(call(&state, "GET", "/openapi.json", None, None).await.status, StatusCode::OK);
    assert_eq!(call(&state, "GET", "/docs", None, None).await.status, StatusCode::OK);
}

#[sqlx::test]
async fn missing_files_and_posts_are_404_not_the_page(pool: PgPool) {
    let state = app_state(pool);
    for path in ["/assets/missing-abc123.js", "/missing.js", "/favicon-nope.ico"] {
        let (status, _, _) = call_raw(&state, path, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }
    let r = call(&state, "POST", "/login", None, Some(serde_json::json!({}))).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[cfg(feature = "ui")]
#[sqlx::test]
async fn plain_http_on_another_host_gets_a_clear_page(pool: PgPool) {
    let mut state = app_state(pool);
    let mut cfg = (*state.config).clone();
    cfg.public_url = Some("http://mail.example.com".into());
    state.config = std::sync::Arc::new(cfg);
    let (status, _, body) = call_raw(&state, "/login", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(String::from_utf8_lossy(&body).contains("AIKIDO_INSECURE_COOKIES"));
}

/// An API-only build (`--no-default-features`) has no app page. Every unknown path is a JSON 404.
#[cfg(not(feature = "ui"))]
#[sqlx::test]
async fn an_api_only_build_has_no_page(pool: PgPool) {
    let state = app_state(pool);
    let r = call(&state, "GET", "/", None, None).await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::NOT_FOUND, Some("not_found")));
}
