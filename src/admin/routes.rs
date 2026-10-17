//! `/v1/keys/*` (scope `admin`)
use crate::auth::api_keys::{self, KeyChangeError, KeyInfo, Scope};
use crate::auth::middleware::require_scope;
use crate::error::{ApiError, ApiJson, ApiPath, ApiResult};
use crate::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn router(state: AppState) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_keys, create_key))
        .routes(routes!(rotate_key))
        .routes(routes!(revoke_key))
        .route_layer(axum::middleware::from_fn_with_state((state, Scope::Admin), require_scope))
}

/// List keys: prefix, scope, name, dates. Never the key itself.
#[utoipa::path(get, path = "/v1/keys", tag = "admin",
    responses((status = 200, body = Vec<KeyInfo>)),
    security(("api_key" = ["admin"])), description = "Scope: `admin`.")]
async fn list_keys(State(st): State<AppState>) -> ApiResult<ApiJson<Vec<KeyInfo>>> {
    Ok(ApiJson(api_keys::list(&st.pool).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct CreateKey {
    pub scope: Scope,
    pub name: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct NewKeyReply {
    pub key: KeyInfo,
    /// The plain key. Shown this one time only. It is not stored.
    pub plain_key: String,
}

/// Make a new key for a scope. The plain key is returned once.
#[utoipa::path(post, path = "/v1/keys", tag = "admin",
    request_body = CreateKey,
    responses((status = 201, body = NewKeyReply)),
    security(("api_key" = ["admin"])), description = "Scope: `admin`. The plain key is shown one time only.")]
async fn create_key(State(st): State<AppState>, ApiJson(body): ApiJson<CreateKey>) -> ApiResult<(StatusCode, ApiJson<NewKeyReply>)> {
    let name = body.name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| format!("{} key", body.scope.as_str()));
    let (key, plain_key) = api_keys::create(&st.pool, body.scope, name.trim()).await?;
    Ok((StatusCode::CREATED, ApiJson(NewKeyReply { key, plain_key })))
}

#[derive(Deserialize, ToSchema, Default)]
pub struct RotateKey {
    /// The old key keeps working this many hours. Default 0 (revoked at once).
    pub grace_hours: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct RotateReply {
    pub key: KeyInfo,
    /// The plain key. Shown this one time only. It is not stored.
    pub plain_key: String,
    pub old_key_id: Uuid,
    /// When the old key stops working.
    pub old_key_revoked_at: DateTime<Utc>,
}

fn key_error(e: KeyChangeError) -> ApiError {
    match e {
        KeyChangeError::NotFound => ApiError::not_found("key_not_found", "No key with this id."),
        KeyChangeError::AlreadyRevoked => ApiError::conflict("key_revoked", "This key is already revoked."),
        KeyChangeError::LastAdmin => ApiError::conflict(
            "last_admin_key",
            "This is the last active admin key. Make another admin key first.",
        ),
        KeyChangeError::Db(e) => e.into(),
    }
}

/// Make a new key and revoke the old one. Optional grace period in hours.
#[utoipa::path(post, path = "/v1/keys/{id}/rotate", tag = "admin",
    params(("id" = Uuid, Path)),
    request_body(content = RotateKey, description = "Optional."),
    responses((status = 200, body = RotateReply)),
    security(("api_key" = ["admin"])), description = "Scope: `admin`.")]
async fn rotate_key(
    State(st): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
    body: axum::body::Bytes,
) -> ApiResult<ApiJson<RotateReply>> {
    let grace = crate::error::parse_optional_json::<RotateKey>(&body)?.grace_hours.unwrap_or(0);
    if !(0..=24 * 365).contains(&grace) {
        return Err(ApiError::unprocessable("invalid_grace", "grace_hours must be 0 to 8760."));
    }
    let (key, plain_key) = api_keys::rotate(&st.pool, id, grace).await.map_err(key_error)?;
    Ok(ApiJson(RotateReply { key, plain_key, old_key_id: id, old_key_revoked_at: Utc::now() + chrono::Duration::hours(grace) }))
}

#[derive(Serialize, ToSchema)]
pub struct RevokedReply {
    pub revoked: bool,
}

/// Revoke a key. Refuses to revoke the last active admin key.
#[utoipa::path(delete, path = "/v1/keys/{id}", tag = "admin",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = RevokedReply), (status = 409, description = "Last admin key, or already revoked")),
    security(("api_key" = ["admin"])), description = "Scope: `admin`.")]
async fn revoke_key(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<RevokedReply>> {
    api_keys::revoke(&st.pool, id).await.map_err(key_error)?;
    Ok(ApiJson(RevokedReply { revoked: true }))
}
