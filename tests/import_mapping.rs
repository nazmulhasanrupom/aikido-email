//! Auto Database part 1: AI column mapping for CSV import (proposal, confirm, templates).
mod common;
use aikido_email::auth::api_keys::Scope;
use aikido_email::generator::llm::fake::response;
use aikido_email::leads::import::{self, RunOpts};
use aikido_email::leads::verify::{MxAnswer, fake::FakeMx};
use axum::http::StatusCode;
use common::gharness::*;
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::Arc;

const CSV: &str = "E-mail Addr,Sector,Full Name,Contact No,Backup,Internal ID\n\
john@acme.com,SaaS,Mary Ann Smith,+1 415 555 0101,bob@x.io,77\n\
sam@globex.io,Fintech,Cher,+1 415 555 0102,amy@y.io,78\n";

const ANSWER: &str = r#"{"columns":[
 {"index":1,"target":"new_field","key":"industry","field_kind":"text","confidence":0.82,"reason":"business sector"},
 {"index":2,"target":"first_name","transform":"split_full_name","confidence":0.9,"reason":"full name"},
 {"index":3,"target":"phone","confidence":0.9,"reason":"phone"},
 {"index":4,"target":"skip","confidence":0.6,"reason":"second email"},
 {"index":5,"target":"skip","confidence":0.9,"reason":"internal id"}]}"#;

async fn setup(pool: PgPool) -> G {
    let mut g = G::new(pool).await;
    g.h.state.mx = Arc::new(FakeMx::new(MxAnswer::Has));
    g.fake.set_responder(|_| Ok(response(ANSWER)));
    g
}

async fn upload(g: &G, csv: &str, query: &str) -> Response {
    call_form(&g.h.state, &format!("/v1/leads/imports{query}"), Some(&g.keys[&Scope::Leads]), &[("file", Some("l.csv"), csv.as_bytes())]).await
}

async fn leads_count(g: &G) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM leads").fetch_one(&g.h.state.pool).await.unwrap()
}

fn err_code(r: &Response) -> &str {
    r.body["error"]["code"].as_str().unwrap_or_default()
}

#[sqlx::test]
async fn odd_headers_get_a_full_proposal_and_nothing_is_imported_before_confirm(pool: PgPool) {
    let g = setup(pool).await;
    let r = upload(&g, CSV, "?auto_map=true").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert_eq!(r.body["status"], "uploaded");
    let p = &r.body["proposal"];
    let col = |i: usize| p["columns"][i].clone();
    assert_eq!(col(0)["target"], json!({ "kind": "builtin", "key": "email" }));
    assert_eq!(col(0)["source"], "rule");
    assert_eq!(col(0)["samples"][0], "j***@acme.com");
    assert_eq!(col(1)["target"]["kind"], "new_field");
    assert_eq!(col(1)["target"]["key"], "industry");
    assert_eq!(col(2)["transform"], "split_full_name");
    assert_eq!(col(5)["target"], json!({ "kind": "skip" }));
    assert_eq!(p["ai"]["used"], true);
    assert_eq!(p["needs_check"], json!([4]));
    // the model saw no email and no phone
    assert_eq!(g.fake.call_count(), 1);
    for req in g.fake.calls() {
        for bad in ["john@acme.com", "bob@x.io", "415 555 0101"] {
            assert!(!req.user.contains(bad) && !req.system.contains(bad), "{bad} reached the model");
        }
    }
    assert_eq!(leads_count(&g).await, 0);
    let fields: i64 = sqlx::query_scalar("SELECT count(*) FROM lead_fields").fetch_one(&g.h.state.pool).await.unwrap();
    assert_eq!(fields, 0);
    let calls: i64 = sqlx::query_scalar("SELECT count(*) FROM llm_calls WHERE purpose = 'import_mapping'").fetch_one(&g.h.state.pool).await.unwrap();
    assert_eq!(calls, 1);
    // read it back, masked and raw
    let id = r.body["id"].as_str().unwrap();
    let got = g.ok(Scope::Leads, "GET", &format!("/v1/leads/imports/{id}/mapping"), None).await;
    assert_eq!(got["columns"][0]["samples"][0], "j***@acme.com");
    let raw = g.ok(Scope::Leads, "GET", &format!("/v1/leads/imports/{id}/mapping?raw_samples=true"), None).await;
    assert_eq!(raw["columns"][0]["samples"][0], "john@acme.com");
}

