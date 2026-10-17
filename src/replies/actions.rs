//! Apply the label of an inbound email: suppress, cancel queued jobs, mark bounces, recheck the bounce rate.
//! Runs inside the same transaction that inserts the `inbound_messages` row, so a second run of the
//! same email never repeats an action. Every action writes a `send_events` row with the inbound id in
//! `detail`, and is listed in `inbound_messages.actions`, so a manual relabel can undo it.
use super::matcher::MatchedJob;
use super::model::{BounceKind, Label};
use super::settings::{OnAutoReply, ReplySettings, StopScope};
use crate::followups::{LeadStop, LeadStopReason};
use crate::sending::health;
use crate::sending::queue::log_event;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

/// What was done. Saved as JSON in `inbound_messages.actions`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// `replied_at` was set on the matched job (first time only).
    JobReplied { job_id: Uuid, set: bool },
    JobBounced { job_id: Uuid, kind: BounceKind, set_bounced_at: bool, previous_kind: Option<BounceKind> },
    Suppression { value: String, kind: String, reason: String, created: bool, upgraded_from: Option<String> },
    CancelledJobs { job_ids: Vec<Uuid>, reason: String },
    BounceRecheck { paused: Option<String> },
    /// Follow-up enrollments that the follow-up step stopped (see `followups::on_lead_stopped`).
    EnrollmentsStopped { enrollment_ids: Vec<Uuid>, reason: String },
    /// Follow-up enrollments paused because of an auto-reply. The follow-up scheduler resumes them.
    EnrollmentsPaused { paused: Vec<PausedEnrollment> },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PausedEnrollment {
    pub enrollment_id: Uuid,
    pub until: DateTime<Utc>,
}

pub struct ActionContext<'a> {
    /// The inbox that got the email.
    pub account_id: Uuid,
    pub own_email: &'a str,
    pub message_id: Uuid,
    pub label: Label,
    pub from_email: &'a str,
    /// The address a bounce or complaint is about.
    pub bounce_recipient: Option<&'a str>,
    pub matched: Option<&'a MatchedJob>,
    pub settings: &'a ReplySettings,
    /// Free mail domains: a domain stop never applies to them (`per_domain_exempt` of the sending settings).
    pub exempt_domains: &'a [String],
    pub now: DateTime<Utc>,
}

fn domain_of(email: &str) -> &str {
    email.rsplit('@').next().unwrap_or_default()
}

fn detail(ctx: &ActionContext<'_>, extra: Value) -> Value {
    let mut d = json!({ "inbound_message_id": ctx.message_id, "label": ctx.label.as_str() });
    if let (Value::Object(base), Value::Object(more)) = (&mut d, extra) {
        base.extend(more);
    }
    d
}

/// Strength order: only a weak `replied` row may be upgraded.
async fn suppress(
    tx: &mut PgConnection,
    ctx: &ActionContext<'_>,
    value: &str,
    kind: &str,
    reason: &str,
) -> Result<Option<Action>, sqlx::Error> {
    let value = value.trim().to_lowercase();
    if value.is_empty() || value == ctx.own_email.to_lowercase() {
        return Ok(None);
    }
    let existing: Option<(String,)> =
        sqlx::query_as("SELECT reason::text FROM suppressions WHERE value = $1::citext AND kind = $2::suppression_kind FOR UPDATE")
            .bind(&value)
            .bind(kind)
            .fetch_optional(&mut *tx)
            .await?;
    let (created, upgraded_from) = match existing {
        None => {
            let done = sqlx::query(
                "INSERT INTO suppressions (value, kind, reason, source_message_id, created_at)
                 VALUES ($1::citext, $2::suppression_kind, $3::suppression_reason, $4, $5) ON CONFLICT DO NOTHING",
            )
            .bind(&value)
            .bind(kind)
            .bind(reason)
            .bind(ctx.message_id)
            .bind(ctx.now)
            .execute(&mut *tx)
            .await?;
            (done.rows_affected() == 1, None)
        }
        Some((old,)) if old == "replied" && reason != "replied" => {
            sqlx::query(
                "UPDATE suppressions SET reason = $3::suppression_reason, source_message_id = $4, created_at = $5
                 WHERE value = $1::citext AND kind = $2::suppression_kind",
            )
            .bind(&value)
            .bind(kind)
            .bind(reason)
            .bind(ctx.message_id)
            .bind(ctx.now)
            .execute(&mut *tx)
            .await?;
            (false, Some(old))
        }
        Some(_) => return Ok(None), // already blocked, as strong or stronger
    };
    if !created && upgraded_from.is_none() {
        return Ok(None);
    }
    log_event(
        tx,
        ctx.matched.map(|j| j.id),
        ctx.account_id,
        "suppressed",
        detail(ctx, json!({ "value": value, "kind": kind, "reason": reason })),
        ctx.now,
    )
    .await?;
    Ok(Some(Action::Suppression { value, kind: kind.into(), reason: reason.into(), created, upgraded_from }))
}

