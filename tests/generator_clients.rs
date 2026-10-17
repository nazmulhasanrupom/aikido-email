//! The LLM clients against a mock HTTP server, the provider store, and the settings of the generator.
//! No real provider is called.
mod common;
use aikido_email::auth::api_keys::Scope;
use aikido_email::generator::llm::anthropic::AnthropicClient;
use aikido_email::generator::llm::openai::OpenAiClient;
use aikido_email::generator::llm::{LlmClient, LlmError, LlmRequest, ProviderConfig, ProviderKind, RealLlmFactory, RetryPolicy};
use aikido_email::generator::providers;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use axum::Router;
use common::*;
use secrecy::SecretString;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

// ---------- a mock provider ----------

struct Scripted {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: String,
    delay_ms: u64,
}

fn ok(body: Value) -> Scripted {
    Scripted { status: 200, headers: vec![], body: body.to_string(), delay_ms: 0 }
}

fn status(code: u16) -> Scripted {
    Scripted { status: code, headers: vec![], body: json!({ "error": { "message": format!("error {code}") } }).to_string(), delay_ms: 0 }
}

#[derive(Clone, Debug)]
struct Seen {
    path: String,
    headers: HashMap<String, String>,
    body: Value,
}

#[derive(Clone, Default)]
struct Mock {
    script: Arc<Mutex<VecDeque<Scripted>>>,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Mock {
    fn push(&self, s: Scripted) {
        self.script.lock().unwrap().push_back(s);
    }
    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

async fn handler(State(m): State<Mock>, uri: Uri, headers: HeaderMap, body: String) -> impl IntoResponse {
    m.seen.lock().unwrap().push(Seen {
        path: uri.path().to_string(),
        headers: headers.iter().map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or_default().to_string())).collect(),
        body: serde_json::from_str(&body).unwrap_or(Value::Null),
    });
    let next = m.script.lock().unwrap().pop_front();
    let Some(s) = next else { return (StatusCode::INTERNAL_SERVER_ERROR, HeaderMap::new(), "script is empty".to_string()) };
    if s.delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(s.delay_ms)).await;
    }
    let mut h = HeaderMap::new();
    for (k, v) in s.headers {
        h.insert(k, v.parse().unwrap());
    }
    (StatusCode::from_u16(s.status).unwrap(), h, s.body)
}

async fn start() -> (String, Mock) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mock = Mock::default();
    let app = Router::new().fallback(handler).with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, mock)
}

fn cfg(kind: ProviderKind, base: &str, key: Option<&str>, model: &str, attempts: u32) -> ProviderConfig {
    ProviderConfig {
        id: Uuid::now_v7(),
        kind,
        base_url: base.into(),
        model: model.into(),
        api_key: key.map(|k| SecretString::from(k.to_string())),
        timeout_secs: 5,
        json_mode: true,
        max_attempts: attempts,
    }
}

fn fast() -> RetryPolicy {
    RetryPolicy { base_delay: Duration::from_millis(10), max_delay: Duration::from_secs(2) }
}

fn req() -> LlmRequest {
    LlmRequest { system: "SYS-RULES".into(), user: "USER-PART".into(), max_output_tokens: 100, json: true, temperature: 0.7 }
}

fn anthropic_ok() -> Value {
    json!({ "model": "claude-haiku-4-5", "stop_reason": "end_turn", "content": [{ "type": "text", "text": "{\"slots\":{}}" }],
        "usage": { "input_tokens": 12, "cache_read_input_tokens": 88, "cache_creation_input_tokens": 0, "output_tokens": 9 } })
}

fn openai_ok() -> Value {
    json!({ "model": "gpt-x", "choices": [{ "finish_reason": "stop", "message": { "content": "{\"slots\":{}}" } }],
        "usage": { "prompt_tokens": 100, "completion_tokens": 9, "prompt_tokens_details": { "cached_tokens": 64 } } })
}

fn anthropic(base: &str, model: &str, attempts: u32) -> AnthropicClient {
    AnthropicClient::new(&cfg(ProviderKind::Anthropic, base, Some("sk-ant-test-KEY"), model, attempts), fast()).unwrap()
}

fn openai(base: &str, key: Option<&str>, attempts: u32) -> OpenAiClient {
    OpenAiClient::new(&cfg(ProviderKind::OpenAiCompatible, base, key, "gpt-x", attempts), fast()).unwrap()
}

// ---------- Anthropic ----------

