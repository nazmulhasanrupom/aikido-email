//! `/v1/followups/*`
use super::enroll::{self, EnrollError, EnrollItem, EnrollStatus, NewEnrollment};
use super::lifecycle;
use super::model::*;
use super::repo;
use super::settings::{FollowupSettings, SettingsError};
use super::template::{BUILTINS, StepTemplates, ai_var, build_vars, sender_name_of};
use crate::generator::slots::{self, AiSlot};
use crate::auth::api_keys::Scope;
use crate::auth::middleware::require_scope;
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::sending::queue::{self, Job};
use crate::state::AppState;
use axum::extract::State;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Routes that need a key with scope `followups`.
pub fn router(state: AppState) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(create_sequence, list_sequences))
        .routes(routes!(get_sequence, patch_sequence))
        .routes(routes!(put_steps))
        .routes(routes!(preview))
        .routes(routes!(create_enrollments))
        .routes(routes!(list_enrollments))
        .routes(routes!(get_enrollment))
        .routes(routes!(pause_enrollment))
        .routes(routes!(resume_enrollment))
        .routes(routes!(stop_enrollment))
        .routes(routes!(get_settings, patch_settings))
        .routes(routes!(pause_engine))
        .routes(routes!(resume_engine))
        .routes(routes!(engine_status))
        .route_layer(axum::middleware::from_fn_with_state((state, Scope::Followups), require_scope))
}

fn settings_error(e: SettingsError) -> ApiError {
    ApiError::unprocessable(e.code, e.message)
}

fn limit_of(limit: Option<i64>) -> ApiResult<i64> {
    let limit = limit.unwrap_or(50);
    if (1..=500).contains(&limit) { Ok(limit) } else { Err(ApiError::unprocessable("invalid_limit", "limit must be 1 to 500.")) }
}

// ---------- sequences ----------

#[derive(Deserialize, Serialize, ToSchema, Clone, Default, Debug)]
pub struct StepInput {
    /// Step 1: 0 (or leave out). Step 2 and later: 1 to 365.
    pub delay_amount: Option<i32>,
    /// `business_days` (default) or `calendar_days`.
    pub delay_type: Option<DelayType>,
    /// Required for step 1. Step 2 and later: empty means "Re: " plus the subject of step 1. Variables: `{{name}}`, `{{name|fallback}}`.
    pub subject: Option<String>,
    /// Give `body_text`, `body_html`, or both.
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    /// The AI slots of this step. Every `{{ai:key}}` in the text needs an entry here, and every entry must be used.
    /// `{{ai:...}}` is allowed in the subject of step 1 only (a changed subject breaks the thread).
    pub ai_slots: Option<Vec<AiSlot>>,
    /// AI mode: what to write in this email. Up to 8000 characters. Variables: `{{name}}`, `{{brief}}`, `{{previous_emails}}`, `{{lead_notes}}`.
    pub ai_user_prompt: Option<String>,
}

/// A prompt: up to 8000 characters, and the `{{ }}` parts must parse.
pub(crate) fn check_prompt(what: &str, text: &str) -> ApiResult<()> {
    if text.chars().count() > 8000 {
        return Err(ApiError::unprocessable("invalid_prompt", format!("{what} must be up to 8000 characters.")));
    }
    crate::followups::template::Template::parse(text).map_err(|e| ApiError::unprocessable("invalid_prompt", format!("{what}: {}", e.message)))?;
    Ok(())
}

pub(crate) struct ValidStep {
    ai_user_prompt: Option<String>,
    number: i32,
    delay_amount: i32,
    delay_type: DelayType,
    subject: String,
    body_text: Option<String>,
    body_html: Option<String>,
    ai_slots: Vec<AiSlot>,
    step_hash: String,
    templates: StepTemplates,
}

fn invalid_steps(message: impl Into<String>) -> ApiError {
    ApiError::unprocessable("invalid_steps", message)
}

/// 1 to 10 steps, in order. Step numbers come from the order.
pub(crate) fn validate_steps(inputs: &[StepInput]) -> ApiResult<Vec<ValidStep>> {
    if inputs.is_empty() || inputs.len() > 10 {
        return Err(invalid_steps("A sequence has 1 to 10 steps."));
    }
    let mut out = Vec::new();
    for (i, s) in inputs.iter().enumerate() {
        let n = i as i32 + 1;
        let delay_amount = if n == 1 {
            match s.delay_amount {
                None | Some(0) => 0,
                Some(_) => return Err(invalid_steps("Step 1 goes out at once: its delay_amount must be 0.")),
            }
        } else {
            match s.delay_amount {
                Some(d) if (1..=365).contains(&d) => d,
                _ => return Err(invalid_steps(format!("Step {n}: delay_amount must be 1 to 365."))),
            }
        };
        let subject = s.subject.clone().unwrap_or_default();
        if n == 1 && subject.trim().is_empty() {
            return Err(invalid_steps("Step 1 needs a subject."));
        }
        if subject.len() > 998 || subject.contains(['\r', '\n']) {
            return Err(invalid_steps(format!("Step {n}: the subject must be one line, up to 998 characters.")));
        }
        let has = |b: &Option<String>| b.as_deref().is_some_and(|t| !t.trim().is_empty());
        if !has(&s.body_text) && !has(&s.body_html) {
            return Err(invalid_steps(format!("Step {n}: give body_text, body_html, or both.")));
        }
        let templates = StepTemplates::parse(&subject, s.body_text.as_deref(), s.body_html.as_deref())
            .map_err(|e| ApiError::unprocessable("invalid_template", format!("Step {n}: {}", e.message)))?;
        let ai_slots = slots::validate(s.ai_slots.as_deref().unwrap_or(&[])).map_err(|m| ApiError::unprocessable("invalid_ai_slots", format!("Step {n}: {m}")))?;
        let used = templates.ai_keys();
        if let Some(k) = used.iter().find(|k| !ai_slots.iter().any(|s| &s.key == *k)) {
            return Err(ApiError::unprocessable("unknown_ai_slot", format!("Step {n}: {{{{ai:{k}}}}} has no entry in ai_slots.")));
        }
        if let Some(sl) = ai_slots.iter().find(|s| !used.contains(&s.key)) {
            return Err(ApiError::unprocessable("unused_ai_slot", format!("Step {n}: the ai_slot '{}' is not used. Write {{{{ai:{}}}}} in the text, or remove it.", sl.key, sl.key)));
        }
        if n > 1 {
            if let Some(k) = templates.ai_keys_in_subject().first() {
                return Err(ApiError::unprocessable(
                    "ai_slot_in_threaded_subject",
                    format!("Step {n}: {{{{ai:{k}}}}} is in the subject. From step 2 the subject must stay the same, or the thread breaks."),
                ));
            }
        }
        let body_text = s.body_text.clone().filter(|t| !t.trim().is_empty());
        let body_html = s.body_html.clone().filter(|t| !t.trim().is_empty());
        let step_hash = crate::generator::hash::step_hash(&subject, body_text.as_deref(), body_html.as_deref(), &ai_slots);
        let ai_user_prompt = s.ai_user_prompt.clone().map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
        if let Some(p) = &ai_user_prompt {
            check_prompt(&format!("Step {n}: ai_user_prompt"), p)?;
        }
        out.push(ValidStep {
            ai_user_prompt,
            number: n,
            delay_amount,
            delay_type: s.delay_type.unwrap_or_default(),
            subject,
            body_text,
            body_html,
            ai_slots,
            step_hash,
            templates,
        });
    }
    Ok(out)
}

