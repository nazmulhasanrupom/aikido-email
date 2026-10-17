//! Sync one folder of one account: list new mail, read the headers, download only the mail that
//! may link to our sends, then parse, classify, match, act, and save the cursor.
//! Read only. Never logs a token, a password, or an email body.
use super::dsn;
use super::fetchers::{Cursor, FetchError, FetcherFactory, Folder, MailFetcher, RemoteHeaders, headers_to_bytes};
use super::ingest::{self, Ingested, Inbound};
use super::parse::{HeaderMap, message_ids};
use super::repo;
use super::settings::ReplySettings;
use crate::accounts::model::{AccountStatus, EmailAccount, OAuthSecrets, PasswordSecrets, Secrets};
use crate::email_management::oauth_common::RefreshError;
use crate::sending::{queue, tokens};
use crate::state::{AppState, Clock};
use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;

/// Messages listed per page.
const PAGE: usize = 200;
/// Wait after a rate limit with no `Retry-After`.
const DEFAULT_RETRY: Duration = Duration::minutes(5);
const MAX_BACKOFF: Duration = Duration::hours(1);

/// The reply check engine: what the poller and the sync need.
pub struct ReplyEngine {
    pub state: AppState,
    pub factory: Arc<dyn FetcherFactory>,
    pub clock: Clock,
    pub cancel: tokio_util::sync::CancellationToken,
}

