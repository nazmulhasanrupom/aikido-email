//! The AI node (build map 7.3): `ai.agent`. One call to a provider of the generator (the same client, the same
//! retries, the same key store), text or JSON out. A JSON answer that is not valid gets one repair try. Every call
//! writes an `llm_calls` row and counts in the daily token budget of the generator.
//!
//! No prompt text and no answer text goes to a log or to `llm_calls`. The error messages never repeat the answer.
use super::ai_backend::{AiBackend, AiCall, AiProvider};
use super::ai_tools::{self, Reply, Tool, ToolCall};
use crate::automations::registry::{Item, NodeCtx, NodeError, NodeInput, NodeOutput, NodeSpec, NodeType};
use crate::generator::llm::{LlmError, LlmRequest, LlmResponse};
use crate::generator::parse::json_part;
use crate::prompting::sender_builder::strip_dashes;
use async_trait::async_trait;
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use std::time::{Duration, Instant};

const MAX_SYSTEM_CHARS: usize = 20_000;
const MAX_PROMPT_CHARS: usize = 50_000;
const DEFAULT_TIMEOUT_SECS: u64 = 120;
/// Used when `temperature` is not set.
const DEFAULT_TEMPERATURE: f64 = 0.7;
const PLAIN_SYSTEM: &str = "You are a careful assistant. Answer in plain text. Do not use em dashes or en dashes.";

/// The node for a spec, or the spec back when it is not an AI node.
#[allow(clippy::result_large_err)]
pub fn make(spec: NodeSpec) -> Result<Box<dyn NodeType>, NodeSpec> {
    match spec.type_id {
        "ai.agent" => Ok(Box::new(AiAgent(spec))),
        _ => Err(spec),
    }
}

fn bad(msg: impl Into<String>) -> NodeError {
    NodeError::new("invalid_param_value", msg)
}

// ---------- the output fields ----------

#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub key: String,
    pub ty: String,
    pub description: String,
    pub required: bool,
}

/// The `output.fields` of the params. Checked: 1 to 40 characters (letters, digits, `_`), no repeats, known types.
pub fn parse_fields(v: Option<&Value>) -> Result<Vec<Field>, NodeError> {
    let Some(Value::Array(list)) = v else { return Ok(vec![]) };
    if list.len() > 30 {
        return Err(bad("output.fields can have up to 30 fields."));
    }
    let mut out: Vec<Field> = vec![];
    let mut seen = BTreeSet::new();
    for (i, f) in list.iter().enumerate() {
        let key = f.get("key").and_then(Value::as_str).unwrap_or("").to_string();
        let mut chars = key.chars();
        let ok = key.len() <= 40 && chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !ok {
            return Err(bad(format!("output.fields[{i}].key `{key}` is not a valid key. Use 1 to 40 letters, digits and _, starting with a letter.")));
        }
        if !seen.insert(key.clone()) {
            return Err(bad(format!("output.fields[{i}].key `{key}` is used twice.")));
        }
        let ty = f.get("type").and_then(Value::as_str).unwrap_or("string").to_string();
        if !["string", "number", "boolean", "array", "object"].contains(&ty.as_str()) {
            return Err(bad(format!("output.fields[{i}].type `{ty}` is not string, number, boolean, array or object.")));
        }
        out.push(Field { key, ty, description: f.get("description").and_then(Value::as_str).unwrap_or("").trim().to_string(), required: f.get("required").and_then(Value::as_bool).unwrap_or(true) });
    }
    Ok(out)
}

/// The JSON Schema the model is asked to follow (build map 7.3).
pub fn schema_of(fields: &[Field]) -> Value {
    let mut props = Map::new();
    for f in fields {
        let mut p = json!({ "type": f.ty });
        if !f.description.is_empty() {
            p["description"] = json!(f.description);
        }
        props.insert(f.key.clone(), p);
    }
    json!({
        "type": "object",
        "properties": props,
        "required": fields.iter().filter(|f| f.required).map(|f| f.key.clone()).collect::<Vec<_>>(),
        "additionalProperties": false,
    })
}

