//! Lead Management meets the other engines: enroll by lead, the lead hooks (sent, reply, bounce, finish),
//! the backfill, live variables, and the field delete guard.
mod common;
use aikido_email::auth::api_keys::Scope;
use aikido_email::followups::scheduler::{self, FollowupEngine};
use aikido_email::sending::queue::JobStatus;
use axum::http::StatusCode;
use chrono::Duration;
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

const FIXTURE_ID: &str = "<sent-0001@acme-outreach.example>";
const JANE: &str = "jane@prospect.example";
const ME_ACCOUNT: &str = "sender@acme-outreach.example";

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

    async fn account(&self, email: &str) -> Uuid {
        let a = self.h.account(email).await;
        self.h.settings(a, json!({ "reply_check_max_age_mins": 0 })).await;
        a
    }

    async fn fapi(&self, method: &str, uri: &str, body: Option<Value>) -> Response {
        call(&self.h.state, method, uri, Some(&self.keys[&Scope::Followups]), body).await
    }

    async fn lapi(&self, method: &str, uri: &str, body: Option<Value>) -> Response {
        call(&self.h.state, method, uri, Some(&self.keys[&Scope::Leads]), body).await
    }

    async fn lok(&self, method: &str, uri: &str, body: Option<Value>) -> Value {
        let r = self.lapi(method, uri, body).await;
        assert!(r.status.is_success(), "{method} {uri} -> {} {}", r.status, r.body);
        r.body
    }

    /// Steps: each delay is a step. Subject of step 1: `Hello {{first_name|there}}`. Bodies: `Step n for {{first_name|friend}}`.
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
        let r = self.fapi("POST", "/v1/followups/sequences", Some(body)).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        r.body["sequence"]["id"].as_str().unwrap().parse().unwrap()
    }

    async fn enroll_raw(&self, seq: Uuid, item: Value) -> Response {
        self.fapi("POST", &format!("/v1/followups/sequences/{seq}/enrollments"), Some(json!({ "enrollments": [item] }))).await
    }

    async fn enroll(&self, seq: Uuid, account: Uuid, email: &str) -> Uuid {
        let r = self.enroll_raw(seq, json!({ "to_email": email, "account_id": account })).await;
        assert_eq!(r.body["enrolled"], 1, "{}", r.body);
        r.body["items"][0]["enrollment"]["id"].as_str().unwrap().parse().unwrap()
    }

    async fn status(&self, e: Uuid) -> String {
        self.fapi("GET", &format!("/v1/followups/enrollments/{e}"), None).await.body["enrollment"]["status"].as_str().unwrap().to_string()
    }

    async fn jobs(&self, e: Uuid) -> Vec<Uuid> {
        let rows: Vec<(Uuid,)> = sqlx::query_as("SELECT id FROM send_jobs WHERE enrollment_id = $1 ORDER BY step_number").bind(e).fetch_all(&self.h.state.pool).await.unwrap();
        rows.into_iter().map(|r| r.0).collect()
    }

    async fn round(&self, step: Duration) {
        scheduler::tick(&self.fe).await.unwrap();
        self.h.tick().await;
        self.h.advance(step);
    }

    async fn until_sent(&self, e: Uuid, n: usize) -> Uuid {
        for _ in 0..1500 {
            let jobs = self.jobs(e).await;
            if let Some(j) = jobs.get(n - 1) {
                if self.h.job(*j).await.status == JobStatus::Sent {
                    return *j;
                }
            }
            self.round(Duration::minutes(10)).await;
        }
        panic!("step {n} never sent");
    }

    async fn set_message_id(&self, job: Uuid, id: &str) {
        sqlx::query("UPDATE send_jobs SET message_id_header = $2 WHERE id = $1").bind(job).bind(id).execute(&self.h.state.pool).await.unwrap();
    }

    async fn ingest(&self, account: Uuid, raw: &[u8]) {
        ingest_raw(&self.h.state, account, "p1", raw, self.h.now()).await;
    }

    async fn lead_by_email(&self, email: &str) -> Value {
        let r = self.lok("GET", &format!("/v1/leads?q={}&limit=1", email.replace('@', "%40")), None).await;
        r["leads"][0].clone()
    }

    async fn events(&self, lead: &Value) -> Vec<String> {
        let id = lead["id"].as_str().unwrap();
        let r = self.lok("GET", &format!("/v1/leads/{id}/events?limit=500"), None).await;
        r["events"].as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap().to_string()).collect()
    }

    async fn state_of(&self, email: &str) -> String {
        self.lead_by_email(email).await["contact_state"].as_str().unwrap().to_string()
    }

    /// Step 1 sent to JANE, with the message id that the fixtures point to.
    async fn after_step_one(&self, delays: &[i32]) -> (Uuid, Uuid) {
        let a = self.account(ME_ACCOUNT).await;
        let seq = self.sequence(delays).await;
        let e = self.enroll(seq, a, JANE).await;
        let j1 = self.until_sent(e, 1).await;
        self.set_message_id(j1, FIXTURE_ID).await;
        (a, e)
    }
}

