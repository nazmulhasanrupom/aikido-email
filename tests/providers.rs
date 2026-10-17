//! Senders against a local Mailpit (SMTP) and against mock Gmail / Graph / token servers.
mod common;
use aikido_email::accounts::model::*;
use aikido_email::accounts::repo;
use aikido_email::sending::queue::JobStatus;
use aikido_email::sending::senders::gmail::GmailSender;
use aikido_email::sending::senders::microsoft::MicrosoftSender;
use aikido_email::sending::senders::smtp::{SmtpAuth, SmtpSender, build_transport};
use aikido_email::sending::senders::{MailSender, OutgoingMessage, SendError};
use aikido_email::sending::settings::ListUnsubscribe;
use aikido_email::sending::worker::{Engine, RealSenders};
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use base64::Engine as _;
use chrono::Duration;
use common::*;
use secrecy::SecretString;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

fn message(to: &str) -> OutgoingMessage {
    OutgoingMessage {
        message_id: "<test-1@example.com>".into(),
        from_email: "sender@example.com".into(),
        from_name: Some("Sender".into()),
        to_email: to.into(),
        to_name: Some("Sam".into()),
        subject: "Quick question".into(),
        body_text: Some("Hi Sam".into()),
        body_html: Some("<p>Hi Sam</p>".into()),
        headers: [("X-Campaign".to_string(), "c7".to_string())].into(),
        in_reply_to: Some("<prev@company.com>".into()),
        references: Some("<prev@company.com>".into()),
        list_unsubscribe: Some(ListUnsubscribe { mailto: Some("u@example.com".into()), url: None }),
        thread_id: Some("thread-1".into()),
    }
}

// ---------- SMTP through Mailpit ----------

fn mailpit_up() -> bool {
    std::net::TcpStream::connect_timeout(&"127.0.0.1:1025".parse().unwrap(), std::time::Duration::from_millis(300)).is_ok()
}

async fn mailpit_messages() -> Vec<Value> {
    let text = reqwest::get("http://127.0.0.1:8025/api/v1/messages").await.unwrap().text().await.unwrap();
    serde_json::from_str::<Value>(&text).unwrap()["messages"].as_array().cloned().unwrap_or_default()
}

#[tokio::test]
async fn smtp_sender_delivers_to_mailpit_with_the_right_headers() {
    if !mailpit_up() {
        eprintln!("SKIPPED: Mailpit is not running");
        return;
    }
    let _ = rustls::crypto::ring::default_provider().install_default();
    let transport = build_transport("127.0.0.1", 1025, TlsMode::Plain, SmtpAuth::None).unwrap();
    let sender = SmtpSender::new(transport);
    let mut msg = message(&format!("smtp-test-{}@company.com", uuid::Uuid::now_v7()));
    msg.subject = format!("SMTP test {}", uuid::Uuid::now_v7());
    sender.send(&msg).await.unwrap();

    let all = mailpit_messages().await;
    let found = all.iter().find(|m| m["Subject"] == msg.subject.as_str()).expect("the email arrived in Mailpit");
    let id = found["ID"].as_str().unwrap();
    let raw = reqwest::get(format!("http://127.0.0.1:8025/api/v1/message/{id}/raw")).await.unwrap().text().await.unwrap();
    for needle in [
        "Message-ID: <test-1@example.com>",
        "In-Reply-To: <prev@company.com>",
        "X-Campaign: c7",
        "List-Unsubscribe: <mailto:u@example.com>",
        "multipart/alternative",
        "Hi Sam",
    ] {
        assert!(raw.contains(needle), "missing {needle:?}");
    }
}

#[tokio::test]
async fn smtp_errors_are_mapped() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    // Nothing listens on port 1: a connection error before DATA is safe to retry.
    let sender = SmtpSender::new(build_transport("127.0.0.1", 1, TlsMode::Plain, SmtpAuth::None).unwrap());
    let err = sender.send(&message("a@company.com")).await.unwrap_err();
    assert!(matches!(err, SendError::Transient(_)), "{err:?}");
    // No encryption is refused for remote hosts.
    assert!(build_transport("smtp.example.com", 25, TlsMode::Plain, SmtpAuth::None).is_err());
    // A bad address never reaches the network.
    let mut bad = message("not-an-address");
    bad.to_email = "not-an-address".into();
    let err = sender.send(&bad).await.unwrap_err();
    assert!(matches!(err, SendError::Permanent { hard_bounce: false, .. }), "{err:?}");
}

