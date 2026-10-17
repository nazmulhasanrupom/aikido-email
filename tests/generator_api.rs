//! Email Generator API: step rules, preview, the sequence writer, the generation routes, usage, status, launch checks.
mod common;
use aikido_email::auth::api_keys::Scope;
use aikido_email::generator::llm::LlmError;
use aikido_email::generator::llm::fake::{response, slots_response};
use axum::http::StatusCode;
use chrono::Duration;
use common::gharness::*;
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

fn err_code(r: &Response) -> &str {
    r.body["error"]["code"].as_str().unwrap_or_default()
}

// =====================================================================================================
// Template syntax and step rules
// =====================================================================================================

#[sqlx::test]
async fn an_ai_slot_parses_with_or_without_an_inline_fallback_and_the_step_gets_a_hash(pool: PgPool) {
    let g = G::new(pool).await;
    let r = g
        .api(
            Scope::Followups,
            "POST",
            "/v1/followups/sequences",
            Some(json!({ "name": "S", "brief": "  We sell widgets.  ", "steps": [
                ai_step(0, "Hi {{ai:subj|there}}", "A {{ai:opener}} and {{ai:tail|bye}}", json!([
                    { "key": "subj", "instruction": "A word." },
                    { "key": "opener", "instruction": "A line.", "max_words": 20, "allow_links": true, "fallback": "Hello" },
                    { "key": "tail", "instruction": "A P.S." }
                ])),
                { "delay_amount": 2, "subject": "", "body_text": "Plain step" }
            ] })),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["sequence"]["brief"], "We sell widgets.");
    let slots = r.body["steps"][0]["ai_slots"].as_array().unwrap();
    assert_eq!(slots.len(), 3);
    assert_eq!(slots[1], json!({ "key": "opener", "instruction": "A line.", "max_words": 20, "allow_links": true, "fallback": "Hello" }));
    assert_eq!(slots[0]["max_words"], 40, "the default");
    assert_eq!(r.body["steps"][1]["ai_slots"], json!([]), "an old style step has no slots");
    let hashes: Vec<(Option<String>,)> = sqlx::query_as("SELECT step_hash FROM sequence_steps ORDER BY step_number").fetch_all(&g.h.state.pool).await.unwrap();
    assert!(hashes.iter().all(|h| h.0.as_deref().is_some_and(|h| h.len() == 64)));
    assert_ne!(hashes[0], hashes[1]);
}

#[sqlx::test]
async fn bad_ai_slots_get_422(pool: PgPool) {
    let g = G::new(pool).await;
    let body = |subject: &str, text: &str, slots: Value| json!({ "name": "S", "steps": [ai_step(0, "Hi", "x", json!([])), ai_step(1, subject, text, slots)] });
    let post = |b: Value| async { g.api(Scope::Followups, "POST", "/v1/followups/sequences", Some(b)).await };
    let op = || json!([{ "key": "opener", "instruction": "A line." }]);

    // a slot with no entry
    let r = post(body("", "Hi {{ai:nope}}", op())).await;
    assert_eq!((r.status, err_code(&r)), (StatusCode::UNPROCESSABLE_ENTITY, "unknown_ai_slot"));
    assert!(r.body["error"]["message"].as_str().unwrap().contains("Step 2"));
    // an entry that is not used
    let r = post(body("", "Hi", op())).await;
    assert_eq!(err_code(&r), "unused_ai_slot");
    // a slot in the subject of step 2 breaks the thread
    let r = post(body("Hello {{ai:opener}}", "Hi {{ai:opener}}", op())).await;
    assert_eq!((r.status, err_code(&r)), (StatusCode::UNPROCESSABLE_ENTITY, "ai_slot_in_threaded_subject"));
    // but step 1 may have one
    let ok = g.api(Scope::Followups, "POST", "/v1/followups/sequences", Some(json!({ "name": "S", "steps": [ai_step(0, "Hello {{ai:opener}}", "x", op())] }))).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    // bad entries
    for slots in [
        json!([{ "key": "1x", "instruction": "A" }]),
        json!([{ "key": "a b", "instruction": "A" }]),
        json!([{ "key": "opener", "instruction": "" }]),
        json!([{ "key": "opener", "instruction": "A", "max_words": 0 }]),
        json!([{ "key": "opener", "instruction": "A", "max_words": 500 }]),
        json!([{ "key": "opener", "instruction": "A" }, { "key": "opener", "instruction": "B" }]),
        json!([{ "key": "opener", "instruction": "A", "fallback": "x".repeat(501) }]),
    ] {
        let r = post(body("", "Hi {{ai:opener}}", slots.clone())).await;
        assert_eq!((r.status, err_code(&r)), (StatusCode::UNPROCESSABLE_ENTITY, "invalid_ai_slots"), "{slots}");
    }
    // a bad slot syntax
    let r = post(body("", "Hi {{ai:}}", op())).await;
    assert_eq!(err_code(&r), "invalid_template");
    // PUT steps follows the same rules
    let seq = g.sequence(json!([step1()])).await;
    let r = g.put_steps(seq, json!([ai_step(0, "Hi", "{{ai:other}}", opener())])).await;
    assert_eq!(err_code(&r), "unknown_ai_slot");
    // the brief
    let r = g.api(Scope::Followups, "PATCH", &format!("/v1/followups/sequences/{seq}"), Some(json!({ "brief": "x".repeat(4001) }))).await;
    assert_eq!(err_code(&r), "invalid_brief");
    let r = g.api(Scope::Followups, "PATCH", &format!("/v1/followups/sequences/{seq}"), Some(json!({ "brief": "Sell widgets" }))).await;
    assert_eq!(r.body["sequence"]["brief"], "Sell widgets");
    let r = g.api(Scope::Followups, "PATCH", &format!("/v1/followups/sequences/{seq}"), Some(json!({ "brief": " " }))).await;
    assert_eq!(r.body["sequence"]["brief"], Value::Null, "an empty brief clears it");
    let r = g.api(Scope::Followups, "PATCH", &format!("/v1/followups/sequences/{seq}"), Some(json!({ "name": "Renamed" }))).await;
    assert_eq!(r.body["sequence"]["brief"], Value::Null);
}