fn campaign_managed() -> ApiError {
    ApiError::unprocessable("campaign_managed", "A campaign sets the campaign_id of its sequence itself. Create the campaign with POST /v1/campaigns.")
}

fn check_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(ApiError::unprocessable("invalid_name", "name must be 1 to 200 characters."));
    }
    Ok(name.to_string())
}

/// `Some(None)`: an empty brief (clears it). Up to 4000 characters.
fn check_brief(brief: &str) -> ApiResult<Option<String>> {
    let brief = brief.trim();
    if brief.chars().count() > 4000 {
        return Err(ApiError::unprocessable("invalid_brief", "brief must be up to 4000 characters."));
    }
    Ok((!brief.is_empty()).then(|| brief.to_string()))
}

fn check_pause_days(days: i32) -> ApiResult<i32> {
    if (1..=30).contains(&days) { Ok(days) } else { Err(ApiError::unprocessable("invalid_setting", "auto_reply_pause_days must be 1 to 30.")) }
}

#[derive(Deserialize, ToSchema, Clone)]
pub struct CreateSequence {
    pub name: String,
    /// Default `false`: an auto-reply pauses the enrollment. `true`: it stops it.
    pub stop_on_auto_reply: Option<bool>,
    /// 1 to 30, default 3. Business days an auto-reply pauses the enrollment.
    pub auto_reply_pause_days: Option<i32>,
    /// Default `true`: follow-ups show as replies to the first email.
    pub thread_followups: Option<bool>,
    /// Not allowed: a campaign sets this link itself (`POST /v1/campaigns`). Sending it gives `422 campaign_managed`.
    pub campaign_id: Option<Uuid>,
    /// What the Email Generator should know about the sender (product, offer, audience). Up to 4000 characters.
    pub brief: Option<String>,
    /// 1 to 10 steps. The order gives the step numbers.
    pub steps: Vec<StepInput>,
}

#[derive(Serialize, ToSchema)]
pub struct SequenceDetail {
    pub sequence: Sequence,
    pub steps: Vec<SequenceStep>,
    /// Variants B to E of step 1 (variant A is step 1 itself).
    pub variants: Vec<crate::campaigns::emails::StepVariant>,
    /// Enrollments per status.
    pub counts: BTreeMap<String, i64>,
}

async fn sequence_detail(st: &AppState, id: Uuid) -> ApiResult<SequenceDetail> {
    let mut conn = st.pool.acquire().await?;
    let sequence = repo::get_sequence(&mut conn, id).await?.ok_or_else(|| ApiError::not_found("sequence_not_found", "No sequence with this id."))?;
    let steps = repo::get_steps(&mut conn, id).await?;
    let counts: Vec<(String, i64)> =
        sqlx::query_as("SELECT status::text, count(*) FROM enrollments WHERE sequence_id = $1 GROUP BY 1").bind(id).fetch_all(&mut *conn).await?;
    let variants = sqlx::query_as(&format!("SELECT {} FROM step_variants WHERE sequence_id = $1 ORDER BY position, key", crate::campaigns::emails::VARIANT_COLS))
        .bind(id)
        .fetch_all(&mut *conn)
        .await?;
    Ok(SequenceDetail { sequence, steps, variants, counts: counts.into_iter().collect() })
}

pub(crate) async fn write_steps(conn: &mut sqlx::PgConnection, sequence_id: Uuid, steps: &[ValidStep], now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    for s in steps {
        sqlx::query(
            "INSERT INTO sequence_steps (id, sequence_id, step_number, delay_amount, delay_type, subject, body_text, body_html, updated_at, ai_slots, step_hash, ai_user_prompt)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
             ON CONFLICT (sequence_id, step_number) DO UPDATE SET delay_amount = EXCLUDED.delay_amount,
                delay_type = EXCLUDED.delay_type, subject = EXCLUDED.subject, body_text = EXCLUDED.body_text,
                body_html = EXCLUDED.body_html, updated_at = EXCLUDED.updated_at, ai_slots = EXCLUDED.ai_slots, step_hash = EXCLUDED.step_hash, ai_user_prompt = EXCLUDED.ai_user_prompt",
        )
        .bind(Uuid::now_v7())
        .bind(sequence_id)
        .bind(s.number)
        .bind(s.delay_amount)
        .bind(s.delay_type)
        .bind(&s.subject)
        .bind(&s.body_text)
        .bind(&s.body_html)
        .bind(now)
        .bind(serde_json::to_value(&s.ai_slots).unwrap_or_default())
        .bind(&s.step_hash)
        .bind(&s.ai_user_prompt)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Make a sequence with its steps.
#[utoipa::path(post, path = "/v1/followups/sequences", tag = "followups",
    request_body = CreateSequence,
    responses((status = 200, body = SequenceDetail), (status = 422, description = "A field, a step or a template is not valid")),
    security(("api_key" = ["followups"])),
    description = "Scope: `followups`. Step 1 is the first email. Step 2 and later are follow-ups; `delay_amount` is the time after the step before.")]
