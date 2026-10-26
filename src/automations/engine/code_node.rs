//! The code node (build map decision D4, phase 9): `code.js` runs a piece of JavaScript with `rquickjs` (QuickJS).
//!
//! The sandbox is the engine itself: a fresh runtime for every call, no modules, no `fetch`, no `require`, no file
//! or network access, no timers and no promises (so no `async`/`await`). Limits: 1 second of running time (an interrupt handler stops the code), 32 MB of memory, a small stack.
//! The call runs on a blocking thread, so a busy loop never holds up the async workers.
//!
//! The code is the body of a function with two arguments: `items` (a list of the input items, plain objects) and
//! `vars` (the variables of the workflow). It returns a list of objects, or one object.
use crate::automations::registry::{Item, LINEAGE_CUT, NodeCtx, NodeError, NodeInput, NodeOutput, NodeSpec, NodeType};
use async_trait::async_trait;
use rquickjs::context::intrinsic;
use rquickjs::context::EvalOptions;
use rquickjs::{CatchResultExt, CaughtError, Context, Function, Runtime, Value as JsValue};
use serde_json::{Map, Value};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The longest code (characters).
pub const MAX_CODE_CHARS: usize = 50_000;
/// Running time of one call.
pub const CPU_LIMIT: Duration = Duration::from_secs(1);
/// Memory of one call.
pub const MEMORY_LIMIT: usize = 32 * 1024 * 1024;
const STACK_LIMIT: usize = 512 * 1024;
const FILE_NAME: &str = "code.js";

/// The node for a spec, or the spec back when it is not the code node.
#[allow(clippy::result_large_err)]
pub fn make(spec: NodeSpec) -> Result<Box<dyn NodeType>, NodeSpec> {
    match spec.type_id {
        "code.js" => Ok(Box::new(CodeJs(spec))),
        _ => Err(spec),
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub cpu: Duration,
    pub memory: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { cpu: CPU_LIMIT, memory: MEMORY_LIMIT }
    }
}

struct CodeJs(NodeSpec);

#[async_trait]
impl NodeType for CodeJs {
    fn spec(&self) -> &NodeSpec {
        &self.0
    }

    async fn run(&self, ctx: &NodeCtx<'_>, input: NodeInput) -> Result<NodeOutput, NodeError> {
        // the code is read as it is: a `${{` in a template string of the code is not an expression
        let code = ctx.raw("code").and_then(Value::as_str).unwrap_or("").to_string();
        if code.trim().is_empty() {
            return Err(NodeError::new("invalid_param_value", "`code` is empty. Write the body of a function that returns the items."));
        }
        if code.chars().count() > MAX_CODE_CHARS {
            return Err(NodeError::new("code_too_long", format!("The code is longer than {MAX_CODE_CHARS} characters.")));
        }
        let items: Vec<Value> = input.items.iter().map(|i| Value::Object(i.json.clone())).collect();
        let vars = Value::Object(ctx.eval.facts().vars.clone());
        let out = tokio::task::spawn_blocking(move || run_js(&code, &items, &vars, Limits::default()))
            .await
            .map_err(|_| NodeError::new("code_failed", "The code could not be run."))??;
        // the same number of items out as in: item i is linked to input i. Else the link to the nodes before is cut.
        let same = out.len() == input.items.len();
        let items = out
            .into_iter()
            .enumerate()
            .map(|(i, json)| if same { input.items[i].child(json) } else { Item { json, lineage: vec![(LINEAGE_CUT.into(), 0)] } })
            .collect();
        Ok(NodeOutput::main(items))
    }
}

fn fail(code: &str, message: impl Into<String>) -> NodeError {
    NodeError::new(code, message)
}

/// The kind of a JSON value, for messages.
fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

