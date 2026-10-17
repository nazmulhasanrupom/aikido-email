//! The follow-up scheduler. It waits until a step is due, checks that the enrollment is still open,
//! and only then makes one `send_jobs` row. A reply between two steps needs no job to cancel: the job does
//! not exist yet. The sending engine then does its normal work: window, gap, cap, safety gate, cooldown.
//!
//! Each enrollment is handled in its own transaction with a row lock (`FOR UPDATE SKIP LOCKED`). Two server
//! copies skip each other's rows. A crash rolls the transaction back, so no lease is needed.
use super::lifecycle::{self, fail, stop};
use super::model::*;
use super::repo;
use super::template::{StepTemplates, build_vars, sender_name_of};
use crate::generator::hooks::AiOutcome;
use crate::sending::queue::{self, EnrollmentLink, NewJob, log_event};
use crate::state::AppState;
use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use sqlx::PgPool;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Most due enrollments one tick handles.
const BATCH: i64 = 200;

pub struct FollowupEngine {
    pub state: AppState,
    pub cancel: CancellationToken,
}

impl FollowupEngine {
    pub fn new(state: AppState) -> Self {
        Self { state, cancel: CancellationToken::new() }
    }
    pub fn now(&self) -> DateTime<Utc> {
        self.state.now()
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct TickReport {
    /// The follow-up engine is paused.
    pub paused: bool,
    /// Enrollments that came back from an auto-reply pause.
    pub resumed: usize,
    /// Enrollments that waited too long in the send queue and failed.
    pub stalled: usize,
    /// Steps that became a send job.
    pub queued: usize,
    pub finished: usize,
    pub stopped: usize,
    pub failed: usize,
    /// Due steps held back because their AI text is not ready (or waits for approval).
    pub waiting: usize,
}

/// What happened to one due enrollment.
#[derive(Debug, PartialEq)]
pub enum DueOutcome {
    /// Another copy has it, it moved on, or its sequence is not active: nothing done.
    Skipped,
    Queued(Uuid),
    Finished,
    Stopped(String),
    Failed(String),
    /// The step is due but its AI text is not ready: held until the next tick. The reason is `generation_pending` or `generation_review`.
    Waiting(&'static str),
}

/// One tick: resume, stall check, then the due steps.
pub async fn tick(engine: &FollowupEngine) -> anyhow::Result<TickReport> {
    let pool = &engine.state.pool;
    let now = engine.now();
    repo::touch_tick(pool, now).await?;
    if repo::engine_paused(pool).await? {
        return Ok(TickReport { paused: true, ..Default::default() });
    }
    let mut conn = pool.acquire().await?;
    let settings = repo::load_settings(&mut conn).await?;
    drop(conn);
    let mut report = TickReport::default();

    report.resumed = resume_due_pauses(pool, now).await?;
    report.stalled = fail_stalled(pool, now, settings.max_wait_days).await?;

    let due: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT e.id FROM enrollments e JOIN sequences s ON s.id = e.sequence_id
         WHERE e.status = 'active' AND e.next_due_at <= $1 AND s.status = 'active'
         ORDER BY e.next_due_at, e.id LIMIT $2",
    )
    .bind(now)
    .bind(BATCH)
    .fetch_all(pool)
    .await?;
    for (id,) in due {
        if engine.cancel.is_cancelled() {
            break;
        }
        match process_due(pool, id, now).await {
            Ok(DueOutcome::Queued(_)) => report.queued += 1,
            Ok(DueOutcome::Finished) => report.finished += 1,
            Ok(DueOutcome::Stopped(_)) => report.stopped += 1,
            Ok(DueOutcome::Failed(_)) => report.failed += 1,
            Ok(DueOutcome::Waiting(_)) => report.waiting += 1,
            Ok(DueOutcome::Skipped) => {}
            // One bad row must not stop the others.
            Err(e) => tracing::error!(enrollment_id = %id, error = %e, "follow-up step failed to start"),
        }
    }
    Ok(report)
}

/// Runs until the engine is cancelled. The tick speed is the `tick_secs` setting, read on every round.
pub async fn run(engine: Arc<FollowupEngine>) {
    tracing::info!("follow-up scheduler started");
    loop {
        if let Err(e) = tick(&engine).await {
            tracing::error!(error = %format!("{e:#}"), "follow-up tick failed");
        }
        let secs = match engine.state.pool.acquire().await {
            Ok(mut c) => repo::load_settings(&mut c).await.map(|s| s.tick_secs).unwrap_or(30),
            Err(_) => 30,
        };
        tokio::select! {
            _ = engine.cancel.cancelled() => break,
            _ = tokio::time::sleep(std::time::Duration::from_secs(secs.max(1) as u64)) => {}
        }
    }
    tracing::info!("follow-up scheduler stopped");
}

/// A pause with an end time (an auto-reply) ends when the time has passed.
async fn resume_due_pauses(pool: &PgPool, now: DateTime<Utc>) -> Result<usize, sqlx::Error> {
    let ids: Vec<(Uuid,)> =
        sqlx::query_as("SELECT id FROM enrollments WHERE status = 'paused' AND pause_until <= $1 ORDER BY pause_until LIMIT $2")
            .bind(now)
            .bind(BATCH)
            .fetch_all(pool)
            .await?;
    let mut n = 0;
    for (id,) in ids {
        let mut tx = pool.begin().await?;
        // Re-check under the lock: a person may have changed it meanwhile.
        let Some(e) = lifecycle::lock(&mut tx, id).await? else { continue };
        if e.status != EnrollmentStatus::Paused || !e.pause_until.is_some_and(|t| t <= now) {
            continue;
        }
        lifecycle::resume(&mut tx, &e, "pause_ended", now).await?;
        tx.commit().await?;
        n += 1;
    }
    Ok(n)
}

/// A follow-up that waits in the send queue longer than `max_wait_days` is cancelled and the enrollment fails.
/// A follow-up that goes out weeks late is worse than none. Step 1 is not touched: how fast it goes is
/// the choice of whoever enrolled the batch.
async fn fail_stalled(pool: &PgPool, now: DateTime<Utc>, max_wait_days: u32) -> Result<usize, sqlx::Error> {
    let limit = now - Duration::days(max_wait_days as i64);
    let ids: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM enrollments WHERE status = 'waiting_send' AND next_step > 1 AND step_queued_at < $1 ORDER BY step_queued_at LIMIT $2",
    )
    .bind(limit)
    .bind(BATCH)
    .fetch_all(pool)
    .await?;
    let mut n = 0;
    for (id,) in ids {
        let mut tx = pool.begin().await?;
        let Some(e) = lifecycle::lock(&mut tx, id).await? else { continue };
        if e.status != EnrollmentStatus::WaitingSend || !e.step_queued_at.is_some_and(|t| t < limit) {
            continue;
        }
        // A job that is being sent right now is left alone: its result decides.
        if let Some(job_id) = e.current_job_id {
            let (status,): (String,) = sqlx::query_as("SELECT status::text FROM send_jobs WHERE id = $1").bind(job_id).fetch_one(&mut *tx).await?;
            if status == "sending" {
                continue;
            }
        }
        fail(&mut tx, &e, "stalled", json!({ "waited_days": max_wait_days }), now).await?;
        tx.commit().await?;
        n += 1;
    }
    Ok(n)
}

/// Make the send job of the next step of one due enrollment.
pub async fn process_due(pool: &PgPool, id: Uuid, now: DateTime<Utc>) -> Result<DueOutcome, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // 1. Take the row. Another copy that has it, or a row that moved on: nothing to do.
    let e: Option<Enrollment> = sqlx::query_as(&format!(
        "SELECT {ENROLLMENT_COLS} FROM enrollments
         WHERE id = $1 AND status = 'active' AND next_due_at <= $2 FOR UPDATE SKIP LOCKED"
    ))
    .bind(id)
    .bind(now)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(e) = e else { return Ok(DueOutcome::Skipped) };
    let Some(seq) = repo::get_sequence(&mut tx, e.sequence_id).await?.filter(|s| s.status == SequenceStatus::Active) else {
        return Ok(DueOutcome::Skipped);
    };

