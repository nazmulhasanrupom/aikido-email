//! UI update map v2, U3: AI mode. Prompts, runs, the worker, review, auto-write and the feeder gate.
//! `FakeLlm` answers every call. No real provider.
mod common;
use aikido_email::auth::api_keys::Scope;
use aikido_email::campaigns::feeder;
use aikido_email::generator::llm::LlmError;
use axum::http::StatusCode;
use chrono::Duration;
use common::gharness::*;
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

struct A {
    g: G,
    camp: String,
    seq: String,
}

impl A {
    /// A campaign in AI mode with two steps (the primary email and one follow up), each with a user prompt.
    async fn new(pool: PgPool) -> Self {
        let g = G::new(pool).await;
        let steps = json!([
            { "delay_amount": 0, "subject": "Hi", "body_text": "x", "ai_user_prompt": "Write to {{first_name}} at {{company_name}}." },
            { "delay_amount": 1, "subject": "", "body_text": "y", "ai_user_prompt": "Follow up for {{first_name}}. Earlier: {{previous_emails}}" }
        ]);
        let r = g.ok(Scope::Campaigns, "POST", "/v1/campaigns", Some(json!({ "name": "AI", "account_ids": [g.account], "sequence": { "name": "S", "steps": steps } }))).await;
        let camp = r["id"].as_str().unwrap().to_string();
        let seq = r["sequence_id"].as_str().unwrap().to_string();
        let a = Self { g, camp, seq };
        a.cok("PATCH", &format!("/v1/campaigns/{}", a.camp), Some(json!({ "content_mode": "ai" }))).await;
        a
    }

    async fn capi(&self, method: &str, uri: &str, body: Option<Value>) -> Response {
        self.g.api(Scope::Campaigns, method, uri, body).await
    }

    async fn cok(&self, method: &str, uri: &str, body: Option<Value>) -> Value {
        self.g.ok(Scope::Campaigns, method, uri, body).await
    }

    async fn leads(&self, n: usize) -> Vec<Uuid> {
        let mut ids = Vec::new();
        for i in 0..n {
            ids.push(self.g.lead(&format!("u{i}@acme.example"), &format!("N{i}")).await);
        }
        self.cok("POST", &format!("/v1/campaigns/{}/leads", self.camp), Some(json!({ "lead_ids": ids }))).await;
        ids
    }

    async fn generate(&self, body: Value) -> Value {
        let r = self.capi("POST", &format!("/v1/campaigns/{}/emails/generate", self.camp), Some(body)).await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.body);
        r.body
    }

    async fn email(&self, lead: Uuid, step: i32) -> Value {
        self.cok("GET", &format!("/v1/campaigns/{}/leads/{lead}/emails/{step}", self.camp), None).await
    }

    async fn count(&self, status: &str) -> i64 {
        let c: Uuid = self.camp.parse().unwrap();
        sqlx::query_as::<_, (i64,)>("SELECT count(*) FROM lead_emails WHERE campaign_id = $1 AND status = $2").bind(c).bind(status).fetch_one(&self.g.h.state.pool).await.unwrap().0
    }

    async fn run(&self, id: &str) -> Value {
        self.cok("GET", &format!("/v1/campaigns/{}/email-runs/{id}", self.camp), None).await
    }
}

