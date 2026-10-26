//! The executor (build map sections 5 and 8). It runs one workflow from one trigger.
//!
//! - Nodes run one at a time, in the order of `graph::plan`.
//! - A node with no input items is skipped. A node with 2 input edges runs once per edge (Merge runs once).
//! - A per-item node runs once for each item (in groups when the node has `batch`), with `retry`,
//!   a timeout and `on_error`. An all-items node runs once.
//! - After each node the executor saves a checkpoint through the `RunStore`. A new executor can go on from it.
//! - Test runs use pins and run side-effect nodes dry (decision D3).
use super::Control;
use super::env::NodeEnv;
use super::eval::{Evaluator, RunFacts, RunMode};
use super::graph;
use super::state::RunState;
use super::store::{NodeRecord, NodeStatus, RunStore, StoreError};
use crate::automations::definition::{Node, OnError, Retry, SaveData, WorkflowDefinition};
use crate::automations::expr::ExprEngine;
use crate::automations::registry::{ExecMode, Item, NodeCtx, NodeError, NodeInput, NodeOutput, NodeType, Registry};
use crate::automations::secrets::Secrets;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The most items one node execution may give.
pub const MAX_ITEMS: usize = 10_000;
/// The biggest item (JSON text).
pub const MAX_ITEM_BYTES: usize = 1_048_576;
/// The stored output of one node is cut at this size.
pub const MAX_STORED_OUTPUT_BYTES: usize = 512 * 1024;
/// A wait up to this long keeps the run in the task. A longer one parks it.
pub const SHORT_WAIT: Duration = Duration::from_secs(60);

pub struct RunSpec {
    pub def: WorkflowDefinition,
    pub facts: RunFacts,
    /// Test step: run only the path from the trigger to this node.
    pub only_node: Option<String>,
    /// Where the item of the first node comes from, when that node runs alone (the node before it in the file).
    pub trigger_lineage: Vec<(String, usize)>,
    /// How long one node call may take.
    pub node_timeout: Duration,
    /// Node types that are not in the registry. Empty in production; tests put fake nodes here.
    pub custom: BTreeMap<String, Arc<dyn NodeType>>,
    /// The secret values the run can use. They are hidden in everything the run stores (build map 11.3).
    pub secrets: Arc<Secrets>,
    /// The services of the server for nodes that talk to the outside. `None` in tests of pure nodes.
    pub env: Option<Arc<NodeEnv>>,
}

impl RunSpec {
    pub fn new(def: WorkflowDefinition, facts: RunFacts) -> Self {
        RunSpec { def, facts, only_node: None, trigger_lineage: vec![], node_timeout: Duration::from_secs(120), custom: BTreeMap::new(), secrets: Arc::new(Secrets::default()), env: None }
    }

