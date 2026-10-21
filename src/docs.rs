//! The OpenAPI spec and the `/docs` page. Every route is registered through `OpenApiRouter`,
//! so a route cannot exist without being in the spec. A test checks this.
use crate::state::AppState;
use axum::{Json, Router, extract::State, routing::get};
use serde::Serialize;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, Http, HttpAuthScheme, SecurityScheme};
use utoipa::{Modify, OpenApi, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use utoipa_scalar::{Scalar, Servable};

struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_with(Default::default);
        let mut scheme = Http::new(HttpAuthScheme::Bearer);
        scheme.description = Some(
            "Send `Authorization: Bearer <key>` (or `X-API-Key: <key>`). Each route lists the scope it needs. \
             A key with scope `admin` works on every route."
                .into(),
        );
        components.add_security_scheme("api_key", SecurityScheme::Http(scheme));
        components.add_security_scheme(
            "session",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
                "__Host-aikido_session",
                "The web login cookie. Calls that change data also need the `X-CSRF-Token` header and an `Origin` that is this app.",
            ))),
        );
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "AIKIDO EMAIL Sending Engine",
        version = "0.1.0",
        description = "Stores connected email accounts and sends queued emails one by one, with human-like timing."
    ),
    modifiers(&SecurityAddon),
    tags(
        (name = "accounts", description = "Connected email accounts. Scope: accounts"),
        (name = "sending", description = "Queue, rules, and engine control. Scope: sending"),
        (name = "replies", description = "Reply check: inbox reading, replies, bounces. Scope: replies"),
        (name = "followups", description = "Follow-up sequences and enrollments. Scope: followups"),
        (name = "leads", description = "Leads, lists, tags, import and export, email check. Scope: leads"),
        (name = "campaigns", description = "Campaigns: a sequence, sender accounts and leads. Scope: campaigns"),
        (name = "generator", description = "Email Generator: AI text for the slots of a step, providers, review, usage. Scope: generator"),
        (name = "integrations", description = "Apps (Reoon email verifier): install, verify leads, send rule. Scope: integrations"),
        (name = "tracking", description = "Open tracking: tracking domains, campaign settings, previews, open numbers. Scope: tracking. The image routes `/i/...` and `/p/...` need no key"),
        (name = "dev", description = "Developer tools: live log, SQL console, tables, controls. Scope: admin (owner and admin)"),
        (name = "admin", description = "API keys. Scope: admin"),
        (name = "auth", description = "Web login, first-boot setup, invites. No key needed"),
        (name = "users", description = "Web users, sessions, team. Needs a web session"),
        (name = "ui", description = "Helpers for the web app: config, onboarding, alerts. A key or a session"),
        (name = "reports", description = "Daily numbers for charts. Scope: campaigns"),
        (name = "system", description = "No key needed"),
    )
)]
struct ApiDoc;

#[derive(Serialize, ToSchema)]
pub struct Health {
    pub status: &'static str,
}

/// The server and the database are up.
#[utoipa::path(get, path = "/health", tag = "system",
    responses((status = 200, body = Health), (status = 503, description = "The database does not answer")),
    description = "No key needed.")]
async fn health(State(st): State<AppState>) -> Result<Json<Health>, crate::error::ApiError> {
    sqlx::query("SELECT 1").execute(&st.pool).await.map_err(|e| {
        tracing::error!(error = %e, "health check failed");
        crate::error::ApiError::new(axum::http::StatusCode::SERVICE_UNAVAILABLE, "database_down", "The database does not answer.")
    })?;
    Ok(Json(Health { status: "ok" }))
}

/// All routes and the OpenAPI spec that describes them.
pub fn api_router(state: AppState) -> (axum::Router<AppState>, utoipa::openapi::OpenApi) {
    let (router, api) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        .routes(routes!(health))
        .merge(crate::accounts::routes::public_router())
        .merge(crate::accounts::routes::router(state.clone()))
        .merge(crate::sending::unsubscribe::public_router())
        .merge(crate::tracking::server::public_router())
        .merge(crate::sending::routes::router(state.clone()))
        .merge(crate::replies::routes::router(state.clone()))
        .merge(crate::followups::routes::router(state.clone()))
        .merge(crate::leads::routes::router(state.clone()))
        .merge(crate::campaigns::routes::router(state.clone()))
        .merge(crate::generator::routes::router(state.clone()))
        .merge(crate::integrations::routes::router(state.clone()))
        .merge(crate::verify_reoon::routes::router(state.clone()))
        .merge(crate::prompting::routes::router(state.clone()))
        .merge(crate::tracking::routes::router(state.clone()))
        .merge(crate::devtools::routes::router(state.clone()))
        .merge(crate::reports::routes::router(state.clone()))
        .merge(crate::admin::routes::router(state.clone()))
        .merge(crate::users::routes::public_router())
        .merge(crate::users::routes::router(state.clone()))
        .merge(crate::ui_api::routes::router(state))
        .split_for_parts();
    (router, api)
}

/// The full app: the routes, `/openapi.json`, and `/docs`.
pub fn app(state: AppState) -> Router {
    let (router, api) = api_router(state.clone());
    let spec = api.clone();
    router
        .route("/openapi.json", get(move || async move { Json(spec) }))
        .merge(Scalar::with_url("/docs", api))
        .fallback(crate::web::fallback)
        .layer(axum::middleware::from_fn(crate::devtools::log_requests))
        .layer(axum::extract::DefaultBodyLimit::max(25 * 1024 * 1024))
        .layer(tower_http::cors::CorsLayer::permissive())
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(
            axum::http::StatusCode::REQUEST_TIMEOUT,
            std::time::Duration::from_secs(60),
        ))
        .layer(
            // Method and path only. The query string of the OAuth callback holds a login code.
            tower_http::trace::TraceLayer::new_for_http().make_span_with(|req: &axum::http::Request<_>| {
                tracing::info_span!("http", method = %req.method(), path = %req.uri().path())
            }),
        )
        .with_state(state)
}
