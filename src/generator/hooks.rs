//! What the follow-up engine tells the generator, and what the generator tells the follow-up scheduler.
//!
//! Engine to generator (`on_enrollment_closed`, `on_resume`): each runs in a savepoint and never blocks the caller,
//! like the lead hooks (`leads::hooks`).
//!
//! Generator to scheduler (`ai_text`): at due time, which text goes into the AI slots of a step.
use super::repo;
use super::settings::OnFailure;
use super::slots::{self, AiSlot};
use crate::followups::model::{Enrollment, SequenceStep};
use crate::followups::template::{StepTemplates, Vars, ai_var};
use crate::leads::hooks::guarded;
use chrono::{DateTime, Duration, Utc};
use sqlx::PgConnection;
use uuid::Uuid;

/// The enrollment ended: the text for its steps from `from_step` on is never used.
pub async fn on_enrollment_closed(conn: &mut PgConnection, enrollment_id: Uuid, from_step: i32, now: DateTime<Utc>) {
    guarded(conn, "generator_cancel", |c| {
        Box::pin(async move {
            sqlx::query(
                "UPDATE generations SET status = 'cancelled', claim_id = NULL, updated_at = $3
                 WHERE enrollment_id = $1 AND step_number >= $2
                   AND status IN ('pending', 'generating', 'ready', 'pending_review', 'approved')",
            )
            .bind(enrollment_id)
            .bind(from_step)
            .bind(now)
            .execute(c)
            .await?;
            Ok(())
        })
    })
    .await;
}

/// The enrollment came back from a pause. Text that is older than twice the look-ahead is made again (fresh data).
pub async fn on_resume(conn: &mut PgConnection, enrollment_id: Uuid, now: DateTime<Utc>) {
    guarded(conn, "generator_stale", |c| {
        Box::pin(async move {
            let settings = repo::load_settings(c).await?;
            let limit = now - Duration::hours(settings.lookahead_hours as i64 * 2);
            sqlx::query(
                "UPDATE generations SET status = 'stale', claim_id = NULL, updated_at = $3
                 WHERE enrollment_id = $1 AND updated_at < $2
                   AND status IN ('pending', 'ready', 'pending_review', 'approved')",
            )
            .bind(enrollment_id)
            .bind(limit)
            .bind(now)
            .execute(c)
            .await?;
            Ok(())
        })
    })
    .await;
}

/// What the scheduler does with a step that has AI slots.
#[derive(Debug)]
pub enum AiOutcome {
    /// Render now. `vars` holds `ai:key` values. `fallback_keys` are the slots filled from a fallback text.
    Texts { vars: Vars, fallback_keys: Vec<String> },
    /// Not now. The enrollment keeps `wait_reason`.
    Hold { reason: &'static str },
    /// Nothing to put in a slot, and no fallback: the enrollment fails.
    Fail { message: String },
}

fn text_of(slots_json: &serde_json::Value, key: &str) -> Option<String> {
    slots_json.get(key).and_then(|v| v.as_str()).map(str::trim).filter(|v| !v.is_empty()).map(String::from)
}

/// Decide the AI text of a due step for one enrollment (read the `ready` or `approved` row of the current step hash).
pub async fn ai_text(conn: &mut PgConnection, e: &Enrollment, step: &SequenceStep, templates: &StepTemplates, now: DateTime<Utc>) -> Result<AiOutcome, sqlx::Error> {
    let slot_defs: Vec<AiSlot> = slots::from_json(&step.ai_slots);
    let hash = super::hash::step_hash(&step.subject, step.body_text.as_deref(), step.body_html.as_deref(), &slot_defs);
    let row: Option<(String, serde_json::Value)> =
        sqlx::query_as("SELECT status, slots FROM generations WHERE enrollment_id = $1 AND step_number = $2 AND step_hash = $3")
            .bind(e.id)
            .bind(step.step_number)
            .bind(&hash)
            .fetch_optional(&mut *conn)
            .await?;
    let keys = templates.ai_keys();
    let mut vars = Vars::new();

    if let Some((status, slots_json)) = &row {
        if status == "ready" || status == "approved" {
            for k in &keys {
                if let Some(t) = text_of(slots_json, k) {
                    vars.insert(ai_var(k), t);
                }
            }
            return Ok(complete(vars, &keys, &slot_defs, templates, step.step_number));
        }
        // Review mode: a person acts on this text. A step never goes out on the fallback behind their back.
        if status == "pending_review" || status == "rejected" {
            return Ok(AiOutcome::Hold { reason: "generation_review" });
        }
    }

    // No usable text yet. How long has this step been held?
    let settings = repo::load_settings(conn).await?;
    let (since,): (Option<DateTime<Utc>>,) = sqlx::query_as("SELECT wait_since FROM enrollments WHERE id = $1").bind(e.id).fetch_one(&mut *conn).await?;
    let elapsed = now - since.unwrap_or(now);
    let failed = row.as_ref().is_some_and(|r| r.0 == "failed");
    let use_fallback = match settings.on_failure {
        OnFailure::Wait => elapsed >= Duration::minutes(settings.max_wait_mins as i64),
        OnFailure::Fallback => failed || elapsed >= Duration::minutes(settings.due_grace_mins as i64),
    };
    if !use_fallback {
        return Ok(AiOutcome::Hold { reason: "generation_pending" });
    }
    Ok(complete(vars, &keys, &slot_defs, templates, step.step_number))
}

/// Fill every slot that has no text from its fallback. A slot with neither fails the step.
fn complete(mut vars: Vars, keys: &[String], defs: &[AiSlot], templates: &StepTemplates, step_number: i32) -> AiOutcome {
    let inline = templates.ai_keys_with_inline_fallback();
    let mut fallback_keys = Vec::new();
    let mut lacking = Vec::new();
    for k in keys {
        if vars.contains_key(&ai_var(k)) {
            continue;
        }
        match defs.iter().find(|d| &d.key == k).and_then(|d| d.fallback.clone()) {
            Some(f) => {
                vars.insert(ai_var(k), f);
                fallback_keys.push(k.clone());
            }
            None if inline.contains(k) => fallback_keys.push(k.clone()),
            None => lacking.push(k.clone()),
        }
    }
    if lacking.is_empty() {
        AiOutcome::Texts { vars, fallback_keys }
    } else {
        AiOutcome::Fail { message: format!("step {step_number} has AI slot(s) with no text and no fallback: {}", lacking.join(", ")) }
    }
}
