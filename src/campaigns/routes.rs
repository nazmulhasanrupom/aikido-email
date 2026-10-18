//! `/v1/campaigns/*` (scope `campaigns`). The routes read leads and sequences themselves: this scope does
//! not need the `leads` or `followups` key.
use super::model::*;
use crate::auth::api_keys::Scope;
use crate::auth::middleware::require_scope;
use crate::error::{ApiError, ApiJson, ApiPath, ApiQuery, ApiResult};
use crate::followups::model::{Sequence, SequenceStep};
use crate::followups::routes::{CreateSequence, insert_sequence};
use crate::followups::template::{BUILTINS, StepTemplates};
use crate::followups::repo as followup_repo;
use crate::leads::fields::FieldDefs;
use crate::leads::model::TEXT_FIELDS;
use crate::leads::query::{LeadFilter, push_where};
use crate::leads::routes::{limit_of, load_settings};
use crate::sending::{queue as send_queue, rules};
use crate::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sqlx::{PgConnection, QueryBuilder};
use std::collections::{BTreeMap, HashSet};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub const MAX_IDS: usize = 10_000;

/// Routes that need a key with scope `campaigns`.
pub fn router(state: AppState) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(create_campaign, list_campaigns))
        .routes(routes!(get_campaign, patch_campaign))
        .routes(routes!(add_leads, remove_leads, list_campaign_leads))
        .routes(routes!(requeue_verification))
        .routes(routes!(launch))
        .routes(routes!(pause))
        .routes(routes!(resume))
        .routes(routes!(archive))
        .routes(routes!(stats))
        .routes(routes!(feeder_pause))
        .routes(routes!(feeder_resume))
        .routes(routes!(feeder_status))
        .merge(super::email_routes::routes())
        .merge(super::extras::routes())
        .route_layer(axum::middleware::from_fn_with_state((state, Scope::Campaigns), require_scope))
}

fn not_found() -> ApiError {
    ApiError::not_found("campaign_not_found", "No campaign with this id.")
}

fn bad_status(what: &str, status: CampaignStatus) -> ApiError {
    ApiError::conflict("invalid_status", format!("The campaign is {}. It can not {what}.", status.as_str()))
}

// ---------- reading ----------

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<Campaign>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {CAMPAIGN_COLS} FROM campaigns WHERE id = $1")).bind(id).fetch_optional(conn).await
}

async fn lock(conn: &mut PgConnection, id: Uuid) -> ApiResult<Campaign> {
    sqlx::query_as(&format!("SELECT {CAMPAIGN_COLS} FROM campaigns WHERE id = $1 FOR UPDATE"))
        .bind(id)
        .fetch_optional(conn)
        .await?
        .ok_or_else(not_found)
}

#[derive(Serialize, ToSchema, Default, Clone, Debug)]
pub struct Counts {
    /// Leads waiting for the feeder.
    pub pending: i64,
    /// Leads the feeder enrolled.
    pub enrolled: i64,
    pub skipped: i64,
    /// Why leads were skipped: `suppressed`, `unverified`, `active_elsewhere`, ...
    pub skipped_by_reason: BTreeMap<String, i64>,
    /// The enrollments of this campaign by status: `active`, `waiting_send`, `paused`, `finished`, `stopped`, `failed`.
    pub enrollments: BTreeMap<String, i64>,
}

pub async fn counts(conn: &mut PgConnection, id: Uuid) -> Result<Counts, sqlx::Error> {
    let mut c = Counts::default();
    let rows: Vec<(String, Option<String>, i64)> =
        sqlx::query_as("SELECT state::text, skip_reason::text, count(*) FROM campaign_leads WHERE campaign_id = $1 GROUP BY 1, 2").bind(id).fetch_all(&mut *conn).await?;
    for (state, reason, n) in rows {
        match state.as_str() {
            "pending" => c.pending += n,
            "enrolled" => c.enrolled += n,
            _ => {
                c.skipped += n;
                *c.skipped_by_reason.entry(reason.unwrap_or_else(|| "unknown".into())).or_default() += n;
            }
        }
    }
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT e.status::text, count(*) FROM campaign_leads cl JOIN enrollments e ON e.id = cl.enrollment_id WHERE cl.campaign_id = $1 GROUP BY 1",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    c.enrollments = rows.into_iter().collect();
    Ok(c)
}

#[derive(Serialize, ToSchema, Clone, Debug, sqlx::FromRow)]
pub struct PoolAccount {
    pub id: Uuid,
    pub email: String,
    /// Connected, not paused, not in a bounce-rate pause. Only healthy accounts get new leads.
    pub healthy: bool,
}

pub async fn pool_accounts(conn: &mut PgConnection, id: Uuid) -> Result<Vec<PoolAccount>, sqlx::Error> {
    sqlx::query_as(
        "SELECT a.id, a.email::text AS email, (a.status = 'active' AND NOT COALESCE(s.paused, false)) AS healthy
           FROM campaign_accounts ca JOIN email_accounts a ON a.id = ca.account_id LEFT JOIN account_send_state s ON s.account_id = a.id
          WHERE ca.campaign_id = $1 AND a.deleted_at IS NULL ORDER BY a.email",
    )
    .bind(id)
    .fetch_all(conn)
    .await
}

/// The most new emails the pool can send in a day: the sum of the daily caps of the healthy accounts.
async fn pool_daily_cap(conn: &mut PgConnection, accounts: &[PoolAccount]) -> Result<u64, sqlx::Error> {
    let mut total = 0u64;
    for a in accounts.iter().filter(|a| a.healthy) {
        let s = send_queue::load_settings(conn, a.id).await?;
        let day: Option<(i32,)> = sqlx::query_as("SELECT warmup_day FROM account_send_state WHERE account_id = $1").bind(a.id).fetch_optional(&mut *conn).await?;
        total += rules::base_cap(&s, day.map(|d| d.0).unwrap_or(0)) as u64;
    }
    Ok(total)
}

/// A warning (not an error): the plan asks for more emails per day than the accounts can send.
async fn capacity_warning(conn: &mut PgConnection, c: &Campaign, steps: usize, accounts: &[PoolAccount]) -> Result<Option<String>, sqlx::Error> {
    let cap = pool_daily_cap(conn, accounts).await?;
    let need = c.daily_new_leads as u64 * steps as u64;
    Ok((need > cap).then(|| {
        format!(
            "daily_new_leads ({}) x {steps} step(s) is {need} emails, and the healthy accounts of the pool can send {cap} a day. Leads will wait in the send queue.",
            c.daily_new_leads
        )
    }))
}

/// A rough token estimate for the AI slots of the campaign (not an error): every pending lead, every step with slots.
async fn token_warning(conn: &mut PgConnection, c: &Campaign, steps: &[SequenceStep]) -> Result<Option<String>, sqlx::Error> {
    let slot_steps = steps.iter().filter(|s| s.ai_slots.as_array().is_some_and(|a| !a.is_empty())).count() as i64;
    if slot_steps == 0 {
        return Ok(None);
    }
    let (leads,): (i64,) = sqlx::query_as("SELECT count(*) FROM campaign_leads WHERE campaign_id = $1 AND state = 'pending'").bind(c.id).fetch_one(&mut *conn).await?;
    let settings = crate::generator::repo::load_settings(conn).await?;
    // About 1500 input tokens per call (rules, the step text, the lead data; the provider may cache most of it) and the output limit.
    let per_call = 1500 + settings.max_output_tokens as i64;
    let estimate = leads * slot_steps * per_call;
    Ok(Some(format!(
        "AI slots: about {estimate} tokens for the {leads} waiting lead(s) over {slot_steps} step(s) with slots, at most (an estimate; replies and stops save tokens, a provider cache lowers the input). \
         Set daily_token_budget in the generator settings, or a token_budget for this campaign, to cap it."
    )))
}