#[sqlx::test]
async fn enrolling_does_not_need_a_value_for_an_ai_slot_and_the_followup_preview_shows_a_marker(pool: PgPool) {
    let g = G::new(pool).await;
    let seq = g.sequence(json!([step1(), ai_step(1, "", "Again {{ai:opener}}", opener())])).await;
    let (_, _) = g.enroll_new(seq, "ana@acme.example").await; // 200, not 422 missing_variable
    let r = g.api(Scope::Followups, "POST", &format!("/v1/followups/sequences/{seq}/preview"), Some(json!({ "variables": { "first_name": "Ana" } }))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["steps"][0]["body_text"], "Hi Ana,\n[AI: opener]\nBye");
    assert_eq!(r.body["steps"][1]["body_text"], "Again [AI: opener]");
    assert_eq!(g.fake.call_count(), 0, "a marker costs no tokens");
    // generate = true asks the model for one lead
    let lead = g.lead("bob@acme.example", "Bob").await;
    let r = g.api(Scope::Followups, "POST", &format!("/v1/followups/sequences/{seq}/preview"), Some(json!({ "variables": { "first_name": "Bob" }, "generate": true, "lead_id": lead }))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["steps"][0]["body_text"], "Hi Bob,\nFake text for opener.\nBye");
    assert_eq!(g.fake.call_count(), 2, "one call per step with slots");
    let r = g.api(Scope::Followups, "POST", &format!("/v1/followups/sequences/{seq}/preview"), Some(json!({ "generate": true }))).await;
    assert_eq!(err_code(&r), "lead_required");
    let purposes: Vec<(String,)> = sqlx::query_as("SELECT DISTINCT purpose FROM llm_calls").fetch_all(&g.h.state.pool).await.unwrap();
    assert_eq!(purposes, vec![("preview".to_string(),)]);
}

#[sqlx::test]
async fn a_field_used_in_the_instruction_of_a_slot_can_not_be_deleted(pool: PgPool) {
    let g = G::new(pool).await;
    g.ok(Scope::Leads, "POST", "/v1/leads/fields", Some(json!({ "key": "industry" }))).await;
    g.ok(Scope::Leads, "POST", "/v1/leads/fields", Some(json!({ "key": "spare" }))).await;
    let slots = json!([{ "key": "opener", "instruction": "Write about their work in {{industry|their field}}." }]);
    let seq = g.sequence(json!([ai_step(0, "Hi", "{{ai:opener}}", slots)])).await;
    g.enroll_new(seq, "ana@acme.example").await;
    let r = g.api(Scope::Leads, "DELETE", "/v1/leads/fields/industry", None).await;
    assert_eq!((r.status, err_code(&r)), (StatusCode::CONFLICT, "field_in_use"));
    assert!(r.body["error"]["message"].as_str().unwrap().contains("AI sequence"));
    assert_eq!(g.api(Scope::Leads, "DELETE", "/v1/leads/fields/spare", None).await.status, StatusCode::OK);
}

// =====================================================================================================
// Campaign launch
// =====================================================================================================

