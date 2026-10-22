#![allow(dead_code)]
pub mod gharness;
use aikido_email::accounts::model::*;
use aikido_email::accounts::repo;
use aikido_email::auth::api_keys::{self, Scope};
use aikido_email::config::Config;
use aikido_email::crypto::Crypto;
use aikido_email::sending::queue::{self, Job, NewJob, StateRow};
use aikido_email::sending::scheduler::{self, TickReport};
use aikido_email::sending::senders::fake::FakeSender;
use aikido_email::sending::worker::{Engine, FakeSenders};
use aikido_email::state::AppState;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Duration, TimeZone, Utc};
use secrecy::SecretString;
use serde_json::Value;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use uuid::Uuid;

pub fn utc(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
}

pub fn test_config(env_api_keys: HashMap<Scope, String>) -> Config {
    Config {
        database_url: String::new(),
        bind_addr: "127.0.0.1:0".into(),
        public_url: Some("http://localhost:8080".into()),
        data_dir: std::env::temp_dir().join(format!("aikido-test-{}", Uuid::now_v7())),
        encryption_key: None,
        scheduler_tick_secs: 15,
        max_parallel_accounts: 20,
        dry_run: false,
        reply_tick_secs: 15,
        max_parallel_syncs: 10,
        max_message_bytes: 10_485_760,
        reply_check_enabled: false,
        followups_enabled: false,
        leads_enabled: false,
        generator_enabled: false,
        insecure_cookies: false,
        trust_proxy: false,
        tracking: Default::default(),
        dev_tools: true,
        env_api_keys,
    }
}

pub fn app_state(pool: PgPool) -> AppState {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut state = AppState::new(pool, Crypto::from_key_bytes(&[42u8; 32]).unwrap(), test_config(HashMap::new()));
    // No real DNS in tests: every domain has a mail server, unless a test says otherwise.
    state.mx = Arc::new(aikido_email::leads::verify::fake::FakeMx::new(aikido_email::leads::verify::MxAnswer::Has));
    state
}

/// Make one key per scope and return them by scope. The server makes no keys by itself.
pub async fn bootstrap_keys(state: &AppState) -> HashMap<Scope, String> {
    let mut keys = HashMap::new();
    for &scope in Scope::ALL {
        let (_, key) = api_keys::create(&state.pool, scope, "test").await.unwrap();
        keys.insert(scope, key);
    }
    keys
}

pub struct Response {
    pub status: StatusCode,
    pub body: Value,
}

pub async fn call(state: &AppState, method: &str, uri: &str, key: Option<&str>, body: Option<Value>) -> Response {
    let app = aikido_email::docs::app(state.clone());
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(k) = key {
        req = req.header("authorization", format!("Bearer {k}"));
    }
    let req = match body {
        Some(b) => req.header("content-type", "application/json").body(Body::from(b.to_string())).unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 10 * 1024 * 1024).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Response { status, body }
}

pub struct Full {
    pub status: StatusCode,
    pub headers: axum::http::HeaderMap,
    pub body: Value,
}

