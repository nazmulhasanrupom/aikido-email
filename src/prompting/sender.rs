//! The sender profile: who sends the mail and what they offer. One row per saved version, never updated in place.
//! The current profile is the newest version with `is_default`. Only a saved profile is used by campaigns.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, PgConnection};
use utoipa::ToSchema;
use uuid::Uuid;

pub const MAX_FIELD_CHARS: usize = 1000;

pub const PROFILE_COLS: &str = "id, version, sender_first_name, ideal_customer, not_a_fit, problem_solved, offer, outcome, proof_or_none, low_friction_offer, \
    allow_low_personalization, other_offers, sources_used, created_at";

#[derive(FromRow, Serialize, Clone, Debug, ToSchema)]
pub struct SenderProfile {
    pub id: Uuid,
    pub version: i32,
    pub sender_first_name: String,
    pub ideal_customer: String,
    pub not_a_fit: String,
    pub problem_solved: String,
    pub offer: String,
    pub outcome: String,
    /// `none` when there is no real proof.
    pub proof_or_none: String,
    pub low_friction_offer: String,
    pub allow_low_personalization: bool,
    #[schema(value_type = Vec<String>)]
    pub other_offers: Value,
    /// Field key to the source tags that support it.
    #[schema(value_type = Object)]
    pub sources_used: Value,
    pub created_at: DateTime<Utc>,
}

/// The nine fields a person edits. Used for the save body and for a draft from the AI.
#[derive(Deserialize, Serialize, ToSchema, Clone, Debug, Default, PartialEq)]
pub struct SenderFields {
    #[serde(default)]
    pub sender_first_name: String,
    #[serde(default)]
    pub ideal_customer: String,
    #[serde(default)]
    pub not_a_fit: String,
    #[serde(default)]
    pub problem_solved: String,
    #[serde(default)]
    pub offer: String,
    #[serde(default)]
    pub outcome: String,
    #[serde(default = "none_text")]
    pub proof_or_none: String,
    #[serde(default)]
    pub low_friction_offer: String,
    #[serde(default)]
    pub allow_low_personalization: bool,
}

fn none_text() -> String {
    "none".into()
}

impl SenderFields {
    /// Trim, turn empty proof into `none`, check the length.
    pub fn cleaned(mut self) -> Result<Self, String> {
        for (name, v) in [
            ("sender_first_name", &mut self.sender_first_name),
            ("ideal_customer", &mut self.ideal_customer),
            ("not_a_fit", &mut self.not_a_fit),
            ("problem_solved", &mut self.problem_solved),
            ("offer", &mut self.offer),
            ("outcome", &mut self.outcome),
            ("proof_or_none", &mut self.proof_or_none),
            ("low_friction_offer", &mut self.low_friction_offer),
        ] {
            *v = v.trim().to_string();
            if v.chars().count() > MAX_FIELD_CHARS {
                return Err(format!("{name} is longer than {MAX_FIELD_CHARS} characters"));
            }
        }
        if self.proof_or_none.is_empty() {
            self.proof_or_none = "none".into();
        }
        Ok(self)
    }
}

impl SenderProfile {
    pub fn fields(&self) -> SenderFields {
        SenderFields {
            sender_first_name: self.sender_first_name.clone(),
            ideal_customer: self.ideal_customer.clone(),
            not_a_fit: self.not_a_fit.clone(),
            problem_solved: self.problem_solved.clone(),
            offer: self.offer.clone(),
            outcome: self.outcome.clone(),
            proof_or_none: self.proof_or_none.clone(),
            low_friction_offer: self.low_friction_offer.clone(),
            allow_low_personalization: self.allow_low_personalization,
        }
    }
}

pub async fn current(conn: &mut PgConnection) -> Result<Option<SenderProfile>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {PROFILE_COLS} FROM sender_profiles WHERE is_default ORDER BY version DESC LIMIT 1")).fetch_optional(conn).await
}

pub async fn get(conn: &mut PgConnection, id: Uuid) -> Result<Option<SenderProfile>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {PROFILE_COLS} FROM sender_profiles WHERE id = $1")).bind(id).fetch_optional(conn).await
}

/// The newest `n` versions, newest first.
pub async fn versions(conn: &mut PgConnection, n: i64) -> Result<Vec<SenderProfile>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {PROFILE_COLS} FROM sender_profiles WHERE is_default ORDER BY version DESC LIMIT $1")).bind(n).fetch_all(conn).await
}

/// Save a new version. The old ones stay.
pub async fn save(conn: &mut PgConnection, f: &SenderFields, other_offers: &Value, sources_used: &Value, now: DateTime<Utc>) -> Result<SenderProfile, sqlx::Error> {
    sqlx::query("LOCK TABLE sender_profiles IN SHARE ROW EXCLUSIVE MODE").execute(&mut *conn).await?;
    sqlx::query_as(&format!(
        "INSERT INTO sender_profiles (id, is_default, version, sender_first_name, ideal_customer, not_a_fit, problem_solved, offer, outcome, proof_or_none,
                low_friction_offer, allow_low_personalization, other_offers, sources_used, created_at)
         SELECT $1, true, COALESCE(max(version), 0) + 1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13 FROM sender_profiles WHERE is_default
         RETURNING {PROFILE_COLS}"
    ))
    .bind(Uuid::now_v7())
    .bind(&f.sender_first_name)
    .bind(&f.ideal_customer)
    .bind(&f.not_a_fit)
    .bind(&f.problem_solved)
    .bind(&f.offer)
    .bind(&f.outcome)
    .bind(&f.proof_or_none)
    .bind(&f.low_friction_offer)
    .bind(f.allow_low_personalization)
    .bind(other_offers)
    .bind(sources_used)
    .bind(now)
    .fetch_one(conn)
    .await
}

/// Mark the unsent mails of every Optimised campaign out of date. Call it after a new profile version.
pub async fn profile_changed(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    let ids: Vec<(Uuid,)> = sqlx::query_as("SELECT c.id FROM campaigns c JOIN sequences s ON s.id = c.sequence_id WHERE c.content_mode = 'optimized'")
        .fetch_all(&mut *conn)
        .await?;
    for (id,) in ids {
        crate::campaigns::emails::mark_stale(conn, id, None).await?;
    }
    Ok(())
}

/// A profile for tests.
#[cfg(test)]
pub fn test_profile(version: i32) -> SenderProfile {
    SenderProfile {
        id: Uuid::nil(),
        version,
        sender_first_name: "Dana".into(),
        ideal_customer: "Heads of ops at dental groups".into(),
        not_a_fit: "Solo practices".into(),
        problem_solved: "Patients do not find the clinic online".into(),
        offer: "Paid search for dentists".into(),
        outcome: "More booked patients".into(),
        proof_or_none: "none".into(),
        low_friction_offer: "A free audit".into(),
        allow_low_personalization: false,
        other_offers: serde_json::json!([]),
        sources_used: serde_json::json!({}),
        created_at: chrono::Utc::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_proof_becomes_none_and_long_text_is_refused() {
        let f = SenderFields { proof_or_none: "  ".into(), ..Default::default() }.cleaned().unwrap();
        assert_eq!(f.proof_or_none, "none");
        let long = SenderFields { offer: "x".repeat(MAX_FIELD_CHARS + 1), ..Default::default() };
        assert!(long.cleaned().is_err());
    }
}
