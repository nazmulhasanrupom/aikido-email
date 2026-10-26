//! Tests of the phase 9 features: the code node, `flow.execute_workflow`, AI agent tools, pagination of HTTP Request,
//! and what the validator says about them. No database: a fake internal caller, a fake AI, a local web server.
//! (The error workflow is in `error_workflow.rs`; the SQL part is in `tests/automations_phase9.rs`.)
use super::*;
use crate::automations::engine::sub_workflow::{SubWorkflow, WorkflowLoader};
use crate::automations::validate::{Options, validate_value};
use crate::generator::llm::fake::{final_response, tool_call_response};
use std::sync::atomic::Ordering as AtomicOrdering;

fn logs(r: &Res) -> Vec<String> {
    r.store.events.lock().unwrap().iter().filter(|e| e.0 == "log").map(|e| e.2["message"].as_str().unwrap_or("").to_string()).collect()
}

fn def_of(nodes: Value, edges: Value) -> Value {
    json!({ "format": "aikido-workflow", "version": 1, "name": "T", "nodes": nodes, "edges": edges })
}

// ---------- the code node ----------

#[tokio::test]
async fn code_node_runs_in_a_workflow_and_keeps_the_link_when_the_count_is_the_same() {
    let d = def_of(
        json!([
            { "id": "start", "type": "trigger.manual" },
            { "id": "split", "type": "data.split_out", "params": { "path": "names" } },
            { "id": "code", "type": "code.js", "params": { "code": "return items.map(i => ({ ...i, n: items.length }));" } },
            { "id": "after", "type": "data.set", "params": { "fields": { "who": "${{ nodes.split.json.value }}" } } }
        ]),
        json!([{ "from": "start", "to": "split" }, { "from": "split", "to": "code" }, { "from": "code", "to": "after" }]),
    );
    let r = run_def(d, json!({ "names": ["ana", "bo"] }), Setup::new(Fake::ok()), Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    assert_eq!(r.items("code"), vec![json!({ "value": "ana", "n": 2 }), json!({ "value": "bo", "n": 2 })]);
    assert_eq!(r.items("after")[1]["who"], "bo", "item 1 stays item 1");

    // another count: the link is cut and a read of an earlier node says so
    let d = def_of(
        json!([
            { "id": "start", "type": "trigger.manual" },
            { "id": "split", "type": "data.split_out", "params": { "path": "names" } },
            { "id": "code", "type": "code.js", "params": { "code": "return [{ total: items.length }];" } },
            { "id": "after", "type": "data.set", "params": { "fields": { "who": "${{ nodes.split.json.value }}" } } }
        ]),
        json!([{ "from": "start", "to": "split" }, { "from": "split", "to": "code" }, { "from": "code", "to": "after" }]),
    );
    let r = run_def(d, json!({ "names": ["ana", "bo"] }), Setup::new(Fake::ok()), Mode::Production).await;
    assert_eq!(r.items("code"), vec![json!({ "total": 2 })]);
    assert_eq!(r.code(), "ambiguous_reference");
}

#[tokio::test]
async fn code_node_errors_name_the_node_and_the_line() {
    let r = run_node("code.js", json!({ "code": "const a = 1;\nreturn [{ a }" }), json!({}), Setup::new(Fake::ok()), Mode::Production).await;
    assert_eq!(r.code(), "code_syntax");
    assert_eq!(r.error()["node_id"], "n");
    let r = run_node("code.js", json!({ "code": "\n\nthrow new Error('no good');" }), json!({}), Setup::new(Fake::ok()), Mode::Production).await;
    assert_eq!(r.code(), "code_error");
    assert!(r.error()["message"].as_str().unwrap().contains("(line 3)"), "{}", r.error());
    let r = run_node("code.js", json!({ "code": "return 5;" }), json!({}), Setup::new(Fake::ok()), Mode::Production).await;
    assert_eq!(r.code(), "code_bad_return");
    let r = run_node("code.js", json!({ "code": "  " }), json!({}), Setup::new(Fake::ok()), Mode::Production).await;
    assert_eq!(r.code(), "invalid_param_value");
}

#[tokio::test]
async fn a_dollar_brace_in_the_code_is_code_not_an_expression() {
    let code = "const x = 2; return [{ text: `sum ${x + 1}`, literal: '${{ 1 + 1 }}', vars_ok: typeof vars }];";
    let d = one_node("code.js", json!({ "code": code }));
    let v = validate_value(&d, &Options::default());
    assert!(v.errors.is_empty(), "{:?}", v.errors);
    let r = run_def(d, json!({}), Setup::new(Fake::ok()), Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    assert_eq!(r.items("n"), vec![json!({ "text": "sum 3", "literal": "${{ 1 + 1 }}", "vars_ok": "object" })]);
}

#[tokio::test]
async fn every_kind_of_trigger_passes_its_item_on() {
    // a webhook trigger was a stub that answered `not_implemented`
    for t in ["trigger.webhook", "trigger.schedule", "trigger.reply_received", "trigger.lead_created"] {
        let d = def_of(json!([{ "id": "start", "type": t }, { "id": "n", "type": "flow.noop" }]), json!([{ "from": "start", "to": "n" }]));
        let r = run_def(d, json!({ "a": 1 }), Setup::new(Fake::ok()), Mode::Production).await;
        assert_eq!(r.outcome, Outcome::Success, "{t}: {:?}", r.outcome);
        assert_eq!(r.items("n"), vec![json!({ "a": 1 })], "{t}");
    }
}

// ---------- sub-workflows ----------

struct Loader {
    defs: Mutex<HashMap<String, Value>>,
    asked: Mutex<Vec<(String, RunMode)>>,
}

impl Loader {
    fn new(defs: Vec<(&str, Value)>) -> Arc<Self> {
        Arc::new(Loader { defs: Mutex::new(defs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()), asked: Mutex::new(vec![]) })
    }
}

#[async_trait]
impl WorkflowLoader for Loader {
    async fn load(&self, id: &str, mode: RunMode) -> Result<SubWorkflow, NodeError> {
        self.asked.lock().unwrap().push((id.to_string(), mode));
        let v = self.defs.lock().unwrap().get(id).cloned().ok_or_else(|| NodeError::new("workflow_not_found", "There is no workflow with this id."))?;
        let def: WorkflowDefinition = serde_json::from_value(v).expect("a definition");
        Ok(SubWorkflow { def, version: (mode == RunMode::Production).then_some(3), secrets: Arc::new(Secrets::default()) })
    }
}

/// trigger -> n: `flow.execute_workflow`.
fn caller(next: &str, extra: Value) -> Value {
    let mut p = json!({ "workflow_id": next });
    p.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap_or_default());
    one_node("flow.execute_workflow", p)
}

/// trigger -> up: Set fields `upper`.
fn echo() -> Value {
    def_of(
        json!([{ "id": "go", "type": "trigger.manual" }, { "id": "up", "type": "data.set", "params": { "fields": { "upper": "${{ json.name | upper }}" } } }]),
        json!([{ "from": "go", "to": "up" }]),
    )
}

async fn run_top(top: &str, def: Value, trigger: Value, loader: &Arc<Loader>, fake: Arc<Fake>, mode: Mode) -> Res {
    let mut setup = Setup::new(fake);
    setup.workflows = Some(loader.clone());
    let (mut spec, env) = build(def, trigger, setup, mode);
    spec.facts.workflow_id = top.to_string();
    let store = MemStore::new();
    let mut state = RunState::default();
    let outcome = execute(&spec, &mut state, &store, &Control::new()).await.expect("the store works");
    Res { outcome, state, store, env }
}

#[tokio::test]
async fn execute_workflow_runs_the_sub_workflow_for_each_item_and_gives_its_last_items() {
    let loader = Loader::new(vec![("echo", echo())]);
    let d = def_of(
        json!([
            { "id": "start", "type": "trigger.manual" },
            { "id": "split", "type": "data.split_out", "params": { "path": "people" } },
            { "id": "n", "type": "flow.execute_workflow", "params": { "workflow_id": "echo" } },
            { "id": "after", "type": "data.set", "params": { "fields": { "again": "${{ nodes.split.json.name }}" } } }
        ]),
        json!([{ "from": "start", "to": "split" }, { "from": "split", "to": "n" }, { "from": "n", "to": "after" }]),
    );
    let r = run_top("top", d, json!({ "people": [{ "name": "ana" }, { "name": "bo" }] }), &loader, Fake::ok(), Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    assert_eq!(r.items("n"), vec![json!({ "name": "ana", "upper": "ANA" }), json!({ "name": "bo", "upper": "BO" })]);
    // each result is linked to its input item
    assert_eq!(r.items("after")[1]["again"], "bo");
    // production runs ask for the published version
    assert!(loader.asked.lock().unwrap().iter().all(|(id, m)| id == "echo" && *m == RunMode::Production));
    // the log of the calling run has one line per sub-run, with the nodes
    let l = logs(&r);
    assert_eq!(l.iter().filter(|m| m.starts_with("Sub-workflow \"T\" v3")).count(), 2, "{l:?}");
    assert!(l[0].contains("up success"), "{l:?}");
}

#[tokio::test]
async fn execute_workflow_input_all_gives_the_sub_workflow_all_items_in_one_run() {
    let sub = def_of(
        json!([{ "id": "go", "type": "trigger.manual" }, { "id": "c", "type": "data.set", "params": { "fields": { "count": "${{ json['items'] | length }}" } } }]),
        json!([{ "from": "go", "to": "c" }]),
    );
    let loader = Loader::new(vec![("count", sub)]);
    let d = def_of(
        json!([
            { "id": "start", "type": "trigger.manual" },
            { "id": "split", "type": "data.split_out", "params": { "path": "people" } },
            { "id": "n", "type": "flow.execute_workflow", "params": { "workflow_id": "count", "input": "all" } }
        ]),
        json!([{ "from": "start", "to": "split" }, { "from": "split", "to": "n" }]),
    );
    let r = run_top("top", d, json!({ "people": [{ "name": "ana" }, { "name": "bo" }, { "name": "cy" }] }), &loader, Fake::ok(), Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    let out = r.items("n");
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["count"], 3);
    assert_eq!(out[0]["items"][2]["name"], "cy");
    assert_eq!(loader.asked.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn execute_workflow_return_node_and_trigger_can_be_chosen() {
    let sub = def_of(
        json!([
            { "id": "go", "type": "trigger.manual" },
            { "id": "hook", "type": "trigger.webhook" },
            { "id": "a", "type": "data.set", "params": { "fields": { "step": "a" } } },
            { "id": "b", "type": "data.set", "params": { "fields": { "step": "b" } } }
        ]),
        json!([{ "from": "go", "to": "a" }, { "from": "hook", "to": "a" }, { "from": "a", "to": "b" }]),
    );
    let loader = Loader::new(vec![("sub", sub)]);
    let r = run_top("top", caller("sub", json!({})), json!({}), &loader, Fake::ok(), Mode::Production).await;
    assert_eq!(r.items("n"), vec![json!({ "step": "b" })], "the last node with items");
    let r = run_top("top", caller("sub", json!({ "return_node": "a" })), json!({}), &loader, Fake::ok(), Mode::Production).await;
    assert_eq!(r.items("n"), vec![json!({ "step": "a" })]);
    let r = run_top("top", caller("sub", json!({ "trigger": "hook" })), json!({ "x": 1 }), &loader, Fake::ok(), Mode::Production).await;
    assert_eq!(r.items("n"), vec![json!({ "x": 1, "step": "b" })]);
    let r = run_top("top", caller("sub", json!({ "trigger": "a" })), json!({}), &loader, Fake::ok(), Mode::Production).await;
    assert_eq!(r.code(), "invalid_param_value", "a is not a trigger");
    let r = run_top("top", caller("sub", json!({ "return_node": "zzz" })), json!({}), &loader, Fake::ok(), Mode::Production).await;
    assert_eq!(r.code(), "invalid_param_value");
}

#[tokio::test]
async fn execute_workflow_depth_is_limited_to_five_levels() {
    // w0 -> w1 -> ... -> wN; the last one just echoes
    let chain = |n: usize| -> Arc<Loader> {
        let mut defs: Vec<(String, Value)> = (0..n).map(|i| (format!("w{i}"), caller(&format!("w{}", i + 1), json!({})))).collect();
        defs.push((format!("w{n}"), echo()));
        let l = Loader::new(vec![]);
        l.defs.lock().unwrap().extend(defs);
        l
    };
    // top -> w0 -> w1 -> w2 -> w3 -> w4 (levels 1 to 5): fine
    let loader = chain(4);
    let r = run_top("top", caller("w0", json!({})), json!({ "name": "ok" }), &loader, Fake::ok(), Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    assert_eq!(r.items("n"), vec![json!({ "name": "ok", "upper": "OK" })]);
    // one more level is too deep, and the code survives the levels above
    let loader = chain(5);
    let r = run_top("top", caller("w0", json!({})), json!({ "name": "ok" }), &loader, Fake::ok(), Mode::Production).await;
    assert_eq!(r.code(), "sub_workflow_depth", "{:?}", r.outcome);
    assert!(r.error()["message"].as_str().unwrap().contains("5 levels"), "{}", r.error());
}

#[tokio::test]
async fn execute_workflow_refuses_a_loop() {
    // a -> b -> a
    let loader = Loader::new(vec![("a", caller("b", json!({}))), ("b", caller("a", json!({})))]);
    let r = run_top("a", caller("b", json!({})), json!({}), &loader, Fake::ok(), Mode::Production).await;
    assert_eq!(r.code(), "sub_workflow_loop", "{:?}", r.outcome);
    // a workflow that calls itself
    let loader = Loader::new(vec![("a", caller("a", json!({})))]);
    let r = run_top("a", caller("a", json!({})), json!({}), &loader, Fake::ok(), Mode::Production).await;
    assert_eq!(r.code(), "sub_workflow_loop");
    assert!(loader.asked.lock().unwrap().is_empty(), "nothing was loaded");
    // the same workflow twice in a row (not nested) is fine
    let loader = Loader::new(vec![("echo", echo())]);
    let d = def_of(
        json!([{ "id": "start", "type": "trigger.manual" }, { "id": "one", "type": "flow.execute_workflow", "params": { "workflow_id": "echo" } }, { "id": "two", "type": "flow.execute_workflow", "params": { "workflow_id": "echo" } }]),
        json!([{ "from": "start", "to": "one" }, { "from": "one", "to": "two" }]),
    );
    let r = run_top("top", d, json!({ "name": "x" }), &loader, Fake::ok(), Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
}

#[tokio::test]
async fn a_failing_sub_workflow_fails_the_node_and_names_the_cause() {
    let sub = def_of(
        json!([{ "id": "go", "type": "trigger.manual" }, { "id": "bad", "type": "flow.stop", "params": { "message": "boom", "error": true } }]),
        json!([{ "from": "go", "to": "bad" }]),
    );
    let loader = Loader::new(vec![("sub", sub)]);
    let r = run_top("top", caller("sub", json!({})), json!({}), &loader, Fake::ok(), Mode::Production).await;
    assert_eq!(r.code(), "sub_workflow_failed");
    let m = r.error()["message"].as_str().unwrap().to_string();
    assert!(m.contains("in node `bad`") && m.contains("boom") && m.contains("stopped_by_node"), "{m}");
    assert_eq!(r.error()["node_id"], "n");
    // with on_error continue the run goes on, the item carries the error
    let d = def_of(
        json!([{ "id": "start", "type": "trigger.manual" }, { "id": "n", "type": "flow.execute_workflow", "on_error": "continue", "params": { "workflow_id": "sub" } }, { "id": "after", "type": "flow.noop" }]),
        json!([{ "from": "start", "to": "n" }, { "from": "n", "to": "after" }]),
    );
    let r = run_top("top", d, json!({ "k": 1 }), &loader, Fake::ok(), Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success);
    assert_eq!(r.items("after")[0]["error"]["code"], "sub_workflow_failed");
    // a workflow that does not exist
    let r = run_top("top", caller("nope", json!({})), json!({}), &loader, Fake::ok(), Mode::Production).await;
    assert_eq!(r.code(), "workflow_not_found");
    // no loader (a test of pure nodes)
    let r = run_node("flow.execute_workflow", json!({ "workflow_id": "x" }), json!({}), Setup::new(Fake::ok()), Mode::Production).await;
    assert_eq!(r.code(), "no_environment");
}

#[tokio::test]
async fn a_sub_workflow_can_not_park_the_run() {
    let sub = def_of(
        json!([{ "id": "go", "type": "trigger.manual" }, { "id": "w", "type": "flow.wait", "params": { "mode": "duration", "amount": 10, "unit": "minutes" } }]),
        json!([{ "from": "go", "to": "w" }]),
    );
    let loader = Loader::new(vec![("sub", sub)]);
    let r = run_top("top", caller("sub", json!({})), json!({}), &loader, Fake::ok(), Mode::Production).await;
    assert_eq!(r.code(), "sub_workflow_waiting");
}

#[tokio::test]
async fn a_test_run_uses_the_draft_and_keeps_side_effects_dry_in_the_sub_workflow_too() {
    let sub = def_of(
        json!([
            { "id": "go", "type": "trigger.manual" },
            { "id": "write", "type": "aikido.lead.update", "params": { "lead_id": LEAD, "fields": { "first_name": "Z" } } }
        ]),
        json!([{ "from": "go", "to": "write" }]),
    );
    let loader = Loader::new(vec![("sub", sub)]);
    // dry test: the loader is asked for the draft, nothing is written
    let fake = Fake::ok();
    let r = run_top("top", caller("sub", json!({})), json!({}), &loader, fake.clone(), Mode::TestDry).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    assert_eq!(r.items("n")[0]["dry_run"], true);
    assert_eq!(loader.asked.lock().unwrap()[0].1, RunMode::Test);
    // real writes: it writes
    let fake = Fake::ok();
    let r = run_top("top", caller("sub", json!({})), json!({}), &loader, fake.clone(), Mode::TestReal).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    assert_eq!(fake.calls().len(), 1);
    // production: it writes, with the published version
    let fake = Fake::ok();
    let r = run_top("top", caller("sub", json!({})), json!({}), &loader, fake.clone(), Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success);
    assert_eq!(fake.calls().len(), 1);
    assert_eq!(loader.asked.lock().unwrap().last().unwrap().1, RunMode::Production);
    // the outside calls of the sub-workflow count in the calling run
    assert_eq!(r.env.calls_made(), 1, "the update of the sub-workflow counts in the run of the caller");
}

#[tokio::test]
async fn outside_calls_of_a_sub_workflow_count_in_the_whole_run() {
    let sub = one_node("aikido.lead.get", json!({ "lead_id": LEAD }));
    let loader = Loader::new(vec![("sub", sub)]);
    // the sub-workflow has "start" as its trigger, like one_node
    let r = run_top("top", caller("sub", json!({})), json!({}), &loader, Fake::ok(), Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    // the same counter: fill it up, the next sub-run fails with too_many_calls
    for _ in r.env.calls_made()..MAX_CALLS_PER_RUN {
        r.env.count_call().unwrap();
    }
    assert_eq!(r.env.count_call().unwrap_err().code, "too_many_calls");
}

// ---------- AI agent tools ----------

fn get_lead_tool() -> Value {
    json!({ "name": "get_lead", "description": "Read a lead by its id.", "node": "aikido.lead.get", "args": [{ "key": "lead_id", "type": "string", "description": "The lead id" }] })
}

fn tool_setup(ai: &Arc<FakeAi>, fake: &Arc<Fake>) -> Setup {
    let mut s = Setup::new(fake.clone());
    s.ai = Some(ai.clone());
    s
}

#[tokio::test]
async fn the_agent_calls_a_tool_then_answers() {
    let ai = FakeAi::new();
    let fake = Fake::new(|_| CallResponse { status: 200, body: json!({ "id": LEAD, "first_name": "Ana", "company_name": "Acme" }) });
    ai.llm.push(Ok(tool_call_response("get_lead", json!({ "lead_id": LEAD }))));
    ai.llm.push(Ok(final_response(json!("Hello Ana from Acme."))));
    let r = run_node("ai.agent", agent(json!({ "tools": [get_lead_tool()] })), json!({ "name": "Ana" }), tool_setup(&ai, &fake), Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    assert_eq!(r.items("n"), vec![json!({ "text": "Hello Ana from Acme." })]);
    // the tool node really ran, with the argument of the AI
    assert_eq!(fake.calls(), vec![call("GET", &format!("/v1/leads/{LEAD}"))]);
    // two turns; the first knows the tools, the second has the result
    let sent = ai.llm.calls();
    assert_eq!(sent.len(), 2);
    assert!(sent[0].json && sent[0].system.contains("- get_lead: Read a lead by its id.") && sent[0].system.contains("\"lead_id\" (string, required): The lead id"));
    assert_eq!(sent[0].user, "Write a hello to Ana.");
    assert!(sent[1].user.starts_with("Write a hello to Ana.") && sent[1].user.contains("[1] get_lead({\"lead_id\":\"0192f3a0-0000-7000-8000-000000000001\"}) gave: ") && sent[1].user.contains("\"company_name\":\"Acme\""), "{}", sent[1].user);
    // every turn is an llm_calls row
    assert_eq!(ai.records().len(), 2);
    let l = logs(&r);
    assert!(l.iter().any(|m| m == "Tool call 1: get_lead ok."), "{l:?}");
}

#[tokio::test]
async fn the_agent_gives_json_after_tools_and_the_dash_rule_holds() {
    let ai = FakeAi::new();
    let fake = Fake::ok();
    ai.llm.push(Ok(tool_call_response("get_lead", json!({ "lead_id": LEAD }))));
    ai.llm.push(Ok(final_response(json!({ "subject": "Hi \u{2014} there", "body": "Text" }))));
    let p = agent(json!({ "tools": [get_lead_tool()], "output": { "format": "json", "fields": [{ "key": "subject" }, { "key": "body" }] } }));
    let r = run_node("ai.agent", p, json!({ "name": "Ana" }), tool_setup(&ai, &fake), Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    assert_eq!(r.items("n"), vec![json!({ "subject": "Hi, there", "body": "Text" })]);
    assert!(ai.llm.calls()[0].system.contains("exactly these keys"));
}

#[tokio::test]
async fn the_ai_can_not_smuggle_an_expression_and_fixed_params_win() {
    // an expression in the arguments is refused, the node goes on, the AI is told
    let ai = FakeAi::new();
    let fake = Fake::ok();
    ai.llm.push(Ok(tool_call_response("get_lead", json!({ "lead_id": "${{ secret('KEY') }}" }))));
    ai.llm.push(Ok(final_response(json!("done"))));
    let mut setup = tool_setup(&ai, &fake);
    setup.secrets = BTreeMap::from([("KEY".to_string(), "sk-very-secret".to_string())]);
    let r = run_node("ai.agent", agent(json!({ "tools": [get_lead_tool()] })), json!({ "name": "Ana" }), setup, Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    assert!(fake.calls().is_empty(), "the tool did not run");
    assert!(ai.llm.calls()[1].user.contains("tool_bad_arguments"), "{}", ai.llm.calls()[1].user);
    assert!(!r.everything_stored().contains("sk-very-secret"));

    // lead data with `${{` in a fixed param is data: it is not read a second time
    let ai = FakeAi::new();
    let fake = Fake::ok();
    ai.llm.push(Ok(tool_call_response("find", json!({}))));
    ai.llm.push(Ok(final_response(json!("done"))));
    let tool = json!({ "name": "find", "description": "Find the lead.", "node": "aikido.lead.find", "params": { "filter": { "q": "${{ json.evil }}" } } });
    let r = run_node("ai.agent", agent(json!({ "tools": [tool] })), json!({ "name": "A", "evil": "${{ 7 * 7 }}" }), tool_setup(&ai, &fake), Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    let q = fake.calls()[0].query.clone();
    assert!(q.contains(&("q".to_string(), "${{ 7 * 7 }}".to_string())), "{q:?}");

    // a fixed param wins over the argument of the AI
    let ai = FakeAi::new();
    let fake = Fake::ok();
    ai.llm.push(Ok(tool_call_response("get_lead", json!({ "lead_id": "evil-id" }))));
    ai.llm.push(Ok(final_response(json!("done"))));
    let mut tool = get_lead_tool();
    tool["params"] = json!({ "lead_id": LEAD });
    let r = run_node("ai.agent", agent(json!({ "tools": [tool] })), json!({ "name": "A" }), tool_setup(&ai, &fake), Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success);
    assert_eq!(fake.calls(), vec![call("GET", &format!("/v1/leads/{LEAD}"))]);
}

#[tokio::test]
async fn a_tool_that_changes_data_is_dry_in_a_test_run() {
    let tool = json!({ "name": "rename", "description": "Rename the lead.", "node": "aikido.lead.update", "params": { "lead_id": LEAD }, "args": [{ "key": "fields", "type": "object" }] });
    for (mode, writes) in [(Mode::TestDry, 0), (Mode::TestReal, 1), (Mode::Production, 1)] {
        let ai = FakeAi::new();
        let fake = Fake::ok();
        ai.llm.push(Ok(tool_call_response("rename", json!({ "fields": { "first_name": "Z" } }))));
        ai.llm.push(Ok(final_response(json!("ok"))));
        let r = run_node("ai.agent", agent(json!({ "tools": [tool.clone()] })), json!({ "name": "A" }), tool_setup(&ai, &fake), mode).await;
        assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
        assert_eq!(fake.calls().len(), writes);
        let second = &ai.llm.calls()[1].user;
        assert_eq!(second.contains("\"dry_run\":true"), writes == 0, "{second}");
        if writes == 0 {
            assert!(second.contains("\"would_call\"") && second.contains("PATCH"), "{second}");
        }
    }
}

#[tokio::test]
async fn the_tool_limit_and_bad_tool_calls() {
    // max_tool_calls 1: the second call is not run; the AI is told to answer
    let ai = FakeAi::new();
    let fake = Fake::ok();
    ai.llm.push(Ok(tool_call_response("get_lead", json!({ "lead_id": LEAD }))));
    ai.llm.push(Ok(final_response(json!("answered"))));
    let r = run_node("ai.agent", agent(json!({ "tools": [get_lead_tool()], "max_tool_calls": 1 })), json!({ "name": "A" }), tool_setup(&ai, &fake), Mode::Production).await;
    assert_eq!(r.items("n"), vec![json!({ "text": "answered" })]);
    assert!(ai.llm.calls()[1].user.contains("You can not call more tools"), "{}", ai.llm.calls()[1].user);
    assert!(ai.llm.calls()[0].system.contains("at most 1 times"));

    // it calls again after the limit: the node fails, the tool ran once
    let ai = FakeAi::new();
    let fake = Fake::ok();
    for _ in 0..3 {
        ai.llm.push(Ok(tool_call_response("get_lead", json!({ "lead_id": LEAD }))));
    }
    let r = run_node("ai.agent", agent(json!({ "tools": [get_lead_tool()], "max_tool_calls": 1 })), json!({ "name": "A" }), tool_setup(&ai, &fake), Mode::Production).await;
    assert_eq!(r.code(), "ai_bad_output");
    assert_eq!(fake.calls().len(), 1);
    assert_eq!(ai.llm.call_count(), 2);

    // an unknown tool name and a failing tool are told to the AI, which can go on
    let ai = FakeAi::new();
    let fake = Fake::new(|_| CallResponse { status: 404, body: json!({ "error": { "code": "lead_not_found", "message": "No such lead." } }) });
    ai.llm.push(Ok(tool_call_response("nope", json!({}))));
    ai.llm.push(Ok(tool_call_response("get_lead", json!({ "lead_id": LEAD }))));
    ai.llm.push(Ok(final_response(json!("gave up"))));
    let r = run_node("ai.agent", agent(json!({ "tools": [get_lead_tool()] })), json!({ "name": "A" }), tool_setup(&ai, &fake), Mode::Production).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    let sent = ai.llm.calls();
    assert!(sent[1].user.contains("tool_unknown") && sent[1].user.contains("The tools are: get_lead"), "{}", sent[1].user);
    assert!(sent[2].user.contains("[2] get_lead") && sent[2].user.contains("error"), "{}", sent[2].user);
    assert_eq!(r.items("n"), vec![json!({ "text": "gave up" })]);

    // a reply that is not JSON gets one repair try
    let ai = FakeAi::new();
    ai.llm.push(Ok(response("Sure, I will help.")));
    ai.llm.push(Ok(final_response(json!("fine"))));
    let r = run_node("ai.agent", agent(json!({ "tools": [get_lead_tool()] })), json!({ "name": "A" }), tool_setup(&ai, &Fake::ok()), Mode::Production).await;
    assert_eq!(r.items("n"), vec![json!({ "text": "fine" })]);
    assert!(ai.llm.calls()[1].user.contains("was not usable"));
    let ai = FakeAi::new();
    ai.llm.push(Ok(response("nope")));
    ai.llm.push(Ok(response("still nope")));
    let r = run_node("ai.agent", agent(json!({ "tools": [get_lead_tool()] })), json!({ "name": "A" }), tool_setup(&ai, &Fake::ok()), Mode::Production).await;
    assert_eq!(r.code(), "ai_bad_output");

    // a bad tool list stops the node before any call
    let ai = FakeAi::new();
    let mut t = get_lead_tool();
    t["node"] = json!("aikido.api");
    let r = run_node("ai.agent", agent(json!({ "tools": [t] })), json!({ "name": "A" }), tool_setup(&ai, &Fake::ok()), Mode::Production).await;
    assert_eq!(r.code(), "invalid_param_value");
    assert_eq!(ai.llm.call_count(), 0);
}

#[tokio::test]
async fn the_budget_still_makes_the_run_wait_between_tool_calls() {
    let ai = FakeAi::new();
    ai.llm.push(Ok(tool_call_response("get_lead", json!({ "lead_id": LEAD }))));
    ai.budget_used_up.store(true, AtomicOrdering::SeqCst);
    let r = run_node("ai.agent", agent(json!({ "tools": [get_lead_tool()] })), json!({ "name": "A" }), tool_setup(&ai, &Fake::ok()), Mode::Production).await;
    // the budget is checked before the first call: the node reports it, the executor makes the run wait
    assert!(matches!(r.outcome, Outcome::Waiting(_)) || r.code() == "ai_budget_reached", "{:?}", r.outcome);
    assert_eq!(ai.llm.call_count(), 0);
}

#[tokio::test(start_paused = true)]
async fn the_node_timeout_has_room_for_the_tool_turns() {
    use crate::automations::registry::Registry;
    let node = Registry::global().get("ai.agent").unwrap();
    let t = |p: Value| node.timeout(p.as_object().unwrap()).unwrap().as_secs();
    assert_eq!(t(json!({ "timeout_secs": 10 })), 50);
    assert_eq!(t(json!({ "timeout_secs": 10, "tools": [{}] })), 10 * 7 + 30);
    assert_eq!(t(json!({ "timeout_secs": 10, "tools": [{}], "max_tool_calls": 20 })), 10 * 22 + 30);
}

// ---------- HTTP pagination ----------

async fn page_server() -> u16 {
    use axum::Json;
    use axum::extract::Query;
    use axum::routing::{get, post};
    type Q = Query<HashMap<String, String>>;
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let app = axum::Router::new()
        // next address in the answer (relative), 2 items per page, 3 pages
        .route("/next", get(move |q: Q| async move {
            let n: u64 = q.get("n").and_then(|v| v.parse().ok()).unwrap_or(1);
            let items: Vec<Value> = (0..2).map(|i| json!({ "id": n * 10 + i })).collect();
            Json(json!({ "data": items, "next": if n < 3 { Some(format!("/next?n={}", n + 1)) } else { None } }))
        }))
        // page numbers: 3 full pages, then empty
        .route("/page", get(move |q: Q| async move {
            let p: u64 = q.get("page").and_then(|v| v.parse().ok()).unwrap_or(1);
            let items: Vec<Value> = if p <= 3 { (0..2).map(|i| json!({ "id": p * 10 + i })).collect() } else { vec![] };
            Json(json!({ "results": items }))
        }))
        // offsets: 5 items in all
        .route("/offset", get(move |q: Q| async move {
            let o: u64 = q.get("offset").and_then(|v| v.parse().ok()).unwrap_or(0);
            Json(json!({ "results": (o..(o + 2).min(5)).map(|i| json!({ "id": i })).collect::<Vec<_>>() }))
        }))
        // cursors: none -> a -> b -> end
        .route("/cursor", get(move |q: Q| async move {
            let c = q.get("cursor").cloned();
            let next = match c.as_deref() {
                None => Some("a"),
                Some("a") => Some("b"),
                _ => None,
            };
            Json(json!({ "data": [{ "c": c.unwrap_or_else(|| "start".into()) }], "next_cursor": next }))
        }))
        .route("/body", post(move |Json(b): Json<Value>| async move {
            let p = b["page"].as_u64().unwrap_or(1);
            let items: Vec<Value> = if p <= 2 { vec![json!({ "id": p, "q": b["q"] })] } else { vec![] };
            Json(json!({ "results": items }))
        }))
        .route("/loop", get(|| async { Json(json!({ "data": [{ "x": 1 }], "next": "/loop" })) }))
        .route("/cross", get(move || async move { Json(json!({ "data": [], "next": format!("http://blocked.test:{port}/next") })) }));
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    port
}

async fn paged(port: u16, path: &str, extra: Value) -> Res {
    let mut p = json!({ "url": format!("http://local.test:{port}{path}") });
    p.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap_or_default());
    run_node("http.request", p, json!({}), private_setup(port), Mode::Production).await
}

fn ids(r: &Res) -> Vec<u64> {
    r.items("n").iter().filter_map(|i| i["id"].as_u64()).collect()
}

#[tokio::test]
async fn pagination_follows_the_next_address_and_stops_at_the_end() {
    let port = page_server().await;
    let r = paged(port, "/next", json!({ "split": "data", "pagination": { "mode": "next_url" } })).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    assert_eq!(ids(&r), vec![10, 11, 20, 21, 30, 31]);
    assert_eq!(r.env.calls_made(), 3, "one call per page");
    assert!(logs(&r).iter().any(|m| m == "Pagination: read 3 page(s), 6 item(s). Stopped because the answer has no next address."), "{:?}", logs(&r));
    // another path for the address
    let r = paged(port, "/next", json!({ "split": "data", "pagination": { "mode": "next_url", "next": "body.next" } })).await;
    assert_eq!(ids(&r).len(), 6);
    // no split: one item per page, with its number
    let r = paged(port, "/next", json!({ "pagination": { "mode": "next_url" } })).await;
    let pages = r.items("n");
    assert_eq!(pages.len(), 3);
    assert_eq!(pages.iter().map(|p| p["page"].as_u64().unwrap()).collect::<Vec<_>>(), vec![1, 2, 3]);
    assert_eq!(pages[2]["body"]["data"][0]["id"], 30);
    // no pagination: one request, no `page` field
    let r = paged(port, "/next", json!({})).await;
    assert_eq!(r.items("n").len(), 1);
    assert!(r.items("n")[0].get("page").is_none());
}

#[tokio::test]
async fn pagination_max_pages_is_a_limit_not_an_error() {
    let port = page_server().await;
    let r = paged(port, "/next", json!({ "split": "data", "pagination": { "mode": "next_url", "max_pages": 2 } })).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    assert_eq!(ids(&r), vec![10, 11, 20, 21]);
    assert_eq!(r.env.calls_made(), 2);
    assert!(logs(&r).iter().any(|m| m.contains("Stopped because max_pages was reached")), "{:?}", logs(&r));
    // the default is 10 pages: an endless API is cut
    let r = paged(port, "/page", json!({ "split": "results", "pagination": { "mode": "page", "param": "page", "max_pages": 100 }, "timeout_secs": 5 })).await;
    assert_eq!(ids(&r).len(), 6, "stops at the empty page, long before 100");
}

#[tokio::test]
async fn pagination_by_page_number_offset_and_cursor() {
    let port = page_server().await;
    // page numbers go up by one; an empty page ends it
    let r = paged(port, "/page", json!({ "split": "results", "pagination": { "mode": "page", "param": "page" } })).await;
    assert_eq!(ids(&r), vec![10, 11, 20, 21, 30, 31]);
    assert_eq!(r.env.calls_made(), 4);
    assert!(logs(&r).iter().any(|m| m.contains("the page was empty")));
    // another start
    let r = paged(port, "/page", json!({ "split": "results", "pagination": { "mode": "page", "param": "page", "start": 2 } })).await;
    assert_eq!(ids(&r), vec![20, 21, 30, 31]);
    // offset: the step is the size of the last page
    let r = paged(port, "/offset", json!({ "split": "results", "pagination": { "mode": "offset", "param": "offset" } })).await;
    assert_eq!(ids(&r), vec![0, 1, 2, 3, 4]);
    // offset with a step
    let r = paged(port, "/offset", json!({ "split": "results", "pagination": { "mode": "offset", "param": "offset", "step": 2 } })).await;
    assert_eq!(ids(&r), vec![0, 1, 2, 3, 4]);
    // cursor: none on the first request, then what the answer gives
    let r = paged(port, "/cursor", json!({ "split": "data", "pagination": { "mode": "cursor", "param": "cursor" } })).await;
    let cs: Vec<String> = r.items("n").iter().map(|i| i["c"].as_str().unwrap().to_string()).collect();
    assert_eq!(cs, vec!["start", "a", "b"]);
    assert!(logs(&r).iter().any(|m| m.contains("the answer has no next cursor")));
    // an existing query param is kept, the page param replaces its own old value
    let r = paged(port, "/page?other=1&page=9", json!({ "split": "results", "pagination": { "mode": "page", "param": "page" } })).await;
    assert_eq!(ids(&r), vec![10, 11, 20, 21, 30, 31]);
}

#[tokio::test]
async fn pagination_can_put_the_page_in_the_json_body_and_stop_by_expression() {
    let port = page_server().await;
    let r = paged(port, "/body", json!({ "method": "POST", "body": { "q": "x" }, "split": "results", "pagination": { "mode": "page", "param": "page", "location": "body" } })).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    assert_eq!(r.items("n").iter().map(|i| i["id"].as_u64().unwrap()).collect::<Vec<_>>(), vec![1, 2]);
    assert_eq!(r.items("n")[0]["q"], "x", "the rest of the body is sent on every page");
    // the stop condition reads the page
    let r = paged(port, "/page", json!({ "pagination": { "mode": "page", "param": "page", "stop_when": "${{ page.number >= 2 }}" } })).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    assert_eq!(r.items("n").len(), 2);
    assert!(logs(&r).iter().any(|m| m.contains("stop_when was true")));
    let r = paged(port, "/next", json!({ "split": "data", "pagination": { "mode": "next_url", "stop_when": "${{ page.items[0].id >= 20 }}" } })).await;
    assert_eq!(ids(&r), vec![10, 11, 20, 21], "the page that makes it true is kept");
    let r = paged(port, "/next", json!({ "split": "data", "pagination": { "mode": "next_url", "stop_when": "${{ page.count == 2 and page.status == 200 and page.body.next is none }}" } })).await;
    assert_eq!(ids(&r).len(), 6);
    // a stop condition that can not be read is an error that says so
    let r = paged(port, "/next", json!({ "split": "data", "pagination": { "mode": "next_url", "stop_when": "${{ page.nothing.deeper }}" } })).await;
    assert_eq!(r.code(), "pagination_stop_error");
}

#[tokio::test]
async fn pagination_refuses_loops_other_hosts_and_unclear_settings() {
    let port = page_server().await;
    // the next address is the page itself: one page, then stop
    let r = paged(port, "/loop", json!({ "split": "data", "pagination": { "mode": "next_url" } })).await;
    assert_eq!(r.outcome, Outcome::Success);
    assert_eq!(r.items("n").len(), 1);
    assert!(logs(&r).iter().any(|m| m.contains("the next address was seen before")));
    // another host: refused before any call (the auth header would go there)
    let r = paged(port, "/cross", json!({ "split": "data", "pagination": { "mode": "next_url" } })).await;
    assert_eq!(r.code(), "pagination_cross_origin");
    assert_eq!(r.env.calls_made(), 1);
    // settings
    for (pg, why) in [
        (json!({ "mode": "page", "param": "page" }), "needs `split`"),
        (json!({ "mode": "offset", "param": "o", "stop_when": "${{ true }}" }), "needs `step`"),
        (json!({ "mode": "page" }), "`pagination.param`"),
        (json!({ "mode": "weekly" }), "not next_url"),
        (json!({ "mode": "next_url", "max_pages": 0 }), "max_pages"),
        (json!({ "mode": "next_url", "max_pages": 500 }), "max_pages"),
        (json!({ "mode": "page", "param": "p", "stop_when": "${{ true }}", "location": "header" }), "location"),
        (json!({ "mode": "offset", "param": "o", "step": 0, "split": "results" }), "step"),
    ] {
        let r = paged(port, "/page", json!({ "pagination": pg })).await;
        assert_eq!(r.code(), "invalid_param_value", "{pg}");
        assert!(r.error()["message"].as_str().unwrap().contains(why), "{pg}: {}", r.error());
    }
    // a failing page fails the node (the first pages are not given)
    let r = paged(port, "/page", json!({ "split": "nothing_here", "pagination": { "mode": "page", "param": "page" } })).await;
    assert_eq!(r.code(), "split_not_a_list");
}

// ---------- the validator ----------

#[test]
fn the_validator_checks_the_new_params() {
    let v = |type_id: &str, params: Value| validate_value(&one_node(type_id, params), &Options::default());
    let has = |r: &crate::automations::validate::ValidationResult, code: &str, path: &str| r.errors.iter().any(|e| e.code == code && e.path.as_deref() == Some(path));

    // code.js: a syntax error is found while editing, with the line
    let r = v("code.js", json!({ "code": "const a = ;\nreturn items;" }));
    assert!(has(&r, "code_syntax", "params.code"), "{:?}", r.errors);
    assert!(r.errors.iter().any(|e| e.message.contains("(line 1)")), "{:?}", r.errors);
    assert!(v("code.js", json!({ "code": "return items;" })).ok);
    assert!(!v("code.js", json!({})).ok, "code is required");
    assert!(v("code.js", json!({ "code": "return [{ a: `${{ not: 1 }.not}` }];" })).errors.iter().all(|e| e.code != "expression_syntax"), "code is not an expression");

    // flow.execute_workflow
    let id = "0192f3a0-0000-7000-8000-0000000000bb";
    assert!(v("flow.execute_workflow", json!({ "workflow_id": id })).ok);
    assert!(v("flow.execute_workflow", json!({ "workflow_id": "${{ vars.wf }}" })).errors.iter().all(|e| e.code != "invalid_params"));
    assert!(has(&v("flow.execute_workflow", json!({ "workflow_id": "my-flow" })), "invalid_params", "params.workflow_id"));
    assert!(!v("flow.execute_workflow", json!({ "workflow_id": id, "input": "some" })).ok);

    // ai.agent tools
    let tool = json!({ "name": "get_lead", "description": "Read a lead.", "node": "aikido.lead.get" });
    assert!(v("ai.agent", json!({ "prompt": "x", "tools": [tool.clone()] })).ok);
    let mut bad = tool.clone();
    bad["node"] = json!("aikido.api");
    let r = v("ai.agent", json!({ "prompt": "x", "tools": [bad] }));
    assert!(has(&r, "invalid_params", "params.tools"), "{:?}", r.errors);
    let mut bad = tool.clone();
    bad["name"] = json!("Get Lead");
    assert!(has(&v("ai.agent", json!({ "prompt": "x", "tools": [bad] })), "invalid_params", "params.tools"));
    assert!(!v("ai.agent", json!({ "prompt": "x", "tools": [tool.clone()], "max_tool_calls": 99 })).ok);
    // expressions in a tool's params are checked like any other
    let mut with_expr = tool.clone();
    with_expr["params"] = json!({ "lead_id": "${{ nodes.nothing.json.id }}" });
    assert!(v("ai.agent", json!({ "prompt": "x", "tools": [with_expr] })).errors.iter().any(|e| e.code == "unknown_reference"));

    // http pagination
    let ok = v("http.request", json!({ "url": "https://x.test", "split": "data", "pagination": { "mode": "next_url", "stop_when": "${{ page.count == 0 }}" } }));
    assert!(ok.ok, "page.* is allowed in stop_when: {:?}", ok.errors);
    let r = v("http.request", json!({ "url": "https://x.test", "pagination": { "mode": "page", "param": "page" } }));
    assert!(has(&r, "invalid_params", "params.pagination"), "{:?}", r.errors);
    assert!(has(&v("http.request", json!({ "url": "https://x.test", "pagination": { "mode": "cursor" } })), "invalid_params", "params.pagination"));
    assert!(!v("http.request", json!({ "url": "https://x.test", "pagination": { "mode": "magic" } })).ok);
    // `page` is not a name anywhere else
    let r = v("http.request", json!({ "url": "https://x.test/${{ page.number }}" }));
    assert!(r.errors.iter().any(|e| e.code == "unknown_reference"), "{:?}", r.errors);
}

#[test]
fn error_workflow_id_must_be_a_workflow_id() {
    let mut d = one_node("flow.noop", json!({}));
    d["settings"] = json!({ "error_workflow_id": "0192f3a0-0000-7000-8000-0000000000bb" });
    assert!(validate_value(&d, &Options::default()).ok);
    d["settings"] = json!({ "error_workflow_id": "" });
    assert!(validate_value(&d, &Options::default()).ok);
    d["settings"] = json!({ "error_workflow_id": "my error flow" });
    let r = validate_value(&d, &Options::default());
    assert!(r.errors.iter().any(|e| e.path.as_deref() == Some("settings.error_workflow_id")), "{:?}", r.errors);
}
