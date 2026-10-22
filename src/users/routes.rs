//! `/v1/auth/*` (no key) and `/v1/users/*` (a web session).
use super::model::{Role, UserOut, UserRow};
use super::password::{self, PolicyError};
use super::rate;
use super::repo::{self, SessionInfo};
use super::session::{self, SessionCtx};
use crate::error::{ApiError, ApiJson, ApiPath, ApiResult};
use crate::state::AppState;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn public_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(auth_state))
        .routes(routes!(setup))
        .routes(routes!(login))
        .routes(routes!(logout))
        .routes(routes!(invite_info))
        .routes(routes!(invite_accept))
        .routes(routes!(reset_info))
        .routes(routes!(reset_accept))
}

pub fn router(state: AppState) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_me, patch_me))
        .routes(routes!(change_password))
        .routes(routes!(list_my_sessions))
        .routes(routes!(end_my_session))
        .routes(routes!(list_users))
        .routes(routes!(patch_user, delete_user))
        .routes(routes!(create_invite, list_invites))
        .routes(routes!(delete_invite))
        .routes(routes!(transfer_ownership))
        .routes(routes!(make_reset_link))
        .route_layer(axum::middleware::from_fn_with_state(state, require_session))
}

/// The routes under `/v1/users` need a session. A key does not work here.
async fn require_session(State(st): State<AppState>, mut req: Request, next: Next) -> Response {
    match session::resolve(&st, req.method(), req.headers()).await {
        Ok(Some(ctx)) => {
            req.extensions_mut().insert(ctx);
            next.run(req).await
        }
        Ok(None) => ApiError::unauthorized("not_logged_in", "Log in first.").into_response(),
        Err(e) => e.into_response(),
    }
}

/// The client address as a request extractor.
pub struct ClientIp(pub String);

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        Ok(ClientIp(session::client_ip(&state.config, &parts.headers, &parts.extensions)))
    }
}

// ---------- helpers ----------

fn bad_login() -> ApiError {
    ApiError::unauthorized("invalid_credentials", "Email or password is wrong.")
}

fn wait_text(secs: i64) -> String {
    if secs >= 90 {
        format!("{} minutes", (secs + 59) / 60)
    } else if secs == 1 {
        "1 second".into()
    } else {
        format!("{secs} seconds")
    }
}

fn policy(e: PolicyError) -> ApiError {
    ApiError::unprocessable(e.code(), e.message())
}

/// A page on plain HTTP (not this machine) cannot keep the cookie. Say so, not "wrong password".
fn guard_transport(st: &AppState) -> ApiResult<()> {
    if st.config.plain_http_remote() {
        return Err(ApiError::bad_request(
            "insecure_connection",
            "This address uses plain HTTP, so the browser would not keep the login. Use HTTPS, or set AIKIDO_INSECURE_COOKIES=true on a trusted network.",
        ));
    }
    Ok(())
}

/// For calls without a session (login, setup, accept): if a browser says where it came from, it must be our page.
fn guard_origin(st: &AppState, headers: &HeaderMap) -> ApiResult<()> {
    if session::has_origin_header(headers) && !session::origin_matches(&st.config, headers) {
        return Err(ApiError::forbidden("csrf_failed", "The request did not come from this app."));
    }
    Ok(())
}

fn valid_name(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty() && n.chars().count() <= 100 && !n.chars().any(char::is_control)
}

fn check_identity(name: &str, email: &str) -> ApiResult<()> {
    if !valid_name(name) {
        return Err(ApiError::unprocessable("invalid_name", "Give a name (up to 100 characters)."));
    }
    if !repo::valid_email(email) {
        return Err(ApiError::unprocessable("invalid_email", "This email address does not look right."));
    }
    Ok(())
}

type CookieHeader = [(HeaderName, HeaderValue); 1];

/// Make a session for a user and the `Set-Cookie` header for it.
async fn start_session(st: &AppState, user_id: Uuid, ip: &str, headers: &HeaderMap) -> ApiResult<(CookieHeader, String)> {
    let token = repo::random_token();
    let csrf = session::csrf_token(st, &token);
    repo::create_session(&st.pool, user_id, &token, &repo::sha256(&csrf), ip, &session::user_agent(headers), st.now()).await?;
    Ok(([(header::SET_COOKIE, session::set_cookie(&st.config, &token))], csrf))
}

/// Check a password of a logged-in user, with the same rate limits as the login.
async fn verify_current(st: &AppState, user: &UserRow, password: &str, ip: &str) -> ApiResult<()> {
    let now = st.now();
    let keys = [rate::ip_key(ip), rate::email_key(&user.email)];
    if let Some(secs) = rate::blocked_for(&st.pool, &keys, now).await? {
        return Err(ApiError::too_many("too_many_attempts", format!("Too many tries. Try again in {}.", wait_text(secs)), secs));
    }
    if password.len() > 1024 || !st.passwords.verify(password, Some(&user.password_hash)).await {
        rate::record_failure(&st.pool, ip, &user.email, now).await?;
        return Err(ApiError::forbidden("wrong_password", "The current password is wrong."));
    }
    Ok(())
}

