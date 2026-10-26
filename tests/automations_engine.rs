//! Automations, phase 2: test runs, the worker, run routes, Wait, lease recovery, pause, cancel, cleanup.
//! These tests need Postgres (not run in the session that wrote them).
mod common;
use aikido_email::auth::api_keys::Scope;
use aikido_email::automations::engine::worker::{self, WorkerOpts};
use aikido_email::state::AppState;
use axum::http::StatusCode;
use chrono::Duration;
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

const WF: &str = "/v1/automations/workflows";
const RUNS: &str = "/v1/automations/runs";

fn opts() -> WorkerOpts {
    WorkerOpts::default()
}

fn flow() -> Value {
    json!({
        "format": "aikido-workflow", "version": 1, "name": "x",
        "nodes": [
            { "id": "start", "type": "trigger.manual" },
            { "id": "split", "type": "data.split_out", "params": { "path": "input.leads" } },
            { "id": "big", "type": "flow.filter", "params": { "conditions": [{ "left": "${{ json.n }}", "op": "gt", "right": 4 }] } },
            { "id": "tag", "type": "data.set", "params": { "fields": { "who": "${{ trigger.input.company }}" } } }
        ],
        "edges": [{ "from": "start", "to": "split" }, { "from": "split", "to": "big" }, { "from": "big", "to": "tag" }]
    })
}

async fn make(state: &AppState, key: &str, def: Value) -> String {
    let r = call(state, "POST", WF, Some(key), Some(json!({ "name": "Test flow", "definition": def }))).await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    r.body["id"].as_str().unwrap().to_string()
}

async fn start_test(state: &AppState, key: &str, wf: &str, body: Value) -> Uuid {
    let r = call(state, "POST", &format!("{WF}/{wf}/test"), Some(key), Some(body)).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{:?}", r.body);
    r.body["run_id"].as_str().unwrap().parse().unwrap()
}

async fn run_of(state: &AppState, key: &str, id: Uuid) -> Value {
    let r = call(state, "GET", &format!("{RUNS}/{id}"), Some(key), None).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    r.body
}

fn node<'a>(run: &'a Value, id: &str) -> &'a Value {
    run["nodes"].as_array().unwrap().iter().find(|n| n["node_id"] == id).unwrap_or_else(|| panic!("no row for {id}: {run}"))
}

#[sqlx::test]
async fn a_test_run_goes_through_the_queue_and_leaves_rows_and_events(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Automations];
    let wf = make(&state, k, flow()).await;
    let run = start_test(&state, k, &wf, json!({ "trigger_item": { "input": { "company": "Acme", "leads": [{ "n": 1 }, { "n": 5 }, { "n": 9 }] } } })).await;

    let queued = run_of(&state, k, run).await;
    assert_eq!(queued["status"], "queued");
    assert_eq!(queued["mode"], "test");
    assert_eq!(worker::tick(&state, &opts()).await.unwrap(), 1);

    let done = run_of(&state, k, run).await;
    assert_eq!(done["status"], "success", "{done}");
    assert_eq!(node(&done, "split")["output_count"]["main"], 3);
    assert_eq!(node(&done, "big")["output_count"]["main"], 2);
    let tag = node(&done, "tag");
    assert_eq!(tag["status"], "success");
    assert_eq!(tag["output"]["main"][1], json!({ "n": 9, "who": "Acme" }));
    assert!(done["finished_at"].is_string() && done["duration_ms"].is_number());

    let ev = call(&state, "GET", &format!("{RUNS}/{run}/events"), Some(k), None).await.body;
    assert_eq!(ev["status"], "success");
    let kinds: Vec<&str> = ev["events"].as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds.iter().filter(|k| **k == "node_finished").count(), 4);
    // polling with `after` returns only new events
    let last = ev["events"].as_array().unwrap().last().unwrap()["id"].as_i64().unwrap();
    let none = call(&state, "GET", &format!("{RUNS}/{run}/events?after={last}"), Some(k), None).await.body;
    assert!(none["events"].as_array().unwrap().is_empty());

    // the list and the node window
    let list = call(&state, "GET", &format!("{RUNS}?workflow_id={wf}&mode=test"), Some(k), None).await.body;
    assert_eq!(list["runs"].as_array().unwrap().len(), 1);
    assert_eq!(list["runs"][0]["status"], "success");
    let win = call(&state, "GET", &format!("{RUNS}/{run}/nodes/split?offset=1&limit=1"), Some(k), None).await.body;
    assert_eq!(win["output"]["main"], json!([{ "n": 5 }]));
    let missing = call(&state, "GET", &format!("{RUNS}/{run}/nodes/nope"), Some(k), None).await;
    assert_eq!(missing.body["error"]["code"], "node_not_in_run");
    let gone = call(&state, "GET", &format!("{RUNS}/{}", Uuid::now_v7()), Some(k), None).await;
    assert_eq!((gone.status, gone.body["error"]["code"].as_str()), (StatusCode::NOT_FOUND, Some("run_not_found")));

    // the workflow list shows the run
    let wl = call(&state, "GET", WF, Some(k), None).await.body;
    assert_eq!(wl["workflows"][0]["last_run"]["status"], "success");
}

