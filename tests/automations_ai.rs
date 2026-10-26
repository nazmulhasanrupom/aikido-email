//! Automations, phase 4: the AI Agent node on the real routes, the generator's provider store and `llm_calls`.
//! These tests need Postgres (not run in the session that wrote them).
mod common;
use aikido_email::auth::api_keys::Scope;
use aikido_email::automations::engine::worker::{self, WorkerOpts};
use aikido_email::generator::llm::fake::{FakeFactory, FakeLlm, response};
use aikido_email::state::AppState;
use axum::http::StatusCode;
use chrono::Duration;
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

const WF: &str = "/v1/automations/workflows";
const RUNS: &str = "/v1/automations/runs";

struct Ctx {
    state: AppState,
    fake: Arc<FakeLlm>,
    keys: std::collections::HashMap<Scope, String>,
    provider: Uuid,
}

impl Ctx {
    async fn new(pool: PgPool) -> Self {
        let mut state = app_state(pool);
        let fake = FakeLlm::new();
        state.llm = Arc::new(FakeFactory(fake.clone()));
        let keys = bootstrap_keys(&state).await;
        let p = call(
            &state,
            "POST",
            "/v1/generator/providers",
            Some(&keys[&Scope::Generator]),
            Some(json!({ "kind": "openai_compatible", "name": "Fake", "base_url": "http://127.0.0.1:1/v1", "model": "fake-model" })),
        )
        .await;
        assert!(p.status.is_success(), "{:?}", p.body);
        let provider: Uuid = p.body["id"].as_str().unwrap().parse().unwrap();
        let c = Ctx { state, fake, keys, provider };
        c.generator_settings(json!({ "slot_provider_id": provider, "draft_provider_id": provider })).await;
        c
    }

    async fn generator_settings(&self, patch: Value) {
        let r = call(&self.state, "PATCH", "/v1/generator/settings", Some(&self.keys[&Scope::Generator]), Some(patch)).await;
        assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    }

    fn k(&self) -> &str {
        &self.keys[&Scope::Automations]
    }

    async fn make(&self, def: Value) -> String {
        let r = call(&self.state, "POST", WF, Some(self.k()), Some(json!({ "name": "AI", "definition": def }))).await;
        assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
        r.body["id"].as_str().unwrap().to_string()
    }

    async fn start(&self, wf: &str) -> Uuid {
        let r = call(&self.state, "POST", &format!("{WF}/{wf}/test"), Some(self.k()), Some(json!({}))).await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{:?}", r.body);
        r.body["run_id"].as_str().unwrap().parse().unwrap()
    }

    async fn run(&self, id: Uuid) -> Value {
        let r = call(&self.state, "GET", &format!("{RUNS}/{id}"), Some(self.k()), None).await;
        assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
        r.body
    }
}

fn agent_flow(params: Value) -> Value {
    json!({
        "format": "aikido-workflow", "version": 1, "name": "x",
        "nodes": [{ "id": "start", "type": "trigger.manual" }, { "id": "write", "type": "ai.agent", "params": params }],
        "edges": [{ "from": "start", "to": "write" }]
    })
}

fn node<'a>(run: &'a Value, id: &str) -> &'a Value {
    run["nodes"].as_array().unwrap().iter().find(|n| n["node_id"] == id).unwrap_or_else(|| panic!("no row for {id}: {run}"))
}

