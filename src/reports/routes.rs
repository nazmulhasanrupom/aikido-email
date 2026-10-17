use crate::auth::api_keys::Scope;
use crate::auth::middleware::require_scope;
use crate::error::{ApiError, ApiJson, ApiQuery, ApiResult};
use crate::state::AppState;
use axum::extract::State;
use chrono::{Duration, NaiveDate};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::{BTreeMap, HashMap};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn router(state: AppState) -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(daily))
        .routes(routes!(accounts))
        .route_layer(axum::middleware::from_fn_with_state((state, Scope::Campaigns), require_scope))
}

pub const MAX_DAYS: i64 = 366;

/// The time zone of the days: the `tz` given, else the sending default.
async fn zone_of(pool: &PgPool, tz: Option<String>) -> ApiResult<String> {
    let name = match tz.filter(|t| !t.trim().is_empty()) {
        Some(t) => t,
        None => {
            let mut conn = pool.acquire().await?;
            crate::sending::queue::load_defaults(&mut conn).await?.timezone
        }
    };
    name.parse::<chrono_tz::Tz>().map_err(|_| ApiError::unprocessable("invalid_timezone", format!("'{name}' is not a time zone name like Europe/Berlin.")))?;
    Ok(name)
}

fn check_range(from: NaiveDate, to: NaiveDate) -> ApiResult<()> {
    if to < from {
        return Err(ApiError::unprocessable("invalid_range", "'to' must not be before 'from'."));
    }
    if (to - from).num_days() + 1 > MAX_DAYS {
        return Err(ApiError::unprocessable("range_too_long", format!("Ask for {MAX_DAYS} days or fewer.")));
    }
    Ok(())
}

#[derive(Deserialize, IntoParams)]
pub struct DailyQuery {
    /// First day, `YYYY-MM-DD`, in the time zone `tz`.
    from: NaiveDate,
    /// Last day (included).
    to: NaiveDate,
    /// An IANA name like `Europe/Berlin`. Default: the time zone of the sending defaults.
    tz: Option<String>,
    campaign_id: Option<Uuid>,
    account_id: Option<Uuid>,
}

#[derive(Serialize, ToSchema, Default, Clone)]
pub struct DayRow {
    pub date: NaiveDate,
    /// Emails that went out.
    pub sent: i64,
    /// People who replied that day. A person counts once a day.
    pub replied: i64,
    pub auto_replied: i64,
    /// Hard bounces (people).
    pub bounced: i64,
    pub unsubscribed: i64,
    pub complained: i64,
    /// Leads that a campaign took into its sequence.
    pub new_leads_enrolled: i64,
}

#[derive(Serialize, ToSchema)]
pub struct DailyReply {
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub tz: String,
    /// One row for every day from `from` to `to`, also the days with nothing.
    pub days: Vec<DayRow>,
    pub totals: DayRow,
}

/// Numbers per day, for the charts.
#[utoipa::path(get, path = "/v1/reports/daily", tag = "reports",
    params(DailyQuery),
    responses((status = 200, body = DailyReply), (status = 422, description = "invalid_range, range_too_long, invalid_timezone")),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. Days are cut in the time zone `tz`. At most 366 days. Sent comes from `send_jobs`, replies and bounces from the reply check, enrolled leads from the campaigns.")]