/// Check what the code returned: a list of objects, or one object.
pub fn read_return(v: Value) -> Result<Vec<Map<String, Value>>, NodeError> {
    match v {
        Value::Object(m) => Ok(vec![m]),
        Value::Array(list) => list
            .into_iter()
            .enumerate()
            .map(|(i, x)| match x {
                Value::Object(m) => Ok(m),
                other => Err(fail("code_bad_return", format!("The code returned a list, and item {i} is {}. Every item must be an object like {{ \"name\": \"x\" }}.", kind(&other)))),
            })
            .collect(),
        Value::Null => Err(fail("code_bad_return", "The code returned nothing. End the code with `return items;` (a list of objects).")),
        other => Err(fail("code_bad_return", format!("The code returned {}. Return a list of objects, like `return items.map(i => ({{ ...i, ok: true }}));`.", kind(&other)))),
    }
}

/// The line of the code (1 based) in a QuickJS stack text. The wrapper adds one line before the code.
fn line_in(stack: &str) -> Option<usize> {
    let at = stack.find(FILE_NAME)? + FILE_NAME.len();
    let rest = stack[at..].strip_prefix(':')?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let line: usize = digits.parse().ok()?;
    Some(line.saturating_sub(1).max(1))
}

fn at_line(line: Option<usize>) -> String {
    line.map(|l| format!(" (line {l})")).unwrap_or_default()
}

/// The message of a caught JavaScript error, with the line of the code.
fn describe<'js>(ctx: &rquickjs::Ctx<'js>, caught: CaughtError<'js>) -> String {
    match caught {
        CaughtError::Exception(ex) => {
            let message = ex.message().unwrap_or_default();
            let line = ex.stack().as_deref().and_then(line_in);
            format!("{}{}", message, at_line(line))
        }
        CaughtError::Value(v) => {
            let text = ctx.json_stringify(&v).ok().flatten().and_then(|s| s.to_string().ok()).unwrap_or_else(|| "a value".into());
            format!("The code threw {text}")
        }
        CaughtError::Error(e) => e.to_string(),
    }
}

/// Compile the code without running it. `Err` is a syntax error with its line. For the validator.
pub fn check_syntax(code: &str) -> Result<(), NodeError> {
    let rt = Runtime::new().map_err(|e| fail("code_failed", format!("The code engine could not start: {e}")))?;
    rt.set_memory_limit(MEMORY_LIMIT);
    rt.set_max_stack_size(STACK_LIMIT);
    let context = Context::custom::<(intrinsic::Eval,)>(&rt).map_err(|e| fail("code_failed", format!("The code engine could not start: {e}")))?;
    let source = format!("(function (items, vars) {{\n{code}\n}})");
    context.with(|ctx| {
        let mut opts = EvalOptions::default();
        opts.filename = Some(FILE_NAME.into());
        // evaluating the function expression only makes the function; the body does not run
        match ctx.eval_with_options::<Function, _>(source, opts).catch(&ctx) {
            Ok(_) => Ok(()),
            Err(e) => Err(fail("code_syntax", format!("Syntax error: {}", describe(&ctx, e)))),
        }
    })
}

