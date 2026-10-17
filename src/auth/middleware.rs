//! axum layer: check the key and the scope.
use super::api_keys::{self, AuthError, Scope};
use crate::error::ApiError;
use crate::state::AppState;
use crate::users::session;
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

/// The scope of the API key that made the call. Put in the request extensions.
#[derive(Clone, Copy, Debug)]
pub struct KeyScope(pub Scope);

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
/// A call is allowed with an API key of the scope, or with a web session.
/// Owner and admin sessions act as the `admin` scope. A member session acts as every scope
/// except `admin`, and is also refused on the system switches (`session::admin_only_for_sessions`).
pub async fn require_scope(State((state, required)): State<(AppState, Scope)>, req: Request, next: Next) -> Response {
    authorize(state, Some(required), req, next).await
}

/// Any valid key (any scope) or any session. For the routes that only help the web app.
pub async fn require_any(State(state): State<AppState>, req: Request, next: Next) -> Response {
    authorize(state, None, req, next).await
}

async fn authorize(state: AppState, required: Option<Scope>, mut req: Request, next: Next) -> Response {
    let Some(key) = extract_key(req.headers()) else {
        return match session::resolve(&state, req.method(), req.headers()).await {
            Ok(Some(ctx)) => {
                let wants_admin = required == Some(Scope::Admin) || session::admin_only_for_sessions(req.method(), req.uri().path());
                if wants_admin && !ctx.role.is_admin() {
                    return ApiError::forbidden("admin_only", "Only an owner or an admin can do this.").into_response();
                }
                req.extensions_mut().insert(ctx.user_id);
                req.extensions_mut().insert(ctx);
                next.run(req).await
            }
            Ok(None) => ApiError::unauthorized("missing_api_key", "Send your key as 'Authorization: Bearer <key>' or 'X-API-Key: <key>', or log in.")
                .into_response(),
            Err(e) => e.into_response(),
        };
    };
    let auth = match api_keys::authenticate(&state.pool, &key).await {
        Ok(a) => a,
        Err(AuthError::Invalid) => {
            return ApiError::unauthorized("invalid_api_key", "The API key is not valid.").into_response();
        }
        Err(AuthError::Db(e)) => return ApiError::from(e).into_response(),
    };
    if let Some(required) = required {
        if !auth.scope.allows(required) {
            return ApiError::forbidden(
                "wrong_scope",
                format!("This route needs a key with scope '{}'.", required.as_str()),
            )
            .into_response();
        }
    }
    if state.last_used.should_write(auth.id) {
        if let Err(e) = api_keys::touch_last_used(&state.pool, auth.id).await {
            tracing::warn!(error = %e, "could not update last_used_at");
        }
    }
    req.extensions_mut().insert(auth.id);
    req.extensions_mut().insert(KeyScope(auth.scope));
    next.run(req).await
}