async fn current_user(st: &AppState, ctx: &SessionCtx) -> ApiResult<UserRow> {
    repo::find(&st.pool, ctx.user_id).await?.ok_or_else(|| ApiError::unauthorized("not_logged_in", "Log in first."))
}

fn need_admin(ctx: &SessionCtx) -> ApiResult<()> {
    if ctx.role.is_admin() { Ok(()) } else { Err(ApiError::forbidden("admin_only", "Only an owner or an admin can do this.")) }
}

// ---------- state ----------

#[derive(Serialize, ToSchema)]
pub struct AuthStateReply {
    /// No user exists yet: show the setup page.
    pub setup_needed: bool,
    pub logged_in: bool,
    pub user: Option<UserOut>,
    /// Send this in `X-CSRF-Token` on every call that is not a GET.
    pub csrf_token: Option<String>,
    pub workspace_name: String,
    pub workspace_logo: Option<String>,
    /// The app address is plain HTTP on another host: login will not work. Show a clear page.
    pub insecure_connection: bool,
}

/// First call of the web app: is setup needed, is the user logged in.
#[utoipa::path(get, path = "/v1/auth/state", tag = "auth",
    responses((status = 200, body = AuthStateReply)),
    description = "No key needed. Reads the session cookie, if there is one.")]
async fn auth_state(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<ApiJson<AuthStateReply>> {
    let app = repo::app_state(&st.pool).await?;
    let setup_needed = repo::count_users(&st.pool).await? == 0;
    let (mut user, mut csrf_token) = (None, None);
    // A GET: the session is read, the CSRF check does not apply.
    if let Some(ctx) = session::resolve(&st, &axum::http::Method::GET, &headers).await? {
        if let Some(u) = repo::find(&st.pool, ctx.user_id).await? {
            user = Some(u.out(st.now()));
            csrf_token = session::cookie_token(&st.config, &headers).map(|t| session::csrf_token(&st, &t));
        }
    }
    Ok(ApiJson(AuthStateReply {
        setup_needed,
        logged_in: user.is_some(),
        user,
        csrf_token,
        workspace_name: app.workspace_name,
        workspace_logo: app.workspace_logo,
        insecure_connection: st.config.plain_http_remote(),
    }))
}

// ---------- setup ----------

#[derive(Deserialize, ToSchema)]
pub struct SetupBody {
    pub name: String,
    pub email: String,
    pub password: String,
    pub workspace_name: String,
}

#[derive(Serialize, ToSchema)]
pub struct LoginReply {
    pub user: UserOut,
    pub csrf_token: String,
}

/// Make the owner account on a fresh server. Works one time.
#[utoipa::path(post, path = "/v1/auth/setup", tag = "auth",
    request_body = SetupBody,
    responses((status = 201, body = LoginReply), (status = 409, description = "setup_done")),
    description = "No key needed. The first account made on a fresh server becomes the owner. Logs the new owner in.")]
async fn setup(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    ApiJson(b): ApiJson<SetupBody>,
) -> ApiResult<(StatusCode, CookieHeader, ApiJson<LoginReply>)> {
    guard_transport(&st)?;
    guard_origin(&st, &headers)?;
    let now = st.now();
    if repo::count_users(&st.pool).await? > 0 {
        return Err(ApiError::conflict("setup_done", "The owner account exists already. Log in."));
    }
    let email = repo::clean_email(&b.email);
    check_identity(&b.name, &email)?;
    let workspace = b.workspace_name.trim();
    if workspace.is_empty() || workspace.chars().count() > 80 {
        return Err(ApiError::unprocessable("invalid_workspace_name", "Give the workspace a name (up to 80 characters)."));
    }
    password::check_policy(&b.password, &email, &b.name).map_err(policy)?;

    let keys = [rate::ip_key(&ip)];
    if let Some(secs) = rate::blocked_for(&st.pool, &keys, now).await? {
        return Err(ApiError::too_many("too_many_attempts", format!("Too many tries. Try again in {}.", wait_text(secs)), secs));
    }
    let hash = st.passwords.hash(&b.password).await?;

    let mut tx = st.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(7306002)").execute(&mut *tx).await?;
    let (users,): (i64,) = sqlx::query_as("SELECT count(*) FROM users").fetch_one(&mut *tx).await?;
    if users > 0 {
        return Err(ApiError::conflict("setup_done", "The owner account exists already. Log in."));
    }
    let id = repo::insert_user(&mut *tx, &email, &b.name, &hash, "owner").await?;
    sqlx::query("UPDATE app_state SET workspace_name = $1, setup_done_at = $2, setup_code_hash = NULL WHERE id = 1")
        .bind(workspace)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    super::setup::remove_file(&st.config);

    let (cookie, csrf) = start_session(&st, id, &ip, &headers).await?;
    let user = repo::find(&st.pool, id).await?.ok_or_else(ApiError::internal)?;
    Ok((StatusCode::CREATED, cookie, ApiJson(LoginReply { user: user.out(now), csrf_token: csrf })))
}

