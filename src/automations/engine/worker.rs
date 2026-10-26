//! The worker (build map 8.2): it claims queued runs with a lease, runs them, wakes waiting runs,
//! takes back runs of a dead server copy, and cleans old runs.
use super::env::NodeEnv;
use super::eval::{RunFacts, RunMode};
use super::run::{Outcome, RunSpec, execute};
use crate::automations::registry::Item;
use std::collections::BTreeMap;
use super::state::RunState;
use super::store::{NodeRecord, RunStore, StoreError};
use super::Control;
use crate::automations::definition::WorkflowDefinition;
use crate::automations::internal_call::{CallRequest, CallResponse, Internal, RouterCaller, Who};
use crate::automations::registry::NodeError;
use crate::users::model::Role;
use crate::state::AppState;
use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde_json::{Map, Value, json};
use sqlx::{FromRow, PgPool};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// How long a claim is good for. The worker renews it every 20 seconds while it runs.
const LEASE_SECS: i64 = 60;
const RENEW_EVERY: Duration = Duration::from_secs(20);
const DEFAULT_RETENTION_DAYS: i64 = 30;
const DEFAULT_TEST_RUNS_KEPT: i64 = 20;
/// Test runs older than this are deleted, whatever the count.
const TEST_RUN_MAX_AGE_DAYS: i64 = 7;
const KEEP_DRAFT_HISTORY: i64 = 50;

/// The id of this server copy, in `claimed_by`.
static WORKER_ID: LazyLock<String> = LazyLock::new(|| format!("automations-{}", Uuid::now_v7()));
static LAST_TICK: AtomicI64 = AtomicI64::new(0);
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Called by `main` when this copy runs the worker.
pub fn mark_enabled(on: bool) {
    ENABLED.store(on, Ordering::SeqCst);
}
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}
/// When this copy last looked for runs.
pub fn last_tick() -> Option<DateTime<Utc>> {
    let t = LAST_TICK.load(Ordering::SeqCst);
    (t > 0).then(|| DateTime::from_timestamp(t, 0)).flatten()
}

pub struct WorkerOpts {
    pub tick: Duration,
    pub max_parallel: usize,
    pub node_timeout: Duration,
}

impl Default for WorkerOpts {
    fn default() -> Self {
        WorkerOpts { tick: Duration::from_millis(500), max_parallel: 10, node_timeout: Duration::from_secs(120) }
    }
}

impl WorkerOpts {
    /// `AIKIDO_AUTOMATIONS_TICK_MS` (default 500) and `AIKIDO_AUTOMATIONS_MAX_PARALLEL` (default 10).
    pub fn from_env() -> Self {
        let get = |k: &str, d: u64| std::env::var(k).ok().and_then(|v| v.trim().parse::<u64>().ok()).filter(|v| *v > 0).unwrap_or(d);
        WorkerOpts {
            tick: Duration::from_millis(get("AIKIDO_AUTOMATIONS_TICK_MS", 500)),
            max_parallel: get("AIKIDO_AUTOMATIONS_MAX_PARALLEL", 10) as usize,
            node_timeout: Duration::from_secs(120),
        }
    }
}

/// Is `AIKIDO_AUTOMATIONS_ENABLED` on? (Default on.)
pub fn enabled_by_env() -> bool {
    !matches!(std::env::var("AIKIDO_AUTOMATIONS_ENABLED").ok().as_deref().map(str::trim), Some("false" | "0" | "no" | "off"))
}

// ---------- settings ----------

pub struct EngineSettings {
    pub paused: bool,
    pub retention_days: i64,
    pub test_runs_kept: i64,
}

