//! Automations, phase 7: export, import, templates, workprocess.md.
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
        "format": "aikido-workflow", "version": 1, "name": "Exported",
        "variables": [{ "key": "list_id", "kind": "list", "value": null, "required": true }],
        "nodes": [
            { "id": "start", "type": "trigger.manual", "name": "Start" },
            { "id": "read", "type": "web.fetch_page", "name": "Read", "params": { "url": "https://acme.com" } }
        ],
        "edges": [{ "from": "start", "to": "read" }],
        "pins": { "read": [{ "title": "Acme", "email": "ana@acme.com" }] }
    })
}

async fn make(state: &aikido_email::state::AppState, key: &str, def: Value) -> Value {
    let r = call(state, "POST", WF, Some(key), Some(json!({ "name": "Exported", "definition": def }))).await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    r.body
}

#[sqlx::test]
async fn export_has_no_pins_and_a_file_name(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Automations];
    let wf = make(&state, k, definition()).await;
    let id = wf["id"].as_str().unwrap();
    let (status, headers, bytes) = call_raw(&state, &format!("{WF}/{id}/export"), Some(k)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers["content-disposition"].to_str().unwrap().contains("exported.aikido-workflow.json"));
    let text = String::from_utf8(bytes).unwrap();
    assert!(!text.contains("ana@acme.com"));
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["format"], "aikido-workflow");
    assert_eq!(v["meta"]["exported_from_version"], Value::Null);
    assert_eq!(v["meta"]["requires"]["node_types"], json!(["trigger.manual", "web.fetch_page"]));
    let (_, _, with_pins) = call_raw(&state, &format!("{WF}/{id}/export?include_pins=true"), Some(k)).await;
    assert!(String::from_utf8(with_pins).unwrap().contains("ana@acme.com"));
    let (status, _, _) = call_raw(&state, &format!("{WF}/{id}/export?version=9"), Some(k)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = call_raw(&state, &format!("{WF}/{}/export", uuid::Uuid::now_v7()), Some(k)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn import_saves_a_new_inactive_draft_with_a_report(pool: PgPool) {
    let state = app_state(pool.clone());
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Automations];
    let text = "Here you go:\n```json\n{ \"nodes\": [ { \"id\": \"On Start\", \"type\": \"trigger.manual\" }, { \"id\": \"Read Site\", \"type\": \"web.fetch_page\", \"params\": { \"url\": \"${{ secret('SITE_URL') }}\" } } ], \"edges\": [ { \"source\": \"On Start\", \"target\": \"Read Site\" } ] }\n```";
    let r = call(&state, "POST", "/v1/automations/import", Some(k), Some(json!({ "text": text }))).await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    let wf = &r.body["workflow"];
    assert_eq!(wf["active"], false);
    assert_eq!(wf["published"], Value::Null);
    assert_eq!(wf["draft"]["nodes"][1]["id"], "read_site");
    assert_eq!(wf["draft"]["edges"][0], json!({ "from": "on_start", "to": "read_site" }));
    let fixed = r.body["report"]["fixed"].to_string();
    assert!(fixed.contains("code fence") && fixed.contains("added format") && fixed.contains("renamed"), "{fixed}");
    assert_eq!(r.body["report"]["needs_you"][0], json!({ "kind": "secret", "name": "SITE_URL", "message": "Add the secret SITE_URL in Automations > Secrets." }));
    let id = wf["id"].as_str().unwrap();
    let source: String = sqlx::query_scalar("SELECT source FROM automation_draft_history WHERE workflow_id = $1::uuid ORDER BY id DESC LIMIT 1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(source, "import");
}

#[sqlx::test]
async fn an_import_with_errors_is_still_saved(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Automations];
    let def = json!({ "nodes": [{ "id": "a", "type": "web.fetch_pag" }], "edges": [] });
    let r = call(&state, "POST", "/v1/automations/import", Some(k), Some(json!({ "definition": def, "name": "Mine" }))).await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    assert_eq!(r.body["workflow"]["name"], "Mine");
    assert_eq!(r.body["report"]["validation"]["ok"], false);
    assert_eq!(r.body["workflow"]["validation"]["ok"], false);
}

#[sqlx::test]
async fn import_errors(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Automations];
    let bad = call(&state, "POST", "/v1/automations/import", Some(k), Some(json!({ "text": "{\n \"a\": ,\n}" }))).await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(bad.body["error"]["code"], "invalid_json");
    assert_eq!(bad.body["error"]["line"], 2);
    assert!(bad.body["error"]["column"].as_u64().unwrap() > 0);
    let none = call(&state, "POST", "/v1/automations/import", Some(k), Some(json!({}))).await;
    assert_eq!(none.body["error"]["code"], "import_source");
    let two = call(&state, "POST", "/v1/automations/import", Some(k), Some(json!({ "text": "{}", "url": "https://x.com" }))).await;
    assert_eq!(two.body["error"]["code"], "import_source");
    let new = call(&state, "POST", "/v1/automations/import", Some(k), Some(json!({ "definition": { "version": 2, "nodes": [] } }))).await;
    assert_eq!(new.body["error"]["code"], "format_too_new");
    let local = call(&state, "POST", "/v1/automations/import", Some(k), Some(json!({ "url": "http://127.0.0.1:1/x.json" }))).await;
    assert_eq!(local.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(local.body["error"]["code"], "import_fetch_failed");
    let list = call(&state, "GET", WF, Some(k), None).await.body;
    assert_eq!(list["workflows"].as_array().unwrap().len(), 0, "a failed import saves nothing");
}

#[sqlx::test]
async fn check_saves_nothing_and_maps_variables(pool: PgPool) {
    let state = app_state(pool.clone());
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Automations];
    let list_id = uuid::Uuid::now_v7();
    sqlx::query("INSERT INTO lead_lists (id, name) VALUES ($1, 'Warm leads')").bind(list_id).execute(&pool).await.unwrap();
    let mut def = definition();
    def["meta"] = json!({ "requires": { "variables": [{ "key": "list_id", "kind": "list", "hint": "warm leads" }] } });
    let r = call(&state, "POST", "/v1/automations/import/check", Some(k), Some(json!({ "definition": def }))).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["definition"]["variables"][0]["value"], list_id.to_string());
    assert_eq!(r.body["report"]["needs_you"], json!([]));
    let list = call(&state, "GET", WF, Some(k), None).await.body;
    assert_eq!(list["workflows"].as_array().unwrap().len(), 0);
    // an export on this install writes the name as a hint and no id
    let mut def2 = definition();
    def2["variables"][0]["value"] = json!(list_id.to_string());
    let wf = make(&state, k, def2).await;
    let (_, _, bytes) = call_raw(&state, &format!("{WF}/{}/export", wf["id"].as_str().unwrap()), Some(k)).await;
    let text = String::from_utf8(bytes).unwrap();
    assert!(!text.contains(&list_id.to_string()));
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["variables"][0]["value"], Value::Null);
    assert_eq!(v["meta"]["requires"]["variables"][0]["hint"], "Warm leads");
}