#[sqlx::test]
async fn generate_the_primary_email_for_20_leads_then_the_follow_ups_see_the_primary_text(pool: PgPool) {
    let a = A::new(pool).await;
    let leads = a.leads(20).await;
    let est = a.cok("POST", &format!("/v1/campaigns/{}/emails/estimate", a.camp), Some(json!({ "steps": [1] }))).await;
    assert_eq!(est["emails"], 20);
    assert!(est["tokens"].as_i64().unwrap() > 0);

    let run = a.generate(json!({ "steps": [1], "leads": { "count": 20 } })).await;
    assert_eq!(run["total"], 20);
    let t = a.g.gen_tick().await;
    assert_eq!((t.emails_written, t.emails_failed), (20, 0));
    assert_eq!(a.count("ready").await, 20);
    let done = a.run(run["id"].as_str().unwrap()).await;
    assert_eq!((done["status"].as_str(), done["done"].as_i64(), done["failed"].as_i64()), (Some("done"), Some(20), Some(0)));
    assert!(done["tokens"].as_i64().unwrap() > 0);
    let e = a.email(leads[0], 1).await;
    assert_eq!(e["source"], "ai");
    assert_eq!(e["subject"], "Fake subject");
    assert_eq!(e["body_text"], "Fake email body for step 1.");
    // the user prompt was filled with the lead values
    assert!(a.g.fake.calls().iter().any(|c| c.user == "Write to N0 at Acme."));

    // the follow ups, apart: {{previous_emails}} holds the primary text
    a.generate(json!({ "steps": [2] })).await;
    a.g.gen_tick().await;
    let followup = a.g.fake.calls().into_iter().rev().find(|c| c.user.starts_with("Follow up for")).unwrap();
    assert!(followup.user.contains("Primary email:") && followup.user.contains("Fake email body for step 1."), "{}", followup.user);
    // a threaded follow up has no subject
    assert_eq!(a.email(leads[0], 2).await["subject"], "");
    assert!(followup.system.contains("set \"subject\" to an empty string"));
    // calls are logged with the new purpose
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM llm_calls WHERE purpose = 'lead_email'").fetch_one(&a.g.h.state.pool).await.unwrap();
    assert_eq!(n, 40);
}

#[sqlx::test]
async fn two_workers_make_one_email_per_lead_and_step(pool: PgPool) {
    let a = A::new(pool).await;
    a.leads(12).await;
    a.generate(json!({})).await;
    let (x, y) = tokio::join!(aikido_email::generator::worker::tick(&a.g.ge), aikido_email::generator::worker::tick(&a.g.ge));
    let (x, y) = (x.unwrap(), y.unwrap());
    assert_eq!(x.emails_written + y.emails_written, 24);
    assert_eq!(a.g.fake.call_count(), 24, "no email was written twice");
    assert_eq!(a.count("ready").await, 24);
}

#[sqlx::test]
async fn a_cancelled_run_stops_at_once_and_puts_the_rows_back(pool: PgPool) {
    let a = A::new(pool).await;
    a.leads(5).await;
    let run = a.generate(json!({ "steps": [1] })).await;
    a.cok("POST", &format!("/v1/campaigns/{}/email-runs/{}/cancel", a.camp, run["id"].as_str().unwrap()), None).await;
    let t = a.g.gen_tick().await;
    assert_eq!((t.claimed, t.emails_written), (0, 0));
    assert_eq!(a.g.fake.call_count(), 0);
    assert_eq!(a.count("none").await, 5);
    assert_eq!(a.run(run["id"].as_str().unwrap()).await["status"], "cancelled");
}

#[sqlx::test]
async fn generate_checks_the_mode_the_model_and_the_prompts(pool: PgPool) {
    let a = A::new(pool).await;
    a.leads(1).await;
    let uri = format!("/v1/campaigns/{}/emails/generate", a.camp);
    // no working model
    sqlx::query("UPDATE llm_providers SET status = 'auth_failed'").execute(&a.g.h.state.pool).await.unwrap();
    let r = a.capi("POST", &uri, Some(json!({}))).await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("generator_not_ready")));
    sqlx::query("UPDATE llm_providers SET status = 'ok'").execute(&a.g.h.state.pool).await.unwrap();
    // a step with no user prompt
    sqlx::query("UPDATE sequence_steps SET ai_user_prompt = NULL WHERE step_number = 2").execute(&a.g.h.state.pool).await.unwrap();
    let r = a.capi("POST", &uri, Some(json!({}))).await;
    assert_eq!(r.body["error"]["code"], "missing_prompt");
    // template mode
    a.cok("PATCH", &format!("/v1/campaigns/{}", a.camp), Some(json!({ "content_mode": "template" }))).await;
    let r = a.capi("POST", &uri, Some(json!({ "steps": [1] }))).await;
    assert_eq!(r.body["error"]["code"], "wrong_mode");
}

