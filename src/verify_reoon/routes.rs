//! `/v1/verify/reoon/*` (scope `integrations`). A member session may use these.
use super::client::{Balance, Mode, VerifyError};
use super::settings::{self, SettingsError, VerifySettings};
use crate::auth::api_keys::Scope;
use crate::auth::middleware::require_scope;
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::integrations::{routes::load_error, store};
use crate::leads::query::{LeadFilter, PageQuery, cursor_position, push_where};
use crate::state::AppState;
use crate::users::session::SessionCtx;
use axum::Extension;
use axum::extract::State;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, Postgres, QueryBuilder};
use std::collections::HashMap;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn router(state: AppState) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_balance))
        .routes(routes!(list_leads))
        .routes(routes!(get_lead_result))
        .routes(routes!(create_run, list_runs))
        .routes(routes!(get_run))
        .routes(routes!(cancel_run))
        .routes(routes!(resume_run))
        .routes(routes!(get_settings, patch_settings))
        .routes(routes!(get_status))
        .routes(routes!(pause_engine))
        .routes(routes!(resume_engine))
        .route_layer(axum::middleware::from_fn_with_state((state, Scope::Integrations), require_scope))
}

/// The live Reoon credit balance.
#[utoipa::path(get, path = "/v1/verify/reoon/balance", tag = "integrations",
    responses((status = 200, body = Balance), (status = 404, description = "`not_installed`"), (status = 409, description = "`integration_auth_failed`: Reoon refused the key"),
        (status = 502, description = "`integration_unreachable`")),
    security(("api_key" = ["integrations"]), ("session" = [])),
    description = "Scope: `integrations`. Calls Reoon now.")]
async fn get_balance(State(st): State<AppState>) -> ApiResult<ApiJson<Balance>> {
    let (id, status, client) = store::load_client(&st, "reoon").await.map_err(load_error)?;
    if status == "auth_failed" {
        return Err(ApiError::conflict("integration_auth_failed", "Reoon refused the key. Update it in Settings > Integrations."));
    }
    match client.balance().await {
        Ok(b) => Ok(ApiJson(b)),
        Err(VerifyError::Auth) => {
            store::mark_auth_failed(&st, id).await;
            Err(ApiError::conflict("integration_auth_failed", "Reoon refused the key. Update it in Settings > Integrations."))
        }
        Err(e) => {
            tracing::warn!(code = %e.code(), "Reoon balance call failed");
            Err(ApiError::new(axum::http::StatusCode::BAD_GATEWAY, "integration_unreachable", "Reoon did not answer. Try again in a minute."))
        }
    }
}

// ---------- settings ----------

#[derive(Serialize, ToSchema)]
pub struct SettingsReply {
    pub settings: VerifySettings,
    pub engine_paused: bool,
}

fn settings_error(e: SettingsError) -> ApiError {
    ApiError::unprocessable(e.code, e.message)
}

/// Read the verifier settings and the send rule.
#[utoipa::path(get, path = "/v1/verify/reoon/settings", tag = "integrations",
    responses((status = 200, body = SettingsReply)), security(("api_key" = ["integrations"]), ("session" = [])), description = "Scope: `integrations`.")]
async fn get_settings(State(st): State<AppState>) -> ApiResult<ApiJson<SettingsReply>> {
    Ok(ApiJson(SettingsReply { settings: settings::load(&st.pool).await?, engine_paused: settings::is_paused(&st.pool).await? }))
}

/// Change the settings. Send only the fields to change. `send_rule` is replaced as a whole.
#[utoipa::path(patch, path = "/v1/verify/reoon/settings", tag = "integrations",
    request_body(content = Value, description = "`mode`, `concurrency`, `reuse_days`, `tick_secs`, `send_rule: { allowed: [status], allow_not_verified }`"),
    responses((status = 200, body = SettingsReply), (status = 422, description = "`invalid_settings`, or `status_not_allowed`: a locked status (invalid, disabled, disposable, spamtrap) is in the rule")),
    security(("api_key" = ["integrations"]), ("session" = [])),
    description = "Scope: `integrations`. The rule applies to leads that start a campaign after the change. Leads already in a sequence keep going.")]
