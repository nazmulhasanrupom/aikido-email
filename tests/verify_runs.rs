//! Runs, the worker, the send-rule settings and the Reoon lead table. A `FakeVerifier`; no network.
mod common;
use aikido_email::auth::api_keys::Scope;
use aikido_email::state::AppState;
use aikido_email::verify_reoon::client::{FakeFactory, FakeVerifier, VerifyError};
use aikido_email::verify_reoon::worker::{VerifyEngine, tick};
use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

struct V {
    state: AppState,
    keys: HashMap<Scope, String>,
    fake: Arc<FakeVerifier>,
    next: std::sync::atomic::AtomicUsize,
}

impl V {
    async fn new(pool: PgPool) -> Self {
        let mut state = app_state(pool);
        let fake = FakeVerifier::new();
        state.verifier = Arc::new(FakeFactory(fake.clone()));
        let keys = bootstrap_keys(&state).await;
        let v = Self { state, keys, fake, next: Default::default() };
        let r = v.call(Scope::Integrations, "POST", "/v1/integrations/apps/reoon/install", Some(json!({ "api_key": "k" }))).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        v
    }
    async fn call(&self, scope: Scope, method: &str, uri: &str, body: Option<Value>) -> Response {
        call(&self.state, method, uri, Some(&self.keys[&scope]), body).await
    }
    async fn api(&self, method: &str, uri: &str, body: Option<Value>) -> Response {
        self.call(Scope::Integrations, method, uri, body).await
    }
    async fn leads(&self, n: usize) -> Vec<Uuid> {
        let mut ids = Vec::new();
        for _ in 0..n {
            let i = self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let r = self.call(Scope::Leads, "POST", "/v1/leads", Some(json!({ "email": format!("lead{i}@acme.example") }))).await;
            ids.push(r.body["id"].as_str().unwrap().parse().unwrap());
        }
        ids
    }
    async fn run(&self, ids: &[Uuid], force: bool) -> Uuid {
        let r = self.api("POST", "/v1/verify/reoon/runs", Some(json!({ "lead_ids": ids, "force": force }))).await;
        assert!(r.status.is_success(), "{}", r.body);
        r.body["run"]["id"].as_str().unwrap().parse().unwrap()
    }
    async fn tick_until_quiet(&self) {
        let e = VerifyEngine::new(self.state.clone());
        for _ in 0..30 {
            let r = tick(&e).await.unwrap();
            if r.claimed == 0 && r.started == 0 {
                break;
            }
        }
    }
    async fn get_run(&self, id: Uuid) -> Value {
        self.api("GET", &format!("/v1/verify/reoon/runs/{id}"), None).await.body
    }
}

#[sqlx::test]
async fn a_run_saves_results_reuses_recent_ones_and_force_calls_again(pool: PgPool) {
    let v = V::new(pool).await;
    let ids = v.leads(3).await;
    v.fake.push(Ok(FakeVerifier::with_status("catch_all")));
    let run = v.run(&ids, false).await;
    v.tick_until_quiet().await;
    let r = v.get_run(run).await;
    assert_eq!((r["status"].as_str(), r["done_count"].as_i64(), r["credits_used"].as_i64()), (Some("done"), Some(3), Some(3)));
    assert_eq!(v.fake.calls(), 3);

    let list = v.api("GET", "/v1/verify/reoon/leads", None).await.body;
    let statuses: Vec<_> = list["leads"].as_array().unwrap().iter().map(|l| l["result"]["status"].as_str().unwrap().to_string()).collect();
    assert_eq!(statuses.iter().filter(|s| *s == "safe").count(), 2);
    assert_eq!(statuses.iter().filter(|s| *s == "catch_all").count(), 1);
    let only = v.api("GET", "/v1/verify/reoon/leads?verification_status=catch_all", None).await.body;
    assert_eq!(only["leads"].as_array().unwrap().len(), 1);
    let none = v.api("GET", "/v1/verify/reoon/leads?verification_status=none", None).await.body;
    assert_eq!(none["leads"].as_array().unwrap().len(), 0);
    let (events,): (i64,) = sqlx::query_as("SELECT count(*) FROM lead_events WHERE kind = 'email_verified'").fetch_one(&v.state.pool).await.unwrap();
    assert_eq!(events, 3);

    // a new run: recent results are skipped for free; force checks again (the row is replaced, not doubled)
    let run2 = v.run(&ids, false).await;
    v.tick_until_quiet().await;
    assert_eq!((v.get_run(run2).await["skipped_count"].as_i64(), v.fake.calls()), (Some(3), 3));
    let run3 = v.run(&ids, true).await;
    v.tick_until_quiet().await;
    assert_eq!((v.get_run(run3).await["done_count"].as_i64(), v.fake.calls()), (Some(3), 6));
    let (rows,): (i64,) = sqlx::query_as("SELECT count(*) FROM email_verifications").fetch_one(&v.state.pool).await.unwrap();
    assert_eq!(rows, 3);
}

