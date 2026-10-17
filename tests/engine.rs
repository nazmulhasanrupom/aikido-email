mod common;
use aikido_email::sending::queue::JobStatus;
use aikido_email::sending::senders::{SendError, SendReceipt};
use aikido_email::sending::worker::{Engine, FakeSenders};
use aikido_email::accounts::model::AccountStatus;
use chrono::{Datelike, Duration, Timelike};
use common::*;
use serde_json::json;
use sqlx::PgPool;
use std::sync::Arc;

fn ok() -> Result<SendReceipt, SendError> {
    Ok(SendReceipt { provider_message_id: Some("m1".into()), provider_thread_id: None, message_id_header: None })
}

#[sqlx::test]
async fn sends_one_email_then_waits_for_the_gap(pool: PgPool) {
    let h = Harness::new(pool);
    let account = h.account("a@example.com").await;
    let jobs = [h.queue(account, "one@company.com").await, h.queue(account, "two@company.com").await, h.queue(account, "three@company.com").await];

    let report = h.tick().await;
    assert_eq!(report.sent, 1);
    assert_eq!(h.sender.calls().len(), 1);
    assert_eq!(h.job(jobs[0]).await.status, JobStatus::Sent);
    assert_eq!(h.job(jobs[0]).await.sent_at, Some(h.now()));
    let st = h.send_state(account).await;
    assert_eq!(st.sent_today, 1);
    assert_eq!(st.last_sent_at, Some(h.now()));
    let gap = (st.next_send_at.unwrap() - h.now()).num_seconds();
    assert!((420..=900).contains(&gap), "gap was {gap}");

    // Nothing happens before the gap ends.
    h.advance(Duration::minutes(6));
    assert_eq!(h.tick().await.sent, 0);
    // After 15 minutes the next one goes.
    h.advance(Duration::minutes(9) + Duration::seconds(1));
    assert_eq!(h.tick().await.sent, 1);
    assert_eq!(h.job(jobs[1]).await.status, JobStatus::Sent);
    assert_eq!(h.job(jobs[2]).await.status, JobStatus::Queued);
    assert_eq!(h.events(jobs[0]).await, ["queued", "claimed", "sent"]);
}

#[sqlx::test]
async fn emails_go_7_to_15_minutes_apart_and_only_inside_the_window(pool: PgPool) {
    // From Saturday morning to Monday evening, one tick every 3 minutes.
    let h = Harness::at(pool, utc(2026, 10, 17, 8, 0));
    let account = h.account("a@example.com").await;
    let mut jobs = Vec::new();
    for i in 0..12 {
        jobs.push(h.queue(account, &format!("lead{i}@company{i}.com")).await);
    }
    let end = utc(2026, 10, 19, 17, 30);
    while h.now() < end {
        h.tick().await;
        h.advance(Duration::minutes(3));
    }
    let mut sent = Vec::new();
    for id in &jobs {
        let j = h.job(*id).await;
        assert_eq!(j.status, JobStatus::Sent, "job {} is {:?}", j.to_email, j.status);
        sent.push(j.sent_at.unwrap());
    }
    sent.sort();
    for t in &sent {
        assert_eq!(t.date_naive().day(), 19, "nothing goes on the weekend, got {t}");
        let minutes = t.hour() * 60 + t.minute();
        assert!(minutes >= 9 * 60 && minutes < 17 * 60, "{t} is outside 09:00-17:00");
    }
    // The first one goes 0 to 15 minutes after the window opens (plus the 3 minute tick).
    assert!(sent[0] <= utc(2026, 10, 19, 9, 18), "first send at {}", sent[0]);
    for w in sent.windows(2) {
        let gap = (w[1] - w[0]).num_seconds();
        assert!((420..=900 + 180).contains(&gap), "gap {gap}s between {} and {}", w[0], w[1]);
    }
}

#[sqlx::test]
async fn nothing_is_sent_outside_the_window_or_on_a_weekend(pool: PgPool) {
    for (when, label) in [
        (utc(2026, 10, 19, 8, 59), "before the window"),
        (utc(2026, 10, 19, 17, 0), "at closing time"),
        (utc(2026, 10, 17, 11, 0), "on Saturday"),
        (utc(2026, 10, 18, 11, 0), "on Sunday"),
    ] {
        let pool = pool_copy(&pool).await;
        let h = Harness::at(pool, when);
        let account = h.account("a@example.com").await;
        h.queue(account, "lead@company.com").await;
        assert_eq!(h.tick().await.sent, 0, "{label}");
        assert!(h.sender.calls().is_empty(), "{label}");
    }
}

/// A clean copy of the database, so one test can try several starting points.
async fn pool_copy(pool: &PgPool) -> PgPool {
    sqlx::query("TRUNCATE send_events, send_jobs, account_send_state, account_sending_settings, email_accounts, suppressions CASCADE")
        .execute(pool)
        .await
        .unwrap();
    pool.clone()
}

#[sqlx::test]
async fn the_account_time_zone_decides(pool: PgPool) {
    // 04:00 UTC Monday is 10:00 in Dhaka.
    let h = Harness::at(pool, utc(2026, 10, 19, 4, 0));
    let account = h.account("a@example.com").await;
    h.settings(account, json!({ "timezone": "Asia/Dhaka" })).await;
    h.queue(account, "lead@company.com").await;
    assert_eq!(h.tick().await.sent, 1);
}

