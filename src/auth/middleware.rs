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

/// The user an automation run acts as (build map 8.8). Only the automation engine puts this in the
/// request extensions, on a request it sends to the app router in the same process. It is never read from
/// a header, so a caller from outside can not make one: a request that comes in over the network has no way
/// to fill the extensions.
#[derive(Clone, Debug)]
pub struct InternalPrincipal {
    pub user_id: Uuid,
    pub role: crate::users::model::Role,
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
    // An automation run calls the routes as a user. It counts as a session of that user, with no cookie and no
    // CSRF check (there is no browser). An owner or admin acts as the `admin` scope. A member acts as every scope
    // except `admin`, and is refused on the system switches, like a member session.
    if let Some(p) = req.extensions().get::<InternalPrincipal>().cloned() {
        let wants_admin = required == Some(Scope::Admin) || session::admin_only_for_sessions(req.method(), req.uri().path());
        if wants_admin && !p.role.is_admin() {
            return ApiError::forbidden("admin_only", "Only an owner or an admin can do this.").into_response();
        }
        let ctx = session::SessionCtx { session_id: Uuid::nil(), user_id: p.user_id, role: p.role };
        req.extensions_mut().insert(ctx.user_id);
        req.extensions_mut().insert(ctx);
        return next.run(req).await;
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::users::model::Role;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request as HttpRequest, StatusCode};
    use axum::routing::{get, post};
    use tower::ServiceExt;

    fn state() -> AppState {
        crate::automations::testkit::lazy_state()
    }

    fn app(scope: Scope) -> Router {
        let st = state();
        Router::new()
            .route("/probe", get(|| async { "ok" }))
            .route("/v1/sending/engine/pause", post(|| async { "paused" }))
            .route_layer(axum::middleware::from_fn_with_state((st.clone(), scope), require_scope))
            .with_state(st)
    }

    fn req(method: &str, path: &str) -> HttpRequest<Body> {
        HttpRequest::builder().method(method).uri(path).body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn an_internal_principal_in_the_extensions_is_a_session_of_that_user() {
        let mut r = req("GET", "/probe");
        r.extensions_mut().insert(InternalPrincipal { user_id: Uuid::now_v7(), role: Role::Member });
        // a member may use every scope except admin
        assert_eq!(app(Scope::Automations).oneshot(r).await.unwrap().status(), StatusCode::OK);
        let mut r = req("GET", "/probe");
        r.extensions_mut().insert(InternalPrincipal { user_id: Uuid::now_v7(), role: Role::Member });
        assert_eq!(app(Scope::Admin).oneshot(r).await.unwrap().status(), StatusCode::FORBIDDEN);
        // an owner acts as admin
        let mut r = req("GET", "/probe");
        r.extensions_mut().insert(InternalPrincipal { user_id: Uuid::now_v7(), role: Role::Owner });
        assert_eq!(app(Scope::Admin).oneshot(r).await.unwrap().status(), StatusCode::OK);
        // a member is refused on the system switches, like a member session
        let mut r = req("POST", "/v1/sending/engine/pause");
        r.extensions_mut().insert(InternalPrincipal { user_id: Uuid::now_v7(), role: Role::Member });
        assert_eq!(app(Scope::Sending).oneshot(r).await.unwrap().status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn a_caller_from_outside_can_not_fake_an_internal_principal() {
        // headers that look like an internal call, with or without a key: nothing gets in
        for headers in [
            vec![("x-internal-user", "0192f3a0-0000-7000-8000-000000000001"), ("x-internal-role", "owner")],
            vec![("x-automation-principal", "owner"), ("x-user-id", "0192f3a0-0000-7000-8000-000000000001")],
            vec![("x-internal-principal", "owner")],
        ] {
            let mut b = HttpRequest::builder().method("GET").uri("/probe");
            for (k, v) in headers {
                b = b.header(k, v);
            }
            let resp = app(Scope::Automations).oneshot(b.body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        }
    }
}
