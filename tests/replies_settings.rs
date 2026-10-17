mod common;
use aikido_email::accounts::model::*;
use aikido_email::accounts::repo;
use aikido_email::auth::api_keys::Scope;
use axum::http::StatusCode;
use common::*;
use secrecy::SecretString;
use serde_json::json;
use sqlx::PgPool;

async fn keys(pool: PgPool) -> (aikido_email::state::AppState, String) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let key = keys[&Scope::Replies].clone();
    (state, key)
}

#[sqlx::test]
async fn defaults_can_be_read_and_changed(pool: PgPool) {
    let (st, key) = keys(pool).await;
    let r = call(&st, "GET", "/v1/replies/defaults", Some(&key), None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["settings"]["poll_interval_secs"], 120);
    assert_eq!(r.body["settings"]["folders"], json!(["inbox", "spam"]));

    let r = call(&st, "PATCH", "/v1/replies/defaults", Some(&key), Some(json!({ "poll_interval_secs": 60, "stop_scope": "domain" }))).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["settings"]["poll_interval_secs"], 60);
    assert_eq!(r.body["settings"]["stop_scope"], "domain");
    // Other fields stay.
    assert_eq!(r.body["settings"]["soft_bounce_limit"], 3);
}

#[sqlx::test]
async fn bad_settings_get_422_with_a_clear_message(pool: PgPool) {
    let (st, key) = keys(pool).await;
    for (patch, needle) in [
        (json!({ "poll_interval_secs": 5 }), "30 to 3600"),
        (json!({ "folders": [] }), "not be empty"),
        (json!({ "folders": ["trash"] }), "not a folder"),
        (json!({ "initial_lookback_days": 200 }), "0 to 90"),
        (json!({ "unsubscribe_keywords": [""] }), "empty item"),
        (json!({ "soft_bounce_limit": 0 }), "1 to 10"),
        (json!({ "nope": 1 }), "not a reply setting"),
        (json!({ "stop_scope": "all" }), "unknown variant"),
    ] {
        let r = call(&st, "PATCH", "/v1/replies/defaults", Some(&key), Some(patch.clone())).await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{patch}");
        assert!(r.body["error"]["message"].as_str().unwrap().contains(needle), "{patch}: {}", r.body);
    }
    // Nothing was saved.
    let r = call(&st, "GET", "/v1/replies/defaults", Some(&key), None).await;
    assert_eq!(r.body["settings"]["poll_interval_secs"], 120);
}

