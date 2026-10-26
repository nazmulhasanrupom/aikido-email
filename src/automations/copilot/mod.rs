//! The AI box (build map section 16, phase 8): a chat that builds and changes workflows.
//!
//! - `diff.rs`: the diff of two drafts, and keeping node positions and pins (pure)
//! - `prompt.rs`: the system text, the state block, the rolling summary rules, the rate limit (pure)
//! - `turn.rs`: one model call with parsing and one repair try (works on `LlmClient`, so tests use `FakeLlm`)
//! - `routes.rs`: threads, messages, undo (`/v1/automations/copilot/*`)
//!
//! The system prompt is `../copilot_prompt.md` (Appendix B), embedded.
pub mod diff;
pub mod prompt;
pub mod routes;
pub mod turn;