/// Like `call`, with any headers (cookies, CSRF token, Origin) and the response headers back.
pub async fn call_with(state: &AppState, method: &str, uri: &str, headers: &[(&str, &str)], body: Option<Value>) -> Full {
    let app = aikido_email::docs::app(state.clone());
    let mut req = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let req = match body {
        Some(b) => req.header("content-type", "application/json").body(Body::from(b.to_string())).unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let resp = app.oneshot(req).await.unwrap();
    let (parts, body) = resp.into_parts();
    let bytes = axum::body::to_bytes(body, 10 * 1024 * 1024).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Full { status: parts.status, headers: parts.headers, body }
}

/// A logged-in web user in a test.
#[derive(Clone)]
pub struct Web {
    pub cookie: String,
    pub csrf: String,
}

pub const ORIGIN: &str = "http://localhost:8080";

impl Web {
    /// Call as this user: cookie, CSRF token and Origin, like the browser does.
    pub async fn call(&self, state: &AppState, method: &str, uri: &str, body: Option<Value>) -> Full {
        let headers = [("cookie", self.cookie.as_str()), ("x-csrf-token", self.csrf.as_str()), ("origin", ORIGIN)];
        call_with(state, method, uri, &headers, body).await
    }
}

/// Read the session cookie out of a login answer.
pub fn web_from(resp: &Full) -> Web {
    let set = resp.headers.get("set-cookie").expect("a Set-Cookie header").to_str().unwrap();
    let cookie = set.split(';').next().unwrap().to_string();
    Web { cookie, csrf: resp.body["csrf_token"].as_str().expect("a csrf_token").to_string() }
}

pub const STRONG: &str = "correct horse battery staple";

/// A clock the test moves, shared by all copies of the server in the test.
pub fn shared_clock(state: &mut AppState, start: DateTime<Utc>) -> Arc<Mutex<DateTime<Utc>>> {
    let clock = Arc::new(Mutex::new(start));
    let c = clock.clone();
    state.clock = Arc::new(move || *c.lock().unwrap());
    clock
}

/// First boot: set up the owner, return the owner's session.
pub async fn setup_owner(state: &AppState) -> Web {
    let body = serde_json::json!({
        "name": "Olivia Owner", "email": "owner@example.com", "password": STRONG, "workspace_name": "Acme"
    });
    let resp = call_with(state, "POST", "/v1/auth/setup", &[("origin", ORIGIN)], Some(body)).await;
    assert_eq!(resp.status, StatusCode::CREATED, "{:?}", resp.body);
    web_from(&resp)
}

/// An invite for `email`, accepted. Returns the new user's session.
pub async fn add_user(state: &AppState, owner: &Web, email: &str, role: &str) -> Web {
    let r = owner.call(state, "POST", "/v1/users/invites", Some(serde_json::json!({ "email": email, "role": role }))).await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
    let token = r.body["link"].as_str().unwrap().rsplit('/').next().unwrap().to_string();
    let body = serde_json::json!({ "name": "Some Person", "password": "purple monkey dishwasher lamp" });
    let a = call_with(state, "POST", &format!("/v1/auth/invites/{token}/accept"), &[("origin", ORIGIN)], Some(body)).await;
    assert_eq!(a.status, StatusCode::CREATED, "{:?}", a.body);
    web_from(&a)
}

/// A multipart form. Each part: (name, file name, bytes).
pub async fn call_form(state: &AppState, uri: &str, key: Option<&str>, parts: &[(&str, Option<&str>, &[u8])]) -> Response {
    let app = aikido_email::docs::app(state.clone());
    let boundary = "----aikido-test-boundary";
    let mut body: Vec<u8> = Vec::new();
    for (name, file, data) in parts {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        match file {
            Some(f) => body.extend_from_slice(format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{f}\"\r\nContent-Type: text/csv\r\n\r\n").as_bytes()),
            None => body.extend_from_slice(format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes()),
        }
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    let mut req = Request::builder().method("POST").uri(uri).header("content-type", format!("multipart/form-data; boundary={boundary}"));
    if let Some(k) = key {
        req = req.header("authorization", format!("Bearer {k}"));
    }
    let resp = app.oneshot(req.body(Body::from(body)).unwrap()).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024 * 1024).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Response { status, body }
}

/// The raw body of a GET (for the CSV export).
pub async fn call_raw(state: &AppState, uri: &str, key: Option<&str>) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let app = aikido_email::docs::app(state.clone());
    let mut req = Request::builder().method("GET").uri(uri);
    if let Some(k) = key {
        req = req.header("authorization", format!("Bearer {k}"));
    }
    let resp = app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap();
    let (parts, body) = resp.into_parts();
    let bytes = axum::body::to_bytes(body, 512 * 1024 * 1024).await.unwrap();
    (parts.status, parts.headers, bytes.to_vec())
}

/// An SMTP account saved directly in the store (no login test).
pub async fn add_account(state: &AppState, email: &str) -> Uuid {
    let secrets = Secrets::Password(PasswordSecrets {
        smtp_username: Some(email.into()),
        smtp_password: Some(SecretString::from("super-secret-password".to_string())),
        imap_username: None,
        imap_password: None,
    });
    let new = repo::NewAccount {
        email: email.into(),
        provider: Provider::Smtp,
        display_name: "Test Sender".into(),
        smtp: Some(HostInfo { host: "localhost".into(), port: 1025, tls: TlsMode::Plain }),
        imap: None,
        token_expires_at: None,
    };
    repo::upsert_connected(&state.pool, &state.crypto, new, &secrets).await.unwrap().id
}

