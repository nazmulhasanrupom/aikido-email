//! Follow-up automation: sequences, enrollments, the scheduler, threading, stops and pauses.
mod common;
use aikido_email::auth::api_keys::Scope;
use aikido_email::followups::scheduler::{self, FollowupEngine, TickReport};
use aikido_email::sending::queue::JobStatus;
use aikido_email::sending::senders::SendError;
use axum::http::StatusCode;
use chrono::{DateTime, Duration, Utc};
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

/// The sending engine, the follow-up scheduler and a key for the API. One fake clock for all.
struct F {
    h: Harness,
    fe: Arc<FollowupEngine>,
    keys: HashMap<Scope, String>,
}

impl F {
    async fn new(pool: PgPool) -> Self {
        let h = Harness::new(pool);
        let keys = bootstrap_keys(&h.state).await;
        let fe = Arc::new(FollowupEngine::new(h.state.clone()));
        Self { h, fe, keys }
    }

    /// An account for tests. The inbox gate is off, so follow-ups do not wait for an inbox sync.
    async fn account(&self, email: &str) -> Uuid {
        let a = self.h.account(email).await;
        self.h.settings(a, json!({ "reply_check_max_age_mins": 0 })).await;
        a
    }

    async fn api(&self, method: &str, uri: &str, body: Option<Value>) -> Response {
        call(&self.h.state, method, uri, Some(&self.keys[&Scope::Followups]), body).await
    }

    async fn fu_tick(&self) -> TickReport {
        scheduler::tick(&self.fe).await.unwrap()
    }

    /// A sequence. Each delay is a step (the first must be 0). Subject: `Hello {{first_name|there}}`.
    async fn sequence(&self, delays: &[i32]) -> Uuid {
        let steps: Vec<Value> = delays
            .iter()
            .enumerate()
            .map(|(i, d)| {
                json!({
                    "delay_amount": d,
                    "subject": if i == 0 { "Hello {{first_name|there}}".to_string() } else { String::new() },
                    "body_text": format!("Step {} for {{{{first_name|friend}}}}", i + 1),
                })
            })
            .collect();
        self.sequence_from(json!({ "name": "Test sequence", "steps": steps })).await
    }

    async fn sequence_from(&self, body: Value) -> Uuid {
        let r = self.api("POST", "/v1/followups/sequences", Some(body)).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        r.body["sequence"]["id"].as_str().unwrap().parse().unwrap()
    }

    async fn enroll(&self, seq: Uuid, account: Uuid, email: &str) -> Uuid {
        self.enroll_with(seq, account, email, json!({ "first_name": "Ana" })).await
    }

    async fn enroll_with(&self, seq: Uuid, account: Uuid, email: &str, variables: Value) -> Uuid {
        let r = self
            .api(
                "POST",
                &format!("/v1/followups/sequences/{seq}/enrollments"),
                Some(json!({ "enrollments": [{ "to_email": email, "account_id": account, "variables": variables }] })),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        assert_eq!(r.body["enrolled"], 1, "{}", r.body);
        r.body["items"][0]["enrollment"]["id"].as_str().unwrap().parse().unwrap()
    }

    async fn detail(&self, id: Uuid) -> Value {
        let r = self.api("GET", &format!("/v1/followups/enrollments/{id}"), None).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        r.body
    }

    async fn status(&self, id: Uuid) -> String {
        self.detail(id).await["enrollment"]["status"].as_str().unwrap().to_string()
    }

    async fn reason(&self, id: Uuid) -> Option<String> {
        self.detail(id).await["enrollment"]["stop_reason"].as_str().map(String::from)
    }

    /// The send jobs of an enrollment, step 1 first.
    async fn jobs(&self, id: Uuid) -> Vec<Uuid> {
        let rows: Vec<(Uuid,)> = sqlx::query_as("SELECT id FROM send_jobs WHERE enrollment_id = $1 ORDER BY step_number")
            .bind(id)
            .fetch_all(&self.h.state.pool)
            .await
            .unwrap();
        rows.into_iter().map(|r| r.0).collect()
    }

    /// One round: the follow-up scheduler, then the sending engine, then the clock moves on.
    async fn round(&self, step: Duration) {
        self.fu_tick().await;
        self.h.tick().await;
        self.h.advance(step);
    }

    /// Rounds until `done` holds (at most 1500 rounds of 10 minutes).
    async fn pump_until<Fut: std::future::Future<Output = bool>>(&self, what: &str, mut done: impl FnMut() -> Fut) {
        for _ in 0..1500 {
            if done().await {
                return;
            }
            self.round(Duration::minutes(10)).await;
        }
        panic!("never happened: {what}");
    }

    /// Run until step `n` of the enrollment is sent.
    async fn until_sent(&self, e: Uuid, n: usize) -> Uuid {
        self.pump_until(&format!("step {n} sent"), || async move {
            let jobs = self.jobs(e).await;
            match jobs.get(n - 1) {
                Some(j) => self.h.job(*j).await.status == JobStatus::Sent,
                None => false,
            }
        })
        .await;
        self.jobs(e).await[n - 1]
    }

    async fn event_kinds(&self, id: Uuid) -> Vec<String> {
        self.detail(id).await["events"].as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap().to_string()).collect()
    }

    async fn set_message_id(&self, job: Uuid, id: &str) {
        sqlx::query("UPDATE send_jobs SET message_id_header = $2 WHERE id = $1").bind(job).bind(id).execute(&self.h.state.pool).await.unwrap();
    }

    async fn ingest(&self, account: Uuid, provider_id: &str, raw: &[u8]) {
        ingest_raw(&self.h.state, account, provider_id, raw, self.h.now()).await;
    }
}

/// The id that every fixture in `tests/fixtures/inbound` points to.
const FIXTURE_ID: &str = "<sent-0001@acme-outreach.example>";
/// The lead of those fixtures.
const JANE: &str = "jane@prospect.example";

fn hours(n: i64) -> Duration {
    Duration::hours(n)
}

// =====================================================================================================
// Phase 1: scope, settings, migration
// =====================================================================================================

#[sqlx::test]
async fn the_followups_scope_guards_every_route(pool: PgPool) {
    let f = F::new(pool).await;
    let st = &f.h.state;
    assert_eq!(call(st, "GET", "/v1/followups/status", None, None).await.status, StatusCode::UNAUTHORIZED);
    let wrong = call(st, "GET", "/v1/followups/status", Some(&f.keys[&Scope::Sending]), None).await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);
    assert!(wrong.body["error"]["message"].as_str().unwrap().contains("followups"));
    // The followups key does not open the other steps. The admin key opens this one.
    assert_eq!(call(st, "GET", "/v1/sending/engine/status", Some(&f.keys[&Scope::Followups]), None).await.status, StatusCode::FORBIDDEN);
    assert_eq!(call(st, "GET", "/v1/followups/status", Some(&f.keys[&Scope::Admin]), None).await.status, StatusCode::OK);
    assert_eq!(f.api("GET", "/v1/followups/status", None).await.status, StatusCode::OK);
}

#[sqlx::test]
async fn settings_have_defaults_and_bad_values_get_422(pool: PgPool) {
    let f = F::new(pool).await;
    let r = f.api("GET", "/v1/followups/settings", None).await;
    assert_eq!(r.body["settings"], json!({ "one_active_per_email": true, "max_wait_days": 7, "tick_secs": 30 }));
    let r = f.api("PATCH", "/v1/followups/settings", Some(json!({ "max_wait_days": 14 }))).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["settings"]["max_wait_days"], 14);
    assert_eq!(r.body["settings"]["tick_secs"], 30);
    for bad in [json!({ "max_wait_days": 0 }), json!({ "tick_secs": 1 }), json!({ "nope": 1 }), json!({ "tick_secs": "fast" })] {
        let r = f.api("PATCH", "/v1/followups/settings", Some(bad.clone())).await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
        assert!(r.body["error"]["code"].is_string());
    }
    // A bad patch changed nothing.
    assert_eq!(f.api("GET", "/v1/followups/settings", None).await.body["settings"]["max_wait_days"], 14);
}

#[sqlx::test]
async fn the_pause_flag_is_saved_in_the_database(pool: PgPool) {
    let f = F::new(pool).await;
    assert_eq!(f.api("GET", "/v1/followups/status", None).await.body["paused"], false);
    assert_eq!(f.api("POST", "/v1/followups/pause", None).await.body["paused"], true);
    assert_eq!(f.api("GET", "/v1/followups/status", None).await.body["paused"], true);
    assert_eq!(f.api("POST", "/v1/followups/resume", None).await.body["paused"], false);
}

// =====================================================================================================
// Phase 2: sequences, steps, preview
// =====================================================================================================

fn step(delay: i32, subject: &str, body: &str) -> Value {
    json!({ "delay_amount": delay, "subject": subject, "body_text": body })
}

