//! Email Generator: an AI writes the personal parts ("slots", for example `{{ai:opener}}`) of a sequence
//! step, for each lead, a few hours before the step is due. The follow-up scheduler then sends it like any
//! other step. Build map: "Email Generator Build Map".
//!
//! The AI never writes a whole email in v1. Your words stay around the slots, so the signature, the offer and
//! the unsubscribe line are always yours.
//!
//! - `llm/`: one trait (`LlmClient`), three kinds: Anthropic, OpenAI-compatible, and a fake for tests
//! - `settings.rs`: `GeneratorSettings`
//! - `slots.rs`, `hash.rs`: the slots of a step and the hash that tells when a step changed
//! - `guard.rs`, `parse.rs`, `prompt.rs`: pure functions (checks, JSON reader, prompt builder)
//! - `pipeline.rs`: prompt -> call -> parse -> guard -> one repair try
//! - `worker.rs`: the tick loop that fills the slots
//! - `hooks.rs`: what the follow-up engine tells the generator (stop, resume)
//! - `draft.rs`: the sequence writer and the preview
//! - `routes.rs`, `generations.rs`: the API
//!
//! Rule from every map: no key, no prompt text and no slot text in the logs. Only ids, token counts and codes.
pub mod draft;
pub mod pipeline;
pub mod generations;
pub mod guard;
pub mod hash;
pub mod hooks;
pub mod llm;
pub mod parse;
pub mod prompt;
pub mod providers;
pub mod repo;
pub mod routes;
pub mod settings;
pub mod slots;
pub mod worker;