/// Cancel queued jobs to these addresses (and domains). `only_automated`: leave `manual` jobs alone.
/// Jobs of a follow-up enrollment are not touched here: `followups::on_lead_stopped` decides about them.
async fn cancel_queued(
    tx: &mut PgConnection,
    ctx: &ActionContext<'_>,
    emails: &[String],
    domains: &[String],
    only_automated: bool,
    reason: &str,
) -> Result<Option<Action>, sqlx::Error> {
    if emails.is_empty() && domains.is_empty() {
        return Ok(None);
    }
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "UPDATE send_jobs SET status = 'cancelled', last_error = $4, updated_at = $5
         WHERE status = 'queued' AND ($3 = false OR kind = 'automated')
           AND enrollment_id IS NULL
           AND (to_email = ANY($1::citext[]) OR recipient_domain = ANY($2))
         RETURNING id, account_id",
    )
    .bind(emails)
    .bind(domains)
    .bind(only_automated)
    .bind(format!("stopped: {reason}"))
    .bind(ctx.now)
    .fetch_all(&mut *tx)
    .await?;
    for (job_id, account_id) in &rows {
        log_event(tx, Some(*job_id), *account_id, "cancelled", detail(ctx, json!({ "reason": reason })), ctx.now).await?;
    }
    Ok((!rows.is_empty()).then(|| Action::CancelledJobs { job_ids: rows.into_iter().map(|r| r.0).collect(), reason: reason.into() }))
}

async fn mark_replied(tx: &mut PgConnection, ctx: &ActionContext<'_>) -> Result<Option<Action>, sqlx::Error> {
    let Some(job) = ctx.matched else { return Ok(None) };
    let done = sqlx::query("UPDATE send_jobs SET replied_at = $2, updated_at = $2 WHERE id = $1 AND replied_at IS NULL")
        .bind(job.id)
        .bind(ctx.now)
        .execute(tx)
        .await?;
    Ok(Some(Action::JobReplied { job_id: job.id, set: done.rows_affected() == 1 }))
}

async fn mark_bounced(tx: &mut PgConnection, ctx: &ActionContext<'_>, kind: BounceKind) -> Result<Option<Action>, sqlx::Error> {
    let Some(job) = ctx.matched else { return Ok(None) };
    let (bounced_at, previous): (Option<DateTime<Utc>>, Option<BounceKind>) =
        sqlx::query_as("SELECT bounced_at, bounce_kind FROM send_jobs WHERE id = $1 FOR UPDATE").bind(job.id).fetch_one(&mut *tx).await?;
    // A soft bounce can become a hard one. A hard one stays hard.
    let upgrade = previous.is_none() || (previous == Some(BounceKind::Soft) && kind != BounceKind::Soft);
    if bounced_at.is_none() || upgrade {
        sqlx::query(
            "UPDATE send_jobs SET bounced_at = COALESCE(bounced_at, $2),
                    bounce_kind = CASE WHEN $3 THEN $4 ELSE bounce_kind END, updated_at = $2 WHERE id = $1",
        )
        .bind(job.id)
        .bind(ctx.now)
        .bind(upgrade)
        .bind(kind)
        .execute(tx)
        .await?;
        return Ok(Some(Action::JobBounced { job_id: job.id, kind, set_bounced_at: bounced_at.is_none(), previous_kind: previous }));
    }
    Ok(None)
}

/// Tell the follow-up step. It stops or pauses the enrollments and cancels their queued jobs.
async fn tell_followups(
    tx: &mut PgConnection,
    ctx: &ActionContext<'_>,
    reason: LeadStopReason,
    emails: &[String],
    domains: &[String],
    actions: &mut Vec<Action>,
) -> Result<(), sqlx::Error> {
    let r = crate::followups::on_lead_stopped(
        tx,
        &LeadStop { reason, matched_job_id: ctx.matched.map(|j| j.id), emails, domains, inbound_message_id: Some(ctx.message_id) },
        ctx.now,
    )
    .await?;
    if !r.stopped.is_empty() {
        actions.push(Action::EnrollmentsStopped { enrollment_ids: r.stopped, reason: reason.as_str().into() });
    }
    if !r.paused.is_empty() {
        actions.push(Action::EnrollmentsPaused {
            paused: r.paused.into_iter().map(|(enrollment_id, until)| PausedEnrollment { enrollment_id, until }).collect(),
        });
    }
    if !r.cancelled_jobs.is_empty() {
        actions.push(Action::CancelledJobs { job_ids: r.cancelled_jobs, reason: reason.as_str().into() });
    }
    Ok(())
}

