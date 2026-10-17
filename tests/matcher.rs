mod common;
use aikido_email::auth::api_keys::Scope;
use aikido_email::replies::matcher::{MatchInput, find_match};
use aikido_email::replies::model::MatchMethod;
use axum::http::StatusCode;
use base64::Engine;
use common::*;
use serde_json::json;
use sqlx::PgPool;

fn input<'a>(account: Option<uuid::Uuid>, from: &'a str) -> MatchInput<'a> {
    MatchInput { account_id: account, from_email: from, match_window_days: 30, ..Default::default() }
}

#[sqlx::test]
async fn in_reply_to_wins_over_sender_address(pool: PgPool) {
    let st = app_state(pool);
    let a = add_account(&st, "a@example.com").await;
    let now = utc(2026, 10, 20, 12, 0);
    // The lead got two emails. The reply points to the first one.
    let first = sent_job(&st, a, "jane@prospect.example", "<m1@example.com>", None, now - chrono::Duration::days(3)).await;
    let _second = sent_job(&st, a, "jane@prospect.example", "<m2@example.com>", None, now - chrono::Duration::days(1)).await;

    let mut conn = st.pool.acquire().await.unwrap();
    let mut i = input(Some(a), "jane@prospect.example");
    let r = find_match(&mut conn, &i, now).await.unwrap();
    assert_eq!(r.method, MatchMethod::SenderAddress);
    assert_ne!(r.job.unwrap().id, first, "sender address takes the newest send");

    i.in_reply_to = Some("<m1@example.com>");
    let r = find_match(&mut conn, &i, now).await.unwrap();
    assert_eq!(r.method, MatchMethod::InReplyTo);
    assert_eq!(r.job.unwrap().id, first);
}

#[sqlx::test]
async fn the_order_is_dsn_then_in_reply_to_then_references_then_thread_then_sender(pool: PgPool) {
    let st = app_state(pool);
    let a = add_account(&st, "a@example.com").await;
    let now = utc(2026, 10, 20, 12, 0);
    let j_dsn = sent_job(&st, a, "x1@p.example", "<dsn@example.com>", None, now - chrono::Duration::days(1)).await;
    let j_irt = sent_job(&st, a, "x2@p.example", "<irt@example.com>", None, now - chrono::Duration::days(1)).await;
    let j_ref_old = sent_job(&st, a, "x3@p.example", "<ref-old@example.com>", None, now - chrono::Duration::days(5)).await;
    let j_ref_new = sent_job(&st, a, "x3@p.example", "<ref-new@example.com>", None, now - chrono::Duration::days(2)).await;
    let j_thread = sent_job(&st, a, "x4@p.example", "<th@example.com>", Some("thread-1"), now - chrono::Duration::days(1)).await;
    let j_sender = sent_job(&st, a, "who@p.example", "<s@example.com>", None, now - chrono::Duration::days(1)).await;
    let mut conn = st.pool.acquire().await.unwrap();

    let refs = vec!["<ref-old@example.com>".to_string(), "<ref-new@example.com>".to_string()];
    let all = MatchInput {
        account_id: Some(a),
        from_email: "who@p.example",
        in_reply_to: Some("<irt@example.com>"),
        references: &refs,
        thread_id: Some("thread-1"),
        original_message_id: Some("<dsn@example.com>"),
        bounce_recipient: None,
        bounce_candidates: &[],
        match_window_days: 30,
    };
    let r = find_match(&mut conn, &all, now).await.unwrap();
    assert_eq!((r.method, r.job.unwrap().id), (MatchMethod::DsnOriginal, j_dsn));

    let i = MatchInput { original_message_id: None, ..all.clone() };
    let r = find_match(&mut conn, &i, now).await.unwrap();
    assert_eq!((r.method, r.job.unwrap().id), (MatchMethod::InReplyTo, j_irt));

    let i = MatchInput { in_reply_to: None, ..i };
    let r = find_match(&mut conn, &i, now).await.unwrap();
    assert_eq!((r.method, r.job.unwrap().id), (MatchMethod::References, j_ref_new), "newest job in References");
    let _ = j_ref_old;

    let i = MatchInput { references: &[], ..i };
    let r = find_match(&mut conn, &i, now).await.unwrap();
    assert_eq!((r.method, r.job.unwrap().id), (MatchMethod::Thread, j_thread));

    let i = MatchInput { thread_id: None, ..i };
    let r = find_match(&mut conn, &i, now).await.unwrap();
    assert_eq!((r.method, r.job.unwrap().id), (MatchMethod::SenderAddress, j_sender));
}

