//! Gmail API reader. Read only: `messages.list` and `messages.get`, nothing else.
//! Scope: `gmail.readonly`. It cannot change mail.
//! Gmail has one mailbox, so `inbox` and `spam` share one sync row with folder `all`.
use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64URL};
use chrono::TimeZone;
use futures::StreamExt;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};

pub const GMAIL_API_BASE: &str = "https://gmail.googleapis.com/gmail/v1/users/me";

/// The headers we ask for. Enough to decide if a mail is worth downloading.
const METADATA_HEADERS: &[&str] = &[
    "From", "To", "Subject", "Message-ID", "In-Reply-To", "References", "Content-Type", "Auto-Submitted", "X-Autoreply",
    "X-Autorespond", "Precedence", "Date",
];
/// Mail that arrives in the last 10 minutes may not be in the search index yet: look again at it next time.
const OVERLAP_SECS: i64 = 600;
/// One listing covers at most this many days. A long catch-up is split in pages.
const SLICE_DAYS: i64 = 1;
const MAX_IDS: usize = 5000;
const PAGE_SIZE: usize = 500;

pub struct GmailFetcher {
    access_token: SecretString,
    base: String,
    /// Test hook: the clock.
    now: fn() -> DateTime<Utc>,
}

impl GmailFetcher {
    pub fn new(access_token: SecretString) -> Self {
        Self::with_base(access_token, std::env::var("GMAIL_API_BASE_URL").unwrap_or_else(|_| GMAIL_API_BASE.into()))
    }
    pub fn with_base(access_token: SecretString, base: String) -> Self {
        Self { access_token, base: base.trim_end_matches('/').to_string(), now: Utc::now }
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

    async fn metadata(&self, client: &reqwest::Client, id: &str) -> Result<RemoteHeaders, FetchError> {
        let mut query: Vec<(&str, String)> = vec![("format", "metadata".into())];
        query.extend(METADATA_HEADERS.iter().map(|h| ("metadataHeaders", h.to_string())));
        let resp = self.get(client, &format!("{}/messages/{id}", self.base), &query).await?;
        let json = json_body(resp).await?;
        Ok(parse_metadata(&json))
    }
}

/// Gmail errors to `FetchError`. The same map as the Gmail sender.
pub fn map_error(status: u16, headers: &reqwest::header::HeaderMap, body: &str) -> FetchError {
    let json: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let message = json["error"]["message"].as_str().map(clip).unwrap_or_else(|| clip(body));
    let reasons: Vec<&str> = json["error"]["errors"].as_array().map(|a| a.iter().filter_map(|e| e["reason"].as_str()).collect()).unwrap_or_default();
    let has = |r: &str| reasons.contains(&r);
    match status {
        401 => FetchError::AuthFailed(message),
        429 => FetchError::RateLimited { retry_after: retry_after(headers) },
        403 if has("rateLimitExceeded") || has("userRateLimitExceeded") || has("dailyLimitExceeded") => {
            FetchError::RateLimited { retry_after: retry_after(headers) }
        }
        // The token has no permission to read (the read scope was not granted).
        403 if has("insufficientPermissions") || message.to_lowercase().contains("insufficient authentication scopes") => FetchError::ScopeMissing,
        403 => FetchError::AuthFailed(message),
        404 => FetchError::NotFound,
        _ => FetchError::Transient(format!("Gmail answered {status}: {message}")),
    }
}

fn parse_metadata(json: &Value) -> RemoteHeaders {
    let mut headers = HeaderMap::new();
    for h in json["payload"]["headers"].as_array().into_iter().flatten() {
        if let (Some(n), Some(v)) = (h["name"].as_str(), h["value"].as_str()) {
            add_header(&mut headers, n, v);
        }
    }
    RemoteHeaders {
        id: json["id"].as_str().unwrap_or_default().to_string(),
        thread_id: json["threadId"].as_str().map(str::to_string),
        received_at: json["internalDate"].as_str().and_then(|ms| ms.parse::<i64>().ok()).and_then(|ms| Utc.timestamp_millis_opt(ms).single()),
        headers,
        labels: json["labelIds"].as_array().into_iter().flatten().filter_map(|l| l.as_str().map(str::to_string)).collect(),
    }
}

#[async_trait]
impl MailFetcher for GmailFetcher {
    /// `{ "after": <unix seconds> }`
    fn initial_cursor(&self, since: DateTime<Utc>) -> Cursor {
        json!({ "after": since.timestamp() })
    }

    /// Stopped after `last_done`: continue from its time. One second back, because `after:` is not inclusive.
    fn cursor_after(&self, listing_cursor: &Cursor, last_done: &RemoteHeaders) -> Cursor {
        match last_done.received_at {
            Some(t) => json!({ "after": t.timestamp() - 1 }),
            None => listing_cursor.clone(),
        }
    }

