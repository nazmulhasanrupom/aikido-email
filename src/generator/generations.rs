//! `/v1/generator/generations*`: look at the AI text, edit it, and approve or reject it (review mode).
use super::guard::{self, GuardOptions};
use super::repo;
use super::settings::SpamCheck;
use super::slots::{self, AiSlot};
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::state::AppState;
use axum::extract::{Extension, State};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

pub const STATUSES: [&str; 9] = ["pending", "generating", "ready", "pending_review", "approved", "rejected", "failed", "cancelled", "stale"];

const COLS: &str = "g.id, g.enrollment_id, g.lead_id, g.sequence_id, s.campaign_id, g.step_number, g.status, g.slots, g.warnings, g.due_at, g.attempts, g.error_code, \
    g.provider_id, g.model, g.input_tokens, g.output_tokens, g.cached_tokens, g.reviewed_by, g.created_at, g.updated_at, g.seq";

#[derive(Clone, Debug, sqlx::FromRow, Serialize, ToSchema)]
pub struct Generation {
    pub id: Uuid,
    pub enrollment_id: Uuid,
    pub lead_id: Uuid,
    pub sequence_id: Uuid,
    pub campaign_id: Option<Uuid>,
    pub step_number: i32,
    /// `pending`, `generating`, `ready`, `pending_review`, `approved`, `rejected`, `failed`, `cancelled`, `stale`.
    pub status: String,
    /// The text of each slot (`{ "opener": "..." }`). Empty until the text is written.
    #[schema(value_type = Object)]
    pub slots: Value,
    /// Things the guard let through but you should look at (a spam word, a language doubt).
    #[schema(value_type = Vec<String>)]
    pub warnings: Value,
    pub due_at: Option<DateTime<Utc>>,
    pub attempts: i32,
    /// Why it failed (`guard_failed`, `bad_output`, `bad_request`, `provider_auth`, `rate_limited`, ...). Never the text of the model.
    pub error_code: Option<String>,
    pub provider_id: Option<Uuid>,
    pub model: Option<String>,
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub cached_tokens: i32,
    /// The id of the API key that edited, approved or rejected it.
    pub reviewed_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The paging cursor.
    pub seq: i64,
}

async fn load(conn: &mut sqlx::PgConnection, id: Uuid) -> ApiResult<Generation> {
    sqlx::query_as(&format!("SELECT {COLS} FROM generations g JOIN sequences s ON s.id = g.sequence_id WHERE g.id = $1"))
        .bind(id)
        .fetch_optional(conn)
        .await?
        .ok_or_else(|| ApiError::not_found("generation_not_found", "No generation with this id."))
}

#[derive(Deserialize, IntoParams)]
pub struct ListQuery {
    campaign_id: Option<Uuid>,
    sequence_id: Option<Uuid>,
    lead_id: Option<Uuid>,
    enrollment_id: Option<Uuid>,
    /// One of the nine statuses.
    status: Option<String>,
    /// `next_cursor` of the last page.
    cursor: Option<i64>,
    /// 1 to 500. Default 50.
    limit: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct ListReply {
    pub generations: Vec<Generation>,
    pub next_cursor: Option<i64>,
}

fn check_status(s: &str) -> ApiResult<()> {
    if STATUSES.contains(&s) { Ok(()) } else { Err(ApiError::unprocessable("invalid_status", format!("status must be one of: {}.", STATUSES.join(", ")))) }
}

/// List generations, newest first. Keyset paging.
#[utoipa::path(get, path = "/v1/generator/generations", tag = "generator",
    params(ListQuery), responses((status = 200, body = ListReply)),
    security(("api_key" = ["generator"])), description = "Scope: `generator`. Filter by campaign, sequence, lead, enrollment or status.")]
