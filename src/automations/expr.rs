//! Expressions (build map section 6): `${{ ... }}` inside a param, powered by MiniJinja.
//!
//! - Only `${{ }}`, `${% %}` and `${# #}` are read. `{{first_name}}` and `{{ai:opener}}` (the email
//!   template tags) are plain text here and pass through untouched.
//! - A param that is exactly one `${{ expr }}` gives the typed value (list, object, number, bool, null).
//!   Text around it makes a string.
//! - A missing value is an error, not an empty text, so a test shows the mistake instead of sending "Hi ,".
//!   MiniJinja has no mode that does this exactly (CHECK item 10 of the build map): `Chainable` lets a
//!   path through a missing key go on as "undefined" (so `default` and `is defined` work), and our own
//!   formatter fails when an undefined value is printed. A whole-field expression that is undefined fails too.
//!   `null` prints as an empty text (lead fields are often `null`); `default` treats `null` like a missing value.
use chrono::{DateTime, Duration, NaiveDate, SecondsFormat, Utc};
use md5::Md5;
use minijinja::value::{Rest, ValueKind};
use minijinja::{Environment, Error, ErrorKind, UndefinedBehavior, Value, syntax::SyntaxConfig};
use serde_json::{Map, Value as Json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::sync::Arc;

/// Work one evaluation may do (MiniJinja instructions). A runaway loop stops here.
const FUEL: u64 = 200_000;
/// The longest text one param may make.
pub const MAX_OUTPUT_BYTES: usize = 1_048_576;

/// Functions that MiniJinja counts as "variables" when it scans a template. They are not data references.
const FUNCTIONS: &[&str] = &["secret", "now", "uuid", "get", "range", "dict", "namespace", "lipsum", "cycler", "joiner", "debug"];
/// The names an expression may read (section 6.3).
pub const ROOT_NAMES: &[&str] = &["json", "trigger", "nodes", "vars", "run", "workflow"];

#[derive(Debug, Clone, PartialEq)]
pub struct ExprError {
    /// `expression_syntax`, `expression_error`, `secret_not_found` or `invalid_param_value`.
    pub code: &'static str,
    pub message: String,
}

impl ExprError {
    fn syntax(message: impl Into<String>) -> Self {
        ExprError { code: "expression_syntax", message: message.into() }
    }
    fn eval(message: impl Into<String>) -> Self {
        ExprError { code: "expression_error", message: message.into() }
    }
}

impl std::fmt::Display for ExprError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

/// An error in one param of a node: where, and what.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamError {
    /// `params.prompt`, `params.cases[0].left`
    pub path: String,
    pub error: ExprError,
}

/// What an expression reads: `nodes.fetch_site.json.title` is root `nodes`, path `fetch_site.json.title`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Reference {
    pub root: String,
    pub path: Vec<String>,
}

impl Reference {
    pub fn full(&self) -> String {
        std::iter::once(self.root.as_str()).chain(self.path.iter().map(String::as_str)).collect::<Vec<_>>().join(".")
    }
    /// For `nodes.<id>...`: the node id.
    pub fn node_id(&self) -> Option<&str> {
        (self.root == "nodes").then(|| self.path.first().map(String::as_str)).flatten()
    }
    /// For `vars.<key>...`: the variable key.
    pub fn var_key(&self) -> Option<&str> {
        (self.root == "vars").then(|| self.path.first().map(String::as_str)).flatten()
    }
}

type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

pub struct ExprEngine {
    env: Environment<'static>,
}

impl Default for ExprEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl ExprEngine {
    pub fn new() -> Self {
        Self::build(HashMap::new(), Arc::new(Utc::now))
    }

    /// With the secret values of a run, for `secret('NAME')`.
    pub fn with_secrets(secrets: HashMap<String, String>) -> Self {
        Self::build(secrets, Arc::new(Utc::now))
    }

    /// With a fixed clock, for tests.
    pub fn with_clock(clock: impl Fn() -> DateTime<Utc> + Send + Sync + 'static) -> Self {
        Self::build(HashMap::new(), Arc::new(clock))
    }