pub async fn engine_settings(pool: &PgPool) -> Result<EngineSettings, sqlx::Error> {
    let row: Option<(Value, bool)> = sqlx::query_as("SELECT settings, paused FROM automation_settings LIMIT 1").fetch_optional(pool).await?;
    let (s, paused) = row.unwrap_or((json!({}), false));
    let num = |k: &str, d: i64, lo: i64, hi: i64| s.get(k).and_then(Value::as_i64).unwrap_or(d).clamp(lo, hi);
    Ok(EngineSettings { paused, retention_days: num("retention_days", DEFAULT_RETENTION_DAYS, 1, 365), test_runs_kept: num("test_runs_kept", DEFAULT_TEST_RUNS_KEPT, 1, 100) })
}

// ---------- the database store ----------

pub struct DbStore {
    pool: PgPool,
    run_id: Uuid,
    worker: String,
    clock: crate::state::Clock,
}

impl DbStore {
    pub fn new(st: &AppState, run_id: Uuid, worker: &str) -> Self {
        DbStore { pool: st.pool.clone(), run_id, worker: worker.to_string(), clock: st.clock.clone() }
    }
}

#[async_trait]
impl RunStore for DbStore {
    async fn node_started(&self, node_id: &str, seq: u32, at: DateTime<Utc>) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO automation_run_nodes (run_id, node_id, seq, status, started_at) VALUES ($1, $2, $3, 'running', $4) \
             ON CONFLICT (run_id, node_id, seq) DO UPDATE SET status = 'running', started_at = $4, finished_at = NULL",
        )
        .bind(self.run_id)
        .bind(node_id)
        .bind(seq as i32)
        .bind(at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn step_done(&self, records: &[NodeRecord], state: &RunState) -> Result<(), StoreError> {
        let bytes = state.encode().map_err(|e| if e == "run_state_too_large" { StoreError::StateTooLarge } else { StoreError::Db(e) })?;
        let mut tx = self.pool.begin().await?;
        for r in records {
            sqlx::query(
                "INSERT INTO automation_run_nodes (run_id, node_id, seq, status, input_count, output_count, output, truncated, error, started_at, finished_at) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
                 ON CONFLICT (run_id, node_id, seq) DO UPDATE SET status = $4, input_count = $5, output_count = $6, output = $7, \
                   truncated = $8, error = $9, started_at = $10, finished_at = $11",
            )
            .bind(self.run_id)
            .bind(&r.node_id)
            .bind(r.seq as i32)
            .bind(r.status.as_str())
            .bind(r.input_count as i32)
            .bind(serde_json::to_value(&r.output_count).unwrap_or(json!({})))
            .bind(&r.output)
            .bind(r.truncated)
            .bind(&r.error)
            .bind(r.started_at)
            .bind(r.finished_at)
            .execute(&mut *tx)
            .await?;
        }
        let lease = (self.clock)() + ChronoDuration::seconds(LEASE_SECS);
        let n = sqlx::query("UPDATE automation_runs SET state = $2, active_ms = $3, lease_until = $4 WHERE id = $1 AND claimed_by = $5 AND status = 'running'")
            .bind(self.run_id)
            .bind(bytes)
            .bind(state.active_ms as i64)
            .bind(lease)
            .bind(&self.worker)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if n == 0 {
            return Err(StoreError::LeaseLost);
        }
        tx.commit().await?;
        Ok(())
    }

    async fn event(&self, kind: &str, node_id: Option<&str>, detail: Value) {
        let r = sqlx::query("INSERT INTO automation_run_events (run_id, at, kind, node_id, detail) VALUES ($1, $2, $3, $4, $5)")
            .bind(self.run_id)
            .bind((self.clock)())
            .bind(kind)
            .bind(node_id)
            .bind(detail)
            .execute(&self.pool)
            .await;
        if let Err(e) = r {
            tracing::warn!(run = %self.run_id, error = %e, "could not write a run event");
        }
    }
}

