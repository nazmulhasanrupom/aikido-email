//! The loop that ticks every N seconds.
use super::queue;
use super::worker::{Engine, process_account};
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Default, PartialEq)]
pub struct TickReport {
    pub paused: bool,
    pub stuck: usize,
    pub candidates: usize,
    pub sent: usize,
}

/// One tick: stuck check, find ready accounts, handle each in its own task
/// (at most `max_parallel` at once). Returns when all of them are done.
pub async fn tick(engine: &Arc<Engine>, max_parallel: usize) -> anyhow::Result<TickReport> {
    let pool = &engine.state.pool;
    // 1. Global pause.
    if queue::engine_paused(pool).await? {
        return Ok(TickReport { paused: true, ..Default::default() });
    }
    let now = engine.now();
    // 2. Stuck check.
    let stuck = queue::mark_stuck(pool, now, engine.stuck_after).await?;
    for (job_id, account_id) in &stuck {
        tracing::warn!(%job_id, %account_id, "job was sending for too long: marked stuck, not resent");
    }
    // 3. Ready accounts.
    let ready = queue::ready_accounts(pool, now, 100).await?;
    let candidates = ready.len();
    // 4. One task per account, capped.
    let cancel = engine.cancel.clone();
    let results: Vec<bool> = futures::stream::iter(ready)
        .take_while(|_| std::future::ready(!cancel.is_cancelled()))
        .map(|account_id| {
            let engine = engine.clone();
            async move {
                // Each account runs in its own task, so a panic cannot stop the loop.
                match tokio::spawn(async move { process_account(&engine, account_id).await }).await {
                    Ok(Ok(super::worker::AccountResult::Sent(_))) => true,
                    Ok(Ok(_)) => false,
                    Ok(Err(e)) => {
                        tracing::error!(%account_id, error = %format!("{e:#}"), "account handling failed");
                        false
                    }
                    Err(e) => {
                        tracing::error!(%account_id, error = %e, "account task panicked");
                        false
                    }
                }
            }
        })
        .buffer_unordered(max_parallel.max(1))
        .collect()
        .await;
    Ok(TickReport { paused: false, stuck: stuck.len(), candidates, sent: results.iter().filter(|r| **r).count() })
}

/// Runs until the engine is cancelled. On cancel it stops claiming new jobs;
/// sends that are already running are finished by the tick that holds them.
pub async fn run(engine: Arc<Engine>, tick_secs: u64, max_parallel: usize) {
    tracing::info!(tick_secs, max_parallel, "scheduler started");
    let mut interval = tokio::time::interval(Duration::from_secs(tick_secs.max(1)));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = engine.cancel.cancelled() => break,
            _ = interval.tick() => {}
        }
        if let Err(e) = tick(&engine, max_parallel).await {
            tracing::error!(error = %format!("{e:#}"), "scheduler tick failed");
        }
    }
    tracing::info!("scheduler stopped");
}