#[sqlx::test]
async fn two_engine_copies_never_send_from_one_account_at_once(pool: PgPool) {
    let h = Harness::new(pool);
    h.sender.set_delay(std::time::Duration::from_millis(300));
    let account = h.account("a@example.com").await;
    for i in 0..10 {
        h.queue(account, &format!("lead{i}@company{i}.com")).await;
    }
    // Four copies of the engine tick at the same moment on one database.
    let copies: Vec<Arc<Engine>> = (0..4)
        .map(|_| {
            Arc::new(Engine::new(h.state.clone(), Arc::new(FakeSenders(h.sender.clone()))))
        })
        .collect();
    let ticks = copies.iter().map(|e| aikido_email::sending::scheduler::tick(e, 20));
    let reports = futures::future::join_all(ticks).await;
    let sent: usize = reports.into_iter().map(|r| r.unwrap().sent).sum();
    assert_eq!(sent, 1, "only one copy may send from this account");
    assert_eq!(h.sender.calls().len(), 1);
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM send_jobs WHERE status = 'sending'").fetch_one(&h.state.pool).await.unwrap();
    assert_eq!(n, 0);
}

#[sqlx::test]
async fn many_accounts_send_in_parallel_one_each(pool: PgPool) {
    let h = Harness::new(pool);
    h.sender.set_delay(std::time::Duration::from_millis(200));
    for a in 0..5 {
        let account = h.account(&format!("acct{a}@example.com")).await;
        for i in 0..4 {
            h.queue(account, &format!("lead{a}{i}@company{a}{i}.com")).await;
        }
    }
    let copies: Vec<Arc<Engine>> = (0..3)
        .map(|_| {
            Arc::new(Engine::new(h.state.clone(), Arc::new(FakeSenders(h.sender.clone()))))
        })
        .collect();
    let started = std::time::Instant::now();
    let reports = futures::future::join_all(copies.iter().map(|e| aikido_email::sending::scheduler::tick(e, 20))).await;
    let sent: usize = reports.into_iter().map(|r| r.unwrap().sent).sum();
    assert_eq!(sent, 5, "exactly one email per account");
    assert!(started.elapsed() < std::time::Duration::from_millis(900), "accounts run in parallel");
    let froms: std::collections::HashSet<String> = h.sender.calls().iter().map(|m| m.from_email.clone()).collect();
    assert_eq!(froms.len(), 5);
}

#[sqlx::test]
async fn a_job_in_sending_for_11_minutes_becomes_stuck_and_is_not_resent(pool: PgPool) {
    let h = Harness::new(pool);
    let account = h.account("a@example.com").await;
    let crashed = h.queue(account, "crashed@company.com").await;
    sqlx::query("UPDATE send_jobs SET status = 'sending', attempts = 1, claimed_at = $2 WHERE id = $1")
        .bind(crashed)
        .bind(h.now() - Duration::minutes(11))
        .execute(&h.state.pool)
        .await
        .unwrap();
    let recent = h.queue(account, "recent@company.com").await;
    sqlx::query("UPDATE send_jobs SET status = 'sending', attempts = 1, claimed_at = $2 WHERE id = $1")
        .bind(recent)
        .bind(h.now() - Duration::minutes(9))
        .execute(&h.state.pool)
        .await
        .unwrap();

    let report = h.tick().await;
    assert_eq!(report.stuck, 1);
    assert_eq!(h.job(crashed).await.status, JobStatus::Stuck);
    assert_eq!(h.job(recent).await.status, JobStatus::Sending, "9 minutes is still fine");
    assert!(h.events(crashed).await.contains(&"stuck".to_string()));
    // Never resent, no matter how many ticks.
    for _ in 0..3 {
        h.advance(Duration::minutes(20));
        h.tick().await;
    }
    assert!(h.sender.calls().iter().all(|m| m.to_email != "crashed@company.com"));
    assert_eq!(h.job(crashed).await.status, JobStatus::Stuck);
}

#[sqlx::test]
async fn three_failures_in_a_row_pause_the_account_with_a_reason(pool: PgPool) {
    let h = Harness::new(pool);
    let account = h.account("a@example.com").await;
    for i in 0..4 {
        h.queue(account, &format!("lead{i}@company{i}.com")).await;
    }
    h.sender.push_many([
        Err(SendError::Transient("connection reset".into())),
        Err(SendError::Transient("connection reset".into())),
        Err(SendError::Transient("connection reset".into())),
    ]);
    for _ in 0..3 {
        assert_eq!(h.tick().await.sent, 1);
        h.advance(Duration::minutes(16));
    }
    let st = h.send_state(account).await;
    assert!(st.paused);
    assert_eq!(st.consecutive_failures, 3);
    let reason = st.paused_reason.unwrap();
    assert!(reason.contains("3 failures in a row") && reason.contains("connection reset"), "{reason}");
    assert_eq!(h.account_row(account).await.status, AccountStatus::Error);
    // A paused account sends nothing.
    let calls = h.sender.calls().len();
    h.advance(Duration::minutes(30));
    h.tick().await;
    assert_eq!(h.sender.calls().len(), calls);

    // Resume through the API: failures reset, sending continues.
    let keys = bootstrap_keys(&h.state).await;
    let r = call(&h.state, "POST", &format!("/v1/sending/accounts/{account}/resume"), Some(&keys[&aikido_email::auth::api_keys::Scope::Sending]), None).await;
    assert_eq!(r.status, 200);
    let st = h.send_state(account).await;
    assert!(!st.paused && st.consecutive_failures == 0);
    assert_eq!(h.account_row(account).await.status, AccountStatus::Active);
}

