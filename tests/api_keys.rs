mod common;
use aikido_email::auth::api_keys::{self, Scope};
use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn first_boot_makes_no_keys(pool: PgPool) {
    let state = app_state(pool);
    api_keys::bootstrap(&state.pool, &state.config).await.unwrap();
    assert!(api_keys::list(&state.pool).await.unwrap().is_empty());
}

#[sqlx::test]
async fn keys_from_env_are_saved_and_the_rest_are_not_made(pool: PgPool) {
    let mut state = app_state(pool);
    let mut cfg = (*state.config).clone();
    cfg.env_api_keys.insert(Scope::Admin, "my-own-admin-key-from-env-0123456789".into());
    state.config = std::sync::Arc::new(cfg);
    api_keys::bootstrap(&state.pool, &state.config).await.unwrap();
    api_keys::bootstrap(&state.pool, &state.config).await.unwrap();
    let list = api_keys::list(&state.pool).await.unwrap();
    assert_eq!(list.len(), 1, "only the env key is saved, and only one time");
    assert_eq!(list[0].scope, "admin");
    let r = call(&state, "GET", "/v1/keys", Some("my-own-admin-key-from-env-0123456789"), None).await;
    assert_eq!(r.status, StatusCode::OK);
}

#[sqlx::test]
async fn an_env_key_is_skipped_when_the_scope_has_a_key(pool: PgPool) {
    let mut state = app_state(pool);
    let (_, made) = api_keys::create(&state.pool, Scope::Admin, "mine").await.unwrap();
    let mut cfg = (*state.config).clone();
    cfg.env_api_keys.insert(Scope::Admin, "my-own-admin-key-from-env-0123456789".into());
    state.config = std::sync::Arc::new(cfg);
    api_keys::bootstrap(&state.pool, &state.config).await.unwrap();
    assert_eq!(api_keys::list(&state.pool).await.unwrap().len(), 1);
    assert_eq!(call(&state, "GET", "/v1/keys", Some(&made), None).await.status, StatusCode::OK);
}

#[sqlx::test]
async fn auth_errors_and_scopes(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    // No key, bad key: 401.
    assert_eq!(call(&state, "GET", "/v1/accounts", None, None).await.status, StatusCode::UNAUTHORIZED);
    let bad = call(&state, "GET", "/v1/accounts", Some("aik_accounts_nope"), None).await;
    assert_eq!(bad.status, StatusCode::UNAUTHORIZED);
    assert_eq!(bad.body["error"]["code"], "invalid_api_key");
    // Right key, wrong scope: 403.
    let wrong = call(&state, "GET", "/v1/accounts", Some(&keys[&Scope::Sending]), None).await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);
    assert_eq!(wrong.body["error"]["code"], "wrong_scope");
    assert_eq!(call(&state, "GET", "/v1/sending/engine/status", Some(&keys[&Scope::Accounts]), None).await.status, StatusCode::FORBIDDEN);
    assert_eq!(call(&state, "GET", "/v1/keys", Some(&keys[&Scope::Sending]), None).await.status, StatusCode::FORBIDDEN);
    // Right scope: 200. Admin passes every scope.
    assert_eq!(call(&state, "GET", "/v1/accounts", Some(&keys[&Scope::Accounts]), None).await.status, StatusCode::OK);
    assert_eq!(call(&state, "GET", "/v1/sending/engine/status", Some(&keys[&Scope::Sending]), None).await.status, StatusCode::OK);
    assert_eq!(call(&state, "GET", "/v1/accounts", Some(&keys[&Scope::Admin]), None).await.status, StatusCode::OK);
    assert_eq!(call(&state, "GET", "/v1/sending/engine/status", Some(&keys[&Scope::Admin]), None).await.status, StatusCode::OK);
    // No key needed: health, docs, spec.
    assert_eq!(call(&state, "GET", "/health", None, None).await.status, StatusCode::OK);
    assert_eq!(call(&state, "GET", "/openapi.json", None, None).await.status, StatusCode::OK);
}

