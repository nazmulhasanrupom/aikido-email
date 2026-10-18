//! `/v1/replies/*`
use super::classify::classify_raw;
use super::matcher::{self, MatchInput};
use super::model::{BounceInfo, INBOUND_COLS, InboundMessage, Label, MatchMethod};
use super::quote::strip_quoted;
use super::repo;
use super::settings::{ReplySettings, SettingsError};
use crate::auth::api_keys::Scope;
use crate::auth::middleware::require_scope;
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::state::AppState;
use axum::extract::State;
use base64::Engine;
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::BTreeMap;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Routes that need a key with scope `replies`.
pub fn router(state: AppState) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_defaults, patch_defaults))
        .routes(routes!(get_account_settings, patch_account_settings))
        .routes(routes!(reset_account_settings))
        .routes(routes!(classify_email))
        .routes(routes!(account_status))
        .routes(routes!(sync_now))
        .routes(routes!(pause_account))
        .routes(routes!(resume_account))
        .routes(routes!(list_messages))
        .routes(routes!(get_message, patch_message))
        .routes(routes!(lookup))
        .routes(routes!(pause_engine))
        .routes(routes!(resume_engine))
        .routes(routes!(engine_status))
        .route_layer(axum::middleware::from_fn_with_state((state, Scope::Replies), require_scope))
}

fn settings_error(e: SettingsError) -> ApiError {
    ApiError::unprocessable(e.code, e.message)
}

#[derive(Serialize, ToSchema)]
pub struct SettingsReply {
    pub settings: ReplySettings,
}

/// Read the defaults that new accounts copy.
#[utoipa::path(get, path = "/v1/replies/defaults", tag = "replies",
    responses((status = 200, body = SettingsReply)),
    security(("api_key" = ["replies"])), description = "Scope: `replies`.")]
async fn get_defaults(State(st): State<AppState>) -> ApiResult<ApiJson<SettingsReply>> {
    let mut conn = st.pool.acquire().await?;
    Ok(ApiJson(SettingsReply { settings: repo::load_defaults(&mut conn).await? }))
}

/// Change the defaults for new accounts. Send only the fields to change.
#[utoipa::path(patch, path = "/v1/replies/defaults", tag = "replies",
    request_body(content = ReplySettings, description = "Only the fields to change."),
    responses((status = 200, body = SettingsReply), (status = 422, description = "A setting is not valid")),
    security(("api_key" = ["replies"])),
    description = "Scope: `replies`. Merges the fields, validates the full result, then saves. Existing accounts keep their own settings.")]
async fn patch_defaults(State(st): State<AppState>, ApiJson(patch): ApiJson<Value>) -> ApiResult<ApiJson<SettingsReply>> {
    let mut tx = st.pool.begin().await?;
    let (current,): (Value,) = sqlx::query_as("SELECT settings FROM reply_defaults WHERE id = 1 FOR UPDATE").fetch_one(&mut *tx).await?;
    let merged = ReplySettings::from_stored(current).merged(&patch).map_err(settings_error)?;
    merged.validate().map_err(settings_error)?;
    sqlx::query("UPDATE reply_defaults SET settings = $1, updated_at = now() WHERE id = 1")
        .bind(serde_json::to_value(&merged).unwrap_or_default())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(ApiJson(SettingsReply { settings: merged }))
}

async fn require_account(st: &AppState, id: Uuid) -> ApiResult<crate::accounts::model::EmailAccount> {
    crate::accounts::repo::get(&st.pool, id)
        .await?
        .ok_or_else(|| ApiError::not_found("account_not_found", "No account with this id."))
}

/// Read the reply settings of one account.
#[utoipa::path(get, path = "/v1/replies/accounts/{id}/settings", tag = "replies",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = SettingsReply)),
    security(("api_key" = ["replies"])), description = "Scope: `replies`.")]
async fn get_account_settings(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<SettingsReply>> {
    require_account(&st, id).await?;
    let mut conn = st.pool.acquire().await?;
    Ok(ApiJson(SettingsReply { settings: repo::load_settings(&mut conn, id).await? }))
}

async fn save_account_settings(conn: &mut sqlx::PgConnection, id: Uuid, s: &ReplySettings) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO account_reply_settings (account_id, settings) VALUES ($1, $2)
         ON CONFLICT (account_id) DO UPDATE SET settings = EXCLUDED.settings, updated_at = now()",
    )
    .bind(id)
    .bind(serde_json::to_value(s).unwrap_or_default())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Change the reply settings of one account. Send only the fields to change.