pub async fn log_event(pool: &PgPool, run_id: Uuid, at: DateTime<Utc>, kind: &str, node_id: Option<&str>, detail: Value) {
    let r = sqlx::query("INSERT INTO automation_run_events (run_id, at, kind, node_id, detail) VALUES ($1, $2, $3, $4, $5)")
        .bind(run_id)
        .bind(at)
        .bind(kind)
        .bind(node_id)
        .bind(detail)
        .execute(pool)
        .await;
    if let Err(e) = r {
        tracing::warn!(run = %run_id, error = %e, "could not write a run event");
    }
}

// ---------- claim ----------

#[derive(FromRow)]
struct Claimed {
    id: Uuid,
    workflow_id: Uuid,
    version_id: Option<Uuid>,
    definition: Option<Value>,
    mode: String,
    trigger_node_id: String,
    trigger_item: Value,
    options: Value,
    state: Option<Vec<u8>>,
    active_ms: i64,
    started_at: Option<DateTime<Utc>>,
    started_by: Option<Uuid>,
    run_as_user_id: Option<Uuid>,
}

/// Take the next queued run. Test runs go first (a person is waiting). A production run waits when the
/// engine is paused, or when its workflow already has `max_concurrent_runs` runs going.
async fn claim(st: &AppState, worker: &str) -> Result<Option<Claimed>, sqlx::Error> {
    let now = st.now();
    let paused = engine_settings(&st.pool).await?.paused;
    let mut tx = st.pool.begin().await?;
    let candidates: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT id, workflow_id, mode FROM automation_runs WHERE status = 'queued' \
         ORDER BY (mode = 'test') DESC, queued_at, id LIMIT 20 FOR UPDATE SKIP LOCKED",
    )
    .fetch_all(&mut *tx)
    .await?;
    for (id, workflow_id, mode) in candidates {
        if mode == "production" {
            if paused {
                continue;
            }
            // one claim at a time per workflow, so the limit holds with several server copies
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 0))").bind(workflow_id.to_string()).execute(&mut *tx).await?;
            let limit: Option<i32> = sqlx::query_scalar(
                "SELECT (COALESCE(r.definition, v.definition) -> 'settings' ->> 'max_concurrent_runs')::int \
                 FROM automation_runs r LEFT JOIN automation_versions v ON v.id = r.version_id WHERE r.id = $1",
            )
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
            let running: i64 = sqlx::query_scalar("SELECT count(*) FROM automation_runs WHERE workflow_id = $1 AND mode = 'production' AND status = 'running'")
                .bind(workflow_id)
                .fetch_one(&mut *tx)
                .await?;
            if running >= i64::from(limit.unwrap_or(5)) {
                continue;
            }
        }
        let row: Claimed = sqlx::query_as(
            "UPDATE automation_runs SET status = 'running', claimed_by = $2, lease_until = $3, started_at = COALESCE(started_at, $4), attempt = attempt + 1 \
             WHERE id = $1 RETURNING id, workflow_id, version_id, definition, mode, trigger_node_id, trigger_item, options, state, active_ms, started_at, started_by, run_as_user_id",
        )
        .bind(id)
        .bind(worker)
        .bind(now + ChronoDuration::seconds(LEASE_SECS))
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        return Ok(Some(row));
    }
    Ok(None)
}

// ---------- housekeeping ----------

