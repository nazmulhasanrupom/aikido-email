//! Automations, phase 5: publish, versions, activate, time triggers, event triggers, webhooks, manual runs, limits.
//! These tests need Postgres (not run in the session that wrote them).
mod common;
use aikido_email::auth::api_keys::Scope;
use aikido_email::automations::engine::worker::{self, WorkerOpts};
use aikido_email::automations::production::tick::{self, TickReport};
use aikido_email::campaigns::feeder;
use aikido_email::state::AppState;
use axum::http::StatusCode;
use chrono::Duration;
use common::gharness::*;
use common::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

const WF: &str = "/v1/automations/workflows";
const RUNS: &str = "/v1/automations/runs";

struct Ctx {
    state: AppState,
    keys: HashMap<Scope, String>,
    /// The session of the owner made at the start.
    owner: Web,
}

impl Ctx {
    /// An owner (production runs act as a user) and one key per scope.
    async fn new(state: AppState) -> Self {
        let owner = setup_owner(&state).await;
        let keys = bootstrap_keys(&state).await;
        Ctx { state, keys, owner }
    }

    /// For tests that already have keys (the generator harness).
    async fn with_keys(state: AppState, keys: HashMap<Scope, String>) -> Self {
        let owner = setup_owner(&state).await;
        Ctx { state, keys, owner }
    }

    fn k(&self) -> &str {
        &self.keys[&Scope::Automations]
    }

    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> Response {
        call(&self.state, method, uri, Some(self.k()), body).await
    }

    async fn ok(&self, method: &str, uri: &str, body: Option<Value>) -> Value {
        let r = self.call(method, uri, body).await;
        assert!(r.status.is_success(), "{method} {uri} -> {} {}", r.status, r.body);
        r.body
    }

    async fn make(&self, def: Value) -> String {
        let r = self.call("POST", WF, Some(json!({ "name": "Production", "definition": def }))).await;
        assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.body);
        r.body["id"].as_str().unwrap().to_string()
    }

    async fn publish(&self, wf: &str) -> Value {
        self.ok("POST", &format!("{WF}/{wf}/publish"), Some(json!({}))).await
    }

    async fn activate(&self, wf: &str, body: Value) -> Value {
        self.ok("POST", &format!("{WF}/{wf}/activate"), Some(body)).await
    }

    async fn go(&self, def: Value) -> String {
        let wf = self.make(def).await;
        self.publish(&wf).await;
        self.activate(&wf, json!({})).await;
        wf
    }

    /// Let the worker process every queued run.
    async fn work(&self) {
        worker::tick(&self.state, &WorkerOpts::default()).await.unwrap();
    }

    async fn runs(&self, wf: &str, mode: &str) -> Vec<Value> {
        self.ok("GET", &format!("{RUNS}?workflow_id={wf}&mode={mode}&limit=200"), None).await["runs"].as_array().unwrap().clone()
    }

    async fn run(&self, id: &str) -> Value {
        self.ok("GET", &format!("{RUNS}/{id}"), None).await
    }

    async fn draft(&self, wf: &str) -> Value {
        self.ok("GET", &format!("{WF}/{wf}"), None).await["draft"].clone()
    }

    async fn set_draft(&self, wf: &str, draft: Value) {
        self.ok("PATCH", &format!("{WF}/{wf}"), Some(json!({ "draft": draft }))).await;
    }

    async fn trigger_row(&self, wf: &str) -> (Option<chrono::DateTime<chrono::Utc>>, Option<Value>, Option<String>) {
        sqlx::query_as("SELECT next_run_at, cursor, webhook_path FROM automation_triggers WHERE workflow_id = $1::uuid ORDER BY node_id LIMIT 1").bind(wf).fetch_one(&self.state.pool).await.unwrap()
    }
}

fn node<'a>(run: &'a Value, id: &str) -> &'a Value {
    run["nodes"].as_array().unwrap().iter().find(|n| n["node_id"] == id).unwrap_or_else(|| panic!("no row for {id}: {run}"))
}

fn def(trigger: Value, who: &str) -> Value {
    json!({
        "format": "aikido-workflow", "version": 1, "name": "Production",
        "nodes": [trigger, { "id": "tag", "type": "data.set", "params": { "fields": { "who": who } } }],
        "edges": [{ "from": "start", "to": "tag" }]
    })
}

fn manual(who: &str) -> Value {
    def(json!({ "id": "start", "type": "trigger.manual" }), who)
}

fn ticks(r: &TickReport) -> (usize, usize, usize) {
    (r.fired, r.runs, r.held)
}

// ---------- publish, versions, restore ----------

#[sqlx::test]
async fn publish_freezes_the_definition_so_editing_the_draft_does_not_change_a_production_run(pool: PgPool) {
    let c = Ctx::new(app_state(pool)).await;
    let wf = c.make(manual("old")).await;
    let p = c.publish(&wf).await;
    assert_eq!(p["version"], 1);
    assert_eq!(p["workflow"]["published"]["version"], 1);

    // edit the draft after publishing
    let mut d = c.draft(&wf).await;
    d["nodes"][1]["params"]["fields"]["who"] = json!("new");
    c.set_draft(&wf, d).await;
    let list = c.ok("GET", WF, None).await;
    assert_eq!(list["workflows"][0]["draft_changed"], true);
    assert_eq!(list["workflows"][0]["published_version"], 1);

    c.activate(&wf, json!({})).await;
    let started = c.ok("POST", &format!("{WF}/{wf}/run"), Some(json!({}))).await;
    c.work().await;
    let run = c.run(started["run_id"].as_str().unwrap()).await;
    assert_eq!(run["status"], "success", "{run}");
    assert_eq!(run["mode"], "production");
    assert_eq!(run["version"], 1);
    assert_eq!(node(&run, "tag")["output"]["main"][0]["who"], "old", "the run used the published version");

    // a test run still uses the draft
    let t = c.ok("POST", &format!("{WF}/{wf}/test"), Some(json!({}))).await;
    c.work().await;
    assert_eq!(node(&c.run(t["run_id"].as_str().unwrap()).await, "tag")["output"]["main"][0]["who"], "new");
}

