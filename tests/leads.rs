//! Lead management: fields, settings, leads, lists, tags, bulk actions, timeline.
mod common;
use aikido_email::auth::api_keys::Scope;
use aikido_email::followups::scheduler::{self, FollowupEngine};
use aikido_email::leads::verify::{MxAnswer, fake::FakeMx};
use axum::http::StatusCode;
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

struct L {
    h: Harness,
    keys: HashMap<Scope, String>,
    mx: FakeMx,
}

impl L {
    async fn new(pool: PgPool) -> Self {
        let mut h = Harness::new(pool);
        let mx = FakeMx::new(MxAnswer::Has);
        h.state.mx = Arc::new(mx.clone());
        let keys = bootstrap_keys(&h.state).await;
        Self { h, keys, mx }
    }

    async fn api(&self, method: &str, uri: &str, body: Option<Value>) -> Response {
        call(&self.h.state, method, uri, Some(&self.keys[&Scope::Leads]), body).await
    }

    async fn ok(&self, method: &str, uri: &str, body: Option<Value>) -> Value {
        let r = self.api(method, uri, body).await;
        assert!(r.status.is_success(), "{method} {uri} -> {} {}", r.status, r.body);
        r.body
    }

    async fn lead(&self, email: &str) -> Uuid {
        let r = self.ok("POST", "/v1/leads", Some(json!({ "email": email }))).await;
        r.get("id").unwrap().as_str().unwrap().parse().unwrap()
    }

    async fn field(&self, key: &str, kind: &str) {
        self.ok("POST", "/v1/leads/fields", Some(json!({ "key": key, "kind": kind }))).await;
    }

    async fn list(&self, name: &str) -> Uuid {
        self.ok("POST", "/v1/leads/lists", Some(json!({ "name": name }))).await["id"].as_str().unwrap().parse().unwrap()
    }

    async fn events(&self, id: Uuid) -> Vec<String> {
        let r = self.ok("GET", &format!("/v1/leads/{id}/events?limit=500"), None).await;
        r["events"].as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap().to_string()).collect()
    }
}

fn id_of(v: &Value) -> Uuid {
    v["id"].as_str().unwrap().parse().unwrap()
}

// ---------- phase 1: scopes, settings, fields ----------

