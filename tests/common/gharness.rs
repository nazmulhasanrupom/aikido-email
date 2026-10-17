//! A harness for the Email Generator tests: the sending engine, the follow-up scheduler, the generator worker and a
//! `FakeLlm`, all on one fake clock.
use super::*;
use aikido_email::followups::scheduler::{self, FollowupEngine, TickReport as FuReport};
use aikido_email::generator::llm::fake::{FakeFactory, FakeLlm};
use aikido_email::generator::worker::{self, GeneratorEngine, TickReport as GenReport};
use axum::http::StatusCode;
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};

pub const SENDER: &str = "sender@acme-outreach.example";
/// The lead of the reply fixtures (`tests/fixtures/inbound`).
pub const JANE: &str = "jane@prospect.example";
pub const FIXTURE_ID: &str = "<sent-0001@acme-outreach.example>";

pub struct G {
    pub h: Harness,
    pub fe: Arc<FollowupEngine>,
    pub ge: Arc<GeneratorEngine>,
    pub fake: Arc<FakeLlm>,
    pub keys: HashMap<Scope, String>,
    pub account: Uuid,
    pub provider: Uuid,
}

pub fn opener() -> Value {
    json!([{ "key": "opener", "instruction": "Write one friendly line about their company.", "max_words": 30, "fallback": "I came across your company." }])
}

/// A step with an AI slot `opener`.
pub fn ai_step(delay: i32, subject: &str, body: &str, slots: Value) -> Value {
    json!({ "delay_amount": delay, "subject": subject, "body_text": body, "ai_slots": slots })
}

/// Step 1 of a normal AI sequence.
pub fn step1() -> Value {
    ai_step(0, "Hello {{first_name|there}}", "Hi {{first_name|there}},\n{{ai:opener}}\nBye", opener())
}

impl G {
    pub async fn new(pool: PgPool) -> Self {
        let mut h = Harness::new(pool);
        let fake = FakeLlm::new();
        h.state.llm = Arc::new(FakeFactory(fake.clone()));
        let keys = bootstrap_keys(&h.state).await;
        let fe = Arc::new(FollowupEngine::new(h.state.clone()));
        let ge = Arc::new(GeneratorEngine::new(h.state.clone()));
        let account = h.account(SENDER).await;
        h.settings(account, json!({ "reply_check_max_age_mins": 0 })).await;
        let mut g = Self { h, fe, ge, fake, keys, account, provider: Uuid::nil() };
        let p = g
            .ok(Scope::Generator, "POST", "/v1/generator/providers", Some(json!({ "kind": "openai_compatible", "name": "Fake", "base_url": "http://127.0.0.1:1/v1", "model": "fake-model" })))
            .await;
        g.provider = p["id"].as_str().unwrap().parse().unwrap();
        // grace 0: a due step with no AI text uses `on_failure` at once (the tests of the grace set it themselves)
        g.settings(json!({ "slot_provider_id": g.provider, "draft_provider_id": g.provider, "due_grace_mins": 0 })).await;
        g
    }

    pub async fn api(&self, scope: Scope, method: &str, uri: &str, body: Option<Value>) -> Response {
        call(&self.h.state, method, uri, Some(&self.keys[&scope]), body).await
    }

    pub async fn ok(&self, scope: Scope, method: &str, uri: &str, body: Option<Value>) -> Value {
        let r = self.api(scope, method, uri, body).await;
        assert!(r.status.is_success(), "{method} {uri} -> {} {}", r.status, r.body);
        r.body
    }

    pub async fn gapi(&self, method: &str, uri: &str, body: Option<Value>) -> Response {
        self.api(Scope::Generator, method, uri, body).await
    }

    pub async fn genok(&self, method: &str, uri: &str, body: Option<Value>) -> Value {
        self.ok(Scope::Generator, method, uri, body).await
    }

    pub async fn settings(&self, patch: Value) {
        let r = self.gapi("PATCH", "/v1/generator/settings", Some(patch)).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    }

    pub async fn lead(&self, email: &str, first_name: &str) -> Uuid {
        let r = self.ok(Scope::Leads, "POST", "/v1/leads", Some(json!({ "email": email, "first_name": first_name, "company_name": "Acme" }))).await;
        r["id"].as_str().unwrap().parse().unwrap()
    }

    pub async fn sequence(&self, steps: Value) -> Uuid {
        let r = self.api(Scope::Followups, "POST", "/v1/followups/sequences", Some(json!({ "name": "AI sequence", "steps": steps }))).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        r.body["sequence"]["id"].as_str().unwrap().parse().unwrap()
    }

    pub async fn put_steps(&self, seq: Uuid, steps: Value) -> Response {
        self.api(Scope::Followups, "PUT", &format!("/v1/followups/sequences/{seq}/steps"), Some(json!({ "steps": steps }))).await
    }

