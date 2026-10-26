//! Automations: a node editor for email work (build map: `AIKIDO_EMAIL_Automations_Build_Map.md`).
//!
//! Phases 1 to 3 are built: the workflow file format, the node catalog, the validator, the expression
//! engine, the routes to save drafts, the engine (test runs), and the nodes that talk to the outside
//! (AIKIDO nodes, HTTP Request, Fetch web page, secrets, settings).
//!
//! - `definition.rs`: `WorkflowDefinition` and its parts, canonical JSON, `draft_hash`
//! - `schema_kit.rs`: a tiny builder for the JSON Schema of node params
//! - `registry.rs`: `NodeType`, `NodeSpec`, the registry
//! - `catalog.rs`: the spec of every node type (a node that is not built yet is a stub: `run` answers `not_implemented`)
//! - `internal_call.rs`: calls to the app's own routes, the API operation index and the deny list
//! - `actor.rs`: tags lead events with the workflow that made the change
//! - `secrets.rs`, `settings.rs`, `robots.rs`: secrets with redaction, the settings, the robots.txt reader
//! - `expr.rs`: `${{ }}` expressions (MiniJinja)
//! - `validate.rs`: every rule of the build map, sections 4.2 to 4.8
//! - `routes.rs`: `/v1/automations/*` (scope `automations`)
//! - `transfer.rs`, `transfer_routes.rs`, `templates.rs`, `workprocess.rs`: export, import (with fix-ups), the 4 templates, `workprocess.md`
pub mod actor;
pub mod catalog;
pub mod definition;
pub mod editor;
pub mod engine;
pub mod expr;
pub mod internal_call;
pub mod production;
pub mod registry;
pub mod robots;
pub mod routes;
pub mod schema_kit;
pub mod secrets;
pub mod settings;
pub mod templates;
#[cfg(test)]
pub mod testkit;
pub mod transfer;
pub mod transfer_routes;
pub mod validate;
pub mod workprocess;
