//! The node registry (build map section 7). Every node type has a `NodeSpec` (what it is) and
//! implements `NodeType` (what it does). Nodes that are not built yet are stubs: `run` answers
//! `not_implemented`. The specs are real: the validator, the catalog route, `schema.json` and
//! `workprocess.md` read them.
use super::engine::{Control, env::NodeEnv, eval::Evaluator};
use super::schema_kit::substitute_expressions;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Trigger,
    Aikido,
    Ai,
    Web,
    Flow,
    Data,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Trigger => "trigger",
            Category::Aikido => "aikido",
            Category::Ai => "ai",
            Category::Web => "web",
            Category::Flow => "flow",
            Category::Data => "data",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecMode {
    /// Runs once for each input item.
    PerItem,
    /// Runs once with the whole list of input items.
    AllItems,
}

pub struct NodeSpec {
    /// `http.request`
    pub type_id: &'static str,
    /// `HTTP Request`
    pub label: &'static str,
    pub category: Category,
    /// One or two sentences, for people and LLMs.
    pub description: &'static str,
    /// Clean JSON Schema of `params` (with `x-ui` hints).
    pub params_schema: Value,
    /// Output ports. Switch lists `case_1` to `case_20` and `default`; the validator checks the real count.
    pub ports: Vec<&'static str>,
    pub mode: ExecMode,
    /// `true`: dry in test runs unless "Real writes" is on (decision D3).
    pub side_effect: bool,
    /// Only an owner or admin may publish a workflow with this node (section 11.2).
    pub admin_only: bool,
    /// The output has fields that the example does not list (a web page body, a lead with custom fields).
    /// The validator then does not warn about a reference to an unknown field.
    pub open_output: bool,
    /// One output item. Shown in docs and used for autocomplete.
    pub output_example: Value,
    /// Tips for LLMs, short.
    pub docs_notes: &'static str,
}

impl NodeSpec {
    pub fn new(type_id: &'static str, label: &'static str, category: Category, description: &'static str) -> Self {
        NodeSpec {
            type_id,
            label,
            category,
            description,
            params_schema: json!({ "type": "object", "properties": {} }),
            ports: vec!["main"],
            mode: ExecMode::PerItem,
            side_effect: false,
            admin_only: false,
            open_output: false,
            output_example: json!({}),
            docs_notes: "",
        }
    }
    pub fn schema(mut self, s: Value) -> Self {
        self.params_schema = s;
        self
    }
    pub fn ports(mut self, p: &[&'static str]) -> Self {
        self.ports = p.to_vec();
        self
    }
    pub fn all_items(mut self) -> Self {
        self.mode = ExecMode::AllItems;
        self
    }
    pub fn side_effect(mut self) -> Self {
        self.side_effect = true;
        self
    }
    pub fn admin(mut self) -> Self {
        self.admin_only = true;
        self
    }
    pub fn open(mut self) -> Self {
        self.open_output = true;
        self
    }
    pub fn example(mut self, v: Value) -> Self {
        self.output_example = v;
        self
    }
    pub fn notes(mut self, n: &'static str) -> Self {
        self.docs_notes = n;
        self
    }
    pub fn is_trigger(&self) -> bool {
        self.category == Category::Trigger
    }

    /// The spec as the catalog route and `workprocess.md` show it.
    pub fn to_json(&self) -> Value {
        json!({
            "type": self.type_id,
            "label": self.label,
            "category": self.category.as_str(),
            "description": self.description,
            "params_schema": self.params_schema,
            "ports": self.ports,
            "mode": self.mode,
            "side_effect": self.side_effect,
            "admin_only": self.admin_only,
            "output_example": self.output_example,
            "docs_notes": self.docs_notes,
        })
    }
}

/// The node id that marks "the link to the nodes before is cut" in a lineage (Aggregate, Merge append).
pub const LINEAGE_CUT: &str = "*";

/// One piece of data that moves along an edge (section 5.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    /// Always an object.
    pub json: Map<String, Value>,
    /// Where the item came from, oldest first: (node id, index of the item in the output of that node).
    /// The engine adds the node that made the item; a node only decides what it inherits.
    pub lineage: Vec<(String, usize)>,
}

impl Item {
    pub fn new(json: Map<String, Value>) -> Self {
        Item { json, lineage: vec![] }
    }
    /// A new item that inherits the lineage of `self`.
    pub fn child(&self, json: Map<String, Value>) -> Self {
        Item { json, lineage: self.lineage.clone() }
    }
}

/// What a node may use while it runs.
pub struct NodeCtx<'a> {
    pub node_id: &'a str,
    /// The params as written in the workflow (expressions not evaluated).
    pub params: &'a Map<String, Value>,
    pub eval: &'a Evaluator<'a>,
    pub control: &'a Control,
    /// The services of the server. `None` in tests of pure nodes.
    pub env: Option<&'a NodeEnv>,
}

