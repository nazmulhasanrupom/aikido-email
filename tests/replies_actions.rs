mod common;
use aikido_email::replies::ingest::{Ingested, RelabelError, relabel};
use aikido_email::replies::model::Label;
use common::*;
use serde_json::json;
use sqlx::PgPool;

const ME: &str = "sender@acme-outreach.example";

fn now() -> chrono::DateTime<chrono::Utc> {
    utc(2026, 10, 20, 12, 0)
}

async fn set_reply_settings(st: &aikido_email::state::AppState, account: uuid::Uuid, patch: serde_json::Value) {
    let mut conn = st.pool.acquire().await.unwrap();
    let cur = aikido_email::replies::repo::load_settings(&mut conn, account).await.unwrap();
    let merged = cur.merged(&patch).unwrap();
    merged.validate().unwrap();
    sqlx::query("UPDATE account_reply_settings SET settings = $2 WHERE account_id = $1")
        .bind(account)
        .bind(serde_json::to_value(merged).unwrap())
        .execute(&mut *conn)
        .await
        .unwrap();
}

#[sqlx::test]
async fn a_reply_cancels_the_queued_follow_up(pool: PgPool) {
    let st = app_state(pool);
    let a = add_account(&st, ME).await;
    let sent = sent_job(&st, a, "jane@prospect.example", "<sent-0001@acme-outreach.example>", None, now() - chrono::Duration::days(1)).await;
    let follow_up = queued_job(&st, a, "jane@prospect.example", "automated").await;
    let manual = queued_job(&st, a, "jane@prospect.example", "manual").await;
    let other = queued_job(&st, a, "bob@elsewhere.example", "automated").await;

    let raw = fixture("human_gmail_quote.eml");
    let r = ingest_raw(&st, a, "p1", &raw, now()).await;
    assert!(matches!(r, Ingested::Stored { label: Label::Reply, .. }), "{r:?}");

    assert_eq!(job_status(&st, follow_up).await, "cancelled");
    assert_eq!(job_status(&st, manual).await, "queued", "a manual job is kept");
    assert_eq!(job_status(&st, other).await, "queued");
    let (last_error,): (Option<String>,) = sqlx::query_as("SELECT last_error FROM send_jobs WHERE id = $1").bind(follow_up).fetch_one(&st.pool).await.unwrap();
    assert_eq!(last_error.as_deref(), Some("stopped: replied"));
    assert_eq!(suppression_reason(&st, "jane@prospect.example").await.as_deref(), Some("replied"));
    let (replied_at,): (Option<chrono::DateTime<chrono::Utc>>,) = sqlx::query_as("SELECT replied_at FROM send_jobs WHERE id = $1").bind(sent).fetch_one(&st.pool).await.unwrap();
    assert_eq!(replied_at, Some(now()));

    // The stored row.
    let row: (String, String, Option<String>, serde_json::Value, Option<uuid::Uuid>) =
        sqlx::query_as("SELECT label::text, match_method::text, reply_text, actions, matched_job_id FROM inbound_messages")
            .fetch_one(&st.pool).await.unwrap();
    assert_eq!((row.0.as_str(), row.1.as_str()), ("reply", "in_reply_to"));
    assert_eq!(row.2.as_deref(), Some("Sounds good, let's talk Thursday."));
    assert_eq!(row.4, Some(sent));
    assert!(row.3.as_array().unwrap().len() >= 3, "{}", row.3);
    // The event holds the inbound id.
    let events = count(&st, "SELECT count(*) FROM send_events WHERE event = 'replied' AND detail ? 'inbound_message_id'").await;
    assert_eq!(events, 1);
    assert_eq!(count(&st, "SELECT count(*) FROM send_events WHERE event = 'cancelled' AND job_id IS NOT NULL AND detail ? 'inbound_message_id'").await, 1);
}

#[sqlx::test]
async fn the_same_email_twice_runs_the_actions_once(pool: PgPool) {
    let st = app_state(pool);
    let a = add_account(&st, ME).await;
    sent_job(&st, a, "jane@prospect.example", "<sent-0001@acme-outreach.example>", None, now() - chrono::Duration::days(1)).await;
    queued_job(&st, a, "jane@prospect.example", "automated").await;
    let raw = fixture("human_plain.eml");

    assert!(matches!(ingest_raw(&st, a, "same-id", &raw, now()).await, Ingested::Stored { .. }));
    assert!(matches!(ingest_raw(&st, a, "same-id", &raw, now()).await, Ingested::Duplicate));
    assert!(matches!(ingest_raw(&st, a, "same-id", &raw, now()).await, Ingested::Duplicate));

    assert_eq!(count(&st, "SELECT count(*) FROM inbound_messages").await, 1);
    assert_eq!(count(&st, "SELECT count(*) FROM suppressions").await, 1);
    assert_eq!(count(&st, "SELECT count(*) FROM send_events WHERE event = 'replied'").await, 1);
    assert_eq!(count(&st, "SELECT count(*) FROM send_events WHERE event = 'suppressed'").await, 1);
}

