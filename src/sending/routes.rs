//! `/v1/sending/*`
use super::queue::{self, EnqueueItem, EventRow, Job, JobChangeError, JobFilter, JobStatus, NewJob};
use super::rules::{self, Counts, Decision, SendState, WaitReason};
use super::settings::{SendingSettings, SettingsError};
use crate::accounts::model::AccountStatus;
use crate::auth::api_keys::Scope;
use crate::auth::middleware::require_scope;
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::state::AppState;
use axum::extract::State;
use chrono::{DateTime, Utc};
use rand::SeedableRng;
use rand::rngs::StdRng;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Routes that need a key with scope `sending`.
pub fn router(state: AppState) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_defaults, patch_defaults))
        .routes(routes!(get_account_settings, patch_account_settings))
        .routes(routes!(reset_account_settings))
        .routes(routes!(account_status))
        .routes(routes!(pause_account))
        .routes(routes!(resume_account))
        .routes(routes!(preview))
        .routes(routes!(create_jobs, list_jobs))
        .routes(routes!(get_job, cancel_job))
        .routes(routes!(requeue_job))
        .routes(routes!(list_suppressions, add_suppression, remove_suppression))
        .routes(routes!(pause_engine))
        .routes(routes!(resume_engine))
        .routes(routes!(engine_status))
        .route_layer(axum::middleware::from_fn_with_state((state, Scope::Sending), require_scope))
}

fn settings_error(e: SettingsError) -> ApiError {
    ApiError::unprocessable(e.code, e.message)
}

// ---------- settings ----------

#[derive(Serialize, ToSchema)]
pub struct SettingsReply {
    pub settings: SendingSettings,
    /// Not errors: things to look at.
    pub warnings: Vec<String>,
}

fn settings_reply(settings: SendingSettings) -> SettingsReply {
    let warnings = settings.validate().unwrap_or_default();
    SettingsReply { settings, warnings }
}

/// Read the defaults that new accounts copy.
#[utoipa::path(get, path = "/v1/sending/defaults", tag = "sending",
    responses((status = 200, body = SettingsReply)),
    security(("api_key" = ["sending"])), description = "Scope: `sending`.")]
async fn get_defaults(State(st): State<AppState>) -> ApiResult<ApiJson<SettingsReply>> {
    let mut conn = st.pool.acquire().await?;
    Ok(ApiJson(settings_reply(queue::load_defaults(&mut conn).await?)))
}

/// Change the defaults for new accounts. Send only the fields to change.
#[utoipa::path(patch, path = "/v1/sending/defaults", tag = "sending",
    request_body(content = SendingSettings, description = "Only the fields to change."),
    responses((status = 200, body = SettingsReply), (status = 422, description = "A setting is not valid")),
    security(("api_key" = ["sending"])),
    description = "Scope: `sending`. Merges the fields, validates the full result, then saves. Existing accounts keep their own settings.")]
async fn patch_defaults(State(st): State<AppState>, ApiJson(patch): ApiJson<Value>) -> ApiResult<ApiJson<SettingsReply>> {
    let mut tx = st.pool.begin().await?;
    let (current,): (Value,) = sqlx::query_as("SELECT settings FROM sending_defaults WHERE id = 1 FOR UPDATE")
        .fetch_one(&mut *tx)
        .await?;
    let merged = SendingSettings::from_stored(current).merged(&patch).map_err(settings_error)?;
    let warnings = merged.validate().map_err(settings_error)?;
    sqlx::query("UPDATE sending_defaults SET settings = $1, updated_at = now() WHERE id = 1")
        .bind(serde_json::to_value(&merged).unwrap_or_default())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(ApiJson(SettingsReply { settings: merged, warnings }))
}

async fn require_account(st: &AppState, id: Uuid) -> ApiResult<crate::accounts::model::EmailAccount> {
    crate::accounts::repo::get(&st.pool, id)
        .await?
        .ok_or_else(|| ApiError::not_found("account_not_found", "No account with this id."))
}