    // 2. The account must still exist.
    let Some((account_email, display_name)) = repo::account_identity(&mut tx, e.account_id).await? else {
        fail(&mut tx, &e, "account_deleted", json!({}), now).await?;
        tx.commit().await?;
        return Ok(DueOutcome::Failed("account_deleted".into()));
    };

    // 3. A suppressed address never gets a step.
    if let Some(reason) = repo::suppression_of(&mut tx, &e.to_email).await? {
        stop(&mut tx, &e, "suppressed", json!({ "suppression": reason }), now).await?;
        tx.commit().await?;
        return Ok(DueOutcome::Stopped("suppressed".into()));
    }

    // 4. A second guard after the Reply Check: a reply or a bounce is already linked to a job of this enrollment.
    let marked: Option<(Option<DateTime<Utc>>, Option<String>)> = sqlx::query_as(
        "SELECT min(replied_at), min(bounce_kind::text) FILTER (WHERE bounce_kind IN ('hard', 'block'))
         FROM send_jobs WHERE enrollment_id = $1",
    )
    .bind(e.id)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some((replied_at, bounce)) = marked {
        let reason = if replied_at.is_some() { Some("reply") } else if bounce.is_some() { Some("bounce_hard") } else { None };
        if let Some(reason) = reason {
            stop(&mut tx, &e, reason, json!({ "found_by": "scheduler_guard" }), now).await?;
            tx.commit().await?;
            return Ok(DueOutcome::Stopped(reason.into()));
        }
    }

