//! `flow.execute_workflow` (phase 9): one workflow runs another workflow and uses its result.
//!
//! How it works:
//! - The sub-workflow runs **inside the node**, in the same task, with the same run user, the same `Control` (Cancel
//!   stops it too) and the same counter of outside calls. It has no run row of its own: what it did is one summary
//!   line in the log of the calling run (node by node, with the status and the first error).
//! - Production run: the **published** version of the other workflow (it need not be active). Test run: its **draft**,
//!   in test mode, so its side-effect nodes are dry too (unless "Real writes" is on), and its pins are used.
//! - Depth limit 5 (`MAX_DEPTH`): a workflow started by a trigger is level 0, its sub-workflow level 1, and so on.
//!   Loop guard: a workflow that is already running higher up in the chain can not be called again
//!   (`sub_workflow_loop`), so A to B to A stops at once.
//! - The sub-workflow starts at its manual trigger (else its first trigger, or the node `trigger`). The trigger item is
//!   one input item (`input: each`, the default: one sub-run per input item) or `{ "items": [...] }` with all input
//!   items (`input: all`: one sub-run).
//! - It gives back the items of the **last node that made items** (in run order, trigger excluded), all ports together,
//!   or the node named in `return_node`. A sub-workflow that made no items gives none.
//! - A sub-workflow can wait at most 60 seconds (a long Wait would have to park the calling run in the middle of a
//!   node). A failure in the sub-workflow fails this node with `sub_workflow_failed` and names the failed node.
use super::env::NodeEnv;
use super::eval::{RunFacts, RunMode};
use super::run::{Outcome, RunSpec, execute};
use super::state::RunState;
use super::store::{MemStore, NodeStatus};
use super::{Control, graph};
use crate::automations::definition::WorkflowDefinition;
use crate::automations::registry::{Item, LINEAGE_CUT, NodeCtx, NodeError, NodeInput, NodeOutput, NodeSpec, NodeType, Registry};
use crate::automations::secrets::Secrets;
use crate::state::AppState;
use async_trait::async_trait;
use serde_json::{Map, Value, json};
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// The deepest level of sub-workflows.
pub const MAX_DEPTH: u32 = 5;
/// The most sub-runs one node execution makes in mode `each`.
pub const MAX_EACH: usize = 500;

/// A workflow loaded to run as a sub-workflow.
pub struct SubWorkflow {
    pub def: WorkflowDefinition,
    /// The published version. `None` for a draft.
    pub version: Option<i32>,
    pub secrets: Arc<Secrets>,
}

#[async_trait]
pub trait WorkflowLoader: Send + Sync {
    /// The workflow to run: the draft in test mode, the published version in production.
    async fn load(&self, workflow_id: &str, mode: RunMode) -> Result<SubWorkflow, NodeError>;
}

/// The real loader: the database.
pub struct DbWorkflows {
    pub state: AppState,
}

fn db(e: sqlx::Error) -> NodeError {
    tracing::error!(error = %e, "database error in flow.execute_workflow");
    NodeError::new("internal_error", "A database error stopped the sub-workflow.")
}

#[async_trait]
impl WorkflowLoader for DbWorkflows {
    async fn load(&self, workflow_id: &str, mode: RunMode) -> Result<SubWorkflow, NodeError> {
        let id = Uuid::parse_str(workflow_id.trim()).map_err(|_| NodeError::new("workflow_not_found", format!("`{workflow_id}` is not a workflow id.")))?;
        let pool = &self.state.pool;
        let (definition, version): (Value, Option<i32>) = match mode {
            RunMode::Test => {
                let row: Option<(Value, Option<chrono::DateTime<chrono::Utc>>)> = sqlx::query_as("SELECT draft, archived_at FROM automation_workflows WHERE id = $1").bind(id).fetch_optional(pool).await.map_err(db)?;
                match row {
                    None => return Err(NodeError::new("workflow_not_found", "There is no workflow with this id.")),
                    Some((_, Some(_))) => return Err(NodeError::new("workflow_archived", "This workflow is archived.")),
                    Some((draft, None)) => (draft, None),
                }
            }
            RunMode::Production => {
                let row: Option<(Option<chrono::DateTime<chrono::Utc>>, Option<Value>, Option<i32>)> = sqlx::query_as(
                    "SELECT w.archived_at, v.definition, v.version FROM automation_workflows w LEFT JOIN automation_versions v ON v.id = w.published_version_id WHERE w.id = $1",
                )
                .bind(id)
                .fetch_optional(pool)
                .await
                .map_err(db)?;
                match row {
                    None => return Err(NodeError::new("workflow_not_found", "There is no workflow with this id.")),
                    Some((Some(_), _, _)) => return Err(NodeError::new("workflow_archived", "This workflow is archived.")),
                    Some((None, None, _)) => return Err(NodeError::new("workflow_not_published", "This workflow has no published version. Publish it first (a production run uses the published version).")),
                    Some((None, Some(d), v)) => (d, v),
                }
            }
        };
        let def: WorkflowDefinition = serde_json::from_value(definition).map_err(|e| NodeError::new("invalid_definition", format!("The sub-workflow cannot be read: {e}")))?;
        let mut secrets = crate::automations::secrets::load(&self.state, &crate::automations::secrets::names_used(&def)).await.map_err(db)?;
        for (user, name) in crate::automations::secrets::basic_auth_pairs(&def) {
            secrets.add_basic(&user, &name);
        }
        Ok(SubWorkflow { def, version, secrets: Arc::new(secrets) })
    }
}

