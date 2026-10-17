//! How an enrollment moves. Every function takes a connection that is inside a transaction,
//! so a change and its events are written together or not at all.
//!
//! Rules (the build map, section 7):
//! - Only the scheduler moves `active` to `waiting_send` (`scheduler.rs`).
//! - Only the send result moves `waiting_send` on (`on_job_result`).
//! - Only `on_lead_stopped` and the API move a row to `stopped` or `paused`.
//! - `stopped`, `finished` and `failed` never change again.
//!
//! Lock order, everywhere: the enrollment row first, then the job rows. A stop and a send result that
//! meet on the same enrollment then wait for each other; they never deadlock.
use super::model::{DelayType, ENROLLMENT_COLS, Enrollment, EnrollmentStatus};
use super::schedule::add_delay;
use crate::leads::hooks::{self as leads_hooks, Ended};
use crate::sending::queue::{self, Job, log_event};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

/// What happened to the send job of an enrollment.
#[derive(Clone, Debug, PartialEq)]
pub enum JobOutcome {
    Sent,
    /// The address bounced at send time (a permanent "no such user").
    HardBounce,
    /// A permanent error, or the retries ran out.
    Failed(String),
    /// The engine skipped the job (the address is suppressed).
    Skipped(String),
    /// The job was cancelled.
    Cancelled(String),
    /// A retry is planned, or the result is unknown (`stuck`). The enrollment keeps waiting.
    NoChange,
}

pub async fn log(
    conn: &mut PgConnection,
    enrollment_id: Uuid,
    kind: &str,
    step: Option<i32>,
    detail: Value,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO enrollment_events (enrollment_id, kind, step_number, detail, at) VALUES ($1, $2, $3, $4, $5)")
        .bind(enrollment_id)
        .bind(kind)
        .bind(step)
        .bind(detail)
        .bind(now)
        .execute(conn)
        .await?;
    Ok(())
}

/// Read one enrollment and lock the row.
pub async fn lock(conn: &mut PgConnection, id: Uuid) -> Result<Option<Enrollment>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {ENROLLMENT_COLS} FROM enrollments WHERE id = $1 FOR UPDATE")).bind(id).fetch_optional(conn).await
}

/// Cancel a job that is still `queued`. A job that is `sending` or later is left alone.
async fn cancel_queued_job(conn: &mut PgConnection, job_id: Uuid, reason: &str, now: DateTime<Utc>) -> Result<Option<Uuid>, sqlx::Error> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "UPDATE send_jobs SET status = 'cancelled', last_error = $2, updated_at = $3
         WHERE id = $1 AND status = 'queued' RETURNING account_id",
    )
    .bind(job_id)
    .bind(reason)
    .bind(now)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((account_id,)) = row else { return Ok(None) };
    log_event(conn, Some(job_id), account_id, "cancelled", json!({ "reason": reason }), now).await?;
    Ok(Some(job_id))
}

/// The enrollment ends for good. Cancels the queued job. Returns the job it cancelled.
pub async fn stop(conn: &mut PgConnection, e: &Enrollment, reason: &str, detail: Value, now: DateTime<Utc>) -> Result<Option<Uuid>, sqlx::Error> {
    end(conn, e, EnrollmentStatus::Stopped, reason, detail, now).await
}

/// The enrollment ends for good because something broke. Cancels the queued job.
pub async fn fail(conn: &mut PgConnection, e: &Enrollment, reason: &str, detail: Value, now: DateTime<Utc>) -> Result<Option<Uuid>, sqlx::Error> {
    end(conn, e, EnrollmentStatus::Failed, reason, detail, now).await
}

