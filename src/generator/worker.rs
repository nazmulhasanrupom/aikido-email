//! The generation worker. Same pattern as the reply poller and the campaign feeder.
//!
//! Each tick:
//! 1. stop if the engine is paused, or there is no working slot provider;
//! 2. mark text for a changed step `stale`;
//! 3. insert a `pending` row for each open enrollment whose next step has AI slots and is due within `lookahead_hours`
//!    (a unique index stops doubles);
//! 4. claim `pending` rows with `FOR UPDATE SKIP LOCKED`, a claim id and a 10 minute lease, oldest due first;
//! 5. check the budgets, build the context and the prompt, call the model, run the guard, save the text, and write one
//!    `llm_calls` row per call.
//!
//! No database transaction is open during the network call: claim, commit, call, then write the result with the claim id.
use super::pipeline::{self, CallInfo, FailKind, GenInput};
use super::llm::LlmClient;
use super::prompt::{PromptLead, PromptStep};
use super::providers::{self, ConfigError};
use super::repo;
use super::settings::GeneratorSettings;
use super::slots::{self, AiSlot};
use crate::leads::context::{self as lead_context, ContextOpts};
use crate::state::AppState;
use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use sqlx::PgPool;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Most rows one tick writes. The next tick goes on.
const MAX_PER_TICK: usize = 2000;
/// Most new rows one tick inserts.
const DISCOVER_BATCH: i64 = 1000;
const LEASE_MINS: i64 = 10;

pub struct GeneratorEngine {
    pub state: AppState,
    pub cancel: CancellationToken,
}