    fn build(secrets: HashMap<String, String>, clock: Clock) -> Self {
        let mut env = Environment::new();
        let syntax = SyntaxConfig::builder()
            .block_delimiters("${%", "%}")
            .variable_delimiters("${{", "}}")
            .comment_delimiters("${#", "#}")
            .build()
            .expect("the delimiters are valid");
        env.set_syntax(syntax);
        env.set_undefined_behavior(UndefinedBehavior::Chainable);
        env.set_fuel(Some(FUEL));
        env.set_keep_trailing_newline(true);
        env.set_formatter(format_value);

        env.add_filter("default", default_filter);
        env.add_filter("d", default_filter);
        env.add_filter("from_json", |s: String| -> Result<Value, Error> {
            serde_json::from_str::<Json>(&s).map(Value::from_serialize).map_err(|e| Error::new(ErrorKind::InvalidOperation, format!("from_json: {e}")))
        });
        env.add_filter("domain", |s: String| domain_of(&s));
        env.add_filter("first_word", |s: String| s.split_whitespace().next().unwrap_or("").to_string());
        env.add_filter("strip_html", |s: String| strip_html(&s));
        env.add_filter("truncate", truncate_filter);
        env.add_filter("words", |s: String, n: usize| s.split_whitespace().take(n).collect::<Vec<_>>().join(" "));
        env.add_filter("slug", |s: String| slug(&s));
        env.add_filter("date", date_filter);
        env.add_filter("add_days", |s: String, n: i64| add_time(&s, Duration::try_days(n)));
        env.add_filter("add_hours", |s: String, n: i64| add_time(&s, Duration::try_hours(n)));
        env.add_filter("pick", |v: Value, keys: Rest<String>| pick_keys(&v, &keys, true));
        env.add_filter("omit", |v: Value, keys: Rest<String>| pick_keys(&v, &keys, false));
        env.add_filter("md5", |s: String| hex(&Md5::digest(s.as_bytes())));
        env.add_filter("sha256", |s: String| hex(&Sha256::digest(s.as_bytes())));

        env.add_function("uuid", || uuid::Uuid::now_v7().to_string());
        env.add_function("now", move || clock().to_rfc3339_opts(SecondsFormat::Secs, true));
        env.add_function("get", get_function);
        let secrets = Arc::new(secrets);
        env.add_function("secret", move |name: String| -> Result<String, Error> {
            secrets.get(&name).cloned().ok_or_else(|| Error::new(ErrorKind::InvalidOperation, format!("secret_not_found: {name}")))
        });
        ExprEngine { env }
    }

    /// Is this text one `${{ expr }}` and nothing else? Gives the expression inside.
    pub fn whole_expression(text: &str) -> Option<&str> {
        let t = text.trim();
        let inner = t.strip_prefix("${{")?.strip_suffix("}}")?;
        (t.matches("${{").count() == 1 && t.matches("}}").count() == 1 && !t.contains("${%") && !t.contains("${#")).then_some(inner)
    }

    /// Check the syntax of a text with expressions. `Ok` for plain text.
    pub fn check(&self, text: &str) -> Result<(), ExprError> {
        if !super::schema_kit::has_expression(text) {
            return Ok(());
        }
        match Self::whole_expression(text) {
            Some(inner) => self.env.compile_expression(inner).map(|_| ()),
            None => self.env.template_from_str(text).map(|_| ()),
        }
        .map_err(|e| ExprError::syntax(clean_message(&e)))
    }

    /// What the text reads. The syntax must be right (call `check` first).
    pub fn references(&self, text: &str) -> Vec<Reference> {
        if !super::schema_kit::has_expression(text) {
            return vec![];
        }
        let Ok(t) = self.env.template_from_str(text) else {
            return vec![];
        };
        let mut out = BTreeSet::new();
        for name in t.undeclared_variables(true) {
            let mut parts = name.split('.').map(str::to_string);
            let root = parts.next().unwrap_or_default();
            if FUNCTIONS.contains(&root.as_str()) {
                continue;
            }
            out.insert(Reference { root, path: parts.collect() });
        }
        out.into_iter().collect()
    }

    /// The names of the secrets the text uses: `secret('SEARCH_API_KEY')`.
    pub fn secret_names(text: &str) -> Vec<String> {
        static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r#"secret\(\s*['"]([A-Za-z0-9_]+)['"]\s*\)"#).unwrap());
        if !super::schema_kit::has_expression(text) {
            return vec![];
        }
        RE.captures_iter(text).map(|c| c[1].to_string()).collect()
    }

    /// Evaluate a text. A text that is exactly one expression gives the typed value. Else a string.
    /// `ctx` is a JSON object with `json`, `trigger`, `nodes`, `vars`, `run`, `workflow`.
    pub fn eval(&self, text: &str, ctx: &Json) -> Result<Json, ExprError> {
        if !super::schema_kit::has_expression(text) {
            return Ok(Json::String(text.to_string()));
        }
        let mj_ctx = Value::from_serialize(ctx);
        if let Some(inner) = Self::whole_expression(text) {
            let expr = self.env.compile_expression(inner).map_err(|e| ExprError::syntax(clean_message(&e)))?;
            let v = expr.eval(&mj_ctx).map_err(|e| self.eval_error(text, ctx, &e))?;
            if v.is_undefined() {
                return Err(self.missing(text, ctx));
            }
            return serde_json::to_value(&v).map_err(|e| ExprError::eval(format!("the value cannot be used as JSON: {e}")));
        }
        let out = self.env.render_str(text, &mj_ctx).map_err(|e| self.eval_error(text, ctx, &e))?;
        if out.len() > MAX_OUTPUT_BYTES {
            return Err(ExprError::eval(format!("the text is longer than {MAX_OUTPUT_BYTES} bytes")));
        }
        Ok(Json::String(out))
    }

    /// Evaluate every string in a params value (objects and lists are walked). Keys are not evaluated.
    pub fn eval_params(&self, params: &Json, ctx: &Json) -> Result<Json, ParamError> {
        self.walk(params, ctx, "params")
    }