impl NodeCtx<'_> {
    /// The services of the server, for a node that talks to the outside.
    pub fn env(&self) -> Result<&NodeEnv, NodeError> {
        self.env.ok_or_else(|| NodeError::new("no_environment", "This node needs the server and can not run here."))
    }

    pub fn raw(&self, key: &str) -> Option<&Value> {
        self.params.get(key).filter(|v| !v.is_null())
    }

    /// Evaluate any value (strings are read for `${{ }}`, lists and objects are walked).
    pub fn value(&self, item: &Item, index: usize, v: &Value) -> Result<Value, NodeError> {
        self.eval.value(item, index, v)
    }

    /// Evaluate the param `key`. `None`: the param is missing.
    pub fn param(&self, item: &Item, index: usize, key: &str) -> Result<Option<Value>, NodeError> {
        match self.raw(key) {
            Some(v) => self.value(item, index, v).map(Some),
            None => Ok(None),
        }
    }

    fn typed(&self, item: &Item, index: usize, key: &str, schema: Value) -> Result<Option<Value>, NodeError> {
        let Some(v) = self.param(item, index, key)? else {
            return Ok(None);
        };
        super::expr::coerce_to_schema(&v, &schema).map(Some).map_err(|e| NodeError::new("invalid_param_value", format!("params.{key}: {e}")))
    }

    pub fn str_param(&self, item: &Item, index: usize, key: &str) -> Result<Option<String>, NodeError> {
        Ok(self.typed(item, index, key, json!({ "type": "string" }))?.and_then(|v| v.as_str().map(str::to_string)))
    }
    pub fn i64_param(&self, item: &Item, index: usize, key: &str) -> Result<Option<i64>, NodeError> {
        Ok(self.typed(item, index, key, json!({ "type": "integer" }))?.and_then(|v| v.as_i64()))
    }
    pub fn f64_param(&self, item: &Item, index: usize, key: &str) -> Result<Option<f64>, NodeError> {
        Ok(self.typed(item, index, key, json!({ "type": "number" }))?.and_then(|v| v.as_f64()))
    }
    pub fn bool_param(&self, item: &Item, index: usize, key: &str) -> Result<Option<bool>, NodeError> {
        Ok(self.typed(item, index, key, json!({ "type": "boolean" }))?.and_then(|v| v.as_bool()))
    }

    /// An empty item, for params that do not depend on the item.
    pub fn no_item() -> Item {
        Item::new(Map::new())
    }
}

#[derive(Clone)]
pub struct NodeInput {
    /// The items to work on. A per-item node gets exactly one.
    pub items: Vec<Item>,
    /// The items of each input edge, in edge order (Merge uses it). One entry for most nodes.
    pub inputs: Vec<Vec<Item>>,
    /// The position of `items[0]` among all the items of this node (`run.item_index`).
    pub first_index: usize,
}

impl NodeInput {
    pub fn one(items: Vec<Item>) -> Self {
        NodeInput { inputs: vec![items.clone()], items, first_index: 0 }
    }
}

/// The run must end after this node.
#[derive(Debug, Clone, PartialEq)]
pub struct StopRequest {
    pub message: String,
    pub error: bool,
}

/// The items a node gives, per output port.
#[derive(Default)]
pub struct NodeOutput {
    pub ports: BTreeMap<String, Vec<Item>>,
    /// The run must wait until this time before the next node (a long Wait node).
    pub wait_until: Option<DateTime<Utc>>,
    pub stop: Option<StopRequest>,
    /// Lines for the run log (event kind `log`). Redacted before they are stored.
    pub logs: Vec<String>,
}

impl NodeOutput {
    pub fn port(port: &str, items: Vec<Item>) -> Self {
        let mut ports = BTreeMap::new();
        ports.insert(port.to_string(), items);
        NodeOutput { ports, ..Default::default() }
    }
    pub fn main(items: Vec<Item>) -> Self {
        Self::port("main", items)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodeError {
    pub code: String,
    pub message: String,
    /// The HTTP status, when the error comes from a call.
    pub status: Option<u16>,
}

impl NodeError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        NodeError { code: code.into(), message: message.into(), status: None }
    }
    pub fn not_implemented(type_id: &str) -> Self {
        NodeError::new("not_implemented", format!("The node type {type_id} cannot run yet."))
    }
}

impl From<super::expr::ExprError> for NodeError {
    fn from(e: super::expr::ExprError) -> Self {
        NodeError::new(e.code, e.message)
    }
}