#[derive(Serialize, ToSchema)]
pub struct CampaignSummary {
    #[serde(flatten)]
    pub campaign: Campaign,
    pub counts: Counts,
}

#[derive(Serialize, ToSchema)]
pub struct CampaignDetail {
    #[serde(flatten)]
    pub campaign: Campaign,
    pub counts: Counts,
    /// The sender accounts of the campaign.
    pub accounts: Vec<PoolAccount>,
    pub sequence: Sequence,
    pub steps: Vec<SequenceStep>,
    /// Variants B to E of step 1.
    pub variants: Vec<super::emails::StepVariant>,
    /// Things to look at: the feeder is stuck, an account is not healthy, the plan is bigger than the pool can send.
    pub warnings: Vec<String>,
}

pub(crate) async fn detail(st: &AppState, id: Uuid) -> ApiResult<CampaignDetail> {
    let mut conn = st.pool.acquire().await?;
    let campaign = get(&mut conn, id).await?.ok_or_else(not_found)?;
    let sequence = followup_repo::get_sequence(&mut conn, campaign.sequence_id).await?.ok_or_else(ApiError::internal)?;
    let steps = followup_repo::get_steps(&mut conn, campaign.sequence_id).await?;
    let accounts = pool_accounts(&mut conn, id).await?;
    let counts = counts(&mut conn, id).await?;
    let variants = sqlx::query_as(&format!("SELECT {} FROM step_variants WHERE sequence_id = $1 ORDER BY position, key", super::emails::VARIANT_COLS))
        .bind(campaign.sequence_id)
        .fetch_all(&mut *conn)
        .await?;
    let mut warnings = Vec::new();
    if let Some(w) = &campaign.warning {
        warnings.push(w.clone());
    }
    if matches!(campaign.status, CampaignStatus::Draft | CampaignStatus::Active | CampaignStatus::Paused) {
        for a in accounts.iter().filter(|a| !a.healthy) {
            warnings.push(format!("The account {} is not healthy (paused, disconnected or in error). It gets no new leads.", a.email));
        }
        if let Some(w) = capacity_warning(&mut conn, &campaign, steps.len(), &accounts).await? {
            warnings.push(w);
        }
        if let Some(w) = token_warning(&mut conn, &campaign, &steps).await? {
            warnings.push(w);
        }
        if let Some(rule) = crate::verify_reoon::gate::effective_rule(&mut conn, campaign.verification_rule.as_ref()).await? {
            let (blocked, unverified) = crate::verify_reoon::gate::count_pending(&mut conn, id, &rule).await?;
            if blocked > 0 {
                warnings.push(format!("{blocked} pending leads will be skipped by the verification rule."));
            }
            if unverified > 0 {
                warnings.push(format!("{unverified} pending leads are not verified."));
            }
        }
    }
    Ok(CampaignDetail { campaign, counts, accounts, sequence, steps, variants, warnings })
}

// ---------- create ----------

#[derive(Deserialize, ToSchema)]
pub struct CreateCampaign {
    pub name: String,
    /// An existing sequence that no campaign owns. Give this or `sequence`.
    pub sequence_id: Option<Uuid>,
    /// A new sequence, made with the campaign (same body as `POST /v1/followups/sequences`). Give this or `sequence_id`.
    pub sequence: Option<CreateSequence>,
    /// The sender accounts. Needed to launch.
    pub account_ids: Option<Vec<Uuid>>,
    /// 1 to 5000. Default: the lead setting `default_daily_new_leads` (50).
    pub daily_new_leads: Option<i32>,
    /// Default `least_loaded`.
    pub account_pick: Option<AccountPick>,
    /// Default `false`.
    pub stop_company_on_reply: Option<bool>,
    /// Default `false`.
    pub allow_unverified: Option<bool>,
    /// IANA name. Default: the time zone of the sending defaults.
    pub timezone: Option<String>,
}

fn check_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(ApiError::unprocessable("invalid_name", "name must be 1 to 200 characters."));
    }
    Ok(name.to_string())
}

fn check_daily(n: i32) -> ApiResult<i32> {
    if (1..=5000).contains(&n) { Ok(n) } else { Err(ApiError::unprocessable("invalid_setting", "daily_new_leads must be 1 to 5000.")) }
}

fn check_tz(tz: &str) -> ApiResult<String> {
    let tz = tz.trim();
    tz.parse::<chrono_tz::Tz>().map_err(|_| ApiError::unprocessable("invalid_timezone", format!("'{tz}' is not an IANA time zone (like Asia/Dhaka).")))?;
    Ok(tz.to_string())
}

async fn check_accounts(conn: &mut PgConnection, ids: &[Uuid]) -> ApiResult<()> {
    let found: Vec<(Uuid,)> = sqlx::query_as("SELECT id FROM email_accounts WHERE id = ANY($1) AND deleted_at IS NULL").bind(ids).fetch_all(conn).await?;
    let found: HashSet<Uuid> = found.into_iter().map(|r| r.0).collect();
    let missing: Vec<String> = ids.iter().filter(|i| !found.contains(i)).map(Uuid::to_string).collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(ApiError::unprocessable("account_not_found", format!("No such account: {}.", missing.join(", "))))
    }
}

fn dedupe(ids: &[Uuid]) -> Vec<Uuid> {
    let mut seen = HashSet::new();
    ids.iter().copied().filter(|i| seen.insert(*i)).collect()
}

/// Make a campaign (status `draft`).
#[utoipa::path(post, path = "/v1/campaigns", tag = "campaigns",
    request_body = CreateCampaign,
    responses((status = 201, body = CampaignDetail), (status = 404, description = "No such sequence"),
        (status = 409, description = "The sequence belongs to a campaign, or is archived"),
        (status = 422, description = "A value is not valid, or an account does not exist")),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. One campaign owns one sequence. A new sequence (`sequence`) starts `paused` and becomes `active` at launch. \
        A sequence you pass by `sequence_id` keeps its status until launch.")]