#[sqlx::test]
async fn a_campaign_with_ai_slots_needs_a_working_slot_provider_to_launch(pool: PgPool) {
    let g = G::new(pool).await;
    let r = g.ok(Scope::Campaigns, "POST", "/v1/campaigns", Some(json!({ "name": "AI", "sequence": { "name": "S", "steps": [step1()] }, "account_ids": [g.account], "daily_new_leads": 100 }))).await;
    let cid = r["id"].as_str().unwrap().to_string();
    let ids: Vec<Value> = {
        let leads: Vec<Value> = (0..5).map(|i| json!({ "email": format!("c{i}@acme.example") })).collect();
        let bulk = g.ok(Scope::Leads, "POST", "/v1/leads/bulk", Some(json!({ "leads": leads }))).await;
        bulk["items"].as_array().unwrap().iter().map(|i| i["lead_id"].clone()).collect()
    };
    g.ok(Scope::Campaigns, "POST", &format!("/v1/campaigns/{cid}/leads"), Some(json!({ "lead_ids": ids }))).await;

    // no provider
    g.settings(json!({ "slot_provider_id": null })).await;
    let r = g.api(Scope::Campaigns, "POST", &format!("/v1/campaigns/{cid}/launch"), None).await;
    assert_eq!((r.status, err_code(&r)), (StatusCode::UNPROCESSABLE_ENTITY, "generator_not_ready"));
    // a provider whose key was refused
    g.settings(json!({ "slot_provider_id": g.provider })).await;
    sqlx::query("UPDATE llm_providers SET status = 'auth_failed'").execute(&g.h.state.pool).await.unwrap();
    let r = g.api(Scope::Campaigns, "POST", &format!("/v1/campaigns/{cid}/launch"), None).await;
    assert_eq!(err_code(&r), "generator_not_ready");
    assert!(r.body["error"]["message"].as_str().unwrap().contains("refused"));
    sqlx::query("UPDATE llm_providers SET status = 'ok'").execute(&g.h.state.pool).await.unwrap();
    let launched = g.ok(Scope::Campaigns, "POST", &format!("/v1/campaigns/{cid}/launch"), None).await;
    assert_eq!(launched["status"], "active");
    // the capacity warning adds a token estimate: 5 waiting leads x 1 step x (1500 + 400)
    let w: Vec<&str> = launched["warnings"].as_array().unwrap().iter().filter_map(|w| w.as_str()).collect();
    assert!(w.iter().any(|w| w.starts_with("AI slots: about 9500 tokens for the 5 waiting")), "{w:?}");

    // a campaign without slots does not care
    g.settings(json!({ "slot_provider_id": null })).await;
    let plain = g.ok(Scope::Campaigns, "POST", "/v1/campaigns", Some(json!({ "name": "Plain", "sequence": { "name": "P", "steps": [{ "subject": "Hi", "body_text": "x" }] }, "account_ids": [g.account] }))).await;
    let pid = plain["id"].as_str().unwrap();
    let lead = g.lead("plain@acme.example", "P").await;
    g.ok(Scope::Campaigns, "POST", &format!("/v1/campaigns/{pid}/leads"), Some(json!({ "lead_ids": [lead] }))).await;
    let l = g.ok(Scope::Campaigns, "POST", &format!("/v1/campaigns/{pid}/launch"), None).await;
    assert!(!l["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().starts_with("AI slots")));
}

// =====================================================================================================
// Preview
// =====================================================================================================

#[sqlx::test]
async fn the_preview_shows_the_full_email_and_the_tokens_and_saves_nothing_but_a_call_row(pool: PgPool) {
    let g = G::new(pool).await;
    let seq = g.sequence(json!([step1(), ai_step(1, "", "Back again.\n{{ai:opener}}", opener())])).await;
    let lead = g.lead("ana@acme.example", "Ana").await;
    let body = |step: i32| json!({ "sequence_id": seq, "step_number": step, "lead_id": lead, "account_id": g.account });
    g.fake.push(Ok(slots_response(&[("opener", "Congrats on the launch.")])));
    let r = g.gapi("POST", "/v1/generator/preview", Some(body(1))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["subject"], "Hello Ana");
    assert_eq!(r.body["body_text"], "Hi Ana,\nCongrats on the launch.\nBye");
    assert_eq!(r.body["slots"], json!({ "opener": "Congrats on the launch." }));
    assert_eq!((r.body["input_tokens"].clone(), r.body["output_tokens"].clone(), r.body["model"].clone()), (json!(100), json!(20), json!("fake-model")));
    // step 2 keeps the thread subject
    let r = g.gapi("POST", "/v1/generator/preview", Some(body(2))).await;
    assert_eq!(r.body["subject"], "Re: Hello Ana");
    assert_eq!(count(&g.h.state, "SELECT count(*) FROM generations").await, 0);
    let rows: Vec<(String,)> = sqlx::query_as("SELECT purpose FROM llm_calls").fetch_all(&g.h.state.pool).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.0 == "preview"));
}

