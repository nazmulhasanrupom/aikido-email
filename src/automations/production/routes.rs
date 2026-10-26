//! Publish, versions, restore, draft history, activate, deactivate, manual production runs and retry
//! (build map sections 9.1, 9.5, 10.4 and 13).
use super::plan::{self, TriggerPlan};
use super::runs::{self, NewRun};
use super::{events, schedule};
use crate::auth::api_keys::Scope;
use crate::auth::middleware::{KeyScope, require_scope};
use crate::automations::definition::{WorkflowDefinition, draft_hash};
use crate::automations::routes::{Caller, WorkflowDetail, caller, draft_name, insert_history, load_detail, not_found, validate_draft};
use crate::automations::validate::{Mode, ValidationResult};
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::state::AppState;
use crate::users::session::SessionCtx;
use axum::Extension;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgConnection};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

const MAX_NOTE_CHARS: usize = 500;
/// The most hours an event trigger may start in the past (`backfill_hours`).
const MAX_BACKFILL_HOURS: i64 = 168;

pub fn router(state: AppState) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(publish_workflow))
        .routes(routes!(activate_workflow))
        .routes(routes!(deactivate_workflow))
        .routes(routes!(list_versions))
        .routes(routes!(get_version))
        .routes(routes!(restore_version))
        .routes(routes!(list_draft_history))
        .routes(routes!(restore_draft_history))
        .routes(routes!(run_workflow))
        .routes(routes!(retry_run))
        .routes(routes!(cron_preview))
        .route_layer(axum::middleware::from_fn_with_state((state, Scope::Automations), require_scope))
}

fn invalid(code: &str, message: &str, validation: &ValidationResult) -> Response {
    let body = json!({ "error": { "code": code, "message": message, "validation": validation } });
    (StatusCode::UNPROCESSABLE_ENTITY, axum::Json(body)).into_response()
}

#[derive(FromRow)]
struct Wf {
    draft: Value,
    active: bool,
    archived_at: Option<DateTime<Utc>>,
    published_version_id: Option<Uuid>,
}

/// Lock the workflow row for the rest of the transaction.
async fn lock(tx: &mut PgConnection, id: Uuid) -> ApiResult<Wf> {
    let row: Option<Wf> = sqlx::query_as("SELECT draft, active, archived_at, published_version_id FROM automation_workflows WHERE id = $1 FOR UPDATE").bind(id).fetch_optional(tx).await?;
    row.ok_or_else(not_found)
}

fn not_archived(w: &Wf) -> ApiResult<()> {
    if w.archived_at.is_some() {
        return Err(ApiError::conflict("workflow_archived", "This workflow is archived. Bring it back first."));
    }
    Ok(())
}

async fn default_tz(conn: &mut PgConnection) -> Result<String, sqlx::Error> {
    Ok(crate::sending::queue::load_defaults(conn).await?.timezone)
}

// ---------- triggers ----------

#[derive(Serialize, ToSchema)]
pub struct TriggerView {
    pub node_id: String,
    /// `schedule`, `cron`, `webhook` or `event`.
    pub kind: String,
    /// The node type, for example `trigger.lead_bounced`.
    pub node_type: String,
    /// Webhooks: the production URL.
    pub webhook_url: Option<String>,
    pub next_run_at: Option<DateTime<Utc>>,
}

fn webhook_url(st: &AppState, path: &str) -> String {
    format!("{}/hooks/{path}", st.site_url().unwrap_or_default())
}

#[derive(FromRow)]
struct OldTrigger {
    id: Uuid,
    node_id: String,
    kind: String,
    trigger_type: String,
    config: Value,
    next_run_at: Option<DateTime<Utc>>,
    cursor: Option<Value>,
    webhook_path: Option<String>,
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    e.as_database_error().is_some_and(|d| d.code().as_deref() == Some("23505"))
}