/// Read the sending settings of one account.
#[utoipa::path(get, path = "/v1/sending/accounts/{id}/settings", tag = "sending",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = SettingsReply)),
    security(("api_key" = ["sending"])), description = "Scope: `sending`.")]
async fn get_account_settings(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<SettingsReply>> {
    require_account(&st, id).await?;
    let mut conn = st.pool.acquire().await?;
    Ok(ApiJson(settings_reply(queue::load_settings(&mut conn, id).await?)))
}

/// After a settings change: the gap is measured again from the last send,
/// and today's cap is picked again, so the change works at once.
async fn refresh_state_after_settings(
    conn: &mut sqlx::PgConnection,
    account_id: Uuid,
    s: &SendingSettings,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE account_send_state SET next_send_at = CASE
             WHEN last_sent_at IS NOT NULL AND last_sent_at + make_interval(secs => $2) > $3
             THEN last_sent_at + make_interval(secs => $2) ELSE NULL END
         WHERE account_id = $1",
    )
    .bind(account_id)
    .bind(s.base_delay_secs as f64)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    let state: Option<queue::StateRow> =
        sqlx::query_as(&format!("SELECT {} FROM account_send_state WHERE account_id = $1", queue::STATE_COLS))
            .bind(account_id)
            .fetch_optional(&mut *conn)
            .await?;
    if let Some(state) = state {
        if state.local_day == Some(rules::local_date(s, now)) {
            let limit = rules::pick_today_limit(s, state.warmup_day, &mut StdRng::from_entropy()) as i32;
            sqlx::query("UPDATE account_send_state SET today_limit = $2 WHERE account_id = $1")
                .bind(account_id)
                .bind(limit)
                .execute(&mut *conn)
                .await?;
        }
    }
    Ok(())
}

/// Change the sending settings of one account. Send only the fields to change.
#[utoipa::path(patch, path = "/v1/sending/accounts/{id}/settings", tag = "sending",
    params(("id" = Uuid, Path)),
    request_body(content = SendingSettings, description = "Only the fields to change."),
    responses((status = 200, body = SettingsReply), (status = 422, description = "A setting is not valid")),
    security(("api_key" = ["sending"])),
    description = "Scope: `sending`. Merges the fields, validates the full result, then saves.")]
async fn patch_account_settings(
    State(st): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(patch): ApiJson<Value>,
) -> ApiResult<ApiJson<SettingsReply>> {
    require_account(&st, id).await?;
    let mut tx = st.pool.begin().await?;
    let current = queue::load_settings(&mut tx, id).await?;
    let merged = current.merged(&patch).map_err(settings_error)?;
    let warnings = merged.validate().map_err(settings_error)?;
    sqlx::query(
        "INSERT INTO account_sending_settings (account_id, settings) VALUES ($1, $2)
         ON CONFLICT (account_id) DO UPDATE SET settings = EXCLUDED.settings, updated_at = now()",
    )
    .bind(id)
    .bind(serde_json::to_value(&merged).unwrap_or_default())
    .execute(&mut *tx)
    .await?;
    refresh_state_after_settings(&mut tx, id, &merged, st.now()).await?;
    tx.commit().await?;
    Ok(ApiJson(SettingsReply { settings: merged, warnings }))
}

/// Copy the defaults onto this account.
#[utoipa::path(post, path = "/v1/sending/accounts/{id}/settings/reset", tag = "sending",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = SettingsReply)),
    security(("api_key" = ["sending"])), description = "Scope: `sending`.")]
