//! The queue: enqueue, claim, mark sent/failed. Safe with many workers:
//! every pick uses Postgres row locks (`FOR UPDATE SKIP LOCKED`).
use super::rules::{self, Counts, Decision, SendState};
use super::senders::{RESERVED_HEADERS, SendReceipt, new_message_id};
use super::settings::SendingSettings;
use crate::replies::model::{BounceKind, JobKind};
use crate::accounts::model::{ACCOUNT_COLS, AccountStatus, EmailAccount};
use chrono::{DateTime, Duration, Utc};
use rand::SeedableRng;
use rand::rngs::StdRng;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgConnection, PgPool};
use std::collections::{BTreeMap, HashSet};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(sqlx::Type, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, ToSchema)]
#[sqlx(type_name = "job_status", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Sending,
    Sent,
    Failed,
    Cancelled,
    Skipped,
    Stuck,
}

pub const JOB_COLS: &str = "id, account_id, idempotency_key, to_email::text AS to_email, to_name, recipient_domain, subject, \
    body_html, body_text, headers, in_reply_to, \"references\", lead_id, campaign_id, metadata, priority, send_after, \
    status, attempts, last_error, message_id_header, provider_message_id, provider_thread_id, created_at, claimed_at, \
    sent_at, updated_at, kind, replied_at, bounced_at, bounce_kind, enrollment_id, step_number";

#[derive(Clone, Debug, FromRow, Serialize, ToSchema)]
pub struct Job {
    pub id: Uuid,
    pub account_id: Uuid,
    pub idempotency_key: Option<String>,
    pub to_email: String,
    pub to_name: Option<String>,
    pub recipient_domain: String,
    pub subject: String,
    pub body_html: Option<String>,
    pub body_text: Option<String>,
    #[schema(value_type = Object)]
    pub headers: Value,
    pub in_reply_to: Option<String>,
    #[sqlx(rename = "references")]
    pub references: Option<String>,
    pub lead_id: Option<Uuid>,
    pub campaign_id: Option<Uuid>,
    #[schema(value_type = Object)]
    pub metadata: Value,
    pub priority: i16,
    pub send_after: Option<DateTime<Utc>>,
    pub status: JobStatus,
    pub attempts: i32,
    pub last_error: Option<String>,
    pub message_id_header: Option<String>,
    pub provider_message_id: Option<String>,
    pub provider_thread_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub claimed_at: Option<DateTime<Utc>>,
    pub sent_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
    /// `automated` (campaign, follow-up) or `manual` (written by a person).
    pub kind: JobKind,
    /// Set by the reply check when the lead answered.
    pub replied_at: Option<DateTime<Utc>>,
    /// Set by the reply check (or at send time) when the email bounced.
    pub bounced_at: Option<DateTime<Utc>>,
    pub bounce_kind: Option<BounceKind>,
    /// Set when a follow-up sequence made this job.
    pub enrollment_id: Option<Uuid>,
    /// The step of the sequence (1 = first email).
    pub step_number: Option<i32>,
}

/// SQL test for a test email: `metadata.test = true`.
const IS_TEST_SQL: &str = "(metadata->>'test') = 'true'";

impl Job {
    /// A test email (`metadata.test = true`) goes out at once: no window, cap or gap. It does not count as a send.
    pub fn is_test(&self) -> bool {
        self.metadata.get("test").and_then(Value::as_bool) == Some(true)
    }
}

#[derive(Clone, Debug, FromRow, Serialize, ToSchema)]
pub struct EventRow {
    pub id: i64,
    pub job_id: Option<Uuid>,
    pub account_id: Uuid,
    pub event: String,
    #[schema(value_type = Object)]
    pub detail: Value,
    pub at: DateTime<Utc>,
}

/// Append one row to the audit log.
pub async fn log_event(
    conn: &mut PgConnection,
    job_id: Option<Uuid>,
    account_id: Uuid,
    event: &str,
    detail: Value,
    at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO send_events (job_id, account_id, event, detail, at) VALUES ($1, $2, $3, $4, $5)")
        .bind(job_id)
        .bind(account_id)
        .bind(event)
        .bind(detail)
        .bind(at)
        .execute(conn)
        .await?;
    Ok(())
}

// ---------- enqueue ----------

#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct NewJob {
    pub account_id: Uuid,
    pub to_email: String,
    pub to_name: Option<String>,
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    /// Extra headers. `From`, `To`, `Subject`, `Message-ID`, `In-Reply-To`, `References`, `List-Unsubscribe` are set by the engine.
    pub headers: Option<BTreeMap<String, String>>,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    /// Gmail thread id, so a follow-up lands in the same thread.
    pub provider_thread_id: Option<String>,
    /// Not before this time.
    pub send_after: Option<DateTime<Utc>>,
    /// Higher goes first. Default 0.
    pub priority: Option<i16>,
    /// A repeat key returns the old job, not a new one.
    pub idempotency_key: Option<String>,
    pub lead_id: Option<Uuid>,
    pub campaign_id: Option<Uuid>,
    #[schema(value_type = Option<Object>)]
    pub metadata: Option<Value>,
    /// `automated` (default) or `manual`. A manual job is still sent to an address that replied;
    /// it is blocked by every other suppression.
    /// Set `metadata.test = true` for a test email: it goes out at once, outside the window and caps.
    pub kind: Option<JobKind>,
}

#[derive(Debug, Serialize, ToSchema, Clone, PartialEq)]
pub struct ItemError {
    pub code: String,
    pub message: String,
}

