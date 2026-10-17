//! `/v1/generator/*` (scope `generator`): settings, providers, the review of generations, usage, status, pause.
use super::draft;
use super::generations;
use super::llm::{LlmRequest, ProviderKind};
use super::providers::{self, Provider};
use super::repo;
use super::settings::{GeneratorSettings, SettingsError};
use crate::auth::api_keys::Scope;
use crate::auth::middleware::require_scope;
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Routes that need a key with scope `generator`.
pub fn router(state: AppState) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_settings, patch_settings))
        .routes(routes!(list_providers, create_provider))
        .routes(routes!(patch_provider, delete_provider))
        .routes(routes!(test_provider))
        .routes(routes!(draft::draft))
        .routes(routes!(draft::preview))
        .routes(routes!(generations::list_generations))
        .routes(routes!(generations::bulk))
        .routes(routes!(generations::get_generation, generations::patch_generation))
        .routes(routes!(generations::approve))
        .routes(routes!(generations::reject))
        .routes(routes!(get_usage))
        .routes(routes!(get_status))
        .routes(routes!(pause_engine))
        .routes(routes!(resume_engine))
        .routes(routes!(get_campaign, patch_campaign))
        .route_layer(axum::middleware::from_fn_with_state((state, Scope::Generator), require_scope))
}

fn settings_error(e: SettingsError) -> ApiError {
    ApiError::unprocessable(e.code, e.message)
}

// ---------- settings ----------

#[derive(Serialize, ToSchema)]
pub struct SettingsReply {
    pub settings: GeneratorSettings,
}

/// Read the generator settings.
#[utoipa::path(get, path = "/v1/generator/settings", tag = "generator",
    responses((status = 200, body = SettingsReply)), security(("api_key" = ["generator"])), description = "Scope: `generator`.")]
async fn get_settings(State(st): State<AppState>) -> ApiResult<ApiJson<SettingsReply>> {
    let mut conn = st.pool.acquire().await?;
    Ok(ApiJson(SettingsReply { settings: repo::load_settings(&mut conn).await? }))
}

/// Change the generator settings. Send only the fields to change.
#[utoipa::path(patch, path = "/v1/generator/settings", tag = "generator",
    request_body(content = GeneratorSettings, description = "Only the fields to change."),
    responses((status = 200, body = SettingsReply), (status = 422, description = "A setting is not valid, or a provider id does not exist")),
    security(("api_key" = ["generator"])),
    description = "Scope: `generator`. Merges the fields, validates the full result, then saves. `slot_provider_id` and `draft_provider_id` must be providers that exist.")]
async fn patch_settings(State(st): State<AppState>, ApiJson(patch): ApiJson<Value>) -> ApiResult<ApiJson<SettingsReply>> {
    let mut tx = st.pool.begin().await?;
    let (current,): (Value,) = sqlx::query_as("SELECT settings FROM generator_defaults WHERE id = 1 FOR UPDATE").fetch_one(&mut *tx).await?;
    let merged = GeneratorSettings::from_stored(current).merged(&patch).map_err(settings_error)?;
    merged.validate().map_err(settings_error)?;
    for (name, id) in [("slot_provider_id", merged.slot_provider_id), ("draft_provider_id", merged.draft_provider_id)] {
        if let Some(id) = id {
            if providers::get(&mut tx, id).await?.is_none() {
                return Err(ApiError::unprocessable("unknown_provider", format!("{name}: no provider with this id.")));
            }
        }
    }
    sqlx::query("UPDATE generator_defaults SET settings = $1, updated_at = now() WHERE id = 1").bind(serde_json::to_value(&merged).unwrap_or_default()).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(ApiJson(SettingsReply { settings: merged }))
}

// ---------- providers ----------

#[derive(Serialize, ToSchema)]
pub struct ProvidersReply {
    pub providers: Vec<Provider>,
}

/// List the providers. The API key is never returned.
#[utoipa::path(get, path = "/v1/generator/providers", tag = "generator",
    responses((status = 200, body = ProvidersReply)), security(("api_key" = ["generator"])), description = "Scope: `generator`.")]
