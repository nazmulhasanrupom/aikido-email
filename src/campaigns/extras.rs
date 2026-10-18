//! Helpers that make cold email easier (UI update map v2, section 5): the launch checklist (I2), duplicate (I5, B19)
//! and a test email to yourself (needed by the checklist).
use super::emails::{self, EMAIL_COLS, LeadEmail};
use super::routes::{detail, pool_accounts, CampaignDetail};
use crate::error::{ApiError, ApiJson, ApiPath, ApiResult};
use crate::followups::routes::{CreateSequence, StepInput, insert_sequence};
use crate::sending::queue::{self, NewJob};
use crate::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(checklist)).routes(routes!(duplicate)).routes(routes!(send_test))
}

fn no_campaign() -> ApiError {
    ApiError::not_found("campaign_not_found", "No campaign with this id.")
}

// ---------- the launch checklist (I2) ----------

#[derive(Serialize, ToSchema)]
pub struct ChecklistItem {
    /// `accounts`, `domains`, `leads`, `emails`, `values` or `test`.
    pub key: String,
    pub label: String,
    pub ok: bool,
    pub detail: String,
    /// Where the fix is: `settings`, `leads`, `editor`.
    pub fix: String,
}

#[derive(Serialize, ToSchema)]
pub struct Checklist {
    pub items: Vec<ChecklistItem>,
    pub ready: i64,
    pub total: i64,
}

/// The checklist of a campaign. It blocks nothing; it shows what is left.
#[utoipa::path(get, path = "/v1/campaigns/{id}/checklist", tag = "campaigns",
    params(("id" = Uuid, Path)), responses((status = 200, body = Checklist), (status = 404, description = "No such campaign")),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. Six items: accounts healthy, domains pass (no red SPF, DKIM, DMARC or MX), leads added, emails written for day 1, no missing values, test email sent.")]
