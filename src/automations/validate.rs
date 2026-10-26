//! The validator (build map sections 4.2 to 4.8). It answers with every problem at once, in the same
//! shape for a save, a publish, an import and `POST /v1/automations/validate`.
//!
//! The input is raw JSON, because a draft may be broken while a person is still building it.
//! A draft save is allowed with errors. Publish, activate and test runs need `ok: true`.
use super::definition::*;
use super::expr::{ExprEngine, ROOT_NAMES, Reference, expression_strings};
use super::registry::{Registry, closest};
use super::schema_kit::has_expression;
use serde::Serialize;
use utoipa::ToSchema;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A draft save: a missing variable value or secret is not an error yet.
    Draft,
    /// Publish and activate: everything must be right.
    Publish,
}

pub struct Options<'a> {
    pub mode: Mode,
    /// The AI provider names of this install. `None`: do not check `unknown_provider`.
    pub providers: Option<&'a HashSet<String>>,
    /// The secret names of this install. `None`: do not check `unknown_secret`.
    pub secrets: Option<&'a HashSet<String>>,
    /// An owner or admin. A member may not publish a workflow with an admin-only node.
    pub is_admin: bool,
    /// The operations of the API, for the `aikido.api` node. `None`: only the text of the operation is checked.
    pub operations: Option<&'a crate::automations::internal_call::OperationIndex>,
    /// The owner turned on `allow_lead_erase`.
    pub allow_lead_erase: bool,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Options { mode: Mode::Draft, providers: None, secrets: None, is_admin: true, operations: None, allow_lead_erase: false }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Issue {
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct ValidationResult {
    pub ok: bool,
    pub errors: Vec<Issue>,
    pub warnings: Vec<Issue>,
}

impl ValidationResult {
    pub fn has_code(&self, code: &str) -> bool {
        self.errors.iter().chain(&self.warnings).any(|i| i.code == code)
    }
}

#[derive(Default)]
struct Sink {
    errors: Vec<Issue>,
    warnings: Vec<Issue>,
}

impl Sink {
    fn err(&mut self, code: &str, node_id: Option<&str>, path: Option<String>, message: impl Into<String>) {
        self.errors.push(Issue { code: code.into(), node_id: node_id.map(str::to_string), path, message: message.into() });
    }
    fn warn(&mut self, code: &str, node_id: Option<&str>, path: Option<String>, message: impl Into<String>) {
        self.warnings.push(Issue { code: code.into(), node_id: node_id.map(str::to_string), path, message: message.into() });
    }
}

/// Validate a raw definition.
pub fn validate_value(raw: &Value, opts: &Options) -> ValidationResult {
    let mut sink = Sink::default();
    if let Some(def) = parse(raw, &mut sink) {
        check(&def, opts, &mut sink);
    }
    ValidationResult { ok: sink.errors.is_empty(), errors: sink.errors, warnings: sink.warnings }
}

/// Validate a definition that is already typed.
pub fn validate(def: &WorkflowDefinition, opts: &Options) -> ValidationResult {
    validate_value(&def.to_value(), opts)
}

/// Read the raw JSON into a definition, part by part, so each problem has a place.
/// `None` when the file is too broken to go on.
fn parse(raw: &Value, sink: &mut Sink) -> Option<WorkflowDefinition> {
    let Some(top) = raw.as_object() else {
        sink.err("invalid_format", None, None, "The workflow must be one JSON object.");
        return None;
    };
    for key in top.keys() {
        if !TOP_LEVEL_KEYS.contains(&key.as_str()) {
            sink.warn("unknown_field_ignored", None, Some(key.clone()), format!("The top-level field `{key}` is not part of the format. It is ignored."));
        }
    }
    match top.get("format").and_then(Value::as_str) {
        Some(FORMAT) => {}
        Some(other) => sink.err("invalid_format", None, Some("format".into()), format!("`format` must be \"{FORMAT}\", not \"{other}\".")),
        None => sink.err("invalid_format", None, Some("format".into()), format!("`format` is missing. It must be \"{FORMAT}\".")),
    }
    let version = match top.get("version").and_then(Value::as_u64) {
        Some(v) if v > u64::from(FORMAT_VERSION) => {
            sink.err("format_too_new", None, Some("version".into()), format!("This file is format version {v}. This server reads up to version {FORMAT_VERSION}. Update the server."));
            return None;
        }
        Some(v) => v as u32,
        None => {
            sink.err("invalid_format", None, Some("version".into()), "`version` is missing. It must be a whole number (1).");
            FORMAT_VERSION
        }
    };
    let name = match top.get("name").and_then(Value::as_str) {
        Some(n) if (1..=120).contains(&n.trim().chars().count()) => n.to_string(),
        Some(_) => {
            sink.err("invalid_name", None, Some("name".into()), "`name` must be 1 to 120 characters.");
            String::new()
        }
        None => {
            sink.err("invalid_name", None, Some("name".into()), "`name` is missing. It must be 1 to 120 characters.");
            String::new()
        }
    };
    let description = match top.get("description") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) if s.chars().count() <= 2000 => s.clone(),
        Some(_) => {
            sink.err("invalid_format", None, Some("description".into()), "`description` must be text of up to 2000 characters.");
            String::new()
        }
    };

    let nodes: Vec<Node> = parse_list(top, "nodes", true, sink, |v| v.get("id").and_then(Value::as_str).map(str::to_string));
    let edges: Vec<Edge> = parse_list(top, "edges", true, sink, |_| None);
    let variables: Vec<Variable> = parse_list(top, "variables", false, sink, |_| None);
    let notes: Vec<StickyNote> = parse_list(top, "notes", false, sink, |_| None);
    let settings = match top.get("settings") {
        None | Some(Value::Null) => Settings::default(),
        Some(v) => serde_json::from_value(v.clone()).unwrap_or_else(|e| {
            sink.err("invalid_format", None, Some("settings".into()), format!("settings: {e}"));
            Settings::default()
        }),
    };
    let pins = match top.get("pins") {
        None | Some(Value::Null) => BTreeMap::new(),
        Some(v) => serde_json::from_value(v.clone()).unwrap_or_else(|e| {
            sink.err("invalid_format", None, Some("pins".into()), format!("pins: {e}. Use {{ \"<node_id>\": [ {{item json}}, ... ] }}."));
            BTreeMap::new()
        }),
    };
    let meta = top.get("meta").and_then(Value::as_object).cloned();
    Some(WorkflowDefinition { format: FORMAT.into(), version, name, description, variables, settings, nodes, edges, notes, pins, meta })
}

fn parse_list<T: serde::de::DeserializeOwned>(top: &Map<String, Value>, key: &str, required: bool, sink: &mut Sink, id_of: impl Fn(&Value) -> Option<String>) -> Vec<T> {
    let Some(v) = top.get(key).filter(|v| !v.is_null()) else {
        if required {
            sink.err("invalid_format", None, Some(key.into()), format!("`{key}` is missing. It must be a list."));
        }
        return vec![];
    };
    let Some(list) = v.as_array() else {
        sink.err("invalid_format", None, Some(key.into()), format!("`{key}` must be a list."));
        return vec![];
    };
    let mut out = Vec::new();
    for (i, item) in list.iter().enumerate() {
        match serde_json::from_value::<T>(item.clone()) {
            Ok(t) => out.push(t),
            Err(e) => sink.err("invalid_format", id_of(item).as_deref(), Some(format!("{key}[{i}]")), format!("{key}[{i}]: {e}")),
        }
    }
    out
}

