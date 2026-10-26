//! Automation settings (build map 11.6): `GET` and `PATCH /v1/automations/settings`.
//! They are kept as one JSON object in `automation_settings.settings`. A missing key means the default.
use crate::automations::routes::caller;
use crate::auth::api_keys::Scope;
use crate::auth::middleware::{KeyScope, require_scope};
use crate::error::{ApiError, ApiJson, ApiResult};
use crate::state::AppState;
use crate::users::session::SessionCtx;
use axum::Extension;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AutomationSettings {
    /// HTTP Request may call private addresses (Ollama or n8n on the LAN). Off by default.
    pub allow_private_network: bool,
    /// `aikido.api` may call the lead delete route. Off by default.
    pub allow_lead_erase: bool,
    /// Fetch web page always obeys robots.txt when this is on.
    pub fetch_respect_robots: bool,
    /// Fetch web page refuses these hosts (and their sub-domains).
    pub fetch_blocked_domains: Vec<String>,
    /// Production runs are kept this many days (1 to 365).
    pub retention_days: i64,
    /// Test runs kept per workflow (1 to 100).
    pub test_runs_kept: i64,
    /// Webhook requests per minute per path (1 to 6000).
    pub webhook_rate_per_min: i64,
    /// How often the event triggers look for new events, in seconds (1 to 300).
    pub event_tick_secs: i64,
    /// A run waits this many hours for the AI budget before it fails (0 to 168).
    pub ai_budget_wait_hours: i64,
    /// Runs at the same time on one server copy (1 to 100). The env var `AIKIDO_AUTOMATIONS_MAX_PARALLEL` wins.
    pub max_parallel_runs: i64,
}

impl Default for AutomationSettings {
    fn default() -> Self {
        AutomationSettings {
            allow_private_network: false,
            allow_lead_erase: false,
            fetch_respect_robots: true,
            fetch_blocked_domains: vec!["linkedin.com".into(), "www.linkedin.com".into()],
            retention_days: 30,
            test_runs_kept: 20,
            webhook_rate_per_min: 60,
            event_tick_secs: 5,
            ai_budget_wait_hours: 24,
            max_parallel_runs: 10,
        }
    }
}

const KEYS: &[&str] = &[
    "allow_private_network",
    "allow_lead_erase",
    "fetch_respect_robots",
    "fetch_blocked_domains",
    "retention_days",
    "test_runs_kept",
    "webhook_rate_per_min",
    "event_tick_secs",
    "ai_budget_wait_hours",
    "max_parallel_runs",
];

fn bad(key: &str, why: &str) -> ApiError {
    ApiError::unprocessable("invalid_setting", format!("{key}: {why}"))
}

impl AutomationSettings {
    /// The settings from the stored object. Keys that are missing or wrong take the default.
    pub fn from_stored(v: &Value) -> Self {
        let mut s = AutomationSettings::default();
        if let Some(o) = v.as_object() {
            let _ = s.apply(o, false);
        }
        s
    }

    /// Change settings. With `strict`, an unknown key or a bad value is an error; else it is skipped.
    pub fn apply(&mut self, patch: &Map<String, Value>, strict: bool) -> Result<(), ApiError> {
        for (k, v) in patch {
            if !KEYS.contains(&k.as_str()) {
                if strict {
                    return Err(ApiError::unprocessable("unknown_setting", format!("There is no setting named `{k}`.")));
                }
                continue;
            }
            let r = self.set(k, v);
            if let (Err(e), true) = (r, strict) {
                return Err(e);
            }
        }
        Ok(())
    }

    fn set(&mut self, k: &str, v: &Value) -> Result<(), ApiError> {
        let flag = |v: &Value| v.as_bool().ok_or_else(|| bad(k, "must be true or false."));
        let num = |v: &Value, lo: i64, hi: i64| v.as_i64().filter(|n| (lo..=hi).contains(n)).ok_or_else(|| bad(k, &format!("must be a whole number from {lo} to {hi}.")));
        match k {
            "allow_private_network" => self.allow_private_network = flag(v)?,
            "allow_lead_erase" => self.allow_lead_erase = flag(v)?,
            "fetch_respect_robots" => self.fetch_respect_robots = flag(v)?,
            "retention_days" => self.retention_days = num(v, 1, 365)?,
            "test_runs_kept" => self.test_runs_kept = num(v, 1, 100)?,
            "webhook_rate_per_min" => self.webhook_rate_per_min = num(v, 1, 6000)?,
            "event_tick_secs" => self.event_tick_secs = num(v, 1, 300)?,
            "ai_budget_wait_hours" => self.ai_budget_wait_hours = num(v, 0, 168)?,
            "max_parallel_runs" => self.max_parallel_runs = num(v, 1, 100)?,
            "fetch_blocked_domains" => {
                let list = v.as_array().filter(|a| a.len() <= 200).ok_or_else(|| bad(k, "must be a list of up to 200 host names."))?;
                let mut out = vec![];
                for x in list {
                    let h = x.as_str().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty() && s.len() <= 253 && !s.contains(['/', ' ', '@', ':'])).ok_or_else(|| bad(k, "every entry must be a host name like `example.com`."))?;
                    if !out.contains(&h) {
                        out.push(h);
                    }
                }
                self.fetch_blocked_domains = out;
            }
            _ => {}
        }
        Ok(())
    }

    /// Is this host (or a parent domain of it) on the block list?
    pub fn is_blocked_host(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_lowercase();
        self.fetch_blocked_domains.iter().any(|d| host == *d || host.ends_with(&format!(".{d}")))
    }
}

