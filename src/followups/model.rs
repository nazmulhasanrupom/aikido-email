//! Rows and enums of the follow-up step.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(sqlx::Type, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, ToSchema)]
#[sqlx(type_name = "sequence_status", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum SequenceStatus {
    Active,
    Paused,
    Archived,
}

#[derive(sqlx::Type, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default, ToSchema)]
#[sqlx(type_name = "delay_type", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum DelayType {
    /// Saturday and Sunday do not count (in the time zone of the sending account).
    #[default]
    BusinessDays,
    CalendarDays,
}

#[derive(sqlx::Type, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, ToSchema)]
#[sqlx(type_name = "enrollment_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum EnrollmentStatus {
    /// Waits for `next_due_at`.
    Active,
    /// The step is a job in the send queue.
    WaitingSend,
    Paused,
    Finished,
    Stopped,
    Failed,
}

impl EnrollmentStatus {
    /// An enrollment that can still move.
    pub fn is_open(self) -> bool {
        matches!(self, Self::Active | Self::WaitingSend | Self::Paused)
    }
}

pub const SEQUENCE_COLS: &str = "id, name, status, stop_on_auto_reply, auto_reply_pause_days, thread_followups, campaign_id, brief, created_at, updated_at";

#[derive(Clone, Debug, FromRow, Serialize, ToSchema)]
pub struct Sequence {
    pub id: Uuid,
    pub name: String,
    pub status: SequenceStatus,
    /// `false`: an auto-reply (out of office) pauses the enrollment. `true`: it stops it.
    pub stop_on_auto_reply: bool,
    /// How many business days an auto-reply pauses the enrollment. 1 to 30.
    pub auto_reply_pause_days: i32,
    /// Follow-ups show as replies to the first email.
    pub thread_followups: bool,
    pub campaign_id: Option<Uuid>,
    /// What the Email Generator knows about the sender: product, offer, audience. Used in the prompt of the AI slots.
    pub brief: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub const STEP_COLS: &str = "id, sequence_id, step_number, delay_amount, delay_type, subject, body_text, body_html, ai_slots, updated_at";

#[derive(Clone, Debug, FromRow, Serialize, ToSchema)]
pub struct SequenceStep {
    pub id: Uuid,
    pub sequence_id: Uuid,
    /// 1 to 10. Step 1 is the first email.
    pub step_number: i32,
    /// Time after the step before it was sent. Always 0 for step 1.
    pub delay_amount: i32,
    pub delay_type: DelayType,
    /// Step 2 and later: empty means "Re: " plus the subject of step 1.
    pub subject: String,
    pub body_text: Option<String>,
    pub body_html: Option<String>,
    /// The AI slots of the step: `[{key, instruction, max_words, allow_links, fallback}]`. See `{{ai:key}}`.
    #[schema(value_type = Vec<crate::generator::slots::AiSlot>)]
    pub ai_slots: Value,
    pub updated_at: DateTime<Utc>,
}

pub const ENROLLMENT_COLS: &str = "id, sequence_id, account_id, to_email::text AS to_email, variables, status, next_step, next_due_at, \
    current_job_id, step_queued_at, stop_reason, pause_until, idempotency_key, lead_id, wait_reason, created_at, updated_at";

#[derive(Clone, Debug, FromRow, Serialize, ToSchema)]
pub struct Enrollment {
    pub id: Uuid,
    pub sequence_id: Uuid,
    /// The one account that sends every step. It never changes.
    pub account_id: Uuid,
    pub to_email: String,
    #[schema(value_type = Object)]
    pub variables: Value,
    pub status: EnrollmentStatus,
    /// The step that is queued, or goes out next.
    pub next_step: i32,
    pub next_due_at: Option<DateTime<Utc>>,
    /// The send job of the step that is queued or being sent.
    pub current_job_id: Option<Uuid>,
    pub step_queued_at: Option<DateTime<Utc>>,
    /// Why the enrollment ended (`stopped` or `failed`): `reply`, `unsubscribe`, `bounce_hard`, `bounce_block`,
    /// `complaint`, `auto_reply`, `manual`, `suppressed`, `job_cancelled`, `stalled`, `send_error`, ...
    pub stop_reason: Option<String>,
    pub pause_until: Option<DateTime<Utc>>,
    pub idempotency_key: Option<String>,
    pub lead_id: Option<Uuid>,
    /// Why a due step is held back: `generation_pending` (the AI text is not ready yet) or `generation_review` (it waits for approval).
    pub wait_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, FromRow, Serialize, ToSchema)]
pub struct EnrollmentEvent {
    pub id: i64,
    pub enrollment_id: Uuid,
    /// `enrolled`, `step_queued`, `step_sent`, `step_failed`, `paused`, `resumed`, `stopped`, `finished`, `failed`, `warning`, ...
    pub kind: String,
    pub step_number: Option<i32>,
    #[schema(value_type = Object)]
    pub detail: Value,
    pub at: DateTime<Utc>,
}
