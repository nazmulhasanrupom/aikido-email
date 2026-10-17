//! The Gmail and Microsoft readers against mock Gmail / Graph / token servers.
mod common;
use aikido_email::accounts::model::*;
use aikido_email::accounts::repo;
use aikido_email::replies::fetchers::RealFetchers;
use aikido_email::replies::sync::ReplyEngine;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use common::*;
use secrecy::SecretString;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use uuid::Uuid;

/// Now, to the second (the database keeps microseconds).
fn start() -> DateTime<Utc> {
    use chrono::Timelike;
    let n = Utc::now();
    n - Duration::nanoseconds(n.nanosecond() as i64)
}

const ME: &str = "sender@acme-outreach.example";

#[derive(Clone)]
struct MockMail {
    /// Which test owns it. Tests share one server and look only at their own mail.
    owner: String,
    id: String,
    thread: String,
    received: DateTime<Utc>,
    labels: Vec<String>,
    raw: Vec<u8>,
}

#[derive(Default)]
struct Mock {
    mails: Vec<MockMail>,
    /// (owner, method, path, query)
    requests: Vec<(String, String, String, String)>,
    token_calls: Vec<String>,
    /// Ids that answer 404 (deleted since the listing).
    gone: Vec<String>,
}

type Shared = Arc<Mutex<Mock>>;

fn lock(m: &Shared) -> MutexGuard<'_, Mock> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The token says who owns the request: `<owner>:<behavior>`.
fn who(h: &HeaderMap) -> (String, String) {
    let t = h.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).unwrap_or("").trim_start_matches("Bearer ").to_string();
    let (owner, behavior) = t.split_once(':').unwrap_or((&t, "good"));
    (owner.to_string(), behavior.to_string())
}

fn record(m: &Shared, owner: &str, method: &str, path: &str, q: &HashMap<String, String>) {
    let mut qs: Vec<String> = q.iter().map(|(k, v)| format!("{k}={v}")).collect();
    qs.sort();
    lock(m).requests.push((owner.into(), method.into(), path.into(), qs.join("&")));
}

fn json_ok(v: Value) -> axum::response::Response {
    (StatusCode::OK, [(header::CONTENT_TYPE, "application/json")], v.to_string()).into_response()
}