impl ReplyEngine {
    pub fn new(state: AppState, factory: Arc<dyn FetcherFactory>) -> Self {
        Self { clock: state.clock.clone(), state, factory, cancel: tokio_util::sync::CancellationToken::new() }
    }
    pub fn now(&self) -> DateTime<Utc> {
        (self.clock)()
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct SyncReport {
    /// Messages the provider listed.
    pub listed: usize,
    /// Messages that passed the header check and were downloaded.
    pub downloaded: usize,
    pub stored: usize,
    /// Replies that did not link to our sends. Not stored.
    pub dropped: usize,
    /// Already stored: nothing ran again.
    pub duplicates: usize,
    /// More mail is waiting: the next tick continues.
    pub more: bool,
    pub error: Option<String>,
}

// ---------- stage 1: is this mail worth downloading? ----------

/// What the database knows about the mail of one page. Built in a few queries, so `is_candidate` stays pure.
#[derive(Debug, Default)]
pub struct Lookup {
    /// `Message-ID`s of our sends that this page points to.
    pub message_ids: HashSet<String>,
    /// Gmail threads of this account's sends.
    pub threads: HashSet<String>,
    /// Addresses this account sent to within `match_window_days`.
    pub senders: HashSet<String>,
}

pub fn header_ids(h: &HeaderMap) -> Vec<String> {
    let mut ids = h.get("in-reply-to").map(|v| message_ids(v)).unwrap_or_default();
    ids.extend(h.get("references").map(|v| message_ids(v)).unwrap_or_default());
    ids
}

pub fn from_address(h: &HeaderMap) -> Option<String> {
    h.get("from").and_then(|v| dsn::addresses_in_text(v).into_iter().next())
}

/// Looks like a bounce or a complaint: a report, a system sender, or a bounce subject.
pub fn looks_like_bounce(h: &HeaderMap, s: &ReplySettings) -> bool {
    if h.get("content-type").is_some_and(|v| v.to_lowercase().trim_start().starts_with("multipart/report")) {
        return true;
    }
    if from_address(h).is_some_and(|a| a.starts_with("mailer-daemon@") || a.starts_with("postmaster@")) {
        return true;
    }
    let subject = h.get("subject").map(|v| v.to_lowercase()).unwrap_or_default();
    ReplySettings::lower(&s.bounce_subject_patterns).iter().any(|p| subject.contains(p.as_str()))
}

/// Download this mail? Yes when any of these is true:
/// it points to a `Message-ID` we sent, it is in a Gmail thread of ours, the sender got a send from this account
/// lately, or it looks like a bounce or a complaint.
pub fn is_candidate(h: &RemoteHeaders, lookup: &Lookup, s: &ReplySettings) -> bool {
    if header_ids(&h.headers).iter().any(|id| lookup.message_ids.contains(id)) {
        return true;
    }
    if h.thread_id.as_ref().is_some_and(|t| lookup.threads.contains(t)) {
        return true;
    }
    if from_address(&h.headers).is_some_and(|a| lookup.senders.contains(&a)) {
        return true;
    }
    looks_like_bounce(&h.headers, s)
}

async fn build_lookup(
    pool: &sqlx::PgPool,
    account_id: Uuid,
    hs: &[&RemoteHeaders],
    window_days: u32,
    now: DateTime<Utc>,
) -> Result<Lookup, sqlx::Error> {
    let mut ids: Vec<String> = hs.iter().flat_map(|h| header_ids(&h.headers)).collect();
    ids.sort();
    ids.dedup();
    let mut threads: Vec<String> = hs.iter().filter_map(|h| h.thread_id.clone()).collect();
    threads.sort();
    threads.dedup();
    let mut senders: Vec<String> = hs.iter().filter_map(|h| from_address(&h.headers)).collect();
    senders.sort();
    senders.dedup();

    let message_ids: Vec<(String,)> = sqlx::query_as("SELECT message_id_header FROM send_jobs WHERE message_id_header = ANY($1)")
        .bind(&ids)
        .fetch_all(pool)
        .await?;
    let thread_rows: Vec<(String,)> =
        sqlx::query_as("SELECT DISTINCT provider_thread_id FROM send_jobs WHERE account_id = $1 AND provider_thread_id = ANY($2)")
            .bind(account_id)
            .bind(&threads)
            .fetch_all(pool)
            .await?;
    let sender_rows: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT to_email::text FROM send_jobs
         WHERE account_id = $1 AND status = 'sent' AND sent_at > $3 AND to_email = ANY($2::citext[])",
    )
    .bind(account_id)
    .bind(&senders)
    .bind(now - Duration::days(window_days as i64))
    .fetch_all(pool)
    .await?;
    Ok(Lookup {
        message_ids: message_ids.into_iter().map(|r| r.0).collect(),
        threads: thread_rows.into_iter().map(|r| r.0).collect(),
        senders: sender_rows.into_iter().map(|r| r.0.to_lowercase()).collect(),
    })
}

// ---------- the sync ----------

#[derive(Debug)]
enum SyncError {
    Fetch(FetchError),
    Db(sqlx::Error),
}

impl From<FetchError> for SyncError {
    fn from(e: FetchError) -> Self {
        SyncError::Fetch(e)
    }
}
impl From<sqlx::Error> for SyncError {
    fn from(e: sqlx::Error) -> Self {
        SyncError::Db(e)
    }
}

/// The folder to ask the fetcher for. Gmail has one mailbox: `spam` is part of it only if it is wanted.
pub fn fetch_folder(row_folder: &str, s: &ReplySettings) -> Option<Folder> {
    match Folder::parse(row_folder)? {
        Folder::All if !s.folders.iter().any(|f| f == "spam") => Some(Folder::Inbox),
        other => Some(other),
    }
}

struct Run<'a> {
    engine: &'a ReplyEngine,
    account: &'a EmailAccount,
    settings: &'a ReplySettings,
    exempt: &'a [String],
    row_folder: &'a str,
    folder: Folder,
    report: SyncReport,
    /// The cursor to save. `None` = nothing new to save.
    progress: Option<Cursor>,
}

