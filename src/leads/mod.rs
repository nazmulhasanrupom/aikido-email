//! Lead management: one row per email address, custom fields, lists, tags, bulk import and export,
//! email checks, and the timeline. Build map: "Lead Management Build Map".
//!
//! - `email.rs`: normalize and check an address (pure)
//! - `model.rs`: the lead types and enums
//! - `settings.rs`, `fields.rs`: settings and custom field definitions
//! - `input.rs`: one lead from the outside world, checked
//! - `upsert.rs`: `upsert_batch`, the one function that writes leads in bulk (API, CSV, later Auto Database)
//! - `query.rs`: the filter that the list, export and bulk actions share
//! - `events.rs`: the timeline
//! - `import.rs`, `export.rs`, `verify.rs`: CSV in, CSV out, MX check
//! - `routes.rs`: the API (scope `leads`)
pub mod backfill;
pub mod context;
pub mod email;
pub mod events;
pub mod export;
pub mod fields;
pub mod hooks;
pub mod import;
pub mod input;
pub mod mapping;
pub mod model;
pub mod query;
pub mod repo;
pub mod routes;
pub mod settings;
pub mod upsert;
pub mod vars;
pub mod verify;
