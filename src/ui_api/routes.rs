use crate::auth::api_keys::Scope;
use crate::auth::middleware::{KeyScope, require_any};
use crate::error::{ApiError, ApiJson, ApiResult};
use crate::state::AppState;
use crate::users::repo;
use crate::users::session::SessionCtx;
use axum::Extension;
use axum::extract::State;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

pub fn router(state: AppState) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_config))
        .routes(routes!(get_onboarding, patch_onboarding))
        .routes(routes!(get_alerts))
        .routes(routes!(get_workspace, patch_workspace))
        .route_layer(axum::middleware::from_fn_with_state(state, require_any))
}

/// Owner, admin, or a key with scope `admin`.
fn need_admin(session: &Option<Extension<SessionCtx>>, key: &Option<Extension<KeyScope>>) -> ApiResult<()> {
    let ok = match (session, key) {
        (Some(Extension(s)), _) => s.role.is_admin(),
        (None, Some(Extension(k))) => k.0 == Scope::Admin,
        _ => false,
    };
    if ok { Ok(()) } else { Err(ApiError::forbidden("admin_only", "Only an owner or an admin can do this.")) }
}

// ---------- config ----------

#[derive(Serialize, ToSchema)]
pub struct OauthReady {
    /// The Google client id and secret are set on the server.
    pub gmail: bool,
    /// The Microsoft client id and secret are set on the server.
    pub microsoft: bool,
}

#[derive(Serialize, ToSchema)]
pub struct RedirectUris {
    /// The address to enter in the Google Cloud console. Empty when `AIKIDO_PUBLIC_URL` is not set.
    pub gmail: Option<String>,
    pub microsoft: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct Features {
    /// The reply check runs on this server copy.
    pub replies: bool,
    pub followups: bool,
    pub leads: bool,
    pub generator: bool,
    /// Automations (the node editor) are on (`AIKIDO_AUTOMATIONS_ENABLED`, default on).
    pub automations: bool,
}

#[derive(Serialize, ToSchema)]
pub struct UiConfig {
    pub version: &'static str,
    /// `AIKIDO_PUBLIC_URL` is set. The Google and Microsoft login needs it.
    pub public_url_set: bool,
    pub oauth: OauthReady,
    pub redirect_uris: RedirectUris,
    /// Emails are logged, not sent.
    pub dry_run: bool,
    pub features: Features,
}

/// Set in the env, or saved in Settings → Environment.
fn env_set(name: &str) -> bool {
    crate::app_settings::get(name).is_some()
}

/// What the app needs to know about the server. No secrets.
#[utoipa::path(get, path = "/v1/ui/config", tag = "ui",
    responses((status = 200, body = UiConfig)),
    security(("api_key" = []), ("session" = [])), description = "Any key, or a session.")]
async fn get_config(State(st): State<AppState>) -> ApiJson<UiConfig> {
    let cfg = &st.config;
    let uri = |provider: &str| cfg.public_url.as_ref().map(|u| format!("{u}/v1/accounts/oauth/{provider}/callback"));
    ApiJson(UiConfig {
        version: env!("CARGO_PKG_VERSION"),
        public_url_set: cfg.public_url.is_some(),
        oauth: OauthReady {
            gmail: env_set("GOOGLE_CLIENT_ID") && env_set("GOOGLE_CLIENT_SECRET"),
            microsoft: env_set("MS_CLIENT_ID") && env_set("MS_CLIENT_SECRET"),
        },
        redirect_uris: RedirectUris { gmail: uri("gmail"), microsoft: uri("microsoft") },
        dry_run: cfg.dry_run,
        features: Features {
            replies: cfg.reply_check_enabled,
            followups: cfg.followups_enabled,
            leads: cfg.leads_enabled,
            generator: cfg.generator_enabled,
            automations: crate::automations::engine::worker::enabled_by_env(),
        },
    })
}

// ---------- onboarding ----------

pub const ONBOARDING_STEPS: &[&str] = &["design", "done"];

/// Step names of older versions (a 5-step wizard). Saved state and old clients may still send them.
const OLD_STEPS: &[&str] = &["connect-email", "sending-basics", "ai", "import"];

/// `design` or `done`. An old step name becomes `design`. `None` for a name that was never a step.
fn normalize_step(s: &str) -> Option<&'static str> {
    if let Some(step) = ONBOARDING_STEPS.iter().find(|x| **x == s) {
        return Some(step);
    }
    OLD_STEPS.contains(&s).then_some("design")
}