impl Run<'_> {
    async fn run(&mut self, fetcher: &dyn MailFetcher, start: Cursor) -> Result<(), SyncError> {
        let (engine, account, s) = (self.engine, self.account, self.settings);
        let pool = &engine.state.pool;
        let max_downloads = s.max_messages_per_sync as usize;
        let max_bytes = engine.state.config.max_message_bytes;
        let own = account.email.to_lowercase();
        let mut cursor = start;

        loop {
            if engine.cancel.is_cancelled() {
                self.report.more = true;
                return Ok(());
            }
            let listing = fetcher.list_new(self.folder, Some(&cursor), PAGE).await?;
            self.report.listed += listing.ids.len();

            // Stage 1: headers only. Oldest first, so a stop in the middle can save a clean cursor.
            let mut hs = if listing.ids.is_empty() { vec![] } else { fetcher.headers(self.folder, &listing.ids).await? };
            if hs.iter().all(|h| h.received_at.is_some()) {
                hs.sort_by_key(|h| h.received_at);
            }
            let stored: HashSet<String> = if hs.is_empty() {
                HashSet::new()
            } else {
                let ids: Vec<String> = hs.iter().map(|h| h.id.clone()).collect();
                sqlx::query_as::<_, (String,)>("SELECT provider_message_id FROM inbound_messages WHERE account_id = $1 AND provider_message_id = ANY($2)")
                    .bind(account.id)
                    .bind(&ids)
                    .fetch_all(pool)
                    .await?
                    .into_iter()
                    .map(|r| r.0)
                    .collect()
            };
            let fresh: Vec<&RemoteHeaders> = hs
                .iter()
                .filter(|h| !stored.contains(&h.id))
                // Our own mail, drafts, and sent items are never replies.
                .filter(|h| from_address(&h.headers).as_deref() != Some(own.as_str()))
                .filter(|h| !h.labels.iter().any(|l| l.eq_ignore_ascii_case("SENT") || l.eq_ignore_ascii_case("DRAFT")))
                .collect();
            let lookup = build_lookup(pool, account.id, &fresh, s.match_window_days, engine.now()).await?;
            let todo: HashSet<&str> = fresh.iter().filter(|h| is_candidate(h, &lookup, s)).map(|h| h.id.as_str()).collect();

            // Stage 2: the full email, only for candidates.
            let mut last_done: Option<&RemoteHeaders> = None;
            for h in &hs {
                if !todo.contains(h.id.as_str()) {
                    last_done = Some(h);
                    continue;
                }
                if self.report.downloaded >= max_downloads {
                    // Out of budget. Save where we are; the next tick continues.
                    if let Some(done) = last_done {
                        self.progress = Some(fetcher.cursor_after(&listing.next_cursor, done));
                    }
                    self.report.more = true;
                    return Ok(());
                }
                let raw = match fetcher.raw(self.folder, &h.id, max_bytes).await {
                    Ok(r) => r,
                    Err(FetchError::NotFound) => {
                        last_done = Some(h);
                        continue; // deleted since the listing
                    }
                    Err(e) => {
                        if let Some(done) = last_done {
                            self.progress = Some(fetcher.cursor_after(&listing.next_cursor, done));
                        }
                        return Err(e.into());
                    }
                };
                self.report.downloaded += 1;
                // Over the cap: only the headers came back. Classify from those.
                let bytes = if raw.truncated && raw.bytes.is_empty() { headers_to_bytes(&h.headers) } else { raw.bytes };
                let inbound = Inbound {
                    account,
                    folder: self.row_folder,
                    provider_message_id: &h.id,
                    provider_thread_id: h.thread_id.as_deref(),
                    raw: &bytes,
                    truncated: raw.truncated,
                    headers: Some(&h.headers),
                    received_at: h.received_at,
                };
                match ingest::ingest(pool, &inbound, s, self.exempt, engine.now()).await {
                    Ok(Ingested::Stored { label, .. }) => {
                        self.report.stored += 1;
                        tracing::info!(account_id = %account.id, label = label.as_str(), "inbound email stored");
                    }
                    Ok(Ingested::Dropped) => self.report.dropped += 1,
                    Ok(Ingested::Duplicate) => self.report.duplicates += 1,
                    Err(e) => {
                        if let Some(done) = last_done {
                            self.progress = Some(fetcher.cursor_after(&listing.next_cursor, done));
                        }
                        return Err(e.into());
                    }
                }
                last_done = Some(h);
            }

            // The page is done.
            cursor = listing.next_cursor.clone();
            self.progress = Some(cursor.clone());
            repo::save_cursor(pool, account.id, self.row_folder, &cursor).await?;
            repo::extend_lease(pool, account.id, self.row_folder, engine.now()).await?;
            if !listing.more {
                return Ok(());
            }
        }
    }
}

