//! Automations, phase 3: the AIKIDO nodes on the real routes, dry runs, the deny list, the internal
//! principal, secrets and settings. These tests need Postgres (not run in the session that wrote them).
mod common;
use aikido_email::auth::api_keys::Scope;
use aikido_email::automations::engine::worker::{self, WorkerOpts};
use aikido_email::state::AppState;
use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

const WF: &str = "/v1/automations/workflows";
const RUNS: &str = "/v1/automations/runs";

async fn make(state: &AppState, key: &str, def: Value) -> String {
    let r = call(state, "POST", WF, Some(key), Some(json!({ "name": "Nodes", "definition": def }))).await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    r.body["id"].as_str().unwrap().to_string()
}

/// Start a test run, let the worker do it, and give back the finished run.
async fn test_run(state: &AppState, key: &str, wf: &str, body: Value) -> Value {
    let r = call(state, "POST", &format!("{WF}/{wf}/test"), Some(key), Some(body)).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{:?}", r.body);
    let id = r.body["run_id"].as_str().unwrap().to_string();
    worker::tick(state, &WorkerOpts::default()).await.unwrap();
    let run = call(state, "GET", &format!("{RUNS}/{id}"), Some(key), None).await;
    assert_eq!(run.status, StatusCode::OK, "{:?}", run.body);
    run.body
}

fn node<'a>(run: &'a Value, id: &str) -> &'a Value {
    run["nodes"].as_array().unwrap().iter().find(|n| n["node_id"] == id).unwrap_or_else(|| panic!("no row for {id}: {run}"))
}

fn one(type_id: &str, params: Value) -> Value {
    json!({
        "format": "aikido-workflow", "version": 1, "name": "x",
        "nodes": [{ "id": "start", "type": "trigger.manual" }, { "id": "n", "type": type_id, "params": params }],
        "edges": [{ "from": "start", "to": "n" }]
    })
}

#[sqlx::test]
async fn the_nodes_write_leads_campaign_emails_and_jobs_through_the_real_routes(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let _owner = setup_owner(&state).await; // a key run acts as the oldest active owner
    let k = &keys[&Scope::Automations];
    let account = add_account(&state, "sender@example.com").await;
    let campaign = call(
        &state,
        "POST",
        "/v1/campaigns",
        Some(&keys[&Scope::Campaigns]),
        Some(json!({ "name": "Research", "sequence": { "name": "Seq", "steps": [{ "delay_amount": 0, "subject": "Hello", "body_text": "Hi" }] }, "account_ids": [account] })),
    )
    .await
    .body["id"]
        .as_str()
        .unwrap()
        .to_string();

    let def = json!({
        "format": "aikido-workflow", "version": 1, "name": "Research",
        "nodes": [
            { "id": "start", "type": "trigger.manual" },
            { "id": "make", "type": "aikido.lead.upsert", "params": { "lead": { "email": "ana@acme.com", "first_name": "Ana" }, "campaign_id": campaign } },
            { "id": "note", "type": "aikido.lead.add_note", "params": { "lead_id": "${{ nodes.make.json.lead_id }}", "text": "Researched by a workflow." } },
            { "id": "save", "type": "aikido.campaign.save_lead_email", "params": { "campaign_id": campaign, "lead_id": "${{ nodes.make.json.lead_id }}", "step": 1, "subject": "Hello Ana", "body_text": "Hi Ana, I read your site." } },
            { "id": "send", "type": "aikido.email.send", "params": { "account_id": account, "to_email": "ana@acme.com", "subject": "Direct", "body_text": "Hi", "lead_id": "${{ nodes.make.json.lead_id }}" } }
        ],
        "edges": [{ "from": "start", "to": "make" }, { "from": "make", "to": "note" }, { "from": "note", "to": "save" }, { "from": "save", "to": "send" }]
    });
    let wf = make(&state, k, def).await;
    let run = test_run(&state, k, &wf, json!({ "real_writes": true })).await;
    assert_eq!(run["status"], "success", "{run}");

    // the lead exists and is in the campaign
    let lead_id: Uuid = node(&run, "make")["output"]["main"][0]["lead_id"].as_str().unwrap().parse().unwrap();
    assert_eq!(count(&state, &format!("SELECT count(*) FROM campaign_leads WHERE lead_id = '{lead_id}' AND campaign_id = '{campaign}'")).await, 1);
    // the email is saved by hand and ready
    assert_eq!(count(&state, &format!("SELECT count(*) FROM lead_emails WHERE lead_id = '{lead_id}' AND status = 'ready' AND edited_by_hand")).await, 1);
    // the events say that a workflow did it
    let by_actor = count(&state, &format!("SELECT count(*) FROM lead_events WHERE lead_id = '{lead_id}' AND detail->>'actor' = 'automation:{wf}'")).await;
    assert!(by_actor >= 2, "the note and the campaign add carry the actor, got {by_actor}");
    // the send went through the queue with the key of the run
    let key: String = sqlx::query_scalar("SELECT idempotency_key FROM send_jobs WHERE to_email = 'ana@acme.com' AND subject = 'Direct'").fetch_one(&state.pool).await.unwrap();
    assert!(key.starts_with("wf:") && key.ends_with(":send:0"), "{key}");
    // the same key again makes no second job
    let again = call(
        &state,
        "POST",
        "/v1/sending/jobs",
        Some(&keys[&Scope::Sending]),
        Some(json!({ "jobs": [{ "account_id": account, "to_email": "ana@acme.com", "subject": "Direct", "body_text": "Hi", "idempotency_key": key }] })),
    )
    .await;
    assert_eq!(again.body["items"][0]["status"], "duplicate");
    assert_eq!(count(&state, "SELECT count(*) FROM send_jobs WHERE to_email = 'ana@acme.com'").await, 1);
}