#[sqlx::test]
async fn x_api_key_header_works(pool: PgPool) {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let req = Request::builder().uri("/v1/accounts").header("x-api-key", &keys[&Scope::Accounts]).body(Body::empty()).unwrap();
    let resp = aikido_email::docs::app(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[sqlx::test]
async fn list_never_shows_the_key(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let r = call(&state, "GET", "/v1/keys", Some(&keys[&Scope::Admin]), None).await;
    let text = r.body.to_string();
    assert_eq!(r.body.as_array().unwrap().len(), 11);
    for key in keys.values() {
        assert!(!text.contains(key.as_str()));
    }
    assert!(text.contains("key_prefix"));
    assert!(!text.contains("key_hash"));
}

#[sqlx::test]
async fn rotate_revokes_the_old_key(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let admin = &keys[&Scope::Admin];
    let list = call(&state, "GET", "/v1/keys", Some(admin), None).await;
    let sending_id = list.body.as_array().unwrap().iter().find(|k| k["scope"] == "sending").unwrap()["id"].as_str().unwrap().to_string();

    let r = call(&state, "POST", &format!("/v1/keys/{sending_id}/rotate"), Some(admin), None).await;
    assert_eq!(r.status, StatusCode::OK);
    let new_key = r.body["plain_key"].as_str().unwrap().to_string();
    assert!(new_key.starts_with("aik_sending_"));
    // Old key: 401. New key: 200.
    assert_eq!(call(&state, "GET", "/v1/sending/engine/status", Some(&keys[&Scope::Sending]), None).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(call(&state, "GET", "/v1/sending/engine/status", Some(&new_key), None).await.status, StatusCode::OK);
    // Rotating a revoked key is refused.
    let again = call(&state, "POST", &format!("/v1/keys/{sending_id}/rotate"), Some(admin), None).await;
    assert_eq!(again.status, StatusCode::CONFLICT);
}

#[sqlx::test]
async fn rotate_with_grace_keeps_the_old_key_working(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let admin = &keys[&Scope::Admin];
    let list = call(&state, "GET", "/v1/keys", Some(admin), None).await;
    let id = list.body.as_array().unwrap().iter().find(|k| k["scope"] == "accounts").unwrap()["id"].as_str().unwrap().to_string();
    let r = call(&state, "POST", &format!("/v1/keys/{id}/rotate"), Some(admin), Some(json!({ "grace_hours": 2 }))).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(call(&state, "GET", "/v1/accounts", Some(&keys[&Scope::Accounts]), None).await.status, StatusCode::OK);
}

#[sqlx::test]
async fn the_last_admin_key_cannot_be_revoked(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let admin = &keys[&Scope::Admin];
    let list = call(&state, "GET", "/v1/keys", Some(admin), None).await;
    let admin_id = list.body.as_array().unwrap().iter().find(|k| k["scope"] == "admin").unwrap()["id"].as_str().unwrap().to_string();
    let r = call(&state, "DELETE", &format!("/v1/keys/{admin_id}"), Some(admin), None).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.body["error"]["code"], "last_admin_key");

    // With a second admin key, one can go.
    let made = call(&state, "POST", "/v1/keys", Some(admin), Some(json!({ "scope": "admin", "name": "second" }))).await;
    assert_eq!(made.status, StatusCode::CREATED);
    let r = call(&state, "DELETE", &format!("/v1/keys/{admin_id}"), Some(admin), None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(call(&state, "GET", "/v1/keys", Some(admin), None).await.status, StatusCode::UNAUTHORIZED);
    let second = made.body["plain_key"].as_str().unwrap();
    let (active,): (i64,) = sqlx::query_as("SELECT count(*) FROM api_keys WHERE scope = 'admin' AND revoked_at IS NULL")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(active, 1);
    assert_eq!(call(&state, "GET", "/v1/keys", Some(second), None).await.status, StatusCode::OK);
}

#[sqlx::test]
async fn error_shape_is_stable(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let r = call(&state, "POST", "/v1/keys", Some(&keys[&Scope::Admin]), Some(json!({ "scope": "nonsense" }))).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(r.body["error"]["code"].is_string());
    assert!(r.body["error"]["message"].is_string());
}

#[sqlx::test]
async fn the_generator_key_works_on_generator_routes_only(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Generator];
    assert!(k.starts_with("aik_generator_"));
    assert_eq!(call(&state, "GET", "/v1/generator/status", Some(k), None).await.status, StatusCode::OK);
    assert_eq!(call(&state, "GET", "/v1/followups/status", Some(k), None).await.status, StatusCode::FORBIDDEN);
    assert_eq!(call(&state, "GET", "/v1/generator/status", Some(&keys[&Scope::Followups]), None).await.status, StatusCode::FORBIDDEN);
    assert_eq!(call(&state, "GET", "/v1/generator/status", Some(&keys[&Scope::Admin]), None).await.status, StatusCode::OK);
    assert_eq!(call(&state, "GET", "/v1/generator/status", None, None).await.status, StatusCode::UNAUTHORIZED);
}

