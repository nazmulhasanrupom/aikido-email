//! What a run remembers between nodes. It is saved after each node (a checkpoint, section 8.4), so
//! another server copy can go on when the first one dies.
use crate::automations::registry::Item;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};

/// The biggest checkpoint (the JSON text before packing). More: the run fails with `run_state_too_large`.
pub const MAX_STATE_BYTES: usize = 20 * 1024 * 1024;

/// Everything one node gave, over all its executions.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeOut {
    /// All output items, in the order they were made. The lineage index of an item is its position here.
    pub items: Vec<Item>,
    /// Port name to the positions in `items`.
    pub ports: BTreeMap<String, Vec<usize>>,
}

impl NodeOut {
    pub fn port_items(&self, port: &str) -> Vec<Item> {
        self.ports.get(port).map(|ix| ix.iter().filter_map(|i| self.items.get(*i).cloned()).collect()).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunState {
    pub outputs: BTreeMap<String, NodeOut>,
    /// Nodes that are done (success, skipped, or failed and went on).
    pub finished: BTreeSet<String>,
    /// How many node executions were started. The next one gets this number.
    pub seq: u32,
    /// Time spent working, in ms. Time in a long Wait does not count.
    pub active_ms: u64,
    /// Since when the run waits for the daily AI token budget (build map 7.3). Cleared when a node finishes.
    #[serde(default)]
    pub budget_wait_since: Option<chrono::DateTime<chrono::Utc>>,
}

impl RunState {
    /// The items a node gave on a port. Empty when the node did not run.
    pub fn port_items(&self, node: &str, port: &str) -> Vec<Item> {
        self.outputs.get(node).map(|o| o.port_items(port)).unwrap_or_default()
    }

    /// JSON, packed with gzip. (The map says zstd; the project has no zstd, and gzip is already in the build.)
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        let raw = serde_json::to_vec(self).map_err(|e| e.to_string())?;
        if raw.len() > MAX_STATE_BYTES {
            return Err("run_state_too_large".into());
        }
        let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(&raw).map_err(|e| e.to_string())?;
        enc.finish().map_err(|e| e.to_string())
    }

    pub fn decode(bytes: &[u8]) -> Result<RunState, String> {
        let mut raw = Vec::new();
        GzDecoder::new(bytes).take(MAX_STATE_BYTES as u64 + 1).read_to_end(&mut raw).map_err(|e| e.to_string())?;
        serde_json::from_slice(&raw).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_state_survives_encode_and_decode() {
        let mut s = RunState::default();
        let item = Item { json: json!({ "a": 1 }).as_object().unwrap().clone(), lineage: vec![("start".into(), 0)] };
        s.outputs.insert("start".into(), NodeOut { items: vec![item], ports: BTreeMap::from([("main".to_string(), vec![0])]) });
        s.finished.insert("start".into());
        s.seq = 3;
        s.active_ms = 40;
        let back = RunState::decode(&s.encode().unwrap()).unwrap();
        assert_eq!(s, back);
        assert_eq!(back.port_items("start", "main").len(), 1);
        assert!(back.port_items("start", "false").is_empty());
        assert!(back.port_items("nope", "main").is_empty());
    }
}
