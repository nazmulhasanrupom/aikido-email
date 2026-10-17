//! Small queries that the worker, the routes and the tests share.
use super::settings::GeneratorSettings;
use chrono::{DateTime, Duration, TimeZone, Utc};
use serde_json::Value;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

pub async fn load_settings(conn: &mut PgConnection) -> Result<GeneratorSettings, sqlx::Error> {
    let (v,): (Value,) = sqlx::query_as("SELECT settings FROM generator_defaults WHERE id = 1").fetch_one(conn).await?;
    Ok(GeneratorSettings::from_stored(v))
}

pub async fn engine_paused(pool: &PgPool) -> Result<bool, sqlx::Error> {
    let (p,): (bool,) = sqlx::query_as("SELECT engine_paused FROM generator_defaults WHERE id = 1").fetch_one(pool).await?;
    Ok(p)
}

pub async fn touch_tick(pool: &PgPool, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE generator_defaults SET last_tick_at = $1 WHERE id = 1").bind(now).execute(pool).await?;
    Ok(())
}

/// One LLM call, for `llm_calls`. Never holds a prompt or an answer.
#[derive(Clone, Debug)]
pub struct CallRecord {
    pub purpose: &'static str,
    pub provider_id: Option<Uuid>,
    pub model: String,
    pub campaign_id: Option<Uuid>,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_tokens: u32,
    pub duration_ms: u32,
    pub ok: bool,
    pub error_code: Option<String>,
}

pub async fn record_call(conn: &mut PgConnection, c: &CallRecord, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO llm_calls (purpose, provider_id, model, campaign_id, input_tokens, output_tokens, cached_tokens, duration_ms, ok, error_code, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(c.purpose)
    .bind(c.provider_id)
    .bind(&c.model)
    .bind(c.campaign_id)
    .bind(c.input_tokens as i32)
    .bind(c.output_tokens as i32)
    .bind(c.cached_tokens as i32)
    .bind(c.duration_ms as i32)
    .bind(c.ok)
    .bind(&c.error_code)
    .bind(now)
    .execute(conn)
    .await?;
    Ok(())
}

/// The start of the UTC day that holds `now`.
pub fn day_start(now: DateTime<Utc>) -> DateTime<Utc> {
    Utc.from_utc_datetime(&now.date_naive().and_hms_opt(0, 0, 0).unwrap_or_default())
}

/// Input + output tokens of today (UTC), over all purposes.
pub async fn tokens_today(conn: &mut PgConnection, now: DateTime<Utc>) -> Result<i64, sqlx::Error> {
    let start = day_start(now);
    let (n,): (i64,) = sqlx::query_as("SELECT COALESCE(SUM(input_tokens + output_tokens), 0)::bigint FROM llm_calls WHERE created_at >= $1 AND created_at < $2")
        .bind(start)
        .bind(start + Duration::days(1))
        .fetch_one(conn)
        .await?;
    Ok(n)
}

pub async fn tokens_of_campaign(conn: &mut PgConnection, campaign_id: Uuid) -> Result<i64, sqlx::Error> {
    let (n,): (i64,) = sqlx::query_as("SELECT COALESCE(SUM(input_tokens + output_tokens), 0)::bigint FROM llm_calls WHERE campaign_id = $1")
        .bind(campaign_id)
        .fetch_one(conn)
        .await?;
    Ok(n)
}

/// (`review_required` override, `token_budget`) of a campaign.
pub async fn campaign_overrides(conn: &mut PgConnection, campaign_id: Uuid) -> Result<(Option<bool>, Option<i64>), sqlx::Error> {
    let row: Option<(Option<bool>, Option<i64>)> =
        sqlx::query_as("SELECT review_required, token_budget FROM generator_campaigns WHERE campaign_id = $1").bind(campaign_id).fetch_optional(conn).await?;
    Ok(row.unwrap_or((None, None)))
}

/// The campaign a sequence belongs to.
pub async fn campaign_of_sequence(conn: &mut PgConnection, sequence_id: Uuid) -> Result<Option<Uuid>, sqlx::Error> {
    let row: Option<(Option<Uuid>,)> = sqlx::query_as("SELECT campaign_id FROM sequences WHERE id = $1").bind(sequence_id).fetch_optional(conn).await?;
    Ok(row.and_then(|r| r.0))
}