// ---------- enroll makes or finds the lead ----------

#[sqlx::test]
async fn enrolling_an_address_makes_the_lead_and_links_it(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1]).await;
    let e = f.enroll(seq, a, "New.Person@Example.com").await;
    let lead = f.lead_by_email("new.person@example.com").await;
    assert_eq!(lead["source"], "enrollment");
    assert_eq!(lead["contact_state"], "in_sequence");
    let d = f.fapi("GET", &format!("/v1/followups/enrollments/{e}"), None).await;
    assert_eq!(d.body["enrollment"]["lead_id"], lead["id"]);
    assert_eq!(f.events(&lead).await, ["enrolled", "created"]);
    let got = f.lok("GET", &format!("/v1/leads/{}", lead["id"].as_str().unwrap()), None).await;
    assert_eq!(got["active_enrollment_id"], e.to_string());
}

#[sqlx::test]
async fn enrolling_an_address_that_is_a_lead_links_the_existing_lead(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let lead = f.lok("POST", "/v1/leads", Some(json!({ "email": "ana@acme.com", "first_name": "Ana", "interest": "interested" }))).await;
    let seq = f.sequence(&[0]).await;
    let e = f.enroll(seq, a, "ANA@acme.com").await;
    let d = f.fapi("GET", &format!("/v1/followups/enrollments/{e}"), None).await;
    assert_eq!(d.body["enrollment"]["lead_id"], lead["id"]);
    // the lead's fields fill the steps
    assert_eq!(d.body["enrollment"]["variables"]["first_name"], "Ana");
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM leads").fetch_one(&f.h.state.pool).await.unwrap();
    assert_eq!(n, 1);
}

