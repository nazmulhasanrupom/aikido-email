//! `/v1/settings/environment` (scope `admin`): see and change the settings that the app can save.
use super::{CATALOG, Source, store};
use crate::auth::api_keys::Scope;
use crate::auth::middleware::require_scope;
use crate::error::{ApiError, ApiJson, ApiPath, ApiResult};
use crate::state::AppState;
use crate::users::session::SessionCtx;
use axum::Extension;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

pub fn router(state: AppState) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_environment, put_environment))
        .routes(routes!(test_group))
        .route_layer(axum::middleware::from_fn_with_state((state, Scope::Admin), require_scope))
}

#[derive(Serialize, ToSchema)]
pub struct EnvSettingItem {
    pub key: String,
    /// `google` or `microsoft`.
    pub group: String,
    pub label: String,
    pub help: String,
    pub secret: bool,
    /// `env` (set on the server, locked), `app` (saved in the app) or `unset`.
    pub source: String,
    /// The value. Never sent for a secret.
    pub value: Option<String>,
    /// A value exists, from the env or from the app.
    pub is_set: bool,
    /// Secrets only: the last 4 characters.
    pub last4: Option<String>,
    pub default: Option<String>,
    /// Always `false`: a saved value works at once.
    pub restart_needed: bool,
}

#[derive(Serialize, ToSchema)]
pub struct EnvReadOnlyItem {
    pub key: String,
    /// `None` for a secret, and for a value that is not set.
    pub value: Option<String>,
    pub secret: bool,
}

#[derive(Serialize, ToSchema)]
pub struct EnvSettingsReply {
    pub items: Vec<EnvSettingItem>,
    /// Settings that only the `.env` file on the server can change (the server needs them before it can read the database).
    pub read_only: Vec<EnvReadOnlyItem>,
}

fn item_out(i: &super::Item) -> EnvSettingItem {
    let value = super::get(i.key);
    let source = super::source(i.key);
    EnvSettingItem {
        key: i.key.into(),
        group: i.group.into(),
        label: i.label.into(),
        help: i.help.into(),
        secret: i.secret,
        source: source.as_str().into(),
        last4: if i.secret { value.as_deref().and_then(super::last4) } else { None },
        is_set: value.is_some(),
        value: if i.secret { None } else { value },
        default: i.default.map(str::to_string),
        restart_needed: false,
    }
}

fn read_only(st: &AppState) -> Vec<EnvReadOnlyItem> {
    let c = &st.config;
    let on = |b: bool| Some(b.to_string());
    let rows: Vec<(&str, Option<String>, bool)> = vec![
        ("DATABASE_URL", None, true),
        ("AIKIDO_ENCRYPTION_KEY", None, true),
        ("AIKIDO_DATA_DIR", Some(c.data_dir.display().to_string()), false),
        ("AIKIDO_BIND_ADDR", Some(c.bind_addr.clone()), false),
        ("AIKIDO_PUBLIC_URL", c.public_url.clone(), false),
        ("AIKIDO_INSECURE_COOKIES", on(c.insecure_cookies), false),
        ("AIKIDO_TRUST_PROXY", on(c.trust_proxy), false),
        ("AIKIDO_DRY_RUN", on(c.dry_run), false),
        ("AIKIDO_TRACKING_CNAME_TARGET", c.tracking.cname_target.clone(), false),
        ("AIKIDO_ACME_EMAIL", c.tracking.acme_email.clone(), false),
        ("AIKIDO_TRACKING_HTTPS_ADDR", c.tracking.https_addr.clone(), false),
        ("AIKIDO_TRACKING_HTTP_ADDR", c.tracking.http_addr.clone(), false),
        ("AIKIDO_TRACKING_ENABLED", on(c.tracking.enabled), false),
        ("AIKIDO_REPLY_CHECK_ENABLED", on(c.reply_check_enabled), false),
        ("AIKIDO_FOLLOWUPS_ENABLED", on(c.followups_enabled), false),
        ("AIKIDO_LEADS_ENABLED", on(c.leads_enabled), false),
        ("AIKIDO_GENERATOR_ENABLED", on(c.generator_enabled), false),
        ("AIKIDO_SCHEDULER_TICK_SECS", Some(c.scheduler_tick_secs.to_string()), false),
        ("AIKIDO_MAX_PARALLEL_ACCOUNTS", Some(c.max_parallel_accounts.to_string()), false),
        ("AIKIDO_REPLY_TICK_SECS", Some(c.reply_tick_secs.to_string()), false),
        ("AIKIDO_MAX_PARALLEL_SYNCS", Some(c.max_parallel_syncs.to_string()), false),
    ];
    rows.into_iter().map(|(key, value, secret)| EnvReadOnlyItem { key: key.into(), value, secret }).collect()
}

