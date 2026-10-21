//! Developer tools (Settings > Developer): a live log of what the server does, a SQL console, and a few controls.
//! Owner and admin only (and keys of scope `admin`). `AIKIDO_DEV_TOOLS=false` turns the routes off.
pub mod logs;
pub mod routes;

pub use logs::{Dev, init_logging};

/// One log line for each request, inside the request span: the status and the time. Errors are `error` (5xx) and
/// `warn` (4xx), changes are `info`, and reads are `debug`. The panel's own polling and `/health` are left out.
pub async fn log_requests(req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let quiet = path == "/health" || (method == axum::http::Method::GET && path.starts_with("/v1/dev/")) || path.starts_with("/assets/");
    let started = std::time::Instant::now();
    let resp = next.run(req).await;
    if quiet {
        return resp;
    }
    let status = resp.status().as_u16();
    let ms = started.elapsed().as_millis() as u64;
    let public = path.starts_with("/i/") || path.starts_with("/p/");
    if status >= 500 {
        tracing::error!(status, ms, "request failed");
    } else if status >= 400 {
        tracing::warn!(status, ms, "request refused");
    } else if method != axum::http::Method::GET && method != axum::http::Method::HEAD && !public {
        tracing::info!(status, ms, "request done");
    } else {
        tracing::debug!(status, ms, "request done");
    }
    resp
}
