//! The error workflow (phase 9, build map 4.7 `error_workflow_id`).
//!
//! When a **production** run ends in `error`, and the version it ran has `settings.error_workflow_id`, the **published**
//! version of that workflow is queued as a new production run. Its trigger item describes the failure (see
//! `failure_item`). The run acts as the user who published the error workflow (like any production run of it).
//!
//! Rules:
//! - No chain: a run that was started as an error workflow carries `options.error_run_for` (the id of the failed run).
//!   When that run fails, no other error workflow is started.
//! - Only `error` ends count. Cancelled runs, test runs and runs that wait do not start it.
//! - One failed run starts at most one error run (idempotency key `error_workflow:<run id>`).
//! - The error workflow must be published and not archived. It need not be active. It obeys its own
//!   `max_runs_per_hour` and the queue back-pressure (`production::runs::limits`). When it can not start, the failed run
//!   gets a log event `error_workflow_skipped` with the reason, and nothing else happens.
use super::worker::log_event;
use crate::automations::definition::WorkflowDefinition;
use crate::automations::production::runs::{self, NewRun};
use crate::state::AppState;
use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use uuid::Uuid;

/// The trigger item of the error workflow: flat fields (easy in expressions) and the whole `error`.
pub fn failure_item(workflow_id: &str, workflow_name: &str, version: Option<i32>, run_id: &str, started_at: Option<DateTime<Utc>>, failed_at: DateTime<Utc>, node_name: Option<&str>, error: &Value) -> Map<String, Value> {
    let text = |k: &str| error.get(k).and_then(Value::as_str).map(str::to_string);
    json!({
        "workflow_id": workflow_id,
        "workflow_name": workflow_name,
        "workflow_version": version,
        "run_id": run_id,
        "run_started_at": started_at,
        "failed_at": failed_at,
        "failed_node_id": text("node_id"),
        "failed_node_name": node_name,
        "error_code": text("code").unwrap_or_else(|| "error".into()),
        "error_message": text("message").unwrap_or_default(),
        "error": error,
    })
    .as_object()
    .cloned()
    .unwrap_or_default()
}

