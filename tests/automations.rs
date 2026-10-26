//! Automations, phase 1: workflows (draft), validate, the node catalog and the schema.
//! These tests need Postgres (not run in the session that wrote them).
mod common;
use aikido_email::auth::api_keys::Scope;
use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;

const WF: &str = "/v1/automations/workflows";

fn definition() -> Value {
    json!({
        "format": "aikido-workflow", "version": 1, "name": "ignored, the body name wins",
        "nodes": [
            { "id": "start", "type": "trigger.manual" },
            { "id": "read", "type": "web.fetch_page", "params": { "url": "https://acme.com" } }
        ],
        "edges": [{ "from": "start", "to": "read" }]
    })
}

async fn make(state: &aikido_email::state::AppState, key: &str, name: &str, def: Option<Value>) -> Value {
    let mut body = json!({ "name": name });
    if let Some(d) = def {
        body["definition"] = d;
    }
    let r = call(state, "POST", WF, Some(key), Some(body)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    r.body
}

#[sqlx::test]
async fn create_blank_then_read_and_list(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Automations];
    let wf = make(&state, k, "My first", None).await;
    assert_eq!(wf["name"], "My first");
    assert_eq!(wf["active"], false);
    assert_eq!(wf["published"], Value::Null);
    assert_eq!(wf["draft"]["format"], "aikido-workflow");
    assert_eq!(wf["draft"]["nodes"][0]["type"], "trigger.manual");
    assert_eq!(wf["validation"]["ok"], true, "{:?}", wf["validation"]);
    assert_eq!(wf["draft_hash"].as_str().unwrap().len(), 64);

    let id = wf["id"].as_str().unwrap();
    let got = call(&state, "GET", &format!("{WF}/{id}"), Some(k), None).await;
    assert_eq!(got.status, StatusCode::OK);
    assert_eq!(got.body["draft_hash"], wf["draft_hash"]);

    let list = call(&state, "GET", WF, Some(k), None).await.body;
    assert_eq!(list["workflows"].as_array().unwrap().len(), 1);
    assert_eq!(list["workflows"][0]["runs_24h"], json!({ "success": 0, "error": 0 }));
    assert_eq!(list["workflows"][0]["last_run"], Value::Null);
    assert_eq!(list["workflows"][0]["published_version"], Value::Null);
    let miss = call(&state, "GET", &format!("{WF}/{}", uuid::Uuid::now_v7()), Some(k), None).await;
    assert_eq!(miss.status, StatusCode::NOT_FOUND);
    assert_eq!(miss.body["error"]["code"], "workflow_not_found");
}

#[sqlx::test]
async fn a_broken_draft_is_saved_with_its_errors(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Automations];
    let mut def = definition();
    def["nodes"][1]["type"] = json!("web.fetch_pag");
    let wf = make(&state, k, "Broken", Some(def)).await;
    assert_eq!(wf["validation"]["ok"], false);
    assert_eq!(wf["validation"]["errors"][0]["code"], "unknown_node_type");
    assert_eq!(wf["name"], "Broken", "the body name wins");
    assert_eq!(wf["draft"]["name"], "Broken");
    let list = call(&state, "GET", WF, Some(k), None).await.body;
    assert_eq!(list["workflows"][0]["draft_valid"], false);
}

#[sqlx::test]
async fn create_checks_its_input(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Automations];
    let r = call(&state, "POST", WF, Some(k), Some(json!({ "name": "  " }))).await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_name")));
    let r = call(&state, "POST", WF, Some(k), Some(json!({ "name": "x", "template": "lead_research" }))).await;
    assert_eq!(r.body["error"]["code"], "unknown_template");
    let r = call(&state, "POST", WF, Some(k), Some(json!({ "name": "x", "definition": [1, 2] }))).await;
    assert_eq!(r.body["error"]["code"], "invalid_format");
}