#[sqlx::test]
async fn enrolling_by_lead_id(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    f.lok("POST", "/v1/leads/fields", Some(json!({ "key": "pain_point" }))).await;
    let lead = f.lok("POST", "/v1/leads", Some(json!({ "email": "ana@acme.com", "first_name": "Ana", "custom_fields": { "pain_point": "slow sales" } }))).await;
    let id = lead["id"].as_str().unwrap();
    let seq = f.sequence_from(json!({ "name": "S", "steps": [{ "subject": "Hi {{first_name}}", "body_text": "About {{pain_point}}" }] })).await;
    let r = f.enroll_raw(seq, json!({ "lead_id": id, "account_id": a })).await;
    assert_eq!(r.body["enrolled"], 1, "{}", r.body);
    let e: Uuid = r.body["items"][0]["enrollment"]["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(r.body["items"][0]["enrollment"]["to_email"], "ana@acme.com");
    let j = f.until_sent(e, 1).await;
    let job = f.h.job(j).await;
    assert_eq!(job.subject, "Hi Ana");
    assert_eq!(job.body_text.as_deref(), Some("About slow sales"));
    assert_eq!(job.lead_id.unwrap().to_string(), id);

    // errors on their own item
    let code = |r: &Response| r.body["items"][0]["error"]["code"].as_str().unwrap_or("").to_string();
    let r = f.enroll_raw(seq, json!({ "lead_id": Uuid::now_v7(), "account_id": a })).await;
    assert_eq!(code(&r), "lead_not_found");
    let r = f.enroll_raw(seq, json!({ "lead_id": id, "to_email": "other@acme.com", "account_id": a })).await;
    assert_eq!(code(&r), "lead_email_mismatch");
    let r = f.enroll_raw(seq, json!({ "account_id": a })).await;
    assert_eq!(code(&r), "invalid_to_email");
    // a matching to_email is fine (the first enrollment finished, so the lead can be enrolled again)
    let r = f.enroll_raw(seq, json!({ "lead_id": id, "to_email": "ANA@acme.com", "account_id": a })).await;
    assert_eq!(r.body["enrolled"], 1, "{}", r.body);
    // and now it is in an open enrollment
    let r = f.enroll_raw(seq, json!({ "lead_id": id, "account_id": a })).await;
    assert_eq!(code(&r), "already_enrolled");
}

#[sqlx::test]
async fn a_missing_lead_value_blocks_the_call_unless_the_field_has_a_fallback(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    f.lok("POST", "/v1/leads/fields", Some(json!({ "key": "pain_point" }))).await;
    let lead = f.lok("POST", "/v1/leads", Some(json!({ "email": "ana@acme.com" }))).await;
    let seq = f.sequence_from(json!({ "name": "S", "steps": [{ "subject": "Hi", "body_text": "About {{pain_point}}" }] })).await;
    let r = f.enroll_raw(seq, json!({ "lead_id": lead["id"], "account_id": a })).await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("missing_variable")));
    // the field's own fallback fills it
    f.lok("PATCH", "/v1/leads/fields/pain_point", Some(json!({ "fallback": "growth" }))).await;
    let r = f.enroll_raw(seq, json!({ "lead_id": lead["id"], "account_id": a })).await;
    assert_eq!(r.body["enrolled"], 1, "{}", r.body);
    let e: Uuid = r.body["items"][0]["enrollment"]["id"].as_str().unwrap().parse().unwrap();
    let j = f.until_sent(e, 1).await;
    assert_eq!(f.h.job(j).await.body_text.as_deref(), Some("About growth"));
}

#[sqlx::test]
async fn a_suppressed_address_makes_no_lead_when_enroll_refuses_it(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    sqlx::query("INSERT INTO suppressions (value, kind, reason) VALUES ('no@x.com', 'email', 'manual')").execute(&f.h.state.pool).await.unwrap();
    let seq = f.sequence(&[0]).await;
    let r = f.enroll_raw(seq, json!({ "to_email": "no@x.com", "account_id": a })).await;
    assert_eq!(r.body["items"][0]["error"]["code"], "suppressed");
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM leads").fetch_one(&f.h.state.pool).await.unwrap();
    assert_eq!(n, 0);
}

// ---------- live variables (decision A) ----------

#[sqlx::test]
async fn a_typo_fixed_after_enroll_goes_out_right(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let lead = f.lok("POST", "/v1/leads", Some(json!({ "email": "ana@acme.com", "first_name": "Anna" }))).await;
    let seq = f.sequence(&[0, 1]).await;
    let r = f.enroll_raw(seq, json!({ "lead_id": lead["id"], "account_id": a })).await;
    let e: Uuid = r.body["items"][0]["enrollment"]["id"].as_str().unwrap().parse().unwrap();
    let j1 = f.until_sent(e, 1).await;
    assert_eq!(f.h.job(j1).await.subject, "Hello Anna");
    // the typo is fixed between the steps
    f.lok("PATCH", &format!("/v1/leads/{}", lead["id"].as_str().unwrap()), Some(json!({ "first_name": "Ana" }))).await;
    let j2 = f.until_sent(e, 2).await;
    assert_eq!(f.h.job(j2).await.body_text.as_deref(), Some("Step 2 for Ana"), "the current lead field wins over the snapshot");
}

#[sqlx::test]
async fn a_deleted_lead_keeps_its_snapshot_and_the_enrollment_runs_on(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let lead = f.lok("POST", "/v1/leads", Some(json!({ "email": "ana@acme.com", "first_name": "Ana" }))).await;
    let seq = f.sequence(&[0, 1]).await;
    let r = f.enroll_raw(seq, json!({ "lead_id": lead["id"], "account_id": a })).await;
    let e: Uuid = r.body["items"][0]["enrollment"]["id"].as_str().unwrap().parse().unwrap();
    // an FK with SET NULL: delete the row directly (the API would also stop the enrollment)
    sqlx::query("DELETE FROM leads WHERE id = $1").bind(lead["id"].as_str().unwrap().parse::<Uuid>().unwrap()).execute(&f.h.state.pool).await.unwrap();
    let (lead_id,): (Option<Uuid>,) = sqlx::query_as("SELECT lead_id FROM enrollments WHERE id = $1").bind(e).fetch_one(&f.h.state.pool).await.unwrap();
    assert_eq!(lead_id, None);
    let j = f.until_sent(e, 1).await;
    assert_eq!(f.h.job(j).await.subject, "Hello Ana");
}