#[sqlx::test]
async fn a_suppressed_address_is_skipped_and_does_not_start_the_gap(pool: PgPool) {
    let h = Harness::new(pool);
    let account = h.account("a@example.com").await;
    sqlx::query("INSERT INTO suppressions (value, kind, reason) VALUES ('blocked@company.com', 'email', 'unsubscribed'), ('spam.org', 'domain', 'complained')")
        .execute(&h.state.pool)
        .await
        .unwrap();
    let blocked = h.queue(account, "Blocked@Company.com").await;
    let domain_blocked = h.queue(account, "someone@spam.org").await;

    let before = h.send_state(account).await;
    h.tick().await;
    assert_eq!(h.job(blocked).await.status, JobStatus::Skipped);
    assert_eq!(h.job(domain_blocked).await.status, JobStatus::Skipped);
    assert!(h.job(blocked).await.last_error.unwrap().contains("unsubscribed"));
    assert!(h.sender.calls().is_empty());
    let after = h.send_state(account).await;
    assert_eq!(after.sent_today, 0);
    assert!(after.last_sent_at.is_none());
    assert!(after.next_send_at.map_or(true, |n| n <= h.now()), "no gap timer: {:?}", after.next_send_at);
    let _ = before;

    // A good email queued next goes at once.
    let good = h.queue(account, "good@company.com").await;
    h.advance(Duration::seconds(15));
    assert_eq!(h.tick().await.sent, 1);
    assert_eq!(h.job(good).await.status, JobStatus::Sent);
}

#[sqlx::test]
async fn skipped_jobs_do_not_block_a_good_job_in_the_same_tick(pool: PgPool) {
    let h = Harness::new(pool);
    let account = h.account("a@example.com").await;
    sqlx::query("INSERT INTO suppressions (value, kind, reason) VALUES ('blocked@company.com', 'email', 'manual')").execute(&h.state.pool).await.unwrap();
    let blocked = h.queue(account, "blocked@company.com").await;
    let good = h.queue(account, "good@company.com").await;
    assert_eq!(h.tick().await.sent, 1);
    assert_eq!(h.job(blocked).await.status, JobStatus::Skipped);
    assert_eq!(h.job(good).await.status, JobStatus::Sent);
}

#[sqlx::test]
async fn auth_failed_disconnects_the_account_and_requeues_the_job(pool: PgPool) {
    let h = Harness::new(pool);
    let account = h.account("a@example.com").await;
    let job = h.queue(account, "lead@company.com").await;
    h.sender.push(Err(SendError::AuthFailed("535 bad credentials".into())));
    h.tick().await;

    let j = h.job(job).await;
    assert_eq!(j.status, JobStatus::Queued);
    assert_eq!(j.attempts, 0, "the attempt is not counted");
    let acct = h.account_row(account).await;
    assert_eq!(acct.status, AccountStatus::Disconnected);
    assert!(acct.last_error.unwrap().contains("535"));
    let st = h.send_state(account).await;
    assert!(st.paused);
    assert!(st.paused_reason.unwrap().contains("Connect the account again"));
    // Nothing more is sent.
    h.advance(Duration::minutes(20));
    assert_eq!(h.tick().await.sent, 0);

    // Resume is refused while disconnected.
    let keys = bootstrap_keys(&h.state).await;
    let r = call(&h.state, "POST", &format!("/v1/sending/accounts/{account}/resume"), Some(&keys[&aikido_email::auth::api_keys::Scope::Sending]), None).await;
    assert_eq!(r.status, 409);

    // Connecting again fixes it, and the queued job goes out.
    let again = add_account(&h.state, "a@example.com").await;
    assert_eq!(again, account);
    assert_eq!(h.account_row(account).await.status, AccountStatus::Active);
    assert!(!h.send_state(account).await.paused);
    h.advance(Duration::minutes(20));
    assert_eq!(h.tick().await.sent, 1);
    assert_eq!(h.job(job).await.status, JobStatus::Sent);
}