/// Wake waiting runs, take back runs of a dead server copy, end cancelled runs that no one is working on.
pub async fn housekeeping(st: &AppState) -> Result<(), sqlx::Error> {
    let now = st.now();
    LAST_TICK.store(now.timestamp(), Ordering::SeqCst);

    let woken: Vec<Uuid> = sqlx::query_scalar(
        "UPDATE automation_runs SET status = 'queued', resume_at = NULL WHERE status = 'waiting' AND resume_at <= $1 AND NOT cancel_requested RETURNING id",
    )
    .bind(now)
    .fetch_all(&st.pool)
    .await?;
    for id in woken {
        log_event(&st.pool, id, now, "resumed", None, json!({})).await;
    }

    let lost: Vec<Uuid> = sqlx::query_scalar(
        "UPDATE automation_runs SET status = 'queued', claimed_by = NULL, lease_until = NULL WHERE status = 'running' AND lease_until < $1 RETURNING id",
    )
    .bind(now)
    .fetch_all(&st.pool)
    .await?;
    for id in lost {
        tracing::warn!(run = %id, "the lease of an automation run ran out; the run is queued again");
        log_event(&st.pool, id, now, "log", None, json!({ "message": "The server that ran this run stopped. The run goes on from its last saved step." })).await;
    }

    sqlx::query("UPDATE automation_runs SET status = 'cancelled', finished_at = $1, claimed_by = NULL, lease_until = NULL, state = NULL WHERE status IN ('queued', 'waiting') AND cancel_requested")
        .bind(now)
        .execute(&st.pool)
        .await?;
    Ok(())
}

/// Delete old runs, expired webhook listens and extra draft history (build map section 12).
pub async fn cleanup(st: &AppState) -> Result<(), sqlx::Error> {
    let now = st.now();
    let s = engine_settings(&st.pool).await?;
    sqlx::query("DELETE FROM automation_runs WHERE mode = 'production' AND status IN ('success', 'error', 'cancelled') AND finished_at < $1")
        .bind(now - ChronoDuration::days(s.retention_days))
        .execute(&st.pool)
        .await?;
    sqlx::query(
        "DELETE FROM automation_runs WHERE mode = 'test' AND status IN ('success', 'error', 'cancelled') AND \
           (COALESCE(finished_at, queued_at) < $1 OR id IN ( \
              SELECT id FROM (SELECT id, status, row_number() OVER (PARTITION BY workflow_id ORDER BY queued_at DESC, id DESC) AS rn \
                              FROM automation_runs WHERE mode = 'test') t WHERE rn > $2))",
    )
    .bind(now - ChronoDuration::days(TEST_RUN_MAX_AGE_DAYS))
    .bind(s.test_runs_kept)
    .execute(&st.pool)
    .await?;
    sqlx::query("DELETE FROM automation_test_listens WHERE until < $1").bind(now).execute(&st.pool).await?;
    sqlx::query(
        "DELETE FROM automation_draft_history WHERE id IN ( \
           SELECT id FROM (SELECT id, row_number() OVER (PARTITION BY workflow_id ORDER BY id DESC) AS rn FROM automation_draft_history) t WHERE rn > $1)",
    )
    .bind(KEEP_DRAFT_HISTORY)
    .execute(&st.pool)
    .await?;
    Ok(())
}

// ---------- process one run ----------

async fn finish(st: &AppState, run_id: Uuid, worker: &str, outcome: &Outcome, active_ms: u64) -> Result<(), sqlx::Error> {
    let now = st.now();
    let (status, error, resume_at, done): (&str, Option<Value>, Option<DateTime<Utc>>, bool) = match outcome {
        Outcome::Success => ("success", None, None, true),
        Outcome::Error(e) => ("error", Some(e.clone()), None, true),
        Outcome::Cancelled => ("cancelled", None, None, true),
        Outcome::Waiting(t) => ("waiting", None, Some(*t), false),
        Outcome::Lost => return Ok(()),
    };
    let done_rows = sqlx::query(
        "UPDATE automation_runs SET status = $2, error = $3, resume_at = $4, finished_at = $5, claimed_by = NULL, lease_until = NULL, active_ms = $6, \
           state = CASE WHEN $7 THEN NULL ELSE state END \
         WHERE id = $1 AND claimed_by = $8",
    )
    .bind(run_id)
    .bind(status)
    .bind(error)
    .bind(resume_at)
    .bind(done.then_some(now))
    .bind(active_ms as i64)
    .bind(done)
    .bind(worker)
    .execute(&st.pool)
    .await?;
    // a production run that failed starts the error workflow of its version (phase 9)
    if let (Outcome::Error(e), 1) = (outcome, done_rows.rows_affected()) {
        super::error_workflow::on_failed(st, run_id, e).await;
    }
    Ok(())
}

