//! One copilot turn against a model (build map 16.3): ask, read the answer, check the workflow, and make one
//! repair try when the answer or the workflow is bad. It talks to `LlmClient` only, so tests use `FakeLlm`.
//! It does not touch the database; the routes write `llm_calls` from `Turn::calls`.
use crate::automations::validate::{Issue, ValidationResult};
use crate::generator::llm::{LlmClient, LlmError, LlmRequest, LlmResponse};
use serde_json::Value;
use std::time::{Duration, Instant};

const MAX_OUTPUT_TOKENS: u32 = 16_000;
const TIMEOUT_SECS: u64 = 180;
/// The bad answer sent back for the repair try is cut to this many characters.
const MAX_REPAIR_ECHO: usize = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Create,
    Replace,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::None => "none",
            Action::Create => "create",
            Action::Replace => "replace",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Answer {
    pub reply: String,
    pub action: Action,
    pub workflow: Option<Value>,
}

/// Read the model's text: the first JSON object in it (code fences and words around it are ignored).
pub fn parse_answer(text: &str) -> Result<Answer, String> {
    let obj = first_object(text).ok_or("The answer has no JSON object. Answer with one JSON object: {\"reply\": ..., \"action\": ..., \"workflow\": ...}.")?;
    let reply = match obj.get("reply") {
        Some(Value::String(s)) => s.trim().to_string(),
        None | Some(Value::Null) => String::new(),
        Some(_) => return Err("`reply` must be a text.".into()),
    };
    let workflow = obj.get("workflow").filter(|w| !w.is_null()).cloned();
    let action = match obj.get("action").and_then(Value::as_str).map(|a| a.trim().to_ascii_lowercase()) {
        Some(a) if a == "none" => Action::None,
        Some(a) if a == "create" => Action::Create,
        Some(a) if a == "replace" => Action::Replace,
        Some(other) => return Err(format!("`action` must be \"none\", \"create\" or \"replace\", not \"{other}\".")),
        None if workflow.is_some() => Action::Replace,
        None => Action::None,
    };
    if action != Action::None && !workflow.as_ref().is_some_and(Value::is_object) {
        return Err(format!("`action` \"{}\" needs `workflow` to be the full workflow as one JSON object.", action.as_str()));
    }
    Ok(Answer { reply, action, workflow: if action == Action::None { None } else { workflow } })
}

/// The first `{...}` in the text that is a JSON object.
fn first_object(text: &str) -> Option<serde_json::Map<String, Value>> {
    for (i, _) in text.match_indices('{').take(20) {
        let mut it = serde_json::Deserializer::from_str(&text[i..]).into_iter::<Value>();
        if let Some(Ok(Value::Object(o))) = it.next() {
            return Some(o);
        }
    }
    None
}

pub struct Prompt {
    pub system: String,
    pub user: String,
}

/// One model call, for `llm_calls`. Never holds a prompt or an answer.
#[derive(Debug, Clone, Default)]
pub struct CallLog {
    pub model: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_tokens: u32,
    pub duration_ms: u32,
    pub ok: bool,
    pub error_code: Option<String>,
}

