mod common;
use aikido_email::accounts::model::AccountStatus;
use aikido_email::replies::fetchers::{FetchError, Folder};
use aikido_email::replies::repo;
use chrono::Duration;
use common::*;
use serde_json::json;
use sqlx::PgPool;
use std::time::Duration as StdDuration;

const ME: &str = "sender@acme-outreach.example";

/// A reply to `<s{i}@acme-outreach.example>` from `lead{i}@company{i}.example`.
fn lead_reply(i: usize) -> Vec<u8> {
    reply_eml(&format!("Lead {i} <lead{i}@company{i}.example>"), &format!("<s{i}@acme-outreach.example>"), "Re: Hello", "Yes, interested.").into_bytes()
}

/// A send to lead `i`, one day ago.
async fn send_to_lead(h: &ReplyHarness, account: uuid::Uuid, i: usize) -> uuid::Uuid {
    sent_job(&h.state, account, &format!("lead{i}@company{i}.example"), &format!("<s{i}@acme-outreach.example>"), None, h.now() - Duration::days(1)).await
}

#[sqlx::test]
async fn the_first_sync_downloads_only_mail_that_may_link_to_our_sends(pool: PgPool) {
    let h = ReplyHarness::new(pool);
    let a = h.account(ME).await;
    let job = send_to_lead(&h, a, 0).await;
    let t = h.now() - Duration::hours(2);
    h.fetcher.add(Folder::Inbox, lead_reply(0), t);
    h.fetcher.add(Folder::Inbox, b"From: Friend <friend@elsewhere.example>\nTo: sender@acme-outreach.example\nSubject: Lunch?\nMessage-ID: <l1@elsewhere.example>\n\nLunch?\n".to_vec(), t + Duration::minutes(1));
    h.fetcher.add(Folder::Inbox, b"From: Shop <news@shop.example>\nSubject: Sale\nMessage-ID: <n1@shop.example>\n\nbig sale\n".to_vec(), t + Duration::minutes(2));
    // Older than the 14 day lookback: never listed.
    h.fetcher.add(Folder::Inbox, lead_reply(0), h.now() - Duration::days(20));

    let r = h.tick().await;
    assert_eq!((r.claimed, r.synced, r.failed), (1, 1, 0));
    assert_eq!(h.fetcher.raw_calls(), 1, "only the reply is downloaded, not the other mail");
    assert_eq!(count(&h.state, "SELECT count(*) FROM inbound_messages").await, 1);
    let (label, matched): (String, Option<uuid::Uuid>) = sqlx::query_as("SELECT label::text, matched_job_id FROM inbound_messages").fetch_one(&h.state.pool).await.unwrap();
    assert_eq!((label.as_str(), matched), ("reply", Some(job)));

    let row = h.sync_row(a, "inbox").await;
    assert_eq!(row.last_success_at, Some(h.now()));
    assert_eq!(row.next_sync_at, Some(h.now() + Duration::seconds(120)));
    assert_eq!(row.consecutive_errors, 0);
    assert!(row.sync_started_at.is_none());
    assert!(row.cursor.is_some());
}

#[sqlx::test]
async fn a_sync_is_not_due_until_its_interval_has_passed(pool: PgPool) {
    let h = ReplyHarness::new(pool);
    let a = h.account(ME).await;
    send_to_lead(&h, a, 0).await;
    assert_eq!(h.tick().await.claimed, 1);
    assert_eq!(h.tick().await.claimed, 0);
    h.advance(Duration::seconds(119));
    assert_eq!(h.tick().await.claimed, 0);
    h.advance(Duration::seconds(2));
    assert_eq!(h.tick().await.claimed, 1);
    // A new reply is read on the next run, not before.
    h.fetcher.add(Folder::Inbox, lead_reply(0), h.now());
    assert_eq!(count(&h.state, "SELECT count(*) FROM inbound_messages").await, 0);
    h.advance(Duration::seconds(121));
    h.tick().await;
    assert_eq!(count(&h.state, "SELECT count(*) FROM inbound_messages").await, 1);
}