#[sqlx::test]
async fn a_sequence_is_made_with_its_steps(pool: PgPool) {
    let f = F::new(pool).await;
    let r = f
        .api(
            "POST",
            "/v1/followups/sequences",
            Some(json!({
                "name": "  Cold outreach  ",
                "steps": [
                    step(0, "Hi {{first_name|there}}", "First"),
                    { "delay_amount": 3, "delay_type": "calendar_days", "body_html": "<p>Second</p>" },
                    step(4, "Last one", "Third"),
                ]
            })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["sequence"]["name"], "Cold outreach");
    assert_eq!(r.body["sequence"]["status"], "active");
    assert_eq!(r.body["sequence"]["stop_on_auto_reply"], false);
    assert_eq!(r.body["sequence"]["auto_reply_pause_days"], 3);
    assert_eq!(r.body["sequence"]["thread_followups"], true);
    let steps = r.body["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 3);
    assert_eq!(steps[0]["step_number"], 1);
    assert_eq!(steps[0]["delay_type"], "business_days");
    assert_eq!(steps[1]["delay_type"], "calendar_days");
    assert_eq!(steps[1]["subject"], "");
    assert_eq!(steps[2]["delay_amount"], 4);
    let id = r.body["sequence"]["id"].as_str().unwrap();
    let got = f.api("GET", &format!("/v1/followups/sequences/{id}"), None).await;
    assert_eq!(got.status, StatusCode::OK);
    assert_eq!(got.body["steps"].as_array().unwrap().len(), 3);
    assert_eq!(got.body["counts"], json!({}));
    assert_eq!(f.api("GET", &format!("/v1/followups/sequences/{}", Uuid::now_v7()), None).await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn bad_sequences_get_422(pool: PgPool) {
    let f = F::new(pool).await;
    let long: Vec<Value> = (0..11).map(|i| step(if i == 0 { 0 } else { 1 }, "S", "B")).collect();
    let cases: Vec<(&str, Value, &str)> = vec![
        ("no steps", json!({ "name": "x", "steps": [] }), "invalid_steps"),
        ("11 steps", json!({ "name": "x", "steps": long }), "invalid_steps"),
        ("empty name", json!({ "name": "  ", "steps": [step(0, "S", "B")] }), "invalid_name"),
        ("step 1 without subject", json!({ "name": "x", "steps": [step(0, "", "B")] }), "invalid_steps"),
        ("step 1 with a delay", json!({ "name": "x", "steps": [step(2, "S", "B")] }), "invalid_steps"),
        ("step 2 without a delay", json!({ "name": "x", "steps": [step(0, "S", "B"), { "body_text": "B" }] }), "invalid_steps"),
        ("step 2 with delay 0", json!({ "name": "x", "steps": [step(0, "S", "B"), step(0, "", "B")] }), "invalid_steps"),
        ("step 2 with delay 366", json!({ "name": "x", "steps": [step(0, "S", "B"), step(366, "", "B")] }), "invalid_steps"),
        ("no body", json!({ "name": "x", "steps": [{ "subject": "S" }] }), "invalid_steps"),
        ("blank body", json!({ "name": "x", "steps": [{ "subject": "S", "body_text": "  " }] }), "invalid_steps"),
        ("subject with a line break", json!({ "name": "x", "steps": [step(0, "S\nBcc: x@y.com", "B")] }), "invalid_steps"),
        ("open brace in the body", json!({ "name": "x", "steps": [step(0, "S", "Hi {{name")] }), "invalid_template"),
        ("bad name in the subject", json!({ "name": "x", "steps": [step(0, "Hi {{first name}}", "B")] }), "invalid_template"),
        ("pause days 0", json!({ "name": "x", "auto_reply_pause_days": 0, "steps": [step(0, "S", "B")] }), "invalid_setting"),
        ("pause days 31", json!({ "name": "x", "auto_reply_pause_days": 31, "steps": [step(0, "S", "B")] }), "invalid_setting"),
        ("bad delay type", json!({ "name": "x", "steps": [{ "delay_type": "weeks", "subject": "S", "body_text": "B" }] }), "invalid_json"),
    ];
    for (what, body, code) in cases {
        let r = f.api("POST", "/v1/followups/sequences", Some(body)).await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{what}: {}", r.body);
        assert_eq!(r.body["error"]["code"], code, "{what}: {}", r.body);
    }
    assert_eq!(f.api("GET", "/v1/followups/sequences", None).await.body["sequences"], json!([]), "nothing was saved");
}

#[sqlx::test]
async fn sequences_are_listed_with_paging(pool: PgPool) {
    let f = F::new(pool).await;
    let mut ids = Vec::new();
    for _ in 0..3 {
        ids.push(f.sequence(&[0]).await);
    }
    let r = f.api("GET", "/v1/followups/sequences?limit=2", None).await;
    assert_eq!(r.body["sequences"].as_array().unwrap().len(), 2);
    assert_eq!(r.body["sequences"][0]["id"], ids[2].to_string(), "newest first");
    let cursor = r.body["next_cursor"].as_str().unwrap().to_string();
    let r = f.api("GET", &format!("/v1/followups/sequences?limit=2&cursor={cursor}"), None).await;
    assert_eq!(r.body["sequences"].as_array().unwrap().len(), 1);
    assert!(r.body["next_cursor"].is_null());
    f.api("PATCH", &format!("/v1/followups/sequences/{}", ids[0]), Some(json!({ "action": "archive" }))).await;
    let r = f.api("GET", "/v1/followups/sequences?status=archived", None).await;
    assert_eq!(r.body["sequences"].as_array().unwrap().len(), 1);
    assert_eq!(f.api("GET", "/v1/followups/sequences?limit=0", None).await.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test]
async fn preview_renders_every_step_and_writes_nothing(pool: PgPool) {
    let f = F::new(pool).await;
    let seq = f
        .sequence_from(json!({ "name": "p", "steps": [
            step(0, "Hi {{first_name|there}}", "Dear {{first_name}}, I am {{sender_name}} <{{sender_email}}> writing to {{email}}."),
            step(2, "", "Second for {{company|your team}}"),
            { "delay_amount": 3, "subject": "Custom {{company|team}}", "body_html": "<p>{{first_name}} &amp; co</p>" },
        ] }))
        .await;
    let a = f.account("sam@acme.test").await;
    let r = f
        .api(
            "POST",
            &format!("/v1/followups/sequences/{seq}/preview"),
            Some(json!({ "variables": { "first_name": "Ana <b>" }, "to_email": "Ana@Corp.com", "account_id": a })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let s = r.body["steps"].as_array().unwrap();
    assert_eq!(s.len(), 3);
    assert_eq!(s[0]["subject"], "Hi Ana <b>");
    assert_eq!(s[0]["body_text"], "Dear Ana <b>, I am Test Sender <sam@acme.test> writing to ana@corp.com.");
    assert_eq!(s[1]["subject"], "Re: Hi Ana <b>", "an empty subject is Re: plus the subject of step 1");
    assert_eq!(s[1]["body_text"], "Second for your team");
    assert_eq!(s[2]["subject"], "Custom team");
    assert_eq!(s[2]["body_html"], "<p>Ana &lt;b&gt; &amp; co</p>", "values are escaped in HTML");
    assert_eq!(s[2]["delay_amount"], 3);
    assert_eq!(count(&f.h.state, "SELECT count(*) FROM send_jobs").await, 0);
    assert_eq!(count(&f.h.state, "SELECT count(*) FROM enrollments").await, 0);
    // No body at all: the defaults, with the fallbacks, where the step 1 body needs first_name.
    let r = f.api("POST", &format!("/v1/followups/sequences/{seq}/preview"), None).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.body["error"]["code"], "missing_variable");
    assert!(r.body["error"]["message"].as_str().unwrap().contains("step 1 needs: first_name"), "{}", r.body);
    assert_eq!(f.api("POST", &format!("/v1/followups/sequences/{}/preview", Uuid::now_v7()), Some(json!({}))).await.status, StatusCode::NOT_FOUND);
    let bad = f.api("POST", &format!("/v1/followups/sequences/{seq}/preview"), Some(json!({ "variables": { "a": [1] } }))).await;
    assert_eq!(bad.body["error"]["code"], "invalid_variables");
}

#[sqlx::test]
async fn put_steps_replaces_the_list(pool: PgPool) {
    let f = F::new(pool).await;
    let seq = f.sequence(&[0, 1, 2]).await;
    let r = f
        .api(
            "PUT",
            &format!("/v1/followups/sequences/{seq}/steps"),
            Some(json!({ "steps": [step(0, "New subject", "New first"), step(5, "", "New second")] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let steps = r.body["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 2, "step 3 is gone");
    assert_eq!(steps[0]["subject"], "New subject");
    assert_eq!(steps[1]["delay_amount"], 5);
    assert_eq!(count(&f.h.state, "SELECT count(*) FROM sequence_steps").await, 2);
    let bad = f.api("PUT", &format!("/v1/followups/sequences/{seq}/steps"), Some(json!({ "steps": [] }))).await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(count(&f.h.state, "SELECT count(*) FROM sequence_steps").await, 2, "a bad list changes nothing");
    assert_eq!(f.api("PUT", &format!("/v1/followups/sequences/{}/steps", Uuid::now_v7()), Some(json!({ "steps": [step(0, "S", "B")] }))).await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn a_step_cannot_be_removed_while_an_enrollment_has_it_queued(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1, 1]).await;
    let e = f.enroll(seq, a, "lead@company.com").await;
    let j1 = f.until_sent(e, 1).await;
    assert_eq!(f.h.job(j1).await.status, JobStatus::Sent);
    // Step 2 is due after a business day. Move to the day after, make the job, but do not send it.
    f.h.advance(hours(24));
    f.fu_tick().await;
    assert_eq!(f.status(e).await, "waiting_send");
    // The new list has 1 step: it removes step 2, which is queued.
    let r = f.api("PUT", &format!("/v1/followups/sequences/{seq}/steps"), Some(json!({ "steps": [step(0, "Only", "One")] }))).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.body);
    assert_eq!(r.body["error"]["code"], "step_in_use");
    // Removing only step 3 is fine.
    let ok = f
        .api("PUT", &format!("/v1/followups/sequences/{seq}/steps"), Some(json!({ "steps": [step(0, "Hello", "One"), step(1, "", "Two")] })))
        .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
}

#[sqlx::test]
async fn new_text_that_needs_a_value_an_open_enrollment_lacks_is_refused(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1]).await;
    let e = f.enroll_with(seq, a, "lead@company.com", json!({ "first_name": "Ana" })).await;
    let r = f
        .api(
            "PUT",
            &format!("/v1/followups/sequences/{seq}/steps"),
            Some(json!({ "steps": [step(0, "Hi", "Body"), step(1, "", "About {{company}}")] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.body);
    assert_eq!(r.body["error"]["code"], "missing_variable");
    assert!(r.body["error"]["message"].as_str().unwrap().contains("'company'"));
    // With a fallback it is fine. A built-in name needs no value.
    let ok = f
        .api(
            "PUT",
            &format!("/v1/followups/sequences/{seq}/steps"),
            Some(json!({ "steps": [step(0, "Hi", "Body"), step(1, "", "About {{company|you}} from {{sender_name}}")] })),
        )
        .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    assert_eq!(f.status(e).await, "active");
}

#[sqlx::test]
async fn patch_changes_settings_and_status(pool: PgPool) {
    let f = F::new(pool).await;
    let seq = f.sequence(&[0]).await;
    let url = format!("/v1/followups/sequences/{seq}");
    let r = f
        .api("PATCH", &url, Some(json!({ "name": "Renamed", "stop_on_auto_reply": true, "auto_reply_pause_days": 5, "thread_followups": false })))
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["sequence"]["name"], "Renamed");
    assert_eq!(r.body["sequence"]["stop_on_auto_reply"], true);
    assert_eq!(r.body["sequence"]["auto_reply_pause_days"], 5);
    assert_eq!(r.body["sequence"]["thread_followups"], false);
    for (action, status) in [("pause", "paused"), ("resume", "active"), ("archive", "archived"), ("resume", "active")] {
        let r = f.api("PATCH", &url, Some(json!({ "action": action }))).await;
        assert_eq!(r.body["sequence"]["status"], status, "{action}");
    }
    assert_eq!(f.api("PATCH", &url, Some(json!({ "name": " " }))).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(f.api("PATCH", &url, Some(json!({ "action": "delete" }))).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(f.api("PATCH", &format!("/v1/followups/sequences/{}", Uuid::now_v7()), Some(json!({ "name": "x" }))).await.status, StatusCode::NOT_FOUND);
}

// =====================================================================================================
// Phase 3: enroll
// =====================================================================================================

#[sqlx::test]
async fn the_same_key_twice_makes_one_enrollment(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1]).await;
    let body = json!({ "enrollments": [{ "to_email": "lead@company.com", "account_id": a, "variables": { "first_name": "Ana" }, "idempotency_key": "k1" }] });
    let url = format!("/v1/followups/sequences/{seq}/enrollments");
    let first = f.api("POST", &url, Some(body.clone())).await;
    assert_eq!((first.body["enrolled"].as_i64(), first.body["duplicates"].as_i64()), (Some(1), Some(0)), "{}", first.body);
    let second = f.api("POST", &url, Some(body)).await;
    assert_eq!((second.body["enrolled"].as_i64(), second.body["duplicates"].as_i64()), (Some(0), Some(1)), "{}", second.body);
    assert_eq!(second.body["items"][0]["status"], "duplicate");
    assert_eq!(first.body["items"][0]["enrollment"]["id"], second.body["items"][0]["enrollment"]["id"]);
    assert_eq!(count(&f.h.state, "SELECT count(*) FROM enrollments").await, 1);
    // Step 1 is due now.
    let e = &first.body["items"][0]["enrollment"];
    assert_eq!(e["status"], "active");
    assert_eq!(e["next_step"], 1);
    assert!(e["next_due_at"].is_string());
}

#[sqlx::test]
async fn an_address_in_an_open_enrollment_is_refused_in_a_second_sequence(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let (s1, s2) = (f.sequence(&[0, 1]).await, f.sequence(&[0, 2]).await);
    f.enroll(s1, a, "lead@company.com").await;
    let r = f
        .api(
            "POST",
            &format!("/v1/followups/sequences/{s2}/enrollments"),
            Some(json!({ "enrollments": [
                { "to_email": "Lead@Company.com", "account_id": a, "variables": { "first_name": "Ana" } },
                { "to_email": "other@company.com", "account_id": a, "variables": { "first_name": "Bo" } },
            ] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["items"][0]["error"]["code"], "already_enrolled");
    assert_eq!(r.body["items"][1]["status"], "enrolled", "the good item is still enrolled");
    assert_eq!((r.body["enrolled"].as_i64(), r.body["errors"].as_i64()), (Some(1), Some(1)));
    // The setting turns the rule off.
    f.api("PATCH", "/v1/followups/settings", Some(json!({ "one_active_per_email": false }))).await;
    let r = f
        .api(
            "POST",
            &format!("/v1/followups/sequences/{s2}/enrollments"),
            Some(json!({ "enrollments": [{ "to_email": "lead@company.com", "account_id": a, "variables": { "first_name": "Ana" } }] })),
        )
        .await;
    assert_eq!(r.body["enrolled"], 1, "{}", r.body);
}

#[sqlx::test]
async fn an_address_can_be_enrolled_again_after_the_first_enrollment_ended(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0]).await;
    let e = f.enroll(seq, a, "lead@company.com").await;
    f.until_sent(e, 1).await;
    assert_eq!(f.status(e).await, "finished");
    f.enroll(seq, a, "lead@company.com").await;
}

#[sqlx::test]
async fn two_calls_at_the_same_moment_enroll_an_address_once(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let (s1, s2) = (f.sequence(&[0]).await, f.sequence(&[0]).await);
    let body = |_: ()| Some(json!({ "enrollments": [{ "to_email": "race@company.com", "account_id": a, "variables": { "first_name": "Ana" } }] }));
    let (u1, u2) = (format!("/v1/followups/sequences/{s1}/enrollments"), format!("/v1/followups/sequences/{s2}/enrollments"));
    let (r1, r2) = tokio::join!(f.api("POST", &u1, body(())), f.api("POST", &u2, body(())));
    let enrolled = r1.body["enrolled"].as_i64().unwrap() + r2.body["enrolled"].as_i64().unwrap();
    assert_eq!(enrolled, 1, "{} {}", r1.body, r2.body);
    assert_eq!(count(&f.h.state, "SELECT count(*) FROM enrollments").await, 1);
}

#[sqlx::test]
async fn a_missing_variable_blocks_the_whole_call_with_422(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    // Step 3 needs `company`: the failure must come now, not when step 3 is due.
    let seq = f
        .sequence_from(json!({ "name": "m", "steps": [step(0, "Hi {{first_name|there}}", "B"), step(1, "", "B"), step(1, "", "About {{company}}")] }))
        .await;
    let r = f
        .api(
            "POST",
            &format!("/v1/followups/sequences/{seq}/enrollments"),
            Some(json!({ "enrollments": [
                { "to_email": "good@company.com", "account_id": a, "variables": { "company": "Acme" } },
                { "to_email": "bad@company.com", "account_id": a, "variables": { "first_name": "Bo" } },
            ] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.body);
    assert_eq!(r.body["error"]["code"], "missing_variable");
    let msg = r.body["error"]["message"].as_str().unwrap();
    assert!(msg.contains("item 1") && msg.contains("step 3 needs: company"), "{msg}");
    assert!(!msg.contains("item 0"));
    assert_eq!(count(&f.h.state, "SELECT count(*) FROM enrollments").await, 0, "nothing was enrolled, also not the good item");
    // An empty value counts as missing.
    let r = f
        .api(
            "POST",
            &format!("/v1/followups/sequences/{seq}/enrollments"),
            Some(json!({ "enrollments": [{ "to_email": "e@company.com", "account_id": a, "variables": { "company": "  " } }] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test]
async fn bad_items_get_their_own_error_and_the_rest_is_enrolled(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0]).await;
    sqlx::query("INSERT INTO suppressions (value, kind, reason) VALUES ('bounced@company.com', 'email', 'hard_bounce'), ('blocked.example', 'domain', 'manual')")
        .execute(&f.h.state.pool)
        .await
        .unwrap();
    let ok = |email: &str| json!({ "to_email": email, "account_id": a, "variables": { "first_name": "X" } });
    let r = f
        .api(
            "POST",
            &format!("/v1/followups/sequences/{seq}/enrollments"),
            Some(json!({ "enrollments": [
                ok("not-an-email"),
                json!({ "to_email": "x@company.com", "account_id": Uuid::now_v7(), "variables": { "first_name": "X" } }),
                json!({ "to_email": "y@company.com", "account_id": a, "variables": { "first_name": ["no"] } }),
                json!({ "to_email": "z@company.com", "account_id": a, "variables": { "first_name": "Z" }, "idempotency_key": "" }),
                ok("bounced@company.com"),
                ok("anyone@blocked.example"),
                ok("fine@company.com"),
            ] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let codes: Vec<Option<&str>> = r.body["items"].as_array().unwrap().iter().map(|i| i["error"]["code"].as_str()).collect();
    assert_eq!(
        codes,
        vec![
            Some("invalid_to_email"),
            Some("account_not_found"),
            Some("invalid_variables"),
            Some("invalid_idempotency_key"),
            Some("suppressed"),
            Some("suppressed"),
            None
        ]
    );
    assert_eq!(r.body["enrolled"], 1);
    assert_eq!(r.body["errors"], 6);
    assert_eq!(count(&f.h.state, "SELECT count(*) FROM enrollments").await, 1);
}

#[sqlx::test]
async fn batch_size_and_sequence_state_are_checked(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0]).await;
    let url = format!("/v1/followups/sequences/{seq}/enrollments");
    let r = f.api("POST", &url, Some(json!({ "enrollments": [] }))).await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_batch_size")));
    let many: Vec<Value> = (0..1001).map(|i| json!({ "to_email": format!("l{i}@c.com"), "account_id": a })).collect();
    assert_eq!(f.api("POST", &url, Some(json!({ "enrollments": many }))).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(f.api("POST", &format!("/v1/followups/sequences/{}/enrollments", Uuid::now_v7()), Some(json!({ "enrollments": [{ "to_email": "a@b.com", "account_id": a }] }))).await.status, StatusCode::NOT_FOUND);
    f.api("PATCH", &format!("/v1/followups/sequences/{seq}"), Some(json!({ "action": "archive" }))).await;
    let r = f.api("POST", &url, Some(json!({ "enrollments": [{ "to_email": "a@b.com", "account_id": a }] }))).await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::CONFLICT, Some("sequence_archived")));
    // A paused sequence takes enrollments. They wait.
    f.api("PATCH", &format!("/v1/followups/sequences/{seq}"), Some(json!({ "action": "pause" }))).await;
    let r = f.api("POST", &url, Some(json!({ "enrollments": [{ "to_email": "a@b.com", "account_id": a, "variables": { "first_name": "A" } }] }))).await;
    assert_eq!(r.body["enrolled"], 1, "{}", r.body);
}

#[sqlx::test]
async fn a_thousand_enrollments_in_one_call(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1]).await;
    let items: Vec<Value> = (0..1000).map(|i| json!({ "to_email": format!("lead{i}@company{i}.com"), "account_id": a, "variables": { "first_name": "X" }, "idempotency_key": format!("k{i}") })).collect();
    let r = f.api("POST", &format!("/v1/followups/sequences/{seq}/enrollments"), Some(json!({ "enrollments": items }))).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["enrolled"], 1000);
    let counts = f.api("GET", &format!("/v1/followups/sequences/{seq}"), None).await.body["counts"].clone();
    assert_eq!(counts, json!({ "active": 1000 }));
}

// =====================================================================================================
// Phase 4: the scheduler
// =====================================================================================================

#[sqlx::test]
async fn a_due_step_becomes_one_automated_job(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1]).await;
    let e = f.enroll_with(seq, a, "Lead@Company.com", json!({ "first_name": "Ana" })).await;
    let r = f.fu_tick().await;
    assert_eq!(r.queued, 1);
    let d = f.detail(e).await;
    assert_eq!(d["enrollment"]["status"], "waiting_send");
    assert!(d["enrollment"]["next_due_at"].is_null());
    let job = &d["jobs"][0];
    assert_eq!(d["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(job["kind"], "automated");
    assert_eq!(job["status"], "queued");
    assert_eq!(job["to_email"], "lead@company.com");
    assert_eq!(job["subject"], "Hello Ana");
    assert_eq!(job["body_text"], "Step 1 for Ana");
    assert_eq!(job["step_number"], 1);
    assert_eq!(job["enrollment_id"], e.to_string());
    assert_eq!(job["idempotency_key"], format!("fu:{e}:1"));
    assert_eq!(job["metadata"]["sequence_id"], seq.to_string());
    assert!(job["in_reply_to"].is_null(), "step 1 starts the thread");
    assert_eq!(d["enrollment"]["current_job_id"], job["id"]);
    assert_eq!(f.event_kinds(e).await, vec!["enrolled", "step_queued"]);
    // A second tick makes no second job.
    assert_eq!(f.fu_tick().await.queued, 0);
    assert_eq!(f.jobs(e).await.len(), 1);
}

#[sqlx::test]
async fn nothing_is_made_before_the_start_time(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0]).await;
    let start = f.h.now() + hours(3);
    let r = f
        .api(
            "POST",
            &format!("/v1/followups/sequences/{seq}/enrollments"),
            Some(json!({ "enrollments": [{ "to_email": "l@company.com", "account_id": a, "variables": { "first_name": "A" }, "start_at": start }] })),
        )
        .await;
    assert_eq!(r.body["enrolled"], 1, "{}", r.body);
    assert_eq!(f.fu_tick().await.queued, 0);
    f.h.advance(hours(3) - Duration::seconds(1));
    assert_eq!(f.fu_tick().await.queued, 0);
    f.h.advance(Duration::seconds(1));
    assert_eq!(f.fu_tick().await.queued, 1);
}

#[sqlx::test]
async fn two_scheduler_copies_make_one_job_per_enrollment(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1]).await;
    let items: Vec<Value> = (0..40).map(|i| json!({ "to_email": format!("l{i}@company{i}.com"), "account_id": a, "variables": { "first_name": "X" } })).collect();
    f.api("POST", &format!("/v1/followups/sequences/{seq}/enrollments"), Some(json!({ "enrollments": items }))).await;
    let second = FollowupEngine::new(f.h.state.clone());
    for _ in 0..3 {
        let (r1, r2) = tokio::join!(scheduler::tick(&f.fe), scheduler::tick(&second));
        let _ = (r1.unwrap(), r2.unwrap());
    }
    assert_eq!(count(&f.h.state, "SELECT count(*) FROM send_jobs WHERE enrollment_id IS NOT NULL").await, 40);
    assert_eq!(count(&f.h.state, "SELECT count(DISTINCT enrollment_id) FROM send_jobs").await, 40);
    assert_eq!(count(&f.h.state, "SELECT count(*) FROM enrollments WHERE status = 'waiting_send'").await, 40);
    assert_eq!(count(&f.h.state, "SELECT count(*) FROM enrollment_events WHERE kind = 'step_queued'").await, 40);
}

#[sqlx::test]
async fn a_paused_sequence_or_a_paused_engine_makes_no_steps(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0]).await;
    f.enroll(seq, a, "l@company.com").await;
    f.api("PATCH", &format!("/v1/followups/sequences/{seq}"), Some(json!({ "action": "pause" }))).await;
    assert_eq!(f.fu_tick().await, TickReport::default());
    f.api("PATCH", &format!("/v1/followups/sequences/{seq}"), Some(json!({ "action": "resume" }))).await;
    f.api("POST", "/v1/followups/pause", None).await;
    assert_eq!(f.fu_tick().await, TickReport { paused: true, ..Default::default() });
    assert_eq!(count(&f.h.state, "SELECT count(*) FROM send_jobs").await, 0);
    f.api("POST", "/v1/followups/resume", None).await;
    assert_eq!(f.fu_tick().await.queued, 1);
}

#[sqlx::test]
async fn the_next_step_is_due_a_business_day_after_the_real_send(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1]).await;
    // Friday 2026-10-23 10:00 UTC.
    f.h.set_time(utc(2026, 10, 23, 10, 0));
    let e = f.enroll(seq, a, "lead@company.com").await;
    f.until_sent(e, 1).await;
    let sent_at = f.h.job(f.jobs(e).await[0]).await.sent_at.unwrap();
    let d = f.detail(e).await;
    assert_eq!(d["enrollment"]["status"], "active");
    assert_eq!(d["enrollment"]["next_step"], 2);
    assert!(d["enrollment"]["current_job_id"].is_null());
    let due: DateTime<Utc> = d["enrollment"]["next_due_at"].as_str().unwrap().parse().unwrap();
    // Friday plus one business day is Monday, at the same time of day as the real send.
    assert_eq!(due.date_naive().to_string(), "2026-10-26", "sent {sent_at}");
    assert_eq!(due.time(), sent_at.time());
}

// =====================================================================================================
// Phase 5: threading
// =====================================================================================================

#[sqlx::test]
async fn follow_ups_are_threaded_to_the_earlier_emails(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1, 1]).await;
    let e = f.enroll(seq, a, "lead@company.com").await;
    // The sender gives a thread id for step 1 (as Gmail does).
    f.h.sender.push(Ok(aikido_email::sending::senders::SendReceipt {
        provider_message_id: Some("g1".into()),
        provider_thread_id: Some("thread-77".into()),
        message_id_header: None,
    }));
    let j1 = f.until_sent(e, 1).await;
    let j2 = f.until_sent(e, 2).await;
    let j3 = f.until_sent(e, 3).await;
    let ids: Vec<String> = {
        let mut v = Vec::new();
        for j in [j1, j2, j3] {
            v.push(f.h.job(j).await.message_id_header.unwrap());
        }
        v
    };
    let calls = f.h.sender.calls();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].in_reply_to, None);
    assert_eq!(calls[0].references, None);
    assert_eq!(calls[0].subject, "Hello Ana");
    assert_eq!(calls[1].subject, "Re: Hello Ana");
    assert_eq!(calls[1].in_reply_to.as_deref(), Some(ids[0].as_str()));
    assert_eq!(calls[1].references.as_deref(), Some(ids[0].as_str()));
    assert_eq!(calls[1].thread_id.as_deref(), Some("thread-77"));
    assert_eq!(calls[2].subject, "Re: Hello Ana");
    assert_eq!(calls[2].in_reply_to.as_deref(), Some(ids[1].as_str()), "answers the last step");
    assert_eq!(calls[2].references.as_deref(), Some(format!("{} {}", ids[0], ids[1]).as_str()));
    assert_eq!(calls[2].thread_id.as_deref(), Some("thread-77"));
}

#[sqlx::test]
async fn threading_can_be_turned_off(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f
        .sequence_from(json!({ "name": "n", "thread_followups": false, "steps": [step(0, "Hello", "One"), step(1, "", "Two"), step(1, "Own subject", "Three")] }))
        .await;
    let e = f.enroll(seq, a, "lead@company.com").await;
    f.until_sent(e, 3).await;
    let calls = f.h.sender.calls();
    assert!(calls.iter().all(|c| c.in_reply_to.is_none() && c.references.is_none() && c.thread_id.is_none()));
    assert_eq!(calls[1].subject, "Hello", "no Re: when the emails are not threaded");
    assert_eq!(calls[2].subject, "Own subject");
}

#[sqlx::test]
async fn a_missing_earlier_message_id_sends_without_headers_and_warns(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1]).await;
    let e = f.enroll(seq, a, "lead@company.com").await;
    let j1 = f.until_sent(e, 1).await;
    sqlx::query("UPDATE send_jobs SET message_id_header = NULL WHERE id = $1").bind(j1).execute(&f.h.state.pool).await.unwrap();
    let j2 = f.until_sent(e, 2).await;
    let sent = f.h.sender.calls();
    assert_eq!(sent.len(), 2);
    assert!(sent[1].in_reply_to.is_none() && sent[1].references.is_none());
    assert_eq!(sent[1].subject, "Re: Hello Ana", "the subject still matches");
    assert_eq!(f.h.job(j2).await.status, JobStatus::Sent, "the step was not blocked");
    let d = f.detail(e).await;
    let warning = d["events"].as_array().unwrap().iter().find(|ev| ev["kind"] == "warning").expect("a warning event");
    assert_eq!(warning["detail"]["warning"], "thread_headers_missing");
}