#[sqlx::test]
async fn the_preview_reports_every_kind_of_problem(pool: PgPool) {
    let g = G::new(pool).await;
    let seq = g.sequence(json!([step1()])).await;
    let plain = g.sequence(json!([{ "subject": "Hi", "body_text": "plain" }])).await;
    let lead = g.lead("ana@acme.example", "Ana").await;
    let b = |seq: Uuid, step: i32, lead: Uuid| json!({ "sequence_id": seq, "step_number": step, "lead_id": lead });
    let r = g.gapi("POST", "/v1/generator/preview", Some(b(plain, 1, lead))).await;
    assert_eq!(err_code(&r), "no_ai_slots");
    assert_eq!(err_code(&g.gapi("POST", "/v1/generator/preview", Some(b(seq, 5, lead))).await), "step_not_found");
    assert_eq!(err_code(&g.gapi("POST", "/v1/generator/preview", Some(b(Uuid::now_v7(), 1, lead))).await), "sequence_not_found");
    assert_eq!(err_code(&g.gapi("POST", "/v1/generator/preview", Some(b(seq, 1, Uuid::now_v7()))).await), "lead_not_found");
    // the model's text does not pass the guard, twice
    g.fake.push(Ok(slots_response(&[("opener", "**bold** claim")])));
    g.fake.push(Ok(slots_response(&[("opener", "- a list")])));
    let r = g.gapi("POST", "/v1/generator/preview", Some(b(seq, 1, lead))).await;
    assert_eq!((r.status, err_code(&r)), (StatusCode::UNPROCESSABLE_ENTITY, "generation_guard_failed"));
    assert!(r.body["error"]["message"].as_str().unwrap().contains("markdown"));
    // the provider is down
    g.fake.push(Err(LlmError::Transient { code: "server_error", retry_after: None }));
    let r = g.gapi("POST", "/v1/generator/preview", Some(b(seq, 1, lead))).await;
    assert_eq!((r.status, err_code(&r)), (StatusCode::BAD_GATEWAY, "generation_server_error"));
    // no provider
    g.settings(json!({ "slot_provider_id": null })).await;
    assert_eq!(err_code(&g.gapi("POST", "/v1/generator/preview", Some(b(seq, 1, lead))).await), "generator_not_ready");
    // the budget
    g.settings(json!({ "slot_provider_id": g.provider, "daily_token_budget": 10 })).await;
    sqlx::query("INSERT INTO llm_calls (purpose, model, input_tokens, output_tokens, ok, created_at) VALUES ('slot', 'm', 50, 0, true, $1)").bind(g.h.now()).execute(&g.h.state.pool).await.unwrap();
    let r = g.gapi("POST", "/v1/generator/preview", Some(b(seq, 1, lead))).await;
    assert_eq!((r.status, err_code(&r)), (StatusCode::CONFLICT, "daily_budget_reached"));
}

// =====================================================================================================
// The sequence writer
// =====================================================================================================

fn brief() -> Value {
    json!({ "product": "Route planning software", "offer": "A free 20 minute audit", "audience": "Logistics managers", "footer": "Reply STOP to opt out." })
}

