//! The engine (build map section 8): it runs a workflow, node by node, with items moving along the edges.
//!
//! - `state.rs`: what a run remembers between nodes (the checkpoint)
//! - `graph.rs`: which nodes a run visits, and in which order
//! - `env.rs`: what nodes that talk to the outside may use (internal caller, fetcher, secrets, settings)
//! - `eval.rs`: reads `${{ }}` for one item (paired reads of earlier nodes)
//! - `nodes.rs`: the manual trigger, the flow nodes and the data nodes
//! - `aikido_nodes.rs`: the AIKIDO nodes and `aikido.api` (calls to the app's own routes)
//! - `ai_nodes.rs`, `ai_backend.rs`: the AI Agent node and what it needs (providers, token budget, `llm_calls`)
//! - `ai_tools.rs`: tools of the AI Agent (the agent calls chosen nodes; a JSON protocol on the one LLM call shape)
//! - `code_node.rs`: the code node `code.js` (QuickJS, 1 s, 32 MB, no network)
//! - `sub_workflow.rs`: `flow.execute_workflow` (a workflow that runs another one) and how the sub-workflow is loaded
//! - `error_workflow.rs`: the workflow named in `settings.error_workflow_id` starts when a production run fails
//! - `web_nodes.rs`: `http.request` and `web.fetch_page`
//! - `run.rs`: the executor (retry, `on_error`, batch, timeout, cancel, Wait, pins, dry runs)
//! - `store.rs`: where an executor writes progress (`DbStore` for Postgres, `MemStore` for tests)
//! - `worker.rs`: claims queued runs with a lease, resumes waiting runs, cleans old runs
//! - `routes.rs`: test runs, run lists, run events, cancel, engine status and pause
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub mod ai_backend;
pub mod ai_nodes;
pub mod ai_tools;
pub mod aikido_nodes;
pub mod code_node;
pub mod env;
pub mod eval;
pub mod graph;
pub mod nodes;
pub mod error_workflow;
pub mod routes;
pub mod run;
pub mod state;
pub mod store;
pub mod sub_workflow;
pub mod web_nodes;
pub mod worker;

#[cfg(test)]
mod node_tests;
#[cfg(test)]
mod tests;

/// Tells a running run to stop. The worker sets the flags; the executor and the nodes look at them.
#[derive(Default)]
pub struct Control {
    cancelled: AtomicBool,
    lost: AtomicBool,
}

impl Control {
    pub fn new() -> Self {
        Self::default()
    }
    /// Someone pressed Cancel.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
    /// This server copy lost the lease: another copy may run the run now.
    pub fn lose(&self) {
        self.lost.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }
    pub fn should_stop(&self) -> bool {
        self.is_cancelled() || self.is_lost()
    }

    /// Sleep, but wake up early when the run must stop.
    pub async fn sleep(&self, d: Duration) {
        let end = tokio::time::Instant::now() + d;
        loop {
            if self.should_stop() {
                return;
            }
            let left = end.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return;
            }
            tokio::time::sleep(left.min(Duration::from_millis(200))).await;
        }
    }
}