async fn list_providers(State(st): State<AppState>) -> ApiResult<ApiJson<ProvidersReply>> {
    let mut conn = st.pool.acquire().await?;
    Ok(ApiJson(ProvidersReply { providers: providers::list(&mut conn).await? }))
}

#[derive(Deserialize, ToSchema)]
pub struct NewProvider {
    /// `anthropic` or `openai_compatible` (OpenAI, OpenRouter, Ollama, LM Studio, vLLM).
    pub kind: String,
    pub name: String,
    /// `anthropic`: default `https://api.anthropic.com`. `openai_compatible`: required, for example `https://api.openai.com/v1` or `http://localhost:11434/v1`. `/chat/completions` is added.
    pub base_url: Option<String>,
    /// The model id, for example `claude-haiku-5-5`.
    pub model: String,
    /// Write only. Needed for `anthropic`; optional for a local server.
    pub api_key: Option<String>,
    /// 1 to 64. Default 4.
    pub max_concurrency: Option<i32>,
    /// 5 to 600. Default 60.
    pub timeout_secs: Option<i32>,
    /// `openai_compatible` only: ask for JSON mode. Default `true`. Turn it off for a server that refuses `response_format`.
    pub json_mode: Option<bool>,
}

fn invalid_provider(m: impl Into<String>) -> ApiError {
    ApiError::unprocessable("invalid_provider", m)
}

fn check_text(what: &str, v: &str, max: usize) -> ApiResult<String> {
    let v = v.trim();
    if v.is_empty() || v.chars().count() > max {
        return Err(invalid_provider(format!("{what} must be 1 to {max} characters.")));
    }
    Ok(v.to_string())
}

fn check_url(url: &str) -> ApiResult<String> {
    let url = url.trim().trim_end_matches('/');
    match reqwest::Url::parse(url) {
        Ok(u) if matches!(u.scheme(), "http" | "https") && u.host().is_some() && u.username().is_empty() && u.password().is_none() => Ok(url.to_string()),
        _ => Err(invalid_provider("base_url must be an http or https address without a user name or password.")),
    }
}

fn check_range(what: &str, v: i32, min: i32, max: i32) -> ApiResult<i32> {
    if (min..=max).contains(&v) { Ok(v) } else { Err(invalid_provider(format!("{what} must be {min} to {max}."))) }
}

fn check_key(key: &str) -> ApiResult<String> {
    let key = key.trim();
    if key.is_empty() || key.len() > 500 || key.chars().any(char::is_control) {
        return Err(invalid_provider("api_key must be 1 to 500 characters."));
    }
    Ok(key.to_string())
}

/// Add a provider.
#[utoipa::path(post, path = "/v1/generator/providers", tag = "generator",
    request_body = NewProvider, responses((status = 201, body = Provider), (status = 422, description = "A field is not valid")),
    security(("api_key" = ["generator"])),
    description = "Scope: `generator`. The key is encrypted (AES-256-GCM) and is never returned. Test it with `POST .../providers/{id}/test`.")]