#[utoipa::path(patch, path = "/v1/replies/accounts/{id}/settings", tag = "replies",
    params(("id" = Uuid, Path)),
    request_body(content = ReplySettings, description = "Only the fields to change."),
    responses((status = 200, body = SettingsReply), (status = 422, description = "A setting is not valid")),
    security(("api_key" = ["replies"])),
    description = "Scope: `replies`. Merges the fields, validates the full result, then saves.")]
async fn patch_account_settings(
    State(st): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(patch): ApiJson<Value>,
) -> ApiResult<ApiJson<SettingsReply>> {
    require_account(&st, id).await?;
    let mut tx = st.pool.begin().await?;
    let current = repo::load_settings(&mut tx, id).await?;
    let merged = current.merged(&patch).map_err(settings_error)?;
    merged.validate().map_err(settings_error)?;
    save_account_settings(&mut tx, id, &merged).await?;
    // `folders` may have changed: make and remove sync rows to match.
    let account = require_account(&st, id).await?;
    repo::ensure_sync_rows(&mut tx, &account, &merged).await?;
    tx.commit().await?;
    Ok(ApiJson(SettingsReply { settings: merged }))
}

/// Copy the defaults onto this account.
#[utoipa::path(post, path = "/v1/replies/accounts/{id}/settings/reset", tag = "replies",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = SettingsReply)),
    security(("api_key" = ["replies"])), description = "Scope: `replies`.")]
async fn reset_account_settings(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<SettingsReply>> {
    require_account(&st, id).await?;
    let mut tx = st.pool.begin().await?;
    let defaults = repo::load_defaults(&mut tx).await?;
    save_account_settings(&mut tx, id, &defaults).await?;
    let account = require_account(&st, id).await?;
    repo::ensure_sync_rows(&mut tx, &account, &defaults).await?;
    tx.commit().await?;
    Ok(ApiJson(SettingsReply { settings: defaults }))
}

// ---------- classify (dry run) ----------

#[derive(Deserialize, ToSchema)]
pub struct ClassifyBody {
    /// Base64 of the `.eml` file.
    pub raw: String,
    /// Use the settings of this account, and match against its sends. Default: the defaults, all accounts.
    pub account_id: Option<Uuid>,
}

#[derive(Serialize, ToSchema)]
pub struct ClassifyMatch {
    pub method: MatchMethod,
    pub job_id: Option<Uuid>,
    pub account_id: Option<Uuid>,
    pub lead_id: Option<Uuid>,
    pub campaign_id: Option<Uuid>,
}

#[derive(Serialize, ToSchema)]
pub struct ClassifyReply {
    pub label: Label,
    pub reason: String,
    pub bounce: Option<BounceInfo>,
    /// The new text of the email, without old quoted text.
    pub reply_text: Option<String>,
    /// The match this email would make. Nothing is written.
    pub matched: ClassifyMatch,
    /// `true` if the stored copy of this email would be dropped (`reply`, `auto_reply` or `unsubscribe` with no match).
    pub would_be_dropped: bool,
}

/// Classify and match one email without writing anything. For tests and for tuning the keyword lists.
#[utoipa::path(post, path = "/v1/replies/classify", tag = "replies",
    request_body = ClassifyBody,
    responses((status = 200, body = ClassifyReply), (status = 422, description = "The body is not base64")),
    security(("api_key" = ["replies"])),
    description = "Scope: `replies`. Writes nothing.")]
