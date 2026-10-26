//! Internal calls (build map 8.8): an AIKIDO node calls the real route of the app, in the same process.
//!
//! - The request goes through the app router with `tower::ServiceExt::oneshot`: same validation, same errors,
//!   same events as a call from outside. No network, no key.
//! - The user comes as an `InternalPrincipal` in the request extensions (never from a header).
//! - While the call runs, `actor` tells the lead event writers which workflow made the change.
//! - `OperationIndex` lists every operation of the OpenAPI spec for the generic `aikido.api` node, with the
//!   deny list (keys, login, users, developer tools, environment, automations, OAuth, lead erase).
use super::actor::{self, Actor};
use super::registry::NodeError;
use crate::auth::middleware::InternalPrincipal;
use crate::state::AppState;
use crate::users::model::Role;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, header};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};
use tower::ServiceExt;
use tracing::Instrument;
use uuid::Uuid;

/// The operations of the API, set once when the router is first made. The node `aikido.api` reads it to know
/// whether an operation changes data (a node spec has no access to the app state).
pub static KNOWN_OPERATIONS: OnceLock<Arc<OperationIndex>> = OnceLock::new();

/// The biggest answer a node reads (10 MB).
const MAX_ANSWER_BYTES: usize = 10 * 1024 * 1024;

/// Who makes the call.
#[derive(Debug, Clone)]
pub struct Who {
    pub user_id: Uuid,
    pub role: Role,
    pub run_id: String,
    pub workflow_id: String,
}

/// One call to a route.
#[derive(Debug, Clone, PartialEq)]
pub struct CallRequest {
    pub method: String,
    /// The path with the path parameters filled in: `/v1/leads/0192...`.
    pub path: String,
    pub query: Vec<(String, String)>,
    pub body: Option<Value>,
}

impl CallRequest {
    pub fn new(method: &str, path: impl Into<String>) -> Self {
        CallRequest { method: method.to_string(), path: path.into(), query: vec![], body: None }
    }
    pub fn query(mut self, key: &str, value: impl ToString) -> Self {
        self.query.push((key.to_string(), value.to_string()));
        self
    }
    pub fn body(mut self, body: Value) -> Self {
        self.body = Some(body);
        self
    }
    /// What a dry run shows.
    pub fn would_call(&self) -> Value {
        let mut v = json!({ "method": self.method, "path": self.path });
        if !self.query.is_empty() {
            v["query"] = Value::Object(self.query.iter().map(|(k, x)| (k.clone(), json!(x))).collect());
        }
        if let Some(b) = &self.body {
            v["body"] = b.clone();
        }
        v
    }
}

#[derive(Debug, Clone)]
pub struct CallResponse {
    pub status: u16,
    pub body: Value,
}

impl CallResponse {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Sends a request to a route. The real one uses the app router; tests use a fake.
#[async_trait]
pub trait Internal: Send + Sync {
    async fn call(&self, who: &Who, req: CallRequest) -> Result<CallResponse, NodeError>;
}

// ---------- the real caller ----------

/// The operation index, made on first use (it needs the routes, and they need the `AppState`, so it can not be
/// made when the state is made). Shared by every clone of the state. The router itself is not kept here: every
/// route holds a clone of the state, so keeping it in the state would make a loop and the state (and its pool)
/// would never be freed.
#[derive(Default)]
pub struct InternalRouter {
    cell: OnceLock<Arc<OperationIndex>>,
}

impl InternalRouter {
    pub fn operations(&self, st: &AppState) -> Arc<OperationIndex> {
        self.cell
            .get_or_init(|| {
                let (_, api) = crate::docs::api_router(st.clone());
                let ops = Arc::new(OperationIndex::from_openapi(&api));
                let _ = KNOWN_OPERATIONS.set(ops.clone());
                ops
            })
            .clone()
    }
}

/// Calls the app router in-process. Made once per run; the router is built on the first call and freed with it.
pub struct RouterCaller {
    state: AppState,
    router: OnceLock<axum::Router>,
}

impl RouterCaller {
    pub fn new(state: AppState) -> Self {
        Self { state, router: OnceLock::new() }
    }