#[sqlx::test]
async fn the_same_email_synced_twice_gives_one_row_one_suppression_one_event(pool: PgPool) {
    let h = ReplyHarness::new(pool);
    let a = h.account(ME).await;
    send_to_lead(&h, a, 0).await;
    queued_job(&h.state, a, "lead0@company0.example", "automated").await;
    h.fetcher.add(Folder::Inbox, lead_reply(0), h.now() - Duration::hours(1));
    h.tick().await;
    assert_eq!(count(&h.state, "SELECT count(*) FROM inbound_messages").await, 1);

    // Reset the cursor: the same mail is listed again.
    sqlx::query("UPDATE inbox_sync_state SET cursor = NULL, next_sync_at = NULL").execute(&h.state.pool).await.unwrap();
    let before = h.fetcher.raw_calls();
    let r = h.tick().await;
    assert_eq!(r.synced, 1);
    assert_eq!(h.fetcher.raw_calls(), before, "mail that is already stored is not downloaded again");
    assert_eq!(count(&h.state, "SELECT count(*) FROM inbound_messages").await, 1);
    assert_eq!(count(&h.state, "SELECT count(*) FROM suppressions").await, 1);
    assert_eq!(count(&h.state, "SELECT count(*) FROM send_events WHERE event = 'replied'").await, 1);
    assert_eq!(count(&h.state, "SELECT count(*) FROM send_events WHERE event = 'cancelled'").await, 1);
}

#[sqlx::test]
async fn a_reply_cancels_the_queued_follow_up_in_one_poll(pool: PgPool) {
    let h = ReplyHarness::new(pool);
    let a = h.account(ME).await;
    send_to_lead(&h, a, 0).await;
    let follow_up = queued_job(&h.state, a, "lead0@company0.example", "automated").await;
    h.fetcher.add(Folder::Inbox, lead_reply(0), h.now() - Duration::minutes(5));
    h.tick().await;
    assert_eq!(job_status(&h.state, follow_up).await, "cancelled");
    assert_eq!(suppression_reason(&h.state, "lead0@company0.example").await.as_deref(), Some("replied"));
}

#[sqlx::test]
async fn spam_folder_mail_is_read_too_and_the_folder_can_be_turned_off(pool: PgPool) {
    let h = ReplyHarness::new(pool);
    let a = h.account(ME).await;
    send_to_lead(&h, a, 0).await;
    send_to_lead(&h, a, 1).await;
    h.reply_settings(a, json!({ "folders": ["inbox", "spam"] })).await;
    h.fetcher.add(Folder::Spam, lead_reply(0), h.now() - Duration::hours(1));
    h.tick().await;
    let (folder,): (String,) = sqlx::query_as("SELECT folder FROM inbound_messages").fetch_one(&h.state.pool).await.unwrap();
    assert_eq!(folder, "spam");
    assert_eq!(count(&h.state, "SELECT count(*) FROM inbox_sync_state").await, 2);

    // Only the inbox from now on: the spam row goes away once the settings are applied.
    h.reply_settings(a, json!({ "folders": ["inbox"] })).await;
    let acct = load_account(&h.state, a).await;
    let mut conn = h.state.pool.acquire().await.unwrap();
    let s = repo::load_settings(&mut conn, a).await.unwrap();
    repo::ensure_sync_rows(&mut conn, &acct, &s).await.unwrap();
    assert_eq!(count(&h.state, "SELECT count(*) FROM inbox_sync_state").await, 1);
}