#[derive(Serialize, ToSchema)]
pub struct OnboardingState {
    /// `design` or `done`.
    pub step: String,
    pub done: bool,
    /// Steps the user chose to skip.
    pub skipped: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub struct OnboardingFacts {
    pub accounts: i64,
    pub readable_accounts: i64,
    /// AI providers.
    pub providers: i64,
    pub leads: i64,
    /// The default sending settings are not the built-in ones any more.
    pub sending_defaults_changed: bool,
}

#[derive(Serialize, ToSchema)]
pub struct OnboardingReply {
    pub state: OnboardingState,
    /// What exists already, so the wizard can mark steps as done.
    pub facts: OnboardingFacts,
}

fn parse_onboarding(v: &Value) -> OnboardingState {
    OnboardingState {
        step: v["step"].as_str().and_then(normalize_step).unwrap_or("design").to_string(),
        done: v["done"].as_bool().unwrap_or(false),
        skipped: v["skipped"].as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()).unwrap_or_default(),
    }
}

async fn onboarding_reply(st: &AppState) -> ApiResult<OnboardingReply> {
    let app = repo::app_state(&st.pool).await?;
    let (accounts, readable_accounts): (i64, i64) =
        sqlx::query_as("SELECT count(*), count(*) FILTER (WHERE can_read) FROM email_accounts WHERE deleted_at IS NULL").fetch_one(&st.pool).await?;
    let providers: i64 = sqlx::query_scalar("SELECT count(*) FROM llm_providers").fetch_one(&st.pool).await?;
    let leads: i64 = sqlx::query_scalar("SELECT count(*) FROM leads").fetch_one(&st.pool).await?;
    let changed: bool = sqlx::query_scalar("SELECT settings <> '{}'::jsonb FROM sending_defaults WHERE id = 1").fetch_one(&st.pool).await?;
    Ok(OnboardingReply {
        state: parse_onboarding(&app.onboarding),
        facts: OnboardingFacts { accounts, readable_accounts, providers, leads, sending_defaults_changed: changed },
    })
}

/// Where the setup wizard stands, and what exists already.
#[utoipa::path(get, path = "/v1/ui/onboarding", tag = "ui",
    responses((status = 200, body = OnboardingReply)),
    security(("api_key" = []), ("session" = [])), description = "Any key, or a session.")]
async fn get_onboarding(State(st): State<AppState>) -> ApiResult<ApiJson<OnboardingReply>> {
    Ok(ApiJson(onboarding_reply(&st).await?))
}

#[derive(Deserialize, ToSchema, Default)]
pub struct PatchOnboarding {
    pub step: Option<String>,
    pub done: Option<bool>,
    pub skipped: Option<Vec<String>>,
}

/// Save the step of the setup wizard.
#[utoipa::path(patch, path = "/v1/ui/onboarding", tag = "ui",
    request_body = PatchOnboarding,
    responses((status = 200, body = OnboardingReply), (status = 422, description = "invalid_step")),
    security(("api_key" = ["admin"]), ("session" = [])), description = "Owner and admin.")]
async fn patch_onboarding(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiJson(b): ApiJson<PatchOnboarding>,
) -> ApiResult<ApiJson<OnboardingReply>> {
    need_admin(&session, &key)?;
    let bad = |s: &str| normalize_step(s).is_none();
    if b.step.as_deref().is_some_and(bad) || b.skipped.as_ref().is_some_and(|l| l.iter().any(|s| bad(s))) {
        return Err(ApiError::unprocessable("invalid_step", format!("Steps are: {}.", ONBOARDING_STEPS.join(", "))));
    }
    let current = repo::app_state(&st.pool).await?;
    let mut now = parse_onboarding(&current.onboarding);
    if let Some(s) = b.step.as_deref().and_then(normalize_step) {
        now.step = s.to_string();
    }
    if let Some(d) = b.done {
        now.done = d;
    }
    if let Some(mut l) = b.skipped {
        l.sort();
        l.dedup();
        now.skipped = l;
    }
    sqlx::query("UPDATE app_state SET onboarding = $1 WHERE id = 1")
        .bind(json!({ "step": now.step, "done": now.done, "skipped": now.skipped }))
        .execute(&st.pool)
        .await?;
    Ok(ApiJson(onboarding_reply(&st).await?))
}

// ---------- workspace ----------

#[derive(Serialize, ToSchema)]
pub struct Workspace {
    pub name: String,
    /// A small image as a data URL (up to 200 KB), or empty.
    pub logo: Option<String>,
}

/// The workspace name and logo.
#[utoipa::path(get, path = "/v1/ui/workspace", tag = "ui",
    responses((status = 200, body = Workspace)),
    security(("api_key" = []), ("session" = [])), description = "Any key, or a session.")]
async fn get_workspace(State(st): State<AppState>) -> ApiResult<ApiJson<Workspace>> {
    let app = repo::app_state(&st.pool).await?;
    Ok(ApiJson(Workspace { name: app.workspace_name, logo: app.workspace_logo }))
}

/// `"logo": null` removes the logo. A missing `logo` keeps it.
fn double_option<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Ok(Some(Option::<String>::deserialize(d)?))
}

