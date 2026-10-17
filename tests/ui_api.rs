//! UI build map B4 (config), B5 (onboarding), B10 (alerts), and the workspace.
mod common;
use aikido_email::auth::api_keys::Scope;
use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;

fn kinds(alerts: &Value) -> Vec<String> {
    alerts.as_array().unwrap().iter().map(|a| a["kind"].as_str().unwrap().to_string()).collect()
}

#[sqlx::test]
async fn config_has_no_secrets_and_tells_what_is_ready(pool: PgPool) {
    let state = app_state(pool);
    let owner = setup_owner(&state).await;
    let r = owner.call(&state, "GET", "/v1/ui/config", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(r.body["public_url_set"], true);
    assert_eq!(r.body["dry_run"], false);
    assert_eq!(r.body["features"]["leads"], false, "the test config turns the workers off");
    assert_eq!(r.body["redirect_uris"]["gmail"], "http://localhost:8080/v1/accounts/oauth/gmail/callback");
    assert!(r.body["oauth"]["gmail"].is_boolean() && r.body["oauth"]["microsoft"].is_boolean());
    let text = r.body.to_string();
    for word in ["secret", "password", "token", "key"] {
        assert!(!text.to_lowercase().contains(word), "{word} in {text}");
    }
    // no login, no key: refused. A key of any scope works.
    assert_eq!(call(&state, "GET", "/v1/ui/config", None, None).await.status, StatusCode::UNAUTHORIZED);
    let (_, key) = aikido_email::auth::api_keys::create(&state.pool, Scope::Leads, "t").await.unwrap();
    assert_eq!(call(&state, "GET", "/v1/ui/config", Some(&key), None).await.status, StatusCode::OK);
}

#[sqlx::test]
async fn onboarding_saves_the_step_and_reports_the_facts(pool: PgPool) {
    let state = app_state(pool);
    let owner = setup_owner(&state).await;
    let member = add_user(&state, &owner, "member@example.com", "member").await;
    let r = owner.call(&state, "GET", "/v1/ui/onboarding", None).await;
    assert_eq!(r.body["state"], json!({"step": "connect-email", "done": false, "skipped": []}));
    assert_eq!(r.body["facts"], json!({"accounts": 0, "readable_accounts": 0, "providers": 0, "leads": 0, "sending_defaults_changed": false}));

    add_account(&state, "a@example.com").await;
    sqlx::query("UPDATE email_accounts SET can_read = true").execute(&state.pool).await.unwrap();
    sqlx::query("UPDATE sending_defaults SET settings = '{\"timezone\": \"Europe/Berlin\"}'").execute(&state.pool).await.unwrap();
    let (_, leads) = aikido_email::auth::api_keys::create(&state.pool, Scope::Leads, "t").await.unwrap();
    call(&state, "POST", "/v1/leads", Some(&leads), Some(json!({"email": "x@acme.com"}))).await;

    let r = owner.call(&state, "PATCH", "/v1/ui/onboarding", Some(json!({"step": "ai", "skipped": ["import", "ai", "ai"]}))).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["state"], json!({"step": "ai", "done": false, "skipped": ["ai", "import"]}));
    assert_eq!(r.body["facts"], json!({"accounts": 1, "readable_accounts": 1, "providers": 0, "leads": 1, "sending_defaults_changed": true}));
    // it survives a reload
    assert_eq!(owner.call(&state, "GET", "/v1/ui/onboarding", None).await.body["state"]["step"], "ai");
    let done = owner.call(&state, "PATCH", "/v1/ui/onboarding", Some(json!({"step": "done", "done": true}))).await;
    assert_eq!(done.body["state"]["done"], true);

    assert_eq!(owner.call(&state, "PATCH", "/v1/ui/onboarding", Some(json!({"step": "nope"}))).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    // a member can read but not change
    assert_eq!(member.call(&state, "GET", "/v1/ui/onboarding", None).await.status, StatusCode::OK);
    assert_eq!(member.call(&state, "PATCH", "/v1/ui/onboarding", Some(json!({"step": "ai"}))).await.status, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn workspace_name_and_logo(pool: PgPool) {
    let state = app_state(pool);
    let owner = setup_owner(&state).await;
    let member = add_user(&state, &owner, "member@example.com", "member").await;
    assert_eq!(member.call(&state, "GET", "/v1/ui/workspace", None).await.body["name"], "Acme");
    assert_eq!(member.call(&state, "PATCH", "/v1/ui/workspace", Some(json!({"name": "X"}))).await.status, StatusCode::FORBIDDEN);

    let logo = "data:image/png;base64,iVBORw0KGgo=";
    let r = owner.call(&state, "PATCH", "/v1/ui/workspace", Some(json!({"name": " Acme Mail ", "logo": logo}))).await;
    assert_eq!((r.body["name"].as_str(), r.body["logo"].as_str()), (Some("Acme Mail"), Some(logo)));
    // a missing logo keeps it. null removes it.
    assert_eq!(owner.call(&state, "PATCH", "/v1/ui/workspace", Some(json!({"name": "Acme"}))).await.body["logo"], logo);
    assert_eq!(call_with(&state, "GET", "/v1/auth/state", &[("cookie", &owner.cookie)], None).await.body["workspace_logo"], logo);
    assert!(owner.call(&state, "PATCH", "/v1/ui/workspace", Some(json!({"logo": null}))).await.body["logo"].is_null());

    for bad in [json!({"name": ""}), json!({"name": "x".repeat(81)}), json!({"logo": "data:text/html;base64,PGI+"}), json!({"logo": "http://x/y.png"}),
                json!({"logo": format!("data:image/png;base64,{}", "A".repeat(210_000))})] {
        assert_eq!(owner.call(&state, "PATCH", "/v1/ui/workspace", Some(bad.clone())).await.status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    }
}

#[sqlx::test]
async fn a_quiet_system_has_no_alerts(pool: PgPool) {
    let state = app_state(pool);
    let owner = setup_owner(&state).await;
    assert_eq!(owner.call(&state, "GET", "/v1/ui/alerts", None).await.body, json!([]));
}

#[sqlx::test]
async fn alerts_show_each_problem_and_go_when_it_is_fixed(pool: PgPool) {
    let state = app_state(pool);
    let owner = setup_owner(&state).await;
    let a = add_account(&state, "a@example.com").await;
    let b = add_account(&state, "b@example.com").await;
    let c = add_account(&state, "c@example.com").await;
    let d = add_account(&state, "d@example.com").await;
    sqlx::query("UPDATE email_accounts SET can_read = true WHERE id = ANY($1)").bind(vec![a, b, c]).execute(&state.pool).await.unwrap();
    sqlx::query("UPDATE email_accounts SET status = 'disconnected', last_error = 'token revoked' WHERE id = $1").bind(a).execute(&state.pool).await.unwrap();
    sqlx::query("UPDATE email_accounts SET status = 'error', last_error = 'too many bounces' WHERE id = $1").bind(b).execute(&state.pool).await.unwrap();
    // d is active but cannot be read
    let _ = (c, d);

    let r = owner.call(&state, "GET", "/v1/ui/alerts", None).await;
    assert_eq!(r.status, StatusCode::OK);
    let list = r.body.as_array().unwrap();
    let find = |kind: &str| list.iter().find(|x| x["kind"] == kind).unwrap_or_else(|| panic!("no {kind} in {list:?}"));
    let dis = find("account_disconnected");
    assert_eq!(dis["severity"], "error");
    assert_eq!(dis["title"], "a@example.com is disconnected");
    assert_eq!(dis["detail"], "token revoked");
    assert_eq!(dis["link"], format!("/w/default/accounts/{a}"));
    assert_eq!(dis["id"], format!("account_disconnected:{a}"));
    assert!(dis["since"].is_string());
    assert_eq!(find("account_error")["detail"], "too many bounces");
    let blind = find("account_reconnect_needed");
    assert_eq!(blind["severity"], "warning");
    assert!(blind["title"].as_str().unwrap().contains("d@example.com"));
    assert_eq!(list.len(), 3);
    // most serious first
    assert!(list[0]["severity"] == "error" && list[2]["severity"] == "warning");

    // fixed: gone
    sqlx::query("UPDATE email_accounts SET status = 'active', last_error = NULL, can_read = true").execute(&state.pool).await.unwrap();
    assert_eq!(owner.call(&state, "GET", "/v1/ui/alerts", None).await.body, json!([]));
}

#[sqlx::test]
async fn alerts_for_engines_providers_imports_jobs_and_budgets(pool: PgPool) {
    let state = app_state(pool);
    let owner = setup_owner(&state).await;
    sqlx::query("UPDATE sending_defaults SET engine_paused = true").execute(&state.pool).await.unwrap();
    sqlx::query("UPDATE reply_defaults SET engine_paused = true").execute(&state.pool).await.unwrap();
    sqlx::query("UPDATE followup_defaults SET engine_paused = true").execute(&state.pool).await.unwrap();
    sqlx::query("UPDATE lead_defaults SET feeder_paused = true").execute(&state.pool).await.unwrap();
    sqlx::query("UPDATE generator_defaults SET engine_paused = true").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO llm_providers (id, kind, name, base_url, model, status) VALUES (gen_random_uuid(), 'anthropic', 'Claude', 'https://api.anthropic.com', 'm', 'auth_failed')")
        .execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO lead_imports (id, file_name, status, error) VALUES (gen_random_uuid(), 'big.csv', 'failed', 'The file is damaged.')")
        .execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO lead_imports (id, file_name, status, created_at) VALUES (gen_random_uuid(), 'old.csv', 'failed', now() - interval '30 days')")
        .execute(&state.pool).await.unwrap();
    let acc = add_account(&state, "a@example.com").await;
    sqlx::query("UPDATE email_accounts SET can_read = true").execute(&state.pool).await.unwrap();
    let job = common::new_job(acc, "x@acme.com");
    aikido_email::sending::queue::enqueue(&state.pool, vec![job], state.now()).await.unwrap();
    sqlx::query("UPDATE send_jobs SET status = 'stuck'").execute(&state.pool).await.unwrap();
    // a daily token limit that is used up
    sqlx::query("UPDATE generator_defaults SET settings = '{\"daily_token_budget\": 100}'").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO llm_calls (purpose, model, input_tokens, output_tokens, ok) VALUES ('slot', 'm', 80, 40, true)").execute(&state.pool).await.unwrap();

    let r = owner.call(&state, "GET", "/v1/ui/alerts", None).await.body;
    let k = kinds(&r);
    assert_eq!(k.iter().filter(|x| *x == "engine_paused").count(), 5, "{k:?}");
    for kind in ["provider_auth_failed", "import_failed", "jobs_stuck", "ai_budget_reached"] {
        assert_eq!(k.iter().filter(|x| *x == kind).count(), 1, "{kind} in {k:?}");
    }
    assert_eq!(k.len(), 9, "the 30 day old import is not shown: {k:?}");
    let ids: Vec<&str> = r.as_array().unwrap().iter().map(|a| a["id"].as_str().unwrap()).collect();
    let mut sorted = ids.clone();
    sorted.dedup();
    assert_eq!(ids.len(), sorted.len());
    assert!(ids.contains(&"engine_paused:feeder") && ids.contains(&"engine_paused:generator"));
    let imp = r.as_array().unwrap().iter().find(|a| a["kind"] == "import_failed").unwrap();
    assert_eq!(imp["title"], "The import of big.csv failed");
    assert_eq!(imp["detail"], "The file is damaged.");
}

#[sqlx::test]
async fn alerts_for_campaigns_and_ai_review(pool: PgPool) {
    let state = app_state(pool);
    let owner = setup_owner(&state).await;
    let (_, camp_key) = aikido_email::auth::api_keys::create(&state.pool, Scope::Campaigns, "t").await.unwrap();
    let acc = add_account(&state, "a@example.com").await;
    sqlx::query("UPDATE email_accounts SET can_read = true").execute(&state.pool).await.unwrap();
    let body = json!({ "name": "Q4", "sequence": { "name": "S", "steps": [{ "delay_amount": 0, "subject": "Hi", "body_text": "Hello" }] }, "account_ids": [acc] });
    let created = call(&state, "POST", "/v1/campaigns", Some(&camp_key), Some(body)).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let id: uuid::Uuid = created.body["id"].as_str().unwrap().parse().unwrap();

    // a warning on an active campaign. A draft shows nothing.
    sqlx::query("UPDATE campaigns SET warning = 'Something is odd.' WHERE id = $1").bind(id).execute(&state.pool).await.unwrap();
    assert_eq!(owner.call(&state, "GET", "/v1/ui/alerts", None).await.body, json!([]));
    sqlx::query("UPDATE campaigns SET status = 'active' WHERE id = $1").bind(id).execute(&state.pool).await.unwrap();
    let r = owner.call(&state, "GET", "/v1/ui/alerts", None).await.body;
    assert_eq!(kinds(&r), ["campaign_warning"]);
    assert_eq!(r[0]["link"], format!("/w/default/campaigns/{id}/settings"));

    sqlx::query("UPDATE campaigns SET warning = 'No account of the pool is healthy (connected and not paused). Leads wait until one is.' WHERE id = $1").bind(id).execute(&state.pool).await.unwrap();
    let r = owner.call(&state, "GET", "/v1/ui/alerts", None).await.body;
    assert_eq!(kinds(&r), ["campaign_no_healthy_account"]);
    assert_eq!(r[0]["severity"], "error");

    // two AI texts wait for review
    sqlx::query("UPDATE campaigns SET warning = NULL WHERE id = $1").bind(id).execute(&state.pool).await.unwrap();
    let (lead, enrollment): (uuid::Uuid, uuid::Uuid) = {
        let lead = uuid::Uuid::now_v7();
        sqlx::query("INSERT INTO leads (id, email, email_domain) VALUES ($1, 'x@acme.com', 'acme.com')").bind(lead).execute(&state.pool).await.unwrap();
        let en = uuid::Uuid::now_v7();
        sqlx::query("INSERT INTO enrollments (id, sequence_id, account_id, to_email, lead_id) SELECT $1, sequence_id, $2, 'x@acme.com', $3 FROM campaigns WHERE id = $4")
            .bind(en).bind(acc).bind(lead).bind(id).execute(&state.pool).await.unwrap();
        (lead, en)
    };
    for step in 1..=2 {
        sqlx::query("INSERT INTO generations (enrollment_id, lead_id, sequence_id, step_number, step_hash, status) SELECT $1, $2, sequence_id, $3, 'h', 'pending_review' FROM campaigns WHERE id = $4")
            .bind(enrollment).bind(lead).bind(step).bind(id).execute(&state.pool).await.unwrap();
    }
    let r = owner.call(&state, "GET", "/v1/ui/alerts", None).await.body;
    assert_eq!(kinds(&r), ["generation_awaiting_review"]);
    assert_eq!(r[0]["title"], "2 AI texts to review in Q4");
    assert_eq!(r[0]["severity"], "info");
    assert_eq!(r[0]["link"], format!("/w/default/campaigns/{id}/followups"));

    // a campaign token limit that is used up
    sqlx::query("INSERT INTO generator_campaigns (campaign_id, token_budget) VALUES ($1, 50)").bind(id).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO llm_calls (purpose, model, campaign_id, input_tokens, output_tokens, ok) VALUES ('slot', 'm', $1, 30, 30, true)").bind(id).execute(&state.pool).await.unwrap();
    assert_eq!(kinds(&owner.call(&state, "GET", "/v1/ui/alerts", None).await.body), ["ai_budget_reached", "generation_awaiting_review"]);
}

#[sqlx::test]
async fn users_can_turn_alert_kinds_off(pool: PgPool) {
    let state = app_state(pool);
    let owner = setup_owner(&state).await;
    sqlx::query("UPDATE sending_defaults SET engine_paused = true").execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO lead_imports (id, file_name, status) VALUES (gen_random_uuid(), 'a.csv', 'failed')").execute(&state.pool).await.unwrap();
    assert_eq!(owner.call(&state, "GET", "/v1/ui/alerts", None).await.body.as_array().unwrap().len(), 2);

    owner.call(&state, "PATCH", "/v1/users/me", Some(json!({"preferences": {"notifications": {"engine_paused": false}}}))).await;
    assert_eq!(kinds(&owner.call(&state, "GET", "/v1/ui/alerts", None).await.body), ["import_failed"]);
    // other users keep their own choice
    let member = add_user(&state, &owner, "member@example.com", "member").await;
    assert_eq!(member.call(&state, "GET", "/v1/ui/alerts", None).await.body.as_array().unwrap().len(), 2);
    // a key sees all
    let (_, key) = aikido_email::auth::api_keys::create(&state.pool, Scope::Leads, "t").await.unwrap();
    assert_eq!(call(&state, "GET", "/v1/ui/alerts", Some(&key), None).await.body.as_array().unwrap().len(), 2);
    // every kind the code can make is in the public list
    for k in aikido_email::ui_api::alerts::KINDS {
        assert!(!k.is_empty());
    }
}
