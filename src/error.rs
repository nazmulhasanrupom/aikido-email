//! One error type for the HTTP API. Always renders as
//! `{ "error": { "code": "snake_case", "message": "..." } }`.
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: String,
    pub message: String,
    /// Seconds to wait. Sent as the `Retry-After` header and as `error.retry_after_secs`.
    pub retry_after: Option<i64>,
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn new(status: StatusCode, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self { status, code: code.into(), message: message.into(), retry_after: None }
    }
    pub fn bad_request(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }
    pub fn unauthorized(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, code, message)
    }
    pub fn forbidden(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, code, message)
    }
    pub fn not_found(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, code, message)
    }
    pub fn conflict(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }
    pub fn unprocessable(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, code, message)
    }
    pub fn too_many(code: impl Into<String>, message: impl Into<String>, retry_after: i64) -> Self {
        let mut e = Self::new(StatusCode::TOO_MANY_REQUESTS, code, message);
        e.retry_after = Some(retry_after);
        e
    }
    pub fn internal() -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", "Something went wrong on the server.")
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut error = json!({ "code": self.code, "message": self.message });
        let response = match self.retry_after {
            Some(secs) => {
                error["retry_after_secs"] = json!(secs);
                let mut r = (self.status, axum::Json(json!({ "error": error }))).into_response();
                r.headers_mut().insert(axum::http::header::RETRY_AFTER, axum::http::HeaderValue::from(secs.max(1) as u64));
                r
            }
            None => (self.status, axum::Json(json!({ "error": error }))).into_response(),
        };
        response
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        tracing::error!(error = %e, "database error");
        Self::internal()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        // `{:#}` prints the chain. Error text never holds secrets (see crypto and senders).
        tracing::error!(error = %format!("{e:#}"), "internal error");
        Self::internal()
    }
}

impl From<JsonRejection> for ApiError {
    fn from(r: JsonRejection) -> Self {
        let status = match r {
            JsonRejection::JsonDataError(_) | JsonRejection::JsonSyntaxError(_) => StatusCode::UNPROCESSABLE_ENTITY,
            _ => r.status(),
        };
        Self::new(status, "invalid_json", r.body_text())
    }
}

impl From<PathRejection> for ApiError {
    fn from(r: PathRejection) -> Self {
        Self::bad_request("invalid_path", r.body_text())
    }
}

impl From<QueryRejection> for ApiError {
    fn from(r: QueryRejection) -> Self {
        Self::bad_request("invalid_query", r.body_text())
    }
}

/// JSON body extractor that fails with our error shape.
#[derive(axum::extract::FromRequest)]
#[from_request(via(axum::Json), rejection(ApiError))]
pub struct ApiJson<T>(pub T);

/// Path extractor that fails with our error shape.
#[derive(axum::extract::FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(ApiError))]
pub struct ApiPath<T>(pub T);

/// Query extractor that fails with our error shape.
#[derive(axum::extract::FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(ApiError))]
pub struct ApiQuery<T>(pub T);

impl<T: serde::Serialize> IntoResponse for ApiJson<T> {
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

/// An optional JSON body: an empty body means "all defaults".
pub fn parse_optional_json<T: serde::de::DeserializeOwned + Default>(body: &[u8]) -> Result<T, ApiError> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(body).map_err(|e| ApiError::unprocessable("invalid_json", e.to_string()))
}
