//! Making production runs, and the limits that hold them back (build map 9.3 and 11.5).
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

/// A workflow with this many queued production runs gets no new ones from its triggers (back-pressure).
pub const BACKLOG_LIMIT: i64 = 500;
/// The default of the workflow setting `max_runs_per_hour`.
pub const DEFAULT_MAX_PER_HOUR: i64 = 1000;

pub struct Limits {
    /// How many more runs may be made now before `max_runs_per_hour` is reached.
    pub remaining: i64,
    /// The workflow has `BACKLOG_LIMIT` or more runs waiting in the queue.
    pub backlog: bool,
}

impl Limits {
    pub fn open(&self) -> bool {
        self.remaining > 0 && !self.backlog
    }
}

pub async fn limits(conn: &mut PgConnection, workflow_id: Uuid, max_per_hour: i64, now: DateTime<Utc>) -> Result<Limits, sqlx::Error> {
    let (used, queued): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE queued_at > $2), count(*) FILTER (WHERE status = 'queued') \
           FROM automation_runs WHERE workflow_id = $1 AND mode = 'production' AND (queued_at > $2 OR status = 'queued')",
    )
    .bind(workflow_id)
    .bind(now - Duration::hours(1))
    .fetch_one(conn)
    .await?;
    Ok(Limits { remaining: (max_per_hour - used).max(0), backlog: queued >= BACKLOG_LIMIT })
}

/// The `max_runs_per_hour` of a version (from its settings).
pub fn max_per_hour(settings: Option<i32>) -> i64 {
    settings.map_or(DEFAULT_MAX_PER_HOUR, i64::from).clamp(1, 100_000)
}

pub struct NewRun<'a> {
    pub workflow_id: Uuid,
    pub version_id: Uuid,
    pub trigger_node_id: &'a str,
    pub trigger_item: Value,
    /// The publisher of the version. Production runs act as this user.
    pub run_as_user_id: Option<Uuid>,
    /// Who pressed a button (manual runs, retries).
    pub started_by: Option<Uuid>,
    /// A repeat with the same key makes no second run (`trigger:<id>:event:<event id>`).
    pub idempotency_key: Option<String>,
}

/// Queue a production run. `None`: a run with this idempotency key exists already.
pub async fn insert_run(conn: &mut PgConnection, now: DateTime<Utc>, r: NewRun<'_>) -> Result<Option<Uuid>, sqlx::Error> {
    let id = Uuid::now_v7();
    let made: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO automation_runs (id, workflow_id, version_id, mode, status, trigger_node_id, trigger_item, options, started_by, run_as_user_id, idempotency_key, queued_at) \
         VALUES ($1, $2, $3, 'production', 'queued', $4, $5, $6, $7, $8, $9, $10) ON CONFLICT (idempotency_key) DO NOTHING RETURNING id",
    )
    .bind(id)
    .bind(r.workflow_id)
    .bind(r.version_id)
    .bind(r.trigger_node_id)
    .bind(r.trigger_item)
    .bind(json!({}))
    .bind(r.started_by)
    .bind(r.run_as_user_id)
    .bind(r.idempotency_key)
    .bind(now)
    .fetch_optional(conn)
    .await?;
    Ok(made)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hourly_limit_comes_from_the_settings() {
        assert_eq!(max_per_hour(None), 1000);
        assert_eq!(max_per_hour(Some(20)), 20);
        assert_eq!(max_per_hour(Some(0)), 1);
        assert_eq!(max_per_hour(Some(1_000_000)), 100_000);
    }

    #[test]
    fn limits_are_open_only_with_room_and_no_backlog() {
        assert!(Limits { remaining: 1, backlog: false }.open());
        assert!(!Limits { remaining: 0, backlog: false }.open());
        assert!(!Limits { remaining: 5, backlog: true }.open());
    }
}
