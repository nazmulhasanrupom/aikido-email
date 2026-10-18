//! The feeder: every `feeder_tick_secs` it moves waiting leads of each active campaign into the sequence,
//! up to `daily_new_leads` a day (the day starts at midnight in the campaign time zone). Adding 5000 leads to
//! a campaign does not enroll 5000 at once. The sending engine still decides when each email goes out.
//!
//! Safe with many server copies: a campaign is claimed with `FOR UPDATE SKIP LOCKED` and a lease, each lead
//! row is locked while it is worked, and the enrollment has the key `campaign:<id>:lead:<id>`.
use super::model::*;
use super::routes::{PoolAccount, pool_accounts};
use crate::followups::enroll::{self, EnrollError, EnrollStatus, NewEnrollment};
use crate::followups::repo as followup_repo;
use crate::leads::model::{ContactState, Verification};
use crate::leads::routes::load_settings;
use crate::sending::rules::resolve_local;
use crate::state::AppState;
use chrono::{DateTime, Duration, Utc};
use rand::seq::SliceRandom;
use sqlx::PgConnection;
use std::time::Duration as StdDuration;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// A campaign that a server copy holds for longer than this is taken by another copy.
pub const LEASE_SECS: i64 = 300;
const CAMPAIGNS_PER_TICK: i64 = 50;

#[derive(Debug, Default, PartialEq, Eq, Clone)]
pub struct TickReport {
    /// Campaigns looked at.
    pub campaigns: usize,
    pub enrolled: usize,
    pub skipped: usize,
    /// Campaigns that became `completed`.
    pub completed: usize,
    /// Campaigns that could not go on (no healthy account, or an enroll error). They try again next tick.
    pub stuck: usize,
}

/// The start of the campaign's current day, as a UTC moment.
pub fn day_start(tz: &str, now: DateTime<Utc>) -> DateTime<Utc> {
    let tz: chrono_tz::Tz = tz.parse().unwrap_or(chrono_tz::Tz::UTC);
    let date = now.with_timezone(&tz).date_naive();
    resolve_local(tz, date.and_hms_opt(0, 0, 0).unwrap_or_default())
}

pub async fn tick(st: &AppState) -> Result<TickReport, sqlx::Error> {
    let now = st.now();
    sqlx::query("UPDATE lead_defaults SET last_tick_at = $1 WHERE id = 1").bind(now).execute(&st.pool).await?;
    let (paused,): (bool,) = sqlx::query_as("SELECT feeder_paused FROM lead_defaults WHERE id = 1").fetch_one(&st.pool).await?;
    let mut report = TickReport::default();
    if paused {
        return Ok(report);
    }
    let ids: Vec<(Uuid,)> = sqlx::query_as(
        "UPDATE campaigns SET claimed_at = $1
          WHERE id IN (SELECT id FROM campaigns WHERE status = 'active' AND (claimed_at IS NULL OR claimed_at < $2)
                       ORDER BY id LIMIT $3 FOR UPDATE SKIP LOCKED)
         RETURNING id",
    )
    .bind(now)
    .bind(now - Duration::seconds(LEASE_SECS))
    .bind(CAMPAIGNS_PER_TICK)
    .fetch_all(&st.pool)
    .await?;
    for (id,) in ids {
        report.campaigns += 1;
        let result = feed_campaign(st, id, now).await;
        // let go of the campaign, also after an error
        sqlx::query("UPDATE campaigns SET claimed_at = NULL WHERE id = $1").bind(id).execute(&st.pool).await?;
        match result {
            Ok(r) => {
                report.enrolled += r.enrolled;
                report.skipped += r.skipped;
                report.completed += usize::from(r.completed);
                report.stuck += usize::from(r.stuck);
            }
            // One bad campaign must not stop the others.
            Err(e) => {
                tracing::error!(campaign = %id, error = %e, "feeder failed for a campaign");
                report.stuck += 1;
            }
        }
    }
    Ok(report)
}

