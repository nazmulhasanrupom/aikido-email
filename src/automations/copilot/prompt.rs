//! What the copilot sends to the model (build map 16.2 and 16.3), and the two rules around it: when old turns
//! are folded into the summary, and the rate limit. Pure functions: no database, no network.
//!
//! The fixed part (Appendix B prompt, then `workprocess.md` with the install part) goes in `system`, so
//! Anthropic prompt caching works (the client sends `system` as a block with `cache_control`).
//! Everything that changes per turn (summary, chat, state, the new message) goes in `user`.
use crate::automations::transfer::InstallNames;
use crate::automations::validate::ValidationResult;
use crate::automations::workprocess;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::fmt::Write;

/// Appendix B, embedded.
pub const PROMPT: &str = include_str!("../copilot_prompt.md");
/// Old turns are folded into the summary when the unfolded chat is bigger than this (estimated tokens).
pub const FOLD_TOKENS: usize = 24_000;
/// After a fold, this many of the newest messages stay in the window.
pub const KEEP_MESSAGES: usize = 12;
/// Longest user message, in characters.
pub const MAX_MESSAGE_CHARS: usize = 8000;
/// Messages per minute per user.
pub const RATE_PER_MIN: usize = 10;
/// The draft in the state block is cut after this many characters.
const MAX_STATE_DRAFT_CHARS: usize = 200_000;

/// A rough token count: 4 characters per token. Enough to decide when to fold.
pub fn estimate_tokens(s: &str) -> usize {
    s.chars().count().div_ceil(4)
}

/// The fixed system text: the prompt, then the merged `workprocess.md` (with the names of this install).
pub fn system_text(install: Option<&InstallNames>) -> String {
    format!("{}\n\n---\n\n{}", PROMPT.trim_end(), workprocess::workprocess(install))
}

// ---------- the rolling summary ----------

/// The summary is stored in `automation_copilot_threads.summary` as `<!--upto:<message id>-->\n<text>`.
/// The id says which messages are already in it (so the table needs no new column).
pub fn split_summary(raw: &str) -> (i64, &str) {
    if let Some(rest) = raw.strip_prefix("<!--upto:") {
        if let Some((n, text)) = rest.split_once("-->") {
            if let Ok(n) = n.parse::<i64>() {
                return (n, text.strip_prefix('\n').unwrap_or(text));
            }
        }
    }
    (0, raw)
}

