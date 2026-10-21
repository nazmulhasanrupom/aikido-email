//! Routes for the emails saved per lead (UI update map v2, B14). Scope `campaigns`.
use super::email_ai::{self, GenerateBody};
use super::emails::{self, EMAIL_COLS, LeadEmail, Overwrite};
use crate::error::{ApiError, ApiJson, ApiPath, ApiResult};
use crate::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_email, patch_email))
        .routes(routes!(estimate))
        .routes(routes!(render))
        .routes(routes!(generate))
        .routes(routes!(test_email))
        .routes(routes!(actions))
        .routes(routes!(list_runs))
        .routes(routes!(get_run))
        .routes(routes!(cancel_run))
}

fn no_campaign() -> ApiError {
    ApiError::not_found("campaign_not_found", "No campaign with this id.")
}

#[derive(FromRow, Serialize, ToSchema, Clone, Debug)]
pub struct EmailRun {
    pub id: Uuid,
    pub campaign_id: Uuid,
    /// `template` or `ai`.
    pub kind: String,
    pub steps: Vec<i32>,
    /// `skip`, `keep_edited` or `all`.
    pub overwrite: String,
    /// `queued`, `running`, `done`, `cancelled` or `failed`.
    pub status: String,
    pub total: i32,
    pub done: i32,
    pub failed: i32,
    pub tokens: i64,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

const RUN_COLS: &str = "id, campaign_id, kind, steps, overwrite, status, total, done, failed, tokens, created_at, started_at, finished_at";

// ---------- one email ----------

/// One saved email.
#[utoipa::path(get, path = "/v1/campaigns/{id}/leads/{lead_id}/emails/{step}", tag = "campaigns",
    params(("id" = Uuid, Path), ("lead_id" = Uuid, Path), ("step" = i32, Path)),
    responses((status = 200, body = LeadEmail), (status = 404, description = "No such campaign, or no email saved for this lead and step")),
    security(("api_key" = ["campaigns"])), description = "Scope: `campaigns`. Step 1 is the primary email, step 2 is Follow up 1, and so on.")]
async fn get_email(State(st): State<AppState>, ApiPath((id, lead_id, step)): ApiPath<(Uuid, Uuid, i32)>) -> ApiResult<ApiJson<LeadEmail>> {
    let row: Option<LeadEmail> = sqlx::query_as(&format!("SELECT {EMAIL_COLS} FROM lead_emails WHERE campaign_id = $1 AND lead_id = $2 AND step_number = $3"))
        .bind(id)
        .bind(lead_id)
        .bind(step)
        .fetch_optional(&st.pool)
        .await?;
    row.map(ApiJson).ok_or_else(|| ApiError::not_found("email_not_found", "No email is saved for this lead and step."))
}

#[derive(Deserialize, ToSchema)]
pub struct PatchEmail {
    pub subject: Option<String>,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
}

/// Edit an email by hand.
#[utoipa::path(patch, path = "/v1/campaigns/{id}/leads/{lead_id}/emails/{step}", tag = "campaigns",
    params(("id" = Uuid, Path), ("lead_id" = Uuid, Path), ("step" = i32, Path)), request_body = PatchEmail,
    responses((status = 200, body = LeadEmail), (status = 404, description = "No such campaign, lead or step"),
        (status = 409, description = "email_sent: the email was sent"), (status = 422, description = "The text is not valid")),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. The email becomes `manual`, `edited_by_hand` and `ready`. A later Save keeps it unless it asks to replace all. \
        The text is saved as written: write real values, or the tokens `{{sender_name}}` and `{{sender_email}}`, which are filled when the email is sent.")]