/// Runs until cancelled. The speed is the setting `feeder_tick_secs`, read on every round.
pub async fn run(st: AppState, cancel: CancellationToken) {
    tracing::info!("campaign feeder started");
    loop {
        if let Err(e) = tick(&st).await {
            tracing::error!(error = %e, "feeder tick failed");
        }
        let secs = load_settings(&st.pool).await.map(|s| s.feeder_tick_secs).unwrap_or(60);
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(StdDuration::from_secs(secs.max(1) as u64)) => {}
        }
    }
    tracing::info!("campaign feeder stopped");
}

#[derive(Default)]
struct CampaignReport {
    enrolled: usize,
    skipped: usize,
    completed: bool,
    stuck: bool,
}

enum Step {
    /// The row was dealt with (enrolled or skipped).
    Done { enrolled: bool },
    /// Nothing left to take.
    Empty,
    /// The campaign can not go on right now. The row stays `pending`.
    Stuck(String),
}

async fn feed_campaign(st: &AppState, id: Uuid, now: DateTime<Utc>) -> Result<CampaignReport, sqlx::Error> {
    let mut report = CampaignReport::default();
    let mut conn = st.pool.acquire().await?;
    let Some(c) = super::routes::get(&mut conn, id).await? else { return Ok(report) };
    if c.status != CampaignStatus::Active {
        return Ok(report);
    }
    drop(conn);

    let (today,): (i64,) = sqlx::query_as("SELECT count(*) FROM campaign_leads WHERE campaign_id = $1 AND state = 'enrolled' AND enrolled_at >= $2")
        .bind(id)
        .bind(day_start(&c.timezone, now))
        .fetch_one(&st.pool)
        .await?;
    let mut room = (c.daily_new_leads as i64 - today).max(0);
    let mut warning: Option<String> = None;
    while room > 0 {
        match feed_one(st, &c, now).await? {
            Step::Done { enrolled } => {
                if enrolled {
                    report.enrolled += 1;
                    room -= 1;
                } else {
                    report.skipped += 1;
                }
            }
            Step::Empty => break,
            Step::Stuck(why) => {
                warning = Some(why);
                report.stuck = true;
                break;
            }
        }
    }

    // `warning` shows why the campaign is stuck, and goes away when it is not
    sqlx::query("UPDATE campaigns SET warning = $2 WHERE id = $1 AND warning IS DISTINCT FROM $2").bind(id).bind(&warning).execute(&st.pool).await?;

    if !report.stuck && complete_if_done(st, id, now).await? {
        report.completed = true;
    }
    Ok(report)
}

/// Done when nothing is pending and every enrollment of the campaign ended. A campaign with no lead at all is not done.
async fn complete_if_done(st: &AppState, id: Uuid, now: DateTime<Utc>) -> Result<bool, sqlx::Error> {
    let done = sqlx::query(
        "UPDATE campaigns SET status = 'completed', completed_at = $2, updated_at = $2, warning = NULL
          WHERE id = $1 AND status = 'active'
            AND EXISTS (SELECT 1 FROM campaign_leads WHERE campaign_id = $1)
            AND NOT EXISTS (SELECT 1 FROM campaign_leads WHERE campaign_id = $1 AND state = 'pending')
            AND NOT EXISTS (SELECT 1 FROM campaign_leads cl JOIN enrollments e ON e.id = cl.enrollment_id
                             WHERE cl.campaign_id = $1 AND e.status IN ('active', 'waiting_send', 'paused'))",
    )
    .bind(id)
    .bind(now)
    .execute(&st.pool)
    .await?
    .rows_affected();
    Ok(done == 1)
}

