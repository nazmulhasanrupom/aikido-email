//! Any OpenAI-compatible server: `POST {base_url}/chat/completions`. Covers OpenAI, OpenRouter, Ollama, LM Studio, vLLM.
//! The key is optional (a local Ollama needs none). JSON mode (`response_format`) is sent when the provider has `json_mode`.
use super::http::{build_client, post_json};
use super::{LlmClient, LlmError, LlmRequest, LlmResponse, ProviderConfig, RetryPolicy};
use async_trait::async_trait;
use secrecy::ExposeSecret;
use serde_json::{Value, json};

pub struct OpenAiClient {
    http: reqwest::Client,
    url: String,
    model: String,
    key: Option<secrecy::SecretString>,
    json_mode: bool,
    max_attempts: u32,
    retry: RetryPolicy,
}

impl OpenAiClient {
    pub fn new(cfg: &ProviderConfig, retry: RetryPolicy) -> Result<Self, LlmError> {
        if cfg.base_url.trim().is_empty() {
            return Err(LlmError::BadRequest("base_url is needed".into()));
        }
        Ok(Self {
            http: build_client(cfg.timeout_secs)?,
            url: format!("{}/chat/completions", cfg.base_url.trim_end_matches('/')),
            model: cfg.model.clone(),
            key: cfg.api_key.clone(),
            json_mode: cfg.json_mode,
            max_attempts: cfg.max_attempts,
            retry,
        })
    }

    fn body(&self, req: &LlmRequest) -> Value {
        let mut body = json!({
            "model": self.model,
            "messages": [{ "role": "system", "content": req.system }, { "role": "user", "content": req.user }],
            "max_tokens": req.max_output_tokens,
            "temperature": req.temperature,
        });
        if req.json && self.json_mode {
            body["response_format"] = json!({ "type": "json_object" });
        }
        body
    }
}

#[async_trait]
impl LlmClient for OpenAiClient {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let mut headers: Vec<(&str, String)> = Vec::new();
        if let Some(k) = &self.key {
            headers.push(("authorization", format!("Bearer {}", k.expose_secret())));
        }
        let v = post_json(&self.http, &self.url, &headers, &self.body(req), self.max_attempts, &self.retry).await?;
        parse_response(&v, &self.model)
    }
}

pub fn parse_response(v: &Value, fallback_model: &str) -> Result<LlmResponse, LlmError> {
    let choice = v.pointer("/choices/0").ok_or_else(|| LlmError::BadResponse("no choice in the answer".into()))?;
    let stop_reason = choice.get("finish_reason").and_then(|s| s.as_str()).map(String::from);
    if stop_reason.as_deref() == Some("content_filter") {
        return Err(LlmError::Refused);
    }
    let text = choice.pointer("/message/content").and_then(|c| c.as_str()).ok_or_else(|| LlmError::BadResponse("no text in the answer".into()))?.to_string();
    let n = |p: &str| v.pointer(p).and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    Ok(LlmResponse {
        text,
        input_tokens: n("/usage/prompt_tokens"),
        output_tokens: n("/usage/completion_tokens"),
        cached_tokens: n("/usage/prompt_tokens_details/cached_tokens"),
        model: v.get("model").and_then(|m| m.as_str()).unwrap_or(fallback_model).to_string(),
        stop_reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_text_and_tokens() {
        let v = json!({ "model": "gpt-x", "choices": [{ "finish_reason": "stop", "message": { "content": "hi" } }],
            "usage": { "prompt_tokens": 50, "completion_tokens": 4, "prompt_tokens_details": { "cached_tokens": 32 } } });
        let r = parse_response(&v, "m").unwrap();
        assert_eq!((r.text.as_str(), r.input_tokens, r.output_tokens, r.cached_tokens), ("hi", 50, 4, 32));
    }

    #[test]
    fn a_server_without_usage_gives_zero_tokens_and_a_bad_shape_fails() {
        let v = json!({ "choices": [{ "message": { "content": "hi" } }] });
        assert_eq!(parse_response(&v, "m").unwrap().input_tokens, 0);
        assert!(matches!(parse_response(&json!({ "choices": [] }), "m").unwrap_err(), LlmError::BadResponse(_)));
        assert_eq!(parse_response(&json!({ "choices": [{ "finish_reason": "content_filter", "message": { "content": "" } }] }), "m").unwrap_err(), LlmError::Refused);
    }
}