async fn patch_settings(State(st): State<AppState>, ApiJson(patch): ApiJson<Value>) -> ApiResult<ApiJson<SettingsReply>> {
    let mut tx = st.pool.begin().await?;
    let (current,): (Value,) = sqlx::query_as("SELECT settings FROM verify_defaults WHERE id = 1 FOR UPDATE").fetch_one(&mut *tx).await?;
    let merged = VerifySettings::from_stored(current).merged(&patch).map_err(settings_error)?;
    merged.validate().map_err(settings_error)?;
    sqlx::query("UPDATE verify_defaults SET settings = $1, updated_at = now() WHERE id = 1").bind(serde_json::to_value(&merged).unwrap_or_default()).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(ApiJson(SettingsReply { settings: merged, engine_paused: settings::is_paused(&st.pool).await? }))
}

#[derive(Serialize, ToSchema)]
pub struct EnginePauseReply {
    pub engine_paused: bool,
}

async fn set_paused(st: &AppState, paused: bool) -> ApiResult<ApiJson<EnginePauseReply>> {
    sqlx::query("UPDATE verify_defaults SET engine_paused = $1, updated_at = now() WHERE id = 1").bind(paused).execute(&st.pool).await?;
    Ok(ApiJson(EnginePauseReply { engine_paused: paused }))
}

/// Pause the verifier worker. Runs wait.
#[utoipa::path(post, path = "/v1/verify/reoon/pause", tag = "integrations",
    responses((status = 200, body = EnginePauseReply)), security(("api_key" = ["integrations"]), ("session" = [])), description = "Scope: `integrations`. Owner and admin sessions only.")]
async fn pause_engine(State(st): State<AppState>) -> ApiResult<ApiJson<EnginePauseReply>> {
    set_paused(&st, true).await
}

/// Resume the verifier worker.
#[utoipa::path(post, path = "/v1/verify/reoon/resume", tag = "integrations",
    responses((status = 200, body = EnginePauseReply)), security(("api_key" = ["integrations"]), ("session" = [])), description = "Scope: `integrations`. Owner and admin sessions only.")]
async fn resume_engine(State(st): State<AppState>) -> ApiResult<ApiJson<EnginePauseReply>> {
    set_paused(&st, false).await
}

// ---------- the table of leads with their latest result ----------

/// Extra filter on the latest Reoon result. The status filter is `verification_status` of the lead filter.
#[derive(Deserialize, ToSchema, IntoParams, Default, Clone, Debug)]
pub struct ResultFilter {
    /// Leads whose last check is older than this time, or that were never checked.
    pub checked_before: Option<DateTime<Utc>>,
}

/// The result counts only while the lead still has the address that was checked.
const LATEST: &str = "SELECT 1 FROM email_verifications v WHERE v.lead_id = leads.id AND v.provider = 'reoon' AND v.email = leads.email::text";

fn push_result_filter<'a>(qb: &mut QueryBuilder<'a, Postgres>, f: &'a ResultFilter) {
    if let Some(t) = f.checked_before {
        qb.push(format!(" AND (NOT EXISTS ({LATEST}) OR EXISTS ({LATEST} AND v.checked_at < ")).push_bind(t).push("))");
    }
}

#[derive(Serialize, ToSchema, Clone)]
pub struct LatestResult {
    pub status: String,
    pub mode: String,
    pub score: Option<i32>,
    pub safe_to_send: Option<bool>,
    #[schema(value_type = Object)]
    pub flags: Value,
    pub checked_at: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
pub struct VerifyLeadRow {
    pub id: Uuid,
    pub email: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub company_name: Option<String>,
    /// Null: not verified yet (or the address changed since the check).
    pub result: Option<LatestResult>,
}

#[derive(Serialize, ToSchema)]
pub struct VerifyLeadsReply {
    pub leads: Vec<VerifyLeadRow>,
    pub next_cursor: Option<Uuid>,
}

#[derive(FromRow)]
struct LeadBasics {
    id: Uuid,
    email: String,
    first_name: Option<String>,
    last_name: Option<String>,
    company_name: Option<String>,
}

#[derive(FromRow)]
struct ResultRow {
    lead_id: Uuid,
    email: String,
    status: String,
    mode: String,
    score: Option<i32>,
    safe_to_send: Option<bool>,
    flags: Value,
    checked_at: DateTime<Utc>,
}

/// The leads for the Reoon page, newest first, with the latest result of each.
#[utoipa::path(get, path = "/v1/verify/reoon/leads", tag = "integrations",
    params(LeadFilter, ResultFilter, PageQuery),
    responses((status = 200, body = VerifyLeadsReply), (status = 400, description = "`invalid_cursor`")),
    security(("api_key" = ["integrations"]), ("session" = [])),
    description = "Scope: `integrations`. The same filter as `GET /v1/leads`, plus `verification_status` and `checked_before`. `cursor` is the id of the last lead of the previous page.")]