#[sqlx::test]
async fn new_scopes_guard_the_routes(pool: PgPool) {
    let l = L::new(pool).await;
    assert_eq!(call(&l.h.state, "GET", "/v1/leads", None, None).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(call(&l.h.state, "GET", "/v1/leads", Some(&l.keys[&Scope::Sending]), None).await.status, StatusCode::FORBIDDEN);
    assert_eq!(call(&l.h.state, "GET", "/v1/leads", Some(&l.keys[&Scope::Campaigns]), None).await.status, StatusCode::FORBIDDEN);
    assert_eq!(call(&l.h.state, "GET", "/v1/leads", Some(&l.keys[&Scope::Admin]), None).await.status, StatusCode::OK);
    assert_eq!(l.api("GET", "/v1/leads", None).await.status, StatusCode::OK);
}

#[sqlx::test]
async fn migration_adds_only_new_tables(pool: PgPool) {
    let tables: Vec<(String,)> = sqlx::query_as("SELECT table_name::text FROM information_schema.tables WHERE table_schema = 'public' AND table_name LIKE 'lead%' ORDER BY 1")
        .fetch_all(&pool)
        .await
        .unwrap();
    let names: Vec<&str> = tables.iter().map(|t| t.0.as_str()).collect();
    assert_eq!(names, ["lead_defaults", "lead_email_runs", "lead_emails", "lead_events", "lead_fields", "lead_import_files", "lead_imports", "lead_list_members", "lead_list_rows", "lead_lists", "leads"]);
    // the old tables did not change
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM information_schema.columns WHERE table_name = 'enrollments'").fetch_one(&pool).await.unwrap();
    // 16 before the lead tables; the Email Generator (0006) added wait_reason and wait_since
    assert_eq!(n, 18, "enrollments has the same columns as before, plus the two wait columns");
}

#[sqlx::test]
async fn settings_read_patch_and_validation(pool: PgPool) {
    let l = L::new(pool).await;
    let r = l.ok("GET", "/v1/leads/settings", None).await;
    assert_eq!(r["settings"]["import_max_rows"], 100_000);
    assert_eq!(r["settings"]["default_dedupe_mode"], "fill_empty");
    assert_eq!(r["settings"]["default_daily_new_leads"], 50);
    let r = l.ok("PATCH", "/v1/leads/settings", Some(json!({ "mx_check": false, "free_mail_domains_extra": ["Acme.io"] }))).await;
    assert_eq!(r["settings"]["mx_check"], false);
    assert_eq!(r["settings"]["free_mail_domains_extra"], json!(["acme.io"]));
    assert_eq!(l.ok("GET", "/v1/leads/settings", None).await["settings"]["mx_check"], false);
    let bad = l.api("PATCH", "/v1/leads/settings", Some(json!({ "import_max_rows": 0 }))).await;
    assert_eq!(bad.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(bad.body["error"]["code"], "invalid_setting");
    assert_eq!(l.api("PATCH", "/v1/leads/settings", Some(json!({ "nope": 1 }))).await.body["error"]["code"], "unknown_setting");
}

#[sqlx::test]
async fn fields_create_patch_delete(pool: PgPool) {
    let l = L::new(pool).await;
    for bad in ["Pain", "1x", "pain-point", "email", "first_name", "tags", ""] {
        let r = l.api("POST", "/v1/leads/fields", Some(json!({ "key": bad }))).await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
        assert_eq!(r.body["error"]["code"], "invalid_key");
    }
    let r = l.api("POST", "/v1/leads/fields", Some(json!({ "key": "pain_point", "label": "Pain point", "fallback": "growth" }))).await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_eq!(r.body["kind"], "text");
    assert_eq!(l.api("POST", "/v1/leads/fields", Some(json!({ "key": "pain_point" }))).await.status, StatusCode::CONFLICT);

    let r = l.ok("PATCH", "/v1/leads/fields/pain_point", Some(json!({ "label": "Pain", "fallback": null }))).await;
    assert_eq!(r["label"], "Pain");
    assert_eq!(r["fallback"], Value::Null);
    assert_eq!(l.api("PATCH", "/v1/leads/fields/pain_point", Some(json!({ "kind": "number" }))).await.body["error"]["code"], "field_locked");
    assert_eq!(l.api("PATCH", "/v1/leads/fields/nope", Some(json!({ "label": "x" }))).await.status, StatusCode::NOT_FOUND);

    let id = l.lead("a@example.com").await;
    l.ok("PATCH", &format!("/v1/leads/{id}"), Some(json!({ "custom_fields": { "pain_point": "slow sales" } }))).await;
    let r = l.ok("DELETE", "/v1/leads/fields/pain_point", None).await;
    assert_eq!(r["leads_cleaned"], 1);
    assert_eq!(l.ok("GET", &format!("/v1/leads/{id}"), None).await["custom_fields"], json!({}));
    assert_eq!(l.ok("GET", "/v1/leads/fields", None).await, json!([]));
    assert_eq!(l.api("DELETE", "/v1/leads/fields/pain_point", None).await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn at_most_200_fields(pool: PgPool) {
    let l = L::new(pool).await;
    for i in 0..200 {
        l.field(&format!("f{i}"), "text").await;
    }
    let r = l.api("POST", "/v1/leads/fields", Some(json!({ "key": "one_more" }))).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.body["error"]["code"], "too_many_fields");
}

// ---------- phase 2: create, read, update, delete, search, paging ----------

#[sqlx::test]
async fn create_normalizes_email_and_refuses_the_same_email_in_other_case(pool: PgPool) {
    let l = L::new(pool).await;
    let r = l.api("POST", "/v1/leads", Some(json!({ "email": "  John@Example.COM ", "first_name": " John ", "tags": ["SaaS", "saas"] }))).await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
    assert_eq!(r.body["email"], "john@example.com");
    assert_eq!(r.body["email_domain"], "example.com");
    assert_eq!(r.body["first_name"], "John");
    assert_eq!(r.body["tags"], json!(["saas"]));
    assert_eq!(r.body["contact_state"], "new");
    assert_eq!(r.body["source"], "api");
    assert_eq!(r.body["verification"], "mx_ok", "the MX check ran");
    assert_eq!(r.body["suppressed"], false);
    assert_eq!(r.body["active_enrollment_id"], Value::Null);
    assert_eq!(r.body["lists"], json!([]));

    let again = l.api("POST", "/v1/leads", Some(json!({ "email": "JOHN@example.com" }))).await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(again.body["error"]["code"], "email_exists");
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM leads").fetch_one(&l.h.state.pool).await.unwrap();
    assert_eq!(n, 1);
}

#[sqlx::test]
async fn create_checks_values(pool: PgPool) {
    let l = L::new(pool).await;
    l.field("mrr", "number").await;
    for (body, what) in [
        (json!({ "email": "nope" }), "email"),
        (json!({ "email": "a@b.com", "timezone": "Mars/Base" }), "time zone"),
        (json!({ "email": "a@b.com", "custom_fields": { "unknown": "x" } }), "unknown custom field"),
        (json!({ "email": "a@b.com", "custom_fields": { "mrr": "lots" } }), "not a number"),
        (json!({ "email": "a@b.com", "first_name": "x".repeat(1001) }), "longer"),
        (json!({ "email": "a@b.com", "interest": "maybe" }), "interest"),
    ] {
        let r = l.api("POST", "/v1/leads", Some(body)).await;
        assert!(r.status == StatusCode::UNPROCESSABLE_ENTITY, "{what}: {} {}", r.status, r.body);
    }
    let ok = l.api("POST", "/v1/leads", Some(json!({ "email": "a@b.com", "timezone": "Asia/Dhaka", "custom_fields": { "mrr": 300 }, "interest": "interested" }))).await;
    assert_eq!(ok.status, StatusCode::CREATED, "{}", ok.body);
    assert_eq!(ok.body["custom_fields"], json!({ "mrr": "300" }));
    assert_eq!(ok.body["interest"], "interested");
    assert_eq!(ok.body["timezone"], "Asia/Dhaka");
}

#[sqlx::test]
async fn get_unknown_lead_is_404_and_timeline_starts_with_created(pool: PgPool) {
    let l = L::new(pool).await;
    assert_eq!(l.api("GET", &format!("/v1/leads/{}", Uuid::now_v7()), None).await.status, StatusCode::NOT_FOUND);
    assert_eq!(l.api("GET", "/v1/leads/not-a-uuid", None).await.status, StatusCode::BAD_REQUEST);
    let id = l.lead("a@example.com").await;
    assert_eq!(l.events(id).await, vec!["created"]);
}

#[sqlx::test]
async fn patch_changes_clears_and_logs(pool: PgPool) {
    let l = L::new(pool).await;
    l.field("pain_point", "text").await;
    l.field("mrr", "number").await;
    let id = l.lead("a@example.com").await;
    let url = format!("/v1/leads/{id}");
    let r = l.ok("PATCH", &url, Some(json!({ "first_name": "Ana", "company_name": "Acme", "tags": ["a", "b"], "notes": "hi", "custom_fields": { "pain_point": "slow", "mrr": 5 } }))).await;
    assert_eq!(r["first_name"], "Ana");
    assert_eq!(r["custom_fields"], json!({ "pain_point": "slow", "mrr": "5" }));
    assert_eq!(r["tags"], json!(["a", "b"]));

    // null and empty text clear. custom_fields merge, and null removes a key.
    let r = l.ok("PATCH", &url, Some(json!({ "first_name": null, "company_name": "  ", "custom_fields": { "mrr": null } }))).await;
    assert_eq!(r["first_name"], Value::Null);
    assert_eq!(r["company_name"], Value::Null);
    assert_eq!(r["custom_fields"], json!({ "pain_point": "slow" }));
    assert_eq!(r["notes"], "hi");

    let r = l.ok("PATCH", &url, Some(json!({ "interest": "meeting_booked" }))).await;
    assert_eq!(r["interest"], "meeting_booked");
    let r = l.ok("PATCH", &url, Some(json!({ "interest": null }))).await;
    assert_eq!(r["interest"], Value::Null);
    let events = l.ok("GET", &format!("{url}/events"), None).await;
    let kinds: Vec<&str> = events["events"].as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["interest_changed", "interest_changed", "updated", "updated", "created"]);
    assert_eq!(events["events"][1]["detail"], json!({ "from": null, "to": "meeting_booked" }));

    for bad in [json!({ "contact_state": "replied" }), json!({ "verification": "mx_ok" }), json!({ "id": "x" }), json!({ "tags": "a" }), json!([1])] {
        assert_eq!(l.api("PATCH", &url, Some(bad)).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    }
}

#[sqlx::test]
async fn email_change_rules(pool: PgPool) {
    let l = L::new(pool).await;
    let a = l.lead("a@example.com").await;
    l.lead("b@example.com").await;
    let r = l.api("PATCH", &format!("/v1/leads/{a}"), Some(json!({ "email": "B@example.com" }))).await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::CONFLICT, Some("email_exists")));
    let r = l.ok("PATCH", &format!("/v1/leads/{a}"), Some(json!({ "email": " New@Other.io " }))).await;
    assert_eq!((r["email"].as_str(), r["email_domain"].as_str()), (Some("new@other.io"), Some("other.io")));

    // an open enrollment locks the email
    let acct = l.h.account("sender@mail.test").await;
    l.h.settings(acct, json!({ "reply_check_max_age_mins": 0 })).await;
    let fkey = &l.keys[&Scope::Followups];
    let seq = call(&l.h.state, "POST", "/v1/followups/sequences", Some(fkey), Some(json!({ "name": "S", "steps": [{ "subject": "Hi", "body_text": "Hello" }] }))).await;
    let seq = seq.body["sequence"]["id"].as_str().unwrap().to_string();
    let en = call(&l.h.state, "POST", &format!("/v1/followups/sequences/{seq}/enrollments"), Some(fkey),
        Some(json!({ "enrollments": [{ "to_email": "new@other.io", "account_id": acct }] }))).await;
    assert_eq!(en.body["enrolled"], 1, "{}", en.body);
    let r = l.api("PATCH", &format!("/v1/leads/{a}"), Some(json!({ "email": "later@other.io" }))).await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::CONFLICT, Some("email_locked")));
    let r = l.ok("GET", &format!("/v1/leads/{a}"), None).await;
    assert!(r["active_enrollment_id"].is_string());
    // other fields can still change
    l.ok("PATCH", &format!("/v1/leads/{a}"), Some(json!({ "first_name": "Ok" }))).await;
}