#[sqlx::test]
async fn transient_errors_retry_with_backoff_then_fail(pool: PgPool) {
    let h = Harness::new(pool);
    let account = h.account("a@example.com").await;
    h.settings(account, json!({ "pause_after_failures": 10 })).await;
    let job = h.queue(account, "lead@company.com").await;
    h.sender.push_many([
        Err(SendError::Transient("try later".into())),
        Err(SendError::Transient("try later".into())),
        Err(SendError::Transient("try later".into())),
    ]);

    h.tick().await;
    let j = h.job(job).await;
    assert_eq!((j.status, j.attempts), (JobStatus::Queued, 1));
    assert_eq!(j.send_after, Some(h.now() + Duration::minutes(15)));
    let first_message_id = j.message_id_header.clone().unwrap();

    h.advance(Duration::minutes(14));
    assert_eq!(h.tick().await.sent, 0, "still waiting for the backoff");
    h.advance(Duration::minutes(2));
    h.tick().await;
    let j = h.job(job).await;
    assert_eq!((j.status, j.attempts), (JobStatus::Queued, 2));
    assert_eq!(j.send_after, Some(h.now() + Duration::minutes(60)));
    assert_eq!(j.message_id_header.unwrap(), first_message_id, "the Message-ID stays the same on a retry");

    h.advance(Duration::minutes(61));
    h.tick().await;
    let j = h.job(job).await;
    assert_eq!((j.status, j.attempts), (JobStatus::Failed, 3));
    assert!(j.last_error.unwrap().contains("try later"));
    assert_eq!(h.sender.calls().len(), 3);
}

#[sqlx::test]
async fn a_hard_bounce_fails_the_job_and_suppresses_the_address(pool: PgPool) {
    let h = Harness::new(pool);
    let account = h.account("a@example.com").await;
    let dead = h.queue(account, "dead@company.com").await;
    h.sender.push(Err(SendError::Permanent { reason: "550 no such user".into(), hard_bounce: true }));
    h.tick().await;
    assert_eq!(h.job(dead).await.status, JobStatus::Failed);
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM suppressions WHERE value = 'dead@company.com' AND reason = 'hard_bounce'").fetch_one(&h.state.pool).await.unwrap();
    assert_eq!(n, 1);
    assert!(h.events(dead).await.contains(&"hard_bounce".to_string()));
    // The gap applies after the attempt. A hard bounce does not count as an "in a row" failure.
    assert_eq!(h.send_state(account).await.consecutive_failures, 0);
    // A later job to the same address is skipped.
    let again = h.queue(account, "dead@company.com").await;
    h.advance(Duration::minutes(20));
    h.tick().await;
    assert_eq!(h.job(again).await.status, JobStatus::Skipped);
}

#[sqlx::test]
async fn a_high_bounce_rate_pauses_the_account(pool: PgPool) {
    let h = Harness::new(pool);
    let account = h.account("a@example.com").await;
    h.settings(account, json!({ "daily_limit": 100, "hourly_limit": 100 })).await;
    for i in 0..25 {
        h.queue(account, &format!("lead{i}@gmail.com")).await;
    }
    // 18 good sends, then 2 hard bounces: 2 of 20 = 10%, over the 5% limit.
    for _ in 0..18 {
        h.sender.push(ok());
    }
    for _ in 0..2 {
        h.sender.push(Err(SendError::Permanent { reason: "550 no such user".into(), hard_bounce: true }));
    }
    for i in 0..20 {
        assert_eq!(h.tick().await.sent, 1, "send {i}");
        if i < 19 {
            assert!(!h.send_state(account).await.paused, "not paused yet at send {i}");
        }
        h.advance(Duration::minutes(16));
    }
    let st = h.send_state(account).await;
    assert!(st.paused);
    assert!(st.paused_reason.unwrap().contains("Bounce rate too high"));
}

#[sqlx::test]
async fn a_low_bounce_rate_does_not_pause(pool: PgPool) {
    let h = Harness::new(pool);
    let account = h.account("a@example.com").await;
    h.settings(account, json!({ "daily_limit": 100, "hourly_limit": 100 })).await;
    for i in 0..25 {
        h.queue(account, &format!("lead{i}@gmail.com")).await;
    }
    // 1 bounce in 20 sends is exactly 5%. That is not over the limit.
    for _ in 0..19 {
        h.sender.push(ok());
    }
    h.sender.push(Err(SendError::Permanent { reason: "550 no such user".into(), hard_bounce: true }));
    for _ in 0..20 {
        h.tick().await;
        h.advance(Duration::minutes(16));
    }
    assert!(!h.send_state(account).await.paused);
}

#[sqlx::test]
async fn rate_limits_wait_without_counting_the_attempt(pool: PgPool) {
    let h = Harness::new(pool);
    let account = h.account("a@example.com").await;
    let job = h.queue(account, "lead@company.com").await;
    h.sender.push(Err(SendError::RateLimited { retry_after: Some(std::time::Duration::from_secs(30 * 60)), daily: false }));
    h.tick().await;
    let j = h.job(job).await;
    assert_eq!((j.status, j.attempts), (JobStatus::Queued, 0));
    let st = h.send_state(account).await;
    assert_eq!(st.next_send_at, Some(h.now() + Duration::minutes(30)));
    assert_eq!(st.consecutive_failures, 0);
    h.advance(Duration::minutes(29));
    assert_eq!(h.tick().await.sent, 0);
    h.advance(Duration::minutes(2));
    assert_eq!(h.tick().await.sent, 1);

    // A daily quota: wait until the next local day.
    let job2 = h.queue(account, "lead2@company.com").await;
    h.advance(Duration::minutes(20));
    h.sender.push(Err(SendError::RateLimited { retry_after: None, daily: true }));
    h.tick().await;
    assert_eq!(h.job(job2).await.status, JobStatus::Queued);
    assert_eq!(h.send_state(account).await.next_send_at, Some(utc(2026, 10, 20, 9, 0)));
}