    fn walk(&self, v: &Json, ctx: &Json, path: &str) -> Result<Json, ParamError> {
        match v {
            Json::String(s) => self.eval(s, ctx).map_err(|error| ParamError { path: path.to_string(), error }),
            Json::Array(a) => a.iter().enumerate().map(|(i, x)| self.walk(x, ctx, &format!("{path}[{i}]"))).collect::<Result<Vec<_>, _>>().map(Json::Array),
            Json::Object(m) => {
                let mut out = Map::new();
                for (k, x) in m {
                    out.insert(k.clone(), self.walk(x, ctx, &format!("{path}.{k}"))?);
                }
                Ok(Json::Object(out))
            }
            other => Ok(other.clone()),
        }
    }

    fn eval_error(&self, text: &str, ctx: &Json, e: &Error) -> ExprError {
        let msg = clean_message(e);
        if let Some(rest) = msg.split("secret_not_found: ").nth(1) {
            return ExprError { code: "secret_not_found", message: format!("The secret {} does not exist.", rest.split_whitespace().next().unwrap_or("")) };
        }
        if e.kind() == ErrorKind::UndefinedError {
            return self.missing(text, ctx);
        }
        ExprError::eval(msg)
    }

    /// The error for a missing value. It names the first path in the text that has nothing behind it.
    fn missing(&self, text: &str, ctx: &Json) -> ExprError {
        let hole = self.references(text).into_iter().find_map(|r| first_hole(&r, ctx));
        let message = match hole {
            Some(path) => format!("`{path}` has no value. Use `| default('')` if it may be missing."),
            None => format!("`{}` gave no value. Use `| default('')` if it may be missing.", text.trim()),
        };
        ExprError::eval(message)
    }
}

/// The deepest reference whose last known step is missing: `nodes.fetch_site.json.text` when `fetch_site` is not in `nodes`.
fn first_hole(r: &Reference, ctx: &Json) -> Option<String> {
    let mut cur = ctx.get(&r.root)?;
    for seg in &r.path {
        match cur {
            Json::Object(m) => match m.get(seg) {
                Some(next) => cur = next,
                None => return Some(r.full()),
            },
            Json::Array(a) => match seg.parse::<usize>().ok().and_then(|i| a.get(i)) {
                Some(next) => cur = next,
                None => return Some(r.full()),
            },
            _ => return None,
        }
    }
    None
}

/// MiniJinja error text without the template dump.
fn clean_message(e: &Error) -> String {
    let mut s = match e.detail() {
        Some(d) => format!("{}: {d}", e.kind()),
        None => e.kind().to_string(),
    };
    if let Some(line) = e.line() {
        let _ = write!(s, " (line {line})");
    }
    s
}

/// Prints a value into a text. Undefined is an error. `null` is empty. Lists and objects are JSON.
fn format_value(out: &mut minijinja::Output, _state: &minijinja::State, value: &Value) -> Result<(), Error> {
    match value.kind() {
        ValueKind::Undefined => Err(Error::new(ErrorKind::UndefinedError, "a value is missing")),
        ValueKind::None => Ok(()),
        ValueKind::Seq | ValueKind::Map => {
            let json = serde_json::to_string(value).map_err(|e| Error::new(ErrorKind::InvalidOperation, e.to_string()))?;
            out.write_str(&json).map_err(|_| Error::new(ErrorKind::WriteFailure, "write failed"))
        }
        _ => write!(out, "{value}").map_err(|_| Error::new(ErrorKind::WriteFailure, "write failed")),
    }
}

/// `default(x, "text")`: the text when x is missing or `null`. With `boolean = true`, also when x is empty or false.
fn default_filter(v: Value, fallback: Option<Value>, boolean: Option<bool>) -> Value {
    let replace = v.is_undefined() || v.is_none() || (boolean.unwrap_or(false) && !v.is_true());
    if replace { fallback.unwrap_or_else(|| Value::from("")) } else { v }
}

/// `truncate(length = 255, killwords = false, end = "...")`: cut a long text. Without `killwords` it cuts at a word end.
fn truncate_filter(s: String, length: Option<usize>, killwords: Option<bool>, end: Option<String>) -> String {
    let length = length.unwrap_or(255);
    let end = end.unwrap_or_else(|| "...".to_string());
    if s.chars().count() <= length {
        return s;
    }
    let keep = length.saturating_sub(end.chars().count());
    let mut cut: String = s.chars().take(keep).collect();
    if !killwords.unwrap_or(false) {
        if let Some(i) = cut.rfind(char::is_whitespace) {
            cut.truncate(i);
        }
    }
    format!("{}{end}", cut.trim_end())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `ana@acme.com` and `https://www.acme.com/x?y` both give `acme.com`.
pub fn domain_of(s: &str) -> String {
    let s = s.trim();
    let host = if let Some((_, d)) = s.rsplit_once('@') {
        d.split(['/', '?', '#', ':']).next().unwrap_or("")
    } else {
        let rest = s.split_once("://").map_or(s, |(_, r)| r);
        let rest = rest.rsplit_once('@').map_or(rest, |(_, r)| r);
        rest.split(['/', '?', '#', ':']).next().unwrap_or("")
    };
    let host = host.to_lowercase();
    host.strip_prefix("www.").map(str::to_string).unwrap_or(host)
}

pub fn slug(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    out.trim_end_matches('-').to_string()
}

fn strip_html(s: &str) -> String {
    html2text::from_read(s.as_bytes(), 100_000).unwrap_or_else(|_| s.to_string()).trim().to_string()
}

pub(crate) fn parse_time(s: &str) -> Result<DateTime<Utc>, Error> {
    let s = s.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(t.with_timezone(&Utc));
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(d.and_hms_opt(0, 0, 0).expect("midnight exists").and_utc());
    }
    Err(Error::new(ErrorKind::InvalidOperation, format!("`{s}` is not an ISO time (like 2026-10-25T09:00:00Z)")))
}

