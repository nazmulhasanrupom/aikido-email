//! Company stop (`campaigns.stop_company_on_reply`). When a lead replies, the other open enrollments of the
//! same campaign at the same company stop, and its pending leads are skipped. Different people with a
//! `gmail.com` address are not one company, so free-mail domains are left out.
use crate::followups::lifecycle;
use crate::leads::email::is_free_mail;
use crate::leads::routes::load_settings;
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::PgConnection;
use uuid::Uuid;

/// Returns how many enrollments were stopped.
pub async fn on_reply(conn: &mut PgConnection, replied_leads: &[Uuid], now: DateTime<Utc>) -> Result<usize, sqlx::Error> {
    if replied_leads.is_empty() {
        return Ok(0);
    }
    let rows: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT cl.campaign_id, l.id, l.email_domain
           FROM campaign_leads cl JOIN campaigns c ON c.id = cl.campaign_id JOIN leads l ON l.id = cl.lead_id
          WHERE cl.lead_id = ANY($1) AND cl.state = 'enrolled' AND c.stop_company_on_reply AND c.status IN ('active', 'paused')",
    )
    .bind(replied_leads)
    .fetch_all(&mut *conn)
    .await?;
    if rows.is_empty() {
        return Ok(0);
    }
    let extra = load_settings(&mut *conn).await?.free_mail_domains_extra;
    let mut stopped = 0;
    for (campaign_id, lead_id, domain) in rows {
        if is_free_mail(&domain, &extra) {
            continue;
        }
        let open: Vec<(Uuid,)> = sqlx::query_as(
            "SELECT e.id FROM campaign_leads cl JOIN leads l ON l.id = cl.lead_id JOIN enrollments e ON e.id = cl.enrollment_id
              WHERE cl.campaign_id = $1 AND l.email_domain = $2 AND cl.lead_id <> $3 AND e.status IN ('active', 'waiting_send', 'paused')
              ORDER BY e.id",
        )
        .bind(campaign_id)
        .bind(&domain)
        .bind(lead_id)
        .fetch_all(&mut *conn)
        .await?;
        for (enrollment_id,) in open {
            // the row lock; it may have ended since the read above
            let Some(e) = lifecycle::lock(conn, enrollment_id).await? else { continue };
            if !e.status.is_open() {
                continue;
            }
            lifecycle::stop(conn, &e, "company_replied", json!({ "replied_lead_id": lead_id, "campaign_id": campaign_id }), now).await?;
            stopped += 1;
        }
        sqlx::query(
            "UPDATE campaign_leads cl SET state = 'skipped', skip_reason = 'company_replied'
               FROM leads l WHERE l.id = cl.lead_id AND cl.campaign_id = $1 AND cl.state = 'pending' AND l.email_domain = $2",
        )
        .bind(campaign_id)
        .bind(&domain)
        .execute(&mut *conn)
        .await?;
    }
    Ok(stopped)
}