#[sqlx::test]
async fn accounts_that_must_not_be_read_are_skipped(pool: PgPool) {
    let h = ReplyHarness::new(pool);
    // No recent send: nothing to match.
    let quiet = h.account("quiet@acme-outreach.example").await;
    sent_job(&h.state, quiet, "x@y.example", "<q@acme-outreach.example>", None, h.now() - Duration::days(31)).await;
    // Cannot read.
    let blind = h.account("blind@acme-outreach.example").await;
    sqlx::query("UPDATE email_accounts SET can_read = false WHERE id = $1").bind(blind).execute(&h.state.pool).await.unwrap();
    send_to_lead(&h, blind, 1).await;
    // Disconnected.
    let gone = h.account("gone@acme-outreach.example").await;
    send_to_lead(&h, gone, 2).await;
    sqlx::query("UPDATE email_accounts SET status = 'disconnected' WHERE id = $1").bind(gone).execute(&h.state.pool).await.unwrap();
    // Reply check turned off for this account.
    let off = h.account("off@acme-outreach.example").await;
    send_to_lead(&h, off, 3).await;
    h.reply_settings(off, json!({ "enabled": false })).await;
    // Folder paused.
    let paused_folder = h.account("pf@acme-outreach.example").await;
    send_to_lead(&h, paused_folder, 4).await;
    // The one that should run: paused for SENDING only.
    let ok = h.account("ok@acme-outreach.example").await;
    send_to_lead(&h, ok, 5).await;
    sqlx::query("UPDATE account_send_state SET paused = true, paused_reason = 'bounce_rate' WHERE account_id = $1").bind(ok).execute(&h.state.pool).await.unwrap();
    sqlx::query("UPDATE email_accounts SET status = 'error' WHERE id = $1").bind(ok).execute(&h.state.pool).await.unwrap();

    // First tick makes the sync rows; mark the paused one.
    h.tick().await;
    sqlx::query("UPDATE inbox_sync_state SET paused = true, next_sync_at = NULL WHERE account_id = $1").bind(paused_folder).execute(&h.state.pool).await.unwrap();
    sqlx::query("UPDATE inbox_sync_state SET next_sync_at = NULL").execute(&h.state.pool).await.unwrap();
    let claimed = repo::claim_due(&h.state.pool, h.now(), 100).await.unwrap();
    let accounts: Vec<_> = claimed.iter().map(|c| c.0).collect();
    assert!(accounts.contains(&ok), "an account paused for sending is still read: replies keep coming");
    for skipped in [quiet, blind, gone, off, paused_folder] {
        assert!(!accounts.contains(&skipped), "{skipped} should not be claimed");
    }
}

#[sqlx::test]
async fn the_global_pause_stops_every_poll(pool: PgPool) {
    let h = ReplyHarness::new(pool);
    let a = h.account(ME).await;
    send_to_lead(&h, a, 0).await;
    sqlx::query("UPDATE reply_defaults SET engine_paused = true").execute(&h.state.pool).await.unwrap();
    let r = h.tick().await;
    assert!(r.paused);
    assert_eq!(h.fetcher.list_calls(), 0);
    sqlx::query("UPDATE reply_defaults SET engine_paused = false").execute(&h.state.pool).await.unwrap();
    assert_eq!(h.tick().await.synced, 1);
}

#[sqlx::test]
async fn the_download_budget_splits_a_big_inbox_over_several_ticks(pool: PgPool) {
    let h = ReplyHarness::new(pool);
    let a = h.account(ME).await;
    h.reply_settings(a, json!({ "max_messages_per_sync": 10 })).await;
    for i in 0..25 {
        send_to_lead(&h, a, i).await;
        // Oldest first: lead 0 wrote 5 hours ago.
        h.fetcher.add(Folder::Inbox, lead_reply(i), h.now() - Duration::hours(5) + Duration::minutes(i as i64));
    }
    h.tick().await;
    assert_eq!(count(&h.state, "SELECT count(*) FROM inbound_messages").await, 10);
    let row = h.sync_row(a, "inbox").await;
    assert_eq!(row.next_sync_at, Some(h.now()), "more is left: the next tick continues at once");

    h.tick().await;
    assert_eq!(count(&h.state, "SELECT count(*) FROM inbound_messages").await, 20);
    h.tick().await;
    assert_eq!(count(&h.state, "SELECT count(*) FROM inbound_messages").await, 25);
    assert_eq!(count(&h.state, "SELECT count(DISTINCT matched_job_id) FROM inbound_messages").await, 25);
    assert_eq!(count(&h.state, "SELECT count(*) FROM suppressions").await, 25);
    assert_eq!(count(&h.state, "SELECT count(*) FROM send_events WHERE event = 'replied'").await, 25);
    // Now it waits for the interval.
    assert_eq!(h.sync_row(a, "inbox").await.next_sync_at, Some(h.now() + Duration::seconds(120)));
}

