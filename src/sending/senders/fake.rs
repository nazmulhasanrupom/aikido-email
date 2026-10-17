use super::{MailSender, OutgoingMessage, SendError, SendReceipt};
use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Returns results from a script. Use it in tests. When the script is empty it returns `Ok`.
/// Clones share the same script and the same list of sent messages.
#[derive(Clone, Default)]
pub struct FakeSender {
    script: Arc<Mutex<VecDeque<Result<SendReceipt, SendError>>>>,
    sent: Arc<Mutex<Vec<OutgoingMessage>>>,
    delay: Arc<Mutex<std::time::Duration>>,
}

impl FakeSender {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, result: Result<SendReceipt, SendError>) {
        self.script.lock().unwrap().push_back(result);
    }

    pub fn push_many(&self, results: impl IntoIterator<Item = Result<SendReceipt, SendError>>) {
        self.script.lock().unwrap().extend(results);
    }

    /// Every send takes this long. Use it to make two workers overlap.
    pub fn set_delay(&self, delay: std::time::Duration) {
        *self.delay.lock().unwrap() = delay;
    }

    /// Every message the sender was asked to send (also the ones that got an error).
    pub fn calls(&self) -> Vec<OutgoingMessage> {
        self.sent.lock().unwrap().clone()
    }
}

#[async_trait]
impl MailSender for FakeSender {
    async fn send(&self, msg: &OutgoingMessage) -> Result<SendReceipt, SendError> {
        self.sent.lock().unwrap().push(msg.clone());
        let delay = *self.delay.lock().unwrap();
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        self.script
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(SendReceipt { provider_message_id: Some("fake-id".into()), provider_thread_id: None, message_id_header: None }))
    }
}
