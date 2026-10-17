//! Rows and enums of campaigns. They map to the Postgres enums of `0005_campaigns.sql`.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(sqlx::Type, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, ToSchema)]
#[sqlx(type_name = "campaign_status", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum CampaignStatus {
    Draft,
    Active,
    Paused,
    Completed,
    Archived,
}

impl CampaignStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Archived => "archived",
        }
    }
}

#[derive(sqlx::Type, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, ToSchema)]
#[sqlx(type_name = "campaign_account_pick", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum AccountPick {
    /// The account with the fewest open enrollments. A tie: random.
    LeastLoaded,
    /// The next account after the last one used.
    RoundRobin,
}

#[derive(sqlx::Type, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, ToSchema)]
#[sqlx(type_name = "campaign_lead_state", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum CampaignLeadState {
    Pending,
    Enrolled,
    Skipped,
}

#[derive(sqlx::Type, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, ToSchema)]
#[sqlx(type_name = "campaign_skip_reason", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    Suppressed,
    Unverified,
    /// The address is in an open enrollment of another sequence.
    ActiveElsewhere,
    NoHealthyAccount,
    LeadDeleted,
    AlreadyInCampaign,
    /// A step needs a value that this lead does not have.
    MissingVariable,
    /// Someone at the same company replied (`stop_company_on_reply`).
    CompanyReplied,
    CampaignArchived,
}

pub const CAMPAIGN_COLS: &str = "id, name, status, sequence_id, daily_new_leads, account_pick, stop_company_on_reply, allow_unverified, timezone, \
    last_account_id, warning, launched_at, completed_at, created_at, updated_at";

#[derive(FromRow, Serialize, Clone, Debug, ToSchema)]
pub struct Campaign {
    pub id: Uuid,
    pub name: String,
    pub status: CampaignStatus,
    /// The one sequence of this campaign.
    pub sequence_id: Uuid,
    /// 1 to 5000. How many leads the feeder enrolls per day.
    pub daily_new_leads: i32,
    pub account_pick: AccountPick,
    /// A reply stops the other open enrollments of the campaign at the same company (not for free-mail domains).
    pub stop_company_on_reply: bool,
    /// `false`: leads with `no_mx` or `invalid` are skipped.
    pub allow_unverified: bool,
    /// IANA time zone. The day boundary of `daily_new_leads`.
    pub timezone: String,
    pub last_account_id: Option<Uuid>,
    /// Why the feeder is stuck right now (for example: no healthy account). `null` when it is not.
    pub warning: Option<String>,
    pub launched_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