async fn create_provider(State(st): State<AppState>, ApiJson(b): ApiJson<NewProvider>) -> ApiResult<(StatusCode, ApiJson<Provider>)> {
    let kind = ProviderKind::parse(b.kind.trim()).ok_or_else(|| invalid_provider("kind must be anthropic or openai_compatible."))?;
    let name = check_text("name", &b.name, 100)?;
    let model = check_text("model", &b.model, 200)?;
    let base_url = match (kind, b.base_url.as_deref().filter(|u| !u.trim().is_empty())) {
        (_, Some(u)) => check_url(u)?,
        (ProviderKind::Anthropic, None) => super::llm::anthropic::DEFAULT_BASE_URL.to_string(),
        (ProviderKind::OpenAiCompatible, None) => return Err(invalid_provider("base_url is required for openai_compatible.")),
    };
    let key = b.api_key.as_deref().filter(|k| !k.trim().is_empty()).map(check_key).transpose()?;
    if kind == ProviderKind::Anthropic && key.is_none() {
        return Err(invalid_provider("api_key is required for anthropic."));
    }
    let max_concurrency = check_range("max_concurrency", b.max_concurrency.unwrap_or(4), 1, 64)?;
    let timeout_secs = check_range("timeout_secs", b.timeout_secs.unwrap_or(60), 5, 600)?;
    let id = Uuid::now_v7();
    let sealed = key.as_deref().map(|k| providers::seal_key(&st.crypto, id, k)).transpose().map_err(|e| {
        tracing::error!(error = %e, "could not encrypt a provider key");
        ApiError::internal()
    })?;
    let (ct, nonce, version) = match sealed {
        Some((c, n, v)) => (Some(c), Some(n), Some(v)),
        None => (None, None, None),
    };
    let now = st.now();
    sqlx::query(
        "INSERT INTO llm_providers (id, kind, name, base_url, model, api_key_ciphertext, api_key_nonce, key_version, max_concurrency, timeout_secs, json_mode, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $12)",
    )
    .bind(id)
    .bind(kind.as_str())
    .bind(name)
    .bind(base_url)
    .bind(model)
    .bind(ct)
    .bind(nonce)
    .bind(version)
    .bind(max_concurrency)
    .bind(timeout_secs)
    .bind(b.json_mode.unwrap_or(true))
    .bind(now)
    .execute(&st.pool)
    .await?;
    let mut conn = st.pool.acquire().await?;
    let p = providers::get(&mut conn, id).await?.ok_or_else(ApiError::internal)?;
    tracing::info!(provider_id = %id, kind = kind.as_str(), "LLM provider added");
    Ok((StatusCode::CREATED, ApiJson(p)))
}

#[derive(Deserialize, ToSchema)]
pub struct PatchProvider {
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    /// Write only. A new key also clears `auth_failed`.
    pub api_key: Option<String>,
    pub max_concurrency: Option<i32>,
    pub timeout_secs: Option<i32>,
    pub json_mode: Option<bool>,
}

/// Change a provider. Send only the fields to change.
#[utoipa::path(patch, path = "/v1/generator/providers/{id}", tag = "generator",
    params(("id" = Uuid, Path)), request_body = PatchProvider,
    responses((status = 200, body = Provider), (status = 404, description = "No such provider"), (status = 422, description = "A field is not valid")),
    security(("api_key" = ["generator"])), description = "Scope: `generator`. The kind can not change. A new `api_key` replaces the old one and sets the status back to `ok`.")]
async fn patch_provider(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(b): ApiJson<PatchProvider>) -> ApiResult<ApiJson<Provider>> {
    let name = b.name.as_deref().map(|v| check_text("name", v, 100)).transpose()?;
    let model = b.model.as_deref().map(|v| check_text("model", v, 200)).transpose()?;
    let base_url = b.base_url.as_deref().map(check_url).transpose()?;
    let key = b.api_key.as_deref().map(check_key).transpose()?;
    let max_concurrency = b.max_concurrency.map(|v| check_range("max_concurrency", v, 1, 64)).transpose()?;
    let timeout_secs = b.timeout_secs.map(|v| check_range("timeout_secs", v, 5, 600)).transpose()?;
    let sealed = key.as_deref().map(|k| providers::seal_key(&st.crypto, id, k)).transpose().map_err(|e| {
        tracing::error!(error = %e, "could not encrypt a provider key");
        ApiError::internal()
    })?;
    let (ct, nonce, version) = match sealed {
        Some((c, n, v)) => (Some(c), Some(n), Some(v)),
        None => (None, None, None),
    };
    let done = sqlx::query(
        "UPDATE llm_providers SET name = COALESCE($2, name), base_url = COALESCE($3, base_url), model = COALESCE($4, model),
                max_concurrency = COALESCE($5, max_concurrency), timeout_secs = COALESCE($6, timeout_secs), json_mode = COALESCE($7, json_mode),
                api_key_ciphertext = COALESCE($8, api_key_ciphertext), api_key_nonce = COALESCE($9, api_key_nonce), key_version = COALESCE($10, key_version),
                status = CASE WHEN $8::bytea IS NOT NULL THEN 'ok' ELSE status END, updated_at = $11
          WHERE id = $1",
    )
    .bind(id)
    .bind(name)
    .bind(base_url)
    .bind(model)
    .bind(max_concurrency)
    .bind(timeout_secs)
    .bind(b.json_mode)
    .bind(ct)
    .bind(nonce)
    .bind(version)
    .bind(st.now())
    .execute(&st.pool)
    .await?;
    if done.rows_affected() == 0 {
        return Err(ApiError::not_found("provider_not_found", "No provider with this id."));
    }
    let mut conn = st.pool.acquire().await?;
    Ok(ApiJson(providers::get(&mut conn, id).await?.ok_or_else(ApiError::internal)?))
}