async fn daily(State(st): State<AppState>, ApiQuery(q): ApiQuery<DailyQuery>) -> ApiResult<ApiJson<DailyReply>> {
    check_range(q.from, q.to)?;
    let tz = zone_of(&st.pool, q.tz).await?;
    let mut rows: BTreeMap<NaiveDate, DayRow> = BTreeMap::new();
    let mut day = q.from;
    while day <= q.to {
        rows.insert(day, DayRow { date: day, ..Default::default() });
        day += Duration::days(1);
    }
    // Both ends are midnight in the zone: [from 00:00, to+1 00:00).
    let bounds = "$1::date::timestamp AT TIME ZONE $3";
    let end = "($2::date + 1)::timestamp AT TIME ZONE $3";

    let sent: Vec<(NaiveDate, i64)> = sqlx::query_as(&format!(
        "SELECT (sent_at AT TIME ZONE $3)::date AS d, count(*) FROM send_jobs
          WHERE status = 'sent' AND sent_at >= {bounds} AND sent_at < {end}
            AND ($4::uuid IS NULL OR campaign_id = $4) AND ($5::uuid IS NULL OR account_id = $5)
          GROUP BY 1"
    ))
    .bind(q.from)
    .bind(q.to)
    .bind(&tz)
    .bind(q.campaign_id)
    .bind(q.account_id)
    .fetch_all(&st.pool)
    .await?;
    for (d, n) in sent {
        if let Some(r) = rows.get_mut(&d) {
            r.sent = n;
        }
    }

    // one person counts once per day and label
    let inbound: Vec<(NaiveDate, String, i64)> = sqlx::query_as(&format!(
        "SELECT (received_at AT TIME ZONE $3)::date AS d, label::text,
                count(DISTINCT lower(COALESCE(bounce_recipient::text, from_email::text)))
           FROM inbound_messages
          WHERE received_at >= {bounds} AND received_at < {end}
            AND label IN ('reply', 'auto_reply', 'bounce_hard', 'unsubscribe', 'complaint')
            AND ($4::uuid IS NULL OR campaign_id = $4) AND ($5::uuid IS NULL OR account_id = $5)
          GROUP BY 1, 2"
    ))
    .bind(q.from)
    .bind(q.to)
    .bind(&tz)
    .bind(q.campaign_id)
    .bind(q.account_id)
    .fetch_all(&st.pool)
    .await?;
    for (d, label, n) in inbound {
        let Some(r) = rows.get_mut(&d) else { continue };
        match label.as_str() {
            "reply" => r.replied = n,
            "auto_reply" => r.auto_replied = n,
            "bounce_hard" => r.bounced = n,
            "unsubscribe" => r.unsubscribed = n,
            "complaint" => r.complained = n,
            _ => {}
        }
    }

    let enrolled: Vec<(NaiveDate, i64)> = sqlx::query_as(&format!(
        "SELECT (cl.enrolled_at AT TIME ZONE $3)::date AS d, count(*) FROM campaign_leads cl
           LEFT JOIN enrollments e ON e.id = cl.enrollment_id
          WHERE cl.state = 'enrolled' AND cl.enrolled_at >= {bounds} AND cl.enrolled_at < {end}
            AND ($4::uuid IS NULL OR cl.campaign_id = $4) AND ($5::uuid IS NULL OR e.account_id = $5)
          GROUP BY 1"
    ))
    .bind(q.from)
    .bind(q.to)
    .bind(&tz)
    .bind(q.campaign_id)
    .bind(q.account_id)
    .fetch_all(&st.pool)
    .await?;
    for (d, n) in enrolled {
        if let Some(r) = rows.get_mut(&d) {
            r.new_leads_enrolled = n;
        }
    }

    let days: Vec<DayRow> = rows.into_values().collect();
    let mut totals = DayRow { date: q.to, ..Default::default() };
    for d in &days {
        totals.sent += d.sent;
        totals.replied += d.replied;
        totals.auto_replied += d.auto_replied;
        totals.bounced += d.bounced;
        totals.unsubscribed += d.unsubscribed;
        totals.complained += d.complained;
        totals.new_leads_enrolled += d.new_leads_enrolled;
    }
    Ok(ApiJson(DailyReply { from: q.from, to: q.to, tz, days, totals }))
}

