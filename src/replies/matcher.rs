//! Find the `send_jobs` row an inbound email answers. First match wins.
//! Methods 1 to 3 look at the `Message-ID` and search all accounts: a lead may reply to all,
//! and the mail can land in another connected inbox. Methods 4 to 6 search this account only.
use super::model::MatchMethod;
use chrono::{DateTime, Duration, Utc};
use sqlx::{FromRow, PgConnection};
use uuid::Uuid;

/// A bounce recipient counts only for sends this recent.
pub const BOUNCE_RECIPIENT_DAYS: i64 = 7;

#[derive(Clone, Debug, FromRow, PartialEq)]
pub struct MatchedJob {
    pub id: Uuid,
    pub account_id: Uuid,
    pub lead_id: Option<Uuid>,
    pub campaign_id: Option<Uuid>,
    pub to_email: String,
    pub recipient_domain: String,
    pub replied_at: Option<DateTime<Utc>>,
}

const JOB_COLS: &str = "id, account_id, lead_id, campaign_id, to_email::text AS to_email, recipient_domain, replied_at";

#[derive(Clone, Debug)]
pub struct MatchResult {
    pub method: MatchMethod,
    pub job: Option<MatchedJob>,
    /// For a bounce without `Final-Recipient`: the address from the body that we really sent to.
    pub resolved_recipient: Option<String>,
}

impl MatchResult {
    fn none() -> Self {
        Self { method: MatchMethod::None, job: None, resolved_recipient: None }
    }
    fn found(method: MatchMethod, job: MatchedJob) -> Self {
        Self { method, job: Some(job), resolved_recipient: None }
    }
}

/// Everything the matcher needs to know about one inbound email.
#[derive(Clone, Debug, Default)]
pub struct MatchInput<'a> {
    /// The inbox that got the email. `None` = search all accounts (for the dry-run route).
    pub account_id: Option<Uuid>,
    pub from_email: &'a str,
    pub in_reply_to: Option<&'a str>,
    pub references: &'a [String],
    pub thread_id: Option<&'a str>,
    /// `Message-ID` of the original, read from a bounce report.
    pub original_message_id: Option<&'a str>,
    /// The recipient named by a bounce report or a complaint.
    pub bounce_recipient: Option<&'a str>,
    /// Addresses found in the body of an old-style bounce.
    pub bounce_candidates: &'a [String],
    pub match_window_days: u32,
}

async fn by_message_id(conn: &mut PgConnection, ids: &[String]) -> Result<Option<MatchedJob>, sqlx::Error> {
    if ids.is_empty() {
        return Ok(None);
    }
    sqlx::query_as(&format!(
        "SELECT {JOB_COLS} FROM send_jobs WHERE message_id_header = ANY($1)
         ORDER BY sent_at DESC NULLS LAST, created_at DESC, id DESC LIMIT 1"
    ))
    .bind(ids)
    .fetch_optional(conn)
    .await
}

async fn recent_send_to(
    conn: &mut PgConnection,
    account: Option<Uuid>,
    to: &str,
    since: DateTime<Utc>,
) -> Result<Option<MatchedJob>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT {JOB_COLS} FROM send_jobs
         WHERE ($1::uuid IS NULL OR account_id = $1) AND to_email = $2::citext AND status = 'sent' AND sent_at > $3
         ORDER BY sent_at DESC, id DESC LIMIT 1"
    ))
    .bind(account)
    .bind(to)
    .bind(since)
    .fetch_optional(conn)
    .await
}

pub async fn find_match(conn: &mut PgConnection, input: &MatchInput<'_>, now: DateTime<Utc>) -> Result<MatchResult, sqlx::Error> {
    // 1. The original Message-ID inside a bounce report.
    if let Some(id) = input.original_message_id {
        if let Some(job) = by_message_id(conn, &[id.to_string()]).await? {
            return Ok(MatchResult::found(MatchMethod::DsnOriginal, job));
        }
    }
    // 2. In-Reply-To.
    if let Some(id) = input.in_reply_to {
        if let Some(job) = by_message_id(conn, &[id.to_string()]).await? {
            return Ok(MatchResult::found(MatchMethod::InReplyTo, job));
        }
    }
    // 3. Any id in References. The newest job wins.
    if let Some(job) = by_message_id(conn, input.references).await? {
        return Ok(MatchResult::found(MatchMethod::References, job));
    }
    // 4. The Gmail thread of this account.
    if let Some(thread) = input.thread_id {
        let job: Option<MatchedJob> = sqlx::query_as(&format!(
            "SELECT {JOB_COLS} FROM send_jobs
             WHERE ($1::uuid IS NULL OR account_id = $1) AND provider_thread_id = $2 AND status = 'sent'
             ORDER BY sent_at DESC, id DESC LIMIT 1"
        ))
        .bind(input.account_id)
        .bind(thread)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some(job) = job {
            return Ok(MatchResult::found(MatchMethod::Thread, job));
        }
    }
    // 5. The recipient named by a bounce report (or, for old servers, an address in the body).
    let since = now - Duration::days(BOUNCE_RECIPIENT_DAYS);
    let mut recipients: Vec<&str> = input.bounce_recipient.into_iter().collect();
    recipients.extend(input.bounce_candidates.iter().map(String::as_str));
    for r in recipients {
        if let Some(job) = recent_send_to(conn, input.account_id, r, since).await? {
            return Ok(MatchResult { resolved_recipient: Some(r.to_string()), ..MatchResult::found(MatchMethod::BounceRecipient, job) });
        }
    }
    // 6. The sender got a send from this account lately.
    let since = now - Duration::days(input.match_window_days as i64);
    if let Some(job) = recent_send_to(conn, input.account_id, input.from_email, since).await? {
        return Ok(MatchResult::found(MatchMethod::SenderAddress, job));
    }
    Ok(MatchResult::none())
}
