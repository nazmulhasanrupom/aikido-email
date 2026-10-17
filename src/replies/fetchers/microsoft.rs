//! Microsoft Graph reader. Read only: only `GET`. Never `PATCH`, so a message is never marked read.
//! Scope: `Mail.Read` (delegated). Folders: the well-known names `inbox` and `junkemail`.
use super::*;
use futures::StreamExt;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Mutex;

pub const GRAPH_BASE: &str = "https://graph.microsoft.com/v1.0";
const SELECT: &str = "id,internetMessageId,receivedDateTime,from,subject,conversationId,internetMessageHeaders";
const OVERLAP_SECS: i64 = 600;
const PAGE_SIZE: usize = 50;

pub struct MicrosoftFetcher {
    access_token: SecretString,
    base: String,
    /// Headers that came with the listing, so they are not asked for again.
    seen: Mutex<HashMap<String, RemoteHeaders>>,
}

impl MicrosoftFetcher {
    pub fn new(access_token: SecretString) -> Self {
        Self::with_base(access_token, std::env::var("MS_GRAPH_BASE_URL").unwrap_or_else(|_| GRAPH_BASE.into()))
    }
    pub fn with_base(access_token: SecretString, base: String) -> Self {
        Self { access_token, base: base.trim_end_matches('/').to_string(), seen: Mutex::default() }
    }

    async fn get(&self, client: &reqwest::Client, url: &str, query: &[(&str, String)]) -> Result<reqwest::Response, FetchError> {
        let resp = client.get(url_with(url, query)?).bearer_auth(self.access_token.expose_secret()).send().await.map_err(map_transport)?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(resp);
        }
        let headers = resp.headers().clone();
        let body = resp.text().await.unwrap_or_default();
        Err(map_error(status, &headers, &body))
    }

    async fn one(&self, client: &reqwest::Client, id: &str) -> Result<RemoteHeaders, FetchError> {
        if let Some(h) = self.seen.lock().unwrap().remove(id) {
            return Ok(h);
        }
        let resp = self.get(client, &format!("{}/me/messages/{id}", self.base), &[("$select", SELECT.into())]).await?;
        let json = json_body(resp).await?;
        Ok(parse_message(&json))
    }
}

/// Graph errors to `FetchError`. The same map as the Microsoft sender.
pub fn map_error(status: u16, headers: &reqwest::header::HeaderMap, body: &str) -> FetchError {
    let json: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let message = json["error"]["message"].as_str().map(clip).unwrap_or_else(|| clip(body));
    let code = json["error"]["code"].as_str().unwrap_or("");
    match status {
        401 => FetchError::AuthFailed(message),
        // The token is good but has no right to read the mailbox.
        403 => FetchError::ScopeMissing,
        404 if code == "ErrorItemNotFound" || code == "itemNotFound" || code.is_empty() => FetchError::NotFound,
        429 => FetchError::RateLimited { retry_after: retry_after(headers) },
        503 | 504 if retry_after(headers).is_some() => FetchError::RateLimited { retry_after: retry_after(headers) },
        _ => FetchError::Transient(format!("Microsoft Graph answered {status}: {message}")),
    }
}

fn parse_message(json: &Value) -> RemoteHeaders {
    let mut headers = HeaderMap::new();
    for h in json["internetMessageHeaders"].as_array().into_iter().flatten() {
        if let (Some(n), Some(v)) = (h["name"].as_str(), h["value"].as_str()) {
            add_header(&mut headers, n, v);
        }
    }
    // Graph does not always return every header: fill the main ones from the JSON fields.
    if !headers.contains_key("message-id") {
        if let Some(id) = json["internetMessageId"].as_str() {
            add_header(&mut headers, "message-id", id);
        }
    }
    if !headers.contains_key("from") {
        if let Some(addr) = json["from"]["emailAddress"]["address"].as_str() {
            let name = json["from"]["emailAddress"]["name"].as_str().unwrap_or_default();
            add_header(&mut headers, "from", &format!("{name} <{addr}>"));
        }
    }
    if !headers.contains_key("subject") {
        if let Some(s) = json["subject"].as_str() {
            add_header(&mut headers, "subject", s);
        }
    }
    RemoteHeaders {
        id: json["id"].as_str().unwrap_or_default().to_string(),
        // The Microsoft sender saves `conversationId` as `provider_thread_id`.
        thread_id: json["conversationId"].as_str().map(str::to_string),
        received_at: json["receivedDateTime"].as_str().and_then(|t| DateTime::parse_from_rfc3339(t).ok()).map(|t| t.with_timezone(&Utc)),
        headers,
        labels: vec![],
    }
}

fn folder_name(folder: Folder) -> &'static str {
    match folder {
        Folder::Spam => "junkemail",
        Folder::Inbox | Folder::All => "inbox",
    }
}

#[async_trait]
impl MailFetcher for MicrosoftFetcher {
    /// `{ "received_ge": "<iso time>" }`
    fn initial_cursor(&self, since: DateTime<Utc>) -> Cursor {
        json!({ "received_ge": since.to_rfc3339_opts(chrono::SecondsFormat::Secs, true) })
    }

