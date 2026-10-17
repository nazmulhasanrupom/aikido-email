//! Email Generator: the worker, the budgets, the scheduler hook, review mode, stale text, stops and erasing.
//! `FakeLlm` answers every call. No real provider.
mod common;
use aikido_email::auth::api_keys::Scope;
use aikido_email::generator::llm::LlmError;
use aikido_email::generator::llm::fake::{response, slots_response};
use aikido_email::generator::worker::{self, GeneratorEngine};
use axum::http::StatusCode;
use chrono::Duration;
use common::gharness::*;
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

const AI_TEXT: &str = "Fake text for opener.";
const FALLBACK: &str = "I came across your company.";

// =====================================================================================================
// The worker
// =====================================================================================================

#[sqlx::test]
async fn a_step_is_written_only_when_it_is_due_within_the_look_ahead(pool: PgPool) {
    let g = G::new(pool).await;
    let seq = g.sequence(json!([step1()])).await;
    let lead = g.lead("ana@acme.example", "Ana").await;
    let e = g.enroll(seq, lead, Some(g.h.now() + Duration::hours(10))).await;

    let r = g.gen_tick().await;
    assert_eq!((r.discovered, r.claimed), (0, 0), "due in 10 hours, look-ahead is 6");
    assert_eq!(g.fake.call_count(), 0);

    g.h.advance(Duration::hours(5)); // due in 5 hours
    let r = g.gen_tick().await;
    assert_eq!((r.discovered, r.claimed, r.ready), (1, 1, 1));
    assert_eq!(g.gens(e).await, vec![(1, "ready".to_string())]);
    assert_eq!(g.fake.call_count(), 1);
    // the text, the usage and the provider are saved
    let row: (Value, i32, i32, Option<String>, Option<Uuid>, i32) =
        sqlx::query_as("SELECT slots, input_tokens, output_tokens, model, provider_id, attempts FROM generations WHERE enrollment_id = $1").bind(e).fetch_one(&g.h.state.pool).await.unwrap();
    assert_eq!(row.0, json!({ "opener": AI_TEXT }));
    assert_eq!((row.1, row.2, row.3.as_deref(), row.4, row.5), (100, 20, Some("fake-model"), Some(g.provider), 1));
    // one llm_calls row, with no text
    let call: (String, Option<Uuid>, i32, i32, bool) = sqlx::query_as("SELECT purpose, campaign_id, input_tokens, output_tokens, ok FROM llm_calls").fetch_one(&g.h.state.pool).await.unwrap();
    assert_eq!(call, ("slot".to_string(), None, 100, 20, true));

    // a second tick does nothing: the row exists
    let r = g.gen_tick().await;
    assert_eq!((r.discovered, r.claimed), (0, 0));
    assert_eq!(g.fake.call_count(), 1);

    // at due time the scheduler fills the slot from the saved text. No call at send time.
    g.h.advance(Duration::hours(5));
    assert_eq!(g.fu_tick().await.queued, 1);
    let (subject, text, _) = g.job_text(e, 1).await;
    assert_eq!(subject, "Hello Ana");
    assert_eq!(text.as_deref(), Some(&*format!("Hi Ana,\n{AI_TEXT}\nBye")));
    assert_eq!(g.fake.call_count(), 1);
    assert_eq!(g.wait_reason(e).await, None);
}

#[sqlx::test]
async fn the_prompt_has_the_fixed_part_first_the_lead_in_a_data_block_and_no_email_address(pool: PgPool) {
    let g = G::new(pool).await;
    let seq = g.sequence(json!([step1()])).await;
    g.api(Scope::Followups, "PATCH", &format!("/v1/followups/sequences/{seq}"), Some(json!({ "brief": "We sell widgets to logistics firms." }))).await;
    g.ok(Scope::Leads, "POST", "/v1/leads/fields", Some(json!({ "key": "pain_point" }))).await;
    let l1 = g.ok(Scope::Leads, "POST", "/v1/leads", Some(json!({ "email": "ana@acme.example", "first_name": "Ana", "company_name": "Acme", "phone": "+44 20 7946 0958", "custom_fields": { "pain_point": "late deliveries" } }))).await;
    let l2 = g.lead("bob@globex.example", "Bob").await;
    g.ok(Scope::Leads, "POST", &format!("/v1/leads/{}/notes", l1["id"].as_str().unwrap()), Some(json!({ "text": "Met at LogiCon 2026" }))).await;
    g.enroll(seq, l1["id"].as_str().unwrap().parse().unwrap(), None).await;
    g.enroll(seq, l2, None).await;
    g.gen_tick().await;

    let calls = g.fake.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].system, calls[1].system, "the fixed part is the same for two leads, so the provider can cache it");
    assert!(calls[0].system.contains("We sell widgets to logistics firms.") && calls[0].system.contains("{{ai:opener}}"));
    assert!(calls[0].json);
    let (ana, bob): (Vec<_>, Vec<_>) = calls.iter().partition(|c| c.user.contains("Ana"));
    assert_eq!((ana.len(), bob.len()), (1, 1));
    let u = &ana[0].user;
    assert!(u.starts_with("<lead_data>") && u.contains("</lead_data>"));
    for needle in ["Company: Acme", "pain_point: late deliveries", "Met at LogiCon 2026"] {
        assert!(u.contains(needle), "{needle} in {u}");
    }
    for c in &calls {
        let all = format!("{}{}", c.system, c.user);
        assert!(!all.contains("@acme.example") && !all.contains("@globex.example"), "the email address is not sent to the model");
        assert!(!all.contains("7946"), "the phone number is not sent to the model");
    }
}

