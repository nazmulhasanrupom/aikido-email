//! The optimized prompt of a campaign (AI mode): the default blocks, the campaign copy, reset, and a preview on one lead.
//! Scope `campaigns`. The 10 a minute limit counts the preview.
use super::email_ai;
use super::email_opt::{self, Verdict};
use super::emails;
use crate::error::{ApiError, ApiJson, ApiPath, ApiResult};
use crate::prompting::checks::{self, CheckInput, CheckResult, ModelOut};
use crate::prompting::{self, DEFAULT_PROMPT_VERSION};
use crate::state::AppState;
use axum::Extension;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sqlx::PgConnection;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(get_defaults))
        .routes(routes!(get_prompts, put_prompts))
        .routes(routes!(reset_prompt))
        .routes(routes!(preview))
}

fn no_campaign() -> ApiError {
    ApiError::not_found("campaign_not_found", "No campaign with this id.")
}

#[derive(Serialize, ToSchema)]
pub struct Defaults {
    /// The version of the shipped blocks.
    pub version: i32,
    pub core: String,
    pub primary: String,
    pub followup: String,
}

/// The shipped default prompt blocks.
#[utoipa::path(get, path = "/v1/prompts/defaults", tag = "campaigns",
    responses((status = 200, body = Defaults)),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. Core is used by every step. Primary is for step 1. Follow-up is for step 2 and later.")]
async fn get_defaults() -> ApiJson<Defaults> {
    ApiJson(Defaults {
        version: DEFAULT_PROMPT_VERSION,
        core: prompting::DEFAULT_CORE.to_string(),
        primary: prompting::DEFAULT_PRIMARY.to_string(),
        followup: prompting::DEFAULT_FOLLOWUP.to_string(),
    })
}

#[derive(Serialize, ToSchema)]
pub struct StepDelay {
    pub step_number: i32,
    pub delay_amount: i32,
    pub delay_type: String,
}

#[derive(Serialize, ToSchema)]
pub struct PromptBlock {
    /// The text in use: the campaign copy, or the shipped default when the campaign has none.
    pub text: String,
    /// `true`: the campaign has no copy and uses the shipped default.
    pub is_default: bool,
}

#[derive(Serialize, ToSchema)]
pub struct CampaignPrompts {
    /// `optimized` or `custom`.
    pub kind: String,
    pub core: PromptBlock,
    pub primary: PromptBlock,
    pub followup: PromptBlock,
    /// The shipped version the campaign copy came from.
    pub version: Option<i32>,
    pub shipped_version: i32,
    /// The shipped blocks are newer than the copy. Nothing is updated by itself.
    pub newer_default: bool,
    pub sender_profile_id: Option<Uuid>,
    /// A sender profile is saved. Without one, mails are held.
    pub has_profile: bool,
    pub profile_version: Option<i32>,
    pub warnings: Vec<String>,
    /// The delay of each step. Change it in the Follow-ups tab.
    pub delays: Vec<StepDelay>,
}

