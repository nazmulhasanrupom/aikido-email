//! Production (build map sections 9, 10.3, 10.4 and 13): publish, versions, activate, and the triggers that
//! start runs by themselves.
//!
//! - `schedule.rs`: next times of schedule and cron triggers (pure)
//! - `plan.rs`: which `automation_triggers` rows a version needs (pure)
//! - `events.rs`: event triggers: read new events with a cursor, filter, build the trigger item
//! - `runs.rs`: make production runs; `max_runs_per_hour` and back-pressure
//! - `tick.rs`: the loops: time triggers every 15 s, event triggers every `event_tick_secs`
//! - `routes.rs`: publish, activate, deactivate, versions, draft history, manual production run, retry, cron preview
//! - `webhooks.rs`: `/hooks/{path}`, `/hooks/test/{path}` and the listen route
pub mod events;
pub mod plan;
pub mod routes;
pub mod runs;
pub mod schedule;
pub mod tick;
pub mod webhooks;