pub async fn list_generations(State(st): State<AppState>, ApiQuery(q): ApiQuery<ListQuery>) -> ApiResult<ApiJson<ListReply>> {
    let limit = q.limit.unwrap_or(50);
    if !(1..=500).contains(&limit) {
        return Err(ApiError::unprocessable("invalid_limit", "limit must be 1 to 500."));
    }
    if let Some(s) = &q.status {
        check_status(s)?;
    }
    let mut rows: Vec<Generation> = sqlx::query_as(&format!(
        "SELECT {COLS} FROM generations g JOIN sequences s ON s.id = g.sequence_id
          WHERE ($1::uuid IS NULL OR s.campaign_id = $1) AND ($2::uuid IS NULL OR g.sequence_id = $2) AND ($3::uuid IS NULL OR g.lead_id = $3)
            AND ($4::uuid IS NULL OR g.enrollment_id = $4) AND ($5::text IS NULL OR g.status = $5) AND ($6::bigint IS NULL OR g.seq < $6)
          ORDER BY g.seq DESC LIMIT $7"
    ))
    .bind(q.campaign_id)
    .bind(q.sequence_id)
    .bind(q.lead_id)
    .bind(q.enrollment_id)
    .bind(&q.status)
    .bind(q.cursor)
    .bind(limit + 1)
    .fetch_all(&st.pool)
    .await?;
    let next_cursor = if rows.len() as i64 > limit {
        rows.truncate(limit as usize);
        rows.last().map(|g| g.seq)
    } else {
        None
    };
    Ok(ApiJson(ListReply { generations: rows, next_cursor }))
}

/// One generation with the text of its slots.
#[utoipa::path(get, path = "/v1/generator/generations/{id}", tag = "generator",
    params(("id" = Uuid, Path)), responses((status = 200, body = Generation), (status = 404, description = "No such generation")),
    security(("api_key" = ["generator"])), description = "Scope: `generator`.")]
