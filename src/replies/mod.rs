//! Reply Check: read the inboxes of connected accounts, find mail that comes back from our sends
//! (replies, auto-replies, bounces, unsubscribes, complaints), and act on it.
//! Read only: this code never marks, moves, deletes or flags mail.
pub mod actions;
pub mod classify;
pub mod dsn;
pub mod fetchers;
pub mod ingest;
pub mod matcher;
pub mod model;
pub mod parse;
pub mod quote;
pub mod poller;
pub mod repo;
pub mod routes;
pub mod settings;
pub mod sync;