async fn patch_email(State(st): State<AppState>, ApiPath((id, lead_id, step)): ApiPath<(Uuid, Uuid, i32)>, ApiJson(p): ApiJson<PatchEmail>) -> ApiResult<ApiJson<LeadEmail>> {
    let now = st.now();
    let mut tx = st.pool.begin().await?;
    let ctx = emails::load_ctx(&mut tx, id).await?.ok_or_else(no_campaign)?;
    let step_row = ctx.steps.iter().find(|s| s.step_number == step).ok_or_else(|| ApiError::not_found("step_not_found", "The sequence has no such step."))?;
    let in_campaign: Option<(i32,)> = sqlx::query_as("SELECT 1 FROM campaign_leads WHERE campaign_id = $1 AND lead_id = $2").bind(id).bind(lead_id).fetch_optional(&mut *tx).await?;
    if in_campaign.is_none() {
        return Err(ApiError::not_found("lead_not_in_campaign", "This lead is not in the campaign."));
    }
    let cur: Option<LeadEmail> = sqlx::query_as(&format!("SELECT {EMAIL_COLS} FROM lead_emails WHERE campaign_id = $1 AND lead_id = $2 AND step_number = $3 FOR UPDATE"))
        .bind(id)
        .bind(lead_id)
        .bind(step)
        .fetch_optional(&mut *tx)
        .await?;
    if cur.as_ref().is_some_and(|c| c.status == "sent") {
        return Err(ApiError::conflict("email_sent", "This email was sent. It can not be changed."));
    }
    let subject = p.subject.or_else(|| cur.as_ref().map(|c| c.subject.clone())).unwrap_or_default();
    let body_text = p.body_text.or_else(|| cur.as_ref().and_then(|c| c.body_text.clone())).filter(|t| !t.trim().is_empty());
    let body_html = p.body_html.or_else(|| cur.as_ref().and_then(|c| c.body_html.clone())).filter(|t| !t.trim().is_empty());
    let subject = subject.trim().to_string();
    if subject.chars().count() > 998 || subject.contains(['\r', '\n']) {
        return Err(ApiError::unprocessable("invalid_email", "The subject must be one line, up to 998 characters."));
    }
    if step_row.step_number == 1 && subject.is_empty() {
        return Err(ApiError::unprocessable("invalid_email", "The first email needs a subject."));
    }
    if body_text.is_none() && body_html.is_none() {
        return Err(ApiError::unprocessable("invalid_email", "Give body_text, body_html, or both."));
    }
    let variant = cur.as_ref().and_then(|c| c.variant_key.clone()).or_else(|| (step == 1).then(|| "A".to_string()));
    let row: LeadEmail = sqlx::query_as(&format!(
        "INSERT INTO lead_emails (id, campaign_id, lead_id, step_number, variant_key, subject, body_text, body_html, source, status, edited_by_hand, created_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'manual', 'ready', true, $9, $9)
         ON CONFLICT (campaign_id, lead_id, step_number) DO UPDATE SET subject = EXCLUDED.subject, body_text = EXCLUDED.body_text, body_html = EXCLUDED.body_html,
            source = 'manual', status = 'ready', edited_by_hand = true, error_code = NULL, updated_at = EXCLUDED.updated_at
         RETURNING {EMAIL_COLS}"
    ))
    .bind(Uuid::now_v7())
    .bind(id)
    .bind(lead_id)
    .bind(step)
    .bind(variant)
    .bind(subject)
    .bind(body_text)
    .bind(body_html)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(ApiJson(row))
}

// ---------- Save (template mode) ----------

#[derive(Deserialize, ToSchema, Default)]
pub struct RenderBody {
    /// `skip`, `keep_edited` (default) or `all`.
    #[serde(default)]
    pub overwrite: Overwrite,
}

#[derive(Serialize, ToSchema)]
pub struct Estimate {
    /// How many emails would be written.
    pub emails: i64,
    /// Emails edited by hand that would be replaced (`overwrite: all`).
    pub edited_by_hand: i64,
    /// Template mode: steps with an AI part. Save leaves them to the Email Generator.
    pub skipped_ai_steps: Vec<i32>,
    pub tokens: i64,
    /// From `prices` in the generator settings. `0` when the model has no price (`cost_known` is `false`).
    pub cost_usd: f64,
    pub cost_known: bool,
    /// AI mode: some chosen follow ups are written before their primary email, so `{{previous_emails}}` is empty for them.
    pub follow_ups_without_primary: bool,
}