async fn create_campaign(State(st): State<AppState>, ApiJson(body): ApiJson<CreateCampaign>) -> ApiResult<(StatusCode, ApiJson<CampaignDetail>)> {
    let name = check_name(&body.name)?;
    let settings = load_settings(&st.pool).await?;
    let daily = check_daily(body.daily_new_leads.unwrap_or(settings.default_daily_new_leads as i32))?;
    let now = st.now();
    let mut tx = st.pool.begin().await?;
    let tz = match body.timezone.as_deref() {
        Some(t) => check_tz(t)?,
        None => send_queue::load_defaults(&mut tx).await?.timezone,
    };
    let accounts = dedupe(body.account_ids.as_deref().unwrap_or(&[]));
    check_accounts(&mut tx, &accounts).await?;

    let sequence_id = match (body.sequence_id, &body.sequence) {
        (Some(id), None) => {
            let row: Option<(Option<Uuid>, crate::followups::model::SequenceStatus)> =
                sqlx::query_as("SELECT campaign_id, status FROM sequences WHERE id = $1 FOR UPDATE").bind(id).fetch_optional(&mut *tx).await?;
            let Some((owner, status)) = row else { return Err(ApiError::not_found("sequence_not_found", "No sequence with this id.")) };
            if owner.is_some() {
                return Err(ApiError::conflict("sequence_in_campaign", "This sequence belongs to a campaign already."));
            }
            if status == crate::followups::model::SequenceStatus::Archived {
                return Err(ApiError::conflict("sequence_archived", "This sequence is archived."));
            }
            id
        }
        (None, Some(seq)) => {
            if seq.campaign_id.is_some() {
                return Err(ApiError::unprocessable("campaign_managed", "A campaign sets the campaign_id of its sequence itself."));
            }
            let id = insert_sequence(&mut tx, seq, now).await?;
            sqlx::query("UPDATE sequences SET status = 'paused' WHERE id = $1").bind(id).execute(&mut *tx).await?;
            id
        }
        _ => return Err(ApiError::unprocessable("sequence_required", "Give sequence_id (an existing sequence) or sequence (a new one), one of them.")),
    };

    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO campaigns (id, name, status, sequence_id, daily_new_leads, account_pick, stop_company_on_reply, allow_unverified, timezone, created_at, updated_at)
         VALUES ($1, $2, 'draft', $3, $4, $5, $6, $7, $8, $9, $9)",
    )
    .bind(id)
    .bind(&name)
    .bind(sequence_id)
    .bind(daily)
    .bind(body.account_pick.unwrap_or(AccountPick::LeastLoaded))
    .bind(body.stop_company_on_reply.unwrap_or(false))
    .bind(body.allow_unverified.unwrap_or(false))
    .bind(&tz)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE sequences SET campaign_id = $2, updated_at = $3 WHERE id = $1").bind(sequence_id).bind(id).bind(now).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO campaign_accounts (campaign_id, account_id) SELECT $1, a FROM unnest($2::uuid[]) AS t(a)").bind(id).bind(&accounts).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, ApiJson(detail(&st, id).await?)))
}

#[derive(Deserialize, IntoParams)]
pub struct ListCampaignsQuery {
    /// Only campaigns with this status.
    status: Option<CampaignStatus>,
    /// The `next_cursor` of the last page.
    cursor: Option<Uuid>,
    /// 1 to 500. Default 50.
    limit: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct ListCampaignsReply {
    pub campaigns: Vec<CampaignSummary>,
    pub next_cursor: Option<Uuid>,
}

/// List campaigns, newest first, with the counts of their leads and enrollments.
#[utoipa::path(get, path = "/v1/campaigns", tag = "campaigns",
    params(ListCampaignsQuery),
    responses((status = 200, body = ListCampaignsReply)),
    security(("api_key" = ["campaigns"])), description = "Scope: `campaigns`.")]
async fn list_campaigns(State(st): State<AppState>, ApiQuery(q): ApiQuery<ListCampaignsQuery>) -> ApiResult<ApiJson<ListCampaignsReply>> {
    let limit = limit_of(q.limit)?;
    let mut conn = st.pool.acquire().await?;
    let mut rows: Vec<Campaign> = sqlx::query_as(&format!(
        "SELECT {CAMPAIGN_COLS} FROM campaigns WHERE ($1::campaign_status IS NULL OR status = $1) AND ($2::uuid IS NULL OR id < $2) ORDER BY id DESC LIMIT $3"
    ))
    .bind(q.status)
    .bind(q.cursor)
    .bind(limit + 1)
    .fetch_all(&mut *conn)
    .await?;
    let next_cursor = if rows.len() as i64 > limit {
        rows.truncate(limit as usize);
        rows.last().map(|c| c.id)
    } else {
        None
    };
    let mut campaigns = Vec::with_capacity(rows.len());
    for campaign in rows {
        let counts = counts(&mut conn, campaign.id).await?;
        campaigns.push(CampaignSummary { campaign, counts });
    }
    Ok(ApiJson(ListCampaignsReply { campaigns, next_cursor }))
}

/// One campaign with its accounts, sequence, counts and warnings.
#[utoipa::path(get, path = "/v1/campaigns/{id}", tag = "campaigns",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = CampaignDetail), (status = 404, description = "No such campaign")),
    security(("api_key" = ["campaigns"])), description = "Scope: `campaigns`.")]
