mod common;
use aikido_email::auth::api_keys::Scope;
use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;

#[sqlx::test]
async fn same_idempotency_key_makes_one_job(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Sending];
    let account = add_account(&state, "a@example.com").await;
    let body = json!({ "jobs": [{
        "account_id": account, "to_email": "lead@company.com", "subject": "Quick question",
        "body_text": "Hi", "idempotency_key": "campaign-7-lead-42-step-1"
    }]});
    let first = call(&state, "POST", "/v1/sending/jobs", Some(k), Some(body.clone())).await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    assert_eq!(first.body["items"][0]["status"], "queued");
    let second = call(&state, "POST", "/v1/sending/jobs", Some(k), Some(body)).await;
    assert_eq!(second.body["items"][0]["status"], "duplicate");
    assert_eq!(first.body["items"][0]["job"]["id"], second.body["items"][0]["job"]["id"]);
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM send_jobs").fetch_one(&state.pool).await.unwrap();
    assert_eq!(n, 1);
}

#[sqlx::test]
async fn duplicate_keys_in_one_batch(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let account = add_account(&state, "a@example.com").await;
    let job = json!({ "account_id": account, "to_email": "lead@company.com", "subject": "S", "body_text": "B", "idempotency_key": "k1" });
    let r = call(&state, "POST", "/v1/sending/jobs", Some(&keys[&Scope::Sending]), Some(json!({ "jobs": [job.clone(), job] }))).await;
    assert_eq!(r.body["queued"], 1);
    assert_eq!(r.body["duplicates"], 1);
}

#[sqlx::test]
async fn bad_items_get_an_error_good_items_are_queued(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let account = add_account(&state, "a@example.com").await;
    let body = json!({ "jobs": [
        { "account_id": account, "to_email": "good@company.com", "subject": "Hi", "body_text": "x" },
        { "account_id": account, "to_email": "not-an-email", "subject": "Hi", "body_text": "x" },
        { "account_id": account, "to_email": "a@b.com", "subject": "Hi" },
        { "account_id": "0192aaaa-0000-7000-8000-000000000000", "to_email": "a@b.com", "subject": "Hi", "body_text": "x" },
        { "account_id": account, "to_email": "a@b.com", "subject": "Hi", "body_text": "x", "headers": { "From": "evil@x.com" } },
        { "account_id": account, "to_email": "a@b.com", "subject": "Hi\nBcc: x@y.com", "body_text": "x" },
        { "account_id": account, "to_email": "ok2@company.com", "subject": "Hi", "body_html": "<p>x</p>", "priority": 5 },
    ]});
    let r = call(&state, "POST", "/v1/sending/jobs", Some(&keys[&Scope::Sending]), Some(body)).await;
    assert_eq!(r.status, StatusCode::OK);
    let items = r.body["items"].as_array().unwrap();
    let codes: Vec<Value> = items.iter().map(|i| i["error"]["code"].clone()).collect();
    assert_eq!(codes[0], Value::Null);
    assert_eq!(codes[1], "invalid_to_email");
    assert_eq!(codes[2], "missing_body");
    assert_eq!(codes[3], "account_not_found");
    assert_eq!(codes[4], "reserved_header");
    assert_eq!(codes[5], "invalid_subject");
    assert_eq!(codes[6], Value::Null);
    assert_eq!(r.body["queued"], 2);
    assert_eq!(r.body["errors"], 5);
}

#[sqlx::test]
async fn batch_size_is_checked(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let r = call(&state, "POST", "/v1/sending/jobs", Some(&keys[&Scope::Sending]), Some(json!({ "jobs": [] }))).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.body["error"]["code"], "invalid_batch_size");
}

#[sqlx::test]
async fn list_filters_and_cursor_paging(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Sending];
    let a = add_account(&state, "a@example.com").await;
    let b = add_account(&state, "b@example.com").await;
    let campaign = uuid::Uuid::now_v7();
    let jobs: Vec<Value> = (0..5)
        .map(|i| json!({ "account_id": if i < 3 { a } else { b }, "to_email": format!("p{i}@company.com"), "subject": "S", "body_text": "B",
                         "campaign_id": if i == 4 { Some(campaign) } else { None } }))
        .collect();
    call(&state, "POST", "/v1/sending/jobs", Some(k), Some(json!({ "jobs": jobs }))).await;

    let all = call(&state, "GET", "/v1/sending/jobs?limit=2", Some(k), None).await;
    assert_eq!(all.body["jobs"].as_array().unwrap().len(), 2);
    let cursor = all.body["next_cursor"].as_str().unwrap();
    let mut seen: Vec<String> = all.body["jobs"].as_array().unwrap().iter().map(|j| j["to_email"].as_str().unwrap().to_string()).collect();
    let page2 = call(&state, "GET", &format!("/v1/sending/jobs?limit=2&cursor={cursor}"), Some(k), None).await;
    seen.extend(page2.body["jobs"].as_array().unwrap().iter().map(|j| j["to_email"].as_str().unwrap().to_string()));
    let cursor = page2.body["next_cursor"].as_str().unwrap();
    let page3 = call(&state, "GET", &format!("/v1/sending/jobs?limit=2&cursor={cursor}"), Some(k), None).await;
    seen.extend(page3.body["jobs"].as_array().unwrap().iter().map(|j| j["to_email"].as_str().unwrap().to_string()));
    assert!(page3.body["next_cursor"].is_null());
    assert_eq!(seen, ["p4@company.com", "p3@company.com", "p2@company.com", "p1@company.com", "p0@company.com"]);

    let by_account = call(&state, "GET", &format!("/v1/sending/jobs?account_id={a}"), Some(k), None).await;
    assert_eq!(by_account.body["jobs"].as_array().unwrap().len(), 3);
    let by_campaign = call(&state, "GET", &format!("/v1/sending/jobs?campaign_id={campaign}"), Some(k), None).await;
    assert_eq!(by_campaign.body["jobs"].as_array().unwrap().len(), 1);
    let by_status = call(&state, "GET", "/v1/sending/jobs?status=sent", Some(k), None).await;
    assert_eq!(by_status.body["jobs"].as_array().unwrap().len(), 0);
}