/// A tiny SMTP server that answers each recipient with a scripted code.
async fn fake_smtp(rcpt_reply: &'static str, auth_reply: Option<&'static str>) -> u16 {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { return };
            tokio::spawn(async move {
                let (r, mut w) = stream.into_split();
                let mut r = BufReader::new(r);
                w.write_all(b"220 fake ESMTP\r\n").await.ok();
                let mut line = String::new();
                loop {
                    line.clear();
                    if r.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let cmd = line.trim().to_uppercase();
                    let reply: String = if cmd.starts_with("EHLO") || cmd.starts_with("HELO") {
                        "250-fake\r\n250 AUTH PLAIN\r\n".into()
                    } else if cmd.starts_with("AUTH") {
                        format!("{}\r\n", auth_reply.unwrap_or("235 ok"))
                    } else if cmd.starts_with("MAIL") {
                        "250 ok\r\n".into()
                    } else if cmd.starts_with("RCPT") {
                        format!("{rcpt_reply}\r\n")
                    } else if cmd.starts_with("DATA") {
                        "354 go\r\n".into()
                    } else if cmd.starts_with("QUIT") {
                        w.write_all(b"221 bye\r\n").await.ok();
                        return;
                    } else {
                        "250 ok\r\n".into()
                    };
                    w.write_all(reply.as_bytes()).await.ok();
                }
            });
        }
    });
    port
}

#[tokio::test]
async fn smtp_reply_codes_are_mapped() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    for (reply, expect) in [
        ("550 5.1.1 no such user", "hard_bounce"),
        ("553 bad address", "hard_bounce"),
        ("551 user not local", "hard_bounce"),
        ("554 rejected", "permanent"),
        ("421 busy", "transient"),
        ("450 mailbox busy", "transient"),
        ("451 local error", "transient"),
        ("452 no space", "transient"),
    ] {
        let port = fake_smtp(Box::leak(reply.to_string().into_boxed_str()), None).await;
        let sender = SmtpSender::new(build_transport("localhost", port, TlsMode::Plain, SmtpAuth::None).unwrap());
        let err = sender.send(&message("a@company.com")).await.unwrap_err();
        match (expect, &err) {
            ("hard_bounce", SendError::Permanent { hard_bounce: true, .. }) => {}
            ("permanent", SendError::Permanent { hard_bounce: false, .. }) => {}
            ("transient", SendError::Transient(_)) => {}
            _ => panic!("{reply} gave {err:?}"),
        }
    }
}

// ---------- mock Gmail, Graph and token servers ----------

#[derive(Default)]
struct Seen {
    gmail_bodies: Vec<(String, Value)>,
    graph_bodies: Vec<(String, String, String)>,
    token_calls: Vec<String>,
}

type Shared = Arc<Mutex<Seen>>;

/// The tests run in parallel and share one mock server: a panic in one must not poison the others.
fn lock(seen: &Shared) -> MutexGuard<'_, Seen> {
    seen.lock().unwrap_or_else(|e| e.into_inner())
}

fn bearer(h: &HeaderMap) -> String {
    h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).unwrap_or("").trim_start_matches("Bearer ").to_string()
}

