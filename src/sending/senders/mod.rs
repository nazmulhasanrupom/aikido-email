//! One trait, three providers. The worker never knows which provider it uses.
//! Each provider maps its own errors into the same `SendError`.
pub mod dry_run;
pub mod fake;
pub mod gmail;
pub mod microsoft;
pub mod smtp;

use crate::accounts::model::{EmailAccount, Provider, Secrets};
use crate::sending::settings::ListUnsubscribe;
use async_trait::async_trait;
use lettre::message::header::{HeaderName, HeaderValue};
use lettre::message::{Mailbox, MultiPart, SinglePart};
use lettre::{Address, Message};
use std::collections::BTreeMap;
use std::time::Duration;

/// Everything needed to build one email.
#[derive(Clone, Debug)]
pub struct OutgoingMessage {
    /// Full header value, like `<uuid@example.com>`.
    pub message_id: String,
    pub from_email: String,
    pub from_name: Option<String>,
    pub to_email: String,
    pub to_name: Option<String>,
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    pub list_unsubscribe: Option<ListUnsubscribe>,
    /// Gmail only: put the email in this thread.
    pub thread_id: Option<String>,
}

/// Header names the caller may not set. We set these ourselves.
pub const RESERVED_HEADERS: &[&str] = &[
    "from", "to", "cc", "bcc", "subject", "message-id", "in-reply-to", "references", "date", "mime-version",
    "content-type", "content-transfer-encoding", "sender", "reply-to-sender", "list-unsubscribe",
    "list-unsubscribe-post",
];

pub fn new_message_id(from_email: &str) -> String {
    let domain = from_email.rsplit('@').next().filter(|d| !d.is_empty()).unwrap_or("localhost");
    format!("<{}@{}>", uuid::Uuid::now_v7(), domain)
}

impl OutgoingMessage {
    /// Builds the MIME message. `multipart/alternative` when both HTML and text exist.
    pub fn build(&self) -> Result<Message, SendError> {
        let bad = |what: &str, e: &dyn std::fmt::Display| SendError::Permanent {
            reason: format!("bad {what}: {e}"),
            hard_bounce: false,
        };
        let from = Mailbox::new(
            self.from_name.clone().filter(|n| !n.is_empty()),
            self.from_email.parse::<Address>().map_err(|e| bad("from address", &e))?,
        );
        let to = Mailbox::new(
            self.to_name.clone().filter(|n| !n.is_empty()),
            self.to_email.parse::<Address>().map_err(|e| bad("recipient address", &e))?,
        );
        let mut builder = Message::builder()
            .from(from)
            .to(to)
            .subject(self.subject.clone())
            .message_id(Some(self.message_id.clone()));
        if let Some(v) = &self.in_reply_to {
            builder = builder.in_reply_to(v.clone());
        }
        if let Some(v) = &self.references {
            builder = builder.references(v.clone());
        }
        let mut msg = match (&self.body_text, &self.body_html) {
            (Some(t), Some(h)) => builder
                .multipart(MultiPart::alternative_plain_html(t.clone(), h.clone()))
                .map_err(|e| bad("message", &e))?,
            (None, Some(h)) => builder.singlepart(SinglePart::html(h.clone())).map_err(|e| bad("message", &e))?,
            (Some(t), None) => builder.singlepart(SinglePart::plain(t.clone())).map_err(|e| bad("message", &e))?,
            (None, None) => return Err(SendError::Permanent { reason: "the email has no body".into(), hard_bounce: false }),
        };
        let mut raw: Vec<(String, String)> = self
            .headers
            .iter()
            .filter(|(k, _)| !RESERVED_HEADERS.contains(&k.to_ascii_lowercase().as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if let Some(lu) = &self.list_unsubscribe {
            let mut parts = Vec::new();
            if let Some(m) = lu.mailto.as_deref().filter(|m| !m.is_empty()) {
                parts.push(format!("<mailto:{m}>"));
            }
            if let Some(u) = lu.url.as_deref().filter(|u| !u.is_empty()) {
                parts.push(format!("<{u}>"));
            }
            if !parts.is_empty() {
                raw.push(("List-Unsubscribe".into(), parts.join(", ")));
                if lu.url.as_deref().is_some_and(|u| !u.is_empty()) {
                    raw.push(("List-Unsubscribe-Post".into(), "List-Unsubscribe=One-Click".into()));
                }
            }
        }
        for (name, value) in raw {
            let name = HeaderName::new_from_ascii(name.clone()).map_err(|e| bad(&format!("header name '{name}'"), &e))?;
            msg.headers_mut().insert_raw(HeaderValue::new(name, value));
        }
        Ok(msg)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SendReceipt {
    pub provider_message_id: Option<String>,
    pub provider_thread_id: Option<String>,
    /// The `Message-ID` the recipient really sees, when the provider replaced ours.
    /// The reply check matches replies by this id, so the engine saves it in `send_jobs.message_id_header`.
    pub message_id_header: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SendError {
    /// Try again later.
    Transient(String),
    /// Never retry this job.
    Permanent { reason: String, hard_bounce: bool },
    RateLimited { retry_after: Option<Duration>, daily: bool },
    /// Token or password is bad.
    AuthFailed(String),
    /// May or may not have sent.
    Unknown(String),
}

impl SendError {
    pub fn describe(&self) -> String {
        match self {
            SendError::Transient(m) => format!("temporary error: {m}"),
            SendError::Permanent { reason, hard_bounce } => {
                format!("permanent error{}: {reason}", if *hard_bounce { " (hard bounce)" } else { "" })
            }
            SendError::RateLimited { retry_after, daily } => format!(
                "rate limited{}{}",
                if *daily { " (daily quota)" } else { "" },
                retry_after.map(|d| format!(", retry after {}s", d.as_secs())).unwrap_or_default()
            ),
            SendError::AuthFailed(m) => format!("login failed: {m}"),
            SendError::Unknown(m) => format!("unknown result: {m}"),
        }
    }
}

#[async_trait]
pub trait MailSender: Send + Sync {
    async fn send(&self, msg: &OutgoingMessage) -> Result<SendReceipt, SendError>;
}

/// Picks the sender for an account.
pub fn sender_for(account: &EmailAccount, secrets: Secrets) -> Result<Box<dyn MailSender>, SendError> {
    match (account.provider, secrets) {
        (Provider::Gmail, Secrets::OAuth(o)) => Ok(Box::new(gmail::GmailSender::new(o.access_token))),
        (Provider::Microsoft, Secrets::OAuth(o)) => Ok(Box::new(microsoft::MicrosoftSender::new(o.access_token))),
        (Provider::Smtp, Secrets::Password(p)) => Ok(Box::new(smtp::SmtpSender::from_account(account, p)?)),
        _ => Err(SendError::Permanent {
            reason: "the saved credentials do not match the account type".into(),
            hard_bounce: false,
        }),
    }
}

/// Truncates text that comes from a remote server, so logs and `last_error` stay small.
pub fn clip(text: &str) -> String {
    let t: String = text.chars().take(300).collect();
    t.replace(['\n', '\r'], " ")
}