#[sqlx::test]
async fn two_worker_copies_make_one_call_per_item_and_a_dead_lease_is_taken_again(pool: PgPool) {
    let v = V::new(pool).await;
    v.api("PATCH", "/v1/verify/reoon/settings", Some(json!({ "concurrency": 20 }))).await;
    let ids = v.leads(50).await;
    let run = v.run(&ids, false).await;
    let (a, b) = (VerifyEngine::new(v.state.clone()), VerifyEngine::new(v.state.clone()));
    for _ in 0..10 {
        let (x, y) = tokio::join!(tick(&a), tick(&b));
        if x.unwrap().claimed + y.unwrap().claimed == 0 {
            break;
        }
    }
    assert_eq!(v.fake.calls(), 50);
    assert_eq!(v.get_run(run).await["status"], "done");

    // an item claimed by a worker that died: after the lease it is picked up again
    let ids = v.leads(1).await;
    let run = v.run(&ids, false).await;
    tick(&a).await.unwrap(); // starts the run (and checks the one item)
    sqlx::query("UPDATE verification_items SET state = 'claimed', claim_id = $1, claimed_at = now() - interval '10 minutes' WHERE run_id = $2")
        .bind(Uuid::now_v7())
        .bind(run)
        .execute(&v.state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE verification_runs SET status = 'running', done_count = 0, credits_used = 0 WHERE id = $1").bind(run).execute(&v.state.pool).await.unwrap();
    v.tick_until_quiet().await;
    assert_eq!(v.get_run(run).await["status"], "done");
}

#[sqlx::test]
async fn out_of_credits_and_a_refused_key_stop_the_run_and_resume_finishes_it(pool: PgPool) {
    let v = V::new(pool).await;
    let ids = v.leads(3).await;
    v.fake.push(Ok(FakeVerifier::safe()));
    v.fake.push(Err(VerifyError::OutOfCredits));
    v.api("PATCH", "/v1/verify/reoon/settings", Some(json!({ "concurrency": 1 }))).await;
    let run = v.run(&ids, false).await;
    v.tick_until_quiet().await;
    let r = v.get_run(run).await;
    assert_eq!((r["status"].as_str(), r["error"].as_str(), r["done_count"].as_i64()), (Some("failed"), Some("out_of_credits"), Some(1)));
    let (pending,): (i64,) = sqlx::query_as("SELECT count(*) FROM verification_items WHERE state = 'pending'").fetch_one(&v.state.pool).await.unwrap();
    assert_eq!(pending, 2);
    assert_eq!(v.api("POST", &format!("/v1/verify/reoon/runs/{run}/resume"), None).await.status, StatusCode::OK);
    v.tick_until_quiet().await;
    assert_eq!(v.get_run(run).await["status"], "done");

    // a refused key: the app is marked, the run stops, resume is refused until the key is replaced
    let ids = v.leads(1).await;
    v.fake.push(Err(VerifyError::Auth));
    let run = v.run(&ids, false).await;
    v.tick_until_quiet().await;
    assert_eq!(v.get_run(run).await["error"], "auth_failed");
    let apps = v.api("GET", "/v1/integrations/apps", None).await.body;
    assert_eq!(apps["apps"][0]["status"], "auth_failed");
    assert_eq!(v.api("POST", &format!("/v1/verify/reoon/runs/{run}/resume"), None).await.status, StatusCode::CONFLICT);
    assert_eq!(v.api("PATCH", "/v1/integrations/apps/reoon", Some(json!({ "api_key": "new" }))).await.status, StatusCode::OK);
    assert_eq!(v.api("POST", &format!("/v1/verify/reoon/runs/{run}/resume"), None).await.status, StatusCode::OK);
    v.tick_until_quiet().await;
    assert_eq!(v.get_run(run).await["status"], "done");
}

#[sqlx::test]
async fn the_send_rule_refuses_locked_statuses_and_a_member_can_start_a_run(pool: PgPool) {
    let v = V::new(pool).await;
    let r = v.api("PATCH", "/v1/verify/reoon/settings", Some(json!({ "send_rule": { "allowed": ["safe", "Spamtrap"], "allow_not_verified": false } }))).await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("status_not_allowed")));
    let ok = v.api("PATCH", "/v1/verify/reoon/settings", Some(json!({ "send_rule": { "allowed": ["safe", "catch_all"], "allow_not_verified": false } }))).await;
    assert_eq!(ok.body["settings"]["send_rule"]["allowed"], json!(["catch_all", "safe"]));
    assert_eq!(v.api("PATCH", "/v1/verify/reoon/settings", Some(json!({ "concurrency": 99 }))).await.status, StatusCode::UNPROCESSABLE_ENTITY);

    let ids = v.leads(1).await;
    let key = v.api("POST", "/v1/verify/reoon/runs", Some(json!({ "lead_ids": ids, "idempotency_key": "abc" }))).await;
    let again = v.api("POST", "/v1/verify/reoon/runs", Some(json!({ "lead_ids": ids, "idempotency_key": "abc" }))).await;
    assert_eq!((key.status, again.status), (StatusCode::CREATED, StatusCode::OK));
    assert_eq!(key.body["run"]["id"], again.body["run"]["id"]);
    assert_eq!(key.body["estimated_credits"], 1);
}
