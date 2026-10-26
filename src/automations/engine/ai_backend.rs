//! What the AI Agent node needs from the server (build map 7.3 and 18.11): find a provider, check the daily
//! token budget, write the `llm_calls` row. The node talks to this trait, so unit tests use a fake with
//! `generator::llm::fake::FakeLlm` and no database.
//!
//! There is one LLM client in the app (`generator::llm`). This file does not add a second one: `DbAi` loads the
//! provider with `generator::providers::load_config` and builds the client with the app's `LlmFactory`.
use crate::automations::registry::NodeError;
use crate::generator::llm::LlmClient;
use crate::generator::{providers, repo};
use crate::state::AppState;
use async_trait::async_trait;
use std::sync::Arc;
use uuid::Uuid;

/// A provider that can be called now.
#[derive(Clone)]
pub struct AiProvider {
    pub id: Uuid,
    pub model: String,
    pub client: Arc<dyn LlmClient>,
}

/// One LLM call, for `llm_calls`. Never holds a prompt or an answer.
#[derive(Debug, Clone, Default)]
pub struct AiCall {
    pub provider_id: Option<Uuid>,
    pub model: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_tokens: u32,
    pub duration_ms: u32,
    pub ok: bool,
    pub error_code: Option<String>,
    pub run_id: String,
    pub workflow_id: String,
    pub node_id: String,
}

#[async_trait]
pub trait AiBackend: Send + Sync {
    /// `default_draft`, `default_slot`, a provider name, or a provider id.
    async fn provider(&self, which: &str) -> Result<AiProvider, NodeError>;
    /// Is today's token budget of the generator used up? (It is shared with the generator.)
    async fn budget_reached(&self) -> Result<bool, NodeError>;
    /// Write the `llm_calls` row. A failure is logged, it never fails the node.
    async fn record(&self, call: AiCall);
    /// The provider answered 401 or 403.
    async fn auth_failed(&self, provider_id: Uuid);
}

/// The real backend: the database and the generator's provider store.
pub struct DbAi {
    pub state: AppState,
}

fn db(e: sqlx::Error) -> NodeError {
    tracing::error!(error = %e, "database error in the AI Agent node");
    NodeError::new("internal_error", "A database error stopped the AI Agent node.")
}

#[async_trait]
impl AiBackend for DbAi {
    async fn provider(&self, which: &str) -> Result<AiProvider, NodeError> {
        let st = &self.state;
        let mut conn = st.pool.acquire().await.map_err(db)?;
        let settings = repo::load_settings(&mut conn).await.map_err(db)?;
        let (id, role) = match which {
            "default_draft" => (settings.draft_provider_id, "draft provider"),
            "default_slot" => (settings.slot_provider_id, "slot provider"),
            other => {
                let found = match Uuid::parse_str(other) {
                    Ok(id) => Some(id),
                    Err(_) => sqlx::query_scalar("SELECT id FROM llm_providers WHERE lower(name) = lower($1) ORDER BY created_at LIMIT 1")
                        .bind(other)
                        .fetch_optional(&mut *conn)
                        .await
                        .map_err(db)?,
                };
                match found {
                    Some(id) => (Some(id), "provider"),
                    None => return Err(NodeError::new("ai_provider_not_found", format!("There is no AI provider named `{other}`."))),
                }
            }
        };
        drop(conn);
        let Some(id) = id else {
            return Err(NodeError::new("ai_not_configured", format!("No {role} is set. Add a provider and set it in Settings > AI.")));
        };
        match providers::load_config(&st.pool, &st.crypto, id, settings.max_attempts).await {
            Ok((cfg, status)) => {
                if status == "auth_failed" {
                    return Err(NodeError::new("ai_provider_auth_failed", "The AI provider refused its API key. Replace the key, or test the provider again."));
                }
                let client = st.llm.build(&cfg).map_err(|_| NodeError::new("ai_not_configured", "The AI provider is not set up right (check its base_url)."))?;
                Ok(AiProvider { id, model: cfg.model, client })
            }
            Err(providers::ConfigError::NotFound) => Err(NodeError::new("ai_provider_not_found", "The AI provider does not exist any more.")),
            Err(providers::ConfigError::BadKey) => Err(NodeError::new("ai_not_configured", "The API key of the AI provider can not be read. Save the key again.")),
            Err(providers::ConfigError::Db(e)) => Err(db(e)),
        }
    }

    async fn budget_reached(&self) -> Result<bool, NodeError> {
        let mut conn = self.state.pool.acquire().await.map_err(db)?;
        let settings = repo::load_settings(&mut conn).await.map_err(db)?;
        if settings.daily_token_budget == 0 {
            return Ok(false);
        }
        let used = repo::tokens_today(&mut conn, self.state.now()).await.map_err(db)?;
        Ok(used as u64 >= settings.daily_token_budget)
    }

    async fn record(&self, c: AiCall) {
        let r = sqlx::query(
            "INSERT INTO llm_calls (purpose, provider_id, model, input_tokens, output_tokens, cached_tokens, duration_ms, ok, error_code, created_at, automation_run_id, workflow_id, node_id)
             VALUES ('automation', $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(c.provider_id)
        .bind(&c.model)
        .bind(c.input_tokens as i32)
        .bind(c.output_tokens as i32)
        .bind(c.cached_tokens as i32)
        .bind(c.duration_ms as i32)
        .bind(c.ok)
        .bind(&c.error_code)
        .bind(self.state.now())
        .bind(Uuid::parse_str(&c.run_id).ok())
        .bind(Uuid::parse_str(&c.workflow_id).ok())
        .bind(&c.node_id)
        .execute(&self.state.pool)
        .await;
        if let Err(e) = r {
            tracing::error!(error = %e, "could not write the llm_calls row of an AI Agent call");
        }
    }

    async fn auth_failed(&self, provider_id: Uuid) {
        providers::mark_auth_failed(&self.state.pool, provider_id, self.state.now()).await;
    }
}
