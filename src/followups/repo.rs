//! Small queries that the scheduler, the routes and the tests share.
use super::model::*;
use super::settings::FollowupSettings;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

pub async fn load_settings(conn: &mut PgConnection) -> Result<FollowupSettings, sqlx::Error> {
    let (v,): (Value,) = sqlx::query_as("SELECT settings FROM followup_defaults WHERE id = 1").fetch_one(conn).await?;
    Ok(FollowupSettings::from_stored(v))
}

pub async fn engine_paused(pool: &PgPool) -> Result<bool, sqlx::Error> {
    let (p,): (bool,) = sqlx::query_as("SELECT engine_paused FROM followup_defaults WHERE id = 1").fetch_one(pool).await?;
    Ok(p)
}

pub async fn touch_tick(pool: &PgPool, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE followup_defaults SET last_tick_at = $1 WHERE id = 1").bind(now).execute(pool).await?;
    Ok(())
}

pub async fn get_sequence(conn: &mut PgConnection, id: Uuid) -> Result<Option<Sequence>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {SEQUENCE_COLS} FROM sequences WHERE id = $1")).bind(id).fetch_optional(conn).await
}

pub async fn get_steps(conn: &mut PgConnection, sequence_id: Uuid) -> Result<Vec<SequenceStep>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {STEP_COLS} FROM sequence_steps WHERE sequence_id = $1 ORDER BY step_number"))
        .bind(sequence_id)
        .fetch_all(conn)
        .await
}

pub async fn get_enrollment(pool: &PgPool, id: Uuid) -> Result<Option<Enrollment>, sqlx::Error> {
    sqlx::query_as(&format!("SELECT {ENROLLMENT_COLS} FROM enrollments WHERE id = $1")).bind(id).fetch_optional(pool).await
}

pub async fn enrollment_events(pool: &PgPool, id: Uuid) -> Result<Vec<EnrollmentEvent>, sqlx::Error> {
    sqlx::query_as("SELECT id, enrollment_id, kind, step_number, detail, at FROM enrollment_events WHERE enrollment_id = $1 ORDER BY id")
        .bind(id)
        .fetch_all(pool)
        .await
}

/// The account data a template needs: (email, display name).
pub async fn account_identity(conn: &mut PgConnection, account_id: Uuid) -> Result<Option<(String, String)>, sqlx::Error> {
    sqlx::query_as("SELECT email::text, display_name FROM email_accounts WHERE id = $1 AND deleted_at IS NULL")
        .bind(account_id)
        .fetch_optional(conn)
        .await
}

/// Why an address may not get mail, if it may not (an `automated` job: every suppression counts).
pub async fn suppression_of(conn: &mut PgConnection, email: &str) -> Result<Option<String>, sqlx::Error> {
    let domain = email.rsplit('@').next().unwrap_or_default();
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT reason::text FROM suppressions
         WHERE (kind = 'email' AND value = $1::citext) OR (kind = 'domain' AND value = $2::citext) LIMIT 1",
    )
    .bind(email)
    .bind(domain)
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|r| r.0))
}
