//! UI build map B6: daily numbers and the account table.
mod common;
use aikido_email::auth::api_keys::Scope;
use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

async fn key(state: &aikido_email::state::AppState) -> String {
    aikido_email::auth::api_keys::create(&state.pool, Scope::Campaigns, "t").await.unwrap().1
}

async fn inbound(state: &aikido_email::state::AppState, account: Uuid, campaign: Option<Uuid>, from: &str, label: &str, at: chrono::DateTime<chrono::Utc>) {
    sqlx::query(
        "INSERT INTO inbound_messages (id, account_id, folder, provider_message_id, from_email, received_at, label, match_method, campaign_id)
         VALUES ($1, $2, 'inbox', $1::text, $3, $4, $5::inbound_label, 'none', $6)",
    )
    .bind(Uuid::now_v7())
    .bind(account)
    .bind(from)
    .bind(at)
    .bind(label)
    .bind(campaign)
    .execute(&state.pool)
    .await
    .unwrap();
}

fn day<'a>(body: &'a Value, date: &str) -> &'a Value {
    body["days"].as_array().unwrap().iter().find(|d| d["date"] == date).unwrap_or_else(|| panic!("no {date}"))
}

#[sqlx::test]
async fn daily_counts_fill_empty_days_and_cut_days_in_the_zone(pool: PgPool) {
    let state = app_state(pool);
    let k = key(&state).await;
    let a = add_account(&state, "a@example.com").await;
    // 23:30 UTC on the 19th is already the 20th in Tokyo
    sent_job(&state, a, "p1@acme.com", "<1@x>", None, utc(2026, 10, 19, 23, 30)).await;
    sent_job(&state, a, "p2@acme.com", "<2@x>", None, utc(2026, 10, 19, 10, 0)).await;
    sent_job(&state, a, "p3@acme.com", "<3@x>", None, utc(2026, 10, 21, 9, 0)).await;
    // a job that did not go out is not counted
    sqlx::query("UPDATE send_jobs SET status = 'failed' WHERE message_id_header = '<3@x>'").execute(&state.pool).await.unwrap();
    inbound(&state, a, None, "p1@acme.com", "reply", utc(2026, 10, 19, 12, 0)).await;
    inbound(&state, a, None, "p1@acme.com", "reply", utc(2026, 10, 19, 14, 0)).await; // same person, same day: once
    inbound(&state, a, None, "p2@acme.com", "reply", utc(2026, 10, 19, 15, 0)).await;
    inbound(&state, a, None, "p2@acme.com", "auto_reply", utc(2026, 10, 20, 8, 0)).await;
    inbound(&state, a, None, "bad@acme.com", "bounce_hard", utc(2026, 10, 20, 9, 0)).await;
    inbound(&state, a, None, "soft@acme.com", "bounce_soft", utc(2026, 10, 20, 9, 0)).await; // not counted
    inbound(&state, a, None, "u@acme.com", "unsubscribe", utc(2026, 10, 20, 10, 0)).await;
    inbound(&state, a, None, "c@acme.com", "complaint", utc(2026, 10, 20, 11, 0)).await;

    let r = call(&state, "GET", "/v1/reports/daily?from=2026-10-18&to=2026-10-22&tz=UTC", Some(&k), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["tz"], "UTC");
    assert_eq!(r.body["days"].as_array().unwrap().len(), 5, "every day is there, also the empty ones");
    let d19 = day(&r.body, "2026-10-19");
    assert_eq!((d19["sent"].as_i64(), d19["replied"].as_i64()), (Some(2), Some(2)));
    let d20 = day(&r.body, "2026-10-20");
    assert_eq!(
        (d20["sent"].as_i64(), d20["auto_replied"].as_i64(), d20["bounced"].as_i64(), d20["unsubscribed"].as_i64(), d20["complained"].as_i64()),
        (Some(0), Some(1), Some(1), Some(1), Some(1))
    );
    assert_eq!(day(&r.body, "2026-10-18")["sent"], 0);
    assert_eq!(day(&r.body, "2026-10-21")["sent"], 0, "the failed job");
    assert_eq!(r.body["totals"]["sent"], 2);
    assert_eq!(r.body["totals"]["replied"], 2);

    // Tokyo: the 23:30 UTC email belongs to the 20th
    let t = call(&state, "GET", "/v1/reports/daily?from=2026-10-19&to=2026-10-20&tz=Asia/Tokyo", Some(&k), None).await;
    assert_eq!(day(&t.body, "2026-10-19")["sent"], 1);
    assert_eq!(day(&t.body, "2026-10-20")["sent"], 1);
    assert_eq!(t.body["totals"]["sent"], 2);
}