impl GeneratorEngine {
    pub fn new(state: AppState) -> Self {
        Self { state, cancel: CancellationToken::new() }
    }
    pub fn now(&self) -> DateTime<Utc> {
        self.state.now()
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct TickReport {
    /// The generator is paused.
    pub paused: bool,
    /// There is no slot provider, or it does not exist.
    pub no_provider: bool,
    /// The slot provider refused its key.
    pub auth_failed: bool,
    /// Rows made `stale` because their step changed.
    pub staled: usize,
    /// New `pending` rows.
    pub discovered: usize,
    /// Rows claimed.
    pub claimed: usize,
    pub ready: usize,
    pub pending_review: usize,
    pub failed: usize,
    /// Rows put back to `pending` after a transient error.
    pub retried: usize,
    /// Rows whose step changed while they waited.
    pub stale: usize,
    /// The daily token budget is used up: nothing more was called.
    pub daily_budget_reached: bool,
}

pub async fn tick(engine: &GeneratorEngine) -> anyhow::Result<TickReport> {
    let pool = &engine.state.pool;
    let now = engine.now();
    repo::touch_tick(pool, now).await?;
    let mut report = TickReport::default();
    if repo::engine_paused(pool).await? {
        report.paused = true;
        return Ok(report);
    }
    let mut conn = pool.acquire().await?;
    let settings = repo::load_settings(&mut conn).await?;
    let provider = match settings.slot_provider_id {
        Some(id) => providers::get(&mut conn, id).await?,
        None => None,
    };
    drop(conn);
    let Some(provider) = provider else {
        report.no_provider = true;
        return Ok(report);
    };
    if provider.status == "auth_failed" {
        report.auth_failed = true;
        return Ok(report);
    }
    let cfg = match providers::load_config(pool, &engine.state.crypto, provider.id, settings.max_attempts).await {
        Ok((cfg, _)) => cfg,
        Err(ConfigError::NotFound) => {
            report.no_provider = true;
            return Ok(report);
        }
        Err(ConfigError::BadKey) => {
            tracing::error!(provider_id = %provider.id, "the API key of the provider can not be decrypted");
            report.auth_failed = true;
            return Ok(report);
        }
        Err(ConfigError::Db(e)) => return Err(e.into()),
    };
    let client = match engine.state.llm.build(&cfg) {
        Ok(c) => c,
        Err(_) => {
            report.no_provider = true;
            return Ok(report);
        }
    };

    report.staled = mark_stale(pool, now).await?;
    report.discovered = discover(pool, now, &settings).await?;

    let width = settings.concurrency.min(provider.max_concurrency.max(1) as u32).max(1) as usize;
    let ctx = Arc::new(Ctx { client, provider_id: provider.id, settings: settings.clone() });
    let mut total = 0usize;
    while total < MAX_PER_TICK && !engine.cancel.is_cancelled() {
        let mut conn = pool.acquire().await?;
        if settings.daily_token_budget > 0 && repo::tokens_today(&mut conn, now).await? as u64 >= settings.daily_token_budget {
            report.daily_budget_reached = true;
            break;
        }
        drop(conn);
        let rows = claim(pool, now, width as i64).await?;
        if rows.is_empty() {
            break;
        }
        total += rows.len();
        report.claimed += rows.len();
        let results = futures::future::join_all(rows.into_iter().map(|row| {
            let ctx = ctx.clone();
            let state = engine.state.clone();
            async move { process(&state, &ctx, row).await }
        }))
        .await;
        let mut stop = false;
        for r in results {
            match r {
                Ok(Done::Ready) => report.ready += 1,
                Ok(Done::PendingReview) => report.pending_review += 1,
                Ok(Done::Failed) => report.failed += 1,
                Ok(Done::Retry) => report.retried += 1,
                Ok(Done::Stale) => report.stale += 1,
                Ok(Done::AuthFailed) => {
                    report.auth_failed = true;
                    stop = true;
                }
                Ok(Done::Lost) => {}
                // One bad row must not stop the others.
                Err(e) => tracing::error!(error = %e, "generation failed to save"),
            }
        }
        if stop {
            break;
        }
    }
    Ok(report)
}

/// Runs until the engine is cancelled.
pub async fn run(engine: Arc<GeneratorEngine>) {
    tracing::info!("generator worker started");
    loop {
        match tick(&engine).await {
            Ok(r) if r.claimed > 0 || r.discovered > 0 => {
                tracing::info!(discovered = r.discovered, claimed = r.claimed, ready = r.ready, pending_review = r.pending_review, failed = r.failed, "generator tick")
            }
            Ok(_) => {}
            Err(e) => tracing::error!(error = %format!("{e:#}"), "generator tick failed"),
        }
        let secs = match engine.state.pool.acquire().await {
            Ok(mut c) => repo::load_settings(&mut c).await.map(|s| s.tick_secs).unwrap_or(10),
            Err(_) => 10,
        };
        tokio::select! {
            _ = engine.cancel.cancelled() => break,
            _ = tokio::time::sleep(std::time::Duration::from_secs(secs.max(1) as u64)) => {}
        }
    }
    tracing::info!("generator worker stopped");
}

struct Ctx {
    client: Arc<dyn LlmClient>,
    provider_id: Uuid,
    settings: GeneratorSettings,
}

/// Text for a step that changed (or was removed) is `stale`. Only the open enrollments at that step are looked at.
async fn mark_stale(pool: &PgPool, now: DateTime<Utc>) -> Result<usize, sqlx::Error> {
    let done = sqlx::query(
        "UPDATE generations g SET status = 'stale', claim_id = NULL, updated_at = $1
           FROM enrollments e
          WHERE e.id = g.enrollment_id AND e.next_step = g.step_number AND e.status IN ('active', 'paused')
            AND g.status IN ('pending', 'generating', 'ready', 'pending_review', 'approved')
            AND NOT EXISTS (SELECT 1 FROM sequence_steps st WHERE st.sequence_id = g.sequence_id AND st.step_number = g.step_number AND st.step_hash = g.step_hash)",
    )
    .bind(now)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() as usize)
}

/// A `pending` row for each open enrollment whose next step has AI slots and is due soon. Text that went `stale` or was
/// `rejected` for the same step is made again.
async fn discover(pool: &PgPool, now: DateTime<Utc>, settings: &GeneratorSettings) -> Result<usize, sqlx::Error> {
    let done = sqlx::query(
        "INSERT INTO generations (enrollment_id, lead_id, sequence_id, step_number, step_hash, status, due_at, created_at, updated_at)
         SELECT e.id, e.lead_id, e.sequence_id, e.next_step, st.step_hash, 'pending', e.next_due_at, $1, $1
           FROM enrollments e
           JOIN sequences s ON s.id = e.sequence_id AND s.status = 'active'
           JOIN sequence_steps st ON st.sequence_id = e.sequence_id AND st.step_number = e.next_step
          WHERE e.status = 'active' AND e.lead_id IS NOT NULL AND e.next_due_at <= $1 + make_interval(hours => $2)
            AND st.ai_slots <> '[]'::jsonb AND st.step_hash IS NOT NULL
            AND NOT EXISTS (SELECT 1 FROM generations g WHERE g.enrollment_id = e.id AND g.step_number = e.next_step
                              AND g.step_hash = st.step_hash AND g.status NOT IN ('stale', 'rejected'))
          ORDER BY e.next_due_at, e.id
          LIMIT $3
         ON CONFLICT (enrollment_id, step_number, step_hash) DO UPDATE
            SET status = 'pending', attempts = 0, slots = '{}', warnings = '[]', error_code = NULL, claim_id = NULL, claimed_at = NULL,
                retry_at = NULL, due_at = EXCLUDED.due_at, updated_at = EXCLUDED.updated_at
          WHERE generations.status IN ('stale', 'rejected')",
    )
    .bind(now)
    .bind(settings.lookahead_hours as i32)
    .bind(DISCOVER_BATCH)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() as usize)
}

