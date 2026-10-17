//! What the other engines tell Lead Management. Each hook runs inside the caller's transaction, in a
//! savepoint. A failed lead update is logged (with ids only) and rolled back alone: it never blocks a send
//! or a stop.
use super::events;
use super::model::ContactState;
use crate::followups::model::Enrollment;
use crate::followups::{LeadStop, LeadStopReason};
use crate::sending::queue::Job;
use chrono::{DateTime, Utc};
use futures::future::BoxFuture;
use serde_json::json;
use sqlx::{Connection, PgConnection};
use uuid::Uuid;

/// Run `f` in a savepoint. An error is logged and undone; the transaction of the caller goes on.
pub(crate) async fn guarded<T, F>(conn: &mut PgConnection, what: &'static str, f: F) -> Option<T>
where
    F: for<'c> FnOnce(&'c mut PgConnection) -> BoxFuture<'c, Result<T, sqlx::Error>>,
{
    let mut sp = match conn.begin().await {
        Ok(sp) => sp,
        Err(e) => {
            tracing::error!(hook = what, error = %e, "lead hook: no savepoint");
            return None;
        }
    };
    match f(&mut sp).await {
        Ok(v) => match sp.commit().await {
            Ok(()) => Some(v),
            Err(e) => {
                tracing::error!(hook = what, error = %e, "lead hook: commit failed");
                None
            }
        },
        Err(e) => {
            tracing::error!(hook = what, error = %e, "lead hook failed; the lead was not updated");
            // dropping the savepoint rolls it back
            None
        }
    }
}

/// The lead of an address: by id if it exists, else by email.
pub async fn resolve(conn: &mut PgConnection, lead_id: Option<Uuid>, email: &str) -> Result<Option<Uuid>, sqlx::Error> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM (SELECT id, 0 AS rank FROM leads WHERE id = $1 UNION ALL SELECT id, 1 FROM leads WHERE email = $2) x ORDER BY rank LIMIT 1",
    )
    .bind(lead_id)
    .bind(email.trim().to_lowercase())
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|r| r.0))
}

/// An enrollment was made for this lead: it is in a sequence now. (Not for `bounced` and `unsubscribed`:
/// those never change again.)
pub async fn on_enrolled(conn: &mut PgConnection, lead_id: Uuid, enrollment_id: Uuid, now: DateTime<Utc>) {
    guarded(conn, "on_enrolled", |c| {
        Box::pin(async move {
            sqlx::query(
                "UPDATE leads SET contact_state = 'in_sequence', updated_at = $2
                 WHERE id = $1 AND contact_state IN ('new', 'contacted', 'replied', 'completed', 'in_sequence')",
            )
            .bind(lead_id)
            .bind(now)
            .execute(&mut *c)
            .await?;
            events::log(c, lead_id, "enrolled", Some(enrollment_id), json!({}), now).await
        })
    })
    .await;
}

/// An email was sent (a sequence step or a manual send). Matches the lead by `lead_id` or by address.
pub async fn on_sent(conn: &mut PgConnection, job: &Job, now: DateTime<Utc>) {
    let (job_id, lead_id, email, enrollment_id, step) = (job.id, job.lead_id, job.to_email.clone(), job.enrollment_id, job.step_number);
    guarded(conn, "on_sent", |c| {
        Box::pin(async move {
            let Some(id) = resolve(c, lead_id, &email).await? else { return Ok(()) };
            sqlx::query(
                "UPDATE leads SET last_contacted_at = $2, updated_at = $2,
                        contact_state = CASE WHEN contact_state IN ('new', 'in_sequence') THEN 'contacted'::lead_contact_state ELSE contact_state END
                 WHERE id = $1",
            )
            .bind(id)
            .bind(now)
            .execute(&mut *c)
            .await?;
            events::log(c, id, "step_sent", Some(job_id), json!({ "enrollment_id": enrollment_id, "step_number": step }), now).await
        })
    })
    .await;
}