async fn skip(tx: &mut PgConnection, campaign: Uuid, lead: Uuid, reason: SkipReason) -> Result<Step, sqlx::Error> {
    sqlx::query("UPDATE campaign_leads SET state = 'skipped', skip_reason = $3 WHERE campaign_id = $1 AND lead_id = $2")
        .bind(campaign)
        .bind(lead)
        .bind(reason)
        .execute(tx)
        .await?;
    Ok(Step::Done { enrolled: false })
}

/// Take the oldest pending lead and enroll it, or skip it with a reason.
async fn feed_one(st: &AppState, c: &Campaign, now: DateTime<Utc>) -> Result<Step, sqlx::Error> {
    let mut tx = st.pool.begin().await?;
    let row: Option<(Uuid, String, ContactState, Verification)> = sqlx::query_as(
        "SELECT cl.lead_id, l.email, l.contact_state, l.verification
           FROM campaign_leads cl JOIN leads l ON l.id = cl.lead_id
          WHERE cl.campaign_id = $1 AND cl.state = 'pending'
            -- AI mode: a lead is enrolled only when its Primary email is ready. The others wait and use no room.
            AND ($2 = false OR EXISTS (SELECT 1 FROM lead_emails le WHERE le.campaign_id = cl.campaign_id AND le.lead_id = cl.lead_id
                                          AND le.step_number = 1 AND le.status IN ('ready', 'stale')))
          ORDER BY cl.added_at, cl.lead_id LIMIT 1 FOR UPDATE OF cl SKIP LOCKED",
    )
    .bind(c.id)
    .bind(c.content_mode == "ai")
    .fetch_optional(&mut *tx)
    .await?;
    let Some((lead_id, email, state, verification)) = row else { return Ok(Step::Empty) };

    // 0. A copy that died after it made the enrollment, but before it saved the row, left an enrollment with our key.
    // Take it: the lead is enrolled already.
    let key = format!("campaign:{}:lead:{}", c.id, lead_id);
    let made: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM enrollments WHERE idempotency_key = $1").bind(&key).fetch_optional(&mut *tx).await?;
    if let Some((enrollment_id,)) = made {
        sqlx::query("UPDATE campaign_leads SET state = 'enrolled', skip_reason = NULL, enrollment_id = $3, enrolled_at = $4 WHERE campaign_id = $1 AND lead_id = $2")
            .bind(c.id)
            .bind(lead_id)
            .bind(enrollment_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(Step::Done { enrolled: true });
    }

    // 1. Never contact a suppressed address, or one that bounced or unsubscribed.
    if followup_repo::suppression_of(&mut tx, &email).await?.is_some() || matches!(state, ContactState::Bounced | ContactState::Unsubscribed) {
        let step = skip(&mut tx, c.id, lead_id, SkipReason::Suppressed).await?;
        tx.commit().await?;
        return Ok(step);
    }
    // 2. The address must have been checked (unless the campaign allows the others).
    if !c.allow_unverified && !matches!(verification, Verification::MxOk | Verification::SyntaxOk) {
        let step = skip(&mut tx, c.id, lead_id, SkipReason::Unverified).await?;
        tx.commit().await?;
        return Ok(step);
    }
    // 3. One open enrollment per address (the follow-up setting).
    let settings = followup_repo::load_settings(&mut tx).await?;
    if settings.one_active_per_email {
        let open: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM enrollments WHERE to_email = $1::citext AND status IN ('active', 'waiting_send', 'paused') LIMIT 1").bind(&email).fetch_optional(&mut *tx).await?;
        if open.is_some() {
            let step = skip(&mut tx, c.id, lead_id, SkipReason::ActiveElsewhere).await?;
            tx.commit().await?;
            return Ok(step);
        }
    }
    // 4. An account of the pool. If none is healthy, wait: the row stays pending.
    let accounts = pool_accounts(&mut tx, c.id).await?;
    // the last account used is read now, not at the start of the tick: round_robin must see the lead before this one
    let (last_account_id,): (Option<Uuid>,) = sqlx::query_as("SELECT last_account_id FROM campaigns WHERE id = $1").bind(c.id).fetch_one(&mut *tx).await?;
    let Some(account) = pick_account(&mut tx, c, last_account_id, &accounts).await? else {
        return Ok(Step::Stuck("No account of the pool is healthy (connected and not paused). Leads wait until one is.".into()));
    };
    // 5. Enroll with the same function as the API. The key makes a retry safe.
    let item = NewEnrollment {
        to_email: None,
        account_id: account,
        variables: None,
        idempotency_key: Some(key),
        start_at: None,
        lead_id: Some(lead_id),
    };
    let items = match enroll::enroll(&st.pool, c.sequence_id, vec![item], now).await {
        Ok(items) => items,
        Err(EnrollError::MissingVariables(_)) => {
            let step = skip(&mut tx, c.id, lead_id, SkipReason::MissingVariable).await?;
            tx.commit().await?;
            return Ok(step);
        }
        Err(EnrollError::Db(e)) => return Err(e),
    };
    let Some(item) = items.into_iter().next() else { return Ok(Step::Stuck("The enroll call answered nothing.".into())) };
    match (item.status, item.enrollment, item.error) {
        (EnrollStatus::Enrolled | EnrollStatus::Duplicate, Some(e), _) => {
            sqlx::query("UPDATE campaign_leads SET state = 'enrolled', skip_reason = NULL, enrollment_id = $3, enrolled_at = $4 WHERE campaign_id = $1 AND lead_id = $2")
                .bind(c.id)
                .bind(lead_id)
                .bind(e.id)
                .bind(now)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE campaigns SET last_account_id = $2 WHERE id = $1").bind(c.id).bind(account).execute(&mut *tx).await?;
            tx.commit().await?;
            Ok(Step::Done { enrolled: true })
        }
        (_, _, Some(err)) => {
            let reason = match err.code.as_str() {
                "suppressed" => Some(SkipReason::Suppressed),
                "already_enrolled" => Some(SkipReason::ActiveElsewhere),
                "lead_not_found" => Some(SkipReason::LeadDeleted),
                _ => None,
            };
            match reason {
                Some(r) => {
                    let step = skip(&mut tx, c.id, lead_id, r).await?;
                    tx.commit().await?;
                    Ok(step)
                }
                None => Ok(Step::Stuck(format!("A lead could not be enrolled: {} ({}).", err.message, err.code))),
            }
        }
        _ => Ok(Step::Stuck("The enroll call gave no enrollment and no error.".into())),
    }
}

/// `least_loaded`: the healthy account with the fewest open enrollments, a tie at random.
/// `round_robin`: the healthy account after the last one used (by id), then around again.
async fn pick_account(conn: &mut PgConnection, c: &Campaign, last_account_id: Option<Uuid>, accounts: &[PoolAccount]) -> Result<Option<Uuid>, sqlx::Error> {
    let mut healthy: Vec<Uuid> = accounts.iter().filter(|a| a.healthy).map(|a| a.id).collect();
    if healthy.is_empty() {
        return Ok(None);
    }
    healthy.sort();
    Ok(match c.account_pick {
        AccountPick::RoundRobin => match last_account_id {
            Some(last) => healthy.iter().copied().find(|a| *a > last).or_else(|| healthy.first().copied()),
            None => healthy.first().copied(),
        },
        AccountPick::LeastLoaded => {
            let loads: Vec<(Uuid, i64)> = sqlx::query_as(
                "SELECT account_id, count(*) FROM enrollments WHERE account_id = ANY($1) AND status IN ('active', 'waiting_send', 'paused') GROUP BY 1",
            )
            .bind(&healthy)
            .fetch_all(conn)
            .await?;
            let load = |a: &Uuid| loads.iter().find(|l| l.0 == *a).map(|l| l.1).unwrap_or(0);
            healthy.shuffle(&mut rand::thread_rng());
            healthy.into_iter().min_by_key(|a| load(a))
        }
    })
}
