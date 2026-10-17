//! `FakeFetcher`: returns emails from a list (saved `.eml` files). For unit and engine tests. No network.
use super::*;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct FakeMail {
    pub folder: Folder,
    pub raw: Vec<u8>,
    pub thread_id: Option<String>,
    pub received_at: DateTime<Utc>,
    pub labels: Vec<String>,
}

#[derive(Default)]
struct Inner {
    mails: Vec<FakeMail>,
    errors: VecDeque<FetchError>,
    list_calls: usize,
    raw_calls: usize,
    /// Pretend the cursor is bad on the next listing.
    max_raw: Option<usize>,
}

/// Clones share the same list of mails.
#[derive(Clone, Default)]
pub struct FakeFetcher {
    inner: Arc<Mutex<Inner>>,
}

impl FakeFetcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an email. Returns its id.
    pub fn add(&self, folder: Folder, raw: Vec<u8>, received_at: DateTime<Utc>) -> RemoteId {
        self.add_mail(FakeMail { folder, raw, thread_id: None, received_at, labels: vec![] })
    }

    pub fn add_mail(&self, mail: FakeMail) -> RemoteId {
        let mut i = self.inner.lock().unwrap();
        i.mails.push(mail);
        (i.mails.len() - 1).to_string()
    }

    /// The next call to the fetcher fails with this error.
    pub fn fail_next(&self, e: FetchError) {
        self.inner.lock().unwrap().errors.push_back(e);
    }

    pub fn list_calls(&self) -> usize {
        self.inner.lock().unwrap().list_calls
    }
    pub fn raw_calls(&self) -> usize {
        self.inner.lock().unwrap().raw_calls
    }

    fn next_error(&self) -> Option<FetchError> {
        self.inner.lock().unwrap().errors.pop_front()
    }
}

/// Cursor shape: `{ "after": <unix seconds> }`: mails received after that time.
#[async_trait]
impl MailFetcher for FakeFetcher {
    fn initial_cursor(&self, since: DateTime<Utc>) -> Cursor {
        serde_json::json!({ "after": since.timestamp() })
    }

    fn cursor_after(&self, _listing_cursor: &Cursor, last_done: &RemoteHeaders) -> Cursor {
        serde_json::json!({ "after": last_done.received_at.map(|t| t.timestamp()).unwrap_or(0) })
    }

    async fn list_new(&self, folder: Folder, cursor: Option<&Cursor>, limit: usize) -> Result<Listing, FetchError> {
        if let Some(e) = self.next_error() {
            return Err(e);
        }
        let mut i = self.inner.lock().unwrap();
        i.list_calls += 1;
        let after = cursor.and_then(|c| c["after"].as_i64()).unwrap_or(0);
        let mut found: Vec<(usize, DateTime<Utc>)> = i
            .mails
            .iter()
            .enumerate()
            .filter(|(_, m)| (m.folder == folder || folder == Folder::All) && m.received_at.timestamp() > after)
            .map(|(n, m)| (n, m.received_at))
            .collect();
        found.sort_by_key(|(n, t)| (*t, *n));
        let more = found.len() > limit;
        found.truncate(limit);
        let next_after = found.last().map(|(_, t)| t.timestamp()).unwrap_or(after);
        Ok(Listing {
            ids: found.iter().map(|(n, _)| n.to_string()).collect(),
            next_cursor: serde_json::json!({ "after": next_after }),
            more,
        })
    }

    async fn headers(&self, _folder: Folder, ids: &[RemoteId]) -> Result<Vec<RemoteHeaders>, FetchError> {
        if let Some(e) = self.next_error() {
            return Err(e);
        }
        let i = self.inner.lock().unwrap();
        let mut out = Vec::new();
        for id in ids {
            let Some(m) = id.parse::<usize>().ok().and_then(|n| i.mails.get(n)) else { continue };
            out.push(RemoteHeaders {
                id: id.clone(),
                thread_id: m.thread_id.clone(),
                received_at: Some(m.received_at),
                headers: crate::replies::parse::parse_header_block(&m.raw),
                labels: m.labels.clone(),
            });
        }
        Ok(out)
    }

    async fn raw(&self, _folder: Folder, id: &RemoteId, max_bytes: usize) -> Result<RawEmail, FetchError> {
        if let Some(e) = self.next_error() {
            return Err(e);
        }
        let mut i = self.inner.lock().unwrap();
        i.raw_calls += 1;
        let _ = i.max_raw;
        let m = id.parse::<usize>().ok().and_then(|n| i.mails.get(n)).ok_or(FetchError::NotFound)?;
        if m.raw.len() > max_bytes {
            let h = crate::replies::parse::parse_header_block(&m.raw);
            return Ok(RawEmail { bytes: headers_to_bytes(&h), truncated: true });
        }
        Ok(RawEmail { bytes: m.raw.clone(), truncated: false })
    }
}
