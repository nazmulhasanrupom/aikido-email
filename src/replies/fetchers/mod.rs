//! One trait, three providers. The sync code never knows which provider it uses.
//! Each provider maps its own errors into the same `FetchError`.
//! Read only: no fetcher marks, moves, deletes, labels or flags mail.
pub mod fake;
pub mod gmail;
pub mod imap;
pub mod microsoft;

use super::parse::HeaderMap;
use crate::accounts::model::{EmailAccount, Provider, Secrets};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::time::Duration;

/// The provider's id for a message (Gmail id, Graph id, IMAP UID as text).
pub type RemoteId = String;
/// Where the last sync stopped. Saved as `jsonb` in `inbox_sync_state`. Its shape is the fetcher's own.
pub type Cursor = Value;

/// The sync folder. Gmail has one mailbox, so `inbox` and `spam` share the folder `all`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Folder {
    Inbox,
    Spam,
    All,
}

impl Folder {
    pub fn as_str(self) -> &'static str {
        match self {
            Folder::Inbox => "inbox",
            Folder::Spam => "spam",
            Folder::All => "all",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "inbox" => Some(Folder::Inbox),
            "spam" => Some(Folder::Spam),
            "all" => Some(Folder::All),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Listing {
    /// New messages. The sync sorts them by `received_at` when the headers have it.
    pub ids: Vec<RemoteId>,
    /// Where to continue after all `ids` are done.
    pub next_cursor: Cursor,
    /// More messages are waiting: call `list_new` again with `next_cursor`.
    pub more: bool,
}

/// The headers of one message (stage 1). Header names are lowercase.
#[derive(Clone, Debug, Default)]
pub struct RemoteHeaders {
    pub id: RemoteId,
    /// Gmail `threadId`.
    pub thread_id: Option<String>,
    /// When the provider got the message.
    pub received_at: Option<DateTime<Utc>>,
    pub headers: HeaderMap,
    /// Provider labels, like Gmail `SENT`, `DRAFT`, `SPAM`. Empty for other providers.
    pub labels: Vec<String>,
}

/// The raw MIME of one message (stage 2). Over the size cap, only the headers: `truncated = true`.
#[derive(Clone, Debug)]
pub struct RawEmail {
    pub bytes: Vec<u8>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FetchError {
    AuthFailed(String),
    /// The token has no permission to read mail.
    ScopeMissing,
    RateLimited { retry_after: Option<Duration> },
    Transient(String),
    /// The cursor is no longer good (for example IMAP `UIDVALIDITY` changed).
    CursorInvalid,
    /// The message was deleted since the listing: skip it.
    NotFound,
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FetchError::AuthFailed(m) => write!(f, "login failed: {m}"),
            FetchError::ScopeMissing => write!(f, "the login has no permission to read mail"),
            FetchError::RateLimited { retry_after } => {
                write!(f, "rate limited{}", retry_after.map(|d| format!(", retry after {}s", d.as_secs())).unwrap_or_default())
            }
            FetchError::Transient(m) => write!(f, "temporary error: {m}"),
            FetchError::CursorInvalid => write!(f, "the saved position in the mailbox is no longer valid"),
            FetchError::NotFound => write!(f, "message not found"),
        }
    }
}

/// Reads one inbox. Read only.
#[async_trait]
pub trait MailFetcher: Send + Sync {
    /// The cursor for a first sync: start at `since`.
    fn initial_cursor(&self, since: DateTime<Utc>) -> Cursor;
    /// The cursor to save when the sync stops after `last_done` (it ran out of its download budget).
    /// `listing_cursor` is the `next_cursor` of the listing that `last_done` came from.
    fn cursor_after(&self, listing_cursor: &Cursor, last_done: &RemoteHeaders) -> Cursor;

