mod common;
use aikido_email::auth::api_keys::Scope;
use axum::http::StatusCode;
use chrono::Duration;
use common::*;
use serde_json::json;
use sqlx::PgPool;

const ME: &str = "sender@acme-outreach.example";

struct R {
    h: ReplyHarness,
    key: String,
}

impl R {
    async fn new(pool: PgPool) -> R {
        let h = ReplyHarness::new(pool);
        let key = bootstrap_keys(&h.state).await[&Scope::Replies].clone();
        R { h, key }
    }
    async fn get(&self, path: &str) -> Response {
        call(&self.h.state, "GET", path, Some(&self.key), None).await
    }
    async fn post(&self, path: &str) -> Response {
        call(&self.h.state, "POST", path, Some(&self.key), None).await
    }
    async fn patch(&self, path: &str, body: serde_json::Value) -> Response {
        call(&self.h.state, "PATCH", path, Some(&self.key), Some(body)).await
    }
}

#[sqlx::test]
async fn status_sync_pause_and_resume(pool: PgPool) {
    let r = R::new(pool).await;
    let a = r.h.account(ME).await;
    sent_job(&r.h.state, a, "jane@prospect.example", "<sent-0001@acme-outreach.example>", None, r.h.now() - Duration::days(1)).await;
    r.h.fetcher.add(aikido_email::replies::fetchers::Folder::Inbox, fixture("human_plain.eml"), r.h.now() - Duration::hours(1));
    r.h.tick().await;

    let s = r.get(&format!("/v1/replies/accounts/{a}/status")).await;
    assert_eq!(s.status, StatusCode::OK);
    assert_eq!(s.body["can_read"], true);
    assert_eq!(s.body["read_status"], "ok");
    assert_eq!(s.body["folders"][0]["folder"], "inbox");
    assert!(s.body["folders"][0]["last_success_at"].is_string());
    assert!(s.body["folders"][0]["next_sync_at"].is_string());
    assert_eq!(s.body["labels_7d"]["reply"], 1);
    assert!(s.body["last_error"].is_null());

    // Sync now: 202, and the next tick reads at once.
    assert_eq!(r.h.tick().await.claimed, 0);
    let sync = r.post(&format!("/v1/replies/accounts/{a}/sync")).await;
    assert_eq!(sync.status, StatusCode::ACCEPTED);
    assert_eq!(r.h.tick().await.claimed, 1);

    // Pause: the poller leaves it alone. Resume: errors reset, read again.
    assert_eq!(r.post(&format!("/v1/replies/accounts/{a}/pause")).await.body["paused"], true);
    r.h.advance(Duration::hours(1));
    assert_eq!(r.h.tick().await.claimed, 0);
    sqlx::query("UPDATE inbox_sync_state SET consecutive_errors = 5, last_error = 'x'").execute(&r.h.state.pool).await.unwrap();
    assert_eq!(r.post(&format!("/v1/replies/accounts/{a}/resume")).await.body["paused"], false);
    let row = r.h.sync_row(a, "inbox").await;
    assert_eq!((row.consecutive_errors, row.paused, row.last_error), (0, false, None));
    assert_eq!(r.h.tick().await.claimed, 1);

    // An account that cannot be read: sync is refused with a clear message.
    let blind = add_account(&r.h.state, "blind@acme-outreach.example").await;
    let sync = r.post(&format!("/v1/replies/accounts/{blind}/sync")).await;
    assert_eq!(sync.status, StatusCode::CONFLICT);
    assert_eq!(sync.body["error"]["code"], "reading_not_available");
    let s = r.get(&format!("/v1/replies/accounts/{blind}/status")).await;
    assert_eq!(s.body["can_read"], false);
    assert_eq!(s.body["read_status"], "not_configured");

    assert_eq!(r.post(&format!("/v1/replies/accounts/{}/sync", uuid::Uuid::now_v7())).await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn messages_filters_paging_and_detail(pool: PgPool) {
    let r = R::new(pool).await;
    let a = r.h.account(ME).await;
    let (lead, campaign) = (uuid::Uuid::now_v7(), uuid::Uuid::now_v7());
    let job = sent_job(&r.h.state, a, "jane@prospect.example", "<sent-0001@acme-outreach.example>", None, r.h.now() - Duration::days(1)).await;
    sqlx::query("UPDATE send_jobs SET lead_id = $2, campaign_id = $3 WHERE id = $1").bind(job).bind(lead).bind(campaign).execute(&r.h.state.pool).await.unwrap();
    for (i, f) in ["human_plain.eml", "gmail_vacation.eml", "outlook_automatic_reply.eml"].iter().enumerate() {
        ingest_raw(&r.h.state, a, &format!("p{i}"), &fixture(f), r.h.now() - Duration::hours(3 - i as i64)).await;
    }
    let all = r.get("/v1/replies/messages").await;
    assert_eq!(all.body["messages"].as_array().unwrap().len(), 3);
    assert!(all.body["next_cursor"].is_null());
    assert_eq!(all.body["messages"][0]["lead_id"], lead.to_string());
    assert_eq!(all.body["messages"][0]["campaign_id"], campaign.to_string());

    let only = r.get("/v1/replies/messages?label=auto_reply").await;
    assert_eq!(only.body["messages"].as_array().unwrap().len(), 2);
    assert_eq!(r.get(&format!("/v1/replies/messages?lead_id={lead}")).await.body["messages"].as_array().unwrap().len(), 3);
    assert_eq!(r.get(&format!("/v1/replies/messages?campaign_id={}", uuid::Uuid::now_v7())).await.body["messages"].as_array().unwrap().len(), 0);
    assert_eq!(r.get(&format!("/v1/replies/messages?job_id={job}")).await.body["messages"].as_array().unwrap().len(), 3);
    assert_eq!(r.get(&format!("/v1/replies/messages?account_id={a}&label=reply")).await.body["messages"].as_array().unwrap().len(), 1);
    let since = (r.h.now() - Duration::minutes(150)).to_rfc3339().replace('+', "%2B");
    assert_eq!(r.get(&format!("/v1/replies/messages?since={since}")).await.body["messages"].as_array().unwrap().len(), 2);

    // Paging.
    let p1 = r.get("/v1/replies/messages?limit=2").await;
    assert_eq!(p1.body["messages"].as_array().unwrap().len(), 2);
    let cursor = p1.body["next_cursor"].as_str().unwrap().to_string();
    let p2 = r.get(&format!("/v1/replies/messages?limit=2&cursor={cursor}")).await;
    assert_eq!(p2.body["messages"].as_array().unwrap().len(), 1);
    assert!(p2.body["next_cursor"].is_null());
    assert_eq!(r.get("/v1/replies/messages?limit=0").await.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.get("/v1/replies/messages?label=nope").await.status, StatusCode::BAD_REQUEST);

    // Detail with actions.
    let id = all.body["messages"].as_array().unwrap().iter().find(|m| m["label"] == "reply").unwrap()["id"].as_str().unwrap().to_string();
    let one = r.get(&format!("/v1/replies/messages/{id}")).await;
    assert_eq!(one.status, StatusCode::OK);
    assert_eq!(one.body["label"], "reply");
    assert_eq!(one.body["match_method"], "in_reply_to");
    assert!(one.body["actions"].as_array().unwrap().iter().any(|x| x["type"] == "suppression"));
    assert_eq!(r.get(&format!("/v1/replies/messages/{}", uuid::Uuid::now_v7())).await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn fixing_a_label_by_hand_through_the_api(pool: PgPool) {
    let r = R::new(pool).await;
    let a = r.h.account(ME).await;
    sent_job(&r.h.state, a, "jane@prospect.example", "<sent-0001@acme-outreach.example>", None, r.h.now() - Duration::days(1)).await;
    ingest_raw(&r.h.state, a, "p1", &fixture("unsubscribe_short.eml"), r.h.now()).await;
    assert_eq!(suppression_reason(&r.h.state, "jane@prospect.example").await.as_deref(), Some("unsubscribed"));
    let (id,): (uuid::Uuid,) = sqlx::query_as("SELECT id FROM inbound_messages").fetch_one(&r.h.state.pool).await.unwrap();

    let p = r.patch(&format!("/v1/replies/messages/{id}"), json!({ "label": "reply" })).await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.body);
    assert_eq!((p.body["label"].as_str(), p.body["label_source"].as_str()), (Some("reply"), Some("manual")));
    assert_eq!(suppression_reason(&r.h.state, "jane@prospect.example").await.as_deref(), Some("replied"));

    assert_eq!(r.patch(&format!("/v1/replies/messages/{id}"), json!({ "label": "nonsense" })).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.patch(&format!("/v1/replies/messages/{}", uuid::Uuid::now_v7()), json!({ "label": "reply" })).await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn lookup_tells_what_one_address_did(pool: PgPool) {
    let r = R::new(pool).await;
    let a = r.h.account(ME).await;
    sent_job(&r.h.state, a, "jane@prospect.example", "<sent-0001@acme-outreach.example>", None, r.h.now() - Duration::days(1)).await;
    sent_job(&r.h.state, a, "dead@prospect.example", "<sent-dead@acme-outreach.example>", None, r.h.now() - Duration::days(1)).await;
    let blank = r.get("/v1/replies/lookup?email=Nobody@Prospect.example").await;
    assert_eq!(blank.status, StatusCode::OK);
    assert_eq!(blank.body["email"], "nobody@prospect.example");
    assert!(blank.body["replied_at"].is_null() && blank.body["bounced"].is_null() && blank.body["suppressed_as"].is_null());

    ingest_raw(&r.h.state, a, "p1", &fixture("human_plain.eml"), r.h.now()).await;
    let eml = dsn_eml("<sent-dead@acme-outreach.example>", "dead@prospect.example", "5.1.1", "failed");
    ingest_raw(&r.h.state, a, "b1", eml.as_bytes(), r.h.now()).await;

    let jane = r.get("/v1/replies/lookup?email=jane@prospect.example").await;
    assert!(jane.body["replied_at"].is_string());
    assert_eq!(jane.body["suppressed_as"], "replied");
    assert!(jane.body["bounced"].is_null());
    let dead = r.get("/v1/replies/lookup?email=dead@prospect.example").await;
    assert_eq!(dead.body["bounced"]["kind"], "hard");
    assert!(dead.body["bounced"]["at"].is_string());
    assert_eq!(dead.body["suppressed_as"], "hard_bounce");
    assert_eq!(r.get("/v1/replies/lookup?email=not-an-email").await.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test]
async fn engine_pause_resume_and_status(pool: PgPool) {
    let r = R::new(pool).await;
    let a = r.h.account(ME).await;
    sent_job(&r.h.state, a, "jane@prospect.example", "<sent-0001@acme-outreach.example>", None, r.h.now() - Duration::days(1)).await;
    ingest_raw(&r.h.state, a, "p1", &fixture("human_plain.eml"), r.h.now()).await;
    sqlx::query("UPDATE email_accounts SET read_status = NULL WHERE id <> $1").bind(uuid::Uuid::now_v7()).execute(&r.h.state.pool).await.unwrap();

    let s = r.get("/v1/replies/engine/status").await;
    assert_eq!(s.body["paused"], false);
    assert_eq!(s.body["today"]["reply"], 1);
    assert_eq!(s.body["accounts"]["unchecked"], 1);

    assert_eq!(r.post("/v1/replies/engine/pause").await.body["paused"], true);
    assert_eq!(r.get("/v1/replies/engine/status").await.body["paused"], true);
    assert!(r.h.tick().await.paused);
    assert_eq!(r.post("/v1/replies/engine/resume").await.body["paused"], false);
    assert!(!r.h.tick().await.paused);
}

#[sqlx::test]
async fn a_folder_change_in_the_settings_updates_the_sync_rows(pool: PgPool) {
    let r = R::new(pool).await;
    let a = r.h.account(ME).await;
    r.h.tick().await;
    assert_eq!(count(&r.h.state, "SELECT count(*) FROM inbox_sync_state").await, 1);
    let p = r.patch(&format!("/v1/replies/accounts/{a}/settings"), json!({ "folders": ["inbox", "spam"] })).await;
    assert_eq!(p.status, StatusCode::OK);
    assert_eq!(count(&r.h.state, "SELECT count(*) FROM inbox_sync_state").await, 2);
    r.patch(&format!("/v1/replies/accounts/{a}/settings"), json!({ "folders": ["spam"] })).await;
    let (folder,): (String,) = sqlx::query_as("SELECT folder FROM inbox_sync_state").fetch_one(&r.h.state.pool).await.unwrap();
    assert_eq!(folder, "spam");
}

#[sqlx::test]
async fn every_reply_route_needs_the_replies_scope(pool: PgPool) {
    let h = ReplyHarness::new(pool);
    let keys = bootstrap_keys(&h.state).await;
    let id = uuid::Uuid::now_v7();
    for (m, p) in [
        ("GET", format!("/v1/replies/accounts/{id}/status")), ("POST", format!("/v1/replies/accounts/{id}/sync")),
        ("POST", format!("/v1/replies/accounts/{id}/pause")), ("GET", "/v1/replies/messages".into()),
        ("GET", "/v1/replies/lookup?email=a@b.co".into()), ("POST", "/v1/replies/engine/pause".into()),
        ("GET", "/v1/replies/engine/status".into()),
    ] {
        assert_eq!(call(&h.state, m, &p, Some(&keys[&Scope::Sending]), None).await.status, StatusCode::FORBIDDEN, "{m} {p}");
        assert_eq!(call(&h.state, m, &p, None, None).await.status, StatusCode::UNAUTHORIZED, "{m} {p}");
        assert_ne!(call(&h.state, m, &p, Some(&keys[&Scope::Admin]), None).await.status, StatusCode::FORBIDDEN, "admin passes {m} {p}");
    }
}