async fn checklist(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<ApiJson<Checklist>> {
    let mut conn = st.pool.acquire().await?;
    let ctx = emails::load_ctx(&mut conn, id).await?.ok_or_else(no_campaign)?;
    let c = &ctx.campaign;
    let mut items: Vec<ChecklistItem> = Vec::new();
    let mut add = |key: &str, label: &str, ok: bool, detail: String, fix: &str| items.push(ChecklistItem { key: key.into(), label: label.into(), ok, detail, fix: fix.into() });

    // 1. accounts
    let accounts = pool_accounts(&mut conn, id).await?;
    let healthy = accounts.iter().filter(|a| a.healthy).count();
    add(
        "accounts",
        "Sending accounts are healthy",
        !accounts.is_empty() && healthy == accounts.len(),
        if accounts.is_empty() { "No account is chosen.".into() } else { format!("{healthy} of {} accounts are healthy.", accounts.len()) },
        "settings",
    );

    // 2. domains
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT a.email::text, a.provider::text FROM campaign_accounts ca JOIN email_accounts a ON a.id = ca.account_id WHERE ca.campaign_id = $1 AND a.deleted_at IS NULL")
        .bind(id)
        .fetch_all(&mut *conn)
        .await?;
    let mut seen: Vec<String> = Vec::new();
    let mut bad: Vec<String> = Vec::new();
    for (email, provider) in &rows {
        let domain = email.rsplit('@').next().unwrap_or_default().to_string();
        if seen.contains(&domain) {
            continue;
        }
        seen.push(domain.clone());
        let r = st.dns.check(st.mx.as_ref(), &domain, provider, None, st.now()).await;
        if r.overall == "fail" {
            bad.push(domain);
        }
    }
    add(
        "domains",
        "Sending domains pass the check",
        !rows.is_empty() && bad.is_empty(),
        if rows.is_empty() { "No account is chosen.".into() } else if bad.is_empty() { format!("{} domain(s) checked.", seen.len()) } else { format!("Fix the DNS of: {}.", bad.join(", ")) },
        "settings",
    );

    // 3. leads
    let (leads_n,): (i64,) = sqlx::query_as("SELECT count(*) FROM campaign_leads WHERE campaign_id = $1 AND state <> 'skipped'").bind(id).fetch_one(&mut *conn).await?;
    add("leads", "Leads are added", leads_n > 0, format!("{leads_n} lead(s) in the campaign."), "leads");

    // 4. emails for day 1: the first `daily_new_leads` leads that wait
    let day1: Vec<(Uuid, Option<String>)> = sqlx::query_as(
        "SELECT cl.lead_id, (SELECT le.status FROM lead_emails le WHERE le.campaign_id = cl.campaign_id AND le.lead_id = cl.lead_id AND le.step_number = 1)
           FROM campaign_leads cl WHERE cl.campaign_id = $1 AND cl.state = 'pending' ORDER BY cl.added_at, cl.lead_id LIMIT $2",
    )
    .bind(id)
    .bind(c.daily_new_leads as i64)
    .fetch_all(&mut *conn)
    .await?;
    let written = day1.iter().filter(|(_, s)| matches!(s.as_deref(), Some("ready" | "stale" | "sent"))).count();
    // a campaign whose leads are all in the sequence has nothing left to write
    let emails_ok = (!day1.is_empty() && written == day1.len()) || (day1.is_empty() && leads_n > 0);
    add(
        "emails",
        "Emails are written for day 1",
        emails_ok,
        if day1.is_empty() {
            if leads_n > 0 { "No lead waits: they are all in the sequence.".into() } else { "Add leads first.".into() }
        } else {
            format!("{written} of {} leads have their primary email{}.", day1.len(), if c.content_mode == "ai" { "" } else { ". Press Save in the Editor to write them" })
        },
        "editor",
    );
    // 5. missing values
    let prompts = if c.content_mode == "ai" { Some(super::email_ai::load_prompts(&mut conn, &ctx).await?) } else { None };
    let needs = emails::required_names(&ctx, prompts.as_ref());
    let cond = emails::missing_sql(&needs, &ctx.defs);
    let (missing_n,): (i64,) = sqlx::query_as(&format!(
        "SELECT count(*) FROM campaign_leads cl JOIN leads l ON l.id = cl.lead_id WHERE cl.campaign_id = $1 AND cl.state <> 'skipped' AND ({cond})"
    ))
    .bind(id)
    .fetch_one(&mut *conn)
    .await?;
    add("values", "No lead lacks a value the emails need", missing_n == 0, if missing_n == 0 { "All leads have their values.".into() } else { format!("{missing_n} lead(s) lack a value.") }, "leads");

    // 6. a test email
    let (tests,): (i64,) = sqlx::query_as("SELECT count(*) FROM send_jobs WHERE metadata ->> 'campaign_test' = $1 AND status = 'sent'").bind(id.to_string()).fetch_one(&mut *conn).await?;
    add("test", "A test email was sent to you", tests > 0, if tests > 0 { "A test email was sent.".into() } else { "Send one to yourself and read it.".into() }, "editor");

    let ready = items.iter().filter(|i| i.ok).count() as i64;
    let total = items.len() as i64;
    Ok(ApiJson(Checklist { items, ready, total }))
}

// ---------- duplicate (I5, B19) ----------

/// Copy a campaign: the sequence, the prompts, the variants and the settings. No leads and no emails.
#[utoipa::path(post, path = "/v1/campaigns/{id}/duplicate", tag = "campaigns",
    params(("id" = Uuid, Path)), responses((status = 201, body = CampaignDetail), (status = 404, description = "No such campaign")),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. The copy is a `draft` named \"<name> (copy)\". It keeps the account pool, the settings, the footer, the content mode, the steps, the AI prompts and the variants.")]
