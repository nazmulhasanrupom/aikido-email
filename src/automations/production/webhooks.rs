//! Webhooks (build map 9.4 and 10.3).
//!
//! - `ANY /hooks/{path}`: the production URL. It starts a production run, and works only while the workflow is active.
//! - `ANY /hooks/test/{path}`: the test URL. It works for 120 seconds after `POST .../listen`, accepts one request,
//!   and starts a test run of the draft (also when the workflow is not active).
//!
//! Both are public: no API key. The two never mix: a test request never starts a production run.
use super::plan;
use super::runs::{self, NewRun};
use crate::auth::api_keys::Scope;
use crate::auth::middleware::{KeyScope, require_scope};
use crate::automations::definition::WorkflowDefinition;
use crate::automations::engine::routes::{TestBody, queue_test};
use crate::automations::routes::{caller, not_found};
use crate::error::{ApiError, ApiJson, ApiPath, ApiResult};
use crate::state::AppState;
use crate::users::session::SessionCtx;
use axum::Extension;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{BTreeSet, HashMap};
use std::sync::{LazyLock, Mutex};
use subtle::ConstantTimeEq;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

/// The biggest request body.
const MAX_BODY_BYTES: usize = 5 * 1024 * 1024;
/// How long a test listen lasts.
const LISTEN_SECS: i64 = 120;
/// `response: when_done` waits this long for the run.
const WAIT_SECS: u64 = 30;

// ---------- the listen route (needs a key) ----------

pub fn router(state: AppState) -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(listen)).route_layer(axum::middleware::from_fn_with_state((state, Scope::Automations), require_scope))
}

/// The public routes: no key.
pub fn public_router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().route("/hooks/test/{path}", axum::routing::any(test_hook)).route("/hooks/{path}", axum::routing::any(production_hook))
}

#[derive(Deserialize, ToSchema)]
pub struct ListenBody {
    /// The id of a `trigger.webhook` node of the draft.
    node_id: String,
}

#[derive(Serialize, ToSchema)]
pub struct ListenReply {
    /// Send one request to this URL. It starts a test run of the draft.
    pub test_url: String,
    pub path: String,
    /// The URL stops working at this time.
    pub until: DateTime<Utc>,
}

/// Listen for one test request on a webhook node.
#[utoipa::path(post, path = "/v1/automations/workflows/{id}/listen", tag = "automations", params(("id" = Uuid, Path)), request_body = ListenBody,
    responses((status = 200, body = ListenReply), (status = 404, description = "workflow_not_found"), (status = 422, description = "invalid_trigger_node, invalid_trigger")),
    security(("api_key" = ["automations"]), ("session" = [])),
    description = "Scope: `automations`. For 120 seconds, `ANY /hooks/test/{path}` accepts one request and starts a test run of the draft with it, \
        even when the workflow is not active. Then the URL answers `404 webhook_not_listening`. The path is the `path` of the node (or a new random one). \
        The method and the auth rule of the node apply to the test request too. A production URL never starts a test run, and this URL never starts a production run.")]
async fn listen(
    State(st): State<AppState>,
    session: Option<Extension<SessionCtx>>,
    key: Option<Extension<KeyScope>>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(b): ApiJson<ListenBody>,
) -> ApiResult<ApiJson<ListenReply>> {
    let c = caller(&session, &key);
    let draft: Option<Value> = sqlx::query_scalar("SELECT draft FROM automation_workflows WHERE id = $1").bind(id).fetch_optional(&st.pool).await?;
    let draft = draft.ok_or_else(not_found)?;
    let def: WorkflowDefinition = serde_json::from_value(draft).map_err(|e| ApiError::unprocessable("invalid_format", e.to_string()))?;
    let node = def.node(&b.node_id).filter(|n| n.type_id == "trigger.webhook").ok_or_else(|| ApiError::unprocessable("invalid_trigger_node", format!("`{}` is not a webhook trigger of this workflow.", b.node_id)))?;
    let tz = {
        let mut conn = st.pool.acquire().await?;
        crate::sending::queue::load_defaults(&mut conn).await?.timezone
    };
    let planned = plan::plan_node(&def, node, &tz).map_err(|e| ApiError::unprocessable("invalid_trigger", e.message))?.ok_or_else(ApiError::internal)?;
    let path = planned.webhook_path.ok_or_else(ApiError::internal)?;
    let until = st.now() + Duration::seconds(LISTEN_SECS);
    let mut tx = st.pool.begin().await?;
    sqlx::query("DELETE FROM automation_test_listens WHERE (workflow_id = $1 AND node_id = $2) OR webhook_path = $3").bind(id).bind(&node.id).bind(&path).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO automation_test_listens (webhook_path, workflow_id, node_id, user_id, until) VALUES ($1, $2, $3, $4, $5)")
        .bind(&path)
        .bind(id)
        .bind(&node.id)
        .bind(c.user_id)
        .bind(until)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(ApiJson(ListenReply { test_url: format!("{}/hooks/test/{path}", st.site_url().unwrap_or_default()), path, until }))
}