#[sqlx::test]
async fn templates_list_get_and_create(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Automations];
    let list = call(&state, "GET", "/v1/automations/templates", Some(k), None).await;
    assert_eq!(list.status, StatusCode::OK);
    assert_eq!(list.body["templates"].as_array().unwrap().len(), 4);
    let one = call(&state, "GET", "/v1/automations/templates/tag-hot-replies", Some(k), None).await;
    assert_eq!(one.body["format"], "aikido-workflow");
    let miss = call(&state, "GET", "/v1/automations/templates/nope", Some(k), None).await;
    assert_eq!(miss.status, StatusCode::NOT_FOUND);
    let made = call(&state, "POST", "/v1/automations/templates/website-form-to-lead/create", Some(k), Some(json!({ "name": "My form" }))).await;
    assert_eq!(made.status, StatusCode::CREATED, "{:?}", made.body);
    assert_eq!(made.body["workflow"]["name"], "My form");
    assert_eq!(made.body["workflow"]["active"], false);
    assert_eq!(made.body["workflow"]["validation"]["ok"], true);
    let kinds: Vec<&str> = made.body["report"]["needs_you"].as_array().unwrap().iter().map(|n| n["kind"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"variable") && kinds.contains(&"secret"), "{kinds:?}");
    let blank = call(&state, "POST", "/v1/automations/templates/tag-hot-replies/create", Some(k), None).await;
    assert_eq!(blank.status, StatusCode::CREATED, "{:?}", blank.body);
}

#[sqlx::test]
async fn workprocess_is_merged_and_the_install_part_is_optional(pool: PgPool) {
    let state = app_state(pool.clone());
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Automations];
    sqlx::query("INSERT INTO lead_lists (id, name) VALUES ($1, 'Warm leads')").bind(uuid::Uuid::now_v7()).execute(&pool).await.unwrap();
    let (status, headers, bytes) = call_raw(&state, "/v1/automations/workprocess.md", Some(k)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers["content-type"].to_str().unwrap().starts_with("text/markdown"));
    let plain = String::from_utf8(bytes).unwrap();
    assert!(plain.contains("#### `http.request`") && !plain.contains("NODE_CATALOG") && !plain.contains("Warm leads"));
    let (_, _, bytes) = call_raw(&state, "/v1/automations/workprocess.md?install=true", Some(k)).await;
    assert!(String::from_utf8(bytes).unwrap().contains("Warm leads"));
    let (status, _, _) = call_raw(&state, "/v1/automations/workprocess.md", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
