//! Open numbers. A lead is **Opened** after its first real open. A lead whose opens all come from a proxy or a
//! scanner is **Maybe opened**. More real opens are **Repeat opens**.
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{FromRow, PgConnection, PgPool};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Serialize, ToSchema, Clone, Debug, Default, PartialEq)]
pub struct OpenSummary {
    /// `opened` (a real open), `maybe_opened` (only proxy opens) or `none`.
    pub status: String,
    /// Real opens.
    pub opens: i64,
    /// Opens by Apple Mail Privacy Protection, the Google image proxy or a scanner.
    pub proxy_opens: i64,
    pub first_opened_at: Option<DateTime<Utc>>,
    pub last_opened_at: Option<DateTime<Utc>>,
}

pub fn status_of(real: i64, proxy: i64) -> &'static str {
    if real > 0 {
        "opened"
    } else if proxy > 0 {
        "maybe_opened"
    } else {
        "none"
    }
}

/// The opens of one lead (all campaigns). Used by the lead context and `GET /v1/leads/{id}/opens`.
pub async fn lead_summary(conn: &mut PgConnection, lead_id: Uuid) -> Result<OpenSummary, sqlx::Error> {
    let (real, proxy, first, last): (i64, i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE NOT is_proxy), count(*) FILTER (WHERE is_proxy), min(opened_at) FILTER (WHERE NOT is_proxy), max(opened_at) FROM email_opens WHERE lead_id = $1",
    )
    .bind(lead_id)
    .fetch_one(conn)
    .await?;
    Ok(OpenSummary { status: status_of(real, proxy).into(), opens: real, proxy_opens: proxy, first_opened_at: first, last_opened_at: last })
}

#[derive(FromRow, Serialize, ToSchema, Debug)]
pub struct OpenRow {
    pub id: i64,
    pub send_job_id: Uuid,
    /// The step of the sequence (1 = the first email).
    pub step_number: Option<i32>,
    pub campaign_id: Option<Uuid>,
    pub opened_at: DateTime<Utc>,
    pub is_proxy: bool,
    /// `apple`, `google` or `scanner` when `is_proxy`.
    pub proxy_kind: Option<String>,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
}

pub async fn lead_opens(pool: &PgPool, lead_id: Uuid, limit: i64) -> Result<Vec<OpenRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT o.id, o.send_job_id, j.step_number, o.campaign_id, o.opened_at, o.is_proxy, o.proxy_kind, o.ip, o.user_agent
           FROM email_opens o LEFT JOIN send_jobs j ON j.id = o.send_job_id WHERE o.lead_id = $1 ORDER BY o.opened_at DESC, o.id DESC LIMIT $2",
    )
    .bind(lead_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

#[derive(Serialize, ToSchema, Debug, Default)]
pub struct CampaignOpens {
    /// Leads that got at least one email with a tracker in it.
    pub tracked_leads: i64,
    /// Leads with a real open.
    pub opened: i64,
    /// Leads whose opens all came from a proxy or a scanner.
    pub maybe_opened: i64,
    /// `opened` divided by `tracked_leads` (0 to 1).
    pub open_rate: f64,
    /// Real opens after the first one of a lead.
    pub repeat_opens: i64,
    pub total_opens: i64,
    pub proxy_opens: i64,
}

pub async fn campaign_summary(pool: &PgPool, campaign_id: Uuid) -> Result<CampaignOpens, sqlx::Error> {
    let tracked: i64 = sqlx::query_scalar(
        "SELECT count(DISTINCT t.lead_id) FROM tracking_tokens t JOIN send_jobs j ON j.id = t.send_job_id WHERE t.campaign_id = $1 AND j.status = 'sent' AND t.lead_id IS NOT NULL",
    )
    .bind(campaign_id)
    .fetch_one(pool)
    .await?;
    let (opened, maybe, repeat, real_total, proxy_total): (i64, i64, i64, i64, i64) = sqlx::query_as(
        "WITH per AS (
            SELECT lead_id, count(*) FILTER (WHERE NOT is_proxy) AS real, count(*) FILTER (WHERE is_proxy) AS proxy
              FROM email_opens WHERE campaign_id = $1 AND lead_id IS NOT NULL GROUP BY lead_id)
         SELECT count(*) FILTER (WHERE real > 0), count(*) FILTER (WHERE real = 0 AND proxy > 0),
                COALESCE(sum(GREATEST(real - 1, 0)), 0)::bigint, COALESCE(sum(real), 0)::bigint, COALESCE(sum(proxy), 0)::bigint FROM per",
    )
    .bind(campaign_id)
    .fetch_one(pool)
    .await?;
    Ok(CampaignOpens {
        tracked_leads: tracked,
        opened,
        maybe_opened: maybe,
        open_rate: if tracked > 0 { (opened as f64 / tracked as f64).min(1.0) } else { 0.0 },
        repeat_opens: repeat,
        total_opens: real_total,
        proxy_opens: proxy_total,
    })
}

#[derive(FromRow, Serialize, ToSchema, Debug)]
pub struct RecentLead {
    pub lead_id: Uuid,
    pub email: String,
    pub status: String,
    pub opens: i64,
    pub proxy_opens: i64,
    pub last_opened_at: DateTime<Utc>,
}

/// The leads that opened last, newest first.
pub async fn recent_leads(pool: &PgPool, campaign_id: Uuid, limit: i64) -> Result<Vec<RecentLead>, sqlx::Error> {
    sqlx::query_as(
        "SELECT o.lead_id, l.email::text AS email,
                CASE WHEN count(*) FILTER (WHERE NOT o.is_proxy) > 0 THEN 'opened' ELSE 'maybe_opened' END AS status,
                count(*) FILTER (WHERE NOT o.is_proxy) AS opens, count(*) FILTER (WHERE o.is_proxy) AS proxy_opens, max(o.opened_at) AS last_opened_at
           FROM email_opens o JOIN leads l ON l.id = o.lead_id WHERE o.campaign_id = $1 GROUP BY o.lead_id, l.email ORDER BY max(o.opened_at) DESC LIMIT $2",
    )
    .bind(campaign_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// The open status and the real open count of many leads at once, for the lead tables. Leads with no open are not in the map.
pub async fn briefs(conn: &mut PgConnection, ids: &[Uuid]) -> Result<std::collections::HashMap<Uuid, (String, i64)>, sqlx::Error> {
    let rows: Vec<(Uuid, i64, i64)> = sqlx::query_as(
        "SELECT lead_id, count(*) FILTER (WHERE NOT is_proxy), count(*) FILTER (WHERE is_proxy) FROM email_opens WHERE lead_id = ANY($1) GROUP BY lead_id",
    )
    .bind(ids)
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(|(id, real, proxy)| (id, (status_of(real, proxy).to_string(), real))).collect())
}

/// Erase the tracking rows of leads (lead delete, decision B).
pub async fn erase_leads(conn: &mut PgConnection, ids: &[Uuid]) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM email_opens WHERE lead_id = ANY($1)").bind(ids).execute(&mut *conn).await?;
    sqlx::query("DELETE FROM tracking_tokens WHERE lead_id = ANY($1)").bind(ids).execute(&mut *conn).await?;
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_follows_real_and_proxy_opens() {
        assert_eq!(status_of(1, 5), "opened");
        assert_eq!(status_of(0, 2), "maybe_opened");
        assert_eq!(status_of(0, 0), "none");
    }
}