// ---------- the rate limit ----------

/// One minute windows per path, in memory of this server copy.
static WINDOWS: LazyLock<Mutex<HashMap<String, (i64, i64)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Count a request. `Err(seconds)`: too many, try again in that long.
pub fn rate_check(path: &str, per_minute: i64, now: DateTime<Utc>) -> Result<(), i64> {
    let minute = now.timestamp().div_euclid(60);
    let mut w = WINDOWS.lock().unwrap_or_else(|e| e.into_inner());
    if w.len() > 10_000 {
        w.retain(|_, (m, _)| *m == minute);
    }
    let e = w.entry(path.to_string()).or_insert((minute, 0));
    if e.0 != minute {
        *e = (minute, 0);
    }
    if e.1 >= per_minute {
        return Err(60 - now.timestamp().rem_euclid(60));
    }
    e.1 += 1;
    Ok(())
}

// ---------- reading the request ----------

fn fail(status: StatusCode, code: &str, message: &str) -> Response {
    (status, axum::Json(json!({ "error": { "code": code, "message": message } }))).into_response()
}

struct Hook {
    method: String,
    query: Map<String, Value>,
    headers: HeaderMap,
    body: Value,
}

/// Pairs of a query string or a form body. A key that comes twice keeps the last value.
fn pairs(raw: &str) -> Option<Map<String, Value>> {
    let uri: axum::http::Uri = format!("/?{raw}").parse().ok()?;
    let list = axum::extract::Query::<Vec<(String, String)>>::try_from_uri(&uri).ok()?.0;
    Some(list.into_iter().map(|(k, v)| (k, Value::String(v))).collect())
}

fn parse_body(content_type: &str, bytes: &[u8]) -> Value {
    if bytes.is_empty() {
        return Value::Null;
    }
    let ct = content_type.to_ascii_lowercase();
    if ct.contains("json") {
        if let Ok(v) = serde_json::from_slice(bytes) {
            return v;
        }
    } else if ct.contains("x-www-form-urlencoded") {
        if let Some(m) = std::str::from_utf8(bytes).ok().and_then(pairs) {
            return Value::Object(m);
        }
    }
    Value::String(String::from_utf8_lossy(bytes).into_owned())
}

async fn read_request(req: Request) -> Result<Hook, Response> {
    let (parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, MAX_BODY_BYTES).await.map_err(|_| fail(StatusCode::PAYLOAD_TOO_LARGE, "body_too_large", "The body is bigger than 5 MB."))?;
    let content_type = parts.headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or_default();
    let body = parse_body(content_type, &bytes);
    let query = parts.uri.query().and_then(pairs).unwrap_or_default();
    Ok(Hook { method: parts.method.as_str().to_string(), query, headers: parts.headers, body })
}

/// The trigger item of a webhook (build map 7.1). The header that holds the secret, `Authorization` and cookies are left out.
fn trigger_item(h: &Hook, path: &str, config: &Value, now: DateTime<Utc>) -> Value {
    let hidden = config.get("header_name").and_then(Value::as_str).unwrap_or_default();
    let headers: Map<String, Value> = h
        .headers
        .iter()
        .filter(|(k, _)| !matches!(k.as_str(), "authorization" | "proxy-authorization" | "cookie") && k.as_str() != hidden)
        .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().to_string(), Value::String(v.to_string()))))
        .collect();
    json!({ "method": h.method, "path": path, "query": h.query, "headers": headers, "body": h.body, "received_at": now })
}

/// The method rule of the trigger.
fn method_ok(config: &Value, method: &str) -> bool {
    match config.get("method").and_then(Value::as_str).unwrap_or("POST") {
        "ANY" => true,
        m => m.eq_ignore_ascii_case(method),
    }
}