/// The node for a spec, or the spec back when it is not this node.
#[allow(clippy::result_large_err)]
pub fn make(spec: NodeSpec) -> Result<Box<dyn NodeType>, NodeSpec> {
    match spec.type_id {
        "flow.execute_workflow" => Ok(Box::new(ExecuteWorkflow(spec))),
        _ => Err(spec),
    }
}

struct ExecuteWorkflow(NodeSpec);

fn bad(msg: impl Into<String>) -> NodeError {
    NodeError::new("invalid_param_value", msg)
}

/// The trigger to start a sub-workflow from: the named node, else the manual trigger, else the first trigger.
pub fn pick_trigger(def: &WorkflowDefinition, wanted: Option<&str>) -> Result<String, NodeError> {
    if let Some(w) = wanted.filter(|w| !w.is_empty()) {
        let node = def.node(w).ok_or_else(|| NodeError::new("invalid_param_value", format!("The sub-workflow has no node `{w}`.")))?;
        return if Registry::global().spec(&node.type_id).is_some_and(|s| s.is_trigger()) { Ok(w.to_string()) } else { Err(bad(format!("`{w}` is not a trigger of the sub-workflow."))) };
    }
    let is_trigger = |n: &&crate::automations::definition::Node| Registry::global().spec(&n.type_id).is_some_and(|s| s.is_trigger());
    def.nodes
        .iter()
        .find(|n| n.type_id == "trigger.manual")
        .or_else(|| def.nodes.iter().find(is_trigger))
        .map(|n| n.id.clone())
        .ok_or_else(|| NodeError::new("invalid_definition", "The sub-workflow has no trigger."))
}

/// The items a sub-run gives back: the named node, else the last node (in run order, trigger excluded) with items.
pub fn result_items(def: &WorkflowDefinition, trigger: &str, state: &RunState, return_node: Option<&str>) -> Result<Vec<Item>, NodeError> {
    let from = |id: &str| -> Vec<Item> { state.outputs.get(id).map(|o| o.items.clone()).unwrap_or_default() };
    if let Some(id) = return_node.filter(|r| !r.is_empty()) {
        if def.node(id).is_none() {
            return Err(bad(format!("`return_node` `{id}` is not a node of the sub-workflow.")));
        }
        return Ok(from(id));
    }
    let plan = graph::plan(def, trigger, None).map_err(|m| NodeError::new("invalid_plan", m))?;
    Ok(plan.order.iter().rev().filter(|id| **id != trigger).map(|id| from(id)).find(|items| !items.is_empty()).unwrap_or_default())
}

/// One line for the log: the nodes of the sub-run with their status.
fn summary(name: &str, version: Option<i32>, store: &MemStore, took: Duration) -> String {
    let recs = store.records.lock().unwrap();
    let nodes: Vec<String> = recs.iter().filter(|r| r.status != NodeStatus::Skipped).map(|r| format!("{} {}", r.node_id, r.status.as_str())).collect();
    let v = version.map(|v| format!(" v{v}")).unwrap_or_else(|| " (draft)".into());
    format!("Sub-workflow \"{name}\"{v}: {} in {} ms.", if nodes.is_empty() { "no node ran".to_string() } else { nodes.join(", ") }, took.as_millis())
}

struct SubRun<'a> {
    env: &'a NodeEnv,
    parent: &'a RunFacts,
    control: &'a Control,
    sub: &'a SubWorkflow,
    workflow_id: &'a str,
    trigger: &'a str,
    chain: &'a [String],
}