#[sqlx::test]
async fn an_unknown_result_is_stuck_and_never_resent(pool: PgPool) {
    let h = Harness::new(pool);
    let account = h.account("a@example.com").await;
    let job = h.queue(account, "lead@company.com").await;
    h.sender.push(Err(SendError::Unknown("timeout after DATA".into())));
    h.tick().await;
    assert_eq!(h.job(job).await.status, JobStatus::Stuck);
    assert!(h.send_state(account).await.next_send_at.unwrap() > h.now(), "the gap still applies");
    for _ in 0..3 {
        h.advance(Duration::minutes(30));
        h.tick().await;
    }
    assert_eq!(h.sender.calls().len(), 1);
}

#[sqlx::test]
async fn the_same_recipient_cooldown_works_across_accounts(pool: PgPool) {
    let h = Harness::new(pool);
    let a = h.account("a@example.com").await;
    let b = h.account("b@example.org").await;
    h.queue(a, "lead@company.com").await;
    h.tick().await;
    assert_eq!(h.sender.calls().len(), 1);

    // Account B has the same lead and another one. The lead waits, the other goes.
    let from_b = h.queue(b, "lead@company.com").await;
    let other = h.queue(b, "other@company.com").await;
    h.advance(Duration::seconds(30));
    h.tick().await;
    assert_eq!(h.sender.calls().len(), 2);
    assert_eq!(h.job(other).await.status, JobStatus::Sent);
    let deferred = h.job(from_b).await;
    assert_eq!(deferred.status, JobStatus::Queued);
    assert!(deferred.send_after.unwrap() >= utc(2026, 10, 20, 10, 0) && deferred.send_after.unwrap() <= utc(2026, 10, 20, 10, 1));
    assert!(h.events(from_b).await.contains(&"deferred".to_string()));

    // After the cooldown (on a send day, inside the window) it goes.
    h.set_time(utc(2026, 10, 20, 10, 5));
    h.tick().await;
    assert_eq!(h.job(from_b).await.status, JobStatus::Sent);
}

#[sqlx::test]
async fn two_accounts_claiming_the_same_lead_at_once_send_it_only_once(pool: PgPool) {
    let h = Harness::new(pool);
    h.sender.set_delay(std::time::Duration::from_millis(50));
    let a = h.account("a@example.com").await;
    let b = h.account("b@example.org").await;
    let c = h.account("c@example.net").await;
    for round in 0..8 {
        let lead = format!("lead{round}@company.com");
        for account in [a, b, c] {
            h.queue(account, &lead).await;
        }
    }
    // Eight rounds. Each round, three engine copies tick together; the three accounts compete for one lead.
    for _ in 0..8 {
        let copies: Vec<Arc<Engine>> = (0..3).map(|_| Arc::new(Engine::new(h.state.clone(), Arc::new(FakeSenders(h.sender.clone()))))).collect();
        futures::future::join_all(copies.iter().map(|e| aikido_email::sending::scheduler::tick(e, 20))).await;
        h.advance(Duration::minutes(16));
    }
    let mut per_lead = std::collections::HashMap::new();
    for m in h.sender.calls() {
        *per_lead.entry(m.to_email.clone()).or_insert(0) += 1;
    }
    assert!(!per_lead.is_empty());
    for (lead, n) in per_lead {
        assert_eq!(n, 1, "{lead} was sent {n} times");
    }
}

#[sqlx::test]
async fn the_per_domain_daily_limit(pool: PgPool) {
    let h = Harness::new(pool);
    let account = h.account("a@example.com").await;
    h.settings(account, json!({ "per_domain_daily_limit": 2, "hourly_limit": 50 })).await;
    let acme: Vec<_> = {
        let mut v = Vec::new();
        for i in 0..4 {
            v.push(h.queue(account, &format!("p{i}@acme.com")).await);
        }
        v
    };
    let other = h.queue(account, "x@other.com").await;
    let free: Vec<_> = {
        let mut v = Vec::new();
        for i in 0..3 {
            v.push(h.queue(account, &format!("free{i}@gmail.com")).await);
        }
        v
    };
    // Run through the day.
    for _ in 0..30 {
        h.tick().await;
        h.advance(Duration::minutes(16));
        if h.now() >= utc(2026, 10, 19, 16, 59) {
            break;
        }
    }
    assert_eq!(h.job(acme[0]).await.status, JobStatus::Sent);
    assert_eq!(h.job(acme[1]).await.status, JobStatus::Sent);
    assert_eq!(h.job(acme[2]).await.status, JobStatus::Queued, "acme.com is over 2 a day");
    assert_eq!(h.job(acme[3]).await.status, JobStatus::Queued);
    assert_eq!(h.job(other).await.status, JobStatus::Sent);
    for f in free {
        assert_eq!(h.job(f).await.status, JobStatus::Sent, "free mail domains are exempt");
    }
    // The held jobs wait for the next window start.
    let held = h.job(acme[2]).await;
    assert_eq!(held.send_after, Some(utc(2026, 10, 20, 9, 0)));

    // Next day they go.
    h.set_time(utc(2026, 10, 20, 9, 30));
    for _ in 0..4 {
        h.tick().await;
        h.advance(Duration::minutes(16));
    }
    assert_eq!(h.job(acme[2]).await.status, JobStatus::Sent);
    assert_eq!(h.job(acme[3]).await.status, JobStatus::Sent);
}