/// Run the code. Blocking: call it from a blocking thread.
pub fn run_js(code: &str, items: &[Value], vars: &Value, limits: Limits) -> Result<Vec<Map<String, Value>>, NodeError> {
    let rt = Runtime::new().map_err(|e| fail("code_failed", format!("The code engine could not start: {e}")))?;
    rt.set_memory_limit(limits.memory);
    rt.set_max_stack_size(STACK_LIMIT);
    let timed_out = Arc::new(AtomicBool::new(false));
    {
        let (flag, deadline) = (timed_out.clone(), Instant::now() + limits.cpu);
        rt.set_interrupt_handler(Some(Box::new(move || {
            let over = Instant::now() >= deadline;
            if over {
                flag.store(true, Ordering::SeqCst);
            }
            over
        })));
    }
    // Eval is needed to compile the code (and gives `eval()`, which gets no more rights than the code has). No Promise, no
    // WeakRef, no Performance, no Proxy.
    let context = Context::custom::<(intrinsic::Eval, intrinsic::Date, intrinsic::RegExpCompiler, intrinsic::RegExp, intrinsic::Json, intrinsic::MapSet, intrinsic::TypedArrays)>(&rt)
        .map_err(|e| fail("code_failed", format!("The code engine could not start: {e}")))?;
    let items_json = serde_json::to_string(items).map_err(|e| fail("code_failed", e.to_string()))?;
    let vars_json = vars.to_string();
    // the code is the body of a function; its first line is line 2 of the source
    let source = format!("(function (items, vars) {{\n{code}\n}})");

    let result: Result<Option<String>, String> = context.with(|ctx| {
        let mut opts = EvalOptions::default();
        opts.filename = Some(FILE_NAME.into());
        let func: Function = match ctx.eval_with_options::<Function, _>(source, opts).catch(&ctx) {
            Ok(f) => f,
            Err(e) => return Err(format!("syntax:{}", describe(&ctx, e))),
        };
        let items_v = ctx.json_parse(items_json).catch(&ctx).map_err(|e| format!("run:{}", describe(&ctx, e)))?;
        let vars_v = ctx.json_parse(vars_json).catch(&ctx).map_err(|e| format!("run:{}", describe(&ctx, e)))?;
        let ret: JsValue = func.call((items_v, vars_v)).catch(&ctx).map_err(|e| format!("run:{}", describe(&ctx, e)))?;
        let text = ctx.json_stringify(ret).catch(&ctx).map_err(|e| format!("run:{}", describe(&ctx, e)))?;
        match text {
            Some(s) => s.to_string().map(Some).map_err(|e| format!("run:{e}")),
            None => Ok(None),
        }
    });

    if timed_out.load(Ordering::SeqCst) {
        return Err(fail("code_timeout", format!("The code ran longer than {} second(s) and was stopped. Look for a loop that never ends, or do less work per run.", limits.cpu.as_secs_f32().max(0.001))));
    }
    match result {
        Ok(Some(text)) => read_return(serde_json::from_str(&text).map_err(|_| fail("code_bad_return", "The code returned a value that is not JSON."))?),
        Ok(None) => Err(fail("code_bad_return", "The code returned nothing. End the code with `return items;` (a list of objects).")),
        Err(m) => {
            let (stage, text) = m.split_once(':').unwrap_or(("run", &m));
            let lower = text.to_ascii_lowercase();
            if lower.contains("out of memory") {
                return Err(fail("code_memory", format!("The code used more than {} MB of memory and was stopped.", limits.memory / (1024 * 1024))));
            }
            if stage == "syntax" {
                Err(fail("code_syntax", format!("Syntax error: {text}")))
            } else {
                Err(fail("code_error", format!("The code failed: {text}")))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn go(code: &str, items: Value) -> Result<Vec<Value>, NodeError> {
        let list = items.as_array().cloned().unwrap_or_default();
        run_js(code, &list, &json!({ "greeting": "hi" }), Limits::default()).map(|v| v.into_iter().map(Value::Object).collect())
    }

    #[test]
    fn items_in_items_out() {
        let out = go("return items.map(i => ({ ...i, name: i.name.toUpperCase(), n: items.length }));", json!([{ "name": "ana" }, { "name": "bo" }])).unwrap();
        assert_eq!(out, vec![json!({ "name": "ANA", "n": 2 }), json!({ "name": "BO", "n": 2 })]);
    }

    #[test]
    fn vars_and_one_object_and_json_features() {
        let out = go("const d = new Date('2026-01-02T00:00:00Z'); const m = new Map([['a', 1]]); return { g: vars.greeting, year: d.getUTCFullYear(), m: m.get('a'), re: /a(b+)/.exec('xabbb')[1], j: JSON.parse('{\"x\":1}').x };", json!([])).unwrap();
        assert_eq!(out, vec![json!({ "g": "hi", "year": 2026, "m": 1, "re": "bbb", "j": 1 })]);
    }

    #[test]
    fn unicode_survives_both_ways() {
        let out = go("return items.map(i => ({ s: i.s + '\\u00e9' }));", json!([{ "s": "caf\u{e9} \u{1F600}" }])).unwrap();
        assert_eq!(out, vec![json!({ "s": "caf\u{e9} \u{1F600}\u{e9}" })]);
    }

    #[test]
    fn a_syntax_error_names_the_line() {
        let e = go("const a = 1;\nconst b = ;\nreturn items;", json!([])).unwrap_err();
        assert_eq!(e.code, "code_syntax");
        assert!(e.message.starts_with("Syntax error:"), "{}", e.message);
        assert!(e.message.contains("(line 2)"), "{}", e.message);
    }

    #[test]
    fn a_thrown_error_names_the_line() {
        let e = go("const a = 1;\n\nthrow new Error('boom');", json!([])).unwrap_err();
        assert_eq!(e.code, "code_error");
        assert!(e.message.contains("boom") && e.message.contains("(line 3)"), "{}", e.message);
        let e = go("return items[0].a.b;", json!([{}])).unwrap_err();
        assert_eq!(e.code, "code_error");
        assert!(e.message.contains("line 1"), "{}", e.message);
        let e = go("throw 'plain text';", json!([])).unwrap_err();
        assert!(e.message.contains("plain text"), "{}", e.message);
    }

    #[test]
    fn an_endless_loop_is_stopped_after_the_time_limit() {
        let t = Instant::now();
        let e = run_js("while (true) {}", &[], &json!({}), Limits { cpu: Duration::from_millis(200), memory: MEMORY_LIMIT }).unwrap_err();
        assert_eq!(e.code, "code_timeout");
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
        // the code can not hide it with try/catch
        let e = run_js("try { while (true) {} } catch (e) {} return items;", &[], &json!({}), Limits { cpu: Duration::from_millis(100), memory: MEMORY_LIMIT }).unwrap_err();
        assert_eq!(e.code, "code_timeout");
    }

    #[test]
    fn too_much_memory_is_stopped() {
        let e = run_js("const a = []; for (;;) { a.push('x'.repeat(100000)); }", &[], &json!({}), Limits { cpu: Duration::from_secs(10), memory: 8 * 1024 * 1024 }).unwrap_err();
        assert_eq!(e.code, "code_memory", "{}", e.message);
        assert!(e.message.contains("8 MB"));
    }

    #[test]
    fn deep_recursion_is_an_error_not_a_crash() {
        let e = go("function f(n) { return f(n + 1) + 1; } return [{ n: f(0) }];", json!([])).unwrap_err();
        assert_eq!(e.code, "code_error");
    }

    #[test]
    fn the_return_shape_is_checked() {
        assert_eq!(go("return;", json!([])).unwrap_err().code, "code_bad_return");
        assert_eq!(go("return null;", json!([])).unwrap_err().code, "code_bad_return");
        assert_eq!(go("return 5;", json!([])).unwrap_err().code, "code_bad_return");
        assert_eq!(go("return 'x';", json!([])).unwrap_err().code, "code_bad_return");
        let e = go("return [{ a: 1 }, 2];", json!([])).unwrap_err();
        assert_eq!(e.code, "code_bad_return");
        assert!(e.message.contains("item 1 is a number"), "{}", e.message);
        assert_eq!(go("return [[1]];", json!([])).unwrap_err().code, "code_bad_return");
        assert_eq!(go("return [];", json!([])).unwrap(), Vec::<Value>::new());
        assert_eq!(go("return () => 1;", json!([])).unwrap_err().code, "code_bad_return");
    }

    #[test]
    fn there_is_no_way_out_of_the_sandbox() {
        for code in [
            "return [{ t: typeof fetch }]",
            "return [{ t: typeof require }]",
            "return [{ t: typeof process }]",
            "return [{ t: typeof XMLHttpRequest }]",
            "return [{ t: typeof setTimeout }]",
            "return [{ t: typeof std }]",
            "return [{ t: typeof os }]",
        ] {
            let out = go(code, json!([])).unwrap();
            assert_eq!(out[0]["t"], "undefined", "{code}");
        }
        // no promises, so no async code
        assert_eq!(go("return [{ t: typeof Promise }]", json!([])).unwrap()[0]["t"], "undefined");
        // a module import is a syntax error in a function body
        assert_eq!(go("import fs from 'fs'; return items;", json!([])).unwrap_err().code, "code_syntax");
    }

    #[test]
    fn the_input_is_a_copy() {
        // the code changes its input; a second call sees the original again
        let items = vec![json!({ "a": 1 })];
        let _ = run_js("items[0].a = 99; return items;", &items, &json!({}), Limits::default()).unwrap();
        assert_eq!(items[0]["a"], 1);
    }
}