async fn end(
    conn: &mut PgConnection,
    e: &Enrollment,
    status: EnrollmentStatus,
    reason: &str,
    detail: Value,
    now: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    let cancelled = match e.current_job_id {
        Some(job_id) => cancel_queued_job(conn, job_id, &format!("{}: {reason}", if status == EnrollmentStatus::Failed { "failed" } else { "stopped" }), now).await?,
        None => None,
    };
    sqlx::query(
        "UPDATE enrollments SET status = $2, stop_reason = $3, next_due_at = NULL, pause_until = NULL, wait_reason = NULL, wait_since = NULL, updated_at = $4 WHERE id = $1",
    )
        .bind(e.id)
        .bind(status)
        .bind(reason)
        .bind(now)
        .execute(&mut *conn)
        .await?;
    let mut d = json!({ "reason": reason, "cancelled_job": cancelled });
    if let (Value::Object(base), Value::Object(more)) = (&mut d, detail) {
        base.extend(more);
    }
    let kind = if status == EnrollmentStatus::Failed { "failed" } else { "stopped" };
    log(conn, e.id, kind, Some(e.next_step), d, now).await?;
    let how = if status == EnrollmentStatus::Failed { Ended::Failed(reason) } else { Ended::Stopped(reason) };
    leads_hooks::on_enrollment_ended(conn, e, how, now).await;
    // The AI text for the steps that will never go out is not needed any more.
    crate::generator::hooks::on_enrollment_closed(conn, e.id, e.next_step, now).await;
    Ok(cancelled)
}

/// Last step sent: the sequence is done.
async fn finish(conn: &mut PgConnection, e: &Enrollment, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE enrollments SET status = 'finished', next_due_at = NULL, current_job_id = NULL, step_queued_at = NULL,
                pause_until = NULL, updated_at = $2 WHERE id = $1",
    )
    .bind(e.id)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    log(conn, e.id, "finished", Some(e.next_step), json!({}), now).await?;
    leads_hooks::on_enrollment_ended(conn, e, Ended::Finished, now).await;
    crate::generator::hooks::on_enrollment_closed(conn, e.id, e.next_step + 1, now).await;
    Ok(())
}

/// Pause. `until = None` waits for a person to resume. A queued job is cancelled: the step is made again
/// when the enrollment resumes. A job that is already being sent cannot be called back.
pub async fn pause(
    conn: &mut PgConnection,
    e: &Enrollment,
    until: Option<DateTime<Utc>>,
    why: &str,
    now: DateTime<Utc>,
) -> Result<Option<Uuid>, sqlx::Error> {
    let cancelled = match e.current_job_id {
        Some(job_id) => cancel_queued_job(conn, job_id, &format!("paused: {why}"), now).await?,
        None => None,
    };
    // A cancelled job means the step is not queued any more: due again as soon as the enrollment resumes.
    sqlx::query(
        "UPDATE enrollments SET status = 'paused', pause_until = $2, updated_at = $3,
                current_job_id = CASE WHEN $4 THEN NULL ELSE current_job_id END,
                step_queued_at = CASE WHEN $4 THEN NULL ELSE step_queued_at END,
                next_due_at = CASE WHEN $4 THEN $3 ELSE next_due_at END
         WHERE id = $1",
    )
    .bind(e.id)
    .bind(until)
    .bind(now)
    .bind(cancelled.is_some())
    .execute(&mut *conn)
    .await?;
    log(conn, e.id, "paused", Some(e.next_step), json!({ "why": why, "until": until, "cancelled_job": cancelled }), now).await?;
    Ok(cancelled)
}

/// Back from `paused`. The due time never moves into the past: a time that already passed becomes now.
pub async fn resume(conn: &mut PgConnection, e: &Enrollment, why: &str, now: DateTime<Utc>) -> Result<EnrollmentStatus, sqlx::Error> {
    let (status,): (EnrollmentStatus,) = sqlx::query_as(
        "UPDATE enrollments SET pause_until = NULL, updated_at = $2,
                status = CASE WHEN current_job_id IS NOT NULL THEN 'waiting_send'::enrollment_status ELSE 'active'::enrollment_status END,
                next_due_at = CASE WHEN current_job_id IS NOT NULL THEN NULL ELSE GREATEST(COALESCE(next_due_at, $2), $2) END
         WHERE id = $1 RETURNING status",
    )
    .bind(e.id)
    .bind(now)
    .fetch_one(&mut *conn)
    .await?;
    log(conn, e.id, "resumed", Some(e.next_step), json!({ "why": why }), now).await?;
    // AI text written before a long pause is made again (fresh lead data).
    crate::generator::hooks::on_resume(conn, e.id, now).await;
    Ok(status)
}

async fn timezone_of(conn: &mut PgConnection, account_id: Uuid) -> Result<chrono_tz::Tz, sqlx::Error> {
    Ok(queue::load_settings(conn, account_id).await?.tz())
}

