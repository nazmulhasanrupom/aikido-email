//! AI mode (UI update map v2, section 4.2): the model writes one email per lead and step from two prompts.
//!
//! - The system prompt is one per campaign (`sequences.ai_system_prompt`). The user prompt is one per step
//!   (`sequence_steps.ai_user_prompt`). Both take every lead variable, plus `{{brief}}`, `{{previous_emails}}`, `{{lead_notes}}`.
//! - The server adds the output rules at the end of the system prompt. The user does not write them.
//! - The footer is not written by the model. It is added after, like in template mode.
//! - A run (`lead_email_runs`) puts rows in `lead_emails` with status `writing`. The generator worker claims them
//!   (`FOR UPDATE SKIP LOCKED` and a lease), calls the model, checks the answer with the guard, and saves it.
//!
//! Rule from the generator: no key, no prompt text and no email text in the logs. Only ids, token counts and codes.
use super::emails::{self, Ctx, Overwrite};
use crate::followups::repo as followup_repo;
use crate::followups::template::{BUILTINS, Escape, Template, Vars, sender_name_of};
use crate::generator::guard::{self, GuardOptions};
use crate::generator::llm::{LlmClient, LlmRequest};
use crate::generator::parse::json_part;
use crate::generator::pipeline::{CallInfo, FailKind, GenFailure, failure_of};
use crate::generator::prompt::repair_prompt;
use crate::generator::repo as gen_repo;
use crate::generator::settings::{GeneratorSettings, SpamCheck};
use crate::generator::slots::AiSlot;
use crate::leads::fields::FieldDefs;
use crate::leads::model::{Lead, TEXT_FIELDS};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio_util::sync::CancellationToken;
use utoipa::ToSchema;
use uuid::Uuid;

/// Names that only a prompt can use.
pub const PROMPT_VARS: [&str; 3] = ["brief", "previous_emails", "lead_notes"];
pub const EMAIL_MAX_WORDS: u32 = 200;
const SUBJECT_MAX_WORDS: u32 = 14;
const LEASE_MINS: i64 = 10;
const DISCOVER_BATCH: i64 = 500;
const MAX_PER_TICK: usize = 2000;

pub const DEFAULT_SYSTEM: &str = "You write short, personal cold emails for {{sender_name}}. Write like a person, not a company. Use only facts that are in the prompt.";

/// Names a prompt uses that no lead field, custom field or fallback covers.
pub fn unknown_names(text: &str, defs: &FieldDefs) -> Vec<String> {
    let Ok(t) = Template::parse(text) else { return vec![] };
    t.required()
        .into_iter()
        .filter(|n| !(BUILTINS.contains(n) || PROMPT_VARS.contains(n) || TEXT_FIELDS.contains(n) || crate::prompting::optimized::is_known(n) || defs.by_key.contains_key(*n)))
        .map(String::from)
        .collect()
}

/// The prompts of a campaign, read once.
pub struct Prompts {
    pub system: Option<String>,
    pub users: HashMap<i32, String>,
    pub brief: Option<String>,
    pub thread: bool,
    pub sender_name: String,
    pub sender_email: String,
    /// `Some` in the Optimised mode. Then `system` and `users` are not used.
    pub optimized: Option<super::email_opt::Optimized>,
}

pub async fn load_prompts(conn: &mut PgConnection, ctx: &Ctx) -> Result<Prompts, sqlx::Error> {
    let seq = followup_repo::get_sequence(conn, ctx.campaign.sequence_id).await?;
    let first: Option<(Uuid,)> = sqlx::query_as("SELECT account_id FROM campaign_accounts WHERE campaign_id = $1 ORDER BY account_id LIMIT 1").bind(ctx.campaign.id).fetch_optional(&mut *conn).await?;
    let (mut sender_name, mut sender_email) = (String::new(), String::new());
    if let Some((a,)) = first {
        if let Some((email, display)) = followup_repo::account_identity(conn, a).await? {
            sender_name = sender_name_of(&display, &email);
            sender_email = email;
        }
    }
    let optimized = if ctx.campaign.content_mode == "optimized" {
        let profile = crate::prompting::sender::current(conn).await?;
        let own = seq.as_ref().and_then(|s| s.opt_system_prompt.clone()).filter(|p| !p.trim().is_empty());
        let user = seq.as_ref().and_then(|s| s.opt_user_prompt.clone()).filter(|p| !p.trim().is_empty());
        Some(super::email_opt::Optimized { system: own, user, profile, fallback_name: sender_name.clone() })
    } else {
        None
    };
    Ok(Prompts {
        optimized,
        system: seq.as_ref().and_then(|s| s.ai_system_prompt.clone()).filter(|p| !p.trim().is_empty()),
        users: ctx.steps.iter().filter_map(|s| s.ai_user_prompt.clone().filter(|p| !p.trim().is_empty()).map(|p| (s.step_number, p))).collect(),
        brief: seq.as_ref().and_then(|s| s.brief.clone()),
        thread: seq.map(|s| s.thread_followups).unwrap_or(true),
        sender_name,
        sender_email,
    })
}