    // 5. The step must still exist (a sequence edit may have removed it).
    let step: Option<SequenceStep> = sqlx::query_as(&format!(
        "SELECT {STEP_COLS} FROM sequence_steps WHERE sequence_id = $1 AND step_number = $2"
    ))
    .bind(e.sequence_id)
    .bind(e.next_step)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(step) = step else {
        sqlx::query("UPDATE enrollments SET status = 'finished', next_due_at = NULL, updated_at = $2 WHERE id = $1")
            .bind(e.id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        lifecycle::log(&mut tx, e.id, "finished", Some(e.next_step), json!({ "reason": "the step does not exist any more" }), now).await?;
        crate::leads::hooks::on_enrollment_ended(&mut tx, &e, crate::leads::hooks::Ended::Finished, now).await;
        tx.commit().await?;
        return Ok(DueOutcome::Finished);
    };

    // 6. Render with the values of this lead. The text of the step is read now, so an edit counts.
    // Decision A (live variables): the current fields of the lead win over the snapshot saved at enroll time.
    let lead: Option<crate::leads::model::Lead> = match e.lead_id {
        Some(id) => crate::leads::repo::get(&mut tx, id).await?,
        None => None,
    };
    let defs = crate::leads::fields::FieldDefs::load(&mut tx).await?;
    let live = crate::leads::vars::render_variables(&e.variables, lead.as_ref(), &defs);
    let vars = match build_vars(&live, &e.to_email, &sender_name_of(&display_name, &account_email), &account_email) {
        Ok(v) => v,
        Err(msg) => return fail_render(tx, &e, "invalid_variables", &msg, now).await,
    };
    let templates = match StepTemplates::parse(&step.subject, step.body_text.as_deref(), step.body_html.as_deref()) {
        Ok(t) => t,
        Err(err) => return fail_render(tx, &e, "invalid_template", &err.message, now).await,
    };
    // The AI slots: the text the generator wrote, or the fallback, or hold the step.
    let mut vars = vars;
    if !templates.ai_keys().is_empty() {
        match crate::generator::hooks::ai_text(&mut tx, &e, &step, &templates, now).await? {
            AiOutcome::Texts { vars: ai, fallback_keys } => {
                vars.extend(ai);
                if !fallback_keys.is_empty() {
                    lifecycle::log(&mut tx, e.id, "generation_fallback", Some(step.step_number), json!({ "slots": fallback_keys }), now).await?;
                }
            }
            AiOutcome::Hold { reason } => {
                let first = e.wait_reason.as_deref() != Some(reason);
                sqlx::query("UPDATE enrollments SET wait_reason = $2, wait_since = COALESCE(wait_since, $3) WHERE id = $1").bind(e.id).bind(reason).bind(now).execute(&mut *tx).await?;
                if first {
                    lifecycle::log(&mut tx, e.id, "step_waiting", Some(step.step_number), json!({ "reason": reason }), now).await?;
                }
                tx.commit().await?;
                return Ok(DueOutcome::Waiting(reason));
            }
            AiOutcome::Fail { message } => return fail_render(tx, &e, "generation_failed", &message, now).await,
        }
    }
    let rendered = match templates.render(&vars) {
        Ok(r) => r,
        Err(missing) => {
            return fail_render(tx, &e, "missing_variable", &format!("step {} needs: {}", step.step_number, missing.join(", ")), now).await;
        }
    };

    // 7. Threading: a follow-up answers the earlier emails of this enrollment.
    let mut subject = rendered.subject.trim().to_string();
    let (mut in_reply_to, mut references, mut thread_id) = (None, None, None);
    if step.step_number > 1 {
        let earlier: Vec<(i32, String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT step_number, subject, message_id_header, provider_thread_id FROM send_jobs
             WHERE enrollment_id = $1 AND step_number < $2 AND status = 'sent' ORDER BY step_number",
        )
        .bind(e.id)
        .bind(step.step_number)
        .fetch_all(&mut *tx)
        .await?;
        let first_subject = earlier.first().map(|r| r.1.clone());
        if subject.is_empty() {
            let base = first_subject.clone().unwrap_or_default();
            subject = if !seq.thread_followups || base.to_ascii_lowercase().starts_with("re:") { base } else { format!("Re: {base}") };
        }
        if seq.thread_followups {
            let ids: Vec<&str> = earlier.iter().filter_map(|r| r.2.as_deref()).collect();
            match ids.last() {
                Some(last) => {
                    in_reply_to = Some(last.to_string());
                    references = Some(ids.join(" "));
                    thread_id = earlier.iter().rev().find_map(|r| r.3.clone());
                }
                None => {
                    // Send without thread headers, and say so. Do not block the step.
                    lifecycle::log(&mut tx, e.id, "warning", Some(step.step_number), json!({ "warning": "thread_headers_missing" }), now).await?;
                }
            }
        }
    }

    // 8. The job. The key is the same for the same step, so a step is never made twice. A step that is made again
    // (it was paused while queued, and the job was called back) gets a counter, because its old job still holds the key.
    let (made_before,): (i64,) = sqlx::query_as("SELECT count(*) FROM send_jobs WHERE enrollment_id = $1 AND step_number = $2")
        .bind(e.id)
        .bind(step.step_number)
        .fetch_one(&mut *tx)
        .await?;
    let idempotency_key = if made_before == 0 { format!("fu:{}:{}", e.id, step.step_number) } else { format!("fu:{}:{}:{}", e.id, step.step_number, made_before) };
    let new_job = NewJob {
        account_id: e.account_id,
        to_email: e.to_email.clone(),
        to_name: None,
        subject,
        body_text: rendered.body_text,
        body_html: rendered.body_html,
        headers: None,
        in_reply_to,
        references,
        provider_thread_id: thread_id,
        send_after: None,
        priority: None,
        idempotency_key: Some(idempotency_key),
        lead_id: e.lead_id,
        campaign_id: seq.campaign_id,
        metadata: Some(json!({ "sequence_id": e.sequence_id, "enrollment_id": e.id, "step_number": step.step_number })),
        kind: Some(crate::replies::model::JobKind::Automated),
    };
    let (to_email, domain) = match queue::check_new_job(&new_job) {
        Ok(p) => p,
        Err(err) => return fail_render(tx, &e, &err.code, &err.message, now).await,
    };
    let link = EnrollmentLink { enrollment_id: e.id, step_number: step.step_number };
    let job = match queue::insert_job(&mut tx, &new_job, &to_email, &domain, Some(link)).await? {
        Some(job) => {
            log_event(&mut tx, Some(job.id), job.account_id, "queued", json!({ "enrollment_id": e.id, "step_number": step.step_number }), now).await?;
            job
        }
        // The job of this step exists already (an earlier run was cut short). Link it, do not make a second.
        None => sqlx::query_as(&format!("SELECT {} FROM send_jobs WHERE idempotency_key = $1", queue::JOB_COLS))
            .bind(new_job.idempotency_key.as_deref())
            .fetch_one(&mut *tx)
            .await?,
    };
    sqlx::query(
        "UPDATE enrollments SET status = 'waiting_send', current_job_id = $2, step_queued_at = $3, next_due_at = NULL, wait_reason = NULL, wait_since = NULL, updated_at = $3 WHERE id = $1",
    )
    .bind(e.id)
    .bind(job.id)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    lifecycle::log(&mut tx, e.id, "step_queued", Some(step.step_number), json!({ "job_id": job.id }), now).await?;
    tx.commit().await?;
    Ok(DueOutcome::Queued(job.id))
}

async fn fail_render(
    mut tx: sqlx::Transaction<'_, sqlx::Postgres>,
    e: &Enrollment,
    code: &str,
    message: &str,
    now: DateTime<Utc>,
) -> Result<DueOutcome, sqlx::Error> {
    fail(&mut tx, e, code, json!({ "message": message }), now).await?;
    tx.commit().await?;
    Ok(DueOutcome::Failed(code.into()))
}