pub fn new_job(account_id: Uuid, to: &str) -> NewJob {
    NewJob {
        account_id,
        to_email: to.into(),
        to_name: None,
        subject: format!("Hello {to}"),
        body_text: Some("Hi there".into()),
        body_html: None,
        headers: None,
        in_reply_to: None,
        references: None,
        provider_thread_id: None,
        send_after: None,
        priority: None,
        idempotency_key: None,
        lead_id: None,
        campaign_id: None,
        metadata: None,
        kind: None,
    }
}

/// The engine with a `FakeSender` and a clock that only moves when the test says so.
pub struct Harness {
    pub state: AppState,
    pub engine: Arc<Engine>,
    pub sender: FakeSender,
    clock: Arc<Mutex<DateTime<Utc>>>,
}

impl Harness {
    /// Starts on Monday 2026-10-19 10:00 UTC: a send day, inside the default window.
    pub fn new(pool: PgPool) -> Self {
        Self::at(pool, utc(2026, 10, 19, 10, 0))
    }

    pub fn at(pool: PgPool, start: DateTime<Utc>) -> Self {
        let mut state = app_state(pool);
        let sender = FakeSender::new();
        let clock = Arc::new(Mutex::new(start));
        let c = clock.clone();
        state.clock = Arc::new(move || *c.lock().unwrap());
        state.senders = Arc::new(FakeSenders(sender.clone()));
        let engine = Engine::new(state.clone(), Arc::new(FakeSenders(sender.clone())));
        Self { state, engine: Arc::new(engine), sender, clock }
    }

    pub fn now(&self) -> DateTime<Utc> {
        *self.clock.lock().unwrap()
    }
    pub fn set_time(&self, t: DateTime<Utc>) {
        *self.clock.lock().unwrap() = t;
    }
    pub fn advance(&self, d: Duration) {
        *self.clock.lock().unwrap() += d;
    }

    pub async fn tick(&self) -> TickReport {
        scheduler::tick(&self.engine, 20).await.unwrap()
    }

    pub async fn account(&self, email: &str) -> Uuid {
        add_account(&self.state, email).await
    }

    pub async fn queue(&self, account: Uuid, to: &str) -> Uuid {
        self.queue_job(new_job(account, to)).await
    }

    pub async fn queue_job(&self, job: NewJob) -> Uuid {
        let items = queue::enqueue(&self.state.pool, vec![job], self.now()).await.unwrap();
        items[0].job.as_ref().expect("job queued").id
    }

    pub async fn job(&self, id: Uuid) -> Job {
        queue::get_job(&self.state.pool, id).await.unwrap().unwrap()
    }

    pub async fn send_state(&self, account: Uuid) -> StateRow {
        sqlx::query_as(&format!("SELECT {} FROM account_send_state WHERE account_id = $1", queue::STATE_COLS))
            .bind(account)
            .fetch_one(&self.state.pool)
            .await
            .unwrap()
    }

    pub async fn account_row(&self, account: Uuid) -> EmailAccount {
        sqlx::query_as(&format!("SELECT {ACCOUNT_COLS} FROM email_accounts WHERE id = $1"))
            .bind(account)
            .fetch_one(&self.state.pool)
            .await
            .unwrap()
    }

    pub async fn events(&self, job: Uuid) -> Vec<String> {
        queue::job_events(&self.state.pool, job).await.unwrap().into_iter().map(|e| e.event).collect()
    }

    /// Patch the sending settings of one account.
    pub async fn settings(&self, account: Uuid, patch: Value) {
        let mut conn = self.state.pool.acquire().await.unwrap();
        let current = queue::load_settings(&mut conn, account).await.unwrap();
        let merged = current.merged(&patch).unwrap();
        merged.validate().unwrap();
        sqlx::query("UPDATE account_sending_settings SET settings = $2 WHERE account_id = $1")
            .bind(account)
            .bind(serde_json::to_value(&merged).unwrap())
            .execute(&mut *conn)
            .await
            .unwrap();
    }
}

/// A job that was already sent (no engine run). `message_id` is the header the lead's reply will point to.
pub async fn sent_job(
    st: &AppState,
    account: Uuid,
    to: &str,
    message_id: &str,
    thread_id: Option<&str>,
    sent_at: DateTime<Utc>,
) -> Uuid {
    let id = Uuid::now_v7();
    let domain = to.rsplit('@').next().unwrap();
    sqlx::query(
        "INSERT INTO send_jobs (id, account_id, to_email, recipient_domain, subject, body_text, status, message_id_header,
                provider_thread_id, sent_at, created_at, updated_at)
         VALUES ($1, $2, $3::citext, $4, 'Hello', 'Hi', 'sent', $5, $6, $7, $7, $7)",
    )
    .bind(id)
    .bind(account)
    .bind(to)
    .bind(domain)
    .bind(message_id)
    .bind(thread_id)
    .bind(sent_at)
    .execute(&st.pool)
    .await
    .unwrap();
    id
}

