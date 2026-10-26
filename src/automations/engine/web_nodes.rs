//! The web nodes (build map 7.4): `http.request` and `web.fetch_page`. Both go out through the safe fetcher
//! (`net::safe_fetch`): private, loopback and link-local addresses are refused, the connection goes to the
//! address that was checked, and every redirect is checked again.
use super::env::NodeEnv;
use super::nodes::get_path;
use crate::automations::registry::{Item, NodeCtx, NodeError, NodeInput, NodeOutput, NodeSpec, NodeType};
use crate::automations::robots::{self, Robots};
use crate::net::safe_fetch::{FetchError, HopCheck, RawRequest, RequestOptions};
use async_trait::async_trait;
use base64::Engine as _;
use reqwest::Url;
use serde_json::{Map, Value, json};
use std::sync::Arc;
use std::time::Duration;

const MAX_HTTP_BODY: usize = 5 * 1024 * 1024;
const MAX_PAGE_BYTES: usize = 2 * 1024 * 1024;
const MAX_LINKS: usize = 100;

/// The node for a spec, or the spec back when it is not a web node.
#[allow(clippy::result_large_err)]
pub fn make(spec: NodeSpec) -> Result<Box<dyn NodeType>, NodeSpec> {
    match spec.type_id {
        "http.request" => Ok(Box::new(HttpRequest(spec))),
        "web.fetch_page" => Ok(Box::new(FetchPage(spec))),
        _ => Err(spec),
    }
}

fn bad(msg: impl Into<String>) -> NodeError {
    NodeError::new("invalid_param_value", msg)
}

/// The params of the node for one item, with every expression evaluated.
pub(super) fn evaluated(ctx: &NodeCtx<'_>, item: &Item, idx: usize) -> Result<Map<String, Value>, NodeError> {
    match ctx.value(item, idx, &Value::Object(ctx.params.clone()))? {
        Value::Object(m) => Ok(m),
        _ => Ok(Map::new()),
    }
}

fn user_agent(env: &NodeEnv) -> String {
    match &env.public_url {
        Some(u) => format!("AikidoEmail-Automation/{} (+{u})", env!("CARGO_PKG_VERSION")),
        None => format!("AikidoEmail-Automation/{}", env!("CARGO_PKG_VERSION")),
    }
}

fn fetch_err(e: FetchError) -> NodeError {
    let msg = e.to_string();
    match e {
        FetchError::BadUrl(_) => NodeError::new("invalid_url", msg),
        FetchError::Blocked(m) if m.starts_with("domain_blocked:") => {
            NodeError::new("web_domain_blocked", format!("{} is on the block list (Automations > Settings > fetch_blocked_domains).", m.trim_start_matches("domain_blocked:")))
        }
        FetchError::Blocked(_) => NodeError::new("http_blocked", msg),
        FetchError::TooManyRedirects => NodeError::new("http_too_many_redirects", msg),
        FetchError::TooLarge => NodeError::new("http_response_too_large", msg),
        FetchError::NotText(_) | FetchError::NotImage(_) => NodeError::new("web_not_text", msg),
        FetchError::Status(s) => NodeError { code: format!("http_status_{s}"), message: msg, status: Some(s) },
        FetchError::Network(_) => NodeError::new("http_error", msg),
    }
}

fn text_of(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        other => Some(other.to_string()),
    }
}

fn to_item(parent: &Item, v: Value) -> Item {
    match v {
        Value::Object(m) => parent.child(m),
        other => parent.child(json!({ "value": other }).as_object().cloned().unwrap_or_default()),
    }
}

// ---------- http.request ----------

struct HttpRequest(NodeSpec);

#[async_trait]
impl NodeType for HttpRequest {
    fn spec(&self) -> &NodeSpec {
        &self.0
    }

