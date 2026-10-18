//! `/v1/integrations/apps*` (scope `integrations`): the app catalog, install, update, uninstall.
use super::store::{self, AppInfo, LoadError};
use crate::auth::api_keys::Scope;
use crate::auth::middleware::require_scope;
use crate::error::{ApiError, ApiJson, ApiPath, ApiResult};
use crate::state::AppState;
use crate::users::session::SessionCtx;
use crate::verify_reoon::client::{Balance, VerifyError};
use axum::Extension;
use axum::extract::State;
use axum::http::StatusCode;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn router(state: AppState) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_apps))
        .routes(routes!(install_app, update_app, uninstall_app))
        .route_layer(axum::middleware::from_fn_with_state((state, Scope::Integrations), require_scope))
}

#[derive(Serialize, ToSchema)]
pub struct AppsReply {
    pub apps: Vec<AppInfo>,
}

/// The app catalog, with the state of each app.
#[utoipa::path(get, path = "/v1/integrations/apps", tag = "integrations",
    responses((status = 200, body = AppsReply)), security(("api_key" = ["integrations"]), ("session" = [])),
    description = "Scope: `integrations`. The web app builds the sidebar from this list: one item for each installed app with `sidebar: true`.")]
async fn list_apps(State(st): State<AppState>) -> ApiResult<ApiJson<AppsReply>> {
    let rows = store::list(&st.pool).await?;
    let apps = store::CATALOG.iter().map(|a| store::info(a, rows.iter().find(|r| r.app == a.app))).collect();
    Ok(ApiJson(AppsReply { apps }))
}

#[derive(Deserialize, ToSchema)]
pub struct InstallBody {
    /// Write only. The Reoon API key.
    pub api_key: String,
}

#[derive(Serialize, ToSchema)]
pub struct InstallReply {
    pub app: AppInfo,
    pub balance: Balance,
}

fn app_or_404(app: &str) -> ApiResult<&'static store::CatalogApp> {
    store::catalog_app(app).ok_or_else(|| ApiError::not_found("unknown_app", "No app with this name."))
}

fn clean_key(k: &str) -> ApiResult<String> {
    let k = k.trim();
    if k.is_empty() || k.len() > 256 || k.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(ApiError::unprocessable("integration_key_invalid", "Reoon did not accept this key. Copy it again from your Reoon API page."));
    }
    Ok(k.to_string())
}

/// Test a key with the balance call.
async fn test_key(st: &AppState, key: &str) -> ApiResult<Balance> {
    match st.verifier.build(SecretString::from(key.to_string())).balance().await {
        Ok(b) => Ok(b),
        Err(VerifyError::Auth) => Err(ApiError::unprocessable("integration_key_invalid", "Reoon did not accept this key. Copy it again from your Reoon API page.")),
        Err(e) => {
            tracing::warn!(code = %e.code(), "Reoon balance check failed");
            Err(ApiError::new(StatusCode::BAD_GATEWAY, "integration_unreachable", "Reoon did not answer. Try again in a minute."))
        }
    }
}

/// Install an app. The key is tested first; a bad key saves nothing.
#[utoipa::path(post, path = "/v1/integrations/apps/{app}/install", tag = "integrations",
    params(("app" = String, Path, description = "`reoon`")), request_body = InstallBody,
    responses((status = 200, body = InstallReply), (status = 409, description = "`already_installed`"),
        (status = 422, description = "`integration_key_invalid`: Reoon refused the key"), (status = 502, description = "`integration_unreachable`")),
    security(("api_key" = ["integrations"]), ("session" = [])),
    description = "Scope: `integrations`. Owner and admin sessions only. Calls the balance route of Reoon first, then saves the key encrypted (AES-256-GCM).")]
async fn install_app(State(st): State<AppState>, ApiPath(app): ApiPath<String>, session: Option<Extension<SessionCtx>>, ApiJson(body): ApiJson<InstallBody>) -> ApiResult<ApiJson<InstallReply>> {
    let cat = app_or_404(&app)?;
    if store::get(&st.pool, cat.app).await?.is_some() {
        return Err(ApiError::conflict("already_installed", "This app is already installed."));
    }
    let key = clean_key(&body.api_key)?;
    let balance = test_key(&st, &key).await?;
    let id = Uuid::now_v7();
    let sealed = st.crypto.encrypt(key.as_bytes(), id.as_bytes()).map_err(|_| ApiError::internal())?;
    let inserted = sqlx::query(
        "INSERT INTO integrations (id, app, secret_ciphertext, secret_nonce, key_version, installed_by, installed_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $7) ON CONFLICT (app) DO NOTHING",
    )
    .bind(id)
    .bind(cat.app)
    .bind(sealed.ciphertext)
    .bind(sealed.nonce)
    .bind(sealed.key_version)
    .bind(session.map(|Extension(s)| s.user_id))
    .bind(st.now())
    .execute(&st.pool)
    .await?;
    if inserted.rows_affected() == 0 {
        return Err(ApiError::conflict("already_installed", "This app is already installed."));
    }
    let row = store::get(&st.pool, cat.app).await?;
    Ok(ApiJson(InstallReply { app: store::info(cat, row.as_ref()), balance }))
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateBody {
    /// Write only. A new key is tested first. It clears `auth_failed`.
    pub api_key: Option<String>,
    /// Merged into the app settings.
    pub settings: Option<Value>,
}

/// Replace the key and/or change the app settings.
#[utoipa::path(patch, path = "/v1/integrations/apps/{app}", tag = "integrations",
    params(("app" = String, Path, description = "`reoon`")), request_body = UpdateBody,
    responses((status = 200, body = AppInfo), (status = 404, description = "`not_installed`"), (status = 422, description = "`integration_key_invalid`")),
    security(("api_key" = ["integrations"]), ("session" = [])),
    description = "Scope: `integrations`. Owner and admin sessions only.")]