async fn create_sequence(State(st): State<AppState>, ApiJson(body): ApiJson<CreateSequence>) -> ApiResult<ApiJson<SequenceDetail>> {
    if body.campaign_id.is_some() {
        return Err(campaign_managed());
    }
    let mut tx = st.pool.begin().await?;
    let id = insert_sequence(&mut tx, &body, st.now()).await?;
    tx.commit().await?;
    Ok(ApiJson(sequence_detail(&st, id).await?))
}

/// Check and save a sequence with its steps (status `active`, no campaign). Campaigns call this for an inline sequence.
pub async fn insert_sequence(conn: &mut sqlx::PgConnection, body: &CreateSequence, now: DateTime<Utc>) -> ApiResult<Uuid> {
    let name = check_name(&body.name)?;
    let pause_days = check_pause_days(body.auto_reply_pause_days.unwrap_or(3))?;
    let brief = body.brief.as_deref().map(check_brief).transpose()?.flatten();
    let steps = validate_steps(&body.steps)?;
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO sequences (id, name, stop_on_auto_reply, auto_reply_pause_days, thread_followups, campaign_id, created_at, updated_at, brief)
         VALUES ($1, $2, $3, $4, $5, NULL, $6, $6, $7)",
    )
    .bind(id)
    .bind(name)
    .bind(body.stop_on_auto_reply.unwrap_or(false))
    .bind(pause_days)
    .bind(body.thread_followups.unwrap_or(true))
    .bind(now)
    .bind(brief)
    .execute(&mut *conn)
    .await?;
    write_steps(conn, id, &steps, now).await?;
    Ok(id)
}

#[derive(Deserialize, IntoParams)]
pub struct ListSequencesQuery {
    status: Option<SequenceStatus>,
    /// `next_cursor` from the last page.
    cursor: Option<Uuid>,
    /// 1 to 500. Default 50.
    limit: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct ListSequencesReply {
    pub sequences: Vec<Sequence>,
    pub next_cursor: Option<Uuid>,
}

/// List sequences, newest first. Cursor paging.
#[utoipa::path(get, path = "/v1/followups/sequences", tag = "followups",
    params(ListSequencesQuery),
    responses((status = 200, body = ListSequencesReply)),
    security(("api_key" = ["followups"])), description = "Scope: `followups`.")]
async fn list_sequences(State(st): State<AppState>, ApiQuery(q): ApiQuery<ListSequencesQuery>) -> ApiResult<ApiJson<ListSequencesReply>> {
    let limit = limit_of(q.limit)?;
    let mut rows: Vec<Sequence> = sqlx::query_as(&format!(
        "SELECT {SEQUENCE_COLS} FROM sequences
         WHERE ($1::sequence_status IS NULL OR status = $1) AND ($2::uuid IS NULL OR id < $2)
         ORDER BY id DESC LIMIT $3"
    ))
    .bind(q.status)
    .bind(q.cursor)
    .bind(limit + 1)
    .fetch_all(&st.pool)
    .await?;
    let next_cursor = if rows.len() as i64 > limit {
        rows.truncate(limit as usize);
        rows.last().map(|s| s.id)
    } else {
        None
    };
    Ok(ApiJson(ListSequencesReply { sequences: rows, next_cursor }))
}

/// One sequence with its steps and the number of enrollments per status.
#[utoipa::path(get, path = "/v1/followups/sequences/{id}", tag = "followups",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = SequenceDetail), (status = 404, description = "No such sequence")),
    security(("api_key" = ["followups"])), description = "Scope: `followups`.")]
async fn get_sequence(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<SequenceDetail>> {
    Ok(ApiJson(sequence_detail(&st, id).await?))
}

#[derive(Deserialize, ToSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum SequenceAction {
    /// The scheduler makes no new steps for this sequence. Open enrollments wait.
    Pause,
    /// Back to `active`. Steps that were due meanwhile go out at once.
    Resume,
    /// Like pause, and no new enrollments. Can be resumed.
    Archive,
}

#[derive(Deserialize, ToSchema)]
pub struct PatchSequence {
    pub name: Option<String>,
    pub stop_on_auto_reply: Option<bool>,
    pub auto_reply_pause_days: Option<i32>,
    pub thread_followups: Option<bool>,
    pub campaign_id: Option<Uuid>,
    /// For the Email Generator. An empty text clears it.
    pub brief: Option<String>,
    /// AI mode: the system prompt for all steps. An empty text clears it.
    pub ai_system_prompt: Option<String>,
    pub action: Option<SequenceAction>,
}

/// Change the name, the settings, or the status (`action`: pause, resume, archive).
#[utoipa::path(patch, path = "/v1/followups/sequences/{id}", tag = "followups",
    params(("id" = Uuid, Path)), request_body = PatchSequence,
    responses((status = 200, body = SequenceDetail), (status = 404, description = "No such sequence"), (status = 422, description = "A field is not valid")),
    security(("api_key" = ["followups"])),
    description = "Scope: `followups`. Send only the fields to change. Enrollments that are already open keep going with the new settings.")]
