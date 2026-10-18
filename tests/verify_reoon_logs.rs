//! No Reoon key and no email text in the logs, even when the network call fails (the URL holds the key).
//! This file sets the global logger, so it holds one test.
mod common;
use aikido_email::auth::api_keys::Scope;
use aikido_email::verify_reoon::client::{Mode, RealVerifierFactory, VerifierFactory};
use common::*;
use serde_json::json;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;

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
async fn no_secret_and_no_email_text_reach_the_logs(pool: PgPool) {
    let logs = LogBuffer::default();
    tracing::subscriber::set_global_default(tracing_subscriber::fmt().with_max_level(tracing::Level::TRACE).with_ansi(false).with_writer(logs.clone()).finish())
        .expect("this test binary sets the global logger once");
    let _ = rustls::crypto::ring::default_provider().install_default();

    // nothing listens on port 1: every call is a network error whose text holds the URL
    let mut state = app_state(pool);
    state.verifier = Arc::new(RealVerifierFactory { base_url: "http://127.0.0.1:1".into(), retry_delay: Duration::from_millis(1) });
    let keys = bootstrap_keys(&state).await;
    let r = call(&state, "POST", "/v1/integrations/apps/reoon/install", Some(&keys[&Scope::Integrations]), Some(json!({ "api_key": "REOON-SECRET-KEY-999" }))).await;
    assert_eq!(r.status, axum::http::StatusCode::BAD_GATEWAY);
    assert!(!r.body.to_string().contains("REOON-SECRET-KEY-999"));

    let client = state.verifier.build(secrecy::SecretString::from("REOON-SECRET-KEY-999".to_string()));
    let _ = client.verify("secret.person@private-company.example", Mode::Power).await;
    let _ = client.balance().await;

    let text = String::from_utf8_lossy(&logs.0.lock().unwrap()).to_string();
    assert!(!text.contains("REOON-SECRET-KEY-999"), "the key reached the logs");
    assert!(!text.contains("secret.person"), "an email address reached the logs");
    assert!(!text.contains("/api/v1/") && !text.contains("key="), "the URL reached the logs");
}
