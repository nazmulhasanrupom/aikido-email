//! `/v1/automations/*` (scope `automations`): workflows (draft only for now), validate, the node
//! catalog and `schema.json`. Publish, activate, test runs, runs, secrets and the rest come in later phases.
use super::definition::{WorkflowDefinition, draft_hash};
use super::registry::Registry;
use super::validate::{Mode, Options, ValidationResult, validate_value};
use crate::auth::api_keys::Scope;
use crate::auth::middleware::{KeyScope, require_any, require_scope};
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::state::AppState;
use crate::users::session::SessionCtx;
use axum::Extension;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use std::collections::HashSet;
use std::sync::LazyLock;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// The biggest draft we keep (the JSON text).
const MAX_DRAFT_BYTES: usize = 5 * 1024 * 1024;
const MAX_TAGS: usize = 20;
const KEEP_DRAFT_HISTORY: i64 = 50;

pub fn router(state: AppState) -> OpenApiRouter<AppState> {
    let state2 = state.clone();
    let scoped = OpenApiRouter::new()
        .routes(routes!(list_workflows, create_workflow))
        .routes(routes!(get_workflow, patch_workflow, delete_workflow))
        .routes(routes!(duplicate_workflow))
        .routes(routes!(validate_definition))
        .routes(routes!(get_nodes))
        .route_layer(axum::middleware::from_fn_with_state((state.clone(), Scope::Automations), require_scope));
    // The schema is for LLM tools: any valid key or session may read it.
    let any_key = OpenApiRouter::new().routes(routes!(get_schema)).route_layer(axum::middleware::from_fn_with_state(state, require_any));
    scoped
        .merge(any_key)
        .merge(super::engine::routes::router(state2.clone()))
        .merge(super::editor::router(state2.clone()))
        .merge(super::production::routes::router(state2.clone()))
        .merge(super::production::webhooks::router(state2.clone()))
        .merge(super::transfer_routes::router(state2.clone()))
        .merge(super::secrets::router(state2.clone()))
        .merge(super::settings::router(state2))
}

// ---------- who is calling ----------

pub(super) struct Caller {
    pub(super) user_id: Option<Uuid>,
    pub(super) is_admin: bool,
}

pub(super) fn caller(session: &Option<Extension<SessionCtx>>, key: &Option<Extension<KeyScope>>) -> Caller {
    match (session, key) {
        (Some(Extension(s)), _) => Caller { user_id: Some(s.user_id), is_admin: s.role.is_admin() },
        (None, Some(Extension(k))) => Caller { user_id: None, is_admin: k.0 == Scope::Admin },
        (None, None) => Caller { user_id: None, is_admin: false },
    }
}

/// The AI provider names and secret names of this install, for the checks `unknown_provider` and `unknown_secret`.
async fn install_names(pool: &PgPool) -> Result<(HashSet<String>, HashSet<String>), sqlx::Error> {
    let providers: Vec<String> = sqlx::query_scalar("SELECT name FROM llm_providers").fetch_all(pool).await?;
    let secrets: Vec<String> = sqlx::query_scalar("SELECT name FROM automation_secrets").fetch_all(pool).await?;
    Ok((providers.into_iter().collect(), secrets.into_iter().collect()))
}

pub(super) async fn validate_draft(st: &AppState, draft: &Value, mode: Mode, is_admin: bool) -> Result<ValidationResult, sqlx::Error> {
    let (providers, secrets) = install_names(&st.pool).await?;
    let allow_lead_erase = super::settings::load(&st.pool).await?.allow_lead_erase;
    let ops = st.internal_router.operations(st);
    Ok(validate_value(draft, &Options { mode, providers: Some(&providers), secrets: Some(&secrets), is_admin, operations: Some(ops.as_ref()), allow_lead_erase }))
}

// ---------- shapes ----------