// ---------- the hooks ----------

#[sqlx::test]
async fn a_sent_step_sets_contacted_and_logs_step_sent(pool: PgPool) {
    let f = F::new(pool).await;
    let (_, e) = f.after_step_one(&[0, 1, 1]).await;
    let lead = f.lead_by_email(JANE).await;
    assert_eq!(lead["contact_state"], "contacted");
    assert!(lead["last_contacted_at"].is_string());
    assert_eq!(f.events(&lead).await, ["step_sent", "enrolled", "created"]);
    let ev = f.lok("GET", &format!("/v1/leads/{}/events", lead["id"].as_str().unwrap()), None).await;
    assert_eq!(ev["events"][0]["detail"]["enrollment_id"], e.to_string());
    assert_eq!(ev["events"][0]["detail"]["step_number"], 1);
}

#[sqlx::test]
async fn a_manual_send_to_a_lead_counts_too(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    f.lok("POST", "/v1/leads", Some(json!({ "email": "bob@acme.com" }))).await;
    let job = f.h.queue(a, "bob@acme.com").await;
    f.h.tick().await;
    assert_eq!(f.h.job(job).await.status, JobStatus::Sent);
    let lead = f.lead_by_email("bob@acme.com").await;
    assert_eq!(lead["contact_state"], "contacted");
    assert!(lead["last_contacted_at"].is_string());
    assert_eq!(f.events(&lead).await, ["step_sent", "created"]);
    // an address that is not a lead does not make one
    f.h.advance(Duration::hours(1));
    let job = f.h.queue(a, "stranger@acme.com").await;
    f.h.tick().await;
    assert_eq!(f.h.job(job).await.status, JobStatus::Sent);
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM leads").fetch_one(&f.h.state.pool).await.unwrap();
    assert_eq!(n, 1);
}

#[sqlx::test]
async fn a_reply_sets_replied_logs_an_event_and_keeps_the_interest(pool: PgPool) {
    let f = F::new(pool).await;
    let (a, e) = f.after_step_one(&[0, 1, 1]).await;
    let lead = f.lead_by_email(JANE).await;
    let id = lead["id"].as_str().unwrap();
    f.lok("PATCH", &format!("/v1/leads/{id}"), Some(json!({ "interest": "meeting_booked" }))).await;
    f.ingest(a, &fixture("human_plain.eml")).await;
    assert_eq!(f.status(e).await, "stopped");
    let lead = f.lead_by_email(JANE).await;
    assert_eq!(lead["contact_state"], "replied");
    assert!(lead["last_replied_at"].is_string());
    assert_eq!(lead["interest"], "meeting_booked", "a reply never overwrites the interest");
    let kinds = f.events(&lead).await;
    assert_eq!(kinds[0], "replied", "{kinds:?}");
    assert!(kinds.contains(&"interest_changed".to_string()));
    // the same reply again changes nothing more
    f.ingest(a, &fixture("human_plain.eml")).await;
    assert_eq!(f.events(&lead).await.iter().filter(|k| *k == "replied").count(), 1);
}

#[sqlx::test]
async fn an_auto_reply_only_writes_an_event(pool: PgPool) {
    let f = F::new(pool).await;
    let (a, e) = f.after_step_one(&[0, 1, 1]).await;
    f.ingest(a, &fixture("gmail_vacation.eml")).await;
    assert_eq!(f.status(e).await, "paused");
    let lead = f.lead_by_email(JANE).await;
    assert_eq!(lead["contact_state"], "contacted", "an out-of-office does not change the state");
    assert_eq!(lead["last_replied_at"], Value::Null);
    assert_eq!(f.events(&lead).await[0], "auto_replied");
}

