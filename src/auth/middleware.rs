//! axum layer: check the key and the scope.
use super::api_keys::{self, AuthError, Scope};
use crate::error::ApiError;
use crate::state::AppState;
use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Remembers when `last_used_at` was last written, so the DB is hit at most once a minute per key.
#[derive(Default)]
pub struct LastUsedTracker {
    seen: Mutex<HashMap<Uuid, Instant>>,
}

impl LastUsedTracker {
    pub fn should_write(&self, id: Uuid) -> bool {
        let mut seen = self.seen.lock().unwrap();
        let now = Instant::now();
        match seen.get(&id) {
            Some(t) if now.duration_since(*t) < Duration::from_secs(60) => false,
            _ => {
                seen.insert(id, now);
                true
            }
        }
    }
}

/// `Authorization: Bearer <key>`, or `X-API-Key: <key>`.
pub fn extract_key(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers.get(axum::http::header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        let mut parts = v.splitn(2, ' ');
        if let (Some(scheme), Some(token)) = (parts.next(), parts.next()) {
            if scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty() {
                return Some(token.trim().to_string());
            }
        }
    }
    headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Use with `axum::middleware::from_fn_with_state((state, Scope::X), require_scope)`.
pub async fn require_scope(
    State((state, required)): State<(AppState, Scope)>,
    mut req: Request,
    next: Next,
) -> Response {
    let Some(key) = extract_key(req.headers()) else {
        return ApiError::unauthorized("missing_api_key", "Send your key as 'Authorization: Bearer <key>' or 'X-API-Key: <key>'.")
            .into_response();
    };
    let auth = match api_keys::authenticate(&state.pool, &key).await {
        Ok(a) => a,
        Err(AuthError::Invalid) => {
            return ApiError::unauthorized("invalid_api_key", "The API key is not valid.").into_response();
        }
        Err(AuthError::Db(e)) => return ApiError::from(e).into_response(),
    };
    if !auth.scope.allows(required) {
        return ApiError::forbidden(
            "wrong_scope",
            format!("This route needs a key with scope '{}'.", required.as_str()),
        )
        .into_response();
    }
    if state.last_used.should_write(auth.id) {
        if let Err(e) = api_keys::touch_last_used(&state.pool, auth.id).await {
            tracing::warn!(error = %e, "could not update last_used_at");
        }
    }
    req.extensions_mut().insert(auth.id);
    next.run(req).await
}