    pub async fn enroll(&self, seq: Uuid, lead: Uuid, start_at: Option<DateTime<Utc>>) -> Uuid {
        let mut item = json!({ "lead_id": lead, "account_id": self.account });
        if let Some(t) = start_at {
            item["start_at"] = json!(t);
        }
        let r = self.api(Scope::Followups, "POST", &format!("/v1/followups/sequences/{seq}/enrollments"), Some(json!({ "enrollments": [item] }))).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        assert_eq!(r.body["enrolled"], 1, "{}", r.body);
        r.body["items"][0]["enrollment"]["id"].as_str().unwrap().parse().unwrap()
    }

    /// A lead and an enrollment of it.
    pub async fn enroll_new(&self, seq: Uuid, email: &str) -> (Uuid, Uuid) {
        let lead = self.lead(email, "Ana").await;
        (lead, self.enroll(seq, lead, None).await)
    }

    pub async fn gen_tick(&self) -> GenReport {
        worker::tick(&self.ge).await.unwrap()
    }

    pub async fn fu_tick(&self) -> FuReport {
        scheduler::tick(&self.fe).await.unwrap()
    }

    /// One round: the generator, the follow-up scheduler, the sending engine, then the clock moves.
    pub async fn round(&self, step: Duration) {
        self.gen_tick().await;
        self.fu_tick().await;
        self.h.tick().await;
        self.h.advance(step);
    }

    pub async fn enrollment(&self, e: Uuid) -> Value {
        let r = self.api(Scope::Followups, "GET", &format!("/v1/followups/enrollments/{e}"), None).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        r.body
    }

    pub async fn status(&self, e: Uuid) -> String {
        self.enrollment(e).await["enrollment"]["status"].as_str().unwrap().to_string()
    }

    pub async fn reason(&self, e: Uuid) -> Option<String> {
        self.enrollment(e).await["enrollment"]["stop_reason"].as_str().map(String::from)
    }

    pub async fn wait_reason(&self, e: Uuid) -> Option<String> {
        self.enrollment(e).await["enrollment"]["wait_reason"].as_str().map(String::from)
    }

    pub async fn event_kinds(&self, e: Uuid) -> Vec<String> {
        self.enrollment(e).await["events"].as_array().unwrap().iter().map(|ev| ev["kind"].as_str().unwrap().to_string()).collect()
    }

    pub async fn jobs(&self, e: Uuid) -> Vec<Uuid> {
        let rows: Vec<(Uuid,)> = sqlx::query_as("SELECT id FROM send_jobs WHERE enrollment_id = $1 ORDER BY step_number, created_at").bind(e).fetch_all(&self.h.state.pool).await.unwrap();
        rows.into_iter().map(|r| r.0).collect()
    }

    /// (subject, body_text, body_html) of the job of a step.
    pub async fn job_text(&self, e: Uuid, step: i32) -> (String, Option<String>, Option<String>) {
        sqlx::query_as("SELECT subject, body_text, body_html FROM send_jobs WHERE enrollment_id = $1 AND step_number = $2 ORDER BY created_at DESC LIMIT 1")
            .bind(e)
            .bind(step)
            .fetch_one(&self.h.state.pool)
            .await
            .unwrap_or_else(|_| panic!("no job for step {step}"))
    }

    /// (step, status) of the generations of an enrollment.
    pub async fn gens(&self, e: Uuid) -> Vec<(i32, String)> {
        sqlx::query_as("SELECT step_number, status FROM generations WHERE enrollment_id = $1 ORDER BY step_number, seq").bind(e).fetch_all(&self.h.state.pool).await.unwrap()
    }

    pub async fn gen_id(&self, e: Uuid, step: i32) -> Uuid {
        let r: (Uuid,) = sqlx::query_as("SELECT id FROM generations WHERE enrollment_id = $1 AND step_number = $2 ORDER BY seq DESC LIMIT 1").bind(e).bind(step).fetch_one(&self.h.state.pool).await.unwrap();
        r.0
    }

    pub async fn calls_logged(&self) -> i64 {
        count(&self.h.state, "SELECT count(*) FROM llm_calls").await
    }

    pub async fn set_message_id(&self, job: Uuid, id: &str) {
        sqlx::query("UPDATE send_jobs SET message_id_header = $2 WHERE id = $1").bind(job).bind(id).execute(&self.h.state.pool).await.unwrap();
    }

    /// Rounds until step `n` of the enrollment is sent (at most 600 rounds of 10 minutes).
    pub async fn until_sent(&self, e: Uuid, n: i32) -> Uuid {
        for _ in 0..600 {
            let sent: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM send_jobs WHERE enrollment_id = $1 AND step_number = $2 AND status = 'sent'").bind(e).bind(n).fetch_optional(&self.h.state.pool).await.unwrap();
            if let Some((id,)) = sent {
                return id;
            }
            self.round(Duration::minutes(10)).await;
        }
        panic!("step {n} was never sent");
    }
}