pub enum Ended<'a> {
    /// The last step went out and nobody replied.
    Finished,
    Stopped(&'a str),
    Failed(&'a str),
}

/// Reasons that `on_outcome` already puts on the timeline with a better event.
const OUTCOME_REASONS: [&str; 6] = ["reply", "auto_reply", "unsubscribe", "bounce_hard", "bounce_block", "complaint"];

/// An enrollment ended. Called after the enrollment row was updated.
pub async fn on_enrollment_ended(conn: &mut PgConnection, e: &Enrollment, how: Ended<'_>, now: DateTime<Utc>) {
    let (enrollment_id, lead_id, email) = (e.id, e.lead_id, e.to_email.clone());
    let (reason, failed, finished) = match how {
        Ended::Finished => ("finished".to_string(), false, true),
        Ended::Stopped(r) => (r.to_string(), false, false),
        Ended::Failed(r) => (r.to_string(), true, false),
    };
    guarded(conn, "on_enrollment_ended", |c| {
        Box::pin(async move {
            let Some(id) = resolve(c, lead_id, &email).await? else { return Ok(()) };
            if finished {
                sqlx::query(
                    "UPDATE leads SET contact_state = 'completed', updated_at = $2 WHERE id = $1 AND contact_state IN ('in_sequence', 'contacted')",
                )
                .bind(id)
                .bind(now)
                .execute(&mut *c)
                .await?;
            } else {
                // Never stay `in_sequence` when no sequence runs: back to what the lead really is.
                sqlx::query(
                    "UPDATE leads SET updated_at = $2,
                            contact_state = CASE WHEN last_contacted_at IS NOT NULL THEN 'contacted'::lead_contact_state ELSE 'new'::lead_contact_state END
                     WHERE id = $1 AND contact_state = 'in_sequence'
                       AND NOT EXISTS (SELECT 1 FROM enrollments o WHERE o.to_email = $3::citext AND o.id <> $4 AND o.status IN ('active', 'waiting_send', 'paused'))",
                )
                .bind(id)
                .bind(now)
                .bind(&email)
                .bind(enrollment_id)
                .execute(&mut *c)
                .await?;
            }
            if !OUTCOME_REASONS.contains(&reason.as_str()) {
                events::log(c, id, "stopped", Some(enrollment_id), json!({ "reason": reason, "failed": failed }), now).await?;
            }
            Ok(())
        })
    })
    .await;
}

/// The Reply Check classified an inbound email. Sets `contact_state`, `last_replied_at` and the timeline.
/// `interest` is never touched: a reply must not overwrite "meeting booked".
pub async fn on_outcome(conn: &mut PgConnection, ev: &LeadStop<'_>, now: DateTime<Utc>) {
    let emails: Vec<String> = ev.emails.iter().map(|e| e.trim().to_lowercase()).filter(|e| !e.is_empty()).collect();
    let (reason, job_id, message_id) = (ev.reason, ev.matched_job_id, ev.inbound_message_id);
    let replied = guarded(conn, "on_outcome", |c| {
        Box::pin(async move {
            // the leads this is about: the addresses named, else the lead of the matched send job
            let mut ids: Vec<Uuid> = Vec::new();
            if !emails.is_empty() {
                let rows: Vec<(Uuid,)> = sqlx::query_as("SELECT id FROM leads WHERE email = ANY($1)").bind(&emails).fetch_all(&mut *c).await?;
                ids.extend(rows.into_iter().map(|r| r.0));
            }
            if ids.is_empty() {
                if let Some(job_id) = job_id {
                    let job: Option<(Option<Uuid>, String)> = sqlx::query_as("SELECT lead_id, to_email::text FROM send_jobs WHERE id = $1").bind(job_id).fetch_optional(&mut *c).await?;
                    if let Some((lead_id, to)) = job {
                        ids.extend(resolve(c, lead_id, &to).await?);
                    }
                }
            }
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            let detail = json!({ "reason": reason.as_str(), "inbound_message_id": message_id });
            let (kind, new_state): (&str, Option<ContactState>) = match reason {
                LeadStopReason::Reply => ("replied", Some(ContactState::Replied)),
                LeadStopReason::AutoReply | LeadStopReason::AutoReplyStop => ("auto_replied", None),
                LeadStopReason::Unsubscribe => ("unsubscribed", Some(ContactState::Unsubscribed)),
                LeadStopReason::HardBounce => ("bounced", Some(ContactState::Bounced)),
                LeadStopReason::BounceBlock => ("bounced", None),
                LeadStopReason::Complaint => ("complained", Some(ContactState::Unsubscribed)),
            };
            if let Some(state) = new_state {
                // `bounced` and `unsubscribed` never change again
                sqlx::query(
                    "UPDATE leads SET contact_state = $2, updated_at = $3,
                            last_replied_at = CASE WHEN $4 THEN $3 ELSE last_replied_at END
                     WHERE id = ANY($1) AND contact_state NOT IN ('bounced', 'unsubscribed')",
                )
                .bind(&ids)
                .bind(state)
                .bind(now)
                .bind(reason == LeadStopReason::Reply)
                .execute(&mut *c)
                .await?;
                if reason == LeadStopReason::Reply {
                    // a reply after a bounce or an unsubscribe still counts as a reply time
                    sqlx::query("UPDATE leads SET last_replied_at = $2 WHERE id = ANY($1) AND last_replied_at IS DISTINCT FROM $2").bind(&ids).bind(now).execute(&mut *c).await?;
                }
            }
            let detail = if reason == LeadStopReason::BounceBlock { json!({ "reason": "bounce_block", "block": true, "inbound_message_id": message_id }) } else { detail };
            events::log_many(c, &ids, kind, job_id, detail, now).await?;
            Ok(ids)
        })
    })
    .await;
    // Company stop: a reply stops the other open enrollments of the campaign at the same company.
    // Apart from the update above, so a failure here keeps the lead state.
    if reason == LeadStopReason::Reply {
        if let Some(ids) = replied.filter(|ids| !ids.is_empty()) {
            guarded(conn, "company_stop", |c| Box::pin(async move { crate::campaigns::company::on_reply(c, &ids, now).await })).await;
        }
    }
}
