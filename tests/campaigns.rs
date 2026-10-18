//! Campaigns: create, add leads, launch checks, pause, resume, archive, and the feeder.
mod common;
use aikido_email::auth::api_keys::Scope;
use aikido_email::campaigns::feeder;
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

struct C {
    h: Harness,
    fe: Arc<FollowupEngine>,
    keys: HashMap<Scope, String>,
}

impl C {
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

    async fn api(&self, method: &str, uri: &str, body: Option<Value>) -> Response {
        call(&self.h.state, method, uri, Some(&self.keys[&Scope::Campaigns]), body).await
    }

    async fn ok(&self, method: &str, uri: &str, body: Option<Value>) -> Value {
        let r = self.api(method, uri, body).await;
        assert!(r.status.is_success(), "{method} {uri} -> {} {}", r.status, r.body);
        r.body
    }

    async fn leads(&self, method: &str, uri: &str, body: Option<Value>) -> Value {
        let r = call(&self.h.state, method, uri, Some(&self.keys[&Scope::Leads]), body).await;
        assert!(r.status.is_success(), "{method} {uri} -> {} {}", r.status, r.body);
        r.body
    }

    /// Pause an account with the sending key.
    async fn pause_account(&self, account: Uuid) {
        let r = call(&self.h.state, "POST", &format!("/v1/sending/accounts/{account}/pause"), Some(&self.keys[&Scope::Sending]), None).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    }

    async fn fapi(&self, method: &str, uri: &str, body: Option<Value>) -> Response {
        call(&self.h.state, method, uri, Some(&self.keys[&Scope::Followups]), body).await
    }

    fn steps(n: usize) -> Value {
        Value::Array(
            (0..n)
                .map(|i| json!({ "delay_amount": if i == 0 { 0 } else { 1 }, "subject": if i == 0 { "Hello {{first_name|there}}" } else { "" }, "body_text": format!("Step {} for {{{{first_name|friend}}}}", i + 1) }))
                .collect(),
        )
    }

    /// A draft campaign with a new sequence and the given accounts.
    async fn campaign(&self, accounts: &[Uuid], extra: Value) -> Value {
        let mut body = json!({ "name": "Q4 outreach", "sequence": { "name": "Seq", "steps": Self::steps(2) }, "account_ids": accounts });
        for (k, v) in extra.as_object().cloned().unwrap_or_default() {
            body[k] = v;
        }
        let r = self.api("POST", "/v1/campaigns", Some(body)).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
        r.body
    }

    async fn lead(&self, email: &str) -> Uuid {
        let r = self.leads("POST", "/v1/leads", Some(json!({ "email": email, "first_name": "Ana" }))).await;
        r["id"].as_str().unwrap().parse().unwrap()
    }

    async fn many_leads(&self, n: usize, domain: &str) -> Vec<Uuid> {
        let leads: Vec<Value> = (0..n).map(|i| json!({ "email": format!("u{i}@{domain}"), "first_name": format!("N{i}") })).collect();
        let r = self.leads("POST", "/v1/leads/bulk", Some(json!({ "leads": leads }))).await;
        r["items"].as_array().unwrap().iter().map(|i| i["lead_id"].as_str().unwrap().parse().unwrap()).collect()
    }

    async fn add(&self, campaign: &str, ids: &[Uuid]) -> Value {
        self.ok("POST", &format!("/v1/campaigns/{campaign}/leads"), Some(json!({ "lead_ids": ids }))).await
    }

    async fn launch(&self, campaign: &str) -> Value {
        self.ok("POST", &format!("/v1/campaigns/{campaign}/launch"), None).await
    }

    async fn get(&self, campaign: &str) -> Value {
        self.ok("GET", &format!("/v1/campaigns/{campaign}"), None).await
    }

    async fn feed(&self) -> feeder::TickReport {
        feeder::tick(&self.h.state).await.unwrap()
    }

    async fn fu_tick(&self) {
        scheduler::tick(&self.fe).await.unwrap();
    }

    /// One round: the feeder, the follow-up scheduler, the sending engine, then the clock moves.
    async fn round(&self, step: Duration) {
        self.feed().await;
        self.fu_tick().await;
        self.h.tick().await;
        self.h.advance(step);
    }

    async fn enrollments_of(&self, campaign: &str) -> Vec<(Uuid, Uuid, String)> {
        sqlx::query_as("SELECT e.id, e.account_id, e.status::text FROM campaign_leads cl JOIN enrollments e ON e.id = cl.enrollment_id WHERE cl.campaign_id = $1 ORDER BY cl.added_at, cl.lead_id")
            .bind(campaign.parse::<Uuid>().unwrap())
            .fetch_all(&self.h.state.pool)
            .await
            .unwrap()
    }

    async fn skip_reasons(&self, campaign: &str) -> HashMap<String, String> {
        let rows: Vec<(String, String)> = sqlx::query_as("SELECT l.email, cl.skip_reason::text FROM campaign_leads cl JOIN leads l ON l.id = cl.lead_id WHERE cl.campaign_id = $1 AND cl.state = 'skipped'")
            .bind(campaign.parse::<Uuid>().unwrap())
            .fetch_all(&self.h.state.pool)
            .await
            .unwrap();
        rows.into_iter().collect()
    }

    async fn sequence_status(&self, campaign: &str) -> String {
        let (s,): (String,) = sqlx::query_as("SELECT s.status::text FROM sequences s JOIN campaigns c ON c.sequence_id = s.id WHERE c.id = $1")
            .bind(campaign.parse::<Uuid>().unwrap())
            .fetch_one(&self.h.state.pool)
            .await
            .unwrap();
        s
    }

    async fn count_leads(&self, campaign: &str, state: &str) -> i64 {
        sqlx::query_as::<_, (i64,)>("SELECT count(*) FROM campaign_leads WHERE campaign_id = $1 AND state::text = $2")
            .bind(campaign.parse::<Uuid>().unwrap())
            .bind(state)
            .fetch_one(&self.h.state.pool)
            .await
            .unwrap()
            .0
    }
}

fn id(v: &Value) -> String {
    v["id"].as_str().unwrap().to_string()
}

fn code(r: &Response) -> String {
    r.body["error"]["code"].as_str().unwrap_or("").to_string()
}

// ---------- phase 7: create ----------