#[async_trait]
pub trait NodeType: Send + Sync {
    fn spec(&self) -> &NodeSpec;
    async fn run(&self, ctx: &NodeCtx<'_>, input: NodeInput) -> Result<NodeOutput, NodeError>;

    /// Does this node, with these params, change data? Then a test run does not call it (decision D3).
    /// The default is the flag of the spec. `aikido.api` decides by the method of the operation.
    fn side_effect(&self, _params: &Map<String, Value>) -> bool {
        self.spec().side_effect
    }

    /// How long one call of this node may take, when it is more than the engine default (an AI call with a repair).
    fn timeout(&self, _params: &Map<String, Value>) -> Option<std::time::Duration> {
        None
    }

    /// What a dry run shows for one item: the call the node would make, as `{ method, path, query?, body? }`.
    /// `None`: the generic answer (the type and the evaluated params).
    fn dry(&self, _ctx: &NodeCtx<'_>, _item: &Item, _index: usize) -> Option<Result<Value, NodeError>> {
        None
    }
}

/// A node with a spec and no code yet.
pub struct StubNode(pub NodeSpec);

#[async_trait]
impl NodeType for StubNode {
    fn spec(&self) -> &NodeSpec {
        &self.0
    }
    async fn run(&self, _ctx: &NodeCtx<'_>, _input: NodeInput) -> Result<NodeOutput, NodeError> {
        Err(NodeError::not_implemented(self.0.type_id))
    }
}

struct Entry {
    node: Box<dyn NodeType>,
    /// Checks `params` (expressions are swapped for sample values first).
    validator: jsonschema::Validator,
    schema: Value,
}

pub struct Registry {
    entries: BTreeMap<&'static str, Entry>,
}

static REGISTRY: LazyLock<Registry> = LazyLock::new(Registry::build);

impl Registry {
    pub fn global() -> &'static Registry {
        &REGISTRY
    }

    fn build() -> Registry {
        let mut entries = BTreeMap::new();
        for spec in super::catalog::all_specs() {
            let id = spec.type_id;
            let validator = jsonschema::validator_for(&spec.params_schema)
                .unwrap_or_else(|e| panic!("the params schema of {id} is not valid: {e}"));
            let schema = spec.params_schema.clone();
            let node: Box<dyn NodeType> = super::engine::nodes::make_node(spec);
            assert!(entries.insert(id, Entry { node, validator, schema }).is_none(), "node type {id} is registered twice");
        }
        Registry { entries }
    }

    pub fn get(&self, type_id: &str) -> Option<&dyn NodeType> {
        self.entries.get(type_id).map(|e| e.node.as_ref())
    }

    pub fn spec(&self, type_id: &str) -> Option<&NodeSpec> {
        self.get(type_id).map(|n| n.spec())
    }

    pub fn specs(&self) -> impl Iterator<Item = &NodeSpec> {
        self.entries.values().map(|e| e.node.spec())
    }

    pub fn type_ids(&self) -> Vec<&'static str> {
        self.entries.keys().copied().collect()
    }

    /// Check `params` against the schema of the type. Each error is (JSON pointer inside params, message).
    pub fn check_params(&self, type_id: &str, params: &Value) -> Vec<(String, String)> {
        let Some(e) = self.entries.get(type_id) else {
            return vec![];
        };
        let params = substitute_expressions(params, &e.schema);
        e.validator.iter_errors(&params).map(|err| (err.instance_path().to_string(), err.to_string())).collect()
    }

    /// The names closest to `word`, best first (at most `n`).
    pub fn closest(&self, word: &str, n: usize) -> Vec<&'static str> {
        closest(word, self.entries.keys().copied(), n)
    }
}

/// Edit distance between two words.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur.push((prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The candidates closest to `word`. A candidate that is a sub-string of the word (or the other way
/// round) counts as close. Far candidates are left out.
pub fn closest<'a, I: IntoIterator<Item = &'a str>>(word: &str, candidates: I, n: usize) -> Vec<&'a str> {
    let mut scored: Vec<(usize, &str)> = candidates
        .into_iter()
        .map(|c| {
            let mut d = levenshtein(word, c);
            if !word.is_empty() && (c.contains(word) || word.contains(c)) {
                d = d.min(1);
            }
            (d, c)
        })
        .filter(|(d, c)| *d <= (word.len().max(c.len()) / 2).max(2))
        .collect();
    scored.sort();
    scored.into_iter().take(n).map(|(_, c)| c).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_distance() {
        assert_eq!(levenshtein("fetch_sit", "fetch_site"), 1);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("same", "same"), 0);
    }

    #[test]
    fn closest_names() {
        let ids = ["fetch_site", "write_email", "save_email"];
        assert_eq!(closest("fetch_sit", ids, 3), vec!["fetch_site"]);
        assert_eq!(closest("save_emial", ids, 1), vec!["save_email"]);
        assert!(closest("zzzzzzzz", ids, 3).is_empty());
    }
}