#[sqlx::test]
async fn a_hard_bounce_marks_the_lead_bounced_for_good(pool: PgPool) {
    let f = F::new(pool).await;
    let (a, _) = f.after_step_one(&[0, 1]).await;
    f.ingest(a, &fixture("gmail_bounce_user_unknown.eml")).await;
    let lead = f.lead_by_email(JANE).await;
    assert_eq!(lead["contact_state"], "bounced");
    assert_eq!(f.events(&lead).await[0], "bounced");
    // bounced never changes again: not even a later reply
    f.ingest(a, &fixture("human_plain.eml")).await;
    assert_eq!(f.state_of(JANE).await, "bounced");
}

#[sqlx::test]
async fn unsubscribe_and_complaint_mark_the_lead_unsubscribed(pool: PgPool) {
    let f = F::new(pool.clone()).await;
    let (a, _) = f.after_step_one(&[0, 1]).await;
    f.ingest(a, &fixture("unsubscribe_short.eml")).await;
    let lead = f.lead_by_email(JANE).await;
    assert_eq!(lead["contact_state"], "unsubscribed");
    assert_eq!(f.events(&lead).await[0], "unsubscribed");
}

#[sqlx::test]
async fn a_complaint_is_logged_and_the_lead_is_unsubscribed(pool: PgPool) {
    let f = F::new(pool).await;
    let (a, _) = f.after_step_one(&[0, 1]).await;
    f.ingest(a, &fixture("spam_complaint.eml")).await;
    let lead = f.lead_by_email(JANE).await;
    assert_eq!(lead["contact_state"], "unsubscribed");
    assert_eq!(f.events(&lead).await[0], "complained");
}

#[sqlx::test]
async fn a_finished_sequence_without_a_reply_is_completed(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1]).await;
    let e = f.enroll(seq, a, "end@acme.com").await;
    f.until_sent(e, 2).await;
    assert_eq!(f.status(e).await, "finished");
    let lead = f.lead_by_email("end@acme.com").await;
    assert_eq!(lead["contact_state"], "completed");
    assert_eq!(f.events(&lead).await[..2], ["stopped", "step_sent"], "step 2 went out, then the sequence finished");
    // enrolled again (a new sequence): in a sequence again
    let seq2 = f.sequence(&[0]).await;
    f.enroll(seq2, a, "end@acme.com").await;
    assert_eq!(f.state_of("end@acme.com").await, "in_sequence");
}

#[sqlx::test]
async fn a_manual_stop_does_not_leave_the_lead_in_a_sequence(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    let seq = f.sequence(&[0, 1]).await;
    let e = f.enroll(seq, a, "stop@acme.com").await;
    assert_eq!(f.state_of("stop@acme.com").await, "in_sequence");
    f.fapi("POST", &format!("/v1/followups/enrollments/{e}/stop"), None).await;
    let lead = f.lead_by_email("stop@acme.com").await;
    assert_eq!(lead["contact_state"], "new", "nothing was sent, so the lead is new again");
    assert_eq!(f.events(&lead).await[0], "stopped");
}

#[sqlx::test]
async fn a_lead_hook_that_fails_never_blocks_the_send(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    f.lok("POST", "/v1/leads", Some(json!({ "email": "bob@acme.com" }))).await;
    // make every write to the timeline fail
    sqlx::query("ALTER TABLE lead_events ADD CONSTRAINT never CHECK (false) NOT VALID").execute(&f.h.state.pool).await.unwrap();
    let job = f.h.queue(a, "bob@acme.com").await;
    f.h.tick().await;
    assert_eq!(f.h.job(job).await.status, JobStatus::Sent, "the email went out");
    let (state,): (String,) = sqlx::query_as("SELECT contact_state::text FROM leads WHERE email = 'bob@acme.com'").fetch_one(&f.h.state.pool).await.unwrap();
    assert_eq!(state, "new", "the failed hook was rolled back as a whole");
}

// ---------- delete a lead with an open enrollment (FK + erase) ----------