#[sqlx::test]
async fn the_agent_uses_the_generator_provider_and_writes_an_llm_calls_row_with_the_run_ids(pool: PgPool) {
    let c = Ctx::new(pool.clone()).await;
    c.fake.push(Ok(response("{\"subject\": \"Hello \u{2014} Ana\", \"body\": \"Hi Ana\"}")));
    let wf = c.make(agent_flow(json!({
        "prompt": "Write to Ana.",
        "output": { "format": "json", "fields": [{ "key": "subject", "type": "string" }, { "key": "body", "type": "string" }] }
    }))).await;
    let run_id = c.start(&wf).await;
    worker::tick(&c.state, &WorkerOpts::default()).await.unwrap();

    let run = c.run(run_id).await;
    assert_eq!(run["status"], "success", "{run}");
    assert_eq!(node(&run, "write")["output"]["main"][0]["subject"], "Hello, Ana", "dashes are stripped");

    let row: (String, Option<Uuid>, Option<Uuid>, Option<Uuid>, Option<String>, bool) =
        sqlx::query_as("SELECT purpose, provider_id, automation_run_id, workflow_id, node_id, ok FROM llm_calls WHERE automation_run_id = $1").bind(run_id).fetch_one(&pool).await.unwrap();
    assert_eq!(row.0, "automation");
    assert_eq!(row.1, Some(c.provider));
    assert_eq!(row.2, Some(run_id));
    assert_eq!(row.3, Some(wf.parse().unwrap()));
    assert_eq!(row.4.as_deref(), Some("write"));
    assert!(row.5);

    // the generator's usage report groups the new purpose like the others
    let usage = call(&c.state, "GET", "/v1/generator/usage", Some(&c.keys[&Scope::Generator]), None).await;
    assert_eq!(usage.status, StatusCode::OK);
    assert!(usage.body["by_purpose"].as_array().unwrap().iter().any(|r| r["key"] == "automation"), "{}", usage.body);
}

#[sqlx::test]
async fn a_provider_can_be_picked_by_name_or_id_and_an_unknown_one_is_refused(pool: PgPool) {
    let c = Ctx::new(pool).await;
    for which in ["Fake", "fake", &c.provider.to_string()] {
        let wf = c.make(agent_flow(json!({ "prompt": "Hi", "provider": which }))).await;
        let run_id = c.start(&wf).await;
        worker::tick(&c.state, &WorkerOpts::default()).await.unwrap();
        assert_eq!(c.run(run_id).await["status"], "success", "provider {which}");
    }
    let def = agent_flow(json!({ "prompt": "Hi", "provider": "gpt-nowhere" }));
    let v = call(&c.state, "POST", "/v1/automations/validate", Some(c.k()), Some(json!({ "definition": def }))).await;
    assert!(v.body["errors"].as_array().unwrap().iter().any(|e| e["code"] == "unknown_provider"), "{}", v.body);
}

#[sqlx::test]
async fn no_provider_set_fails_the_node_with_ai_not_configured(pool: PgPool) {
    let c = Ctx::new(pool).await;
    c.generator_settings(json!({ "draft_provider_id": null })).await;
    let wf = c.make(agent_flow(json!({ "prompt": "Hi" }))).await;
    let run_id = c.start(&wf).await;
    worker::tick(&c.state, &WorkerOpts::default()).await.unwrap();
    let run = c.run(run_id).await;
    assert_eq!(run["status"], "error");
    assert_eq!(run["error"]["code"], "ai_not_configured", "{run}");
}

#[sqlx::test]
async fn a_used_up_daily_budget_parks_the_run_and_it_goes_on_when_the_budget_is_back(pool: PgPool) {
    let mut state = app_state(pool.clone());
    let fake = FakeLlm::new();
    state.llm = Arc::new(FakeFactory(fake.clone()));
    let clock = shared_clock(&mut state, utc(2026, 10, 25, 9, 0));
    let keys = bootstrap_keys(&state).await;
    let mut c = Ctx { state, fake, keys, provider: Uuid::nil() };
    let p = call(&c.state, "POST", "/v1/generator/providers", Some(&c.keys[&Scope::Generator]), Some(json!({ "kind": "openai_compatible", "name": "Fake", "base_url": "http://127.0.0.1:1/v1", "model": "fake-model" }))).await;
    c.provider = p.body["id"].as_str().unwrap().parse().unwrap();
    // the first call uses 120 tokens (fake: 100 in + 20 out), so a budget of 100 is used up after it
    c.generator_settings(json!({ "draft_provider_id": c.provider, "slot_provider_id": c.provider, "daily_token_budget": 100 })).await;

    let wf = c.make(agent_flow(json!({ "prompt": "Hi" }))).await;
    let first = c.start(&wf).await;
    worker::tick(&c.state, &WorkerOpts::default()).await.unwrap();
    assert_eq!(c.run(first).await["status"], "success");
    assert_eq!(c.fake.call_count(), 1);

    // the second run: the budget is used up, so the run waits 10 minutes and makes no call
    let second = c.start(&wf).await;
    worker::tick(&c.state, &WorkerOpts::default()).await.unwrap();
    let run = c.run(second).await;
    assert_eq!(run["status"], "waiting", "{run}");
    assert_eq!(run["resume_at"].as_str().unwrap(), "2026-10-25T09:10:00Z");
    assert_eq!(node(&run, "write")["status"], "waiting");
    assert_eq!(c.fake.call_count(), 1);

    // not yet; then 10 minutes pass but the budget is still used up: it waits again
    *clock.lock().unwrap() += Duration::minutes(10);
    worker::tick(&c.state, &WorkerOpts::default()).await.unwrap();
    assert_eq!(c.run(second).await["status"], "waiting");
    assert_eq!(c.fake.call_count(), 1);

    // the owner raises the budget: the next look makes the call and the run ends well
    c.generator_settings(json!({ "daily_token_budget": 100_000 })).await;
    *clock.lock().unwrap() += Duration::minutes(10);
    worker::tick(&c.state, &WorkerOpts::default()).await.unwrap();
    let run = c.run(second).await;
    assert_eq!(run["status"], "success", "{run}");
    assert_eq!(c.fake.call_count(), 2);
}