async fn classify_email(State(st): State<AppState>, ApiJson(body): ApiJson<ClassifyBody>) -> ApiResult<ApiJson<ClassifyReply>> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(body.raw.trim())
        .map_err(|_| ApiError::unprocessable("invalid_raw", "raw must be the base64 text of an .eml file."))?;
    let mut conn = st.pool.acquire().await?;
    let settings = match body.account_id {
        Some(id) => {
            require_account(&st, id).await?;
            repo::load_settings(&mut conn, id).await?
        }
        None => repo::load_defaults(&mut conn).await?,
    };
    let (parsed, c) = classify_raw(&raw, &settings);
    let (matched, reply_text) = match &parsed {
        Some(p) => {
            let candidates = if c.bounce.as_ref().is_some_and(|b| b.recipient.is_none()) {
                super::dsn::extract_bounce(p).1
            } else {
                vec![]
            };
            let input = MatchInput {
                account_id: body.account_id,
                from_email: p.from_email.as_deref().unwrap_or_default(),
                in_reply_to: p.in_reply_to.as_deref(),
                references: &p.references,
                thread_id: None,
                original_message_id: c.bounce.as_ref().and_then(|b| b.original_message_id.as_deref()),
                bounce_recipient: c.bounce.as_ref().and_then(|b| b.recipient.as_deref()),
                bounce_candidates: &candidates,
                match_window_days: settings.match_window_days,
            };
            let m = matcher::find_match(&mut conn, &input, st.now()).await?;
            let text = matches!(c.label, Label::Reply | Label::AutoReply | Label::Unsubscribe).then(|| strip_quoted(&p.text));
            (m, text)
        }
        None => (matcher::MatchResult { method: MatchMethod::None, job: None, resolved_recipient: None }, None),
    };
    let would_be_dropped = c.label.needs_match() && matched.job.is_none();
    Ok(ApiJson(ClassifyReply {
        label: c.label,
        reason: c.reason,
        bounce: c.bounce,
        reply_text,
        matched: ClassifyMatch {
            method: matched.method,
            job_id: matched.job.as_ref().map(|j| j.id),
            account_id: matched.job.as_ref().map(|j| j.account_id),
            lead_id: matched.job.as_ref().and_then(|j| j.lead_id),
            campaign_id: matched.job.as_ref().and_then(|j| j.campaign_id),
        },
        would_be_dropped,
    }))
}

// ---------- one account ----------

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct FolderStatus {
    pub folder: String,
    pub paused: bool,
    pub last_success_at: Option<DateTime<Utc>>,
    pub next_sync_at: Option<DateTime<Utc>>,
    pub consecutive_errors: i32,
    pub last_error: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct AccountReplyStatus {
    pub account_id: Uuid,
    pub can_read: bool,
    /// `ok`, `reconnect_needed`, `login_failed`, `not_configured`, or `null` (not checked yet).
    pub read_status: Option<String>,
    pub granted_scopes: Vec<String>,
    pub folders: Vec<FolderStatus>,
    /// The newest error of any folder.
    pub last_error: Option<String>,
    /// Inbound emails per label in the last 7 days.
    pub labels_7d: BTreeMap<String, i64>,
}

/// Can the inbox be read, when it was read, when it is read next, the last error, and the labels of the last 7 days.
#[utoipa::path(get, path = "/v1/replies/accounts/{id}/status", tag = "replies",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = AccountReplyStatus)),
    security(("api_key" = ["replies"])), description = "Scope: `replies`.")]
async fn account_status(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<AccountReplyStatus>> {
    let account = require_account(&st, id).await?;
    let folders: Vec<FolderStatus> = sqlx::query_as(
        "SELECT folder, paused, last_success_at, next_sync_at, consecutive_errors, last_error
         FROM inbox_sync_state WHERE account_id = $1 ORDER BY folder",
    )
    .bind(id)
    .fetch_all(&st.pool)
    .await?;
    let counts: Vec<(String, i64)> = sqlx::query_as(
        "SELECT label::text, count(*) FROM inbound_messages WHERE account_id = $1 AND received_at > $2 GROUP BY 1",
    )
    .bind(id)
    .bind(st.now() - chrono::Duration::days(7))
    .fetch_all(&st.pool)
    .await?;
    let last_error = folders.iter().filter_map(|f| f.last_error.clone()).next();
    Ok(ApiJson(AccountReplyStatus {
        account_id: id,
        can_read: account.can_read,
        read_status: account.read_status.clone(),
        granted_scopes: account.granted_scopes.clone(),
        folders,
        last_error,
        labels_7d: counts.into_iter().collect(),
    }))
}

#[derive(Serialize, ToSchema)]
pub struct SyncNowReply {
    /// Folders that will be read on the next tick.
    pub folders: i64,
}

/// Sync now: the next poller tick reads the inbox. Sets `next_sync_at = now`.
#[utoipa::path(post, path = "/v1/replies/accounts/{id}/sync", tag = "replies",
    params(("id" = Uuid, Path)),
    responses((status = 202, body = SyncNowReply), (status = 409, description = "The inbox cannot be read")),
    security(("api_key" = ["replies"])),
    description = "Scope: `replies`. Returns at once. The poller does the work on its next tick.")]