pub async fn load(pool: &sqlx::PgPool) -> Result<AutomationSettings, sqlx::Error> {
    let v: Option<Value> = sqlx::query_scalar("SELECT settings FROM automation_settings LIMIT 1").fetch_optional(pool).await?;
    Ok(AutomationSettings::from_stored(&v.unwrap_or_else(|| json!({}))))
}

pub fn router(state: AppState) -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(get_settings, patch_settings)).route_layer(axum::middleware::from_fn_with_state((state, Scope::Automations), require_scope))
}

/// The automation settings.
#[utoipa::path(get, path = "/v1/automations/settings", tag = "automations",
    responses((status = 200, body = AutomationSettings)),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`.")]
pub async fn get_settings(State(st): State<AppState>) -> ApiResult<ApiJson<AutomationSettings>> {
    Ok(ApiJson(load(&st.pool).await?))
}

/// Change automation settings. Send only the keys to change.
#[utoipa::path(patch, path = "/v1/automations/settings", tag = "automations",
    request_body = Object,
    responses((status = 200, body = AutomationSettings), (status = 403, description = "admin_only"), (status = 422, description = "invalid_setting or unknown_setting")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`, and an owner/admin session or the `admin` key.")]
pub async fn patch_settings(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiJson(patch): ApiJson<Map<String, Value>>,
) -> ApiResult<ApiJson<AutomationSettings>> {
    if !caller(&session, &key).is_admin {
        return Err(ApiError::forbidden("admin_only", "Only an owner or an admin can change the automation settings."));
    }
    let mut tx = st.pool.begin().await?;
    let stored: Option<Value> = sqlx::query_scalar("SELECT settings FROM automation_settings LIMIT 1 FOR UPDATE").fetch_optional(&mut *tx).await?;
    let mut stored = stored.and_then(|v| v.as_object().cloned()).unwrap_or_default();
    let mut s = AutomationSettings::from_stored(&Value::Object(stored.clone()));
    s.apply(&patch, true)?;
    for k in patch.keys() {
        if let Some(v) = serde_json::to_value(&s).ok().and_then(|v| v.get(k).cloned()) {
            stored.insert(k.clone(), v);
        }
    }
    sqlx::query("UPDATE automation_settings SET settings = $1").bind(Value::Object(stored)).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(ApiJson(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap()
    }

    #[test]
    fn defaults_follow_the_build_map() {
        let s = AutomationSettings::default();
        assert!(!s.allow_private_network && !s.allow_lead_erase && s.fetch_respect_robots);
        assert_eq!(s.fetch_blocked_domains, vec!["linkedin.com", "www.linkedin.com"]);
        assert_eq!((s.retention_days, s.test_runs_kept, s.webhook_rate_per_min, s.event_tick_secs, s.ai_budget_wait_hours, s.max_parallel_runs), (30, 20, 60, 5, 24, 10));
        assert_eq!(AutomationSettings::from_stored(&json!({})), s);
        assert_eq!(AutomationSettings::from_stored(&Value::Null), s);
    }

    #[test]
    fn a_patch_is_checked() {
        let mut s = AutomationSettings::default();
        s.apply(&obj(json!({ "allow_private_network": true, "retention_days": 90, "fetch_blocked_domains": [" Example.COM ", "example.com"] })), true).unwrap();
        assert!(s.allow_private_network);
        assert_eq!(s.retention_days, 90);
        assert_eq!(s.fetch_blocked_domains, vec!["example.com"]);
        for bad in [json!({ "retention_days": 0 }), json!({ "retention_days": "x" }), json!({ "allow_lead_erase": "yes" }), json!({ "fetch_blocked_domains": ["a/b"] }), json!({ "max_parallel_runs": 101 })] {
            assert_eq!(AutomationSettings::default().apply(&obj(bad.clone()), true).unwrap_err().code, "invalid_setting", "{bad}");
        }
        assert_eq!(AutomationSettings::default().apply(&obj(json!({ "nope": 1 })), true).unwrap_err().code, "unknown_setting");
        // a stored object with a bad or unknown key does not break reading
        let s = AutomationSettings::from_stored(&json!({ "retention_days": -5, "nope": 1, "test_runs_kept": 7 }));
        assert_eq!((s.retention_days, s.test_runs_kept), (30, 7));
    }

    #[test]
    fn blocked_hosts_include_sub_domains() {
        let s = AutomationSettings::default();
        assert!(s.is_blocked_host("linkedin.com"));
        assert!(s.is_blocked_host("WWW.LinkedIn.com"));
        assert!(s.is_blocked_host("de.linkedin.com"));
        assert!(!s.is_blocked_host("notlinkedin.com"));
        assert!(!s.is_blocked_host("example.com"));
    }
}
