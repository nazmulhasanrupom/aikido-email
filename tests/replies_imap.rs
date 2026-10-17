//! Reply check against GreenMail (a local mail server with SMTP and IMAP):
//!   docker compose up -d greenmail
//! The tests skip themselves if GreenMail is not running.
mod common;
use aikido_email::accounts::model::*;
use aikido_email::accounts::repo;
use aikido_email::replies::fetchers::{RealFetchers, imap::{ImapAuth, ImapFetcher}};
use aikido_email::replies::sync::{ReplyEngine, sync_folder};
use aikido_email::replies::poller;
use aikido_email::sending::queue::JobStatus;
use aikido_email::sending::worker::{Engine, RealSenders};
use chrono::{DateTime, Duration, Utc};
use common::*;
use secrecy::SecretString;
use serde_json::json;
use sqlx::PgPool;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const SMTP_PORT: u16 = 3025;
const IMAP_PORT: u16 = 3143;

fn greenmail_up() -> bool {
    [SMTP_PORT, IMAP_PORT].iter().all(|p| {
        TcpStream::connect_timeout(&format!("127.0.0.1:{p}").parse().unwrap(), std::time::Duration::from_millis(300)).is_ok()
    })
}

macro_rules! need_greenmail {
    () => {
        if !greenmail_up() {
            eprintln!("GreenMail is not running (docker compose up -d greenmail): test skipped");
            return;
        }
    };
}

struct World {
    state: aikido_email::state::AppState,
    sender: Arc<Engine>,
    replies: Arc<ReplyEngine>,
    clock: Arc<Mutex<DateTime<Utc>>>,
    /// Our inbox.
    me: String,
    account: Uuid,
    suffix: String,
}

impl World {
    async fn new(pool: PgPool) -> World {
        let mut state = app_state(pool);
        let clock = Arc::new(Mutex::new(utc(2026, 10, 20, 12, 0)));
        let c = clock.clone();
        state.clock = Arc::new(move || *c.lock().unwrap());
        let suffix = Uuid::now_v7().simple().to_string()[20..].to_string();
        let me = format!("me-{suffix}@acme.test");
        let secrets = Secrets::Password(PasswordSecrets {
            smtp_username: Some(me.clone()),
            smtp_password: Some(SecretString::from("pw".to_string())),
            imap_username: Some(me.clone()),
            imap_password: Some(SecretString::from("pw".to_string())),
        });
        let new = repo::NewAccount {
            email: me.clone(),
            provider: Provider::Smtp,
            display_name: "Me".into(),
            smtp: Some(HostInfo { host: "127.0.0.1".into(), port: SMTP_PORT, tls: TlsMode::Plain }),
            imap: Some(HostInfo { host: "127.0.0.1".into(), port: IMAP_PORT, tls: TlsMode::Plain }),
            token_expires_at: None,
        };
        let account = repo::upsert_connected(&state.pool, &state.crypto, new, &secrets).await.unwrap().id;
        // One folder: GreenMail has no spam folder unless a test makes one.
        sqlx::query("UPDATE account_reply_settings SET settings = settings || '{\"folders\": [\"inbox\"]}'::jsonb WHERE account_id = $1")
            .bind(account).execute(&state.pool).await.unwrap();
        sqlx::query("UPDATE account_sending_settings SET settings = '{\"same_recipient_cooldown_hours\": 0, \"jitter_min_secs\": 0, \"jitter_max_secs\": 0, \"base_delay_secs\": 60}'::jsonb WHERE account_id = $1")
            .bind(account).execute(&state.pool).await.unwrap();
        let sender = Arc::new(Engine::new(state.clone(), Arc::new(RealSenders { dry_run: false })));
        let replies = Arc::new(ReplyEngine::new(state.clone(), Arc::new(RealFetchers)));
        World { state, sender, replies, clock, me, account, suffix }
    }

    fn now(&self) -> DateTime<Utc> {
        *self.clock.lock().unwrap()
    }
    fn advance(&self, d: Duration) {
        *self.clock.lock().unwrap() += d;
    }
    fn lead(&self, n: u32) -> String {
        format!("lead{n}-{}@prospect.test", self.suffix)
    }

