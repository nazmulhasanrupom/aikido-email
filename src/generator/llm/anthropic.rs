//! Anthropic Messages API (`POST {base_url}/v1/messages`).
//!
//! The fixed part of the prompt is the `system` block with `cache_control`, so the provider can cache it for the
//! next leads of the same step. Models of the newer families (Opus/Sonnet/Haiku 5.x, Fable, Mythos, Opus 4.7/4.8)
//! refuse a custom `temperature`, and they think by default. For those the client leaves `temperature` out, asks for
//! `effort: low`, and adds head room to `max_tokens`, so thinking does not eat the answer.
use super::http::{build_client, post_json};
use super::{LlmClient, LlmError, LlmRequest, LlmResponse, ProviderConfig, RetryPolicy};
use async_trait::async_trait;
use secrecy::ExposeSecret;
use serde_json::{Value, json};

pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const VERSION: &str = "2023-06-01";
/// Extra output tokens for thinking, on the models that think by default.
const THINKING_ROOM: u32 = 1024;

pub struct AnthropicClient {
    http: reqwest::Client,
    url: String,
    model: String,
    key: Option<secrecy::SecretString>,
    max_attempts: u32,
    retry: RetryPolicy,
}

/// Models that take no `temperature`, think by default and know `output_config.effort`.
pub fn is_new_family(model: &str) -> bool {
    ["claude-opus-5", "claude-sonnet-5", "claude-haiku-5", "claude-fable", "claude-mythos", "claude-opus-4-7", "claude-opus-4-8"]
        .iter()
        .any(|p| model.starts_with(p))
}

impl AnthropicClient {
    pub fn new(cfg: &ProviderConfig, retry: RetryPolicy) -> Result<Self, LlmError> {
        let base = if cfg.base_url.trim().is_empty() { DEFAULT_BASE_URL } else { cfg.base_url.trim_end_matches('/') };
        Ok(Self {
            http: build_client(cfg.timeout_secs)?,
            url: format!("{base}/v1/messages"),
            model: cfg.model.clone(),
            key: cfg.api_key.clone(),
            max_attempts: cfg.max_attempts,
            retry,
        })
    }

    fn body(&self, req: &LlmRequest) -> Value {
        let new = is_new_family(&self.model);
        let mut body = json!({
            "model": self.model,
            "max_tokens": req.max_output_tokens + if new { THINKING_ROOM } else { 0 },
            "system": [{ "type": "text", "text": req.system, "cache_control": { "type": "ephemeral" } }],
            "messages": [{ "role": "user", "content": req.user }],
        });
        if new {
            body["output_config"] = json!({ "effort": "low" });
        } else {
            body["temperature"] = json!(req.temperature.clamp(0.0, 1.0));
        }
        body
    }
}

#[async_trait]
impl LlmClient for AnthropicClient {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let mut headers = vec![("anthropic-version", VERSION.to_string())];
        if let Some(k) = &self.key {
            headers.push(("x-api-key", k.expose_secret().to_string()));
        }
        let v = post_json(&self.http, &self.url, &headers, &self.body(req), self.max_attempts, &self.retry).await?;
        parse_response(&v, &self.model)
    }
}

pub fn parse_response(v: &Value, fallback_model: &str) -> Result<LlmResponse, LlmError> {
    let stop_reason = v.get("stop_reason").and_then(|s| s.as_str()).map(String::from);
    if stop_reason.as_deref() == Some("refusal") {
        return Err(LlmError::Refused);
    }
    let text: String = v
        .get("content")
        .and_then(|c| c.as_array())
        .map(|blocks| blocks.iter().filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text")).filter_map(|b| b.get("text").and_then(|t| t.as_str())).collect::<Vec<_>>().join(""))
        .ok_or_else(|| LlmError::BadResponse("no content in the answer".into()))?;
    let n = |p: &str| v.pointer(p).and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    let cached = n("/usage/cache_read_input_tokens");
    // `input_tokens` leaves out the cached tokens. The total input is all three.
    let input = n("/usage/input_tokens") + cached + n("/usage/cache_creation_input_tokens");
    Ok(LlmResponse {
        text,
        input_tokens: input,
        output_tokens: n("/usage/output_tokens"),
        cached_tokens: cached,
        model: v.get("model").and_then(|m| m.as_str()).unwrap_or(fallback_model).to_string(),
        stop_reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_family_detection() {
        for m in ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5", "claude-fable-5-1", "claude-opus-4-8"] {
            assert!(is_new_family(m), "{m}");
        }
        for m in ["claude-haiku-4-5", "claude-sonnet-4-6", "claude-opus-4-6", "claude-3-5-sonnet-latest"] {
            assert!(!is_new_family(m), "{m}");
        }
    }

    #[test]
    fn reads_text_and_tokens_and_counts_the_cache() {
        let v = json!({ "model": "m", "stop_reason": "end_turn", "content": [{ "type": "thinking", "thinking": "" }, { "type": "text", "text": "{\"a\":1}" }],
            "usage": { "input_tokens": 10, "cache_read_input_tokens": 90, "cache_creation_input_tokens": 5, "output_tokens": 7 } });
        let r = parse_response(&v, "x").unwrap();
        assert_eq!(r.text, "{\"a\":1}");
        assert_eq!((r.input_tokens, r.cached_tokens, r.output_tokens), (105, 90, 7));
        assert_eq!(r.stop_reason.as_deref(), Some("end_turn"));
    }

    #[test]
    fn a_refusal_and_a_bad_shape_are_errors() {
        assert_eq!(parse_response(&json!({ "stop_reason": "refusal", "content": [] }), "x").unwrap_err(), LlmError::Refused);
        assert!(matches!(parse_response(&json!({ "nope": 1 }), "x").unwrap_err(), LlmError::BadResponse(_)));
    }
}