fn date_filter(s: String, fmt: String, tz: Option<String>) -> Result<String, Error> {
    let t = parse_time(&s)?;
    let zone: chrono_tz::Tz = match tz {
        Some(z) => z.parse().map_err(|_| Error::new(ErrorKind::InvalidOperation, format!("`{z}` is not a time zone name")))?,
        None => chrono_tz::UTC,
    };
    let mut out = String::new();
    write!(out, "{}", t.with_timezone(&zone).format(&fmt)).map_err(|_| Error::new(ErrorKind::InvalidOperation, format!("`{fmt}` is not a valid date format")))?;
    Ok(out)
}

fn add_time(s: &str, by: Option<Duration>) -> Result<String, Error> {
    let by = by.ok_or_else(|| Error::new(ErrorKind::InvalidOperation, "the time span is too large"))?;
    let t = parse_time(s)?.checked_add_signed(by).ok_or_else(|| Error::new(ErrorKind::InvalidOperation, "the time is out of range"))?;
    Ok(t.to_rfc3339_opts(SecondsFormat::Secs, true))
}

fn pick_keys(v: &Value, keys: &[String], keep: bool) -> Result<Value, Error> {
    let json = serde_json::to_value(v).map_err(|e| Error::new(ErrorKind::InvalidOperation, e.to_string()))?;
    let Json::Object(m) = json else {
        return Err(Error::new(ErrorKind::InvalidOperation, "pick and omit need an object"));
    };
    let out: Map<String, Json> = m.into_iter().filter(|(k, _)| keys.contains(k) == keep).collect();
    Ok(Value::from_serialize(Json::Object(out)))
}

/// `get(obj, "a.b.0.c", default)`: a safe deep read. Never fails.
fn get_function(v: Value, path: String, fallback: Option<Value>) -> Value {
    let mut cur = v;
    for seg in path.split('.').filter(|s| !s.is_empty()) {
        let next = match cur.kind() {
            ValueKind::Seq => seg.parse::<usize>().ok().and_then(|i| cur.get_item_by_index(i).ok()),
            ValueKind::Map => cur.get_attr(seg).ok(),
            _ => None,
        };
        match next {
            Some(n) if !n.is_undefined() => cur = n,
            _ => return fallback.unwrap_or(Value::from(())),
        }
    }
    cur
}