#[sqlx::test]
async fn patch_saves_the_draft_keeps_history_and_detects_a_conflict(pool: PgPool) {
    let state = app_state(pool.clone());
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Automations];
    let wf = make(&state, k, "Flow", None).await;
    let id = wf["id"].as_str().unwrap();
    let uri = format!("{WF}/{id}");
    let hash0 = wf["draft_hash"].as_str().unwrap().to_string();

    let mut draft = wf["draft"].clone();
    draft["nodes"].as_array_mut().unwrap().push(json!({ "id": "read", "type": "web.fetch_page", "params": { "url": "acme.com" } }));
    draft["edges"] = json!([{ "from": "start", "to": "read" }]);
    let saved = call(&state, "PATCH", &uri, Some(k), Some(json!({ "draft": draft, "if_hash": hash0 }))).await;
    assert_eq!(saved.status, StatusCode::OK, "{:?}", saved.body);
    let hash1 = saved.body["draft_hash"].as_str().unwrap().to_string();
    assert_ne!(hash0, hash1);
    assert_eq!(saved.body["draft"]["nodes"].as_array().unwrap().len(), 2);

    // a save with the old hash is a conflict, and the answer holds the newest draft
    let stale = call(&state, "PATCH", &uri, Some(k), Some(json!({ "draft": wf["draft"], "if_hash": hash0 }))).await;
    assert_eq!(stale.status, StatusCode::CONFLICT);
    assert_eq!(stale.body["error"]["code"], "draft_conflict");
    assert_eq!(stale.body["error"]["draft_hash"], hash1);
    assert_eq!(stale.body["error"]["draft"]["nodes"].as_array().unwrap().len(), 2);

    // the same draft again changes nothing and adds no history
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM automation_draft_history WHERE workflow_id = $1").bind(uuid::Uuid::parse_str(id).unwrap()).fetch_one(&pool).await.unwrap();
    let again = call(&state, "PATCH", &uri, Some(k), Some(json!({ "draft": saved.body["draft"], "if_hash": hash1 }))).await;
    assert_eq!(again.status, StatusCode::OK);
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM automation_draft_history WHERE workflow_id = $1").bind(uuid::Uuid::parse_str(id).unwrap()).fetch_one(&pool).await.unwrap();
    assert_eq!(before, after);

    // name and tags
    let renamed = call(&state, "PATCH", &uri, Some(k), Some(json!({ "name": "Renamed", "tags": ["a", "b", "a"], "description": "Does things" }))).await;
    assert_eq!(renamed.status, StatusCode::OK, "{:?}", renamed.body);
    assert_eq!(renamed.body["name"], "Renamed");
    assert_eq!(renamed.body["draft"]["name"], "Renamed");
    assert_eq!(renamed.body["description"], "Does things");
    assert_eq!(renamed.body["tags"], json!(["a", "b"]));
    let by_tag = call(&state, "GET", &format!("{WF}?tag=b"), Some(k), None).await.body;
    assert_eq!(by_tag["workflows"].as_array().unwrap().len(), 1);
    let by_q = call(&state, "GET", &format!("{WF}?q=renam"), Some(k), None).await.body;
    assert_eq!(by_q["workflows"].as_array().unwrap().len(), 1);
    let none = call(&state, "GET", &format!("{WF}?q=zzz"), Some(k), None).await.body;
    assert!(none["workflows"].as_array().unwrap().is_empty());
}

#[sqlx::test]
async fn draft_history_keeps_the_last_50(pool: PgPool) {
    let state = app_state(pool.clone());
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Automations];
    let wf = make(&state, k, "Many saves", None).await;
    let id = wf["id"].as_str().unwrap();
    for i in 0..55 {
        let mut d = wf["draft"].clone();
        d["description"] = json!(format!("save {i}"));
        let r = call(&state, "PATCH", &format!("{WF}/{id}"), Some(k), Some(json!({ "draft": d }))).await;
        assert_eq!(r.status, StatusCode::OK);
    }
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM automation_draft_history WHERE workflow_id = $1").bind(uuid::Uuid::parse_str(id).unwrap()).fetch_one(&pool).await.unwrap();
    assert_eq!(n, 50);
}