#[sqlx::test]
async fn two_pollers_ingesting_the_same_email_at_once_still_run_the_actions_once(pool: PgPool) {
    let st = app_state(pool);
    let a = add_account(&st, ME).await;
    sent_job(&st, a, "jane@prospect.example", "<sent-0001@acme-outreach.example>", None, now() - chrono::Duration::days(1)).await;
    queued_job(&st, a, "jane@prospect.example", "automated").await;
    let raw = fixture("human_plain.eml");
    let (r1, r2, r3) = tokio::join!(
        ingest_raw(&st, a, "race", &raw, now()),
        ingest_raw(&st, a, "race", &raw, now()),
        ingest_raw(&st, a, "race", &raw, now())
    );
    let stored = [&r1, &r2, &r3].iter().filter(|r| matches!(r, Ingested::Stored { .. })).count();
    assert_eq!(stored, 1);
    assert_eq!(count(&st, "SELECT count(*) FROM inbound_messages").await, 1);
    assert_eq!(count(&st, "SELECT count(*) FROM send_events WHERE event = 'replied'").await, 1);
    assert_eq!(count(&st, "SELECT count(*) FROM suppressions").await, 1);
}

#[sqlx::test]
async fn a_reply_all_from_a_colleague_stops_both_addresses(pool: PgPool) {
    let st = app_state(pool);
    let a = add_account(&st, ME).await;
    sent_job(&st, a, "jane@prospect.example", "<sent-0001@acme-outreach.example>", None, now() - chrono::Duration::days(1)).await;
    let r = ingest_raw(&st, a, "p1", &fixture("reply_all_colleague.eml"), now()).await;
    assert!(matches!(r, Ingested::Stored { label: Label::Reply, .. }));
    assert_eq!(suppression_reason(&st, "jane@prospect.example").await.as_deref(), Some("replied"));
    assert_eq!(suppression_reason(&st, "tom@prospect.example").await.as_deref(), Some("replied"));
}

#[sqlx::test]
async fn domain_scope_stops_the_company_but_never_a_free_mail_domain(pool: PgPool) {
    let st = app_state(pool);
    let a = add_account(&st, ME).await;
    set_reply_settings(&st, a, json!({ "stop_scope": "domain" })).await;
    sent_job(&st, a, "jane@prospect.example", "<sent-0001@acme-outreach.example>", None, now() - chrono::Duration::days(1)).await;
    let colleague = queued_job(&st, a, "tom@prospect.example", "automated").await;
    let manual_colleague = queued_job(&st, a, "ann@prospect.example", "manual").await;
    ingest_raw(&st, a, "p1", &fixture("human_plain.eml"), now()).await;
    let domain: Option<(String,)> = sqlx::query_as("SELECT reason::text FROM suppressions WHERE value = 'prospect.example' AND kind = 'domain'").fetch_optional(&st.pool).await.unwrap();
    assert_eq!(domain.unwrap().0, "replied");
    assert_eq!(job_status(&st, colleague).await, "cancelled");
    assert_eq!(job_status(&st, manual_colleague).await, "queued");

    // Gmail is exempt.
    sent_job(&st, a, "lead@gmail.com", "<g1@acme-outreach.example>", None, now() - chrono::Duration::days(1)).await;
    let other_gmail = queued_job(&st, a, "other@gmail.com", "automated").await;
    let eml = reply_eml("Lead <lead@gmail.com>", "<g1@acme-outreach.example>", "Re: Hi", "Yes please.");
    ingest_raw(&st, a, "p2", eml.as_bytes(), now()).await;
    assert_eq!(suppression_reason(&st, "lead@gmail.com").await.as_deref(), Some("replied"));
    assert_eq!(count(&st, "SELECT count(*) FROM suppressions WHERE value = 'gmail.com'").await, 0);
    assert_eq!(job_status(&st, other_gmail).await, "queued");
}

#[sqlx::test]
async fn auto_replies_follow_the_setting(pool: PgPool) {
    let st = app_state(pool);
    let a = add_account(&st, ME).await;
    sent_job(&st, a, "jane@prospect.example", "<sent-0001@acme-outreach.example>", None, now() - chrono::Duration::days(1)).await;
    let follow_up = queued_job(&st, a, "jane@prospect.example", "automated").await;

    let r = ingest_raw(&st, a, "p1", &fixture("gmail_vacation.eml"), now()).await;
    assert!(matches!(r, Ingested::Stored { label: Label::AutoReply, actions: 0, .. }), "{r:?}");
    assert_eq!(job_status(&st, follow_up).await, "queued", "ignore: follow-ups continue");
    assert_eq!(count(&st, "SELECT count(*) FROM suppressions").await, 0);
    let (replied,): (Option<chrono::DateTime<chrono::Utc>>,) = sqlx::query_as("SELECT replied_at FROM send_jobs WHERE status = 'sent'").fetch_one(&st.pool).await.unwrap();
    assert!(replied.is_none(), "an auto-reply is not a reply");

    set_reply_settings(&st, a, json!({ "on_auto_reply": "stop" })).await;
    ingest_raw(&st, a, "p2", &fixture("outlook_automatic_reply.eml"), now()).await;
    assert_eq!(job_status(&st, follow_up).await, "cancelled");
    assert_eq!(suppression_reason(&st, "jane@prospect.example").await.as_deref(), Some("replied"));
}