/// Every string in `v` that holds an expression, with its path (`params.prompt`, `params.cases[0].left`).
pub fn expression_strings<'a>(v: &'a Json, base: &str) -> Vec<(String, &'a str)> {
    fn go<'a>(v: &'a Json, path: String, out: &mut Vec<(String, &'a str)>) {
        match v {
            Json::String(s) if super::schema_kit::has_expression(s) => out.push((path, s.as_str())),
            Json::Array(a) => a.iter().enumerate().for_each(|(i, x)| go(x, format!("{path}[{i}]"), out)),
            Json::Object(m) => m.iter().for_each(|(k, x)| go(x, format!("{path}.{k}"), out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    go(v, base.to_string(), &mut out);
    out
}

/// Turn a value into the type that a param schema asks for (section 6.2): `"42"` to a number, `"true"` to a bool.
/// `null` stays `null`. Objects and lists are checked inside.
pub fn coerce_to_schema(v: &Json, schema: &Json) -> Result<Json, String> {
    if v.is_null() {
        return Ok(Json::Null);
    }
    let kind = |v: &Json| match v {
        Json::Null => "null",
        Json::Bool(_) => "boolean",
        Json::Number(_) => "number",
        Json::String(_) => "string",
        Json::Array(_) => "list",
        Json::Object(_) => "object",
    };
    match schema.get("type").and_then(Json::as_str) {
        Some("integer") => match v {
            Json::Number(n) if n.is_i64() || n.is_u64() => Ok(v.clone()),
            Json::Number(n) => n.as_f64().filter(|f| f.fract() == 0.0 && f.abs() < 9e15).map(|f| Json::from(f as i64)).ok_or_else(|| "expected a whole number".to_string()),
            Json::String(s) => s.trim().parse::<i64>().map(Json::from).map_err(|_| format!("expected a whole number, got the text \"{s}\"")),
            other => Err(format!("expected a whole number, got a {}", kind(other))),
        },
        Some("number") => match v {
            Json::Number(_) => Ok(v.clone()),
            Json::String(s) => s.trim().parse::<f64>().ok().filter(|f| f.is_finite()).and_then(serde_json::Number::from_f64).map(Json::Number).ok_or_else(|| format!("expected a number, got the text \"{s}\"")),
            other => Err(format!("expected a number, got a {}", kind(other))),
        },
        Some("boolean") => match v {
            Json::Bool(_) => Ok(v.clone()),
            Json::String(s) if s.trim().eq_ignore_ascii_case("true") => Ok(Json::Bool(true)),
            Json::String(s) if s.trim().eq_ignore_ascii_case("false") => Ok(Json::Bool(false)),
            other => Err(format!("expected true or false, got a {}", kind(other))),
        },
        Some("string") => match v {
            Json::String(_) => Ok(v.clone()),
            Json::Number(n) => Ok(Json::String(n.to_string())),
            Json::Bool(b) => Ok(Json::String(b.to_string())),
            other => Err(format!("expected text, got a {}", kind(other))),
        },
        Some("array") => match v {
            Json::Array(items) => match schema.get("items") {
                Some(item_schema) => items.iter().enumerate().map(|(i, x)| coerce_to_schema(x, item_schema).map_err(|e| format!("item {i}: {e}"))).collect::<Result<Vec<_>, _>>().map(Json::Array),
                None => Ok(v.clone()),
            },
            other => Err(format!("expected a list, got a {}", kind(other))),
        },
        Some("object") => match v {
            Json::Object(m) => {
                let props = schema.get("properties").and_then(Json::as_object);
                let mut out = Map::new();
                for (k, x) in m {
                    let coerced = match props.and_then(|p| p.get(k)) {
                        Some(ps) => coerce_to_schema(x, ps).map_err(|e| format!("{k}: {e}"))?,
                        None => x.clone(),
                    };
                    out.insert(k.clone(), coerced);
                }
                Ok(Json::Object(out))
            }
            other => Err(format!("expected an object, got a {}", kind(other))),
        },
        _ => Ok(v.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx() -> Json {
        json!({
            "json": { "title": "Acme", "n": 3, "price": "42", "list": [1, 2, 3], "nothing": null },
            "trigger": { "lead": { "first_name": "Ana", "company_name": "Acme", "email": "Ana@Acme.com", "website": "https://www.acme.com/about" } },
            "nodes": { "fetch": { "json": { "text": "About us", "tags": ["a", "b"], "meta": { "k": 1 } }, "items": [{ "text": "About us" }] } },
            "vars": { "campaign_id": "c-1", "limit": 5 },
            "run": { "id": "r-1", "mode": "test" }
        })
    }

    fn ev(text: &str) -> Result<Json, ExprError> {
        ExprEngine::new().eval(text, &ctx())
    }

    #[test]
    fn a_whole_field_expression_gives_a_typed_value() {
        assert_eq!(ev("${{ nodes.fetch.json.tags }}").unwrap(), json!(["a", "b"]));
        assert_eq!(ev("${{ nodes.fetch.json.meta }}").unwrap(), json!({ "k": 1 }));
        assert_eq!(ev("${{ json.n }}").unwrap(), json!(3));
        assert_eq!(ev("  ${{ vars.limit }}  ").unwrap(), json!(5));
        assert_eq!(ev("${{ json.nothing }}").unwrap(), Json::Null);
        assert_eq!(ev("${{ json.n > 2 }}").unwrap(), json!(true));
        assert_eq!(ev("${{ json.n + 1 }}").unwrap(), json!(4));
    }

    #[test]
    fn text_around_an_expression_gives_a_string() {
        assert_eq!(ev("n=${{ json.n }}").unwrap(), json!("n=3"));
        assert_eq!(ev("${{ json.n }}${{ json.n }}").unwrap(), json!("33"));
        assert_eq!(ev("List: ${{ json.list }}").unwrap(), json!("List: [1,2,3]"));
        assert_eq!(ev("Meta: ${{ nodes.fetch.json.meta }}").unwrap(), json!("Meta: {\"k\":1}"));
        assert_eq!(ev("Hi ${{ json.nothing }}!").unwrap(), json!("Hi !"), "null prints as empty text");
        assert_eq!(ev("plain text").unwrap(), json!("plain text"));
        assert_eq!(ev("line 1\nline 2\n").unwrap(), json!("line 1\nline 2\n"));
        assert_eq!(ev("a\n${{ json.n }}\n").unwrap(), json!("a\n3\n"), "a trailing newline stays");
    }

    #[test]
    fn email_template_tags_pass_through() {
        assert_eq!(ev("Hi {{first_name}}, {{ai:opener}} and {{first_name|there}}").unwrap(), json!("Hi {{first_name}}, {{ai:opener}} and {{first_name|there}}"));
        assert_eq!(ev("Hi {{first_name}} from ${{ trigger.lead.company_name }}").unwrap(), json!("Hi {{first_name}} from Acme"));
        assert_eq!(ev("${% if json.n > 1 %}Hi {{first_name}}${% endif %}").unwrap(), json!("Hi {{first_name}}"));
    }

    #[test]
    fn a_literal_start_marker_can_be_printed() {
        assert_eq!(ev("${{ '${{' }}").unwrap(), json!("${{"));
        assert_eq!(ev("a ${{ '${{' }} b").unwrap(), json!("a ${{ b"));
    }

    #[test]
    fn blocks_and_comments() {
        assert_eq!(ev("${% for x in json.list %}${{ x }},${% endfor %}").unwrap(), json!("1,2,3,"));
        assert_eq!(ev("a${# hidden #}b").unwrap(), json!("ab"));
    }

    #[test]
    fn a_missing_value_is_an_error_with_the_path() {
        let e = ev("Hi ${{ trigger.lead.nickname }}").unwrap_err();
        assert_eq!(e.code, "expression_error");
        assert!(e.message.contains("trigger.lead.nickname"), "{}", e.message);
        let e = ev("${{ nodes.fetch_sit.json.text }}").unwrap_err();
        assert!(e.message.contains("nodes.fetch_sit.json.text"), "{}", e.message);
        let e = ev("x ${{ nodes.nope.json.a.b }} y").unwrap_err();
        assert!(e.message.contains("nodes.nope.json.a.b"), "{}", e.message);
    }

    #[test]
    fn default_and_is_defined_work_on_missing_values() {
        assert_eq!(ev("${{ json.missing | default('x') }}").unwrap(), json!("x"));
        assert_eq!(ev("${{ nodes.nope.json.text | default('none') }}").unwrap(), json!("none"), "a path through a missing key is fine with default");
        assert_eq!(ev("A ${{ json.missing.deeper | default('') }}B").unwrap(), json!("A B"));
        assert_eq!(ev("${{ json.nothing | default('y') }}").unwrap(), json!("y"), "default treats null as missing");
        assert_eq!(ev("${{ json.title | default('y') }}").unwrap(), json!("Acme"));
        assert_eq!(ev("${{ json.missing is defined }}").unwrap(), json!(false));
        assert_eq!(ev("${{ json.title is defined }}").unwrap(), json!(true));
        assert_eq!(ev("${% if json.missing %}yes${% else %}no${% endif %}").unwrap(), json!("no"));
    }

    #[test]
    fn a_syntax_error_is_found_by_check() {
        let e = ExprEngine::new();
        assert!(e.check("${{ json.a | default( }}").is_err());
        assert!(e.check("${% if x %}no end").is_err());
        assert_eq!(e.check("${{ json.a ").unwrap_err().code, "expression_syntax");
        assert!(e.check("${{ json.a | default('') }}").is_ok());
        assert!(e.check("plain {{first_name}}").is_ok());
        assert!(e.check("").is_ok());
    }

    #[test]
    fn fuel_stops_a_runaway_loop() {
        let e = ev("${% for i in range(100000000) %}x${% endfor %}").unwrap_err();
        assert_eq!(e.code, "expression_error");
        let e = ev("${% for i in range(1000) %}${% for j in range(1000) %}${% for k in range(1000) %}${% endfor %}${% endfor %}${% endfor %}").unwrap_err();
        assert_eq!(e.code, "expression_error");
    }

    #[test]
    fn the_built_in_filters_the_map_lists_exist() {
        let c = json!({ "s": "  Hello World  ", "l": [3, 1, 2], "words": "a b c", "d": {"a": 1, "b": 2}, "num": "3.6", "rows": [{ "k": 1 }, { "k": 2 }] });
        let e = ExprEngine::new();
        let cases: &[(&str, Json)] = &[
            ("${{ s | lower }}", json!("  hello world  ")),
            ("${{ s | upper }}", json!("  HELLO WORLD  ")),
            ("${{ s | trim }}", json!("Hello World")),
            ("${{ s | title }}", json!("  Hello World  ")),
            ("${{ s | replace('World', 'X') | trim }}", json!("Hello X")),
            ("${{ l | length }}", json!(3)),
            ("${{ l | first }}", json!(3)),
            ("${{ l | last }}", json!(2)),
            ("${{ l | join('-') }}", json!("3-1-2")),
            ("${{ words | split(' ') }}", json!(["a", "b", "c"])),
            ("${{ l | list }}", json!([3, 1, 2])),
            ("${{ l | sort }}", json!([1, 2, 3])),
            ("${{ [1, 1, 2] | unique | list }}", json!([1, 2])),
            ("${{ num | float | round }}", json!(4.0)),
            ("${{ num | float | int }}", json!(3)),
            ("${{ l | tojson }}", json!("[3,1,2]")),
            ("${{ words | truncate(5) }}", json!("a b c")),
            ("${{ 'one two three four' | truncate(12) }}", json!("one two...")),
            ("${{ 'one two three four' | truncate(12, true) }}", json!("one two t...")),
            ("${{ 'short' | truncate(12) }}", json!("short")),
            ("${{ rows | map(attribute='k') | list }}", json!([1, 2])),
            ("${{ rows | selectattr('k', 'gt', 1) | list }}", json!([{ "k": 2 }])),
            ("${{ l | select('odd') | list }}", json!([3, 1])),
            ("${{ l | reject('odd') | list }}", json!([2])),
            ("${{ l | batch(2) | list }}", json!([[3, 1], [2]])),
            ("${{ d | items }}", json!([["a", 1], ["b", 2]])),
            ("${{ d | dictsort }}", json!([["a", 1], ["b", 2]])),
            ("${{ 'a\nb' | indent(2) }}", json!("a\n  b")),
        ];
        for (text, want) in cases {
            let got = e.eval(text, &c).unwrap_or_else(|err| panic!("{text}: {err}"));
            assert_eq!(&got, want, "{text}");
        }
    }

    #[test]
    fn the_aikido_filters() {
        assert_eq!(ev("${{ trigger.lead.email | domain }}").unwrap(), json!("acme.com"));
        assert_eq!(ev("${{ trigger.lead.website | domain }}").unwrap(), json!("acme.com"));
        assert_eq!(ev("${{ 'Ana Maria Lee' | first_word }}").unwrap(), json!("Ana"));
        let stripped = ev("${{ '<p>Hello <b>big</b> world</p><script>x</script>' | strip_html }}").unwrap();
        let stripped = stripped.as_str().unwrap();
        assert!(stripped.contains("Hello") && stripped.contains("world") && !stripped.contains('<'), "{stripped}");
        assert_eq!(ev("${{ 'one two three four' | words(2) }}").unwrap(), json!("one two"));
        assert_eq!(ev("${{ 'Hello, World! 2026' | slug }}").unwrap(), json!("hello-world-2026"));
        assert_eq!(ev("${{ '2026-10-25T09:30:00Z' | date('%d %b %Y %H:%M') }}").unwrap(), json!("25 Oct 2026 09:30"));
        assert_eq!(ev("${{ '2026-10-25T20:30:00Z' | date('%Y-%m-%d %H:%M', 'Asia/Dhaka') }}").unwrap(), json!("2026-10-26 02:30"));
        assert_eq!(ev("${{ '2026-10-25' | add_days(3) }}").unwrap(), json!("2026-10-28T00:00:00Z"));
        assert_eq!(ev("${{ '2026-10-25T23:00:00Z' | add_hours(2) }}").unwrap(), json!("2026-10-26T01:00:00Z"));
        assert_eq!(ev("${{ trigger.lead | pick('first_name') }}").unwrap(), json!({ "first_name": "Ana" }));
        assert_eq!(ev("${{ trigger.lead | omit('email', 'website', 'company_name') }}").unwrap(), json!({ "first_name": "Ana" }));
        assert_eq!(ev("${{ 'abc' | md5 }}").unwrap(), json!("900150983cd24fb0d6963f7d28e17f72"));
        assert_eq!(ev("${{ 'abc' | sha256 }}").unwrap(), json!("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"));
        assert_eq!(ev("${{ '{\"a\": [1, 2]}' | from_json }}").unwrap(), json!({ "a": [1, 2] }));
        assert_eq!(ev("${{ ('{\"a\": [1, 2]}' | from_json).a[1] }}").unwrap(), json!(2));
        assert!(ev("${{ 'not json' | from_json }}").is_err());
        assert!(ev("${{ 'tomorrow' | add_days(1) }}").is_err());
    }

    #[test]
    fn the_functions() {
        assert_eq!(ev("${{ get(nodes, 'fetch.json.tags.1') }}").unwrap(), json!("b"));
        assert_eq!(ev("${{ get(nodes, 'fetch.json.nope.deeper', 'dflt') }}").unwrap(), json!("dflt"));
        assert_eq!(ev("${{ get(nodes, 'nope') }}").unwrap(), Json::Null);
        let u = ev("${{ uuid() }}").unwrap();
        assert_eq!(u.as_str().unwrap().len(), 36);
        assert_ne!(ev("${{ uuid() }}").unwrap(), u);
        let fixed = ExprEngine::with_clock(|| "2026-10-25T09:00:00Z".parse().unwrap());
        assert_eq!(fixed.eval("${{ now() }}", &ctx()).unwrap(), json!("2026-10-25T09:00:00Z"));
        assert_eq!(fixed.eval("${{ now() | add_days(1) }}", &ctx()).unwrap(), json!("2026-10-26T09:00:00Z"));
    }

    #[test]
    fn secrets() {
        let e = ExprEngine::with_secrets(HashMap::from([("SEARCH_API_KEY".to_string(), "s3cr3t".to_string())]));
        assert_eq!(e.eval("Bearer ${{ secret('SEARCH_API_KEY') }}", &ctx()).unwrap(), json!("Bearer s3cr3t"));
        let err = e.eval("${{ secret('OTHER') }}", &ctx()).unwrap_err();
        assert_eq!(err.code, "secret_not_found");
        assert!(err.message.contains("OTHER"));
        assert_eq!(ExprEngine::new().eval("${{ secret('SEARCH_API_KEY') }}", &ctx()).unwrap_err().code, "secret_not_found");
        assert_eq!(ExprEngine::secret_names("a ${{ secret('A_B') }} ${{ secret(\"C\") }}"), vec!["A_B", "C"]);
        assert!(ExprEngine::secret_names("secret('X') without markers").is_empty());
    }

    #[test]
    fn references_are_found() {
        let e = ExprEngine::new();
        let refs = e.references("${{ nodes.fetch.json.text | default('') }} and ${{ vars.campaign_id }} ${{ trigger.lead.email }} ${{ secret('X') }} ${{ now() }}");
        let full: Vec<String> = refs.iter().map(Reference::full).collect();
        assert!(full.contains(&"nodes.fetch.json.text".to_string()), "{full:?}");
        assert!(full.contains(&"vars.campaign_id".to_string()));
        assert!(full.contains(&"trigger.lead.email".to_string()));
        assert!(!full.iter().any(|f| f.starts_with("secret") || f.starts_with("now")), "{full:?}");
        let node = refs.iter().find_map(Reference::node_id);
        assert_eq!(node, Some("fetch"));
        assert_eq!(refs.iter().find_map(Reference::var_key), Some("campaign_id"));
        // a loop variable is not a reference
        let r = e.references("${% for x in json.list %}${{ x.name }}${% endfor %}");
        assert_eq!(r.iter().map(Reference::full).collect::<Vec<_>>(), vec!["json.list"]);
        assert!(e.references("plain {{first_name}}").is_empty());
    }

    #[test]
    fn params_are_walked_and_the_error_has_the_path() {
        let e = ExprEngine::new();
        let params = json!({
            "url": "${{ trigger.lead.website }}",
            "limit": "${{ vars.limit }}",
            "headers": { "X-Co": "co=${{ trigger.lead.company_name }}" },
            "list": ["a", "${{ json.n }}"],
            "flag": true
        });
        assert_eq!(
            e.eval_params(&params, &ctx()).unwrap(),
            json!({ "url": "https://www.acme.com/about", "limit": 5, "headers": { "X-Co": "co=Acme" }, "list": ["a", 3], "flag": true })
        );
        let bad = json!({ "cases": [{ "left": "ok" }, { "left": "${{ json.nope }}" }] });
        let err = e.eval_params(&bad, &ctx()).unwrap_err();
        assert_eq!(err.path, "params.cases[1].left");
        assert_eq!(err.error.code, "expression_error");
    }

    #[test]
    fn expression_strings_lists_the_places() {
        let p = json!({ "a": "x", "b": "${{ vars.k }}", "c": [{ "d": "${% if x %}y${% endif %}" }, "plain"], "e": 1 });
        let mut got = expression_strings(&p, "params");
        got.sort();
        assert_eq!(got.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(), vec!["params.b", "params.c[0].d"]);
    }

    #[test]
    fn values_are_coerced_to_the_param_type() {
        let s = json!({ "type": "integer" });
        assert_eq!(coerce_to_schema(&json!("42"), &s).unwrap(), json!(42));
        assert_eq!(coerce_to_schema(&json!(7.0), &s).unwrap(), json!(7));
        assert!(coerce_to_schema(&json!("4x"), &s).is_err());
        assert!(coerce_to_schema(&json!(7.5), &s).is_err());
        assert_eq!(coerce_to_schema(&json!("2.5"), &json!({ "type": "number" })).unwrap(), json!(2.5));
        assert_eq!(coerce_to_schema(&json!("TRUE"), &json!({ "type": "boolean" })).unwrap(), json!(true));
        assert!(coerce_to_schema(&json!("yes"), &json!({ "type": "boolean" })).is_err());
        assert_eq!(coerce_to_schema(&json!(5), &json!({ "type": "string" })).unwrap(), json!("5"));
        assert_eq!(coerce_to_schema(&Json::Null, &s).unwrap(), Json::Null);
        let obj = json!({ "type": "object", "properties": { "n": { "type": "integer" }, "tags": { "type": "array", "items": { "type": "string" } } } });
        assert_eq!(coerce_to_schema(&json!({ "n": "3", "tags": [1, "b"], "extra": "x" }), &obj).unwrap(), json!({ "n": 3, "tags": ["1", "b"], "extra": "x" }));
        let err = coerce_to_schema(&json!({ "n": "three" }), &obj).unwrap_err();
        assert!(err.starts_with("n:"), "{err}");
    }

    #[test]
    fn whole_expression_detection() {
        assert_eq!(ExprEngine::whole_expression("${{ a }}"), Some(" a "));
        assert_eq!(ExprEngine::whole_expression("  ${{a}}\n"), Some("a"));
        assert_eq!(ExprEngine::whole_expression("x ${{ a }}"), None);
        assert_eq!(ExprEngine::whole_expression("${{ a }} ${{ b }}"), None);
        assert_eq!(ExprEngine::whole_expression("${{ '${{' }}"), None);
        assert_eq!(ExprEngine::whole_expression("${% if a %}${{ b }}${% endif %}"), None);
    }
}