/// The send worker calls this inside the transaction that saves the job result.
///
/// `Sent`: the next step gets its due time (counted from the real send time, so a late step pushes the rest
/// back), or the last step finishes the enrollment. A closed enrollment only gets an event.
pub async fn on_job_result(conn: &mut PgConnection, enrollment_id: Uuid, job: &Job, outcome: JobOutcome, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    if outcome == JobOutcome::NoChange {
        return Ok(());
    }
    let Some(e) = lock(conn, enrollment_id).await? else { return Ok(()) };
    let step = job.step_number;
    if !e.status.is_open() {
        if outcome == JobOutcome::Sent {
            // The email was already on its way when the enrollment ended.
            log(conn, e.id, "step_sent_after_end", step, json!({ "job_id": job.id, "status": e.status }), now).await?;
        }
        return Ok(());
    }
    if e.current_job_id != Some(job.id) {
        log(conn, e.id, "result_ignored", step, json!({ "job_id": job.id, "reason": "not the job this enrollment waits on" }), now).await?;
        return Ok(());
    }
    match outcome {
        JobOutcome::NoChange => {}
        JobOutcome::Sent => {
            log(conn, e.id, "step_sent", step, json!({ "job_id": job.id, "sent_at": now }), now).await?;
            let sent_step = step.unwrap_or(e.next_step);
            let next: Option<(i32, DelayType)> = sqlx::query_as(
                "SELECT delay_amount, delay_type FROM sequence_steps WHERE sequence_id = $1 AND step_number = $2",
            )
            .bind(e.sequence_id)
            .bind(sent_step + 1)
            .fetch_optional(&mut *conn)
            .await?;
            match next {
                None => finish(conn, &e, now).await?,
                Some((amount, kind)) => {
                    let due = add_delay(timezone_of(conn, e.account_id).await?, now, amount, kind);
                    sqlx::query(
                        "UPDATE enrollments SET next_step = $2, next_due_at = $3, current_job_id = NULL, step_queued_at = NULL,
                                status = CASE WHEN status = 'paused' THEN 'paused'::enrollment_status ELSE 'active'::enrollment_status END,
                                updated_at = $4
                         WHERE id = $1",
                    )
                    .bind(e.id)
                    .bind(sent_step + 1)
                    .bind(due)
                    .bind(now)
                    .execute(&mut *conn)
                    .await?;
                }
            }
        }
        JobOutcome::HardBounce => {
            log(conn, e.id, "step_failed", step, json!({ "job_id": job.id, "reason": "hard bounce at send time" }), now).await?;
            stop(conn, &e, "bounce_hard", json!({ "job_id": job.id }), now).await?;
        }
        JobOutcome::Failed(msg) => {
            log(conn, e.id, "step_failed", step, json!({ "job_id": job.id, "error": msg }), now).await?;
            fail(conn, &e, "send_error", json!({ "job_id": job.id, "error": msg }), now).await?;
        }
        JobOutcome::Skipped(msg) => {
            stop(conn, &e, "suppressed", json!({ "job_id": job.id, "detail": msg }), now).await?;
        }
        JobOutcome::Cancelled(msg) => {
            stop(conn, &e, "job_cancelled", json!({ "job_id": job.id, "detail": msg }), now).await?;
        }
    }
    Ok(())
}

// ---------- the Reply Check hook ----------

/// Why the Reply Check calls `on_lead_stopped`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeadStopReason {
    Reply,
    /// An auto-reply (out of office). The sequence decides: pause, or stop.
    AutoReply,
    /// An auto-reply that the Reply Check settings treat like a reply.
    AutoReplyStop,
    Unsubscribe,
    HardBounce,
    /// The mail was blocked (a `5.7.x` policy bounce).
    BounceBlock,
    Complaint,
}

impl LeadStopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reply => "reply",
            Self::AutoReply | Self::AutoReplyStop => "auto_reply",
            Self::Unsubscribe => "unsubscribe",
            Self::HardBounce => "bounce_hard",
            Self::BounceBlock => "bounce_block",
            Self::Complaint => "complaint",
        }
    }

    /// Stop every open enrollment of the address, in all sequences. Else only the one that was answered.
    fn all_sequences(self) -> bool {
        !matches!(self, Self::AutoReply | Self::BounceBlock)
    }
}