impl SubRun<'_> {
    async fn run(&self, trigger_item: Map<String, Value>, return_node: Option<&str>, logs: &mut Vec<String>) -> Result<Vec<Item>, NodeError> {
        let sub = self.sub;
        let facts = RunFacts {
            run_id: Uuid::now_v7().to_string(),
            mode: self.parent.mode,
            started_at: self.parent.now(),
            workflow_id: self.workflow_id.to_string(),
            workflow_name: sub.def.name.clone(),
            workflow_version: sub.version,
            vars: sub.def.variables.iter().map(|v| (v.key.clone(), v.value.clone().unwrap_or(Value::Null))).collect(),
            trigger_node_id: self.trigger.to_string(),
            trigger: trigger_item,
            real_writes: self.parent.real_writes,
            clock: self.parent.clock.clone(),
        };
        let child_env = Arc::new(self.env.child(sub.secrets.clone(), self.chain.to_vec()));
        let mut spec = RunSpec::new(sub.def.clone(), facts);
        spec.secrets = sub.secrets.clone();
        spec.env = Some(child_env);
        let store = MemStore::new();
        let mut state = RunState::default();
        let t0 = Instant::now();
        let outcome = execute(&spec, &mut state, &store, self.control).await.map_err(|e| NodeError::new("sub_workflow_failed", format!("The sub-workflow could not be saved in memory: {e:?}")))?;
        logs.push(summary(&sub.def.name, sub.version, &store, t0.elapsed()));
        match outcome {
            Outcome::Success => result_items(&sub.def, self.trigger, &state, return_node),
            Outcome::Error(e) => {
                let node = e["node_id"].as_str().map(|n| format!(" in node `{n}`")).unwrap_or_default();
                let code = e["code"].as_str().unwrap_or("error");
                // a depth or loop error from further down keeps its own code, so the cause is easy to see
                let own = if code.starts_with("sub_workflow_") { code.to_string() } else { "sub_workflow_failed".to_string() };
                Err(NodeError::new(own, format!("The sub-workflow \"{}\" failed{node} ({code}): {}", sub.def.name, e["message"].as_str().unwrap_or(""))))
            }
            Outcome::Waiting(_) => Err(NodeError::new("sub_workflow_waiting", format!("The sub-workflow \"{}\" wants to wait longer than 60 seconds. A sub-workflow can wait at most 60 seconds.", sub.def.name))),
            Outcome::Cancelled | Outcome::Lost => Err(NodeError::new("sub_workflow_cancelled", "The run was cancelled.")),
        }
    }
}

#[async_trait]
impl NodeType for ExecuteWorkflow {
    fn spec(&self) -> &NodeSpec {
        &self.0
    }

    /// A sub-workflow may run many nodes: it gets more time than a normal node (its own nodes have their limits).
    fn timeout(&self, _params: &Map<String, Value>) -> Option<Duration> {
        Some(Duration::from_secs(900))
    }

    async fn run(&self, ctx: &NodeCtx<'_>, input: NodeInput) -> Result<NodeOutput, NodeError> {
        let env = ctx.env()?;
        let loader = env.workflows.as_ref().ok_or_else(|| NodeError::new("no_environment", "Execute workflow needs the server and can not run here."))?;
        let none = input.items.first().cloned().unwrap_or_else(NodeCtx::no_item);
        let workflow_id = ctx.str_param(&none, input.first_index, "workflow_id")?.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).ok_or_else(|| bad("`workflow_id` is empty."))?;
        let each = match ctx.str_param(&none, input.first_index, "input")?.as_deref().unwrap_or("each") {
            "each" => true,
            "all" => false,
            other => return Err(bad(format!("`input` `{other}` is not each or all."))),
        };
        let trigger_param = ctx.str_param(&none, input.first_index, "trigger")?;
        let return_node = ctx.str_param(&none, input.first_index, "return_node")?;

        // depth and loop guards
        let facts = ctx.eval.facts();
        if env.depth >= MAX_DEPTH {
            return Err(NodeError::new("sub_workflow_depth", format!("Sub-workflows can go {MAX_DEPTH} levels deep. This one would be level {}.", env.depth + 1)));
        }
        let mut chain = env.chain.clone();
        if !chain.contains(&facts.workflow_id) {
            chain.push(facts.workflow_id.clone());
        }
        if chain.iter().any(|w| w.eq_ignore_ascii_case(&workflow_id)) {
            return Err(NodeError::new("sub_workflow_loop", "This workflow is already running higher up in the chain. A workflow can not call itself, directly or through another workflow."));
        }
        let mut sub_chain = chain;
        sub_chain.push(workflow_id.clone());

        let sub = loader.load(&workflow_id, facts.mode).await?;
        let trigger = pick_trigger(&sub.def, trigger_param.as_deref())?;
        if each && input.items.len() > MAX_EACH {
            return Err(NodeError::new("too_many_sub_runs", format!("{} items would start {} sub-runs. The limit is {MAX_EACH}. Use `input: all` or Limit the items first.", input.items.len(), input.items.len())));
        }

        let run = SubRun { env, parent: facts, control: ctx.control, sub: &sub, workflow_id: &workflow_id, trigger: &trigger, chain: &sub_chain };
        let mut logs = vec![];
        let mut out: Vec<Item> = vec![];
        if each {
            for item in &input.items {
                if ctx.control.should_stop() {
                    break;
                }
                let items = run.run(item.json.clone(), return_node.as_deref(), &mut logs).await?;
                // the result is linked to the input item it was made from
                out.extend(items.into_iter().map(|i| item.child(i.json)));
            }
        } else {
            let all = Value::Array(input.items.iter().map(|i| Value::Object(i.json.clone())).collect());
            let trigger_item = json!({ "items": all }).as_object().cloned().unwrap_or_default();
            let items = run.run(trigger_item, return_node.as_deref(), &mut logs).await?;
            out.extend(items.into_iter().map(|i| Item { json: i.json, lineage: vec![(LINEAGE_CUT.into(), 0)] }));
        }
        let mut o = NodeOutput::main(out);
        o.logs = logs;
        Ok(o)
    }
}