// ---------- login, logout ----------

#[derive(Deserialize, ToSchema)]
pub struct LoginBody {
    pub email: String,
    pub password: String,
}

/// Log in with email and password. Sets the session cookie.
#[utoipa::path(post, path = "/v1/auth/login", tag = "auth",
    request_body = LoginBody,
    responses((status = 200, body = LoginReply), (status = 401, description = "invalid_credentials"), (status = 429, description = "too_many_attempts")),
    description = "No key needed. Every failure gives the same answer. After many failures the email or the IP must wait (`Retry-After`).")]
async fn login(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    ApiJson(b): ApiJson<LoginBody>,
) -> ApiResult<(CookieHeader, ApiJson<LoginReply>)> {
    guard_transport(&st)?;
    guard_origin(&st, &headers)?;
    let now = st.now();
    let email = repo::clean_email(&b.email);
    let keys = [rate::ip_key(&ip), rate::email_key(&email)];
    if let Some(secs) = rate::blocked_for(&st.pool, &keys, now).await? {
        return Err(ApiError::too_many("too_many_attempts", format!("Too many tries. Try again in {}.", wait_text(secs)), secs));
    }
    let user = if email.len() <= 254 { repo::find_by_email(&st.pool, &email).await? } else { None };
    // An unknown email still costs one hash, so the time does not tell if the email exists.
    let password_ok = b.password.len() <= 1024 && st.passwords.verify(&b.password, user.as_ref().map(|u| u.password_hash.as_str())).await;
    let user = match user {
        Some(u) if password_ok && u.active() => u,
        _ => {
            rate::record_failure(&st.pool, &ip, &email, now).await?;
            return Err(bad_login());
        }
    };
    rate::record_success(&st.pool, &email).await?;
    if password::Passwords::needs_rehash(&user.password_hash) {
        let fresh = st.passwords.hash(&b.password).await?;
        repo::replace_hash(&st.pool, user.id, &fresh).await?;
    }
    sqlx::query("UPDATE users SET last_login_at = $2 WHERE id = $1").bind(user.id).bind(now).execute(&st.pool).await?;
    let (cookie, csrf) = start_session(&st, user.id, &ip, &headers).await?;
    let mut out = user.out(now);
    out.last_login_at = Some(now);
    Ok((cookie, ApiJson(LoginReply { user: out, csrf_token: csrf })))
}

/// End the session and clear the cookie.
#[utoipa::path(post, path = "/v1/auth/logout", tag = "auth",
    responses((status = 204, description = "Logged out. Also when there was no session.")),
    description = "Needs the `X-CSRF-Token` header when a session exists.")]
async fn logout(State(st): State<AppState>, headers: HeaderMap) -> ApiResult<(StatusCode, CookieHeader)> {
    if let Some(ctx) = session::resolve(&st, &axum::http::Method::POST, &headers).await? {
        repo::delete_session(&st.pool, ctx.session_id).await?;
    }
    Ok((StatusCode::NO_CONTENT, [(header::SET_COOKIE, session::clear_cookie(&st.config))]))
}

// ---------- your own account ----------

/// Your account.
#[utoipa::path(get, path = "/v1/users/me", tag = "users",
    responses((status = 200, body = UserOut)), security(("session" = [])), description = "Needs a session.")]
async fn get_me(State(st): State<AppState>, axum::Extension(ctx): axum::Extension<SessionCtx>) -> ApiResult<ApiJson<UserOut>> {
    Ok(ApiJson(current_user(&st, &ctx).await?.out(st.now())))
}

#[derive(Deserialize, ToSchema, Default)]
pub struct PatchMe {
    pub name: Option<String>,
    pub email: Option<String>,
    /// Needed to change the email.
    pub current_password: Option<String>,
    /// Merged into the saved preferences, key by key. A `null` value removes the key.
    #[schema(value_type = Option<Object>)]
    pub preferences: Option<serde_json::Value>,
}

const MAX_PREFERENCES_BYTES: usize = 16 * 1024;

/// Merge `patch` into `base` key by key. `null` removes a key.
fn merge_preferences(base: &serde_json::Value, patch: &serde_json::Value) -> Option<serde_json::Value> {
    let mut out = base.as_object().cloned().unwrap_or_default();
    for (k, v) in patch.as_object()? {
        if v.is_null() {
            out.remove(k);
        } else {
            out.insert(k.clone(), v.clone());
        }
    }
    let merged = serde_json::Value::Object(out);
    (merged.to_string().len() <= MAX_PREFERENCES_BYTES).then_some(merged)
}