#[sqlx::test]
async fn delete_erases_stops_enrollment_cancels_job_and_suppresses(pool: PgPool) {
    let l = L::new(pool).await;
    let id = l.lead("gone@example.com").await;
    let other = l.lead("stays@example.com").await;
    let acct = l.h.account("sender@mail.test").await;
    l.h.settings(acct, json!({ "reply_check_max_age_mins": 0 })).await;
    let fkey = &l.keys[&Scope::Followups];
    let seq = call(&l.h.state, "POST", "/v1/followups/sequences", Some(fkey), Some(json!({ "name": "S", "steps": [{ "subject": "Hi", "body_text": "Hello" }] }))).await;
    let seq = seq.body["sequence"]["id"].as_str().unwrap().to_string();
    let en = call(&l.h.state, "POST", &format!("/v1/followups/sequences/{seq}/enrollments"), Some(fkey),
        Some(json!({ "enrollments": [{ "to_email": "gone@example.com", "account_id": acct }] }))).await;
    let enrollment = en.body["items"][0]["enrollment"]["id"].as_str().unwrap().to_string();
    // the scheduler makes the job of step 1; it waits in the queue
    let fe = FollowupEngine::new(l.h.state.clone());
    scheduler::tick(&fe).await.unwrap();
    let (job, status): (Uuid, String) = sqlx::query_as("SELECT id, status::text FROM send_jobs WHERE enrollment_id = $1").bind(enrollment.parse::<Uuid>().unwrap()).fetch_one(&l.h.state.pool).await.unwrap();
    assert_eq!(status, "queued");

    let r = l.ok("DELETE", &format!("/v1/leads/{id}"), None).await;
    assert_eq!(r, json!({ "deleted": 1, "stopped_enrollments": 1, "cancelled_jobs": 1 }));
    assert_eq!(l.api("GET", &format!("/v1/leads/{id}"), None).await.status, StatusCode::NOT_FOUND);
    assert_eq!(l.api("DELETE", &format!("/v1/leads/{id}"), None).await.status, StatusCode::NOT_FOUND);
    let (st,): (String,) = sqlx::query_as("SELECT status::text FROM send_jobs WHERE id = $1").bind(job).fetch_one(&l.h.state.pool).await.unwrap();
    assert_eq!(st, "cancelled");
    let (est, reason): (String, Option<String>) = sqlx::query_as("SELECT status::text, stop_reason FROM enrollments WHERE id = $1").bind(enrollment.parse::<Uuid>().unwrap()).fetch_one(&l.h.state.pool).await.unwrap();
    assert_eq!((est.as_str(), reason.as_deref()), ("stopped", Some("lead_deleted")));
    // never contacted again: the address is suppressed with reason erased
    let (reason,): (String,) = sqlx::query_as("SELECT reason::text FROM suppressions WHERE value = 'gone@example.com' AND kind = 'email'").fetch_one(&l.h.state.pool).await.unwrap();
    assert_eq!(reason, "erased");
    let (events,): (i64,) = sqlx::query_as("SELECT count(*) FROM lead_events WHERE lead_id = $1").bind(id).fetch_one(&l.h.state.pool).await.unwrap();
    assert_eq!(events, 0, "the timeline went with the lead");
    // imported again: saved, but it shows as suppressed
    let again = l.ok("POST", "/v1/leads", Some(json!({ "email": "gone@example.com" }))).await;
    assert_eq!(again["suppressed"], true);
    assert!(l.api("GET", &format!("/v1/leads/{other}"), None).await.status.is_success());
}