/// Make the fetcher: decrypt, refresh the OAuth token if it ends in less than 5 minutes (same code as the sender).
async fn make_fetcher(
    engine: &ReplyEngine,
    account: &EmailAccount,
    force_refresh: bool,
) -> Result<(Box<dyn MailFetcher>, bool), FetchError> {
    let needs = engine.factory.needs_credentials();
    let mut secrets = if needs {
        crate::accounts::repo::load_secrets(&engine.state.crypto, account).map_err(|e| {
            tracing::error!(account_id = %account.id, error = %format!("{e:#}"), "cannot decrypt account secrets");
            FetchError::Transient("cannot decrypt the saved credentials (is AIKIDO_ENCRYPTION_KEY the same key?)".into())
        })?
    } else {
        Secrets::Password(PasswordSecrets::default())
    };
    let mut refreshed = false;
    if let Secrets::OAuth(o) = &secrets {
        if needs && (force_refresh || tokens::needs_refresh(account.token_expires_at, engine.now())) {
            secrets = Secrets::OAuth(refresh(engine, account, o).await?);
            refreshed = true;
        }
    }
    Ok((engine.factory.make(account, secrets)?, refreshed))
}

async fn refresh(engine: &ReplyEngine, account: &EmailAccount, o: &OAuthSecrets) -> Result<OAuthSecrets, FetchError> {
    match tokens::refresh(&engine.state.pool, &engine.state.crypto, account, o, engine.now()).await {
        Ok(fresh) => Ok(fresh),
        Err(RefreshError::InvalidGrant(m)) => Err(FetchError::AuthFailed(format!("the login is no longer valid ({m})"))),
        Err(RefreshError::Other(m)) => Err(FetchError::Transient(format!("token refresh failed: {m}"))),
    }
}

/// Sync one folder of one account. The poller calls this for each claimed row.
pub async fn sync_folder(engine: &ReplyEngine, account_id: Uuid, row_folder: &str) -> anyhow::Result<SyncReport> {
    let pool = &engine.state.pool;
    let now = engine.now();
    let mut report = SyncReport::default();

    let Some(account) = crate::accounts::repo::get(pool, account_id).await? else { return Ok(report) };
    let Some(state) = repo::load_sync_state(pool, account_id, row_folder).await? else { return Ok(report) };
    let mut conn = pool.acquire().await?;
    let settings = repo::load_settings(&mut conn, account_id).await?;
    let exempt = queue::load_settings(&mut conn, account_id).await?.per_domain_exempt;
    drop(conn);
    let Some(folder) = fetch_folder(row_folder, &settings) else { return Ok(report) };
    if !account.can_read || account.status == AccountStatus::Disconnected {
        repo::sync_ok(pool, account_id, row_folder, state.last_success_at.unwrap_or(now), now + Duration::seconds(settings.poll_interval_secs as i64)).await?;
        return Ok(report);
    }

    let start = |fetcher: &dyn MailFetcher| match &state.cursor {
        // After `CursorInvalid`: start again from the saved time.
        Some(c) if c.get("reset_since").is_some() => {
            let since = c["reset_since"].as_str().and_then(|t| DateTime::parse_from_rfc3339(t).ok()).map(|t| t.with_timezone(&Utc));
            fetcher.initial_cursor(since.unwrap_or(now - Duration::days(settings.initial_lookback_days as i64)))
        }
        Some(c) => c.clone(),
        None => fetcher.initial_cursor(now - Duration::days(settings.initial_lookback_days as i64)),
    };

    let mut forced = false;
    let (result, mut progress) = loop {
        let (fetcher, refreshed) = match make_fetcher(engine, &account, forced).await {
            Ok(f) => f,
            Err(e) => break (Err(SyncError::Fetch(e)), None),
        };
        let mut run = Run {
            engine,
            account: &account,
            settings: &settings,
            exempt: &exempt,
            row_folder,
            folder,
            report: SyncReport::default(),
            progress: None,
        };
        let r = run.run(fetcher.as_ref(), start(fetcher.as_ref())).await;
        fetcher.close().await;
        report = run.report;
        // A login error on an OAuth account: refresh the token once and try again.
        let retry = matches!(&r, Err(SyncError::Fetch(FetchError::AuthFailed(_))))
            && account.provider.is_oauth()
            && engine.factory.needs_credentials()
            && !refreshed
            && !forced;
        if retry {
            forced = true;
            continue;
        }
        break (r, run.progress);
    };

    // Save the progress, also after an error.
    if let Some(c) = progress.take() {
        repo::save_cursor(pool, account_id, row_folder, &c).await?;
    }
    let now = engine.now();
    let poll = Duration::seconds(settings.poll_interval_secs as i64);
    match result {
        Ok(()) => {
            let next = if report.more { now } else { now + poll };
            repo::sync_ok(pool, account_id, row_folder, now, next).await?;
        }
        Err(err) => handle_error(engine, &account, row_folder, &state, &settings, err, &mut report).await?,
    }
    Ok(report)
}