async fn fail_early(st: &AppState, row: &Claimed, worker: &str, code: &str, message: &str) -> Result<(), sqlx::Error> {
    log_event(&st.pool, row.id, st.now(), "node_error", None, json!({ "code": code, "message": message })).await;
    finish(st, row.id, worker, &Outcome::Error(super::run::error_object(code, message, None, None, None)), row.active_ms.max(0) as u64).await
}

/// Refuses every call: the run has no user to act as.
struct NoUser;

#[async_trait]
impl Internal for NoUser {
    async fn call(&self, _who: &Who, _req: CallRequest) -> Result<CallResponse, NodeError> {
        Err(NodeError::new("run_as_user_inactive", "The user this run acts as is disabled or does not exist. Publish the workflow again as an active user."))
    }
}

/// The user a run acts as (build map 8.9). A production run acts as the user who published; a test run acts as the
/// user who pressed Test. A test started with an API key has no user: it acts as the oldest active owner, but only
/// with the rights of a member. `None`: there is no active user.
async fn run_as(pool: &sqlx::PgPool, user: Option<Uuid>) -> Result<Option<(Uuid, Role)>, sqlx::Error> {
    match user {
        Some(id) => {
            let row: Option<(String, String)> = sqlx::query_as("SELECT role, status FROM users WHERE id = $1").bind(id).fetch_optional(pool).await?;
            Ok(row.filter(|(_, status)| status == "active").map(|(role, _)| (id, Role::parse(&role).unwrap_or(Role::Member))))
        }
        None => {
            let id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM users WHERE role = 'owner' AND status = 'active' ORDER BY created_at, id LIMIT 1").fetch_optional(pool).await?;
            Ok(id.map(|id| (id, Role::Member)))
        }
    }
}