async fn reset_account_settings(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<SettingsReply>> {
    require_account(&st, id).await?;
    let mut tx = st.pool.begin().await?;
    let defaults = queue::load_defaults(&mut tx).await?;
    sqlx::query(
        "INSERT INTO account_sending_settings (account_id, settings) VALUES ($1, $2)
         ON CONFLICT (account_id) DO UPDATE SET settings = EXCLUDED.settings, updated_at = now()",
    )
    .bind(id)
    .bind(serde_json::to_value(&defaults).unwrap_or_default())
    .execute(&mut *tx)
    .await?;
    refresh_state_after_settings(&mut tx, id, &defaults, st.now()).await?;
    tx.commit().await?;
    Ok(ApiJson(settings_reply(defaults)))
}

// ---------- status, pause, resume, preview ----------

#[derive(Serialize, ToSchema)]
pub struct WaitInfo {
    pub reason: WaitReason,
    pub until: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
pub struct StatusReply {
    pub account_id: Uuid,
    pub email: String,
    pub account_status: AccountStatus,
    pub paused: bool,
    pub paused_reason: Option<String>,
    pub paused_at: Option<DateTime<Utc>>,
    pub timezone: String,
    /// Account local time, like `2026-10-19 09:12:03 +06`.
    pub local_time: String,
    pub inside_window: bool,
    pub sent_today: i32,
    pub today_limit: i32,
    /// Warm-up is on in the settings of this account.
    pub warmup_enabled: bool,
    /// Days of warm-up with sends so far. 0 on the first day.
    pub warmup_day: i32,
    pub consecutive_failures: i32,
    pub next_send_at: Option<DateTime<Utc>>,
    pub last_sent_at: Option<DateTime<Utc>>,
    pub queued_jobs: i64,
    pub sending_jobs: i64,
    /// `null` means the rules allow a send right now. Otherwise: why not, and until when.
    pub wait: Option<WaitInfo>,
}

/// The state of an account as the rules see it today.
async fn rules_view(
    st: &AppState,
    id: Uuid,
    now: DateTime<Utc>,
) -> ApiResult<(SendingSettings, queue::StateRow, SendState, Vec<DateTime<Utc>>)> {
    let mut conn = st.pool.acquire().await?;
    let settings = queue::load_settings(&mut conn, id).await?;
    let row: queue::StateRow =
        sqlx::query_as(&format!("SELECT {} FROM account_send_state WHERE account_id = $1", queue::STATE_COLS))
            .bind(id)
            .fetch_one(&mut *conn)
            .await?;
    let recent = queue::recent_sends(&mut conn, id, now).await?;
    let today = rules::local_date(&settings, now);
    let mut state = row.rules_state();
    if state.local_day != Some(today) {
        // A new day has not been rolled over yet: show it as it will start.
        state.sent_today = 0;
        state.today_limit = Some(rules::base_cap(&settings, state.warmup_day) as i32);
        state.local_day = Some(today);
    }
    Ok((settings, row, state, recent))
}

/// Sent today, today's cap, next send time, inside window or not, pause reason, and why it waits.
#[utoipa::path(get, path = "/v1/sending/accounts/{id}/status", tag = "sending",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = StatusReply)),
    security(("api_key" = ["sending"])), description = "Scope: `sending`.")]
async fn account_status(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<StatusReply>> {
    let account = require_account(&st, id).await?;
    let now = st.now();
    let (settings, row, state, recent) = rules_view(&st, id, now).await?;
    let decision = rules::can_send(&settings, &state, &Counts { recent_sends: recent }, now);
    let (queued, sending): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE status = 'queued'), count(*) FILTER (WHERE status = 'sending')
         FROM send_jobs WHERE account_id = $1 AND status IN ('queued', 'sending')",
    )
    .bind(id)
    .fetch_one(&st.pool)
    .await?;
    Ok(ApiJson(StatusReply {
        account_id: id,
        email: account.email.clone(),
        account_status: account.status,
        paused: row.paused,
        paused_reason: row.paused_reason.clone(),
        paused_at: row.paused_at,
        timezone: settings.timezone.clone(),
        local_time: now.with_timezone(&settings.tz()).format("%Y-%m-%d %H:%M:%S %Z").to_string(),
        inside_window: rules::inside_window(&settings, now),
        sent_today: state.sent_today,
        today_limit: state.today_limit.unwrap_or(settings.daily_limit as i32),
        warmup_enabled: settings.warmup_enabled,
        warmup_day: state.warmup_day,
        consecutive_failures: row.consecutive_failures,
        next_send_at: row.next_send_at,
        last_sent_at: row.last_sent_at,
        queued_jobs: queued,
        sending_jobs: sending,
        wait: match decision {
            Decision::Wait { reason, until } => Some(WaitInfo { reason, until }),
            Decision::Ok => follow_up_gate_wait(&st, &account, &settings, now).await?,
        },
    }))
}