async fn list_leads(State(st): State<AppState>, ApiQuery(filter): ApiQuery<LeadFilter>, ApiQuery(rf): ApiQuery<ResultFilter>, ApiQuery(page): ApiQuery<PageQuery>) -> ApiResult<ApiJson<VerifyLeadsReply>> {
    let limit = page.limit.unwrap_or(50);
    if !(1..=500).contains(&limit) {
        return Err(ApiError::unprocessable("invalid_limit", "limit must be 1 to 500."));
    }
    let mut conn = st.pool.acquire().await?;
    let after = match page.cursor {
        None => None,
        Some(id) => Some(cursor_position(&mut conn, id).await?.ok_or_else(|| ApiError::bad_request("invalid_cursor", "The cursor lead does not exist any more. Start again without a cursor."))?),
    };
    let mut qb = QueryBuilder::new("SELECT id, email::text AS email, first_name, last_name, company_name FROM leads");
    push_where(&mut qb, &filter);
    push_result_filter(&mut qb, &rf);
    if let Some((at, id)) = after {
        qb.push(" AND (created_at, id) < (").push_bind(at).push(", ").push_bind(id).push(")");
    }
    qb.push(" ORDER BY created_at DESC, id DESC LIMIT ").push_bind(limit + 1);
    let mut leads: Vec<LeadBasics> = qb.build_query_as().fetch_all(&mut *conn).await?;
    let next_cursor = if leads.len() as i64 > limit {
        leads.truncate(limit as usize);
        leads.last().map(|l| l.id)
    } else {
        None
    };
    let ids: Vec<Uuid> = leads.iter().map(|l| l.id).collect();
    let results: Vec<ResultRow> =
        sqlx::query_as("SELECT lead_id, email, status, mode, score, safe_to_send, flags, checked_at FROM email_verifications WHERE provider = 'reoon' AND lead_id = ANY($1)")
            .bind(&ids)
            .fetch_all(&mut *conn)
            .await?;
    let mut by_lead: HashMap<Uuid, ResultRow> = results.into_iter().map(|r| (r.lead_id, r)).collect();
    let leads = leads
        .into_iter()
        .map(|l| {
            let result = by_lead.remove(&l.id).filter(|r| r.email == l.email).map(|r| LatestResult {
                status: r.status,
                mode: r.mode,
                score: r.score,
                safe_to_send: r.safe_to_send,
                flags: r.flags,
                checked_at: r.checked_at,
            });
            VerifyLeadRow { id: l.id, email: l.email, first_name: l.first_name, last_name: l.last_name, company_name: l.company_name, result }
        })
        .collect();
    Ok(ApiJson(VerifyLeadsReply { leads, next_cursor }))
}

// ---------- runs ----------

const MAX_RUN: i64 = 10_000;