    async fn run(&self, ctx: &NodeCtx<'_>, input: NodeInput) -> Result<NodeOutput, NodeError> {
        let env = ctx.env()?;
        let item = input.items.first().ok_or_else(|| bad("no input item"))?;
        // `pagination.stop_when` reads `page.*`, which exists only after a page was read: it is evaluated later
        let stop_when = ctx.raw("pagination").and_then(|p| p.get("stop_when")).and_then(Value::as_str).map(str::to_string);
        let p = {
            let mut params = ctx.params.clone();
            if let Some(Value::Object(pg)) = params.get_mut("pagination") {
                pg.remove("stop_when");
            }
            match ctx.value(item, input.first_index, &Value::Object(params))? {
                Value::Object(m) => m,
                _ => Map::new(),
            }
        };
        let pagination = Pagination::parse(p.get("pagination"), stop_when)?;
        env.count_call()?;

        let method = p.get("method").and_then(Value::as_str).unwrap_or("GET").to_ascii_uppercase();
        let m = reqwest::Method::from_bytes(method.as_bytes()).map_err(|_| bad(format!("`{method}` is not a method")))?;
        if !["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"].contains(&method.as_str()) {
            return Err(bad(format!("`{method}` is not a method this node can use")));
        }
        let mut url = p.get("url").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| bad("`url` is empty"))?.to_string();
        if !url.contains("://") {
            url = format!("https://{url}");
        }
        let mut parsed = Url::parse(&url).map_err(|_| NodeError::new("invalid_url", "The url is not a valid web address."))?;
        if let Some(Value::Object(q)) = p.get("query") {
            let mut pairs = parsed.query_pairs_mut();
            for (k, v) in q {
                if let Some(t) = text_of(v) {
                    pairs.append_pair(k, &t);
                }
            }
        }

        let mut headers: Vec<(String, String)> = vec![];
        if let Some(Value::Object(h)) = p.get("headers") {
            for (k, v) in h {
                let lower = k.to_ascii_lowercase();
                if lower == "host" || lower == "content-length" {
                    return Err(bad(format!("The header `{k}` is not allowed.")));
                }
                reqwest::header::HeaderName::from_bytes(k.as_bytes()).map_err(|_| bad(format!("`{k}` is not a valid header name")))?;
                if let Some(t) = text_of(v) {
                    reqwest::header::HeaderValue::from_str(&t).map_err(|_| bad(format!("The value of the header `{k}` is not valid")))?;
                    headers.push((k.clone(), t));
                }
            }
        }

