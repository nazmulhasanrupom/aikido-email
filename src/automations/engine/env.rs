//! What a node that talks to the outside may use while it runs: the internal caller (the app's own routes),
//! the safe fetcher, the secrets, the settings and the user the run acts as. Pure nodes (flow, data) ignore it.
use super::ai_backend::AiBackend;
use super::sub_workflow::WorkflowLoader;
use crate::automations::internal_call::{Internal, OperationIndex, Who};
use crate::automations::registry::NodeError;
use crate::automations::secrets::Secrets;
use crate::automations::settings::AutomationSettings;
use crate::net::safe_fetch::SafeFetcher;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU32, Ordering};

/// The most outside calls one run may make (build map 11.4).
pub const MAX_CALLS_PER_RUN: u32 = 2000;

pub struct NodeEnv {
    pub internal: Arc<dyn Internal>,
    pub fetcher: Arc<SafeFetcher>,
    pub secrets: Arc<Secrets>,
    pub settings: AutomationSettings,
    pub ops: Arc<OperationIndex>,
    pub who: Who,
    /// The AI providers and the token budget, for the AI Agent node. `None` in tests of other nodes.
    pub ai: Option<Arc<dyn AiBackend>>,
    /// The web address of the app, for the user agent of Fetch web page.
    pub public_url: Option<String>,
    /// A test run: nodes add a warning to the log when they send something out for real.
    pub test_run: bool,
    /// The robots.txt of each host, read once per run.
    pub robots: Mutex<HashMap<String, Arc<crate::automations::robots::Robots>>>,
    /// Where `flow.execute_workflow` finds the workflow to run. `None` in tests of other nodes.
    pub workflows: Option<Arc<dyn WorkflowLoader>>,
    /// How many workflows are running above this one (0 for a workflow started by a trigger or a test).
    pub depth: u32,
    /// The ids of the workflows above this one and of this one, outermost first. Empty at depth 0: the node adds its own id.
    pub chain: Vec<String>,
    /// Shared with the sub-workflows, so `MAX_CALLS_PER_RUN` holds for the whole run.
    calls: Arc<AtomicU32>,
}

impl NodeEnv {
    pub fn new(
        internal: Arc<dyn Internal>,
        fetcher: Arc<SafeFetcher>,
        secrets: Arc<Secrets>,
        settings: AutomationSettings,
        ops: Arc<OperationIndex>,
        who: Who,
        public_url: Option<String>,
    ) -> Self {
        NodeEnv { internal, fetcher, secrets, settings, ops, who, ai: None, public_url, test_run: false, robots: Mutex::new(HashMap::new()), workflows: None, depth: 0, chain: vec![], calls: Arc::new(AtomicU32::new(0)) }
    }

    /// The services for a sub-workflow one level down: the same server services, the secrets of the sub-workflow,
    /// one counter of outside calls for the whole run.
    pub fn child(&self, secrets: Arc<Secrets>, chain: Vec<String>) -> NodeEnv {
        NodeEnv {
            internal: self.internal.clone(),
            fetcher: self.fetcher.clone(),
            secrets,
            settings: self.settings.clone(),
            ops: self.ops.clone(),
            who: self.who.clone(),
            ai: self.ai.clone(),
            public_url: self.public_url.clone(),
            test_run: self.test_run,
            robots: Mutex::new(HashMap::new()),
            workflows: self.workflows.clone(),
            depth: self.depth + 1,
            chain,
            calls: self.calls.clone(),
        }
    }

    /// Count one outside call. Over the limit: `too_many_calls`.
    pub fn count_call(&self) -> Result<(), NodeError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) >= MAX_CALLS_PER_RUN {
            return Err(NodeError::new("too_many_calls", format!("This run made more than {MAX_CALLS_PER_RUN} outside calls.")));
        }
        Ok(())
    }

    pub fn calls_made(&self) -> u32 {
        self.calls.load(Ordering::SeqCst)
    }
}
