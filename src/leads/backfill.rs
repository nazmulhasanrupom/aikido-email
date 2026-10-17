//! Old data joins Lead Management. Every enrollment with no `lead_id` gets a lead (found by address, or made
//! with source `enrollment`). A lead that was made or linked here and still says `new` gets the state and the
//! times that the old jobs and enrollments show. Safe to run twice: the second run finds nothing to do.
use super::model::{DedupeMode, LeadSource};
use super::input::LeadInput;
use super::upsert::{Outcome, UpsertCtx, upsert_batch};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

const BATCH: i64 = 1000;

#[derive(Debug, Default, Serialize, PartialEq)]
pub struct BackfillReport {
    /// Enrollments that got a `lead_id`.
    pub linked: u64,
    /// Leads made for addresses that had none.
    pub leads_created: u64,
}

pub async fn run(pool: &PgPool, now: DateTime<Utc>) -> Result<BackfillReport, sqlx::Error> {
    let mut report = BackfillReport::default();
    loop {
        let emails: Vec<(String,)> =
            sqlx::query_as("SELECT DISTINCT lower(to_email::text) FROM enrollments WHERE lead_id IS NULL LIMIT $1").bind(BATCH).fetch_all(pool).await?;
        if emails.is_empty() {
            break;
        }
        let emails: Vec<String> = emails.into_iter().map(|e| e.0).collect();
        let inputs: Vec<LeadInput> =
            emails.iter().map(|e| LeadInput { email: e.clone(), domain: super::email::domain_of(e), ..Default::default() }).collect();
        let mut tx = pool.begin().await?;
        let ctx = UpsertCtx { mode: DedupeMode::Skip, source: LeadSource::Enrollment, source_ref: Some("backfill"), ref_id: None, now };
        let made = upsert_batch(&mut tx, &inputs, &ctx).await?;
        report.leads_created += made.iter().filter(|m| m.outcome == Outcome::Created).count() as u64;
        let ids: Vec<Uuid> = made.iter().map(|m| m.id).collect();
        let linked = sqlx::query(
            "UPDATE enrollments e SET lead_id = l.id FROM leads l WHERE e.lead_id IS NULL AND l.email = lower(e.to_email::text) AND l.email = ANY($1)",
        )
        .bind(&emails)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        report.linked += linked;
        // state and times from what the old data shows
        sqlx::query(
            "UPDATE leads l SET last_contacted_at = x.last_sent, last_replied_at = x.last_reply, contact_state = x.state, updated_at = $2
               FROM (
                 SELECT s.id, s.last_sent, s.last_reply,
                        CASE
                          WHEN EXISTS (SELECT 1 FROM suppressions p WHERE p.kind = 'email' AND p.value = s.email::citext AND p.reason IN ('unsubscribed', 'complained'))
                            THEN 'unsubscribed'::lead_contact_state
                          WHEN s.hard_bounced OR EXISTS (SELECT 1 FROM suppressions p WHERE p.kind = 'email' AND p.value = s.email::citext AND p.reason = 'hard_bounce')
                            THEN 'bounced'::lead_contact_state
                          WHEN s.last_reply IS NOT NULL THEN 'replied'::lead_contact_state
                          WHEN s.open_enrollment THEN (CASE WHEN s.last_sent IS NOT NULL THEN 'contacted' ELSE 'in_sequence' END)::lead_contact_state
                          WHEN s.finished_enrollment THEN 'completed'::lead_contact_state
                          WHEN s.last_sent IS NOT NULL THEN 'contacted'::lead_contact_state
                          ELSE 'new'::lead_contact_state
                        END AS state
                   FROM (
                     SELECT l2.id, l2.email,
                            (SELECT max(j.sent_at) FROM send_jobs j WHERE j.to_email = l2.email::citext AND j.status = 'sent') AS last_sent,
                            (SELECT max(j.replied_at) FROM send_jobs j WHERE j.to_email = l2.email::citext) AS last_reply,
                            EXISTS (SELECT 1 FROM send_jobs j WHERE j.to_email = l2.email::citext AND j.bounce_kind = 'hard') AS hard_bounced,
                            EXISTS (SELECT 1 FROM enrollments e WHERE e.to_email = l2.email::citext AND e.status IN ('active', 'waiting_send', 'paused')) AS open_enrollment,
                            EXISTS (SELECT 1 FROM enrollments e WHERE e.to_email = l2.email::citext AND e.status = 'finished') AS finished_enrollment
                       FROM leads l2 WHERE l2.id = ANY($1) AND l2.contact_state = 'new'
                   ) s
               ) x
              WHERE l.id = x.id",
        )
        .bind(&ids)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        if linked == 0 {
            // nothing could be linked: stop, rather than loop for ever
            break;
        }
    }
    Ok(report)
}