pub struct LeadStop<'a> {
    pub reason: LeadStopReason,
    /// The send job that the inbound email answers, if the matcher found one.
    pub matched_job_id: Option<Uuid>,
    /// Addresses to stop (every open enrollment to them).
    pub emails: &'a [String],
    /// Companies to stop (a domain stop of the Reply Check).
    pub domains: &'a [String],
    pub inbound_message_id: Option<Uuid>,
}

#[derive(Debug, Default, PartialEq)]
pub struct LeadStopResult {
    pub stopped: Vec<Uuid>,
    pub paused: Vec<(Uuid, DateTime<Utc>)>,
    pub cancelled_jobs: Vec<Uuid>,
}

/// The Reply Check calls this inside the transaction that stores the inbound email, so the same email
/// twice runs it once. Stops or pauses the enrollments (table in section 11 of the build map) and cancels
/// their queued jobs.
pub async fn on_lead_stopped(conn: &mut PgConnection, ev: &LeadStop<'_>, now: DateTime<Utc>) -> Result<LeadStopResult, sqlx::Error> {
    let matched: Option<Uuid> = match ev.matched_job_id {
        Some(job_id) => {
            let row: Option<(Option<Uuid>,)> = sqlx::query_as("SELECT enrollment_id FROM send_jobs WHERE id = $1").bind(job_id).fetch_optional(&mut *conn).await?;
            row.and_then(|r| r.0)
        }
        None => None,
    };
    let emails: Vec<String> = ev.emails.iter().map(|e| e.trim().to_lowercase()).filter(|e| !e.is_empty()).collect();
    let by_address = ev.reason.all_sequences();
    // ORDER BY id: two stops that meet always lock in the same order.
    let rows: Vec<Enrollment> = sqlx::query_as(&format!(
        "SELECT {ENROLLMENT_COLS} FROM enrollments
         WHERE status IN ('active', 'waiting_send', 'paused')
           AND (id = $1 OR ($2 AND (to_email = ANY($3::citext[]) OR split_part(to_email::text, '@', 2) = ANY($4))))
         ORDER BY id FOR UPDATE"
    ))
    .bind(matched)
    .bind(by_address)
    .bind(&emails)
    .bind(ev.domains)
    .fetch_all(&mut *conn)
    .await?;

    let mut result = LeadStopResult::default();
    for e in rows {
        let detail = json!({ "label": ev.reason.as_str(), "inbound_message_id": ev.inbound_message_id, "matched_job_id": ev.matched_job_id });
        if ev.reason == LeadStopReason::AutoReply {
            let (stop_on_auto_reply, pause_days): (bool, i32) =
                sqlx::query_as("SELECT stop_on_auto_reply, auto_reply_pause_days FROM sequences WHERE id = $1")
                    .bind(e.sequence_id)
                    .fetch_one(&mut *conn)
                    .await?;
            if !stop_on_auto_reply {
                if e.status == EnrollmentStatus::Paused {
                    continue; // already paused (by hand, or by an earlier auto-reply)
                }
                let until = add_delay(timezone_of(conn, e.account_id).await?, now, pause_days, DelayType::BusinessDays);
                result.cancelled_jobs.extend(pause(conn, &e, Some(until), "auto_reply", now).await?);
                result.paused.push((e.id, until));
                continue;
            }
        }
        result.cancelled_jobs.extend(stop(conn, &e, ev.reason.as_str(), detail, now).await?);
        result.stopped.push(e.id);
    }
    // The lead: contact state, last reply time, timeline. After the stops, so `replied` is the last word.
    leads_hooks::on_outcome(conn, ev, now).await;
    Ok(result)
}

/// The account was deleted: its open enrollments can never send again.
pub async fn on_account_deleted(conn: &mut PgConnection, account_id: Uuid, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    let rows: Vec<Enrollment> = sqlx::query_as(&format!(
        "SELECT {ENROLLMENT_COLS} FROM enrollments
         WHERE account_id = $1 AND status IN ('active', 'waiting_send', 'paused') ORDER BY id FOR UPDATE"
    ))
    .bind(account_id)
    .fetch_all(&mut *conn)
    .await?;
    for e in rows {
        fail(conn, &e, "account_deleted", json!({}), now).await?;
    }
    Ok(())
}
