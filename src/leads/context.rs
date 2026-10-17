//! The full picture of one lead: fields, timeline, sent emails and replies. The route `GET /v1/leads/{id}/context`
//! and the Email Generator both call `build`; the generator does not call its own HTTP server.
use super::events;
use super::fields::FieldDefs;
use super::model::*;
use super::repo::{self, LeadDetail};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgConnection;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Serialize, ToSchema, Clone, Debug)]
pub struct ContextField {
    pub key: String,
    pub label: String,
    pub kind: FieldKind,
    /// `null` when the lead has no value.
    pub value: Option<String>,
}

#[derive(Serialize, ToSchema, Clone, Debug)]
pub struct ContextMessage {
    /// `sent` (we wrote it) or `received` (the lead, or a mail server, wrote it).
    pub direction: String,
    pub at: DateTime<Utc>,
    pub subject: Option<String>,
    /// Plain text. Long texts are cut at 10000 characters.
    pub text: Option<String>,
    /// For `received`: `reply`, `auto_reply`, `unsubscribe`, `bounce_hard`, ...
    pub label: Option<String>,
    /// For `sent`: the step of the sequence (1 = the first email).
    pub step_number: Option<i32>,
}

#[derive(Serialize, ToSchema, Clone, Debug)]
pub struct LeadContext {
    pub lead: LeadDetail,
    /// Every custom field, with the value of this lead.
    pub fields: Vec<ContextField>,
    /// The last 50 timeline events, newest first.
    pub events: Vec<LeadEvent>,
    /// The emails sent to the lead and the messages that came back (subject and text only), oldest first. Up to 50 of each.
    pub messages: Vec<ContextMessage>,
}

/// How much to read. The route uses the defaults; the generator asks for less.
#[derive(Clone, Debug)]
pub struct ContextOpts {
    pub max_events: i64,
    /// For the sent emails and for the received messages, each.
    pub max_messages: i64,
    /// Read the messages that came back (reply text). The generator leaves them out.
    pub include_received: bool,
    pub max_text_chars: usize,
}

impl Default for ContextOpts {
    fn default() -> Self {
        Self { max_events: 50, max_messages: 50, include_received: true, max_text_chars: 10_000 }
    }
}

pub async fn build(conn: &mut PgConnection, id: Uuid, opts: &ContextOpts) -> Result<Option<LeadContext>, sqlx::Error> {
    let Some(lead) = repo::get(conn, id).await? else { return Ok(None) };
    let defs = FieldDefs::load(conn).await?;
    let mut keys: Vec<&String> = defs.by_key.keys().collect();
    keys.sort();
    let fields: Vec<ContextField> = keys
        .into_iter()
        .map(|k| {
            let d = &defs.by_key[k];
            ContextField { key: d.key.clone(), label: d.label.clone(), kind: d.kind, value: lead.custom_fields.get(k).and_then(|v| v.as_str()).map(str::to_string) }
        })
        .collect();
    let events = events::list(conn, id, None, opts.max_events).await?;

    let sent: Vec<(DateTime<Utc>, String, Option<String>, Option<String>, Option<i32>)> = sqlx::query_as(
        "SELECT COALESCE(sent_at, created_at), subject, body_text, body_html, step_number FROM send_jobs
          WHERE (lead_id = $1 OR to_email = $2::citext) AND status = 'sent' ORDER BY COALESCE(sent_at, created_at) DESC LIMIT $3",
    )
    .bind(id)
    .bind(&lead.email)
    .bind(opts.max_messages)
    .fetch_all(&mut *conn)
    .await?;
    let received: Vec<(DateTime<Utc>, Option<String>, Option<String>, String)> = if opts.include_received {
        sqlx::query_as(
            "SELECT received_at, subject, reply_text, label::text FROM inbound_messages
              WHERE lead_id = $1 OR from_email = $2::citext OR bounce_recipient = $2::citext ORDER BY received_at DESC LIMIT $3",
        )
        .bind(id)
        .bind(&lead.email)
        .bind(opts.max_messages)
        .fetch_all(&mut *conn)
        .await?
    } else {
        Vec::new()
    };
    let cut = |text: &str| -> String { text.chars().take(opts.max_text_chars).collect() };
    let mut messages: Vec<ContextMessage> = Vec::new();
    for (at, subject, text, html, step) in sent {
        let text = text.filter(|t| !t.trim().is_empty()).or_else(|| html.map(|h| html2text::from_read(h.as_bytes(), 100).unwrap_or(h)));
        messages.push(ContextMessage { direction: "sent".into(), at, subject: Some(subject), text: text.as_deref().map(&cut), label: None, step_number: step });
    }
    for (at, subject, text, label) in received {
        messages.push(ContextMessage { direction: "received".into(), at, subject, text: text.as_deref().map(&cut), label: Some(label), step_number: None });
    }
    messages.sort_by_key(|m| m.at);
    let lead = repo::detail(conn, lead).await?;
    Ok(Some(LeadContext { lead, fields, events, messages }))
}