#[derive(Deserialize, ToSchema, Default)]
pub struct PatchWorkspace {
    pub name: Option<String>,
    /// A data URL of a PNG, JPEG, WebP, GIF or SVG image, up to 200 KB. `null` removes the logo.
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<String>)]
    pub logo: Option<Option<String>>,
}

pub const MAX_LOGO_BYTES: usize = 200 * 1024;

fn valid_logo(url: &str) -> bool {
    const KINDS: &[&str] = &["png", "jpeg", "webp", "gif", "svg+xml"];
    let Some(rest) = url.strip_prefix("data:image/") else { return false };
    let Some((kind, data)) = rest.split_once(";base64,") else { return false };
    KINDS.contains(&kind) && !data.is_empty() && data.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
}

/// Change the workspace name or logo.
#[utoipa::path(patch, path = "/v1/ui/workspace", tag = "ui",
    request_body = PatchWorkspace,
    responses((status = 200, body = Workspace), (status = 422, description = "invalid_workspace_name, invalid_logo")),
    security(("api_key" = ["admin"]), ("session" = [])), description = "Owner and admin.")]
async fn patch_workspace(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiJson(b): ApiJson<PatchWorkspace>,
) -> ApiResult<ApiJson<Workspace>> {
    need_admin(&session, &key)?;
    let current = repo::app_state(&st.pool).await?;
    let mut name = current.workspace_name;
    let mut logo = current.workspace_logo;
    if let Some(n) = b.name {
        let n = n.trim();
        if n.is_empty() || n.chars().count() > 80 {
            return Err(ApiError::unprocessable("invalid_workspace_name", "Give the workspace a name (up to 80 characters)."));
        }
        name = n.to_string();
    }
    if let Some(l) = b.logo {
        match l {
            None => logo = None,
            Some(url) => {
                if url.len() > MAX_LOGO_BYTES || !valid_logo(&url) {
                    return Err(ApiError::unprocessable(
                        "invalid_logo",
                        "The logo must be a PNG, JPEG, WebP, GIF or SVG image as a data URL, up to 200 KB.",
                    ));
                }
                logo = Some(url);
            }
        }
    }
    sqlx::query("UPDATE app_state SET workspace_name = $1, workspace_logo = $2 WHERE id = 1").bind(&name).bind(&logo).execute(&st.pool).await?;
    Ok(ApiJson(Workspace { name, logo }))
}

// ---------- alerts ----------

/// The alerts for the bell. Polled every 60 seconds.
#[utoipa::path(get, path = "/v1/ui/alerts", tag = "ui",
    responses((status = 200, body = Vec<super::alerts::Alert>)),
    security(("api_key" = []), ("session" = [])),
    description = "Any key, or a session. With a session, the kinds the user turned off in Settings > Notifications are left out.")]
async fn get_alerts(State(st): State<AppState>, session: Option<Extension<SessionCtx>>) -> ApiResult<ApiJson<Vec<super::alerts::Alert>>> {
    let mut list = super::alerts::collect(&st).await?;
    if let Some(Extension(ctx)) = session {
        if let Some(user) = repo::find(&st.pool, ctx.user_id).await? {
            list.retain(|a| user.preferences["notifications"][a.kind.as_str()].as_bool() != Some(false));
        }
    }
    Ok(ApiJson(list))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logo_must_be_a_base64_image_data_url() {
        assert!(valid_logo("data:image/png;base64,iVBORw0KGgo="));
        assert!(valid_logo("data:image/svg+xml;base64,PHN2Zz48L3N2Zz4="));
        assert!(!valid_logo("data:text/html;base64,PHNjcmlwdD4="));
        assert!(!valid_logo("https://example.com/logo.png"));
        assert!(!valid_logo("data:image/png;base64,\"onerror=alert(1)"));
        assert!(!valid_logo("data:image/png,raw"));
        assert!(!valid_logo("data:image/png;base64,"));
    }

    #[test]
    fn onboarding_defaults() {
        let s = parse_onboarding(&json!({}));
        assert_eq!((s.step.as_str(), s.done, s.skipped.len()), ("design", false, 0));
        let s = parse_onboarding(&json!({"step": "nope", "done": true, "skipped": ["ai"]}));
        assert_eq!((s.step.as_str(), s.done, s.skipped), ("design", true, vec!["ai".to_string()]));
    }

    #[test]
    fn old_step_names_become_design() {
        for old in ["connect-email", "sending-basics", "ai", "import"] {
            assert_eq!(parse_onboarding(&json!({ "step": old })).step, "design");
            assert_eq!(normalize_step(old), Some("design"));
        }
        assert_eq!(normalize_step("done"), Some("done"));
        assert_eq!(normalize_step("design"), Some("design"));
        assert_eq!(normalize_step("nope"), None);
    }
}
