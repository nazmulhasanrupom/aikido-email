//! `/v1/automations/copilot/*` (scope `automations`): chat threads, messages, undo (build map 13 and 16).
//!
//! A turn: check the limits, load the memory (summary, newest messages), build the state block from the real draft,
//! ask the model (`turn::run_turn`: one repair try), then apply. Apply only ever edits a draft: `replace` saves
//! the old draft in the draft history first (source `copilot`) so Undo can put it back; `create` makes a new
//! workflow. Nothing here publishes, activates, runs or writes secrets.
use super::diff::{self, Diff};
use super::prompt::{self, ChatMsg, TestNode, TestRunInfo, Window};
use super::turn::{self, Action, CallLog, Outcome, Prompt};
use crate::auth::api_keys::Scope;
use crate::auth::middleware::{KeyScope, require_scope};
use crate::automations::definition::draft_hash;
use crate::automations::engine::ai_backend::{AiBackend, AiProvider, DbAi};
use crate::automations::registry::NodeError;
use crate::automations::routes::{Caller, WorkflowDetail, caller, draft_name, insert_history, insert_workflow, load_detail, not_found};
use crate::automations::validate::{Mode, Options, ValidationResult, validate_value};
use crate::automations::{settings, transfer_routes};
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::generator::llm::LlmError;
use crate::generator::providers;
use crate::state::AppState;
use crate::users::session::SessionCtx;
use axum::Extension;
use axum::extract::State;
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::HashSet;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

const MAX_DRAFT_BYTES: usize = 5 * 1024 * 1024;

pub fn router(state: AppState) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(copilot_list_threads, copilot_create_thread))
        .routes(routes!(copilot_delete_thread))
        .routes(routes!(copilot_list_messages, copilot_send_message))
        .routes(routes!(copilot_undo_message))
        .route_layer(axum::middleware::from_fn_with_state((state, Scope::Automations), require_scope))
}

// ---------- shapes ----------