/// Change your name, email or preferences.
#[utoipa::path(patch, path = "/v1/users/me", tag = "users",
    request_body = PatchMe,
    responses((status = 200, body = UserOut), (status = 409, description = "email_taken")),
    security(("session" = [])), description = "Needs a session. Changing the email needs `current_password`.")]
async fn patch_me(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    axum::Extension(ctx): axum::Extension<SessionCtx>,
    ApiJson(b): ApiJson<PatchMe>,
) -> ApiResult<ApiJson<UserOut>> {
    let user = current_user(&st, &ctx).await?;
    let mut name = user.name.clone();
    let mut email = user.email.clone();
    if let Some(n) = &b.name {
        if !valid_name(n) {
            return Err(ApiError::unprocessable("invalid_name", "Give a name (up to 100 characters)."));
        }
        name = n.trim().to_string();
    }
    if let Some(e) = &b.email {
        let e = repo::clean_email(e);
        if e != user.email {
            if !repo::valid_email(&e) {
                return Err(ApiError::unprocessable("invalid_email", "This email address does not look right."));
            }
            verify_current(&st, &user, b.current_password.as_deref().unwrap_or(""), &ip).await?;
            email = e;
        }
    }
    let mut preferences = user.preferences.clone();
    if let Some(p) = &b.preferences {
        preferences = merge_preferences(&user.preferences, p)
            .ok_or_else(|| ApiError::unprocessable("invalid_preferences", "Preferences must be a JSON object under 16 KB."))?;
    }
    let res = sqlx::query("UPDATE users SET name = $2, email = $3, preferences = $4, updated_at = $5 WHERE id = $1")
        .bind(user.id)
        .bind(&name)
        .bind(&email)
        .bind(&preferences)
        .bind(st.now())
        .execute(&st.pool)
        .await;
    match res {
        Ok(_) => {}
        Err(sqlx::Error::Database(d)) if d.is_unique_violation() => {
            return Err(ApiError::conflict("email_taken", "Another user has this email."));
        }
        Err(e) => return Err(e.into()),
    }
    Ok(ApiJson(current_user(&st, &ctx).await?.out(st.now())))
}

#[derive(Deserialize, ToSchema)]
pub struct ChangePassword {
    pub current_password: String,
    pub new_password: String,
}

/// Change your password. All your other sessions end.
#[utoipa::path(post, path = "/v1/users/me/password", tag = "users",
    request_body = ChangePassword,
    responses((status = 204, description = "Changed"), (status = 403, description = "wrong_password"), (status = 422, description = "password rules")),
    security(("session" = [])), description = "Needs a session.")]
async fn change_password(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    axum::Extension(ctx): axum::Extension<SessionCtx>,
    ApiJson(b): ApiJson<ChangePassword>,
) -> ApiResult<StatusCode> {
    let user = current_user(&st, &ctx).await?;
    verify_current(&st, &user, &b.current_password, &ip).await?;
    password::check_policy(&b.new_password, &user.email, &user.name).map_err(policy)?;
    let hash = st.passwords.hash(&b.new_password).await?;
    repo::set_password(&st.pool, user.id, &hash, st.now()).await?;
    repo::delete_other_sessions(&st.pool, user.id, Some(ctx.session_id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Your open sessions.
#[utoipa::path(get, path = "/v1/users/me/sessions", tag = "users",
    responses((status = 200, body = Vec<SessionInfo>)), security(("session" = [])), description = "Needs a session. `current` marks this one.")]
async fn list_my_sessions(State(st): State<AppState>, axum::Extension(ctx): axum::Extension<SessionCtx>) -> ApiResult<ApiJson<Vec<SessionInfo>>> {
    let mut list = repo::list_sessions(&st.pool, ctx.user_id, st.now()).await?;
    for s in &mut list {
        s.current = s.id == ctx.session_id;
    }
    Ok(ApiJson(list))
}

/// End one of your sessions.
#[utoipa::path(delete, path = "/v1/users/me/sessions/{id}", tag = "users",
    params(("id" = Uuid, Path)),
    responses((status = 204, description = "Ended"), (status = 404, description = "session_not_found")),
    security(("session" = [])), description = "Needs a session. Ending the current one logs you out.")]
async fn end_my_session(
    State(st): State<AppState>,
    axum::Extension(ctx): axum::Extension<SessionCtx>,
    ApiPath(id): ApiPath<Uuid>,
) -> ApiResult<StatusCode> {
    let res = sqlx::query("DELETE FROM user_sessions WHERE id = $1 AND user_id = $2").bind(id).bind(ctx.user_id).execute(&st.pool).await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::not_found("session_not_found", "No session with this id."));
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------- team ----------

/// All users.
#[utoipa::path(get, path = "/v1/users", tag = "users",
    responses((status = 200, body = Vec<UserOut>)), security(("session" = [])), description = "Owner and admin.")]