async fn load(conn: &mut PgConnection, campaign_id: Uuid) -> ApiResult<CampaignPrompts> {
    let ctx = emails::load_ctx(conn, campaign_id).await?.ok_or_else(no_campaign)?;
    let ps = email_ai::prompt_settings(conn, ctx.campaign.sequence_id).await?.ok_or_else(ApiError::internal)?;
    let profile = crate::prompting::sender::for_sequence(conn, ps.sender_profile_id).await?;
    let blocks = ps.blocks();
    let block = |stored: &Option<String>, shipped: &str| {
        let own = stored.as_deref().filter(|t| !t.trim().is_empty());
        PromptBlock { text: own.unwrap_or(shipped).to_string(), is_default: own.is_none() }
    };
    let (core, primary, followup) = (block(&blocks.core, prompting::DEFAULT_CORE), block(&blocks.primary, prompting::DEFAULT_PRIMARY), block(&blocks.followup, prompting::DEFAULT_FOLLOWUP));
    let mut warnings = Vec::new();
    if ps.kind == "optimized" {
        if profile.is_none() {
            warnings.push("no_profile".to_string());
        }
        if !core.text.contains("# OUTPUT") || !core.text.contains("# HOLD RULE") {
            warnings.push("core_missing_sections".to_string());
        }
    }
    let has_copy = !(core.is_default && primary.is_default && followup.is_default);
    Ok(CampaignPrompts {
        newer_default: ps.kind == "optimized" && has_copy && ps.version.is_some_and(|v| v < DEFAULT_PROMPT_VERSION),
        kind: ps.kind,
        core,
        primary,
        followup,
        version: ps.version,
        shipped_version: DEFAULT_PROMPT_VERSION,
        sender_profile_id: ps.sender_profile_id,
        has_profile: profile.is_some(),
        profile_version: profile.as_ref().map(|p| p.version),
        warnings,
        delays: ctx.steps.iter().map(|s| StepDelay { step_number: s.step_number, delay_amount: s.delay_amount, delay_type: format!("{:?}", s.delay_type).to_lowercase() }).collect(),
    })
}