/// What changes per lead and is not a lead field.
#[derive(Default)]
pub struct Extra {
    pub previous_emails: String,
    pub lead_notes: String,
}

fn vars_for(ctx: &Ctx, p: &Prompts, lead: &Lead, extra: &Extra) -> Result<Vars, String> {
    let mut v = emails::lead_vars(ctx, lead)?;
    v.insert("sender_name".into(), p.sender_name.clone());
    v.insert("sender_email".into(), p.sender_email.clone());
    v.insert("brief".into(), p.brief.clone().unwrap_or_default());
    v.insert("previous_emails".into(), if extra.previous_emails.trim().is_empty() { "(none: this is the first email)".into() } else { extra.previous_emails.clone() });
    v.insert("lead_notes".into(), if extra.lead_notes.trim().is_empty() { "(no notes)".into() } else { extra.lead_notes.clone() });
    Ok(v)
}

fn fill(src: &str, vars: &Vars) -> Result<String, Vec<String>> {
    let t = Template::parse(src).map_err(|e| vec![format!("invalid_prompt: {}", e.message)])?;
    t.render(vars, Escape::Text)
}

/// Does the email of this step have a subject? A threaded follow up goes out as a reply, so it has none.
pub fn wants_subject(step: i32, thread: bool) -> bool {
    step == 1 || !thread
}

fn rules(step: i32, thread: bool) -> String {
    let subject = if wants_subject(step, thread) {
        format!("\"subject\" is a short subject line, up to {SUBJECT_MAX_WORDS} words.")
    } else {
        "This email is a reply in the same thread: set \"subject\" to an empty string.".to_string()
    };
    format!(
        "\n\nRules for your answer (fixed, they come from the server):\n\
         - Answer with one JSON object only: {{\"subject\": \"...\", \"body\": \"...\"}}.\n\
         - {subject}\n\
         - \"body\" is plain text, up to {EMAIL_MAX_WORDS} words. No markdown, no HTML.\n\
         - Write no placeholder such as [Name] or {{{{x}}}}. Write the real words.\n\
         - Write no link, no email address and no phone number.\n\
         - Do not write a signature or an opt-out line: they are added for you.\n\
         <email_request>{{\"step\":{step}}}</email_request>"
    )
}

/// The filled system and user prompt for one lead and step. `Err` lists the missing names.
pub fn build_prompts(ctx: &Ctx, p: &Prompts, lead: &Lead, step: i32, extra: &Extra) -> Result<(String, String), Vec<String>> {
    if let Some(o) = &p.optimized {
        return Ok(o.prompts(lead));
    }
    let vars = vars_for(ctx, p, lead, extra).map_err(|m| vec![m])?;
    let user_src = p.users.get(&step).ok_or_else(|| vec!["missing_prompt".to_string()])?;
    let system = fill(p.system.as_deref().unwrap_or(DEFAULT_SYSTEM), &vars)?;
    let user = fill(user_src, &vars)?;
    Ok((format!("{system}{}", rules(step, p.thread)), user))
}