/// Make `automation_triggers` match the plans of a version (build map 9.1). A trigger that was there before keeps its row,
/// its event cursor, its generated webhook path and (if its schedule did not change) its next time.
/// `backfill_hours` applies only to triggers that are new.
async fn install_triggers(
    tx: &mut PgConnection,
    st: &AppState,
    workflow_id: Uuid,
    version_id: Uuid,
    def: &WorkflowDefinition,
    backfill_hours: i64,
) -> ApiResult<Vec<TriggerView>> {
    let now = st.now();
    let tz = default_tz(tx).await?;
    let plans = plan::plan_triggers(def, &tz).map_err(|e| ApiError::unprocessable("invalid_trigger", format!("Trigger `{}`: {}", e.node_id, e.message)))?;
    let old: Vec<OldTrigger> =
        sqlx::query_as("SELECT id, node_id, kind, trigger_type, config, next_run_at, cursor, webhook_path FROM automation_triggers WHERE workflow_id = $1").bind(workflow_id).fetch_all(&mut *tx).await?;
    let keep: Vec<&str> = plans.iter().map(|p| p.node_id.as_str()).collect();
    // rows of nodes that are gone, or that changed kind
    for o in &old {
        let same_kind = plans.iter().any(|p| p.node_id == o.node_id && p.kind == o.kind);
        if !keep.contains(&o.node_id.as_str()) || !same_kind {
            sqlx::query("DELETE FROM automation_triggers WHERE id = $1").bind(o.id).execute(&mut *tx).await?;
        }
    }
    let mut out = Vec::new();
    for mut p in plans {
        let before = old.iter().find(|o| o.node_id == p.node_id && o.kind == p.kind);
        if let (Some(o), true) = (before, p.path_generated) {
            // the node has no path of its own: keep the one made before, so the URL does not change on publish
            if let Some(path) = &o.webhook_path {
                p.webhook_path = Some(path.clone());
                p.config["path"] = json!(path);
            }
        }
        let next_run_at = match p.kind {
            "schedule" | "cron" => match before.filter(|o| o.config == p.config).and_then(|o| o.next_run_at) {
                Some(t) => Some(t),
                None => Some(schedule::first_fire(p.kind, &p.config, now).map_err(|m| ApiError::unprocessable("invalid_trigger", format!("Trigger `{}`: {m}", p.node_id)))?),
            },
            _ => None,
        };
        let cursor = match p.kind {
            "event" => match before.filter(|o| o.trigger_type == p.type_id).and_then(|o| o.cursor.clone()) {
                Some(c) => Some(c),
                None => match events::source_of(&p.type_id) {
                    Some(source) => Some(events::initial_cursor(tx, source, now, backfill_hours).await?),
                    None => None,
                },
            },
            _ => None,
        };
        let saved = save_trigger(tx, workflow_id, version_id, before.map(|o| o.id), &p, next_run_at, cursor).await;
        if let Err(e) = saved {
            if is_unique_violation(&e) {
                return Err(ApiError::conflict("webhook_path_taken", format!("The webhook path `{}` is used by another workflow. Change the path of `{}`.", p.webhook_path.as_deref().unwrap_or_default(), p.node_id)));
            }
            return Err(e.into());
        }
        out.push(TriggerView {
            node_id: p.node_id.clone(),
            kind: p.kind.to_string(),
            node_type: p.type_id.clone(),
            webhook_url: p.webhook_path.as_deref().map(|path| webhook_url(st, path)),
            next_run_at,
        });
    }
    Ok(out)
}

async fn save_trigger(
    tx: &mut PgConnection,
    workflow_id: Uuid,
    version_id: Uuid,
    existing: Option<Uuid>,
    p: &TriggerPlan,
    next_run_at: Option<DateTime<Utc>>,
    cursor: Option<Value>,
) -> Result<(), sqlx::Error> {
    match existing {
        Some(id) => {
            sqlx::query(
                "UPDATE automation_triggers SET version_id = $2, trigger_type = $3, event_types = $4, config = $5, next_run_at = $6, cursor = $7, webhook_path = $8, last_error = NULL, rate_limited_at = NULL \
                 WHERE id = $1",
            )
            .bind(id)
            .bind(version_id)
            .bind(&p.type_id)
            .bind(&p.event_types)
            .bind(&p.config)
            .bind(next_run_at)
            .bind(cursor)
            .bind(&p.webhook_path)
            .execute(tx)
            .await?;
        }
        None => {
            sqlx::query(
                "INSERT INTO automation_triggers (id, workflow_id, version_id, node_id, kind, trigger_type, event_types, config, next_run_at, cursor, webhook_path) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
            )
            .bind(Uuid::now_v7())
            .bind(workflow_id)
            .bind(version_id)
            .bind(&p.node_id)
            .bind(p.kind)
            .bind(&p.type_id)
            .bind(&p.event_types)
            .bind(&p.config)
            .bind(next_run_at)
            .bind(cursor)
            .bind(&p.webhook_path)
            .execute(tx)
            .await?;
        }
    }
    Ok(())
}