#[derive(sqlx::FromRow)]
struct Claimed {
    id: Uuid,
    sequence_id: Uuid,
    lead_id: Uuid,
    step_number: i32,
    step_hash: String,
    attempts: i32,
    claim_id: Uuid,
}

/// Take up to `n` rows. A row whose lease ran out (a crashed copy) is taken again. A campaign over its token budget is skipped.
async fn claim(pool: &PgPool, now: DateTime<Utc>, n: i64) -> Result<Vec<Claimed>, sqlx::Error> {
    sqlx::query_as(
        "WITH picked AS (
            SELECT g.id FROM generations g JOIN enrollments e ON e.id = g.enrollment_id
             WHERE (g.status = 'pending' OR (g.status = 'generating' AND g.claimed_at < $1 - make_interval(mins => $4)))
               AND (g.retry_at IS NULL OR g.retry_at <= $1)
               AND e.status = 'active' AND e.next_step = g.step_number
               AND NOT EXISTS (
                    SELECT 1 FROM sequences s JOIN generator_campaigns gc ON gc.campaign_id = s.campaign_id
                     WHERE s.id = g.sequence_id AND gc.token_budget IS NOT NULL
                       AND (SELECT COALESCE(SUM(input_tokens + output_tokens), 0) FROM llm_calls WHERE campaign_id = s.campaign_id) >= gc.token_budget)
             ORDER BY g.due_at NULLS LAST, g.seq
             LIMIT $2
             FOR UPDATE OF g SKIP LOCKED)
         UPDATE generations g SET status = 'generating', claim_id = $3, claimed_at = $1, attempts = g.attempts + 1, updated_at = $1
           FROM picked WHERE g.id = picked.id
         RETURNING g.id, g.sequence_id, g.lead_id, g.step_number, g.step_hash, g.attempts, g.claim_id",
    )
    .bind(now)
    .bind(n)
    .bind(Uuid::now_v7())
    .bind(LEASE_MINS as i32)
    .fetch_all(pool)
    .await
}

enum Done {
    Ready,
    PendingReview,
    Failed,
    Retry,
    Stale,
    AuthFailed,
    /// The row was cancelled or taken by another copy meanwhile: the result is dropped.
    Lost,
}

/// How a claimed row ends.
struct RowResult {
    status: &'static str,
    slots: serde_json::Value,
    warnings: Vec<String>,
    error_code: Option<String>,
    retry_at: Option<DateTime<Utc>>,
    calls: Vec<CallInfo>,
}

impl RowResult {
    fn plain(status: &'static str, code: &str) -> Self {
        Self { status, slots: json!({}), warnings: vec![], error_code: Some(code.into()), retry_at: None, calls: vec![] }
    }
}

struct StepRow {
    subject: String,
    body_text: Option<String>,
    body_html: Option<String>,
    ai_slots: serde_json::Value,
    step_hash: Option<String>,
}