    /// Send a real email through the sending engine to GreenMail. Returns the job.
    async fn send(&self, to: &str) -> aikido_email::sending::queue::Job {
        let mut j = new_job(self.account, to);
        j.subject = format!("Hello {to}");
        let items = aikido_email::sending::queue::enqueue(&self.state.pool, vec![j], self.now()).await.unwrap();
        let id = items[0].job.as_ref().unwrap().id;
        aikido_email::sending::scheduler::tick(&self.sender, 5).await.unwrap();
        let job = aikido_email::sending::queue::get_job(&self.state.pool, id).await.unwrap().unwrap();
        assert_eq!(job.status, JobStatus::Sent, "send failed: {:?}", job.last_error);
        self.advance(Duration::minutes(20));
        job
    }

    async fn poll(&self) -> poller::TickReport {
        sqlx::query("UPDATE inbox_sync_state SET next_sync_at = NULL WHERE account_id = $1").bind(self.account).execute(&self.state.pool).await.unwrap();
        poller::tick(&self.replies, 4).await.unwrap()
    }

    async fn count(&self, sql: &str) -> i64 {
        count(&self.state, sql).await
    }
}

/// Read one SMTP answer: up to the line that has a 3 digit code followed by a space.
fn smtp_read(s: &mut TcpStream) -> String {
    let mut out = String::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = s.read(&mut buf).unwrap();
        assert!(n > 0, "the SMTP server closed the connection: {out}");
        out.push_str(&String::from_utf8_lossy(&buf[..n]));
        if out.lines().last().is_some_and(|l| l.len() >= 4 && l.as_bytes()[..3].iter().all(u8::is_ascii_digit) && l.as_bytes()[3] == b' ') && out.ends_with('\n') {
            return out;
        }
    }
}

/// Talk to the SMTP server: send one line, return its answer.
fn smtp_line(s: &mut TcpStream, line: &str) -> String {
    s.write_all(line.as_bytes()).unwrap();
    smtp_read(s)
}

/// Deliver an email to GreenMail over SMTP.
fn deliver(from: &str, to: &str, subject: &str, in_reply_to: Option<&str>, body: &str) {
    let mut s = TcpStream::connect(("127.0.0.1", SMTP_PORT)).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    smtp_read(&mut s);
    smtp_line(&mut s, "HELO test\r\n");
    smtp_line(&mut s, &format!("MAIL FROM:<{from}>\r\n"));
    smtp_line(&mut s, &format!("RCPT TO:<{to}>\r\n"));
    smtp_line(&mut s, "DATA\r\n");
    let mut msg = format!("From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\nMessage-ID: <{}@prospect.test>\r\nDate: Tue, 20 Oct 2026 11:00:00 +0000\r\n", Uuid::now_v7());
    if let Some(id) = in_reply_to {
        msg.push_str(&format!("In-Reply-To: {id}\r\nReferences: {id}\r\n"));
    }
    msg.push_str(&format!("Content-Type: text/plain\r\n\r\n{body}\r\n.\r\n"));
    let r = smtp_line(&mut s, &msg);
    assert!(r.starts_with("250"), "SMTP DATA failed: {r}");
    smtp_line(&mut s, "QUIT\r\n");
}

/// A tiny IMAP client for the test: sends commands, returns everything the server said.
fn imap_session(user: &str, commands: &[&str]) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", IMAP_PORT)).unwrap();
    s.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
    let mut out = String::new();
    let mut buf = [0u8; 16384];
    let n = s.read(&mut buf).unwrap();
    out.push_str(&String::from_utf8_lossy(&buf[..n]));
    let mut all: Vec<String> = vec![format!("LOGIN \"{user}\" \"pw\"")];
    all.extend(commands.iter().map(|c| c.to_string()));
    all.push("LOGOUT".into());
    for (i, c) in all.iter().enumerate() {
        let tag = format!("t{i}");
        s.write_all(format!("{tag} {c}\r\n").as_bytes()).unwrap();
        loop {
            let n = s.read(&mut buf).unwrap_or(0);
            if n == 0 {
                break;
            }
            let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
            out.push_str(&chunk);
            if chunk.contains(&format!("{tag} OK")) || chunk.contains(&format!("{tag} NO")) || chunk.contains(&format!("{tag} BAD")) {
                break;
            }
        }
    }
    out
}