#[sqlx::test]
async fn a_subject_that_starts_with_re_is_not_doubled(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence_from(json!({ "name": "n", "steps": [step(0, "Re: your invoice", "One"), step(1, "", "Two")] })).await;
    let e = f.enroll(seq, a, "lead@company.com").await;
    f.until_sent(e, 2).await;
    assert_eq!(f.h.sender.calls()[1].subject, "Re: your invoice");
}

#[sqlx::test]
async fn a_follow_up_goes_through_the_real_smtp_sender_into_mailpit(pool: PgPool) {
    use aikido_email::sending::worker::{Engine, RealSenders};
    if std::net::TcpStream::connect_timeout(&"127.0.0.1:1025".parse().unwrap(), std::time::Duration::from_millis(300)).is_err() {
        eprintln!("SKIPPED: Mailpit is not running");
        return;
    }
    let mut f = F::new(pool).await;
    f.h.engine = Arc::new(Engine::new(f.h.state.clone(), Arc::new(RealSenders { dry_run: false })));
    let a = f.account("followup-test@example.com").await;
    let tag = Uuid::now_v7().to_string();
    let seq = f
        .sequence_from(json!({ "name": "m", "steps": [step(0, &format!("Mailpit {tag}"), "First"), step(1, "", "Second")] }))
        .await;
    let to = format!("mp-{tag}@company.com");
    let e = f.enroll(seq, a, &to).await;
    let (j1, j2) = (f.until_sent(e, 1).await, f.until_sent(e, 2).await);
    let (id1, id2) = (f.h.job(j1).await.message_id_header.unwrap(), f.h.job(j2).await.message_id_header.unwrap());
    let text = reqwest::get("http://127.0.0.1:8025/api/v1/messages").await.unwrap().text().await.unwrap();
    let all: Vec<Value> = serde_json::from_str::<Value>(&text).unwrap()["messages"].as_array().cloned().unwrap_or_default();
    let mine: Vec<&Value> = all.iter().filter(|m| m["Subject"].as_str().is_some_and(|s| s.contains(&tag))).collect();
    assert_eq!(mine.len(), 2, "both emails arrived");
    let raw_of = |m: &Value| {
        let id = m["ID"].as_str().unwrap().to_string();
        async move { reqwest::get(format!("http://127.0.0.1:8025/api/v1/message/{id}/raw")).await.unwrap().text().await.unwrap() }
    };
    let second = mine.iter().find(|m| m["Subject"].as_str().unwrap().starts_with("Re: ")).expect("the follow-up has the Re: subject");
    let raw = raw_of(second).await;
    assert!(raw.contains(&format!("Message-ID: {id2}")), "{raw}");
    assert!(raw.contains(&format!("In-Reply-To: {id1}")), "{raw}");
    assert!(raw.contains(&format!("References: {id1}")), "{raw}");
}