#[tokio::test]
async fn anthropic_request_has_the_key_the_cached_system_part_and_the_tokens_are_read() {
    let (url, mock) = start().await;
    mock.push(ok(anthropic_ok()));
    let r = anthropic(&url, "claude-haiku-4-5", 3).complete(&req()).await.unwrap();
    assert_eq!(r.text, "{\"slots\":{}}");
    assert_eq!((r.input_tokens, r.cached_tokens, r.output_tokens), (100, 88, 9));
    assert_eq!(r.model, "claude-haiku-4-5");
    let seen = &mock.seen()[0];
    assert_eq!(seen.path, "/v1/messages");
    assert_eq!(seen.headers["x-api-key"], "sk-ant-test-KEY");
    assert_eq!(seen.headers["anthropic-version"], "2023-06-01");
    assert_eq!(seen.body["model"], "claude-haiku-4-5");
    assert_eq!(seen.body["max_tokens"], 100);
    assert_eq!(seen.body["temperature"], 0.7);
    assert_eq!(seen.body["system"][0]["text"], "SYS-RULES");
    assert_eq!(seen.body["system"][0]["cache_control"]["type"], "ephemeral", "the fixed part can be cached");
    assert_eq!(seen.body["messages"][0], json!({ "role": "user", "content": "USER-PART" }));
}

#[tokio::test]
async fn anthropic_new_models_get_no_temperature_low_effort_and_room_for_thinking() {
    let (url, mock) = start().await;
    mock.push(ok(anthropic_ok()));
    anthropic(&url, "claude-sonnet-5-5", 1).complete(&req()).await.unwrap();
    let b = &mock.seen()[0].body;
    assert!(b.get("temperature").is_none());
    assert_eq!(b["output_config"]["effort"], "low");
    assert_eq!(b["max_tokens"], 100 + 1024);
}

#[tokio::test]
async fn anthropic_refusal_is_an_error() {
    let (url, mock) = start().await;
    mock.push(ok(json!({ "stop_reason": "refusal", "content": [], "usage": {} })));
    assert_eq!(anthropic(&url, "claude-haiku-4-5", 3).complete(&req()).await.unwrap_err(), LlmError::Refused);
    assert_eq!(mock.seen().len(), 1, "a refusal is not retried");
}

// ---------- OpenAI-compatible ----------

#[tokio::test]
async fn openai_request_has_the_bearer_key_json_mode_and_the_tokens_are_read() {
    let (url, mock) = start().await;
    mock.push(ok(openai_ok()));
    let r = openai(&format!("{url}/v1/"), Some("sk-openai-KEY"), 3).complete(&req()).await.unwrap();
    assert_eq!((r.input_tokens, r.cached_tokens, r.output_tokens), (100, 64, 9));
    let seen = &mock.seen()[0];
    assert_eq!(seen.path, "/v1/chat/completions", "base_url + /chat/completions");
    assert_eq!(seen.headers["authorization"], "Bearer sk-openai-KEY");
    assert_eq!(seen.body["response_format"]["type"], "json_object");
    assert_eq!(seen.body["max_tokens"], 100);
    assert_eq!(seen.body["messages"][0], json!({ "role": "system", "content": "SYS-RULES" }));
    assert_eq!(seen.body["messages"][1], json!({ "role": "user", "content": "USER-PART" }));
}

#[tokio::test]
async fn openai_without_a_key_sends_no_auth_header_and_json_mode_can_be_off() {
    let (url, mock) = start().await;
    mock.push(ok(openai_ok()));
    let mut c = cfg(ProviderKind::OpenAiCompatible, &url, None, "llama3", 1);
    c.json_mode = false;
    OpenAiClient::new(&c, fast()).unwrap().complete(&req()).await.unwrap();
    let seen = &mock.seen()[0];
    assert!(!seen.headers.contains_key("authorization"));
    assert!(seen.body.get("response_format").is_none());
}

// ---------- errors and retries (both kinds share the code) ----------

#[tokio::test]
async fn a_429_with_retry_after_waits_and_tries_again() {
    let (url, mock) = start().await;
    mock.push(Scripted { status: 429, headers: vec![("retry-after", "1".into())], body: "{}".into(), delay_ms: 0 });
    mock.push(ok(openai_ok()));
    let t = Instant::now();
    let r = openai(&url, Some("k"), 3).complete(&req()).await.unwrap();
    assert_eq!(r.output_tokens, 9);
    assert_eq!(mock.seen().len(), 2);
    assert!(t.elapsed() >= Duration::from_millis(950), "Retry-After was respected: {:?}", t.elapsed());
}