async fn list_users(State(st): State<AppState>, axum::Extension(ctx): axum::Extension<SessionCtx>) -> ApiResult<ApiJson<Vec<UserOut>>> {
    need_admin(&ctx)?;
    let now = st.now();
    Ok(ApiJson(repo::list(&st.pool).await?.iter().map(|u| u.out(now)).collect()))
}

#[derive(Deserialize, ToSchema, Default)]
pub struct PatchUser {
    /// `admin` or `member`. To make someone the owner, use transfer-ownership.
    pub role: Option<Role>,
    /// `active` or `disabled`. Disabling ends all sessions of the user.
    pub status: Option<String>,
    /// `true` clears the lock after wrong passwords.
    pub unlock: Option<bool>,
}

fn target_of(rows: Option<UserRow>) -> ApiResult<UserRow> {
    rows.ok_or_else(|| ApiError::not_found("user_not_found", "No user with this id."))
}

/// Change the role or status of a user, or unlock them.
#[utoipa::path(patch, path = "/v1/users/{id}", tag = "users",
    params(("id" = Uuid, Path)), request_body = PatchUser,
    responses((status = 200, body = UserOut), (status = 403, description = "owner_protected, cannot_change_self")),
    security(("session" = [])), description = "Owner and admin. Nobody changes the owner here.")]
async fn patch_user(
    State(st): State<AppState>,
    axum::Extension(ctx): axum::Extension<SessionCtx>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(b): ApiJson<PatchUser>,
) -> ApiResult<ApiJson<UserOut>> {
    need_admin(&ctx)?;
    let target = target_of(repo::find(&st.pool, id).await?)?;
    let changes_access = b.role.is_some() || b.status.is_some();
    if changes_access {
        if target.role() == Role::Owner {
            return Err(ApiError::forbidden("owner_protected", "The owner cannot be changed. Transfer ownership first."));
        }
        if target.id == ctx.user_id {
            return Err(ApiError::forbidden("cannot_change_self", "You cannot change your own role or status."));
        }
    }
    if let Some(role) = b.role {
        if role == Role::Owner {
            return Err(ApiError::unprocessable("use_transfer_ownership", "To make someone the owner, use transfer-ownership."));
        }
        sqlx::query("UPDATE users SET role = $2, updated_at = now() WHERE id = $1").bind(id).bind(role.as_str()).execute(&st.pool).await?;
    }
    if let Some(status) = &b.status {
        if status != "active" && status != "disabled" {
            return Err(ApiError::unprocessable("invalid_status", "Status must be 'active' or 'disabled'."));
        }
        sqlx::query("UPDATE users SET status = $2, updated_at = now() WHERE id = $1").bind(id).bind(status).execute(&st.pool).await?;
        if status == "disabled" {
            repo::delete_other_sessions(&st.pool, id, None).await?;
        }
    }
    if b.unlock == Some(true) {
        rate::clear_email(&st.pool, &target.email).await?;
    }
    Ok(ApiJson(target_of(repo::find(&st.pool, id).await?)?.out(st.now())))
}

/// Remove a user and end their sessions.
#[utoipa::path(delete, path = "/v1/users/{id}", tag = "users",
    params(("id" = Uuid, Path)),
    responses((status = 204, description = "Removed"), (status = 403, description = "owner_protected, cannot_change_self")),
    security(("session" = [])), description = "Owner and admin.")]
async fn delete_user(State(st): State<AppState>, axum::Extension(ctx): axum::Extension<SessionCtx>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<StatusCode> {
    need_admin(&ctx)?;
    let target = target_of(repo::find(&st.pool, id).await?)?;
    if target.role() == Role::Owner {
        return Err(ApiError::forbidden("owner_protected", "The owner cannot be removed. Transfer ownership first."));
    }
    if target.id == ctx.user_id {
        return Err(ApiError::forbidden("cannot_change_self", "You cannot remove yourself."));
    }
    sqlx::query("DELETE FROM users WHERE id = $1").bind(id).execute(&st.pool).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct TransferBody {
    /// The owner's own password, again.
    pub password: String,
}

/// Make an admin the owner. You become an admin.
#[utoipa::path(post, path = "/v1/users/{id}/transfer-ownership", tag = "users",
    params(("id" = Uuid, Path)), request_body = TransferBody,
    responses((status = 204, description = "Done"), (status = 403, description = "wrong_password, owner_only"), (status = 422, description = "target_not_admin")),
    security(("session" = [])), description = "Owner only. The new owner must be an active admin.")]