async fn handle_error(
    engine: &ReplyEngine,
    account: &EmailAccount,
    row_folder: &str,
    state: &repo::SyncState,
    settings: &ReplySettings,
    err: SyncError,
    report: &mut SyncReport,
) -> anyhow::Result<()> {
    let pool = &engine.state.pool;
    let now = engine.now();
    let poll = Duration::seconds(settings.poll_interval_secs as i64);
    let backoff = |errors: i32| {
        let factor = 2i64.saturating_pow(errors.clamp(0, 20) as u32);
        (poll * factor.min(i32::MAX as i64) as i32).min(MAX_BACKOFF)
    };
    let err = match err {
        SyncError::Db(e) => FetchError::Transient(format!("database error: {e}")),
        SyncError::Fetch(e) => e,
    };
    report.error = Some(err.to_string());
    tracing::warn!(account_id = %account.id, folder = row_folder, error = %err, "reply sync did not finish");
    let text = err.to_string();
    match err {
        FetchError::AuthFailed(msg) if account.provider.is_oauth() => {
            // The refresh did not help: the login is gone. Sending stops too, as in the sending engine.
            let reason = format!("Login failed while reading mail: {msg}. Connect the account again.");
            let mut tx = pool.begin().await?;
            sqlx::query("UPDATE email_accounts SET status = 'disconnected', last_error = $2, can_read = false, read_status = 'reconnect_needed', updated_at = $3 WHERE id = $1")
                .bind(account.id)
                .bind(&reason)
                .bind(now)
                .execute(&mut *tx)
                .await?;
            queue::pause_state(&mut tx, account.id, &reason, now).await?;
            queue::log_event(&mut tx, None, account.id, "paused", json!({ "reason": reason, "disconnected": true }), now).await?;
            tx.commit().await?;
            repo::sync_failed(pool, account.id, row_folder, &text, now + MAX_BACKOFF, false).await?;
        }
        FetchError::AuthFailed(_) => {
            // A wrong IMAP password. SMTP may still work, so sending goes on. Reading stops until the user fixes it.
            repo::set_read_access(pool, account.id, None, false, "login_failed").await?;
            repo::sync_failed(pool, account.id, row_folder, &text, now + MAX_BACKOFF, false).await?;
        }
        FetchError::ScopeMissing => {
            repo::set_read_access(pool, account.id, None, false, "reconnect_needed").await?;
            repo::sync_failed(pool, account.id, row_folder, &text, now + MAX_BACKOFF, false).await?;
        }
        FetchError::RateLimited { retry_after } => {
            let wait = retry_after.map(|d| Duration::seconds(d.as_secs() as i64)).unwrap_or(DEFAULT_RETRY);
            repo::sync_failed(pool, account.id, row_folder, &text, now + wait, false).await?;
        }
        FetchError::Transient(_) => {
            let errors = state.consecutive_errors + 1;
            repo::sync_failed(pool, account.id, row_folder, &text, now + backoff(errors), true).await?;
        }
        FetchError::CursorInvalid => {
            // Start again from a day before the last good sync. Dedup stops double actions.
            let since = state.last_success_at.unwrap_or(now) - Duration::days(1);
            sqlx::query("UPDATE inbox_sync_state SET cursor = $3 WHERE account_id = $1 AND folder = $2")
                .bind(account.id)
                .bind(row_folder)
                .bind(json!({ "reset_since": since.to_rfc3339() }))
                .execute(pool)
                .await?;
            repo::sync_failed(pool, account.id, row_folder, &text, now, false).await?;
        }
        FetchError::NotFound => {
            repo::sync_failed(pool, account.id, row_folder, &text, now + poll, false).await?;
        }
    }
    Ok(())
}
