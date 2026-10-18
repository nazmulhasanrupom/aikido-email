//! Serves the web app (`ui/dist`, embedded in the binary) at `/`.
//! Any path that is not an API path and not a file gets `index.html` (the app has its own router).
//! An unknown `/v1/...` path stays a JSON 404: the app page never hides an API error.
use crate::error::ApiError;
use crate::state::AppState;
use axum::extract::{Request, State};
#[cfg(feature = "ui")]
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::http::Method;
use axum::response::{IntoResponse, Response};

#[cfg(feature = "ui")]
#[derive(rust_embed::RustEmbed)]
#[folder = "ui/dist/"]
struct Assets;

/// Paths that belong to the server, not to the app.
fn is_server_path(path: &str) -> bool {
    let starts = |p: &str| path == p || path.starts_with(&format!("{p}/"));
    starts("/v1") || starts("/docs") || starts("/openapi.json") || starts("/health")
}

#[cfg(feature = "ui")]
const CSP: &str = "default-src 'self'; img-src 'self' data: https:; style-src 'self' 'unsafe-inline'; font-src 'self'; connect-src 'self'; \
                   frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

#[cfg(feature = "ui")]
fn security_headers(st: &AppState, h: &mut HeaderMap) {
    let set = |h: &mut HeaderMap, name: &'static str, value: &'static str| {
        h.insert(HeaderName::from_static(name), HeaderValue::from_static(value));
    };
    set(h, "content-security-policy", CSP);
    set(h, "x-content-type-options", "nosniff");
    set(h, "referrer-policy", "same-origin");
    set(h, "permissions-policy", "camera=(), microphone=(), geolocation=()");
    if st.config.https() {
        set(h, "strict-transport-security", "max-age=31536000");
    }
}

fn not_found(path: &str) -> Response {
    ApiError::not_found("not_found", format!("No route {path}.")).into_response()
}

/// The router fallback.
pub async fn fallback(State(st): State<AppState>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    if is_server_path(&path) || !matches!(*req.method(), Method::GET | Method::HEAD) {
        return not_found(&path);
    }
    page(&st, &path)
}

#[cfg(not(feature = "ui"))]
fn page(_st: &AppState, path: &str) -> Response {
    not_found(path)
}

#[cfg(feature = "ui")]
fn page(st: &AppState, path: &str) -> Response {
    let mut response = if st.config.plain_http_remote() {
        refused_page()
    } else {
        serve_asset(path)
    };
    security_headers(st, response.headers_mut());
    response
}

#[cfg(feature = "ui")]
fn serve_asset(path: &str) -> Response {
    let name = path.trim_start_matches('/');
    let last = name.rsplit('/').next().unwrap_or("");
    let (file, cache) = match Assets::get(name).filter(|_| !name.is_empty()) {
        Some(f) if name.starts_with("assets/") => (f, "public, max-age=31536000, immutable"),
        Some(f) if name == "index.html" => (f, "no-cache"),
        Some(f) => (f, "public, max-age=3600"),
        // A missing file is a 404. A path with no dot is a route of the app.
        None if name.starts_with("assets/") || last.contains('.') => return not_found(path),
        None => match Assets::get("index.html") {
            Some(f) => (f, "no-cache"),
            None => return not_found(path),
        },
    };
    let mime = if file.metadata.mimetype().is_empty() { "application/octet-stream" } else { file.metadata.mimetype() };
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, mime.to_string()), (header::CACHE_CONTROL, cache.to_string())],
        file.data.into_owned(),
    )
        .into_response()
}

#[cfg(feature = "ui")]
fn refused_page() -> Response {
    let html = "<!doctype html><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>Use HTTPS</title>\
        <body style=\"font-family:system-ui;max-width:36rem;margin:4rem auto;padding:0 1rem;line-height:1.5\">\
        <h1>This address is not safe</h1>\
        <p>The app address uses plain HTTP, so the browser would not keep your login.</p>\
        <p>Open the app over HTTPS. Or, on a network you trust, set <code>AIKIDO_INSECURE_COOKIES=true</code> and restart the server.</p>";
    (StatusCode::SERVICE_UNAVAILABLE, [(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-store")], html).into_response()
}