#[derive(Serialize, ToSchema)]
pub struct LastRun {
    pub status: String,
    pub at: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
pub struct Runs24h {
    pub success: i64,
    pub error: i64,
}

#[derive(Serialize, ToSchema)]
pub struct WorkflowListItem {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub active: bool,
    /// The number of the published version. `None`: never published.
    pub published_version: Option<i32>,
    /// A version is published and the draft is different from it.
    pub draft_changed: bool,
    pub draft_valid: bool,
    pub last_run: Option<LastRun>,
    pub runs_24h: Runs24h,
    pub tags: Vec<String>,
    pub archived_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

#[derive(FromRow)]
struct ListRow {
    id: Uuid,
    name: String,
    description: String,
    active: bool,
    published_version: Option<i32>,
    draft_changed: bool,
    draft_valid: bool,
    last_status: Option<String>,
    last_at: Option<DateTime<Utc>>,
    ok_24h: i64,
    failed_24h: i64,
    tags: Vec<String>,
    archived_at: Option<DateTime<Utc>>,
    updated_at: DateTime<Utc>,
}

impl From<ListRow> for WorkflowListItem {
    fn from(r: ListRow) -> Self {
        WorkflowListItem {
            id: r.id,
            name: r.name,
            description: r.description,
            active: r.active,
            published_version: r.published_version,
            draft_changed: r.draft_changed,
            draft_valid: r.draft_valid,
            last_run: r.last_status.zip(r.last_at).map(|(status, at)| LastRun { status, at }),
            runs_24h: Runs24h { success: r.ok_24h, error: r.failed_24h },
            tags: r.tags,
            archived_at: r.archived_at,
            updated_at: r.updated_at,
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct WorkflowListReply {
    pub workflows: Vec<WorkflowListItem>,
    /// Pass this as `cursor` for the next page. `None`: no more.
    pub next_cursor: Option<Uuid>,
}

#[derive(Serialize, ToSchema)]
pub struct PublishedInfo {
    pub version: i32,
    pub note: String,
    pub at: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
pub struct WorkflowDetail {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub active: bool,
    pub tags: Vec<String>,
    pub archived_at: Option<DateTime<Utc>>,
    /// The workflow definition you edit (see section 4 of the build map). May hold errors while you build.
    #[schema(value_type = Object)]
    pub draft: Value,
    /// Send it back as `if_hash` when you save, so a change from another tab is not lost.
    pub draft_hash: String,
    pub validation: ValidationResult,
    pub published: Option<PublishedInfo>,
    pub run_as_user_id: Option<Uuid>,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(FromRow)]
struct DetailRow {
    id: Uuid,
    name: String,
    description: String,
    active: bool,
    tags: Vec<String>,
    archived_at: Option<DateTime<Utc>>,
    draft: Value,
    draft_hash: String,
    run_as_user_id: Option<Uuid>,
    created_by: Option<Uuid>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    pub_version: Option<i32>,
    pub_note: Option<String>,
    pub_at: Option<DateTime<Utc>>,
}

const DETAIL_SQL: &str = "SELECT w.id, w.name, w.description, w.active, w.tags, w.archived_at, w.draft, w.draft_hash, \
    w.run_as_user_id, w.created_by, w.created_at, w.updated_at, \
    v.version AS pub_version, v.note AS pub_note, v.published_at AS pub_at \
    FROM automation_workflows w LEFT JOIN automation_versions v ON v.id = w.published_version_id WHERE w.id = $1";

pub(super) async fn load_detail(st: &AppState, id: Uuid, is_admin: bool) -> ApiResult<WorkflowDetail> {
    let row: Option<DetailRow> = sqlx::query_as(DETAIL_SQL).bind(id).fetch_optional(&st.pool).await?;
    let r = row.ok_or_else(not_found)?;
    let validation = validate_draft(st, &r.draft, Mode::Draft, is_admin).await?;
    let published = r.pub_version.zip(r.pub_at).map(|(version, at)| PublishedInfo { version, note: r.pub_note.unwrap_or_default(), at });
    Ok(WorkflowDetail {
        id: r.id,
        name: r.name,
        description: r.description,
        active: r.active,
        tags: r.tags,
        archived_at: r.archived_at,
        draft: r.draft,
        draft_hash: r.draft_hash,
        validation,
        published,
        run_as_user_id: r.run_as_user_id,
        created_by: r.created_by,
        created_at: r.created_at,
        updated_at: r.updated_at,
    })
}

pub(super) fn not_found() -> ApiError {
    ApiError::not_found("workflow_not_found", "There is no workflow with this id.")
}

// ---------- list ----------

#[derive(Deserialize, IntoParams)]
pub struct ListQuery {
    /// Text in the name or description.
    q: Option<String>,
    /// `true`: only active workflows. `false`: only inactive.
    active: Option<bool>,
    tag: Option<String>,
    /// `true`: only archived workflows. Default `false`: archived ones are hidden.
    archived: Option<bool>,
    /// The `next_cursor` of the last page.
    cursor: Option<Uuid>,
    /// 1 to 200. Default 50.
    limit: Option<i64>,
}

/// List workflows, newest first.
#[utoipa::path(get, path = "/v1/automations/workflows", tag = "automations", params(ListQuery),
    responses((status = 200, body = WorkflowListReply)),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. Newest first. Archived workflows are hidden unless `archived=true`.")]
async fn list_workflows(State(st): State<AppState>, ApiQuery(q): ApiQuery<ListQuery>) -> ApiResult<ApiJson<WorkflowListReply>> {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let pattern = q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(crate::leads::query::like_pattern);
    let rows: Vec<ListRow> = sqlx::query_as(
        "SELECT w.id, w.name, w.description, w.active, v.version AS published_version, \
            (v.id IS NOT NULL AND w.draft IS DISTINCT FROM v.definition) AS draft_changed, w.draft_valid, \
            lr.status AS last_status, lr.at AS last_at, \
            COALESCE(rc.ok, 0) AS ok_24h, COALESCE(rc.failed, 0) AS failed_24h, \
            w.tags, w.archived_at, w.updated_at \
         FROM automation_workflows w \
         LEFT JOIN automation_versions v ON v.id = w.published_version_id \
         LEFT JOIN LATERAL (SELECT r.status, COALESCE(r.finished_at, r.started_at, r.queued_at) AS at \
                            FROM automation_runs r WHERE r.workflow_id = w.id ORDER BY r.queued_at DESC LIMIT 1) lr ON true \
         LEFT JOIN LATERAL (SELECT count(*) FILTER (WHERE r.status = 'success') AS ok, count(*) FILTER (WHERE r.status = 'error') AS failed \
                            FROM automation_runs r WHERE r.workflow_id = w.id AND r.mode = 'production' \
                              AND r.queued_at > now() - interval '24 hours') rc ON true \
         WHERE (w.archived_at IS NOT NULL) = $1 \
           AND ($2::bool IS NULL OR w.active = $2) \
           AND ($3::text IS NULL OR $3 = ANY (w.tags)) \
           AND ($4::text IS NULL OR w.name ILIKE $4 OR w.description ILIKE $4) \
           AND ($5::uuid IS NULL OR w.id < $5) \
         ORDER BY w.id DESC LIMIT $6",
    )
    .bind(q.archived.unwrap_or(false))
    .bind(q.active)
    .bind(q.tag.as_deref().map(str::trim).filter(|t| !t.is_empty()))
    .bind(pattern)
    .bind(q.cursor)
    .bind(limit + 1)
    .fetch_all(&st.pool)
    .await?;
    let more = rows.len() as i64 > limit;
    let workflows: Vec<WorkflowListItem> = rows.into_iter().take(limit as usize).map(Into::into).collect();
    let next_cursor = if more { workflows.last().map(|w| w.id) } else { None };
    Ok(ApiJson(WorkflowListReply { workflows, next_cursor }))
}

// ---------- create ----------

#[derive(Deserialize, ToSchema)]
pub struct CreateWorkflow {
    /// 1 to 120 characters.
    name: String,
    /// A workflow definition. Left out: a draft with one manual trigger. It may have errors.
    #[schema(value_type = Option<Object>)]
    definition: Option<Value>,
    /// Built-in templates come in a later phase. Any value answers `422 unknown_template` for now.
    template: Option<String>,
}

fn check_name(name: &str) -> ApiResult<String> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > 120 {
        return Err(ApiError::unprocessable("invalid_name", "The name must be 1 to 120 characters."));
    }
    Ok(n.to_string())
}

fn check_draft(draft: &Value) -> ApiResult<()> {
    if !draft.is_object() {
        return Err(ApiError::unprocessable("invalid_format", "The draft must be one JSON object."));
    }
    if draft.to_string().len() > MAX_DRAFT_BYTES {
        return Err(ApiError::unprocessable("draft_too_large", "The draft is bigger than 5 MB."));
    }
    Ok(())
}

fn check_tags(tags: &[String]) -> ApiResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for t in tags {
        let t = t.trim();
        if t.is_empty() || t.chars().count() > 40 {
            return Err(ApiError::unprocessable("invalid_tag", "A tag is 1 to 40 characters."));
        }
        if !out.iter().any(|x| x == t) {
            out.push(t.to_string());
        }
    }
    if out.len() > MAX_TAGS {
        return Err(ApiError::unprocessable("invalid_tag", format!("A workflow can have up to {MAX_TAGS} tags.")));
    }
    Ok(out)
}

/// The name column follows the name in the draft.
pub(super) fn draft_name(draft: &Value, fallback: &str) -> String {
    draft.get("name").and_then(Value::as_str).map(str::trim).filter(|n| !n.is_empty() && n.chars().count() <= 120).unwrap_or(fallback).to_string()
}

pub(super) async fn insert_history(tx: &mut sqlx::PgConnection, workflow_id: Uuid, draft: &Value, hash: &str, source: &str, user: Option<Uuid>) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO automation_draft_history (workflow_id, draft, draft_hash, source, created_by) VALUES ($1, $2, $3, $4, $5)")
        .bind(workflow_id)
        .bind(draft)
        .bind(hash)
        .bind(source)
        .bind(user)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "DELETE FROM automation_draft_history WHERE workflow_id = $1 \
         AND id NOT IN (SELECT id FROM automation_draft_history WHERE workflow_id = $1 ORDER BY id DESC LIMIT $2)",
    )
    .bind(workflow_id)
    .bind(KEEP_DRAFT_HISTORY)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Make a workflow row with its first history entry.
pub(super) async fn insert_workflow(st: &AppState, name: &str, draft: Value, tags: &[String], source: &str, c: &Caller) -> ApiResult<Uuid> {
    check_draft(&draft)?;
    let id = Uuid::now_v7();
    let hash = draft_hash(&draft);
    let valid = validate_draft(st, &draft, Mode::Draft, c.is_admin).await?.ok;
    let description = draft.get("description").and_then(Value::as_str).unwrap_or("").to_string();
    let mut tx = st.pool.begin().await?;
    sqlx::query(
        "INSERT INTO automation_workflows (id, name, description, draft, draft_hash, draft_valid, tags, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(id)
    .bind(name)
    .bind(description)
    .bind(&draft)
    .bind(&hash)
    .bind(valid)
    .bind(tags)
    .bind(c.user_id)
    .execute(&mut *tx)
    .await?;
    insert_history(&mut tx, id, &draft, &hash, source, c.user_id).await?;
    tx.commit().await?;
    Ok(id)
}

/// Make a workflow.
#[utoipa::path(post, path = "/v1/automations/workflows", tag = "automations", request_body = CreateWorkflow,
    responses((status = 201, body = WorkflowDetail), (status = 422, description = "invalid_name, invalid_format, draft_too_large, unknown_template")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. The new workflow is a draft: not published, not active. The draft is saved even when it has errors; `validation` lists them.")]
async fn create_workflow(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiJson(b): ApiJson<CreateWorkflow>,
) -> ApiResult<(StatusCode, ApiJson<WorkflowDetail>)> {
    let c = caller(&session, &key);
    let name = check_name(&b.name)?;
    if let Some(t) = &b.template {
        return Err(ApiError::unprocessable("unknown_template", format!("There is no template `{t}`. Use `POST /v1/automations/templates/{{id}}/create` to make a workflow from a template.")));
    }
    let imported = b.definition.is_some();
    let mut draft = b.definition.unwrap_or_else(|| WorkflowDefinition::blank(&name).to_value());
    check_draft(&draft)?;
    draft["name"] = json!(name);
    let id = insert_workflow(&st, &name, draft, &[], if imported { "import" } else { "editor" }, &c).await?;
    Ok((StatusCode::CREATED, ApiJson(load_detail(&st, id, c.is_admin).await?)))
}

// ---------- read ----------

/// One workflow with its draft and the check of the draft.
#[utoipa::path(get, path = "/v1/automations/workflows/{id}", tag = "automations", params(("id" = Uuid, Path)),
    responses((status = 200, body = WorkflowDetail), (status = 404, description = "workflow_not_found")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. `validation` is the check of the draft as it is now.")]
async fn get_workflow(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiPath(id): ApiPath<Uuid>,
) -> ApiResult<ApiJson<WorkflowDetail>> {
    Ok(ApiJson(load_detail(&st, id, caller(&session, &key).is_admin).await?))
}

// ---------- save ----------

#[derive(Deserialize, ToSchema)]
pub struct PatchWorkflow {
    /// The new draft (the whole definition). Errors are allowed: you may still be building.
    #[schema(value_type = Option<Object>)]
    draft: Option<Value>,
    /// The `draft_hash` you last saw. If the draft changed since, the save answers `409 draft_conflict`.
    if_hash: Option<String>,
    /// Also written into the draft.
    name: Option<String>,
    /// Also written into the draft.
    description: Option<String>,
    tags: Option<Vec<String>>,
    /// `false` brings an archived workflow back.
    archived: Option<bool>,
}

/// Save the draft, or change the name, description or tags.
#[utoipa::path(patch, path = "/v1/automations/workflows/{id}", tag = "automations", params(("id" = Uuid, Path)), request_body = PatchWorkflow,
    responses((status = 200, body = WorkflowDetail), (status = 404, description = "workflow_not_found"),
        (status = 409, description = "draft_conflict: the draft changed since `if_hash`. The answer holds the newest `draft` and `draft_hash`."),
        (status = 422, description = "invalid_name, invalid_format, invalid_tag, draft_too_large")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. The draft is saved even when it has errors. The last 50 saves are kept as draft history.")]
async fn patch_workflow(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(b): ApiJson<PatchWorkflow>,
) -> ApiResult<Response> {
    let c = caller(&session, &key);
    if let Some(d) = &b.draft {
        check_draft(d)?;
    }
    let name = b.name.as_deref().map(check_name).transpose()?;
    let tags = b.tags.as_deref().map(check_tags).transpose()?;

    let mut tx = st.pool.begin().await?;
    let cur: Option<(String, Value, String, String)> =
        sqlx::query_as("SELECT name, draft, draft_hash, description FROM automation_workflows WHERE id = $1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?;
    let (cur_name, cur_draft, cur_hash, cur_desc) = cur.ok_or_else(not_found)?;
    if b.if_hash.as_deref().is_some_and(|h| h != cur_hash) && (b.draft.is_some() || name.is_some() || b.description.is_some()) {
        let body = json!({ "error": {
            "code": "draft_conflict",
            "message": "The draft changed since you loaded it. Reload it, or save again without `if_hash` to overwrite.",
            "draft": cur_draft, "draft_hash": cur_hash } });
        return Ok((StatusCode::CONFLICT, axum::Json(body)).into_response());
    }

    let mut draft = b.draft.unwrap_or_else(|| cur_draft.clone());
    if let Some(n) = &name {
        draft["name"] = json!(n);
    }
    if let Some(d) = &b.description {
        let d = d.trim();
        if d.chars().count() > 2000 {
            return Err(ApiError::unprocessable("invalid_format", "The description can have up to 2000 characters."));
        }
        draft["description"] = json!(d);
    }
    let hash = draft_hash(&draft);
    if hash != cur_hash {
        let valid = validate_draft(&st, &draft, Mode::Draft, c.is_admin).await?.ok;
        let new_name = draft_name(&draft, &cur_name);
        let new_desc = draft.get("description").and_then(Value::as_str).map_or(cur_desc, str::to_string);
        sqlx::query("UPDATE automation_workflows SET draft = $2, draft_hash = $3, draft_valid = $4, name = $5, description = $6, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(&draft)
            .bind(&hash)
            .bind(valid)
            .bind(new_name)
            .bind(new_desc)
            .execute(&mut *tx)
            .await?;
        insert_history(&mut tx, id, &draft, &hash, "editor", c.user_id).await?;
    }
    if let Some(tags) = &tags {
        sqlx::query("UPDATE automation_workflows SET tags = $2, updated_at = now() WHERE id = $1").bind(id).bind(tags).execute(&mut *tx).await?;
    }
    if b.archived == Some(false) {
        sqlx::query("UPDATE automation_workflows SET archived_at = NULL, updated_at = now() WHERE id = $1").bind(id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(ApiJson(load_detail(&st, id, c.is_admin).await?).into_response())
}

// ---------- delete, duplicate ----------

#[derive(Deserialize, IntoParams)]
pub struct DeleteQuery {
    /// `true`: delete the workflow and its runs for good. Owner and admin only.
    hard: Option<bool>,
}

/// Archive a workflow, or delete it for good.
#[utoipa::path(delete, path = "/v1/automations/workflows/{id}", tag = "automations", params(("id" = Uuid, Path), DeleteQuery),
    responses((status = 204, description = "Archived or deleted"), (status = 403, description = "admin_only: `hard=true` is for owner and admin"), (status = 404, description = "workflow_not_found")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. Without `hard`, the workflow is archived and switched off. Bring it back with `PATCH {\"archived\": false}`.")]
async fn delete_workflow(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(q): ApiQuery<DeleteQuery>,
) -> ApiResult<StatusCode> {
    let c = caller(&session, &key);
    let affected = if q.hard.unwrap_or(false) {
        if !c.is_admin {
            return Err(ApiError::forbidden("admin_only", "Only an owner or an admin can delete a workflow for good."));
        }
        sqlx::query("DELETE FROM automation_workflows WHERE id = $1").bind(id).execute(&st.pool).await?.rows_affected()
    } else {
        // switching off removes the triggers (they are made in a later phase)
        let mut tx = st.pool.begin().await?;
        sqlx::query("DELETE FROM automation_triggers WHERE workflow_id = $1").bind(id).execute(&mut *tx).await?;
        let n = sqlx::query("UPDATE automation_workflows SET archived_at = COALESCE(archived_at, now()), active = false, updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        n
    };
    if affected == 0 {
        return Err(not_found());
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Copy a workflow: a new draft, not published, not active.
#[utoipa::path(post, path = "/v1/automations/workflows/{id}/duplicate", tag = "automations", params(("id" = Uuid, Path)),
    responses((status = 201, body = WorkflowDetail), (status = 404, description = "workflow_not_found")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. The copy is named `<name> (copy)`. Pins are copied. Nothing else is.")]
async fn duplicate_workflow(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiPath(id): ApiPath<Uuid>,
) -> ApiResult<(StatusCode, ApiJson<WorkflowDetail>)> {
    let c = caller(&session, &key);
    let row: Option<(String, Value, Vec<String>)> = sqlx::query_as("SELECT name, draft, tags FROM automation_workflows WHERE id = $1").bind(id).fetch_optional(&st.pool).await?;
    let (name, mut draft, tags) = row.ok_or_else(not_found)?;
    let suffix = " (copy)";
    let base: String = name.chars().take(120 - suffix.len()).collect();
    let new_name = format!("{base}{suffix}");
    draft["name"] = json!(new_name);
    let new_id = insert_workflow(&st, &new_name, draft, &tags, "editor", &c).await?;
    Ok((StatusCode::CREATED, ApiJson(load_detail(&st, new_id, c.is_admin).await?)))
}

// ---------- validate ----------

#[derive(Deserialize, ToSchema)]
pub struct ValidateBody {
    /// A workflow definition. It may be broken: that is what you want to know.
    #[schema(value_type = Object)]
    definition: Value,
    /// `true`: check as for publish (a required variable without a value, or a missing secret, is an error).
    for_publish: Option<bool>,
}

/// Check a workflow definition. Writes nothing.
#[utoipa::path(post, path = "/v1/automations/validate", tag = "automations", request_body = ValidateBody,
    responses((status = 200, body = ValidationResult)),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. Always answers 200. Read `ok`, `errors` and `warnings`. Each issue has a `code`, and often a `node_id` and a `path`.")]
async fn validate_definition(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiJson(b): ApiJson<ValidateBody>,
) -> ApiResult<ApiJson<ValidationResult>> {
    let c = caller(&session, &key);
    let mode = if b.for_publish.unwrap_or(false) { Mode::Publish } else { Mode::Draft };
    Ok(ApiJson(validate_draft(&st, &b.definition, mode, c.is_admin).await?))
}

// ---------- catalog and schema ----------

#[derive(Serialize, ToSchema)]
pub struct NodeCatalog {
    /// One entry per node type: `type`, `label`, `category`, `description`, `params_schema` (JSON Schema with `x-ui` hints),
    /// `ports`, `mode`, `side_effect`, `admin_only`, `output_example`, `docs_notes`.
    #[schema(value_type = Vec<Object>)]
    pub nodes: Vec<Value>,
}

/// Every node type, with its params schema and an output example.
#[utoipa::path(get, path = "/v1/automations/nodes", tag = "automations",
    responses((status = 200, body = NodeCatalog)),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. The editor and LLM tools read this to build forms and to know what each node gives.")]
async fn get_nodes() -> ApiJson<NodeCatalog> {
    ApiJson(NodeCatalog { nodes: Registry::global().specs().map(|s| s.to_json()).collect() })
}

/// The JSON Schema (draft 2020-12) of a workflow definition. Every node's `params` are in a `oneOf` keyed by `type`.
pub fn definition_schema() -> &'static Value {
    static SCHEMA: LazyLock<Value> = LazyLock::new(build_schema);
    &SCHEMA
}

fn build_schema() -> Value {
    let mut schema = serde_json::to_value(schemars::schema_for!(WorkflowDefinition)).expect("a schema is JSON");
    let branches: Vec<Value> = Registry::global()
        .specs()
        .map(|s| {
            json!({
                "title": s.label,
                "description": s.description,
                "properties": { "type": { "const": s.type_id }, "params": s.params_schema },
                "required": ["type"]
            })
        })
        .collect();
    if let Some(node) = schema.pointer_mut("/$defs/Node").and_then(Value::as_object_mut) {
        node.insert("oneOf".into(), Value::Array(branches));
    }
    schema["$id"] = json!("https://aikido.email/schemas/aikido-workflow.v1.json");
    schema["title"] = json!("AIKIDO EMAIL workflow");
    schema
}

/// The JSON Schema of a workflow definition.
#[utoipa::path(get, path = "/v1/automations/schema.json", tag = "automations",
    responses((status = 200, description = "JSON Schema, draft 2020-12", body = Object)),
    security(("api_key" = []), ("session" = [])),
    description = "Any valid key or session. Tools that support structured output can use it directly.")]
async fn get_schema() -> axum::Json<Value> {
    axum::Json(definition_schema().clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_has_one_branch_per_node_type() {
        let s = definition_schema();
        assert!(s["$schema"].as_str().unwrap().contains("2020-12"), "{}", s["$schema"]);
        let branches = s.pointer("/$defs/Node/oneOf").and_then(Value::as_array).expect("Node has a oneOf");
        assert_eq!(branches.len(), Registry::global().type_ids().len());
        assert!(branches.iter().any(|b| b["properties"]["type"]["const"] == "http.request"));
        // the file is a valid schema, and the 4.1 style example passes it
        let v = jsonschema::validator_for(s).expect("the schema is valid");
        let ok = json!({ "format": "aikido-workflow", "version": 1, "name": "T", "nodes": [{ "id": "a", "type": "trigger.manual" }], "edges": [] });
        assert!(v.is_valid(&ok), "{:?}", v.iter_errors(&ok).map(|e| e.to_string()).collect::<Vec<_>>());
        let bad = json!({ "format": "aikido-workflow", "version": 1, "name": "T", "nodes": [{ "id": "a", "type": "web.fetch_page", "params": { "max_chars": 1 } }], "edges": [] });
        assert!(!v.is_valid(&bad), "web.fetch_page needs url");
    }

    #[test]
    fn names_tags_and_drafts_are_checked() {
        assert!(check_name("  ").is_err());
        assert!(check_name(&"x".repeat(121)).is_err());
        assert_eq!(check_name("  Lead research ").unwrap(), "Lead research");
        assert_eq!(check_tags(&["a".into(), "a".into(), " b ".into()]).unwrap(), vec!["a", "b"]);
        assert!(check_tags(&["".into()]).is_err());
        assert!(check_tags(&(0..21).map(|i| format!("t{i}")).collect::<Vec<_>>()).is_err());
        assert!(check_draft(&json!([1])).is_err());
        assert!(check_draft(&json!({})).is_ok());
        assert_eq!(draft_name(&json!({ "name": " New " }), "old"), "New");
        assert_eq!(draft_name(&json!({ "name": "" }), "old"), "old");
        assert_eq!(draft_name(&json!({}), "old"), "old");
    }
}