#[sqlx::test]
async fn archive_restore_and_delete(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let (k, admin) = (&keys[&Scope::Automations], &keys[&Scope::Admin]);
    let wf = make(&state, k, "To archive", None).await;
    let uri = format!("{WF}/{}", wf["id"].as_str().unwrap());

    assert_eq!(call(&state, "DELETE", &uri, Some(k), None).await.status, StatusCode::NO_CONTENT);
    assert!(call(&state, "GET", WF, Some(k), None).await.body["workflows"].as_array().unwrap().is_empty(), "archived ones are hidden");
    let archived = call(&state, "GET", &format!("{WF}?archived=true"), Some(k), None).await.body;
    assert_eq!(archived["workflows"].as_array().unwrap().len(), 1);
    let back = call(&state, "PATCH", &uri, Some(k), Some(json!({ "archived": false }))).await;
    assert_eq!(back.body["archived_at"], Value::Null);
    assert_eq!(call(&state, "GET", WF, Some(k), None).await.body["workflows"].as_array().unwrap().len(), 1);

    // hard delete is for admins
    let no = call(&state, "DELETE", &format!("{uri}?hard=true"), Some(k), None).await;
    assert_eq!((no.status, no.body["error"]["code"].as_str()), (StatusCode::FORBIDDEN, Some("admin_only")));
    assert_eq!(call(&state, "DELETE", &format!("{uri}?hard=true"), Some(admin), None).await.status, StatusCode::NO_CONTENT);
    assert_eq!(call(&state, "GET", &uri, Some(k), None).await.status, StatusCode::NOT_FOUND);
    assert_eq!(call(&state, "DELETE", &uri, Some(k), None).await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn duplicate_makes_an_inactive_draft_copy(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Automations];
    let wf = make(&state, k, "Original", Some(definition())).await;
    let copy = call(&state, "POST", &format!("{WF}/{}/duplicate", wf["id"].as_str().unwrap()), Some(k), None).await;
    assert_eq!(copy.status, StatusCode::CREATED, "{:?}", copy.body);
    assert_eq!(copy.body["name"], "Original (copy)");
    assert_eq!(copy.body["draft"]["name"], "Original (copy)");
    assert_ne!(copy.body["id"], wf["id"]);
    assert_eq!(copy.body["active"], false);
    assert_eq!(copy.body["published"], Value::Null);
    assert_eq!(copy.body["draft"]["nodes"], wf["draft"]["nodes"]);
}

#[sqlx::test]
async fn validate_writes_nothing_and_reports_every_problem(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Automations];
    let mut def = definition();
    def["nodes"][1]["params"] = json!({ "url": "${{ nodes.nope.json.x }}" });
    def["edges"].as_array_mut().unwrap().push(json!({ "from": "read", "to": "ghost" }));
    let r = call(&state, "POST", "/v1/automations/validate", Some(k), Some(json!({ "definition": def }))).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["ok"], false);
    let codes: Vec<&str> = r.body["errors"].as_array().unwrap().iter().map(|e| e["code"].as_str().unwrap()).collect();
    assert!(codes.contains(&"unknown_reference") && codes.contains(&"unknown_edge_node"), "{codes:?}");
    assert!(call(&state, "GET", WF, Some(k), None).await.body["workflows"].as_array().unwrap().is_empty());
    // a good definition
    let ok = call(&state, "POST", "/v1/automations/validate", Some(k), Some(json!({ "definition": definition() }))).await;
    assert_eq!(ok.body["ok"], true, "{:?}", ok.body);
    // not an object at all
    let bad = call(&state, "POST", "/v1/automations/validate", Some(k), Some(json!({ "definition": 5 }))).await;
    assert_eq!(bad.body["errors"][0]["code"], "invalid_format");
}

#[sqlx::test]
async fn the_node_catalog_and_the_schema(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let nodes = call(&state, "GET", "/v1/automations/nodes", Some(&keys[&Scope::Automations]), None).await;
    assert_eq!(nodes.status, StatusCode::OK);
    let list = nodes.body["nodes"].as_array().unwrap();
    assert_eq!(list.len(), 61);
    let http = list.iter().find(|n| n["type"] == "http.request").unwrap();
    assert_eq!(http["category"], "web");
    assert_eq!(http["params_schema"]["required"], json!(["url"]));
    // the schema is open to any valid key
    for scope in [Scope::Leads, Scope::Automations] {
        let s = call(&state, "GET", "/v1/automations/schema.json", Some(&keys[&scope]), None).await;
        assert_eq!(s.status, StatusCode::OK);
        assert!(s.body["$schema"].as_str().unwrap().contains("2020-12"));
    }
    assert_eq!(call(&state, "GET", "/v1/automations/schema.json", None, None).await.status, StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn the_scope_is_checked(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let wrong = call(&state, "GET", WF, Some(&keys[&Scope::Leads]), None).await;
    assert_eq!((wrong.status, wrong.body["error"]["code"].as_str()), (StatusCode::FORBIDDEN, Some("wrong_scope")));
    assert_eq!(call(&state, "GET", WF, None, None).await.status, StatusCode::UNAUTHORIZED);
    // an admin key passes every scope
    assert_eq!(call(&state, "GET", WF, Some(&keys[&Scope::Admin]), None).await.status, StatusCode::OK);
}

#[sqlx::test]
async fn a_member_session_can_use_workflows(pool: PgPool) {
    let state = app_state(pool);
    let owner = setup_owner(&state).await;
    let member = add_user(&state, &owner, "member@example.com", "member").await;
    let made = member.call(&state, "POST", WF, Some(json!({ "name": "By a member" }))).await;
    assert_eq!(made.status, StatusCode::CREATED, "{:?}", made.body);
    assert!(made.body["created_by"].is_string());
    let hard = member.call(&state, "DELETE", &format!("{WF}/{}?hard=true", made.body["id"].as_str().unwrap()), None).await;
    assert_eq!(hard.status, StatusCode::FORBIDDEN);
    let seen = owner.call(&state, "GET", WF, None).await;
    assert_eq!(seen.body["workflows"].as_array().unwrap().len(), 1);
}