    fn router(&self) -> axum::Router {
        self.router
            .get_or_init(|| {
                let (router, _) = crate::docs::api_router(self.state.clone());
                // the same request log line as a call from outside (it runs inside the `automation_call` span)
                router.with_state(self.state.clone()).layer(axum::middleware::from_fn(crate::devtools::log_requests))
            })
            .clone()
    }
}

#[async_trait]
impl Internal for RouterCaller {
    async fn call(&self, who: &Who, req: CallRequest) -> Result<CallResponse, NodeError> {
        self.send(who, req).await.map_err(|e| NodeError::new("internal_call_failed", e))
    }
}

impl RouterCaller {
    async fn send(&self, who: &Who, req: CallRequest) -> Result<CallResponse, String> {
        let router = self.router();
        let method = Method::from_bytes(req.method.as_bytes()).map_err(|_| format!("bad method {}", req.method))?;
        let mut uri = req.path.clone();
        if !req.query.is_empty() {
            let q: Vec<String> = req.query.iter().map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v))).collect();
            uri = format!("{uri}?{}", q.join("&"));
        }
        let mut b = Request::builder().method(method).uri(&uri);
        let body = match &req.body {
            Some(v) => {
                b = b.header(header::CONTENT_TYPE, "application/json");
                Body::from(serde_json::to_vec(v).map_err(|e| e.to_string())?)
            }
            None => Body::empty(),
        };
        let mut request = b.body(body).map_err(|e| e.to_string())?;
        request.extensions_mut().insert(InternalPrincipal { user_id: who.user_id, role: who.role });
        let span = tracing::info_span!("automation_call", automation_run = %who.run_id);
        let actor = Actor { workflow_id: who.workflow_id.clone(), run_id: who.run_id.clone() };
        let resp = actor::scope(actor, router.oneshot(request)).instrument(span).await.map_err(|e| format!("{e}"))?;
        let status = resp.status().as_u16();
        let bytes = axum::body::to_bytes(resp.into_body(), MAX_ANSWER_BYTES).await.map_err(|e| format!("the answer is too large or broken: {e}"))?;
        let body = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned())) };
        Ok(CallResponse { status, body })
    }
}

pub fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------- the operations of the API ----------

#[derive(Debug, Clone, PartialEq)]
pub struct Operation {
    /// `METHOD /path`. It does not change when a route is added.
    pub id: String,
    /// The OpenAPI `operationId` (the function name), when no other route has the same one.
    pub operation_id: Option<String>,
    pub method: String,
    /// `/v1/leads/{id}`
    pub path: String,
    pub tag: String,
    pub summary: String,
    pub path_params: Vec<String>,
    pub query_params: Vec<String>,
    pub has_body: bool,
    pub denied: Option<&'static str>,
}

#[derive(Debug, Default)]
pub struct OperationIndex {
    ops: BTreeMap<String, Operation>,
    /// `operationId` to the key of `ops`, for the ids that are unique.
    aliases: BTreeMap<String, String>,
}

/// Why a call is not offered to a workflow (build map 7.2). `None`: allowed.
pub fn deny_reason(method: &str, path: &str, allow_lead_erase: bool) -> Option<&'static str> {
    let path = path.trim_end_matches('/');
    let m = method.to_ascii_uppercase();
    const PREFIXES: &[(&str, &str)] = &[
        ("/v1/keys", "API keys can not be changed by a workflow."),
        ("/v1/auth", "Login routes can not be called by a workflow."),
        ("/v1/users", "User routes can not be called by a workflow."),
        ("/v1/dev", "Developer tools can not be called by a workflow."),
        ("/v1/settings/environment", "Environment settings can not be changed by a workflow."),
        ("/v1/automations", "A workflow can not change workflows."),
        ("/v1/integrations/apps", "Apps can not be installed or removed by a workflow."),
    ];
    for (p, why) in PREFIXES {
        if path == *p || path.starts_with(&format!("{p}/")) {
            return Some(why);
        }
    }
    if path.starts_with("/v1/accounts/oauth") || path.contains("/oauth/") {
        return Some("OAuth routes can not be called by a workflow.");
    }
    if !allow_lead_erase && m == "DELETE" && path.starts_with("/v1/leads/") && path.matches('/').count() == 3 {
        return Some("Erasing a lead is off for workflows. The owner can turn it on in Automations > Settings (allow_lead_erase).");
    }
    None
}