fn path_of(pointer: &str) -> String {
    // `/conditions/0/op` -> `params.conditions[0].op`
    let mut out = String::from("params");
    for seg in pointer.split('/').filter(|s| !s.is_empty()) {
        if seg.chars().all(|c| c.is_ascii_digit()) {
            out.push_str(&format!("[{seg}]"));
        } else {
            out.push('.');
            out.push_str(seg);
        }
    }
    out
}

fn check(def: &WorkflowDefinition, opts: &Options, sink: &mut Sink) {
    let reg = Registry::global();
    let engine = ExprEngine::new();

    if def.nodes.len() > MAX_NODES {
        sink.err("too_many_nodes", None, Some("nodes".into()), format!("A workflow can have up to {MAX_NODES} nodes. This one has {}.", def.nodes.len()));
    }
    if def.notes.len() > MAX_NOTES {
        sink.err("invalid_format", None, Some("notes".into()), format!("A workflow can have up to {MAX_NOTES} notes."));
    }
    check_settings(def, sink);
    let var_keys = check_variables(def, opts, sink);

    // ---- nodes: ids and types
    let mut seen: HashSet<&str> = HashSet::new();
    for (i, n) in def.nodes.iter().enumerate() {
        let id = n.id.as_str();
        let at = Some(format!("nodes[{i}].id"));
        if !is_slug(id) {
            sink.err("invalid_node_id", Some(id), at.clone(), format!("`{id}` is not a valid node id. Use lower-case letters, digits and _, start with a letter, up to 40 characters (for example `fetch_site`)."));
        } else if RESERVED_IDS.contains(&id) {
            sink.err("reserved_node_id", Some(id), at.clone(), format!("`{id}` is a reserved word. Pick another id."));
        }
        if !seen.insert(id) {
            sink.err("duplicate_node_id", Some(id), at, format!("Two nodes have the id `{id}`."));
        }
        if let Some(name) = &n.name {
            if name.chars().count() > 80 {
                sink.err("invalid_format", Some(id), Some(format!("nodes[{i}].name")), "A node name can have up to 80 characters.");
            }
        }
        if n.notes.as_ref().is_some_and(|t| t.chars().count() > 500) {
            sink.err("invalid_format", Some(id), Some(format!("nodes[{i}].notes")), "Node notes can have up to 500 characters.");
        }
        if let Some(r) = n.retry {
            if r.max > 5 || !(100..=60_000).contains(&r.delay_ms) {
                sink.err("invalid_format", Some(id), Some(format!("nodes[{i}].retry")), "`retry.max` is 0 to 5 and `retry.delay_ms` is 100 to 60000.");
            }
        }
        if let Some(b) = n.batch {
            if !(1..=100).contains(&b.size) || b.delay_ms > 600_000 {
                sink.err("invalid_format", Some(id), Some(format!("nodes[{i}].batch")), "`batch.size` is 1 to 100 and `batch.delay_ms` is 0 to 600000.");
            }
        }
        match reg.spec(&n.type_id) {
            None => {
                let near = reg.closest(&n.type_id, 3);
                let hint = if near.is_empty() { String::new() } else { format!(" Did you mean {}?", near.iter().map(|t| format!("`{t}`")).collect::<Vec<_>>().join(", ")) };
                sink.err("unknown_node_type", Some(id), Some(format!("nodes[{i}].type")), format!("`{}` is not a node type.{hint}", n.type_id));
            }
            Some(spec) => {
                if let Some(b) = n.batch {
                    if spec.mode == super::registry::ExecMode::AllItems && b.size > 1 {
                        sink.err("invalid_format", Some(id), Some(format!("nodes[{i}].batch")), format!("`batch` works only on nodes that run once per item. {} runs once for all items.", spec.label));
                    }
                }
            }
        }
    }

    let by_id: HashMap<&str, &Node> = def.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let is_trigger = |n: &Node| reg.spec(&n.type_id).is_some_and(|s| s.is_trigger());

    // ---- triggers
    let triggers: Vec<&Node> = def.nodes.iter().filter(|n| is_trigger(n)).collect();
    if triggers.is_empty() {
        sink.err("missing_trigger", None, Some("nodes".into()), "A workflow needs at least one trigger node (for example `trigger.manual`).");
    }
    if triggers.len() > MAX_TRIGGERS {
        sink.err("too_many_triggers", None, Some("nodes".into()), format!("A workflow can have up to {MAX_TRIGGERS} triggers. This one has {}.", triggers.len()));
    }

    // ---- edges
    let mut edge_set: HashSet<(&str, &str, &str)> = HashSet::new();
    let mut good_edges: Vec<&Edge> = Vec::new();
    if def.edges.is_empty() && def.nodes.len() > 1 {
        sink.warn("unreachable_node", None, Some("edges".into()), "The workflow has several nodes and no edges. Only the triggers will run.");
    }
    for (i, e) in def.edges.iter().enumerate() {
        let at = |f: &str| Some(format!("edges[{i}].{f}"));
        let from = by_id.get(e.from.as_str());
        let to = by_id.get(e.to.as_str());
        if from.is_none() {
            sink.err("unknown_edge_node", Some(&e.from), at("from"), format!("The edge starts at `{}`, which is not a node.{}", e.from, did_you_mean(&e.from, by_id.keys().copied())));
        }
        if to.is_none() {
            sink.err("unknown_edge_node", Some(&e.to), at("to"), format!("The edge ends at `{}`, which is not a node.{}", e.to, did_you_mean(&e.to, by_id.keys().copied())));
        }
        let (Some(from), Some(to)) = (from, to) else { continue };
        if is_trigger(to) {
            sink.err("trigger_has_input", Some(&e.to), at("to"), format!("`{}` is a trigger. A trigger starts a run and cannot have an incoming edge.", e.to));
            continue;
        }
        if let Some(spec) = reg.spec(&from.type_id) {
            let port = e.port();
            if !port_ok(spec.type_id, &spec.ports, from, port) {
                let allowed = allowed_ports(&spec.ports, from);
                sink.err(
                    "unknown_port",
                    Some(&e.from),
                    at("port"),
                    format!("`{}` ({}) has no output port `{port}`. {}", e.from, spec.label, if allowed.is_empty() { "It has no output.".to_string() } else { format!("Ports: {}.", allowed.join(", ")) }),
                );
                continue;
            }
        }
        if !edge_set.insert((&e.from, e.port(), &e.to)) {
            sink.err("duplicate_edge", Some(&e.from), Some(format!("edges[{i}]")), format!("The edge {} -> {} is listed twice.", e.from, e.to));
            continue;
        }
        good_edges.push(e);
    }

    // ---- graph: cycles, reachability, many inputs
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut parents: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in &good_edges {
        children.entry(&e.from).or_default().push(&e.to);
        parents.entry(&e.to).or_default().push(&e.from);
    }
    if let Some(cycle) = find_cycle(&def.nodes, &children) {
        let id = cycle.first().cloned().unwrap_or_default();
        sink.err("cycle_detected", Some(&id), Some("edges".into()), format!("The edges make a loop: {}. A workflow cannot go back. To handle many things, use items (see Split out).", cycle.join(" -> ")));
    }
    let reachable = reach(triggers.iter().map(|n| n.id.as_str()), &children);
    for n in &def.nodes {
        if !is_trigger(n) && !reachable.contains(n.id.as_str()) {
            sink.warn("unreachable_node", Some(&n.id), None, format!("No trigger reaches `{}`. It will never run.", n.id));
        }
        if parents.get(n.id.as_str()).map_or(0, |p| p.len()) >= 2 && n.type_id != "flow.merge" {
            sink.warn("multiple_inputs", Some(&n.id), None, format!("Node `{}` has 2 or more inputs and will run once for each. Use a Merge node if you need both.", n.id));
        }
    }

    // ---- params and expressions
    let upstream = |id: &str| ancestors(id, &parents);
    let mut used_secrets: BTreeSet<(String, String)> = BTreeSet::new();
    for n in &def.nodes {
        let Some(spec) = reg.spec(&n.type_id) else { continue };
        let id = n.id.as_str();
        let params = Value::Object(n.params.clone());
        for (ptr, msg) in reg.check_params(&n.type_id, &params) {
            sink.err("invalid_params", Some(id), Some(path_of(&ptr)), format!("{}: {msg}", spec.label));
        }
        if let Some(props) = spec.params_schema.get("properties").and_then(Value::as_object) {
            for key in n.params.keys() {
                if !props.contains_key(key) {
                    sink.warn("unknown_field_ignored", Some(id), Some(format!("params.{key}")), format!("{} has no param `{key}`. It is ignored.", spec.label));
                }
            }
        }
        if let Some(Value::String(p)) = n.params.get("provider").filter(|_| n.type_id == "ai.agent") {
            // a provider id works too (it is turned into the name on export)
            let is_id = uuid::Uuid::parse_str(p).is_ok();
            if !has_expression(p) && !is_id && !["default_draft", "default_slot"].contains(&p.as_str()) && opts.providers.is_some_and(|set| !set.iter().any(|s| s.to_lowercase() == p.to_lowercase())) {
                sink.err("unknown_provider", Some(id), Some("params.provider".into()), format!("There is no AI provider named `{p}`. Use default_draft, default_slot, or a name from Settings > AI."));
            }
        }
        if n.type_id == "ai.agent" {
            let out = n.params.get("output");
            if out.and_then(|o| o.get("format")).and_then(Value::as_str) == Some("json") {
                if let Err(e) = crate::automations::engine::ai_nodes::parse_fields(out.and_then(|o| o.get("fields"))) {
                    sink.err("invalid_params", Some(id), Some("params.output.fields".into()), e.message);
                }
            }
        }
        if n.type_id == "ai.agent" {
            if let Err(e) = crate::automations::engine::ai_tools::parse_tools(n.params.get("tools")) {
                sink.err("invalid_params", Some(id), Some("params.tools".into()), e.message);
            }
        }
        if n.type_id == "http.request" {
            if let Err(e) = crate::automations::engine::web_nodes::check_pagination(&n.params) {
                sink.err("invalid_params", Some(id), Some("params.pagination".into()), e.message);
            }
        }
        if n.type_id == "flow.execute_workflow" {
            if let Some(w) = n.params.get("workflow_id").and_then(Value::as_str).filter(|w| !has_expression(w)) {
                if uuid::Uuid::parse_str(w.trim()).is_err() {
                    sink.err("invalid_params", Some(id), Some("params.workflow_id".into()), format!("`{w}` is not a workflow id. Use the id from the workflow list."));
                }
            }
        }
        if n.type_id == "code.js" {
            if let Some(code) = n.params.get("code").and_then(Value::as_str).filter(|c| !c.trim().is_empty()) {
                if let Err(e) = crate::automations::engine::code_node::check_syntax(code) {
                    sink.err("code_syntax", Some(id), Some("params.code".into()), e.message);
                }
            }
        }
        // names of secrets in plain params (the `auth` block and the webhook header)
        if let Some(name) = n.params.get("secret").and_then(Value::as_str).filter(|s| !has_expression(s)) {
            used_secrets.insert((name.to_string(), id.to_string()));
        }
        if let Some(name) = n.params.get("auth").and_then(|a| a.get("secret")).and_then(Value::as_str).filter(|s| !has_expression(s)) {
            used_secrets.insert((name.to_string(), id.to_string()));
        }
        if n.type_id == "aikido.api" {
            check_api_operation(n, opts, sink);
        }
        if spec.admin_only && !opts.is_admin && opts.mode == Mode::Publish {
            sink.err("admin_node_not_allowed", Some(id), None, format!("{} can be published only by an owner or an admin.", spec.label));
        }

        let up = upstream(id);
        let triggers_example_keys = trigger_keys(&triggers, reg);
        for (path, text) in expression_strings(&params, "params") {
            // JavaScript is not an expression: a `${{` in a template string of the code is code
            if n.type_id == "code.js" && path == "params.code" {
                continue;
            }
            if let Err(e) = engine.check(text) {
                sink.err("expression_syntax", Some(id), Some(path.clone()), e.message);
                continue;
            }
            for name in ExprEngine::secret_names(text) {
                used_secrets.insert((name, id.to_string()));
            }
            for r in engine.references(text) {
                check_reference(&r, n, &path, spec.is_trigger(), &RefEnv { def, by_id: &by_id, up: &up, var_keys: &var_keys, trigger_keys: &triggers_example_keys, parents: &parents, children: &children }, sink);
            }
        }
    }

    // ---- secrets
    if let Some(known) = opts.secrets {
        for (name, node) in &used_secrets {
            if !known.contains(name) {
                let msg = format!("The secret `{name}` does not exist. Add it in Automations > Secrets.");
                if opts.mode == Mode::Publish {
                    sink.err("unknown_secret", Some(node), None, msg);
                } else {
                    sink.warn("unknown_secret", Some(node), None, msg);
                }
            }
        }
    }

    // ---- pins and positions
    for id in def.pins.keys() {
        if !by_id.contains_key(id.as_str()) {
            sink.warn("unknown_field_ignored", None, Some(format!("pins.{id}")), format!("There is a pin for `{id}`, which is not a node. It is ignored."));
        }
    }
    if !def.pins.is_empty() {
        sink.warn("pinned_data_in_production", None, Some("pins".into()), "Pins are used in test runs only. Production runs ignore them.");
    }
    if def.nodes.len() > 1 && def.nodes.iter().all(|n| n.position.is_none()) {
        sink.warn("no_positions", None, None, "No node has a position. The editor will lay the nodes out from left to right.");
    }
}