#[sqlx::test]
async fn the_daily_limit_stops_sending_and_the_next_day_starts_over(pool: PgPool) {
    let h = Harness::new(pool);
    let account = h.account("a@example.com").await;
    h.settings(account, json!({ "daily_limit": 3 })).await;
    for i in 0..6 {
        h.queue(account, &format!("lead{i}@company{i}.com")).await;
    }
    for _ in 0..12 {
        h.tick().await;
        h.advance(Duration::minutes(16));
    }
    assert_eq!(h.sender.calls().len(), 3, "3 is the limit for Monday");
    let st = h.send_state(account).await;
    assert_eq!(st.sent_today, 3);
    assert_eq!(st.today_limit, Some(3));
    // The account is parked until Tuesday's window opens.
    assert!(st.next_send_at.unwrap() >= utc(2026, 10, 20, 9, 0));

    h.set_time(utc(2026, 10, 20, 9, 20));
    for _ in 0..8 {
        h.tick().await;
        h.advance(Duration::minutes(16));
    }
    assert_eq!(h.sender.calls().len(), 6);
    let st = h.send_state(account).await;
    assert_eq!(st.local_day.unwrap().day(), 20);
    assert_eq!(st.sent_today, 3);
}

#[sqlx::test]
async fn warm_up_grows_the_cap_every_sending_day(pool: PgPool) {
    let h = Harness::new(pool);
    let account = h.account("a@example.com").await;
    h.settings(account, json!({ "warmup_enabled": true, "hourly_limit": 50, "base_delay_secs": 60, "jitter_min_secs": 0, "jitter_max_secs": 0 })).await;
    for i in 0..30 {
        h.queue(account, &format!("lead{i}@gmail.com")).await;
    }
    let mut per_day = Vec::new();
    for day in 0..3 {
        h.set_time(utc(2026, 10, 19 + day, 9, 30));
        let before = h.sender.calls().len();
        for _ in 0..40 {
            h.tick().await;
            h.advance(Duration::minutes(2));
        }
        per_day.push(h.sender.calls().len() - before);
    }
    assert_eq!(per_day, [5, 7, 9], "day 1 = 5, then +2 each sending day");
}

#[sqlx::test]
async fn the_hourly_limit_is_a_safety_net(pool: PgPool) {
    let h = Harness::new(pool);
    let account = h.account("a@example.com").await;
    h.settings(account, json!({ "base_delay_secs": 60, "jitter_min_secs": 0, "jitter_max_secs": 0, "hourly_limit": 3 })).await;
    for i in 0..8 {
        h.queue(account, &format!("lead{i}@gmail.com")).await;
    }
    // One tick per minute for an hour: only 3 go out.
    for _ in 0..58 {
        h.tick().await;
        h.advance(Duration::minutes(1));
    }
    assert_eq!(h.sender.calls().len(), 3);
    // When the first send is more than an hour old, the next one goes.
    h.advance(Duration::minutes(10));
    h.tick().await;
    assert_eq!(h.sender.calls().len(), 4);
}

#[sqlx::test]
async fn engine_pause_stops_everything(pool: PgPool) {
    let h = Harness::new(pool);
    let keys = bootstrap_keys(&h.state).await;
    let k = &keys[&aikido_email::auth::api_keys::Scope::Sending];
    let account = h.account("a@example.com").await;
    h.queue(account, "lead@company.com").await;

    assert_eq!(call(&h.state, "POST", "/v1/sending/engine/pause", Some(k), None).await.status, 200);
    let report = h.tick().await;
    assert!(report.paused);
    assert!(h.sender.calls().is_empty());
    let s = call(&h.state, "GET", "/v1/sending/engine/status", Some(k), None).await;
    assert_eq!(s.body["paused"], true);
    assert_eq!(s.body["jobs"]["queued"], 1);
    assert_eq!(s.body["accounts"]["active"], 1);

    call(&h.state, "POST", "/v1/sending/engine/resume", Some(k), None).await;
    assert_eq!(h.tick().await.sent, 1);
}

#[sqlx::test]
async fn pause_and_resume_one_account(pool: PgPool) {
    let h = Harness::new(pool);
    let keys = bootstrap_keys(&h.state).await;
    let k = &keys[&aikido_email::auth::api_keys::Scope::Sending];
    let account = h.account("a@example.com").await;
    h.queue(account, "lead@company.com").await;
    let r = call(&h.state, "POST", &format!("/v1/sending/accounts/{account}/pause"), Some(k), Some(json!({ "reason": "vacation" }))).await;
    assert_eq!(r.status, 200);
    assert_eq!(h.tick().await.sent, 0);
    let s = call(&h.state, "GET", &format!("/v1/sending/accounts/{account}/status"), Some(k), None).await;
    assert_eq!(s.body["paused"], true);
    assert_eq!(s.body["paused_reason"], "vacation");
    assert_eq!(s.body["account_status"], "paused");
    call(&h.state, "POST", &format!("/v1/sending/accounts/{account}/resume"), Some(k), None).await;
    assert_eq!(h.tick().await.sent, 1);
}

