//! Follow-up automation: send a short series of emails (a sequence) to each recipient, and stop at once
//! when they reply, bounce, unsubscribe or complain. Build map: "Follow-Up Automation Build Map".
//!
//! The main idea: a step becomes a `send_jobs` row only when it is due (`scheduler.rs`). A reply between
//! two steps then needs no job to cancel. The sending engine applies its own rules to every step.
//!
//! - `template.rs`: `{{name|fallback}}` rendering (pure)
//! - `schedule.rs`: due time math, business days (pure)
//! - `scheduler.rs`: the tick loop that makes the jobs
//! - `lifecycle.rs`: stop, pause, resume, and the two hooks (`on_lead_stopped`, `on_job_result`)
//! - `enroll.rs`, `routes.rs`: the API
pub mod enroll;
pub mod lifecycle;
pub mod model;
pub mod repo;
pub mod routes;
pub mod schedule;
pub mod scheduler;
pub mod settings;
pub mod template;

pub use lifecycle::{JobOutcome, LeadStop, LeadStopReason, LeadStopResult, on_account_deleted, on_job_result, on_lead_stopped};
