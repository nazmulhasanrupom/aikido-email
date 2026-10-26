//! Which nodes a run visits and in which order (build map section 8.3).
use crate::automations::definition::WorkflowDefinition;
use std::collections::{HashMap, HashSet};

pub struct Plan<'a> {
    /// Node ids in the order they run.
    pub order: Vec<&'a str>,
    /// For each node: its input edges `(from, port)`, in the order of the edge list. Only edges from visited nodes.
    pub incoming: HashMap<&'a str, Vec<(&'a str, &'a str)>>,
}

/// The nodes a trigger can reach, the trigger included.
pub fn reachable(def: &WorkflowDefinition, trigger: &str) -> HashSet<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut stack = vec![trigger.to_string()];
    while let Some(n) = stack.pop() {
        if !seen.insert(n.clone()) {
            continue;
        }
        stack.extend(def.edges.iter().filter(|e| e.from == n).map(|e| e.to.clone()));
    }
    seen
}

/// The nodes that lead to `target`, the target included.
pub fn ancestors(def: &WorkflowDefinition, target: &str) -> HashSet<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut stack = vec![target.to_string()];
    while let Some(n) = stack.pop() {
        if !seen.insert(n.clone()) {
            continue;
        }
        stack.extend(def.edges.iter().filter(|e| e.to == n).map(|e| e.from.clone()));
    }
    seen
}

/// Plan a run from `trigger`. With `only_node`, the plan is the path from the trigger to that node.
pub fn plan<'a>(def: &'a WorkflowDefinition, trigger: &str, only_node: Option<&str>) -> Result<Plan<'a>, String> {
    if def.node(trigger).is_none() {
        return Err(format!("There is no node `{trigger}`."));
    }
    let mut keep = reachable(def, trigger);
    if let Some(target) = only_node {
        if !keep.contains(target) {
            return Err(format!("The trigger `{trigger}` does not reach the node `{target}`."));
        }
        let up = ancestors(def, target);
        keep.retain(|n| up.contains(n));
    }
    let nodes: Vec<&'a str> = def.nodes.iter().map(|n| n.id.as_str()).filter(|id| keep.contains(*id)).collect();
    let mut incoming: HashMap<&'a str, Vec<(&'a str, &'a str)>> = HashMap::new();
    for e in &def.edges {
        if keep.contains(&e.from) && keep.contains(&e.to) {
            incoming.entry(e.to.as_str()).or_default().push((e.from.as_str(), e.port()));
        }
    }
    // Kahn: always take the first ready node of the file, so the order is stable.
    let mut order: Vec<&'a str> = Vec::new();
    let mut placed: HashSet<&str> = HashSet::new();
    while order.len() < nodes.len() {
        let next = nodes
            .iter()
            .find(|id| !placed.contains(**id) && incoming.get(**id).is_none_or(|ins| ins.iter().all(|(f, _)| placed.contains(f))))
            .copied();
        match next {
            Some(id) => {
                placed.insert(id);
                order.push(id);
            }
            None => return Err("The workflow has a cycle.".into()),
        }
    }
    Ok(Plan { order, incoming })
}