#[sqlx::test]
async fn a_lead_reply_in_greenmail_is_found_in_one_poll_and_the_follow_up_is_cancelled(pool: PgPool) {
    need_greenmail!();
    let w = World::new(pool).await;
    let lead = w.lead(1);
    let job = w.send(&lead).await;
    // The follow-up is queued and waits.
    let follow_up = queued_job(&w.state, w.account, &lead, "automated").await;
    // The lead answers from his own mailbox.
    deliver(&lead, &w.me, &format!("Re: Hello {lead}"), job.message_id_header.as_deref(), "Sounds good, call me tomorrow.");

    let r = w.poll().await;
    assert_eq!((r.claimed, r.synced, r.failed), (1, 1, 0), "{r:?}");
    let (label, method, text, matched): (String, String, Option<String>, Option<Uuid>) =
        sqlx::query_as("SELECT label::text, match_method::text, reply_text, matched_job_id FROM inbound_messages").fetch_one(&w.state.pool).await.unwrap();
    assert_eq!((label.as_str(), method.as_str()), ("reply", "in_reply_to"));
    assert_eq!(text.as_deref(), Some("Sounds good, call me tomorrow."));
    assert_eq!(matched, Some(job.id));
    assert_eq!(job_status(&w.state, follow_up).await, "cancelled");
    assert_eq!(suppression_reason(&w.state, &lead).await.as_deref(), Some("replied"));
}

#[sqlx::test]
async fn reading_never_marks_the_mail_as_read(pool: PgPool) {
    need_greenmail!();
    let w = World::new(pool).await;
    let lead = w.lead(1);
    let job = w.send(&lead).await;
    deliver(&lead, &w.me, "Re: Hello", job.message_id_header.as_deref(), "Yes.");
    deliver(&format!("news-{}@shop.test", w.suffix), &w.me, "Sale today", None, "Buy now");

    w.poll().await;
    assert_eq!(w.count("SELECT count(*) FROM inbound_messages").await, 1);

    let out = imap_session(&w.me, &["EXAMINE INBOX", "FETCH 1:* (FLAGS)"]);
    let flag_lines: Vec<&str> = out.lines().filter(|l| l.contains("FETCH (FLAGS")).collect();
    assert_eq!(flag_lines.len(), 2, "both mails are in the inbox: {out}");
    assert!(flag_lines.iter().all(|l| !l.contains("\\Seen")), "a mail was marked as read: {out}");
    // Control: a normal read (BODY[]) does mark the mail. This is what the reply check avoids.
    let out = imap_session(&w.me, &["SELECT INBOX", "FETCH 1 (BODY[])", "FETCH 1 (FLAGS)"]);
    assert!(out.lines().any(|l| l.contains("FETCH (FLAGS") && l.contains("\\Seen")), "control failed: GreenMail did not mark a normal read: {out}");
}

#[sqlx::test]
async fn an_unrelated_mail_in_the_inbox_is_not_stored(pool: PgPool) {
    need_greenmail!();
    let w = World::new(pool).await;
    w.send(&w.lead(1)).await;
    deliver(&format!("friend-{}@elsewhere.test", w.suffix), &w.me, "Lunch?", None, "Lunch tomorrow?");
    deliver(&format!("friend-{}@elsewhere.test", w.suffix), &w.me, "Unsubscribe me from your newsletter", None, "unsubscribe");
    let r = w.poll().await;
    assert_eq!(r.synced, 1);
    assert_eq!(w.count("SELECT count(*) FROM inbound_messages").await, 0);
    assert_eq!(w.count("SELECT count(*) FROM suppressions").await, 0);
}

#[sqlx::test]
async fn only_new_mail_is_read_on_the_next_poll(pool: PgPool) {
    need_greenmail!();
    let w = World::new(pool).await;
    let (l1, l2) = (w.lead(1), w.lead(2));
    let j1 = w.send(&l1).await;
    let j2 = w.send(&l2).await;
    deliver(&l1, &w.me, "Re: Hello", j1.message_id_header.as_deref(), "First reply");
    let r = sync_after_due(&w).await;
    assert_eq!((r.listed, r.downloaded, r.stored), (1, 1, 1));
    // Nothing new: `UID n:*` returns the last mail anyway; it must be dropped.
    let r = sync_after_due(&w).await;
    assert_eq!((r.listed, r.downloaded, r.stored), (0, 0, 0), "{r:?}");
    deliver(&l2, &w.me, "Re: Hello", j2.message_id_header.as_deref(), "Second reply");
    let r = sync_after_due(&w).await;
    assert_eq!((r.listed, r.downloaded, r.stored), (1, 1, 1), "{r:?}");
    assert_eq!(w.count("SELECT count(*) FROM inbound_messages").await, 2);
}