/// One line per field: `- "key" (type, required): description`.
pub fn field_lines(fields: &[Field]) -> String {
    let mut s = String::new();
    for f in fields {
        let need = if f.required { "required" } else { "optional, null if unknown" };
        s.push_str(&format!("- \"{}\" ({}, {need}){}\n", f.key, f.ty, if f.description.is_empty() { String::new() } else { format!(": {}", f.description) }));
    }
    s
}

/// The system text: the user's words, then the rules for the JSON answer.
pub fn system_text(system: &str, fields: &[Field], json_out: bool) -> String {
    let system = system.trim();
    if !json_out {
        return if system.is_empty() { PLAIN_SYSTEM.to_string() } else { system.to_string() };
    }
    let mut s = String::new();
    if !system.is_empty() {
        s.push_str(system);
        s.push_str("\n\n");
    }
    s.push_str("Answer with one JSON object and nothing else. No code fence, no extra text. Do not use em dashes or en dashes.\n");
    if fields.is_empty() {
        s.push_str("The object can have any keys that fit the task.");
    } else {
        s.push_str("The object has exactly these keys:\n");
        s.push_str(&field_lines(fields));
        s.push_str(&format!("\nJSON Schema:\n{}", schema_of(fields)));
    }
    s
}

fn coerce(v: &Value, ty: &str) -> Option<Value> {
    match (ty, v) {
        ("string", Value::String(_)) | ("array", Value::Array(_)) | ("object", Value::Object(_)) | ("boolean", Value::Bool(_)) => Some(v.clone()),
        ("number", Value::Number(_)) => Some(v.clone()),
        ("number", Value::String(s)) => s.trim().parse::<f64>().ok().filter(|n| n.is_finite()).and_then(|n| if n.fract() == 0.0 && n.abs() < 9e15 { Some(json!(n as i64)) } else { serde_json::Number::from_f64(n).map(Value::Number) }),
        ("boolean", Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "true" => Some(json!(true)),
            "false" => Some(json!(false)),
            _ => None,
        },
        _ => None,
    }
}

/// Read the answer. `Err` is a short reason for the repair try; it never holds the answer itself.
pub fn read_json(text: &str, fields: &[Field]) -> Result<Map<String, Value>, String> {
    let part = json_part(text).ok_or("The answer has no JSON object.")?;
    let v: Value = serde_json::from_str(part).map_err(|_| "The answer is not valid JSON.".to_string())?;
    let Value::Object(obj) = v else { return Err("The answer is not a JSON object.".into()) };
    if fields.is_empty() {
        return Ok(obj);
    }
    let mut out = Map::new();
    for f in fields {
        match obj.get(&f.key) {
            None | Some(Value::Null) if f.required => return Err(format!("The key \"{}\" is missing.", f.key)),
            None | Some(Value::Null) => {
                out.insert(f.key.clone(), Value::Null);
            }
            Some(v) => match coerce(v, &f.ty) {
                Some(c) => {
                    out.insert(f.key.clone(), c);
                }
                None => return Err(format!("The key \"{}\" must be a {}.", f.key, f.ty)),
            },
        }
    }
    Ok(out)
}

fn strip_value(v: &mut Value) {
    match v {
        Value::String(s) => *s = strip_dashes(s),
        Value::Array(a) => a.iter_mut().for_each(strip_value),
        Value::Object(o) => o.values_mut().for_each(strip_value),
        _ => {}
    }
}

fn llm_error(e: &LlmError) -> NodeError {
    match e {
        LlmError::Transient { code, .. } => NodeError::new("ai_provider_unavailable", format!("The AI provider could not answer ({code}).")),
        LlmError::Auth => NodeError::new("ai_auth_failed", "The AI provider refused the API key."),
        LlmError::BadRequest(m) => NodeError::new("ai_bad_request", format!("The AI provider refused the request: {m}")),
        LlmError::Refused => NodeError::new("ai_refused", "The model declined to answer."),
        LlmError::BadResponse(m) => NodeError::new("ai_bad_response", format!("The answer of the AI provider could not be read: {m}")),
    }
}