pub fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/fixtures/inbound/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap_or_else(|e| panic!("{name}: {e}"))
}

pub async fn load_account(st: &AppState, id: Uuid) -> EmailAccount {
    repo::get(&st.pool, id).await.unwrap().unwrap()
}

/// Run one raw email through the reply pipeline, like the sync does.
pub async fn ingest_raw(
    st: &AppState,
    account: Uuid,
    provider_id: &str,
    raw: &[u8],
    now: DateTime<Utc>,
) -> aikido_email::replies::ingest::Ingested {
    use aikido_email::replies::{ingest, repo as rrepo};
    let acct = load_account(st, account).await;
    let mut conn = st.pool.acquire().await.unwrap();
    let settings = rrepo::load_settings(&mut conn, account).await.unwrap();
    let exempt = queue::load_settings(&mut conn, account).await.unwrap().per_domain_exempt;
    drop(conn);
    ingest::ingest(
        &st.pool,
        &ingest::Inbound {
            account: &acct,
            folder: "inbox",
            provider_message_id: provider_id,
            provider_thread_id: None,
            raw,
            truncated: false,
            headers: None,
            received_at: Some(now),
        },
        &settings,
        &exempt,
        now,
    )
    .await
    .unwrap()
}

/// A plain reply to one of our emails.
pub fn reply_eml(from: &str, in_reply_to: &str, subject: &str, body: &str) -> String {
    format!(
        "From: {from}\nTo: sender@acme-outreach.example\nSubject: {subject}\nMessage-ID: <r-{}@prospect.example>\n\
         In-Reply-To: {in_reply_to}\nReferences: {in_reply_to}\nDate: Tue, 20 Oct 2026 09:00:00 +0000\n\
         Content-Type: text/plain; charset=UTF-8\n\n{body}\n",
        Uuid::now_v7()
    )
}

/// A bounce report for one of our emails.
pub fn dsn_eml(orig_message_id: &str, recipient: &str, status: &str, action: &str) -> String {
    format!(
        "From: Mail Delivery Subsystem <mailer-daemon@googlemail.com>\nTo: sender@acme-outreach.example\n\
         Subject: Delivery Status Notification (Failure)\nMessage-ID: <b-{}@bounce.example>\nDate: Tue, 20 Oct 2026 09:00:00 +0000\n\
         MIME-Version: 1.0\nContent-Type: multipart/report; report-type=delivery-status; boundary=\"B1\"\n\n\
         --B1\nContent-Type: text/plain\n\nDelivery failed.\n\n\
         --B1\nContent-Type: message/delivery-status\n\nReporting-MTA: dns; googlemail.com\n\n\
         Final-Recipient: rfc822; {recipient}\nAction: {action}\nStatus: {status}\nDiagnostic-Code: smtp; 550 test\n\n\
         --B1\nContent-Type: message/rfc822\n\nMessage-ID: {orig_message_id}\nFrom: sender@acme-outreach.example\nTo: {recipient}\nSubject: Hello\n\n\
         --B1--\n",
        Uuid::now_v7()
    )
}

pub async fn count(st: &AppState, sql: &str) -> i64 {
    let (n,): (i64,) = sqlx::query_as(sql).fetch_one(&st.pool).await.unwrap();
    n
}

pub async fn queued_job(st: &AppState, account: Uuid, to: &str, kind: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO send_jobs (id, account_id, to_email, recipient_domain, subject, body_text, status, kind)
         VALUES ($1, $2, $3::citext, $4, 'Follow-up', 'Hi again', 'queued', $5::job_kind)",
    )
    .bind(id)
    .bind(account)
    .bind(to)
    .bind(to.rsplit('@').next().unwrap())
    .bind(kind)
    .execute(&st.pool)
    .await
    .unwrap();
    id
}

pub async fn job_status(st: &AppState, id: Uuid) -> String {
    let (s,): (String,) = sqlx::query_as("SELECT status::text FROM send_jobs WHERE id = $1").bind(id).fetch_one(&st.pool).await.unwrap();
    s
}