        // auth: the secret is read here and never shown in the run data
        if let Some(Value::Object(a)) = p.get("auth") {
            let kind = a.get("type").and_then(Value::as_str).unwrap_or("none");
            if kind != "none" {
                let name = a.get("secret").and_then(Value::as_str).ok_or_else(|| bad("`auth.secret` is missing"))?;
                let secret = env.secrets.get(name).ok_or_else(|| NodeError::new("secret_not_found", format!("The secret {name} does not exist.")))?;
                match kind {
                    "bearer" => headers.push(("Authorization".into(), format!("Bearer {secret}"))),
                    "basic" => {
                        let user = a.get("user").and_then(Value::as_str).unwrap_or("");
                        headers.push(("Authorization".into(), format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{user}:{secret}")))));
                    }
                    "header" => {
                        let h = a.get("header_name").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or_else(|| bad("`auth.header_name` is missing"))?;
                        reqwest::header::HeaderName::from_bytes(h.as_bytes()).map_err(|_| bad(format!("`{h}` is not a valid header name")))?;
                        headers.push((h.to_string(), secret.to_string()));
                    }
                    other => return Err(bad(format!("`auth.type` `{other}` is not known"))),
                }
            }
        }

        // body
        let body_value = p.get("body").filter(|v| !v.is_null());
        // paging in the body: the body is an object, and the page param is set on it for each page
        let mut body_object: Option<Map<String, Value>> = None;
        if pagination.as_ref().is_some_and(|pg| pg.in_body) {
            body_object = match body_value {
                None => Some(Map::new()),
                Some(Value::Object(o)) => Some(o.clone()),
                Some(_) => return Err(bad("With `pagination.location` `body`, `body` must be an object.")),
            };
        }
        let default_type = if matches!(method.as_str(), "POST" | "PUT" | "PATCH") && (body_value.is_some() || body_object.is_some()) { "json" } else { "none" };
        let body_type = p.get("body_type").and_then(Value::as_str).unwrap_or(default_type);
        let has_header = |name: &str, hs: &[(String, String)]| hs.iter().any(|(k, _)| k.eq_ignore_ascii_case(name));
        if body_object.is_some() && body_type != "json" {
            return Err(bad("With `pagination.location` `body`, `body_type` must be `json`."));
        }
        let body: Option<Vec<u8>> = match (body_type, body_value) {
            ("json", _) if body_object.is_some() => Some(Vec::new()),
            ("none", _) | (_, None) => None,
            ("json", Some(Value::String(s))) => Some(s.clone().into_bytes()),
            ("json", Some(v)) => Some(serde_json::to_vec(v).map_err(|e| bad(e.to_string()))?),
            ("form", Some(Value::Object(o))) => {
                let pairs: Vec<String> = o.iter().filter_map(|(k, v)| text_of(v).map(|t| format!("{}={}", crate::automations::internal_call::urlencode(k), crate::automations::internal_call::urlencode(&t)))).collect();
                Some(pairs.join("&").into_bytes())
            }
            ("form", Some(_)) => return Err(bad("A `form` body must be an object.")),
            ("text", Some(v)) => Some(text_of(v).unwrap_or_default().into_bytes()),
            (other, _) => return Err(bad(format!("`body_type` `{other}` is not known"))),
        };
        if body.is_some() && !has_header("content-type", &headers) {
            let ct = match body_type {
                "json" => "application/json",
                "form" => "application/x-www-form-urlencoded",
                _ => "text/plain; charset=utf-8",
            };
            headers.push(("Content-Type".into(), ct.into()));
        }
        if !has_header("accept", &headers) {
            headers.push(("Accept".into(), "application/json, text/plain, */*".into()));
        }

        let timeout = p.get("timeout_secs").and_then(Value::as_u64).unwrap_or(30).clamp(1, 120);
        let follow = p.get("follow_redirects").and_then(Value::as_bool).unwrap_or(true);
        let fail_on_status = p.get("fail_on_status").and_then(Value::as_bool).unwrap_or(true);
        let opts = RequestOptions {
            any_port: true,
            allow_private: env.settings.allow_private_network,
            max_redirects: if follow { 5 } else { 0 },
            max_bytes: MAX_HTTP_BODY,
            timeout: Duration::from_secs(timeout),
            user_agent: user_agent(env),
            hop_check: None,
        };
        let host = parsed.host_str().unwrap_or_default().to_string();
        let format = p.get("response_format").and_then(Value::as_str).unwrap_or("auto").to_string();
        let split = p.get("split").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
        if let Some(pg) = &pagination {
            pg.check_stops(split.is_some())?;
        }
        let req = Req { method: m, headers, format, fail_on_status, opts };

        let mut logs = vec![];
        if env.test_run && method != "GET" && method != "HEAD" {
            logs.push(format!("Test run: this {method} request to {host} was really sent."));
        }

        let Some(pg) = pagination else {
            let page = fetch_once(env, &req, &parsed, body).await?;
            let items = page_items(item, page, split.as_deref(), None)?;
            let mut out = NodeOutput::main(items);
            out.logs = logs;
            return Ok(out);
        };

        // ---- several pages
        let first_url = parsed.clone();
        let mut url = parsed;
        let mut value = pg.first_value();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::from([url.to_string()]);
        let mut out_items: Vec<Item> = vec![];
        let mut number: u32 = 0;
        let why = loop {
            number += 1;
            if number > 1 {
                env.count_call()?;
                if ctx.control.should_stop() {
                    return Err(NodeError::new("cancelled", "The run was cancelled."));
                }
            }
            // this page's request: the page param goes in the query or in the body
            let mut page_url = url.clone();
            let mut page_body = body.clone();
            if let Some(o) = &body_object {
                let mut o = o.clone();
                if let (Some(param), Some(v)) = (&pg.param, &value) {
                    o.insert(param.clone(), v.clone());
                }
                page_body = Some(serde_json::to_vec(&Value::Object(o)).map_err(|e| bad(e.to_string()))?);
            } else if let (Some(param), Some(v)) = (&pg.param, &value) {
                set_query(&mut page_url, param, &text_of(v).unwrap_or_default());
            }
            let page = fetch_once(env, &req, &page_url, page_body).await?;
            let list: Option<Vec<Value>> = match &split {
                Some(path) => Some(split_list(&page.body, path)?),
                None => None,
            };
            let count = list.as_ref().map(Vec::len);
            let page_ctx = json!({ "number": number, "status": page.status, "headers": page.headers, "body": page.body, "url": page.url, "items": list, "count": count });
            let reading = Value::Object(Map::from_iter([("status".into(), json!(page.status)), ("headers".into(), Value::Object(page.headers.clone())), ("body".into(), page.body.clone()), ("url".into(), json!(page.url))]));
            let final_url = page.url.clone();
            out_items.extend(page_items(item, page, split.as_deref(), Some(number))?);
            if out_items.len() > super::run::MAX_ITEMS {
                return Err(NodeError::new("too_many_items", format!("The pages gave more than {} items. Lower `max_pages`.", super::run::MAX_ITEMS)));
            }

            // why stop?
            if let Some(expr) = &pg.stop_when {
                let v = ctx.eval.value_with(item, input.first_index, &Value::String(expr.clone()), "page", page_ctx).map_err(|e| NodeError::new("pagination_stop_error", format!("pagination.stop_when: {}", e.message)))?;
                if truthy(&v) {
                    break "stop_when was true";
                }
            }
            let next = match pg.mode {
                Mode::NextUrl => {
                    let Some(found) = path_text(&reading, &pg.next) else { break "the answer has no next address" };
                    let next = Url::parse(&final_url).and_then(|u| u.join(&found)).map_err(|_| NodeError::new("pagination_bad_url", format!("`{}` in the answer is not a web address.", pg.next)))?;
                    if (next.scheme(), next.host_str(), next.port_or_known_default()) != (first_url.scheme(), first_url.host_str(), first_url.port_or_known_default()) {
                        return Err(NodeError::new("pagination_cross_origin", format!("The next address ({}) is on another host than the first request. It is refused so that the auth header is not sent there.", next.host_str().unwrap_or(""))));
                    }
                    if !seen.insert(next.to_string()) {
                        break "the next address was seen before";
                    }
                    Next::Url(next)
                }
                Mode::Cursor => match reading_value(&reading, &pg.next).filter(|v| !v.is_null() && v.as_str() != Some("")) {
                    Some(c) => Next::Value(c),
                    None => break "the answer has no next cursor",
                },
                Mode::Page | Mode::Offset => {
                    if count == Some(0) {
                        break "the page was empty";
                    }
                    let cur = value.as_ref().and_then(Value::as_i64).unwrap_or(0);
                    let step = match (pg.mode, pg.step, count) {
                        (Mode::Page, _, _) => 1,
                        (_, Some(s), _) => s,
                        (_, None, Some(c)) => c as i64,
                        (_, None, None) => 1,
                    };
                    Next::Value(json!(cur + step))
                }
            };
            if number >= pg.max_pages {
                break "max_pages was reached";
            }
            match next {
                Next::Url(u) => url = u,
                Next::Value(v) => value = Some(v),
            }
            if pg.delay_ms > 0 {
                ctx.control.sleep(Duration::from_millis(pg.delay_ms)).await;
            }
        };
        logs.push(format!("Pagination: read {number} page(s), {} item(s). Stopped because {why}.", out_items.len()));
        let mut out = NodeOutput::main(out_items);
        out.logs = logs;
        Ok(out)
    }
}

// ---------- http.request: one request, and pagination ----------

struct Req {
    method: reqwest::Method,
    headers: Vec<(String, String)>,
    format: String,
    fail_on_status: bool,
    opts: RequestOptions,
}

/// One answer, read.
struct Page {
    status: u16,
    headers: Map<String, Value>,
    body: Value,
    url: String,
}

async fn fetch_once(env: &NodeEnv, req: &Req, url: &Url, body: Option<Vec<u8>>) -> Result<Page, NodeError> {
    let resp = env.fetcher.request(RawRequest { method: req.method.clone(), url: url.to_string(), headers: req.headers.clone(), body }, &req.opts).await.map_err(fetch_err)?;
    let looks_json = resp.content_type.contains("json");
    let body_out: Value = match req.format.as_str() {
        "text" => Value::String(String::from_utf8_lossy(&resp.body).into_owned()),
        "json" => serde_json::from_slice(&resp.body).map_err(|e| NodeError::new("http_bad_json", format!("The answer is not JSON ({e}).")))?,
        _ if resp.body.is_empty() => Value::Null,
        _ if looks_json => serde_json::from_slice(&resp.body).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&resp.body).into_owned())),
        _ => Value::String(String::from_utf8_lossy(&resp.body).into_owned()),
    };
    if req.fail_on_status && resp.status >= 400 {
        let snippet: String = match &body_out {
            Value::String(s) => s.chars().take(300).collect(),
            Value::Null => String::new(),
            other => other.to_string().chars().take(300).collect(),
        };
        return Err(NodeError { code: format!("http_status_{}", resp.status), message: format!("The server answered with status {}. {snippet}", resp.status).trim().to_string(), status: Some(resp.status) });
    }
    let mut headers = Map::new();
    for (k, v) in &resp.headers {
        match headers.get_mut(k) {
            Some(Value::String(old)) => {
                old.push_str(", ");
                old.push_str(v);
            }
            _ => {
                headers.insert(k.clone(), json!(v));
            }
        }
    }
    Ok(Page { status: resp.status, headers, body: body_out, url: resp.url.to_string() })
}