#[sqlx::test]
async fn a_failed_run_keeps_the_error_and_the_failing_node(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let _owner = setup_owner(&state).await; // a key run acts as the oldest owner
    let k = &keys[&Scope::Automations];
    let def = json!({ "format": "aikido-workflow", "version": 1, "name": "x",
        "nodes": [{ "id": "start", "type": "trigger.manual" }, { "id": "read", "type": "aikido.lead.update", "params": { "lead_id": "0192f3a0-0000-7000-8000-000000000001", "fields": { "note": "x" } } }],
        "edges": [{ "from": "start", "to": "read" }] });
    let wf = make(&state, k, def).await;
    let run = start_test(&state, k, &wf, json!({ "real_writes": true })).await;
    worker::tick(&state, &opts()).await.unwrap();
    let r = run_of(&state, k, run).await;
    assert_eq!(r["status"], "error");
    assert_eq!(r["error"]["code"], "aikido_lead_not_found");
    assert_eq!(r["error"]["node_id"], "read");
    assert_eq!(node(&r, "read")["status"], "error");
    let list = call(&state, "GET", &format!("{RUNS}?status=error"), Some(k), None).await.body;
    assert_eq!(list["runs"][0]["error_code"], "aikido_lead_not_found");
}

#[sqlx::test]
async fn only_errors_on_the_tested_path_stop_a_test(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Automations];
    // `broken` is not reachable from the trigger: its error does not matter
    let def = json!({ "format": "aikido-workflow", "version": 1, "name": "x",
        "nodes": [{ "id": "start", "type": "trigger.manual" }, { "id": "ok", "type": "flow.noop" }, { "id": "broken", "type": "web.fetch_pag" }],
        "edges": [{ "from": "start", "to": "ok" }] });
    let wf = make(&state, k, def).await;
    let run = start_test(&state, k, &wf, json!({})).await;
    worker::tick(&state, &opts()).await.unwrap();
    assert_eq!(run_of(&state, k, run).await["status"], "success");

    // an error on the path answers 422 with the validation
    let def = json!({ "format": "aikido-workflow", "version": 1, "name": "x",
        "nodes": [{ "id": "start", "type": "trigger.manual" }, { "id": "broken", "type": "web.fetch_pag" }], "edges": [{ "from": "start", "to": "broken" }] });
    let wf2 = make(&state, k, def).await;
    let r = call(&state, "POST", &format!("{WF}/{wf2}/test"), Some(k), Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.body["error"]["code"], "workflow_has_errors");
    assert_eq!(r.body["error"]["validation"]["errors"][0]["code"], "unknown_node_type");

    // the other 422s
    let r = call(&state, "POST", &format!("{WF}/{wf}/test"), Some(k), Some(json!({ "trigger_node_id": "ok" }))).await;
    assert_eq!(r.body["error"]["code"], "invalid_trigger_node");
    let r = call(&state, "POST", &format!("{WF}/{wf}/test"), Some(k), Some(json!({ "only_node": "broken" }))).await;
    assert_eq!(r.body["error"]["code"], "invalid_only_node");
    let r = call(&state, "POST", &format!("{WF}/{wf}/test"), Some(k), Some(json!({ "trigger_item": [1] }))).await;
    assert_eq!(r.body["error"]["code"], "invalid_trigger_item");
    let r = call(&state, "POST", &format!("{WF}/{}/test", Uuid::now_v7()), Some(k), Some(json!({}))).await;
    assert_eq!(r.body["error"]["code"], "workflow_not_found");
}