async fn gmail_send(State(seen): State<Shared>, headers: HeaderMap, body: String) -> impl IntoResponse {
    let token = bearer(&headers);
    lock(&seen).gmail_bodies.push((token.clone(), serde_json::from_str(&body).unwrap_or(Value::Null)));
    match token.as_str() {
        "fresh-token" | "ok-token" => (StatusCode::OK, [(header::CONTENT_TYPE, "application/json")], r#"{"id":"gmail-id-1","threadId":"thread-9"}"#.to_string()),
        "limited-token" => (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "600")], "{}".to_string()),
        "daily-token" => (StatusCode::FORBIDDEN, [(header::CONTENT_TYPE, "application/json")], r#"{"error":{"message":"Daily limit","errors":[{"reason":"dailyLimitExceeded"}]}}"#.to_string()),
        "broken-token" => (StatusCode::SERVICE_UNAVAILABLE, [(header::CONTENT_TYPE, "text/plain")], "unavailable".to_string()),
        "bad-request-token" => (StatusCode::BAD_REQUEST, [(header::CONTENT_TYPE, "application/json")], r#"{"error":{"message":"Invalid To header"}}"#.to_string()),
        _ => (StatusCode::UNAUTHORIZED, [(header::CONTENT_TYPE, "application/json")], r#"{"error":{"message":"Invalid Credentials"}}"#.to_string()),
    }
}

async fn graph_send(State(seen): State<Shared>, headers: HeaderMap, body: String) -> impl IntoResponse {
    let token = bearer(&headers);
    let ct = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    lock(&seen).graph_bodies.push((token.clone(), ct, body));
    match token.as_str() {
        "fresh-token" | "ok-token" => StatusCode::ACCEPTED.into_response(),
        "limited-token" => (StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "45")], "{}").into_response(),
        "gateway-token" => StatusCode::GATEWAY_TIMEOUT.into_response(),
        _ => (StatusCode::UNAUTHORIZED, r#"{"error":{"code":"InvalidAuthenticationToken","message":"expired"}}"#).into_response(),
    }
}

/// The sent message read back from Gmail. For `ok-token` Gmail "replaced" our Message-ID.
async fn gmail_get_message(headers: HeaderMap) -> impl IntoResponse {
    let id = if bearer(&headers) == "ok-token" { "<CAreplaced123@mail.gmail.com>" } else { "<test-1@example.com>" };
    ([(header::CONTENT_TYPE, "application/json")], json!({ "id": "gmail-id-1", "payload": { "headers": [ { "name": "Message-ID", "value": id } ] } }).to_string())
}

/// The sent items. For `fresh-token`, Graph found the mail under our Message-ID. For `ok-token` it changed the id:
/// the lookup by our id is empty, and the newest item has a new id.
async fn graph_sent_items(headers: HeaderMap, axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>) -> impl IntoResponse {
    let ct = [(header::CONTENT_TYPE, "application/json")];
    if bearer(&headers) != "ok-token" {
        return (ct, r#"{"value":[{"id":"graph-id-1","conversationId":"conv-1"}]}"#.to_string());
    }
    let by_id = q.get("$filter").is_some_and(|f| f.starts_with("internetMessageId"));
    if by_id {
        return (ct, r#"{"value":[]}"#.to_string());
    }
    (ct, json!({ "value": [
        { "id": "other", "conversationId": "c0", "internetMessageId": "<x@y>", "subject": "Something else", "toRecipients": [] },
        { "id": "graph-id-1", "conversationId": "conv-1", "internetMessageId": "<graph-new-id@outlook.example>",
          "subject": "Quick question", "toRecipients": [ { "emailAddress": { "address": "Lead@Company.com" } } ] } ] }).to_string())
}

async fn token_endpoint(State(seen): State<Shared>, body: String) -> impl IntoResponse {
    let refresh = oauth2::url::form_urlencoded::parse(body.as_bytes()).find(|(k, _)| k == "refresh_token").map(|(_, v)| v.into_owned()).unwrap_or_default();
    lock(&seen).token_calls.push(refresh.clone());
    let ok = |extra: Value| {
        let mut v = json!({ "access_token": "fresh-token", "token_type": "Bearer", "expires_in": 3600 });
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        (StatusCode::OK, [(header::CONTENT_TYPE, "application/json")], v.to_string())
    };
    match refresh.as_str() {
        "refresh-ok" => ok(json!({})),
        r if r.starts_with("logcheck-refresh") => ok(json!({})),
        "refresh-rotating" => ok(json!({ "refresh_token": "refresh-rotated-2" })),
        "refresh-revoked" => (StatusCode::BAD_REQUEST, [(header::CONTENT_TYPE, "application/json")], r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#.to_string()),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, [(header::CONTENT_TYPE, "text/plain")], "boom".to_string()),
    }
}

/// One mock server for the whole test binary, on its own thread. Sets the env vars the code reads.
fn mock() -> Shared {
    static MOCK: OnceLock<Shared> = OnceLock::new();
    MOCK.get_or_init(|| {
        let seen: Shared = Arc::default();
        let app = Router::new()
            .route("/gmail/send", post(gmail_send))
            .route("/gmail/messages/{id}", get(gmail_get_message))
            .route("/graph/me/sendMail", post(graph_send))
            .route("/graph/me/mailFolders/sentitems/messages", get(graph_sent_items))
            .route("/token", post(token_endpoint))
            .with_state(seen.clone());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                tx.send(listener.local_addr().unwrap()).unwrap();
                axum::serve(listener, app).await.unwrap();
            });
        });
        let addr = rx.recv().unwrap();
        unsafe {
            std::env::set_var("GOOGLE_CLIENT_ID", "gid");
            std::env::set_var("GOOGLE_CLIENT_SECRET", "gsecret");
            std::env::set_var("GOOGLE_TOKEN_URL", format!("http://{addr}/token"));
            std::env::set_var("GMAIL_SEND_URL", format!("http://{addr}/gmail/send"));
            std::env::set_var("GMAIL_API_BASE_URL", format!("http://{addr}/gmail"));
            std::env::set_var("MS_CLIENT_ID", "mid");
            std::env::set_var("MS_CLIENT_SECRET", "msecret");
            std::env::set_var("MS_TOKEN_URL", format!("http://{addr}/token"));
            std::env::set_var("MS_GRAPH_BASE_URL", format!("http://{addr}/graph"));
            std::env::set_var("MS_SENT_LOOKUP_DELAY_MS", "0");
        }
        seen
    })
    .clone()
}