    fn node_type(&self, type_id: &str) -> Option<&dyn NodeType> {
        match self.custom.get(type_id) {
            Some(n) => Some(n.as_ref()),
            None => Registry::global().get(type_id),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Success,
    /// The run error: `{ code, message, node_id?, item_index?, status? }`.
    Error(Value),
    Cancelled,
    /// A long Wait: save the run as `waiting` and queue it again at this time.
    Waiting(DateTime<Utc>),
    /// This server copy lost the run. Write nothing.
    Lost,
}

pub fn error_object(code: &str, message: &str, node_id: Option<&str>, item_index: Option<usize>, status: Option<u16>) -> Value {
    let mut o = json!({ "code": code, "message": message });
    if let Some(n) = node_id {
        o["node_id"] = json!(n);
    }
    if let Some(i) = item_index {
        o["item_index"] = json!(i);
    }
    if let Some(s) = status {
        o["status"] = json!(s);
    }
    o
}

fn node_error_object(e: &NodeError, node_id: &str, item_index: Option<usize>) -> Value {
    error_object(&e.code, &e.message, Some(node_id), item_index, e.status)
}

/// One batch of items that a node gets in one execution.
struct Group {
    items: Vec<Item>,
    inputs: Vec<Vec<Item>>,
}

enum GroupEnd {
    Done { ports: BTreeMap<String, Vec<Item>>, status: NodeStatus, wait_until: Option<DateTime<Utc>>, stop: Option<crate::automations::registry::StopRequest>, logs: Vec<String> },
    Failed(Value),
    /// Cancel or lost lease while the node was working.
    Interrupted,
    /// The daily AI token budget is used up. The run waits and the node runs again (build map 7.3).
    BudgetWait(Value),
}

/// The error code of the AI Agent node when the shared token budget is used up.
const BUDGET_CODE: &str = "ai_budget_reached";
/// How long a run waits before it looks at the budget again.
const BUDGET_RECHECK_MINS: i64 = 10;

enum StepEnd {
    Next,
    Finish(Outcome),
}

/// Run (or go on with) a run. `state` is the checkpoint: empty for a new run.
pub async fn execute(spec: &RunSpec, state: &mut RunState, store: &dyn RunStore, control: &Control) -> Result<Outcome, StoreError> {
    let plan = match graph::plan(&spec.def, &spec.facts.trigger_node_id, spec.only_node.as_deref()) {
        Ok(p) => p,
        Err(m) => return Ok(Outcome::Error(error_object("invalid_plan", &m, None, None, None))),
    };
    let engine = if spec.secrets.is_empty() { ExprEngine::new() } else { ExprEngine::with_secrets(spec.secrets.to_map()) };
    let timeout_ms = u64::from(spec.def.settings.run_timeout_secs()) * 1000;
    for id in &plan.order {
        if control.is_lost() {
            return Ok(Outcome::Lost);
        }
        if control.is_cancelled() {
            return Ok(Outcome::Cancelled);
        }
        if state.finished.contains(*id) {
            if spec.only_node.as_deref() == Some(*id) {
                break;
            }
            continue;
        }
        if state.active_ms > timeout_ms {
            return Ok(Outcome::Error(error_object("run_timeout", &format!("The run worked for more than {} seconds.", timeout_ms / 1000), None, None, None)));
        }
        let t0 = Instant::now();
        match step(spec, &engine, &plan, state, store, control, id, t0).await? {
            StepEnd::Next => {}
            StepEnd::Finish(outcome) => return Ok(outcome),
        }
        if spec.only_node.as_deref() == Some(*id) {
            break;
        }
    }
    Ok(Outcome::Success)
}

#[allow(clippy::too_many_arguments)]
async fn step(
    spec: &RunSpec,
    engine: &ExprEngine,
    plan: &graph::Plan<'_>,
    state: &mut RunState,
    store: &dyn RunStore,
    control: &Control,
    id: &str,
    t0: Instant,
) -> Result<StepEnd, StoreError> {
    let node = spec.def.node(id).expect("the plan only has nodes of the file");
    let Some(node_impl) = spec.node_type(&node.type_id) else {
        let e = error_object("unknown_node_type", &format!("The node type `{}` does not exist.", node.type_id), Some(id), None, None);
        return Ok(StepEnd::Finish(Outcome::Error(e)));
    };

    // 1. The groups of items this node works on.
    let ins = plan.incoming.get(id).cloned().unwrap_or_default();
    let per_edge: Vec<Vec<Item>> = ins.iter().map(|(from, port)| state.port_items(from, port)).collect();
    let groups: Vec<Group> = if id == spec.facts.trigger_node_id {
        let mut first = Item::new(spec.facts.trigger.clone());
        first.lineage = spec.trigger_lineage.clone();
        let items = vec![first];
        vec![Group { inputs: vec![items.clone()], items }]
    } else if node.type_id == "flow.merge" {
        if per_edge.iter().all(Vec::is_empty) { vec![] } else { vec![Group { items: per_edge.iter().flatten().cloned().collect(), inputs: per_edge }] }
    } else {
        per_edge.into_iter().filter(|v| !v.is_empty()).map(|v| Group { inputs: vec![v.clone()], items: v }).collect()
    };

    let now = spec.facts.now();
    if groups.is_empty() {
        // no input items: the node does not run, and its children get nothing
        let seq = next_seq(state);
        let rec = NodeRecord {
            node_id: id.into(),
            seq,
            status: NodeStatus::Skipped,
            input_count: 0,
            output_count: BTreeMap::new(),
            output: None,
            truncated: false,
            error: None,
            started_at: now,
            finished_at: now,
        };
        state.finished.insert(id.to_string());
        state.active_ms += t0.elapsed().as_millis() as u64;
        save(store, &[rec], state).await?;
        return Ok(StepEnd::Next);
    }

    // 2. Run the groups.
    let mut records: Vec<NodeRecord> = vec![];
    let mut wait: Option<DateTime<Utc>> = None;
    let mut stop = None;
    let pinned = spec.facts.mode == RunMode::Test && spec.def.pins.contains_key(id);
    let outputs_before = state.outputs.get(id).cloned();
    for (gi, group) in groups.into_iter().enumerate() {
        let seq = next_seq(state);
        let started_at = spec.facts.now();
        store.node_started(id, seq, started_at).await?;
        store.event("node_started", Some(id), json!({ "seq": seq, "items": group.items.len() })).await;
        let input_count = group.items.len();

        let end = if pinned {
            if gi > 0 {
                // pins replace the whole node: one output, however many times it would have run
                records.push(skipped_record(id, seq, started_at));
                continue;
            }
            store.event("log", Some(id), json!({ "message": "The pinned output was used. The node was not called." })).await;
            pinned_end(spec, node_impl, id, &group)
        } else if node.disabled {
            GroupEnd::Done { ports: BTreeMap::from([("main".to_string(), group.items.clone())]), status: NodeStatus::Success, wait_until: None, stop: None, logs: vec![] }
        } else if node_impl.side_effect(&node.params) && spec.facts.mode == RunMode::Test && !spec.facts.real_writes {
            dry_end(spec, engine, state, control, node, node_impl, &group)
        } else {
            run_group(spec, engine, state, control, node, node_impl, &group).await
        };

        // the AI token budget is used up: the run waits and tries this node again, up to `ai_budget_wait_hours`
        let end = match end {
            GroupEnd::BudgetWait(mut err) => {
                let now = spec.facts.now();
                let since = *state.budget_wait_since.get_or_insert(now);
                let hours = spec.env.as_ref().map_or(0, |e| e.settings.ai_budget_wait_hours);
                if hours <= 0 || now - since >= ChronoDuration::hours(hours) {
                    err["message"] = json!(if hours <= 0 {
                        "The daily token budget of the AI is used up, and the setting ai_budget_wait_hours is 0.".to_string()
                    } else {
                        format!("The daily token budget of the AI stayed used up for {hours} hours.")
                    });
                    GroupEnd::Failed(err)
                } else {
                    // this node runs again from the start: forget what it gave so far
                    match &outputs_before {
                        Some(o) => {
                            state.outputs.insert(id.to_string(), o.clone());
                        }
                        None => {
                            state.outputs.remove(id);
                        }
                    }
                    let until = now + ChronoDuration::minutes(BUDGET_RECHECK_MINS);
                    records.push(NodeRecord {
                        node_id: id.into(),
                        seq,
                        status: NodeStatus::Waiting,
                        input_count,
                        output_count: BTreeMap::new(),
                        output: None,
                        truncated: false,
                        error: Some(err.clone()),
                        started_at,
                        finished_at: now,
                    });
                    state.active_ms += t0.elapsed().as_millis() as u64;
                    store.event("waiting", Some(id), json!({ "reason": BUDGET_CODE, "resume_at": until, "waiting_since": since })).await;
                    save(store, &records, state).await?;
                    return Ok(StepEnd::Finish(Outcome::Waiting(until)));
                }
            }
            other => other,
        };

        match end {
            GroupEnd::BudgetWait(_) => unreachable!("handled above"),
            GroupEnd::Interrupted => return Ok(StepEnd::Finish(if control.is_lost() { Outcome::Lost } else { Outcome::Cancelled })),
            GroupEnd::Failed(mut err) => {
                spec.secrets.redact(&mut err);
                let rec = NodeRecord {
                    node_id: id.into(),
                    seq,
                    status: NodeStatus::Error,
                    input_count,
                    output_count: BTreeMap::new(),
                    output: None,
                    truncated: false,
                    error: Some(err.clone()),
                    started_at,
                    finished_at: spec.facts.now(),
                };
                store.event("node_error", Some(id), err.clone()).await;
                records.push(rec);
                state.active_ms += t0.elapsed().as_millis() as u64;
                save(store, &records, state).await?;
                return Ok(StepEnd::Finish(Outcome::Error(err)));
            }
            GroupEnd::Done { mut ports, status, wait_until, stop: stop_req, logs } => {
                for m in logs {
                    store.event("log", Some(id), json!({ "message": m })).await;
                }
                // a secret never reaches the state, the stored output or the next node
                for item in ports.values_mut().flatten() {
                    spec.secrets.redact_map(&mut item.json);
                }
                let counts = commit(state, id, ports.clone());
                let save_output = spec.facts.mode == RunMode::Test || spec.def.settings.save_data() == SaveData::All;
                let (output, truncated) = if save_output { stored_output(&ports) } else { (None, false) };
                store.event("node_finished", Some(id), json!({ "seq": seq, "status": status.as_str(), "output": counts })).await;
                records.push(NodeRecord {
                    node_id: id.into(),
                    seq,
                    status,
                    input_count,
                    output_count: counts,
                    output,
                    truncated,
                    error: None,
                    started_at,
                    finished_at: spec.facts.now(),
                });
                wait = wait.max(wait_until);
                stop = stop.or(stop_req);
            }
        }
    }
    if control.should_stop() {
        return Ok(StepEnd::Finish(if control.is_lost() { Outcome::Lost } else { Outcome::Cancelled }));
    }

    state.finished.insert(id.to_string());
    state.budget_wait_since = None;
    state.active_ms += t0.elapsed().as_millis() as u64;
    save(store, &records, state).await?;

    if let Some(s) = stop {
        return Ok(StepEnd::Finish(if s.error {
            Outcome::Error(error_object("stopped_by_node", &s.message, Some(id), None, None))
        } else {
            Outcome::Success
        }));
    }
    if let Some(until) = wait {
        if until > spec.facts.now() {
            store.event("waiting", Some(id), json!({ "resume_at": until })).await;
            return Ok(StepEnd::Finish(Outcome::Waiting(until)));
        }
    }
    Ok(StepEnd::Next)
}

fn next_seq(state: &mut RunState) -> u32 {
    let s = state.seq;
    state.seq += 1;
    s
}

fn skipped_record(id: &str, seq: u32, at: DateTime<Utc>) -> NodeRecord {
    NodeRecord {
        node_id: id.into(),
        seq,
        status: NodeStatus::Skipped,
        input_count: 0,
        output_count: BTreeMap::new(),
        output: None,
        truncated: false,
        error: None,
        started_at: at,
        finished_at: at,
    }
}

async fn save(store: &dyn RunStore, records: &[NodeRecord], state: &RunState) -> Result<(), StoreError> {
    store.step_done(records, state).await
}

/// Put the items of a node execution into the run state. The item gets the lineage `(node, position)`.
/// Put the output of a node into the state without running it (a node that runs alone reads it).
/// Returns the lineage of the first item on `port`.
pub fn seed_output(state: &mut RunState, id: &str, ports: BTreeMap<String, Vec<Item>>, port: &str) -> Vec<(String, usize)> {
    commit(state, id, ports);
    state.outputs.get(id).and_then(|o| o.port_items(port).into_iter().next()).map(|i| i.lineage).unwrap_or_default()
}

fn commit(state: &mut RunState, id: &str, ports: BTreeMap<String, Vec<Item>>) -> BTreeMap<String, usize> {
    let out = state.outputs.entry(id.to_string()).or_default();
    let mut counts = BTreeMap::new();
    for (port, items) in ports {
        counts.insert(port.clone(), items.len());
        for mut item in items {
            let at = out.items.len();
            item.lineage.push((id.to_string(), at));
            out.items.push(item);
            out.ports.entry(port.clone()).or_default().push(at);
        }
    }
    counts
}

/// The output as stored for the run page: item JSON per port, cut at 512 KB.
fn stored_output(ports: &BTreeMap<String, Vec<Item>>) -> (Option<Value>, bool) {
    let mut budget = MAX_STORED_OUTPUT_BYTES;
    let mut truncated = false;
    let mut out = Map::new();
    for (port, items) in ports {
        let mut list = vec![];
        for item in items {
            let size = serde_json::to_string(&item.json).map(|s| s.len()).unwrap_or(0);
            if size > budget {
                truncated = true;
                break;
            }
            budget -= size;
            list.push(Value::Object(item.json.clone()));
        }
        out.insert(port.clone(), Value::Array(list));
    }
    (Some(Value::Object(out)), truncated)
}

fn pinned_end(spec: &RunSpec, node_impl: &dyn NodeType, id: &str, group: &Group) -> GroupEnd {
    let port = node_impl.spec().ports.first().copied().unwrap_or("main");
    let items = spec.def.pins.get(id).cloned().unwrap_or_default();
    let items: Vec<Item> = items
        .into_iter()
        .enumerate()
        .map(|(i, json)| match group.items.get(i) {
            Some(src) => src.child(json),
            None => Item::new(json),
        })
        .collect();
    GroupEnd::Done { ports: BTreeMap::from([(port.to_string(), items)]), status: NodeStatus::Success, wait_until: None, stop: None, logs: vec![] }
}

/// A side-effect node in a test run: say what it would do and change nothing (decision D3).
/// A node with a plan of its own shows the real method and path; the others show their type and params.
fn dry_end(spec: &RunSpec, engine: &ExprEngine, state: &RunState, control: &Control, node: &Node, node_impl: &dyn NodeType, group: &Group) -> GroupEnd {
    let ev = Evaluator::new(engine, &spec.facts, &state.outputs, &node.params);
    let ctx = NodeCtx { node_id: &node.id, params: &node.params, eval: &ev, control, env: spec.env.as_deref() };
    let params = Value::Object(node.params.clone());
    let mut out = vec![];
    for (i, item) in group.items.iter().enumerate() {
        let would = match node_impl.dry(&ctx, item, i) {
            Some(Ok(w)) => w,
            Some(Err(e)) => return GroupEnd::Failed(node_error_object(&e, &node.id, Some(i))),
            None => match ev.value(item, i, &params) {
                Ok(p) => json!({ "node_type": node.type_id, "params": p }),
                Err(e) => return GroupEnd::Failed(node_error_object(&e, &node.id, Some(i))),
            },
        };
        out.push(item.child(json!({ "dry_run": true, "would_call": would }).as_object().cloned().unwrap_or_default()));
    }
    GroupEnd::Done { ports: BTreeMap::from([("main".to_string(), out)]), status: NodeStatus::Dry, wait_until: None, stop: None, logs: vec![] }
}

fn check_limits(ports: &BTreeMap<String, Vec<Item>>) -> Result<(), NodeError> {
    let total: usize = ports.values().map(Vec::len).sum();
    if total > MAX_ITEMS {
        return Err(NodeError::new("too_many_items", format!("The node gave {total} items. The limit is {MAX_ITEMS}.")));
    }
    for item in ports.values().flatten() {
        if serde_json::to_vec(&item.json).map(|v| v.len()).unwrap_or(0) > MAX_ITEM_BYTES {
            return Err(NodeError::new("item_too_large", "An item is bigger than 1 MB."));
        }
    }
    Ok(())
}

/// Call a node with retries and a timeout. `retry.max` more tries; the wait grows by `delay_ms` each time.
async fn call(node_impl: &dyn NodeType, ctx: &NodeCtx<'_>, input: &NodeInput, retry: Retry, timeout: Duration, control: &Control) -> Result<NodeOutput, NodeError> {
    let mut attempt: u32 = 0;
    loop {
        let err = match tokio::time::timeout(timeout, node_impl.run(ctx, input.clone())).await {
            Ok(Ok(out)) => return Ok(out),
            Ok(Err(e)) => e,
            Err(_) => NodeError::new("node_timeout", format!("The node took more than {} seconds.", timeout.as_secs())),
        };
        if attempt >= retry.max || control.should_stop() || err.code == BUDGET_CODE {
            return Err(err);
        }
        attempt += 1;
        control.sleep(Duration::from_millis(retry.delay_ms.saturating_mul(u64::from(attempt)))).await;
    }
}

/// What happens to an item that failed, per `on_error`.
fn failed_item(item: &Item, err: &Value) -> Item {
    let mut json = item.json.clone();
    json.insert("error".into(), err.clone());
    item.child(json)
}

async fn run_group(spec: &RunSpec, engine: &ExprEngine, state: &RunState, control: &Control, node: &Node, node_impl: &dyn NodeType, group: &Group) -> GroupEnd {
    let evaluator = Evaluator::new(engine, &spec.facts, &state.outputs, &node.params);
    let ctx = NodeCtx { node_id: &node.id, params: &node.params, eval: &evaluator, control, env: spec.env.as_deref() };
    let retry = node.retry.unwrap_or(Retry { max: 0, delay_ms: 1000 });
    let timeout = node_impl.timeout(&node.params).map_or(spec.node_timeout, |t| t.max(spec.node_timeout));
    let on_error = node.on_error.unwrap_or_else(|| spec.def.settings.on_error());
    let mut ports: BTreeMap<String, Vec<Item>> = BTreeMap::new();
    let mut wait: Option<DateTime<Utc>> = None;
    let mut stop = None;
    let mut logs: Vec<String> = vec![];

    match node_impl.spec().mode {
        ExecMode::AllItems => {
            let input = NodeInput { items: group.items.clone(), inputs: group.inputs.clone(), first_index: 0 };
            match call(node_impl, &ctx, &input, retry, timeout, control).await {
                Ok(out) => {
                    wait = out.wait_until;
                    stop = out.stop;
                    logs.extend(out.logs);
                    for (p, items) in out.ports {
                        ports.entry(p).or_default().extend(items);
                    }
                }
                Err(_) if control.should_stop() => return GroupEnd::Interrupted,
                Err(e) if e.code == BUDGET_CODE => return GroupEnd::BudgetWait(node_error_object(&e, &node.id, None)),
                Err(e) => {
                    let err = node_error_object(&e, &node.id, None);
                    match on_error {
                        OnError::Stop => return GroupEnd::Failed(err),
                        OnError::Continue => ports.entry("main".into()).or_default().extend(group.items.iter().map(|i| failed_item(i, &err))),
                        OnError::ErrorPort => ports.entry("error".into()).or_default().extend(group.items.iter().map(|i| failed_item(i, &err))),
                    }
                }
            }
        }
        ExecMode::PerItem => {
            let (size, delay) = node.batch.map(|b| (b.size.max(1) as usize, b.delay_ms)).unwrap_or((1, 0));
            let total = group.items.len();
            let mut offset = 0;
            while offset < total {
                if control.should_stop() {
                    return GroupEnd::Interrupted;
                }
                let end = (offset + size).min(total);
                let calls = group.items[offset..end].iter().enumerate().map(|(k, item)| {
                    let input = NodeInput { items: vec![item.clone()], inputs: vec![vec![item.clone()]], first_index: offset + k };
                    let ctx = &ctx;
                    async move { call(node_impl, ctx, &input, retry, timeout, control).await }
                });
                let results = futures::future::join_all(calls).await;
                if control.should_stop() {
                    return GroupEnd::Interrupted;
                }
                for (k, result) in results.into_iter().enumerate() {
                    let index = offset + k;
                    match result {
                        Ok(out) => {
                            wait = wait.max(out.wait_until);
                            stop = stop.or(out.stop);
                            logs.extend(out.logs);
                            for (p, items) in out.ports {
                                ports.entry(p).or_default().extend(items);
                            }
                        }
                        Err(e) if e.code == BUDGET_CODE => return GroupEnd::BudgetWait(node_error_object(&e, &node.id, Some(index))),
                        Err(e) => {
                            let err = node_error_object(&e, &node.id, Some(index));
                            let item = &group.items[index];
                            match on_error {
                                OnError::Stop => return GroupEnd::Failed(err),
                                OnError::Continue => ports.entry("main".into()).or_default().push(failed_item(item, &err)),
                                OnError::ErrorPort => ports.entry("error".into()).or_default().push(failed_item(item, &err)),
                            }
                        }
                    }
                }
                if stop.is_some() {
                    break;
                }
                offset = end;
                if offset < total && delay > 0 {
                    control.sleep(Duration::from_millis(delay)).await;
                }
            }
        }
    }
    if let Err(e) = check_limits(&ports) {
        return GroupEnd::Failed(node_error_object(&e, &node.id, None));
    }
    GroupEnd::Done { ports, status: NodeStatus::Success, wait_until: wait, stop, logs }
}