#[sqlx::test]
async fn two_worker_copies_make_one_call_per_step(pool: PgPool) {
    let g = G::new(pool).await;
    let seq = g.sequence(json!([step1()])).await;
    let items: Vec<Value> = (0..40).map(|i| json!({ "to_email": format!("p{i}@acme.example"), "account_id": g.account })).collect();
    let r = g.api(Scope::Followups, "POST", &format!("/v1/followups/sequences/{seq}/enrollments"), Some(json!({ "enrollments": items }))).await;
    assert_eq!(r.body["enrolled"], 40, "{}", r.body);
    let other = Arc::new(GeneratorEngine::new(g.h.state.clone()));
    g.fake.set_delay(std::time::Duration::from_millis(15));
    for _ in 0..3 {
        let (a, b) = tokio::join!(worker::tick(&g.ge), worker::tick(&other));
        a.unwrap();
        b.unwrap();
    }
    assert_eq!(g.fake.call_count(), 40, "two copies, one call per step");
    assert_eq!(count(&g.h.state, "SELECT count(*) FROM generations WHERE status = 'ready'").await, 40);
    assert_eq!(count(&g.h.state, "SELECT count(*) FROM generations").await, 40);
    assert_eq!(count(&g.h.state, "SELECT count(*) FROM llm_calls").await, 40);
}

#[sqlx::test]
async fn a_lease_that_ran_out_is_picked_up_again_and_a_live_lease_is_left_alone(pool: PgPool) {
    let g = G::new(pool).await;
    let seq = g.sequence(json!([step1()])).await;
    let (_, e) = g.enroll_new(seq, "ana@acme.example").await;
    g.gen_tick().await;
    assert_eq!(g.fake.call_count(), 1);
    let crashed = |minutes: i64| {
        let at = g.h.now() - Duration::minutes(minutes);
        sqlx::query("UPDATE generations SET status = 'generating', claim_id = gen_random_uuid(), claimed_at = $2, slots = '{}' WHERE enrollment_id = $1").bind(e).bind(at)
    };
    // another copy took it 5 minutes ago: the lease is alive
    crashed(5).execute(&g.h.state.pool).await.unwrap();
    assert_eq!(g.gen_tick().await.claimed, 0);
    assert_eq!(g.fake.call_count(), 1);
    // 11 minutes ago: that copy is dead
    crashed(11).execute(&g.h.state.pool).await.unwrap();
    let r = g.gen_tick().await;
    assert_eq!((r.claimed, r.ready), (1, 1));
    assert_eq!(g.fake.call_count(), 2);
    assert_eq!(g.gens(e).await, vec![(1, "ready".to_string())]);
    let (attempts,): (i32,) = sqlx::query_as("SELECT attempts FROM generations WHERE enrollment_id = $1").bind(e).fetch_one(&g.h.state.pool).await.unwrap();
    assert_eq!(attempts, 2);
}

#[sqlx::test]
async fn a_paused_generator_or_a_missing_provider_makes_nothing(pool: PgPool) {
    let g = G::new(pool).await;
    let seq = g.sequence(json!([step1()])).await;
    g.enroll_new(seq, "ana@acme.example").await;
    g.genok("POST", "/v1/generator/pause", None).await;
    assert!(g.gen_tick().await.paused);
    assert_eq!(count(&g.h.state, "SELECT count(*) FROM generations").await, 0);
    g.genok("POST", "/v1/generator/resume", None).await;
    g.settings(json!({ "slot_provider_id": null })).await;
    assert!(g.gen_tick().await.no_provider);
    assert_eq!(g.fake.call_count(), 0);
    g.settings(json!({ "slot_provider_id": g.provider })).await;
    assert_eq!(g.gen_tick().await.ready, 1);
}

// =====================================================================================================
// Errors from the provider
// =====================================================================================================