async fn duplicate(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>) -> ApiResult<(StatusCode, ApiJson<CampaignDetail>)> {
    let now = st.now();
    let mut tx = st.pool.begin().await?;
    let ctx = emails::load_ctx(&mut tx, id).await?.ok_or_else(no_campaign)?;
    let c = &ctx.campaign;
    let seq = crate::followups::repo::get_sequence(&mut tx, c.sequence_id).await?.ok_or_else(ApiError::internal)?;
    let steps: Vec<StepInput> = ctx
        .steps
        .iter()
        .map(|s| StepInput {
            delay_amount: Some(s.delay_amount),
            delay_type: Some(s.delay_type),
            subject: Some(s.subject.clone()),
            body_text: s.body_text.clone(),
            body_html: s.body_html.clone(),
            ai_slots: serde_json::from_value(s.ai_slots.clone()).ok(),
            ai_user_prompt: s.ai_user_prompt.clone(),
        })
        .collect();
    let body = CreateSequence {
        name: format!("{} (copy)", seq.name).chars().take(200).collect(),
        stop_on_auto_reply: Some(seq.stop_on_auto_reply),
        auto_reply_pause_days: Some(seq.auto_reply_pause_days),
        thread_followups: Some(seq.thread_followups),
        campaign_id: None,
        brief: seq.brief.clone(),
        steps,
    };
    let new_seq = insert_sequence(&mut tx, &body, now).await?;
    sqlx::query("UPDATE sequences SET status = 'paused', ai_system_prompt = $2, variant_a_active = $3 WHERE id = $1").bind(new_seq).bind(&seq.ai_system_prompt).bind(seq.variant_a_active).execute(&mut *tx).await?;
    sqlx::query(
        "UPDATE sequences n SET ai_prompt_kind = o.ai_prompt_kind, ai_prompt_core = o.ai_prompt_core, ai_prompt_primary = o.ai_prompt_primary,
                ai_prompt_followup = o.ai_prompt_followup, ai_prompt_version = o.ai_prompt_version, sender_profile_id = o.sender_profile_id
           FROM sequences o WHERE o.id = $1 AND n.id = $2",
    )
    .bind(c.sequence_id)
    .bind(new_seq)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO step_variants (id, sequence_id, key, name, subject, body_text, body_html, ai_slots, active, position)
         SELECT gen_random_uuid(), $2, key, name, subject, body_text, body_html, ai_slots, active, position FROM step_variants WHERE sequence_id = $1",
    )
    .bind(c.sequence_id)
    .bind(new_seq)
    .execute(&mut *tx)
    .await?;
    let new_id = Uuid::now_v7();
    let name: String = format!("{} (copy)", c.name).chars().take(200).collect();
    sqlx::query(
        "INSERT INTO campaigns (id, name, status, sequence_id, daily_new_leads, account_pick, stop_company_on_reply, allow_unverified, timezone, content_mode, auto_write, footer, created_at, updated_at)
         VALUES ($1, $2, 'draft', $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $12)",
    )
    .bind(new_id)
    .bind(name)
    .bind(new_seq)
    .bind(c.daily_new_leads)
    .bind(c.account_pick)
    .bind(c.stop_company_on_reply)
    .bind(c.allow_unverified)
    .bind(&c.timezone)
    .bind(&c.content_mode)
    .bind(c.auto_write)
    .bind(&c.footer)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE sequences SET campaign_id = $2, updated_at = $3 WHERE id = $1").bind(new_seq).bind(new_id).bind(now).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO campaign_accounts (campaign_id, account_id) SELECT $2, account_id FROM campaign_accounts WHERE campaign_id = $1").bind(id).bind(new_id).execute(&mut *tx).await?;
    // the generator review override and token budget travel too
    sqlx::query("INSERT INTO generator_campaigns (campaign_id, review_required, token_budget) SELECT $2, review_required, token_budget FROM generator_campaigns WHERE campaign_id = $1")
        .bind(id)
        .bind(new_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, ApiJson(detail(&st, new_id).await?)))
}

// ---------- a test email (needed by the checklist) ----------

#[derive(Deserialize, ToSchema)]
pub struct SendTestBody {
    /// Where to send it: your own address.
    pub to: String,
    /// The lead whose email is shown. Default: the first lead of the campaign.
    pub lead_id: Option<Uuid>,
    /// 1 = Primary email (default).
    pub step: Option<i32>,
}

#[derive(Serialize, ToSchema)]
pub struct SendTestReply {
    pub job_id: Uuid,
}

/// Send the email of one lead to yourself, from the first healthy account of the pool.
#[utoipa::path(post, path = "/v1/campaigns/{id}/emails/send-test", tag = "campaigns",
    params(("id" = Uuid, Path)), request_body = SendTestBody,
    responses((status = 202, body = SendTestReply), (status = 404, description = "No such campaign, lead or step"),
        (status = 422, description = "no_healthy_account, email_not_ready (AI mode, nothing written), missing_variable or an address that is not valid")),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. The job is `manual` and is not counted in the campaign numbers. The subject starts with \"[Test] \". It goes out through the normal sending engine (window, gap, cap).")]
