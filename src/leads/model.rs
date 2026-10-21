//! Lead types. Enums map to Postgres enums from `0004_leads.sql`.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

macro_rules! pg_enum {
    ($(#[$m:meta])* $name:ident, $pg:literal, { $($variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(sqlx::Type, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, ToSchema)]
        #[sqlx(type_name = $pg, rename_all = "snake_case")]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }
        }
    };
}

pg_enum!(
    /// What the system knows about the contact. Only the system sets it.
    ContactState, "lead_contact_state", {
        New => "new", InSequence => "in_sequence", Contacted => "contacted", Replied => "replied",
        Bounced => "bounced", Unsubscribed => "unsubscribed", Completed => "completed",
    }
);

pg_enum!(
    /// What a person (or the AI agent later) decides. The system never sets it.
    Interest, "lead_interest", {
        Interested => "interested", MeetingBooked => "meeting_booked", Won => "won",
        NotInterested => "not_interested", WrongPerson => "wrong_person", Lost => "lost",
    }
);

pg_enum!(
    Verification, "lead_verification", {
        Unknown => "unknown", SyntaxOk => "syntax_ok", MxOk => "mx_ok", NoMx => "no_mx", Invalid => "invalid",
    }
);

pg_enum!(
    LeadSource, "lead_source", {
        Api => "api", Csv => "csv", Enrollment => "enrollment", AutoDatabase => "auto_database", Manual => "manual",
    }
);

pg_enum!(
    FieldKind, "lead_field_kind", { Text => "text", Number => "number", Url => "url", Date => "date" }
);

pg_enum!(
    /// What happens when a lead with the same email already exists.
    DedupeMode, "lead_dedupe_mode", {
        Skip => "skip", FillEmpty => "fill_empty", Overwrite => "overwrite",
    }
);

pg_enum!(
    ImportStatus, "lead_import_status", {
        Uploaded => "uploaded", Pending => "pending", Running => "running", Done => "done", Failed => "failed", Cancelled => "cancelled",
    }
);

/// Built-in text fields a caller can set. Custom field keys can not use these names.
pub const TEXT_FIELDS: [&str; 12] = [
    "first_name", "last_name", "company_name", "job_title", "phone", "website", "linkedin_url", "location", "timezone", "industry", "personalization_note",
    "trigger_event",
];

/// Every name that is taken: the text fields and the other columns of a lead.
pub const RESERVED_KEYS: [&str; 28] = [
    "id", "email", "email_domain", "first_name", "last_name", "company_name", "job_title", "phone", "website", "linkedin_url", "location",
    "timezone", "custom_fields", "tags", "contact_state", "interest", "verification", "source", "source_ref", "notes", "last_contacted_at",
    "last_replied_at", "created_at", "updated_at", "industry", "personalization_note", "trigger_event", "trigger_verified",
];

pub const LEAD_COLS: &str = "id, email, email_domain, first_name, last_name, company_name, job_title, phone, website, linkedin_url, location, timezone, \
    custom_fields, tags, contact_state, interest, verification, source, source_ref, notes, last_contacted_at, last_replied_at, created_at, updated_at, \
    industry, personalization_note, trigger_event, trigger_verified";

#[derive(FromRow, Serialize, Clone, Debug, ToSchema)]
pub struct Lead {
    pub id: Uuid,
    pub email: String,
    pub email_domain: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub company_name: Option<String>,
    pub job_title: Option<String>,
    pub phone: Option<String>,
    pub website: Option<String>,
    pub linkedin_url: Option<String>,
    pub location: Option<String>,
    /// IANA name, like `Asia/Dhaka`. Stored only for now.
    pub timezone: Option<String>,
    /// Values of the custom fields. A key is a `lead_fields` key.
    pub custom_fields: Value,
    pub tags: Vec<String>,
    pub contact_state: ContactState,
    pub interest: Option<Interest>,
    pub verification: Verification,
    pub source: LeadSource,
    pub source_ref: Option<String>,
    pub notes: Option<String>,
    pub last_contacted_at: Option<DateTime<Utc>>,
    pub last_replied_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The industry of the company. Used by the default mail prompt.
    pub industry: Option<String>,
    /// A fact about this lead the mail may use. Used by the default mail prompt.
    pub personalization_note: Option<String>,
    /// A recent event about the lead or company (the `trigger` of the prompt).
    pub trigger_event: Option<String>,
    /// True when a person checked `trigger_event`.
    pub trigger_verified: bool,
    /// The latest Reoon status of this address (`safe`, `catch_all`, ...). Null: not verified, or Reoon is not installed. Filled by the list and detail routes.
    #[sqlx(skip)]
    pub verification_status: Option<String>,
    /// Reoon score 0 to 100 (power mode only).
    #[sqlx(skip)]
    pub verification_score: Option<i32>,
    #[sqlx(skip)]
    pub verification_checked_at: Option<DateTime<Utc>>,
    /// Open tracking: `opened`, `maybe_opened` (only proxy opens) or `none`. Filled by the list route.
    #[sqlx(skip)]
    pub open_status: Option<String>,
    /// Real opens of this lead (open tracking). Filled by the list route.
    #[sqlx(skip)]
    pub open_count: i64,
}

#[derive(FromRow, Serialize, Clone, Debug, ToSchema)]
pub struct LeadField {
    /// Lower snake case, 1 to 40 characters. The template variable is `{{key}}`.
    pub key: String,
    pub label: String,
    pub kind: FieldKind,
    /// Used in templates when the lead has no value.
    pub fallback: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(FromRow, Serialize, Clone, Debug, ToSchema)]
pub struct LeadList {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// How many leads are in the list.
    pub member_count: i64,
    /// The column names of the list's own rows, in order.
    #[schema(value_type = Vec<String>)]
    pub columns: serde_json::Value,
    /// How many own rows the list holds (imported into the list, not leads).
    pub row_count: i64,
}

#[derive(FromRow, Serialize, Clone, Debug, ToSchema)]
pub struct LeadEvent {
    pub id: i64,
    pub lead_id: Uuid,
    pub kind: String,
    pub ref_id: Option<Uuid>,
    pub detail: Value,
    pub at: DateTime<Utc>,
}