/// A new AI mode campaign starts with the optimized prompt and the three shipped blocks copied in.
/// An old campaign that already has a custom prompt is left alone.
pub async fn init_optimized_if_blank(conn: &mut PgConnection, sequence_id: Uuid) -> Result<bool, sqlx::Error> {
    let r = sqlx::query(
        "UPDATE sequences SET ai_prompt_kind = 'optimized', ai_prompt_core = $2, ai_prompt_primary = $3, ai_prompt_followup = $4, ai_prompt_version = $5
          WHERE id = $1 AND ai_prompt_kind = 'custom' AND ai_prompt_core IS NULL AND (ai_system_prompt IS NULL OR btrim(ai_system_prompt) = '')
            AND NOT EXISTS (SELECT 1 FROM sequence_steps s WHERE s.sequence_id = sequences.id AND s.ai_user_prompt IS NOT NULL AND btrim(s.ai_user_prompt) <> '')",
    )
    .bind(sequence_id)
    .bind(prompting::DEFAULT_CORE)
    .bind(prompting::DEFAULT_PRIMARY)
    .bind(prompting::DEFAULT_FOLLOWUP)
    .bind(DEFAULT_PROMPT_VERSION)
    .execute(conn)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// The optimized prompt of a campaign.
#[utoipa::path(get, path = "/v1/campaigns/{id}/prompts", tag = "campaigns",
    params(("id" = Uuid, Path)), responses((status = 200, body = CampaignPrompts), (status = 404, description = "No such campaign")),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. `kind` is `optimized` (the server builds the user prompt; the three blocks are the system prompt) or `custom` (today's prompts, unchanged).")]
async fn get_prompts(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<CampaignPrompts>> {
    let mut conn = st.pool.acquire().await?;
    Ok(ApiJson(load(&mut conn, id).await?))
}

const MAX_BLOCK_CHARS: usize = 16_000;

/// Change the kind and the blocks of a campaign.
#[utoipa::path(put, path = "/v1/campaigns/{id}/prompts", tag = "campaigns",
    params(("id" = Uuid, Path)), request_body(content = Object, description = "`{ kind?, core?, primary?, followup?, sender_profile_id? }`. A block that is not sent stays. `null` resets a block to the shipped default."),
    responses((status = 200, body = CampaignPrompts), (status = 404, description = "No such campaign"), (status = 422, description = "A value is not valid")),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. Edits change this campaign only. A block is up to 16000 characters. Switching an old campaign to `optimized` copies the shipped blocks in. \
        Unsent emails are marked out of date.")]
async fn put_prompts(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(patch): ApiJson<Map<String, Value>>) -> ApiResult<ApiJson<CampaignPrompts>> {
    let mut tx = st.pool.begin().await?;
    let ctx = emails::load_ctx(&mut tx, id).await?.ok_or_else(no_campaign)?;
    sqlx::query("SELECT 1 FROM sequences WHERE id = $1 FOR UPDATE").bind(ctx.campaign.sequence_id).execute(&mut *tx).await?;
    let mut ps = email_ai::prompt_settings(&mut tx, ctx.campaign.sequence_id).await?.ok_or_else(ApiError::internal)?;
    let was_custom = ps.kind == "custom";
    for (k, v) in &patch {
        let bad = |what: &str| ApiError::unprocessable("invalid_setting", format!("{k} must be {what}"));
        let block = |v: &Value| -> ApiResult<Option<String>> {
            match v {
                Value::Null => Ok(None),
                Value::String(t) if t.trim().is_empty() => Ok(None),
                Value::String(t) if t.chars().count() <= MAX_BLOCK_CHARS => Ok(Some(t.clone())),
                _ => Err(bad("text up to 16000 characters, or null")),
            }
        };
        match k.as_str() {
            "kind" => {
                ps.kind = match v.as_str() {
                    Some(m @ ("optimized" | "custom")) => m.to_string(),
                    _ => return Err(bad("optimized or custom")),
                }
            }
            "core" => ps.core = block(v)?,
            "primary" => ps.primary = block(v)?,
            "followup" => ps.followup = block(v)?,
            "sender_profile_id" => {
                ps.sender_profile_id = match v {
                    Value::Null => None,
                    Value::String(s) => {
                        let pid: Uuid = s.parse().map_err(|_| bad("an id or null"))?;
                        crate::prompting::sender::get(&mut tx, pid).await?.ok_or_else(|| ApiError::unprocessable("invalid_setting", "No sender profile with this id."))?;
                        Some(pid)
                    }
                    _ => return Err(bad("an id or null")),
                }
            }
            other => return Err(ApiError::unprocessable("unknown_field", format!("'{other}' can not be changed here."))),
        }
    }
    // Switching to optimized: copy the shipped blocks in for the blocks the caller did not send.
    if was_custom && ps.kind == "optimized" {
        for (key, stored, shipped) in [("core", &mut ps.core, prompting::DEFAULT_CORE), ("primary", &mut ps.primary, prompting::DEFAULT_PRIMARY), ("followup", &mut ps.followup, prompting::DEFAULT_FOLLOWUP)] {
            if !patch.contains_key(key) {
                *stored = Some(shipped.to_string());
            }
        }
        ps.version = Some(DEFAULT_PROMPT_VERSION);
    }
    sqlx::query(
        "UPDATE sequences SET ai_prompt_kind = $2, ai_prompt_core = $3, ai_prompt_primary = $4, ai_prompt_followup = $5, ai_prompt_version = $6, sender_profile_id = $7, updated_at = $8 WHERE id = $1",
    )
    .bind(ctx.campaign.sequence_id)
    .bind(&ps.kind)
    .bind(&ps.core)
    .bind(&ps.primary)
    .bind(&ps.followup)
    .bind(ps.version)
    .bind(ps.sender_profile_id)
    .bind(st.now())
    .execute(&mut *tx)
    .await?;
    emails::mark_stale(&mut tx, id, None).await?;
    let reply = load(&mut tx, id).await?;
    tx.commit().await?;
    Ok(ApiJson(reply))
}

#[derive(Deserialize, ToSchema)]
pub struct ResetPromptBody {
    /// `core`, `primary` or `followup`.
    pub block: String,
}

/// Reset one block to the shipped default.
#[utoipa::path(post, path = "/v1/campaigns/{id}/prompts/reset", tag = "campaigns",
    params(("id" = Uuid, Path)), request_body = ResetPromptBody,
    responses((status = 200, body = CampaignPrompts), (status = 404, description = "No such campaign"), (status = 422, description = "Unknown block")),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. The block is set to null, so the campaign uses the shipped default (and follows later versions).")]
async fn reset_prompt(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(b): ApiJson<ResetPromptBody>) -> ApiResult<ApiJson<CampaignPrompts>> {
    let col = match b.block.as_str() {
        "core" => "ai_prompt_core",
        "primary" => "ai_prompt_primary",
        "followup" => "ai_prompt_followup",
        _ => return Err(ApiError::unprocessable("invalid_block", "block must be core, primary or followup.")),
    };
    let mut tx = st.pool.begin().await?;
    let ctx = emails::load_ctx(&mut tx, id).await?.ok_or_else(no_campaign)?;
    sqlx::query(&format!("UPDATE sequences SET {col} = NULL, updated_at = $2 WHERE id = $1")).bind(ctx.campaign.sequence_id).bind(st.now()).execute(&mut *tx).await?;
    // when no copy is left, the campaign follows the shipped version
    sqlx::query("UPDATE sequences SET ai_prompt_version = $2 WHERE id = $1 AND ai_prompt_core IS NULL AND ai_prompt_primary IS NULL AND ai_prompt_followup IS NULL").bind(ctx.campaign.sequence_id).bind(DEFAULT_PROMPT_VERSION).execute(&mut *tx).await?;
    emails::mark_stale(&mut tx, id, None).await?;
    let reply = load(&mut tx, id).await?;
    tx.commit().await?;
    Ok(ApiJson(reply))
}

// ---------- preview ----------

#[derive(Deserialize, ToSchema)]
pub struct OptimizedPreviewBody {
    pub lead_id: Uuid,
    /// 1 = Primary email. 2 and later are follow-ups.
    pub step: i32,
    /// Angles of the earlier mails. Without it, the saved emails of the lead are used.
    pub previous_angles: Option<Vec<String>>,
    /// `true`: only build the prompts. No model call, no tokens, no limit. `status` is `PROMPTS_ONLY`.
    pub dry_run: Option<bool>,
}

#[derive(Serialize, ToSchema)]
pub struct HeldInfo {
    pub reason: String,
    pub failures: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub struct OptimizedPreviewReply {
    pub system: String,
    pub user: String,
    pub previous_angles: Vec<String>,
    /// The mail, when it passed. Null when it was held.
    pub status: String,
    pub subject: String,
    pub body: String,
    pub angle_used: String,
    pub used_trigger: bool,
    pub claims_check: Vec<String>,
    pub word_count: usize,
    /// The held reason, when the mail was held (not written, or failed the checks twice).
    pub held: Option<HeldInfo>,
    /// Every check on the last answer of the model.
    pub checks: Vec<CheckResult>,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub calls: usize,
}

pub fn limit(st: &AppState, who: Option<Uuid>) -> ApiResult<()> {
    let key = who.map(|u| u.to_string()).unwrap_or_else(|| "key".to_string());
    st.ai_limiter
        .check(&key, crate::prompting::limit::PER_MINUTE, std::time::Duration::from_secs(60))
        .map_err(|wait| ApiError::too_many("rate_limited", "This costs tokens. You can do it 10 times a minute. Wait a moment.", wait as i64))
}

/// Run the optimized prompt on one lead and step. Saves nothing except an `llm_calls` row (purpose `lead_email_test`).
#[utoipa::path(post, path = "/v1/campaigns/{id}/preview", tag = "campaigns",
    params(("id" = Uuid, Path)), request_body = OptimizedPreviewBody,
    responses((status = 200, body = OptimizedPreviewReply), (status = 404, description = "No such campaign, lead or step"), (status = 422, description = "generator_not_ready, wrong_mode (not the optimized prompt) or generation_failed"),
        (status = 429, description = "rate_limited: 10 a minute")),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. Costs tokens (up to 3 model calls), unless `dry_run` is true. Shows the built prompts, the result, the word count and each code check.")]
async fn preview(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, who: Option<Extension<Uuid>>, ApiJson(b): ApiJson<OptimizedPreviewBody>) -> ApiResult<ApiJson<OptimizedPreviewReply>> {
    let dry = b.dry_run == Some(true);
    if !dry {
        limit(&st, who.map(|e| e.0))?;
    }
    let now = st.now();
    let mut conn = st.pool.acquire().await?;
    let ctx = emails::load_ctx(&mut conn, id).await?.ok_or_else(no_campaign)?;
    if b.step < 1 || !ctx.steps.iter().any(|s| s.step_number == b.step) {
        return Err(ApiError::not_found("step_not_found", "The sequence has no such step."));
    }
    let lead = crate::leads::repo::get(&mut conn, b.lead_id).await?.ok_or_else(|| ApiError::not_found("lead_not_found", "No lead with this id."))?;
    let prompts = email_ai::load_prompts(&mut conn, &ctx).await?;
    let Some(opt) = prompts.optimized.clone() else {
        return Err(ApiError::unprocessable("wrong_mode", "This campaign uses a custom prompt. Switch to the optimized prompt first."));
    };
    let angles = match &b.previous_angles {
        Some(a) => a.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
        None => email_opt::previous_state(&mut conn, id, b.lead_id, b.step).await?.0,
    };
    if dry {
        let (system, user) = opt.prompts(&lead, b.step, &angles, &[]);
        return Ok(ApiJson(OptimizedPreviewReply {
            system,
            user,
            previous_angles: angles,
            status: "PROMPTS_ONLY".into(),
            subject: String::new(),
            body: String::new(),
            angle_used: String::new(),
            used_trigger: false,
            claims_check: vec![],
            word_count: 0,
            held: None,
            checks: vec![],
            tokens_in: 0,
            tokens_out: 0,
            calls: 0,
        }));
    }
    let settings = crate::generator::repo::load_settings(&mut conn).await?;
    let provider = super::email_routes::ready_provider(&mut conn).await?;
    let (cfg, _) = crate::generator::providers::load_config(&st.pool, &st.crypto, provider.id, settings.max_attempts)
        .await
        .map_err(|_| ApiError::unprocessable("generator_not_ready", "The AI model can not be used (its key can not be read)."))?;
    let client = st.llm.build(&cfg).map_err(|_| ApiError::unprocessable("generator_not_ready", "The AI model can not be used."))?;
    let (system, user) = opt.prompts(&lead, b.step, &angles, &[]);
    let result = email_opt::generate(client.as_ref(), &settings, &opt, &lead, b.step, &angles).await;
    let calls = match &result {
        Ok(g) => g.calls.clone(),
        Err(f) => f.calls.clone(),
    };
    for c in &calls {
        crate::generator::repo::record_call(&mut conn, &c.record("lead_email_test", Some(provider.id), Some(id), &provider.model), now).await?;
    }
    let (tin, tout, _) = crate::generator::pipeline::totals(&calls);
    let g = result.map_err(|f| ApiError::unprocessable("generation_failed", format!("{} ({})", f.message, f.code)))?;
    let (held, shown): (Option<HeldInfo>, Option<ModelOut>) = match g.verdict {
        Verdict::Mail(m) => (None, Some(m)),
        Verdict::Held { reason, failures } => (Some(HeldInfo { reason, failures }), g.last),
    };
    let input = CheckInput { step: b.step, previous_angles: &angles, proof: opt.proof(), extra_banned: &settings.banned_phrases };
    let results = shown.as_ref().map(|m| checks::report(m, &input)).unwrap_or_default();
    let m = shown.unwrap_or_default();
    Ok(ApiJson(OptimizedPreviewReply {
        system,
        user,
        previous_angles: angles.clone(),
        word_count: checks::words(&m.body),
        status: if held.is_some() { "HOLD".into() } else { m.status.clone() },
        subject: m.subject,
        body: m.body,
        angle_used: m.angle_used,
        used_trigger: m.used_trigger,
        claims_check: m.claims_check,
        held,
        checks: results,
        tokens_in: tin as i64,
        tokens_out: tout as i64,
        calls: calls.len(),
    }))
}

