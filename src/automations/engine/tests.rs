//! Tests of the executor and the nodes. They use `MemStore`, so no database is needed.
use super::eval::{RunFacts, RunMode};
use super::run::{Outcome, RunSpec, execute};
use super::state::RunState;
use super::store::{MemStore, NodeStatus, StoreError};
use super::Control;
use crate::automations::definition::WorkflowDefinition;
use crate::automations::registry::{Category, NodeCtx, NodeError, NodeInput, NodeOutput, NodeSpec, NodeType};
use async_trait::async_trait;
use serde_json::{Map, Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

// ---------- test nodes ----------

/// Fails the first `fail` calls, then passes the item on.
struct Flaky {
    spec: NodeSpec,
    fail: usize,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl NodeType for Flaky {
    fn spec(&self) -> &NodeSpec {
        &self.spec
    }
    async fn run(&self, _ctx: &NodeCtx<'_>, input: NodeInput) -> Result<NodeOutput, NodeError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n < self.fail { Err(NodeError::new("flaky", format!("call {n} failed"))) } else { Ok(NodeOutput::main(input.items)) }
    }
}

/// Never answers in time.
struct Hang(NodeSpec);

#[async_trait]
impl NodeType for Hang {
    fn spec(&self) -> &NodeSpec {
        &self.0
    }
    async fn run(&self, _ctx: &NodeCtx<'_>, input: NodeInput) -> Result<NodeOutput, NodeError> {
        tokio::time::sleep(Duration::from_secs(30)).await;
        Ok(NodeOutput::main(input.items))
    }
}

/// Presses Cancel while it works on the item with index `at`.
struct CancelAt {
    spec: NodeSpec,
    at: usize,
    seen: Arc<AtomicUsize>,
}

#[async_trait]
impl NodeType for CancelAt {
    fn spec(&self) -> &NodeSpec {
        &self.spec
    }
    async fn run(&self, ctx: &NodeCtx<'_>, input: NodeInput) -> Result<NodeOutput, NodeError> {
        self.seen.fetch_add(1, Ordering::SeqCst);
        if input.first_index == self.at {
            ctx.control.cancel();
        }
        Ok(NodeOutput::main(input.items))
    }
}

fn spec_of(id: &'static str) -> NodeSpec {
    NodeSpec::new(id, id, Category::Data, "a test node")
}

// ---------- helpers ----------

fn def(nodes: Value, edges: Value) -> Value {
    json!({ "format": "aikido-workflow", "version": 1, "name": "T", "nodes": nodes, "edges": edges })
}

fn edges(ids: &[&str]) -> Value {
    Value::Array(ids.windows(2).map(|w| json!({ "from": w[0], "to": w[1] })).collect())
}

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().cloned().expect("an object")
}

fn spec_for(def_json: Value, trigger: Value) -> RunSpec {
    let def: WorkflowDefinition = serde_json::from_value(def_json).expect("a valid definition");
    let facts = RunFacts::test("start", obj(trigger));
    RunSpec::new(def, facts)
}

struct Res {
    outcome: Outcome,
    state: RunState,
    store: MemStore,
}

impl Res {
    fn items(&self, node: &str, port: &str) -> Vec<Value> {
        self.state.port_items(node, port).into_iter().map(|i| Value::Object(i.json)).collect()
    }
    fn status(&self, node: &str) -> Vec<NodeStatus> {
        self.store.records_of(node).into_iter().map(|r| r.status).collect()
    }
    fn error_code(&self) -> String {
        match &self.outcome {
            Outcome::Error(e) => e["code"].as_str().unwrap_or_default().to_string(),
            other => panic!("expected an error, got {other:?}"),
        }
    }
}

async fn go(def_json: Value, trigger: Value) -> Res {
    go_with(spec_for(def_json, trigger)).await
}

async fn go_with(spec: RunSpec) -> Res {
    let store = MemStore::new();
    let mut state = RunState::default();
    let outcome = execute(&spec, &mut state, &store, &Control::new()).await.expect("the store works");
    Res { outcome, state, store }
}

fn start() -> Value {
    json!({ "id": "start", "type": "trigger.manual" })
}

// ---------- chains and expressions ----------

#[tokio::test]
async fn a_linear_chain_passes_data_and_reads_earlier_nodes() {
    let d = def(
        json!([
            start(),
            { "id": "greet", "type": "data.set", "params": { "fields": { "greeting": "Hi ${{ json.name }}", "n2": "${{ json.n * 2 }}" } } },
            { "id": "both", "type": "data.set", "params": { "fields": { "both": "${{ nodes.start.json.name }}/${{ nodes.greet.json.greeting }}/${{ trigger.n }}" } } }
        ]),
        edges(&["start", "greet", "both"]),
    );
    let r = go(d, json!({ "name": "Ana", "n": 4 })).await;
    assert_eq!(r.outcome, Outcome::Success);
    assert_eq!(r.items("greet", "main"), vec![json!({ "name": "Ana", "n": 4, "greeting": "Hi Ana", "n2": 8 })]);
    assert_eq!(r.items("both", "main")[0]["both"], "Ana/Hi Ana/4");
    // every node has one row, and the events tell the story
    assert_eq!(r.store.records.lock().unwrap().len(), 3);
    assert_eq!(r.store.event_kinds().iter().filter(|k| *k == "node_finished").count(), 3);
    // the checkpoint holds all three nodes
    assert_eq!(r.store.state().unwrap().finished.len(), 3);
}

#[tokio::test]
async fn set_makes_nested_fields_and_can_drop_the_rest() {
    let d = def(
        json!([start(), { "id": "s", "type": "data.set", "params": { "keep": "none", "fields": { "company.name": "${{ json.c }}", "company.size": 5, "tags": ["a", "${{ json.c }}"] } } }]),
        edges(&["start", "s"]),
    );
    let r = go(d, json!({ "c": "Acme", "other": 1 })).await;
    assert_eq!(r.items("s", "main"), vec![json!({ "company": { "name": "Acme", "size": 5 }, "tags": ["a", "Acme"] })]);
}

#[tokio::test]
async fn a_missing_value_is_an_error_with_the_node_and_the_path() {
    let d = def(json!([start(), { "id": "s", "type": "data.set", "params": { "fields": { "x": "${{ json.nope }}" } } }]), edges(&["start", "s"]));
    let r = go(d, json!({})).await;
    assert_eq!(r.error_code(), "expression_error");
    let Outcome::Error(e) = &r.outcome else { unreachable!() };
    assert_eq!(e["node_id"], "s");
    assert!(e["message"].as_str().unwrap().contains("json.nope"), "{e}");
    assert_eq!(r.status("s"), vec![NodeStatus::Error]);
}

// ---------- flow ----------

fn leads_trigger() -> Value {
    json!({ "company": "Acme", "leads": [{ "n": 1 }, { "n": 5 }, { "n": 9 }] })
}

fn split() -> Value {
    json!({ "id": "split", "type": "data.split_out", "params": { "path": "leads" } })
}

fn gt(n: i64) -> Value {
    json!([{ "left": "${{ json.n }}", "op": "gt", "right": n }])
}

#[tokio::test]
async fn if_sends_items_to_both_ports() {
    let d = def(
        json!([start(), split(), { "id": "check", "type": "flow.if", "params": { "conditions": gt(4) } },
               { "id": "yes", "type": "flow.noop" }, { "id": "no", "type": "flow.noop" }]),
        json!([{ "from": "start", "to": "split" }, { "from": "split", "to": "check" },
               { "from": "check", "port": "true", "to": "yes" }, { "from": "check", "port": "false", "to": "no" }]),
    );
    let r = go(d, leads_trigger()).await;
    assert_eq!(r.outcome, Outcome::Success);
    assert_eq!(r.items("yes", "main").len(), 2);
    assert_eq!(r.items("no", "main"), vec![json!({ "n": 1 })]);
    let rec = &r.store.records_of("check")[0];
    assert_eq!(rec.output_count.get("true"), Some(&2));
    assert_eq!(rec.output_count.get("false"), Some(&1));
}

#[tokio::test]
async fn if_with_an_expression_and_with_or() {
    let d = def(
        json!([start(), split(),
               { "id": "a", "type": "flow.filter", "params": { "expression": "${{ json.n >= 5 }}" } },
               { "id": "b", "type": "flow.filter", "params": { "combine": "or", "conditions": [
                   { "left": "${{ json.n }}", "op": "equals", "right": "1" }, { "left": "${{ json.n }}", "op": "gte", "right": 9 }] } }]),
        json!([{ "from": "start", "to": "split" }, { "from": "split", "to": "a" }, { "from": "split", "to": "b" }]),
    );
    let r = go(d, leads_trigger()).await;
    assert_eq!(r.items("a", "main").len(), 2);
    // 5 and "5" style equality, and `or`: n=1 and n=9
    let ns: Vec<Value> = r.items("b", "main").into_iter().map(|j| j["n"].clone()).collect();
    assert_eq!(ns, vec![json!(1), json!(9)]);
}

#[tokio::test]
async fn switch_routes_to_the_first_match_and_to_default() {
    let d = def(
        json!([start(), split(),
               { "id": "sw", "type": "flow.switch", "params": { "cases": [
                   { "conditions": [{ "left": "${{ json.n }}", "op": "lt", "right": 3 }] },
                   { "conditions": [{ "left": "${{ json.n }}", "op": "lt", "right": 6 }] }] } },
               { "id": "c1", "type": "flow.noop" }, { "id": "c2", "type": "flow.noop" }, { "id": "other", "type": "flow.noop" }]),
        json!([{ "from": "start", "to": "split" }, { "from": "split", "to": "sw" },
               { "from": "sw", "port": "case_1", "to": "c1" }, { "from": "sw", "port": "case_2", "to": "c2" }, { "from": "sw", "port": "default", "to": "other" }]),
    );
    let r = go(d, leads_trigger()).await;
    assert_eq!(r.items("c1", "main"), vec![json!({ "n": 1 })]);
    assert_eq!(r.items("c2", "main"), vec![json!({ "n": 5 })]);
    assert_eq!(r.items("other", "main"), vec![json!({ "n": 9 })]);
}

#[tokio::test]
async fn a_filter_that_drops_everything_skips_the_children() {
    let d = def(
        json!([start(), split(), { "id": "f", "type": "flow.filter", "params": { "conditions": gt(100) } }, { "id": "after", "type": "data.set", "params": { "fields": { "x": 1 } } },
               { "id": "last", "type": "flow.noop" }]),
        edges(&["start", "split", "f", "after", "last"]),
    );
    let r = go(d, leads_trigger()).await;
    assert_eq!(r.outcome, Outcome::Success);
    assert_eq!(r.status("f"), vec![NodeStatus::Success]);
    assert_eq!(r.status("after"), vec![NodeStatus::Skipped]);
    assert_eq!(r.status("last"), vec![NodeStatus::Skipped]);
    assert!(r.items("after", "main").is_empty());
}

#[tokio::test]
async fn split_out_runs_per_item_then_aggregate_joins() {
    let d = def(
        json!([start(), split(),
               { "id": "double", "type": "data.set", "params": { "fields": { "double": "${{ json.n * 2 }}", "company": "${{ nodes.start.json.company }}" } } },
               { "id": "agg", "type": "data.aggregate" }]),
        edges(&["start", "split", "double", "agg"]),
    );
    let r = go(d, leads_trigger()).await;
    assert_eq!(r.outcome, Outcome::Success);
    // paired reads: every item still sees the trigger
    let doubled = r.items("double", "main");
    assert_eq!(doubled.len(), 3);
    assert!(doubled.iter().all(|j| j["company"] == "Acme"));
    assert_eq!(doubled[2]["double"], 18);
    let agg = r.items("agg", "main");
    assert_eq!(agg.len(), 1);
    assert_eq!(agg[0]["items"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn aggregate_fields_collects_chosen_values() {
    let d = def(json!([start(), split(), { "id": "agg", "type": "data.aggregate", "params": { "mode": "fields", "fields": ["n", "missing"] } }]), edges(&["start", "split", "agg"]));
    let r = go(d, leads_trigger()).await;
    assert_eq!(r.items("agg", "main"), vec![json!({ "n": [1, 5, 9], "missing": [null, null, null] })]);
}

#[tokio::test]
async fn reading_one_item_of_a_node_before_aggregate_is_ambiguous() {
    let d = def(
        json!([start(), split(), { "id": "agg", "type": "data.aggregate" },
               { "id": "bad", "type": "data.set", "params": { "fields": { "x": "${{ nodes.split.json.n }}" } } }]),
        edges(&["start", "split", "agg", "bad"]),
    );
    let r = go(d, leads_trigger()).await;
    assert_eq!(r.error_code(), "ambiguous_reference");
    // `nodes.split.items` is the way out
    let d = def(
        json!([start(), split(), { "id": "agg", "type": "data.aggregate" },
               { "id": "ok", "type": "data.set", "params": { "fields": { "x": "${{ nodes.split.items | length }}" } } }]),
        edges(&["start", "split", "agg", "ok"]),
    );
    let r = go(d, leads_trigger()).await;
    assert_eq!(r.items("ok", "main")[0]["x"], 3);
}

#[tokio::test]
async fn a_node_on_another_branch_has_no_paired_item() {
    let d = def(
        json!([start(), split(),
               { "id": "check", "type": "flow.if", "params": { "conditions": gt(4) } },
               { "id": "big", "type": "flow.noop" },
               { "id": "small", "type": "data.set", "params": { "fields": { "x": "${{ nodes.big.json.n }}" } } }]),
        json!([{ "from": "start", "to": "split" }, { "from": "split", "to": "check" },
               { "from": "check", "port": "true", "to": "big" }, { "from": "check", "port": "false", "to": "small" }]),
    );
    let r = go(d, leads_trigger()).await;
    assert_eq!(r.error_code(), "no_paired_item");
}

fn merge_def(mode: &str) -> Value {
    def(
        json!([start(), split(), { "id": "check", "type": "flow.if", "params": { "conditions": gt(4) } },
               { "id": "a", "type": "data.set", "params": { "fields": { "side": "a" } } },
               { "id": "b", "type": "data.set", "params": { "fields": { "side": "b", "extra": 1 } } },
               { "id": "m", "type": "flow.merge", "params": { "mode": mode } }]),
        json!([{ "from": "start", "to": "split" }, { "from": "split", "to": "check" },
               { "from": "check", "port": "true", "to": "a" }, { "from": "check", "port": "false", "to": "b" },
               { "from": "a", "to": "m" }, { "from": "b", "to": "m" }]),
    )
}

#[tokio::test]
async fn merge_append_joins_all_inputs_in_edge_order() {
    let r = go(merge_def("append"), leads_trigger()).await;
    let sides: Vec<Value> = r.items("m", "main").into_iter().map(|j| j["side"].clone()).collect();
    assert_eq!(sides, vec![json!("a"), json!("a"), json!("b")]);
    assert_eq!(r.store.records_of("m").len(), 1, "Merge runs once");
}

#[tokio::test]
async fn merge_combine_by_position_merges_item_i_of_each_input() {
    let r = go(merge_def("combine_by_position"), leads_trigger()).await;
    let items = r.items("m", "main");
    assert_eq!(items.len(), 2);
    // item 0: input 1 {n:5,side:a} + input 2 {n:1,side:b,extra:1}, later inputs win
    assert_eq!(items[0], json!({ "n": 1, "side": "b", "extra": 1 }));
    // item 1 has no partner in input 2
    assert_eq!(items[1], json!({ "n": 9, "side": "a" }));
}

#[tokio::test]
async fn merge_wait_all_gives_the_items_of_input_1() {
    let r = go(merge_def("wait_all"), leads_trigger()).await;
    let sides: Vec<Value> = r.items("m", "main").into_iter().map(|j| j["side"].clone()).collect();
    assert_eq!(sides, vec![json!("a"), json!("a")]);
}

#[tokio::test]
async fn merge_runs_when_only_one_input_has_items() {
    let d = def(
        json!([start(), split(), { "id": "check", "type": "flow.if", "params": { "conditions": gt(100) } },
               { "id": "a", "type": "flow.noop" }, { "id": "b", "type": "flow.noop" }, { "id": "m", "type": "flow.merge" }]),
        json!([{ "from": "start", "to": "split" }, { "from": "split", "to": "check" },
               { "from": "check", "port": "true", "to": "a" }, { "from": "check", "port": "false", "to": "b" },
               { "from": "a", "to": "m" }, { "from": "b", "to": "m" }]),
    );
    let r = go(d, leads_trigger()).await;
    assert_eq!(r.items("m", "main").len(), 3);
    assert_eq!(r.status("a"), vec![NodeStatus::Skipped]);
}

#[tokio::test]
async fn a_node_with_two_inputs_that_is_not_merge_runs_once_per_input() {
    let d = def(
        json!([start(), split(), { "id": "check", "type": "flow.if", "params": { "conditions": gt(4) } },
               { "id": "join", "type": "data.set", "params": { "fields": { "seen": "${{ json.n }}" } } }]),
        json!([{ "from": "start", "to": "split" }, { "from": "split", "to": "check" },
               { "from": "check", "port": "true", "to": "join" }, { "from": "check", "port": "false", "to": "join" }]),
    );
    let r = go(d, leads_trigger()).await;
    assert_eq!(r.store.records_of("join").len(), 2);
    assert_eq!(r.items("join", "main").len(), 3);
}

#[tokio::test]
async fn stop_ends_the_run_with_an_error_or_a_success() {
    let d = def(
        json!([start(), { "id": "halt", "type": "flow.stop", "params": { "message": "no lead for ${{ json.name }}" } }, { "id": "never", "type": "flow.noop" }]),
        edges(&["start", "halt", "never"]),
    );
    let r = go(d, json!({ "name": "Ana" })).await;
    let Outcome::Error(e) = &r.outcome else { panic!("{:?}", r.outcome) };
    assert_eq!(e["code"], "stopped_by_node");
    assert_eq!(e["message"], "no lead for Ana");
    assert_eq!(e["node_id"], "halt");
    assert!(r.store.records_of("never").is_empty());

    let d = def(json!([start(), { "id": "halt", "type": "flow.stop", "params": { "error": false } }, { "id": "never", "type": "flow.noop" }]), edges(&["start", "halt", "never"]));
    let r = go(d, json!({})).await;
    assert_eq!(r.outcome, Outcome::Success);
    assert!(r.store.records_of("never").is_empty());
}

#[tokio::test]
async fn a_disabled_node_passes_its_items_through() {
    let d = def(
        json!([start(), { "id": "s", "type": "data.set", "disabled": true, "params": { "fields": { "x": 1 } } }, { "id": "t", "type": "flow.noop" }]),
        edges(&["start", "s", "t"]),
    );
    let r = go(d, json!({ "a": 1 })).await;
    assert_eq!(r.items("t", "main"), vec![json!({ "a": 1 })]);
}

// ---------- data nodes ----------

#[tokio::test]
async fn limit_dedupe_sort() {
    let trig = json!({ "rows": [{ "e": "B@x.com", "k": 2 }, { "e": "a@x.com", "k": 10 }, { "e": "b@x.com", "k": 7 }, { "e": "c@x.com", "k": 1 }, { "e": "d@x.com", "k": null }] });
    let rows = json!({ "id": "rows", "type": "data.split_out", "params": { "path": "rows" } });
    let d = def(
        json!([start(), rows, { "id": "uniq", "type": "data.dedupe", "params": { "key": "${{ json.e | lower }}" } },
               { "id": "sorted", "type": "data.sort", "params": { "by": "${{ json.k }}", "order": "desc" } },
               { "id": "last2", "type": "data.limit", "params": { "max": 2, "from": "end" } }]),
        edges(&["start", "rows", "uniq", "sorted", "last2"]),
    );
    let r = go(d, trig).await;
    assert_eq!(r.items("uniq", "main").len(), 4, "b@x.com and B@x.com are one; the first one stays");
    let ks: Vec<Value> = r.items("sorted", "main").into_iter().map(|j| j["k"].clone()).collect();
    assert_eq!(ks, vec![json!(10), json!(2), json!(1), json!(null)], "desc, null last");
    let last: Vec<Value> = r.items("last2", "main").into_iter().map(|j| j["k"].clone()).collect();
    assert_eq!(last, vec![json!(1), json!(null)]);
}

#[tokio::test]
async fn limit_zero_gives_nothing_and_skips_children() {
    let d = def(json!([start(), split(), { "id": "l", "type": "data.limit", "params": { "max": 0 } }, { "id": "x", "type": "flow.noop" }]), edges(&["start", "split", "l", "x"]));
    let r = go(d, leads_trigger()).await;
    assert_eq!(r.status("x"), vec![NodeStatus::Skipped]);
}

#[tokio::test]
async fn parse_json_and_date() {
    let d = def(
        json!([start(),
               { "id": "p", "type": "data.parse_json", "params": { "path": "raw", "into": "parsed" } },
               { "id": "when", "type": "data.date", "params": { "value": "2026-10-25T10:00:00Z", "action": "add", "amount": 2, "unit": "days", "into": "later" } },
               { "id": "fmt", "type": "data.date", "params": { "value": "${{ json.later }}", "action": "format", "format": "%d/%m/%Y", "into": "pretty" } },
               { "id": "diff", "type": "data.date", "params": { "value": "${{ json.later }}", "action": "diff", "other": "2026-10-25T10:00:00Z", "unit": "hours", "into": "hours" } }]),
        edges(&["start", "p", "when", "fmt", "diff"]),
    );
    let r = go(d, json!({ "raw": "{\"a\":[1,2]}" })).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    let last = &r.items("diff", "main")[0];
    assert_eq!(last["parsed"], json!({ "a": [1, 2] }));
    assert_eq!(last["later"], "2026-10-27T10:00:00Z");
    assert_eq!(last["pretty"], "27/10/2026");
    assert_eq!(last["hours"], 48);

    let d = def(json!([start(), { "id": "p", "type": "data.parse_json", "params": { "path": "raw" } }]), edges(&["start", "p"]));
    assert_eq!(go(d, json!({ "raw": "{oops" })).await.error_code(), "invalid_json");
}

#[tokio::test]
async fn split_out_needs_a_list() {
    let d = def(json!([start(), { "id": "s", "type": "data.split_out", "params": { "path": "company" } }]), edges(&["start", "s"]));
    assert_eq!(go(d, leads_trigger()).await.error_code(), "path_not_a_list");
    // keep_parent: the other fields stay on each element
    let d = def(json!([start(), { "id": "s", "type": "data.split_out", "params": { "path": "leads", "keep_parent": true } }]), edges(&["start", "s"]));
    let r = go(d, leads_trigger()).await;
    assert_eq!(r.items("s", "main")[1], json!({ "company": "Acme", "n": 5 }));
}

#[tokio::test]
async fn condition_operators() {
    use super::nodes;
    let _ = nodes::get_path; // the path helpers are public
    let cases = [
        (json!("abc"), "contains", json!("b"), true),
        (json!(["a", "b"]), "contains", json!("b"), true),
        (json!("abc"), "not_contains", json!("z"), true),
        (json!("abc"), "starts_with", json!("ab"), true),
        (json!("abc"), "ends_with", json!("bc"), true),
        (json!(5), "equals", json!("5"), true),
        (json!("007"), "equals", json!("7"), false),
        (json!("10"), "gt", json!("9"), true),
        (json!("b"), "gt", json!("a"), true),
        (json!(""), "is_empty", json!(null), true),
        (json!([1]), "is_not_empty", json!(null), true),
        (json!(true), "is_true", json!(null), true),
        (json!("false"), "is_false", json!(null), true),
        (json!("ana@acme.com"), "matches", json!("^[a-z]+@acme\\.com$"), true),
        (json!("x"), "matches", json!("^y"), false),
    ];
    for (left, op, right, want) in cases {
        let d = def(json!([start(), { "id": "f", "type": "flow.filter", "params": { "conditions": [{ "left": left, "op": op, "right": right }] } }]), edges(&["start", "f"]));
        let r = go(d, json!({})).await;
        assert_eq!(r.items("f", "main").len() == 1, want, "{left} {op} {right}");
    }
    // a pattern that does not compile is an error, not a silent false
    let d = def(json!([start(), { "id": "f", "type": "flow.filter", "params": { "conditions": [{ "left": "x", "op": "matches", "right": "(" }] } }]), edges(&["start", "f"]));
    assert_eq!(go(d, json!({})).await.error_code(), "invalid_param_value");
}

// ---------- errors, retry, timeout, cancel ----------

fn fetch(extra: Value) -> Value {
    // `web.fetch_page` needs the server services, so in these tests (no environment) it fails with no_environment
    let mut n = json!({ "id": "fetch", "type": "web.fetch_page", "params": { "url": "acme.com" } });
    n.as_object_mut().unwrap().extend(extra.as_object().cloned().unwrap_or_default());
    n
}

#[tokio::test]
async fn on_error_stop_ends_the_run_with_the_node_error() {
    let d = def(json!([start(), fetch(json!({})), { "id": "after", "type": "flow.noop" }]), edges(&["start", "fetch", "after"]));
    let r = go(d, json!({})).await;
    assert_eq!(r.error_code(), "no_environment");
    let Outcome::Error(e) = &r.outcome else { unreachable!() };
    assert_eq!(e["node_id"], "fetch");
    assert_eq!(e["item_index"], 0);
    assert_eq!(r.status("fetch"), vec![NodeStatus::Error]);
    assert!(r.store.records_of("after").is_empty());
    assert!(r.store.event_kinds().contains(&"node_error".to_string()));
}

#[tokio::test]
async fn on_error_continue_sends_the_item_on_with_an_error_field() {
    let d = def(
        json!([start(), fetch(json!({ "on_error": "continue" })), { "id": "after", "type": "data.set", "params": { "fields": { "code": "${{ json.error.code }}", "same": "${{ nodes.fetch.error.code }}" } } }]),
        edges(&["start", "fetch", "after"]),
    );
    let r = go(d, json!({ "name": "Ana" })).await;
    assert_eq!(r.outcome, Outcome::Success);
    let item = &r.items("after", "main")[0];
    assert_eq!(item["name"], "Ana");
    assert_eq!(item["code"], "no_environment");
    assert_eq!(item["same"], "no_environment");
    assert_eq!(item["error"]["node_id"], "fetch");
}

#[tokio::test]
async fn on_error_error_port_sends_failed_items_to_the_error_port() {
    let d = def(
        json!([start(), fetch(json!({ "on_error": "error_port" })), { "id": "ok", "type": "flow.noop" }, { "id": "failed", "type": "flow.noop" }]),
        json!([{ "from": "start", "to": "fetch" }, { "from": "fetch", "to": "ok" }, { "from": "fetch", "port": "error", "to": "failed" }]),
    );
    let r = go(d, json!({})).await;
    assert_eq!(r.outcome, Outcome::Success);
    assert_eq!(r.status("ok"), vec![NodeStatus::Skipped]);
    assert_eq!(r.items("failed", "main").len(), 1);
}

#[tokio::test]
async fn the_workflow_setting_on_error_is_the_default() {
    let mut d = def(json!([start(), fetch(json!({})), { "id": "after", "type": "flow.noop" }]), edges(&["start", "fetch", "after"]));
    d["settings"] = json!({ "on_error": "continue" });
    let r = go(d, json!({})).await;
    assert_eq!(r.outcome, Outcome::Success);
    assert_eq!(r.items("after", "main").len(), 1);
}

#[tokio::test]
async fn retry_tries_again_and_counts() {
    let calls = Arc::new(AtomicUsize::new(0));
    let d = def(
        json!([start(), { "id": "flaky", "type": "test.flaky", "retry": { "max": 2, "delay_ms": 1 } }]),
        edges(&["start", "flaky"]),
    );
    let mut spec = spec_for(d.clone(), json!({}));
    spec.custom.insert("test.flaky".into(), Arc::new(Flaky { spec: spec_of("test.flaky"), fail: 2, calls: calls.clone() }));
    let r = go_with(spec).await;
    assert_eq!(r.outcome, Outcome::Success);
    assert_eq!(calls.load(Ordering::SeqCst), 3, "2 failures and 1 success");

    // one retry is not enough for two failures
    let calls = Arc::new(AtomicUsize::new(0));
    let d = def(json!([start(), { "id": "flaky", "type": "test.flaky", "retry": { "max": 1, "delay_ms": 1 } }]), edges(&["start", "flaky"]));
    let mut spec = spec_for(d, json!({}));
    spec.custom.insert("test.flaky".into(), Arc::new(Flaky { spec: spec_of("test.flaky"), fail: 2, calls: calls.clone() }));
    let r = go_with(spec).await;
    assert_eq!(r.error_code(), "flaky");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_node_that_takes_too_long_times_out() {
    let d = def(json!([start(), { "id": "slow", "type": "test.hang" }]), edges(&["start", "slow"]));
    let mut spec = spec_for(d, json!({}));
    spec.node_timeout = Duration::from_millis(60);
    spec.custom.insert("test.hang".into(), Arc::new(Hang(spec_of("test.hang"))));
    let r = go_with(spec).await;
    assert_eq!(r.error_code(), "node_timeout");
}

#[tokio::test]
async fn cancel_stops_between_items() {
    let seen = Arc::new(AtomicUsize::new(0));
    let d = def(json!([start(), split(), { "id": "c", "type": "test.cancel_at" }, { "id": "after", "type": "flow.noop" }]), edges(&["start", "split", "c", "after"]));
    let mut spec = spec_for(d, leads_trigger());
    spec.custom.insert("test.cancel_at".into(), Arc::new(CancelAt { spec: spec_of("test.cancel_at"), at: 1, seen: seen.clone() }));
    let r = go_with(spec).await;
    assert_eq!(r.outcome, Outcome::Cancelled);
    assert_eq!(seen.load(Ordering::SeqCst), 2, "items 0 and 1 ran, item 2 did not");
    assert!(r.store.records_of("after").is_empty());
}

#[tokio::test]
async fn batch_runs_items_in_groups_and_keeps_the_order() {
    let calls = Arc::new(AtomicUsize::new(0));
    let d = def(json!([start(), split(), { "id": "f", "type": "test.flaky", "batch": { "size": 2, "delay_ms": 1 } }]), edges(&["start", "split", "f"]));
    let mut spec = spec_for(d, leads_trigger());
    spec.custom.insert("test.flaky".into(), Arc::new(Flaky { spec: spec_of("test.flaky"), fail: 0, calls: calls.clone() }));
    let r = go_with(spec).await;
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let ns: Vec<Value> = r.items("f", "main").into_iter().map(|j| j["n"].clone()).collect();
    assert_eq!(ns, vec![json!(1), json!(5), json!(9)]);
}

#[tokio::test]
async fn a_run_that_works_too_long_is_stopped() {
    let mut d = def(json!([start(), { "id": "a", "type": "flow.noop" }]), edges(&["start", "a"]));
    d["settings"] = json!({ "run_timeout_secs": 60 });
    let spec = spec_for(d, json!({}));
    let store = MemStore::new();
    let mut state = RunState { active_ms: 61_000, ..Default::default() };
    let out = execute(&spec, &mut state, &store, &Control::new()).await.unwrap();
    let Outcome::Error(e) = out else { panic!("{out:?}") };
    assert_eq!(e["code"], "run_timeout");
}

// ---------- checkpoints and Wait ----------

#[tokio::test]
async fn another_worker_goes_on_from_the_last_saved_step() {
    let d = def(
        json!([start(), { "id": "a", "type": "data.set", "params": { "fields": { "a": 1 } } }, { "id": "b", "type": "data.set", "params": { "fields": { "b": "${{ json.a }}" } } },
               { "id": "c", "type": "data.set", "params": { "fields": { "c": "${{ json.b }}" } } }]),
        edges(&["start", "a", "b", "c"]),
    );
    // the first worker dies when it tries to save node 3
    let store1 = MemStore::new();
    *store1.abort_after_steps.lock().unwrap() = Some(2);
    let mut state1 = RunState::default();
    let died = execute(&spec_for(d.clone(), json!({})), &mut state1, &store1, &Control::new()).await;
    assert_eq!(died, Err(StoreError::LeaseLost));
    let saved = store1.state().expect("two steps were saved");
    assert_eq!(saved.finished.len(), 2);
    assert!(saved.outputs.get("b").is_none(), "node 3 was never saved");

    // the second worker starts from the saved state
    let store2 = MemStore::new();
    let mut state2 = saved;
    let out = execute(&spec_for(d, json!({})), &mut state2, &store2, &Control::new()).await.unwrap();
    assert_eq!(out, Outcome::Success);
    let ran: Vec<String> = store2.records.lock().unwrap().iter().map(|r| r.node_id.clone()).collect();
    assert_eq!(ran, vec!["b", "c"], "start and a are not run again");
    // no output is doubled
    assert_eq!(state2.outputs["start"].items.len(), 1);
    assert_eq!(state2.outputs["a"].items.len(), 1);
    assert_eq!(state2.outputs["c"].items[0].json["c"], 1);
    // the sequence numbers go on
    assert_eq!(store2.records.lock().unwrap()[0].seq, 2);
}

#[tokio::test]
async fn a_short_wait_happens_in_the_task() {
    let d = def(json!([start(), { "id": "w", "type": "flow.wait", "params": { "mode": "duration", "amount": 0.05, "unit": "seconds" } }, { "id": "x", "type": "flow.noop" }]), edges(&["start", "w", "x"]));
    let t = std::time::Instant::now();
    let r = go(d, json!({})).await;
    assert_eq!(r.outcome, Outcome::Success);
    assert!(t.elapsed() >= Duration::from_millis(45));
    assert_eq!(r.items("x", "main").len(), 1);
}

#[tokio::test]
async fn a_long_wait_parks_the_run_and_it_goes_on_later() {
    let d = def(
        json!([start(), { "id": "w", "type": "flow.wait", "params": { "mode": "duration", "amount": 2, "unit": "minutes" } }, { "id": "x", "type": "data.set", "params": { "fields": { "done": true } } }]),
        edges(&["start", "w", "x"]),
    );
    let before = chrono::Utc::now();
    let r = go(d.clone(), json!({ "k": 1 })).await;
    let Outcome::Waiting(at) = r.outcome else { panic!("{:?}", r.outcome) };
    assert!(at >= before + chrono::Duration::seconds(119) && at <= before + chrono::Duration::seconds(125), "{at}");
    assert!(r.store.records_of("x").is_empty());
    assert!(r.store.event_kinds().contains(&"waiting".to_string()));

    // later: the saved state has the Wait node done, so the run goes on with the next node
    let mut state = r.store.state().expect("the wait was saved");
    assert!(state.finished.contains("w"));
    let store = MemStore::new();
    let out = execute(&spec_for(d, json!({ "k": 1 })), &mut state, &store, &Control::new()).await.unwrap();
    assert_eq!(out, Outcome::Success);
    assert_eq!(state.outputs["x"].items[0].json["done"], true);
    assert_eq!(store.records_of("w").len(), 0, "the Wait node does not run again");
}

#[tokio::test]
async fn wait_until_a_time_and_the_30_day_limit() {
    let far = (chrono::Utc::now() + chrono::Duration::days(40)).to_rfc3339();
    let d = def(json!([start(), { "id": "w", "type": "flow.wait", "params": { "mode": "until", "until": far } }]), edges(&["start", "w"]));
    assert_eq!(go(d, json!({})).await.error_code(), "wait_too_long");

    let soon = (chrono::Utc::now() + chrono::Duration::hours(3)).to_rfc3339();
    let d = def(json!([start(), { "id": "w", "type": "flow.wait", "params": { "mode": "until", "until": soon } }]), edges(&["start", "w"]));
    assert!(matches!(go(d, json!({})).await.outcome, Outcome::Waiting(_)));

    // a time in the past does not wait
    let d = def(json!([start(), { "id": "w", "type": "flow.wait", "params": { "mode": "until", "until": "2020-01-01T00:00:00Z" } }]), edges(&["start", "w"]));
    assert_eq!(go(d, json!({})).await.outcome, Outcome::Success);
}

// ---------- pins, dry runs, test step ----------

#[tokio::test]
async fn pins_replace_the_call_in_test_runs_and_are_ignored_in_production() {
    let mut d = def(
        json!([start(), { "id": "fetch", "type": "web.fetch_page", "params": { "url": "acme.com" } }, { "id": "use", "type": "data.set", "params": { "fields": { "t": "${{ json.title }}" } } }]),
        edges(&["start", "fetch", "use"]),
    );
    d["pins"] = json!({ "fetch": [{ "title": "Pinned title" }] });

    // test run: the node would fail with no_environment, the pin makes it work
    let r = go(d.clone(), json!({})).await;
    assert_eq!(r.outcome, Outcome::Success, "{:?}", r.outcome);
    assert_eq!(r.items("use", "main")[0]["t"], "Pinned title");
    assert!(r.store.events.lock().unwrap().iter().any(|e| e.0 == "log" && e.1.as_deref() == Some("fetch")));

    // production run: the pin is ignored, the node is called (and fails)
    let mut spec = spec_for(d, json!({}));
    spec.facts.mode = RunMode::Production;
    let r = go_with(spec).await;
    assert_eq!(r.error_code(), "no_environment");
}

const LEAD: &str = "0192f3a0-0000-7000-8000-000000000001";

#[tokio::test]
async fn side_effect_nodes_run_dry_in_test_runs() {
    let d = def(
        json!([start(), { "id": "upd", "type": "aikido.lead.update", "params": { "lead_id": "${{ json.id }}", "fields": { "first_name": "x" } } }, { "id": "x", "type": "flow.noop" }]),
        edges(&["start", "upd", "x"]),
    );
    let r = go(d.clone(), json!({ "id": LEAD })).await;
    assert_eq!(r.outcome, Outcome::Success);
    assert_eq!(r.status("upd"), vec![NodeStatus::Dry]);
    let item = &r.items("upd", "main")[0];
    assert_eq!(item["dry_run"], true);
    // the real method, path and body
    assert_eq!(item["would_call"]["method"], "PATCH");
    assert_eq!(item["would_call"]["path"], format!("/v1/leads/{LEAD}"));
    assert_eq!(item["would_call"]["body"], json!({ "first_name": "x" }));

    // "Real writes" turns it on: the node is called (no server services in this test, so it says so)
    let mut spec = spec_for(d.clone(), json!({ "id": LEAD }));
    spec.facts.real_writes = true;
    assert_eq!(go_with(spec).await.error_code(), "no_environment");
    // production runs are always real
    let mut spec = spec_for(d, json!({ "id": LEAD }));
    spec.facts.mode = RunMode::Production;
    assert_eq!(go_with(spec).await.error_code(), "no_environment");
}

/// A node with a side effect and no plan of its own: the dry run shows its type and params.
struct Writer(NodeSpec);

#[async_trait]
impl NodeType for Writer {
    fn spec(&self) -> &NodeSpec {
        &self.0
    }
    async fn run(&self, _ctx: &NodeCtx<'_>, input: NodeInput) -> Result<NodeOutput, NodeError> {
        Ok(NodeOutput::main(input.items))
    }
}

#[tokio::test]
async fn a_side_effect_node_without_a_plan_shows_its_type_and_params() {
    let d = def(json!([start(), { "id": "w", "type": "custom.write", "params": { "a": "${{ json.v }}" } }]), edges(&["start", "w"]));
    let mut spec = spec_for(d, json!({ "v": 7 }));
    spec.custom.insert("custom.write".into(), Arc::new(Writer(spec_of("custom.write").side_effect())));
    let r = go_with(spec).await;
    assert_eq!(r.status("w"), vec![NodeStatus::Dry]);
    assert_eq!(r.items("w", "main")[0]["would_call"], json!({ "node_type": "custom.write", "params": { "a": 7 } }));
}

#[tokio::test]
async fn a_dry_node_still_reports_a_bad_expression() {
    let d = def(json!([start(), { "id": "upd", "type": "aikido.lead.update", "params": { "lead_id": "${{ json.nope }}", "fields": {} } }]), edges(&["start", "upd"]));
    assert_eq!(go(d, json!({})).await.error_code(), "expression_error");
}

#[tokio::test]
async fn only_node_runs_the_path_to_that_node() {
    let d = def(
        json!([start(), { "id": "a", "type": "data.set", "params": { "fields": { "a": 1 } } }, { "id": "b", "type": "data.set", "params": { "fields": { "b": 2 } } },
               { "id": "c", "type": "flow.noop" }, { "id": "side", "type": "flow.noop" }]),
        json!([{ "from": "start", "to": "a" }, { "from": "a", "to": "b" }, { "from": "b", "to": "c" }, { "from": "start", "to": "side" }]),
    );
    let mut spec = spec_for(d, json!({}));
    spec.only_node = Some("b".into());
    let r = go_with(spec).await;
    assert_eq!(r.outcome, Outcome::Success);
    assert_eq!(r.items("b", "main")[0]["b"], 2);
    assert!(r.store.records_of("c").is_empty(), "nodes after the tested node do not run");
    assert!(r.store.records_of("side").is_empty(), "nodes off the path do not run");
}

#[tokio::test]
async fn only_node_that_the_trigger_cannot_reach_is_an_error() {
    let d = def(json!([start(), { "id": "lonely", "type": "flow.noop" }]), json!([]));
    let mut spec = spec_for(d, json!({}));
    spec.only_node = Some("lonely".into());
    assert_eq!(go_with(spec).await.error_code(), "invalid_plan");
}

#[tokio::test]
async fn the_trigger_item_is_the_start_of_every_lineage() {
    let d = def(json!([start(), { "id": "a", "type": "flow.noop" }]), edges(&["start", "a"]));
    let r = go(d, json!({ "k": 1 })).await;
    let item = &r.state.outputs["a"].items[0];
    assert_eq!(item.lineage, vec![("start".to_string(), 0), ("a".to_string(), 0)]);
}

#[tokio::test]
async fn the_stored_output_is_cut_at_512_kb_but_the_state_keeps_everything() {
    let big = "x".repeat(100_000);
    let rows: Vec<Value> = (0..10).map(|i| json!({ "i": i, "pad": big })).collect();
    let d = def(json!([start(), { "id": "rows", "type": "data.split_out", "params": { "path": "rows" } }]), edges(&["start", "rows"]));
    let r = go(d, json!({ "rows": rows })).await;
    let rec = &r.store.records_of("rows")[0];
    assert!(rec.truncated);
    assert_eq!(rec.output_count["main"], 10);
    assert!(rec.output.as_ref().unwrap()["main"].as_array().unwrap().len() < 10);
    assert_eq!(r.items("rows", "main").len(), 10);
}

#[tokio::test]
async fn save_data_errors_only_keeps_no_output_of_good_nodes() {
    let mut d = def(json!([start(), { "id": "a", "type": "flow.noop" }]), edges(&["start", "a"]));
    d["settings"] = json!({ "save_data": "errors_only" });
    // a test run always keeps everything
    let r = go(d.clone(), json!({})).await;
    assert!(r.store.records_of("a")[0].output.is_some());
    let mut spec = spec_for(d, json!({}));
    spec.facts.mode = RunMode::Production;
    let r = go_with(spec).await;
    assert!(r.store.records_of("a")[0].output.is_none());
    assert_eq!(r.items("a", "main").len(), 1, "the run itself still has the data");
}
