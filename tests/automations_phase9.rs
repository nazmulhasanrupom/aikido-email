//! Automations, phase 9: the error workflow and `flow.execute_workflow` on the real engine (database, worker, versions).
//! These tests need Postgres (not run in the session that wrote them).
mod common;
use aikido_email::auth::api_keys::Scope;
use aikido_email::automations::engine::worker::{self, WorkerOpts};
use aikido_email::state::AppState;
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::HashMap;

const WF: &str = "/v1/automations/workflows";
const RUNS: &str = "/v1/automations/runs";

struct Ctx {
    state: AppState,
    keys: HashMap<Scope, String>,
}

impl Ctx {
    async fn new(state: AppState) -> Self {
        let _owner = setup_owner(&state).await;
        let keys = bootstrap_keys(&state).await;
        Ctx { state, keys }
    }

    async fn ok(&self, method: &str, uri: &str, body: Option<Value>) -> Value {
        let r = call(&self.state, method, uri, Some(&self.keys[&Scope::Automations]), body).await;
        assert!(r.status.is_success(), "{method} {uri} -> {} {}", r.status, r.body);
        r.body
    }

    /// Make, publish and activate a workflow. Returns its id.
    async fn go(&self, def: Value) -> String {
        let r = self.ok("POST", WF, Some(json!({ "name": def["name"].as_str().unwrap_or("P9"), "definition": def }))).await;
        let wf = r["id"].as_str().unwrap().to_string();
        self.ok("POST", &format!("{WF}/{wf}/publish"), Some(json!({}))).await;
        self.ok("POST", &format!("{WF}/{wf}/activate"), Some(json!({}))).await;
        wf
    }

    async fn start(&self, wf: &str) -> String {
        self.ok("POST", &format!("{WF}/{wf}/run"), Some(json!({ "input": { "n": 1 } }))).await["run_id"].as_str().unwrap().to_string()
    }

    /// Let the worker process every queued run (also the runs that other runs queue).
    async fn work(&self) {
        for _ in 0..3 {
            worker::tick(&self.state, &WorkerOpts::default()).await.unwrap();
        }
    }

    async fn run(&self, id: &str) -> Value {
        self.ok("GET", &format!("{RUNS}/{id}"), None).await
    }

    /// (status, trigger item, options) of the production runs of a workflow, oldest first.
    async fn rows(&self, wf: &str) -> Vec<(String, Value, Value)> {
        sqlx::query_as("SELECT status, trigger_item, options FROM automation_runs WHERE workflow_id = $1::uuid AND mode = 'production' ORDER BY queued_at, id").bind(wf).fetch_all(&self.state.pool).await.unwrap()
    }
}

fn failing(error_workflow: Option<&str>) -> Value {
    let mut d = json!({
        "format": "aikido-workflow", "version": 1, "name": "Fails",
        "nodes": [{ "id": "start", "type": "trigger.manual" }, { "id": "bad", "name": "The bad step", "type": "flow.stop", "params": { "message": "boom", "error": true } }],
        "edges": [{ "from": "start", "to": "bad" }]
    });
    if let Some(id) = error_workflow {
        d["settings"] = json!({ "error_workflow_id": id });
    }
    d
}

fn handler() -> Value {
    json!({
        "format": "aikido-workflow", "version": 1, "name": "Error handler",
        "nodes": [{ "id": "start", "type": "trigger.manual" }, { "id": "note", "type": "data.set", "params": { "fields": { "seen": "${{ json.error_code }}" } } }],
        "edges": [{ "from": "start", "to": "note" }]
    })
}

#[sqlx::test]
async fn a_failed_production_run_starts_the_error_workflow_with_the_failure(pool: PgPool) {
    let c = Ctx::new(app_state(pool)).await;
    let handler_id = c.go(handler()).await;
    let wf = c.go(failing(Some(&handler_id))).await;
    let run_id = c.start(&wf).await;
    c.work().await;

    let failed = c.run(&run_id).await;
    assert_eq!(failed["status"], "error");
    let h = c.rows(&handler_id).await;
    assert_eq!(h.len(), 1, "the error workflow ran once");
    let (status, item, options) = &h[0];
    assert_eq!(status, "success");
    assert_eq!(item["workflow_id"], wf.as_str());
    assert_eq!(item["workflow_name"], "Fails");
    assert_eq!(item["run_id"], run_id.as_str());
    assert_eq!(item["failed_node_id"], "bad");
    assert_eq!(item["failed_node_name"], "The bad step");
    assert_eq!(item["error_code"], "stopped_by_node");
    assert_eq!(item["error_message"], "boom");
    assert_eq!(options["error_run_for"], run_id.as_str());
    // the handler read the item
    let events: Vec<(String,)> = sqlx::query_as("SELECT kind FROM automation_run_events WHERE run_id = $1::uuid AND kind = 'error_workflow_started'").bind(&run_id).fetch_all(&c.state.pool).await.unwrap();
    assert_eq!(events.len(), 1);
}