// ---------- publish ----------

#[derive(Deserialize, ToSchema, Default)]
pub struct PublishBody {
    /// What changed. Up to 500 characters.
    note: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct PublishReply {
    /// The number of the new version.
    pub version: i32,
    pub workflow: WorkflowDetail,
}

/// Freeze the draft as the next version.
#[utoipa::path(post, path = "/v1/automations/workflows/{id}/publish", tag = "automations", params(("id" = Uuid, Path)), request_body(content = PublishBody, description = "Optional"),
    responses((status = 200, body = PublishReply), (status = 404, description = "workflow_not_found"), (status = 409, description = "workflow_archived, webhook_path_taken"),
        (status = 422, description = "workflow_has_errors (the answer holds `validation`), invalid_trigger, invalid_note")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. Checks the draft as for production (a required variable needs a value, secrets must exist, a member may not publish an admin-only node), \
        copies it into a new version and makes it the published one. Production runs use the published version. \
        If the workflow is active, its triggers switch to the new version at once; event triggers keep their place. Runs that are queued already keep their old version. \
        The user who publishes is the user the production runs act as. Publish with a session to run with your rights; an API key runs as a member.")]
async fn publish_workflow(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiPath(id): ApiPath<Uuid>,
    body: Bytes,
) -> ApiResult<Response> {
    let c = caller(&session, &key);
    let b: PublishBody = crate::error::parse_optional_json(&body)?;
    let note = b.note.as_deref().map(str::trim).unwrap_or_default().to_string();
    if note.chars().count() > MAX_NOTE_CHARS {
        return Err(ApiError::unprocessable("invalid_note", format!("The note can have up to {MAX_NOTE_CHARS} characters.")));
    }
    let mut tx = st.pool.begin().await?;
    let w = lock(&mut tx, id).await?;
    not_archived(&w)?;
    let validation = validate_draft(&st, &w.draft, Mode::Publish, c.is_admin).await?;
    if !validation.ok {
        return Ok(invalid("workflow_has_errors", "The workflow has errors. Fix them before you publish.", &validation));
    }
    let def: WorkflowDefinition = serde_json::from_value(w.draft.clone()).map_err(|e| ApiError::unprocessable("invalid_format", e.to_string()))?;
    let version: i32 = sqlx::query_scalar("SELECT COALESCE(max(version), 0) + 1 FROM automation_versions WHERE workflow_id = $1").bind(id).fetch_one(&mut *tx).await?;
    let version_id = Uuid::now_v7();
    sqlx::query("INSERT INTO automation_versions (id, workflow_id, version, definition, note, published_by, published_at) VALUES ($1, $2, $3, $4, $5, $6, $7)")
        .bind(version_id)
        .bind(id)
        .bind(version)
        .bind(&w.draft)
        .bind(&note)
        .bind(c.user_id)
        .bind(st.now())
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE automation_workflows SET published_version_id = $2, run_as_user_id = $3, updated_at = now() WHERE id = $1").bind(id).bind(version_id).bind(c.user_id).execute(&mut *tx).await?;
    if w.active {
        install_triggers(&mut tx, &st, id, version_id, &def, 0).await?;
    } else {
        // refuse a version whose triggers could not be switched on later
        let tz = default_tz(&mut tx).await?;
        plan::plan_triggers(&def, &tz).map_err(|e| ApiError::unprocessable("invalid_trigger", format!("Trigger `{}`: {}", e.node_id, e.message)))?;
    }
    tx.commit().await?;
    Ok(ApiJson(PublishReply { version, workflow: load_detail(&st, id, c.is_admin).await? }).into_response())
}

// ---------- activate, deactivate ----------

#[derive(Deserialize, ToSchema, Default)]
pub struct ActivateBody {
    /// Event triggers also run for events of the last N hours (0 to 168). Default 0: only new events.
    backfill_hours: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct ActivateReply {
    pub workflow: WorkflowDetail,
    /// The triggers that are switched on.
    pub triggers: Vec<TriggerView>,
}

/// Switch the triggers of the published version on.
#[utoipa::path(post, path = "/v1/automations/workflows/{id}/activate", tag = "automations", params(("id" = Uuid, Path)), request_body(content = ActivateBody, description = "Optional"),
    responses((status = 200, body = ActivateReply), (status = 404, description = "workflow_not_found"),
        (status = 409, description = "not_published, workflow_archived, webhook_path_taken"), (status = 422, description = "workflow_has_errors, invalid_trigger, invalid_backfill")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. Needs a published version that passes the publish checks. Event triggers start at the newest event now (no old events), \
        unless `backfill_hours` goes back. A schedule, cron or webhook trigger starts at once. A workflow with only a manual trigger can be active too: that lets `POST .../run` start it.")]
async fn activate_workflow(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiPath(id): ApiPath<Uuid>,
    body: Bytes,
) -> ApiResult<Response> {
    let c = caller(&session, &key);
    let b: ActivateBody = crate::error::parse_optional_json(&body)?;
    let backfill = b.backfill_hours.unwrap_or(0);
    if !(0..=MAX_BACKFILL_HOURS).contains(&backfill) {
        return Err(ApiError::unprocessable("invalid_backfill", format!("`backfill_hours` must be 0 to {MAX_BACKFILL_HOURS}.")));
    }
    let mut tx = st.pool.begin().await?;
    let w = lock(&mut tx, id).await?;
    not_archived(&w)?;
    let Some(version_id) = w.published_version_id else {
        return Err(ApiError::conflict("not_published", "Publish the workflow first."));
    };
    let definition: Value = sqlx::query_scalar("SELECT definition FROM automation_versions WHERE id = $1").bind(version_id).fetch_one(&mut *tx).await?;
    let validation = validate_draft(&st, &definition, Mode::Publish, c.is_admin).await?;
    if !validation.ok {
        return Ok(invalid("workflow_has_errors", "The published version has errors now (a secret may be gone). Fix the draft and publish again.", &validation));
    }
    let def: WorkflowDefinition = serde_json::from_value(definition).map_err(|e| ApiError::unprocessable("invalid_format", e.to_string()))?;
    let triggers = install_triggers(&mut tx, &st, id, version_id, &def, backfill).await?;
    sqlx::query("UPDATE automation_workflows SET active = true, updated_at = now() WHERE id = $1").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(ApiJson(ActivateReply { workflow: load_detail(&st, id, c.is_admin).await?, triggers }).into_response())
}

#[derive(Deserialize, ToSchema, Default)]
pub struct DeactivateBody {
    /// `true`: also cancel the production runs that are queued, waiting or running. Default `false`: they go on.
    cancel_runs: Option<bool>,
}

/// Switch the triggers off.
#[utoipa::path(post, path = "/v1/automations/workflows/{id}/deactivate", tag = "automations", params(("id" = Uuid, Path)), request_body(content = DeactivateBody, description = "Optional"),
    responses((status = 200, body = WorkflowDetail), (status = 404, description = "workflow_not_found")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. Removes the triggers. Runs that are queued or running go on, unless `cancel_runs` is true.")]
async fn deactivate_workflow(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiPath(id): ApiPath<Uuid>,
    body: Bytes,
) -> ApiResult<ApiJson<WorkflowDetail>> {
    let c = caller(&session, &key);
    let b: DeactivateBody = crate::error::parse_optional_json(&body)?;
    let mut tx = st.pool.begin().await?;
    lock(&mut tx, id).await?;
    sqlx::query("DELETE FROM automation_triggers WHERE workflow_id = $1").bind(id).execute(&mut *tx).await?;
    sqlx::query("UPDATE automation_workflows SET active = false, updated_at = now() WHERE id = $1").bind(id).execute(&mut *tx).await?;
    if b.cancel_runs.unwrap_or(false) {
        sqlx::query(
            "UPDATE automation_runs SET cancel_requested = true, \
               status = CASE WHEN status IN ('queued', 'waiting') THEN 'cancelled' ELSE status END, \
               finished_at = CASE WHEN status IN ('queued', 'waiting') THEN $2 ELSE finished_at END, \
               state = CASE WHEN status IN ('queued', 'waiting') THEN NULL ELSE state END \
             WHERE workflow_id = $1 AND mode = 'production' AND status IN ('queued', 'waiting', 'running')",
        )
        .bind(id)
        .bind(st.now())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(ApiJson(load_detail(&st, id, c.is_admin).await?))
}

// ---------- versions ----------

#[derive(Serialize, ToSchema)]
pub struct VersionInfo {
    pub version: i32,
    pub note: String,
    pub published_by: Option<Uuid>,
    pub published_at: DateTime<Utc>,
    /// This is the version production runs use.
    pub is_published: bool,
}

#[derive(Serialize, ToSchema)]
pub struct VersionList {
    /// Newest first.
    pub versions: Vec<VersionInfo>,
}

#[derive(FromRow)]
struct VersionRow {
    version: i32,
    note: String,
    published_by: Option<Uuid>,
    published_at: DateTime<Utc>,
    is_published: bool,
}

impl From<VersionRow> for VersionInfo {
    fn from(r: VersionRow) -> Self {
        VersionInfo { version: r.version, note: r.note, published_by: r.published_by, published_at: r.published_at, is_published: r.is_published }
    }
}

/// The published versions of a workflow.
#[utoipa::path(get, path = "/v1/automations/workflows/{id}/versions", tag = "automations", params(("id" = Uuid, Path)),
    responses((status = 200, body = VersionList), (status = 404, description = "workflow_not_found")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. Newest first.")]
async fn list_versions(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<VersionList>> {
    let exists: Option<i32> = sqlx::query_scalar("SELECT 1 FROM automation_workflows WHERE id = $1").bind(id).fetch_optional(&st.pool).await?;
    exists.ok_or_else(not_found)?;
    let rows: Vec<VersionRow> = sqlx::query_as(
        "SELECT v.version, v.note, v.published_by, v.published_at, (w.published_version_id = v.id) AS is_published \
           FROM automation_versions v JOIN automation_workflows w ON w.id = v.workflow_id WHERE v.workflow_id = $1 ORDER BY v.version DESC",
    )
    .bind(id)
    .fetch_all(&st.pool)
    .await?;
    Ok(ApiJson(VersionList { versions: rows.into_iter().map(Into::into).collect() }))
}

#[derive(Serialize, ToSchema)]
pub struct VersionDetail {
    #[serde(flatten)]
    pub info: VersionInfo,
    /// The frozen definition.
    #[schema(value_type = Object)]
    pub definition: Value,
}

async fn load_version(st: &AppState, id: Uuid, version: i32) -> ApiResult<VersionDetail> {
    let row: Option<(i32, String, Option<Uuid>, DateTime<Utc>, bool, Value)> = sqlx::query_as(
        "SELECT v.version, v.note, v.published_by, v.published_at, (w.published_version_id = v.id), v.definition \
           FROM automation_versions v JOIN automation_workflows w ON w.id = v.workflow_id WHERE v.workflow_id = $1 AND v.version = $2",
    )
    .bind(id)
    .bind(version)
    .fetch_optional(&st.pool)
    .await?;
    let (version, note, published_by, published_at, is_published, definition) = row.ok_or_else(|| ApiError::not_found("version_not_found", "This workflow has no such version."))?;
    Ok(VersionDetail { info: VersionInfo { version, note, published_by, published_at, is_published }, definition })
}

/// One version with its definition.
#[utoipa::path(get, path = "/v1/automations/workflows/{id}/versions/{version}", tag = "automations", params(("id" = Uuid, Path), ("version" = i32, Path)),
    responses((status = 200, body = VersionDetail), (status = 404, description = "version_not_found")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`.")]
async fn get_version(State(st): State<AppState>, ApiPath((id, version)): ApiPath<(Uuid, i32)>) -> ApiResult<ApiJson<VersionDetail>> {
    Ok(ApiJson(load_version(&st, id, version).await?))
}

/// Replace the draft of a workflow with another draft (restore). Writes a draft history entry.
async fn set_draft(st: &AppState, tx: &mut PgConnection, id: Uuid, draft: Value, source: &str, c: &Caller) -> ApiResult<()> {
    let (cur_name, cur_desc): (String, String) = sqlx::query_as("SELECT name, description FROM automation_workflows WHERE id = $1").bind(id).fetch_one(&mut *tx).await?;
    let hash = draft_hash(&draft);
    let valid = validate_draft(st, &draft, Mode::Draft, c.is_admin).await?.ok;
    let name = draft_name(&draft, &cur_name);
    let description = draft.get("description").and_then(Value::as_str).map_or(cur_desc, str::to_string);
    sqlx::query("UPDATE automation_workflows SET draft = $2, draft_hash = $3, draft_valid = $4, name = $5, description = $6, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(&draft)
        .bind(&hash)
        .bind(valid)
        .bind(name)
        .bind(description)
        .execute(&mut *tx)
        .await?;
    insert_history(tx, id, &draft, &hash, source, c.user_id).await?;
    Ok(())
}

/// Copy a version into the draft.
#[utoipa::path(post, path = "/v1/automations/workflows/{id}/versions/{version}/restore", tag = "automations", params(("id" = Uuid, Path), ("version" = i32, Path)),
    responses((status = 200, body = WorkflowDetail), (status = 404, description = "workflow_not_found, version_not_found")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. The draft becomes a copy of that version. Nothing is published: press Publish to use it in production. \
        The old draft stays in the draft history.")]
async fn restore_version(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiPath((id, version)): ApiPath<(Uuid, i32)>,
) -> ApiResult<ApiJson<WorkflowDetail>> {
    let c = caller(&session, &key);
    let v = load_version(&st, id, version).await?;
    let mut tx = st.pool.begin().await?;
    lock(&mut tx, id).await?;
    set_draft(&st, &mut tx, id, v.definition, "restore", &c).await?;
    tx.commit().await?;
    Ok(ApiJson(load_detail(&st, id, c.is_admin).await?))
}

// ---------- draft history ----------

#[derive(Serialize, ToSchema)]
pub struct HistoryEntry {
    pub id: i64,
    /// `editor`, `copilot`, `import` or `restore`.
    pub source: String,
    pub draft_hash: String,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
pub struct HistoryList {
    /// Newest first. The last 50 saves.
    pub history: Vec<HistoryEntry>,
}

/// The last 50 saves of the draft.
#[utoipa::path(get, path = "/v1/automations/workflows/{id}/draft-history", tag = "automations", params(("id" = Uuid, Path)),
    responses((status = 200, body = HistoryList), (status = 404, description = "workflow_not_found")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. Newest first. Restore one with `POST .../draft-history/{hid}/restore`.")]
async fn list_draft_history(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<HistoryList>> {
    let exists: Option<i32> = sqlx::query_scalar("SELECT 1 FROM automation_workflows WHERE id = $1").bind(id).fetch_optional(&st.pool).await?;
    exists.ok_or_else(not_found)?;
    let rows: Vec<(i64, String, String, Option<Uuid>, DateTime<Utc>)> =
        sqlx::query_as("SELECT id, source, draft_hash, created_by, created_at FROM automation_draft_history WHERE workflow_id = $1 ORDER BY id DESC LIMIT 50").bind(id).fetch_all(&st.pool).await?;
    Ok(ApiJson(HistoryList { history: rows.into_iter().map(|(id, source, draft_hash, created_by, created_at)| HistoryEntry { id, source, draft_hash, created_by, created_at }).collect() }))
}

/// Put an earlier draft back.
#[utoipa::path(post, path = "/v1/automations/workflows/{id}/draft-history/{hid}/restore", tag = "automations", params(("id" = Uuid, Path), ("hid" = i64, Path)),
    responses((status = 200, body = WorkflowDetail), (status = 404, description = "workflow_not_found, history_not_found")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. The draft becomes the saved draft with that id. The draft before the restore is saved in the history too.")]
async fn restore_draft_history(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiPath((id, hid)): ApiPath<(Uuid, i64)>,
) -> ApiResult<ApiJson<WorkflowDetail>> {
    let c = caller(&session, &key);
    let mut tx = st.pool.begin().await?;
    lock(&mut tx, id).await?;
    let old: Option<Value> = sqlx::query_scalar("SELECT draft FROM automation_draft_history WHERE id = $1 AND workflow_id = $2").bind(hid).bind(id).fetch_optional(&mut *tx).await?;
    let old = old.ok_or_else(|| ApiError::not_found("history_not_found", "This workflow has no such saved draft."))?;
    set_draft(&st, &mut tx, id, old, "restore", &c).await?;
    tx.commit().await?;
    Ok(ApiJson(load_detail(&st, id, c.is_admin).await?))
}

// ---------- manual production run, retry ----------

#[derive(Deserialize, ToSchema, Default)]
pub struct RunBody {
    /// Becomes `trigger.input` in the workflow. Must be a JSON object.
    #[schema(value_type = Option<Object>)]
    input: Option<Value>,
    /// Which manual trigger to start. Default: the first one.
    trigger_node_id: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct StartedProductionRun {
    pub run_id: Uuid,
}

fn busy(what: &str) -> ApiError {
    ApiError::too_many("rate_limited", format!("{what} Try again later."), 60)
}

/// Start a production run by hand (or from another tool).
#[utoipa::path(post, path = "/v1/automations/workflows/{id}/run", tag = "automations", params(("id" = Uuid, Path)), request_body(content = RunBody, description = "Optional"),
    responses((status = 202, body = StartedProductionRun), (status = 404, description = "workflow_not_found"),
        (status = 409, description = "workflow_not_active"), (status = 422, description = "invalid_trigger_node, invalid_input"), (status = 429, description = "rate_limited: `max_runs_per_hour` is reached or the queue is full")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. Needs an active workflow with a manual trigger. Runs the published version, for real, as the user who published it. \
        This is how other tools (or n8n) start an AIKIDO workflow with an API key. `input` becomes `trigger.input`.")]
async fn run_workflow(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiPath(id): ApiPath<Uuid>,
    body: Bytes,
) -> ApiResult<(StatusCode, ApiJson<StartedProductionRun>)> {
    let c = caller(&session, &key);
    let b: RunBody = crate::error::parse_optional_json(&body)?;
    let input = match b.input {
        None | Some(Value::Null) => json!({}),
        Some(v @ Value::Object(_)) => v,
        Some(_) => return Err(ApiError::unprocessable("invalid_input", "`input` must be a JSON object.")),
    };
    let mut tx = st.pool.begin().await?;
    let row: Option<(bool, Option<DateTime<Utc>>, Option<Uuid>, Option<Uuid>)> =
        sqlx::query_as("SELECT active, archived_at, published_version_id, run_as_user_id FROM automation_workflows WHERE id = $1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?;
    let (active, archived, version_id, run_as) = row.ok_or_else(not_found)?;
    let Some(version_id) = version_id.filter(|_| active && archived.is_none()) else {
        return Err(ApiError::conflict("workflow_not_active", "Publish and activate the workflow first."));
    };
    let definition: Value = sqlx::query_scalar("SELECT definition FROM automation_versions WHERE id = $1").bind(version_id).fetch_one(&mut *tx).await?;
    let def: WorkflowDefinition = serde_json::from_value(definition).map_err(|e| ApiError::unprocessable("invalid_format", e.to_string()))?;
    let trigger = match &b.trigger_node_id {
        Some(t) => def.node(t).filter(|n| n.type_id == "trigger.manual"),
        None => def.nodes.iter().find(|n| n.type_id == "trigger.manual" && !n.disabled),
    };
    let Some(trigger) = trigger else {
        return Err(ApiError::unprocessable("invalid_trigger_node", "The published version has no manual trigger (or not that one)."));
    };
    let now = st.now();
    let lim = runs::limits(&mut tx, id, runs::max_per_hour(def.settings.max_runs_per_hour.map(|n| n as i32)), now).await?;
    if !lim.open() {
        return Err(busy(if lim.backlog { "This workflow has too many runs waiting." } else { "This workflow reached `max_runs_per_hour`." }));
    }
    let item = json!({ "started_by": c.user_id, "input": input });
    let run = NewRun { workflow_id: id, version_id, trigger_node_id: &trigger.id, trigger_item: item, run_as_user_id: run_as, started_by: c.user_id, idempotency_key: None };
    let run_id = runs::insert_run(&mut tx, now, run).await?.ok_or_else(ApiError::internal)?;
    tx.commit().await?;
    Ok((StatusCode::ACCEPTED, ApiJson(StartedProductionRun { run_id })))
}

/// Run a failed production run again.
#[utoipa::path(post, path = "/v1/automations/runs/{id}/retry", tag = "automations", params(("id" = Uuid, Path)),
    responses((status = 202, body = StartedProductionRun), (status = 404, description = "run_not_found"),
        (status = 409, description = "run_not_retryable: only a production run in `error` can be retried"), (status = 429, description = "rate_limited")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. Makes a new run with the same trigger item and the same version. The old run stays as it is. \
        Nodes that already sent email or changed data do it again: use this for runs that failed early, and check the old run first.")]
async fn retry_run(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiPath(id): ApiPath<Uuid>,
) -> ApiResult<(StatusCode, ApiJson<StartedProductionRun>)> {
    let c = caller(&session, &key);
    let row: Option<(Uuid, Option<Uuid>, String, Value, String, String, Option<Uuid>, Option<i32>)> = sqlx::query_as(
        "SELECT r.workflow_id, r.version_id, r.mode, r.trigger_item, r.status, r.trigger_node_id, r.run_as_user_id, \
                (v.definition -> 'settings' ->> 'max_runs_per_hour')::int \
           FROM automation_runs r LEFT JOIN automation_versions v ON v.id = r.version_id WHERE r.id = $1",
    )
    .bind(id)
    .fetch_optional(&st.pool)
    .await?;
    let (workflow_id, version_id, mode, item, status, node, run_as, max) = row.ok_or_else(|| ApiError::not_found("run_not_found", "There is no run with this id."))?;
    let Some(version_id) = version_id.filter(|_| mode == "production" && status == "error") else {
        return Err(ApiError::conflict("run_not_retryable", "Only a production run that ended in `error` can be retried."));
    };
    let mut tx = st.pool.begin().await?;
    let now = st.now();
    let lim = runs::limits(&mut tx, workflow_id, runs::max_per_hour(max), now).await?;
    if !lim.open() {
        return Err(busy(if lim.backlog { "This workflow has too many runs waiting." } else { "This workflow reached `max_runs_per_hour`." }));
    }
    let run = NewRun { workflow_id, version_id, trigger_node_id: &node, trigger_item: item, run_as_user_id: run_as, started_by: c.user_id, idempotency_key: None };
    let run_id = runs::insert_run(&mut tx, now, run).await?.ok_or_else(ApiError::internal)?;
    tx.commit().await?;
    Ok((StatusCode::ACCEPTED, ApiJson(StartedProductionRun { run_id })))
}

// ---------- cron preview ----------

#[derive(Deserialize, IntoParams)]
pub struct CronQuery {
    /// 5 fields: minute, hour, day of month, month, day of week.
    expression: String,
    /// IANA name. Default: the sending default time zone.
    timezone: Option<String>,
    /// How many times to show, 1 to 20. Default 5.
    count: Option<usize>,
}

#[derive(Serialize, ToSchema)]
pub struct CronPreview {
    pub timezone: String,
    /// The next times, in UTC.
    pub next: Vec<DateTime<Utc>>,
    /// The same times in the time zone of the trigger, as `2026-10-26 09:00 +06`.
    pub local: Vec<String>,
}

/// The next times a cron expression fires.
#[utoipa::path(get, path = "/v1/automations/cron/preview", tag = "automations", params(CronQuery),
    responses((status = 200, body = CronPreview), (status = 422, description = "invalid_cron, invalid_timezone")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. For the editor: shows the next 5 times of a cron trigger.")]
async fn cron_preview(State(st): State<AppState>, ApiQuery(q): ApiQuery<CronQuery>) -> ApiResult<ApiJson<CronPreview>> {
    let tz_name = match q.timezone.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => t.to_string(),
        None => {
            let mut conn = st.pool.acquire().await?;
            default_tz(&mut conn).await?
        }
    };
    let tz = schedule::parse_tz(&tz_name).map_err(|m| ApiError::unprocessable("invalid_timezone", m))?;
    let next = schedule::cron_preview(&q.expression, tz, st.now(), q.count.unwrap_or(5).clamp(1, 20)).map_err(|m| ApiError::unprocessable("invalid_cron", m))?;
    let local = next.iter().map(|t| t.with_timezone(&tz).format("%Y-%m-%d %H:%M %:z").to_string()).collect();
    Ok(ApiJson(CronPreview { timezone: tz_name, next, local }))
}
