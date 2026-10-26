//! The trigger loops (build map 9.2 and 9.3): time triggers fire when due, event triggers read new events.
//!
//! One trigger is handled in one transaction that holds the row lock (`FOR UPDATE SKIP LOCKED`). The runs it
//! makes and the move of its cursor or next time commit together, so two server copies never fire the same
//! trigger twice, and a crash loses nothing.
use super::events::{self, Built};
use super::runs::{self, NewRun};
use super::schedule;
use crate::state::AppState;
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use sqlx::{FromRow, PgConnection};
use std::time::Duration as StdDuration;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// How often the loop looks for due time triggers (build map 9.2).
const TIME_TICK: StdDuration = StdDuration::from_secs(15);
/// A due time trigger that is this late counts as "missed" for `catch_up: skip`.
const LATE_SECS: i64 = 60;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TickReport {
    /// Triggers that made at least one run, or moved their time.
    pub fired: usize,
    /// Runs made.
    pub runs: usize,
    /// Triggers held back by `max_runs_per_hour` or by a full queue.
    pub held: usize,
    /// Triggers that failed. The reason is in `automation_triggers.last_error`.
    pub errors: usize,
}

#[derive(FromRow)]
struct Due {
    id: Uuid,
    workflow_id: Uuid,
    version_id: Uuid,
    node_id: String,
    kind: String,
    trigger_type: String,
    config: Value,
    next_run_at: Option<DateTime<Utc>>,
    cursor: Option<Value>,
    run_as_user_id: Option<Uuid>,
    max_per_hour: Option<i32>,
}

const SELECT: &str = "SELECT t.id, t.workflow_id, t.version_id, t.node_id, t.kind, t.trigger_type, t.config, t.next_run_at, t.cursor, w.run_as_user_id, \
    (v.definition -> 'settings' ->> 'max_runs_per_hour')::int AS max_per_hour \
    FROM automation_triggers t JOIN automation_workflows w ON w.id = t.workflow_id JOIN automation_versions v ON v.id = t.version_id";

enum Fired {
    /// The trigger did its work (maybe no run was needed).
    Done { runs: usize },
    /// The trigger waits: `max_runs_per_hour` is reached or the queue is full.
    Held,
}

async fn record_error(st: &AppState, trigger: Uuid, message: &str) {
    let r = sqlx::query("UPDATE automation_triggers SET last_error = $2 WHERE id = $1").bind(trigger).bind(message).execute(&st.pool).await;
    if let Err(e) = r {
        tracing::warn!(trigger = %trigger, error = %e, "could not save a trigger error");
    }
}

/// Set or clear the "held back by the hourly limit" mark.
async fn mark_held(conn: &mut PgConnection, trigger: Uuid, rate_limited: bool, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    if rate_limited {
        sqlx::query("UPDATE automation_triggers SET rate_limited_at = COALESCE(rate_limited_at, $2) WHERE id = $1").bind(trigger).bind(now).execute(conn).await?;
    }
    Ok(())
}

// ---------- time triggers ----------