#[sqlx::test]
async fn unsubscribe_blocks_everything_and_upgrades_a_reply_suppression(pool: PgPool) {
    let st = app_state(pool);
    let a = add_account(&st, ME).await;
    sent_job(&st, a, "jane@prospect.example", "<sent-0001@acme-outreach.example>", None, now() - chrono::Duration::days(1)).await;
    // She replied first.
    ingest_raw(&st, a, "p1", &fixture("human_plain.eml"), now()).await;
    assert_eq!(suppression_reason(&st, "jane@prospect.example").await.as_deref(), Some("replied"));
    let manual = queued_job(&st, a, "jane@prospect.example", "manual").await;
    let auto = queued_job(&st, a, "jane@prospect.example", "automated").await;
    // Then she asks to stop.
    let r = ingest_raw(&st, a, "p2", &fixture("unsubscribe_short.eml"), now()).await;
    assert!(matches!(r, Ingested::Stored { label: Label::Unsubscribe, .. }), "{r:?}");
    assert_eq!(suppression_reason(&st, "jane@prospect.example").await.as_deref(), Some("unsubscribed"));
    assert_eq!(job_status(&st, manual).await, "cancelled", "unsubscribe cancels manual jobs too");
    assert_eq!(job_status(&st, auto).await, "cancelled");
}

#[sqlx::test]
async fn a_hard_bounce_suppresses_cancels_and_marks_the_job(pool: PgPool) {
    let st = app_state(pool);
    let a = add_account(&st, ME).await;
    let sent = sent_job(&st, a, "nobody@prospect.example", "<sent-0001@acme-outreach.example>", None, now() - chrono::Duration::hours(1)).await;
    let manual = queued_job(&st, a, "nobody@prospect.example", "manual").await;
    let r = ingest_raw(&st, a, "b1", &fixture("gmail_bounce_user_unknown.eml"), now()).await;
    assert!(matches!(r, Ingested::Stored { label: Label::BounceHard, .. }), "{r:?}");
    assert_eq!(suppression_reason(&st, "nobody@prospect.example").await.as_deref(), Some("hard_bounce"));
    assert_eq!(job_status(&st, manual).await, "cancelled");
    let (kind, at): (Option<String>, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("SELECT bounce_kind::text, bounced_at FROM send_jobs WHERE id = $1").bind(sent).fetch_one(&st.pool).await.unwrap();
    assert_eq!((kind.as_deref(), at), (Some("hard"), Some(now())));
    let (src,): (Option<uuid::Uuid>,) = sqlx::query_as("SELECT source_message_id FROM suppressions").fetch_one(&st.pool).await.unwrap();
    assert!(src.is_some(), "every suppression row names the inbound email");
    let (status, rec): (Option<String>, Option<String>) = sqlx::query_as("SELECT bounce_status, bounce_recipient::text FROM inbound_messages").fetch_one(&st.pool).await.unwrap();
    assert_eq!((status.as_deref(), rec.as_deref()), (Some("5.1.1"), Some("nobody@prospect.example")));
}

/// 20 sent jobs to 20 leads. Returns their message ids.
async fn twenty_sends(st: &aikido_email::state::AppState, a: uuid::Uuid) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for i in 0..20 {
        let (mid, to) = (format!("<s{i}@acme-outreach.example>"), format!("lead{i}@company{i}.example"));
        sent_job(st, a, &to, &mid, None, now() - chrono::Duration::hours(30 - i)).await;
        out.push((mid, to));
    }
    out
}

