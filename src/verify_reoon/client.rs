//! The Reoon client. One trait (`EmailVerifier`), a real client and a fake for tests.
//! The key travels in the query string, so an error from `reqwest` is never logged or returned:
//! it holds the full URL. Only short codes leave this file.
use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use utoipa::ToSchema;

pub const DEFAULT_BASE_URL: &str = "https://emailverifier.reoon.com";
/// Not shown in the PDF of the user. Check the exact path on the live Reoon page.
pub const BALANCE_PATH: &str = "/api/v1/check-account-balance/";
pub const VERIFY_PATH: &str = "/api/v1/verify";

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Quick,
    Power,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Quick => "quick",
            Mode::Power => "power",
        }
    }
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "quick" => Some(Mode::Quick),
            "power" => Some(Mode::Power),
            _ => None,
        }
    }
    fn timeout(self) -> Duration {
        match self {
            Mode::Quick => Duration::from_secs(10),
            Mode::Power => Duration::from_secs(120),
        }
    }
}

/// What we keep of a Reoon answer.
#[derive(Clone, Debug, PartialEq)]
pub struct Verification {
    /// Any string. Unknown values are stored as they are.
    pub status: String,
    pub score: Option<i32>,
    pub safe_to_send: Option<bool>,
    /// The boolean fields of the answer (`is_catch_all`, `is_disposable`, ...).
    pub flags: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, ToSchema)]
pub struct Balance {
    pub daily_credits: Option<i64>,
    pub instant_credits: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum VerifyError {
    /// Network error, timeout, 5xx. Worth another try later.
    Transient(&'static str),
    /// 401, 403 or a "bad key" answer.
    Auth,
    OutOfCredits,
    /// Other 4xx. This request will never work.
    BadRequest(String),
    /// The answer has a shape we do not know (for example no `status`).
    BadResponse(String),
}

impl VerifyError {
    pub fn code(&self) -> String {
        match self {
            VerifyError::Transient(c) => (*c).to_string(),
            VerifyError::Auth => "auth_failed".into(),
            VerifyError::OutOfCredits => "out_of_credits".into(),
            VerifyError::BadRequest(_) => "bad_request".into(),
            VerifyError::BadResponse(_) => "bad_response".into(),
        }
    }
}

#[async_trait]
pub trait EmailVerifier: Send + Sync {
    async fn verify(&self, email: &str, mode: Mode) -> Result<Verification, VerifyError>;
    async fn balance(&self) -> Result<Balance, VerifyError>;
}

/// Builds the client for one key. Tests replace the real one with `FakeFactory`.
pub trait VerifierFactory: Send + Sync {
    fn build(&self, key: SecretString) -> Arc<dyn EmailVerifier>;
}

#[derive(Clone)]
pub struct RealVerifierFactory {
    pub base_url: String,
    pub retry_delay: Duration,
}

impl Default for RealVerifierFactory {
    fn default() -> Self {
        // AIKIDO_REOON_BASE_URL points the server at a mock Reoon (the browser tests use it)
        let base_url = std::env::var("AIKIDO_REOON_BASE_URL").ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| DEFAULT_BASE_URL.into());
        Self { base_url, retry_delay: Duration::from_secs(2) }
    }
}

impl VerifierFactory for RealVerifierFactory {
    fn build(&self, key: SecretString) -> Arc<dyn EmailVerifier> {
        Arc::new(ReoonClient { http: reqwest::Client::new(), base_url: self.base_url.trim_end_matches('/').to_string(), key, retry_delay: self.retry_delay })
    }
}

pub struct ReoonClient {
    http: reqwest::Client,
    base_url: String,
    key: SecretString,
    retry_delay: Duration,
}

/// Tries for one call: the first one and one more.
const TRIES: u32 = 2;

impl ReoonClient {
    /// One GET with retries on network errors and 5xx. Returns the JSON body of a 2xx answer.
    async fn get(&self, path: &str, extra: &[(&str, &str)], timeout: Duration) -> Result<Value, VerifyError> {
        let mut last = VerifyError::Transient("network");
        for attempt in 1..=TRIES {
            let Ok(mut url) = reqwest::Url::parse(&format!("{}{path}", self.base_url)) else { return Err(VerifyError::BadRequest("the Reoon address is not valid".into())) };
            {
                let mut pairs = url.query_pairs_mut();
                for (k, v) in extra {
                    pairs.append_pair(k, v);
                }
                pairs.append_pair("key", self.key.expose_secret());
            }
            let sent = self.http.get(url).timeout(timeout).send().await;
            let outcome = match sent {
                // the reqwest error holds the URL (and the key): only a code is kept
                Err(e) => Err(VerifyError::Transient(if e.is_timeout() { "timeout" } else { "network" })),
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let text = resp.text().await.unwrap_or_default();
                    classify(status, &text)
                }
            };
            match outcome {
                Ok(v) => return Ok(v),
                Err(VerifyError::Transient(c)) => {
                    last = VerifyError::Transient(c);
                    if attempt < TRIES {
                        tokio::time::sleep(self.retry_delay * attempt).await;
                    }
                }
                Err(other) => return Err(other),
            }
        }
        Err(last)
    }
}