impl OperationIndex {
    pub fn from_openapi(api: &utoipa::openapi::OpenApi) -> Self {
        let mut ops = BTreeMap::new();
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        let mut list: Vec<Operation> = vec![];
        for (path, item) in &api.paths.paths {
            for (method, op) in [("GET", &item.get), ("POST", &item.post), ("PUT", &item.put), ("PATCH", &item.patch), ("DELETE", &item.delete)] {
                let Some(op) = op else { continue };
                let mut path_params = vec![];
                let mut query_params = vec![];
                for p in op.parameters.iter().flatten() {
                    let utoipa::openapi::RefOr::T(p) = p else { continue };
                    match p.parameter_in {
                        utoipa::openapi::path::ParameterIn::Path => path_params.push(p.name.clone()),
                        utoipa::openapi::path::ParameterIn::Query => query_params.push(p.name.clone()),
                        _ => {}
                    }
                }
                // parameters that the path template names but the spec does not list
                for name in template_params(path) {
                    if !path_params.contains(&name) {
                        path_params.push(name);
                    }
                }
                if let Some(id) = &op.operation_id {
                    *seen.entry(id.clone()).or_default() += 1;
                }
                list.push(Operation {
                    id: format!("{method} {path}"),
                    operation_id: op.operation_id.clone(),
                    method: method.to_string(),
                    path: path.clone(),
                    tag: op.tags.iter().flatten().next().cloned().unwrap_or_default(),
                    summary: op.summary.clone().or_else(|| op.description.as_ref().map(|d| d.lines().next().unwrap_or_default().to_string())).unwrap_or_default(),
                    path_params,
                    query_params,
                    has_body: op.request_body.is_some(),
                    denied: None,
                });
            }
        }
        let mut aliases = BTreeMap::new();
        for mut o in list {
            // an operation id that two routes share is not an alias
            if o.operation_id.as_ref().is_some_and(|id| seen.get(id).copied().unwrap_or(0) > 1) {
                o.operation_id = None;
            }
            if let Some(id) = &o.operation_id {
                aliases.insert(id.clone(), o.id.clone());
            }
            o.denied = deny_reason(&o.method, &o.path, true);
            ops.insert(o.id.clone(), o);
        }
        OperationIndex { ops, aliases }
    }

    /// Find an operation by its id or by `METHOD /path`.
    pub fn find(&self, operation: &str) -> Option<&Operation> {
        let op = operation.trim();
        self.ops.get(op).or_else(|| self.aliases.get(op).and_then(|k| self.ops.get(k))).or_else(|| {
            let (m, p) = op.split_once(' ')?;
            self.ops.get(&format!("{} {}", m.to_ascii_uppercase(), p.trim()))
        })
    }