/// What Save (template mode) or Generate (AI mode) would do. Writes nothing.
#[utoipa::path(post, path = "/v1/campaigns/{id}/emails/estimate", tag = "campaigns",
    params(("id" = Uuid, Path)), request_body = GenerateBody,
    responses((status = 200, body = Estimate), (status = 404, description = "No such campaign")),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. Same body as generate (`steps`, `leads`, `overwrite`; only `overwrite` counts in template mode). \
        Template mode: tokens and cost are 0. AI mode: a rough count, 4 characters per token, about 220 output tokens per email.")]
async fn estimate(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(b): ApiJson<GenerateBody>) -> ApiResult<ApiJson<Estimate>> {
    let mut conn = st.pool.acquire().await?;
    let ctx = emails::load_ctx(&mut conn, id).await?.ok_or_else(no_campaign)?;
    if !ctx.campaign.is_ai() {
        let plan = emails::plan(&mut conn, &ctx, b.overwrite.unwrap_or_default(), None).await?;
        return Ok(ApiJson(Estimate {
            emails: plan.emails,
            edited_by_hand: plan.edited_by_hand,
            skipped_ai_steps: plan.skipped_ai_steps,
            tokens: 0,
            cost_usd: 0.0,
            cost_known: true,
            follow_ups_without_primary: false,
        }));
    }
    let mut pairs = email_ai::targets(&mut conn, &ctx, &b).await?;
    let prompts = email_ai::load_prompts(&mut conn, &ctx).await?;
    if prompts.optimized.is_some() {
        let mut seen = std::collections::HashSet::new();
        pairs.retain(|(lead, _)| seen.insert(*lead));
    }
    let settings = crate::generator::repo::load_settings(&mut conn).await?;
    let rules_len = 700usize;
    let mut tokens_in = 0i64;
    for (_, step) in &pairs {
        let chars = match &prompts.optimized {
            // the instructions, the lead block and the answer format
            Some(o) => o.system.as_deref().map(str::len).unwrap_or(crate::prompting::optimized::DEFAULT_SYSTEM.len()) + o.user.as_deref().map(str::len).unwrap_or(crate::prompting::optimized::DEFAULT_USER.len()) + rules_len,
            None => prompts.system.as_deref().map(str::len).unwrap_or(email_ai::DEFAULT_SYSTEM.len()) + prompts.users.get(step).map(String::len).unwrap_or(0) + rules_len,
        };
        tokens_in += (chars / 4) as i64 + 60;
    }
    let tokens_out = pairs.len() as i64 * if prompts.optimized.is_some() { 600 } else { 220 };
    let model = match settings.slot_provider_id {
        Some(p) => crate::generator::providers::get(&mut conn, p).await?.map(|p| p.model),
        None => None,
    };
    let price = model.and_then(|m| settings.prices.get(&m).cloned());
    let cost = price.as_ref().map(|p| (tokens_in as f64 * p.input + tokens_out as f64 * p.output) / 1_000_000.0).unwrap_or(0.0);
    let have = emails::existing(&mut conn, id).await?;
    let wrote_primary = |lead: &Uuid| pairs.iter().any(|(l, s)| l == lead && *s == 1) || have.get(&(*lead, 1)).is_some_and(|h| matches!(h.0.as_str(), "ready" | "review" | "stale" | "sent"));
    let edited = pairs.iter().filter(|(l, s)| have.get(&(*l, *s)).is_some_and(|h| h.1)).count() as i64;
    Ok(ApiJson(Estimate {
        emails: pairs.len() as i64,
        edited_by_hand: edited,
        skipped_ai_steps: vec![],
        tokens: tokens_in + tokens_out,
        cost_usd: cost,
        cost_known: price.is_some(),
        follow_ups_without_primary: pairs.iter().any(|(l, s)| *s > 1 && !wrote_primary(l)),
    }))
}