#[sqlx::test]
async fn deleting_a_lead_stops_its_enrollment_and_cancels_the_queued_job(pool: PgPool) {
    let f = F::new(pool).await;
    let (_, e) = f.after_step_one(&[0, 1]).await;
    f.h.advance(Duration::days(2));
    scheduler::tick(&f.fe).await.unwrap();
    let jobs = f.jobs(e).await;
    assert_eq!(jobs.len(), 2);
    assert_eq!(f.h.job(jobs[1]).await.status, JobStatus::Queued);
    let lead = f.lead_by_email(JANE).await;
    let r = f.lok("DELETE", &format!("/v1/leads/{}", lead["id"].as_str().unwrap()), None).await;
    assert_eq!(r["stopped_enrollments"], 1);
    assert_eq!(f.h.job(jobs[1]).await.status, JobStatus::Cancelled);
    assert_eq!(f.status(e).await, "stopped");
    let d = f.fapi("GET", &format!("/v1/followups/enrollments/{e}"), None).await;
    assert_eq!(d.body["enrollment"]["lead_id"], Value::Null);
    assert_eq!(d.body["enrollment"]["stop_reason"], "lead_deleted");
}

// ---------- the backfill ----------

#[sqlx::test]
async fn backfill_links_old_enrollments_and_runs_twice_without_duplicates(pool: PgPool) {
    let f = F::new(pool).await;
    let (a, _) = f.after_step_one(&[0, 1]).await;
    let seq = f.sequence(&[0]).await;
    let e2 = f.enroll(seq, a, "second@acme.com").await;
    let e3 = f.enroll(seq, a, "third@acme.com").await;
    f.fapi("POST", &format!("/v1/followups/enrollments/{e3}/stop"), None).await;
    // make it look like old data: no leads, no links
    sqlx::query("UPDATE enrollments SET lead_id = NULL").execute(&f.h.state.pool).await.unwrap();
    sqlx::query("DELETE FROM leads").execute(&f.h.state.pool).await.unwrap();
    // an old reply on the first address
    sqlx::query("UPDATE send_jobs SET replied_at = now() WHERE to_email = $1").bind(JANE).execute(&f.h.state.pool).await.unwrap();

    let now = f.h.now();
    let r = aikido_email::leads::backfill::run(&f.h.state.pool, now).await.unwrap();
    assert_eq!((r.linked, r.leads_created), (3, 3));
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM enrollments WHERE lead_id IS NULL").fetch_one(&f.h.state.pool).await.unwrap();
    assert_eq!(n, 0);
    assert_eq!(f.state_of(JANE).await, "replied");
    assert_eq!(f.state_of("second@acme.com").await, "in_sequence");
    assert_eq!(f.state_of("third@acme.com").await, "new");
    let lead = f.lead_by_email("second@acme.com").await;
    assert_eq!(lead["source"], "enrollment");
    let d = f.fapi("GET", &format!("/v1/followups/enrollments/{e2}"), None).await;
    assert_eq!(d.body["enrollment"]["lead_id"], lead["id"]);
    assert!(f.lead_by_email(JANE).await["last_replied_at"].is_string());

    // twice: nothing changes
    let r = aikido_email::leads::backfill::run(&f.h.state.pool, now).await.unwrap();
    assert_eq!((r.linked, r.leads_created), (0, 0));
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM leads").fetch_one(&f.h.state.pool).await.unwrap();
    assert_eq!(n, 3);
}

// ---------- the field delete guard ----------

#[sqlx::test]
async fn a_field_used_by_a_sequence_with_open_enrollments_can_not_be_deleted(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    f.lok("POST", "/v1/leads/fields", Some(json!({ "key": "pain_point", "fallback": "growth" }))).await;
    f.lok("POST", "/v1/leads/fields", Some(json!({ "key": "unused" }))).await;
    let seq = f.sequence_from(json!({ "name": "Pain sequence", "steps": [{ "subject": "Hi", "body_text": "About {{pain_point}}" }] })).await;
    // no enrollment yet: the field can go (nothing needs it)
    // (made again below)
    f.lok("DELETE", "/v1/leads/fields/unused", None).await;
    let e = f.enroll(seq, a, "ana@acme.com").await;
    let r = f.lapi("DELETE", "/v1/leads/fields/pain_point", None).await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::CONFLICT, Some("field_in_use")));
    assert!(r.body["error"]["message"].as_str().unwrap().contains("Pain sequence"));
    // archived: still counts, because it can be resumed
    f.fapi("PATCH", &format!("/v1/followups/sequences/{seq}"), Some(json!({ "action": "archive" }))).await;
    assert_eq!(f.lapi("DELETE", "/v1/leads/fields/pain_point", None).await.status, StatusCode::CONFLICT);
    // the enrollment ends: the field can go
    f.fapi("POST", &format!("/v1/followups/enrollments/{e}/stop"), None).await;
    assert_eq!(f.lapi("DELETE", "/v1/leads/fields/pain_point", None).await.status, StatusCode::OK);
}