pub async fn get_generation(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<Generation>> {
    let mut conn = st.pool.acquire().await?;
    Ok(ApiJson(load(&mut conn, id).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct PatchGeneration {
    /// The slots to change: `{ "opener": "new text" }`. The others keep their text.
    pub slots: BTreeMap<String, String>,
}

/// Edit the text by hand. The guard runs on the result. The text counts as approved.
#[utoipa::path(patch, path = "/v1/generator/generations/{id}", tag = "generator",
    params(("id" = Uuid, Path)), request_body = PatchGeneration,
    responses((status = 200, body = Generation), (status = 404, description = "No such generation"),
        (status = 409, description = "The text is being written, is closed, or the step changed"),
        (status = 422, description = "unknown_slot, missing_slot, or guard_failed")),
    security(("api_key" = ["generator"])),
    description = "Scope: `generator`. Allowed for `ready`, `pending_review`, `approved`, `rejected` and `failed` text. A `failed` row has no slots yet: give all of them.")]
pub async fn patch_generation(
    State(st): State<AppState>,
    Extension(key_id): Extension<Uuid>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(body): ApiJson<PatchGeneration>,
) -> ApiResult<ApiJson<Generation>> {
    let now = st.now();
    let mut tx = st.pool.begin().await?;
    let row: Option<(String, Uuid, i32, String, Value)> =
        sqlx::query_as("SELECT status, sequence_id, step_number, step_hash, slots FROM generations WHERE id = $1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?;
    let Some((status, sequence_id, step_number, hash, current)) = row else {
        return Err(ApiError::not_found("generation_not_found", "No generation with this id."));
    };
    match status.as_str() {
        "ready" | "pending_review" | "approved" | "rejected" | "failed" => {}
        "pending" | "generating" => return Err(ApiError::conflict("generation_busy", "The text is not written yet.")),
        _ => return Err(ApiError::conflict("generation_closed", format!("A {status} generation can not be edited."))),
    }
    let step: Option<(Value, Option<String>)> = sqlx::query_as(
        "SELECT st.ai_slots, st.step_hash FROM sequence_steps st WHERE st.sequence_id = $1 AND st.step_number = $2",
    )
    .bind(sequence_id)
    .bind(step_number)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((slot_json, step_hash)) = step.filter(|s| s.1.as_deref() == Some(hash.as_str())) else {
        return Err(ApiError::conflict("generation_stale", "The step changed after this text was written."));
    };
    let defs: Vec<AiSlot> = slots::from_json(&slot_json);
    if let Some(k) = body.slots.keys().find(|k| !defs.iter().any(|d| &d.key == *k)) {
        return Err(ApiError::unprocessable("unknown_slot", format!("'{k}' is not a slot of this step.")));
    }
    let mut merged: BTreeMap<String, String> = current.as_object().map(|o| o.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect()).unwrap_or_default();
    merged.extend(body.slots);
    if let Some(d) = defs.iter().find(|d| merged.get(&d.key).is_none_or(|t| t.trim().is_empty())) {
        return Err(ApiError::unprocessable("missing_slot", format!("The slot '{}' has no text.", d.key)));
    }
    let settings = repo::load_settings(&mut tx).await?;
    let report = guard::check(
        &merged,
        &GuardOptions {
            slots: &defs,
            banned_phrases: &settings.banned_phrases,
            spam_words: &settings.spam_words,
            spam_fail: settings.spam_check == SpamCheck::Fail,
            language: settings.language.as_deref(),
        },
    );
    if !report.ok() {
        return Err(ApiError::unprocessable("guard_failed", report.reason()));
    }
    let _ = step_hash;
    let trimmed: BTreeMap<String, String> = merged.into_iter().map(|(k, v)| (k, v.trim().to_string())).collect();
    sqlx::query(
        "UPDATE generations SET slots = $2, warnings = $3, status = 'approved', error_code = NULL, reviewed_by = $4, claim_id = NULL, updated_at = $5 WHERE id = $1",
    )
    .bind(id)
    .bind(serde_json::to_value(&trimmed).unwrap_or_default())
    .bind(serde_json::json!(report.warnings))
    .bind(key_id)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    let out = load(&mut tx, id).await?;
    tx.commit().await?;
    Ok(ApiJson(out))
}

async fn review(st: &AppState, key_id: Uuid, id: Uuid, from: &[&str], to: &str) -> ApiResult<ApiJson<Generation>> {
    let mut tx = st.pool.begin().await?;
    let row: Option<(String,)> = sqlx::query_as("SELECT status FROM generations WHERE id = $1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?;
    let Some((status,)) = row else { return Err(ApiError::not_found("generation_not_found", "No generation with this id.")) };
    if status != to {
        if !from.contains(&status.as_str()) {
            return Err(ApiError::conflict("invalid_status", format!("A {status} generation can not become {to}.")));
        }
        sqlx::query("UPDATE generations SET status = $2, reviewed_by = $3, updated_at = $4 WHERE id = $1").bind(id).bind(to).bind(key_id).bind(st.now()).execute(&mut *tx).await?;
    }
    let out = load(&mut tx, id).await?;
    tx.commit().await?;
    Ok(ApiJson(out))
}

/// Approve the text (review mode). The step can send it at due time.
#[utoipa::path(post, path = "/v1/generator/generations/{id}/approve", tag = "generator",
    params(("id" = Uuid, Path)), responses((status = 200, body = Generation), (status = 404, description = "No such generation"), (status = 409, description = "The text can not be approved in its status")),
    security(("api_key" = ["generator"])), description = "Scope: `generator`. From `pending_review` or `ready`. Approving an approved text does nothing.")]
pub async fn approve(State(st): State<AppState>, Extension(key_id): Extension<Uuid>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<Generation>> {
    review(&st, key_id, id, &["pending_review", "ready"], "approved").await
}

/// Reject the text. The worker makes it again.
#[utoipa::path(post, path = "/v1/generator/generations/{id}/reject", tag = "generator",
    params(("id" = Uuid, Path)), responses((status = 200, body = Generation), (status = 404, description = "No such generation"), (status = 409, description = "The text can not be rejected in its status")),
    security(("api_key" = ["generator"])), description = "Scope: `generator`. From `pending_review`, `ready` or `approved`. The next worker tick writes a new text.")]
pub async fn reject(State(st): State<AppState>, Extension(key_id): Extension<Uuid>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<Generation>> {
    review(&st, key_id, id, &["pending_review", "ready", "approved"], "rejected").await
}

#[derive(Deserialize, ToSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum BulkAction {
    Approve,
    Reject,
    Regenerate,
}

#[derive(Deserialize, ToSchema, Default)]
pub struct BulkFilter {
    pub campaign_id: Option<Uuid>,
    pub sequence_id: Option<Uuid>,
    pub lead_id: Option<Uuid>,
    pub status: Option<String>,
    /// Up to 10000 ids.
    pub ids: Option<Vec<Uuid>>,
}

#[derive(Deserialize, ToSchema)]
pub struct BulkBody {
    pub action: BulkAction,
    pub filter: BulkFilter,
}

#[derive(Serialize, ToSchema)]
pub struct BulkReply {
    /// Generations that match the filter.
    pub matched: i64,
    /// Generations that changed (the others were in a status the action does not apply to).
    pub changed: u64,
}

/// Approve, reject or regenerate many generations at once.
#[utoipa::path(post, path = "/v1/generator/generations/bulk", tag = "generator",
    request_body = BulkBody, responses((status = 200, body = BulkReply), (status = 422, description = "The filter is empty")),
    security(("api_key" = ["generator"])),
    description = "Scope: `generator`. An empty filter is refused (`empty_filter`). `approve` applies to `pending_review` and `ready`. `reject` to `pending_review`, `ready` and `approved`. \
        `regenerate` makes the text again (status `pending`) for `ready`, `pending_review`, `approved`, `rejected`, `failed` and `stale` text whose enrollment is still at that step.")]