/// The address a bounce or a complaint is about.
fn target_address(ctx: &ActionContext<'_>) -> Option<String> {
    ctx.bounce_recipient.map(str::to_string).or_else(|| ctx.matched.map(|j| j.to_email.clone()))
}

/// Stop an address after a reply: suppress it, and cancel queued automated jobs.
/// `stop_scope = domain` also stops everyone at the company (never for free mail domains).
async fn stop_after_reply(
    tx: &mut PgConnection,
    ctx: &ActionContext<'_>,
    reason: &str,
    stop: LeadStopReason,
    actions: &mut Vec<Action>,
) -> Result<(), sqlx::Error> {
    let mut emails: Vec<String> = Vec::new();
    for e in ctx.matched.map(|j| j.to_email.clone()).into_iter().chain([ctx.from_email.to_string()]) {
        let e = e.trim().to_lowercase();
        if !e.is_empty() && e != ctx.own_email.to_lowercase() && !emails.contains(&e) {
            emails.push(e);
        }
    }
    let mut domains: Vec<String> = Vec::new();
    if ctx.settings.stop_scope == StopScope::Domain {
        for e in &emails {
            let d = domain_of(e).to_lowercase();
            if !d.is_empty() && !ctx.exempt_domains.iter().any(|x| x.eq_ignore_ascii_case(&d)) && !domains.contains(&d) {
                domains.push(d);
            }
        }
    }
    for e in &emails {
        actions.extend(suppress(tx, ctx, e, "email", "replied").await?);
    }
    for d in &domains {
        actions.extend(suppress(tx, ctx, d, "domain", "replied").await?);
    }
    actions.extend(cancel_queued(tx, ctx, &emails, &domains, true, reason).await?);
    tell_followups(tx, ctx, stop, &emails, &domains, actions).await?;
    Ok(())
}

/// Do what the label says. Returns what was done.
pub async fn apply(tx: &mut PgConnection, ctx: &ActionContext<'_>) -> Result<Vec<Action>, sqlx::Error> {
    let mut actions: Vec<Action> = Vec::new();
    let job_id = ctx.matched.map(|j| j.id);
    let event = match ctx.label {
        Label::Reply => {
            actions.extend(mark_replied(tx, ctx).await?);
            stop_after_reply(tx, ctx, "replied", LeadStopReason::Reply, &mut actions).await?;
            "replied"
        }
        Label::AutoReply => {
            if ctx.settings.on_auto_reply == OnAutoReply::Stop {
                stop_after_reply(tx, ctx, "auto-reply", LeadStopReason::AutoReplyStop, &mut actions).await?;
            } else {
                // The sequence decides: pause (default) or stop.
                tell_followups(tx, ctx, LeadStopReason::AutoReply, &[], &[], &mut actions).await?;
            }
            "auto_reply"
        }
        Label::Unsubscribe => {
            actions.extend(mark_replied(tx, ctx).await?);
            let sender = ctx.from_email.trim().to_lowercase();
            actions.extend(suppress(tx, ctx, &sender, "email", "unsubscribed").await?);
            actions.extend(cancel_queued(tx, ctx, std::slice::from_ref(&sender), &[], false, "unsubscribed").await?);
            tell_followups(tx, ctx, LeadStopReason::Unsubscribe, std::slice::from_ref(&sender), &[], &mut actions).await?;
            "unsubscribed"
        }
        Label::BounceHard => {
            actions.extend(mark_bounced(tx, ctx, BounceKind::Hard).await?);
            if let Some(addr) = target_address(ctx) {
                actions.extend(suppress(tx, ctx, &addr, "email", "hard_bounce").await?);
                actions.extend(cancel_queued(tx, ctx, std::slice::from_ref(&addr), &[], false, "hard bounce").await?);
                tell_followups(tx, ctx, LeadStopReason::HardBounce, std::slice::from_ref(&addr), &[], &mut actions).await?;
            }
            recheck(tx, ctx, &mut actions).await?;
            "bounce_hard"
        }
        Label::BounceBlock => {
            actions.extend(mark_bounced(tx, ctx, BounceKind::Block).await?);
            tell_followups(tx, ctx, LeadStopReason::BounceBlock, &[], &[], &mut actions).await?;
            recheck(tx, ctx, &mut actions).await?;
            "bounce_block"
        }
        Label::BounceSoft => {
            actions.extend(mark_bounced(tx, ctx, BounceKind::Soft).await?);
            if let Some(addr) = target_address(ctx) {
                // Enough soft bounces to the same address become a hard suppression. Never on a guess:
                // the count only has bounces whose recipient we know.
                let (soft,): (i64,) = sqlx::query_as(
                    "SELECT count(*) FROM inbound_messages
                     WHERE label = 'bounce_soft' AND bounce_recipient = $1::citext AND received_at > $2",
                )
                .bind(&addr)
                .bind(ctx.now - Duration::days(30))
                .fetch_one(&mut *tx)
                .await?;
                if soft >= ctx.settings.soft_bounce_limit as i64 {
                    actions.extend(suppress(tx, ctx, &addr, "email", "hard_bounce").await?);
                }
            }
            "bounce_soft"
        }
        Label::Complaint => {
            if let Some(addr) = target_address(ctx) {
                actions.extend(suppress(tx, ctx, &addr, "email", "complained").await?);
                actions.extend(cancel_queued(tx, ctx, std::slice::from_ref(&addr), &[], false, "complaint").await?);
                tell_followups(tx, ctx, LeadStopReason::Complaint, std::slice::from_ref(&addr), &[], &mut actions).await?;
            }
            "complaint"
        }
        Label::Delayed => "delayed",
        Label::Unknown => "inbound_unknown",
    };
    log_event(tx, job_id, ctx.account_id, event, detail(ctx, json!({ "from": ctx.from_email })), ctx.now).await?;
    Ok(actions)
}