#[derive(Deserialize, IntoParams)]
pub struct AccountsQuery {
    from: NaiveDate,
    to: NaiveDate,
    /// Default: the time zone of the sending defaults.
    tz: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct AccountRow {
    pub account_id: Uuid,
    pub email: String,
    /// `active`, `paused`, `error` or `disconnected`.
    pub status: String,
    pub sent: i64,
    /// People who got an email in the range.
    pub contacted: i64,
    /// People who replied, divided by `contacted` (0 to 1).
    pub reply_rate: f64,
    /// People whose address bounced hard, divided by `contacted` (0 to 1).
    pub bounce_rate: f64,
}

#[derive(Serialize, ToSchema)]
pub struct AccountsReply {
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub tz: String,
    /// The account is paused when its recent bounce rate passes this (in percent).
    pub bounce_limit_pct: u32,
    pub accounts: Vec<AccountRow>,
}

fn rate(n: i64, of: i64) -> f64 {
    if of > 0 { (n as f64 / of as f64 * 10_000.0).round() / 10_000.0 } else { 0.0 }
}

/// Sent, reply rate and bounce rate for each account.
#[utoipa::path(get, path = "/v1/reports/accounts", tag = "reports",
    params(AccountsQuery),
    responses((status = 200, body = AccountsReply), (status = 422, description = "invalid_range, range_too_long, invalid_timezone")),
    security(("api_key" = ["campaigns"])),
    description = "Scope: `campaigns`. Every account that is not deleted, also the ones that sent nothing.")]
async fn accounts(State(st): State<AppState>, ApiQuery(q): ApiQuery<AccountsQuery>) -> ApiResult<ApiJson<AccountsReply>> {
    check_range(q.from, q.to)?;
    let tz = zone_of(&st.pool, q.tz).await?;
    let bounds = "$1::date::timestamp AT TIME ZONE $3";
    let end = "($2::date + 1)::timestamp AT TIME ZONE $3";
    let list: Vec<(Uuid, String, String)> =
        sqlx::query_as("SELECT id, email::text, status::text FROM email_accounts WHERE deleted_at IS NULL ORDER BY email").fetch_all(&st.pool).await?;
    let sent: Vec<(Uuid, i64, i64)> = sqlx::query_as(&format!(
        "SELECT account_id, count(*), count(DISTINCT to_email) FROM send_jobs
          WHERE status = 'sent' AND sent_at >= {bounds} AND sent_at < {end} GROUP BY 1"
    ))
    .bind(q.from)
    .bind(q.to)
    .bind(&tz)
    .fetch_all(&st.pool)
    .await?;
    let inbound: Vec<(Uuid, String, i64)> = sqlx::query_as(&format!(
        "SELECT account_id, label::text, count(DISTINCT lower(COALESCE(bounce_recipient::text, from_email::text))) FROM inbound_messages
          WHERE received_at >= {bounds} AND received_at < {end} AND label IN ('reply', 'bounce_hard') GROUP BY 1, 2"
    ))
    .bind(q.from)
    .bind(q.to)
    .bind(&tz)
    .fetch_all(&st.pool)
    .await?;
    let sent: HashMap<Uuid, (i64, i64)> = sent.into_iter().map(|(a, n, c)| (a, (n, c))).collect();
    let count = |account: Uuid, label: &str| inbound.iter().find(|(a, l, _)| *a == account && l == label).map(|x| x.2).unwrap_or(0);
    let bounce_limit_pct = {
        let mut conn = st.pool.acquire().await?;
        crate::sending::queue::load_defaults(&mut conn).await?.pause_bounce_rate_pct
    };
    let accounts = list
        .into_iter()
        .map(|(account_id, email, status)| {
            let (n, contacted) = sent.get(&account_id).copied().unwrap_or((0, 0));
            AccountRow {
                account_id,
                email,
                status,
                sent: n,
                contacted,
                reply_rate: rate(count(account_id, "reply"), contacted),
                bounce_rate: rate(count(account_id, "bounce_hard"), contacted),
            }
        })
        .collect();
    Ok(ApiJson(AccountsReply { from: q.from, to: q.to, tz, bounce_limit_pct, accounts }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_rules() {
        let d = |m, day| NaiveDate::from_ymd_opt(2026, m, day).unwrap();
        assert!(check_range(d(1, 1), d(1, 1)).is_ok());
        assert!(check_range(d(1, 2), d(1, 1)).is_err());
        assert!(check_range(d(1, 1), d(12, 31)).is_ok(), "365 days");
        let long = NaiveDate::from_ymd_opt(2027, 1, 2).unwrap();
        assert!(check_range(d(1, 1), long).is_err(), "367 days");
    }

    #[test]
    fn rates_round_to_four_decimals() {
        assert_eq!(rate(1, 3), 0.3333);
        assert_eq!(rate(1, 0), 0.0);
    }
}