#[derive(Serialize, ToSchema)]
pub struct DeletedReply {
    pub deleted: bool,
}

/// Remove a provider.
#[utoipa::path(delete, path = "/v1/generator/providers/{id}", tag = "generator",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = DeletedReply), (status = 404, description = "No such provider"), (status = 409, description = "provider_in_use: a setting still points at it")),
    security(("api_key" = ["generator"])), description = "Scope: `generator`. Refused while `slot_provider_id` or `draft_provider_id` is this provider.")]
async fn delete_provider(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<DeletedReply>> {
    let mut tx = st.pool.begin().await?;
    // The settings row is the lock: a PATCH of the settings waits for this.
    let (settings,): (Value,) = sqlx::query_as("SELECT settings FROM generator_defaults WHERE id = 1 FOR UPDATE").fetch_one(&mut *tx).await?;
    let settings = GeneratorSettings::from_stored(settings);
    if settings.slot_provider_id == Some(id) || settings.draft_provider_id == Some(id) {
        return Err(ApiError::conflict("provider_in_use", "A setting still uses this provider. Point slot_provider_id and draft_provider_id at another one first."));
    }
    let done = sqlx::query("DELETE FROM llm_providers WHERE id = $1").bind(id).execute(&mut *tx).await?;
    if done.rows_affected() == 0 {
        return Err(ApiError::not_found("provider_not_found", "No provider with this id."));
    }
    tx.commit().await?;
    Ok(ApiJson(DeletedReply { deleted: true }))
}

#[derive(Serialize, ToSchema)]
pub struct TestReply {
    pub ok: bool,
    pub model: String,
    pub duration_ms: u64,
    /// Why it failed: `provider_auth`, `rate_limited`, `bad_request`, ...
    pub error_code: Option<String>,
    pub message: Option<String>,
}

/// Send one tiny call to the provider.
#[utoipa::path(post, path = "/v1/generator/providers/{id}/test", tag = "generator",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = TestReply), (status = 404, description = "No such provider")),
    security(("api_key" = ["generator"])),
    description = "Scope: `generator`. Always answers 200 when the provider exists; `ok` says whether the call worked. A refused key sets the status to `auth_failed`; a good call sets it back to `ok`. Writes an `llm_calls` row (purpose `test`).")]