#[sqlx::test]
async fn a_parse_error_is_stored_as_unknown_and_the_batch_goes_on(pool: PgPool) {
    let h = ReplyHarness::new(pool);
    let a = h.account(ME).await;
    send_to_lead(&h, a, 0).await;
    send_to_lead(&h, a, 1).await;
    let t = h.now() - Duration::hours(3);
    // Points to our send but has no From address: it cannot be read.
    h.fetcher.add(Folder::Inbox, b"To: sender@acme-outreach.example\nSubject: Re: Hello\nMessage-ID: <broken@x>\nIn-Reply-To: <s0@acme-outreach.example>\n\nbody\n".to_vec(), t);
    h.fetcher.add(Folder::Inbox, lead_reply(1), t + Duration::minutes(1));
    h.tick().await;
    let rows: Vec<(String, Option<String>)> = sqlx::query_as("SELECT label::text, label_reason FROM inbound_messages ORDER BY received_at").fetch_all(&h.state.pool).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "unknown");
    assert!(rows[0].1.as_deref().unwrap().contains("parse error"));
    assert_eq!(rows[1].0, "reply");
    assert_eq!(count(&h.state, "SELECT count(*) FROM suppressions").await, 1, "no actions for the unreadable one");
    assert_eq!(count(&h.state, "SELECT count(*) FROM send_events WHERE event = 'inbound_unknown'").await, 1);
}

#[sqlx::test]
async fn a_mail_over_the_size_cap_is_classified_from_its_headers_only(pool: PgPool) {
    let mut state = app_state(pool);
    let mut cfg = (*state.config).clone();
    cfg.max_message_bytes = 2_000;
    state.config = std::sync::Arc::new(cfg);
    let h = ReplyHarness::with_state(state, utc(2026, 10, 20, 12, 0));
    let a = h.account(ME).await;
    send_to_lead(&h, a, 0).await;
    let big = reply_eml("Lead <lead0@company0.example>", "<s0@acme-outreach.example>", "Re: Hello", &"x ".repeat(5_000));
    h.fetcher.add(Folder::Inbox, big.into_bytes(), h.now() - Duration::hours(1));
    h.tick().await;
    let (label, truncated, text): (String, bool, Option<String>) =
        sqlx::query_as("SELECT label::text, truncated, reply_text FROM inbound_messages").fetch_one(&h.state.pool).await.unwrap();
    assert_eq!(label, "reply");
    assert!(truncated);
    assert_eq!(text.as_deref(), Some(""), "no body was downloaded");
    assert_eq!(suppression_reason(&h.state, "lead0@company0.example").await.as_deref(), Some("replied"));
}

// ---------- errors ----------

#[sqlx::test]
async fn a_transient_error_backs_off_and_a_success_resets_it(pool: PgPool) {
    let h = ReplyHarness::new(pool);
    let a = h.account(ME).await;
    send_to_lead(&h, a, 0).await;
    for (n, expected_secs) in [(1, 240), (2, 480), (3, 960)] {
        h.fetcher.fail_next(FetchError::Transient("server busy".into()));
        let r = h.tick().await;
        assert_eq!((r.synced, r.failed), (0, 1));
        let row = h.sync_row(a, "inbox").await;
        assert_eq!(row.consecutive_errors, n);
        assert_eq!(row.next_sync_at, Some(h.now() + Duration::seconds(expected_secs)), "poll 120s * 2^{n}");
        assert!(row.last_error.unwrap().contains("server busy"));
        assert!(row.sync_started_at.is_none());
        h.advance(Duration::seconds(expected_secs + 1));
    }
    // The wait stops at 1 hour.
    for _ in 0..5 {
        h.fetcher.fail_next(FetchError::Transient("still busy".into()));
        h.tick().await;
        let row = h.sync_row(a, "inbox").await;
        let wait = row.next_sync_at.unwrap() - h.now();
        assert!(wait <= Duration::hours(1), "the wait never passes 1 hour, got {wait}");
        if row.consecutive_errors >= 5 {
            assert_eq!(wait, Duration::hours(1), "120s * 2^{} is over the cap", row.consecutive_errors);
        }
        h.advance(Duration::hours(2));
    }
    assert_eq!(h.sync_row(a, "inbox").await.consecutive_errors, 8);
    h.tick().await;
    let row = h.sync_row(a, "inbox").await;
    assert_eq!(row.consecutive_errors, 0);
    assert!(row.last_error.is_none());
}