/// The rules allow a send, but the only emails that wait are follow-ups and the inbox was not read lately.
async fn follow_up_gate_wait(
    st: &AppState,
    account: &crate::accounts::model::EmailAccount,
    settings: &SendingSettings,
    now: DateTime<Utc>,
) -> ApiResult<Option<WaitInfo>> {
    let mut conn = st.pool.acquire().await?;
    let Some(block) = queue::reply_check_block(&mut conn, account, settings, now).await? else { return Ok(None) };
    // Due jobs that the gate does not hold back: first emails, and manual jobs.
    let (open, held): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE NOT (kind = 'automated' AND in_reply_to IS NOT NULL) AND (send_after IS NULL OR send_after <= $2)),
                count(*) FILTER (WHERE kind = 'automated' AND in_reply_to IS NOT NULL)
         FROM send_jobs WHERE account_id = $1 AND status = 'queued'",
    )
    .bind(account.id)
    .bind(now)
    .fetch_one(&mut *conn)
    .await?;
    if open > 0 || held == 0 {
        return Ok(None);
    }
    Ok(Some(WaitInfo { reason: block.wait_reason(), until: now + chrono::Duration::minutes(5) }))
}

#[derive(Deserialize, ToSchema, Default)]
pub struct PauseBody {
    pub reason: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct PauseReply {
    pub paused: bool,
}

/// Pause one account.
#[utoipa::path(post, path = "/v1/sending/accounts/{id}/pause", tag = "sending",
    params(("id" = Uuid, Path)),
    request_body(content = PauseBody, description = "Optional reason."),
    responses((status = 200, body = PauseReply)),
    security(("api_key" = ["sending"])), description = "Scope: `sending`.")]
async fn pause_account(
    State(st): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
    body: axum::body::Bytes,
) -> ApiResult<ApiJson<PauseReply>> {
    require_account(&st, id).await?;
    let reason = crate::error::parse_optional_json::<PauseBody>(&body)?.reason.filter(|r| !r.trim().is_empty()).unwrap_or_else(|| "paused by user".into());
    let now = st.now();
    let mut tx = st.pool.begin().await?;
    queue::pause_state(&mut tx, id, &reason, now).await?;
    sqlx::query("UPDATE email_accounts SET status = 'paused', updated_at = $2 WHERE id = $1 AND status = 'active'")
        .bind(id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    queue::log_event(&mut tx, None, id, "paused", json!({ "reason": reason, "by": "user" }), now).await?;
    tx.commit().await?;
    Ok(ApiJson(PauseReply { paused: true }))
}

/// Resume one account. Resets `consecutive_failures`.
#[utoipa::path(post, path = "/v1/sending/accounts/{id}/resume", tag = "sending",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = PauseReply), (status = 409, description = "The account is disconnected. Connect it again.")),
    security(("api_key" = ["sending"])), description = "Scope: `sending`.")]
