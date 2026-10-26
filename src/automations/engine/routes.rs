//! Test runs, run lists, run events, cancel, engine status and pause (build map section 13).
//! Publish, triggers and webhooks are in `production/`.
use super::graph;
use super::worker;
use crate::auth::api_keys::Scope;
use crate::auth::middleware::{KeyScope, require_scope};
use crate::automations::definition::{Node, WorkflowDefinition};
use crate::automations::registry::Registry;
use crate::automations::production::events as trigger_events;
use crate::automations::routes::{Caller, caller, validate_draft};
use crate::automations::validate::{Mode, ValidationResult};
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::state::AppState;
use crate::users::session::SessionCtx;
use axum::Extension;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sqlx::FromRow;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// The most data (JSON text) a node that runs alone takes from earlier test runs.
const LONE_SEED_BYTES: usize = 2 * 1024 * 1024;

/// A run page shows this many items per port. The rest is read with the node route.
const ITEMS_ON_RUN_PAGE: usize = 20;

pub fn router(state: AppState) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(test_workflow))
        .routes(routes!(list_runs))
        .routes(routes!(get_run))
        .routes(routes!(get_run_node))
        .routes(routes!(get_run_events))
        .routes(routes!(cancel_run))
        .routes(routes!(engine_status))
        .routes(routes!(engine_pause))
        .routes(routes!(engine_resume))
        .route_layer(axum::middleware::from_fn_with_state((state, Scope::Automations), require_scope))
}

fn run_not_found() -> ApiError {
    ApiError::not_found("run_not_found", "There is no run with this id.")
}

// ---------- test run ----------

/// A real lead, campaign, list or message to build the trigger item from, like a real event (`event.id` is `test`).
#[derive(Deserialize, ToSchema, Default)]
pub struct TestEvent {
    /// For the lead triggers: the lead the item is about.
    pub lead_id: Option<Uuid>,
    /// `trigger.lead_added_to_campaign`: the campaign.
    pub campaign_id: Option<Uuid>,
    /// `trigger.lead_added_to_list`: the list.
    pub list_id: Option<Uuid>,
    /// `trigger.reply_received`: a stored inbound message.
    pub message_id: Option<Uuid>,
}

#[derive(Deserialize, ToSchema, Default)]
pub struct TestBody {
    /// The trigger to start from. Default: the manual trigger, else the first trigger.
    pub trigger_node_id: Option<String>,
    /// The item the trigger gives. Default: the `test_event`, else the pin of the trigger, else a sample.
    #[schema(value_type = Option<Object>)]
    pub trigger_item: Option<Value>,
    /// Build the trigger item from a real lead, campaign, list or message, the way a real event would.
    pub test_event: Option<TestEvent>,
    /// Test only this node: the engine runs the path from the trigger to it and stops there. Pins replace the nodes on the way. A node that no trigger reaches runs alone: its input is the output of the node before it (a pin, else the newest test run), else `trigger_item`, else an empty item.
    pub only_node: Option<String>,
    /// `true`: nodes that change data really do it. Default `false`: they say what they would do.
    pub real_writes: Option<bool>,
    /// Values for the workflow variables, for this run only.
    #[schema(value_type = Option<Object>)]
    pub variables: Option<Map<String, Value>>,
}

#[derive(Serialize, ToSchema)]
pub struct StartedRun {
    pub run_id: Uuid,
}

fn invalid(code: &str, message: &str, validation: &ValidationResult) -> Response {
    let body = json!({ "error": { "code": code, "message": message, "validation": validation } });
    (StatusCode::UNPROCESSABLE_ENTITY, axum::Json(body)).into_response()
}

