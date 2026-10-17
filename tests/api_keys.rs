mod common;
use aikido_email::auth::api_keys::{self, Scope};
use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

#[sqlx::test]
async fn first_boot_makes_eight_keys_second_boot_none(pool: PgPool) {
    let state = app_state(pool);
    let first = api_keys::bootstrap(&state.pool, &state.config).await.unwrap();
    assert_eq!(first.len(), 8);
    for (scope, key) in &first {
        assert!(key.starts_with(&format!("aik_{}_", scope.as_str())));
    }
    let second = api_keys::bootstrap(&state.pool, &state.config).await.unwrap();
    assert!(second.is_empty());
    // Only hashes are stored.
    let (plain_stored,): (i64,) = sqlx::query_as("SELECT count(*) FROM api_keys WHERE key_hash = convert_to($1, 'UTF8')")
        .bind(&first[0].1)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(plain_stored, 0);
}

#[sqlx::test]
async fn keys_from_env_are_used_and_not_printed(pool: PgPool) {
    let mut state = app_state(pool);
    let mut cfg = (*state.config).clone();
    cfg.env_api_keys.insert(Scope::Admin, "my-own-admin-key-from-env-0123456789".into());
    state.config = std::sync::Arc::new(cfg);
    let made = api_keys::bootstrap(&state.pool, &state.config).await.unwrap();
    assert_eq!(made.len(), 7, "every scope except admin is made by the server");
    let r = call(&state, "GET", "/v1/keys", Some("my-own-admin-key-from-env-0123456789"), None).await;
    assert_eq!(r.status, StatusCode::OK);
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
    assert_eq!(r.body.as_array().unwrap().len(), 8);
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
async fn an_old_database_gets_only_the_new_replies_key(pool: PgPool) {
    let state = app_state(pool);
    let first = api_keys::bootstrap(&state.pool, &state.config).await.unwrap();
    // Pretend this database was made before the Reply Check step: no `replies` key yet.
    sqlx::query("DELETE FROM api_keys WHERE scope = 'replies'").execute(&state.pool).await.unwrap();
    let next = api_keys::bootstrap(&state.pool, &state.config).await.unwrap();
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].0, Scope::Replies);
    assert!(next[0].1.starts_with("aik_replies_"));
    // The old keys still work.
    let sending = &first.iter().find(|(s, _)| *s == Scope::Sending).unwrap().1;
    assert_eq!(call(&state, "GET", "/v1/sending/engine/status", Some(sending), None).await.status, StatusCode::OK);
}

#[sqlx::test]
async fn an_old_database_gets_only_the_new_leads_and_campaigns_keys(pool: PgPool) {
    let state = app_state(pool);
    api_keys::bootstrap(&state.pool, &state.config).await.unwrap();
    sqlx::query("DELETE FROM api_keys WHERE scope IN ('leads', 'campaigns')").execute(&state.pool).await.unwrap();
    let mut next: Vec<_> = api_keys::bootstrap(&state.pool, &state.config).await.unwrap().into_iter().map(|k| k.0.as_str()).collect();
    next.sort();
    assert_eq!(next, ["campaigns", "leads"]);
}

#[sqlx::test]
async fn an_old_database_gets_only_the_new_generator_key(pool: PgPool) {
    let state = app_state(pool);
    api_keys::bootstrap(&state.pool, &state.config).await.unwrap();
    sqlx::query("DELETE FROM api_keys WHERE scope = 'generator'").execute(&state.pool).await.unwrap();
    let next: Vec<_> = api_keys::bootstrap(&state.pool, &state.config).await.unwrap().into_iter().map(|k| k.0.as_str()).collect();
    assert_eq!(next, ["generator"]);
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
