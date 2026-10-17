//! A scripted LLM for tests (like `FakeSender`). No network.
//!
//! Without a script it answers every request in the format the generator asks for: each slot gets "Fake text for <key>",
//! and a sequence draft request gets a draft with the number of steps asked.
use super::{LlmClient, LlmError, LlmFactory, LlmRequest, LlmResponse, ProviderConfig};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

type Responder = Box<dyn Fn(&LlmRequest) -> Result<LlmResponse, LlmError> + Send + Sync>;

pub struct FakeLlm {
    script: Mutex<VecDeque<Result<LlmResponse, LlmError>>>,
    responder: Mutex<Responder>,
    calls: Mutex<Vec<LlmRequest>>,
    count: AtomicUsize,
    delay: Mutex<Option<Duration>>,
}

pub fn response(text: impl Into<String>) -> LlmResponse {
    LlmResponse { text: text.into(), input_tokens: 100, output_tokens: 20, cached_tokens: 0, model: "fake-model".into(), stop_reason: Some("end_turn".into()) }
}

/// `{"slots": {...}}` as the model would answer.
pub fn slots_response(slots: &[(&str, &str)]) -> LlmResponse {
    let map: serde_json::Map<String, Value> = slots.iter().map(|(k, v)| (k.to_string(), json!(v))).collect();
    response(json!({ "slots": map }).to_string())
}

/// The text between `<tag>` and `</tag>`.
pub fn between<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(&text[start..end])
}

fn default_answer(req: &LlmRequest) -> Result<LlmResponse, LlmError> {
    if let Some(spec) = between(&req.system, "draft_request") {
        let n = serde_json::from_str::<Value>(spec).ok().and_then(|v| v["steps"].as_u64()).unwrap_or(3);
        let steps: Vec<Value> = (1..=n)
            .map(|i| {
                let mut s = json!({
                    "subject": if i == 1 { "Quick question for {{first_name|you}}".to_string() } else { String::new() },
                    "body_text": format!("Hi {{{{first_name|there}}}},\n\n{{{{ai:opener}}}}\n\nStep {i} body.\n\n{{{{sender_name}}}}"),
                    "ai_slots": [{ "key": "opener", "instruction": "One friendly line about their company.", "max_words": 30, "fallback": "I came across your company." }],
                });
                if i > 1 {
                    s["delay_amount"] = json!(3);
                }
                s
            })
            .collect();
        return Ok(response(json!({ "steps": steps }).to_string()));
    }
    let keys: Vec<String> = between(&req.system, "slots")
        .and_then(|s| serde_json::from_str::<Vec<Value>>(s).ok())
        .map(|v| v.iter().filter_map(|s| s["key"].as_str().map(String::from)).collect())
        .unwrap_or_default();
    let pairs: Vec<(String, String)> = keys.into_iter().map(|k| (k.clone(), format!("Fake text for {k}."))).collect();
    let refs: Vec<(&str, &str)> = pairs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    Ok(slots_response(&refs))
}

impl FakeLlm {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(VecDeque::new()),
            responder: Mutex::new(Box::new(default_answer)),
            calls: Mutex::new(Vec::new()),
            count: AtomicUsize::new(0),
            delay: Mutex::new(None),
        })
    }

    /// The next call gets this answer (first in, first out). When the script is empty the responder answers.
    pub fn push(&self, r: Result<LlmResponse, LlmError>) {
        self.script.lock().unwrap_or_else(|e| e.into_inner()).push_back(r);
    }

    pub fn set_responder(&self, f: impl Fn(&LlmRequest) -> Result<LlmResponse, LlmError> + Send + Sync + 'static) {
        *self.responder.lock().unwrap_or_else(|e| e.into_inner()) = Box::new(f);
    }

    /// Every call waits this long (to make two server copies meet).
    pub fn set_delay(&self, d: Duration) {
        *self.delay.lock().unwrap_or_else(|e| e.into_inner()) = Some(d);
    }

    pub fn call_count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }

    pub fn calls(&self) -> Vec<LlmRequest> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[async_trait]
impl LlmClient for FakeLlm {
    async fn complete(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let delay = *self.delay.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }
        self.count.fetch_add(1, Ordering::SeqCst);
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).push(req.clone());
        if let Some(r) = self.script.lock().unwrap_or_else(|e| e.into_inner()).pop_front() {
            return r;
        }
        (self.responder.lock().unwrap_or_else(|e| e.into_inner()))(req)
    }
}

/// Gives the same `FakeLlm` for every provider.
pub struct FakeFactory(pub Arc<FakeLlm>);

impl LlmFactory for FakeFactory {
    fn build(&self, _cfg: &ProviderConfig) -> Result<Arc<dyn LlmClient>, LlmError> {
        Ok(self.0.clone())
    }
}