// =====================================================================================================
// Phase 6: the send result
// =====================================================================================================

#[sqlx::test]
async fn a_three_step_sequence_runs_to_finished(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1, 2]).await;
    let e = f.enroll(seq, a, "lead@company.com").await;
    f.until_sent(e, 3).await;
    let d = f.detail(e).await;
    assert_eq!(d["enrollment"]["status"], "finished", "{d}");
    assert!(d["enrollment"]["next_due_at"].is_null());
    assert!(d["enrollment"]["current_job_id"].is_null());
    assert_eq!(f.event_kinds(e).await, vec!["enrolled", "step_queued", "step_sent", "step_queued", "step_sent", "step_queued", "step_sent", "finished"]);
    assert_eq!(f.h.sender.calls().len(), 3);
    // Finished is final.
    f.h.advance(Duration::days(30));
    assert_eq!(f.fu_tick().await, TickReport::default());
    assert_eq!(f.jobs(e).await.len(), 3);
}

#[sqlx::test]
async fn a_late_step_one_moves_the_other_steps_back(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1, 1]).await;
    let e = f.enroll(seq, a, "lead@company.com").await;
    // The account is paused for two days: step 1 is late.
    f.h.sender.set_delay(std::time::Duration::from_millis(0));
    sqlx::query("UPDATE account_send_state SET paused = true, paused_reason = 'test' WHERE account_id = $1").bind(a).execute(&f.h.state.pool).await.unwrap();
    f.round(Duration::minutes(10)).await;
    f.h.advance(Duration::days(2));
    sqlx::query("UPDATE account_send_state SET paused = false, paused_reason = NULL WHERE account_id = $1").bind(a).execute(&f.h.state.pool).await.unwrap();
    let j1 = f.until_sent(e, 1).await;
    let sent1 = f.h.job(j1).await.sent_at.unwrap();
    assert!(sent1 >= utc(2026, 10, 21, 10, 0), "step 1 went out late: {sent1}");
    let due: DateTime<Utc> = f.detail(e).await["enrollment"]["next_due_at"].as_str().unwrap().parse().unwrap();
    assert!(due > sent1 + hours(23), "step 2 counts from the real send, not from the plan: {due} vs {sent1}");
    let j2 = f.until_sent(e, 2).await;
    let sent2 = f.h.job(j2).await.sent_at.unwrap();
    assert!(sent2 - sent1 >= hours(24));
    let due3: DateTime<Utc> = f.detail(e).await["enrollment"]["next_due_at"].as_str().unwrap().parse().unwrap();
    assert!(due3 > sent2 + hours(23));
}

