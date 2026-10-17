//! One generation: prompt -> call -> read the JSON -> guard -> (one repair try) -> done.
//! The worker, the preview and the follow-up preview all use this. It touches no database.
use super::guard::{self, GuardOptions};
use super::llm::{LlmClient, LlmError, LlmRequest};
use super::parse::parse_slots;
use super::prompt::{self, PromptLead, PromptStep};
use super::repo::CallRecord;
use super::settings::{GeneratorSettings, SpamCheck};
use super::slots::AiSlot;
use std::collections::BTreeMap;
use std::time::Instant;
use uuid::Uuid;

pub struct GenInput<'a> {
    pub client: &'a dyn LlmClient,
    pub settings: &'a GeneratorSettings,
    pub step: PromptStep<'a>,
    pub slots: &'a [AiSlot],
    pub brief: Option<&'a str>,
    pub lead: &'a PromptLead,
}

/// One call, as it is saved in `llm_calls`.
#[derive(Clone, Debug, Default)]
pub struct CallInfo {
    pub model: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_tokens: u32,
    pub duration_ms: u32,
    pub ok: bool,
    pub error_code: Option<String>,
}

impl CallInfo {
    pub fn record(&self, purpose: &'static str, provider_id: Option<Uuid>, campaign_id: Option<Uuid>, fallback_model: &str) -> CallRecord {
        CallRecord {
            purpose,
            provider_id,
            model: if self.model.is_empty() { fallback_model.to_string() } else { self.model.clone() },
            campaign_id,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cached_tokens: self.cached_tokens,
            duration_ms: self.duration_ms,
            ok: self.ok,
            error_code: self.error_code.clone(),
        }
    }
}