fn reply(st: &AppState) -> EnvSettingsReply {
    EnvSettingsReply { items: CATALOG.iter().map(item_out).collect(), read_only: read_only(st) }
}

/// The settings the app can save, and the ones that only `.env` can change.
#[utoipa::path(get, path = "/v1/settings/environment", tag = "settings",
    responses((status = 200, body = EnvSettingsReply)),
    security(("api_key" = ["admin"]), ("session" = [])),
    description = "Scope: `admin` (owner and admin sessions). A secret is never sent: only `is_set` and the last 4 characters. \
        A value set in the env wins over a saved one (`source` is `env`) and cannot be changed here.")]
async fn get_environment(State(st): State<AppState>) -> ApiJson<EnvSettingsReply> {
    ApiJson(reply(&st))
}

#[derive(Deserialize, ToSchema)]
pub struct EnvSettingsPut {
    /// `KEY: "value"` saves. `KEY: null` removes the saved value.
    pub values: BTreeMap<String, Option<String>>,
}

/// Check the whole change. Returns what to save.
fn check_changes(values: &BTreeMap<String, Option<String>>) -> ApiResult<Vec<(String, Option<String>)>> {
    let mut out = Vec::new();
    for (key, value) in values {
        if super::item(key).is_none() {
            return Err(ApiError::unprocessable("unknown_setting", format!("{key} is not a setting that the app can save.")));
        }
        if super::source(key) == Source::Env {
            return Err(ApiError::conflict("set_by_env", format!("{key} is set in the env of the server. Change it there.")));
        }
        let value = match value {
            Some(v) => {
                if let Some(why) = super::validate(key, v) {
                    return Err(ApiError::unprocessable("invalid_value", format!("{key}: {why}")));
                }
                Some(v.trim().to_string())
            }
            None => None,
        };
        out.push((key.clone(), value));
    }
    Ok(out)
}

/// Save or remove settings.
#[utoipa::path(put, path = "/v1/settings/environment", tag = "settings",
    request_body = EnvSettingsPut,
    responses((status = 200, body = EnvSettingsReply), (status = 409, description = "set_by_env"), (status = 422, description = "unknown_setting, invalid_value")),
    security(("api_key" = ["admin"]), ("session" = [])),
    description = "Scope: `admin`. Nothing is saved when one key is wrong. A saved value works at once. Other server copies read it within 60 seconds. Values are never logged.")]
async fn put_environment(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    ApiJson(b): ApiJson<EnvSettingsPut>,
) -> ApiResult<ApiJson<EnvSettingsReply>> {
    let changes = check_changes(&b.values)?;
    let user = session.map(|Extension(s)| s.user_id);
    store::apply(&st.pool, &st.crypto, user, &changes).await?;
    store::refresh(&st.pool, &st.crypto).await?;
    tracing::info!(keys = ?changes.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(), "app settings saved");
    Ok(ApiJson(reply(&st)))
}

#[derive(Serialize, ToSchema)]
pub struct EnvTestReply {
    pub ok: bool,
    /// What was checked, in plain words.
    pub message: String,
}

/// Check the Google keys by their shape. No login.
fn check_google(client_id: Option<&str>, secret_set: bool, public_url: bool) -> Result<(), String> {
    let Some(id) = client_id else { return Err("The Client ID is not set.".into()) };
    if !id.ends_with(".apps.googleusercontent.com") {
        return Err("The Client ID should end in .apps.googleusercontent.com. Copy it again from Google Cloud.".into());
    }
    if !secret_set {
        return Err("The Client secret is not set.".into());
    }
    if !public_url {
        return Err("AIKIDO_PUBLIC_URL is not set. Google needs it for the redirect address. Set it in .env.".into());
    }
    Ok(())
}