#[sqlx::test]
async fn late_hard_bounces_pause_the_account_at_the_set_rate(pool: PgPool) {
    let st = app_state(pool);
    let a = add_account(&st, ME).await;
    let sends = twenty_sends(&st, a).await;
    // 5% limit (the default) in 20 sends: 1 bounce is 5%, which is not over. 2 is over.
    let bounce = |i: usize| dsn_eml(&sends[i].0, &sends[i].1, "5.1.1", "failed");

    ingest_raw(&st, a, "b0", bounce(0).as_bytes(), now()).await;
    let (paused,): (bool,) = sqlx::query_as("SELECT paused FROM account_send_state WHERE account_id = $1").bind(a).fetch_one(&st.pool).await.unwrap();
    assert!(!paused, "1 of 20 is 5%, not over the limit");

    ingest_raw(&st, a, "b1", bounce(1).as_bytes(), now()).await;
    let (paused, reason): (bool, Option<String>) = sqlx::query_as("SELECT paused, paused_reason FROM account_send_state WHERE account_id = $1").bind(a).fetch_one(&st.pool).await.unwrap();
    assert!(paused);
    assert!(reason.unwrap().starts_with("bounce_rate"), "reason names bounce_rate");
    let acct = load_account(&st, a).await;
    assert_eq!(acct.status, aikido_email::accounts::model::AccountStatus::Error);

    // A third bounce does not pause it twice.
    ingest_raw(&st, a, "b2", bounce(2).as_bytes(), now()).await;
    assert_eq!(count(&st, "SELECT count(*) FROM send_events WHERE event = 'paused'").await, 1);
}

#[sqlx::test]
async fn blocks_count_in_the_bounce_rate_only_when_the_setting_says_so(pool: PgPool) {
    for (count_blocks, expect_paused) in [(true, true), (false, false)] {
        let st = app_state(pool.clone());
        let a = add_account(&st, ME).await;
        set_reply_settings(&st, a, json!({ "count_blocks_in_bounce_rate": count_blocks })).await;
        let sends = twenty_sends(&st, a).await;
        for i in 0..3 {
            let eml = dsn_eml(&sends[i].0, &sends[i].1, "5.7.1", "failed");
            let r = ingest_raw(&st, a, &format!("blk{i}"), eml.as_bytes(), now()).await;
            assert!(matches!(r, Ingested::Stored { label: Label::BounceBlock, .. }), "{r:?}");
        }
        let (paused,): (bool,) = sqlx::query_as("SELECT paused FROM account_send_state WHERE account_id = $1").bind(a).fetch_one(&st.pool).await.unwrap();
        assert_eq!(paused, expect_paused, "count_blocks_in_bounce_rate = {count_blocks}");
        // A block never suppresses the address.
        assert_eq!(count(&st, "SELECT count(*) FROM suppressions").await, 0);
        sqlx::query("TRUNCATE inbound_messages, send_jobs, send_events, account_send_state, account_reply_settings, account_sending_settings, email_accounts CASCADE")
            .execute(&st.pool).await.unwrap();
    }
}

#[sqlx::test]
async fn soft_bounces_become_a_hard_suppression_after_the_limit(pool: PgPool) {
    let st = app_state(pool);
    let a = add_account(&st, ME).await;
    let to = "full@prospect.example";
    let mut ids = Vec::new();
    for i in 0..3 {
        let mid = format!("<soft{i}@acme-outreach.example>");
        sent_job(&st, a, to, &mid, None, now() - chrono::Duration::days(10 - i)).await;
        ids.push(mid);
    }
    for (i, mid) in ids.iter().enumerate() {
        let eml = dsn_eml(mid, to, "5.2.2", "failed");
        let r = ingest_raw(&st, a, &format!("soft{i}"), eml.as_bytes(), now() - chrono::Duration::days(3 - i as i64)).await;
        assert!(matches!(r, Ingested::Stored { label: Label::BounceSoft, .. }), "{r:?}");
        let expected = if i == 2 { Some("hard_bounce") } else { None };
        assert_eq!(suppression_reason(&st, to).await.as_deref(), expected, "after soft bounce {}", i + 1);
    }
    // Soft bounces never pause the account.
    let (paused,): (bool,) = sqlx::query_as("SELECT paused FROM account_send_state WHERE account_id = $1").bind(a).fetch_one(&st.pool).await.unwrap();
    assert!(!paused);
}

#[sqlx::test]
async fn a_complaint_suppresses_and_cancels_everything(pool: PgPool) {
    let st = app_state(pool);
    let a = add_account(&st, ME).await;
    sent_job(&st, a, "jane@prospect.example", "<sent-0001@acme-outreach.example>", None, now() - chrono::Duration::days(1)).await;
    let manual = queued_job(&st, a, "jane@prospect.example", "manual").await;
    let r = ingest_raw(&st, a, "c1", &fixture("spam_complaint.eml"), now()).await;
    assert!(matches!(r, Ingested::Stored { label: Label::Complaint, .. }), "{r:?}");
    assert_eq!(suppression_reason(&st, "jane@prospect.example").await.as_deref(), Some("complained"));
    assert_eq!(job_status(&st, manual).await, "cancelled");
    assert_eq!(count(&st, "SELECT count(*) FROM send_events WHERE event = 'complaint'").await, 1);
}

