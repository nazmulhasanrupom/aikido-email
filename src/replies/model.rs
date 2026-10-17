//! The types the reply check shares: labels, match methods, the classifier result, the DB row.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(sqlx::Type, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, ToSchema)]
#[sqlx(type_name = "inbound_label", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum Label {
    Reply,
    AutoReply,
    Unsubscribe,
    BounceHard,
    BounceSoft,
    BounceBlock,
    Complaint,
    Delayed,
    Unknown,
}

impl Label {
    pub fn as_str(self) -> &'static str {
        match self {
            Label::Reply => "reply",
            Label::AutoReply => "auto_reply",
            Label::Unsubscribe => "unsubscribe",
            Label::BounceHard => "bounce_hard",
            Label::BounceSoft => "bounce_soft",
            Label::BounceBlock => "bounce_block",
            Label::Complaint => "complaint",
            Label::Delayed => "delayed",
            Label::Unknown => "unknown",
        }
    }
    /// Labels that only make sense when the email links to one of our sends.
    pub fn needs_match(self) -> bool {
        matches!(self, Label::Reply | Label::AutoReply | Label::Unsubscribe)
    }
}

#[derive(sqlx::Type, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, ToSchema)]
#[sqlx(type_name = "match_method", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum MatchMethod {
    DsnOriginal,
    InReplyTo,
    References,
    Thread,
    BounceRecipient,
    SenderAddress,
    None,
}

#[derive(sqlx::Type, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, ToSchema)]
#[sqlx(type_name = "label_source", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum LabelSource {
    Auto,
    Manual,
}

#[derive(sqlx::Type, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, ToSchema)]
#[sqlx(type_name = "bounce_kind", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum BounceKind {
    Hard,
    Soft,
    Block,
}

#[derive(sqlx::Type, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, ToSchema)]
#[sqlx(type_name = "job_kind", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Automated,
    Manual,
}

/// What a bounce report says.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct BounceInfo {
    /// The address that did not get the email.
    pub recipient: Option<String>,
    /// For example `5.1.1`.
    pub status: Option<String>,
    pub diagnostic: Option<String>,
    /// `failed`, `delayed`, ...
    pub action: Option<String>,
    /// `Message-ID` of the email we sent, read from the report.
    pub original_message_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Classification {
    pub label: Label,
    pub reason: String,
    pub bounce: Option<BounceInfo>,
}

/// One stored inbound email. Only mail that links to our sends is stored.
#[derive(Clone, Debug, FromRow, Serialize, ToSchema)]
pub struct InboundMessage {
    pub id: Uuid,
    pub account_id: Uuid,
    pub folder: String,
    pub provider_message_id: String,
    pub message_id_header: Option<String>,
    pub in_reply_to: Option<String>,
    pub references_ids: Vec<String>,
    pub provider_thread_id: Option<String>,
    pub from_email: String,
    pub from_name: Option<String>,
    pub subject: Option<String>,
    pub received_at: DateTime<Utc>,
    pub label: Label,
    pub label_reason: Option<String>,
    pub label_source: LabelSource,
    pub match_method: MatchMethod,
    pub matched_job_id: Option<Uuid>,
    pub lead_id: Option<Uuid>,
    pub campaign_id: Option<Uuid>,
    pub bounce_recipient: Option<String>,
    pub bounce_status: Option<String>,
    pub bounce_diagnostic: Option<String>,
    pub reply_text: Option<String>,
    pub truncated: bool,
    #[schema(value_type = Vec<Object>)]
    pub actions: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub const INBOUND_COLS: &str = "id, account_id, folder, provider_message_id, message_id_header, in_reply_to, references_ids, \
    provider_thread_id, from_email::text AS from_email, from_name, subject, received_at, label, label_reason, label_source, \
    match_method, matched_job_id, lead_id, campaign_id, bounce_recipient::text AS bounce_recipient, bounce_status, \
    bounce_diagnostic, reply_text, truncated, actions, created_at, updated_at";