#[tokio::test]
async fn a_500_is_retried_and_when_the_tries_run_out_the_last_error_comes_back() {
    let (url, mock) = start().await;
    mock.push(status(500));
    mock.push(status(503));
    mock.push(ok(anthropic_ok()));
    anthropic(&url, "claude-haiku-4-5", 3).complete(&req()).await.unwrap();
    assert_eq!(mock.seen().len(), 3);

    let (url, mock) = start().await;
    for _ in 0..5 {
        mock.push(status(500));
    }
    let e = openai(&url, Some("k"), 3).complete(&req()).await.unwrap_err();
    assert_eq!(e, LlmError::Transient { code: "server_error", retry_after: None });
    assert_eq!(mock.seen().len(), 3, "max_attempts tries, no more");
    let (url, mock) = start().await;
    mock.push(status(429));
    mock.push(status(429));
    let e = openai(&url, Some("k"), 2).complete(&req()).await.unwrap_err();
    assert_eq!(e.code(), "rate_limited");
    assert_eq!(mock.seen().len(), 2);
}

#[tokio::test]
async fn a_401_and_a_403_are_auth_errors_and_not_retried() {
    for code in [401, 403] {
        let (url, mock) = start().await;
        mock.push(status(code));
        let e = anthropic(&url, "claude-haiku-4-5", 3).complete(&req()).await.unwrap_err();
        assert_eq!(e, LlmError::Auth);
        assert_eq!(mock.seen().len(), 1);
    }
}

#[tokio::test]
async fn a_400_fails_the_request_with_the_provider_message_and_is_not_retried() {
    let (url, mock) = start().await;
    mock.push(Scripted { status: 400, headers: vec![], body: json!({ "error": { "message": "prompt is too long" } }).to_string(), delay_ms: 0 });
    let e = openai(&url, Some("k"), 3).complete(&req()).await.unwrap_err();
    assert_eq!(e, LlmError::BadRequest("prompt is too long".into()));
    assert_eq!(mock.seen().len(), 1);
}

#[tokio::test]
async fn a_slow_server_times_out_and_is_retried() {
    let (url, mock) = start().await;
    mock.push(Scripted { status: 200, headers: vec![], body: openai_ok().to_string(), delay_ms: 2500 });
    mock.push(ok(openai_ok()));
    let mut c = cfg(ProviderKind::OpenAiCompatible, &url, Some("k"), "m", 2);
    c.timeout_secs = 1;
    let r = OpenAiClient::new(&c, fast()).unwrap().complete(&req()).await.unwrap();
    assert_eq!(r.output_tokens, 9);
    assert_eq!(mock.seen().len(), 2);

    let (url, mock) = start().await;
    mock.push(Scripted { status: 200, headers: vec![], body: openai_ok().to_string(), delay_ms: 2500 });
    let mut c = cfg(ProviderKind::OpenAiCompatible, &url, Some("k"), "m", 1);
    c.timeout_secs = 1;
    let e = OpenAiClient::new(&c, fast()).unwrap().complete(&req()).await.unwrap_err();
    assert_eq!(e.code(), "timeout");
}

#[tokio::test]
async fn a_closed_port_is_a_network_error_and_a_non_json_answer_is_a_bad_response() {
    let e = openai("http://127.0.0.1:1", Some("k"), 2).complete(&req()).await.unwrap_err();
    assert_eq!(e.code(), "network");
    let (url, mock) = start().await;
    mock.push(Scripted { status: 200, headers: vec![], body: "<html>hello</html>".into(), delay_ms: 0 });
    assert!(matches!(openai(&url, Some("k"), 3).complete(&req()).await.unwrap_err(), LlmError::BadResponse(_)));
    assert_eq!(mock.seen().len(), 1);
}

// ---------- the provider store and the routes ----------

struct P {
    state: aikido_email::state::AppState,
    keys: HashMap<Scope, String>,
}

impl P {
    async fn new(pool: PgPool) -> Self {
        let mut state = app_state(pool);
        state.llm = Arc::new(RealLlmFactory::with_retry(fast()));
        let keys = bootstrap_keys(&state).await;
        Self { state, keys }
    }
    async fn api(&self, method: &str, uri: &str, body: Option<Value>) -> Response {
        call(&self.state, method, uri, Some(&self.keys[&Scope::Generator]), body).await
    }
    async fn add(&self, body: Value) -> Value {
        let r = self.api("POST", "/v1/generator/providers", Some(body)).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.body);
        r.body
    }
}