pub fn join_summary(upto: i64, text: &str) -> String {
    format!("<!--upto:{upto}-->\n{}", text.trim())
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatMsg {
    pub id: i64,
    /// `user` or `assistant`
    pub role: String,
    pub content: String,
}

#[derive(Debug, PartialEq)]
pub enum Window {
    /// Send these messages as they are.
    Keep(Vec<ChatMsg>),
    /// The chat is too long: fold `old` into the summary, then send `keep`.
    Fold { old: Vec<ChatMsg>, keep: Vec<ChatMsg> },
}

/// `msgs` are the messages not yet in the summary, oldest first.
pub fn plan_window(summary_text: &str, msgs: Vec<ChatMsg>) -> Window {
    let total = estimate_tokens(summary_text) + msgs.iter().map(|m| estimate_tokens(&m.content) + 4).sum::<usize>();
    if total > FOLD_TOKENS && msgs.len() > KEEP_MESSAGES {
        let mut old = msgs;
        let keep = old.split_off(old.len() - KEEP_MESSAGES);
        return Window::Fold { old, keep };
    }
    Window::Keep(msgs)
}

pub const SUMMARY_SYSTEM: &str = "You keep the memory of a chat between a user and the Automations assistant of AIKIDO EMAIL. \
Write a short summary (at most 400 words, plain text, no JSON) of the old turns you get. Keep: what the user wants, the decisions made, \
the names, ids and variable names used, what was built and what is still open. Merge it with the earlier summary if there is one. \
Do not use em dashes or en dashes.";

pub fn summary_user(prev: &str, old: &[ChatMsg]) -> String {
    let mut s = String::new();
    if !prev.trim().is_empty() {
        let _ = write!(s, "## Earlier summary\n{}\n\n", prev.trim());
    }
    s.push_str("## Old turns to fold in\n");
    s.push_str(&chat_text(old));
    s
}

pub fn chat_text(msgs: &[ChatMsg]) -> String {
    let mut s = String::new();
    for m in msgs {
        let _ = writeln!(s, "{}: {}\n", m.role, m.content.trim());
    }
    s
}

// ---------- the state block ----------

#[derive(Debug, Clone, Default)]
pub struct TestNode {
    pub node_id: String,
    pub status: String,
    pub input_count: i64,
    /// Items per port, for example `{ "main": 3 }`.
    pub output_count: Value,
    pub error: Option<Value>,
}

#[derive(Debug, Clone, Default)]
pub struct TestRunInfo {
    pub run_id: String,
    pub status: String,
    pub error: Option<Value>,
    pub nodes: Vec<TestNode>,
}

fn error_line(e: &Value) -> String {
    let code = e.get("code").and_then(Value::as_str).unwrap_or("error");
    let msg = e.get("message").and_then(Value::as_str).map_or_else(|| e.to_string(), str::to_string);
    let msg: String = msg.chars().take(400).collect();
    format!("{code}: {}", msg.replace('\n', " "))
}

/// The draft without `pins` (test data can be huge and is kept by the server anyway), as compact JSON.
fn draft_text(draft: &Value) -> (String, Vec<String>) {
    let mut d = draft.clone();
    let pinned: Vec<String> = d.get("pins").and_then(Value::as_object).map(|p| p.keys().cloned().collect()).unwrap_or_default();
    if let Some(o) = d.as_object_mut() {
        o.remove("pins");
    }
    let mut text = d.to_string();
    if text.chars().count() > MAX_STATE_DRAFT_CHARS {
        text = text.chars().take(MAX_STATE_DRAFT_CHARS).collect::<String>() + " ...(cut)";
    }
    (text, pinned)
}

/// The state block (16.2): the real current draft, its validation, and the newest test run.
/// The install names are not here: they are in the system text.
pub fn state_block(draft: Option<&Value>, validation: Option<&ValidationResult>, test: Option<&TestRunInfo>) -> String {
    let mut s = String::from("## STATE\n\n");
    let Some(draft) = draft else {
        s.push_str("No workflow is open. If the user wants one, use action \"create\".\n");
        return s;
    };
    let (text, pinned) = draft_text(draft);
    let _ = write!(s, "### Current workflow (JSON)\n{text}\n\n");
    if !pinned.is_empty() {
        let _ = write!(s, "Saved test data (pins) exists on: {}. It is not shown; the server keeps it.\n\n", pinned.join(", "));
    }
    s.push_str("### Validation of the current workflow\n");
    match validation {
        Some(v) if v.errors.is_empty() && v.warnings.is_empty() => s.push_str("ok, no errors, no warnings.\n"),
        Some(v) => {
            let _ = writeln!(s, "{}", if v.ok { "ok (warnings only)" } else { "NOT ok" });
            for (kind, list) in [("error", &v.errors), ("warning", &v.warnings)] {
                for i in list.iter().take(30) {
                    let _ = writeln!(s, "- {kind} [{}]{}{}: {}", i.code, i.node_id.as_deref().map_or(String::new(), |n| format!(" node `{n}`")), i.path.as_deref().map_or(String::new(), |p| format!(" at {p}")), i.message);
                }
                if list.len() > 30 {
                    let _ = writeln!(s, "- ...and {} more {kind}s", list.len() - 30);
                }
            }
        }
        None => s.push_str("(not checked)\n"),
    }
    s.push_str("\n### Newest test run\n");
    match test {
        None => s.push_str("There is no test run yet.\n"),
        Some(t) => {
            let _ = writeln!(s, "status: {}", t.status);
            if let Some(e) = &t.error {
                let _ = writeln!(s, "run error: {}", error_line(e));
            }
            for n in t.nodes.iter().take(100) {
                let _ = write!(s, "- `{}`: {}, items in {}, items out {}", n.node_id, n.status, n.input_count, n.output_count);
                if let Some(e) = &n.error {
                    let _ = write!(s, ", ERROR {}", error_line(e));
                }
                s.push('\n');
            }
        }
    }
    s
}

/// The `user` text of one turn.
pub fn user_text(summary: &str, chat: &[ChatMsg], state: &str, new_message: &str) -> String {
    let mut s = String::new();
    if !summary.trim().is_empty() {
        let _ = write!(s, "## Summary of older turns\n{}\n\n", summary.trim());
    }
    if !chat.is_empty() {
        let _ = write!(s, "## Chat so far\n{}\n", chat_text(chat));
    }
    s.push_str(state);
    let _ = write!(s, "\n## New message from the user\n{}\n", new_message.trim());
    s
}

// ---------- rate limit ----------

/// `times`: when this user sent messages. `Err(seconds)`: the limit is reached, try again after that many seconds.
pub fn rate_check(times: &[DateTime<Utc>], now: DateTime<Utc>) -> Result<(), i64> {
    let mut recent: Vec<&DateTime<Utc>> = times.iter().filter(|t| now.signed_duration_since(**t).num_seconds() < 60 && **t <= now).collect();
    if recent.len() < RATE_PER_MIN {
        return Ok(());
    }
    recent.sort();
    // the limit opens when the message that is `RATE_PER_MIN` back leaves the minute
    let blocking = recent[recent.len() - RATE_PER_MIN];
    Err((60 - now.signed_duration_since(*blocking).num_seconds()).clamp(1, 60))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automations::validate::Issue;
    use chrono::Duration;
    use serde_json::json;

    fn msg(id: i64, role: &str, n: usize) -> ChatMsg {
        ChatMsg { id, role: role.into(), content: "x".repeat(n) }
    }

    #[test]
    fn the_summary_remembers_how_far_it_goes() {
        let raw = join_summary(42, "  They want a lead flow.  ");
        assert_eq!(split_summary(&raw), (42, "They want a lead flow."));
        assert_eq!(split_summary("plain"), (0, "plain"));
        assert_eq!(split_summary(""), (0, ""));
        assert_eq!(split_summary("<!--upto:x-->\nabc"), (0, "<!--upto:x-->\nabc"));
    }

    #[test]
    fn a_short_chat_is_kept_whole() {
        let msgs: Vec<ChatMsg> = (1..=30).map(|i| msg(i, "user", 40)).collect();
        assert_eq!(plan_window("", msgs.clone()), Window::Keep(msgs));
    }

    #[test]
    fn a_long_chat_folds_all_but_the_last_12() {
        // 20 messages of 8000 characters = 40000 tokens
        let msgs: Vec<ChatMsg> = (1..=20).map(|i| msg(i, if i % 2 == 1 { "user" } else { "assistant" }, 8000)).collect();
        match plan_window("old summary", msgs) {
            Window::Fold { old, keep } => {
                assert_eq!(old.len(), 8);
                assert_eq!(keep.len(), KEEP_MESSAGES);
                assert_eq!(old.last().unwrap().id, 8);
                assert_eq!(keep.first().unwrap().id, 9);
            }
            w => panic!("expected a fold, got {w:?}"),
        }
        // a few huge messages are not folded: there is nothing older than the window
        let few: Vec<ChatMsg> = (1..=3).map(|i| msg(i, "user", 40_000)).collect();
        assert!(matches!(plan_window("", few), Window::Keep(_)));
    }

    #[test]
    fn the_summary_call_gets_the_old_turns_and_the_earlier_summary() {
        let u = summary_user("Earlier text", &[ChatMsg { id: 1, role: "user".into(), content: "Make a flow".into() }]);
        assert!(u.contains("Earlier text") && u.contains("user: Make a flow"));
        assert!(!summary_user("", &[]).contains("Earlier summary"));
    }

    #[test]
    fn the_system_text_starts_with_the_prompt_then_the_guide() {
        let s = system_text(None);
        assert!(s.starts_with("You are the Automations assistant"));
        let i = s.find("AIKIDO EMAIL Workflow Guide").expect("the guide follows the prompt");
        assert!(i > PROMPT.len() - 5);
        assert!(!s.contains("<!-- NODE_CATALOG -->"), "the catalog is merged");
        let names = InstallNames { campaigns: vec![("c-1".into(), "Spring push".into())], ..Default::default() };
        let with = system_text(Some(&names));
        assert!(with.contains("Spring push") && with.contains("c-1"));
        assert!(with.starts_with(&s[..2000]), "the fixed part comes first, so the cache holds");
    }

    #[test]
    fn the_state_block_has_the_draft_the_validation_and_the_newest_test_errors() {
        let draft = json!({ "name": "T", "nodes": [{ "id": "a", "type": "trigger.manual" }], "edges": [], "pins": { "a": [{ "big": "data" }] } });
        let v = ValidationResult { ok: false, errors: vec![Issue { code: "unknown_reference".into(), node_id: Some("b".into()), path: Some("params.url".into()), message: "no node `x`".into() }], warnings: vec![] };
        let t = TestRunInfo {
            run_id: "r1".into(),
            status: "error".into(),
            error: None,
            nodes: vec![
                TestNode { node_id: "a".into(), status: "success".into(), input_count: 0, output_count: json!({ "main": 3 }), error: None },
                TestNode { node_id: "b".into(), status: "error".into(), input_count: 3, output_count: json!({}), error: Some(json!({ "code": "http_status", "message": "got 500" })) },
            ],
        };
        let s = state_block(Some(&draft), Some(&v), Some(&t));
        assert!(s.contains(r#""nodes":[{"id":"a""#));
        assert!(!s.contains("big"), "pins are not shown");
        assert!(s.contains("Saved test data (pins) exists on: a"));
        assert!(s.contains("NOT ok") && s.contains("[unknown_reference] node `b`"));
        assert!(s.contains("`a`: success, items in 0, items out {\"main\":3}"));
        assert!(s.contains("`b`: error") && s.contains("ERROR http_status: got 500"));
        assert!(state_block(None, None, None).contains("No workflow is open"));
        assert!(state_block(Some(&draft), None, None).contains("There is no test run yet"));
    }

    #[test]
    fn the_user_text_orders_summary_chat_state_message() {
        let chat = vec![ChatMsg { id: 1, role: "assistant".into(), content: "Hi".into() }];
        let u = user_text("S", &chat, "## STATE\n\nx\n", " Do it ");
        let (a, b, c, d) = (u.find("Summary of older").unwrap(), u.find("Chat so far").unwrap(), u.find("## STATE").unwrap(), u.find("New message").unwrap());
        assert!(a < b && b < c && c < d);
        assert!(u.ends_with("Do it\n"));
    }

    #[test]
    fn ten_messages_a_minute() {
        let now = Utc::now();
        let times: Vec<DateTime<Utc>> = (0..9).map(|i| now - Duration::seconds(i * 3)).collect();
        assert_eq!(rate_check(&times, now), Ok(()));
        let mut ten = times.clone();
        ten.push(now - Duration::seconds(40));
        let wait = rate_check(&ten, now).unwrap_err();
        assert!((1..=60).contains(&wait), "{wait}");
        assert_eq!(wait, 20, "the oldest message leaves the minute in 20 s");
        // old messages do not count
        let old: Vec<DateTime<Utc>> = (0..20).map(|i| now - Duration::seconds(61 + i)).collect();
        assert_eq!(rate_check(&old, now), Ok(()));
    }
}