fn did_you_mean<'a>(word: &str, candidates: impl Iterator<Item = &'a str>) -> String {
    let near = closest(word, candidates, 3);
    if near.is_empty() { String::new() } else { format!(" Did you mean {}?", near.iter().map(|t| format!("`{t}`")).collect::<Vec<_>>().join(", ")) }
}

// ---------- ports ----------

fn switch_cases(n: &Node) -> Option<usize> {
    n.params.get("cases").and_then(Value::as_array).map(Vec::len)
}

fn allowed_ports(ports: &[&'static str], n: &Node) -> Vec<String> {
    let mut out: Vec<String> = if n.type_id == "flow.switch" {
        let count = switch_cases(n).unwrap_or(20);
        (1..=count.min(20)).map(|i| format!("case_{i}")).chain(std::iter::once("default".to_string())).collect()
    } else {
        ports.iter().map(|p| p.to_string()).collect()
    };
    if n.on_error == Some(OnError::ErrorPort) {
        out.push("error".into());
    }
    out
}

fn port_ok(_type_id: &str, ports: &[&'static str], n: &Node, port: &str) -> bool {
    allowed_ports(ports, n).iter().any(|p| p == port)
}

// ---------- graph helpers ----------

fn find_cycle(nodes: &[Node], children: &HashMap<&str, Vec<&str>>) -> Option<Vec<String>> {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Open,
        Done,
    }
    fn visit<'a>(id: &'a str, children: &HashMap<&'a str, Vec<&'a str>>, marks: &mut HashMap<&'a str, Mark>, stack: &mut Vec<&'a str>) -> Option<Vec<String>> {
        match marks.get(id) {
            Some(Mark::Done) => return None,
            Some(Mark::Open) => {
                let start = stack.iter().position(|s| *s == id).unwrap_or(0);
                let mut cycle: Vec<String> = stack[start..].iter().map(|s| s.to_string()).collect();
                cycle.push(id.to_string());
                return Some(cycle);
            }
            None => {}
        }
        marks.insert(id, Mark::Open);
        stack.push(id);
        for &c in children.get(id).into_iter().flatten() {
            if let Some(found) = visit(c, children, marks, stack) {
                return Some(found);
            }
        }
        stack.pop();
        marks.insert(id, Mark::Done);
        None
    }
    let mut marks = HashMap::new();
    for n in nodes {
        if let Some(c) = visit(&n.id, children, &mut marks, &mut Vec::new()) {
            return Some(c);
        }
    }
    None
}