async fn test_provider(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<TestReply>> {
    let mut conn = st.pool.acquire().await?;
    let max_attempts = repo::load_settings(&mut conn).await?.max_attempts;
    drop(conn);
    let (cfg, _) = providers::load_config(&st.pool, &st.crypto, id, max_attempts).await.map_err(|e| match e {
        providers::ConfigError::NotFound => ApiError::not_found("provider_not_found", "No provider with this id."),
        providers::ConfigError::BadKey => ApiError::unprocessable("provider_key_unreadable", "The saved API key can not be read. Save the key again."),
        providers::ConfigError::Db(e) => e.into(),
    })?;
    let client = st.llm.build(&cfg).map_err(|_| ApiError::unprocessable("invalid_provider", "The provider is not set up right (check its base_url)."))?;
    let req = LlmRequest { system: "Answer with the JSON {\"ok\": true} and nothing else.".into(), user: "ping".into(), max_output_tokens: 20, json: true, temperature: 0.0 };
    let started = std::time::Instant::now();
    let res = client.complete(&req).await;
    let duration_ms = started.elapsed().as_millis() as u64;
    let now = st.now();
    let mut conn = st.pool.acquire().await?;
    let (ok, model, code, tokens) = match &res {
        Ok(r) => (true, r.model.clone(), None, (r.input_tokens, r.output_tokens, r.cached_tokens)),
        Err(e) => (false, cfg.model.clone(), Some(e.code()), (0, 0, 0)),
    };
    repo::record_call(
        &mut conn,
        &repo::CallRecord {
            purpose: "test",
            provider_id: Some(id),
            model: model.clone(),
            campaign_id: None,
            input_tokens: tokens.0,
            output_tokens: tokens.1,
            cached_tokens: tokens.2,
            duration_ms: duration_ms as u32,
            ok,
            error_code: code.clone(),
        },
        now,
    )
    .await?;
    drop(conn);
    match &res {
        Ok(_) => providers::set_status(&st.pool, id, "ok", now).await?,
        Err(super::llm::LlmError::Auth) => providers::mark_auth_failed(&st.pool, id, now).await,
        Err(_) => {}
    }
    let message = res.err().map(|e| match e {
        super::llm::LlmError::Auth => "The provider refused the API key.".to_string(),
        super::llm::LlmError::Transient { code, .. } => format!("The provider could not answer ({code})."),
        super::llm::LlmError::BadRequest(m) => format!("The provider refused the request: {m}"),
        super::llm::LlmError::Refused => "The model declined the test.".to_string(),
        super::llm::LlmError::BadResponse(m) => format!("The answer could not be read: {m}"),
    });
    Ok(ApiJson(TestReply { ok, model, duration_ms, error_code: code, message }))
}

// ---------- usage ----------

#[derive(Deserialize, IntoParams)]
pub struct UsageQuery {
    /// First day (UTC), `YYYY-MM-DD`. Default: 30 days ago.
    from: Option<NaiveDate>,
    /// Last day (UTC), included. Default: today.
    to: Option<NaiveDate>,
}

#[derive(Serialize, ToSchema, Default, Clone)]
pub struct UsageRow {
    /// The day, the campaign id, the model or the purpose.
    pub key: String,
    pub calls: i64,
    pub failed_calls: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Input tokens read from the provider cache. A high number means the cache works.
    pub cached_tokens: i64,
    /// Only for models that have a price in the settings (`prices`).
    pub cost_usd: Option<f64>,
}

#[derive(Serialize, ToSchema)]
pub struct UsageReply {
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub total: UsageRow,
    pub by_day: Vec<UsageRow>,
    pub by_campaign: Vec<UsageRow>,
    pub by_model: Vec<UsageRow>,
    pub by_purpose: Vec<UsageRow>,
}

type UsageTuple = (Option<String>, i64, i64, i64, i64, i64);

fn rows(v: Vec<UsageTuple>) -> Vec<UsageRow> {
    v.into_iter()
        .map(|(key, calls, failed_calls, input_tokens, output_tokens, cached_tokens)| UsageRow {
            key: key.unwrap_or_else(|| "none".into()),
            calls,
            failed_calls,
            input_tokens,
            output_tokens,
            cached_tokens,
            cost_usd: None,
        })
        .collect()
}

fn cost(settings: &GeneratorSettings, model: &str, input: i64, output: i64) -> Option<f64> {
    settings.prices.get(model).map(|p| (input as f64 * p.input + output as f64 * p.output) / 1_000_000.0)
}

/// Tokens by day, campaign, model and purpose.
#[utoipa::path(get, path = "/v1/generator/usage", tag = "generator",
    params(UsageQuery), responses((status = 200, body = UsageReply), (status = 422, description = "from is after to, or the range is over 366 days")),
    security(("api_key" = ["generator"])),
    description = "Scope: `generator`. From the `llm_calls` table: one row per call, no prompt text. Check `cached_tokens` to see whether the provider cache works. \
        Money (`cost_usd`) is shown for models that have a price in the `prices` setting.")]