fn item_error(code: &str, message: impl Into<String>) -> ItemError {
    ItemError { code: code.into(), message: message.into() }
}

#[derive(Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ItemStatus {
    Queued,
    Duplicate,
    Error,
}

#[derive(Serialize, ToSchema)]
pub struct EnqueueItem {
    pub index: usize,
    pub status: ItemStatus,
    pub job: Option<Job>,
    pub error: Option<ItemError>,
}

/// A job that passed the checks, with a clean address.
struct Prepared {
    to_email: String,
    domain: String,
}

fn validate_new_job(j: &NewJob) -> Result<Prepared, ItemError> {
    let email = j.to_email.trim().to_lowercase();
    if email.parse::<lettre::Address>().is_err() {
        return Err(item_error("invalid_to_email", format!("'{}' is not a valid email address.", j.to_email)));
    }
    let domain = email.rsplit('@').next().unwrap_or_default().to_string();
    let subject = j.subject.trim();
    if subject.is_empty() {
        return Err(item_error("invalid_subject", "The subject must not be empty."));
    }
    if j.subject.contains(['\r', '\n']) || j.subject.len() > 998 {
        return Err(item_error("invalid_subject", "The subject must be one line, up to 998 characters."));
    }
    let has = |b: &Option<String>| b.as_deref().is_some_and(|t| !t.trim().is_empty());
    if !has(&j.body_text) && !has(&j.body_html) {
        return Err(item_error("missing_body", "Give body_text, body_html, or both."));
    }
    for (name, value) in j.headers.iter().flatten() {
        let ok_name = !name.is_empty() && name.bytes().all(|b| b.is_ascii_graphic() && b != b':');
        if !ok_name {
            return Err(item_error("invalid_header", format!("'{name}' is not a valid header name.")));
        }
        if RESERVED_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
            return Err(item_error("reserved_header", format!("The header '{name}' is set by the engine. Remove it.")));
        }
        if value.contains(['\r', '\n']) {
            return Err(item_error("invalid_header", format!("The value of '{name}' must be one line.")));
        }
    }
    for (name, v) in [("in_reply_to", &j.in_reply_to), ("references", &j.references)] {
        if v.as_deref().is_some_and(|t| t.contains(['\r', '\n'])) {
            return Err(item_error("invalid_header", format!("{name} must be one line.")));
        }
    }
    if j.idempotency_key.as_deref().is_some_and(|k| k.is_empty() || k.len() > 200) {
        return Err(item_error("invalid_idempotency_key", "idempotency_key must be 1 to 200 characters."));
    }
    if let Some(Value::Object(_) | Value::Null) | None = &j.metadata {
    } else {
        return Err(item_error("invalid_metadata", "metadata must be a JSON object."));
    }
    Ok(Prepared { to_email: email, domain })
}

/// Links a job to a follow-up enrollment.
#[derive(Clone, Copy, Debug)]
pub struct EnrollmentLink {
    pub enrollment_id: Uuid,
    pub step_number: i32,
}

/// Write one `send_jobs` row. `None` means the `idempotency_key` is taken (nothing was written).
pub async fn insert_job(
    conn: &mut PgConnection,
    j: &NewJob,
    to_email: &str,
    domain: &str,
    link: Option<EnrollmentLink>,
) -> Result<Option<Job>, sqlx::Error> {
    let headers = serde_json::to_value(j.headers.clone().unwrap_or_default()).unwrap_or(json!({}));
    sqlx::query_as(&format!(
        "INSERT INTO send_jobs (id, account_id, idempotency_key, to_email, to_name, recipient_domain, subject,
            body_html, body_text, headers, in_reply_to, \"references\", provider_thread_id, lead_id, campaign_id,
            metadata, priority, send_after, kind, enrollment_id, step_number, created_at, updated_at)
         VALUES ($1, $2, $3, $4::citext, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21,
            clock_timestamp(), clock_timestamp())
         ON CONFLICT (idempotency_key) DO NOTHING
         RETURNING {JOB_COLS}"
    ))
    .bind(Uuid::now_v7())
    .bind(j.account_id)
    .bind(&j.idempotency_key)
    .bind(to_email)
    .bind(&j.to_name)
    .bind(domain)
    .bind(j.subject.trim())
    .bind(&j.body_html)
    .bind(&j.body_text)
    .bind(headers)
    .bind(&j.in_reply_to)
    .bind(&j.references)
    .bind(&j.provider_thread_id)
    .bind(j.lead_id)
    .bind(j.campaign_id)
    .bind(j.metadata.clone().filter(|m| !m.is_null()).unwrap_or(json!({})))
    .bind(j.priority.unwrap_or(0))
    .bind(j.send_after)
    .bind(j.kind.unwrap_or(JobKind::Automated))
    .bind(link.map(|l| l.enrollment_id))
    .bind(link.map(|l| l.step_number))
    .fetch_optional(conn)
    .await
}

/// Checks one job from the API. Returns the clean address and its domain.
pub fn check_new_job(j: &NewJob) -> Result<(String, String), ItemError> {
    validate_new_job(j).map(|p| (p.to_email, p.domain))
}

