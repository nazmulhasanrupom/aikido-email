//! Tests of the phase 3 nodes: the AIKIDO nodes (against a fake that records the calls), `aikido.api`,
//! HTTP Request, Fetch web page, dry runs and the secrets. No database is needed.
use super::ai_backend::{AiBackend, AiCall, AiProvider};
use super::env::{MAX_CALLS_PER_RUN, NodeEnv};
use super::eval::{RunFacts, RunMode};
use super::run::{Outcome, RunSpec, execute};
use super::state::RunState;
use super::store::{MemStore, NodeStatus};
use super::Control;
use crate::automations::definition::WorkflowDefinition;
use crate::automations::internal_call::{CallRequest, CallResponse, Internal, OperationIndex, Who};
use crate::automations::registry::NodeError;
use crate::automations::secrets::Secrets;
use crate::automations::settings::AutomationSettings;
use crate::net::safe_fetch::{Resolver, SafeFetcher, ip_blocked};
use crate::users::model::Role;
use async_trait::async_trait;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const LEAD: &str = "0192f3a0-0000-7000-8000-000000000001";
const CAMP: &str = "0192f3a0-0000-7000-8000-000000000002";
const ACCT: &str = "0192f3a0-0000-7000-8000-000000000003";
const MSG: &str = "0192f3a0-0000-7000-8000-000000000004";
const ENR: &str = "0192f3a0-0000-7000-8000-000000000005";

// ---------- the fake caller ----------

type Handler = Box<dyn Fn(&CallRequest) -> CallResponse + Send + Sync>;

struct Fake {
    calls: Mutex<Vec<CallRequest>>,
    handler: Handler,
}

impl Fake {
    fn new(handler: impl Fn(&CallRequest) -> CallResponse + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Fake { calls: Mutex::new(vec![]), handler: Box::new(handler) })
    }
    /// Answers every call with a body that fits every shape.
    fn ok() -> Arc<Self> {
        Fake::new(|_| CallResponse {
            status: 200,
            body: json!({ "id": "x", "items": [{ "index": 0, "status": "queued", "lead_id": LEAD, "job": { "id": "J1" }, "error": null }], "leads": [], "events": [], "messages": [], "next_cursor": null }),
        })
    }
    fn calls(&self) -> Vec<CallRequest> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl Internal for Fake {
    async fn call(&self, _who: &Who, req: CallRequest) -> Result<CallResponse, NodeError> {
        self.calls.lock().unwrap().push(req.clone());
        Ok((self.handler)(&req))
    }
}

// ---------- a fake DNS ----------