async fn patch_sequence(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(p): ApiJson<PatchSequence>) -> ApiResult<ApiJson<SequenceDetail>> {
    if p.campaign_id.is_some() {
        return Err(campaign_managed());
    }
    let name = p.name.as_deref().map(check_name).transpose()?;
    let days = p.auto_reply_pause_days.map(check_pause_days).transpose()?;
    let brief = p.brief.as_deref().map(check_brief).transpose()?;
    let system_prompt = p.ai_system_prompt.as_deref().map(|t| {
        let t = t.trim();
        check_prompt("ai_system_prompt", t).map(|_| (!t.is_empty()).then(|| t.to_string()))
    });
    let system_prompt = system_prompt.transpose()?;
    if let Some(Some(text)) = &system_prompt {
        let mut conn = st.pool.acquire().await?;
        let defs = crate::leads::fields::FieldDefs::load(&mut conn).await?;
        let unknown = crate::campaigns::email_ai::unknown_names(text, &defs);
        if !unknown.is_empty() {
            return Err(ApiError::unprocessable("missing_variable", format!("ai_system_prompt uses {}, which no lead field, custom field or fallback covers.", unknown.join(", "))));
        }
    }
    let status = p.action.map(|a| match a {
        SequenceAction::Pause => SequenceStatus::Paused,
        SequenceAction::Resume => SequenceStatus::Active,
        SequenceAction::Archive => SequenceStatus::Archived,
    });
    let done = sqlx::query(
        "UPDATE sequences SET name = COALESCE($2, name), stop_on_auto_reply = COALESCE($3, stop_on_auto_reply),
                auto_reply_pause_days = COALESCE($4, auto_reply_pause_days), thread_followups = COALESCE($5, thread_followups),
                status = COALESCE($6, status), updated_at = $7,
                brief = CASE WHEN $8 THEN $9 ELSE brief END,
                ai_system_prompt = CASE WHEN $10 THEN $11 ELSE ai_system_prompt END
         WHERE id = $1",
    )
    .bind(id)
    .bind(name)
    .bind(p.stop_on_auto_reply)
    .bind(days)
    .bind(p.thread_followups)
    .bind(status)
    .bind(st.now())
    .bind(brief.is_some())
    .bind(brief.flatten())
    .bind(system_prompt.is_some())
    .bind(system_prompt.flatten())
    .execute(&st.pool)
    .await?;
    if done.rows_affected() == 0 {
        return Err(ApiError::not_found("sequence_not_found", "No sequence with this id."));
    }
    if p.ai_system_prompt.is_some() {
        let mut conn = st.pool.acquire().await?;
        crate::campaigns::emails::sequence_changed(&mut conn, id).await?;
    }
    Ok(ApiJson(sequence_detail(&st, id).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct PutSteps {
    /// The whole list of steps (1 to 10). The order gives the step numbers.
    pub steps: Vec<StepInput>,
    /// Variants of step 1, up to 4 (keys `B` to `E` in the order given). Leave out to keep the saved ones; `[]` removes them.
    pub variants: Option<Vec<VariantInput>>,
    /// `false`: variant A gets no new leads. At least one variant must stay on. Leave out to keep it.
    pub variant_a_active: Option<bool>,
}

#[derive(Deserialize, ToSchema, Clone, Debug)]
pub struct VariantInput {
    /// A short name for the tab. Default: the key.
    pub name: Option<String>,
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub ai_slots: Option<Vec<AiSlot>>,
    /// `false`: the variant gets no new leads. Default `true`.
    pub active: Option<bool>,
}

/// Replace the steps of a sequence.
#[utoipa::path(put, path = "/v1/followups/sequences/{id}/steps", tag = "followups",
    params(("id" = Uuid, Path)), request_body = PutSteps,
    responses((status = 200, body = SequenceDetail), (status = 404, description = "No such sequence"),
        (status = 409, description = "An enrollment is waiting to send a step that the new list removes"),
        (status = 422, description = "A step or a template is not valid, or an open enrollment lacks a variable the new text needs")),
    security(("api_key" = ["followups"])),
    description = "Scope: `followups`. A step is read when it is due, so a change counts for every enrollment that has not made that step yet. \
        Emails that were sent do not change. A step cannot be removed while an enrollment has it queued.")]
async fn put_steps(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(body): ApiJson<PutSteps>) -> ApiResult<ApiJson<SequenceDetail>> {
    let steps = validate_steps(&body.steps)?;
    let now = st.now();
    let mut tx = st.pool.begin().await?;
    let found: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM sequences WHERE id = $1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?;
    if found.is_none() {
        return Err(ApiError::not_found("sequence_not_found", "No sequence with this id."));
    }
    // 1. A step that an enrollment has queued must stay.
    let (in_use,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM enrollments WHERE sequence_id = $1 AND status IN ('active', 'waiting_send', 'paused')
           AND current_job_id IS NOT NULL AND next_step > $2",
    )
    .bind(id)
    .bind(steps.len() as i32)
    .fetch_one(&mut *tx)
    .await?;
    if in_use > 0 {
        return Err(ApiError::conflict(
            "step_in_use",
            format!("{in_use} enrollment(s) have a step queued that the new list removes. Wait until it is sent, or stop them."),
        ));
    }
    // 2. Every open enrollment that has not made a step yet must have the values the new text needs.
    for s in &steps {
        for name in s.templates.required().into_iter().filter(|n| !BUILTINS.contains(&n.as_str())) {
            let (n,): (i64,) = sqlx::query_as(
                "SELECT count(*) FROM enrollments
                 WHERE sequence_id = $1 AND status IN ('active', 'waiting_send', 'paused')
                   AND (next_step < $2 OR (next_step = $2 AND current_job_id IS NULL))
                   AND btrim(COALESCE(variables ->> $3, '')) = ''",
            )
            .bind(id)
            .bind(s.number)
            .bind(&name)
            .fetch_one(&mut *tx)
            .await?;
            if n > 0 {
                return Err(ApiError::unprocessable(
                    "missing_variable",
                    format!("Step {} needs '{name}', and {n} open enrollment(s) have no value for it. Add a fallback like {{{{{name}|there}}}}.", s.number),
                ));
            }
        }
    }
    {
        let defs = crate::leads::fields::FieldDefs::load(&mut tx).await?;
        for s in &steps {
            if let Some(p) = &s.ai_user_prompt {
                let unknown = crate::campaigns::email_ai::unknown_names(p, &defs);
                if !unknown.is_empty() {
                    return Err(ApiError::unprocessable(
                        "missing_variable",
                        format!("Step {}: the prompt uses {}, which no lead field, custom field or fallback covers.", s.number, unknown.join(", ")),
                    ));
                }
            }
        }
    }
    write_steps(&mut tx, id, &steps, now).await?;
    sqlx::query("DELETE FROM sequence_steps WHERE sequence_id = $1 AND step_number > $2").bind(id).bind(steps.len() as i32).execute(&mut *tx).await?;
    if let Some(a) = body.variant_a_active {
        let others_on = match &body.variants {
            Some(v) => v.iter().any(|x| x.active.unwrap_or(true)),
            None => sqlx::query_as::<_, (bool,)>("SELECT EXISTS (SELECT 1 FROM step_variants WHERE sequence_id = $1 AND active)").bind(id).fetch_one(&mut *tx).await?.0,
        };
        if !a && !others_on {
            return Err(invalid_steps("At least one variant must stay on."));
        }
        sqlx::query("UPDATE sequences SET variant_a_active = $2 WHERE id = $1").bind(id).bind(a).execute(&mut *tx).await?;
    }
    if let Some(variants) = &body.variants {
        if variants.len() > 4 {
            return Err(invalid_steps("Step 1 can have up to 5 variants: A and 4 more."));
        }
        sqlx::query("DELETE FROM step_variants WHERE sequence_id = $1").bind(id).execute(&mut *tx).await?;
        for (i, v) in variants.iter().enumerate() {
            let key = ["B", "C", "D", "E"][i];
            // Same checks as step 1.
            let checked = validate_steps(&[StepInput {
                delay_amount: None,
                delay_type: None,
                subject: Some(v.subject.clone()),
                body_text: v.body_text.clone(),
                body_html: v.body_html.clone(),
                ai_slots: v.ai_slots.clone(),
                ai_user_prompt: None,
            }])
            .map_err(|e| ApiError::unprocessable("invalid_steps", format!("Variant {key}: {}", e.message)))?;
            let c = &checked[0];
            sqlx::query(
                "INSERT INTO step_variants (id, sequence_id, key, name, subject, body_text, body_html, ai_slots, active, position) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            )
            .bind(Uuid::now_v7())
            .bind(id)
            .bind(key)
            .bind(v.name.clone().unwrap_or_default().chars().take(40).collect::<String>())
            .bind(&c.subject)
            .bind(&c.body_text)
            .bind(&c.body_html)
            .bind(serde_json::to_value(&c.ai_slots).unwrap_or_default())
            .bind(v.active.unwrap_or(true))
            .bind(i as i32 + 1)
            .execute(&mut *tx)
            .await?;
        }
    }
    sqlx::query("UPDATE sequences SET updated_at = $2 WHERE id = $1").bind(id).bind(now).execute(&mut *tx).await?;
    // Saved emails that came from the old text are now out of date.
    crate::campaigns::emails::sequence_changed(&mut tx, id).await?;
    tx.commit().await?;
    Ok(ApiJson(sequence_detail(&st, id).await?))
}

#[derive(Deserialize, ToSchema, Default)]
pub struct PreviewBody {
    /// The values for `{{name}}`.
    #[schema(value_type = Option<Object>)]
    pub variables: Option<Value>,
    /// Default `lead@example.com`.
    pub to_email: Option<String>,
    /// Take `sender_name` and `sender_email` from this account.
    pub account_id: Option<Uuid>,
    /// `true`: ask the Email Generator to write the AI slots for `lead_id` (one model call per step with slots; it costs tokens).
    /// Default `false`: an AI slot shows as `[AI: key]`.
    pub generate: Option<bool>,
    /// The lead whose data the AI uses, and whose fields fill the variables. Needed with `generate`.
    pub lead_id: Option<Uuid>,
}

#[derive(Serialize, ToSchema)]
pub struct PreviewStep {
    pub step_number: i32,
    pub delay_amount: i32,
    pub delay_type: DelayType,
    /// What the recipient sees. For step 2 and later, an empty subject shows as "Re: " plus the subject of step 1.
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct PreviewReply {
    pub steps: Vec<PreviewStep>,
}

/// Render every step for the given values. Nothing is sent.
#[utoipa::path(post, path = "/v1/followups/sequences/{id}/preview", tag = "followups",
    params(("id" = Uuid, Path)), request_body = PreviewBody,
    responses((status = 200, body = PreviewReply), (status = 404, description = "No such sequence"),
        (status = 422, description = "A variable has no value and no fallback")),
    security(("api_key" = ["followups"])), description = "Scope: `followups`. Writes nothing.")]
async fn preview(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, raw: axum::body::Bytes) -> ApiResult<ApiJson<PreviewReply>> {
    // An empty body means "no values".
    let body: PreviewBody = crate::error::parse_optional_json(&raw)?;
    let mut conn = st.pool.acquire().await?;
    let seq = repo::get_sequence(&mut conn, id).await?.ok_or_else(|| ApiError::not_found("sequence_not_found", "No sequence with this id."))?;
    let steps = repo::get_steps(&mut conn, id).await?;
    let (account_email, display_name) = match body.account_id {
        Some(a) => repo::account_identity(&mut conn, a).await?.ok_or_else(|| ApiError::not_found("account_not_found", "No account with this id."))?,
        None => ("sender@example.com".to_string(), "Sender".to_string()),
    };
    let email = body.to_email.unwrap_or_else(|| "lead@example.com".into()).trim().to_lowercase();
    let vars = build_vars(body.variables.as_ref().unwrap_or(&Value::Null), &email, &sender_name_of(&display_name, &account_email), &account_email)
        .map_err(|m| ApiError::unprocessable("invalid_variables", m))?;
    let templates = enroll::parse_steps(&steps).map_err(|m| ApiError::unprocessable("invalid_template", m))?;
    drop(conn);
    let mut out: Vec<PreviewStep> = Vec::new();
    let mut problems: Vec<String> = Vec::new();
    if body.generate.unwrap_or(false) && body.lead_id.is_none() {
        return Err(ApiError::unprocessable("lead_required", "generate needs a lead_id: the AI writes for one lead."));
    }
    for (step, (_, t)) in steps.iter().zip(&templates) {
        // An AI slot shows as a marker, or as the text the generator writes for the lead now.
        let mut vars = vars.clone();
        let ai_keys = t.ai_keys();
        if !ai_keys.is_empty() {
            match body.lead_id.filter(|_| body.generate.unwrap_or(false)) {
                Some(lead_id) => {
                    let texts = crate::generator::draft::generate_step(&st, &seq, step, lead_id, "preview").await?;
                    for (k, v) in texts.slots {
                        vars.insert(ai_var(&k), v);
                    }
                }
                None => {
                    for k in ai_keys {
                        vars.insert(ai_var(&k), format!("[AI: {k}]"));
                    }
                }
            }
        }
        match t.render(&vars) {
            Ok(r) => {
                let mut subject = r.subject.trim().to_string();
                if subject.is_empty() && step.step_number > 1 {
                    let base = out.first().map(|f| f.subject.clone()).unwrap_or_default();
                    subject = if !seq.thread_followups || base.to_ascii_lowercase().starts_with("re:") { base } else { format!("Re: {base}") };
                }
                out.push(PreviewStep {
                    step_number: step.step_number,
                    delay_amount: step.delay_amount,
                    delay_type: step.delay_type,
                    subject,
                    body_text: r.body_text,
                    body_html: r.body_html,
                });
            }
            Err(missing) => problems.push(format!("step {} needs: {}", step.step_number, missing.join(", "))),
        }
    }
    if !problems.is_empty() {
        return Err(ApiError::unprocessable("missing_variable", format!("{}. Give a value, or add a fallback like {{{{name|there}}}}.", problems.join("; "))));
    }
    Ok(ApiJson(PreviewReply { steps: out }))
}

// ---------- enrollments ----------

#[derive(Deserialize, ToSchema)]
pub struct CreateEnrollments {
    /// 1 to 1000 recipients.
    pub enrollments: Vec<NewEnrollment>,
}

#[derive(Serialize, ToSchema)]
pub struct CreateEnrollmentsReply {
    pub items: Vec<EnrollItem>,
    pub enrolled: usize,
    pub duplicates: usize,
    pub errors: usize,
}

/// Enroll 1 to 1000 recipients into a sequence.
#[utoipa::path(post, path = "/v1/followups/sequences/{id}/enrollments", tag = "followups",
    params(("id" = Uuid, Path)), request_body = CreateEnrollments,
    responses((status = 200, body = CreateEnrollmentsReply), (status = 404, description = "No such sequence"),
        (status = 409, description = "The sequence is archived"),
        (status = 422, description = "The batch size is wrong, or a step needs a variable that an item does not have (nothing is enrolled)")),
    security(("api_key" = ["followups"])),
    description = "Scope: `followups`. A repeated `idempotency_key` returns the old enrollment. An address that is in an open enrollment, or is \
        suppressed, gets an error on its own item; the other items are still enrolled. A step with a variable that has no value and no fallback \
        blocks the whole call with `422 missing_variable`.")]