fn reach<'a>(starts: impl Iterator<Item = &'a str>, children: &HashMap<&'a str, Vec<&'a str>>) -> HashSet<&'a str> {
    let mut seen = HashSet::new();
    let mut stack: Vec<&str> = starts.collect();
    while let Some(id) = stack.pop() {
        if seen.insert(id) {
            stack.extend(children.get(id).into_iter().flatten().copied());
        }
    }
    seen
}

/// Every node that has a path to `id`.
fn ancestors<'a>(id: &str, parents: &HashMap<&'a str, Vec<&'a str>>) -> HashSet<&'a str> {
    let mut seen = HashSet::new();
    let mut stack: Vec<&str> = parents.get(id).into_iter().flatten().copied().collect();
    while let Some(p) = stack.pop() {
        if seen.insert(p) {
            stack.extend(parents.get(p).into_iter().flatten().copied());
        }
    }
    seen
}

// ---------- references ----------

struct RefEnv<'a> {
    def: &'a WorkflowDefinition,
    by_id: &'a HashMap<&'a str, &'a Node>,
    up: &'a HashSet<&'a str>,
    var_keys: &'a HashSet<String>,
    trigger_keys: &'a Option<BTreeSet<String>>,
    parents: &'a HashMap<&'a str, Vec<&'a str>>,
    children: &'a HashMap<&'a str, Vec<&'a str>>,
}

/// The first-level keys that the triggers of the workflow give. `None` when there is no known trigger.
fn trigger_keys(triggers: &[&Node], reg: &Registry) -> Option<BTreeSet<String>> {
    let mut keys = BTreeSet::new();
    let mut any = false;
    for t in triggers {
        if let Some(spec) = reg.spec(&t.type_id) {
            any = true;
            keys.extend(spec.output_example.as_object().into_iter().flat_map(|m| m.keys().cloned()));
        }
    }
    any.then_some(keys)
}