#[sqlx::test]
async fn a_budget_wait_longer_than_the_setting_fails_the_run(pool: PgPool) {
    let mut state = app_state(pool);
    let fake = FakeLlm::new();
    state.llm = Arc::new(FakeFactory(fake.clone()));
    let clock = shared_clock(&mut state, utc(2026, 10, 25, 9, 0));
    let keys = bootstrap_keys(&state).await;
    let mut c = Ctx { state, fake, keys, provider: Uuid::nil() };
    let p = call(&c.state, "POST", "/v1/generator/providers", Some(&c.keys[&Scope::Generator]), Some(json!({ "kind": "openai_compatible", "name": "Fake", "base_url": "http://127.0.0.1:1/v1", "model": "fake-model" }))).await;
    c.provider = p.body["id"].as_str().unwrap().parse().unwrap();
    // a budget of 1 token is used up by any call; make one call by hand so the table has a row for today
    c.generator_settings(json!({ "draft_provider_id": c.provider, "daily_token_budget": 1 })).await;
    let t = call(&c.state, "POST", &format!("/v1/generator/providers/{}/test", c.provider), Some(&c.keys[&Scope::Generator]), None).await;
    assert_eq!(t.status, StatusCode::OK, "{:?}", t.body);
    let s = call(&c.state, "PATCH", "/v1/automations/settings", Some(&c.keys[&Scope::Admin]), Some(json!({ "ai_budget_wait_hours": 1 }))).await;
    assert_eq!(s.status, StatusCode::OK, "{:?}", s.body);

    let wf = c.make(agent_flow(json!({ "prompt": "Hi" }))).await;
    let run_id = c.start(&wf).await;
    worker::tick(&c.state, &WorkerOpts::default()).await.unwrap();
    assert_eq!(c.run(run_id).await["status"], "waiting");

    *clock.lock().unwrap() += Duration::minutes(70);
    worker::tick(&c.state, &WorkerOpts::default()).await.unwrap();
    let run = c.run(run_id).await;
    assert_eq!(run["status"], "error", "{run}");
    assert_eq!(run["error"]["code"], "ai_budget_reached");
    assert!(run["error"]["message"].as_str().unwrap().contains("1 hours"));
}

#[sqlx::test]
async fn the_node_is_in_the_catalog_with_the_field_builder(pool: PgPool) {
    let c = Ctx::new(pool).await;
    let r = call(&c.state, "GET", "/v1/automations/nodes", Some(c.k()), None).await;
    assert_eq!(r.status, StatusCode::OK);
    let nodes = r.body["nodes"].as_array().or_else(|| r.body.as_array()).expect("a node list");
    let agent = nodes.iter().find(|n| n["type"] == "ai.agent").expect("ai.agent");
    assert_eq!(agent["params_schema"]["properties"]["output"]["properties"]["fields"]["x-ui"]["widget"], "fields");
}