async fn get_usage(State(st): State<AppState>, ApiQuery(q): ApiQuery<UsageQuery>) -> ApiResult<ApiJson<UsageReply>> {
    let today = st.now().date_naive();
    let to = q.to.unwrap_or(today);
    let from = q.from.unwrap_or(to - Duration::days(30));
    if from > to || (to - from).num_days() > 366 {
        return Err(ApiError::unprocessable("invalid_range", "from must not be after to, and the range is at most 366 days."));
    }
    let start: DateTime<Utc> = Utc.from_utc_datetime(&from.and_hms_opt(0, 0, 0).unwrap_or_default());
    let end: DateTime<Utc> = Utc.from_utc_datetime(&(to + Duration::days(1)).and_hms_opt(0, 0, 0).unwrap_or_default());
    let mut conn = st.pool.acquire().await?;
    let settings = repo::load_settings(&mut conn).await?;
    async fn fetch(pool: &sqlx::PgPool, expr: &str, start: DateTime<Utc>, end: DateTime<Utc>) -> Result<Vec<UsageRow>, sqlx::Error> {
        let sql = format!(
            "SELECT {expr}, count(*), count(*) FILTER (WHERE NOT ok), COALESCE(SUM(input_tokens), 0)::bigint, COALESCE(SUM(output_tokens), 0)::bigint,
                    COALESCE(SUM(cached_tokens), 0)::bigint
               FROM llm_calls WHERE created_at >= $1 AND created_at < $2 GROUP BY 1 ORDER BY 1"
        );
        Ok(rows(sqlx::query_as::<_, UsageTuple>(&sql).bind(start).bind(end).fetch_all(pool).await?))
    }
    let by_day = fetch(&st.pool, "to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD')", start, end).await?;
    let by_campaign = fetch(&st.pool, "campaign_id::text", start, end).await?;
    let mut by_model = fetch(&st.pool, "model", start, end).await?;
    let by_purpose = fetch(&st.pool, "purpose", start, end).await?;
    drop(conn);
    for r in &mut by_model {
        r.cost_usd = cost(&settings, &r.key, r.input_tokens, r.output_tokens);
    }
    let mut total = by_day.iter().fold(UsageRow { key: "total".into(), ..Default::default() }, |mut a, r| {
        a.calls += r.calls;
        a.failed_calls += r.failed_calls;
        a.input_tokens += r.input_tokens;
        a.output_tokens += r.output_tokens;
        a.cached_tokens += r.cached_tokens;
        a
    });
    let priced: Vec<f64> = by_model.iter().filter_map(|r| r.cost_usd).collect();
    total.cost_usd = (!priced.is_empty()).then(|| priced.iter().sum());
    Ok(ApiJson(UsageReply { from, to, total, by_day, by_campaign, by_model, by_purpose }))
}

// ---------- status, pause ----------

#[derive(Serialize, ToSchema)]
pub struct ProviderState {
    pub id: Uuid,
    pub name: String,
    pub model: String,
    /// `ok` or `auth_failed`.
    pub status: String,
}

#[derive(Serialize, ToSchema)]
pub struct BudgetState {
    /// 0 = no limit.
    pub daily_limit: u64,
    pub used_today: i64,
    /// `null` when there is no limit.
    pub left_today: Option<i64>,
    pub reached: bool,
}

#[derive(Serialize, ToSchema)]
pub struct GeneratorStatus {
    pub paused: bool,
    /// `false` when the server runs with `AIKIDO_GENERATOR_ENABLED=false` (this copy writes no AI text).
    pub worker_enabled: bool,
    /// When a worker last ran. `null`: never.
    pub last_tick_at: Option<DateTime<Utc>>,
    /// Generations per status.
    pub queue: BTreeMap<String, i64>,
    pub budget: BudgetState,
    pub slot_provider: Option<ProviderState>,
    pub draft_provider: Option<ProviderState>,
    /// Things to look at. Each starts with a code: `paused`, `no_slot_provider`, `provider_auth_failed`, `daily_budget_reached`,
    /// `campaign_budget_reached`, `failed_generations`, `awaiting_review`.
    pub warnings: Vec<String>,
}

async fn provider_state(conn: &mut sqlx::PgConnection, id: Option<Uuid>) -> Result<Option<ProviderState>, sqlx::Error> {
    Ok(match id {
        Some(id) => providers::get(conn, id).await?.map(|p| ProviderState { id: p.id, name: p.name, model: p.model, status: p.status }),
        None => None,
    })
}

/// Paused or not, the last tick, the queue, the budget left, the provider state and warnings.
#[utoipa::path(get, path = "/v1/generator/status", tag = "generator",
    responses((status = 200, body = GeneratorStatus)), security(("api_key" = ["generator"])), description = "Scope: `generator`.")]