fn mentions(text: &str, word: &str) -> bool {
    text.to_ascii_lowercase().contains(word)
}

/// Status code and body to a JSON value or an error. The exact answers of Reoon for a bad key and for no
/// credits are not confirmed yet (see progress.md): 401/403 and 402 are certain, the body words are a guess.
fn classify(status: u16, text: &str) -> Result<Value, VerifyError> {
    match status {
        401 | 403 => return Err(VerifyError::Auth),
        402 => return Err(VerifyError::OutOfCredits),
        408 | 429 | 500..=599 => return Err(VerifyError::Transient(if status == 429 { "rate_limited" } else if status == 408 { "timeout" } else { "server_error" })),
        200..=299 => {}
        _ => return Err(VerifyError::BadRequest(format!("Reoon answered {status}"))),
    }
    let body: Value = serde_json::from_str(text).map_err(|_| VerifyError::BadResponse("the answer is not JSON".into()))?;
    let is_error = body.get("status").and_then(Value::as_str) == Some("error") || body.get("api_status").and_then(Value::as_str) == Some("error");
    if is_error {
        let reason = body.get("reason").or_else(|| body.get("message")).and_then(Value::as_str).unwrap_or("");
        if mentions(reason, "credit") {
            return Err(VerifyError::OutOfCredits);
        }
        if mentions(reason, "key") || mentions(reason, "auth") {
            return Err(VerifyError::Auth);
        }
        return Err(VerifyError::BadRequest("Reoon refused the request".into()));
    }
    Ok(body)
}

const FLAGS: &[&str] = &[
    "is_safe_to_send", "is_deliverable", "is_catch_all", "is_disposable", "is_role_account", "is_spamtrap", "has_inbox_full",
    "is_disabled", "is_free_email", "can_connect_smtp", "mx_accepts_mail",
];

/// Power and quick answers. Unknown fields are ignored. A missing `status` is an error.
pub fn parse_verification(body: &Value) -> Result<Verification, VerifyError> {
    let status = body.get("status").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| VerifyError::BadResponse("the answer has no status".into()))?;
    let mut flags = Map::new();
    for f in FLAGS {
        if let Some(b) = body.get(*f).and_then(Value::as_bool) {
            flags.insert((*f).into(), Value::Bool(b));
        }
    }
    let score = body.get("overall_score").and_then(|v| v.as_f64()).map(|s| s.round().clamp(0.0, 100.0) as i32);
    Ok(Verification { status: status.to_ascii_lowercase(), score, safe_to_send: body.get("is_safe_to_send").and_then(Value::as_bool), flags: Value::Object(flags) })
}

pub fn parse_balance(body: &Value) -> Result<Balance, VerifyError> {
    let n = |k: &str| body.get(k).and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)));
    let (daily, instant) = (n("remaining_daily_credits"), n("remaining_instant_credits"));
    if daily.is_none() && instant.is_none() {
        return Err(VerifyError::BadResponse("the answer has no credits".into()));
    }
    Ok(Balance { daily_credits: daily, instant_credits: instant })
}

#[async_trait]
impl EmailVerifier for ReoonClient {
    async fn verify(&self, email: &str, mode: Mode) -> Result<Verification, VerifyError> {
        let body = self.get(VERIFY_PATH, &[("email", email), ("mode", mode.as_str())], mode.timeout()).await?;
        parse_verification(&body)
    }

    async fn balance(&self) -> Result<Balance, VerifyError> {
        let body = self.get(BALANCE_PATH, &[], Duration::from_secs(15)).await?;
        parse_balance(&body)
    }
}