#[sqlx::test]
async fn a_prompt_with_an_unknown_variable_is_refused(pool: PgPool) {
    let a = A::new(pool).await;
    let steps = json!([{ "delay_amount": 0, "subject": "Hi", "body_text": "x", "ai_user_prompt": "Hello {{nope}}" }]);
    let r = a.g.api(Scope::Followups, "PUT", &format!("/v1/followups/sequences/{}/steps", a.seq), Some(json!({ "steps": steps }))).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.body["error"]["code"], "missing_variable");
    // a fallback covers it, and so do the prompt-only names
    let steps = json!([{ "delay_amount": 0, "subject": "Hi", "body_text": "x", "ai_user_prompt": "Hello {{nope|there}} {{brief}} {{lead_notes}} {{previous_emails}}" }]);
    let r = a.g.api(Scope::Followups, "PUT", &format!("/v1/followups/sequences/{}/steps", a.seq), Some(json!({ "steps": steps }))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let r = a.g.api(Scope::Followups, "PATCH", &format!("/v1/followups/sequences/{}", a.seq), Some(json!({ "ai_system_prompt": "You write for {{sender_name}} about {{zzz}}" }))).await;
    assert_eq!(r.body["error"]["code"], "missing_variable");
}

#[sqlx::test]
async fn test_on_one_lead_saves_nothing_but_the_call(pool: PgPool) {
    let a = A::new(pool).await;
    let leads = a.leads(1).await;
    let r = a.cok("POST", &format!("/v1/campaigns/{}/emails/test", a.camp), Some(json!({ "lead_id": leads[0], "step": 1 }))).await;
    assert_eq!(r["mode"], "ai");
    assert_eq!(r["user"], "Write to N0 at Acme.");
    assert!(r["system"].as_str().unwrap().contains("Rules for your answer"));
    assert_eq!(r["body"], "Fake email body for step 1.");
    assert_eq!(r["tokens_in"], 100);
    assert_eq!(a.count("ready").await + a.count("writing").await, 0);
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM llm_calls WHERE purpose = 'lead_email_test'").fetch_one(&a.g.h.state.pool).await.unwrap();
    assert_eq!(n, 1);
}

#[sqlx::test]
async fn review_holds_the_step_until_the_email_is_approved(pool: PgPool) {
    let a = A::new(pool).await;
    let leads = a.leads(2).await;
    a.g.genok("PATCH", &format!("/v1/generator/campaigns/{}", a.camp), Some(json!({ "review_required": true }))).await;
    a.generate(json!({ "steps": [1] })).await;
    a.g.gen_tick().await;
    assert_eq!(a.count("review").await, 2);
    // the feeder does not enroll a lead whose primary email waits for review
    a.cok("POST", &format!("/v1/campaigns/{}/launch", a.camp), None).await;
    assert_eq!(feeder::tick(&a.g.h.state).await.unwrap().enrolled, 0);
    let n = a.cok("POST", &format!("/v1/campaigns/{}/emails/actions", a.camp), Some(json!({ "action": "approve", "ids": [a.email(leads[0], 1).await["id"]] }))).await;
    assert_eq!(n["changed"], 1);
    assert_eq!(a.email(leads[0], 1).await["status"], "ready");
    assert_eq!(feeder::tick(&a.g.h.state).await.unwrap().enrolled, 1, "only the approved lead is enrolled");
    // approve all by filter
    let n = a.cok("POST", &format!("/v1/campaigns/{}/emails/actions", a.camp), Some(json!({ "action": "approve", "filter": { "step": 1, "status": "review" } }))).await;
    assert_eq!(n["changed"], 1);
    // reject writes it again
    let id = a.email(leads[1], 1).await["id"].clone();
    a.cok("POST", &format!("/v1/campaigns/{}/emails/actions", a.camp), Some(json!({ "action": "reject", "ids": [id] }))).await;
    assert_eq!(a.email(leads[1], 1).await["status"], "writing");
}