#[sqlx::test]
async fn a_retry_keeps_the_enrollment_waiting_then_it_goes_on(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1]).await;
    let e = f.enroll(seq, a, "lead@company.com").await;
    f.h.sender.push(Err(SendError::Transient("server busy".into())));
    f.round(Duration::minutes(10)).await;
    let j1 = f.jobs(e).await[0];
    assert_eq!(f.h.job(j1).await.status, JobStatus::Queued, "a retry is planned");
    assert_eq!(f.status(e).await, "waiting_send");
    f.until_sent(e, 1).await;
    assert_eq!(f.status(e).await, "active");
}

#[sqlx::test]
async fn a_permanent_error_fails_the_enrollment(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1]).await;
    let e = f.enroll(seq, a, "lead@company.com").await;
    f.h.sender.push(Err(SendError::Permanent { reason: "message too large".into(), hard_bounce: false }));
    f.round(Duration::minutes(10)).await;
    assert_eq!(f.status(e).await, "failed");
    assert_eq!(f.reason(e).await.as_deref(), Some("send_error"));
    f.h.advance(Duration::days(5));
    f.fu_tick().await;
    assert_eq!(f.jobs(e).await.len(), 1, "no second step");
    assert_eq!(f.status(e).await, "failed", "failed never changes again");
}

#[sqlx::test]
async fn retries_that_run_out_fail_the_enrollment(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1]).await;
    let e = f.enroll(seq, a, "lead@company.com").await;
    f.h.sender.push_many((0..3).map(|_| Err(SendError::Transient("down".into()))));
    for _ in 0..40 {
        f.round(Duration::minutes(30)).await;
    }
    assert_eq!(f.status(e).await, "failed");
}

#[sqlx::test]
async fn a_hard_bounce_at_send_time_stops_the_enrollment(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1]).await;
    let e = f.enroll(seq, a, "gone@company.com").await;
    f.h.sender.push(Err(SendError::Permanent { reason: "550 no such user".into(), hard_bounce: true }));
    f.round(Duration::minutes(10)).await;
    assert_eq!(f.status(e).await, "stopped");
    assert_eq!(f.reason(e).await.as_deref(), Some("bounce_hard"));
    assert_eq!(suppression_reason(&f.h.state, "gone@company.com").await.as_deref(), Some("hard_bounce"));
}

#[sqlx::test]
async fn a_job_cancelled_by_hand_stops_the_enrollment(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1]).await;
    let e = f.enroll(seq, a, "lead@company.com").await;
    f.fu_tick().await;
    let j = f.jobs(e).await[0];
    let key = &f.keys[&Scope::Sending];
    let r = call(&f.h.state, "DELETE", &format!("/v1/sending/jobs/{j}"), Some(key), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(f.status(e).await, "stopped");
    assert_eq!(f.reason(e).await.as_deref(), Some("job_cancelled"));
}

#[sqlx::test]
async fn a_suppression_added_after_the_job_was_made_stops_the_enrollment(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1]).await;
    let e = f.enroll(seq, a, "lead@company.com").await;
    f.fu_tick().await;
    sqlx::query("INSERT INTO suppressions (value, kind, reason) VALUES ('lead@company.com', 'email', 'manual')").execute(&f.h.state.pool).await.unwrap();
    f.h.tick().await;
    let j = f.jobs(e).await[0];
    assert_eq!(f.h.job(j).await.status, JobStatus::Skipped);
    assert_eq!(f.status(e).await, "stopped");
    assert_eq!(f.reason(e).await.as_deref(), Some("suppressed"));
    assert!(f.h.sender.calls().is_empty());
}

#[sqlx::test]
async fn a_suppressed_address_at_due_time_gets_no_job(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1]).await;
    let e = f.enroll(seq, a, "lead@company.com").await;
    sqlx::query("INSERT INTO suppressions (value, kind, reason) VALUES ('company.com', 'domain', 'manual')").execute(&f.h.state.pool).await.unwrap();
    let r = f.fu_tick().await;
    assert_eq!((r.queued, r.stopped), (0, 1));
    assert_eq!(f.status(e).await, "stopped");
    assert_eq!(f.reason(e).await.as_deref(), Some("suppressed"));
    assert_eq!(count(&f.h.state, "SELECT count(*) FROM send_jobs").await, 0);
}

#[sqlx::test]
async fn a_step_edited_after_step_one_went_out_uses_the_new_text(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1]).await;
    let e = f.enroll(seq, a, "lead@company.com").await;
    f.until_sent(e, 1).await;
    let r = f
        .api(
            "PUT",
            &format!("/v1/followups/sequences/{seq}/steps"),
            Some(json!({ "steps": [step(0, "Hello {{first_name|there}}", "Step 1 for {{first_name|friend}}"), step(1, "A new subject", "Brand new text for {{first_name|friend}}")] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let j2 = f.until_sent(e, 2).await;
    let job = f.h.job(j2).await;
    assert_eq!(job.subject, "A new subject");
    assert_eq!(job.body_text.as_deref(), Some("Brand new text for Ana"));
    // The first email is as it was sent.
    assert_eq!(f.h.job(f.jobs(e).await[0]).await.body_text.as_deref(), Some("Step 1 for Ana"));
}

#[sqlx::test]
async fn a_removed_step_finishes_the_enrollment(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1, 1]).await;
    let e = f.enroll(seq, a, "lead@company.com").await;
    f.until_sent(e, 2).await;
    // The enrollment waits for step 3. Remove it.
    let r = f.api("PUT", &format!("/v1/followups/sequences/{seq}/steps"), Some(json!({ "steps": [step(0, "Hi", "One"), step(1, "", "Two")] }))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    f.h.advance(Duration::days(3));
    let rep = f.fu_tick().await;
    assert_eq!(rep.finished, 1);
    assert_eq!(f.status(e).await, "finished");
}

#[sqlx::test]
async fn a_changed_sequence_is_used_for_the_next_due_time(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence_from(json!({ "name": "n", "steps": [step(0, "Hi", "One"), step(1, "", "Two")] })).await;
    let e = f.enroll(seq, a, "lead@company.com").await;
    // Before step 1 is sent, the delay of step 2 changes to 5 calendar days.
    f.api(
        "PUT",
        &format!("/v1/followups/sequences/{seq}/steps"),
        Some(json!({ "steps": [step(0, "Hi", "One"), { "delay_amount": 5, "delay_type": "calendar_days", "body_text": "Two" }] })),
    )
    .await;
    let j1 = f.until_sent(e, 1).await;
    let sent = f.h.job(j1).await.sent_at.unwrap();
    let due: DateTime<Utc> = f.detail(e).await["enrollment"]["next_due_at"].as_str().unwrap().parse().unwrap();
    assert_eq!(due, sent + Duration::days(5));
}

#[sqlx::test]
async fn the_due_time_keeps_the_local_clock_over_a_dst_change(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    f.h.settings(a, json!({ "timezone": "Europe/Berlin", "window_start": "08:00", "window_end": "18:00", "window_start_jitter_mins": 0 })).await;
    let seq = f.sequence(&[0, 3]).await;
    // Friday 2026-10-23, 10:00 in Berlin (summer time, UTC+2) is 08:00 UTC.
    f.h.set_time(utc(2026, 10, 23, 8, 0));
    let e = f.enroll(seq, a, "lead@company.com").await;
    f.until_sent(e, 1).await;
    let sent: DateTime<Utc> = f.h.job(f.jobs(e).await[0]).await.sent_at.unwrap();
    let due: DateTime<Utc> = f.detail(e).await["enrollment"]["next_due_at"].as_str().unwrap().parse().unwrap();
    // Berlin goes back to winter time (UTC+1) on Sunday 2026-10-25. Three business days after Friday is
    // Wednesday 2026-10-28, at the same Berlin clock time: one hour later in UTC.
    assert_eq!(due.date_naive().to_string(), "2026-10-28", "{sent} -> {due}");
    assert_eq!(due.time(), sent.time() + Duration::hours(1));
}

// =====================================================================================================
// Waiting, stalling, the inbox gate
// =====================================================================================================

async fn make_readable(f: &F, account: Uuid) {
    sqlx::query("UPDATE email_accounts SET can_read = true, read_status = 'ok' WHERE id = $1").bind(account).execute(&f.h.state.pool).await.unwrap();
    sqlx::query("INSERT INTO inbox_sync_state (account_id, folder) VALUES ($1, 'inbox') ON CONFLICT DO NOTHING").bind(account).execute(&f.h.state.pool).await.unwrap();
}

async fn synced(f: &F, account: Uuid, at: DateTime<Utc>) {
    sqlx::query("UPDATE inbox_sync_state SET last_success_at = $2 WHERE account_id = $1").bind(account).bind(at).execute(&f.h.state.pool).await.unwrap();
}

#[sqlx::test]
async fn a_step_waits_while_the_inbox_sync_is_stale_and_goes_when_it_is_fresh(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.h.account("a@x.test").await; // the default gate: 15 minutes
    make_readable(&f, a).await;
    let seq = f.sequence(&[0, 1]).await;
    let e = f.enroll(seq, a, "lead@company.com").await;
    // Step 1 is not gated (nothing to answer yet).
    f.until_sent(e, 1).await;
    f.h.advance(Duration::days(1));
    synced(&f, a, f.h.now() - hours(3)).await;
    f.fu_tick().await;
    f.h.tick().await;
    let j2 = f.jobs(e).await[1];
    assert_eq!(f.status(e).await, "waiting_send");
    assert_eq!(f.h.job(j2).await.status, JobStatus::Queued, "the stale sync holds the step");
    assert!(f.h.events(j2).await.contains(&"deferred".to_string()));
    f.h.advance(Duration::minutes(6));
    synced(&f, a, f.h.now()).await;
    f.h.tick().await;
    assert_eq!(f.h.job(j2).await.status, JobStatus::Sent);
    assert_eq!(f.status(e).await, "finished");
}

#[sqlx::test]
async fn an_account_paused_for_eight_days_fails_the_follow_up_as_stalled(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1]).await;
    let e = f.enroll(seq, a, "lead@company.com").await;
    f.until_sent(e, 1).await;
    sqlx::query("UPDATE account_send_state SET paused = true, paused_reason = 'test' WHERE account_id = $1").bind(a).execute(&f.h.state.pool).await.unwrap();
    f.h.advance(Duration::days(2));
    f.fu_tick().await;
    let j2 = f.jobs(e).await[1];
    assert_eq!(f.status(e).await, "waiting_send");
    f.h.advance(Duration::days(6) + hours(23));
    f.round(Duration::minutes(1)).await;
    assert_eq!(f.status(e).await, "waiting_send", "not yet 7 days");
    f.h.advance(Duration::minutes(70));
    let r = f.fu_tick().await;
    assert_eq!(r.stalled, 1);
    assert_eq!(f.status(e).await, "failed");
    assert_eq!(f.reason(e).await.as_deref(), Some("stalled"));
    assert_eq!(f.h.job(j2).await.status, JobStatus::Cancelled, "the late job is cancelled");
    let s = f.api("GET", "/v1/followups/status", None).await.body;
    assert_eq!(s["stalled"], 1);
}