    pub fn all(&self) -> impl Iterator<Item = &Operation> {
        self.ops.values()
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

fn template_params(path: &str) -> Vec<String> {
    path.split('/').filter_map(|s| s.strip_prefix('{').and_then(|s| s.strip_suffix('}')).map(str::to_string)).collect()
}

/// Fill `{name}` in a path template from an object of values. A missing or unsafe value is an error.
pub fn fill_path(template: &str, values: &serde_json::Map<String, Value>) -> Result<String, String> {
    let mut out = vec![];
    for seg in template.split('/') {
        match seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            Some(name) => {
                let v = values.get(name).ok_or_else(|| format!("the path parameter `{name}` is missing"))?;
                let text = match v {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    _ => return Err(format!("the path parameter `{name}` must be text or a number")),
                };
                if text.is_empty() || text.contains(['/', '?', '#', '\\']) || text == ".." || text == "." {
                    return Err(format!("the path parameter `{name}` has a value that is not allowed"));
                }
                out.push(urlencode(&text));
            }
            None => out.push(seg.to_string()),
        }
    }
    Ok(out.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deny_list() {
        for (m, p) in [
            ("GET", "/v1/keys"),
            ("POST", "/v1/keys/abc/rotate"),
            ("POST", "/v1/auth/login"),
            ("GET", "/v1/users/me"),
            ("POST", "/v1/dev/sql"),
            ("PATCH", "/v1/settings/environment"),
            ("GET", "/v1/automations/workflows"),
            ("POST", "/v1/automations/workflows/x/run"),
            ("POST", "/v1/accounts/oauth/google/start"),
            ("GET", "/v1/accounts/oauth/google/callback"),
            ("DELETE", "/v1/leads/0192"),
            ("POST", "/v1/integrations/apps/reoon/install"),
        ] {
            assert!(deny_reason(m, p, false).is_some(), "{m} {p} must be denied");
        }
        for (m, p) in [("GET", "/v1/leads"), ("GET", "/v1/leads/0192"), ("POST", "/v1/leads/actions"), ("DELETE", "/v1/sending/suppressions"), ("DELETE", "/v1/campaigns/x/leads"), ("GET", "/v1/keysmith")] {
            assert!(deny_reason(m, p, false).is_none(), "{m} {p} must be allowed");
        }
        // the owner can lift the erase rule
        assert!(deny_reason("DELETE", "/v1/leads/0192", true).is_none());
        assert!(deny_reason("GET", "/v1/keys", true).is_some());
    }

    #[test]
    fn path_params_are_filled_and_checked() {
        let v = json!({ "id": "0192-a", "n": 3 });
        let m = v.as_object().unwrap();
        assert_eq!(fill_path("/v1/leads/{id}/events", m).unwrap(), "/v1/leads/0192-a/events");
        assert_eq!(fill_path("/v1/x/{n}", m).unwrap(), "/v1/x/3");
        assert!(fill_path("/v1/x/{missing}", m).unwrap_err().contains("missing"));
        for bad in ["a/b", "..", "a?b=1", ""] {
            let v = json!({ "id": bad });
            assert!(fill_path("/v1/leads/{id}", v.as_object().unwrap()).is_err(), "{bad:?}");
        }
        assert_eq!(urlencode("a b&c"), "a%20b%26c");
    }

    #[tokio::test]
    async fn the_index_knows_the_operations_and_the_denied_ones() {
        let st = crate::automations::testkit::lazy_state();
        let ops = st.internal_router.operations(&st);
        assert!(ops.len() > 100, "{} operations", ops.len());
        // lookup by `METHOD /path` always works
        let get_lead = ops.find("GET /v1/leads/{id}").expect("get lead");
        assert_eq!(get_lead.path_params, vec!["id"]);
        assert!(get_lead.denied.is_none());
        assert_eq!(get_lead.id, "GET /v1/leads/{id}");
        // an operation id that is unique works too
        let alias = ops.all().find_map(|o| o.operation_id.clone()).expect("some operation has a unique id");
        assert!(ops.find(&alias).is_some());
        assert!(ops.find("get /v1/leads/{id}").is_some(), "the method may be lower case");
        assert!(ops.find("POST /v1/keys").unwrap().denied.is_some());
        assert!(ops.find("GET /v1/automations/workflows").unwrap().denied.is_some());
        assert!(ops.find("nothing_here").is_none());
        // every id is unique (the index is keyed by it) and every route is in the index
        let n_paths: usize = ops.all().count();
        assert_eq!(n_paths, ops.len());
    }
}