async fn create_enrollments(
    State(st): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(body): ApiJson<CreateEnrollments>,
) -> ApiResult<ApiJson<CreateEnrollmentsReply>> {
    if body.enrollments.is_empty() || body.enrollments.len() > 1000 {
        return Err(ApiError::unprocessable("invalid_batch_size", "Send 1 to 1000 enrollments in one call."));
    }
    let seq = {
        let mut conn = st.pool.acquire().await?;
        repo::get_sequence(&mut conn, id).await?.ok_or_else(|| ApiError::not_found("sequence_not_found", "No sequence with this id."))?
    };
    if seq.status == SequenceStatus::Archived {
        return Err(ApiError::conflict("sequence_archived", "This sequence is archived. Resume it to enroll."));
    }
    let items = match enroll::enroll(&st.pool, id, body.enrollments, st.now()).await {
        Ok(items) => items,
        Err(EnrollError::MissingVariables(list)) => {
            return Err(ApiError::unprocessable(
                "missing_variable",
                format!("Nothing was enrolled. {}. Give a value, or add a fallback like {{{{name|there}}}}.", list.join(" | ")),
            ));
        }
        Err(EnrollError::Db(e)) => return Err(e.into()),
    };
    let count = |s: EnrollStatus| items.iter().filter(|i| i.status == s).count();
    Ok(ApiJson(CreateEnrollmentsReply {
        enrolled: count(EnrollStatus::Enrolled),
        duplicates: count(EnrollStatus::Duplicate),
        errors: count(EnrollStatus::Error),
        items,
    }))
}

