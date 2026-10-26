//! Small routes for the editor page (phase 6): the live preview of an expression (build map 6.6) and the
//! list of operations for the `aikido.api` node picker (map 13).
use super::definition::WorkflowDefinition;
use super::engine::graph::ancestors;
use super::expr::ExprEngine;
use super::internal_call::deny_reason;
use crate::auth::api_keys::Scope;
use crate::auth::middleware::require_scope;
use crate::error::{ApiError, ApiJson, ApiResult};
use crate::state::AppState;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sqlx::FromRow;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn router(state: AppState) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(preview_expression))
        .routes(routes!(aikido_api_operations))
        .route_layer(axum::middleware::from_fn_with_state((state, Scope::Automations), require_scope))
}

// ---------- operations of aikido.api ----------

#[derive(Serialize, ToSchema)]
pub struct ApiOperation {
    /// `METHOD /path`: the value for the `operation` param.
    pub operation: String,
    pub method: String,
    pub path: String,
    pub tag: String,
    pub summary: String,
    pub path_params: Vec<String>,
    pub query_params: Vec<String>,
    pub has_body: bool,
}

#[derive(Serialize, ToSchema)]
pub struct ApiOperations {
    pub operations: Vec<ApiOperation>,
}

/// The routes the `aikido.api` node may call, with their tag, method and parameters. Routes a workflow may not call are left out.
#[utoipa::path(get, path = "/v1/automations/nodes/aikido-api/operations", tag = "automations",
    responses((status = 200, body = ApiOperations)),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. For the operation picker of the editor.")]
async fn aikido_api_operations(State(st): State<AppState>) -> ApiResult<ApiJson<ApiOperations>> {
    let allow_erase = super::settings::load(&st.pool).await?.allow_lead_erase;
    let index = st.internal_router.operations(&st);
    let mut operations: Vec<ApiOperation> = index
        .all()
        .filter(|o| deny_reason(&o.method, &o.path, allow_erase).is_none())
        .map(|o| ApiOperation {
            operation: o.id.clone(),
            method: o.method.clone(),
            path: o.path.clone(),
            tag: o.tag.clone(),
            summary: o.summary.clone(),
            path_params: o.path_params.clone(),
            query_params: o.query_params.clone(),
            has_body: o.has_body,
        })
        .collect();
    operations.sort_by(|a, b| (&a.tag, &a.path, &a.method).cmp(&(&b.tag, &b.path, &b.method)));
    Ok(ApiJson(ApiOperations { operations }))
}

// ---------- expression preview ----------

#[derive(Deserialize, ToSchema)]
pub struct PreviewBody {
    pub workflow_id: Uuid,
    /// The node whose param is shown. Its input decides what `json` is.
    pub node_id: String,
    /// The text of the param, with `${{ }}`.
    pub text: String,
    /// A test run to read the data from. Default: the newest test run of the workflow.
    pub run_id: Option<Uuid>,
    /// Which item of the data to use. Default 0.
    pub item_index: Option<usize>,
}

#[derive(Serialize, ToSchema)]
pub struct PreviewError {
    pub code: String,
    pub message: String,
}

#[derive(Serialize, ToSchema)]
pub struct PreviewReply {
    #[schema(value_type = Option<Object>)]
    pub value: Option<Value>,
    /// `string`, `number`, `boolean`, `array`, `object` or `null`.
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub error: Option<PreviewError>,
    /// The run the data came from. `None`: there is no test run yet, so nothing was evaluated.
    pub run_id: Option<Uuid>,
}