#[sqlx::test]
async fn confirm_uses_the_users_target_makes_the_field_and_imports(pool: PgPool) {
    let g = setup(pool).await;
    let r = upload(&g, CSV, "?auto_map=true").await;
    let id = r.body["id"].as_str().unwrap().to_string();
    // too early: the worker does not take an uploaded import
    assert_eq!(import::tick(&g.h.state, &RunOpts::default()).await.unwrap(), 0);
    let url = format!("/v1/leads/imports/{id}/mapping/confirm");
    // no email column: 422 with a list, nothing is made
    let bad = g.api(Scope::Leads, "POST", &url, Some(json!({ "columns": [{ "index": 0, "target": { "kind": "skip" } }] }))).await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(err_code(&bad), "mapping_invalid");
    assert!(bad.body["error"]["problems"][0].as_str().unwrap().contains("email"));
    let fields: i64 = sqlx::query_scalar("SELECT count(*) FROM lead_fields").fetch_one(&g.h.state.pool).await.unwrap();
    assert_eq!(fields, 0);
    // split on a wrong column, and two columns on one target
    let bad = g
        .api(Scope::Leads, "POST", &url, Some(json!({ "columns": [
            { "index": 5, "target": { "kind": "builtin", "key": "phone" } },
            { "index": 1, "target": { "kind": "builtin", "key": "company_name" }, "transform": "split_full_name" }] })))
        .await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(bad.body["error"]["problems"].as_array().unwrap().len() >= 2, "{}", bad.body);
    // the user changes Sector: company_name instead of the AI's new field
    let ok = g
        .ok(Scope::Leads, "POST", &url, Some(json!({ "columns": [{ "index": 1, "target": { "kind": "builtin", "key": "company_name" } }], "tags": ["csv"] })))
        .await;
    assert_eq!(ok["status"], "pending");
    // a second confirm does not start it again
    let again = g.api(Scope::Leads, "POST", &url, Some(json!({}))).await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(import::tick(&g.h.state, &RunOpts::default()).await.unwrap(), 1);
    let row: (Option<String>, Option<String>, Option<String>) =
        sqlx::query_as("SELECT first_name, last_name, company_name FROM leads WHERE email = 'john@acme.com'").fetch_one(&g.h.state.pool).await.unwrap();
    assert_eq!(row, (Some("Mary Ann".into()), Some("Smith".into()), Some("SaaS".into())));
    let one: (Option<String>, Option<String>) = sqlx::query_as("SELECT first_name, last_name FROM leads WHERE email = 'sam@globex.io'").fetch_one(&g.h.state.pool).await.unwrap();
    assert_eq!(one, (Some("Cher".into()), None));
}

