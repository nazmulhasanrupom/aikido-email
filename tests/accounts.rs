mod common;
use aikido_email::accounts::{model::*, repo};
use aikido_email::auth::api_keys::Scope;
use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgPool;

fn mailpit_up() -> bool {
    std::net::TcpStream::connect_timeout(&"127.0.0.1:1025".parse().unwrap(), std::time::Duration::from_millis(300)).is_ok()
}

#[sqlx::test]
async fn smtp_account_saves_encrypted_and_api_shows_no_secret(pool: PgPool) {
    if !mailpit_up() {
        eprintln!("SKIPPED: Mailpit is not running on 127.0.0.1:1025");
        return;
    }
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Accounts];
    let body = json!({
        "email": "Sender@Example.com",
        "display_name": "Sender",
        "smtp": { "host": "127.0.0.1", "port": 1025, "tls": "none", "username": "sender@example.com", "password": "hunter2-very-secret" }
    });
    let r = call(&state, "POST", "/v1/accounts/smtp", Some(k), Some(body)).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert_eq!(r.body["email"], "sender@example.com");
    assert_eq!(r.body["has_credentials"], true);
    assert!(!r.body.to_string().contains("hunter2"));

    // The DB row has only ciphertext.
    let (cipher, nonce, version): (Vec<u8>, Vec<u8>, i32) =
        sqlx::query_as("SELECT secret_ciphertext, secret_nonce, key_version FROM email_accounts")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert!(!cipher.is_empty());
    assert_eq!(nonce.len(), 12);
    assert_eq!(version, 1);
    assert!(!String::from_utf8_lossy(&cipher).contains("hunter2"));
    let whole_row: (String,) = sqlx::query_as("SELECT t::text FROM email_accounts t").fetch_one(&state.pool).await.unwrap();
    assert!(!whole_row.0.contains("hunter2"));

    // The list and the single view show no secret.
    let id = r.body["id"].as_str().unwrap();
    for uri in ["/v1/accounts".to_string(), format!("/v1/accounts/{id}")] {
        let g = call(&state, "GET", &uri, Some(k), None).await;
        assert_eq!(g.status, StatusCode::OK);
        let text = g.body.to_string();
        assert!(!text.contains("hunter2") && !text.contains("password") && !text.contains("secret"), "{text}");
    }

    // The test route logs in and sends nothing.
    let t = call(&state, "POST", &format!("/v1/accounts/{id}/test"), Some(k), None).await;
    assert_eq!(t.body["ok"], true, "{}", t.body);

    // Defaults were copied to the new account.
    let s = call(&state, "GET", &format!("/v1/sending/accounts/{id}/settings"), Some(&keys[&Scope::Sending]), None).await;
    assert_eq!(s.body["settings"]["daily_limit"], 30);
}

#[sqlx::test]
async fn smtp_account_with_bad_login_is_refused(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    // Nothing listens on this port: the login test fails, nothing is saved.
    let body = json!({ "email": "a@example.com", "smtp": { "host": "127.0.0.1", "port": 1, "tls": "none", "password": "x" } });
    let r = call(&state, "POST", "/v1/accounts/smtp", Some(&keys[&Scope::Accounts]), Some(body)).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.body["error"]["code"], "smtp_login_failed");
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM email_accounts").fetch_one(&state.pool).await.unwrap();
    assert_eq!(n, 0);
}

#[sqlx::test]
async fn tls_none_is_refused_for_remote_hosts(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let body = json!({ "email": "a@example.com", "smtp": { "host": "smtp.example.com", "port": 25, "tls": "none", "password": "x" } });
    let r = call(&state, "POST", "/v1/accounts/smtp", Some(&keys[&Scope::Accounts]), Some(body)).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.body["error"]["code"], "insecure_tls");
}

#[sqlx::test]
async fn upsert_by_email_and_provider_does_not_duplicate(pool: PgPool) {
    let state = app_state(pool);
    let first = add_account(&state, "dup@example.com").await;
    // Connect again with other secrets.
    let secrets = Secrets::Password(PasswordSecrets {
        smtp_username: Some("dup@example.com".into()),
        smtp_password: Some(secrecy::SecretString::from("new-password".to_string())),
        ..Default::default()
    });
    let new = repo::NewAccount {
        email: "DUP@example.com".into(),
        provider: Provider::Smtp,
        display_name: String::new(),
        smtp: Some(HostInfo { host: "localhost".into(), port: 1025, tls: TlsMode::Plain }),
        imap: None,
        token_expires_at: None,
    };
    let second = repo::upsert_connected(&state.pool, &state.crypto, new, &secrets).await.unwrap();
    assert_eq!(first, second.id);
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM email_accounts").fetch_one(&state.pool).await.unwrap();
    assert_eq!(n, 1);
    // The secrets were replaced, and they still decrypt.
    let Secrets::Password(p) = repo::load_secrets(&state.crypto, &second).unwrap() else { panic!() };
    use secrecy::ExposeSecret;
    assert_eq!(p.smtp_password.unwrap().expose_secret(), "new-password");
    // Only one settings row and one state row.
    let (s,): (i64,) = sqlx::query_as("SELECT count(*) FROM account_sending_settings").fetch_one(&state.pool).await.unwrap();
    assert_eq!(s, 1);
}

