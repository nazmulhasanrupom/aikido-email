//! Email optimization: the new lead fields, the default prompt blocks, the optimized AI flow with checks and HOLD,
//! the PERSONALIZATION sources and the sender profile. `FakeLlm` answers every call. No real provider, no real network.
mod common;
use aikido_email::auth::api_keys::Scope;
use aikido_email::generator::llm::fake::response;
use axum::http::StatusCode;
use common::gharness::*;
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

struct A {
    g: G,
    camp: String,
}

fn good(step: i32, angle: &str) -> Value {
    json!({
        "status": "OK", "subject": if step == 1 || step >= 4 { "short subject" } else { "" },
        "body": "Hi Ana, the new depot caught my eye. Routing gets slow at that size. Worth a look?\nDana",
        "angle_used": angle, "used_trigger": false, "claims_check": [], "hold_reason": ""
    })
}

impl A {
    /// An AI mode campaign with four steps and no prompt text: it starts with the optimized prompt.
    async fn new(pool: PgPool) -> Self {
        let g = G::new(pool).await;
        let steps = json!([
            { "delay_amount": 0, "subject": "Hi", "body_text": "x" },
            { "delay_amount": 3, "subject": "", "body_text": "y" },
            { "delay_amount": 4, "subject": "", "body_text": "z" },
            { "delay_amount": 5, "subject": "", "body_text": "w" }
        ]);
        let r = g.ok(Scope::Campaigns, "POST", "/v1/campaigns", Some(json!({ "name": "Opt", "account_ids": [g.account], "sequence": { "name": "S", "steps": steps } }))).await;
        let camp = r["id"].as_str().unwrap().to_string();
        let a = Self { g, camp };
        a.cok("PATCH", &format!("/v1/campaigns/{}", a.camp), Some(json!({ "content_mode": "ai" }))).await;
        a
    }

    async fn capi(&self, method: &str, uri: &str, body: Option<Value>) -> Response {
        self.g.api(Scope::Campaigns, method, uri, body).await
    }

    async fn cok(&self, method: &str, uri: &str, body: Option<Value>) -> Value {
        self.g.ok(Scope::Campaigns, method, uri, body).await
    }

    async fn pok(&self, method: &str, uri: &str, body: Option<Value>) -> Value {
        self.g.ok(Scope::Personalization, method, uri, body).await
    }

    async fn add_lead(&self, email: &str, extra: Value) -> Uuid {
        let mut body = json!({ "email": email });
        for (k, v) in extra.as_object().unwrap() {
            body[k] = v.clone();
        }
        let r = self.g.ok(Scope::Leads, "POST", "/v1/leads", Some(body)).await;
        let id: Uuid = r["id"].as_str().unwrap().parse().unwrap();
        self.cok("POST", &format!("/v1/campaigns/{}/leads", self.camp), Some(json!({ "lead_ids": [id] }))).await;
        id
    }

    async fn generate(&self, body: Value) -> Value {
        let r = self.capi("POST", &format!("/v1/campaigns/{}/emails/generate", self.camp), Some(body)).await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.body);
        r.body
    }

    async fn email(&self, lead: Uuid, step: i32) -> Value {
        self.cok("GET", &format!("/v1/campaigns/{}/leads/{lead}/emails/{step}", self.camp), None).await
    }

    async fn action(&self, action: &str, ids: Value) -> Value {
        self.cok("POST", &format!("/v1/campaigns/{}/emails/actions", self.camp), Some(json!({ "action": action, "ids": ids }))).await
    }

    async fn email_id(&self, lead: Uuid, step: i32) -> String {
        self.email(lead, step).await["id"].as_str().unwrap().to_string()
    }
}

// ---------- the default prompt blocks ----------

