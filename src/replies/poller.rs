//! The loop that checks each inbox on a timer.
use super::repo;
use super::sync::{ReplyEngine, sync_folder};
use crate::accounts::model::{EmailAccount, Secrets};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use uuid::Uuid;

/// Most sync rows one tick claims.
const CLAIM_LIMIT: i64 = 100;

#[derive(Debug, Default, PartialEq)]
pub struct TickReport {
    /// Reply check is paused for the whole engine.
    pub paused: bool,
    /// Sync rows this tick claimed.
    pub claimed: usize,
    /// Folders that finished without an error.
    pub synced: usize,
    /// Folders that ended with an error (saved in `inbox_sync_state.last_error`).
    pub failed: usize,
}

/// Check the read access of accounts that were never checked, and make the sync rows that are missing.
/// Old accounts (connected before this step) and new accounts both pass through here.
async fn prepare_accounts(engine: &ReplyEngine) -> anyhow::Result<()> {
    let pool = &engine.state.pool;
    for account in repo::accounts_unchecked(pool).await? {
        let (scopes, can_read, status) = read_access_of(engine, &account);
        repo::set_read_access(pool, account.id, Some(&scopes), can_read, status).await?;
    }
    for account in repo::accounts_without_sync_rows(pool).await? {
        let mut conn = pool.acquire().await?;
        let settings = repo::load_settings(&mut conn, account.id).await?;
        repo::ensure_sync_rows(&mut conn, &account, &settings).await?;
    }
    Ok(())
}

/// Read access of an account from what is saved: the granted scopes (OAuth) or the IMAP details (SMTP).
fn read_access_of(engine: &ReplyEngine, account: &EmailAccount) -> (Vec<String>, bool, &'static str) {
    if !engine.factory.needs_credentials() {
        return (account.granted_scopes.clone(), account.can_read, "ok");
    }
    match crate::accounts::repo::load_secrets(&engine.state.crypto, account) {
        Ok(secrets @ Secrets::OAuth(_)) => EmailAccount::read_access(account.provider, &secrets, false),
        Ok(secrets) => EmailAccount::read_access(account.provider, &secrets, account.imap_host.is_some()),
        Err(_) => (vec![], false, "reconnect_needed"),
    }
}

/// One tick: claim the due sync rows, and sync each account in its own task (at most `max_parallel` at once).
/// Returns when all of them are done.
pub async fn tick(engine: &Arc<ReplyEngine>, max_parallel: usize) -> anyhow::Result<TickReport> {
    let pool = &engine.state.pool;
    // 1. Global pause.
    if repo::engine_paused(pool).await? {
        return Ok(TickReport { paused: true, ..Default::default() });
    }
    prepare_accounts(engine).await?;
    // 2. Find and claim the due rows. Other server copies skip rows that are claimed.
    let claimed = repo::claim_due(pool, engine.now(), CLAIM_LIMIT).await?;
    let mut report = TickReport { claimed: claimed.len(), ..Default::default() };
    // 3. One task per account. Its folders run one after the other.
    let mut by_account: BTreeMap<Uuid, Vec<String>> = BTreeMap::new();
    for (account_id, folder) in claimed {
        by_account.entry(account_id).or_default().push(folder);
    }
    let permits = Arc::new(Semaphore::new(max_parallel.max(1)));
    let mut tasks = tokio::task::JoinSet::new();
    for (account_id, folders) in by_account {
        let (engine, permits) = (engine.clone(), permits.clone());
        tasks.spawn(async move {
            let _permit = permits.acquire().await.expect("the semaphore is never closed");
            let mut results = Vec::new();
            for folder in folders {
                let r = sync_folder(&engine, account_id, &folder).await;
                if let Err(e) = &r {
                    tracing::error!(%account_id, %folder, error = %format!("{e:#}"), "reply sync failed");
                }
                results.push(matches!(&r, Ok(rep) if rep.error.is_none()));
            }
            results
        });
    }
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(results) => {
                report.synced += results.iter().filter(|ok| **ok).count();
                report.failed += results.iter().filter(|ok| !**ok).count();
            }
            // A panic in one account must not stop the loop.
            Err(e) => {
                tracing::error!(error = %e, "reply sync task panicked");
                report.failed += 1;
            }
        }
    }
    Ok(report)
}

/// Runs until the engine is cancelled.
pub async fn run(engine: Arc<ReplyEngine>, tick_secs: u64, max_parallel: usize) {
    tracing::info!(tick_secs, max_parallel, "reply check started");
    let mut interval = tokio::time::interval(Duration::from_secs(tick_secs.max(1)));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = engine.cancel.cancelled() => break,
            _ = interval.tick() => {}
        }
        if let Err(e) = tick(&engine, max_parallel).await {
            tracing::error!(error = %format!("{e:#}"), "reply check tick failed");
        }
    }
    tracing::info!("reply check stopped");
}