#[sqlx::test]
async fn new_accounts_copy_the_defaults_and_can_differ(pool: PgPool) {
    let (st, key) = keys(pool).await;
    call(&st, "PATCH", "/v1/replies/defaults", Some(&key), Some(json!({ "soft_bounce_limit": 5 }))).await;
    let a = add_account(&st, "a@example.com").await;
    let b = add_account(&st, "b@example.com").await;
    let r = call(&st, "GET", &format!("/v1/replies/accounts/{a}/settings"), Some(&key), None).await;
    assert_eq!(r.body["settings"]["soft_bounce_limit"], 5);

    let r = call(&st, "PATCH", &format!("/v1/replies/accounts/{a}/settings"), Some(&key), Some(json!({ "match_window_days": 7 }))).await;
    assert_eq!(r.body["settings"]["match_window_days"], 7);
    let r = call(&st, "GET", &format!("/v1/replies/accounts/{b}/settings"), Some(&key), None).await;
    assert_eq!(r.body["settings"]["match_window_days"], 30);

    let r = call(&st, "POST", &format!("/v1/replies/accounts/{a}/settings/reset"), Some(&key), None).await;
    assert_eq!(r.body["settings"]["match_window_days"], 30);
    assert_eq!(r.body["settings"]["soft_bounce_limit"], 5);

    let r = call(&st, "PATCH", &format!("/v1/replies/accounts/{a}/settings"), Some(&key), Some(json!({ "match_window_days": 181 }))).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = call(&st, "GET", &format!("/v1/replies/accounts/{}/settings", uuid::Uuid::now_v7()), Some(&key), None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn the_replies_scope_is_enforced(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let r = call(&state, "GET", "/v1/replies/defaults", Some(&keys[&Scope::Sending]), None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = call(&state, "GET", "/v1/replies/defaults", Some(&keys[&Scope::Admin]), None).await;
    assert_eq!(r.status, StatusCode::OK);
    let r = call(&state, "GET", "/v1/replies/defaults", None, None).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
}

fn oauth(scope: &str) -> Secrets {
    Secrets::OAuth(OAuthSecrets {
        access_token: SecretString::from("tok".to_string()),
        refresh_token: Some(SecretString::from("ref".to_string())),
        scope: Some(scope.into()),
    })
}

fn new_oauth(email: &str, provider: Provider) -> repo::NewAccount {
    repo::NewAccount { email: email.into(), provider, display_name: String::new(), smtp: None, imap: None, token_expires_at: None }
}

#[sqlx::test]
async fn read_access_follows_the_scopes_the_provider_granted(pool: PgPool) {
    let st = app_state(pool);
    // Gmail with the read scope.
    let a = repo::upsert_connected(&st.pool, &st.crypto, new_oauth("g@example.com", Provider::Gmail),
        &oauth("https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/gmail.send")).await.unwrap();
    assert!(a.can_read);
    assert_eq!(a.read_status.as_deref(), Some("ok"));
    assert_eq!(a.granted_scopes.len(), 2);
    // Gmail where the user un-ticked the read box: sending only.
    let b = repo::upsert_connected(&st.pool, &st.crypto, new_oauth("g2@example.com", Provider::Gmail),
        &oauth("https://www.googleapis.com/auth/gmail.send")).await.unwrap();
    assert!(!b.can_read);
    assert_eq!(b.read_status.as_deref(), Some("reconnect_needed"));
    // Connect again with the read scope: same row, now readable.
    let b2 = repo::upsert_connected(&st.pool, &st.crypto, new_oauth("g2@example.com", Provider::Gmail),
        &oauth("https://www.googleapis.com/auth/gmail.send https://www.googleapis.com/auth/gmail.readonly")).await.unwrap();
    assert_eq!(b2.id, b.id);
    assert!(b2.can_read);
    // Microsoft.
    let m = repo::upsert_connected(&st.pool, &st.crypto, new_oauth("m@example.com", Provider::Microsoft),
        &oauth("https://graph.microsoft.com/Mail.Read https://graph.microsoft.com/Mail.Send offline_access")).await.unwrap();
    assert!(m.can_read);
    let m2 = repo::upsert_connected(&st.pool, &st.crypto, new_oauth("m2@example.com", Provider::Microsoft),
        &oauth("https://graph.microsoft.com/Mail.Send")).await.unwrap();
    assert!(!m2.can_read);
    // Wrong provider's scope name does not count.
    assert!(!has_read_scope(Provider::Gmail, &["Mail.Read".to_string()]));
}

#[sqlx::test]
async fn smtp_accounts_can_read_only_with_imap_details(pool: PgPool) {
    let st = app_state(pool);
    let send_only = add_account(&st, "s@example.com").await;
    let row = repo::get(&st.pool, send_only).await.unwrap().unwrap();
    assert!(!row.can_read);
    assert_eq!(row.read_status.as_deref(), Some("not_configured"));

    let secrets = Secrets::Password(PasswordSecrets {
        smtp_username: Some("i@example.com".into()),
        smtp_password: Some(SecretString::from("p".to_string())),
        imap_username: Some("i@example.com".into()),
        imap_password: Some(SecretString::from("p".to_string())),
    });
    let imap = HostInfo { host: "localhost".into(), port: 3143, tls: TlsMode::Plain };
    let new = repo::NewAccount {
        email: "i@example.com".into(), provider: Provider::Smtp, display_name: String::new(),
        smtp: Some(HostInfo { host: "localhost".into(), port: 1025, tls: TlsMode::Plain }), imap: Some(imap), token_expires_at: None,
    };
    let acct = repo::upsert_connected(&st.pool, &st.crypto, new, &secrets).await.unwrap();
    assert!(acct.can_read);
}

#[sqlx::test]
async fn account_delete_wipes_reply_text_and_sync_rows(pool: PgPool) {
    let st = app_state(pool);
    let a = add_account(&st, "a@example.com").await;
    sqlx::query("INSERT INTO inbox_sync_state (account_id, folder) VALUES ($1, 'inbox')").bind(a).execute(&st.pool).await.unwrap();
    sqlx::query(
        "INSERT INTO inbound_messages (id, account_id, folder, provider_message_id, from_email, subject, received_at, label, match_method, reply_text)
         VALUES ($1, $2, 'inbox', 'p1', 'x@y.com', 'Secret subject', now(), 'reply', 'none', 'private words')",
    )
    .bind(uuid::Uuid::now_v7()).bind(a).execute(&st.pool).await.unwrap();
    repo::soft_delete(&st.pool, a).await.unwrap();
    let (text, subject): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT reply_text, subject FROM inbound_messages WHERE account_id = $1").bind(a).fetch_one(&st.pool).await.unwrap();
    assert!(text.is_none() && subject.is_none());
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM inbox_sync_state WHERE account_id = $1").bind(a).fetch_one(&st.pool).await.unwrap();
    assert_eq!(n, 0);
}