#[derive(Deserialize, IntoParams)]
pub struct ListEnrollmentsQuery {
    sequence_id: Option<Uuid>,
    status: Option<EnrollmentStatus>,
    /// The recipient address (exact match).
    email: Option<String>,
    /// `next_cursor` from the last page.
    cursor: Option<Uuid>,
    /// 1 to 500. Default 50.
    limit: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct ListEnrollmentsReply {
    pub enrollments: Vec<Enrollment>,
    pub next_cursor: Option<Uuid>,
}

/// List enrollments, newest first. Filter by sequence, status, or address.
#[utoipa::path(get, path = "/v1/followups/enrollments", tag = "followups",
    params(ListEnrollmentsQuery),
    responses((status = 200, body = ListEnrollmentsReply)),
    security(("api_key" = ["followups"])), description = "Scope: `followups`.")]
async fn list_enrollments(State(st): State<AppState>, ApiQuery(q): ApiQuery<ListEnrollmentsQuery>) -> ApiResult<ApiJson<ListEnrollmentsReply>> {
    let limit = limit_of(q.limit)?;
    let email = q.email.as_deref().map(|e| e.trim().to_lowercase());
    let mut rows: Vec<Enrollment> = sqlx::query_as(&format!(
        "SELECT {ENROLLMENT_COLS} FROM enrollments
         WHERE ($1::uuid IS NULL OR sequence_id = $1) AND ($2::enrollment_status IS NULL OR status = $2)
           AND ($3::citext IS NULL OR to_email = $3) AND ($4::uuid IS NULL OR id < $4)
         ORDER BY id DESC LIMIT $5"
    ))
    .bind(q.sequence_id)
    .bind(q.status)
    .bind(email)
    .bind(q.cursor)
    .bind(limit + 1)
    .fetch_all(&st.pool)
    .await?;
    let next_cursor = if rows.len() as i64 > limit {
        rows.truncate(limit as usize);
        rows.last().map(|e| e.id)
    } else {
        None
    };
    Ok(ApiJson(ListEnrollmentsReply { enrollments: rows, next_cursor }))
}