/// Queue 1 to 1000 emails. Bad items get an error each. Good items are still queued.
pub async fn enqueue(pool: &PgPool, jobs: Vec<NewJob>, now: DateTime<Utc>) -> Result<Vec<EnqueueItem>, sqlx::Error> {
    let mut results: Vec<Option<EnqueueItem>> = (0..jobs.len()).map(|_| None).collect();
    let mut prepared: Vec<Option<Prepared>> = Vec::with_capacity(jobs.len());
    for (i, j) in jobs.iter().enumerate() {
        match validate_new_job(j) {
            Ok(p) => prepared.push(Some(p)),
            Err(e) => {
                prepared.push(None);
                results[i] = Some(EnqueueItem { index: i, status: ItemStatus::Error, job: None, error: Some(e) });
            }
        }
    }
    let wanted: Vec<Uuid> = jobs.iter().map(|j| j.account_id).collect::<HashSet<_>>().into_iter().collect();
    let known: Vec<(Uuid,)> = sqlx::query_as("SELECT id FROM email_accounts WHERE id = ANY($1) AND deleted_at IS NULL")
        .bind(&wanted)
        .fetch_all(pool)
        .await?;
    let known: HashSet<Uuid> = known.into_iter().map(|r| r.0).collect();

    let mut tx = pool.begin().await?;
    for (i, j) in jobs.iter().enumerate() {
        let Some(p) = &prepared[i] else { continue };
        if !known.contains(&j.account_id) {
            results[i] = Some(EnqueueItem {
                index: i,
                status: ItemStatus::Error,
                job: None,
                error: Some(item_error("account_not_found", "No account with this id.")),
            });
            continue;
        }
        let inserted = insert_job(&mut tx, j, &p.to_email, &p.domain, None).await?;
        results[i] = Some(match inserted {
            Some(job) => {
                log_event(&mut tx, Some(job.id), job.account_id, "queued", json!({}), now).await?;
                EnqueueItem { index: i, status: ItemStatus::Queued, job: Some(job), error: None }
            }
            None => {
                // The key is taken: return the old job.
                let old: Job = sqlx::query_as(&format!("SELECT {JOB_COLS} FROM send_jobs WHERE idempotency_key = $1"))
                    .bind(&j.idempotency_key)
                    .fetch_one(&mut *tx)
                    .await?;
                EnqueueItem { index: i, status: ItemStatus::Duplicate, job: Some(old), error: None }
            }
        });
    }
    tx.commit().await?;
    Ok(results.into_iter().flatten().collect())
}

// ---------- read, cancel, requeue ----------

pub async fn get_job(pool: &PgPool, id: Uuid) -> Result<Option<Job>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {JOB_COLS} FROM send_jobs WHERE id = $1")).bind(id).fetch_optional(pool).await
}

pub async fn job_events(pool: &PgPool, id: Uuid) -> Result<Vec<EventRow>, sqlx::Error> {
    sqlx::query_as("SELECT id, job_id, account_id, event, detail, at FROM send_events WHERE job_id = $1 ORDER BY id")
        .bind(id)
        .fetch_all(pool)
        .await
}

#[derive(Default)]
pub struct JobFilter {
    pub status: Option<JobStatus>,
    pub account_id: Option<Uuid>,
    pub campaign_id: Option<Uuid>,
    pub lead_id: Option<Uuid>,
    pub cursor: Option<Uuid>,
    pub limit: i64,
}

/// Newest first. Cursor paging by job id (ids sort by time).
pub async fn list_jobs(pool: &PgPool, f: &JobFilter) -> Result<(Vec<Job>, Option<Uuid>), sqlx::Error> {
    let mut jobs: Vec<Job> = sqlx::query_as(&format!(
        "SELECT {JOB_COLS} FROM send_jobs
         WHERE ($1::job_status IS NULL OR status = $1)
           AND ($2::uuid IS NULL OR account_id = $2)
           AND ($3::uuid IS NULL OR campaign_id = $3)
           AND ($4::uuid IS NULL OR lead_id = $4)
           AND ($5::uuid IS NULL OR id < $5)
         ORDER BY id DESC LIMIT $6"
    ))
    .bind(f.status)
    .bind(f.account_id)
    .bind(f.campaign_id)
    .bind(f.lead_id)
    .bind(f.cursor)
    .bind(f.limit + 1)
    .fetch_all(pool)
    .await?;
    let next = if jobs.len() as i64 > f.limit {
        jobs.truncate(f.limit as usize);
        jobs.last().map(|j| j.id)
    } else {
        None
    };
    Ok((jobs, next))
}

pub enum JobChangeError {
    NotFound,
    WrongStatus(JobStatus),
    Db(sqlx::Error),
}

impl From<sqlx::Error> for JobChangeError {
    fn from(e: sqlx::Error) -> Self {
        JobChangeError::Db(e)
    }
}

/// Cancel. Works only while `queued`.
pub async fn cancel(pool: &PgPool, id: Uuid, now: DateTime<Utc>) -> Result<Job, JobChangeError> {
    let mut tx = pool.begin().await?;
    // Lock order everywhere: the enrollment first, then the job. This avoids a deadlock with a stop.
    let enrollment: Option<(Option<Uuid>,)> =
        sqlx::query_as("SELECT enrollment_id FROM send_jobs WHERE id = $1").bind(id).fetch_optional(&mut *tx).await?;
    if let Some((Some(eid),)) = enrollment {
        sqlx::query("SELECT id FROM enrollments WHERE id = $1 FOR UPDATE").bind(eid).execute(&mut *tx).await?;
    }
    let job: Option<Job> = sqlx::query_as(&format!(
        "UPDATE send_jobs SET status = 'cancelled', updated_at = $2 WHERE id = $1 AND status = 'queued' RETURNING {JOB_COLS}"
    ))
    .bind(id)
    .bind(now)
    .fetch_optional(&mut *tx)
    .await?;
    match job {
        Some(job) => {
            log_event(&mut tx, Some(id), job.account_id, "cancelled", json!({}), now).await?;
            if let Some(eid) = job.enrollment_id {
                crate::followups::on_job_result(&mut tx, eid, &job, crate::followups::JobOutcome::Cancelled("the job was cancelled by hand".into()), now).await?;
            }
            tx.commit().await?;
            Ok(job)
        }
        None => match get_job(pool, id).await? {
            None => Err(JobChangeError::NotFound),
            Some(j) => Err(JobChangeError::WrongStatus(j.status)),
        },
    }
}