#[derive(Debug)]
pub struct Generated {
    pub slots: BTreeMap<String, String>,
    pub warnings: Vec<String>,
    pub calls: Vec<CallInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailKind {
    /// Rate limit, server error, timeout: try again later.
    Transient,
    /// The key was refused.
    Auth,
    /// This request will never work.
    Permanent,
}

#[derive(Debug)]
pub struct GenFailure {
    pub kind: FailKind,
    pub code: String,
    pub message: String,
    pub calls: Vec<CallInfo>,
}

impl Generated {
    pub fn totals(&self) -> (u32, u32, u32) {
        totals(&self.calls)
    }
}

pub fn totals(calls: &[CallInfo]) -> (u32, u32, u32) {
    calls.iter().fold((0, 0, 0), |a, c| (a.0 + c.input_tokens, a.1 + c.output_tokens, a.2 + c.cached_tokens))
}

pub fn failure_of(e: &LlmError, calls: Vec<CallInfo>) -> GenFailure {
    let kind = match e {
        LlmError::Transient { .. } => FailKind::Transient,
        LlmError::Auth => FailKind::Auth,
        _ => FailKind::Permanent,
    };
    let message = match e {
        LlmError::Transient { code, .. } => format!("The provider could not answer ({code})."),
        LlmError::Auth => "The provider refused the API key.".into(),
        LlmError::BadRequest(m) => format!("The provider refused the request: {m}"),
        LlmError::Refused => "The model declined to write this text.".into(),
        LlmError::BadResponse(m) => format!("The provider's answer could not be read: {m}"),
    };
    GenFailure { kind, code: e.code(), message, calls }
}

/// Write the slots of one step for one lead. Calls the model once, and once more if the first answer is not valid.
pub async fn generate(input: &GenInput<'_>) -> Result<Generated, GenFailure> {
    let s = input.settings;
    let system = prompt::system_prompt(&input.step, input.slots, input.brief, s);
    let user = prompt::user_prompt(&prompt::lead_block(input.lead, s.context_max_chars as usize));
    let keys: Vec<&str> = input.slots.iter().map(|x| x.key.as_str()).collect();
    let banned = s.banned_phrases.clone();
    let spam = s.spam_words.clone();
    let opts = GuardOptions { slots: input.slots, banned_phrases: &banned, spam_words: &spam, spam_fail: s.spam_check == SpamCheck::Fail, language: s.language.as_deref() };

    let mut calls: Vec<CallInfo> = Vec::new();
    let mut reason = String::new();
    let mut code = "bad_output";
    for attempt in 0..2 {
        let req = LlmRequest {
            system: system.clone(),
            user: if attempt == 0 { user.clone() } else { prompt::repair_prompt(&user, &reason) },
            max_output_tokens: s.max_output_tokens,
            json: true,
            temperature: s.temperature,
        };
        let started = Instant::now();
        let resp = match input.client.complete(&req).await {
            Ok(r) => r,
            Err(e) => {
                calls.push(CallInfo { duration_ms: started.elapsed().as_millis() as u32, ok: false, error_code: Some(e.code()), ..Default::default() });
                return Err(failure_of(&e, calls));
            }
        };
        let mut info = CallInfo {
            model: resp.model.clone(),
            input_tokens: resp.input_tokens,
            output_tokens: resp.output_tokens,
            cached_tokens: resp.cached_tokens,
            duration_ms: started.elapsed().as_millis() as u32,
            ok: true,
            error_code: None,
        };
        match parse_slots(&resp.text, &keys) {
            Err(msg) => {
                info.ok = false;
                info.error_code = Some("bad_output".into());
                calls.push(info);
                reason = msg;
                code = "bad_output";
            }
            Ok(values) => {
                let report = guard::check(&values, &opts);
                if report.ok() {
                    calls.push(info);
                    return Ok(Generated { slots: values, warnings: report.warnings, calls });
                }
                info.ok = false;
                info.error_code = Some("guard_failed".into());
                calls.push(info);
                reason = report.reason();
                code = "guard_failed";
            }
        }
    }
    // Only the code and the reason of the guard are kept, never the text of the model.
    Err(GenFailure { kind: FailKind::Permanent, code: code.into(), message: reason, calls })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generator::llm::fake::{FakeLlm, response, slots_response};
    use crate::generator::llm::LlmError;

    fn slots() -> Vec<AiSlot> {
        vec![AiSlot { key: "opener".into(), instruction: "x".into(), max_words: 10, allow_links: false, fallback: None }]
    }

    async fn run(fake: &FakeLlm) -> Result<Generated, GenFailure> {
        let settings = GeneratorSettings::default();
        let lead = PromptLead { name: "Ana".into(), ..Default::default() };
        let sl = slots();
        let input = GenInput {
            client: fake,
            settings: &settings,
            step: PromptStep { step_number: 1, subject: "Hi", body_text: Some("{{ai:opener}}"), body_html: None },
            slots: &sl,
            brief: None,
            lead: &lead,
        };
        generate(&input).await
    }

    #[tokio::test]
    async fn a_good_answer_is_used_with_one_call() {
        let fake = FakeLlm::new();
        fake.push(Ok(slots_response(&[("opener", "Nice work on the launch.")])));
        let g = run(&fake).await.unwrap();
        assert_eq!(g.slots["opener"], "Nice work on the launch.");
        assert_eq!((fake.call_count(), g.calls.len()), (1, 1));
        assert_eq!(g.totals(), (100, 20, 0));
    }

    #[tokio::test]
    async fn a_bad_answer_gets_one_repair_try_with_the_reason() {
        let fake = FakeLlm::new();
        fake.push(Ok(response("sorry, no json")));
        fake.push(Ok(slots_response(&[("opener", "Fine now.")])));
        let g = run(&fake).await.unwrap();
        assert_eq!(g.slots["opener"], "Fine now.");
        let calls = fake.calls();
        assert!(!calls[0].user.contains("refused"));
        assert!(calls[1].user.contains("Your last answer was refused") && calls[1].user.contains("no JSON"));
        assert_eq!(calls[0].system, calls[1].system, "the fixed part is the same, so it can be cached");
        assert_eq!(g.calls.len(), 2);
        assert!(!g.calls[0].ok && g.calls[1].ok);
    }

    #[tokio::test]
    async fn a_guard_failure_is_repaired_once_then_it_fails_without_the_text() {
        let fake = FakeLlm::new();
        let long = "word ".repeat(30);
        fake.push(Ok(slots_response(&[("opener", &long)])));
        fake.push(Ok(slots_response(&[("opener", "see https://x.com now")])));
        let f = run(&fake).await.unwrap_err();
        assert_eq!((f.kind, f.code.as_str(), fake.call_count()), (FailKind::Permanent, "guard_failed", 2));
        assert!(f.message.contains("link") && !f.message.contains("https://x.com"));
        assert_eq!(f.calls.len(), 2);
    }

    #[tokio::test]
    async fn provider_errors_are_classified_and_not_repaired() {
        for (err, kind, code) in [
            (LlmError::Transient { code: "rate_limited", retry_after: None }, FailKind::Transient, "rate_limited"),
            (LlmError::Auth, FailKind::Auth, "provider_auth"),
            (LlmError::BadRequest("too long".into()), FailKind::Permanent, "bad_request"),
            (LlmError::Refused, FailKind::Permanent, "refused"),
        ] {
            let fake = FakeLlm::new();
            fake.push(Err(err));
            let f = run(&fake).await.unwrap_err();
            assert_eq!((f.kind, f.code.as_str(), fake.call_count()), (kind, code, 1));
            assert!(!f.calls[0].ok);
        }
    }
}