#[derive(Serialize, ToSchema)]
pub struct ThreadOut {
    pub id: Uuid,
    /// The workflow the chat is about. `None`: a chat from the list page that has not made a workflow yet.
    pub workflow_id: Option<Uuid>,
    /// The first words of the first message.
    pub title: String,
    pub message_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
pub struct MessageOut {
    pub id: i64,
    /// `user` or `assistant`.
    pub role: String,
    pub content: String,
    /// Assistant messages only. Success: `{ workflow_id, mode: "create"|"replace"|"proposed", diff, draft_history_id?, undone? }`.
    /// Failure: `{ error, error_code: "invalid_workflow"|"bad_answer", errors: [issue] }`. `null`: the AI only answered.
    #[schema(value_type = Option<Object>)]
    pub applied: Option<Value>,
    /// The AI sent a workflow with this message.
    pub has_workflow: bool,
    pub created_at: DateTime<Utc>,
}

type MessageRow = (i64, String, String, Option<Value>, bool, DateTime<Utc>);

const MESSAGE_COLS: &str = "id, role, content, applied, (workflow_json IS NOT NULL), created_at";

fn message_out(r: MessageRow) -> MessageOut {
    MessageOut { id: r.0, role: r.1, content: r.2, applied: r.3, has_workflow: r.4, created_at: r.5 }
}

type ThreadRow = (Uuid, Option<Uuid>, String, DateTime<Utc>, DateTime<Utc>, i64);

const THREAD_SQL: &str = "SELECT t.id, t.workflow_id, t.title, t.created_at, t.updated_at, \
    (SELECT count(*) FROM automation_copilot_messages m WHERE m.thread_id = t.id) FROM automation_copilot_threads t";

fn thread_out(r: ThreadRow) -> ThreadOut {
    ThreadOut { id: r.0, workflow_id: r.1, title: r.2, created_at: r.3, updated_at: r.4, message_count: r.5 }
}

fn thread_not_found() -> ApiError {
    ApiError::not_found("thread_not_found", "There is no chat with this id.")
}

/// Who owns the threads this caller sees. A session sees its own; an API key sees all.
fn owner(c: &Caller) -> Option<Uuid> {
    c.user_id
}

async fn load_thread(pool: &PgPool, id: Uuid, c: &Caller) -> ApiResult<ThreadOut> {
    let sql = format!("{THREAD_SQL} WHERE t.id = $1 AND ($2::uuid IS NULL OR t.user_id = $2)");
    let row: Option<ThreadRow> = sqlx::query_as(&sql).bind(id).bind(owner(c)).fetch_optional(pool).await?;
    row.map(thread_out).ok_or_else(thread_not_found)
}

// ---------- threads ----------

#[derive(Deserialize, IntoParams)]
pub struct ThreadsQuery {
    /// Only the chats of this workflow.
    workflow_id: Option<Uuid>,
}

#[derive(Serialize, ToSchema)]
pub struct ThreadList {
    /// Newest first (by last message). Up to 100.
    pub threads: Vec<ThreadOut>,
}

/// The chats, newest first. Filter by `workflow_id` to get the chats of one workflow (the first one is the one to open).
#[utoipa::path(get, path = "/v1/automations/copilot/threads", tag = "automations", params(ThreadsQuery),
    responses((status = 200, body = ThreadList)),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. A session sees its own chats. An API key sees all chats.")]
async fn copilot_list_threads(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiQuery(q): ApiQuery<ThreadsQuery>,
) -> ApiResult<ApiJson<ThreadList>> {
    let c = caller(&session, &key);
    let sql = format!("{THREAD_SQL} WHERE ($1::uuid IS NULL OR t.user_id = $1) AND ($2::uuid IS NULL OR t.workflow_id = $2) ORDER BY t.updated_at DESC, t.id DESC LIMIT 100");
    let rows: Vec<ThreadRow> = sqlx::query_as(&sql).bind(owner(&c)).bind(q.workflow_id).fetch_all(&st.pool).await?;
    Ok(ApiJson(ThreadList { threads: rows.into_iter().map(thread_out).collect() }))
}

#[derive(Deserialize, ToSchema, Default)]
pub struct CreateThread {
    /// The workflow to talk about. Left out: a chat for the list page; the AI can make a workflow, and the chat is bound to it.
    workflow_id: Option<Uuid>,
}

/// Start a new chat ("New chat"). Old chats of the workflow stay.
#[utoipa::path(post, path = "/v1/automations/copilot/threads", tag = "automations", request_body(content = CreateThread, description = "Optional"),
    responses((status = 201, body = ThreadOut), (status = 404, description = "workflow_not_found")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`.")]
async fn copilot_create_thread(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    body: axum::body::Bytes,
) -> ApiResult<(StatusCode, ApiJson<ThreadOut>)> {
    let c = caller(&session, &key);
    let b: CreateThread = crate::error::parse_optional_json(&body)?;
    if let Some(w) = b.workflow_id {
        let found: Option<Uuid> = sqlx::query_scalar("SELECT id FROM automation_workflows WHERE id = $1").bind(w).fetch_optional(&st.pool).await?;
        found.ok_or_else(not_found)?;
    }
    let id = Uuid::now_v7();
    let now = st.now();
    sqlx::query("INSERT INTO automation_copilot_threads (id, workflow_id, user_id, created_at, updated_at) VALUES ($1, $2, $3, $4, $4)")
        .bind(id)
        .bind(b.workflow_id)
        .bind(c.user_id.unwrap_or_else(Uuid::nil))
        .bind(now)
        .execute(&st.pool)
        .await?;
    Ok((StatusCode::CREATED, ApiJson(load_thread(&st.pool, id, &c).await?)))
}

/// Delete a chat and its memory.
#[utoipa::path(delete, path = "/v1/automations/copilot/threads/{id}", tag = "automations", params(("id" = Uuid, Path)),
    responses((status = 204, description = "Deleted"), (status = 404, description = "thread_not_found")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. Changes the AI already applied to the workflow stay.")]
async fn copilot_delete_thread(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiPath(id): ApiPath<Uuid>,
) -> ApiResult<StatusCode> {
    let c = caller(&session, &key);
    let n = sqlx::query("DELETE FROM automation_copilot_threads WHERE id = $1 AND ($2::uuid IS NULL OR user_id = $2)").bind(id).bind(owner(&c)).execute(&st.pool).await?.rows_affected();
    if n == 0 {
        return Err(thread_not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---------- messages ----------

#[derive(Deserialize, IntoParams)]
pub struct MessagesQuery {
    /// Only messages with an id above this (for polling).
    after: Option<i64>,
    /// 1 to 500. Default 200. Without `after`, the newest `limit` messages.
    limit: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct MessageList {
    pub thread: ThreadOut,
    /// Oldest first.
    pub messages: Vec<MessageOut>,
}

/// The chat.
#[utoipa::path(get, path = "/v1/automations/copilot/threads/{id}/messages", tag = "automations", params(("id" = Uuid, Path), MessagesQuery),
    responses((status = 200, body = MessageList), (status = 404, description = "thread_not_found")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. Oldest first. The workflow the AI sent is not returned (`has_workflow` tells it exists); the workflow route has the draft.")]
async fn copilot_list_messages(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(q): ApiQuery<MessagesQuery>,
) -> ApiResult<ApiJson<MessageList>> {
    let c = caller(&session, &key);
    let thread = load_thread(&st.pool, id, &c).await?;
    let limit = q.limit.unwrap_or(200).clamp(1, 500);
    let sql = format!(
        "SELECT * FROM (SELECT {MESSAGE_COLS} FROM automation_copilot_messages WHERE thread_id = $1 AND id > $2 ORDER BY id {} LIMIT $3) m ORDER BY id",
        if q.after.is_some() { "ASC" } else { "DESC" }
    );
    let rows: Vec<MessageRow> = sqlx::query_as(&sql).bind(id).bind(q.after.unwrap_or(0)).bind(limit).fetch_all(&st.pool).await?;
    Ok(ApiJson(MessageList { thread, messages: rows.into_iter().map(message_out).collect() }))
}

#[derive(Deserialize, ToSchema)]
pub struct SendMessage {
    /// 1 to 8000 characters.
    text: String,
    /// The workflow in the editor. Needed when the chat has no workflow yet and the user opened one. Must match the chat's workflow otherwise.
    workflow_id: Option<Uuid>,
    /// `false`: do not change anything, only return what the AI would do (`applied.mode` is `proposed`). Default `true`.
    apply: Option<bool>,
}

#[derive(Serialize, ToSchema)]
pub struct SendReply {
    pub thread: ThreadOut,
    pub user_message: MessageOut,
    /// The AI's answer. `applied` says what changed. When the AI could not make a valid workflow, `applied.error` is set (still HTTP 200).
    pub message: MessageOut,
    /// The workflow the chat is bound to now. After `create` it is the new workflow: open it in the editor.
    pub workflow_id: Option<Uuid>,
    /// The `draft_hash` of the workflow after the change (use it as `if_hash` on the next save). `None`: nothing was applied.
    pub draft_hash: Option<String>,
}

fn api_from_node(e: NodeError) -> ApiError {
    match e.code.as_str() {
        "ai_not_configured" => ApiError::conflict("ai_not_configured", "Set up an AI provider in Settings > AI."),
        "ai_provider_not_found" => ApiError::conflict("ai_provider_not_found", e.message),
        "ai_provider_auth_failed" => ApiError::conflict("ai_provider_auth_failed", e.message),
        _ => ApiError::internal(),
    }
}

fn api_from_llm(e: &LlmError) -> ApiError {
    let bad = |code: &str, msg: String| ApiError::new(StatusCode::BAD_GATEWAY, code, msg);
    match e {
        LlmError::Transient { code, .. } => bad("ai_provider_unavailable", format!("The AI provider could not answer ({code}). Try again.")),
        LlmError::Auth => ApiError::conflict("ai_provider_auth_failed", "The AI provider refused the API key. Replace the key in Settings > AI."),
        LlmError::BadRequest(m) => bad("ai_bad_request", format!("The AI provider refused the request: {m}")),
        LlmError::Refused => bad("ai_refused", "The model declined to answer.".into()),
        LlmError::BadResponse(m) => bad("ai_bad_response", format!("The answer of the AI provider could not be read: {m}")),
    }
}

async fn record_calls(st: &AppState, purpose: &str, provider: &AiProvider, workflow_id: Option<Uuid>, calls: &[CallLog]) {
    for c in calls {
        let model = if c.model.is_empty() { provider.model.as_str() } else { c.model.as_str() };
        let r = sqlx::query(
            "INSERT INTO llm_calls (purpose, provider_id, model, input_tokens, output_tokens, cached_tokens, duration_ms, ok, error_code, created_at, workflow_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(purpose)
        .bind(provider.id)
        .bind(model)
        .bind(c.input_tokens as i32)
        .bind(c.output_tokens as i32)
        .bind(c.cached_tokens as i32)
        .bind(c.duration_ms as i32)
        .bind(c.ok)
        .bind(&c.error_code)
        .bind(st.now())
        .bind(workflow_id)
        .execute(&st.pool)
        .await;
        if let Err(e) = r {
            tracing::error!(error = %e, "could not write the llm_calls row of a copilot call");
        }
    }
}

/// The newest test run of a workflow: which nodes failed, and the item counts per node.
async fn newest_test_run(pool: &PgPool, workflow_id: Uuid) -> Result<Option<TestRunInfo>, sqlx::Error> {
    let run: Option<(Uuid, String, Option<Value>)> =
        sqlx::query_as("SELECT id, status, error FROM automation_runs WHERE workflow_id = $1 AND mode = 'test' ORDER BY queued_at DESC LIMIT 1").bind(workflow_id).fetch_optional(pool).await?;
    let Some((id, status, error)) = run else { return Ok(None) };
    let rows: Vec<(String, String, i32, Value, Option<Value>)> = sqlx::query_as(
        "SELECT DISTINCT ON (node_id) node_id, status, input_count, output_count, error FROM automation_run_nodes WHERE run_id = $1 ORDER BY node_id, seq DESC",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;
    let nodes = rows.into_iter().map(|(node_id, status, input_count, output_count, error)| TestNode { node_id, status, input_count: i64::from(input_count), output_count, error }).collect();
    Ok(Some(TestRunInfo { run_id: id.to_string(), status, error, nodes }))
}

/// The things the validator needs from the database, loaded once so the check can run in a plain closure.
struct Checker {
    providers: HashSet<String>,
    secrets: HashSet<String>,
    ops: std::sync::Arc<crate::automations::internal_call::OperationIndex>,
    allow_lead_erase: bool,
    is_admin: bool,
}

impl Checker {
    async fn load(st: &AppState, is_admin: bool) -> Result<Checker, sqlx::Error> {
        let providers: Vec<String> = sqlx::query_scalar("SELECT name FROM llm_providers").fetch_all(&st.pool).await?;
        let secrets: Vec<String> = sqlx::query_scalar("SELECT name FROM automation_secrets").fetch_all(&st.pool).await?;
        Ok(Checker { providers: providers.into_iter().collect(), secrets: secrets.into_iter().collect(), ops: st.internal_router.operations(st), allow_lead_erase: settings::load(&st.pool).await?.allow_lead_erase, is_admin })
    }
    fn check(&self, w: &Value) -> ValidationResult {
        validate_value(w, &Options { mode: Mode::Draft, providers: Some(&self.providers), secrets: Some(&self.secrets), is_admin: self.is_admin, operations: Some(self.ops.as_ref()), allow_lead_erase: self.allow_lead_erase })
    }
}

async fn chat_messages(pool: &PgPool, thread_id: Uuid, after: i64) -> Result<Vec<ChatMsg>, sqlx::Error> {
    let rows: Vec<(i64, String, String)> = sqlx::query_as("SELECT id, role, content FROM automation_copilot_messages WHERE thread_id = $1 AND id > $2 ORDER BY id").bind(thread_id).bind(after).fetch_all(pool).await?;
    Ok(rows.into_iter().map(|(id, role, content)| ChatMsg { id, role, content }).collect())
}

/// Fold old turns into the summary when the chat is too long. A failed fold is not an error: the turn goes on with the newest messages.
async fn memory(st: &AppState, thread_id: Uuid, provider: &AiProvider, workflow_id: Option<Uuid>) -> Result<(String, Vec<ChatMsg>), ApiError> {
    let raw: String = sqlx::query_scalar("SELECT summary FROM automation_copilot_threads WHERE id = $1").bind(thread_id).fetch_one(&st.pool).await?;
    let (upto, text) = prompt::split_summary(&raw);
    let text = text.to_string();
    let msgs = chat_messages(&st.pool, thread_id, upto).await?;
    match prompt::plan_window(&text, msgs) {
        Window::Keep(m) => Ok((text, m)),
        Window::Fold { old, keep } => {
            let mut calls = vec![];
            let r = turn::call(provider.client.as_ref(), prompt::SUMMARY_SYSTEM, &prompt::summary_user(&text, &old), 1500, false, &mut calls).await;
            record_calls(st, "automation_copilot_summary", provider, workflow_id, &calls).await;
            match r {
                Ok(resp) if !resp.text.trim().is_empty() => {
                    let upto = old.last().map_or(upto, |m| m.id);
                    let joined = prompt::join_summary(upto, resp.text.trim());
                    sqlx::query("UPDATE automation_copilot_threads SET summary = $2 WHERE id = $1").bind(thread_id).bind(&joined).execute(&st.pool).await?;
                    Ok((resp.text.trim().to_string(), keep))
                }
                _ => Ok((text, keep)),
            }
        }
    }
}

fn ensure_size(draft: &Value) -> ApiResult<()> {
    if draft.to_string().len() > MAX_DRAFT_BYTES {
        return Err(ApiError::unprocessable("draft_too_large", "The workflow is bigger than 5 MB."));
    }
    Ok(())
}

/// Save a `replace`: the old draft goes to the draft history first (source `copilot`), then the new draft.
/// Returns the id of the history entry that holds the old draft (what Undo restores), or `None` when nothing changed.
async fn apply_replace(st: &AppState, id: Uuid, mut new: Value, c: &Caller, checker: &Checker) -> ApiResult<(Value, Option<i64>, String)> {
    ensure_size(&new)?;
    let mut tx = st.pool.begin().await?;
    let cur: Option<(String, Value, String, String)> = sqlx::query_as("SELECT name, draft, draft_hash, description FROM automation_workflows WHERE id = $1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?;
    let (cur_name, old, cur_hash, cur_desc) = cur.ok_or_else(not_found)?;
    diff::keep_layout(&old, &mut new);
    let hash = draft_hash(&new);
    if hash == cur_hash {
        return Ok((new, None, hash));
    }
    let history_id: i64 = sqlx::query_scalar("INSERT INTO automation_draft_history (workflow_id, draft, draft_hash, source, created_by) VALUES ($1, $2, $3, 'copilot', $4) RETURNING id")
        .bind(id)
        .bind(&old)
        .bind(&cur_hash)
        .bind(c.user_id)
        .fetch_one(&mut *tx)
        .await?;
    let valid = checker.check(&new).ok;
    let name = draft_name(&new, &cur_name);
    let description = new.get("description").and_then(Value::as_str).map_or(cur_desc, str::to_string);
    sqlx::query("UPDATE automation_workflows SET draft = $2, draft_hash = $3, draft_valid = $4, name = $5, description = $6, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(&new)
        .bind(&hash)
        .bind(valid)
        .bind(name)
        .bind(description)
        .execute(&mut *tx)
        .await?;
    insert_history(&mut tx, id, &new, &hash, "copilot", c.user_id).await?;
    tx.commit().await?;
    Ok((new, Some(history_id), hash))
}

async fn insert_message(st: &AppState, thread_id: Uuid, role: &str, content: &str, workflow_json: Option<&Value>, applied: Option<&Value>, tokens: Option<(i32, i32)>) -> Result<MessageRow, sqlx::Error> {
    let sql = format!(
        "INSERT INTO automation_copilot_messages (thread_id, role, content, workflow_json, applied, tokens_in, tokens_out, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {MESSAGE_COLS}"
    );
    sqlx::query_as(&sql)
        .bind(thread_id)
        .bind(role)
        .bind(content)
        .bind(workflow_json)
        .bind(applied)
        .bind(tokens.map(|t| t.0))
        .bind(tokens.map(|t| t.1))
        .bind(st.now())
        .fetch_one(&st.pool)
        .await
}

fn one_line_title(text: &str) -> String {
    let t: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    t.chars().take(60).collect()
}

/// Send a message to the AI box. The answer may change the draft (`replace`) or make a workflow (`create`).
#[utoipa::path(post, path = "/v1/automations/copilot/threads/{id}/messages", tag = "automations", params(("id" = Uuid, Path)), request_body = SendMessage,
    responses((status = 200, body = SendReply),
        (status = 404, description = "thread_not_found, workflow_not_found"),
        (status = 409, description = "ai_not_configured (\"Set up an AI provider in Settings > AI\"), ai_provider_not_found, ai_provider_auth_failed, daily_budget_reached, copilot_disabled, thread_workflow_mismatch"),
        (status = 422, description = "empty_message, message_too_long (8000 characters), draft_too_large"),
        (status = 429, description = "rate_limited: 10 messages per minute per user"),
        (status = 502, description = "ai_provider_unavailable, ai_bad_request, ai_refused, ai_bad_response")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. The AI gets the guide, the real current draft with its validation and the newest test errors, a summary of old turns and the last messages. \
        It answers `{reply, action, workflow}`. A bad answer gets one repair try; if it is still bad the answer is saved with `applied.error` and the draft is not changed. \
        `replace` saves the old draft in the draft history (source `copilot`) and keeps the positions and pins of nodes that still exist. \
        The AI never publishes, activates, runs, or adds secrets. Every call is written to `llm_calls` (purpose `automation_copilot`) and counts in the daily token budget.")]
async fn copilot_send_message(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(b): ApiJson<SendMessage>,
) -> ApiResult<ApiJson<SendReply>> {
    let c = caller(&session, &key);
    let text = b.text.trim().to_string();
    if text.is_empty() {
        return Err(ApiError::unprocessable("empty_message", "Write a message."));
    }
    if text.chars().count() > prompt::MAX_MESSAGE_CHARS {
        return Err(ApiError::unprocessable("message_too_long", format!("A message can have up to {} characters.", prompt::MAX_MESSAGE_CHARS)));
    }
    let thread = load_thread(&st.pool, id, &c).await?;
    let auto = settings::load(&st.pool).await?;
    if !auto.copilot_enabled {
        return Err(ApiError::conflict("copilot_disabled", "The AI box is turned off in the automation settings."));
    }

    // rate limit: 10 messages a minute per user
    let now = st.now();
    let times: Vec<DateTime<Utc>> = sqlx::query_scalar(
        "SELECT m.created_at FROM automation_copilot_messages m JOIN automation_copilot_threads t ON t.id = m.thread_id WHERE t.user_id = $1 AND m.role = 'user' AND m.created_at > $2",
    )
    .bind(c.user_id.unwrap_or_else(Uuid::nil))
    .bind(now - chrono::Duration::seconds(60))
    .fetch_all(&st.pool)
    .await?;
    if let Err(wait) = prompt::rate_check(&times, now) {
        return Err(ApiError::too_many("rate_limited", "You sent 10 messages in a minute. Wait a little.", wait));
    }

    // the workflow of this chat
    let workflow_id = match (thread.workflow_id, b.workflow_id) {
        (Some(a), Some(b)) if a != b => return Err(ApiError::conflict("thread_workflow_mismatch", "This chat belongs to another workflow. Start a new chat.")),
        (a, b) => a.or(b),
    };
    let current: Option<(Value, String)> = match workflow_id {
        Some(w) => Some(sqlx::query_as("SELECT draft, draft_hash FROM automation_workflows WHERE id = $1").bind(w).fetch_optional(&st.pool).await?.ok_or_else(not_found)?),
        None => None,
    };

    // budget and provider
    let ai = DbAi { state: st.clone() };
    if ai.budget_reached().await.map_err(api_from_node)? {
        return Err(ApiError::conflict("daily_budget_reached", "The daily AI token budget is used up. Try again tomorrow, or raise it in Settings > AI."));
    }
    let provider = ai.provider(&auto.copilot_provider).await.map_err(api_from_node)?;

    // memory and state
    let (summary, chat) = memory(&st, id, &provider, workflow_id).await?;
    let checker = Checker::load(&st, c.is_admin).await?;
    let validation = current.as_ref().map(|(d, _)| checker.check(d));
    let test = match workflow_id {
        Some(w) => newest_test_run(&st.pool, w).await?,
        None => None,
    };
    let state = prompt::state_block(current.as_ref().map(|(d, _)| d), validation.as_ref(), test.as_ref());
    let names = transfer_routes::load_names(&st.pool).await?;
    let prompt = Prompt { system: prompt::system_text(Some(&names)), user: prompt::user_text(&summary, &chat, &state, &text) };

    // save the user's message first: it counts for the rate limit, and the chat keeps it if the provider fails
    if workflow_id != thread.workflow_id {
        sqlx::query("UPDATE automation_copilot_threads SET workflow_id = $2 WHERE id = $1").bind(id).bind(workflow_id).execute(&st.pool).await?;
    }
    let user_row = insert_message(&st, id, "user", &text, None, None, None).await?;

    let t = turn::run_turn(provider.client.as_ref(), &prompt, &|w| checker.check(w)).await;
    record_calls(&st, "automation_copilot", &provider, workflow_id, &t.calls).await;
    let tokens = (t.calls.iter().map(|x| x.input_tokens as i32).sum::<i32>(), t.calls.iter().map(|x| x.output_tokens as i32).sum::<i32>());

    let (reply, workflow_json, applied, bound, new_hash): (String, Option<Value>, Option<Value>, Option<Uuid>, Option<String>) = match t.outcome {
        Outcome::Llm(e) => {
            if e == LlmError::Auth {
                providers::mark_auth_failed(&st.pool, provider.id, st.now()).await;
            }
            return Err(api_from_llm(&e));
        }
        Outcome::Unusable { reply, code, message, errors, workflow } => {
            let reply = if reply.is_empty() { "I could not make a valid workflow from that.".to_string() } else { reply };
            (reply, workflow, Some(json!({ "error": message, "error_code": code, "errors": errors })), workflow_id, None)
        }
        Outcome::Answered(a) => match (a.action, a.workflow) {
            (Action::None, _) | (_, None) => (a.reply, None, None, workflow_id, None),
            (action, Some(wf)) => {
                let action = if workflow_id.is_none() { Action::Create } else { action };
                let old = current.as_ref().map_or_else(diff::empty_draft, |(d, _)| d.clone());
                if b.apply == Some(false) {
                    let mut laid = wf.clone();
                    diff::keep_layout(&old, &mut laid);
                    let d: Diff = diff::diff(&old, &laid);
                    (a.reply, Some(wf), Some(json!({ "workflow_id": workflow_id, "mode": "proposed", "diff": d })), workflow_id, None)
                } else if action == Action::Create {
                    let mut laid = wf.clone();
                    diff::keep_layout(&diff::empty_draft(), &mut laid);
                    ensure_size(&laid)?;
                    let name = draft_name(&laid, "New workflow");
                    let new_id = insert_workflow(&st, &name, laid.clone(), &[], "copilot", &c).await?;
                    sqlx::query("UPDATE automation_copilot_threads SET workflow_id = $2 WHERE id = $1").bind(id).bind(new_id).execute(&st.pool).await?;
                    let d = diff::diff(&diff::empty_draft(), &laid);
                    (a.reply, Some(wf), Some(json!({ "workflow_id": new_id, "mode": "create", "diff": d })), Some(new_id), Some(draft_hash(&laid)))
                } else {
                    let w = workflow_id.ok_or_else(not_found)?;
                    let (laid, history_id, hash) = apply_replace(&st, w, wf.clone(), &c, &checker).await?;
                    let d = diff::diff(&old, &laid);
                    (a.reply, Some(wf), Some(json!({ "workflow_id": w, "mode": "replace", "diff": d, "draft_history_id": history_id })), Some(w), Some(hash))
                }
            }
        },
    };

    let row = insert_message(&st, id, "assistant", &reply, workflow_json.as_ref(), applied.as_ref(), Some(tokens)).await?;
    let title = if thread.title.is_empty() { one_line_title(&text) } else { thread.title.clone() };
    sqlx::query("UPDATE automation_copilot_threads SET title = $2, updated_at = $3 WHERE id = $1").bind(id).bind(title).bind(st.now()).execute(&st.pool).await?;
    let thread = load_thread(&st.pool, id, &c).await?;
    Ok(ApiJson(SendReply { thread, user_message: message_out(user_row), message: message_out(row), workflow_id: bound, draft_hash: new_hash }))
}

// ---------- undo ----------

#[derive(Serialize, ToSchema)]
pub struct UndoReply {
    pub message: MessageOut,
    pub workflow: WorkflowDetail,
}

/// Undo the change of an AI message: the draft goes back to what it was before that message.
#[utoipa::path(post, path = "/v1/automations/copilot/messages/{id}/undo", tag = "automations", params(("id" = i64, Path)),
    responses((status = 200, body = UndoReply),
        (status = 404, description = "message_not_found, workflow_not_found, history_not_found"),
        (status = 409, description = "nothing_to_undo: the message changed no draft, or it is undone already")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. Restores the saved draft from `applied.draft_history_id` (the draft before the change). The draft as it is now is saved in the draft history first (source `restore`), so nothing is lost. Sets `applied.undone` to `true`.")]
async fn copilot_undo_message(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiPath(mid): ApiPath<i64>,
) -> ApiResult<ApiJson<UndoReply>> {
    let c = caller(&session, &key);
    let row: Option<(Option<Value>,)> = sqlx::query_as(
        "SELECT m.applied FROM automation_copilot_messages m JOIN automation_copilot_threads t ON t.id = m.thread_id WHERE m.id = $1 AND m.role = 'assistant' AND ($2::uuid IS NULL OR t.user_id = $2)",
    )
    .bind(mid)
    .bind(owner(&c))
    .fetch_optional(&st.pool)
    .await?;
    let applied = row.ok_or_else(|| ApiError::not_found("message_not_found", "There is no such message."))?.0;
    let nothing = || ApiError::conflict("nothing_to_undo", "This message changed no draft, or it is undone already.");
    let applied = applied.ok_or_else(nothing)?;
    let (Some(hid), Some(wid)) = (applied.get("draft_history_id").and_then(Value::as_i64), applied.get("workflow_id").and_then(Value::as_str).and_then(|s| Uuid::parse_str(s).ok())) else { return Err(nothing()) };
    if applied.get("undone").and_then(Value::as_bool) == Some(true) || applied.get("mode").and_then(Value::as_str) != Some("replace") {
        return Err(nothing());
    }
    let checker = Checker::load(&st, c.is_admin).await?;
    let mut tx = st.pool.begin().await?;
    let cur: Option<(String, Value, String, String)> = sqlx::query_as("SELECT name, draft, draft_hash, description FROM automation_workflows WHERE id = $1 FOR UPDATE").bind(wid).fetch_optional(&mut *tx).await?;
    let (cur_name, cur_draft, cur_hash, cur_desc) = cur.ok_or_else(not_found)?;
    let old: Option<Value> = sqlx::query_scalar("SELECT draft FROM automation_draft_history WHERE id = $1 AND workflow_id = $2").bind(hid).bind(wid).fetch_optional(&mut *tx).await?;
    let old = old.ok_or_else(|| ApiError::not_found("history_not_found", "The saved draft from before this change is gone (the draft history keeps the last 50 saves)."))?;
    let hash = draft_hash(&old);
    if hash != cur_hash {
        // the draft as it is now stays reachable in the history
        sqlx::query("INSERT INTO automation_draft_history (workflow_id, draft, draft_hash, source, created_by) VALUES ($1, $2, $3, 'restore', $4)").bind(wid).bind(&cur_draft).bind(&cur_hash).bind(c.user_id).execute(&mut *tx).await?;
        let valid = checker.check(&old).ok;
        let name = draft_name(&old, &cur_name);
        let description = old.get("description").and_then(Value::as_str).map_or(cur_desc, str::to_string);
        sqlx::query("UPDATE automation_workflows SET draft = $2, draft_hash = $3, draft_valid = $4, name = $5, description = $6, updated_at = now() WHERE id = $1")
            .bind(wid)
            .bind(&old)
            .bind(&hash)
            .bind(valid)
            .bind(name)
            .bind(description)
            .execute(&mut *tx)
            .await?;
        insert_history(&mut tx, wid, &old, &hash, "restore", c.user_id).await?;
    }
    let sql = format!("UPDATE automation_copilot_messages SET applied = jsonb_set(applied, '{{undone}}', 'true'::jsonb) WHERE id = $1 RETURNING {MESSAGE_COLS}");
    let msg: MessageRow = sqlx::query_as(&sql).bind(mid).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(ApiJson(UndoReply { message: message_out(msg), workflow: load_detail(&st, wid, c.is_admin).await? }))
}