    async fn list_new(&self, folder: Folder, cursor: Option<&Cursor>, limit: usize) -> Result<Listing, FetchError>;
    /// Headers of these messages. A message that is gone is left out.
    async fn headers(&self, folder: Folder, ids: &[RemoteId]) -> Result<Vec<RemoteHeaders>, FetchError>;
    async fn raw(&self, folder: Folder, id: &RemoteId, max_bytes: usize) -> Result<RawEmail, FetchError>;
    /// End of the sync: close the connection (IMAP `LOGOUT`).
    async fn close(&self) {}
}

/// Makes the fetcher for one account. The sync never knows which provider it uses.
pub trait FetcherFactory: Send + Sync {
    fn make(&self, account: &EmailAccount, secrets: Secrets) -> Result<Box<dyn MailFetcher>, FetchError>;
    /// `false` for fetchers that do no login (tests): the sync then skips decrypt and token refresh.
    fn needs_credentials(&self) -> bool {
        true
    }
}

/// The real fetchers.
pub struct RealFetchers;

impl FetcherFactory for RealFetchers {
    fn make(&self, account: &EmailAccount, secrets: Secrets) -> Result<Box<dyn MailFetcher>, FetchError> {
        fetcher_for(account, secrets)
    }
}

/// Always gives the same `FakeFetcher` (for tests). It does no login.
pub struct FakeFetchers(pub fake::FakeFetcher);

impl FetcherFactory for FakeFetchers {
    fn make(&self, _: &EmailAccount, _: Secrets) -> Result<Box<dyn MailFetcher>, FetchError> {
        Ok(Box::new(self.0.clone()))
    }
    fn needs_credentials(&self) -> bool {
        false
    }
}

/// Picks the fetcher for an account.
pub fn fetcher_for(account: &EmailAccount, secrets: Secrets) -> Result<Box<dyn MailFetcher>, FetchError> {
    match (account.provider, secrets) {
        (Provider::Gmail, Secrets::OAuth(o)) => Ok(Box::new(gmail::GmailFetcher::new(o.access_token))),
        (Provider::Microsoft, Secrets::OAuth(o)) => Ok(Box::new(microsoft::MicrosoftFetcher::new(o.access_token))),
        (Provider::Smtp, Secrets::Password(p)) => Ok(Box::new(imap::ImapFetcher::from_account(account, p)?)),
        _ => Err(FetchError::AuthFailed("the saved credentials do not match the account type".into())),
    }
}

/// Truncate text from a remote server, so logs and `last_error` stay small.
pub fn clip(text: &str) -> String {
    let t: String = text.chars().take(300).collect();
    t.replace(['\n', '\r'], " ")
}

/// Header bytes for a message we only know the headers of (an over-size message).
pub fn headers_to_bytes(h: &HeaderMap) -> Vec<u8> {
    let mut out = String::new();
    for (k, v) in h {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    out.into_bytes()
}

// ---------- helpers for the HTTP providers (Gmail, Microsoft) ----------

/// How many header requests run at once.
pub(crate) const HEADER_PARALLEL: usize = 8;

pub(crate) fn http_client() -> Result<reqwest::Client, FetchError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(55))
        .build()
        .map_err(|e| FetchError::Transient(clip(&e.without_url().to_string())))
}

/// A request that never got an answer. Without the URL: it can hold ids.
pub(crate) fn map_transport(e: reqwest::Error) -> FetchError {
    let e = e.without_url();
    let mut reason = e.to_string();
    let mut source = std::error::Error::source(&e);
    while let Some(s) = source {
        reason.push_str(&format!(": {s}"));
        source = s.source();
    }
    FetchError::Transient(clip(&reason))
}

pub(crate) fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// Read a body, but stop at `max` bytes. `None` = it is bigger than `max`.
pub(crate) async fn read_capped(mut resp: reqwest::Response, max: usize) -> Result<Option<Vec<u8>>, FetchError> {
    if resp.content_length().is_some_and(|n| n as usize > max) {
        return Ok(None);
    }
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(map_transport)? {
        out.extend_from_slice(&chunk);
        if out.len() > max {
            return Ok(None);
        }
    }
    Ok(Some(out))
}

/// Add a header to a map. Names are lowercase. Two values are joined with `, `.
pub(crate) fn add_header(map: &mut HeaderMap, name: &str, value: &str) {
    let (name, value) = (name.to_lowercase(), value.split_whitespace().collect::<Vec<_>>().join(" "));
    match map.get_mut(&name) {
        Some(old) => {
            old.push_str(", ");
            old.push_str(&value);
        }
        None => {
            map.insert(name, value);
        }
    }
}

/// A URL with query parameters (percent-encoded).
pub(crate) fn url_with(base: &str, query: &[(&str, String)]) -> Result<String, FetchError> {
    let mut url = oauth2::url::Url::parse(base).map_err(|_| FetchError::Transient("bad API address".into()))?;
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query.iter().map(|(k, v)| (*k, v.as_str())));
    }
    Ok(url.to_string())
}

/// Read a JSON answer.
pub(crate) async fn json_body(resp: reqwest::Response) -> Result<serde_json::Value, FetchError> {
    let text = resp.text().await.map_err(map_transport)?;
    serde_json::from_str(&text).map_err(|e| FetchError::Transient(clip(&format!("the API sent something that is not JSON: {e}"))))
}