#[sqlx::test]
async fn a_required_variable_needs_a_value_and_can_be_given_per_run(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Automations];
    let def = json!({ "format": "aikido-workflow", "version": 1, "name": "x",
        "variables": [{ "key": "city", "kind": "text", "value": null, "required": true }],
        "nodes": [{ "id": "start", "type": "trigger.manual" }, { "id": "s", "type": "data.set", "params": { "fields": { "city": "${{ vars.city }}" } } }],
        "edges": [{ "from": "start", "to": "s" }] });
    let wf = make(&state, k, def).await;
    let r = call(&state, "POST", &format!("{WF}/{wf}/test"), Some(k), Some(json!({}))).await;
    assert_eq!(r.body["error"]["code"], "variable_missing");
    let run = start_test(&state, k, &wf, json!({ "variables": { "city": "Dhaka" } })).await;
    worker::tick(&state, &opts()).await.unwrap();
    let done = run_of(&state, k, run).await;
    assert_eq!(node(&done, "s")["output"]["main"][0]["city"], "Dhaka");
}

#[sqlx::test]
async fn side_effect_nodes_run_dry_and_pins_replace_nodes(pool: PgPool) {
    let state = app_state(pool.clone());
    let keys = bootstrap_keys(&state).await;
    let _owner = setup_owner(&state).await; // a key run acts as the oldest owner
    let k = &keys[&Scope::Automations];
    let def = json!({ "format": "aikido-workflow", "version": 1, "name": "x",
        "nodes": [{ "id": "start", "type": "trigger.manual" },
                  { "id": "read", "type": "web.fetch_page", "params": { "url": "acme.com" } },
                  { "id": "upd", "type": "aikido.lead.update", "params": { "lead_id": "0192f3a0-0000-7000-8000-000000000001", "fields": { "note": "${{ json.title }}" } } }],
        "edges": [{ "from": "start", "to": "read" }, { "from": "read", "to": "upd" }],
        "pins": { "read": [{ "title": "Pinned" }] } });
    let wf = make(&state, k, def).await;
    let leads_before: i64 = sqlx::query_scalar("SELECT count(*) FROM leads").fetch_one(&pool).await.unwrap();
    let run = start_test(&state, k, &wf, json!({})).await;
    worker::tick(&state, &opts()).await.unwrap();
    let done = run_of(&state, k, run).await;
    assert_eq!(done["status"], "success", "{done}");
    assert_eq!(node(&done, "read")["status"], "success");
    assert_eq!(node(&done, "upd")["status"], "dry");
    assert_eq!(node(&done, "upd")["output"]["main"][0]["would_call"]["body"]["note"], "Pinned");
    let leads_after: i64 = sqlx::query_scalar("SELECT count(*) FROM leads").fetch_one(&pool).await.unwrap();
    assert_eq!(leads_before, leads_after);

    // "Real writes" calls the node for real: the lead does not exist, so the route answers an error
    let run = start_test(&state, k, &wf, json!({ "real_writes": true })).await;
    worker::tick(&state, &opts()).await.unwrap();
    let r = run_of(&state, k, run).await;
    assert_eq!(r["error"]["code"], "aikido_lead_not_found");
}