fn addr_of(var: &str) -> String {
    std::env::var(var).unwrap()
}

#[tokio::test]
async fn gmail_sender_posts_base64url_mime_and_maps_errors() {
    let seen = mock();
    let url = addr_of("GMAIL_SEND_URL");
    let send = |token: &str| {
        let sender = GmailSender::with_url(SecretString::from(token.to_string()), url.clone());
        let msg = message("lead@company.com");
        async move { sender.send(&msg).await }
    };

    let receipt = send("ok-token").await.unwrap();
    assert_eq!(receipt.provider_message_id.as_deref(), Some("gmail-id-1"));
    assert_eq!(receipt.provider_thread_id.as_deref(), Some("thread-9"));
    // Gmail replaced our Message-ID: the real one is read back and returned, so replies can be matched.
    assert_eq!(receipt.message_id_header.as_deref(), Some("<CAreplaced123@mail.gmail.com>"));
    // Gmail kept ours: nothing to save.
    assert_eq!(send("fresh-token").await.unwrap().message_id_header, None);
    let (_, body) = lock(&seen).gmail_bodies.iter().rev().find(|(t, _)| t == "ok-token").cloned().unwrap();
    assert_eq!(body["threadId"], "thread-1");
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(body["raw"].as_str().unwrap()).unwrap();
    let mime = String::from_utf8(raw).unwrap();
    for needle in ["To: Sam <lead@company.com>", "Subject: Quick question", "Message-ID: <test-1@example.com>", "In-Reply-To: <prev@company.com>", "multipart/alternative"] {
        assert!(mime.contains(needle), "missing {needle:?} in\n{mime}");
    }

    assert!(matches!(send("stale").await.unwrap_err(), SendError::AuthFailed(m) if m.contains("Invalid Credentials")));
    assert_eq!(send("limited-token").await.unwrap_err(), SendError::RateLimited { retry_after: Some(std::time::Duration::from_secs(600)), daily: false });
    assert!(matches!(send("daily-token").await.unwrap_err(), SendError::RateLimited { daily: true, .. }));
    assert!(matches!(send("broken-token").await.unwrap_err(), SendError::Transient(_)));
    assert!(matches!(send("bad-request-token").await.unwrap_err(), SendError::Permanent { hard_bounce: false, .. }));
}

#[tokio::test]
async fn microsoft_sender_posts_base64_mime_as_text_plain() {
    let seen = mock();
    let base = addr_of("MS_GRAPH_BASE_URL");
    let send = |token: &str| {
        let sender = MicrosoftSender::with_base_url(SecretString::from(token.to_string()), base.clone(), std::time::Duration::ZERO);
        let msg = message("lead@company.com");
        async move { sender.send(&msg).await }
    };
    let receipt = send("ok-token").await.unwrap();
    // The ids come from the sent-items lookup.
    assert_eq!(receipt.provider_message_id.as_deref(), Some("graph-id-1"));
    assert_eq!(receipt.provider_thread_id.as_deref(), Some("conv-1"));
    // Graph replaced our Message-ID: found by subject and recipient, and the real id is returned.
    assert_eq!(receipt.message_id_header.as_deref(), Some("<graph-new-id@outlook.example>"));
    // Graph kept ours: found by our id, nothing to save.
    assert_eq!(send("fresh-token").await.unwrap().message_id_header, None);
    let (_, ct, body) = lock(&seen).graph_bodies.iter().rev().find(|(t, _, _)| t == "ok-token").cloned().unwrap();
    assert_eq!(ct, "text/plain");
    let mime = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(body).unwrap()).unwrap();
    assert!(mime.contains("Message-ID: <test-1@example.com>") && mime.contains("In-Reply-To: <prev@company.com>"));

    assert!(matches!(send("stale").await.unwrap_err(), SendError::AuthFailed(_)));
    assert_eq!(send("limited-token").await.unwrap_err(), SendError::RateLimited { retry_after: Some(std::time::Duration::from_secs(45)), daily: false });
    assert!(matches!(send("gateway-token").await.unwrap_err(), SendError::Transient(_)));
}

