//! Reads `${{ }}` for one item while a node runs (build map sections 5.4 and 6.3).
//!
//! The data an expression sees:
//! - `json`: the item the node works on
//! - `trigger`: the trigger item of the run
//! - `nodes.<id>.json`: the item of node `<id>` that this item came from (found by its lineage)
//! - `nodes.<id>.items`: all output items of node `<id>`
//! - `vars`, `run`, `workflow`
use crate::automations::expr::{ExprEngine, expression_strings};
use crate::automations::registry::{Item, LINEAGE_CUT, NodeError};
use crate::automations::schema_kit::has_expression;
use super::state::NodeOut;
use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Test,
    Production,
}

impl RunMode {
    pub fn as_str(self) -> &'static str {
        match self {
            RunMode::Test => "test",
            RunMode::Production => "production",
        }
    }
}

/// The facts of a run that do not change while it runs.
#[derive(Clone)]
pub struct RunFacts {
    pub run_id: String,
    pub mode: RunMode,
    pub started_at: DateTime<Utc>,
    pub workflow_id: String,
    pub workflow_name: String,
    /// `None` in test runs of the draft.
    pub workflow_version: Option<i32>,
    pub vars: Map<String, Value>,
    pub trigger_node_id: String,
    /// The item the trigger gave.
    pub trigger: Map<String, Value>,
    /// Test runs only: side-effect nodes really write (D3).
    pub real_writes: bool,
    pub clock: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
}

impl RunFacts {
    pub fn now(&self) -> DateTime<Utc> {
        (self.clock)()
    }

    /// Facts for a test run, with the system clock.
    pub fn test(trigger_node_id: &str, trigger: Map<String, Value>) -> Self {
        RunFacts {
            run_id: uuid::Uuid::now_v7().to_string(),
            mode: RunMode::Test,
            started_at: Utc::now(),
            workflow_id: uuid::Uuid::now_v7().to_string(),
            workflow_name: "Test".into(),
            workflow_version: None,
            vars: Map::new(),
            trigger_node_id: trigger_node_id.into(),
            trigger,
            real_writes: false,
            clock: Arc::new(Utc::now),
        }
    }
}

pub struct Evaluator<'a> {
    engine: &'a ExprEngine,
    facts: &'a RunFacts,
    outputs: &'a BTreeMap<String, NodeOut>,
    /// Nodes whose data the params read. `true`: also their `items` list.
    needed: BTreeMap<String, bool>,
    all_nodes: bool,
}

impl<'a> Evaluator<'a> {
    /// `params` are the params of the node that will be evaluated; they tell which earlier nodes matter.
    pub fn new(engine: &'a ExprEngine, facts: &'a RunFacts, outputs: &'a BTreeMap<String, NodeOut>, params: &Map<String, Value>) -> Self {
        let mut needed: BTreeMap<String, bool> = BTreeMap::new();
        let mut all_nodes = false;
        let params = Value::Object(params.clone());
        for (_, text) in expression_strings(&params, "params") {
            for r in engine.references(text) {
                if r.root != "nodes" {
                    continue;
                }
                match r.node_id() {
                    Some(id) => {
                        let items = r.path.get(1).is_some_and(|p| p == "items");
                        let e = needed.entry(id.to_string()).or_insert(false);
                        *e = *e || items;
                    }
                    // `nodes[x]` or the whole `nodes` object
                    None => all_nodes = true,
                }
            }
        }
        Evaluator { engine, facts, outputs, needed, all_nodes }
    }

    pub fn facts(&self) -> &RunFacts {
        self.facts
    }

    fn wants(&self, id: &str) -> Option<bool> {
        if self.all_nodes { Some(true) } else { self.needed.get(id).copied() }
    }