#[sqlx::test]
async fn a_sequence_that_does_not_use_the_field_does_not_block_it(pool: PgPool) {
    let f = F::new(pool).await;
    let a = f.account("a@x.test").await;
    f.lok("POST", "/v1/leads/fields", Some(json!({ "key": "pain_point" }))).await;
    let seq = f.sequence(&[0]).await;
    f.enroll(seq, a, "ana@acme.com").await;
    assert_eq!(f.lapi("DELETE", "/v1/leads/fields/pain_point", None).await.status, StatusCode::OK);
}

#[sqlx::test]
async fn a_sequence_can_not_be_given_a_campaign_by_hand(pool: PgPool) {
    let f = F::new(pool).await;
    let r = f.fapi("POST", "/v1/followups/sequences", Some(json!({ "name": "S", "campaign_id": Uuid::now_v7(), "steps": [{ "subject": "Hi", "body_text": "x" }] }))).await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("campaign_managed")));
    let seq = f.sequence(&[0]).await;
    let r = f.fapi("PATCH", &format!("/v1/followups/sequences/{seq}"), Some(json!({ "campaign_id": Uuid::now_v7() }))).await;
    assert_eq!(r.body["error"]["code"], "campaign_managed");
}

#[sqlx::test]
async fn the_migration_links_enrollments_to_leads(pool: PgPool) {
    let f = F::new(pool).await;
    let (n,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM information_schema.table_constraints WHERE constraint_name IN ('enrollments_lead_fk', 'sequences_campaign_fk')",
    )
    .fetch_one(&f.h.state.pool)
    .await
    .unwrap();
    assert_eq!(n, 2);
}

/// An old database (made before this step, with an id in `lead_id` and `campaign_id` that pointed at nothing)
/// takes migration 0005 without an error. The dangling ids are cleared, the links are made.
#[sqlx::test(migrations = false)]
async fn an_old_database_takes_migration_0005_with_dangling_ids(pool: PgPool) {
    let all = &aikido_email::db::MIGRATOR;
    let upto_4: Vec<_> = all.iter().filter(|m| m.version <= 4).cloned().collect();
    let old = sqlx::migrate::Migrator { migrations: std::borrow::Cow::Owned(upto_4), ..sqlx::migrate::Migrator::DEFAULT };
    old.run(&pool).await.unwrap();

    let account = Uuid::now_v7();
    sqlx::query("INSERT INTO email_accounts (id, email, provider, display_name, status, secret_ciphertext, secret_nonce, key_version) VALUES ($1, 'a@x.test', 'smtp', 'A', 'active', decode('00', 'hex'), decode('00', 'hex'), 1)").bind(account).execute(&pool).await.unwrap();
    let seq = Uuid::now_v7();
    sqlx::query("INSERT INTO sequences (id, name, campaign_id) VALUES ($1, 'S', $2)").bind(seq).bind(Uuid::now_v7()).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO enrollments (id, sequence_id, account_id, to_email, lead_id) VALUES ($1, $2, $3, 'x@y.com', $4)").bind(Uuid::now_v7()).bind(seq).bind(account).bind(Uuid::now_v7()).execute(&pool).await.unwrap();

    aikido_email::db::MIGRATOR.run(&pool).await.unwrap();
    let (lead, camp): (Option<Uuid>, Option<Uuid>) = sqlx::query_as("SELECT (SELECT lead_id FROM enrollments), (SELECT campaign_id FROM sequences)").fetch_one(&pool).await.unwrap();
    assert_eq!((lead, camp), (None, None));
    // the old row is still there, and the backfill gives it a lead
    let r = aikido_email::leads::backfill::run(&pool, chrono::Utc::now()).await.unwrap();
    assert_eq!((r.linked, r.leads_created), (1, 1));
}