#[sqlx::test]
async fn a_rate_limit_waits_for_retry_after_without_counting_an_error(pool: PgPool) {
    let h = ReplyHarness::new(pool);
    let a = h.account(ME).await;
    send_to_lead(&h, a, 0).await;
    h.fetcher.fail_next(FetchError::RateLimited { retry_after: Some(StdDuration::from_secs(90)) });
    h.tick().await;
    let row = h.sync_row(a, "inbox").await;
    assert_eq!(row.next_sync_at, Some(h.now() + Duration::seconds(90)));
    assert_eq!(row.consecutive_errors, 0);
    h.advance(Duration::seconds(91));
    h.fetcher.fail_next(FetchError::RateLimited { retry_after: None });
    h.tick().await;
    assert_eq!(h.sync_row(a, "inbox").await.next_sync_at, Some(h.now() + Duration::minutes(5)));
}

#[sqlx::test]
async fn a_missing_read_scope_turns_reading_off_but_sending_goes_on(pool: PgPool) {
    let h = ReplyHarness::new(pool);
    let a = h.account(ME).await;
    send_to_lead(&h, a, 0).await;
    h.fetcher.fail_next(FetchError::ScopeMissing);
    h.tick().await;
    let acct = load_account(&h.state, a).await;
    assert!(!acct.can_read);
    assert_eq!(acct.read_status.as_deref(), Some("reconnect_needed"));
    assert_eq!(acct.status, AccountStatus::Active, "sending is not touched");
    let (paused,): (bool,) = sqlx::query_as("SELECT paused FROM account_send_state WHERE account_id = $1").bind(a).fetch_one(&h.state.pool).await.unwrap();
    assert!(!paused);
    // Not read again until the user connects it again.
    h.advance(Duration::hours(3));
    assert_eq!(h.tick().await.claimed, 0);
}

#[sqlx::test]
async fn a_revoked_login_disconnects_the_account_and_stops_sending_and_syncing(pool: PgPool) {
    let h = ReplyHarness::new(pool);
    let a = h.gmail_account("g@acme-outreach.example").await;
    sqlx::query("UPDATE email_accounts SET can_read = true WHERE id = $1").bind(a).execute(&h.state.pool).await.unwrap();
    sent_job(&h.state, a, "lead@company.example", "<s@acme-outreach.example>", None, h.now() - Duration::days(1)).await;
    let queued = queued_job(&h.state, a, "lead@company.example", "automated").await;
    h.fetcher.fail_next(FetchError::AuthFailed("invalid_grant".into()));
    h.tick().await;
    let acct = load_account(&h.state, a).await;
    assert_eq!(acct.status, AccountStatus::Disconnected);
    assert!(acct.last_error.unwrap().contains("Connect the account again"));
    let (paused,): (bool,) = sqlx::query_as("SELECT paused FROM account_send_state WHERE account_id = $1").bind(a).fetch_one(&h.state.pool).await.unwrap();
    assert!(paused, "sending stops");
    h.advance(Duration::hours(3));
    assert_eq!(h.tick().await.claimed, 0, "syncing stops");
    assert_eq!(job_status(&h.state, queued).await, "queued", "nothing sent, nothing lost");
}

#[sqlx::test]
async fn a_wrong_imap_password_stops_reading_only(pool: PgPool) {
    let h = ReplyHarness::new(pool);
    let a = h.account(ME).await;
    send_to_lead(&h, a, 0).await;
    h.fetcher.fail_next(FetchError::AuthFailed("AUTHENTICATIONFAILED".into()));
    h.tick().await;
    let acct = load_account(&h.state, a).await;
    assert_eq!(acct.status, AccountStatus::Active);
    assert_eq!(acct.read_status.as_deref(), Some("login_failed"));
    assert!(!acct.can_read);
}