#[derive(Serialize, FromRow, ToSchema)]
pub struct Run {
    pub id: Uuid,
    pub provider: String,
    pub mode: String,
    /// `queued`, `running`, `done`, `cancelled`, `failed`
    pub status: String,
    pub force: bool,
    /// Made by the server for new leads (verify on import), not by a person.
    pub auto: bool,
    pub total: i32,
    pub done_count: i32,
    pub failed_count: i32,
    pub skipped_count: i32,
    pub credits_used: i32,
    /// For example a low balance at the start.
    pub warning: Option<String>,
    /// `out_of_credits` or `auth_failed` when the run stopped. Use resume after you fix it.
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

const RUN_COLS: &str = "id, provider, mode, status, force, auto, total, done_count, failed_count, skipped_count, credits_used, warning, error, created_at, finished_at";

#[derive(Deserialize, ToSchema)]
pub struct NewRun {
    /// Up to 10000 lead ids. Send this or `filter`.
    pub lead_ids: Option<Vec<Uuid>>,
    /// All leads that match (up to 10000).
    pub filter: Option<LeadFilter>,
    #[serde(flatten)]
    pub result_filter: ResultFilter,
    /// Default: the mode in the settings.
    pub mode: Option<Mode>,
    /// `true`: check again even when a recent result exists.
    #[serde(default)]
    pub force: bool,
    /// A second call with the same key returns the first run.
    pub idempotency_key: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct NewRunReply {
    pub run: Run,
    /// Emails that will cost a credit: the run total minus the results that are new enough to reuse (none when `force`).
    pub estimated_credits: i64,
}

async fn active_or_error(st: &AppState) -> ApiResult<()> {
    let (_, status, _) = store::load_client(st, "reoon").await.map_err(load_error)?;
    if status != "installed" {
        return Err(ApiError::conflict("integration_auth_failed", "Reoon refused the key. Update it in Settings > Integrations."));
    }
    Ok(())
}

async fn estimate(st: &AppState, run_id: Uuid, force: bool, reuse_days: i32) -> Result<i64, sqlx::Error> {
    let (total, recent): (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE NOT $2 AND $3 > 0 AND EXISTS (
             SELECT 1 FROM email_verifications v WHERE v.lead_id = i.lead_id AND v.provider = 'reoon' AND v.email = i.email AND v.checked_at > $4))
         FROM verification_items i WHERE i.run_id = $1",
    )
    .bind(run_id)
    .bind(force)
    .bind(reuse_days)
    .bind(st.now() - chrono::Duration::days(reuse_days as i64))
    .fetch_one(&st.pool)
    .await?;
    Ok(total - recent)
}

/// Make a run: one item for each lead. The worker calls Reoon one email at a time.
#[utoipa::path(post, path = "/v1/verify/reoon/runs", tag = "integrations",
    request_body = NewRun,
    responses((status = 201, body = NewRunReply), (status = 200, description = "The `idempotency_key` was used before: the first run"), (status = 404, description = "`not_installed`"),
        (status = 409, description = "`integration_auth_failed`"), (status = 422, description = "`too_many_leads` (over 10000), `no_leads`, or `invalid_run`")),
    security(("api_key" = ["integrations"]), ("session" = [])),
    description = "Scope: `integrations`. Results newer than `reuse_days` are skipped at no cost unless `force` is true.")]
async fn create_run(State(st): State<AppState>, session: Option<Extension<SessionCtx>>, ApiJson(body): ApiJson<NewRun>) -> ApiResult<(axum::http::StatusCode, ApiJson<NewRunReply>)> {
    active_or_error(&st).await?;
    let cfg = settings::load(&st.pool).await?;
    if let Some(k) = body.idempotency_key.as_deref() {
        if k.is_empty() || k.len() > 200 {
            return Err(ApiError::unprocessable("invalid_run", "idempotency_key must be 1 to 200 characters."));
        }
        let old: Option<Run> = sqlx::query_as(&format!("SELECT {RUN_COLS} FROM verification_runs WHERE idempotency_key = $1")).bind(k).fetch_optional(&st.pool).await?;
        if let Some(run) = old {
            let estimated_credits = estimate(&st, run.id, run.force, cfg.reuse_days).await?;
            return Ok((axum::http::StatusCode::OK, ApiJson(NewRunReply { run, estimated_credits })));
        }
    }
    let mut conn = st.pool.acquire().await?;
    let ids: Vec<Uuid> = match (&body.lead_ids, &body.filter) {
        (Some(ids), None) => ids.clone(),
        (None, Some(filter)) => {
            let mut qb = QueryBuilder::new("SELECT id FROM leads");
            push_where(&mut qb, filter);
            push_result_filter(&mut qb, &body.result_filter);
            qb.push(" ORDER BY created_at DESC, id DESC LIMIT ").push_bind(MAX_RUN + 1);
            let rows: Vec<(Uuid,)> = qb.build_query_as().fetch_all(&mut *conn).await?;
            rows.into_iter().map(|r| r.0).collect()
        }
        _ => return Err(ApiError::unprocessable("invalid_run", "Send lead_ids or filter, not both and not neither.")),
    };
    drop(conn);
    if ids.len() as i64 > MAX_RUN {
        return Err(ApiError::unprocessable("too_many_leads", "A run can have up to 10000 leads."));
    }
    let run_id = Uuid::now_v7();
    let mode = body.mode.unwrap_or(cfg.mode);
    let mut tx = st.pool.begin().await?;
    sqlx::query("INSERT INTO verification_runs (id, mode, force, idempotency_key, created_by, created_at) VALUES ($1, $2, $3, $4, $5, $6)")
        .bind(run_id)
        .bind(mode.as_str())
        .bind(body.force)
        .bind(&body.idempotency_key)
        .bind(session.map(|Extension(s)| s.user_id))
        .bind(st.now())
        .execute(&mut *tx)
        .await?;
    let made = sqlx::query("INSERT INTO verification_items (run_id, lead_id, email) SELECT $1, id, email::text FROM leads WHERE id = ANY($2) ON CONFLICT DO NOTHING")
        .bind(run_id)
        .bind(&ids)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if made == 0 {
        return Err(ApiError::unprocessable("no_leads", "No lead to check."));
    }
    sqlx::query("UPDATE verification_runs SET total = $2 WHERE id = $1").bind(run_id).bind(made as i32).execute(&mut *tx).await?;
    tx.commit().await?;
    let run: Run = sqlx::query_as(&format!("SELECT {RUN_COLS} FROM verification_runs WHERE id = $1")).bind(run_id).fetch_one(&st.pool).await?;
    let estimated_credits = estimate(&st, run_id, body.force, cfg.reuse_days).await?;
    Ok((axum::http::StatusCode::CREATED, ApiJson(NewRunReply { run, estimated_credits })))
}