#[derive(Serialize, ToSchema)]
pub struct EnrollmentDetail {
    pub enrollment: Enrollment,
    pub events: Vec<EnrollmentEvent>,
    /// The send jobs of the steps, in order.
    pub jobs: Vec<Job>,
}

async fn enrollment_detail(st: &AppState, id: Uuid) -> ApiResult<EnrollmentDetail> {
    let enrollment = repo::get_enrollment(&st.pool, id).await?.ok_or_else(|| ApiError::not_found("enrollment_not_found", "No enrollment with this id."))?;
    let events = repo::enrollment_events(&st.pool, id).await?;
    let jobs: Vec<Job> = sqlx::query_as(&format!("SELECT {} FROM send_jobs WHERE enrollment_id = $1 ORDER BY step_number, id", queue::JOB_COLS))
        .bind(id)
        .fetch_all(&st.pool)
        .await?;
    Ok(EnrollmentDetail { enrollment, events, jobs })
}

/// One enrollment with its events and send jobs.
#[utoipa::path(get, path = "/v1/followups/enrollments/{id}", tag = "followups",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = EnrollmentDetail), (status = 404, description = "No such enrollment")),
    security(("api_key" = ["followups"])), description = "Scope: `followups`.")]
async fn get_enrollment(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<EnrollmentDetail>> {
    Ok(ApiJson(enrollment_detail(&st, id).await?))
}

fn closed(status: EnrollmentStatus) -> ApiError {
    ApiError::conflict("enrollment_closed", format!("This enrollment is {}. It cannot change any more.", serde_json::to_value(status).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()))
}

async fn locked(tx: &mut sqlx::PgConnection, id: Uuid) -> ApiResult<Enrollment> {
    lifecycle::lock(tx, id).await?.ok_or_else(|| ApiError::not_found("enrollment_not_found", "No enrollment with this id."))
}

/// Pause one enrollment until a person resumes it.
#[utoipa::path(post, path = "/v1/followups/enrollments/{id}/pause", tag = "followups",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = EnrollmentDetail), (status = 404, description = "No such enrollment"), (status = 409, description = "The enrollment is finished, stopped or failed")),
    security(("api_key" = ["followups"])),
    description = "Scope: `followups`. A step that is queued is cancelled; it is made again on resume. A step that is being sent right now still goes out.")]
async fn pause_enrollment(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<EnrollmentDetail>> {
    let mut tx = st.pool.begin().await?;
    let e = locked(&mut tx, id).await?;
    match e.status {
        EnrollmentStatus::Active | EnrollmentStatus::WaitingSend => {
            lifecycle::pause(&mut tx, &e, None, "manual", st.now()).await?;
        }
        EnrollmentStatus::Paused => {}
        other => return Err(closed(other)),
    }
    tx.commit().await?;
    Ok(ApiJson(enrollment_detail(&st, id).await?))
}

/// Resume a paused enrollment.
#[utoipa::path(post, path = "/v1/followups/enrollments/{id}/resume", tag = "followups",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = EnrollmentDetail), (status = 404, description = "No such enrollment"), (status = 409, description = "The enrollment is finished, stopped or failed")),
    security(("api_key" = ["followups"])),
    description = "Scope: `followups`. If the due time already passed, the step is due now.")]
async fn resume_enrollment(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<EnrollmentDetail>> {
    let mut tx = st.pool.begin().await?;
    let e = locked(&mut tx, id).await?;
    match e.status {
        EnrollmentStatus::Paused => {
            lifecycle::resume(&mut tx, &e, "manual", st.now()).await?;
        }
        EnrollmentStatus::Active | EnrollmentStatus::WaitingSend => {}
        other => return Err(closed(other)),
    }
    tx.commit().await?;
    Ok(ApiJson(enrollment_detail(&st, id).await?))
}

/// Stop one enrollment for good (reason `manual`).
#[utoipa::path(post, path = "/v1/followups/enrollments/{id}/stop", tag = "followups",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = EnrollmentDetail), (status = 404, description = "No such enrollment"), (status = 409, description = "The enrollment is finished or failed")),
    security(("api_key" = ["followups"])),
    description = "Scope: `followups`. A step that is queued is cancelled. A step that is being sent right now still goes out.")]
async fn stop_enrollment(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<EnrollmentDetail>> {
    let mut tx = st.pool.begin().await?;
    let e = locked(&mut tx, id).await?;
    match e.status {
        s if s.is_open() => {
            lifecycle::stop(&mut tx, &e, "manual", serde_json::json!({}), st.now()).await?;
        }
        EnrollmentStatus::Stopped => {}
        other => return Err(closed(other)),
    }
    tx.commit().await?;
    Ok(ApiJson(enrollment_detail(&st, id).await?))
}

// ---------- settings and the whole engine ----------

#[derive(Serialize, ToSchema)]
pub struct SettingsReply {
    pub settings: FollowupSettings,
}

/// Read the follow-up settings.
#[utoipa::path(get, path = "/v1/followups/settings", tag = "followups",
    responses((status = 200, body = SettingsReply)),
    security(("api_key" = ["followups"])), description = "Scope: `followups`.")]