#[sqlx::test]
async fn a_transient_error_is_retried_later_and_gives_up_after_max_attempts(pool: PgPool) {
    let g = G::new(pool).await;
    let seq = g.sequence(json!([step1()])).await;
    let (_, e) = g.enroll_new(seq, "ana@acme.example").await;
    let transient = || Err(LlmError::Transient { code: "rate_limited", retry_after: None });
    g.fake.push(transient());
    let r = g.gen_tick().await;
    assert_eq!((r.claimed, r.retried, r.ready), (1, 1, 0));
    let (status, attempts, code, retry): (String, i32, Option<String>, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("SELECT status, attempts, error_code, retry_at FROM generations WHERE enrollment_id = $1").bind(e).fetch_one(&g.h.state.pool).await.unwrap();
    assert_eq!((status.as_str(), attempts, code.as_deref()), ("pending", 1, Some("rate_limited")));
    assert!(retry.unwrap() > g.h.now());
    // not before retry_at
    assert_eq!(g.gen_tick().await.claimed, 0);
    g.h.advance(Duration::seconds(31));
    g.fake.push(transient());
    g.gen_tick().await; // attempt 2
    g.h.advance(Duration::seconds(61));
    g.fake.push(transient());
    let r = g.gen_tick().await; // attempt 3 = max_attempts
    assert_eq!((r.failed, r.retried), (1, 0));
    let (status, attempts, code): (String, i32, Option<String>) =
        sqlx::query_as("SELECT status, attempts, error_code FROM generations WHERE enrollment_id = $1").bind(e).fetch_one(&g.h.state.pool).await.unwrap();
    assert_eq!((status.as_str(), attempts, code.as_deref()), ("failed", 3, Some("rate_limited")));
    assert_eq!(g.fake.call_count(), 3);
    // a failed text is not tried again by itself; the step uses its fallback at once, even with a grace time
    g.settings(json!({ "due_grace_mins": 30 })).await;
    assert_eq!(g.fu_tick().await.queued, 1);
    assert_eq!(g.job_text(e, 1).await.1.as_deref(), Some(&*format!("Hi Ana,\n{FALLBACK}\nBye")));
}

#[sqlx::test]
async fn a_refused_key_pauses_the_generator_and_a_new_key_starts_it_again(pool: PgPool) {
    let g = G::new(pool).await;
    let seq = g.sequence(json!([step1()])).await;
    let (_, e) = g.enroll_new(seq, "ana@acme.example").await;
    g.fake.push(Err(LlmError::Auth));
    let r = g.gen_tick().await;
    assert!(r.auth_failed);
    assert_eq!(g.gens(e).await, vec![(1, "pending".to_string())], "the text waits; it did not fail");
    let p = g.genok("GET", "/v1/generator/providers", None).await;
    assert_eq!(p["providers"][0]["status"], "auth_failed");
    // no more calls while the key is refused
    let r = g.gen_tick().await;
    assert!(r.auth_failed);
    assert_eq!(g.fake.call_count(), 1);
    let s = g.genok("GET", "/v1/generator/status", None).await;
    assert!(s["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().starts_with("provider_auth_failed")), "{s}");
    assert_eq!(s["slot_provider"]["status"], "auth_failed");
    // the preview says why
    let seq2 = g.sequence(json!([step1()])).await;
    let lead = g.lead("bob@acme.example", "Bob").await;
    let pv = g.gapi("POST", "/v1/generator/preview", Some(json!({ "sequence_id": seq2, "step_number": 1, "lead_id": lead }))).await;
    assert_eq!((pv.status, pv.body["error"]["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("generator_not_ready")));
    // a new key
    g.genok("PATCH", &format!("/v1/generator/providers/{}", g.provider), Some(json!({ "api_key": "new-key" }))).await;
    let r = g.gen_tick().await;
    assert!(!r.auth_failed);
    assert_eq!(g.gens(e).await, vec![(1, "ready".to_string())]);
}

#[sqlx::test]
async fn a_bad_answer_is_repaired_once_and_a_second_bad_answer_fails_without_the_text(pool: PgPool) {
    let g = G::new(pool).await;
    let seq = g.sequence(json!([step1()])).await;
    let (_, e1) = g.enroll_new(seq, "ana@acme.example").await;
    // first answer is not JSON, the repair is good
    g.fake.push(Ok(response("Sorry, here you go: no json")));
    g.fake.push(Ok(slots_response(&[("opener", "A calm line.")])));
    let r = g.gen_tick().await;
    assert_eq!(r.ready, 1);
    assert_eq!(g.fake.call_count(), 2);
    assert!(g.fake.calls()[1].user.contains("Your last answer was refused"));
    let (calls, bad): (i64, i64) = sqlx::query_as("SELECT count(*), count(*) FILTER (WHERE NOT ok) FROM llm_calls").fetch_one(&g.h.state.pool).await.unwrap();
    assert_eq!((calls, bad), (2, 1), "both calls are logged");
    // two guard failures: failed, no text
    let (_, e2) = g.enroll_new(seq, "bob@acme.example").await;
    g.fake.push(Ok(slots_response(&[("opener", "see https://evil.example/x")])));
    g.fake.push(Ok(slots_response(&[("opener", "write me at bob@evil.example")])));
    let r = g.gen_tick().await;
    assert_eq!(r.failed, 1);
    let (status, slots, code, warnings): (String, Value, Option<String>, Value) =
        sqlx::query_as("SELECT status, slots, error_code, warnings FROM generations WHERE enrollment_id = $1").bind(e2).fetch_one(&g.h.state.pool).await.unwrap();
    assert_eq!((status.as_str(), code.as_deref()), ("failed", Some("guard_failed")));
    assert_eq!(slots, json!({}));
    assert!(!warnings.to_string().contains("evil.example"), "the text of the model is not kept");
    let _ = e1;
}

// =====================================================================================================
// Budgets
// =====================================================================================================

#[sqlx::test]
async fn over_the_daily_budget_no_call_is_made_a_warning_shows_and_the_fallback_is_used(pool: PgPool) {
    let g = G::new(pool).await;
    g.settings(json!({ "daily_token_budget": 500 })).await;
    sqlx::query("INSERT INTO llm_calls (purpose, model, input_tokens, output_tokens, ok, created_at) VALUES ('slot', 'm', 600, 0, true, $1)").bind(g.h.now()).execute(&g.h.state.pool).await.unwrap();
    // yesterday does not count
    sqlx::query("INSERT INTO llm_calls (purpose, model, input_tokens, output_tokens, ok, created_at) VALUES ('slot', 'm', 99999, 0, true, $1)").bind(g.h.now() - Duration::days(1)).execute(&g.h.state.pool).await.unwrap();
    let seq = g.sequence(json!([step1()])).await;
    let (_, e) = g.enroll_new(seq, "ana@acme.example").await;
    let r = g.gen_tick().await;
    assert!(r.daily_budget_reached);
    assert_eq!((r.claimed, g.fake.call_count()), (0, 0));
    assert_eq!(g.gens(e).await, vec![(1, "pending".to_string())], "left as pending");
    let s = g.genok("GET", "/v1/generator/status", None).await;
    assert_eq!((s["budget"]["daily_limit"].clone(), s["budget"]["used_today"].clone(), s["budget"]["left_today"].clone(), s["budget"]["reached"].clone()), (json!(500), json!(600), json!(0), json!(true)));
    assert!(s["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().starts_with("daily_budget_reached")));
    // at due time: the fallback
    assert_eq!(g.fu_tick().await.queued, 1);
    assert_eq!(g.job_text(e, 1).await.1.as_deref(), Some(&*format!("Hi Ana,\n{FALLBACK}\nBye")));
    assert!(g.event_kinds(e).await.contains(&"generation_fallback".to_string()));
}

#[sqlx::test]
async fn the_budget_stops_the_calls_and_the_next_day_starts_them_again(pool: PgPool) {
    let g = G::new(pool).await;
    g.settings(json!({ "daily_token_budget": 200, "concurrency": 1 })).await;
    let seq = g.sequence(json!([step1()])).await;
    for i in 0..3 {
        g.enroll_new(seq, &format!("p{i}@acme.example")).await;
    }
    let r = g.gen_tick().await;
    assert!(r.daily_budget_reached);
    assert_eq!(g.fake.call_count(), 2, "120 tokens a call: 120, then 240 which is over 200");
    g.h.advance(Duration::days(1));
    let r = g.gen_tick().await;
    assert_eq!((r.claimed, r.ready), (1, 1));
    let u = g.genok("GET", "/v1/generator/usage", None).await;
    assert_eq!(u["total"]["calls"], 3);
    assert_eq!(u["by_day"].as_array().unwrap().len(), 2);
}

#[sqlx::test]
async fn a_campaign_budget_stops_the_calls_of_that_campaign_only(pool: PgPool) {
    let g = G::new(pool).await;
    g.settings(json!({ "concurrency": 1 })).await;
    let r = g
        .ok(
            Scope::Campaigns,
            "POST",
            "/v1/campaigns",
            Some(json!({ "name": "AI campaign", "sequence": { "name": "S", "steps": [step1()] }, "account_ids": [g.account], "daily_new_leads": 10 })),
        )
        .await;
    let cid = r["id"].as_str().unwrap().to_string();
    let leads: Vec<Value> = (0..3).map(|i| json!({ "email": format!("c{i}@acme.example"), "first_name": "C" })).collect();
    let bulk = g.ok(Scope::Leads, "POST", "/v1/leads/bulk", Some(json!({ "leads": leads }))).await;
    let ids: Vec<Value> = bulk["items"].as_array().unwrap().iter().map(|i| i["lead_id"].clone()).collect();
    g.ok(Scope::Campaigns, "POST", &format!("/v1/campaigns/{cid}/leads"), Some(json!({ "lead_ids": ids }))).await;
    g.ok(Scope::Campaigns, "POST", &format!("/v1/campaigns/{cid}/launch"), None).await;
    aikido_email::campaigns::feeder::tick(&g.h.state).await.unwrap();
    // an outside enrollment is not in the campaign
    let seq = g.sequence(json!([step1()])).await;
    g.enroll_new(seq, "outside@acme.example").await;

    let c = g.genok("PATCH", &format!("/v1/generator/campaigns/{cid}"), Some(json!({ "token_budget": 150 }))).await;
    assert_eq!((c["token_budget"].clone(), c["review_required"].clone(), c["tokens_used"].clone()), (json!(150), Value::Null, json!(0)));
    g.gen_tick().await;
    // 120, then 240 (over 150): the campaign is stopped. The outside step was written too.
    let by: Vec<(Option<Uuid>, i64)> = sqlx::query_as("SELECT campaign_id, count(*) FROM llm_calls GROUP BY 1 ORDER BY 2").fetch_all(&g.h.state.pool).await.unwrap();
    let cuuid: Uuid = cid.parse().unwrap();
    assert!(by.contains(&(Some(cuuid), 2)) && by.contains(&(None, 1)), "{by:?}");
    assert_eq!(count(&g.h.state, "SELECT count(*) FROM generations WHERE status = 'pending'").await, 1);
    let c = g.genok("GET", &format!("/v1/generator/campaigns/{cid}"), None).await;
    assert_eq!(c["tokens_used"], 240);
    let s = g.genok("GET", "/v1/generator/status", None).await;
    assert!(s["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().starts_with("campaign_budget_reached")), "{s}");
    // the budget can be raised or removed
    g.genok("PATCH", &format!("/v1/generator/campaigns/{cid}"), Some(json!({ "token_budget": null }))).await;
    assert_eq!(g.gen_tick().await.ready, 1);
    let bad = g.gapi("PATCH", &format!("/v1/generator/campaigns/{cid}"), Some(json!({ "token_budget": -1 }))).await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(g.gapi("GET", &format!("/v1/generator/campaigns/{}", Uuid::now_v7()), None).await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn a_campaign_can_override_the_review_mode(pool: PgPool) {
    let g = G::new(pool).await;
    let r = g.ok(Scope::Campaigns, "POST", "/v1/campaigns", Some(json!({ "name": "Review me", "sequence": { "name": "S", "steps": [step1()] }, "account_ids": [g.account] }))).await;
    let cid = r["id"].as_str().unwrap().to_string();
    let lead = g.lead("c1@acme.example", "C").await;
    g.ok(Scope::Campaigns, "POST", &format!("/v1/campaigns/{cid}/leads"), Some(json!({ "lead_ids": [lead] }))).await;
    g.ok(Scope::Campaigns, "POST", &format!("/v1/campaigns/{cid}/launch"), None).await;
    g.genok("PATCH", &format!("/v1/generator/campaigns/{cid}"), Some(json!({ "review_required": true }))).await;
    aikido_email::campaigns::feeder::tick(&g.h.state).await.unwrap();
    assert_eq!(g.gen_tick().await.pending_review, 1, "the global setting is off, the campaign asks for review");
    // `null` goes back to the global setting
    let c = g.genok("PATCH", &format!("/v1/generator/campaigns/{cid}"), Some(json!({ "review_required": null }))).await;
    assert_eq!(c["review_required"], Value::Null);
}

// =====================================================================================================
// The follow-up scheduler hook
// =====================================================================================================

#[sqlx::test]
async fn a_ready_text_is_used_and_html_values_are_escaped(pool: PgPool) {
    let g = G::new(pool).await;
    let seq = g.sequence(json!([{ "delay_amount": 0, "subject": "Hi", "body_text": "{{ai:opener}}", "body_html": "<p>{{ai:opener}}</p>", "ai_slots": opener() }])).await;
    let (_, e) = g.enroll_new(seq, "ana@acme.example").await;
    g.fake.push(Ok(slots_response(&[("opener", "Tom & Jerry's \"team\" rocks")])));
    g.gen_tick().await;
    g.fu_tick().await;
    let (_, text, html) = g.job_text(e, 1).await;
    assert_eq!(text.as_deref(), Some("Tom & Jerry's \"team\" rocks"));
    assert_eq!(html.as_deref(), Some("<p>Tom &amp; Jerry&#39;s &quot;team&quot; rocks</p>"));
}

#[sqlx::test]
async fn no_text_and_on_failure_fallback_uses_the_fallback_text(pool: PgPool) {
    let g = G::new(pool).await;
    let seq = g.sequence(json!([step1()])).await;
    let (_, e) = g.enroll_new(seq, "ana@acme.example").await;
    // the worker did not run
    let r = g.fu_tick().await;
    assert_eq!((r.queued, r.waiting), (1, 0));
    assert_eq!(g.job_text(e, 1).await.1.as_deref(), Some(&*format!("Hi Ana,\n{FALLBACK}\nBye")));
    assert!(g.event_kinds(e).await.contains(&"generation_fallback".to_string()));
    assert_eq!(g.fake.call_count(), 0);
}

#[sqlx::test]
async fn an_inline_fallback_is_used_when_the_slot_has_none_and_no_fallback_at_all_fails_the_enrollment(pool: PgPool) {
    let g = G::new(pool).await;
    let bare = json!([{ "key": "opener", "instruction": "Write a line." }]);
    let s1 = g.sequence(json!([ai_step(0, "Hi", "A {{ai:opener|inline text}} B", bare.clone())])).await;
    let (_, e1) = g.enroll_new(s1, "ana@acme.example").await;
    g.fu_tick().await;
    assert_eq!(g.job_text(e1, 1).await.1.as_deref(), Some("A inline text B"));

    let s2 = g.sequence(json!([ai_step(0, "Hi", "A {{ai:opener}} B", bare)])).await;
    let (_, e2) = g.enroll_new(s2, "bob@acme.example").await;
    let r = g.fu_tick().await;
    assert_eq!(r.failed, 1);
    assert_eq!(g.status(e2).await, "failed");
    assert_eq!(g.reason(e2).await.as_deref(), Some("generation_failed"));
    assert!(g.jobs(e2).await.is_empty());
}

#[sqlx::test]
async fn on_failure_wait_holds_the_step_and_then_uses_the_fallback_after_max_wait_mins(pool: PgPool) {
    let g = G::new(pool).await;
    g.settings(json!({ "on_failure": "wait", "max_wait_mins": 120 })).await;
    let seq = g.sequence(json!([step1()])).await;
    let (_, e) = g.enroll_new(seq, "ana@acme.example").await;
    let r = g.fu_tick().await;
    assert_eq!((r.queued, r.waiting), (0, 1));
    assert_eq!(g.status(e).await, "active");
    assert_eq!(g.wait_reason(e).await.as_deref(), Some("generation_pending"));
    assert!(g.jobs(e).await.is_empty());
    g.h.advance(Duration::minutes(119));
    assert_eq!(g.fu_tick().await.waiting, 1);
    // the stall check (7 days) is not what ends the wait; max_wait_mins is
    g.h.advance(Duration::minutes(1));
    assert_eq!(g.fu_tick().await.queued, 1);
    assert_eq!(g.job_text(e, 1).await.1.as_deref(), Some(&*format!("Hi Ana,\n{FALLBACK}\nBye")));
    assert_eq!(g.wait_reason(e).await, None, "the wait ends with the job");
    let kinds = g.event_kinds(e).await;
    assert_eq!(kinds.iter().filter(|k| *k == "step_waiting").count(), 1, "one event for the whole wait");
}

#[sqlx::test]
async fn a_wait_ends_as_soon_as_the_text_is_ready(pool: PgPool) {
    let g = G::new(pool).await;
    g.settings(json!({ "on_failure": "wait" })).await;
    let seq = g.sequence(json!([step1()])).await;
    let (_, e) = g.enroll_new(seq, "ana@acme.example").await;
    assert_eq!(g.fu_tick().await.waiting, 1);
    g.gen_tick().await;
    assert_eq!(g.fu_tick().await.queued, 1);
    assert_eq!(g.job_text(e, 1).await.1.as_deref(), Some(&*format!("Hi Ana,\n{AI_TEXT}\nBye")));
}

#[sqlx::test]
async fn the_grace_time_gives_the_worker_a_chance_before_on_failure_applies(pool: PgPool) {
    let g = G::new(pool).await;
    g.settings(json!({ "due_grace_mins": 5 })).await;
    let seq = g.sequence(json!([step1()])).await;
    // 1. the worker is quick: its text is used
    let (_, e1) = g.enroll_new(seq, "ana@acme.example").await;
    assert_eq!(g.fu_tick().await.waiting, 1);
    assert_eq!(g.wait_reason(e1).await.as_deref(), Some("generation_pending"));
    g.h.advance(Duration::minutes(1));
    g.gen_tick().await;
    assert_eq!(g.fu_tick().await.queued, 1);
    assert_eq!(g.job_text(e1, 1).await.1.as_deref(), Some(&*format!("Hi Ana,\n{AI_TEXT}\nBye")));
    // 2. the worker is down: after 5 minutes the fallback goes
    let (_, e2) = g.enroll_new(seq, "bob@acme.example").await;
    g.fu_tick().await;
    g.h.advance(Duration::minutes(4));
    assert_eq!(g.fu_tick().await.waiting, 1);
    g.h.advance(Duration::minutes(1));
    assert_eq!(g.fu_tick().await.queued, 1);
    assert_eq!(g.job_text(e2, 1).await.1.as_deref(), Some(&*format!("Hi Ana,\n{FALLBACK}\nBye")));
}

#[sqlx::test]
async fn a_failed_text_waits_for_a_hand_edit_in_wait_mode_and_the_edit_is_used(pool: PgPool) {
    let g = G::new(pool).await;
    g.settings(json!({ "on_failure": "wait" })).await;
    let seq = g.sequence(json!([step1()])).await;
    let (_, e) = g.enroll_new(seq, "ana@acme.example").await;
    g.fake.push(Ok(slots_response(&[("opener", "see https://x.example")])));
    g.fake.push(Ok(slots_response(&[("opener", "see https://x.example")])));
    assert_eq!(g.gen_tick().await.failed, 1);
    assert_eq!(g.fu_tick().await.waiting, 1);
    let id = g.gen_id(e, 1).await;
    let r = g.genok("PATCH", &format!("/v1/generator/generations/{id}"), Some(json!({ "slots": { "opener": "A line I wrote myself." } }))).await;
    assert_eq!(r["status"], "approved");
    assert!(r["reviewed_by"].is_string());
    assert_eq!(g.fu_tick().await.queued, 1);
    assert_eq!(g.job_text(e, 1).await.1.as_deref(), Some("Hi Ana,\nA line I wrote myself.\nBye"));
}

// =====================================================================================================
// Review mode
// =====================================================================================================

#[sqlx::test]
async fn in_review_mode_the_step_waits_for_approval_and_then_sends(pool: PgPool) {
    let g = G::new(pool).await;
    g.settings(json!({ "review_required": true })).await;
    let seq = g.sequence(json!([step1()])).await;
    let (_, e) = g.enroll_new(seq, "ana@acme.example").await;
    let r = g.gen_tick().await;
    assert_eq!((r.ready, r.pending_review), (0, 1));
    assert_eq!(g.gens(e).await, vec![(1, "pending_review".to_string())]);
    let r = g.fu_tick().await;
    assert_eq!((r.queued, r.waiting), (0, 1));
    assert_eq!(g.wait_reason(e).await.as_deref(), Some("generation_review"));
    // it does not fall back, however long it takes
    g.h.advance(Duration::days(3));
    assert_eq!(g.fu_tick().await.waiting, 1);
    assert!(g.jobs(e).await.is_empty());
    let s = g.genok("GET", "/v1/generator/status", None).await;
    assert!(s["warnings"].as_array().unwrap().iter().any(|w| w.as_str().unwrap().starts_with("awaiting_review")));

    let id = g.gen_id(e, 1);
    let id = id.await;
    let a = g.genok("POST", &format!("/v1/generator/generations/{id}/approve"), None).await;
    assert_eq!(a["status"], "approved");
    assert!(a["reviewed_by"].is_string());
    assert_eq!(g.fu_tick().await.queued, 1);
    assert_eq!(g.job_text(e, 1).await.1.as_deref(), Some(&*format!("Hi Ana,\n{AI_TEXT}\nBye")));
}

#[sqlx::test]
async fn a_rejected_text_is_made_again(pool: PgPool) {
    let g = G::new(pool).await;
    g.settings(json!({ "review_required": true })).await;
    let seq = g.sequence(json!([step1()])).await;
    let (_, e) = g.enroll_new(seq, "ana@acme.example").await;
    g.fake.push(Ok(slots_response(&[("opener", "First try.")])));
    g.gen_tick().await;
    let id = g.gen_id(e, 1).await;
    let rej = g.genok("POST", &format!("/v1/generator/generations/{id}/reject"), None).await;
    assert_eq!(rej["status"], "rejected");
    assert_eq!(g.fu_tick().await.waiting, 1, "a rejected text is not used; the step waits for the new one");
    assert_eq!(g.wait_reason(e).await.as_deref(), Some("generation_review"));
    g.fake.push(Ok(slots_response(&[("opener", "Second try.")])));
    let r = g.gen_tick().await;
    assert_eq!((r.discovered, r.pending_review), (1, 1));
    assert_eq!(g.fake.call_count(), 2);
    assert_eq!(g.gens(e).await, vec![(1, "pending_review".to_string())], "the same row, made again");
    let (slots,): (Value,) = sqlx::query_as("SELECT slots FROM generations WHERE id = $1").bind(id).fetch_one(&g.h.state.pool).await.unwrap();
    assert_eq!(slots["opener"], "Second try.");
    // approve all by filter
    let b = g.genok("POST", "/v1/generator/generations/bulk", Some(json!({ "action": "approve", "filter": { "sequence_id": seq } }))).await;
    assert_eq!((b["matched"].clone(), b["changed"].clone()), (json!(1), json!(1)));
    assert_eq!(g.fu_tick().await.queued, 1);
    assert_eq!(g.job_text(e, 1).await.1.as_deref(), Some("Hi Ana,\nSecond try.\nBye"));
}

// =====================================================================================================
// A changed step, a stop, a pause, an erase
// =====================================================================================================

#[sqlx::test]
async fn a_step_edited_after_generation_makes_the_text_stale_and_a_new_one_is_used(pool: PgPool) {
    let g = G::new(pool).await;
    let seq = g.sequence(json!([step1()])).await;
    let (_, e) = g.enroll_new(seq, "ana@acme.example").await;
    g.fake.push(Ok(slots_response(&[("opener", "Old text.")])));
    g.gen_tick().await;
    assert_eq!(g.gens(e).await, vec![(1, "ready".to_string())]);
    let (old_hash,): (String,) = sqlx::query_as("SELECT step_hash FROM sequence_steps WHERE sequence_id = $1").bind(seq).fetch_one(&g.h.state.pool).await.unwrap();

    let r = g.put_steps(seq, json!([ai_step(0, "Hello {{first_name|there}}", "Hello {{first_name|there}}!\n{{ai:opener}}\nCheers", opener())])).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let (new_hash,): (String,) = sqlx::query_as("SELECT step_hash FROM sequence_steps WHERE sequence_id = $1").bind(seq).fetch_one(&g.h.state.pool).await.unwrap();
    assert_ne!(old_hash, new_hash);

    g.fake.push(Ok(slots_response(&[("opener", "New text.")])));
    let r = g.gen_tick().await;
    assert_eq!((r.staled, r.discovered, r.ready), (1, 1, 1));
    assert_eq!(g.gens(e).await.len(), 2);
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT step_hash, status FROM generations WHERE enrollment_id = $1 ORDER BY seq").bind(e).fetch_all(&g.h.state.pool).await.unwrap();
    assert_eq!(rows, vec![(old_hash, "stale".to_string()), (new_hash, "ready".to_string())]);
    g.fu_tick().await;
    assert_eq!(g.job_text(e, 1).await.1.as_deref(), Some("Hello Ana!\nNew text.\nCheers"));
}

#[sqlx::test]
async fn the_old_text_is_never_used_for_an_edited_step(pool: PgPool) {
    let g = G::new(pool).await;
    g.settings(json!({ "due_grace_mins": 5 })).await;
    let seq = g.sequence(json!([step1()])).await;
    let (_, e) = g.enroll_new(seq, "ana@acme.example").await;
    g.gen_tick().await;
    // edited before the worker looked again: the scheduler does not know the old text for the new step
    g.put_steps(seq, json!([ai_step(0, "Hello", "Changed {{ai:opener}}", opener())])).await;
    assert_eq!(g.fu_tick().await.waiting, 1);
    assert!(g.jobs(e).await.is_empty());
    g.gen_tick().await;
    g.fu_tick().await;
    assert_eq!(g.job_text(e, 1).await.1.as_deref(), Some("Changed Fake text for opener."));
}

#[sqlx::test]
async fn a_reply_between_steps_cancels_the_text_and_no_call_is_made_for_that_step(pool: PgPool) {
    let g = G::new(pool).await;
    let seq = g.sequence(json!([step1(), ai_step(1, "", "Again {{ai:opener}}", opener())])).await;
    let lead = g.lead(JANE, "Jane").await;
    let e = g.enroll(seq, lead, None).await;
    let j1 = g.until_sent(e, 1).await;
    g.set_message_id(j1, FIXTURE_ID).await;
    // step 2 is due the next business day: the worker writes it within the look-ahead
    g.h.advance(Duration::hours(20));
    g.gen_tick().await;
    assert_eq!(g.gens(e).await, vec![(1, "ready".to_string()), (2, "ready".to_string())]);
    let before = g.fake.call_count();
    ingest_raw(&g.h.state, g.account, "p1", &fixture("human_plain.eml"), g.h.now()).await;
    assert_eq!(g.status(e).await, "stopped");
    assert_eq!(g.gens(e).await, vec![(1, "ready".to_string()), (2, "cancelled".to_string())], "the sent step keeps its row");
    g.h.advance(Duration::days(3));
    g.gen_tick().await;
    g.fu_tick().await;
    assert_eq!(g.fake.call_count(), before);
    assert_eq!(g.jobs(e).await.len(), 1);
}

#[sqlx::test]
async fn a_reply_before_the_text_is_written_means_no_call_at_all(pool: PgPool) {
    let g = G::new(pool).await;
    let seq = g.sequence(json!([step1(), ai_step(1, "", "Again {{ai:opener}}", opener())])).await;
    let lead = g.lead(JANE, "Jane").await;
    let e = g.enroll(seq, lead, None).await;
    let j1 = g.until_sent(e, 1).await;
    g.set_message_id(j1, FIXTURE_ID).await;
    let before = g.fake.call_count();
    ingest_raw(&g.h.state, g.account, "p1", &fixture("human_plain.eml"), g.h.now()).await;
    g.h.advance(Duration::days(3));
    let r = g.gen_tick().await;
    assert_eq!((r.discovered, r.claimed), (0, 0));
    assert_eq!(g.fake.call_count(), before, "no call for step 2");
    assert_eq!(count(&g.h.state, "SELECT count(*) FROM generations WHERE step_number = 2").await, 0);
}

#[sqlx::test]
async fn a_stopped_enrollment_cancels_its_pending_text(pool: PgPool) {
    let g = G::new(pool).await;
    g.genok("POST", "/v1/generator/pause", None).await;
    let seq = g.sequence(json!([step1()])).await;
    let (_, e) = g.enroll_new(seq, "ana@acme.example").await;
    g.genok("POST", "/v1/generator/resume", None).await;
    g.settings(json!({ "daily_token_budget": 1 })).await;
    sqlx::query("INSERT INTO llm_calls (purpose, model, input_tokens, output_tokens, ok, created_at) VALUES ('slot', 'm', 5, 0, true, $1)").bind(g.h.now()).execute(&g.h.state.pool).await.unwrap();
    g.gen_tick().await; // makes the row, the budget stops the call
    assert_eq!(g.gens(e).await, vec![(1, "pending".to_string())]);
    g.ok(Scope::Followups, "POST", &format!("/v1/followups/enrollments/{e}/stop"), None).await;
    assert_eq!(g.gens(e).await, vec![(1, "cancelled".to_string())]);
    g.settings(json!({ "daily_token_budget": 0 })).await;
    g.gen_tick().await;
    assert_eq!(g.fake.call_count(), 0);
}

#[sqlx::test]
async fn text_written_before_a_long_pause_is_made_again_after_the_resume(pool: PgPool) {
    let g = G::new(pool).await;
    let seq = g.sequence(json!([step1()])).await;
    let (_, e) = g.enroll_new(seq, "ana@acme.example").await;
    g.fake.push(Ok(slots_response(&[("opener", "Before the pause.")])));
    g.gen_tick().await;
    g.ok(Scope::Followups, "POST", &format!("/v1/followups/enrollments/{e}/pause"), None).await;
    // a short pause (1 hour, under twice the look-ahead): the text stays
    g.h.advance(Duration::hours(1));
    g.ok(Scope::Followups, "POST", &format!("/v1/followups/enrollments/{e}/resume"), None).await;
    assert_eq!(g.gens(e).await, vec![(1, "ready".to_string())]);
    // a long pause (13 hours, over 12): it is stale
    g.ok(Scope::Followups, "POST", &format!("/v1/followups/enrollments/{e}/pause"), None).await;
    g.h.advance(Duration::hours(13));
    g.ok(Scope::Followups, "POST", &format!("/v1/followups/enrollments/{e}/resume"), None).await;
    assert_eq!(g.gens(e).await, vec![(1, "stale".to_string())]);
    g.fake.push(Ok(slots_response(&[("opener", "After the pause.")])));
    let r = g.gen_tick().await;
    assert_eq!((r.discovered, r.ready), (1, 1), "the same row is made again");
    g.fu_tick().await;
    assert_eq!(g.job_text(e, 1).await.1.as_deref(), Some("Hi Ana,\nAfter the pause.\nBye"));
}

#[sqlx::test]
async fn erasing_a_lead_deletes_its_generations(pool: PgPool) {
    let g = G::new(pool).await;
    let seq = g.sequence(json!([step1()])).await;
    let (lead, e) = g.enroll_new(seq, "ana@acme.example").await;
    let (_, e2) = g.enroll_new(seq, "bob@acme.example").await;
    g.gen_tick().await;
    assert_eq!(count(&g.h.state, "SELECT count(*) FROM generations").await, 2);
    g.ok(Scope::Leads, "DELETE", &format!("/v1/leads/{lead}"), None).await;
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM generations WHERE lead_id = $1").bind(lead).fetch_one(&g.h.state.pool).await.unwrap();
    assert_eq!(n, 0, "the generations of an erased lead are gone");
    let left: Vec<(Uuid,)> = sqlx::query_as("SELECT enrollment_id FROM generations").fetch_all(&g.h.state.pool).await.unwrap();
    assert_eq!(left, vec![(e2,)], "only the other lead's text is left");
    let _ = e;
}
