//! One trait, three kinds of LLM client: `anthropic`, `openai_compatible` (OpenAI, OpenRouter, Ollama, LM Studio,
//! vLLM), and `fake` (tests). Plain `reqwest`, no provider SDK crates.
//!
//! The client retries rate limits (429), server errors (5xx) and timeouts by itself, with a back-off that
//! respects `Retry-After`. The caller gets the last error when the tries run out.
//! No key and no prompt text are ever written to a log from here.
pub mod anthropic;
pub mod fake;
pub mod http;
pub mod openai;

use async_trait::async_trait;
use secrecy::SecretString;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct LlmRequest {
    /// The fixed part: rules, the step text, the slots. The same for every lead of a step, so the provider can cache it.
    pub system: String,
    /// The part that changes per lead.
    pub user: String,
    pub max_output_tokens: u32,
    pub json: bool,
    pub temperature: f64,
}

#[derive(Clone, Debug, Default)]
pub struct LlmResponse {
    pub text: String,
    /// All input tokens, including the cached ones.
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Input tokens that were read from the provider cache.
    pub cached_tokens: u32,
    pub model: String,
    pub stop_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LlmError {
    /// 429, 5xx, timeout, connection error. Worth another try later.
    Transient { code: &'static str, retry_after: Option<Duration> },
    /// 401 or 403: the key is wrong or revoked.
    Auth,
    /// 400 and other 4xx: this request will never work (too long, bad model name, ...).
    BadRequest(String),
    /// The model declined (safety).
    Refused,
    /// The answer has a shape we do not know.
    BadResponse(String),
}

impl LlmError {
    /// A short code for the logs, `llm_calls.error_code` and `generations.error_code`.
    pub fn code(&self) -> String {
        match self {
            LlmError::Transient { code, .. } => (*code).to_string(),
            LlmError::Auth => "provider_auth".into(),
            LlmError::BadRequest(_) => "bad_request".into(),
            LlmError::Refused => "refused".into(),
            LlmError::BadResponse(_) => "bad_response".into(),
        }
    }
}

#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    Anthropic,
    OpenAiCompatible,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::OpenAiCompatible => "openai_compatible",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "anthropic" => Some(ProviderKind::Anthropic),
            "openai_compatible" => Some(ProviderKind::OpenAiCompatible),
            _ => None,
        }
    }
}

/// Everything needed to talk to one provider. The key is decrypted just before the call.
#[derive(Clone)]
pub struct ProviderConfig {
    pub id: Uuid,
    pub kind: ProviderKind,
    pub base_url: String,
    pub model: String,
    pub api_key: Option<SecretString>,
    pub timeout_secs: u64,
    pub json_mode: bool,
    /// Tries for one call.
    pub max_attempts: u32,
}

impl std::fmt::Debug for ProviderConfig {
    // No key in a debug print.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderConfig").field("id", &self.id).field("kind", &self.kind).field("model", &self.model).finish_non_exhaustive()
    }
}

/// Makes the client of a provider. Tests replace the real one with `fake::FakeFactory`.
pub trait LlmFactory: Send + Sync {
    fn build(&self, cfg: &ProviderConfig) -> Result<Arc<dyn LlmClient>, LlmError>;
}

#[derive(Clone, Debug)]
pub struct RetryPolicy {
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self { base_delay: Duration::from_secs(1), max_delay: Duration::from_secs(30) }
    }
}

#[derive(Default)]
pub struct RealLlmFactory {
    pub retry: RetryPolicy,
}

impl RealLlmFactory {
    /// Short waits, for tests that run against a mock server.
    pub fn with_retry(retry: RetryPolicy) -> Self {
        Self { retry }
    }
}

impl LlmFactory for RealLlmFactory {
    fn build(&self, cfg: &ProviderConfig) -> Result<Arc<dyn LlmClient>, LlmError> {
        Ok(match cfg.kind {
            ProviderKind::Anthropic => Arc::new(anthropic::AnthropicClient::new(cfg, self.retry.clone())?),
            ProviderKind::OpenAiCompatible => Arc::new(openai::OpenAiClient::new(cfg, self.retry.clone())?),
        })
    }
}