struct Dns(HashMap<&'static str, Vec<IpAddr>>);

#[async_trait]
impl Resolver for Dns {
    async fn resolve(&self, host: &str, _port: u16) -> std::io::Result<Vec<IpAddr>> {
        self.0.get(host).cloned().ok_or_else(|| std::io::Error::other("no such host"))
    }
}

fn ip(s: &str) -> IpAddr {
    s.parse().unwrap()
}

/// A fetcher with the real block rule and a fake DNS. `local.test` is the loopback, `evil.test` is private.
fn fetcher() -> SafeFetcher {
    let m = HashMap::from([("local.test", vec![ip("127.0.0.1")]), ("evil.test", vec![ip("10.0.0.5")]), ("public.test", vec![ip("93.184.216.34")])]);
    SafeFetcher::with(Arc::new(Dns(m)), Arc::new(ip_blocked), vec![80, 443])
}

/// Like `fetcher`, but the loopback is let through for `local.test` (the page nodes never allow private addresses).
fn local_fetcher(port: u16) -> SafeFetcher {
    let m = HashMap::from([("local.test", vec![ip("127.0.0.1")]), ("evil.test", vec![ip("10.0.0.5")]), ("blocked.test", vec![ip("127.0.0.1")])]);
    SafeFetcher::with(Arc::new(Dns(m)), Arc::new(|a: &IpAddr| !a.is_loopback() && ip_blocked(a)), vec![port])
}

// ---------- the harness ----------

struct Setup {
    ai: Option<Arc<dyn AiBackend>>,
    internal: Arc<dyn Internal>,
    fetcher: SafeFetcher,
    secrets: BTreeMap<String, String>,
    settings: AutomationSettings,
    workflows: Option<Arc<dyn super::sub_workflow::WorkflowLoader>>,
}

impl Setup {
    fn new(internal: Arc<dyn Internal>) -> Self {
        Setup { ai: None, internal, fetcher: fetcher(), secrets: BTreeMap::new(), settings: AutomationSettings::default(), workflows: None }
    }
}

struct Res {
    outcome: Outcome,
    state: RunState,
    store: MemStore,
    env: Arc<NodeEnv>,
}

impl Res {
    fn items(&self, node: &str) -> Vec<Value> {
        self.state.port_items(node, "main").into_iter().map(|i| Value::Object(i.json)).collect()
    }
    fn error(&self) -> Value {
        match &self.outcome {
            Outcome::Error(e) => e.clone(),
            other => panic!("expected an error, got {other:?}"),
        }
    }
    fn code(&self) -> String {
        self.error()["code"].as_str().unwrap_or_default().to_string()
    }
    /// Everything the run kept: records, events and the checkpoint, as one text.
    fn everything_stored(&self) -> String {
        let records = serde_json::to_string(&self.store.records.lock().unwrap().iter().map(|r| json!({ "o": r.output, "e": r.error })).collect::<Vec<_>>()).unwrap();
        let events = serde_json::to_string(&*self.store.events.lock().unwrap()).unwrap();
        let state = serde_json::to_string(&self.store.state().map(|s| s.outputs.values().flat_map(|o| o.items.iter().map(|i| i.json.clone())).collect::<Vec<_>>())).unwrap();
        format!("{records}\n{events}\n{state}\n{:?}", self.outcome)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// A production run: nodes really call.
    Production,
    /// A test run without "Real writes".
    TestDry,
    /// A test run with "Real writes".
    TestReal,
}

async fn run_def(def: Value, trigger: Value, setup: Setup, mode: Mode) -> Res {
    let (spec, env) = build(def, trigger, setup, mode);
    let store = MemStore::new();
    let mut state = RunState::default();
    let outcome = execute(&spec, &mut state, &store, &Control::new()).await.expect("the store works");
    Res { outcome, state, store, env }
}

fn build(def: Value, trigger: Value, setup: Setup, mode: Mode) -> (RunSpec, Arc<NodeEnv>) {
    let def: WorkflowDefinition = serde_json::from_value(def).expect("a definition");
    let mut facts = RunFacts::test("start", trigger.as_object().cloned().unwrap_or_default());
    facts.mode = if mode == Mode::Production { RunMode::Production } else { RunMode::Test };
    facts.real_writes = mode == Mode::TestReal;
    let st = crate::automations::testkit::lazy_state();
    let ops: Arc<OperationIndex> = st.internal_router.operations(&st);
    let secrets = Arc::new(Secrets::new(setup.secrets));
    let who = Who { user_id: Uuid::now_v7(), role: Role::Owner, run_id: facts.run_id.clone(), workflow_id: facts.workflow_id.clone() };
    let mut env = NodeEnv::new(setup.internal, Arc::new(setup.fetcher), secrets.clone(), setup.settings, ops, who, Some("https://app.example".into()));
    env.test_run = mode != Mode::Production;
    env.ai = setup.ai;
    env.workflows = setup.workflows;
    let env = Arc::new(env);
    let mut spec = RunSpec::new(def, facts);
    spec.secrets = secrets;
    spec.env = Some(env.clone());
    (spec, env)
}

/// A manual trigger and one node after it.
fn one_node(type_id: &str, params: Value) -> Value {
    json!({
        "format": "aikido-workflow", "version": 1, "name": "T",
        "nodes": [{ "id": "start", "type": "trigger.manual" }, { "id": "n", "type": type_id, "params": params }],
        "edges": [{ "from": "start", "to": "n" }]
    })
}

async fn run_node(type_id: &str, params: Value, trigger: Value, setup: Setup, mode: Mode) -> Res {
    run_def(one_node(type_id, params), trigger, setup, mode).await
}

// ---------- the AIKIDO nodes: each node calls the right route ----------

fn call(method: &str, path: &str) -> CallRequest {
    CallRequest::new(method, path)
}

#[tokio::test]
async fn every_curated_aikido_node_calls_its_route() {
    let a = ACCT;
    #[rustfmt::skip]
    let table: Vec<(&str, Value, CallRequest)> = vec![
        ("aikido.lead.get", json!({ "lead_id": LEAD }), call("GET", &format!("/v1/leads/{LEAD}"))),
        ("aikido.lead.context", json!({ "lead_id": LEAD }), call("GET", &format!("/v1/leads/{LEAD}/context"))),
        ("aikido.lead.find", json!({ "filter": { "q": "ana", "suppressed": false, "list_id": CAMP }, "max_results": 5 }),
            call("GET", "/v1/leads").query("q", "ana").query("list_id", CAMP).query("suppressed", "false").query("limit", 5)),
        ("aikido.lead.create", json!({ "email": "a@b.co", "first_name": "A" }), call("POST", "/v1/leads").body(json!({ "email": "a@b.co", "first_name": "A" }))),
        ("aikido.lead.upsert", json!({ "lead": { "email": "a@b.co" }, "dedupe_mode": "overwrite", "list_id": CAMP }),
            call("POST", "/v1/leads/bulk").body(json!({ "dedupe_mode": "overwrite", "list_id": CAMP, "leads": [{ "email": "a@b.co" }] }))),
        ("aikido.lead.update", json!({ "lead_id": LEAD, "fields": { "first_name": "Z", "custom_fields": { "pain": "slow" } } }),
            call("PATCH", &format!("/v1/leads/{LEAD}")).body(json!({ "first_name": "Z", "custom_fields": { "pain": "slow" } }))),
        ("aikido.lead.add_note", json!({ "lead_id": LEAD, "text": "Researched." }), call("POST", &format!("/v1/leads/{LEAD}/notes")).body(json!({ "text": "Researched." }))),
        ("aikido.lead.action", json!({ "action": "add_tags", "lead_ids": [LEAD], "tags": ["hot"] }),
            call("POST", "/v1/leads/actions").body(json!({ "action": "add_tags", "lead_ids": [LEAD], "tags": ["hot"] }))),
        ("aikido.lead.events", json!({ "lead_id": LEAD }), call("GET", &format!("/v1/leads/{LEAD}/events")).query("limit", 500)),
        ("aikido.list.create", json!({ "name": "Founders" }), call("POST", "/v1/leads/lists").body(json!({ "name": "Founders" }))),
        ("aikido.campaign.get", json!({ "campaign_id": CAMP }), call("GET", &format!("/v1/campaigns/{CAMP}"))),
        ("aikido.campaign.leads", json!({ "campaign_id": CAMP, "state": "pending", "max_results": 10 }), call("GET", &format!("/v1/campaigns/{CAMP}/leads")).query("state", "pending").query("limit", 10)),
        ("aikido.campaign.add_leads", json!({ "campaign_id": CAMP, "lead_ids": [LEAD] }), call("POST", &format!("/v1/campaigns/{CAMP}/leads")).body(json!({ "lead_ids": [LEAD] }))),
        ("aikido.campaign.save_lead_email", json!({ "campaign_id": CAMP, "lead_id": LEAD, "step": 1, "subject": "Hi", "body_text": "Text" }),
            call("PATCH", &format!("/v1/campaigns/{CAMP}/leads/{LEAD}/emails/1")).body(json!({ "subject": "Hi", "body_text": "Text" }))),
        ("aikido.campaign.get_lead_email", json!({ "campaign_id": CAMP, "lead_id": LEAD, "step": 2 }), call("GET", &format!("/v1/campaigns/{CAMP}/leads/{LEAD}/emails/2"))),
        ("aikido.campaign.status", json!({ "campaign_id": CAMP, "action": "pause" }), call("POST", &format!("/v1/campaigns/{CAMP}/pause")).body(json!({}))),
        ("aikido.campaign.stats", json!({ "campaign_id": CAMP }), call("GET", &format!("/v1/campaigns/{CAMP}/stats"))),
        ("aikido.reply.list", json!({ "label": "reply", "max_results": 3 }), call("GET", "/v1/replies/messages").query("label", "reply").query("limit", 3)),
        ("aikido.reply.set_label", json!({ "message_id": MSG, "label": "reply" }), call("PATCH", &format!("/v1/replies/messages/{MSG}")).body(json!({ "label": "reply" }))),
        ("aikido.suppression.add", json!({ "value": "Ana@Acme.com", "reason": "manual" }), call("POST", "/v1/sending/suppressions").body(json!({ "value": "ana@acme.com", "kind": "email", "reason": "manual" }))),
        ("aikido.enrollment.stop", json!({ "enrollment_id": ENR }), call("POST", &format!("/v1/followups/enrollments/{ENR}/stop")).body(json!({}))),
        ("aikido.generator.campaign_test", json!({ "campaign_id": CAMP, "lead_id": LEAD }), call("POST", &format!("/v1/campaigns/{CAMP}/emails/test")).body(json!({ "lead_id": LEAD, "step": 1 }))),
        ("aikido.verify.reoon", json!({ "lead_ids": [LEAD] }), call("POST", "/v1/verify/reoon/runs").body(json!({ "lead_ids": [LEAD] }))),
    ];
    let _ = a;
    for (type_id, params, want) in table {
        let fake = Fake::ok();
        let r = run_node(type_id, params, json!({}), Setup::new(fake.clone()), Mode::Production).await;
        assert_eq!(r.outcome, Outcome::Success, "{type_id}: {:?}", r.outcome);
        let calls = fake.calls();
        assert_eq!(calls.len(), 1, "{type_id}: {calls:?}");
        let mut got = calls[0].clone();
        // the order of the query pairs does not matter
        got.query.sort();
        let mut want = want;
        want.query.sort();
        assert_eq!(got, want, "{type_id}");
    }
}

#[tokio::test]
async fn a_dry_test_run_shows_the_call_and_sends_nothing() {
    let fake = Fake::ok();
    let d = json!({
        "format": "aikido-workflow", "version": 1, "name": "T",
        "nodes": [
            { "id": "start", "type": "trigger.manual" },
            { "id": "save", "type": "aikido.campaign.save_lead_email", "params": { "campaign_id": CAMP, "lead_id": LEAD, "step": 1, "subject": "S", "body_text": "B" } },
            { "id": "read", "type": "aikido.lead.get", "params": { "lead_id": LEAD } }
        ],
        "edges": [{ "from": "start", "to": "save" }, { "from": "save", "to": "read" }]
    });
    let r = run_def(d.clone(), json!({}), Setup::new(fake.clone()), Mode::TestDry).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    // the write is dry, the read really ran
    let calls = fake.calls();
    assert_eq!(calls, vec![call("GET", &format!("/v1/leads/{LEAD}"))], "only the read goes out");
    let dry = &r.items("save")[0];
    assert_eq!(dry["dry_run"], true);
    assert_eq!(dry["would_call"]["method"], "PATCH");
    assert_eq!(dry["would_call"]["path"], format!("/v1/campaigns/{CAMP}/leads/{LEAD}/emails/1"));
    assert_eq!(dry["would_call"]["body"], json!({ "subject": "S", "body_text": "B" }));
    assert_eq!(r.state.outputs["save"].items.len(), 1);
    // "Real writes": both go out
    let fake = Fake::ok();
    run_def(d, json!({}), Setup::new(fake.clone()), Mode::TestReal).await;
    assert_eq!(fake.calls().len(), 2);
}

#[tokio::test]
async fn paged_lists_follow_the_cursor_and_stop_at_max_results() {
    // three pages of 2 leads; max 5
    let fake = Fake::new(|req| {
        let cursor = req.query.iter().find(|(k, _)| k == "cursor").map(|(_, v)| v.clone());
        let (leads, next) = match cursor.as_deref() {
            None => (vec!["a", "b"], Some("c1")),
            Some("c1") => (vec!["c", "d"], Some("c2")),
            _ => (vec!["e", "f"], None),
        };
        CallResponse { status: 200, body: json!({ "leads": leads.iter().map(|l| json!({ "email": l })).collect::<Vec<_>>(), "next_cursor": next }) }
    });
    let r = run_node("aikido.lead.find", json!({ "max_results": 5 }), json!({}), Setup::new(fake.clone()), Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success);
    let emails: Vec<String> = r.items("n").iter().map(|i| i["email"].as_str().unwrap().to_string()).collect();
    assert_eq!(emails, vec!["a", "b", "c", "d", "e"]);
    assert_eq!(fake.calls().len(), 3);
    // the last page asks only for what is left
    assert!(fake.calls()[2].query.contains(&("limit".to_string(), "1".to_string())), "{:?}", fake.calls()[2]);
    // with a smaller max, one page is enough
    let fake2 = Fake::new(|_| CallResponse { status: 200, body: json!({ "leads": [{ "email": "a" }, { "email": "b" }], "next_cursor": "c1" }) });
    let r = run_node("aikido.lead.find", json!({ "max_results": 2 }), json!({}), Setup::new(fake2.clone()), Mode::Production).await;
    assert_eq!(r.items("n").len(), 2);
    assert_eq!(fake2.calls().len(), 1);
}

#[tokio::test]
async fn a_refused_call_becomes_a_node_error_with_the_route_code() {
    let fake = Fake::new(|_| CallResponse { status: 409, body: json!({ "error": { "code": "email_exists", "message": "A lead with the email a@b.co exists." } }) });
    let r = run_node("aikido.lead.create", json!({ "email": "a@b.co" }), json!({}), Setup::new(fake), Mode::Production).await;
    let e = r.error();
    assert_eq!(e["code"], "aikido_email_exists");
    assert_eq!(e["status"], 409);
    assert!(e["message"].as_str().unwrap().contains("exists"));
    // a body that is not the error shape
    let fake = Fake::new(|_| CallResponse { status: 502, body: Value::Null });
    let r = run_node("aikido.lead.get", json!({ "lead_id": LEAD }), json!({}), Setup::new(fake), Mode::Production).await;
    assert_eq!(r.code(), "aikido_http_502");
}

#[tokio::test]
async fn ids_are_checked_so_a_value_can_not_change_the_path() {
    let fake = Fake::ok();
    for bad in ["../keys", "abc", "", "0192f3a0/x"] {
        let r = run_node("aikido.lead.get", json!({ "lead_id": bad }), json!({}), Setup::new(fake.clone()), Mode::Production).await;
        assert!(matches!(r.code().as_str(), "invalid_param_value"), "{bad:?}: {:?}", r.outcome);
    }
    assert!(fake.calls().is_empty());
}

#[tokio::test]
async fn lead_delete_is_not_offered() {
    let fake = Fake::ok();
    let r = run_node("aikido.lead.action", json!({ "action": "delete", "lead_ids": [LEAD] }), json!({}), Setup::new(fake.clone()), Mode::Production).await;
    assert_eq!(r.code(), "invalid_param_value");
    assert!(fake.calls().is_empty());
}

#[tokio::test]
async fn bulk_nodes_give_the_first_entry_only() {
    let fake = Fake::ok();
    let r = run_node("aikido.lead.upsert", json!({ "lead": { "email": "a@b.co" } }), json!({}), Setup::new(fake), Mode::Production).await;
    assert_eq!(r.items("n"), vec![json!({ "status": "queued", "lead_id": LEAD, "error": null })]);
}

// ---------- send email: idempotency and threads ----------

#[tokio::test]
async fn send_has_a_stable_idempotency_key_and_the_same_item_twice_makes_one_job() {
    let params = json!({ "account_id": ACCT, "to_email": "ana@acme.com", "subject": "Hi", "body_text": "Text", "lead_id": LEAD });
    let fake = Fake::ok();
    let r = run_node("aikido.email.send", params.clone(), json!({}), Setup::new(fake.clone()), Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success);
    let job = fake.calls()[0].body.clone().unwrap()["jobs"][0].clone();
    assert_eq!(job["idempotency_key"], format!("wf:{}:n:0", r.env.who.run_id));
    assert_eq!(job["kind"], "manual");
    assert_eq!(job["to_email"], "ana@acme.com");
    // a run that goes on after a crash sends the same key again, so the sending queue answers `duplicate`
    let (a, b) = (job["idempotency_key"].clone(), {
        let again = Fake::ok();
        let mut setup = Setup::new(again.clone());
        setup.settings = AutomationSettings::default();
        // same run id: build the run by hand
        let def: WorkflowDefinition = serde_json::from_value(one_node("aikido.email.send", params.clone())).unwrap();
        let mut facts = RunFacts::test("start", Map::new());
        facts.run_id = r.env.who.run_id.clone();
        facts.mode = RunMode::Production;
        let st = crate::automations::testkit::lazy_state();
        let who = Who { user_id: Uuid::now_v7(), role: Role::Owner, run_id: facts.run_id.clone(), workflow_id: facts.workflow_id.clone() };
        let env = NodeEnv::new(again.clone(), Arc::new(fetcher()), Arc::new(Secrets::default()), AutomationSettings::default(), st.internal_router.operations(&st), who, None);
        let mut spec = RunSpec::new(def, facts);
        spec.env = Some(Arc::new(env));
        execute(&spec, &mut RunState::default(), &MemStore::new(), &Control::new()).await.unwrap();
        again.calls()[0].body.clone().unwrap()["jobs"][0]["idempotency_key"].clone()
    });
    assert_eq!(a, b);

    // each item of a list gets its own key
    let fake = Fake::ok();
    let d = json!({
        "format": "aikido-workflow", "version": 1, "name": "T",
        "nodes": [
            { "id": "start", "type": "trigger.manual" },
            { "id": "split", "type": "data.split_out", "params": { "path": "people" } },
            { "id": "n", "type": "aikido.email.send", "params": { "account_id": ACCT, "to_email": "${{ json.email }}", "subject": "Hi", "body_text": "T" } }
        ],
        "edges": [{ "from": "start", "to": "split" }, { "from": "split", "to": "n" }]
    });
    run_def(d, json!({ "people": [{ "email": "a@x.co" }, { "email": "b@x.co" }] }), Setup::new(fake.clone()), Mode::Production).await;
    let keys: Vec<String> = fake.calls().iter().map(|c| c.body.clone().unwrap()["jobs"][0]["idempotency_key"].as_str().unwrap().to_string()).collect();
    assert_eq!(keys.len(), 2);
    assert!(keys[0].ends_with(":n:0") && keys[1].ends_with(":n:1"), "{keys:?}");

    // a key written by the user is kept
    let fake = Fake::ok();
    let mut p = params;
    p["idempotency_key"] = json!("mine-1");
    run_node("aikido.email.send", p, json!({}), Setup::new(fake.clone()), Mode::Production).await;
    assert_eq!(fake.calls()[0].body.clone().unwrap()["jobs"][0]["idempotency_key"], "mine-1");
}

#[tokio::test]
async fn reply_fills_the_thread_from_the_message() {
    let msg = json!({
        "id": MSG, "account_id": ACCT, "from_email": "ana@acme.com", "message_id_header": "<m3@acme.com>",
        "references_ids": ["<m1@x>", "<m2@x>"], "provider_thread_id": "T1", "lead_id": LEAD, "campaign_id": CAMP
    });
    let fake = Fake::ok();
    let r = run_node(
        "aikido.email.reply",
        json!({ "message": "${{ trigger.message }}", "subject": "Re: Hi", "body_text": "Thanks" }),
        json!({ "message": msg }),
        Setup::new(fake.clone()),
        Mode::Production,
    )
    .await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    let job = fake.calls()[0].body.clone().unwrap()["jobs"][0].clone();
    assert_eq!(job["in_reply_to"], "<m3@acme.com>");
    assert_eq!(job["references"], "<m1@x> <m2@x> <m3@acme.com>");
    assert_eq!(job["provider_thread_id"], "T1");
    assert_eq!(job["account_id"], ACCT);
    assert_eq!(job["to_email"], "ana@acme.com");
    assert_eq!(job["lead_id"], LEAD);
    assert_eq!(job["priority"], 100);
    assert_eq!(job["subject"], "Re: Hi");
}

// ---------- aikido.api ----------

#[tokio::test]
async fn the_generic_node_fills_paths_and_splits_lists() {
    let fake = Fake::new(|req| CallResponse { status: 200, body: json!({ "path": req.path, "leads": [{ "email": "a" }, { "email": "b" }] }) });
    let r = run_node("aikido.api", json!({ "operation": "GET /v1/leads/{id}", "path": { "id": LEAD } }), json!({}), Setup::new(fake.clone()), Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    assert_eq!(fake.calls()[0], call("GET", &format!("/v1/leads/{LEAD}")));
    assert_eq!(r.items("n")[0]["path"], format!("/v1/leads/{LEAD}"));
    // split
    let r = run_node("aikido.api", json!({ "operation": "GET /v1/leads", "query": { "limit": 2 }, "split": "leads" }), json!({}), Setup::new(fake.clone()), Mode::Production).await;
    assert_eq!(r.items("n").len(), 2);
    assert_eq!(fake.calls()[1], call("GET", "/v1/leads").query("limit", 2));
    // a body
    let r = run_node("aikido.api", json!({ "operation": "POST /v1/leads/lists", "body": { "name": "X" } }), json!({}), Setup::new(fake.clone()), Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success);
    assert_eq!(fake.calls()[2], call("POST", "/v1/leads/lists").body(json!({ "name": "X" })));
    // a missing path value
    let r = run_node("aikido.api", json!({ "operation": "GET /v1/leads/{id}" }), json!({}), Setup::new(fake.clone()), Mode::Production).await;
    assert_eq!(r.code(), "invalid_param_value");
}

#[tokio::test]
async fn the_generic_node_refuses_the_deny_list_at_run_time() {
    for (op, path) in [("POST /v1/keys", json!({})), ("GET /v1/users/me", json!({})), ("POST /v1/dev/sql", json!({})), ("GET /v1/automations/workflows", json!({})), ("DELETE /v1/leads/{id}", json!({ "id": LEAD }))] {
        let fake = Fake::ok();
        let r = run_node("aikido.api", json!({ "operation": op, "path": path }), json!({}), Setup::new(fake.clone()), Mode::Production).await;
        assert_eq!(r.code(), "endpoint_not_allowed", "{op}: {:?}", r.outcome);
        assert!(fake.calls().is_empty(), "{op} must not be called");
    }
    // the owner can allow lead erase
    let fake = Fake::ok();
    let mut setup = Setup::new(fake.clone());
    setup.settings.allow_lead_erase = true;
    let r = run_node("aikido.api", json!({ "operation": "DELETE /v1/leads/{id}", "path": { "id": LEAD } }), json!({}), setup, Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    // and even then keys stay denied
    let mut setup = Setup::new(Fake::ok());
    setup.settings.allow_lead_erase = true;
    assert_eq!(run_node("aikido.api", json!({ "operation": "GET /v1/keys" }), json!({}), setup, Mode::Production).await.code(), "endpoint_not_allowed");
}

#[tokio::test]
async fn the_generic_node_suggests_a_close_operation() {
    let r = run_node("aikido.api", json!({ "operation": "GET /v1/lead/{id}" }), json!({}), Setup::new(Fake::ok()), Mode::Production).await;
    let e = r.error();
    assert_eq!(e["code"], "unknown_operation");
    assert!(e["message"].as_str().unwrap().contains("GET /v1/leads/{id}"), "{e}");
}

#[tokio::test]
async fn the_generic_node_is_dry_only_when_it_changes_data() {
    let fake = Fake::ok();
    // a GET is a read: it runs in a test run
    let r = run_node("aikido.api", json!({ "operation": "GET /v1/leads/{id}", "path": { "id": LEAD } }), json!({}), Setup::new(fake.clone()), Mode::TestDry).await;
    assert_eq!(r.store.records_of("n")[0].status, NodeStatus::Success);
    assert_eq!(fake.calls().len(), 1);
    // a POST is dry
    let r = run_node("aikido.api", json!({ "operation": "POST /v1/leads/lists", "body": { "name": "X" } }), json!({}), Setup::new(fake.clone()), Mode::TestDry).await;
    assert_eq!(r.store.records_of("n")[0].status, NodeStatus::Dry);
    assert_eq!(fake.calls().len(), 1, "the dry node sent nothing");
    assert_eq!(r.items("n")[0]["would_call"], json!({ "method": "POST", "path": "/v1/leads/lists", "body": { "name": "X" } }));
}

#[tokio::test]
async fn the_validator_refuses_denied_operations() {
    let st = crate::automations::testkit::lazy_state();
    let ops = st.internal_router.operations(&st);
    let opts = crate::automations::validate::Options { operations: Some(ops.as_ref()), ..Default::default() };
    for (op, code) in [("POST /v1/keys", "endpoint_not_allowed"), ("GET /v1/automations/workflows", "endpoint_not_allowed"), ("DELETE /v1/leads/{id}", "endpoint_not_allowed"), ("GET /v1/nothing", "unknown_operation")] {
        let r = crate::automations::validate::validate_value(&one_node("aikido.api", json!({ "operation": op })), &opts);
        assert!(r.errors.iter().any(|e| e.code == code), "{op}: {:?}", r.errors);
    }
    let ok = crate::automations::validate::validate_value(&one_node("aikido.api", json!({ "operation": "GET /v1/leads" })), &opts);
    assert!(!ok.errors.iter().any(|e| e.code == "endpoint_not_allowed" || e.code == "unknown_operation"), "{:?}", ok.errors);
    // without the operation list the text is still checked
    let r = crate::automations::validate::validate_value(&one_node("aikido.api", json!({ "operation": "POST /v1/keys" })), &Default::default());
    assert!(r.errors.iter().any(|e| e.code == "endpoint_not_allowed"));
    // an expression is checked at run time
    let r = crate::automations::validate::validate_value(&one_node("aikido.api", json!({ "operation": "${{ json.op }}" })), &opts);
    assert!(!r.errors.iter().any(|e| e.code == "endpoint_not_allowed"));
}

// ---------- the call limit ----------

#[tokio::test]
async fn a_run_can_make_only_so_many_outside_calls() {
    let fake = Fake::ok();
    let r = run_node("aikido.lead.get", json!({ "lead_id": LEAD }), json!({}), Setup::new(fake), Mode::Production).await;
    assert_eq!(r.env.calls_made(), 1);
    for _ in 1..MAX_CALLS_PER_RUN {
        r.env.count_call().unwrap();
    }
    assert_eq!(r.env.count_call().unwrap_err().code, "too_many_calls");
}

// ---------- HTTP Request ----------

/// A local server. `/echo` answers the method, the headers and the body. Returns the port.
async fn web_server() -> u16 {
    use axum::routing::{any, get};
    let app = axum::Router::new()
        .route(
            "/echo",
            any(|method: axum::http::Method, headers: axum::http::HeaderMap, uri: axum::http::Uri, body: String| async move {
                let h: serde_json::Map<String, Value> = headers.iter().map(|(k, v)| (k.as_str().to_string(), json!(v.to_str().unwrap_or("")))).collect();
                axum::Json(json!({ "method": method.as_str(), "headers": h, "query": uri.query(), "body": body }))
            }),
        )
        .route("/list", get(|| async { axum::Json(json!({ "data": { "results": [{ "n": 1 }, { "n": 2 }, 3] } })) }))
        .route("/text", get(|| async { ([("content-type", "text/plain")], "plain words") }))
        .route("/missing", get(|| async { (axum::http::StatusCode::NOT_FOUND, axum::Json(json!({ "error": "nope" }))) }))
        .route("/fail", get(|uri: axum::http::Uri| async move { (axum::http::StatusCode::INTERNAL_SERVER_ERROR, format!("bad request: {}", uri.query().unwrap_or(""))) }))
        .route("/limited", get(|| async { (axum::http::StatusCode::TOO_MANY_REQUESTS, "slow down") }))
        .route("/go", get(|| async { axum::response::Redirect::temporary("http://evil.test/x") }))
        .route(
            "/robots.txt",
            get(|| async { ([("content-type", "text/plain")], "User-agent: *\nDisallow: /private\nAllow: /private/open\n") }),
        )
        .route("/page", get(|headers: axum::http::HeaderMap| async move {
            let ua = headers.get("user-agent").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
            ([("content-type", "text/html; charset=utf-8")], format!(
                "<html><head><title>Acme Rockets</title><meta name=\"description\" content=\"We build rockets\"></head><body><nav>menu</nav><h1>Hello</h1><p>{}</p><a href=\"/about\">About</a><p>UA:{ua}</p></body></html>",
                "word ".repeat(400)
            ))
        }))
        .route("/private/secret", get(|| async { ([("content-type", "text/html")], "<p>hidden</p>") }))
        .route("/private/open", get(|| async { ([("content-type", "text/html")], "<p>open</p>") }))
        .route("/image", get(|| async { ([("content-type", "image/png")], "png") }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    port
}

fn private_setup(port: u16) -> Setup {
    let mut s = Setup::new(Fake::ok());
    s.fetcher = local_fetcher(port);
    s.settings.allow_private_network = true;
    s
}

#[tokio::test]
async fn http_request_sends_json_and_gives_status_headers_and_body() {
    let port = web_server().await;
    let r = run_node(
        "http.request",
        json!({ "method": "POST", "url": format!("http://local.test:{port}/echo"), "query": { "a": "1 2" }, "headers": { "X-Test": "yes" }, "body": { "k": "${{ json.v }}" } }),
        json!({ "v": 7 }),
        private_setup(port),
        Mode::Production,
    )
    .await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    let out = &r.items("n")[0];
    assert_eq!(out["status"], 200);
    assert_eq!(out["body"]["method"], "POST");
    assert_eq!(out["body"]["query"], "a=1+2");
    assert_eq!(out["body"]["headers"]["x-test"], "yes");
    assert_eq!(out["body"]["headers"]["content-type"], "application/json");
    assert_eq!(serde_json::from_str::<Value>(out["body"]["body"].as_str().unwrap()).unwrap(), json!({ "k": 7 }));
    assert!(out["headers"]["content-type"].as_str().unwrap().contains("json"));
    assert!(out["url"].as_str().unwrap().contains("/echo"));
    assert!(!out["headers"].as_object().unwrap().contains_key("set-cookie"));
    // the user agent says who we are
    assert!(out["body"]["headers"]["user-agent"].as_str().unwrap().starts_with("AikidoEmail-Automation/"), "{out}");
}

#[tokio::test]
async fn http_request_form_text_and_split() {
    let port = web_server().await;
    let url = format!("http://local.test:{port}");
    let r = run_node("http.request", json!({ "method": "POST", "url": format!("{url}/echo"), "body_type": "form", "body": { "a": "x y", "b": 2 } }), json!({}), private_setup(port), Mode::Production).await;
    let out = &r.items("n")[0];
    assert_eq!(out["body"]["body"], "a=x%20y&b=2");
    assert_eq!(out["body"]["headers"]["content-type"], "application/x-www-form-urlencoded");
    let r = run_node("http.request", json!({ "method": "PUT", "url": format!("{url}/echo"), "body_type": "text", "body": "hello" }), json!({}), private_setup(port), Mode::Production).await;
    assert_eq!(r.items("n")[0]["body"]["body"], "hello");
    // text answer
    let r = run_node("http.request", json!({ "url": format!("{url}/text") }), json!({}), private_setup(port), Mode::Production).await;
    assert_eq!(r.items("n")[0]["body"], "plain words");
    // split: one item per element; a number is wrapped
    let r = run_node("http.request", json!({ "url": format!("{url}/list"), "split": "data.results" }), json!({}), private_setup(port), Mode::Production).await;
    assert_eq!(r.items("n"), vec![json!({ "n": 1 }), json!({ "n": 2 }), json!({ "value": 3 })]);
    let r = run_node("http.request", json!({ "url": format!("{url}/list"), "split": "data.nope" }), json!({}), private_setup(port), Mode::Production).await;
    assert_eq!(r.code(), "split_not_a_list");
}

#[tokio::test]
async fn http_request_fail_on_status() {
    let port = web_server().await;
    let url = format!("http://local.test:{port}");
    let r = run_node("http.request", json!({ "url": format!("{url}/limited") }), json!({}), private_setup(port), Mode::Production).await;
    let e = r.error();
    assert_eq!(e["code"], "http_status_429");
    assert_eq!(e["status"], 429);
    assert!(e["message"].as_str().unwrap().contains("slow down"));
    // fail_on_status false: the item has the status and the run goes on
    let r = run_node("http.request", json!({ "url": format!("{url}/missing"), "fail_on_status": false }), json!({}), private_setup(port), Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success);
    assert_eq!(r.items("n")[0]["status"], 404);
    assert_eq!(r.items("n")[0]["body"], json!({ "error": "nope" }));
}

#[tokio::test]
async fn http_request_auth_types_use_secrets_that_never_show() {
    let port = web_server().await;
    let url = format!("http://local.test:{port}/echo");
    let secret = "s3cr3t-Token/42+x";
    for (auth, header) in [
        (json!({ "type": "bearer", "secret": "API_KEY" }), "authorization"),
        (json!({ "type": "basic", "user": "ana", "secret": "API_KEY" }), "authorization"),
        (json!({ "type": "header", "header_name": "X-Api-Key", "secret": "API_KEY" }), "x-api-key"),
    ] {
        let mut setup = private_setup(port);
        setup.secrets.insert("API_KEY".into(), secret.into());
        // the run hides the secret and, for basic auth, the credentials made from it
        let def: WorkflowDefinition = serde_json::from_value(one_node("http.request", json!({ "url": url, "auth": auth }))).unwrap();
        let mut secrets = Secrets::new(setup.secrets.clone());
        for (user, name) in crate::automations::secrets::basic_auth_pairs(&def) {
            secrets.add_basic(&user, &name);
        }
        let mut facts = RunFacts::test("start", Map::new());
        facts.mode = RunMode::Production;
        let st = crate::automations::testkit::lazy_state();
        let who = Who { user_id: Uuid::now_v7(), role: Role::Owner, run_id: facts.run_id.clone(), workflow_id: facts.workflow_id.clone() };
        let secrets = Arc::new(secrets);
        let env = NodeEnv::new(setup.internal.clone(), Arc::new(setup.fetcher), secrets.clone(), setup.settings, st.internal_router.operations(&st), who, None);
        let mut spec = RunSpec::new(def, facts);
        spec.secrets = secrets;
        spec.env = Some(Arc::new(env));
        let store = MemStore::new();
        let mut state = RunState::default();
        let outcome = execute(&spec, &mut state, &store, &Control::new()).await.unwrap();
        assert_eq!(outcome, Outcome::Success, "{outcome:?}");
        // the server did get the header (it echoes it), and what the run kept is hidden
        let kept = state.outputs["n"].items[0].json["body"]["headers"][header].as_str().unwrap_or_default().to_string();
        assert!(kept.contains("[secret:API_KEY]"), "{header}: the echo of the header must be hidden, got {kept:?}");
        let everything = format!("{:?} {:?}", store.records.lock().unwrap(), store.events.lock().unwrap());
        for form in [secret.to_string(), base64_std(secret), base64_std(&format!("ana:{secret}")), crate::automations::internal_call::urlencode(secret)] {
            assert!(!everything.contains(&form), "{header}: the secret ({form}) is in the stored data");
        }
    }
    // a secret that does not exist
    let r = run_node("http.request", json!({ "url": url, "auth": { "type": "bearer", "secret": "NOPE" } }), json!({}), private_setup(port), Mode::Production).await;
    assert_eq!(r.code(), "secret_not_found");
}

fn base64_std(s: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(s)
}

#[tokio::test]
async fn secrets_in_expressions_errors_and_dry_runs_are_hidden() {
    let port = web_server().await;
    let secret = "hunter2-hunter2";
    // 1. a secret() in a header: the server echoes it, the stored output hides it
    let mut setup = private_setup(port);
    setup.secrets.insert("KEY".into(), secret.into());
    let r = run_node("http.request", json!({ "url": format!("http://local.test:{port}/echo"), "headers": { "X-K": "${{ secret('KEY') }}" }, "query": { "k": "${{ secret('KEY') }}" } }), json!({}), setup, Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    assert!(!r.everything_stored().contains(secret), "{}", r.everything_stored());
    assert!(r.everything_stored().contains("[secret:KEY]"));
    // 2. an error message that holds the secret (it is in the address)
    let mut setup = private_setup(port);
    setup.secrets.insert("KEY".into(), secret.into());
    let r = run_node("http.request", json!({ "url": format!("http://local.test:{port}/fail?key=${{{{ secret('KEY') }}}}") }), json!({}), setup, Mode::Production).await;
    assert_eq!(r.code(), "http_status_500");
    assert!(r.error()["message"].as_str().unwrap().contains("[secret:KEY]"), "the server echoed the key: {}", r.error());
    assert!(!r.everything_stored().contains(secret));
    // the same with on_error continue: the error rides on the item
    let mut setup = private_setup(port);
    setup.secrets.insert("KEY".into(), secret.into());
    let mut d = one_node("http.request", json!({ "url": format!("http://local.test:{port}/fail?key=${{{{ secret('KEY') }}}}") }));
    d["nodes"][1]["on_error"] = json!("continue");
    let r = run_def(d, json!({}), setup, Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success);
    assert!(!r.everything_stored().contains(secret), "{}", r.everything_stored());
    // 3. a dry run shows the call, not the secret
    let mut setup = Setup::new(Fake::ok());
    setup.secrets.insert("KEY".into(), secret.into());
    let r = run_node("aikido.lead.create", json!({ "email": "a@b.co", "notes": "key ${{ secret('KEY') }}" }), json!({}), setup, Mode::TestDry).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    assert!(!r.everything_stored().contains(secret));
    assert_eq!(r.items("n")[0]["would_call"]["body"]["notes"], "key [secret:KEY]");
}

#[tokio::test]
async fn http_request_refuses_private_addresses_and_redirects_to_them() {
    for url in ["http://127.0.0.1:8080/", "http://169.254.169.254/latest/meta-data/", "http://10.0.0.1/", "http://evil.test/", "http://[::1]:9000/", "http://192.168.1.1:3000/x"] {
        let r = run_node("http.request", json!({ "url": url }), json!({}), Setup::new(Fake::ok()), Mode::Production).await;
        assert_eq!(r.code(), "http_blocked", "{url}: {:?}", r.outcome);
    }
    // a name that does not exist is a plain network error
    let r = run_node("http.request", json!({ "url": "http://nothing.test/" }), json!({}), Setup::new(Fake::ok()), Mode::Production).await;
    assert_eq!(r.code(), "http_error");
    // a server we may reach (the test rule lets the loopback through) that redirects to a private name
    let port = web_server().await;
    let mut setup = Setup::new(Fake::ok());
    setup.fetcher = local_fetcher(port);
    let r = run_node("http.request", json!({ "url": format!("http://local.test:{port}/go") }), json!({}), setup, Mode::Production).await;
    assert_eq!(r.code(), "http_blocked", "{:?}", r.outcome);
    assert!(r.error()["message"].as_str().unwrap().contains("evil.test"), "{}", r.error());
    // with redirects off, the redirect is the answer and nothing is followed
    let mut setup = Setup::new(Fake::ok());
    setup.fetcher = local_fetcher(port);
    let r = run_node("http.request", json!({ "url": format!("http://local.test:{port}/go"), "follow_redirects": false, "fail_on_status": false }), json!({}), setup, Mode::Production).await;
    assert_eq!(r.items("n")[0]["status"], 307);
    // `allow_private_network` lets a local service through (the real block rule, a loopback address)
    let mut setup = Setup::new(Fake::ok());
    setup.settings.allow_private_network = true;
    let r = run_node("http.request", json!({ "url": format!("http://127.0.0.1:{port}/echo") }), json!({}), setup, Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    assert_eq!(r.items("n")[0]["body"]["method"], "GET");
    // and without it the same address is refused
    let r = run_node("http.request", json!({ "url": format!("http://127.0.0.1:{port}/echo") }), json!({}), Setup::new(Fake::ok()), Mode::Production).await;
    assert_eq!(r.code(), "http_blocked");
}

#[tokio::test]
async fn a_non_get_request_in_a_test_run_leaves_a_warning_in_the_log() {
    let port = web_server().await;
    let r = run_node("http.request", json!({ "method": "POST", "url": format!("http://local.test:{port}/echo"), "body": { "a": 1 } }), json!({}), private_setup(port), Mode::TestDry).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    assert_eq!(r.store.records_of("n")[0].status, NodeStatus::Success, "HTTP Request always runs for real");
    assert!(r.store.events.lock().unwrap().iter().any(|e| e.0 == "log" && e.2["message"].as_str().is_some_and(|m| m.contains("was really sent"))));
    // a GET leaves no warning
    let r = run_node("http.request", json!({ "url": format!("http://local.test:{port}/echo") }), json!({}), private_setup(port), Mode::TestDry).await;
    assert!(!r.store.events.lock().unwrap().iter().any(|e| e.0 == "log"));
}

// ---------- Fetch web page ----------

fn page_setup(port: u16) -> Setup {
    let mut s = Setup::new(Fake::ok());
    s.fetcher = local_fetcher(port);
    s
}

#[tokio::test]
async fn fetch_page_reads_title_text_links_and_sends_a_clear_user_agent() {
    let port = web_server().await;
    let r = run_node("web.fetch_page", json!({ "url": format!("http://local.test:{port}/page"), "include_links": true, "max_chars": 600 }), json!({}), page_setup(port), Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    let out = &r.items("n")[0];
    assert_eq!(out["title"], "Acme Rockets");
    assert_eq!(out["description"], "We build rockets");
    assert_eq!(out["status"], 200);
    assert_eq!(out["truncated"], true, "the page is longer than 600 characters");
    assert!(out["text"].as_str().unwrap().chars().count() <= 600);
    assert!(out["text"].as_str().unwrap().contains("Hello"));
    assert!(!out["text"].as_str().unwrap().contains("menu"), "the nav part is dropped");
    assert_eq!(out["links"], json!([{ "text": "About", "href": format!("http://local.test:{port}/about") }]));
    // the user agent
    let r = run_node("web.fetch_page", json!({ "url": format!("http://local.test:{port}/page"), "max_chars": 50000 }), json!({}), page_setup(port), Mode::Production).await;
    let text = r.items("n")[0]["text"].as_str().unwrap().to_string();
    assert!(text.contains("UA:AikidoEmail-Automation/") && text.contains("https://app.example"), "{text}");
    assert_eq!(r.items("n")[0]["truncated"], false);
    assert!(r.items("n")[0].get("links").is_none());
}

#[tokio::test]
async fn fetch_page_obeys_robots_txt() {
    let port = web_server().await;
    let base = format!("http://local.test:{port}");
    let r = run_node("web.fetch_page", json!({ "url": format!("{base}/private/secret") }), json!({}), page_setup(port), Mode::Production).await;
    assert_eq!(r.code(), "web_robots_disallowed");
    // the longer Allow rule wins
    let r = run_node("web.fetch_page", json!({ "url": format!("{base}/private/open") }), json!({}), page_setup(port), Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    // the node param can not turn it off while the setting forces it on
    let r = run_node("web.fetch_page", json!({ "url": format!("{base}/private/secret"), "respect_robots": false }), json!({}), page_setup(port), Mode::Production).await;
    assert_eq!(r.code(), "web_robots_disallowed");
    // with the setting off, the param decides
    let mut s = page_setup(port);
    s.settings.fetch_respect_robots = false;
    let r = run_node("web.fetch_page", json!({ "url": format!("{base}/private/secret"), "respect_robots": false }), json!({}), s, Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    // robots.txt is read once per run, not once per item
    let d = json!({
        "format": "aikido-workflow", "version": 1, "name": "T",
        "nodes": [
            { "id": "start", "type": "trigger.manual" },
            { "id": "split", "type": "data.split_out", "params": { "path": "urls" } },
            { "id": "n", "type": "web.fetch_page", "params": { "url": "${{ json.value }}" } }
        ],
        "edges": [{ "from": "start", "to": "split" }, { "from": "split", "to": "n" }]
    });
    let r = run_def(d, json!({ "urls": [format!("{base}/page"), format!("{base}/page"), format!("{base}/page")] }), page_setup(port), Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    assert_eq!(r.env.calls_made(), 4, "1 robots.txt + 3 pages");
}

#[tokio::test]
async fn fetch_page_refuses_blocked_domains_and_non_text() {
    let port = web_server().await;
    // linkedin is on the default list
    let r = run_node("web.fetch_page", json!({ "url": "https://www.linkedin.com/in/someone" }), json!({}), page_setup(port), Mode::Production).await;
    assert_eq!(r.code(), "web_domain_blocked");
    let r = run_node("web.fetch_page", json!({ "url": "de.linkedin.com/in/x" }), json!({}), page_setup(port), Mode::Production).await;
    assert_eq!(r.code(), "web_domain_blocked");
    // a host the owner added
    let mut s = page_setup(port);
    s.settings.fetch_blocked_domains.push("local.test".into());
    let r = run_node("web.fetch_page", json!({ "url": format!("http://local.test:{port}/page") }), json!({}), s, Mode::Production).await;
    assert_eq!(r.code(), "web_domain_blocked");
    // not a page
    let mut s = page_setup(port);
    s.settings.fetch_respect_robots = false;
    let r = run_node("web.fetch_page", json!({ "url": format!("http://local.test:{port}/image"), "respect_robots": false }), json!({}), s, Mode::Production).await;
    assert_eq!(r.code(), "web_not_text");
    // private addresses are refused, whatever the HTTP Request setting says
    let mut s = Setup::new(Fake::ok());
    s.settings.allow_private_network = true;
    s.settings.fetch_respect_robots = false;
    let r = run_node("web.fetch_page", json!({ "url": "http://10.0.0.1/", "respect_robots": false }), json!({}), s, Mode::Production).await;
    assert_eq!(r.code(), "http_blocked");
    // a page that answers 404
    let mut s = page_setup(port);
    s.settings.fetch_respect_robots = false;
    let r = run_node("web.fetch_page", json!({ "url": format!("http://local.test:{port}/nothing"), "respect_robots": false }), json!({}), s, Mode::Production).await;
    assert_eq!(r.code(), "http_status_404");
}

// ---------- the AI Agent node (phase 4) ----------

use crate::generator::llm::fake::{FakeLlm, response};
use crate::generator::llm::LlmError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const PROVIDER: &str = "0192f3a0-0000-7000-8000-0000000000aa";

/// A provider store for tests: every name works except `missing`; the budget is a switch.
struct FakeAi {
    llm: Arc<FakeLlm>,
    calls: Mutex<Vec<AiCall>>,
    asked: Mutex<Vec<String>>,
    auth_failed: Mutex<Vec<Uuid>>,
    budget_used_up: AtomicBool,
}

impl FakeAi {
    fn new() -> Arc<Self> {
        Arc::new(FakeAi { llm: FakeLlm::new(), calls: Mutex::new(vec![]), asked: Mutex::new(vec![]), auth_failed: Mutex::new(vec![]), budget_used_up: AtomicBool::new(false) })
    }
    fn setup(self: &Arc<Self>) -> Setup {
        let mut s = Setup::new(Fake::ok());
        s.ai = Some(self.clone());
        s
    }
    fn records(&self) -> Vec<AiCall> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl AiBackend for FakeAi {
    async fn provider(&self, which: &str) -> Result<AiProvider, NodeError> {
        self.asked.lock().unwrap().push(which.to_string());
        if which == "missing" {
            return Err(NodeError::new("ai_provider_not_found", "There is no AI provider named `missing`."));
        }
        Ok(AiProvider { id: PROVIDER.parse().unwrap(), model: "fake-model".into(), client: self.llm.clone() })
    }
    async fn budget_reached(&self) -> Result<bool, NodeError> {
        Ok(self.budget_used_up.load(Ordering::SeqCst))
    }
    async fn record(&self, call: AiCall) {
        self.calls.lock().unwrap().push(call);
    }
    async fn auth_failed(&self, provider_id: Uuid) {
        self.auth_failed.lock().unwrap().push(provider_id);
    }
}

fn agent(extra: Value) -> Value {
    let mut p = json!({ "prompt": "Write a hello to ${{ json.name }}." });
    p.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap_or_default());
    p
}

fn json_agent() -> Value {
    agent(json!({
        "system": "You write short emails.",
        "output": { "format": "json", "fields": [
            { "key": "subject", "type": "string", "description": "Under 8 words" },
            { "key": "body", "type": "string" },
            { "key": "score", "type": "number", "required": false }
        ] }
    }))
}

#[tokio::test]
async fn ai_agent_text_output_strips_dashes_and_writes_a_call_row() {
    let ai = FakeAi::new();
    ai.llm.push(Ok(response("Hello Ana \u{2014} nice to meet you.\n")));
    let r = run_node("ai.agent", agent(json!({ "provider": "default_slot", "max_tokens": 300, "temperature": 0.2 })), json!({ "name": "Ana" }), ai.setup(), Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    assert_eq!(r.items("n"), vec![json!({ "text": "Hello Ana, nice to meet you." })]);

    let sent = ai.llm.calls();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].user, "Write a hello to Ana.", "the expression is read first");
    assert_eq!(sent[0].max_output_tokens, 300);
    assert_eq!(sent[0].temperature, 0.2);
    assert!(!sent[0].json);
    assert_eq!(*ai.asked.lock().unwrap(), vec!["default_slot".to_string()]);

    // one llm_calls row with the ids of the run and the node, and no text
    let rows = ai.records();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].ok);
    assert_eq!((rows[0].input_tokens, rows[0].output_tokens), (100, 20));
    assert_eq!(rows[0].node_id, "n");
    assert_eq!(rows[0].run_id, r.env.who.run_id);
    assert_eq!(rows[0].workflow_id, r.env.who.workflow_id);
    assert_eq!(rows[0].provider_id, Some(PROVIDER.parse().unwrap()));
    assert!(!format!("{rows:?}").contains("Hello Ana"));
}

#[tokio::test]
async fn ai_agent_default_provider_is_the_draft_provider_and_keeps_dashes_when_asked() {
    let ai = FakeAi::new();
    ai.llm.push(Ok(response("a \u{2014} b")));
    let r = run_node("ai.agent", agent(json!({ "strip_dashes": false })), json!({ "name": "Ana" }), ai.setup(), Mode::Production).await;
    assert_eq!(r.items("n"), vec![json!({ "text": "a \u{2014} b" })]);
    assert_eq!(*ai.asked.lock().unwrap(), vec!["default_draft".to_string()]);
    // a provider that does not exist
    let r = run_node("ai.agent", agent(json!({ "provider": "missing" })), json!({ "name": "Ana" }), ai.setup(), Mode::Production).await;
    assert_eq!(r.code(), "ai_provider_not_found");
}

#[tokio::test]
async fn ai_agent_json_output_has_only_the_declared_keys_and_the_schema_is_in_the_system_text() {
    let ai = FakeAi::new();
    ai.llm.push(Ok(response("```json\n{\"subject\": \"Hi \u{2013} there\", \"body\": \"Text\", \"extra\": 1}\n```")));
    let r = run_node("ai.agent", json_agent(), json!({ "name": "Ana" }), ai.setup(), Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    // dashes are gone, the extra key is dropped, the optional key is null
    assert_eq!(r.items("n"), vec![json!({ "subject": "Hi, there", "body": "Text", "score": null })]);
    let sent = &ai.llm.calls()[0];
    assert!(sent.json);
    assert!(sent.system.starts_with("You write short emails."));
    assert!(sent.system.contains("\"subject\" (string, required): Under 8 words"));
    assert!(sent.system.contains("\"additionalProperties\":false"));
    // the next node reads the keys
    let d = json!({
        "format": "aikido-workflow", "version": 1, "name": "T",
        "nodes": [{ "id": "start", "type": "trigger.manual" }, { "id": "n", "type": "ai.agent", "params": json_agent() },
                  { "id": "use", "type": "data.set", "params": { "fields": { "s": "${{ nodes.n.json.subject }}" } } }],
        "edges": [{ "from": "start", "to": "n" }, { "from": "n", "to": "use" }]
    });
    ai.llm.push(Ok(response("{\"subject\": \"Hello\", \"body\": \"B\"}")));
    let r = run_def(d, json!({ "name": "Ana" }), ai.setup(), Mode::Production).await;
    assert_eq!(r.items("use")[0]["s"], "Hello");
}

#[tokio::test]
async fn ai_agent_bad_json_gets_one_repair_try() {
    let ai = FakeAi::new();
    ai.llm.push(Ok(response("I am sorry, here you go: no json at all")));
    ai.llm.push(Ok(response("{\"subject\": \"S\", \"body\": \"B\"}")));
    let r = run_node("ai.agent", json_agent(), json!({ "name": "Ana" }), ai.setup(), Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    assert_eq!(r.items("n")[0]["subject"], "S");
    let calls = ai.llm.calls();
    assert_eq!(calls.len(), 2);
    assert!(calls[1].user.starts_with("Write a hello to Ana."));
    assert!(calls[1].user.contains("Your last answer was not usable: The answer has no JSON object."));
    assert!(!calls[1].user.contains("I am sorry"), "the bad answer is not sent back");
    assert_eq!(ai.records().len(), 2, "both calls are in llm_calls");
}

#[tokio::test]
async fn ai_agent_bad_json_twice_fails_with_ai_bad_output() {
    let ai = FakeAi::new();
    ai.llm.push(Ok(response("{\"subject\": \"S\"}")));
    ai.llm.push(Ok(response("{\"subject\": 5, \"body\": \"B\"}")));
    let r = run_node("ai.agent", json_agent(), json!({ "name": "Ana" }), ai.setup(), Mode::Production).await;
    assert_eq!(r.code(), "ai_bad_output");
    assert!(r.error()["message"].as_str().unwrap().contains("\"subject\" must be a string"));
    assert_eq!(ai.llm.call_count(), 2, "one repair try, not more");
    assert_eq!(ai.records().len(), 2);
    // a text answer that is empty fails too, with no repair try
    let ai = FakeAi::new();
    ai.llm.push(Ok(response("   ")));
    let r = run_node("ai.agent", agent(json!({})), json!({ "name": "Ana" }), ai.setup(), Mode::Production).await;
    assert_eq!(r.code(), "ai_bad_output");
    assert_eq!(ai.llm.call_count(), 1);
}

#[tokio::test]
async fn ai_agent_provider_errors_get_their_own_codes() {
    for (err, code) in [
        (LlmError::Transient { code: "rate_limited", retry_after: None }, "ai_provider_unavailable"),
        (LlmError::BadRequest("too long".into()), "ai_bad_request"),
        (LlmError::Refused, "ai_refused"),
        (LlmError::BadResponse("odd".into()), "ai_bad_response"),
        (LlmError::Auth, "ai_auth_failed"),
    ] {
        let ai = FakeAi::new();
        ai.llm.push(Err(err.clone()));
        let r = run_node("ai.agent", agent(json!({})), json!({ "name": "Ana" }), ai.setup(), Mode::Production).await;
        assert_eq!(r.code(), code);
        let rows = ai.records();
        assert_eq!(rows.len(), 1, "a failed call is in llm_calls too");
        assert!(!rows[0].ok);
        assert_eq!(rows[0].error_code.as_deref(), Some(err.code().as_str()));
        // a refused key marks the provider
        assert_eq!(ai.auth_failed.lock().unwrap().len(), usize::from(err == LlmError::Auth));
    }
}

#[tokio::test(start_paused = true)]
async fn ai_agent_timeout_is_ai_timeout() {
    let ai = FakeAi::new();
    ai.llm.set_delay(Duration::from_secs(60));
    let r = run_node("ai.agent", agent(json!({ "timeout_secs": 5 })), json!({ "name": "Ana" }), ai.setup(), Mode::Production).await;
    assert_eq!(r.code(), "ai_timeout");
    assert_eq!(ai.records().len(), 1);
    assert_eq!(ai.records()[0].error_code.as_deref(), Some("timeout"));
}

#[tokio::test]
async fn ai_agent_input_is_checked() {
    let ai = FakeAi::new();
    let r = run_node("ai.agent", json!({ "prompt": "   " }), json!({}), ai.setup(), Mode::Production).await;
    assert_eq!(r.code(), "invalid_param_value");
    let r = run_node("ai.agent", agent(json!({ "prompt": "x".repeat(50_001) })), json!({}), ai.setup(), Mode::Production).await;
    assert_eq!(r.code(), "prompt_too_long");
    let r = run_node("ai.agent", agent(json!({ "output": { "format": "json", "fields": [{ "key": "a" }, { "key": "a" }] } })), json!({ "name": "A" }), ai.setup(), Mode::Production).await;
    assert_eq!(r.code(), "invalid_param_value");
    assert_eq!(ai.llm.call_count(), 0, "nothing reached the provider");
    // no AI backend at all
    let r = run_node("ai.agent", agent(json!({})), json!({ "name": "A" }), Setup::new(Fake::ok()), Mode::Production).await;
    assert_eq!(r.code(), "no_environment");
}

#[tokio::test]
async fn ai_agent_calls_for_real_in_a_dry_test_run_and_counts_against_the_call_limit() {
    // it costs tokens but changes no data, so it is not a side-effect node
    let ai = FakeAi::new();
    ai.llm.push(Ok(response("hi")));
    let r = run_node("ai.agent", agent(json!({})), json!({ "name": "A" }), ai.setup(), Mode::TestDry).await;
    assert_eq!(r.outcome, Outcome::Success);
    assert_eq!(r.items("n"), vec![json!({ "text": "hi" })]);
    assert_eq!(r.env.calls_made(), 1);
}

#[tokio::test]
async fn ai_agent_runs_once_per_item() {
    let ai = FakeAi::new();
    ai.llm.set_responder(|req| Ok(response(format!("echo: {}", req.user))));
    let d = json!({
        "format": "aikido-workflow", "version": 1, "name": "T",
        "nodes": [{ "id": "start", "type": "trigger.manual" }, { "id": "split", "type": "data.split_out", "params": { "path": "names" } },
                  { "id": "n", "type": "ai.agent", "params": { "prompt": "Hi ${{ json.value }}" } }],
        "edges": [{ "from": "start", "to": "split" }, { "from": "split", "to": "n" }]
    });
    let r = run_def(d, json!({ "names": ["Ana", "Bo", "Cy"] }), ai.setup(), Mode::Production).await;
    assert_eq!(r.items("n").iter().map(|i| i["text"].as_str().unwrap().to_string()).collect::<Vec<_>>(), vec!["echo: Hi Ana", "echo: Hi Bo", "echo: Hi Cy"]);
    assert_eq!(ai.records().len(), 3);
}

// ---------- the AI token budget: the run waits (build map 7.3) ----------

fn budget_def() -> Value {
    json!({
        "format": "aikido-workflow", "version": 1, "name": "T",
        "nodes": [{ "id": "start", "type": "trigger.manual" }, { "id": "n", "type": "ai.agent", "on_error": "continue", "retry": { "max": 3, "delay_ms": 100 }, "params": { "prompt": "Hi" } },
                  { "id": "after", "type": "flow.noop" }],
        "edges": [{ "from": "start", "to": "n" }, { "from": "n", "to": "after" }]
    })
}

#[tokio::test]
async fn a_used_up_budget_makes_the_run_wait_and_the_node_runs_again_later() {
    let ai = FakeAi::new();
    ai.budget_used_up.store(true, Ordering::SeqCst);
    ai.llm.push(Ok(response("done")));
    let (spec, _env) = build(budget_def(), json!({}), ai.setup(), Mode::Production);
    let (store, mut state) = (MemStore::new(), RunState::default());

    // 1. the budget is used up: no call, the run waits 10 minutes (on_error and retry do not apply)
    let out = execute(&spec, &mut state, &store, &Control::new()).await.unwrap();
    let Outcome::Waiting(until) = out else { panic!("expected waiting, got {out:?}") };
    assert!(until > spec.facts.now() + chrono::Duration::minutes(9) && until <= spec.facts.now() + chrono::Duration::minutes(11));
    assert_eq!(ai.llm.call_count(), 0);
    assert_eq!(store.records_of("n").iter().map(|r| r.status).collect::<Vec<_>>(), vec![NodeStatus::Waiting]);
    assert_eq!(store.records_of("n")[0].error.as_ref().unwrap()["code"], "ai_budget_reached");
    assert!(store.event_kinds().contains(&"waiting".to_string()));
    assert!(!state.finished.contains("n"));
    assert!(state.budget_wait_since.is_some());
    assert!(state.outputs.get("n").is_none());

    // 2. still used up: it waits again, the wait start does not move
    let since = state.budget_wait_since;
    assert!(matches!(execute(&spec, &mut state, &store, &Control::new()).await.unwrap(), Outcome::Waiting(_)));
    assert_eq!(state.budget_wait_since, since);

    // 3. the budget is back (a new day): the node runs, nothing is doubled, the wait mark is cleared
    ai.budget_used_up.store(false, Ordering::SeqCst);
    assert_eq!(execute(&spec, &mut state, &store, &Control::new()).await.unwrap(), Outcome::Success);
    assert_eq!(state.port_items("n", "main").len(), 1);
    assert_eq!(state.port_items("after", "main").len(), 1);
    assert_eq!(state.budget_wait_since, None);
    assert_eq!(ai.llm.call_count(), 1);
}

#[tokio::test]
async fn the_budget_wait_ends_with_an_error_after_ai_budget_wait_hours() {
    for hours in [0i64, 2] {
        let ai = FakeAi::new();
        ai.budget_used_up.store(true, Ordering::SeqCst);
        let offset = Arc::new(std::sync::atomic::AtomicI64::new(0));
        let mut setup = ai.setup();
        setup.settings.ai_budget_wait_hours = hours;
        let (mut spec, _env) = build(budget_def(), json!({}), setup, Mode::Production);
        let start = chrono::Utc::now();
        let o = offset.clone();
        spec.facts.clock = Arc::new(move || start + chrono::Duration::minutes(o.load(Ordering::SeqCst)));
        let (store, mut state) = (MemStore::new(), RunState::default());

        let first = execute(&spec, &mut state, &store, &Control::new()).await.unwrap();
        if hours == 0 {
            // the setting says: do not wait
            let Outcome::Error(e) = first else { panic!("expected an error, got {first:?}") };
            assert_eq!(e["code"], "ai_budget_reached");
            assert!(e["message"].as_str().unwrap().contains("ai_budget_wait_hours is 0"));
            continue;
        }
        assert!(matches!(first, Outcome::Waiting(_)));
        // one hour later: still waiting. Three hours later: the run fails.
        offset.store(60, Ordering::SeqCst);
        assert!(matches!(execute(&spec, &mut state, &store, &Control::new()).await.unwrap(), Outcome::Waiting(_)));
        offset.store(180, Ordering::SeqCst);
        let Outcome::Error(e) = execute(&spec, &mut state, &store, &Control::new()).await.unwrap() else { panic!("expected an error") };
        assert_eq!(e["code"], "ai_budget_reached");
        assert_eq!(e["node_id"], "n");
        assert!(e["message"].as_str().unwrap().contains("stayed used up for 2 hours"));
        assert_eq!(ai.llm.call_count(), 0);
    }
}

#[path = "phase9_tests.rs"]
mod phase9_tests;