// ---------- the worker with OAuth accounts ----------

async fn oauth_account(
    h: &Harness,
    email: &str,
    provider: Provider,
    access: &str,
    refresh: &str,
    expires_in_minutes: i64,
) -> uuid::Uuid {
    let secrets = Secrets::OAuth(OAuthSecrets {
        access_token: SecretString::from(access.to_string()),
        refresh_token: Some(SecretString::from(refresh.to_string())),
        scope: Some("gmail.send".into()),
    });
    let new = repo::NewAccount {
        email: email.into(),
        provider,
        display_name: "OAuth Sender".into(),
        smtp: None,
        imap: None,
        token_expires_at: Some(h.now() + Duration::minutes(expires_in_minutes)),
    };
    repo::upsert_connected(&h.state.pool, &h.state.crypto, new, &secrets).await.unwrap().id
}

fn real_engine(h: &mut Harness) {
    h.engine = Arc::new(Engine::new(h.state.clone(), Arc::new(RealSenders { dry_run: false })));
}

fn plain_token(h: &Harness, account: &EmailAccount) -> (String, Option<String>) {
    use secrecy::ExposeSecret;
    let Secrets::OAuth(o) = repo::load_secrets(&h.state.crypto, account).unwrap() else { panic!() };
    (o.access_token.expose_secret().to_string(), o.refresh_token.map(|r| r.expose_secret().to_string()))
}

#[sqlx::test]
async fn gmail_account_refreshes_an_old_token_before_sending(pool: PgPool) {
    let seen = mock();
    let mut h = Harness::new(pool);
    real_engine(&mut h);
    // The token ends in 2 minutes: less than 5, so refresh first.
    let account = oauth_account(&h, "g@example.com", Provider::Gmail, "old-token-refresh-test", "refresh-ok", 2).await;
    let job = h.queue(account, "lead@company.com").await;
    h.tick().await;

    let j = h.job(job).await;
    assert_eq!(j.status, JobStatus::Sent, "{:?}", j.last_error);
    assert_eq!(j.provider_message_id.as_deref(), Some("gmail-id-1"));
    assert_eq!(j.provider_thread_id.as_deref(), Some("thread-9"));
    assert!(lock(&seen).gmail_bodies.iter().any(|(t, _)| t == "fresh-token"));
    assert!(!lock(&seen).gmail_bodies.iter().any(|(t, _)| t == "old-token-refresh-test"), "the old token was never used");
    // The new token is saved, encrypted, with a new expiry.
    let row = h.account_row(account).await;
    assert_eq!(plain_token(&h, &row).0, "fresh-token");
    assert_eq!(row.token_expires_at, Some(h.now() + Duration::seconds(3600)));
    assert!(!String::from_utf8_lossy(&row.secret_ciphertext).contains("fresh-token"));
}

#[sqlx::test]
async fn a_good_token_is_not_refreshed(pool: PgPool) {
    let seen = mock();
    let mut h = Harness::new(pool);
    real_engine(&mut h);
    let account = oauth_account(&h, "g2@example.com", Provider::Gmail, "ok-token", "refresh-never-used", 30).await;
    let job = h.queue(account, "lead@company.com").await;
    h.tick().await;
    assert_eq!(h.job(job).await.status, JobStatus::Sent);
    assert!(!lock(&seen).token_calls.iter().any(|r| r == "refresh-never-used"), "no refresh for a good token");
}