async fn send_test(State(st): State<AppState>, ApiPath(id): ApiPath<Uuid>, ApiJson(b): ApiJson<SendTestBody>) -> ApiResult<(StatusCode, ApiJson<SendTestReply>)> {
    let mut tx = st.pool.begin().await?;
    let ctx = emails::load_ctx(&mut tx, id).await?.ok_or_else(no_campaign)?;
    let step_n = b.step.unwrap_or(1);
    let step = ctx.steps.iter().find(|s| s.step_number == step_n).ok_or_else(|| ApiError::not_found("step_not_found", "The sequence has no such step."))?.clone();
    let lead_id = match b.lead_id {
        Some(l) => l,
        None => sqlx::query_as::<_, (Uuid,)>("SELECT lead_id FROM campaign_leads WHERE campaign_id = $1 AND state <> 'skipped' ORDER BY added_at, lead_id LIMIT 1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| ApiError::unprocessable("no_leads", "Add a lead first: the test shows the email of a lead."))?
            .0,
    };
    let lead = crate::leads::repo::get(&mut tx, lead_id).await?.ok_or_else(|| ApiError::not_found("lead_not_found", "No lead with this id."))?;
    let accounts = pool_accounts(&mut tx, id).await?;
    let account = accounts.iter().find(|a| a.healthy).ok_or_else(|| ApiError::unprocessable("no_healthy_account", "No account of the pool is healthy."))?;
    let (acc_email, display) = crate::followups::repo::account_identity(&mut tx, account.id).await?.ok_or_else(ApiError::internal)?;
    let saved: Option<LeadEmail> = sqlx::query_as(&format!("SELECT {EMAIL_COLS} FROM lead_emails WHERE campaign_id = $1 AND lead_id = $2 AND step_number = $3 AND status IN ('ready', 'stale', 'review', 'sent')"))
        .bind(id)
        .bind(lead_id)
        .bind(step_n)
        .fetch_optional(&mut *tx)
        .await?;
    let (subject, text, html) = match saved {
        Some(e) => (e.subject, e.body_text, e.body_html),
        None if ctx.campaign.content_mode != "ai" => {
            let built = emails::build(&ctx, &lead, &step, "A").map_err(|m| ApiError::unprocessable("missing_variable", format!("Missing: {}", m.join(", "))))?;
            (built.subject, built.body_text, built.body_html)
        }
        None => return Err(ApiError::unprocessable("email_not_ready", "No email is written for this lead yet. Generate it first.")),
    };
    let name = crate::followups::template::sender_name_of(&display, &acc_email);
    let subject = if subject.is_empty() { format!("[Test] (reply in the same thread) step {step_n}") } else { format!("[Test] {subject}") };
    let job = NewJob {
        account_id: account.id,
        to_email: b.to.trim().to_string(),
        to_name: None,
        subject: emails::fill_sender(&subject, &name, &acc_email, false).replace(['\r', '\n'], " "),
        body_text: text.map(|t| emails::fill_sender(&t, &name, &acc_email, false)),
        body_html: html.map(|t| emails::fill_sender(&t, &name, &acc_email, true)),
        headers: None,
        in_reply_to: None,
        references: None,
        provider_thread_id: None,
        send_after: None,
        priority: Some(10),
        idempotency_key: None,
        lead_id: None,
        campaign_id: None,
        metadata: Some(json!({ "campaign_test": id.to_string() })),
        kind: Some(crate::replies::model::JobKind::Manual),
    };
    let (to_email, domain) = queue::check_new_job(&job).map_err(|e| ApiError::unprocessable(e.code, e.message))?;
    let inserted = queue::insert_job(&mut tx, &job, &to_email, &domain, None).await?.ok_or_else(ApiError::internal)?;
    tx.commit().await?;
    Ok((StatusCode::ACCEPTED, ApiJson(SendTestReply { job_id: inserted.id })))
}