#[sqlx::test]
async fn the_default_zone_is_the_sending_default(pool: PgPool) {
    let state = app_state(pool);
    let k = key(&state).await;
    sqlx::query("UPDATE sending_defaults SET settings = '{\"timezone\": \"Asia/Tokyo\"}'").execute(&state.pool).await.unwrap();
    let a = add_account(&state, "a@example.com").await;
    sent_job(&state, a, "p1@acme.com", "<1@x>", None, utc(2026, 10, 19, 23, 30)).await;
    let r = call(&state, "GET", "/v1/reports/daily?from=2026-10-20&to=2026-10-20", Some(&k), None).await;
    assert_eq!(r.body["tz"], "Asia/Tokyo");
    assert_eq!(r.body["days"][0]["sent"], 1);
}

#[sqlx::test]
async fn daily_filters_by_campaign_and_account(pool: PgPool) {
    let state = app_state(pool);
    let k = key(&state).await;
    let (a, b) = (add_account(&state, "a@example.com").await, add_account(&state, "b@example.com").await);
    let camp = Uuid::now_v7();
    let j = sent_job(&state, a, "p1@acme.com", "<1@x>", None, utc(2026, 10, 19, 10, 0)).await;
    sqlx::query("UPDATE send_jobs SET campaign_id = $2 WHERE id = $1").bind(j).bind(camp).execute(&state.pool).await.unwrap();
    sent_job(&state, b, "p2@acme.com", "<2@x>", None, utc(2026, 10, 19, 11, 0)).await;
    inbound(&state, a, Some(camp), "p1@acme.com", "reply", utc(2026, 10, 19, 12, 0)).await;
    inbound(&state, b, None, "p2@acme.com", "reply", utc(2026, 10, 19, 12, 0)).await;

    let q = |extra: &str| format!("/v1/reports/daily?from=2026-10-19&to=2026-10-19&tz=UTC{extra}");
    let all = call(&state, "GET", &q(""), Some(&k), None).await.body;
    assert_eq!((all["totals"]["sent"].as_i64(), all["totals"]["replied"].as_i64()), (Some(2), Some(2)));
    let one = call(&state, "GET", &q(&format!("&campaign_id={camp}")), Some(&k), None).await.body;
    assert_eq!((one["totals"]["sent"].as_i64(), one["totals"]["replied"].as_i64()), (Some(1), Some(1)));
    let acct = call(&state, "GET", &q(&format!("&account_id={b}")), Some(&k), None).await.body;
    assert_eq!((acct["totals"]["sent"].as_i64(), acct["totals"]["replied"].as_i64()), (Some(1), Some(1)));
    let none = call(&state, "GET", &q(&format!("&campaign_id={}", Uuid::now_v7())), Some(&k), None).await.body;
    assert_eq!(none["totals"]["sent"], 0);
}

#[sqlx::test]
async fn daily_counts_new_leads_enrolled(pool: PgPool) {
    let state = app_state(pool);
    let k = key(&state).await;
    let a = add_account(&state, "a@example.com").await;
    sqlx::query("UPDATE email_accounts SET can_read = true").execute(&state.pool).await.unwrap();
    let body = json!({ "name": "Q4", "sequence": { "name": "S", "steps": [{ "delay_amount": 0, "subject": "Hi", "body_text": "Hello" }] }, "account_ids": [a] });
    let camp: Uuid = call(&state, "POST", "/v1/campaigns", Some(&k), Some(body)).await.body["id"].as_str().unwrap().parse().unwrap();
    for (i, at) in [utc(2026, 10, 19, 9, 0), utc(2026, 10, 19, 9, 5), utc(2026, 10, 20, 9, 0)].into_iter().enumerate() {
        let lead = Uuid::now_v7();
        sqlx::query("INSERT INTO leads (id, email, email_domain) VALUES ($1, $2, 'acme.com')").bind(lead).bind(format!("l{i}@acme.com")).execute(&state.pool).await.unwrap();
        let en = Uuid::now_v7();
        sqlx::query("INSERT INTO enrollments (id, sequence_id, account_id, to_email, lead_id) SELECT $1, sequence_id, $2, $3, $4 FROM campaigns WHERE id = $5")
            .bind(en).bind(a).bind(format!("l{i}@acme.com")).bind(lead).bind(camp).execute(&state.pool).await.unwrap();
        sqlx::query("INSERT INTO campaign_leads (campaign_id, lead_id, state, enrollment_id, enrolled_at) VALUES ($1, $2, 'enrolled', $3, $4)")
            .bind(camp).bind(lead).bind(en).bind(at).execute(&state.pool).await.unwrap();
    }
    let r = call(&state, "GET", &format!("/v1/reports/daily?from=2026-10-19&to=2026-10-20&tz=UTC&campaign_id={camp}"), Some(&k), None).await.body;
    assert_eq!(day(&r, "2026-10-19")["new_leads_enrolled"], 2);
    assert_eq!(day(&r, "2026-10-20")["new_leads_enrolled"], 1);
    let other = call(&state, "GET", &format!("/v1/reports/daily?from=2026-10-19&to=2026-10-20&tz=UTC&account_id={}", Uuid::now_v7()), Some(&k), None).await.body;
    assert_eq!(other["totals"]["new_leads_enrolled"], 0);
}