#[sqlx::test]
async fn the_message_has_the_right_headers(pool: PgPool) {
    let h = Harness::new(pool);
    let account = h.account("sender@example.com").await;
    // (This test is about headers, not the follow-up gate: the gate is turned off here.)
    h.settings(account, json!({ "reply_check_max_age_mins": 0, "list_unsubscribe": { "mailto": "unsub@example.com", "url": "https://example.com/u" } })).await;
    let mut job = new_job(account, "lead@company.com");
    job.to_name = Some("Sam".into());
    job.body_html = Some("<p>Hi Sam</p>".into());
    job.in_reply_to = Some("<prev@company.com>".into());
    job.references = Some("<first@company.com> <prev@company.com>".into());
    job.headers = Some([("X-Campaign".to_string(), "c7".to_string())].into());
    job.provider_thread_id = Some("thread-1".into());
    let id = h.queue_job(job).await;
    h.tick().await;

    let sent = &h.sender.calls()[0];
    assert!(sent.message_id.starts_with('<') && sent.message_id.ends_with("@example.com>"));
    assert_eq!(h.job(id).await.message_id_header.as_deref(), Some(sent.message_id.as_str()));
    assert_eq!(sent.thread_id.as_deref(), Some("thread-1"));
    let text = String::from_utf8(sent.build().unwrap().formatted()).unwrap();
    for needle in [
        "multipart/alternative",
        "From: \"Test Sender\" <sender@example.com>",
        "To: Sam <lead@company.com>",
        "In-Reply-To: <prev@company.com>",
        "References: <first@company.com> <prev@company.com>",
        "X-Campaign: c7",
        "List-Unsubscribe: <mailto:unsub@example.com>, <https://example.com/u>",
        "List-Unsubscribe-Post: List-Unsubscribe=One-Click",
        &format!("Message-ID: {}", sent.message_id),
        "text/plain",
        "text/html",
    ] {
        assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
    }
}

#[sqlx::test]
async fn changing_settings_through_the_api(pool: PgPool) {
    let h = Harness::new(pool);
    let keys = bootstrap_keys(&h.state).await;
    let k = &keys[&aikido_email::auth::api_keys::Scope::Sending];
    let account = h.account("a@example.com").await;

    // Invalid values are refused with a clear code.
    for (patch, code) in [
        (json!({ "timezone": "Dhaka" }), "invalid_timezone"),
        (json!({ "send_days": [] }), "invalid_send_days"),
        (json!({ "window_start": "18:00" }), "invalid_window"),
        (json!({ "jitter_min_secs": 9999 }), "invalid_jitter"),
        (json!({ "base_delay_secs": 10 }), "invalid_base_delay"),
        (json!({ "daily_limit": 501 }), "invalid_daily_limit"),
        (json!({ "max_attempts": 9 }), "invalid_retry_backoff"),
        (json!({ "made_up": 1 }), "unknown_setting"),
    ] {
        let r = call(&h.state, "PATCH", &format!("/v1/sending/accounts/{account}/settings"), Some(k), Some(patch.clone())).await;
        assert_eq!(r.status, 422, "{patch}");
        assert_eq!(r.body["error"]["code"], code, "{patch}");
    }
    let r = call(&h.state, "PATCH", &format!("/v1/sending/accounts/{account}/settings"), Some(k), Some(json!({ "timezone": "Asia/Dhaka", "daily_limit": 80 }))).await;
    assert_eq!(r.status, 200);
    assert_eq!(r.body["settings"]["timezone"], "Asia/Dhaka");
    assert_eq!(r.body["settings"]["base_delay_secs"], 300, "other fields are unchanged");
    assert_eq!(r.body["warnings"].as_array().unwrap().len(), 1, "a daily limit above 50 is a warning");

    // New accounts copy the defaults. Old accounts keep their own.
    let d = call(&h.state, "PATCH", "/v1/sending/defaults", Some(k), Some(json!({ "daily_limit": 12, "timezone": "America/New_York" }))).await;
    assert_eq!(d.status, 200);
    let new_account = h.account("b@example.org").await;
    let s = call(&h.state, "GET", &format!("/v1/sending/accounts/{new_account}/settings"), Some(k), None).await;
    assert_eq!(s.body["settings"]["daily_limit"], 12);
    assert_eq!(s.body["settings"]["timezone"], "America/New_York");
    let old = call(&h.state, "GET", &format!("/v1/sending/accounts/{account}/settings"), Some(k), None).await;
    assert_eq!(old.body["settings"]["daily_limit"], 80);
    // Reset copies the defaults onto the account.
    let r = call(&h.state, "POST", &format!("/v1/sending/accounts/{account}/settings/reset"), Some(k), None).await;
    assert_eq!(r.body["settings"]["daily_limit"], 12);
}