#[sqlx::test]
async fn publish_refuses_a_workflow_with_errors_and_says_which(pool: PgPool) {
    let c = Ctx::new(app_state(pool)).await;
    let wf = c.make(manual("${{ nodes.nope.json.x }}")).await;
    let r = c.call("POST", &format!("{WF}/{wf}/publish"), Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{:?}", r.body);
    assert_eq!(r.body["error"]["code"], "workflow_has_errors");
    assert_eq!(r.body["error"]["validation"]["errors"][0]["code"], "unknown_reference");
    // nothing was published
    assert_eq!(c.ok("GET", &format!("{WF}/{wf}/versions"), None).await["versions"].as_array().unwrap().len(), 0);

    // a required variable without a value cannot be published, but can be saved
    let mut d = manual("x");
    d["variables"] = json!([{ "key": "campaign_id", "kind": "campaign", "value": null, "required": true }]);
    let wf2 = c.make(d).await;
    let r = c.call("POST", &format!("{WF}/{wf2}/publish"), Some(json!({}))).await;
    assert_eq!(r.body["error"]["validation"]["errors"][0]["code"], "variable_missing", "{:?}", r.body);
}

#[sqlx::test]
async fn versions_are_numbered_and_restore_copies_into_the_draft_without_publishing(pool: PgPool) {
    let c = Ctx::new(app_state(pool)).await;
    let wf = c.make(manual("one")).await;
    c.publish(&wf).await;
    let mut d = c.draft(&wf).await;
    d["nodes"][1]["params"]["fields"]["who"] = json!("two");
    c.set_draft(&wf, d).await;
    let p2 = c.ok("POST", &format!("{WF}/{wf}/publish"), Some(json!({ "note": "second" }))).await;
    assert_eq!(p2["version"], 2);

    let list = c.ok("GET", &format!("{WF}/{wf}/versions"), None).await;
    let versions = list["versions"].as_array().unwrap();
    assert_eq!((versions[0]["version"].as_i64(), versions[0]["note"].as_str(), versions[0]["is_published"].as_bool()), (Some(2), Some("second"), Some(true)));
    assert_eq!(versions[1]["is_published"], false);
    let v1 = c.ok("GET", &format!("{WF}/{wf}/versions/1"), None).await;
    assert_eq!(v1["definition"]["nodes"][1]["params"]["fields"]["who"], "one");
    assert_eq!(c.call("GET", &format!("{WF}/{wf}/versions/9"), None).await.body["error"]["code"], "version_not_found");

    // restore version 1: the draft changes, the published version does not
    let restored = c.ok("POST", &format!("{WF}/{wf}/versions/1/restore"), None).await;
    assert_eq!(restored["draft"]["nodes"][1]["params"]["fields"]["who"], "one");
    assert_eq!(restored["published"]["version"], 2);

    // the draft history holds the saves, and an old one can be put back
    let hist = c.ok("GET", &format!("{WF}/{wf}/draft-history"), None).await;
    let entries = hist["history"].as_array().unwrap();
    assert!(entries.len() >= 3, "{hist}");
    assert_eq!(entries[0]["source"], "restore");
    let editor = entries.iter().find(|e| e["source"] == "editor" && e["draft_hash"] != entries[0]["draft_hash"]).unwrap();
    let back = c.ok("POST", &format!("{WF}/{wf}/draft-history/{}/restore", editor["id"]), None).await;
    assert_ne!(back["draft"], restored["draft"]);
    assert_eq!(c.call("POST", &format!("{WF}/{wf}/draft-history/999999999/restore"), None).await.body["error"]["code"], "history_not_found");
}

#[sqlx::test]
async fn activate_needs_a_published_version_and_deactivate_removes_the_triggers(pool: PgPool) {
    let c = Ctx::new(app_state(pool)).await;
    let wf = c.make(def(json!({ "id": "start", "type": "trigger.webhook", "params": { "path": "needs-publish-1" } }), "x")).await;
    let r = c.call("POST", &format!("{WF}/{wf}/activate"), Some(json!({}))).await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::CONFLICT, Some("not_published")));
    c.publish(&wf).await;
    let a = c.activate(&wf, json!({})).await;
    assert_eq!(a["workflow"]["active"], true);
    assert_eq!(a["triggers"][0]["kind"], "webhook");
    assert!(a["triggers"][0]["webhook_url"].as_str().unwrap().ends_with("/hooks/needs-publish-1"));
    assert_eq!(count(&c.state, "SELECT count(*) FROM automation_triggers").await, 1);

    let d = c.ok("POST", &format!("{WF}/{wf}/deactivate"), None).await;
    assert_eq!(d["active"], false);
    assert_eq!(count(&c.state, "SELECT count(*) FROM automation_triggers").await, 0);
    // archiving a workflow switches it off too
    c.activate(&wf, json!({})).await;
    let r = c.call("DELETE", &format!("{WF}/{wf}"), None).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(count(&c.state, "SELECT count(*) FROM automation_triggers").await, 0);
    let r = c.call("POST", &format!("{WF}/{wf}/activate"), Some(json!({}))).await;
    assert_eq!(r.body["error"]["code"], "workflow_archived");
}

#[sqlx::test]
async fn publishing_while_active_swaps_the_triggers_and_keeps_the_webhook_url_and_the_event_cursor(pool: PgPool) {
    let c = Ctx::new(app_state(pool)).await;
    // a webhook without a path of its own: the generated path stays through a publish
    let wf = c.go(def(json!({ "id": "start", "type": "trigger.webhook" }), "x")).await;
    let (_, _, path) = c.trigger_row(&wf).await;
    let path = path.unwrap();
    assert_eq!(path.len(), 16);
    c.publish(&wf).await;
    let (_, _, again) = c.trigger_row(&wf).await;
    assert_eq!(again.as_deref(), Some(path.as_str()));
    let version: i32 = sqlx::query_scalar("SELECT v.version FROM automation_triggers t JOIN automation_versions v ON v.id = t.version_id WHERE t.workflow_id = $1::uuid").bind(&wf).fetch_one(&c.state.pool).await.unwrap();
    assert_eq!(version, 2, "the trigger points at the new version");

    // an event trigger keeps its cursor
    let ev = c.go(def(json!({ "id": "start", "type": "trigger.lead_created" }), "x")).await;
    sqlx::query("UPDATE automation_triggers SET cursor = '{\"id\": 777}' WHERE workflow_id = $1::uuid").bind(&ev).execute(&c.state.pool).await.unwrap();
    c.publish(&ev).await;
    assert_eq!(c.trigger_row(&ev).await.1, Some(json!({ "id": 777 })));
}