/// Save: write the emails of every lead from the templates.
#[utoipa::path(post, path = "/v1/campaigns/{id}/emails/render", tag = "campaigns",
    params(("id" = Uuid, Path)), request_body = RenderBody,
    responses((status = 202, body = EmailRun), (status = 404, description = "No such campaign"), (status = 422, description = "wrong_mode: the campaign is in AI mode")),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. Template mode. Variants of step 1 are split evenly in the order the leads were added. Steps with an AI part are left to the Email Generator. \
        Sent emails never change. After the first Save, leads added later get their emails at once.")]
async fn render(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(b): ApiJson<RenderBody>) -> ApiResult<(StatusCode, ApiJson<EmailRun>)> {
    let now = st.now();
    let mut tx = st.pool.begin().await?;
    sqlx::query("SELECT 1 FROM campaigns WHERE id = $1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?.ok_or_else(no_campaign)?;
    let ctx = emails::load_ctx(&mut tx, id).await?.ok_or_else(no_campaign)?;
    if ctx.campaign.content_mode != "template" {
        return Err(ApiError::unprocessable("wrong_mode", "The campaign is in AI mode. Use generate."));
    }
    let run_id = Uuid::now_v7();
    let steps: Vec<i32> = ctx.steps.iter().map(|s| s.step_number).collect();
    sqlx::query("INSERT INTO lead_email_runs (id, campaign_id, kind, steps, overwrite, status, created_at, started_at) VALUES ($1, $2, 'template', $3, $4, 'running', $5, $5)")
        .bind(run_id)
        .bind(id)
        .bind(&steps)
        .bind(b.overwrite.as_str())
        .bind(now)
        .execute(&mut *tx)
        .await?;
    let w = emails::write(&mut tx, &ctx, b.overwrite, None, Some(run_id), now).await?;
    let run: EmailRun = sqlx::query_as(&format!(
        "UPDATE lead_email_runs SET status = 'done', total = $2, done = $3, failed = $4, finished_at = $5 WHERE id = $1 RETURNING {RUN_COLS}"
    ))
    .bind(run_id)
    .bind((w.done + w.failed) as i32)
    .bind(w.done as i32)
    .bind(w.failed as i32)
    .bind(now)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok((StatusCode::ACCEPTED, ApiJson(run)))
}

// ---------- Generate (AI mode) ----------

/// The slot provider must exist and have a working key.
pub(crate) async fn ready_provider(conn: &mut sqlx::PgConnection) -> ApiResult<crate::generator::providers::Provider> {
    let settings = crate::generator::repo::load_settings(conn).await?;
    let p = match settings.slot_provider_id {
        Some(id) => crate::generator::providers::get(conn, id).await?,
        None => None,
    };
    match p {
        Some(p) if p.status == "ok" => Ok(p),
        _ => Err(ApiError::unprocessable("generator_not_ready", "Set up an AI model first (Settings, AI). The model for per-lead text must have a working key.")),
    }
}

/// Generate: the model writes the emails.
#[utoipa::path(post, path = "/v1/campaigns/{id}/emails/generate", tag = "campaigns",
    params(("id" = Uuid, Path)), request_body = GenerateBody,
    responses((status = 202, body = EmailRun), (status = 404, description = "No such campaign"),
        (status = 422, description = "wrong_mode (template mode), generator_not_ready (no working AI model), missing_prompt (a step has no user prompt)")),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. AI mode. Starts a run and answers at once; the generator worker writes the emails (status `writing`, then `ready`, or `review` when review is on). \
        `steps`: step numbers (default all). `leads`: `{ count }`, `{ lead_ids }` or `{ filter: { step, status } }` (default all). `overwrite`: `skip` (default), `keep_edited`, `all`. \
        The primary email of a lead is written before its follow ups in the same run. Sent emails never change.")]
async fn generate(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(b): ApiJson<GenerateBody>) -> ApiResult<(StatusCode, ApiJson<EmailRun>)> {
    let now = st.now();
    let mut tx = st.pool.begin().await?;
    sqlx::query("SELECT 1 FROM campaigns WHERE id = $1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?.ok_or_else(no_campaign)?;
    let ctx = emails::load_ctx(&mut tx, id).await?.ok_or_else(no_campaign)?;
    if !ctx.campaign.is_ai() {
        return Err(ApiError::unprocessable("wrong_mode", "The campaign is in template mode. Use render (Save)."));
    }
    ready_provider(&mut tx).await?;
    let wanted: Vec<i32> = match &b.steps {
        Some(s) if !s.is_empty() => s.clone(),
        _ => ctx.steps.iter().map(|x| x.step_number).collect(),
    };
    let optimized = ctx.campaign.content_mode == "optimized";
    for n in &wanted {
        let step = ctx.steps.iter().find(|s| s.step_number == *n).ok_or_else(|| ApiError::unprocessable("invalid_steps", format!("The sequence has no step {n}.")))?;
        if !optimized && step.ai_user_prompt.as_deref().is_none_or(|p| p.trim().is_empty()) {
            return Err(ApiError::unprocessable("missing_prompt", format!("Step {n} has no user prompt. Write it in the Editor first.")));
        }
    }
    let mut pairs = email_ai::targets(&mut tx, &ctx, &b).await?;
    if optimized {
        // one call writes all four emails of a lead, so a lead is queued once, at its first wanted step
        let mut seen = std::collections::HashSet::new();
        pairs.retain(|(lead, _)| seen.insert(*lead));
    }
    let steps: Vec<i32> = { let mut v: Vec<i32> = pairs.iter().map(|p| p.1).collect(); v.sort_unstable(); v.dedup(); v };
    let run_id = Uuid::now_v7();
    sqlx::query("INSERT INTO lead_email_runs (id, campaign_id, kind, steps, scope, overwrite, status, total, created_at) VALUES ($1, $2, 'ai', $3, $4, $5, $6, $7, $8)")
        .bind(run_id)
        .bind(id)
        .bind(&steps)
        .bind(serde_json::to_value(&b.leads).unwrap_or_default())
        .bind(b.overwrite.unwrap_or(Overwrite::Skip).as_str())
        .bind(if pairs.is_empty() { "done" } else { "queued" })
        .bind(pairs.len() as i32)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    email_ai::enqueue(&mut tx, id, &pairs, Some(run_id), now).await?;
    let run: EmailRun = sqlx::query_as(&format!("SELECT {RUN_COLS} FROM lead_email_runs WHERE id = $1")).bind(run_id).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok((StatusCode::ACCEPTED, ApiJson(run)))
}

#[derive(Deserialize, ToSchema)]
pub struct TestBody {
    pub lead_id: Uuid,
    /// 1 = Primary email.
    pub step: i32,
}

#[derive(Serialize, ToSchema)]
pub struct EmailTestReply {
    /// `template` or `ai`.
    pub mode: String,
    /// AI mode: the filled prompts (the system prompt has the fixed rules at the end).
    pub system: Option<String>,
    pub user: Option<String>,
    pub subject: String,
    pub body: String,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub warnings: Vec<String>,
    /// Optimised mode: all 4 emails (the primary email and the 3 follow-ups). `subject` and `body` are the one asked for.
    pub emails: Vec<TestMail>,
}

#[derive(Serialize, ToSchema)]
pub struct TestMail {
    pub step: i32,
    pub subject: String,
    pub body: String,
}

/// Try the prompts (or the template) on one lead. Saves nothing.
#[utoipa::path(post, path = "/v1/campaigns/{id}/emails/test", tag = "campaigns",
    params(("id" = Uuid, Path)), request_body = TestBody,
    responses((status = 200, body = EmailTestReply), (status = 404, description = "No such campaign, lead or step"),
        (status = 422, description = "generator_not_ready, missing_prompt, missing_variable or generation_failed")),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. AI mode calls the model once (and once more to repair) and saves only an `llm_calls` row (purpose `lead_email_test`). \
        Template mode renders the template with the lead values. The sender tokens `{{sender_name}}` stay as they are.")]
async fn test_email(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(b): ApiJson<TestBody>) -> ApiResult<ApiJson<EmailTestReply>> {
    let now = st.now();
    let mut conn = st.pool.acquire().await?;
    let ctx = emails::load_ctx(&mut conn, id).await?.ok_or_else(no_campaign)?;
    let step = ctx.steps.iter().find(|s| s.step_number == b.step).ok_or_else(|| ApiError::not_found("step_not_found", "The sequence has no such step."))?.clone();
    let lead = crate::leads::repo::get(&mut conn, b.lead_id).await?.ok_or_else(|| ApiError::not_found("lead_not_found", "No lead with this id."))?;
    if !ctx.campaign.is_ai() {
        let built = emails::build(&ctx, &lead, &step, "A").map_err(|m| ApiError::unprocessable("missing_variable", format!("Missing: {}", m.join(", "))))?;
        return Ok(ApiJson(EmailTestReply {
            mode: "template".into(),
            system: None,
            user: None,
            subject: built.subject,
            body: built.body_text.unwrap_or_default(),
            tokens_in: 0,
            tokens_out: 0,
            warnings: vec![],
            emails: vec![],
        }));
    }
    let settings = crate::generator::repo::load_settings(&mut conn).await?;
    let provider = ready_provider(&mut conn).await?;
    let prompts = email_ai::load_prompts(&mut conn, &ctx).await?;
    if prompts.optimized.is_none() && !prompts.users.contains_key(&b.step) {
        return Err(ApiError::unprocessable("missing_prompt", format!("Step {} has no user prompt.", b.step)));
    }
    let (previous_emails, step_emails) = email_ai::saved_emails(&mut conn, id, b.lead_id, b.step).await?;
    let extra = email_ai::Extra { previous_emails, step_emails, lead_notes: String::new() };
    let (system, user) = email_ai::build_prompts(&ctx, &prompts, &lead, b.step, &extra).map_err(|m| ApiError::unprocessable("missing_variable", format!("Missing: {}", m.join(", "))))?;
    let (cfg, _) = crate::generator::providers::load_config(&st.pool, &st.crypto, provider.id, settings.max_attempts)
        .await
        .map_err(|_| ApiError::unprocessable("generator_not_ready", "The AI model can not be used (its key can not be read)."))?;
    let client = st.llm.build(&cfg).map_err(|_| ApiError::unprocessable("generator_not_ready", "The AI model can not be used."))?;
    if let Some(opt) = &prompts.optimized {
        // Optimised mode: one call writes all four
        let out = super::email_opt::generate(client.as_ref(), &settings, opt, &lead).await;
        let calls = match &out {
            Ok(g) => g.calls.clone(),
            Err(f) => f.calls.clone(),
        };
        for c in &calls {
            crate::generator::repo::record_call(&mut conn, &c.record("lead_email_test", Some(provider.id), Some(id), &provider.model), now).await?;
        }
        let (tin, tout, _) = crate::generator::pipeline::totals(&calls);
        let g = out.map_err(|f| ApiError::unprocessable("generation_failed", format!("{} ({})", f.message, f.code)))?;
        let emails: Vec<TestMail> = g.mails.iter().enumerate().map(|(i, m)| TestMail { step: i as i32 + 1, subject: m.subject.clone(), body: m.body.clone() }).collect();
        let mine = emails.iter().find(|m| m.step == b.step).or(emails.first());
        return Ok(ApiJson(EmailTestReply {
            mode: "optimized".into(),
            system: Some(system),
            user: Some(user),
            subject: mine.map(|m| m.subject.clone()).unwrap_or_default(),
            body: mine.map(|m| m.body.clone()).unwrap_or_default(),
            tokens_in: tin as i64,
            tokens_out: tout as i64,
            warnings: vec![],
            emails,
        }));
    }
    let result = email_ai::generate_email(client.as_ref(), &settings, &system, &user, email_ai::wants_subject(b.step, prompts.thread)).await;
    let calls = match &result {
        Ok(g) => g.calls.clone(),
        Err(f) => f.calls.clone(),
    };
    for c in &calls {
        crate::generator::repo::record_call(&mut conn, &c.record("lead_email_test", Some(provider.id), Some(id), &provider.model), now).await?;
    }
    let (tin, tout, _) = crate::generator::pipeline::totals(&calls);
    match result {
        Ok(g) => Ok(ApiJson(EmailTestReply { mode: "ai".into(), system: Some(system), user: Some(user), subject: g.subject, body: g.body, tokens_in: tin as i64, tokens_out: tout as i64, warnings: g.warnings, emails: vec![] })),
        Err(f) => Err(ApiError::unprocessable("generation_failed", format!("{} ({})", f.message, f.code))),
    }
}

#[derive(Deserialize, ToSchema)]
pub struct ActionBody {
    /// `approve` (review to ready), `reject` or `regenerate` (write again), `clear` (delete the email).
    pub action: String,
    /// Ids of saved emails.
    pub ids: Option<Vec<Uuid>>,
    pub filter: Option<ActionFilter>,
}

#[derive(Deserialize, ToSchema)]
pub struct ActionFilter {
    pub step: Option<i32>,
    pub status: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ActionReply {
    pub changed: i64,
}

/// Approve, reject, regenerate or clear saved emails.
#[utoipa::path(post, path = "/v1/campaigns/{id}/emails/actions", tag = "campaigns",
    params(("id" = Uuid, Path)), request_body = ActionBody,
    responses((status = 200, body = ActionReply), (status = 404, description = "No such campaign"), (status = 422, description = "Unknown action, or no ids and no filter")),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. Pick emails by `ids` or by `filter: { step, status }`. Sent emails never change. \
        `approve`: `review` to `ready`. `reject` and `regenerate`: the model writes the email again (the old text stays until the new one is saved). `clear`: the email is deleted.")]
async fn actions(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(b): ApiJson<ActionBody>) -> ApiResult<ApiJson<ActionReply>> {
    if !matches!(b.action.as_str(), "approve" | "reject" | "regenerate" | "clear") {
        return Err(ApiError::unprocessable("invalid_action", "action must be approve, reject, regenerate or clear."));
    }
    if b.ids.is_none() && b.filter.is_none() {
        return Err(ApiError::unprocessable("invalid_selection", "Give ids or a filter."));
    }
    let now = st.now();
    let mut tx = st.pool.begin().await?;
    sqlx::query("SELECT 1 FROM campaigns WHERE id = $1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?.ok_or_else(no_campaign)?;
    let f = b.filter.as_ref();
    let sel = "campaign_id = $1 AND status <> 'sent' AND ($2::uuid[] IS NULL OR id = ANY($2)) AND ($3::int IS NULL OR step_number = $3) AND ($4::text IS NULL OR status = $4)";
    let (ids, step, status) = (b.ids.clone(), f.and_then(|f| f.step), f.and_then(|f| f.status.clone()));
    let sql = match b.action.as_str() {
        "approve" => format!("UPDATE lead_emails SET status = 'ready', updated_at = $5 WHERE {sel} AND status = 'review'"),
        "clear" => format!("DELETE FROM lead_emails WHERE {sel} AND $5::timestamptz IS NOT NULL"),
        _ => format!(
            "UPDATE lead_emails SET status = 'writing', source = 'ai', run_id = NULL, attempts = 0, claim_id = NULL, claimed_at = NULL, retry_at = NULL, error_code = NULL, updated_at = $5
              WHERE {sel} AND status <> 'writing'"
        ),
    };
    let changed = sqlx::query(&sql).bind(id).bind(ids).bind(step).bind(status).bind(now).execute(&mut *tx).await?.rows_affected() as i64;
    tx.commit().await?;
    Ok(ApiJson(ActionReply { changed }))
}

// ---------- runs ----------

/// The runs of a campaign, newest first (up to 50).
#[utoipa::path(get, path = "/v1/campaigns/{id}/email-runs", tag = "campaigns",
    params(("id" = Uuid, Path)), responses((status = 200, body = Vec<EmailRun>), (status = 404, description = "No such campaign")),
    security(("api_key" = ["campaigns"])), description = "Scope: `campaigns`.")]
async fn list_runs(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<Vec<EmailRun>>> {
    sqlx::query("SELECT 1 FROM campaigns WHERE id = $1").bind(id).fetch_optional(&st.pool).await?.ok_or_else(no_campaign)?;
    let rows = sqlx::query_as(&format!("SELECT {RUN_COLS} FROM lead_email_runs WHERE campaign_id = $1 ORDER BY created_at DESC LIMIT 50")).bind(id).fetch_all(&st.pool).await?;
    Ok(ApiJson(rows))
}

/// One run with its progress.
#[utoipa::path(get, path = "/v1/campaigns/{id}/email-runs/{run}", tag = "campaigns",
    params(("id" = Uuid, Path), ("run" = Uuid, Path)), responses((status = 200, body = EmailRun), (status = 404, description = "No such run")),
    security(("api_key" = ["campaigns"])), description = "Scope: `campaigns`.")]
async fn get_run(State(st): State<AppState>, ApiPath((id, run)): ApiPath<(Uuid, Uuid)>) -> ApiResult<ApiJson<EmailRun>> {
    let r: Option<EmailRun> = sqlx::query_as(&format!("SELECT {RUN_COLS} FROM lead_email_runs WHERE campaign_id = $1 AND id = $2")).bind(id).bind(run).fetch_optional(&st.pool).await?;
    r.map(ApiJson).ok_or_else(|| ApiError::not_found("run_not_found", "No such run."))
}

/// Stop a run that is not finished.
#[utoipa::path(post, path = "/v1/campaigns/{id}/email-runs/{run}/cancel", tag = "campaigns",
    params(("id" = Uuid, Path), ("run" = Uuid, Path)), responses((status = 200, body = EmailRun), (status = 404, description = "No such run"), (status = 409, description = "run_finished")),
    security(("api_key" = ["campaigns"])), description = "Scope: `campaigns`. A template run is done when the call returns, so it can not be cancelled.")]