#[sqlx::test]
async fn a_test_step_runs_only_the_path_to_the_node(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Automations];
    let wf = make(&state, k, flow()).await;
    let run = start_test(&state, k, &wf, json!({ "only_node": "big", "trigger_item": { "input": { "company": "A", "leads": [{ "n": 7 }] } } })).await;
    worker::tick(&state, &opts()).await.unwrap();
    let done = run_of(&state, k, run).await;
    assert_eq!(done["status"], "success");
    assert!(done["nodes"].as_array().unwrap().iter().all(|n| n["node_id"] != "tag"));
    assert_eq!(done["options"]["only_node"], "big");
}

#[sqlx::test]
async fn a_long_wait_parks_the_run_and_the_worker_wakes_it(pool: PgPool) {
    let mut state = app_state(pool.clone());
    let clock = shared_clock(&mut state, utc(2026, 10, 25, 9, 0));
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Automations];
    let def = json!({ "format": "aikido-workflow", "version": 1, "name": "x",
        "nodes": [{ "id": "start", "type": "trigger.manual" }, { "id": "w", "type": "flow.wait", "params": { "mode": "duration", "amount": 2, "unit": "minutes" } },
                  { "id": "after", "type": "data.set", "params": { "fields": { "done": true } } }],
        "edges": [{ "from": "start", "to": "w" }, { "from": "w", "to": "after" }] });
    let wf = make(&state, k, def).await;
    let run = start_test(&state, k, &wf, json!({})).await;
    worker::tick(&state, &opts()).await.unwrap();
    let r = run_of(&state, k, run).await;
    assert_eq!(r["status"], "waiting", "{r}");
    assert_eq!(r["resume_at"].as_str().unwrap(), "2026-10-25T09:02:00Z");
    assert!(r["nodes"].as_array().unwrap().iter().all(|n| n["node_id"] != "after"));
    let state_kept: bool = sqlx::query_scalar("SELECT state IS NOT NULL FROM automation_runs WHERE id = $1").bind(run).fetch_one(&pool).await.unwrap();
    assert!(state_kept);

    // not yet
    *clock.lock().unwrap() += Duration::seconds(90);
    assert_eq!(worker::tick(&state, &opts()).await.unwrap(), 0);
    assert_eq!(run_of(&state, k, run).await["status"], "waiting");
    // now
    *clock.lock().unwrap() += Duration::seconds(40);
    assert_eq!(worker::tick(&state, &opts()).await.unwrap(), 1);
    let r = run_of(&state, k, run).await;
    assert_eq!(r["status"], "success", "{r}");
    assert_eq!(node(&r, "after")["output"]["main"][0]["done"], true);
    // the Wait node ran once
    assert_eq!(r["nodes"].as_array().unwrap().iter().filter(|n| n["node_id"] == "w").count(), 1);
    let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM automation_run_events WHERE run_id = $1 ORDER BY id").bind(run).fetch_all(&pool).await.unwrap();
    assert!(kinds.contains(&"waiting".to_string()) && kinds.contains(&"resumed".to_string()), "{kinds:?}");
}