/// Run one claimed run until it ends, waits, or loses its lease.
async fn process(st: &AppState, row: Claimed, opts: &WorkerOpts, worker: &str) -> Result<(), sqlx::Error> {
    // the definition: the snapshot of a test run, or the published version
    let (def_value, version): (Value, Option<i32>) = match (&row.definition, row.version_id) {
        (Some(d), _) => (d.clone(), None),
        (None, Some(vid)) => {
            let v: Option<(Value, i32)> = sqlx::query_as("SELECT definition, version FROM automation_versions WHERE id = $1").bind(vid).fetch_optional(&st.pool).await?;
            match v {
                Some((d, n)) => (d, Some(n)),
                None => return fail_early(st, &row, worker, "version_not_found", "The published version of this workflow does not exist any more.").await,
            }
        }
        (None, None) => return fail_early(st, &row, worker, "invalid_definition", "The run has no workflow definition.").await,
    };
    let def: WorkflowDefinition = match serde_json::from_value(def_value) {
        Ok(d) => d,
        Err(e) => return fail_early(st, &row, worker, "invalid_definition", &format!("The workflow definition cannot be read: {e}")).await,
    };
    let mut state = match &row.state {
        Some(bytes) => match RunState::decode(bytes) {
            Ok(s) => s,
            Err(e) => return fail_early(st, &row, worker, "invalid_checkpoint", &format!("The saved state of the run cannot be read: {e}")).await,
        },
        None => RunState::default(),
    };
    state.active_ms = state.active_ms.max(row.active_ms.max(0) as u64);

    // nodes that were running when the last server copy died run again
    let interrupted: Vec<(String, i32)> = sqlx::query_as("SELECT node_id, seq FROM automation_run_nodes WHERE run_id = $1 AND status = 'running'").bind(row.id).fetch_all(&st.pool).await?;
    for (node_id, seq) in interrupted {
        log_event(&st.pool, row.id, st.now(), "node_restarted", Some(&node_id), json!({ "seq": seq })).await;
    }

    let mode = if row.mode == "test" { RunMode::Test } else { RunMode::Production };
    let vars: Map<String, Value> = def.variables.iter().map(|v| (v.key.clone(), v.value.clone().unwrap_or(Value::Null))).collect();
    let clock = st.clock.clone();
    let facts = RunFacts {
        run_id: row.id.to_string(),
        mode,
        started_at: row.started_at.unwrap_or_else(|| st.now()),
        workflow_id: row.workflow_id.to_string(),
        workflow_name: def.name.clone(),
        workflow_version: version,
        vars,
        trigger_node_id: row.trigger_node_id.clone(),
        trigger: row.trigger_item.as_object().cloned().unwrap_or_default(),
        real_writes: row.options.get("real_writes").and_then(Value::as_bool).unwrap_or(false),
        clock,
    };
    // who the run acts as, the secrets it can use, and the services of the nodes that talk to the outside
    let user = if row.mode == "test" { row.started_by } else { row.run_as_user_id };
    let principal = run_as(&st.pool, user).await?;
    if principal.is_none() && row.mode != "test" {
        return fail_early(st, &row, worker, "run_as_user_inactive", "The user this run acts as is disabled or does not exist. Publish the workflow again as an active user.").await;
    }
    let mut secrets = crate::automations::secrets::load(st, &crate::automations::secrets::names_used(&def)).await?;
    for (user, name) in crate::automations::secrets::basic_auth_pairs(&def) {
        secrets.add_basic(&user, &name);
    }
    let secrets = Arc::new(secrets);
    let settings = crate::automations::settings::load(&st.pool).await?;
    let who = Who { user_id: principal.map(|p| p.0).unwrap_or_else(Uuid::nil), role: principal.map(|p| p.1).unwrap_or(Role::Member), run_id: row.id.to_string(), workflow_id: row.workflow_id.to_string() };
    let internal: Arc<dyn Internal> = if principal.is_some() { Arc::new(RouterCaller::new(st.clone())) } else { Arc::new(NoUser) };
    let mut env = NodeEnv::new(internal, st.fetcher.clone(), secrets.clone(), settings, st.internal_router.operations(st), who, st.site_url());
    env.test_run = row.mode == "test";
    env.ai = Some(Arc::new(super::ai_backend::DbAi { state: st.clone() }));
    env.workflows = Some(Arc::new(super::sub_workflow::DbWorkflows { state: st.clone() }));
    let mut spec = RunSpec::new(def, facts);
    spec.secrets = secrets;
    spec.env = Some(Arc::new(env));
    spec.only_node = row.options.get("only_node").and_then(Value::as_str).map(str::to_string);
    // a node that runs alone: the data of the nodes before it
    if state.outputs.is_empty() {
        if let Some(seed) = row.options.get("lone_seed").and_then(Value::as_object) {
            let from = row.options.get("lone_input").and_then(|v| v.get("node")).and_then(Value::as_str).map(str::to_string);
            let from_port = row.options.get("lone_input").and_then(|v| v.get("port")).and_then(Value::as_str).unwrap_or("main").to_string();
            for (node_id, ports) in seed {
                let ports: BTreeMap<String, Vec<Item>> = ports
                    .as_object()
                    .map(|o| o.iter().map(|(p, list)| (p.clone(), list.as_array().map(|a| a.iter().filter_map(|x| x.as_object().cloned()).map(Item::new).collect()).unwrap_or_default())).collect())
                    .unwrap_or_default();
                let lineage = super::run::seed_output(&mut state, node_id, ports, if from.as_deref() == Some(node_id) { &from_port } else { "main" });
                if from.as_deref() == Some(node_id) {
                    spec.trigger_lineage = lineage;
                }
            }
        }
    }
    spec.node_timeout = opts.node_timeout;

    let store = DbStore::new(st, row.id, worker);
    let control = Arc::new(Control::new());
    let watcher = {
        let (pool, clock, control, worker, run_id) = (st.pool.clone(), st.clock.clone(), control.clone(), worker.to_string(), row.id);
        tokio::spawn(async move {
            let mut last_renew = tokio::time::Instant::now();
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let r: Result<Option<(bool, Option<String>, String)>, _> =
                    sqlx::query_as("SELECT cancel_requested, claimed_by, status FROM automation_runs WHERE id = $1").bind(run_id).fetch_optional(&pool).await;
                match r {
                    Ok(Some((cancel, Some(by), status))) if by == worker && status == "running" => {
                        if cancel {
                            control.cancel();
                        }
                        if last_renew.elapsed() >= RENEW_EVERY {
                            let lease = clock() + ChronoDuration::seconds(LEASE_SECS);
                            let _ = sqlx::query("UPDATE automation_runs SET lease_until = $2 WHERE id = $1 AND claimed_by = $3").bind(run_id).bind(lease).bind(&worker).execute(&pool).await;
                            last_renew = tokio::time::Instant::now();
                        }
                    }
                    // the run is not ours any more
                    Ok(_) => {
                        control.lose();
                        return;
                    }
                    // a database hiccup: try again
                    Err(_) => {}
                }
            }
        })
    };

    let result = execute(&spec, &mut state, &store, &control).await;
    watcher.abort();
    let outcome = match result {
        Ok(o) => o,
        Err(StoreError::LeaseLost) => Outcome::Lost,
        Err(StoreError::StateTooLarge) => Outcome::Error(super::run::error_object("run_state_too_large", "The data of the run is bigger than 20 MB.", None, None, None)),
        Err(StoreError::Db(e)) => {
            // leave the run as it is; the lease runs out and another try starts from the last saved step
            tracing::error!(run = %row.id, error = %e, "automation run: database error");
            return Ok(());
        }
    };
    tracing::info!(run = %row.id, workflow = %row.workflow_id, mode = %row.mode, outcome = ?outcome, "automation run ended");
    finish(st, row.id, worker, &outcome, state.active_ms).await
}