#[sqlx::test]
async fn a_401_refreshes_once_and_tries_again(pool: PgPool) {
    let seen = mock();
    let mut h = Harness::new(pool);
    real_engine(&mut h);
    // The token looks fine, but Google says 401.
    let account = oauth_account(&h, "g3@example.com", Provider::Gmail, "stale-401-test", "refresh-ok", 30).await;
    let job = h.queue(account, "lead@company.com").await;
    h.tick().await;
    assert_eq!(h.job(job).await.status, JobStatus::Sent);
    let stale_tries = lock(&seen).gmail_bodies.iter().filter(|(t, _)| t == "stale-401-test").count();
    assert_eq!(stale_tries, 1, "the stale token was tried once, then replaced");
    assert_eq!(h.job(job).await.attempts, 1, "the second try is not a new attempt");
}

#[sqlx::test]
async fn invalid_grant_disconnects_the_account(pool: PgPool) {
    mock();
    let mut h = Harness::new(pool);
    real_engine(&mut h);
    let account = oauth_account(&h, "g4@example.com", Provider::Gmail, "old", "refresh-revoked", 1).await;
    let job = h.queue(account, "lead@company.com").await;
    h.tick().await;
    let j = h.job(job).await;
    assert_eq!((j.status, j.attempts), (JobStatus::Queued, 0));
    let row = h.account_row(account).await;
    assert_eq!(row.status, AccountStatus::Disconnected);
    assert!(row.last_error.unwrap().contains("Connect the account again"));
    let st = h.send_state(account).await;
    assert!(st.paused && st.paused_reason.unwrap().contains("no longer valid"));
}

#[sqlx::test]
async fn a_failed_refresh_without_invalid_grant_is_a_temporary_error(pool: PgPool) {
    mock();
    let mut h = Harness::new(pool);
    real_engine(&mut h);
    let account = oauth_account(&h, "g5@example.com", Provider::Gmail, "old", "refresh-down", 1).await;
    let job = h.queue(account, "lead@company.com").await;
    h.tick().await;
    let j = h.job(job).await;
    assert_eq!((j.status, j.attempts), (JobStatus::Queued, 1), "retried later");
    assert_eq!(h.account_row(account).await.status, AccountStatus::Active, "not disconnected");
    assert!(j.last_error.unwrap().contains("token refresh failed"));
}

#[sqlx::test]
async fn microsoft_account_saves_the_rotated_refresh_token(pool: PgPool) {
    let seen = mock();
    let mut h = Harness::new(pool);
    real_engine(&mut h);
    let account = oauth_account(&h, "m@example.com", Provider::Microsoft, "old", "refresh-rotating", 1).await;
    let job = h.queue(account, "lead@company.com").await;
    h.tick().await;
    let j = h.job(job).await;
    assert_eq!(j.status, JobStatus::Sent, "{:?}", j.last_error);
    assert_eq!(j.provider_message_id.as_deref(), Some("graph-id-1"));
    assert!(lock(&seen).graph_bodies.iter().any(|(t, ct, _)| t == "fresh-token" && ct == "text/plain"));
    let row = h.account_row(account).await;
    let (access, refresh) = plain_token(&h, &row);
    assert_eq!(access, "fresh-token");
    assert_eq!(refresh.as_deref(), Some("refresh-rotated-2"), "Microsoft's new refresh token is saved");
}

#[sqlx::test]
async fn a_rate_limit_from_the_provider_parks_the_account(pool: PgPool) {
    mock();
    let mut h = Harness::new(pool);
    real_engine(&mut h);
    let account = oauth_account(&h, "g6@example.com", Provider::Gmail, "limited-token", "refresh-ok", 30).await;
    let job = h.queue(account, "lead@company.com").await;
    h.tick().await;
    assert_eq!(h.job(job).await.status, JobStatus::Queued);
    assert_eq!(h.send_state(account).await.next_send_at, Some(h.now() + Duration::seconds(600)));
}

// ---------- the whole engine into Mailpit ----------

#[sqlx::test]
async fn five_emails_through_the_real_smtp_sender_into_mailpit(pool: PgPool) {
    if !mailpit_up() {
        eprintln!("SKIPPED: Mailpit is not running");
        return;
    }
    let mut h = Harness::new(pool);
    real_engine(&mut h);
    // A real SMTP account (localhost, no TLS): the secrets are decrypted and used.
    let account = h.account("engine-test@example.com").await;
    let tag = uuid::Uuid::now_v7().to_string();
    let mut jobs = Vec::new();
    for i in 0..5 {
        let mut j = new_job(account, &format!("lead{i}@company{i}.com"));
        j.subject = format!("Engine test {tag} #{i}");
        jobs.push(h.queue_job(j).await);
    }
    let mut sent_at = Vec::new();
    for _ in 0..200 {
        h.tick().await;
        h.advance(Duration::minutes(3));
        if h.job(*jobs.last().unwrap()).await.status == JobStatus::Sent {
            break;
        }
    }
    for id in &jobs {
        let j = h.job(*id).await;
        assert_eq!(j.status, JobStatus::Sent, "{:?}", j.last_error);
        sent_at.push(j.sent_at.unwrap());
    }
    let arrived: Vec<Value> = mailpit_messages().await.into_iter().filter(|m| m["Subject"].as_str().is_some_and(|s| s.contains(&tag))).collect();
    assert_eq!(arrived.len(), 5, "all five emails arrived in Mailpit");
    for w in sent_at.windows(2) {
        assert!((420..=900 + 180).contains(&(w[1] - w[0]).num_seconds()));
    }
}