#[sqlx::test]
async fn delayed_notices_and_parse_errors_only_log_an_event(pool: PgPool) {
    let st = app_state(pool);
    let a = add_account(&st, ME).await;
    sent_job(&st, a, "jane@prospect.example", "<sent-0001@acme-outreach.example>", None, now() - chrono::Duration::hours(1)).await;
    let queued = queued_job(&st, a, "jane@prospect.example", "automated").await;
    let eml = dsn_eml("<sent-0001@acme-outreach.example>", "jane@prospect.example", "4.4.1", "delayed");
    let r = ingest_raw(&st, a, "d1", eml.as_bytes(), now()).await;
    assert!(matches!(r, Ingested::Stored { label: Label::Delayed, actions: 0, .. }), "{r:?}");
    assert_eq!(count(&st, "SELECT count(*) FROM send_events WHERE event = 'delayed'").await, 1);
    assert_eq!(job_status(&st, queued).await, "queued");

    // A related email we cannot parse is stored with label `unknown` and no actions.
    let acct = load_account(&st, a).await;
    let headers: aikido_email::replies::parse::HeaderMap = [("in-reply-to".to_string(), "<sent-0001@acme-outreach.example>".to_string())].into();
    let mut conn = st.pool.acquire().await.unwrap();
    let settings = aikido_email::replies::repo::load_settings(&mut conn, a).await.unwrap();
    drop(conn);
    let r = aikido_email::replies::ingest::ingest(
        &st.pool,
        &aikido_email::replies::ingest::Inbound {
            account: &acct, folder: "inbox", provider_message_id: "broken", provider_thread_id: None,
            raw: &[0xff, 0xfe, 0x00], truncated: false, headers: Some(&headers), received_at: Some(now()),
        },
        &settings, &[], now(),
    ).await.unwrap();
    assert!(matches!(r, Ingested::Stored { label: Label::Unknown, actions: 0, .. }), "{r:?}");
    let (label, reason, job): (String, Option<String>, Option<uuid::Uuid>) =
        sqlx::query_as("SELECT label::text, label_reason, matched_job_id FROM inbound_messages WHERE provider_message_id = 'broken'").fetch_one(&st.pool).await.unwrap();
    assert_eq!(label, "unknown");
    assert!(reason.unwrap().contains("parse error"));
    assert!(job.is_some(), "matched by In-Reply-To from the listing headers");
    assert_eq!(count(&st, "SELECT count(*) FROM suppressions").await, 0);
    assert_eq!(count(&st, "SELECT count(*) FROM send_events WHERE event = 'inbound_unknown'").await, 1);
}

#[sqlx::test]
async fn an_unrelated_email_is_not_stored(pool: PgPool) {
    let st = app_state(pool);
    let a = add_account(&st, ME).await;
    sent_job(&st, a, "jane@prospect.example", "<sent-0001@acme-outreach.example>", None, now() - chrono::Duration::days(1)).await;
    let eml = "From: Friend <friend@elsewhere.example>\nTo: sender@acme-outreach.example\nSubject: Lunch?\nMessage-ID: <x@elsewhere.example>\n\nLunch tomorrow?\n";
    assert!(matches!(ingest_raw(&st, a, "u1", eml.as_bytes(), now()).await, Ingested::Dropped));
    // Even an unsubscribe-looking one, when it does not link to our sends.
    let eml = "From: Friend <friend@elsewhere.example>\nTo: sender@acme-outreach.example\nSubject: Re: news\nMessage-ID: <y@elsewhere.example>\n\nPlease unsubscribe me from this.\n";
    assert!(matches!(ingest_raw(&st, a, "u2", eml.as_bytes(), now()).await, Ingested::Dropped));
    assert_eq!(count(&st, "SELECT count(*) FROM inbound_messages").await, 0);
    assert_eq!(count(&st, "SELECT count(*) FROM suppressions").await, 0);
}

#[sqlx::test]
async fn a_manual_relabel_undoes_the_old_actions_and_runs_the_new_ones(pool: PgPool) {
    let st = app_state(pool);
    let a = add_account(&st, ME).await;
    let sent = sent_job(&st, a, "jane@prospect.example", "<sent-0001@acme-outreach.example>", None, now() - chrono::Duration::days(1)).await;
    let queued = queued_job(&st, a, "jane@prospect.example", "automated").await;
    ingest_raw(&st, a, "p1", &fixture("unsubscribe_short.eml"), now()).await;
    assert_eq!(suppression_reason(&st, "jane@prospect.example").await.as_deref(), Some("unsubscribed"));
    let (id,): (uuid::Uuid,) = sqlx::query_as("SELECT id FROM inbound_messages").fetch_one(&st.pool).await.unwrap();

    // It was a plain reply, not an unsubscribe. Fix it by hand.
    let row = relabel(&st.pool, id, Label::Reply, now()).await.unwrap();
    assert_eq!(row.label, Label::Reply);
    assert_eq!(row.label_source, aikido_email::replies::model::LabelSource::Manual);
    assert_eq!(suppression_reason(&st, "jane@prospect.example").await.as_deref(), Some("replied"), "unsubscribed removed, replied added");
    assert_eq!(job_status(&st, queued).await, "cancelled", "cancelled jobs are not restored");
    let (replied_at,): (Option<chrono::DateTime<chrono::Utc>>,) = sqlx::query_as("SELECT replied_at FROM send_jobs WHERE id = $1").bind(sent).fetch_one(&st.pool).await.unwrap();
    assert!(replied_at.is_some());

    // Auto-reply by hand: all suppressions of this email go away (on_auto_reply = ignore).
    relabel(&st.pool, id, Label::AutoReply, now()).await.unwrap();
    assert_eq!(count(&st, "SELECT count(*) FROM suppressions").await, 0);
    let (replied_at,): (Option<chrono::DateTime<chrono::Utc>>,) = sqlx::query_as("SELECT replied_at FROM send_jobs WHERE id = $1").bind(sent).fetch_one(&st.pool).await.unwrap();
    assert!(replied_at.is_none());

    assert!(matches!(relabel(&st.pool, uuid::Uuid::now_v7(), Label::Reply, now()).await, Err(RelabelError::NotFound)));
}

