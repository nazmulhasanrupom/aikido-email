//! Where an executor writes its progress. `DbStore` (in `worker.rs`) writes to Postgres.
//! `MemStore` keeps everything in memory, for tests of the executor without a database.
use super::state::RunState;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeStatus {
    Running,
    Success,
    Error,
    Skipped,
    /// A side-effect node in a test run: it did nothing and said what it would do.
    Dry,
    /// The node could not run yet (the AI token budget is used up). It runs again when the run wakes up.
    Waiting,
}

impl NodeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            NodeStatus::Running => "running",
            NodeStatus::Success => "success",
            NodeStatus::Error => "error",
            NodeStatus::Skipped => "skipped",
            NodeStatus::Dry => "dry",
            NodeStatus::Waiting => "waiting",
        }
    }
}

/// One execution of one node (a row of `automation_run_nodes`).
#[derive(Debug, Clone)]
pub struct NodeRecord {
    pub node_id: String,
    pub seq: u32,
    pub status: NodeStatus,
    pub input_count: usize,
    /// Items per output port.
    pub output_count: BTreeMap<String, usize>,
    /// `{ "<port>": [item json, ...] }`, cut at 512 KB. `None`: not kept (run setting `save_data`).
    pub output: Option<Value>,
    pub truncated: bool,
    pub error: Option<Value>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StoreError {
    /// Another server copy owns the run now. Stop without writing.
    LeaseLost,
    /// The checkpoint is bigger than 20 MB.
    StateTooLarge,
    Db(String),
}

impl From<sqlx::Error> for StoreError {
    fn from(e: sqlx::Error) -> Self {
        StoreError::Db(e.to_string())
    }
}

#[async_trait]
pub trait RunStore: Send + Sync {
    /// A node execution starts (shown as `running`).
    async fn node_started(&self, node_id: &str, seq: u32, at: DateTime<Utc>) -> Result<(), StoreError>;
    /// A node is done: its rows and the new checkpoint are saved together.
    async fn step_done(&self, records: &[NodeRecord], state: &RunState) -> Result<(), StoreError>;
    /// A line of the run log (`node_started`, `node_finished`, `node_error`, `log`, ...). Never fails the run.
    async fn event(&self, kind: &str, node_id: Option<&str>, detail: Value);
}

/// An in-memory store. `abort_after_steps` makes it fail like a dead server after that many saved steps.
#[derive(Default)]
pub struct MemStore {
    pub records: Mutex<Vec<NodeRecord>>,
    pub events: Mutex<Vec<(String, Option<String>, Value)>>,
    /// The last saved checkpoint, packed like the database does.
    pub saved_state: Mutex<Option<Vec<u8>>>,
    pub steps: Mutex<usize>,
    pub abort_after_steps: Mutex<Option<usize>>,
}

impl MemStore {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn records_of(&self, node_id: &str) -> Vec<NodeRecord> {
        self.records.lock().unwrap().iter().filter(|r| r.node_id == node_id).cloned().collect()
    }
    pub fn event_kinds(&self) -> Vec<String> {
        self.events.lock().unwrap().iter().map(|e| e.0.clone()).collect()
    }
    pub fn state(&self) -> Option<RunState> {
        self.saved_state.lock().unwrap().as_ref().map(|b| RunState::decode(b).expect("the saved state can be read"))
    }
}

#[async_trait]
impl RunStore for MemStore {
    async fn node_started(&self, _node_id: &str, _seq: u32, _at: DateTime<Utc>) -> Result<(), StoreError> {
        Ok(())
    }

    async fn step_done(&self, records: &[NodeRecord], state: &RunState) -> Result<(), StoreError> {
        let bytes = state.encode().map_err(|e| if e == "run_state_too_large" { StoreError::StateTooLarge } else { StoreError::Db(e) })?;
        let mut steps = self.steps.lock().unwrap();
        if self.abort_after_steps.lock().unwrap().is_some_and(|n| *steps >= n) {
            return Err(StoreError::LeaseLost);
        }
        *steps += 1;
        self.records.lock().unwrap().extend(records.iter().cloned());
        *self.saved_state.lock().unwrap() = Some(bytes);
        Ok(())
    }

    async fn event(&self, kind: &str, node_id: Option<&str>, detail: Value) {
        self.events.lock().unwrap().push((kind.to_string(), node_id.map(str::to_string), detail));
    }
}