#[sqlx::test]
async fn a_run_of_a_dead_server_copy_is_taken_back_and_finished(pool: PgPool) {
    let mut state = app_state(pool.clone());
    let clock = shared_clock(&mut state, utc(2026, 10, 25, 9, 0));
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Automations];
    let wf = make(&state, k, flow()).await;
    let run = start_test(&state, k, &wf, json!({ "trigger_item": { "input": { "company": "A", "leads": [{ "n": 7 }] } } })).await;
    // a copy claimed the run and died: status running, a lease that is over
    sqlx::query("UPDATE automation_runs SET status = 'running', claimed_by = 'dead-copy', lease_until = $2, started_at = $2 WHERE id = $1")
        .bind(run)
        .bind(utc(2026, 10, 25, 8, 58))
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(worker::tick(&state, &opts()).await.unwrap(), 1);
    let r = run_of(&state, k, run).await;
    assert_eq!(r["status"], "success", "{r}");
    let attempt: i32 = sqlx::query_scalar("SELECT attempt FROM automation_runs WHERE id = $1").bind(run).fetch_one(&pool).await.unwrap();
    assert_eq!(attempt, 1);
    // a live lease is respected
    let run2 = start_test(&state, k, &wf, json!({})).await;
    sqlx::query("UPDATE automation_runs SET status = 'running', claimed_by = 'live-copy', lease_until = $2 WHERE id = $1")
        .bind(run2)
        .bind(*clock.lock().unwrap() + Duration::seconds(30))
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(worker::tick(&state, &opts()).await.unwrap(), 0);
    assert_eq!(run_of(&state, k, run2).await["status"], "running");
}

#[sqlx::test]
async fn a_run_goes_on_from_its_checkpoint_after_a_crash(pool: PgPool) {
    use aikido_email::automations::engine::state::RunState;
    let mut state = app_state(pool.clone());
    let _clock = shared_clock(&mut state, utc(2026, 10, 25, 9, 0));
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Automations];
    let wf = make(&state, k, flow()).await;
    let run = start_test(&state, k, &wf, json!({ "trigger_item": { "input": { "company": "A", "leads": [{ "n": 7 }, { "n": 2 }] } } })).await;
    // run it once in full to get a real checkpoint, then rewind: keep only the first two nodes
    worker::tick(&state, &opts()).await.unwrap();
    let full: Option<Vec<u8>> = sqlx::query_scalar("SELECT state FROM automation_runs WHERE id = $1").bind(run).fetch_one(&pool).await.unwrap();
    assert!(full.is_none(), "a finished run keeps no checkpoint");

    let run2 = start_test(&state, k, &wf, json!({ "trigger_item": { "input": { "company": "A", "leads": [{ "n": 7 }, { "n": 2 }] } } })).await;
    let mut cp = RunState::default();
    // the checkpoint after `start` and `split` (what the first copy saved before it died)
    let item = |n: i64, i: usize| aikido_email::automations::registry::Item { json: json!({ "n": n }).as_object().unwrap().clone(), lineage: vec![("start".into(), 0), ("split".into(), i)] };
    let start_item = aikido_email::automations::registry::Item { json: json!({ "input": { "company": "A", "leads": [{ "n": 7 }, { "n": 2 }] } }).as_object().unwrap().clone(), lineage: vec![("start".into(), 0)] };
    cp.outputs.insert("start".into(), aikido_email::automations::engine::state::NodeOut { items: vec![start_item], ports: [("main".to_string(), vec![0])].into() });
    cp.outputs.insert("split".into(), aikido_email::automations::engine::state::NodeOut { items: vec![item(7, 0), item(2, 1)], ports: [("main".to_string(), vec![0, 1])].into() });
    cp.finished.extend(["start".to_string(), "split".to_string()]);
    cp.seq = 2;
    sqlx::query("UPDATE automation_runs SET state = $2, status = 'running', claimed_by = 'dead-copy', lease_until = $3 WHERE id = $1")
        .bind(run2)
        .bind(cp.encode().unwrap())
        .bind(utc(2026, 10, 25, 8, 0))
        .execute(&pool)
        .await
        .unwrap();
    // the interrupted node row
    sqlx::query("INSERT INTO automation_run_nodes (run_id, node_id, seq, status, started_at) VALUES ($1, 'big', 2, 'running', $2)").bind(run2).bind(utc(2026, 10, 25, 8, 0)).execute(&pool).await.unwrap();
    worker::tick(&state, &opts()).await.unwrap();
    let r = run_of(&state, k, run2).await;
    assert_eq!(r["status"], "success", "{r}");
    // start and split did not run again: they have no rows in this run
    assert!(r["nodes"].as_array().unwrap().iter().all(|n| n["node_id"] != "start" && n["node_id"] != "split"));
    assert_eq!(node(&r, "big")["status"], "success");
    assert_eq!(node(&r, "tag")["output"]["main"], json!([{ "n": 7, "who": "A" }]));
    let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM automation_run_events WHERE run_id = $1").bind(run2).fetch_all(&pool).await.unwrap();
    assert!(kinds.contains(&"node_restarted".to_string()), "{kinds:?}");
}