#[sqlx::test]
async fn a_reply_all_that_lands_in_another_account_matches_by_message_id(pool: PgPool) {
    let st = app_state(pool);
    let a = add_account(&st, "a@example.com").await;
    let b = add_account(&st, "b@example.com").await;
    let now = utc(2026, 10, 20, 12, 0);
    let job = sent_job(&st, a, "jane@prospect.example", "<m1@example.com>", Some("t1"), now - chrono::Duration::days(1)).await;
    let mut conn = st.pool.acquire().await.unwrap();

    // Tom (a colleague) replies to all. The mail lands in inbox B.
    let mut i = input(Some(b), "tom@prospect.example");
    i.in_reply_to = Some("<m1@example.com>");
    let r = find_match(&mut conn, &i, now).await.unwrap();
    assert_eq!(r.method, MatchMethod::InReplyTo);
    assert_eq!(r.job.unwrap().id, job);

    // The weak methods stay inside the account: B never sent to Jane, and the thread is A's.
    let mut i = input(Some(b), "jane@prospect.example");
    i.thread_id = Some("t1");
    assert_eq!(find_match(&mut conn, &i, now).await.unwrap().method, MatchMethod::None);
}

#[sqlx::test]
async fn windows_limit_the_weak_methods(pool: PgPool) {
    let st = app_state(pool);
    let a = add_account(&st, "a@example.com").await;
    let now = utc(2026, 10, 20, 12, 0);
    sent_job(&st, a, "old@p.example", "<o@example.com>", None, now - chrono::Duration::days(31)).await;
    sent_job(&st, a, "recent@p.example", "<r@example.com>", None, now - chrono::Duration::days(29)).await;
    let mut conn = st.pool.acquire().await.unwrap();
    // 30 day window for the sender address.
    assert_eq!(find_match(&mut conn, &input(Some(a), "old@p.example"), now).await.unwrap().method, MatchMethod::None);
    assert_eq!(find_match(&mut conn, &input(Some(a), "recent@p.example"), now).await.unwrap().method, MatchMethod::SenderAddress);
    // 7 day window for the bounce recipient.
    let mut i = input(Some(a), "mailer-daemon@x.example");
    i.bounce_recipient = Some("recent@p.example");
    assert_eq!(find_match(&mut conn, &i, now).await.unwrap().method, MatchMethod::None);
    sent_job(&st, a, "fresh@p.example", "<f@example.com>", None, now - chrono::Duration::days(2)).await;
    i.bounce_recipient = Some("fresh@p.example");
    assert_eq!(find_match(&mut conn, &i, now).await.unwrap().method, MatchMethod::BounceRecipient);
}

#[sqlx::test]
async fn an_old_style_bounce_picks_the_body_address_we_sent_to(pool: PgPool) {
    let st = app_state(pool);
    let a = add_account(&st, "a@example.com").await;
    let now = utc(2026, 10, 20, 12, 0);
    sent_job(&st, a, "jane@prospect.example", "<m1@example.com>", None, now - chrono::Duration::hours(2)).await;
    let mut conn = st.pool.acquire().await.unwrap();
    let candidates = vec!["admin@mx2.prospect.example".to_string(), "jane@prospect.example".to_string()];
    let mut i = input(Some(a), "mailer-daemon@mx2.prospect.example");
    i.bounce_candidates = &candidates;
    let r = find_match(&mut conn, &i, now).await.unwrap();
    assert_eq!(r.method, MatchMethod::BounceRecipient);
    assert_eq!(r.resolved_recipient.as_deref(), Some("jane@prospect.example"));
}