#[sqlx::test]
async fn daily_checks_its_input(pool: PgPool) {
    let state = app_state(pool);
    let k = key(&state).await;
    let get = |q: &'static str| {
        let (state, k) = (state.clone(), k.clone());
        async move { call(&state, "GET", &format!("/v1/reports/daily{q}"), Some(&k), None).await }
    };
    // 366 days is the most
    assert_eq!(get("?from=2026-01-01&to=2026-12-31&tz=UTC").await.status, StatusCode::OK);
    let long = get("?from=2026-01-01&to=2027-01-02&tz=UTC").await;
    assert_eq!((long.status, long.body["error"]["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("range_too_long")));
    assert_eq!(get("?from=2026-02-02&to=2026-02-01").await.body["error"]["code"], "invalid_range");
    assert_eq!(get("?from=2026-02-01&to=2026-02-02&tz=Mars/Base").await.body["error"]["code"], "invalid_timezone");
    assert_eq!(get("?from=nope&to=2026-02-02").await.status, StatusCode::BAD_REQUEST);
    assert_eq!(get("?to=2026-02-02").await.status, StatusCode::BAD_REQUEST);
    // a key of another scope is refused
    let other = aikido_email::auth::api_keys::create(&state.pool, Scope::Leads, "t").await.unwrap().1;
    assert_eq!(call(&state, "GET", "/v1/reports/daily?from=2026-02-01&to=2026-02-02", Some(&other), None).await.status, StatusCode::FORBIDDEN);
    // a daylight saving day (23 hours) works
    assert_eq!(get("?from=2026-03-29&to=2026-03-29&tz=Europe/Berlin").await.body["days"].as_array().unwrap().len(), 1);
}

#[sqlx::test]
async fn account_table_has_rates_and_the_bounce_limit(pool: PgPool) {
    let state = app_state(pool);
    let k = key(&state).await;
    let (a, b) = (add_account(&state, "a@example.com").await, add_account(&state, "b@example.com").await);
    for (i, to) in ["p1@acme.com", "p2@acme.com", "p3@acme.com", "p4@acme.com"].into_iter().enumerate() {
        sent_job(&state, a, to, &format!("<{i}@x>"), None, utc(2026, 10, 19, 10, i as u32)).await;
    }
    sent_job(&state, a, "p1@acme.com", "<again@x>", None, utc(2026, 10, 19, 12, 0)).await; // 5 sent, 4 people
    inbound(&state, a, None, "p1@acme.com", "reply", utc(2026, 10, 19, 13, 0)).await;
    inbound(&state, a, None, "p9@acme.com", "bounce_hard", utc(2026, 10, 19, 13, 0)).await;
    sqlx::query("UPDATE email_accounts SET status = 'error' WHERE id = $1").bind(b).execute(&state.pool).await.unwrap();

    let r = call(&state, "GET", "/v1/reports/accounts?from=2026-10-19&to=2026-10-19&tz=UTC", Some(&k), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["bounce_limit_pct"], 5);
    let rows = r.body["accounts"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["email"], "a@example.com");
    assert_eq!((rows[0]["sent"].as_i64(), rows[0]["contacted"].as_i64()), (Some(5), Some(4)));
    assert_eq!(rows[0]["reply_rate"], 0.25);
    assert_eq!(rows[0]["bounce_rate"], 0.25);
    assert_eq!(rows[1]["status"], "error");
    assert_eq!((rows[1]["sent"].as_i64(), rows[1]["reply_rate"].as_f64()), (Some(0), Some(0.0)), "no division by zero");
    // outside the range: nothing
    let other = call(&state, "GET", "/v1/reports/accounts?from=2026-10-20&to=2026-10-21&tz=UTC", Some(&k), None).await;
    assert_eq!(other.body["accounts"][0]["sent"], 0);
}