/// The `settings.error_workflow_id` of a definition, when it is set to something.
pub fn target_of(def: &Value) -> Option<String> {
    def.get("settings")?.get("error_workflow_id")?.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

type RunRow = (Uuid, String, Value, Option<DateTime<Utc>>, String, Option<Value>, Option<i32>);

/// A run ended in error: start the error workflow, if there is one. Never fails the caller: problems are logged.
pub async fn on_failed(st: &AppState, run_id: Uuid, error: &Value) {
    if let Err(e) = start(st, run_id, error).await {
        tracing::error!(run = %run_id, error = %e, "could not start the error workflow");
    }
}

async fn skipped(st: &AppState, run_id: Uuid, reason: &str, target: Option<&str>) {
    log_event(&st.pool, run_id, st.now(), "error_workflow_skipped", None, json!({ "reason": reason, "error_workflow_id": target })).await;
}

async fn start(st: &AppState, run_id: Uuid, error: &Value) -> Result<(), sqlx::Error> {
    let row: Option<RunRow> = sqlx::query_as(
        "SELECT r.workflow_id, r.mode, r.options, r.started_at, w.name, COALESCE(r.definition, v.definition), v.version \
         FROM automation_runs r JOIN automation_workflows w ON w.id = r.workflow_id LEFT JOIN automation_versions v ON v.id = r.version_id WHERE r.id = $1",
    )
    .bind(run_id)
    .fetch_optional(&st.pool)
    .await?;
    let Some((workflow_id, mode, options, started_at, name, def, version)) = row else { return Ok(()) };
    if mode != "production" {
        return Ok(());
    }
    let Some(def_value) = def else { return Ok(()) };
    let Some(target) = target_of(&def_value) else { return Ok(()) };
    if options.get("error_run_for").is_some_and(|v| !v.is_null()) {
        // the run that failed was itself an error workflow: no chain
        skipped(st, run_id, "This run was started as an error workflow. An error workflow that fails does not start another one.", Some(&target)).await;
        return Ok(());
    }
    let Ok(target_id) = Uuid::parse_str(&target) else {
        skipped(st, run_id, "settings.error_workflow_id is not a workflow id.", Some(&target)).await;
        return Ok(());
    };
    let now = st.now();
    let mut tx = st.pool.begin().await?;
    let tw: Option<(Option<DateTime<Utc>>, Option<Uuid>, Option<Uuid>, Option<Value>)> = sqlx::query_as(
        "SELECT w.archived_at, w.published_version_id, w.run_as_user_id, v.definition FROM automation_workflows w \
         LEFT JOIN automation_versions v ON v.id = w.published_version_id WHERE w.id = $1 FOR UPDATE OF w",
    )
    .bind(target_id)
    .fetch_optional(&mut *tx)
    .await?;
    let (version_id, run_as, target_def) = match tw {
        None => {
            tx.rollback().await?;
            skipped(st, run_id, "The error workflow does not exist.", Some(&target)).await;
            return Ok(());
        }
        Some((Some(_), ..)) => {
            tx.rollback().await?;
            skipped(st, run_id, "The error workflow is archived.", Some(&target)).await;
            return Ok(());
        }
        Some((None, None, ..)) | Some((None, _, _, None)) => {
            tx.rollback().await?;
            skipped(st, run_id, "The error workflow has no published version. Publish it.", Some(&target)).await;
            return Ok(());
        }
        Some((None, Some(v), run_as, Some(d))) => (v, run_as, d),
    };
    let Ok(target_def) = serde_json::from_value::<WorkflowDefinition>(target_def) else {
        tx.rollback().await?;
        skipped(st, run_id, "The published version of the error workflow can not be read.", Some(&target)).await;
        return Ok(());
    };
    let trigger = match super::sub_workflow::pick_trigger(&target_def, None) {
        Ok(t) => t,
        Err(e) => {
            tx.rollback().await?;
            skipped(st, run_id, &e.message, Some(&target)).await;
            return Ok(());
        }
    };
    let limits = runs::limits(&mut tx, target_id, runs::max_per_hour(target_def.settings.max_runs_per_hour.map(|n| n as i32)), now).await?;
    if !limits.open() {
        tx.rollback().await?;
        skipped(st, run_id, "The error workflow is at its limit of runs (max_runs_per_hour or a full queue).", Some(&target)).await;
        return Ok(());
    }
    let node_name = error.get("node_id").and_then(Value::as_str).and_then(|id| def_value.get("nodes")?.as_array()?.iter().find(|n| n["id"] == id)).and_then(|n| n.get("name")?.as_str().map(str::to_string));
    let item = failure_item(&workflow_id.to_string(), &name, version, &run_id.to_string(), started_at, now, node_name.as_deref(), error);
    let new = NewRun { workflow_id: target_id, version_id, trigger_node_id: &trigger, trigger_item: Value::Object(item), run_as_user_id: run_as, started_by: None, idempotency_key: Some(format!("error_workflow:{run_id}")) };
    let Some(new_id) = runs::insert_run(&mut tx, now, new).await? else {
        tx.rollback().await?;
        return Ok(());
    };
    sqlx::query("UPDATE automation_runs SET options = options || $2 WHERE id = $1").bind(new_id).bind(json!({ "error_run_for": run_id.to_string() })).execute(&mut *tx).await?;
    tx.commit().await?;
    log_event(&st.pool, run_id, now, "error_workflow_started", None, json!({ "error_workflow_id": target, "run_id": new_id.to_string() })).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_failure_item_describes_the_failure() {
        let at = DateTime::parse_from_rfc3339("2026-10-26T10:00:00Z").unwrap().with_timezone(&Utc);
        let err = json!({ "code": "http_status_500", "message": "The server answered with status 500.", "node_id": "call", "status": 500 });
        let item = failure_item("w1", "Research", Some(3), "r1", Some(at), at, Some("Call the API"), &err);
        assert_eq!(item["workflow_id"], "w1");
        assert_eq!(item["workflow_name"], "Research");
        assert_eq!(item["workflow_version"], 3);
        assert_eq!(item["run_id"], "r1");
        assert_eq!(item["failed_node_id"], "call");
        assert_eq!(item["failed_node_name"], "Call the API");
        assert_eq!(item["error_code"], "http_status_500");
        assert_eq!(item["error_message"], "The server answered with status 500.");
        assert_eq!(item["error"]["status"], 500);
        assert_eq!(item["failed_at"], "2026-10-26T10:00:00Z");
        // an error without a node (run timeout)
        let item = failure_item("w1", "R", None, "r2", None, at, None, &json!({ "code": "run_timeout", "message": "Too long." }));
        assert_eq!(item["failed_node_id"], Value::Null);
        assert_eq!(item["workflow_version"], Value::Null);
        assert_eq!(item["run_started_at"], Value::Null);
    }

    #[test]
    fn the_target_is_read_from_the_settings() {
        assert_eq!(target_of(&json!({ "settings": { "error_workflow_id": " abc " } })).as_deref(), Some("abc"));
        assert_eq!(target_of(&json!({ "settings": { "error_workflow_id": "" } })), None);
        assert_eq!(target_of(&json!({ "settings": { "error_workflow_id": null } })), None);
        assert_eq!(target_of(&json!({ "settings": {} })), None);
        assert_eq!(target_of(&json!({})), None);
    }
}