async fn fire_time(tx: &mut PgConnection, t: &Due, now: DateTime<Utc>) -> Result<Fired, sqlx::Error> {
    let Some(scheduled) = t.next_run_at else { return Ok(Fired::Done { runs: 0 }) };
    let lim = runs::limits(tx, t.workflow_id, runs::max_per_hour(t.max_per_hour), now).await?;
    if !lim.open() {
        mark_held(tx, t.id, lim.remaining <= 0, now).await?;
        return Ok(Fired::Held);
    }
    // `catch_up: skip`: a time that was missed is not run late
    let late = (now - scheduled).num_seconds() > LATE_SECS;
    let skip = late && t.config.get("catch_up").and_then(Value::as_str) == Some("skip");
    let mut made = 0;
    if !skip {
        let item = json!({ "fired_at": now, "scheduled_for": scheduled, "timezone": t.config.get("timezone").cloned().unwrap_or_else(|| json!("UTC")) });
        let run = NewRun {
            workflow_id: t.workflow_id,
            version_id: t.version_id,
            trigger_node_id: &t.node_id,
            trigger_item: item,
            run_as_user_id: t.run_as_user_id,
            started_by: None,
            idempotency_key: Some(format!("trigger:{}:at:{}", t.id, scheduled.to_rfc3339())),
        };
        if runs::insert_run(tx, now, run).await?.is_some() {
            made = 1;
        }
    }
    match schedule::next_after(&t.kind, &t.config, scheduled, now) {
        Ok(next) => {
            sqlx::query("UPDATE automation_triggers SET next_run_at = $2, last_fired_at = CASE WHEN $3 THEN $4 ELSE last_fired_at END, last_error = NULL, rate_limited_at = NULL WHERE id = $1")
                .bind(t.id)
                .bind(next)
                .bind(made > 0)
                .bind(now)
                .execute(tx)
                .await?;
        }
        Err(message) => {
            // it fired, but it cannot be planned again: try again in an hour and show the reason
            sqlx::query("UPDATE automation_triggers SET next_run_at = $2, last_fired_at = $3, last_error = $4 WHERE id = $1")
                .bind(t.id)
                .bind(now + Duration::hours(1))
                .bind(now)
                .bind(message)
                .execute(tx)
                .await?;
        }
    }
    Ok(Fired::Done { runs: made })
}

/// Fire every time trigger that is due.
pub async fn time_tick(st: &AppState) -> Result<TickReport, sqlx::Error> {
    let now = st.now();
    let mut report = TickReport::default();
    let mut seen: Vec<Uuid> = Vec::new();
    loop {
        let mut tx = st.pool.begin().await?;
        let row: Option<Due> = sqlx::query_as(&format!(
            "{SELECT} WHERE t.kind IN ('schedule', 'cron') AND t.next_run_at <= $1 AND w.active AND w.archived_at IS NULL AND t.id <> ALL($2) \
             ORDER BY t.next_run_at, t.id LIMIT 1 FOR UPDATE OF t SKIP LOCKED"
        ))
        .bind(now)
        .bind(&seen)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(t) = row else { break };
        seen.push(t.id);
        match fire_time(&mut tx, &t, now).await {
            Ok(Fired::Done { runs }) => {
                tx.commit().await?;
                report.fired += 1;
                report.runs += runs;
            }
            Ok(Fired::Held) => {
                tx.commit().await?;
                report.held += 1;
            }
            Err(e) => {
                drop(tx);
                tracing::warn!(trigger = %t.id, workflow = %t.workflow_id, error = %e, "automations: a time trigger failed");
                record_error(st, t.id, &format!("database error: {e}")).await;
                report.errors += 1;
            }
        }
    }
    Ok(report)
}

// ---------- event triggers ----------

async fn fire_events(tx: &mut PgConnection, t: &Due, now: DateTime<Utc>, settle_secs: f64) -> Result<Fired, sqlx::Error> {
    let lim = runs::limits(tx, t.workflow_id, runs::max_per_hour(t.max_per_hour), now).await?;
    if !lim.open() {
        mark_held(tx, t.id, lim.remaining <= 0, now).await?;
        return Ok(Fired::Held);
    }
    let Some(source) = events::source_of(&t.trigger_type) else {
        sqlx::query("UPDATE automation_triggers SET last_error = $2 WHERE id = $1").bind(t.id).bind(format!("`{}` is not an event trigger.", t.trigger_type)).execute(tx).await?;
        return Ok(Fired::Done { runs: 0 });
    };
    let cursor = match &t.cursor {
        Some(c) => c.clone(),
        None => events::initial_cursor(tx, source, now, 0).await?,
    };
    let app_settle = now - Duration::milliseconds((settle_secs * 1000.0) as i64);
    let batch = events::read_batch(tx, &t.trigger_type, &t.config, &cursor, lim.remaining, app_settle, settle_secs).await?;
    let (mut made, mut deleted) = (0usize, 0i64);
    for ev in &batch.events {
        if !events::early_filter(&t.trigger_type, &t.config, t.workflow_id, ev) {
            continue;
        }
        match events::build_item(tx, &t.trigger_type, &t.config, ev).await? {
            Built::Item(item) => {
                let run = NewRun {
                    workflow_id: t.workflow_id,
                    version_id: t.version_id,
                    trigger_node_id: &t.node_id,
                    trigger_item: Value::Object(item),
                    run_as_user_id: t.run_as_user_id,
                    started_by: None,
                    idempotency_key: Some(format!("trigger:{}:event:{}", t.id, ev.key)),
                };
                if runs::insert_run(tx, now, run).await?.is_some() {
                    made += 1;
                }
            }
            Built::Deleted => deleted += 1,
            Built::Filtered => {}
        }
    }
    sqlx::query(
        "UPDATE automation_triggers SET cursor = $2, skipped_deleted = skipped_deleted + $3, \
           last_fired_at = CASE WHEN $4 > 0 THEN $5 ELSE last_fired_at END, last_error = NULL, rate_limited_at = NULL WHERE id = $1",
    )
    .bind(t.id)
    .bind(&batch.cursor)
    .bind(deleted)
    .bind(made as i64)
    .bind(now)
    .execute(tx)
    .await?;
    Ok(Fired::Done { runs: made })
}