#[sqlx::test]
async fn cancel_ends_queued_runs_and_refuses_finished_ones(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Automations];
    let wf = make(&state, k, flow()).await;
    let run = start_test(&state, k, &wf, json!({})).await;
    let c = call(&state, "POST", &format!("{RUNS}/{run}/cancel"), Some(k), None).await;
    assert_eq!((c.status, c.body["status"].as_str()), (StatusCode::OK, Some("cancelled")));
    assert_eq!(worker::tick(&state, &opts()).await.unwrap(), 0, "a cancelled run is not claimed");
    let again = call(&state, "POST", &format!("{RUNS}/{run}/cancel"), Some(k), None).await;
    assert_eq!((again.status, again.body["error"]["code"].as_str()), (StatusCode::CONFLICT, Some("run_finished")));
    let gone = call(&state, "POST", &format!("{RUNS}/{}/cancel", Uuid::now_v7()), Some(k), None).await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn cancel_of_a_waiting_run_ends_it_at_once(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Automations];
    let def = json!({ "format": "aikido-workflow", "version": 1, "name": "x",
        "nodes": [{ "id": "start", "type": "trigger.manual" }, { "id": "w", "type": "flow.wait", "params": { "mode": "duration", "amount": 1, "unit": "hours" } }],
        "edges": [{ "from": "start", "to": "w" }] });
    let wf = make(&state, k, def).await;
    let run = start_test(&state, k, &wf, json!({})).await;
    worker::tick(&state, &opts()).await.unwrap();
    assert_eq!(run_of(&state, k, run).await["status"], "waiting");
    let c = call(&state, "POST", &format!("{RUNS}/{run}/cancel"), Some(k), None).await;
    assert_eq!(c.body["status"], "cancelled");
    assert_eq!(run_of(&state, k, run).await["status"], "cancelled");
}

/// A published version and a production run, made by hand (publish comes in phase 5).
async fn production_run(pool: &PgPool, state: &AppState, def: &Value, max_concurrent: Option<i64>) -> Uuid {
    let mut def = def.clone();
    if let Some(m) = max_concurrent {
        def["settings"] = json!({ "max_concurrent_runs": m });
    }
    let wf = Uuid::now_v7();
    sqlx::query("INSERT INTO automation_workflows (id, name, draft, draft_hash) VALUES ($1, 'p', $2, 'h')").bind(wf).bind(&def).execute(pool).await.unwrap();
    let ver = Uuid::now_v7();
    sqlx::query("INSERT INTO automation_versions (id, workflow_id, version, definition) VALUES ($1, $2, 1, $3)").bind(ver).bind(wf).bind(&def).execute(pool).await.unwrap();
    production_run_of(pool, state, wf, ver).await
}

async fn production_run_of(pool: &PgPool, state: &AppState, wf: Uuid, ver: Uuid) -> Uuid {
    let run = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO automation_runs (id, workflow_id, version_id, mode, status, trigger_node_id, trigger_item, queued_at) VALUES ($1, $2, $3, 'production', 'queued', 'start', '{}', $4)",
    )
    .bind(run)
    .bind(wf)
    .bind(ver)
    .bind(state.now())
    .execute(pool)
    .await
    .unwrap();
    run
}