async fn sync_now(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<(axum::http::StatusCode, ApiJson<SyncNowReply>)> {
    let account = require_account(&st, id).await?;
    if !account.can_read {
        return Err(ApiError::conflict(
            "reading_not_available",
            "This inbox cannot be read. Connect the account again with the read permission (or fix the IMAP login).",
        ));
    }
    let mut tx = st.pool.begin().await?;
    let settings = repo::load_settings(&mut tx, id).await?;
    repo::ensure_sync_rows(&mut tx, &account, &settings).await?;
    let done = sqlx::query("UPDATE inbox_sync_state SET next_sync_at = $2 WHERE account_id = $1 AND sync_started_at IS NULL")
        .bind(id)
        .bind(st.now())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok((axum::http::StatusCode::ACCEPTED, ApiJson(SyncNowReply { folders: done.rows_affected() as i64 })))
}

#[derive(Serialize, ToSchema)]
pub struct PausedReply {
    pub paused: bool,
}

async fn set_account_paused(st: &AppState, id: Uuid, paused: bool) -> ApiResult<ApiJson<PausedReply>> {
    let account = require_account(st, id).await?;
    let mut tx = st.pool.begin().await?;
    let settings = repo::load_settings(&mut tx, id).await?;
    repo::ensure_sync_rows(&mut tx, &account, &settings).await?;
    if paused {
        sqlx::query("UPDATE inbox_sync_state SET paused = true WHERE account_id = $1").bind(id).execute(&mut *tx).await?;
    } else {
        sqlx::query("UPDATE inbox_sync_state SET paused = false, consecutive_errors = 0, last_error = NULL, next_sync_at = NULL WHERE account_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(ApiJson(PausedReply { paused }))
}

/// Pause the reply check for this account. Sending is not touched.
#[utoipa::path(post, path = "/v1/replies/accounts/{id}/pause", tag = "replies",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = PausedReply)),
    security(("api_key" = ["replies"])),
    description = "Scope: `replies`. While it is paused, follow-ups of this account wait (see `reply_check_max_age_mins`).")]
async fn pause_account(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<PausedReply>> {
    set_account_paused(&st, id, true).await
}

/// Resume the reply check for this account. Resets `consecutive_errors`.
#[utoipa::path(post, path = "/v1/replies/accounts/{id}/resume", tag = "replies",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = PausedReply)),
    security(("api_key" = ["replies"])), description = "Scope: `replies`.")]
async fn resume_account(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<PausedReply>> {
    set_account_paused(&st, id, false).await
}

// ---------- inbound emails ----------

#[derive(Deserialize, IntoParams)]
pub struct ListMessagesQuery {
    label: Option<Label>,
    account_id: Option<Uuid>,
    lead_id: Option<Uuid>,
    campaign_id: Option<Uuid>,
    /// The send job the email answers.
    job_id: Option<Uuid>,
    /// Only emails received at or after this time (ISO 8601).
    since: Option<DateTime<Utc>>,
    /// `next_cursor` from the last page.
    cursor: Option<Uuid>,
    /// 1 to 500. Default 50.
    limit: Option<i64>,
    /// `false`: only emails whose lead has no interest set yet ("not handled"). `true`: only those that have one.
    handled: Option<bool>,
}

#[derive(Serialize, ToSchema)]
pub struct ListMessagesReply {
    pub messages: Vec<InboundMessage>,
    pub next_cursor: Option<Uuid>,
}

/// List inbound emails, newest first. Cursor paging.
#[utoipa::path(get, path = "/v1/replies/messages", tag = "replies",
    params(ListMessagesQuery),
    responses((status = 200, body = ListMessagesReply)),
    security(("api_key" = ["replies"])), description = "Scope: `replies`.")]
async fn list_messages(State(st): State<AppState>, ApiQuery(q): ApiQuery<ListMessagesQuery>) -> ApiResult<ApiJson<ListMessagesReply>> {
    let limit = q.limit.unwrap_or(50);
    if !(1..=500).contains(&limit) {
        return Err(ApiError::unprocessable("invalid_limit", "limit must be 1 to 500."));
    }
    let mut messages: Vec<InboundMessage> = sqlx::query_as(&format!(
        "SELECT {INBOUND_COLS} FROM inbound_messages
         WHERE ($1::inbound_label IS NULL OR label = $1)
           AND ($2::uuid IS NULL OR account_id = $2)
           AND ($3::uuid IS NULL OR lead_id = $3)
           AND ($4::uuid IS NULL OR campaign_id = $4)
           AND ($5::uuid IS NULL OR matched_job_id = $5)
           AND ($6::timestamptz IS NULL OR received_at >= $6)
           AND ($7::uuid IS NULL OR id < $7)
           AND ($9::bool IS NULL OR $9 = EXISTS (SELECT 1 FROM leads l WHERE l.id = inbound_messages.lead_id AND l.interest IS NOT NULL))
         ORDER BY id DESC LIMIT $8"
    ))
    .bind(q.label)
    .bind(q.account_id)
    .bind(q.lead_id)
    .bind(q.campaign_id)
    .bind(q.job_id)
    .bind(q.since)
    .bind(q.cursor)
    .bind(limit + 1)
    .bind(q.handled)
    .fetch_all(&st.pool)
    .await?;
    let next_cursor = if messages.len() as i64 > limit {
        messages.truncate(limit as usize);
        messages.last().map(|m| m.id)
    } else {
        None
    };
    Ok(ApiJson(ListMessagesReply { messages, next_cursor }))
}