#[sqlx::test]
async fn step_one_is_never_failed_as_stalled(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1]).await;
    let e = f.enroll(seq, a, "lead@company.com").await;
    sqlx::query("UPDATE account_send_state SET paused = true, paused_reason = 'test' WHERE account_id = $1").bind(a).execute(&f.h.state.pool).await.unwrap();
    f.fu_tick().await;
    f.h.advance(Duration::days(30));
    assert_eq!(f.fu_tick().await.stalled, 0);
    assert_eq!(f.status(e).await, "waiting_send");
}

#[sqlx::test]
async fn the_max_wait_setting_is_used(pool: PgPool) {
    let f = F::new(pool).await;
    f.api("PATCH", "/v1/followups/settings", Some(json!({ "max_wait_days": 2 }))).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1]).await;
    let e = f.enroll(seq, a, "lead@company.com").await;
    f.until_sent(e, 1).await;
    sqlx::query("UPDATE account_send_state SET paused = true, paused_reason = 'test' WHERE account_id = $1").bind(a).execute(&f.h.state.pool).await.unwrap();
    f.h.advance(Duration::days(2));
    f.fu_tick().await;
    f.h.advance(Duration::days(2) + Duration::minutes(1));
    assert_eq!(f.fu_tick().await.stalled, 1);
}

#[sqlx::test]
async fn a_follow_up_goes_before_a_new_first_email_when_the_account_has_both(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1]).await;
    let old = f.enroll(seq, a, "old@company1.com").await;
    f.until_sent(old, 1).await;
    // A day later: the follow-up of `old` is due, and a new lead is due at the same moment.
    f.h.advance(Duration::days(1));
    let new = f.enroll(seq, a, "new@company2.com").await;
    f.fu_tick().await;
    let (follow_up, first) = (f.jobs(old).await[1], f.jobs(new).await[0]);
    // The first email was created first in time order here, so only the setting can put the follow-up in front.
    sqlx::query("UPDATE send_jobs SET created_at = created_at - interval '1 hour' WHERE id = $1").bind(first).execute(&f.h.state.pool).await.unwrap();
    f.h.tick().await;
    assert_eq!(f.h.job(follow_up).await.status, JobStatus::Sent, "the thread that started goes first");
    assert_eq!(f.h.job(first).await.status, JobStatus::Queued);

}

#[sqlx::test]
async fn followups_first_can_be_turned_off(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    f.h.settings(a, json!({ "followups_first": false })).await;
    let seq = f.sequence(&[0, 1]).await;
    let old = f.enroll(seq, a, "old@company1.com").await;
    f.until_sent(old, 1).await;
    f.h.advance(Duration::days(1));
    let new = f.enroll(seq, a, "new@company2.com").await;
    f.fu_tick().await;
    let (follow_up, first) = (f.jobs(old).await[1], f.jobs(new).await[0]);
    sqlx::query("UPDATE send_jobs SET created_at = created_at - interval '1 hour' WHERE id = $1").bind(first).execute(&f.h.state.pool).await.unwrap();
    f.h.tick().await;
    assert_eq!(f.h.job(first).await.status, JobStatus::Sent, "oldest first");
    assert_eq!(f.h.job(follow_up).await.status, JobStatus::Queued);
}

// =====================================================================================================
// Phase 7: stop and pause
// =====================================================================================================

/// An enrollment whose step 1 is sent, with the id that the fixtures point to. Lead: jane@prospect.example.
async fn after_step_one(f: &F, delays: &[i32]) -> (Uuid, Uuid, Uuid) {
    let a = f.account(ME_ACCOUNT).await;
    let seq = f.sequence(delays).await;
    let e = f.enroll(seq, a, JANE).await;
    let j1 = f.until_sent(e, 1).await;
    f.set_message_id(j1, FIXTURE_ID).await;
    (a, e, j1)
}

const ME_ACCOUNT: &str = "sender@acme-outreach.example";

#[sqlx::test]
async fn a_reply_between_step_one_and_two_means_step_two_is_never_made(pool: PgPool) {
    let f = F::new(pool).await;
    let (a, e, _) = after_step_one(&f, &[0, 1, 1]).await;
    f.ingest(a, "p1", &fixture("human_plain.eml")).await;
    assert_eq!(f.status(e).await, "stopped");
    assert_eq!(f.reason(e).await.as_deref(), Some("reply"));
    f.h.advance(Duration::days(10));
    let r = f.fu_tick().await;
    assert_eq!((r.queued, r.stopped), (0, 0));
    assert_eq!(f.jobs(e).await.len(), 1, "step 2 was never made");
    f.h.tick().await;
    assert_eq!(f.h.sender.calls().len(), 1);
}

#[sqlx::test]
async fn a_reply_while_step_two_is_queued_cancels_the_job_and_stops_the_row(pool: PgPool) {
    let f = F::new(pool).await;
    let (a, e, _) = after_step_one(&f, &[0, 1, 1]).await;
    f.h.advance(Duration::days(2));
    f.fu_tick().await;
    let j2 = f.jobs(e).await[1];
    assert_eq!(f.h.job(j2).await.status, JobStatus::Queued);
    f.ingest(a, "p1", &fixture("human_plain.eml")).await;
    assert_eq!(f.h.job(j2).await.status, JobStatus::Cancelled);
    assert_eq!(f.status(e).await, "stopped");
    f.h.tick().await;
    assert_eq!(f.h.sender.calls().len(), 1, "the cancelled job never goes out");
    let d = f.detail(e).await;
    let stopped = d["events"].as_array().unwrap().iter().find(|ev| ev["kind"] == "stopped").unwrap();
    assert_eq!(stopped["detail"]["reason"], "reply");
    assert_eq!(stopped["detail"]["cancelled_job"], j2.to_string());
    // The reply is stored with what the follow-up step did.
    let actions: (Value,) = sqlx::query_as("SELECT actions FROM inbound_messages WHERE label = 'reply'").fetch_one(&f.h.state.pool).await.unwrap();
    let text = actions.0.to_string();
    assert!(text.contains("enrollments_stopped") && text.contains(&e.to_string()), "{text}");
}

#[sqlx::test]
async fn the_reply_check_leaves_the_cancel_of_a_follow_up_job_to_the_follow_up_step(pool: PgPool) {
    let f = F::new(pool).await;
    let (a, e, _) = after_step_one(&f, &[0, 1]).await;
    // A plain automated job to the same lead (no enrollment) is still cancelled by the Reply Check itself.
    let plain = queued_job(&f.h.state, a, JANE, "automated").await;
    f.h.advance(Duration::days(2));
    f.fu_tick().await;
    f.ingest(a, "p1", &fixture("human_plain.eml")).await;
    assert_eq!(job_status(&f.h.state, plain).await, "cancelled");
    assert_eq!(job_status(&f.h.state, f.jobs(e).await[1]).await, "cancelled");
    assert_eq!(f.status(e).await, "stopped");
}

#[sqlx::test]
async fn the_same_reply_twice_runs_the_stop_once(pool: PgPool) {
    let f = F::new(pool).await;
    let (a, e, _) = after_step_one(&f, &[0, 1]).await;
    f.ingest(a, "p1", &fixture("human_plain.eml")).await;
    f.ingest(a, "p1", &fixture("human_plain.eml")).await;
    f.ingest(a, "p1", &fixture("human_plain.eml")).await;
    let kinds = f.event_kinds(e).await;
    assert_eq!(kinds.iter().filter(|k| *k == "stopped").count(), 1, "{kinds:?}");
    assert_eq!(count(&f.h.state, "SELECT count(*) FROM inbound_messages").await, 1);
}

#[sqlx::test]
async fn a_reply_and_the_claim_of_the_next_step_meet_at_the_same_moment(pool: PgPool) {
    // The race: the reply is stored while the worker is about to claim step 2.
    // The stop holds the lock of the enrollment, so the claim leaves the job alone; then the job is cancelled.
    let f = F::new(pool).await;
    let (a, e, _) = after_step_one(&f, &[0, 1]).await;
    f.h.advance(Duration::days(2));
    f.fu_tick().await;
    let j2 = f.jobs(e).await[1];
    // The stop has started: it holds the enrollment row.
    let mut stop_tx = f.h.state.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM enrollments WHERE id = $1 FOR UPDATE").bind(e).execute(&mut *stop_tx).await.unwrap();
    let before = f.h.sender.calls().len();
    f.h.tick().await;
    assert_eq!(f.h.sender.calls().len(), before, "the claim did not send while the stop was running");
    assert_eq!(f.h.job(j2).await.status, JobStatus::Queued);
    stop_tx.rollback().await.unwrap();
    // Now the real stop comes through the reply check.
    f.ingest(a, "p1", &fixture("human_plain.eml")).await;
    f.h.tick().await;
    assert_eq!(f.h.sender.calls().len(), before, "the follow-up never went out");
    assert_eq!(f.h.job(j2).await.status, JobStatus::Cancelled);
}

#[sqlx::test]
async fn a_reply_after_the_worker_claimed_the_step_only_stops_what_comes_next(pool: PgPool) {
    let f = F::new(pool).await;
    let (a, e, _) = after_step_one(&f, &[0, 1, 1]).await;
    f.h.advance(Duration::days(2));
    f.fu_tick().await;
    let j2 = f.jobs(e).await[1];
    // The worker has claimed step 2: it is being sent, and a job in that state cannot be called back.
    sqlx::query("UPDATE send_jobs SET status = 'sending', claimed_at = $2 WHERE id = $1").bind(j2).bind(f.h.now()).execute(&f.h.state.pool).await.unwrap();
    f.ingest(a, "p1", &fixture("human_plain.eml")).await;
    assert_eq!(f.status(e).await, "stopped");
    assert_eq!(job_status(&f.h.state, j2).await, "sending", "not touched");
    // The result comes in afterwards: it is written down, and nothing else moves. Step 3 is never made.
    let job = f.h.job(j2).await;
    let mut tx = f.h.state.pool.begin().await.unwrap();
    aikido_email::followups::on_job_result(&mut tx, e, &job, aikido_email::followups::JobOutcome::Sent, f.h.now()).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(f.status(e).await, "stopped");
    assert!(f.event_kinds(e).await.contains(&"step_sent_after_end".to_string()));
    f.h.advance(Duration::days(10));
    f.fu_tick().await;
    assert_eq!(f.jobs(e).await.len(), 2);
}