async fn process(state: &AppState, ctx: &Ctx, row: Claimed) -> Result<Done, sqlx::Error> {
    let now = state.now();
    let pool = &state.pool;
    let mut conn = pool.acquire().await?;
    let campaign_id = repo::campaign_of_sequence(&mut conn, row.sequence_id).await?;
    let (campaign_review, _) = match campaign_id {
        Some(c) => repo::campaign_overrides(&mut conn, c).await?,
        None => (None, None),
    };
    let review = campaign_review.unwrap_or(ctx.settings.review_required);

    let step: Option<StepRow> = sqlx::query_as::<_, (String, Option<String>, Option<String>, serde_json::Value, Option<String>)>(
        "SELECT subject, body_text, body_html, ai_slots, step_hash FROM sequence_steps WHERE sequence_id = $1 AND step_number = $2",
    )
    .bind(row.sequence_id)
    .bind(row.step_number)
    .fetch_optional(&mut *conn)
    .await?
    .map(|(subject, body_text, body_html, ai_slots, step_hash)| StepRow { subject, body_text, body_html, ai_slots, step_hash });
    let brief: Option<String> = sqlx::query_as::<_, (Option<String>,)>("SELECT brief FROM sequences WHERE id = $1").bind(row.sequence_id).fetch_optional(&mut *conn).await?.and_then(|r| r.0);

    let result = match step {
        Some(step) if step.step_hash.as_deref() == Some(row.step_hash.as_str()) => {
            if row.attempts > ctx.settings.max_attempts as i32 {
                RowResult::plain("failed", "attempts_exceeded")
            } else {
                let opts = ContextOpts { max_events: 20, max_messages: 2, include_received: false, max_text_chars: 600 };
                match lead_context::build(&mut conn, row.lead_id, &opts).await? {
                    None => RowResult::plain("failed", "no_lead"),
                    Some(lead_ctx) => {
                        drop(conn);
                        let lead = PromptLead::from_context(&lead_ctx);
                        let ai_slots: Vec<AiSlot> = slots::from_json(&step.ai_slots);
                        let input = GenInput {
                            client: ctx.client.as_ref(),
                            settings: &ctx.settings,
                            step: PromptStep { step_number: row.step_number, subject: &step.subject, body_text: step.body_text.as_deref(), body_html: step.body_html.as_deref() },
                            slots: &ai_slots,
                            brief: brief.as_deref(),
                            lead: &lead,
                        };
                        // The network call. No transaction is open here.
                        match pipeline::generate(&input).await {
                            Ok(g) => RowResult {
                                status: if review { "pending_review" } else { "ready" },
                                slots: serde_json::to_value(&g.slots).unwrap_or_default(),
                                warnings: g.warnings,
                                error_code: None,
                                retry_at: None,
                                calls: g.calls,
                            },
                            Err(f) => match f.kind {
                                FailKind::Auth => RowResult { status: "pending", slots: json!({}), warnings: vec![], error_code: Some(f.code), retry_at: None, calls: f.calls },
                                FailKind::Transient if row.attempts < ctx.settings.max_attempts as i32 => RowResult {
                                    status: "pending",
                                    slots: json!({}),
                                    warnings: vec![],
                                    error_code: Some(f.code),
                                    retry_at: Some(now + Duration::seconds((30 * row.attempts as i64).min(600))),
                                    calls: f.calls,
                                },
                                _ => RowResult { status: "failed", slots: json!({}), warnings: vec![f.message.chars().take(300).collect()], error_code: Some(f.code), retry_at: None, calls: f.calls },
                            },
                        }
                    }
                }
            }
        }
        // The step changed or was removed while the row waited.
        _ => RowResult::plain("stale", "step_changed"),
    };

    save(state, ctx, &row, campaign_id, result, now).await
}

async fn save(state: &AppState, ctx: &Ctx, row: &Claimed, campaign_id: Option<Uuid>, r: RowResult, now: DateTime<Utc>) -> Result<Done, sqlx::Error> {
    let mut tx = state.pool.begin().await?;
    // The calls cost money whatever happens to the row.
    let mut model = String::new();
    for c in &r.calls {
        if model.is_empty() {
            model = c.model.clone();
        }
        repo::record_call(&mut tx, &c.record("slot", Some(ctx.provider_id), campaign_id, &model), now).await?;
    }
    let (input, output, cached) = pipeline::totals(&r.calls);
    let done = sqlx::query(
        "UPDATE generations SET status = $3, slots = $4, warnings = $5, error_code = $6, retry_at = $7, claim_id = NULL, claimed_at = NULL,
                provider_id = $8, model = NULLIF($9, ''), input_tokens = input_tokens + $10, output_tokens = output_tokens + $11,
                cached_tokens = cached_tokens + $12, updated_at = $13
          WHERE id = $1 AND claim_id = $2 AND status = 'generating'",
    )
    .bind(row.id)
    .bind(row.claim_id)
    .bind(r.status)
    .bind(&r.slots)
    .bind(json!(r.warnings))
    .bind(&r.error_code)
    .bind(r.retry_at)
    .bind(ctx.provider_id)
    .bind(&model)
    .bind(input as i32)
    .bind(output as i32)
    .bind(cached as i32)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    if done.rows_affected() == 0 {
        return Ok(Done::Lost);
    }
    let auth = r.status == "pending" && r.error_code.as_deref() == Some("provider_auth");
    if auth {
        providers::mark_auth_failed(&state.pool, ctx.provider_id, now).await;
        return Ok(Done::AuthFailed);
    }
    tracing::info!(generation_id = %row.id, status = r.status, input_tokens = input, output_tokens = output, code = r.error_code.as_deref().unwrap_or(""), "generation saved");
    Ok(match r.status {
        "ready" => Done::Ready,
        "pending_review" => Done::PendingReview,
        "failed" => Done::Failed,
        "stale" => Done::Stale,
        _ => Done::Retry,
    })
}