async fn resume_account(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<PauseReply>> {
    let account = require_account(&st, id).await?;
    if account.status == AccountStatus::Disconnected {
        return Err(ApiError::conflict(
            "account_disconnected",
            "The login of this account is no longer valid. Connect the account again.",
        ));
    }
    let now = st.now();
    let mut tx = st.pool.begin().await?;
    sqlx::query(
        "UPDATE account_send_state SET paused = false, paused_reason = NULL, paused_at = NULL, consecutive_failures = 0
         WHERE account_id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE email_accounts SET status = 'active', last_error = NULL, updated_at = $2
         WHERE id = $1 AND status IN ('paused', 'error')",
    )
    .bind(id)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    queue::log_event(&mut tx, None, id, "resumed", json!({ "by": "user" }), now).await?;
    tx.commit().await?;
    Ok(ApiJson(PauseReply { paused: false }))
}

#[derive(Deserialize, IntoParams)]
pub struct PreviewQuery {
    /// How many send times to simulate. 1 to 200. Default 20.
    count: Option<usize>,
}

#[derive(Serialize, ToSchema)]
pub struct PreviewTime {
    pub at: DateTime<Utc>,
    /// The same moment in the account time zone.
    pub local: String,
}

#[derive(Serialize, ToSchema)]
pub struct PreviewReply {
    pub timezone: String,
    pub times: Vec<PreviewTime>,
}

/// Simulates the next send times with the current rules. Sends nothing.
#[utoipa::path(get, path = "/v1/sending/accounts/{id}/preview", tag = "sending",
    params(("id" = Uuid, Path), PreviewQuery),
    responses((status = 200, body = PreviewReply)),
    security(("api_key" = ["sending"])), description = "Scope: `sending`. Sends nothing.")]
async fn preview(
    State(st): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(q): ApiQuery<PreviewQuery>,
) -> ApiResult<ApiJson<PreviewReply>> {
    require_account(&st, id).await?;
    let count = q.count.unwrap_or(20);
    if !(1..=200).contains(&count) {
        return Err(ApiError::unprocessable("invalid_count", "count must be 1 to 200."));
    }
    let now = st.now();
    let (settings, _row, state, recent) = rules_view(&st, id, now).await?;
    let times = rules::simulate(&settings, &state, &recent, now, count, &mut StdRng::from_entropy());
    let tz = settings.tz();
    Ok(ApiJson(PreviewReply {
        timezone: settings.timezone.clone(),
        times: times
            .into_iter()
            .map(|at| PreviewTime { at, local: at.with_timezone(&tz).format("%Y-%m-%d %H:%M:%S %Z").to_string() })
            .collect(),
    }))
}

// ---------- jobs ----------

#[derive(Deserialize, ToSchema)]
pub struct CreateJobs {
    /// 1 to 1000 emails.
    pub jobs: Vec<NewJob>,
}

#[derive(Serialize, ToSchema)]
pub struct CreateJobsReply {
    pub items: Vec<EnqueueItem>,
    pub queued: usize,
    pub duplicates: usize,
    pub errors: usize,
}

/// Queue 1 to 1000 emails. Bad items get an error each. Good items are still queued.
#[utoipa::path(post, path = "/v1/sending/jobs", tag = "sending",
    request_body = CreateJobs,
    responses((status = 200, body = CreateJobsReply)),
    security(("api_key" = ["sending"])),
    description = "Scope: `sending`. A repeated `idempotency_key` returns the old job, not a new one.")]
async fn create_jobs(State(st): State<AppState>, ApiJson(body): ApiJson<CreateJobs>) -> ApiResult<ApiJson<CreateJobsReply>> {
    if body.jobs.is_empty() || body.jobs.len() > 1000 {
        return Err(ApiError::unprocessable("invalid_batch_size", "Send 1 to 1000 jobs in one call."));
    }
    let items = queue::enqueue(&st.pool, body.jobs, st.now()).await?;
    let count = |s: &str| items.iter().filter(|i| serde_json::to_value(&i.status).ok().and_then(|v| v.as_str().map(|x| x == s)).unwrap_or(false)).count();
    Ok(ApiJson(CreateJobsReply { queued: count("queued"), duplicates: count("duplicate"), errors: count("error"), items }))
}