#[sqlx::test]
async fn an_auto_reply_pauses_for_three_business_days_then_the_enrollment_goes_on(pool: PgPool) {
    let f = F::new(pool).await;
    // Monday 2026-10-19: step 1 goes out. The out-of-office arrives the same day.
    let (a, e, _) = after_step_one(&f, &[0, 1, 1]).await;
    let now = f.h.now();
    f.ingest(a, "p1", &fixture("gmail_vacation.eml")).await;
    let d = f.detail(e).await;
    assert_eq!(d["enrollment"]["status"], "paused");
    let until: DateTime<Utc> = d["enrollment"]["pause_until"].as_str().unwrap().parse().unwrap();
    // Three business days later, at the same time of day.
    let expected = aikido_email::followups::schedule::add_delay(chrono_tz::Tz::UTC, now, 3, aikido_email::followups::model::DelayType::BusinessDays);
    assert_eq!(until, expected);
    assert!(until > now + hours(48));
    // Nothing is made while it is paused, also when the step is due.
    f.h.set_time(until - Duration::minutes(1));
    f.fu_tick().await;
    assert_eq!(f.jobs(e).await.len(), 1);
    assert_eq!(f.status(e).await, "paused");
    // The pause ends: the enrollment resumes and the due step is made.
    f.h.advance(Duration::minutes(2));
    let r = f.fu_tick().await;
    assert_eq!((r.resumed, r.queued), (1, 1), "{r:?}");
    assert_eq!(f.status(e).await, "waiting_send");
    f.until_sent(e, 2).await;
    f.until_sent(e, 3).await;
    assert_eq!(f.status(e).await, "finished");
    // An auto-reply is not a reply: no suppression.
    assert_eq!(count(&f.h.state, "SELECT count(*) FROM suppressions").await, 0);
    let kinds = f.event_kinds(e).await;
    assert!(kinds.contains(&"paused".to_string()) && kinds.contains(&"resumed".to_string()), "{kinds:?}");
}

#[sqlx::test]
async fn an_auto_reply_while_the_next_step_is_queued_takes_it_back(pool: PgPool) {
    let f = F::new(pool).await;
    let (a, e, _) = after_step_one(&f, &[0, 1]).await;
    f.h.advance(Duration::days(2));
    f.fu_tick().await;
    let j2 = f.jobs(e).await[1];
    f.ingest(a, "p1", &fixture("gmail_vacation.eml")).await;
    assert_eq!(f.status(e).await, "paused");
    assert_eq!(f.h.job(j2).await.status, JobStatus::Cancelled);
    let d = f.detail(e).await;
    assert!(d["enrollment"]["current_job_id"].is_null());
    // After the pause the step is made again, as a new job.
    f.h.advance(Duration::days(6));
    f.fu_tick().await;
    f.fu_tick().await;
    let jobs = f.jobs(e).await;
    assert_eq!(jobs.len(), 3, "step 1, the cancelled step 2, and the step 2 that was made again");
    assert_eq!(f.h.job(jobs[2]).await.status, JobStatus::Queued);
    assert_eq!(f.h.job(jobs[2]).await.step_number, Some(2));
    assert_ne!(jobs[1], jobs[2]);
    assert_eq!(f.status(e).await, "waiting_send");
    f.until_sent(e, 3).await;
    assert_eq!(f.status(e).await, "finished");
}

#[sqlx::test]
async fn a_sequence_can_stop_on_an_auto_reply_instead(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account(ME_ACCOUNT).await;
    let seq = f.sequence_from(json!({ "name": "s", "stop_on_auto_reply": true, "steps": [step(0, "Hi", "One"), step(1, "", "Two")] })).await;
    let e = f.enroll(seq, a, JANE).await;
    let j1 = f.until_sent(e, 1).await;
    f.set_message_id(j1, FIXTURE_ID).await;
    f.ingest(a, "p1", &fixture("gmail_vacation.eml")).await;
    assert_eq!(f.status(e).await, "stopped");
    assert_eq!(f.reason(e).await.as_deref(), Some("auto_reply"));
    assert_eq!(count(&f.h.state, "SELECT count(*) FROM suppressions").await, 0, "still no suppression");
}

#[sqlx::test]
async fn the_reply_check_setting_that_stops_on_auto_replies_stops_the_enrollment_too(pool: PgPool) {
    let f = F::new(pool).await;
    let (a, e, _) = after_step_one(&f, &[0, 1]).await;
    sqlx::query("UPDATE account_reply_settings SET settings = settings || '{\"on_auto_reply\": \"stop\"}'::jsonb WHERE account_id = $1").bind(a).execute(&f.h.state.pool).await.unwrap();
    f.ingest(a, "p1", &fixture("gmail_vacation.eml")).await;
    assert_eq!(f.status(e).await, "stopped");
    assert_eq!(suppression_reason(&f.h.state, JANE).await.as_deref(), Some("replied"));
}

async fn stops_on(pool: PgPool, fixture_name: &str, reason: &str) {
    let f = F::new(pool).await;
    let (a, e, _) = after_step_one(&f, &[0, 1]).await;
    f.ingest(a, "p1", &fixture(fixture_name)).await;
    assert_eq!(f.status(e).await, "stopped", "{reason}");
    assert_eq!(f.reason(e).await.as_deref(), Some(reason));
    f.h.advance(Duration::days(5));
    assert_eq!(f.fu_tick().await.queued, 0);
    assert_eq!(f.jobs(e).await.len(), 1);
}

#[sqlx::test]
async fn an_unsubscribe_stops_the_enrollment(pool: PgPool) {
    stops_on(pool, "unsubscribe_short.eml", "unsubscribe").await;
}

#[sqlx::test]
async fn a_hard_bounce_that_comes_back_later_stops_the_enrollment(pool: PgPool) {
    stops_on(pool, "gmail_bounce_user_unknown.eml", "bounce_hard").await;
}

#[sqlx::test]
async fn a_spam_complaint_stops_the_enrollment(pool: PgPool) {
    stops_on(pool, "spam_complaint.eml", "complaint").await;
}

#[sqlx::test]
async fn a_blocked_message_stops_the_enrollment(pool: PgPool) {
    stops_on(pool, "outlook_bounce_5_7_1.eml", "bounce_block").await;
}

#[sqlx::test]
async fn a_soft_bounce_or_a_delay_notice_changes_nothing(pool: PgPool) {
    let f = F::new(pool).await;
    let (a, e, _) = after_step_one(&f, &[0, 1]).await;
    f.ingest(a, "p1", &fixture("soft_bounce_4_4_7.eml")).await;
    f.ingest(a, "p2", &fixture("delayed_notice.eml")).await;
    assert_eq!(f.status(e).await, "active");
    f.h.advance(Duration::days(2));
    assert_eq!(f.fu_tick().await.queued, 1, "the engine retries on its own");
}

#[sqlx::test]
async fn a_reply_stops_every_open_enrollment_of_the_address_in_all_sequences(pool: PgPool) {
    let f = F::new(pool).await;
    f.api("PATCH", "/v1/followups/settings", Some(json!({ "one_active_per_email": false }))).await;
    let a = f.account(ME_ACCOUNT).await;
    let (s1, s2) = (f.sequence(&[0, 1]).await, f.sequence(&[0, 2]).await);
    let e1 = f.enroll(s1, a, JANE).await;
    let e2 = f.enroll(s2, a, JANE).await;
    let other = f.enroll(s1, a, "someone@else.example").await;
    let j1 = f.until_sent(e1, 1).await;
    f.set_message_id(j1, FIXTURE_ID).await;
    f.ingest(a, "p1", &fixture("human_plain.eml")).await;
    for e in [e1, e2] {
        assert_eq!(f.status(e).await, "stopped");
        assert_eq!(f.reason(e).await.as_deref(), Some("reply"));
    }
    assert_ne!(f.status(other).await, "stopped", "another address is not touched");
    // The second enrollment had a queued or sent step 1 job: if it was queued it is cancelled.
    for j in f.jobs(e2).await {
        assert_ne!(job_status(&f.h.state, j).await, "queued");
    }
}

#[sqlx::test]
async fn an_auto_reply_pauses_only_the_enrollment_that_was_answered(pool: PgPool) {
    let f = F::new(pool).await;
    f.api("PATCH", "/v1/followups/settings", Some(json!({ "one_active_per_email": false }))).await;
    let a = f.account(ME_ACCOUNT).await;
    let (s1, s2) = (f.sequence(&[0, 1]).await, f.sequence(&[0, 2]).await);
    let e1 = f.enroll(s1, a, JANE).await;
    let j1 = f.until_sent(e1, 1).await;
    f.set_message_id(j1, FIXTURE_ID).await;
    let e2 = f.enroll(s2, a, JANE).await;
    f.ingest(a, "p1", &fixture("gmail_vacation.eml")).await;
    assert_eq!(f.status(e1).await, "paused");
    assert_ne!(f.status(e2).await, "paused");
}

#[sqlx::test]
async fn a_domain_stop_of_the_reply_check_stops_the_whole_company(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account(ME_ACCOUNT).await;
    sqlx::query("UPDATE account_reply_settings SET settings = settings || '{\"stop_scope\": \"domain\"}'::jsonb WHERE account_id = $1").bind(a).execute(&f.h.state.pool).await.unwrap();
    let seq = f.sequence(&[0, 1]).await;
    let jane = f.enroll(seq, a, JANE).await;
    let colleague = f.enroll(seq, a, "tom@prospect.example").await;
    let other = f.enroll(seq, a, "ann@other.example").await;
    let j1 = f.until_sent(jane, 1).await;
    f.set_message_id(j1, FIXTURE_ID).await;
    f.ingest(a, "p1", &fixture("human_plain.eml")).await;
    assert_eq!(f.status(jane).await, "stopped");
    assert_eq!(f.status(colleague).await, "stopped", "same company");
    assert_ne!(f.status(other).await, "stopped");
}

#[sqlx::test]
async fn the_scheduler_guard_stops_a_row_the_reply_check_marked_but_did_not_stop(pool: PgPool) {
    // Second guard: the enrollment is still `active`, but a job of it carries `replied_at`.
    let f = F::new(pool).await;
    let (_, e, j1) = after_step_one(&f, &[0, 1]).await;
    sqlx::query("UPDATE send_jobs SET replied_at = $2 WHERE id = $1").bind(j1).bind(f.h.now()).execute(&f.h.state.pool).await.unwrap();
    f.h.advance(Duration::days(2));
    let r = f.fu_tick().await;
    assert_eq!((r.queued, r.stopped), (0, 1));
    assert_eq!(f.reason(e).await.as_deref(), Some("reply"));
    assert_eq!(f.jobs(e).await.len(), 1);
}

#[sqlx::test]
async fn a_hard_bounce_mark_on_a_job_also_stops_the_row_at_due_time(pool: PgPool) {
    let f = F::new(pool).await;
    let (_, e, j1) = after_step_one(&f, &[0, 1]).await;
    sqlx::query("UPDATE send_jobs SET bounced_at = $2, bounce_kind = 'hard' WHERE id = $1").bind(j1).bind(f.h.now()).execute(&f.h.state.pool).await.unwrap();
    f.h.advance(Duration::days(2));
    f.fu_tick().await;
    assert_eq!(f.reason(e).await.as_deref(), Some("bounce_hard"));
}

// =====================================================================================================
// Phase 8: the routes
// =====================================================================================================