/// Read new events for every event trigger. Events younger than `SETTLE_SECS` wait for the next tick.
pub async fn event_tick(st: &AppState) -> Result<TickReport, sqlx::Error> {
    event_tick_with(st, events::SETTLE_SECS).await
}

/// Like `event_tick`, with your own settle time. Tests use 0.
pub async fn event_tick_with(st: &AppState, settle_secs: f64) -> Result<TickReport, sqlx::Error> {
    let now = st.now();
    let mut report = TickReport::default();
    let mut seen: Vec<Uuid> = Vec::new();
    loop {
        let mut tx = st.pool.begin().await?;
        let row: Option<Due> = sqlx::query_as(&format!(
            "{SELECT} WHERE t.kind = 'event' AND w.active AND w.archived_at IS NULL AND t.id <> ALL($1) ORDER BY t.id LIMIT 1 FOR UPDATE OF t SKIP LOCKED"
        ))
        .bind(&seen)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(t) = row else { break };
        seen.push(t.id);
        match fire_events(&mut tx, &t, now, settle_secs).await {
            Ok(Fired::Done { runs }) => {
                tx.commit().await?;
                report.fired += usize::from(runs > 0);
                report.runs += runs;
            }
            Ok(Fired::Held) => {
                tx.commit().await?;
                report.held += 1;
            }
            Err(e) => {
                drop(tx);
                tracing::warn!(trigger = %t.id, workflow = %t.workflow_id, error = %e, "automations: an event trigger failed");
                record_error(st, t.id, &format!("database error: {e}")).await;
                report.errors += 1;
            }
        }
    }
    Ok(report)
}

/// Both ticks once. The developer panel calls this (`automations_trigger_tick`).
pub async fn tick_all(st: &AppState) -> Result<(TickReport, TickReport), sqlx::Error> {
    Ok((time_tick(st).await?, event_tick(st).await?))
}

/// The trigger loop of this server copy.
pub async fn run(st: AppState, cancel: CancellationToken) {
    let mut last_time: Option<tokio::time::Instant> = None;
    let mut last_event: Option<tokio::time::Instant> = None;
    loop {
        let every = match crate::automations::settings::load(&st.pool).await {
            Ok(s) => StdDuration::from_secs(s.event_tick_secs.clamp(1, 300) as u64),
            Err(_) => StdDuration::from_secs(5),
        };
        if last_time.is_none_or(|t| t.elapsed() >= TIME_TICK) {
            last_time = Some(tokio::time::Instant::now());
            if let Err(e) = time_tick(&st).await {
                tracing::error!(error = %e, "automations: the time trigger tick failed");
            }
        }
        if last_event.is_none_or(|t| t.elapsed() >= every) {
            last_event = Some(tokio::time::Instant::now());
            if let Err(e) = event_tick(&st).await {
                tracing::error!(error = %e, "automations: the event trigger tick failed");
            }
        }
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(StdDuration::from_secs(1)) => {}
        }
    }
}