#[sqlx::test]
async fn delete_keeps_an_older_suppression_reason(pool: PgPool) {
    let l = L::new(pool).await;
    let id = l.lead("unsub@example.com").await;
    sqlx::query("INSERT INTO suppressions (value, kind, reason) VALUES ('unsub@example.com', 'email', 'unsubscribed')").execute(&l.h.state.pool).await.unwrap();
    l.ok("DELETE", &format!("/v1/leads/{id}"), None).await;
    let (reason,): (String,) = sqlx::query_as("SELECT reason::text FROM suppressions WHERE value = 'unsub@example.com'").fetch_one(&l.h.state.pool).await.unwrap();
    assert_eq!(reason, "unsubscribed");
}

async fn seed(pool: &PgPool, n: i32) {
    sqlx::query(
        "INSERT INTO leads (id, email, email_domain, first_name, last_name, company_name, verification, source, created_at, updated_at)
         SELECT gen_random_uuid(), 'user' || g || '@company' || (g % 500) || '.com', 'company' || (g % 500) || '.com',
                (ARRAY['Ana','Ben','Chen','Dina','Eli','Fay'])[1 + g % 6], 'Last' || g, 'Company ' || (g % 500) || ' Ltd', 'mx_ok', 'api',
                timestamptz '2026-01-01' + g * interval '1 minute', timestamptz '2026-01-01' + g * interval '1 minute'
           FROM generate_series(1, $1) g",
    )
    .bind(n)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("ANALYZE leads").execute(pool).await.unwrap();
}

#[sqlx::test]
async fn search_10000_leads_is_fast_and_uses_the_trigram_index(pool: PgPool) {
    let l = L::new(pool).await;
    seed(&l.h.state.pool, 10_000).await;
    // the same query the route runs, with EXPLAIN
    // (with 10000 rows the planner likes a plain scan, so the test forbids it (and the plain index walk) to see that the indexes can serve the query)
    let mut tx = l.h.state.pool.begin().await.unwrap();
    sqlx::query("SET LOCAL enable_seqscan = off").execute(&mut *tx).await.unwrap();
    sqlx::query("SET LOCAL enable_indexscan = off").execute(&mut *tx).await.unwrap();
    let plan: Vec<(String,)> = sqlx::query_as(
        "EXPLAIN SELECT id FROM leads WHERE (email ILIKE '%user777@%' OR first_name ILIKE '%user777@%' OR last_name ILIKE '%user777@%' OR company_name ILIKE '%user777@%')
         ORDER BY created_at DESC, id DESC LIMIT 51",
    )
    .fetch_all(&mut *tx)
    .await
    .unwrap();
    drop(tx);
    let plan = plan.into_iter().map(|r| r.0).collect::<Vec<_>>().join("\n");
    assert!(plan.contains("leads_email_trgm_idx") && plan.contains("leads_company_trgm_idx"), "plan: {plan}");

    let started = std::time::Instant::now();
    let r = l.ok("GET", "/v1/leads?q=user777%40", None).await;
    let took = started.elapsed();
    assert_eq!(r["leads"].as_array().unwrap().len(), 1);
    assert!(took < std::time::Duration::from_millis(100), "search took {took:?}");
    // part of a company name, and a LIKE character that is plain text
    let r = l.ok("GET", "/v1/leads?q=company+42+ltd&limit=500", None).await;
    assert_eq!(r["leads"].as_array().unwrap().len(), 20);
    assert_eq!(l.ok("GET", "/v1/leads?q=%25", None).await["leads"].as_array().unwrap().len(), 0, "% is not a wildcard");
}