#[derive(Deserialize, IntoParams)]
pub struct ListJobsQuery {
    status: Option<JobStatus>,
    account_id: Option<Uuid>,
    campaign_id: Option<Uuid>,
    lead_id: Option<Uuid>,
    /// `next_cursor` from the last page.
    cursor: Option<Uuid>,
    /// 1 to 500. Default 50.
    limit: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct ListJobsReply {
    pub jobs: Vec<Job>,
    pub next_cursor: Option<Uuid>,
}

/// List jobs, newest first. Cursor paging.
#[utoipa::path(get, path = "/v1/sending/jobs", tag = "sending",
    params(ListJobsQuery),
    responses((status = 200, body = ListJobsReply)),
    security(("api_key" = ["sending"])), description = "Scope: `sending`.")]
async fn list_jobs(State(st): State<AppState>, ApiQuery(q): ApiQuery<ListJobsQuery>) -> ApiResult<ApiJson<ListJobsReply>> {
    let limit = q.limit.unwrap_or(50);
    if !(1..=500).contains(&limit) {
        return Err(ApiError::unprocessable("invalid_limit", "limit must be 1 to 500."));
    }
    let filter = JobFilter {
        status: q.status,
        account_id: q.account_id,
        campaign_id: q.campaign_id,
        lead_id: q.lead_id,
        cursor: q.cursor,
        limit,
    };
    let (jobs, next_cursor) = queue::list_jobs(&st.pool, &filter).await?;
    Ok(ApiJson(ListJobsReply { jobs, next_cursor }))
}

#[derive(Serialize, ToSchema)]
pub struct JobDetail {
    pub job: Job,
    pub events: Vec<EventRow>,
}

/// One job with its events.
#[utoipa::path(get, path = "/v1/sending/jobs/{id}", tag = "sending",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = JobDetail)),
    security(("api_key" = ["sending"])), description = "Scope: `sending`.")]
async fn get_job(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<JobDetail>> {
    let job = queue::get_job(&st.pool, id)
        .await?
        .ok_or_else(|| ApiError::not_found("job_not_found", "No job with this id."))?;
    let events = queue::job_events(&st.pool, id).await?;
    Ok(ApiJson(JobDetail { job, events }))
}

fn job_change_error(e: JobChangeError, wanted: &str) -> ApiError {
    match e {
        JobChangeError::NotFound => ApiError::not_found("job_not_found", "No job with this id."),
        JobChangeError::WrongStatus(s) => ApiError::conflict(
            "wrong_job_status",
            format!("The job is '{}'. {wanted}", serde_json::to_value(s).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()),
        ),
        JobChangeError::Db(e) => e.into(),
    }
}

/// Cancel a job. Works only while it is `queued`.
#[utoipa::path(delete, path = "/v1/sending/jobs/{id}", tag = "sending",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = Job), (status = 409, description = "The job is not queued")),
    security(("api_key" = ["sending"])), description = "Scope: `sending`.")]
async fn cancel_job(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<Job>> {
    queue::cancel(&st.pool, id, st.now())
        .await
        .map(ApiJson)
        .map_err(|e| job_change_error(e, "Only a queued job can be cancelled."))
}

/// Put a `stuck` or `failed` job back in the queue, on purpose.
#[utoipa::path(post, path = "/v1/sending/jobs/{id}/requeue", tag = "sending",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = Job), (status = 409, description = "The job is not stuck or failed")),
    security(("api_key" = ["sending"])),
    description = "Scope: `sending`. A stuck job may already have been delivered. Check before you requeue it.")]
async fn requeue_job(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<Job>> {
    queue::requeue(&st.pool, id, st.now())
        .await
        .map(ApiJson)
        .map_err(|e| job_change_error(e, "Only a stuck or failed job can be requeued."))
}

// ---------- suppressions ----------

#[derive(sqlx::Type, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, ToSchema)]
#[sqlx(type_name = "suppression_kind", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum SuppressionKind {
    Email,
    Domain,
}