/// Hash of the filled prompts (without the parts that change when an earlier email changes). It tells when a lead or a prompt changed.
pub fn prompt_hash(ctx: &Ctx, p: &Prompts, lead: &Lead, step: i32) -> Option<String> {
    if let Some(o) = &p.optimized {
        return Some(o.hash(lead, step));
    }
    let (system, user) = build_prompts(ctx, p, lead, step, &Extra::default()).ok()?;
    let mut h = Sha256::new();
    for part in [system.as_str(), user.as_str(), ctx.campaign.footer.as_deref().unwrap_or("")] {
        h.update(part.as_bytes());
        h.update([0]);
    }
    Some(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

// ---------- the model call ----------

pub struct EmailOut {
    pub subject: String,
    pub body: String,
    pub warnings: Vec<String>,
    pub calls: Vec<CallInfo>,
}

fn parse_email(text: &str, want_subject: bool) -> Result<(String, String), String> {
    let json = json_part(text).ok_or("The answer has no JSON object.")?;
    let v: Value = serde_json::from_str(json).map_err(|_| "The answer is not valid JSON.".to_string())?;
    let body = v.get("body").and_then(|b| b.as_str()).map(str::trim).filter(|b| !b.is_empty()).ok_or("The JSON has no \"body\" text.")?;
    let subject = match v.get("subject") {
        Some(Value::String(s)) => s.trim().to_string(),
        None if !want_subject => String::new(),
        _ => return Err("The JSON has no \"subject\" text.".into()),
    };
    Ok((if want_subject { subject } else { String::new() }, body.to_string()))
}

/// One email. Calls the model once, and once more if the first answer is not valid.
pub async fn generate_email(client: &dyn LlmClient, s: &GeneratorSettings, system: &str, user: &str, want_subject: bool) -> Result<EmailOut, GenFailure> {
    let mut slots = vec![AiSlot { key: "body".into(), instruction: String::new(), max_words: EMAIL_MAX_WORDS, allow_links: false, fallback: None }];
    if want_subject {
        slots.push(AiSlot { key: "subject".into(), instruction: String::new(), max_words: SUBJECT_MAX_WORDS, allow_links: false, fallback: None });
    }
    let opts = GuardOptions { slots: &slots, banned_phrases: &s.banned_phrases, spam_words: &s.spam_words, spam_fail: s.spam_check == SpamCheck::Fail, language: s.language.as_deref() };
    let mut calls: Vec<CallInfo> = Vec::new();
    let mut reason = String::new();
    let mut code = "bad_output";
    for attempt in 0..2 {
        let req = LlmRequest {
            system: system.to_string(),
            user: if attempt == 0 { user.to_string() } else { repair_prompt(user, &reason) },
            max_output_tokens: s.max_output_tokens.max(600),
            json: true,
            temperature: s.temperature,
        };
        let started = Instant::now();
        let resp = match client.complete(&req).await {
            Ok(r) => r,
            Err(e) => {
                calls.push(CallInfo { duration_ms: started.elapsed().as_millis() as u32, ok: false, error_code: Some(e.code()), ..Default::default() });
                return Err(failure_of(&e, calls));
            }
        };
        let mut info = CallInfo {
            model: resp.model.clone(),
            input_tokens: resp.input_tokens,
            output_tokens: resp.output_tokens,
            cached_tokens: resp.cached_tokens,
            duration_ms: started.elapsed().as_millis() as u32,
            ok: true,
            error_code: None,
        };
        match parse_email(&resp.text, want_subject) {
            Err(msg) => {
                info.ok = false;
                info.error_code = Some("bad_output".into());
                calls.push(info);
                reason = msg;
                code = "bad_output";
            }
            Ok((subject, body)) => {
                let mut values = std::collections::BTreeMap::new();
                values.insert("body".to_string(), body.clone());
                if want_subject {
                    values.insert("subject".to_string(), subject.clone());
                }
                let report = guard::check(&values, &opts);
                if report.ok() {
                    calls.push(info);
                    return Ok(EmailOut { subject, body, warnings: report.warnings, calls });
                }
                info.ok = false;
                info.error_code = Some("guard_failed".into());
                calls.push(info);
                reason = report.reason();
                code = "guard_failed";
            }
        }
    }
    Err(GenFailure { kind: FailKind::Permanent, code: code.into(), message: reason, calls })
}

// ---------- which emails a run writes ----------

#[derive(Deserialize, Serialize, ToSchema, Default, Clone, Debug)]
pub struct LeadScope {
    /// The first N leads (in the order they were added) that still need one of the steps.
    pub count: Option<i64>,
    /// These leads.
    pub lead_ids: Option<Vec<Uuid>>,
    /// Leads whose email has this `status` (a missing email counts as `none`). `step` limits it to one step.
    pub filter: Option<ScopeFilter>,
}

#[derive(Deserialize, Serialize, ToSchema, Default, Clone, Debug)]
pub struct ScopeFilter {
    pub step: Option<i32>,
    pub status: Option<String>,
}

#[derive(Deserialize, ToSchema, Default)]
pub struct GenerateBody {
    /// Step numbers (1 = Primary email). Default: all steps.
    pub steps: Option<Vec<i32>>,
    /// Default: all leads of the campaign.
    pub leads: Option<LeadScope>,
    /// `skip` (default), `keep_edited` or `all`.
    pub overwrite: Option<Overwrite>,
}

/// The (lead, step) pairs a run would write.
pub async fn targets(conn: &mut PgConnection, ctx: &Ctx, body: &GenerateBody) -> Result<Vec<(Uuid, i32)>, sqlx::Error> {
    let overwrite = body.overwrite.unwrap_or(Overwrite::Skip);
    let steps: Vec<i32> = match &body.steps {
        Some(s) if !s.is_empty() => ctx.steps.iter().map(|x| x.step_number).filter(|n| s.contains(n)).collect(),
        _ => ctx.steps.iter().map(|x| x.step_number).collect(),
    };
    let leads = emails::eligible(conn, ctx.campaign.id).await?;
    let have = emails::existing(conn, ctx.campaign.id).await?;
    let scope = body.leads.clone().unwrap_or_default();
    let mut out: Vec<(Uuid, i32)> = Vec::new();
    let mut taken = 0i64;
    for l in &leads {
        if scope.lead_ids.as_ref().is_some_and(|ids| !ids.contains(&l.lead_id)) {
            continue;
        }
        let status_of = |step: i32| have.get(&(l.lead_id, step)).map(|h| h.0.as_str()).unwrap_or("none").to_string();
        if let Some(f) = &scope.filter {
            if let Some(want) = &f.status {
                let hit = match f.step {
                    Some(s) => status_of(s) == *want,
                    None => steps.iter().any(|s| status_of(*s) == *want),
                };
                if !hit {
                    continue;
                }
            }
        }
        let mine: Vec<(Uuid, i32)> = steps
            .iter()
            .copied()
            .filter(|s| *s >= l.first_open_step && emails::rewrite(have.get(&(l.lead_id, *s)), overwrite))
            .map(|s| (l.lead_id, s))
            .collect();
        if mine.is_empty() {
            continue;
        }
        if scope.count.is_some_and(|c| taken >= c) {
            break;
        }
        taken += 1;
        out.extend(mine);
    }
    Ok(out)
}

/// Put the pairs in the queue (status `writing`). The old text stays until a new one is saved.
pub async fn enqueue(conn: &mut PgConnection, campaign_id: Uuid, pairs: &[(Uuid, i32)], run_id: Option<Uuid>, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    for chunk in pairs.chunks(2000) {
        let leads: Vec<Uuid> = chunk.iter().map(|p| p.0).collect();
        let steps: Vec<i32> = chunk.iter().map(|p| p.1).collect();
        sqlx::query(
            "INSERT INTO lead_emails (id, campaign_id, lead_id, step_number, subject, source, status, run_id, created_at, updated_at)
             SELECT gen_random_uuid(), $1, t.lead, t.step, '', 'ai', 'writing', $4, $5, $5 FROM unnest($2::uuid[], $3::int[]) AS t(lead, step)
             ON CONFLICT (campaign_id, lead_id, step_number) DO UPDATE SET status = 'writing', source = 'ai', run_id = EXCLUDED.run_id, attempts = 0,
                claim_id = NULL, claimed_at = NULL, retry_at = NULL, error_code = NULL, updated_at = EXCLUDED.updated_at
             WHERE lead_emails.status <> 'sent'",
        )
        .bind(campaign_id)
        .bind(&leads)
        .bind(&steps)
        .bind(run_id)
        .bind(now)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

// ---------- the worker ----------

#[derive(Debug, Default, PartialEq)]
pub struct EmailTick {
    /// Rows the auto-write put in the queue.
    pub discovered: usize,
    pub claimed: usize,
    pub written: usize,
    pub failed: usize,
    pub retried: usize,
    pub auth_failed: bool,
    pub daily_budget_reached: bool,
}

#[derive(sqlx::FromRow)]
struct Claimed {
    id: Uuid,
    campaign_id: Uuid,
    lead_id: Uuid,
    step_number: i32,
    run_id: Option<Uuid>,
    attempts: i32,
    claim_id: Uuid,
}

/// Auto-write (decision D5): a row for the next step of each open enrollment that is due soon and has no email.
async fn discover(pool: &PgPool, now: DateTime<Utc>, lookahead_hours: u32) -> Result<usize, sqlx::Error> {
    let done = sqlx::query(
        "INSERT INTO lead_emails (id, campaign_id, lead_id, step_number, subject, source, status, created_at, updated_at)
         SELECT gen_random_uuid(), c.id, e.lead_id, e.next_step, '', 'ai', 'writing', $1, $1
           FROM enrollments e
           JOIN sequences s ON s.id = e.sequence_id AND s.status = 'active'
           JOIN campaigns c ON c.sequence_id = s.id AND c.content_mode IN ('ai', 'optimized') AND c.auto_write AND c.status = 'active'
          WHERE e.status = 'active' AND e.lead_id IS NOT NULL AND e.next_due_at <= $1 + make_interval(hours => $2)
            AND NOT EXISTS (SELECT 1 FROM lead_emails le WHERE le.campaign_id = c.id AND le.lead_id = e.lead_id AND le.step_number = e.next_step)
          ORDER BY e.next_due_at, e.id LIMIT $3
         ON CONFLICT (campaign_id, lead_id, step_number) DO NOTHING",
    )
    .bind(now)
    .bind(lookahead_hours as i32)
    .bind(DISCOVER_BATCH)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() as usize)
}

async fn claim(pool: &PgPool, now: DateTime<Utc>, n: i64) -> Result<Vec<Claimed>, sqlx::Error> {
    sqlx::query_as(
        "WITH picked AS (
            SELECT le.id FROM lead_emails le
             WHERE le.status = 'writing' AND (le.claim_id IS NULL OR le.claimed_at < $1 - make_interval(mins => $4))
               AND (le.retry_at IS NULL OR le.retry_at <= $1)
               AND (le.run_id IS NULL OR EXISTS (SELECT 1 FROM lead_email_runs r WHERE r.id = le.run_id AND r.status IN ('queued', 'running')))
               -- the primary email is written before the follow ups of the same lead and run
               AND NOT EXISTS (SELECT 1 FROM lead_emails p WHERE p.campaign_id = le.campaign_id AND p.lead_id = le.lead_id
                                  AND p.step_number < le.step_number AND p.status = 'writing' AND p.run_id IS NOT DISTINCT FROM le.run_id)
               AND NOT EXISTS (
                    SELECT 1 FROM generator_campaigns gc
                     WHERE gc.campaign_id = le.campaign_id AND gc.token_budget IS NOT NULL
                       AND (SELECT COALESCE(SUM(input_tokens + output_tokens), 0) FROM llm_calls WHERE campaign_id = le.campaign_id) >= gc.token_budget)
             ORDER BY le.step_number, le.created_at, le.id
             LIMIT $2
             FOR UPDATE OF le SKIP LOCKED)
         UPDATE lead_emails le SET claim_id = $3, claimed_at = $1, attempts = le.attempts + 1, updated_at = $1
           FROM picked WHERE le.id = picked.id
         RETURNING le.id, le.campaign_id, le.lead_id, le.step_number, le.run_id, le.attempts, le.claim_id",
    )
    .bind(now)
    .bind(n)
    .bind(Uuid::now_v7())
    .bind(LEASE_MINS as i32)
    .fetch_all(pool)
    .await
}

enum Done {
    Written,
    Failed,
    Retry,
    AuthFailed,
    Lost,
}

struct Outcome {
    /// Optimised mode: the other emails of the set (step, subject, body, hash). They are saved with the claimed one.
    others: Vec<(i32, String, String, Option<String>)>,
    status: &'static str,
    out: Option<(String, String)>,
    hash: Option<String>,
    error_code: Option<String>,
    retry_at: Option<DateTime<Utc>>,
    calls: Vec<CallInfo>,
}

impl Outcome {
    fn failed(code: impl Into<String>) -> Self {
        Self { others: vec![], status: "failed", out: None, hash: None, error_code: Some(code.into()), retry_at: None, calls: vec![] }
    }
}

/// One tick of AI mode: auto-write, then write the claimed rows. Called by the generator worker, which owns the provider.
pub async fn tick(
    state: &crate::state::AppState,
    client: &Arc<dyn LlmClient>,
    provider_id: Uuid,
    settings: &GeneratorSettings,
    width: usize,
    cancel: &CancellationToken,
) -> Result<EmailTick, sqlx::Error> {
    let pool = &state.pool;
    let now = state.now();
    let mut report = EmailTick { discovered: discover(pool, now, settings.lookahead_hours).await?, ..Default::default() };
    let mut total = 0usize;
    while total < MAX_PER_TICK && !cancel.is_cancelled() {
        let mut conn = pool.acquire().await?;
        if settings.daily_token_budget > 0 && gen_repo::tokens_today(&mut conn, now).await? as u64 >= settings.daily_token_budget {
            report.daily_budget_reached = true;
            break;
        }
        drop(conn);
        let rows = claim(pool, now, width as i64).await?;
        if rows.is_empty() {
            break;
        }
        let runs: Vec<Uuid> = rows.iter().filter_map(|r| r.run_id).collect();
        if !runs.is_empty() {
            sqlx::query("UPDATE lead_email_runs SET status = 'running', started_at = COALESCE(started_at, $2) WHERE id = ANY($1) AND status = 'queued'").bind(&runs).bind(now).execute(pool).await?;
        }
        total += rows.len();
        report.claimed += rows.len();
        let results = futures::future::join_all(rows.into_iter().map(|row| process(state, client.as_ref(), provider_id, settings, row))).await;
        let mut stop = false;
        for r in results {
            match r {
                Ok(Done::Written) => report.written += 1,
                Ok(Done::Failed) => report.failed += 1,
                Ok(Done::Retry) => report.retried += 1,
                Ok(Done::AuthFailed) => {
                    report.auth_failed = true;
                    stop = true;
                }
                Ok(Done::Lost) => {}
                Err(e) => tracing::error!(error = %e, "email failed to save"),
            }
        }
        if stop {
            break;
        }
    }
    // A run is done when none of its emails waits any more.
    sqlx::query(
        "UPDATE lead_email_runs r SET status = 'done', finished_at = $1
          WHERE r.status IN ('queued', 'running') AND r.kind = 'ai' AND NOT EXISTS (SELECT 1 FROM lead_emails le WHERE le.run_id = r.id AND le.status = 'writing')",
    )
    .bind(now)
    .execute(pool)
    .await?;
    Ok(report)
}

async fn previous_emails(conn: &mut PgConnection, campaign_id: Uuid, lead_id: Uuid, step: i32) -> Result<String, sqlx::Error> {
    let rows: Vec<(i32, String, Option<String>)> = sqlx::query_as(
        "SELECT step_number, subject, body_text FROM lead_emails
          WHERE campaign_id = $1 AND lead_id = $2 AND step_number < $3 AND status IN ('ready', 'review', 'stale', 'sent') AND body_text IS NOT NULL
          ORDER BY step_number",
    )
    .bind(campaign_id)
    .bind(lead_id)
    .bind(step)
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(n, subject, body)| {
            let name = if n == 1 { "Primary email".to_string() } else { format!("Follow up {}", n - 1) };
            let subject = if subject.is_empty() { String::new() } else { format!("Subject: {subject}\n") };
            format!("{name}:\n{subject}{}", body.unwrap_or_default().chars().take(1200).collect::<String>())
        })
        .collect::<Vec<_>>()
        .join("\n\n"))
}