#[sqlx::test]
async fn an_invalid_cursor_restarts_from_a_day_before_the_last_success_with_no_double_actions(pool: PgPool) {
    let h = ReplyHarness::new(pool);
    let a = h.account(ME).await;
    send_to_lead(&h, a, 0).await;
    queued_job(&h.state, a, "lead0@company0.example", "automated").await;
    h.fetcher.add(Folder::Inbox, lead_reply(0), h.now() - Duration::hours(2));
    h.tick().await;
    let last_ok = h.now();
    h.advance(Duration::minutes(10));
    h.fetcher.fail_next(FetchError::CursorInvalid);
    h.tick().await;
    let row = h.sync_row(a, "inbox").await;
    assert_eq!(row.next_sync_at, Some(h.now()), "run again at once");
    assert_eq!(row.cursor.unwrap()["reset_since"], (last_ok - Duration::days(1)).to_rfc3339());
    let r = h.tick().await;
    assert_eq!(r.synced, 1);
    assert_eq!(count(&h.state, "SELECT count(*) FROM inbound_messages").await, 1);
    assert_eq!(count(&h.state, "SELECT count(*) FROM suppressions").await, 1);
    assert_eq!(count(&h.state, "SELECT count(*) FROM send_events WHERE event = 'replied'").await, 1);
}

#[sqlx::test]
async fn two_pollers_on_one_database_never_claim_the_same_row(pool: PgPool) {
    let h = ReplyHarness::new(pool);
    for i in 0..30 {
        let a = h.account(&format!("acc{i}@acme-outreach.example")).await;
        send_to_lead(&h, a, i).await;
    }
    h.tick().await; // makes the rows
    sqlx::query("UPDATE inbox_sync_state SET next_sync_at = NULL, sync_started_at = NULL").execute(&h.state.pool).await.unwrap();
    let (p1, p2, p3) = (h.state.pool.clone(), h.state.pool.clone(), h.state.pool.clone());
    let now = h.now();
    let (r1, r2, r3) = tokio::join!(
        repo::claim_due(&p1, now, 12),
        repo::claim_due(&p2, now, 12),
        repo::claim_due(&p3, now, 12)
    );
    let mut all: Vec<_> = r1.unwrap().into_iter().chain(r2.unwrap()).chain(r3.unwrap()).collect();
    let total = all.len();
    all.sort();
    all.dedup();
    assert_eq!(all.len(), total, "a row was claimed twice");
    assert_eq!(total, 30.min(36), "all rows are claimed, each once");
    // A claimed row holds a 10 minute lease.
    let (leased,): (i64,) = sqlx::query_as("SELECT count(*) FROM inbox_sync_state WHERE next_sync_at = $1 + interval '10 minutes' AND sync_started_at = $1").bind(now).fetch_one(&h.state.pool).await.unwrap();
    assert_eq!(leased as usize, total);
}

#[sqlx::test]
async fn a_crashed_sync_is_retried_after_its_lease_ends(pool: PgPool) {
    let h = ReplyHarness::new(pool);
    let a = h.account(ME).await;
    send_to_lead(&h, a, 0).await;
    h.fetcher.add(Folder::Inbox, lead_reply(0), h.now() - Duration::hours(1));
    h.tick().await;
    sqlx::query("UPDATE inbox_sync_state SET cursor = NULL").execute(&h.state.pool).await.unwrap();
    // Pretend a server claimed the row and died.
    let claimed = repo::claim_due(&h.state.pool, h.now() + Duration::minutes(3), 10).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(h.tick().await.claimed, 0, "the lease holds the row");
    h.advance(Duration::minutes(14));
    let r = h.tick().await;
    assert_eq!(r.claimed, 1, "after 10 minutes the lease is gone");
    assert_eq!(count(&h.state, "SELECT count(*) FROM suppressions").await, 1, "dedup stops double actions");
}