#[sqlx::test]
async fn a_dry_test_run_changes_nothing(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Automations];
    let account = add_account(&state, "sender@example.com").await;
    let def = json!({
        "format": "aikido-workflow", "version": 1, "name": "Dry",
        "nodes": [
            { "id": "start", "type": "trigger.manual" },
            { "id": "lead", "type": "aikido.lead.create", "params": { "email": "dry@acme.com" } },
            { "id": "list", "type": "aikido.list.create", "params": { "name": "Dry list" } },
            { "id": "mail", "type": "aikido.email.send", "params": { "account_id": account, "to_email": "dry@acme.com", "subject": "S", "body_text": "B" } },
            { "id": "supp", "type": "aikido.suppression.add", "params": { "value": "dry@acme.com" } },
            { "id": "api", "type": "aikido.api", "params": { "operation": "POST /v1/leads/lists", "body": { "name": "Dry list 2" } } }
        ],
        "edges": [{ "from": "start", "to": "lead" }, { "from": "lead", "to": "list" }, { "from": "list", "to": "mail" }, { "from": "mail", "to": "supp" }, { "from": "supp", "to": "api" }]
    });
    let tables = ["leads", "lead_lists", "send_jobs", "suppressions", "lead_events"];
    let mut before = vec![];
    for t in tables {
        before.push(count(&state, &format!("SELECT count(*) FROM {t}")).await);
    }
    let wf = make(&state, k, def).await;
    let run = test_run(&state, k, &wf, json!({})).await;
    assert_eq!(run["status"], "success", "{run}");
    for id in ["lead", "list", "mail", "supp", "api"] {
        assert_eq!(node(&run, id)["status"], "dry", "{id}");
        assert_eq!(node(&run, id)["output"]["main"][0]["dry_run"], true);
    }
    assert_eq!(node(&run, "lead")["output"]["main"][0]["would_call"]["method"], "POST");
    assert_eq!(node(&run, "lead")["output"]["main"][0]["would_call"]["path"], "/v1/leads");
    for (i, t) in tables.iter().enumerate() {
        assert_eq!(count(&state, &format!("SELECT count(*) FROM {t}")).await, before[i], "the table {t} must not change");
    }
}