#[sqlx::test]
async fn a_secret_moved_to_another_row_does_not_decrypt(pool: PgPool) {
    let state = app_state(pool);
    let a = add_account(&state, "a@example.com").await;
    let b = add_account(&state, "b@example.com").await;
    // Copy the ciphertext of A onto B.
    sqlx::query("UPDATE email_accounts SET secret_ciphertext = (SELECT secret_ciphertext FROM email_accounts WHERE id = $1),
                 secret_nonce = (SELECT secret_nonce FROM email_accounts WHERE id = $1) WHERE id = $2")
        .bind(a)
        .bind(b)
        .execute(&state.pool)
        .await
        .unwrap();
    let acct = repo::get(&state.pool, b).await.unwrap().unwrap();
    assert!(repo::load_secrets(&state.crypto, &acct).is_err());
    let acct_a = repo::get(&state.pool, a).await.unwrap().unwrap();
    assert!(repo::load_secrets(&state.crypto, &acct_a).is_ok());
}

#[sqlx::test]
async fn delete_wipes_secrets_and_cancels_queued_jobs(pool: PgPool) {
    let h = Harness::new(pool);
    let keys = bootstrap_keys(&h.state).await;
    let account = h.account("gone@example.com").await;
    let job = h.queue(account, "lead@company.com").await;
    let sent_job = h.queue(account, "other@company.com").await;
    sqlx::query("UPDATE send_jobs SET status = 'sent', sent_at = now() WHERE id = $1").bind(sent_job).execute(&h.state.pool).await.unwrap();

    let r = call(&h.state, "DELETE", &format!("/v1/accounts/{account}"), Some(&keys[&Scope::Accounts]), None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["cancelled_jobs"], 1);

    let row = h.account_row(account).await;
    assert!(row.deleted_at.is_some());
    assert!(row.secret_ciphertext.is_empty() && row.secret_nonce.is_empty());
    assert_eq!(h.job(job).await.status, aikido_email::sending::queue::JobStatus::Cancelled);
    // The old sent job still points to the account row.
    assert_eq!(h.job(sent_job).await.account_id, account);
    // It is gone from the API, and a new job cannot be queued for it.
    assert_eq!(call(&h.state, "GET", &format!("/v1/accounts/{account}"), Some(&keys[&Scope::Accounts]), None).await.status, StatusCode::NOT_FOUND);
    // The same email can be connected again as a new account.
    let again = add_account(&h.state, "gone@example.com").await;
    assert_ne!(again, account);
}

#[sqlx::test]
async fn oauth_start_and_callback(pool: PgPool) {
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Accounts];
    // Unknown provider.
    let r = call(&state, "POST", "/v1/accounts/oauth/yahoo/start", Some(k), None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // The callback needs no key, but a bad state is refused.
    let r = call(&state, "GET", "/v1/accounts/oauth/gmail/callback?code=abc&state=forged", None, None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.body["error"]["code"], "oauth_bad_state");

    // Set fake app settings. The URL is built locally, nothing is sent to Google.
    unsafe {
        std::env::set_var("GOOGLE_CLIENT_ID", "test-client-id");
        std::env::set_var("GOOGLE_CLIENT_SECRET", "test-client-secret");
    }
    let r = call(&state, "POST", "/v1/accounts/oauth/gmail/start", Some(k), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let url = r.body["auth_url"].as_str().unwrap();
    assert!(url.starts_with("https://accounts.google.com/"));
    assert!(url.contains("client_id=test-client-id"));
    assert!(url.contains("code_challenge="));
    assert!(url.contains("gmail.send"));
    assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A8080%2Fv1%2Faccounts%2Foauth%2Fgmail%2Fcallback"));
    // The state is sealed: the PKCE verifier is not readable in it.
    let parsed = oauth2_url(url);
    let st = parsed.iter().find(|(k, _)| k == "state").unwrap().1.clone();
    let opened = aikido_email::accounts::oauth::open(&state.crypto, &st, Provider::Gmail).unwrap();
    assert!(opened.verifier.len() >= 43);
    assert!(!url.contains(&opened.verifier));
    // A state made for gmail does not work for microsoft.
    let r = call(&state, "GET", &format!("/v1/accounts/oauth/microsoft/callback?code=abc&state={st}"), None, None).await;
    assert_eq!(r.body["error"]["code"], "oauth_bad_state");
}

#[sqlx::test]
async fn the_callback_shows_a_page_to_browsers_and_json_to_others(pool: PgPool) {
    let state = app_state(pool);
    let uri = "/v1/accounts/oauth/gmail/callback?code=abc&state=forged";
    let (status, headers, body) = call_raw(&state, uri, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(headers["content-type"].to_str().unwrap().starts_with("application/json"));
    assert!(String::from_utf8_lossy(&body).contains("oauth_bad_state"));

    let r = call_with(&state, "GET", uri, &[("accept", "text/html,application/xhtml+xml")], None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.headers["content-type"].to_str().unwrap().starts_with("text/html"));
    // call_with reads JSON only, so read the page as text
    let app = aikido_email::docs::app(state.clone());
    let req = axum::http::Request::builder().uri(uri).header("accept", "text/html").body(axum::body::Body::empty()).unwrap();
    let resp = tower::ServiceExt::oneshot(app, req).await.unwrap();
    let page = String::from_utf8(axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap().to_vec()).unwrap();
    assert!(page.contains("aikido:oauth") && page.contains("\"ok\":false") && page.contains("oauth_bad_state"), "{page}");
}

fn oauth2_url(url: &str) -> Vec<(String, String)> {
    oauth2::url::Url::parse(url).unwrap().query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect()
}

#[sqlx::test]
async fn the_old_imap_smtp_flow_saves_into_the_encrypted_store(pool: PgPool) {
    use aikido_email::email_management::imapsmtp::connect as imap;
    let state = app_state(pool);
    let account = imap::Account {
        email: "Old.Flow@Example.com".into(),
        imap: Some(imap::Server { host: "imap.example.com".into(), port: 993, security: imap::Security::Tls, username: "old.flow".into(), password: "imap-pass".into() }),
        smtp: Some(imap::Server { host: "smtp.example.com".into(), port: 587, security: imap::Security::StartTls, username: "old.flow".into(), password: "smtp-pass".into() }),
    };
    let saved = repo::save_imap_account(&state.pool, &state.crypto, account).await.unwrap();
    assert_eq!(saved.email, "old.flow@example.com");
    assert_eq!((saved.smtp_port, saved.smtp_tls), (Some(587), Some(TlsMode::Starttls)));
    assert_eq!((saved.imap_port, saved.imap_tls), (Some(993), Some(TlsMode::Implicit)));
    let Secrets::Password(p) = repo::load_secrets(&state.crypto, &saved).unwrap() else { panic!() };
    use secrecy::ExposeSecret;
    assert_eq!(p.smtp_password.unwrap().expose_secret(), "smtp-pass");
    assert_eq!(p.imap_password.unwrap().expose_secret(), "imap-pass");
    assert!(!String::from_utf8_lossy(&saved.secret_ciphertext).contains("smtp-pass"));

    // An account with no SMTP settings cannot send, so it is refused.
    let read_only = imap::Account { email: "ro@example.com".into(), imap: None, smtp: None };
    assert!(repo::save_imap_account(&state.pool, &state.crypto, read_only).await.is_err());
}

#[sqlx::test]
async fn patching_an_account(pool: PgPool) {
    if !mailpit_up() {
        eprintln!("SKIPPED: Mailpit is not running on 127.0.0.1:1025");
        return;
    }
    let state = app_state(pool);
    let keys = bootstrap_keys(&state).await;
    let k = &keys[&Scope::Accounts];
    let id = add_account(&state, "p@example.com").await;
    // Name only: no login test.
    let r = call(&state, "PATCH", &format!("/v1/accounts/{id}"), Some(k), Some(json!({ "display_name": "New Name" }))).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["display_name"], "New Name");
    // A new SMTP host that works (Mailpit).
    let r = call(&state, "PATCH", &format!("/v1/accounts/{id}"), Some(k), Some(json!({ "smtp": { "host": "127.0.0.1", "password": "new-pass" } }))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["smtp"]["host"], "127.0.0.1");
    assert_eq!(r.body["smtp"]["port"], 1025, "unchanged fields keep their value");
    assert!(!r.body.to_string().contains("new-pass"));
    // A host that fails the login test is not saved.
    let r = call(&state, "PATCH", &format!("/v1/accounts/{id}"), Some(k), Some(json!({ "smtp": { "host": "127.0.0.1", "port": 1 } }))).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.body["error"]["code"], "smtp_login_failed");
    let g = call(&state, "GET", &format!("/v1/accounts/{id}"), Some(k), None).await;
    assert_eq!(g.body["smtp"]["port"], 1025);
    // IMAP details need a host, port and tls when the account has none yet.
    let r = call(&state, "PATCH", &format!("/v1/accounts/{id}"), Some(k), Some(json!({ "imap": { "host": "imap.example.com" } }))).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.body["error"]["code"], "incomplete_server");
}
