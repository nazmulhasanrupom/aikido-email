//! The app store (Reoon install, update, uninstall) and the balance route. A mock Reoon server and a fake verifier.
mod common;
use aikido_email::auth::api_keys::Scope;
use aikido_email::crypto::Crypto;
use aikido_email::verify_reoon::client::{FakeFactory, FakeVerifier, RealVerifierFactory, VerifyError};
use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

#[derive(Clone, Default)]
struct Mock {
    /// keys the mock accepts
    good: Arc<Mutex<Vec<String>>>,
    seen_paths: Arc<Mutex<Vec<String>>>,
}

async fn handler(State(m): State<Mock>, uri: axum::http::Uri, Query(q): Query<HashMap<String, String>>) -> impl IntoResponse {
    m.seen_paths.lock().unwrap().push(uri.path().to_string());
    let key = q.get("key").cloned().unwrap_or_default();
    if !m.good.lock().unwrap().contains(&key) {
        return (StatusCode::UNAUTHORIZED, "{}".to_string());
    }
    (StatusCode::OK, json!({ "api_status": "success", "remaining_daily_credits": 150, "remaining_instant_credits": 5000, "status": "ok" }).to_string())
}

async fn mock_reoon(good: &[&str]) -> (String, Mock) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mock = Mock::default();
    *mock.good.lock().unwrap() = good.iter().map(|s| s.to_string()).collect();
    let app = Router::new().fallback(handler).with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, mock)
}

async fn with_mock(pool: PgPool, good: &[&str]) -> (aikido_email::state::AppState, HashMap<Scope, String>, Mock) {
    let (url, mock) = mock_reoon(good).await;
    let mut state = app_state(pool);
    state.verifier = Arc::new(RealVerifierFactory { base_url: url, retry_delay: Duration::from_millis(5) });
    let keys = bootstrap_keys(&state).await;
    (state, keys, mock)
}