#[sqlx::test]
async fn two_workflows_cannot_use_the_same_webhook_path(pool: PgPool) {
    let c = Ctx::new(app_state(pool)).await;
    let params = json!({ "path": "same-path-1" });
    c.go(def(json!({ "id": "start", "type": "trigger.webhook", "params": params }), "x")).await;
    let second = c.make(def(json!({ "id": "start", "type": "trigger.webhook", "params": params }), "y")).await;
    c.publish(&second).await;
    let r = c.call("POST", &format!("{WF}/{second}/activate"), Some(json!({}))).await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::CONFLICT, Some("webhook_path_taken")), "{:?}", r.body);
    assert_eq!(c.ok("GET", &format!("{WF}/{second}"), None).await["active"], false, "the failed activation left nothing behind");
}

// ---------- manual production run, retry ----------

#[sqlx::test]
async fn a_manual_production_run_needs_an_active_workflow_and_obeys_the_hourly_limit(pool: PgPool) {
    let c = Ctx::new(app_state(pool)).await;
    let mut d = manual("x");
    d["settings"] = json!({ "max_runs_per_hour": 2 });
    let wf = c.make(d).await;
    c.publish(&wf).await;
    let r = c.call("POST", &format!("{WF}/{wf}/run"), Some(json!({}))).await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::CONFLICT, Some("workflow_not_active")));
    c.activate(&wf, json!({})).await;

    let bad = c.call("POST", &format!("{WF}/{wf}/run"), Some(json!({ "input": [1] }))).await;
    assert_eq!(bad.body["error"]["code"], "invalid_input");
    for _ in 0..2 {
        let r = c.call("POST", &format!("{WF}/{wf}/run"), Some(json!({ "input": { "n": 1 } }))).await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{:?}", r.body);
    }
    let third = c.call("POST", &format!("{WF}/{wf}/run"), Some(json!({}))).await;
    assert_eq!((third.status, third.body["error"]["code"].as_str()), (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited")));
    c.work().await;
    let runs = c.runs(&wf, "production").await;
    assert_eq!(runs.len(), 2);
    assert!(runs.iter().all(|r| r["status"] == "success"));
    let first = c.run(runs[0]["id"].as_str().unwrap()).await;
    assert_eq!(first["trigger_item"]["input"]["n"], 1);
}

#[sqlx::test]
async fn retry_makes_a_new_run_from_a_failed_production_run_only(pool: PgPool) {
    let c = Ctx::new(app_state(pool)).await;
    let d = json!({
        "format": "aikido-workflow", "version": 1, "name": "Fails",
        "nodes": [{ "id": "start", "type": "trigger.manual" }, { "id": "stop", "type": "flow.stop", "params": { "message": "boom", "error": true } }],
        "edges": [{ "from": "start", "to": "stop" }]
    });
    let wf = c.go(d).await;
    let started = c.ok("POST", &format!("{WF}/{wf}/run"), Some(json!({ "input": { "a": 1 } }))).await;
    c.work().await;
    let failed = c.run(started["run_id"].as_str().unwrap()).await;
    assert_eq!((failed["status"].as_str(), failed["error"]["message"].as_str()), (Some("error"), Some("boom")), "{failed}");

    let retry = c.ok("POST", &format!("{RUNS}/{}/retry", failed["id"].as_str().unwrap()), None).await;
    let again = c.run(retry["run_id"].as_str().unwrap()).await;
    assert_eq!(again["status"], "queued");
    assert_eq!(again["trigger_item"], failed["trigger_item"], "the same trigger item");
    assert_eq!(again["version"], failed["version"]);

    // a test run cannot be retried
    c.work().await;
    let t = c.ok("POST", &format!("{WF}/{wf}/test"), Some(json!({}))).await;
    let r = c.call("POST", &format!("{RUNS}/{}/retry", t["run_id"].as_str().unwrap()), None).await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::CONFLICT, Some("run_not_retryable")));
    let r = c.call("POST", &format!("{RUNS}/{}/retry", Uuid::now_v7()), None).await;
    assert_eq!(r.body["error"]["code"], "run_not_found");
}

// ---------- time triggers ----------

#[sqlx::test]
async fn an_interval_trigger_fires_when_due_and_makes_one_run_after_downtime(pool: PgPool) {
    let h = Harness::at(pool, utc(2026, 10, 26, 10, 0));
    let c = Ctx::new(h.state.clone()).await;
    let params = json!({ "mode": "interval", "every": 15, "unit": "minutes" });
    let wf = c.go(def(json!({ "id": "start", "type": "trigger.schedule", "params": params }), "x")).await;
    assert_eq!(c.trigger_row(&wf).await.0, Some(utc(2026, 10, 26, 10, 15)));

    assert_eq!(ticks(&tick::time_tick(&c.state).await.unwrap()), (0, 0, 0), "not due yet");
    h.advance(Duration::minutes(16));
    assert_eq!(ticks(&tick::time_tick(&c.state).await.unwrap()), (1, 1, 0));
    assert_eq!(c.trigger_row(&wf).await.0, Some(utc(2026, 10, 26, 10, 30)));
    // asking again in the same minute makes nothing
    assert_eq!(ticks(&tick::time_tick(&c.state).await.unwrap()), (0, 0, 0));

    // down for 3 hours: one run for the missed time, not twelve
    h.advance(Duration::hours(3));
    assert_eq!(ticks(&tick::time_tick(&c.state).await.unwrap()), (1, 1, 0));
    assert_eq!(c.runs(&wf, "production").await.len(), 2);
    let next = c.trigger_row(&wf).await.0.unwrap();
    assert!(next > h.now() && next <= h.now() + Duration::minutes(15), "{next}");

    // the run holds the time it was due
    c.work().await;
    let run = c.run(c.runs(&wf, "production").await[0]["id"].as_str().unwrap()).await;
    assert_eq!(run["status"], "success", "{run}");
    assert_eq!(run["trigger_item"]["timezone"], "UTC");
}

#[sqlx::test]
async fn catch_up_skip_makes_no_run_for_a_missed_time(pool: PgPool) {
    let h = Harness::at(pool, utc(2026, 10, 26, 10, 0));
    let c = Ctx::new(h.state.clone()).await;
    let params = json!({ "mode": "interval", "every": 15, "unit": "minutes", "catch_up": "skip" });
    let wf = c.go(def(json!({ "id": "start", "type": "trigger.schedule", "params": params }), "x")).await;
    h.advance(Duration::hours(3));
    assert_eq!(ticks(&tick::time_tick(&c.state).await.unwrap()), (1, 0, 0), "the trigger moved on, but no run was made");
    assert!(c.trigger_row(&wf).await.0.unwrap() > h.now());
    // on time, it runs
    h.advance(Duration::minutes(16));
    assert_eq!(ticks(&tick::time_tick(&c.state).await.unwrap()), (1, 1, 0));
}