/// One call to the provider: the budget check, the call, the `llm_calls` row, and the errors as node errors.
async fn ask(ai: &dyn AiBackend, env: &super::env::NodeEnv, node_id: &str, provider: &AiProvider, req: &LlmRequest, timeout: Duration) -> Result<LlmResponse, NodeError> {
    // the budget is checked before each call: a run waits for it, it does not fail (map 7.3)
    if ai.budget_reached().await? {
        return Err(NodeError::new("ai_budget_reached", "The daily token budget of the AI is used up."));
    }
    env.count_call()?;
    let started = Instant::now();
    let res = match tokio::time::timeout(timeout, provider.client.complete(req)).await {
        Ok(r) => r,
        Err(_) => Err(LlmError::Transient { code: "timeout", retry_after: None }),
    };
    let mut call = AiCall {
        provider_id: Some(provider.id),
        model: provider.model.clone(),
        duration_ms: started.elapsed().as_millis() as u32,
        run_id: env.who.run_id.clone(),
        workflow_id: env.who.workflow_id.clone(),
        node_id: node_id.to_string(),
        ..Default::default()
    };
    match res {
        Ok(r) => {
            call.ok = true;
            call.input_tokens = r.input_tokens;
            call.output_tokens = r.output_tokens;
            call.cached_tokens = r.cached_tokens;
            if !r.model.is_empty() {
                call.model = r.model.clone();
            }
            ai.record(call).await;
            Ok(r)
        }
        Err(e) => {
            call.error_code = Some(e.code());
            ai.record(call).await;
            if e == LlmError::Auth {
                ai.auth_failed(provider.id).await;
            }
            if matches!(&e, LlmError::Transient { code: "timeout", .. }) {
                return Err(NodeError::new("ai_timeout", format!("The AI provider did not answer in {} seconds.", timeout.as_secs())));
            }
            Err(llm_error(&e))
        }
    }
}

/// Everything `run_with_tools` needs.
struct ToolRun<'a> {
    ai: &'a dyn AiBackend,
    provider: &'a AiProvider,
    tools: &'a [Tool],
    fields: &'a [Field],
    json_out: bool,
    strip: bool,
    system: &'a str,
    prompt: &'a str,
    max_tokens: u32,
    temperature: f64,
    timeout: Duration,
    max_calls: u64,
}

