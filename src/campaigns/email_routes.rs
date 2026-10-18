//! Routes for the emails saved per lead (UI update map v2, B14). Scope `campaigns`.
use super::emails::{self, EMAIL_COLS, LeadEmail, Overwrite, Plan};
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
    #[serde(flatten)]
    pub plan: Plan,
    pub tokens: i64,
    pub cost_usd: f64,
}

/// What Save would do. Writes nothing.
#[utoipa::path(post, path = "/v1/campaigns/{id}/emails/estimate", tag = "campaigns",
    params(("id" = Uuid, Path)), request_body = RenderBody,
    responses((status = 200, body = Estimate), (status = 404, description = "No such campaign")),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. Template mode: `emails` is how many emails Save would write, `edited_by_hand` how many hand edits exist (Save with `all` would replace them). Tokens and cost are 0.")]
async fn estimate(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(b): ApiJson<RenderBody>) -> ApiResult<ApiJson<Estimate>> {
    let mut conn = st.pool.acquire().await?;
    let ctx = emails::load_ctx(&mut conn, id).await?.ok_or_else(no_campaign)?;
    let plan = emails::plan(&mut conn, &ctx, b.overwrite, None).await?;
    Ok(ApiJson(Estimate { plan, tokens: 0, cost_usd: 0.0 }))
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
        return Ok(ApiJson(r));
    }
    let exists: Option<(i32,)> = sqlx::query_as("SELECT 1 FROM lead_email_runs WHERE campaign_id = $1 AND id = $2").bind(id).bind(run).fetch_optional(&st.pool).await?;
    Err(if exists.is_some() { ApiError::conflict("run_finished", "The run is finished.") } else { ApiError::not_found("run_not_found", "No such run.") })
}