/// The list at `path` in a JSON body.
fn split_list(body: &Value, path: &str) -> Result<Vec<Value>, NodeError> {
    body.as_object().and_then(|o| get_path(o, path)).and_then(Value::as_array).cloned().ok_or_else(|| NodeError::new("split_not_a_list", format!("`{path}` is not a list in the answer.")))
}

/// The output items of one page: one per element (`split`), or the answer itself (`page` is its number in a paginated read).
fn page_items(parent: &Item, page: Page, split: Option<&str>, number: Option<u32>) -> Result<Vec<Item>, NodeError> {
    Ok(match split {
        Some(path) => split_list(&page.body, path)?.into_iter().map(|v| to_item(parent, v)).collect(),
        None => {
            let mut m = Map::new();
            m.insert("status".into(), json!(page.status));
            m.insert("headers".into(), Value::Object(page.headers));
            m.insert("body".into(), page.body);
            m.insert("url".into(), json!(page.url));
            if let Some(n) = number {
                m.insert("page".into(), json!(n));
            }
            vec![parent.child(m)]
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Mode {
    NextUrl,
    Page,
    Offset,
    Cursor,
}

enum Next {
    Url(Url),
    Value(Value),
}

/// The `pagination` param, checked.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Pagination {
    pub mode: Mode,
    /// Where the next address or cursor is, as a path into `{ status, headers, body, url }`.
    pub next: String,
    pub param: Option<String>,
    pub in_body: bool,
    pub start: Option<i64>,
    pub step: Option<i64>,
    pub stop_when: Option<String>,
    pub max_pages: u32,
    pub delay_ms: u64,
}

impl Pagination {
    pub(super) fn parse(v: Option<&Value>, stop_when: Option<String>) -> Result<Option<Pagination>, NodeError> {
        let Some(v) = v.filter(|v| !v.is_null()) else { return Ok(None) };
        let o = v.as_object().ok_or_else(|| bad("`pagination` must be an object."))?;
        let mode = match o.get("mode").and_then(Value::as_str) {
            Some("next_url") => Mode::NextUrl,
            Some("page") => Mode::Page,
            Some("offset") => Mode::Offset,
            Some("cursor") => Mode::Cursor,
            Some(other) => return Err(bad(format!("`pagination.mode` `{other}` is not next_url, page, offset or cursor."))),
            None => return Err(bad("`pagination.mode` is missing.")),
        };
        let param = o.get("param").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
        if mode != Mode::NextUrl && param.is_none() {
            return Err(bad("`pagination.param` is missing: the name of the page, offset or cursor param."));
        }
        let in_body = match o.get("location").and_then(Value::as_str).unwrap_or("query") {
            "query" => false,
            "body" => true,
            other => return Err(bad(format!("`pagination.location` `{other}` is not query or body."))),
        };
        let default_next = if mode == Mode::Cursor { "body.next_cursor" } else { "body.next" };
        let next = o.get("next").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).unwrap_or(default_next).to_string();
        let num = |k: &str| o.get(k).and_then(Value::as_i64);
        let step = num("step");
        if step.is_some_and(|s| s < 1) {
            return Err(bad("`pagination.step` must be 1 or more."));
        }
        let max_pages = num("max_pages").unwrap_or(10);
        if !(1..=100).contains(&max_pages) {
            return Err(bad("`pagination.max_pages` must be 1 to 100."));
        }
        let delay_ms = num("delay_ms").unwrap_or(0);
        if !(0..=10_000).contains(&delay_ms) {
            return Err(bad("`pagination.delay_ms` must be 0 to 10000."));
        }
        let stop_when = stop_when.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        Ok(Some(Pagination { mode, next, param, in_body, start: num("start"), step, stop_when, max_pages: max_pages as u32, delay_ms: delay_ms as u64 }))
    }

    /// Page and offset reading end only on an empty page or a stop condition: one of them must exist.
    pub(super) fn check_stops(&self, has_split: bool) -> Result<(), NodeError> {
        if matches!(self.mode, Mode::Page | Mode::Offset) && !has_split && self.stop_when.is_none() {
            return Err(bad("Pagination by `page` or `offset` needs `split` (it stops at an empty page) or `stop_when`."));
        }
        if self.mode == Mode::Offset && self.step.is_none() && !has_split {
            return Err(bad("Pagination by `offset` needs `step`, or `split` (the step is the number of items of the last page)."));
        }
        Ok(())
    }

    /// The value of the page param on the first request. `None`: the first request has none.
    pub(super) fn first_value(&self) -> Option<Value> {
        match self.mode {
            Mode::Page => Some(json!(self.start.unwrap_or(1))),
            Mode::Offset => Some(json!(self.start.unwrap_or(0))),
            Mode::NextUrl | Mode::Cursor => None,
        }
    }
}

/// For the validator: is `params.pagination` (with `split`) a read that knows when to stop?
pub fn check_pagination(params: &Map<String, Value>) -> Result<(), NodeError> {
    let Some(pg) = params.get("pagination").filter(|v| !v.is_null()) else { return Ok(()) };
    // an expression for the whole block can not be checked here
    if pg.as_str().is_some() {
        return Ok(());
    }
    let has_split = params.get("split").and_then(Value::as_str).is_some_and(|s| !s.trim().is_empty());
    let stop_when = pg.get("stop_when").and_then(Value::as_str).map(str::to_string);
    // values that are expressions can not be read yet: leave them to the run
    let plain = Value::Object(pg.as_object().cloned().unwrap_or_default().into_iter().filter(|(_, v)| !v.as_str().is_some_and(crate::automations::schema_kit::has_expression)).collect());
    match Pagination::parse(Some(&plain), stop_when) {
        Ok(Some(p)) => p.check_stops(has_split),
        Ok(None) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Replace (or add) one query param.
fn set_query(url: &mut Url, key: &str, value: &str) {
    let rest: Vec<(String, String)> = url.query_pairs().filter(|(k, _)| k != key).map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    let mut pairs = url.query_pairs_mut();
    pairs.clear();
    for (k, v) in rest {
        pairs.append_pair(&k, &v);
    }
    pairs.append_pair(key, value);
}

/// A value of the answer by path (`body.meta.next`).
fn reading_value(reading: &Value, path: &str) -> Option<Value> {
    reading.as_object().and_then(|o| get_path(o, path)).cloned()
}

fn path_text(reading: &Value, path: &str) -> Option<String> {
    match reading_value(reading, path)? {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        _ => None,
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !matches!(s.trim().to_ascii_lowercase().as_str(), "" | "false" | "0" | "no" | "none"),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

// ---------- web.fetch_page ----------

struct FetchPage(NodeSpec);

fn page_url(raw: &str) -> Result<Url, NodeError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(bad("`url` is empty"));
    }
    let with_scheme = if raw.contains("://") { raw.to_string() } else { format!("https://{raw}") };
    Url::parse(&with_scheme).map_err(|_| NodeError::new("invalid_url", "The url is not a valid web address."))
}

fn decode_entities(s: &str) -> String {
    s.replace("&nbsp;", " ").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&#x27;", "'").replace("&amp;", "&")
}

fn first_capture(re: &str, text: &str) -> Option<String> {
    regex::Regex::new(re).ok()?.captures(text).and_then(|c| c.get(1)).map(|m| decode_entities(m.as_str().trim()))
}

fn tidy(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The page with `script`, `style` and similar parts removed. For `extract: html`.
fn clean_html(html: &str) -> String {
    let mut s = regex::Regex::new(r"(?s)<!--.*?-->").unwrap().replace_all(html, " ").into_owned();
    for t in ["script", "style", "noscript", "svg", "iframe"] {
        s = regex::Regex::new(&format!(r"(?is)<{t}\b[^>]*>.*?</{t}\s*>")).unwrap().replace_all(&s, " ").into_owned();
    }
    s
}

fn links_of(base: &Url, html: &str) -> Vec<Value> {
    let re = regex::Regex::new(r#"(?is)<a\b[^>]*?href\s*=\s*["']([^"']+)["'][^>]*>(.*?)</a>"#).unwrap();
    let tags = regex::Regex::new(r"(?s)<[^>]*>").unwrap();
    let mut seen = std::collections::HashSet::new();
    let mut out = vec![];
    for c in re.captures_iter(html) {
        let href = c[1].trim();
        if href.starts_with('#') || href.to_ascii_lowercase().starts_with("javascript:") || href.to_ascii_lowercase().starts_with("mailto:") {
            continue;
        }
        let Ok(mut u) = base.join(href) else { continue };
        if !matches!(u.scheme(), "http" | "https") {
            continue;
        }
        u.set_fragment(None);
        if !seen.insert(u.to_string()) {
            continue;
        }
        let text = tidy(&decode_entities(&tags.replace_all(&c[2], " ")));
        out.push(json!({ "text": text, "href": u.to_string() }));
        if out.len() >= MAX_LINKS {
            break;
        }
    }
    out
}

impl FetchPage {
    /// The robots.txt of the host, read once per run.
    async fn robots_for(&self, env: &NodeEnv, url: &Url, opts: &RequestOptions) -> Result<Arc<Robots>, NodeError> {
        let host = url.host_str().unwrap_or_default();
        let key = format!("{}://{}:{}", url.scheme(), host, url.port_or_known_default().unwrap_or(0));
        if let Some(r) = env.robots.lock().ok().and_then(|m| m.get(&key).cloned()) {
            return Ok(r);
        }
        let mut robots_url = url.clone();
        robots_url.set_path("/robots.txt");
        robots_url.set_query(None);
        robots_url.set_fragment(None);
        env.count_call()?;
        let ropts = RequestOptions { max_bytes: 512 * 1024, ..opts.clone() };
        let robots = match env.fetcher.request(RawRequest::get(robots_url.as_str()), &ropts).await {
            Ok(r) => Robots::from_response(r.status, &String::from_utf8_lossy(&r.body)),
            Err(e @ (FetchError::BadUrl(_) | FetchError::Blocked(_))) => return Err(fetch_err(e)),
            Err(FetchError::TooLarge) => Robots::AllowAll,
            // no answer: the rule is to stay away
            Err(_) => Robots::DisallowAll,
        };
        let robots = Arc::new(robots);
        if let Ok(mut m) = env.robots.lock() {
            m.insert(key, robots.clone());
        }
        Ok(robots)
    }
}

#[async_trait]
impl NodeType for FetchPage {
    fn spec(&self) -> &NodeSpec {
        &self.0
    }

    async fn run(&self, ctx: &NodeCtx<'_>, input: NodeInput) -> Result<NodeOutput, NodeError> {
        let env = ctx.env()?;
        let item = input.items.first().ok_or_else(|| bad("no input item"))?;
        let p = evaluated(ctx, item, input.first_index)?;
        let url = page_url(p.get("url").and_then(Value::as_str).unwrap_or(""))?;
        let max_chars = p.get("max_chars").and_then(Value::as_u64).unwrap_or(8000).clamp(500, 50_000) as usize;
        let extract = p.get("extract").and_then(Value::as_str).unwrap_or("text");
        let include_links = p.get("include_links").and_then(Value::as_bool).unwrap_or(false);
        let respect = p.get("respect_robots").and_then(Value::as_bool).unwrap_or(true) || env.settings.fetch_respect_robots;

        // the block list is checked for the first address and for every redirect
        let settings = env.settings.clone();
        let hop_check: HopCheck = Arc::new(move |u: &Url| match u.host_str() {
            Some(h) if settings.is_blocked_host(h) => Err(FetchError::Blocked(format!("domain_blocked:{h}"))),
            _ => Ok(()),
        });
        let opts = RequestOptions { any_port: false, allow_private: false, max_redirects: 3, max_bytes: MAX_PAGE_BYTES, timeout: Duration::from_secs(10), user_agent: user_agent(env), hop_check: Some(hop_check.clone()) };
        hop_check(&url).map_err(fetch_err)?;

        if respect {
            let robots = self.robots_for(env, &url, &opts).await?;
            let target = match url.query() {
                Some(q) => format!("{}?{q}", url.path()),
                None => url.path().to_string(),
            };
            if !robots.allows(&target) {
                return Err(NodeError::new("web_robots_disallowed", format!("robots.txt of {} does not allow {} for {}.", url.host_str().unwrap_or(""), url.path(), robots::AGENT)));
            }
        }

        env.count_call()?;
        let mut headers = vec![("Accept".to_string(), "text/html,application/xhtml+xml,text/plain".to_string())];
        headers.push(("Accept-Language".into(), "en".into()));
        let resp = env.fetcher.request(RawRequest { method: reqwest::Method::GET, url: url.to_string(), headers, body: None }, &opts).await.map_err(fetch_err)?;
        if resp.status >= 400 {
            return Err(NodeError { code: format!("http_status_{}", resp.status), message: format!("The page answered with status {}.", resp.status), status: Some(resp.status) });
        }
        let essence = resp.content_type.split(';').next().unwrap_or("").trim().to_string();
        if !["text/html", "text/plain", "application/xhtml+xml"].contains(&essence.as_str()) {
            return Err(fetch_err(FetchError::NotText(if essence.is_empty() { "no content type".into() } else { essence })));
        }
        let html = String::from_utf8_lossy(&resp.body).into_owned();
        let is_html = essence != "text/plain";

        let title = if is_html { first_capture(r"(?is)<title[^>]*>(.*?)</title>", &html).map(|t| tidy(&t)) } else { None };
        let description = if is_html {
            first_capture(r#"(?is)<meta\s[^>]*?name\s*=\s*["']description["'][^>]*?content\s*=\s*["']([^"']*)["']"#, &html)
                .or_else(|| first_capture(r#"(?is)<meta\s[^>]*?content\s*=\s*["']([^"']*)["'][^>]*?name\s*=\s*["']description["']"#, &html))
                .map(|t| tidy(&t))
        } else {
            None
        };
        let body_text = match (is_html, extract) {
            (false, _) => html.clone(),
            (true, "html") => clean_html(&html),
            // `markdown` is the same reading as `text`: headings and lists stay as lines, links keep their address
            (true, _) => crate::prompting::sources::html_to_text(&html),
        };
        let total = body_text.chars().count();
        let text: String = body_text.chars().take(max_chars).collect();

        let mut m = Map::new();
        m.insert("url".into(), json!(url.to_string()));
        m.insert("final_url".into(), json!(resp.url.to_string()));
        m.insert("status".into(), json!(resp.status));
        m.insert("title".into(), json!(title));
        m.insert("description".into(), json!(description));
        m.insert("text".into(), json!(text));
        if include_links && is_html {
            m.insert("links".into(), Value::Array(links_of(&resp.url, &html)));
        }
        m.insert("truncated".into(), json!(total > max_chars));
        Ok(NodeOutput::main(vec![item.child(m)]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_urls() {
        assert_eq!(page_url("acme.com").unwrap().as_str(), "https://acme.com/");
        assert_eq!(page_url(" http://acme.com/a?b=1 ").unwrap().as_str(), "http://acme.com/a?b=1");
        assert!(page_url("").is_err());
    }

    #[test]
    fn title_description_and_links() {
        let html = r##"<html><head><title> Acme &amp; Co </title><meta name="description" content="We make rockets"></head>
            <body><a href="/about">About us</a> <a href="#top">top</a> <a href="mailto:a@b.c">mail</a> <a href="https://other.test/x#f">Other</a> <a href="/about">About again</a></body></html>"##;
        assert_eq!(first_capture(r"(?is)<title[^>]*>(.*?)</title>", html).map(|t| tidy(&t)).as_deref(), Some("Acme & Co"));
        let base = Url::parse("https://acme.com/home").unwrap();
        let links = links_of(&base, html);
        assert_eq!(links, vec![json!({ "text": "About us", "href": "https://acme.com/about" }), json!({ "text": "Other", "href": "https://other.test/x" })]);
    }

    #[test]
    fn clean_html_drops_scripts() {
        let out = clean_html("<p>a</p><script>var x=1;</script><style>p{}</style><!-- c --><p>b</p>");
        assert!(out.contains("<p>a</p>") && out.contains("<p>b</p>"));
        assert!(!out.contains("var x") && !out.contains("p{}") && !out.contains("<!--"));
    }
}