async fn transfer_ownership(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    axum::Extension(ctx): axum::Extension<SessionCtx>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(b): ApiJson<TransferBody>,
) -> ApiResult<StatusCode> {
    if ctx.role != Role::Owner {
        return Err(ApiError::forbidden("owner_only", "Only the owner can do this."));
    }
    let me = current_user(&st, &ctx).await?;
    verify_current(&st, &me, &b.password, &ip).await?;
    let target = target_of(repo::find(&st.pool, id).await?)?;
    if target.role() != Role::Admin || !target.active() {
        return Err(ApiError::unprocessable("target_not_admin", "Make the person an admin first. Then transfer ownership."));
    }
    let mut tx = st.pool.begin().await?;
    sqlx::query("UPDATE users SET role = 'admin', updated_at = now() WHERE id = $1").bind(me.id).execute(&mut *tx).await?;
    sqlx::query("UPDATE users SET role = 'owner', updated_at = now() WHERE id = $1").bind(target.id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---------- invites ----------

#[derive(Deserialize, ToSchema)]
pub struct NewInvite {
    pub email: String,
    /// `admin` or `member`.
    pub role: Role,
}

#[derive(Serialize, FromRow, ToSchema)]
pub struct InviteOut {
    pub id: Uuid,
    pub email: String,
    pub role: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
pub struct NewInviteReply {
    pub invite: InviteOut,
    /// The link to give to the person. Shown this one time only. It is not stored.
    pub link: String,
}

/// Make an invite link (valid 7 days).
#[utoipa::path(post, path = "/v1/users/invites", tag = "users",
    request_body = NewInvite,
    responses((status = 201, body = NewInviteReply), (status = 409, description = "email_taken")),
    security(("session" = [])), description = "Owner and admin. The server does not send email: copy the link.")]
async fn create_invite(
    State(st): State<AppState>,
    axum::Extension(ctx): axum::Extension<SessionCtx>,
    ApiJson(b): ApiJson<NewInvite>,
) -> ApiResult<(StatusCode, ApiJson<NewInviteReply>)> {
    need_admin(&ctx)?;
    let email = repo::clean_email(&b.email);
    if !repo::valid_email(&email) {
        return Err(ApiError::unprocessable("invalid_email", "This email address does not look right."));
    }
    if b.role == Role::Owner {
        return Err(ApiError::unprocessable("invalid_role", "Invite as 'admin' or 'member'."));
    }
    if repo::find_by_email(&st.pool, &email).await?.is_some() {
        return Err(ApiError::conflict("email_taken", "A user with this email exists already."));
    }
    let token = repo::random_token();
    let now = st.now();
    let invite: InviteOut = sqlx::query_as(
        "INSERT INTO user_invites (id, email, role, token_hash, invited_by, created_at, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id, email::text AS email, role, created_at, expires_at",
    )
    .bind(Uuid::now_v7())
    .bind(&email)
    .bind(b.role.as_str())
    .bind(repo::sha256(&token))
    .bind(ctx.user_id)
    .bind(now)
    .bind(now + Duration::days(repo::INVITE_DAYS))
    .fetch_one(&st.pool)
    .await?;
    let link = format!("{}/invite/{token}", st.site_url().unwrap_or_default());
    Ok((StatusCode::CREATED, ApiJson(NewInviteReply { invite, link })))
}

/// Invites that are not used and not expired.
#[utoipa::path(get, path = "/v1/users/invites", tag = "users",
    responses((status = 200, body = Vec<InviteOut>)), security(("session" = [])), description = "Owner and admin.")]
async fn list_invites(State(st): State<AppState>, axum::Extension(ctx): axum::Extension<SessionCtx>) -> ApiResult<ApiJson<Vec<InviteOut>>> {
    need_admin(&ctx)?;
    Ok(ApiJson(
        sqlx::query_as(
            "SELECT id, email::text AS email, role, created_at, expires_at FROM user_invites
             WHERE used_at IS NULL AND expires_at > $1 ORDER BY created_at DESC",
        )
        .bind(st.now())
        .fetch_all(&st.pool)
        .await?,
    ))
}

/// Cancel an invite.
#[utoipa::path(delete, path = "/v1/users/invites/{id}", tag = "users",
    params(("id" = Uuid, Path)),
    responses((status = 204, description = "Cancelled"), (status = 404, description = "invite_not_found")),
    security(("session" = [])), description = "Owner and admin.")]
async fn delete_invite(State(st): State<AppState>, axum::Extension(ctx): axum::Extension<SessionCtx>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<StatusCode> {
    need_admin(&ctx)?;
    let res = sqlx::query("DELETE FROM user_invites WHERE id = $1 AND used_at IS NULL").bind(id).execute(&st.pool).await?;
    if res.rows_affected() == 0 {
        return Err(ApiError::not_found("invite_not_found", "No open invite with this id."));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize, ToSchema)]
pub struct InviteInfo {
    pub email: String,
    pub role: String,
    pub workspace_name: String,
    pub expires_at: DateTime<Utc>,
}

fn invite_gone() -> ApiError {
    ApiError::not_found("invite_invalid", "This invite link is not valid. It may be used or expired. Ask for a new one.")
}

/// What an invite link is for. The page shows it before the person sets a password.
#[utoipa::path(get, path = "/v1/auth/invites/{token}", tag = "auth",
    params(("token" = String, Path)),
    responses((status = 200, body = InviteInfo), (status = 404, description = "invite_invalid")),
    description = "No key needed.")]