async fn get_status(State(st): State<AppState>) -> ApiResult<ApiJson<GeneratorStatus>> {
    let now = st.now();
    let mut conn = st.pool.acquire().await?;
    let settings = repo::load_settings(&mut conn).await?;
    let (paused, last_tick_at): (bool, Option<DateTime<Utc>>) =
        sqlx::query_as("SELECT engine_paused, last_tick_at FROM generator_defaults WHERE id = 1").fetch_one(&mut *conn).await?;
    let queue: BTreeMap<String, i64> = sqlx::query_as::<_, (String, i64)>("SELECT status, count(*) FROM generations GROUP BY 1").fetch_all(&mut *conn).await?.into_iter().collect();
    let used_today = repo::tokens_today(&mut conn, now).await?;
    let slot_provider = provider_state(&mut conn, settings.slot_provider_id).await?;
    let draft_provider = provider_state(&mut conn, settings.draft_provider_id).await?;
    let (over_budget,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM generator_campaigns gc WHERE gc.token_budget IS NOT NULL
            AND (SELECT COALESCE(SUM(input_tokens + output_tokens), 0) FROM llm_calls WHERE campaign_id = gc.campaign_id) >= gc.token_budget",
    )
    .fetch_one(&mut *conn)
    .await?;
    drop(conn);
    let reached = settings.daily_token_budget > 0 && used_today as u64 >= settings.daily_token_budget;
    let mut warnings = Vec::new();
    if paused {
        warnings.push("paused: the generator is paused. No new AI text is written.".to_string());
    }
    match &slot_provider {
        None => warnings.push("no_slot_provider: no slot provider is set (or it was removed). Steps with AI slots use their fallback text.".into()),
        Some(p) if p.status == "auth_failed" => warnings.push(format!("provider_auth_failed: the slot provider '{}' refused its API key. Replace the key or test it again.", p.name)),
        Some(_) => {}
    }
    if reached {
        warnings.push("daily_budget_reached: the daily token budget is used up. Steps use their fallback text (or wait).".into());
    }
    if over_budget > 0 {
        warnings.push(format!("campaign_budget_reached: {over_budget} campaign(s) used their token budget. Their steps use fallback text (or wait)."));
    }
    if let Some(n) = queue.get("failed").filter(|n| **n > 0) {
        warnings.push(format!("failed_generations: {n} generation(s) failed. See GET /v1/generator/generations?status=failed."));
    }
    if let Some(n) = queue.get("pending_review").filter(|n| **n > 0) {
        warnings.push(format!("awaiting_review: {n} generation(s) wait for approval. Their steps are held."));
    }
    Ok(ApiJson(GeneratorStatus {
        paused,
        worker_enabled: st.config.generator_enabled,
        last_tick_at,
        queue,
        budget: BudgetState {
            daily_limit: settings.daily_token_budget,
            used_today,
            left_today: (settings.daily_token_budget > 0).then(|| (settings.daily_token_budget as i64 - used_today).max(0)),
            reached,
        },
        slot_provider,
        draft_provider,
        warnings,
    }))
}

#[derive(Serialize, ToSchema)]
pub struct EnginePauseReply {
    pub paused: bool,
}

async fn set_engine_paused(st: &AppState, paused: bool) -> ApiResult<ApiJson<EnginePauseReply>> {
    sqlx::query("UPDATE generator_defaults SET engine_paused = $1, updated_at = now() WHERE id = 1").bind(paused).execute(&st.pool).await?;
    tracing::info!(paused, "generator pause flag changed");
    Ok(ApiJson(EnginePauseReply { paused }))
}

/// Pause the generator.
#[utoipa::path(post, path = "/v1/generator/pause", tag = "generator",
    responses((status = 200, body = EnginePauseReply)), security(("api_key" = ["generator"])),
    description = "Scope: `generator`. No new AI text is written. Text that is ready is still used. Steps that have none use their fallback text (or wait).")]
async fn pause_engine(State(st): State<AppState>) -> ApiResult<ApiJson<EnginePauseReply>> {
    set_engine_paused(&st, true).await
}

/// Resume the generator.
#[utoipa::path(post, path = "/v1/generator/resume", tag = "generator",
    responses((status = 200, body = EnginePauseReply)), security(("api_key" = ["generator"])), description = "Scope: `generator`.")]