pub async fn bulk(State(st): State<AppState>, Extension(key_id): Extension<Uuid>, ApiJson(b): ApiJson<BulkBody>) -> ApiResult<ApiJson<BulkReply>> {
    let f = &b.filter;
    if f.campaign_id.is_none() && f.sequence_id.is_none() && f.lead_id.is_none() && f.status.is_none() && f.ids.as_ref().is_none_or(|i| i.is_empty()) {
        return Err(ApiError::unprocessable("empty_filter", "Give at least one filter: campaign_id, sequence_id, lead_id, status or ids."));
    }
    if let Some(s) = &f.status {
        check_status(s)?;
    }
    if f.ids.as_ref().is_some_and(|i| i.len() > 10_000) {
        return Err(ApiError::unprocessable("too_many_ids", "ids can have up to 10000 entries."));
    }
    let ids = f.ids.clone().filter(|i| !i.is_empty());
    let where_ = "($1::uuid IS NULL OR s.campaign_id = $1) AND ($2::uuid IS NULL OR g.sequence_id = $2) AND ($3::uuid IS NULL OR g.lead_id = $3)
        AND ($4::text IS NULL OR g.status = $4) AND ($5::uuid[] IS NULL OR g.id = ANY($5))";
    let (matched,): (i64,) = sqlx::query_as(&format!("SELECT count(*) FROM generations g JOIN sequences s ON s.id = g.sequence_id WHERE {where_}"))
        .bind(f.campaign_id)
        .bind(f.sequence_id)
        .bind(f.lead_id)
        .bind(&f.status)
        .bind(&ids)
        .fetch_one(&st.pool)
        .await?;
    let now = st.now();
    let sql = match b.action {
        BulkAction::Approve => format!(
            "UPDATE generations SET status = 'approved', reviewed_by = $6, updated_at = $7 WHERE id IN
               (SELECT g.id FROM generations g JOIN sequences s ON s.id = g.sequence_id WHERE {where_} AND g.status IN ('pending_review', 'ready'))"
        ),
        BulkAction::Reject => format!(
            "UPDATE generations SET status = 'rejected', reviewed_by = $6, updated_at = $7 WHERE id IN
               (SELECT g.id FROM generations g JOIN sequences s ON s.id = g.sequence_id WHERE {where_} AND g.status IN ('pending_review', 'ready', 'approved'))"
        ),
        BulkAction::Regenerate => format!(
            "UPDATE generations SET status = 'pending', slots = '{{}}', warnings = '[]', error_code = NULL, attempts = 0, claim_id = NULL, claimed_at = NULL, retry_at = NULL,
                    reviewed_by = $6, updated_at = $7 WHERE id IN
               (SELECT g.id FROM generations g JOIN sequences s ON s.id = g.sequence_id JOIN enrollments e ON e.id = g.enrollment_id
                 WHERE {where_} AND g.status IN ('ready', 'pending_review', 'approved', 'rejected', 'failed', 'stale')
                   AND e.next_step = g.step_number AND e.status IN ('active', 'paused'))"
        ),
    };
    let done = sqlx::query(&sql).bind(f.campaign_id).bind(f.sequence_id).bind(f.lead_id).bind(&f.status).bind(&ids).bind(key_id).bind(now).execute(&st.pool).await?;
    Ok(ApiJson(BulkReply { matched, changed: done.rows_affected() }))
}
