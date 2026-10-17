//! `/v1/ui/*`: small helpers for the web app. Config, onboarding, alerts, workspace.
//! A key (any scope) or a web session can read. Changes need an owner or admin (or an `admin` key).
pub mod alerts;
pub mod routes;