    async fn list_new(&self, folder: Folder, cursor: Option<&Cursor>, _limit: usize) -> Result<Listing, FetchError> {
        let now = (self.now)();
        let after = cursor.and_then(|c| c["after"].as_i64()).unwrap_or_else(|| (now - chrono::Duration::days(14)).timestamp());
        // A long catch-up is read in slices of one day, so it can be stopped and continued.
        let slice_end = after + SLICE_DAYS * 86_400;
        let sliced = slice_end < now.timestamp();
        let mut q = format!("after:{after} -in:sent -in:drafts");
        if sliced {
            q.push_str(&format!(" before:{slice_end}"));
        }
        let client = http_client()?;
        let mut ids = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut query: Vec<(&str, String)> = vec![("q", q.clone()), ("maxResults", PAGE_SIZE.to_string())];
            // The spam folder is part of "all mail" only when asked for. This also finds replies the user already deleted.
            if folder == Folder::All {
                query.push(("includeSpamTrash", "true".into()));
            }
            if let Some(t) = &page_token {
                query.push(("pageToken", t.clone()));
            }
            let json = json_body(self.get(&client, &format!("{}/messages", self.base), &query).await?).await?;
            ids.extend(json["messages"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str().map(str::to_string)));
            page_token = json["nextPageToken"].as_str().map(str::to_string);
            if page_token.is_none() || ids.len() >= MAX_IDS {
                break;
            }
        }
        // The next cursor: the end of the slice, or (when we are at the end) the listing time minus the overlap.
        let next_after = if sliced { slice_end } else { now.timestamp() - OVERLAP_SECS };
        Ok(Listing { ids, next_cursor: json!({ "after": next_after }), more: sliced })
    }

    async fn headers(&self, _folder: Folder, ids: &[RemoteId]) -> Result<Vec<RemoteHeaders>, FetchError> {
        let client = http_client()?;
        let results: Vec<Result<RemoteHeaders, FetchError>> = futures::stream::iter(ids.to_vec())
            .map(|id| {
                let client = client.clone();
                async move { self.metadata(&client, &id).await }
            })
            .buffered(HEADER_PARALLEL)
            .collect()
            .await;
        let mut out = Vec::new();
        for r in results {
            match r {
                Ok(h) => out.push(h),
                Err(FetchError::NotFound) => {} // deleted since the listing
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    async fn raw(&self, _folder: Folder, id: &RemoteId, max_bytes: usize) -> Result<RawEmail, FetchError> {
        let client = http_client()?;
        let resp = self.get(&client, &format!("{}/messages/{id}", self.base), &[("format", "raw".into())]).await?;
        // The raw text is base64 inside JSON: about 4/3 bigger, plus a few JSON fields.
        let cap = max_bytes / 3 * 4 + 4096;
        match read_capped(resp, cap).await? {
            Some(body) => {
                let json: Value = serde_json::from_slice(&body).map_err(|e| FetchError::Transient(clip(&format!("bad answer from Gmail: {e}"))))?;
                let raw = json["raw"].as_str().unwrap_or_default().trim_end_matches('=');
                let bytes = B64URL.decode(raw).map_err(|_| FetchError::Transient("Gmail sent a raw message that is not base64url".into()))?;
                Ok(RawEmail { bytes, truncated: false })
            }
            None => {
                // Too big: use the headers only.
                let h = self.metadata(&client, id).await?;
                Ok(RawEmail { bytes: headers_to_bytes(&h.headers), truncated: true })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderMap as Hm;

    fn err(status: u16, body: &str) -> FetchError {
        map_error(status, &Hm::new(), body)
    }

    #[test]
    fn error_mapping() {
        assert!(matches!(err(401, "{}"), FetchError::AuthFailed(_)));
        assert_eq!(err(404, "{}"), FetchError::NotFound);
        assert!(matches!(err(429, "{}"), FetchError::RateLimited { .. }));
        assert!(matches!(err(500, "{}"), FetchError::Transient(_)));
        let limit = r#"{"error":{"message":"Rate","errors":[{"reason":"userRateLimitExceeded"}]}}"#;
        assert!(matches!(err(403, limit), FetchError::RateLimited { .. }));
        let scope = r#"{"error":{"message":"Insufficient Permission","errors":[{"reason":"insufficientPermissions"}]}}"#;
        assert_eq!(err(403, scope), FetchError::ScopeMissing);
        let scope2 = r#"{"error":{"message":"Request had insufficient authentication scopes.","status":"PERMISSION_DENIED"}}"#;
        assert_eq!(err(403, scope2), FetchError::ScopeMissing);
        assert!(matches!(err(403, r#"{"error":{"message":"forbidden"}}"#), FetchError::AuthFailed(_)));
        let mut h = Hm::new();
        h.insert(reqwest::header::RETRY_AFTER, "30".parse().unwrap());
        assert_eq!(map_error(429, &h, "{}"), FetchError::RateLimited { retry_after: Some(Duration::from_secs(30)) });
    }

    #[test]
    fn metadata_is_read() {
        let json = json!({
            "id": "abc", "threadId": "t1", "labelIds": ["INBOX", "UNREAD"], "internalDate": "1760000000000",
            "payload": { "headers": [
                { "name": "From", "value": "Jane <jane@x.example>" },
                { "name": "In-Reply-To", "value": "<a@b>" },
                { "name": "Received", "value": "one" }, { "name": "Received", "value": "two" } ] }
        });
        let h = parse_metadata(&json);
        assert_eq!((h.id.as_str(), h.thread_id.as_deref()), ("abc", Some("t1")));
        assert_eq!(h.headers["from"], "Jane <jane@x.example>");
        assert_eq!(h.headers["received"], "one, two");
        assert_eq!(h.received_at.unwrap().timestamp(), 1_760_000_000);
        assert_eq!(h.labels, ["INBOX", "UNREAD"]);
    }
}
