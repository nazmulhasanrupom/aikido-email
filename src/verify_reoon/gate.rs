//! The send rule. The campaign feeder asks `check` for each lead that wants to start a campaign.
//! Only the feeder uses it: a lead already in a sequence is never stopped by a rule change.
use super::settings::{self, SendRule};
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

/// The rule that counts for a campaign: its own (`campaigns.verification_rule`), or the workspace rule.
/// `None` when Reoon is not installed: then there is no rule.
pub async fn effective_rule(conn: &mut PgConnection, campaign_rule: Option<&Value>) -> Result<Option<SendRule>, sqlx::Error> {
    let installed: Option<(i32,)> = sqlx::query_as("SELECT 1 FROM integrations WHERE app = 'reoon'").fetch_optional(&mut *conn).await?;
    if installed.is_none() {
        return Ok(None);
    }
    if let Some(own) = campaign_rule.and_then(|v| serde_json::from_value::<SendRule>(v.clone()).ok()) {
        return Ok(Some(own.normalized()));
    }
    Ok(Some(settings::load(&mut *conn).await?.send_rule))
}

/// `true`: the lead may start the campaign.
pub async fn check(conn: &mut PgConnection, rule: &SendRule, lead_id: Uuid, email: &str) -> Result<bool, sqlx::Error> {
    let status: Option<(String,)> =
        sqlx::query_as("SELECT status FROM email_verifications WHERE lead_id = $1 AND provider = 'reoon' AND email = $2").bind(lead_id).bind(email.to_ascii_lowercase()).fetch_optional(conn).await?;
    Ok(match status {
        None => rule.allow_not_verified,
        Some((s,)) => rule.allowed.iter().any(|a| *a == s),
    })
}

/// Pending leads of a campaign that the rule would skip, and pending leads with no result at all.
pub async fn count_pending(conn: &mut PgConnection, campaign: Uuid, rule: &SendRule) -> Result<(i64, i64), sqlx::Error> {
    sqlx::query_as(
        "SELECT count(*) FILTER (WHERE r.status IS NULL AND NOT $3 OR r.status IS NOT NULL AND NOT (r.status = ANY($2))),
                count(*) FILTER (WHERE r.status IS NULL)
           FROM campaign_leads cl JOIN leads l ON l.id = cl.lead_id
           LEFT JOIN email_verifications r ON r.lead_id = l.id AND r.provider = 'reoon' AND r.email = l.email::text
          WHERE cl.campaign_id = $1 AND cl.state = 'pending'",
    )
    .bind(campaign)
    .bind(&rule.allowed)
    .bind(rule.allow_not_verified)
    .fetch_one(conn)
    .await
}

/// Move leads that were skipped by the rule back to `pending`, when they pass the rule now. Returns how many.
pub async fn requeue(conn: &mut PgConnection, campaign: Uuid, rule: &SendRule) -> Result<u64, sqlx::Error> {
    let r = sqlx::query(
        "UPDATE campaign_leads cl SET state = 'pending', skip_reason = NULL
           FROM leads l LEFT JOIN email_verifications r ON r.lead_id = l.id AND r.provider = 'reoon' AND r.email = l.email::text
          WHERE cl.lead_id = l.id AND cl.campaign_id = $1 AND cl.state = 'skipped' AND cl.skip_reason = 'verification_not_allowed'
            AND CASE WHEN r.status IS NULL THEN $3 ELSE r.status = ANY($2) END",
    )
    .bind(campaign)
    .bind(&rule.allowed)
    .bind(rule.allow_not_verified)
    .execute(conn)
    .await?;
    Ok(r.rows_affected())
}