/// Check the secret header. `Err`: the answer to send.
async fn check_auth(st: &AppState, config: &Value, headers: &HeaderMap) -> Result<(), Response> {
    if config.get("auth").and_then(Value::as_str) != Some("header") {
        return Ok(());
    }
    let denied = || fail(StatusCode::UNAUTHORIZED, "webhook_unauthorized", "The secret header is missing or wrong.");
    let (Some(name), Some(secret)) = (config.get("header_name").and_then(Value::as_str), config.get("secret").and_then(Value::as_str)) else { return Err(denied()) };
    let secrets = crate::automations::secrets::load(st, &BTreeSet::from([secret.to_string()])).await.map_err(|_| fail(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", "Something went wrong."))?;
    let Some(want) = secrets.get(secret) else {
        tracing::warn!(secret, "automations: a webhook uses a secret that does not exist");
        return Err(denied());
    };
    let got = headers.get(name).map(|v| v.as_bytes()).unwrap_or_default();
    if got.is_empty() || !bool::from(got.ct_eq(want.as_bytes())) {
        return Err(denied());
    }
    Ok(())
}

// ---------- answering ----------

/// Wait for a run to end and answer with the output of its last node (`response: when_done`).
/// `202` with the run id if it is not done in time.
async fn answer_when_done(st: &AppState, run_id: Uuid) -> Response {
    for _ in 0..(WAIT_SECS * 4) {
        let row: Result<Option<(String, Option<Value>)>, _> = sqlx::query_as("SELECT status, error FROM automation_runs WHERE id = $1").bind(run_id).fetch_optional(&st.pool).await;
        match row {
            Ok(Some((status, error))) => match status.as_str() {
                "success" => return last_output(st, run_id).await,
                "error" => {
                    let e = error.unwrap_or(Value::Null);
                    let code = e.get("code").and_then(Value::as_str).unwrap_or("run_failed");
                    let msg = e.get("message").and_then(Value::as_str).unwrap_or("The run failed.");
                    return (StatusCode::INTERNAL_SERVER_ERROR, axum::Json(json!({ "error": { "code": code, "message": msg }, "run_id": run_id }))).into_response();
                }
                "cancelled" => return (StatusCode::CONFLICT, axum::Json(json!({ "error": { "code": "run_cancelled", "message": "The run was cancelled." }, "run_id": run_id }))).into_response(),
                _ => {}
            },
            Ok(None) => break,
            Err(_) => break,
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    accepted(run_id)
}

fn accepted(run_id: Uuid) -> Response {
    (StatusCode::ACCEPTED, axum::Json(json!({ "run_id": run_id }))).into_response()
}

/// The items of the last node that ran: one item as an object, several as a list.
async fn last_output(st: &AppState, run_id: Uuid) -> Response {
    let out: Option<Option<Value>> =
        sqlx::query_scalar("SELECT output FROM automation_run_nodes WHERE run_id = $1 AND status IN ('success', 'dry') ORDER BY seq DESC LIMIT 1").bind(run_id).fetch_optional(&st.pool).await.ok().flatten();
    let items = out.flatten().and_then(|o| {
        let ports = o.as_object()?;
        let list = ports.get("main").or_else(|| ports.values().next())?.as_array()?.clone();
        Some(list)
    });
    match items {
        Some(mut l) if l.len() == 1 => axum::Json(l.remove(0)).into_response(),
        Some(l) => axum::Json(Value::Array(l)).into_response(),
        None => axum::Json(json!({ "run_id": run_id, "status": "success" })).into_response(),
    }
}

// ---------- the production URL ----------

#[derive(sqlx::FromRow)]
struct Hit {
    id: Uuid,
    workflow_id: Uuid,
    version_id: Uuid,
    node_id: String,
    config: Value,
    run_as_user_id: Option<Uuid>,
    max_per_hour: Option<i32>,
}

fn not_found_response() -> Response {
    fail(StatusCode::NOT_FOUND, "webhook_not_found", "There is no active webhook at this address.")
}

async fn production_hook(State(st): State<AppState>, Path(path): Path<String>, req: Request) -> Response {
    match handle_production(&st, &path, req).await {
        Ok(r) | Err(r) => r,
    }
}

async fn handle_production(st: &AppState, path: &str, req: Request) -> Result<Response, Response> {
    let db_fail = |e: sqlx::Error| {
        tracing::error!(error = %e, "automations: webhook failed");
        fail(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", "Something went wrong.")
    };
    // an unknown address and an inactive workflow look the same
    let hit: Option<Hit> = sqlx::query_as(
        "SELECT t.id, t.workflow_id, t.version_id, t.node_id, t.config, w.run_as_user_id, (v.definition -> 'settings' ->> 'max_runs_per_hour')::int AS max_per_hour \
           FROM automation_triggers t JOIN automation_workflows w ON w.id = t.workflow_id JOIN automation_versions v ON v.id = t.version_id \
          WHERE t.webhook_path = $1 AND t.kind = 'webhook' AND w.active AND w.archived_at IS NULL",
    )
    .bind(path)
    .fetch_optional(&st.pool)
    .await
    .map_err(db_fail)?;
    let Some(hit) = hit else { return Err(not_found_response()) };

    check_auth(st, &hit.config, req.headers()).await?;
    if !method_ok(&hit.config, req.method().as_str()) {
        return Err(fail(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed", "This webhook does not take this method."));
    }
    let per_min = crate::automations::settings::load(&st.pool).await.map_err(db_fail)?.webhook_rate_per_min;
    let now = st.now();
    if let Err(wait) = rate_check(path, per_min, now) {
        let mut r = fail(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "Too many requests to this webhook.");
        r.headers_mut().insert(header::RETRY_AFTER, wait.into());
        return Err(r);
    }
    let hook = read_request(req).await?;

    let mut tx = st.pool.begin().await.map_err(db_fail)?;
    let lim = runs::limits(&mut tx, hit.workflow_id, runs::max_per_hour(hit.max_per_hour), now).await.map_err(db_fail)?;
    if !lim.open() {
        if lim.remaining <= 0 {
            let _ = sqlx::query("UPDATE automation_triggers SET rate_limited_at = COALESCE(rate_limited_at, $2) WHERE id = $1").bind(hit.id).bind(now).execute(&mut *tx).await;
            let _ = tx.commit().await;
        }
        let mut r = fail(StatusCode::TOO_MANY_REQUESTS, "rate_limited", "This workflow is busy. Try again later.");
        r.headers_mut().insert(header::RETRY_AFTER, 60.into());
        return Err(r);
    }
    let item = trigger_item(&hook, path, &hit.config, now);
    let run = NewRun { workflow_id: hit.workflow_id, version_id: hit.version_id, trigger_node_id: &hit.node_id, trigger_item: item, run_as_user_id: hit.run_as_user_id, started_by: None, idempotency_key: None };
    let run_id = runs::insert_run(&mut tx, now, run).await.map_err(db_fail)?.ok_or_else(|| fail(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", "Something went wrong."))?;
    sqlx::query("UPDATE automation_triggers SET last_fired_at = $2, last_error = NULL, rate_limited_at = NULL WHERE id = $1").bind(hit.id).bind(now).execute(&mut *tx).await.map_err(db_fail)?;
    tx.commit().await.map_err(db_fail)?;

    if hit.config.get("response").and_then(Value::as_str) == Some("when_done") {
        return Ok(answer_when_done(st, run_id).await);
    }
    Ok(accepted(run_id))
}

// ---------- the test URL ----------

async fn test_hook(State(st): State<AppState>, Path(path): Path<String>, req: Request) -> Response {
    match handle_test(&st, &path, req).await {
        Ok(r) | Err(r) => r,
    }
}

async fn handle_test(st: &AppState, path: &str, req: Request) -> Result<Response, Response> {
    let db_fail = |e: sqlx::Error| {
        tracing::error!(error = %e, "automations: test webhook failed");
        fail(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", "Something went wrong.")
    };
    let not_listening = || fail(StatusCode::NOT_FOUND, "webhook_not_listening", "Nobody listens at this address. Press Listen for test event in the editor first.");
    let now = st.now();
    let listen: Option<(Uuid, String, Option<Uuid>)> =
        sqlx::query_as("SELECT workflow_id, node_id, user_id FROM automation_test_listens WHERE webhook_path = $1 AND until > $2").bind(path).bind(now).fetch_optional(&st.pool).await.map_err(db_fail)?;
    let Some((workflow_id, node_id, user_id)) = listen else { return Err(not_listening()) };

    // the rules of the node, from the draft
    let draft: Option<Value> = sqlx::query_scalar("SELECT draft FROM automation_workflows WHERE id = $1").bind(workflow_id).fetch_optional(&st.pool).await.map_err(db_fail)?;
    let def: WorkflowDefinition = draft
        .and_then(|d| serde_json::from_value(d).ok())
        .ok_or_else(|| fail(StatusCode::UNPROCESSABLE_ENTITY, "invalid_format", "The draft cannot be read."))?;
    let tz = {
        let mut conn = st.pool.acquire().await.map_err(db_fail)?;
        crate::sending::queue::load_defaults(&mut conn).await.map_err(db_fail)?.timezone
    };
    let planned = def
        .node(&node_id)
        .ok_or_else(not_listening)
        .and_then(|n| plan::plan_node(&def, n, &tz).map_err(|e| fail(StatusCode::UNPROCESSABLE_ENTITY, "invalid_trigger", &e.message)))?
        .ok_or_else(not_listening)?;
    check_auth(st, &planned.config, req.headers()).await?;
    if !method_ok(&planned.config, req.method().as_str()) {
        return Err(fail(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed", "This webhook does not take this method."));
    }
    let hook = read_request(req).await?;

    // one request only
    let taken: Option<i32> = sqlx::query_scalar("DELETE FROM automation_test_listens WHERE webhook_path = $1 AND until > $2 RETURNING 1").bind(path).bind(now).fetch_optional(&st.pool).await.map_err(db_fail)?;
    if taken.is_none() {
        return Err(not_listening());
    }
    let item = trigger_item(&hook, path, &planned.config, now);
    let body = TestBody { trigger_node_id: Some(node_id), trigger_item: Some(item), real_writes: Some(false), ..Default::default() };
    let queued = queue_test(st, workflow_id, body, user_id, false).await.map_err(|e| e.into_response())?;
    let run_id = match queued {
        Ok(id) => id,
        Err(refused) => return Err(refused),
    };
    if planned.config.get("response").and_then(Value::as_str) == Some("when_done") {
        return Ok(answer_when_done(st, run_id).await);
    }
    Ok(accepted(run_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn the_rate_limit_counts_per_path_and_per_minute() {
        let t = |s| Utc.with_ymd_and_hms(2026, 10, 26, 10, 0, s).unwrap();
        let path = format!("rate-{}", Uuid::now_v7());
        for _ in 0..3 {
            assert!(rate_check(&path, 3, t(10)).is_ok());
        }
        assert_eq!(rate_check(&path, 3, t(10)), Err(50), "the next minute starts in 50 seconds");
        assert!(rate_check(&format!("other-{path}"), 3, t(10)).is_ok(), "another path has its own count");
        // a new minute, a new count
        let later = Utc.with_ymd_and_hms(2026, 10, 26, 10, 1, 0).unwrap();
        assert!(rate_check(&path, 3, later).is_ok());
    }

    #[test]
    fn bodies_are_read_by_content_type() {
        assert_eq!(parse_body("application/json", br#"{"a":1}"#), json!({ "a": 1 }));
        assert_eq!(parse_body("application/json; charset=utf-8", b"[1,2]"), json!([1, 2]));
        assert_eq!(parse_body("application/x-www-form-urlencoded", b"name=Ana+Lee&email=ana%40acme.com"), json!({ "name": "Ana Lee", "email": "ana@acme.com" }));
        assert_eq!(parse_body("text/plain", b"hello"), json!("hello"));
        assert_eq!(parse_body("application/json", b"not json"), json!("not json"), "bad JSON is kept as text");
        assert_eq!(parse_body("application/json", b""), Value::Null);
    }

    #[test]
    fn the_method_rule() {
        assert!(method_ok(&json!({}), "POST"));
        assert!(!method_ok(&json!({}), "GET"));
        assert!(method_ok(&json!({ "method": "ANY" }), "DELETE"));
        assert!(method_ok(&json!({ "method": "GET" }), "get"));
    }

    #[test]
    fn the_trigger_item_hides_the_secret_header_and_cookies() {
        let mut headers = HeaderMap::new();
        headers.insert("x-api-secret", "s3cret".parse().unwrap());
        headers.insert("authorization", "Bearer x".parse().unwrap());
        headers.insert("cookie", "a=b".parse().unwrap());
        headers.insert("x-source", "form".parse().unwrap());
        let h = Hook { method: "POST".into(), query: Map::from_iter([("a".to_string(), json!("1"))]), headers, body: json!({ "email": "ana@acme.com" }) };
        let now = Utc::now();
        let item = trigger_item(&h, "my-form", &json!({ "header_name": "x-api-secret" }), now);
        assert_eq!(item["headers"], json!({ "x-source": "form" }));
        assert_eq!((item["method"].as_str(), item["path"].as_str()), (Some("POST"), Some("my-form")));
        assert_eq!(item["query"], json!({ "a": "1" }));
        assert_eq!(item["body"]["email"], "ana@acme.com");
        assert!(!item.to_string().contains("s3cret"));
    }

    #[test]
    fn query_strings_become_an_object() {
        let m = pairs("a=1&b=two%20words&a=3").unwrap();
        assert_eq!(m["a"], "3");
        assert_eq!(m["b"], "two words");
        assert!(pairs("").unwrap().is_empty());
    }
}