// ---------- the fake ----------

/// For tests. Answers from a script, then a default. Counts the calls.
pub struct FakeVerifier {
    script: Mutex<VecDeque<Result<Verification, VerifyError>>>,
    balance: Mutex<Result<Balance, VerifyError>>,
    pub verify_calls: AtomicUsize,
    pub balance_calls: AtomicUsize,
    pub emails: Mutex<Vec<String>>,
}

impl FakeVerifier {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(VecDeque::new()),
            balance: Mutex::new(Ok(Balance { daily_credits: Some(150), instant_credits: Some(5000) })),
            verify_calls: AtomicUsize::new(0),
            balance_calls: AtomicUsize::new(0),
            emails: Mutex::new(Vec::new()),
        })
    }
    pub fn push(&self, r: Result<Verification, VerifyError>) {
        self.script.lock().unwrap().push_back(r);
    }
    pub fn set_balance(&self, r: Result<Balance, VerifyError>) {
        *self.balance.lock().unwrap() = r;
    }
    pub fn calls(&self) -> usize {
        self.verify_calls.load(Ordering::SeqCst)
    }
    pub fn safe() -> Verification {
        Verification { status: "safe".into(), score: Some(98), safe_to_send: Some(true), flags: serde_json::json!({ "is_safe_to_send": true, "is_deliverable": true }) }
    }
    pub fn with_status(status: &str) -> Verification {
        Verification { status: status.into(), score: Some(60), safe_to_send: Some(false), flags: serde_json::json!({}) }
    }
}

#[async_trait]
impl EmailVerifier for FakeVerifier {
    async fn verify(&self, email: &str, _mode: Mode) -> Result<Verification, VerifyError> {
        self.verify_calls.fetch_add(1, Ordering::SeqCst);
        self.emails.lock().unwrap().push(email.to_string());
        self.script.lock().unwrap().pop_front().unwrap_or_else(|| Ok(Self::safe()))
    }
    async fn balance(&self) -> Result<Balance, VerifyError> {
        self.balance_calls.fetch_add(1, Ordering::SeqCst);
        self.balance.lock().unwrap().clone()
    }
}

pub struct FakeFactory(pub Arc<FakeVerifier>);

impl VerifierFactory for FakeFactory {
    fn build(&self, _key: SecretString) -> Arc<dyn EmailVerifier> {
        self.0.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn power_and_quick_answers_parse_and_unknown_status_is_kept() {
        let power = json!({ "status": "safe", "overall_score": 98, "is_safe_to_send": true, "is_catch_all": false, "extra": 1 });
        let v = parse_verification(&power).unwrap();
        assert_eq!((v.status.as_str(), v.score, v.safe_to_send), ("safe", Some(98), Some(true)));
        assert_eq!(v.flags["is_catch_all"], json!(false));
        let quick = parse_verification(&json!({ "status": "valid" })).unwrap();
        assert_eq!((quick.status.as_str(), quick.score), ("valid", None));
        assert_eq!(parse_verification(&json!({ "status": "Brand_New" })).unwrap().status, "brand_new");
        assert!(matches!(parse_verification(&json!({ "email": "a@b.c" })), Err(VerifyError::BadResponse(_))));
    }

    #[test]
    fn answers_map_to_errors() {
        assert_eq!(classify(401, ""), Err(VerifyError::Auth));
        assert_eq!(classify(402, ""), Err(VerifyError::OutOfCredits));
        assert_eq!(classify(503, ""), Err(VerifyError::Transient("server_error")));
        assert_eq!(classify(200, r#"{"status":"error","reason":"Not enough credits"}"#), Err(VerifyError::OutOfCredits));
        assert_eq!(classify(200, r#"{"status":"error","reason":"Invalid API key"}"#), Err(VerifyError::Auth));
        assert!(classify(200, r#"{"status":"safe"}"#).is_ok());
    }

    #[test]
    fn balance_parses() {
        let b = parse_balance(&json!({ "api_status": "success", "remaining_daily_credits": 150, "remaining_instant_credits": 5000, "status": "ok" })).unwrap();
        assert_eq!((b.daily_credits, b.instant_credits), (Some(150), Some(5000)));
        assert!(parse_balance(&json!({ "api_status": "success" })).is_err());
    }
}