#[derive(sqlx::Type, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, ToSchema)]
#[sqlx(type_name = "suppression_reason", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum SuppressionReason {
    Unsubscribed,
    HardBounce,
    Replied,
    Complained,
    Manual,
    /// The lead was deleted on request (lead management). The address is never contacted again.
    Erased,
}

#[derive(sqlx::FromRow, Serialize, ToSchema)]
pub struct Suppression {
    pub value: String,
    pub kind: SuppressionKind,
    pub reason: SuppressionReason,
    pub created_at: DateTime<Utc>,
}

#[derive(Deserialize, IntoParams)]
pub struct ListSuppressionsQuery {
    kind: Option<SuppressionKind>,
    /// 1 to 1000. Default 100.
    limit: Option<i64>,
    offset: Option<i64>,
}

/// Read the blocked emails and domains.
#[utoipa::path(get, path = "/v1/sending/suppressions", tag = "sending",
    params(ListSuppressionsQuery),
    responses((status = 200, body = Vec<Suppression>)),
    security(("api_key" = ["sending"])), description = "Scope: `sending`.")]
async fn list_suppressions(State(st): State<AppState>, ApiQuery(q): ApiQuery<ListSuppressionsQuery>) -> ApiResult<ApiJson<Vec<Suppression>>> {
    let limit = q.limit.unwrap_or(100);
    if !(1..=1000).contains(&limit) {
        return Err(ApiError::unprocessable("invalid_limit", "limit must be 1 to 1000."));
    }
    let rows = sqlx::query_as(
        "SELECT value::text AS value, kind, reason, created_at FROM suppressions
         WHERE ($1::suppression_kind IS NULL OR kind = $1) ORDER BY created_at DESC, value LIMIT $2 OFFSET $3",
    )
    .bind(q.kind)
    .bind(limit)
    .bind(q.offset.unwrap_or(0).max(0))
    .fetch_all(&st.pool)
    .await?;
    Ok(ApiJson(rows))
}

#[derive(Deserialize, ToSchema)]
pub struct AddSuppression {
    pub value: String,
    pub kind: SuppressionKind,
    /// Default `manual`.
    pub reason: Option<SuppressionReason>,
}

/// Block an email address or a whole domain.
#[utoipa::path(post, path = "/v1/sending/suppressions", tag = "sending",
    request_body = AddSuppression,
    responses((status = 200, body = Suppression)),
    security(("api_key" = ["sending"])), description = "Scope: `sending`. Adding the same value again updates the reason.")]
async fn add_suppression(State(st): State<AppState>, ApiJson(body): ApiJson<AddSuppression>) -> ApiResult<ApiJson<Suppression>> {
    let value = body.value.trim().to_lowercase();
    match body.kind {
        SuppressionKind::Email if value.parse::<lettre::Address>().is_err() => {
            return Err(ApiError::unprocessable("invalid_value", format!("'{value}' is not a valid email address.")));
        }
        SuppressionKind::Domain if value.is_empty() || value.contains(['@', ' ']) => {
            return Err(ApiError::unprocessable("invalid_value", format!("'{value}' is not a valid domain.")));
        }
        _ => {}
    }
    let row = sqlx::query_as(
        "INSERT INTO suppressions (value, kind, reason) VALUES ($1::citext, $2, $3)
         ON CONFLICT (value, kind) DO UPDATE SET reason = EXCLUDED.reason
         RETURNING value::text AS value, kind, reason, created_at",
    )
    .bind(&value)
    .bind(body.kind)
    .bind(body.reason.unwrap_or(SuppressionReason::Manual))
    .fetch_one(&st.pool)
    .await?;
    Ok(ApiJson(row))
}

