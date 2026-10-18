//! Email optimization: the new lead fields, the Optimised mode (one call writes the primary email and 3 follow-ups),
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
    seq: String,
}

const GOOD: &str = r#"{"primary":{"subject":"new depot","body":"Hi Ana, one"},"followup_1":{"body":"two"},"followup_2":{"body":"three"},"followup_3":{"body":"four"}}"#;

impl A {
    /// A campaign with one template step, switched to the Optimised mode.
    async fn new(pool: PgPool) -> Self {
        let g = G::new(pool).await;
        let steps = json!([{ "delay_amount": 0, "subject": "Hi", "body_text": "x" }]);
        let r = g.ok(Scope::Campaigns, "POST", "/v1/campaigns", Some(json!({ "name": "Opt", "account_ids": [g.account], "sequence": { "name": "S", "steps": steps } }))).await;
        let camp = r["id"].as_str().unwrap().to_string();
        let seq = r["sequence_id"].as_str().unwrap().to_string();
        let a = Self { g, camp, seq };
        a.cok("PATCH", &format!("/v1/campaigns/{}", a.camp), Some(json!({ "content_mode": "optimized" }))).await;
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
}

// ---------- the Optimised mode ----------

#[sqlx::test]
async fn switching_to_optimised_makes_four_steps_and_the_other_modes_stay_as_they_are(pool: PgPool) {
    let a = A::new(pool).await;
    let c = a.cok("GET", &format!("/v1/campaigns/{}", a.camp), None).await;
    assert_eq!(c["content_mode"], "optimized");
    let steps = c["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 4);
    assert_eq!(steps.iter().map(|s| s["delay_amount"].as_i64().unwrap()).collect::<Vec<_>>(), [0, 3, 4, 5]);
    assert_eq!(steps[0]["body_text"], "x", "the existing step is kept");
    // back to a template: the steps the mode made by itself are taken away again
    a.cok("PATCH", &format!("/v1/campaigns/{}", a.camp), Some(json!({ "content_mode": "template" }))).await;
    let c = a.cok("GET", &format!("/v1/campaigns/{}", a.camp), None).await;
    assert_eq!(c["steps"].as_array().unwrap().len(), 1);
    assert_eq!(c["steps"][0]["body_text"], "x");
    a.cok("PATCH", &format!("/v1/campaigns/{}", a.camp), Some(json!({ "content_mode": "ai" }))).await;
    a.cok("PATCH", &format!("/v1/campaigns/{}", a.camp), Some(json!({ "content_mode": "optimized" }))).await;
    assert_eq!(a.cok("GET", &format!("/v1/campaigns/{}", a.camp), None).await["steps"].as_array().unwrap().len(), 4);
    // more than 4 steps: refused with a plain message
    let b = a.g.ok(Scope::Campaigns, "POST", "/v1/campaigns", Some(json!({ "name": "Five", "account_ids": [a.g.account], "sequence": { "name": "S5", "steps": [
        { "delay_amount": 0, "subject": "a", "body_text": "x" }, { "delay_amount": 1, "body_text": "x" }, { "delay_amount": 1, "body_text": "x" },
        { "delay_amount": 1, "body_text": "x" }, { "delay_amount": 1, "body_text": "x" }] } }))).await;
    let r = a.capi("PATCH", &format!("/v1/campaigns/{}", b["id"].as_str().unwrap()), Some(json!({ "content_mode": "optimized" }))).await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("too_many_steps")));
    let _ = &a.seq;
}

#[sqlx::test]
async fn one_call_writes_the_primary_email_and_three_follow_ups_into_their_own_places(pool: PgPool) {
    let a = A::new(pool).await;
    let lead = a.add_lead("ana@acme.example", json!({ "first_name": "Ana", "last_name": "Silva", "company_name": "Acme", "industry": "Logistics", "trigger_event": "Opened a depot", "trigger_verified": true })).await;
    let est = a.cok("POST", &format!("/v1/campaigns/{}/emails/estimate", a.camp), Some(json!({}))).await;
    assert_eq!(est["emails"], 1, "one call per lead");
    let run = a.generate(json!({})).await;
    assert_eq!(run["total"], 1);
    let t = a.g.gen_tick().await;
    assert_eq!((t.emails_written, t.emails_failed), (1, 0));
    assert_eq!(a.g.fake.call_count(), 1, "one model call for the four emails");
    let bodies: Vec<String> = {
        let mut v = Vec::new();
        for s in 1..=4 {
            let e = a.email(lead, s).await;
            assert_eq!((e["status"].as_str(), e["source"].as_str()), (Some("ready"), Some("ai")), "step {s}");
            v.push(e["body_text"].as_str().unwrap().to_string());
        }
        v
    };
    assert!(bodies[0].starts_with("Hi there, fake primary email"));
    assert!(bodies[1].starts_with("Fake follow up 1") && bodies[2].starts_with("Fake follow up 2") && bodies[3].starts_with("Fake follow up 3"));
    assert_eq!(a.email(lead, 1).await["subject"], "fake subject");
    for s in 2..=4 {
        assert_eq!(a.email(lead, s).await["subject"], "", "follow-ups go in the same thread");
    }
    // the prompt is plain text with the lead values, no JSON, and no footer is added
    let call = &a.g.fake.calls()[0];
    assert!(call.user.contains("Company: Acme") && call.user.contains("Industry: Logistics") && call.user.contains("Recent event: Opened a depot"), "{}", call.user);
    assert!(!call.user.contains('{'));
    assert!(call.system.contains("followup_3"));
    assert!(!bodies[0].contains("Footer"));
    // the run is done
    let r = a.cok("GET", &format!("/v1/campaigns/{}/email-runs/{}", a.camp, run["id"].as_str().unwrap()), None).await;
    assert_eq!((r["status"].as_str(), r["done"].as_i64()), (Some("done"), Some(1)));
}

