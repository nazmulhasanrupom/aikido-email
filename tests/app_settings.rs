//! Settings → Environment. These tests need Postgres (not run in the session that wrote them).
mod common;
use aikido_email::auth::api_keys::Scope;
use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

const URI: &str = "/v1/settings/environment";

// The saved values live in one process-wide copy, so these tests run one at a time.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[sqlx::test]
async fn save_read_back_and_clear(pool: PgPool) {
    let _one = ONE_AT_A_TIME.lock().await;
    let state = app_state(pool);
    let owner = setup_owner(&state).await;
    let put = owner
        .call(&state, "PUT", URI, Some(json!({ "values": { "GOOGLE_CLIENT_ID": " abc.apps.googleusercontent.com ", "GOOGLE_CLIENT_SECRET": "GOCSPX-secret-1234" } })))
        .await;
    assert_eq!(put.status, StatusCode::OK, "{:?}", put.body);
    let items = put.body["items"].as_array().unwrap();
    let id = items.iter().find(|i| i["key"] == "GOOGLE_CLIENT_ID").unwrap();
    assert_eq!(id["value"], "abc.apps.googleusercontent.com");
    assert_eq!(id["source"], "app");
    let secret = items.iter().find(|i| i["key"] == "GOOGLE_CLIENT_SECRET").unwrap();
    assert_eq!(secret["is_set"], true);
    assert_eq!(secret["last4"], "1234");
    assert!(secret["value"].is_null());
    assert!(!put.body.to_string().contains("GOCSPX-secret-1234"));

    // the secret is sealed in the table
    let (plain, sealed): (Option<String>, Option<Vec<u8>>) =
        sqlx::query_as("SELECT value, sealed FROM app_settings WHERE key = 'GOOGLE_CLIENT_SECRET'").fetch_one(&state.pool).await.unwrap();
    assert!(plain.is_none() && sealed.is_some());

    // the live config follows at once
    let cfg = owner.call(&state, "GET", "/v1/ui/config", None).await;
    assert_eq!(cfg.body["oauth"]["gmail"], true);

    let cleared = owner.call(&state, "PUT", URI, Some(json!({ "values": { "GOOGLE_CLIENT_ID": null, "GOOGLE_CLIENT_SECRET": null } }))).await;
    assert_eq!(cleared.status, StatusCode::OK);
    let cfg = owner.call(&state, "GET", "/v1/ui/config", None).await;
    assert_eq!(cfg.body["oauth"]["gmail"], false);
}

#[sqlx::test]
async fn bad_requests_are_refused_and_nothing_is_saved(pool: PgPool) {
    let _one = ONE_AT_A_TIME.lock().await;
    let state = app_state(pool);
    let owner = setup_owner(&state).await;
    let unknown = owner.call(&state, "PUT", URI, Some(json!({ "values": { "GOOGLE_CLIENT_ID": "ok.apps.googleusercontent.com", "DATABASE_URL": "x" } }))).await;
    assert_eq!(unknown.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(unknown.body["error"]["code"], "unknown_setting");
    let bad = owner.call(&state, "PUT", URI, Some(json!({ "values": { "MS_TENANT": "no spaces" } }))).await;
    assert_eq!(bad.body["error"]["code"], "invalid_value");
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM app_settings").fetch_one(&state.pool).await.unwrap();
    assert_eq!(n, 0);
    let group = owner.call(&state, "POST", &format!("{URI}/test/nonsense"), None).await;
    assert_eq!(group.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn only_an_admin_can_use_it(pool: PgPool) {
    let _one = ONE_AT_A_TIME.lock().await;
    let state = app_state(pool);
    let owner = setup_owner(&state).await;
    let member = add_user(&state, &owner, "member@example.com", "member").await;
    assert_eq!(member.call(&state, "GET", URI, None).await.status, StatusCode::FORBIDDEN);
    assert_eq!(call(&state, "GET", URI, None, None).await.status, StatusCode::UNAUTHORIZED);
    let keys = bootstrap_keys(&state).await;
    assert_eq!(call(&state, "GET", URI, Some(&keys[&Scope::Sending]), None).await.status, StatusCode::FORBIDDEN);
    assert_eq!(call(&state, "GET", URI, Some(&keys[&Scope::Admin]), None).await.status, StatusCode::OK);
}

#[sqlx::test]
async fn the_google_test_reads_the_keys(pool: PgPool) {
    let _one = ONE_AT_A_TIME.lock().await;
    let state = app_state(pool);
    let owner = setup_owner(&state).await;
    let r = owner.call(&state, "POST", &format!("{URI}/test/google"), None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["ok"], false);
    owner
        .call(&state, "PUT", URI, Some(json!({ "values": { "GOOGLE_CLIENT_ID": "1.apps.googleusercontent.com", "GOOGLE_CLIENT_SECRET": "GOCSPX-abcdefgh" } })))
        .await;
    let r = owner.call(&state, "POST", &format!("{URI}/test/google"), None).await;
    assert_eq!(r.body["ok"], true, "{:?}", r.body);
}