#[sqlx::test]
async fn a_due_step_without_an_email_waits_then_auto_write_fills_it(pool: PgPool) {
    let a = A::new(pool).await;
    let leads = a.leads(1).await;
    a.cok("PATCH", &format!("/v1/campaigns/{}", a.camp), Some(json!({ "auto_write": false }))).await;
    // the feeder waits for the primary email, and the daily room is not used
    a.cok("POST", &format!("/v1/campaigns/{}/launch", a.camp), None).await;
    assert_eq!(feeder::tick(&a.g.h.state).await.unwrap().enrolled, 0);
    a.generate(json!({ "steps": [1] })).await;
    a.g.gen_tick().await;
    assert_eq!(feeder::tick(&a.g.h.state).await.unwrap().enrolled, 1);
    let (e,): (Uuid,) = sqlx::query_as("SELECT id FROM enrollments LIMIT 1").fetch_one(&a.g.h.state.pool).await.unwrap();
    a.g.until_sent(e, 1).await;
    assert_eq!(a.email(leads[0], 1).await["status"], "sent");
    // step 2 is due, but has no email and auto-write is off: it waits
    a.g.h.advance(Duration::days(4));
    a.g.fu_tick().await;
    assert_eq!(a.g.wait_reason(e).await.as_deref(), Some("email_not_ready"));
    assert_eq!(a.g.jobs(e).await.len(), 1);
    // after an hour the bell shows it
    let none = aikido_email::ui_api::alerts::collect(&a.g.h.state).await.unwrap();
    assert!(!none.iter().any(|x| x.kind == "emails_not_ready"));
    a.g.h.advance(Duration::minutes(61));
    let alerts = aikido_email::ui_api::alerts::collect(&a.g.h.state).await.unwrap();
    let al = alerts.iter().find(|x| x.kind == "emails_not_ready").expect("an alert after an hour");
    assert!(al.link.ends_with("/leads") && al.title.contains("not ready"));
    // auto-write on: the worker writes it and the step goes out
    a.cok("PATCH", &format!("/v1/campaigns/{}", a.camp), Some(json!({ "auto_write": true }))).await;
    let t = a.g.gen_tick().await;
    assert_eq!(t.emails_written, 1);
    a.g.until_sent(e, 2).await;
    assert_eq!(a.email(leads[0], 2).await["status"], "sent");
}

#[sqlx::test]
async fn a_lead_edit_makes_an_ai_email_out_of_date_and_the_text_stays(pool: PgPool) {
    let a = A::new(pool).await;
    let leads = a.leads(1).await;
    a.generate(json!({ "steps": [1] })).await;
    a.g.gen_tick().await;
    a.g.ok(Scope::Leads, "PATCH", &format!("/v1/leads/{}", leads[0]), Some(json!({ "company_name": "Zed Corp" }))).await;
    let e = a.email(leads[0], 1).await;
    assert_eq!(e["status"], "stale");
    assert_eq!(e["body_text"], "Fake email body for step 1.");
    // regenerate writes it again
    a.cok("POST", &format!("/v1/campaigns/{}/emails/actions", a.camp), Some(json!({ "action": "regenerate", "ids": [e["id"]] }))).await;
    a.g.gen_tick().await;
    assert_eq!(a.email(leads[0], 1).await["status"], "ready");
    assert!(a.g.fake.calls().iter().any(|c| c.user == "Write to N0 at Zed Corp."));
}

#[sqlx::test]
async fn a_bad_answer_is_repaired_once_and_a_failure_keeps_the_old_text(pool: PgPool) {
    let a = A::new(pool).await;
    let leads = a.leads(1).await;
    a.generate(json!({ "steps": [1] })).await;
    a.g.fake.push(Ok(aikido_email::generator::llm::fake::response("no json")));
    a.g.gen_tick().await;
    assert_eq!(a.g.fake.call_count(), 2, "one repair try");
    assert_eq!(a.email(leads[0], 1).await["status"], "ready");
    // now a failing regenerate: both answers have a placeholder
    let bad = aikido_email::generator::llm::fake::response(r#"{"subject":"Hi","body":"Hello [Name]"}"#);
    a.g.fake.push(Ok(bad.clone()));
    a.g.fake.push(Ok(bad));
    let id = a.email(leads[0], 1).await["id"].clone();
    a.cok("POST", &format!("/v1/campaigns/{}/emails/actions", a.camp), Some(json!({ "action": "regenerate", "ids": [id] }))).await;
    a.g.gen_tick().await;
    let e = a.email(leads[0], 1).await;
    assert_eq!(e["status"], "failed");
    assert_eq!(e["error_code"], "guard_failed");
    assert_eq!(e["body_text"], "Fake email body for step 1.", "the old text is kept");
    // a provider that is down is retried later, not failed
    a.g.fake.push(Err(LlmError::Transient { code: "rate_limited", retry_after: None }));
    let id = e["id"].clone();
    a.cok("POST", &format!("/v1/campaigns/{}/emails/actions", a.camp), Some(json!({ "action": "regenerate", "ids": [id] }))).await;
    let t = a.g.gen_tick().await;
    assert_eq!(t.emails_failed, 0);
    assert_eq!(a.email(leads[0], 1).await["status"], "writing");
}