async fn get_campaign(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<CampaignDetail>> {
    Ok(ApiJson(detail(&st, id).await?))
}

#[derive(Deserialize, ToSchema)]
#[allow(dead_code)]
pub struct PatchCampaign {
    pub name: Option<String>,
    pub daily_new_leads: Option<i32>,
    pub account_pick: Option<AccountPick>,
    pub stop_company_on_reply: Option<bool>,
    pub allow_unverified: Option<bool>,
    pub timezone: Option<String>,
    /// Replaces the pool. Open enrollments keep their account. Only new enrollments use the new pool.
    pub account_ids: Option<Vec<Uuid>>,
}

/// Change the name, the settings or the account pool.
#[utoipa::path(patch, path = "/v1/campaigns/{id}", tag = "campaigns",
    params(("id" = Uuid, Path)), request_body = PatchCampaign,
    responses((status = 200, body = CampaignDetail), (status = 404, description = "No such campaign"),
        (status = 409, description = "The campaign is archived"), (status = 422, description = "A value is not valid")),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. Send only the fields to change. A new `daily_new_leads` counts at the next feeder tick. \
        Removing an account from the pool does not touch its open enrollments.")]
async fn patch_campaign(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(patch): ApiJson<Map<String, Value>>) -> ApiResult<ApiJson<CampaignDetail>> {
    let mut tx = st.pool.begin().await?;
    let mut c = lock(&mut tx, id).await?;
    if c.status == CampaignStatus::Archived {
        return Err(bad_status("be changed", c.status));
    }
    let mut accounts: Option<Vec<Uuid>> = None;
    let old_mode = c.content_mode.clone();
    for (k, v) in &patch {
        let bad = |what: &str| ApiError::unprocessable("invalid_setting", format!("{k} must be {what}"));
        match k.as_str() {
            "name" => c.name = check_name(v.as_str().ok_or_else(|| bad("text"))?)?,
            "daily_new_leads" => c.daily_new_leads = check_daily(v.as_i64().and_then(|n| i32::try_from(n).ok()).ok_or_else(|| bad("a whole number"))?)?,
            "account_pick" => c.account_pick = serde_json::from_value(v.clone()).map_err(|_| bad("least_loaded or round_robin"))?,
            "stop_company_on_reply" => c.stop_company_on_reply = v.as_bool().ok_or_else(|| bad("true or false"))?,
            "allow_unverified" => c.allow_unverified = v.as_bool().ok_or_else(|| bad("true or false"))?,
            "timezone" => c.timezone = check_tz(v.as_str().ok_or_else(|| bad("text"))?)?,
            "content_mode" => {
                c.content_mode = match v.as_str() {
                    Some(m @ ("template" | "ai" | "optimized")) => m.to_string(),
                    _ => return Err(bad("template, ai or optimized")),
                }
            }
            "auto_write" => c.auto_write = v.as_bool().ok_or_else(|| bad("true or false"))?,
            "footer" => {
                c.footer = match v {
                    Value::Null => None,
                    Value::String(t) if t.trim().is_empty() => None,
                    Value::String(t) if t.chars().count() <= 2000 => {
                        crate::followups::template::Template::parse(t).map_err(|e| ApiError::unprocessable("invalid_template", format!("footer: {}", e.message)))?;
                        Some(t.clone())
                    }
                    _ => return Err(bad("text up to 2000 characters, or null")),
                }
            }
            "verification_rule" => {
                c.verification_rule = match v {
                    Value::Null => None,
                    other => {
                        let rule: crate::verify_reoon::settings::SendRule = serde_json::from_value(other.clone()).map_err(|_| bad("null or { allowed: [status], allow_not_verified }"))?;
                        let rule = rule.normalized();
                        if let Some(s) = rule.locked_status() {
                            return Err(ApiError::unprocessable("status_not_allowed", format!("The status '{s}' can not be allowed. Sending to it hurts your inbox.")));
                        }
                        Some(serde_json::to_value(rule).unwrap_or_default())
                    }
                }
            }
            "account_ids" => {
                let ids: Vec<Uuid> = serde_json::from_value(v.clone()).map_err(|_| bad("a list of account ids"))?;
                let ids = dedupe(&ids);
                check_accounts(&mut tx, &ids).await?;
                accounts = Some(ids);
            }
            other => return Err(ApiError::unprocessable("unknown_field", format!("'{other}' can not be changed. The sequence is changed through /v1/followups."))),
        }
    }
    sqlx::query(
        "UPDATE campaigns SET name = $2, daily_new_leads = $3, account_pick = $4, stop_company_on_reply = $5, allow_unverified = $6, timezone = $7, updated_at = $8, content_mode = $9, auto_write = $10, footer = $11, verification_rule = $12 WHERE id = $1",
    )
    .bind(id)
    .bind(&c.name)
    .bind(c.daily_new_leads)
    .bind(c.account_pick)
    .bind(c.stop_company_on_reply)
    .bind(c.allow_unverified)
    .bind(&c.timezone)
    .bind(st.now())
    .bind(&c.content_mode)
    .bind(c.auto_write)
    .bind(&c.footer)
    .bind(&c.verification_rule)
    .execute(&mut *tx)
    .await?;
    if patch.contains_key("footer") {
        super::emails::mark_stale(&mut tx, id, None).await?;
    }
    if patch.contains_key("content_mode") && old_mode == "optimized" && c.content_mode != "optimized" {
        super::email_opt::drop_placeholder_steps(&mut tx, c.sequence_id, c.status == CampaignStatus::Draft).await?;
    }
    // The Optimised mode writes exactly 4 emails: the primary email and 3 follow-ups.
    if patch.contains_key("content_mode") && c.content_mode == "optimized" {
        super::email_opt::ensure_four_steps(&mut tx, c.sequence_id, st.now()).await?;
    }
    if let Some(ids) = accounts {
        sqlx::query("DELETE FROM campaign_accounts WHERE campaign_id = $1 AND NOT account_id = ANY($2)").bind(id).bind(&ids).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO campaign_accounts (campaign_id, account_id) SELECT $1, a FROM unnest($2::uuid[]) AS t(a) ON CONFLICT DO NOTHING").bind(id).bind(&ids).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(ApiJson(detail(&st, id).await?))
}

// ---------- leads of a campaign ----------

#[derive(Deserialize, ToSchema)]
pub struct LeadSelection {
    /// Pick leads by id (up to 10000). Give one of `lead_ids`, `list_id`, `filter`.
    pub lead_ids: Option<Vec<Uuid>>,
    /// Every lead in this list.
    pub list_id: Option<Uuid>,
    /// Every lead that matches the filter (same fields as `GET /v1/leads`). It needs at least one condition.
    pub filter: Option<LeadFilter>,
}

enum Sel {
    Ids(Vec<Uuid>),
    Filter(LeadFilter),
}

impl LeadSelection {
    fn resolve(self) -> ApiResult<Sel> {
        match (self.lead_ids, self.list_id, self.filter) {
            (Some(ids), None, None) => {
                if ids.is_empty() || ids.len() > MAX_IDS {
                    return Err(ApiError::unprocessable("invalid_selection", format!("lead_ids holds 1 to {MAX_IDS} ids.")));
                }
                Ok(Sel::Ids(ids))
            }
            (None, Some(list_id), None) => Ok(Sel::Filter(LeadFilter { list_id: Some(list_id), ..Default::default() })),
            (None, None, Some(f)) => {
                if f.is_empty() {
                    Err(ApiError::unprocessable("empty_selection", "The filter has no condition: it would match every lead."))
                } else {
                    Ok(Sel::Filter(f))
                }
            }
            _ => Err(ApiError::unprocessable("bad_selection", "Give lead_ids, list_id or filter, one of them.")),
        }
    }
}

/// Pushes `SELECT id FROM leads WHERE ...` for the selection.
fn push_select_ids<'a>(qb: &mut QueryBuilder<'a, sqlx::Postgres>, sel: &'a Sel) {
    qb.push("SELECT id FROM leads");
    match sel {
        Sel::Ids(ids) => {
            qb.push(" WHERE id = ANY(").push_bind(ids.as_slice()).push(")");
        }
        Sel::Filter(f) => push_where(qb, f),
    }
}

#[derive(Serialize, ToSchema)]
pub struct AddLeadsReply {
    /// Leads the selection matched.
    pub matched: i64,
    /// Leads put in the campaign (state `pending`).
    pub added: i64,
    /// Leads that were in the campaign already. They are left as they are.
    pub already_in_campaign: i64,
}

/// Add leads to a campaign. They wait as `pending` until the feeder enrolls them.
#[utoipa::path(post, path = "/v1/campaigns/{id}/leads", tag = "campaigns",
    params(("id" = Uuid, Path)), request_body = LeadSelection,
    responses((status = 200, body = AddLeadsReply), (status = 404, description = "No such campaign"),
        (status = 409, description = "The campaign is completed or archived"), (status = 422, description = "Bad selection")),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. Allowed while the campaign is `draft`, `active` or `paused`. Suppressed, unverified and busy leads are added too; \
        the feeder skips them later and says why (`GET /v1/campaigns/{id}/leads`). Adding 5000 leads does not send 5000 emails: the feeder enrolls `daily_new_leads` a day.")]
async fn add_leads(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(body): ApiJson<LeadSelection>) -> ApiResult<ApiJson<AddLeadsReply>> {
    let sel = body.resolve()?;
    let now = st.now();
    let mut tx = st.pool.begin().await?;
    let c = lock(&mut tx, id).await?;
    if !matches!(c.status, CampaignStatus::Draft | CampaignStatus::Active | CampaignStatus::Paused) {
        return Err(bad_status("take more leads", c.status));
    }
    let mut count = QueryBuilder::new("SELECT count(*) FROM (");
    push_select_ids(&mut count, &sel);
    count.push(") m");
    let (matched,): (i64,) = count.build_query_as().fetch_one(&mut *tx).await?;

    let mut qb = QueryBuilder::new("WITH ins AS (INSERT INTO campaign_leads (campaign_id, lead_id, added_at) SELECT ");
    qb.push_bind(id).push(", m.id, ").push_bind(now).push(" FROM (");
    push_select_ids(&mut qb, &sel);
    qb.push(" ORDER BY created_at, id) m ON CONFLICT DO NOTHING RETURNING lead_id), ev AS (INSERT INTO lead_events (lead_id, kind, ref_id, detail, at) SELECT lead_id, 'campaign_added', ");
    qb.push_bind(id).push(", '{}'::jsonb, ").push_bind(now).push(" FROM ins RETURNING 1) SELECT count(*) FROM ev");
    let (added,): (i64,) = qb.build_query_as().fetch_one(&mut *tx).await?;
    // After the first Save, a new lead gets its emails at once (template mode costs nothing).
    if added > 0 && c.emails_saved_at.is_some() && c.content_mode == "template" {
        if let Some(ctx) = super::emails::load_ctx(&mut tx, id).await? {
            super::emails::write(&mut tx, &ctx, super::emails::Overwrite::Skip, None, None, now).await?;
        }
    }
    tx.commit().await?;
    Ok(ApiJson(AddLeadsReply { matched, added, already_in_campaign: matched - added }))
}

#[derive(Serialize, ToSchema)]
pub struct RemoveLeadsReply {
    pub matched: i64,
    /// Pending leads taken out of the campaign.
    pub removed: i64,
    /// Leads that were not removed because they are already enrolled or skipped. Stop an enrollment through the enrollment.
    pub not_removed: i64,
}

/// Take pending leads out of a campaign.
#[utoipa::path(delete, path = "/v1/campaigns/{id}/leads", tag = "campaigns",
    params(("id" = Uuid, Path)), request_body = LeadSelection,
    responses((status = 200, body = RemoveLeadsReply), (status = 404, description = "No such campaign"), (status = 422, description = "Bad selection")),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. Only `pending` leads are removed. An enrolled lead must be stopped through its enrollment (`POST /v1/followups/enrollments/{id}/stop`).")]
async fn remove_leads(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(body): ApiJson<LeadSelection>) -> ApiResult<ApiJson<RemoveLeadsReply>> {
    let sel = body.resolve()?;
    let mut tx = st.pool.begin().await?;
    lock(&mut tx, id).await?;
    let mut qb = QueryBuilder::new("SELECT count(*) FROM campaign_leads WHERE campaign_id = ");
    qb.push_bind(id).push(" AND lead_id IN (");
    push_select_ids(&mut qb, &sel);
    qb.push(")");
    let (matched,): (i64,) = qb.build_query_as().fetch_one(&mut *tx).await?;
    let mut qb = QueryBuilder::new("DELETE FROM campaign_leads WHERE state = 'pending' AND campaign_id = ");
    qb.push_bind(id).push(" AND lead_id IN (");
    push_select_ids(&mut qb, &sel);
    qb.push(")");
    let removed = qb.build().execute(&mut *tx).await?.rows_affected() as i64;
    tx.commit().await?;
    Ok(ApiJson(RemoveLeadsReply { matched, removed, not_removed: matched - removed }))
}

#[derive(Deserialize, IntoParams)]
pub struct CampaignLeadsQuery {
    /// Only leads in this state.
    state: Option<CampaignLeadState>,
    /// Only skipped leads with this reason.
    skip_reason: Option<SkipReason>,
    /// The `next_cursor` of the last page: the id of the last lead.
    cursor: Option<Uuid>,
    /// 1 to 500. Default 50.
    limit: Option<i64>,
    /// `true`: only leads that lack a value one of the emails needs (a variable with no fallback).
    missing: Option<bool>,
    /// Latest Reoon status, a comma list like `safe,catch_all`. `none` means not verified yet.
    verification_status: Option<String>,
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct CampaignLeadRow {
    pub lead_id: Uuid,
    pub email: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub company_name: Option<String>,
    /// Values of the custom fields of this lead (one lead, the same everywhere).
    pub custom_fields: serde_json::Value,
    pub state: CampaignLeadState,
    pub skip_reason: Option<SkipReason>,
    pub enrollment_id: Option<Uuid>,
    /// The status of the enrollment, if the lead was enrolled.
    pub enrollment_status: Option<crate::followups::model::EnrollmentStatus>,
    pub added_at: DateTime<Utc>,
    pub enrolled_at: Option<DateTime<Utc>>,
    pub interest: Option<crate::leads::model::Interest>,
    pub contact_state: crate::leads::model::ContactState,
    /// The step the enrollment is on: the one that is queued, or goes next (1-based). Empty if the lead is not enrolled.
    pub current_step: Option<i32>,
    /// How many steps the sequence has.
    pub steps_total: i64,
    /// When the next step is due.
    pub next_due_at: Option<DateTime<Utc>>,
    /// Why the lead is not moving, in a code. `waiting_delay`, `queued`, `waiting_ai_review`, `paused`,
    /// `finished`, or the stop reason (`reply`, `bounce_hard`, `unsubscribe`, `manual`, ...). Empty when the next step is due now.
    pub wait_reason: Option<String>,
    /// The saved emails of this lead in this campaign, one per step that has one.
    #[sqlx(skip)]
    pub emails: Vec<EmailCell>,
    /// The values this lead lacks for the emails of the campaign (names). Empty when it has them all.
    #[sqlx(skip)]
    pub missing: Vec<String>,
    /// The latest Reoon status of this address. Null: not verified, or Reoon is not installed.
    #[sqlx(skip)]
    pub verification_status: Option<String>,
    #[sqlx(skip)]
    pub verification_score: Option<i32>,
    #[sqlx(skip)]
    pub verification_checked_at: Option<DateTime<Utc>>,
}

#[derive(Serialize, ToSchema, sqlx::FromRow)]
pub struct EmailCell {
    /// The saved email. Use it with `POST .../emails/actions`.
    pub id: Uuid,
    #[serde(skip)]
    pub lead_id: Uuid,
    pub step_number: i32,
    /// `none`, `writing`, `review`, `ready`, `stale`, `failed` or `sent`.
    pub status: String,
    pub variant_key: Option<String>,
    /// The first words of the subject (or of the body for a follow up with no subject).
    pub subject_preview: String,
    pub edited_by_hand: bool,
    pub error_code: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct CampaignLeadsReply {
    pub leads: Vec<CampaignLeadRow>,
    pub next_cursor: Option<Uuid>,
}

/// The leads of a campaign, in the order the feeder takes them.
#[utoipa::path(get, path = "/v1/campaigns/{id}/leads", tag = "campaigns",
    params(("id" = Uuid, Path), CampaignLeadsQuery),
    responses((status = 200, body = CampaignLeadsReply), (status = 400, description = "invalid_cursor"), (status = 404, description = "No such campaign")),
    security(("api_key" = ["campaigns"])), description = "Scope: `campaigns`. Oldest added first.")]
async fn list_campaign_leads(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiQuery(q): ApiQuery<CampaignLeadsQuery>) -> ApiResult<ApiJson<CampaignLeadsReply>> {
    let limit = limit_of(q.limit)?;
    let mut conn = st.pool.acquire().await?;
    get(&mut conn, id).await?.ok_or_else(not_found)?;
    let after: Option<(DateTime<Utc>, Uuid)> = match q.cursor {
        None => None,
        Some(lead) => Some(
            sqlx::query_as("SELECT added_at, lead_id FROM campaign_leads WHERE campaign_id = $1 AND lead_id = $2")
                .bind(id)
                .bind(lead)
                .fetch_optional(&mut *conn)
                .await?
                .ok_or_else(|| ApiError::bad_request("invalid_cursor", "The cursor lead is not in this campaign (any more). Start again without a cursor."))?,
        ),
    };
    let ctx = super::emails::load_ctx(&mut conn, id).await?.ok_or_else(not_found)?;
    let prompts = if ctx.campaign.is_ai() { Some(super::email_ai::load_prompts(&mut conn, &ctx).await?) } else { None };
    let needs = super::emails::required_names(&ctx, prompts.as_ref());
    let missing_cond = if q.missing == Some(true) { super::emails::missing_sql(&needs, &ctx.defs) } else { "true".to_string() };
    let statuses = crate::verify_reoon::auto::parse_statuses(q.verification_status.as_deref());
    let status_cond = crate::verify_reoon::auto::status_cond(7);
    let mut leads: Vec<CampaignLeadRow> = sqlx::query_as(&format!(
        "SELECT cl.lead_id, l.email, l.first_name, l.last_name, l.company_name, l.custom_fields, cl.state, cl.skip_reason, cl.enrollment_id,
                e.status AS enrollment_status, cl.added_at, cl.enrolled_at, l.interest, l.contact_state,
                CASE WHEN e.id IS NULL THEN NULL ELSE LEAST(e.next_step, GREATEST(steps.n, 1)::int) END AS current_step,
                steps.n AS steps_total,
                e.next_due_at,
                CASE
                  WHEN e.id IS NULL THEN NULL
                  WHEN e.status IN ('stopped', 'failed') THEN COALESCE(e.stop_reason, 'stopped')
                  WHEN e.status = 'finished' THEN 'finished'
                  WHEN e.status = 'paused' THEN 'paused'
                  WHEN EXISTS (SELECT 1 FROM generations g WHERE g.enrollment_id = e.id AND g.step_number = e.next_step AND g.status = 'pending_review')
                    THEN 'waiting_ai_review'
                  WHEN e.status = 'waiting_send' THEN 'queued'
                  WHEN e.next_due_at > now() THEN 'waiting_delay'
                  ELSE NULL
                END AS wait_reason
           FROM campaign_leads cl JOIN leads l ON l.id = cl.lead_id LEFT JOIN enrollments e ON e.id = cl.enrollment_id
           CROSS JOIN (SELECT count(*)::bigint AS n FROM sequence_steps WHERE sequence_id = (SELECT sequence_id FROM campaigns WHERE id = $1)) steps
          WHERE cl.campaign_id = $1 AND ($2::campaign_lead_state IS NULL OR cl.state = $2) AND ($3::campaign_skip_reason IS NULL OR cl.skip_reason = $3)
            AND ($4::timestamptz IS NULL OR (cl.added_at, cl.lead_id) > ($4, $5))
            AND ({missing_cond}) AND {status_cond}
          ORDER BY cl.added_at, cl.lead_id LIMIT $6",
    ))
    .bind(id)
    .bind(q.state)
    .bind(q.skip_reason)
    .bind(after.map(|a| a.0))
    .bind(after.map(|a| a.1))
    .bind(limit + 1)
    .bind(&statuses)
    .fetch_all(&mut *conn)
    .await?;
    let next_cursor = if leads.len() as i64 > limit {
        leads.truncate(limit as usize);
        leads.last().map(|l| l.lead_id)
    } else {
        None
    };
    let ids: Vec<Uuid> = leads.iter().map(|l| l.lead_id).collect();
    let cells: Vec<EmailCell> = sqlx::query_as(
        "SELECT id, lead_id, step_number, status, variant_key, left(COALESCE(NULLIF(subject, ''), body_text, ''), 60) AS subject_preview, edited_by_hand, error_code
           FROM lead_emails WHERE campaign_id = $1 AND lead_id = ANY($2) ORDER BY step_number",
    )
    .bind(id)
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await?;
    let mut by_lead: std::collections::HashMap<Uuid, Vec<EmailCell>> = std::collections::HashMap::new();
    for c in cells {
        by_lead.entry(c.lead_id).or_default().push(c);
    }
    let lead_rows = if needs.is_empty() { Default::default() } else { super::emails::load_leads(&mut conn, &ids).await? };
    let briefs = crate::verify_reoon::auto::briefs(&mut conn, &ids).await?;
    for l in &mut leads {
        if let Some(b) = briefs.get(&l.lead_id) {
            (l.verification_status, l.verification_score, l.verification_checked_at) = (Some(b.status.clone()), b.score, Some(b.checked_at));
        }
        l.emails = by_lead.remove(&l.lead_id).unwrap_or_default();
        if let Some(lead) = lead_rows.get(&l.lead_id) {
            l.missing = super::emails::missing_for(lead, &needs, &ctx.defs);
        }
    }
    Ok(ApiJson(CampaignLeadsReply { leads, next_cursor }))
}

// ---------- launch, pause, resume, archive ----------

/// Every variable the steps need must be a built-in name, a lead field, a custom field, or have its own fallback.
async fn missing_variables(conn: &mut PgConnection, steps: &[SequenceStep]) -> Result<Vec<String>, sqlx::Error> {
    let defs = FieldDefs::load(conn).await?;
    let known = |name: &str| BUILTINS.contains(&name) || TEXT_FIELDS.contains(&name) || defs.by_key.contains_key(name);
    let mut missing: Vec<String> = Vec::new();
    for s in steps {
        let Ok(t) = StepTemplates::parse(&s.subject, s.body_text.as_deref(), s.body_html.as_deref()) else { continue };
        for name in t.required() {
            if !known(&name) && !missing.contains(&name) {
                missing.push(name);
            }
        }
    }
    missing.sort();
    Ok(missing)
}

async fn set_sequence_status(conn: &mut PgConnection, sequence_id: Uuid, status: &str, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE sequences SET status = $2::sequence_status, updated_at = $3 WHERE id = $1").bind(sequence_id).bind(status).bind(now).execute(conn).await?;
    Ok(())
}

async fn set_status(conn: &mut PgConnection, c: &Campaign, status: CampaignStatus, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE campaigns SET status = $2, updated_at = $3, warning = NULL,
                launched_at = CASE WHEN $2 = 'active' AND launched_at IS NULL THEN $3 ELSE launched_at END
          WHERE id = $1",
    )
    .bind(c.id)
    .bind(status)
    .bind(now)
    .execute(conn)
    .await?;
    Ok(())
}