/// Put a `stuck` or `failed` job back in the queue, on purpose.
pub async fn requeue(pool: &PgPool, id: Uuid, now: DateTime<Utc>) -> Result<Job, JobChangeError> {
    let mut tx = pool.begin().await?;
    let job: Option<Job> = sqlx::query_as(&format!(
        "UPDATE send_jobs SET status = 'queued', attempts = 0, send_after = NULL, claimed_at = NULL, updated_at = $2
         WHERE id = $1 AND status IN ('stuck', 'failed')
           AND EXISTS (SELECT 1 FROM email_accounts a WHERE a.id = send_jobs.account_id AND a.deleted_at IS NULL)
         RETURNING {JOB_COLS}"
    ))
    .bind(id)
    .bind(now)
    .fetch_optional(&mut *tx)
    .await?;
    match job {
        Some(job) => {
            log_event(&mut tx, Some(id), job.account_id, "requeued", json!({}), now).await?;
            tx.commit().await?;
            Ok(job)
        }
        None => match get_job(pool, id).await? {
            None => Err(JobChangeError::NotFound),
            Some(j) => Err(JobChangeError::WrongStatus(j.status)),
        },
    }
}

// ---------- engine state in the DB ----------

#[derive(Clone, Debug, FromRow)]
pub struct StateRow {
    pub account_id: Uuid,
    pub next_send_at: Option<DateTime<Utc>>,
    pub local_day: Option<chrono::NaiveDate>,
    pub sent_today: i32,
    pub today_limit: Option<i32>,
    pub warmup_day: i32,
    pub consecutive_failures: i32,
    pub paused: bool,
    pub paused_reason: Option<String>,
    pub paused_at: Option<DateTime<Utc>>,
    pub last_sent_at: Option<DateTime<Utc>>,
}

pub const STATE_COLS: &str = "account_id, next_send_at, local_day, sent_today, today_limit, warmup_day, consecutive_failures, \
    paused, paused_reason, paused_at, last_sent_at";

impl StateRow {
    pub fn rules_state(&self) -> SendState {
        SendState {
            next_send_at: self.next_send_at,
            local_day: self.local_day,
            sent_today: self.sent_today,
            today_limit: self.today_limit,
            warmup_day: self.warmup_day,
        }
    }
}

pub async fn load_settings(conn: &mut PgConnection, account_id: Uuid) -> Result<SendingSettings, sqlx::Error> {
    let row: Option<(Value,)> = sqlx::query_as("SELECT settings FROM account_sending_settings WHERE account_id = $1")
        .bind(account_id)
        .fetch_optional(&mut *conn)
        .await?;
    match row {
        Some((v,)) => Ok(SendingSettings::from_stored(v)),
        None => load_defaults(conn).await,
    }
}

pub async fn load_defaults(conn: &mut PgConnection) -> Result<SendingSettings, sqlx::Error> {
    let (v,): (Value,) = sqlx::query_as("SELECT settings FROM sending_defaults WHERE id = 1").fetch_one(conn).await?;
    Ok(SendingSettings::from_stored(v))
}

/// Times of the sends in the last 60 minutes.
pub async fn recent_sends(conn: &mut PgConnection, account_id: Uuid, now: DateTime<Utc>) -> Result<Vec<DateTime<Utc>>, sqlx::Error> {
    let rows: Vec<(DateTime<Utc>,)> =
        sqlx::query_as("SELECT sent_at FROM send_jobs WHERE account_id = $1 AND status = 'sent' AND sent_at > $2")
            .bind(account_id)
            .bind(now - Duration::hours(1))
            .fetch_all(conn)
            .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

pub async fn engine_paused(pool: &PgPool) -> Result<bool, sqlx::Error> {
    let (p,): (bool,) = sqlx::query_as("SELECT engine_paused FROM sending_defaults WHERE id = 1").fetch_one(pool).await?;
    Ok(p)
}

/// Accounts that may be ready to send: active, not paused, has a due queued job, gap is over.
/// A due test email makes the account ready even when the gap is not over.
pub async fn ready_accounts(pool: &PgPool, now: DateTime<Utc>, limit: i64) -> Result<Vec<Uuid>, sqlx::Error> {
    let rows: Vec<(Uuid,)> = sqlx::query_as(&format!(
        "SELECT a.id FROM email_accounts a
         JOIN account_send_state s ON s.account_id = a.id
         WHERE a.status = 'active' AND a.deleted_at IS NULL AND NOT s.paused
           AND (s.next_send_at IS NULL OR s.next_send_at <= $1
                OR EXISTS (SELECT 1 FROM send_jobs j WHERE j.account_id = a.id AND j.status = 'queued'
                           AND (j.send_after IS NULL OR j.send_after <= $1) AND j.{IS_TEST_SQL}))
           AND EXISTS (SELECT 1 FROM send_jobs j WHERE j.account_id = a.id AND j.status = 'queued'
                       AND (j.send_after IS NULL OR j.send_after <= $1))
         ORDER BY s.next_send_at NULLS FIRST, a.id
         LIMIT $2"
    ))
    .bind(now)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| r.0).collect())
}