pub async fn suppression_reason(st: &AppState, value: &str) -> Option<String> {
    let r: Option<(String,)> = sqlx::query_as("SELECT reason::text FROM suppressions WHERE value = $1::citext").bind(value).fetch_optional(&st.pool).await.unwrap();
    r.map(|r| r.0)
}

// ---------- reply check harness ----------

use aikido_email::replies::fetchers::fake::FakeFetcher;
use aikido_email::replies::fetchers::FakeFetchers;
use aikido_email::replies::poller::{self, TickReport as ReplyTickReport};
use aikido_email::replies::sync::ReplyEngine;

/// The reply engine with a `FakeFetcher` and a clock that only moves when the test says so.
pub struct ReplyHarness {
    pub state: AppState,
    pub engine: Arc<ReplyEngine>,
    pub fetcher: FakeFetcher,
    clock: Arc<Mutex<DateTime<Utc>>>,
}

impl ReplyHarness {
    /// Starts on Tuesday 2026-10-20 12:00 UTC.
    pub fn new(pool: PgPool) -> Self {
        Self::at(pool, utc(2026, 10, 20, 12, 0))
    }

    pub fn at(pool: PgPool, start: DateTime<Utc>) -> Self {
        Self::with_state(app_state(pool), start)
    }

    pub fn with_state(mut state: AppState, start: DateTime<Utc>) -> Self {
        let fetcher = FakeFetcher::new();
        let clock = Arc::new(Mutex::new(start));
        let c = clock.clone();
        state.clock = Arc::new(move || *c.lock().unwrap());
        let engine = ReplyEngine::new(state.clone(), Arc::new(FakeFetchers(fetcher.clone())));
        Self { state, engine: Arc::new(engine), fetcher, clock }
    }

    pub fn now(&self) -> DateTime<Utc> {
        *self.clock.lock().unwrap()
    }
    pub fn advance(&self, d: Duration) {
        *self.clock.lock().unwrap() += d;
    }

    pub async fn tick(&self) -> ReplyTickReport {
        poller::tick(&self.engine, 10).await.unwrap()
    }

    /// An account that can be read (the fake fetcher needs no login).
    pub async fn account(&self, email: &str) -> Uuid {
        let id = add_account(&self.state, email).await;
        sqlx::query("UPDATE email_accounts SET can_read = true, read_status = 'ok' WHERE id = $1").bind(id).execute(&self.state.pool).await.unwrap();
        // One folder keeps the numbers in the tests simple. Tests for the spam folder turn it back on.
        sqlx::query("UPDATE account_reply_settings SET settings = settings || '{\"folders\": [\"inbox\"]}'::jsonb WHERE account_id = $1")
            .bind(id).execute(&self.state.pool).await.unwrap();
        id
    }

    /// A Gmail account row (OAuth), readable.
    pub async fn gmail_account(&self, email: &str) -> Uuid {
        let secrets = Secrets::OAuth(OAuthSecrets {
            access_token: SecretString::from("tok".to_string()),
            refresh_token: Some(SecretString::from("ref".to_string())),
            scope: Some("https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/gmail.send".into()),
        });
        let new = repo::NewAccount {
            email: email.into(), provider: Provider::Gmail, display_name: String::new(), smtp: None, imap: None,
            token_expires_at: Some(Utc::now() + Duration::hours(1)),
        };
        repo::upsert_connected(&self.state.pool, &self.state.crypto, new, &secrets).await.unwrap().id
    }

    pub async fn sync_row(&self, account: Uuid, folder: &str) -> aikido_email::replies::repo::SyncState {
        aikido_email::replies::repo::load_sync_state(&self.state.pool, account, folder).await.unwrap().expect("sync row")
    }

    pub async fn reply_settings(&self, account: Uuid, patch: Value) {
        let mut conn = self.state.pool.acquire().await.unwrap();
        let cur = aikido_email::replies::repo::load_settings(&mut conn, account).await.unwrap();
        let merged = cur.merged(&patch).unwrap();
        merged.validate().unwrap();
        sqlx::query("UPDATE account_reply_settings SET settings = $2 WHERE account_id = $1")
            .bind(account).bind(serde_json::to_value(merged).unwrap()).execute(&mut *conn).await.unwrap();
    }

    /// Make the sync due now (as if the timer ran out).
    pub async fn make_due(&self, account: Uuid) {
        sqlx::query("UPDATE inbox_sync_state SET next_sync_at = NULL WHERE account_id = $1").bind(account).execute(&self.state.pool).await.unwrap();
    }
}
