//! The nodes of phase 2: the manual trigger (build map 7.1), the flow nodes (7.5) and the data nodes (7.6).
//! The specs are in `catalog.rs`; this file holds what they do.
use crate::automations::registry::{Item, LINEAGE_CUT, NodeCtx, NodeError, NodeInput, NodeOutput, NodeSpec, NodeType, StopRequest, StubNode};
use async_trait::async_trait;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde_json::{Map, Value, json};
use std::cmp::Ordering;
use std::fmt::Write as _;

/// A wait up to this long happens inside the task. A longer one parks the run (build map 8.5).
const SHORT_WAIT_SECS: f64 = 60.0;
/// The longest wait (30 days).
const MAX_WAIT_SECS: f64 = 30.0 * 86400.0;

#[derive(Clone, Copy)]
enum Kind {
    Manual,
    If,
    Switch,
    Filter,
    Merge,
    Wait,
    Stop,
    Noop,
    Set,
    SplitOut,
    Aggregate,
    Limit,
    Dedupe,
    Sort,
    ParseJson,
    Date,
}

struct Builtin {
    spec: NodeSpec,
    kind: Kind,
}

/// The real node for a spec, or a stub when the node is not built yet.
pub fn make_node(spec: NodeSpec) -> Box<dyn NodeType> {
    // every trigger gives its item on: the run was started by the trigger plan, the node itself does nothing
    let kind = match spec.type_id {
        id if id.starts_with("trigger.") => Kind::Manual,
        "flow.if" => Kind::If,
        "flow.switch" => Kind::Switch,
        "flow.filter" => Kind::Filter,
        "flow.merge" => Kind::Merge,
        "flow.wait" => Kind::Wait,
        "flow.stop" => Kind::Stop,
        "flow.noop" => Kind::Noop,
        "data.set" => Kind::Set,
        "data.split_out" => Kind::SplitOut,
        "data.aggregate" => Kind::Aggregate,
        "data.limit" => Kind::Limit,
        "data.dedupe" => Kind::Dedupe,
        "data.sort" => Kind::Sort,
        "data.parse_json" => Kind::ParseJson,
        "data.date" => Kind::Date,
        _ => {
            // the nodes that talk to the outside
            let id = spec.type_id;
            let spec = if id.starts_with("aikido.") {
                match super::aikido_nodes::make(spec) {
                    Ok(n) => return n,
                    Err(spec) => spec,
                }
            } else {
                spec
            };
            let spec = match super::ai_nodes::make(spec) {
                Ok(n) => return n,
                Err(spec) => spec,
            };
            let spec = match super::code_node::make(spec) {
                Ok(n) => return n,
                Err(spec) => spec,
            };
            let spec = match super::sub_workflow::make(spec) {
                Ok(n) => return n,
                Err(spec) => spec,
            };
            return match super::web_nodes::make(spec) {
                Ok(n) => n,
                Err(spec) => Box::new(StubNode(spec)),
            };
        }
    };
    Box::new(Builtin { spec, kind })
}

#[async_trait]
impl NodeType for Builtin {
    fn spec(&self) -> &NodeSpec {
        &self.spec
    }

    async fn run(&self, ctx: &NodeCtx<'_>, input: NodeInput) -> Result<NodeOutput, NodeError> {
        match self.kind {
            Kind::Manual | Kind::Noop => Ok(NodeOutput::main(input.items)),
            Kind::If => route_if(ctx, input),
            Kind::Switch => route_switch(ctx, input),
            Kind::Filter => filter(ctx, input),
            Kind::Merge => merge(ctx, input),
            Kind::Wait => wait(ctx, input).await,
            Kind::Stop => stop(ctx, input),
            Kind::Set => set_fields(ctx, input),
            Kind::SplitOut => split_out(ctx, input),
            Kind::Aggregate => aggregate(ctx, input),
            Kind::Limit => limit(ctx, input),
            Kind::Dedupe => dedupe(ctx, input),
            Kind::Sort => sort(ctx, input),
            Kind::ParseJson => parse_json(ctx, input),
            Kind::Date => date(ctx, input),
        }
    }
}

fn bad(msg: impl Into<String>) -> NodeError {
    NodeError::new("invalid_param_value", msg)
}

// ---------- paths ----------