#[sqlx::test]
async fn a_relabel_from_unsubscribe_restores_an_earlier_reply_suppression(pool: PgPool) {
    let st = app_state(pool);
    let a = add_account(&st, ME).await;
    sent_job(&st, a, "jane@prospect.example", "<sent-0001@acme-outreach.example>", None, now() - chrono::Duration::days(1)).await;
    ingest_raw(&st, a, "p1", &fixture("human_plain.eml"), now()).await;
    ingest_raw(&st, a, "p2", &fixture("unsubscribe_short.eml"), now()).await;
    assert_eq!(suppression_reason(&st, "jane@prospect.example").await.as_deref(), Some("unsubscribed"));
    let (id,): (uuid::Uuid,) = sqlx::query_as("SELECT id FROM inbound_messages WHERE provider_message_id = 'p2'").fetch_one(&st.pool).await.unwrap();
    relabel(&st.pool, id, Label::AutoReply, now()).await.unwrap();
    assert_eq!(suppression_reason(&st, "jane@prospect.example").await.as_deref(), Some("replied"), "back to what the first reply made");
}

#[sqlx::test]
async fn a_manual_job_to_a_replied_address_sends_but_an_automated_one_is_skipped(pool: PgPool) {
    let h = Harness::new(pool);
    let a = h.account(ME).await;
    sqlx::query("UPDATE account_sending_settings SET settings = settings || '{\"same_recipient_cooldown_hours\": 0}'::jsonb").execute(&h.state.pool).await.unwrap();
    sent_job(&h.state, a, "jane@prospect.example", "<sent-0001@acme-outreach.example>", None, h.now() - chrono::Duration::days(1)).await;
    ingest_raw(&h.state, a, "p1", &fixture("human_plain.eml"), h.now()).await;
    assert_eq!(suppression_reason(&h.state, "jane@prospect.example").await.as_deref(), Some("replied"));

    let mut auto = new_job(a, "jane@prospect.example");
    auto.subject = "Automated follow-up".into();
    let auto_id = h.queue_job(auto).await;
    let mut manual = new_job(a, "jane@prospect.example");
    manual.subject = "My answer".into();
    manual.kind = Some(aikido_email::replies::model::JobKind::Manual);
    let manual_id = h.queue_job(manual).await;

    // Both are queued (the reply cancelled only jobs that existed then). The claim step decides.
    h.tick().await;
    assert_eq!(h.job(manual_id).await.status, aikido_email::sending::queue::JobStatus::Sent);
    assert_eq!(h.sender.calls().len(), 1);
    assert_eq!(h.sender.calls()[0].subject, "My answer");
    h.advance(chrono::Duration::minutes(20));
    h.tick().await;
    let auto = h.job(auto_id).await;
    assert_eq!(auto.status, aikido_email::sending::queue::JobStatus::Skipped);
    assert_eq!(auto.last_error.as_deref(), Some("suppressed: replied"));

    // But an unsubscribed address blocks a manual job too.
    sqlx::query("UPDATE suppressions SET reason = 'unsubscribed' WHERE value = 'jane@prospect.example'").execute(&h.state.pool).await.unwrap();
    let mut manual2 = new_job(a, "jane@prospect.example");
    manual2.kind = Some(aikido_email::replies::model::JobKind::Manual);
    let m2 = h.queue_job(manual2).await;
    h.advance(chrono::Duration::minutes(20));
    h.tick().await;
    assert_eq!(h.job(m2).await.status, aikido_email::sending::queue::JobStatus::Skipped);
}