#[sqlx::test]
async fn the_sequence_writer_returns_valid_steps_and_can_save_a_paused_sequence(pool: PgPool) {
    let g = G::new(pool).await;
    g.ok(Scope::Leads, "POST", "/v1/leads/fields", Some(json!({ "key": "industry" }))).await;
    let r = g.gapi("POST", "/v1/generator/sequences/draft", Some(json!({ "brief": brief(), "steps": 3 }))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let steps = r.body["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 3);
    assert_eq!(steps[0]["delay_amount"], 0);
    assert_eq!(steps[1]["delay_amount"], 3);
    assert_eq!(steps[1]["subject"], "");
    assert!(steps[0]["body_text"].as_str().unwrap().contains("{{ai:opener}}"));
    assert_eq!(steps[0]["ai_slots"][0]["key"], "opener");
    assert_eq!(r.body["sequence_id"], Value::Null, "not saved unless asked");
    assert_eq!((r.body["input_tokens"].clone(), r.body["output_tokens"].clone()), (json!(100), json!(20)));
    // the prompt carries the brief as data, the custom fields, and the step count
    let call = &g.fake.calls()[0];
    assert!(call.system.contains("\"steps\":3") && call.system.contains("industry") && !call.system.contains("Route planning"));
    assert!(call.user.starts_with("<brief>") && call.user.contains("Route planning software") && call.user.contains("Reply STOP to opt out."));
    assert!(call.max_output_tokens >= 2000);
    // the steps are a valid body for the followups routes
    let created = g.api(Scope::Followups, "POST", "/v1/followups/sequences", Some(json!({ "name": "From draft", "steps": steps }))).await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.body);
    assert_eq!(count(&g.h.state, "SELECT count(*) FROM llm_calls WHERE purpose = 'draft'").await, 1);

    // save = true
    let r = g.gapi("POST", "/v1/generator/sequences/draft", Some(json!({ "brief": brief(), "steps": 2, "delay_days": 5, "save": true, "name": "Audit offer" }))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let id = r.body["sequence_id"].as_str().unwrap();
    let seq = g.api(Scope::Followups, "GET", &format!("/v1/followups/sequences/{id}"), None).await.body;
    assert_eq!(seq["sequence"]["status"], "paused");
    assert_eq!(seq["sequence"]["name"], "Audit offer");
    assert!(seq["sequence"]["brief"].as_str().unwrap().contains("Product: Route planning software"));
    assert_eq!(seq["steps"].as_array().unwrap().len(), 2);
    assert_eq!(seq["steps"][1]["delay_amount"], 5);
    assert_eq!(seq["steps"][0]["ai_slots"][0]["key"], "opener");
}

#[sqlx::test]
async fn the_sequence_writer_forces_the_thread_subject_and_repairs_a_bad_draft_once(pool: PgPool) {
    let g = G::new(pool).await;
    // the model puts a subject with a slot on step 2: it is dropped
    g.fake.push(Ok(response(json!({ "steps": [
        { "subject": "Hello", "body_text": "A {{ai:a}}", "ai_slots": [{ "key": "a", "instruction": "x" }] },
        { "subject": "Hello {{ai:a}}", "body_text": "B {{ai:a}}", "delay_amount": 2, "ai_slots": [{ "key": "a", "instruction": "x" }] }
    ] }).to_string())));
    let r = g.gapi("POST", "/v1/generator/sequences/draft", Some(json!({ "brief": brief(), "steps": 2 }))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["steps"][1]["subject"], "");
    assert_eq!(r.body["steps"][1]["delay_amount"], 2);

    // an invalid draft (a slot with no entry), then a good one
    g.fake.push(Ok(response(json!({ "steps": [{ "subject": "Hi", "body_text": "A {{ai:missing}}" }] }).to_string())));
    let r = g.gapi("POST", "/v1/generator/sequences/draft", Some(json!({ "brief": brief(), "steps": 1 }))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert!(g.fake.calls().last().unwrap().user.contains("Your last draft was refused") && g.fake.calls().last().unwrap().user.contains("ai:missing"));

    // two bad drafts
    g.fake.push(Ok(response("not json")));
    g.fake.push(Ok(response(json!({ "steps": [] }).to_string())));
    let r = g.gapi("POST", "/v1/generator/sequences/draft", Some(json!({ "brief": brief(), "steps": 1, "save": true }))).await;
    assert_eq!((r.status, err_code(&r)), (StatusCode::UNPROCESSABLE_ENTITY, "draft_invalid"));
    assert_eq!(count(&g.h.state, "SELECT count(*) FROM sequences").await, 0, "nothing is saved");
    // the calls are all logged
    assert_eq!(count(&g.h.state, "SELECT count(*) FROM llm_calls WHERE purpose = 'draft'").await, 1 + 2 + 2);
}

#[sqlx::test]
async fn the_sequence_writer_checks_its_input_and_its_provider(pool: PgPool) {
    let g = G::new(pool).await;
    for body in [
        json!({ "brief": { "product": " " } }),
        json!({ "brief": { "product": "x".repeat(1001) } }),
        json!({ "brief": brief(), "steps": 0 }),
        json!({ "brief": brief(), "steps": 11 }),
        json!({ "brief": brief(), "delay_days": 0 }),
    ] {
        let r = g.gapi("POST", "/v1/generator/sequences/draft", Some(body.clone())).await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }
    assert_eq!(g.fake.call_count(), 0);
    g.settings(json!({ "draft_provider_id": null })).await;
    assert_eq!(err_code(&g.gapi("POST", "/v1/generator/sequences/draft", Some(json!({ "brief": brief() }))).await), "generator_not_ready");
    g.settings(json!({ "draft_provider_id": g.provider })).await;
    g.fake.push(Err(LlmError::Auth));
    let r = g.gapi("POST", "/v1/generator/sequences/draft", Some(json!({ "brief": brief() }))).await;
    assert_eq!(r.status, StatusCode::BAD_GATEWAY);
    assert_eq!(g.genok("GET", "/v1/generator/providers", None).await["providers"][0]["status"], "auth_failed");
}

// =====================================================================================================
// The generation routes
// =====================================================================================================

/// Two sequences, three generations.
async fn three_generations(g: &G) -> (Uuid, Uuid, Vec<Uuid>) {
    let s1 = g.sequence(json!([step1()])).await;
    let s2 = g.sequence(json!([step1()])).await;
    let (_, e1) = g.enroll_new(s1, "a@acme.example").await;
    let (_, e2) = g.enroll_new(s1, "b@acme.example").await;
    let (_, e3) = g.enroll_new(s2, "c@acme.example").await;
    g.gen_tick().await;
    (s1, s2, vec![e1, e2, e3])
}

#[sqlx::test]
async fn generations_can_be_listed_filtered_paged_and_read(pool: PgPool) {
    let g = G::new(pool).await;
    let (s1, s2, es) = three_generations(&g).await;
    let all = g.genok("GET", "/v1/generator/generations", None).await;
    assert_eq!(all["generations"].as_array().unwrap().len(), 3);
    assert_eq!(all["next_cursor"], Value::Null);
    let first = &all["generations"][0];
    assert_eq!(first["status"], "ready");
    assert_eq!(first["slots"], json!({ "opener": "Fake text for opener." }));
    assert_eq!((first["input_tokens"].clone(), first["output_tokens"].clone(), first["model"].clone()), (json!(100), json!(20), json!("fake-model")));
    assert!(first.get("step_hash").is_none() && first.get("claim_id").is_none());
    // newest first, keyset paging
    let p1 = g.genok("GET", "/v1/generator/generations?limit=2", None).await;
    assert_eq!(p1["generations"].as_array().unwrap().len(), 2);
    let cursor = p1["next_cursor"].as_i64().unwrap();
    let p2 = g.genok("GET", &format!("/v1/generator/generations?limit=2&cursor={cursor}"), None).await;
    assert_eq!(p2["generations"].as_array().unwrap().len(), 1);
    assert_eq!(p2["next_cursor"], Value::Null);
    let mut seen: Vec<&str> = p1["generations"].as_array().unwrap().iter().chain(p2["generations"].as_array().unwrap()).map(|x| x["id"].as_str().unwrap()).collect();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 3);
    // filters
    assert_eq!(g.genok("GET", &format!("/v1/generator/generations?sequence_id={s1}"), None).await["generations"].as_array().unwrap().len(), 2);
    assert_eq!(g.genok("GET", &format!("/v1/generator/generations?sequence_id={s2}&status=ready"), None).await["generations"].as_array().unwrap().len(), 1);
    assert_eq!(g.genok("GET", "/v1/generator/generations?status=failed", None).await["generations"].as_array().unwrap().len(), 0);
    assert_eq!(g.genok("GET", &format!("/v1/generator/generations?enrollment_id={}", es[0]), None).await["generations"].as_array().unwrap().len(), 1);
    assert_eq!(g.gapi("GET", "/v1/generator/generations?status=nope", None).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(g.gapi("GET", "/v1/generator/generations?limit=0", None).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    // one
    let id = g.gen_id(es[0], 1).await;
    let one = g.genok("GET", &format!("/v1/generator/generations/{id}"), None).await;
    assert_eq!(one["enrollment_id"], json!(es[0]));
    assert_eq!(g.gapi("GET", &format!("/v1/generator/generations/{}", Uuid::now_v7()), None).await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn a_hand_edit_runs_the_guard_and_counts_as_approved(pool: PgPool) {
    let g = G::new(pool).await;
    let (_, _, es) = three_generations(&g).await;
    let id = g.gen_id(es[0], 1).await;
    let url = format!("/v1/generator/generations/{id}");
    let patch = |t: &str| Some(json!({ "slots": { "opener": t } }));
    for (text, code) in [("see https://x.example", "guard_failed"), ("", "missing_slot"), ("  ", "missing_slot")] {
        let r = g.gapi("PATCH", &url, patch(text)).await;
        assert_eq!((r.status, err_code(&r)), (StatusCode::UNPROCESSABLE_ENTITY, code), "{text:?}");
    }
    let r = g.gapi("PATCH", &url, Some(json!({ "slots": { "nope": "x" } }))).await;
    assert_eq!(err_code(&r), "unknown_slot");
    let long = "word ".repeat(31);
    assert_eq!(err_code(&g.gapi("PATCH", &url, patch(&long)).await), "guard_failed");
    let r = g.genok("PATCH", &url, patch("  My own line. ")).await;
    assert_eq!((r["status"].clone(), r["slots"]["opener"].clone()), (json!("approved"), json!("My own line.")));
    assert!(r["reviewed_by"].is_string());
    // a pending text can not be edited
    sqlx::query("UPDATE generations SET status = 'pending' WHERE id = $1").bind(id).execute(&g.h.state.pool).await.unwrap();
    assert_eq!(err_code(&g.gapi("PATCH", &url, patch("x")).await), "generation_busy");
    sqlx::query("UPDATE generations SET status = 'cancelled' WHERE id = $1").bind(id).execute(&g.h.state.pool).await.unwrap();
    assert_eq!(err_code(&g.gapi("PATCH", &url, patch("x")).await), "generation_closed");
    // a step that changed since: 409
    let id2 = g.gen_id(es[1], 1).await;
    let seq: (Uuid,) = sqlx::query_as("SELECT sequence_id FROM generations WHERE id = $1").bind(id2).fetch_one(&g.h.state.pool).await.unwrap();
    g.put_steps(seq.0, json!([ai_step(0, "Changed", "Changed {{ai:opener}}", opener())])).await;
    assert_eq!(err_code(&g.gapi("PATCH", &format!("/v1/generator/generations/{id2}"), patch("x")).await), "generation_stale");
}

#[sqlx::test]
async fn approve_and_reject_check_the_status(pool: PgPool) {
    let g = G::new(pool).await;
    let (_, _, es) = three_generations(&g).await;
    let id = g.gen_id(es[0], 1).await;
    let a = g.genok("POST", &format!("/v1/generator/generations/{id}/approve"), None).await;
    assert_eq!(a["status"], "approved");
    assert_eq!(g.genok("POST", &format!("/v1/generator/generations/{id}/approve"), None).await["status"], "approved", "approving twice is fine");
    assert_eq!(g.genok("POST", &format!("/v1/generator/generations/{id}/reject"), None).await["status"], "rejected");
    // a rejected text can not be approved; a missing one is 404
    let r = g.gapi("POST", &format!("/v1/generator/generations/{id}/approve"), None).await;
    assert_eq!((r.status, err_code(&r)), (StatusCode::CONFLICT, "invalid_status"));
    assert_eq!(g.gapi("POST", &format!("/v1/generator/generations/{}/reject", Uuid::now_v7()), None).await.status, StatusCode::NOT_FOUND);
    let id3 = g.gen_id(es[2], 1).await;
    sqlx::query("UPDATE generations SET status = 'failed' WHERE id = $1").bind(id3).execute(&g.h.state.pool).await.unwrap();
    assert_eq!(g.gapi("POST", &format!("/v1/generator/generations/{id3}/approve"), None).await.status, StatusCode::CONFLICT);
}

#[sqlx::test]
async fn bulk_actions_need_a_filter_and_only_touch_what_they_match(pool: PgPool) {
    let g = G::new(pool).await;
    let (s1, s2, es) = three_generations(&g).await;
    let r = g.gapi("POST", "/v1/generator/generations/bulk", Some(json!({ "action": "reject", "filter": {} }))).await;
    assert_eq!((r.status, err_code(&r)), (StatusCode::UNPROCESSABLE_ENTITY, "empty_filter"));
    assert_eq!(err_code(&g.gapi("POST", "/v1/generator/generations/bulk", Some(json!({ "action": "reject", "filter": { "ids": [] } }))).await), "empty_filter");
    assert_eq!(count(&g.h.state, "SELECT count(*) FROM generations WHERE status = 'ready'").await, 3);
    // reject one sequence
    let b = g.genok("POST", "/v1/generator/generations/bulk", Some(json!({ "action": "reject", "filter": { "sequence_id": s1 } }))).await;
    assert_eq!((b["matched"].clone(), b["changed"].clone()), (json!(2), json!(2)));
    assert_eq!(g.gens(es[2]).await, vec![(1, "ready".to_string())], "the other sequence is untouched");
    // approve does not apply to rejected text
    let b = g.genok("POST", "/v1/generator/generations/bulk", Some(json!({ "action": "approve", "filter": { "sequence_id": s1 } }))).await;
    assert_eq!((b["matched"].clone(), b["changed"].clone()), (json!(2), json!(0)));
    // regenerate: pending again, and the worker writes it again
    let calls = g.fake.call_count();
    let b = g.genok("POST", "/v1/generator/generations/bulk", Some(json!({ "action": "regenerate", "filter": { "status": "rejected" } }))).await;
    assert_eq!((b["matched"].clone(), b["changed"].clone()), (json!(2), json!(2)));
    assert_eq!(g.gens(es[0]).await, vec![(1, "pending".to_string())]);
    g.gen_tick().await;
    assert_eq!(g.fake.call_count(), calls + 2);
    // by ids
    let id = g.gen_id(es[2], 1).await;
    let b = g.genok("POST", "/v1/generator/generations/bulk", Some(json!({ "action": "approve", "filter": { "ids": [id] } }))).await;
    assert_eq!(b["changed"], 1);
    assert_eq!(g.gens(es[2]).await, vec![(1, "approved".to_string())]);
    assert_eq!(g.gapi("POST", "/v1/generator/generations/bulk", Some(json!({ "action": "burn", "filter": { "sequence_id": s2 } }))).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(g.gapi("POST", "/v1/generator/generations/bulk", Some(json!({ "action": "approve", "filter": { "status": "nope" } }))).await.status, StatusCode::UNPROCESSABLE_ENTITY);
}

// =====================================================================================================
// Usage, status, pause
// =====================================================================================================

#[sqlx::test]
async fn usage_shows_tokens_by_day_campaign_model_and_purpose_and_money_when_there_is_a_price(pool: PgPool) {
    let g = G::new(pool).await;
    sqlx::query("INSERT INTO llm_calls (purpose, model, input_tokens, output_tokens, cached_tokens, ok, created_at) VALUES ('slot', 'cheap', 1000000, 500000, 800000, true, $1), ('draft', 'strong', 200, 100, 0, true, $1), ('slot', 'cheap', 10, 0, 0, false, $2)")
        .bind(g.h.now())
        .bind(g.h.now() - Duration::days(2))
        .execute(&g.h.state.pool)
        .await
        .unwrap();
    g.settings(json!({ "prices": { "cheap": { "input": 0.5, "output": 2.0 } } })).await;
    let u = g.genok("GET", "/v1/generator/usage", None).await;
    assert_eq!((u["total"]["calls"].clone(), u["total"]["failed_calls"].clone(), u["total"]["input_tokens"].clone(), u["total"]["output_tokens"].clone()), (json!(3), json!(1), json!(1_000_210), json!(500_100)));
    assert_eq!(u["total"]["cached_tokens"], 800_000);
    let days: Vec<&str> = u["by_day"].as_array().unwrap().iter().map(|d| d["key"].as_str().unwrap()).collect();
    assert_eq!(days, ["2026-10-17", "2026-10-19"]);
    let model = |k: &str| u["by_model"].as_array().unwrap().iter().find(|m| m["key"] == k).cloned().unwrap();
    // 1,000,010 input x 0.5 + 500,000 output x 2.0 = 0.500005 + 1.0 per 1M
    let cost = model("cheap")["cost_usd"].as_f64().unwrap();
    assert!((cost - 1.500005).abs() < 1e-9, "{cost}");
    assert_eq!(model("strong")["cost_usd"], Value::Null, "no price, no money");
    assert!((u["total"]["cost_usd"].as_f64().unwrap() - 1.500005).abs() < 1e-9);
    let purposes: Vec<&str> = u["by_purpose"].as_array().unwrap().iter().map(|d| d["key"].as_str().unwrap()).collect();
    assert_eq!(purposes, ["draft", "slot"]);
    assert_eq!(u["by_campaign"][0]["key"], "none");
    // a range
    let u = g.genok("GET", "/v1/generator/usage?from=2026-10-19&to=2026-10-19", None).await;
    assert_eq!(u["total"]["calls"], 2);
    assert_eq!(g.gapi("GET", "/v1/generator/usage?from=2026-10-20&to=2026-10-19", None).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(g.gapi("GET", "/v1/generator/usage?from=2020-01-01&to=2026-10-19", None).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(g.gapi("GET", "/v1/generator/usage?from=yesterday", None).await.status, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn status_shows_the_queue_the_providers_and_what_to_look_at(pool: PgPool) {
    let g = G::new(pool).await;
    let s = g.genok("GET", "/v1/generator/status", None).await;
    assert_eq!((s["paused"].clone(), s["worker_enabled"].clone(), s["last_tick_at"].clone()), (json!(false), json!(false), Value::Null));
    assert_eq!(s["slot_provider"]["name"], "Fake");
    assert_eq!(s["budget"]["daily_limit"], 0);
    assert_eq!(s["budget"]["left_today"], Value::Null);
    assert_eq!(s["warnings"], json!([]));
    three_generations(&g).await;
    let s = g.genok("GET", "/v1/generator/status", None).await;
    assert_eq!(s["queue"], json!({ "ready": 3 }));
    assert!(s["last_tick_at"].is_string());
    assert_eq!(s["budget"]["used_today"], 360);
    g.genok("POST", "/v1/generator/pause", None).await;
    sqlx::query("UPDATE generations SET status = 'failed' WHERE id = (SELECT id FROM generations LIMIT 1)").execute(&g.h.state.pool).await.unwrap();
    let s = g.genok("GET", "/v1/generator/status", None).await;
    assert_eq!(s["paused"], true);
    let w: Vec<&str> = s["warnings"].as_array().unwrap().iter().map(|w| w.as_str().unwrap().split(':').next().unwrap()).collect();
    assert!(w.contains(&"paused") && w.contains(&"failed_generations"), "{w:?}");
    g.settings(json!({ "slot_provider_id": null })).await;
    let s = g.genok("GET", "/v1/generator/status", None).await;
    assert!(s["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().starts_with("no_slot_provider")));
    assert_eq!(s["slot_provider"], Value::Null);
}

// =====================================================================================================
// Migration 0006
// =====================================================================================================

#[sqlx::test]
async fn migration_adds_only_new_tables_and_columns(pool: PgPool) {
    let new: Vec<(String,)> = sqlx::query_as(
        "SELECT table_name::text FROM information_schema.tables WHERE table_schema = 'public' AND (table_name LIKE 'generat%' OR table_name LIKE 'llm_%') ORDER BY 1",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let names: Vec<&str> = new.iter().map(|t| t.0.as_str()).collect();
    assert_eq!(names, ["generations", "generator_campaigns", "generator_defaults", "llm_calls", "llm_providers"]);
    // the old tables got only the columns the build map names (plus the two wait columns on enrollments)
    let cols = |table: &'static str| {
        let pool = pool.clone();
        async move {
            let rows: Vec<(String,)> = sqlx::query_as("SELECT column_name::text FROM information_schema.columns WHERE table_schema = 'public' AND table_name = $1").bind(table).fetch_all(&pool).await.unwrap();
            rows.into_iter().map(|r| r.0).collect::<Vec<_>>()
        }
    };
    for (table, new_cols) in [("sequence_steps", vec!["ai_slots", "step_hash"]), ("sequences", vec!["brief"]), ("enrollments", vec!["wait_reason", "wait_since"])] {
        let have = cols(table).await;
        for c in new_cols {
            assert!(have.contains(&c.to_string()), "{table}.{c}");
        }
    }
    assert_eq!(cols("sequence_steps").await.len(), 11);
    assert_eq!(cols("sequences").await.len(), 10);
    // a step written before the Email Generator has no slots
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM generator_defaults WHERE id = 1 AND settings = '{}'::jsonb AND NOT engine_paused").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1);
}