#[sqlx::test]
async fn cron_triggers_use_their_time_zone_and_follow_daylight_saving(pool: PgPool) {
    let h = Harness::at(pool, utc(2026, 10, 26, 0, 0));
    let c = Ctx::new(h.state.clone()).await;
    let dhaka = c.go(def(json!({ "id": "start", "type": "trigger.cron", "params": { "expression": "0 9 * * *", "timezone": "Asia/Dhaka" } }), "x")).await;
    assert_eq!(c.trigger_row(&dhaka).await.0, Some(utc(2026, 10, 26, 3, 0)));

    // Europe/Berlin on the night the clocks go forward: 09:00 local is 07:00 UTC on 2026-03-29
    h.set_time(utc(2026, 3, 28, 12, 0));
    let berlin = c.go(def(json!({ "id": "start", "type": "trigger.cron", "params": { "expression": "0 9 * * *", "timezone": "Europe/Berlin" } }), "y")).await;
    assert_eq!(c.trigger_row(&berlin).await.0, Some(utc(2026, 3, 29, 7, 0)));

    // the editor's preview
    let p = c.ok("GET", "/v1/automations/cron/preview?expression=0%209%20*%20*%20*&timezone=Asia/Dhaka&count=3", None).await;
    assert_eq!(p["next"].as_array().unwrap().len(), 3);
    assert!(p["local"][0].as_str().unwrap().contains("09:00 +06:00"), "{p}");
    let bad = c.call("GET", "/v1/automations/cron/preview?expression=nope", None).await;
    assert_eq!((bad.status, bad.body["error"]["code"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("invalid_cron")));
    let bad = c.call("GET", "/v1/automations/cron/preview?expression=0%209%20*%20*%20*&timezone=Mars/Base", None).await;
    assert_eq!(bad.body["error"]["code"], "invalid_timezone");
}

#[sqlx::test]
async fn a_cron_trigger_that_cannot_be_planned_is_refused_on_activate(pool: PgPool) {
    let c = Ctx::new(app_state(pool)).await;
    let wf = c.make(def(json!({ "id": "start", "type": "trigger.cron", "params": { "expression": "0 0 9 * * *" } }), "x")).await;
    // the schema allows only 5 fields, so the draft itself has an error
    let r = c.call("POST", &format!("{WF}/{wf}/publish"), Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
}

// ---------- event triggers ----------

fn campaign_flow(campaign: &str, settings: Value) -> Value {
    json!({
        "format": "aikido-workflow", "version": 1, "name": "On lead",
        "variables": [{ "key": "campaign_id", "kind": "campaign", "value": campaign, "required": true }],
        "settings": settings,
        "nodes": [
            { "id": "start", "type": "trigger.lead_added_to_campaign", "params": { "campaign_id": "${{ vars.campaign_id }}" } },
            { "id": "tag", "type": "data.set", "params": { "fields": { "who": "${{ trigger.lead.first_name }}" } } }
        ],
        "edges": [{ "from": "start", "to": "tag" }]
    })
}

struct Camp {
    g: G,
    c: Ctx,
    camp: String,
}

impl Camp {
    async fn new(pool: PgPool) -> Self {
        let g = G::new(pool).await;
        let steps = json!([{ "delay_amount": 0, "subject": "Hi", "body_text": "x", "ai_user_prompt": "Write to {{first_name}}." }]);
        let r = g.ok(Scope::Campaigns, "POST", "/v1/campaigns", Some(json!({ "name": "Research", "account_ids": [g.account], "sequence": { "name": "S", "steps": steps } }))).await;
        let camp = r["id"].as_str().unwrap().to_string();
        let c = Ctx::with_keys(g.h.state.clone(), g.keys.clone()).await;
        Camp { g, c, camp }
    }

    async fn ai_mode(&self) {
        self.g.ok(Scope::Campaigns, "PATCH", &format!("/v1/campaigns/{}", self.camp), Some(json!({ "content_mode": "ai", "auto_write": false }))).await;
    }

    async fn add(&self, email: &str, name: &str) -> Uuid {
        let lead = self.g.lead(email, name).await;
        self.g.ok(Scope::Campaigns, "POST", &format!("/v1/campaigns/{}/leads", self.camp), Some(json!({ "lead_ids": [lead] }))).await;
        lead
    }

    async fn events(&self) -> TickReport {
        tick::event_tick_with(&self.c.state, 0.0).await.unwrap()
    }
}

#[sqlx::test]
async fn an_event_trigger_starts_at_now_makes_one_run_per_event_and_never_a_second(pool: PgPool) {
    let t = Camp::new(pool).await;
    t.add("old@acme.example", "Old").await;
    let wf = t.c.go(campaign_flow(&t.camp, json!({}))).await;
    assert_eq!(ticks(&t.events().await), (0, 0, 0), "an event from before the activation starts nothing");

    t.add("ana@acme.example", "Ana").await;
    assert_eq!(ticks(&t.events().await), (1, 1, 0));
    assert_eq!(ticks(&t.events().await), (0, 0, 0), "the cursor moved: the event is not read again");
    let runs = t.c.runs(&wf, "production").await;
    assert_eq!(runs.len(), 1);

    t.c.work().await;
    let run = t.c.run(runs[0]["id"].as_str().unwrap()).await;
    assert_eq!(run["status"], "success", "{run}");
    assert_eq!(run["trigger_item"]["lead"]["first_name"], "Ana");
    assert_eq!(run["trigger_item"]["campaign"]["id"], t.camp.as_str());
    assert_eq!(run["trigger_item"]["event"]["type"], "campaign_added");
    assert_eq!(node(&run, "tag")["output"]["main"][0]["who"], "Ana");

    // a lead in another campaign does not start it
    let other = t.g.ok(Scope::Campaigns, "POST", "/v1/campaigns", Some(json!({ "name": "Other", "account_ids": [t.g.account], "sequence": { "name": "S2", "steps": [{ "delay_amount": 0, "subject": "Hi", "body_text": "x" }] } }))).await;
    let lead = t.g.lead("bo@acme.example", "Bo").await;
    t.g.ok(Scope::Campaigns, "POST", &format!("/v1/campaigns/{}/leads", other["id"].as_str().unwrap()), Some(json!({ "lead_ids": [lead] }))).await;
    assert_eq!(ticks(&t.events().await), (0, 0, 0));
}

#[sqlx::test]
async fn backfill_hours_starts_the_cursor_in_the_past(pool: PgPool) {
    let t = Camp::new(pool).await;
    t.add("early@acme.example", "Early").await;
    t.g.h.advance(Duration::minutes(30));
    t.add("late@acme.example", "Late").await;
    let wf = t.c.make(campaign_flow(&t.camp, json!({}))).await;
    t.c.publish(&wf).await;
    t.c.activate(&wf, json!({ "backfill_hours": 1 })).await;
    assert_eq!(ticks(&t.events().await), (1, 2, 0), "both events are inside the last hour");

    // 100 hours is too far
    let r = t.c.call("POST", &format!("{WF}/{wf}/activate"), Some(json!({ "backfill_hours": 200 }))).await;
    assert_eq!(r.body["error"]["code"], "invalid_backfill");
}

#[sqlx::test]
async fn a_workflow_skips_events_it_made_itself_but_not_those_of_other_workflows(pool: PgPool) {
    let t = Camp::new(pool).await;
    let wf = t.c.go(campaign_flow(&t.camp, json!({}))).await;
    let lead = t.g.lead("ana@acme.example", "Ana").await;
    let camp: Uuid = t.camp.parse().unwrap();
    let now = t.g.h.now();
    for actor in [format!("automation:{wf}"), format!("automation:{}", Uuid::now_v7())] {
        sqlx::query("INSERT INTO lead_events (lead_id, kind, ref_id, detail, at) VALUES ($1, 'campaign_added', $2, $3, $4)")
            .bind(lead)
            .bind(camp)
            .bind(json!({ "actor": actor }))
            .bind(now)
            .execute(&t.c.state.pool)
            .await
            .unwrap();
    }
    assert_eq!(ticks(&t.events().await), (1, 1, 0), "only the event of the other workflow starts a run");
}

#[sqlx::test]
async fn a_full_queue_holds_the_cursor_and_nothing_is_lost(pool: PgPool) {
    let t = Camp::new(pool).await;
    let wf = t.c.go(campaign_flow(&t.camp, json!({}))).await;
    let version: Uuid = sqlx::query_scalar("SELECT published_version_id FROM automation_workflows WHERE id = $1::uuid").bind(&wf).fetch_one(&t.c.state.pool).await.unwrap();
    sqlx::query(
        "INSERT INTO automation_runs (id, workflow_id, version_id, mode, status, trigger_node_id, trigger_item) \
         SELECT gen_random_uuid(), $1::uuid, $2, 'production', 'queued', 'start', '{}' FROM generate_series(1, 500)",
    )
    .bind(&wf)
    .bind(version)
    .execute(&t.c.state.pool)
    .await
    .unwrap();
    let before = t.c.trigger_row(&wf).await.1;
    t.add("ana@acme.example", "Ana").await;
    assert_eq!(ticks(&t.events().await), (0, 0, 1), "the trigger is held back");
    assert_eq!(t.c.trigger_row(&wf).await.1, before, "the cursor did not move");

    sqlx::query("DELETE FROM automation_runs WHERE workflow_id = $1::uuid").bind(&wf).execute(&t.c.state.pool).await.unwrap();
    assert_eq!(ticks(&t.events().await), (1, 1, 0), "the event is read once the queue has room");
}

#[sqlx::test]
async fn the_hourly_limit_holds_the_trigger_raises_an_alert_and_clears_after_the_hour(pool: PgPool) {
    let t = Camp::new(pool).await;
    let wf = t.c.go(campaign_flow(&t.camp, json!({ "max_runs_per_hour": 2 }))).await;
    for (i, name) in ["A", "B", "C"].iter().enumerate() {
        t.add(&format!("l{i}@acme.example"), name).await;
    }
    assert_eq!(ticks(&t.events().await), (1, 2, 0), "two runs, the limit");
    assert_eq!(ticks(&t.events().await), (0, 0, 1), "held");
    let limited: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar("SELECT rate_limited_at FROM automation_triggers WHERE workflow_id = $1::uuid").bind(&wf).fetch_one(&t.c.state.pool).await.unwrap();
    assert!(limited.is_some());
    let alerts = aikido_email::ui_api::alerts::collect(&t.c.state).await.unwrap();
    assert!(alerts.iter().any(|a| a.kind == "automation_rate_limited"), "{alerts:?}");
    let status = t.c.ok("GET", "/v1/automations/status", None).await;
    assert_eq!(status["trigger_problems"][0]["workflow_id"], wf.as_str());

    t.g.h.advance(Duration::minutes(61));
    assert_eq!(ticks(&t.events().await), (1, 1, 0), "the third event waited, it was not lost");
    let limited: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar("SELECT rate_limited_at FROM automation_triggers WHERE workflow_id = $1::uuid").bind(&wf).fetch_one(&t.c.state.pool).await.unwrap();
    assert!(limited.is_none());
}

#[sqlx::test]
async fn two_server_copies_make_one_run_per_event(pool: PgPool) {
    let t = Camp::new(pool).await;
    let wf = t.c.go(campaign_flow(&t.camp, json!({}))).await;
    for i in 0..10 {
        t.add(&format!("l{i}@acme.example"), &format!("N{i}")).await;
    }
    let (a, b) = tokio::join!(tick::event_tick_with(&t.c.state, 0.0), tick::event_tick_with(&t.c.state, 0.0));
    assert_eq!(a.unwrap().runs + b.unwrap().runs, 10);
    assert_eq!(t.c.runs(&wf, "production").await.len(), 10);
    assert_eq!(count(&t.c.state, "SELECT count(DISTINCT idempotency_key) FROM automation_runs").await, 10);
}

#[sqlx::test]
async fn lead_updated_fires_only_for_the_fields_you_chose(pool: PgPool) {
    let c = Ctx::new(app_state(pool)).await;
    let d = json!({
        "format": "aikido-workflow", "version": 1, "name": "Titles",
        "nodes": [{ "id": "start", "type": "trigger.lead_updated", "params": { "fields": ["job_title"] } }, { "id": "tag", "type": "data.set", "params": { "fields": { "x": 1 } } }],
        "edges": [{ "from": "start", "to": "tag" }]
    });
    let wf = c.go(d).await;
    let lead = call(&c.state, "POST", "/v1/leads", Some(&c.keys[&Scope::Leads]), Some(json!({ "email": "ana@acme.example", "first_name": "Ana" }))).await.body["id"].as_str().unwrap().to_string();
    let uri = format!("/v1/leads/{lead}");
    let leads_key = c.keys[&Scope::Leads].clone();
    let patch = |body: Value| {
        let (state, uri, key) = (c.state.clone(), uri.clone(), leads_key.clone());
        async move { call(&state, "PATCH", &uri, Some(&key), Some(body)).await }
    };
    assert_eq!(patch(json!({ "phone": "+15550001" })).await.status, StatusCode::OK);
    assert_eq!(ticks(&tick::event_tick_with(&c.state, 0.0).await.unwrap()), (0, 0, 0), "a phone change is not a title change");
    assert_eq!(patch(json!({ "job_title": "CEO" })).await.status, StatusCode::OK);
    assert_eq!(ticks(&tick::event_tick_with(&c.state, 0.0).await.unwrap()), (1, 1, 0));
    let run = c.run(c.runs(&wf, "production").await[0]["id"].as_str().unwrap()).await;
    assert_eq!(run["trigger_item"]["changed"], json!(["job_title"]));
    assert_eq!(run["trigger_item"]["lead"]["job_title"], "CEO", "the lead is read when the run is made");
}

#[sqlx::test]
async fn campaign_status_changes_are_recorded_and_start_the_trigger(pool: PgPool) {
    let t = Camp::new(pool).await;
    let d = json!({
        "format": "aikido-workflow", "version": 1, "name": "Status",
        "nodes": [{ "id": "start", "type": "trigger.campaign_status_changed", "params": { "to": ["active"] } }, { "id": "tag", "type": "data.set", "params": { "fields": { "x": 1 } } }],
        "edges": [{ "from": "start", "to": "tag" }]
    });
    let wf = t.c.go(d).await;
    t.g.ok(Scope::Campaigns, "POST", &format!("/v1/campaigns/{}/launch", t.camp), None).await;
    assert_eq!(count(&t.c.state, "SELECT count(*) FROM campaign_events WHERE from_status = 'draft' AND to_status = 'active'").await, 1);
    assert_eq!(ticks(&t.events().await), (1, 1, 0));
    t.g.ok(Scope::Campaigns, "POST", &format!("/v1/campaigns/{}/pause", t.camp), None).await;
    assert_eq!(count(&t.c.state, "SELECT count(*) FROM campaign_events").await, 2);
    assert_eq!(ticks(&t.events().await), (0, 0, 0), "a pause is not in `to: [active]`");
    let run = t.c.run(t.c.runs(&wf, "production").await[0]["id"].as_str().unwrap()).await;
    assert_eq!((run["trigger_item"]["from"].as_str(), run["trigger_item"]["to"].as_str()), (Some("draft"), Some("active")));
    assert_eq!(run["trigger_item"]["campaign"]["name"], "Research");
}

#[sqlx::test]
async fn a_reply_starts_the_reply_trigger_with_the_stored_message(pool: PgPool) {
    use aikido_email::replies::ingest::Ingested;
    let c = Ctx::new(app_state(pool)).await;
    let now = c.state.now();
    let account = add_account(&c.state, "sender@acme-outreach.example").await;
    sent_job(&c.state, account, "jane@prospect.example", "<sent-0001@acme-outreach.example>", None, now - Duration::days(1)).await;
    let d = json!({
        "format": "aikido-workflow", "version": 1, "name": "Replies",
        "nodes": [{ "id": "start", "type": "trigger.reply_received" }, { "id": "tag", "type": "data.set", "params": { "fields": { "from": "${{ trigger.message.from_email }}" } } }],
        "edges": [{ "from": "start", "to": "tag" }]
    });
    let wf = c.go(d).await;
    assert_eq!(ticks(&tick::event_tick_with(&c.state, 0.0).await.unwrap()), (0, 0, 0));
    let r = ingest_raw(&c.state, account, "p1", &fixture("human_gmail_quote.eml"), c.state.now()).await; // stored after the trigger was switched on
    assert!(matches!(r, Ingested::Stored { .. }), "{r:?}");
    assert_eq!(ticks(&tick::event_tick_with(&c.state, 0.0).await.unwrap()), (1, 1, 0));
    c.work().await;
    let run = c.run(c.runs(&wf, "production").await[0]["id"].as_str().unwrap()).await;
    assert_eq!(run["status"], "success", "{run}");
    assert_eq!(run["trigger_item"]["message"]["label"], "reply");
    assert!(run["trigger_item"]["message"]["text"].as_str().unwrap().contains("Thursday"));
    assert_eq!(node(&run, "tag")["output"]["main"][0]["from"], "jane@prospect.example");
}

// ---------- webhooks ----------

fn hook_flow(params: Value) -> Value {
    def(json!({ "id": "start", "type": "trigger.webhook", "params": params }), "${{ trigger.body.email }}")
}

#[sqlx::test]
async fn the_production_url_works_only_while_the_workflow_is_active(pool: PgPool) {
    let c = Ctx::new(app_state(pool)).await;
    let wf = c.make(hook_flow(json!({ "path": "prod-hook-1" }))).await;
    let r = call(&c.state, "POST", "/hooks/prod-hook-1", None, Some(json!({ "email": "ana@acme.example" }))).await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::NOT_FOUND, Some("webhook_not_found")), "a draft has no URL");
    c.publish(&wf).await;
    assert_eq!(call(&c.state, "POST", "/hooks/prod-hook-1", None, Some(json!({}))).await.status, StatusCode::NOT_FOUND, "published is not active");
    c.activate(&wf, json!({})).await;

    let r = call(&c.state, "POST", "/hooks/prod-hook-1?src=form", None, Some(json!({ "email": "ana@acme.example" }))).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{:?}", r.body);
    let run_id = r.body["run_id"].as_str().unwrap().to_string();
    c.work().await;
    let run = c.run(&run_id).await;
    assert_eq!((run["mode"].as_str(), run["status"].as_str()), (Some("production"), Some("success")), "{run}");
    assert_eq!(run["trigger_item"]["body"]["email"], "ana@acme.example");
    assert_eq!(run["trigger_item"]["query"]["src"], "form");
    assert_eq!(run["trigger_item"]["method"], "POST");
    assert_eq!(node(&run, "tag")["output"]["main"][0]["who"], "ana@acme.example");

    // another method, and a path that does not exist
    assert_eq!(call(&c.state, "GET", "/hooks/prod-hook-1", None, None).await.status, StatusCode::METHOD_NOT_ALLOWED);
    let r = call(&c.state, "POST", "/hooks/nothing-here", None, Some(json!({}))).await;
    assert_eq!(r.body["error"]["code"], "webhook_not_found");
    // /hooks never falls through to the web app
    let r = call(&c.state, "GET", "/hooks/a/b/c", None, None).await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::NOT_FOUND, Some("not_found")));

    // switched off, the URL is gone
    c.ok("POST", &format!("{WF}/{wf}/deactivate"), None).await;
    assert_eq!(call(&c.state, "POST", "/hooks/prod-hook-1", None, Some(json!({}))).await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn a_webhook_with_a_secret_header_refuses_wrong_callers(pool: PgPool) {
    let c = Ctx::new(app_state(pool)).await;
    let admin = &c.keys[&Scope::Admin];
    let s = call(&c.state, "POST", "/v1/automations/secrets", Some(admin), Some(json!({ "name": "FORM_KEY", "value": "super-secret-value-1" }))).await;
    assert!(s.status.is_success(), "{:?}", s.body);
    c.go(hook_flow(json!({ "path": "secure-hook-1", "auth": "header", "header_name": "X-Form-Key", "secret": "FORM_KEY" }))).await;

    let none = call(&c.state, "POST", "/hooks/secure-hook-1", None, Some(json!({}))).await;
    assert_eq!((none.status, none.body["error"]["code"].as_str()), (StatusCode::UNAUTHORIZED, Some("webhook_unauthorized")));
    let wrong = call_with(&c.state, "POST", "/hooks/secure-hook-1", &[("x-form-key", "nope")], Some(json!({}))).await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    let right = call_with(&c.state, "POST", "/hooks/secure-hook-1", &[("x-form-key", "super-secret-value-1")], Some(json!({ "email": "a@b.example" }))).await;
    assert_eq!(right.status, StatusCode::ACCEPTED, "{:?}", right.body);
    // the secret is not in the stored trigger item
    c.work().await;
    let (item,): (Value,) = sqlx::query_as("SELECT trigger_item FROM automation_runs LIMIT 1").fetch_one(&c.state.pool).await.unwrap();
    assert!(!item.to_string().contains("super-secret-value-1"), "{item}");
}

#[sqlx::test]
async fn a_webhook_is_rate_limited_per_path(pool: PgPool) {
    let c = Ctx::new(app_state(pool)).await;
    let r = call(&c.state, "PATCH", "/v1/automations/settings", Some(&c.keys[&Scope::Admin]), Some(json!({ "webhook_rate_per_min": 2 }))).await;
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    c.go(hook_flow(json!({ "path": "limited-hook-1" }))).await;
    for _ in 0..2 {
        assert_eq!(call(&c.state, "POST", "/hooks/limited-hook-1", None, Some(json!({}))).await.status, StatusCode::ACCEPTED);
    }
    let r = call_with(&c.state, "POST", "/hooks/limited-hook-1", &[], Some(json!({}))).await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited")));
    assert!(r.headers.get("retry-after").is_some());
}

#[sqlx::test]
async fn when_done_answers_with_the_output_of_the_last_node(pool: PgPool) {
    let c = Ctx::new(app_state(pool)).await;
    let wf = c.go(hook_flow(json!({ "path": "sync-hook-1", "response": "when_done" }))).await;
    let state = c.state.clone();
    let caller = tokio::spawn(async move { call(&state, "POST", "/hooks/sync-hook-1", None, Some(json!({ "email": "ana@acme.example" }))).await });
    // the worker is the other server copy
    for _ in 0..40 {
        c.work().await;
        if caller.is_finished() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let r = caller.await.unwrap();
    assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
    assert_eq!(r.body["who"], "ana@acme.example");
    assert_eq!(c.runs(&wf, "production").await.len(), 1);
}

#[sqlx::test]
async fn the_test_url_listens_for_one_request_and_never_starts_a_production_run(pool: PgPool) {
    let c = Ctx::new(app_state(pool)).await;
    let wf = c.make(hook_flow(json!({ "path": "test-hook-77" }))).await;
    let r = call(&c.state, "POST", "/hooks/test/test-hook-77", None, Some(json!({}))).await;
    assert_eq!((r.status, r.body["error"]["code"].as_str()), (StatusCode::NOT_FOUND, Some("webhook_not_listening")));

    // the workflow is a draft: the test URL still works while listening
    let l = c.ok("POST", &format!("{WF}/{wf}/listen"), Some(json!({ "node_id": "start" }))).await;
    assert!(l["test_url"].as_str().unwrap().ends_with("/hooks/test/test-hook-77"), "{l}");
    assert_eq!(l["path"], "test-hook-77");
    let r = call(&c.state, "POST", "/hooks/test/test-hook-77", None, Some(json!({ "email": "ana@acme.example" }))).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{:?}", r.body);
    c.work().await;
    let run = c.run(r.body["run_id"].as_str().unwrap()).await;
    assert_eq!((run["mode"].as_str(), run["status"].as_str()), (Some("test"), Some("success")), "{run}");
    assert_eq!(run["trigger_item"]["body"]["email"], "ana@acme.example");
    assert_eq!(c.runs(&wf, "production").await.len(), 0);

    // one request only
    let again = call(&c.state, "POST", "/hooks/test/test-hook-77", None, Some(json!({}))).await;
    assert_eq!(again.body["error"]["code"], "webhook_not_listening");
    // the production URL is a different thing
    assert_eq!(call(&c.state, "POST", "/hooks/test-hook-77", None, Some(json!({}))).await.body["error"]["code"], "webhook_not_found");
    // a node that is not a webhook cannot listen
    let bad = c.call("POST", &format!("{WF}/{wf}/listen"), Some(json!({ "node_id": "tag" }))).await;
    assert_eq!(bad.body["error"]["code"], "invalid_trigger_node");
}

#[sqlx::test]
async fn a_listen_ends_after_two_minutes(pool: PgPool) {
    let h = Harness::at(pool, utc(2026, 10, 26, 10, 0));
    let c = Ctx::new(h.state.clone()).await;
    let wf = c.make(hook_flow(json!({ "path": "late-hook-1" }))).await;
    c.ok("POST", &format!("{WF}/{wf}/listen"), Some(json!({ "node_id": "start" }))).await;
    h.advance(Duration::seconds(121));
    let r = call(&c.state, "POST", "/hooks/test/late-hook-1", None, Some(json!({}))).await;
    assert_eq!(r.body["error"]["code"], "webhook_not_listening");
}

// ---------- status, alerts, dev actions ----------

#[sqlx::test]
async fn a_failed_production_run_shows_in_the_alerts_and_the_dev_tick_runs_the_triggers(pool: PgPool) {
    let h = Harness::at(pool, utc(2026, 10, 26, 10, 0));
    let c = Ctx::new(h.state.clone()).await;
    let d = json!({
        "format": "aikido-workflow", "version": 1, "name": "Fails",
        "nodes": [{ "id": "start", "type": "trigger.schedule", "params": { "mode": "interval", "every": 5, "unit": "minutes" } }, { "id": "stop", "type": "flow.stop", "params": { "message": "boom", "error": true } }],
        "edges": [{ "from": "start", "to": "stop" }]
    });
    let wf = c.go(d).await;
    h.advance(Duration::minutes(6));
    let msg = call(&c.state, "POST", "/v1/dev/actions/automations_trigger_tick", Some(&c.keys[&Scope::Admin]), None).await;
    assert_eq!(msg.status, StatusCode::OK, "{:?}", msg.body);
    let msg = call(&c.state, "POST", "/v1/dev/actions/automations_tick", Some(&c.keys[&Scope::Admin]), None).await;
    assert_eq!(msg.status, StatusCode::OK, "{:?}", msg.body);
    let runs = c.runs(&wf, "production").await;
    assert_eq!((runs.len(), runs[0]["status"].as_str()), (1, Some("error")));

    let alerts = aikido_email::ui_api::alerts::collect(&c.state).await.unwrap();
    assert!(alerts.iter().any(|a| a.kind == "automation_failed" && a.id.ends_with(&wf)), "{alerts:?}");
    let status = c.ok("GET", "/v1/automations/status", None).await;
    assert_eq!(status["active_triggers"], 1);
    assert_eq!(c.runs(&wf, "test").await.len(), 0);
}

#[sqlx::test]
async fn an_inactive_publisher_raises_an_alert_and_the_run_fails_with_its_reason(pool: PgPool) {
    let c = Ctx::new(app_state(pool)).await;
    let wf = c.make(manual("x")).await;
    // published by a session, so production runs act as that user
    let p = c.owner.call(&c.state, "POST", &format!("{WF}/{wf}/publish"), Some(json!({}))).await;
    assert_eq!(p.status, StatusCode::OK, "{:?}", p.body);
    c.activate(&wf, json!({})).await;
    let (run_as,): (Option<Uuid>,) = sqlx::query_as("SELECT run_as_user_id FROM automation_workflows WHERE id = $1::uuid").bind(&wf).fetch_one(&c.state.pool).await.unwrap();
    assert!(run_as.is_some(), "the publisher is the user the runs act as");

    sqlx::query("UPDATE users SET status = 'disabled'").execute(&c.state.pool).await.unwrap();
    let started = c.ok("POST", &format!("{WF}/{wf}/run"), Some(json!({}))).await;
    c.work().await;
    let run = c.run(started["run_id"].as_str().unwrap()).await;
    assert_eq!((run["status"].as_str(), run["error"]["code"].as_str()), (Some("error"), Some("run_as_user_inactive")), "{run}");
    let alerts = aikido_email::ui_api::alerts::collect(&c.state).await.unwrap();
    assert!(alerts.iter().any(|a| a.kind == "automation_run_as_inactive"), "{alerts:?}");
}

// ---------- the main story (build map D2) ----------

#[sqlx::test]
async fn the_main_story_a_workflow_writes_the_primary_email_before_the_feeder_enrolls_the_lead(pool: PgPool) {
    let t = Camp::new(pool).await;
    t.ai_mode().await;
    let wf_def = json!({
        "format": "aikido-workflow", "version": 1, "name": "Research",
        "variables": [{ "key": "campaign_id", "kind": "campaign", "value": t.camp, "required": true }],
        "nodes": [
            { "id": "on_lead", "type": "trigger.lead_added_to_campaign", "params": { "campaign_id": "${{ vars.campaign_id }}" } },
            { "id": "write", "type": "ai.agent", "params": {
                "provider": "default_draft",
                "prompt": "Write to ${{ trigger.lead.first_name }} at ${{ trigger.lead.company_name }}.",
                "output": { "format": "json", "fields": [{ "key": "subject", "type": "string" }, { "key": "body", "type": "string" }] } } },
            { "id": "save", "type": "aikido.campaign.save_lead_email", "params": {
                "campaign_id": "${{ trigger.campaign.id }}", "lead_id": "${{ trigger.lead.id }}", "step": 1,
                "subject": "${{ nodes.write.json.subject }}", "body_text": "${{ nodes.write.json.body }}" } }
        ],
        "edges": [{ "from": "on_lead", "to": "write" }, { "from": "write", "to": "save" }]
    });
    let wf = t.c.go(wf_def).await;
    t.g.fake.push(Ok(aikido_email::generator::llm::fake::response("{\"subject\": \"Quick idea for Acme\", \"body\": \"Hi Ana, I read your site.\"}")));

    let lead = t.add("ana@acme.example", "Ana").await;
    t.g.ok(Scope::Campaigns, "POST", &format!("/v1/campaigns/{}/launch", t.camp), None).await;
    // the lead waits: its primary email is not written yet
    assert_eq!(feeder::tick(&t.c.state).await.unwrap().enrolled, 0);

    assert_eq!(ticks(&t.events().await), (1, 1, 0));
    t.c.work().await;
    let runs = t.c.runs(&wf, "production").await;
    let run = t.c.run(runs[0]["id"].as_str().unwrap()).await;
    assert_eq!(run["status"], "success", "{run}");
    let email = t.g.ok(Scope::Campaigns, "GET", &format!("/v1/campaigns/{}/leads/{lead}/emails/1", t.camp), None).await;
    assert_eq!((email["status"].as_str(), email["subject"].as_str()), (Some("ready"), Some("Quick idea for Acme")), "{email}");

    // now the feeder enrolls it, and the sent email carries the text of the workflow
    assert_eq!(feeder::tick(&t.c.state).await.unwrap().enrolled, 1);
    let (e,): (Uuid,) = sqlx::query_as("SELECT id FROM enrollments LIMIT 1").fetch_one(&t.c.state.pool).await.unwrap();
    t.g.until_sent(e, 1).await;
    let (subject, body, _) = t.g.job_text(e, 1).await;
    assert_eq!(subject, "Quick idea for Acme");
    assert!(body.unwrap().contains("I read your site."));
    // auto-write is off and the email is edited by hand: the AI writer never touched it
    assert_eq!(t.g.fake.call_count(), 1, "only the workflow called the model");
    let (n,): (i64,) = sqlx::query_as("SELECT count(*) FROM lead_emails WHERE campaign_id = $1::uuid AND edited_by_hand").bind(&t.camp).fetch_one(&t.c.state.pool).await.unwrap();
    assert_eq!(n, 1);
}