#[sqlx::test]
async fn keyset_paging_walks_every_lead_once(pool: PgPool) {
    let l = L::new(pool).await;
    seed(&l.h.state.pool, 95).await;
    // two leads share one created_at: the id breaks the tie
    sqlx::query("UPDATE leads SET created_at = (SELECT created_at FROM leads ORDER BY created_at LIMIT 1) WHERE email IN ('user50@company50.com', 'user51@company51.com')")
        .execute(&l.h.state.pool).await.unwrap();
    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let url = match &cursor {
            Some(c) => format!("/v1/leads?limit=10&cursor={c}"),
            None => "/v1/leads?limit=10".to_string(),
        };
        let r = l.ok("GET", &url, None).await;
        pages += 1;
        seen.extend(r["leads"].as_array().unwrap().iter().map(|x| x["id"].as_str().unwrap().to_string()));
        match r["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    assert_eq!(pages, 10);
    assert_eq!(seen.len(), 95);
    let unique: std::collections::HashSet<_> = seen.iter().collect();
    assert_eq!(unique.len(), 95);
    // newest first
    let first = l.ok("GET", "/v1/leads?limit=1", None).await;
    assert_eq!(first["leads"][0]["email"], "user95@company95.com");
    let r = l.api("GET", "/v1/leads?limit=0", None).await;
    assert_eq!(r.body["error"]["code"], "invalid_limit");
    assert_eq!(l.api("GET", "/v1/leads?limit=501", None).await.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test]
async fn deleted_cursor_lead_is_invalid_cursor(pool: PgPool) {
    let l = L::new(pool).await;
    let a = l.lead("a@example.com").await;
    l.lead("b@example.com").await;
    l.ok("DELETE", &format!("/v1/leads/{a}"), None).await;
    let r = l.api("GET", &format!("/v1/leads?cursor={a}"), None).await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_cursor")));
}

#[sqlx::test]
async fn filters(pool: PgPool) {
    let l = L::new(pool).await;
    let a = l.lead("a@one.com").await;
    let b = l.lead("b@two.com").await;
    l.lead("c@three.com").await;
    let list = l.list("L").await;
    l.ok("PATCH", &format!("/v1/leads/{a}"), Some(json!({ "tags": ["vip", "saas"], "interest": "interested" }))).await;
    l.ok("POST", "/v1/leads/actions", Some(json!({ "action": "add_to_list", "lead_ids": [a, b], "list_id": list }))).await;
    sqlx::query("UPDATE leads SET contact_state = 'replied' WHERE id = $1").bind(b).execute(&l.h.state.pool).await.unwrap();
    sqlx::query("UPDATE leads SET verification = 'no_mx' WHERE email = 'c@three.com'").execute(&l.h.state.pool).await.unwrap();
    sqlx::query("INSERT INTO suppressions (value, kind, reason) VALUES ('two.com', 'domain', 'manual')").execute(&l.h.state.pool).await.unwrap();

    let emails = |r: &Value| -> Vec<String> { let mut v: Vec<String> = r["leads"].as_array().unwrap().iter().map(|x| x["email"].as_str().unwrap().to_string()).collect(); v.sort(); v };
    assert_eq!(emails(&l.ok("GET", "/v1/leads?tag=VIP", None).await), ["a@one.com"]);
    assert_eq!(emails(&l.ok("GET", &format!("/v1/leads?list_id={list}"), None).await), ["a@one.com", "b@two.com"]);
    assert_eq!(emails(&l.ok("GET", "/v1/leads?contact_state=replied", None).await), ["b@two.com"]);
    assert_eq!(emails(&l.ok("GET", "/v1/leads?interest=interested", None).await), ["a@one.com"]);
    assert_eq!(emails(&l.ok("GET", "/v1/leads?verification=no_mx", None).await), ["c@three.com"]);
    assert_eq!(emails(&l.ok("GET", "/v1/leads?suppressed=true", None).await), ["b@two.com"]);
    assert_eq!(emails(&l.ok("GET", "/v1/leads?suppressed=false", None).await), ["a@one.com", "c@three.com"]);
    assert_eq!(emails(&l.ok("GET", "/v1/leads?created_from=2999-01-01T00:00:00Z", None).await), Vec::<String>::new());
    assert_eq!(emails(&l.ok("GET", "/v1/leads?created_to=2000-01-01T00:00:00Z", None).await), Vec::<String>::new());
    assert_eq!(emails(&l.ok("GET", "/v1/leads?q=TWO&tag=nope", None).await), Vec::<String>::new());
    assert_eq!(l.api("GET", "/v1/leads?contact_state=zzz", None).await.status, StatusCode::BAD_REQUEST);
    assert_eq!(l.ok("GET", &format!("/v1/leads/{b}"), None).await["suppressed"], true);
}

// ---------- phase 3: lists, tags, bulk actions, notes ----------

#[sqlx::test]
async fn lists_crud(pool: PgPool) {
    let l = L::new(pool).await;
    let a = l.list("Prospects").await;
    assert_eq!(l.api("POST", "/v1/leads/lists", Some(json!({ "name": "Prospects" }))).await.status, StatusCode::CONFLICT);
    assert_eq!(l.api("POST", "/v1/leads/lists", Some(json!({ "name": " " }))).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    let b = l.list("Other").await;
    assert_eq!(l.api("PATCH", &format!("/v1/leads/lists/{b}"), Some(json!({ "name": "Prospects" }))).await.status, StatusCode::CONFLICT);
    let r = l.ok("PATCH", &format!("/v1/leads/lists/{a}"), Some(json!({ "name": "Hot", "description": "d" }))).await;
    assert_eq!((r["name"].as_str(), r["description"].as_str()), (Some("Hot"), Some("d")));
    let lead = l.lead("a@example.com").await;
    l.ok("POST", "/v1/leads/actions", Some(json!({ "action": "add_to_list", "lead_ids": [lead], "list_id": a }))).await;
    let r = l.ok("GET", "/v1/leads/lists", None).await;
    assert_eq!(r[0]["name"], "Hot");
    assert_eq!(r[0]["member_count"], 1);
    assert_eq!(l.ok("GET", &format!("/v1/leads/{lead}"), None).await["lists"][0]["name"], "Hot");
    // deleting a list keeps the leads
    l.ok("DELETE", &format!("/v1/leads/lists/{a}"), None).await;
    assert_eq!(l.api("DELETE", &format!("/v1/leads/lists/{a}"), None).await.status, StatusCode::NOT_FOUND);
    assert!(l.api("GET", &format!("/v1/leads/{lead}"), None).await.status.is_success());
    assert_eq!(l.ok("GET", &format!("/v1/leads/{lead}"), None).await["lists"], json!([]));
}

#[sqlx::test]
async fn action_by_filter_changes_the_right_rows_only(pool: PgPool) {
    let l = L::new(pool).await;
    let list = l.list("L").await;
    for i in 0..6 {
        l.ok("POST", "/v1/leads", Some(json!({ "email": format!("u{i}@acme.com"), "company_name": "Acme" }))).await;
    }
    for i in 0..4 {
        l.ok("POST", "/v1/leads", Some(json!({ "email": format!("v{i}@other.com"), "company_name": "Other" }))).await;
    }
    let r = l.ok("POST", "/v1/leads/actions", Some(json!({ "action": "add_to_list", "list_id": list, "filter": { "q": "acme" } }))).await;
    assert_eq!((r["matched"].as_i64(), r["changed"].as_i64()), (Some(6), Some(6)));
    // again: nothing new
    let r = l.ok("POST", "/v1/leads/actions", Some(json!({ "action": "add_to_list", "list_id": list, "filter": { "q": "acme" } }))).await;
    assert_eq!((r["matched"].as_i64(), r["changed"].as_i64()), (Some(6), Some(0)));
    let r = l.ok("POST", "/v1/leads/actions", Some(json!({ "action": "add_tags", "tags": ["Hot", "q4"], "filter": { "list_id": list } }))).await;
    assert_eq!(r["changed"], 6);
    assert_eq!(l.ok("GET", "/v1/leads?tag=hot&limit=500", None).await["leads"].as_array().unwrap().len(), 6);
    assert_eq!(l.ok("GET", "/v1/leads?tag=hot&q=other", None).await["leads"].as_array().unwrap().len(), 0);
    let r = l.ok("POST", "/v1/leads/actions", Some(json!({ "action": "remove_tags", "tags": ["hot"], "filter": { "q": "acme" } }))).await;
    assert_eq!(r["changed"], 6);
    assert_eq!(l.ok("GET", "/v1/leads?tag=hot", None).await["leads"].as_array().unwrap().len(), 0);
    assert_eq!(l.ok("GET", "/v1/leads?tag=q4&limit=500", None).await["leads"].as_array().unwrap().len(), 6);
    let r = l.ok("POST", "/v1/leads/actions", Some(json!({ "action": "set_interest", "interest": "won", "filter": { "tag": "q4" } }))).await;
    assert_eq!(r["changed"], 6);
    assert_eq!(l.ok("GET", "/v1/leads?interest=won&limit=500", None).await["leads"].as_array().unwrap().len(), 6);
    let r = l.ok("POST", "/v1/leads/actions", Some(json!({ "action": "set_interest", "filter": { "interest": "won" } }))).await;
    assert_eq!(r["changed"], 6, "no interest clears it");
    let r = l.ok("POST", "/v1/leads/actions", Some(json!({ "action": "remove_from_list", "list_id": list, "filter": { "q": "u1@" } }))).await;
    assert_eq!(r["changed"], 1);
    let r = l.ok("POST", "/v1/leads/actions", Some(json!({ "action": "delete", "filter": { "q": "other.com" } }))).await;
    assert_eq!(r["erased"]["deleted"], 4);
    assert_eq!(l.ok("GET", "/v1/leads?limit=500", None).await["leads"].as_array().unwrap().len(), 6);
}

#[sqlx::test]
async fn action_selection_rules(pool: PgPool) {
    let l = L::new(pool).await;
    let a = l.lead("a@example.com").await;
    let code = |r: &Response| r.body["error"]["code"].as_str().unwrap_or("").to_string();
    let r = l.api("POST", "/v1/leads/actions", Some(json!({ "action": "delete" }))).await;
    assert_eq!((r.status, code(&r).as_str()), (StatusCode::UNPROCESSABLE_ENTITY, "bad_selection"));
    let r = l.api("POST", "/v1/leads/actions", Some(json!({ "action": "delete", "lead_ids": [a], "filter": { "q": "a" } }))).await;
    assert_eq!(code(&r), "bad_selection");
    let r = l.api("POST", "/v1/leads/actions", Some(json!({ "action": "delete", "filter": {} }))).await;
    assert_eq!(code(&r), "empty_selection", "an empty filter must not delete everything");
    let r = l.api("POST", "/v1/leads/actions", Some(json!({ "action": "delete", "lead_ids": [] }))).await;
    assert_eq!(code(&r), "empty_selection");
    let r = l.api("POST", "/v1/leads/actions", Some(json!({ "action": "add_to_list", "lead_ids": [a] }))).await;
    assert_eq!(code(&r), "list_required");
    let r = l.api("POST", "/v1/leads/actions", Some(json!({ "action": "add_to_list", "lead_ids": [a], "list_id": Uuid::now_v7() }))).await;
    assert_eq!((r.status, code(&r).as_str()), (StatusCode::NOT_FOUND, "list_not_found"));
    let r = l.api("POST", "/v1/leads/actions", Some(json!({ "action": "add_tags", "lead_ids": [a], "tags": [] }))).await;
    assert_eq!(code(&r), "tags_required");
    let many: Vec<Uuid> = (0..10_001).map(|_| Uuid::now_v7()).collect();
    let r = l.api("POST", "/v1/leads/actions", Some(json!({ "action": "delete", "lead_ids": many }))).await;
    assert_eq!(code(&r), "too_many_leads");
    // unknown ids are just not matched
    let r = l.ok("POST", "/v1/leads/actions", Some(json!({ "action": "add_tags", "lead_ids": [a, Uuid::now_v7()], "tags": ["x"] }))).await;
    assert_eq!((r["matched"].as_i64(), r["changed"].as_i64()), (Some(1), Some(1)));
}

#[sqlx::test]
async fn add_tags_respects_the_limit_of_20(pool: PgPool) {
    let l = L::new(pool).await;
    let a = l.lead("a@example.com").await;
    let b = l.lead("b@example.com").await;
    let full: Vec<String> = (0..20).map(|i| format!("t{i}")).collect();
    l.ok("PATCH", &format!("/v1/leads/{a}"), Some(json!({ "tags": full }))).await;
    let r = l.ok("POST", "/v1/leads/actions", Some(json!({ "action": "add_tags", "lead_ids": [a, b], "tags": ["extra"] }))).await;
    assert_eq!((r["matched"].as_i64(), r["changed"].as_i64()), (Some(2), Some(1)), "the full lead is left alone");
    assert_eq!(l.ok("GET", &format!("/v1/leads/{a}"), None).await["tags"].as_array().unwrap().len(), 20);
    let too_many: Vec<String> = (0..21).map(|i| format!("t{i}")).collect();
    assert_eq!(l.api("PATCH", &format!("/v1/leads/{b}"), Some(json!({ "tags": too_many }))).await.status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[sqlx::test]
async fn timeline_records_changes_and_notes_and_pages(pool: PgPool) {
    let l = L::new(pool).await;
    let a = l.lead("a@example.com").await;
    let list = l.list("L").await;
    l.ok("POST", "/v1/leads/actions", Some(json!({ "action": "add_to_list", "lead_ids": [a], "list_id": list }))).await;
    l.ok("POST", "/v1/leads/actions", Some(json!({ "action": "remove_from_list", "lead_ids": [a], "list_id": list }))).await;
    let note = l.api("POST", &format!("/v1/leads/{a}/notes"), Some(json!({ "text": "  Called, call back Monday  " }))).await;
    assert_eq!(note.status, StatusCode::CREATED);
    assert_eq!(note.body["kind"], "note");
    assert_eq!(note.body["detail"]["text"], "Called, call back Monday");
    assert_eq!(l.api("POST", &format!("/v1/leads/{a}/notes"), Some(json!({ "text": " " }))).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(l.api("POST", &format!("/v1/leads/{}/notes", Uuid::now_v7()), Some(json!({ "text": "x" }))).await.status, StatusCode::NOT_FOUND);
    assert_eq!(l.events(a).await, ["note", "list_removed", "list_added", "created"]);
    // paging: 2 per page
    let p1 = l.ok("GET", &format!("/v1/leads/{a}/events?limit=3"), None).await;
    assert_eq!(p1["events"].as_array().unwrap().len(), 3);
    let cur = p1["next_cursor"].as_i64().unwrap();
    let p2 = l.ok("GET", &format!("/v1/leads/{a}/events?limit=3&cursor={cur}"), None).await;
    assert_eq!(p2["events"].as_array().unwrap().len(), 1);
    assert_eq!(p2["next_cursor"], Value::Null);
    assert_eq!(l.api("GET", &format!("/v1/leads/{}/events", Uuid::now_v7()), None).await.status, StatusCode::NOT_FOUND);
}

// ---------- the JSON bulk route ----------

#[sqlx::test]
async fn bulk_upsert_modes_and_per_item_results(pool: PgPool) {
    let l = L::new(pool).await;
    l.field("pain_point", "text").await;
    let list = l.list("Batch").await;
    l.ok("POST", "/v1/leads", Some(json!({ "email": "old@example.com", "first_name": "Old", "custom_fields": { "pain_point": "keep" }, "tags": ["a"] }))).await;

    let body = |mode: &str| json!({
        "dedupe_mode": mode, "list_id": list, "tags": ["Batch1"],
        "leads": [
            { "email": "old@example.com", "first_name": "New", "last_name": "Name", "custom_fields": { "pain_point": "changed" }, "tags": ["b"] },
            { "email": "fresh@example.com", "first_name": "Fresh" },
            { "email": "FRESH@example.com", "first_name": "Dup" },
            { "email": "bad-email" },
            { "email": "x@y.com", "custom_fields": { "nope": 1 } },
        ]
    });
    // skip: the old lead is not touched, but it still joins the list and gets the tag
    let r = l.ok("POST", "/v1/leads/bulk", Some(body("skip"))).await;
    assert_eq!((r["created"].as_i64(), r["updated"].as_i64(), r["skipped"].as_i64(), r["invalid"].as_i64()), (Some(1), Some(0), Some(2), Some(2)), "{r}");
    let statuses: Vec<&str> = r["items"].as_array().unwrap().iter().map(|i| i["status"].as_str().unwrap()).collect();
    assert_eq!(statuses, ["skipped", "created", "skipped", "invalid", "invalid"]);
    assert_eq!(r["items"][3]["error"]["code"], "invalid_lead");
    assert_eq!(r["items"][2]["lead_id"], r["items"][1]["lead_id"], "the repeat points at the first row's lead");
    let old = l.ok("GET", &format!("/v1/leads/{}", r["items"][0]["lead_id"].as_str().unwrap()), None).await;
    assert_eq!(old["first_name"], "Old");
    assert_eq!(old["last_name"], Value::Null);
    assert_eq!(old["tags"], json!(["a", "batch1"]));
    assert_eq!(old["lists"][0]["name"], "Batch");
    let fresh = l.ok("GET", &format!("/v1/leads/{}", r["items"][1]["lead_id"].as_str().unwrap()), None).await;
    assert_eq!(fresh["first_name"], "Fresh", "the first row wins");

    // fill_empty: only the empty fields of the old lead are filled
    let r = l.ok("POST", "/v1/leads/bulk", Some(body("fill_empty"))).await;
    assert_eq!((r["created"].as_i64(), r["updated"].as_i64(), r["skipped"].as_i64()), (Some(0), Some(1), Some(2)));
    let old = l.ok("GET", &format!("/v1/leads/{}", r["items"][0]["lead_id"].as_str().unwrap()), None).await;
    assert_eq!((old["first_name"].as_str(), old["last_name"].as_str()), (Some("Old"), Some("Name")));
    assert_eq!(old["custom_fields"]["pain_point"], "keep");
    assert_eq!(old["tags"], json!(["a", "batch1", "b"]));

    // run it again: nothing left to fill, so nothing changes
    let r = l.ok("POST", "/v1/leads/bulk", Some(body("fill_empty"))).await;
    assert_eq!((r["updated"].as_i64(), r["skipped"].as_i64()), (Some(0), Some(3)));

    // overwrite: a value in the row replaces the old one. A missing value never erases.
    let r = l.ok("POST", "/v1/leads/bulk", Some(json!({ "dedupe_mode": "overwrite", "leads": [
        { "email": "old@example.com", "first_name": "Replaced", "last_name": "" }] }))).await;
    assert_eq!(r["updated"], 1);
    let old = l.ok("GET", &format!("/v1/leads/{}", r["items"][0]["lead_id"].as_str().unwrap()), None).await;
    assert_eq!((old["first_name"].as_str(), old["last_name"].as_str()), (Some("Replaced"), Some("Name")));
    let r = l.ok("POST", "/v1/leads/bulk", Some(json!({ "dedupe_mode": "overwrite", "leads": [
        { "email": "old@example.com", "custom_fields": { "pain_point": "changed" } }] }))).await;
    let old = l.ok("GET", &format!("/v1/leads/{}", r["items"][0]["lead_id"].as_str().unwrap()), None).await;
    assert_eq!(old["custom_fields"]["pain_point"], "changed");
    assert_eq!(old["first_name"], "Replaced");

    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM leads").fetch_one(&l.h.state.pool).await.unwrap();
    assert_eq!(n, 2);
}

#[sqlx::test]
async fn bulk_limits_and_default_mode(pool: PgPool) {
    let l = L::new(pool).await;
    assert_eq!(l.api("POST", "/v1/leads/bulk", Some(json!({ "leads": [] }))).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    let many: Vec<Value> = (0..1001).map(|i| json!({ "email": format!("u{i}@example.com") })).collect();
    assert_eq!(l.api("POST", "/v1/leads/bulk", Some(json!({ "leads": many }))).await.status, StatusCode::UNPROCESSABLE_ENTITY);
    let thousand: Vec<Value> = (0..1000).map(|i| json!({ "email": format!("u{i}@example.com") })).collect();
    let r = l.ok("POST", "/v1/leads/bulk", Some(json!({ "leads": thousand }))).await;
    assert_eq!(r["created"], 1000);
    assert_eq!(l.api("POST", "/v1/leads/bulk", Some(json!({ "leads": [{ "email": "a@b.com" }], "list_id": Uuid::now_v7() }))).await.status, StatusCode::NOT_FOUND);
    // no dedupe_mode: the setting decides (fill_empty by default)
    l.ok("PATCH", "/v1/leads/settings", Some(json!({ "default_dedupe_mode": "skip" }))).await;
    l.ok("POST", "/v1/leads/bulk", Some(json!({ "leads": [{ "email": "u1@example.com", "first_name": "Z" }] }))).await;
    assert_eq!(l.ok("GET", "/v1/leads?q=u1%40example&limit=1", None).await["leads"][0]["first_name"], Value::Null);
}

#[sqlx::test]
async fn interest_survives_bulk_overwrite(pool: PgPool) {
    let l = L::new(pool).await;
    let a = l.ok("POST", "/v1/leads", Some(json!({ "email": "a@example.com", "interest": "meeting_booked" }))).await;
    l.ok("POST", "/v1/leads/bulk", Some(json!({ "dedupe_mode": "overwrite", "leads": [{ "email": "a@example.com", "first_name": "X" }] }))).await;
    assert_eq!(l.ok("GET", &format!("/v1/leads/{}", id_of(&a)), None).await["interest"], "meeting_booked");
}

// ---------- phase 5 (part): the MX check ----------

#[sqlx::test]
async fn mx_check_marks_domains_and_caches_one_lookup_per_domain(pool: PgPool) {
    let l = L::new(pool).await;
    l.mx.set("nomx.test", MxAnswer::No);
    l.mx.set("slow.test", MxAnswer::Unknown);
    let mut leads: Vec<Value> = (0..998).map(|i| json!({ "email": format!("u{i}@same.test") })).collect();
    leads.push(json!({ "email": "a@nomx.test" }));
    leads.push(json!({ "email": "a@slow.test" }));
    l.ok("POST", "/v1/leads/bulk", Some(json!({ "leads": leads }))).await;
    let calls = l.mx.calls();
    assert_eq!(calls.iter().filter(|d| *d == "same.test").count(), 1, "998 leads on one domain make 1 lookup");
    assert_eq!(calls.len(), 3);
    let v = |email: &str| { let pool = l.h.state.pool.clone(); let email = email.to_string(); async move { sqlx::query_as::<_, (String,)>("SELECT verification::text FROM leads WHERE email = $1").bind(email).fetch_one(&pool).await.unwrap().0 } };
    assert_eq!(v("u5@same.test").await, "mx_ok");
    assert_eq!(v("a@nomx.test").await, "no_mx");
    assert_eq!(v("a@slow.test").await, "syntax_ok", "a timeout is not no_mx");
    // a second batch on a known domain: no new lookup
    l.ok("POST", "/v1/leads/bulk", Some(json!({ "leads": [{ "email": "new@same.test" }, { "email": "n2@nomx.test" }] }))).await;
    assert_eq!(l.mx.calls().len(), 3);
    assert_eq!(v("new@same.test").await, "mx_ok");
    assert_eq!(v("n2@nomx.test").await, "no_mx");
}

#[sqlx::test]
async fn mx_check_off_makes_no_lookup(pool: PgPool) {
    let l = L::new(pool).await;
    l.ok("PATCH", "/v1/leads/settings", Some(json!({ "mx_check": false }))).await;
    let r = l.ok("POST", "/v1/leads", Some(json!({ "email": "a@example.com" }))).await;
    assert_eq!(r["verification"], "syntax_ok");
    assert!(l.mx.calls().is_empty());
}

#[sqlx::test]
async fn verify_action_rechecks_and_ignores_the_cache(pool: PgPool) {
    let l = L::new(pool).await;
    l.mx.set("fixed.test", MxAnswer::No);
    let a = l.lead("a@fixed.test").await;
    assert_eq!(l.ok("GET", &format!("/v1/leads/{a}"), None).await["verification"], "no_mx");
    // the DNS record is fixed; the cache still says no
    l.mx.set("fixed.test", MxAnswer::Has);
    let r = l.ok("POST", "/v1/leads/actions", Some(json!({ "action": "verify", "lead_ids": [a] }))).await;
    assert_eq!(r["verify"], json!({ "mx_ok": 1, "no_mx": 0, "unchecked": 0, "leads_changed": 1 }));
    assert_eq!(r["changed"], 1);
    assert_eq!(l.ok("GET", &format!("/v1/leads/{a}"), None).await["verification"], "mx_ok");
    // a stored address that fails the syntax check becomes invalid
    sqlx::query("ALTER TABLE leads DISABLE TRIGGER ALL").execute(&l.h.state.pool).await.unwrap();
    sqlx::query("UPDATE leads SET email = 'broken@@x', email_domain = 'x' WHERE id = $1").bind(a).execute(&l.h.state.pool).await.unwrap();
    l.ok("POST", "/v1/leads/actions", Some(json!({ "action": "verify", "lead_ids": [a] }))).await;
    assert_eq!(l.ok("GET", &format!("/v1/leads/{a}"), None).await["verification"], "invalid");
}