/// Start a test run of the draft.
#[utoipa::path(post, path = "/v1/automations/workflows/{id}/test", tag = "automations", params(("id" = Uuid, Path)), request_body = TestBody,
    responses((status = 202, body = StartedRun), (status = 404, description = "workflow_not_found"),
        (status = 422, description = "workflow_has_errors (the answer holds `validation`), invalid_trigger_node, invalid_only_node, variable_missing, invalid_trigger_item")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. Runs the current draft, active or not. The run is queued; read it with `GET /v1/automations/runs/{id}` and poll `.../events`. \
        Only errors on the tested path stop a test. Pins replace the nodes they belong to. Nodes that change data run dry unless `real_writes` is true.")]
pub async fn test_workflow(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(b): ApiJson<TestBody>,
) -> ApiResult<Response> {
    let c = caller(&session, &key);
    match queue_test(&st, id, b, c.user_id, c.is_admin).await? {
        Ok(run_id) => Ok((StatusCode::ACCEPTED, ApiJson(StartedRun { run_id })).into_response()),
        Err(refused) => Ok(refused),
    }
}

/// Queue a test run of the draft. The routes `.../test` and `/hooks/test/{path}` share it.
/// `Ok(Err(response))`: the draft has errors on the tested path (the response is the 422 answer).
pub(crate) async fn queue_test(st: &AppState, id: Uuid, b: TestBody, user_id: Option<Uuid>, is_admin: bool) -> ApiResult<Result<Uuid, Response>> {
    let c = Caller { user_id, is_admin };
    let st = st.clone();
    let row: Option<(Value,)> = sqlx::query_as("SELECT draft FROM automation_workflows WHERE id = $1").bind(id).fetch_optional(&st.pool).await?;
    let (draft,) = row.ok_or_else(|| ApiError::not_found("workflow_not_found", "There is no workflow with this id."))?;
    let mut validation = validate_draft(&st, &draft, Mode::Draft, c.is_admin).await?;
    let mut def: WorkflowDefinition = match serde_json::from_value(draft.clone()) {
        Ok(d) => d,
        Err(_) => return Ok(Err(invalid("workflow_has_errors", "The draft has errors. Fix them first.", &validation))),
    };

    // a node that no trigger reaches (or a workflow without a trigger) runs alone
    let is_trigger_node = |n: &Node| Registry::global().spec(&n.type_id).is_some_and(|s| s.is_trigger());
    let lone: Option<String> = match &b.only_node {
        Some(only) => match def.node(only) {
            None => return Err(ApiError::unprocessable("invalid_only_node", format!("There is no node `{only}`."))),
            Some(n) if is_trigger_node(n) => None,
            Some(_) => {
                let reaches = |t: &Node| graph::reachable(&def, &t.id).contains(only);
                let wanted = b.trigger_node_id.as_deref().and_then(|t| def.node(t));
                let reached = match wanted {
                    Some(t) => is_trigger_node(t) && reaches(t),
                    None => def.nodes.iter().any(|t| is_trigger_node(t) && reaches(t)),
                };
                if reached { None } else { Some(only.clone()) }
            }
        },
        None => None,
    };

    // the trigger
    let trigger_id = match (&lone, &b.trigger_node_id) {
        (Some(only), _) => only.clone(),
        (None, Some(t)) => t.clone(),
        (None, None) => {
            let is_trigger = |n: &&Node| is_trigger_node(n);
            match &b.only_node {
                // the first trigger that leads to the node
                Some(only) => def.nodes.iter().filter(is_trigger).find(|t| graph::reachable(&def, &t.id).contains(only)).map(|n| n.id.clone()),
                None => def.nodes.iter().find(|n| n.type_id == "trigger.manual").or_else(|| def.nodes.iter().find(is_trigger)).map(|n| n.id.clone()),
            }
            .or_else(|| def.nodes.iter().find(|n| n.type_id == "trigger.manual").or_else(|| def.nodes.iter().find(is_trigger)).map(|n| n.id.clone()))
            .unwrap_or_default()
        }
    };
    let trigger_type = def.node(&trigger_id).map(|n| n.type_id.clone());
    if lone.is_none() {
        if !trigger_type.as_deref().and_then(|t| Registry::global().spec(t)).is_some_and(|s| s.is_trigger()) {
            return Err(ApiError::unprocessable("invalid_trigger_node", format!("`{trigger_id}` is not a trigger node of this workflow.")));
        }
        if let Some(only) = &b.only_node {
            if def.node(only).is_none() || !graph::reachable(&def, &trigger_id).contains(only) {
                return Err(ApiError::unprocessable("invalid_only_node", format!("The trigger `{trigger_id}` does not reach the node `{only}`.")));
            }
        }
    }

    // only the errors on the tested path stop a test run
    let mut path = graph::reachable(&def, &trigger_id);
    if let Some(only) = &b.only_node {
        let up = graph::ancestors(&def, only);
        path.retain(|n| up.contains(n));
    }
    if lone.is_some() {
        validation.errors.retain(|e| e.node_id.as_deref() == lone.as_deref());
    } else {
        validation.errors.retain(|e| e.node_id.as_ref().is_none_or(|n| path.contains(n)));
    }
    validation.ok = validation.errors.is_empty();
    if !validation.ok {
        return Ok(Err(invalid("workflow_has_errors", "The workflow has errors on the tested path. Fix them first.", &validation)));
    }

    // variables
    if let Some(over) = &b.variables {
        for v in &mut def.variables {
            if let Some(x) = over.get(&v.key) {
                v.value = Some(x.clone());
            }
        }
    }
    let missing: Vec<&str> = def.variables.iter().filter(|v| v.required && v.value.as_ref().is_none_or(Value::is_null)).map(|v| v.key.as_str()).collect();
    if !missing.is_empty() {
        return Err(ApiError::unprocessable("variable_missing", format!("Give a value for: {}. Set it in the workflow or send it in `variables`.", missing.join(", "))));
    }

    // a lone node gets the output of the node before it (a pin, else the newest test), else an empty item
    let mut lone_seed = Map::new();
    let mut lone_input: Option<Value> = None;
    let mut lone_item: Option<Map<String, Value>> = None;
    if let Some(only) = &lone {
        let mut ups = graph::ancestors(&def, only);
        ups.remove(only);
        let mut ids: Vec<&String> = ups.iter().collect();
        ids.sort();
        for nid in &ids {
            if let Some(pin) = def.pins.get(*nid).filter(|p| !p.is_empty()) {
                let port = def.node(nid).and_then(|n| Registry::global().spec(&n.type_id)).and_then(|s| s.ports.first().copied()).unwrap_or("main");
                lone_seed.insert((*nid).clone(), json!({ port: pin }));
            }
        }
        let want: Vec<String> = ids.iter().filter(|n| !lone_seed.contains_key(n.as_str())).map(|n| (*n).clone()).collect();
        if !want.is_empty() {
            let rows: Vec<(String, Value)> = sqlx::query_as(
                "SELECT DISTINCT ON (n.node_id) n.node_id, n.output FROM automation_run_nodes n JOIN automation_runs r ON r.id = n.run_id \
                 WHERE r.workflow_id = $1 AND r.mode = 'test' AND n.node_id = ANY($2) AND n.output IS NOT NULL AND n.status IN ('success', 'dry') \
                 ORDER BY n.node_id, r.queued_at DESC, n.seq DESC",
            )
            .bind(id)
            .bind(&want)
            .fetch_all(&st.pool)
            .await?;
            let mut size = 0usize;
            for (nid, out) in rows {
                size += out.to_string().len();
                if size > LONE_SEED_BYTES {
                    break;
                }
                lone_seed.insert(nid, out);
            }
        }
        for e in def.edges.iter().filter(|e| &e.to == only) {
            let first = lone_seed.get(&e.from).and_then(|o| o.get(e.port())).and_then(Value::as_array).and_then(|a| a.first()).and_then(Value::as_object);
            if let Some(first) = first {
                lone_item = Some(first.clone());
                lone_input = Some(json!({ "node": e.from, "port": e.port() }));
                break;
            }
        }
    }

    // the item the trigger gives
    let trigger_item: Map<String, Value> = match &b.trigger_item {
        Some(Value::Object(o)) => o.clone(),
        Some(_) => return Err(ApiError::unprocessable("invalid_trigger_item", "`trigger_item` must be a JSON object.")),
        None if lone.is_some() => lone_item.unwrap_or_default(),
        None => {
            // a real lead, campaign, list or message the user picked
            let from_event = match (&b.test_event, trigger_type.as_deref()) {
                (Some(ev), Some(t)) if trigger_events::source_of(t).is_some() => {
                    let item = trigger_events::test_item(&st, t, ev.lead_id, ev.campaign_id, ev.list_id, ev.message_id).await?;
                    Some(item.ok_or_else(|| ApiError::unprocessable("invalid_test_event", "The lead, campaign, list or message in `test_event` was not found, or does not fit this trigger."))?)
                }
                _ => None,
            };
            match from_event {
                Some(item) => item,
                None => match def.pins.get(&trigger_id).and_then(|p| p.first()) {
                    Some(p) => p.clone(),
                    None if trigger_type.as_deref() == Some("trigger.manual") => {
                        json!({ "started_by": c.user_id, "input": {} }).as_object().cloned().unwrap_or_default()
                    }
                    None => Registry::global().spec(trigger_type.as_deref().unwrap_or_default()).and_then(|s| s.output_example.as_object().cloned()).unwrap_or_default(),
                },
            }
        }
    };

    let run_id = Uuid::now_v7();
    let snapshot = def.to_value();
    sqlx::query(
        "INSERT INTO automation_runs (id, workflow_id, definition, mode, status, trigger_node_id, trigger_item, options, started_by, run_as_user_id, queued_at) \
         VALUES ($1, $2, $3, 'test', 'queued', $4, $5, $6, $7, $7, $8)",
    )
    .bind(run_id)
    .bind(id)
    .bind(snapshot)
    .bind(&trigger_id)
    .bind(Value::Object(trigger_item))
    .bind(json!({ "real_writes": b.real_writes.unwrap_or(false), "only_node": b.only_node, "lone_seed": lone_seed, "lone_input": lone_input }))
    .bind(c.user_id)
    .bind(st.now())
    .execute(&st.pool)
    .await?;
    Ok(Ok(run_id))
}

// ---------- runs ----------

#[derive(Serialize, ToSchema)]
pub struct RunSummary {
    pub id: Uuid,
    pub workflow_id: Uuid,
    /// The published version of a production run. `None` for test runs.
    pub version: Option<i32>,
    /// `test` or `production`.
    pub mode: String,
    /// `queued`, `running`, `waiting`, `success`, `error` or `cancelled`.
    pub status: String,
    pub trigger_node_id: String,
    pub queued_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<i64>,
    /// The `code` of the run error.
    pub error_code: Option<String>,
}

#[derive(FromRow)]
struct RunRow {
    id: Uuid,
    workflow_id: Uuid,
    version: Option<i32>,
    mode: String,
    status: String,
    trigger_node_id: String,
    queued_at: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
    error_code: Option<String>,
}

impl From<RunRow> for RunSummary {
    fn from(r: RunRow) -> Self {
        let duration_ms = r.started_at.zip(r.finished_at).map(|(a, b)| (b - a).num_milliseconds().max(0));
        RunSummary {
            id: r.id,
            workflow_id: r.workflow_id,
            version: r.version,
            mode: r.mode,
            status: r.status,
            trigger_node_id: r.trigger_node_id,
            queued_at: r.queued_at,
            started_at: r.started_at,
            finished_at: r.finished_at,
            duration_ms,
            error_code: r.error_code,
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct RunList {
    pub runs: Vec<RunSummary>,
    pub next_cursor: Option<Uuid>,
}

#[derive(Deserialize, IntoParams)]
pub struct RunsQuery {
    workflow_id: Option<Uuid>,
    /// `test` or `production`.
    mode: Option<String>,
    status: Option<String>,
    /// Only runs queued at or after this time.
    since: Option<DateTime<Utc>>,
    cursor: Option<Uuid>,
    /// 1 to 200. Default 50.
    limit: Option<i64>,
}

const RUN_COLUMNS: &str = "r.id, r.workflow_id, v.version, r.mode, r.status, r.trigger_node_id, r.queued_at, r.started_at, r.finished_at, r.error ->> 'code' AS error_code";

/// List runs, newest first.
#[utoipa::path(get, path = "/v1/automations/runs", tag = "automations", params(RunsQuery),
    responses((status = 200, body = RunList)),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. Newest first. Page with `next_cursor`.")]
pub async fn list_runs(State(st): State<AppState>, ApiQuery(q): ApiQuery<RunsQuery>) -> ApiResult<ApiJson<RunList>> {
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let rows: Vec<RunRow> = sqlx::query_as(&format!(
        "SELECT {RUN_COLUMNS} FROM automation_runs r LEFT JOIN automation_versions v ON v.id = r.version_id \
         WHERE ($1::uuid IS NULL OR r.workflow_id = $1) AND ($2::text IS NULL OR r.mode = $2) AND ($3::text IS NULL OR r.status = $3) \
           AND ($4::timestamptz IS NULL OR r.queued_at >= $4) AND ($5::uuid IS NULL OR r.id < $5) \
         ORDER BY r.id DESC LIMIT $6"
    ))
    .bind(q.workflow_id)
    .bind(q.mode.as_deref())
    .bind(q.status.as_deref())
    .bind(q.since)
    .bind(q.cursor)
    .bind(limit + 1)
    .fetch_all(&st.pool)
    .await?;
    let more = rows.len() as i64 > limit;
    let runs: Vec<RunSummary> = rows.into_iter().take(limit as usize).map(Into::into).collect();
    let next_cursor = if more { runs.last().map(|r| r.id) } else { None };
    Ok(ApiJson(RunList { runs, next_cursor }))
}

#[derive(Serialize, ToSchema)]
pub struct RunNodeRow {
    pub node_id: String,
    /// The order the node ran in. A node can run more than once.
    pub seq: i32,
    /// `running`, `success`, `error`, `skipped` or `dry`.
    pub status: String,
    pub input_count: i32,
    /// Items per output port.
    #[schema(value_type = Object)]
    pub output_count: Value,
    /// `{ "<port>": [item, ...] }`, up to 20 items per port. `null`: not kept.
    #[schema(value_type = Option<Object>)]
    pub output: Option<Value>,
    /// Items left out of `output`, per port.
    #[schema(value_type = Object)]
    pub truncated_items: Value,
    /// The stored output was cut at 512 KB.
    pub truncated: bool,
    #[schema(value_type = Option<Object>)]
    pub error: Option<Value>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(FromRow)]
struct NodeRow {
    node_id: String,
    seq: i32,
    status: String,
    input_count: i32,
    output_count: Value,
    output: Option<Value>,
    truncated: bool,
    error: Option<Value>,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
}

fn cut_output(output: Option<Value>) -> (Option<Value>, Value) {
    let Some(Value::Object(ports)) = output else { return (output, json!({})) };
    let mut left = Map::new();
    let mut out = Map::new();
    for (port, items) in ports {
        match items {
            Value::Array(mut a) if a.len() > ITEMS_ON_RUN_PAGE => {
                left.insert(port.clone(), json!(a.len() - ITEMS_ON_RUN_PAGE));
                a.truncate(ITEMS_ON_RUN_PAGE);
                out.insert(port, Value::Array(a));
            }
            other => {
                out.insert(port, other);
            }
        }
    }
    (Some(Value::Object(out)), Value::Object(left))
}

impl From<NodeRow> for RunNodeRow {
    fn from(r: NodeRow) -> Self {
        let (output, truncated_items) = cut_output(r.output);
        RunNodeRow {
            node_id: r.node_id,
            seq: r.seq,
            status: r.status,
            input_count: r.input_count,
            output_count: r.output_count,
            output,
            truncated_items,
            truncated: r.truncated,
            error: r.error,
            started_at: r.started_at,
            finished_at: r.finished_at,
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct RunDetail {
    #[serde(flatten)]
    pub run: RunSummary,
    #[schema(value_type = Object)]
    pub trigger_item: Value,
    /// `{ real_writes, only_node }` of a test run.
    #[schema(value_type = Object)]
    pub options: Value,
    /// The run error: `{ code, message, node_id?, item_index?, status? }`.
    #[schema(value_type = Option<Object>)]
    pub error: Option<Value>,
    /// Set while the run waits in a long Wait node.
    pub resume_at: Option<DateTime<Utc>>,
    pub cancel_requested: bool,
    /// Time spent working (a long Wait does not count).
    pub active_ms: i64,
    pub nodes: Vec<RunNodeRow>,
}

#[derive(FromRow)]
struct DetailExtra {
    trigger_item: Value,
    options: Value,
    error: Option<Value>,
    resume_at: Option<DateTime<Utc>>,
    cancel_requested: bool,
    active_ms: i64,
}

/// One run with a row for every node execution.
#[utoipa::path(get, path = "/v1/automations/runs/{id}", tag = "automations", params(("id" = Uuid, Path)),
    responses((status = 200, body = RunDetail), (status = 404, description = "run_not_found")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. Node outputs are cut to 20 items per port (`truncated_items` says how many are left). Read the rest with `GET /v1/automations/runs/{id}/nodes/{node_id}`.")]
pub async fn get_run(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<RunDetail>> {
    let row: Option<RunRow> = sqlx::query_as(&format!("SELECT {RUN_COLUMNS} FROM automation_runs r LEFT JOIN automation_versions v ON v.id = r.version_id WHERE r.id = $1"))
        .bind(id)
        .fetch_optional(&st.pool)
        .await?;
    let run: RunSummary = row.ok_or_else(run_not_found)?.into();
    let extra: DetailExtra = sqlx::query_as("SELECT trigger_item, options, error, resume_at, cancel_requested, active_ms FROM automation_runs WHERE id = $1").bind(id).fetch_one(&st.pool).await?;
    let nodes: Vec<NodeRow> = sqlx::query_as(
        "SELECT node_id, seq, status, input_count, output_count, output, truncated, error, started_at, finished_at FROM automation_run_nodes WHERE run_id = $1 ORDER BY seq",
    )
    .bind(id)
    .fetch_all(&st.pool)
    .await?;
    Ok(ApiJson(RunDetail {
        run,
        trigger_item: extra.trigger_item,
        options: extra.options,
        error: extra.error,
        resume_at: extra.resume_at,
        cancel_requested: extra.cancel_requested,
        active_ms: extra.active_ms,
        nodes: nodes.into_iter().map(Into::into).collect(),
    }))
}

#[derive(Deserialize, IntoParams)]
pub struct NodeQuery {
    /// Which execution of the node. Default: the last one.
    seq: Option<i32>,
    /// Only this port.
    port: Option<String>,
    /// First item to return. Default 0.
    offset: Option<usize>,
    /// 1 to 1000. Default 100.
    limit: Option<usize>,
}

#[derive(Serialize, ToSchema)]
pub struct RunNodeOutput {
    pub node_id: String,
    pub seq: i32,
    pub status: String,
    pub input_count: i32,
    /// Items per port, as the node gave them.
    #[schema(value_type = Object)]
    pub output_count: Value,
    /// Items per port: the window `offset` / `limit`.
    #[schema(value_type = Object)]
    pub output: Value,
    /// The stored output was cut at 512 KB, so `output` may have fewer items than `output_count`.
    pub truncated: bool,
    #[schema(value_type = Option<Object>)]
    pub error: Option<Value>,
}

/// The full stored output of one node.
#[utoipa::path(get, path = "/v1/automations/runs/{id}/nodes/{node_id}", tag = "automations", params(("id" = Uuid, Path), ("node_id" = String, Path), NodeQuery),
    responses((status = 200, body = RunNodeOutput), (status = 404, description = "run_not_found, node_not_in_run")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. Read long outputs in windows with `offset` and `limit`.")]
pub async fn get_run_node(State(st): State<AppState>, ApiPath((id, node_id)): ApiPath<(Uuid, String)>, ApiQuery(q): ApiQuery<NodeQuery>) -> ApiResult<ApiJson<RunNodeOutput>> {
    let exists: Option<i32> = sqlx::query_scalar("SELECT 1 FROM automation_runs WHERE id = $1").bind(id).fetch_optional(&st.pool).await?;
    exists.ok_or_else(run_not_found)?;
    let row: Option<NodeRow> = sqlx::query_as(
        "SELECT node_id, seq, status, input_count, output_count, output, truncated, error, started_at, finished_at FROM automation_run_nodes \
         WHERE run_id = $1 AND node_id = $2 AND ($3::int IS NULL OR seq = $3) ORDER BY seq DESC LIMIT 1",
    )
    .bind(id)
    .bind(&node_id)
    .bind(q.seq)
    .fetch_optional(&st.pool)
    .await?;
    let r = row.ok_or_else(|| ApiError::not_found("node_not_in_run", "This node has no row in this run."))?;
    let (offset, limit) = (q.offset.unwrap_or(0), q.limit.unwrap_or(100).clamp(1, 1000));
    let mut window = Map::new();
    if let Some(Value::Object(ports)) = r.output {
        for (port, items) in ports {
            if q.port.as_deref().is_some_and(|p| p != port) {
                continue;
            }
            let items: Vec<Value> = items.as_array().map(|a| a.iter().skip(offset).take(limit).cloned().collect()).unwrap_or_default();
            window.insert(port, Value::Array(items));
        }
    }
    Ok(ApiJson(RunNodeOutput {
        node_id: r.node_id,
        seq: r.seq,
        status: r.status,
        input_count: r.input_count,
        output_count: r.output_count,
        output: Value::Object(window),
        truncated: r.truncated,
        error: r.error,
    }))
}

#[derive(Serialize, ToSchema)]
pub struct RunEvent {
    pub id: i64,
    pub at: DateTime<Utc>,
    /// `node_started`, `node_finished`, `node_error`, `waiting`, `resumed`, `node_restarted` or `log`.
    pub kind: String,
    pub node_id: Option<String>,
    #[schema(value_type = Object)]
    pub detail: Value,
}

#[derive(Serialize, ToSchema)]
pub struct RunEvents {
    /// The status of the run now. Stop polling when it is `success`, `error` or `cancelled`.
    pub status: String,
    pub events: Vec<RunEvent>,
}

#[derive(Deserialize, IntoParams)]
pub struct EventsQuery {
    /// Return events with an id above this one.
    after: Option<i64>,
    /// 1 to 500. Default 200.
    limit: Option<i64>,
}

#[derive(FromRow)]
struct EventRow {
    id: i64,
    at: DateTime<Utc>,
    kind: String,
    node_id: Option<String>,
    detail: Value,
}

/// The log of a run. Poll it with `after` to follow a run live.
#[utoipa::path(get, path = "/v1/automations/runs/{id}/events", tag = "automations", params(("id" = Uuid, Path), EventsQuery),
    responses((status = 200, body = RunEvents), (status = 404, description = "run_not_found")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. Oldest first. Send the biggest `id` you have as `after` to get only new events.")]
pub async fn get_run_events(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiQuery(q): ApiQuery<EventsQuery>) -> ApiResult<ApiJson<RunEvents>> {
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM automation_runs WHERE id = $1").bind(id).fetch_optional(&st.pool).await?;
    let status = status.ok_or_else(run_not_found)?;
    let rows: Vec<EventRow> = sqlx::query_as("SELECT id, at, kind, node_id, detail FROM automation_run_events WHERE run_id = $1 AND id > $2 ORDER BY id LIMIT $3")
        .bind(id)
        .bind(q.after.unwrap_or(0))
        .bind(q.limit.unwrap_or(200).clamp(1, 500))
        .fetch_all(&st.pool)
        .await?;
    Ok(ApiJson(RunEvents { status, events: rows.into_iter().map(|r| RunEvent { id: r.id, at: r.at, kind: r.kind, node_id: r.node_id, detail: r.detail }).collect() }))
}

#[derive(Serialize, ToSchema)]
pub struct CancelReply {
    pub id: Uuid,
    /// `cancelled` for a run that had not started or was waiting. `running` for a run that stops after the node it is in.
    pub status: String,
}

/// Cancel a run.
#[utoipa::path(post, path = "/v1/automations/runs/{id}/cancel", tag = "automations", params(("id" = Uuid, Path)),
    responses((status = 200, body = CancelReply), (status = 404, description = "run_not_found"), (status = 409, description = "run_finished")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. A queued or waiting run is cancelled at once. A running run stops between items and between nodes; a call in flight finishes first.")]
pub async fn cancel_run(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<CancelReply>> {
    let now = st.now();
    let status: Option<String> = sqlx::query_scalar(
        "UPDATE automation_runs SET cancel_requested = true, \
           status = CASE WHEN status IN ('queued', 'waiting') THEN 'cancelled' ELSE status END, \
           finished_at = CASE WHEN status IN ('queued', 'waiting') THEN $2 ELSE finished_at END, \
           state = CASE WHEN status IN ('queued', 'waiting') THEN NULL ELSE state END \
         WHERE id = $1 AND status IN ('queued', 'waiting', 'running') RETURNING status",
    )
    .bind(id)
    .bind(now)
    .fetch_optional(&st.pool)
    .await?;
    match status {
        Some(status) => Ok(ApiJson(CancelReply { id, status })),
        None => {
            let exists: Option<i32> = sqlx::query_scalar("SELECT 1 FROM automation_runs WHERE id = $1").bind(id).fetch_optional(&st.pool).await?;
            exists.ok_or_else(run_not_found)?;
            Err(ApiError::conflict("run_finished", "This run has already ended."))
        }
    }
}

// ---------- engine ----------

#[derive(Serialize, ToSchema)]
pub struct QueueCount {
    pub status: String,
    pub mode: String,
    pub count: i64,
}

#[derive(Serialize, ToSchema)]
pub struct EngineStatus {
    /// Production runs wait while the engine is paused. Test runs still run.
    pub paused: bool,
    /// This server copy runs the worker (`AIKIDO_AUTOMATIONS_ENABLED`).
    pub worker_enabled: bool,
    /// When this server copy last looked for runs.
    pub last_tick_at: Option<DateTime<Utc>>,
    /// Runs that are not finished, by status and mode.
    pub queue: Vec<QueueCount>,
    pub active_workflows: i64,
    /// Triggers that are switched on.
    pub active_triggers: i64,
    /// Triggers that cannot fire (`last_error`) or are held back by `max_runs_per_hour` (`rate_limited_at`).
    pub trigger_problems: Vec<TriggerProblem>,
    /// Plain sentences about things that need a look.
    pub warnings: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub struct TriggerProblem {
    pub workflow_id: Uuid,
    pub workflow_name: String,
    pub node_id: String,
    /// `schedule`, `cron`, `webhook` or `event`.
    pub kind: String,
    pub last_error: Option<String>,
    pub rate_limited_at: Option<DateTime<Utc>>,
}

async fn status_now(st: &AppState) -> ApiResult<EngineStatus> {
    let paused = worker::engine_settings(&st.pool).await?.paused;
    let rows: Vec<(String, String, i64)> =
        sqlx::query_as("SELECT status, mode, count(*) FROM automation_runs WHERE status IN ('queued', 'running', 'waiting') GROUP BY status, mode ORDER BY status, mode")
            .fetch_all(&st.pool)
            .await?;
    let active_workflows: i64 = sqlx::query_scalar("SELECT count(*) FROM automation_workflows WHERE active AND archived_at IS NULL").fetch_one(&st.pool).await?;
    let active_triggers: i64 = sqlx::query_scalar("SELECT count(*) FROM automation_triggers").fetch_one(&st.pool).await?;
    let problems: Vec<(Uuid, String, String, String, Option<String>, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT w.id, w.name, t.node_id, t.kind, t.last_error, t.rate_limited_at FROM automation_triggers t JOIN automation_workflows w ON w.id = t.workflow_id \
          WHERE t.last_error IS NOT NULL OR t.rate_limited_at IS NOT NULL ORDER BY w.name, t.node_id",
    )
    .fetch_all(&st.pool)
    .await?;
    let trigger_problems: Vec<TriggerProblem> = problems
        .into_iter()
        .map(|(workflow_id, workflow_name, node_id, kind, last_error, rate_limited_at)| TriggerProblem { workflow_id, workflow_name, node_id, kind, last_error, rate_limited_at })
        .collect();
    let mut warnings = Vec::new();
    if paused {
        warnings.push("The engine is paused: production runs wait in the queue.".to_string());
    }
    if !worker::is_enabled() {
        warnings.push("This server copy does not run automations (AIKIDO_AUTOMATIONS_ENABLED is off).".to_string());
    }
    if active_triggers > 0 && worker::last_tick().is_none_or(|t| (st.now() - t).num_seconds() > 120) {
        warnings.push("Triggers are switched on, but this server copy has not looked for runs in the last two minutes.".to_string());
    }
    for p in &trigger_problems {
        if let Some(e) = &p.last_error {
            warnings.push(format!("{} / {}: {e}", p.workflow_name, p.node_id));
        }
        if p.rate_limited_at.is_some() {
            warnings.push(format!("{} reached its hourly run limit.", p.workflow_name));
        }
    }
    Ok(EngineStatus {
        paused,
        worker_enabled: worker::is_enabled(),
        last_tick_at: worker::last_tick(),
        queue: rows.into_iter().map(|(status, mode, count)| QueueCount { status, mode, count }).collect(),
        active_workflows,
        active_triggers,
        trigger_problems,
        warnings,
    })
}

/// State of the engine.
#[utoipa::path(get, path = "/v1/automations/status", tag = "automations",
    responses((status = 200, body = EngineStatus)),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. `worker_enabled` and `last_tick_at` are about the server copy that answers.")]
pub async fn engine_status(State(st): State<AppState>) -> ApiResult<ApiJson<EngineStatus>> {
    Ok(ApiJson(status_now(&st).await?))
}

async fn set_paused(st: &AppState, session: &Option<Extension<SessionCtx>>, key: &Option<Extension<KeyScope>>, paused: bool) -> ApiResult<ApiJson<EngineStatus>> {
    if !caller(session, key).is_admin {
        return Err(ApiError::forbidden("admin_only", "Only an owner or an admin can pause or resume the engine."));
    }
    sqlx::query("UPDATE automation_settings SET paused = $1").bind(paused).execute(&st.pool).await?;
    Ok(ApiJson(status_now(st).await?))
}

/// Pause the engine.
#[utoipa::path(post, path = "/v1/automations/engine/pause", tag = "automations",
    responses((status = 200, body = EngineStatus), (status = 403, description = "admin_only")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`, and an owner/admin session or the `admin` key. Queued production runs wait. Test runs still run.")]
pub async fn engine_pause(State(st): State<AppState>, session: Option<Extension<SessionCtx>>, key: Option<Extension<KeyScope>>) -> ApiResult<ApiJson<EngineStatus>> {
    set_paused(&st, &session, &key, true).await
}

/// Resume the engine.
#[utoipa::path(post, path = "/v1/automations/engine/resume", tag = "automations",
    responses((status = 200, body = EngineStatus), (status = 403, description = "admin_only")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`, and an owner/admin session or the `admin` key.")]
pub async fn engine_resume(State(st): State<AppState>, session: Option<Extension<SessionCtx>>, key: Option<Extension<KeyScope>>) -> ApiResult<ApiJson<EngineStatus>> {
    set_paused(&st, &session, &key, false).await
}