#[derive(Serialize, ToSchema)]
pub struct RunsReply {
    pub runs: Vec<Run>,
}

/// The last 50 runs, newest first.
#[utoipa::path(get, path = "/v1/verify/reoon/runs", tag = "integrations",
    responses((status = 200, body = RunsReply)), security(("api_key" = ["integrations"]), ("session" = [])), description = "Scope: `integrations`.")]
async fn list_runs(State(st): State<AppState>) -> ApiResult<ApiJson<RunsReply>> {
    let runs = sqlx::query_as(&format!("SELECT {RUN_COLS} FROM verification_runs ORDER BY created_at DESC, id DESC LIMIT 50")).fetch_all(&st.pool).await?;
    Ok(ApiJson(RunsReply { runs }))
}

async fn load_run(st: &AppState, id: Uuid) -> ApiResult<Run> {
    sqlx::query_as(&format!("SELECT {RUN_COLS} FROM verification_runs WHERE id = $1")).bind(id).fetch_optional(&st.pool).await?.ok_or_else(|| ApiError::not_found("run_not_found", "No such run."))
}

/// One run, with live counters.
#[utoipa::path(get, path = "/v1/verify/reoon/runs/{id}", tag = "integrations",
    params(("id" = Uuid, Path)), responses((status = 200, body = Run), (status = 404, description = "`run_not_found`")),
    security(("api_key" = ["integrations"]), ("session" = [])), description = "Scope: `integrations`.")]
async fn get_run(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<Run>> {
    Ok(ApiJson(load_run(&st, id).await?))
}

/// Cancel a run. Items not yet checked are skipped. Results already saved stay.
#[utoipa::path(post, path = "/v1/verify/reoon/runs/{id}/cancel", tag = "integrations",
    params(("id" = Uuid, Path)), responses((status = 200, body = Run), (status = 404, description = "`run_not_found`"), (status = 409, description = "`run_finished`")),
    security(("api_key" = ["integrations"]), ("session" = [])), description = "Scope: `integrations`.")]