/// Check the campaign, then start it.
#[utoipa::path(post, path = "/v1/campaigns/{id}/launch", tag = "campaigns",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = CampaignDetail), (status = 404, description = "No such campaign"),
        (status = 409, description = "The campaign is not a draft"),
        (status = 422, description = "no_accounts, no_healthy_account, missing_variables (the message names them), or no steps")),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. Checks: the sequence has a step, the pool has a healthy account, and every `{{variable}}` of the steps is a built-in name, \
        a lead field, a custom field, or has its own fallback. Then the campaign and its sequence become `active`. \
        A plan bigger than the pool can send (`daily_new_leads` x steps over the sum of the daily caps) is not an error: it is in `warnings`.")]
async fn launch(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<CampaignDetail>> {
    let now = st.now();
    let mut tx = st.pool.begin().await?;
    let c = lock(&mut tx, id).await?;
    if c.status != CampaignStatus::Draft {
        return Err(bad_status("be launched", c.status));
    }
    let steps = followup_repo::get_steps(&mut tx, c.sequence_id).await?;
    if steps.is_empty() {
        return Err(ApiError::unprocessable("no_steps", "The sequence has no steps."));
    }
    let accounts = pool_accounts(&mut tx, id).await?;
    if accounts.is_empty() {
        return Err(ApiError::unprocessable("no_accounts", "The campaign has no sender account. Add account_ids with PATCH."));
    }
    if !accounts.iter().any(|a| a.healthy) {
        return Err(ApiError::unprocessable("no_healthy_account", "No account of the pool is healthy (connected and not paused)."));
    }
    let missing = missing_variables(&mut tx, &steps).await?;
    if !missing.is_empty() {
        return Err(ApiError::unprocessable(
            "missing_variables",
            format!(
                "The steps use variables that are not a built-in name, a lead field or a custom field, and have no fallback: {}. Make the custom fields, or write a fallback like {{{{name|there}}}}.",
                missing.join(", ")
            ),
        ));
    }
    // A campaign with AI slots needs a slot provider that works.
    if steps.iter().any(|s| s.ai_slots.as_array().is_some_and(|a| !a.is_empty())) {
        let settings = crate::generator::repo::load_settings(&mut tx).await?;
        let provider = match settings.slot_provider_id {
            Some(pid) => crate::generator::providers::get(&mut tx, pid).await?,
            None => None,
        };
        match provider {
            None => {
                return Err(ApiError::unprocessable(
                    "generator_not_ready",
                    "The sequence has AI slots, but no slot provider is set. Add a provider and set slot_provider_id in PATCH /v1/generator/settings.",
                ));
            }
            Some(p) if p.status == "auth_failed" => {
                return Err(ApiError::unprocessable(
                    "generator_not_ready",
                    format!("The sequence has AI slots, but the slot provider '{}' refused its API key. Replace the key first.", p.name),
                ));
            }
            Some(_) => {}
        }
    }
    set_sequence_status(&mut tx, c.sequence_id, "active", now).await?;
    set_status(&mut tx, &c, CampaignStatus::Active, now).await?;
    tx.commit().await?;
    Ok(ApiJson(detail(&st, id).await?))
}

#[derive(Serialize, ToSchema)]
pub struct RequeueReply {
    pub moved: u64,
}

/// Move leads that the Reoon rule skipped back to `pending`, when they pass the rule now.
#[utoipa::path(post, path = "/v1/campaigns/{id}/leads/requeue-verification", tag = "campaigns",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = RequeueReply), (status = 404, description = "No such campaign"), (status = 409, description = "The campaign is archived")),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. Only leads skipped with `verification_not_allowed` whose result passes the rule of the campaign (or the workspace rule) now. Nothing moves by itself.")]
async fn requeue_verification(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<RequeueReply>> {
    let mut tx = st.pool.begin().await?;
    let c = lock(&mut tx, id).await?;
    if c.status == CampaignStatus::Archived {
        return Err(bad_status("be changed", c.status));
    }
    let moved = match crate::verify_reoon::gate::effective_rule(&mut tx, c.verification_rule.as_ref()).await? {
        Some(rule) => crate::verify_reoon::gate::requeue(&mut tx, id, &rule).await?,
        None => 0,
    };
    tx.commit().await?;
    Ok(ApiJson(RequeueReply { moved }))
}

/// Pause a campaign. The feeder stops, and so does its sequence.
#[utoipa::path(post, path = "/v1/campaigns/{id}/pause", tag = "campaigns",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = CampaignDetail), (status = 404, description = "No such campaign"), (status = 409, description = "The campaign is not active")),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. The sequence is paused too, so the follow-up engine makes no new steps for the enrolled leads. Emails already in the send queue still go out.")]
async fn pause(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<CampaignDetail>> {
    let now = st.now();
    let mut tx = st.pool.begin().await?;
    let c = lock(&mut tx, id).await?;
    if c.status != CampaignStatus::Active {
        return Err(bad_status("be paused", c.status));
    }
    set_sequence_status(&mut tx, c.sequence_id, "paused", now).await?;
    set_status(&mut tx, &c, CampaignStatus::Paused, now).await?;
    tx.commit().await?;
    Ok(ApiJson(detail(&st, id).await?))
}

/// Resume a paused campaign.
#[utoipa::path(post, path = "/v1/campaigns/{id}/resume", tag = "campaigns",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = CampaignDetail), (status = 404, description = "No such campaign"), (status = 409, description = "The campaign is not paused")),
    security(("api_key" = ["campaigns"])), description = "Scope: `campaigns`. The campaign and its sequence become `active` again.")]
async fn resume(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<CampaignDetail>> {
    let now = st.now();
    let mut tx = st.pool.begin().await?;
    let c = lock(&mut tx, id).await?;
    if c.status != CampaignStatus::Paused {
        return Err(bad_status("be resumed", c.status));
    }
    set_sequence_status(&mut tx, c.sequence_id, "active", now).await?;
    set_status(&mut tx, &c, CampaignStatus::Active, now).await?;
    tx.commit().await?;
    Ok(ApiJson(detail(&st, id).await?))
}

/// Archive a paused or completed campaign.
#[utoipa::path(post, path = "/v1/campaigns/{id}/archive", tag = "campaigns",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = CampaignDetail), (status = 404, description = "No such campaign"), (status = 409, description = "Only a paused or completed campaign can be archived")),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. Pending leads become `skipped` (`campaign_archived`). The sequence is archived too (no new enrollments; open enrollments are not stopped).")]
async fn archive(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<CampaignDetail>> {
    let now = st.now();
    let mut tx = st.pool.begin().await?;
    let c = lock(&mut tx, id).await?;
    if !matches!(c.status, CampaignStatus::Paused | CampaignStatus::Completed) {
        return Err(bad_status("be archived (pause it first)", c.status));
    }
    sqlx::query("UPDATE campaign_leads SET state = 'skipped', skip_reason = 'campaign_archived' WHERE campaign_id = $1 AND state = 'pending'").bind(id).execute(&mut *tx).await?;
    set_sequence_status(&mut tx, c.sequence_id, "archived", now).await?;
    set_status(&mut tx, &c, CampaignStatus::Archived, now).await?;
    tx.commit().await?;
    Ok(ApiJson(detail(&st, id).await?))
}

// ---------- stats ----------

#[derive(Serialize, ToSchema, Default, Clone, Debug, PartialEq)]
pub struct Tally {
    /// Emails sent.
    pub sent: i64,
    /// People who replied (not counting out-of-office replies).
    pub replied: i64,
    /// Addresses that bounced for good.
    pub bounced: i64,
    pub unsubscribed: i64,
}

#[derive(Serialize, ToSchema, Clone, Debug)]
pub struct StepStats {
    pub step_number: i32,
    #[serde(flatten)]
    pub tally: Tally,
}

#[derive(Serialize, ToSchema, Clone, Debug)]
pub struct AccountStats {
    pub account_id: Uuid,
    pub email: String,
    #[serde(flatten)]
    pub tally: Tally,
}

#[derive(Serialize, ToSchema)]
pub struct StatsReply {
    #[serde(flatten)]
    pub total: Tally,
    /// People who got at least one email.
    pub leads_contacted: i64,
    /// Spam complaints.
    pub complained: i64,
    /// Out-of-office and other automatic replies.
    pub auto_replied: i64,
    /// `replied` divided by `leads_contacted` (0 when nobody was contacted).
    pub reply_rate: f64,
    pub by_step: Vec<StepStats>,
    pub by_account: Vec<AccountStats>,
    /// The first email by variant (only when the campaign has more than one variant in use).
    pub by_variant: Vec<VariantStats>,
}

#[derive(Serialize, ToSchema)]
pub struct VariantStats {
    /// `A` to `E`.
    pub variant_key: String,
    pub sent: i64,
    pub replied: i64,
    /// `replied` divided by `sent`.
    pub reply_rate: f64,
}

/// Numbers of a campaign: sent, replied, bounced, unsubscribed, the reply rate, by step and by account.
#[utoipa::path(get, path = "/v1/campaigns/{id}/stats", tag = "campaigns",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = StatsReply), (status = 404, description = "No such campaign")),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. `sent` comes from the send jobs of the campaign. Replies, bounces and unsubscribes come from the inbound emails the Reply Check linked to those jobs; \
        each person counts once per row. Per step, the number is for the email that was answered.")]
async fn stats(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<StatsReply>> {
    let mut conn = st.pool.acquire().await?;
    get(&mut conn, id).await?.ok_or_else(not_found)?;
    let sent: Vec<(Option<i32>, Uuid, i64)> =
        sqlx::query_as("SELECT step_number, account_id, count(*) FROM send_jobs WHERE campaign_id = $1 AND status = 'sent' GROUP BY 1, 2").bind(id).fetch_all(&mut *conn).await?;
    let (sent_total, contacted): (i64, i64) =
        sqlx::query_as("SELECT count(*), count(DISTINCT to_email) FROM send_jobs WHERE campaign_id = $1 AND status = 'sent'").bind(id).fetch_one(&mut *conn).await?;
    // one row per person and label, with the step and account of the email that was answered
    let people: Vec<(Option<i32>, Uuid, String, i64)> = sqlx::query_as(
        "SELECT j.step_number, m.account_id, m.label::text, count(DISTINCT lower(COALESCE(m.bounce_recipient::text, m.from_email::text)))
           FROM inbound_messages m LEFT JOIN send_jobs j ON j.id = m.matched_job_id
          WHERE m.campaign_id = $1 GROUP BY 1, 2, 3",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let totals: Vec<(String, i64)> = sqlx::query_as(
        "SELECT label::text, count(DISTINCT lower(COALESCE(bounce_recipient::text, from_email::text))) FROM inbound_messages WHERE campaign_id = $1 GROUP BY 1",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let emails: Vec<(Uuid, String)> = sqlx::query_as("SELECT id, email::text FROM email_accounts WHERE id = ANY($1)")
        .bind(sent.iter().map(|s| s.1).chain(people.iter().map(|p| p.1)).collect::<Vec<_>>())
        .fetch_all(&mut *conn)
        .await?;
    let email_of = |a: Uuid| emails.iter().find(|e| e.0 == a).map(|e| e.1.clone()).unwrap_or_default();

    fn add(t: &mut Tally, label: &str, n: i64) {
        match label {
            "reply" => t.replied += n,
            "bounce_hard" => t.bounced += n,
            "unsubscribe" => t.unsubscribed += n,
            _ => {}
        }
    }
    let label_total = |l: &str| totals.iter().find(|t| t.0 == l).map(|t| t.1).unwrap_or(0);
    let mut by_step: BTreeMap<i32, Tally> = BTreeMap::new();
    let mut by_account: BTreeMap<Uuid, Tally> = BTreeMap::new();
    for (step, account, n) in &sent {
        by_step.entry(step.unwrap_or(1)).or_default().sent += n;
        by_account.entry(*account).or_default().sent += n;
    }
    for (step, account, label, n) in &people {
        add(by_step.entry(step.unwrap_or(1)).or_default(), label, *n);
        add(by_account.entry(*account).or_default(), label, *n);
    }
    let variant_rows: Vec<(String, i64, i64)> = sqlx::query_as(
        "SELECT le.variant_key, count(*), count(sj.replied_at)
           FROM lead_emails le JOIN send_jobs sj ON sj.id = le.job_id
          WHERE le.campaign_id = $1 AND le.step_number = 1 AND le.variant_key IS NOT NULL AND sj.status = 'sent'
          GROUP BY 1 ORDER BY 1",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    let by_variant: Vec<VariantStats> = if variant_rows.len() > 1 {
        variant_rows
            .into_iter()
            .map(|(variant_key, sent, replied)| VariantStats { variant_key, sent, replied, reply_rate: if sent > 0 { (replied as f64 / sent as f64 * 10_000.0).round() / 10_000.0 } else { 0.0 } })
            .collect()
    } else {
        vec![]
    };
    let total = Tally { sent: sent_total, replied: label_total("reply"), bounced: label_total("bounce_hard"), unsubscribed: label_total("unsubscribe") };
    let reply_rate = if contacted > 0 { (total.replied as f64 / contacted as f64 * 10_000.0).round() / 10_000.0 } else { 0.0 };
    Ok(ApiJson(StatsReply {
        total,
        leads_contacted: contacted,
        complained: label_total("complaint"),
        auto_replied: label_total("auto_reply"),
        reply_rate,
        by_step: by_step.into_iter().map(|(step_number, tally)| StepStats { step_number, tally }).collect(),
        by_account: by_account.into_iter().map(|(account_id, tally)| AccountStats { account_id, email: email_of(account_id), tally }).collect(),
        by_variant,
    }))
}

// ---------- the feeder ----------

#[derive(Serialize, ToSchema)]
pub struct FeederPauseReply {
    pub paused: bool,
}

async fn set_feeder_paused(st: &AppState, paused: bool) -> ApiResult<ApiJson<FeederPauseReply>> {
    sqlx::query("UPDATE lead_defaults SET feeder_paused = $1, updated_at = $2 WHERE id = 1").bind(paused).bind(st.now()).execute(&st.pool).await?;
    tracing::info!(paused, "campaign feeder pause flag changed");
    Ok(ApiJson(FeederPauseReply { paused }))
}

/// Pause the feeder of every campaign.
#[utoipa::path(post, path = "/v1/campaigns/feeder/pause", tag = "campaigns",
    responses((status = 200, body = FeederPauseReply)),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. No new lead is enrolled by any campaign. Enrollments already made go on. The flag is in the database, so every server copy obeys it.")]
async fn feeder_pause(State(st): State<AppState>) -> ApiResult<ApiJson<FeederPauseReply>> {
    set_feeder_paused(&st, true).await
}

/// Resume the feeder.
#[utoipa::path(post, path = "/v1/campaigns/feeder/resume", tag = "campaigns",
    responses((status = 200, body = FeederPauseReply)),
    security(("api_key" = ["campaigns"])), description = "Scope: `campaigns`.")]
async fn feeder_resume(State(st): State<AppState>) -> ApiResult<ApiJson<FeederPauseReply>> {
    set_feeder_paused(&st, false).await
}

#[derive(Serialize, ToSchema)]
pub struct FeederStatus {
    pub paused: bool,
    pub last_tick_at: Option<DateTime<Utc>>,
    /// Seconds between feeder rounds (setting `feeder_tick_secs`).
    pub tick_secs: u32,
    /// `false`: this server copy does not run the feeder (`AIKIDO_LEADS_ENABLED`).
    pub feeder_enabled: bool,
    pub active_campaigns: i64,
    /// Active campaigns that are stuck: usually because no account of the pool is healthy.
    pub waiting: Vec<WaitingCampaign>,
    /// Campaigns whose plan is bigger than the pool can send in a day.
    pub capacity_warnings: Vec<WaitingCampaign>,
}

#[derive(Serialize, ToSchema)]
pub struct WaitingCampaign {
    pub campaign_id: Uuid,
    pub name: String,
    pub message: String,
}

/// The feeder: last round, stuck campaigns, capacity warnings.
#[utoipa::path(get, path = "/v1/campaigns/status", tag = "campaigns",
    responses((status = 200, body = FeederStatus)),
    security(("api_key" = ["campaigns"])), description = "Scope: `campaigns`.")]
async fn feeder_status(State(st): State<AppState>) -> ApiResult<ApiJson<FeederStatus>> {
    let mut conn = st.pool.acquire().await?;
    let (paused, last_tick_at): (bool, Option<DateTime<Utc>>) = sqlx::query_as("SELECT feeder_paused, last_tick_at FROM lead_defaults WHERE id = 1").fetch_one(&mut *conn).await?;
    let settings = load_settings(&mut *conn).await?;
    let active: Vec<Campaign> = sqlx::query_as(&format!("SELECT {CAMPAIGN_COLS} FROM campaigns WHERE status = 'active' ORDER BY created_at")).fetch_all(&mut *conn).await?;
    let mut waiting = Vec::new();
    let mut capacity_warnings = Vec::new();
    for c in &active {
        if let Some(w) = &c.warning {
            waiting.push(WaitingCampaign { campaign_id: c.id, name: c.name.clone(), message: w.clone() });
        }
        let steps = followup_repo::get_steps(&mut conn, c.sequence_id).await?.len();
        let accounts = pool_accounts(&mut conn, c.id).await?;
        if let Some(w) = capacity_warning(&mut conn, c, steps, &accounts).await? {
            capacity_warnings.push(WaitingCampaign { campaign_id: c.id, name: c.name.clone(), message: w });
        }
    }
    Ok(ApiJson(FeederStatus {
        paused,
        last_tick_at,
        tick_secs: settings.feeder_tick_secs,
        feeder_enabled: st.config.leads_enabled,
        active_campaigns: active.len() as i64,
        waiting,
        capacity_warnings,
    }))
}
