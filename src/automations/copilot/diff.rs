//! The diff between two drafts (build map 16.5), and the two things the copilot keeps from the old draft:
//! the positions of nodes that still exist, and their pins. Pure functions on JSON, no database.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use utoipa::ToSchema;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Diff {
    /// Node ids that are new.
    pub added: Vec<String>,
    /// Node ids that are gone.
    pub removed: Vec<String>,
    /// Node ids in both drafts whose `type`, `params`, `on_error` or another field changed (a moved node is not a change).
    pub changed: Vec<String>,
    pub edges_added: usize,
    pub edges_removed: usize,
    /// Top-level fields that changed (`name`, `description`, `variables`, `settings`, `notes`).
    pub fields_changed: Vec<String>,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty() && self.edges_added == 0 && self.edges_removed == 0 && self.fields_changed.is_empty()
    }
}

fn nodes_by_id(def: &Value) -> BTreeMap<String, &Value> {
    def.get("nodes").and_then(Value::as_array).map(|a| a.iter().filter_map(|n| n.get("id").and_then(Value::as_str).map(|id| (id.to_string(), n))).collect()).unwrap_or_default()
}

fn edge_keys(def: &Value) -> BTreeSet<(String, String, String)> {
    def.get("edges")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    let from = e.get("from")?.as_str()?;
                    let to = e.get("to")?.as_str()?;
                    let port = e.get("port").and_then(Value::as_str).unwrap_or("main");
                    Some((from.to_string(), port.to_string(), to.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn without_position(n: &Value) -> Value {
    let mut n = n.clone();
    if let Some(o) = n.as_object_mut() {
        o.remove("position");
    }
    n
}

/// Compare by node id. Nodes keep the order of the new draft in `added` and `changed`.
pub fn diff(old: &Value, new: &Value) -> Diff {
    let (a, b) = (nodes_by_id(old), nodes_by_id(new));
    let order = |def: &Value| -> Vec<String> { def.get("nodes").and_then(Value::as_array).map(|n| n.iter().filter_map(|x| x.get("id").and_then(Value::as_str).map(str::to_string)).collect()).unwrap_or_default() };
    let added = order(new).into_iter().filter(|id| !a.contains_key(id)).collect();
    let removed = order(old).into_iter().filter(|id| !b.contains_key(id)).collect();
    let changed = order(new).into_iter().filter(|id| a.get(id).is_some_and(|o| without_position(o) != without_position(b[id]))).collect();
    let (ea, eb) = (edge_keys(old), edge_keys(new));
    let fields_changed = ["name", "description", "variables", "settings", "notes"].into_iter().filter(|k| old.get(*k).filter(|v| !is_empty(v)) != new.get(*k).filter(|v| !is_empty(v))).map(str::to_string).collect();
    Diff { added, removed, changed, edges_added: eb.difference(&ea).count(), edges_removed: ea.difference(&eb).count(), fields_changed }
}

fn is_empty(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

/// A draft with nothing in it, the "old" side of a diff for a new workflow.
pub fn empty_draft() -> Value {
    json!({ "nodes": [], "edges": [] })
}

const STEP_X: f64 = 280.0;
const STEP_Y: f64 = 140.0;

fn pos_of(n: &Value) -> Option<(f64, f64)> {
    let p = n.get("position")?;
    Some((p.get("x")?.as_f64()?, p.get("y")?.as_f64()?))
}

fn free_spot(placed: &[(f64, f64)], x: f64, mut y: f64) -> f64 {
    for _ in 0..200 {
        if !placed.iter().any(|(px, py)| (px - x).abs() < 200.0 && (py - y).abs() < 100.0) {
            break;
        }
        y += STEP_Y;
    }
    y
}

/// Make `new` keep what the user built in `old`:
/// - a node that is in both keeps the old `position`;
/// - a new node gets a position next to a parent that already has one (right of it, in a free spot);
/// - `pins` are the old pins of nodes that still exist (pins the model wrote are dropped: test data is not the model's to make).
pub fn keep_layout(old: &Value, new: &mut Value) {
    let old_pos: BTreeMap<String, Value> = old
        .get("nodes")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|n| Some((n.get("id")?.as_str()?.to_string(), n.get("position").filter(|_| pos_of(n).is_some())?.clone()))).collect())
        .unwrap_or_default();
    let parents: Vec<(String, String)> = edge_keys(new).into_iter().map(|(f, _, t)| (f, t)).collect();
    let Some(nodes) = new.get_mut("nodes").and_then(Value::as_array_mut) else { return };

    let mut placed: Vec<(f64, f64)> = vec![];
    let mut at: BTreeMap<String, (f64, f64)> = BTreeMap::new();
    for n in nodes.iter_mut() {
        let Some(id) = n.get("id").and_then(Value::as_str).map(str::to_string) else { continue };
        if let Some(p) = old_pos.get(&id) {
            n["position"] = p.clone();
            let xy = pos_of(n).unwrap_or((0.0, 0.0));
            placed.push(xy);
            at.insert(id, xy);
        } else if let Some(o) = n.as_object_mut() {
            o.remove("position");
        }
    }
    loop {
        let mut progress = false;
        for n in nodes.iter_mut() {
            let Some(id) = n.get("id").and_then(Value::as_str).map(str::to_string) else { continue };
            if at.contains_key(&id) {
                continue;
            }
            if let Some(&(px, py)) = parents.iter().filter(|(_, t)| *t == id).find_map(|(f, _)| at.get(f)) {
                let (x, y) = (px + STEP_X, free_spot(&placed, px + STEP_X, py));
                n["position"] = json!({ "x": x, "y": y });
                placed.push((x, y));
                at.insert(id, (x, y));
                progress = true;
            }
        }
        if progress {
            continue;
        }
        // nothing left that has a placed parent: put the next node without a position as a root, below everything
        let Some(n) = nodes.iter_mut().find(|n| n.get("id").and_then(Value::as_str).is_some_and(|id| !at.contains_key(id))) else { break };
        let id = n["id"].as_str().unwrap_or_default().to_string();
        let y = if placed.is_empty() { 0.0 } else { placed.iter().map(|p| p.1).fold(f64::MIN, f64::max) + STEP_Y };
        n["position"] = json!({ "x": 0.0, "y": y });
        placed.push((0.0, y));
        at.insert(id, (0.0, y));
    }

    let ids: BTreeSet<String> = nodes.iter().filter_map(|n| n.get("id").and_then(Value::as_str).map(str::to_string)).collect();
    let pins: Map<String, Value> = old.get("pins").and_then(Value::as_object).map(|p| p.iter().filter(|(k, _)| ids.contains(*k)).map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
    if let Some(o) = new.as_object_mut() {
        if pins.is_empty() {
            o.remove("pins");
        } else {
            o.insert("pins".into(), Value::Object(pins));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wf(nodes: Value, edges: Value) -> Value {
        json!({ "format": "aikido-workflow", "version": 1, "name": "T", "nodes": nodes, "edges": edges })
    }

    #[test]
    fn diff_lists_added_removed_changed_and_edges() {
        let old = wf(
            json!([{ "id": "a", "type": "trigger.manual", "position": { "x": 0, "y": 0 } }, { "id": "b", "type": "flow.noop" }, { "id": "c", "type": "flow.noop", "params": { "x": 1 } }]),
            json!([{ "from": "a", "to": "b" }, { "from": "b", "to": "c" }]),
        );
        let new = wf(
            json!([{ "id": "a", "type": "trigger.manual", "position": { "x": 50, "y": 9 } }, { "id": "c", "type": "flow.noop", "params": { "x": 2 } }, { "id": "d", "type": "flow.noop" }]),
            json!([{ "from": "a", "port": "main", "to": "c" }, { "from": "c", "to": "d" }]),
        );
        let d = diff(&old, &new);
        assert_eq!(d.added, vec!["d"]);
        assert_eq!(d.removed, vec!["b"]);
        assert_eq!(d.changed, vec!["c"], "a moved node is not changed");
        assert_eq!((d.edges_added, d.edges_removed), (2, 2));
        assert!(!d.is_empty());
        assert!(diff(&new, &new).is_empty());
    }

    #[test]
    fn a_default_port_is_main() {
        let old = wf(json!([{ "id": "a", "type": "x" }, { "id": "b", "type": "x" }]), json!([{ "from": "a", "to": "b" }]));
        let new = wf(json!([{ "id": "a", "type": "x" }, { "id": "b", "type": "x" }]), json!([{ "from": "a", "port": "main", "to": "b" }]));
        let d = diff(&old, &new);
        assert_eq!((d.edges_added, d.edges_removed), (0, 0));
    }

    #[test]
    fn top_level_changes_are_listed() {
        let old = wf(json!([]), json!([]));
        let mut new = old.clone();
        new["name"] = json!("Other");
        new["variables"] = json!([{ "key": "k" }]);
        assert_eq!(diff(&old, &new).fields_changed, vec!["name", "variables"]);
    }

    #[test]
    fn old_positions_and_pins_are_kept_and_new_nodes_go_next_to_their_parent() {
        let old = json!({
            "nodes": [{ "id": "a", "type": "trigger.manual", "position": { "x": 10, "y": 20 } }, { "id": "b", "type": "flow.noop", "position": { "x": 300, "y": 40 } }, { "id": "gone", "type": "flow.noop" }],
            "edges": [],
            "pins": { "a": [{ "k": 1 }], "gone": [{ "k": 2 }] }
        });
        let mut new = wf(
            json!([
                { "id": "a", "type": "trigger.manual", "position": { "x": 999, "y": 999 } },
                { "id": "b", "type": "flow.noop" },
                { "id": "c", "type": "flow.noop", "position": { "x": 1, "y": 1 } },
                { "id": "d", "type": "flow.noop" }
            ]),
            json!([{ "from": "b", "to": "c" }, { "from": "b", "to": "d" }]),
        );
        new["pins"] = json!({ "c": [{ "made": "up" }] });
        keep_layout(&old, &mut new);
        let p = |id: &str| new["nodes"].as_array().unwrap().iter().find(|n| n["id"] == id).unwrap()["position"].clone();
        assert_eq!(p("a"), json!({ "x": 10, "y": 20 }));
        assert_eq!(p("b"), json!({ "x": 300, "y": 40 }));
        assert_eq!(p("c"), json!({ "x": 580.0, "y": 40.0 }), "right of the parent");
        assert_eq!(p("d"), json!({ "x": 580.0, "y": 180.0 }), "a second child goes below the first");
        assert_eq!(new["pins"], json!({ "a": [{ "k": 1 }] }));
    }

    #[test]
    fn a_new_workflow_is_laid_out_left_to_right() {
        let mut new = wf(json!([{ "id": "a", "type": "x" }, { "id": "b", "type": "x" }, { "id": "z", "type": "x" }]), json!([{ "from": "a", "to": "b" }]));
        keep_layout(&empty_draft(), &mut new);
        let p = |id: &str| new["nodes"].as_array().unwrap().iter().find(|n| n["id"] == id).unwrap()["position"].clone();
        assert_eq!(p("a"), json!({ "x": 0.0, "y": 0.0 }));
        assert_eq!(p("b"), json!({ "x": 280.0, "y": 0.0 }));
        assert_eq!(p("z"), json!({ "x": 0.0, "y": 140.0 }));
        assert!(new.get("pins").is_none());
    }
}