#[sqlx::test]
async fn own_prompts_the_saved_profile_a_hand_edit_and_a_bad_format(pool: PgPool) {
    let a = A::new(pool).await;
    let lead = a.add_lead("ana@acme.example", json!({ "first_name": "Ana", "company_name": "Acme" })).await;
    // the profile fills the sender part of the user prompt
    a.pok("PUT", "/v1/personalization/profile", Some(json!({ "fields": { "sender_first_name": "Dana", "offer": "Route software", "outcome": "Faster deliveries" } }))).await;
    // own prompts are kept apart from the AI mode prompts
    a.g.ok(Scope::Followups, "PATCH", &format!("/v1/followups/sequences/{}", a.seq), Some(json!({ "ai_system_prompt": "AI mode text, not used here." }))).await;
    let r = a.g.ok(Scope::Followups, "PATCH", &format!("/v1/followups/sequences/{}", a.seq), Some(json!({
        "opt_system_prompt": "Be very short for {{sender_first_name}}.",
        "opt_user_prompt": "Lead: {{first_name}} at {{company}}. I sell: {{offer}}. Ask: {{low_friction_offer|a short call}}."
    }))).await;
    assert_eq!(r["sequence"]["opt_system_prompt"], "Be very short for {{sender_first_name}}.");

    // a hand edit of follow-up 2 is kept when the set is written again
    a.generate(json!({})).await;
    a.g.gen_tick().await;
    let call = &a.g.fake.calls()[0];
    assert!(call.system.starts_with("Be very short for Dana."), "{}", call.system);
    assert_eq!(call.user, "Lead: Ana at Acme. I sell: Route software. Ask: a short call.");
    let e = a.email(lead, 3).await;
    a.cok("PATCH", &format!("/v1/campaigns/{}/leads/{lead}/emails/3", a.camp), Some(json!({ "subject": "", "body_text": "My own words" }))).await;
    assert_eq!(e["status"], "ready");
    a.cok("POST", &format!("/v1/campaigns/{}/emails/actions", a.camp), Some(json!({ "action": "regenerate", "ids": [a.email(lead, 1).await["id"]] }))).await;
    a.g.gen_tick().await;
    assert_eq!(a.email(lead, 3).await["body_text"], "My own words", "a hand edit is never replaced");
    assert!(a.email(lead, 2).await["body_text"].as_str().unwrap().starts_with("Fake follow up 1"));

    // a new profile version marks the unsent emails out of date
    a.pok("PUT", "/v1/personalization/profile", Some(json!({ "fields": { "sender_first_name": "Dana", "offer": "Route software v2" } }))).await;
    assert_eq!(a.email(lead, 2).await["status"], "stale");

    // a wrong format: one more try, then the email fails
    let l2 = a.add_lead("bob@acme.example", json!({ "first_name": "Bob", "company_name": "Acme" })).await;
    a.generate(json!({ "leads": { "lead_ids": [l2] } })).await;
    let before = a.g.fake.call_count();
    a.g.fake.push(Ok(response("sorry, no")));
    a.g.fake.push(Ok(response(GOOD)));
    let t = a.g.gen_tick().await;
    assert_eq!((t.emails_written, a.g.fake.call_count() - before), (1, 2));
    assert_eq!(a.email(l2, 4).await["body_text"], "four");
    let l3 = a.add_lead("cy@acme.example", json!({ "first_name": "Cy", "company_name": "Acme" })).await;
    a.generate(json!({ "leads": { "lead_ids": [l3] } })).await;
    a.g.fake.push(Ok(response("no")));
    a.g.fake.push(Ok(response("still no")));
    let t = a.g.gen_tick().await;
    assert_eq!(t.emails_failed, 1);
    assert_eq!(a.email(l3, 1).await["status"], "failed");
}

#[sqlx::test]
async fn the_test_route_shows_all_four_and_saves_nothing(pool: PgPool) {
    let a = A::new(pool).await;
    let lead = a.add_lead("ana@acme.example", json!({ "first_name": "Ana", "company_name": "Acme" })).await;
    let r = a.cok("POST", &format!("/v1/campaigns/{}/emails/test", a.camp), Some(json!({ "lead_id": lead, "step": 1 }))).await;
    assert_eq!(r["mode"], "optimized");
    assert_eq!(r["emails"].as_array().unwrap().len(), 4);
    assert_eq!(r["subject"], "fake subject");
    assert_eq!(r["emails"][2]["step"], 3);
    let saved = a.capi("GET", &format!("/v1/campaigns/{}/leads/{lead}/emails/1", a.camp), None).await;
    assert_eq!(saved.status, StatusCode::NOT_FOUND);
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