#[sqlx::test]
async fn a_provider_is_saved_with_an_encrypted_key_that_is_never_returned(pool: PgPool) {
    let p = P::new(pool).await;
    let secret = "sk-ant-SUPER-SECRET-123456";
    let added = p.add(json!({ "kind": "anthropic", "name": "Claude", "model": "claude-haiku-5-5", "api_key": secret })).await;
    assert_eq!(added["has_api_key"], true);
    assert_eq!(added["base_url"], "https://api.anthropic.com");
    assert_eq!(added["status"], "ok");
    assert!(added.get("api_key").is_none());
    let id: Uuid = added["id"].as_str().unwrap().parse().unwrap();

    for r in [p.api("GET", "/v1/generator/providers", None).await, p.api("PATCH", &format!("/v1/generator/providers/{id}"), Some(json!({ "name": "Renamed" }))).await] {
        assert!(!r.body.to_string().contains(secret), "the key is write only");
    }
    // The database holds ciphertext only.
    let (ct, nonce, version): (Vec<u8>, Vec<u8>, i32) =
        sqlx::query_as("SELECT api_key_ciphertext, api_key_nonce, key_version FROM llm_providers WHERE id = $1").bind(id).fetch_one(&p.state.pool).await.unwrap();
    assert!(!String::from_utf8_lossy(&ct).contains("SECRET") && nonce.len() == 12 && version == 1);
    let (cfg, status) = providers::load_config(&p.state.pool, &p.state.crypto, id, 3).await.unwrap();
    assert_eq!(status, "ok");
    use secrecy::ExposeSecret;
    assert_eq!(cfg.api_key.as_ref().unwrap().expose_secret(), secret);
    assert!(!format!("{cfg:?}").contains("SECRET"), "a debug print has no key");

    // The key is bound to the provider id (AAD): moved to another row it can not be read.
    let other = p.add(json!({ "kind": "openai_compatible", "name": "Local", "base_url": "http://localhost:11434/v1", "model": "llama3" })).await;
    let other_id: Uuid = other["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(other["has_api_key"], false);
    sqlx::query("UPDATE llm_providers SET api_key_ciphertext = $2, api_key_nonce = $3, key_version = $4 WHERE id = $1")
        .bind(other_id)
        .bind(&ct)
        .bind(&nonce)
        .bind(version)
        .execute(&p.state.pool)
        .await
        .unwrap();
    assert!(matches!(providers::load_config(&p.state.pool, &p.state.crypto, other_id, 3).await, Err(providers::ConfigError::BadKey)));
    let t = p.api("POST", &format!("/v1/generator/providers/{other_id}/test"), None).await;
    assert_eq!(t.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(t.body["error"]["code"], "provider_key_unreadable");
}

#[sqlx::test]
async fn bad_provider_fields_get_422(pool: PgPool) {
    let p = P::new(pool).await;
    let base = json!({ "kind": "openai_compatible", "name": "n", "base_url": "http://localhost:1/v1", "model": "m" });
    let bad = |patch: Value| {
        let mut b = base.clone();
        for (k, v) in patch.as_object().unwrap() {
            b[k] = v.clone();
        }
        b
    };
    for body in [
        bad(json!({ "kind": "gemini" })),
        bad(json!({ "name": " " })),
        bad(json!({ "model": "" })),
        bad(json!({ "base_url": null })),
        bad(json!({ "base_url": "ftp://x.com" })),
        bad(json!({ "base_url": "http://user:pass@x.com" })),
        bad(json!({ "base_url": "not a url" })),
        bad(json!({ "max_concurrency": 0 })),
        bad(json!({ "timeout_secs": 1 })),
        bad(json!({ "api_key": "bad\nkey" })),
        json!({ "kind": "anthropic", "name": "n", "model": "m" }),
    ] {
        let r = p.api("POST", "/v1/generator/providers", Some(body.clone())).await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{body} -> {}", r.body);
        assert_eq!(r.body["error"]["code"], "invalid_provider");
    }
    assert_eq!(p.api("GET", "/v1/generator/providers", None).await.body["providers"].as_array().unwrap().len(), 0);
    assert_eq!(p.api("PATCH", &format!("/v1/generator/providers/{}", Uuid::now_v7()), Some(json!({ "name": "x" }))).await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn the_test_route_works_and_a_refused_key_sets_auth_failed_until_the_key_is_replaced(pool: PgPool) {
    let (url, mock) = start().await;
    let p = P::new(pool).await;
    let added = p.add(json!({ "kind": "openai_compatible", "name": "Mock", "base_url": format!("{url}/v1"), "model": "gpt-x", "api_key": "k1" })).await;
    let id = added["id"].as_str().unwrap().to_string();

    mock.push(ok(openai_ok()));
    let r = p.api("POST", &format!("/v1/generator/providers/{id}/test"), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["ok"], true);
    assert_eq!(r.body["model"], "gpt-x");
    assert!(r.body["duration_ms"].is_number());

    mock.push(status(401));
    let r = p.api("POST", &format!("/v1/generator/providers/{id}/test"), None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!((r.body["ok"].clone(), r.body["error_code"].clone()), (json!(false), json!("provider_auth")));
    let list = p.api("GET", "/v1/generator/providers", None).await.body;
    assert_eq!(list["providers"][0]["status"], "auth_failed");

    // A new key clears it; so does a good test.
    let r = p.api("PATCH", &format!("/v1/generator/providers/{id}"), Some(json!({ "api_key": "k2" }))).await;
    assert_eq!(r.body["status"], "ok");
    mock.push(status(403));
    p.api("POST", &format!("/v1/generator/providers/{id}/test"), None).await;
    mock.push(ok(openai_ok()));
    p.api("POST", &format!("/v1/generator/providers/{id}/test"), None).await;
    assert_eq!(p.api("GET", "/v1/generator/providers", None).await.body["providers"][0]["status"], "ok");
    assert_eq!(mock.seen().last().unwrap().headers["authorization"], "Bearer k2");

    // one llm_calls row per call, purpose `test`, no text
    let rows: Vec<(String, bool, Option<String>)> = sqlx::query_as("SELECT purpose, ok, error_code FROM llm_calls ORDER BY id").fetch_all(&p.state.pool).await.unwrap();
    assert_eq!(rows.len(), 4);
    assert!(rows.iter().all(|r| r.0 == "test"));
    assert_eq!(rows[1], ("test".into(), false, Some("provider_auth".into())));
    assert_eq!(p.api("POST", &format!("/v1/generator/providers/{}/test", Uuid::now_v7()), None).await.status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn settings_have_defaults_validate_and_point_only_at_real_providers(pool: PgPool) {
    let p = P::new(pool).await;
    let s = p.api("GET", "/v1/generator/settings", None).await.body["settings"].clone();
    assert_eq!(s["lookahead_hours"], 6);
    assert_eq!(s["on_failure"], "fallback");
    assert_eq!(s["max_wait_mins"], 120);
    assert_eq!(s["review_required"], false);
    assert_eq!(s["context_max_chars"], 4000);
    assert_eq!(s["max_output_tokens"], 400);
    assert_eq!(s["temperature"], 0.7);
    assert_eq!(s["max_attempts"], 3);
    assert_eq!(s["concurrency"], 4);
    assert_eq!(s["daily_token_budget"], 0);
    assert_eq!(s["spam_check"], "warn");
    assert!(s["slot_provider_id"].is_null());
    assert!(s["banned_phrases"].as_array().unwrap().len() >= 3);

    for (patch, code) in [
        (json!({ "lookahead_hours": 0 }), "invalid_setting"),
        (json!({ "lookahead_hours": 100 }), "invalid_setting"),
        (json!({ "on_failure": "explode" }), "invalid_settings"),
        (json!({ "nope": 1 }), "unknown_setting"),
        (json!({ "slot_provider_id": Uuid::now_v7() }), "unknown_provider"),
    ] {
        let r = p.api("PATCH", "/v1/generator/settings", Some(patch.clone())).await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{patch}");
        assert_eq!(r.body["error"]["code"], code, "{patch}");
    }
    let prov = p.add(json!({ "kind": "openai_compatible", "name": "n", "base_url": "http://localhost:1/v1", "model": "m" })).await;
    let r = p.api("PATCH", "/v1/generator/settings", Some(json!({ "slot_provider_id": prov["id"], "on_failure": "wait", "lookahead_hours": 12 }))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(p.api("GET", "/v1/generator/settings", None).await.body["settings"]["slot_provider_id"], prov["id"]);

    // a provider that a setting uses can not be deleted
    let del = p.api("DELETE", &format!("/v1/generator/providers/{}", prov["id"].as_str().unwrap()), None).await;
    assert_eq!(del.status, StatusCode::CONFLICT);
    assert_eq!(del.body["error"]["code"], "provider_in_use");
    p.api("PATCH", "/v1/generator/settings", Some(json!({ "slot_provider_id": null }))).await;
    assert_eq!(p.api("DELETE", &format!("/v1/generator/providers/{}", prov["id"].as_str().unwrap()), None).await.status, StatusCode::OK);
    assert_eq!(p.api("DELETE", &format!("/v1/generator/providers/{}", prov["id"].as_str().unwrap()), None).await.status, StatusCode::NOT_FOUND);
}