#[sqlx::test]
async fn pause_and_resume_one_enrollment(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1]).await;
    let e = f.enroll(seq, a, "lead@company.com").await;
    f.fu_tick().await;
    let j1 = f.jobs(e).await[0];
    // Pause while the step is queued: the job is called back.
    let r = f.api("POST", &format!("/v1/followups/enrollments/{e}/pause"), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["enrollment"]["status"], "paused");
    assert!(r.body["enrollment"]["pause_until"].is_null(), "a person resumes it");
    assert_eq!(f.h.job(j1).await.status, JobStatus::Cancelled);
    f.h.advance(Duration::days(30));
    assert_eq!(f.fu_tick().await, TickReport::default(), "paused for good until a person resumes");
    // Pausing again is fine.
    assert_eq!(f.api("POST", &format!("/v1/followups/enrollments/{e}/pause"), None).await.status, StatusCode::OK);
    // Resume: the due time passed long ago, so the step is due now.
    let r = f.api("POST", &format!("/v1/followups/enrollments/{e}/resume"), None).await;
    assert_eq!(r.body["enrollment"]["status"], "active");
    let due: DateTime<Utc> = r.body["enrollment"]["next_due_at"].as_str().unwrap().parse().unwrap();
    assert_eq!(due, f.h.now(), "recomputed from now");
    assert_eq!(f.fu_tick().await.queued, 1);
    assert_eq!(f.jobs(e).await.len(), 2, "a new job for the same step");
    assert_eq!(f.api("POST", &format!("/v1/followups/enrollments/{e}/resume"), None).await.status, StatusCode::OK, "resume of a running one is a no-op");
    // The first job was called back, so the second job in the list is step 1 again; then comes step 2.
    f.until_sent(e, 2).await;
    assert_eq!(f.detail(e).await["enrollment"]["next_step"], 2);
    f.until_sent(e, 3).await;
    assert_eq!(f.status(e).await, "finished");
    // A finished enrollment cannot be paused, resumed or stopped.
    for what in ["pause", "resume", "stop"] {
        let r = f.api("POST", &format!("/v1/followups/enrollments/{e}/{what}"), None).await;
        assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::CONFLICT, Some("enrollment_closed")), "{what}");
    }
}

#[sqlx::test]
async fn a_future_due_time_is_kept_on_resume(pool: PgPool) {
    let f = F::new(pool).await;
    let (_, e, _) = after_step_one(&f, &[0, 3]).await;
    let before = f.detail(e).await["enrollment"]["next_due_at"].clone();
    f.api("POST", &format!("/v1/followups/enrollments/{e}/pause"), None).await;
    let r = f.api("POST", &format!("/v1/followups/enrollments/{e}/resume"), None).await;
    assert_eq!(r.body["enrollment"]["next_due_at"], before, "a time in the future stays");
}

#[sqlx::test]
async fn stop_one_enrollment_by_hand(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1]).await;
    let e = f.enroll(seq, a, "lead@company.com").await;
    f.fu_tick().await;
    let j1 = f.jobs(e).await[0];
    let r = f.api("POST", &format!("/v1/followups/enrollments/{e}/stop"), None).await;
    assert_eq!(r.body["enrollment"]["status"], "stopped");
    assert_eq!(r.body["enrollment"]["stop_reason"], "manual");
    assert_eq!(f.h.job(j1).await.status, JobStatus::Cancelled);
    assert_eq!(f.api("POST", &format!("/v1/followups/enrollments/{e}/stop"), None).await.status, StatusCode::OK, "stopping twice is fine");
    assert_eq!(f.api("POST", &format!("/v1/followups/enrollments/{e}/resume"), None).await.status, StatusCode::CONFLICT, "stopped never changes again");
    assert_eq!(f.api("POST", &format!("/v1/followups/enrollments/{}/stop", Uuid::now_v7()), None).await.status, StatusCode::NOT_FOUND);
    // The address can be enrolled again.
    f.enroll(seq, a, "lead@company.com").await;
}

#[sqlx::test]
async fn enrollments_are_listed_and_filtered(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let (s1, s2) = (f.sequence(&[0]).await, f.sequence(&[0]).await);
    let e1 = f.enroll(s1, a, "one@company.com").await;
    let e2 = f.enroll(s1, a, "two@company.com").await;
    let e3 = f.enroll(s2, a, "three@company.com").await;
    f.api("POST", &format!("/v1/followups/enrollments/{e2}/stop"), None).await;
    let ids = |r: &Response| -> Vec<String> { r.body["enrollments"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap().to_string()).collect() };
    let all = f.api("GET", "/v1/followups/enrollments", None).await;
    assert_eq!(ids(&all), vec![e3.to_string(), e2.to_string(), e1.to_string()], "newest first");
    assert_eq!(ids(&f.api("GET", &format!("/v1/followups/enrollments?sequence_id={s1}"), None).await), vec![e2.to_string(), e1.to_string()]);
    assert_eq!(ids(&f.api("GET", "/v1/followups/enrollments?status=stopped", None).await), vec![e2.to_string()]);
    assert_eq!(ids(&f.api("GET", "/v1/followups/enrollments?email=THREE@company.com", None).await), vec![e3.to_string()]);
    let page = f.api("GET", "/v1/followups/enrollments?limit=2", None).await;
    assert_eq!(ids(&page).len(), 2);
    let next = page.body["next_cursor"].as_str().unwrap();
    assert_eq!(ids(&f.api("GET", &format!("/v1/followups/enrollments?limit=2&cursor={next}"), None).await), vec![e1.to_string()]);
    assert_eq!(f.api("GET", "/v1/followups/enrollments?status=sleeping", None).await.status, StatusCode::BAD_REQUEST);
    assert_eq!(f.api("GET", &format!("/v1/followups/enrollments/{}", Uuid::now_v7()), None).await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn the_status_route_counts_and_looks_ahead(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1]).await;
    let s0 = f.api("GET", "/v1/followups/status", None).await.body;
    assert_eq!((s0["paused"].as_bool(), s0["scheduler_enabled"].as_bool(), s0["counts"].clone(), s0["last_tick_at"].is_null()), (Some(false), Some(false), json!({}), true));
    let e = f.enroll(seq, a, "lead@company.com").await;
    f.enroll(seq, a, "two@company.com").await;
    let s1 = f.api("GET", "/v1/followups/status", None).await.body;
    assert_eq!(s1["counts"], json!({ "active": 2 }));
    assert_eq!(s1["due_next_hour"], 2);
    f.until_sent(e, 1).await;
    let s2 = f.api("GET", "/v1/followups/status", None).await.body;
    assert!(s2["last_tick_at"].is_string());
    assert_eq!(s2["due_next_hour"], 0, "the next steps are a day away");
    assert_eq!(s2["stalled"], 0);
}

#[sqlx::test]
async fn deleting_the_account_fails_its_open_enrollments(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1]).await;
    let e1 = f.enroll(seq, a, "one@company.com").await;
    let e2 = f.enroll(seq, a, "two@company.com").await;
    f.fu_tick().await;
    let r = call(&f.h.state, "DELETE", &format!("/v1/accounts/{a}"), Some(&f.keys[&Scope::Accounts]), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    for e in [e1, e2] {
        assert_eq!(f.status(e).await, "failed");
        assert_eq!(f.reason(e).await.as_deref(), Some("account_deleted"));
    }
    let _ = (e1, e2);
}

#[sqlx::test]
async fn an_account_deleted_before_the_step_is_due_fails_the_row(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0]).await;
    let e = f.enroll(seq, a, "one@company.com").await;
    sqlx::query("UPDATE email_accounts SET deleted_at = now() WHERE id = $1").bind(a).execute(&f.h.state.pool).await.unwrap();
    let r = f.fu_tick().await;
    assert_eq!(r.failed, 1);
    assert_eq!(f.reason(e).await.as_deref(), Some("account_deleted"));
}

#[sqlx::test]
async fn an_enrollment_with_a_broken_template_fails_instead_of_sending_a_broken_email(pool: PgPool) {
    // The text was fine when the enrollment was made, and a direct database edit broke it. Nothing is sent.
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0]).await;
    let e = f.enroll(seq, a, "one@company.com").await;
    sqlx::query("UPDATE sequence_steps SET body_text = 'Hi {{company}}' WHERE sequence_id = $1").bind(seq).execute(&f.h.state.pool).await.unwrap();
    let r = f.fu_tick().await;
    assert_eq!((r.queued, r.failed), (0, 1));
    assert_eq!(f.reason(e).await.as_deref(), Some("missing_variable"));
    let d = f.detail(e).await;
    let failed = d["events"].as_array().unwrap().iter().find(|ev| ev["kind"] == "failed").unwrap();
    assert!(failed["detail"]["message"].as_str().unwrap().contains("company"));
    assert_eq!(count(&f.h.state, "SELECT count(*) FROM send_jobs").await, 0);
}

// =====================================================================================================
// No email text and no secret in the logs
// =====================================================================================================

#[derive(Clone, Default)]
struct LogBuffer(Arc<std::sync::Mutex<Vec<u8>>>);

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
async fn no_secret_and_no_email_text_reach_the_logs(pool: PgPool) {
    let logs = LogBuffer::default();
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt().with_max_level(tracing::Level::TRACE).with_ansi(false).with_writer(logs.clone()).finish(),
    )
    .expect("this test binary sets the global logger once");

    let f = F::new(pool).await;
    let a = f.account(ME_ACCOUNT).await;
    let seq = f
        .sequence_from(json!({ "name": "log", "steps": [
            step(0, "Subject-SECRET-{{first_name|x}}", "Body-SECRET-one for {{first_name|x}}"),
            step(1, "", "Body-SECRET-two"),
        ] }))
        .await;
    let e = f.enroll_with(seq, a, JANE, json!({ "first_name": "Variable-SECRET" })).await;
    let j1 = f.until_sent(e, 1).await;
    f.set_message_id(j1, FIXTURE_ID).await;
    f.h.advance(Duration::days(2));
    f.fu_tick().await;
    f.ingest(a, "p1", &fixture("gmail_vacation.eml")).await; // pause + cancel
    f.api("POST", &format!("/v1/followups/enrollments/{e}/resume"), None).await;
    f.until_sent(e, 3).await; // jobs: step 1, step 2 (called back), step 2 again
    // Errors on the way: a bad key, a bad request, a missing variable.
    call(&f.h.state, "GET", "/v1/followups/status", Some("aik_followups_logcheck-WRONG-KEY"), None).await;
    f.api("POST", &format!("/v1/followups/sequences/{seq}/enrollments"), Some(json!({ "enrollments": [{ "to_email": "x@y.com", "account_id": a, "variables": { "k": "Variable-SECRET-2" } }] }))).await;

    let log = String::from_utf8_lossy(&logs.0.lock().unwrap_or_else(|e| e.into_inner())).to_string();
    assert!(log.contains("email sent"), "the logger captured the engine: {log}");
    let mut secrets: Vec<String> = f.keys.values().cloned().collect();
    secrets.extend(["Subject-SECRET", "Body-SECRET", "Variable-SECRET", "aik_followups_logcheck-WRONG-KEY", "super-secret-password"].map(String::from));
    for secret in secrets {
        assert!(!log.contains(&secret), "{secret:?} is in the logs");
    }
}