fn api_error(behavior: &str) -> Option<axum::response::Response> {
    match behavior {
        "noscope" => Some((StatusCode::FORBIDDEN, [(header::CONTENT_TYPE, "application/json")],
            r#"{"error":{"code":403,"message":"Request had insufficient authentication scopes.","status":"PERMISSION_DENIED"}}"#).into_response()),
        "ratelimit" => Some((StatusCode::TOO_MANY_REQUESTS, [(header::RETRY_AFTER, "17")], "{}").into_response()),
        "stale" => Some((StatusCode::UNAUTHORIZED, [(header::CONTENT_TYPE, "application/json")], r#"{"error":{"message":"Invalid Credentials"}}"#).into_response()),
        _ => None,
    }
}

// ----- Gmail -----

async fn gmail_list(State(m): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> axum::response::Response {
    let (owner, behavior) = who(&headers);
    record(&m, &owner, "GET", "/gmail/messages", &q);
    if let Some(e) = api_error(&behavior) {
        return e;
    }
    let query = q.get("q").cloned().unwrap_or_default();
    let num = |key: &str| query.split_whitespace().find_map(|w| w.strip_prefix(key)).and_then(|v| v.parse::<i64>().ok());
    let (after, before) = (num("after:").unwrap_or(0), num("before:"));
    let spam_too = q.get("includeSpamTrash").is_some_and(|v| v == "true");
    let mut found: Vec<MockMail> = lock(&m).mails.iter().filter(|x| x.owner == owner).cloned().collect();
    found.retain(|x| {
        let t = x.received.timestamp();
        t > after && before.is_none_or(|b| t < b)
            && !(query.contains("-in:sent") && x.labels.iter().any(|l| l == "SENT"))
            && !(query.contains("-in:drafts") && x.labels.iter().any(|l| l == "DRAFT"))
            && (spam_too || !x.labels.iter().any(|l| l == "SPAM"))
    });
    found.sort_by_key(|x| std::cmp::Reverse(x.received));
    // Two per page, to test paging.
    let start: usize = q.get("pageToken").and_then(|t| t.parse().ok()).unwrap_or(0);
    let page: Vec<Value> = found.iter().skip(start).take(2).map(|x| json!({ "id": x.id, "threadId": x.thread })).collect();
    let mut out = json!({ "messages": page, "resultSizeEstimate": found.len() });
    if start + 2 < found.len() {
        out["nextPageToken"] = json!((start + 2).to_string());
    }
    json_ok(out)
}

async fn gmail_get(State(m): State<Shared>, Path(id): Path<String>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> axum::response::Response {
    let (owner, behavior) = who(&headers);
    record(&m, &owner, "GET", &format!("/gmail/messages/{id}"), &q);
    if let Some(e) = api_error(&behavior) {
        return e;
    }
    let mail = {
        let g = lock(&m);
        if g.gone.contains(&id) {
            return (StatusCode::NOT_FOUND, "{}").into_response();
        }
        g.mails.iter().find(|x| x.owner == owner && x.id == id).cloned()
    };
    let Some(mail) = mail else { return (StatusCode::NOT_FOUND, "{}").into_response() };
    match q.get("format").map(String::as_str) {
        Some("raw") => json_ok(json!({ "id": mail.id, "threadId": mail.thread, "raw": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&mail.raw) })),
        _ => {
            let parsed = aikido_email::replies::parse::parse_header_block(&mail.raw);
            let wanted = ["from", "to", "subject", "message-id", "in-reply-to", "references", "content-type", "auto-submitted", "x-autoreply", "x-autorespond", "precedence", "date"];
            let hs: Vec<Value> = parsed.iter().filter(|(k, _)| wanted.contains(&k.as_str())).map(|(k, v)| json!({ "name": k, "value": v })).collect();
            json_ok(json!({
                "id": mail.id, "threadId": mail.thread, "labelIds": mail.labels,
                "internalDate": (mail.received.timestamp() * 1000).to_string(), "sizeEstimate": mail.raw.len(),
                "payload": { "headers": hs }
            }))
        }
    }
}

// ----- Microsoft Graph -----

async fn graph_list(State(m): State<Shared>, Path(folder): Path<String>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> axum::response::Response {
    let (owner, behavior) = who(&headers);
    record(&m, &owner, "GET", &format!("/graph/me/mailFolders/{folder}/messages"), &q);
    if let Some(e) = api_error(&behavior) {
        return e;
    }
    let filter = q.get("$filter").cloned().unwrap_or_default();
    let from = filter.strip_prefix("receivedDateTime ge ").and_then(|t| DateTime::parse_from_rfc3339(t).ok()).map(|t| t.with_timezone(&Utc)).unwrap_or(DateTime::UNIX_EPOCH);
    let wanted_label = if folder == "junkemail" { "JUNK" } else { "INBOX" };
    let mut found: Vec<MockMail> = lock(&m).mails.iter().filter(|x| x.owner == owner && x.received >= from && x.labels.iter().any(|l| l == wanted_label)).cloned().collect();
    found.sort_by_key(|x| x.received);
    // Two per page and a nextLink, to test paging.
    let skip: usize = q.get("skip").and_then(|t| t.parse().ok()).unwrap_or(0);
    let with_headers = behavior != "noheaders";
    let page: Vec<Value> = found.iter().skip(skip).take(2).map(|x| graph_item(x, with_headers)).collect();
    let mut out = json!({ "value": page });
    if skip + 2 < found.len() {
        let base = std::env::var("MS_GRAPH_BASE_URL").unwrap();
        let mut link = oauth2::url::Url::parse(&format!("{base}/me/mailFolders/{folder}/messages")).unwrap();
        link.query_pairs_mut().append_pair("$filter", &filter).append_pair("skip", &(skip + 2).to_string());
        out["@odata.nextLink"] = json!(link.to_string());
    }
    json_ok(out)
}

fn graph_item(x: &MockMail, with_headers: bool) -> Value {
    let parsed = aikido_email::replies::parse::parse_header_block(&x.raw);
    let mut item = json!({
        "id": x.id, "receivedDateTime": x.received.to_rfc3339_opts(chrono::SecondsFormat::Secs, true), "conversationId": x.thread,
        "subject": parsed.get("subject"),
    });
    if with_headers {
        item["internetMessageHeaders"] = Value::Array(parsed.iter().map(|(k, v)| json!({ "name": k, "value": v })).collect());
    }
    item
}

async fn graph_message(State(m): State<Shared>, Path(id): Path<String>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> axum::response::Response {
    let (owner, behavior) = who(&headers);
    record(&m, &owner, "GET", &format!("/graph/me/messages/{id}"), &q);
    if let Some(e) = api_error(&behavior) {
        return e;
    }
    let mail = lock(&m).mails.iter().find(|x| x.owner == owner && x.id == id).cloned();
    match mail {
        Some(x) => json_ok(graph_item(&x, true)),
        None => (StatusCode::NOT_FOUND, r#"{"error":{"code":"ErrorItemNotFound","message":"gone"}}"#).into_response(),
    }
}

async fn graph_value(State(m): State<Shared>, Path(id): Path<String>, headers: HeaderMap) -> axum::response::Response {
    let (owner, _) = who(&headers);
    record(&m, &owner, "GET", &format!("/graph/me/messages/{id}/$value"), &HashMap::new());
    let mail = lock(&m).mails.iter().find(|x| x.owner == owner && x.id == id).cloned();
    match mail {
        Some(x) => (StatusCode::OK, [(header::CONTENT_TYPE, "message/rfc822")], x.raw).into_response(),
        None => (StatusCode::NOT_FOUND, r#"{"error":{"code":"ErrorItemNotFound"}}"#).into_response(),
    }
}

/// A method other than GET on a mailbox is a bug: it could change the mail.
async fn graph_write(State(m): State<Shared>, headers: HeaderMap) -> axum::response::Response {
    let (owner, _) = who(&headers);
    record(&m, &owner, "WRITE", "?", &HashMap::new());
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}

async fn token_endpoint(State(m): State<Shared>, body: String) -> axum::response::Response {
    let refresh = oauth2::url::form_urlencoded::parse(body.as_bytes()).find(|(k, _)| k == "refresh_token").map(|(_, v)| v.into_owned()).unwrap_or_default();
    lock(&m).token_calls.push(refresh.clone());
    // "<owner>:refresh-ok" gives an access token of that owner.
    match refresh.split_once(':') {
        Some((owner, "refresh-ok")) => json_ok(json!({ "access_token": format!("{owner}:good"), "token_type": "Bearer", "expires_in": 3600 })),
        Some((_, "refresh-revoked")) => (StatusCode::BAD_REQUEST, [(header::CONTENT_TYPE, "application/json")], r#"{"error":"invalid_grant"}"#).into_response(),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response(),
    }
}

fn mock() -> Shared {
    static MOCK: OnceLock<Shared> = OnceLock::new();
    MOCK.get_or_init(|| {
        let shared: Shared = Arc::default();
        let app = Router::new()
            .route("/gmail/messages", get(gmail_list))
            .route("/gmail/messages/{id}", get(gmail_get))
            .route("/graph/me/mailFolders/{folder}/messages", get(graph_list).post(graph_write).patch(graph_write).delete(graph_write))
            .route("/graph/me/messages/{id}", get(graph_message).patch(graph_write).delete(graph_write))
            .route("/graph/me/messages/{id}/$value", get(graph_value))
            .route("/token", post(token_endpoint))
            .with_state(shared.clone());
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
            std::env::set_var("GMAIL_API_BASE_URL", format!("http://{addr}/gmail"));
            std::env::set_var("MS_CLIENT_ID", "mid");
            std::env::set_var("MS_CLIENT_SECRET", "msecret");
            std::env::set_var("MS_TOKEN_URL", format!("http://{addr}/token"));
            std::env::set_var("MS_GRAPH_BASE_URL", format!("http://{addr}/graph"));
        }
        shared
    })
    .clone()
}

// ----- the tests -----

struct T {
    h: ReplyHarness,
    owner: String,
    mock: Shared,
    account: Uuid,
}

fn real_engine(h: &mut ReplyHarness) {
    h.engine = Arc::new(ReplyEngine::new(h.state.clone(), Arc::new(RealFetchers)));
}

impl T {
    /// An OAuth account. `behavior` is how the mock API answers this token.
    async fn new(pool: PgPool, provider: Provider, behavior: &str, scope: &str, refresh: &str, expires_in: Duration) -> T {
        let mock = mock();
        let mut h = ReplyHarness::at(pool, start());
        real_engine(&mut h);
        let owner = format!("t{}", Uuid::now_v7().simple());
        let secrets = Secrets::OAuth(OAuthSecrets {
            access_token: SecretString::from(format!("{owner}:{behavior}")),
            refresh_token: Some(SecretString::from(format!("{owner}:{refresh}"))),
            scope: Some(scope.into()),
        });
        let new = repo::NewAccount {
            email: ME.into(), provider, display_name: String::new(), smtp: None, imap: None,
            token_expires_at: Some(Utc::now() + expires_in),
        };
        let account = repo::upsert_connected(&h.state.pool, &h.state.crypto, new, &secrets).await.unwrap().id;
        sqlx::query("UPDATE account_reply_settings SET settings = settings || '{\"folders\": [\"inbox\"]}'::jsonb WHERE account_id = $1")
            .bind(account).execute(&h.state.pool).await.unwrap();
        T { h, owner, mock, account }
    }

    async fn gmail(pool: PgPool, behavior: &str) -> T {
        Self::new(pool, Provider::Gmail, behavior, "https://www.googleapis.com/auth/gmail.readonly https://www.googleapis.com/auth/gmail.send", "refresh-ok", Duration::hours(1)).await
    }
    async fn microsoft(pool: PgPool, behavior: &str) -> T {
        Self::new(pool, Provider::Microsoft, behavior, "https://graph.microsoft.com/Mail.Read https://graph.microsoft.com/Mail.Send offline_access", "refresh-ok", Duration::hours(1)).await
    }

    fn add(&self, id: &str, labels: &[&str], age: Duration, raw: Vec<u8>) {
        lock(&self.mock).mails.push(MockMail {
            owner: self.owner.clone(), id: id.into(), thread: format!("thread-{id}"),
            received: self.h.now() - age, labels: labels.iter().map(|l| l.to_string()).collect(), raw,
        });
    }

    fn requests(&self) -> Vec<(String, String, String)> {
        lock(&self.mock).requests.iter().filter(|r| r.0 == self.owner).map(|r| (r.1.clone(), r.2.clone(), r.3.clone())).collect()
    }

    async fn send(&self, i: usize) {
        sent_job(&self.h.state, self.account, &format!("lead{i}@company{i}.example"), &format!("<s{i}@acme-outreach.example>"), None, self.h.now() - Duration::days(1)).await;
    }

    fn reply(&self, i: usize) -> Vec<u8> {
        reply_eml(&format!("Lead {i} <lead{i}@company{i}.example>"), &format!("<s{i}@acme-outreach.example>"), "Re: Hello", "Yes, interested.").into_bytes()
    }

    async fn poll(&self) -> aikido_email::replies::poller::TickReport {
        self.h.tick().await
    }
}

#[sqlx::test]
async fn gmail_a_reply_is_found_in_one_poll_and_only_candidates_are_downloaded(pool: PgPool) {
    let t = T::gmail(pool, "good").await;
    t.send(0).await;
    t.send(1).await;
    t.add("m1", &["INBOX"], Duration::hours(3), t.reply(0));
    t.add("m2", &["INBOX"], Duration::hours(2), b"From: Shop <news@shop.example>\nSubject: Sale\nMessage-ID: <n1@shop.example>\n\nbuy\n".to_vec());
    t.add("m3", &["SENT"], Duration::hours(2), b"From: sender@acme-outreach.example\nTo: lead0@company0.example\nSubject: Hello\nMessage-ID: <s0@acme-outreach.example>\n\nHi\n".to_vec());
    t.add("m4", &["INBOX"], Duration::hours(1), t.reply(1));
    t.add("m5", &["INBOX"], Duration::hours(1), b"From: Friend <friend@elsewhere.example>\nSubject: Lunch\nMessage-ID: <l1@elsewhere.example>\n\nlunch?\n".to_vec());

    let r = t.poll().await;
    assert_eq!((r.synced, r.failed), (1, 0), "{r:?}");
    assert_eq!(count(&t.h.state, "SELECT count(*) FROM inbound_messages WHERE label = 'reply'").await, 2);
    assert_eq!(count(&t.h.state, "SELECT count(*) FROM suppressions").await, 2);

    let reqs = t.requests();
    assert!(reqs.iter().all(|(m, _, _)| m == "GET"), "read only: {reqs:?}");
    let raw_downloads: Vec<_> = reqs.iter().filter(|(_, _, q)| q.contains("format=raw")).collect();
    assert_eq!(raw_downloads.len(), 2, "only the two replies are downloaded in full: {reqs:?}");
    let list = reqs.iter().find(|(_, p, _)| p == "/gmail/messages").unwrap();
    assert!(list.2.contains("-in%3Asent") || list.2.contains("-in:sent"), "{list:?}");
    assert!(!list.2.contains("includeSpamTrash"), "spam folder not wanted");
    // The metadata call asks only for the headers we need.
    assert!(reqs.iter().any(|(_, _, q)| q.contains("format=metadata") && q.contains("metadataHeaders")));
    // One sync row for Gmail: `all`.
    let (folder,): (String,) = sqlx::query_as("SELECT folder FROM inbox_sync_state").fetch_one(&t.h.state.pool).await.unwrap();
    assert_eq!(folder, "all");
    // Paging (2 per page) worked: all 4 non-sent mails were looked at.
    assert_eq!(reqs.iter().filter(|(_, p, q)| p.starts_with("/gmail/messages/") && q.contains("metadata")).count(), 4);
}

#[sqlx::test]
async fn gmail_reads_the_spam_folder_when_it_is_wanted(pool: PgPool) {
    let t = T::gmail(pool, "good").await;
    t.send(0).await;
    t.add("spam1", &["SPAM"], Duration::hours(1), t.reply(0));
    t.poll().await;
    assert_eq!(count(&t.h.state, "SELECT count(*) FROM inbound_messages").await, 0, "inbox only: the spam reply is not seen");

    t.h.reply_settings(t.account, json!({ "folders": ["inbox", "spam"] })).await;
    t.h.make_due(t.account).await;
    sqlx::query("UPDATE inbox_sync_state SET cursor = NULL").execute(&t.h.state.pool).await.unwrap();
    t.poll().await;
    assert_eq!(count(&t.h.state, "SELECT count(*) FROM inbound_messages WHERE label = 'reply'").await, 1);
    let reqs = t.requests();
    assert!(reqs.iter().any(|(_, p, q)| p == "/gmail/messages" && q.contains("includeSpamTrash=true")));
}

#[sqlx::test]
async fn gmail_a_catch_up_is_read_in_slices_of_one_day(pool: PgPool) {
    let t = T::gmail(pool, "good").await;
    t.send(0).await;
    t.send(1).await;
    t.send(2).await;
    t.add("old", &["INBOX"], Duration::days(5) + Duration::hours(1), t.reply(0));
    t.add("mid", &["INBOX"], Duration::days(2) + Duration::hours(1), t.reply(1));
    t.add("new", &["INBOX"], Duration::hours(1), t.reply(2));
    t.h.reply_settings(t.account, json!({ "initial_lookback_days": 7 })).await;
    t.poll().await;
    assert_eq!(count(&t.h.state, "SELECT count(*) FROM inbound_messages WHERE label = 'reply'").await, 3);
    let lists = t.requests().into_iter().filter(|(_, p, _)| p == "/gmail/messages").count();
    assert!(lists >= 7, "7 days = 7 slices, plus paging: got {lists}");
    // The saved cursor is the listing time minus 10 minutes of overlap.
    let cursor: Value = sqlx::query_scalar("SELECT cursor FROM inbox_sync_state").fetch_one(&t.h.state.pool).await.unwrap();
    let after = cursor["after"].as_i64().unwrap();
    let expect = Utc::now().timestamp() - 600;
    assert!((after - expect).abs() < 30, "cursor {after} vs {expect}");
}

#[sqlx::test]
async fn gmail_a_message_deleted_after_the_listing_is_skipped(pool: PgPool) {
    let t = T::gmail(pool, "good").await;
    t.send(0).await;
    t.send(1).await;
    t.add("gone", &["INBOX"], Duration::hours(2), t.reply(0));
    t.add("here", &["INBOX"], Duration::hours(1), t.reply(1));
    lock(&t.mock).gone.push("gone".into());
    let r = t.poll().await;
    assert_eq!((r.synced, r.failed), (1, 0));
    assert_eq!(count(&t.h.state, "SELECT count(*) FROM inbound_messages").await, 1);
}

#[sqlx::test]
async fn gmail_a_mail_over_the_cap_is_read_by_its_headers_only(pool: PgPool) {
    let mut state = app_state(pool);
    let mut cfg = (*state.config).clone();
    cfg.max_message_bytes = 3_000;
    state.config = Arc::new(cfg);
    let mock = mock();
    let mut h = ReplyHarness::with_state(state, start());
    real_engine(&mut h);
    let owner = format!("t{}", Uuid::now_v7().simple());
    let secrets = Secrets::OAuth(OAuthSecrets {
        access_token: SecretString::from(format!("{owner}:good")), refresh_token: None,
        scope: Some("https://www.googleapis.com/auth/gmail.readonly".into()),
    });
    let new = repo::NewAccount { email: ME.into(), provider: Provider::Gmail, display_name: String::new(), smtp: None, imap: None, token_expires_at: Some(Utc::now() + Duration::hours(1)) };
    let account = repo::upsert_connected(&h.state.pool, &h.state.crypto, new, &secrets).await.unwrap().id;
    sent_job(&h.state, account, "lead0@company0.example", "<s0@acme-outreach.example>", None, h.now() - Duration::days(1)).await;
    let big = reply_eml("Lead <lead0@company0.example>", "<s0@acme-outreach.example>", "Re: Hello", &"word ".repeat(4_000));
    lock(&mock).mails.push(MockMail { owner, id: "big".into(), thread: "t".into(), received: h.now() - Duration::hours(1), labels: vec!["INBOX".into()], raw: big.into_bytes() });
    h.tick().await;
    let (label, truncated): (String, bool) = sqlx::query_as("SELECT label::text, truncated FROM inbound_messages").fetch_one(&h.state.pool).await.unwrap();
    assert_eq!((label.as_str(), truncated), ("reply", true));
}

#[sqlx::test]
async fn gmail_errors_are_mapped(pool: PgPool) {
    // No read scope at the provider: reading is off, sending is not touched.
    let t = T::gmail(pool.clone(), "noscope").await;
    t.send(0).await;
    t.poll().await;
    let a = load_account(&t.h.state, t.account).await;
    assert!(!a.can_read);
    assert_eq!(a.read_status.as_deref(), Some("reconnect_needed"));
    assert_eq!(a.status, AccountStatus::Active);
}

#[sqlx::test]
async fn gmail_rate_limit_waits_for_retry_after(pool: PgPool) {
    let t = T::gmail(pool, "ratelimit").await;
    t.send(0).await;
    let r = t.poll().await;
    assert_eq!(r.failed, 1);
    let row = t.h.sync_row(t.account, "all").await;
    assert_eq!(row.next_sync_at, Some(t.h.now() + Duration::seconds(17)));
    assert_eq!(row.consecutive_errors, 0);
}

#[sqlx::test]
async fn gmail_a_401_refreshes_the_token_once_and_goes_on(pool: PgPool) {
    let t = T::gmail(pool, "stale").await;
    t.send(0).await;
    t.add("m1", &["INBOX"], Duration::hours(1), t.reply(0));
    let r = t.poll().await;
    assert_eq!((r.synced, r.failed), (1, 0), "{r:?}");
    assert_eq!(count(&t.h.state, "SELECT count(*) FROM inbound_messages").await, 1);
    let calls: Vec<String> = lock(&t.mock).token_calls.iter().filter(|c| c.starts_with(&t.owner)).cloned().collect();
    assert_eq!(calls.len(), 1, "one refresh: {calls:?}");
    // The new token was saved (encrypted).
    let secrets = repo::load_secrets(&t.h.state.crypto, &load_account(&t.h.state, t.account).await).unwrap();
    let Secrets::OAuth(o) = secrets else { panic!() };
    use secrecy::ExposeSecret;
    assert_eq!(o.access_token.expose_secret(), &format!("{}:good", t.owner));
}

#[sqlx::test]
async fn an_expired_token_is_refreshed_before_reading(pool: PgPool) {
    let t = T::new(pool, Provider::Gmail, "stale", "https://www.googleapis.com/auth/gmail.readonly", "refresh-ok", Duration::minutes(2)).await;
    t.send(0).await;
    t.add("m1", &["INBOX"], Duration::hours(1), t.reply(0));
    let r = t.poll().await;
    assert_eq!(r.synced, 1);
    let calls = lock(&t.mock).token_calls.iter().filter(|c| c.starts_with(&t.owner)).count();
    assert_eq!(calls, 1, "ends in less than 5 minutes: refreshed first, and only once");
}

#[sqlx::test]
async fn a_revoked_token_disconnects_the_account_and_stops_sending_and_syncing(pool: PgPool) {
    let t = T::new(pool, Provider::Gmail, "good", "https://www.googleapis.com/auth/gmail.readonly", "refresh-revoked", Duration::minutes(-5)).await;
    t.send(0).await;
    let queued = queued_job(&t.h.state, t.account, "lead0@company0.example", "automated").await;
    let r = t.poll().await;
    assert_eq!(r.failed, 1);
    let a = load_account(&t.h.state, t.account).await;
    assert_eq!(a.status, AccountStatus::Disconnected);
    assert!(a.last_error.unwrap().contains("Connect the account again"));
    let (paused,): (bool,) = sqlx::query_as("SELECT paused FROM account_send_state WHERE account_id = $1").bind(t.account).fetch_one(&t.h.state.pool).await.unwrap();
    assert!(paused, "sending stops");
    t.h.advance(Duration::hours(3));
    assert_eq!(t.poll().await.claimed, 0, "syncing stops");
    assert_eq!(job_status(&t.h.state, queued).await, "queued");
    // No mail API call was made with the dead login.
    assert!(t.requests().is_empty());
}

#[sqlx::test]
async fn microsoft_a_reply_is_found_and_the_mailbox_is_only_read(pool: PgPool) {
    let t = T::microsoft(pool, "good").await;
    for i in 0..3 {
        t.send(i).await;
    }
    t.add("g1", &["INBOX"], Duration::hours(4), t.reply(0));
    t.add("g2", &["INBOX"], Duration::hours(3), b"From: Shop <news@shop.example>\nSubject: Sale\nMessage-ID: <n1@shop.example>\n\nbuy\n".to_vec());
    t.add("g3", &["INBOX"], Duration::hours(2), t.reply(1));
    t.add("g4", &["JUNK"], Duration::hours(1), t.reply(2));
    t.h.reply_settings(t.account, json!({ "folders": ["inbox", "spam"] })).await;

    let r = t.poll().await;
    assert_eq!((r.claimed, r.synced, r.failed), (2, 2, 0), "{r:?}");
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT folder, label::text FROM inbound_messages ORDER BY received_at").fetch_all(&t.h.state.pool).await.unwrap();
    assert_eq!(rows, [("inbox".into(), "reply".into()), ("inbox".into(), "reply".into()), ("spam".into(), "reply".into())]);

    let reqs = t.requests();
    assert!(reqs.iter().all(|(m, _, _)| m == "GET"), "never PATCH, POST or DELETE: {reqs:?}");
    // The mail itself is downloaded only for the 3 replies, with `$value`.
    assert_eq!(reqs.iter().filter(|(_, p, _)| p.ends_with("/$value")).count(), 3);
    // Well-known folder names, oldest first, with the filter.
    let lists: Vec<_> = reqs.iter().filter(|(_, p, _)| p.contains("/mailFolders/")).collect();
    assert!(lists.iter().any(|(_, p, _)| p.contains("/mailFolders/inbox/")));
    assert!(lists.iter().any(|(_, p, _)| p.contains("/mailFolders/junkemail/")));
    // (The second page is the `nextLink` the server gave, which has its own query.)
    assert!(lists.iter().filter(|(_, _, q)| !q.contains("skip=")).all(|(_, _, q)| q.contains("$orderby=receivedDateTime asc") && q.contains("$filter=receivedDateTime ge ")), "{lists:?}");
    // The headers came with the list: no per-message header calls.
    assert!(reqs.iter().all(|(_, p, _)| !(p.starts_with("/graph/me/messages/") && !p.ends_with("/$value"))), "{reqs:?}");
}

#[sqlx::test]
async fn microsoft_asks_for_the_headers_per_message_when_the_list_has_none(pool: PgPool) {
    let t = T::microsoft(pool, "noheaders").await;
    t.send(0).await;
    t.add("g1", &["INBOX"], Duration::hours(1), t.reply(0));
    t.add("g2", &["INBOX"], Duration::hours(1) + Duration::minutes(5), b"From: Shop <news@shop.example>\nSubject: Sale\nMessage-ID: <n1@shop.example>\n\nbuy\n".to_vec());
    // The mock's per-message answer always has headers; the list had none.
    let r = t.poll().await;
    assert_eq!(r.synced, 1);
    assert_eq!(count(&t.h.state, "SELECT count(*) FROM inbound_messages").await, 1);
    let per_message = t.requests().into_iter().filter(|(_, p, _)| p.starts_with("/graph/me/messages/") && !p.ends_with("/$value")).count();
    assert_eq!(per_message, 2);
}

#[sqlx::test]
async fn microsoft_errors_are_mapped(pool: PgPool) {
    let t = T::microsoft(pool.clone(), "ratelimit").await;
    t.send(0).await;
    t.poll().await;
    assert_eq!(t.h.sync_row(t.account, "inbox").await.next_sync_at, Some(t.h.now() + Duration::seconds(17)));

    let t = T::microsoft(pool, "stale").await;
    t.send(0).await;
    t.add("g1", &["INBOX"], Duration::hours(1), t.reply(0));
    let r = t.poll().await;
    assert_eq!((r.synced, r.failed), (1, 0), "a 401 refreshes the token once, then it works");
}