async fn update_app(State(st): State<AppState>, ApiPath(app): ApiPath<String>, ApiJson(body): ApiJson<UpdateBody>) -> ApiResult<ApiJson<AppInfo>> {
    let cat = app_or_404(&app)?;
    let Some(row) = store::get(&st.pool, cat.app).await? else {
        return Err(ApiError::not_found("not_installed", "This app is not installed."));
    };
    if let Some(settings) = &body.settings {
        if !settings.is_object() {
            return Err(ApiError::unprocessable("invalid_settings", "settings must be an object."));
        }
    }
    let mut tx = st.pool.begin().await?;
    if let Some(k) = &body.api_key {
        let key = clean_key(k)?;
        test_key(&st, &key).await?;
        let sealed = st.crypto.encrypt(key.as_bytes(), row.id.as_bytes()).map_err(|_| ApiError::internal())?;
        sqlx::query("UPDATE integrations SET secret_ciphertext = $2, secret_nonce = $3, key_version = $4, status = 'installed', updated_at = $5 WHERE id = $1")
            .bind(row.id)
            .bind(sealed.ciphertext)
            .bind(sealed.nonce)
            .bind(sealed.key_version)
            .bind(st.now())
            .execute(&mut *tx)
            .await?;
    }
    if let Some(Value::Object(patch)) = body.settings {
        let mut merged = row.settings.as_object().cloned().unwrap_or_default();
        for (k, v) in patch {
            if v.is_null() {
                merged.remove(&k);
            } else {
                merged.insert(k, v);
            }
        }
        sqlx::query("UPDATE integrations SET settings = $2, updated_at = $3 WHERE id = $1").bind(row.id).bind(Value::Object(merged)).bind(st.now()).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    let row = store::get(&st.pool, cat.app).await?;
    Ok(ApiJson(store::info(cat, row.as_ref())))
}

/// Uninstall. Open runs are cancelled and the key is deleted. The results stay.
#[utoipa::path(delete, path = "/v1/integrations/apps/{app}", tag = "integrations",
    params(("app" = String, Path, description = "`reoon`")),
    responses((status = 204, description = "Uninstalled"), (status = 404, description = "`not_installed`")),
    security(("api_key" = ["integrations"]), ("session" = [])),
    description = "Scope: `integrations`. Owner and admin sessions only. Cancels open verification runs and deletes the key. \
        The rows of `email_verifications` stay, so a reinstall shows them again. The send rule stops working while the app is uninstalled.")]
async fn uninstall_app(State(st): State<AppState>, ApiPath(app): ApiPath<String>) -> ApiResult<StatusCode> {
    let cat = app_or_404(&app)?;
    let mut tx = st.pool.begin().await?;
    let gone = sqlx::query("DELETE FROM integrations WHERE app = $1").bind(cat.app).execute(&mut *tx).await?;
    if gone.rows_affected() == 0 {
        return Err(ApiError::not_found("not_installed", "This app is not installed."));
    }
    sqlx::query("UPDATE verification_items SET state = 'skipped', claim_id = NULL WHERE state IN ('pending', 'claimed') AND run_id IN (SELECT id FROM verification_runs WHERE status IN ('queued', 'running'))")
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE verification_runs SET status = 'cancelled', finished_at = $1 WHERE status IN ('queued', 'running')").bind(st.now()).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Shared by the verify routes: the 4xx answer for a failed `load_client`.
pub fn load_error(e: LoadError) -> ApiError {
    match e {
        LoadError::NotInstalled => ApiError::not_found("not_installed", "Reoon is not installed."),
        LoadError::BadKey => {
            tracing::error!("the saved Reoon key can not be decrypted");
            ApiError::internal()
        }
        LoadError::Db(e) => e.into(),
    }
}
