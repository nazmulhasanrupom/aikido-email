use super::{MailSender, OutgoingMessage, SendError, SendReceipt};
use async_trait::async_trait;

/// Logs the email and returns `Ok`. On when `AIKIDO_DRY_RUN=true`.
/// Runs the whole engine with real timing and sends no real emails.
pub struct DryRunSender;

#[async_trait]
impl MailSender for DryRunSender {
    async fn send(&self, msg: &OutgoingMessage) -> Result<SendReceipt, SendError> {
        tracing::info!(to = %msg.to_email, from = %msg.from_email, subject = %msg.subject, "DRY RUN: email not sent");
        Ok(SendReceipt { provider_message_id: Some(format!("dry-run-{}", uuid::Uuid::now_v7())), provider_thread_id: None, message_id_header: None })
    }
}
