//! The verify worker. Same pattern as the generator worker.
//!
//! Each tick:
//! 1. stop if the engine is paused, or Reoon is not installed or refused the key;
//! 2. start `queued` runs: read the balance (a low balance is a warning, not a stop);
//! 3. claim up to `concurrency` items of running runs with `FOR UPDATE SKIP LOCKED`, a claim id and a 5 minute lease;
//! 4. skip, or call Reoon, and save the result, the lead event and the counters in one transaction;
//! 5. close runs that have no open items.
//!
//! No transaction is open during the network call. No key, no URL and no email text goes to a log.
use super::client::{EmailVerifier, Mode, Verification, VerifyError};
use super::settings;
use crate::integrations::store;
use crate::leads::events;
use crate::state::AppState;
use chrono::{DateTime, Duration, Utc};
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const LEASE_MINS: i64 = 5;
/// Tries for one item after transient errors.
const MAX_ATTEMPTS: i32 = 3;

pub struct VerifyEngine {
    pub state: AppState,
    pub cancel: CancellationToken,
}

impl VerifyEngine {
    pub fn new(state: AppState) -> Self {
        Self { state, cancel: CancellationToken::new() }
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct TickReport {
    pub paused: bool,
    pub not_installed: bool,
    pub auth_failed: bool,
    pub started: usize,
    pub claimed: usize,
    pub done: usize,
    pub skipped: usize,
    pub failed: usize,
    pub finished: usize,
    pub out_of_credits: bool,
}

struct Claimed {
    run_id: Uuid,
    lead_id: Uuid,
    email: String,
    claim_id: Uuid,
    mode: String,
    force: bool,
    attempts: i32,
}

enum Outcome {
    Done,
    Skipped,
    Failed,
    /// Back to `pending`: a transient error.
    Retry,
    /// Stop the run with this error (the item goes back to `pending`).
    StopRun(&'static str),
}

pub async fn tick(engine: &VerifyEngine) -> anyhow::Result<TickReport> {
    let st = &engine.state;
    let now = st.now();
    let mut report = TickReport::default();
    sqlx::query("UPDATE verify_defaults SET last_tick_at = $1 WHERE id = 1").bind(now).execute(&st.pool).await?;
    if settings::is_paused(&st.pool).await? {
        report.paused = true;
        return Ok(report);
    }
    let (integration_id, status, client) = match store::load_client(st, "reoon").await {
        Ok(x) => x,
        Err(store::LoadError::NotInstalled) => {
            report.not_installed = true;
            return Ok(report);
        }
        Err(store::LoadError::BadKey) => {
            tracing::error!("the saved Reoon key can not be decrypted");
            return Ok(report);
        }
        Err(store::LoadError::Db(e)) => return Err(e.into()),
    };
    if status != "installed" {
        report.auth_failed = true;
        return Ok(report);
    }
    let cfg = settings::load(&st.pool).await?;

    report.started = start_queued(st, client.as_ref(), integration_id, &mut report).await?;
    if report.auth_failed {
        return Ok(report);
    }

    let claimed = claim(st, cfg.concurrency.max(1) as i64, now).await?;
    report.claimed = claimed.len();
    let results = futures::future::join_all(claimed.into_iter().map(|c| process(st, client.clone(), cfg.reuse_days, c))).await;
    let mut stop: Option<(Uuid, &'static str)> = None;
    for (run_id, r) in results {
        match r {
            Ok(Outcome::Done) => report.done += 1,
            Ok(Outcome::Skipped) => report.skipped += 1,
            Ok(Outcome::Failed) => report.failed += 1,
            Ok(Outcome::Retry) => {}
            Ok(Outcome::StopRun(code)) => stop = Some((run_id, code)),
            Err(e) => tracing::error!(error = %e, "verify item failed to save"),
        }
    }
    if let Some((run_id, code)) = stop {
        if code == "auth_failed" {
            store::mark_auth_failed(st, integration_id).await;
            report.auth_failed = true;
        } else {
            report.out_of_credits = true;
        }
        // stop every running run: they all share the same key and the same credits
        sqlx::query("UPDATE verification_runs SET status = 'failed', error = $1, finished_at = $2 WHERE status = 'running'").bind(code).bind(st.now()).execute(&st.pool).await?;
        tracing::warn!(run_id = %run_id, code, "verification runs stopped");
    }
    report.finished = close_finished(st).await?;
    Ok(report)
}

/// `queued` -> `running`, with a balance check.
async fn start_queued(st: &AppState, client: &dyn EmailVerifier, integration_id: Uuid, report: &mut TickReport) -> Result<usize, sqlx::Error> {
    let queued: Vec<(Uuid, i32)> = sqlx::query_as("SELECT id, total FROM verification_runs WHERE status = 'queued' ORDER BY created_at LIMIT 20").fetch_all(&st.pool).await?;
    if queued.is_empty() {
        return Ok(0);
    }
    let balance = client.balance().await;
    if matches!(balance, Err(VerifyError::Auth)) {
        store::mark_auth_failed(st, integration_id).await;
        sqlx::query("UPDATE verification_runs SET status = 'failed', error = 'auth_failed', finished_at = $1 WHERE status IN ('queued', 'running')").bind(st.now()).execute(&st.pool).await?;
        report.auth_failed = true;
        return Ok(0);
    }
    let credits = balance.ok().map(|b| b.daily_credits.unwrap_or(0) + b.instant_credits.unwrap_or(0));
    let mut n = 0;
    for (id, total) in queued {
        let warning = credits.filter(|c| *c < total as i64).map(|c| format!("Reoon shows {c} credits for {total} emails. The run may stop when credits run out."));
        let r = sqlx::query("UPDATE verification_runs SET status = 'running', warning = $2 WHERE id = $1 AND status = 'queued'").bind(id).bind(warning).execute(&st.pool).await?;
        n += r.rows_affected() as usize;
    }
    Ok(n)
}

async fn claim(st: &AppState, limit: i64, now: DateTime<Utc>) -> Result<Vec<Claimed>, sqlx::Error> {
    let claim_id = Uuid::now_v7();
    let rows: Vec<(Uuid, Uuid, String, String, bool, i32)> = sqlx::query_as(
        "WITH pick AS (
             SELECT i.run_id, i.lead_id FROM verification_items i JOIN verification_runs r ON r.id = i.run_id
              WHERE r.status = 'running' AND (i.state = 'pending' OR (i.state = 'claimed' AND i.claimed_at < $3))
              ORDER BY r.created_at, i.lead_id LIMIT $2 FOR UPDATE OF i SKIP LOCKED)
         UPDATE verification_items i SET state = 'claimed', claim_id = $1, claimed_at = $4, attempts = i.attempts + 1
           FROM pick, verification_runs r
          WHERE i.run_id = pick.run_id AND i.lead_id = pick.lead_id AND r.id = i.run_id
         RETURNING i.run_id, i.lead_id, i.email, r.mode, r.force, i.attempts",
    )
    .bind(claim_id)
    .bind(limit)
    .bind(now - Duration::minutes(LEASE_MINS))
    .bind(now)
    .fetch_all(&st.pool)
    .await?;
    Ok(rows.into_iter().map(|(run_id, lead_id, email, mode, force, attempts)| Claimed { run_id, lead_id, email, claim_id, mode, force, attempts }).collect())
}

async fn process(st: &AppState, client: Arc<dyn EmailVerifier>, reuse_days: i32, c: Claimed) -> (Uuid, Result<Outcome, sqlx::Error>) {
    let run_id = c.run_id;
    (run_id, process_inner(st, client, reuse_days, c).await)
}

async fn process_inner(st: &AppState, client: Arc<dyn EmailVerifier>, reuse_days: i32, c: Claimed) -> Result<Outcome, sqlx::Error> {
    let now = st.now();
    // the lead is gone, or its email changed: nothing to check
    let current: Option<(String,)> = sqlx::query_as("SELECT email::text FROM leads WHERE id = $1").bind(c.lead_id).fetch_optional(&st.pool).await?;
    if current.as_ref().is_none_or(|(e,)| !e.eq_ignore_ascii_case(&c.email)) {
        return finish_item(st, &c, "skipped", None, None).await;
    }
    if !c.force && reuse_days > 0 {
        let recent: Option<(i32,)> = sqlx::query_as("SELECT 1 FROM email_verifications WHERE lead_id = $1 AND provider = 'reoon' AND email = $2 AND checked_at > $3")
            .bind(c.lead_id)
            .bind(&c.email)
            .bind(now - Duration::days(reuse_days as i64))
            .fetch_optional(&st.pool)
            .await?;
        if recent.is_some() {
            return finish_item(st, &c, "skipped", None, None).await;
        }
    }
    let mode = Mode::parse(&c.mode).unwrap_or(Mode::Power);
    match client.verify(&c.email, mode).await {
        Ok(v) => finish_item(st, &c, "done", Some((&v, mode)), None).await,
        Err(VerifyError::Auth) => release(st, &c, "auth_failed").await.map(|_| Outcome::StopRun("auth_failed")),
        Err(VerifyError::OutOfCredits) => release(st, &c, "out_of_credits").await.map(|_| Outcome::StopRun("out_of_credits")),
        Err(VerifyError::Transient(code)) if c.attempts < MAX_ATTEMPTS => release(st, &c, code).await.map(|_| Outcome::Retry),
        Err(e) => finish_item(st, &c, "failed", None, Some(e.code())).await,
    }
}

/// Put the item back to `pending` (keeps the error code for the person who looks).
async fn release(st: &AppState, c: &Claimed, code: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE verification_items SET state = 'pending', claim_id = NULL, error = $3 WHERE run_id = $1 AND lead_id = $2 AND claim_id = $4")
        .bind(c.run_id)
        .bind(c.lead_id)
        .bind(code)
        .bind(c.claim_id)
        .execute(&st.pool)
        .await?;
    Ok(())
}

/// Close an item and update the run counters. For `done`, save the result and write the lead event too.
/// All in one transaction, guarded by the claim id: a stale worker writes nothing.
async fn finish_item(st: &AppState, c: &Claimed, state: &str, result: Option<(&Verification, Mode)>, error: Option<String>) -> Result<Outcome, sqlx::Error> {
    let now = st.now();
    let mut tx = st.pool.begin().await?;
    let closed = sqlx::query("UPDATE verification_items SET state = $3, error = $4 WHERE run_id = $1 AND lead_id = $2 AND claim_id = $5 AND state = 'claimed'")
        .bind(c.run_id)
        .bind(c.lead_id)
        .bind(state)
        .bind(&error)
        .bind(c.claim_id)
        .execute(&mut *tx)
        .await?;
    if closed.rows_affected() == 0 {
        // the lease ran out and another copy took the item, or the run was cancelled
        return Ok(Outcome::Retry);
    }
    let column = match state {
        "done" => "done_count = done_count + 1, credits_used = credits_used + 1",
        "failed" => "failed_count = failed_count + 1",
        _ => "skipped_count = skipped_count + 1",
    };
    sqlx::query(&format!("UPDATE verification_runs SET {column} WHERE id = $1")).bind(c.run_id).execute(&mut *tx).await?;
    if let Some((v, mode)) = result {
        sqlx::query(
            "INSERT INTO email_verifications (id, lead_id, email, provider, mode, status, score, safe_to_send, flags, checked_at, run_id)
             VALUES ($1, $2, $3, 'reoon', $4, $5, $6, $7, $8, $9, $10)
             ON CONFLICT (lead_id, provider) DO UPDATE SET email = EXCLUDED.email, mode = EXCLUDED.mode, status = EXCLUDED.status, score = EXCLUDED.score,
                 safe_to_send = EXCLUDED.safe_to_send, flags = EXCLUDED.flags, checked_at = EXCLUDED.checked_at, run_id = EXCLUDED.run_id",
        )
        .bind(Uuid::now_v7())
        .bind(c.lead_id)
        .bind(&c.email)
        .bind(mode.as_str())
        .bind(&v.status)
        .bind(v.score)
        .bind(v.safe_to_send)
        .bind(&v.flags)
        .bind(now)
        .bind(c.run_id)
        .execute(&mut *tx)
        .await?;
        events::log(&mut tx, c.lead_id, "email_verified", Some(c.run_id), json!({ "provider": "reoon", "status": v.status, "mode": mode.as_str(), "score": v.score }), now).await?;
    }
    tx.commit().await?;
    Ok(match state {
        "done" => Outcome::Done,
        "failed" => Outcome::Failed,
        _ => Outcome::Skipped,
    })
}

/// Running runs with no open items are `done`.
async fn close_finished(st: &AppState) -> Result<usize, sqlx::Error> {
    let r = sqlx::query(
        "UPDATE verification_runs r SET status = 'done', finished_at = $1
          WHERE r.status = 'running' AND NOT EXISTS (SELECT 1 FROM verification_items i WHERE i.run_id = r.id AND i.state IN ('pending', 'claimed'))",
    )
    .bind(st.now())
    .execute(&st.pool)
    .await?;
    Ok(r.rows_affected() as usize)
}

pub async fn run(engine: Arc<VerifyEngine>) {
    tracing::info!("verify worker started");
    loop {
        match tick(&engine).await {
            Ok(r) if r.claimed > 0 || r.started > 0 => {
                tracing::info!(started = r.started, claimed = r.claimed, done = r.done, skipped = r.skipped, failed = r.failed, "verify tick")
            }
            Ok(_) => {}
            Err(e) => tracing::error!(error = %format!("{e:#}"), "verify tick failed"),
        }
        let secs = settings::load(&engine.state.pool).await.map(|s| s.tick_secs).unwrap_or(5);
        tokio::select! {
            _ = engine.cancel.cancelled() => break,
            _ = tokio::time::sleep(std::time::Duration::from_secs(secs.max(1) as u64)) => {}
        }
    }
    tracing::info!("verify worker stopped");
}