async fn lead_notes(conn: &mut PgConnection, lead: &Lead) -> Result<String, sqlx::Error> {
    let mut notes: Vec<String> = Vec::new();
    if let Some(n) = lead.notes.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        notes.push(n.chars().take(500).collect());
    }
    let events: Vec<(Option<String>,)> = sqlx::query_as("SELECT detail ->> 'text' FROM lead_events WHERE lead_id = $1 AND kind = 'note' ORDER BY at DESC LIMIT 5").bind(lead.id).fetch_all(conn).await?;
    for (t,) in events {
        if notes.len() >= 5 {
            break;
        }
        if let Some(t) = t.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) {
            notes.push(t.chars().take(500).collect());
        }
    }
    Ok(notes.join("\n"))
}

/// The footer of the campaign filled with the lead values. The sender tokens stay: the account is chosen later.
fn footer_text(ctx: &Ctx, lead: &Lead) -> Option<String> {
    let f = ctx.campaign.footer.as_deref().filter(|f| !f.trim().is_empty())?;
    let vars = emails::lead_vars(ctx, lead).ok()?;
    Template::parse(f).ok()?.render(&vars, Escape::Text).ok()
}

async fn process(state: &crate::state::AppState, client: &dyn LlmClient, provider_id: Uuid, settings: &GeneratorSettings, row: Claimed) -> Result<Done, sqlx::Error> {
    let now = state.now();
    let mut conn = state.pool.acquire().await?;
    let outcome = 'o: {
        if row.attempts > settings.max_attempts as i32 {
            break 'o Outcome::failed("attempts_exceeded");
        }
        let Some(ctx) = emails::load_ctx(&mut conn, row.campaign_id).await? else { break 'o Outcome::failed("no_campaign") };
        let Some(lead) = crate::leads::repo::get(&mut conn, row.lead_id).await? else { break 'o Outcome::failed("no_lead") };
        let prompts = load_prompts(&mut conn, &ctx).await?;
        if let Some(opt) = &prompts.optimized {
            let review = match gen_repo::campaign_overrides(&mut conn, row.campaign_id).await? {
                (Some(r), _) => r,
                _ => settings.review_required,
            };
            drop(conn);
            let step = row.step_number;
            if !(1..=super::email_opt::EMAILS as i32).contains(&step) {
                break 'o Outcome::failed("step_out_of_range");
            }
            break 'o match super::email_opt::generate(client, settings, opt, &lead).await {
                Ok(g) => {
                    let status = if review { "review" } else { "ready" };
                    let mine = &g.mails[step as usize - 1];
                    let others = g.mails.iter().enumerate().filter(|(i, _)| *i as i32 + 1 != step).map(|(i, m)| (i as i32 + 1, m.subject.clone(), m.body.clone(), Some(opt.hash(&lead, i as i32 + 1)))).collect();
                    Outcome { others, status, out: Some((mine.subject.clone(), mine.body.clone())), hash: Some(opt.hash(&lead, step)), error_code: None, retry_at: None, calls: g.calls }
                }
                Err(f) => failure_outcome(f, row.attempts, settings, now),
            };
        }
        let extra = Extra { previous_emails: previous_emails(&mut conn, row.campaign_id, row.lead_id, row.step_number).await?, lead_notes: lead_notes(&mut conn, &lead).await? };
        let (system, user) = match build_prompts(&ctx, &prompts, &lead, row.step_number, &extra) {
            Ok(p) => p,
            Err(missing) if missing == ["missing_prompt"] => break 'o Outcome::failed("missing_prompt"),
            Err(missing) => break 'o Outcome::failed(format!("missing_variable:{}", missing.join(","))),
        };
        let review = match gen_repo::campaign_overrides(&mut conn, row.campaign_id).await? {
            (Some(r), _) => r,
            _ => settings.review_required,
        };
        let hash = prompt_hash(&ctx, &prompts, &lead, row.step_number);
        let footer = footer_text(&ctx, &lead);
        drop(conn);
        // The network call. No transaction is open here.
        match generate_email(client, settings, &system, &user, wants_subject(row.step_number, prompts.thread)).await {
            Ok(g) => {
                let mut body = g.body;
                if let Some(f) = footer {
                    body.push_str("\n\n");
                    body.push_str(&f);
                }
                Outcome { others: vec![], status: if review { "review" } else { "ready" }, out: Some((g.subject, body)), hash, error_code: None, retry_at: None, calls: g.calls }
            }
            Err(f) => failure_outcome(f, row.attempts, settings, now),
        }
    };
    save(state, provider_id, &row, outcome, now).await
}