/// Jobs in `sending` for longer than `max_age` become `stuck`. They are NOT resent:
/// a double email to a lead is worse than a missed one.
pub async fn mark_stuck(pool: &PgPool, now: DateTime<Utc>, max_age: Duration) -> Result<Vec<(Uuid, Uuid)>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "UPDATE send_jobs SET status = 'stuck', updated_at = $1,
                last_error = 'No result after the send started. The email may or may not have gone out. It is not resent on its own.'
         WHERE status = 'sending' AND claimed_at < $2
         RETURNING id, account_id",
    )
    .bind(now)
    .bind(now - max_age)
    .fetch_all(&mut *tx)
    .await?;
    for (job_id, account_id) in &rows {
        log_event(&mut tx, Some(*job_id), *account_id, "stuck", json!({ "reason": "sending too long" }), now).await?;
    }
    tx.commit().await?;
    Ok(rows)
}

// ---------- claim ----------

pub struct Claimed {
    pub job: Job,
    pub account: EmailAccount,
    pub settings: SendingSettings,
}

pub enum ClaimOutcome {
    Claimed(Box<Claimed>),
    /// Another worker holds this account, or one of its emails is still being sent.
    Busy,
    /// The account is not active, or paused.
    NotReady,
    /// A rule says wait.
    Waiting(Decision),
    /// Every queued job was skipped or deferred.
    NothingToSend,
}

