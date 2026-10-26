//! Automations, phase 8: the AI box (copilot) on the real routes with the fake provider.
//! These tests need Postgres (not run in the session that wrote them). The pure parts (diff, positions, parsing,
//! repair, summary folding, state block, rate limit) are unit tests in `src/automations/copilot/`.
mod common;
use aikido_email::auth::api_keys::Scope;
use aikido_email::generator::llm::fake::{FakeFactory, FakeLlm, response};
use aikido_email::state::AppState;
use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::Arc;

const WF: &str = "/v1/automations/workflows";
const TH: &str = "/v1/automations/copilot/threads";

struct Ctx {
    state: AppState,
    fake: Arc<FakeLlm>,
    keys: std::collections::HashMap<Scope, String>,
}

impl Ctx {
    async fn new(pool: PgPool, with_provider: bool) -> Self {
        let mut state = app_state(pool);
        let fake = FakeLlm::new();
        state.llm = Arc::new(FakeFactory(fake.clone()));
        let keys = bootstrap_keys(&state).await;
        let c = Ctx { state, fake, keys };
        if with_provider {
            let p = call(&c.state, "POST", "/v1/generator/providers", Some(&c.keys[&Scope::Generator]), Some(json!({ "kind": "openai_compatible", "name": "Fake", "base_url": "http://127.0.0.1:1/v1", "model": "fake-model" }))).await;
            assert!(p.status.is_success(), "{:?}", p.body);
            let r = call(&c.state, "PATCH", "/v1/generator/settings", Some(&c.keys[&Scope::Generator]), Some(json!({ "draft_provider_id": p.body["id"] }))).await;
            assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
        }
        c
    }

    fn k(&self) -> &str {
        &self.keys[&Scope::Automations]
    }

    async fn thread(&self, workflow: Option<&str>) -> String {
        let r = call(&self.state, "POST", TH, Some(self.k()), Some(json!({ "workflow_id": workflow }))).await;
        assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
        r.body["id"].as_str().unwrap().to_string()
    }

    async fn say(&self, thread: &str, text: &str) -> Response {
        call(&self.state, "POST", &format!("{TH}/{thread}/messages"), Some(self.k()), Some(json!({ "text": text }))).await
    }

    async fn draft(&self, wf: &str) -> Value {
        let r = call(&self.state, "GET", &format!("{WF}/{wf}"), Some(self.k()), None).await;
        assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
        r.body["draft"].clone()
    }
}

fn flow(name: &str, extra_node: Option<&str>) -> Value {
    let mut nodes = vec![json!({ "id": "start", "type": "trigger.manual" }), json!({ "id": "pass", "type": "flow.noop" })];
    let mut edges = vec![json!({ "from": "start", "to": "pass" })];
    if let Some(id) = extra_node {
        nodes.push(json!({ "id": id, "type": "flow.noop" }));
        edges.push(json!({ "from": "pass", "to": id }));
    }
    json!({ "format": "aikido-workflow", "version": 1, "name": name, "nodes": nodes, "edges": edges })
}

fn answer(action: &str, w: Option<Value>) -> String {
    json!({ "reply": "Done.", "action": action, "workflow": w }).to_string()
}

#[sqlx::test]
async fn create_from_the_list_page_makes_a_workflow_and_binds_the_thread(pool: PgPool) {
    let c = Ctx::new(pool.clone(), true).await;
    let t = c.thread(None).await;
    c.fake.push(Ok(response(answer("create", Some(flow("From chat", None))))));
    let r = c.say(&t, "Make a flow").await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["message"]["applied"]["mode"], "create");
    let wf = r.body["workflow_id"].as_str().unwrap();
    assert_eq!(r.body["message"]["applied"]["diff"]["added"], json!(["start", "pass"]));
    assert_eq!(c.draft(wf).await["name"], "From chat");
    let th = call(&c.state, "GET", &format!("{TH}?workflow_id={wf}"), Some(c.k()), None).await;
    assert_eq!(th.body["threads"][0]["id"], t.as_str());
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM llm_calls WHERE purpose = 'automation_copilot'").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1);
}