/// One inbound email with its actions.
#[utoipa::path(get, path = "/v1/replies/messages/{id}", tag = "replies",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = InboundMessage), (status = 404, description = "No such email")),
    security(("api_key" = ["replies"])), description = "Scope: `replies`. `actions` lists what the email did.")]
async fn get_message(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<InboundMessage>> {
    super::ingest::get(&st.pool, id)
        .await?
        .map(ApiJson)
        .ok_or_else(|| ApiError::not_found("message_not_found", "No inbound email with this id."))
}

#[derive(Deserialize, ToSchema)]
pub struct RelabelBody {
    pub label: Label,
}

/// Fix a label by hand.
#[utoipa::path(patch, path = "/v1/replies/messages/{id}", tag = "replies",
    params(("id" = Uuid, Path)), request_body = RelabelBody,
    responses((status = 200, body = InboundMessage), (status = 404, description = "No such email")),
    security(("api_key" = ["replies"])),
    description = "Scope: `replies`. Sets `label_source = manual`. Removes the suppressions this email made, then runs the actions of the new label. Cancelled jobs are not restored: requeue them on purpose.")]
async fn patch_message(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(body): ApiJson<RelabelBody>) -> ApiResult<ApiJson<InboundMessage>> {
    match super::ingest::relabel(&st.pool, id, body.label, st.now()).await {
        Ok(row) => Ok(ApiJson(row)),
        Err(super::ingest::RelabelError::NotFound) => Err(ApiError::not_found("message_not_found", "No inbound email with this id.")),
        Err(super::ingest::RelabelError::Db(e)) => Err(e.into()),
    }
}

// ---------- lookup ----------

#[derive(Deserialize, IntoParams)]
pub struct LookupQuery {
    email: String,
}

#[derive(Serialize, ToSchema)]
pub struct BouncedInfo {
    pub at: DateTime<Utc>,
    /// `hard`, `soft` or `block`.
    pub kind: String,
}

#[derive(Serialize, ToSchema)]
pub struct LookupReply {
    pub email: String,
    pub replied_at: Option<DateTime<Utc>>,
    pub bounced: Option<BouncedInfo>,
    pub unsubscribed_at: Option<DateTime<Utc>>,
    pub complained_at: Option<DateTime<Utc>>,
    /// The reason in `suppressions`, if the address is blocked.
    pub suppressed_as: Option<String>,
}

/// One address: did it reply, bounce, unsubscribe, or complain, and when. For follow-ups and the UI.
#[utoipa::path(get, path = "/v1/replies/lookup", tag = "replies",
    params(LookupQuery),
    responses((status = 200, body = LookupReply)),
    security(("api_key" = ["replies"])), description = "Scope: `replies`.")]