#[derive(FromRow)]
struct RunPick {
    id: Uuid,
    trigger_node_id: String,
    trigger_item: Value,
    options: Value,
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// The items a stored node output holds: the `main` port if there is one, else every port.
fn stored_items(output: &Value) -> Vec<Value> {
    let Some(ports) = output.as_object() else { return vec![] };
    if let Some(Value::Array(a)) = ports.get("main") {
        return a.clone();
    }
    ports.values().filter_map(Value::as_array).flatten().cloned().collect()
}

/// Read a text against the data of a test run. The data is the stored output of each node. The stored
/// items have no lineage, so `nodes.x.json` is the item at the same position, not the exact paired item.
#[utoipa::path(post, path = "/v1/automations/expression/preview", tag = "automations", request_body = PreviewBody,
    responses((status = 200, body = PreviewReply), (status = 404, description = "workflow_not_found, run_not_found")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. Reads `text` with the data of a test run and answers the value, its type or the error. `secret()` is not available here.")]
async fn preview_expression(State(st): State<AppState>, ApiJson(body): ApiJson<PreviewBody>) -> ApiResult<ApiJson<PreviewReply>> {
    let draft: Option<Value> = sqlx::query_scalar("SELECT draft FROM automation_workflows WHERE id = $1").bind(body.workflow_id).fetch_optional(&st.pool).await?;
    let draft = draft.ok_or_else(|| ApiError::not_found("workflow_not_found", "There is no workflow with this id."))?;
    let none = |run_id| PreviewReply { value: None, kind: None, error: None, run_id };
    let Ok(def) = serde_json::from_value::<WorkflowDefinition>(draft) else { return Ok(ApiJson(none(None))) };
    if def.node(&body.node_id).is_none() {
        return Err(ApiError::not_found("node_not_found", "The workflow has no node with this id."));
    }
    let run: Option<RunPick> = match body.run_id {
        Some(id) => sqlx::query_as("SELECT id, trigger_node_id, trigger_item, options FROM automation_runs WHERE id = $1 AND workflow_id = $2").bind(id).bind(body.workflow_id).fetch_optional(&st.pool).await?,
        None => sqlx::query_as("SELECT id, trigger_node_id, trigger_item, options FROM automation_runs WHERE workflow_id = $1 AND mode = 'test' ORDER BY queued_at DESC LIMIT 1").bind(body.workflow_id).fetch_optional(&st.pool).await?,
    };
    let Some(run) = run else {
        if body.run_id.is_some() {
            return Err(ApiError::not_found("run_not_found", "There is no run with this id."));
        }
        return Ok(ApiJson(none(None)));
    };
    let stored: Vec<(String, Option<Value>)> =
        sqlx::query_as("SELECT DISTINCT ON (node_id) node_id, output FROM automation_run_nodes WHERE run_id = $1 ORDER BY node_id, seq DESC").bind(run.id).fetch_all(&st.pool).await?;
    let idx = body.item_index.unwrap_or(0);
    let pick = |items: &[Value]| items.get(idx.min(items.len().saturating_sub(1))).cloned();
    let up = ancestors(&def, &body.node_id);

    let mut nodes = Map::new();
    let mut by_node: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    for (id, out) in stored {
        if let Some(out) = out {
            by_node.insert(id, out);
        }
    }
    for id in &up {
        if *id == body.node_id {
            continue;
        }
        let mut entry = Map::new();
        if *id == run.trigger_node_id {
            entry.insert("json".into(), run.trigger_item.clone());
            entry.insert("items".into(), json!([run.trigger_item]));
        } else if let Some(out) = by_node.get(id) {
            let items = stored_items(out);
            if let Some(j) = pick(&items) {
                entry.insert("json".into(), j);
            }
            entry.insert("items".into(), Value::Array(items));
        }
        nodes.insert(id.clone(), Value::Object(entry));
    }
    // `json` is what the node receives: the output of its first parent on the edge's port.
    let input = def
        .edges
        .iter()
        .find(|e| e.to == body.node_id && up.contains(&e.from))
        .and_then(|e| {
            if e.from == run.trigger_node_id {
                return Some(run.trigger_item.clone());
            }
            let out = by_node.get(&e.from)?;
            let items = out.get(e.port()).and_then(Value::as_array).cloned().unwrap_or_default();
            pick(&items)
        })
        .or_else(|| (body.node_id == run.trigger_node_id).then(|| run.trigger_item.clone()))
        .unwrap_or_else(|| json!({}));

    let mut vars = Map::new();
    for v in &def.variables {
        if let Some(val) = &v.value {
            vars.insert(v.key.clone(), val.clone());
        }
    }
    if let Some(Value::Object(given)) = run.options.get("variables") {
        for (k, v) in given {
            vars.insert(k.clone(), v.clone());
        }
    }
    let ctx = json!({
        "json": input,
        "trigger": run.trigger_item,
        "nodes": nodes,
        "vars": vars,
        "run": { "id": run.id.to_string(), "mode": "test", "started_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true), "item_index": idx },
        "workflow": { "id": body.workflow_id.to_string(), "name": def.name, "version": Value::Null },
    });
    let reply = match ExprEngine::new().eval(&body.text, &ctx) {
        Ok(v) => PreviewReply { kind: Some(kind_of(&v).to_string()), value: Some(v), error: None, run_id: Some(run.id) },
        Err(e) => PreviewReply { value: None, kind: None, error: Some(PreviewError { code: e.code.to_string(), message: e.message }), run_id: Some(run.id) },
    };
    Ok(ApiJson(reply))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_items_prefer_the_main_port() {
        let out = json!({ "main": [{ "a": 1 }], "error": [{ "b": 2 }] });
        assert_eq!(stored_items(&out), vec![json!({ "a": 1 })]);
        let out = json!({ "true": [{ "a": 1 }], "false": [{ "b": 2 }] });
        assert_eq!(stored_items(&out).len(), 2);
        assert!(stored_items(&json!(null)).is_empty());
    }

    #[test]
    fn kinds_are_named() {
        assert_eq!(kind_of(&json!([1])), "array");
        assert_eq!(kind_of(&json!(null)), "null");
        assert_eq!(kind_of(&json!("x")), "string");
    }
}
