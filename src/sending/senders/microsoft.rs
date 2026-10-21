//! Microsoft Graph sender: `POST /me/sendMail` with the MIME message as base64 text.
//! MIME keeps our own headers, like `In-Reply-To`.
//! `sendMail` answers `202` with no id. We keep our own `Message-ID`, and try once to find
//! the sent item by `internetMessageId` to save the Graph ids (best effort, needs `Mail.Read`).
use super::gmail::{LOOKUP_REQUEST_TIMEOUT, lookup_budget, map_request_error, retry_after};
use super::{MailSender, OutgoingMessage, SendError, SendReceipt, clip};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use std::time::Duration;

pub const GRAPH_BASE_URL: &str = "https://graph.microsoft.com/v1.0";

pub struct MicrosoftSender {
    access_token: SecretString,
    base_url: String,
    lookup_delay: Duration,
}

impl MicrosoftSender {
    pub fn new(access_token: SecretString) -> Self {
        Self {
            access_token,
            base_url: std::env::var("MS_GRAPH_BASE_URL").unwrap_or_else(|_| GRAPH_BASE_URL.into()),
            // Sent items show up a moment after `sendMail` answers.
            lookup_delay: Duration::from_millis(
                std::env::var("MS_SENT_LOOKUP_DELAY_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(3000),
            ),
        }
    }
    pub fn with_base_url(access_token: SecretString, base_url: String, lookup_delay: Duration) -> Self {
        Self { access_token, base_url, lookup_delay }
    }

    /// Find the sent item. First by our Message-ID. If Graph replaced the id, by subject and recipient among the newest items.
    /// Returns (graph id, conversation id, the Message-ID the recipient sees).
    async fn find_sent_item(&self, client: &reqwest::Client, msg: &OutgoingMessage) -> Option<(String, Option<String>, Option<String>)> {
        tokio::time::sleep(self.lookup_delay).await;
        let base = format!("{}/me/mailFolders/sentitems/messages", self.base_url);
        let query = |filter: String, extra: &[(&str, &str)]| {
            let mut url = oauth2::url::Url::parse(&base).ok()?;
            {
                let mut q = url.query_pairs_mut();
                q.append_pair("$filter", &filter).append_pair("$select", "id,conversationId,internetMessageId,subject,toRecipients");
                for (k, v) in extra {
                    q.append_pair(k, v);
                }
            }
            Some(url)
        };
        let fetch = |url: oauth2::url::Url| async move {
            let resp = client.get(url).timeout(LOOKUP_REQUEST_TIMEOUT).bearer_auth(self.access_token.expose_secret()).send().await.ok()?;
            if !resp.status().is_success() {
                return None;
            }
            resp.json_value().await
        };
        // 1. By our own Message-ID.
        let url = query(format!("internetMessageId eq '{}'", msg.message_id.replace('\'', "''")), &[("$top", "1")])?;
        if let Some(item) = fetch(url).await.and_then(|j| j["value"].as_array()?.first().cloned()) {
            return Some((item["id"].as_str()?.to_string(), item["conversationId"].as_str().map(str::to_string), None));
        }
        // 2. Graph kept the mail but changed the id: look at the newest sent items.
        let since = (chrono::Utc::now() - chrono::Duration::minutes(10)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let url = query(format!("sentDateTime ge {since}"), &[("$orderby", "sentDateTime desc"), ("$top", "10")])?;
        let items = fetch(url).await?;
        let item = items["value"].as_array()?.iter().find(|i| {
            i["subject"].as_str() == Some(msg.subject.as_str())
                && i["toRecipients"].as_array().is_some_and(|r| {
                    r.iter().any(|x| x["emailAddress"]["address"].as_str().is_some_and(|a| a.eq_ignore_ascii_case(&msg.to_email)))
                })
        })?;
        let real = item["internetMessageId"].as_str().map(str::to_string).filter(|r| *r != msg.message_id);
        Some((item["id"].as_str()?.to_string(), item["conversationId"].as_str().map(str::to_string), real))
    }
}

/// Small helper: read a response body as JSON.
trait JsonValue {
    async fn json_value(self) -> Option<Value>;
}
impl JsonValue for reqwest::Response {
    async fn json_value(self) -> Option<Value> {
        serde_json::from_str(&self.text().await.ok()?).ok()
    }
}

pub fn map_response(status: u16, headers: &reqwest::header::HeaderMap, body: &str) -> Result<(), SendError> {
    let json: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let code = json["error"]["code"].as_str().unwrap_or("");
    let message = json["error"]["message"].as_str().map(clip).unwrap_or_else(|| clip(body));
    match status {
        200..=299 => Ok(()),
        401 => Err(SendError::AuthFailed(message)),
        _ if code == "ErrorSendQuotaExceeded" || code == "ErrorExceededMessageLimit" => {
            Err(SendError::RateLimited { retry_after: retry_after(headers), daily: true })
        }
        429 => Err(SendError::RateLimited { retry_after: retry_after(headers), daily: false }),
        500..=599 => Err(SendError::Transient(message)),
        _ if code == "ErrorInvalidRecipients" || code == "ErrorRecipientNotFound" => {
            Err(SendError::Permanent { reason: message, hard_bounce: true })
        }
        _ => Err(SendError::Permanent { reason: message, hard_bounce: false }),
    }
}

#[async_trait]
impl MailSender for MicrosoftSender {
    async fn send(&self, msg: &OutgoingMessage) -> Result<SendReceipt, SendError> {
        let mime = msg.build()?.formatted();
        let started = std::time::Instant::now();
        let client = reqwest::Client::builder().timeout(Duration::from_secs(55)).build().map_err(map_request_error)?;
        let resp = client
            .post(format!("{}/me/sendMail", self.base_url))
            .bearer_auth(self.access_token.expose_secret())
            .header(reqwest::header::CONTENT_TYPE, "text/plain")
            .body(B64.encode(mime))
            .send()
            .await
            .map_err(map_request_error)?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let body = resp.text().await.unwrap_or_default();
        map_response(status, &headers, &body)?;
        // The email is sent. A slow lookup (its wait counts too) must not turn it into a timeout.
        let found = match lookup_budget(started.elapsed()) {
            Some(budget) => tokio::time::timeout(budget, self.find_sent_item(&client, msg)).await.ok().flatten(),
            None => None,
        };
        Ok(SendReceipt {
            provider_message_id: found.as_ref().map(|f| f.0.clone()),
            message_id_header: found.as_ref().and_then(|f| f.2.clone()),
            provider_thread_id: found.and_then(|f| f.1),
        })
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
    fn error_mapping() {
        assert!(map_response(202, &HeaderMap::new(), "").is_ok());
        assert!(matches!(err(401, "{}"), SendError::AuthFailed(_)));
        assert!(matches!(err(503, "{}"), SendError::Transient(_)));
        assert!(matches!(err(504, "{}"), SendError::Transient(_)));
        assert!(matches!(err(400, "{}"), SendError::Permanent { hard_bounce: false, .. }));
        assert!(matches!(err(404, "{}"), SendError::Permanent { .. }));
        let invalid = r#"{"error":{"code":"ErrorInvalidRecipients","message":"bad"}}"#;
        assert!(matches!(err(400, invalid), SendError::Permanent { hard_bounce: true, .. }));
        let quota = r#"{"error":{"code":"ErrorSendQuotaExceeded"}}"#;
        assert!(matches!(err(400, quota), SendError::RateLimited { daily: true, .. }));
        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, HeaderValue::from_static("30"));
        assert_eq!(
            map_response(429, &h, "{}").unwrap_err(),
            SendError::RateLimited { retry_after: Some(Duration::from_secs(30)), daily: false }
        );
    }
}