/// Check the Microsoft keys. The tenant must answer on the OpenID address. No login.
async fn check_microsoft(client_id: bool, secret_set: bool, tenant: &str) -> Result<(), String> {
    if !client_id {
        return Err("The Application (client) ID is not set.".into());
    }
    if !secret_set {
        return Err("The Client secret is not set.".into());
    }
    let url = format!("https://login.microsoftonline.com/{tenant}/v2.0/.well-known/openid-configuration");
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).build().map_err(|e| e.to_string())?;
    match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => Ok(()),
        Ok(_) => Err(format!("Microsoft does not know the tenant \"{tenant}\". Check the Tenant value.")),
        Err(_) => Err("Could not reach login.microsoftonline.com. Check the internet access of the server.".into()),
    }
}

/// Check a group of keys. Makes no login.
#[utoipa::path(post, path = "/v1/settings/environment/test/{group}", tag = "settings",
    params(("group" = String, Path, description = "`google` or `microsoft`")),
    responses((status = 200, body = EnvTestReply), (status = 404, description = "unknown_group")),
    security(("api_key" = ["admin"]), ("session" = [])),
    description = "Scope: `admin`. Google: the Client ID has the right shape, the secret is set, `AIKIDO_PUBLIC_URL` is set. \
        Microsoft: the keys are set and the tenant answers on its OpenID address. Real logins are not tried.")]
async fn test_group(State(st): State<AppState>, ApiPath(group): ApiPath<String>) -> ApiResult<ApiJson<EnvTestReply>> {
    let result = match group.as_str() {
        super::GOOGLE => check_google(
            super::get("GOOGLE_CLIENT_ID").as_deref(),
            super::get("GOOGLE_CLIENT_SECRET").is_some(),
            st.config.public_url.is_some(),
        ),
        super::MICROSOFT => {
            let tenant = super::get("MS_TENANT").unwrap_or_else(|| "common".into());
            check_microsoft(super::get("MS_CLIENT_ID").is_some(), super::get("MS_CLIENT_SECRET").is_some(), &tenant).await
        }
        _ => return Err(ApiError::not_found("unknown_group", "The group is `google` or `microsoft`.")),
    };
    Ok(ApiJson(match result {
        Ok(()) => EnvTestReply { ok: true, message: "The keys look right. Connect an account to try a real login.".into() },
        Err(message) => EnvTestReply { ok: false, message },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(pairs: &[(&str, Option<&str>)]) -> BTreeMap<String, Option<String>> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.map(str::to_string))).collect()
    }

    #[test]
    fn unknown_keys_and_bad_values_are_refused() {
        let e = check_changes(&put(&[("DATABASE_URL", Some("x"))])).unwrap_err();
        assert_eq!(e.code, "unknown_setting");
        let e = check_changes(&put(&[("GOOGLE_CLIENT_ID", Some("  "))])).unwrap_err();
        assert_eq!(e.code, "invalid_value");
        let e = check_changes(&put(&[("MS_TENANT", Some("a b"))])).unwrap_err();
        assert_eq!(e.code, "invalid_value");
    }

    #[test]
    fn good_values_are_trimmed_and_null_clears() {
        let ok = check_changes(&put(&[("GOOGLE_CLIENT_ID", Some("  abc.apps.googleusercontent.com ")), ("MS_TENANT", None)])).unwrap();
        assert_eq!(ok[0], ("GOOGLE_CLIENT_ID".to_string(), Some("abc.apps.googleusercontent.com".to_string())));
        assert_eq!(ok[1], ("MS_TENANT".to_string(), None));
    }

    #[test]
    fn a_secret_is_never_in_the_item() {
        let secret = super::super::item("GOOGLE_CLIENT_SECRET").unwrap();
        let out = item_out(secret);
        assert!(out.secret);
        assert!(out.value.is_none());
    }

    #[test]
    fn google_check_reads_the_shape_of_the_keys() {
        assert!(check_google(None, true, true).is_err());
        assert!(check_google(Some("12345"), true, true).is_err());
        assert!(check_google(Some("1.apps.googleusercontent.com"), false, true).is_err());
        assert!(check_google(Some("1.apps.googleusercontent.com"), true, false).is_err());
        assert!(check_google(Some("1.apps.googleusercontent.com"), true, true).is_ok());
    }
}