#[sqlx::test]
async fn an_unrelated_email_gets_no_match(pool: PgPool) {
    let st = app_state(pool);
    let a = add_account(&st, "a@example.com").await;
    sent_job(&st, a, "jane@prospect.example", "<m1@example.com>", None, utc(2026, 10, 19, 10, 0)).await;
    let mut conn = st.pool.acquire().await.unwrap();
    let mut i = input(Some(a), "newsletter@shop.example");
    let refs = vec!["<other@elsewhere.example>".to_string()];
    i.references = &refs;
    i.in_reply_to = Some("<other@elsewhere.example>");
    let r = find_match(&mut conn, &i, utc(2026, 10, 20, 12, 0)).await.unwrap();
    assert_eq!(r.method, MatchMethod::None);
    assert!(r.job.is_none());
}

#[sqlx::test]
async fn the_classify_route_labels_and_matches_but_writes_nothing(pool: PgPool) {
    let st = app_state(pool);
    let keys = bootstrap_keys(&st).await;
    let key = &keys[&Scope::Replies];
    let a = add_account(&st, "sender@acme-outreach.example").await;
    let job = sent_job(&st, a, "jane@prospect.example", "<sent-0001@acme-outreach.example>", None, chrono::Utc::now() - chrono::Duration::hours(3)).await;
    let b64 = |name: &str| base64::engine::general_purpose::STANDARD.encode(fixture(name));

    let r = call(&st, "POST", "/v1/replies/classify", Some(key), Some(json!({ "raw": b64("human_gmail_quote.eml") }))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["label"], "reply");
    assert_eq!(r.body["matched"]["method"], "in_reply_to");
    assert_eq!(r.body["matched"]["job_id"], job.to_string());
    assert_eq!(r.body["reply_text"], "Sounds good, let's talk Thursday.");
    assert_eq!(r.body["would_be_dropped"], false);

    let r = call(&st, "POST", "/v1/replies/classify", Some(key), Some(json!({ "raw": b64("gmail_bounce_user_unknown.eml"), "account_id": a }))).await;
    assert_eq!(r.body["label"], "bounce_hard");
    assert_eq!(r.body["matched"]["method"], "dsn_original");
    assert_eq!(r.body["bounce"]["status"], "5.1.1");
    assert_eq!(r.body["bounce"]["recipient"], "nobody@prospect.example");

    // An email that does not link to our sends.
    let loose = "From: News <news@shop.example>\nTo: sender@acme-outreach.example\nSubject: Sale\nMessage-ID: <n1@shop.example>\n\nbig sale";
    let r = call(&st, "POST", "/v1/replies/classify", Some(key), Some(json!({ "raw": base64::engine::general_purpose::STANDARD.encode(loose) }))).await;
    assert_eq!(r.body["label"], "reply");
    assert_eq!(r.body["matched"]["method"], "none");
    assert_eq!(r.body["would_be_dropped"], true);

    // Bad input.
    let r = call(&st, "POST", "/v1/replies/classify", Some(key), Some(json!({ "raw": "***" }))).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = call(&st, "POST", "/v1/replies/classify", Some(key), Some(json!({ "raw": base64::engine::general_purpose::STANDARD.encode("not an email") }))).await;
    assert_eq!(r.body["label"], "unknown");

    // Nothing was written.
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM inbound_messages").fetch_one(&st.pool).await.unwrap();
    assert_eq!(n, 0);
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM suppressions").fetch_one(&st.pool).await.unwrap();
    assert_eq!(n, 0);
}