#[sqlx::test]
async fn the_generic_node_refuses_the_deny_list_and_a_member_can_not_reach_admin_routes(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Automations];
    // validate refuses the deny list
    let v = call(&state, "POST", "/v1/automations/validate", Some(k), Some(json!({ "definition": one("aikido.api", json!({ "operation": "POST /v1/keys" })) }))).await;
    assert_eq!(v.body["ok"], false);
    assert!(v.body["errors"].as_array().unwrap().iter().any(|e| e["code"] == "endpoint_not_allowed"), "{}", v.body);
    // and so does a run (an expression hides the text from the validator)
    let wf = make(&state, k, one("aikido.api", json!({ "operation": "${{ 'POST /v1/keys' }}", "body": { "scope": "admin", "name": "x" } }))).await;
    let run = test_run(&state, k, &wf, json!({ "real_writes": true })).await;
    assert_eq!(run["status"], "error");
    assert_eq!(run["error"]["code"], "endpoint_not_allowed");
    assert_eq!(count(&state, "SELECT count(*) FROM api_keys WHERE name = 'x'").await, 0);

    // a member's workflow acts as the member: an admin route answers 403
    let owner = setup_owner(&state).await;
    let member = add_user(&state, &owner, "member@example.com", "member").await;
    let def = one("aikido.api", json!({ "operation": "POST /v1/sending/engine/pause" }));
    let r = member.call(&state, "POST", WF, Some(json!({ "name": "Member flow", "definition": def }))).await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    let id = r.body["id"].as_str().unwrap().to_string();
    let started = member.call(&state, "POST", &format!("{WF}/{id}/test"), Some(json!({ "real_writes": true }))).await;
    assert_eq!(started.status, StatusCode::ACCEPTED, "{:?}", started.body);
    worker::tick(&state, &WorkerOpts::default()).await.unwrap();
    let run = member.call(&state, "GET", &format!("{RUNS}/{}", started.body["run_id"].as_str().unwrap()), None).await;
    assert_eq!(run.body["status"], "error", "{}", run.body);
    assert_eq!(run.body["error"]["code"], "aikido_admin_only");
    assert_eq!(run.body["error"]["status"], 403);
    // the owner's workflow with the same node may do it
    let r = owner.call(&state, "POST", WF, Some(json!({ "name": "Owner flow", "definition": one("aikido.api", json!({ "operation": "POST /v1/sending/engine/pause" })) }))).await;
    let id = r.body["id"].as_str().unwrap().to_string();
    let started = owner.call(&state, "POST", &format!("{WF}/{id}/test"), Some(json!({ "real_writes": true }))).await;
    worker::tick(&state, &WorkerOpts::default()).await.unwrap();
    let run = owner.call(&state, "GET", &format!("{RUNS}/{}", started.body["run_id"].as_str().unwrap()), None).await;
    assert_eq!(run.body["status"], "success", "{}", run.body);
}

#[sqlx::test]
async fn an_internal_principal_can_not_be_faked_from_outside(pool: PgPool) {
    let state = app_state(pool);
    let _ = bootstrap_keys(&state).await;
    for headers in [
        vec![("x-internal-user", "0192f3a0-0000-7000-8000-000000000001"), ("x-internal-role", "owner")],
        vec![("x-automation-principal", "owner")],
        vec![("x-user-id", "0192f3a0-0000-7000-8000-000000000001")],
    ] {
        let r = call_with(&state, "GET", "/v1/leads", &headers, None).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{:?}", r.body);
        let r = call_with(&state, "POST", "/v1/keys", &headers, Some(json!({ "scope": "admin", "name": "evil" }))).await;
        assert!(r.status == StatusCode::UNAUTHORIZED || r.status == StatusCode::FORBIDDEN, "{:?}", r.status);
    }
}