fn check_reference(r: &Reference, node: &Node, path: &str, in_trigger: bool, env: &RefEnv, sink: &mut Sink) {
    let id = Some(node.id.as_str());
    let reg = Registry::global();
    if in_trigger && r.root != "vars" {
        sink.err("trigger_param_not_static", id, Some(path.to_string()), format!("A trigger can read only `vars.*` in its params (there is no data yet). `{}` is not allowed.", r.full()));
        return;
    }
    // the stop condition of HTTP pagination reads the page that was just read
    if r.root == "page" && node.type_id == "http.request" && path == "params.pagination.stop_when" {
        return;
    }
    if !ROOT_NAMES.contains(&r.root.as_str()) {
        sink.err("unknown_reference", id, Some(path.to_string()), format!("`{}` is not something an expression can read. Use json, trigger, nodes, vars, run or workflow.", r.root));
        return;
    }
    match r.root.as_str() {
        "vars" => {
            if let Some(key) = r.var_key() {
                if !env.var_keys.contains(key) {
                    sink.err("unknown_variable", id, Some(path.to_string()), format!("`vars.{key}` is not a variable of this workflow.{}", did_you_mean(key, env.var_keys.iter().map(String::as_str))));
                }
            }
        }
        "trigger" => {
            if let (Some(first), Some(keys)) = (r.path.first(), env.trigger_keys) {
                if !keys.contains(first) {
                    sink.warn("expression_maybe_empty", id, Some(path.to_string()), format!("`trigger.{first}` is not a field of the trigger item. The trigger gives: {}.", keys.iter().cloned().collect::<Vec<_>>().join(", ")));
                }
            }
        }
        "nodes" => {
            let Some(target) = r.node_id() else { return };
            let all_ids = env.by_id.keys().copied();
            if target == node.id {
                sink.err("unknown_reference", id, Some(path.to_string()), format!("`nodes.{target}` is the node itself. A node cannot read its own output."));
                return;
            }
            let Some(target_node) = env.by_id.get(target) else {
                // a typo: suggest the close node ids that are upstream first
                let up_ids = env.up.iter().copied();
                let mut hint = did_you_mean(target, up_ids);
                if hint.is_empty() {
                    hint = did_you_mean(target, all_ids);
                }
                sink.err("unknown_reference", id, Some(path.to_string()), format!("nodes.{target} is not a node.{hint}"));
                return;
            };
            if !env.up.contains(target) {
                sink.err(
                    "unknown_reference",
                    id,
                    Some(path.to_string()),
                    format!("nodes.{target} is not a node upstream of {}. A node can read only the nodes that run before it on its path.", node.id),
                );
                return;
            }
            match r.path.get(1).map(String::as_str) {
                None | Some("json" | "items" | "error") => {}
                Some(other) => {
                    sink.err("unknown_reference", id, Some(path.to_string()), format!("nodes.{target}.{other} does not exist. Use nodes.{target}.json, nodes.{target}.items or nodes.{target}.error."));
                    return;
                }
            }
            if r.path.get(1).map(String::as_str) == Some("json") {
                // after Aggregate or Merge the per-item link to earlier nodes is lost
                if let Some(blocker) = lineage_breaker(target, &node.id, env) {
                    sink.warn(
                        "ambiguous_reference",
                        id,
                        Some(path.to_string()),
                        format!("`{blocker}` joins items between {target} and {}. nodes.{target}.json may not point to one item here; use nodes.{target}.items.", node.id),
                    );
                }
                if let (Some(field), Some(spec)) = (r.path.get(2), reg.spec(&target_node.type_id)) {
                    if let Some(keys) = known_output_keys(target_node, spec) {
                        if !keys.contains(field) {
                            sink.warn(
                                "expression_maybe_empty",
                                id,
                                Some(path.to_string()),
                                format!("`{}` has no output field `{field}`. It gives: {}.", target, keys.iter().cloned().collect::<Vec<_>>().join(", ")),
                            );
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

/// The output fields that a node is known to give, or `None` when the node's output is open.
fn known_output_keys(node: &Node, spec: &super::registry::NodeSpec) -> Option<BTreeSet<String>> {
    if node.type_id == "ai.agent" {
        let out = node.params.get("output");
        if out.and_then(|o| o.get("format")).and_then(Value::as_str) == Some("json") {
            let fields = out?.get("fields")?.as_array()?;
            return Some(fields.iter().filter_map(|f| f.get("key").and_then(Value::as_str)).map(str::to_string).collect());
        }
        return Some(BTreeSet::from(["text".to_string()]));
    }
    if spec.open_output {
        return None;
    }
    spec.output_example.as_object().map(|m| m.keys().cloned().collect())
}

/// An Aggregate or Merge node that sits on a path from `from` to `to`.
fn lineage_breaker<'a>(from: &str, to: &str, env: &RefEnv<'a>) -> Option<&'a str> {
    let below = reach(std::iter::once(from), env.children);
    let above = ancestors(to, env.parents);
    env.def
        .nodes
        .iter()
        .find(|n| matches!(n.type_id.as_str(), "data.aggregate" | "flow.merge") && below.contains(n.id.as_str()) && n.id != from && above.contains(n.id.as_str()))
        .map(|n| n.id.as_str())
}

// ---------- variables and settings ----------

fn check_variables(def: &WorkflowDefinition, opts: &Options, sink: &mut Sink) -> HashSet<String> {
    let mut keys = HashSet::new();
    if def.variables.len() > MAX_VARIABLES {
        sink.err("invalid_format", None, Some("variables".into()), format!("A workflow can have up to {MAX_VARIABLES} variables."));
    }
    for (i, v) in def.variables.iter().enumerate() {
        let at = Some(format!("variables[{i}].key"));
        if !is_slug(&v.key) {
            sink.err("invalid_format", None, at, format!("`{}` is not a valid variable key. Use lower-case letters, digits and _, start with a letter, up to 40 characters.", v.key));
        } else if !keys.insert(v.key.clone()) {
            sink.err("invalid_format", None, at, format!("Two variables have the key `{}`.", v.key));
        }
        let empty = v.value.as_ref().is_none_or(|x| x.is_null() || x.as_str() == Some(""));
        if v.required && empty && opts.mode == Mode::Publish {
            sink.err("variable_missing", None, Some(format!("variables[{i}].value")), format!("The variable `{}` is required and has no value. Set it before you publish.", v.key));
        }
    }
    keys
}

fn check_settings(def: &WorkflowDefinition, sink: &mut Sink) {
    let s = &def.settings;
    let mut bad = |field: &str, rule: &str| sink.err("invalid_format", None, Some(format!("settings.{field}")), format!("`settings.{field}` must be {rule}."));
    if let Some(tz) = &s.timezone {
        if tz.parse::<chrono_tz::Tz>().is_err() {
            bad("timezone", "an IANA time zone name, like Asia/Dhaka");
        }
    }
    if s.on_error == Some(OnError::ErrorPort) {
        bad("on_error", "stop or continue");
    }
    if s.max_concurrent_runs.is_some_and(|v| !(1..=50).contains(&v)) {
        bad("max_concurrent_runs", "1 to 50");
    }
    if s.max_runs_per_hour.is_some_and(|v| !(1..=100_000).contains(&v)) {
        bad("max_runs_per_hour", "1 to 100000");
    }
    if s.run_timeout_secs.is_some_and(|v| !(60..=86_400).contains(&v)) {
        bad("run_timeout_secs", "60 to 86400");
    }
    if s.error_workflow_id.as_deref().is_some_and(|v| !v.trim().is_empty() && uuid::Uuid::parse_str(v.trim()).is_err()) {
        bad("error_workflow_id", "the id of a workflow (or empty)");
    }
}

/// `aikido.api`: a plain-text operation must exist and must not be on the deny list (build map 7.2).
fn check_api_operation(n: &Node, opts: &Options, sink: &mut Sink) {
    use crate::automations::internal_call::deny_reason;
    let Some(op) = n.params.get("operation").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty() && !has_expression(s)) else { return };
    let (method, path) = match opts.operations {
        Some(ops) => match ops.find(op) {
            Some(o) => (o.method.clone(), o.path.clone()),
            None => {
                let names: Vec<&str> = ops.all().map(|o| o.id.as_str()).collect();
                let near = closest(op, names, 3);
                let hint = if near.is_empty() { String::new() } else { format!(" Did you mean {}?", near.iter().map(|x| format!("`{x}`")).collect::<Vec<_>>().join(", ")) };
                sink.err("unknown_operation", Some(&n.id), Some("params.operation".into()), format!("There is no API operation `{op}`.{hint}"));
                return;
            }
        },
        None => match op.split_once(' ') {
            Some((m, p)) => (m.to_ascii_uppercase(), p.trim().to_string()),
            None => return,
        },
    };
    if let Some(why) = deny_reason(&method, &path, opts.allow_lead_erase) {
        sink.err("endpoint_not_allowed", Some(&n.id), Some("params.operation".into()), format!("{method} {path}: {why}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The example of section 4.1, with the campaign set.
    fn example() -> Value {
        json!({
            "format": "aikido-workflow",
            "version": 1,
            "name": "Research lead and write first email",
            "description": "When a lead joins the campaign, read its site, ask AI for a first email, save it.",
            "variables": [{ "key": "campaign_id", "label": "Campaign", "kind": "campaign", "value": null, "required": true }],
            "settings": { "timezone": "UTC", "max_concurrent_runs": 3, "on_error": "stop" },
            "nodes": [
                { "id": "on_lead_added", "type": "trigger.lead_added_to_campaign", "name": "Lead added to campaign", "params": { "campaign_id": "${{ vars.campaign_id }}" } },
                { "id": "fetch_site", "type": "web.fetch_page", "name": "Read website", "params": { "url": "${{ trigger.lead.website }}", "max_chars": 6000 }, "on_error": "continue" },
                { "id": "write_email", "type": "ai.agent", "name": "Write email", "params": {
                    "provider": "default_draft",
                    "system": "You write short, plain cold emails. Use only facts from the context.",
                    "prompt": "Lead: ${{ trigger.lead.first_name }} at ${{ trigger.lead.company_name }}.\nSite text:\n${{ nodes.fetch_site.json.text | default('none') }}",
                    "output": { "format": "json", "fields": [
                        { "key": "subject", "type": "string", "description": "Under 8 words" },
                        { "key": "body", "type": "string", "description": "Under 120 words, plain text" } ] } } },
                { "id": "save_email", "type": "aikido.campaign.save_lead_email", "name": "Save primary email", "params": {
                    "campaign_id": "${{ trigger.campaign.id }}", "lead_id": "${{ trigger.lead.id }}", "step": 1,
                    "subject": "${{ nodes.write_email.json.subject }}", "body_text": "${{ nodes.write_email.json.body }}" } }
            ],
            "edges": [
                { "from": "on_lead_added", "to": "fetch_site" },
                { "from": "fetch_site", "to": "write_email" },
                { "from": "write_email", "to": "save_email" }
            ]
        })
    }

    fn run(v: &Value) -> ValidationResult {
        validate_value(v, &Options::default())
    }

    fn publish(v: &Value) -> ValidationResult {
        validate_value(v, &Options { mode: Mode::Publish, ..Options::default() })
    }

    /// Change the example with a closure and validate it.
    fn with(f: impl FnOnce(&mut Value)) -> ValidationResult {
        let mut v = example();
        f(&mut v);
        run(&v)
    }

    #[track_caller]
    fn has(r: &ValidationResult, code: &str) -> Issue {
        r.errors
            .iter()
            .chain(&r.warnings)
            .find(|i| i.code == code)
            .unwrap_or_else(|| panic!("no `{code}` in errors {:?} / warnings {:?}", r.errors, r.warnings))
            .clone()
    }

    #[test]
    fn the_example_of_section_4_1_is_valid() {
        let r = run(&example());
        assert!(r.ok, "{:?}", r.errors);
        assert!(r.errors.is_empty());
        // in a draft, the empty required variable is fine. On publish it is not.
        let p = publish(&example());
        has(&p, "variable_missing");
        let mut filled = example();
        filled["variables"][0]["value"] = json!("0192f3a0-0000-7000-8000-000000000002");
        let p = publish(&filled);
        assert!(p.ok, "{:?}", p.errors);
    }

    #[test]
    fn the_typed_definition_validates_too() {
        let d = WorkflowDefinition::blank("Blank");
        let r = validate(&d, &Options::default());
        assert!(r.ok, "{:?}", r.errors);
    }

    // ----- one failing case for every error code of section 4.8 -----

    #[test]
    fn invalid_format() {
        assert!(has(&run(&json!([1, 2])), "invalid_format").message.contains("one JSON object"));
        let r = with(|v| v["format"] = json!("other"));
        assert_eq!(has(&r, "invalid_format").path.as_deref(), Some("format"));
        let r = with(|v| {
            v.as_object_mut().unwrap().remove("version");
        });
        has(&r, "invalid_format");
        let r = with(|v| v["nodes"][1]["on_error"] = json!("explode"));
        let i = has(&r, "invalid_format");
        assert_eq!(i.node_id.as_deref(), Some("fetch_site"));
        assert!(i.message.contains("on_error") || i.message.contains("explode") || i.message.contains("variant"), "{}", i.message);
        let r = with(|v| v["nodes"] = json!("nope"));
        has(&r, "invalid_format");
        let r = with(|v| v["settings"]["max_concurrent_runs"] = json!(99));
        assert_eq!(has(&r, "invalid_format").path.as_deref(), Some("settings.max_concurrent_runs"));
        let r = with(|v| v["settings"]["timezone"] = json!("Mars/Base"));
        has(&r, "invalid_format");
        let r = with(|v| v["nodes"][1]["retry"] = json!({ "max": 9 }));
        has(&r, "invalid_format");
        let r = with(|v| v["nodes"][1]["batch"] = json!({ "size": 500 }));
        has(&r, "invalid_format");
    }

    #[test]
    fn format_too_new() {
        let r = with(|v| v["version"] = json!(2));
        has(&r, "format_too_new");
        assert!(!r.ok);
    }

    #[test]
    fn invalid_name() {
        has(&with(|v| v["name"] = json!("")), "invalid_name");
        has(&with(|v| v["name"] = json!("x".repeat(121))), "invalid_name");
        has(&with(|v| {
            v.as_object_mut().unwrap().remove("name");
        }), "invalid_name");
    }

    #[test]
    fn too_many_nodes() {
        let r = with(|v| {
            let extra: Vec<Value> = (0..201).map(|i| json!({ "id": format!("n{i}"), "type": "flow.noop" })).collect();
            v["nodes"].as_array_mut().unwrap().extend(extra);
        });
        has(&r, "too_many_nodes");
    }

    #[test]
    fn invalid_and_reserved_and_duplicate_node_ids() {
        let i = has(&with(|v| v["nodes"][1]["id"] = json!("Fetch Site")), "invalid_node_id");
        assert_eq!(i.path.as_deref(), Some("nodes[1].id"));
        has(&with(|v| v["nodes"][1]["id"] = json!("1abc")), "invalid_node_id");
        let r = with(|v| v["nodes"][1]["id"] = json!("trigger"));
        has(&r, "reserved_node_id");
        for word in RESERVED_IDS {
            has(&with(|v| v["nodes"][1]["id"] = json!(word)), "reserved_node_id");
        }
        let r = with(|v| v["nodes"][2]["id"] = json!("fetch_site"));
        has(&r, "duplicate_node_id");
    }

    #[test]
    fn unknown_node_type_suggests_names() {
        let i = has(&with(|v| v["nodes"][1]["type"] = json!("web.fetch_pag")), "unknown_node_type");
        assert!(i.message.contains("web.fetch_page"), "{}", i.message);
        let i = has(&with(|v| v["nodes"][1]["type"] = json!("http.requests")), "unknown_node_type");
        assert!(i.message.contains("http.request"), "{}", i.message);
        has(&with(|v| v["nodes"][1]["type"] = json!("zzz")), "unknown_node_type");
    }

    #[test]
    fn invalid_params_has_the_path() {
        let i = has(&with(|v| v["nodes"][1]["params"]["max_chars"] = json!(10)), "invalid_params");
        assert_eq!(i.node_id.as_deref(), Some("fetch_site"));
        assert_eq!(i.path.as_deref(), Some("params.max_chars"));
        let i = has(&with(|v| v["nodes"][1]["params"] = json!({})), "invalid_params");
        assert!(i.message.contains("url"), "{}", i.message);
        let i = has(&with(|v| v["nodes"][2]["params"]["output"]["fields"][0]["type"] = json!("weird")), "invalid_params");
        assert_eq!(i.path.as_deref(), Some("params.output.fields[0].type"));
        // an expression stands for a number
        let r = with(|v| v["nodes"][1]["params"]["max_chars"] = json!("${{ vars.campaign_id }}"));
        assert!(r.ok, "{:?}", r.errors);
        has(&with(|v| v["nodes"][1]["params"]["max_chars"] = json!("lots")), "invalid_params");
    }

    #[test]
    fn missing_trigger_and_too_many_triggers() {
        has(&with(|v| v["nodes"] = json!([{ "id": "a", "type": "flow.noop" }])), "missing_trigger");
        let r = with(|v| {
            let extra: Vec<Value> = (0..5).map(|i| json!({ "id": format!("t{i}"), "type": "trigger.manual" })).collect();
            v["nodes"].as_array_mut().unwrap().extend(extra);
        });
        has(&r, "too_many_triggers");
    }

    #[test]
    fn trigger_has_input() {
        let r = with(|v| v["edges"].as_array_mut().unwrap().push(json!({ "from": "fetch_site", "to": "on_lead_added" })));
        let i = has(&r, "trigger_has_input");
        assert_eq!(i.node_id.as_deref(), Some("on_lead_added"));
    }

    #[test]
    fn trigger_param_not_static() {
        let r = with(|v| v["nodes"][0]["params"]["campaign_id"] = json!("${{ trigger.lead.id }}"));
        has(&r, "trigger_param_not_static");
        let r = with(|v| v["nodes"][0]["params"]["campaign_id"] = json!("${{ nodes.fetch_site.json.title }}"));
        has(&r, "trigger_param_not_static");
    }

    #[test]
    fn unknown_edge_node_and_unknown_port_and_duplicate_edge() {
        let i = has(&with(|v| v["edges"][0]["to"] = json!("fetch_sit")), "unknown_edge_node");
        assert!(i.message.contains("fetch_site"), "{}", i.message);
        has(&with(|v| v["edges"][0]["from"] = json!("nobody")), "unknown_edge_node");
        let i = has(&with(|v| v["edges"][1]["port"] = json!("true")), "unknown_port");
        assert!(i.message.contains("main"), "{}", i.message);
        has(&with(|v| v["edges"][1]["port"] = json!("error")), "unknown_port");
        let r = with(|v| {
            v["nodes"][1]["on_error"] = json!("error_port");
            v["nodes"].as_array_mut().unwrap().push(json!({ "id": "handle", "type": "flow.noop" }));
            v["edges"].as_array_mut().unwrap().push(json!({ "from": "fetch_site", "port": "error", "to": "handle" }));
        });
        assert!(r.ok, "{:?}", r.errors);
        let r = with(|v| v["edges"].as_array_mut().unwrap().push(json!({ "from": "fetch_site", "to": "write_email" })));
        has(&r, "duplicate_edge");
        // flow.stop has no output port
        let r = with(|v| {
            v["nodes"].as_array_mut().unwrap().push(json!({ "id": "stop", "type": "flow.stop" }));
            v["edges"].as_array_mut().unwrap().push(json!({ "from": "stop", "to": "save_email" }));
        });
        has(&r, "unknown_port");
    }

    #[test]
    fn if_and_switch_ports() {
        let r = with(|v| {
            v["nodes"].as_array_mut().unwrap().extend([
                json!({ "id": "check", "type": "flow.if", "params": { "expression": "${{ json.title == 'x' }}" } }),
                json!({ "id": "yes", "type": "flow.noop" }),
                json!({ "id": "no", "type": "flow.noop" }),
                json!({ "id": "route", "type": "flow.switch", "params": { "cases": [{ "expression": "${{ true }}" }, { "expression": "${{ false }}" }] } }),
                json!({ "id": "c1", "type": "flow.noop" }),
                json!({ "id": "c3", "type": "flow.noop" }),
                json!({ "id": "other", "type": "flow.noop" }),
            ]);
            v["edges"].as_array_mut().unwrap().extend([
                json!({ "from": "fetch_site", "to": "check" }),
                json!({ "from": "check", "port": "true", "to": "yes" }),
                json!({ "from": "check", "port": "false", "to": "no" }),
                json!({ "from": "fetch_site", "to": "route" }),
                json!({ "from": "route", "port": "case_1", "to": "c1" }),
                json!({ "from": "route", "port": "default", "to": "other" }),
            ]);
        });
        assert!(r.ok, "{:?}", r.errors);
        // case_3 does not exist: the switch has 2 cases
        let r = with(|v| {
            v["nodes"].as_array_mut().unwrap().extend([
                json!({ "id": "route", "type": "flow.switch", "params": { "cases": [{ "expression": "${{ true }}" }] } }),
                json!({ "id": "c3", "type": "flow.noop" }),
            ]);
            v["edges"].as_array_mut().unwrap().extend([json!({ "from": "fetch_site", "to": "route" }), json!({ "from": "route", "port": "case_3", "to": "c3" })]);
        });
        has(&r, "unknown_port");
        // an If has no `main`
        let r = with(|v| {
            v["nodes"].as_array_mut().unwrap().extend([json!({ "id": "check", "type": "flow.if", "params": { "expression": "${{ true }}" } }), json!({ "id": "x", "type": "flow.noop" })]);
            v["edges"].as_array_mut().unwrap().extend([json!({ "from": "fetch_site", "to": "check" }), json!({ "from": "check", "to": "x" })]);
        });
        has(&r, "unknown_port");
    }

    #[test]
    fn cycle_detected_names_the_nodes() {
        let r = with(|v| v["edges"].as_array_mut().unwrap().push(json!({ "from": "save_email", "to": "fetch_site" })));
        let i = has(&r, "cycle_detected");
        assert!(i.message.contains("fetch_site -> write_email -> save_email -> fetch_site"), "{}", i.message);
        let r = with(|v| v["edges"].as_array_mut().unwrap().push(json!({ "from": "fetch_site", "to": "fetch_site" })));
        let i = has(&r, "cycle_detected");
        assert!(i.message.contains("fetch_site -> fetch_site"), "{}", i.message);
    }

    #[test]
    fn expression_syntax_is_reported_with_the_path() {
        let i = has(&with(|v| v["nodes"][2]["params"]["prompt"] = json!("Hi ${{ trigger.lead.first_name | default( }}")), "expression_syntax");
        assert_eq!(i.node_id.as_deref(), Some("write_email"));
        assert_eq!(i.path.as_deref(), Some("params.prompt"));
        has(&with(|v| v["nodes"][2]["params"]["system"] = json!("${% if x %}open")), "expression_syntax");
        let i = has(&with(|v| v["nodes"][2]["params"]["output"]["fields"][0]["description"] = json!("${{ oops")), "expression_syntax");
        assert_eq!(i.path.as_deref(), Some("params.output.fields[0].description"));
    }

    #[test]
    fn unknown_reference_suggests_the_close_id() {
        let r = with(|v| v["nodes"][2]["params"]["prompt"] = json!("${{ nodes.fetch_sit.json.text }}"));
        let i = has(&r, "unknown_reference");
        assert_eq!(i.node_id.as_deref(), Some("write_email"));
        assert_eq!(i.path.as_deref(), Some("params.prompt"));
        assert!(i.message.contains("nodes.fetch_sit is not a node"), "{}", i.message);
        assert!(i.message.contains("Did you mean `fetch_site`?"), "{}", i.message);
        // a name that is not a root
        has(&with(|v| v["nodes"][2]["params"]["prompt"] = json!("${{ lead.email }}")), "unknown_reference");
        // a wrong second step
        has(&with(|v| v["nodes"][2]["params"]["prompt"] = json!("${{ nodes.fetch_site.output.text }}")), "unknown_reference");
        // the node itself
        has(&with(|v| v["nodes"][2]["params"]["prompt"] = json!("${{ nodes.write_email.json.subject }}")), "unknown_reference");
    }

    #[test]
    fn a_node_can_read_only_upstream_nodes() {
        // a later node
        let r = with(|v| v["nodes"][1]["params"]["url"] = json!("${{ nodes.write_email.json.subject }}"));
        let i = has(&r, "unknown_reference");
        assert!(i.message.contains("not a node upstream"), "{}", i.message);

        // a diamond: trigger -> a -> (b, c) -> merge -> d
        let diamond = |reader: &str, read: &str| {
            let mut nodes = vec![
                json!({ "id": "t", "type": "trigger.manual" }),
                json!({ "id": "a", "type": "flow.noop" }),
                json!({ "id": "b", "type": "flow.noop" }),
                json!({ "id": "c", "type": "flow.noop" }),
                json!({ "id": "m", "type": "flow.merge" }),
                json!({ "id": "d", "type": "flow.noop" }),
            ];
            for n in nodes.iter_mut() {
                if n["id"] == reader {
                    n["type"] = json!("data.set");
                    n["params"] = json!({ "fields": { "x": format!("${{{{ {read} }}}}") } });
                }
            }
            json!({ "format": "aikido-workflow", "version": 1, "name": "Diamond", "nodes": nodes, "edges": [
                { "from": "t", "to": "a" }, { "from": "a", "to": "b" }, { "from": "a", "to": "c" },
                { "from": "b", "to": "m" }, { "from": "c", "to": "m" }, { "from": "m", "to": "d" } ] })
        };
        // d (after the merge) may read b, c, a
        for ok in ["nodes.a.json", "nodes.b.json", "nodes.c.json", "nodes.m.json"] {
            let r = run(&diamond("d", ok));
            assert!(!r.errors.iter().any(|e| e.code == "unknown_reference"), "{ok}: {:?}", r.errors);
        }
        // b may not read its sibling branch c
        let r = run(&diamond("b", "nodes.c.json.x"));
        has(&r, "unknown_reference");
        // a may not read anything below it
        has(&run(&diamond("a", "nodes.d.json.x")), "unknown_reference");
        // after the merge, reading `b` is ambiguous (the merge joins items)
        let r = run(&diamond("d", "nodes.b.json.x"));
        has(&r, "ambiguous_reference");
        assert!(r.ok, "a warning, not an error: {:?}", r.errors);
    }

    #[test]
    fn unknown_variable() {
        let i = has(&with(|v| v["nodes"][1]["params"]["url"] = json!("${{ vars.campaing_id }}")), "unknown_variable");
        assert!(i.message.contains("campaign_id"), "{}", i.message);
        has(&with(|v| v["nodes"][0]["params"]["campaign_id"] = json!("${{ vars.nope }}")), "unknown_variable");
    }

    #[test]
    fn variable_rules() {
        has(&with(|v| v["variables"][0]["key"] = json!("Campaign Id")), "invalid_format");
        has(&with(|v| v["variables"] = json!([{ "key": "a", "kind": "text" }, { "key": "a", "kind": "text" }])), "invalid_format");
        has(&with(|v| v["variables"][0]["kind"] = json!("weird")), "invalid_format");
        let i = has(&publish(&example()), "variable_missing");
        assert_eq!(i.path.as_deref(), Some("variables[0].value"));
    }

    #[test]
    fn unknown_provider() {
        let known: HashSet<String> = ["claude-main".to_string()].into();
        let opts = Options { providers: Some(&known), ..Options::default() };
        let mut v = example();
        v["nodes"][2]["params"]["provider"] = json!("claude-main");
        assert!(validate_value(&v, &opts).ok);
        v["nodes"][2]["params"]["provider"] = json!("gpt-nowhere");
        let r = validate_value(&v, &opts);
        has(&r, "unknown_provider");
        v["nodes"][2]["params"]["provider"] = json!("default_slot");
        assert!(validate_value(&v, &opts).ok);
        // a provider id is fine too
        v["nodes"][2]["params"]["provider"] = json!("0192f3a0-0000-7000-8000-0000000000aa");
        assert!(validate_value(&v, &opts).ok);
        // not checked when the list is not known
        v["nodes"][2]["params"]["provider"] = json!("gpt-nowhere");
        assert!(run(&v).ok);
    }

    #[test]
    fn ai_agent_output_fields_are_checked() {
        let mut v = example();
        v["nodes"][2]["params"]["output"]["fields"] = json!([{ "key": "subject" }, { "key": "subject" }]);
        let r = run(&v);
        has(&r, "invalid_params");
        let e = r.errors.iter().find(|e| e.code == "invalid_params" && e.path.as_deref() == Some("params.output.fields")).expect("the fields error");
        assert!(e.message.contains("used twice"), "{}", e.message);
        v["nodes"][2]["params"]["output"]["fields"] = json!([{ "key": "1bad" }]);
        has(&run(&v), "invalid_params");
        // a text answer has no fields to check
        v["nodes"][2]["params"]["output"] = json!({ "format": "text" });
        assert!(run(&v).ok);
    }

    #[test]
    fn unknown_secret_is_a_warning_in_a_draft_and_an_error_on_publish() {
        let known: HashSet<String> = HashSet::new();
        let mut v = example();
        v["variables"][0]["value"] = json!("c1");
        v["nodes"].as_array_mut().unwrap().push(json!({ "id": "search", "type": "http.request", "params": {
            "url": "https://api.example.com/search", "headers": { "X-Key": "${{ secret('SEARCH_API_KEY') }}" },
            "auth": { "type": "bearer", "secret": "OTHER_KEY" } } }));
        v["edges"].as_array_mut().unwrap().push(json!({ "from": "fetch_site", "to": "search" }));
        let draft = validate_value(&v, &Options { secrets: Some(&known), ..Options::default() });
        assert!(draft.ok, "{:?}", draft.errors);
        assert_eq!(draft.warnings.iter().filter(|w| w.code == "unknown_secret").count(), 2);
        let publish = validate_value(&v, &Options { mode: Mode::Publish, secrets: Some(&known), ..Options::default() });
        assert!(!publish.ok);
        assert_eq!(publish.errors.iter().filter(|w| w.code == "unknown_secret").count(), 2);
        let have: HashSet<String> = ["SEARCH_API_KEY".to_string(), "OTHER_KEY".to_string()].into();
        let ok = validate_value(&v, &Options { mode: Mode::Publish, secrets: Some(&have), ..Options::default() });
        assert!(ok.ok, "{:?}", ok.errors);
    }

    #[test]
    fn admin_node_not_allowed_for_a_member_on_publish() {
        let mut v = example();
        v["variables"][0]["value"] = json!("c1");
        v["nodes"].as_array_mut().unwrap().push(json!({ "id": "go", "type": "aikido.campaign.status", "params": { "campaign_id": "c1", "action": "launch" } }));
        v["edges"].as_array_mut().unwrap().push(json!({ "from": "save_email", "to": "go" }));
        let member = Options { mode: Mode::Publish, is_admin: false, ..Options::default() };
        has(&validate_value(&v, &member), "admin_node_not_allowed");
        let admin = Options { mode: Mode::Publish, is_admin: true, ..Options::default() };
        assert!(validate_value(&v, &admin).ok);
        // a member may still edit the draft
        assert!(validate_value(&v, &Options { is_admin: false, ..Options::default() }).ok);
    }

    // ----- warnings -----

    #[test]
    fn warnings() {
        // unreachable
        let r = with(|v| v["nodes"].as_array_mut().unwrap().push(json!({ "id": "old_step", "type": "flow.noop" })));
        assert_eq!(has(&r, "unreachable_node").node_id.as_deref(), Some("old_step"));
        assert!(r.ok);
        // two inputs
        let r = with(|v| v["edges"].as_array_mut().unwrap().push(json!({ "from": "on_lead_added", "to": "write_email" })));
        assert_eq!(has(&r, "multiple_inputs").node_id.as_deref(), Some("write_email"));
        // unknown fields
        let r = with(|v| {
            v["colour"] = json!("red");
            v["nodes"][1]["params"]["extra"] = json!(1);
        });
        assert!(r.warnings.iter().any(|w| w.code == "unknown_field_ignored" && w.path.as_deref() == Some("colour")));
        assert!(r.warnings.iter().any(|w| w.code == "unknown_field_ignored" && w.path.as_deref() == Some("params.extra")));
        assert!(r.ok);
        // no positions
        has(&run(&example()), "no_positions");
        let r = with(|v| {
            for n in v["nodes"].as_array_mut().unwrap() {
                n["position"] = json!({ "x": 0, "y": 0 });
            }
        });
        assert!(!r.has_code("no_positions"));
        // pins
        let r = with(|v| v["pins"] = json!({ "fetch_site": [{ "text": "x" }], "ghost": [{}] }));
        has(&r, "pinned_data_in_production");
        assert!(r.warnings.iter().any(|w| w.code == "unknown_field_ignored" && w.path.as_deref() == Some("pins.ghost")));
    }

    #[test]
    fn expression_maybe_empty_for_a_field_the_node_does_not_give() {
        // the AI node declares subject and body; `title` is not one of them
        let r = with(|v| v["nodes"][3]["params"]["subject"] = json!("${{ nodes.write_email.json.title }}"));
        let w = has(&r, "expression_maybe_empty");
        assert!(w.message.contains("body, subject"), "{}", w.message);
        assert!(r.ok);
        // a misspelt field of the trigger item
        let r = with(|v| v["nodes"][1]["params"]["url"] = json!("${{ trigger.leads.website }}"));
        has(&r, "expression_maybe_empty");
        // fetch_page gives these keys
        let r = with(|v| v["nodes"][2]["params"]["prompt"] = json!("${{ nodes.fetch_site.json.body }}"));
        has(&r, "expression_maybe_empty");
        // an open output is not checked
        let r = with(|v| {
            v["nodes"].as_array_mut().unwrap().push(json!({ "id": "web", "type": "http.request", "params": { "url": "https://x.example" } }));
            v["nodes"].as_array_mut().unwrap().push(json!({ "id": "use", "type": "data.set", "params": { "fields": { "a": "${{ nodes.web.json.body.anything }}" } } }));
            v["edges"].as_array_mut().unwrap().extend([json!({ "from": "fetch_site", "to": "web" }), json!({ "from": "web", "to": "use" })]);
        });
        assert!(!r.has_code("expression_maybe_empty"));
    }

    #[test]
    fn many_problems_are_all_reported_at_once() {
        let r = with(|v| {
            v["nodes"][1]["params"]["max_chars"] = json!(1);
            v["nodes"][2]["params"]["prompt"] = json!("${{ nodes.nope.json.x }}");
            v["edges"][2]["to"] = json!("ghost");
        });
        assert!(r.errors.len() >= 3, "{:?}", r.errors);
        assert!(!r.ok);
    }
}