fn failure_outcome(f: GenFailure, attempts: i32, settings: &GeneratorSettings, now: DateTime<Utc>) -> Outcome {
    match f.kind {
        FailKind::Auth => Outcome { status: "writing", error_code: Some(f.code), calls: f.calls, ..Outcome::failed("") },
        FailKind::Transient if attempts < settings.max_attempts as i32 => Outcome {
            status: "writing",
            error_code: Some(f.code),
            retry_at: Some(now + Duration::seconds((30 * attempts as i64).min(600))),
            calls: f.calls,
            ..Outcome::failed("")
        },
        _ => Outcome { error_code: Some(f.code), calls: f.calls, ..Outcome::failed("") },
    }
}

async fn save(state: &crate::state::AppState, provider_id: Uuid, row: &Claimed, o: Outcome, now: DateTime<Utc>) -> Result<Done, sqlx::Error> {
    let mut tx = state.pool.begin().await?;
    // The calls cost money whatever happens to the row.
    let mut model = String::new();
    for c in &o.calls {
        if model.is_empty() {
            model = c.model.clone();
        }
        gen_repo::record_call(&mut tx, &c.record("lead_email", Some(provider_id), Some(row.campaign_id), &model), now).await?;
    }
    let (input, output, _cached) = crate::generator::pipeline::totals(&o.calls);
    let (subject, body) = match &o.out {
        Some((s, b)) => (Some(s.clone()), Some(b.clone())),
        None => (None, None),
    };
    let done = sqlx::query(
        "UPDATE lead_emails SET status = $3, subject = COALESCE($4, subject), body_text = COALESCE($5, body_text), body_html = CASE WHEN $5 IS NULL THEN body_html ELSE NULL END,
                source = 'ai', variant_key = NULL, edited_by_hand = CASE WHEN $5 IS NULL THEN edited_by_hand ELSE false END,
                input_hash = COALESCE($6, input_hash), error_code = $7, retry_at = $8, claim_id = NULL, claimed_at = NULL,
                tokens_in = tokens_in + $9, tokens_out = tokens_out + $10, updated_at = $11
          WHERE id = $1 AND claim_id = $2 AND status = 'writing'",
    )
    .bind(row.id)
    .bind(row.claim_id)
    .bind(o.status)
    .bind(subject)
    .bind(body)
    .bind(&o.hash)
    .bind(&o.error_code)
    .bind(o.retry_at)
    .bind(input as i32)
    .bind(output as i32)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() == 0 {
        tx.commit().await?;
        return Ok(Done::Lost);
    }
    // Optimised mode: the other emails of the set go to their own rows. A sent or hand edited email is never replaced.
    for (step, subject, body, hash) in &o.others {
        sqlx::query(
            "INSERT INTO lead_emails (id, campaign_id, lead_id, step_number, subject, body_text, source, status, input_hash, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, 'ai', $7, $8, $9, $9)
             ON CONFLICT (campaign_id, lead_id, step_number) DO UPDATE SET subject = EXCLUDED.subject, body_text = EXCLUDED.body_text, body_html = NULL,
                source = 'ai', status = EXCLUDED.status, input_hash = EXCLUDED.input_hash, error_code = NULL, retry_at = NULL, claim_id = NULL, claimed_at = NULL,
                run_id = NULL, updated_at = EXCLUDED.updated_at
             WHERE lead_emails.status <> 'sent' AND NOT lead_emails.edited_by_hand AND (lead_emails.status <> 'writing' OR lead_emails.claim_id IS NULL)",
        )
        .bind(Uuid::now_v7())
        .bind(row.campaign_id)
        .bind(row.lead_id)
        .bind(step)
        .bind(subject)
        .bind(body)
        .bind(o.status)
        .bind(hash)
        .bind(now)
        .execute(&mut *tx)
        .await?;
    }
    let finished = o.status != "writing";
    if let (Some(run), true) = (row.run_id, finished) {
        let failed = i32::from(o.status == "failed");
        sqlx::query(
            "UPDATE lead_email_runs SET done = done + $2, failed = failed + $3, tokens = tokens + $4 WHERE id = $1",
        )
        .bind(run)
        .bind(1 - failed)
        .bind(failed)
        .bind((input + output) as i64)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    let auth = o.status == "writing" && o.error_code.as_deref() == Some("provider_auth");
    if auth {
        crate::generator::providers::mark_auth_failed(&state.pool, provider_id, now).await;
        return Ok(Done::AuthFailed);
    }
    tracing::info!(email_id = %row.id, status = o.status, input_tokens = input, output_tokens = output, code = o.error_code.as_deref().unwrap_or(""), "email saved");
    Ok(match o.status {
        "ready" | "review" => Done::Written,
        "failed" => Done::Failed,
        _ => Done::Retry,
    })
}