#[sqlx::test]
async fn a_new_ai_campaign_starts_optimized_and_a_block_can_be_edited_and_reset(pool: PgPool) {
    let a = A::new(pool).await;
    let d = a.cok("GET", "/v1/prompts/defaults", None).await;
    assert_eq!(d["version"], 1);
    for (key, mark) in [("core", "# OUTPUT"), ("primary", "under 80 words"), ("followup", "under 60 words")] {
        assert!(d[key].as_str().unwrap().contains(mark), "{key}");
    }
    let p = a.cok("GET", &format!("/v1/campaigns/{}/prompts", a.camp), None).await;
    assert_eq!(p["kind"], "optimized");
    assert_eq!(p["version"], 1);
    assert_eq!(p["core"]["text"], d["core"]);
    assert_eq!(p["newer_default"], false);
    assert!(p["warnings"].as_array().unwrap().contains(&json!("no_profile")), "{p}");
    assert_eq!(p["delays"].as_array().unwrap().len(), 4);
    assert_eq!(p["delays"][1]["delay_amount"], 3);

    // edits change this campaign only
    let put = a.cok("PUT", &format!("/v1/campaigns/{}/prompts", a.camp), Some(json!({ "primary": "# STEP RULES\nMy own rule." }))).await;
    assert_eq!(put["primary"]["text"], "# STEP RULES\nMy own rule.");
    assert_eq!(a.cok("GET", "/v1/prompts/defaults", None).await["primary"], d["primary"]);
    // a core without # OUTPUT gets a warning
    let put = a.cok("PUT", &format!("/v1/campaigns/{}/prompts", a.camp), Some(json!({ "core": "# ROLE\nshort" }))).await;
    assert!(put["warnings"].as_array().unwrap().contains(&json!("core_missing_sections")));
    // reset
    let r = a.cok("POST", &format!("/v1/campaigns/{}/prompts/reset", a.camp), Some(json!({ "block": "core" }))).await;
    assert_eq!(r["core"]["is_default"], true);
    assert_eq!(r["core"]["text"], d["core"]);
    let bad = a.capi("POST", &format!("/v1/campaigns/{}/prompts/reset", a.camp), Some(json!({ "block": "x" }))).await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    let long = a.capi("PUT", &format!("/v1/campaigns/{}/prompts", a.camp), Some(json!({ "core": "x".repeat(16001) }))).await;
    assert_eq!(long.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test]
async fn an_old_campaign_with_prompts_stays_custom_and_can_switch(pool: PgPool) {
    let g = G::new(pool).await;
    let steps = json!([{ "delay_amount": 0, "subject": "Hi", "body_text": "x", "ai_user_prompt": "Write to {{first_name}}." }]);
    let r = g.ok(Scope::Campaigns, "POST", "/v1/campaigns", Some(json!({ "name": "Old", "account_ids": [g.account], "sequence": { "name": "S", "steps": steps } }))).await;
    let id = r["id"].as_str().unwrap();
    g.ok(Scope::Campaigns, "PATCH", &format!("/v1/campaigns/{id}"), Some(json!({ "content_mode": "ai" }))).await;
    let p = g.ok(Scope::Campaigns, "GET", &format!("/v1/campaigns/{id}/prompts"), None).await;
    assert_eq!(p["kind"], "custom");
    // the preview refuses a custom campaign
    let lead = g.lead("a@acme.example", "Ana").await;
    let pre = g.api(Scope::Campaigns, "POST", &format!("/v1/campaigns/{id}/preview"), Some(json!({ "lead_id": lead, "step": 1 }))).await;
    assert_eq!(pre.status, StatusCode::UNPROCESSABLE_ENTITY);
    // switch with one call: the shipped blocks are copied in
    let p = g.ok(Scope::Campaigns, "PUT", &format!("/v1/campaigns/{id}/prompts"), Some(json!({ "kind": "optimized" }))).await;
    assert_eq!((p["kind"].as_str(), p["version"].as_i64()), (Some("optimized"), Some(1)));
    assert_eq!(p["primary"]["is_default"], false);
    // and back
    let p = g.ok(Scope::Campaigns, "PUT", &format!("/v1/campaigns/{id}/prompts"), Some(json!({ "kind": "custom" }))).await;
    assert_eq!(p["kind"], "custom");
}

// ---------- the optimized flow ----------

#[sqlx::test]
async fn four_steps_follow_the_subject_rules_and_each_step_sees_the_earlier_angles(pool: PgPool) {
    let a = A::new(pool).await;
    let lead = a.add_lead("ana@acme.example", json!({ "first_name": "Ana", "company_name": "Acme" })).await;
    a.generate(json!({})).await;
    let t = a.g.gen_tick().await;
    assert_eq!((t.emails_written, t.emails_held, t.emails_failed), (4, 0, 0));
    for (step, subject) in [(1, "fake subject"), (2, ""), (3, ""), (4, "fake subject")] {
        let e = a.email(lead, step).await;
        assert_eq!(e["status"], "ready", "step {step}: {e}");
        assert_eq!(e["subject"], subject, "step {step}");
        assert_eq!(e["angle_used"], format!("fake angle {step}"));
        assert!(e["body_text"].as_str().unwrap().starts_with("Hi there, fake mail body"));
    }
    let calls = a.g.fake.calls();
    assert_eq!(calls.len(), 4);
    let third = calls.iter().find(|c| c.system.contains("\"step\":3")).unwrap();
    let lead_block: Value = serde_json::from_str(third.user.split("LEAD\n").nth(1).unwrap().split("\n\nWrite").next().unwrap()).unwrap();
    assert_eq!(lead_block["previous_angles"], json!(["fake angle 1", "fake angle 2"]));
    assert_eq!(lead_block["sequence_step"], 3);
    assert_eq!(lead_block["company"], "Acme");
    assert!(third.system.contains("FOLLOW-UP MAIL") && !third.system.contains("PRIMARY MAIL"));
    assert!(calls.iter().find(|c| c.system.contains("\"step\":1")).unwrap().system.contains("PRIMARY MAIL"));
    // the user prompt has no private data
    assert!(!third.user.contains("ana@acme.example"));
}

#[sqlx::test]
async fn an_empty_required_field_holds_the_mail_with_no_model_call_and_a_retry_works(pool: PgPool) {
    let a = A::new(pool).await;
    let lead = a.add_lead("bob@acme.example", json!({ "first_name": "Bob" })).await;
    a.generate(json!({ "steps": [1] })).await;
    let t = a.g.gen_tick().await;
    assert_eq!((t.emails_written, t.emails_held), (0, 1));
    assert_eq!(a.g.fake.call_count(), 0);
    let e = a.email(lead, 1).await;
    assert_eq!((e["status"].as_str(), e["hold_reason"].as_str()), (Some("held"), Some("missing_required: company")));
    // the Held filter
    let held = a.cok("GET", &format!("/v1/campaigns/{}/leads?held=true", a.camp), None).await;
    assert_eq!(held["leads"].as_array().unwrap().len(), 1);
    assert_eq!(held["leads"][0]["emails"][0]["hold_reason"], "missing_required: company");
    let all = a.cok("GET", &format!("/v1/campaigns/{}/leads", a.camp), None).await;
    assert_eq!(all["leads"].as_array().unwrap().len(), 1);
    // edit the lead and retry
    a.g.ok(Scope::Leads, "PATCH", &format!("/v1/leads/{lead}"), Some(json!({ "company_name": "Acme" }))).await;
    assert_eq!(a.action("regenerate", json!([a.email_id(lead, 1).await])).await["changed"], 1);
    let t = a.g.gen_tick().await;
    assert_eq!((t.emails_written, t.emails_held), (1, 0));
    let e = a.email(lead, 1).await;
    assert_eq!(e["status"], "ready");
    assert!(e["hold_reason"].is_null());
    assert_eq!(a.cok("GET", &format!("/v1/campaigns/{}/leads?held=true", a.camp), None).await["leads"].as_array().unwrap().len(), 0);
}

#[sqlx::test]
async fn a_model_hold_is_saved_a_failed_check_is_tried_once_more_and_then_held(pool: PgPool) {
    let a = A::new(pool).await;
    let l1 = a.add_lead("one@acme.example", json!({ "first_name": "One", "company_name": "Acme" })).await;
    a.generate(json!({ "steps": [1] })).await;
    a.g.fake.push(Ok(response(json!({ "status": "HOLD", "subject": "", "body": "", "angle_used": "", "used_trigger": false, "claims_check": [], "hold_reason": "sender_incomplete: offer is empty" }).to_string())));
    a.g.gen_tick().await;
    let e = a.email(l1, 1).await;
    assert_eq!((e["status"].as_str(), e["hold_reason"].as_str()), (Some("held"), Some("sender_incomplete: offer is empty")));
    assert_eq!(a.g.fake.call_count(), 1, "a model HOLD is not tried again");

    // two answers with a dash: held after 2 calls, with the failure list
    let l2 = a.add_lead("two@acme.example", json!({ "first_name": "Two", "company_name": "Acme" })).await;
    a.generate(json!({ "steps": [1], "leads": { "lead_ids": [l2] } })).await;
    let mut bad = good(1, "x");
    bad["body"] = json!("Hi \u{2014} routing is slow.");
    a.g.fake.push(Ok(response(bad.to_string())));
    a.g.fake.push(Ok(response(bad.to_string())));
    a.g.gen_tick().await;
    assert_eq!(a.g.fake.call_count(), 3);
    let e = a.email(l2, 1).await;
    assert_eq!(e["status"], "held");
    assert!(e["hold_reason"].as_str().unwrap().starts_with("checks_failed: "), "{e}");
    assert!(e["check_failures"][0].as_str().unwrap().contains("dash"));
    assert!(e["body_text"].is_null(), "a held mail has no text to send");

    // one bad answer and then a good one: written after 2 calls
    let l3 = a.add_lead("three@acme.example", json!({ "first_name": "Three", "company_name": "Acme" })).await;
    a.generate(json!({ "steps": [1], "leads": { "lead_ids": [l3] } })).await;
    a.g.fake.push(Ok(response(bad.to_string())));
    a.g.fake.push(Ok(response(good(1, "depot growth").to_string())));
    a.g.gen_tick().await;
    assert_eq!(a.g.fake.call_count(), 5);
    let e = a.email(l3, 1).await;
    assert_eq!((e["status"].as_str(), e["angle_used"].as_str()), (Some("ready"), Some("depot growth")));
    assert!(a.g.fake.calls()[4].user.contains("Your last answer failed these checks"));
}

#[sqlx::test]
async fn a_held_step_holds_the_steps_after_it_and_skip_stops_the_lead(pool: PgPool) {
    let a = A::new(pool).await;
    let lead = a.add_lead("ana@acme.example", json!({ "first_name": "Ana", "company_name": "Acme" })).await;
    a.generate(json!({ "steps": [1, 2] })).await;
    a.g.fake.push(Ok(response(json!({ "status": "HOLD", "subject": "", "body": "", "angle_used": "", "used_trigger": false, "claims_check": [], "hold_reason": "no hook" }).to_string())));
    let t = a.g.gen_tick().await;
    assert_eq!(t.emails_held, 2);
    assert_eq!(a.g.fake.call_count(), 1, "step 2 made no call");
    assert_eq!(a.email(lead, 2).await["hold_reason"], "previous_step_held");

    // skip: the lead leaves the campaign
    let ids = json!([a.email_id(lead, 1).await]);
    assert_eq!(a.action("skip", ids).await["changed"], 1);
    let row = a.cok("GET", &format!("/v1/campaigns/{}/leads?state=skipped", a.camp), None).await;
    assert_eq!(row["leads"][0]["skip_reason"], "held_skipped");
}

#[sqlx::test]
async fn the_saved_profile_goes_into_the_prompt_and_a_new_version_marks_mails_stale(pool: PgPool) {
    let a = A::new(pool).await;
    let lead = a.add_lead("ana@acme.example", json!({ "first_name": "Ana", "company_name": "Acme", "industry": "Logistics", "trigger_event": "Opened a depot", "trigger_verified": true })).await;
    let fields = json!({ "sender_first_name": "Dana", "ideal_customer": "Ops heads", "problem_solved": "Slow routing", "offer": "Route software", "outcome": "Faster deliveries" });
    let saved = a.pok("PUT", "/v1/personalization/profile", Some(json!({ "fields": fields }))).await;
    assert_eq!(saved["version"], 1);
    a.generate(json!({ "steps": [1] })).await;
    a.g.gen_tick().await;
    let call = &a.g.fake.calls()[0];
    let sender: Value = serde_json::from_str(call.user.split("SENDER\n").nth(1).unwrap().split("\n\nLEAD").next().unwrap()).unwrap();
    assert_eq!((sender["offer"].as_str(), sender["proof_or_none"].as_str(), sender["allow_low_personalization"].as_bool()), (Some("Route software"), Some("none"), Some(false)));
    let lead_block: Value = serde_json::from_str(call.user.split("LEAD\n").nth(1).unwrap().split("\n\nWrite").next().unwrap()).unwrap();
    assert_eq!((lead_block["trigger"].as_str(), lead_block["trigger_verified"].as_bool(), lead_block["industry"].as_str()), (Some("Opened a depot"), Some(true), Some("Logistics")));
    assert_eq!(a.email(lead, 1).await["status"], "ready");

    // a new version marks the unsent mail out of date
    let mut f2 = fields.clone();
    f2["offer"] = json!("Route software v2");
    let v2 = a.pok("PUT", "/v1/personalization/profile", Some(json!({ "fields": f2 }))).await;
    assert_eq!(v2["version"], 2);
    assert_eq!(a.email(lead, 1).await["status"], "stale");
    let versions = a.pok("GET", "/v1/personalization/profile/versions", None).await;
    assert_eq!(versions.as_array().unwrap().len(), 2);
    assert_eq!(versions[0]["version"], 2);
    assert_eq!(a.pok("GET", "/v1/personalization/profile", None).await["profile"]["offer"], "Route software v2");
}

#[sqlx::test]
async fn preview_shows_the_prompts_the_result_and_every_check_and_is_rate_limited(pool: PgPool) {
    let a = A::new(pool).await;
    let lead = a.add_lead("ana@acme.example", json!({ "first_name": "Ana", "company_name": "Acme" })).await;
    let uri = format!("/v1/campaigns/{}/preview", a.camp);
    a.g.fake.push(Ok(response(good(1, "depot growth").to_string())));
    let r = a.cok("POST", &uri, Some(json!({ "lead_id": lead, "step": 1 }))).await;
    assert_eq!(r["status"], "OK");
    assert_eq!(r["angle_used"], "depot growth");
    assert!(r["word_count"].as_u64().unwrap() > 5);
    assert_eq!(r["checks"].as_array().unwrap().len(), 8);
    assert!(r["checks"].as_array().unwrap().iter().all(|c| c["passed"] == true));
    assert!(r["system"].as_str().unwrap().contains("PRIMARY MAIL") && r["user"].as_str().unwrap().contains("SENDER"));
    assert_eq!(r["calls"], 1);
    // dry run: the prompts only, no model call, not counted by the limit
    let calls_before = a.g.fake.call_count();
    let d = a.cok("POST", &uri, Some(json!({ "lead_id": lead, "step": 1, "dry_run": true }))).await;
    assert_eq!(d["status"], "PROMPTS_ONLY");
    assert!(d["user"].as_str().unwrap().contains("\"company\": \"Acme\""));
    assert_eq!(a.g.fake.call_count(), calls_before);
    // step 2 with earlier angles typed in
    let r = a.cok("POST", &uri, Some(json!({ "lead_id": lead, "step": 2, "previous_angles": ["fake angle 1"] }))).await;
    assert_eq!(r["previous_angles"], json!(["fake angle 1"]));
    // a held result shows the checks of the last answer
    let mut bad = good(1, "x");
    bad["body"] = json!("see https://acme.example now");
    a.g.fake.push(Ok(response(bad.to_string())));
    a.g.fake.push(Ok(response(bad.to_string())));
    let r = a.cok("POST", &uri, Some(json!({ "lead_id": lead, "step": 1 }))).await;
    assert_eq!(r["status"], "HOLD");
    assert!(r["held"]["reason"].as_str().unwrap().starts_with("checks_failed"));
    assert!(r["checks"].as_array().unwrap().iter().any(|c| c["name"] == "links" && c["passed"] == false));
    let none = a.capi("POST", &uri, Some(json!({ "lead_id": lead, "step": 9 }))).await;
    assert_eq!(none.status, StatusCode::NOT_FOUND);
    // calls are logged
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM llm_calls WHERE purpose = 'lead_email_test'").fetch_one(&a.g.h.state.pool).await.unwrap();
    assert_eq!(n, 4);
    // the limit: 10 a minute (4 calls so far counted as 4 requests: 3 ok + 1 not found)
    let mut last = StatusCode::OK;
    for _ in 0..10 {
        last = a.capi("POST", &uri, Some(json!({ "lead_id": lead, "step": 1 }))).await.status;
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
}

// ---------- lead fields ----------

#[sqlx::test]
async fn the_new_lead_columns_import_edit_and_export(pool: PgPool) {
    let g = G::new(pool).await;
    let csv = "Email,First Name,Company,Industry,Trigger,Trigger Verified,Icebreaker\n\
               a@acme.example,Ana,Acme,Logistics,Opened a depot,YES,Loves routing\n\
               b@acme.example,Ben,Acme,,,maybe,\n\
               c@acme.example,Cy,Acme,Retail,Hired a CTO,n,\n";
    let pv = call_form(&g.h.state, "/v1/leads/imports/preview", Some(&g.keys[&Scope::Leads]), &[("file", Some("a.csv"), csv.as_bytes())]).await;
    assert_eq!(pv.status, StatusCode::OK, "{}", pv.body);
    let text = pv.body.to_string();
    for t in ["industry", "trigger_event", "trigger_verified", "personalization_note"] {
        assert!(text.contains(t), "mapping guess has {t}: {text}");
    }
    let r = call_form(&g.h.state, "/v1/leads/imports", Some(&g.keys[&Scope::Leads]), &[("file", Some("a.csv"), csv.as_bytes())]).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    let id = r.body["id"].as_str().unwrap().to_string();
    aikido_email::leads::import::tick(&g.h.state, &aikido_email::leads::import::RunOpts::default()).await.unwrap();
    let imp = g.ok(Scope::Leads, "GET", &format!("/v1/leads/imports/{id}"), None).await;
    assert_eq!((imp["created"].as_i64(), imp["invalid"].as_i64()), (Some(2), Some(1)), "{imp}");
    assert!(imp["errors"][0]["reason"].as_str().unwrap().contains("trigger_verified"), "{imp}");
    let ana = &g.ok(Scope::Leads, "GET", "/v1/leads?q=a%40acme.example&limit=1", None).await["leads"][0];
    assert_eq!((ana["industry"].as_str(), ana["trigger_event"].as_str(), ana["trigger_verified"].as_bool(), ana["personalization_note"].as_str()), (Some("Logistics"), Some("Opened a depot"), Some(true), Some("Loves routing")));
    let cy = &g.ok(Scope::Leads, "GET", "/v1/leads?q=c%40acme.example&limit=1", None).await["leads"][0];
    assert_eq!(cy["trigger_verified"], false);

    // edit
    let p = g.ok(Scope::Leads, "PATCH", &format!("/v1/leads/{}", cy["id"].as_str().unwrap()), Some(json!({ "trigger_verified": true, "industry": "Retail chains" }))).await;
    assert_eq!((p["trigger_verified"].as_bool(), p["industry"].as_str()), (Some(true), Some("Retail chains")));
    let bad = g.api(Scope::Leads, "PATCH", &format!("/v1/leads/{}", cy["id"].as_str().unwrap()), Some(json!({ "trigger_verified": "yes" }))).await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    // create with the new columns
    let c = g.ok(Scope::Leads, "POST", "/v1/leads", Some(json!({ "email": "d@acme.example", "trigger_event": "Raised funding", "trigger_verified": true }))).await;
    assert_eq!((c["trigger_event"].as_str(), c["trigger_verified"].as_bool()), (Some("Raised funding"), Some(true)));
    // a custom field can not take a reserved name
    let reserved = g.api(Scope::Leads, "POST", "/v1/leads/fields", Some(json!({ "key": "industry" }))).await;
    assert_eq!(reserved.status, StatusCode::UNPROCESSABLE_ENTITY);

    // export
    let (status, _, bytes) = call_raw(&g.h.state, "/v1/leads/export.csv", Some(&g.keys[&Scope::Leads])).await;
    assert_eq!(status, StatusCode::OK);
    let csv = String::from_utf8(bytes).unwrap();
    let header = csv.lines().next().unwrap();
    assert!(header.contains("industry") && header.contains("trigger_event") && header.contains("trigger_verified") && header.contains("personalization_note"), "{header}");
    assert!(csv.contains("Opened a depot") && csv.contains("yes"));
}

#[sqlx::test]
async fn the_field_registry_lists_the_optimized_lead_fields(pool: PgPool) {
    let g = G::new(pool).await;
    let f = g.ok(Scope::Leads, "GET", "/v1/fields", None).await;
    let all = f.as_array().unwrap();
    let lead_opt: Vec<&str> = all.iter().filter(|x| x["group"] == "lead" && x["optimized"] == true).map(|x| x["key"].as_str().unwrap()).collect();
    assert_eq!(lead_opt, ["first_name", "company", "title", "industry", "website", "personalization_note", "trigger", "trigger_verified"]);
    let company = all.iter().find(|x| x["key"] == "company").unwrap();
    assert_eq!((company["column"].as_str(), company["required"].as_bool()), (Some("company_name"), Some(true)));
    assert_eq!(all.iter().filter(|x| x["group"] == "sender").count(), 9);
    assert_eq!(all.iter().find(|x| x["key"] == "proof_or_none").unwrap()["empty"], "none");
}

// ---------- PERSONALIZATION ----------

#[sqlx::test]
async fn sources_are_cleaned_listed_deleted_and_the_cv_file_is_not_kept(pool: PgPool) {
    let g = G::new(pool).await;
    let key = g.keys[&Scope::Personalization].clone();
    let p = |m: &'static str, u: &str, b: Option<Value>| {
        let (state, key, u) = (g.h.state.clone(), key.clone(), u.to_string());
        async move { call(&state, m, &u, Some(&key), b).await }
    };
    let r = p("POST", "/v1/personalization/sources", Some(json!({ "kind": "text", "text": "  I help dentists   get patients.\n\n\nI help dentists   get patients.\nPaid search only. " }))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body[0]["tag"], "text");
    assert_eq!(r.body[0]["clean_text"], "I help dentists get patients.\nPaid search only.");
    let r = p("POST", "/v1/personalization/sources", Some(json!({ "kind": "detail", "details": [{ "key": "Clients", "value": "Smile Co" }, { "key": "", "value": "dropped" }] }))).await;
    assert_eq!(r.body.as_array().unwrap().len(), 1);
    assert_eq!(r.body[0]["tag"], "detail:Clients");
    let r = p("POST", "/v1/personalization/sources", Some(json!({ "kind": "guide", "guide": { "who_you_help": "Dental groups", "real_proof": "Grew Smile Co" } }))).await;
    assert_eq!(r.body[0]["tag"], "guide");
    assert!(r.body[0]["clean_text"].as_str().unwrap().contains("Who I help: Dental groups"));
    // limits
    let r = p("POST", "/v1/personalization/sources", Some(json!({ "kind": "text", "text": "x".repeat(10_001) }))).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let rows: Vec<Value> = (0..21).map(|i| json!({ "key": format!("k{i}"), "value": "v" })).collect();
    assert_eq!(p("POST", "/v1/personalization/sources", Some(json!({ "kind": "detail", "details": rows }))).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    // a private address is refused before any connection
    for url in ["http://127.0.0.1/", "http://169.254.169.254/latest/meta-data/", "http://localhost/", "ftp://example.com/", "http://10.0.0.1:8080/"] {
        let r = p("POST", "/v1/personalization/sources", Some(json!({ "kind": "url", "url": url }))).await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{url}: {}", r.body);
    }
    // a CV: only the text is kept
    let up = |name: &'static str, data: &'static [u8]| {
        let (state, key) = (g.h.state.clone(), key.clone());
        async move { call_form(&state, "/v1/personalization/sources/cv", Some(&key), &[("file", Some(name), data)]).await }
    };
    let r = up("cv.txt", b"Ten years running paid search for dental groups. Led a team of six.").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!((r.body["tag"].as_str(), r.body["origin"].as_str()), (Some("cv"), Some("cv.txt")));
    let r = up("cv.txt", b"   \n  ").await;
    assert_eq!((r.status, r.body["error"]["message"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("No text found in this file.")), "{}", r.body);
    assert_eq!(up("cv.exe", b"MZ").await.status, StatusCode::UNPROCESSABLE_ENTITY);
    // no table holds file bytes
    let (cols,): (i64,) = sqlx::query_as("SELECT count(*) FROM information_schema.columns WHERE table_name = 'personalization_sources' AND data_type = 'bytea'").fetch_one(&g.h.state.pool).await.unwrap();
    assert_eq!(cols, 0);

    let list = p("GET", "/v1/personalization/sources", None).await.body;
    assert_eq!(list.as_array().unwrap().len(), 4);
    let id = list[0]["id"].as_str().unwrap().to_string();
    assert_eq!(p("DELETE", &format!("/v1/personalization/sources/{id}"), None).await.status, StatusCode::NO_CONTENT);
    assert_eq!(p("DELETE", &format!("/v1/personalization/sources/{id}"), None).await.status, StatusCode::NOT_FOUND);
    assert_eq!(p("GET", "/v1/personalization/sources", None).await.body.as_array().unwrap().len(), 3);
}

#[sqlx::test]
async fn the_ai_drafts_a_profile_nothing_saves_by_itself_and_delete_all_removes_everything(pool: PgPool) {
    let g = G::new(pool).await;
    let ok = |m: &'static str, u: &'static str, b: Option<Value>| g.ok(Scope::Personalization, m, u, b);
    // (the closure borrows `g`, which also serves the other calls below)
    // no sources yet
    let r = g.api(Scope::Personalization, "POST", "/v1/personalization/profile/generate", None).await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("no_sources")), "{}", r.body);
    ok("POST", "/v1/personalization/sources", Some(json!({ "kind": "text", "text": "I am Dana. I run paid search for dental groups and grew Smile Co." }))).await;
    let d = ok("POST", "/v1/personalization/profile/generate", None).await;
    assert_eq!(d["draft"]["fields"]["sender_first_name"], "Dana");
    assert_eq!(d["draft"]["fields"]["offer"], "Fake offer.");
    assert_eq!(d["draft"]["fields"]["allow_low_personalization"], false);
    assert_eq!(d["sources"], json!(["text"]));
    assert_eq!(d["draft"]["sources_used"]["offer"], json!(["text"]));
    // the model saw the source, and the call is logged with its own purpose
    assert!(g.fake.calls()[0].user.contains("<source tag=\"text\">"));
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM llm_calls WHERE purpose = 'sender_profile'").fetch_one(&g.h.state.pool).await.unwrap();
    assert_eq!(n, 1);
    // nothing was saved
    assert!(ok("GET", "/v1/personalization/profile", None).await["profile"].is_null());
    // save: a person sets the switch
    let mut fields = d["draft"]["fields"].clone();
    fields["allow_low_personalization"] = json!(true);
    let saved = ok("PUT", "/v1/personalization/profile", Some(json!({ "fields": fields, "sources_used": d["draft"]["sources_used"] }))).await;
    assert_eq!((saved["version"].as_i64(), saved["allow_low_personalization"].as_bool()), (Some(1), Some(true)));
    let long = g.api(Scope::Personalization, "PUT", "/v1/personalization/profile", Some(json!({ "fields": { "offer": "x".repeat(1001) } }))).await;
    assert_eq!(long.status, StatusCode::UNPROCESSABLE_ENTITY);
    // delete all
    let r = g.api(Scope::Personalization, "DELETE", "/v1/personalization", None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert!(ok("GET", "/v1/personalization/profile", None).await["profile"].is_null());
    assert_eq!(ok("GET", "/v1/personalization/sources", None).await.as_array().unwrap().len(), 0);
}

#[sqlx::test]
async fn the_personalization_scope_is_separate(pool: PgPool) {
    let g = G::new(pool).await;
    let r = g.api(Scope::Leads, "GET", "/v1/personalization/sources", None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = g.api(Scope::Personalization, "GET", "/v1/fields", None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(g.api(Scope::Admin, "GET", "/v1/personalization/sources", None).await.status, StatusCode::OK);
}