async fn sync_after_due(w: &World) -> aikido_email::replies::sync::SyncReport {
    let acct = load_account(&w.state, w.account).await;
    let mut conn = w.state.pool.acquire().await.unwrap();
    let settings = aikido_email::replies::repo::load_settings(&mut conn, w.account).await.unwrap();
    aikido_email::replies::repo::ensure_sync_rows(&mut conn, &acct, &settings).await.unwrap();
    drop(conn);
    sqlx::query("UPDATE inbox_sync_state SET next_sync_at = NULL WHERE account_id = $1").bind(w.account).execute(&w.state.pool).await.unwrap();
    sync_folder(&w.replies, w.account, "inbox").await.unwrap()
}

#[sqlx::test]
async fn a_uidvalidity_change_resyncs_without_double_actions(pool: PgPool) {
    need_greenmail!();
    let w = World::new(pool).await;
    let lead = w.lead(1);
    let job = w.send(&lead).await;
    queued_job(&w.state, w.account, &lead, "automated").await;
    deliver(&lead, &w.me, "Re: Hello", job.message_id_header.as_deref(), "Yes please.");
    w.poll().await;
    assert_eq!(w.count("SELECT count(*) FROM inbound_messages").await, 1);
    let cursor: serde_json::Value = sqlx::query_scalar("SELECT cursor FROM inbox_sync_state").fetch_one(&w.state.pool).await.unwrap();
    assert!(cursor["uidvalidity"].is_u64());

    // The server's UIDVALIDITY changed: the saved one no longer matches.
    sqlx::query("UPDATE inbox_sync_state SET cursor = jsonb_set(cursor, '{uidvalidity}', '12345')").execute(&w.state.pool).await.unwrap();
    let r = w.poll().await;
    assert_eq!(r.failed, 1, "CursorInvalid ends the run");
    assert_eq!(w.count("SELECT count(*) FROM inbox_sync_state WHERE cursor ? 'reset_since'").await, 1);
    let r = w.poll().await;
    assert_eq!(r.synced, 1);
    let cursor: serde_json::Value = sqlx::query_scalar("SELECT cursor FROM inbox_sync_state").fetch_one(&w.state.pool).await.unwrap();
    assert!(cursor["uidvalidity"].is_u64() && cursor["uidvalidity"] != json!(12345), "a fresh cursor");
    assert_eq!(w.count("SELECT count(*) FROM inbound_messages").await, 1);
    assert_eq!(w.count("SELECT count(*) FROM suppressions").await, 1);
    assert_eq!(w.count("SELECT count(*) FROM send_events WHERE event = 'replied'").await, 1);
}

#[sqlx::test]
async fn the_spam_folder_is_found_by_name_and_read(pool: PgPool) {
    need_greenmail!();
    let w = World::new(pool).await;
    sqlx::query("UPDATE account_reply_settings SET settings = settings || '{\"folders\": [\"inbox\", \"spam\"]}'::jsonb WHERE account_id = $1").bind(w.account).execute(&w.state.pool).await.unwrap();
    let lead = w.lead(1);
    let job = w.send(&lead).await;
    // The reply landed in the Junk folder.
    let msg = format!(
        "From: {lead}\r\nTo: {}\r\nSubject: Re: Hello\r\nMessage-ID: <junked-{}@prospect.test>\r\nIn-Reply-To: {}\r\n\r\nPlease unsubscribe me.\r\n",
        w.me, w.suffix, job.message_id_header.as_deref().unwrap()
    );
    let out = imap_session(&w.me, &["CREATE Junk", &format!("APPEND Junk {{{}}}\r\n{}", msg.len(), msg)]);
    assert!(out.contains("t1 OK") || out.contains("t1 NO"), "{out}");
    let r = w.poll().await;
    assert_eq!(r.failed, 0, "{r:?}");
    let (folder, label): (String, String) = sqlx::query_as("SELECT folder, label::text FROM inbound_messages").fetch_one(&w.state.pool).await.unwrap();
    assert_eq!((folder.as_str(), label.as_str()), ("spam", "unsubscribe"));
    assert_eq!(suppression_reason(&w.state, &lead).await.as_deref(), Some("unsubscribed"));
}

#[tokio::test]
async fn imap_without_encryption_is_refused_unless_the_host_is_localhost() {
    let auth = || ImapAuth::Password { username: "u".into(), password: SecretString::from("p".to_string()) };
    assert!(ImapFetcher::new("imap.example.com".into(), 143, TlsMode::Plain, auth()).is_err());
    assert!(ImapFetcher::new("127.0.0.1".into(), 3143, TlsMode::Plain, auth()).is_ok());
    assert!(ImapFetcher::new("localhost".into(), 143, TlsMode::Plain, auth()).is_ok());
    assert!(ImapFetcher::new("imap.example.com".into(), 993, TlsMode::Implicit, auth()).is_ok());
}