#[sqlx::test]
async fn a_new_field_is_made_only_on_confirm_and_a_template_saves_the_next_call(pool: PgPool) {
    let g = setup(pool).await;
    let r = upload(&g, CSV, "?auto_map=true").await;
    let id = r.body["id"].as_str().unwrap();
    let done = g.ok(Scope::Leads, "POST", &format!("/v1/leads/imports/{id}/mapping/confirm"), Some(json!({}))).await;
    assert_eq!(done["warnings"], json!([]));
    let kind: String = sqlx::query_scalar("SELECT kind::text FROM lead_fields WHERE key = 'industry'").fetch_one(&g.h.state.pool).await.unwrap();
    assert_eq!(kind, "text");
    assert_eq!(import::tick(&g.h.state, &RunOpts::default()).await.unwrap(), 1);
    let v: Value = sqlx::query_scalar("SELECT custom_fields FROM leads WHERE email = 'john@acme.com'").fetch_one(&g.h.state.pool).await.unwrap();
    assert_eq!(v["industry"], "SaaS");
    // same headers again: no model call
    assert_eq!(g.fake.call_count(), 1);
    let r2 = upload(&g, CSV, "?auto_map=true").await;
    assert_eq!(g.fake.call_count(), 1);
    assert!(r2.body["proposal"]["template_used"].is_string());
    assert_eq!(r2.body["proposal"]["columns"][1]["target"]["kind"], "custom");
    assert_eq!(r2.body["proposal"]["columns"][1]["source"], "template");
    let list = g.ok(Scope::Leads, "GET", "/v1/leads/mapping-templates", None).await;
    assert_eq!(list["templates"].as_array().unwrap().len(), 1);
    assert_eq!(list["templates"][0]["used_count"], 1);
    // other order of headers: new hash, new proposal, one more call
    let swapped = "Sector,E-mail Addr\nSaaS,a@b.io\n";
    let r3 = upload(&g, swapped, "?auto_map=true").await;
    assert!(r3.body["proposal"]["template_used"].is_null());
    // "map again" asks the model again
    let before = g.fake.call_count();
    let id2 = r2.body["id"].as_str().unwrap();
    g.ok(Scope::Leads, "POST", &format!("/v1/leads/imports/{id2}/mapping/propose"), None).await;
    assert_eq!(g.fake.call_count(), before + 1);
    // delete the template
    let tid = list["templates"][0]["id"].as_str().unwrap();
    g.ok(Scope::Leads, "DELETE", &format!("/v1/leads/mapping-templates/{tid}"), None).await;
    assert_eq!(g.api(Scope::Leads, "DELETE", &format!("/v1/leads/mapping-templates/{tid}"), None).await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn no_provider_bad_json_and_budget_fall_back_to_the_rules(pool: PgPool) {
    let g = setup(pool).await;
    // bad JSON twice (answer and repair): rules only
    g.fake.set_responder(|_| Ok(response("sorry, no json")));
    let r = upload(&g, CSV, "?auto_map=true").await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert!(r.body["proposal"]["warnings"].as_array().unwrap().contains(&json!("ai_mapping_failed")));
    assert_eq!(r.body["proposal"]["columns"][0]["target"]["key"], "email");
    assert_eq!(r.body["proposal"]["ai"]["used"], false);
    assert_eq!(g.fake.call_count(), 2);
    // over the daily budget
    g.settings(json!({ "daily_token_budget": 1 })).await;
    let r = upload(&g, CSV, "?auto_map=true").await;
    assert!(r.body["proposal"]["warnings"].as_array().unwrap().contains(&json!("ai_budget_reached")));
    assert_eq!(g.fake.call_count(), 2);
    // no provider at all
    g.settings(json!({ "daily_token_budget": 0, "slot_provider_id": null, "draft_provider_id": null })).await;
    let r = upload(&g, CSV, "?auto_map=true").await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert!(r.body["proposal"]["warnings"].as_array().unwrap().contains(&json!("no_ai_provider")));
    assert_eq!(r.body["proposal"]["columns"].as_array().unwrap().len(), 6);
    assert_eq!(g.fake.call_count(), 2);
    // AI off in the settings: no warning, no call
    g.settings(json!({ "slot_provider_id": g.provider })).await;
    g.ok(Scope::Leads, "PATCH", "/v1/leads/settings", Some(json!({ "ai_mapping_enabled": false }))).await;
    let r = upload(&g, CSV, "?auto_map=true").await;
    assert_eq!(r.body["proposal"]["warnings"], json!([]));
    assert_eq!(g.fake.call_count(), 2);
}

#[sqlx::test]
async fn the_model_answer_is_guarded_and_150_columns_send_only_100(pool: PgPool) {
    let g = setup(pool).await;
    // two columns on email: the rule column wins; an unknown target is skipped
    g.fake.set_responder(|_| Ok(response(r#"{"columns":[{"index":1,"target":"email","confidence":0.99,"reason":"x"},{"index":2,"target":"nonsense","confidence":0.9,"reason":"y"}]}"#)));
    let r = upload(&g, "Email,B,C\na@b.io,x@y.io,2\n", "?auto_map=true").await;
    let p = &r.body["proposal"];
    assert_eq!(p["columns"][0]["target"]["key"], "email");
    assert_eq!(p["columns"][1]["target"]["kind"], "skip");
    assert_eq!(p["columns"][2]["target"]["kind"], "skip");
    let w = p["warnings"].as_array().unwrap();
    assert!(w.contains(&json!("duplicate_target:1")) && w.contains(&json!("ai_unknown_target:2")), "{w:?}");
    // 150 columns
    g.fake.set_responder(|_| Ok(response(r#"{"columns":[]}"#)));
    let head: Vec<String> = (0..150).map(|i| format!("col{i}")).collect();
    let row: Vec<String> = (0..150).map(|i| format!("v{i}")).collect();
    let csv = format!("{}\n{}\n", head.join(","), row.join(","));
    let before = g.fake.call_count();
    let r = upload(&g, &csv, "?auto_map=true").await;
    assert!(r.body["proposal"]["warnings"].as_array().unwrap().contains(&json!("columns_over_100")));
    let req = g.fake.calls().pop().unwrap();
    assert!(g.fake.call_count() > before);
    assert!(req.user.contains("Column 99 ") && !req.user.contains("Column 100 "));
}

#[sqlx::test]
async fn old_upload_is_unchanged_and_bad_settings_get_422(pool: PgPool) {
    let g = setup(pool).await;
    let r = upload(&g, "Email,First\na@b.io,Ann\n", "").await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_eq!(r.body["status"], "pending");
    assert_eq!(g.fake.call_count(), 0);
    let both = call_form(&g.h.state, "/v1/leads/imports?auto_map=true", Some(&g.keys[&Scope::Leads]), &[("file", Some("l.csv"), b"Email\na@b.io\n"), ("mapping", None, br#"{"Email":"email"}"#)]).await;
    assert_eq!(err_code(&both), "auto_map_with_mapping");
    for bad in [json!({ "mapping_sample_rows": 11 }), json!({ "mapping_min_confidence": 2 })] {
        assert_eq!(g.api(Scope::Leads, "PATCH", "/v1/leads/settings", Some(bad)).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    }
}