#[sqlx::test]
async fn a_send_time_hard_bounce_and_a_late_bounce_count_once(pool: PgPool) {
    use aikido_email::sending::senders::SendError;
    let h = Harness::new(pool);
    let a = h.account(ME).await;
    h.settings(a, json!({ "pause_bounce_rate_pct": 50, "bounce_window": 4, "daily_limit": 100, "hourly_limit": 100, "base_delay_secs": 60, "jitter_min_secs": 0, "jitter_max_secs": 0, "same_recipient_cooldown_hours": 0 })).await;
    h.sender.push(Err(SendError::Permanent { reason: "550 no such user".into(), hard_bounce: true }));
    let j = h.queue(a, "dead@company.com").await;
    h.tick().await;
    let job = h.job(j).await;
    assert!(job.bounced_at.is_some());
    assert_eq!(job.bounce_kind, Some(aikido_email::replies::model::BounceKind::Hard));
    // A late DSN for the same job must not count it a second time: the same two columns, set once.
    let first_at = job.bounced_at;
    h.advance(chrono::Duration::minutes(30));
    let eml = dsn_eml(job.message_id_header.as_deref().unwrap(), "dead@company.com", "5.1.1", "failed");
    // (The job never reached `sent`; the DSN still matches by Message-ID.)
    let r = ingest_raw(&h.state, a, "late", eml.as_bytes(), h.now()).await;
    assert!(matches!(r, Ingested::Stored { label: Label::BounceHard, .. }), "{r:?}");
    let job = h.job(j).await;
    assert_eq!(job.bounced_at, first_at, "bounced_at is not set a second time");
    let mut conn = h.state.pool.acquire().await.unwrap();
    let in_window: (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE bounce_kind = 'hard') FROM send_jobs WHERE account_id = $1 AND (status = 'sent' OR bounced_at IS NOT NULL)",
    )
    .bind(a)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(in_window, (1, 1));
}

// ---------- the follow-up safety gate ----------

mod gate {
    use super::*;
    use aikido_email::sending::queue::JobStatus;
    use aikido_email::auth::api_keys::Scope;

    /// A follow-up: an automated job that answers an earlier email.
    fn follow_up(account: uuid::Uuid, to: &str) -> aikido_email::sending::queue::NewJob {
        let mut j = new_job(account, to);
        j.in_reply_to = Some("<first@example.com>".into());
        j.subject = "Re: Hello".into();
        j
    }

    async fn readable(h: &Harness, account: uuid::Uuid) {
        sqlx::query("UPDATE email_accounts SET can_read = true, read_status = 'ok' WHERE id = $1").bind(account).execute(&h.state.pool).await.unwrap();
        sqlx::query("INSERT INTO inbox_sync_state (account_id, folder) VALUES ($1, 'inbox'), ($1, 'spam') ON CONFLICT DO NOTHING").bind(account).execute(&h.state.pool).await.unwrap();
    }

    async fn synced(h: &Harness, account: uuid::Uuid, folder: &str, at: chrono::DateTime<chrono::Utc>) {
        sqlx::query("UPDATE inbox_sync_state SET last_success_at = $3 WHERE account_id = $1 AND folder = $2").bind(account).bind(folder).bind(at).execute(&h.state.pool).await.unwrap();
    }

    #[sqlx::test]
    async fn a_follow_up_waits_while_the_sync_is_stale_and_sends_when_it_is_fresh(pool: PgPool) {
        let h = Harness::new(pool);
        let a = h.account("a@example.com").await;
        readable(&h, a).await;
        let id = h.queue_job(follow_up(a, "lead@company.com")).await;

        // Never synced: stale.
        h.tick().await;
        assert_eq!(h.sender.calls().len(), 0);
        let j = h.job(id).await;
        assert_eq!(j.status, JobStatus::Queued);
        assert_eq!(j.send_after, Some(h.now() + chrono::Duration::minutes(5)));
        assert!(h.events(id).await.contains(&"deferred".to_string()));

        // The oldest folder decides: inbox fresh, spam 40 minutes old => still stale.
        h.advance(chrono::Duration::minutes(6));
        synced(&h, a, "inbox", h.now()).await;
        synced(&h, a, "spam", h.now() - chrono::Duration::minutes(40)).await;
        h.tick().await;
        assert_eq!(h.sender.calls().len(), 0, "the oldest folder is too old");

        // Both fresh (14 minutes ago is inside the 15 minute limit): it goes.
        h.advance(chrono::Duration::minutes(6));
        synced(&h, a, "inbox", h.now() - chrono::Duration::minutes(14)).await;
        synced(&h, a, "spam", h.now() - chrono::Duration::minutes(3)).await;
        h.tick().await;
        assert_eq!(h.sender.calls().len(), 1);
        assert_eq!(h.job(id).await.status, JobStatus::Sent);
    }

    #[sqlx::test]
    async fn first_emails_and_manual_jobs_are_not_held_back(pool: PgPool) {
        let h = Harness::new(pool);
        let a = h.account("a@example.com").await;
        readable(&h, a).await; // never synced => the gate is closed for follow-ups
        let first = h.queue(a, "first@company.com").await;
        h.tick().await;
        assert_eq!(h.job(first).await.status, JobStatus::Sent, "a first email has no follow-up gate");

        h.advance(chrono::Duration::minutes(20));
        let mut m = follow_up(a, "manual@company2.com");
        m.kind = Some(aikido_email::replies::model::JobKind::Manual);
        let manual = h.queue_job(m).await;
        h.tick().await;
        assert_eq!(h.job(manual).await.status, JobStatus::Sent, "a manual answer is not held back");
    }