async fn resume_engine(State(st): State<AppState>) -> ApiResult<ApiJson<EnginePauseReply>> {
    set_engine_paused(&st, false).await
}

// ---------- campaign overrides ----------

#[derive(Serialize, ToSchema)]
pub struct CampaignGeneratorReply {
    pub campaign_id: Uuid,
    /// `null`: use the global setting.
    pub review_required: Option<bool>,
    /// Total tokens for the campaign (input + output). `null`: no limit.
    pub token_budget: Option<i64>,
    /// Tokens used by the campaign so far.
    pub tokens_used: i64,
}

async fn campaign_reply(st: &AppState, id: Uuid) -> ApiResult<ApiJson<CampaignGeneratorReply>> {
    let mut conn = st.pool.acquire().await?;
    let exists: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM campaigns WHERE id = $1").bind(id).fetch_optional(&mut *conn).await?;
    if exists.is_none() {
        return Err(ApiError::not_found("campaign_not_found", "No campaign with this id."));
    }
    let (review_required, token_budget) = repo::campaign_overrides(&mut conn, id).await?;
    let tokens_used = repo::tokens_of_campaign(&mut conn, id).await?;
    Ok(ApiJson(CampaignGeneratorReply { campaign_id: id, review_required, token_budget, tokens_used }))
}

/// The generator settings of one campaign.
#[utoipa::path(get, path = "/v1/generator/campaigns/{id}", tag = "generator",
    params(("id" = Uuid, Path)), responses((status = 200, body = CampaignGeneratorReply), (status = 404, description = "No such campaign")),
    security(("api_key" = ["generator"])), description = "Scope: `generator`.")]
async fn get_campaign(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<CampaignGeneratorReply>> {
    campaign_reply(&st, id).await
}

#[derive(Deserialize, ToSchema)]
pub struct PatchCampaign {
    /// `true` or `false` overrides the global `review_required`. `null` goes back to the global setting.
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<bool>)]
    pub review_required: Option<Option<bool>>,
    /// Total tokens for the campaign. `null` removes the limit.
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<i64>)]
    pub token_budget: Option<Option<i64>>,
}

/// Tells "not sent" (`None`) from "sent as null" (`Some(None)`).
fn double_option<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

/// Override the review mode, or set a token budget, for one campaign.
#[utoipa::path(patch, path = "/v1/generator/campaigns/{id}", tag = "generator",
    params(("id" = Uuid, Path)), request_body = PatchCampaign,
    responses((status = 200, body = CampaignGeneratorReply), (status = 404, description = "No such campaign"), (status = 422, description = "token_budget is negative")),
    security(("api_key" = ["generator"])),
    description = "Scope: `generator`. Send only the fields to change; `null` clears one. When the budget is used up, the campaign's steps use their fallback text (or wait).")]
async fn patch_campaign(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(p): ApiJson<PatchCampaign>) -> ApiResult<ApiJson<CampaignGeneratorReply>> {
    if p.token_budget.flatten().is_some_and(|b| b < 0) {
        return Err(ApiError::unprocessable("invalid_setting", "token_budget must be 0 or more."));
    }
    let mut tx = st.pool.begin().await?;
    let exists: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM campaigns WHERE id = $1").bind(id).fetch_optional(&mut *tx).await?;
    if exists.is_none() {
        return Err(ApiError::not_found("campaign_not_found", "No campaign with this id."));
    }
    sqlx::query("INSERT INTO generator_campaigns (campaign_id) VALUES ($1) ON CONFLICT DO NOTHING").bind(id).execute(&mut *tx).await?;
    sqlx::query(
        "UPDATE generator_campaigns SET review_required = CASE WHEN $2 THEN $3 ELSE review_required END,
                token_budget = CASE WHEN $4 THEN $5 ELSE token_budget END, updated_at = $6 WHERE campaign_id = $1",
    )
    .bind(id)
    .bind(p.review_required.is_some())
    .bind(p.review_required.flatten())
    .bind(p.token_budget.is_some())
    .bind(p.token_budget.flatten())
    .bind(st.now())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    campaign_reply(&st, id).await
}