async fn invite_info(State(st): State<AppState>, ApiPath(token): ApiPath<String>) -> ApiResult<ApiJson<InviteInfo>> {
    let row: Option<(String, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT email::text, role, expires_at FROM user_invites WHERE token_hash = $1 AND used_at IS NULL AND expires_at > $2",
    )
    .bind(repo::sha256(&token))
    .bind(st.now())
    .fetch_optional(&st.pool)
    .await?;
    let (email, role, expires_at) = row.ok_or_else(invite_gone)?;
    let app = repo::app_state(&st.pool).await?;
    Ok(ApiJson(InviteInfo { email, role, workspace_name: app.workspace_name, expires_at }))
}

#[derive(Deserialize, ToSchema)]
pub struct AcceptInvite {
    pub name: String,
    pub password: String,
}

/// Accept an invite: set a name and a password. Logs the new user in.
#[utoipa::path(post, path = "/v1/auth/invites/{token}/accept", tag = "auth",
    params(("token" = String, Path)), request_body = AcceptInvite,
    responses((status = 201, body = LoginReply), (status = 404, description = "invite_invalid"), (status = 409, description = "email_taken")),
    description = "No key needed.")]
async fn invite_accept(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    ApiPath(token): ApiPath<String>,
    ApiJson(b): ApiJson<AcceptInvite>,
) -> ApiResult<(StatusCode, CookieHeader, ApiJson<LoginReply>)> {
    guard_transport(&st)?;
    guard_origin(&st, &headers)?;
    let now = st.now();
    let hash_of_token = repo::sha256(&token);
    let row: Option<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, email::text, role FROM user_invites WHERE token_hash = $1 AND used_at IS NULL AND expires_at > $2",
    )
    .bind(&hash_of_token)
    .bind(now)
    .fetch_optional(&st.pool)
    .await?;
    let (invite_id, email, role) = row.ok_or_else(invite_gone)?;
    if !valid_name(&b.name) {
        return Err(ApiError::unprocessable("invalid_name", "Give a name (up to 100 characters)."));
    }
    password::check_policy(&b.password, &email, &b.name).map_err(policy)?;
    let hash = st.passwords.hash(&b.password).await?;

    let mut tx = st.pool.begin().await?;
    // Mark it used first: two people with the same link cannot both get in.
    let used = sqlx::query("UPDATE user_invites SET used_at = $2 WHERE id = $1 AND used_at IS NULL").bind(invite_id).bind(now).execute(&mut *tx).await?;
    if used.rows_affected() == 0 {
        return Err(invite_gone());
    }
    let id = match repo::insert_user(&mut *tx, &email, &b.name, &hash, &role).await {
        Ok(id) => id,
        Err(sqlx::Error::Database(d)) if d.is_unique_violation() => {
            return Err(ApiError::conflict("email_taken", "A user with this email exists already."));
        }
        Err(e) => return Err(e.into()),
    };
    tx.commit().await?;
    let (cookie, csrf) = start_session(&st, id, &ip, &headers).await?;
    let user = repo::find(&st.pool, id).await?.ok_or_else(ApiError::internal)?;
    Ok((StatusCode::CREATED, cookie, ApiJson(LoginReply { user: user.out(now), csrf_token: csrf })))
}

// ---------- password reset links ----------

/// Make a one-time reset link for a user (valid 1 hour). Used by the `reset-password` command.
pub async fn new_reset_link(pool: &PgPool, public_url: Option<&str>, user_id: Uuid, now: DateTime<Utc>) -> Result<String, sqlx::Error> {
    let token = repo::random_token();
    sqlx::query("INSERT INTO user_resets (id, user_id, token_hash, created_at, expires_at) VALUES ($1, $2, $3, $4, $5)")
        .bind(Uuid::now_v7())
        .bind(user_id)
        .bind(repo::sha256(&token))
        .bind(now)
        .bind(now + Duration::hours(repo::RESET_HOURS))
        .execute(pool)
        .await?;
    Ok(format!("{}/reset/{token}", public_url.unwrap_or("")))
}

#[derive(Serialize, ToSchema)]
pub struct ResetLinkReply {
    /// Shown this one time only. Valid 1 hour.
    pub link: String,
}

/// Make a one-time password reset link for a user.
#[utoipa::path(post, path = "/v1/users/{id}/reset-link", tag = "users",
    params(("id" = Uuid, Path)),
    responses((status = 201, body = ResetLinkReply), (status = 403, description = "owner_protected")),
    security(("session" = [])), description = "Owner and admin. Only the owner can make a link for the owner.")]