    #[sqlx::test]
    async fn with_no_read_access_follow_ups_wait_until_reconnected_or_the_setting_is_zero(pool: PgPool) {
        let h = Harness::new(pool);
        let a = h.account("a@example.com").await; // an SMTP account with no IMAP: can_read = false
        let id = h.queue_job(follow_up(a, "lead@company.com")).await;
        h.tick().await;
        assert_eq!(h.job(id).await.status, JobStatus::Queued);

        let keys = bootstrap_keys(&h.state).await;
        let r = call(&h.state, "GET", &format!("/v1/sending/accounts/{a}/status"), Some(&keys[&Scope::Sending]), None).await;
        assert_eq!(r.body["wait"]["reason"], "reply_check_unavailable", "{}", r.body);

        // Reconnected, and read lately: it goes.
        h.advance(chrono::Duration::minutes(6));
        readable(&h, a).await;
        synced(&h, a, "inbox", h.now()).await;
        synced(&h, a, "spam", h.now()).await;
        h.tick().await;
        assert_eq!(h.job(id).await.status, JobStatus::Sent);

        // The other way out: turn the gate off.
        sqlx::query("UPDATE email_accounts SET can_read = false WHERE id = $1").bind(a).execute(&h.state.pool).await.unwrap();
        let id2 = h.queue_job(follow_up(a, "other@company2.com")).await;
        h.advance(chrono::Duration::minutes(20));
        h.tick().await;
        assert_eq!(h.job(id2).await.status, JobStatus::Queued);
        h.settings(a, json!({ "reply_check_max_age_mins": 0 })).await;
        h.advance(chrono::Duration::minutes(6));
        h.tick().await;
        assert_eq!(h.job(id2).await.status, JobStatus::Sent);
    }

    #[sqlx::test]
    async fn the_status_route_says_reply_check_stale(pool: PgPool) {
        let h = Harness::new(pool);
        let a = h.account("a@example.com").await;
        readable(&h, a).await;
        h.queue_job(follow_up(a, "lead@company.com")).await;
        let keys = bootstrap_keys(&h.state).await;
        let status = |h: &Harness| {
            let (state, key) = (h.state.clone(), keys[&Scope::Sending].clone());
            async move { call(&state, "GET", &format!("/v1/sending/accounts/{a}/status"), Some(&key), None).await.body }
        };
        assert_eq!(status(&h).await["wait"]["reason"], "reply_check_stale");
        synced(&h, a, "inbox", h.now()).await;
        synced(&h, a, "spam", h.now()).await;
        assert!(status(&h).await["wait"].is_null(), "fresh inbox: nothing to wait for");
        // A first email is due too: the account is not "waiting".
        sqlx::query("UPDATE inbox_sync_state SET last_success_at = NULL").execute(&h.state.pool).await.unwrap();
        h.queue(a, "first@company.com").await;
        assert!(status(&h).await["wait"].is_null());
    }

    #[sqlx::test]
    async fn the_gate_works_with_the_real_reply_poller(pool: PgPool) {
        // The whole loop: the poller reads the inbox, which opens the gate, and the follow-up goes out.
        let h = ReplyHarness::new(pool);
        let a = h.account(ME).await;
        sent_job(&h.state, a, "lead@company.example", "<first@acme-outreach.example>", None, h.now() - chrono::Duration::days(2)).await;
        let sender_state = {
            // The same database and clock for a sending engine.
            let fake = aikido_email::sending::senders::fake::FakeSender::new();
            let mut st = h.state.clone();
            let eng = aikido_email::sending::worker::Engine::new(st.clone(), std::sync::Arc::new(aikido_email::sending::worker::FakeSenders(fake.clone())));
            st.clock = h.state.clock.clone();
            (eng, fake)
        };
        let (engine, fake) = sender_state;
        let engine = std::sync::Arc::new(engine);
        let mut j = new_job(a, "lead@company.example");
        j.in_reply_to = Some("<first@acme-outreach.example>".into());
        aikido_email::sending::queue::enqueue(&h.state.pool, vec![j], h.now()).await.unwrap();

        aikido_email::sending::scheduler::tick(&engine, 5).await.unwrap();
        assert_eq!(fake.calls().len(), 0, "the inbox was not read yet");
        h.tick().await; // the reply poller reads the inbox: no reply
        h.advance(chrono::Duration::minutes(6));
        aikido_email::sending::scheduler::tick(&engine, 5).await.unwrap();
        assert_eq!(fake.calls().len(), 1, "the inbox was read a moment ago: the follow-up goes out");
    }
}
