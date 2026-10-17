//! The shared HTTP part of the real clients: one POST with retries.
use super::{LlmError, RetryPolicy};
use serde_json::Value;
use std::time::Duration;

pub fn build_client(timeout_secs: u64) -> Result<reqwest::Client, LlmError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs.max(1)))
        .build()
        .map_err(|_| LlmError::BadRequest("the HTTP client could not be built".into()))
}

fn short(text: &str) -> String {
    text.chars().take(200).collect()
}

/// The `error.message` of a provider answer, cut short. (It may echo part of the request, so it is shown to
/// the caller of the API but never logged.)
fn error_message(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.pointer("/error/message").and_then(|m| m.as_str()).map(short))
        .unwrap_or_else(|| "the provider refused the request".into())
}

/// POST a JSON body. Retries 408, 429, 5xx, timeouts and connection errors up to `max_attempts` times.
/// 401 and 403 give `Auth`. Other 4xx give `BadRequest`. A 2xx answer is returned as JSON.
pub async fn post_json(
    http: &reqwest::Client,
    url: &str,
    headers: &[(&str, String)],
    body: &Value,
    max_attempts: u32,
    policy: &RetryPolicy,
) -> Result<Value, LlmError> {
    let attempts = max_attempts.max(1);
    let payload = body.to_string();
    let mut last = LlmError::Transient { code: "network", retry_after: None };
    for attempt in 1..=attempts {
        let mut req = http.post(url).header(reqwest::header::CONTENT_TYPE, "application/json").body(payload.clone());
        for (k, v) in headers {
            req = req.header(*k, v.as_str());
        }
        let outcome: Result<Value, LlmError> = match req.send().await {
            Err(e) => Err(LlmError::Transient { code: if e.is_timeout() { "timeout" } else { "network" }, retry_after: None }),
            Ok(resp) => {
                let status = resp.status().as_u16();
                let retry_after = crate::sending::senders::gmail::retry_after(resp.headers());
                let text = resp.text().await.unwrap_or_default();
                match status {
                    200..=299 => serde_json::from_str::<Value>(&text).map_err(|_| LlmError::BadResponse("the answer is not JSON".into())),
                    401 | 403 => Err(LlmError::Auth),
                    408 | 429 => Err(LlmError::Transient { code: if status == 429 { "rate_limited" } else { "timeout" }, retry_after }),
                    500..=599 => Err(LlmError::Transient { code: "server_error", retry_after }),
                    _ => Err(LlmError::BadRequest(error_message(&text))),
                }
            }
        };
        match outcome {
            Ok(v) => return Ok(v),
            Err(LlmError::Transient { code, retry_after }) => {
                last = LlmError::Transient { code, retry_after };
                if attempt < attempts {
                    let backoff = policy.base_delay.saturating_mul(1 << (attempt - 1).min(10));
                    let wait = retry_after.unwrap_or(backoff).min(policy.max_delay);
                    tokio::time::sleep(wait).await;
                }
            }
            Err(other) => return Err(other),
        }
    }
    Err(last)
}