async fn cancel_run(State(st): State<AppState>, ApiPath((id, run)): ApiPath<(Uuid, Uuid)>) -> ApiResult<ApiJson<EmailRun>> {
    let r: Option<EmailRun> = sqlx::query_as(&format!(
        "UPDATE lead_email_runs SET status = 'cancelled', finished_at = now() WHERE campaign_id = $1 AND id = $2 AND status IN ('queued', 'running') RETURNING {RUN_COLS}"
    ))
    .bind(id)
    .bind(run)
    .fetch_optional(&st.pool)
    .await?;
    if let Some(r) = r {
        // Rows that still wait go back: with text they are ready again, without text they are not written.
        sqlx::query(
            "UPDATE lead_emails SET status = CASE WHEN body_text IS NOT NULL THEN 'ready' ELSE 'none' END, claim_id = NULL, claimed_at = NULL, updated_at = now()
              WHERE run_id = $1 AND status = 'writing'",
        )
        .bind(r.id)
        .execute(&st.pool)
        .await?;
        return Ok(ApiJson(r));
    }
    let exists: Option<(i32,)> = sqlx::query_as("SELECT 1 FROM lead_email_runs WHERE campaign_id = $1 AND id = $2").bind(id).bind(run).fetch_optional(&st.pool).await?;
    Err(if exists.is_some() { ApiError::conflict("run_finished", "The run is finished.") } else { ApiError::not_found("run_not_found", "No such run.") })
}