// ---------- loops ----------

/// Do all the work that is due now: housekeeping, then every claimable run, one after the other.
/// The loop below runs several at once. Tests call this to drive the engine step by step.
pub async fn tick(st: &AppState, opts: &WorkerOpts) -> anyhow::Result<usize> {
    housekeeping(st).await?;
    let mut n = 0;
    while let Some(row) = claim(st, &WORKER_ID).await? {
        process(st, row, opts, &WORKER_ID).await?;
        n += 1;
    }
    Ok(n)
}

/// The worker loop of this server copy.
pub async fn run(st: AppState, cancel: CancellationToken, opts: WorkerOpts) {
    let sem = Arc::new(tokio::sync::Semaphore::new(opts.max_parallel.max(1)));
    let opts = Arc::new(opts);
    let mut last_cleanup: Option<tokio::time::Instant> = None;
    loop {
        if let Err(e) = housekeeping(&st).await {
            tracing::error!(error = %e, "automations: housekeeping failed");
        }
        while let Ok(permit) = sem.clone().try_acquire_owned() {
            match claim(&st, &WORKER_ID).await {
                Ok(Some(row)) => {
                    let (st, opts) = (st.clone(), opts.clone());
                    tokio::spawn(async move {
                        if let Err(e) = process(&st, row, &opts, &WORKER_ID).await {
                            tracing::error!(error = %e, "automations: a run failed to process");
                        }
                        drop(permit);
                    });
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::error!(error = %e, "automations: claim failed");
                    break;
                }
            }
        }
        if last_cleanup.is_none_or(|t| t.elapsed() >= Duration::from_secs(3600)) {
            last_cleanup = Some(tokio::time::Instant::now());
            if let Err(e) = cleanup(&st).await {
                tracing::error!(error = %e, "automations: cleanup failed");
            }
        }
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(opts.tick) => {}
        }
    }
}