#[sqlx::test]
async fn an_error_workflow_that_fails_does_not_start_another_one(pool: PgPool) {
    let c = Ctx::new(app_state(pool)).await;
    // b fails and names a as its error workflow, a fails and names b: a ping-pong must stop after one hop
    let a = c.go(failing(None)).await;
    let b = c.go(failing(Some(&a))).await;
    // point a at b (publish again with the setting)
    let r = c.ok("GET", &format!("{WF}/{a}"), None).await;
    let mut draft = r["draft"].clone();
    draft["settings"] = json!({ "error_workflow_id": b });
    c.ok("PATCH", &format!("{WF}/{a}"), Some(json!({ "draft": draft }))).await;
    c.ok("POST", &format!("{WF}/{a}/publish"), Some(json!({}))).await;
    c.start(&a).await;
    c.work().await;
    assert_eq!(c.rows(&a).await.len(), 1, "a ran once and was not started again by b");
    let b_rows = c.rows(&b).await;
    assert_eq!(b_rows.len(), 1, "b ran once, as the error workflow of a");
    assert_eq!(b_rows[0].0, "error");
    let skipped: Vec<(Value,)> = sqlx::query_as("SELECT detail FROM automation_run_events WHERE kind = 'error_workflow_skipped'").fetch_all(&c.state.pool).await.unwrap();
    assert_eq!(skipped.len(), 1, "the chain stop is in the log of b");
}

#[sqlx::test]
async fn the_error_workflow_needs_a_published_version_and_a_cancelled_run_starts_nothing(pool: PgPool) {
    let c = Ctx::new(app_state(pool)).await;
    // a draft that was never published
    let r = c.ok("POST", WF, Some(json!({ "name": "Handler", "definition": handler() }))).await;
    let draft_only = r["id"].as_str().unwrap().to_string();
    let wf = c.go(failing(Some(&draft_only))).await;
    let run_id = c.start(&wf).await;
    c.work().await;
    assert_eq!(c.run(&run_id).await["status"], "error");
    assert!(c.rows(&draft_only).await.is_empty());
    let skipped: Vec<(Value,)> = sqlx::query_as("SELECT detail FROM automation_run_events WHERE run_id = $1::uuid AND kind = 'error_workflow_skipped'").bind(&run_id).fetch_all(&c.state.pool).await.unwrap();
    assert_eq!(skipped.len(), 1);
    assert!(skipped[0].0["reason"].as_str().unwrap().contains("no published version"));
}

#[sqlx::test]
async fn execute_workflow_uses_the_published_version_in_production_and_the_draft_in_a_test(pool: PgPool) {
    let c = Ctx::new(app_state(pool)).await;
    let child = |who: &str| {
        json!({
            "format": "aikido-workflow", "version": 1, "name": "Child",
            "nodes": [{ "id": "start", "type": "trigger.manual" }, { "id": "tag", "type": "data.set", "params": { "fields": { "who": who } } }],
            "edges": [{ "from": "start", "to": "tag" }]
        })
    };
    let child_id = c.go(child("published")).await;
    // change the draft after publishing
    let r = c.ok("GET", &format!("{WF}/{child_id}"), None).await;
    let mut draft = r["draft"].clone();
    draft["nodes"][1]["params"]["fields"]["who"] = json!("draft");
    c.ok("PATCH", &format!("{WF}/{child_id}"), Some(json!({ "draft": draft }))).await;

    let parent = json!({
        "format": "aikido-workflow", "version": 1, "name": "Parent",
        "nodes": [{ "id": "start", "type": "trigger.manual" }, { "id": "sub", "type": "flow.execute_workflow", "params": { "workflow_id": child_id } }],
        "edges": [{ "from": "start", "to": "sub" }]
    });
    let parent_id = c.go(parent).await;
    let run_id = c.start(&parent_id).await;
    c.work().await;
    let run = c.run(&run_id).await;
    assert_eq!(run["status"], "success", "{run}");
    let sub = run["nodes"].as_array().unwrap().iter().find(|n| n["node_id"] == "sub").unwrap();
    assert_eq!(sub["output"]["main"][0]["who"], "published");

    let t = c.ok("POST", &format!("{WF}/{parent_id}/test"), Some(json!({}))).await;
    c.work().await;
    let run = c.run(t["run_id"].as_str().unwrap()).await;
    let sub = run["nodes"].as_array().unwrap().iter().find(|n| n["node_id"] == "sub").unwrap();
    assert_eq!(sub["output"]["main"][0]["who"], "draft");
}

#[sqlx::test]
async fn execute_workflow_refuses_a_workflow_that_calls_itself(pool: PgPool) {
    let c = Ctx::new(app_state(pool)).await;
    let r = c.ok("POST", WF, Some(json!({ "name": "Loop", "definition": failing(None) }))).await;
    let id = r["id"].as_str().unwrap().to_string();
    let def = json!({
        "format": "aikido-workflow", "version": 1, "name": "Loop",
        "nodes": [{ "id": "start", "type": "trigger.manual" }, { "id": "again", "type": "flow.execute_workflow", "params": { "workflow_id": id } }],
        "edges": [{ "from": "start", "to": "again" }]
    });
    c.ok("PATCH", &format!("{WF}/{id}"), Some(json!({ "draft": def }))).await;
    c.ok("POST", &format!("{WF}/{id}/publish"), Some(json!({}))).await;
    c.ok("POST", &format!("{WF}/{id}/activate"), Some(json!({}))).await;
    let run_id = c.start(&id).await;
    c.work().await;
    let run = c.run(&run_id).await;
    assert_eq!(run["status"], "error");
    assert_eq!(run["error"]["code"], "sub_workflow_loop");
}