async fn lookup(State(st): State<AppState>, ApiQuery(q): ApiQuery<LookupQuery>) -> ApiResult<ApiJson<LookupReply>> {
    let email = q.email.trim().to_lowercase();
    if email.parse::<lettre::Address>().is_err() {
        return Err(ApiError::unprocessable("invalid_email", format!("'{}' is not a valid email address.", q.email)));
    }
    let (replied_at,): (Option<DateTime<Utc>>,) = sqlx::query_as(
        "SELECT max(m.received_at) FROM inbound_messages m LEFT JOIN send_jobs j ON j.id = m.matched_job_id
         WHERE m.label = 'reply' AND (m.from_email = $1::citext OR j.to_email = $1::citext)",
    )
    .bind(&email)
    .fetch_one(&st.pool)
    .await?;
    let (unsubscribed_at,): (Option<DateTime<Utc>>,) =
        sqlx::query_as("SELECT max(received_at) FROM inbound_messages WHERE label = 'unsubscribe' AND from_email = $1::citext")
            .bind(&email)
            .fetch_one(&st.pool)
            .await?;
    let (complained_at,): (Option<DateTime<Utc>>,) =
        sqlx::query_as("SELECT max(received_at) FROM inbound_messages WHERE label = 'complaint' AND bounce_recipient = $1::citext")
            .bind(&email)
            .fetch_one(&st.pool)
            .await?;
    let bounced: Option<(DateTime<Utc>, String)> = sqlx::query_as(
        "SELECT received_at, substr(label::text, 8) FROM inbound_messages
         WHERE label IN ('bounce_hard', 'bounce_soft', 'bounce_block') AND bounce_recipient = $1::citext
         ORDER BY received_at DESC LIMIT 1",
    )
    .bind(&email)
    .fetch_optional(&st.pool)
    .await?;
    let suppressed: Option<(String,)> =
        sqlx::query_as("SELECT reason::text FROM suppressions WHERE kind = 'email' AND value = $1::citext")
            .bind(&email)
            .fetch_optional(&st.pool)
            .await?;
    Ok(ApiJson(LookupReply {
        email,
        replied_at,
        bounced: bounced.map(|(at, kind)| BouncedInfo { at, kind }),
        unsubscribed_at,
        complained_at,
        suppressed_as: suppressed.map(|s| s.0),
    }))
}

// ---------- the whole reply check ----------

#[derive(Serialize, ToSchema)]
pub struct EnginePauseReply {
    pub paused: bool,
}

async fn set_engine_paused(st: &AppState, paused: bool) -> ApiResult<ApiJson<EnginePauseReply>> {
    sqlx::query("UPDATE reply_defaults SET engine_paused = $1, updated_at = now() WHERE id = 1")
        .bind(paused)
        .execute(&st.pool)
        .await?;
    tracing::info!(paused, "reply check pause flag changed");
    Ok(ApiJson(EnginePauseReply { paused }))
}

/// Pause all reply checks.
#[utoipa::path(post, path = "/v1/replies/engine/pause", tag = "replies",
    responses((status = 200, body = EnginePauseReply)),
    security(("api_key" = ["replies"])),
    description = "Scope: `replies`. Syncs already running finish. Follow-ups wait while the inbox is not read (see `reply_check_max_age_mins`).")]
async fn pause_engine(State(st): State<AppState>) -> ApiResult<ApiJson<EnginePauseReply>> {
    set_engine_paused(&st, true).await
}

/// Resume all reply checks.
#[utoipa::path(post, path = "/v1/replies/engine/resume", tag = "replies",
    responses((status = 200, body = EnginePauseReply)),
    security(("api_key" = ["replies"])), description = "Scope: `replies`.")]
async fn resume_engine(State(st): State<AppState>) -> ApiResult<ApiJson<EnginePauseReply>> {
    set_engine_paused(&st, false).await
}

#[derive(Serialize, ToSchema)]
pub struct ReplyEngineStatus {
    pub paused: bool,
    pub poller_enabled: bool,
    /// Accounts per `read_status`. `unchecked` = not looked at yet.
    pub accounts: BTreeMap<String, i64>,
    /// Inbound emails per label since 00:00 UTC today.
    pub today: BTreeMap<String, i64>,
}

/// Paused or running, accounts per read status, and emails per label today.
#[utoipa::path(get, path = "/v1/replies/engine/status", tag = "replies",
    responses((status = 200, body = ReplyEngineStatus)),
    security(("api_key" = ["replies"])), description = "Scope: `replies`.")]
async fn engine_status(State(st): State<AppState>) -> ApiResult<ApiJson<ReplyEngineStatus>> {
    let paused = repo::engine_paused(&st.pool).await?;
    let accounts: Vec<(String, i64)> = sqlx::query_as(
        "SELECT COALESCE(read_status, 'unchecked'), count(*) FROM email_accounts WHERE deleted_at IS NULL GROUP BY 1",
    )
    .fetch_all(&st.pool)
    .await?;
    let day_start = st.now().date_naive().and_hms_opt(0, 0, 0).expect("midnight exists").and_utc();
    let today: Vec<(String, i64)> =
        sqlx::query_as("SELECT label::text, count(*) FROM inbound_messages WHERE received_at >= $1 GROUP BY 1")
            .bind(day_start)
            .fetch_all(&st.pool)
            .await?;
    Ok(ApiJson(ReplyEngineStatus {
        paused,
        poller_enabled: st.config.reply_check_enabled,
        accounts: accounts.into_iter().collect(),
        today: today.into_iter().collect(),
    }))
}