#[sqlx::test]
async fn a_settings_change_works_on_the_same_day(pool: PgPool) {
    let h = Harness::new(pool);
    let keys = bootstrap_keys(&h.state).await;
    let k = &keys[&aikido_email::auth::api_keys::Scope::Sending];
    let account = h.account("a@example.com").await;
    h.settings(account, json!({ "daily_limit": 1 })).await;
    h.queue(account, "one@company.com").await;
    h.queue(account, "two@company.com").await;
    h.tick().await;
    h.advance(Duration::minutes(20));
    assert_eq!(h.tick().await.sent, 0, "the limit of 1 is used up");
    // Raise the limit: the account sends again today.
    let r = call(&h.state, "PATCH", &format!("/v1/sending/accounts/{account}/settings"), Some(k), Some(json!({ "daily_limit": 10 }))).await;
    assert_eq!(r.status, 200);
    h.advance(Duration::minutes(1));
    assert_eq!(h.tick().await.sent, 1);
}

#[sqlx::test]
async fn status_and_preview(pool: PgPool) {
    let h = Harness::new(pool);
    let keys = bootstrap_keys(&h.state).await;
    let k = &keys[&aikido_email::auth::api_keys::Scope::Sending];
    let account = h.account("a@example.com").await;
    h.queue(account, "lead@company.com").await;

    let s = call(&h.state, "GET", &format!("/v1/sending/accounts/{account}/status"), Some(k), None).await;
    assert_eq!(s.status, 200, "{}", s.body);
    for field in ["sent_today", "today_limit", "next_send_at", "inside_window", "paused_reason", "wait", "queued_jobs", "local_time", "timezone"] {
        assert!(s.body.get(field).is_some(), "status has no {field}: {}", s.body);
    }
    assert_eq!(s.body["queued_jobs"], 1);
    assert_eq!(s.body["today_limit"], 30);

    let p = call(&h.state, "GET", &format!("/v1/sending/accounts/{account}/preview?count=20"), Some(k), None).await;
    assert_eq!(p.status, 200);
    let times: Vec<chrono::DateTime<chrono::Utc>> = p.body["times"].as_array().unwrap().iter().map(|t| t["at"].as_str().unwrap().parse().unwrap()).collect();
    assert_eq!(times.len(), 20);
    for w in times.windows(2) {
        assert!(w[1] > w[0]);
        if w[0].date_naive() == w[1].date_naive() {
            assert!((420..=900).contains(&(w[1] - w[0]).num_seconds()));
        }
    }
    for t in &times {
        let m = t.hour() * 60 + t.minute();
        assert!(m >= 9 * 60 && m < 17 * 60 && t.weekday().number_from_monday() <= 5, "{t}");
    }
    let bad = call(&h.state, "GET", &format!("/v1/sending/accounts/{account}/preview?count=0"), Some(k), None).await;
    assert_eq!(bad.status, 422);
    // Preview sends nothing and changes nothing.
    assert!(h.sender.calls().is_empty());
    assert_eq!(h.send_state(account).await.sent_today, 0);
}

#[sqlx::test]
async fn dry_run_sends_nothing_but_runs_the_whole_engine(pool: PgPool) {
    use aikido_email::sending::worker::RealSenders;
    let mut h = Harness::new(pool);
    h.engine = Arc::new(Engine::new(h.state.clone(), Arc::new(RealSenders { dry_run: true })));
    let account = h.account("a@example.com").await;
    let job = h.queue(account, "lead@company.com").await;
    assert_eq!(h.tick().await.sent, 1);
    let j = h.job(job).await;
    assert_eq!(j.status, JobStatus::Sent);
    assert!(j.provider_message_id.unwrap().starts_with("dry-run-"));
    assert!(h.sender.calls().is_empty());
}

#[sqlx::test]
async fn a_deleted_account_sends_nothing(pool: PgPool) {
    let h = Harness::new(pool);
    let keys = bootstrap_keys(&h.state).await;
    let account = h.account("a@example.com").await;
    let job = h.queue(account, "lead@company.com").await;
    call(&h.state, "DELETE", &format!("/v1/accounts/{account}"), Some(&keys[&aikido_email::auth::api_keys::Scope::Accounts]), None).await;
    assert_eq!(h.tick().await.sent, 0);
    assert_eq!(h.job(job).await.status, JobStatus::Cancelled);
}

#[sqlx::test]
async fn the_real_message_id_from_the_provider_is_saved_on_the_job(pool: PgPool) {
    let h = Harness::new(pool);
    let a = h.account("a@example.com").await;
    let job = h.queue(a, "lead@company.com").await;
    h.sender.push(Ok(SendReceipt {
        provider_message_id: Some("m1".into()),
        provider_thread_id: None,
        message_id_header: Some("<CAreal123@mail.gmail.com>".into()),
    }));
    h.tick().await;
    let j = h.job(job).await;
    assert_eq!(j.status, JobStatus::Sent);
    // Replies point to the id the recipient saw, so this is the one the reply check must match.
    assert_eq!(j.message_id_header.as_deref(), Some("<CAreal123@mail.gmail.com>"));

    // No replacement: the id we made stays.
    let job2 = h.queue(a, "other@company2.com").await;
    h.advance(Duration::minutes(20));
    h.tick().await;
    let j2 = h.job(job2).await;
    assert!(j2.message_id_header.as_deref().unwrap().ends_with("@example.com>"));
}