async fn get_settings(State(st): State<AppState>) -> ApiResult<ApiJson<SettingsReply>> {
    let mut conn = st.pool.acquire().await?;
    Ok(ApiJson(SettingsReply { settings: repo::load_settings(&mut conn).await? }))
}

/// Change the follow-up settings. Send only the fields to change.
#[utoipa::path(patch, path = "/v1/followups/settings", tag = "followups",
    request_body(content = FollowupSettings, description = "Only the fields to change."),
    responses((status = 200, body = SettingsReply), (status = 422, description = "A setting is not valid")),
    security(("api_key" = ["followups"])),
    description = "Scope: `followups`. Merges the fields, validates the full result, then saves.")]
async fn patch_settings(State(st): State<AppState>, ApiJson(patch): ApiJson<Value>) -> ApiResult<ApiJson<SettingsReply>> {
    let mut tx = st.pool.begin().await?;
    let (current,): (Value,) = sqlx::query_as("SELECT settings FROM followup_defaults WHERE id = 1 FOR UPDATE").fetch_one(&mut *tx).await?;
    let merged = FollowupSettings::from_stored(current).merged(&patch).map_err(settings_error)?;
    merged.validate().map_err(settings_error)?;
    sqlx::query("UPDATE followup_defaults SET settings = $1, updated_at = now() WHERE id = 1")
        .bind(serde_json::to_value(&merged).unwrap_or_default())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(ApiJson(SettingsReply { settings: merged }))
}

#[derive(Serialize, ToSchema)]
pub struct EnginePauseReply {
    pub paused: bool,
}

async fn set_engine_paused(st: &AppState, paused: bool) -> ApiResult<ApiJson<EnginePauseReply>> {
    sqlx::query("UPDATE followup_defaults SET engine_paused = $1, updated_at = now() WHERE id = 1").bind(paused).execute(&st.pool).await?;
    tracing::info!(paused, "follow-up pause flag changed");
    Ok(ApiJson(EnginePauseReply { paused }))
}

/// Pause the whole follow-up engine.
#[utoipa::path(post, path = "/v1/followups/pause", tag = "followups",
    responses((status = 200, body = EnginePauseReply)),
    security(("api_key" = ["followups"])),
    description = "Scope: `followups`. No new steps are made. Steps that are already in the send queue still go out (pause the sending engine to stop those).")]
async fn pause_engine(State(st): State<AppState>) -> ApiResult<ApiJson<EnginePauseReply>> {
    set_engine_paused(&st, true).await
}

/// Resume the whole follow-up engine.
#[utoipa::path(post, path = "/v1/followups/resume", tag = "followups",
    responses((status = 200, body = EnginePauseReply)),
    security(("api_key" = ["followups"])), description = "Scope: `followups`.")]
async fn resume_engine(State(st): State<AppState>) -> ApiResult<ApiJson<EnginePauseReply>> {
    set_engine_paused(&st, false).await
}

#[derive(Serialize, ToSchema)]
pub struct FollowupStatus {
    pub paused: bool,
    /// `false` when the server runs with `AIKIDO_FOLLOWUPS_ENABLED=false` (this copy makes no steps).
    pub scheduler_enabled: bool,
    /// Enrollments per status.
    pub counts: BTreeMap<String, i64>,
    /// Active enrollments whose next step is due within the next hour (or is already due).
    pub due_next_hour: i64,
    /// Enrollments that failed with reason `stalled`.
    pub stalled: i64,
    /// Follow-ups that have waited in the send queue longer than `max_wait_days`: the next tick fails them.
    pub waiting_too_long: i64,
    /// When a scheduler last ran. `null`: never.
    pub last_tick_at: Option<DateTime<Utc>>,
}

/// Counts by status, what is due soon, stalled rows, and the last tick.
#[utoipa::path(get, path = "/v1/followups/status", tag = "followups",
    responses((status = 200, body = FollowupStatus)),
    security(("api_key" = ["followups"])), description = "Scope: `followups`.")]
async fn engine_status(State(st): State<AppState>) -> ApiResult<ApiJson<FollowupStatus>> {
    let now = st.now();
    let mut conn = st.pool.acquire().await?;
    let settings = repo::load_settings(&mut conn).await?;
    let (paused, last_tick_at): (bool, Option<DateTime<Utc>>) =
        sqlx::query_as("SELECT engine_paused, last_tick_at FROM followup_defaults WHERE id = 1").fetch_one(&mut *conn).await?;
    let counts: Vec<(String, i64)> = sqlx::query_as("SELECT status::text, count(*) FROM enrollments GROUP BY 1").fetch_all(&mut *conn).await?;
    let (due_next_hour,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM enrollments e JOIN sequences s ON s.id = e.sequence_id
         WHERE e.status = 'active' AND e.next_due_at <= $1 AND s.status = 'active'",
    )
    .bind(now + chrono::Duration::hours(1))
    .fetch_one(&mut *conn)
    .await?;
    let (stalled,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM enrollments WHERE status = 'failed' AND stop_reason = 'stalled'").fetch_one(&mut *conn).await?;
    let (waiting_too_long,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM enrollments WHERE status = 'waiting_send' AND next_step > 1 AND step_queued_at < $1",
    )
    .bind(now - chrono::Duration::days(settings.max_wait_days as i64))
    .fetch_one(&mut *conn)
    .await?;
    Ok(ApiJson(FollowupStatus {
        paused,
        scheduler_enabled: st.config.followups_enabled,
        counts: counts.into_iter().collect(),
        due_next_hour,
        stalled,
        waiting_too_long,
        last_tick_at,
    }))
}