#[sqlx::test]
async fn secrets_are_sealed_listed_by_name_and_only_admins_change_them(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let owner = setup_owner(&state).await;
    let member = add_user(&state, &owner, "member@example.com", "member").await;

    // the owner adds one
    let r = owner.call(&state, "POST", "/v1/automations/secrets", Some(json!({ "name": "SEARCH_API_KEY", "value": "sk-live-1234567890" }))).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["last4"], "7890");
    assert!(!r.body.to_string().contains("sk-live"));
    // the database holds no plain text
    let raw: Vec<u8> = sqlx::query_scalar("SELECT value_enc FROM automation_secrets WHERE name = 'SEARCH_API_KEY'").fetch_one(&state.pool).await.unwrap();
    assert!(!String::from_utf8_lossy(&raw).contains("sk-live"));
    // a list shows the names
    let list = member.call(&state, "GET", "/v1/automations/secrets", None).await;
    assert_eq!(list.status, StatusCode::OK);
    assert_eq!(list.body["secrets"][0]["name"], "SEARCH_API_KEY");
    assert!(!list.body.to_string().contains("sk-live"));
    // a member and a non-admin key can not add or delete
    let denied = member.call(&state, "POST", "/v1/automations/secrets", Some(json!({ "name": "OTHER_KEY", "value": "abcdefgh" }))).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let denied = call(&state, "POST", "/v1/automations/secrets", Some(&keys[&Scope::Automations]), Some(json!({ "name": "OTHER_KEY", "value": "abcdefgh" }))).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    assert_eq!(member.call(&state, "DELETE", "/v1/automations/secrets/SEARCH_API_KEY", None).await.status, StatusCode::FORBIDDEN);
    // an admin key can
    let ok = call(&state, "POST", "/v1/automations/secrets", Some(&keys[&Scope::Admin]), Some(json!({ "name": "OTHER_KEY", "value": "abcdefgh" }))).await;
    assert_eq!(ok.status, StatusCode::OK, "{:?}", ok.body);
    // bad names and values
    for body in [json!({ "name": "lower", "value": "abcdefgh" }), json!({ "name": "OK_NAME", "value": "abc" }), json!({ "name": "9BAD", "value": "abcdefgh" })] {
        assert_eq!(owner.call(&state, "POST", "/v1/automations/secrets", Some(body)).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    }
    // replace, delete, delete again
    let r = owner.call(&state, "POST", "/v1/automations/secrets", Some(json!({ "name": "SEARCH_API_KEY", "value": "sk-live-new-value-0001" }))).await;
    assert_eq!(r.body["last4"], "0001");
    assert_eq!(owner.call(&state, "DELETE", "/v1/automations/secrets/SEARCH_API_KEY", None).await.status, StatusCode::NO_CONTENT);
    assert_eq!(owner.call(&state, "DELETE", "/v1/automations/secrets/SEARCH_API_KEY", None).await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn a_secret_never_reaches_the_stored_run(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let _owner = setup_owner(&state).await; // a key run acts as the oldest active owner
    let k = &keys[&Scope::Automations];
    call(&state, "POST", "/v1/automations/secrets", Some(&keys[&Scope::Admin]), Some(json!({ "name": "TOKEN", "value": "tok-very-secret-9876" }))).await;
    // the aikido node puts the secret into a note; the real route stores it, the run must not
    let lead = call(&state, "POST", "/v1/leads", Some(&keys[&Scope::Leads]), Some(json!({ "email": "ana@acme.com" }))).await.body["id"].as_str().unwrap().to_string();
    let wf = make(&state, k, one("aikido.lead.add_note", json!({ "lead_id": lead, "text": "key ${{ secret('TOKEN') }}" }))).await;
    let run = test_run(&state, k, &wf, json!({ "real_writes": true })).await;
    assert_eq!(run["status"], "success", "{run}");
    let stored = run.to_string();
    assert!(!stored.contains("tok-very-secret-9876"), "{stored}");
    assert!(stored.contains("[secret:TOKEN]"));
    let events: String = sqlx::query_scalar("SELECT coalesce(string_agg(detail::text, ' '), '') FROM automation_run_events").fetch_one(&state.pool).await.unwrap();
    assert!(!events.contains("tok-very-secret-9876"));
    let nodes: String = sqlx::query_scalar("SELECT coalesce(string_agg(coalesce(output::text, '') || coalesce(error::text, ''), ' '), '') FROM automation_run_nodes").fetch_one(&state.pool).await.unwrap();
    assert!(!nodes.contains("tok-very-secret-9876"));
}

#[sqlx::test]
async fn settings_have_defaults_and_only_admins_change_them(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let owner = setup_owner(&state).await;
    let member = add_user(&state, &owner, "member@example.com", "member").await;
    let k = &keys[&Scope::Automations];
    let s = call(&state, "GET", "/v1/automations/settings", Some(k), None).await;
    assert_eq!(s.status, StatusCode::OK);
    assert_eq!(s.body["allow_private_network"], false);
    assert_eq!(s.body["fetch_respect_robots"], true);
    assert_eq!(s.body["fetch_blocked_domains"], json!(["linkedin.com", "www.linkedin.com"]));
    assert_eq!(member.call(&state, "PATCH", "/v1/automations/settings", Some(json!({ "allow_private_network": true }))).await.status, StatusCode::FORBIDDEN);
    assert_eq!(call(&state, "PATCH", "/v1/automations/settings", Some(k), Some(json!({ "allow_private_network": true }))).await.status, StatusCode::FORBIDDEN);
    let r = owner.call(&state, "PATCH", "/v1/automations/settings", Some(json!({ "allow_private_network": true, "fetch_blocked_domains": ["Example.com"] }))).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["allow_private_network"], true);
    assert_eq!(r.body["fetch_blocked_domains"], json!(["example.com"]));
    // the change is kept, and the other keys are untouched
    let s = call(&state, "GET", "/v1/automations/settings", Some(k), None).await;
    assert_eq!(s.body["allow_private_network"], true);
    assert_eq!(s.body["retention_days"], 30);
    assert_eq!(owner.call(&state, "PATCH", "/v1/automations/settings", Some(json!({ "retention_days": 0 }))).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(owner.call(&state, "PATCH", "/v1/automations/settings", Some(json!({ "nope": 1 }))).await.status, StatusCode::UNPROCESSABLE_ENTITY);
}