#[sqlx::test]
async fn replace_keeps_positions_and_pins_and_undo_restores_the_exact_draft(pool: PgPool) {
    let c = Ctx::new(pool, true).await;
    let mut def = flow("Mine", None);
    def["nodes"][0]["position"] = json!({ "x": 11, "y": 22 });
    def["pins"] = json!({ "pass": [{ "a": 1 }] });
    let made = call(&c.state, "POST", WF, Some(c.k()), Some(json!({ "name": "Mine", "definition": def }))).await;
    let wf = made.body["id"].as_str().unwrap().to_string();
    let before = c.draft(&wf).await;
    let t = c.thread(Some(&wf)).await;

    c.fake.push(Ok(response(answer("replace", Some(flow("Mine", Some("extra")))))));
    let r = c.say(&t, "Add a node").await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    let applied = &r.body["message"]["applied"];
    assert_eq!(applied["mode"], "replace");
    assert_eq!(applied["diff"]["added"], json!(["extra"]));
    let after = c.draft(&wf).await;
    assert_eq!(after["nodes"][0]["position"], json!({ "x": 11, "y": 22 }));
    assert!(after["nodes"][2]["position"].is_object(), "the new node got a position");
    assert_eq!(after["pins"], json!({ "pass": [{ "a": 1 }] }));
    // the state block the model saw held the real draft
    assert!(c.fake.calls()[0].user.contains("\"id\":\"pass\""));

    let mid = r.body["message"]["id"].as_i64().unwrap();
    let u = call(&c.state, "POST", &format!("/v1/automations/copilot/messages/{mid}/undo"), Some(c.k()), None).await;
    assert_eq!(u.status, StatusCode::OK, "{:?}", u.body);
    assert_eq!(c.draft(&wf).await, before, "undo gives back the exact draft");
    assert_eq!(u.body["message"]["applied"]["undone"], true);
    let again = call(&c.state, "POST", &format!("/v1/automations/copilot/messages/{mid}/undo"), Some(c.k()), None).await;
    assert_eq!(again.status, StatusCode::CONFLICT);
}

#[sqlx::test]
async fn a_bad_answer_is_repaired_once_and_then_saved_with_an_error(pool: PgPool) {
    let c = Ctx::new(pool, true).await;
    let wf = call(&c.state, "POST", WF, Some(c.k()), Some(json!({ "name": "W", "definition": flow("W", None) }))).await.body["id"].as_str().unwrap().to_string();
    let before = c.draft(&wf).await;
    let t = c.thread(Some(&wf)).await;
    let mut bad = flow("W", None);
    bad["nodes"][1]["type"] = json!("no.such_node");
    c.fake.push(Ok(response(answer("replace", Some(bad.clone())))));
    c.fake.push(Ok(response(answer("replace", Some(bad)))));
    let r = c.say(&t, "Break it").await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(c.fake.call_count(), 2);
    assert_eq!(r.body["message"]["applied"]["error_code"], "invalid_workflow");
    assert!(r.body["message"]["applied"]["errors"].as_array().is_some_and(|e| !e.is_empty()));
    assert_eq!(c.draft(&wf).await, before, "a bad answer changes nothing");
}

#[sqlx::test]
async fn no_provider_gives_a_clear_error(pool: PgPool) {
    let none = Ctx::new(pool.clone(), false).await;
    let t = none.thread(None).await;
    let r = none.say(&t, "hi").await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::CONFLICT, Some("ai_not_configured")));
    assert!(r.body["error"]["message"].as_str().unwrap().contains("Settings > AI"));
}

#[sqlx::test]
async fn ten_messages_a_minute_and_the_message_size(pool: PgPool) {
    let c = Ctx::new(pool, true).await;
    let t = c.thread(None).await;
    let long = c.say(&t, &"x".repeat(8001)).await;
    assert_eq!((long.status, long.body["error"]["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("message_too_long")));
    for _ in 0..10 {
        c.fake.push(Ok(response(answer("none", None))));
        assert_eq!(c.say(&t, "question").await.status, StatusCode::OK);
    }
    let r = c.say(&t, "one more").await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited")));
}