#[derive(Deserialize, IntoParams)]
pub struct RemoveSuppressionQuery {
    value: String,
    kind: SuppressionKind,
}

#[derive(Serialize, ToSchema)]
pub struct RemovedReply {
    pub removed: bool,
}

/// Unblock an email address or a domain.
#[utoipa::path(delete, path = "/v1/sending/suppressions", tag = "sending",
    params(RemoveSuppressionQuery),
    responses((status = 200, body = RemovedReply)),
    security(("api_key" = ["sending"])), description = "Scope: `sending`.")]
async fn remove_suppression(State(st): State<AppState>, ApiQuery(q): ApiQuery<RemoveSuppressionQuery>) -> ApiResult<ApiJson<RemovedReply>> {
    let done = sqlx::query("DELETE FROM suppressions WHERE value = $1::citext AND kind = $2")
        .bind(q.value.trim())
        .bind(q.kind)
        .execute(&st.pool)
        .await?;
    if done.rows_affected() == 0 {
        return Err(ApiError::not_found("suppression_not_found", "This value is not blocked."));
    }
    Ok(ApiJson(RemovedReply { removed: true }))
}

// ---------- the whole engine ----------

#[derive(Serialize, ToSchema)]
pub struct EnginePauseReply {
    pub paused: bool,
}

async fn set_engine_paused(st: &AppState, paused: bool) -> ApiResult<ApiJson<EnginePauseReply>> {
    sqlx::query("UPDATE sending_defaults SET engine_paused = $1, updated_at = now() WHERE id = 1")
        .bind(paused)
        .execute(&st.pool)
        .await?;
    tracing::info!(paused, "engine pause flag changed");
    Ok(ApiJson(EnginePauseReply { paused }))
}

/// Pause all sending.
#[utoipa::path(post, path = "/v1/sending/engine/pause", tag = "sending",
    responses((status = 200, body = EnginePauseReply)),
    security(("api_key" = ["sending"])), description = "Scope: `sending`. Sends already running finish.")]
async fn pause_engine(State(st): State<AppState>) -> ApiResult<ApiJson<EnginePauseReply>> {
    set_engine_paused(&st, true).await
}

/// Resume all sending.
#[utoipa::path(post, path = "/v1/sending/engine/resume", tag = "sending",
    responses((status = 200, body = EnginePauseReply)),
    security(("api_key" = ["sending"])), description = "Scope: `sending`.")]
async fn resume_engine(State(st): State<AppState>) -> ApiResult<ApiJson<EnginePauseReply>> {
    set_engine_paused(&st, false).await
}

#[derive(Serialize, ToSchema)]
pub struct EngineStatus {
    pub paused: bool,
    pub dry_run: bool,
    pub jobs: BTreeMap<String, i64>,
    pub accounts: BTreeMap<String, i64>,
}

/// Paused or running, jobs per status, accounts per status.
#[utoipa::path(get, path = "/v1/sending/engine/status", tag = "sending",
    responses((status = 200, body = EngineStatus)),
    security(("api_key" = ["sending"])), description = "Scope: `sending`.")]
async fn engine_status(State(st): State<AppState>) -> ApiResult<ApiJson<EngineStatus>> {
    let paused = queue::engine_paused(&st.pool).await?;
    let jobs: Vec<(String, i64)> = sqlx::query_as("SELECT status::text, count(*) FROM send_jobs GROUP BY 1").fetch_all(&st.pool).await?;
    let accounts: Vec<(String, i64)> =
        sqlx::query_as("SELECT status::text, count(*) FROM email_accounts WHERE deleted_at IS NULL GROUP BY 1")
            .fetch_all(&st.pool)
            .await?;
    Ok(ApiJson(EngineStatus {
        paused,
        dry_run: st.config.dry_run,
        jobs: jobs.into_iter().collect(),
        accounts: accounts.into_iter().collect(),
    }))
}