#[derive(Debug)]
pub enum Outcome {
    /// A good answer. `workflow` passed the validator (for `create` and `replace`).
    Answered(Answer),
    /// Still bad after the repair try.
    Unusable { reply: String, code: &'static str, message: String, errors: Vec<Issue>, workflow: Option<Value> },
    /// The provider failed.
    Llm(LlmError),
}

#[derive(Debug)]
pub struct Turn {
    pub calls: Vec<CallLog>,
    pub outcome: Outcome,
}

/// One plain call with a timeout. Fills a `CallLog`.
pub async fn call(client: &dyn LlmClient, system: &str, user: &str, max_output_tokens: u32, json: bool, calls: &mut Vec<CallLog>) -> Result<LlmResponse, LlmError> {
    let req = LlmRequest { system: system.to_string(), user: user.to_string(), max_output_tokens, json, temperature: 0.2 };
    let started = Instant::now();
    let res = match tokio::time::timeout(Duration::from_secs(TIMEOUT_SECS), client.complete(&req)).await {
        Ok(r) => r,
        Err(_) => Err(LlmError::Transient { code: "timeout", retry_after: None }),
    };
    let duration_ms = started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
    match &res {
        Ok(r) => calls.push(CallLog { model: r.model.clone(), input_tokens: r.input_tokens, output_tokens: r.output_tokens, cached_tokens: r.cached_tokens, duration_ms, ok: true, error_code: None }),
        Err(e) => calls.push(CallLog { duration_ms, ok: false, error_code: Some(e.code()), ..Default::default() }),
    }
    res
}

/// Ask, check, repair once. `validate` checks a workflow (the real one uses the install's providers and secrets).
pub async fn run_turn(client: &dyn LlmClient, prompt: &Prompt, validate: &(dyn Fn(&Value) -> ValidationResult + Sync)) -> Turn {
    let mut calls = vec![];
    let first = match call(client, &prompt.system, &prompt.user, MAX_OUTPUT_TOKENS, true, &mut calls).await {
        Ok(r) => r,
        Err(e) => return Turn { calls, outcome: Outcome::Llm(e) },
    };
    let problem = match check(&first.text, validate) {
        Ok(a) => return Turn { calls, outcome: Outcome::Answered(a) },
        Err(p) => p,
    };
    let echo: String = first.text.chars().take(MAX_REPAIR_ECHO).collect();
    let user = format!(
        "{}\n## Your last answer was not usable\nYour answer:\n{echo}\n\nProblem:\n{}\n\nAnswer again with ONE JSON object: {{\"reply\": ..., \"action\": ..., \"workflow\": ...}}. If action is create or replace, send the full corrected workflow.\n",
        prompt.user,
        problem.message
    );
    let second = match call(client, &prompt.system, &user, MAX_OUTPUT_TOKENS, true, &mut calls).await {
        Ok(r) => r,
        Err(e) => return Turn { calls, outcome: Outcome::Llm(e) },
    };
    match check(&second.text, validate) {
        Ok(a) => Turn { calls, outcome: Outcome::Answered(a) },
        Err(p) => {
            let message = p.short();
            Turn { calls, outcome: Outcome::Unusable { reply: p.reply, code: p.code, message, errors: p.errors, workflow: p.workflow } }
        }
    }
}

struct Problem {
    code: &'static str,
    message: String,
    errors: Vec<Issue>,
    reply: String,
    workflow: Option<Value>,
}

impl Problem {
    fn short(&self) -> String {
        match self.code {
            "invalid_workflow" => format!("The workflow has {} error{}.", self.errors.len(), if self.errors.len() == 1 { "" } else { "s" }),
            _ => "The answer of the AI could not be read.".into(),
        }
    }
}

fn check(text: &str, validate: &(dyn Fn(&Value) -> ValidationResult + Sync)) -> Result<Answer, Problem> {
    let answer = parse_answer(text).map_err(|m| Problem { code: "bad_answer", message: m, errors: vec![], reply: String::new(), workflow: None })?;
    if let Some(w) = &answer.workflow {
        let v = validate(w);
        if !v.ok {
            let mut message = String::from("The workflow has errors. Fix every one of them:\n");
            for i in v.errors.iter().take(30) {
                message.push_str(&format!("- [{}]{}{}: {}\n", i.code, i.node_id.as_deref().map_or(String::new(), |n| format!(" node `{n}`")), i.path.as_deref().map_or(String::new(), |p| format!(" at {p}")), i.message));
            }
            return Err(Problem { code: "invalid_workflow", message, errors: v.errors, reply: answer.reply, workflow: answer.workflow });
        }
    }
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automations::validate::{Options, validate_value};
    use crate::generator::llm::fake::{FakeLlm, response};
    use serde_json::json;

    fn validate(w: &Value) -> ValidationResult {
        validate_value(w, &Options::default())
    }

    pub fn good_workflow() -> Value {
        json!({ "format": "aikido-workflow", "version": 1, "name": "Copilot test",
            "nodes": [{ "id": "start", "type": "trigger.manual" }, { "id": "pass", "type": "flow.noop" }],
            "edges": [{ "from": "start", "to": "pass" }] })
    }

    fn answer(action: &str, w: Option<Value>) -> String {
        json!({ "reply": "Done.", "action": action, "workflow": w }).to_string()
    }

    fn prompt() -> Prompt {
        Prompt { system: "SYS".into(), user: "USER".into() }
    }

    #[test]
    fn parse_strips_fences_and_text_around_the_object() {
        let t = format!("Here you go:\n```json\n{}\n```\nBye {{not json}}", answer("replace", Some(good_workflow())));
        let a = parse_answer(&t).unwrap();
        assert_eq!(a.action, Action::Replace);
        assert_eq!(a.reply, "Done.");
        assert!(a.workflow.is_some());
        let none = parse_answer(r#"{"reply":"It sends at 9.","action":"none","workflow":null}"#).unwrap();
        assert_eq!((none.action, none.workflow), (Action::None, None));
        // a missing action with a workflow means replace
        assert_eq!(parse_answer(&json!({ "workflow": good_workflow() }).to_string()).unwrap().action, Action::Replace);
    }

    #[test]
    fn parse_rejects_what_cannot_be_used() {
        assert!(parse_answer("no json here").is_err());
        assert!(parse_answer(r#"{"reply":"x","action":"publish"}"#).unwrap_err().contains("publish"));
        assert!(parse_answer(r#"{"reply":"x","action":"create","workflow":null}"#).is_err());
        assert!(parse_answer(r#"{"reply":"x","action":"replace","workflow":[1]}"#).is_err());
        assert!(parse_answer(r#"{"reply":5,"action":"none"}"#).is_err());
        assert!(parse_answer("{broken").is_err());
    }

    #[tokio::test]
    async fn create_replace_and_answer_only() {
        for (action, expect) in [("create", Action::Create), ("replace", Action::Replace)] {
            let llm = FakeLlm::new();
            llm.push(Ok(response(answer(action, Some(good_workflow())))));
            let t = run_turn(llm.as_ref(), &prompt(), &validate).await;
            assert_eq!(llm.call_count(), 1);
            assert_eq!(t.calls.len(), 1);
            assert!(t.calls[0].ok);
            match t.outcome {
                Outcome::Answered(a) => assert_eq!((a.action, a.workflow.is_some()), (expect, true)),
                o => panic!("{o:?}"),
            }
        }
        let llm = FakeLlm::new();
        llm.push(Ok(response(answer("none", None))));
        let t = run_turn(llm.as_ref(), &prompt(), &validate).await;
        assert!(matches!(t.outcome, Outcome::Answered(Answer { action: Action::None, workflow: None, .. })));
    }

    #[tokio::test]
    async fn invalid_output_gets_one_repair_try_with_the_errors() {
        let llm = FakeLlm::new();
        let mut bad = good_workflow();
        bad["nodes"][1]["type"] = json!("no.such_node");
        llm.push(Ok(response(answer("replace", Some(bad)))));
        llm.push(Ok(response(answer("replace", Some(good_workflow())))));
        let t = run_turn(llm.as_ref(), &prompt(), &validate).await;
        assert_eq!(llm.call_count(), 2);
        assert_eq!(t.calls.len(), 2);
        let second = &llm.calls()[1];
        assert!(second.user.starts_with("USER"));
        assert!(second.user.contains("no.such_node") && second.user.contains("unknown_node_type"), "{}", second.user);
        assert_eq!(second.system, "SYS", "the fixed part is the same, so it stays cached");
        assert!(matches!(t.outcome, Outcome::Answered(_)));
    }

    #[tokio::test]
    async fn bad_json_then_good_json_is_repaired() {
        let llm = FakeLlm::new();
        llm.push(Ok(response("I think you should add a node.")));
        llm.push(Ok(response(answer("replace", Some(good_workflow())))));
        let t = run_turn(llm.as_ref(), &prompt(), &validate).await;
        assert!(llm.calls()[1].user.contains("no JSON object"));
        assert!(matches!(t.outcome, Outcome::Answered(_)));
    }

    #[tokio::test]
    async fn still_bad_after_the_repair_is_unusable() {
        let llm = FakeLlm::new();
        let mut bad = good_workflow();
        bad["edges"] = json!([{ "from": "start", "to": "ghost" }]);
        llm.push(Ok(response(answer("replace", Some(bad.clone())))));
        llm.push(Ok(response(answer("replace", Some(bad)))));
        llm.push(Ok(response(answer("replace", Some(good_workflow())))));
        let t = run_turn(llm.as_ref(), &prompt(), &validate).await;
        assert_eq!(llm.call_count(), 2, "one repair try only");
        match t.outcome {
            Outcome::Unusable { code, errors, message, workflow, reply } => {
                assert_eq!(code, "invalid_workflow");
                assert!(!errors.is_empty());
                assert!(message.starts_with("The workflow has"));
                assert!(workflow.is_some());
                assert_eq!(reply, "Done.");
            }
            o => panic!("{o:?}"),
        }
        let llm = FakeLlm::new();
        llm.push(Ok(response("nope")));
        llm.push(Ok(response("still nope")));
        match run_turn(llm.as_ref(), &prompt(), &validate).await.outcome {
            Outcome::Unusable { code, .. } => assert_eq!(code, "bad_answer"),
            o => panic!("{o:?}"),
        }
    }

    #[tokio::test]
    async fn a_provider_error_is_reported_and_logged() {
        let llm = FakeLlm::new();
        llm.push(Err(LlmError::Auth));
        let t = run_turn(llm.as_ref(), &prompt(), &validate).await;
        assert!(matches!(t.outcome, Outcome::Llm(LlmError::Auth)));
        assert_eq!((t.calls[0].ok, t.calls[0].error_code.as_deref()), (false, Some("provider_auth")));
    }
}
