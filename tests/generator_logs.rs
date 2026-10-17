//! No key, no prompt text and no slot text in the logs. This file sets the global logger, so it holds one test.
mod common;
use aikido_email::auth::api_keys::Scope;
use aikido_email::generator::llm::LlmError;
use aikido_email::generator::llm::fake::slots_response;
use common::gharness::*;
use common::*;
use serde_json::json;
use sqlx::PgPool;
use std::sync::Arc;

#[derive(Clone, Default)]
struct LogBuffer(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogBuffer {
    type Writer = LogBuffer;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[sqlx::test]
async fn no_key_no_prompt_text_and_no_slot_text_reach_the_logs(pool: PgPool) {
    let logs = LogBuffer::default();
    tracing::subscriber::set_global_default(tracing_subscriber::fmt().with_max_level(tracing::Level::TRACE).with_ansi(false).with_writer(logs.clone()).finish())
        .expect("this test binary sets the global logger once");

    let g = G::new(pool).await;
    // a provider with a key, and a lead, a note, a brief and an instruction full of marker words
    g.genok("PATCH", &format!("/v1/generator/providers/{}", g.provider), Some(json!({ "api_key": "sk-LOGCHECK-API-KEY-123" }))).await;
    let slots = json!([{ "key": "opener", "instruction": "Instruction-SECRET about the company", "fallback": "Fallback-SECRET" }]);
    let seq = g.sequence(json!([ai_step(0, "Subject-SECRET {{first_name|x}}", "Body-SECRET {{ai:opener}}", slots)])).await;
    g.api(Scope::Followups, "PATCH", &format!("/v1/followups/sequences/{seq}"), Some(json!({ "brief": "Brief-SECRET we sell things" }))).await;
    let lead = g.ok(Scope::Leads, "POST", "/v1/leads", Some(json!({ "email": "ana@acme.example", "first_name": "Lead-SECRET-Name", "company_name": "Company-SECRET" }))).await;
    let lead_id = lead["id"].as_str().unwrap().to_string();
    g.ok(Scope::Leads, "POST", &format!("/v1/leads/{lead_id}/notes"), Some(json!({ "text": "Note-SECRET" }))).await;
    g.enroll(seq, lead_id.parse().unwrap(), None).await;

    // a good call, a refused key, a transient error, a bad answer, a guard failure
    g.fake.push(Ok(slots_response(&[("opener", "Slot-SECRET-text one")])));
    g.gen_tick().await;
    g.fu_tick().await;
    let l2 = g.lead("bob@acme.example", "Bob").await;
    g.enroll(seq, l2, None).await;
    g.fake.push(Err(LlmError::Auth));
    g.gen_tick().await;
    g.genok("PATCH", &format!("/v1/generator/providers/{}", g.provider), Some(json!({ "api_key": "sk-LOGCHECK-API-KEY-456" }))).await;
    g.fake.push(Err(LlmError::Transient { code: "rate_limited", retry_after: None }));
    g.gen_tick().await;
    g.h.advance(chrono::Duration::minutes(5));
    g.fake.push(Ok(slots_response(&[("opener", "Slot-SECRET-text https://bad.example")])));
    g.fake.push(Ok(slots_response(&[("opener", "Slot-SECRET-text two")])));
    g.gen_tick().await;
    // the preview and the writer
    g.gapi("POST", "/v1/generator/preview", Some(json!({ "sequence_id": seq, "step_number": 1, "lead_id": lead_id }))).await;
    g.gapi("POST", "/v1/generator/sequences/draft", Some(json!({ "brief": { "product": "Product-SECRET" }, "steps": 1 }))).await;
    // a bad key on the API itself
    call(&g.h.state, "GET", "/v1/generator/status", Some("aik_generator_logcheck-WRONG-KEY"), None).await;

    let log = String::from_utf8_lossy(&logs.0.lock().unwrap_or_else(|e| e.into_inner())).to_string();
    assert!(log.contains("generation saved"), "the logger captured the generator: {log}");
    assert!(log.contains("refused the key"), "the refused key was logged (by id only)");
    let mut secrets: Vec<String> = g.keys.values().cloned().collect();
    secrets.extend(
        ["sk-LOGCHECK-API-KEY", "SECRET", "aik_generator_logcheck-WRONG-KEY", "super-secret-password", "ana@acme.example", "bob@acme.example"].map(String::from),
    );
    for secret in secrets {
        assert!(!log.contains(&secret), "{secret:?} is in the logs");
    }
}