#[sqlx::test]
async fn pause_holds_production_runs_but_not_test_runs(pool: PgPool) {
    let state = app_state(pool.clone());
    let keys = bootstrap_keys(&state).await;
    let _owner = setup_owner(&state).await; // a production run without a publisher acts as the oldest owner
    let k = &keys[&Scope::Automations];
    let prod = production_run(&pool, &state, &json!({ "format": "aikido-workflow", "version": 1, "name": "p", "nodes": [{ "id": "start", "type": "trigger.manual" }], "edges": [] }), None).await;
    let wf = make(&state, k, flow()).await;
    let test = start_test(&state, k, &wf, json!({ "trigger_item": { "input": { "company": "A", "leads": [{ "n": 7 }] } } })).await;

    // a normal key may not pause; the admin key may
    let denied = call(&state, "POST", "/v1/automations/engine/pause", Some(k), None).await;
    assert_eq!((denied.status, denied.body["error"]["code"].as_str()), (StatusCode::FORBIDDEN, Some("admin_only")));
    let admin = &keys[&Scope::Admin];
    let paused = call(&state, "POST", "/v1/automations/engine/pause", Some(admin), None).await;
    assert_eq!(paused.status, StatusCode::OK, "{:?}", paused.body);
    assert_eq!(paused.body["paused"], true);

    assert_eq!(worker::tick(&state, &opts()).await.unwrap(), 1, "only the test run");
    assert_eq!(run_of(&state, k, test).await["status"], "success");
    assert_eq!(run_of(&state, k, prod).await["status"], "queued");
    let st = call(&state, "GET", "/v1/automations/status", Some(k), None).await.body;
    assert_eq!(st["paused"], true);
    assert_eq!(st["queue"], json!([{ "status": "queued", "mode": "production", "count": 1 }]));

    call(&state, "POST", "/v1/automations/engine/resume", Some(admin), None).await;
    assert_eq!(worker::tick(&state, &opts()).await.unwrap(), 1);
    let r = run_of(&state, k, prod).await;
    assert_eq!((r["status"].as_str(), r["mode"].as_str(), r["version"].as_i64()), (Some("success"), Some("production"), Some(1)));
}

#[sqlx::test]
async fn max_concurrent_runs_limits_production_runs_of_one_workflow(pool: PgPool) {
    let mut state = app_state(pool.clone());
    let clock = shared_clock(&mut state, utc(2026, 10, 25, 9, 0));
    let keys = bootstrap_keys(&state).await;
    let _owner = setup_owner(&state).await; // a production run without a publisher acts as the oldest owner
    let k = &keys[&Scope::Automations];
    let def = json!({ "format": "aikido-workflow", "version": 1, "name": "p", "nodes": [{ "id": "start", "type": "trigger.manual" }], "edges": [] });
    let first = production_run(&pool, &state, &def, Some(1)).await;
    let (wf, ver): (Uuid, Uuid) = sqlx::query_as("SELECT workflow_id, version_id FROM automation_runs WHERE id = $1").bind(first).fetch_one(&pool).await.unwrap();
    let second = production_run_of(&pool, &state, wf, ver).await;
    // the first one is running on another copy, with a live lease
    sqlx::query("UPDATE automation_runs SET status = 'running', claimed_by = 'other', lease_until = $2 WHERE id = $1")
        .bind(first)
        .bind(*clock.lock().unwrap() + Duration::seconds(50))
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(worker::tick(&state, &opts()).await.unwrap(), 0);
    assert_eq!(run_of(&state, k, second).await["status"], "queued");
    // when the first one ends, the second one goes
    sqlx::query("UPDATE automation_runs SET status = 'success', finished_at = $2 WHERE id = $1").bind(first).bind(*clock.lock().unwrap()).execute(&pool).await.unwrap();
    assert_eq!(worker::tick(&state, &opts()).await.unwrap(), 1);
    assert_eq!(run_of(&state, k, second).await["status"], "success");
}