async fn recheck(tx: &mut PgConnection, ctx: &ActionContext<'_>, actions: &mut Vec<Action>) -> Result<(), sqlx::Error> {
    // The account whose send was bounced: normally the inbox account, but a bounce can land in another inbox.
    let account = ctx.matched.map(|j| j.account_id).unwrap_or(ctx.account_id);
    let paused = health::recheck(tx, account, ctx.now).await?;
    actions.push(Action::BounceRecheck { paused });
    Ok(())
}

/// Undo the actions of one inbound email, for a manual relabel: remove the suppressions it made and
/// the marks it put on the job. Cancelled jobs are not restored: requeue them on purpose.
pub async fn undo(tx: &mut PgConnection, message_id: Uuid, actions: &Value) -> Result<(), sqlx::Error> {
    let list: Vec<Action> = serde_json::from_value(actions.clone()).unwrap_or_default();
    for a in list {
        match a {
            Action::Suppression { value, kind, created, upgraded_from, .. } => {
                if created {
                    sqlx::query(
                        "DELETE FROM suppressions WHERE value = $1::citext AND kind = $2::suppression_kind AND source_message_id = $3",
                    )
                    .bind(&value)
                    .bind(&kind)
                    .bind(message_id)
                    .execute(&mut *tx)
                    .await?;
                } else if let Some(old) = upgraded_from {
                    sqlx::query(
                        "UPDATE suppressions SET reason = $4::suppression_reason, source_message_id = NULL
                         WHERE value = $1::citext AND kind = $2::suppression_kind AND source_message_id = $3",
                    )
                    .bind(&value)
                    .bind(&kind)
                    .bind(message_id)
                    .bind(old)
                    .execute(&mut *tx)
                    .await?;
                }
            }
            Action::JobReplied { job_id, set: true } => {
                sqlx::query(
                    "UPDATE send_jobs SET replied_at = NULL WHERE id = $1 AND NOT EXISTS (
                         SELECT 1 FROM inbound_messages WHERE matched_job_id = $1 AND id <> $2 AND label IN ('reply', 'unsubscribe'))",
                )
                .bind(job_id)
                .bind(message_id)
                .execute(&mut *tx)
                .await?;
            }
            Action::JobBounced { job_id, set_bounced_at, previous_kind, .. } => {
                sqlx::query(
                    "UPDATE send_jobs SET bounce_kind = $2, bounced_at = CASE WHEN $3 THEN NULL ELSE bounced_at END WHERE id = $1",
                )
                .bind(job_id)
                .bind(previous_kind)
                .bind(set_bounced_at)
                .execute(&mut *tx)
                .await?;
            }
            _ => {}
        }
    }
    Ok(())
}