#[sqlx::test]
async fn cancel_works_only_while_queued_and_requeue_only_stuck_or_failed(pool: PgPool) {
    let h = Harness::new(pool);
    let keys = bootstrap_keys(&h.state).await;
    let k = &keys[&Scope::Sending];
    let account = h.account("a@example.com").await;
    let job = h.queue(account, "lead@company.com").await;

    let r = call(&h.state, "POST", &format!("/v1/sending/jobs/{job}/requeue"), Some(k), None).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let r = call(&h.state, "DELETE", &format!("/v1/sending/jobs/{job}"), Some(k), None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["status"], "cancelled");
    let again = call(&h.state, "DELETE", &format!("/v1/sending/jobs/{job}"), Some(k), None).await;
    assert_eq!(again.status, StatusCode::CONFLICT);

    sqlx::query("UPDATE send_jobs SET status = 'stuck', attempts = 1 WHERE id = $1").bind(job).execute(&h.state.pool).await.unwrap();
    let r = call(&h.state, "POST", &format!("/v1/sending/jobs/{job}/requeue"), Some(k), None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["status"], "queued");
    assert_eq!(r.body["attempts"], 0);

    let one = call(&h.state, "GET", &format!("/v1/sending/jobs/{job}"), Some(k), None).await;
    let events: Vec<&str> = one.body["events"].as_array().unwrap().iter().map(|e| e["event"].as_str().unwrap()).collect();
    assert_eq!(events, ["queued", "cancelled", "requeued"]);

    let missing = call(&h.state, "GET", "/v1/sending/jobs/0192aaaa-0000-7000-8000-000000000000", Some(k), None).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn jobs_keep_the_order_priority_then_time(pool: PgPool) {
    let h = Harness::new(pool);
    let account = h.account("a@example.com").await;
    let first = h.queue(account, "first@company.com").await;
    let second = h.queue(account, "second@company.com").await;
    let mut urgent = new_job(account, "urgent@company.com");
    urgent.priority = Some(10);
    let urgent = h.queue_job(urgent).await;
        h.tick().await;
    assert_eq!(h.job(urgent).await.status, aikido_email::sending::queue::JobStatus::Sent);
    assert_eq!(h.job(first).await.status, aikido_email::sending::queue::JobStatus::Queued);
    h.advance(chrono::Duration::minutes(20));
    h.tick().await;
    assert_eq!(h.job(first).await.status, aikido_email::sending::queue::JobStatus::Sent);
    assert_eq!(h.job(second).await.status, aikido_email::sending::queue::JobStatus::Queued);
}

#[sqlx::test]
async fn suppressions_api(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Sending];
    let r = call(&state, "POST", "/v1/sending/suppressions", Some(k), Some(json!({ "value": "Bad@Company.com", "kind": "email" }))).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["value"], "bad@company.com");
    assert_eq!(r.body["reason"], "manual");
    call(&state, "POST", "/v1/sending/suppressions", Some(k), Some(json!({ "value": "spam.com", "kind": "domain", "reason": "complained" }))).await;
    let bad = call(&state, "POST", "/v1/sending/suppressions", Some(k), Some(json!({ "value": "nope", "kind": "email" }))).await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);

    let list = call(&state, "GET", "/v1/sending/suppressions", Some(k), None).await;
    assert_eq!(list.body.as_array().unwrap().len(), 2);
    let only_domains = call(&state, "GET", "/v1/sending/suppressions?kind=domain", Some(k), None).await;
    assert_eq!(only_domains.body.as_array().unwrap().len(), 1);

    let del = call(&state, "DELETE", "/v1/sending/suppressions?value=BAD@company.com&kind=email", Some(k), None).await;
    assert_eq!(del.status, StatusCode::OK);
    let del = call(&state, "DELETE", "/v1/sending/suppressions?value=BAD@company.com&kind=email", Some(k), None).await;
    assert_eq!(del.status, StatusCode::NOT_FOUND);
}