/// Read `a.b.0.c` from an object.
pub fn get_path<'a>(m: &'a Map<String, Value>, path: &str) -> Option<&'a Value> {
    let mut parts = path.split('.').filter(|p| !p.is_empty());
    let mut cur = m.get(parts.next()?)?;
    for p in parts {
        cur = match cur {
            Value::Object(o) => o.get(p)?,
            Value::Array(a) => a.get(p.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

/// Write `a.b.c`, making the objects on the way. A value that is in the way and is not an object is replaced.
pub fn set_path(m: &mut Map<String, Value>, path: &[&str], v: Value) {
    match path {
        [] => {}
        [last] => {
            m.insert((*last).to_string(), v);
        }
        [first, rest @ ..] => {
            let e = m.entry((*first).to_string()).or_insert_with(|| json!({}));
            if !e.is_object() {
                *e = json!({});
            }
            if let Some(o) = e.as_object_mut() {
                set_path(o, rest, v);
            }
        }
    }
}

fn path_parts(path: &str) -> Vec<&str> {
    path.split('.').filter(|p| !p.is_empty()).collect()
}

// ---------- conditions (If, Switch, Filter) ----------

fn number_of(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok().filter(|f| f.is_finite()),
        _ => None,
    }
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Equal. A number equals a text that reads as that number (`5` and `"5"`); two texts are compared as text.
fn loose_equal(a: &Value, b: &Value) -> bool {
    if matches!(a, Value::Number(_)) || matches!(b, Value::Number(_)) {
        if let (Some(x), Some(y)) = (number_of(a), number_of(b)) {
            return x == y;
        }
    }
    a == b
}

fn is_empty_value(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.trim().is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

fn is_truthy(v: &Value, want: bool) -> bool {
    match v {
        Value::Bool(b) => *b == want,
        Value::String(s) => s.trim().eq_ignore_ascii_case(if want { "true" } else { "false" }),
        _ => false,
    }
}

fn compare(op: &str, left: &Value, right: &Value) -> Result<bool, NodeError> {
    Ok(match op {
        "equals" => loose_equal(left, right),
        "not_equals" => !loose_equal(left, right),
        "contains" | "not_contains" => {
            let found = match left {
                Value::Array(a) => a.iter().any(|x| loose_equal(x, right)),
                Value::Object(o) => o.contains_key(&text_of(right)),
                other => text_of(other).contains(&text_of(right)),
            };
            found == (op == "contains")
        }
        "starts_with" => text_of(left).starts_with(&text_of(right)),
        "ends_with" => text_of(left).ends_with(&text_of(right)),
        "gt" | "gte" | "lt" | "lte" => {
            let ord = match (number_of(left), number_of(right)) {
                (Some(a), Some(b)) => a.partial_cmp(&b),
                _ => match (left, right) {
                    (Value::String(a), Value::String(b)) => Some(a.cmp(b)),
                    _ => None,
                },
            };
            match ord {
                None => false,
                Some(o) => match op {
                    "gt" => o == Ordering::Greater,
                    "gte" => o != Ordering::Less,
                    "lt" => o == Ordering::Less,
                    _ => o != Ordering::Greater,
                },
            }
        }
        "is_empty" => is_empty_value(left),
        "is_not_empty" => !is_empty_value(left),
        "is_true" => is_truthy(left, true),
        "is_false" => is_truthy(left, false),
        "matches" => {
            let pattern = text_of(right);
            if pattern.chars().count() > 200 {
                return Err(bad("A `matches` pattern can have up to 200 characters."));
            }
            let re = regex::RegexBuilder::new(&pattern).size_limit(1 << 20).build().map_err(|e| bad(format!("The `matches` pattern is not valid: {e}")))?;
            re.is_match(&text_of(left))
        }
        other => return Err(bad(format!("`{other}` is not a known condition."))),
    })
}

/// A value read as true or false (the result of a `expression` param).
fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Null => false,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !matches!(s.trim().to_ascii_lowercase().as_str(), "" | "false" | "0"),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Do the rules (`conditions` + `combine`, or one `expression`) match this item?
fn rules_match(ctx: &NodeCtx<'_>, rules: &Map<String, Value>, item: &Item, index: usize) -> Result<bool, NodeError> {
    if let Some(expr) = rules.get("expression").filter(|v| !v.is_null()) {
        return Ok(truthy(&ctx.value(item, index, expr)?));
    }
    let Some(conds) = rules.get("conditions").and_then(Value::as_array) else {
        return Err(NodeError::new("invalid_params", "This node needs `conditions` or `expression`."));
    };
    let any = rules.get("combine").and_then(Value::as_str) == Some("or");
    if conds.is_empty() {
        return Ok(true);
    }
    for (i, c) in conds.iter().enumerate() {
        let Some(c) = c.as_object() else {
            return Err(bad(format!("conditions[{i}] must be an object with `left`, `op` and `right`.")));
        };
        let op = c.get("op").and_then(Value::as_str).ok_or_else(|| bad(format!("conditions[{i}] needs `op`.")))?;
        let left = ctx.value(item, index, c.get("left").unwrap_or(&Value::Null))?;
        let right = ctx.value(item, index, c.get("right").unwrap_or(&Value::Null))?;
        let hit = compare(op, &left, &right)?;
        if any && hit {
            return Ok(true);
        }
        if !any && !hit {
            return Ok(false);
        }
    }
    Ok(!any)
}

fn route_if(ctx: &NodeCtx<'_>, input: NodeInput) -> Result<NodeOutput, NodeError> {
    let (mut yes, mut no) = (vec![], vec![]);
    for (i, item) in input.items.into_iter().enumerate() {
        if rules_match(ctx, ctx.params, &item, input.first_index + i)? { yes.push(item) } else { no.push(item) }
    }
    let mut out = NodeOutput::port("true", yes);
    out.ports.insert("false".into(), no);
    Ok(out)
}

fn route_switch(ctx: &NodeCtx<'_>, input: NodeInput) -> Result<NodeOutput, NodeError> {
    let cases = ctx.raw("cases").and_then(Value::as_array).ok_or_else(|| NodeError::new("invalid_params", "Switch needs `cases`."))?;
    let first_only = ctx.raw("first_match_only").and_then(Value::as_bool).unwrap_or(true);
    let mut out = NodeOutput::default();
    for (i, item) in input.items.into_iter().enumerate() {
        let mut hit = false;
        for (n, case) in cases.iter().enumerate() {
            let rules = case.as_object().ok_or_else(|| bad(format!("cases[{n}] must be an object.")))?;
            if rules_match(ctx, rules, &item, input.first_index + i)? {
                hit = true;
                out.ports.entry(format!("case_{}", n + 1)).or_default().push(item.clone());
                if first_only {
                    break;
                }
            }
        }
        if !hit {
            out.ports.entry("default".into()).or_default().push(item);
        }
    }
    Ok(out)
}

fn filter(ctx: &NodeCtx<'_>, input: NodeInput) -> Result<NodeOutput, NodeError> {
    let mut keep = vec![];
    for (i, item) in input.items.into_iter().enumerate() {
        if rules_match(ctx, ctx.params, &item, input.first_index + i)? {
            keep.push(item);
        }
    }
    Ok(NodeOutput::main(keep))
}

// ---------- merge, wait, stop ----------

fn merge(ctx: &NodeCtx<'_>, input: NodeInput) -> Result<NodeOutput, NodeError> {
    let mode = ctx.raw("mode").and_then(Value::as_str).unwrap_or("append");
    match mode {
        // The items lose the link to the nodes before (build map 5.5).
        "append" => Ok(NodeOutput::main(
            input.inputs.into_iter().flatten().map(|i| Item { json: i.json, lineage: vec![(LINEAGE_CUT.into(), 0)] }).collect(),
        )),
        "wait_all" => Ok(NodeOutput::main(input.inputs.into_iter().next().unwrap_or_default())),
        "combine_by_position" => {
            let len = input.inputs.iter().map(Vec::len).max().unwrap_or(0);
            let first = input.inputs.first();
            let mut items = Vec::with_capacity(len);
            for i in 0..len {
                let mut json = Map::new();
                for inp in &input.inputs {
                    if let Some(it) = inp.get(i) {
                        json.extend(it.json.clone());
                    }
                }
                // the lineage of input 1 is kept; without an item there, the link is cut
                let lineage = match first.and_then(|f| f.get(i)) {
                    Some(it) => it.lineage.clone(),
                    None => vec![(LINEAGE_CUT.into(), 0)],
                };
                items.push(Item { json, lineage });
            }
            Ok(NodeOutput::main(items))
        }
        other => Err(bad(format!("`{other}` is not a merge mode."))),
    }
}

fn unit_seconds(unit: &str) -> Result<f64, NodeError> {
    match unit {
        "seconds" => Ok(1.0),
        "minutes" => Ok(60.0),
        "hours" => Ok(3600.0),
        "days" => Ok(86400.0),
        other => Err(bad(format!("`{other}` is not a time unit. Use seconds, minutes, hours or days."))),
    }
}

fn parse_iso(s: &str) -> Result<DateTime<Utc>, NodeError> {
    crate::automations::expr::parse_time(s).map_err(|e| bad(e.detail().map(str::to_string).unwrap_or_else(|| e.to_string())))
}

async fn wait(ctx: &NodeCtx<'_>, input: NodeInput) -> Result<NodeOutput, NodeError> {
    let item = input.items.first().cloned().unwrap_or_else(NodeCtx::no_item);
    let idx = input.first_index;
    let now = ctx.eval.facts().now();
    let mode = ctx.str_param(&item, idx, "mode")?.unwrap_or_else(|| "duration".into());
    let secs = match mode.as_str() {
        "duration" => {
            let amount = ctx.f64_param(&item, idx, "amount")?.ok_or_else(|| NodeError::new("invalid_params", "Wait needs `amount` and `unit`."))?;
            let unit = ctx.str_param(&item, idx, "unit")?.unwrap_or_else(|| "seconds".into());
            if amount < 0.0 {
                return Err(bad("`amount` cannot be negative."));
            }
            amount * unit_seconds(&unit)?
        }
        "until" => {
            let at = ctx.str_param(&item, idx, "until")?.ok_or_else(|| NodeError::new("invalid_params", "Wait needs `until`."))?;
            ((parse_iso(&at)? - now).num_milliseconds() as f64 / 1000.0).max(0.0)
        }
        other => return Err(bad(format!("`{other}` is not a wait mode. Use duration or until."))),
    };
    if secs > MAX_WAIT_SECS {
        return Err(NodeError::new("wait_too_long", "A Wait node can wait up to 30 days."));
    }
    let mut out = NodeOutput::main(input.items);
    if secs <= SHORT_WAIT_SECS {
        ctx.control.sleep(std::time::Duration::from_secs_f64(secs)).await;
    } else {
        out.wait_until = Some(now + Duration::milliseconds((secs * 1000.0) as i64));
    }
    Ok(out)
}

fn stop(ctx: &NodeCtx<'_>, input: NodeInput) -> Result<NodeOutput, NodeError> {
    let item = input.items.first().cloned().unwrap_or_else(NodeCtx::no_item);
    let message = ctx.str_param(&item, input.first_index, "message")?.unwrap_or_else(|| "The workflow was stopped by a Stop node.".into());
    let error = ctx.bool_param(&item, input.first_index, "error")?.unwrap_or(true);
    Ok(NodeOutput { stop: Some(StopRequest { message, error }), ..Default::default() })
}

// ---------- data ----------

fn set_fields(ctx: &NodeCtx<'_>, input: NodeInput) -> Result<NodeOutput, NodeError> {
    let fields = ctx.raw("fields").and_then(Value::as_object).ok_or_else(|| NodeError::new("invalid_params", "Set fields needs `fields`."))?;
    let keep_all = ctx.raw("keep").and_then(Value::as_str) != Some("none");
    let mut out = Vec::with_capacity(input.items.len());
    for (i, item) in input.items.iter().enumerate() {
        let mut json = if keep_all { item.json.clone() } else { Map::new() };
        for (key, value) in fields {
            let v = ctx.value(item, input.first_index + i, value)?;
            let parts = path_parts(key);
            if parts.is_empty() {
                return Err(bad("A field name cannot be empty."));
            }
            set_path(&mut json, &parts, v);
        }
        out.push(item.child(json));
    }
    Ok(NodeOutput::main(out))
}

fn split_out(ctx: &NodeCtx<'_>, input: NodeInput) -> Result<NodeOutput, NodeError> {
    let path = ctx.raw("path").and_then(Value::as_str).ok_or_else(|| NodeError::new("invalid_params", "Split out needs `path`."))?;
    let keep_parent = ctx.raw("keep_parent").and_then(Value::as_bool).unwrap_or(false);
    let mut out = vec![];
    for item in &input.items {
        let list = match get_path(&item.json, path) {
            Some(Value::Array(a)) => a.clone(),
            Some(Value::Null) => vec![],
            Some(_) => return Err(NodeError::new("path_not_a_list", format!("`{path}` is not a list."))),
            None => return Err(NodeError::new("path_not_a_list", format!("`{path}` is not in the item."))),
        };
        for el in list {
            let mut json = if keep_parent {
                let mut p = item.json.clone();
                // the list itself is replaced by its element
                let parts = path_parts(path);
                if let [only] = parts.as_slice() {
                    p.remove(*only);
                }
                p
            } else {
                Map::new()
            };
            match el {
                Value::Object(o) => json.extend(o),
                other => {
                    json.insert("value".into(), other);
                }
            }
            out.push(item.child(json));
        }
    }
    Ok(NodeOutput::main(out))
}

fn aggregate(ctx: &NodeCtx<'_>, input: NodeInput) -> Result<NodeOutput, NodeError> {
    let mode = ctx.raw("mode").and_then(Value::as_str).unwrap_or("list");
    let mut json = Map::new();
    match mode {
        "list" => {
            json.insert("items".into(), Value::Array(input.items.iter().map(|i| Value::Object(i.json.clone())).collect()));
        }
        "fields" => {
            let fields = ctx.raw("fields").and_then(Value::as_array).ok_or_else(|| NodeError::new("invalid_params", "Aggregate in mode `fields` needs `fields`."))?;
            for f in fields {
                let name = f.as_str().ok_or_else(|| bad("`fields` is a list of field names."))?;
                let values: Vec<Value> = input.items.iter().map(|i| get_path(&i.json, name).cloned().unwrap_or(Value::Null)).collect();
                json.insert(name.to_string(), Value::Array(values));
            }
        }
        other => return Err(bad(format!("`{other}` is not an aggregate mode."))),
    }
    // many items become one: the link to the nodes before is cut (build map 5.4)
    Ok(NodeOutput::main(vec![Item { json, lineage: vec![(LINEAGE_CUT.into(), 0)] }]))
}

fn limit(ctx: &NodeCtx<'_>, mut input: NodeInput) -> Result<NodeOutput, NodeError> {
    let item = NodeCtx::no_item();
    let max = ctx.i64_param(&item, 0, "max")?.ok_or_else(|| NodeError::new("invalid_params", "Limit needs `max`."))?;
    if max < 0 {
        return Err(bad("`max` cannot be negative."));
    }
    let max = max as usize;
    let from_end = ctx.str_param(&item, 0, "from")?.as_deref() == Some("end");
    if input.items.len() > max {
        input.items = if from_end { input.items.split_off(input.items.len() - max) } else { input.items.into_iter().take(max).collect() };
    }
    Ok(NodeOutput::main(input.items))
}

fn dedupe(ctx: &NodeCtx<'_>, input: NodeInput) -> Result<NodeOutput, NodeError> {
    let key = ctx.raw("key").ok_or_else(|| NodeError::new("invalid_params", "Remove duplicates needs `key`."))?;
    let mut seen = std::collections::HashSet::new();
    let mut out = vec![];
    for (i, item) in input.items.into_iter().enumerate() {
        let k = ctx.value(&item, i, key)?;
        if seen.insert(crate::automations::definition::canonical_json(&k)) {
            out.push(item);
        }
    }
    Ok(NodeOutput::main(out))
}

fn rank(v: &Value) -> u8 {
    match v {
        Value::Number(_) => 0,
        Value::String(_) => 1,
        Value::Bool(_) => 2,
        Value::Null => 4,
        _ => 3,
    }
}

fn order_values(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64().partial_cmp(&y.as_f64()).unwrap_or(Ordering::Equal),
        (Value::String(x), Value::String(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ => rank(a).cmp(&rank(b)).then_with(|| a.to_string().cmp(&b.to_string())),
    }
}

fn sort(ctx: &NodeCtx<'_>, input: NodeInput) -> Result<NodeOutput, NodeError> {
    let by = ctx.raw("by").ok_or_else(|| NodeError::new("invalid_params", "Sort needs `by`."))?;
    let desc = ctx.raw("order").and_then(Value::as_str) == Some("desc");
    let mut keyed: Vec<(Value, Item)> = Vec::with_capacity(input.items.len());
    for (i, item) in input.items.into_iter().enumerate() {
        keyed.push((ctx.value(&item, i, by)?, item));
    }
    // empty values (null) are always last
    keyed.sort_by(|(a, _), (b, _)| match (a.is_null(), b.is_null()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => if desc { order_values(b, a) } else { order_values(a, b) },
    });
    Ok(NodeOutput::main(keyed.into_iter().map(|(_, i)| i).collect()))
}

fn parse_json(ctx: &NodeCtx<'_>, input: NodeInput) -> Result<NodeOutput, NodeError> {
    let path = ctx.raw("path").and_then(Value::as_str).ok_or_else(|| NodeError::new("invalid_params", "Parse JSON needs `path`."))?;
    let into = ctx.raw("into").and_then(Value::as_str).unwrap_or(path);
    let mut out = vec![];
    for item in input.items {
        let parsed = match get_path(&item.json, path) {
            Some(Value::String(s)) => serde_json::from_str::<Value>(s).map_err(|e| NodeError::new("invalid_json", format!("`{path}` is not valid JSON: {e}")))?,
            Some(v @ (Value::Object(_) | Value::Array(_))) => v.clone(),
            Some(_) => return Err(NodeError::new("invalid_json", format!("`{path}` is not a text."))),
            None => return Err(NodeError::new("invalid_json", format!("`{path}` is not in the item."))),
        };
        let mut json = item.json.clone();
        set_path(&mut json, &path_parts(into), parsed);
        out.push(item.child(json));
    }
    Ok(NodeOutput::main(out))
}

fn date(ctx: &NodeCtx<'_>, input: NodeInput) -> Result<NodeOutput, NodeError> {
    let mut out = vec![];
    for (i, item) in input.items.into_iter().enumerate() {
        let idx = input.first_index + i;
        let value = ctx.str_param(&item, idx, "value")?.ok_or_else(|| NodeError::new("invalid_params", "Date needs `value`."))?;
        let t = parse_iso(&value)?;
        let action = ctx.str_param(&item, idx, "action")?.ok_or_else(|| NodeError::new("invalid_params", "Date needs `action`."))?;
        let unit = ctx.str_param(&item, idx, "unit")?.unwrap_or_else(|| "seconds".into());
        let result: Value = match action.as_str() {
            "format" => {
                let fmt = ctx.str_param(&item, idx, "format")?.unwrap_or_else(|| "%Y-%m-%dT%H:%M:%SZ".into());
                let mut s = String::new();
                write!(s, "{}", t.format(&fmt)).map_err(|_| bad(format!("`{fmt}` is not a valid date format.")))?;
                Value::String(s)
            }
            "add" => {
                let amount = ctx.f64_param(&item, idx, "amount")?.ok_or_else(|| NodeError::new("invalid_params", "Date `add` needs `amount`."))?;
                let ms = amount * unit_seconds(&unit)? * 1000.0;
                if !ms.is_finite() || ms.abs() > 1e15 {
                    return Err(bad("The time span is too large."));
                }
                let t2 = t.checked_add_signed(Duration::milliseconds(ms as i64)).ok_or_else(|| bad("The time is out of range."))?;
                Value::String(t2.to_rfc3339_opts(SecondsFormat::Secs, true))
            }
            "diff" => {
                let other = ctx.str_param(&item, idx, "other")?.ok_or_else(|| NodeError::new("invalid_params", "Date `diff` needs `other`."))?;
                let d = (t - parse_iso(&other)?).num_milliseconds() as f64 / 1000.0 / unit_seconds(&unit)?;
                if d.fract() == 0.0 { json!(d as i64) } else { json!(d) }
            }
            other => return Err(bad(format!("`{other}` is not a date action. Use format, add or diff."))),
        };
        let into = ctx.str_param(&item, idx, "into")?.unwrap_or_else(|| "result".into());
        let mut json = item.json.clone();
        set_path(&mut json, &path_parts(&into), result);
        out.push(item.child(json));
    }
    Ok(NodeOutput::main(out))
}