async fn make_reset_link(
    State(st): State<AppState>,
    axum::Extension(ctx): axum::Extension<SessionCtx>,
    ApiPath(id): ApiPath<Uuid>,
) -> ApiResult<(StatusCode, ApiJson<ResetLinkReply>)> {
    need_admin(&ctx)?;
    let target = target_of(repo::find(&st.pool, id).await?)?;
    if target.role() == Role::Owner && ctx.role != Role::Owner {
        return Err(ApiError::forbidden("owner_protected", "Only the owner can reset the owner's password."));
    }
    let link = new_reset_link(&st.pool, st.site_url().as_deref(), id, st.now()).await?;
    Ok((StatusCode::CREATED, ApiJson(ResetLinkReply { link })))
}

#[derive(Serialize, ToSchema)]
pub struct ResetInfo {
    pub email: String,
}

fn reset_gone() -> ApiError {
    ApiError::not_found("reset_invalid", "This reset link is not valid. It may be used or expired. Ask for a new one.")
}

/// Whose password a reset link is for.
#[utoipa::path(get, path = "/v1/auth/reset/{token}", tag = "auth",
    params(("token" = String, Path)),
    responses((status = 200, body = ResetInfo), (status = 404, description = "reset_invalid")),
    description = "No key needed.")]
async fn reset_info(State(st): State<AppState>, ApiPath(token): ApiPath<String>) -> ApiResult<ApiJson<ResetInfo>> {
    let email: Option<String> = sqlx::query_scalar(
        "SELECT u.email::text FROM user_resets r JOIN users u ON u.id = r.user_id
         WHERE r.token_hash = $1 AND r.used_at IS NULL AND r.expires_at > $2",
    )
    .bind(repo::sha256(&token))
    .bind(st.now())
    .fetch_optional(&st.pool)
    .await?;
    Ok(ApiJson(ResetInfo { email: email.ok_or_else(reset_gone)? }))
}

#[derive(Deserialize, ToSchema)]
pub struct ResetBody {
    pub new_password: String,
}

/// Set a new password with a reset link. All sessions of the user end. Logs the user in.
#[utoipa::path(post, path = "/v1/auth/reset/{token}", tag = "auth",
    params(("token" = String, Path)), request_body = ResetBody,
    responses((status = 200, body = LoginReply), (status = 404, description = "reset_invalid"), (status = 422, description = "password rules")),
    description = "No key needed.")]
async fn reset_accept(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    ApiPath(token): ApiPath<String>,
    ApiJson(b): ApiJson<ResetBody>,
) -> ApiResult<(CookieHeader, ApiJson<LoginReply>)> {
    guard_transport(&st)?;
    guard_origin(&st, &headers)?;
    let now = st.now();
    let token_hash = repo::sha256(&token);
    let who: Option<Uuid> = sqlx::query_scalar("SELECT user_id FROM user_resets WHERE token_hash = $1 AND used_at IS NULL AND expires_at > $2")
        .bind(&token_hash)
        .bind(now)
        .fetch_optional(&st.pool)
        .await?;
    let user = repo::find(&st.pool, who.ok_or_else(reset_gone)?).await?.ok_or_else(reset_gone)?;
    password::check_policy(&b.new_password, &user.email, &user.name).map_err(policy)?;
    let hash = st.passwords.hash(&b.new_password).await?;
    let used = sqlx::query("UPDATE user_resets SET used_at = $2 WHERE token_hash = $1 AND used_at IS NULL")
        .bind(&token_hash)
        .bind(now)
        .execute(&st.pool)
        .await?;
    if used.rows_affected() == 0 {
        return Err(reset_gone());
    }
    repo::set_password(&st.pool, user.id, &hash, now).await?;
    repo::delete_other_sessions(&st.pool, user.id, None).await?;
    rate::clear_email(&st.pool, &user.email).await?;
    sqlx::query("UPDATE users SET last_login_at = $2 WHERE id = $1").bind(user.id).bind(now).execute(&st.pool).await?;
    let (cookie, csrf) = start_session(&st, user.id, &ip, &headers).await?;
    let fresh = repo::find(&st.pool, user.id).await?.ok_or_else(ApiError::internal)?;
    Ok((cookie, ApiJson(LoginReply { user: fresh.out(now), csrf_token: csrf })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn preferences_merge_key_by_key() {
        let base = json!({"theme": "dark", "a": 1});
        let merged = merge_preferences(&base, &json!({"theme": "light", "a": null, "b": true})).unwrap();
        assert_eq!(merged, json!({"theme": "light", "b": true}));
        assert!(merge_preferences(&base, &json!([1])).is_none());
        let big = json!({"x": "y".repeat(MAX_PREFERENCES_BYTES)});
        assert!(merge_preferences(&base, &big).is_none());
    }

    #[test]
    fn wait_text_is_plain() {
        assert_eq!(wait_text(1), "1 second");
        assert_eq!(wait_text(16), "16 seconds");
        assert_eq!(wait_text(900), "15 minutes");
    }
}