async fn cancel_run(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<Run>> {
    let run = load_run(&st, id).await?;
    if matches!(run.status.as_str(), "done" | "cancelled") {
        return Err(ApiError::conflict("run_finished", "This run is already finished."));
    }
    let mut tx = st.pool.begin().await?;
    let skipped = sqlx::query("UPDATE verification_items SET state = 'skipped', claim_id = NULL WHERE run_id = $1 AND state IN ('pending', 'claimed')").bind(id).execute(&mut *tx).await?.rows_affected();
    sqlx::query("UPDATE verification_runs SET status = 'cancelled', skipped_count = skipped_count + $2, finished_at = $3 WHERE id = $1").bind(id).bind(skipped as i32).bind(st.now()).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(ApiJson(load_run(&st, id).await?))
}

/// Resume a run that stopped with `out_of_credits` or `auth_failed`.
#[utoipa::path(post, path = "/v1/verify/reoon/runs/{id}/resume", tag = "integrations",
    params(("id" = Uuid, Path)), responses((status = 200, body = Run), (status = 404, description = "`run_not_found`"), (status = 409, description = "`run_not_stopped` or `integration_auth_failed`")),
    security(("api_key" = ["integrations"]), ("session" = [])),
    description = "Scope: `integrations`. Items that were not checked are still `pending`, so the run goes on where it stopped. Update the key first after `auth_failed`.")]
async fn resume_run(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<Run>> {
    let run = load_run(&st, id).await?;
    if run.status != "failed" {
        return Err(ApiError::conflict("run_not_stopped", "Only a stopped run can be resumed."));
    }
    active_or_error(&st).await?;
    sqlx::query("UPDATE verification_runs SET status = 'running', error = NULL, finished_at = NULL WHERE id = $1").bind(id).execute(&st.pool).await?;
    Ok(ApiJson(load_run(&st, id).await?))
}

/// The latest Reoon result of one lead, with the flags. Null when there is none.
#[utoipa::path(get, path = "/v1/verify/reoon/leads/{id}", tag = "integrations",
    params(("id" = Uuid, Path)), responses((status = 200, body = Option<LatestResult>)),
    security(("api_key" = ["integrations"]), ("session" = [])), description = "Scope: `integrations`. For the lead drawer.")]
async fn get_lead_result(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<Option<LatestResult>>> {
    let row: Option<ResultRow> = sqlx::query_as(
        "SELECT v.lead_id, v.email, v.status, v.mode, v.score, v.safe_to_send, v.flags, v.checked_at FROM email_verifications v JOIN leads l ON l.id = v.lead_id
          WHERE v.lead_id = $1 AND v.provider = 'reoon' AND v.email = l.email::text",
    )
    .bind(id)
    .fetch_optional(&st.pool)
    .await?;
    Ok(ApiJson(row.map(|r| LatestResult { status: r.status, mode: r.mode, score: r.score, safe_to_send: r.safe_to_send, flags: r.flags, checked_at: r.checked_at })))
}

#[derive(Serialize, ToSchema)]
pub struct VerifierStatus {
    pub installed: bool,
    pub paused: bool,
    /// `false`: this server copy does not run the verify worker (`AIKIDO_LEADS_ENABLED`).
    pub worker_enabled: bool,
    pub last_tick_at: Option<DateTime<Utc>>,
    pub open_runs: i64,
    /// Emails waiting in open runs.
    pub pending_items: i64,
    /// `installed`, `auth_failed`, or null when not installed.
    pub integration_status: Option<String>,
}

/// The verifier worker: paused or not, last round, open runs.
#[utoipa::path(get, path = "/v1/verify/reoon/status", tag = "integrations",
    responses((status = 200, body = VerifierStatus)), security(("api_key" = ["integrations"]), ("session" = [])), description = "Scope: `integrations`.")]
async fn get_status(State(st): State<AppState>) -> ApiResult<ApiJson<VerifierStatus>> {
    let (paused, last_tick_at): (bool, Option<DateTime<Utc>>) = sqlx::query_as("SELECT engine_paused, last_tick_at FROM verify_defaults WHERE id = 1").fetch_one(&st.pool).await?;
    let (open_runs,): (i64,) = sqlx::query_as("SELECT count(*) FROM verification_runs WHERE status IN ('queued', 'running')").fetch_one(&st.pool).await?;
    let (pending_items,): (i64,) = sqlx::query_as("SELECT count(*) FROM verification_items i JOIN verification_runs r ON r.id = i.run_id WHERE r.status IN ('queued', 'running') AND i.state IN ('pending', 'claimed')")
        .fetch_one(&st.pool)
        .await?;
    let integration_status = store::get(&st.pool, "reoon").await?.map(|r| r.status);
    Ok(ApiJson(VerifierStatus { installed: integration_status.is_some(), paused, worker_enabled: st.config.leads_enabled, last_tick_at, open_runs, pending_items, integration_status }))
}