#[sqlx::test]
async fn the_campaigns_scope_guards_the_routes(pool: PgPool) {
    let c = C::new(pool).await;
    assert_eq!(call(&c.h.state, "GET", "/v1/campaigns", None, None).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(call(&c.h.state, "GET", "/v1/campaigns", Some(&c.keys[&Scope::Leads]), None).await.status, StatusCode::FORBIDDEN);
    assert_eq!(call(&c.h.state, "GET", "/v1/campaigns", Some(&c.keys[&Scope::Followups]), None).await.status, StatusCode::FORBIDDEN);
    assert_eq!(call(&c.h.state, "GET", "/v1/campaigns", Some(&c.keys[&Scope::Admin]), None).await.status, StatusCode::OK);
    assert_eq!(c.api("GET", "/v1/campaigns", None).await.status, StatusCode::OK);
    // the campaigns key reads leads and sequences inside its own routes, but does not open the other scopes
    assert_eq!(call(&c.h.state, "GET", "/v1/leads", Some(&c.keys[&Scope::Campaigns]), None).await.status, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn create_with_a_new_sequence(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let b = c.account("b@x.test").await;
    let r = c.campaign(&[a, b, a], json!({ "daily_new_leads": 20, "stop_company_on_reply": true, "account_pick": "round_robin", "timezone": "Asia/Dhaka" })).await;
    assert_eq!(r["status"], "draft");
    assert_eq!(r["daily_new_leads"], 20);
    assert_eq!(r["account_pick"], "round_robin");
    assert_eq!(r["stop_company_on_reply"], true);
    assert_eq!(r["allow_unverified"], false);
    assert_eq!(r["timezone"], "Asia/Dhaka");
    assert_eq!(r["accounts"].as_array().unwrap().len(), 2, "a repeated account counts once");
    assert_eq!(r["steps"].as_array().unwrap().len(), 2);
    assert_eq!(r["counts"]["pending"], 0);
    // both sides of the link
    assert_eq!(r["sequence"]["campaign_id"], r["id"]);
    assert_eq!(r["sequence_id"], r["sequence"]["id"]);
    assert_eq!(c.sequence_status(r["id"].as_str().unwrap()).await, "paused", "a draft campaign does not run its sequence");
    // the defaults
    let d = c.campaign(&[a], json!({})).await;
    assert_eq!(d["daily_new_leads"], 50);
    assert_eq!(d["account_pick"], "least_loaded");
    assert_eq!(d["timezone"], "UTC");
}

#[sqlx::test]
async fn create_with_an_existing_sequence(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let seq = c.fapi("POST", "/v1/followups/sequences", Some(json!({ "name": "Mine", "steps": C::steps(1) }))).await.body["sequence"]["id"].as_str().unwrap().to_string();
    let r = c.ok("POST", "/v1/campaigns", Some(json!({ "name": "C", "sequence_id": seq, "account_ids": [a] }))).await;
    assert_eq!(r["sequence_id"], seq);
    assert_eq!(c.sequence_status(r["id"].as_str().unwrap()).await, "active", "an existing sequence keeps its status until launch");
    // one campaign per sequence
    let again = c.api("POST", "/v1/campaigns", Some(json!({ "name": "C2", "sequence_id": seq }))).await;
    assert_eq!((again.status, code(&again).as_str()), (StatusCode::CONFLICT, "sequence_in_campaign"));
    // archived sequences are refused
    let seq2 = c.fapi("POST", "/v1/followups/sequences", Some(json!({ "name": "Old", "steps": C::steps(1) }))).await.body["sequence"]["id"].as_str().unwrap().to_string();
    c.fapi("PATCH", &format!("/v1/followups/sequences/{seq2}"), Some(json!({ "action": "archive" }))).await;
    assert_eq!(code(&c.api("POST", "/v1/campaigns", Some(json!({ "name": "C3", "sequence_id": seq2 }))).await), "sequence_archived");
    let nf = c.api("POST", "/v1/campaigns", Some(json!({ "name": "C4", "sequence_id": Uuid::now_v7() }))).await;
    assert_eq!(nf.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn create_checks_its_input_and_leaves_nothing_behind(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let seq = json!({ "name": "S", "steps": C::steps(1) });
    for (body, expected) in [
        (json!({ "name": "C" }), "sequence_required"),
        (json!({ "name": "C", "sequence": seq, "sequence_id": Uuid::now_v7() }), "sequence_required"),
        (json!({ "name": " ", "sequence": seq }), "invalid_name"),
        (json!({ "name": "C", "sequence": seq, "daily_new_leads": 0 }), "invalid_setting"),
        (json!({ "name": "C", "sequence": seq, "daily_new_leads": 5001 }), "invalid_setting"),
        (json!({ "name": "C", "sequence": seq, "timezone": "Mars/Base" }), "invalid_timezone"),
        (json!({ "name": "C", "sequence": seq, "account_ids": [Uuid::now_v7()] }), "account_not_found"),
        (json!({ "name": "C", "sequence": { "name": "S", "steps": [] } }), "invalid_steps"),
        (json!({ "name": "C", "sequence": { "name": "S", "campaign_id": Uuid::now_v7(), "steps": C::steps(1) } }), "campaign_managed"),
    ] {
        let r = c.api("POST", "/v1/campaigns", Some(body.clone())).await;
        assert_eq!((r.status, code(&r).as_str()), (StatusCode::UNPROCESSABLE_ENTITY, expected), "{body}");
    }
    let (n,): (i64,) = sqlx::query_as("SELECT (SELECT count(*) FROM campaigns) + (SELECT count(*) FROM sequences)").fetch_one(&c.h.state.pool).await.unwrap();
    assert_eq!(n, 0, "a refused campaign made no sequence either");
    let _ = a;
}

#[sqlx::test]
async fn patch_changes_settings_and_the_pool(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let b = c.account("b@x.test").await;
    let camp = c.campaign(&[a], json!({})).await;
    let cid = id(&camp);
    let r = c.ok("PATCH", &format!("/v1/campaigns/{cid}"), Some(json!({ "name": "Renamed", "daily_new_leads": 5, "account_pick": "round_robin", "allow_unverified": true, "timezone": "Europe/Berlin", "account_ids": [b] }))).await;
    assert_eq!((r["name"].as_str(), r["daily_new_leads"].as_i64(), r["allow_unverified"].as_bool()), (Some("Renamed"), Some(5), Some(true)));
    assert_eq!(r["accounts"][0]["email"], "b@x.test");
    assert_eq!(r["accounts"].as_array().unwrap().len(), 1);
    for (bad, expected) in [
        (json!({ "daily_new_leads": 0 }), "invalid_setting"),
        (json!({ "daily_new_leads": "many" }), "invalid_setting"),
        (json!({ "timezone": "Nope" }), "invalid_timezone"),
        (json!({ "account_ids": [Uuid::now_v7()] }), "account_not_found"),
        (json!({ "status": "active" }), "unknown_field"),
        (json!({ "sequence_id": Uuid::now_v7() }), "unknown_field"),
    ] {
        let r = c.api("PATCH", &format!("/v1/campaigns/{cid}"), Some(bad.clone())).await;
        assert_eq!((r.status, code(&r).as_str()), (StatusCode::UNPROCESSABLE_ENTITY, expected), "{bad}");
    }
    assert_eq!(c.get(&cid).await["name"], "Renamed", "a bad patch changed nothing");
    assert_eq!(c.api("PATCH", &format!("/v1/campaigns/{}", Uuid::now_v7()), Some(json!({ "name": "x" }))).await.status, StatusCode::NOT_FOUND);
}

// ---------- phase 7: leads of a campaign ----------

#[sqlx::test]
async fn add_leads_by_ids_list_and_filter(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp = id(&c.campaign(&[a], json!({})).await);
    let ids = c.many_leads(6, "acme.com").await;
    let r = c.add(&camp, &ids[..3]).await;
    assert_eq!((r["matched"].as_i64(), r["added"].as_i64(), r["already_in_campaign"].as_i64()), (Some(3), Some(3), Some(0)));
    // again, with overlap
    let r = c.add(&camp, &ids[1..5]).await;
    assert_eq!((r["matched"].as_i64(), r["added"].as_i64(), r["already_in_campaign"].as_i64()), (Some(4), Some(2), Some(2)));
    // by list
    let list = c.leads("POST", "/v1/leads/lists", Some(json!({ "name": "L" }))).await["id"].as_str().unwrap().to_string();
    c.leads("POST", "/v1/leads/actions", Some(json!({ "action": "add_to_list", "lead_ids": [ids[5]], "list_id": list }))).await;
    let r = c.ok("POST", &format!("/v1/campaigns/{camp}/leads"), Some(json!({ "list_id": list }))).await;
    assert_eq!(r["added"], 1);
    assert_eq!(c.get(&camp).await["counts"]["pending"], 6);
    // by filter
    let other = c.many_leads(2, "other.io").await;
    let r = c.ok("POST", &format!("/v1/campaigns/{camp}/leads"), Some(json!({ "filter": { "q": "other.io" } }))).await;
    assert_eq!((r["matched"].as_i64(), r["added"].as_i64()), (Some(2), Some(2)));
    let _ = other;
    // the lead timeline
    let events = c.leads("GET", &format!("/v1/leads/{}/events", ids[0]), None).await;
    assert_eq!(events["events"][0]["kind"], "campaign_added");
    assert_eq!(events["events"][0]["ref_id"], camp);
    // bad selections
    for (body, expected) in [
        (json!({}), "bad_selection"),
        (json!({ "lead_ids": [ids[0]], "list_id": list }), "bad_selection"),
        (json!({ "lead_ids": [] }), "invalid_selection"),
        (json!({ "filter": {} }), "empty_selection"),
    ] {
        let r = c.api("POST", &format!("/v1/campaigns/{camp}/leads"), Some(body.clone())).await;
        assert_eq!((r.status, code(&r).as_str()), (StatusCode::UNPROCESSABLE_ENTITY, expected), "{body}");
    }
    assert_eq!(c.api("POST", &format!("/v1/campaigns/{}/leads", Uuid::now_v7()), Some(json!({ "lead_ids": [ids[0]] }))).await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn unknown_lead_ids_are_not_matched_and_removed_leads_leave_the_campaign(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp = id(&c.campaign(&[a], json!({})).await);
    let ids = c.many_leads(3, "acme.com").await;
    let r = c.add(&camp, &[ids[0], ids[1], Uuid::now_v7()]).await;
    assert_eq!((r["matched"].as_i64(), r["added"].as_i64()), (Some(2), Some(2)));
    // a lead that is deleted leaves the campaign
    c.leads("DELETE", &format!("/v1/leads/{}", ids[1]), None).await;
    assert_eq!(c.count_leads(&camp, "pending").await, 1);
}

#[sqlx::test]
async fn remove_takes_out_pending_leads_only(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp = id(&c.campaign(&[a], json!({})).await);
    let ids = c.many_leads(3, "acme.com").await;
    c.add(&camp, &ids).await;
    sqlx::query("UPDATE campaign_leads SET state = 'skipped', skip_reason = 'suppressed' WHERE lead_id = $1").bind(ids[2]).execute(&c.h.state.pool).await.unwrap();
    let r = c.ok("DELETE", &format!("/v1/campaigns/{camp}/leads"), Some(json!({ "lead_ids": [ids[0], ids[2]] }))).await;
    assert_eq!((r["matched"].as_i64(), r["removed"].as_i64(), r["not_removed"].as_i64()), (Some(2), Some(1), Some(1)));
    assert_eq!(c.get(&camp).await["counts"]["pending"], 1);
    assert_eq!(c.get(&camp).await["counts"]["skipped"], 1);
    // the lead can be added again
    assert_eq!(c.add(&camp, &[ids[0]]).await["added"], 1);
}

#[sqlx::test]
async fn list_campaign_leads_pages_and_filters(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp = id(&c.campaign(&[a], json!({})).await);
    let ids = c.many_leads(7, "acme.com").await;
    c.add(&camp, &ids).await;
    sqlx::query("UPDATE campaign_leads SET state = 'skipped', skip_reason = 'suppressed' WHERE lead_id = ANY($1)").bind(&ids[..2]).execute(&c.h.state.pool).await.unwrap();
    let mut seen = Vec::new();
    let mut cursor = String::new();
    loop {
        let r = c.ok("GET", &format!("/v1/campaigns/{camp}/leads?limit=3{cursor}"), None).await;
        seen.extend(r["leads"].as_array().unwrap().iter().map(|l| l["lead_id"].as_str().unwrap().to_string()));
        match r["next_cursor"].as_str() {
            Some(n) => cursor = format!("&cursor={n}"),
            None => break,
        }
    }
    assert_eq!(seen.len(), 7);
    let skipped = c.ok("GET", &format!("/v1/campaigns/{camp}/leads?state=skipped"), None).await;
    assert_eq!(skipped["leads"].as_array().unwrap().len(), 2);
    assert_eq!(skipped["leads"][0]["skip_reason"], "suppressed");
    let none = c.ok("GET", &format!("/v1/campaigns/{camp}/leads?skip_reason=unverified"), None).await;
    assert_eq!(none["leads"], json!([]));
    let bad = c.api("GET", &format!("/v1/campaigns/{camp}/leads?cursor={}", Uuid::now_v7()), None).await;
    assert_eq!((bad.status, code(&bad).as_str()), (StatusCode::BAD_REQUEST, "invalid_cursor"));
}

#[sqlx::test]
async fn campaign_list_shows_counts(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let first = id(&c.campaign(&[a], json!({ "name": "First" })).await);
    let second = id(&c.campaign(&[a], json!({ "name": "Second" })).await);
    let ids = c.many_leads(3, "acme.com").await;
    c.add(&first, &ids).await;
    let r = c.ok("GET", "/v1/campaigns", None).await;
    assert_eq!(r["campaigns"][0]["id"], second, "newest first");
    assert_eq!(r["campaigns"][0]["counts"]["pending"], 0);
    assert_eq!(r["campaigns"][1]["counts"]["pending"], 3);
    assert_eq!(c.ok("GET", "/v1/campaigns?status=active", None).await["campaigns"], json!([]));
    let page = c.ok("GET", "/v1/campaigns?limit=1", None).await;
    assert_eq!(page["campaigns"].as_array().unwrap().len(), 1);
    assert_eq!(page["next_cursor"], second);
}

// ---------- phase 7: launch, pause, resume, archive ----------

#[sqlx::test]
async fn launch_checks_accounts(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let none = id(&c.campaign(&[], json!({})).await);
    let r = c.api("POST", &format!("/v1/campaigns/{none}/launch"), None).await;
    assert_eq!((r.status, code(&r).as_str()), (StatusCode::UNPROCESSABLE_ENTITY, "no_accounts"));
    let camp = id(&c.campaign(&[a], json!({})).await);
    c.pause_account(a).await;
    let r = c.api("POST", &format!("/v1/campaigns/{camp}/launch"), None).await;
    assert_eq!((r.status, code(&r).as_str()), (StatusCode::UNPROCESSABLE_ENTITY, "no_healthy_account"));
}

#[sqlx::test]
async fn launch_names_missing_variables(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let body = json!({ "name": "C", "account_ids": [a], "sequence": { "name": "S", "steps": [
        { "subject": "Hi {{first_name}} from {{sender_name}}", "body_text": "About {{pain_point}} and {{budget}} and {{greeting|hello}}, {{company_name}}" }] } });
    let camp = id(&c.ok("POST", "/v1/campaigns", Some(body)).await);
    let r = c.api("POST", &format!("/v1/campaigns/{camp}/launch"), None).await;
    assert_eq!((r.status, code(&r).as_str()), (StatusCode::UNPROCESSABLE_ENTITY, "missing_variables"));
    let msg = r.body["error"]["message"].as_str().unwrap();
    assert!(msg.contains("budget") && msg.contains("pain_point"), "{msg}");
    assert!(!msg.contains("first_name") && !msg.contains("sender_name") && !msg.contains("company_name") && !msg.contains("greeting"), "{msg}");
    // make the fields: now it launches
    for k in ["pain_point", "budget"] {
        c.leads("POST", "/v1/leads/fields", Some(json!({ "key": k }))).await;
    }
    let launched = c.launch(&camp).await;
    assert_eq!(launched["status"], "active");
}

#[sqlx::test]
async fn launch_starts_the_campaign_and_its_sequence_and_warns_about_capacity(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp = id(&c.campaign(&[a], json!({ "daily_new_leads": 5000 })).await);
    assert_eq!(c.sequence_status(&camp).await, "paused");
    let r = c.launch(&camp).await;
    assert_eq!(r["status"], "active");
    assert!(r["launched_at"].is_string());
    assert_eq!(c.sequence_status(&camp).await, "active");
    let warnings: Vec<&str> = r["warnings"].as_array().unwrap().iter().map(|w| w.as_str().unwrap()).collect();
    assert!(warnings.iter().any(|w| w.contains("5000") && w.contains("2 step")), "{warnings:?}");
    // a small plan: no warning
    let small = id(&c.campaign(&[a], json!({ "daily_new_leads": 1 })).await);
    assert_eq!(c.launch(&small).await["warnings"], json!([]));
    // launch twice, or launch what is not a draft
    let again = c.api("POST", &format!("/v1/campaigns/{camp}/launch"), None).await;
    assert_eq!((again.status, code(&again).as_str()), (StatusCode::CONFLICT, "invalid_status"));
    assert_eq!(c.api("POST", &format!("/v1/campaigns/{}/launch", Uuid::now_v7()), None).await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn pause_resume_and_archive(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp = id(&c.campaign(&[a], json!({})).await);
    let post = |action: &'static str| {
        let camp = camp.clone();
        let c = &c;
        async move { c.api("POST", &format!("/v1/campaigns/{camp}/{action}"), None).await }
    };
    // wrong states
    for action in ["pause", "resume", "archive"] {
        let r = post(action).await;
        assert_eq!((r.status, code(&r).as_str()), (StatusCode::CONFLICT, "invalid_status"), "{action} of a draft");
    }
    c.launch(&camp).await;
    assert_eq!(post("resume").await.status, StatusCode::CONFLICT);
    assert_eq!(post("archive").await.status, StatusCode::CONFLICT, "an active campaign must be paused first");
    let r = post("pause").await;
    assert_eq!(r.body["status"], "paused");
    assert_eq!(c.sequence_status(&camp).await, "paused", "the follow-up engine skips it");
    assert_eq!(post("pause").await.status, StatusCode::CONFLICT);
    let r = post("resume").await;
    assert_eq!(r.body["status"], "active");
    assert_eq!(c.sequence_status(&camp).await, "active");

    // archive: pending leads are skipped
    let ids = c.many_leads(3, "acme.com").await;
    c.add(&camp, &ids).await;
    post("pause").await;
    let r = post("archive").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["status"], "archived");
    assert_eq!(r.body["counts"]["skipped"], 3);
    assert_eq!(r.body["counts"]["skipped_by_reason"]["campaign_archived"], 3);
    assert_eq!(c.sequence_status(&camp).await, "archived");
    // an archived campaign takes nothing
    assert_eq!(c.api("POST", &format!("/v1/campaigns/{camp}/leads"), Some(json!({ "lead_ids": ids }))).await.status, StatusCode::CONFLICT);
    assert_eq!(c.api("PATCH", &format!("/v1/campaigns/{camp}"), Some(json!({ "name": "x" }))).await.status, StatusCode::CONFLICT);
    assert_eq!(post("resume").await.status, StatusCode::CONFLICT);
}

#[sqlx::test]
async fn detail_warns_about_an_unhealthy_account(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let b = c.account("b@x.test").await;
    let camp = id(&c.campaign(&[a, b], json!({ "daily_new_leads": 1 })).await);
    c.launch(&camp).await;
    c.pause_account(b).await;
    let d = c.get(&camp).await;
    let w = d["warnings"].as_array().unwrap();
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].as_str().unwrap().contains("b@x.test"));
    let healthy: Vec<bool> = d["accounts"].as_array().unwrap().iter().map(|x| x["healthy"].as_bool().unwrap()).collect();
    assert_eq!(healthy, [true, false]);
}

#[sqlx::test]
async fn the_field_guard_covers_the_sequence_of_an_active_campaign(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    c.leads("POST", "/v1/leads/fields", Some(json!({ "key": "pain_point", "fallback": "growth" }))).await;
    let body = json!({ "name": "C", "account_ids": [a], "sequence": { "name": "Pain seq", "steps": [{ "subject": "Hi", "body_text": "About {{pain_point}}" }] } });
    let camp = id(&c.ok("POST", "/v1/campaigns", Some(body)).await);
    // a draft campaign is not running: launch will check the variables again
    let r = call(&c.h.state, "DELETE", "/v1/leads/fields/pain_point", Some(&c.keys[&Scope::Leads]), None).await;
    assert_eq!(r.status, StatusCode::OK);
    c.leads("POST", "/v1/leads/fields", Some(json!({ "key": "pain_point", "fallback": "growth" }))).await;
    c.launch(&camp).await;
    let r = call(&c.h.state, "DELETE", "/v1/leads/fields/pain_point", Some(&c.keys[&Scope::Leads]), None).await;
    assert_eq!((r.status, code(&r).as_str()), (StatusCode::CONFLICT, "field_in_use"));
    assert!(r.body["error"]["message"].as_str().unwrap().contains("Pain seq"));
    c.ok("POST", &format!("/v1/campaigns/{camp}/pause"), None).await;
    let r = call(&c.h.state, "DELETE", "/v1/leads/fields/pain_point", Some(&c.keys[&Scope::Leads]), None).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "a paused campaign can be resumed");
    let _ = (JobStatus::Queued, Duration::days(1));
}

// =====================================================================================================
// Phase 8: the feeder
// =====================================================================================================

#[sqlx::test]
async fn the_daily_limit_drips_leads_in_over_days(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp = id(&c.campaign(&[a], json!({ "daily_new_leads": 50 })).await);
    let ids = c.many_leads(120, "acme.com").await;
    c.add(&camp, &ids).await;
    // not launched yet: the feeder does nothing
    assert_eq!(c.feed().await.campaigns, 0);
    c.launch(&camp).await;

    let r = c.feed().await;
    assert_eq!((r.campaigns, r.enrolled), (1, 50), "{r:?}");
    assert_eq!((c.count_leads(&camp, "enrolled").await, c.count_leads(&camp, "pending").await), (50, 70));
    // the same day again, a few hours later: nothing more
    c.h.advance(Duration::hours(3));
    assert_eq!(c.feed().await.enrolled, 0);
    assert_eq!(c.count_leads(&camp, "enrolled").await, 50);
    // tomorrow: 50 more
    c.h.advance(Duration::days(1));
    assert_eq!(c.feed().await.enrolled, 50);
    assert_eq!(c.count_leads(&camp, "enrolled").await, 100);
    // the day after: the last 20
    c.h.advance(Duration::days(1));
    assert_eq!(c.feed().await.enrolled, 20);
    assert_eq!((c.count_leads(&camp, "enrolled").await, c.count_leads(&camp, "pending").await), (120, 0));
    c.h.advance(Duration::days(1));
    assert_eq!(c.feed().await.enrolled, 0);
}

#[sqlx::test]
async fn the_day_starts_at_midnight_in_the_campaign_time_zone(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    // The harness clock starts Monday 2026-10-19 10:00 UTC = 16:00 in Dhaka (UTC+6). Midnight in Dhaka is 18:00 UTC.
    let camp = id(&c.campaign(&[a], json!({ "daily_new_leads": 2, "timezone": "Asia/Dhaka" })).await);
    c.add(&camp, &c.many_leads(6, "acme.com").await).await;
    c.launch(&camp).await;
    assert_eq!(c.feed().await.enrolled, 2);
    c.h.set_time(common::utc(2026, 10, 19, 17, 59));
    assert_eq!(c.feed().await.enrolled, 0, "still the same day in Dhaka");
    c.h.set_time(common::utc(2026, 10, 19, 18, 0));
    assert_eq!(c.feed().await.enrolled, 2, "a new day in Dhaka, though it is the same UTC day");
    // a UTC campaign sees the day change at 00:00 UTC
    let utc = id(&c.campaign(&[a], json!({ "daily_new_leads": 1 })).await);
    c.add(&utc, &c.many_leads(3, "utc.io").await).await;
    c.launch(&utc).await;
    assert_eq!(c.feed().await.enrolled, 1);
    c.h.set_time(common::utc(2026, 10, 19, 23, 59));
    assert_eq!(c.feed().await.enrolled, 0);
    c.h.set_time(common::utc(2026, 10, 20, 0, 0));
    // the UTC campaign has a new day (1 lead); the Dhaka campaign is still on the day that began at 18:00 UTC (its 2 are done)
    assert_eq!(c.feed().await.enrolled, 1);
}

#[sqlx::test]
async fn raising_the_daily_limit_counts_at_once(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp = id(&c.campaign(&[a], json!({ "daily_new_leads": 2 })).await);
    c.add(&camp, &c.many_leads(10, "acme.com").await).await;
    c.launch(&camp).await;
    assert_eq!(c.feed().await.enrolled, 2);
    c.ok("PATCH", &format!("/v1/campaigns/{camp}"), Some(json!({ "daily_new_leads": 5 }))).await;
    assert_eq!(c.feed().await.enrolled, 3, "5 a day, 2 done");
}

#[sqlx::test]
async fn skipped_leads_do_not_use_up_the_daily_limit_and_say_why(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp = id(&c.campaign(&[a], json!({ "daily_new_leads": 2 })).await);
    let other_seq = c.fapi("POST", "/v1/followups/sequences", Some(json!({ "name": "Other", "steps": C::steps(2) }))).await.body["sequence"]["id"].as_str().unwrap().to_string();
    // the leads, oldest first
    let mk = |email: &'static str| {
        let c = &c;
        async move { c.lead(email).await }
    };
    let suppressed = mk("blocked@acme.com").await;
    let domain_blocked = mk("someone@blocked-domain.com").await;
    let no_mx = mk("nomx@dead.test").await;
    let busy = mk("busy@acme.com").await;
    let bounced = mk("bounced@acme.com").await;
    let unsub = mk("unsub@acme.com").await;
    let missing = mk("missing@acme.com").await;
    let good1 = mk("good1@acme.com").await;
    let good2 = mk("good2@acme.com").await;
    let good3 = mk("good3@acme.com").await;
    sqlx::query("INSERT INTO suppressions (value, kind, reason) VALUES ('blocked@acme.com', 'email', 'manual'), ('blocked-domain.com', 'domain', 'manual')").execute(&c.h.state.pool).await.unwrap();
    sqlx::query("UPDATE leads SET verification = 'no_mx' WHERE id = $1").bind(no_mx).execute(&c.h.state.pool).await.unwrap();
    sqlx::query("UPDATE leads SET contact_state = 'bounced' WHERE id = $1").bind(bounced).execute(&c.h.state.pool).await.unwrap();
    sqlx::query("UPDATE leads SET contact_state = 'unsubscribed' WHERE id = $1").bind(unsub).execute(&c.h.state.pool).await.unwrap();
    // busy is in an open enrollment of another sequence
    let r = c.fapi("POST", &format!("/v1/followups/sequences/{other_seq}/enrollments"), Some(json!({ "enrollments": [{ "lead_id": busy, "account_id": a }] }))).await;
    assert_eq!(r.body["enrolled"], 1, "{}", r.body);
    // 'missing' lacks a value that the steps need, and the field has no fallback
    c.leads("POST", "/v1/leads/fields", Some(json!({ "key": "pain_point" }))).await;
    sqlx::query("UPDATE sequence_steps SET body_text = 'About {{pain_point}}' WHERE sequence_id = (SELECT sequence_id FROM campaigns WHERE id = $1) AND step_number = 1")
        .bind(camp.parse::<Uuid>().unwrap()).execute(&c.h.state.pool).await.unwrap();
    for g in [good1, good2, good3] {
        c.leads("PATCH", &format!("/v1/leads/{g}"), Some(json!({ "custom_fields": { "pain_point": "growth" } }))).await;
    }
    let all = [suppressed, domain_blocked, no_mx, busy, bounced, unsub, missing, good1, good2, good3];
    c.add(&camp, &all).await;
    c.launch(&camp).await;

    let r = c.feed().await;
    assert_eq!((r.enrolled, r.skipped), (2, 7), "{r:?}");
    let reasons = c.skip_reasons(&camp).await;
    let expect = [
        ("blocked@acme.com", "suppressed"),
        ("someone@blocked-domain.com", "suppressed"),
        ("nomx@dead.test", "unverified"),
        ("busy@acme.com", "active_elsewhere"),
        ("bounced@acme.com", "suppressed"),
        ("unsub@acme.com", "suppressed"),
        ("missing@acme.com", "missing_variable"),
    ];
    for (email, reason) in expect {
        assert_eq!(reasons.get(email).map(String::as_str), Some(reason), "{email}: {reasons:?}");
    }
    let enrolled: Vec<(String,)> = sqlx::query_as("SELECT l.email FROM campaign_leads cl JOIN leads l ON l.id = cl.lead_id WHERE cl.campaign_id = $1 AND cl.state = 'enrolled' ORDER BY l.email").bind(camp.parse::<Uuid>().unwrap()).fetch_all(&c.h.state.pool).await.unwrap();
    assert_eq!(enrolled, [("good1@acme.com".to_string(),), ("good2@acme.com".to_string(),)]);
    assert_eq!(c.count_leads(&camp, "pending").await, 1, "good3 waits for tomorrow");
    let counts = &c.get(&camp).await["counts"];
    assert_eq!(counts["skipped_by_reason"]["suppressed"], 4);
    // the list shows the reason
    let list = c.ok("GET", &format!("/v1/campaigns/{camp}/leads?skip_reason=missing_variable"), None).await;
    assert_eq!(list["leads"][0]["email"], "missing@acme.com");
}

#[sqlx::test]
async fn allow_unverified_lets_a_no_mx_lead_in(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp = id(&c.campaign(&[a], json!({ "allow_unverified": true })).await);
    let l = c.lead("nomx@dead.test").await;
    sqlx::query("UPDATE leads SET verification = 'no_mx' WHERE id = $1").bind(l).execute(&c.h.state.pool).await.unwrap();
    c.add(&camp, &[l]).await;
    c.launch(&camp).await;
    assert_eq!(c.feed().await.enrolled, 1);
}

#[sqlx::test]
async fn an_enrolled_lead_is_in_sequence_with_an_event_and_its_values_fill_the_email(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp = id(&c.campaign(&[a], json!({})).await);
    let lead = c.lead("ana@acme.com").await;
    c.add(&camp, &[lead]).await;
    c.launch(&camp).await;
    assert_eq!(c.feed().await.enrolled, 1);
    let l = c.leads("GET", &format!("/v1/leads/{lead}"), None).await;
    assert_eq!(l["contact_state"], "in_sequence");
    assert!(l["active_enrollment_id"].is_string());
    let events = c.leads("GET", &format!("/v1/leads/{lead}/events"), None).await;
    assert_eq!(events["events"][0]["kind"], "enrolled");
    assert_eq!(events["events"][0]["ref_id"], l["active_enrollment_id"]);
    let cl = c.ok("GET", &format!("/v1/campaigns/{camp}/leads"), None).await;
    assert_eq!(cl["leads"][0]["state"], "enrolled");
    assert_eq!(cl["leads"][0]["enrollment_status"], "active");
    // the enrollment is linked to the lead and the sequence has the campaign
    let (lid, key): (Option<Uuid>, Option<String>) = sqlx::query_as("SELECT lead_id, idempotency_key FROM enrollments").fetch_one(&c.h.state.pool).await.unwrap();
    assert_eq!(lid, Some(lead));
    assert_eq!(key, Some(format!("campaign:{camp}:lead:{lead}")));
    c.round(Duration::minutes(10)).await;
    let (subject, campaign_id): (String, Option<Uuid>) = sqlx::query_as("SELECT subject, campaign_id FROM send_jobs").fetch_one(&c.h.state.pool).await.unwrap();
    assert_eq!(subject, "Hello Ana");
    assert_eq!(campaign_id, Some(camp.parse().unwrap()), "the job knows its campaign");
}

// ---------- account pick ----------

#[sqlx::test]
async fn least_loaded_spreads_the_leads_over_the_accounts(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let b = c.account("b@x.test").await;
    let camp = id(&c.campaign(&[a, b], json!({ "daily_new_leads": 100 })).await);
    c.add(&camp, &c.many_leads(20, "acme.com").await).await;
    c.launch(&camp).await;
    assert_eq!(c.feed().await.enrolled, 20);
    let per: HashMap<Uuid, usize> = c.enrollments_of(&camp).await.into_iter().fold(HashMap::new(), |mut m, (_, acct, _)| {
        *m.entry(acct).or_default() += 1;
        m
    });
    assert_eq!((per[&a], per[&b]), (10, 10), "{per:?}");
}

#[sqlx::test]
async fn least_loaded_counts_enrollments_from_other_campaigns(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let b = c.account("b@x.test").await;
    let first = id(&c.campaign(&[a], json!({ "daily_new_leads": 100 })).await);
    c.add(&first, &c.many_leads(6, "one.io").await).await;
    c.launch(&first).await;
    c.feed().await;
    let second = id(&c.campaign(&[a, b], json!({ "daily_new_leads": 100 })).await);
    c.add(&second, &c.many_leads(4, "two.io").await).await;
    c.launch(&second).await;
    c.feed().await;
    let accounts: Vec<Uuid> = c.enrollments_of(&second).await.into_iter().map(|e| e.1).collect();
    assert!(accounts.iter().all(|x| *x == b), "a has 6 open enrollments, b has none: {accounts:?}");
}

#[sqlx::test]
async fn round_robin_takes_the_accounts_in_turn(pool: PgPool) {
    let c = C::new(pool).await;
    let accounts = [c.account("a@x.test").await, c.account("b@x.test").await, c.account("c@x.test").await];
    let camp = id(&c.campaign(&accounts, json!({ "daily_new_leads": 100, "account_pick": "round_robin" })).await);
    c.add(&camp, &c.many_leads(7, "acme.com").await).await;
    c.launch(&camp).await;
    c.feed().await;
    let used: Vec<Uuid> = c.enrollments_of(&camp).await.into_iter().map(|e| e.1).collect();
    let mut sorted = accounts.to_vec();
    sorted.sort();
    let expected: Vec<Uuid> = (0..7).map(|i| sorted[i % 3]).collect();
    assert_eq!(used, expected);
}

#[sqlx::test]
async fn an_unhealthy_account_gets_no_new_leads(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let b = c.account("b@x.test").await;
    let camp = id(&c.campaign(&[a, b], json!({ "daily_new_leads": 100, "account_pick": "round_robin" })).await);
    c.add(&camp, &c.many_leads(6, "acme.com").await).await;
    c.launch(&camp).await;
    c.pause_account(b).await;
    c.feed().await;
    let used: Vec<Uuid> = c.enrollments_of(&camp).await.into_iter().map(|e| e.1).collect();
    assert_eq!(used.len(), 6);
    assert!(used.iter().all(|x| *x == a));
}

#[sqlx::test]
async fn no_healthy_account_leaves_the_leads_pending_with_a_warning_until_one_returns(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp = id(&c.campaign(&[a], json!({})).await);
    c.add(&camp, &c.many_leads(3, "acme.com").await).await;
    c.launch(&camp).await;
    c.pause_account(a).await;
    let r = c.feed().await;
    assert_eq!((r.enrolled, r.skipped, r.stuck), (0, 0, 1), "{r:?}");
    assert_eq!(c.count_leads(&camp, "pending").await, 3, "not skipped: they retry");
    let d = c.get(&camp).await;
    assert!(d["warning"].as_str().unwrap().contains("healthy"), "{d}");
    assert!(d["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().contains("healthy")));
    let status = c.ok("GET", "/v1/campaigns/status", None).await;
    assert_eq!(status["waiting"][0]["campaign_id"], camp);
    assert_eq!(status["active_campaigns"], 1);
    // the account comes back
    call(&c.h.state, "POST", &format!("/v1/sending/accounts/{a}/resume"), Some(&c.keys[&Scope::Sending]), None).await;
    assert_eq!(c.feed().await.enrolled, 3);
    assert_eq!(c.get(&camp).await["warning"], Value::Null);
    assert_eq!(c.ok("GET", "/v1/campaigns/status", None).await["waiting"], json!([]));
}

#[sqlx::test]
async fn removing_an_account_from_the_pool_leaves_open_enrollments_alone(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let b = c.account("b@x.test").await;
    let camp = id(&c.campaign(&[a], json!({ "daily_new_leads": 2 })).await);
    c.add(&camp, &c.many_leads(4, "acme.com").await).await;
    c.launch(&camp).await;
    assert_eq!(c.feed().await.enrolled, 2);
    c.ok("PATCH", &format!("/v1/campaigns/{camp}"), Some(json!({ "account_ids": [b] }))).await;
    c.h.advance(Duration::days(1));
    assert_eq!(c.feed().await.enrolled, 2);
    let accounts: Vec<Uuid> = c.enrollments_of(&camp).await.into_iter().map(|e| e.1).collect();
    assert_eq!(accounts, [a, a, b, b], "the first two keep a; the new ones use b");
    // and the old ones still send from a
    let open: Vec<(Uuid,)> = sqlx::query_as("SELECT account_id FROM enrollments ORDER BY created_at, id").fetch_all(&c.h.state.pool).await.unwrap();
    assert_eq!(open.iter().filter(|o| o.0 == a).count(), 2);
}

// ---------- complete, pause, the flag ----------

#[sqlx::test]
async fn a_campaign_completes_when_nothing_is_pending_and_every_enrollment_ended(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let body = json!({ "name": "C", "account_ids": [a], "daily_new_leads": 5, "sequence": { "name": "S", "steps": C::steps(1) } });
    let camp = id(&c.ok("POST", "/v1/campaigns", Some(body)).await);
    // active with no lead at all: not done
    c.launch(&camp).await;
    assert_eq!(c.feed().await.completed, 0);
    assert_eq!(c.get(&camp).await["status"], "active");
    let ids = c.many_leads(3, "acme.com").await;
    c.add(&camp, &ids).await;
    assert_eq!(c.feed().await.enrolled, 3);
    assert_eq!(c.feed().await.completed, 0, "the enrollments are still open");
    for _ in 0..60 {
        c.round(Duration::minutes(10)).await;
        if c.get(&camp).await["status"] == "completed" {
            break;
        }
    }
    let d = c.get(&camp).await;
    assert_eq!(d["status"], "completed", "{d}");
    assert!(d["completed_at"].is_string());
    assert_eq!(d["counts"]["enrollments"]["finished"], 3);
    // every lead is completed
    let l = c.leads("GET", &format!("/v1/leads/{}", ids[0]), None).await;
    assert_eq!(l["contact_state"], "completed");
    // a completed campaign takes no more leads, but can be archived
    assert_eq!(c.api("POST", &format!("/v1/campaigns/{camp}/leads"), Some(json!({ "lead_ids": ids }))).await.status, StatusCode::CONFLICT);
    assert_eq!(c.ok("POST", &format!("/v1/campaigns/{camp}/archive"), None).await["status"], "archived");
}

#[sqlx::test]
async fn a_pending_lead_keeps_the_campaign_open(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let body = json!({ "name": "C", "account_ids": [a], "daily_new_leads": 1, "sequence": { "name": "S", "steps": C::steps(1) } });
    let camp = id(&c.ok("POST", "/v1/campaigns", Some(body)).await);
    c.add(&camp, &c.many_leads(2, "acme.com").await).await;
    c.launch(&camp).await;
    for _ in 0..20 {
        c.round(Duration::minutes(10)).await;
    }
    assert_eq!(c.get(&camp).await["status"], "active", "one lead still waits for tomorrow");
    c.h.advance(Duration::days(1));
    for _ in 0..30 {
        c.round(Duration::minutes(10)).await;
    }
    assert_eq!(c.get(&camp).await["status"], "completed");
}

#[sqlx::test]
async fn a_paused_campaign_is_not_fed_and_its_sequence_makes_no_steps(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp = id(&c.campaign(&[a], json!({ "daily_new_leads": 1 })).await);
    c.add(&camp, &c.many_leads(3, "acme.com").await).await;
    c.launch(&camp).await;
    assert_eq!(c.feed().await.enrolled, 1);
    c.ok("POST", &format!("/v1/campaigns/{camp}/pause"), None).await;
    c.h.advance(Duration::days(1));
    assert_eq!(c.feed().await.campaigns, 0);
    c.fu_tick().await;
    let (jobs,): (i64,) = sqlx::query_as("SELECT count(*) FROM send_jobs").fetch_one(&c.h.state.pool).await.unwrap();
    assert_eq!(jobs, 0, "the paused sequence makes no step");
    c.ok("POST", &format!("/v1/campaigns/{camp}/resume"), None).await;
    assert_eq!(c.feed().await.enrolled, 1);
    c.fu_tick().await;
    let (jobs,): (i64,) = sqlx::query_as("SELECT count(*) FROM send_jobs").fetch_one(&c.h.state.pool).await.unwrap();
    assert_eq!(jobs, 2);
}

#[sqlx::test]
async fn the_feeder_flag_pauses_every_campaign_and_is_in_the_database(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp = id(&c.campaign(&[a], json!({})).await);
    c.add(&camp, &c.many_leads(2, "acme.com").await).await;
    c.launch(&camp).await;
    assert_eq!(c.ok("POST", "/v1/campaigns/feeder/pause", None).await["paused"], true);
    assert_eq!(c.feed().await.campaigns, 0);
    let s = c.ok("GET", "/v1/campaigns/status", None).await;
    assert_eq!(s["paused"], true);
    assert!(s["last_tick_at"].is_string(), "the feeder ticked, though it did nothing");
    assert_eq!(c.ok("POST", "/v1/campaigns/feeder/resume", None).await["paused"], false);
    assert_eq!(c.feed().await.enrolled, 2);
    assert_eq!(c.ok("GET", "/v1/campaigns/status", None).await["paused"], false);
}

#[sqlx::test]
async fn status_shows_capacity_warnings(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp = id(&c.campaign(&[a], json!({ "daily_new_leads": 5000 })).await);
    c.launch(&camp).await;
    let s = c.ok("GET", "/v1/campaigns/status", None).await;
    assert_eq!(s["capacity_warnings"][0]["campaign_id"], camp);
    assert_eq!(s["tick_secs"], 60);
}

// ---------- two server copies, a dead copy, a retry ----------

#[sqlx::test]
async fn two_feeders_at_the_same_moment_enroll_each_lead_once(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp = id(&c.campaign(&[a], json!({ "daily_new_leads": 30 })).await);
    c.add(&camp, &c.many_leads(100, "acme.com").await).await;
    c.launch(&camp).await;
    let (x, y) = tokio::join!(feeder::tick(&c.h.state), feeder::tick(&c.h.state));
    let (x, y) = (x.unwrap(), y.unwrap());
    assert_eq!(x.enrolled + y.enrolled, 30, "{x:?} {y:?}");
    let (n, distinct): (i64, i64) = sqlx::query_as("SELECT count(*), count(DISTINCT to_email) FROM enrollments").fetch_one(&c.h.state.pool).await.unwrap();
    assert_eq!((n, distinct), (30, 30));
}

#[sqlx::test]
async fn a_campaign_held_by_a_dead_copy_is_taken_after_the_lease(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp = id(&c.campaign(&[a], json!({})).await);
    c.add(&camp, &c.many_leads(2, "acme.com").await).await;
    c.launch(&camp).await;
    // another copy claimed it and died
    sqlx::query("UPDATE campaigns SET claimed_at = $2 WHERE id = $1").bind(camp.parse::<Uuid>().unwrap()).bind(c.h.now()).execute(&c.h.state.pool).await.unwrap();
    assert_eq!(c.feed().await.campaigns, 0);
    c.h.advance(Duration::seconds(feeder::LEASE_SECS + 1));
    assert_eq!(c.feed().await.enrolled, 2);
}

#[sqlx::test]
async fn a_retry_after_a_crash_does_not_enroll_twice(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp = id(&c.campaign(&[a], json!({ "daily_new_leads": 1 })).await);
    let lead = c.lead("ana@acme.com").await;
    c.add(&camp, &[lead]).await;
    c.launch(&camp).await;
    // the enrollment was made, but the copy died before it saved the row (the key is the same)
    let seq: Uuid = c.get(&camp).await["sequence_id"].as_str().unwrap().parse().unwrap();
    let r = c.fapi("POST", &format!("/v1/followups/sequences/{seq}/enrollments"), Some(json!({ "enrollments": [
        { "lead_id": lead, "account_id": a, "idempotency_key": format!("campaign:{camp}:lead:{lead}") }] }))).await;
    assert_eq!(r.body["enrolled"], 1);
    let first = r.body["items"][0]["enrollment"]["id"].as_str().unwrap().to_string();
    assert_eq!(c.count_leads(&camp, "pending").await, 1);
    // the feeder finds the lead pending, tries again, and gets the old enrollment back. It is "busy" otherwise.
    let rep = c.feed().await;
    let cl = c.ok("GET", &format!("/v1/campaigns/{camp}/leads"), None).await;
    assert_eq!(cl["leads"][0]["enrollment_id"], first, "{rep:?} {cl}");
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM enrollments").fetch_one(&c.h.state.pool).await.unwrap();
    assert_eq!(n, 1);
}

// ---------- company stop ----------

const FIXTURE_ID: &str = "<sent-0001@acme-outreach.example>";
const ME_ACCOUNT: &str = "sender@acme-outreach.example";

/// Three leads at prospect.example: jane (replies), a colleague (enrolled), and a third who waits (limit 2 a day).
async fn company_setup(c: &C, flag: bool) -> (String, Uuid, Vec<Uuid>) {
    let a = c.account(ME_ACCOUNT).await;
    let camp = id(&c.campaign(&[a], json!({ "daily_new_leads": 2, "stop_company_on_reply": flag })).await);
    let mut ids = Vec::new();
    for e in ["jane@prospect.example", "colleague@prospect.example", "third@prospect.example", "other@elsewhere.example"] {
        ids.push(c.lead(e).await);
    }
    c.add(&camp, &ids).await;
    c.launch(&camp).await;
    c.feed().await;
    // send step 1 of both enrolled leads
    for _ in 0..40 {
        c.fu_tick().await;
        c.h.tick().await;
        c.h.advance(Duration::minutes(10));
        let (sent,): (i64,) = sqlx::query_as("SELECT count(*) FROM send_jobs WHERE status = 'sent'").fetch_one(&c.h.state.pool).await.unwrap();
        if sent == 2 {
            break;
        }
    }
    let (job,): (Uuid,) = sqlx::query_as("SELECT id FROM send_jobs WHERE to_email = 'jane@prospect.example'").fetch_one(&c.h.state.pool).await.unwrap();
    sqlx::query("UPDATE send_jobs SET message_id_header = $2 WHERE id = $1").bind(job).bind(FIXTURE_ID).execute(&c.h.state.pool).await.unwrap();
    (camp, a, ids)
}

#[sqlx::test]
async fn a_reply_stops_the_colleagues_at_the_same_company(pool: PgPool) {
    let c = C::new(pool).await;
    let (camp, a, ids) = company_setup(&c, true).await;
    assert_eq!(c.count_leads(&camp, "enrolled").await, 2);
    assert_eq!(c.count_leads(&camp, "pending").await, 2);
    ingest_raw(&c.h.state, a, "p1", &fixture("human_plain.eml"), c.h.now()).await;

    let rows: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT e.to_email::text, e.status::text, e.stop_reason FROM enrollments e ORDER BY e.to_email",
    )
    .fetch_all(&c.h.state.pool)
    .await
    .unwrap();
    assert_eq!(rows[0], ("colleague@prospect.example".into(), "stopped".into(), Some("company_replied".into())));
    assert_eq!(rows[1], ("jane@prospect.example".into(), "stopped".into(), Some("reply".into())));
    let reasons = c.skip_reasons(&camp).await;
    assert_eq!(reasons.get("third@prospect.example").map(String::as_str), Some("company_replied"));
    assert!(!reasons.contains_key("other@elsewhere.example"), "a lead at another company still waits");
    assert_eq!(c.count_leads(&camp, "pending").await, 1);
    // the colleague's timeline says why
    let events = c.leads("GET", &format!("/v1/leads/{}/events", ids[1]), None).await;
    let stopped = events["events"].as_array().unwrap().iter().find(|e| e["kind"] == "stopped").expect("a stopped event");
    assert_eq!(stopped["detail"]["reason"], "company_replied");
    // jane is replied, the colleague is not
    assert_eq!(c.leads("GET", &format!("/v1/leads/{}", ids[0]), None).await["contact_state"], "replied");
    assert_ne!(c.leads("GET", &format!("/v1/leads/{}", ids[1]), None).await["contact_state"], "replied");
}

#[sqlx::test]
async fn the_company_stop_is_off_by_default(pool: PgPool) {
    let c = C::new(pool).await;
    let (_, a, _) = company_setup(&c, false).await;
    ingest_raw(&c.h.state, a, "p1", &fixture("human_plain.eml"), c.h.now()).await;
    let (stopped,): (i64,) = sqlx::query_as("SELECT count(*) FROM enrollments WHERE stop_reason = 'company_replied'").fetch_one(&c.h.state.pool).await.unwrap();
    assert_eq!(stopped, 0);
}

#[sqlx::test]
async fn free_mail_domains_are_not_one_company(pool: PgPool) {
    let c = C::new(pool).await;
    // the reply fixture comes from prospect.example: tell the settings that it is a free-mail domain
    c.leads("PATCH", "/v1/leads/settings", Some(json!({ "free_mail_domains_extra": ["prospect.example"] }))).await;
    let (camp, a, _) = company_setup(&c, true).await;
    ingest_raw(&c.h.state, a, "p1", &fixture("human_plain.eml"), c.h.now()).await;
    let (stopped,): (i64,) = sqlx::query_as("SELECT count(*) FROM enrollments WHERE stop_reason = 'company_replied'").fetch_one(&c.h.state.pool).await.unwrap();
    assert_eq!(stopped, 0, "a free-mail domain never triggers the company stop");
    assert_eq!(c.count_leads(&camp, "pending").await, 2);
}

#[sqlx::test]
async fn gmail_is_always_free_mail(pool: PgPool) {
    use aikido_email::leads::email::is_free_mail;
    for d in ["gmail.com", "outlook.com", "hotmail.com", "yahoo.com", "icloud.com", "proton.me"] {
        assert!(is_free_mail(d, &[]), "{d}");
    }
    let _ = pool;
}

#[sqlx::test]
async fn an_auto_reply_does_not_trigger_the_company_stop(pool: PgPool) {
    let c = C::new(pool).await;
    let (_, a, _) = company_setup(&c, true).await;
    ingest_raw(&c.h.state, a, "p1", &fixture("gmail_vacation.eml"), c.h.now()).await;
    let (stopped,): (i64,) = sqlx::query_as("SELECT count(*) FROM enrollments WHERE stop_reason = 'company_replied'").fetch_one(&c.h.state.pool).await.unwrap();
    assert_eq!(stopped, 0);
}

// =====================================================================================================
// Phase 9: stats, context, the campaign in imports and filters
// =====================================================================================================

#[sqlx::test]
async fn stats_count_sent_replied_and_the_reply_rate_by_step_and_account(pool: PgPool) {
    let c = C::new(pool).await;
    let (camp, a, _) = company_setup(&c, false).await;
    // before any reply
    let s = c.ok("GET", &format!("/v1/campaigns/{camp}/stats"), None).await;
    assert_eq!((s["sent"].as_i64(), s["leads_contacted"].as_i64(), s["replied"].as_i64(), s["reply_rate"].as_f64()), (Some(2), Some(2), Some(0), Some(0.0)));
    ingest_raw(&c.h.state, a, "p1", &fixture("human_plain.eml"), c.h.now()).await;
    let s = c.ok("GET", &format!("/v1/campaigns/{camp}/stats"), None).await;
    assert_eq!((s["sent"].as_i64(), s["replied"].as_i64(), s["bounced"].as_i64(), s["unsubscribed"].as_i64()), (Some(2), Some(1), Some(0), Some(0)));
    assert_eq!(s["reply_rate"].as_f64(), Some(0.5));
    assert_eq!(s["by_step"][0]["step_number"], 1);
    assert_eq!((s["by_step"][0]["sent"].as_i64(), s["by_step"][0]["replied"].as_i64()), (Some(2), Some(1)));
    assert_eq!(s["by_account"][0]["account_id"], a.to_string());
    assert_eq!(s["by_account"][0]["email"], ME_ACCOUNT);
    assert_eq!(s["by_account"][0]["sent"], 2);
    assert_eq!(c.api("GET", &format!("/v1/campaigns/{}/stats", Uuid::now_v7()), None).await.status, StatusCode::NOT_FOUND);
    // a campaign with no sends: zeros, no division by zero
    let empty = id(&c.campaign(&[a], json!({})).await);
    let s = c.ok("GET", &format!("/v1/campaigns/{empty}/stats"), None).await;
    assert_eq!((s["sent"].as_i64(), s["reply_rate"].as_f64(), s["by_step"].as_array().unwrap().len()), (Some(0), Some(0.0), 0));
}

#[sqlx::test]
async fn stats_count_unsubscribes_and_auto_replies(pool: PgPool) {
    let c = C::new(pool).await;
    let (camp, a, _) = company_setup(&c, false).await;
    ingest_raw(&c.h.state, a, "p1", &fixture("unsubscribe_short.eml"), c.h.now()).await;
    let s = c.ok("GET", &format!("/v1/campaigns/{camp}/stats"), None).await;
    assert_eq!((s["unsubscribed"].as_i64(), s["replied"].as_i64()), (Some(1), Some(0)));
    ingest_raw(&c.h.state, a, "p2", &fixture("gmail_vacation.eml"), c.h.now()).await;
    let s = c.ok("GET", &format!("/v1/campaigns/{camp}/stats"), None).await;
    assert_eq!(s["auto_replied"].as_i64(), Some(1));
}

#[sqlx::test]
async fn the_context_gathers_everything_about_a_lead(pool: PgPool) {
    let c = C::new(pool).await;
    c.leads("POST", "/v1/leads/fields", Some(json!({ "key": "pain_point", "label": "Pain point" }))).await;
    c.leads("POST", "/v1/leads/fields", Some(json!({ "key": "budget", "kind": "number" }))).await;
    let (camp, a, ids) = company_setup(&c, false).await;
    c.leads("PATCH", &format!("/v1/leads/{}", ids[0]), Some(json!({ "custom_fields": { "pain_point": "slow sales" }, "tags": ["vip"], "notes": "private" }))).await;
    ingest_raw(&c.h.state, a, "p1", &fixture("human_plain.eml"), c.h.now()).await;
    let r = c.leads("GET", &format!("/v1/leads/{}/context", ids[0]), None).await;
    assert_eq!(r["lead"]["email"], "jane@prospect.example");
    assert_eq!(r["lead"]["contact_state"], "replied");
    assert_eq!(r["lead"]["campaigns"][0]["id"], camp);
    assert_eq!(r["lead"]["campaigns"][0]["state"], "enrolled");
    let fields: Vec<(String, Option<String>)> = r["fields"].as_array().unwrap().iter().map(|f| (f["key"].as_str().unwrap().into(), f["value"].as_str().map(String::from))).collect();
    assert_eq!(fields, [("budget".to_string(), None), ("pain_point".to_string(), Some("slow sales".to_string()))]);
    assert_eq!(r["fields"][1]["label"], "Pain point");
    assert_eq!(r["fields"][0]["kind"], "number");
    let kinds: Vec<&str> = r["events"].as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds[0], "replied", "newest first");
    assert!(kinds.contains(&"replied") && kinds.contains(&"step_sent") && kinds.contains(&"enrolled") && kinds.contains(&"campaign_added") && kinds.contains(&"created"), "{kinds:?}");
    let msgs = r["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2, "{msgs:?}");
    assert_eq!((msgs[0]["direction"].as_str(), msgs[0]["step_number"].as_i64()), (Some("sent"), Some(1)));
    assert!(msgs[0]["subject"].as_str().unwrap().starts_with("Hello"));
    assert!(msgs[0]["text"].as_str().unwrap().contains("Step 1"));
    assert_eq!((msgs[1]["direction"].as_str(), msgs[1]["label"].as_str()), (Some("received"), Some("reply")));
    assert!(msgs[1]["text"].as_str().unwrap().len() > 3);
    // private notes are not template values and not in the messages; the lead object still has them
    assert_eq!(r["lead"]["notes"], "private");
    assert_eq!(c.api("GET", &format!("/v1/leads/{}/context", Uuid::now_v7()), None).await.status, StatusCode::FORBIDDEN, "scope leads only");
    assert_eq!(call(&c.h.state, "GET", &format!("/v1/leads/{}/context", Uuid::now_v7()), Some(&c.keys[&Scope::Leads]), None).await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn bulk_and_import_can_put_leads_in_a_campaign(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp = id(&c.campaign(&[a], json!({})).await);
    let r = c.leads("POST", "/v1/leads/bulk", Some(json!({ "campaign_id": camp, "leads": [{ "email": "a@x.com" }, { "email": "b@x.com" }, { "email": "bad" }] }))).await;
    assert_eq!(r["created"], 2);
    assert_eq!(c.count_leads(&camp, "pending").await, 2);
    // an import
    let leads_key = c.keys[&Scope::Leads].clone();
    let csv = "email,first_name\nc@x.com,C\nd@x.com,D\na@x.com,A\n";
    let resp = call_form(&c.h.state, "/v1/leads/imports", Some(&leads_key), &[("file", Some("l.csv"), csv.as_bytes()), ("campaign_id", None, camp.as_bytes())]).await;
    assert_eq!(resp.status, StatusCode::CREATED, "{}", resp.body);
    assert_eq!(resp.body["campaign_id"], camp);
    aikido_email::leads::import::tick(&c.h.state, &Default::default()).await.unwrap();
    assert_eq!(c.count_leads(&camp, "pending").await, 4, "a@x.com was in already");
    // the filter and the lead detail
    let list = c.leads("GET", &format!("/v1/leads?campaign_id={camp}&limit=500"), None).await;
    assert_eq!(list["leads"].as_array().unwrap().len(), 4);
    let other = c.leads("POST", "/v1/leads", Some(json!({ "email": "z@x.com" }))).await;
    assert!(!list["leads"].as_array().unwrap().iter().any(|l| l["id"] == other["id"]));
    let detail = c.leads("GET", &format!("/v1/leads/{}", list["leads"][0]["id"].as_str().unwrap()), None).await;
    assert_eq!(detail["campaigns"][0]["name"], "Q4 outreach");
    assert_eq!(detail["campaigns"][0]["state"], "pending");
    // add by the campaign filter into another campaign
    let second = id(&c.campaign(&[a], json!({ "name": "Second" })).await);
    let r = c.ok("POST", &format!("/v1/campaigns/{second}/leads"), Some(json!({ "filter": { "campaign_id": camp } }))).await;
    assert_eq!(r["added"], 4);
    // errors
    let bad = call_form(&c.h.state, "/v1/leads/imports", Some(&leads_key), &[("file", Some("l.csv"), csv.as_bytes()), ("campaign_id", None, Uuid::now_v7().to_string().as_bytes())]).await;
    assert_eq!(bad.status, StatusCode::NOT_FOUND);
    c.ok("POST", &format!("/v1/campaigns/{second}/launch"), None).await;
    c.ok("POST", &format!("/v1/campaigns/{second}/pause"), None).await;
    c.ok("POST", &format!("/v1/campaigns/{second}/archive"), None).await;
    let closed = call_form(&c.h.state, "/v1/leads/imports", Some(&leads_key), &[("file", Some("l.csv"), csv.as_bytes()), ("campaign_id", None, second.as_bytes())]).await;
    assert_eq!((closed.status, closed.body["error"]["code"].as_str()), (StatusCode::CONFLICT, Some("campaign_closed")));
    let r = call(&c.h.state, "POST", "/v1/leads/bulk", Some(&leads_key), Some(json!({ "campaign_id": second, "leads": [{ "email": "q@x.com" }] }))).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
}

#[sqlx::test]
async fn deleting_a_lead_takes_it_out_of_its_campaign(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp = id(&c.campaign(&[a], json!({ "daily_new_leads": 1 })).await);
    let ids = c.many_leads(3, "acme.com").await;
    c.add(&camp, &ids).await;
    c.launch(&camp).await;
    c.feed().await;
    // one enrolled, two pending: delete an enrolled and a pending lead
    let enrolled: Vec<(Uuid,)> = sqlx::query_as("SELECT lead_id FROM campaign_leads WHERE state = 'enrolled'").fetch_all(&c.h.state.pool).await.unwrap();
    c.leads("DELETE", &format!("/v1/leads/{}", enrolled[0].0), None).await;
    let pending: Vec<(Uuid,)> = sqlx::query_as("SELECT lead_id FROM campaign_leads WHERE state = 'pending' LIMIT 1").fetch_all(&c.h.state.pool).await.unwrap();
    c.leads("DELETE", &format!("/v1/leads/{}", pending[0].0), None).await;
    let d = c.get(&camp).await;
    assert_eq!((d["counts"]["enrolled"].as_i64(), d["counts"]["pending"].as_i64()), (Some(0), Some(1)));
    // and the stopped enrollment does not stop the campaign from going on
    assert_eq!(d["counts"]["enrollments"], json!({}), "the enrollment row stays, but it is not linked to the campaign any more");
}

// ---------- UI build map B7: the Leads tab rows ----------

#[sqlx::test]
async fn campaign_lead_rows_carry_the_follow_up_status(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp = id(&c.campaign(&[a], json!({ "daily_new_leads": 5000 })).await);
    let ids = c.many_leads(3, "acme.com").await;
    c.add(&camp, &ids).await;

    // not enrolled yet: the step fields are empty, the lead fields are there
    let rows = c.ok("GET", &format!("/v1/campaigns/{camp}/leads"), None).await;
    let first = &rows["leads"][0];
    assert_eq!(first["steps_total"], 2);
    assert!(first["current_step"].is_null() && first["next_due_at"].is_null() && first["wait_reason"].is_null());
    assert_eq!(first["contact_state"], "new");
    assert!(first["interest"].is_null());
    assert_eq!(first["first_name"], "N0");

    c.launch(&camp).await;
    c.round(Duration::minutes(1)).await;
    let rows = c.ok("GET", &format!("/v1/campaigns/{camp}/leads"), None).await;
    let rows = rows["leads"].as_array().unwrap();
    assert_eq!(rows.len(), 3);
    for r in rows {
        assert_eq!(r["state"], "enrolled");
        assert_eq!(r["steps_total"], 2);
        assert!(r["current_step"].as_i64().unwrap() >= 1);
        assert_eq!(r["contact_state"].as_str().map(|s| s == "in_sequence" || s == "contacted"), Some(true), "{r}");
    }
    // one lead sent step 1: now waiting for the delay of step 2
    let waiting = rows.iter().filter(|r| r["wait_reason"] == "waiting_delay").collect::<Vec<_>>();
    assert!(!waiting.is_empty(), "{rows:?}");
    assert_eq!(waiting[0]["current_step"], 2);
    assert!(waiting[0]["next_due_at"].is_string());

    // a stopped enrollment shows its stop reason
    let enrollment = rows[0]["enrollment_id"].as_str().unwrap().to_string();
    let stop = c.fapi("POST", &format!("/v1/followups/enrollments/{enrollment}/stop"), None).await;
    assert!(stop.status.is_success(), "{}", stop.body);
    let rows = c.ok("GET", &format!("/v1/campaigns/{camp}/leads"), None).await;
    let stopped = rows["leads"].as_array().unwrap().iter().find(|r| r["enrollment_id"] == enrollment.as_str()).unwrap();
    assert_eq!(stopped["wait_reason"], "manual");

    // a text with AI parts waiting for review shows as such
    sqlx::query("UPDATE enrollments SET status = 'active', stop_reason = NULL, next_due_at = now() - interval '1 minute' WHERE id = $1")
        .bind(enrollment.parse::<Uuid>().unwrap())
        .execute(&c.h.state.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO generations (enrollment_id, lead_id, sequence_id, step_number, step_hash, status)
         SELECT e.id, e.lead_id, e.sequence_id, e.next_step, 'h', 'pending_review' FROM enrollments e WHERE e.id = $1",
    )
    .bind(enrollment.parse::<Uuid>().unwrap())
    .execute(&c.h.state.pool)
    .await
    .unwrap();
    let rows = c.ok("GET", &format!("/v1/campaigns/{camp}/leads"), None).await;
    let waiting = rows["leads"].as_array().unwrap().iter().find(|r| r["enrollment_id"] == enrollment.as_str()).unwrap();
    assert_eq!(waiting["wait_reason"], "waiting_ai_review");
}

// ---------- UI update map v2: emails saved per lead (U2) ----------

impl C {
    async fn variants_on(&self, campaign: &Value, variants: Value) {
        let seq = campaign["sequence_id"].as_str().unwrap();
        let steps = campaign["steps"].clone();
        let steps: Vec<Value> = steps
            .as_array()
            .unwrap()
            .iter()
            .map(|s| json!({ "delay_amount": s["delay_amount"], "subject": s["subject"], "body_text": s["body_text"] }))
            .collect();
        let r = self.fapi("PUT", &format!("/v1/followups/sequences/{seq}/steps"), Some(json!({ "steps": steps, "variants": variants }))).await;
        assert!(r.status.is_success(), "{}", r.body);
    }

    async fn save(&self, camp: &str, overwrite: Option<&str>) -> Value {
        let body = overwrite.map(|o| json!({ "overwrite": o })).unwrap_or(json!({}));
        let r = self.api("POST", &format!("/v1/campaigns/{camp}/emails/render"), Some(body)).await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.body);
        r.body
    }

    async fn email_of(&self, camp: &str, lead: Uuid, step: i32) -> Value {
        self.ok("GET", &format!("/v1/campaigns/{camp}/leads/{lead}/emails/{step}"), None).await
    }
}

#[sqlx::test]
async fn save_splits_the_variants_evenly_and_keeps_hand_edits(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp_v = c.campaign(&[a], json!({})).await;
    let camp = id(&camp_v);
    let leads = c.many_leads(30, "acme.com").await;
    c.add(&camp, &leads).await;
    c.variants_on(
        &camp_v,
        json!([{ "name": "Short", "subject": "Quick one, {{first_name}}", "body_text": "B text for {{first_name}}. {{sender_name}}" },
               { "name": "Long", "subject": "Idea for {{first_name}}", "body_text": "C text for {{first_name}}" }]),
    )
    .await;
    let run = c.save(&camp, None).await;
    assert_eq!(run["status"], "done");
    assert_eq!(run["done"], 60, "30 leads x 2 steps");
    let counts: Vec<(String, i64)> = sqlx::query_as("SELECT variant_key, count(*) FROM lead_emails WHERE step_number = 1 GROUP BY 1 ORDER BY 1").fetch_all(&c.h.state.pool).await.unwrap();
    assert_eq!(counts, vec![("A".to_string(), 10), ("B".to_string(), 10), ("C".to_string(), 10)]);
    // leads added in one call share a time, so the order is the lead id: A, B, C, A, B, C ...
    let mut sorted = leads.clone();
    sorted.sort();
    for (i, l) in sorted.iter().enumerate().take(6) {
        assert_eq!(c.email_of(&camp, *l, 1).await["variant_key"], ["A", "B", "C"][i % 3]);
    }
    let b = c.email_of(&camp, sorted[1], 1).await;
    assert!(b["body_text"].as_str().unwrap().ends_with(". {{sender_name}}"), "the sender token waits for send time: {b}");
    assert_eq!(b["status"], "ready");

    // a hand edit survives a second Save, and `all` replaces it
    let p = c.ok("PATCH", &format!("/v1/campaigns/{camp}/leads/{}/emails/1", leads[0]), Some(json!({ "subject": "Mine", "body_text": "By hand" }))).await;
    assert_eq!(p["edited_by_hand"], true);
    let est = c.ok("POST", &format!("/v1/campaigns/{camp}/emails/estimate"), Some(json!({ "overwrite": "all" }))).await;
    assert_eq!(est["edited_by_hand"], 1);
    c.save(&camp, None).await;
    assert_eq!(c.email_of(&camp, leads[0], 1).await["subject"], "Mine");
    c.save(&camp, Some("all")).await;
    assert_eq!(c.email_of(&camp, leads[0], 1).await["subject"], "Hello N0");

    // the list shows a cell per step
    let rows = c.ok("GET", &format!("/v1/campaigns/{camp}/leads"), None).await;
    assert_eq!(rows["leads"][0]["emails"].as_array().unwrap().len(), 2);
}

#[sqlx::test]
async fn a_lead_edit_makes_the_email_out_of_date_and_the_sent_text_uses_the_new_value(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp = id(&c.campaign(&[a], json!({})).await);
    let leads = c.many_leads(2, "acme.com").await;
    c.add(&camp, &leads).await;
    c.save(&camp, None).await;
    assert_eq!(c.email_of(&camp, leads[0], 1).await["subject"], "Hello N0");
    c.leads("PATCH", &format!("/v1/leads/{}", leads[0]), Some(json!({ "first_name": "Zed" }))).await;
    assert_eq!(c.email_of(&camp, leads[0], 1).await["status"], "stale");
    assert_eq!(c.email_of(&camp, leads[1], 1).await["status"], "ready", "other leads are not touched");
    // a hand-edited stale email is sent as written; an unedited one is rendered again
    c.launch(&camp).await;
    for _ in 0..4 {
        c.round(Duration::minutes(10)).await;
    }
    let sent: Vec<(String,)> = sqlx::query_as("SELECT subject FROM send_jobs WHERE to_email = 'u0@acme.com' ORDER BY step_number").fetch_all(&c.h.state.pool).await.unwrap();
    assert_eq!(sent[0].0, "Hello Zed");
    let after = c.email_of(&camp, leads[0], 1).await;
    assert_eq!(after["status"], "sent");
    assert!(after["job_id"].is_string());
    assert_eq!(c.api("PATCH", &format!("/v1/campaigns/{camp}/leads/{}/emails/1", leads[0]), Some(json!({ "subject": "x" }))).await.status, StatusCode::CONFLICT);
}

#[sqlx::test]
async fn new_leads_get_their_emails_after_the_first_save_and_a_template_change_makes_them_stale(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp_v = c.campaign(&[a], json!({})).await;
    let camp = id(&camp_v);
    c.add(&camp, &c.many_leads(1, "acme.com").await).await;
    let late = c.many_leads(1, "beta.com").await;
    c.save(&camp, None).await;
    c.add(&camp, &late).await;
    assert_eq!(c.email_of(&camp, late[0], 2).await["status"], "ready");
    // a footer is added to every email, and changing it marks the saved ones out of date
    c.ok("PATCH", &format!("/v1/campaigns/{camp}"), Some(json!({ "footer": "Bye, {{sender_name}}" }))).await;
    assert_eq!(c.email_of(&camp, late[0], 1).await["status"], "stale");
    c.save(&camp, None).await;
    let e = c.email_of(&camp, late[0], 1).await;
    assert!(e["body_text"].as_str().unwrap().ends_with("Bye, {{sender_name}}"), "{e}");
    assert_eq!(e["status"], "ready");
}

#[sqlx::test]
async fn ai_mode_enrolls_only_leads_with_a_ready_primary_email_and_holds_a_step_without_one(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp = id(&c.campaign(&[a], json!({})).await);
    let leads = c.many_leads(1, "acme.com").await;
    c.add(&camp, &leads).await;
    c.ok("PATCH", &format!("/v1/campaigns/{camp}"), Some(json!({ "content_mode": "ai" }))).await;
    let r = c.api("POST", &format!("/v1/campaigns/{camp}/emails/render"), Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    c.launch(&camp).await;
    // no primary email: the lead stays pending
    for _ in 0..3 {
        c.round(Duration::minutes(10)).await;
    }
    assert_eq!(c.count_leads(&camp, "pending").await, 1);
    // an enrolled lead whose step has no email is held
    sqlx::query("INSERT INTO lead_emails (id, campaign_id, lead_id, step_number, source, status) VALUES (gen_random_uuid(), $1, $2, 1, 'manual', 'ready')")
        .bind(camp.parse::<Uuid>().unwrap())
        .bind(leads[0])
        .execute(&c.h.state.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE lead_emails SET subject = 'Hi', body_text = 'Hello'").execute(&c.h.state.pool).await.unwrap();
    for _ in 0..40 {
        c.round(Duration::hours(4)).await;
    }
    let (jobs,): (i64,) = sqlx::query_as("SELECT count(*) FROM send_jobs").fetch_one(&c.h.state.pool).await.unwrap();
    assert_eq!(jobs, 1, "the primary email was sent; the follow up has no email");
    let (reason,): (Option<String>,) = sqlx::query_as("SELECT wait_reason FROM enrollments LIMIT 1").fetch_one(&c.h.state.pool).await.unwrap();
    assert_eq!(reason.as_deref(), Some("email_not_ready"));
}

// ---------- UI update map v2: helpers that make cold email easy (U4) ----------

#[sqlx::test]
async fn the_domain_check_shows_green_orange_and_red_and_keeps_answers_for_an_hour(pool: PgPool) {
    use aikido_email::accounts::dns_check::{DnsChecker, fake::FakeTxt};
    let mut c = C::new(pool).await;
    let txt = FakeTxt::new();
    c.h.state.dns = Arc::new(DnsChecker::new(txt.clone()));
    let a = c.account("sales@acme-outreach.test").await;
    let get = |sel: &str| format!("/v1/accounts/{a}/dns-check{sel}");
    let acc = |uri: String| {
        let st = c.h.state.clone();
        let key = c.keys[&Scope::Accounts].clone();
        async move { call(&st, "GET", &uri, Some(&key), None).await }
    };

    // nothing published: SPF and DMARC are red, DKIM orange, MX green (the fake resolver has an MX)
    let r = acc(get("")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["spf"]["status"], "fail");
    assert!(r.body["spf"]["fix"].as_str().unwrap().contains("v=spf1"));
    assert_eq!(r.body["dkim"]["status"], "warn");
    assert_eq!(r.body["dmarc"]["status"], "fail");
    assert_eq!(r.body["mx"]["status"], "ok");
    assert_eq!(r.body["overall"], "fail");

    // publish the records: the old answer is kept for an hour
    txt.set("acme-outreach.test", &["v=spf1 include:_spf.google.com ~all"]);
    txt.set("_dmarc.acme-outreach.test", &["v=DMARC1; p=none"]);
    txt.set("google._domainkey.acme-outreach.test", &["v=DKIM1; k=rsa; p=ABC"]);
    assert_eq!(acc(get("")).await.body["overall"], "fail", "cached");
    c.h.advance(Duration::minutes(61));
    let r = acc(get("")).await;
    assert_eq!((r.body["spf"]["status"].as_str(), r.body["dkim"]["status"].as_str(), r.body["dmarc"]["status"].as_str()), (Some("ok"), Some("ok"), Some("ok")));
    assert_eq!(r.body["dkim"]["selector"], "google");
    assert_eq!(r.body["overall"], "ok");

    // a typed selector that has no key is red; a record that allows every server is orange
    txt.set("acme-outreach.test", &["v=spf1 +all"]);
    let r = acc(get("?selector=nope")).await;
    assert_eq!(r.body["dkim"]["status"], "ok", "another selector still has a key");
    assert_eq!(r.body["spf"]["status"], "warn");
    txt.set("acme-outreach.test", &["v=spf1 ~all", "v=spf1 -all"]);
    c.h.advance(Duration::minutes(61));
    assert_eq!(acc(get("")).await.body["spf"]["status"], "fail", "two SPF records");
}

#[sqlx::test]
async fn a_lead_missing_a_value_the_emails_need_is_found_and_listed(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let body = json!({ "name": "C", "account_ids": [a], "sequence": { "name": "S", "steps": [{ "subject": "Hi {{first_name|there}}", "body_text": "About {{company_name}} and {{pain}}" }] } });
    c.leads("POST", "/v1/leads/fields", Some(json!({ "key": "pain" }))).await;
    let camp = id(&c.ok("POST", "/v1/campaigns", Some(body)).await);
    let full = c.leads("POST", "/v1/leads", Some(json!({ "email": "full@x.com", "company_name": "X", "custom_fields": { "pain": "slow" } }))).await;
    let no_company = c.leads("POST", "/v1/leads", Some(json!({ "email": "nc@x.com", "custom_fields": { "pain": "slow" } }))).await;
    let no_pain = c.leads("POST", "/v1/leads", Some(json!({ "email": "np@x.com", "company_name": "Y" }))).await;
    c.add(&camp, &[full["id"].as_str().unwrap().parse().unwrap(), no_company["id"].as_str().unwrap().parse().unwrap(), no_pain["id"].as_str().unwrap().parse().unwrap()]).await;
    let all = c.ok("GET", &format!("/v1/campaigns/{camp}/leads"), None).await;
    assert_eq!(all["leads"].as_array().unwrap().len(), 3);
    let only = c.ok("GET", &format!("/v1/campaigns/{camp}/leads?missing=true"), None).await;
    let rows = only["leads"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    let mut missing: Vec<(String, Vec<String>)> = rows.iter().map(|r| (r["email"].as_str().unwrap().to_string(), r["missing"].as_array().unwrap().iter().map(|m| m.as_str().unwrap().to_string()).collect())).collect();
    missing.sort();
    assert_eq!(missing, vec![("nc@x.com".to_string(), vec!["company_name".to_string()]), ("np@x.com".to_string(), vec!["pain".to_string()])]);
    // the checklist counts them
    let list = c.ok("GET", &format!("/v1/campaigns/{camp}/checklist"), None).await;
    let values = list["items"].as_array().unwrap().iter().find(|i| i["key"] == "values").unwrap();
    assert_eq!(values["ok"], false);
    // a fallback on the field covers the lead
    c.leads("PATCH", "/v1/leads/fields/pain", Some(json!({ "fallback": "something" }))).await;
    assert_eq!(c.ok("GET", &format!("/v1/campaigns/{camp}/leads?missing=true"), None).await["leads"].as_array().unwrap().len(), 1);
}

#[sqlx::test]
async fn the_checklist_counts_what_is_ready_and_a_test_email_ticks_the_last_item(pool: PgPool) {
    use aikido_email::accounts::dns_check::{DnsChecker, fake::FakeTxt};
    let mut c = C::new(pool).await;
    let txt = FakeTxt::new();
    c.h.state.dns = Arc::new(DnsChecker::new(txt.clone()));
    let a = c.account("sales@acme-outreach.test").await;
    let camp = id(&c.campaign(&[a], json!({})).await);
    let list = c.ok("GET", &format!("/v1/campaigns/{camp}/checklist"), None).await;
    assert_eq!(list["total"], 6);
    let ok = |l: &Value, k: &str| l["items"].as_array().unwrap().iter().find(|i| i["key"] == k).unwrap()["ok"].as_bool().unwrap();
    assert!(ok(&list, "accounts") && !ok(&list, "domains") && !ok(&list, "leads") && !ok(&list, "emails") && ok(&list, "values") && !ok(&list, "test"));
    txt.set("acme-outreach.test", &["v=spf1 ~all"]);
    txt.set("_dmarc.acme-outreach.test", &["v=DMARC1; p=none"]);
    c.h.advance(Duration::minutes(61));
    let leads = c.many_leads(2, "acme.com").await;
    c.add(&camp, &leads).await;
    let list = c.ok("GET", &format!("/v1/campaigns/{camp}/checklist"), None).await;
    assert!(ok(&list, "domains") && ok(&list, "leads") && !ok(&list, "emails"));
    // Save writes the emails of day 1
    c.save(&camp, None).await;
    assert!(ok(&c.ok("GET", &format!("/v1/campaigns/{camp}/checklist"), None).await, "emails"));
    // a test email goes out through the engine and ticks the last item
    let t = c.ok("POST", &format!("/v1/campaigns/{camp}/emails/send-test"), Some(json!({ "to": "me@example.com" }))).await;
    assert!(t["job_id"].is_string());
    assert!(!ok(&c.ok("GET", &format!("/v1/campaigns/{camp}/checklist"), None).await, "test"), "queued is not sent");
    for _ in 0..30 {
        c.h.tick().await;
        c.h.advance(Duration::minutes(10));
    }
    let list = c.ok("GET", &format!("/v1/campaigns/{camp}/checklist"), None).await;
    assert!(ok(&list, "test"), "{list}");
    assert_eq!(list["ready"], 6);
    let (subject, count): (String, i64) = sqlx::query_as("SELECT subject, (SELECT count(*) FROM send_jobs WHERE campaign_id IS NOT NULL) FROM send_jobs WHERE to_email = 'me@example.com'").fetch_one(&c.h.state.pool).await.unwrap();
    assert!(subject.starts_with("[Test] Hello "));
    assert_eq!(count, 0, "a test job is not counted in the campaign");
}

#[sqlx::test]
async fn duplicate_copies_the_sequence_prompts_variants_and_settings_but_no_leads(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp_v = c.campaign(&[a], json!({ "daily_new_leads": 7, "stop_company_on_reply": true })).await;
    let camp = id(&camp_v);
    c.variants_on(&camp_v, json!([{ "name": "Short", "subject": "B subject", "body_text": "B text" }])).await;
    c.ok("PATCH", &format!("/v1/campaigns/{camp}"), Some(json!({ "footer": "Bye", "content_mode": "ai" }))).await;
    c.add(&camp, &c.many_leads(2, "acme.com").await).await;
    let copy = c.ok("POST", &format!("/v1/campaigns/{camp}/duplicate"), None).await;
    assert_ne!(copy["id"], camp_v["id"]);
    assert_eq!(copy["name"], "Q4 outreach (copy)");
    assert_eq!(copy["status"], "draft");
    assert_eq!((copy["daily_new_leads"].as_i64(), copy["stop_company_on_reply"].as_bool()), (Some(7), Some(true)));
    assert_eq!((copy["footer"].as_str(), copy["content_mode"].as_str()), (Some("Bye"), Some("ai")));
    assert_eq!(copy["steps"].as_array().unwrap().len(), 2);
    assert_eq!(copy["variants"][0]["subject"], "B subject");
    assert_eq!(copy["accounts"].as_array().unwrap().len(), 1);
    assert_eq!(copy["counts"]["pending"], 0);
    assert_ne!(copy["sequence_id"], camp_v["sequence_id"]);
    assert_eq!(c.count_leads(copy["id"].as_str().unwrap(), "pending").await, 0);
}

#[sqlx::test]
async fn reply_rate_by_variant_and_turning_a_variant_off(pool: PgPool) {
    let c = C::new(pool).await;
    let a = c.account("a@x.test").await;
    let camp_v = c.campaign(&[a], json!({})).await;
    let camp = id(&camp_v);
    c.variants_on(&camp_v, json!([{ "name": "B", "subject": "B subject", "body_text": "B text" }])).await;
    let leads = c.many_leads(4, "acme.com").await;
    c.add(&camp, &leads).await;
    c.save(&camp, None).await;
    c.launch(&camp).await;
    for _ in 0..12 {
        c.round(Duration::minutes(10)).await;
    }
    // one lead of variant B replied
    sqlx::query("UPDATE send_jobs SET replied_at = now() WHERE id = (SELECT job_id FROM lead_emails WHERE variant_key = 'B' AND step_number = 1 LIMIT 1)").execute(&c.h.state.pool).await.unwrap();
    let s = c.ok("GET", &format!("/v1/campaigns/{camp}/stats"), None).await;
    let by: Vec<(String, i64, i64)> = s["by_variant"].as_array().unwrap().iter().map(|v| (v["variant_key"].as_str().unwrap().into(), v["sent"].as_i64().unwrap(), v["replied"].as_i64().unwrap())).collect();
    assert_eq!(by, vec![("A".to_string(), 2, 0), ("B".to_string(), 2, 1)]);
    assert_eq!(s["by_variant"][1]["reply_rate"], 0.5);

    // turn A off: new leads all get B. At least one variant must stay on.
    let seq = camp_v["sequence_id"].as_str().unwrap();
    let steps = json!(camp_v["steps"].as_array().unwrap().iter().map(|s| json!({ "delay_amount": s["delay_amount"], "subject": s["subject"], "body_text": s["body_text"] })).collect::<Vec<_>>());
    let r = c.fapi("PUT", &format!("/v1/followups/sequences/{seq}/steps"), Some(json!({ "steps": steps, "variant_a_active": false }))).await;
    assert!(r.status.is_success(), "{}", r.body);
    let late = c.many_leads(2, "beta.com").await;
    c.add(&camp, &late).await;
    c.save(&camp, None).await;
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM lead_emails WHERE step_number = 1 AND variant_key = 'A' AND lead_id = ANY($1)").bind(&late).fetch_one(&c.h.state.pool).await.unwrap();
    assert_eq!(n, 0);
    let off = c.fapi("PUT", &format!("/v1/followups/sequences/{seq}/steps"), Some(json!({ "steps": steps, "variants": [{ "name": "B", "subject": "B subject", "body_text": "B text", "active": false }], "variant_a_active": false }))).await;
    assert_eq!(off.status, StatusCode::UNPROCESSABLE_ENTITY);
}