#[sqlx::test]
async fn save_data_errors_only_keeps_no_output_in_production(pool: PgPool) {
    let state = app_state(pool.clone());
    let keys = bootstrap_keys(&state).await;
    let _owner = setup_owner(&state).await; // a production run without a publisher acts as the oldest owner
    let k = &keys[&Scope::Automations];
    let def = json!({ "format": "aikido-workflow", "version": 1, "name": "p", "settings": { "save_data": "errors_only" },
        "nodes": [{ "id": "start", "type": "trigger.manual" }, { "id": "a", "type": "flow.noop" }], "edges": [{ "from": "start", "to": "a" }] });
    let run = production_run(&pool, &state, &def, None).await;
    worker::tick(&state, &opts()).await.unwrap();
    let r = run_of(&state, k, run).await;
    assert_eq!(r["status"], "success");
    assert_eq!(node(&r, "a")["output"], Value::Null);
    assert_eq!(node(&r, "a")["output_count"]["main"], 1);
}

#[sqlx::test]
async fn cleanup_deletes_old_runs_and_keeps_the_newest_test_runs(pool: PgPool) {
    let mut state = app_state(pool.clone());
    let clock = shared_clock(&mut state, utc(2026, 10, 25, 9, 0));
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Automations];
    let wf = make(&state, k, flow()).await;
    let wf_id: Uuid = wf.parse().unwrap();
    let old = utc(2026, 8, 1, 9, 0);
    // 25 finished test runs from today, 1 test run from last month, 1 old and 1 new production run
    for i in 0..25 {
        let at = *clock.lock().unwrap() - Duration::minutes(i);
        sqlx::query("INSERT INTO automation_runs (id, workflow_id, definition, mode, status, trigger_node_id, trigger_item, queued_at, finished_at) VALUES ($1, $2, '{}', 'test', 'success', 'start', '{}', $3, $3)")
            .bind(Uuid::now_v7()).bind(wf_id).bind(at).execute(&pool).await.unwrap();
    }
    sqlx::query("INSERT INTO automation_runs (id, workflow_id, definition, mode, status, trigger_node_id, trigger_item, queued_at, finished_at) VALUES ($1, $2, '{}', 'test', 'error', 'start', '{}', $3, $3)")
        .bind(Uuid::now_v7()).bind(wf_id).bind(old).execute(&pool).await.unwrap();
    for (at, label) in [(old, "old"), (*clock.lock().unwrap() - Duration::days(1), "new")] {
        let _ = label;
        sqlx::query("INSERT INTO automation_runs (id, workflow_id, mode, status, trigger_node_id, trigger_item, queued_at, finished_at) VALUES ($1, $2, 'production', 'success', 'start', '{}', $3, $3)")
            .bind(Uuid::now_v7()).bind(wf_id).bind(at).execute(&pool).await.unwrap();
    }
    // a run that is still going is never deleted, however old
    sqlx::query("INSERT INTO automation_runs (id, workflow_id, mode, status, trigger_node_id, trigger_item, queued_at) VALUES ($1, $2, 'production', 'waiting', 'start', '{}', $3)")
        .bind(Uuid::now_v7()).bind(wf_id).bind(old).execute(&pool).await.unwrap();
    worker::cleanup(&state).await.unwrap();
    let tests: i64 = sqlx::query_scalar("SELECT count(*) FROM automation_runs WHERE mode = 'test'").fetch_one(&pool).await.unwrap();
    assert_eq!(tests, 20, "the newest 20 test runs stay");
    let prod: Vec<String> = sqlx::query_scalar("SELECT status FROM automation_runs WHERE mode = 'production' ORDER BY status").fetch_all(&pool).await.unwrap();
    assert_eq!(prod, vec!["success", "waiting"]);
}