    fn cursor_after(&self, listing_cursor: &Cursor, last_done: &RemoteHeaders) -> Cursor {
        match last_done.received_at {
            // `ge` is inclusive: the last mail is listed again and skipped as a duplicate.
            Some(t) => json!({ "received_ge": t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true) }),
            None => listing_cursor.clone(),
        }
    }

    async fn list_new(&self, folder: Folder, cursor: Option<&Cursor>, limit: usize) -> Result<Listing, FetchError> {
        let from = cursor
            .and_then(|c| c["received_ge"].as_str())
            .map(str::to_string)
            .unwrap_or_else(|| (Utc::now() - chrono::Duration::days(14)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
        let client = http_client()?;
        let url = format!("{}/me/mailFolders/{}/messages", self.base, folder_name(folder));
        let first = [
            ("$filter", format!("receivedDateTime ge {from}")),
            ("$orderby", "receivedDateTime asc".to_string()),
            ("$select", SELECT.to_string()),
            ("$top", PAGE_SIZE.to_string()),
        ];
        let mut next: Option<String> = None;
        let (mut ids, mut newest, mut more) = (Vec::new(), None::<DateTime<Utc>>, false);
        loop {
            let resp = match &next {
                None => self.get(&client, &url, &first).await?,
                Some(link) => self.get(&client, link, &[]).await?,
            };
            let json = json_body(resp).await?;
            for item in json["value"].as_array().into_iter().flatten() {
                let h = parse_message(item);
                if h.id.is_empty() {
                    continue;
                }
                newest = newest.max(h.received_at);
                ids.push(h.id.clone());
                // Keep the headers if the listing brought them.
                if item["internetMessageHeaders"].is_array() {
                    self.seen.lock().unwrap().insert(h.id.clone(), h);
                }
            }
            next = json["@odata.nextLink"].as_str().map(str::to_string);
            if next.is_none() {
                break;
            }
            if ids.len() >= limit {
                more = true;
                break;
            }
        }
        let next_cursor = match (more, newest) {
            // Stopped in the middle: continue from the newest time we got.
            (true, Some(t)) => json!({ "received_ge": t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true) }),
            // The end: newest time minus the overlap. Mail that is slow to appear is read next time.
            (false, Some(t)) => json!({ "received_ge": (t - chrono::Duration::seconds(OVERLAP_SECS)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true) }),
            (_, None) => json!({ "received_ge": from }),
        };
        Ok(Listing { ids, next_cursor, more })
    }

    async fn headers(&self, _folder: Folder, ids: &[RemoteId]) -> Result<Vec<RemoteHeaders>, FetchError> {
        let client = http_client()?;
        let results: Vec<Result<RemoteHeaders, FetchError>> = futures::stream::iter(ids.to_vec())
            .map(|id| {
                let client = client.clone();
                async move { self.one(&client, &id).await }
            })
            .buffered(HEADER_PARALLEL)
            .collect()
            .await;
        let mut out = Vec::new();
        for r in results {
            match r {
                Ok(h) => out.push(h),
                Err(FetchError::NotFound) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    async fn raw(&self, _folder: Folder, id: &RemoteId, max_bytes: usize) -> Result<RawEmail, FetchError> {
        let client = http_client()?;
        // `$value` is the MIME of the message. Reading it does not mark the message as read.
        let resp = self.get(&client, &format!("{}/me/messages/{id}/$value", self.base), &[]).await?;
        match read_capped(resp, max_bytes).await? {
            Some(bytes) => Ok(RawEmail { bytes, truncated: false }),
            None => {
                let h = self.one(&client, id).await?;
                Ok(RawEmail { bytes: headers_to_bytes(&h.headers), truncated: true })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderMap as Hm;

    #[test]
    fn error_mapping() {
        let e = |s, b: &str| map_error(s, &Hm::new(), b);
        assert!(matches!(e(401, "{}"), FetchError::AuthFailed(_)));
        assert_eq!(e(403, r#"{"error":{"code":"ErrorAccessDenied","message":"Access is denied."}}"#), FetchError::ScopeMissing);
        assert_eq!(e(404, r#"{"error":{"code":"ErrorItemNotFound"}}"#), FetchError::NotFound);
        assert!(matches!(e(429, "{}"), FetchError::RateLimited { .. }));
        assert!(matches!(e(503, "{}"), FetchError::Transient(_)));
        let mut h = Hm::new();
        h.insert(reqwest::header::RETRY_AFTER, "45".parse().unwrap());
        assert_eq!(map_error(429, &h, "{}"), FetchError::RateLimited { retry_after: Some(Duration::from_secs(45)) });
    }

    #[test]
    fn message_is_read_and_missing_headers_are_filled() {
        let json = json!({
            "id": "m1", "internetMessageId": "<abc@x.example>", "receivedDateTime": "2026-10-20T09:00:00Z",
            "conversationId": "conv-1", "subject": "Re: Hello",
            "from": { "emailAddress": { "name": "Jane", "address": "jane@x.example" } },
            "internetMessageHeaders": [ { "name": "In-Reply-To", "value": "<s1@a.example>" } ]
        });
        let h = parse_message(&json);
        assert_eq!(h.thread_id.as_deref(), Some("conv-1"));
        assert_eq!(h.headers["message-id"], "<abc@x.example>");
        assert_eq!(h.headers["from"], "Jane <jane@x.example>");
        assert_eq!(h.headers["in-reply-to"], "<s1@a.example>");
        assert_eq!(h.received_at.unwrap().timestamp(), 1_792_486_800);
    }
}