// ---------- no secret in any log line ----------

#[derive(Clone, Default)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuffer {
    type Writer = LogBuffer;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[sqlx::test]
async fn no_token_password_or_key_ever_reaches_the_logs(pool: PgPool) {
    use aikido_email::auth::api_keys::Scope;
    let logs = LogBuffer::default();
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt().with_max_level(tracing::Level::TRACE).with_ansi(false).with_writer(logs.clone()).finish(),
    )
    .expect("this test binary sets the global logger once");
    mock();

    let mut h = Harness::new(pool);
    real_engine(&mut h);
    let keys = bootstrap_keys(&h.state).await;

    // OAuth account whose token is old: refresh, then send. And one whose token is rejected: 401, refresh, send.
    let a = oauth_account(&h, "log1@example.com", Provider::Gmail, "logcheck-access-OLD-SECRET", "logcheck-refresh-SECRET-1", 1).await;
    let b = oauth_account(&h, "log2@example.com", Provider::Gmail, "logcheck-access-STALE-SECRET", "logcheck-refresh-SECRET-2", 30).await;
    for account in [a, b] {
        h.queue(account, "lead@company.com").await;
    }
    h.tick().await;
    // A revoked refresh token: the error path.
    let c = oauth_account(&h, "log3@example.com", Provider::Gmail, "logcheck-access-REVOKED-SECRET", "refresh-revoked", 1).await;
    h.queue(c, "other@company.com").await;
    h.tick().await;

    // An SMTP account with a password: a failed login test, a stored account, a send.
    let bad = json!({ "email": "smtp@example.com", "smtp": { "host": "127.0.0.1", "port": 1, "tls": "none", "password": "logcheck-password-SECRET" } });
    let r = call(&h.state, "POST", "/v1/accounts/smtp", Some(&keys[&Scope::Accounts]), Some(bad)).await;
    assert_eq!(r.status, 422);
    let smtp = add_account(&h.state, "smtp2@example.com").await;
    h.queue(smtp, "third@company.com").await;
    h.tick().await;
    call(&h.state, "POST", &format!("/v1/accounts/{smtp}/test"), Some(&keys[&Scope::Accounts]), None).await;

    // Wrong keys and right keys.
    call(&h.state, "GET", "/v1/accounts", Some("aik_accounts_logcheck-WRONG-KEY"), None).await;
    call(&h.state, "GET", "/v1/keys", Some(&keys[&Scope::Sending]), None).await;
    call(&h.state, "GET", "/v1/keys", Some(&keys[&Scope::Admin]), None).await;
    call(&h.state, "GET", "/v1/accounts/oauth/gmail/callback?code=logcheck-CODE-SECRET&state=logcheck-STATE-SECRET", None, None).await;

    let log = String::from_utf8_lossy(&logs.0.lock().unwrap_or_else(|e| e.into_inner())).to_string();
    assert!(log.contains("email sent"), "the logger captured the engine: {log}");
    let mut secrets: Vec<String> = keys.values().cloned().collect();
    secrets.extend(
        ["logcheck-access-OLD-SECRET", "logcheck-access-STALE-SECRET", "logcheck-access-REVOKED-SECRET", "logcheck-refresh-SECRET-1",
         "logcheck-refresh-SECRET-2", "logcheck-password-SECRET", "super-secret-password", "aik_accounts_logcheck-WRONG-KEY",
         "logcheck-CODE-SECRET", "logcheck-STATE-SECRET", "fresh-token", "gsecret", "msecret"]
            .map(String::from),
    );
    for secret in secrets {
        assert!(!log.contains(&secret), "the secret {secret:?} is in the logs");
    }
}