/// The agent with tools: ask, run the tool the AI names, ask again, until it gives its final answer.
async fn run_with_tools(ctx: &NodeCtx<'_>, env: &super::env::NodeEnv, item: &Item, index: usize, r: ToolRun<'_>) -> Result<NodeOutput, NodeError> {
    let system = ai_tools::tools_system(r.system, r.fields, r.json_out, r.tools, r.max_calls);
    let mut calls: Vec<ToolCall> = vec![];
    let mut logs: Vec<String> = vec![];
    let mut must_answer = false;
    let mut repaired = false;
    let mut repair_note: Option<String> = None;
    // the tool calls, the answer, one repair, and one refused call over the limit
    for turn in 0..(r.max_calls + 3) {
        let mut user = ai_tools::tools_user(r.prompt, &calls, must_answer);
        if let Some(reason) = &repair_note {
            user.push_str(&format!("\n\nYour last reply was not usable: {reason} Reply with one JSON object: a tool_call or a final answer."));
        }
        let req = LlmRequest { system: system.clone(), user, max_output_tokens: r.max_tokens, json: true, temperature: r.temperature };
        let resp = ask(r.ai, env, ctx.node_id, r.provider, &req, r.timeout).await?;
        logs.push(format!("AI call {}: {}, {} tokens in, {} out.", turn + 1, if resp.model.is_empty() { &r.provider.model } else { &resp.model }, resp.input_tokens, resp.output_tokens));
        repair_note = None;
        match ai_tools::parse_reply(&resp.text) {
            Reply::Tool { name, arguments } => {
                if must_answer {
                    return Err(NodeError::new("ai_bad_output", format!("The AI kept calling tools after the limit of {} calls.", r.max_calls)));
                }
                let result = match r.tools.iter().find(|t| t.name == name) {
                    None => Err(NodeError::new("tool_unknown", format!("There is no tool `{name}`. The tools are: {}.", r.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")))),
                    Some(tool) => ai_tools::call_tool(ctx, tool, &arguments, item, index).await,
                };
                match &result {
                    // these end the node, not just the tool call
                    Err(e) if e.code == "too_many_calls" || e.code == "ai_budget_reached" => return Err(e.clone()),
                    Err(_) if ctx.control.should_stop() => return Err(NodeError::new("cancelled", "The run was cancelled.")),
                    Err(e) => logs.push(format!("Tool call {}: {name} failed ({}).", calls.len() + 1, e.code)),
                    Ok(v) if v.get("dry_run").is_some() => logs.push(format!("Tool call {}: {name} (dry, test run).", calls.len() + 1)),
                    Ok(_) => logs.push(format!("Tool call {}: {name} ok.", calls.len() + 1)),
                }
                calls.push(ToolCall { name, arguments, result: ai_tools::result_text(ctx, result) });
                must_answer = calls.len() as u64 >= r.max_calls;
            }
            Reply::Final(answer) => {
                let checked: Result<Map<String, Value>, String> = if r.json_out {
                    read_json(&answer.to_string(), r.fields)
                } else {
                    match answer.as_str().map(str::trim) {
                        Some(t) if !t.is_empty() => Ok(Map::from_iter([("text".to_string(), json!(t))])),
                        _ => Err("\"final\" must be a non-empty string.".to_string()),
                    }
                };
                match checked {
                    Ok(map) => {
                        let mut value = Value::Object(map);
                        if r.strip {
                            strip_value(&mut value);
                        }
                        let Value::Object(map) = value else { unreachable!("an object stays an object") };
                        let mut out = NodeOutput::main(vec![item.child(map)]);
                        out.logs = logs;
                        return Ok(out);
                    }
                    Err(reason) if !repaired => {
                        repaired = true;
                        logs.push(format!("The answer was not usable ({reason}). Trying once more."));
                        repair_note = Some(reason);
                    }
                    Err(reason) => return Err(NodeError::new("ai_bad_output", format!("The AI did not give a usable answer: {reason}"))),
                }
            }
            Reply::Bad(reason) if !repaired => {
                repaired = true;
                logs.push(format!("The reply was not usable ({reason}). Trying once more."));
                repair_note = Some(reason);
            }
            Reply::Bad(reason) => return Err(NodeError::new("ai_bad_output", format!("The AI did not give a usable reply: {reason}"))),
        }
    }
    Err(NodeError::new("ai_bad_output", "The AI did not give a final answer."))
}

// ---------- the node ----------

struct AiAgent(NodeSpec);

/// The `timeout_secs` of the params when it is a plain number.
fn timeout_of(params: &Map<String, Value>) -> u64 {
    params.get("timeout_secs").and_then(Value::as_u64).filter(|t| (5..=600).contains(t)).unwrap_or(DEFAULT_TIMEOUT_SECS)
}

#[async_trait]
impl NodeType for AiAgent {
    fn spec(&self) -> &NodeSpec {
        &self.0
    }

    /// Two calls (the answer and one repair) must fit, so the node gets more time than the engine default (build map 8.7).
    fn timeout(&self, params: &Map<String, Value>) -> Option<Duration> {
        // with tools: one call per tool call, one for the answer, one for a repair
        let turns = if params.get("tools").and_then(Value::as_array).is_some_and(|t| !t.is_empty()) {
            params.get("max_tool_calls").and_then(Value::as_u64).unwrap_or(ai_tools::DEFAULT_MAX_TOOL_CALLS).clamp(1, 20) + 2
        } else {
            2
        };
        Some(Duration::from_secs(timeout_of(params) * turns + 30))
    }

    async fn run(&self, ctx: &NodeCtx<'_>, input: NodeInput) -> Result<NodeOutput, NodeError> {
        let env = ctx.env()?;
        let ai: &dyn AiBackend = env.ai.as_deref().ok_or_else(|| NodeError::new("no_environment", "The AI Agent needs the server and can not run here."))?;
        let item = input.items.first().ok_or_else(|| bad("no input item"))?;
        // `tools` are read as written: their params are evaluated once, by the tool node (never twice)
        let tools = ai_tools::parse_tools(ctx.raw("tools"))?;
        let p = {
            let mut params = ctx.params.clone();
            params.remove("tools");
            match ctx.value(item, input.first_index, &Value::Object(params))? {
                Value::Object(m) => m,
                _ => Map::new(),
            }
        };

        let text_param = |key: &str| -> Result<String, NodeError> {
            Ok(match p.get(key) {
                None | Some(Value::Null) => String::new(),
                Some(Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
            })
        };
        let prompt = text_param("prompt")?;
        if prompt.trim().is_empty() {
            return Err(bad("`prompt` is empty."));
        }
        let system = text_param("system")?;
        if prompt.chars().count() > MAX_PROMPT_CHARS {
            return Err(NodeError::new("prompt_too_long", format!("The prompt is longer than {MAX_PROMPT_CHARS} characters.")));
        }
        if system.chars().count() > MAX_SYSTEM_CHARS {
            return Err(NodeError::new("prompt_too_long", format!("The system text is longer than {MAX_SYSTEM_CHARS} characters.")));
        }
        let out_p = p.get("output").and_then(Value::as_object);
        let format = out_p.and_then(|o| o.get("format")).and_then(Value::as_str).unwrap_or("text");
        if !["text", "json"].contains(&format) {
            return Err(bad(format!("output.format `{format}` is not text or json.")));
        }
        let json_out = format == "json";
        let fields = if json_out { parse_fields(out_p.and_then(|o| o.get("fields")))? } else { vec![] };
        let which = p.get("provider").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).unwrap_or("default_draft").to_string();
        let max_tokens = p.get("max_tokens").and_then(Value::as_u64).unwrap_or(1000).clamp(50, 8000) as u32;
        let timeout = Duration::from_secs(timeout_of(&p));
        let temperature = p.get("temperature").and_then(Value::as_f64).filter(|t| (0.0..=2.0).contains(t)).unwrap_or(DEFAULT_TEMPERATURE);
        let strip = p.get("strip_dashes").and_then(Value::as_bool).unwrap_or(true);

        let provider = ai.provider(&which).await?;
        if !tools.is_empty() {
            let max_calls = p.get("max_tool_calls").and_then(Value::as_u64).unwrap_or(ai_tools::DEFAULT_MAX_TOOL_CALLS).clamp(1, 20);
            let run = ToolRun { ai, provider: &provider, tools: &tools, fields: &fields, json_out, strip, system: &system, prompt: &prompt, max_tokens, temperature, timeout, max_calls };
            return run_with_tools(ctx, env, item, input.first_index, run).await;
        }
        let system = system_text(&system, &fields, json_out);
        let mut user = prompt;
        let original = user.clone();
        let mut logs: Vec<String> = vec![];

        for attempt in 0..2 {
            let req = LlmRequest { system: system.clone(), user: user.clone(), max_output_tokens: max_tokens, json: json_out, temperature };
            let resp = ask(ai, env, ctx.node_id, &provider, &req, timeout).await?;
            logs.push(format!("AI call {}: {}, {} tokens in, {} out.", attempt + 1, if resp.model.is_empty() { &provider.model } else { &resp.model }, resp.input_tokens, resp.output_tokens));

            let result: Result<Map<String, Value>, String> = if json_out {
                read_json(&resp.text, &fields)
            } else {
                let t = resp.text.trim();
                if t.is_empty() { Err("The answer is empty.".to_string()) } else { Ok(Map::from_iter([("text".to_string(), json!(t))])) }
            };
            match result {
                Ok(map) => {
                    let mut value = Value::Object(map);
                    if strip {
                        strip_value(&mut value);
                    }
                    let Value::Object(map) = value else { unreachable!("an object stays an object") };
                    let mut out = NodeOutput::main(vec![item.child(map)]);
                    out.logs = logs;
                    return Ok(out);
                }
                Err(reason) if attempt == 0 && json_out => {
                    logs.push(format!("The answer was not usable ({reason}). Trying once more."));
                    user = format!("{original}\n\nYour last answer was not usable: {reason} Answer again with the JSON only.");
                }
                Err(reason) => return Err(NodeError::new("ai_bad_output", format!("The AI did not give a usable answer: {reason}"))),
            }
        }
        Err(NodeError::new("ai_bad_output", "The AI did not give a usable answer."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields() -> Vec<Field> {
        parse_fields(Some(&json!([{ "key": "subject", "type": "string", "description": "Under 8 words" }, { "key": "score", "type": "number", "required": false }]))).unwrap()
    }

    #[test]
    fn fields_are_checked() {
        assert_eq!(fields().len(), 2);
        assert!(!fields()[1].required);
        for bad_list in [json!([{ "key": "1x" }]), json!([{ "key": "a b" }]), json!([{ "key": "a" }, { "key": "a" }]), json!([{ "key": "a", "type": "date" }])] {
            assert_eq!(parse_fields(Some(&bad_list)).unwrap_err().code, "invalid_param_value", "{bad_list}");
        }
        assert!(parse_fields(None).unwrap().is_empty());
    }

    #[test]
    fn the_schema_and_the_system_text_name_every_field() {
        let s = schema_of(&fields());
        assert_eq!(s["required"], json!(["subject"]));
        assert_eq!(s["properties"]["subject"]["description"], "Under 8 words");
        let t = system_text("Be brief.", &fields(), true);
        assert!(t.starts_with("Be brief."));
        assert!(t.contains("\"subject\" (string, required): Under 8 words"));
        assert!(t.contains("\"score\" (number, optional"));
        assert_eq!(system_text("", &[], false), PLAIN_SYSTEM);
        assert_eq!(system_text(" Hi ", &[], false), "Hi");
    }

    #[test]
    fn json_answers_are_read_and_checked() {
        let f = fields();
        let ok = read_json("Sure!\n```json\n{\"subject\": \"Hello\", \"score\": \"4\", \"extra\": 1}\n```", &f).unwrap();
        assert_eq!(Value::Object(ok), json!({ "subject": "Hello", "score": 4 }));
        // an optional key that is missing becomes null
        assert_eq!(read_json("{\"subject\":\"x\"}", &f).unwrap()["score"], Value::Null);
        // no fields: any object passes
        assert_eq!(read_json("{\"a\":1}", &[]).unwrap()["a"], 1);
        let e = |t: &str| read_json(t, &f).unwrap_err();
        assert!(e("no json").contains("no JSON"));
        assert!(e("{oops}").contains("not valid JSON"));
        assert!(e("{\"score\":1}").contains("\"subject\" is missing"));
        assert!(e("{\"subject\":null}").contains("\"subject\" is missing"));
        assert!(e("{\"subject\":5}").contains("must be a string"));
        // the reason never repeats the answer
        assert!(!e("{\"subject\":\"SECRET-ANSWER\",\"score\":\"x\"}").contains("SECRET-ANSWER"));
    }

    #[test]
    fn dashes_go_in_nested_text_too() {
        let mut v = json!({ "a": "one \u{2014} two", "b": ["x \u{2013} y"], "c": { "d": "p\u{2014}q" }, "n": 5 });
        strip_value(&mut v);
        assert_eq!(v, json!({ "a": "one, two", "b": ["x, y"], "c": { "d": "p, q" }, "n": 5 }));
    }

    #[test]
    fn the_timeout_gets_room_for_a_repair() {
        assert_eq!(timeout_of(&Map::new()), 120);
        assert_eq!(timeout_of(json!({ "timeout_secs": 30 }).as_object().unwrap()), 30);
        assert_eq!(timeout_of(json!({ "timeout_secs": 5000 }).as_object().unwrap()), 120);
    }
}
