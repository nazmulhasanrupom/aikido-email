//! Gmail API sender: `POST users/me/messages/send` with the base64url MIME message.
//! Gmail says OK even for a dead address. The bounce comes back later as an email;
//! the Reply Check step must read it and write `hard_bounce` to `suppressions`.
use super::{MailSender, OutgoingMessage, SendError, SendReceipt, clip};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64URL};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use std::time::Duration;

pub const GMAIL_SEND_URL: &str = "https://gmail.googleapis.com/gmail/v1/users/me/messages/send";

pub const GMAIL_API_BASE: &str = "https://gmail.googleapis.com/gmail/v1/users/me";

pub struct GmailSender {
    access_token: SecretString,
    url: String,
    /// Where the sent message is read back (to check its `Message-ID`).
    read_base: String,
}

impl GmailSender {
    pub fn new(access_token: SecretString) -> Self {
        Self {
            access_token,
            url: std::env::var("GMAIL_SEND_URL").unwrap_or_else(|_| GMAIL_SEND_URL.into()),
            read_base: std::env::var("GMAIL_API_BASE_URL").unwrap_or_else(|_| GMAIL_API_BASE.into()),
        }
    }
    pub fn with_url(access_token: SecretString, url: String) -> Self {
        Self { access_token, url, read_base: std::env::var("GMAIL_API_BASE_URL").unwrap_or_else(|_| GMAIL_API_BASE.into()) }
    }
    pub fn with_read_base(mut self, base: String) -> Self {
        self.read_base = base;
        self
    }

    /// Gmail may replace our `Message-ID` with its own. Read the sent message back and return the real one.
    /// Best effort: it needs a read scope, and any failure just keeps the id we made.
    async fn real_message_id(&self, client: &reqwest::Client, gmail_id: &str, ours: &str) -> Option<String> {
        let mut url = oauth2::url::Url::parse(&format!("{}/messages/{gmail_id}", self.read_base.trim_end_matches('/'))).ok()?;
        url.query_pairs_mut().append_pair("format", "metadata").append_pair("metadataHeaders", "Message-ID");
        let resp = client.get(url).bearer_auth(self.access_token.expose_secret()).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let json: Value = serde_json::from_str(&resp.text().await.ok()?).ok()?;
        let real = json["payload"]["headers"]
            .as_array()?
            .iter()
            .find(|h| h["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case("message-id")))?["value"]
            .as_str()?
            .trim()
            .to_string();
        let real = if real.starts_with('<') { real } else { format!("<{real}>") };
        (real != ours).then_some(real)
    }
}

pub fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// A request error. Connect errors are safe to retry. Anything else may have reached the server.
pub fn map_request_error(e: reqwest::Error) -> SendError {
    let (connect, timeout) = (e.is_connect(), e.is_timeout());
    // Without the URL. With the reason chain, so the operator can see what went wrong.
    let e = e.without_url();
    let mut reason = e.to_string();
    let mut source = std::error::Error::source(&e);
    while let Some(s) = source {
        reason.push_str(&format!(": {s}"));
        source = s.source();
    }
    let reason = clip(&reason);
    if connect {
        SendError::Transient(reason)
    } else if timeout {
        SendError::Unknown(format!("request timed out ({reason})"))
    } else {
        SendError::Unknown(reason)
    }
}

fn error_reasons(body: &Value) -> Vec<String> {
    body["error"]["errors"]
        .as_array()
        .map(|a| a.iter().filter_map(|e| e["reason"].as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

pub fn map_response(status: u16, headers: &reqwest::header::HeaderMap, body: &str) -> Result<SendReceipt, SendError> {
    let json: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let message = json["error"]["message"].as_str().map(clip).unwrap_or_else(|| clip(body));
    let reasons = error_reasons(&json);
    let has = |r: &str| reasons.iter().any(|x| x == r);
    match status {
        200..=299 => Ok(SendReceipt {
            provider_message_id: json["id"].as_str().map(str::to_string),
            provider_thread_id: json["threadId"].as_str().map(str::to_string),
            message_id_header: None,
        }),
        401 => Err(SendError::AuthFailed(message)),
        429 => Err(SendError::RateLimited { retry_after: retry_after(headers), daily: has("dailyLimitExceeded") }),
        403 if has("dailyLimitExceeded") => Err(SendError::RateLimited { retry_after: None, daily: true }),
        403 if has("rateLimitExceeded") || has("userRateLimitExceeded") => {
            Err(SendError::RateLimited { retry_after: retry_after(headers), daily: false })
        }
        // The token has no permission to send (for example, the gmail.send scope is missing).
        403 if has("insufficientPermissions") || has("forbidden") => Err(SendError::AuthFailed(message)),
        500..=599 => Err(SendError::Transient(message)),
        _ => Err(SendError::Permanent { reason: message, hard_bounce: false }),
    }
}

#[async_trait]
impl MailSender for GmailSender {
    async fn send(&self, msg: &OutgoingMessage) -> Result<SendReceipt, SendError> {
        let mime = msg.build()?.formatted();
        let mut payload = serde_json::json!({ "raw": B64URL.encode(mime) });
        if let Some(thread) = &msg.thread_id {
            payload["threadId"] = Value::String(thread.clone());
        }
        let client = reqwest::Client::builder().timeout(Duration::from_secs(55)).build().map_err(map_request_error)?;
        let resp = client
            .post(&self.url)
            .bearer_auth(self.access_token.expose_secret())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(payload.to_string())
            .send()
            .await
            .map_err(map_request_error)?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let body = resp.text().await.unwrap_or_default();
        let mut receipt = map_response(status, &headers, &body)?;
        if let Some(gmail_id) = receipt.provider_message_id.clone() {
            receipt.message_id_header = self.real_message_id(&client, &gmail_id, &msg.message_id).await;
        }
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};

    fn err(status: u16, body: &str) -> SendError {
        map_response(status, &HeaderMap::new(), body).unwrap_err()
    }

    #[test]
    fn ok_saves_ids() {
        let r = map_response(200, &HeaderMap::new(), r#"{"id":"abc","threadId":"t1"}"#).unwrap();
        assert_eq!(r.provider_message_id.as_deref(), Some("abc"));
        assert_eq!(r.provider_thread_id.as_deref(), Some("t1"));
    }

    #[test]
    fn error_mapping() {
        assert!(matches!(err(401, "{}"), SendError::AuthFailed(_)));
        assert!(matches!(err(429, "{}"), SendError::RateLimited { daily: false, .. }));
        let rl = r#"{"error":{"message":"slow down","errors":[{"reason":"userRateLimitExceeded"}]}}"#;
        assert!(matches!(err(403, rl), SendError::RateLimited { daily: false, .. }));
        let daily = r#"{"error":{"errors":[{"reason":"dailyLimitExceeded"}]}}"#;
        assert!(matches!(err(403, daily), SendError::RateLimited { daily: true, .. }));
        assert!(matches!(err(503, "oops"), SendError::Transient(_)));
        assert!(matches!(err(500, "oops"), SendError::Transient(_)));
        assert!(matches!(err(400, "bad"), SendError::Permanent { hard_bounce: false, .. }));
    }

    #[test]
    fn retry_after_header_is_read() {
        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, HeaderValue::from_static("120"));
        let e = map_response(429, &h, "{}").unwrap_err();
        assert_eq!(e, SendError::RateLimited { retry_after: Some(Duration::from_secs(120)), daily: false });
    }
}