#[sqlx::test]
async fn a_bad_key_saves_nothing_and_a_good_key_installs(pool: PgPool) {
    let (state, keys, mock) = with_mock(pool, &["good-key-1"]).await;
    let k = &keys[&Scope::Integrations];
    let bad = call(&state, "POST", "/v1/integrations/apps/reoon/install", Some(k), Some(json!({ "api_key": "wrong-key" }))).await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(bad.body["error"]["code"], "integration_key_invalid");
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM integrations").fetch_one(&state.pool).await.unwrap();
    assert_eq!(n, 0);

    let ok = call(&state, "POST", "/v1/integrations/apps/reoon/install", Some(k), Some(json!({ "api_key": "good-key-1" }))).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    assert_eq!(ok.body["balance"]["daily_credits"], 150);
    assert_eq!(ok.body["app"]["installed"], true);
    assert!(!ok.body.to_string().contains("good-key-1"));
    assert!(mock.seen_paths.lock().unwrap().iter().all(|p| p.contains("check-account-balance")));

    let again = call(&state, "POST", "/v1/integrations/apps/reoon/install", Some(k), Some(json!({ "api_key": "good-key-1" }))).await;
    assert_eq!((again.status, again.body["error"]["code"].as_str()), (StatusCode::CONFLICT, Some("already_installed")));

    let apps = call(&state, "GET", "/v1/integrations/apps", Some(k), None).await;
    assert_eq!(apps.body["apps"][0]["app"], "reoon");
    assert_eq!(apps.body["apps"][0]["status"], "installed");
    assert_eq!(apps.body["apps"][0]["sidebar"], true);
    let unknown = call(&state, "POST", "/v1/integrations/apps/nope/install", Some(k), Some(json!({ "api_key": "x" }))).await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn the_key_is_stored_encrypted_and_bound_to_its_row(pool: PgPool) {
    let (state, keys, _m) = with_mock(pool, &["good-key-1"]).await;
    let k = &keys[&Scope::Integrations];
    call(&state, "POST", "/v1/integrations/apps/reoon/install", Some(k), Some(json!({ "api_key": "good-key-1" }))).await;
    let (id, ct, nonce, ver): (Uuid, Vec<u8>, Vec<u8>, i32) =
        sqlx::query_as("SELECT id, secret_ciphertext, secret_nonce, key_version FROM integrations").fetch_one(&state.pool).await.unwrap();
    assert!(!String::from_utf8_lossy(&ct).contains("good-key-1"));
    let crypto: &Crypto = &state.crypto;
    assert_eq!(&crypto.decrypt(&ct, &nonce, ver, id.as_bytes()).unwrap()[..], b"good-key-1");
    // moved to another row id, it can not be read
    assert!(crypto.decrypt(&ct, &nonce, ver, Uuid::now_v7().as_bytes()).is_err());
}

#[sqlx::test]
async fn update_tests_the_new_key_and_clears_auth_failed(pool: PgPool) {
    let (state, keys, _m) = with_mock(pool, &["good-key-1", "good-key-2"]).await;
    let k = &keys[&Scope::Integrations];
    call(&state, "POST", "/v1/integrations/apps/reoon/install", Some(k), Some(json!({ "api_key": "good-key-1" }))).await;
    sqlx::query("UPDATE integrations SET status = 'auth_failed'").execute(&state.pool).await.unwrap();
    let bad = call(&state, "PATCH", "/v1/integrations/apps/reoon", Some(k), Some(json!({ "api_key": "nope" }))).await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    let (s,): (String,) = sqlx::query_as("SELECT status FROM integrations").fetch_one(&state.pool).await.unwrap();
    assert_eq!(s, "auth_failed");
    let ok = call(&state, "PATCH", "/v1/integrations/apps/reoon", Some(k), Some(json!({ "api_key": "good-key-2" }))).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    assert_eq!(ok.body["status"], "installed");
}

#[sqlx::test]
async fn uninstall_deletes_the_key_cancels_runs_and_keeps_results(pool: PgPool) {
    let (state, keys, _m) = with_mock(pool, &["good-key-1"]).await;
    let k = &keys[&Scope::Integrations];
    call(&state, "POST", "/v1/integrations/apps/reoon/install", Some(k), Some(json!({ "api_key": "good-key-1" }))).await;
    let lead = call(&state, "POST", "/v1/leads", Some(&keys[&Scope::Leads]), Some(json!({ "email": "ann@acme.example" }))).await;
    let lead_id: Uuid = lead.body["id"].as_str().unwrap().parse().unwrap();
    let run = Uuid::now_v7();
    sqlx::query("INSERT INTO verification_runs (id, mode, status, total) VALUES ($1, 'power', 'running', 1)").bind(run).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO verification_items (run_id, lead_id, email) VALUES ($1, $2, 'ann@acme.example')").bind(run).bind(lead_id).execute(&state.pool).await.unwrap();
    sqlx::query("INSERT INTO email_verifications (id, lead_id, email, mode, status) VALUES ($1, $2, 'ann@acme.example', 'power', 'safe')")
        .bind(Uuid::now_v7())
        .bind(lead_id)
        .execute(&state.pool)
        .await
        .unwrap();

    assert_eq!(call(&state, "DELETE", "/v1/integrations/apps/reoon", Some(k), None).await.status, StatusCode::NO_CONTENT);
    let (keys_left,): (i64,) = sqlx::query_as("SELECT count(*) FROM integrations").fetch_one(&state.pool).await.unwrap();
    assert_eq!(keys_left, 0);
    let (run_status,): (String,) = sqlx::query_as("SELECT status FROM verification_runs").fetch_one(&state.pool).await.unwrap();
    assert_eq!(run_status, "cancelled");
    let (kept,): (i64,) = sqlx::query_as("SELECT count(*) FROM email_verifications").fetch_one(&state.pool).await.unwrap();
    assert_eq!(kept, 1);
    assert_eq!(call(&state, "DELETE", "/v1/integrations/apps/reoon", Some(k), None).await.status, StatusCode::NOT_FOUND);

    // erasing the lead removes its result
    sqlx::query("DELETE FROM leads WHERE id = $1").bind(lead_id).execute(&state.pool).await.unwrap();
    let (gone,): (i64,) = sqlx::query_as("SELECT count(*) FROM email_verifications").fetch_one(&state.pool).await.unwrap();
    assert_eq!(gone, 0);
}

#[sqlx::test]
async fn a_member_can_see_the_balance_but_not_install(pool: PgPool) {
    let mut state = app_state(pool);
    let fake = FakeVerifier::new();
    state.verifier = Arc::new(FakeFactory(fake.clone()));
    let _keys = bootstrap_keys(&state).await;
    let owner = setup_owner(&state).await;
    let member = add_user(&state, &owner, "member@example.com", "member").await;

    let denied = member.call(&state, "POST", "/v1/integrations/apps/reoon/install", Some(json!({ "api_key": "k" }))).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    assert_eq!(member.call(&state, "GET", "/v1/integrations/apps", None).await.status, StatusCode::OK);
    assert_eq!(member.call(&state, "GET", "/v1/verify/reoon/balance", None).await.body["error"]["code"], "not_installed");

    let ok = owner.call(&state, "POST", "/v1/integrations/apps/reoon/install", Some(json!({ "api_key": "k" }))).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    let bal = member.call(&state, "GET", "/v1/verify/reoon/balance", None).await;
    assert_eq!((bal.status, bal.body["instant_credits"].clone()), (StatusCode::OK, json!(5000)));
    assert_eq!(member.call(&state, "DELETE", "/v1/integrations/apps/reoon", None).await.status, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn a_refused_key_on_the_balance_route_sets_auth_failed(pool: PgPool) {
    let mut state = app_state(pool);
    let fake = FakeVerifier::new();
    state.verifier = Arc::new(FakeFactory(fake.clone()));
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Integrations];
    call(&state, "POST", "/v1/integrations/apps/reoon/install", Some(k), Some(json!({ "api_key": "k" }))).await;
    fake.set_balance(Err(VerifyError::Auth));
    let r = call(&state, "GET", "/v1/verify/reoon/balance", Some(k), None).await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::CONFLICT, Some("integration_auth_failed")));
    let apps: Value = call(&state, "GET", "/v1/integrations/apps", Some(k), None).await.body;
    assert_eq!(apps["apps"][0]["status"], "auth_failed");
}