/// Claim the next job of one account, in one short transaction.
/// Steps: lock the account state, roll the day over, check the rules, walk the queue.
pub async fn claim_next(pool: &PgPool, account_id: Uuid, now: DateTime<Utc>) -> Result<ClaimOutcome, sqlx::Error> {
    let mut rng = StdRng::from_entropy();
    let mut tx = pool.begin().await?;

    // One worker per account at a time.
    let state: Option<StateRow> = sqlx::query_as(&format!(
        "SELECT {STATE_COLS} FROM account_send_state WHERE account_id = $1 FOR UPDATE SKIP LOCKED"
    ))
    .bind(account_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(state) = state else { return Ok(ClaimOutcome::Busy) };
    if state.paused {
        return Ok(ClaimOutcome::NotReady);
    }
    let account: Option<EmailAccount> = sqlx::query_as(&format!(
        "SELECT {ACCOUNT_COLS} FROM email_accounts WHERE id = $1 AND deleted_at IS NULL"
    ))
    .bind(account_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(account) = account.filter(|a| a.status == AccountStatus::Active) else {
        return Ok(ClaimOutcome::NotReady);
    };
    // At most one email at a time per account.
    let (sending,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM send_jobs WHERE account_id = $1 AND status = 'sending'")
            .bind(account_id)
            .fetch_one(&mut *tx)
            .await?;
    if sending > 0 {
        return Ok(ClaimOutcome::Busy);
    }

    let settings = load_settings(&mut tx, account_id).await?;

    // 0. A test email goes out now. It skips the window, caps and gap, and leaves the counters alone.
    let test: Option<Job> = sqlx::query_as(&format!(
        "SELECT {JOB_COLS} FROM send_jobs
         WHERE account_id = $1 AND status = 'queued' AND (send_after IS NULL OR send_after <= $2) AND {IS_TEST_SQL}
         ORDER BY created_at, id LIMIT 1 FOR UPDATE SKIP LOCKED"
    ))
    .bind(account_id)
    .bind(now)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(job) = test {
        let message_id = job.message_id_header.clone().unwrap_or_else(|| new_message_id(&account.email));
        let claimed: Job = sqlx::query_as(&format!(
            "UPDATE send_jobs SET status = 'sending', claimed_at = $2, attempts = attempts + 1,
                    message_id_header = $3, updated_at = $2
             WHERE id = $1 RETURNING {JOB_COLS}"
        ))
        .bind(job.id)
        .bind(now)
        .bind(message_id)
        .fetch_one(&mut *tx)
        .await?;
        log_event(&mut tx, Some(job.id), account_id, "claimed", json!({ "attempt": claimed.attempts, "test": true }), now).await?;
        tx.commit().await?;
        return Ok(ClaimOutcome::Claimed(Box::new(Claimed { job: claimed, account, settings })));
    }

    // 1. Day rollover.
    let before = state.rules_state();
    let rolled = rules::rollover(&settings, &before, now, &mut rng);
    if rolled != before {
        sqlx::query(
            "UPDATE account_send_state SET local_day = $2, sent_today = $3, today_limit = $4, warmup_day = $5, next_send_at = $6
             WHERE account_id = $1",
        )
        .bind(account_id)
        .bind(rolled.local_day)
        .bind(rolled.sent_today)
        .bind(rolled.today_limit)
        .bind(rolled.warmup_day)
        .bind(rolled.next_send_at)
        .execute(&mut *tx)
        .await?;
    }

    // 2. Gates.
    let counts = Counts { recent_sends: recent_sends(&mut tx, account_id, now).await? };
    if let Decision::Wait { reason, until } = rules::can_send(&settings, &rolled, &counts, now) {
        let decision = Decision::Wait { reason, until };
        // Park the account until then, so it is not looked at again on every tick.
        if rolled.next_send_at.is_none_or(|n| n < until) {
            sqlx::query("UPDATE account_send_state SET next_send_at = $2 WHERE account_id = $1")
                .bind(account_id)
                .bind(until)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        return Ok(ClaimOutcome::Waiting(decision));
    }

    // 3. Claim a job.
    let today = rules::local_date(&settings, now);
    let day_start = rules::day_start(&settings, today);
    let next_day_start = rules::day_start(&settings, today.succ_opt().unwrap_or(today));
    let candidates: Vec<Job> = sqlx::query_as(&format!(
        "SELECT {JOB_COLS} FROM send_jobs
         WHERE account_id = $1 AND status = 'queued' AND (send_after IS NULL OR send_after <= $2)
         ORDER BY priority DESC, CASE WHEN $3 AND step_number > 1 THEN 0 ELSE 1 END, created_at, id
         LIMIT 20 FOR UPDATE SKIP LOCKED"
    ))
    .bind(account_id)
    .bind(now)
    .bind(settings.followups_first)
    .fetch_all(&mut *tx)
    .await?;

    for job in candidates {
        // A follow-up job belongs to an enrollment. Take the enrollment lock without waiting: a stop that
        // holds it right now wins, and this job is looked at again on the next tick.
        if let Some(enrollment_id) = job.enrollment_id {
            let waiting: Option<(String,)> = sqlx::query_as(
                "SELECT status::text FROM enrollments WHERE id = $1 FOR UPDATE SKIP LOCKED",
            )
            .bind(enrollment_id)
            .fetch_optional(&mut *tx)
            .await?;
            match waiting {
                None => continue,
                Some((status,)) if status != "waiting_send" => {
                    // Should not happen: a stop cancels the queued job in the same transaction.
                    sqlx::query("UPDATE send_jobs SET status = 'cancelled', last_error = $2, updated_at = $3 WHERE id = $1")
                        .bind(job.id)
                        .bind(format!("the enrollment is {status}"))
                        .bind(now)
                        .execute(&mut *tx)
                        .await?;
                    log_event(&mut tx, Some(job.id), account_id, "cancelled", json!({ "reason": format!("enrollment {status}") }), now).await?;
                    continue;
                }
                Some(_) => {}
            }
        }

        // Suppressed address or domain: skip the job. This does not start the gap timer.
        let suppressed: Option<(String,)> = sqlx::query_as(
            "SELECT reason::text FROM suppressions
             WHERE ((kind = 'email' AND value = $1::citext) OR (kind = 'domain' AND value = $2::citext))
               AND ($3 = false OR reason <> 'replied') LIMIT 1",
        )
        .bind(&job.to_email)
        .bind(&job.recipient_domain)
        // A manual job (a person's own answer) skips only the `replied` suppression.
        .bind(job.kind == JobKind::Manual)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((reason,)) = suppressed {
            sqlx::query("UPDATE send_jobs SET status = 'skipped', last_error = $2, updated_at = $3 WHERE id = $1")
                .bind(job.id)
                .bind(format!("suppressed: {reason}"))
                .bind(now)
                .execute(&mut *tx)
                .await?;
            log_event(&mut tx, Some(job.id), account_id, "skipped", json!({ "reason": reason }), now).await?;
            if let Some(enrollment_id) = job.enrollment_id {
                crate::followups::on_job_result(
                    &mut tx,
                    enrollment_id,
                    &job,
                    crate::followups::JobOutcome::Skipped(format!("suppressed: {reason}")),
                    now,
                )
                .await?;
            }
            continue;
        }

        // Follow-up safety gate: a follow-up goes out only if the inbox was read lately. A broken inbox read must
        // never let follow-ups go out blind: a double email to a lead who replied is worse than a late one.
        if settings.reply_check_max_age_mins > 0 && job.kind == JobKind::Automated && (job.in_reply_to.is_some() || job.step_number.is_some_and(|n| n > 1)) {
            if let Some(block) = reply_check_block(&mut tx, &account, &settings, now).await? {
                defer(&mut tx, &job, now + Duration::minutes(5), block.as_str(), now).await?;
                continue;
            }
        }

        // Same address got mail lately, from any account: wait until the cooldown ends.
        if settings.same_recipient_cooldown_hours > 0 {
            // Two accounts must not claim the same address at the same moment: neither would see
            // the other's claim. Take a lock on the address, held until this transaction ends.
            // If another transaction holds it, leave the job for the next tick (no waiting, so no deadlock).
            let (locked,): (bool,) = sqlx::query_as("SELECT pg_try_advisory_xact_lock(hashtextextended($1, 7306002))")
                .bind(job.to_email.to_lowercase())
                .fetch_one(&mut *tx)
                .await?;
            if !locked {
                continue;
            }
            let cooldown = Duration::hours(settings.same_recipient_cooldown_hours as i64);
            let (last,): (Option<DateTime<Utc>>,) = sqlx::query_as(
                "SELECT max(COALESCE(sent_at, claimed_at)) FROM send_jobs
                 WHERE to_email = $1::citext AND id <> $2 AND status IN ('sent', 'sending', 'stuck')
                   AND COALESCE(sent_at, claimed_at) > $3",
            )
            .bind(&job.to_email)
            .bind(job.id)
            .bind(now - cooldown)
            .fetch_one(&mut *tx)
            .await?;
            if let Some(last) = last {
                let until = last + cooldown;
                defer(&mut tx, &job, until, "same_recipient_cooldown", now).await?;
                continue;
            }
        }

        // One company got enough mail today: try again at the next allowed window start.
        if !settings.is_domain_exempt(&job.recipient_domain) {
            let (count,): (i64,) = sqlx::query_as(
                "SELECT count(*) FROM send_jobs
                 WHERE account_id = $1 AND recipient_domain = $2 AND status IN ('sent', 'sending', 'stuck')
                   AND COALESCE(sent_at, claimed_at) >= $3 AND COALESCE(sent_at, claimed_at) < $4",
            )
            .bind(account_id)
            .bind(&job.recipient_domain)
            .bind(day_start)
            .bind(next_day_start)
            .fetch_one(&mut *tx)
            .await?;
            if count >= settings.per_domain_daily_limit as i64 {
                let until = rules::next_window_open(&settings, next_day_start.max(now));
                defer(&mut tx, &job, until, "per_domain_daily_limit", now).await?;
                continue;
            }
        }

        // This job passes. Claim it.
        let message_id = job.message_id_header.clone().unwrap_or_else(|| new_message_id(&account.email));
        let claimed: Job = sqlx::query_as(&format!(
            "UPDATE send_jobs SET status = 'sending', claimed_at = $2, attempts = attempts + 1,
                    message_id_header = $3, updated_at = $2
             WHERE id = $1 RETURNING {JOB_COLS}"
        ))
        .bind(job.id)
        .bind(now)
        .bind(message_id)
        .fetch_one(&mut *tx)
        .await?;
        // A placeholder, so no other worker sends from this inbox meanwhile. The real gap is set after the send.
        sqlx::query("UPDATE account_send_state SET next_send_at = $2 WHERE account_id = $1")
            .bind(account_id)
            .bind(now + Duration::seconds(settings.base_delay_secs as i64))
            .execute(&mut *tx)
            .await?;
        log_event(&mut tx, Some(job.id), account_id, "claimed", json!({ "attempt": claimed.attempts }), now).await?;
        tx.commit().await?;
        return Ok(ClaimOutcome::Claimed(Box::new(Claimed { job: claimed, account, settings })));
    }
    tx.commit().await?;
    Ok(ClaimOutcome::NothingToSend)
}

/// Why the follow-up gate is closed, if it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateBlock {
    /// The inbox was not read within `reply_check_max_age_mins`.
    Stale,
    /// The inbox cannot be read at all (`can_read = false`).
    Unavailable,
}

impl GateBlock {
    pub fn as_str(self) -> &'static str {
        match self {
            GateBlock::Stale => "reply_check_stale",
            GateBlock::Unavailable => "reply_check_unavailable",
        }
    }
    pub fn wait_reason(self) -> rules::WaitReason {
        match self {
            GateBlock::Stale => rules::WaitReason::ReplyCheckStale,
            GateBlock::Unavailable => rules::WaitReason::ReplyCheckUnavailable,
        }
    }
}

/// The follow-up gate. Open (`None`) when the oldest folder of the account was read successfully
/// within `reply_check_max_age_mins`. A folder that was never read counts as stale.
pub async fn reply_check_block(
    conn: &mut PgConnection,
    account: &EmailAccount,
    s: &SendingSettings,
    now: DateTime<Utc>,
) -> Result<Option<GateBlock>, sqlx::Error> {
    if s.reply_check_max_age_mins == 0 {
        return Ok(None);
    }
    if !account.can_read {
        return Ok(Some(GateBlock::Unavailable));
    }
    let (rows, never, oldest): (i64, bool, Option<DateTime<Utc>>) = sqlx::query_as(
        "SELECT count(*), COALESCE(bool_or(last_success_at IS NULL), false), min(last_success_at)
         FROM inbox_sync_state WHERE account_id = $1",
    )
    .bind(account.id)
    .fetch_one(conn)
    .await?;
    let fresh = rows > 0 && !never && oldest.is_some_and(|t| now - t <= Duration::minutes(s.reply_check_max_age_mins as i64));
    Ok((!fresh).then_some(GateBlock::Stale))
}

async fn defer(
    tx: &mut PgConnection,
    job: &Job,
    until: DateTime<Utc>,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE send_jobs SET send_after = $2, updated_at = $3 WHERE id = $1")
        .bind(job.id)
        .bind(until)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    log_event(tx, Some(job.id), job.account_id, "deferred", json!({ "reason": reason, "until": until }), now).await
}

// ---------- finish ----------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureChange {
    Reset,
    Add,
    Keep,
}

/// What to write after a send attempt. Made by the pure function `worker::plan_outcome`.
#[derive(Clone, Debug)]
pub struct Plan {
    pub job_status: JobStatus,
    pub send_after: Option<DateTime<Utc>>,
    /// Rate limit and login errors are not a real try: give the attempt back.
    pub refund_attempt: bool,
    pub error: Option<String>,
    pub receipt: Option<SendReceipt>,
    pub next_send_at: DateTime<Utc>,
    pub count_sent: bool,
    pub failures: FailureChange,
    pub disconnect_reason: Option<String>,
    /// Hard bounce: add the address to `suppressions`.
    pub suppress: bool,
    pub event: &'static str,
    pub detail: Value,
}

/// Write the result of a send attempt: job, counters, events, suppression, auto-pause.
pub async fn apply_plan(pool: &PgPool, claimed: &Claimed, plan: &Plan, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    let job = &claimed.job;
    let account_id = job.account_id;
    let s = &claimed.settings;
    let mut tx = pool.begin().await?;
    // Lock order everywhere: the enrollment first, then the account state, then the job.
    if let Some(enrollment_id) = job.enrollment_id {
        sqlx::query("SELECT id FROM enrollments WHERE id = $1 FOR UPDATE").bind(enrollment_id).execute(&mut *tx).await?;
    }
    let state: StateRow = sqlx::query_as(&format!(
        "SELECT {STATE_COLS} FROM account_send_state WHERE account_id = $1 FOR UPDATE"
    ))
    .bind(account_id)
    .fetch_one(&mut *tx)
    .await?;

    let receipt = plan.receipt.clone().unwrap_or_default();
    sqlx::query(
        "UPDATE send_jobs SET status = $2, send_after = $3, attempts = attempts - $4, last_error = $5,
                provider_message_id = COALESCE($6, provider_message_id), provider_thread_id = COALESCE($7, provider_thread_id),
                message_id_header = COALESCE($9, message_id_header),
                sent_at = CASE WHEN $2 = 'sent'::job_status THEN $8 ELSE sent_at END, updated_at = $8
         WHERE id = $1",
    )
    .bind(job.id)
    .bind(plan.job_status)
    .bind(plan.send_after)
    .bind(i32::from(plan.refund_attempt))
    .bind(&plan.error)
    .bind(receipt.provider_message_id)
    .bind(receipt.provider_thread_id)
    .bind(now)
    .bind(receipt.message_id_header)
    .execute(&mut *tx)
    .await?;

    let failures = match plan.failures {
        FailureChange::Reset => 0,
        FailureChange::Add => state.consecutive_failures + 1,
        FailureChange::Keep => state.consecutive_failures,
    };
    // A test email does not move the gap or the daily count.
    let test = job.is_test();
    sqlx::query(
        "UPDATE account_send_state SET next_send_at = $2, sent_today = sent_today + $3,
                last_sent_at = CASE WHEN $3 = 1 THEN $4 ELSE last_sent_at END, consecutive_failures = $5
         WHERE account_id = $1",
    )
    .bind(account_id)
    .bind(if test { state.next_send_at } else { Some(plan.next_send_at) })
    .bind(i32::from(plan.count_sent && !test))
    .bind(now)
    .bind(failures)
    .execute(&mut *tx)
    .await?;

    log_event(&mut tx, Some(job.id), account_id, plan.event, plan.detail.clone(), now).await?;

    // A sent email (a step or a manual send) is on the timeline of the lead. A failed lead update never blocks the result.
    if plan.job_status == JobStatus::Sent {
        crate::leads::hooks::on_sent(&mut tx, job, now).await;
    }

    if let Some(enrollment_id) = job.enrollment_id {
        let outcome = match plan.job_status {
            JobStatus::Sent => crate::followups::JobOutcome::Sent,
            JobStatus::Failed if plan.suppress => crate::followups::JobOutcome::HardBounce,
            JobStatus::Failed => {
                crate::followups::JobOutcome::Failed(plan.error.clone().unwrap_or_else(|| "the send failed".into()))
            }
            // Retry, rate limit, login problem, or "unknown result": the job lives on, the enrollment waits.
            _ => crate::followups::JobOutcome::NoChange,
        };
        crate::followups::on_job_result(&mut tx, enrollment_id, job, outcome, now).await?;
    }

    if plan.suppress {
        sqlx::query(
            "INSERT INTO suppressions (value, kind, reason) VALUES ($1::citext, 'email', 'hard_bounce') ON CONFLICT DO NOTHING",
        )
        .bind(&job.to_email)
        .execute(&mut *tx)
        .await?;
        // The bounce rate reads these two columns. A late bounce from the reply check sets the same pair.
        sqlx::query("UPDATE send_jobs SET bounced_at = $2, bounce_kind = 'hard' WHERE id = $1 AND bounced_at IS NULL")
            .bind(job.id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        log_event(&mut tx, Some(job.id), account_id, "hard_bounce", json!({ "to": job.to_email }), now).await?;
    }

    if let Some(reason) = &plan.disconnect_reason {
        sqlx::query("UPDATE email_accounts SET status = 'disconnected', last_error = $2, updated_at = $3 WHERE id = $1")
            .bind(account_id)
            .bind(reason)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        pause_state(&mut tx, account_id, reason, now).await?;
        log_event(&mut tx, None, account_id, "paused", json!({ "reason": reason, "disconnected": true }), now).await?;
    } else {
        // Auto-pause check.
        let pause_reason = if failures >= s.pause_after_failures as i32 {
            Some(format!("{failures} failures in a row. Last error: {}", plan.error.as_deref().unwrap_or("unknown")))
        } else {
            let count_blocks = crate::replies::repo::load_settings(&mut tx, account_id).await?.count_blocks_in_bounce_rate;
            super::health::bounce_pause_reason(&mut tx, account_id, s, count_blocks).await?
        };
        if let Some(reason) = pause_reason {
            if !state.paused {
                super::health::pause_for_reason(&mut tx, account_id, &reason, now).await?;
            }
        }
    }
    tx.commit().await
}

pub async fn pause_state(conn: &mut PgConnection, account_id: Uuid, reason: &str, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE account_send_state SET paused = true, paused_reason = $2, paused_at = $3 WHERE account_id = $1")
        .bind(account_id)
        .bind(reason)
        .bind(now)
        .execute(conn)
        .await?;
    Ok(())
}