    /// The output item of node `id` that `item` came from.
    fn paired<'o>(&self, item: &Item, id: &str, out: &'o NodeOut) -> Option<&'o Map<String, Value>> {
        if let Some((_, i)) = item.lineage.iter().rev().find(|(n, _)| n == id) {
            return out.items.get(*i).map(|it| &it.json);
        }
        None
    }

    /// The data an expression sees for this item.
    pub fn context(&self, item: &Item, index: usize) -> Value {
        let mut nodes = Map::new();
        for (id, out) in self.outputs {
            let Some(with_items) = self.wants(id) else { continue };
            let mut entry = Map::new();
            if with_items {
                entry.insert("items".into(), Value::Array(out.items.iter().map(|i| Value::Object(i.json.clone())).collect()));
            }
            let paired = self.paired(item, id, out).or(if *id == self.facts.trigger_node_id { Some(&self.facts.trigger) } else { None });
            if let Some(j) = paired {
                if let Some(err) = j.get("error") {
                    entry.insert("error".into(), err.clone());
                }
                entry.insert("json".into(), Value::Object(j.clone()));
            }
            nodes.insert(id.clone(), Value::Object(entry));
        }
        json!({
            "json": item.json,
            "trigger": self.facts.trigger,
            "nodes": nodes,
            "vars": self.facts.vars,
            "run": {
                "id": self.facts.run_id,
                "mode": self.facts.mode.as_str(),
                "started_at": self.facts.started_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                "item_index": index,
            },
            "workflow": { "id": self.facts.workflow_id, "name": self.facts.workflow_name, "version": self.facts.workflow_version },
        })
    }

    /// Evaluate a value for one item, with one more name in the data an expression sees (`page` for the stop condition
    /// of HTTP pagination). The earlier-node pairing is not checked: the value reads the extra name and `json`.
    pub fn value_with(&self, item: &Item, index: usize, v: &Value, name: &str, extra: Value) -> Result<Value, NodeError> {
        let mut ctx = self.context(item, index);
        if let Some(o) = ctx.as_object_mut() {
            o.insert(name.to_string(), extra);
        }
        self.engine.eval_params(v, &ctx).map_err(|e| NodeError::new(e.error.code, format!("{}: {}", e.path, e.error.message)))
    }

    /// Fail early, with a clear error, when the text reads `nodes.<id>.json` of a node this item did not come from.
    fn check_pairing(&self, item: &Item, text: &str) -> Result<(), NodeError> {
        if !has_expression(text) {
            return Ok(());
        }
        let unpaired: Vec<&str> = self
            .outputs
            .iter()
            .filter(|(id, out)| self.wants(id).is_some() && self.paired(item, id, out).is_none() && **id != self.facts.trigger_node_id)
            .map(|(id, _)| id.as_str())
            .collect();
        if unpaired.is_empty() {
            return Ok(());
        }
        for r in self.engine.references(text) {
            let Some(id) = r.node_id() else { continue };
            if !unpaired.contains(&id) || r.path.get(1).map(String::as_str) != Some("json") {
                continue;
            }
            let cut = item.lineage.iter().any(|(n, _)| n == LINEAGE_CUT);
            return Err(if cut {
                NodeError::new("ambiguous_reference", format!("`nodes.{id}.json` is not clear here: the items were joined (Aggregate or Merge). Use `nodes.{id}.items`."))
            } else {
                NodeError::new("no_paired_item", format!("This item did not come from the node `{id}`. Use `nodes.{id}.items`."))
            });
        }
        Ok(())
    }

    /// Evaluate a value for one item. Strings are read for `${{ }}`; lists and objects are walked.
    pub fn value(&self, item: &Item, index: usize, v: &Value) -> Result<Value, NodeError> {
        if !contains_expression(v) {
            return Ok(v.clone());
        }
        for (_, text) in expression_strings(v, "params") {
            self.check_pairing(item, text)?;
        }
        let ctx = self.context(item, index);
        self.engine.eval_params(v, &ctx).map_err(|e| NodeError::new(e.error.code, format!("{}: {}", e.path, e.error.message)))
    }
}

fn contains_expression(v: &Value) -> bool {
    match v {
        Value::String(s) => has_expression(s),
        Value::Array(a) => a.iter().any(contains_expression),
        Value::Object(m) => m.values().any(contains_expression),
        _ => false,
    }
}
